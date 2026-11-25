//! The check of the claims of an intent file (design 67, part C): the driver writes the forms of
//! the crate `shop` before and after a change, and `semilla_smt::mir::claim` checks each claim
//! against the two versions. Each claim kind has a correct claim (proved), a wrong claim (false,
//! with an input that the check verified), a claim outside the model (claimed, with the cause),
//! and a claim with a reference error (the error with the candidates).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use semilla_smt::mir::claim::{ClaimOptions, ClaimVerdict, Tree, check_claim};
use semilla_smt::mir::{Ir, Step};

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

/// The records of one version of the crate `shop`.
fn records(scratch: &Path, name: &str, source: &str) -> Vec<semilla_normal::BodyRecord> {
    let root = scratch.join(name);
    let _ = std::fs::remove_dir_all(&root);
    write(&root.join("Cargo.toml"), "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n");
    write(&root.join("src/lib.rs"), source);
    let out = scratch.join(format!("records-{name}"));
    let _ = std::fs::remove_dir_all(&out);
    semilla_normal::run_cargo_check(&root, "n0", &out, &scratch.join(format!("target-{name}"))).unwrap_or_else(|error| panic!("{error:#}"));
    semilla_normal::read_records(&out).unwrap().into_iter().filter(|record| record.level == "n0").collect()
}

fn form(record: &semilla_normal::BodyRecord) -> Arc<Ir> {
    Arc::new(serde_json::from_str(record.q.as_deref().expect("a form")).expect("a form that the check reads"))
}

fn std_forms(records: &[semilla_normal::BodyRecord]) -> BTreeMap<String, Arc<Ir>> {
    let mut found = BTreeMap::new();
    for record in records {
        let Some(text) = record.qx.as_deref() else { continue };
        for (text, form) in serde_json::from_str::<BTreeMap<String, Ir>>(text).unwrap() {
            found.entry(text).or_insert_with(|| Arc::new(form));
        }
    }
    found
}

/// The two versions of the crate: the records of each.
struct Versions {
    old: Vec<semilla_normal::BodyRecord>,
    new: Vec<semilla_normal::BodyRecord>,
}

