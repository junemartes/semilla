//! Tests of design 79: the AST pass of the intent language (batch L1).

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{AstEntry, AstState, IntentFile, IntentResult, Step};
use semilla_syntax::Workspace;

use super::astmap::{ClaimCalls, CompiledClaims};
use super::*;

const MANIFEST: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";

/// Loads a crate `demo` with the files of a list.
fn load_files(files: &[(&str, &str)]) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), MANIFEST).unwrap();
    for (path, text) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let ws = Workspace::load(dir.path()).unwrap();
    (dir, ws)
}

/// The words of a text with their byte ranges.
fn words(text: &str) -> Vec<(usize, usize, String)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'_' || bytes[at].is_ascii_alphabetic() {
            let start = at;
            while at < bytes.len() && (bytes[at] == b'_' || bytes[at].is_ascii_alphanumeric()) {
                at += 1;
            }
            out.push((start, at, text[start..at].to_string()));
        } else {
            at += 1;
        }
    }
    out
}

/// A name record of `src/lib.rs` in the form of the driver, for a test: a definition for each
/// item of the tree (a field `T.f` is `T::f`), and a definition for the last name of each path of
/// a `use` line (its path); each other word with the name of one definition resolves to it.
fn record_of(ws: &Workspace) -> semilla_core::namerec::NameIndex {
    let text = ws.rust_files.get("src/lib.rs").cloned().unwrap_or_default();
    let found = words(&text);
    let mut defs: Vec<serde_json::Value> = Vec::new();
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    let mut idents: BTreeSet<usize> = BTreeSet::new();
    let mut use_ranges: Vec<(usize, usize)> = Vec::new();
    for (path, item) in &ws.items {
        let Some((_, start, end)) = crate::records::item_place(ws, item) else { continue };
        if item.kind == semilla_syntax::ItemKind::Use {
            use_ranges.push((start, end));
            // `use a::b::C;` and `use a::b::{C, D};`: each last name is a definition of another
            // crate.
            let line = item.signature.replace(' ', "");
            let target = line.trim_start_matches("use").trim_end_matches(';').to_string();
            let paths: Vec<String> = match target.split_once('{') {
                Some((prefix, rest)) => rest.trim_end_matches('}').split(',').map(|name| format!("{prefix}{name}")).collect(),
                None => vec![target.clone()],
            };
            for full in paths {
                let Some((_, last)) = full.rsplit_once("::") else { continue };
                if let Some((name_start, _, _)) = found.iter().filter(|(s, _, word)| *s >= start && *s < end && word == last).last() {
                    let def = *by_name.entry(last.to_string()).or_insert_with(|| {
                        defs.push(serde_json::json!({"p": full, "n": last, "k": "struct"}));
                        defs.len() - 1
                    });
                    let _ = def;
                    idents.insert(*name_start);
                }
            }
            continue;
        }
        let name = path.rsplit(['.', ':']).next().unwrap_or(path).to_string();
        let Some((name_start, name_end, _)) = found.iter().find(|(s, _, word)| *s >= start && *s < end && *word == name) else { continue };
        idents.insert(*name_start);
        by_name.entry(name.clone()).or_insert(defs.len());
        let kind = match item.kind {
            semilla_syntax::ItemKind::Fn => "function",
            semilla_syntax::ItemKind::Field => "field",
            semilla_syntax::ItemKind::Mod => "module",
            _ => "struct",
        };
        let vis = if item.vis.trim() == "pub" { "pub" } else { "" };
        defs.push(serde_json::json!({"p": path.replace('.', "::"), "n": name, "k": kind, "s": [0, start, end], "i": [0, name_start, name_end], "v": vis}));
    }
    let mut paths: Vec<[i64; 4]> = Vec::new();
    for (start, end, word) in &found {
        if idents.contains(start) {
            // The name of a `use` line resolves to its definition.
            if use_ranges.iter().any(|(s, e)| s <= start && start < e)
                && let Some(def) = by_name.get(word)
            {
                paths.push([0, *start as i64, *end as i64, *def as i64]);
            }
            continue;
        }
        if let Some(def) = by_name.get(word) {
            paths.push([0, *start as i64, *end as i64, *def as i64]);
        }
    }
    let record: semilla_core::namerec::Record = serde_json::from_value(serde_json::json!({"f": ["src/lib.rs"], "d": defs, "p": paths})).unwrap();
    semilla_core::namerec::NameIndex::new([("demo.lib.aaaa", &record)])
}

/// An oracle with the compiled claims of a test.
struct Claims(Option<Result<CompiledClaims, String>>);

impl BodyOracle for Claims {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(Vec::new())
    }

    fn intent_anchors(&self, _: &Step) -> Option<Result<CompiledClaims, String>> {
        self.0.clone()
    }

    /// [R87.1-1] The compiler of the test accepts the first form of each template.
    fn check_templates(&self, _: &Step, asks: &[crate::template::Ask]) -> Option<Vec<crate::template::Verdict>> {
        Some(asks.iter().map(|_| Ok(0)).collect())
    }
}

