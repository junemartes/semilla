//! A tag after the commit (design 62): `semilla tag <rev>`, `semilla tag A..B`, and
//! `semilla fact --commit`, with the real binary on small git repositories.
//!
//! The tests use the default mode (the facts of the syntax tree). The build check runs only in
//! the tests of the build check. The test of the configurations uses the compiled check and the
//! MIR driver of this workspace.

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn cache_dir(repo: &Path) -> PathBuf {
    repo.parent().unwrap().join("cache")
}

/// Runs semilla (one mode, design 83: the build, the MIR, and the build check). `env` adds
/// variables.
fn semilla_env(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo).args(args);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"));
    command.env("SEMILLA_CACHE_DIR", cache_dir(repo));
    command.envs(env.iter().copied());
    command.output().unwrap()
}

/// Runs semilla and returns stdout. Panics with the output if the exit code is not `expect`.
#[track_caller]
fn run_env(repo: &Path, args: &[&str], env: &[(&str, &str)], expect: i32) -> String {
    let output = semilla_env(repo, args, env);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(expect), "semilla {args:?}\n--- stdout\n{stdout}\n--- stderr\n{stderr}");
    stdout
}

#[track_caller]
fn run(repo: &Path, args: &[&str], expect: i32) -> String {
    run_env(repo, args, &[], expect)
}

