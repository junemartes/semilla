//! `&mut` and effects in the claims of a step (design 82): a relation of a type that changes is
//! the check of each item with a value of that type (never equal MIR alone), a false claim with
//! `&mut` gets a suggested test that makes its calls in order, and the help template of an item
//! with `&mut` gives the form of a place.

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

/// A document with a new cached count: `new`, `count`, `clear`, and `fill` keep the relation of
/// the intent file, and `push` forgets the count (its MIR stays equal to the old MIR). `Log`
/// does not change.
const OLD: &str = r#"pub struct Doc { lines: Vec<u8> }
impl Doc {
    pub fn new() -> Doc { Doc { lines: Vec::new() } }
    pub fn count(&self) -> usize { self.lines.len() }
    pub fn push(&mut self, x: u8) { self.lines.push(x) }
    pub fn clear(&mut self) { self.lines.clear() }
}
pub fn fill(d: &mut Doc, x: u8) { d.lines.push(x) }
pub struct Log { v: Vec<u8> }
impl Log {
    pub fn put(&mut self, x: u8) { self.v.push(x) }
}
"#;

const NEW: &str = r#"pub struct Doc { lines: Vec<u8>, n: usize }
impl Doc {
    pub fn new() -> Doc { Doc { lines: Vec::new(), n: 0 } }
    pub fn count(&self) -> usize { self.n }
    pub fn push(&mut self, x: u8) { self.lines.push(x) }
    pub fn clear(&mut self) { self.lines.clear(); self.n = 0 }
}
pub fn fill(d: &mut Doc, x: u8) { d.lines.push(x); d.n += 1 }
pub struct Log { v: Vec<u8> }
impl Log {
    pub fn put(&mut self, x: u8) { self.v.push(x) }
}
"#;

/// A repository with the old version committed and the new version in the working tree.
fn setup() -> (common::TestDir, PathBuf) {
    setup_with(OLD, NEW)
}

fn setup_with(old: &str, new: &str) -> (common::TestDir, PathBuf) {
    let dir = common::TestDir::new();
    let repo = dir.path().join("repo");
    write(&repo, "Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n");
    write(&repo, ".gitignore", "/target\n/.semilla/\n");
    write(&repo, "src/lib.rs", old);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
    write(&repo, "src/lib.rs", new);
    (dir, repo)
}

fn claim_line<'a>(text: &'a str, id: &str) -> &'a str {
    text.lines().find(|line| line.contains(&format!(" {id} "))).unwrap_or_else(|| panic!("no line of {id}:\n{text}"))
}

const INTENT: &str = r#"intent!("Cache the count of a document");

#[relation(before::shop::Doc, after::shop::Doc)]
fn doc(b: &before::shop::Doc, a: &after::shop::Doc) -> bool {
    a.lines == b.lines && a.n == b.lines.len()
}

#[same(shop::Doc::new, shop::Doc::count, shop::Doc::push)]
fn doc_methods() {}

#[same(shop::Log::put)]
fn log_put() {}

#[same(shop::Doc::push)]
fn count_after_push(x: u8) -> bool {
    let mut b = before::shop::Doc::new();
    before::shop::Doc::push(&mut b, x);
    let mut a = after::shop::Doc::new();
    after::shop::Doc::push(&mut a, x);
    before::shop::Doc::count(&b) == after::shop::Doc::count(&a)
}

#[same(shop::fill)]
fn no_call(x: u8) -> bool {
    match x {
        9 => false,
        _ => true,
    }
}
"#;

