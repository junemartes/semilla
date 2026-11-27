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
    command.env("SEMILLA_COMPILED", "0");
    command.env("SEMILLA_NO_CHECK", "1");
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
const NEW: &str = "pub struct Cart {\n    pub entries: Vec<u32>,\n    pub len: usize,\n}\n\npub fn total(c: &Cart) -> u32 {\n    c.entries.iter().sum::<u32>() + c.len as u32\n}\n\npub fn f(x: u32) -> u32 {\n    let y = x;\n    y + 1\n}\n\npub fn caller(x: u32) -> u32 {\n    let bits = x.count_ones();\n    let _ = bits;\n    x * 3\n}\n\npub fn added(values: &[u32], scale: u8) -> u64 {\n    values.iter().map(|value| u64::from(*value) << scale).sum()\n}\n\npub fn uses(x: u32) -> u32 {\n    added(&[x], 0) as u32\n}\n\npub const LIMIT: u32 = 11;\n\npub const EXTRA: u32 = LIMIT + 1;\n\npub fn limit() -> u32 {\n    EXTRA\n}\n";

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

/// The counts of the line `AST changes: …` of an output.
fn counts(text: &str) -> [usize; 3] {
    let line = text.lines().find(|line| line.trim_start().starts_with("AST changes: ")).unwrap_or_else(|| panic!("no AST line:\n{text}"));
    let numbers: Vec<usize> = line.split(|c: char| !c.is_ascii_digit()).filter(|part| !part.is_empty()).map(|part| part.parse().unwrap()).collect();
    [numbers[0], numbers[1], numbers[2]]
}

// rule+ R79.2-1
// rule+ R79.2-4
#[test]
fn the_compiled_claims_anchor_an_item_through_a_relation_function_and_the_fields_of_a_relation() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    let text = run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs", "--compiled"], 0);
    let full = run(&repo, &["expand", "--step", "0"], 0);
    // The claim calls `f` through `f_relation` (a function of the intent crate).
    assert!(full.contains("anchor: i2: same `shop::f` → `shop::f`"), "{full}");
    assert!(full.contains("anchor: i3: relation shop::Cart → shop::Cart; reads `shop::Cart.items` | `shop::Cart.entries`, `shop::Cart.len`"), "{full}");
    assert!(full.lines().any(|line| line.contains("mapped by claim i2 [R79.2-3]: added `let` `shop::f/S0`")), "{full}");
    assert!(full.lines().any(|line| line.contains("mapped by claim i3 [R79.2-4]: added field `shop::Cart.len`")), "{full}");
    assert!(counts(&text)[0] >= 4, "{text}");
}

// rule- R79.2-1
#[test]
fn the_default_mode_takes_the_anchors_from_the_warm_check_of_the_two_trees_only() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    // No `semilla intent check` before the step: the default mode builds nothing for the claims.
    let text = run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs"], 0);
    assert_eq!(counts(&text)[0], 0, "{text}");
    assert!(text.contains("the claims give no anchor: the anchors come from the claims that rustc compiles"), "{text}");
    run(&repo, &["undo"], 0);
    // The check of the same two trees: the step reads it, with no build.
    let check = run(&repo, &["intent", "check", ".semilla/intent/pr.rs"], 0);
    assert!(check.contains("0 error(s) of rustc, 0 error(s) of Semilla"), "{check}");
    let text = run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs"], 0);
    assert!(counts(&text)[0] >= 4, "{text}");
}

// rule+ R79.7-1
#[test]
fn a_body_that_semilla_proves_equivalent_stays_equivalent_with_a_node_that_is_not_mapped() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    let text = run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs", "--compiled"], 0);
    // `caller` keeps its behavior (Semilla proves it), and its two new statements are not mapped:
    // the map is a review aid, so the proof result stays, with the fact of the map next to it.
    let line = text.lines().find(|line| line.contains("`shop::caller` (src/lib.rs:")).unwrap_or_else(|| panic!("{text}"));
    assert!(line.trim_start().starts_with("equivalent (Semilla): `shop::caller`"), "{line}");
    assert!(line.ends_with("[the AST map: 2 node(s) of its item not mapped (complete the intent file)]"), "{line}");
    assert!(text.contains("1 equivalent (Semilla)") || text.contains(" equivalent (Semilla), "), "{text}");
}

// rule- R79.7-1
#[test]
fn a_proof_does_not_remove_the_request_of_a_node_that_is_not_mapped() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    let text = run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs", "--compiled"], 0);
    assert!(text.lines().any(|line| line.trim_start().starts_with("equivalent (Semilla): `shop::caller`")), "{text}");
    // The request lines of the two nodes of `caller` stay, also when Semilla proves the body.
    for at in ["shop::caller/S0", "shop::caller/S1"] {
        assert!(text.lines().any(|line| line.contains("not mapped [R79.5-1]: added `let`") && line.contains(&format!("`{at}`"))), "{at}\n{text}");
    }
}

/// The templates of the lines that are not mapped in a full text.
fn templates(text: &str) -> Vec<String> {
    text.lines().filter_map(|line| line.trim_start().strip_prefix("template: ")).map(str::to_string).collect()
}

// rule+ R79.5-1
#[test]
fn each_template_of_a_node_that_is_not_mapped_passes_the_check_of_the_intent_files() {
    let (_dir, repo) = setup();
    run(&repo, &["step", "the change", "--compiled"], 0);
    let full = run(&repo, &["expand", "--step", "0"], 0);
    let found = templates(&full);
    // A changed function, an added function that a changed body reaches, a removed function
    // with a reader, an added constant, and an added field that a changed body reads.
    for start in ["#[same(shop::caller)]", "#[new(shop::added)]", "#[removed(shop::helper_old)]", "#[new(shop::EXTRA)]", "#[relation(before::shop::Cart, after::shop::Cart)]"] {
        assert!(found.iter().any(|template| template.starts_with(start)), "{start}\n{full}");
    }
    // Each template, in place in an intent file, passes `semilla intent check`.
    for template in &found {
        write(&repo, ".semilla/intent/try.rs", &format!("{template}\n"));
        let check = run(&repo, &["intent", "check", ".semilla/intent/try.rs"], 0);
        assert!(check.contains(": 0 error(s) of rustc, 0 error(s) of Semilla"), "{template}\n{check}");
    }
}

// rule+ R79.6-1
#[test]
fn the_json_and_the_full_text_give_the_same_map() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    let text = run(&repo, &["step", "the change", "--intent", ".semilla/intent/pr.rs", "--compiled"], 0);
    let short = counts(&text);
    // The step passes with nodes that are not mapped.
    assert!(short[2] > 0, "{text}");
    let json: serde_json::Value = serde_json::from_str(&run(&repo, &["verify", "--json"], 0)).unwrap();
    let map = &json["steps"][0]["ast"];
    let entries = map["entries"].as_array().expect("the entries");
    assert_eq!(entries.len() as u64, map["nodes"].as_u64().unwrap());
    let count = |state: &str| entries.iter().filter(|entry| entry["state"] == state).count();
    assert_eq!([count("claim"), count("semilla"), count("not_mapped")], short);
    let full = run(&repo, &["status", "--verbose"], 0);
    assert_eq!(counts(&full), short);
}
