//! The adapters from the verifier traits to the real normal forms and the real SMT solver.

use std::collections::BTreeMap;

use anyhow::Result;
use semilla_core::Step;
use semilla_core::report::Effect;
use semilla_normal::cargo::{Configuration, Features};
use semilla_verify::{BodyComparison, BodyOracle, BodyStatus, EffectCheck, ExprOracle};

use crate::git::Repo;

/// Runs the MIR driver on the snapshots, and compares the bodies with the normal forms.
///
/// Each level has one fixed build folder. Semilla updates it to each tree by delta, so cargo
/// compiles only the changed crates and their dependents. The records of each tree and level are
/// cached. The build folders, the cargo target folders, and the records are in the cache folder
/// of the user, outside the repository folder (I5): the agent cannot write them.
///
/// A tree with build code that the change set changes never uses these shared folders (design
/// 28). It has a separate folder set, `build-code/<key>/`, with its own build folders,
/// target folders, and records. A command with `--no-build-code` does not build such a tree and
/// does not read its records.
pub struct NormalOracle {
    pub repo: Repo,
    /// The build code state of each tree of the change set.
    pub code: crate::buildcode::BuildCode,
    pub mode: crate::buildcode::Mode,
    /// The configurations of each step that this oracle compared (batch 5s).
    pub log: ConfigLog,
    /// The configurations that Semilla adds for a proof (batch 5u, part C).
    pub added: std::sync::Mutex<Added>,
}

/// The added feature selections of the steps of one oracle (batch 5u, part C).
#[derive(Debug, Default)]
pub struct Added {
    /// The feature selections that the verifier set for a step, by the two trees of the step.
    selections: BTreeMap<(String, String), Vec<Features>>,
    /// The result of `checked` for the two trees of a step and a list of added selections. The
    /// records of a tree do not change in the time of one process.
    memo: Vec<((String, String), Vec<Features>, Vec<Configuration>, StepConfigs)>,
    /// The steps that have the builds of the hint.
    hinted: std::collections::BTreeSet<(String, String)>,
}

/// The name of the hint file in the cache folder (batch 5u, part C): the feature selections
/// that the last step with an added configuration needed. Semilla builds them with the default
/// list, so the builds are ready if the step needs them. The hint has no part in a decision.
pub const HINT_FILE: &str = "added-configurations.json";

/// The feature selections of a hint text. A text with a bad form gives no selection.
fn parse_hint(text: &str) -> Vec<Features> {
    let Ok(serde_json::Value::Array(list)) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let name = |value: &serde_json::Value| value.as_str().filter(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || "_-+.".contains(c))).map(str::to_string);
    let entry = |value: &serde_json::Value| -> Option<Features> {
        let features: Vec<String> = value.get("features")?.as_array()?.iter().map(name).collect::<Option<_>>()?;
        if features.is_empty() || features.len() > 8 || !features.is_sorted() {
            return None;
        }
        Some(if value.get("no_default")?.as_bool()? { Features::NoDefaultListed(features) } else { Features::Listed(features) })
    };
    if list.len() > semilla_verify::MOST_SELECTIONS {
        return Vec::new();
    }
    list.iter().map(entry).collect::<Option<Vec<Features>>>().unwrap_or_default()
}

/// The hint text of a list of added feature selections.
fn hint_text(list: &[Features]) -> String {
    let entries: Vec<serde_json::Value> = list
        .iter()
        .filter_map(|features| match features {
            Features::Listed(list) => Some(serde_json::json!({ "no_default": false, "features": list })),
            Features::NoDefaultListed(list) => Some(serde_json::json!({ "no_default": true, "features": list })),
            _ => None,
        })
        .collect();
    serde_json::Value::Array(entries).to_string()
}

/// The configurations of one compared step (batch 5s, B1): the data of the output.
#[derive(Clone, Debug, Default)]
pub struct StepConfigs {
    /// The names of the checked configurations.
    pub checked: Vec<String>,
    /// The configurations of the default list that Semilla did not check, each with the cause.
    pub not_checked: Vec<(String, String)>,
    /// The true settings of each checked configuration, in the tree before the step and in the
    /// tree after it.
    pub truth: [Vec<semilla_verify::ConfigTruth>; 2],
    /// The target triple of the check build.
    pub target: String,
    /// The names of the checked configurations that Semilla added for a proof (batch 5u,
    /// part C).
    pub added: Vec<String>,
}

/// The configurations of each compared step, by the two trees of the step.
pub type ConfigLog = std::sync::Arc<std::sync::Mutex<BTreeMap<(String, String), StepConfigs>>>;


/// The file of the records cache that holds the error of a build with a compile error. The
/// same tree, configuration, and settings give the same error.
const FAILED_FILE: &str = ".failed";

/// True if a build error is a compile error of the tree. A dependency that cargo cannot get,
/// a full disk, and a write that the sandbox stops are not compile errors.
fn is_compile_error(text: &str) -> bool {
    text.contains("cargo check failed") && !semilla_normal::cargo::missing_dependency(text) && !text.contains("No space left on device") && !text.contains("Permission denied")
}

/// The first error line of a build error text.
fn first_error_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|line| line.starts_with("error")).or_else(|| text.lines().map(str::trim).find(|line| !line.is_empty())).unwrap_or("");
    line.chars().take(160).collect()
}

/// The `cargo metadata` data of each tree of this process.
static METADATA: std::sync::Mutex<BTreeMap<String, Option<serde_json::Value>>> = std::sync::Mutex::new(BTreeMap::new());

/// The name of the file that marks the records of a tree with changed build code. The file
/// holds the base tree of the change set.
pub const BUILD_CODE_FILE: &str = ".build-code";

/// The folder set of the trees with one build code state (design 28.9, batch 5v). `marked` is
/// true for the trees with changed build code. A build can write its target folder and its
/// records folder, so two builds share a folder set only if they run the same build code.
pub fn folder_set(repo: &Repo, marked: bool, key: &str) -> std::path::PathBuf {
    repo.cache_dir().join(if marked { "build-code" } else { "shared" }).join(key)
}

/// The line for a build of the records that this process used, if the build ran with a limit
/// of the sandbox missing (design 27).
static NO_SANDBOX: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The name of the file that marks the records of a build with a limit of the sandbox missing.
/// The file holds the line that names each missing limit.
const NO_SANDBOX_FILE: &str = ".no-sandbox";

/// The files that a build of the records of this process left in its records folder, and that
/// are not the record file of a unit of that build (design 28.8).
static OTHER_FILES: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

