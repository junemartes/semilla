//! Spike S1: does N0 or N2 normalization make neutral refactors equal?
//!
//! Each case in `spikes/s1/cases/<name>/` has a `before.rs`, an `after.rs`, and a `case.toml`.
//! The harness runs the MIR driver on both files at each level and compares the bodies.
//!
//! - A **neutral** case passes if at least one level makes all bodies equal.
//! - A **changed** case (a negative control) passes only if no level makes all bodies equal.
//!   A failed control shows an unsound normalization rule.
//!
//! Build the driver first: `cargo build -p semilla-mir`. Then run: `cargo run -p semilla-s1`.
//! The canonical MIR of each case goes to `target/s1/<case>/<level>/`, for a manual diff.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const LEVELS: [&str; 2] = ["n0", "n2"];

#[derive(serde::Deserialize)]
struct Case {
    /// `neutral` or `changed`.
    expect: Expect,
    /// What the case tests.
    note: String,
    /// The level that the design expects to prove a neutral case (for the report only).
    #[serde(default)]
    needs: Option<String>,
    /// Renamed items: the "before" name to the "after" name.
    #[serde(default)]
    rename: BTreeMap<String, String>,
    /// New items that the claim declares, for example the new function of `extract-function`.
    /// They can exist on the "after" side only.
    #[serde(default)]
    added: Vec<String>,
}

#[derive(serde::Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Expect {
    Neutral,
    Changed,
}

#[derive(serde::Deserialize)]
struct BodyRecord {
    name: String,
    text: String,
}

/// The result of one level for one case.
struct LevelResult {
    equal: bool,
    /// The bodies that are different, or that exist on one side only.
    differences: Vec<String>,
}

fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = root.join("../..");
    let driver = workspace.join("target/debug/semilla-mir-driver");
    if !driver.exists() {
        eprintln!("The driver is missing. Run `cargo build -p semilla-mir` first.");
        return ExitCode::FAILURE;
    }
    let out_root = workspace.join("target/s1");

    let mut case_dirs: Vec<PathBuf> = std::fs::read_dir(root.join("cases"))
        .expect("the cases folder exists")
        .map(|entry| entry.expect("a readable folder entry").path())
        .filter(|path| path.is_dir())
        .collect();
    case_dirs.sort();

    println!("| case | expect | n0 | n2 | design level | result | note |");
    println!("|---|---|---|---|---|---|---|");
    let mut failures = 0;
    for dir in &case_dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let case: Case = toml::from_str(
            &std::fs::read_to_string(dir.join("case.toml")).expect("each case has a case.toml"),
        )
        .unwrap_or_else(|error| panic!("{name}/case.toml: {error}"));

        let results: Vec<LevelResult> = LEVELS
            .iter()
            .map(|level| run_level(&driver, dir, &out_root.join(&name).join(level), level, &case))
            .collect();

        let any_equal = results.iter().any(|result| result.equal);
        let passed = match case.expect {
            Expect::Neutral => any_equal,
            Expect::Changed => !any_equal,
        };
        let verdict = match (case.expect, passed) {
            (Expect::Neutral, true) => "proved",
            (Expect::Neutral, false) => "not proved",
            (Expect::Changed, true) => "ok",
            (Expect::Changed, false) => "**UNSOUND**",
        };
        if case.expect == Expect::Changed && !passed {
            failures += 1;
        }

        let cells: Vec<String> = results.iter().map(describe).collect();
        println!(
            "| {name} | {} | {} | {} | {} | {verdict} | {} |",
            match case.expect {
                Expect::Neutral => "neutral",
                Expect::Changed => "changed",
            },
            cells[0],
            cells[1],
            case.needs.as_deref().unwrap_or("-"),
            case.note,
        );
    }

    if failures > 0 {
        eprintln!("{failures} negative control(s) became equal: a normalization rule is unsound.");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn describe(result: &LevelResult) -> String {
    if result.equal {
        "equal".to_string()
    } else {
        format!("diff: {}", result.differences.join(", "))
    }
}

/// Runs one level on both sides of a case, and compares the bodies.
fn run_level(driver: &Path, dir: &Path, out_dir: &Path, level: &str, case: &Case) -> LevelResult {
    let before = run_driver(driver, &dir.join("before.rs"), level);
    let after = run_driver(driver, &dir.join("after.rs"), level);

    // Rename the "before" bodies, so that they match the "after" names.
    let before: BTreeMap<String, String> = before
        .into_iter()
        .map(|(name, text)| (rename(&name, &case.rename), text))
        .collect();

    write_bodies(&out_dir.join("before"), &before);
    write_bodies(&out_dir.join("after"), &after);

    let mut differences = Vec::new();
    for (name, text) in &before {
        match after.get(name) {
            Some(other) if other == text => {}
            Some(_) => differences.push(short(name)),
            None => differences.push(format!("{} (before only)", short(name))),
        }
    }
    for name in after.keys() {
        if !before.contains_key(name) && !case.added.contains(&short(name)) {
            differences.push(format!("{} (after only)", short(name)));
        }
    }
    LevelResult { equal: differences.is_empty(), differences }
}

/// Runs the driver on one file. Returns the canonical text of each body, by its name.
fn run_driver(driver: &Path, file: &Path, level: &str) -> BTreeMap<String, String> {
    let output = Command::new(driver)
        .args(["--level", level, "--"])
        .args(["--crate-type", "lib", "--edition", "2024", "--crate-name", "s1case"])
        .args(["-A", "warnings"])
        .arg(file)
        .env("LD_LIBRARY_PATH", sysroot_lib())
        .output()
        .expect("the driver starts");
    if !output.status.success() {
        panic!(
            "the driver failed on {}:\n{}",
            file.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout)
        .expect("the driver output is UTF-8")
        .lines()
        .map(|line| {
            let record: BodyRecord = serde_json::from_str(line).expect("each line is a record");
            (record.name, record.text)
        })
        .collect()
}

/// The folder that contains the compiler's shared libraries. The driver links to them.
fn sysroot_lib() -> PathBuf {
    let output = Command::new("rustc").args(["--print", "sysroot"]).output().expect("rustc runs");
    let sysroot = String::from_utf8(output.stdout).expect("the sysroot path is UTF-8");
    PathBuf::from(sysroot.trim()).join("lib")
}

/// Applies the rename map to the last segment of a body name.
fn rename(name: &str, map: &BTreeMap<String, String>) -> String {
    let (prefix, last) = name.rsplit_once("::").unwrap_or(("", name));
    match map.get(last) {
        Some(new) if prefix.is_empty() => new.clone(),
        Some(new) => format!("{prefix}::{new}"),
        None => name.to_string(),
    }
}

/// Removes the crate name from a body name.
fn short(name: &str) -> String {
    name.strip_prefix("s1case::").unwrap_or(name).to_string()
}

fn write_bodies(dir: &Path, bodies: &BTreeMap<String, String>) {
    std::fs::create_dir_all(dir).expect("the output folder can be created");
    for (name, text) in bodies {
        let file = dir.join(format!("{}.mir", short(name).replace("::", "__")));
        std::fs::write(file, text).expect("the output file can be written");
    }
}
