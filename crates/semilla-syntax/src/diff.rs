//! The syntax diff of two workspaces.
//!
//! The diff goes top-down (design section 8.6):
//! 1. Match the items. The item map from the identity claims comes first; then the same path.
//! 2. Compare the attributes of each matched item: visibility, signature, docs, members.
//! 3. Map the statements of each body (the statement mapper, design 41): the same text with
//!    corresponding parents, the same text at any depth, the same text after replacements, then
//!    the composites. A mapped statement at a different place is a move. In each block, the
//!    mapped statements in order are in place; the other statements of a gap pair by kind (the
//!    rules of batch 8b). A body above the limit of the mapper keeps the rules of batch 8b.
//! 4. Compare paired nodes. The diff stops at the lowest node that contains the change.
//!
//! The renames from the claims are applied to the "before" side first, so a claimed rename gives
//! no edit.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use semilla_core::{Anchor, View};
use serde::Serialize;

use crate::load::{Item, ItemKind, Workspace};
use crate::mapper::{BodyMap, at};
use crate::tree::{Node, Pos};

/// True for a doc line (`///`, `//!`, or a `#[doc]` attribute). A plain `//` comment is not one.
pub fn is_doc_line(line: &str) -> bool {
    line.starts_with("///") || line.starts_with("//!") || (crate::doc::code_part(line).0.len() == 1 && line.starts_with('#'))
}

/// The kind of an edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditKind {
    ItemAdded,
    ItemRemoved,
    Visibility,
    Signature,
    Docs,
    /// A plain `//` or `/* */` comment changed. A comment is not a doc.
    Comment,
    Attrs,
    Members,
    /// A statement exists only after.
    Inserted,
    /// A statement exists only before.
    Deleted,
    /// The same statement at another place.
    Moved,
    /// The node itself changed (an operator, an annotation); its children are the same.
    Modified,
    /// The node changed to a different node.
    Replaced,
    FileAdded,
    FileRemoved,
    FileChanged,
}

/// One difference between the two workspaces.
#[derive(Clone, Debug, Serialize)]
pub struct Edit {
    pub kind: EditKind,
    pub view: View,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Anchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Anchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_before: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_after: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attr_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attr_after: Option<String>,
    /// The source text of the old and the new node, for views (the token text is for compares).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_after: Option<String>,
    /// True if the old or the new subtree contains statements.
    pub block_like: bool,
    /// The file and the line, for views and hints.
    pub file: Option<String>,
    pub line: usize,
    /// What changed in a file edit, for example `binary file changed` or `mode 100644 → 100755`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The refactoring of a statement edit (design 41): a move, or a part of an extracted or an
    /// inlined variable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refactor: Option<Refactor>,
}

/// The refactoring of an edit (design 41).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Refactor {
    /// A moved statement. `jumped`: the places of the statements that the move passes over, on
    /// the old side and on the new side (design 41.7).
    Moved {
        #[serde(skip)]
        jumped: [Vec<(Pos, Pos)>; 2],
    },
    /// A part of an extracted variable: its new `let`, or a use. `expr` is the display text of
    /// the expression. `region`: for the `let`, the places of the statements from the `let` to
    /// the end of its block, and from the partner of its next statement on the other side.
    Extract {
        var: String,
        expr: String,
        #[serde(skip)]
        region: [Vec<(Pos, Pos)>; 2],
    },
    /// A part of an inlined variable: its removed `let`, or a use.
    Inline {
        var: String,
        expr: String,
        #[serde(skip)]
        region: [Vec<(Pos, Pos)>; 2],
    },
}

/// A statement pair of the mapper that is a move, or that is the same only after replacements
/// (design 41.8). The MIR check of the map uses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementMap {
    pub before: Anchor,
    pub after: Anchor,
    pub moved: bool,
}

/// The extra output of the statement mapper.
#[derive(Clone, Debug, Default)]
pub struct BodyMaps {
    pub maps: Vec<StatementMap>,
    /// The bodies above the limit of the mapper: they keep the alignment of batch 8b.
    pub over_limit: Vec<String>,
    /// The statement pairs of round 3 and of the composite step, and the pairs of the gap
    /// rules of 8b in a mapped body (design 41.8).
    pub round3: usize,
    pub composites: usize,
    pub gap_pairs: usize,
    /// The node pairs of the tree compare of each body, by the path of the old item (design 59,
    /// B-d). The cover check reads them for a changed line with no fact.
    pub pairs: BTreeMap<String, Vec<NodePair>>,
}

/// One node pair of the tree compare (design 59, B-d): the places of the two nodes, and the
/// state of their tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodePair {
    /// The first place and the place after the end, on the old side and on the new side.
    pub ranges: [(Pos, Pos); 2],
    /// The kinds of the two nodes.
    pub kinds: [&'static str; 2],
    /// True if the two nodes have equal tokens.
    pub equal: bool,
    /// True if the own tokens of the two nodes (the tokens that are in no child) are equal, in
    /// order (design 52.1). A token with no meaning (design 52.2) is not in the own tokens.
    pub own_equal: bool,
    /// True for a pair only for the control flow graph (design 60, R60.3-10): the pair of an
    /// arm of two `match` nodes that the compare replaces. The positions of the facts do not
    /// read it.
    pub graph_only: bool,
}

impl Edit {
    fn file_edit(kind: EditKind, file: &str) -> Edit {
        Edit {
            kind,
            view: View::File,
            before: None,
            after: None,
            node_before: None,
            node_after: None,
            text_before: None,
            text_after: None,
            attr_before: None,
            attr_after: None,
            source_before: None,
            source_after: None,
            block_like: false,
            file: Some(file.to_string()),
            line: 0,
            note: None,
            refactor: None,
        }
    }

    /// The anchor of the edit on the "after" side, else on the "before" side.
    pub fn anchor(&self) -> Option<&Anchor> {
        self.after.as_ref().or(self.before.as_ref())
    }

    /// The item path on the "after" side, else on the "before" side.
    pub fn item(&self) -> Option<&str> {
        self.anchor().map(|anchor| anchor.item.as_str())
    }
}

/// The renames that the claims declare. They are applied to the "before" side.
#[derive(Clone, Debug, Default)]
pub struct Renames {
    /// Item paths: before to after (from `rename` and `move-item`).
    pub items: BTreeMap<String, String>,
    /// Identifier renames: (scope item, or `None` for all items) → (old ident, new ident).
    pub idents: Vec<(Option<String>, String, String)>,
    /// Field renames (old name, new name). They apply only where a field name can be: after `.`,
    /// before `:` in a struct literal or pattern, and as a shorthand field (`S { old }` gives
    /// `S { new: old }`). A local variable with the same name does not change.
    pub fields: Vec<(String, String)>,
    /// The site table (design section 51): for an item of the "before" side, the key of each
    /// place that is a site of a rename, with the new name. With a table, the map changes a
    /// name only at a site: `idents` with no scope and `fields` do not apply. `None`: the map
    /// has no table (a map that a test builds by hand). Then the map of words applies.
    pub sites: Option<BTreeMap<String, BTreeMap<String, String>>>,
    /// The count of the sites of each renamed item (the key is the old path).
    pub counts: BTreeMap<String, SiteCount>,
    /// The references that can follow a rename (design 54.17). The first key is the old path
    /// of the renamed item. The second key is an item of the "before" side. The value is the
    /// count of its sites (no declaration) whose new item has the new name at a place of the
    /// same kind.
    pub followed: BTreeMap<String, BTreeMap<String, usize>>,
}

/// The count of the places of one renamed item, by class (design 51.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SiteCount {
    /// The places that the syntax tree resolves to the item.
    pub sure: usize,
    /// The places that have the name, that the tree cannot resolve, and that the rename has.
    pub by_name: usize,
    /// The changed places that have the name, that the tree cannot resolve, and that the rename
    /// does not have (the name is not free). Each one is an edit of its own.
    pub own: usize,
}

