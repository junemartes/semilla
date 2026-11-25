//! The build check of a step (design 53).
//!
//! Each command that records a step runs `cargo check` on the new tree of the step. A step
//! that does not build is not stored. `--no-check` and `SEMILLA_NO_CHECK` turn the check off,
//! and the step record keeps `not run`.
//!
//! The check is not the compiled check (design 50): it makes no fact and no compare result.

use anyhow::{Context, Result};
use semilla_core::BuildCheck;
use semilla_normal::cargo::{BuildRun, CheckRecordFiles, CheckRecords, CheckToolchain, NO_UNIT, TreeToolchain, UnitState};

use crate::app::App;

/// The number of compiler errors that a message shows.
const ERROR_MAX: usize = 5;

/// True if the environment turns the build check off (`SEMILLA_NO_CHECK`, design 53). An
/// empty value and `0` do not turn it off.
pub fn no_check_env() -> bool {
    std::env::var("SEMILLA_NO_CHECK").is_ok_and(|value| !value.is_empty() && value != "0")
}

/// How the author turns the build check off for a command: the option, or the variable.
pub fn skip_cause(option: bool) -> Option<&'static str> {
    if option {
        Some("`--no-check`")
    } else if no_check_env() {
        Some("`SEMILLA_NO_CHECK`")
    } else {
        None
    }
}

/// The result of the build check for the command that records a step.
pub(crate) enum Outcome {
    /// The command can store the step. `lines` are the lines of the step output.
    Stored { record: BuildCheck, lines: Vec<String> },
    /// A target that builds before the step does not build after it. `text` has the result
    /// line and the errors of the compiler. `record` is the record of a tag after the commit
    /// (design 62): the targets and the first errors of the compiler.
    Fails { text: String, record: BuildCheck },
    /// The check cannot run on this machine. This is not a result of the code. `text` has
    /// the result line and the cause.
    CannotRun { text: String },
}

/// A short hash of a text, for a folder name.
fn text_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish() & 0xffff_ffff_ffff
}

/// Keeps the expansion records of a tree (R65.8-1): a copy of the record file of each unit of
/// the build in the folder of the tree, or a file with the cause why the build has no records.
/// A failed write only costs time: the next check of the tree builds again.
fn store_records(setup: &std::result::Result<(CheckRecords, std::path::PathBuf), String>, files: Option<&CheckRecordFiles>, run: &BuildRun, out: &std::path::Path) {
    let store = match setup {
        Ok((_, store)) => store,
        Err(_) => return,
    };
    let _ = std::fs::create_dir_all(store.parent().unwrap_or(store));
    let cause = match files {
        Some(CheckRecordFiles::Files(names)) => {
            let temporary = store.with_extension(format!("tmp{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&temporary);
            let copied = std::fs::create_dir_all(&temporary).is_ok() && names.iter().all(|name| std::fs::copy(out.join(name), temporary.join(name)).is_ok());
            if copied && std::fs::rename(&temporary, store).is_ok() {
                let _ = std::fs::remove_file(store.with_extension("none"));
                return;
            }
            let _ = std::fs::remove_dir_all(&temporary);
            return;
        }
        _ if !run.failed.is_empty() => targets_text(&run.failed, "does not build in this tree, so the tree has no full set of expansion records", "do not build in this tree, so the tree has no full set of expansion records"),
        _ => "the build gave no expansion records".to_string(),
    };
    let _ = std::fs::write(store.with_extension("none"), cause);
}

/// The time of a check, for a result line.
fn seconds(started: std::time::Instant) -> String {
    format!("{:.1} s", started.elapsed().as_secs_f64())
}

/// "the target A" or "the targets A, B", with the verb.
fn targets_text(targets: &[String], one: &str, many: &str) -> String {
    if targets.len() == 1 { format!("the target {} {one}", targets[0]) } else { format!("the targets {} {many}", targets.join(", ")) }
}

/// The text of a failed build check: the result line, then the first errors of the compiler.
/// The errors of the targets in `added` come first: these targets build before the step.
/// `excused` are the other targets that do not build before the step too. `not_checked` are the
/// other targets that cargo did not check before the step.
fn fail_text(run: &BuildRun, added: &[String], excused: &[String], not_checked: &[String], time: &str) -> String {
    let mut text = format!("  ✗ build check: fails ({time}): after the step, {}\n", targets_text(added, "does not build", "do not build"));
    if !excused.is_empty() {
        text.push_str(&format!("    (not a cause of this result: {} before the step too)\n", targets_text(excused, "does not build", "do not build")));
    }
    if !not_checked.is_empty() {
        text.push_str(&format!("    (not a cause of this result: {} after the step, and cargo did not check {} before the step)\n", targets_text(not_checked, "does not build", "do not build"), if not_checked.len() == 1 { "it" } else { "them" }));
    }
    let all = step_errors(run, added);
    // Each error is a block of four spaces, below the result line.
    let indented = |block: &str| format!("\n    {}\n", semilla_core::text::block_indent(block.trim_end(), "    "));
    for error in all.iter().take(ERROR_MAX) {
        text.push_str(&indented(error));
    }
    if all.len() > ERROR_MAX {
        text.push_str(&format!("\n    ({} more errors of the compiler; `cargo check --all-targets` shows each one)\n", all.len() - ERROR_MAX));
    }
    if !run.other.is_empty() && (all.is_empty() || added.iter().any(|unit| unit == NO_UNIT || unit.ends_with("(build script run)"))) {
        text.push_str(&indented(&run.other));
    }
    text
}

/// The errors of the compiler that a step makes: the errors of the targets in `added`. Cargo
/// gives no error of the compiler for some failures (a manifest, a build script that stops).
/// Then each error of the run is in the list.
fn step_errors<'a>(run: &'a BuildRun, added: &[String]) -> Vec<&'a str> {
    let own: Vec<&str> = run.errors.iter().filter(|error| added.iter().any(|unit| error.in_unit(unit))).map(|error| error.text.as_str()).collect();
    let mut all: Vec<&str> = if own.is_empty() { run.errors.iter().map(|error| error.text.as_str()).collect() } else { own };
    all.dedup();
    all
}

