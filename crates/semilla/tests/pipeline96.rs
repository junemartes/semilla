//! Design 96: the step as one graph of tasks. The builds run while Semilla does the work that
//! does not read their results, and the output does not wait for a store write that it does not
//! read. Each test runs the `semilla` binary on a small crate with the timing lines on, and
//! reads the time line of the tasks (R96.7-1).

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

const MANIFEST: &str = "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

const OLD: &str = "pub fn size(a: u32) -> u32 {\n    a + 1\n}\n\npub fn twice(a: u32) -> u32 {\n    a * 2\n}\n";
const NEW: &str = "pub fn size(a: u32) -> u32 {\n    a + 2\n}\n\npub fn twice(a: u32) -> u32 {\n    a * 2\n}\n";
const NEXT: &str = "pub fn size(a: u32) -> u32 {\n    a + 2\n}\n\npub fn twice(a: u32) -> u32 {\n    let b = a;\n    b * 2\n}\n";

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

/// The cache folder of the tests of a repository.
fn cache(repo: &Path) -> PathBuf {
    repo.parent().unwrap().join("cache")
}

/// Runs `semilla ARGS` with the timing lines. Gives the exit code, stdout, and stderr.
fn semilla(repo: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo).args(args);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver")).env("SEMILLA_CACHE_DIR", cache(repo)).env("SEMILLA_TIMING", "1");
    command.env_remove("SEMILLA_COMPILED").env_remove("SEMILLA_NO_CHECK").env_remove("SEMILLA_TEST_RULE_OFF");
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
    let output = command.output().unwrap();
    (output.status.code(), String::from_utf8_lossy(&output.stdout).into_owned(), String::from_utf8_lossy(&output.stderr).into_owned())
}

/// The runs of the tasks of the time line (R96.7-1): the name, the start, and the end.
fn runs(stderr: &str) -> Vec<(String, f64, f64)> {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[timing] task "))
        .filter_map(|rest| {
            let (name, times) = rest.rsplit_once(": from ")?;
            let (start, end) = times.strip_suffix(" s")?.split_once(" s to ")?;
            Some((name.to_string(), start.parse().ok()?, end.parse().ok()?))
        })
        .collect()
}

/// The runs of one task.
fn task<'a>(all: &'a [(String, f64, f64)], name: &str) -> Vec<&'a (String, f64, f64)> {
    all.iter().filter(|(found, _, _)| found == name).collect()
}

/// A count of a timing line of a phase, or zero.
fn count(stderr: &str, phase: &str, name: &str) -> u64 {
    let prefixes = [format!("[timing] total of {phase} ("), format!("[timing] count of {phase} (")];
    let Some(inner) = stderr.lines().find_map(|line| prefixes.iter().find_map(|prefix| line.strip_prefix(prefix.as_str()))) else { return 0 };
    let inner = inner.rsplit_once(')').map(|(inner, _)| inner).unwrap_or(inner);
    inner.split(", ").find_map(|part| part.strip_suffix(name).and_then(|number| number.trim().parse().ok())).unwrap_or(0)
}

/// The files of the folders with the name `name` in the cache, to a depth of 4.
fn files_named(folder: &Path, name: &str, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if path.file_name().is_some_and(|found| found == name) {
            out.extend(std::fs::read_dir(&path).into_iter().flatten().flatten().map(|entry| entry.path()));
        } else if depth > 0 {
            files_named(&path, name, depth - 1, out);
        }
    }
}