impl Renames {
    /// Applies the renames to a token text, for an item on the "before" side.
    pub fn apply(&self, item: &str, text: &str) -> String {
        if self.sites.is_none() {
            return self.apply_words(item, text);
        }
        let spans = self.spans(item, text);
        if spans.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len() + 16);
        let mut from = 0;
        for (start, end, new) in &spans {
            out.push_str(&text[from..*start]);
            out.push_str(new);
            from = *end;
        }
        out.push_str(&text[from..]);
        out
    }

    /// True if the map has a site table. Then a text is equal only after the map.
    pub fn exact(&self) -> bool {
        self.sites.is_some()
    }

    /// The parts of `text` that the map changes: (start, end, new text). A renamed local
    /// changes at a bare name only. Each other name changes only at a site of the table.
    pub fn spans(&self, item: &str, text: &str) -> Vec<(usize, usize, String)> {
        use crate::sites::PlaceKind;
        let Some(sites) = &self.sites else { return Vec::new() };
        let table = sites.get(item);
        let locals: Vec<(&str, &str)> = self.idents.iter().filter(|(scope, _, _)| scope.as_deref() == Some(item)).map(|(_, old, new)| (old.as_str(), new.as_str())).collect();
        if table.is_none() && locals.is_empty() {
            return Vec::new();
        }
        let wanted = |word: &str| locals.iter().any(|(old, _)| *old == word) || table.is_some_and(|table| table.keys().any(|key| key.rsplit(':').next() == Some(word)));
        let mut out = Vec::new();
        for place in crate::sites::places(text, &wanted) {
            let local = locals.iter().find(|(old, _)| *old == place.name).map(|(_, new)| *new);
            let new = match (&place.kind, local) {
                (PlaceKind::Path { qualifier: Some(qualifier), more: false }, Some(new)) if qualifier.is_empty() => new.to_string(),
                // `S { old }` is `S { old : old }`: the label stays, the local changes.
                (PlaceKind::Key { short: true, .. }, Some(new)) => format!("{} : {new}", place.name),
                _ => match table.and_then(|table| table.get(&place.key())) {
                    // The label changes, the local stays.
                    Some(new) if matches!(place.kind, PlaceKind::Key { short: true, .. }) => format!("{new} : {}", place.name),
                    Some(new) => new.clone(),
                    None => continue,
                },
            };
            out.push((place.start, place.end, new));
        }
        out
    }

    /// The map of words: the map of a `Renames` with no site table.
    fn apply_words(&self, item: &str, text: &str) -> String {
        let renames: Vec<_> = self
            .idents
            .iter()
            .filter(|(scope, _, _)| scope.as_deref().is_none_or(|scope| scope == item))
            .collect();
        let text = self.apply_fields(text);
        if renames.is_empty() {
            return text;
        }
        // Replace whole identifiers only. Token text can put a delimiter next to a word
        // (`[len ..]`, `(len)`), so the scan looks at character classes, not at spaces.
        let mut out = String::with_capacity(text.len());
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String| {
            if !word.is_empty() {
                let replaced = renames.iter().find(|(_, old, _)| old == word).map(|(_, _, new)| new.as_str());
                out.push_str(replaced.unwrap_or(word));
                word.clear();
            }
        };
        for c in text.chars() {
            if c.is_alphanumeric() || c == '_' {
                word.push(c);
            } else {
                flush(&mut word, &mut out);
                out.push(c);
            }
        }
        flush(&mut word, &mut out);
        out
    }

    /// Applies the field renames to a token text (the tokens are separated by spaces).
    fn apply_fields(&self, text: &str) -> String {
        if self.fields.is_empty() || !self.fields.iter().any(|(old, _)| text.contains(old.as_str())) {
            return text.to_string();
        }
        let words: Vec<&str> = text.split(' ').collect();
        let keyword = |w: &str| matches!(w, "if" | "while" | "match" | "for" | "else" | "loop" | "unsafe" | "async" | "move" | "in" | "return" | "break" | "const" | "static" | "let" | "mut" | "ref" | "where" | "impl" | "fn" | "dyn" | "as");
        let ident = |w: &str| w.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') && !keyword(w);
        // The open brackets: `(`, `[`, or `{` with true for a struct literal or pattern.
        let mut stack: Vec<(&str, bool)> = Vec::new();
        // The conditions of `if`, `while`, `match`, and `for`: (depth, true while a pattern comes
        // before `=` or `in`). Their `{` opens a block, not a struct literal.
        let mut pending: Vec<(usize, bool)> = Vec::new();
        let mut out: Vec<String> = Vec::with_capacity(words.len());
        for (index, word) in words.iter().enumerate() {
            let prev = index.checked_sub(1).map(|i| words[i]).unwrap_or("");
            let next = words.get(index + 1).copied().unwrap_or("");
            match *word {
                "if" | "while" | "match" => pending.push((stack.len(), false)),
                "for" => pending.push((stack.len(), true)),
                "let" if pending.last().is_some_and(|(depth, _)| *depth == stack.len()) => pending.last_mut().unwrap().1 = true,
                "=" | "in" if pending.last().is_some_and(|(depth, _)| *depth == stack.len()) => pending.last_mut().unwrap().1 = false,
                "{" => {
                    let condition = pending.last().copied().filter(|(depth, _)| *depth == stack.len());
                    let literal = match condition {
                        Some((_, true)) | None => ident(prev) || prev == ">",
                        Some((_, false)) => {
                            pending.pop();
                            false
                        }
                    };
                    stack.push(("{", literal));
                }
                "(" | "[" => stack.push((word, false)),
                ")" | "]" | "}" => {
                    stack.pop();
                }
                _ => {}
            }
            let Some((_, new)) = self.fields.iter().find(|(old, _)| old == word) else {
                out.push(word.to_string());
                continue;
            };
            let in_literal = stack.last().is_some_and(|(kind, literal)| *kind == "{" && *literal) && (prev == "{" || prev == ",");
            if prev == "." && next != "(" && next != "::" || in_literal && next == ":" {
                out.push(new.clone());
            } else if in_literal && (next == "," || next == "}") {
                out.push(format!("{new} : {word}"));
            } else {
                out.push(word.to_string());
            }
        }
        out.join(" ")
    }
}

/// Computes all edits from `before` to `after`.
pub fn diff(before: &Workspace, after: &Workspace, renames: &Renames) -> Vec<Edit> {
    diff_maps(before, after, renames).0
}

/// Computes all edits from `before` to `after`, and the statement maps of the bodies.
pub fn diff_maps(before: &Workspace, after: &Workspace, renames: &Renames) -> (Vec<Edit>, BodyMaps) {
    let mut maps = BodyMaps::default();
    let mut edits = Vec::new();
    let mut matched_after = BTreeSet::new();
    let mut net_files = (crate::net::Files::default(), crate::net::Files::default());

    // A file that the loader cannot parse on one side (design 37.5): the file edit has the
    // change. The items of the file on the other side are not removed and not added.
    let skip = unparsed_files(before, after);
    let (before_uses, after_uses) = (uses_by_module(before), uses_by_module(after));
    for (path, item) in before.items.iter().filter(|(_, item)| item.kind != ItemKind::Use && !skip.contains(&item.file)) {
        let target = renames.items.get(path).cloned().unwrap_or_else(|| path.clone());
        match after.items.get(&target) {
            // Two entities of a different kind do not pair by path (design 37.1): a method of
            // a trait and a method of an inherent impl are two different entities.
            Some(other) if other.kind == item.kind && in_trait(before, path) == in_trait(after, &target) => {
                matched_after.insert(target.clone());
                let start = edits.len();
                // A module with a block on one side and a file on the other side (design
                // 56.3): the two sides compare in one form.
                if item.kind == ItemKind::Mod && before.module_files.contains_key(path) != after.module_files.contains_key(&target) {
                    diff_item(&module_form(before, item), &module_form(after, other), renames, &mut edits, &mut maps);
                } else {
                    diff_item(item, other, renames, &mut edits, &mut maps);
                }
                // The net: a changed token that no edit of the pair has (batch 6a, A6).
                let open = crate::net::uncovered(&mut net_files, before, after, item, other, renames, &edits[start..]);
                edits.extend(open);
            }
            _ => edits.push(item_edit(EditKind::ItemRemoved, Some(item), None)),
        }
    }
    for (path, item) in after.items.iter().filter(|(_, item)| item.kind != ItemKind::Use && !skip.contains(&item.file)) {
        if !matched_after.contains(path) {
            edits.push(item_edit(EditKind::ItemAdded, None, Some(item)));
        }
    }

    // `use` items have no stable position, so they are compared as a set in each module.
    // A renamed module is one module (design 56.3): its `use` lines pair by the rename.
    let module_pair = |old: &String| renames.items.get(old).filter(|new| before.items.get(old).is_some_and(|item| item.kind == ItemKind::Mod) && after.items.get(*new).is_some_and(|item| item.kind == ItemKind::Mod));
    let renamed_modules: BTreeSet<&String> = before_uses.keys().filter_map(|module| module_pair(module)).collect();
    let modules: BTreeSet<&String> = before_uses.keys().chain(after_uses.keys().filter(|module| !renamed_modules.contains(module))).collect();
    for module in modules {
        let old: Vec<&Item> = before_uses.get(module).into_iter().flatten().copied().filter(|item| !skip.contains(&item.file)).collect();
        let new: Vec<&Item> = after_uses.get(module_pair(module).unwrap_or(module)).into_iter().flatten().copied().filter(|item| !skip.contains(&item.file)).collect();
        // The visibility and the attributes are parts of a `use` line (batch 6a, A6): `pub use`
        // that changes to `use` removes a public name.
        let head = |item: &Item| (item.vis.replace(' ', ""), item.attrs.clone());
        for item in old.iter().filter(|item| !new.iter().any(|other| head(other) == head(item) && other.signature == renames.apply(&item.path, &item.signature))) {
            edits.push(item_edit(EditKind::ItemRemoved, Some(item), None));
        }
        for item in new.iter().filter(|item| !old.iter().any(|other| head(other) == head(item) && renames.apply(&other.path, &other.signature) == item.signature)) {
            edits.push(item_edit(EditKind::ItemAdded, None, Some(item)));
        }
        // The docs and the comments of a `use` line that stays (design 56.2, rule 2): the
        // lines pair in their order. A pair with different docs gets the docs edit.
        let mut taken = BTreeSet::new();
        for item in &old {
            let same = |other: &&Item| head(other) == head(item) && other.signature == renames.apply(&item.path, &item.signature);
            let free = |other: &&&Item| same(other) && !taken.contains(&other.path);
            let Some(other) = new.iter().find(|other| free(other) && other.docs == item.docs).or_else(|| new.iter().find(free)) else { continue };
            taken.insert(other.path.clone());
            docs_edits(item, other, &mut edits);
        }
    }

    // The inner attributes of a crate root or a module file (`#![…]`): an attribute edit of the
    // module. A crate root has no item, so the anchor is the crate name.
    // The identity of a module is its path in the item tree, not its file (design 56.3): a
    // renamed module pairs by the rename. A module with a block on one side has its inner
    // attributes in its item on that side: the item pair has the compare.
    let renamed_to: BTreeSet<&String> = renames.items.iter().filter(|(old, new)| before.module_files.contains_key(*old) && after.module_files.contains_key(*new)).map(|(_, new)| new).collect();
    let modules: BTreeSet<&String> = before.inner_attrs.keys().chain(after.inner_attrs.keys().filter(|module| !renamed_to.contains(module))).collect();
    for module in modules {
        let target = renames.items.get(module).filter(|new| renamed_to.contains(new)).unwrap_or(module);
        let (old, new) = (before.inner_attrs.get(module), after.inner_attrs.get(target));
        let inline = |side: &Workspace, path: &String| !side.module_files.contains_key(path) && side.items.get(path).is_some_and(|item| item.kind == ItemKind::Mod);
        if inline(before, module) || inline(after, target) {
            continue;
        }
        if [old, new].iter().flatten().any(|(file, _)| skip.contains(file)) {
            continue;
        }
        // A removed target or a removed module file (batch 7b, part 5.2): its inner attributes
        // go with it. The fact of a removed item of the module shows the removal. With no
        // such fact, the attribute edit stays: it is then the only fact of the removal.
        let gone = new.is_none() && old.is_some_and(|(file, _)| !after.parsed_files.contains(file));
        let inside = format!("{module}::");
        if gone && edits.iter().any(|edit| edit.kind == EditKind::ItemRemoved && edit.item().is_some_and(|item| item == module.as_str() || item.starts_with(&inside))) {
            continue;
        }
        // The inner doc lines (`//!`, or a doc attribute) are a doc edit. Each other inner
        // attribute is an attribute edit.
        let is_doc = |attr: &String| attr.starts_with("//!") || crate::doc::code_part(attr).0.len() == 1 && attr.starts_with('#');
        for docs in [true, false] {
            let texts = |entry: Option<&(String, Vec<String>)>| entry.map(|(_, attrs)| attrs.iter().filter(|attr| is_doc(attr) == docs).cloned().collect::<Vec<_>>().join("\n")).unwrap_or_default();
            if texts(old) == texts(new) {
                continue;
            }
            let file = new.or(old).map(|(file, _)| file.as_str()).unwrap_or("");
            let mut edit = Edit::file_edit(if docs { EditKind::Docs } else { EditKind::Attrs }, file);
            edit.view = View::Syntax;
            edit.before = Some(Anchor::item(module.as_str()));
            edit.after = Some(Anchor::item(target.as_str()));
            edit.text_before = Some(texts(old));
            edit.text_after = Some(texts(new));
            // The display texts (design 39.2), in the order of the compare texts.
            let shown = |side: &Workspace, entry: Option<&(String, Vec<String>)>| -> Option<String> {
                let (_, attrs) = entry?;
                let display = side.inner_attrs_display.get(if std::ptr::eq(side, before) { module } else { target }).filter(|display| display.len() == attrs.len())?;
                Some(attrs.iter().zip(display).filter(|(attr, _)| is_doc(attr) == docs).map(|(_, shown)| shown.clone()).collect::<Vec<_>>().join("\n"))
            };
            edit.source_before = shown(before, old).or_else(|| edit.text_before.clone());
            edit.source_after = shown(after, new).or_else(|| edit.text_after.clone());
            edit.line = 1;
            edits.push(edit);
        }
    }
    edits.extend(file_edits(before, after));
    (edits, maps)
}