/// The text of stderr of a run that must fail with the exit code 2.
#[track_caller]
fn fails(repo: &Path, args: &[&str]) -> String {
    let output = semilla_env(repo, args, &[]);
    assert_eq!(output.status.code(), Some(2), "semilla {args:?}: {}", String::from_utf8_lossy(&output.stdout));
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").current_dir(repo).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn write(repo: &Path, path: &str, text: &str) {
    let full = repo.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

fn commit_all(repo: &Path, message: &str) -> String {
    git(repo, &["add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

/// The note of a commit, as JSON.
fn note(repo: &Path, commit: &str) -> serde_json::Value {
    serde_json::from_str(&git(repo, &["notes", "--ref=semilla", "show", commit])).unwrap()
}

fn has_note(repo: &Path, commit: &str) -> bool {
    Command::new("git").current_dir(repo).args(["notes", "--ref=semilla", "show", commit]).output().unwrap().status.success()
}

const TOTAL_V0: &str = "pub fn total(items: &[u32]) -> u32 {\n    let mut s = 0;\n    for x in items {\n        s += x;\n    }\n    s\n}\n";
const TOTAL_V1: &str = "pub fn total(items: &[u32]) -> u32 {\n    let mut s = 0;\n    for x in items {\n        if *x > 2 {\n            s += x;\n        }\n    }\n    s\n}\n";
/// A new item with a `cfg` attribute: its fact is in the spotlight.
const HOST: &str = "\n#[cfg(unix)]\npub fn host() -> u32 {\n    1\n}\n";

/// A repository with one crate `shop` and one commit (the root commit). Returns the folder.
fn setup() -> common::TestDir {
    let dir = common::TestDir::new();
    let repo = &dir.path().join("repo");
    write(repo, "Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nnightly = []\n");
    write(repo, ".gitignore", "/target\n/.semilla/\n");
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V0}"));
    write(repo, "src/cart.rs", "pub fn price(n: u32) -> u32 {\n    n * 2\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 1\n}\n");
    git(repo, &["init", "-q", "-b", "main"]);
    commit_all(repo, "base");
    dir
}

/// The fact lines of an output: the head line of each fact, the cover line, and the hunks line.
fn fact_lines(text: &str) -> Vec<String> {
    text.lines().filter(|line| line.starts_with("  ◆ ") || line.starts_with("  cover:") || line.starts_with("  hunks:")).map(str::to_string).collect()
}

// rule+ R62.1-1
// rule+ R62.5-1
// rule+ R62.5-4
// rule+ R62.5-5
// rule+ R62.6-1
// rule- R62.6-1
// rule- R62.5-4
// rule- R62.5-5
#[test]
fn one_commit_gets_one_step_and_the_reasons_come_later() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = git(repo, &["rev-parse", "HEAD"]);
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V1}{HOST}"));
    let commit = commit_all(repo, "skip small items");
    let short = git(repo, &["rev-parse", "--short", "HEAD"]);
    // The commit has no change set of the agent: one step from the first parent.
    let out = run(repo, &["tag"], 1);
    let first = out.lines().next().unwrap();
    assert_eq!(first, "result: tagged after the commit · 1 step · 4 facts: 4 code different (1 with a spotlight) · 1 open spotlight", "{out}");
    assert!(out.contains(&format!("tagged {commit} after the commit: one step from the first parent {} to the commit \"skip small items\"", &base[..short.len()])), "{out}");
    assert!(out.contains("! spotlight: a new item with an attribute that can change the behavior: `cfg` on `shop::host`; open: no reason given"), "{out}");
    // The last lines: the exact command that adds the reason.
    let command = format!("  semilla fact --commit {short} --reason 'shop::host=<why>'");
    assert!(out.trim_end().ends_with(&command), "{out}");
    // The note has one step, from the tree of the parent to the tree of the commit.
    let set = note(repo, &commit);
    assert_eq!(set["steps"].as_array().unwrap().len(), 1);
    assert_eq!(set["steps"][0]["tree_before"], git(repo, &["rev-parse", "HEAD^^{tree}"]));
    assert_eq!(set["steps"][0]["tree_after"], git(repo, &["rev-parse", "HEAD^{tree}"]));
    assert_eq!(set["steps"][0]["intent"], "skip small items");
    assert!(set["after_commit"].is_object(), "{set}");
    // `status` says "not complete", and names the open item.
    let status = run(repo, &["status", "HEAD"], 1);
    assert!(status.contains("tagged after the commit (no steps of the agent)") && status.contains("result: not complete · tagged after the commit · 1 step · ") && status.contains("· 1 open spotlight"), "{status}");
    // A reason that belongs to no fact stores nothing; a false fact stores nothing.
    let before = note(repo, &commit);
    let out = run(repo, &["fact", "--commit", &short, "--reason", "shop::nothing=a reason"], 1);
    assert!(out.starts_with(&format!("rejected: nothing is added to the note of commit {short}")) && out.contains("the keys of the open spotlights: `shop::host`"), "{out}");
    let out = run(repo, &["fact", "--commit", &short, "--change", "shop::cart::price=the price changes"], 1);
    assert!(out.contains("rejected: nothing is added") && out.contains("is false"), "{out}");
    assert_eq!(note(repo, &commit), before);
    // The command of the tag output adds the reason. Then no item is open.
    let out = run(repo, &["fact", "--commit", &short, "--reason", "shop::host=only unix hosts run the shop", "--change", "shop::total=small items do not count"], 0);
    assert!(out.contains("1 new fact(s) of the agent, 0 new tag(s), and 1 new reason(s)") && out.contains("tagged after the commit: no open item"), "{out}");
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains("result: accepted · tagged after the commit · 1 step · ") && !status.contains("open spotlight"), "{status}");
    // Rule R62.5-4, the negative case: a commit with no open item has no item in the line, and
    // the exit code 0.
    write(repo, "src/cart.rs", "pub fn price(n: u32) -> u32 {\n    n * 3\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 1\n}\n");
    commit_all(repo, "a new price");
    let out = run(repo, &["tag"], 0);
    assert!(out.lines().next().unwrap().starts_with("result: tagged after the commit · 1 step · 1 fact: "), "{out}");
    assert!(!out.contains("--reason") && !out.contains("open spotlight"), "{out}");
}

