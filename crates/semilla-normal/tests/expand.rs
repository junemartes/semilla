//! The expansion record of the driver (design 63, part A).
//!
//! One workspace has a library and a proc macro crate. The library has a `macro_rules!` macro
//! with two arms, a nested macro, a macro call in an impl block, a derive, an attribute macro
//! of the proc macro crate, a macro with `$crate`, and a macro whose local has the name of a
//! local of the caller. The test reads the expansion records of the library and runs the
//! driver two times.

use std::path::{Path, PathBuf};
use std::process::Command;

use semilla_normal::{read_expansions, read_records, run_cargo_check};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Makes sure that a driver exists, and returns its path.
fn ensure_driver(scratch: &Path) -> PathBuf {
    if let Ok(path) = semilla_normal::cargo::find_driver() {
        return path;
    }
    let target = scratch.join("driver-target");
    let status = Command::new("cargo").args(["build", "-p", "semilla-mir", "--target-dir"]).arg(&target).current_dir(semilla_normal::cargo::semilla_workspace()).status().expect("cargo runs");
    assert!(status.success(), "cannot build the driver");
    target.join("debug/semilla-mir-driver")
}

const MACROS: &str = r#"use proc_macro::TokenStream;

/// Adds a function `twice` after the item.
#[proc_macro_attribute]
pub fn twice(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut text = item.to_string();
    text.push_str(" pub fn twice(x: u32) -> u32 { x * 2 }");
    text.parse().unwrap()
}
"#;

const SOURCE: &str = r#"macro_rules! pick {
    (one $v:expr) => { $v + 1 };
    (two $v:expr) => { $v * 2 };
}

macro_rules! base { ($v:expr) => { $v + 10 } }
macro_rules! fee { ($v:expr) => { base!($v) * 2 } }

macro_rules! field_get { ($name:ident, $f:ident) => { pub fn $name(&self) -> u32 { self.$f } } }

#[macro_export]
macro_rules! make_zero { () => { $crate::zero() } }

macro_rules! shadow { ($e:expr) => {{ let x = 1; $e - x }} }

pub fn zero() -> u32 { 0 }

pub fn arms(v: u32) -> u32 { pick!(one v) + pick!(two v) }

pub fn price(v: u32) -> u32 { fee!(v) }

#[derive(PartialEq)]
pub struct Point { pub x: i32 }

pub struct Cart { sum: u32 }

impl Cart {
    field_get!(total, sum);
}

#[probe_macros::twice]
pub fn once(x: u32) -> u32 { x }

pub fn start() -> u32 { make_zero!() }

pub fn hygiene(x: u32) -> u32 { shadow!(x) }

pub fn text(v: u32) -> String { format!("{v}") }
"#;

/// The fields of one record that the test reads.
fn line(value: &serde_json::Value) -> String {
    let text = value["text"].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
    let uses: Vec<String> = value["uses"].as_array().into_iter().flatten().map(|pair| format!("{}@{}", pair[0].as_str().unwrap_or(""), pair[1].as_str().unwrap_or(""))).collect();
    let items: Vec<String> = value["items"].as_array().into_iter().flatten().map(|item| item.as_str().unwrap_or("").to_string()).collect();
    format!(
        "{}:{} {} {} [{}] {} `{}` outer={} uses={:?} items={:?}",
        value["file"].as_str().unwrap_or(""),
        value["line"],
        value["kind"].as_str().unwrap_or(""),
        value["name"].as_str().unwrap_or(""),
        value["def"].as_str().unwrap_or(""),
        value["form"].as_str().unwrap_or(""),
        text,
        value["outer"],
        uses,
        items
    )
}