/// The files that the loader parses on one side only because of a parse error (design 37.5):
/// each file with a parse error, and each file that a side with a parse error has and does not
/// parse (a module of a file that does not parse).
pub fn unparsed_files(before: &Workspace, after: &Workspace) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = before.parse_errors.keys().chain(after.parse_errors.keys()).cloned().collect();
    for (side, other) in [(before, after), (after, before)] {
        if !side.parse_errors.is_empty() {
            out.extend(other.parsed_files.iter().filter(|file| side.rust_files.contains_key(*file) && !side.parsed_files.contains(*file)).cloned());
        }
    }
    out
}

/// The file edits: one edit for each path whose entry (content or mode) or text differs, except a
/// Rust file that the loader parses on each side where it exists. The syntax diff decides about
/// the content of such a file (a change of the formatting only gives no edit). A mode change of
/// such a file still gives a file edit.
fn file_edits(before: &Workspace, after: &Workspace) -> Vec<Edit> {
    let text = |ws: &'_ Workspace, path: &str| ws.other_files.get(path).or_else(|| ws.rust_files.get(path)).cloned();
    let paths: BTreeSet<&String> = [before, after].iter().flat_map(|ws| ws.files.keys().chain(ws.other_files.keys()).chain(ws.rust_files.keys())).collect();
    let mut edits = Vec::new();
    for path in paths {
        let (old, new) = (before.files.get(path), after.files.get(path));
        let (old_text, new_text) = (text(before, path), text(after, path));
        if old == new && old_text == new_text {
            continue;
        }
        let present = |entry: Option<&crate::load::FileEntry>, text: &Option<String>| entry.is_some() || text.is_some();
        let (in_before, in_after) = (present(old, &old_text), present(new, &new_text));
        let mode_changed = matches!((old, new), (Some(a), Some(b)) if a.mode != b.mode);
        let parsed = |ws: &Workspace, exists: bool| !exists || ws.parsed_files.contains(path);
        if parsed(before, in_before) && parsed(after, in_after) && !mode_changed {
            continue;
        }
        let kind = match (in_before, in_after) {
            (false, _) => EditKind::FileAdded,
            (_, false) => EditKind::FileRemoved,
            _ => EditKind::FileChanged,
        };
        // A file without a text is a binary file (or a link or a submodule, by its mode).
        let what = |entry: Option<&crate::load::FileEntry>, text: &Option<String>| match entry.map(|e| e.mode.as_str()) {
            Some("120000") => Some("symbolic link"),
            Some("160000") => Some("submodule"),
            _ if text.is_none() => Some("binary file"),
            _ => None,
        };
        let mut notes = Vec::new();
        let content_changed = old.map(|e| &e.hash) != new.map(|e| &e.hash) || old_text != new_text;
        match kind {
            EditKind::FileAdded => notes.extend(what(new, &new_text).map(|w| format!("new {w}"))),
            EditKind::FileRemoved => notes.extend(what(old, &old_text).map(|w| format!("removed {w}"))),
            _ => {
                if content_changed {
                    if let Some(w) = what(old, &old_text).or(what(new, &new_text)) {
                        notes.push(format!("{w} changed"));
                    }
                }
                if let (true, Some(a), Some(b)) = (mode_changed, old, new) {
                    notes.push(format!("mode {} → {}", a.mode, b.mode));
                }
            }
        }
        let mut edit = Edit::file_edit(kind, path);
        edit.note = (!notes.is_empty()).then(|| notes.join(", "));
        edits.push(edit);
    }
    edits
}

/// True if the item is a member of a trait definition (`shop::Price::label` of
/// `trait Price`). A method of an inherent impl of a type has the same path form.
pub fn in_trait(workspace: &Workspace, path: &str) -> bool {
    path.rsplit_once("::").and_then(|(owner, _)| workspace.items.get(owner)).is_some_and(|owner| owner.kind == ItemKind::Trait)
}

fn uses_by_module(workspace: &Workspace) -> BTreeMap<String, Vec<&Item>> {
    let mut map: BTreeMap<String, Vec<&Item>> = BTreeMap::new();
    for item in workspace.items.values().filter(|item| item.kind == ItemKind::Use) {
        let module = item.path.rsplit_once("::").map(|(module, _)| module.to_string()).unwrap_or_default();
        map.entry(module).or_default().push(item);
    }
    map
}

fn item_edit(kind: EditKind, before: Option<&Item>, after: Option<&Item>) -> Edit {
    let any = after.or(before).expect("an item edit has an item");
    Edit {
        kind,
        view: View::Syntax,
        before: before.map(|item| Anchor::item(&item.path)),
        after: after.map(|item| Anchor::item(&item.path)),
        node_before: None,
        node_after: None,
        text_before: before.map(|item| item.source.clone()),
        text_after: after.map(|item| item.source.clone()),
        attr_before: before.map(|item| kind_name(item.kind).to_string()),
        attr_after: after.map(|item| kind_name(item.kind).to_string()),
        source_before: before.map(|item| item.source.clone()),
        source_after: after.map(|item| item.source.clone()),
        block_like: true,
        file: Some(any.file.clone()),
        line: any.line_start,
        note: None,
        refactor: None,
    }
}

/// The name of an item kind, as in the JSON form.
pub fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Fn => "fn",
        ItemKind::Struct => "struct",
        ItemKind::Enum => "enum",
        ItemKind::Trait => "trait",
        ItemKind::Impl => "impl",
        ItemKind::Const => "const",
        ItemKind::Static => "static",
        ItemKind::Type => "type",
        ItemKind::Use => "use",
        ItemKind::Mod => "mod",
        ItemKind::Macro => "macro",
        ItemKind::Field => "field",
        ItemKind::Variant => "variant",
        ItemKind::Other => "other",
    }
}

/// An edit of one part of an item pair: the two texts of the part.
fn part_edit(before: &Item, after: &Item, kind: EditKind, view: View, old: String, new: String) -> Edit {
    Edit {
        kind,
        view,
        before: Some(Anchor::item(&before.path)),
        after: Some(Anchor::item(&after.path)),
        node_before: None,
        node_after: None,
        text_before: Some(old),
        text_after: Some(new),
        attr_before: None,
        attr_after: None,
        source_before: None,
        source_after: None,
        block_like: false,
        file: Some(after.file.clone()),
        line: after.line_start,
        note: None,
        refactor: None,
    }
}

/// The edits of the doc lines and of the comments of an item pair. A doc line (`///`, `//!`,
/// `#[doc]`) and a plain comment are two different changes: a doc is text of the API, a
/// comment is not. They get a fact each.
fn docs_edits(before: &Item, after: &Item, edits: &mut Vec<Edit>) {
    let split = |lines: &[String]| -> (Vec<String>, Vec<String>) { lines.iter().cloned().partition(|line| is_doc_line(line)) };
    let (before_docs, before_comments) = split(&before.docs);
    let (after_docs, after_comments) = split(&after.docs);
    if before_docs != after_docs {
        edits.push(part_edit(before, after, EditKind::Docs, View::Syntax, before_docs.join("\n"), after_docs.join("\n")));
    }
    if before_comments != after_comments {
        edits.push(part_edit(before, after, EditKind::Comment, View::Syntax, before_comments.join("\n"), after_comments.join("\n")));
    }
}

