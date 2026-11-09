//! The syntax view of semilla.
//!
//! - [`load`]: loads a Cargo workspace into the entity tree.
//! - [`tree`]: the node tree inside an item.
//! - [`diff`]: the syntax diff of two workspaces.
//! - [`flow`]: flow views of functions.
//! - [`graph`]: the workspace graph.

pub mod diff;
pub mod flow;
pub mod graph;
pub mod load;
pub mod tree;

pub use diff::{Edit, EditKind, Renames, diff};
pub use load::{CrateInfo, Item, ItemKind, Workspace};
pub use tree::Node;

use semilla_core::{Anchor, Segment};

/// Splits an item path at `::`, but not inside `<…>`: `a::<T as fmt::Display>::fmt` gives
/// `a`, `<T as fmt::Display>`, `fmt`.
pub fn split_path(path: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b':' if depth == 0 && bytes.get(index + 1) == Some(&b':') => {
                parts.push(&path[start..index]);
                index += 2;
                start = index;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    parts.push(&path[start..]);
    parts
}

/// The role name of an anchor segment.
pub fn segment_role(segment: &Segment) -> String {
    segment.to_string()
}

/// What an anchor points to.
pub enum Resolved<'a> {
    Item(&'a Item),
    Node(&'a Item, &'a Node),
}

/// Finds the item or the node of an anchor.
pub fn resolve<'a>(workspace: &'a Workspace, anchor: &Anchor) -> Option<Resolved<'a>> {
    let item = workspace.items.get(&anchor.item)?;
    if anchor.path.is_empty() {
        return Some(Resolved::Item(item));
    }
    let mut node = item.body.as_ref()?;
    for segment in &anchor.path {
        node = node.child(&segment_role(segment))?;
    }
    Some(Resolved::Node(item, node))
}

/// Finds the node of an anchor.
pub fn resolve_node<'a>(workspace: &'a Workspace, anchor: &Anchor) -> Option<&'a Node> {
    match resolve(workspace, anchor)? {
        Resolved::Node(_, node) => Some(node),
        Resolved::Item(item) => item.body.as_ref(),
    }
}

/// Returns the anchor of the lowest node at a file position.
///
/// `column` is optional. Without a column, the anchor is the lowest statement on the line.
pub fn anchor_at(workspace: &Workspace, file: &str, line: usize, column: Option<usize>) -> Option<Anchor> {
    let at_line = |item: &&Item| item.file == file && item.line_start <= line && line <= item.line_end;
    // A function or a type first; a module or an impl only when no smaller item has the line.
    let item = workspace
        .items
        .values()
        .filter(|item| at_line(item) && item.kind != ItemKind::Mod && item.kind != ItemKind::Impl)
        .min_by_key(|item| item.line_end - item.line_start)
        .or_else(|| workspace.items.values().filter(at_line).min_by_key(|item| item.line_end - item.line_start))?;
    let mut anchor = Anchor::item(&item.path);
    let Some(body) = &item.body else {
        return Some(anchor);
    };
    if !(body.start.0 <= line && line <= body.end.0) {
        return Some(anchor);
    }
    let roles = match column {
        Some(column) => column_path(body, (line, column)),
        None => {
            let covers = |n: &Node| n.start.0 <= line && line <= n.end.0;
            match body.children.iter().find(|s| s.stmt && covers(s)) {
                Some(statement) => {
                    let mut roles = vec![statement.role.clone()];
                    roles.extend(line_path(statement, line));
                    roles
                }
                None => Vec::new(),
            }
        }
    };
    for role in roles {
        anchor = anchor.child(role_segment(&role));
    }
    Some(anchor)
}

fn role_segment(role: &str) -> Segment {
    match role {
        "tail" => Segment::Tail,
        role => match role.strip_prefix('S').and_then(|n| n.parse().ok()) {
            Some(index) => Segment::Stmt(index),
            None => Segment::Role(role.to_string()),
        },
    }
}

/// With a column: the roles down to the lowest node that contains the position.
fn column_path(node: &Node, pos: tree::Pos) -> Vec<String> {
    match node.children.iter().find(|child| child.contains_pos(pos)) {
        Some(child) => {
            let mut roles = vec![child.role.clone()];
            roles.extend(column_path(child, pos));
            roles
        }
        None => Vec::new(),
    }
}

