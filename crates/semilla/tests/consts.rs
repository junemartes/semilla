//! Constants that the compiler does not evaluate here (design 66, part B), and the small helpers
//! of the standard library that the first try inlines (design 66, part C).
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

/// A dependency with the same text in the two versions.
const DEPOT: &str = r##"
pub fn far(x: u32) -> u32 { x.wrapping_mul(7) }
"##;

/// The helpers: the same text in the two versions.
const HELPERS: &str = r##"
pub struct Grid<T>(pub T);
impl<T> Grid<T> {
    /// A constant of a generic impl with no change.
    pub const CELLS: usize = 4 * core::mem::size_of::<T>();
    /// A constant that reads a function with a change.
    pub const COLS: usize = unit() * core::mem::size_of::<T>();
}
pub trait Size { const N: u32; }
impl Size for u8 { const N: u32 = 1; }
impl Size for u16 { const N: u32 = 2; }
/// A trait constant whose impls do not change.
pub trait Width { const W: u32; }
impl Width for u8 { const W: u32 = 8; }
impl Width for u16 { const W: u32 = 16; }
/// A trait constant with a default value.
pub trait Tone { const T: u32 = 1; }
impl Tone for u16 {}
pub trait Depth { const D: u32; }
impl Depth for u8 { const D: u32 = 3; }
/// A loop: the check does not inline it.
pub fn spin(x: u32) -> u32 { let mut s = x; let mut i = 0; while i < 3 { s = s.rotate_left(1); i += 1; } s }
"##;

const OLD: &str = r##"
impl<T> Grid<T> {
    /// A constant whose definition changes.
    pub const ROWS: usize = 2 * core::mem::size_of::<T>();
}
impl Size for u32 { const N: u32 = 4; }
impl Tone for u8 { const T: u32 = 7; }
pub const fn unit() -> usize { 3 }
pub fn cells_same<T>(x: usize) -> usize { x.wrapping_add(Grid::<T>::CELLS) }
pub fn rows_changed<T>(x: usize) -> usize { x.wrapping_add(Grid::<T>::ROWS) }
pub fn cols_changed<T>(x: usize) -> usize { x.wrapping_add(Grid::<T>::COLS) }
pub fn size_same<T: Width>(x: u32) -> u32 { x ^ T::W }
pub fn size_changed<T: Size>(x: u32) -> u32 { x ^ T::N }
pub fn depth_added<T: Depth>(x: u32) -> u32 { x ^ T::D }
pub fn tone_default<T: Tone>(x: u32) -> u32 { x ^ T::T }
pub fn param_same<const K: usize>(x: usize) -> usize { x.wrapping_add(K) }
pub fn promoted_same<T>(i: usize, _t: T) -> u32 { let t: &[u32; 3] = &[1, 2, 3]; t[i % 3] }
pub fn promoted_changed<T>(i: usize, _t: T) -> u32 { let t: &[u32; 3] = &[1, 2, 3]; t[i % 3] }
pub fn small_std(x: u32, y: &mut u32) -> u32 { *y = spin(*y); x.saturating_sub(5) }
pub fn small_std_same(x: u32) -> u32 { let a = spin(x); a ^ x.saturating_sub(5) }
pub fn large_std(c: char, y: &mut u32) -> Option<u32> { *y = spin(*y); c.to_digit(16) }
pub unsafe fn helper_stop_diff(p: *mut u32, n: usize, a: u32, b: u32) -> (*mut u32, u32) { (unsafe { p.add(n) }, a ^ b) }
pub unsafe fn helper_stop_same(p: *mut u32, n: usize, a: u32, b: u32) -> (*mut u32, u32) { (unsafe { p.add(n) }, a ^ b) }
pub unsafe fn helper_stop_arg(p: *mut u32, n: usize, a: u32, b: u32) -> (*mut u32, u32) { (unsafe { p.add(n) }, a ^ b) }
"##;

