//! A run of the real compiled code confirms each sure difference (design 74, G5 part 4). Each
//! test makes a repository with the crate `depot`, runs `semilla step --compiled`, and reads the
//! run of each result: "confirmed by a run", "not confirmed by a run: CAUSE", or "not decided:
//! the model and the run disagree" (a model defect that a switch of the driver makes on
//! purpose).

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

/// A repository with the old source committed and the new source in the working tree, and the
/// output of `semilla step --compiled` with more arguments and variables.
fn step(dir: &Path, old: &str, new: &str, args: &[&str], env: &[(&str, &str)]) -> String {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    std::fs::write(repo.join("src/lib.rs"), old).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "old"]);
    std::fs::write(repo.join("src/lib.rs"), new).unwrap();
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(&repo).args(["step", "the change", "--compiled"]).args(args);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"));
    command.env("SEMILLA_CACHE_DIR", dir.join("cache")).env("SEMILLA_NO_CHECK", "1").env_remove("SEMILLA_COMPILED").env_remove("SEMILLA_TEST_RULE_OFF");
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
    command.envs(env.iter().copied());
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.code().is_some_and(|code| code <= 1), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    stdout
}

/// The line of the step section of a body.
fn body_line<'a>(output: &'a str, name: &str) -> &'a str {
    output.lines().find(|line| line.trim_start().starts_with(&format!("{name} ("))).unwrap_or_else(|| panic!("no line of {name}:\n{output}"))
}

/// The folder of the probes in a cache folder, if a probe was built.
fn probe_folders(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.file_name().is_some_and(|name| name == "probe") {
                out.push(path);
            } else if depth > 0 {
                walk(&path, depth - 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&dir.join("cache"), 6, &mut out);
    out
}

const OLD: &str = r#"
#[derive(Debug, Clone, Copy)]
pub struct Pt { pub x: i32, pub y: i32 }

pub struct Hidden { v: u32 }

impl Hidden {
    pub fn get(&self) -> u32 { self.v + 1 }
}

fn private_step(x: u32) -> u32 { x * 2 }

pub fn caller(x: u32) -> u32 { private_step(x) + 1 }

pub fn add(a: u32, b: u32) -> u32 { a + b }

pub fn shift(p: Pt) -> Pt { Pt { x: p.x + 1, y: p.y } }

pub fn bump(x: &mut u32) { *x += 1; }

pub fn pick(x: u8) -> Option<u8> { if x > 3 { Some(x) } else { None } }

pub fn guard(x: u32) -> u32 { assert!(x < 10, "too big"); x }
"#;

const NEW: &str = r#"
#[derive(Debug, Clone, Copy)]
pub struct Pt { pub x: i32, pub y: i32 }

pub struct Hidden { v: u32 }

impl Hidden {
    pub fn get(&self) -> u32 { self.v + 2 }
}

fn private_step(x: u32) -> u32 { x * 3 }

pub fn caller(x: u32) -> u32 { private_step(x) + 1 }

pub fn add(a: u32, b: u32) -> u32 { a.wrapping_add(b) }

pub fn shift(p: Pt) -> Pt { Pt { x: p.x + 2, y: p.y } }

pub fn bump(x: &mut u32) { *x += 2; }

pub fn pick(x: u8) -> Option<u8> { if x > 4 { Some(x) } else { None } }

pub fn guard(x: u32) -> u32 { assert!(x < 10, "x is too big"); x }
"#;

fn main_output() -> &'static (String, Vec<PathBuf>) {
    static FOUND: std::sync::OnceLock<(String, Vec<PathBuf>)> = std::sync::OnceLock::new();
    FOUND.get_or_init(|| {
        let dir = common::TestDir::new();
        let output = step(dir.path(), OLD, NEW, &[], &[]);
        (output, probe_folders(dir.path()))
    })
}

// rule+ R74.1-1
#[test]
fn a_run_of_the_real_code_confirms_each_sure_difference() {
    let (output, _) = main_output();
    // A value, a panic with the message of the compiler, a struct and an option with a derived
    // `Debug`, a write through `&mut`, and a literal panic message.
    for name in ["depot::add", "depot::shift", "depot::pick", "depot::bump", "depot::guard"] {
        let line = body_line(output, name);
        assert!(line.contains("confirmed by a run") && !line.contains("not confirmed by a run"), "{line}");
    }
    assert!(body_line(output, "depot::add").contains("before panics (overflow add), after returns"), "{output}");
}

// rule- R74.1-1
#[test]
fn a_call_that_the_probe_cannot_write_keeps_its_difference_with_the_cause() {
    let (output, _) = main_output();
    // The input has a private field: the compiler stops the call, the result stays different.
    let line = body_line(output, "depot::Hidden::get");
    assert!(line.contains("different (for self = ") && line.contains("not confirmed by a run: the compiler does not accept the call in the probe (E0451: field `v` of struct `Hidden` is private)"), "{line}");
}

// rule+ R74.1-3
#[test]
fn a_body_that_is_not_public_is_not_confirmed_and_names_its_confirmed_caller() {
    let (output, _) = main_output();
    let line = body_line(output, "depot::private_step");
    assert!(line.contains("not confirmed by a run: the compiler does not accept the call in the probe (E0603: function `private_step` is private)"), "{line}");
    assert!(line.contains("a run confirms the difference of the caller `depot::caller`"), "{line}");
}

