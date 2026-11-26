//! The model of the check follows the code generation (design 69, G5): the driver writes the
//! forms of the crate `probe` before and after a change in the configurations `dev` and
//! `release`, and the check of `semilla-smt` compares the bodies. An overflow check of a helper
//! of `core` that the check inlines is active only with the overflow checks of the session, a
//! panic message is a part of the outcome, and the layout and panic facts of the driver come
//! from the MIR and the marks of the compiler, not from a name.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use semilla_normal::{Configuration, Features};
use semilla_smt::mir::{Ir, Side, Step, Verdict, check_bodies};

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

/// The N0 records of one version of the crate `probe` in one configuration.
fn records(scratch: &Path, name: &str, source: &str, release: bool) -> Vec<semilla_normal::BodyRecord> {
    let root = scratch.join(format!("{name}-{release}"));
    let _ = std::fs::remove_dir_all(&root);
    write(&root.join("Cargo.toml"), "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n");
    write(&root.join("src/lib.rs"), source);
    let out = scratch.join(format!("records-{name}-{release}"));
    let _ = std::fs::remove_dir_all(&out);
    let config = Configuration::new(release, Features::Default);
    semilla_normal::run_cargo_check_delta_config(&root, "n0", &out, &scratch.join(format!("target-{name}-{release}")), &config).unwrap_or_else(|error| panic!("{error:#}"));
    semilla_normal::read_records(&out).unwrap().into_iter().filter(|record| record.level == "n0" && record.target.starts_with("probe.lib")).collect()
}

fn form(record: &semilla_normal::BodyRecord) -> Arc<Ir> {
    Arc::new(serde_json::from_str(record.q.as_deref().expect("a form")).expect("a form that the check reads"))
}

fn std_forms(record: &semilla_normal::BodyRecord) -> BTreeMap<String, Arc<Ir>> {
    let Some(text) = record.qx.as_deref() else { return BTreeMap::new() };
    serde_json::from_str::<BTreeMap<String, Ir>>(text).unwrap().into_iter().map(|(text, form)| (text, Arc::new(form))).collect()
}

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
    check_bodies(&solver, &Side { ir: form(&a), workspace: &wa, std: &sa, rename: &same }, &Side { ir: form(&b), workspace: &wb, std: &sb, rename: &same }, &step).verdict
}

const OLD: &str = r##"
pub struct Pad { pub a: u32, pub b: u8 }

pub fn magnitude(x: i32) -> i32 { if x < 0 { -x } else { x } }
pub fn next(x: u32) -> u32 { x + 1 }
pub fn unchecked(a: u32, b: u32) -> u32 { if a < 100 && b < 100 { unsafe { a.unchecked_add(b) } } else { 0 } }
pub fn guard(x: u32) -> u32 { assert!(x < 10, "too big"); x }
pub fn same_message(x: u32) -> u32 { assert!(x < 10, "too big"); x }
pub fn keep(p: Option<Pad>) -> Pad { p.unwrap() }
pub fn size() -> usize { std::mem::size_of::<Pad>() }
pub fn stop(x: u32) -> u32 { if x > 3 { panic!("no") } x }
pub fn leave(x: i32) -> i32 { if x > 3 { std::process::exit(x) } x }
#[derive(Debug)]
pub struct Shown { pub a: u8 }
pub fn shown(x: u8) -> Shown { Shown { a: x } }
pub fn pad(x: u32) -> Pad { Pad { a: x, b: 0 } }
"##;

const NEW: &str = r##"
pub struct Pad { pub a: u32, pub b: u8 }

pub fn magnitude(x: i32) -> i32 { x.abs() }
pub fn next(x: u32) -> u32 { x.wrapping_add(1) }
pub fn unchecked(a: u32, b: u32) -> u32 { if a < 100 && b < 100 { a + b } else { 0 } }
pub fn guard(x: u32) -> u32 { assert!(x < 10, "x is too big"); x }
pub fn same_message(x: u32) -> u32 { if x >= 10 { panic!("too big") } x }
pub fn keep(p: Option<Pad>) -> Pad { p.unwrap() }
pub fn size() -> usize { std::mem::size_of::<Pad>() }
pub fn stop(x: u32) -> u32 { if x > 3 { panic!("no") } x }
pub fn leave(x: i32) -> i32 { if x > 3 { std::process::exit(x) } x }
#[derive(Debug)]
pub struct Shown { pub a: u8 }
pub fn shown(x: u8) -> Shown { Shown { a: x.wrapping_add(1) } }
pub fn pad(x: u32) -> Pad { Pad { a: x, b: 1 } }
"##;