/// The form of a module item that does not depend on the place of its content (design 56.3).
/// A module with a block has its inner doc lines and its inner attributes in its item. A
/// module with a file gets the ones of its file. The plain comments are not a part of the
/// form: a comment of the content is a token of the content.
fn module_form(side: &Workspace, item: &Item) -> Item {
    let mut item = item.clone();
    item.docs.retain(|line| is_doc_line(line));
    if let Some((_, inner)) = side.inner_attrs.get(&item.path) {
        let display = side.inner_attrs_display.get(&item.path).filter(|display| display.len() == inner.len());
        for (index, attr) in inner.iter().enumerate() {
            if attr.starts_with("//!") {
                item.docs.push(attr.lines().map(|line| line.replacen("//!", "///", 1)).collect::<Vec<_>>().join("\n"));
            } else {
                item.attrs.push(attr.clone());
                item.display.attrs.push(display.map_or(attr, |display| &display[index]).clone());
            }
        }
    }
    item
}

fn diff_item(before: &Item, after: &Item, renames: &Renames, edits: &mut Vec<Edit>, maps: &mut BodyMaps) {
    let attribute_edit = |kind: EditKind, view: View, old: String, new: String| part_edit(before, after, kind, view, old, new);
    let apply = |text: &str| renames.apply(&before.path, text);

    // The display text of a part is the source text (design 39.2): an edit of a part has it in
    // its source fields. The compare text stays in the text fields.
    let shown = |mut edit: Edit, old: &str, new: &str| {
        (edit.source_before, edit.source_after) = (Some(old.to_string()), Some(new.to_string()));
        edit
    };
    if before.vis != after.vis {
        edits.push(shown(attribute_edit(EditKind::Visibility, View::Signature, before.vis.clone(), after.vis.clone()), &before.display.vis, &after.display.vis));
    }
    // A signature with an equal text on the two sides did not change (design 37.10): a rename of
    // a name applies only where the text changed. A different item can have the same name.
    if before.kind != ItemKind::Mod && (renames.exact() || before.signature != after.signature) && apply(&before.signature) != after.signature {
        // A function signature: one edit for each changed part (design 22.3, A5).
        let parts = match (&before.sig, &after.sig) {
            (Some(old), Some(new)) => signature_parts(before, after, old, new, renames),
            _ => Vec::new(),
        };
        if parts.is_empty() {
            // The kind of the item, for the facts: a field type, a const type, a discriminant.
            let mut edit = shown(attribute_edit(EditKind::Signature, View::Signature, before.signature.clone(), after.signature.clone()), &before.display.signature, &after.display.signature);
            edit.attr_before = Some(kind_name(before.kind).to_string());
            edit.attr_after = Some(kind_name(after.kind).to_string());
            edits.push(edit);
        }
        edits.extend(parts);
    }
    docs_edits(before, after, edits);
    if before.attrs != after.attrs {
        edits.push(shown(attribute_edit(EditKind::Attrs, View::Syntax, before.attrs.join("\n"), after.attrs.join("\n")), &before.display.attrs.join("\n"), &after.display.attrs.join("\n")));
    }
    // A renamed field or variant keeps its place in the member list.
    let renamed_member = |member: &String| {
        [format!("{}.{member}", before.path), format!("{}::{member}", before.path)]
            .iter()
            .find_map(|path| renames.items.get(path))
            .map(|new| new.rsplit(['.', ':']).next().unwrap_or(new).to_string())
    };
    // A member of a struct, an enum, or a variant is a bare name (a field, a variant): it changes
    // only with its item. A member of an impl or a trait is a declaration (`fn name`).
    let bare = renames.exact() && matches!(before.kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Variant);
    let old_members: Vec<String> = before.members.iter().map(|member| renamed_member(member).unwrap_or_else(|| if bare { member.clone() } else { apply(member) })).collect();
    // Each member of an impl, a trait, and a block of foreign items is an item with its own
    // edits (design 56.2). The order of these members has no edit: the rule of the item order
    // has the lines of a member with equal tokens at a new place (design 55.5).
    let block = matches!(before.kind, ItemKind::Impl | ItemKind::Trait) || (before.kind == ItemKind::Other && (before.signature.starts_with("extern") || before.signature.starts_with("unsafe extern")));
    if old_members != after.members && !block {
        // The kind of the owner, for the hints (a struct, a variant, an enum, a trait, an impl).
        let list_edit = |old: String, new: String| {
            let mut edit = attribute_edit(EditKind::Members, View::Signature, old, new);
            edit.attr_before = Some(kind_name(before.kind).to_string());
            edit.attr_after = Some(kind_name(after.kind).to_string());
            edit
        };
        edits.push(shown(list_edit(before.members.join("\n"), after.members.join("\n")), &before.display.members.join("\n"), &after.display.members.join("\n")));
    }
    // A function with a body on one side only (design 37.1): a trait method gets a default
    // body, or its default body goes away. The edit has the full text of the body.
    if before.kind == ItemKind::Fn && before.body.is_some() != after.body.is_some() {
        let body_text = |item: &Item| item.body.as_ref().map(|body| node_source(item, body).unwrap_or_else(|| body.text.clone()));
        let mut edit = attribute_edit(EditKind::Signature, View::Syntax, String::new(), String::new());
        (edit.text_before, edit.text_after) = (body_text(before), body_text(after));
        (edit.source_before, edit.source_after) = (edit.text_before.clone(), edit.text_after.clone());
        edit.node_before = before.body.as_ref().map(|_| "body");
        edit.node_after = after.body.as_ref().map(|_| "body");
        edit.block_like = true;
        edits.push(edit);
    }
    if let (Some(old), Some(new)) = (&before.body, &after.body) {
        // The statement mapper (design 41). A body above its limit keeps the rules of 8b.
        let same = old.text == new.text;
        let map = if same { None } else { crate::mapper::map_body(old, new, &|text: &str| renames.apply(&before.path, text)) };
        if !same && map.is_none() {
            maps.over_limit.push(after.path.clone());
        }
        let visited = map.as_ref().map_or(0, |map| map.old.len());
        if let Some(map) = map.as_ref() {
            maps.round3 += map.round3;
            maps.composites += map.composites;
        }
        let mut context = Context {
            renames,
            item_before: &before.path,
            item_after: &after.path,
            file: &after.file,
            source_before: before,
            source_after: after,
            edits: Vec::new(),
            map,
            visited: vec![false; visited],
            consumed: [HashSet::new(), HashSet::new()],
            gap_pairs: HashMap::new(),
            moves: Vec::new(),
            maps: Vec::new(),
            variable_edits: Vec::new(),
            pairs: Vec::new(),
        };
        context.block(old, new, &[], &[]);
        if context.map.is_some() {
            context.finish();
        } else {
            detect_moves(&mut context.edits);
        }
        maps.gap_pairs += context.gap_pairs.len();
        let Context { edits: mut body_edits, variable_edits, maps: body_maps, pairs, .. } = context;
        maps.pairs.entry(before.path.clone()).or_default().extend(pairs);
        body_edits.extend(variable_edits);
        body_edits.sort_by_key(|edit| edit.line);
        edits.extend(body_edits);
        maps.maps.extend(body_maps);
    }
}

/// The edits of the changed parts of a function signature: the qualifiers, the generics, the
/// `where` clause, each parameter (by name: added, removed, or changed), the order of the
/// parameters, and the return type.
fn signature_parts(before: &Item, after: &Item, old: &Node, new: &Node, renames: &Renames) -> Vec<Edit> {
    let apply = |text: &str| renames.apply(&before.path, text);
    let anchor = |item: &Item, role: &str| Anchor::item(&item.path).child(semilla_core::Segment::Role("sig".into())).child(semilla_core::Segment::Role(role.to_string()));
    let edit = |role: &str, kind: &'static str, old_node: Option<&Node>, new_node: Option<&Node>| Edit {
        kind: EditKind::Signature,
        view: View::Signature,
        before: old_node.map(|n| anchor(before, if kind == "param" { &n.role } else { role })),
        after: new_node.map(|n| anchor(after, if kind == "param" { &n.role } else { role })),
        node_before: old_node.map(|_| kind),
        node_after: new_node.map(|_| kind),
        text_before: old_node.map(|n| n.text.clone()),
        text_after: new_node.map(|n| n.text.clone()),
        attr_before: None,
        attr_after: None,
        // The display text of a part: its source on one line (design 39.2).
        source_before: old_node.map(|n| part_source(before, n)),
        source_after: new_node.map(|n| part_source(after, n)),
        block_like: false,
        file: Some(after.file.clone()),
        line: new_node.or(old_node).map(|n| n.start.0).unwrap_or(after.line_start),
        note: None,
        refactor: None,
    };
    let mut edits = Vec::new();
    for role in ["qualifiers", "generics", "where"] {
        let (a, b) = (old.child(role), new.child(role));
        if a.map(|n| apply(&n.text)) != b.map(|n| n.text.clone()) {
            edits.push(edit(role, a.or(b).map(|n| n.kind).unwrap_or("part"), a, b));
        }
    }
    // The parameters by name. A renamed parameter (`rename-local`) has its new name.
    let old_params: Vec<(String, &Node)> = old.children.iter().filter(|c| c.kind == "param").map(|c| (apply(&c.role), c)).collect();
    let new_params: Vec<(String, &Node)> = new.children.iter().filter(|c| c.kind == "param").map(|c| (c.role.clone(), c)).collect();
    for (role, node) in &old_params {
        match new_params.iter().find(|(r, _)| r == role) {
            None => edits.push(edit(role, "param", Some(node), None)),
            Some((_, other)) if apply(&node.text) != other.text => edits.push(edit(role, "param", Some(node), Some(other))),
            Some(_) => {}
        }
    }
    for (role, node) in &new_params {
        if !old_params.iter().any(|(r, _)| r == role) {
            edits.push(edit(role, "param", None, Some(node)));
        }
    }
    let common = |list: &[(String, &Node)], other: &[(String, &Node)]| list.iter().filter(|(r, _)| other.iter().any(|(o, _)| o == r)).map(|(r, _)| r.clone()).collect::<Vec<_>>();
    let (old_order, new_order) = (common(&old_params, &new_params), common(&new_params, &old_params));
    if old_order != new_order {
        let mut order = edit("params", "params", Some(old), Some(new));
        order.text_before = Some(old_order.join(", "));
        order.text_after = Some(new_order.join(", "));
        (order.source_before, order.source_after) = (order.text_before.clone(), order.text_after.clone());
        edits.push(order);
    }
    let (a, b) = (old.child("ret"), new.child("ret"));
    if a.map(|n| apply(&n.text)) != b.map(|n| n.text.clone()) {
        edits.push(edit("ret", "ret", a, b));
    }
    edits
}