/// The name of the file that holds the names of [`OTHER_FILES`] for the records of one build.
const OTHER_FILES_FILE: &str = ".other-files";

/// The warning for a result, if a build of its records ran with a limit of the sandbox missing.
/// The line names each missing limit.
pub fn no_sandbox_warning() -> Option<String> {
    NO_SANDBOX.lock().ok()?.clone()
}

/// The warning for a result, if a build of its records left a file in the records folder that
/// is not the record file of a unit. Semilla does not copy such a file. The line names each.
pub fn other_files_warning() -> Option<String> {
    let files = OTHER_FILES.lock().ok()?;
    if files.is_empty() {
        return None;
    }
    let names: Vec<String> = files.iter().map(|name| format!("`{name}`")).collect();
    let (noun, verb) = if names.len() == 1 { ("a file", "is") } else { ("files", "are") };
    Some(format!("the build left {noun} in the records folder that {verb} not a record of a build unit: {}. Semilla does not use {}", names.join(", "), if names.len() == 1 { "it" } else { "them" }))
}

/// The warnings of the builds of the records that this process used.
pub fn build_warnings() -> Vec<String> {
    no_sandbox_warning().into_iter().chain(other_files_warning()).collect()
}

/// Reads the sandbox state and the other files of a records folder.
fn note_sandbox(cache: &std::path::Path) {
    if let Ok(line) = std::fs::read_to_string(cache.join(NO_SANDBOX_FILE))
        && let Ok(mut state) = NO_SANDBOX.lock()
    {
        *state = Some(line.trim().to_string());
    }
    if let Ok(names) = std::fs::read_to_string(cache.join(OTHER_FILES_FILE))
        && let Ok(mut files) = OTHER_FILES.lock()
    {
        files.extend(names.lines().filter(|name| !name.is_empty()).map(str::to_string));
    }
}

impl NormalOracle {
    /// The feature sets to check: the default features first, then the sets that the manifest of
    /// either tree lists in `[workspace.metadata.semilla]` or `[package.metadata.semilla]`
    /// `feature-sets` (design 8.12).
    fn feature_sets(&self, trees: &[&str]) -> Result<Vec<Vec<String>>> {
        let mut sets: Vec<Vec<String>> = vec![Vec::new()];
        for tree in trees {
            let dir = self.repo.materialize(tree)?;
            let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else { continue };
            let Ok(manifest) = text.parse::<toml::Table>() else { continue };
            let listed = ["workspace", "package"].iter().find_map(|section| {
                manifest.get(*section)?.get("metadata")?.get("semilla")?.get("feature-sets")?.as_array().cloned()
            });
            for set in listed.unwrap_or_default() {
                let mut features: Vec<String> = set.as_array().into_iter().flatten().filter_map(|f| f.as_str().map(str::to_string)).collect();
                features.sort();
                if !sets.contains(&features) {
                    sets.push(features);
                }
            }
        }
        Ok(sets)
    }

    /// The `cargo metadata` data of a tree (the manifests), or `None` if cargo cannot read them.
    fn metadata(&self, tree: &str) -> Option<serde_json::Value> {
        if let Some(known) = METADATA.lock().ok().and_then(|cache| cache.get(tree).cloned()) {
            return known;
        }
        let found = self.repo.materialize(tree).ok().and_then(|dir| semilla_normal::cargo::cargo_metadata(&dir).ok());
        if let Ok(mut cache) = METADATA.lock() {
            cache.insert(tree.to_string(), found.clone());
        }
        found
    }

    /// The default list of configurations of a step (batch 5s, A2): the two profiles, each
    /// with the default features, no default features, all features, and each listed feature
    /// set. A feature selection with the same feature state as an earlier one, in the two
    /// trees, is not in the list. The list comes only from the manifests of the two trees.
    ///
    /// Then the configurations of the feature selections `extra` that Semilla adds for a
    /// proof (batch 5u, part C). The second part of an entry is true for an added
    /// configuration. The default list is the same for each `extra`: an added selection is
    /// after each selection of the default list.
    fn configurations_with(&self, before: &str, after: &str, extra: &[Features]) -> Result<Vec<(Configuration, bool)>> {
        let mut selections = vec![(Features::Default, false), (Features::NoDefault, false), (Features::All, false)];
        selections.extend(self.feature_sets(&[before, after])?.into_iter().filter(|set| !set.is_empty()).map(|set| (Features::Listed(set), false)));
        selections.extend(extra.iter().map(|features| (features.clone(), true)));
        let metadata = [self.metadata(before), self.metadata(after)];
        let mut seen = Vec::new();
        let mut kept = Vec::new();
        for (selection, added) in selections {
            // A tree with manifests that cargo cannot read has no state: the selection stays.
            let state: Vec<_> = metadata.iter().map(|data| data.as_ref().map(|data| semilla_normal::cargo::feature_state(data, &selection))).collect();
            if state.iter().all(Option::is_some) && seen.contains(&state) {
                continue;
            }
            seen.push(state);
            kept.push((selection, added));
        }
        Ok(kept.into_iter().flat_map(|(features, added)| [false, true].map(|release| (Configuration::new(release, features.clone()), added))).collect())
    }

    /// The cache folder of the records of a tree at a level and a configuration.
    fn cache_folder(&self, tree: &str, level: &str, config: &Configuration) -> Result<(std::path::PathBuf, std::path::PathBuf, bool, String)> {
        let (root, marked) = self.root(tree)?;
        let base = root.join("build");
        let settings = semilla_normal::cargo::settings_key(&base, semilla_normal::cargo::find_driver().ok().as_deref());
        let name = format!("{level}{}", config.suffix());
        let cache = root.join("mir").join(tree).join(format!("{name}-s{settings}"));
        Ok((root, cache, marked, name))
    }

    /// The unit records of a configuration in a tree, by the compiler (batch 5t, part A). The
    /// record files of the N0 build have them. `None`: a record file has no unit record.
    fn units(&self, tree: &str, config: &Configuration) -> Option<Vec<semilla_normal::UnitRecord>> {
        let (_, cache, _, _) = self.cache_folder(tree, "n0", config).ok()?;
        semilla_normal::read_units(&cache)
    }

    /// The target triple of the check build of a tree: the target of the compiler, else the
    /// host of the toolchain.
    fn target(&self, tree: &str, config: &Configuration) -> String {
        self.units(tree, config).and_then(|units| units.into_iter().map(|unit| unit.triple).find(|triple| !triple.is_empty())).unwrap_or_else(semilla_normal::cargo::host_target)
    }

