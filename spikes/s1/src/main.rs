//! Spike S1: does the normal form ladder make neutral refactors equal, without making real
//! changes equal?
//!
//! Each case in `spikes/s1/cases/<name>/` has a `before.rs`, an `after.rs`, and a `case.toml`.
//! The harness runs the MIR driver on both files at the driver levels (n0, n2, n3), and compares
//! the bodies with `semilla_normal::compare`, which tries N0, N1, N2, N3, and N4 in order.
//!
//! - A **neutral** case passes ("proved") if all bodies are equal at some level.
//! - A **changed** case (a negative control) passes ("ok") only if a body stays different at all
//!   levels. A failed control shows an unsound normalization or rewrite rule.
//!
//! The `rename` table and the `added` list of a case are the claims: the harness gives them to
//! `compare` as they are.
//!
//! Build the driver first: `cargo build -p semilla-mir`. Then run: `cargo run -p semilla-s1`.
//! The canonical MIR and the terms of each case go to `target/s1/<case>/<level>/`, for a manual
//! diff.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use semilla_normal::{BodyComparison, BodyRecord, BodyStatus, Levels};

/// The levels of the driver.
const DRIVER_LEVELS: [&str; 3] = ["n0", "n2", "n3"];
/// The levels of the ladder, in order.
const LADDER: [&str; 5] = ["n0", "n1", "n2", "n3", "n4"];

#[derive(serde::Deserialize)]
struct Case {
    /// `neutral` or `changed`.
    expect: Expect,
    /// What the case tests.
    note: String,
    /// The level that the design expects to prove a neutral case (for the report only).
    #[serde(default)]
    needs: Option<String>,
    /// The rename claims: full "before" def path to full "after" def path.
    #[serde(default)]
    rename: BTreeMap<String, String>,
    /// New bodies that the claims declare (full def paths). They can exist on the "after" side
    /// only.
    #[serde(default)]
    added: Vec<String>,
}