/// Without a column: the roles down to the lowest statement that covers the line. The path goes
/// only into blocks whose statements cover the line, so a statement header stays the statement.
fn line_path(node: &Node, line: usize) -> Vec<String> {
    let covers = |n: &Node| n.start.0 <= line && line <= n.end.0;
    for child in &node.children {
        if child.kind == "block" {
            if let Some(statement) = child.children.iter().find(|s| s.stmt && covers(s)) {
                let mut roles = vec![child.role.clone(), statement.role.clone()];
                roles.extend(line_path(statement, line));
                return roles;
            }
        } else if covers(child) && child.has_block() {
            let deeper = line_path(child, line);
            if !deeper.is_empty() {
                let mut roles = vec![child.role.clone()];
                roles.extend(deeper);
                return roles;
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    pub(crate) fn workspace(files: &[(&str, &str)]) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        write_files(dir.path(), files);
        let ws = Workspace::load(dir.path()).unwrap();
        (dir, ws)
    }

    pub(crate) fn write_files(root: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
    }

    const MANIFEST: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";

    #[test]
    fn resolve_and_anchor_at() {
        let src = "pub fn total(v: &[u32], max: u32) -> u32 {\n    let mut s = 0;\n    for x in v {\n        if *x < max {\n            s += x;\n        }\n    }\n    s\n}\n";
        let (_dir, ws) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", src)]);
        let anchor: Anchor = "demo::total/S1/body/S0/cond".parse().unwrap();
        let node = resolve_node(&ws, &anchor).unwrap();
        assert_eq!(node.text, "* x < max");

        let at_line = anchor_at(&ws, "src/lib.rs", 5, None).unwrap();
        assert_eq!(at_line.to_string(), "demo::total/S1/body/S0/then/S0");
        let at_column = anchor_at(&ws, "src/lib.rs", 4, Some(19)).unwrap();
        assert_eq!(at_column.to_string(), "demo::total/S1/body/S0/cond/rhs");
        let header = anchor_at(&ws, "src/lib.rs", 4, None).unwrap();
        assert_eq!(header.to_string(), "demo::total/S1/body/S0", "a header line gives the statement");
        let tail = anchor_at(&ws, "src/lib.rs", 8, None).unwrap();
        assert_eq!(tail.to_string(), "demo::total/tail");
    }

    #[test]
    fn a_long_literal_change_is_a_change_not_a_new_statement() {
        let before = "pub fn f() -> String {\n    let a = 1;\n    let s = \"one two three four five six\";\n    s.to_string()\n}\n";
        let after = "pub fn f() -> String {\n    let a = 1;\n    let s = \"seven eight nine ten eleven twelve\";\n    s.to_string()\n}\n";
        let (_d1, old) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", before)]);
        let (_d2, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", after)]);
        let edits = diff(&old, &new, &Renames::default());
        assert_eq!(edits.len(), 1, "{edits:#?}");
        assert_eq!(edits[0].kind, EditKind::Replaced);
        assert_eq!(edits[0].anchor().unwrap().to_string(), "demo::f/S1/init");
    }

    #[test]
    fn anchor_on_a_module_line() {
        let src = "pub fn f() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n";
        let (_dir, ws) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", src)]);
        assert_eq!(anchor_at(&ws, "src/lib.rs", 4, None).unwrap().to_string(), "demo::tests");
        assert_eq!(anchor_at(&ws, "src/lib.rs", 6, None).unwrap().to_string(), "demo::tests::t");
    }

    #[test]
    fn diff_finds_the_lowest_node() {
        let before = "pub fn f(i: u32, n: u32) -> bool {\n    let a = i + 1;\n    a < n\n}\n";
        let after = "pub fn f(i: u32, n: u32) -> bool {\n    if i == 0 {\n        return false;\n    }\n    let a = i + 2;\n    a <= n\n}\n";
        let (_d1, old) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", before)]);
        let (_d2, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", after)]);
        let edits = diff(&old, &new, &Renames::default());
        let summary: Vec<String> = edits
            .iter()
            .map(|e| format!("{:?} {}", e.kind, e.anchor().map(|a| a.to_string()).unwrap_or_default()))
            .collect();
        assert_eq!(
            summary,
            ["Inserted demo::f/S0", "Replaced demo::f/S1/init/rhs", "Modified demo::f/tail"],
            "{edits:#?}"
        );
        assert_eq!(edits[1].source_before.as_deref(), Some("1"));
        assert_eq!(edits[0].source_after.as_deref(), Some("if i == 0 {\n    return false;\n}"), "{:?}", edits[0].source_after);
        let modified = edits.iter().find(|e| e.kind == EditKind::Modified).unwrap();
        assert_eq!(modified.source_after.as_deref(), Some("a <= n"));
        assert_eq!((modified.attr_before.as_deref(), modified.attr_after.as_deref()), (Some("<"), Some("<=")));
        assert!(edits[0].block_like);
        assert!(!edits[1].block_like);
    }

    #[test]
    fn renames_remove_edits() {
        let before = "fn helper(x: u32) -> u32 { x + 1 }\npub fn f(x: u32) -> u32 { let total = helper(x); total * 2 }\n";
        let after = "fn add_one(x: u32) -> u32 { x + 1 }\npub fn f(x: u32) -> u32 { let sum = add_one(x); sum * 2 }\n";
        let (_d1, old) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", before)]);
        let (_d2, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", after)]);
        assert!(!diff(&old, &new, &Renames::default()).is_empty());
        let mut renames = Renames::default();
        renames.items.insert("demo::helper".into(), "demo::add_one".into());
        renames.idents.push((None, "helper".into(), "add_one".into()));
        renames.idents.push((Some("demo::f".into()), "total".into(), "sum".into()));
        let edits = diff(&old, &new, &renames);
        assert!(edits.is_empty(), "{edits:#?}");
    }

    #[test]
    fn split_path_keeps_trait_impls_together() {
        assert_eq!(split_path("a::date::<T as fmt::Display>::fmt"), ["a", "date", "<T as fmt::Display>", "fmt"]);
        assert_eq!(split_path("a"), ["a"]);
    }

    #[test]
    fn renames_reach_inside_groups() {
        let mut renames = Renames::default();
        renames.idents.push((None, "len".into(), "count".into()));
        assert_eq!(renames.apply("x", "input [len ..] . get (len) + lens + len_2"), "input [count ..] . get (count) + lens + len_2");
    }

    #[test]
    fn moves_and_deletes() {
        let before = "pub fn f() { a(); b(); c(); }\nfn a() {}\nfn b() {}\nfn c() {}\n";
        let after = "pub fn f() { b(); a(); }\nfn a() {}\nfn b() {}\nfn c() {}\n";
        let (_d1, old) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", before)]);
        let (_d2, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", after)]);
        let kinds: Vec<_> = diff(&old, &new, &Renames::default()).iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EditKind::Moved), "{kinds:?}");
        assert!(kinds.contains(&EditKind::Deleted), "{kinds:?}");
    }

    #[test]
    fn item_level_edits() {
        let before = "/// Old doc.\npub struct S { pub a: u32 }\nfn g() -> u32 { 1 }\nuse std::fmt;\n";
        let after = "/// New doc.\npub struct S { pub a: u32, pub b: u32 }\npub fn g() -> u64 { 1 }\nfn h() {}\nuse std::fmt;\n";
        let (_d1, old) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", before)]);
        let (_d2, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", after), ("README.md", "hi")]);
        let edits = diff(&old, &new, &Renames::default());
        let kinds: BTreeSet<_> = edits.iter().map(|e| format!("{:?}", e.kind)).collect();
        for expected in ["Docs", "Members", "Visibility", "Signature", "ItemAdded", "FileAdded"] {
            assert!(kinds.contains(expected), "missing {expected}: {kinds:?}");
        }
        assert!(!edits.iter().any(|e| e.attr_after.as_deref() == Some("use")), "an unchanged use gives no edit");
    }

    use std::collections::BTreeSet;
}