// rule- R62.1-1
// rule+ R62.1-2
// rule- R62.1-2
// rule+ R62.1-3
// rule- R62.1-3
#[test]
fn a_root_commit_a_merge_commit_and_an_empty_commit() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let root = git(repo, &["rev-parse", "HEAD"]);
    // The root commit compares with the empty tree: each file is new.
    let out = run(repo, &["tag", &root], 1);
    assert!(out.contains("one step from the empty tree (a root commit) to the commit \"base\""), "{out}");
    let set = note(repo, &root);
    assert_eq!(set["steps"][0]["tree_before"], "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    assert!(out.contains("added `shop::total`") && out.contains("added `shop::cart`"), "{out}");
    // A merge commit compares with its first parent.
    git(repo, &["checkout", "-q", "-b", "side"]);
    write(repo, "src/cart.rs", "pub fn price(n: u32) -> u32 {\n    n * 2\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 5\n}\n");
    commit_all(repo, "a new fee");
    git(repo, &["checkout", "-q", "main"]);
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V1}"));
    let main = commit_all(repo, "skip small items");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "merge", "-q", "--no-ff", "-m", "merge the fee", "side"]);
    let merge = git(repo, &["rev-parse", "HEAD"]);
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("a merge commit: compared with the first parent"), "{out}");
    // The step has the change of the side branch only: the change of the first parent is in the base.
    assert!(out.contains("shop::cart::fee") && !out.contains("shop::total"), "{out}");
    let set = note(repo, &merge);
    assert_eq!(set["base_tree"], git(repo, &["rev-parse", &format!("{main}^{{tree}}")]));
    assert_eq!(set["after_commit"]["merge"], true);
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains("tagged after the commit (no steps of the agent)\na merge commit: compared with the first parent"), "{status}");
    // The negative cases: a commit with one parent has no merge line and does not compare with
    // the empty tree; a commit that changes no file gets no note.
    let out = run(repo, &["tag", &main], 0);
    assert!(!out.contains("a merge commit") && !out.contains("empty tree"), "{out}");
    let empty = commit_all(repo, "nothing");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("changes no file (its tree is the tree of its first parent): no note"), "{out}");
    assert!(!has_note(repo, &empty));
}

// rule+ R62.2-1
// rule- R62.2-1
// rule+ R62.2-2
#[test]
fn a_range_tags_each_commit_in_order_and_makes_each_tree_one_time() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let start = git(repo, &["rev-parse", "HEAD"]);
    // A new file.
    write(repo, "src/stock.rs", "pub fn count(n: u32) -> u32 {\n    n\n}\n");
    write(repo, "src/lib.rs", &format!("pub mod cart;\npub mod stock;\n\n{TOTAL_V0}"));
    let one = commit_all(repo, "add the stock module");
    // A renamed function.
    write(repo, "src/cart.rs", "pub fn unit_price(n: u32) -> u32 {\n    n * 2\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 1\n}\n");
    let two = commit_all(repo, "rename price");
    // A moved function.
    write(repo, "src/cart.rs", "pub fn unit_price(n: u32) -> u32 {\n    n * 2\n}\n");
    write(repo, "src/stock.rs", "pub fn count(n: u32) -> u32 {\n    n\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 1\n}\n");
    let three = commit_all(repo, "move fee to stock");
    let out = run(repo, &["tag", &format!("{start}..HEAD")], 0);
    assert!(out.starts_with("result: tagged after the commit · 3 commits · 3 steps · "), "{out}");
    let at = |commit: &str| out.find(&format!("tagged {commit} after the commit")).unwrap_or_else(|| panic!("{commit}\n{out}"));
    assert!(at(&one) < at(&two) && at(&two) < at(&three), "{out}");
    assert!(out.contains("added `shop::stock`"), "{out}");
    assert!(out.contains("renamed `shop::cart::price` → `shop::cart::unit_price`"), "{out}");
    assert!(out.contains("moved `shop::cart::fee` → `shop::stock::fee`"), "{out}");
    for commit in [&one, &two, &three] {
        assert_eq!(note(repo, commit)["steps"].as_array().unwrap().len(), 1);
    }
    assert!(!has_note(repo, &start), "the start of the range is not in the range");
    // Rule R62.2-2: 3 commits make 4 trees. Each tree of the range is in the cache one time.
    let snapshots: Vec<PathBuf> = std::fs::read_dir(cache_dir(repo)).unwrap().flatten().map(|entry| entry.path().join("snapshots")).filter(|path| path.is_dir()).collect();
    assert_eq!(snapshots.len(), 1, "{snapshots:?}");
    let count = std::fs::read_dir(&snapshots[0]).unwrap().flatten().filter(|entry| !entry.file_name().to_string_lossy().starts_with('.')).count();
    assert_eq!(count, 4);
    // A second range keeps each note.
    let out = run(repo, &["tag", &format!("{start}..{three}")], 0);
    assert_eq!(out.matches("the note stays").count(), 3, "{out}");
    // The negative case: a start that is not on the first-parent chain stores nothing.
    git(repo, &["checkout", "-q", "-b", "side", &start]);
    write(repo, "src/cart.rs", "pub fn price(n: u32) -> u32 {\n    n * 9\n}\n");
    let side = commit_all(repo, "side");
    let text = fails(repo, &["tag", &format!("{side}..main")]);
    assert!(text.contains("is not on the first-parent chain"), "{text}");
}

