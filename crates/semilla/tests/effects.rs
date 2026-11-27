//! Effects and order (design 64): a call is a value unless it can have an effect (part A), the
//! read and write sets of a call (part B), and a sequence model of `Vec` and slices (part C).
//!
//! The driver writes the forms of the crate `shop` before and after a change, and the check of
//! `semilla-smt` compares each pair of bodies.

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
    // The forms of the standard library of each body (the callees of an inlined body too).
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

/// The helpers: the same text in the two versions.
const HELPERS: &str = r##"
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

pub static HITS: AtomicU32 = AtomicU32::new(0);
pub static TABLE: [u32; 4] = [3, 5, 7, 11];
pub static mut TOTAL: u32 = 0;

/// No effect and no stop: a value of its argument.
pub fn mix(x: u32) -> u32 { x.wrapping_mul(3) ^ 7 }
pub fn fold(x: u32) -> u32 { x.rotate_left(5).wrapping_add(1) }
/// A write through `&mut` with no stop.
pub fn inc(x: &mut u32) { *x = x.wrapping_add(1); }
pub fn dbl(x: &mut u32) { *x = x.rotate_left(1); }
/// A read through `&` with no stop.
pub fn look(x: &u32) -> u32 { x.wrapping_mul(5) }
/// A loop: the call can run forever (it can stop with no return value).
pub fn spin(x: &mut u32) { let mut i = 0; while i < 3 { *x = x.wrapping_add(1); i += 1; } }
pub fn twist(x: &mut u32) { let mut i = 0; while i < 2 { *x = x.rotate_left(1); i += 1; } }
/// A read of a `&mut` target with no write: the target is in the read set.
pub fn peek(x: &u32) -> u32 { let mut i = 0; let mut s = 0u32; while i < 2 { s = s.wrapping_add(*x); i += 1; } s }
/// A panic with a message.
pub fn check_small(x: u32) -> u32 { assert!(x < 10, "too large"); x }
pub fn check_even(x: u32) -> u32 { assert!(x % 2 == 0, "odd"); x }
/// Interior mutability: a write through a shared reference.
pub fn touch(c: &Cell<u32>, v: u32) { c.set(v); }
/// A shared reference to a type that is not `Freeze`, and no raw pointer in the body: only the
/// rule of interior mutability gives the call an order.
pub fn mark(c: &Cell<u32>) -> u32 { let _ = c; 7 }
/// A static with interior mutability.
pub fn hit() -> u32 { HITS.fetch_add(1, Ordering::Relaxed) }
/// A static that cannot change: a constant.
pub fn table(i: usize) -> u32 { TABLE[i % 4] }
/// A `static mut`: a raw pointer to the static.
pub fn add_total(v: u32) { unsafe { TOTAL = TOTAL.wrapping_add(v); } }
/// Input and output.
pub fn say(x: u32) { println!("{x}"); }
/// A write through a raw pointer.
pub unsafe fn put(p: *mut u32, v: u32) { unsafe { *p = v; } }
/// A process exit.
pub fn leave(code: i32) -> ! { std::process::exit(code) }
#[track_caller]
pub fn give_up() -> ! { panic!("give up") }
pub fn quiet(_: &std::panic::PanicHookInfo<'_>) {}
/// A `&mut` reference inside a value of an argument: the capture of a closure.
pub fn call_twice<F: FnMut()>(f: &mut F) { f(); f(); }
pub fn bump_with(a: u32) -> u32 { let mut x = a; let mut inc = || x = x.wrapping_add(1); call_twice(&mut inc); x }
/// A value with a destructor that has an effect (it can also panic).
pub struct Noisy(pub u32);
impl Drop for Noisy { fn drop(&mut self) { println!("{}", self.0); } }
"##;

const OLD: &str = r##"
pub fn pure_pair(x: u32, y: u32) -> (u32, u32) { let a = mix(x); let b = fold(y); (a, b) }
pub fn two_muts(a: &mut u32, b: &mut u32) { inc(a); dbl(b); }
pub fn one_mut(a: &mut u32) { inc(a); dbl(a); }
pub fn one_mut_loop(a: &mut u32) { spin(a); twist(a); }
pub fn read_other(a: &mut u32, b: &mut u32) -> u32 { inc(a); look(b) }
pub fn read_same(a: &mut u32) -> u32 { inc(a); look(a) }
pub fn two_panics(x: u32, y: u32) -> u32 { let a = check_small(x); let b = check_even(y); a ^ b }
pub fn panic_and_pure(x: u32, y: u32) -> u32 { let a = check_small(x); let b = mix(y); a ^ b }
pub fn panic_and_write(x: u32, a: &mut u32) -> u32 { let v = check_small(x); inc(a); v }
pub fn two_cells(a: &std::cell::Cell<u32>, b: &std::cell::Cell<u32>) { touch(a, 1); touch(b, 2); }
pub fn static_hits(x: u32) -> u32 { let h = hit(); let m = mix(x); h ^ m }
pub fn static_twice() -> u32 { let a = hit(); let b = hit(); a.wrapping_sub(b) }
pub fn static_table(x: u32) -> u32 { let a = table(1); let b = mix(x); a ^ b }
pub fn static_mut(a: &mut u32) { add_total(1); inc(a); }
pub fn io(x: u32, a: &mut u32) { say(x); inc(a); }
pub fn io_pure(x: u32) -> u32 { say(x); mix(x) }
pub fn dyn_call(f: &dyn Fn(u32) -> u32, a: &mut u32) -> u32 { let r = f(1); inc(a); r }
pub fn pointer_call(f: fn(u32) -> u32, x: u32) -> u32 { let r = f(x); let m = mix(x); r ^ m }
pub unsafe fn raw(p: *mut u32, q: *mut u32) { unsafe { put(p, 1); put(q, 2); } }
pub fn exit_pure(code: i32, x: u32) -> u32 { let m = mix(x); if code > 0 { leave(code) } m }
pub fn exit_io(code: i32, x: u32) { say(x); if code > 0 { leave(code) } }
pub fn behind_ref(x: u32) -> u32 { let v = x; peek(&v) }
pub fn push_two(v: &mut Vec<u32>) { v.push(1); v.push(2); }
pub fn push_same(v: &mut Vec<u32>, x: u32) { v.push(x); v.push(x | 0); }
pub fn push_pair(a: &mut Vec<u32>, b: &mut Vec<u32>) { a.push(1); b.push(2); }
pub fn len_other(a: &mut Vec<u32>, b: &Vec<u32>) -> usize { a.push(1); b.len() }
pub fn len_same(a: &mut Vec<u32>) -> usize { a.push(1); a.len() }
pub fn make() -> Vec<u32> { Vec::new() }
pub fn make_n(n: usize) -> Vec<u32> { Vec::with_capacity(n) }
pub fn pop_one(v: &mut Vec<u32>) -> Option<u32> { v.pop() }
pub fn pop_more(v: &mut Vec<u32>) -> Option<u32> { v.pop() }
pub fn empty(v: &Vec<u32>) -> bool { v.is_empty() }
pub fn empty_wrong(v: &Vec<u32>) -> bool { v.is_empty() }
pub fn clear_all(v: &mut Vec<u32>) { v.clear() }
pub fn clear_wrong(v: &mut Vec<u32>) { v.clear() }
pub fn insert_end(v: &mut Vec<u32>, x: u32) { v.insert(v.len(), x) }
pub fn insert_one(v: &mut Vec<u32>, x: u32) { v.insert(1, x) }
pub fn remove_first(v: &mut Vec<u32>, i: usize) -> u32 { v.remove(i) }
pub fn remove_guard(v: &mut Vec<u32>) -> u32 { v.remove(0) }
pub fn swap_two(v: &mut Vec<u32>) { v.swap(0, 1) }
pub fn swap_wrong(v: &mut Vec<u32>) { v.swap(0, 1) }
pub fn get_or(v: &Vec<u32>, i: usize) -> u32 { match v.get(i) { Some(x) => *x, None => 0 } }
pub fn get_wrong(v: &Vec<u32>, i: usize) -> u32 { match v.get(i) { Some(x) => *x, None => 0 } }
pub fn first_or(v: &Vec<u32>) -> u32 { match v.first() { Some(x) => *x, None => 0 } }
pub fn last_or(v: &Vec<u32>) -> u32 { match v.last() { Some(x) => *x, None => 0 } }
pub fn index_vec(v: &Vec<u32>, i: usize) -> u32 { v[i] }
pub fn index_guard(v: &Vec<u32>, i: usize) -> u32 { v[i] }
pub fn extend_two(v: &mut Vec<u32>) { v.extend_from_slice(&[1, 2]); }
pub fn extend_wrong(v: &mut Vec<u32>) { v.extend_from_slice(&[1, 2]); }
pub fn array_at(i: usize) -> u32 { let a = [1u32, 2, 3]; a[i] }
pub fn pick(a: [u32; 3], i: usize) -> u32 { a[i] }
pub fn slice_at(s: &[u32], i: usize) -> u32 { s[i] }
pub fn after_stop(x: u32) -> u32 { let _ = check_small(x); x ^ 1 }
pub fn scope_drop(a: &mut u32, s: Noisy) { inc(a); let _ = &s; }
pub fn free_drop(a: &mut u32, v: Vec<u32>) { inc(a); let _ = &v; }
pub fn cells(c: &std::cell::Cell<u32>) -> u32 { let a = mark(c); touch(c, 5); a }
pub fn pairs(v: &mut Vec<(u32, u32)>) { v.push((1, 2)); v.push((3, 4)); }
pub fn total(v: &Vec<u32>) -> u32 { v.iter().sum() }
pub fn extend_slice(v: &mut Vec<u32>, s: &[u32]) { v.extend_from_slice(s); v.push(0); }
pub fn caught_order() -> u32 { let a = std::panic::catch_unwind(hit).unwrap_or(0); let b = hit(); a.wrapping_sub(b) }
pub fn caught_twice() -> u32 { let a = std::panic::catch_unwind(hit).unwrap_or(0); let b = std::panic::catch_unwind(hit).unwrap_or(0); a.wrapping_sub(b) }
pub fn hook_order(x: u32) -> u32 { let _ = std::panic::take_hook(); say(x); x }
pub fn set_order(x: u32) -> u32 { std::panic::set_hook(Box::new(quiet)); say(x); x }
pub fn hook_pure(x: u32) -> u32 { let _ = std::panic::take_hook(); mix(x) }
pub fn fail_order(x: u32, y: u32) -> u32 { if x > 9 { give_up() } mix(y) }
"##;

const NEW: &str = r##"
pub fn pure_pair(x: u32, y: u32) -> (u32, u32) { let b = fold(y); let a = mix(x); (a, b) }
pub fn two_muts(a: &mut u32, b: &mut u32) { dbl(b); inc(a); }
pub fn one_mut(a: &mut u32) { dbl(a); inc(a); }
pub fn one_mut_loop(a: &mut u32) { twist(a); spin(a); }
pub fn read_other(a: &mut u32, b: &mut u32) -> u32 { let r = look(b); inc(a); r }
pub fn read_same(a: &mut u32) -> u32 { let r = look(a); inc(a); r }
pub fn two_panics(x: u32, y: u32) -> u32 { let b = check_even(y); let a = check_small(x); a ^ b }
pub fn panic_and_pure(x: u32, y: u32) -> u32 { let b = mix(y); let a = check_small(x); a ^ b }
pub fn panic_and_write(x: u32, a: &mut u32) -> u32 { inc(a); check_small(x) }
pub fn two_cells(a: &std::cell::Cell<u32>, b: &std::cell::Cell<u32>) { touch(b, 2); touch(a, 1); }
pub fn static_hits(x: u32) -> u32 { let m = mix(x); let h = hit(); h ^ m }
pub fn static_twice() -> u32 { let b = hit(); let a = hit(); a.wrapping_sub(b) }
pub fn static_table(x: u32) -> u32 { let b = mix(x); let a = table(1); a ^ b }
pub fn static_mut(a: &mut u32) { inc(a); add_total(1); }
pub fn io(x: u32, a: &mut u32) { inc(a); say(x); }
pub fn io_pure(x: u32) -> u32 { let m = mix(x); say(x); m }
pub fn dyn_call(f: &dyn Fn(u32) -> u32, a: &mut u32) -> u32 { inc(a); f(1) }
pub fn pointer_call(f: fn(u32) -> u32, x: u32) -> u32 { let m = mix(x); let r = f(x); r ^ m }
pub unsafe fn raw(p: *mut u32, q: *mut u32) { unsafe { put(q, 2); put(p, 1); } }
pub fn exit_pure(code: i32, x: u32) -> u32 { if code > 0 { leave(code) } mix(x) }
pub fn exit_io(code: i32, x: u32) { if code > 0 { leave(code) } say(x); }
pub fn behind_ref(x: u32) -> u32 { let v = x.wrapping_add(1); peek(&v) }
pub fn push_two(v: &mut Vec<u32>) { v.push(2); v.push(1); }
pub fn push_same(v: &mut Vec<u32>, x: u32) { v.push(x | 0); v.push(x); }
pub fn push_pair(a: &mut Vec<u32>, b: &mut Vec<u32>) { b.push(2); a.push(1); }
pub fn len_other(a: &mut Vec<u32>, b: &Vec<u32>) -> usize { let n = b.len(); a.push(1); n }
pub fn len_same(a: &mut Vec<u32>) -> usize { let n = a.len(); a.push(1); n }
pub fn make() -> Vec<u32> { Vec::with_capacity(4) }
pub fn make_n(n: usize) -> Vec<u32> { let _ = n; Vec::new() }
pub fn pop_one(v: &mut Vec<u32>) -> Option<u32> { if v.is_empty() { None } else { v.pop() } }
pub fn pop_more(v: &mut Vec<u32>) -> Option<u32> { v.pop(); v.pop() }
pub fn empty(v: &Vec<u32>) -> bool { v.len() == 0 }
pub fn empty_wrong(v: &Vec<u32>) -> bool { v.len() == 1 }
pub fn clear_all(v: &mut Vec<u32>) { v.truncate(0) }
pub fn clear_wrong(v: &mut Vec<u32>) { v.truncate(1) }
pub fn insert_end(v: &mut Vec<u32>, x: u32) { v.push(x) }
pub fn insert_one(v: &mut Vec<u32>, x: u32) { if v.is_empty() { v.push(x) } else { v.insert(1, x) } }
pub fn remove_first(v: &mut Vec<u32>, i: usize) -> u32 { let r = v.remove(i); r }
pub fn remove_guard(v: &mut Vec<u32>) -> u32 { if v.is_empty() { 0 } else { v.remove(0) } }
pub fn swap_two(v: &mut Vec<u32>) { let a = v[0]; let b = v[1]; v[0] = b; v[1] = a; }
pub fn swap_wrong(v: &mut Vec<u32>) { v.swap(0, 2) }
pub fn get_or(v: &Vec<u32>, i: usize) -> u32 { if i < v.len() { v[i] } else { 0 } }
pub fn get_wrong(v: &Vec<u32>, i: usize) -> u32 { match v.get(i) { Some(x) => *x, None => 1 } }
pub fn first_or(v: &Vec<u32>) -> u32 { if v.is_empty() { 0 } else { v[0] } }
pub fn last_or(v: &Vec<u32>) -> u32 { if v.is_empty() { 0 } else { v[0] } }
pub fn index_vec(v: &Vec<u32>, i: usize) -> u32 { let s: &[u32] = v; s[i] }
pub fn index_guard(v: &Vec<u32>, i: usize) -> u32 { if i < v.len() { v[i] } else { 0 } }
pub fn extend_two(v: &mut Vec<u32>) { v.push(1); v.push(2); }
pub fn extend_wrong(v: &mut Vec<u32>) { v.extend_from_slice(&[2, 1]); }
pub fn array_at(i: usize) -> u32 { let a = [1u32, 2, 4]; a[i] }
pub fn pick(a: [u32; 3], i: usize) -> u32 { if i < 3 { a[i] } else { 0 } }
pub fn slice_at(s: &[u32], i: usize) -> u32 { if i < s.len() { s[i] } else { s[i] } }
pub fn after_stop(x: u32) -> u32 { let _ = check_small(x); x ^ 2 }
pub fn scope_drop(a: &mut u32, s: Noisy) { { let _t = s; } inc(a); }
pub fn free_drop(a: &mut u32, v: Vec<u32>) { { let _t = v; } inc(a); }
pub fn cells(c: &std::cell::Cell<u32>) -> u32 { touch(c, 5); mark(c) }
pub fn pairs(v: &mut Vec<(u32, u32)>) { v.push((3, 4)); v.push((1, 2)); }
pub fn total(v: &Vec<u32>) -> u32 { let mut s = 0; for x in v { s += x; } s }
pub fn extend_slice(v: &mut Vec<u32>, s: &[u32]) { v.push(0); v.extend_from_slice(s); }
pub fn caught_order() -> u32 { let b = hit(); let a = std::panic::catch_unwind(hit).unwrap_or(0); a.wrapping_sub(b) }
pub fn caught_twice() -> u32 { let a = std::panic::catch_unwind(hit).unwrap_or(0); let _ = std::panic::catch_unwind(hit).unwrap_or(0); a.wrapping_sub(a) }
pub fn hook_order(x: u32) -> u32 { say(x); let _ = std::panic::take_hook(); x }
pub fn set_order(x: u32) -> u32 { say(x); std::panic::set_hook(Box::new(quiet)); x }
pub fn hook_pure(x: u32) -> u32 { let m = mix(x); let _ = std::panic::take_hook(); m }
pub fn fail_order(x: u32, y: u32) -> u32 { let m = mix(y); if x > 9 { give_up() } m }
"##;

fn kind(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Equivalent { .. } => "equivalent",
        Verdict::Different { .. } => "different",
        Verdict::NotDecided { .. } => "not decided",
    }
}

