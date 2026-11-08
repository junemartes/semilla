//! Builds two versions of a small 2-crate cargo workspace, runs `run_cargo_check` at each driver
//! level, and compares the versions.
//!
//! The test needs the driver. If `target/debug/semilla-mir-driver` is missing, it builds the
//! driver in a separate target folder (the build lock of the outer `cargo test` is still held).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use semilla_normal::{
    BodyStatus, Levels, compare, group_levels, read_records, run_cargo_check, suggest_renames,
};

const WORKSPACE: &str = r#"[workspace]
resolver = "3"
members = ["core_a", "app_b"]
"#;

const CORE_TOML: &str = r#"[package]
name = "core_a"
version = "0.1.0"
edition = "2024"
"#;

const APP_TOML: &str = r#"[package]
name = "app_b"
version = "0.1.0"
edition = "2024"

[dependencies]
core_a = { path = "../core_a" }
"#;

const CORE_V1: &str = r#"
pub fn helper(x: u32) -> u32 {
    x + 1
}

pub struct Cart {
    pub items: Vec<u32>,
}

impl Cart {
    pub fn total(&self) -> u32 {
        let mut sum = 0;
        for item in &self.items {
            sum += helper(*item);
        }
        sum
    }
}
"#;

const CORE_V2: &str = r#"
pub fn add_one(x: u32) -> u32 {
    x + 1
}

pub struct Cart {
    pub items: Vec<u32>,
}

impl Cart {
    pub fn total(&self) -> u32 {
        let mut sum = 0;
        for item in &self.items {
            sum += add_one(*item);
        }
        sum
    }
}
"#;

const APP_V1: &str = r#"
pub fn uses_helper(x: u32) -> u32 {
    core_a::helper(x) * 2
}

pub fn get(v: &[u32], i: usize) -> Option<u32> {
    if i < v.len() { Some(v[i]) } else { None }
}

pub fn price(x: u32) -> u32 {
    x + 10
}

#[cfg(test)]
mod tests {
    #[test]
    fn price_adds() {
        assert_eq!(super::price(1), 11);
    }
}
"#;

const APP_V2: &str = r#"
pub fn uses_helper(x: u32) -> u32 {
    core_a::add_one(x) * 2
}

pub fn get(v: &[u32], i: usize) -> Option<u32> {
    if i >= v.len() {
        return None;
    }
    Some(v[i])
}

pub fn price(x: u32) -> u32 {
    x + 11
}

#[cfg(test)]
mod tests {
    #[test]
    fn price_adds() {
        assert_eq!(super::price(1), 12);
    }
}
"#;

const APP_MAIN: &str = r#"
fn main() {
    println!("{}", app_b::price(1));
}
"#;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn make_workspace(root: &Path, core: &str, app: &str) {
    let _ = std::fs::remove_dir_all(root);
    write(&root.join("Cargo.toml"), WORKSPACE);
    write(&root.join("core_a/Cargo.toml"), CORE_TOML);
    write(&root.join("core_a/src/lib.rs"), core);
    write(&root.join("app_b/Cargo.toml"), APP_TOML);
    write(&root.join("app_b/src/lib.rs"), app);
    write(&root.join("app_b/src/main.rs"), APP_MAIN);
}

/// Makes sure that a driver exists, and returns its path.
fn ensure_driver(scratch: &Path) -> PathBuf {
    if let Ok(path) = semilla_normal::cargo::find_driver() {
        return path;
    }
    let target = scratch.join("driver-target");
    let status = Command::new("cargo")
        .args(["build", "-p", "semilla-mir", "--target-dir"])
        .arg(&target)
        .current_dir(semilla_normal::cargo::semilla_workspace())
        .status()
        .expect("cargo runs");
    assert!(status.success(), "cannot build the driver");
    target.join("debug/semilla-mir-driver")
}

fn check_all_levels(workspace: &Path, out: &Path, target: &Path) -> Levels {
    let mut records = Vec::new();
    for level in ["n0", "n2", "n3"] {
        let dir = out.join(level);
        let _ = std::fs::remove_dir_all(&dir);
        run_cargo_check(workspace, level, &dir, target).unwrap_or_else(|error| panic!("{error:#}"));
        let level_records = read_records(&dir).unwrap();
        assert!(!level_records.is_empty(), "no records at {level}");
        records.extend(level_records);
    }
    group_levels(records)
}

