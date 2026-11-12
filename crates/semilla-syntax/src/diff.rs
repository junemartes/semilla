//! The syntax diff of two workspaces.
//!
//! The diff goes top-down (design section 8.6):
//! 1. Match the items. The item map from the identity claims comes first; then the same path.
//! 2. Compare the attributes of each matched item: visibility, signature, docs, members.
//! 3. Align the statements of each block (a longest common subsequence on the text), and pair the
//!    statements in each gap that have the same kind.
//! 4. Compare paired nodes. The diff stops at the lowest node that contains the change.
//!
//! The renames from the claims are applied to the "before" side first, so a claimed rename gives
//! no edit.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{Anchor, View};
use serde::Serialize;

use crate::load::{Item, ItemKind, Workspace};
use crate::tree::Node;

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
}

impl Renames {
    /// Applies the identifier renames to a token text, for an item on the "before" side.
    pub fn apply(&self, item: &str, text: &str) -> String {
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
    let mut edits = Vec::new();
    let mut matched_after = BTreeSet::new();

    let (before_uses, after_uses) = (uses_by_module(before), uses_by_module(after));
    for (path, item) in before.items.iter().filter(|(_, item)| item.kind != ItemKind::Use) {
        let target = renames.items.get(path).cloned().unwrap_or_else(|| path.clone());
        match after.items.get(&target) {
            Some(other) if other.kind == item.kind => {
                matched_after.insert(target.clone());
                diff_item(item, other, renames, &mut edits);
            }
            _ => edits.push(item_edit(EditKind::ItemRemoved, Some(item), None)),
        }
    }
    for (path, item) in after.items.iter().filter(|(_, item)| item.kind != ItemKind::Use) {
        if !matched_after.contains(path) {
            edits.push(item_edit(EditKind::ItemAdded, None, Some(item)));
        }
    }

    // `use` items have no stable position, so they are compared as a set in each module.
    let modules: BTreeSet<&String> = before_uses.keys().chain(after_uses.keys()).collect();
    for module in modules {
        let old = before_uses.get(module).cloned().unwrap_or_default();
        let new = after_uses.get(module).cloned().unwrap_or_default();
        for item in old.iter().filter(|item| !new.iter().any(|other| other.signature == renames.apply(&item.path, &item.signature))) {
            edits.push(item_edit(EditKind::ItemRemoved, Some(item), None));
        }
        for item in new.iter().filter(|item| !old.iter().any(|other| renames.apply(&other.path, &other.signature) == item.signature)) {
            edits.push(item_edit(EditKind::ItemAdded, None, Some(item)));
        }
    }

    // The inner attributes of a crate root or a module file (`#![…]`): an attribute edit of the
    // module. A crate root has no item, so the anchor is the crate name.
    let modules: BTreeSet<&String> = before.inner_attrs.keys().chain(after.inner_attrs.keys()).collect();
    for module in modules {
        let (old, new) = (before.inner_attrs.get(module), after.inner_attrs.get(module));
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
            edit.after = Some(Anchor::item(module.as_str()));
            edit.text_before = Some(texts(old));
            edit.text_after = Some(texts(new));
            edit.line = 1;
            edits.push(edit);
        }
    }
    edits.extend(file_edits(before, after));
    edits
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

fn diff_item(before: &Item, after: &Item, renames: &Renames, edits: &mut Vec<Edit>) {
    let attribute_edit = |kind: EditKind, view: View, old: String, new: String| Edit {
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
    };
    let apply = |text: &str| renames.apply(&before.path, text);

    if before.vis != after.vis {
        edits.push(attribute_edit(EditKind::Visibility, View::Signature, before.vis.clone(), after.vis.clone()));
    }
    if before.kind != ItemKind::Mod && apply(&before.signature) != after.signature {
        // A function signature: one edit for each changed part (design 22.3, A5).
        let parts = match (&before.sig, &after.sig) {
            (Some(old), Some(new)) => signature_parts(before, after, old, new, renames),
            _ => Vec::new(),
        };
        if parts.is_empty() {
            // The kind of the item, for the facts: a field type, a const type, a discriminant.
            let mut edit = attribute_edit(EditKind::Signature, View::Signature, before.signature.clone(), after.signature.clone());
            edit.attr_before = Some(kind_name(before.kind).to_string());
            edit.attr_after = Some(kind_name(after.kind).to_string());
            edits.push(edit);
        }
        edits.extend(parts);
    }
    // A doc line (`///`, `//!`, `#[doc]`) and a plain comment are two different changes: a doc
    // is text of the API, a comment is not. They get a fact each.
    let split = |lines: &[String]| -> (Vec<String>, Vec<String>) { lines.iter().cloned().partition(|line| is_doc_line(line)) };
    let (before_docs, before_comments) = split(&before.docs);
    let (after_docs, after_comments) = split(&after.docs);
    if before_docs != after_docs {
        edits.push(attribute_edit(EditKind::Docs, View::Syntax, before_docs.join("\n"), after_docs.join("\n")));
    }
    if before_comments != after_comments {
        edits.push(attribute_edit(EditKind::Comment, View::Syntax, before_comments.join("\n"), after_comments.join("\n")));
    }
    if before.attrs != after.attrs {
        edits.push(attribute_edit(EditKind::Attrs, View::Syntax, before.attrs.join("\n"), after.attrs.join("\n")));
    }
    // A renamed field or variant keeps its place in the member list.
    let renamed_member = |member: &String| {
        [format!("{}.{member}", before.path), format!("{}::{member}", before.path)]
            .iter()
            .find_map(|path| renames.items.get(path))
            .map(|new| new.rsplit(['.', ':']).next().unwrap_or(new).to_string())
    };
    let old_members: Vec<String> = before.members.iter().map(|member| renamed_member(member).unwrap_or_else(|| apply(member))).collect();
    if old_members != after.members {
        // The kind of the owner, for the hints (a struct, a variant, an enum, a trait, an impl).
        let mut edit = attribute_edit(EditKind::Members, View::Signature, before.members.join("\n"), after.members.join("\n"));
        edit.attr_before = Some(kind_name(before.kind).to_string());
        edit.attr_after = Some(kind_name(after.kind).to_string());
        edits.push(edit);
    }
    if let (Some(old), Some(new)) = (&before.body, &after.body) {
        let mut context = Context { renames, item_before: &before.path, item_after: &after.path, file: &after.file, source_before: before, source_after: after, edits: Vec::new() };
        context.block(old, new, &[], &[]);
        detect_moves(&mut context.edits);
        context.edits.sort_by_key(|edit| edit.line);
        edits.extend(context.edits);
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
        source_before: old_node.map(|n| n.text.clone()),
        source_after: new_node.map(|n| n.text.clone()),
        block_like: false,
        file: Some(after.file.clone()),
        line: new_node.or(old_node).map(|n| n.start.0).unwrap_or(after.line_start),
        note: None,
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
        edits.push(order);
    }
    let (a, b) = (old.child("ret"), new.child("ret"));
    if a.map(|n| apply(&n.text)) != b.map(|n| n.text.clone()) {
        edits.push(edit("ret", "ret", a, b));
    }
    edits
}

struct Context<'a> {
    renames: &'a Renames,
    item_before: &'a str,
    item_after: &'a str,
    file: &'a str,
    source_before: &'a Item,
    source_after: &'a Item,
    edits: Vec<Edit>,
}

