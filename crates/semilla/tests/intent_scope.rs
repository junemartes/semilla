//! The relations and the scopes of a step (design 80): `#[relation]` is a hint that the claims
//! use where the types of the two versions differ, and a claim on a scope of more than one item
//! (`impl TYPE`, `impl TRAIT for TYPE`, `MODULE::*`, `callers(ITEM)`, a list) gives one result
//! for each item.

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

const OLD: &str = r#"pub struct Cart { items: Vec<u32> }
impl Cart {
    pub fn new() -> Cart { Cart { items: Vec::new() } }
    pub fn len(&self) -> usize { self.items.len() }
    pub fn is_empty(&self) -> bool { self.items.is_empty() }
    pub fn push(&mut self, x: u32) { self.items.push(x) }
    pub fn first(&self) -> Option<&u32> { self.items.first() }
}
impl std::fmt::Display for Cart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.items.len()) }
}
pub mod calc {
    pub fn half(x: u32) -> u32 { x / 2 }
    pub fn quarter(x: u32) -> u32 { half(half(x)) }
    fn hidden(x: u32) -> u32 { x / 4 }
    pub fn uses_hidden(x: u32) -> u32 { hidden(x) }
}
pub fn use_half(x: u32) -> u32 { calc::half(x) + 1 }
"#;

const NEW: &str = r#"pub struct Cart { items: Vec<u32>, len: usize }
impl Cart {
    pub fn new() -> Cart { Cart { items: Vec::new(), len: 0 } }
    pub fn len(&self) -> usize { self.len }
    pub fn is_empty(&self) -> bool { self.len == 0 }
    pub fn push(&mut self, x: u32) { self.items.push(x); self.len += 1 }
    pub fn first(&self) -> Option<&u32> { self.items.get(0) }
}
impl std::fmt::Display for Cart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.len) }
}
pub mod calc {
    pub fn half(x: u32) -> u32 { x >> 1 }
    pub fn quarter(x: u32) -> u32 { half(x) >> 1 }
    fn hidden(x: u32) -> u32 { x >> 2 }
    pub fn uses_hidden(x: u32) -> u32 { hidden(x) }
}
pub fn use_half(x: u32) -> u32 { 1 + calc::half(x) }
"#;

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

const INTENT: &str = r#"intent!("Cache the length of a cart");

#[relation(before::shop::Cart, after::shop::Cart)]
fn cart(b: &before::shop::Cart, a: &after::shop::Cart) -> bool {
    same(&b.items, &a.items) && a.len == b.items.len()
}

#[same(impl shop::Cart)]
fn cart_methods() {}

#[same(shop::calc::*)]
fn calc_module() {}

#[same(callers(shop::calc::half))]
fn half_callers() {}

#[same(impl std::fmt::Display for *)]
fn displays() {}

#[same(shop::calc::half, shop::use_half)]
fn two_items() {}
"#;

/// The line of a claim and the lines under it (its items).
fn claim_block(text: &str, id: &str) -> String {
    let start = text.find(&format!(" {id} ")).unwrap_or_else(|| panic!("no line of {id}:\n{text}"));
    let start = text[..start].rfind('\n').map(|at| at + 1).unwrap_or(0);
    let rest = &text[start..];
    let mut lines = rest.lines();
    let mut out = vec![lines.next().unwrap_or("").to_string()];
    out.extend(lines.take_while(|line| line.starts_with("      ")).map(str::to_string));
    out.join("\n")
}

fn semantic_section(text: &str) -> String {
    let start = text.find("  semantic changes (").unwrap_or_else(|| panic!("no section \"semantic changes\":\n{text}"));
    let rest = &text[start..];
    let end = rest.lines().skip(1).position(|line| !line.starts_with("    ")).map(|at| rest.lines().take(at + 1).map(|line| line.len() + 1).sum()).unwrap_or(rest.len());
    rest[..end].to_string()
}