#[derive(serde::Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Expect {
    Neutral,
    Changed,
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
    let lib = sysroot_lib();

    let mut case_dirs: Vec<PathBuf> = std::fs::read_dir(root.join("cases"))
        .expect("the cases folder exists")
        .map(|entry| entry.expect("a readable folder entry").path())
        .filter(|path| path.is_dir())
        .collect();
    case_dirs.sort();

    println!(
        "| case | expect | n0 | n1 | n2 | n3 | n4 | design level | proved at | result | note |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    let mut failures = 0;
    let (mut neutral, mut proved) = (0, 0);
    for dir in &case_dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let case: Case = toml::from_str(
            &std::fs::read_to_string(dir.join("case.toml")).expect("each case has a case.toml"),
        )
        .unwrap_or_else(|error| panic!("{name}/case.toml: {error}"));

        let before = run_levels(&driver, &lib, &dir.join("before.rs"));
        let after = run_levels(&driver, &lib, &dir.join("after.rs"));
        write_bodies(&out_root.join(&name), "before", &before);
        write_bodies(&out_root.join(&name), "after", &after);

        let results = semilla_normal::compare(&before, &after, &case.rename, &case.added);
        let all_equal =
            results.iter().all(|result| matches!(result.status, BodyStatus::Equal { .. }));
        let passed = match case.expect {
            Expect::Neutral => all_equal,
            Expect::Changed => !all_equal,
        };
        let verdict = match (case.expect, passed) {
            (Expect::Neutral, true) => "proved",
            (Expect::Neutral, false) => "not proved",
            (Expect::Changed, true) => "ok",
            (Expect::Changed, false) => "**UNSOUND**",
        };
        if case.expect == Expect::Neutral {
            neutral += 1;
            proved += usize::from(passed);
        }
        if case.expect == Expect::Changed && !passed {
            failures += 1;
        }

        let cells: Vec<String> = LADDER.iter().map(|level| cell(&results, level)).collect();
        println!(
            "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {verdict} | {} |",
            match case.expect {
                Expect::Neutral => "neutral",
                Expect::Changed => "changed",
            },
            cells[0],
            cells[1],
            cells[2],
            cells[3],
            cells[4],
            case.needs.as_deref().unwrap_or("-"),
            proved_at(&results),
            case.note,
        );
    }
    println!();
    println!(
        "Summary: {proved} of {neutral} neutral cases are proved. {failures} negative control(s) failed."
    );

    if failures > 0 {
        eprintln!(
            "{failures} negative control(s) became equal: a normalization or rewrite rule is unsound."
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// The index of a ladder level.
fn rank(level: &str) -> usize {
    LADDER.iter().position(|item| *item == level).expect("a ladder level")
}

/// "equal" if all bodies are equal at this level or a lower one. Else the bodies that are not.
fn cell(results: &[BodyComparison], level: &str) -> String {
    let open: Vec<String> = results
        .iter()
        .filter_map(|result| match &result.status {
            BodyStatus::Equal { level: found, .. } if rank(found) <= rank(level) => None,
            BodyStatus::BeforeOnly => Some(format!("{} (before only)", short(&result.name))),
            BodyStatus::AfterOnly => Some(format!("{} (after only)", short(&result.name))),
            _ => Some(short(&result.name)),
        })
        .collect();
    if open.is_empty() { "equal".to_string() } else { format!("diff: {}", open.join(", ")) }
}

/// The highest level that the proof needs, with the N4 rules.
fn proved_at(results: &[BodyComparison]) -> String {
    let mut highest: Option<(&str, &[String])> = None;
    for result in results {
        match &result.status {
            BodyStatus::Equal { level, proof } => {
                if highest.is_none_or(|(current, _)| rank(level) > rank(current)) {
                    highest = Some((level, proof));
                }
            }
            _ => return "-".to_string(),
        }
    }
    match highest {
        Some((level, proof)) if !proof.is_empty() => {
            format!("{} ({})", level.to_uppercase(), proof.join(", "))
        }
        Some((level, _)) => level.to_uppercase(),
        None => "-".to_string(),
    }
}

/// Runs the driver on one file at each driver level.
fn run_levels(driver: &Path, lib: &Path, file: &Path) -> Levels {
    DRIVER_LEVELS
        .iter()
        .map(|level| (level.to_string(), run_driver(driver, lib, file, level)))
        .collect()
}

/// Runs the driver on one file. Returns the records of all bodies.
fn run_driver(driver: &Path, lib: &Path, file: &Path, level: &str) -> Vec<BodyRecord> {
    let output = Command::new(driver)
        .args(["--level", level, "--"])
        .args(["--crate-type", "lib", "--edition", "2024", "--crate-name", "s1case"])
        .args(["-A", "warnings"])
        .arg(file)
        .env("LD_LIBRARY_PATH", lib)
        .output()
        .expect("the driver starts");
    if !output.status.success() {
        panic!(
            "the driver failed on {}:\n{}",
            file.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let text = String::from_utf8(output.stdout).expect("the driver output is UTF-8");
    semilla_normal::parse_records(&text).expect("the driver output is a list of records")
}

/// The folder that contains the compiler's shared libraries. The driver links to them.
fn sysroot_lib() -> PathBuf {
    let output = Command::new("rustc").args(["--print", "sysroot"]).output().expect("rustc runs");
    let sysroot = String::from_utf8(output.stdout).expect("the sysroot path is UTF-8");
    PathBuf::from(sysroot.trim()).join("lib")
}

/// Removes the crate name from a body name.
fn short(name: &str) -> String {
    name.strip_prefix("s1case::").unwrap_or(name).to_string()
}

/// Writes the text (`.mir`) and the term (`.term`) of each body to `<dir>/<level>/<side>/`.
fn write_bodies(dir: &Path, side: &str, levels: &Levels) {
    for (level, records) in levels {
        let folder = dir.join(level).join(side);
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).expect("the output folder can be created");
        for record in records {
            let stem = short(&record.name).replace("::", "__");
            std::fs::write(folder.join(format!("{stem}.mir")), &record.text)
                .expect("the output file can be written");
            if let Some(term) = &record.term {
                std::fs::write(folder.join(format!("{stem}.term")), term)
                    .expect("the output file can be written");
            }
        }
    }
}