/// The compiled claims of a test: the links (each path resolves to itself, unless a pair of the
/// list says otherwise) and the calls and field reads of each claim function of `claims.rs`.
fn compiled(links: &[(&str, Option<&str>)], functions: &[(&str, &[&str], &[&str])], fields: &[(&str, usize, &str, &str)]) -> CompiledClaims {
    let mut out = CompiledClaims::default();
    for (path, resolved) in links {
        out.links.insert(path.to_string(), resolved.map(str::to_string));
    }
    for (name, old, new) in functions {
        let calls = ClaimCalls { calls: [old.iter().map(|text| text.to_string()).collect(), new.iter().map(|text| text.to_string()).collect()], fields: Default::default() };
        out.functions.insert(("claims.rs".to_string(), name.to_string()), calls);
    }
    for (name, side, ty, field) in fields {
        let entry = out.functions.entry(("claims.rs".to_string(), name.to_string())).or_default();
        entry.fields[*side].insert((ty.to_string(), String::new(), field.to_string()));
    }
    out
}

struct Run {
    outcome: StepOutcome,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Run {
    fn map(&self) -> &semilla_core::AstMap {
        self.outcome.report.ast.as_ref().expect("the AST map")
    }

    /// The entry of the node whose anchor text is `at` and whose change is `change`.
    fn entry(&self, change: &str, at: &str) -> &semilla_core::AstEntry {
        self.map().entries.iter().find(|entry| entry.change == change && entry.at == at).unwrap_or_else(|| panic!("no entry {change} {at}: {:#?}", self.map().entries))
    }
}

/// Runs a step of the default mode with the files of each tree, the intent file `claims.rs`,
/// the compiled claims, and the name records.
fn run79(before: &[(&str, &str)], after: &[(&str, &str)], intent: Option<&str>, data: Option<Result<CompiledClaims, String>>, names: bool) -> Run {
    let (d1, old) = load_files(before);
    let (d2, new) = load_files(after);
    if names {
        old.names.set(Some(std::sync::Arc::new(record_of(&old))));
        new.names.set(Some(std::sync::Arc::new(record_of(&new))));
    }
    let mut step = Step { index: 0, intent: "test".into(), claims: Vec::new(), tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default(), syntax_only: true, build_check: None, agent: Default::default(), tags: Vec::new(), configurations: None };
    if let Some(text) = intent {
        step.agent.intent = vec![IntentFile { path: "claims.rs".into(), text: text.into() }];
    }
    let outcome = verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &Claims(data), &NoOracle, true);
    Run { outcome, _dirs: (d1, d2) }
}

/// [R92.3-1] True for a node that the AST pass maps by no rule (design 79): it stays "not
/// mapped" with its request, or (design 92) a syntax-only node with no partner is an addition or
/// a removal with who reaches it.
fn open(entry: &AstEntry) -> bool {
    entry.state == AstState::NotMapped || entry.rule == "R92.3-1"
}

fn lib(text: &str) -> Vec<(&'static str, String)> {
    vec![("src/lib.rs", text.to_string())]
}

fn files<'a>(list: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    list.iter().map(|(path, text)| (*path, text.as_str())).collect()
}

const OLD: &str = "pub fn f(x: u32) -> u32 {\n    let a = x + 1;\n    a\n}\n\npub fn g(x: u32) -> u32 {\n    x * 2\n}\n";
const NEW: &str = "/// The next value.\npub fn f(x: u32) -> u32 {\n    let a = x + 1;\n    let b = a + 0;\n    b\n}\n\npub fn g(x: u32) -> u32 {\n    let z = 3;\n    x * 2\n}\n";

const CLAIM_F: &str = "use semilla_intent::prelude::*;\n#[same(demo::f)]\nfn f_same(x: u32) -> bool { before::demo::f(x) == after::demo::f(x) }\n";

fn f_links() -> CompiledClaims {
    compiled(&[("before::demo::f", Some("before::demo::f")), ("after::demo::f", Some("after::demo::f"))], &[("f_same", &["demo::f"], &["demo::f"])], &[])
}

// rule+ R79.1-1
#[test]
fn each_changed_node_has_exactly_one_entry_and_the_counts_add_up() {
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW)), None, None, false);
    let map = run.map();
    let nodes: Vec<&semilla_syntax::Edit> = run.outcome.edits.iter().filter(|edit| astmap::is_node(edit)).collect();
    assert_eq!(map.nodes, nodes.len());
    assert_eq!(map.entries.len(), nodes.len(), "{:#?}", map.entries);
    assert_eq!(map.count(AstState::Claim) + map.count(AstState::Semilla) + map.count(AstState::NotMapped), map.nodes);
    // The entries are in the order of the syntax diff: entry I is node I.
    for (entry, edit) in map.entries.iter().zip(&nodes) {
        assert_eq!(entry.at, edit.anchor().unwrap().to_string());
    }
    assert!(map.line().starts_with("AST changes: 0 mapped by claims, "), "{}", map.line());
}

// rule- R79.1-1
#[test]
fn a_file_edit_is_not_a_node_of_the_ast_diff() {
    let old = [("src/lib.rs", OLD), ("README.md", "old\n")];
    let new = [("src/lib.rs", OLD), ("README.md", "new\n")];
    let run = run79(&old, &new, None, None, false);
    assert!(run.outcome.edits.iter().any(|edit| edit.kind == semilla_syntax::EditKind::FileChanged), "{:#?}", run.outcome.edits);
    assert_eq!(run.map().nodes, 0);
    assert!(run.map().entries.is_empty());
}