// rule+ R62.3-1
// rule+ R62.4-1
#[test]
fn the_facts_of_a_tag_are_the_facts_of_a_step_and_the_work_stays() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V1}{HOST}"));
    write(repo, "src/cart.rs", "pub fn unit_price(n: u32) -> u32 {\n    n * 2\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 2\n}\n");
    commit_all(repo, "a change");
    // A dirty working tree, an index with a change, and a session of the agent.
    write(repo, "src/cart.rs", "pub fn unit_price(n: u32) -> u32 {\n    n * 7\n}\n");
    write(repo, "notes.txt", "work in progress\n");
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V0}"));
    git(repo, &["add", "src/lib.rs"]);
    write(repo, ".semilla/session.json", "{\"changeset\": {\"base_tree\": \"\", \"head_tree\": \"\"}, \"open\": null}");
    let state = |repo: &Path| {
        let mut files = BTreeMap::new();
        for path in ["src/cart.rs", "src/lib.rs", "notes.txt", ".semilla/session.json"] {
            files.insert(path, std::fs::read_to_string(repo.join(path)).unwrap());
        }
        (git(repo, &["status", "--porcelain"]), git(repo, &["rev-parse", "HEAD"]), git(repo, &["symbolic-ref", "HEAD"]), git(repo, &["diff", "--cached"]), files)
    };
    let before = state(repo);
    let tagged = run(repo, &["tag", "HEAD"], 1);
    assert_eq!(state(repo), before, "the tag changes no part of the work");
    // `semilla step` on the same two trees, in a clone: the same fact lines.
    let clone = dir.path().join("clone");
    git(dir.path(), &["clone", "-q", "repo", "clone"]);
    git(&clone, &["reset", "-q", "--soft", "HEAD^"]);
    let stepped = run(&clone, &["step", "a change"], 1);
    let lines = fact_lines(&tagged);
    assert!(lines.len() >= 5, "{tagged}");
    assert_eq!(lines, fact_lines(&stepped), "tag:\n{tagged}\nstep:\n{stepped}");
}

// rule+ R62.5-2
// rule- R62.5-2
#[test]
fn a_build_that_fails_is_an_open_item_and_the_note_keeps_the_errors() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let check = [];
    write(repo, "src/cart.rs", "pub fn price(n: u32) -> u32 {\n    n * missing_rate()\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 1\n}\n");
    let broken = commit_all(repo, "use a rate");
    let out = run_env(repo, &["tag"], &check, 1);
    assert!(out.starts_with("result: tagged after the commit · 1 step · 1 fact: 1 no compare · 1 build check that fails · 1 error of the check"), "{out}");
    assert!(out.contains("✗ build check: fails") && out.contains("missing_rate"), "{out}");
    let set = note(repo, &broken);
    assert_eq!(set["steps"][0]["build_check"]["result"], "fails", "{set}");
    assert!(set["steps"][0]["build_check"]["errors"][0].as_str().unwrap().contains("missing_rate"), "{set}");
    let status = run(repo, &["status", &broken], 1);
    assert!(status.contains("! build check of step 0: fails") && status.contains("1 build check that fails"), "{status}");
    // The negative case: a commit that builds has the record `passes`.
    write(repo, "src/cart.rs", "pub fn price(n: u32) -> u32 {\n    n * 2\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 1\n}\n");
    let fixed = commit_all(repo, "no rate");
    // Design 83: the compiled check needs the tree of the parent, and it does not build. The
    // error of the check is an open item (R62.5-3); the build check passes.
    let out = run_env(repo, &["tag"], &check, 1);
    assert!(out.contains("build check: passes") && out.contains("1 error of the check") && !out.contains("build check that fails"), "{out}");
    assert_eq!(note(repo, &fixed)["steps"][0]["build_check"]["result"], "passes");
}