    /// The true settings of a checked configuration in a tree.
    fn truth(&self, tree: &str, config: &Configuration) -> semilla_verify::ConfigTruth {
        let units = self.units(tree, config);
        semilla_verify::ConfigTruth {
            name: config.name(),
            known: units.is_some(),
            units: units.into_iter().flatten().map(|unit| semilla_verify::Unit { target: unit.target, package: unit.folder, cfg: unit.cfg, files: unit.files, root: unit.root }).collect(),
        }
    }

    /// The folder set of a tree, and true if it is the separate set of the changed build code.
    ///
    /// The shared folders (`shared/<key>`) serve only a tree with the build code of the base.
    /// The key is the digest of that build code. A tree with no state is never built.
    pub fn root(&self, tree: &str) -> Result<(std::path::PathBuf, bool)> {
        if self.code.clean.contains(tree) {
            let key = if self.code.shared.is_empty() { format!("tree-{}", self.code.base) } else { self.code.shared.clone() };
            return Ok((folder_set(&self.repo, false, &key), false));
        }
        if !self.code.trees.contains(tree) {
            anyhow::bail!("an internal error: tree {tree} has no build code state, so Semilla does not build it");
        }
        if !self.mode.runs_build_code() {
            anyhow::bail!("tree {tree} has build code that the change set changes; a command with --no-build-code does not build it");
        }
        let key = self.code.keys.get(tree).cloned().unwrap_or_else(|| format!("tree-{tree}"));
        Ok((folder_set(&self.repo, true, &key), true))
    }

    /// The records of a tree at a level, with the default features.
    pub fn records(&self, tree: &str, level: &str) -> Result<Vec<semilla_normal::BodyRecord>> {
        self.records_with(tree, level, &Configuration::DEV)
    }

