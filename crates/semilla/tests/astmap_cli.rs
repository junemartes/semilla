//! The AST pass of the intent language (design 79) with the compiled claims of rustc: the
//! anchors of the claims, the proof pass on mapped changes only, the templates of the nodes that
//! are not mapped, and the same data in the JSON.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn semilla_env(repo: &Path, args: &[&str]) -> Output {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo).args(args);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"));
    command.env("SEMILLA_CACHE_DIR", repo.parent().unwrap().join("cache"));
    command.output().unwrap()
}

#[track_caller]
fn run(repo: &Path, args: &[&str], expect: i32) -> String {
    let output = semilla_env(repo, args);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(expect), "semilla {args:?}\n--- stdout\n{stdout}\n--- stderr\n{stderr}");
    stdout
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(repo).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn write(repo: &Path, path: &str, text: &str) {
    let full = repo.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

const OLD: &str = "pub struct Cart {\n    pub items: Vec<u32>,\n}\n\npub fn total(c: &Cart) -> u32 {\n    c.items.iter().sum()\n}\n\npub fn f(x: u32) -> u32 {\n    x + 1\n}\n\npub fn caller(x: u32) -> u32 {\n    x * 3\n}\n\nfn helper_old(x: u32) -> u32 {\n    x + 5\n}\n\npub fn uses(x: u32) -> u32 {\n    helper_old(x)\n}\n\npub const LIMIT: u32 = 10;\n\npub fn limit() -> u32 {\n    LIMIT\n}\n";
const NEW: &str = "pub struct Cart {\n    pub entries: Vec<u32>,\n    pub len: usize,\n}\n\npub fn total(c: &Cart) -> u32 {\n    c.entries.iter().sum::<u32>() + c.len as u32\n}\n\npub fn f(x: u32) -> u32 {\n    let y = x;\n    y + 1\n}\n\npub fn caller(x: u32) -> u32 {\n    let _ = x;\n    let _ = x;\n    x * 3\n}\n\npub fn added(values: &[u32], scale: u8) -> u64 {\n    values.iter().map(|value| u64::from(*value) << scale).sum()\n}\n\npub fn uses(x: u32) -> u32 {\n    added(&[x], 0) as u32\n}\n\npub const LIMIT: u32 = 11;\n\npub const EXTRA: u32 = LIMIT + 1;\n\npub fn limit() -> u32 {\n    EXTRA\n}\n";

/// A repository with the old version committed and the new version in the working tree.
fn setup() -> (common::TestDir, PathBuf) {
    let dir = common::TestDir::new();
    let repo = dir.path().join("repo");
    write(&repo, "Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n");
    write(&repo, ".gitignore", "/target\n/.semilla/\n");
    write(&repo, "src/lib.rs", OLD);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
    write(&repo, "src/lib.rs", NEW);
    (dir, repo)
}

/// The claims: `f` through a relation function of the intent crate, and the relation of `Cart`.
const INTENT: &str = r#"use semilla_intent::prelude::*;

intent!("A new name of a field, a new helper");

fn f_relation(x: u32) -> bool {
    x == u32::MAX || before::shop::f(x) == after::shop::f(x)
}

#[same(shop::f)]
fn f_same(x: u32) -> bool {
    f_relation(x)
}

#[relation(before::shop::Cart, after::shop::Cart)]
fn cart(b: &before::shop::Cart, a: &after::shop::Cart) -> bool {
    a.entries == b.items && a.len == b.items.len()
}
"#;

/// The counts of the node lines of a full text by state (design 90: the coverage line has no
/// count of the claims): mapped by a claim, mapped by Semilla, not mapped.
fn counts(full: &str) -> [usize; 3] {
    let mut out = [0; 3];
    for line in full.lines().map(str::trim_start).filter(|line| line.starts_with('n') && line[1..].starts_with(|c: char| c.is_ascii_digit())) {
        let tail = line.split_once(' ').map_or("", |(_, tail)| tail);
        let at = if tail.starts_with("mapped by claim ") { 0 } else if tail.starts_with("mapped by Semilla ") { 1 } else if tail.starts_with("not mapped ") { 2 } else { continue };
        out[at] += 1;
    }
    out
}

/// The count J of the coverage line `changed nodes: N in M entries; K real edits (S stated by the
/// agent); J not mapped`.
fn not_mapped(text: &str) -> usize {
    let line = text.lines().find(|line| line.trim_start().starts_with("changed nodes: ")).unwrap_or_else(|| panic!("no coverage line:\n{text}"));
    line.trim_end().strip_suffix(" not mapped").and_then(|rest| rest.rsplit(' ').next()).and_then(|number| number.parse().ok()).unwrap_or_else(|| panic!("{line}"))
}

// rule+ R79.2-1
// rule+ R79.2-4
#[test]
fn the_compiled_claims_anchor_an_item_through_a_relation_function_and_the_fields_of_a_relation() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs"], 0);
    let full = run(&repo, &["expand", "--step", "0"], 0);
    // The claim calls `f` through `f_relation` (a function of the intent crate).
    assert!(full.contains("anchor: i2: same `shop::f` → `shop::f`"), "{full}");
    assert!(full.contains("anchor: i3: relation shop::Cart → shop::Cart; reads `shop::Cart.items` | `shop::Cart.entries`, `shop::Cart.len`"), "{full}");
    assert!(full.lines().any(|line| line.contains("mapped by claim i2 [R79.2-3]: added `let` `shop::f/S0`")), "{full}");
    assert!(full.lines().any(|line| line.contains("mapped by claim i3 [R79.2-4]: added field `shop::Cart.len`")), "{full}");
    assert!(counts(&full)[0] >= 4, "{full}");
}