// rule+ R96.1-1
// rule+ R96.2-1
// rule+ R96.3-1
// rule- R96.4-1
// rule+ R96.9-1
#[test]
fn a_step_after_a_step_runs_its_tasks_while_the_new_tree_builds() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, NEW);
    let (code, out, err) = semilla(&repo, &["step", "add one more"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    std::fs::write(repo.join("src/lib.rs"), NEXT).unwrap();
    let (code, out, err) = semilla(&repo, &["step", "add a local"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let all = runs(&err);
    // R96.1-1: the task of the builds starts before this thread loads the syntax trees.
    let builds = task(&all, "the builds of a step");
    assert_eq!(builds.len(), 1, "{err}");
    let loads = task(&all, "the load of a syntax tree");
    assert!(loads.iter().any(|(_, start, _)| *start >= builds[0].1), "{err}");
    // R96.2-1: the task of each unit starts at its message, while cargo builds.
    assert!(count(&err, "the tasks of the units at their messages (design 96)", "units done during the build") >= 1, "{err}");
    // R96.3-1: the opaque inputs come from the guess while the tree builds, and a run uses them.
    let guess = task(&all, "the guess of the opaque inputs");
    assert_eq!(guess.len(), 1, "{err}");
    assert!(guess[0].1 < task(&all, "a build")[0].2, "{err}");
    assert!(count(&err, "the construction of the opaque inputs", "reused") >= 1, "{err}");
    // R96.4-1: a step with no intent file makes no pair before its gate.
    assert!(task(&all, "the pair of the intent check").is_empty(), "{err}");
    // R96.9-1: the store write of the result of the change set comes after the output.
    assert_eq!(task(&all, "the store of the result of the change set").len(), 1, "{err}");
    let mut reports = Vec::new();
    files_named(&cache(&repo), "reports", 4, &mut reports);
    assert!(reports.iter().any(|file| file.file_name().is_some_and(|name| name == "session.json")), "{reports:?}");
}

// rule- R96.1-1
// rule- R96.3-1
#[test]
fn the_first_step_makes_no_guess_and_a_stored_step_starts_no_build() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, NEW);
    let (code, out, err) = semilla(&repo, &["step", "add one more"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    // R96.3-1: the tree before the step builds too, so no guess runs.
    assert!(task(&runs(&err), "the guess of the opaque inputs").is_empty(), "{err}");
    // R96.1-1: the same step again takes its result from the store: no task of the builds.
    let (code, _, _) = semilla(&repo, &["undo"]);
    assert_eq!(code, Some(0));
    let (code, out, err) = semilla(&repo, &["step", "add one more"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(task(&runs(&err), "the builds of a step").is_empty(), "{err}");
}

// rule+ R96.4-1
#[test]
fn a_step_with_an_intent_file_makes_its_pair_when_the_builds_end() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, NEW);
    let (code, out, err) = semilla(&repo, &["step", "add one more"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    std::fs::write(repo.join("src/lib.rs"), NEXT).unwrap();
    std::fs::create_dir_all(repo.join(".semilla/intent")).unwrap();
    std::fs::write(repo.join(".semilla/intent/pr.rs"), "use semilla_intent::prelude::*;\n\n#[same(depot::size)]\nfn size_same() {}\n").unwrap();
    let (code, out, err) = semilla(&repo, &["step", "add a local", "--intent", ".semilla/intent/pr.rs"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let all = runs(&err);
    let (builds, pair) = (task(&all, "the builds of a step"), task(&all, "the pair of the intent check"));
    // The store of the result of the change set checks the step again (its stored step has
    // the reads of the claims): that check has tasks with no work, after the first ones.
    assert!(!builds.is_empty() && !pair.is_empty(), "{err}");
    // The pair starts after the builds, before the end of the check of the step.
    assert!(pair[0].1 >= task(&all, "a build")[0].2 && pair[0].1 <= task(&all, "the check of the step")[0].2, "{err}");
}

// rule+ R96.8-1
#[test]
fn a_fast_step_runs_its_build_check_while_it_checks_the_step() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, NEW);
    let (code, out, err) = semilla(&repo, &["step", "add one more", "--fast"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let all = runs(&err);
    let check = task(&all, "the build check");
    let step = task(&all, "the check of the step");
    assert!(check.len() == 1 && step.len() == 1, "{err}");
    assert!(check[0].1 <= step[0].2, "{err}");
    // The result line has the time of the task, not "a stored result".
    assert!(out.lines().any(|line| line.starts_with("build check: passes (") && line.ends_with(" s)")), "{out}");
    let mut results = Vec::new();
    files_named(&cache(&repo), "results", 4, &mut results);
    assert_eq!(results.len(), 1, "{results:?}");
}

// rule- R96.8-1
#[test]
fn a_fast_step_that_stops_after_its_check_stores_no_build_check() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, NEW);
    let (code, out, err) = semilla(&repo, &["step", "add one more", "--fast", "--tag", "src/lib.rs:999=late"]);
    assert_ne!(code, Some(0), "{out}\n{err}");
    assert!(format!("{out}{err}").contains("is after the last line"), "{out}\n{err}");
    let mut results = Vec::new();
    files_named(&cache(&repo), "results", 4, &mut results);
    assert!(results.is_empty(), "{results:?}");
}

// rule- R96.9-1
#[test]
fn a_rejected_step_stores_no_result_of_its_change_set() {
    let dir = common::TestDir::new();
    let new = format!("{OLD}\n#[cfg(feature = \"x\")]\npub fn host() {{}}\n");
    let repo = repository(dir.path(), OLD, &new);
    let (code, out, err) = semilla(&repo, &["step", "add a host"]);
    assert_eq!(code, Some(1), "{out}\n{err}");
    assert!(out.starts_with("rejected"), "{out}");
    assert!(task(&runs(&err), "the store of the result of the change set").is_empty(), "{err}");
}