    /// The records of a tree at a level and a configuration: from the cache, or from a delta
    /// build.
    fn records_with(&self, tree: &str, level: &str, config: &Configuration) -> Result<Vec<semilla_normal::BodyRecord>> {
        // The cache key: the tree, the level, the configuration, and the build settings that are
        // not in the tree (the outer cargo configuration and the driver binary).
        let (root, cache, marked, name) = self.cache_folder(tree, level, config)?;
        let base = root.join("build");
        // A record with the mark is never a record of the shared folders.
        if !marked && cache.join(BUILD_CODE_FILE).exists() {
            let _ = std::fs::remove_dir_all(&cache);
        }
        // A compile error of this tree in this configuration: the same build gives it again.
        if let Ok(error) = std::fs::read_to_string(cache.join(FAILED_FILE)) {
            anyhow::bail!("{error}");
        }
        if cache.join(".complete").exists() {
            note_sandbox(&cache);
            return semilla_normal::read_records(&cache);
        }
        std::fs::create_dir_all(&base)?;
        // One build folder serves one build at a time, also across processes.
        let lock = std::fs::File::create(base.join(format!("{name}.lock")))?;
        lock.lock()?;
        // Another process can finish the same build while this one waits for the lock.
        if cache.join(".complete").exists() {
            lock.unlock()?;
            note_sandbox(&cache);
            return semilla_normal::read_records(&cache);
        }
        let build = base.join(&name);
        let records = base.join(format!("{name}-records"));
        // The sandbox write set of the build is the target folder and the records folder. For a
        // tree with changed build code, the two are in the separate folder set.
        let target = root.join("target").join(&name);
        std::fs::create_dir_all(root.join("target"))?;
        let started = std::time::Instant::now();
        self.repo.checkout_build(&build, tree)?;
        let built = match semilla_normal::run_cargo_check_delta_config(&build, level, &records, &target, config) {
            Ok(built) => built,
            Err(error) => {
                let text = format!("{error:#}");
                if is_compile_error(&text) && !marked {
                    let _ = std::fs::remove_dir_all(&cache);
                    std::fs::create_dir_all(&cache)?;
                    std::fs::write(cache.join(FAILED_FILE), &text)?;
                }
                return Err(error);
            }
        };
        crate::timing(&format!("cargo check {name} of tree {}", &tree[..8.min(tree.len())]), started);
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache)?;
        // The cache gets only the record file of each unit of this build (design 28.8). Each
        // other file of the records folder stays out, and the result names it. A program of
        // the build decides the name: each hidden character becomes an escape.
        for file in &built.records {
            std::fs::copy(records.join(file), cache.join(file))?;
        }
        if !built.other.is_empty() {
            std::fs::write(cache.join(OTHER_FILES_FILE), built.other.iter().map(|name| format!("{}\n", semilla_core::text::inline(name).replace('`', "'"))).collect::<String>())?;
        }
        // The sandbox state of the build stays with the records: a later result that uses them
        // shows the same warning.
        if let Some(missing) = semilla_normal::sandbox::missing() {
            std::fs::write(cache.join(NO_SANDBOX_FILE), missing.line())?;
        }
        if marked {
            std::fs::write(cache.join(BUILD_CODE_FILE), format!("{}\n", self.code.base))?;
        }
        std::fs::write(cache.join(".complete"), "")?;
        lock.unlock()?;
        note_sandbox(&cache);
        semilla_normal::read_records(&cache)
    }

    /// The records of both trees at some levels. Each level runs in its own thread.
    fn levels(&self, before: &str, after: &str, levels: &[&str], features: &Configuration) -> Result<(semilla_normal::Levels, semilla_normal::Levels)> {
        let results: Vec<Result<(String, Vec<_>, Vec<_>)>> = std::thread::scope(|scope| {
            let handles: Vec<_> = levels
                .iter()
                .map(|level| scope.spawn(move || Ok((level.to_string(), self.records_with(before, level, features)?, self.records_with(after, level, features)?))))
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a build thread failed")))).collect()
        });
        let (mut old, mut new) = (semilla_normal::Levels::default(), semilla_normal::Levels::default());
        for result in results {
            let (level, b, a) = result?;
            old.insert(level.clone(), b);
            new.insert(level, a);
        }
        Ok((old, new))
    }

    /// Builds the N0 records of the two trees of a step in each configuration of the list. The
    /// configurations build in parallel: each has its own build folder and target folder.
    ///
    /// The result for each configuration is `None` (the two trees build), or the first error
    /// line (the tree before the step does not build: Semilla does not check the configuration
    /// in this step). A configuration that builds before the step and not after it is a compile
    /// error of the step (batch 5s, A4).
    fn build_all(&self, step: &Step, configs: &[Configuration]) -> Result<Vec<Option<String>>> {
        // The first error in the order of the list, so that the output is stable.
        self.build_each(step, configs).into_iter().collect()
    }

    /// The build of `build_all`, with the result of each configuration.
    fn build_each(&self, step: &Step, configs: &[Configuration]) -> Vec<Result<Option<String>>> {
        std::thread::scope(|scope| {
            let handles: Vec<_> = configs
                .iter()
                .map(|config| {
                    scope.spawn(move || {
                        if let Err(error) = self.records_with(&step.tree_before, "n0", config) {
                            let text = format!("{error:#}");
                            return if is_compile_error(&text) { Ok(Some(first_error_line(&text))) } else { Err(error) };
                        }
                        match self.records_with(&step.tree_after, "n0", config) {
                            Ok(_) => Ok(None),
                            Err(error) if is_compile_error(&format!("{error:#}")) => {
                                Err(error.context(format!("the code after the step does not compile in the configuration `{}` (it compiles before the step)", config.name())))
                            }
                            Err(error) => Err(error),
                        }
                    })
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a build thread failed")))).collect()
        })
    }

    /// Compares the bodies of one configuration at N0.
    fn compare_n0(
        &self,
        step: &Step,
        renames: &BTreeMap<String, String>,
        added: &[String],
        features: &Configuration,
        index: usize,
        opaque: &dyn semilla_verify::OpaqueInputs,
    ) -> Result<SetState> {
        let (before, after) = self.levels(&step.tree_before, &step.tree_after, &["n0"], features)?;
        let renames = with_moved_closures(renames, added, &before, &after);
        let renames = &renames;
        // The bodies that the build configuration selects, by the syntax tree (batch 5l). No
        // normal form looks into them.
        for (levels, is_after) in [(&before, false), (&after, true)] {
            for record in levels.get("n0").into_iter().flatten() {
                let span = record.span.as_ref().map(|span| (span.file.clone(), span.line_start));
                opaque.note(is_after, &record.name, span.as_ref(), &record.adts);
                opaque.body(is_after, &record.name, span.as_ref(), index, &record.target);
                let names: Vec<semilla_verify::ExpandedName> = record
                    .names
                    .iter()
                    .map(|name| semilla_verify::ExpandedName {
                        word: name.word.clone(),
                        member: name.member,
                        qual: name.qual.clone(),
                        prefix: name.prefix,
                        mac: name.mac.clone(),
                        line: name.line,
                        rival: name.rival.clone(),
                        count: name.count.max(1),
                    })
                    .collect();
                opaque.names(is_after, &record.name, span.as_ref(), &names);
            }
        }
        let marks = |levels: &semilla_normal::Levels, is_after: bool| {
            let mut marks = semilla_normal::Marks::default();
            for record in levels.get("n0").into_iter().flatten() {
                let span = record.span.as_ref().map(|span| (span.file.clone(), span.line_start));
                let callee = opaque.callee(is_after, &record.name, span.as_ref(), &record.adts);
                let layout = opaque.layout(is_after, &record.name, span.as_ref(), &record.adts);
                if callee {
                    marks.opaque.insert(record.name.clone());
                }
                if layout {
                    marks.stop.insert(record.name.clone());
                }
            }
            marks
        };
        let (before_marks, after_marks) = (marks(&before, false), marks(&after, true));
        let comparisons = semilla_normal::compare_with(&before, &after, renames, added, &before_marks, &after_marks);
        Ok(SetState { config: features.clone(), renames: renames.clone(), before, after, before_marks, after_marks, comparisons })
    }

    /// Compares the bodies of one configuration at N2 and N3 (with N4), after `compare_n0`.
    fn compare_deep(&self, step: &Step, added: &[String], state: &mut SetState) -> Result<()> {
        let (old, new) = self.levels(&step.tree_before, &step.tree_after, &["n2", "n3"], &state.config)?;
        state.before.extend(old);
        state.after.extend(new);
        semilla_normal::drop_opaque_inlines(&mut state.before, &state.before_marks);
        semilla_normal::drop_opaque_inlines(&mut state.after, &state.after_marks);
        let started = std::time::Instant::now();
        state.comparisons = semilla_normal::compare_with(&state.before, &state.after, &state.renames, added, &state.before_marks, &state.after_marks);
        crate::timing("compare the bodies (N0 to N4)", started);
        Ok(())
    }
}

/// The comparison of one configuration of a step.
struct SetState {
    config: Configuration,
    renames: BTreeMap<String, String>,
    before: semilla_normal::Levels,
    after: semilla_normal::Levels,
    before_marks: semilla_normal::Marks,
    after_marks: semilla_normal::Marks,
    comparisons: Vec<semilla_normal::BodyComparison>,
}

impl SetState {
    /// True if a body is not equal at N0.
    fn needs_deep(&self) -> bool {
        self.comparisons.iter().any(|c| !matches!(c.status, semilla_normal::BodyStatus::Equal { .. }))
    }

    /// The span of each body: the "after" records win (the name is the "after" name).
    fn spans(&self, spans: &mut BTreeMap<String, (String, usize)>) {
        for record in [&self.before, &self.after].iter().flat_map(|levels| levels.values().flatten()) {
            if let Some(span) = &record.span {
                spans.insert(record.name.clone(), (span.file.clone(), span.line_start));
            }
        }
        if let (Some(old), Some(new)) = (self.before.get("n0"), self.after.get("n0")) {
            spans.extend(changed_target_spans(old, new));
        }
    }
}

/// The span of each body name that is in more than one target, from the target whose N0 text
/// changed (batch 5n, A4).
///
/// Two targets of a package can have the same crate name, and thus a body with the same name
/// (the `main` of `src/main.rs` and of a bench with the name of the package). The status of the
/// name is the worst status of its targets. So the span of the changed target wins: it is the
/// body that needs an owner.
fn changed_target_spans(old: &[semilla_normal::BodyRecord], new: &[semilla_normal::BodyRecord]) -> Vec<(String, (String, usize))> {
    let texts: BTreeMap<(&str, &str), &str> = old.iter().map(|record| ((record.name.as_str(), record.target.as_str()), record.text.as_str())).collect();
    let mut targets: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
    for record in new {
        targets.entry(record.name.as_str()).or_default().insert(record.target.as_str());
    }
    new.iter()
        .filter(|record| targets[record.name.as_str()].len() > 1)
        .filter(|record| texts.get(&(record.name.as_str(), record.target.as_str())) != Some(&record.text.as_str()))
        .filter_map(|record| record.span.as_ref().map(|span| (record.name.clone(), (span.file.clone(), span.line_start))))
        .collect()
}

/// Adds a rename for each closure that moved into a new function (for example with
/// `extract-function`): a closure that exists only before, and a closure of a declared new function
/// that exists only after, with the same N0 text and the same target. The same text means the
/// same body, with the same captured types, so the rename is sound.
fn with_moved_closures(
    renames: &BTreeMap<String, String>,
    added: &[String],
    before: &semilla_normal::Levels,
    after: &semilla_normal::Levels,
) -> BTreeMap<String, String> {
    let original = renames;
    let mut renames = original.clone();
    let (Some(old), Some(new)) = (before.get("n0"), after.get("n0")) else { return renames };
    let is_closure = |name: &str| name.contains("::{closure#");
    let old_names: std::collections::BTreeSet<&str> = old.iter().map(|r| r.name.as_str()).collect();
    let new_names: std::collections::BTreeSet<&str> = new.iter().map(|r| r.name.as_str()).collect();
    let mut taken = std::collections::BTreeSet::new();
    for record in old.iter().filter(|r| is_closure(&r.name) && !new_names.contains(r.name.as_str()) && !original.contains_key(&r.name)) {
        let moved = new.iter().find(|candidate| {
            is_closure(&candidate.name)
                && !old_names.contains(candidate.name.as_str())
                && !taken.contains(&candidate.name)
                && added.iter().any(|item| candidate.name.starts_with(&format!("{item}::{{closure")))
                && candidate.target == record.target
                && candidate.text == record.text
        });
        if let Some(moved) = moved {
            taken.insert(moved.name.clone());
            renames.insert(record.name.clone(), moved.name.clone());
        }
    }
    renames
}

/// Combines the status of one body over two configurations that compile it (batch 5s, A5).
///
/// The body is equal only if it is equal in each configuration. A body that only one side has
/// in each configuration keeps that state. Each other pair is a changed body: for example a
/// body that is equal in `dev` and exists only after the step in `release`.
fn combine(old: BodyStatus, new: BodyStatus) -> BodyStatus {
    match (old, new) {
        (BodyStatus::Equal { level: a, proof: mut p }, BodyStatus::Equal { level: b, proof: q }) => {
            p.extend(q.into_iter().filter(|rule| !p.contains(rule)).collect::<Vec<_>>());
            BodyStatus::Equal { level: a.max(b), proof: p }
        }
        (BodyStatus::Different { cfg_changed: a }, BodyStatus::Different { cfg_changed: b }) => BodyStatus::Different { cfg_changed: a || b },
        (BodyStatus::Different { cfg_changed }, BodyStatus::Equal { .. }) | (BodyStatus::Equal { .. }, BodyStatus::Different { cfg_changed }) => BodyStatus::Different { cfg_changed },
        (BodyStatus::BeforeOnly, BodyStatus::BeforeOnly) => BodyStatus::BeforeOnly,
        (BodyStatus::AfterOnly, BodyStatus::AfterOnly) => BodyStatus::AfterOnly,
        // The set of the configurations that compile the body changed, or the body changed in
        // one of them: the control flow of a build changed.
        _ => BodyStatus::Different { cfg_changed: true },
    }
}

/// The answer with no syntax tree: each input is opaque.
struct AllOpaque;

impl semilla_verify::OpaqueInputs for AllOpaque {
    fn callee(&self, _after: bool, _name: &str, _span: Option<&(String, usize)>, _types: &[String]) -> bool {
        true
    }

    fn layout(&self, _after: bool, _name: &str, _span: Option<&(String, usize)>, types: &[String]) -> bool {
        !types.is_empty()
    }
}

impl BodyOracle for NormalOracle {
    fn compare(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String]) -> Result<Vec<BodyComparison>> {
        self.compare_with(step, renames, added, &AllOpaque)
    }

    fn compare_with(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String], opaque: &dyn semilla_verify::OpaqueInputs) -> Result<Vec<BodyComparison>> {
        let checked = self.checked(step)?;
        // N0 for each configuration. Then N2 and N3 for each configuration with a body that is
        // not equal at N0: these builds run in parallel.
        let mut states = Vec::new();
        for (index, config) in checked.iter().enumerate() {
            states.push(self.compare_n0(step, renames, added, config, index, opaque)?);
        }
        let deep: Vec<Result<()>> = std::thread::scope(|scope| {
            let handles: Vec<_> = states
                .iter()
                .filter(|state| state.needs_deep())
                .map(|state| scope.spawn(move || self.levels(&step.tree_before, &step.tree_after, &["n2", "n3"], &state.config).map(|_| ())))
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a build thread failed")))).collect()
        });
        deep.into_iter().collect::<Result<Vec<()>>>()?;
        let mut spans = BTreeMap::new();
        let mut combined: BTreeMap<String, BodyStatus> = BTreeMap::new();
        // The configurations of each body: those that compile it, and those where it is not equal.
        let mut compiled: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut differs: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut config: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for mut state in states {
            if state.needs_deep() {
                self.compare_deep(step, added, &mut state)?;
            }
            state.spans(&mut spans);
            let name = state.config.name();
            for comparison in state.comparisons {
                if !comparison.config.is_empty() {
                    config.entry(comparison.name.clone()).or_default().extend(comparison.config.iter().cloned());
                }
                let status = match comparison.status {
                    semilla_normal::BodyStatus::Equal { level, proof } => BodyStatus::Equal { level, proof },
                    semilla_normal::BodyStatus::Different { cfg_changed } => BodyStatus::Different { cfg_changed },
                    semilla_normal::BodyStatus::BeforeOnly => BodyStatus::BeforeOnly,
                    semilla_normal::BodyStatus::AfterOnly => BodyStatus::AfterOnly,
                };
                compiled.entry(comparison.name.clone()).or_default().push(name.clone());
                if !matches!(status, BodyStatus::Equal { .. }) {
                    differs.entry(comparison.name.clone()).or_default().push(name.clone());
                }
                let merged = match combined.remove(&comparison.name) {
                    Some(old) => combine(old, status),
                    None => status,
                };
                combined.insert(comparison.name, merged);
            }
        }
        Ok(combined
            .into_iter()
            .map(|(name, status)| {
                // The output names the configurations if the body is equal in one or more of
                // them, and not equal in one or more (batch 5u, part A). The combined status is
                // not `Equal`, so the note cannot make a fact neutral.
                let all = compiled.remove(&name).unwrap_or_default();
                let differs_in = differs.remove(&name).filter(|list| list.len() < all.len()).unwrap_or_default();
                let equal_in = if differs_in.is_empty() { Vec::new() } else { all.into_iter().filter(|config| !differs_in.contains(config)).collect() };
                BodyComparison { span: spans.get(&name).cloned(), status, config: config.remove(&name).into_iter().flatten().collect(), differs_in, equal_in, stopped: Vec::new(), name }
            })
            .collect())
    }

    fn truths(&self, step: &Step) -> Result<[Vec<semilla_verify::ConfigTruth>; 2]> {
        let checked = self.checked(step)?;
        Ok([&step.tree_before, &step.tree_after].map(|tree| checked.iter().map(|config| self.truth(tree, config)).collect()))
    }

    fn facts(&self, step: &Step) -> Result<Option<Vec<semilla_verify::BodyFacts>>> {
        // The facts of each checked configuration (batch 5s, A5). The verifier joins the facts
        // of the bodies of an item, so a property claim must hold in each configuration.
        let mut facts = Vec::new();
        for config in self.checked(step)? {
            facts.extend(self.facts_of(step, &config)?);
        }
        Ok(Some(facts))
    }

    fn set_selections(&self, step: &Step, selections: &[semilla_verify::Selection]) -> Result<bool> {
        let list: Vec<Features> = selections
            .iter()
            .map(|selection| {
                let mut features = selection.features.clone();
                features.sort();
                features.dedup();
                if selection.no_default { Features::NoDefaultListed(features) } else { Features::Listed(features) }
            })
            .collect();
        let key = (step.tree_before.clone(), step.tree_after.clone());
        let mut state = self.added.lock().unwrap_or_else(|error| error.into_inner());
        // The log has the data of this list, if this process has it: the output shows the
        // configurations of the last list.
        if let Some((_, _, _, log)) = state.memo.iter().find(|(step, extra, _, _)| *step == key && *extra == list)
            && let Ok(mut all) = self.log.lock()
        {
            all.insert(key.clone(), log.clone());
        }
        if list.is_empty() {
            state.selections.remove(&key);
            return Ok(true);
        }
        state.selections.insert(key, list.clone());
        drop(state);
        // The hint for the next step. An error is not important: the hint has no part in a
        // decision.
        let (path, text) = (self.repo.cache_dir().join(HINT_FILE), hint_text(&list));
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            let _ = std::fs::write(&path, text);
        }
        Ok(true)
    }
}