// rule+ R63.2-1 R63.2-2 R63.2-3 R63.2-4 R63.2-5 R63.2-6
// rule- R63.2-3 R63.2-4 R63.2-5 R63.2-6
#[test]
fn each_macro_call_has_its_expansion_with_the_definition_the_items_and_the_chain() {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-normal-expand");
    let driver = ensure_driver(&scratch);
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
    let root = scratch.join("ws");
    let _ = std::fs::remove_dir_all(&root);
    write(&root.join("Cargo.toml"), "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nprobe-macros = { path = \"macros\" }\n\n[workspace]\nmembers = [\"macros\"]\n");
    write(&root.join("macros/Cargo.toml"), "[package]\nname = \"probe-macros\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n");
    write(&root.join("macros/src/lib.rs"), MACROS);
    write(&root.join("src/lib.rs"), SOURCE);
    let mut runs: Vec<Vec<String>> = Vec::new();
    for run in ["one", "two"] {
        let out = scratch.join(format!("records-{run}"));
        let _ = std::fs::remove_dir_all(&out);
        run_cargo_check(&root, "n0", &out, &scratch.join(format!("target-{run}"))).unwrap_or_else(|error| panic!("{error:#}"));
        let units = read_expansions(&out).expect("each unit has an expansion list");
        let unit = units.iter().find(|unit| unit.target.starts_with("probe.lib.")).expect("the library unit");
        let lines: Vec<String> = unit.list.iter().map(line).collect();
        let has = |part: &str| lines.iter().any(|line| line.contains(part));
        let all = lines.join("\n");
        // R63.2-1: a function-like macro with two arms gives one record for each call, with
        // the text of its arm.
        assert!(has("src/lib.rs:18 bang pick [src/lib.rs:1] expr `v + 1`"), "{all}");
        assert!(has("src/lib.rs:18 bang pick [src/lib.rs:1] expr `v * 2`"), "{all}");
        // R63.2-1: a nested macro: the outer call has the full text and the chain; the inner
        // call has a record with the outer call.
        assert!(has("src/lib.rs:20 bang fee [src/lib.rs:7] expr `(v + 10) * 2` outer=null uses=[\"base@src/lib.rs:6\"]"), "{all}");
        assert!(has("bang base [src/lib.rs:6] expr `v + 10` outer=") && !has("bang base [src/lib.rs:6] expr `v + 10` outer=null"), "{all}");
        // R63.2-1 and R63.2-6: a call in an impl block makes a method of the impl.
        assert!(has("src/lib.rs:28 bang field_get [src/lib.rs:9] assoc `pub fn total(&self) -> u32 { self.sum }` outer=null uses=[] items=[\"probe::Cart::total\"]"), "{all}");
        // R63.2-1 and R63.2-3: a derive of the standard library has a record with its impls.
        assert!(has("derive PartialEq [crate core] items `#[automatically_derived] impl ::core::marker::StructuralPartialEq for Point { }"), "{all}");
        assert!(has("items=[\"impl ::core::marker::StructuralPartialEq for Point\", \"impl ::core::cmp::PartialEq for Point\"]"), "{all}");
        // R63.2-1: an attribute macro of a proc macro crate of the workspace makes two items.
        assert!(has("attr probe_macros::twice [macros/src/lib.rs:5] items `pub fn once(x: u32) -> u32 { x } pub fn twice(x: u32) -> u32 { x * 2 }`"), "{all}");
        assert!(has("items=[\"probe::once\", \"probe::twice\"]"), "{all}");
        // R63.2-1: `$crate` is the crate.
        assert!(has("bang make_zero [src/lib.rs:12] expr `crate::zero()`"), "{all}");
        // R63.2-4: the local `x` of the macro and the parameter `x` of the caller have two
        // texts. The negative case: a name with one context has no mark (`v` above).
        assert!(has("bang shadow [src/lib.rs:14] expr `{ let x = 1; x\u{b7}1 - x }`") || has("bang shadow [src/lib.rs:14] expr `{ let x\u{b7}1 = 1; x - x\u{b7}1 }`"), "{all}");
        // R65.2-1 (it replaces R63.2-3): `format!` of the standard library has a record too, and
        // its inner `format_args!` is a record inside it.
        assert!(has("src/lib.rs:38 bang format [crate alloc] expr") && has("bang $crate::__export::format_args [crate core]"), "{all}");
        // R63.2-5: the check of the expanded `$v + 10` has the expanded text. The negative
        // case: a check whose tokens are in the source keeps the source text.
        let records = read_records(&out).unwrap();
        let graph = |name: &str| records.iter().find(|record| record.name == name && record.level == "n0").and_then(|record| record.graph.clone()).unwrap_or_else(|| panic!("no graph of {name}"));
        let texts: Vec<String> = graph("probe::price").p.iter().map(|point| point.t.clone()).collect();
        assert!(texts.contains(&"overflow: v + 10".to_string()) && texts.contains(&"overflow: (v + 10) * 2".to_string()), "{texts:?}");
        // The code of a procedural macro: each token has the span of the call, so a span has
        // two texts (`x` of `once` and `x * 2` of `twice`), and the point keeps its source text.
        let texts: Vec<String> = graph("probe::twice").p.iter().map(|point| point.t.clone()).collect();
        assert!(texts.iter().any(|text| text == "overflow: #[probe_macros::twice]"), "{texts:?}");
        runs.push(lines);
    }
    // R63.2-2: two runs give equal records.
    assert_eq!(runs[0], runs[1]);
}

