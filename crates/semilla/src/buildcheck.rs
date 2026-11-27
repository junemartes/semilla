//! The build check of a step (design 53).
//!
//! Each command that records a step runs the build check on the new tree of the step. A step
//! that does not build is not stored. The build of the compiled check is the build check when
//! it builds each target (design 53, rule 9; design 83). Else `cargo check` gives the result
//! and the errors of the compiler. No option turns the check off (design 83).
//!
//! The check makes no fact and no compare result.

use anyhow::{Context, Result};
use semilla_core::BuildCheck;
use semilla_normal::cargo::{BuildRun, CheckToolchain, NO_UNIT, TreeToolchain, UnitState};

use crate::app::App;

/// The number of compiler errors that a message shows.
const ERROR_MAX: usize = 5;

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

    /// The build check of one tree. The result of a tree is in the cache, by the tree and the
    /// build settings. `reuse_failure` is false for the new tree of a step: a stored failure
    /// is not final, and the check runs again. The second value is true for a stored result.
    fn tree_build(&self, tree: &str, reuse_failure: bool) -> Result<(BuildRun, bool)> {
        let folder = self.check_folder();
        let results = folder.join("results");
        std::fs::create_dir_all(&results).with_context(|| format!("cannot create the folder {}", results.display()))?;
        // The key of a result has the identity of the tool chain: a different tool chain runs
        // the check again.
        let toolchain = self.check_toolchain(tree)?;
        let key = semilla_normal::cargo::check_settings_key(&folder, &toolchain);
        let file = results.join(format!("{tree}-{key}.json"));
        let stored = || {
            let text = std::fs::read_to_string(&file).ok()?;
            serde_json::from_str::<BuildRun>(&text).ok().filter(|run| reuse_failure || run.failed.is_empty())
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
        let run = semilla_normal::cargo::build_check(&build, &target, &toolchain)?;
        crate::timing("build check", started);
        // A failed write only costs time: the next check of this tree runs cargo again.
        let _ = std::fs::write(&file, serde_json::to_string(&run)?);
        Ok((run, false))
    }

    /// R68.12-1: a "not checked" line for each `cfg` attribute of the new tree whose condition
    /// the target of the build check makes false around changed code. The true atoms of the
    /// target come from `rustc --print cfg` of the tool chain of the check, with the flags of
    /// `RUSTFLAGS`. A tree with a cargo configuration file can set a different target or other
    /// flags: Semilla does not read it, so the line says that the code under a target
    /// condition is not known to be built. No line has a reject: the rule only adds lines.
    fn target_hidden_lines(&mut self, before: &str, after: &str) -> Vec<String> {
        let Ok(toolchain) = self.check_toolchain(after) else { return Vec::new() };
        let rustc = toolchain.root.join("bin/rustc");
        let flags: Vec<String> = match std::env::var("CARGO_ENCODED_RUSTFLAGS") {
            Ok(text) if !text.is_empty() => text.split('\u{1f}').map(str::to_string).collect(),
            _ => std::env::var("RUSTFLAGS").map(|text| text.split_whitespace().map(str::to_string).collect()).unwrap_or_default(),
        };
        let run = |args: &[&str]| -> Option<String> {
            let output = std::process::Command::new(&rustc).args(args).args(&flags).env_remove("RUSTUP_TOOLCHAIN").output().ok()?;
            output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        };
        let Some(cfg) = run(&["--print", "cfg"]) else { return Vec::new() };
        let host = semilla_verify::print_cfg_atoms(&cfg);
        let target = run(&["-vV"]).and_then(|text| text.lines().find_map(|line| line.strip_prefix("host: ").map(|host| format!("`{}`", host.trim())))).unwrap_or_else(|| "the host".to_string());
        let (Ok(old), Ok(new)) = (self.workspace(before), self.workspace(after)) else { return Vec::new() };
        let config = [".cargo/config.toml", ".cargo/config"].into_iter().find(|file| self.repo.git(&["cat-file", "-e", &format!("{after}:{file}")]).is_ok());
        semilla_verify::target_hidden([&old, &new], &host)
            .iter()
            .map(|hidden| match config {
                Some(file) => format!("  ! build check: maybe not built: the changed code under `{}` at {}:{} has a target condition, and the file `{file}` of the tree can set the target of the check: Semilla does not know if the check built it", hidden.attr, hidden.file, hidden.line),
                None => format!("  {}", hidden.text(&target)),
            })
            .collect()
    }

    /// The build check of a step (design 53), for a command that records the step.
    ///
    /// - `before`, `after`: the trees of the step.
    ///
    /// The compiled check of this command can be the build check: it builds each target of the
    /// new tree, and it rejects a tree that does not build. Then no second build runs.
    pub(crate) fn build_check(&mut self, before: &str, after: &str) -> Outcome {
        if self.compiled_build(before, after) {
            return Outcome::Stored { record: BuildCheck::Passes, lines: vec!["build check: passes (the build of the compiled check)".to_string()] };
        }
        let started = std::time::Instant::now();
        let cannot_run = |error: anyhow::Error| Outcome::CannotRun {
            text: format!("  ! build check: cannot run: {}\n", semilla_core::text::block(format!("{error:#}").trim_end()).replace('\n', "\n    ")),
        };
        let (run, stored) = match self.tree_build(after, false) {
            Ok(found) => found,
            Err(error) => return cannot_run(error),
        };
        // A build with a limit of the sandbox missing (design 27): the result says so.
        let mut notes: Vec<String> = Vec::new();
        // R68.12-1: the changed code that the target of the check does not build.
        notes.extend(self.target_hidden_lines(before, after));
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
        let base = match self.tree_build(before, true) {
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