impl NormalOracle {
    /// The configurations of the default list that Semilla checks in a step: each configuration
    /// that builds before the step. The call builds the N0 records, and keeps the list for the
    /// output.
    fn checked(&self, step: &Step) -> Result<Vec<Configuration>> {
        let key = (step.tree_before.clone(), step.tree_after.clone());
        // The feature selections that the verifier added for a proof in this step (batch 5u,
        // part C). An added configuration is a configuration of the list as each other one.
        let (extra, hint) = {
            let mut state = self.added.lock().unwrap_or_else(|error| error.into_inner());
            let extra = state.selections.get(&key).cloned().unwrap_or_default();
            if let Some((_, _, checked, log)) = state.memo.iter().find(|(step, list, _, _)| *step == key && *list == extra) {
                if let Ok(mut all) = self.log.lock() {
                    all.insert(key, log.clone());
                }
                return Ok(checked.clone());
            }
            // The hint: only with the first builds of a step.
            let hint = if state.hinted.insert(key.clone()) { std::fs::read_to_string(self.repo.cache_dir().join(HINT_FILE)).map(|text| parse_hint(&text)).unwrap_or_default() } else { Vec::new() };
            (extra, hint)
        };
        let listed = self.configurations_with(&step.tree_before, &step.tree_after, &extra)?;
        let configs: Vec<Configuration> = listed.iter().map(|(config, _)| config.clone()).collect();
        // The builds of the hint run with the builds of the list. Their results, and their
        // errors, have no use: only the list decides the checked configurations.
        let mut all = configs.clone();
        for features in hint {
            all.extend([false, true].map(|release| Configuration::new(release, features.clone())).into_iter().filter(|config| !configs.contains(config)));
        }
        let mut built = self.build_each(step, &all);
        built.truncate(configs.len());
        let built: Vec<Option<String>> = built.into_iter().collect::<Result<_>>()?;
        // Batch 5t, part C. A configuration that does not build with each target before the
        // step: Semilla tries the libraries and the binaries only. If this smaller build is
        // good before the step, Semilla checks it. A step that breaks it is a compile error.
        let narrow: Vec<Configuration> = configs.iter().zip(&built).filter(|(_, error)| error.is_some()).map(|(config, _)| config.narrowed()).collect();
        let mut smaller = self.build_all(step, &narrow)?.into_iter();
        let mut log = StepConfigs::default();
        let mut checked = Vec::new();
        for ((config, added), error) in listed.into_iter().zip(built) {
            // The error of the smaller build is not in the output: the error of the build of
            // each target is the cause that the reader can repair.
            let (config, error) = match error {
                Some(error) => match smaller.next() {
                    Some(None) => (config.narrowed(), None),
                    _ => (config, Some(error)),
                },
                None => (config, None),
            };
            match error {
                None => {
                    log.checked.push(config.name());
                    if added {
                        log.added.push(config.name());
                    }
                    log.truth[0].push(self.truth(&step.tree_before, &config));
                    log.truth[1].push(self.truth(&step.tree_after, &config));
                    if log.target.is_empty() {
                        log.target = self.target(&step.tree_after, &config);
                    }
                    checked.push(config);
                }
                Some(error) => log.not_checked.push((config.name(), format!("it does not build before the step: {error}"))),
            }
        }
        if checked.is_empty() {
            // No configuration builds before the step: the error of the first one is the result.
            let config = Configuration::DEV;
            self.records_with(&step.tree_before, "n0", &config)?;
            anyhow::bail!("no configuration of the default list builds before the step");
        }
        if let Ok(mut all) = self.log.lock() {
            all.insert(key.clone(), log.clone());
        }
        self.added.lock().unwrap_or_else(|error| error.into_inner()).memo.push((key, extra, checked.clone(), log));
        Ok(checked)
    }

