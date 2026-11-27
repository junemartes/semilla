//! Design 88: the store by content. A step after a step builds and parses nothing of the tree
//! of the step before (R88.5-2), and the timing lines show the new work of a command (R88.6-1).

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

const MANIFEST: &str = "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

/// A repository with `old` as the commit and `new` in the working tree.
fn repository(folder: &Path, old: &str, new: &str) -> PathBuf {
    let repo = folder.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("Cargo.toml"), MANIFEST).unwrap();
    std::fs::write(repo.join("src/lib.rs"), old).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "old"]);
    std::fs::write(repo.join("src/lib.rs"), new).unwrap();
    repo
}

/// Runs a step with the timing lines. Gives the exit code and stderr.
fn step(repo: &Path, title: &str) -> (Option<i32>, String) {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo).args(["step", title]);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver")).env("SEMILLA_CACHE_DIR", repo.parent().unwrap().join("cache")).env("SEMILLA_TIMING", "1");
    command.env_remove("SEMILLA_COMPILED").env_remove("SEMILLA_NO_CHECK").env_remove("SEMILLA_TEST_RULE_OFF");
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
    let output = command.output().unwrap();
    (output.status.code(), String::from_utf8_lossy(&output.stderr).into_owned())
}

/// The counts of the new work of a command (R88.6-1), by name.
fn work(stderr: &str) -> std::collections::BTreeMap<String, u64> {
    let line = stderr.lines().find(|line| line.starts_with("[timing] count of the new work of the command (")).unwrap_or_else(|| panic!("no line of the new work:\n{stderr}"));
    let inner = line.trim_start_matches("[timing] count of the new work of the command (").trim_end_matches(')');
    inner.split(", ").map(|part| {
        let (count, name) = part.split_once(' ').unwrap();
        (name.to_string(), count.parse().unwrap())
    }).collect()
}

/// The tree of the working folder (a commit of a copy of the index, with no change of the
/// repository).
fn tree_of_working_folder(repo: &Path) -> String {
    let index = repo.join(".git/semilla-test-index");
    let _ = std::fs::copy(repo.join(".git/index"), &index);
    let run = |args: &[&str]| {
        let output = Command::new("git").current_dir(repo).env("GIT_INDEX_FILE", &index).args(args).output().unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    run(&["add", "-A"]);
    let tree = run(&["write-tree"]);
    let _ = std::fs::remove_file(&index);
    tree
}

const OLD: &str = "pub fn limit() -> u32 {\n    10\n}\n\npub fn kept(a: u32) -> u32 {\n    a + 1\n}\n";
const ONE: &str = "pub fn limit() -> u32 {\n    11\n}\n\npub fn kept(a: u32) -> u32 {\n    a + 1\n}\n";
const TWO: &str = "pub fn limit() -> u32 {\n    12\n}\n\npub fn kept(a: u32) -> u32 {\n    a + 1\n}\n";

// [R88.5-2] The new tree of step N is the old tree of step N+1: step N+1 builds nothing of it,
// parses no file of it, and runs no cargo command for it. Only the files of the new tree are
// parsed.
// rule+ R88.5-2
#[test]
fn a_step_after_a_step_builds_and_parses_nothing_of_the_tree_before() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let middle = tree_of_working_folder(&repo);
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    std::fs::write(repo.join("src/lib.rs"), TWO).unwrap();
    let last = tree_of_working_folder(&repo);
    let (code, second) = step(&repo, "two");
    assert_eq!(code, Some(0), "{second}");
    // No build of the tree of step 0, and a build of the new tree.
    assert!(!second.lines().any(|line| line.contains(&format!("of tree {}", &middle[..8]))), "a build of the tree of the step before:\n{second}");
    assert!(second.lines().any(|line| line.starts_with("[timing] cargo check") && line.contains(&format!("of tree {}", &last[..8]))), "no build of the new tree:\n{second}");
    let counts = work(&second);
    // The one file of the new tree is parsed; the tree before comes from the store.
    assert_eq!(counts["files parsed"], 1, "{second}");
    assert!(counts["files from the store"] >= 1, "{second}");
    assert!(second.contains("load the syntax workspace (from the store)"), "{second}");
}

// rule- R88.5-2
#[test]
fn the_first_step_parses_and_builds_each_tree() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    let counts = work(&first);
    // Two trees: each one is written, parsed, and built.
    assert_eq!(counts["files parsed"], 2, "{first}");
    assert_eq!(counts["files from the store"], 0, "{first}");
    assert!(counts["trees written"] >= 2 && counts["cargo commands run"] >= 2, "{first}");
}

// [R88.6-1] The timing lines count the new work of a command: a warm step (the same step
// again, with the cache of the first) writes no tree, parses no file, runs no cargo command,
// and takes the equal bodies from the index.
// rule+ R88.6-1
#[test]
fn a_warm_step_has_no_new_work_in_its_timing_lines() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    // The same step again: the session of the repository goes away, the cache stays.
    std::fs::remove_dir_all(repo.join(".semilla")).unwrap();
    let (code, warm) = step(&repo, "one");
    assert_eq!(code, Some(0), "{warm}");
    let counts = work(&warm);
    assert_eq!((counts["trees written"], counts["files parsed"], counts["cargo commands run"]), (0, 0, 0), "{warm}");
    assert!(counts["bodies from the index"] >= 1 && counts["bodies decoded"] >= 1 && counts["files from the store"] == 2, "{warm}");
}

// rule- R88.6-1
#[test]
fn a_step_with_a_new_tree_counts_its_new_work() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    std::fs::remove_dir_all(repo.join(".semilla")).unwrap();
    // Another change in the place of the first one: the tree after the step is new.
    std::fs::write(repo.join("src/lib.rs"), TWO).unwrap();
    let (code, other) = step(&repo, "two");
    assert_eq!(code, Some(0), "{other}");
    let counts = work(&other);
    assert_eq!(counts["files parsed"], 1, "{other}");
    assert!(counts["trees written"] >= 1 && counts["cargo commands run"] >= 1, "{other}");
}

// [R89.5-1] Design 89: the timing line of the new work counts each stored kind. A warm step
// (the same step again, with the cache of the first) takes each construction of the opaque
// inputs from the store and computes none.
// rule+ R89.5-1
#[test]
fn a_warm_step_takes_the_opaque_inputs_from_the_store() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    std::fs::remove_dir_all(repo.join(".semilla")).unwrap();
    let (code, warm) = step(&repo, "one");
    assert_eq!(code, Some(0), "{warm}");
    let counts = work(&warm);
    assert!(counts["opaque inputs from the store"] >= 1, "{warm}");
    assert_eq!(counts["opaque inputs computed"], 0, "{warm}");
}

// rule- R89.5-1
#[test]
fn the_first_step_computes_the_opaque_inputs() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    let counts = work(&first);
    assert!(counts["opaque inputs computed"] >= 1, "{first}");
    assert_eq!(counts["opaque inputs from the store"], 0, "{first}");
}
