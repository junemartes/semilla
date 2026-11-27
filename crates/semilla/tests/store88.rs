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
    // [R93.1-2] Another intent text: the result of the step is not in the store, so the check
    // runs, with the stored parts of 14f and 14g.
    let (code, warm) = step(&repo, "one, again");
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
    // [R93.1-2] Another intent text: the result of the step is not in the store, so the check
    // runs, with the stored parts of 14f and 14g.
    let (code, warm) = step(&repo, "one, again");
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

/// A repository with two files: `old` as the commit, and `new` in the working tree.
fn two_files(folder: &Path, old: (&str, &str), new: (&str, &str)) -> PathBuf {
    let repo = folder.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("Cargo.toml"), MANIFEST).unwrap();
    std::fs::write(repo.join("src/lib.rs"), format!("pub mod other;\n{}", old.0)).unwrap();
    std::fs::write(repo.join("src/other.rs"), old.1).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "old"]);
    std::fs::write(repo.join("src/lib.rs"), format!("pub mod other;\n{}", new.0)).unwrap();
    std::fs::write(repo.join("src/other.rs"), new.1).unwrap();
    repo
}

const OTHER: &str = "pub fn other(a: u32) -> u32 {\n    a * 2\n}\n";
const OTHER_TWO: &str = "pub fn other(a: u32) -> u32 {\n    a * 3\n}\n";

// [R91.6-1] Design 91: a step after a step parses only the files of the new tree with a new
// content; each other file of the new tree comes from the store (the part of each file,
// R91.2-1). The counts of the line of the new work show it.
// rule+ R91.6-1
#[test]
fn a_new_tree_takes_the_files_that_do_not_change_from_the_store() {
    let dir = common::TestDir::new();
    let repo = two_files(dir.path(), (OLD, OTHER), (ONE, OTHER));
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    std::fs::write(repo.join("src/lib.rs"), format!("pub mod other;\n{TWO}")).unwrap();
    let (code, second) = step(&repo, "two");
    assert_eq!(code, Some(0), "{second}");
    let counts = work(&second);
    // `src/other.rs` of the new tree comes from the store; `src/lib.rs` is new.
    assert_eq!(counts["files parsed"], 1, "{second}");
    assert!(counts["files from the store"] >= 3, "{second}");
}

// rule- R91.6-1
#[test]
fn a_new_tree_parses_each_file_with_a_new_content() {
    let dir = common::TestDir::new();
    let repo = two_files(dir.path(), (OLD, OTHER), (ONE, OTHER));
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    std::fs::write(repo.join("src/lib.rs"), format!("pub mod other;\n{TWO}")).unwrap();
    std::fs::write(repo.join("src/other.rs"), OTHER_TWO).unwrap();
    let (code, second) = step(&repo, "two");
    assert_eq!(code, Some(0), "{second}");
    assert_eq!(work(&second)["files parsed"], 2, "{second}");
}

// [R91.5-1] Design 91: with the timing lines, the driver writes a timing line for each member
// crate that it compiles, and the step prints the split of its build.
// rule+ R91.5-1
#[test]
fn a_step_with_the_timing_lines_has_the_split_of_its_build() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    let line = first.lines().find(|line| line.starts_with("[timing] the split of cargo check n0 of tree")).unwrap_or_else(|| panic!("no split line:\n{first}"));
    // The library and its test target: two compiles of the member crate.
    let crates: u64 = line.split("the compiles of ").nth(1).and_then(|rest| rest.split(' ').next()).and_then(|count| count.parse().ok()).unwrap_or(0);
    assert!(crates >= 1 && line.contains("rustc before the driver") && line.contains("the dependencies and cargo"), "{line}");
    assert!(first.contains("[timing] total of the builds: the driver (the sum over the member crates)"), "{first}");
}

