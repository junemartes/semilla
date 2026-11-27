//! The intent crate (design 78): rustc compiles the claims against the two versions, the
//! driver opens their private items, and the engine checks each claim as MIR.
//!
//! The two versions build in one folder, as the builds of a step: so each crate has one crate
//! identity in the two trees (part 0 of batch 13a2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use semilla_normal::intent::{Compiled, Prepared, Source};
use semilla_smt::mir::Ir;
use semilla_smt::mir::claim::ClaimVerdict;
use semilla_smt::mir::property::{Property, Version, check_property};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn ensure_driver(scratch: &Path) -> PathBuf {
    if let Ok(path) = semilla_normal::cargo::find_driver() {
        return path;
    }
    let target = scratch.join("driver-target");
    let status = Command::new("cargo").args(["build", "-p", "semilla-mir", "--target-dir"]).arg(&target).current_dir(semilla_normal::cargo::semilla_workspace()).status().expect("cargo runs");
    assert!(status.success(), "cannot build the driver");
    target.join("debug/semilla-mir-driver")
}

fn form(record: &semilla_normal::BodyRecord) -> Option<Arc<Ir>> {
    serde_json::from_str(record.q.as_deref()?).ok().map(Arc::new)
}

/// The files of a workspace with the crates `shop` and `user` (`user` calls `shop`).
fn tree(root: &Path, shop: &str, user: &str) {
    let _ = std::fs::remove_dir_all(root.join("crates"));
    write(&root.join("Cargo.toml"), "[workspace]\nresolver = \"3\"\nmembers = [\"crates/shop\", \"crates/user\", \"crates/util\"]\n");
    // A member that the step does not change and that no changed member uses.
    write(&root.join("crates/util/Cargo.toml"), "[package]\nname = \"util\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    write(&root.join("crates/util/src/lib.rs"), "pub struct Tool { pub n: u32 }\npub fn make(n: u32) -> Tool { Tool { n } }\n");
    write(&root.join("crates/shop/Cargo.toml"), "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    write(&root.join("crates/shop/src/lib.rs"), shop);
    write(&root.join("crates/user/Cargo.toml"), "[package]\nname = \"user\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nshop = { path = \"../shop\" }\n");
    write(&root.join("crates/user/src/lib.rs"), user);
}

/// The hash of each file of a folder.
fn hashes(folder: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                found.insert(path.strip_prefix(folder).unwrap().display().to_string(), bytes);
            }
        }
    }
    found
}

/// Two versions built in one folder, the records of each, and the prepared intent crate.
struct Pair {
    scratch: PathBuf,
    old: Vec<semilla_normal::BodyRecord>,
    new: Vec<semilla_normal::BodyRecord>,
    prepared: Prepared,
    /// The files of the target folder after the build of the new version.
    target: BTreeMap<String, Vec<u8>>,
    /// The libraries of the two trees, the members of the second build, and its libraries.
    libraries: Vec<semilla_normal::intent::Libraries>,
    names: Vec<String>,
    renamed: semilla_normal::intent::Libraries,
}

impl Pair {
    fn build(name: &str, old: (&str, &str), new: (&str, &str)) -> Pair {
        Pair::build_in(name, old, new, &semilla_normal::Configuration::DEV)
    }