// rule+ R79.2-1
// rule+ R79.2-3
#[test]
fn a_compiled_claim_anchors_its_item_and_maps_each_node_inside_it() {
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW)), Some(CLAIM_F), Some(Ok(f_links())), false);
    // The added `let` and the new tail of `f`, and its docs: mapped by the claim.
    let added = run.entry("added", "demo::f/S1");
    assert_eq!((added.state, added.by.as_str(), added.rule.as_str()), (AstState::Claim, "i1", "R79.2-3"));
    assert_eq!(run.entry("changed", "demo::f").state, AstState::Claim);
    assert!(run.map().anchors.iter().any(|line| line == "i1: same `demo::f` → `demo::f`"), "{:?}", run.map().anchors);
}

// rule+ R79.2-3
#[test]
fn an_anchor_of_a_type_maps_the_nodes_inside_it() {
    // A type has no call: its scope path is the anchor, and it maps each field of the type.
    let intent = "use semilla_intent::prelude::*;\n#[change(demo::Cart)]\nfn cart_changes(b: &before::demo::Cart, a: &after::demo::Cart) -> bool { let _ = (b, a); true }\n";
    let data = compiled(&[("before::demo::Cart", Some("before::demo::Cart")), ("after::demo::Cart", Some("after::demo::Cart"))], &[("cart_changes", &[], &[])], &[]);
    let run = run79(&files(&lib(OLD_T)), &files(&lib(NEW_T)), Some(intent), Some(Ok(data)), false);
    let extra = run.entry("added", "demo::Cart.extra");
    assert_eq!((extra.state, extra.by.as_str(), extra.rule.as_str()), (AstState::Claim, "i1", "R79.2-3"), "{extra:?}");
}

// rule- R79.2-3
#[test]
fn an_anchor_does_not_map_a_node_of_another_item() {
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW)), Some(CLAIM_F), Some(Ok(f_links())), false);
    let other = run.entry("added", "demo::g/S0");
    assert!(open(other), "{other:?}");
    assert!(other.template.starts_with("#[same(demo::g)] fn g(x: u32) -> bool"), "{}", other.template);
}

// rule+ R79.2-1
#[test]
fn a_call_of_a_generic_claim_anchors_the_definition_with_its_parameters() {
    assert_eq!(astmap::no_args("bitflags::iter::IterNamed::<B>::new"), "bitflags::iter::IterNamed::new");
    assert_eq!(astmap::no_args("<bitflags::iter::IterNamed<B> as core::iter::traits::iterator::Iterator>::next"), "<bitflags::iter::IterNamed as core::iter::traits::iterator::Iterator>::next");
    // The claim calls the instance `demo::Bag::<u8>::new`; rustc resolves the scope to the
    // definition `demo::Bag::<T>::new`.
    let old = "pub struct Bag<T>(Vec<T>);\nimpl<T> Bag<T> {\n    pub fn new() -> Self {\n        Bag(Vec::new())\n    }\n}\n";
    let new = "pub struct Bag<T>(Vec<T>);\nimpl<T> Bag<T> {\n    pub fn new() -> Self {\n        let _marker = 1u8;\n        Bag(Vec::new())\n    }\n}\n";
    let (_dir, tree) = load_files(&[("src/lib.rs", new)]);
    let method = tree.items.keys().find(|path| path.ends_with("::new")).unwrap().clone();
    let intent = "use semilla_intent::prelude::*;\n#[same(demo::Bag::new)]\nfn bag_new() -> bool { let _ = (before::demo::Bag::<u8>::new(), after::demo::Bag::<u8>::new()); true }\n";
    let def_old = format!("before::{}", method.replace("::new", "::<T>::new"));
    let def_new = format!("after::{}", method.replace("::new", "::<T>::new"));
    let call = method.replace("::new", "::<u8>::new");
    let data = compiled(&[("before::demo::Bag::new", Some(def_old.as_str())), ("after::demo::Bag::new", Some(def_new.as_str()))], &[("bag_new", &[call.as_str()], &[call.as_str()])], &[]);
    let run = run79(&[("src/lib.rs", old)], &[("src/lib.rs", new)], Some(intent), Some(Ok(data)), true);
    let inserted = run.entry("added", &format!("{method}/S0"));
    assert_eq!((inserted.state, inserted.by.as_str()), (AstState::Claim, "i1"), "{:?}", run.map().anchors);
}

// rule- R79.2-1
#[test]
fn the_anchor_comes_from_the_definition_that_rustc_resolved_never_from_the_text() {
    // No compiled claims: no anchor, and the map says why.
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW)), Some(CLAIM_F), None, false);
    assert!(open(run.entry("added", "demo::f/S1")));
    assert!(run.map().gap.as_deref().is_some_and(|gap| gap.contains("the claims that rustc compiles")), "{:?}", run.map().gap);
    // The scope text `demo::f` resolves (by rustc) to `demo::g`: the anchor is `g`.
    let data = compiled(&[("before::demo::f", Some("before::demo::g")), ("after::demo::f", Some("after::demo::g"))], &[("f_same", &["demo::g"], &["demo::g"])], &[]);
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW)), Some(CLAIM_F), Some(Ok(data)), false);
    assert!(open(run.entry("added", "demo::f/S1")));
    assert_eq!(run.entry("added", "demo::g/S0").state, AstState::Claim);
    // A property that does not call the new version gives no anchor of it.
    let data = compiled(&[("before::demo::f", Some("before::demo::f")), ("after::demo::f", Some("after::demo::f"))], &[("f_same", &["demo::f"], &[])], &[]);
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW)), Some(CLAIM_F), Some(Ok(data)), false);
    assert!(run.map().anchors.iter().any(|line| line.contains("does not call `after::demo::f`")), "{:?}", run.map().anchors);
    assert!(run.map().anchors.iter().any(|line| line == "i1: same `demo::f` → (none)"), "{:?}", run.map().anchors);
}

