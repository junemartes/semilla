//! Design 99: the step as one pipeline, part 2. Each test runs the `semilla` binary on a small
//! crate with the timing lines on, and reads the time line of the tasks (R96.7-1).

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

const MANIFEST: &str = "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

const OLD: &str = "pub fn size(a: u32) -> u32 {\n    a + 1\n}\n\npub fn twice(a: u32) -> u32 {\n    a * 2\n}\n";
const NEW: &str = "pub fn size(a: u32) -> u32 {\n    a + 2\n}\n\npub fn twice(a: u32) -> u32 {\n    a * 2\n}\n";
const NEXT: &str = "pub fn size(a: u32) -> u32 {\n    a + 2\n}\n\npub fn twice(a: u32) -> u32 {\n    let b = a;\n    b * 2\n}\n";
/// `NEW` with a new value in a body: a node that no rule of Semilla maps, so a template.
const CHANGED: &str = "pub fn size(a: u32) -> u32 {\n    a + 2\n}\n\npub fn twice(a: u32) -> u32 {\n    a * 3\n}\n";
/// `NEW` with one more comment line in a body: the step has no node to map, so no template.
const COMMENT: &str = "pub fn size(a: u32) -> u32 {\n    a + 2\n}\n\npub fn twice(a: u32) -> u32 {\n    // the double\n    a * 2\n}\n";

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

/// The environment of each run of the binary.
fn environment(command: &mut Command, repo: &Path) {
    let root = workspace_root();
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver")).env("SEMILLA_CACHE_DIR", cache(repo)).env("SEMILLA_TIMING", "1");
    command.env_remove("SEMILLA_COMPILED").env_remove("SEMILLA_NO_CHECK").env_remove("SEMILLA_TEST_RULE_OFF");
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
}

/// Runs `semilla ARGS` with the timing lines. Gives the exit code, stdout, and stderr.
fn semilla(repo: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo).args(args);
    environment(&mut command, repo);
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

