//! Design 87: a printed claim template is a checked claim template. Each template of a step goes
//! through one gate (the check of `semilla intent check`), takes its names from the name record
//! of the compiler, and has a unique function name.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn command(repo: &Path) -> Command {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"));
    command.env("SEMILLA_CACHE_DIR", repo.parent().unwrap().join("cache"));
    command
}

fn semilla(repo: &Path, args: &[&str]) -> Output {
    command(repo).args(args).output().unwrap()
}

#[track_caller]
fn run(repo: &Path, args: &[&str], expect: i32) -> String {
    let output = semilla(repo, args);
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

/// The old version: one item of each shape of design 87 (a trait impl method, a derived impl, a
/// generic type with a generic impl, a receiver of a generic type, a type of another crate in a
/// signature, two items with the same short name, and the items that the step removes).
const OLD: &str = "use std::cmp::Ordering;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Hash)]
pub struct Cart { pub n: u32 }

pub struct Table<V> { pub items: Vec<V> }

impl<V: Clone> Table<V> {
    pub fn get(&self, at: usize) -> Option<V> { self.items.get(at).cloned() }
}

pub struct Wrap { pub x: u32 }
impl std::fmt::Display for Wrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, \"{}\", self.x) }
}
impl std::fmt::Debug for Wrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, \"W{}\", self.x) }
}

pub trait Price { fn price(&self) -> u32; }
impl Price for Cart { fn price(&self) -> u32 { self.n * 2 } }

pub fn order(a: u32, b: u32) -> Ordering { a.cmp(&b) }
pub fn count(m: &HashMap<u32, u32>) -> usize { m.len() }
pub fn size(t: &Table<u32>) -> usize { t.items.len() }
pub mod inner { pub fn size(x: u32) -> u32 { x + 1 } }
pub fn gone_flag(b: bool, text: &str) -> bool { !b && text.is_empty() }
impl Wrap { pub fn gone_label(&self, prefix: &str) -> String { format!(\"{prefix}{}\", self.x) } }
";

/// The new version: each body changes; `rank` (a parameter of a type of another crate by its
/// short name), `Table::put` (a method of a generic impl), and `Wrap::triple` are new; the two
/// `gone_` items are removed.
const NEW: &str = "use std::cmp::Ordering;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Hash)]
pub struct Cart { pub n: u64 }

pub struct Table<V> { pub items: Vec<V> }

impl<V: Clone> Table<V> {
    pub fn get(&self, at: usize) -> Option<V> { self.items.get(at + 0).cloned().or(None) }
    pub fn put(&mut self, v: V) { self.items.push(v) }
}

pub struct Wrap { pub x: u32 }
impl std::fmt::Display for Wrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, \"<{}>\", self.x) }
}
impl std::fmt::Debug for Wrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, \"W:{}\", self.x) }
}

pub trait Price { fn price(&self) -> u32; }
impl Price for Cart { fn price(&self) -> u32 { (self.n * 3) as u32 } }

pub fn order(a: u32, b: u32) -> Ordering { b.cmp(&a) }
pub fn count(m: &HashMap<u32, u32>) -> usize { m.len() + 1 }
pub fn size(t: &Table<u32>) -> usize { t.items.len() * 2 }
pub mod inner { pub fn size(x: u32) -> u32 { x + 2 } }
pub fn rank(o: Ordering, k: u32) -> u32 { o as u32 + k }
impl Wrap { pub fn triple(&self) -> u32 { self.x * 3 } }
";

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

/// The coverage line of a body.
fn cover_line<'a>(text: &'a str, name: &str) -> &'a str {
    text.lines().find(|line| line.contains(&format!(": `{name}` ("))).unwrap_or_else(|| panic!("no coverage line of `{name}`:\n{text}"))
}

/// Each template of a full text (the pieces in back quotes of a line with no claim, and the
/// template lines of the AST map); one text is one template.
fn templates_of(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut pieces: Vec<String> = Vec::new();
        if line.contains("MIR changes with no claim: ") {
            pieces.extend(line.split('`').filter(|piece| piece.starts_with("#[")).map(str::to_string));
        }
        if let Some(rest) = line.trim_start().strip_prefix("template: #[") {
            pieces.push(format!("#[{rest}"));
        }
        for piece in pieces {
            if !found.contains(&piece) {
                found.push(piece);
            }
        }
    }
    found
}