    fn build_in(name: &str, old: (&str, &str), new: (&str, &str), config: &semilla_normal::Configuration) -> Pair {
        let scratch = std::env::temp_dir().join(format!("semilla-intent-mir-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        // SAFETY: the tests of this file set the variable to one value before each build.
        unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", ensure_driver(&scratch)) };
        let root = scratch.join("build");
        let target = scratch.join("target");
        let mut records = Vec::new();
        let mut libraries = Vec::new();
        for (side, (shop, user)) in [("old", old), ("new", new)] {
            tree(&root, shop, user);
            let out = scratch.join("records");
            let built = semilla_normal::run_cargo_check_delta_config(&root, "n0", &out, &target, config).unwrap_or_else(|error| panic!("{error:#}"));
            let cache = scratch.join(format!("cache-{side}"));
            std::fs::create_dir_all(&cache).unwrap();
            for file in &built.records {
                std::fs::copy(out.join(file), cache.join(file)).unwrap();
            }
            semilla_normal::intent::save_libraries(&cache, &built).unwrap();
            records.push(semilla_normal::read_records(&cache).unwrap().into_iter().filter(|record| record.level == "n0").collect::<Vec<_>>());
            libraries.push(semilla_normal::intent::read_libraries(&cache).expect("the libraries of the build"));
        }
        let target_files = hashes(&target);
        // [R78.1-2] The second build of the old tree: its files in the build folder again.
        let names = semilla_normal::intent::renamed_members(&libraries[0], &libraries[1]).unwrap();
        tree(&root, old.0, old.1);
        let (renamed, built) = semilla_normal::intent::second_build(&scratch, "pair", &root, &names).unwrap_or_else(|error| panic!("{error:#}"));
        assert!(built);
        let prepared = semilla_normal::intent::prepare(&scratch, "pair", &libraries[0], &libraries[1], Some(&renamed)).unwrap_or_else(|error| panic!("{error:#}"));
        let new = records.pop().unwrap();
        let old = records.pop().unwrap();
        Pair { scratch, old, new, prepared, target: target_files, libraries, names, renamed }
    }

    fn compile(&self, intent: &str) -> Compiled {
        semilla_normal::intent::compile(&self.prepared, &[Source { path: ".semilla/intent/claims.rs".into(), text: intent.into() }], &self.scratch).unwrap()
    }

    /// Checks each claim of an intent text (the claim functions in order), with the item of
    /// the scope of its attribute.
    fn check(&self, intent: &str) -> Vec<(String, ClaimVerdict)> {
        self.check_in(intent, None).into_iter().map(|(name, verdict, ..)| (name, verdict)).collect()
    }

    /// `check` in a budget of solver steps; each claim also gives its solver steps and its
    /// dependence set.
    fn check_in(&self, intent: &str, budget: Option<semilla_smt::budget::Budget>) -> Vec<(String, ClaimVerdict, u64, Vec<String>)> {
        let compiled = self.compile(intent);
        assert!(compiled.ok, "{:#?}", compiled.diagnostics);
        let (heads, findings) = semilla_intent::heads::check("claims.rs", intent);
        assert!(findings.iter().all(|finding| !finding.error), "{findings:?}");
        let solver = semilla_smt::Solver::find().expect("a Z3 solver");
        let texts = |records: &[semilla_normal::BodyRecord]| -> BTreeMap<String, String> { records.iter().map(|record| (record.name.clone(), record.text.clone())).collect() };
        let (ta, tb) = (texts(&self.old), texts(&self.new));
        let changed: BTreeSet<String> = ta.keys().chain(tb.keys()).filter(|name| ta.get(*name) != tb.get(*name)).cloned().collect();
        let is_changed = |name: &str| changed.contains(name);
        let find = |records: &[semilla_normal::BodyRecord], name: &str| records.iter().find(|record| record.name == name && record.q.is_some()).and_then(form);
        let wa = |name: &str| find(&self.old, name);
        let wb = |name: &str| find(&self.new, name);
        let own = |name: &str| find(&compiled.records, name);
        // The forms of the small callees of the standard library, as the oracle takes them: of
        // the claims, and of the bodies of each tree that the claims and their closures reach.
        let std_of = |records: &[&semilla_normal::BodyRecord]| {
            let mut found: BTreeMap<String, Arc<Ir>> = BTreeMap::new();
            for record in records {
                let Some(text) = record.qx.as_deref() else { continue };
                for (key, form) in serde_json::from_str::<BTreeMap<String, Ir>>(text).unwrap_or_default() {
                    found.entry(key).or_insert_with(|| Arc::new(form));
                }
            }
            found
        };
        let own_std = std_of(&compiled.records.iter().collect::<Vec<_>>());
        let reached = |records: &[semilla_normal::BodyRecord], prefix: &str| {
            let mut names: Vec<String> = Vec::new();
            for record in &compiled.records {
                let Some(form) = form(record) else { continue };
                for block in &form.b {
                    if let semilla_smt::mir::IrTermKind::Call(callee, ..) = &block.t.k
                        && let Some(rest) = callee.w.as_deref().and_then(|name| name.strip_prefix(prefix))
                    {
                        names.push(rest.to_string());
                    }
                }
            }
            let mut list: Vec<&semilla_normal::BodyRecord> = Vec::new();
            while let Some(name) = names.pop() {
                if let Some(record) = records.iter().find(|record| record.name == name)
                    && !list.iter().any(|known| std::ptr::eq(*known, record))
                {
                    list.push(record);
                    names.extend(record.callees.iter().cloned());
                }
            }
            std_of(&list)
        };
        let (std_a, std_b) = (reached(&self.old, "before::"), reached(&self.new, "after::"));

        let old = Version { workspace: &wa, std: &std_a, changed: &is_changed };
        let new = Version { workspace: &wb, std: &std_b, changed: &is_changed };

        // [R80.1-1] The relations of the intent text.
        let relations = relations_of(&compiled, &heads);
        heads
            .iter()
            .filter(|head| head.kind != "relation")
            .map(|head| {
                let name = format!("{}::claims::{}", semilla_normal::intent::CLAIMS_CRATE, head.function);
                let ir = find(&compiled.records, &name).unwrap_or_else(|| panic!("no record {name}"));
                let item = head.scope.first().map(|scope| scope.to_string());
                let property = Property { ir, own: &own, std: &own_std, item: (item.clone(), item) };
                let run = || semilla_smt::mir::with_relations(&relations, || check_property(&solver, &property, &old, &new, budget));
                let checked = if head.except.iter().any(|name| name == "panic_message") { semilla_smt::mir::claim::with_panic_messages_ignored(run) } else { run() };
                (head.function.clone(), checked.verdict, checked.steps, checked.depends)
            })
            .collect()
    }
}

/// [R80.1-1] The relations of a compiled intent text: the form of each `#[relation]` function.
fn relations_of(compiled: &Compiled, heads: &[semilla_intent::heads::Head]) -> Vec<semilla_smt::mir::Relation> {
    heads
        .iter()
        .filter(|head| head.kind == "relation")
        .filter_map(|head| {
            let name = format!("{}::claims::{}", semilla_normal::intent::CLAIMS_CRATE, head.function);
            let form = compiled.records.iter().find(|record| record.name == name && record.q.is_some()).and_then(form)?;
            Some(semilla_smt::mir::Relation { name, form })
        })
        .collect()
}

impl Pair {
    /// [R80.1-4] The check of an empty `#[same]` claim on each item, with the relations of an
    /// intent text: the result and the dependence set of each item.
    fn check_items(&self, intent: &str, items: &[&str]) -> Vec<(String, ClaimVerdict, Vec<String>)> {
        let compiled = self.compile(intent);
        assert!(compiled.ok, "{:#?}", compiled.diagnostics);
        let (heads, _) = semilla_intent::heads::check("claims.rs", intent);
        let relations = relations_of(&compiled, &heads);
        let solver = semilla_smt::Solver::find().expect("a Z3 solver");
        let texts = |records: &[semilla_normal::BodyRecord]| -> BTreeMap<String, String> { records.iter().map(|record| (record.name.clone(), record.text.clone())).collect() };
        let (ta, tb) = (texts(&self.old), texts(&self.new));
        let changed: BTreeSet<String> = ta.keys().chain(tb.keys()).filter(|name| ta.get(*name) != tb.get(*name)).cloned().collect();
        let is_changed = |name: &str| changed.contains(name);
        let find = |records: &[semilla_normal::BodyRecord], name: &str| records.iter().find(|record| record.name == name && record.q.is_some()).and_then(form);
        let wa = |name: &str| find(&self.old, name);
        let wb = |name: &str| find(&self.new, name);
        let own = |name: &str| find(&compiled.records, name);
        let std_of = |records: &[semilla_normal::BodyRecord]| {
            let mut found: BTreeMap<String, Arc<Ir>> = BTreeMap::new();
            for record in records {
                let Some(text) = record.qx.as_deref() else { continue };
                for (key, form) in serde_json::from_str::<BTreeMap<String, Ir>>(text).unwrap_or_default() {
                    found.entry(key).or_insert_with(|| Arc::new(form));
                }
            }
            found
        };
        let (std_a, std_b, own_std) = (std_of(&self.old), std_of(&self.new), std_of(&compiled.records));
        let old = Version { workspace: &wa, std: &std_a, changed: &is_changed };
        let new = Version { workspace: &wb, std: &std_b, changed: &is_changed };
        items
            .iter()
            .map(|item| {
                let checked = semilla_smt::mir::with_relations(&relations, || semilla_smt::mir::property::check_related(&solver, (item, item), &own, &own_std, &old, &new, None));
                (item.to_string(), checked.verdict, checked.depends)
            })
            .collect()
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

const OLD_SHOP: &str = r#"
mod inner {
    pub(crate) struct Secret { count: u32, label: u8 }
    impl Secret {
        fn new(count: u32) -> Secret { Secret { count, label: 7 } }
        fn double(&self) -> u32 { self.count.wrapping_mul(2) }
    }
    pub(crate) fn uses(x: u32) -> u32 { Secret::new(x).double() + Secret::new(0).label as u32 + width(&Mode::Wide { x }) }
    // A private enum: the fields of its variants have the visibility of the enum.
    enum Mode { Wide { x: u32 }, Narrow(u32) }
    fn width(m: &Mode) -> u32 { match m { Mode::Wide { x } => *x, Mode::Narrow(y) => *y } }
}
struct Private { a: u32, b: bool }
struct Pair(u32, u32);
impl Private { fn total(&self) -> u32 { if self.b { self.a / 2 } else { self.a } } }
fn private_add(a: u32, b: u32) -> u32 { a + b }
mod globbed {
    pub fn shown() -> u32 { 1 }
    pub(super) fn sibling() -> u32 { 3 }
    #[allow(dead_code)]
    fn hidden() -> u32 { 2 }
}
use globbed::*;
pub mod api { pub use super::globbed::*; }
pub fn half(x: i32) -> i32 { x / 2 }
pub fn parse(x: u32) -> bool { x > 3 }
pub fn tier(k: u8) -> u32 { k as u32 + 1 }
pub fn add(a: u32, b: u32) -> u32 { a + b }
#[derive(Debug)]
pub struct Pt { pub x: u32, pub y: u32 }
pub fn pt(x: u32) -> Pt { Pt { x, y: 1 } }
pub fn ratio(x: f64) -> f64 { x }
pub fn first(v: &Vec<u32>) -> u32 { v[0] }
pub fn guard(x: u32) -> u32 { assert!(x < 10, "too big"); x }
pub fn small(x: u32) -> u32 { x }
pub fn abs(x: i32) -> i32 { if x < 0 { -x } else { x } }
pub struct Bag { items: Vec<u32> }
impl Bag {
    pub fn new() -> Bag { Bag { items: Vec::new() } }
    pub fn size(&self) -> usize { self.items.len() }
}
pub fn sum<const N: usize>(xs: [u32; N]) -> u32 { xs.iter().fold(0, |a, b| a.wrapping_add(*b)) }
mod words {
    pub fn len(s: &[u8]) -> usize { s.len() }
    pub struct Word(pub u32);
    impl Word { pub fn get(&self) -> u32 { self.0 } }
}
pub use words::{len, Word};
pub fn entry(x: u32) -> u32 { private_add(Private { a: x, b: true }.total(), 1) + shown() + sibling() + inner::uses(x) + Pair(x, 0).1 }
"#;

const OLD_USER: &str = "pub fn use_it(x: u32) -> u32 { shop::entry(x) }\nfn private_in_user(x: u32) -> u32 { x.wrapping_mul(3) }\npub fn twice(x: u32) -> u32 { private_in_user(x) }\n";

fn new_shop() -> String {
    OLD_SHOP
        .replace("self.count.wrapping_mul(2)", "self.count.wrapping_shl(1)")
        .replace("fn private_add(a: u32, b: u32) -> u32 { a + b }", "fn private_add(a: u32, b: u32) -> u32 { b + a }")
        .replace("pub fn half(x: i32) -> i32 { x / 2 }", "pub fn half(x: i32) -> i32 { x >> 1 }")
        .replace("pub fn parse(x: u32) -> bool { x > 3 }", "pub fn parse(x: u32, strict: bool) -> bool { if strict { x > 4 } else { x > 3 } }")
        .replace("pub fn tier(k: u8) -> u32 { k as u32 + 1 }", "pub fn tier(k: u8) -> u32 { k as u32 + 2 }")
        .replace("pub fn add(a: u32, b: u32) -> u32 { a + b }", "pub fn add(a: u32, b: u32) -> u32 { a.wrapping_add(b) }")
        .replace("pub fn pt(x: u32) -> Pt { Pt { x, y: 1 } }", "pub fn pt(x: u32) -> Pt { Pt { y: 1, x } }")
        .replace("pub fn first(v: &Vec<u32>) -> u32 { v[0] }", "pub fn first(v: &Vec<u32>) -> Option<u32> { if v.is_empty() { None } else { Some(v[0]) } }")
        .replace("assert!(x < 10, \"too big\")", "assert!(x < 10, \"x is too big\")")
        .replace("pub fn len(s: &[u8]) -> usize { s.len() }", "pub fn len(s: &[u8]) -> usize { s.iter().count() }")
        .replace("pub fn abs(x: i32) -> i32 { if x < 0 { -x } else { x } }", "pub fn abs(x: i32) -> i32 { x.abs() }")
        .replace("pub struct Bag { items: Vec<u32> }", "pub struct Bag { items: Vec<u32>, count: usize }")
        .replace("pub fn new() -> Bag { Bag { items: Vec::new() } }", "pub fn new() -> Bag { Bag { items: Vec::new(), count: 0 } }")
        .replace("pub fn size(&self) -> usize { self.items.len() }", "pub fn size(&self) -> usize { self.count }")
}

fn pair(name: &str) -> Pair {
    Pair::build(name, (OLD_SHOP, OLD_USER), (&new_shop(), OLD_USER))
}

const PRIVATE: &str = r#"use semilla_intent::prelude::*;

// A field read through a reference parameter, before any other use of the type: the type check
// decodes the type from the metadata first (the order of R78.2-1).
fn first_use(s: &before::shop::inner::Secret, p: &after::shop::Private) -> u32 {
    s.count + p.a
}

// The fields of the variants of a private enum: a pattern through a reference and a struct
// expression, and a tuple struct with private fields.
fn variants(m: &before::shop::inner::Mode, x: u32) -> u32 {
    let built = after::shop::inner::Mode::Narrow(x);
    let pair = after::shop::Pair(x, 1);
    let first = match m { before::shop::inner::Mode::Wide { x } => *x, before::shop::inner::Mode::Narrow(y) => *y };
    let second = match built { after::shop::inner::Mode::Wide { x } => x, after::shop::inner::Mode::Narrow(y) => y };
    first + second + pair.0 + pair.1
}

// Each kind of private item through `before::` and `after::`: rustc type-checks the claim.
#[same(shop::entry)]
fn reads_private(x: u32) -> bool {
    let s = before::shop::inner::Secret::new(x);
    let t = after::shop::inner::Secret::new(x);
    let p = before::shop::Private { a: x, b: true };
    let q = after::shop::Private { a: x, b: true };
    s.double() == t.double() && s.count == t.count && s.label == t.label && p.total() == q.total() && p.a == q.a
        && before::shop::private_add(x, 1) == after::shop::private_add(x, 1)
        && before::user::private_in_user(x) == after::user::private_in_user(x)
        && before::shop::shown() == after::shop::api::shown()
        && before::shop::sibling() == after::shop::api::sibling()
        && before::shop::globbed::hidden() == 2
}
"#;

// rule+ R78.1-1 R78.1-2 R78.1-3 R78.2-1 R78.2-2 R78.4-1
// rule- R78.1-1
#[test]
fn rustc_compiles_a_claim_with_each_kind_of_private_item_and_the_trees_do_not_change() {
    let pair = pair("private");
    // R78.2-1 (positive): the private module, struct, fields, methods, and functions of the two
    // versions, and the names of the glob imports, compile in the intent crate.
    let compiled = pair.compile(PRIVATE);
    assert!(compiled.ok, "{:#?}", compiled.diagnostics);
    // The callees have the record name of their tree: the definition path (also through a glob
    // import), with the facade of the tree.
    let claim = compiled.records.iter().find(|record| record.name.ends_with("::reads_private")).expect("the record of the claim");
    for name in ["before::shop::private_add", "after::shop::private_add", "before::shop::globbed::shown", "after::shop::globbed::sibling", "before::user::private_in_user"] {
        assert!(claim.q.as_deref().is_some_and(|form| form.contains(&format!("\"{name}\""))), "{name}");
    }
    // R78.2-1 (negative): a name that the glob import does not give stays an error, as in the
    // normal build (`crate::hidden` is not a name of the root of the crate).
    let wrong = pair.compile("fn f() -> u32 { before::shop::hidden() + before::shop::api::hidden() }\n");
    assert!(!wrong.ok);
    let codes: Vec<Option<String>> = wrong.diagnostics.iter().filter(|d| d.level == "error").map(|d| d.code.clone()).collect();
    assert_eq!(codes, [Some("E0425".to_string()), Some("E0425".to_string())], "{:#?}", wrong.diagnostics);
    assert!(wrong.diagnostics[0].file.as_deref() == Some(".semilla/intent/claims.rs") && wrong.diagnostics[0].rendered.contains(".semilla/intent/claims.rs:1:"), "{:#?}", wrong.diagnostics);
    // R78.1-1 (negative): a facade has the libraries of its tree only.
    let none = pair.compile("fn f() -> u32 { before::nothing::f() }\n");
    assert!(!none.ok && none.diagnostics.iter().any(|d| d.code.as_deref() == Some("E0433")), "{:#?}", none.diagnostics);
    // R78.2-2 (positive): the compile of the two versions does not change: the files of the
    // target folder are the same after the compiles of the intent crate.
    assert_eq!(hashes(&pair.scratch.join("target")), pair.target);
}

// rule- R78.2-1
#[test]
fn without_the_open_folders_each_private_item_is_an_error() {
    let pair = pair("closed");
    // R78.2-1 (negative): the same compile without the override gives the privacy errors of rustc.
    let compiled = semilla_normal::intent::compile_closed(&pair.prepared, &[Source { path: "c.rs".into(), text: PRIVATE.into() }], &pair.scratch).unwrap();
    assert!(!compiled.ok);
    let codes: BTreeSet<String> = compiled.diagnostics.iter().filter_map(|d| d.code.clone()).collect();
    assert!(codes.contains("E0603") && codes.contains("E0616") && codes.contains("E0624"), "{codes:?}");
}

const CLAIMS_TEXT: &str = r#"use semilla_intent::prelude::*;

#[change(shop::half)]
fn half_rounds_down(x: i32) -> bool {
    x >= 0 || x % 2 == 0 || after::shop::half(x) == before::shop::half(x) - 1
}

#[same(shop::half)]
fn half_same(x: i32) -> bool {
    before::shop::half(x) == after::shop::half(x)
}

#[same(shop::parse)]
fn parse_lax(x: u32) -> bool {
    before::shop::parse(x) == after::shop::parse(x, false)
}

#[same(shop::add)]
fn add_same(a: u32, b: u32) -> bool {
    before::shop::add(a, b) == after::shop::add(a, b)
}

#[same(shop::tier)]
fn tier_vacuous(k: u8) -> bool {
    before::shop::tier(k) == before::shop::tier(k) && after::shop::tier(k) == after::shop::tier(k)
}

#[same(shop::parse)]
fn calls_one_version(x: u32) -> bool {
    after::shop::parse(x, true) || x <= 4
}

#[same(shop::pt)]
fn pt_same(x: u32) -> bool {
    same(&before::shop::pt(x), &after::shop::pt(x))
}

#[same(shop::pt)]
fn pt_shapes(x: u32) -> bool {
    same(&before::shop::pt(x), &after::shop::half(1))
}

#[same(shop::ratio)]
fn ratio_float(x: f64) -> bool {
    before::shop::ratio(x) == after::shop::ratio(x)
}

#[same(shop::add)]
fn add_panics(a: u32) -> bool {
    panics(|| before::shop::add(a, u32::MAX)) || a == 0
}
"#;

// rule+ R78.4-2 R78.4-3 R78.4-4 R78.4-5
// rule- R78.4-2 R78.4-3 R78.4-4 R78.4-5
#[test]
fn each_claim_mir_gets_the_result_of_the_engine() {
    let pair = pair("claims");
    let intent = CLAIMS_TEXT;
    let results = pair.check(intent);
    let get = |name: &str| results.iter().find(|(function, _)| function == name).map(|(_, verdict)| verdict.clone()).unwrap();
    // R78.4-4 (positive): a correct property is proved; a wrong one is false with an exact input.
    assert!(matches!(get("half_rounds_down"), ClaimVerdict::Proved { .. }), "{:?}", get("half_rounds_down"));
    assert!(matches!(get("parse_lax"), ClaimVerdict::Proved { .. }), "{:?}", get("parse_lax"));
    let ClaimVerdict::False { input, .. } = get("half_same") else { panic!("{:?}", get("half_same")) };
    assert!(input.starts_with("x = -"), "{input}");
    // R78.4-4 (negative): a claim that holds for most inputs is false with the input that breaks
    // it (an overflow of the old `a + b` panics).
    assert!(matches!(get("add_same"), ClaimVerdict::False { ref detail, .. } if detail.contains("panics")), "{:?}", get("add_same"));
    // R78.4-5 (negative): a claim that is true for any result of the item is never proved, and a
    // claim that calls one version only is never proved.
    assert!(matches!(get("tier_vacuous"), ClaimVerdict::Claimed { ref cause } if cause.contains("for any result")), "{:?}", get("tier_vacuous"));
    assert!(matches!(get("calls_one_version"), ClaimVerdict::Claimed { ref cause } if cause.contains("does not call `before::shop::parse`")), "{:?}", get("calls_one_version"));
    // R78.4-3 (positive): `same(&a, &b)` compares an old and a new value part by part.
    assert!(matches!(get("pt_same"), ClaimVerdict::Proved { .. }), "{:?}", get("pt_same"));
    // R78.4-3 (negative): two values of other shapes are not compared; a relation gives them.
    assert!(matches!(get("pt_shapes"), ClaimVerdict::Claimed { ref cause } if cause.contains("other shapes")), "{:?}", get("pt_shapes"));
    // R78.4-2 (negative): a part outside the model gives "claimed" with the cause (a float, a
    // function of the prelude that the model does not give yet).
    assert!(matches!(get("ratio_float"), ClaimVerdict::Claimed { .. }), "{:?}", get("ratio_float"));
    // The closure of `panics` runs (R78.4-6): the claim calls one version of its item.
    assert!(matches!(get("add_panics"), ClaimVerdict::Claimed { ref cause } if cause.contains("does not call `after::shop::add`")), "{:?}", get("add_panics"));
}

// rule+ R78.4-10
// rule- R78.4-10
#[test]
fn the_claim_mir_check_counts_solver_steps_and_a_small_budget_stops_it() {
    let pair = pair("budget");
    let intent = "use semilla_intent::prelude::*;\n\n#[change(shop::half)]\nfn half_rounds_down(x: i32) -> bool {\n    x < 0 || after::shop::half(x) == before::shop::half(x)\n}\n";
    let full = Some(semilla_smt::budget::Budget::steps(100_000_000));
    // The budget of a claim: proved, with a count of solver steps; two runs give one count (no
    // time decides a result).
    let first = pair.check_in(intent, full);
    let second = pair.check_in(intent, full);
    assert!(matches!(first[0].1, ClaimVerdict::Proved { .. }), "{first:?}");
    assert!(first[0].2 > 0 && first[0].2 == second[0].2, "{first:?} {second:?}");
    // A budget smaller than the steps of the check stops it: "claimed" with the cause of the
    // budget, never a proof and never a false result.
    let small = pair.check_in(intent, Some(semilla_smt::budget::Budget::steps(first[0].2 / 4)));
    assert!(matches!(&small[0].1, ClaimVerdict::Claimed { cause } if cause.contains(semilla_smt::budget::BUDGET_CAUSE)), "{small:?}");
}

// rule+ R78.4-9
// rule- R78.4-9
#[test]
fn a_proved_claim_mir_names_each_changed_body_that_its_proof_uses() {
    let old = "pub fn inner(x: u32) -> u32 { x.wrapping_mul(2) }\npub fn outer(x: u32) -> u32 { inner(x).wrapping_add(1) }\npub fn noise(x: u32) -> u32 { x.wrapping_add(1) }\npub fn quiet(x: u32) -> u32 { let _ = noise(x); x }\n";
    let new = "pub fn inner(x: u32) -> u32 { x.wrapping_add(x) }\npub fn outer(x: u32) -> u32 { 1u32.wrapping_add(inner(x)) }\npub fn noise(x: u32) -> u32 { x.wrapping_add(2) }\npub fn quiet(x: u32) -> u32 { let _unused = noise(x); x }\n";
    let pair = Pair::build("depends", (old, ""), (new, ""));
    let intent = "use semilla_intent::prelude::*;\n\n#[same(shop::outer)]\nfn outer_same(x: u32) -> bool {\n    after::shop::outer(x) == before::shop::outer(x)\n}\n\n#[same(shop::quiet)]\nfn quiet_same(x: u32) -> bool {\n    after::shop::quiet(x) == before::shop::quiet(x)\n}\n";
    let results = pair.check_in(intent, None);
    // The proof of `outer` reads the result of `inner` in each tree: the two versions of
    // `inner` are in its set, after the two versions of the item.
    assert!(matches!(results[0].1, ClaimVerdict::Proved { .. }), "{results:?}");
    assert_eq!(results[0].3, ["before::shop::outer", "after::shop::outer", "before::shop::inner", "after::shop::inner"], "{results:?}");
    // The proof of `quiet` is true for any result of `noise`: `noise` is not in its set.
    assert!(matches!(results[1].1, ClaimVerdict::Proved { .. }), "{results:?}");
    assert_eq!(results[1].3, ["before::shop::quiet", "after::shop::quiet"], "{results:?}");
}

// rule- R78.1-3 R78.3-1
#[test]
fn a_claim_with_a_rustc_error_is_never_checked() {
    let pair = pair("errors");
    // R78.3-1 (negative): an error of rustc stops the compile: no record, so no result.
    let compiled = pair.compile("use semilla_intent::prelude::*;\n#[same(shop::half)]\nfn f(x: i32) -> bool { before::shop::half(x) == after::shop::half(x, 1) }\n");
    assert!(!compiled.ok && compiled.records.is_empty());
    assert_eq!(compiled.diagnostics.iter().filter(|d| d.level == "error").filter_map(|d| d.code.clone()).collect::<Vec<_>>(), ["E0061"]);
    // R78.1-3 (negative): a scope path with no definition has no resolution (the check gives
    // S0112 with the nearest names), also a method of a type.
    let scope = pair.compile("use semilla_intent::prelude::*;\n#[same(shop::hlf)]\nfn f() {}\n#[same(shop::inner::Secret::nw)]\nfn g() {}\n");
    assert!(scope.ok, "{:#?}", scope.diagnostics);
    let link = |path: &str| scope.links.iter().find(|link| link.path == path).cloned().unwrap_or_else(|| panic!("{path}: {:#?}", scope.links));
    assert!(link("before::shop::hlf").resolved.is_none() && link("before::shop::hlf").near.iter().any(|name| name == "before::shop::half"), "{:#?}", scope.links);
    assert!(link("after::shop::inner::Secret::nw").resolved.is_none() && link("after::shop::inner::Secret::nw").near.iter().any(|name| name.ends_with("::new")), "{:#?}", scope.links);
}

// rule+ R78.3-3
#[test]
fn a_machine_applicable_fix_of_rustc_passes_the_check() {
    let pair = pair("fixes");
    // R78.3-3 (positive): each machine-applicable suggestion of rustc, put in place, gives a file
    // that compiles.
    let text = "use semilla_intent::prelude::*;\n#[same(shop::tier)]\nfn f(k: u8) -> bool { let n: u64 = before::shop::tier(k); n == after::shop::tier(k) as u64 }\n";
    let compiled = pair.compile(text);
    assert!(!compiled.ok);
    let fixes: Vec<&semilla_normal::intent::Fix> = compiled.diagnostics.iter().flat_map(|d| &d.fixes).collect();
    assert!(!fixes.is_empty(), "{:#?}", compiled.diagnostics);
    let fixed = semilla_normal::intent::apply_fixes(text, &fixes);
    let again = pair.compile(&fixed);
    assert!(again.ok, "{fixed}\n{:#?}", again.diagnostics);
}

// rule+ R78.1-3
#[test]
fn rustc_resolves_a_method_scope_and_a_scope_through_a_re_export() {
    let pair = pair("scopes");
    // R78.1-3 (positive): `Type::method` (the form of the agent guide), a method of a type of
    // a re-export, and a function of a re-export: each one has its definition path.
    let compiled = pair.compile("use semilla_intent::prelude::*;\n#[same(shop::inner::Secret::new, shop::Word::get)]\nfn methods() {}\n#[change(shop::len)]\nfn len(s: &[u8]) -> bool { before::shop::len(s) == after::shop::len(s) }\n");
    assert!(compiled.ok, "{:#?}", compiled.diagnostics);
    let resolved = |path: &str| compiled.links.iter().find(|link| link.path == path).and_then(|link| link.resolved.clone());
    assert_eq!(resolved("before::shop::inner::Secret::new").as_deref(), Some("before::shop::inner::Secret::new"));
    assert_eq!(resolved("after::shop::Word::get").as_deref(), Some("after::shop::words::Word::get"));
    assert_eq!(resolved("before::shop::len").as_deref(), Some("before::shop::words::len"));
    // The record of the tree has the definition path.
    assert!(pair.old.iter().any(|record| record.name == "shop::words::len"));
}

const CLOSURES_TEXT: &str = r#"use semilla_intent::prelude::*;

#[change(shop::first)]
fn first(v: Vec<u32>) -> bool {
    if v.is_empty() {
        panics(|| before::shop::first(&v)) && after::shop::first(&v).is_none()
    } else {
        after::shop::first(&v) == Some(before::shop::first(&v))
    }
}

#[change(shop::first)]
fn first_message(v: Vec<u32>) -> bool {
    if v.is_empty() { panics_with(|| before::shop::first(&v), "index out of bounds") } else { true }
}

#[same(shop::half)]
fn half_message(x: i32) -> bool {
    panics_with(|| after::shop::half(x), "no")
}

#[same(shop::guard)]
fn guard_message(x: u32) -> bool {
    same_outcome(|| before::shop::guard(x), || after::shop::guard(x))
}

#[same(shop::guard, except = [panic_message])]
fn guard_kind(x: u32) -> bool {
    same_outcome(|| before::shop::guard(x), || after::shop::guard(x))
}

#[same(shop::small)]
fn panic_free(x: u32) -> bool {
    returns(|| before::shop::small(x)) && returns(|| after::shop::small(x))
}
"#;

// rule+ R78.4-6
// rule- R78.4-6
#[test]
fn the_closures_of_the_prelude_functions_run_on_the_engine() {
    let pair = pair("closures");
    let intent = CLOSURES_TEXT;
    let results = pair.check(intent);
    let get = |name: &str| results.iter().find(|(function, _)| function == name).map(|(_, verdict)| verdict.clone()).unwrap();
    // R78.4-6 (positive): a panic of a closure is the outcome of `panics`; the two kinds of
    // panic are the same outcome with `except = [panic_message]`.
    assert!(matches!(get("first"), ClaimVerdict::Proved { .. }), "{:?}", get("first"));
    assert!(matches!(get("guard_kind"), ClaimVerdict::Proved { .. }), "{:?}", get("guard_kind"));
    // R78.4-6 (negative): two messages are two outcomes; `panics_with` on a path that returns
    // is false; on a path that panics it is not exact (R67.3-6); a claim that each version
    // returns is true for any result (R74.2-1).
    assert!(matches!(get("guard_message"), ClaimVerdict::False { ref input, .. } if input.contains("x = ")), "{:?}", get("guard_message"));
    assert!(matches!(get("half_message"), ClaimVerdict::False { .. }), "{:?}", get("half_message"));
    assert!(matches!(get("first_message"), ClaimVerdict::Claimed { .. }), "{:?}", get("first_message"));
    assert!(matches!(get("panic_free"), ClaimVerdict::Claimed { ref cause } if cause.contains("for any result")), "{:?}", get("panic_free"));
}

// rule+ R78.1-2
// rule- R78.1-2
#[test]
fn the_second_build_gives_the_old_members_a_new_identity_and_shares_an_unchanged_member() {
    let pair = pair("second");
    // R78.1-2 (positive): the two changed members (`user` changes through `shop`) get the second
    // build; the unchanged member is one crate for the two facades (one type).
    assert_eq!(pair.names, ["shop", "user"]);
    let compiled = pair.compile("fn shared(t: before::util::Tool) -> after::util::Tool { t }\nfn two(x: u32) -> bool { before::shop::half(x as i32) == after::shop::half(x as i32) && before::user::twice(x) == after::user::twice(x) }\n");
    assert!(compiled.ok, "{:#?}", compiled.diagnostics);
    // A warm check: the second build comes from the cache, with no build.
    let started = std::time::Instant::now();
    let (again, built) = semilla_normal::intent::second_build(&pair.scratch, "pair", &pair.scratch.join("build"), &pair.names).unwrap();
    assert!(!built && again.members.len() == pair.renamed.members.len() && started.elapsed() < std::time::Duration::from_secs(2));
    // R78.1-2 (negative): with no second build, the old copy of a changed member has the
    // identity of the new member, so the pair cannot be prepared.
    let error = semilla_normal::intent::prepare(&pair.scratch, "none", &pair.libraries[0], &pair.libraries[1], None).unwrap_err();
    assert!(format!("{error:#}").contains("no copy of the second build"), "{error:#}");
}

const ROUND_TWO: &str = r#"use semilla_intent::prelude::*;

#[same(shop::abs)]
fn abs(x: i32) -> bool {
    same_outcome(|| before::shop::abs(x), || after::shop::abs(x))
}

#[same(shop::Bag::size)]
fn bag() -> bool {
    let b = before::shop::Bag::new();
    let a = after::shop::Bag::new();
    a.items.len() == b.items.len() && a.count == 0 && after::shop::Bag::size(&a) == before::shop::Bag::size(&b)
}
"#;

// rule+ R78.4-7
// rule- R78.4-7
#[test]
fn the_claims_of_round_two_get_the_results_of_main_in_dev_and_release() {
    // R78.4-7 (positive): in dev the two versions of `abs` panic with the overflow of a
    // negation, in release they return `i32::MIN`: the claim holds in each configuration. A
    // claim over private fields of a value that `Vec::new` makes is proved: the drops of the
    // values of the claim are not panics of the claim.
    let release = semilla_normal::Configuration::new(true, semilla_normal::Features::Default);
    for (name, config) in [("round-dev", semilla_normal::Configuration::DEV), ("round-release", release)] {
        let pair = Pair::build_in(name, (OLD_SHOP, OLD_USER), (&new_shop(), OLD_USER), &config);
        let results = pair.check(ROUND_TWO);
        for (function, verdict) in &results {
            assert!(matches!(verdict, ClaimVerdict::Proved { .. }), "{name}: {function}: {verdict:?}");
        }
    }
    // R78.4-7 (negative): a real difference of an outcome stays false (the message of a panic).
    let pair = pair("round-false");
    let results = pair.check("use semilla_intent::prelude::*;\n#[same(shop::guard)]\nfn g(x: u32) -> bool { same_outcome(|| before::shop::guard(x), || after::shop::guard(x)) }\n");
    assert!(matches!(results[0].1, ClaimVerdict::False { .. }), "{:?}", results[0].1);
}

impl Pair {
    /// The verdict of the model of 10r1 for each claim of an intent text that its parser reads.
    fn check_old(&self, intent: &str) -> Option<Vec<(String, ClaimVerdict)>> {
        use semilla_smt::mir::claim::{ClaimOptions, Tree, check_claim};
        let file = semilla_intent::parse_file("intent.rs", intent).ok()?;
        let solver = semilla_smt::Solver::find().expect("a Z3 solver");
        let texts = |records: &[semilla_normal::BodyRecord]| -> BTreeMap<String, String> { records.iter().map(|record| (record.name.clone(), record.text.clone())).collect() };
        let (ta, tb) = (texts(&self.old), texts(&self.new));
        let changed: BTreeSet<String> = ta.keys().chain(tb.keys()).filter(|name| ta.get(*name) != tb.get(*name)).cloned().collect();
        let changed_names: Vec<String> = changed.iter().cloned().collect();
        let is_changed = |name: &str| changed.contains(name);
        let step = semilla_smt::mir::Step { changed: &is_changed, changed_names: &changed_names };
        let find = |records: &[semilla_normal::BodyRecord], name: &str| records.iter().find(|record| record.name == name && record.q.is_some()).and_then(form);
        let wa = |name: &str| find(&self.old, name);
        let wb = |name: &str| find(&self.new, name);
        let std_of = |records: &[semilla_normal::BodyRecord]| {
            let mut found: BTreeMap<String, Arc<Ir>> = BTreeMap::new();
            for record in records {
                let Some(text) = record.qx.as_deref() else { continue };
                for (key, form) in serde_json::from_str::<BTreeMap<String, Ir>>(text).unwrap_or_default() {
                    found.entry(key).or_insert_with(|| Arc::new(form));
                }
            }
            found
        };
        let (sa, sb) = (std_of(&self.old), std_of(&self.new));
        let same = |text: &str| text.to_string();
        let names = |records: &[semilla_normal::BodyRecord]| -> Vec<String> { records.iter().map(|record| record.name.clone()).collect::<BTreeSet<_>>().into_iter().collect() };
        let (na, nb) = (names(&self.old), names(&self.new));
        let old = Tree { workspace: &wa, std: &sa, rename: &same, names: &na };
        let new = Tree { workspace: &wb, std: &sb, rename: &same, names: &nb };
        Some(
            file.claims()
                .filter(|claim| claim.func.body.is_some())
                .map(|claim| {
                    let options = ClaimOptions { except_panic_message: claim.options.except.iter().any(|name| name == "panic_message"), dry: false };
                    (claim.func.name.clone(), check_claim(&solver, &file, claim, &old, &new, &step, options, None).verdict)
                })
                .collect(),
        )
    }
}

/// [R78.4-8] The claims of this file whose model of 10r1 and claim MIR may differ, with the
/// cause (design 78.4, R78.4-8).
const ACCEPTED: &[(&str, &str)] = &[];

// rule+ R78.4-8
#[test]
fn the_claim_mir_keeps_each_proof_and_each_false_claim_of_the_model_of_10r1() {
    // R78.4-8 (positive): each claim that the model of 10r1 proves or refutes gets the same kind
    // of result from the claim MIR.
    let pair = pair("models");
    let mut problems = Vec::new();
    let mut compared = 0;
    for intent in [ROUND_TWO, CLAIMS_TEXT, CLOSURES_TEXT] {
        let Some(old) = pair.check_old(intent) else { continue };
        let new = pair.check(intent);
        for (name, verdict) in old {
            let kind = |verdict: &ClaimVerdict| match verdict {
                ClaimVerdict::Proved { .. } => "proved",
                ClaimVerdict::False { .. } => "false",
                ClaimVerdict::Claimed { .. } => "claimed",
            };
            if kind(&verdict) == "claimed" {
                continue;
            }
            compared += 1;
            let mir = new.iter().find(|(function, _)| *function == name).map(|(_, verdict)| verdict.clone());
            if mir.as_ref().map(kind) != Some(kind(&verdict)) && !ACCEPTED.iter().any(|(claim, _)| *claim == name) {
                problems.push(format!("{name}: 10r1 {} ({verdict:?}), MIR {mir:?}", kind(&verdict)));
            }
        }
    }
    eprintln!("the two models: {compared} claim(s) with a proof or a false result of 10r1 compared");
    assert!(problems.is_empty() && compared > 0, "{compared} compared: {problems:#?}");
}

// ---------------------------------------------------------------------------------------------
// Design 80, part A: the relations of the types that change.

/// A type with a new private field (a cached length), and a field whose type changes from a type
/// of the crate to a plain integer (the trial of draft 3).
const REL_OLD: &str = r#"
pub struct Cart { items: Vec<u32> }
impl Cart {
    pub fn new() -> Cart { Cart { items: Vec::new() } }
    pub fn len(&self) -> usize { self.items.len() }
    pub fn is_empty(&self) -> bool { self.items.is_empty() }
    pub fn twice(&self) -> usize { self.items.len().wrapping_mul(2) }
    pub fn push(&mut self, x: u32) { self.items.push(x) }
}
#[derive(Clone, Copy)]
pub struct Tier(u8);
pub struct Level { v: Tier }
impl Level {
    pub fn get(&self) -> u8 { self.v.0 }
    pub fn make(x: u8) -> Level { Level { v: Tier(x) } }
}
pub fn level_of(x: u8) -> Option<Level> { if x > 9 { None } else { Some(Level::make(x)) } }
"#;

const REL_NEW: &str = r#"
pub struct Cart { items: Vec<u32>, len: usize }
impl Cart {
    pub fn new() -> Cart { Cart { items: Vec::new(), len: 0 } }
    pub fn len(&self) -> usize { self.len }
    pub fn is_empty(&self) -> bool { self.len == 0 }
    pub fn twice(&self) -> usize { self.len.wrapping_mul(3) }
    pub fn push(&mut self, x: u32) { self.items.push(x); self.len += 1 }
}
pub struct Level { v: i8 }
impl Level {
    pub fn get(&self) -> u8 { self.v as u8 }
    pub fn make(x: u8) -> Level { Level { v: x as i8 } }
}
pub fn level_of(x: u8) -> Option<Level> { if x >= 10 { None } else { Some(Level::make(x)) } }
"#;

const RELATIONS: &str = r#"use semilla_intent::prelude::*;

#[relation(before::shop::Cart, after::shop::Cart)]
fn cart(b: &before::shop::Cart, a: &after::shop::Cart) -> bool {
    same(&b.items, &a.items) && a.len == b.items.len()
}

#[relation(before::shop::Level, after::shop::Level)]
fn level(b: &before::shop::Level, a: &after::shop::Level) -> bool {
    b.v.0 == a.v as u8
}
"#;

const REL_CLAIMS: &str = r#"
#[same(shop::Cart::len)]
fn len_prop(b: before::shop::Cart, a: after::shop::Cart) -> bool {
    before::shop::Cart::len(&b) == after::shop::Cart::len(&a)
}

#[same(shop::Cart::new)]
fn new_same() -> bool {
    same(&before::shop::Cart::new(), &after::shop::Cart::new())
}

#[same(shop::level_of)]
fn level_of_same(x: u8) -> bool {
    same(&before::shop::level_of(x), &after::shop::level_of(x))
}

#[same(shop::Level::make)]
fn make_same(x: u8) -> bool {
    same(&before::shop::Level::make(x), &after::shop::Level::make(x))
}
"#;

fn rel_pair(name: &str) -> Pair {
    Pair::build(name, (REL_OLD, ""), (REL_NEW, ""))
}

// rule+ R80.1-2 R80.1-3
// rule- R80.1-2 R80.1-3
#[test]
fn a_relation_relates_the_inputs_and_the_results_of_a_claim_where_the_types_differ() {
    let pair = rel_pair("relations");
    let with = pair.check(&format!("{RELATIONS}{REL_CLAIMS}"));
    let get = |results: &[(String, ClaimVerdict)], name: &str| results.iter().find(|(function, _)| function == name).map(|(_, verdict)| verdict.clone()).unwrap();
    // R80.1-3 (positive): the inputs of the two types are each pair that the relation holds
    // for, so the cached length is the length of the old list.
    assert!(matches!(get(&with, "len_prop"), ClaimVerdict::Proved { .. }), "{:?}", get(&with, "len_prop"));
    // R80.1-2 (positive): `same` of two values of the two types is their relation, also inside
    // an `Option` (part by part), and for a field whose type changes.
    assert!(matches!(get(&with, "new_same"), ClaimVerdict::Proved { .. }), "{:?}", get(&with, "new_same"));
    assert!(matches!(get(&with, "level_of_same"), ClaimVerdict::Proved { .. }), "{:?}", get(&with, "level_of_same"));
    assert!(matches!(get(&with, "make_same"), ClaimVerdict::Proved { .. }), "{:?}", get(&with, "make_same"));
    // With no relation: R80.1-3 (negative) the inputs are any two values, so the claim is false
    // with an input; R80.1-2 (negative) two values of other shapes have no correspondence.
    let without = pair.check(&format!("use semilla_intent::prelude::*;\n{REL_CLAIMS}"));
    let ClaimVerdict::False { input, .. } = get(&without, "len_prop") else { panic!("{:?}", get(&without, "len_prop")) };
    assert!(input.contains("b = ") && input.contains("a = "), "{input}");
    assert!(matches!(get(&without, "new_same"), ClaimVerdict::Claimed { ref cause } if cause.contains("no `#[relation]`")), "{:?}", get(&without, "new_same"));
}

// rule+ R80.1-4
// rule- R80.1-4
#[test]
fn an_empty_same_claim_on_items_whose_types_change_checks_related_inputs_and_outcomes() {
    let pair = rel_pair("related");
    let results = pair.check_items(RELATIONS, &["shop::Cart::new", "shop::Cart::len", "shop::Cart::is_empty", "shop::Level::get", "shop::Level::make", "shop::Cart::twice", "shop::Cart::push"]);
    let get = |name: &str| results.iter().find(|(item, ..)| item == name).cloned().unwrap();
    // R80.1-4 (positive): related inputs give related outcomes; the proof depends on the two
    // versions of the item.
    for name in ["shop::Cart::new", "shop::Cart::len", "shop::Cart::is_empty", "shop::Level::get", "shop::Level::make"] {
        let (_, verdict, depends) = get(name);
        assert!(matches!(verdict, ClaimVerdict::Proved { .. }), "{name}: {verdict:?}");
        assert_eq!(depends, [format!("before::{name}"), format!("after::{name}")], "{name}");
    }
    // R80.1-4 (negative): a method whose outcome does not correspond is false with an exact
    // pair of inputs. A `&mut self` method of a type that changes has the rule R82.1-4.
    let (_, twice, _) = get("shop::Cart::twice");
    assert!(matches!(&twice, ClaimVerdict::False { input, detail, .. } if input.contains("self_before") && input.contains("self_after") && detail.contains("returns")), "{twice:?}");
    let (_, push, _) = get("shop::Cart::push");
    assert!(matches!(&push, ClaimVerdict::Proved { .. }), "{push:?}");
}

// rule+ R80.1-5
// rule- R80.1-5
#[test]
fn a_relation_is_a_hint_and_never_makes_a_proof_by_itself() {
    let pair = rel_pair("hint");
    // R80.1-5 (positive): a right relation gives proofs that depend on the code (the test of
    // R80.1-4); a wrong relation gives "false" with an input, never "proved": the relation does
    // not say that the cached length is the length of the list.
    let loose = "use semilla_intent::prelude::*;\n#[relation(before::shop::Cart, after::shop::Cart)]\nfn cart(b: &before::shop::Cart, a: &after::shop::Cart) -> bool { same(&b.items, &a.items) }\n";
    let results = pair.check_items(loose, &["shop::Cart::len", "shop::Cart::is_empty"]);
    for (name, verdict, _) in &results {
        assert!(matches!(verdict, ClaimVerdict::False { .. }), "{name}: {verdict:?}");
    }
    let claims = pair.check(&format!("{loose}{REL_CLAIMS}"));
    assert!(matches!(claims.iter().find(|(name, _)| name == "len_prop").map(|(_, verdict)| verdict), Some(ClaimVerdict::False { .. })), "{claims:?}");
    // R80.1-5 (negative): a relation that holds for no pair makes each claim true: "claimed
    // (vacuous)". A relation that holds for each pair makes the result of `new` true for any
    // result of the old version: "claimed", never "proved".
    let never = "use semilla_intent::prelude::*;\n#[relation(before::shop::Cart, after::shop::Cart)]\nfn cart(b: &before::shop::Cart, a: &after::shop::Cart) -> bool { b.items.len() > 3 && b.items.len() < 2 && a.len == 0 }\n";
    let results = pair.check_items(never, &["shop::Cart::len"]);
    assert!(matches!(&results[0].1, ClaimVerdict::Claimed { cause } if cause.starts_with("vacuous")), "{results:?}");
    let claims = pair.check(&format!("{never}{REL_CLAIMS}"));
    assert!(matches!(claims.iter().find(|(name, _)| name == "len_prop").map(|(_, verdict)| verdict), Some(ClaimVerdict::Claimed { cause }) if cause.starts_with("vacuous")), "{claims:?}");
    let always = "use semilla_intent::prelude::*;\n#[relation(before::shop::Cart, after::shop::Cart)]\nfn cart(b: &before::shop::Cart, a: &after::shop::Cart) -> bool { let _ = (b, a); true }\n";
    let results = pair.check_items(always, &["shop::Cart::new", "shop::Cart::len"]);
    assert!(matches!(&results[0].1, ClaimVerdict::Claimed { cause } if cause.contains("for any result of `before::shop::Cart::new`") && cause.contains("never an assumption")), "{results:?}");
    assert!(matches!(&results[1].1, ClaimVerdict::False { .. }), "{results:?}");
}

/// A list of bytes with a new cached length; `count` returns the length, and `bad` of the new
/// version is one too large.
const VIEW_OLD: &str = r#"
pub struct Doc { lines: Vec<u8> }
impl Doc {
    pub fn count(&self) -> usize { self.lines.len() }
    pub fn bad(&self) -> usize { self.lines.len() }
}
pub struct Tag { v: u8 }
impl Tag {
    pub fn get(&self) -> u8 { self.v }
}
"#;

const VIEW_NEW: &str = r#"
pub struct Doc { lines: Vec<u8>, n: usize }
impl Doc {
    pub fn count(&self) -> usize { self.n }
    pub fn bad(&self) -> usize { self.n.wrapping_add(1) }
}
pub struct Tag { v: i8 }
impl Tag {
    pub fn get(&self) -> u8 { self.v as u8 }
}
"#;

const VIEW: &str = "use semilla_intent::prelude::*;\n#[relation(before::shop::Doc, after::shop::Doc)]\nfn view(b: &before::shop::Doc, a: &after::shop::Doc) -> bool {\n    a.lines == b.lines && a.n == b.lines.len()\n}\n";

// rule+ R80.1-6
// rule- R80.1-6
#[test]
fn a_relation_reads_a_vec_field_with_the_model_of_a_claim_body() {
    let pair = Pair::build("view", (VIEW_OLD, ""), (VIEW_NEW, ""));
    // A claim body with `==` of the two lists and their lengths: the same model.
    let claims = pair.check("use semilla_intent::prelude::*;\n#[same(shop::Doc::count)]\nfn count(b: before::shop::Doc, a: after::shop::Doc) -> bool {\n    !(a.lines == b.lines && a.n == b.lines.len()) || before::shop::Doc::count(&b) == after::shop::Doc::count(&a)\n}\n#[same(shop::Doc::bad)]\nfn bad(b: before::shop::Doc, a: after::shop::Doc) -> bool {\n    !(a.lines == b.lines && a.n == b.lines.len()) || before::shop::Doc::bad(&b) == after::shop::Doc::bad(&a)\n}\n");
    assert!(matches!(&claims[0].1, ClaimVerdict::Proved { .. }), "{claims:?}");
    assert!(matches!(&claims[1].1, ClaimVerdict::False { .. }), "{claims:?}");
    let results = pair.check_items(VIEW, &["shop::Doc::count", "shop::Doc::bad"]);
    // R80.1-6 (positive): the relation reads `len` and `==` of the two lists with the sequence
    // model, so the cached length gives the same count.
    assert!(matches!(&results[0].1, ClaimVerdict::Proved { .. }), "{results:?}");
    // R80.1-6 (negative): the new `bad` is one too large: false with an exact input, never proved.
    assert!(matches!(&results[1].1, ClaimVerdict::False { input, .. } if input.contains("self_before") && input.contains("self_after")), "{results:?}");
    // R80.1-6 (positive): the relation runs in the inline mode of its check: a callee of the
    // standard library with a large body (`char::is_alphanumeric`, a table of the standard
    // library) is a value of its inputs in the first try, as in a claim body, so the relation
    // still gives the proof.
    let with_call = format!("{VIEW}#[relation(before::shop::Tag, after::shop::Tag)]\nfn tag(b: &before::shop::Tag, a: &after::shop::Tag) -> bool {{\n    b.v == a.v as u8 && char::from(b.v).is_alphanumeric() == char::from(a.v as u8).is_alphanumeric()\n}}\n");
    let results = pair.check_items(&with_call, &["shop::Tag::get"]);
    assert!(matches!(&results[0].1, ClaimVerdict::Proved { .. }), "{results:?}");
}

// ---------------------------------------------------------------------------------------------
// Design 82: `&mut` and effects in claims.

/// A counter with setters, a function that writes through a `&mut` parameter, and two writers.
/// In the new version `set` clamps the value, `take` returns one more, and `emit_bytes` and
/// `emit_text` write more for one input; `bump`, `double`, `add_to`, and `scale` keep their
/// behavior with another text.
const EFF_OLD: &str = r#"
pub struct Counter { n: u32 }
impl Counter {
    pub fn new() -> Counter { Counter { n: 0 } }
    pub fn get(&self) -> u32 { self.n }
    pub fn bump(&mut self, x: u32) { self.n = self.n.wrapping_add(x) }
    pub fn set(&mut self, x: u32) { self.n = x }
    pub fn double(&mut self) { self.n = self.n.wrapping_mul(2) }
    pub fn take(&mut self) -> u32 { let n = self.n; self.n = 0; n }
}
pub fn add_to(v: &mut u64, x: u64) { *v = v.wrapping_add(x) }
pub fn scale(v: &mut u64, k: u64) { *v = v.wrapping_mul(k) }
pub fn emit_bytes(out: &mut Vec<u8>, x: u8) { out.push(x) }
pub fn emit_text(out: &mut String, x: u32) { if x > 3 { out.push_str("big") } else { out.push('s') } }
"#;

const EFF_NEW: &str = r#"
pub struct Counter { n: u32 }
impl Counter {
    pub fn new() -> Counter { Counter { n: 0 } }
    pub fn get(&self) -> u32 { self.n }
    pub fn bump(&mut self, x: u32) { self.n = x.wrapping_add(self.n) }
    pub fn set(&mut self, x: u32) { if x > 100 { self.n = 100 } else { self.n = x } }
    pub fn double(&mut self) { self.n = self.n.wrapping_add(self.n) }
    pub fn take(&mut self) -> u32 { let n = self.n; self.n = 0; n.wrapping_add(1) }
}
pub fn add_to(v: &mut u64, x: u64) { *v = x.wrapping_add(*v) }
pub fn scale(v: &mut u64, k: u64) { *v = k.wrapping_mul(*v) }
pub fn emit_bytes(out: &mut Vec<u8>, x: u8) { out.push(x); if x == 7 { out.push(x) } }
pub fn emit_text(out: &mut String, x: u32) { if x >= 3 { out.push_str("big") } else { out.push('s') } }
"#;

fn eff_pair(name: &str) -> Pair {
    Pair::build(name, (EFF_OLD, ""), (EFF_NEW, ""))
}

fn verdict_of(results: &[(String, ClaimVerdict)], name: &str) -> ClaimVerdict {
    results.iter().find(|(function, _)| function == name).map(|(_, verdict)| verdict.clone()).unwrap_or_else(|| panic!("no claim {name}"))
}

const PLACES: &str = r#"use semilla_intent::prelude::*;

#[same(shop::Counter::bump)]
fn bump_twice(x: u32, y: u32) -> bool {
    let mut b = before::shop::Counter::new();
    before::shop::Counter::bump(&mut b, x);
    before::shop::Counter::bump(&mut b, y);
    let mut a = after::shop::Counter::new();
    after::shop::Counter::bump(&mut a, x);
    after::shop::Counter::bump(&mut a, y);
    before::shop::Counter::get(&b) == after::shop::Counter::get(&a)
}

#[same(shop::Counter::set)]
fn set_local(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    before::shop::Counter::set(&mut b, x);
    let mut a = after::shop::Counter::new();
    after::shop::Counter::set(&mut a, x);
    before::shop::Counter::get(&b) == after::shop::Counter::get(&a)
}

#[same(shop::Counter::double)]
fn order_kept(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    before::shop::Counter::bump(&mut b, x);
    before::shop::Counter::double(&mut b);
    let mut a = after::shop::Counter::new();
    after::shop::Counter::bump(&mut a, x);
    after::shop::Counter::double(&mut a);
    before::shop::Counter::get(&b) == after::shop::Counter::get(&a)
}

#[same(shop::Counter::double)]
fn order_swapped(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    before::shop::Counter::bump(&mut b, x);
    before::shop::Counter::double(&mut b);
    let mut a = after::shop::Counter::new();
    after::shop::Counter::double(&mut a);
    after::shop::Counter::bump(&mut a, x);
    before::shop::Counter::get(&b) == after::shop::Counter::get(&a)
}

#[same(shop::Counter::set)]
fn read_before(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    let old = before::shop::Counter::get(&b);
    before::shop::Counter::set(&mut b, x);
    let mut a = after::shop::Counter::new();
    let new = after::shop::Counter::get(&a);
    after::shop::Counter::set(&mut a, x);
    old == new
}

#[same(shop::Counter::set)]
fn set_or_keep(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    before::shop::Counter::set(&mut b, x);
    let mut a = after::shop::Counter::new();
    after::shop::Counter::set(&mut a, x);
    let (p, q) = (before::shop::Counter::get(&b), after::shop::Counter::get(&a));
    (p == x || p == 0) && (q == x || q == 100 || q == 0)
}

#[same(shop::add_to)]
fn add_param(v: &mut u64, x: u64) -> bool {
    let start = *v;
    before::shop::add_to(v, x);
    let mut q = start;
    after::shop::add_to(&mut q, x);
    *v == q
}

#[same(shop::scale)]
fn scale_params(p: &mut u64, q: &mut u64, k: u64) -> bool {
    !(*p == *q) || {
        before::shop::scale(p, k);
        after::shop::scale(q, k);
        *p == *q
    }
}

#[same(shop::Counter::set)]
fn set_params(b: &mut before::shop::Counter, a: &mut after::shop::Counter, x: u32) -> bool {
    !(same(&*b, &*a)) || {
        before::shop::Counter::set(b, x);
        after::shop::Counter::set(a, x);
        same(&*b, &*a)
    }
}
"#;

// rule+ R82.1-1 R82.1-2
// rule- R82.1-1 R82.1-2
#[test]
fn a_local_value_and_a_mut_parameter_are_places_that_each_call_writes_in_order() {
    let pair = eff_pair("places");
    let results = pair.check(PLACES);
    // R82.1-1 (positive): two calls in a row see the value of the first; a write that changes
    // the value makes the claim false with an exact input; the order of two writes is seen.
    assert!(matches!(verdict_of(&results, "bump_twice"), ClaimVerdict::Proved { .. }), "{results:?}");
    assert!(matches!(verdict_of(&results, "order_kept"), ClaimVerdict::Proved { .. }), "{results:?}");
    assert!(matches!(verdict_of(&results, "set_local"), ClaimVerdict::False { ref input, .. } if input.starts_with("x = ")), "{results:?}");
    assert!(matches!(verdict_of(&results, "order_swapped"), ClaimVerdict::False { .. }), "{results:?}");
    // The free behavior of a version (R74.2-1) writes a free value through each `&mut`
    // argument: this proof needs the write of each version of `set` (a claim that is also true
    // when `set` writes nothing), so it depends on them and stays proved.
    assert!(matches!(verdict_of(&results, "set_or_keep"), ClaimVerdict::Proved { .. }), "{results:?}");
    // R82.1-1 (negative): a claim that reads the value only before the write never sees the
    // write, so it is not proved (it is true for any result of `set`).
    assert!(matches!(verdict_of(&results, "read_before"), ClaimVerdict::Claimed { ref cause } if cause.contains("for any result of")), "{results:?}");
    // R82.1-2 (positive): a `&mut` parameter of the claim is an input place; its value after
    // the call is the new value.
    assert!(matches!(verdict_of(&results, "add_param"), ClaimVerdict::Proved { .. }), "{results:?}");
    assert!(matches!(verdict_of(&results, "scale_params"), ClaimVerdict::Proved { .. }), "{results:?}");
    // R82.1-2 (negative): a new value that differs after the call makes the claim false, with
    // the input of the place.
    assert!(matches!(verdict_of(&results, "set_params"), ClaimVerdict::False { ref input, .. } if input.contains("b = ") && input.contains("x = ")), "{results:?}");
}

const EFFECTS: &str = r#"use semilla_intent::prelude::*;

#[same(shop::Counter::bump)]
fn bump_effects(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    let mut a = after::shop::Counter::new();
    same_effects(|| before::shop::Counter::bump(&mut b, x), || after::shop::Counter::bump(&mut a, x))
}

#[same(shop::Counter::set)]
fn set_effects(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    let mut a = after::shop::Counter::new();
    same_effects(|| before::shop::Counter::set(&mut b, x), || after::shop::Counter::set(&mut a, x))
}

#[same(shop::Counter::bump)]
fn bump_effects_value(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    let mut a = after::shop::Counter::new();
    same(&effects(|| before::shop::Counter::bump(&mut b, x)), &effects(|| after::shop::Counter::bump(&mut a, x)))
}

#[same(shop::Counter::set)]
fn set_effects_value(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    let mut a = after::shop::Counter::new();
    same(&effects(|| before::shop::Counter::set(&mut b, x)), &effects(|| after::shop::Counter::set(&mut a, x)))
}

#[same(shop::Counter::double)]
fn order_effects(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    let mut a = after::shop::Counter::new();
    same_effects(
        || {
            before::shop::Counter::bump(&mut b, x);
            before::shop::Counter::double(&mut b)
        },
        || {
            after::shop::Counter::double(&mut a);
            after::shop::Counter::bump(&mut a, x)
        },
    )
}

#[same(shop::Counter::take)]
fn take_effects(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    let mut a = after::shop::Counter::new();
    before::shop::Counter::set(&mut b, x);
    after::shop::Counter::set(&mut a, x);
    same_effects(|| before::shop::Counter::take(&mut b), || after::shop::Counter::take(&mut a))
}

#[same(shop::Counter::take)]
fn take_outcome() -> bool {
    let mut b = before::shop::Counter::new();
    let mut a = after::shop::Counter::new();
    same_outcome(|| before::shop::Counter::take(&mut b), || after::shop::Counter::take(&mut a))
}

#[same(shop::Counter::set)]
fn set_outcome(x: u32) -> bool {
    let mut b = before::shop::Counter::new();
    let mut a = after::shop::Counter::new();
    same_outcome(|| before::shop::Counter::set(&mut b, x), || after::shop::Counter::set(&mut a, x))
}
"#;

// rule+ R82.2-1 R82.2-2
// rule- R82.2-1 R82.2-2
#[test]
fn effects_and_same_effects_compare_the_writes_of_the_calls_in_their_order() {
    let pair = eff_pair("effects");
    let results = pair.check(EFFECTS);
    // R82.2-1 (positive): `effects(|| e)` is the ordered effects of a call: two calls that write
    // the same values give the same effects.
    assert!(matches!(verdict_of(&results, "bump_effects_value"), ClaimVerdict::Proved { .. }), "{results:?}");
    // R82.2-1 (negative): two calls that write other values give other effects.
    assert!(matches!(verdict_of(&results, "set_effects_value"), ClaimVerdict::False { .. }), "{results:?}");
    // R82.2-2 (positive): `same_effects` compares the effects only (`take` returns another
    // value with the same writes); the order of the writes of a call is seen.
    assert!(matches!(verdict_of(&results, "bump_effects"), ClaimVerdict::Proved { .. }), "{results:?}");
    assert!(matches!(verdict_of(&results, "take_effects"), ClaimVerdict::Proved { .. }), "{results:?}");
    assert!(matches!(verdict_of(&results, "set_effects"), ClaimVerdict::False { ref input, .. } if input.starts_with("x = ")), "{results:?}");
    assert!(matches!(verdict_of(&results, "order_effects"), ClaimVerdict::False { .. }), "{results:?}");
    // R82.2-2 (negative): `same_outcome` compares the outcomes and the writes: a write is never
    // lost, also when the two calls return `()`.
    assert!(matches!(verdict_of(&results, "set_outcome"), ClaimVerdict::False { .. }), "{results:?}");
    assert!(matches!(verdict_of(&results, "take_outcome"), ClaimVerdict::False { .. }), "{results:?}");
}

const WRITERS: &str = r#"use semilla_intent::prelude::*;

#[same(shop::emit_bytes)]
fn emit_vec(x: u8) -> bool {
    let mut s = Vec::new();
    before::shop::emit_bytes(&mut s, x);
    let mut t = Vec::new();
    after::shop::emit_bytes(&mut t, x);
    s == t
}

#[same(shop::emit_text)]
fn emit_string(x: u32) -> bool {
    let mut s = String::new();
    before::shop::emit_text(&mut s, x);
    let mut t = String::new();
    after::shop::emit_text(&mut t, x);
    s == t
}
"#;

// rule+ R82.2-3
// rule- R82.2-3
#[test]
fn a_writer_is_a_place_and_a_write_that_the_model_does_not_give_names_its_call() {
    let pair = eff_pair("writers");
    let results = pair.check(WRITERS);
    // R82.2-3 (positive): a `&mut String` writer: the check has no model of `String`, so the
    // claim is "claimed", and the cause names the write and its type (13f closes it).
    let string = verdict_of(&results, "emit_string");
    assert!(matches!(&string, ClaimVerdict::Claimed { cause } if cause.contains("writes a place of the type `String`") && cause.contains("13f")), "{string:?}");
    // R82.2-3 (negative): a `&mut Vec<u8>` writer is a place of the model: the claim is false
    // with an exact input, and no cause.
    let bytes = verdict_of(&results, "emit_vec");
    assert!(matches!(&bytes, ClaimVerdict::False { input, .. } if input == "x = 7"), "{bytes:?}");
}

// rule+ R82.3-1
// rule- R82.3-1
#[test]
fn a_false_claim_with_mut_gives_a_test_that_makes_the_calls_in_order() {
    let pair = eff_pair("trace");
    let claims = format!("{PLACES}\n#[same(shop::Counter::set)]\nfn no_call(x: u32) -> bool {{ x != 7 }}\n");
    let results = pair.check(&claims);
    let place = semilla_core::run::TestPlace { flags: "", package: "shop", before: "", after: "for x = 101: the claim returns `false`" };
    // R82.3-1 (positive): the calls of the old version on the failed path, in order: the test
    // makes the value, calls `set` with `&mut` of it, and asserts the old values after the
    // calls (the value behind `&mut`, the result of `get`).
    let ClaimVerdict::False { run: Some(case), .. } = verdict_of(&results, "set_local") else { panic!("{results:?}") };
    assert!(case.sequence && case.calls.iter().all(|call| call.side == semilla_core::run::RunSide::Before), "{case:?}");
    let names: Vec<&str> = case.calls.iter().map(|call| call.name.as_str()).collect();
    assert_eq!(names, ["shop::Counter::new", "shop::Counter::set", "shop::Counter::get"]);
    let test = semilla_core::run::suggest_claim(&case, &place).unwrap();
    for line in ["let mut value1 = crate::Counter::new();", "let _ = crate::Counter::set(&mut value1, arg2_2);", "assert!(matches!(value1, crate::Counter { n: 101 }));", "let got3 = crate::Counter::get(&value1);", "assert_eq!(got3, 101);"] {
        assert!(test.text.contains(line), "{line}\n{}", test.text);
    }
    let order = |text: &str| test.text.find(text).unwrap();
    assert!(order("Counter::new()") < order("Counter::set(") && order("Counter::set(") < order("Counter::get("), "{}", test.text);
    // The order of two writes in the test: `bump`, then `double`.
    let ClaimVerdict::False { run: Some(swapped), .. } = verdict_of(&results, "order_swapped") else { panic!("{results:?}") };
    let text = semilla_core::run::suggest_claim(&swapped, &place).unwrap().text;
    assert!(text.find("crate::Counter::bump(&mut value1").unwrap() < text.find("crate::Counter::double(&mut value1)").unwrap(), "{text}");
    // R82.3-1 (negative): a claim that is false with no call of a version has no calls: no test.
    assert!(matches!(verdict_of(&results, "no_call"), ClaimVerdict::False { run: None, .. }), "{results:?}");
}

/// The shape of a soundness finding of 13c (the coordinator, 2026-11-26): a new private field
/// caches the length; `count` reads it and `add` keeps it, but `push` forgets it (its MIR stays
/// equal to the old MIR).
const DOC_OLD: &str = r#"
pub struct Doc { lines: Vec<u8> }
impl Doc {
    pub fn new() -> Doc { Doc { lines: Vec::new() } }
    pub fn count(&self) -> usize { self.lines.len() }
    pub fn push(&mut self, x: u8) { self.lines.push(x) }
    pub fn add(&mut self, x: u8) { self.lines.push(x) }
}
"#;

const DOC_NEW: &str = r#"
pub struct Doc { lines: Vec<u8>, n: usize }
impl Doc {
    pub fn new() -> Doc { Doc { lines: Vec::new(), n: 0 } }
    pub fn count(&self) -> usize { self.n }
    pub fn push(&mut self, x: u8) { self.lines.push(x) }
    pub fn add(&mut self, x: u8) { self.lines.push(x); self.n += 1 }
}
"#;

const DOC_RELATION: &str = "use semilla_intent::prelude::*;\n#[relation(before::shop::Doc, after::shop::Doc)]\nfn doc(b: &before::shop::Doc, a: &after::shop::Doc) -> bool {\n    a.lines == b.lines && a.n == b.lines.len()\n}\n";

// rule+ R82.1-4
// rule- R82.1-4
#[test]
fn a_mut_self_of_a_type_that_changes_relates_the_two_places_after_the_call() {
    let pair = Pair::build("docs", (DOC_OLD, ""), (DOC_NEW, ""));
    let results = pair.check_items(DOC_RELATION, &["shop::Doc::add", "shop::Doc::push", "shop::Doc::count"]);
    let get = |name: &str| results.iter().find(|(item, ..)| item == name).cloned().unwrap();
    // R82.1-4 (positive): related places before the call give related places after it (`add`
    // keeps the cached length), and the proof depends on the two versions.
    let (_, add, depends) = get("shop::Doc::add");
    assert!(matches!(add, ClaimVerdict::Proved { .. }), "{add:?}");
    assert_eq!(depends, ["before::shop::Doc::add", "after::shop::Doc::add"]);
    assert!(matches!(get("shop::Doc::count").1, ClaimVerdict::Proved { .. }), "{results:?}");
    // R82.1-4 (negative): `push` forgets the cached length: the places after the call do not
    // correspond, so the item is false with an exact input (never "claimed: `&mut`").
    let (_, push, _) = get("shop::Doc::push");
    assert!(matches!(&push, ClaimVerdict::False { input, detail, .. } if input.contains("self_before") && input.contains("self_after") && detail.contains("with `*self` = ")), "{push:?}");
}
