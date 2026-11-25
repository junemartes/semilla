//! The intent files of a step (design 67, parts B, C, D, and E): the commands `semilla step
//! --intent`, `semilla fact --intent` and `--again`, and `semilla intent check` on a small
//! crate `shop`.

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

const OLD: &str = "pub struct Cart { items: Vec<u32>, len: u64 }\n\npub fn parse(x: u32) -> bool { x > 3 }\npub fn half(x: i32) -> i32 { x / 2 }\npub fn cart_len(c: &Cart) -> u64 { c.len }\npub fn report(x: u32) -> u32 { x + 1 }\npub fn mode() -> u32 { if cfg!(debug_assertions) { 1 } else { 2 } }\npub fn fail(code: u32) -> ! { panic!(\"{code}\") }\npub fn limit(x: u32) -> u32 { if x > 9 { fail(1) } x }\npub fn sum(x: u32) -> u32 { x + 2 }\n";
const NEW: &str = "pub struct Cart { entries: Vec<u32>, len: u64 }\n\npub fn parse(x: u32, strict: bool) -> bool { if strict { x > 4 } else { x > 3 } }\npub fn half(x: i32) -> i32 { x >> 1 }\npub fn cart_len(c: &Cart) -> u64 { c.len }\npub fn report(x: u32) -> u32 { 1 + x }\npub fn mode() -> u32 { if cfg!(debug_assertions) { 1 } else { 2 } }\npub fn fail(code: u32) -> ! { panic!(\"{code}\") }\npub fn limit(x: u32) -> u32 { if x > 9 { fail(2) } x }\npub fn total(x: u32) -> u32 { x + 2 }\npub struct Basket { n: u32 }\npub fn basket_n(b: &Basket) -> u32 { b.n }\n";

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

const INTENT: &str = r#"use semilla_intent::prelude::*;

intent!("Add a strict mode to `parse`");

#[same(shop::report)]
fn report() {}

#[same(shop::parse)]
fn parse(x: u32) -> bool {
    after::shop::parse(x, false) == before::shop::parse(x)
}

#[change(shop::half)]
fn half(x: i32) -> bool {
    if x >= 0 { after::shop::half(x) == before::shop::half(x) } else { true }
}

#[same(shop::cart_len)]
fn private_field(c: &after::shop::Cart) -> bool {
    after::shop::cart_len(c) == c.len && before::shop::cart_len(c) == c.len
}

#[same(shop::mode, cfg = "debug_assertions")]
fn mode_in_dev() -> bool {
    after::shop::mode() == before::shop::mode() && after::shop::mode() == 1
}

#[same(shop::limit, except = [panic_message])]
fn limit() {}

#[same(shop::limit, except = [capacity])]
fn limit_capacity() {}

#[rename(before::shop::sum, after::shop::total)]
fn sum_to_total() {}
"#;

fn claim_line<'a>(text: &'a str, id: &str) -> &'a str {
    text.lines().find(|line| line.contains(&format!(" {id} "))).unwrap_or_else(|| panic!("no line of {id}:\n{text}"))
}

// rule+ R67.5-1
// rule+ R67.4-1
// rule+ R67.4-3
#[test]
fn the_default_mode_shows_the_claims_and_checks_the_forms_of_same() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/step.rs", INTENT);
    let text = run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/step.rs"], 0);
    assert!(claim_line(&text, "i1").contains("shown"), "{text}");
    for id in ["i2", "i3", "i4", "i5", "i6", "i7", "i8"] {
        assert!(claim_line(&text, id).contains("claimed: the check needs `--compiled`"), "{id}: {text}");
    }
    assert!(claim_line(&text, "i9").contains("proved: the check of `--same 'rename shop::sum shop::total'` holds"), "{text}");
    // The change claim states its item: `shop::half` is not in the gap list.
    assert!(!text.contains("--change 'shop::half=TEXT'"), "{text}");
    let status = run(&repo, &["status", "--verbose"], 0);
    assert!(status.contains("intent (8 claim(s)"), "{status}");
    let json = run(&repo, &["verify", "--json"], 0);
    assert!(json.contains("\"intent_claims\"") && json.contains("\"result\": \"claimed\""), "{json}");
}