/// The source text of a signature part on one line. The qualifiers and an empty part keep
/// their compare text.
fn part_source(item: &Item, node: &Node) -> String {
    if node.kind == "qualifiers" || node.text.is_empty() {
        return node.text.clone();
    }
    node_source(item, node).map(|text| crate::tree::one_line(&text)).unwrap_or_else(|| node.text.clone())
}

struct Context<'a> {
    renames: &'a Renames,
    item_before: &'a str,
    item_after: &'a str,
    file: &'a str,
    source_before: &'a Item,
    source_after: &'a Item,
    edits: Vec<Edit>,
    /// The statement map of the body (design 41). `None`: the rules of batch 8b.
    map: Option<BodyMap<'a>>,
    /// The old statements that the walk compared with their partners.
    visited: Vec<bool>,
    /// The nodes inside an inserted, a deleted, or a replaced node, on the old and the new side.
    consumed: [HashSet<*const Node>; 2],
    /// The statement pairs of the gap rules: old statement → new statement.
    gap_pairs: HashMap<usize, usize>,
    /// The moved pairs: the index of the move edit, the old statement, the new statement.
    moves: Vec<(usize, usize, usize)>,
    /// The pairs for the MIR check of the map (design 41.8).
    maps: Vec<StatementMap>,
    /// The edits of the extracted and the inlined variables. They are not in `edits` during
    /// the walk, so a node compare does not count them as a change of the node.
    variable_edits: Vec<Edit>,
    /// Each node pair of the compare (design 59, B-d).
    pairs: Vec<NodePair>,
}

/// The source text of a node, cut from the source of its item.
fn node_source(item: &Item, node: &Node) -> Option<String> {
    source_between(item, node.start, node.end).map(|text| dedent(&text, node.start.1))
}

/// The source text of the own tokens of a node on one line (design 52.1): `▫` is in place of
/// each child, and `{▫}` is in place of each block.
fn own_source(item: &Item, node: &Node) -> Option<String> {
    let mut out = String::new();
    let mut at = node.start;
    for child in &node.children {
        if child.start < at || child.end > node.end {
            return None;
        }
        out.push_str(&source_between(item, at, child.start)?);
        out.push_str(if child.kind == "block" { "{▫}" } else { crate::tree::CHILD_MARK });
        at = child.end;
    }
    out.push_str(&source_between(item, at, node.end)?);
    Some(crate::tree::one_line(&out))
}

/// Adds the `;` of a statement to its source text when only one side has the `;` (design
/// 52.1). The source text of an expression statement stops before its `;`.
fn show_semicolon(before: &Node, after: &Node, old: &mut Option<String>, new: &mut Option<String>) {
    let has = |node: &Node| node.stmt && node.text.ends_with(';');
    if has(before) == has(after) {
        return;
    }
    if let Some(text) = if has(before) { old } else { new }
        && !text.ends_with(';')
    {
        text.push(';');
    }
}

/// The source text of an item between two places.
fn source_between(item: &Item, start: Pos, end: Pos) -> Option<String> {
    let lines: Vec<&str> = item.source.lines().collect();
    let first = start.0.checked_sub(item.line_start)?;
    let last = end.0.checked_sub(item.line_start)?;
    if last >= lines.len() || first > last {
        return None;
    }
    let mut text = String::new();
    for (index, line) in lines[first..=last].iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let from = if index == 0 { start.1.min(chars.len()) } else { 0 };
        let to = if first + index == last { end.1.min(chars.len()) } else { chars.len() };
        if index > 0 {
            text.push('\n');
        }
        text.extend(&chars[from.min(to)..to]);
    }
    Some(text)
}

/// Removes `column` spaces from the lines after the first, so that the text reads as if the node
/// started at column 0. A line with less indentation loses only its spaces.
fn dedent(text: &str, column: usize) -> String {
    let mut lines = text.lines();
    let Some(first) = lines.next() else { return String::new() };
    let rest = lines.map(|line| {
        let spaces = line.len() - line.trim_start_matches(' ').len();
        line[spaces.min(column)..].to_string()
    });
    std::iter::once(first.to_string()).chain(rest).collect::<Vec<_>>().join("\n")
}