const OLD_ADD: &str = "pub fn f(x: u32) -> u32 {\n    x + 1\n}\n";
const NEW_ADD: &str = "pub fn f(x: u32) -> u32 {\n    x + 1\n}\n\npub fn g(x: u32) -> u32 {\n    f(x) * 2\n}\n";

// rule+ R79.2-2
#[test]
fn an_anchor_whose_node_and_kind_fit_is_no_error() {
    let intent = "use semilla_intent::prelude::*;\n#[new(demo::g)]\nfn g_new(x: u32) -> bool { after::demo::g(x) == after::demo::f(x) * 2 }\n";
    let data = compiled(&[("after::demo::g", Some("after::demo::g"))], &[("g_new", &[], &["demo::g", "demo::f"])], &[]);
    let run = run79(&files(&lib(OLD_ADD)), &files(&lib(NEW_ADD)), Some(intent), Some(Ok(data)), false);
    assert!(run.map().errors.is_empty(), "{:?}", run.map().errors);
    assert_eq!(run.entry("added", "demo::g").state, AstState::Claim);
}

// rule- R79.2-2
#[test]
fn an_anchor_that_does_not_fit_is_an_error_of_its_claim_with_the_place_and_the_expected_kind() {
    // `#[new]` on an item of the two trees.
    let intent = "use semilla_intent::prelude::*;\n#[new(demo::f)]\nfn f_new(x: u32) -> bool { after::demo::f(x) == x + 1 }\n";
    let data = compiled(&[("after::demo::f", Some("after::demo::f"))], &[("f_new", &[], &["demo::f"])], &[]);
    let run = run79(&files(&lib(OLD_ADD)), &files(&lib(NEW_ADD)), Some(intent), Some(Ok(data)), false);
    assert_eq!(run.map().errors.len(), 1, "{:?}", run.map().errors);
    assert!(run.map().errors[0].starts_with("i1 claims.rs:2:7: [S0120] `#[new(demo::f)]` expects an item that the step adds"), "{}", run.map().errors[0]);
    let claim = run.outcome.report.intent_claims.iter().find(|claim| claim.id == "i1").unwrap();
    assert_eq!(claim.result, IntentResult::Error, "{claim:?}");
    assert!(!run.outcome.report.accepted());
    // A relation of a function: the expected kind is a type.
    let intent = "use semilla_intent::prelude::*;\n#[relation(before::demo::f, after::demo::f)]\nfn rel(b: u32, a: u32) -> bool { a == b }\n";
    let data = compiled(&[("before::demo::f", Some("before::demo::f")), ("after::demo::f", Some("after::demo::f"))], &[], &[]);
    let run = run79(&files(&lib(OLD_ADD)), &files(&lib(NEW_ADD)), Some(intent), Some(Ok(data)), false);
    assert!(run.map().errors.iter().any(|error| error.contains("a relation expects a type")), "{:?}", run.map().errors);
    // A definition with no node in a tree with a name record (an item that a macro makes).
    let intent = "use semilla_intent::prelude::*;\n#[same(demo::made)]\nfn made() {}\n";
    let data = compiled(&[("before::demo::made", Some("before::demo::made")), ("after::demo::made", Some("after::demo::made"))], &[], &[]);
    let run = run79(&files(&lib(OLD_ADD)), &files(&lib(NEW_ADD)), Some(intent), Some(Ok(data)), true);
    assert!(run.map().errors.iter().any(|error| error.contains("has no node of it; expected an item of the source")), "{:?}", run.map().errors);
}

const OLD_T: &str = "pub struct Cart {\n    pub items: Vec<u32>,\n}\n\npub fn total(c: &Cart) -> u32 {\n    c.items.iter().sum()\n}\n";
const NEW_T: &str = "pub struct Cart {\n    pub entries: Vec<u64>,\n    pub extra: bool,\n}\n\npub fn total(c: &Cart) -> u32 {\n    c.entries.iter().sum::<u64>() as u32\n}\n";

// rule+ R79.2-4
#[test]
fn a_relation_maps_the_two_types_and_each_field_that_it_reads() {
    let intent = "use semilla_intent::prelude::*;\n#[relation(before::demo::Cart, after::demo::Cart)]\nfn cart(b: &before::demo::Cart, a: &after::demo::Cart) -> bool { a.entries.len() == b.items.len() }\n";
    let data = compiled(&[("before::demo::Cart", Some("before::demo::Cart")), ("after::demo::Cart", Some("after::demo::Cart"))], &[], &[("cart", 0, "demo::Cart", "items"), ("cart", 1, "demo::Cart", "entries")]);
    let run = run79(&files(&lib(OLD_T)), &files(&lib(NEW_T)), Some(intent), Some(Ok(data)), false);
    assert!(run.map().errors.is_empty(), "{:?}", run.map().errors);
    for entry in run.map().entries.iter().filter(|entry| entry.at == "demo::Cart.items" || entry.at == "demo::Cart.entries" || entry.at == "demo::Cart") {
        assert_eq!((entry.state, entry.rule.as_str()), (AstState::Claim, "R79.2-4"), "{entry:?}");
    }
    assert!(run.map().anchors.iter().any(|line| line.contains("reads `demo::Cart.items` | `demo::Cart.entries`")), "{:?}", run.map().anchors);
}