// rule+ R62.7-1
// rule- R62.7-1
// rule+ R62.7-2
// rule- R62.7-2
#[test]
fn a_commit_with_a_note_keeps_it_and_again_makes_it_again() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V1}{HOST}"));
    let commit = commit_all(repo, "skip small items");
    let short = git(repo, &["rev-parse", "--short", "HEAD"]);
    run(repo, &["tag"], 1);
    run(repo, &["fact", "--commit", &short, "--reason", "shop::host=only unix", "--change", "shop::total=small items do not count"], 0);
    let stored = note(repo, &commit);
    // A second tag keeps the note.
    let out = run(repo, &["tag"], 0);
    assert!(out.contains(&format!("commit {short} has a note of a tag after the commit; the note stays")), "{out}");
    assert_eq!(note(repo, &commit), stored);
    // `--again` makes the note again: the facts and the reasons of the agent stay.
    let out = run(repo, &["tag", "--again"], 0);
    assert!(out.starts_with("result: tagged after the commit · 1 step · ") && out.contains("✓ a1 change: `shop::total`"), "{out}");
    assert_eq!(note(repo, &commit)["steps"][0]["reasons"], stored["steps"][0]["reasons"]);
    // The negative case of R62.7-2: a note with the steps of the agent stays; `--again` refuses it.
    write(repo, "src/cart.rs", "pub fn price(n: u32) -> u32 {\n    n * 4\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 1\n}\n");
    run(repo, &["step", "a new price"], 0);
    let agent = commit_all(repo, "a new price");
    run(repo, &["tag"], 0);
    // A different clone has no session: the note of the commit is the only change set.
    std::fs::remove_file(repo.join(".semilla/session.json")).unwrap();
    let text = fails(repo, &["tag", &agent, "--again"]);
    assert!(text.contains("a note with 1 step(s) of the agent; `--again` makes only a note of a tag after the commit again"), "{text}");
    assert!(note(repo, &agent)["after_commit"].is_null());
    // The negative case of R62.7-1: a commit with no note gets one.
    write(repo, "src/cart.rs", "pub fn price(n: u32) -> u32 {\n    n * 5\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 1\n}\n");
    let next = commit_all(repo, "a newer price");
    assert!(!has_note(repo, &next));
    run(repo, &["tag", &next], 0);
    assert!(has_note(repo, &next));
}

// rule+ R62.8-1
// rule- R62.8-1
#[test]
fn a_commit_with_the_steps_of_the_agent_keeps_the_tag_of_today() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V1}"));
    run(repo, &["step", "skip small items"], 0);
    let commit = commit_all(repo, "skip small items");
    // The options of a tag after the commit are not for a change set of the agent.
    let text = fails(repo, &["tag", "--again"]);
    assert!(text.contains("`--again` is for a tag after the commit"), "{text}");
    let out = run(repo, &["tag"], 0);
    assert!(out.starts_with("result: accepted · 1 step · ") && out.contains(&format!("tagged {commit} with 1 steps; the change set is closed")), "{out}");
    let set = note(repo, &commit);
    assert!(set["after_commit"].is_null(), "{set}");
    // The step refs of today.
    assert_eq!(git(repo, &["rev-parse", &format!("refs/semilla/{commit}/step-0^{{tree}}")]), git(repo, &["rev-parse", "HEAD^{tree}"]));
    // The negative case: a new commit with no step gets a tag after the commit; the session
    // stays the closed change set of the first commit.
    let session = std::fs::read_to_string(repo.join(".semilla/session.json")).unwrap();
    write(repo, "src/cart.rs", "pub fn price(n: u32) -> u32 {\n    n * 4\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 1\n}\n");
    let next = commit_all(repo, "a new price");
    let out = run(repo, &["tag"], 0);
    assert!(out.starts_with("result: tagged after the commit · 1 step · ") && note(repo, &next)["after_commit"].is_object(), "{out}");
    assert_eq!(std::fs::read_to_string(repo.join(".semilla/session.json")).unwrap(), session);
    assert!(git(repo, &["for-each-ref", &format!("refs/semilla/{next}/")]).is_empty(), "a tag after the commit writes only the note");
}