// rule- R67.5-1
#[test]
fn a_step_with_no_intent_file_has_no_section_and_no_help() {
    let (_dir, repo) = setup();
    let text = run(&repo, &["step", "strict parse", "--compiled"], 0);
    assert!(!text.contains("  intent ("), "{text}");
    assert!(!text.contains("intent help"), "{text}");
    let json = run(&repo, &["verify", "--json"], 0);
    assert!(!json.contains("intent_claims"), "{json}");
}

// rule+ R67.5-2
// rule+ R67.3-9
// rule+ R67.3-7
// rule+ R67.3-8
// rule+ R67.6-1
// rule+ R67.4-2
// rule+ R67.3-10
// rule- R67.3-8
#[test]
fn the_compiled_step_checks_each_claim() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/step.rs", INTENT);
    let text = run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/step.rs", "--compiled"], 0);
    assert!(claim_line(&text, "i2").contains("proved: equivalent by equal MIR"), "{text}");
    assert!(claim_line(&text, "i3").contains("proved: by SMT"), "{text}");
    assert!(claim_line(&text, "i4").contains("proved"), "{text}");
    assert!(claim_line(&text, "i5").contains("proved"), "{text}");
    // `cfg = "debug_assertions"`: the release configuration is not a part of the claim.
    assert!(claim_line(&text, "i6").contains("proved"), "{text}");
    // `except = [panic_message]`: a panic with another message is the same panic.
    assert!(claim_line(&text, "i7").contains("proved"), "{text}");
    // An `except` name that the check does not know.
    assert!(claim_line(&text, "i8").contains("claimed"), "{text}");
    // Part E: the help for a body whose parameters change.
    assert!(text.contains("intent help for `shop::parse`") && text.contains("#[same(shop::parse)] fn parse(x: u32) -> bool { after::shop::parse(/* x: u32, strict: bool */) == before::shop::parse(x) }"), "{text}");
}

// rule- R67.5-2
// rule- R67.3-9
// rule- R67.3-7
// rule- R67.6-1
// rule- R67.4-2
// rule- R67.3-10
#[test]
fn a_false_claim_rejects_the_step_with_its_input() {
    let (_dir, repo) = setup();
    let wrong = r#"
#[same(shop::parse)]
fn parse_strict(x: u32) -> bool {
    after::shop::parse(x, true) == before::shop::parse(x)
}

#[same(shop::mode)]
fn mode_always() -> bool {
    after::shop::mode() == 1
}

#[same(shop::limit)]
fn limit() {}

#[same(shop::half)]
fn half_loop(x: i32) -> bool {
    after::shop::half(x) == before::shop::half(x)
}
"#;
    write(&repo, ".semilla/intent/wrong.rs", wrong);
    let text = run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/wrong.rs", "--compiled"], 1);
    assert!(text.starts_with("rejected:"), "{text}");
    assert!(claim_line(&text, "i1").contains("false") && claim_line(&text, "i1").contains("for x = 4"), "{text}");
    // No `cfg`: the claim is false in the release configuration.
    assert!(claim_line(&text, "i2").contains("false") && claim_line(&text, "i2").contains("release"), "{text}");
    // Two panics with different arguments are different panics without `except`.
    assert!(claim_line(&text, "i3").contains("false") && claim_line(&text, "i3").contains("for x = 10"), "{text}");
    assert!(claim_line(&text, "i4").contains("for x = -1"), "{text}");
    // The help is only for a cause that a claim can close.
    assert!(!text.contains("intent help for `shop::half`"), "{text}");
}

// rule+ R67.2-3
// rule- R67.2-3
#[test]
fn intent_check_gives_the_reference_errors_with_the_candidates() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/good.rs", INTENT);
    let text = run(&repo, &["intent", "check", ".semilla/intent/good.rs"], 0);
    assert!(text.contains("0 error(s)"), "{text}");
    let bad = r#"
#[same(shop::cart_len)]
fn field(c: &after::shop::Cart) -> bool {
    c.items.len() == 0
}

#[same(shop::pars)]
fn item(x: u32) -> bool { true }

#[same(shop::parse)]
fn arguments(x: u32) -> bool {
    after::shop::parse(x) == before::shop::parse(x)
}