// rule+ R64.2-1
// rule- R64.2-1
// rule+ R64.2-2
// rule- R64.2-2
// rule+ R64.2-3
// rule- R64.2-3
// rule+ R64.2-4
// rule- R64.2-4
// rule+ R64.3-1
// rule- R64.3-1
// rule+ R64.3-2
// rule- R64.3-2
// rule+ R64.3-3
// rule+ R64.4-1
// rule- R64.4-1
// rule+ R64.4-2
// rule+ R64.4-3
// rule- R64.4-3
// rule+ R65.3-8
// rule- R65.3-8
#[test]
fn each_effect_keeps_the_order_of_a_call_and_a_call_with_no_effect_is_a_value() {
    if !semilla_smt::Solver::available() {
        eprintln!("no Z3 solver: the test does not run");
        return;
    }
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-effects");
    let driver = ensure_driver(&scratch);
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
    let (old, new) = (records(&scratch, "old", &format!("{HELPERS}{OLD}")), records(&scratch, "new", &format!("{HELPERS}{NEW}")));
    // Each pair: the body, the kind of its result, and a part of the cause or the input.
    let expected: &[(&str, &str, &str)] = &[
        ("shop::pure_pair", "equivalent", ""),
        ("shop::two_muts", "equivalent", ""),
        ("shop::one_mut", "different", "a = &"),
        ("shop::one_mut_loop", "not decided", "the order of the calls changes: shop::spin, shop::twist"),
        ("shop::read_other", "equivalent", ""),
        ("shop::read_same", "different", "a = &"),
        // R69.3-1: the message of a panic is a part of its outcome, so two panics in another
        // order are a sure difference when their messages differ.
        ("shop::two_panics", "different", "before: \"panics (`std::rt::panic_fmt` with the message \\\"too large\\\")\", after: \"panics (`std::rt::panic_fmt` with the message \\\"odd\\\")"),
        ("shop::panic_and_pure", "equivalent", ""),
        ("shop::panic_and_write", "different", "with `*a` = 3\", after: \"panics (`std::rt::panic_fmt` with the message \\\"too large\\\") with `*a` = 4"),
        ("shop::two_cells", "not decided", "the order of the calls changes: shop::touch"),
        ("shop::static_hits", "equivalent", ""),
        ("shop::static_twice", "not decided", ""),
        ("shop::static_table", "equivalent", ""),
        ("shop::static_mut", "not decided", "the order of the calls changes"),
        ("shop::io", "not decided", "the order of the calls changes"),
        ("shop::io_pure", "equivalent", ""),
        ("shop::dyn_call", "not decided", "the order of the calls changes"),
        ("shop::pointer_call", "equivalent", ""),
        ("shop::raw", "not decided", "the order of the calls changes: shop::put"),
        ("shop::exit_pure", "equivalent", ""),
        ("shop::exit_io", "not decided", ""),
        ("shop::behind_ref", "not decided", ""),
        ("shop::push_two", "different", "returns () with `*v` = [1, 2]\", after: \"returns () with `*v` = [2, 1]"),
        ("shop::push_same", "equivalent", ""),
        ("shop::push_pair", "equivalent", ""),
        ("shop::len_other", "equivalent", ""),
        ("shop::len_same", "different", ""),
        ("shop::make", "equivalent", ""),
        ("shop::make_n", "different", "capacity overflow"),
        ("shop::pop_one", "equivalent", ""),
        ("shop::pop_more", "different", ""),
        ("shop::empty", "equivalent", ""),
        ("shop::empty_wrong", "different", ""),
        ("shop::clear_all", "equivalent", ""),
        ("shop::clear_wrong", "different", ""),
        ("shop::insert_end", "equivalent", ""),
        ("shop::insert_one", "different", "insertion index"),
        ("shop::remove_first", "equivalent", ""),
        ("shop::remove_guard", "different", "removal index"),
        ("shop::swap_two", "equivalent", ""),
        ("shop::swap_wrong", "different", ""),
        ("shop::get_or", "equivalent", ""),
        ("shop::get_wrong", "different", ""),
        ("shop::first_or", "equivalent", ""),
        ("shop::last_or", "different", ""),
        ("shop::index_vec", "equivalent", ""),
        ("shop::index_guard", "different", "bounds"),
        ("shop::extend_two", "equivalent", ""),
        ("shop::extend_wrong", "different", ""),
        ("shop::array_at", "different", "i = 2"),
        ("shop::pick", "different", "bounds"),
        ("shop::slice_at", "equivalent", ""),
        ("shop::extend_slice", "different", "`*v` = [0, 1]"),
        ("shop::after_stop", "different", "x = 0"),
        ("shop::scope_drop", "not decided", "the order of the calls changes"),
        ("shop::free_drop", "equivalent", ""),
        ("shop::cells", "not decided", "the order of the calls changes: shop::mark, shop::touch"),
        ("shop::pairs", "not decided", "the order of the calls changes: std::vec::Vec::<(u32, u32)>::push"),
        ("shop::total", "not decided", ""),
        // R65.3-8: a panic is a callee that never returns and has `#[track_caller]`, not a name
        // prefix. `catch_unwind` runs its closure (`hit()` writes a static), so the order of the
        // two calls stays; the old prefix `std::panicking::` made it a stop with no effect, and
        // the check gave "equivalent" (a false proof). `set_hook` and `take_hook` write the
        // global hook: a world effect.
        ("shop::caught_order", "not decided", "the order of the calls changes"),
        // Two calls of `catch_unwind(hit)` give two values (the counter changes): the old prefix
        // rule made each one a value of its arguments, so `a - b` was `a - a` ("equivalent").
        ("shop::caught_twice", "not decided", ""),
        ("shop::hook_order", "not decided", "the order of the calls changes"),
        ("shop::set_order", "not decided", ""),
        ("shop::hook_pure", "equivalent", ""),
        // A function of the workspace that never returns and has `#[track_caller]` is a stop.
        ("shop::fail_order", "equivalent", ""),
    ];
    let mut problems = Vec::new();
    for (name, want, part) in expected {
        let found = verdict(&old, &new, name);
        let text = format!("{found:?}");
        if kind(&found) != *want || !text.contains(part) {
            problems.push(format!("{name}: {found:?}, the test wants {want} with `{part}`"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    // R64.2-2: a `&mut` reference inside a value of an argument (a capture by `&mut`) gives the
    // world; a `&mut` argument at the top does not.
    let form_text = |name: &str| old.iter().find(|record| record.name == name).and_then(|record| record.q.cloned()).unwrap_or_default();
    assert!(form_text("shop::bump_with").contains("an argument with a `&mut` reference inside a value"), "{}", form_text("shop::bump_with"));
    assert!(!form_text("shop::two_muts").contains("an argument with a `&mut` reference inside a value"));
    // R65.3-8: `catch_unwind` runs its closure, and `take_hook` writes the global hook: each one
    // is a world effect, not a stop with no effect (the old prefix `std::panicking::`).
    assert!(form_text("shop::caught_twice").contains("\"p\":\"std::panic::catch_unwind::<fn() -> u32 {shop::hit}, u32>\",\"e\":{\"w\":"), "{}", form_text("shop::caught_twice"));
    assert!(form_text("shop::hook_pure").contains("\"p\":\"std::panic::take_hook\",\"e\":{\"w\":"), "{}", form_text("shop::hook_pure"));
    // A function that never returns and has `#[track_caller]` is a stop with no world effect.
    assert!(form_text("shop::fail_order").contains("\"p\":\"shop::give_up\",\"w\":\"shop::give_up\",\"e\":{\"p\":true}"), "{}", form_text("shop::fail_order"));
    let _ = std::fs::remove_dir_all(&scratch);
}