// rule+ R62.10-1
// rule- R62.10-1
#[test]
fn the_review_and_load_show_a_tag_after_the_commit() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V1}"));
    let commit = commit_all(repo, "skip small items");
    run(repo, &["tag"], 0);
    let md = dir.path().join("review.md");
    let out = run(repo, &["review", &commit, "--md", md.to_str().unwrap()], 0);
    assert!(out.contains("result: accepted · tagged after the commit · 1 step"), "{out}");
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("> tagged after the commit (no steps of the agent)"), "{text}");
    let loaded = run(repo, &["load", &commit], 0);
    assert!(loaded.contains("tagged after the commit (no steps of the agent)"), "{loaded}");
    // The negative case: a change set of the agent has no such line.
    write(repo, "src/cart.rs", "pub fn price(n: u32) -> u32 {\n    n * 4\n}\n\npub fn fee(n: u32) -> u32 {\n    n + 1\n}\n");
    run(repo, &["step", "a new price"], 0);
    let agent = commit_all(repo, "a new price");
    run(repo, &["tag"], 0);
    let out = run(repo, &["review", &agent, "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(!text.contains("tagged after the commit") && !out.contains("tagged after the commit"), "{text}");
}

/// Talks to `semilla mcp`: the tool `tag` with a range, and the tool `fact` with a reason.
// rule+ R62.9-2
#[test]
fn the_mcp_tools_tag_and_fact_take_a_commit() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let start = git(repo, &["rev-parse", "HEAD"]);
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V1}{HOST}"));
    let commit = commit_all(repo, "skip small items");
    let mut child = Command::new(env!("CARGO_BIN_EXE_semilla"))
        .env("SEMILLA_CACHE_DIR", cache_dir(repo))
        .current_dir(repo)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |message: serde_json::Value| -> String {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        value["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string()
    };
    ask(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}));
    let out = ask(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "tag", "arguments": {"commit": format!("{start}..HEAD")}}}));
    assert!(out.starts_with("result: tagged after the commit · 1 commits · 1 step · ") || out.starts_with("result: tagged after the commit · 1 commit · 1 step · "), "{out}");
    assert!(out.contains("build check: passes"), "{out}");
    let out = ask(serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "fact", "arguments": {"commit": commit, "reasons": {"shop::host": "only unix hosts"}}}}));
    assert!(out.contains("1 new reason(s)") && out.contains("no open item"), "{out}");
    drop(stdin);
    let _ = child.wait();
    assert_eq!(note(repo, &commit)["steps"][0]["reasons"]["shop::host"], "only unix hosts");
}

// rule+ R62.5-3
// rule- R62.5-3
#[test]
fn a_configuration_that_does_not_build_does_not_stop_the_others() {
    // The MIR driver is a separate binary of the workspace.
    let status = Command::new("cargo").current_dir(workspace_root()).args(["build", "-q", "-p", "semilla-mir"]).status().unwrap();
    assert!(status.success(), "build the MIR driver");
    let dir = setup();
    let repo = &dir.path().join("repo");
    // The step adds code of the feature `nightly` that does not compile, and changes `total`.
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V1}\n#[cfg(feature = \"nightly\")]\npub fn fast() -> u32 {{\n    missing_fast()\n}}\n"));
    let commit = commit_all(repo, "a nightly path");
    let out = run(repo, &["tag"], 1);
    assert!(out.contains("configurations that do not build"), "{out}");
    assert!(out.contains("`dev`") && out.contains("`release`"), "{out}");
    assert!(out.contains("it does not build after the step"), "{out}");
    assert!(out.contains("code different"), "the configurations that build give their results:\n{out}");
    let set = note(repo, &commit);
    let failed = set["after_commit"]["failed_configs"].as_array().unwrap();
    assert!(failed.iter().any(|config| config["name"] == "dev, all features" && config["cause"].as_str().unwrap().contains("missing_fast")), "{set}");
    // The negative case: `semilla step --compiled` on the same trees stops at the first
    // configuration that does not compile (the rule of today).
    git(repo, &["reset", "-q", "--soft", "HEAD^"]);
    let out = semilla_env(repo, &["step", "a nightly path"], &[]);
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_ne!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("does not compile in the configuration"), "{text}");
}