/// The records of the two versions in `dev` and in `release`.
struct Builds {
    dev: (Vec<semilla_normal::BodyRecord>, Vec<semilla_normal::BodyRecord>),
    release: (Vec<semilla_normal::BodyRecord>, Vec<semilla_normal::BodyRecord>),
}

fn builds() -> &'static Builds {
    static FOUND: std::sync::OnceLock<Builds> = std::sync::OnceLock::new();
    FOUND.get_or_init(|| {
        let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("semilla-codegen-{}", std::process::id()));
        let driver = ensure_driver(&scratch);
        // SAFETY: the tests of this file set the variable to one value before each build.
        unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
        let found = Builds {
            dev: (records(&scratch, "old", OLD, false), records(&scratch, "new", NEW, false)),
            release: (records(&scratch, "old", OLD, true), records(&scratch, "new", NEW, true)),
        };
        let _ = std::fs::remove_dir_all(&scratch);
        found
    })
}

fn solver_ok() -> bool {
    let ok = semilla_smt::Solver::available();
    if !ok {
        eprintln!("no Z3 solver: the test does not run");
    }
    ok
}

fn record<'a>(records: &'a [semilla_normal::BodyRecord], name: &str) -> &'a semilla_normal::BodyRecord {
    records.iter().find(|record| record.name == name).unwrap_or_else(|| panic!("no record {name}"))
}

// rule+ R69.1-1
#[test]
fn an_inlined_overflow_check_is_active_only_with_the_overflow_checks_of_the_session() {
    if !solver_ok() {
        return;
    }
    let builds = builds();
    // A real release build of `x.abs()` returns `i32::MIN` for `i32::MIN`: the code generation
    // removes the check of `#[rustc_inherit_overflow_checks]` code of `core`.
    let release = verdict(&builds.release.0, &builds.release.1, "probe::magnitude");
    assert!(matches!(release, Verdict::Equivalent { .. }), "{release:?}");
    let dev = verdict(&builds.dev.0, &builds.dev.1, "probe::magnitude");
    assert!(matches!(dev, Verdict::Equivalent { .. }), "{dev:?}");
    // The form of the release record has no optional overflow check; the form of `dev` has it.
    let qx = |records: &[semilla_normal::BodyRecord]| record(records, "probe::magnitude").qx.clone().unwrap_or_default();
    assert!(qx(&builds.dev.1).contains("overflow neg") && !qx(&builds.release.1).contains("overflow neg"), "{}", qx(&builds.release.1));
    // A runtime check operand (`ub_checks()` of a precondition of `core`) has the value of the
    // session in the form, never an unknown flag.
    for records in [&builds.dev.0, &builds.release.0] {
        let text = record(records, "probe::unchecked").qx.clone().unwrap_or_default();
        assert!(!text.contains("{\"R\":"), "{text}");
    }
}

// rule- R69.1-1
#[test]
fn an_overflow_of_the_code_of_the_step_stays_a_difference_in_dev() {
    if !solver_ok() {
        return;
    }
    let builds = builds();
    let dev = verdict(&builds.dev.0, &builds.dev.1, "probe::next");
    assert!(matches!(&dev, Verdict::Different { input, before, after, .. } if input == "x = 4294967295" && before.contains("overflow add") && after == "returns 0"), "{dev:?}");
    let release = verdict(&builds.release.0, &builds.release.1, "probe::next");
    assert!(matches!(release, Verdict::Equivalent { .. }), "{release:?}");
}