const NEW: &str = r##"
impl<T> Grid<T> {
    /// A constant whose definition changes.
    pub const ROWS: usize = 3 * core::mem::size_of::<T>();
}
impl Size for u32 { const N: u32 = 5; }
impl Tone for u8 {}
impl Depth for u64 { const D: u32 = 9; }
pub const fn unit() -> usize { 4 }
pub fn cells_same<T>(x: usize) -> usize { Grid::<T>::CELLS.wrapping_add(x) }
pub fn rows_changed<T>(x: usize) -> usize { Grid::<T>::ROWS.wrapping_add(x) }
pub fn cols_changed<T>(x: usize) -> usize { Grid::<T>::COLS.wrapping_add(x) }
pub fn size_same<T: Width>(x: u32) -> u32 { T::W ^ x }
pub fn size_changed<T: Size>(x: u32) -> u32 { T::N ^ x }
pub fn depth_added<T: Depth>(x: u32) -> u32 { T::D ^ x }
pub fn tone_default<T: Tone>(x: u32) -> u32 { T::T ^ x }
pub fn param_same<const K: usize>(x: usize) -> usize { K.wrapping_add(x) }
pub fn promoted_same<T>(i: usize, _t: T) -> u32 { let t: &[u32; 3] = &[1, 2, 3]; let j = i % 3; t[j] }
pub fn promoted_changed<T>(i: usize, _t: T) -> u32 { let t: &[u32; 3] = &[1, 2, 4]; t[i % 3] }
pub fn small_std(x: u32, y: &mut u32) -> u32 { *y = spin(*y); x.saturating_sub(6) }
pub fn small_std_same(x: u32) -> u32 { let a = spin(x); x.saturating_sub(5) ^ a }
pub fn large_std(c: char, y: &mut u32) -> Option<u32> { *y = spin(*y); c.to_digit(10) }
pub unsafe fn helper_stop_diff(p: *mut u32, n: usize, a: u32, b: u32) -> (*mut u32, u32) { (unsafe { p.add(n) }, a | b) }
pub unsafe fn helper_stop_same(p: *mut u32, n: usize, a: u32, b: u32) -> (*mut u32, u32) { (unsafe { p.add(n) }, b ^ a) }
pub unsafe fn helper_stop_arg(p: *mut u32, n: usize, a: u32, b: u32) -> (*mut u32, u32) { (unsafe { p.add(n | 1) }, a ^ b) }
"##;

/// The helpers of the parts of a digest that the driver cannot compute (R66.10-1), the same text
/// in the two versions: a chain of functions longer than the limit of the digest and a shorter
/// chain, a chain of trait constants deeper than the limit of the depth and a shorter chain, and
/// an inline constant of a generic body.
fn limit_helpers() -> String {
    let mut text = String::new();
    // A chain of 60 functions: more items than the limit of the digest (48).
    for index in 0..60 {
        text.push_str(&format!("pub const fn k{index}() -> usize {{ k{}() + 1 }}\n", index + 1));
    }
    text.push_str("pub const fn k60() -> usize { 1 }\n");
    // A chain of 10 functions: fewer items than the limit.
    for index in 0..10 {
        text.push_str(&format!("pub const fn j{index}() -> usize {{ j{}() + 1 }}\n", index + 1));
    }
    text.push_str("pub const fn j10() -> usize { 1 }\n");
    text.push_str("pub struct Big<T>(pub T);\nimpl<T> Big<T> {\n    pub const FAR: usize = k0() * core::mem::size_of::<T>();\n    pub const NEAR: usize = j0() * core::mem::size_of::<T>();\n}\n");
    // A chain of 7 trait constants: deeper than the limit of the depth (4); a chain of 2.
    for index in 0..7 {
        let value = if index == 6 { "7".to_string() } else { format!("<u8 as L{}>::X + 1", index + 1) };
        text.push_str(&format!("pub trait L{index} {{ const X: u32; }}\nimpl L{index} for u8 {{ const X: u32 = {value}; }}\n"));
    }
    for index in 0..2 {
        let value = if index == 1 { "7".to_string() } else { format!("<u8 as M{}>::X + 1", index + 1) };
        text.push_str(&format!("pub trait M{index} {{ const X: u32; }}\nimpl M{index} for u8 {{ const X: u32 = {value}; }}\n"));
    }
    text
}

const LIMIT_OLD: &str = r##"
pub fn digest_far<T>(x: usize) -> usize { x.wrapping_add(Big::<T>::FAR) }
pub fn digest_near<T>(x: usize) -> usize { x.wrapping_add(Big::<T>::NEAR) }
pub fn depth_far<T: L0>(x: u32) -> u32 { x ^ T::X }
pub fn depth_near<T: M0>(x: u32) -> u32 { x ^ T::X }
pub fn inline_const<T>(x: usize) -> usize { x.wrapping_add(const { core::mem::size_of::<T>() + 1 }) }
"##;

