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

/// The kind of an edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditKind {
    ItemAdded,
    ItemRemoved,
    Visibility,
    Signature,
    Docs,
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
}

impl Renames {
    /// Applies the identifier renames to a token text, for an item on the "before" side.
    pub fn apply(&self, item: &str, text: &str) -> String {
        let renames: Vec<_> = self
            .idents
            .iter()
            .filter(|(scope, _, _)| scope.as_deref().is_none_or(|scope| scope == item))
            .collect();
        if renames.is_empty() {
            return text.to_string();
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

    for (file, text) in &before.other_files {
        match after.other_files.get(file) {
            Some(other) if other == text => {}
            Some(_) => edits.push(Edit::file_edit(EditKind::FileChanged, file)),
            None => edits.push(Edit::file_edit(EditKind::FileRemoved, file)),
        }
    }
    for file in after.other_files.keys() {
        if !before.other_files.contains_key(file) {
            edits.push(Edit::file_edit(EditKind::FileAdded, file));
        }
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
    };
    let apply = |text: &str| renames.apply(&before.path, text);

    if before.vis != after.vis {
        edits.push(attribute_edit(EditKind::Visibility, View::Signature, before.vis.clone(), after.vis.clone()));
    }
    if before.kind != ItemKind::Mod && apply(&before.signature) != after.signature {
        edits.push(attribute_edit(EditKind::Signature, View::Signature, before.signature.clone(), after.signature.clone()));
    }
    if before.docs != after.docs {
        edits.push(attribute_edit(EditKind::Docs, View::Syntax, before.docs.join("\n"), after.docs.join("\n")));
    }
    if before.attrs != after.attrs {
        edits.push(attribute_edit(EditKind::Attrs, View::Syntax, before.attrs.join("\n"), after.attrs.join("\n")));
    }
    let old_members: Vec<String> = before.members.iter().map(|member| apply(member)).collect();
    if old_members != after.members {
        edits.push(attribute_edit(EditKind::Members, View::Signature, before.members.join("\n"), after.members.join("\n")));
    }
    if let (Some(old), Some(new)) = (&before.body, &after.body) {
        let mut context = Context { renames, item_before: &before.path, item_after: &after.path, file: &after.file, source_before: before, source_after: after, edits: Vec::new() };
        context.block(old, new, &[], &[]);
        detect_moves(&mut context.edits);
        context.edits.sort_by_key(|edit| edit.line);
        edits.extend(context.edits);
    }
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

    /// Compares two paired nodes.
    fn node(&mut self, before: &Node, after: &Node, before_path: &[String], after_path: &[String]) {
        if self.same(before, after) {
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
        if attr_changed || self.edits.len() == edits_before {
            // The node itself changed: an operator, a method name, or text outside the children.
            self.edit(EditKind::Modified, Some((before, before_path)), Some((after, after_path)));
        }
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