// rule+ R68.10-1
// rule- R68.10-1
#[test]
fn a_range_continues_after_a_tool_error_and_the_commit_gets_a_note_with_the_error() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let start = git(repo, &["rev-parse", "HEAD"]);
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V1}"));
    let one = commit_all(repo, "filter the items");
    // A manifest that does not parse: the tag of this commit stops with a tool error.
    write(repo, "Cargo.toml", "[package\nname = \"shop\"\n");
    let broken = commit_all(repo, "break the manifest");
    write(repo, "Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nnightly = []\n");
    let fixed = commit_all(repo, "fix the manifest");
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V0}"));
    let last = commit_all(repo, "keep each item");
    // The exit code is 1: a note has an error.
    let out = run(repo, &["tag", &format!("{start}..HEAD")], 1);
    assert!(out.starts_with("result: tagged after the commit · 4 commits · "), "{out}");
    assert!(out.contains("errors of the check"), "{out}");
    for commit in [&broken, &fixed] {
        let short = &commit[..7];
        assert!(out.contains(&format!("commit {short}: a tool error stops the tag of this commit; the range continues (the note has the error;")), "{out}");
        let stored = note(repo, commit);
        assert!(stored["after_commit"]["tool_error"].as_str().is_some_and(|error| !error.is_empty()), "{stored}");
        assert!(stored["steps"].as_array().is_none_or(|steps| steps.is_empty()), "{stored}");
    }
    // The commits before and after the error get their steps.
    for commit in [&one, &last] {
        assert!(out.contains(&format!("tagged {commit} after the commit")), "{out}");
        let stored = note(repo, commit);
        assert_eq!(stored["steps"].as_array().unwrap().len(), 1);
        // The negative case: a commit with no tool error has no error in its note.
        assert!(stored["after_commit"]["tool_error"].is_null(), "{stored}");
    }
    // Each commit keeps its note. A kept note with an error stays an open item: exit code 1.
    let out = run(repo, &["tag", &format!("{start}..HEAD")], 1);
    assert_eq!(out.matches("the note stays").count(), 4, "{out}");
    assert_eq!(out.matches("has a note with a tool error").count(), 2, "{out}");
    // `--again` makes the note of the first commit again; it has no error.
    let out = run(repo, &["tag", &one, "--again"], 0);
    assert!(out.contains(&format!("tagged {one} after the commit")), "{out}");
}

// rule+ R68.11-1
// rule- R68.11-1
#[test]
fn a_reason_goes_to_a_stored_step_with_semilla_fact() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "src/lib.rs", &format!("pub mod cart;\n\n{TOTAL_V0}{HOST}"));
    // The step needs the reason of the spotlight: the step stores it.
    let out = run(repo, &["step", "add the host", "--reason", "shop::host=a first reason"], 0);
    assert!(out.starts_with("accepted: step 0"), "{out}");
    // A new text for the key of the spotlight replaces the reason of the stored step.
    let out = run(repo, &["fact", "--step", "0", "--reason", "shop::host=only unix hosts run the shop"], 0);
    assert!(out.contains("stored: 0 new fact(s) of the agent, 1 new reason(s) in step 0"), "{out}");
    let status = run(repo, &["status", "--verbose"], 0);
    assert!(status.contains("only unix hosts run the shop") && !status.contains("a first reason"), "{status}");
    // The negative case: a reason that belongs to no fact stores nothing.
    let out = run(repo, &["fact", "--step", "0", "--reason", "shop::nothing=a reason"], 1);
    assert!(out.starts_with("rejected: nothing is added to step 0") && out.contains("belongs to no fact of step 0"), "{out}");
    let status = run(repo, &["status", "--verbose"], 0);
    assert!(status.contains("only unix hosts run the shop") && !status.contains("shop::nothing"), "{status}");
}
