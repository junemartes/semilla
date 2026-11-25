//! Closures, constants that the compiler does not evaluate here, and small helpers of the
//! standard library in the equivalence check (design 66).
//!
//! The driver writes the forms of the crate `shop` before and after a change, and the check of
//! `semilla-smt` compares each pair of bodies (as in `tests/effects.rs`).

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

/// The records of one version of the crate `shop` (the crate `depot` is a dependency with the
/// same text in the two versions).
fn records(scratch: &Path, name: &str, source: &str) -> Vec<semilla_normal::BodyRecord> {
    let root = scratch.join(name);
    let _ = std::fs::remove_dir_all(&root);
    write(&root.join("Cargo.toml"), "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndepot = { path = \"depot\" }\n\n[workspace]\n");
    write(&root.join("src/lib.rs"), source);
    write(&root.join("depot/Cargo.toml"), "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    write(&root.join("depot/src/lib.rs"), DEPOT);
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

/// Checks the body `name` of the two versions. The changed bodies are the bodies with a
/// different text.
fn verdict(old: &[semilla_normal::BodyRecord], new: &[semilla_normal::BodyRecord], name: &str) -> Verdict {
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
    check_bodies(&solver, &old_side, &new_side, &step).verdict
}

/// A dependency with the same text in the two versions: its functions have no MIR in the
/// records of `shop` (no `#[inline]`, not generic).
const DEPOT: &str = r##"
pub fn far(x: u32) -> u32 { x.wrapping_mul(7) }
"##;

/// The helpers: the same text in the two versions.
const HELPERS: &str = r##"
use std::cell::Cell;

pub fn mix(x: u32) -> u32 { x.wrapping_mul(3) ^ 7 }
/// A generic function of the workspace that calls its closure.
pub fn apply<F: Fn(u32) -> u32>(f: F, x: u32) -> u32 { f(x) }
pub fn run<F: Fn() -> u32>(f: F) -> u32 { f() }
pub fn twice<F: FnMut(u32)>(mut f: F) { f(1); f(2); }
pub fn total(v: &[u32]) -> u32 { let mut s = 0u32; for x in v { s = s.wrapping_add(*x); } s }
/// A generic function with a loop: the check does not inline it, so a closure argument is an
/// input of a call.
pub fn apply_loop<F: Fn(u32) -> u32>(f: F, x: u32) -> u32 { let mut s = x; let mut i = 0; while i < 2 { s = f(s); i += 1; } s }
"##;

const OLD: &str = r##"
pub fn call_same(v: u32) -> u32 { let f = |x: u32| x.wrapping_add(1); f(v) }
pub fn call_diff(v: u32) -> u32 { let f = |x: u32| x.wrapping_add(1); f(v) }
pub fn option_map(o: Option<u32>, k: u32) -> Option<u32> { o.map(|x| x.wrapping_add(k)) }
pub fn option_map_diff(o: Option<u32>, k: u32) -> Option<u32> { o.map(|x| x.wrapping_add(k)) }
pub fn capture_mut_order(a: u32) -> u32 { let mut acc = a; let mut add = |y: u32| { acc = acc.wrapping_mul(3).wrapping_add(y); }; add(1); add(2); acc }
pub fn capture_value_order(a: u32) -> u32 { let f = move |y: u32| y.wrapping_add(a); let p = f(1); let q = f(2); p ^ q.rotate_left(1) }
pub fn pass_same(v: u32) -> u32 { apply(|x| x.wrapping_add(1), v) }
pub fn pass_diff(v: u32) -> u32 { apply(|x| x.wrapping_add(1), v) }
pub fn dyn_order(f: &dyn Fn(u32) -> u32) -> u32 { let a = f(1); let b = f(2); a ^ b.rotate_left(1) }
pub fn dyn_same(f: &dyn Fn(u32) -> u32) -> u32 { let a = f(1); let b = f(2); a ^ b.rotate_left(1) }
pub fn cell_capture(c: &Cell<u32>) -> u32 { let get = || c.get(); let a = get(); c.set(5); a }
pub fn plain_capture(c: &u32, d: &mut u32) -> u32 { let get = || c.wrapping_add(1); let a = get(); *d = 5; a }
pub fn once_call(v: u32) -> u32 { let k = v; let f = move || k.wrapping_mul(2); run(f) }
pub fn capture_input(v: u32) -> u32 { let k = v; run(move || k.wrapping_mul(2)) }
pub fn twice_mut(a: u32) -> u32 { let mut s = a; twice(|y| s = s.wrapping_mul(3).wrapping_add(y)); s }
pub fn loop_same(v: u32) -> u32 { apply_loop(|x| x.wrapping_add(1), v) }
pub fn loop_diff(v: u32) -> u32 { apply_loop(|x| x.wrapping_add(1), v) }
pub fn loop_capture(v: u32) -> u32 { let k = v; apply_loop(move |x| x.wrapping_add(k), v) }
pub fn loop_ref_capture(v: u32) -> u32 { let k = v; apply_loop(|x| x.wrapping_add(k), v) }
pub fn array_fold(a: u32, b: u32) -> u32 { [a, b].iter().map(|x| x.wrapping_mul(2)).fold(0u32, |s, x| s.wrapping_add(x)) }
pub fn vec_sum(v: &Vec<u32>, k: u32) -> (u32, u32) { let s: u32 = v.iter().map(|x| x.wrapping_mul(2)).sum(); let m = mix(k); (s, m) }
pub fn result_map(r: Result<u32, u8>) -> Result<u32, u8> { r.map(|x| x.wrapping_add(1)).map_err(|e| e ^ 1) }
pub fn unwrap_else(o: Option<u32>, k: u32) -> u32 { o.unwrap_or_else(|| k.wrapping_mul(2)) }
pub fn vec_sum_diff(v: &Vec<u32>) -> u32 { v.iter().map(|x| x.wrapping_mul(2)).sum() }
pub fn fn_ptr_local(v: u32) -> u32 { let f: fn(u32) -> u32 = |x| x.wrapping_add(1); f(v) }
pub fn fn_ptr_item(v: u32) -> u32 { let f: fn(u32) -> u32 = mix; f(v) }
pub const PICK: fn(u32) -> u32 = mix;
pub fn fn_ptr_const(v: u32) -> u32 { PICK(v) }
"##;

const NEW: &str = r##"
pub fn call_same(v: u32) -> u32 { let g = |y: u32| y.wrapping_add(1); g(v) }
pub fn call_diff(v: u32) -> u32 { let f = |x: u32| x.wrapping_add(2); f(v) }
pub fn option_map(o: Option<u32>, k: u32) -> Option<u32> { match o { Some(x) => Some(x.wrapping_add(k)), None => None } }
pub fn option_map_diff(o: Option<u32>, k: u32) -> Option<u32> { o.map(|x| x.wrapping_sub(k)) }
pub fn capture_mut_order(a: u32) -> u32 { let mut acc = a; let mut add = |y: u32| { acc = acc.wrapping_mul(3).wrapping_add(y); }; add(2); add(1); acc }
pub fn capture_value_order(a: u32) -> u32 { let f = move |y: u32| y.wrapping_add(a); let q = f(2); let p = f(1); p ^ q.rotate_left(1) }
pub fn pass_same(v: u32) -> u32 { apply(|y| y.wrapping_add(1), v) }
pub fn pass_diff(v: u32) -> u32 { apply(|x| x.wrapping_add(2), v) }
pub fn dyn_order(f: &dyn Fn(u32) -> u32) -> u32 { let b = f(2); let a = f(1); a ^ b.rotate_left(1) }
pub fn dyn_same(f: &dyn Fn(u32) -> u32) -> u32 { let a = f(1); let b = f(2); b.rotate_left(1) ^ a }
pub fn cell_capture(c: &Cell<u32>) -> u32 { c.set(5); let get = || c.get(); get() }
pub fn plain_capture(c: &u32, d: &mut u32) -> u32 { *d = 5; let get = || c.wrapping_add(1); get() }
pub fn once_call(v: u32) -> u32 { run(move || v.wrapping_mul(2)) }
pub fn capture_input(v: u32) -> u32 { let k = v.wrapping_add(1); run(move || k.wrapping_mul(2)) }
pub fn twice_mut(a: u32) -> u32 { let mut s = a; twice(|y| s = s.wrapping_add(y).wrapping_mul(3)); s }
pub fn loop_same(v: u32) -> u32 { apply_loop(|y| y.wrapping_add(1), v) }
pub fn loop_diff(v: u32) -> u32 { apply_loop(|x| x.wrapping_add(2), v) }
pub fn loop_capture(v: u32) -> u32 { let k = v.wrapping_add(1); apply_loop(move |x| x.wrapping_add(k), v) }
pub fn loop_ref_capture(v: u32) -> u32 { let k = v.wrapping_add(1); apply_loop(|x| x.wrapping_add(k), v) }
pub fn array_fold(a: u32, b: u32) -> u32 { a.wrapping_mul(2).wrapping_add(b.wrapping_mul(2)) }
pub fn vec_sum(v: &Vec<u32>, k: u32) -> (u32, u32) { let m = mix(k); let s: u32 = v.iter().map(|y| y.wrapping_mul(2)).sum(); (s, m) }
pub fn result_map(r: Result<u32, u8>) -> Result<u32, u8> { match r { Ok(x) => Ok(x.wrapping_add(1)), Err(e) => Err(e ^ 1) } }
pub fn unwrap_else(o: Option<u32>, k: u32) -> u32 { match o { Some(x) => x, None => k.wrapping_mul(2) } }
pub fn vec_sum_diff(v: &Vec<u32>) -> u32 { v.iter().map(|x| x.wrapping_mul(3)).sum() }
pub fn fn_ptr_local(v: u32) -> u32 { let f: fn(u32) -> u32 = |x| x.wrapping_add(2); f(v) }
pub fn fn_ptr_item(v: u32) -> u32 { let f: fn(u32) -> u32 = far_mix; f(v) }
pub fn far_mix(x: u32) -> u32 { x.wrapping_mul(3) ^ 7 }
pub const PICK: fn(u32) -> u32 = far_mix;
pub fn fn_ptr_const(v: u32) -> u32 { PICK(v) }
"##;

fn kind(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Equivalent { .. } => "equivalent",
        Verdict::Different { .. } => "different",
        Verdict::NotDecided { .. } => "not decided",
    }
}

// rule+ R66.2-1
// rule- R66.2-1
// rule+ R66.2-2
// rule- R66.2-2
// rule+ R66.2-3
// rule- R66.2-3
// rule+ R66.2-4
// rule- R66.2-4
// rule+ R66.2-5
// rule- R66.2-5
// rule+ R66.2-6
// rule- R66.2-6
// rule+ R66.2-7
// rule- R66.2-7
#[test]
fn a_closure_is_a_value_and_a_call_of_it_runs_its_body() {
    if !semilla_smt::Solver::available() {
        eprintln!("no Z3 solver: the test does not run");
        return;
    }
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-closures");
    let driver = ensure_driver(&scratch);
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
    let (old, new) = (records(&scratch, "old", &format!("{HELPERS}{OLD}")), records(&scratch, "new", &format!("{HELPERS}{NEW}")));
    // Each pair: the body, the kind of its result, and a part of the cause or the input.
    let expected: &[(&str, &str, &str)] = &[
        // R66.2-1: a closure value is its captures and the identity of its body.
        ("shop::call_same", "equivalent", ""),
        ("shop::call_diff", "different", ""),
        // R66.2-2: a call of a closure runs its body (also in a small helper of the standard
        // library: `Option::map`, `Result::map`, `map_err`, `unwrap_or_else`).
        ("shop::option_map", "equivalent", ""),
        ("shop::option_map_diff", "different", ""),
        // R66.2-3: a capture by `&mut` writes its place (no commute); a capture by value is a
        // value (two calls commute).
        ("shop::capture_mut_order", "different", ""),
        ("shop::capture_value_order", "equivalent", ""),
        // R66.2-5: two closures of the two versions are one value only with a proof of their
        // bodies (`loop_diff`: one name, two bodies; `loop_capture`: one body, two captures).
        ("shop::pass_same", "equivalent", ""),
        ("shop::pass_diff", "different", ""),
        // R66.2-7: a `dyn Fn` is an unknown callee (an effect on the world).
        ("shop::dyn_order", "not decided", "the order of the calls changes"),
        ("shop::dyn_same", "equivalent", ""),
        // R66.2-4: a capture by `&` of a `Cell` is an effect; a capture by `&` of a `u32` is not.
        ("shop::cell_capture", "not decided", "the order of the calls changes"),
        ("shop::plain_capture", "equivalent", ""),
        ("shop::once_call", "equivalent", ""),
        // R66.2-5: one body with another captured value is another value (the second try
        // inlines `run` and finds the input).
        ("shop::capture_input", "different", "v = 0"),
        ("shop::twice_mut", "different", ""),
        ("shop::loop_same", "equivalent", ""),
        ("shop::loop_diff", "not decided", ""),
        ("shop::loop_capture", "not decided", ""),
        // A capture by `&`: the value behind it is an input of the call (R64.2-4).
        ("shop::loop_ref_capture", "not decided", ""),
        // A limit (design 66.8): the iterator of a slice uses raw pointers; it is not in the
        // model, so a fold over an array stays not decided. The cause is not the closure.
        ("shop::array_fold", "not decided", "a raw pointer"),
        // R66.2-6: a closure of the standard library (the closure of `Sum::sum`) has an identity
        // from the closures of the workspace in its arguments.
        ("shop::vec_sum", "equivalent", ""),
        ("shop::result_map", "equivalent", ""),
        ("shop::unwrap_else", "equivalent", ""),
        ("shop::vec_sum_diff", "not decided", ""),
        // R66.2-7: a function pointer to a closure of the body runs the closure; a pointer to
        // a function is an input of the call.
        ("shop::fn_ptr_local", "different", "v = 0"),
        ("shop::fn_ptr_item", "not decided", ""),
        // A finding of this batch (design 66.6): a constant function pointer with a changed
        // value is an input of its call (10o gave "equivalent").
        ("shop::fn_ptr_const", "not decided", ""),
    ];
    let mut problems = Vec::new();
    for (name, want, part) in expected {
        let found = verdict(&old, &new, name);
        let text = format!("{found:?}");
        if std::env::var_os("SEMILLA_TEST_SHOW").is_some() {
            eprintln!("{name}: {text}");
        }
        if kind(&found) != *want || !text.contains(part) {
            problems.push(format!("{name}: {found:?}, the test wants {want} with `{part}`"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    let _ = std::fs::remove_dir_all(&scratch);
}
