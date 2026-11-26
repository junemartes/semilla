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

        heads
            .iter()
            .map(|head| {
                let name = format!("{}::claims::{}", semilla_normal::intent::CLAIMS_CRATE, head.function);
                let ir = find(&compiled.records, &name).unwrap_or_else(|| panic!("no record {name}"));
                let item = head.scope.first().map(|scope| scope.to_string());
                let property = Property { ir, own: &own, std: &own_std, item: (item.clone(), item) };
                let run = || check_property(&solver, &property, &old, &new, budget);
                let checked = if head.except.iter().any(|name| name == "panic_message") { semilla_smt::mir::claim::with_panic_messages_ignored(run) } else { run() };
                (head.function.clone(), checked.verdict, checked.steps, checked.depends)
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