/// The check of the templates in one file.
fn check_all(repo: &Path, templates: &[String]) -> Output {
    let text = format!("use semilla_intent::prelude::*;\n\n{}\n", templates.iter().map(|template| template.replace('…', "true")).collect::<Vec<_>>().join("\n"));
    write(repo, ".semilla/intent/all.rs", &text);
    semilla(repo, &["intent", "check", ".semilla/intent/all.rs"])
}

// rule+ R87.1-1
// rule- R87.1-1
// rule+ R87.1-2
// rule- R87.1-2
// rule+ R87.2-1
// rule- R87.2-1
// rule+ R87.2-2
// rule- R87.2-2
// rule+ R87.5-1
// rule- R87.5-1
// rule+ R87.6-1
#[test]
fn each_printed_template_of_each_shape_passes_the_check_in_one_file() {
    let (_dir, repo) = setup();
    run(&repo, &["step", "the change"], 0);
    let text = run(&repo, &["status", "--verbose"], 0);
    let templates = templates_of(&text);
    assert!(templates.len() >= 20, "{templates:?}\n{text}");
    // [R87.1-1] [R87.5-1] [R87.6-1] Each template passes the check, all in one file.
    let output = check_all(&repo, &templates);
    assert!(output.status.success(), "{templates:#?}\n{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    // [R87.1-1] The change form of `count` names `HashMap`, which no path of the intent crate
    // resolves: the gate drops it, and the line keeps the other templates. The negative case: a
    // form that the check accepts is printed as it is.
    let count = cover_line(&text, "shop::count");
    assert!(!count.contains("#[change(shop::count)]") && count.contains("`#[same(shop::count)] fn count_same() {}`"), "{count}");
    assert!(cover_line(&text, "shop::order").contains("`#[change(shop::order)] fn order_change(a: u32, b: u32) -> bool { … }`"), "{text}");
    // [R87.1-2] A type of another crate by its short name: the written form does not resolve,
    // and the next form (the definition path of the name record) passes. The negative case: a
    // primitive type keeps its written form.
    let rank = cover_line(&text, "shop::rank");
    assert!(rank.contains("o: core::cmp::Ordering, k: u32"), "{rank}");
    // [R87.2-1] The names come from the name record: a derived impl and a trait impl get the
    // qualified path, a method of a generic impl gets the path of the type with no arguments,
    // and a receiver of a generic type gives no parameters. The negative case: no template has a
    // path with generic arguments.
    assert!(cover_line(&text, "<shop::Cart as std::clone::Clone>::clone").contains("`#[same(<shop::Cart as core::clone::Clone>::clone)]"), "{text}");
    assert!(cover_line(&text, "<shop::Cart as shop::Price>::price").contains("`#[same(<shop::Cart as shop::Price>::price)]"), "{text}");
    assert!(cover_line(&text, "shop::Table::<V>::get").contains("`#[same(shop::Table::get)] fn get_same() {}`") && cover_line(&text, "shop::Table::<V>::get").contains("`#[change(shop::Table::get)] fn get_change() -> bool { … }`"), "{text}");
    assert!(templates.iter().all(|template| !template.contains("::<")), "{templates:#?}");
    // A removed method and an added method of an inherent impl: the receiver of the type.
    // Design 90: the removed method is a "delete" of the map; the template is on the coverage
    // line of its body.
    assert!(text.contains("#[removed(shop::Wrap::gone_label)] fn gone_label_removed(this: &before::shop::Wrap, prefix: &str) -> bool { let old = before::shop::Wrap::gone_label(this, prefix); true }"), "{text}");
    assert!(text.contains("#[new(shop::Wrap::triple)] fn triple_new(this: &after::shop::Wrap)"), "{text}");
    // [R87.2-2] A body that no path of the language names: `no template` with the cause. The
    // negative case: each body with a name of the record has a template.
    assert!(cover_line(&text, "dyn std::fmt::Debug").contains("no template: `dyn std::fmt::Debug` is no item of the syntax tree"), "{text}");
    // [R87.5-1] Two items with one short name, and two impls with one method name: the second
    // template gets the next free name. The negative case: a unique name stays.
    assert!(text.contains("fn size_same() {}") && text.contains("fn size_same_2() {}"), "{text}");
    assert!(text.contains("fn fmt_same() {}") && text.contains("fn fmt_same_2() {}"), "{text}");
    assert!(text.contains("fn order_same() {}") && !text.contains("fn order_same_2"), "{text}");
}

// rule+ R87.1-3
// rule- R87.1-3
#[test]
fn a_warm_step_starts_no_check_of_the_templates_again() {
    let (_dir, repo) = setup();
    let first = command(&repo).args(["step", "the change"]).env("SEMILLA_TIMING", "1").output().unwrap();
    let stderr = String::from_utf8_lossy(&first.stderr);
    // The negative case: a cold step checks the templates.
    assert!(stderr.lines().any(|line| line.starts_with("[timing] the gate of the claim templates (") && line.contains(" templates)")), "{stderr}");
    let text = run(&repo, &["status", "--verbose"], 0);
    // The same step in a new session with the same cache: the stored verdicts, no compile.
    std::fs::remove_dir_all(repo.join(".semilla")).unwrap();
    // [R93.1-2] Another intent text: the result of the step is not in the store, so the check
    // runs, with the stored verdicts of the gate.
    let second = command(&repo).args(["step", "the change, again"]).env("SEMILLA_TIMING", "1").output().unwrap();
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("[timing] the gate of the claim templates (from the cache)"), "{stderr}");
    assert_eq!(templates_of(&run(&repo, &["status", "--verbose"], 0)), templates_of(&text));
}