// rule- R74.1-3
#[test]
fn a_public_caller_that_the_walk_reaches_gets_its_own_run() {
    let (output, _) = main_output();
    let line = output.lines().find(|line| line.contains("depot::caller (") && line.contains("different through depot::private_step")).unwrap_or_else(|| panic!("{output}"));
    assert!(line.contains("(confirmed by a run)"), "{line}");
}

// rule+ R74.1-2
#[test]
fn a_run_shows_a_model_defect_as_a_disagreement() {
    // The switch of the driver turns R69.1-1 off: it keeps each optional overflow check of an
    // inlined callee of `core` in `release`, as the driver before batch 11a did. The model then
    // gives the false difference of `i32::abs` (design 69.1), and the run catches it.
    let old = "pub fn magnitude(x: i32) -> i32 {\n    if x < 0 { -x } else { x }\n}\n";
    let new = "pub fn magnitude(x: i32) -> i32 {\n    x.abs()\n}\n";
    let dir = common::TestDir::new();
    let output = step(dir.path(), old, new, &[], &[("SEMILLA_TEST_RULE_OFF", "R69.1-1")]);
    let line = body_line(&output, "depot::magnitude");
    assert!(line.contains("not decided in release (the model and the run disagree (the run: before returns -2147483648, after returns -2147483648); a defect of Semilla)"), "{line}");
    assert!(output.contains("! the model and the run disagree in 1 check(s): a defect of Semilla (design 74)"), "{output}");
    // A run never makes a result equivalent.
    assert!(!line.contains("equivalent in release"), "{line}");
}

// rule- R74.1-2
#[test]
fn the_same_change_with_the_model_rule_is_equivalent_and_has_no_defect_line() {
    let old = "pub fn magnitude(x: i32) -> i32 {\n    if x < 0 { -x } else { x }\n}\n";
    let new = "pub fn magnitude(x: i32) -> i32 {\n    x.abs()\n}\n";
    let dir = common::TestDir::new();
    let output = step(dir.path(), old, new, &[], &[]);
    let line = body_line(&output, "depot::magnitude");
    assert!(line.contains("equivalent (proved by SMT"), "{line}");
    assert!(!output.contains("the model and the run disagree"), "{output}");
}

// rule+ R74.1-4
#[test]
fn a_step_with_no_sure_difference_starts_no_probe() {
    let old = "pub fn twice(x: u32) -> u32 {\n    x.wrapping_mul(2)\n}\n";
    let new = "pub fn twice(x: u32) -> u32 {\n    x.wrapping_add(x)\n}\n";
    let dir = common::TestDir::new();
    let output = step(dir.path(), old, new, &[], &[]);
    assert!(body_line(&output, "depot::twice").contains("equivalent"), "{output}");
    assert!(probe_folders(dir.path()).is_empty(), "{:?}", probe_folders(dir.path()));
}

// rule- R74.1-4
#[test]
fn a_step_with_a_sure_difference_builds_a_probe() {
    let (_, folders) = main_output();
    assert!(!folders.is_empty());
}

// rule+ R74.1-5
#[test]
fn a_false_claim_gets_a_run_of_each_call_on_its_failed_path() {
    let old = "pub fn half(x: i32) -> i32 {\n    x / 2\n}\n";
    let new = "pub fn half(x: i32) -> i32 {\n    x >> 1\n}\n";
    let dir = common::TestDir::new();
    let intent = dir.path().join("intent.rs");
    std::fs::write(&intent, "use semilla_intent::prelude::*;\n\n#[same(depot::half)]\nfn same_half(x: i32) -> bool {\n    before::depot::half(x) == after::depot::half(x)\n}\n\n#[same(depot::half)]\nfn empty() {}\n").unwrap();
    let output = step(dir.path(), old, new, &["--intent", &intent.display().to_string()], &[]);
    let lines: Vec<&str> = output.lines().filter(|line| line.contains("✗ i1 ") || line.contains("✗ i2 ")).collect();
    assert_eq!(lines.len(), 2, "{output}");
    for line in lines {
        assert!(line.contains("false: for x = -1:") && line.ends_with("(confirmed by a run)"), "{line}");
    }
}

// rule- R74.1-5
#[test]
fn a_false_claim_with_no_call_of_a_version_has_no_run() {
    let old = "pub fn half(x: i32) -> i32 {\n    x / 2\n}\n";
    let new = "pub fn half(x: i32) -> i32 {\n    x >> 1\n}\n";
    let dir = common::TestDir::new();
    let intent = dir.path().join("intent.rs");
    std::fs::write(&intent, "use semilla_intent::prelude::*;\n\n#[same(depot::half)]\nfn never(x: i32) -> bool {\n    x != x\n}\n").unwrap();
    let output = step(dir.path(), old, new, &["--intent", &intent.display().to_string()], &[]);
    let line = output.lines().find(|line| line.contains("✗ i1 ")).unwrap_or_else(|| panic!("{output}"));
    assert!(line.contains("false: ") && !line.contains("by a run"), "{line}");
}