// rule- R91.5-1
#[test]
fn a_step_with_no_timing_lines_gives_the_driver_no_file() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let root = workspace_root();
    let output = Command::new(env!("CARGO_BIN_EXE_semilla"))
        .current_dir(&repo)
        .args(["step", "one"])
        .env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"))
        .env("SEMILLA_CACHE_DIR", dir.path().join("cache"))
        .env_remove("SEMILLA_TIMING")
        .env_remove("SEMILLA_COMPILED")
        .env_remove("SEMILLA_NO_CHECK")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("the split of"), "{stderr}");
    fn any_timing_file(dir: &Path) -> bool {
        std::fs::read_dir(dir).into_iter().flatten().flatten().any(|entry| {
            let path = entry.path();
            entry.file_name().to_string_lossy().starts_with("semilla-driver-timing") || (path.is_dir() && !path.is_symlink() && any_timing_file(&path))
        })
    }
    assert!(!any_timing_file(&dir.path().join("cache")), "a timing file of the driver with no timing lines");
}

/// The counts of a timing line of a phase: `[timing] total of PHASE (N times, COUNT NAME, ...)`.
fn phase_counts(stderr: &str, phase: &str) -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    for line in stderr.lines().filter_map(|line| line.strip_prefix(&format!("[timing] total of {phase} ("))) {
        let inner = line.split("): ").next().unwrap_or_default();
        for part in inner.split(", ").skip(1) {
            if let Some((count, name)) = part.split_once(' ') {
                *out.entry(name.to_string()).or_insert(0) += count.parse::<u64>().unwrap_or(0);
            }
        }
    }
    out
}

// [R93.1-2] A step after a step takes the result of the check of the step before from the
// store: the session result of the two steps checks only the new step. The timing lines count
// the stored step results (R93.6-1).
// rule+ R93.1-2
// rule+ R93.6-1
#[test]
fn a_step_after_a_step_takes_the_result_of_the_step_before_from_the_store() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    std::fs::write(repo.join("src/lib.rs"), TWO).unwrap();
    let (code, second) = step(&repo, "two");
    assert_eq!(code, Some(0), "{second}");
    let counts = phase_counts(&second, "the stored step results (design 93)");
    assert_eq!(counts.get("step results from the store"), Some(&1), "{second}");
    assert_eq!(counts.get("step results computed"), Some(&1), "{second}");
}

// rule- R93.1-2
// rule- R93.6-1
#[test]
fn the_first_step_computes_its_result() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    let counts = phase_counts(&first, "the stored step results (design 93)");
    assert_eq!(counts.get("step results from the store"), None, "{first}");
    assert_eq!(counts.get("step results computed"), Some(&1), "{first}");
}

// [R93.3-1] The driver asks the store before it prints: the build of the new tree of a step
// after a step takes the record of each body with a known key from the store, and writes only
// the records that are new. The timing lines count them (R93.6-1).
// rule+ R93.3-1
#[test]
fn a_step_after_a_step_writes_only_the_new_body_records() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, ONE);
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    std::fs::write(repo.join("src/lib.rs"), TWO).unwrap();
    let (code, second) = step(&repo, "two");
    assert_eq!(code, Some(0), "{second}");
    let counts = phase_counts(&second, "the body records of the store (design 93)");
    assert!(counts.get("records from the store").is_some_and(|count| *count >= 1), "{second}");
}

// rule- R93.3-1
#[test]
fn a_line_above_each_body_gives_each_body_a_new_record() {
    let dir = common::TestDir::new();
    // The new tree has a line above each body: each place changes, so no key is known.
    let repo = repository(dir.path(), OLD, &format!("// a new first line\n{OLD}"));
    let (code, first) = step(&repo, "one");
    assert_eq!(code, Some(0), "{first}");
    let counts = phase_counts(&first, "the body records of the store (design 93)");
    assert_eq!(counts.get("records from the store").copied().unwrap_or(0), 0, "{first}");
    assert!(counts.get("records written").is_some_and(|count| *count >= 2), "{first}");
}