// rule+ R87.3-1
// rule- R87.3-1
// rule- R87.6-1
#[test]
fn a_scope_path_that_rustc_cannot_take_gets_an_error_of_semilla_and_no_panic() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/bad.rs", "use semilla_intent::prelude::*;\n\n#[same(shop::Table::<V>::get)]\nfn get_same() {}\n");
    let output = semilla(&repo, &["intent", "check", ".semilla/intent/bad.rs"]);
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    // [R87.6-1] (negative) The check rejects such a template.
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("error[S0113]") && text.contains("write `shop::Table::get`"), "{text}");
    // [R87.3-1] The compiler never sees the claim: no panic text and no internal error.
    assert!(!text.contains("panicked") && !text.contains("S0121") && !text.contains("internal compiler error"), "{text}");
    // The negative case: the path with no arguments passes.
    write(&repo, ".semilla/intent/good.rs", "use semilla_intent::prelude::*;\n\n#[same(shop::Table::get)]\nfn get_same() {}\n");
    run(&repo, &["intent", "check", ".semilla/intent/good.rs"], 0);
}

// rule+ R87.4-1
// rule- R87.4-1
#[test]
fn a_body_with_a_new_claim_has_its_own_state_and_the_exit_code_stays() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/step.rs", "use semilla_intent::prelude::*;\n\n#[new(shop::rank)]\nfn rank_new(k: u32) -> bool { true }\n");
    run(&repo, &["step", "the change", "--intent", ".semilla/intent/step.rs"], 0);
    let text = run(&repo, &["status", "--verbose"], 0);
    let line = cover_line(&text, "shop::rank");
    assert!(line.contains("with a claim that is checked later: `shop::rank`") && line.contains("claim i1 (`#[new]`): the check of `#[new]` comes in batch"), "{line}");
    assert!(text.contains(", 1 with a claim that is checked later"), "{text}");
    let json = run(&repo, &["verify", "--json"], 0);
    assert!(json.contains("\"state\": \"later\""), "{json}");
    // The negative case: with no claim, the body is "MIR changes with no claim".
    let (_dir, repo) = setup();
    run(&repo, &["step", "the change"], 0);
    let text = run(&repo, &["status", "--verbose"], 0);
    assert!(cover_line(&text, "shop::rank").contains("MIR changes with no claim: `shop::rank`") && !text.contains("checked later"), "{text}");
}