/// The first errors of the compiler that a step makes, for the record of a tag after the
/// commit (design 62). A failure with no error of the compiler keeps the lines of cargo.
fn first_errors(run: &BuildRun, added: &[String]) -> Vec<String> {
    let mut errors: Vec<String> = step_errors(run, added).into_iter().take(ERROR_MAX).map(|error| error.trim_end().to_string()).collect();
    if errors.is_empty() && !run.other.is_empty() {
        errors.push(run.other.trim_end().to_string());
    }
    errors
}

/// What a tool chain file names: the key `channel` or the key `path` of the table `toolchain`.
/// The file `rust-toolchain` can also have the old form: one line with the name.
fn toolchain_request(file: &str, text: &str) -> Result<TreeToolchain> {
    let table = text.parse::<toml::Table>().ok();
    let word = |key: &str| table.as_ref()?.get("toolchain")?.get(key)?.as_str().map(str::to_string);
    if let Some(channel) = word("channel") {
        return Ok(TreeToolchain::Channel(channel));
    }
    if let Some(path) = word("path") {
        return Ok(TreeToolchain::Path(path.into()));
    }
    let line = text.trim();
    if file == "rust-toolchain" && table.is_none() && !line.is_empty() && !line.contains(char::is_whitespace) {
        return Ok(TreeToolchain::Channel(line.to_string()));
    }
    anyhow::bail!("the file `{file}` of the tree names no toolchain (no `channel` and no `path` in the table `toolchain`)")
}

/// The lines of an accepted result of the build check: the result line, then one line with a
/// mark for each thing that Semilla did not check. Only a tree in which each target builds
/// has the word `passes`.
fn result_lines(record: &BuildCheck, base_failed: &[String], time: &str) -> Vec<String> {
    let mut lines = vec![format!("build check: {} ({time})", record.text())];
    lines.extend(record.limits(base_failed).into_iter().map(|limit| format!("  ! build check: {limit}")));
    lines
}

impl App {
    /// The folder of the build check in the Semilla cache: the build folder, the target folder
    /// of cargo, and the results by tree. Only a command that records a step uses it.
    pub(crate) fn check_folder(&self) -> std::path::PathBuf {
        self.repo.cache_dir().join("check")
    }