    /// The MIR facts of the bodies after a step in one configuration.
    fn facts_of(&self, step: &Step, config: &Configuration) -> Result<Vec<semilla_verify::BodyFacts>> {
        // A target name is the name of one build: the walk over the live callees stays in it.
        let tag = |target: String| if *config == Configuration::DEV { target } else { format!("{target} [{}]", config.name()) };
        let records = self.records_with(&step.tree_after, "n0", config)?;
        // The MIR signature of each body before the step, by its name and its target (batch 5n,
        // A1). A body with a new name has no entry: its signature is not a known cause.
        let signature = |text: &str| text.lines().next().unwrap_or("").to_string();
        let old: BTreeMap<(String, String), String> = self.records_with(&step.tree_before, "n0", config)?.into_iter().map(|record| ((record.name, record.target), signature(&record.text))).collect();
        Ok(
            records
                .into_iter()
                .map(|record| semilla_verify::BodyFacts {
                    signature_changed: old.get(&(record.name.clone(), record.target.clone())).is_some_and(|before| *before != signature(&record.text)),
                    span: record.span.as_ref().map(|span| (span.file.clone(), span.line_start)),
                    callees: record.callees,
                    panics: semilla_verify::assert_sites(&record.text),
                    origins: record.externals.iter().filter(|call| !call.krate.is_empty()).map(|call| (call.path.clone(), (call.krate.clone(), call.def.clone()))).collect(),
                    externals: record.externals.into_iter().map(|call| call.path).collect(),
                    indirect: record.indirect,
                    reach_types: record.reach_types.into_iter().map(|t| (t.path, t.krate)).collect(),
                    drop_types: record.drop_types.into_iter().map(|t| (t.path, t.krate)).collect(),
                    drop_calls: record.drop_calls.into_iter().map(|t| (t.path, t.call)).collect(),
                    impl_of: record.impl_of,
                    drop_of: record.drop_of,
                    target: tag(record.target),
                    live: record.live_callees,
                    param_types: record.param_types.into_iter().map(|t| (t.path, t.krate)).collect(),
                    const_types: record.const_types.into_iter().map(|t| (t.path, t.krate)).collect(),
                    casts: semilla_verify::discriminant_casts(&record.text),
                    layouts: semilla_verify::layout_types(&record.text),
                    name: record.name,
                })
                .collect(),
        )
    }
}