// rule- R79.2-4
#[test]
fn a_relation_does_not_map_a_field_that_it_does_not_read() {
    let intent = "use semilla_intent::prelude::*;\n#[relation(before::demo::Cart, after::demo::Cart)]\nfn cart(b: &before::demo::Cart, a: &after::demo::Cart) -> bool { a.entries.len() == b.items.len() }\n";
    let data = compiled(&[("before::demo::Cart", Some("before::demo::Cart")), ("after::demo::Cart", Some("after::demo::Cart"))], &[], &[("cart", 0, "demo::Cart", "items"), ("cart", 1, "demo::Cart", "entries")]);
    let run = run79(&files(&lib(OLD_T)), &files(&lib(NEW_T)), Some(intent), Some(Ok(data)), false);
    let extra = run.entry("added", "demo::Cart.extra");
    assert_ne!(extra.state, AstState::Claim, "{extra:?}");
    // A read of a type that is not a type of the relation maps nothing.
    let data = compiled(&[("before::demo::Cart", Some("before::demo::Cart")), ("after::demo::Cart", Some("after::demo::Cart"))], &[], &[("cart", 1, "demo::Other", "extra")]);
    let run = run79(&files(&lib(OLD_T)), &files(&lib(NEW_T)), Some(intent), Some(Ok(data)), false);
    assert_ne!(run.entry("added", "demo::Cart.extra").state, AstState::Claim);
}

const OLD_M: &str = "pub fn f(x: u32) -> u32 {\n    let a = x + 1;\n    let b = x * 3;\n    a + b + 7\n}\n";
const NEW_M: &str = "pub fn f(x: u32) -> u32 {\n    let b = x * 3;\n    let a = x + 1;\n    a + b + 8\n}\n";

// rule+ R79.3-1
#[test]
fn the_matcher_maps_a_node_with_a_partner() {
    let run = run79(&files(&lib(OLD_M)), &files(&lib(NEW_M)), None, None, false);
    let moved: Vec<_> = run.map().entries.iter().filter(|entry| entry.change == "moved").collect();
    assert!(!moved.is_empty(), "{:#?}", run.map().entries);
    assert!(moved.iter().all(|entry| entry.state == AstState::Semilla && entry.rule == "R79.3-1"), "{moved:?}");
    let literal = run.map().entries.iter().find(|entry| entry.node == "literal").expect("a changed literal");
    assert_eq!((literal.state, literal.by.as_str()), (AstState::Semilla, "a pair at one place"));
}

// rule- R79.3-1
#[test]
fn a_node_with_no_partner_is_not_mapped_by_the_matcher() {
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW)), None, None, false);
    let added = run.entry("added", "demo::f/S1");
    assert_eq!((added.state, added.rule.as_str()), (AstState::Semilla, "R92.3-1"));
}

const NEW_DOC: &str = "/// The next value.\npub fn f(x: u32) -> u32 {\n    // one more\n    let a = x + 1;\n    a\n}\n\npub fn g(x: u32) -> u32 {\n    x * 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        assert_eq!(super::f(1), 2);\n    }\n}\n";

// rule+ R79.3-2
#[test]
fn a_doc_a_comment_and_test_code_are_mapped_by_semilla() {
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW_DOC)), None, None, false);
    assert!(run.map().entries.iter().all(|entry| entry.state == AstState::Semilla), "{:#?}", run.map().entries);
    let test = run.entry("added", "demo::tests::t");
    assert_eq!((test.rule.as_str(), test.by.as_str()), ("R79.3-2", "test, bench, or example code: no claim names it"));
    assert!(run.map().entries.iter().any(|entry| entry.node == "docs" && entry.rule == "R79.3-2"), "{:#?}", run.map().entries);
}

// rule- R79.3-2
#[test]
fn a_statement_of_code_that_is_not_a_test_is_not_mapped_by_the_rule_of_no_code() {
    let new = NEW_DOC.replace("    a\n}\n\npub fn g", "    let c = 1;\n    a\n}\n\npub fn g");
    let run = run79(&files(&lib(OLD)), &files(&lib(&new)), None, None, false);
    assert!(run.map().entries.iter().any(|entry| entry.at.starts_with("demo::f/") && open(entry)), "{:#?}", run.map().entries);
}

const OLD_R: &str = "pub fn f(x: u32) -> u32 {\n    helper(x)\n}\n\nfn helper(x: u32) -> u32 {\n    x + 1\n}\n\nfn dead_a(x: u32) -> u32 {\n    dead_b(x)\n}\n\nfn dead_b(x: u32) -> u32 {\n    dead_a(x)\n}\n\npub fn api(x: u32) -> u32 {\n    x\n}\n";
const NEW_R: &str = "pub fn f(x: u32) -> u32 {\n    x + 1\n}\n";