// rule+ R80.3-1 R80.3-2 R80.1-1
#[test]
fn a_claim_on_a_scope_gives_one_line_for_each_item_and_explains_the_bodies_of_its_proofs() {
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/pr.rs", INTENT);
    let text = run(&repo, &["step", "cache the length", "--intent", ".semilla/intent/pr.rs", "--compiled"], 0);
    // R80.1-1: a relation is a hint (not a claim of the count), and the claims use it.
    let relation = claim_block(&text, "i2");
    assert!(relation.contains("hint: a relation of an old type and a new type"), "{text}");
    assert!(text.contains("intent (6 claim(s), 1 hint(s);"), "{text}");
    // `impl TYPE`: each method of the inherent impls (not the method of the trait impl).
    let cart = claim_block(&text, "i3");
    // [R82.1-4] `push` takes `&mut self` of the type that changes: the places after the call are
    // related too. `first` returns a reference (the pointer memory of batch 13e): claimed.
    assert!(cart.contains("claimed: 5 item(s) of the scope `impl shop::Cart`: 4 proved, 0 false, 1 claimed"), "{cart}");
    for item in ["new", "len", "is_empty", "push"] {
        assert!(cart.contains(&format!("`shop::Cart::{item}`: proved: related inputs give related outcomes")), "{item}: {cart}");
    }
    assert!(cart.contains("`shop::Cart::first`: claimed") && !cart.contains("fmt"), "{cart}");
    // `MODULE::*`: each `pub` function of the module.
    let module = claim_block(&text, "i4");
    assert!(module.contains("proved: 3 item(s) of the scope `shop::calc::*`"), "{module}");
    for item in ["half", "quarter", "uses_hidden"] {
        assert!(module.contains(&format!("`shop::calc::{item}`: proved")), "{item}: {module}");
    }
    assert!(!module.contains("      `shop::calc::hidden`:"), "{module}");
    // `callers(ITEM)`: each body that calls the item.
    let callers = claim_block(&text, "i5");
    assert!(callers.contains("proved: 2 item(s) of the scope `callers(shop::calc::half)`") && callers.contains("`shop::calc::quarter`: proved") && callers.contains("`shop::use_half`: proved"), "{callers}");
    // `impl TRAIT for *`: each method of each impl of the trait.
    let displays = claim_block(&text, "i6");
    assert!(displays.contains("1 item(s) of the scope `impl std::fmt::Display for *`") && displays.contains("`<shop::Cart as std::fmt::Display>::fmt`: claimed"), "{displays}");
    // A list: one claim for each item.
    assert!(claim_block(&text, "i7").contains("#[same(shop::calc::half)] fn two_items()") && claim_block(&text, "i8").contains("#[same(shop::use_half)] fn two_items()"), "{text}");
    // R80.3-2: the claim on `impl shop::Cart` is claimed (one item), and its proved items
    // explain their bodies; the claimed item does not.
    let section = semantic_section(&text);
    for item in ["new", "len", "is_empty", "push"] {
        assert!(section.contains(&format!("explained by claim i3: `shop::Cart::{item}`")), "{item}: {section}");
    }
    assert!(section.contains("not explained: `shop::Cart::first`"), "{section}");
    // An empty claim on an item depends on the two versions of the item (R77.1-1): the module
    // claim explains its changed items (`uses_hidden` does not change).
    assert!(section.contains("✓ stays the same: i4 `#[same(shop::calc::*)] fn calc_module()` (explains `shop::calc::half`, `shop::calc::quarter`)"), "{section}");
}

// rule- R80.3-1 R80.3-2
#[test]
fn a_scope_path_with_no_definition_is_an_error_and_a_claimed_item_explains_nothing() {
    let (_dir, repo) = setup();
    // R80.3-1 (negative): rustc resolves each path of a scope, so a type or a module that no
    // version has is an error of the claim (the step is not accepted).
    write(&repo, ".semilla/intent/pr.rs", "#[same(impl shop::Basket)]\nfn baskets() {}\n");
    let text = run(&repo, &["intent", "check", ".semilla/intent/pr.rs"], 1);
    assert!(text.contains("shop::Basket") && text.contains("error"), "{text}");
    write(&repo, ".semilla/intent/pr.rs", "#[same(shop::nothing::*)]\nfn nothing() {}\n");
    let text = run(&repo, &["intent", "check", ".semilla/intent/pr.rs"], 1);
    assert!(text.contains("nothing") && text.contains("error"), "{text}");
    // R80.3-2 (negative): with no relation, each method of the changed type is "claimed", so
    // the claim explains no body of the scope.
    write(&repo, ".semilla/intent/pr.rs", "#[same(impl shop::Cart)]\nfn cart_methods() {}\n");
    let text = run(&repo, &["step", "cache the length", "--intent", ".semilla/intent/pr.rs", "--compiled"], 0);
    let cart = claim_block(&text, "i1");
    assert!(cart.contains("claimed: 5 item(s) of the scope `impl shop::Cart`: 0 proved, 0 false, 5 claimed"), "{cart}");
    let section = semantic_section(&text);
    assert!(!section.contains("explained by claim i1"), "{section}");
}