#[same(shop::basket_n)]
fn one_version(b: &before::shop::Basket) -> bool {
    after::shop::basket_n(b) == 0
}
"#;
    write(&repo, ".semilla/intent/bad.rs", bad);
    let text = run(&repo, &["intent", "check", ".semilla/intent/bad.rs"], 1);
    assert!(text.contains("`after::shop::Cart.items`: the new `shop::Cart` has no field `items`; its fields: `entries`, `len`"), "{text}");
    assert!(text.contains("no version has a body `shop::pars`") && text.contains("`shop::parse`"), "{text}");
    assert!(text.contains("`after::shop::parse` takes 2 argument(s); the claim gives 1"), "{text}");
    assert!(text.contains("`before::shop::Basket`: the old version has no type `shop::Basket`"), "{text}");
    // A parse error names the place and what the parser expected.
    write(&repo, ".semilla/intent/broken.rs", "#[same(shop::parse)]\nfn f( -> bool { true }\n");
    let text = run(&repo, &["intent", "check", ".semilla/intent/broken.rs"], 1);
    assert!(text.contains(".semilla/intent/broken.rs:2:") && text.contains("expected"), "{text}");
}

// rule+ R67.5-2
#[test]
fn a_reference_error_rejects_the_step() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/bad.rs", "#[same(shop::pars)]\nfn item(x: u32) -> bool { true }\n");
    let text = run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/bad.rs", "--compiled"], 1);
    assert!(claim_line(&text, "i1").contains("error") && text.contains("`shop::parse`"), "{text}");
}

// rule+ R67.5-3
#[test]
fn a_later_step_checks_a_claim_of_an_earlier_step_again() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/step.rs", INTENT);
    run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/step.rs", "--compiled"], 0);
    write(&repo, "src/lib.rs", &NEW.replace("pub fn half(x: i32) -> i32 { x >> 1 }", "pub fn half(x: i32) -> i32 { if x == 7 { 99 } else { x >> 1 } }"));
    let text = run(&repo, &["step", "a special case", "--compiled"], 1);
    let line = claim_line(&text, "s0.i4");
    assert!(line.contains("false") && line.contains("this step changes `shop::half`") && line.contains("for x = 7"), "{text}");
}

// rule- R67.5-3
#[test]
fn a_later_step_that_keeps_the_bodies_of_a_claim_does_not_check_it_again() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/step.rs", INTENT);
    run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/step.rs", "--compiled"], 0);
    write(&repo, "src/lib.rs", &format!("{NEW}pub fn extra() -> u32 {{ 3 }}\n"));
    let text = run(&repo, &["step", "an extra function", "--compiled"], 0);
    assert!(!text.contains("s0.i"), "{text}");
}

// rule+ R67.5-4
#[test]
fn fact_again_checks_the_stored_claims_and_fact_intent_replaces_a_file() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/step.rs", INTENT);
    run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/step.rs", "--compiled"], 0);
    // The stored step keeps the text of the file: a change of the file on disk changes nothing.
    write(&repo, ".semilla/intent/step.rs", "#[same(shop::pars)]\nfn item(x: u32) -> bool { true }\n");
    let text = run(&repo, &["fact", "--again"], 0);
    assert!(claim_line(&text, "i3").contains("proved"), "{text}");
    // `--intent` with the path of the stored file replaces it; a reference error stores nothing.
    let text = run(&repo, &["fact", "--intent", ".semilla/intent/step.rs"], 1);
    assert!(text.contains("rejected: nothing is added") && claim_line(&text, "i1").contains("error"), "{text}");
    let status = run(&repo, &["status", "--verbose"], 0);
    assert!(status.contains("intent (8 claim(s)"), "{status}");
}

// rule+ R67.5-5
#[test]
fn an_intent_file_in_a_source_folder_is_refused() {
    let (_dir, repo) = setup();
    write(&repo, "src/intent.rs", INTENT);
    let output = semilla_env(&repo, &["step", "strict parse", "--intent", "src/intent.rs"]);
    assert_ne!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stderr).contains("is in a `src` folder"));
}