/// The SMT checks with Z3. Without a solver, every check gives "not proved".
pub struct SmtOracle {
    solver: Option<semilla_smt::Solver>,
}

impl SmtOracle {
    /// The solver for a check of `repo`. The search never takes a solver from the repository
    /// folder (I5): see [`semilla_smt::Solver::find_for`].
    pub fn for_repo(repo: &Repo) -> SmtOracle {
        SmtOracle { solver: semilla_smt::Solver::find_for(&crate::store::tool_dirs(&repo.root), Some(&repo.root)).ok() }
    }

    /// The path of the solver binary, if one is found.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.solver.as_ref().map(|solver| solver.path())
    }

    pub fn available(&self) -> bool {
        self.solver.is_some()
    }

    fn env(types: &BTreeMap<String, String>) -> semilla_smt::TypeEnv {
        let mut env = semilla_smt::TypeEnv::new();
        for (name, ty) in types {
            env.insert_text(name, ty);
        }
        env
    }
}

impl ExprOracle for SmtOracle {
    fn classify(&self, before: &str, after: &str, types: &BTreeMap<String, String>) -> Option<EffectCheck> {
        let solver = self.solver.as_ref()?;
        let result = semilla_smt::classify_condition(solver, before, after, &Self::env(types)).ok()?;
        let mut witness = result.witness;
        if result.panic_conditions_differ {
            let none = result.notes.iter().find(|n| n.starts_with("no example of the panic difference"));
            let note = match (result.panic_witness, none) {
                (Some(w), _) => format!("the panic conditions differ, for example {w}"),
                (None, Some(none)) => format!("the panic conditions differ ({})", none.trim_start_matches("no example of the panic difference: ").to_string().replace("the integer type of", "no example: the integer type of")),
                (None, None) => "the panic conditions differ".into(),
            };
            witness = Some(match witness {
                Some(w) => format!("{w}; {note}"),
                None => note,
            });
        }
        let effect = if result.panic_conditions_differ && result.effect == Effect::Equivalent { Effect::Unrelated } else { result.effect };
        Some(EffectCheck { effect, witness })
    }