    /// The tool chain of the build check of a tree (design 53.2): the tool chain that cargo
    /// uses for the author in this tree. The file `rust-toolchain` or `rust-toolchain.toml` at
    /// the root of the tree names it (rustup reads `rust-toolchain` first). Else it is the
    /// default of the machine.
    fn check_toolchain(&self, tree: &str) -> Result<CheckToolchain> {
        for file in ["rust-toolchain", "rust-toolchain.toml"] {
            if let Ok(text) = self.repo.git(&["show", &format!("{tree}:{file}")]) {
                return semilla_normal::cargo::check_toolchain(Some((&toolchain_request(file, &text)?, file)));
            }
        }
        semilla_normal::cargo::check_toolchain(None)
    }

    /// The expansion records of the build check of a tree (R65.8-1): the driver of Semilla, the
    /// records folder of the build folder, and the folder of the records of the tree. `Err`: the
    /// cause why the build check of this tree cannot write the records.
    fn records_setup(&self, toolchain: &CheckToolchain, key: &str, tree: &str) -> std::result::Result<(CheckRecords, std::path::PathBuf), String> {
        if !semilla_normal::cargo::driver_toolchain(toolchain) {
            return Err(format!(
                "the build check uses the toolchain {}, and the driver of Semilla that writes the expansion records needs the toolchain `{}`; `--compiled` or `semilla expand` builds them",
                toolchain.name,
                semilla_normal::cargo::toolchain()
            ));
        }
        let driver = semilla_normal::cargo::find_driver().map_err(|error| format!("{error:#}"))?;
        let fingerprint = semilla_normal::cargo::driver_fingerprint(&driver).map_err(|error| format!("cannot read the driver of Semilla: {error:#}"))?;
        let folder = self.check_folder();
        let store = folder.join("expansions").join(format!("{tree}-{key}-{:012x}", text_hash(&fingerprint)));
        Ok((CheckRecords { driver, out: folder.join("records") }, store))
    }

    /// The expansion records that the build check of a tree wrote (R65.8-1), with no build. `Err`
    /// has the cause why the cache has no records of the tree.
    pub(crate) fn check_expansions(&self, tree: &str) -> std::result::Result<Vec<semilla_verify::expand::ExpansionUnit>, String> {
        let toolchain = self.check_toolchain(tree).map_err(|error| format!("{error:#}"))?;
        let key = semilla_normal::cargo::check_settings_key(&self.check_folder(), &toolchain);
        let (_, store) = self.records_setup(&toolchain, &key, tree)?;
        if let Ok(cause) = std::fs::read_to_string(store.with_extension("none")) {
            return Err(cause);
        }
        if !store.is_dir() {
            return Err(format!("the cache has no expansion records of the tree `{}` (a build check of an earlier version of Semilla, or `semilla cache --clean`)", &tree[..tree.len().min(7)]));
        }
        let units = semilla_normal::read_expansions(&store).ok_or_else(|| format!("the expansion records of the tree `{}` cannot be read", &tree[..tree.len().min(7)]))?;
        // The build check builds the configuration `dev` (design 53.2).
        Ok(units.into_iter().map(|unit| semilla_verify::expand::ExpansionUnit { config: "dev".to_string(), target: unit.target, package: unit.package, root: unit.root, cfg: unit.cfg, list: unit.list.into_iter().filter_map(|value| serde_json::from_value(value).ok()).collect() }).collect())
    }

