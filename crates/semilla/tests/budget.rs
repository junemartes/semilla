//! The cost of one body check (design 66.9, part D follow-up): a body with many calls of the
//! functions of another crate of the workspace, each with deep callees, and the time limit of
//! one body check.
//!
//! The driver writes the forms of the crate `shop` before and after a change, and the check of
//! `semilla-smt` compares each pair of bodies (as in `tests/effects.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use semilla_smt::mir::{Ir, Side, Step, Verdict};

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

/// The records of one version of the crate `shop` and of its dependency `depot` (a crate of the
/// workspace).
fn records(scratch: &Path, name: &str, source: &str, depot: &str) -> Vec<semilla_normal::BodyRecord> {
    let root = scratch.join(name);
    let _ = std::fs::remove_dir_all(&root);
    write(&root.join("Cargo.toml"), "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndepot = { path = \"depot\" }\n\n[workspace]\n");
    write(&root.join("src/lib.rs"), source);
    write(&root.join("depot/Cargo.toml"), "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    write(&root.join("depot/src/lib.rs"), depot);
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

/// A crate of the workspace with a number type like a big integer (a `Vec<u64>` inside) and
/// methods with many checks and deep callees. `KEY` changes in the new version, so each
/// method that reads it changes.
const DEPOT: &str = r##"
pub const KEY: u64 = 100;
pub struct Num { pub data: Vec<u64> }
impl Num {
    pub fn from_u64(x: u64) -> Num { let mut d = Vec::new(); if x != 0 { d.push(x); } Num { data: d } }
    pub fn lo(&self) -> u64 { match self.data.first() { Some(x) => *x, None => 0 } }
    pub fn step(&self, k: u64) -> Num {
        let a = self.lo();
        let b = a.wrapping_mul(k) ^ (a >> 3);
        let c = b.wrapping_add(b >> 7) ^ (b << 5);
        let d = c.wrapping_sub(c >> 11).wrapping_add(KEY);
        Num::from_u64(d.wrapping_add(k))
    }
    pub fn root(&self, n: u32) -> Num {
        let mut r = self.step(n as u64);
        if n > 1 { r = r.step(3); }
        if n > 2 { r = r.step(5); }
        if n > 3 { r = r.step(7); }
        r
    }
    pub fn pow(&self, n: u32) -> Num {
        let mut r = self.step(2);
        if n > 1 { r = r.step(11); }
        if n > 2 { r = r.step(13); }
        r
    }
}
"##;

/// A generic test helper of `shop` with the same text in the two versions: many calls of the
/// methods of `depot`, and closures.
const SHOP: &str = r##"
pub fn check<T: Copy + Into<u64>>(x: T, n: u32) -> bool {
    let big = depot::Num::from_u64(x.into());
    let r = big.root(n);
    let s = big.root(n + 1);
    let p = r.pow(n);
    let q = s.pow(n);
    let ok = p.lo() <= big.lo() && q.lo() >= p.lo();
    let t = r.step(2).step(3).step(4);
    let low = |a: &depot::Num| a.lo() % 5;
    ok && low(&t) != low(&q)
}
"##;

/// Checks the body `name` with a budget in solver steps (R77.2-1). The changed bodies are the
/// bodies with a different text.
fn check(old: &[semilla_normal::BodyRecord], new: &[semilla_normal::BodyRecord], name: &str, budget: Option<semilla_smt::budget::Budget>) -> semilla_smt::mir::Checked {
    let solver = semilla_smt::Solver::find().expect("a Z3 solver");
    let find = |records: &[semilla_normal::BodyRecord], name: &str| records.iter().find(|record| record.name == name).cloned();
    let (a, b) = (find(old, name).unwrap_or_else(|| panic!("an old body {name}")), find(new, name).unwrap_or_else(|| panic!("a new body {name}")));
    let texts = |records: &[semilla_normal::BodyRecord]| -> BTreeMap<String, String> { records.iter().map(|record| (record.name.clone(), record.text.clone())).collect() };
    let (ta, tb) = (texts(old), texts(new));
    let changed: BTreeSet<String> = ta.keys().chain(tb.keys()).filter(|name| ta.get(*name) != tb.get(*name)).cloned().collect();
    let names: Vec<String> = changed.iter().cloned().collect();
    let is_changed = |name: &str| changed.contains(name);
    let all_std = |records: &[semilla_normal::BodyRecord]| records.iter().flat_map(std_forms).collect::<BTreeMap<_, _>>();
    let (sa, sb) = (all_std(old), all_std(new));
    let wa = |name: &str| find(old, name).map(|record| form(&record));
    let wb = |name: &str| find(new, name).map(|record| form(&record));
    let same = |text: &str| text.to_string();
    let step = Step { changed: &is_changed, changed_names: &names };
    let old_side = Side { ir: form(&a), workspace: &wa, std: &sa, rename: &same };
    let new_side = Side { ir: form(&b), workspace: &wb, std: &sb, rename: &same };
    semilla_smt::mir::check_bodies_until(&solver, &old_side, &new_side, &step, budget)
}

// rule+ R66.9-1
// rule+ R66.9-2
// rule- R66.9-2
// rule+ R77.2-2
// rule+ R77.2-3
// rule- R77.2-3
#[test]
fn a_body_check_of_many_deep_callees_is_fast_and_stops_at_its_deadline() {
    if !semilla_smt::Solver::available() {
        eprintln!("no Z3 solver: the test does not run");
        return;
    }
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-budget");
    let driver = ensure_driver(&scratch);
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
    let (old, new) = (records(&scratch, "old", SHOP, DEPOT), records(&scratch, "new", SHOP, &DEPOT.replace("KEY: u64 = 100", "KEY: u64 = 101")));
    // R66.9-1: the chain `r.step(2).step(3).step(4)` reads each value many times, so its terms
    // grow with each use. Before the names of long terms, this check took 237 s with no change
    // of the result; now it takes a few seconds.
    let started = std::time::Instant::now();
    let checked = check(&old, &new, "shop::check", None);
    let seconds = started.elapsed().as_secs_f64();
    eprintln!("shop::check: {:?} in {seconds:.2} s ({} queries)", checked.verdict, checked.queries);
    assert!(seconds < 60.0, "the check took {seconds:.2} s");
    assert!(!matches!(&checked.verdict, Verdict::NotDecided { cause } if cause == semilla_smt::mir::DEADLINE_CAUSE), "{:?}", checked.verdict);
    // R66.9-2 and R77.2-1: a check stops at its budget in solver steps, with the cause of the
    // budget. A budget of 0: no run and no solver query. A small budget: the check stops at the
    // next part (a group of steps of a run, or a query of the solver) and gives no result of a
    // part.
    use semilla_smt::budget::Budget;
    let stopped = check(&old, &new, "shop::check", Some(Budget::steps(0)));
    assert_eq!(stopped.verdict, Verdict::NotDecided { cause: semilla_smt::mir::DEADLINE_CAUSE.into() });
    assert_eq!(stopped.queries, 0);
    // A smaller body with solver queries for the runs below: the first one of the methods of
    // `depot` whose check asks the solver.
    let small_bodies = ["depot::Num::step", "depot::Num::lo", "depot::Num::root", "depot::Num::pow"];
    let (name, pow) = small_bodies.iter().map(|name| (*name, check(&old, &new, name, None))).find(|(_, found)| found.queries > 0).expect("a body whose check asks the solver");
    eprintln!("{name}: {:?}, {} queries, {} solver steps", pow.verdict, pow.queries, pow.steps);
    let stopped = check(&old, &new, name, Some(Budget::steps(pow.steps / 4)));
    assert_eq!(stopped.verdict, Verdict::NotDecided { cause: semilla_smt::mir::DEADLINE_CAUSE.into() });
    // The negative case: a budget that the check does not reach gives the normal result.
    let small = check(&old, &new, "depot::Num::lo", Some(Budget::steps(1_000_000_000)));
    assert!(matches!(small.verdict, Verdict::Equivalent { .. }), "{:?}", small.verdict);
    // [R77.2-1] The count of the solver steps of one check is the same in each run, also with a
    // parallel load (four runs of the check at one time): the budget gives the same result on
    // each machine and at each load.
    let again: Vec<semilla_smt::mir::Checked> = std::thread::scope(|scope| {
        let runs: Vec<_> = (0..4).map(|_| scope.spawn(|| check(&old, &new, name, None))).collect();
        runs.into_iter().map(|run| run.join().unwrap()).collect()
    });
    for run in &again {
        assert_eq!((run.steps, &run.verdict, run.queries), (pow.steps, &pow.verdict, pow.queries));
    }
    // A budget just below the count stops the check in each run, and the count itself does not.
    let below = check(&old, &new, name, Some(Budget::steps(pow.steps - pow.steps / 50)));
    assert!(matches!(&below.verdict, Verdict::NotDecided { cause } if cause == semilla_smt::mir::DEADLINE_CAUSE), "{:?}", below.verdict);
    let at = check(&old, &new, name, Some(Budget::steps(pow.steps + 1)));
    assert_eq!(at.verdict, pow.verdict);
    // [R77.2-3] The safety time limit is a separate cause.
    let late = check(&old, &new, name, Some(Budget::with_safety(1_000_000_000, Some(std::time::Instant::now()))));
    assert_eq!(late.verdict, Verdict::NotDecided { cause: semilla_smt::budget::SAFETY_CAUSE.into() });
    let _ = std::fs::remove_dir_all(&scratch);
}
