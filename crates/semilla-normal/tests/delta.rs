//! Delta builds: one fixed build folder, changed in place, with `run_cargo_check_delta`.
//!
//! A reused record of a stale crate would give a false proof, so this test checks that:
//! - an unchanged crate is not compiled again, and keeps its records;
//! - a changed crate gets new records;
//! - a crate that depends on a changed crate is compiled again;
//! - the records of a removed crate are removed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use semilla_normal::{read_records, run_cargo_check_delta};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn ensure_driver(scratch: &Path) {
    if semilla_normal::cargo::find_driver().is_ok() {
        return;
    }
    let target = scratch.join("driver-target");
    let status = Command::new("cargo")
        .args(["build", "-p", "semilla-mir", "--target-dir"])
        .arg(&target)
        .current_dir(semilla_normal::cargo::semilla_workspace())
        .status()
        .expect("cargo runs");
    assert!(status.success(), "cannot build the driver");
    // SAFETY: the test sets the variable before any other thread reads it.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", target.join("debug/semilla-mir-driver")) };
}

/// The records file of a crate, and its modification time.
fn record_file(out: &Path, crate_name: &str) -> Option<(PathBuf, SystemTime)> {
    std::fs::read_dir(out).unwrap().flatten().map(|e| e.path()).find(|p| {
        p.file_name().unwrap().to_string_lossy().starts_with(&format!("{crate_name}.lib"))
    }).map(|p| {
        let time = std::fs::metadata(&p).unwrap().modified().unwrap();
        (p, time)
    })
}

fn body_text(out: &Path, name: &str) -> String {
    read_records(out).unwrap().into_iter().find(|r| r.name == name).map(|r| r.text).unwrap_or_default()
}

#[test]
fn delta_builds_reuse_only_fresh_crates() {
    let scratch = tempfile::tempdir().unwrap();
    ensure_driver(scratch.path());
    let build = scratch.path().join("build");
    let out = scratch.path().join("records");
    let target = scratch.path().join("target");
    write(&build.join("Cargo.toml"), "[workspace]\nresolver = \"3\"\nmembers = [\"core_a\", \"app_b\"]\n");
    write(&build.join("core_a/Cargo.toml"), "[package]\nname = \"core_a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n");
    write(&build.join("core_a/src/lib.rs"), "pub fn helper(x: u32) -> u32 {\n    x + 1\n}\n");
    write(&build.join("app_b/Cargo.toml"), "[package]\nname = \"app_b\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\ncore_a = { path = \"../core_a\" }\n");
    write(&build.join("app_b/src/lib.rs"), "pub fn run(x: u32) -> u32 {\n    core_a::helper(x) * 2\n}\n");

    run_cargo_check_delta(&build, "n0", &out, &target).unwrap_or_else(|e| panic!("{e:#}"));
    let (core_file, core_time) = record_file(&out, "core_a").expect("core_a records");
    let (_, app_time) = record_file(&out, "app_b").expect("app_b records");

    // 1. Change app_b only: core_a is fresh, and keeps its record file unchanged.
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(&build.join("app_b/src/lib.rs"), "pub fn run(x: u32) -> u32 {\n    core_a::helper(x) * 3\n}\n");
    run_cargo_check_delta(&build, "n0", &out, &target).unwrap_or_else(|e| panic!("{e:#}"));
    let (core_file_2, core_time_2) = record_file(&out, "core_a").expect("core_a records stay");
    assert_eq!(core_file, core_file_2);
    assert_eq!(core_time, core_time_2, "a fresh crate is not compiled again");
    let (_, app_time_2) = record_file(&out, "app_b").expect("app_b records");
    assert!(app_time_2 > app_time, "the changed crate is compiled again");
    assert!(body_text(&out, "app_b::run").contains("u32 = 3"), "{}", body_text(&out, "app_b::run"));

    // 2. Change core_a: app_b depends on it, so both are compiled again.
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(&build.join("core_a/src/lib.rs"), "pub fn helper(x: u32) -> u32 {\n    x + 2\n}\n");
    run_cargo_check_delta(&build, "n0", &out, &target).unwrap_or_else(|e| panic!("{e:#}"));
    let (_, core_time_3) = record_file(&out, "core_a").expect("core_a records");
    let (_, app_time_3) = record_file(&out, "app_b").expect("app_b records");
    assert!(core_time_3 > core_time_2);
    assert!(app_time_3 > app_time_2, "a dependent crate is compiled again");
    assert!(body_text(&out, "core_a::helper").contains("u32 = 2"));

    // 3. Remove app_b: its records go.
    write(&build.join("Cargo.toml"), "[workspace]\nresolver = \"3\"\nmembers = [\"core_a\"]\n");
    std::fs::remove_dir_all(build.join("app_b")).unwrap();
    run_cargo_check_delta(&build, "n0", &out, &target).unwrap_or_else(|e| panic!("{e:#}"));
    assert!(record_file(&out, "app_b").is_none(), "the records of a removed crate are removed");
    assert!(record_file(&out, "core_a").is_some());
}