// rule- R67.5-5
// rule- R67.5-4
#[test]
fn a_stored_intent_file_keeps_its_text_until_fact_intent() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/step.rs", INTENT);
    run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/step.rs"], 0);
    let json = run(&repo, &["verify", "--json"], 0);
    assert!(json.contains("\"path\": \".semilla/intent/step.rs\"") || std::fs::read_to_string(repo.join(".semilla/session.json")).unwrap().contains("Add a strict mode"), "{json}");
    // The same text again adds nothing.
    let text = run(&repo, &["fact", "--intent", ".semilla/intent/step.rs"], 0);
    assert!(text.contains("stored: 0 new fact(s)"), "{text}");
}

// rule- R67.4-1
// rule- R67.4-3
#[test]
fn a_false_form_of_same_or_a_change_claim_with_no_edit_rejects_the_step() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/forms.rs", "#[rename(before::shop::parse, after::shop::half)]\nfn wrong() {}\n\n#[change(shop::cart_len)]\nfn no_edit(x: u32) -> bool { true }\n");
    let text = run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/forms.rs"], 1);
    assert!(claim_line(&text, "i1").contains("false"), "{text}");
    assert!(text.lines().any(|line| line.contains("✗") && line.contains("change") && line.contains("shop::cart_len")), "{text}");
}

// rule+ R69.5-1
#[test]
fn a_relation_before_a_claim_keeps_the_error_of_the_claim_in_the_step() {
    let (_dir, repo) = setup();
    let text = "fn same_len(b: &before::shop::Cart, a: &after::shop::Cart) -> bool { b.len == a.len }\n\n#[same(shop::cart_len)]\nfn field(c: &after::shop::Cart) -> bool { c.items.len() == 0 }\n";
    write(&repo, ".semilla/intent/relation.rs", text);
    let check = run(&repo, &["intent", "check", ".semilla/intent/relation.rs"], 1);
    assert!(check.contains("✗ i1 ") && check.contains("has no field `items`"), "{check}");
    // The step finds the same error with the same id; it never shows "pending".
    let step = run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/relation.rs", "--compiled"], 1);
    assert!(claim_line(&step, "i1").contains("error") && step.contains("has no field `items`") && !step.contains("pending"), "{step}");
}

/// A repository of the crate `shop` with a method whose type of `self` changes a field.
fn setup_self_type() -> (common::TestDir, PathBuf) {
    let dir = common::TestDir::new();
    let repo = dir.path().join("repo");
    write(&repo, "Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n");
    write(&repo, ".gitignore", "/target\n/.semilla/\n");
    write(&repo, "src/lib.rs", "pub struct Pt { x: u32, n: u8 }\nimpl Pt {\n    pub fn get(&self) -> u64 { self.x as u64 }\n}\n");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
    write(&repo, "src/lib.rs", "pub struct Pt { x: u64, n: u8 }\nimpl Pt {\n    pub fn get(&self) -> u64 { self.x }\n}\n");
    write(&repo, ".semilla/intent/step.rs", "intent!(\"widen x\");\n");
    (dir, repo)
}

// rule+ R69.5-3
#[test]
fn the_help_names_the_changed_field_of_the_type_of_self() {
    let (_dir, repo) = setup_self_type();
    let text = run(&repo, &["step", "widen x", "--intent", ".semilla/intent/step.rs", "--compiled"], 0);
    assert!(text.contains("intent help for `shop::Pt::get`") && text.contains("`fn get (& self) -> u64`: the type of `self` changes: `shop::Pt`: `x: u32` → `x: u64`"), "{text}");
    assert!(!text.contains("`fn get (& self) -> u64` → `fn get (& self) -> u64`"), "{text}");
    // The result is "not decided": the help gives a template for each case, and the receiver
    // is a value of the old type.
    assert!(text.contains("if the behavior does not change: `#[same(shop::Pt::get)] fn get(this: &before::shop::Pt)"), "{text}");
    assert!(text.contains("if it changes: `#[change(shop::Pt::get, reason = \"…\")] fn get(this: &before::shop::Pt)"), "{text}");
}

// rule- R69.5-3
#[test]
fn the_help_of_two_different_signatures_names_the_two_signatures() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/step.rs", "intent!(\"strict parse\");\n");
    let text = run(&repo, &["step", "strict parse", "--intent", ".semilla/intent/step.rs", "--compiled"], 0);
    assert!(text.contains("`fn parse (x : u32) -> bool` → `fn parse (x : u32 , strict : bool) -> bool`") && !text.contains("the type of `self` changes"), "{text}");
}
