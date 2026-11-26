//! The suggested test of each sure difference (design 75). The decision of the user
//! (2026-11-26): "I think the probe can suggest a test to write to detect the difference. It
//! should not manually do it." Semilla never builds or runs the code of the user in a step: each
//! test of this file makes a repository with the crate `depot`, runs `semilla step --compiled`,
//! and reads the suggested tests. The compile test (in the test suite of Semilla, on fixtures)
//! adds each suggested test to the old tree and to the new tree and runs `cargo test`: each test
//! passes on the old code and fails on the new code.

mod common;
mod suggested;

use suggested::{MANIFEST, Suggested, passing, suggested};

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

/// A command of Semilla in a repository with the variables of the tests.
fn semilla(dir: &Path, repo: &Path, args: &[&str]) -> String {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo).args(args);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"));
    command.env("SEMILLA_CACHE_DIR", dir.join("cache")).env("SEMILLA_NO_CHECK", "1").env_remove("SEMILLA_COMPILED").env_remove("SEMILLA_TEST_RULE_OFF");
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.code().is_some_and(|code| code <= 1), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    stdout
}

/// A repository with the old source committed and the new source in the working tree, and the
/// output of `semilla step --compiled` with more arguments.
fn step(dir: &Path, old: &str, new: &str, args: &[&str]) -> String {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("Cargo.toml"), MANIFEST).unwrap();
    std::fs::write(repo.join("src/lib.rs"), old).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "old"]);
    std::fs::write(repo.join("src/lib.rs"), new).unwrap();
    let mut all = vec!["step", "the change", "--compiled"];
    all.extend(args);
    semilla(dir, &repo, &all)
}

/// The line of the step section of a body.
fn body_line<'a>(output: &'a str, name: &str) -> &'a str {
    output.lines().find(|line| line.trim_start().starts_with(&format!("{name} ("))).unwrap_or_else(|| panic!("no line of {name}:\n{output}"))
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

pub fn push_two(v: &mut Vec<u32>, x: u32) { v.push(x); v.push(1); }

mod inner {
    pub(crate) struct Cell { pub(crate) level: u8 }

    fn deep(c: Cell) -> u8 { c.level.wrapping_mul(2) }

    pub fn outer(x: u8) -> u8 { deep(Cell { level: x }) }

    // tests of mod inner
}

mod tables {
    static TABLE: [u8; 4] = [1, 2, 3, 4];

    // tests of mod tables
}
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

pub fn push_two(v: &mut Vec<u32>, x: u32) { v.push(1); v.push(x); }

mod inner {
    pub(crate) struct Cell { pub(crate) level: u8 }

    fn deep(c: Cell) -> u8 { c.level.wrapping_mul(3) }

    pub fn outer(x: u8) -> u8 { deep(Cell { level: x }) }

    // tests of mod inner
}

mod tables {
    static TABLE: [u8; 4] = [1, 2, 9, 4];

    // tests of mod tables
}
"#;

fn main_output() -> &'static (String, common::TestDir) {
    static FOUND: std::sync::OnceLock<(String, common::TestDir)> = std::sync::OnceLock::new();
    FOUND.get_or_init(|| {
        let dir = common::TestDir::new();
        let output = step(dir.path(), OLD, NEW, &[]);
        (output, dir)
    })
}

// rule+ R75.1-1
#[test]
fn each_sure_difference_names_its_suggested_test_and_the_step_gives_its_text() {
    let (output, _) = main_output();
    for name in ["depot::add", "depot::shift", "depot::pick", "depot::bump", "depot::private_step", "depot::Hidden::get", "depot::push_two", "depot::inner::deep", "depot::tables::TABLE"] {
        let line = body_line(output, name);
        assert!(line.contains("suggested test `semilla_test_"), "{line}");
    }
    assert!(!output.contains("by a run") && !output.contains("the model and the run"), "{output}");
    let tests = suggested(output);
    let add = tests.iter().find(|test| test.function == "depot::add").unwrap_or_else(|| panic!("{output}"));
    assert!(add.text.contains("#[should_panic(expected = \"attempt to add with overflow\")]") && add.text.contains("crate::add("), "{}", add.text);
    // A private function and an input with a private field: the test is in the module of the
    // function, so it can call it and build the value.
    let get = tests.iter().find(|test| test.function == "depot::Hidden::get").unwrap();
    assert!(get.text.contains("let arg1 = crate::Hidden { v: 0 };") && get.text.contains("crate::Hidden::get(&arg1)"), "{}", get.text);
    // A static in a private module: the test reads its value.
    let table = tests.iter().find(|test| test.function == "depot::tables::TABLE").unwrap();
    assert!(table.text.contains("let got = &crate::tables::TABLE;") && table.text.contains("assert!(matches!(got[..], [1, 2, 3, 4]));"), "{}", table.text);
    let push = tests.iter().find(|test| test.function == "depot::push_two").unwrap();
    assert!(push.text.contains("assert!(matches!(v[..], ["), "{}", push.text);
}