impl Context<'_> {
    /// Keeps a node pair of the compare (design 59, B-d). The compare is strict: a rename map
    /// does not make two tokens equal here. For a block, the place of each statement in the own
    /// tokens does not count: each statement has its own pair or its own edit.
    fn pair(&mut self, before: &Node, after: &Node, block: bool) {
        if before.start.0 == 0 || after.start.0 == 0 {
            return;
        }
        let equal = before.text == after.text;
        let own_equal = equal || {
            let tidy = |text: String| if block { text.split(' ').filter(|word| *word != crate::tree::CHILD_MARK).collect::<Vec<_>>().join(" ") } else { text };
            tidy(before.own_text(&[])) == tidy(after.own_text(&[]))
        };
        self.pairs.push(NodePair { ranges: [(before.start, before.end), (after.start, after.end)], kinds: [before.kind, after.kind], equal, own_equal, graph_only: false });
    }

    /// Keeps the node pairs of the arms of two `match` nodes that the compare replaces (design
    /// 60, R60.3-10). The pairs are only for the control flow graph: they give no edit, and the
    /// positions of the facts do not read them. The arms with equal parts pair in order (a
    /// longest common subsequence); then, between two such arms, the arms with an equal
    /// pattern and an equal guard pair in order. Each part of a pair of arms (the pattern, the
    /// guard, the body) is a pair; the scrutinee is a pair too.
    fn arm_pairs(&mut self, before: &Node, after: &Node) {
        // The parts of each arm, by the index in the role `arm[N].part`.
        fn arms(node: &Node) -> (Option<&Node>, Vec<Vec<(&str, &Node)>>) {
            let mut scrutinee = None;
            let mut out: Vec<Vec<(&str, &Node)>> = Vec::new();
            for child in &node.children {
                let Some(rest) = child.role.strip_prefix("arm[") else {
                    if child.role == "scrutinee" {
                        scrutinee = Some(child);
                    }
                    continue;
                };
                let Some((index, part)) = rest.split_once("].") else { continue };
                let Ok(index) = index.parse::<usize>() else { continue };
                while out.len() <= index {
                    out.push(Vec::new());
                }
                out[index].push((part, child));
            }
            (scrutinee, out)
        }
        let (old_scrutinee, old) = arms(before);
        let (new_scrutinee, new) = arms(after);
        let keep = |a: &Node, b: &Node, pairs: &mut Vec<NodePair>| {
            if a.start.0 > 0 && b.start.0 > 0 {
                let equal = a.text == b.text;
                pairs.push(NodePair { ranges: [(a.start, a.end), (b.start, b.end)], kinds: [a.kind, b.kind], equal, own_equal: equal, graph_only: true });
            }
        };
        if let (Some(a), Some(b)) = (old_scrutinee, new_scrutinee) {
            keep(a, b, &mut self.pairs);
        }
        let texts = |arm: &Vec<(&str, &Node)>| arm.iter().map(|(part, node)| (part.to_string(), node.text.clone())).collect::<Vec<_>>();
        let head = |arm: &Vec<(&str, &Node)>| arm.iter().filter(|(part, _)| *part != "body").map(|(part, node)| (part.to_string(), node.text.clone())).collect::<Vec<_>>();
        let (old_texts, new_texts): (Vec<_>, Vec<_>) = (old.iter().map(texts).collect(), new.iter().map(texts).collect());
        let equal = lcs(&old_texts, &new_texts, |a, b| a == b);
        let mut paired: Vec<(usize, usize)> = equal.clone();
        let (mut i, mut j) = (0, 0);
        for (pi, pj) in equal.into_iter().chain(std::iter::once((old.len(), new.len()))) {
            let (old_heads, new_heads): (Vec<_>, Vec<_>) = (old[i..pi].iter().map(head).collect(), new[j..pj].iter().map(head).collect());
            paired.extend(lcs(&old_heads, &new_heads, |a, b| !a.is_empty() && a == b).into_iter().map(|(a, b)| (i + a, j + b)));
            i = pi + 1;
            j = pj + 1;
        }
        paired.sort();
        for (a, b) in paired {
            for (part, node) in &old[a] {
                if let Some((_, other)) = new[b].iter().find(|(other_part, _)| other_part == part) {
                    keep(node, other, &mut self.pairs);
                }
            }
        }
    }

    /// True if a node did not change: the text is equal, or the text with the renames is equal.
    /// A rename of a name applies only where the text changed (design 37.10): a different item
    /// can have the same name (`a.len()` and `b.len()`). A place that keeps the old name and gets
    /// a different meaning shows in the compare of the compiled code.
    fn same(&self, before: &Node, after: &Node) -> bool {
        // With a site table the map is exact (design 51.3): a site that keeps the old name is a
        // difference.
        (!self.renames.exact() && before.text == after.text) || self.renames.apply(self.item_before, &before.text) == after.text
    }

    /// True if the own tokens of two paired nodes are different after the map of the step
    /// (design 52.1). The map applies to the full text of the old node, so each name has the
    /// key of its place. With `marks`, the place of each child in the own tokens counts.
    fn own_changed(&self, before: &Node, after: &Node, marks: bool) -> bool {
        let tidy = |text: String| if marks { text } else { text.split(' ').filter(|word| *word != crate::tree::CHILD_MARK).collect::<Vec<_>>().join(" ") };
        let new = tidy(after.own_text(&[]));
        if self.renames.exact() {
            return tidy(before.own_text(&self.renames.spans(self.item_before, &before.text))) != new;
        }
        let old = tidy(before.own_text(&[]));
        old != new && self.renames.apply(self.item_before, &old) != new
    }

    /// The edit of a node with a block whose own tokens changed (design 52.1). The parts of the
    /// node keep their edits. The texts of this edit show the own tokens only.
    fn own_edit(&mut self, before: &Node, after: &Node, before_path: &[String], after_path: &[String]) {
        self.edit(EditKind::Modified, Some((before, before_path)), Some((after, after_path)));
        if let Some(edit) = self.edits.last_mut() {
            (edit.text_before, edit.text_after) = (Some(before.own_text(&[])), Some(after.own_text(&[])));
            (edit.source_before, edit.source_after) = (own_source(self.source_before, before), own_source(self.source_after, after));
            show_semicolon(before, after, &mut edit.source_before, &mut edit.source_after);
        }
    }

    fn anchor(item: &str, path: &[String]) -> Anchor {
        let text = std::iter::once(item.to_string()).chain(path.iter().cloned()).collect::<Vec<_>>().join("/");
        text.parse().unwrap_or_else(|_| Anchor::item(item))
    }

    fn edit(&mut self, kind: EditKind, before: Option<(&Node, &[String])>, after: Option<(&Node, &[String])>) {
        let line = after.or(before).map(|(node, _)| node.start.0).unwrap_or(0);
        // The statements inside an inserted, a deleted, or a replaced node show in its text.
        if self.map.is_some() && matches!(kind, EditKind::Inserted | EditKind::Deleted | EditKind::Replaced) {
            for (side, found) in [before, after].into_iter().enumerate() {
                if let Some((node, _)) = found.filter(|(node, _)| node.has_block()) {
                    let consumed = &mut self.consumed[side];
                    node.walk(&mut Vec::new(), &mut |_, inner| {
                        consumed.insert(at(inner));
                    });
                }
            }
        }
        self.edits.push(Edit {
            kind,
            view: View::Syntax,
            before: before.map(|(_, path)| Self::anchor(self.item_before, path)),
            after: after.map(|(_, path)| Self::anchor(self.item_after, path)),
            node_before: before.map(|(node, _)| node.kind),
            node_after: after.map(|(node, _)| node.kind),
            text_before: before.map(|(node, _)| node.text.clone()),
            text_after: after.map(|(node, _)| node.text.clone()),
            attr_before: before.and_then(|(node, _)| node.attr.clone()),
            attr_after: after.and_then(|(node, _)| node.attr.clone()),
            source_before: before.and_then(|(node, _)| node_source(self.source_before, node)),
            source_after: after.and_then(|(node, _)| node_source(self.source_after, node)),
            block_like: before.is_some_and(|(node, _)| node.has_block()) || after.is_some_and(|(node, _)| node.has_block()),
            file: Some(self.file.to_string()),
            line,
            note: None,
            refactor: None,
        });
        // Two equal source texts keep the fact of a statement that only gets or loses its `;`.
        if let (Some((old, _)), Some((new, _)), Some(edit)) = (before, after, self.edits.last_mut())
            && edit.source_before != edit.source_after
        {
            show_semicolon(old, new, &mut edit.source_before, &mut edit.source_after);
        }
    }

    /// Compares two blocks. `before_path` and `after_path` are the role paths of the blocks.
    fn block(&mut self, before: &Node, after: &Node, before_path: &[String], after_path: &[String]) {
        self.pair(before, after, true);
        if self.map.is_some() {
            self.block_mapped(before, after, before_path, after_path);
            return;
        }
        let old: Vec<&Node> = before.children.iter().collect();
        let new: Vec<&Node> = after.children.iter().collect();
        let pairs = lcs(&old, &new, |a, b| self.same(a, b));
        for (pi, pj) in &pairs {
            self.pair(old[*pi], new[*pj], false);
        }
        let (mut i, mut j) = (0, 0);
        for (pi, pj) in pairs.into_iter().chain(std::iter::once((old.len(), new.len()))) {
            self.gap(&old[i..pi], &new[j..pj], &old, &new, before_path, after_path);
            i = pi + 1;
            j = pj + 1;
        }
    }

    /// The block of a mapped body (design 41). The mapped statements of the two blocks in
    /// order are in place. A statement whose partner is in this block at a different order, or
    /// in a different block, is moved. Each statement with no partner goes to the gap rules.
    fn block_mapped(&mut self, before: &Node, after: &Node, before_path: &[String], after_path: &[String]) {
        let old: Vec<&Node> = before.children.iter().collect();
        let new: Vec<&Node> = after.children.iter().collect();
        let map = self.map.as_ref().expect("a mapped body");
        let old_ids: Vec<Option<usize>> = old.iter().map(|node| map.old_at.get(&at(node)).copied()).collect();
        let new_ids: Vec<Option<usize>> = new.iter().map(|node| map.new_at.get(&at(node)).copied()).collect();
        // The partner of each old statement in this block.
        let here: Vec<Option<usize>> = old_ids.iter().map(|id| id.and_then(|o| map.pair_old[o]).and_then(|n| new_ids.iter().position(|x| *x == Some(n)))).collect();
        let here_new: Vec<bool> = (0..new.len()).map(|j| here.contains(&Some(j))).collect();
        // A statement with a partner in a different block, or the `let` of a variable.
        let variable = |id: Option<usize>, inline: bool| id.and_then(|id| map.variables.iter().position(|v| v.inline == inline && v.stmt == id));
        let away_old: Vec<bool> = old_ids.iter().enumerate().map(|(k, id)| here[k].is_none() && (id.is_some_and(|o| map.pair_old[o].is_some()) || variable(*id, true).is_some())).collect();
        let away_new: Vec<bool> = new_ids.iter().enumerate().map(|(j, id)| !here_new[j] && (id.is_some_and(|n| map.pair_new[n].is_some()) || variable(*id, false).is_some())).collect();
        let old_variables: Vec<Option<usize>> = old_ids.iter().map(|id| variable(*id, true)).collect();
        let new_variables: Vec<Option<usize>> = new_ids.iter().map(|id| variable(*id, false)).collect();
        let indexes = |count: usize| (0..count).collect::<Vec<usize>>();
        let pairs = lcs(&indexes(old.len()), &indexes(new.len()), |a, b| here[*a] == Some(*b));
        let child_path = |path: &[String], node: &Node| {
            let mut path = path.to_vec();
            path.push(node.role.clone());
            path
        };
        let in_order: BTreeSet<usize> = pairs.iter().map(|pair| pair.0).collect();
        for (k, partner) in here.iter().enumerate() {
            if let Some(j) = partner {
                self.visit(old[k], new[*j], child_path(before_path, old[k]), child_path(after_path, new[*j]), !in_order.contains(&k));
            }
        }
        // The `let` of an extracted or an inlined variable.
        for (k, found) in old_variables.iter().enumerate() {
            if let Some(index) = found {
                let path = child_path(before_path, old[k]);
                self.variable_edit(*index, EditKind::Deleted, Some((old[k], &path)), None);
            }
        }
        for (j, found) in new_variables.iter().enumerate() {
            if let Some(index) = found {
                let path = child_path(after_path, new[j]);
                self.variable_edit(*index, EditKind::Inserted, None, Some((new[j], &path)));
            }
        }
        // The gaps between the statements in order: the statements with no partner.
        let (mut i, mut j) = (0, 0);
        for (pi, pj) in pairs.into_iter().chain(std::iter::once((old.len(), new.len()))) {
            let gap_old: Vec<&Node> = (i..pi).filter(|&k| here[k].is_none() && !away_old[k]).map(|k| old[k]).collect();
            let gap_new: Vec<&Node> = (j..pj).filter(|&k| !here_new[k] && !away_new[k]).map(|k| new[k]).collect();
            self.gap(&gap_old, &gap_new, &old, &new, before_path, after_path);
            i = pi + 1;
            j = pj + 1;
        }
    }

    /// An edit of an extracted or an inlined variable.
    fn variable_edit(&mut self, index: usize, kind: EditKind, before: Option<(&Node, &[String])>, after: Option<(&Node, &[String])>) {
        let Some(variable) = self.map.as_ref().map(|map| map.variables[index].clone()) else { return };
        self.edit(kind, before, after);
        let Some(mut edit) = self.edits.pop() else { return };
        // The display text of the expression: the source of the `let` initializer.
        let init = |item: &Item, node: Option<(&Node, &[String])>| node.and_then(|(node, _)| node.child("init")).and_then(|init| node_source(item, init)).map(|text| crate::tree::one_line(&text));
        let expr = init(self.source_after, after).or_else(|| init(self.source_before, before)).unwrap_or_else(|| variable.expr.clone());
        let region = self.variable_region(&variable);
        edit.refactor = Some(if variable.inline { Refactor::Inline { var: variable.var, expr, region } } else { Refactor::Extract { var: variable.var, expr, region } });
        self.variable_edits.push(edit);
    }

    /// The region of the `let` of a variable (design 41.7): the statements from the `let` to
    /// the end of its block, and on the other side the statements from the partner of the next
    /// mapped statement to the end of that block. The locals of these statements get new numbers.
    fn variable_region(&self, variable: &crate::mapper::Variable) -> [Vec<(Pos, Pos)>; 2] {
        let mut out: [Vec<(Pos, Pos)>; 2] = [Vec::new(), Vec::new()];
        let Some(map) = self.map.as_ref() else { return out };
        let (side, own, other) = if variable.inline { (0, &map.old, &map.new) } else { (1, &map.new, &map.old) };
        let at = &own[variable.stmt];
        let siblings = |stmts: &[crate::mapper::Stmt], parent: Option<usize>, block: &str, from: usize| -> Vec<(Pos, Pos)> { stmts.iter().filter(|stmt| stmt.parent == parent && stmt.block == block && stmt.index >= from).map(|stmt| (stmt.node.start, stmt.node.end)).collect() };
        out[side] = siblings(own, at.parent, &at.block, at.index);
        let partner = |index: usize| if variable.inline { map.pair_old[index].or_else(|| self.gap_pairs.get(&index).copied()) } else { map.pair_new[index].or_else(|| self.gap_pairs.iter().find(|(_, new)| **new == index).map(|(old, _)| *old)) };
        let next = (0..own.len()).filter(|&s| own[s].parent == at.parent && own[s].block == at.block && own[s].index > at.index).filter_map(partner).min_by_key(|&t| other[t].index);
        if let Some(t) = next {
            out[1 - side] = siblings(other, other[t].parent, &other[t].block, other[t].index);
        }
        out
    }

    /// Compares a mapped pair. A moved pair gets a move edit; then the compare of the two nodes
    /// gives an edit for each other change.
    fn visit(&mut self, old: &Node, new: &Node, before_path: Vec<String>, after_path: Vec<String>, moved: bool) {
        let Some(map) = self.map.as_ref() else { return };
        let (Some(&o), Some(&n)) = (map.old_at.get(&at(old)), map.new_at.get(&at(new))) else { return };
        if self.visited[o] {
            return;
        }
        self.visited[o] = true;
        let same = self.same(old, new);
        if same {
            self.mark_visited(old, new);
        }
        if moved || !same {
            let anchor = |item: &str, path: &[String]| Self::anchor(item, path);
            self.maps.push(StatementMap { before: anchor(self.item_before, &before_path), after: anchor(self.item_after, &after_path), moved });
        }
        if moved {
            self.edit(EditKind::Moved, Some((old, &before_path)), Some((new, &after_path)));
            if let Some(edit) = self.edits.last_mut() {
                edit.refactor = Some(Refactor::Moved { jumped: [Vec::new(), Vec::new()] });
            }
            self.moves.push((self.edits.len() - 1, o, n));
        }
        self.node(old, new, &before_path, &after_path);
    }

    /// The mapped pairs that the walk of the blocks did not reach: a statement that moves to a
    /// different block. Then the places that each move passes over.
    fn finish(&mut self) {
        let pending: Vec<(usize, usize)> = {
            let Some(map) = self.map.as_ref() else { return };
            (0..map.old.len()).filter_map(|o| map.pair_old[o].map(|n| (o, n))).collect()
        };
        for (o, n) in pending {
            let map = self.map.as_ref().expect("a mapped body");
            let (old, new) = (map.old[o].node, map.new[n].node);
            if self.visited[o] || (self.consumed[0].contains(&at(old)) && self.consumed[1].contains(&at(new))) {
                continue;
            }
            let (before_path, after_path) = (map.old[o].path.clone(), map.new[n].path.clone());
            self.visit(old, new, before_path, after_path, true);
        }
        for (edit, o, n) in std::mem::take(&mut self.moves) {
            let jumped = self.jumped(o, n);
            if let Some(Refactor::Moved { jumped: places }) = self.edits.get_mut(edit).and_then(|edit| edit.refactor.as_mut()) {
                *places = jumped;
            }
        }
    }

    /// The places of the statements that a move passes over (design 41.7), on the two sides. In
    /// the lowest block pair that contains the two places: each statement from the first
    /// statement whose order to the moved statement changes to the end of the block, and the
    /// statement that the move leaves or enters.
    fn jumped(&self, o: usize, n: usize) -> [Vec<(Pos, Pos)>; 2] {
        let Some(map) = self.map.as_ref() else { return [Vec::new(), Vec::new()] };
        let partner = |x: usize| map.pair_old[x].or_else(|| self.gap_pairs.get(&x).copied());
        let partner_new = |y: usize| map.pair_new[y].or_else(|| self.gap_pairs.iter().find(|(_, b)| **b == y).map(|(a, _)| *a));
        let chain = |stmts: &[crate::mapper::Stmt], mut at: usize| {
            let mut out = vec![at];
            while let Some(parent) = stmts[at].parent {
                out.push(parent);
                at = parent;
            }
            out
        };
        let (old_chain, new_chain) = (chain(&map.old, o), chain(&map.new, n));
        // The lowest old ancestor whose partner is an ancestor of the new statement.
        let mut top = (old_chain.len() - 1, new_chain.len() - 1);
        for (a, &ancestor) in old_chain.iter().enumerate().skip(1) {
            if let Some(b) = partner(ancestor).and_then(|p| new_chain.iter().skip(1).position(|&q| q == p)) {
                top = (a - 1, b);
                break;
            }
        }
        let (x, y) = (old_chain[top.0], new_chain[top.1]);
        let mut out: [Vec<(Pos, Pos)>; 2] = [Vec::new(), Vec::new()];
        let (sx, sy) = (&map.old[x], &map.new[y]);
        // The first statement of each block whose order to the moved statement changes.
        let (mut first_old, mut first_new) = (sx.index, sy.index);
        if sx.block == sy.block {
            for s in (0..map.old.len()).filter(|&s| s != x && map.old[s].parent == sx.parent && map.old[s].block == sx.block) {
                let Some(t) = partner(s).filter(|&t| map.new[t].parent == sy.parent && map.new[t].block == sy.block && t != y) else { continue };
                if (map.old[s].index < sx.index) != (map.new[t].index < sy.index) {
                    first_old = first_old.min(map.old[s].index);
                    first_new = first_new.min(map.new[t].index);
                }
            }
        }
        // The statements from there to the end of the block: the move passes over them, or
        // their locals get new numbers.
        for (side, stmts, at, first) in [(0, &map.old, sx, first_old), (1, &map.new, sy, first_new)] {
            for stmt in stmts.iter().filter(|stmt| stmt.parent == at.parent && stmt.block == at.block && stmt.index >= first) {
                out[side].push((stmt.node.start, stmt.node.end));
            }
        }
        // The statement that the move leaves or enters, on its other side too.
        if x != o
            && let Some(t) = partner(x)
        {
            out[1].push((map.new[t].node.start, map.new[t].node.end));
        }
        if y != n
            && let Some(s) = partner_new(y)
        {
            out[0].push((map.old[s].node.start, map.old[s].node.end));
        }
        out
    }

    /// The gap rules of batch 8b: the statements of a gap with no partner pair by kind, in
    /// order. `old` and `new` are all statements of the two blocks.
    fn gap(&mut self, gap_old: &[&Node], gap_new: &[&Node], old: &[&Node], new: &[&Node], before_path: &[String], after_path: &[String]) {
        let child_path = |path: &[String], node: &Node| {
            let mut path = path.to_vec();
            path.push(node.role.clone());
            path
        };
        {
            let mut used_new = vec![false; gap_new.len()];
            let mut unpaired_old = Vec::new();
            let mut next = 0;
            // The same number of statements, with the same kinds in order: each statement changed in
            // place, also when its text is very different (for example a long string literal).
            // A statement with the same text on the other side of the block is a move candidate,
            // so it does not pair by position.
            let moved = |a: &Node, b: &Node| {
                let a_text = self.renames.apply(self.item_before, &a.text);
                new.iter().any(|n| n.text == a_text) || old.iter().any(|o| self.renames.apply(self.item_before, &o.text) == b.text)
            };
            let positional = gap_old.len() == gap_new.len() && gap_old.iter().zip(gap_new).all(|(a, b)| a.kind == b.kind && !moved(a, b));
            if positional {
                for (old_node, new_node) in gap_old.iter().zip(gap_new) {
                    let (bp, ap) = (child_path(before_path, old_node), child_path(after_path, new_node));
                    self.gap_pair(old_node, new_node);
                    self.node(old_node, new_node, &bp, &ap);
                }
                return;
            }
            for old_node in gap_old {
                // Pair with the most similar new statement of the same kind, in order.
                let old_text = self.renames.apply(self.item_before, &old_node.text);
                let candidate = (next..gap_new.len())
                    .filter(|&k| same_kind(old_node, gap_new[k]))
                    .map(|k| (k, similarity(&old_text, &gap_new[k].text)))
                    .filter(|&(_, score)| score >= 0.25)
                    .max_by(|a, b| a.1.total_cmp(&b.1));
                match candidate {
                    Some((k, _)) => {
                        used_new[k] = true;
                        next = k + 1;
                        let (bp, ap) = (child_path(before_path, old_node), child_path(after_path, gap_new[k]));
                        self.gap_pair(old_node, gap_new[k]);
                        self.node(old_node, gap_new[k], &bp, &ap);
                    }
                    None => unpaired_old.push(*old_node),
                }
            }
            for old_node in unpaired_old {
                let bp = child_path(before_path, old_node);
                self.edit(EditKind::Deleted, Some((old_node, &bp)), None);
            }
            for (k, new_node) in gap_new.iter().enumerate() {
                if !used_new[k] {
                    let ap = child_path(after_path, new_node);
                    self.edit(EditKind::Inserted, None, Some((new_node, &ap)));
                }
            }
        }
    }

    /// Marks the statements of two equal nodes as compared, if their partners are inside the new
    /// node. A statement whose partner is outside keeps its move.
    fn mark_visited(&mut self, old: &Node, new: &Node) {
        let Some(map) = self.map.as_ref() else { return };
        let inside = |index: &HashMap<*const Node, usize>, node: &Node| -> Vec<usize> {
            let mut out = Vec::new();
            node.walk(&mut Vec::new(), &mut |_, inner| out.extend(index.get(&at(inner)).copied()));
            out
        };
        let news: BTreeSet<usize> = inside(&map.new_at, new).into_iter().collect();
        for x in inside(&map.old_at, old) {
            if map.pair_old[x].is_some_and(|y| news.contains(&y)) {
                self.visited[x] = true;
            }
        }
    }

    /// Keeps a statement pair of the gap rules, for the places of a move.
    fn gap_pair(&mut self, old: &Node, new: &Node) {
        if let Some(map) = self.map.as_ref()
            && let (Some(&o), Some(&n)) = (map.old_at.get(&at(old)), map.new_at.get(&at(new)))
        {
            self.gap_pairs.insert(o, n);
        }
    }

    /// Compares two chains of one binary operator with different operand counts (trial 4, D7:
    /// `a + b` → `a + b + c` gave the two edits `a` → `a + b` and `b` → `c`). The result is one
    /// edit for the full chain. If the change only adds or only removes operands, the note of the
    /// edit names them: "added operand `+ c`". Returns false if the nodes are not such chains.
    fn operand_chain(&mut self, before: &Node, after: &Node, before_path: &[String], after_path: &[String]) -> bool {
        if before.kind != "binary" || after.kind != "binary" || before.attr.is_none() || before.attr != after.attr {
            return false;
        }
        let operator = before.attr.as_deref().unwrap_or("");
        let (old, new) = (operands(before, operator), operands(after, operator));
        if old.len() == new.len() {
            return false;
        }
        let pairs = lcs(&old, &new, |a, b| self.same(a, b));
        if pairs.len() != old.len().min(new.len()) {
            // Operands changed, and the count changed: the full expression is the change.
            self.edit(EditKind::Replaced, Some((before, before_path)), Some((after, after_path)));
            return true;
        }
        let added = new.len() > old.len();
        let (list, item, kept): (&[&Node], &Item, Vec<usize>) = if added { (&new, self.source_after, pairs.iter().map(|pair| pair.1).collect()) } else { (&old, self.source_before, pairs.iter().map(|pair| pair.0).collect()) };
        let texts: Vec<String> = (0..list.len())
            .filter(|index| !kept.contains(index))
            .map(|index| {
                let text = crate::tree::one_line(&node_source(item, list[index]).unwrap_or_else(|| list[index].text.clone()));
                if index == 0 { format!("`{text} {operator}`") } else { format!("`{operator} {text}`") }
            })
            .collect();
        let note = format!("{} operand{} {}", if added { "added" } else { "removed" }, if texts.len() == 1 { "" } else { "s" }, texts.join(", "));
        self.edit(EditKind::Modified, Some((before, before_path)), Some((after, after_path)));
        if let Some(edit) = self.edits.last_mut() {
            edit.note = Some(note);
        }
        true
    }

    /// Compares two paired nodes.
    fn node(&mut self, before: &Node, after: &Node, before_path: &[String], after_path: &[String]) {
        self.pair(before, after, false);
        if self.same(before, after) {
            // The statements inside an equal node with a block are in place.
            if self.map.is_some() && before.has_block() {
                self.mark_visited(before, after);
            }
            return;
        }
        // A use of an extracted or an inlined variable (design 41.5): one edit with the mark of
        // the variable.
        let variable = self.map.as_ref().and_then(|map| map.variable_use(&map.old_text(&self.renames.apply(self.item_before, &before.text)), before, after).map(|index| map.variables[index].clone()));
        if let Some(variable) = variable {
            self.edit(EditKind::Replaced, Some((before, before_path)), Some((after, after_path)));
            if let Some(mut edit) = self.edits.pop() {
                let region = [Vec::new(), Vec::new()];
                edit.refactor = Some(if variable.inline { Refactor::Inline { var: variable.var.clone(), expr: String::new(), region } } else { Refactor::Extract { var: variable.var.clone(), expr: String::new(), region } });
                self.variable_edits.push(edit);
            }
            return;
        }
        if self.operand_chain(before, after, before_path, after_path) {
            return;
        }
        // A wrap or an unwrap of an expression in a mapped body (`Ok(f(x))` → `f(x)`, design
        // 41.3): one edit for the node, not an edit for each part.
        if let Some(map) = self.map.as_ref().filter(|_| before.kind == after.kind && !before.has_block() && !after.has_block()) {
            let old_text = |node: &Node| map.old_text(&self.renames.apply(self.item_before, &node.text));
            let inside = |node: &Node, text: &str, rename: bool| {
                let mut found = false;
                for child in &node.children {
                    child.walk(&mut Vec::new(), &mut |_, inner| found |= (if rename { old_text(inner) } else { inner.text.clone() }) == text);
                }
                found
            };
            if inside(before, &after.text, true) || inside(after, &old_text(before), false) {
                self.edit(EditKind::Replaced, Some((before, before_path)), Some((after, after_path)));
                return;
            }
        }
        let roles = |node: &Node| node.children.iter().map(|child| child.role.clone()).collect::<Vec<_>>();
        let leaf = before.children.is_empty() && after.children.is_empty();
        if before.kind != after.kind || roles(before) != roles(after) || leaf {
            self.edit(EditKind::Replaced, Some((before, before_path)), Some((after, after_path)));
            if before.kind == "match" && after.kind == "match" {
                self.arm_pairs(before, after);
            }
            return;
        }
        let edits_before = self.edits.len();
        let variables_before = self.variable_edits.len();
        for (old_child, new_child) in before.children.iter().zip(&after.children) {
            let mut bp = before_path.to_vec();
            bp.push(old_child.role.clone());
            let mut ap = after_path.to_vec();
            ap.push(new_child.role.clone());
            if old_child.kind == "block" && new_child.kind == "block" {
                if !self.same(old_child, new_child) {
                    // The own tokens of a block: its delimiters and its inner attributes. The
                    // count of the statements can be different: each statement has its own
                    // edit. The item compare has the inner attributes of the body of an item.
                    if self.own_changed(old_child, new_child, false) {
                        self.own_edit(old_child, new_child, &bp, &ap);
                    }
                    self.block(old_child, new_child, &bp, &ap);
                } else {
                    self.pair(old_child, new_child, true);
                    self.mark_visited(old_child, new_child);
                }
            } else {
                self.node(old_child, new_child, &bp, &ap);
            }
        }
        // The own tokens of the node (design 52.1): each token that is not in a child. The
        // rule does not depend on the kind of the node.
        let own_changed = self.own_changed(before, after, true);
        let parts_changed = self.edits.len() > edits_before;
        if own_changed && parts_changed && !before.has_block() && !after.has_block() {
            // The node itself and a part of it changed (`a * b / 100` → `a / 100 * b`): one edit
            // for the full expression, not one edit for the node and one for each part (trial 4,
            // D7: one expression edit gave up to four facts that overlap).
            self.edits.truncate(edits_before);
            self.edit(EditKind::Replaced, Some((before, before_path)), Some((after, after_path)));
            return;
        }
        // In a mapped body, a statement that moves in or out of a block, and a use of a
        // variable, have their own edits: a composite whose other text is equal has no edit of
        // its own.
        let explained = self.map.as_ref().is_some_and(|map| self.variable_edits.len() > variables_before || map.old_text(&self.renames.apply(self.item_before, &crate::mapper::shell(before))) == crate::mapper::shell(after));
        if own_changed && parts_changed {
            // A node with a block: its parts keep their edits, and the node gets one edit for
            // its own tokens.
            self.own_edit(before, after, before_path, after_path);
        } else if own_changed || (self.edits.len() == edits_before && !explained) {
            // The node itself changed: an operator, a method name, or text outside the children.
            self.edit(EditKind::Modified, Some((before, before_path)), Some((after, after_path)));
        }
    }
}