// rule+ R79.3-3
#[test]
fn a_removed_item_with_no_reader_is_mapped_by_semilla() {
    for names in [false, true] {
        let run = run79(&files(&lib(OLD_R)), &files(&lib(NEW_R)), None, None, names);
        // Two removed items that read only each other have no reader.
        for at in ["demo::dead_a", "demo::dead_b"] {
            let entry = run.entry("removed", at);
            assert_eq!((entry.state, entry.rule.as_str()), (AstState::Semilla, "R79.3-3"), "names: {names}: {entry:?}");
        }
    }
}

// rule- R79.3-3
#[test]
fn a_removed_item_with_a_reader_or_a_public_item_is_not_mapped() {
    for names in [false, true] {
        let run = run79(&files(&lib(OLD_R)), &files(&lib(NEW_R)), None, None, names);
        // `helper` has a reader in the old tree (`f`); `api` is public.
        for at in ["demo::helper", "demo::api"] {
            let entry = run.entry("removed", at);
            assert!(open(entry), "names: {names}: {entry:?}");
            assert!(entry.template.starts_with("#[removed("), "{}", entry.template);
        }
    }
}

const OLD_A: &str = "pub fn f(x: u32) -> u32 {\n    x + 1\n}\n";
const NEW_A: &str = "pub fn f(x: u32) -> u32 {\n    outer(x)\n}\n\nfn outer(x: u32) -> u32 {\n    inner(x)\n}\n\nfn inner(x: u32) -> u32 {\n    x + 1\n}\n\npub fn extra(x: u32) -> u32 {\n    x * 2\n}\n";

// rule+ R79.3-4
#[test]
fn an_added_item_that_no_old_code_reaches_is_mapped_by_semilla() {
    for names in [false, true] {
        let run = run79(&files(&lib(OLD_A)), &files(&lib(NEW_A)), None, None, names);
        let entry = run.entry("added", "demo::extra");
        assert_eq!((entry.state, entry.rule.as_str()), (AstState::Semilla, "R79.3-4"), "names: {names}: {entry:?}");
    }
}

// rule- R79.3-4
#[test]
fn an_added_item_that_a_changed_body_reaches_is_not_mapped_also_through_another_added_item() {
    for names in [false, true] {
        let run = run79(&files(&lib(OLD_A)), &files(&lib(NEW_A)), None, None, names);
        for at in ["demo::outer", "demo::inner"] {
            let entry = run.entry("added", at);
            assert!(open(entry), "names: {names}: {entry:?}");
            assert!(entry.template.starts_with(&format!("#[new({at})] fn ")), "{}", entry.template);
        }
    }
}

const OLD_U: &str = "pub fn f(x: u32) -> u32 {\n    x + 1\n}\n\npub fn g() -> u32 {\n    7\n}\n";
const NEW_U: &str = "use std::collections::BTreeMap;\nuse std::collections::HashSet;\nuse std::collections::BTreeSet;\n\npub fn f(x: u32) -> u32 {\n    let s: HashSet<u32> = HashSet::new();\n    x + 1 + s.len() as u32\n}\n\npub fn g() -> u32 {\n    let s: BTreeSet<u32> = BTreeSet::new();\n    7 + s.len() as u32\n}\n";

// rule+ R79.3-5
#[test]
fn an_unused_use_line_and_a_use_line_of_changed_readers_are_mapped_by_semilla() {
    let run = run79(&files(&lib(OLD_U)), &files(&lib(NEW_U)), None, None, true);
    let states: Vec<(AstState, String)> = run.map().entries.iter().filter(|entry| entry.node == "`use` line").map(|entry| (entry.state, entry.by.clone())).collect();
    assert_eq!(states.len(), 3, "{:#?}", run.map().entries);
    // `BTreeMap` has no reader; `HashSet` and `BTreeSet` have readers that change (`f`, `g`).
    assert_eq!(states.iter().filter(|(state, by)| *state == AstState::Semilla && by == "an unused `use` line (the name record)").count(), 1, "{states:?}");
    assert_eq!(states.iter().filter(|(state, by)| *state == AstState::Semilla && by.starts_with("a part of the changes of its readers")).count(), 2, "{states:?}");
}

// rule+ R79.3-5
#[test]
fn a_use_line_that_only_moves_names_between_the_lines_of_its_module_is_mapped_by_semilla() {
    let old = "use std::collections::{BTreeMap, HashMap};\n\npub fn g() -> usize {\n    HashMap::<u8, u8>::new().len() + BTreeMap::<u8, u8>::new().len()\n}\n";
    let new = "use std::collections::BTreeMap;\nuse std::collections::HashMap;\n\npub fn g() -> usize {\n    HashMap::<u8, u8>::new().len() + BTreeMap::<u8, u8>::new().len()\n}\n";
    let run = run79(&[("src/lib.rs", old)], &[("src/lib.rs", new)], None, None, true);
    let lines: Vec<_> = run.map().entries.iter().filter(|entry| entry.node == "`use` line").collect();
    assert_eq!(lines.len(), 3, "{:#?}", run.map().entries);
    for entry in lines {
        assert_eq!((entry.state, entry.by.as_str()), (AstState::Semilla, "a new form of the `use` lines: each name of the line stays imported in its module"), "{entry:?}");
    }
}

