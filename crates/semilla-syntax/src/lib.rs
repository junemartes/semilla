//! The syntax view of semilla.
//!
//! - [`load`]: loads a Cargo workspace into the entity tree.
//! - [`tree`]: the node tree inside an item.
//! - [`diff`]: the syntax diff of two workspaces.
//! - [`doc`]: the code part of a doc text (doc tests).
//! - [`flow`]: flow views of functions.
//! - [`graph`]: the workspace graph.

pub mod diff;
pub mod doc;
pub mod flow;
pub mod graph;
pub mod load;
mod net;
pub mod tree;

pub use diff::{Edit, EditKind, Renames, diff};
pub use load::{CodeFile, CrateInfo, Item, ItemKind, Target, Workspace};
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
    // A signature part: `item/sig/param[a]`.
    let (mut node, path) = match &anchor.path[0] {
        Segment::Role(role) if role == "sig" => (item.sig.as_ref()?, &anchor.path[1..]),
        _ => (item.body.as_ref()?, &anchor.path[..]),
    };
    for segment in path {
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

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn summary(edits: &[Edit]) -> Vec<String> {
        edits.iter().map(|e| format!("{:?} {}", e.kind, e.anchor().map(|a| a.to_string()).or(e.file.clone()).unwrap_or_default())).collect()
    }

    #[cfg(unix)]
    #[test]
    fn each_changed_file_gives_an_edit() {
        let lib = "pub fn f() -> u32 {\n    1\n}\n";
        let files = |binary: &[u8], orphan: &[u8], lib: &str| {
            let dir = tempfile::tempdir().unwrap();
            write_files(dir.path(), &[("Cargo.toml", MANIFEST), ("src/lib.rs", lib), ("run.sh", "#!/bin/sh\necho hi\n"), ("build.rs", "fn main() {}\n")]);
            std::fs::write(dir.path().join("logo.bin"), binary).unwrap();
            std::fs::write(dir.path().join("src/orphan.rs"), orphan).unwrap();
            dir
        };
        let old_dir = files(&[0, 1, 2, 0xff, 0xfe], b"pub fn g() {}\n", lib);
        let new_dir = files(&[0, 1, 2, 0xff, 0xfe, 7], &[0xff, 0xfe, b'\n'], "pub fn f() -> u32 { 1 }\n");
        set_mode(&new_dir.path().join("run.sh"), 0o755);
        std::fs::write(new_dir.path().join("build.rs"), "fn main() { println!(\"x\"); }\n").unwrap();
        let (old, new) = (Workspace::load(old_dir.path()).unwrap(), Workspace::load(new_dir.path()).unwrap());
        let edits = diff(&old, &new, &Renames::default());
        // The formatting change of the parsed file `src/lib.rs` gives no edit.
        assert_eq!(summary(&edits), ["FileChanged build.rs", "FileChanged logo.bin", "FileChanged run.sh", "FileChanged src/orphan.rs"], "{edits:#?}");
        let note = |file: &str| edits.iter().find(|e| e.file.as_deref() == Some(file)).and_then(|e| e.note.clone()).unwrap_or_default();
        assert_eq!(note("logo.bin"), "binary file changed");
        assert_eq!(note("run.sh"), "mode 100644 → 100755");
        assert_eq!(note("src/orphan.rs"), "binary file changed", "a Rust file that the loader does not parse is a file");
    }

    #[test]
    fn cfg_twins_get_distinct_paths() {
        let src = |win: &str| {
            format!(
                "#[cfg(windows)]\npub fn sep() -> char {{\n    '{win}'\n}}\n\n#[cfg(not(windows))]\npub fn sep() -> char {{\n    '/'\n}}\n\npub fn other() {{}}\n\n#[cfg(unix)]\nmod imp {{\n    use std::fmt;\n    pub fn f() -> u32 {{ 1 }}\n}}\n#[cfg(windows)]\nmod imp {{\n    use std::fmt;\n    pub fn f() -> u32 {{ 2 }}\n}}\n\npub struct S;\nimpl S {{\n    pub fn a() {{}}\n}}\nimpl S {{\n    pub fn b() {{}}\n}}\n"
            )
        };
        let (_d1, old) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", &src("\\\\"))]);
        let paths: Vec<&str> = old.items.keys().map(String::as_str).collect();
        for expected in ["demo::sep#0", "demo::sep#1", "demo::other", "demo::imp#0", "demo::imp#1", "demo::imp#0::f", "demo::imp#1::f", "demo::imp#0::use#0", "demo::imp#1::use#0", "demo::impl S#0", "demo::impl S#1", "demo::S::a"] {
            assert!(paths.contains(&expected), "missing {expected}: {paths:?}");
        }
        assert!(!paths.contains(&"demo::sep") && !paths.contains(&"demo::imp::f"), "{paths:?}");
        assert_eq!(old.items["demo::sep#0"].line_start, 1);
        assert_eq!(old.items["demo::sep#1"].line_start, 6);
        assert_eq!(anchor_at(&old, "src/lib.rs", 3, None).unwrap().to_string(), "demo::sep#0/tail");

        // A change in the first twin is an edit of the first twin.
        let (_d2, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", &src(";"))]);
        assert_eq!(summary(&diff(&old, &new, &Renames::default())), ["Replaced demo::sep#0/tail"]);

        // A change in the second twin module is an edit of the second twin module.
        let changed = src("\\\\").replace("    use std::fmt;\n    pub fn f() -> u32 { 2 }", "    pub fn f() -> u32 { 2 }");
        let (_d3, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", &changed)]);
        assert_eq!(summary(&diff(&old, &new, &Renames::default())), ["ItemRemoved demo::imp#1::use#0"]);
    }

    #[test]
    fn a_changed_impl_header_is_an_edit_of_the_impl() {
        let before = "pub struct W<T> { pub inner: T }\nimpl<T: Clone> W<T> {\n    pub fn get(&self) -> T { self.inner.clone() }\n}\nimpl<T> std::fmt::Display for W<T> {\n    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, \"w\") }\n}\n";
        let after = "pub struct W<T> { pub inner: T }\nimpl<'a, T: Clone + std::fmt::Debug> W<T> {\n    pub fn get(&self) -> T { self.inner.clone() }\n}\nimpl<T: Copy> std::fmt::Display for W<T> {\n    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, \"w\") }\n}\n";
        let (_d1, old) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", before)]);
        let (_d2, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", after)]);
        assert!(old.items.contains_key("demo::W::get") && old.items.contains_key("demo::<W as std::fmt::Display>::fmt"), "{:?}", old.items.keys());
        assert_eq!(summary(&diff(&old, &new, &Renames::default())), ["Signature demo::impl <W as std::fmt::Display>", "Signature demo::impl W"]);
        // Two impls of one type with the same method name still have distinct paths.
        let (_d3, two) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", "pub struct W<T>(T);\nimpl W<u8> {\n    pub fn get(&self) {}\n}\nimpl W<u16> {\n    pub fn get(&self) {}\n}\n")]);
        for path in ["demo::W::get#0", "demo::W::get#1", "demo::impl W#0", "demo::impl W#1"] {
            assert!(two.items.contains_key(path), "missing {path}: {:?}", two.items.keys());
        }
    }

    const TYPES: &str = "pub struct Account {\n    pub id: u32,\n    pub balance: u32,\n}\npub struct Pair(pub u32, pub u32);\npub enum Event {\n    Open,\n    Deposit(u32),\n    Close { reason: String },\n}\npub trait Price {\n    fn price(&self) -> u32;\n    fn label(&self) -> String {\n        String::from(\"item\")\n    }\n}\n";

    #[test]
    fn members_are_items() {
        let (_d1, old) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", TYPES)]);
        let kind = |path: &str| old.items.get(path).map(|item| item.kind);
        assert_eq!(kind("demo::Account.balance"), Some(ItemKind::Field));
        assert_eq!(kind("demo::Pair.1"), Some(ItemKind::Field));
        assert_eq!(kind("demo::Event::Open"), Some(ItemKind::Variant));
        assert_eq!(kind("demo::Event::Deposit.0"), Some(ItemKind::Field));
        assert_eq!(kind("demo::Event::Close.reason"), Some(ItemKind::Field));
        assert_eq!(kind("demo::Price::label"), Some(ItemKind::Fn));
        assert!(old.items["demo::Price::price"].body.is_none() && old.items["demo::Price::label"].body.is_some());
        assert_eq!(anchor_at(&old, "src/lib.rs", 3, None).unwrap().to_string(), "demo::Account.balance");
        let edits = |after: &str| {
            let (_d2, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", after)]);
            summary(&diff(&old, &new, &Renames::default()))
        };
        // One aspect of one member gives one edit of that member, not a blob edit of its owner.
        assert_eq!(edits(&TYPES.replace("pub balance: u32", "pub balance: u64")), ["Signature demo::Account.balance"]);
        assert_eq!(edits(&TYPES.replace("pub balance: u32", "balance: u32")), ["Visibility demo::Account.balance"]);
        assert_eq!(edits(&TYPES.replace("    pub balance: u32,\n", "    pub balance: u32,\n    pub created: u64,\n")), ["Members demo::Account", "ItemAdded demo::Account.created"]);
        assert_eq!(edits(&TYPES.replace("Close { reason: String }", "Close { reason: String, code: u8 }")), ["Members demo::Event::Close", "ItemAdded demo::Event::Close.code"]);
        assert_eq!(edits(&TYPES.replace("Deposit(u32)", "Deposit { n: u32 }")), ["Signature demo::Event::Deposit", "Members demo::Event::Deposit", "ItemRemoved demo::Event::Deposit.0", "ItemAdded demo::Event::Deposit.n"]);
        assert_eq!(edits(&TYPES.replace("\"item\"", "\"thing\"")), ["Replaced demo::Price::label/tail/args[0]"]);
        assert_eq!(edits(&TYPES.replace("fn price(&self) -> u32;", "fn price(&self, unit: u8) -> u32;")), ["Signature demo::Price::price/sig/param[unit]"]);
        // A new order of the fields is an edit of the owner.
        assert_eq!(edits(&TYPES.replace("    pub id: u32,\n    pub balance: u32,\n", "    pub balance: u32,\n    pub id: u32,\n")), ["Members demo::Account"]);
    }

    #[test]
    fn inner_attributes_give_an_edit() {
        let (_d1, old) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", "pub mod m;\n"), ("src/m.rs", "pub fn f() {}\n")]);
        let (_d2, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", "#![allow(dead_code)]\npub mod m;\n"), ("src/m.rs", "#![cfg(any())]\npub fn f() {}\n")]);
        let edits = diff(&old, &new, &Renames::default());
        assert_eq!(summary(&edits), ["Attrs demo", "Attrs demo::m"], "{edits:#?}");
        assert_eq!(edits[1].text_after.as_deref(), Some("# ! [cfg (any ())]"));
        assert_eq!(edits[1].file.as_deref(), Some("src/m.rs"));
    }

    #[test]
    fn signature_parts_are_nodes() {
        let src = "pub struct S;\nimpl S {\n    pub fn m(&self, a: u32) -> u32 { a }\n}\npub trait T {\n    fn t(&self) -> u32;\n}\npub fn f<X: Clone>(a: u32, b: &str) -> u32 where X: Copy { a }\n";
        let (_d1, old) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", src)]);
        let anchor: Anchor = "demo::f/sig/param[b]".parse().unwrap();
        assert_eq!(resolve_node(&old, &anchor).unwrap().text, "b : & str");
        let edits = |after: &str| {
            let (_d2, new) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", after)]);
            summary(&diff(&old, &new, &Renames::default()))
        };
        assert_eq!(edits(&src.replace("b: &str)", "b: &str, c: bool)")), ["Signature demo::f/sig/param[c]"]);
        assert_eq!(edits(&src.replace("(a: u32, b: &str)", "(a: u32)")), ["Signature demo::f/sig/param[b]"]);
        assert_eq!(edits(&src.replace("b: &str)", "b: String)")), ["Signature demo::f/sig/param[b]"]);
        assert_eq!(edits(&src.replace("(a: u32, b: &str)", "(b: &str, a: u32)")), ["Signature demo::f/sig/params"]);
        assert_eq!(edits(&src.replace("-> u32 where", "-> u64 where")), ["Signature demo::f/sig/ret"]);
        assert_eq!(edits(&src.replace("<X: Clone>", "<X: Clone + Send>")), ["Signature demo::f/sig/generics"]);
        assert_eq!(edits(&src.replace("where X: Copy", "where X: Copy + Send")), ["Signature demo::f/sig/where"]);
        assert_eq!(edits(&src.replace("pub fn f", "pub async fn f")), ["Signature demo::f/sig/qualifiers"]);
        assert_eq!(edits(&src.replace("pub fn m(&self", "pub fn m(&mut self")), ["Signature demo::S::m/sig/param[self]"]);
        assert_eq!(edits(&src.replace("fn t(&self) -> u32;", "fn t(&self, x: u8) -> u32;")), ["Signature demo::T::t/sig/param[x]"]);
        // Two parts change: two edits.
        assert_eq!(edits(&src.replace("b: &str) -> u32 where", "b: String) -> u64 where")), ["Signature demo::f/sig/param[b]", "Signature demo::f/sig/ret"]);
    }

    use std::collections::BTreeSet;
}
