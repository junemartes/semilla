//! The encoding of the equivalence check (design 61, part C): the driver writes the form of the
//! bodies of one crate before and after a change, and the check of `semilla-smt` compares each
//! pair of bodies. Each supported statement and terminator has a rewrite that keeps the outcome
//! and a rewrite that changes it; each unsupported part gives "not decided" with its cause.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use semilla_smt::mir::{Ir, Side, Step, Verdict, check_bodies};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Makes sure that a driver exists, and returns its path.
fn ensure_driver(scratch: &Path) -> PathBuf {
    if let Ok(path) = semilla_normal::cargo::find_driver() {
        return path;
    }
    let target = scratch.join("driver-target");
    let status = Command::new("cargo").args(["build", "-p", "semilla-mir", "--target-dir"]).arg(&target).current_dir(semilla_normal::cargo::semilla_workspace()).status().expect("cargo runs");
    assert!(status.success(), "cannot build the driver");
    target.join("debug/semilla-mir-driver")
}

/// The records of one version of the crate `probe`.
fn records(scratch: &Path, name: &str, source: &str) -> Vec<semilla_normal::BodyRecord> {
    let root = scratch.join(name);
    let _ = std::fs::remove_dir_all(&root);
    write(&root.join("Cargo.toml"), "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n");
    write(&root.join("src/lib.rs"), source);
    let out = scratch.join(format!("records-{name}"));
    let _ = std::fs::remove_dir_all(&out);
    semilla_normal::run_cargo_check(&root, "n0", &out, &scratch.join(format!("target-{name}"))).unwrap_or_else(|error| panic!("{error:#}"));
    semilla_normal::read_records(&out).unwrap().into_iter().filter(|record| record.level == "n0").collect()
}

fn form(record: &semilla_normal::BodyRecord) -> Arc<Ir> {
    Arc::new(serde_json::from_str(record.q.as_deref().expect("a form")).expect("a form that the check reads"))
}

fn std_forms(record: &semilla_normal::BodyRecord) -> BTreeMap<String, Arc<Ir>> {
    let Some(text) = record.qx.as_deref() else { return BTreeMap::new() };
    serde_json::from_str::<BTreeMap<String, Ir>>(text).unwrap().into_iter().map(|(text, form)| (text, Arc::new(form))).collect()
}

/// Checks the body `name` of the two versions, with the bodies of each version for the inline
/// step. The changed bodies are the bodies with a different text.
fn verdict(old: &[semilla_normal::BodyRecord], new: &[semilla_normal::BodyRecord], name: &str) -> Verdict {
    let solver = semilla_smt::Solver::find().expect("a Z3 solver");
    let find = |records: &[semilla_normal::BodyRecord], name: &str| records.iter().find(|record| record.name == name).cloned();
    let (a, b) = (find(old, name).expect("an old body"), find(new, name).expect("a new body"));
    let texts = |records: &[semilla_normal::BodyRecord]| -> BTreeMap<String, String> { records.iter().map(|record| (record.name.clone(), record.text.clone())).collect() };
    let (ta, tb) = (texts(old), texts(new));
    let changed: BTreeSet<String> = ta.keys().chain(tb.keys()).filter(|name| ta.get(*name) != tb.get(*name)).cloned().collect();
    let names: Vec<String> = changed.iter().cloned().collect();
    let is_changed = |name: &str| changed.contains(name);
    let (sa, sb) = (std_forms(&a), std_forms(&b));
    let wa = |name: &str| find(old, name).map(|record| form(&record));
    let wb = |name: &str| find(new, name).map(|record| form(&record));
    let same = |text: &str| text.to_string();
    let step = Step { changed: &is_changed, changed_names: &names };
    let old_side = Side { ir: form(&a), workspace: &wa, std: &sa, rename: &same };
    let new_side = Side { ir: form(&b), workspace: &wb, std: &sb, rename: &same };
    check_bodies(&solver, &old_side, &new_side, &step).verdict
}