/// The operands of a chain of one binary operator, from left to right: `a + b + c` is
/// `(a + b) + c` in the tree, and its operands are `a`, `b`, `c`. Only the left side continues a
/// chain, as in the syntax of Rust.
fn operands<'a>(node: &'a Node, operator: &str) -> Vec<&'a Node> {
    match (node.children.first(), node.children.get(1)) {
        (Some(lhs), Some(rhs)) if node.kind == "binary" && node.attr.as_deref() == Some(operator) && node.children.len() == 2 => {
            let mut list = operands(lhs, operator);
            list.push(rhs);
            list
        }
        _ => vec![node],
    }
}

/// True if two statements can be a pair of "the same statement, changed".
pub(crate) fn same_kind(a: &Node, b: &Node) -> bool {
    let expression = |kind: &str| !matches!(kind, "let" | "item_stmt" | "macro");
    a.kind == b.kind || (expression(a.kind) && expression(b.kind) && a.role.starts_with('S') == b.role.starts_with('S') && !a.has_block() && !b.has_block())
}

/// The share of tokens that two texts have in common (0 to 1).
fn similarity<'a>(a: &'a str, b: &'a str) -> f64 {
    // Only identifiers and literals count. Punctuation is common to most statements.
    let words = |text: &'a str| -> BTreeSet<&'a str> { text.split(' ').filter(|t| t.chars().any(|c| c.is_alphanumeric())).collect() };
    let (left, right) = (words(a), words(b));
    let union = left.union(&right).count();
    if union == 0 { 1.0 } else { left.intersection(&right).count() as f64 / union as f64 }
}

/// Converts a deleted and an inserted statement with the same text in one item into a move.
fn detect_moves(edits: &mut Vec<Edit>) {
    let mut index = 0;
    while index < edits.len() {
        if edits[index].kind == EditKind::Deleted {
            let text = edits[index].text_before.clone();
            if let Some(other) = edits.iter().position(|edit| edit.kind == EditKind::Inserted && edit.text_after == text) {
                let inserted = edits.remove(other);
                let index = if other < index { index - 1 } else { index };
                let edit = &mut edits[index];
                edit.kind = EditKind::Moved;
                edit.after = inserted.after;
                edit.node_after = inserted.node_after;
                edit.text_after = inserted.text_after;
                edit.source_after = inserted.source_after;
                edit.line = inserted.line;
            }
        }
        index += 1;
    }
}

/// The longest common subsequence of two lists, as index pairs.
pub(crate) fn lcs<T>(a: &[T], b: &[T], equal: impl Fn(&T, &T) -> bool) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut table = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if equal(&a[i], &b[j]) { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut pairs) = (0, 0, Vec::new());
    while i < n && j < m {
        if equal(&a[i], &b[j]) {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}
