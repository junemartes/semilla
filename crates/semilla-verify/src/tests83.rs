//! Design 83: the rules of the syntax tree whose fixtures do not build. Semilla has one mode, so
//! a step of such a tree is rejected by its build. These tests call the library layer with no
//! step: the loader of `semilla-syntax` and the check of the facts, with no compiler data.

use crate::{NoOracle, StepInput, verify_step_with};
use semilla_core::{Category, Step};
use semilla_syntax::Workspace;

/// The manifest of each tree of these tests.
const MANIFEST: &str = "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

/// The macro of the tests: it gives its input as items.
const PICK: &str = "macro_rules! pick {\n    ($($t:tt)*) => { $($t)* };\n}\n";

/// Writes one tree: the manifest and each file.
fn tree(files: &[(&str, &str)]) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), MANIFEST).unwrap();
    for (path, text) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let ws = Workspace::load(dir.path()).unwrap();
    (dir, ws)
}

/// The facts of a step from `old` to `new`: the description, the category, and the spotlight mark
/// of each fact, and the errors of the step.
fn facts(old: &[(&str, &str)], new: &[(&str, &str)]) -> (Vec<(String, Category, bool)>, Vec<String>) {
    let (_a, before) = tree(old);
    let (_b, after) = tree(new);
    let step = Step { index: 0, intent: "the change".into(), claims: Vec::new(), tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default(), syntax_only: true, build_check: None, agent: Default::default(), tags: Vec::new(), configurations: None };
    let outcome = verify_step_with(&StepInput { step: &step, before: &before, after: &after }, &NoOracle, &NoOracle, true);
    (outcome.report.facts.iter().map(|fact| (fact.description.clone(), fact.category, fact.spotlight)).collect(), outcome.report.errors.clone())
}

/// The one fact of a file that the loader does not parse: `src/unix.rs` keeps its `file` fact and
/// its spotlight.
fn only_the_file_fact(lib: &str) {
    let (found, errors) = facts(&[("src/lib.rs", lib), ("src/unix.rs", "pub fn open() -> u32 { 3 }\n")], &[("src/lib.rs", lib), ("src/unix.rs", "pub fn open() -> u32 { 4 }\n")]);
    assert_eq!(found, [("src/unix.rs: changed".to_string(), Category::File, true)], "{lib}");
    assert!(errors.is_empty(), "{errors:?}");
}

// The case `macro_module/no_file` before design 83.
// rule+ R59.1-5
#[test]
fn a_mod_line_in_a_macro_input_with_no_file_gives_no_module_and_no_error() {
    let lib = |value: u32| format!("{PICK}pick! {{\n    mod gone;\n}}\npub fn run() -> u32 {{\n    {value}\n}}\n");
    let (found, errors) = facts(&[("src/lib.rs", &lib(1))], &[("src/lib.rs", &lib(2))]);
    assert_eq!(found, [("changed literal `1` → `2` at `depot::run/tail`".to_string(), Category::Semantics, false)]);
    assert!(errors.is_empty(), "{errors:?}");
}

// The case `macro_module/file_of_a_different_folder` before design 83: the line is in the file of
// the module `net`, so its file is `src/net/unix.rs`, and `src/unix.rs` keeps its `file` fact.
// rule- R59.1-2
#[test]
fn a_mod_line_in_the_file_of_a_module_names_a_file_below_its_folder() {
    let net = "pick! {\n    mod unix;\n}\n";
    let lib = format!("{PICK}pub mod net;\n");
    let old = [("src/lib.rs", lib.as_str()), ("src/net.rs", net), ("src/unix.rs", "pub fn open() -> u32 { 3 }\n")];
    let new = [("src/lib.rs", lib.as_str()), ("src/net.rs", net), ("src/unix.rs", "pub fn open() -> u32 { 4 }\n")];
    let (found, errors) = facts(&old, &new);
    assert_eq!(found, [("src/unix.rs: changed".to_string(), Category::File, true)]);
    assert!(errors.is_empty(), "{errors:?}");
}

// The case `macro_module/not_a_macro_variable` before design 83: `mod $unix;` has a macro
// variable, and a `macro_rules!` definition in the input is not a line.
// rule+ R59.1-6
#[test]
fn a_macro_variable_and_a_definition_in_a_macro_input_name_no_module_file() {
    only_the_file_fact(&format!("{PICK}pick! {{\n    mod $unix;\n    macro_rules! inner {{\n        () => {{ mod unix; }};\n    }}\n}}\n"));
}

// The case `macro_module/inline_block_not_top` before design 83: a line in the braces of
// `mod sys { … }` resolves below the folder `sys/`, so it does not name `src/unix.rs`.
// rule- R59.1-8
#[test]
fn a_line_in_an_inline_module_of_a_macro_input_names_no_top_file() {
    only_the_file_fact(&format!("{PICK}\npick! {{\n    mod sys {{\n        pub mod unix;\n    }}\n}}\n"));
}

// The case `macro_module/not_in_a_body` before design 83: a macro call in a body is not at an
// item position.
// rule- R59.1-1
// rule+ R59.1-6
#[test]
fn a_mod_line_in_a_macro_call_in_a_body_names_no_module_file() {
    only_the_file_fact(&format!("{PICK}\npub fn run() -> u32 {{\n    pick! {{\n        mod unix;\n    }}\n    3\n}}\n"));
}