    /// The build check of one tree. The result of a tree is in the cache, by the tree and the
    /// build settings. `reuse_failure` is false for the new tree of a step: a stored failure
    /// is not final, and the check runs again. The second value is true for a stored result.
    /// With `records` (a step of the default mode, R65.8-1), the build also writes the
    /// expansion records of the tree, and a stored result counts only with its records.
    fn tree_build(&self, tree: &str, reuse_failure: bool, records: bool) -> Result<(BuildRun, bool)> {
        let folder = self.check_folder();
        let results = folder.join("results");
        std::fs::create_dir_all(&results).with_context(|| format!("cannot create the folder {}", results.display()))?;
        // The key of a result has the identity of the tool chain: a different tool chain runs
        // the check again.
        let toolchain = self.check_toolchain(tree)?;
        let key = semilla_normal::cargo::check_settings_key(&folder, &toolchain);
        let file = results.join(format!("{tree}-{key}.json"));
        // R65.8-1: the records of the tree. A toolchain that is not the toolchain of the driver
        // gives the cause in place of the records.
        let setup = if records { Some(self.records_setup(&toolchain, &key, tree)) } else { None };
        let has_records = || match &setup {
            Some(Ok((_, store))) => store.is_dir() || store.with_extension("none").is_file(),
            _ => true,
        };
        let stored = || {
            let text = std::fs::read_to_string(&file).ok()?;
            serde_json::from_str::<BuildRun>(&text).ok().filter(|run| reuse_failure || run.failed.is_empty()).filter(|_| has_records())
        };
        if let Some(run) = stored() {
            return Ok((run, true));
        }
        // One build folder serves one build at a time, also across processes.
        let lock = std::fs::File::create(folder.join("lock")).with_context(|| format!("cannot write the folder {}", folder.display()))?;
        lock.lock()?;
        if let Some(run) = stored() {
            return Ok((run, true));
        }
        let build = folder.join("build");
        self.repo.checkout_build(&build, tree).with_context(|| format!("cannot write the tree to the build folder {}", build.display()))?;
        let started = std::time::Instant::now();
        let target = folder.join("target");
        let driver = match &setup {
            Some(Ok((driver, _))) => Some(driver),
            _ => None,
        };
        let (mut run, mut files) = semilla_normal::cargo::build_check_with(&build, &target, &toolchain, driver)?;
        // A fresh unit with no record file: an earlier build of the folder did not write it.
        // The target folder goes away, and one more build compiles each unit.
        if files == Some(CheckRecordFiles::FreshWithout) {
            match std::fs::remove_dir_all(&target) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
                _ => {}
            }
            (run, files) = semilla_normal::cargo::build_check_with(&build, &target, &toolchain, driver)?;
        }
        crate::timing("build check", started);
        if let Some(setup) = &setup {
            store_records(setup, files.as_ref(), &run, &folder.join("records"));
        }
        // A failed write only costs time: the next check of this tree runs cargo again.
        let _ = std::fs::write(&file, serde_json::to_string(&run)?);
        Ok((run, false))
    }

    /// The build check of a step (design 53), for a command that records the step.
    ///
    /// - `skip`: the cause of a check that does not run (`--no-check`, `SEMILLA_NO_CHECK`).
    /// - `before`, `after`: the trees of the step.
    ///
    /// The compiled check of this command can be the build check: it builds each target of the
    /// new tree, and it rejects a tree that does not build. Then no second build runs.
    ///
    /// - `records`: true for a step of the default mode (R65.8-1). The build also writes the
    ///   expansion records of the two trees: the tree before the step builds first (a stored
    ///   build of that tree is used), so the build of the tree after the step is a delta build.
    pub(crate) fn build_check(&mut self, skip: Option<&str>, before: &str, after: &str, records: bool) -> Outcome {
        if let Some(cause) = skip {
            return Outcome::Stored { record: BuildCheck::NotRun, lines: vec![format!("build check: not run ({cause})")] };
        }
        if self.compiled_build(before, after) {
            return Outcome::Stored { record: BuildCheck::Passes, lines: vec!["build check: passes (the build of the compiled check)".to_string()] };
        }
        let started = std::time::Instant::now();
        let cannot_run = |error: anyhow::Error| Outcome::CannotRun {
            text: format!("  ! build check: cannot run: {}\n", semilla_core::text::block(format!("{error:#}").trim_end()).replace('\n', "\n    ")),
        };
        // R65.8-1: the records of the tree before the step. A build of that tree that cannot run
        // gives no records; the build of the tree after the step then tells the cause.
        if records {
            let _ = self.tree_build(before, true, true);
        }
        let (run, stored) = match self.tree_build(after, false, records) {
            Ok(found) => found,
            Err(error) => return cannot_run(error),
        };
        // A build with a limit of the sandbox missing (design 27): the result says so.
        let mut notes: Vec<String> = Vec::new();
        if let Some(missing) = semilla_normal::sandbox::missing().filter(|_| !stored) {
            notes.push(format!("  ! build check: {}", missing.line()));
        }
        let time = if stored { "a stored result of this tree".to_string() } else { seconds(started) };
        if run.failed.is_empty() {
            let mut lines = vec![format!("build check: passes ({time})")];
            lines.extend(notes);
            return Outcome::Stored { record: BuildCheck::Passes, lines };
        }
        // Rule 8: only a build error that the step makes rejects the step. The check of the
        // tree before the step gives the state of each target without the step: it builds, it
        // does not build, or cargo did not check it (a target that it needs does not build).
        let base = match self.tree_build(before, true, records) {
            Ok((base, _)) => base,
            Err(error) => return cannot_run(error),
        };
        let time = seconds(started);
        let (mut added, mut excused, mut not_checked) = (Vec::new(), Vec::new(), Vec::new());
        for unit in &run.failed {
            match base.state(unit) {
                // A target that builds before the step, or a new target: the step makes the error.
                UnitState::Builds | UnitState::Absent => added.push(unit.clone()),
                UnitState::Fails => excused.push(unit.clone()),
                // Semilla cannot tell: this is not a cause for a reject.
                UnitState::NotChecked => not_checked.push(unit.clone()),
            }
        }
        if added.is_empty() {
            let record = BuildCheck::BaseFails { targets: excused, not_checked };
            let mut lines = result_lines(&record, &base.failed, &time);
            lines.extend(notes);
            return Outcome::Stored { record, lines };
        }
        let mut text = fail_text(&run, &added, &excused, &not_checked, &time);
        for note in notes {
            text.push_str(&note);
            text.push('\n');
        }
        let record = BuildCheck::Fails { errors: first_errors(&run, &added), targets: added };
        Outcome::Fails { text, record }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semilla_normal::cargo::BuildError;

    #[test]
    fn the_toolchain_file_of_a_tree_names_a_channel_or_a_path() {
        let channel = |name: &str| TreeToolchain::Channel(name.to_string());
        assert_eq!(toolchain_request("rust-toolchain.toml", "[toolchain]\nchannel = \"1.80.0\"\ncomponents = [\"clippy\"]\n").unwrap(), channel("1.80.0"));
        assert_eq!(toolchain_request("rust-toolchain.toml", "[toolchain]\npath = \"/opt/rust\"\n").unwrap(), TreeToolchain::Path("/opt/rust".into()));
        // The file `rust-toolchain` can have the form of the TOML file, or one line.
        assert_eq!(toolchain_request("rust-toolchain", "[toolchain]\nchannel = \"beta\"\n").unwrap(), channel("beta"));
        assert_eq!(toolchain_request("rust-toolchain", "nightly-2026-01-01\n").unwrap(), channel("nightly-2026-01-01"));
        // The one line form is not permitted in the TOML file. A file with no name is an error.
        for (file, text) in [("rust-toolchain.toml", "stable\n"), ("rust-toolchain.toml", "[toolchain]\nprofile = \"minimal\"\n"), ("rust-toolchain", "two words\n"), ("rust-toolchain", "")] {
            let error = toolchain_request(file, text).unwrap_err().to_string();
            assert!(error.contains(&format!("the file `{file}` of the tree names no toolchain")), "{error}");
        }
    }

    // rule+ R53.3-9
    #[test]
    fn only_a_tree_in_which_each_target_builds_has_the_word_passes() {
        assert_eq!(result_lines(&BuildCheck::Passes, &[], "0.1 s"), ["build check: passes (0.1 s)"]);
        let lib = "`demo` (lib)".to_string();
        let test = "`demo` (test \"api\")".to_string();
        for record in [BuildCheck::BaseFails { targets: vec![lib.clone()], not_checked: vec![] }, BuildCheck::BaseFails { targets: vec![], not_checked: vec![test.clone()] }, BuildCheck::BaseFails { targets: vec![lib.clone()], not_checked: vec![test.clone()] }] {
            let lines = result_lines(&record, std::slice::from_ref(&lib), "0.1 s");
            assert_eq!(lines[0], "build check: no new failure found (0.1 s)");
            // Each line below the result line says a limit, with the mark.
            assert!(lines.len() > 1 && lines[1..].iter().all(|line| line.starts_with("  ! build check: the target `demo` (")), "{lines:?}");
            assert!(!lines.iter().any(|line| line.contains("passes")), "{lines:?}");
        }
        let both = result_lines(&BuildCheck::BaseFails { targets: vec![lib.clone()], not_checked: vec![test] }, &[lib], "0.1 s");
        assert_eq!(both[1], "  ! build check: the target `demo` (lib) does not build before the step and after it, so Semilla did not check a new error of the step in this target, or in a target that needs it");
        assert_eq!(both[2], "  ! build check: the target `demo` (test \"api\") does not build after the step, and cargo did not check it before the step (the target `demo` (lib) did not build then), so Semilla cannot tell if the step makes this error");
    }

    fn error(kind: &str, name: &str, text: &str) -> BuildError {
        BuildError { package: "path+file:///work/demo#0.1.0".to_string(), kinds: vec![kind.to_string()], name: name.to_string(), text: text.to_string() }
    }

    // rule+ R53.4-7
    #[test]
    fn the_errors_of_the_step_come_first() {
        // Rule 8 of design 53: a bench does not build before the step. The step breaks the
        // library. The message shows the error of the library, not the errors of the bench.
        let bench = "`demo` (bench \"speed\")".to_string();
        let lib = vec!["`demo` (lib)".to_string(), "`demo` (lib test)".to_string()];
        let run = BuildRun {
            failed: vec![bench.clone(), lib[0].clone(), lib[1].clone()],
            errors: vec![error("bench", "speed", "error: old\n --> benches/speed.rs:1:1"), error("lib", "demo", "error[E0308]: mismatched types\n --> src/lib.rs:2:5")],
            ..Default::default()
        };
        let text = fail_text(&run, &lib, std::slice::from_ref(&bench), &[], "1.0 s");
        assert!(text.starts_with("  ✗ build check: fails (1.0 s): after the step, the targets `demo` (lib), `demo` (lib test) do not build\n"), "{text}");
        assert!(text.contains("(not a cause of this result: the target `demo` (bench \"speed\") does not build before the step too)"), "{text}");
        assert!(text.contains("\n\n    error[E0308]: mismatched types\n     --> src/lib.rs:2:5\n") && !text.contains("error: old"), "{text}");
    }

    #[test]
    fn a_failure_with_no_compiler_error_shows_the_lines_of_cargo() {
        let run = BuildRun { failed: vec![NO_UNIT.to_string()], other: "error: failed to parse manifest at `Cargo.toml`".to_string(), ..Default::default() };
        let text = fail_text(&run, &run.failed, &[], &[], "0.1 s");
        assert!(text.contains("the target `workspace` (cargo starts no build) does not build") && text.contains("failed to parse manifest"), "{text}");
    }

    // rule+ R53.3-3
    // rule- R53.3-3
    #[test]
    fn a_long_list_of_errors_has_a_count() {
        let unit = vec!["`demo` (lib)".to_string()];
        let errors = (0..8).map(|index| error("lib", "demo", &format!("error: e{index}"))).collect();
        let text = fail_text(&BuildRun { failed: unit.clone(), errors, ..Default::default() }, &unit, &[], &[], "0.1 s");
        assert!(text.contains("error: e4") && !text.contains("error: e5") && text.contains("(3 more errors of the compiler"), "{text}");
        // Negative: a list of 5 errors shows each error and has no count line.
        let errors = (0..5).map(|index| error("lib", "demo", &format!("error: e{index}"))).collect();
        let text = fail_text(&BuildRun { failed: unit.clone(), errors, ..Default::default() }, &unit, &[], &[], "0.1 s");
        assert!(text.contains("error: e4") && !text.contains("more error"), "{text}");
    }

    /// Design 53.3: an error that two targets give shows one time. The negative test: two
    /// different errors show one time each.
    // rule+ R53.3-4
    // rule- R53.3-4
    #[test]
    fn an_error_that_two_targets_give_shows_one_time() {
        let units = vec!["`demo` (lib)".to_string(), "`demo` (lib test)".to_string()];
        let same = "error[E0308]: mismatched types\n --> src/lib.rs:2:5";
        let run = BuildRun { failed: units.clone(), errors: vec![error("lib", "demo", same), error("lib", "demo", same)], ..Default::default() };
        let text = fail_text(&run, &units, &[], &[], "0.1 s");
        assert_eq!(text.matches("error[E0308]").count(), 1, "{text}");
        // Negative: the second target gives a different error. Each error shows.
        let other = "error[E0308]: mismatched types\n --> src/lib.rs:9:5";
        let run = BuildRun { failed: units.clone(), errors: vec![error("lib", "demo", same), error("lib", "demo", other)], ..Default::default() };
        let text = fail_text(&run, &units, &[], &[], "0.1 s");
        assert_eq!(text.matches("error[E0308]").count(), 2, "{text}");
        assert!(text.contains("src/lib.rs:2:5") && text.contains("src/lib.rs:9:5"), "{text}");
    }
}