// rule+ R82.1-3
// rule- R82.1-3
#[test]
fn a_relation_of_a_type_of_the_signature_is_the_check_of_the_item_and_never_equal_mir() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    let text = run(&repo, &["step", "cache the count", "--intent", ".semilla/intent/pr.rs", "--compiled"], 1);
    // R82.1-3 (positive): `push` has equal MIR in the two versions, but it forgets the cached
    // count: related places before the call give places after the call that do not correspond,
    // so it is false with an exact input. [R82.1-5] The proofs of `new` and `count` assume the
    // relation, and `push` breaks it: they are false with the input of `push`.
    for id in ["i3", "i4"] {
        let line = claim_line(&text, id);
        assert!(line.contains("false: the relation `doc` does not hold after `shop::Doc::push`") && line.contains("the proof alone: related inputs give related outcomes"), "{text}");
    }
    let push = claim_line(&text, "i5");
    assert!(push.contains("false") && push.contains("self_before") && push.contains("with `*self` = ") && !push.contains("equal MIR"), "{text}");
    // R82.1-3 (negative): no relation fits a type of the signature of `Log::put`: equal MIR stays
    // a proof.
    assert!(claim_line(&text, "i6").contains("proved: equivalent by equal MIR"), "{text}");
    // A property claim that reads the count after `push` is false too, with a suggested test
    // that makes the calls of the old version in order (R82.3-1).
    let count = claim_line(&text, "i7");
    assert!(count.contains("false") && count.contains("suggested test `semilla_test_"), "{text}");
    for line in ["let mut value1 = crate::Doc::new();", "let _ = crate::Doc::push(&mut value1, arg2_2);", "let got3 = crate::Doc::count(&value1);", "assert_eq!(got3, 1);"] {
        assert!(text.contains(line), "{line}\n{text}");
    }
}

// rule+ R82.3-1
// rule- R82.3-1
#[test]
fn a_false_claim_mir_gets_the_test_of_its_calls_and_a_claim_with_no_call_gets_none() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    let text = run(&repo, &["step", "cache the count", "--intent", ".semilla/intent/pr.rs", "--compiled"], 1);
    // R82.3-1 (positive): the test of the false claim makes the value, calls `push` with
    // `&mut` of it, and asserts the old count after the calls; the step lists it.
    assert!(text.contains("suggested tests: ") && text.contains("fn calls_in_order() {"), "{text}");
    let at = |part: &str| text.find(part).unwrap_or_else(|| panic!("{part}\n{text}"));
    assert!(at("crate::Doc::new();") < at("crate::Doc::push(&mut value1") && at("crate::Doc::push(&mut value1") < at("crate::Doc::count(&value1)"), "{text}");
    // R82.3-1 (negative): a claim that is false by its own operations makes no call of a
    // version: no test.
    let none = claim_line(&text, "i8");
    assert!(none.contains("false") && none.contains("for x = 9") && none.contains("no suggested test: the claim MIR gives no call of a version on its failed path"), "{text}");
}

/// The templates of the help notes of a step output (the text in backquotes after "if the
/// behavior does not change: ").
fn templates(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines().filter(|line| line.contains("intent help for ")) {
        let mark = "if the behavior does not change: `";
        if let Some(at) = line.find(mark) {
            let rest = &line[at + mark.len()..];
            if let Some(end) = rest.rfind('`') {
                out.push(rest[..end].to_string());
            }
        }
    }
    out
}

// rule+ R82.3-2
#[test]
fn the_help_template_of_an_item_with_mut_gives_a_place_of_each_version() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", "intent!(\"Cache the count of a document\");\n");
    let text = run(&repo, &["step", "cache the count", "--intent", ".semilla/intent/pr.rs", "--compiled"], 0);
    let found = templates(&text);
    // R82.3-2 (positive): `&mut self` and a `&mut` parameter are a value of each version that
    // the call takes as `&mut`; the claim compares the values after the calls.
    let clear = "#[same(shop::Doc::clear)] fn clear(mut this_before: before::shop::Doc, mut this_after: after::shop::Doc) -> bool { !(same(&this_before, &this_after)) || { let old = before::shop::Doc::clear(&mut this_before); let new = after::shop::Doc::clear(&mut this_after); new == old && same(&this_before, &this_after) } }";
    assert!(found.iter().any(|template| template == clear), "{found:#?}\n{text}");
    assert!(found.iter().any(|template| template.starts_with("#[same(shop::fill)] fn fill(mut d_before: before::shop::Doc, mut d_after: after::shop::Doc, x: u8) -> bool") && template.contains("before::shop::fill(&mut d_before, x)")), "{found:#?}");
    // Each template, in place in the intent file, passes `semilla intent check`.
    for template in &found {
        write(&repo, ".semilla/intent/try.rs", &format!("{template}\n"));
        let check = run(&repo, &["intent", "check", ".semilla/intent/try.rs"], 0);
        assert!(check.contains(": 0 error(s)"), "{template}\n{check}");
    }
    // With the relation, the template of `clear` is a claim that the step proves alone (a new
    // step of the same change in a new repository: the first step is stored); `push` breaks the
    // relation, so the claim is false (R82.1-5).
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", &format!("#[relation(before::shop::Doc, after::shop::Doc)]\nfn doc(b: &before::shop::Doc, a: &after::shop::Doc) -> bool {{ a.lines == b.lines && a.n == b.lines.len() }}\n{clear}\n"));
    let text = run(&repo, &["step", "cache the count", "--intent", ".semilla/intent/pr.rs", "--compiled"], 1);
    let line = claim_line(&text, "i2");
    assert!(line.contains("does not hold after `shop::Doc::push`") && line.contains("the proof alone: by SMT"), "{text}");
}