// rule+ R69.3-1
#[test]
fn two_panics_with_other_messages_are_different() {
    if !solver_ok() {
        return;
    }
    let builds = builds();
    let found = verdict(&builds.dev.0, &builds.dev.1, "probe::guard");
    assert!(
        matches!(&found, Verdict::Different { input, before, after, .. } if input == "x = 10" && before.contains("with the message \"too big\"") && after.contains("with the message \"x is too big\"")),
        "{found:?}"
    );
}

// rule- R69.3-1
#[test]
fn two_panics_with_one_message_are_equal() {
    if !solver_ok() {
        return;
    }
    let builds = builds();
    let found = verdict(&builds.dev.0, &builds.dev.1, "probe::same_message");
    assert!(matches!(found, Verdict::Equivalent { .. }), "{found:?}");
}

// rule+ R69.2-1
#[test]
fn a_callee_whose_mir_only_moves_a_value_keeps_it_whole() {
    let builds = builds();
    let adts = &record(&builds.dev.0, "probe::keep").adts;
    assert!(adts.iter().any(|entry| entry == "probe::Pad") && !adts.iter().any(|entry| entry == "touch probe::Pad"), "{adts:?}");
}

// rule- R69.2-1
#[test]
fn a_callee_that_asks_for_the_layout_touches_the_type() {
    let builds = builds();
    let adts = &record(&builds.dev.0, "probe::size").adts;
    assert!(adts.iter().any(|entry| entry == "touch probe::Pad"), "{adts:?}");
}

// rule+ R69.2-2
#[test]
fn a_panic_function_of_the_standard_library_is_not_an_end() {
    let builds = builds();
    let ends = record(&builds.dev.0, "probe::stop").sure_ends.clone().unwrap_or_default();
    assert!(ends.get("ends").is_none_or(Vec::is_empty), "{ends:?}");
}

// rule- R69.2-2
#[test]
fn an_exit_of_the_standard_library_is_an_end() {
    let builds = builds();
    let ends = record(&builds.dev.0, "probe::leave").sure_ends.clone().unwrap_or_default();
    assert!(ends.get("ends").is_some_and(|list| list.iter().any(|end| end.contains("exit"))), "{ends:?}");
}

/// The outcome of each version that the suggested test of a difference asserts (design 75).
fn expected(found: &Verdict) -> (semilla_core::run::RunExit, semilla_core::run::RunExit) {
    let Verdict::Different { run: Some(case), .. } = found else { panic!("no run case: {found:?}") };
    (case.calls[0].expect.exit.clone(), case.calls[1].expect.exit.clone())
}

// rule+ R75.1-1
#[test]
fn the_model_gives_the_message_of_a_check_and_the_pattern_of_a_struct_with_no_debug() {
    use semilla_core::run::{Pat, RunExit};
    if !solver_ok() {
        return;
    }
    let builds = builds();
    // The message of the panic of an overflow check: from the panic function of the compiler.
    let (before, after) = expected(&verdict(&builds.dev.0, &builds.dev.1, "probe::next"));
    assert_eq!(before, RunExit::Panics(Some("attempt to add with overflow".into())));
    assert_eq!(after, RunExit::Returns(Pat::Lit("0".into())));
    // A struct: a pattern with the names of the fields (no `Debug` and no `PartialEq` needed).
    let field = |name: &str, value: &str| (name.to_string(), Pat::Lit(value.into()));
    let (before, after) = expected(&verdict(&builds.dev.0, &builds.dev.1, "probe::shown"));
    assert_eq!((before, after), (RunExit::Returns(Pat::Struct { path: "probe::Shown".into(), fields: vec![field("a", "0")] }), RunExit::Returns(Pat::Struct { path: "probe::Shown".into(), fields: vec![field("a", "1")] })));
    let (before, after) = expected(&verdict(&builds.dev.0, &builds.dev.1, "probe::pad"));
    let RunExit::Returns(Pat::Struct { fields: old, .. }) = before else { panic!("{before:?}") };
    let RunExit::Returns(Pat::Struct { fields: new, .. }) = after else { panic!("{after:?}") };
    assert_eq!((old[1].clone(), new[1].clone()), (field("b", "0"), field("b", "1")));
}