impl Versions {
    fn build(name: &str, old: &str, new: &str) -> Versions {
        let scratch = std::env::temp_dir().join(format!("semilla-intent-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        // SAFETY: the tests of this file set the variable to one value before each build.
        unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", ensure_driver(&scratch)) };
        let versions = Versions { old: records(&scratch, "old", old), new: records(&scratch, "new", new) };
        let _ = std::fs::remove_dir_all(&scratch);
        versions
    }

    /// Checks each claim of an intent text, in order.
    fn check(&self, intent: &str, dry: bool) -> Vec<(ClaimVerdict, Vec<String>)> {
        let file = semilla_intent::parse_file("intent.rs", intent).unwrap_or_else(|errors| panic!("{errors:?}"));
        let solver = semilla_smt::Solver::find().expect("a Z3 solver");
        let texts = |records: &[semilla_normal::BodyRecord]| -> BTreeMap<String, String> { records.iter().map(|record| (record.name.clone(), record.text.clone())).collect() };
        let (ta, tb) = (texts(&self.old), texts(&self.new));
        let changed: BTreeSet<String> = ta.keys().chain(tb.keys()).filter(|name| ta.get(*name) != tb.get(*name)).cloned().collect();
        let changed_names: Vec<String> = changed.iter().cloned().collect();
        let is_changed = |name: &str| changed.contains(name);
        let step = Step { changed: &is_changed, changed_names: &changed_names };
        let find = |records: &[semilla_normal::BodyRecord], name: &str| records.iter().find(|record| record.name == name && record.q.is_some()).map(form);
        let wa = |name: &str| find(&self.old, name);
        let wb = |name: &str| find(&self.new, name);
        let (sa, sb) = (std_forms(&self.old), std_forms(&self.new));
        let same = |text: &str| text.to_string();
        let names = |records: &[semilla_normal::BodyRecord]| -> Vec<String> { records.iter().map(|record| record.name.clone()).collect::<BTreeSet<_>>().into_iter().collect() };
        let (na, nb) = (names(&self.old), names(&self.new));
        let old = Tree { workspace: &wa, std: &sa, rename: &same, names: &na };
        let new = Tree { workspace: &wb, std: &sb, rename: &same, names: &nb };
        file.claims()
            .map(|claim| {
                let options = ClaimOptions { except_panic_message: claim.options.except.iter().any(|name| name == "panic_message"), dry };
                let checked = check_claim(&solver, &file, claim, &old, &new, &step, options, None);
                (checked.verdict, checked.errors.iter().map(|error| error.to_string()).collect())
            })
            .collect()
    }
}

const OLD: &str = r##"
pub struct Cart { items: Vec<u32>, len: u64 }

pub fn add(a: u32, b: u32) -> u32 { a + b }
pub fn parse(x: u32) -> bool { x > 3 }
pub fn first(v: &Vec<u32>) -> u32 { v[0] }
pub fn half(x: i32) -> i32 { x / 2 }
pub fn cart_len(c: &Cart) -> u64 { c.len }
pub fn count(c: &Cart) -> u64 { c.len }
pub fn clamp(x: u8) -> u8 { if x > 200 { 200 } else { x } }
pub fn tier(k: u8) -> u32 { k as u32 + 1 }
pub fn cost(k: u8) -> u32 { tier(k) * 2 }
pub fn ratio(x: f64) -> f64 { x }
#[derive(PartialEq)]
pub struct Pair { a: u32, b: u32 }
pub fn pair(x: u32) -> Pair { Pair { a: x, b: 1 } }
pub fn helper(x: u32) -> u32 { x ^ 9 }
pub fn uses(x: u32) -> u32 { helper(x) }
"##;

const NEW: &str = r##"
pub struct Cart { entries: Vec<u32>, len: u64 }

pub fn add(a: u32, b: u32) -> u32 { a.wrapping_add(b) }
pub fn parse(x: u32, strict: bool) -> bool { if strict { x > 4 } else { x > 3 } }
pub fn first(v: &Vec<u32>) -> Option<u32> { if v.is_empty() { None } else { Some(v[0]) } }
pub fn half(x: i32) -> i32 { x >> 1 }
pub fn cart_len(c: &Cart) -> u64 { c.len }
pub fn count(c: &Cart) -> u64 { c.entries.len() as u64 }
pub fn clamp(x: u8) -> u8 { if x == u8::MAX { 0 } else if x > 200 { 200 } else { x } }
pub fn tier(k: u8) -> u32 { k as u32 + 2 }
pub fn cost(k: u8) -> u32 { tier(k) * 2 }
pub fn ratio(x: f64) -> f64 { x }
#[derive(PartialEq)]
pub struct Pair { a: u32, b: u32 }
pub fn pair(x: u32) -> Pair { Pair { a: x, b: 1 } }
pub fn helper(x: u32) -> u32 { x ^ 9 }
pub fn uses(x: u32) -> u32 { let y = x; helper(y) }
"##;

const CLAIMS: &str = r#"
use semilla_intent::prelude::*;

#[same(shop::parse)]
fn parse(x: u32) -> bool {
    after::shop::parse(x, false) == before::shop::parse(x)
}

#[same(shop::parse)]
fn parse_strict(x: u32) -> bool {
    after::shop::parse(x, true) == before::shop::parse(x)
}

#[change(shop::first)]
fn first(v: Vec<u32>) -> bool {
    if v.is_empty() {
        panics(|| before::shop::first(&v)) && after::shop::first(&v).is_none()
    } else {
        after::shop::first(&v) == Some(before::shop::first(&v))
    }
}

#[change(shop::add)]
fn add(a: u32, b: u32) -> bool {
    after::shop::add(a, b) == before::shop::add(a, b)
}

#[same(shop::half)]
fn half_even(x: i32) -> bool {
    if x % 2 == 0 { after::shop::half(x) == before::shop::half(x) } else { true }
}

#[same(shop::cart_len)]
fn private_field(c: &before::shop::Cart) -> bool {
    before::shop::cart_len(c) == c.len
}

#[same(shop::count)]
fn wrong_field(c: &after::shop::Cart) -> bool {
    c.items.len() as u64 == c.len
}

#[same(shop::half)]
fn outside(x: i32) -> bool {
    match x { _ => true }
}

#[different(shop::half)]
fn half_rounds() -> bool {
    before::shop::half(-1) != after::shop::half(-1)
}

#[different(shop::half)]
fn half_even_differs() -> bool {
    before::shop::half(4) != after::shop::half(4)
}

#[same(shop::add)]
fn add_no_overflow(a: u32, b: u32) -> bool {
    if a <= 1000 && b <= 1000 { after::shop::add(a, b) == before::shop::add(a, b) } else { true }
}

#[same(shop::add)]
fn add_wrong_function(a: u32) -> bool {
    after::shop::ad(a, a) == 0
}

#[same(shop::clamp)]
fn clamp_most(x: u8) -> bool {
    after::shop::clamp(x) == before::shop::clamp(x)
}

#[same(shop::cost)]
fn cost_changed_callee(k: u8) -> bool {
    after::shop::cost(k) == before::shop::cost(k)
}

#[same(shop::ratio)]
fn ratio_float(x: f64) -> bool {
    after::shop::ratio(x) == before::shop::ratio(x)
}

#[same(shop::pair)]
fn pair_partial_eq(x: u32) -> bool {
    after::shop::pair(x) == after::shop::pair(x)
}

#[change(shop::first)]
fn first_message(v: Vec<u32>) -> bool {
    if v.is_empty() { panics_with(|| before::shop::first(&v), "index out of bounds") } else { true }
}

#[same(shop::half)]
fn half_message(x: i32) -> bool {
    panics_with(|| after::shop::half(x), "no")
}

#[same(shop::uses)]
fn unchanged_callee(x: u32) -> bool {
    after::shop::uses(x) == before::shop::uses(x)
}
"#;

fn verdicts() -> &'static Vec<(ClaimVerdict, Vec<String>)> {
    static FOUND: std::sync::OnceLock<Vec<(ClaimVerdict, Vec<String>)>> = std::sync::OnceLock::new();
    FOUND.get_or_init(|| Versions::build("main", OLD, NEW).check(CLAIMS, false))
}

fn proved(found: &(ClaimVerdict, Vec<String>)) -> bool {
    matches!(found.0, ClaimVerdict::Proved { .. })
}

fn false_input(found: &(ClaimVerdict, Vec<String>)) -> String {
    match &found.0 {
        ClaimVerdict::False { input, detail } => format!("{input} | {detail}"),
        other => panic!("not false: {other:?} {:?}", found.1),
    }
}

fn claimed(found: &(ClaimVerdict, Vec<String>)) -> String {
    match &found.0 {
        ClaimVerdict::Claimed { cause } => cause.clone(),
        other => panic!("not claimed: {other:?}"),
    }
}

// rule+ R67.3-1
#[test]
fn a_correct_property_claim_is_proved_for_each_input() {
    let found = verdicts();
    assert!(proved(&found[0]), "{:?}", found[0]);
    assert!(proved(&found[2]), "{:?}", found[2]);
    assert!(proved(&found[4]), "{:?}", found[4]);
    assert!(proved(&found[10]), "{:?}", found[10]);
}

// rule- R67.3-1
#[test]
fn a_wrong_property_claim_is_false_with_an_exact_input() {
    let found = verdicts();
    let text = false_input(&found[1]);
    assert!(text.starts_with("x = 4 |"), "{text}");
    assert!(text.contains("`after::shop::parse(x, true)` returns false") && text.contains("`before::shop::parse(x)` returns true"), "{text}");
}

// rule+ R67.3-3
#[test]
fn a_claim_that_holds_for_most_inputs_is_never_proved() {
    let found = verdicts();
    // `a + b` panics on an overflow in the old version, and `wrapping_add` does not.
    let text = false_input(&found[3]);
    assert!(text.contains("panics") && text.contains("overflow"), "{text}");
    // The two versions differ only for `x = u8::MAX`.
    let text = false_input(&found[12]);
    assert!(text.starts_with("x = 255 |"), "{text}");
}

// rule- R67.3-3
#[test]
fn a_condition_of_the_claim_limits_the_inputs() {
    assert!(proved(&verdicts()[10]), "{:?}", verdicts()[10]);
}

// rule+ R67.2-1
#[test]
fn a_claim_reads_a_private_field_of_a_version() {
    assert!(proved(&verdicts()[5]), "{:?}", verdicts()[5]);
}

// rule- R67.2-1
#[test]
fn a_field_that_the_version_does_not_have_is_an_error_with_the_fields() {
    let found = &verdicts()[6];
    assert_eq!(found.1.len(), 1, "{found:?}");
    assert!(found.1[0].contains("`after::shop::Cart.items`: the new `shop::Cart` has no field `items`; its fields: `entries`, `len`"), "{found:?}");
    let found = &verdicts()[11];
    assert!(found.1[0].contains("the new version has no function `shop::ad`") && found.1[0].contains("`shop::add`"), "{found:?}");
}

// rule+ R67.3-5
#[test]
fn a_callee_that_the_step_changes_is_inlined() {
    // `cost` has one text in the two versions, and its callee `tier` changes.
    let text = false_input(&verdicts()[13]);
    assert!(text.contains("`after::shop::cost(k)` returns") && text.contains("`before::shop::cost(k)` returns"), "{text}");
}

// rule- R67.3-5
#[test]
fn an_unchanged_callee_is_a_value_of_its_inputs() {
    assert!(proved(&verdicts()[18]), "{:?}", verdicts()[18]);
}

// rule+ R67.3-4
#[test]
fn equality_on_a_float_is_outside_the_model() {
    let cause = claimed(&verdicts()[14]);
    assert!(cause.contains("`==` on `f64`"), "{cause}");
}

// rule- R67.3-4
#[test]
fn equality_on_a_type_of_the_workspace_calls_its_partial_eq() {
    assert!(proved(&verdicts()[15]), "{:?}", verdicts()[15]);
}

// rule+ R67.3-6
#[test]
fn a_panic_message_is_outside_the_model() {
    let cause = claimed(&verdicts()[16]);
    assert!(cause.contains("no exact input"), "{cause}");
}

// rule- R67.3-6
#[test]
fn panics_with_on_a_path_that_returns_is_false() {
    let text = false_input(&verdicts()[17]);
    assert!(text.contains("the body is false"), "{text}");
}

// rule+ R67.2-2
#[test]
fn a_construct_outside_the_model_gives_claimed_with_the_cause() {
    let cause = claimed(&verdicts()[7]);
    assert!(cause.contains("`match` is outside the language"), "{cause}");
}

// rule+ R67.3-2
#[test]
fn a_different_claim_is_proved_on_its_one_input() {
    assert!(proved(&verdicts()[8]), "{:?}", verdicts()[8]);
}

// rule- R67.2-2
#[test]
fn a_claim_inside_the_model_is_not_claimed() {
    for index in [0, 1, 2, 3] {
        assert!(!matches!(verdicts()[index].0, ClaimVerdict::Claimed { .. }), "{:?}", verdicts()[index]);
    }
}

// rule- R67.3-2
#[test]
fn a_different_claim_with_no_difference_is_false() {
    let text = false_input(&verdicts()[9]);
    assert!(text.starts_with("the input of the claim |"), "{text}");
}