const LIMIT_NEW: &str = r##"
pub fn digest_far<T>(x: usize) -> usize { Big::<T>::FAR.wrapping_add(x) }
pub fn digest_near<T>(x: usize) -> usize { Big::<T>::NEAR.wrapping_add(x) }
pub fn depth_far<T: L0>(x: u32) -> u32 { T::X ^ x }
pub fn depth_near<T: M0>(x: u32) -> u32 { T::X ^ x }
pub fn inline_const<T>(x: usize) -> usize { const { core::mem::size_of::<T>() + 1 }.wrapping_add(x) }
"##;

fn kind(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Equivalent { .. } => "equivalent",
        Verdict::Different { .. } => "different",
        Verdict::NotDecided { .. } => "not decided",
    }
}

// rule+ R66.3-1
// rule- R66.3-1
// rule+ R66.4-1
// rule- R66.4-1
// rule+ R66.10-1
// rule- R66.10-1
// rule+ R66.10-4
// rule- R66.10-4
// rule+ R66.11-4
// rule- R66.11-4
#[test]
fn a_constant_has_the_value_of_its_definition_and_a_small_helper_is_inlined() {
    if !semilla_smt::Solver::available() {
        eprintln!("no Z3 solver: the test does not run");
        return;
    }
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-consts");
    let driver = ensure_driver(&scratch);
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
    // R66.10-1: the build of each version passes (`records` stops the test if it fails), also
    // with a digest that reaches a limit of the driver.
    let limits = limit_helpers();
    let (old, new) = (records(&scratch, "old", &format!("{HELPERS}{OLD}{limits}{LIMIT_OLD}")), records(&scratch, "new", &format!("{HELPERS}{NEW}{limits}{LIMIT_NEW}")));
    if std::env::var_os("SEMILLA_TEST_SHOW").is_some() {
        for record in old.iter().chain(new.iter()).filter(|record| !record.cv.is_empty()) {
            eprintln!("{}: {:?}", record.name, record.cv);
        }
    }
    // Each pair: the body, the kind of its result, and a part of the cause or the input.
    let expected: &[(&str, &str, &str)] = &[
        // R66.3-1: a constant that the compiler does not evaluate here has one value in the two
        // versions only if its definition and each item of its value are equal.
        ("shop::cells_same", "equivalent", ""),
        ("shop::rows_changed", "not decided", "changes: `shop::Grid::<T>::ROWS"),
        ("shop::cols_changed", "not decided", "shop::unit"),
        ("shop::size_same", "equivalent", ""),
        // R66.11-4, the negative case: two known digests that differ are a change.
        ("shop::size_changed", "not decided", "a constant that the compiler does not evaluate here changes: `shop::Size::N::<T>` (changed: `shop::Size::N for u32`)"),
        // An impl of one version only is a new type, not a change of an old one; an impl that
        // takes the default value in place of its own value is a change.
        ("shop::depth_added", "equivalent", ""),
        ("shop::tone_default", "not decided", "`shop::Tone::T for u8`"),
        ("shop::param_same", "equivalent", ""),
        ("shop::promoted_same", "equivalent", ""),
        ("shop::promoted_changed", "not decided", "promoted"),
        // R66.4-1: the first try inlines a small helper of the standard library (no loop, each
        // part in the model); a larger helper stays a value of its inputs.
        ("shop::small_std", "different", "x = "),
        ("shop::small_std_same", "equivalent", ""),
        ("shop::large_std", "not decided", ""),
        // R66.10-4: the inlined body of `<*mut u32>::add` stops at the operation `offset`, so
        // the call is a value of its inputs again: the change of `a ^ b` is a sure difference
        // with an input (as at 10o). The negative cases: an equal second part gives the proof of
        // the call rule (one callee, the same inputs), and another input of the call is no
        // proof.
        ("shop::helper_stop_diff", "different", "a = 1, b = 1"),
        ("shop::helper_stop_same", "equivalent", ""),
        ("shop::helper_stop_arg", "not decided", ""),
        // R66.10-1: a part of a digest that the driver cannot compute is never equal to another
        // value: the limit of the digest, the limit of the depth. A digest under the limits keeps
        // its proof. R66.11-4: the cause says that the check cannot compare the constant (not
        // that it changes), with the cause of the part.
        ("shop::digest_far", "not decided", "{ cause: \"a constant that the check cannot compare: `shop::Big::<T>::FAR::<T>` (the limit of the digest)\" }"),
        ("shop::depth_far", "not decided", "{ cause: \"a constant that the check cannot compare: `shop::L0::X::<T>` (the limit of the depth of the digest)\" }"),
        ("shop::digest_near", "equivalent", ""),
        ("shop::depth_near", "equivalent", ""),
        ("shop::inline_const", "equivalent", ""),
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