const OLD: &str = r##"
pub struct P { pub x: u32, pub y: u32 }
pub fn helper(x: u32) -> u32 { x ^ 9 }

pub fn add(a: u32, b: u32) -> u32 { a + b }
pub fn sub(a: u32) -> u32 { a - 1 }
pub fn more(x: u32) -> bool { x > 3 }
pub fn half(x: i32) -> i32 { x / 2 }
pub fn low(x: u32) -> u32 { x as u8 as u32 }
pub fn pair(p: (u32, u32)) -> u32 { p.0.wrapping_add(p.1) }
pub fn make(x: u32, y: u32) -> P { P { x, y } }
pub fn get(o: Option<u32>) -> u32 { match o { Some(v) => v, None => 0 } }
pub fn wrap(v: u32) -> Option<u32> { if v > 0 { Some(v) } else { None } }
pub fn bump(x: &mut u32) { *x = x.wrapping_add(1); }
pub fn div(x: u32, y: u32) -> u32 { x / y }
pub fn twice(x: u32) -> u32 { helper(x).wrapping_add(helper(x)) }
pub fn shift(x: u32, s: u32) -> u32 { x << (s % 32) }
pub fn scale(x: f64) -> f64 { x * 2.0 }
pub fn count(n: u32) -> u32 { let mut i = 0; let mut s = 0u32; while i < n { s = s.wrapping_add(i); i += 1; } s }
pub fn first(v: &[u32]) -> u32 { v[0] }
pub fn apply(x: u32) -> u32 { let f = |y: u32| y ^ 1; f(x) }
pub fn set(x: &mut u32) { *x = 1; }
pub const K: u32 = 6 * 7;
pub fn key(x: u32) -> u32 { x ^ K }
"##;

const NEW: &str = r##"
pub struct P { pub x: u32, pub y: u32 }
pub fn helper(x: u32) -> u32 { x ^ 9 }

pub fn add(a: u32, b: u32) -> u32 { b + a }
pub fn sub(a: u32) -> u32 { a - 2 }
pub fn more(x: u32) -> bool { 3 < x }
pub fn half(x: i32) -> i32 { x >> 1 }
pub fn low(x: u32) -> u32 { x & 0xff }
pub fn pair(p: (u32, u32)) -> u32 { let (a, b) = p; b.wrapping_add(a) }
pub fn make(x: u32, y: u32) -> P { P { y, x } }
pub fn get(o: Option<u32>) -> u32 { if let Some(v) = o { v } else { 0 } }
pub fn wrap(v: u32) -> Option<u32> { if v == 0 { None } else { Some(v) } }
pub fn bump(x: &mut u32) { *x = x.wrapping_add(2); }
pub fn div(x: u32, y: u32) -> u32 { if y == 0 { 0 } else { x / y } }
pub fn twice(x: u32) -> u32 { let h = helper(x); h.wrapping_add(helper(x)) }
pub fn shift(x: u32, s: u32) -> u32 { x << (s & 31) }
pub fn scale(x: f64) -> f64 { x + x }
pub fn count(n: u32) -> u32 { let mut i = 0; let mut s = 0u32; while i < n { s = s.wrapping_add(i * 1); i += 1; } s }
pub fn first(v: &[u32]) -> u32 { v[0] + 0 }
pub fn apply(x: u32) -> u32 { let f = |y: u32| y ^ 1; f(x) ^ 0 }
pub fn set(x: &mut u32) { *x = 2 - 1; }
pub const K: u32 = 6 * 7;
pub fn key(x: u32) -> u32 { x ^ 42 }
"##;

fn kind(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Equivalent { .. } => "equivalent",
        Verdict::Different { .. } => "different",
        Verdict::NotDecided { .. } => "not decided",
    }
}