/// A document with a cached count whose methods keep the relation; the new version adds `extra`,
/// a method that writes the document (a body of the new version only).
const KEPT_OLD: &str = r#"pub struct Doc { lines: Vec<u8> }
impl Doc {
    pub fn new() -> Doc { Doc { lines: Vec::new() } }
    pub fn count(&self) -> usize { self.lines.len() }
    pub fn add(&mut self, x: u8) { self.lines.push(x) }
}
"#;

const KEPT_NEW: &str = r#"pub struct Doc { lines: Vec<u8>, n: usize }
impl Doc {
    pub fn new() -> Doc { Doc { lines: Vec::new(), n: 0 } }
    pub fn count(&self) -> usize { self.n }
    pub fn add(&mut self, x: u8) { self.lines.push(x); self.n += 1 }
}
"#;

const KEPT_INTENT: &str = r#"#[relation(before::shop::Doc, after::shop::Doc)]
fn doc(b: &before::shop::Doc, a: &after::shop::Doc) -> bool {
    a.lines == b.lines && a.n == b.lines.len()
}

#[same(shop::Doc::new, shop::Doc::count)]
fn doc_methods() {}
"#;

// rule+ R82.1-5
// rule- R82.1-5
#[test]
fn a_proof_with_a_relation_holds_only_if_each_body_that_writes_the_type_keeps_it() {
    // R82.1-5 (positive): `extra` makes a value of the type and has no old version, so the check
    // cannot prove that it keeps the relation: the two proofs are conditional ("claimed"), with
    // the list of the bodies that the check does not prove, and they explain no body.
    let extra = KEPT_NEW.replace("pub fn add(&mut self, x: u8)", "pub fn extra(&mut self) { self.lines.push(0); self.n += 1 }\n    pub fn add(&mut self, x: u8)");
    let (_dir, repo) = setup_with(KEPT_OLD, &extra);
    write(&repo, ".semilla/intent/pr.rs", KEPT_INTENT);
    let text = run(&repo, &["step", "cache the count", "--intent", ".semilla/intent/pr.rs", "--compiled"], 0);
    for id in ["i2", "i3"] {
        let line = claim_line(&text, id);
        assert!(line.contains("claimed: proved if the relation `doc` holds; not checked: `shop::Doc::extra` (a body of one version only) for `doc`"), "{text}");
    }
    assert!(!text.contains("explained by claim i2") && !text.contains("explained by claim i3"), "{text}");
    // R82.1-5 (negative): each body that makes or changes a value of the type (`new`, `add`)
    // keeps the relation: the proofs stay plain proofs, and they explain their bodies.
    let (_dir, repo) = setup_with(KEPT_OLD, KEPT_NEW);
    write(&repo, ".semilla/intent/pr.rs", KEPT_INTENT);
    let text = run(&repo, &["step", "cache the count", "--intent", ".semilla/intent/pr.rs", "--compiled"], 0);
    for id in ["i2", "i3"] {
        let line = claim_line(&text, id);
        assert!(line.contains("proved: related inputs give related outcomes") && line.contains("the relation `doc` holds after each body that makes or changes a value of its type: 2 checked"), "{text}");
    }
    assert!(text.contains("explained by claim i2") && text.contains("explained by claim i3"), "{text}");
}