// rule- R79.2-1
#[test]
fn a_step_with_no_intent_file_has_no_anchor_of_a_claim() {
    // Design 83: each step compiles its intent files. A step with no intent file has no
    // compiled claim, so no node is mapped by a claim; with the file, the claims map the nodes
    // with no `semilla intent check` before the step.
    let (_dir, repo) = setup();
    run(&repo, &["step", "the change"], 0);
    let full = run(&repo, &["expand", "--step", "0"], 0);
    assert_eq!(counts(&full)[0], 0, "{full}");
    run(&repo, &["undo"], 0);
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs"], 0);
    let full = run(&repo, &["expand", "--step", "0"], 0);
    assert!(counts(&full)[0] >= 4, "{full}");
}

// rule+ R79.7-1
#[test]
fn a_body_that_semilla_proves_equivalent_stays_equivalent_and_the_map_gives_its_nodes_a_kind() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    let text = run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs"], 0);
    // `caller` keeps its behavior (an equal MIR text, design 84): the map gives its two new
    // statements the kind "same code" (design 90), and the proof result stays.
    let line = text.lines().find(|line| line.contains("`shop::caller` (src/lib.rs:")).unwrap_or_else(|| panic!("{text}"));
    assert!(line.trim_start().starts_with("equal MIR: `shop::caller`"), "{line}");
    assert!(text.contains("changed bodies: 1 equal MIR, "), "{text}");
    let full = run(&repo, &["expand", "--step", "0"], 0);
    for at in ["shop::caller/S0", "shop::caller/S1"] {
        assert!(full.lines().any(|line| line.contains(&format!("`{at}`")) && line.contains(": same code in f")), "{at}\n{full}");
    }
}

// rule- R79.7-1
#[test]
fn a_node_that_is_not_mapped_changes_no_proof_result() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    let text = run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs"], 0);
    assert!(text.lines().any(|line| line.trim_start().starts_with("equal MIR: `shop::caller`")), "{text}");
    // Each request line has its template or its cause, and the step passes with it.
    let full = run(&repo, &["expand", "--step", "0"], 0);
    assert_eq!(counts(&full)[2], not_mapped(&text), "{full}");
    assert!(text.contains("accepted: step 0"), "{text}");
}

/// The templates of the lines that are not mapped in a full text.
fn templates(text: &str) -> Vec<String> {
    text.lines().filter_map(|line| line.trim_start().strip_prefix("template: ")).map(str::to_string).collect()
}

// rule+ R79.5-1
#[test]
fn each_template_of_a_node_that_is_not_mapped_passes_the_check_of_the_intent_files() {
    let (_dir, repo) = setup();
    run(&repo, &["step", "the change"], 0);
    let full = run(&repo, &["expand", "--step", "0"], 0);
    let found = templates(&full);
    // Each node that is not mapped has a template or a cause.
    assert!(found.len() <= counts(&full)[2], "{full}");
    // Each template, in place in an intent file, passes `semilla intent check`.
    for template in &found {
        write(&repo, ".semilla/intent/try.rs", &format!("{template}\n"));
        let check = run(&repo, &["intent", "check", ".semilla/intent/try.rs"], 0);
        assert!(check.contains(": 0 error(s) of rustc, 0 error(s) of Semilla"), "{template}\n{check}");
    }
}

// rule+ R90.5-3
#[test]
fn the_json_the_full_text_and_the_coverage_line_give_the_same_map() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    let text = run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs"], 0);
    let json: serde_json::Value = serde_json::from_str(&run(&repo, &["verify", "--json"], 0)).unwrap();
    let map = &json["steps"][0]["ast"];
    let entries = map["entries"].as_array().expect("the entries");
    assert_eq!(entries.len() as u64, map["nodes"].as_u64().unwrap());
    // Each entry of the map has its kind and the fact that holds it.
    assert!(entries.iter().all(|entry| entry["kind"].is_string() && entry["entry"].is_string()), "{map:#}");
    let count = |state: &str| entries.iter().filter(|entry| entry["state"] == state).count();
    let full = run(&repo, &["status", "--verbose"], 0);
    assert_eq!([count("claim"), count("semilla"), count("not_mapped")], counts(&full));
    assert_eq!(count("not_mapped"), not_mapped(&text), "{text}");
}