// rule+ R61.4-1
// rule- R61.4-1
// rule+ R61.4-2
// rule+ R61.4-3
// rule- R61.4-3
// rule+ R61.4-7
// rule- R61.4-7
// rule+ R61.2-4
// rule- R61.2-4
#[test]
fn each_supported_part_keeps_or_changes_the_outcome_and_each_other_part_gives_not_decided() {
    if !semilla_smt::Solver::available() {
        eprintln!("no Z3 solver: the test does not run");
        return;
    }
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-equiv");
    let driver = ensure_driver(&scratch);
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
    let (old, new) = (records(&scratch, "old", OLD), records(&scratch, "new", NEW));
    // R61.2-4: the record of a body has the value of each constant item that it reads; a body
    // that reads no constant item has no list.
    let record = |name: &str| old.iter().find(|record| record.name == name).cloned().unwrap();
    assert_eq!(record("probe::key").cv, vec![("probe::K".to_string(), "42".to_string())]);
    assert!(record("probe::add").cv.is_empty());
    // Each pair: the body and the kind of its result (the dev configuration: overflow checks).
    let expected = [
        // A symmetric operation with its overflow check.
        ("probe::add", "equivalent"),
        // A changed literal: the overflow panic and the value differ.
        ("probe::sub", "different"),
        // A comparison with its operands swapped.
        ("probe::more", "equivalent"),
        // A signed division is not a shift: -1 / 2 = 0, -1 >> 1 = -1.
        ("probe::half", "different"),
        // Two integer casts and a mask.
        ("probe::low", "equivalent"),
        // A tuple field and a tuple pattern.
        ("probe::pair", "equivalent"),
        // A struct aggregate in another field order.
        ("probe::make", "equivalent"),
        // A switch on the discriminant of an enum, `match` and `if let`.
        ("probe::get", "equivalent"),
        // An enum aggregate on each branch.
        ("probe::wrap", "equivalent"),
        // A write through `&mut`: a part of the outcome.
        ("probe::bump", "different"),
        ("probe::set", "equivalent"),
        // A constant item and its value.
        ("probe::key", "equivalent"),
        // A panic (division by zero) and a return.
        ("probe::div", "different"),
        // Two calls that the check does not inline: the same calls with equal arguments.
        ("probe::twice", "equivalent"),
        // A shift masks its amount.
        ("probe::shift", "equivalent"),
        // A loop: the inductive check of the two loops (design 61.11).
        ("probe::count", "equivalent"),
        // Unsupported parts.
        // An index of a slice: the sequence model (design 64, part C).
        ("probe::first", "equivalent"),
        ("probe::scale", "not decided"),
        // A closure is a value, and a call of it runs its body (design 66, part A).
        ("probe::apply", "equivalent"),
    ];
    let mut problems = Vec::new();
    for (name, want) in expected {
        let found = verdict(&old, &new, name);
        if kind(&found) != want {
            problems.push(format!("{name}: {found:?}, the test wants {want}"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    // A different result has a real input and the two outcomes.
    match verdict(&old, &new, "probe::half") {
        Verdict::Different { input, before, after } => {
            assert!(input.starts_with("x = -"), "{input}");
            assert_ne!(before, after);
        }
        other => panic!("{other:?}"),
    }
    match verdict(&old, &new, "probe::bump") {
        Verdict::Different { before, after, .. } => assert!(before.contains("`*x` =") && after.contains("`*x` ="), "{before} / {after}"),
        other => panic!("{other:?}"),
    }
    match verdict(&old, &new, "probe::div") {
        Verdict::Different { before, after, .. } => assert!(before.contains("panics (division by zero)") && after.contains("returns 0"), "{before} / {after}"),
        other => panic!("{other:?}"),
    }
    // The cause of each unsupported part.
    for (name, cause) in [("probe::scale", "float")] {
        match verdict(&old, &new, name) {
            Verdict::NotDecided { cause: found } => assert!(found.contains(cause), "{name}: {found}"),
            other => panic!("{name}: {other:?}"),
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