/// The source text of a node, cut from the source of its item.
fn node_source(item: &Item, node: &Node) -> Option<String> {
    let lines: Vec<&str> = item.source.lines().collect();
    let (start, end) = (node.start, node.end);
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
    Some(dedent(&text, start.1))
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
    fn same(&self, before: &Node, after: &Node) -> bool {
        self.renames.apply(self.item_before, &before.text) == after.text
    }

    fn anchor(item: &str, path: &[String]) -> Anchor {
        let text = std::iter::once(item.to_string()).chain(path.iter().cloned()).collect::<Vec<_>>().join("/");
        text.parse().unwrap_or_else(|_| Anchor::item(item))
    }

    fn edit(&mut self, kind: EditKind, before: Option<(&Node, &[String])>, after: Option<(&Node, &[String])>) {
        let line = after.or(before).map(|(node, _)| node.start.0).unwrap_or(0);
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
        });
    }

    /// Compares two blocks. `before_path` and `after_path` are the role paths of the blocks.
    fn block(&mut self, before: &Node, after: &Node, before_path: &[String], after_path: &[String]) {
        let old: Vec<&Node> = before.children.iter().collect();
        let new: Vec<&Node> = after.children.iter().collect();
        let pairs = lcs(&old, &new, |a, b| self.same(a, b));

        let child_path = |path: &[String], node: &Node| {
            let mut path = path.to_vec();
            path.push(node.role.clone());
            path
        };

        let (mut i, mut j) = (0, 0);
        for (pi, pj) in pairs.into_iter().chain(std::iter::once((old.len(), new.len()))) {
            let gap_old = &old[i..pi];
            let gap_new = &new[j..pj];
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
                for (k, (old_node, new_node)) in gap_old.iter().zip(gap_new).enumerate() {
                    used_new[k] = true;
                    let (bp, ap) = (child_path(before_path, old_node), child_path(after_path, new_node));
                    self.node(old_node, new_node, &bp, &ap);
                }
                i = pi + 1;
                j = pj + 1;
                continue;
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
            i = pi + 1;
            j = pj + 1;
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
        if self.same(before, after) {
            return;
        }
        if self.operand_chain(before, after, before_path, after_path) {
            return;
        }
        let roles = |node: &Node| node.children.iter().map(|child| child.role.clone()).collect::<Vec<_>>();
        let leaf = before.children.is_empty() && after.children.is_empty();
        if before.kind != after.kind || roles(before) != roles(after) || leaf {
            self.edit(EditKind::Replaced, Some((before, before_path)), Some((after, after_path)));
            return;
        }
        let edits_before = self.edits.len();
        for (old_child, new_child) in before.children.iter().zip(&after.children) {
            let mut bp = before_path.to_vec();
            bp.push(old_child.role.clone());
            let mut ap = after_path.to_vec();
            ap.push(new_child.role.clone());
            if old_child.kind == "block" && new_child.kind == "block" {
                if !self.same(old_child, new_child) {
                    self.block(old_child, new_child, &bp, &ap);
                }
            } else {
                self.node(old_child, new_child, &bp, &ap);
            }
        }
        let attr_changed = before.attr.as_deref().map(|attr| self.renames.apply(self.item_before, attr)) != after.attr;
        if attr_changed && self.edits.len() > edits_before && !before.has_block() && !after.has_block() {
            // The node itself and a part of it changed (`a * b / 100` → `a / 100 * b`): one edit
            // for the full expression, not one edit for the node and one for each part (trial 4,
            // D7: one expression edit gave up to four facts that overlap).
            self.edits.truncate(edits_before);
            self.edit(EditKind::Replaced, Some((before, before_path)), Some((after, after_path)));
            return;
        }
        if attr_changed || self.edits.len() == edits_before {
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
fn same_kind(a: &Node, b: &Node) -> bool {
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
fn lcs<T>(a: &[T], b: &[T], equal: impl Fn(&T, &T) -> bool) -> Vec<(usize, usize)> {
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