    fn equivalent(&self, before: &str, after: &str, types: &BTreeMap<String, String>) -> Option<bool> {
        let solver = self.solver.as_ref()?;
        match semilla_smt::equivalent(solver, before, after, &Self::env(types)).ok()? {
            semilla_smt::Equivalence::Proved { .. } => Some(true),
            semilla_smt::Equivalence::Refuted { .. } => Some(false),
            semilla_smt::Equivalence::NotProved { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn equal(level: &str) -> BodyStatus {
        BodyStatus::Equal { level: level.into(), proof: Vec::new() }
    }

    // Batch 5n, A4: the binary and a bench have the crate name `demo`, and each has a `main`.
    // Only the `main` of the binary changed, so its span is the span of the name.
    #[test]
    fn the_span_of_a_name_in_two_targets_is_the_span_of_the_changed_target() {
        let record = |target: &str, file: &str, text: &str, name: &str| {
            let line = serde_json::json!({ "name": name, "level": "n0", "text": text, "target": target, "span": { "file": file, "line_start": 3, "line_end": 5 } });
            semilla_normal::parse_records(&line.to_string()).unwrap().remove(0)
        };
        let old = vec![record("demo.bin.1", "src/main.rs", "a", "demo::main"), record("demo.bench.2", "benches/demo.rs", "b", "demo::main"), record("demo.bin.1", "src/main.rs", "c", "demo::only")];
        let new = vec![record("demo.bin.1", "src/main.rs", "a2", "demo::main"), record("demo.bench.2", "benches/demo.rs", "b", "demo::main"), record("demo.bin.1", "src/main.rs", "c2", "demo::only")];
        assert_eq!(changed_target_spans(&old, &new), [("demo::main".to_string(), ("src/main.rs".to_string(), 3))]);
        assert!(changed_target_spans(&old, &old).is_empty());
    }

    // Batch 5s, A5: the status of a body over two configurations, for each pair of states.
    #[test]
    fn a_body_is_equal_only_in_every_configuration() {
        let different = |cfg_changed: bool| BodyStatus::Different { cfg_changed };
        assert_eq!(combine(equal("n0"), equal("n2")), equal("n2"));
        assert_eq!(combine(equal("n0"), different(false)), different(false));
        assert_eq!(combine(different(true), equal("n0")), different(true));
        assert_eq!(combine(different(false), different(true)), different(true));
        assert_eq!(combine(BodyStatus::BeforeOnly, BodyStatus::BeforeOnly), BodyStatus::BeforeOnly);
        assert_eq!(combine(BodyStatus::AfterOnly, BodyStatus::AfterOnly), BodyStatus::AfterOnly);
        // Each mixed pair is a changed body: no pair gives `Equal`, and the order has no effect.
        let states = [equal("n0"), different(false), BodyStatus::BeforeOnly, BodyStatus::AfterOnly];
        for (i, a) in states.iter().enumerate() {
            for (j, b) in states.iter().enumerate() {
                let result = combine(a.clone(), b.clone());
                assert_eq!(matches!(result, BodyStatus::Equal { .. }), i == 0 && j == 0, "{a:?} + {b:?}");
                assert_eq!(matches!(result, BodyStatus::Different { .. }), i != j || i == 1, "{a:?} + {b:?}");
            }
        }
    }

    #[test]
    fn each_configuration_has_a_name_and_a_folder_suffix() {
        let config = Configuration::new;
        assert_eq!(Configuration::DEV.name(), "dev");
        assert_eq!(Configuration::DEV.suffix(), "");
        assert_eq!(config(true, Features::Default).name(), "release");
        assert_eq!(config(false, Features::NoDefault).name(), "dev, no default features");
        assert_eq!(config(true, Features::All).name(), "release, all features");
        assert_eq!(config(false, Features::Listed(vec!["a".into(), "b".into()])).name(), "dev, features a,b");
        let all = [
            Configuration::DEV,
            config(true, Features::Default),
            config(false, Features::NoDefault),
            config(true, Features::NoDefault),
            config(false, Features::All),
            config(true, Features::All),
            config(false, Features::Listed(vec!["serde".into()])),
            config(false, Features::Listed(vec!["std".into()])),
        ];
        let suffixes: std::collections::BTreeSet<String> = all.iter().map(Configuration::suffix).collect();
        assert_eq!(suffixes.len(), all.len());
        // Batch 5t, part C: the smaller build has its own name, folders, and cargo arguments.
        let narrow = config(false, Features::NoDefault).narrowed();
        assert_eq!(narrow.name(), "dev, no default features (libraries and binaries only)");
        assert_eq!(narrow.suffix(), "-nd-lb");
        assert_eq!(narrow.target_args(), ["--lib", "--bins"]);
        assert_eq!(Configuration::DEV.target_args(), ["--all-targets"]);
        assert_eq!(Configuration::NARROW, semilla_core::report::ConfigNote::NARROW);
    }

    // Batch 5s, A2: a feature selection with the feature state of an earlier one gives no build.
    #[test]
    fn the_feature_state_comes_from_the_manifests() {
        use semilla_normal::cargo::feature_state;
        let metadata = serde_json::json!({ "packages": [
            { "name": "plain", "features": {}, "dependencies": [] },
            { "name": "rich", "features": { "default": ["std"], "std": [], "extra": ["std"] }, "dependencies": [{ "name": "serde", "optional": true }] },
        ]});
        let state = |features: Features| feature_state(&metadata, &features);
        let set = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<std::collections::BTreeSet<String>>();
        assert_eq!(state(Features::Default)["plain"], set(&[]));
        assert_eq!(state(Features::NoDefault)["plain"], state(Features::All)["plain"]);
        assert_eq!(state(Features::Default)["rich"], set(&["default", "std"]));
        assert_eq!(state(Features::NoDefault)["rich"], set(&[]));
        assert_eq!(state(Features::All)["rich"], set(&["default", "std", "extra", "dep:serde"]));
        assert_eq!(state(Features::Listed(vec!["extra".into()]))["rich"], set(&["default", "std", "extra"]));
        // `std` is a default feature: the listed set is the default set.
        assert_eq!(state(Features::Listed(vec!["std".into()])), state(Features::Default));
        // Batch 5u, part C: a listed set with no default features.
        assert_eq!(state(Features::NoDefaultListed(vec!["extra".into()]))["rich"], set(&["std", "extra"]));
        assert_eq!(state(Features::NoDefaultListed(vec!["extra".into()]))["plain"], set(&[]));
    }

    // Batch 5u, part C.
    #[test]
    fn a_selection_with_no_default_features_has_its_own_name_and_folder() {
        let config = Configuration::new(false, Features::NoDefaultListed(vec!["a".into(), "b".into()]));
        assert_eq!(config.name(), "dev, no default features, features a,b");
        assert_eq!(Configuration::new(true, Features::NoDefaultListed(vec!["a".into()])).name(), "release, no default features, features a");
        let suffixes: std::collections::BTreeSet<String> = [Features::NoDefault, Features::Listed(vec!["a".into(), "b".into()]), Features::NoDefaultListed(vec!["a".into(), "b".into()]), Features::NoDefaultListed(vec!["a".into()])].into_iter().map(|features| Configuration::new(false, features).suffix()).collect();
        assert_eq!(suffixes.len(), 4, "{suffixes:?}");
    }

    // Batch 5u, part C: the hint is data from a file that a different program can change.
    #[test]
    fn a_hint_with_a_bad_form_gives_no_selection() {
        let list = vec![Features::Listed(vec!["fast".into()]), Features::NoDefaultListed(vec!["a".into(), "b".into()])];
        let text = hint_text(&list);
        assert_eq!(text, r#"[{"no_default":false,"features":["fast"]},{"no_default":true,"features":["a","b"]}]"#);
        assert_eq!(parse_hint(&text), list);
        // A selection of the default list is not a part of the hint.
        assert_eq!(hint_text(&[Features::All, Features::Default]), "[]");
        for bad in [
            "",
            "{}",
            "[1]",
            r#"[{"features":["fast"]}]"#,
            r#"[{"no_default":0,"features":["fast"]}]"#,
            r#"[{"no_default":false,"features":[]}]"#,
            r#"[{"no_default":false,"features":["b","a"]}]"#,
            r#"[{"no_default":false,"features":["a --release"]}]"#,
            r#"[{"no_default":false,"features":["a,b"]}]"#,
            r#"[{"no_default":false,"features":[""]}]"#,
            r#"[{"no_default":false,"features":["a","b","c","d","e","f","g","h","i"]}]"#,
            r#"[{"no_default":false,"features":["a"]},{"no_default":false,"features":["b"]},{"no_default":false,"features":["c"]},{"no_default":false,"features":["d"]},{"no_default":false,"features":["e"]}]"#,
            r#"[{"no_default":false,"features":["a"]},{"no_default":false,"features":"b"}]"#,
        ] {
            assert_eq!(parse_hint(bad), Vec::new(), "{bad}");
        }
    }
}