fn status<'a>(results: &'a [semilla_normal::BodyComparison], name: &str) -> &'a BodyStatus {
    &results
        .iter()
        .find(|result| result.name == name)
        .unwrap_or_else(|| panic!("no result for {name}: {results:#?}"))
        .status
}

#[test]
fn two_versions_of_a_two_crate_workspace() {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-normal-cargo-test");
    let driver = ensure_driver(&scratch);
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
    let target = scratch.join("target");

    let v1 = scratch.join("v1");
    make_workspace(&v1, CORE_V1, APP_V1);
    let before = check_all_levels(&v1, &scratch.join("out-v1"), &target);

    // The records of each target go to their own file: lib, bin, and test of `app_b`.
    let files: Vec<String> = std::fs::read_dir(scratch.join("out-v1/n0"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    for prefix in ["core_a.lib.", "app_b.lib.", "app_b.bin.", "app_b.test."] {
        assert!(
            files.iter().any(|file| file.starts_with(prefix)),
            "no {prefix}* file in {files:?}"
        );
    }
    let first = before["n0"].iter().find(|record| record.name == "app_b::get").expect("app_b::get");
    let span = first.span.as_ref().expect("a span");
    assert!(span.file.ends_with("app_b/src/lib.rs"), "{span:?}");
    assert!(span.line_start < span.line_end);
    assert!(first.loop_free && first.term.is_none(), "N0 records have no term");
    assert!(first.target.starts_with("app_b.lib."), "{}", first.target);
    let n2 =
        before["n2"].iter().find(|record| record.name == "app_b::get").expect("app_b::get at n2");
    assert!(n2.term.is_some());
    // N3 records exist only for bodies with an inlined call (`Cart::total` calls `helper`).
    assert!(before["n3"].iter().any(|record| record.name == "core_a::Cart::total"));
    assert!(!before["n3"].iter().any(|record| record.name == "app_b::get"));

    // A second run of the same version gives the same records (cargo compiles the crates again).
    let again = check_all_levels(&v1, &scratch.join("out-v1-again"), &target);
    let texts = |levels: &Levels| {
        let mut texts: Vec<(String, String)> =
            levels["n0"].iter().map(|record| (record.name.clone(), record.text.clone())).collect();
        texts.sort();
        texts
    };
    assert_eq!(texts(&before), texts(&again));

    let v2 = scratch.join("v2");
    make_workspace(&v2, CORE_V2, APP_V2);
    let after = check_all_levels(&v2, &scratch.join("out-v2"), &target);

    let renames = BTreeMap::from([("core_a::helper".to_string(), "core_a::add_one".to_string())]);
    let results = compare(&before, &after, &renames, &[]);

    // The renamed function itself has the same body.
    assert!(
        matches!(status(&results, "core_a::add_one"), BodyStatus::Equal { level, .. } if level == "n0")
    );
    // The callers differ only by the claimed rename (inside the crate and across crates).
    assert!(
        matches!(status(&results, "core_a::Cart::total"), BodyStatus::Equal { level, .. } if level == "n1")
    );
    assert!(
        matches!(status(&results, "app_b::uses_helper"), BodyStatus::Equal { level, .. } if level == "n1")
    );
    // The guard clause needs N4.
    match status(&results, "app_b::get") {
        BodyStatus::Equal { level, proof } => {
            assert_eq!(level, "n4");
            assert!(proof.contains(&"ite-not".to_string()), "{proof:?}");
        }
        other => panic!("app_b::get: {other:?}"),
    }
    // A real change stays different. The control flow is the same.
    assert_eq!(status(&results, "app_b::price"), &BodyStatus::Different { cfg_changed: false });
    assert!(matches!(status(&results, "app_b::tests::price_adds"), BodyStatus::Different { .. }));
    // The binary did not change.
    assert!(
        matches!(status(&results, "app_b::main"), BodyStatus::Equal { level, .. } if level == "n0")
    );

    // Without the claim, the renamed function is uncovered, and a hint finds it.
    let results = compare(&before, &after, &BTreeMap::new(), &[]);
    assert_eq!(status(&results, "core_a::helper"), &BodyStatus::BeforeOnly);
    assert_eq!(status(&results, "core_a::add_one"), &BodyStatus::AfterOnly);
    assert!(
        suggest_renames(&before, &after)
            .contains(&("core_a::helper".to_string(), "core_a::add_one".to_string())),
        "{:?}",
        suggest_renames(&before, &after)
    );
}