// rule- R79.3-5
#[test]
fn a_use_line_with_an_unchanged_reader_or_with_no_name_record_is_not_mapped() {
    // The new line gives `Set` to `g`, which does not change (the old tree had the name from a
    // glob): the line changes the meaning of a name of an unchanged item.
    let old = "mod inner {\n    pub struct Set;\n    impl Set {\n        pub fn size(&self) -> u32 { 1 }\n    }\n}\nuse inner::*;\n\npub fn g() -> u32 {\n    Set.size()\n}\n";
    let new = "mod inner {\n    pub struct Set;\n    impl Set {\n        pub fn size(&self) -> u32 { 1 }\n    }\n}\nuse inner::*;\nuse inner::Set;\n\npub fn g() -> u32 {\n    Set.size()\n}\n";
    let run = run79(&[("src/lib.rs", old)], &[("src/lib.rs", new)], None, None, true);
    let line = run.map().entries.iter().find(|entry| entry.node == "`use` line").expect("the added line");
    assert!(open(line), "{line:?}");
    assert!(line.template.contains("`demo::g`"), "{}", line.template);
    // No name record: Semilla cannot find the readers.
    let run = run79(&files(&lib(OLD_U)), &files(&lib(NEW_U)), None, None, false);
    for entry in run.map().entries.iter().filter(|entry| entry.node == "`use` line") {
        assert!(open(entry), "{entry:?}");
        assert!(entry.template.contains("no name record"), "{}", entry.template);
    }
}

// rule+ R79.4-1
#[test]
fn the_pass_is_polynomial_on_a_large_step() {
    let mut old = String::new();
    let mut new = String::new();
    for index in 0..300 {
        old.push_str(&format!("pub fn old_{index}(x: u32) -> u32 {{\n    let a = x + {index};\n    a\n}}\n"));
        new.push_str(&format!("pub fn new_{index}(x: u32) -> u32 {{\n    let a = x * {index};\n    let b = a + 1;\n    b\n}}\n"));
    }
    let started = std::time::Instant::now();
    let run = run79(&[("src/lib.rs", &old)], &[("src/lib.rs", &new)], None, None, true);
    let took = started.elapsed();
    assert_eq!(run.map().entries.len(), run.map().nodes);
    assert!(run.map().nodes >= 600, "{}", run.map().nodes);
    // The full step (the load, the diff, the facts, and the pass) of 600 items, under load.
    assert!(took.as_secs() < 120, "{took:?}");
}

// rule- R79.4-1
#[test]
fn the_pass_does_not_search_for_a_pair_that_the_matcher_does_not_give() {
    // A deleted and an inserted statement at one place with no common token: a search for an
    // optimal map could pair them; the pass keeps two entries that are not mapped.
    let old = "pub fn f(x: u32) -> u32 {\n    x + 1\n}\n";
    let new = "pub fn f(x: u32) -> u32 {\n    helper_value()\n}\n\nfn helper_value() -> u32 {\n    9\n}\n";
    let run = run79(&[("src/lib.rs", old)], &[("src/lib.rs", new)], None, None, false);
    let tail: Vec<_> = run.map().entries.iter().filter(|entry| entry.at == "demo::f/tail").collect();
    assert_eq!(tail.len(), 2, "{:#?}", run.map().entries);
    assert!(tail.iter().all(|entry| open(entry)), "{tail:?}");
}

// rule+ R79.5-1
#[test]
fn a_node_that_is_not_mapped_gives_its_texts_and_a_template() {
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW)), None, None, false);
    let entry = run.entry("added", "demo::f/S1");
    assert_eq!(entry.after, "let b = a + 0;");
    assert!(entry.before.is_empty());
    assert_eq!(entry.template, "#[same(demo::f)] fn f(x: u32) -> bool { after::demo::f(x) == before::demo::f(x) }");
    // [R92.3-1] The node has no MIR element and no partner: an addition, with its template.
    let lines = astmap::lines(run.map(), None, 0).join("\n");
    assert!(lines.contains("[R92.3-1]: added `let` `demo::f/S1` (src/lib.rs:4): add, syntax only in f3"), "{lines}");
}

// rule- R79.5-1
#[test]
fn a_node_with_no_claim_form_gives_the_cause_and_no_template() {
    let old = "pub struct W;\nimpl std::fmt::Display for W {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        write!(f, \"w\")\n    }\n}\n";
    let new = "pub struct W;\nimpl std::fmt::Display for W {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        let text = \"w\";\n        write!(f, \"{text}\")\n    }\n}\n";
    let run = run79(&[("src/lib.rs", old)], &[("src/lib.rs", new)], None, None, false);
    let entry = run.map().entries.iter().find(|entry| open(entry)).expect("a node that is not mapped");
    assert!(entry.template.starts_with("no template: "), "{}", entry.template);
}

// rule+ R90.5-3
#[test]
fn the_coverage_line_of_the_map_replaces_the_ast_line_and_the_node_lines_follow_it() {
    let mut run = run79(&files(&lib(OLD)), &files(&lib(NEW)), Some(CLAIM_F), Some(Ok(f_links())), false);
    let lines = intent::semantic_lines(&run.outcome.report, Some(10));
    assert!(!lines.iter().any(|line| line.contains("AST changes")), "{lines:#?}");
    let gap = run.outcome.report.gap_lines(Some(10));
    assert!(gap[0].trim_start().starts_with("changed nodes: "), "{gap:#?}");
    // A node that is not mapped is no reject: the step passes with J > 0.
    assert!(run.map().entries.iter().any(open));
    run.outcome.report.intent_claims.retain(|claim| claim.result != IntentResult::Claimed);
    assert!(run.outcome.report.accepted(), "{:?}", run.outcome.report.errors);
}