/// R66.10-3: the units of a record folder have the order of their data (the package, the target
/// name with no metadata hash, the configuration, the root file), not the order of the file
/// names. Two packages with a test target `integration` keep their order when the metadata hash
/// of cargo changes. The negative case: two files with the same data keep the order of their
/// names (the only tie break).
// rule+ R66.10-3
// rule- R66.10-3
#[test]
fn the_units_of_a_record_folder_have_the_order_of_their_data_not_of_the_file_names() {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-unit-order");
    let unit = |package: &str, target: &str, cfg: &str| format!("{{\"unit\":{{\"target\":\"{target}\",\"package\":\"{package}\",\"root\":\"tests/lib.rs\",\"cfg\":[{cfg}],\"expansions\":[]}}}}\n");
    let order = |files: &[(&str, &str, &str)]| -> Vec<String> {
        let _ = std::fs::remove_dir_all(&scratch);
        for (package, stem, cfg) in files {
            write(&scratch.join(format!("{stem}.jsonl")), &unit(package, stem, cfg));
        }
        let units = read_expansions(&scratch).expect("the expansions of each unit");
        units.iter().map(|unit| format!("{} {} {}", unit.package, unit.target, unit.cfg.join(","))).collect()
    };
    // The hash of `parser` is first in the order of the names, then the hash of `lexer`.
    let first = order(&[("parser", "integration.test.0a1b", ""), ("lexer", "integration.test.f9e8", "")]);
    let second = order(&[("parser", "integration.test.f9e8", ""), ("lexer", "integration.test.0a1b", "")]);
    assert_eq!(first, vec!["lexer integration.test.f9e8 ", "parser integration.test.0a1b "]);
    assert_eq!(second, vec!["lexer integration.test.0a1b ", "parser integration.test.f9e8 "]);
    // One package and one target in two configurations: the configuration gives the order.
    let configs = order(&[("lexer", "integration.test.0a1b", "\"feature=\\\"b\\\"\""), ("lexer", "integration.test.f9e8", "\"feature=\\\"a\\\"\"")]);
    assert_eq!(configs, vec!["lexer integration.test.f9e8 feature=\"a\"", "lexer integration.test.0a1b feature=\"b\""]);
    // The negative case: the same data gives the order of the file names.
    let same = order(&[("lexer", "integration.test.f9e8", ""), ("lexer", "integration.test.0a1b", "")]);
    assert_eq!(same, vec!["lexer integration.test.0a1b ", "lexer integration.test.f9e8 "]);
    // The records of the bodies have the same order of the units.
    let names: Vec<String> = {
        let _ = std::fs::remove_dir_all(&scratch);
        for (package, stem) in [("parser", "integration.test.0a1b"), ("lexer", "integration.test.f9e8")] {
            let record = format!("{{\"name\":\"{package}::suite\",\"level\":\"n0\",\"text\":\"t\",\"shape\":\"s\"}}\n");
            write(&scratch.join(format!("{stem}.jsonl")), &format!("{}{record}", unit(package, stem, "")));
        }
        read_records(&scratch).expect("the records").into_iter().map(|record| record.name).collect()
    };
    assert_eq!(names, vec!["lexer::suite", "parser::suite"]);
    let _ = std::fs::remove_dir_all(&scratch);
}