// rule+ R75.1-1
// rule+ R75.1-3
#[test]
fn the_suggested_tests_pass_on_the_old_tree_and_fail_on_the_new_tree() {
    let (output, dir) = main_output();
    let mut tests = suggested(output);
    // [R75.1-3] A false claim gets the test of its counterexample.
    let claim_dir = common::TestDir::new();
    let intent = claim_dir.path().join("intent.rs");
    std::fs::write(&intent, "use semilla_intent::prelude::*;\n\n#[same(depot::half)]\nfn same_half(x: i32) -> bool {\n    before::depot::half(x) == after::depot::half(x)\n}\n").unwrap();
    let half_old = "pub fn half(x: i32) -> i32 {\n    x / 2\n}\n";
    let half_new = "pub fn half(x: i32) -> i32 {\n    x >> 1\n}\n";
    let claim_output = step(claim_dir.path(), half_old, half_new, &["--intent", &intent.display().to_string()]);
    let claim_line = claim_output.lines().find(|line| line.contains("✗ i1 ")).unwrap_or_else(|| panic!("{claim_output}"));
    assert!(claim_line.contains("false: for x = -1:") && claim_line.contains("suggested test `semilla_test_half_"), "{claim_line}");
    let claim_tests = suggested(&claim_output);
    assert!(claim_tests.iter().any(|test| test.text.contains("fn call_1()") && test.text.contains("crate::half(")), "{claim_output}");
    assert!(tests.len() >= 8, "{output}");
    let old = passing(dir.path(), "old", OLD, &tests);
    let new = passing(dir.path(), "new", NEW, &tests);
    for test in &tests {
        assert!(old.contains(&test.name), "the test {} fails on the old tree:\n{}", test.name, test.text);
        assert!(!new.contains(&test.name), "the test {} passes on the new tree:\n{}", test.name, test.text);
    }
    tests.clear();
    let old = passing(claim_dir.path(), "old", half_old, &claim_tests);
    let new = passing(claim_dir.path(), "new", half_new, &claim_tests);
    for test in &claim_tests {
        assert!(old.contains(&test.name) && !new.contains(&test.name), "{}", test.text);
    }
}

// rule+ R75.1-2
#[test]
fn a_difference_that_a_test_cannot_see_gets_no_test_with_the_cause() {
    let (output, _) = main_output();
    // Two panics: `#[should_panic(expected = "too big")]` takes a part of the message, and the
    // new message "x is too big" has it.
    let line = body_line(output, "depot::guard");
    assert!(line.contains("different (for x = 10: before panics") && line.contains("no suggested test: the two versions panic"), "{line}");
}

// rule- R75.1-2
#[test]
fn a_step_with_no_sure_difference_has_no_suggested_test() {
    let old = "pub fn twice(x: u32) -> u32 {\n    x.wrapping_mul(2)\n}\n";
    let new = "pub fn twice(x: u32) -> u32 {\n    x.wrapping_add(x)\n}\n";
    let dir = common::TestDir::new();
    let output = step(dir.path(), old, new, &[]);
    assert!(body_line(&output, "depot::twice").contains("equivalent"), "{output}");
    assert!(!output.contains("suggested test"), "{output}");
}

// rule- R75.1-3
#[test]
fn a_false_claim_with_no_call_of_a_version_has_no_test() {
    let old = "pub fn half(x: i32) -> i32 {\n    x / 2\n}\n";
    let new = "pub fn half(x: i32) -> i32 {\n    x >> 1\n}\n";
    let dir = common::TestDir::new();
    let intent = dir.path().join("intent.rs");
    std::fs::write(&intent, "use semilla_intent::prelude::*;\n\n#[same(depot::half)]\nfn never(x: i32) -> bool {\n    x != x\n}\n").unwrap();
    let output = step(dir.path(), old, new, &["--intent", &intent.display().to_string()]);
    let line = output.lines().find(|line| line.contains("✗ i1 ")).unwrap_or_else(|| panic!("{output}"));
    assert!(line.contains("false: ") && !line.contains("suggested test"), "{line}");
}

// rule+ R75.1-4
#[test]
fn the_text_of_a_test_is_the_same_in_each_view_and_in_each_run() {
    let (output, dir) = main_output();
    let repo = dir.path().join("repo");
    let verbose = semilla(dir.path(), &repo, &["status", "--verbose"]);
    let json = semilla(dir.path(), &repo, &["verify", "--json"]);
    let tests = suggested(output);
    let first = &tests[0];
    assert!(verbose.contains(first.text.lines().nth(1).unwrap()), "{verbose}");
    assert!(json.contains(&format!("\"name\": \"{}\"", first.name)) || json.contains(&format!("\"name\":\"{}\"", first.name)), "{json}");
    // A second step on the same change gives the same tests.
    let again = common::TestDir::new();
    let second = step(again.path(), OLD, NEW, &[]);
    let names = |found: &[Suggested]| found.iter().map(|test| (test.name.clone(), test.text.clone())).collect::<Vec<_>>();
    assert_eq!(names(&suggested(&second)), names(&tests));
}

// rule- R75.1-4
#[test]
fn a_step_does_not_build_or_run_a_probe() {
    let (_, dir) = main_output();
    // No folder of a probe in the cache.
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "probe") {
                    out.push(path.clone());
                }
                if depth > 0 {
                    walk(&path, depth - 1, out);
                }
            }
        }
    }
    let mut found = Vec::new();
    walk(&dir.path().join("cache"), 6, &mut found);
    assert!(found.is_empty(), "{found:?}");
}