// rule- R90.5-3
#[test]
fn a_report_with_no_map_has_no_coverage_line() {
    let mut run = run79(&files(&lib(OLD)), &files(&lib(NEW)), Some(CLAIM_F), Some(Ok(f_links())), false);
    run.outcome.report.ast = None;
    let lines = intent::semantic_lines(&run.outcome.report, Some(10));
    assert!(!lines.iter().any(|line| line.contains("AST changes") || line.contains("changed nodes:")), "{lines:#?}");
    assert!(!run.outcome.report.gap_lines(Some(10)).iter().any(|line| line.contains("changed nodes:")));
}

// rule+ R90.5-3
#[test]
fn a_step_with_no_intent_file_has_the_coverage_line_and_the_full_text_lists_each_node() {
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW)), None, None, false);
    let short = intent::semantic_lines(&run.outcome.report, Some(10));
    assert!(!short.iter().any(|line| line.contains("AST changes")), "{short:#?}");
    assert!(run.outcome.report.gap_lines(Some(10))[0].trim_start().starts_with("changed nodes: "));
    // The full text lists each node with its number.
    let full = intent::semantic_lines(&run.outcome.report, None);
    assert_eq!(full.iter().filter(|line| line.trim_start().starts_with('n')).count(), run.map().nodes, "{full:#?}");
}

// rule- R90.5-3
#[test]
fn a_step_with_no_intent_file_shows_no_claim_section_and_no_node_line_in_the_semantic_lines() {
    let run = run79(&files(&lib(OLD)), &files(&lib(NEW)), None, None, false);
    let short = intent::semantic_lines(&run.outcome.report, Some(10)).join("\n");
    assert!(!short.contains("semantic changes") && !short.contains("not mapped [") && !short.contains("template:"), "{short}");
    assert!(run.outcome.report.intent_claims.is_empty() && run.outcome.report.coverage.is_empty());
}

const OLD_I: &str = "pub struct W;\n";
const NEW_I: &str = "pub struct W;\n\nimpl Iterator for W {\n    type Item = u8;\n\n    fn next(&mut self) -> Option<u8> {\n        None\n    }\n}\n";

// rule+ R79.3-6
#[test]
fn an_added_impl_block_and_its_associated_type_take_the_state_of_its_members() {
    let (_dir, new) = load_files(&[("src/lib.rs", NEW_I)]);
    let next = new.items.keys().find(|path| path.ends_with("::next")).expect("the method").clone();
    let def = format!("after::{next}");
    let intent = "use semilla_intent::prelude::*;\n#[new(<demo::W as core::iter::Iterator>::next)]\nfn next_new() -> bool { true }\n";
    let data = compiled(&[("after::demo::W::next", Some(def.as_str()))], &[("next_new", &[], &[next.as_str()])], &[]);
    let run = run79(&[("src/lib.rs", OLD_I)], &[("src/lib.rs", NEW_I)], Some(intent), Some(Ok(data)), true);
    assert!(run.map().errors.is_empty(), "{:?}", run.map().errors);
    for node in ["fn", "impl", "type"] {
        let entry = run.map().entries.iter().find(|entry| entry.node == node).unwrap_or_else(|| panic!("no {node}: {:#?}", run.map().entries));
        assert_eq!(entry.state, AstState::Claim, "{entry:?}");
        assert_eq!(entry.by, "i1", "{entry:?}");
    }
    assert!(run.map().entries.iter().any(|entry| entry.node == "impl" && entry.rule == "R79.3-6"), "{:#?}", run.map().entries);
}

// rule+ R79.3-6
#[test]
fn an_added_module_file_takes_the_state_of_the_items_below_its_path() {
    let old = [("src/lib.rs", "pub fn f() -> u32 {\n    1\n}\n")];
    let new = [("src/lib.rs", "mod extra;\n\npub fn f() -> u32 {\n    1\n}\n"), ("src/extra.rs", "pub fn unused() -> u32 {\n    2\n}\n")];
    let run = run79(&old, &new, None, None, false);
    let module = run.entry("added", "demo::extra");
    assert_eq!((module.state, module.rule.as_str()), (AstState::Semilla, "R79.3-6"), "{:#?}", run.map().entries);
}

// rule- R79.3-6
#[test]
fn an_added_impl_block_with_a_member_that_is_not_mapped_is_not_mapped() {
    let run = run79(&[("src/lib.rs", OLD_I)], &[("src/lib.rs", NEW_I)], None, None, true);
    for node in ["fn", "impl", "type"] {
        let entry = run.map().entries.iter().find(|entry| entry.node == node).unwrap_or_else(|| panic!("no {node}: {:#?}", run.map().entries));
        assert!(open(entry), "{entry:?}");
    }
    // The method of the trait impl gives its qualified path, not a template.
    let method = run.map().entries.iter().find(|entry| entry.node == "fn").unwrap();
    assert!(method.template.starts_with("no template: an item of a trait impl has no plain path"), "{}", method.template);
}