// rule+ R99.1-1
// rule+ R99.3-1
// rule+ R99.4-1
// rule+ R99.7-1
#[test]
fn a_step_after_a_step_writes_its_tree_one_time_and_reads_the_old_records_while_it_builds() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, NEW);
    let (code, out, err) = semilla(&repo, &["step", "add one more"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    std::fs::write(repo.join("src/lib.rs"), CHANGED).unwrap();
    let (code, out, err) = semilla(&repo, &["step", "three times"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let all = runs(&err);
    // R99.3-1: one write of the tree of the working files in the step.
    assert_eq!(task(&all, "the tree of the working files").len(), 1, "{err}");
    // R99.1-1: the records of the tree before the step are read while the new tree builds.
    let early = task(&all, "the read of the records before the step");
    let build = task(&all, "a build");
    assert!(early.len() == 1 && build.len() == 1, "{err}");
    assert!(early[0].1 < build[0].2, "{err}");
    // R99.4-1: the step has templates, so its gate runs its parts: the pair and the compile.
    assert_eq!(task(&all, "the compile of the intent crate").len(), 1, "{err}");
    assert_eq!(task(&all, "the builds of the pair of the intent check").len(), 1, "{err}");
    // R99.7-1: each part of the main chain is a task on the time line.
    for name in [
        "the start of the step command",
        "the build code test",
        "the end of a build",
        "read the records of a step",
        "the syntax diff of the step",
        "the compare of the bodies of the step",
        "the facts of the core check",
        "the gate of the claim templates",
        "the write of a step result",
        "the organization of a change set",
        "the end of the check of a change set",
        "the end of the step command",
        "the store of the result of the change set",
        "the review data of a change set",
        "the end of the command",
    ] {
        assert!(!task(&all, name).is_empty(), "no run of `{name}`:\n{err}");
    }
}

// rule- R99.1-1
#[test]
fn the_first_step_reads_no_records_before_its_builds() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, NEW);
    let (code, out, err) = semilla(&repo, &["step", "add one more"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    // The tree before the first step builds in the step: its folder is not complete at the
    // start, so the task reads nothing (and starts no build).
    let all = runs(&err);
    assert!(task(&all, "the read of the records before the step").is_empty(), "{err}");
    assert_eq!(task(&all, "a build").len(), 2, "{err}");
}

// rule- R99.4-1
// rule- R99.4-2
#[test]
fn a_step_with_no_template_compiles_no_intent_crate() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, NEW);
    let (code, out, err) = semilla(&repo, &["step", "add one more"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    std::fs::write(repo.join("src/lib.rs"), COMMENT).unwrap();
    let (code, out, err) = semilla(&repo, &["step", "add a comment"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let all = runs(&err);
    assert!(!out.contains("#["), "the step has a template:\n{out}");
    for name in ["the builds of the pair of the intent check", "the second build of the old tree", "the facades of the intent check", "the compile of the intent crate", "the early compile of the intent files"] {
        assert!(task(&all, name).is_empty(), "a run of `{name}`:\n{err}");
    }
}

/// An intent file for the step from `NEW` to `NEXT`: the result of `twice` stays the same.
const INTENT: &str = "use semilla_intent::prelude::*;\n\nintent!(\"Add a local to `twice`\");\n\n#[same(depot::twice)]\nfn twice() {}\n";

// [R99.4-2] A step with an intent file compiles the intent files when the pair of the intent
// check ends, in the task of the builds: the AST pass reads that compile, so the step compiles
// the intent crate one time.
// rule+ R99.4-2
#[test]
fn a_step_with_an_intent_file_compiles_it_at_the_end_of_the_pair() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, NEW);
    let (code, out, err) = semilla(&repo, &["step", "add one more"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    std::fs::write(repo.join("src/lib.rs"), NEXT).unwrap();
    std::fs::create_dir_all(repo.join(".semilla/intent")).unwrap();
    std::fs::write(repo.join(".semilla/intent/step.rs"), INTENT).unwrap();
    let (code, out, err) = semilla(&repo, &["step", "add a local", "--intent", ".semilla/intent/step.rs"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let all = runs(&err);
    // The runs of the check of the step (the store of the result of the change set after the
    // output has a check of its own, as before 99).
    let end = task(&all, "the end of the step command")[0].1;
    let step: Vec<(String, f64, f64)> = all.iter().filter(|(_, start, _)| *start < end).cloned().collect();
    assert_eq!(task(&step, "the pair of the intent check").len(), 1, "{err}");
    assert_eq!(task(&step, "the early compile of the intent files").len(), 1, "{err}");
    assert_eq!(task(&step, "the compile of the intent crate").len(), 1, "{err}");
    let early = task(&step, "the early compile of the intent files")[0];
    let compile = task(&step, "the compile of the intent crate")[0];
    assert!(early.1 <= compile.1 && compile.2 <= early.2, "{err}");
}

/// Runs `semilla mcp` with the tool calls, and gives the text and the error state of each answer.
fn mcp_calls(repo: &Path, calls: &[(&str, serde_json::Value, Option<&str>)]) -> Vec<(String, bool)> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo).arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    environment(&mut command, repo);
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |message: serde_json::Value| -> serde_json::Value {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    ask(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}));
    let mut out = Vec::new();
    for (index, (name, arguments, file)) in calls.iter().enumerate() {
        // The text of `src/lib.rs` before the call: an edit between two calls of one server.
        if let Some(text) = file {
            std::fs::write(repo.join("src/lib.rs"), text).unwrap();
        }
        let answer = ask(serde_json::json!({"jsonrpc": "2.0", "id": index + 2, "method": "tools/call", "params": {"name": name, "arguments": arguments}}));
        out.push((answer["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string(), answer["result"]["isError"] == true));
    }
    drop(stdin);
    child.wait().unwrap();
    out
}

// rule- R99.3-1
#[test]
fn each_step_of_one_server_writes_its_own_tree() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), OLD, NEW);
    // One process with two steps: the tree of the first step is not the tree of the second one.
    let answers = mcp_calls(&repo, &[("step", serde_json::json!({"intent": "add one more"}), None), ("step", serde_json::json!({"intent": "add a local"}), Some(NEXT))]);
    assert!(!answers[0].1, "{}", answers[0].0);
    assert!(!answers[1].1, "{}", answers[1].0);
    assert!(!answers[1].0.contains("no change since the last step"), "{}", answers[1].0);
    assert!(answers[1].0.contains("step 1"), "{}", answers[1].0);
}
