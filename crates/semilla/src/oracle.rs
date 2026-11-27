//! The adapters from the verifier traits to the real normal forms and the real SMT solver.

use std::collections::BTreeMap;

use anyhow::Result;
use semilla_core::Step;
use semilla_core::report::Effect;
use semilla_normal::cargo::{Configuration, Features};
use semilla_verify::{BodyComparison, BodyOracle, BodyStatus, EffectCheck, ExprOracle};

use crate::git::Repo;

#[path = "reach.rs"]
pub(crate) mod reach;

#[path = "intent.rs"]
mod intent;

#[path = "suggest.rs"]
pub(crate) mod suggest;

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
    /// Design 62: the check of a tag after the commit. A configuration that does not build
    /// after the step is a configuration with no check, not an error of the step.
    pub after_commit: bool,
}

/// Batch 5x, part C (trial 7): the build does not find a module file (`E0583`), and the file is
/// in the folder of the repository. Git ignores the file, or the tree of the step does not have
/// it. Semilla builds a tree of git, so the note says the cause.
pub(crate) fn missing_module_note(repo: &Repo, error: &str, tree: &str) -> Option<String> {
    if !error.contains("E0583") {
        return None;
    }
    let mut notes: Vec<String> = Vec::new();
    // The help line of the compiler: `create file "src/x.rs" or "src/x/mod.rs"`.
    for line in error.lines().filter(|line| line.contains("help: to create the module")) {
        for path in line.split('"').skip(1).step_by(2) {
            if !repo.root.join(path).is_file() || repo.git(&["cat-file", "-e", &format!("{tree}:{path}")]).is_ok() {
                continue;
            }
            let note = if repo.git(&["check-ignore", "-q", "--", path]).is_ok() {
                format!("`{path}` is in the folder, but git ignores it (`git check-ignore -v {path}` shows the rule). Semilla builds only files that git tracks or that are not ignored.")
            } else {
                format!("`{path}` is in the folder, but it is not in the tree that Semilla builds. Semilla builds the tree of the step, not the folder.")
            };
            if !notes.contains(&note) {
                notes.push(note);
            }
        }
    }
    (!notes.is_empty()).then(|| notes.join(" "))
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
    /// The comparison of one configuration of a step, by its inputs (batch 5x, part F). The
    /// verifier asks for the same comparison more than one time. The comparison is a function
    /// of the records of the two trees, the rename map, the added items, and the marks.
    compared: BTreeMap<CompareKey, Compared>,
    /// The records of the two trees of a step in one configuration (design 37.20), by the two
    /// trees and the configuration.
    sides: BTreeMap<SidesKey, std::sync::Arc<Sides>>,
    /// The rename map with the moved closures, by the record set, the rename map, and the
    /// added items (design 37.20).
    moved: Vec<(SidesKey, BTreeMap<String, String>, Vec<String>, BTreeMap<String, String>)>,
    /// The variant trees of the equivalence check of one fact (design 61, part D): each has the
    /// build code of the base, because the two trees of its step have it.
    variants: std::collections::BTreeSet<String>,
    /// The walk of the reverse call graph of each configuration of a step (design 61.10, part
    /// A), by the two trees of the step.
    reached: BTreeMap<(String, String), std::sync::Arc<Vec<(String, reach::ConfigReach)>>>,
    /// [R79.2-1] The compile of the intent crate of a step, by the two trees and the texts of the
    /// intent files: the AST pass and the check of the claims read one compile.
    intent_checks: BTreeMap<(String, String, u64), std::sync::Arc<std::result::Result<crate::intentcrate::Check, String>>>,
}

/// The key of a record set: the two trees of a step, and the configuration.
type SidesKey = (String, String, Configuration);

/// The most record sets in memory: the configurations of two steps.
const MOST_SIDES: usize = 16;

/// The records of the two trees of a step in one configuration, with the data that each compare
/// of the configuration reads (design 37.20). The verifier asks for the compare of a
/// configuration many times in one step. The oracle makes this data one time for the record set.
#[derive(Clone, Debug)]
struct Sides {
    before: semilla_normal::Levels,
    after: semilla_normal::Levels,
    /// The span and the expanded names of each N0 record, in the order of the records: the
    /// records of the tree before the step, then those of the tree after it.
    inputs: [Vec<RecordInputs>; 2],
    /// The result of [`Sides::names_with_two_places`].
    two_places: Vec<String>,
    /// The result of [`Sides::names_with_two_files`].
    two_files: Vec<String>,
}

/// The span and the expanded names of one record.
type RecordInputs = (Option<(String, usize)>, Vec<semilla_verify::ExpandedName>);

impl Sides {
    fn new(before: semilla_normal::Levels, after: semilla_normal::Levels) -> Sides {
        let inputs = [&before, &after].map(|levels| {
            let records = levels.get("n0").into_iter().flatten();
            records
                .map(|record| {
                    let span = record.span.as_ref().map(|span| (span.file.clone(), span.line_start));
                    let names = record
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
                    (span, names)
                })
                .collect()
        });
        let mut sides = Sides { before, after, inputs, two_places: Vec::new(), two_files: Vec::new() };
        sides.two_places = sides.names_with_two_places();
        sides.two_files = sides.names_with_two_files();
        sides
    }

    /// The N0 records of a side, each with its span and its expanded names.
    fn records(&self, is_after: bool) -> impl Iterator<Item = (&semilla_normal::BodyRecord, &RecordInputs)> {
        let levels = if is_after { &self.after } else { &self.before };
        levels.get("n0").into_iter().flatten().zip(&self.inputs[usize::from(is_after)])
    }

    /// The body names with two places or more in the records of one side of this
    /// configuration (two targets with one crate name, or a twin under `cfg(test)`).
    fn names_with_two_places(&self) -> Vec<String> {
        let mut found = Vec::new();
        for side in [&self.before, &self.after] {
            let mut places: BTreeMap<&str, (&str, usize)> = BTreeMap::new();
            for record in side.get("n0").into_iter().flatten() {
                let Some(span) = &record.span else { continue };
                let place = (span.file.as_str(), span.line_start);
                if places.insert(record.name.as_str(), place).is_some_and(|old| old != place) {
                    found.push(record.name.clone());
                }
            }
        }
        found
    }

    /// The body names whose targets have two source files or more in one tree of this
    /// configuration: each file is a source body of its own (R61.14-1). The file of a target is
    /// the file of its first N0 record with a span, as in the compare (`semilla-normal`): the
    /// records of one target can have spans in two files (a closure from a macro of a
    /// dependency). A body that moves to a different file has one file in each tree, so it is not
    /// in the list.
    fn names_with_two_files(&self) -> Vec<String> {
        let mut found = std::collections::BTreeSet::new();
        for side in [&self.before, &self.after] {
            for (name, files) in target_files(side) {
                if files.values().collect::<std::collections::BTreeSet<_>>().len() > 1 {
                    found.insert(name.to_string());
                }
            }
        }
        found.into_iter().collect()
    }
}

/// The inputs of the comparison of one configuration: the two trees, the configuration, the
/// rename map, the added items, and the marks of the two sides (opaque, then stop).
type CompareKey = (String, String, Configuration, BTreeMap<String, String>, Vec<String>, [std::collections::BTreeSet<String>; 4]);

/// The result of the comparison of one configuration: the comparisons and the spans.
type Compared = (Vec<semilla_normal::BodyComparison>, BTreeMap<String, (String, usize)>);

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
    /// Each configuration of the default list that is not in the list of the step, with the
    /// configuration that has the same feature state (batch 5y, C4.4). Only the `dev` names.
    pub same: Vec<(String, String)>,
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

/// The cause in the output for a configuration with a feature that only one tree of the step
/// has (design 37.7). `before` and `after`: the features of the selection that the tree does
/// not have. `None`: the two trees have each feature, or one feature is in no tree.
fn one_side_cause(before: &[String], after: &[String]) -> Option<String> {
    let (side, other, missing) = match (before.is_empty(), after.is_empty()) {
        (true, false) => ("before", "after", after),
        (false, true) => ("after", "before", before),
        _ => return None,
    };
    let names: Vec<String> = missing.iter().map(|name| format!("`{name}`")).collect();
    let noun = if names.len() == 1 { "feature" } else { "features" };
    Some(format!("only the code {side} the step has the {noun} {}. The code {other} the step has no such configuration: no compare", names.join(", ")))
}

/// The causes of a configuration with no check: the side that does not build (design 62).
pub const BEFORE_SIDE: &str = "it does not build before the step";
pub const AFTER_SIDE: &str = "it does not build after the step";
pub const BOTH_SIDES: &str = "it does not build before the step and after it";

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
    ///
    /// The second result (batch 5y, C4.4): each selection of the default list that is not in
    /// the list, with the selection that has the same feature state. An entry has the names
    /// of the two `dev` configurations. The `release` configurations have the same relation.
    #[allow(clippy::type_complexity)]
    fn configurations_with(&self, before: &str, after: &str, extra: &[Features]) -> Result<(Vec<(Configuration, bool)>, Vec<(String, String)>)> {
        let mut selections = vec![(Features::Default, false), (Features::NoDefault, false), (Features::All, false)];
        selections.extend(self.feature_sets(&[before, after])?.into_iter().filter(|set| !set.is_empty()).map(|set| (Features::Listed(set), false)));
        selections.extend(extra.iter().map(|features| (features.clone(), true)));
        let metadata = [self.metadata(before), self.metadata(after)];
        let mut seen: Vec<(Vec<_>, Features)> = Vec::new();
        let mut kept = Vec::new();
        let mut same = Vec::new();
        for (selection, added) in selections {
            // A tree with manifests that cargo cannot read has no state: the selection stays.
            let state: Vec<_> = metadata.iter().map(|data| data.as_ref().map(|data| semilla_normal::cargo::feature_state(data, &selection))).collect();
            if state.iter().all(Option::is_some) {
                if let Some((_, first)) = seen.iter().find(|(known, _)| *known == state) {
                    if !added {
                        same.push((Configuration::new(false, selection.clone()).name(), Configuration::new(false, first.clone()).name()));
                    }
                    continue;
                }
            }
            seen.push((state, selection.clone()));
            kept.push((selection, added));
        }
        Ok((kept.into_iter().flat_map(|(features, added)| [false, true].map(|release| (Configuration::new(release, features.clone()), added))).collect(), same))
    }

    /// The cache folder of the records of a tree at a level and a configuration.
    pub(crate) fn cache_folder(&self, tree: &str, level: &str, config: &Configuration) -> Result<(std::path::PathBuf, std::path::PathBuf, bool, String)> {
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
        if self.code.clean.contains(tree) || self.added.lock().unwrap_or_else(|error| error.into_inner()).variants.contains(tree) {
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
        self.records_or_build(tree, level, config, true)
    }

    /// Makes the records of a tree at a level and a configuration, and reads them with `read`.
    /// A caller that needs only the build reads nothing (batch 5x, part F): the result is empty.
    pub(crate) fn records_or_build(&self, tree: &str, level: &str, config: &Configuration, read: bool) -> Result<Vec<semilla_normal::BodyRecord>> {
        // Design 62: the tree before a root commit is the empty tree. It has no body, so it has
        // no record, and no build runs.
        if crate::git::is_empty_tree(tree) {
            return Ok(Vec::new());
        }
        let read_cached = |cache: &std::path::Path| if read { read_cached(cache) } else { Ok(Vec::new()) };
        // The cache key: the tree, the level, the configuration, and the build settings that are
        // not in the tree (the outer cargo configuration and the driver binary).
        let (root, cache, marked, name) = self.cache_folder(tree, level, config)?;
        let base = root.join("build");
        // A record with the mark is never a record of the shared folders.
        if !marked && cache.join(BUILD_CODE_FILE).exists() {
            remove_cache(&cache);
        }
        // A compile error of this tree in this configuration: the same build gives it again.
        if let Ok(error) = std::fs::read_to_string(cache.join(FAILED_FILE)) {
            anyhow::bail!("{error}");
        }
        if cache.join(".complete").exists() {
            note_sandbox(&cache);
            return read_cached(&cache);
        }
        std::fs::create_dir_all(&base)?;
        // One build folder serves one build at a time, also across processes.
        let lock = std::fs::File::create(base.join(format!("{name}.lock")))?;
        lock.lock()?;
        // Another process can finish the same build while this one waits for the lock.
        if cache.join(".complete").exists() {
            lock.unlock()?;
            note_sandbox(&cache);
            return read_cached(&cache);
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
                    remove_cache(&cache);
                    std::fs::create_dir_all(&cache)?;
                    std::fs::write(cache.join(FAILED_FILE), &text)?;
                }
                return Err(error);
            }
        };
        crate::timing(&format!("cargo check {name} of tree {}", &tree[..8.min(tree.len())]), started);
        remove_cache(&cache);
        std::fs::create_dir_all(&cache)?;
        // The cache gets only the record file of each unit of this build (design 28.8). Each
        // other file of the records folder stays out, and the result names it. A program of
        // the build decides the name: each hidden character becomes an escape.
        for file in &built.records {
            std::fs::copy(records.join(file), cache.join(file))?;
        }
        // [R78.1-2] The intent crate (design 78) compiles against the metadata of the N0 build.
        // The build of the other tree in the same folder writes a file with the same name, so
        // the metadata of each member goes into the cache.
        if level == "n0" {
            semilla_normal::intent::save_libraries(&cache, &built)?;
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
        read_cached(&cache)
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
    /// The result for each configuration is `None` (the two trees build), or the cause with the
    /// first error line (the tree before the step does not build: Semilla does not check the
    /// configuration in this step). A configuration that builds before the step and not after
    /// it is a compile error of the step (batch 5s, A4). In a tag after the commit (design 62),
    /// it is a configuration with no check, and the cause names the side.
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
                        if let Err(error) = self.records_or_build(&step.tree_before, "n0", config, false) {
                            let text = format!("{error:#}");
                            if !is_compile_error(&text) {
                                return Err(error);
                            }
                            // Design 62: a tag after the commit names each side that does not build.
                            if self.after_commit {
                                if let Err(after) = self.records_or_build(&step.tree_after, "n0", config, false) {
                                    if is_compile_error(&format!("{after:#}")) {
                                        return Ok(Some(format!("{BOTH_SIDES}: {}", first_error_line(&text))));
                                    }
                                }
                            }
                            return Ok(Some(format!("{BEFORE_SIDE}: {}", first_error_line(&text))));
                        }
                        match self.records_or_build(&step.tree_after, "n0", config, false) {
                            Ok(_) => Ok(None),
                            Err(error) if self.after_commit && is_compile_error(&format!("{error:#}")) => Ok(Some(format!("{AFTER_SIDE}: {}", first_error_line(&format!("{error:#}"))))),
                            Err(error) if is_compile_error(&format!("{error:#}")) => {
                                // A module file that the folder has and the tree does not have.
                                let ignored = missing_module_note(&self.repo, &format!("{error:#}"), &step.tree_after).map(|note| format!(". {note} The output of the build")).unwrap_or_default();
                                Err(error.context(format!("the code after the step does not compile in the configuration `{}` (it compiles before the step){ignored}", config.name())))
                            }
                            Err(error) => Err(error),
                        }
                    })
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a build thread failed")))).collect()
        })
    }

    /// The records of the two trees of a step in one configuration, from memory or from
    /// [`BodyCache::levels`] (design 37.20).
    fn sides(&self, step: &Step, features: &Configuration) -> Result<std::sync::Arc<Sides>> {
        let key: SidesKey = (step.tree_before.clone(), step.tree_after.clone(), features.clone());
        let known = self.added.lock().unwrap_or_else(|error| error.into_inner()).sides.get(&key).cloned();
        if let Some(sides) = known {
            return Ok(sides);
        }
        let (before, after) = self.levels(&step.tree_before, &step.tree_after, &["n0"], features)?;
        let sides = std::sync::Arc::new(Sides::new(before, after));
        let mut added = self.added.lock().unwrap_or_else(|error| error.into_inner());
        if added.sides.len() >= MOST_SIDES {
            added.sides.clear();
            added.moved.clear();
        }
        added.sides.insert(key, sides.clone());
        Ok(sides)
    }

    /// Reads the record set of each configuration that is not in memory, each in its own
    /// thread (design 37.20). An error stays for the caller that asks for the record set.
    fn read_sides(&self, step: &Step, configs: &[Configuration]) {
        let known = |config: &Configuration| self.added.lock().unwrap_or_else(|error| error.into_inner()).sides.contains_key(&(step.tree_before.clone(), step.tree_after.clone(), config.clone()));
        let missing: Vec<&Configuration> = configs.iter().filter(|config| !known(config)).collect();
        if missing.len() < 2 || missing.len() > MOST_SIDES {
            return;
        }
        std::thread::scope(|scope| {
            for config in missing {
                scope.spawn(move || {
                    let _ = self.sides(step, config);
                });
            }
        });
    }

    /// [`with_moved_closures`] for a record set, from memory if the rename map and the added
    /// items are those of an earlier call (design 37.20).
    fn moved_closures(&self, step: &Step, features: &Configuration, renames: &BTreeMap<String, String>, added: &[String], sides: &Sides) -> BTreeMap<String, String> {
        let key: SidesKey = (step.tree_before.clone(), step.tree_after.clone(), features.clone());
        let fits = |entry: &&(SidesKey, BTreeMap<String, String>, Vec<String>, BTreeMap<String, String>)| entry.0 == key && entry.1 == *renames && entry.2 == added;
        if let Some(entry) = self.added.lock().unwrap_or_else(|error| error.into_inner()).moved.iter().find(fits) {
            return entry.3.clone();
        }
        let made = with_nested_pairs(&with_moved_closures(renames, added, &sides.before, &sides.after), &sides.before, &sides.after);
        self.added.lock().unwrap_or_else(|error| error.into_inner()).moved.push((key, renames.clone(), added.to_vec(), made.clone()));
        made
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
        let sides = self.sides(step, features)?;
        let (before, after) = (&sides.before, &sides.after);
        let renames = self.moved_closures(step, features, renames, added, &sides);
        let renames = &renames;
        let started = std::time::Instant::now();
        // The bodies that the build configuration selects, by the syntax tree (batch 5l). No
        // normal form looks into them.
        for is_after in [false, true] {
            for (record, (span, names)) in sides.records(is_after) {
                opaque.note(is_after, &record.name, span.as_ref(), &record.adts);
                opaque.body(is_after, &record.name, span.as_ref(), index, &record.target);
                opaque.names(is_after, &record.name, span.as_ref(), names);
            }
        }
        let marks = |is_after: bool| {
            let mut marks = semilla_normal::Marks::default();
            for (record, (span, _)) in sides.records(is_after) {
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
        let (before_marks, after_marks) = (marks(false), marks(true));
        MARK_TIME.add(started);
        // The same comparison of an earlier call (batch 5x, part F): the result is in memory.
        let key: CompareKey = (step.tree_before.clone(), step.tree_after.clone(), features.clone(), renames.clone(), added.to_vec(), [before_marks.opaque.clone(), before_marks.stop.clone(), after_marks.opaque.clone(), after_marks.stop.clone()]);
        let known = self.added.lock().unwrap_or_else(|error| error.into_inner()).compared.get(&key).cloned();
        if let Some((comparisons, spans)) = known {
            return Ok(SetState { config: features.clone(), renames: renames.clone(), sides, before_marks, after_marks, comparisons, done: Some(spans) });
        }
        let started = std::time::Instant::now();
        let comparisons = semilla_normal::compare_with(before, after, renames, added, &before_marks, &after_marks);
        N0_TIME.add(started);
        Ok(SetState { config: features.clone(), renames: renames.clone(), sides, before_marks, after_marks, comparisons, done: None })
    }

}

/// The totals of the time of the phases that run for each configuration (batch 5x, part F).
pub(crate) static READ_TIME: crate::TimeTotal = crate::TimeTotal::new();
pub(crate) static N0_TIME: crate::TimeTotal = crate::TimeTotal::new();
pub(crate) static MARK_TIME: crate::TimeTotal = crate::TimeTotal::new();
/// The time of the parts of the body checks of the equivalence check (design 66, part D).
pub(crate) static FORM_TIME: crate::TimeTotal = crate::TimeTotal::new();
pub(crate) static STD_FORM_TIME: crate::TimeTotal = crate::TimeTotal::new();
pub(crate) static SOLVE_TIME: crate::TimeTotal = crate::TimeTotal::new();

/// The key of the records in memory: the cache folder and the time of its `.complete` file.
type FolderKey = (std::path::PathBuf, std::time::SystemTime);

/// The records of the cache folders that this process read last (batch 5x, part F). One check
/// of a step reads the same folder many times. The time of the `.complete` file is a part of
/// the key: a folder that a build made again is a new entry.
static RECORDS: std::sync::Mutex<BTreeMap<FolderKey, std::sync::Arc<Vec<semilla_normal::BodyRecord>>>> = std::sync::Mutex::new(BTreeMap::new());

/// The most folders with records in memory: the folders of two steps with four configurations.
const MOST_FOLDERS: usize = 48;

/// Removes a cache folder, and its records in memory.
fn remove_cache(cache: &std::path::Path) {
    RECORDS.lock().unwrap_or_else(|error| error.into_inner()).retain(|(folder, _), _| folder != cache);
    let _ = std::fs::remove_dir_all(cache);
}

/// Reads the records of a complete cache folder.
fn read_cached(cache: &std::path::Path) -> Result<Vec<semilla_normal::BodyRecord>> {
    let key = std::fs::metadata(cache.join(".complete")).and_then(|file| file.modified()).ok().map(|time| (cache.to_path_buf(), time));
    if let Some(key) = &key {
        let known = RECORDS.lock().unwrap_or_else(|error| error.into_inner()).get(key).cloned();
        if let Some(records) = known {
            return Ok(records.as_ref().clone());
        }
    }
    let started = std::time::Instant::now();
    let records = semilla_normal::read_records(cache);
    READ_TIME.add(started);
    if let (Some(key), Ok(records)) = (key, &records) {
        let mut folders = RECORDS.lock().unwrap_or_else(|error| error.into_inner());
        if folders.len() >= MOST_FOLDERS {
            folders.clear();
        }
        folders.insert(key, std::sync::Arc::new(records.clone()));
    }
    records
}

/// The comparison of one configuration of a step.
struct SetState {
    config: Configuration,
    renames: BTreeMap<String, String>,
    sides: std::sync::Arc<Sides>,
    before_marks: semilla_normal::Marks,
    after_marks: semilla_normal::Marks,
    comparisons: Vec<semilla_normal::BodyComparison>,
    /// The spans of a comparison that an earlier call did: the comparisons are complete.
    done: Option<BTreeMap<String, (String, usize)>>,
}

impl SetState {
    /// The span of each body: the "after" records win (the name is the "after" name), and the
    /// span of a changed target wins over each other span (batch 5n, A4). A name with two spans
    /// in one tree gets the first span in the order of the places, so the span does not depend
    /// on the order of the record files (R61.16-2).
    fn spans(&self, spans: &mut BTreeMap<String, (String, usize)>) {
        let mut found: BTreeMap<String, (String, usize)> = BTreeMap::new();
        for levels in [&self.sides.after, &self.sides.before] {
            let side = first_places(levels.values().flatten().filter_map(|record| record.span.as_ref().map(|span| (record.name.as_str(), (span.file.clone(), span.line_start)))));
            for (name, place) in side {
                found.entry(name.to_string()).or_insert(place);
            }
        }
        if let (Some(old), Some(new)) = (self.sides.before.get("n0"), self.sides.after.get("n0")) {
            let changed = changed_target_spans(old, new);
            found.extend(first_places(changed.iter().map(|(name, place)| (name.as_str(), place.clone()))).into_iter().map(|(name, place)| (name.to_string(), place)));
        }
        spans.extend(found);
    }
}

/// The source file of each target of each name in the N0 records of a tree: the file of the
/// first record of the target with a span (as `Group::files` of `semilla-normal`).
fn target_files(levels: &semilla_normal::Levels) -> BTreeMap<&str, BTreeMap<&str, &str>> {
    let mut found: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    for record in levels.get("n0").into_iter().flatten() {
        if let Some(span) = &record.span {
            found.entry(record.name.as_str()).or_default().entry(record.target.as_str()).or_insert(span.file.as_str());
        }
    }
    found
}

/// The first place of each name in the order of the places (R61.16-2): the result does not
/// depend on the order of the input.
fn first_places<'a>(places: impl Iterator<Item = (&'a str, (String, usize))>) -> BTreeMap<&'a str, (String, usize)> {
    let mut first: BTreeMap<&str, (String, usize)> = BTreeMap::new();
    for (name, place) in places {
        match first.get_mut(name) {
            Some(known) if place < *known => *known = place,
            Some(_) => {}
            None => {
                first.insert(name, place);
            }
        }
    }
    first
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

/// R72.1-1 (part f): the nested bodies of one body (closures, coroutines, constant blocks) pair by
/// the one matcher, not by their numbers (`{closure#0}` is the source order): equal MIR texts at
/// N0 (the old text through the rename map, the own name as one mark), then the best similarity of
/// the texts over [`NESTED_THRESHOLD`], then the source order for a tie. A pair with two numbers
/// goes into the rename map of the compare. A group where an old body with no pair has the name
/// that a pair gives keeps the names (a limit: design 72.6).
fn with_nested_pairs(renames: &BTreeMap<String, String>, before: &semilla_normal::Levels, after: &semilla_normal::Levels) -> BTreeMap<String, String> {
    use semilla_core::matcher::{Ladder, Thing};
    let mut out = renames.clone();
    let (Some(old), Some(new)) = (before.get("n0"), after.get("n0")) else { return out };
    let nested = |name: &str| -> Option<(String, String)> {
        let (parent, last) = name.rsplit_once("::")?;
        last.starts_with('{').then(|| (parent.to_string(), last.chars().filter(|c| !c.is_ascii_digit()).collect()))
    };
    let number = |name: &str| -> usize { name.rsplit_once('#').and_then(|(_, rest)| rest.trim_end_matches('}').parse().ok()).unwrap_or(0) };
    let list: Vec<(String, String)> = renames.iter().map(|(from, to)| (from.clone(), to.clone())).collect();
    type Groups<'r> = BTreeMap<(String, String), [Vec<&'r semilla_normal::BodyRecord>; 2]>;
    let mut groups: Groups = BTreeMap::new();
    let mut seen: [std::collections::BTreeSet<&str>; 2] = Default::default();
    for (side, records) in [old, new].into_iter().enumerate() {
        for record in records {
            if !seen[side].insert(record.name.as_str()) || (side == 0 && renames.contains_key(&record.name)) {
                continue;
            }
            let Some((parent, kind)) = nested(&record.name) else { continue };
            let parent = if side == 0 { semilla_normal::paths::rename(&parent, &list) } else { parent };
            groups.entry((parent, kind)).or_default()[side].push(record);
        }
    }
    for [olds, news] in groups.values_mut() {
        if olds.is_empty() || news.is_empty() {
            continue;
        }
        olds.sort_by_key(|record| number(&record.name));
        news.sort_by_key(|record| number(&record.name));
        let text = |record: &semilla_normal::BodyRecord, old: bool| -> String {
            let own = [(record.name.clone(), "{self}".to_string())];
            let text = semilla_normal::paths::rename(&record.text, &own);
            if old { semilla_normal::paths::rename(&text, &list) } else { text }
        };
        let (old_texts, new_texts): (Vec<String>, Vec<String>) = (olds.iter().map(|record| text(record, true)).collect(), news.iter().map(|record| text(record, false)).collect());
        let thing = |text: &String| Thing { group: String::new(), identity: None, content: Some(text.clone()) };
        let (old_things, new_things): (Vec<Thing>, Vec<Thing>) = (old_texts.iter().map(thing).collect(), new_texts.iter().map(thing).collect());
        let words = |text: &String| text.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        let (old_words, new_words): (Vec<Vec<String>>, Vec<Vec<String>>) = (old_texts.iter().map(words).collect(), new_texts.iter().map(words).collect());
        let similarity = |a: usize, b: usize| semilla_core::matcher::token_similarity(&old_words[a], &new_words[b]);
        let ladder = Ladder { similarity: &similarity, threshold: NESTED_THRESHOLD, in_order: None };
        let pairs = semilla_core::matcher::pair(&old_things, &new_things, &ladder);
        let map: BTreeMap<String, String> = pairs.iter().filter(|pair| olds[pair.old].name != news[pair.new].name).map(|pair| (olds[pair.old].name.clone(), news[pair.new].name.clone())).collect();
        if map.is_empty() {
            continue;
        }
        // An old body with no pair keeps its name: no pair may give that name to a different body.
        let paired: std::collections::BTreeSet<usize> = pairs.iter().map(|pair| pair.old).collect();
        let collision = olds.iter().enumerate().any(|(at, record)| !paired.contains(&at) && map.values().any(|to| *to == record.name));
        if !collision {
            out.extend(map);
        }
    }
    out
}

/// The smallest similarity of a pair of two nested bodies by similarity (Decided by Claude,
/// design 72.4).
const NESTED_THRESHOLD: f64 = 0.5;

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

/// The MIR elements that differ, in the form of the verifier.
fn mir_diff(diff: semilla_normal::elements::ElementDiff) -> semilla_verify::MirDiff {
    let place = |place: Option<semilla_normal::elements::Place>| place.map(|place| (place.file, place.start, place.end));
    let symbol = |symbol: semilla_normal::elements::Symbol| semilla_verify::Sym { path: symbol.path, place: place(symbol.place) };
    let source = |source: semilla_normal::elements::Source| match source {
        semilla_normal::elements::Source::Declaration(side, at) => semilla_verify::MirSource::Declaration(side, (at.file, at.start, at.end)),
        semilla_normal::elements::Source::Element(side, tokens, call) => semilla_verify::MirSource::Element(side, place(tokens), place(call)),
    };
    let element = |element: semilla_normal::elements::Element| semilla_verify::MirElement { tokens: place(element.tokens), call: place(element.call), targets: element.targets, exit: element.exit, glue: element.glue, jump: element.jump, symbols: element.symbols.into_iter().map(symbol).collect(), sources: element.sources.into_iter().map(|group| semilla_verify::MirGroup { follows: group.follows, sources: group.sources.into_iter().map(source).collect() }).collect() };
    let pairs = diff.pairs.into_iter().map(|(old, new)| ((old.file, old.start, old.end), (new.file, new.start, new.end))).collect();
    semilla_verify::MirDiff { old: diff.old.into_iter().map(element).collect(), new: diff.new.into_iter().map(element).collect(), other: diff.other, pairs, no_symbols: diff.no_symbols, evaluated: diff.evaluated, own: diff.own.into_iter().map(|(side, own)| (side, symbol(own))).collect() }
}

/// The graphs of a body in one configuration, in the form of the verifier (design 60). The
/// sides keep their data also for two graphs with the same shape: the verifier pairs the nested
/// bodies through the tree compare, not by their names (R60.3-9).
fn graph_pair(config: &str, pair: &semilla_normal::graph::GraphPair, consts: Vec<(String, String, String)>, statics: Vec<(String, String, String)>) -> semilla_verify::flow::GraphPair {
    use semilla_verify::flow::{FlowSide, GraphPair};
    let side = |exists: bool, graph: &Option<std::sync::Arc<semilla_normal::graph::RecordGraph>>, key: &Option<(String, String)>| match (exists, graph) {
        (false, _) => FlowSide::NoBody,
        (true, _) if pair.not_compared => FlowSide::NotCompared,
        (true, None) => FlowSide::NoGraph,
        (true, Some(graph)) => FlowSide::Graph(semilla_verify::flow::FlowGraph { key: key.clone(), ..flow_graph(graph) }),
    };
    let same = matches!((&pair.old, &pair.new), (Some(old), Some(new)) if old.same_shape(new));
    GraphPair { config: config.to_string(), same, old: side(pair.old_body, &pair.old, &pair.old_key), new: side(pair.new_body, &pair.new, &pair.new_key), consts, statics }
}

/// The constant items that two records read with a different value (design 64, part D): the
/// path, the old value, and the new value. A path that only one record reads is not in the
/// list (the text of the body then changes).
fn changed_consts(old: Option<&Vec<(String, String)>>, new: Option<&Vec<(String, String)>>) -> Vec<(String, String, String)> {
    let (Some(old), Some(new)) = (old, new) else { return Vec::new() };
    // The values of each path: a constant of a generic item can have one value for each type
    // (`SizedTypeProperties::SIZE`), so the set of the values is the compare.
    let values = |list: &Vec<(String, String)>| {
        let mut found: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for (path, value) in list {
            found.entry(path.clone()).or_default().insert(value.clone());
        }
        found
    };
    let (before, after) = (values(old), values(new));
    let mut found = Vec::new();
    for (path, now) in &after {
        if let Some(was) = before.get(path)
            && was != now
        {
            let text = |set: &std::collections::BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>().join(", ");
            found.push((path.clone(), text(was), text(now)));
        }
    }
    found
}

/// The key of a body: its name and its place (see `compare_with`).
type BodyKey = (String, Option<(String, usize)>);

/// The comparisons of the checked configurations as one list for the verifier: one comparison
/// for each body key, with the status, the graphs, the span, and the elements over the
/// configurations. `prepared` has the state of each configuration and its spans; `many` has
/// the names with two places in one configuration.
fn merge_prepared(prepared: Vec<(SetState, BTreeMap<String, (String, usize)>)>, many: std::collections::BTreeSet<String>) -> Vec<BodyComparison> {
    let mut spans: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut combined: BTreeMap<BodyKey, BodyStatus> = BTreeMap::new();
    // The configurations of each body: those that compile it, and those where it is not equal.
    let mut compiled: BTreeMap<BodyKey, Vec<String>> = BTreeMap::new();
    let mut differs: BTreeMap<BodyKey, Vec<String>> = BTreeMap::new();
    let mut config: BTreeMap<BodyKey, std::collections::BTreeSet<String>> = BTreeMap::new();
    // The MIR elements that differ, over the configurations (design 40.3). `None`: a
    // configuration has no element data for a changed body.
    let mut elements: BTreeMap<BodyKey, Option<semilla_normal::elements::ElementDiff>> = BTreeMap::new();
    // The control flow graphs of each body, for each configuration (design 60).
    let mut graphs: BTreeMap<BodyKey, Vec<semilla_verify::flow::GraphPair>> = BTreeMap::new();
    // The byte range of each body by its name and place (design 40.2): the "after" records win.
    let mut ranges: BTreeMap<(String, (String, usize)), (usize, usize)> = BTreeMap::new();
    for (state, _) in &prepared {
        for levels in [&state.sides.before, &state.sides.after] {
            for record in levels.get("n0").into_iter().flatten() {
                if let Some((span, range)) = record.span.as_ref().and_then(|span| Some((span, span.range?))) {
                    ranges.insert((record.name.clone(), (span.file.clone(), span.line_start)), range);
                }
            }
        }
    }
    // The names with N0 records in two files in one tree of a checked configuration: each
    // file is a source body (R61.14-1). In each configuration, each comparison of such a name
    // has the key of the file of its records, also in a configuration that compiles only one
    // of them (R61.16-1). Before, such a comparison had the name as its key and the span of
    // the name, so the records of one file went to the body of the other file.
    let two_files: std::collections::BTreeSet<String> = prepared.iter().flat_map(|(state, _)| state.sides.two_files.iter().cloned()).collect();
    // The keys of a name of `two_files` with no source file: no span (see `key_of`).
    let mut no_file: std::collections::BTreeSet<BodyKey> = std::collections::BTreeSet::new();
    for (state, own) in prepared {
        // The place of each source body of a name with more than one (R61.14-1): the span
        // of its record in its file; the "after" records win, then the first place.
        let mut source_places: BTreeMap<(&str, &str), (String, usize)> = BTreeMap::new();
        // The one file of the records of each name of `two_files` in this configuration:
        // the "after" records win. `None`: the records of a tree are in two files.
        let mut one_file: BTreeMap<&str, Option<&str>> = BTreeMap::new();
        for levels in [&state.sides.after, &state.sides.before] {
            let records: Vec<&semilla_normal::BodyRecord> = levels.get("n0").into_iter().flatten().collect();
            let mut places: BTreeMap<(&str, &str), (String, usize)> = BTreeMap::new();
            let mut files: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
            for (name, targets) in target_files(levels) {
                if two_files.contains(name) {
                    files.insert(name, targets.into_values().collect());
                }
            }
            for record in &records {
                let Some(span) = &record.span else { continue };
                let place = (span.file.clone(), span.line_start);
                match places.get_mut(&(record.name.as_str(), span.file.as_str())) {
                    Some(known) if place < *known => *known = place,
                    Some(_) => {}
                    None => {
                        places.insert((record.name.as_str(), span.file.as_str()), place);
                    }
                }
            }
            for (key, place) in places {
                source_places.entry(key).or_insert(place);
            }
            for (name, files) in files {
                one_file.entry(name).or_insert_with(|| if files.len() == 1 { files.first().copied() } else { None });
            }
        }
        let key_of = |comparison: &semilla_normal::BodyComparison| -> BodyKey {
            let name = &comparison.name;
            if let Some(file) = &comparison.place {
                return (name.clone(), source_places.get(&(name.as_str(), file.as_str())).cloned().or_else(|| Some((file.clone(), 0))));
            }
            if two_files.contains(name) {
                // A comparison of a name with two source bodies that is not split: its
                // records are in one file in this configuration. Records in two files of
                // one tree with no split (records with no target) give no source body.
                let file = one_file.get(name.as_str()).copied().flatten();
                return (name.clone(), file.and_then(|file| source_places.get(&(name.as_str(), file)).cloned()));
            }
            (name.clone(), if many.contains(name) { None } else { own.get(name).cloned() })
        };
        let name = state.config.name();
        // The N0 record of each name and target of each side (design 64, part D).
        let by_key = |levels: &'_ BTreeMap<String, Vec<semilla_normal::BodyRecord>>| -> std::collections::HashMap<(String, String), usize> {
            levels.get("n0").into_iter().flatten().enumerate().map(|(index, record)| ((record.name.clone(), record.target.clone()), index)).collect()
        };
        let (before_keys, after_keys) = (by_key(&state.sides.before), by_key(&state.sides.after));
        let record_of = |levels: &'_ BTreeMap<String, Vec<semilla_normal::BodyRecord>>, keys: &std::collections::HashMap<(String, String), usize>, key: &Option<(String, String)>| -> Option<Vec<(String, String)>> {
            let index = *keys.get(key.as_ref()?)?;
            // Only the constant values: the rest of a record is large.
            Some(levels.get("n0")?.get(index)?.cv.clone())
        };
        // R73.2-1: the initial values of the statics with interior mutability.
        let statics_of = |levels: &'_ BTreeMap<String, Vec<semilla_normal::BodyRecord>>, keys: &std::collections::HashMap<(String, String), usize>, key: &Option<(String, String)>| -> Option<Vec<(String, String)>> {
            let index = *keys.get(key.as_ref()?)?;
            Some(levels.get("n0")?.get(index)?.sv.clone())
        };
        for comparison in &state.comparisons {
            let key = key_of(comparison);
            if key.1.is_none() && two_files.contains(&key.0) {
                no_file.insert(key.clone());
            }
            if !comparison.graphs.is_empty() {
                graphs.entry(key.clone()).or_default().extend(comparison.graphs.iter().map(|pair| {
                    let consts = changed_consts(record_of(&state.sides.before, &before_keys, &pair.old_key).as_ref(), record_of(&state.sides.after, &after_keys, &pair.new_key).as_ref());
                    let statics = changed_consts(statics_of(&state.sides.before, &before_keys, &pair.old_key).as_ref(), statics_of(&state.sides.after, &after_keys, &pair.new_key).as_ref());
                    graph_pair(&name, pair, consts, statics)
                }));
            }
            if !comparison.config.is_empty() {
                config.entry(key.clone()).or_default().extend(comparison.config.iter().cloned());
            }
            let status = match comparison.status.clone() {
                semilla_normal::BodyStatus::Equal { level, proof } => BodyStatus::Equal { level, proof },
                semilla_normal::BodyStatus::Different { cfg_changed } => BodyStatus::Different { cfg_changed },
                semilla_normal::BodyStatus::BeforeOnly => BodyStatus::BeforeOnly,
                semilla_normal::BodyStatus::AfterOnly => BodyStatus::AfterOnly,
            };
            let found = match &status {
                BodyStatus::Equal { .. } => Some(semilla_normal::elements::ElementDiff::default()),
                BodyStatus::Different { .. } => comparison.elements.clone(),
                _ => None,
            };
            let entry = elements.entry(key.clone()).or_insert_with(|| Some(semilla_normal::elements::ElementDiff::default()));
            match (entry.as_mut(), found) {
                (Some(known), Some(found)) => known.merge(found),
                _ => *entry = None,
            }
            compiled.entry(key.clone()).or_default().push(name.clone());
            if !matches!(status, BodyStatus::Equal { .. }) {
                differs.entry(key.clone()).or_default().push(name.clone());
            }
            let merged = match combined.remove(&key) {
                Some(old) => combine(old, status),
                None => status,
            };
            combined.insert(key, merged);
        }
        spans.extend(own);
    }
    combined
        .into_iter()
        .map(|(key, status)| {
            // The output names the configurations if the body is equal in one or more of
            // them, and not equal in one or more (batch 5u, part A). The combined status is
            // not `Equal`, so the note cannot make a fact neutral.
            let all = compiled.remove(&key).unwrap_or_default();
            let differs_in = differs.remove(&key).filter(|list| list.len() < all.len()).unwrap_or_default();
            let equal_in = if differs_in.is_empty() { Vec::new() } else { all.into_iter().filter(|config| !differs_in.contains(config)).collect() };
            let config = config.remove(&key).into_iter().flatten().collect();
            let found = elements.remove(&key).flatten().filter(|_| matches!(status, BodyStatus::Different { .. })).map(mir_diff);
            let graphs = graphs.remove(&key).unwrap_or_default();
            // A name with two source bodies gets no span by its name only (R61.16-1): the span
            // of the name can be the span of the other body.
            let by_name = !no_file.contains(&key);
            let (name, place) = key;
            let span = place.or_else(|| spans.get(&name).filter(|_| by_name).cloned());
            let range = span.as_ref().and_then(|span| ranges.get(&(name.clone(), span.clone())).copied());
            BodyComparison { graphs, span, status, config, differs_in, equal_in, stopped: Vec::new(), name, range, elements: found }
        })
        .collect()
}

/// A graph of a record in the form of the verifier.
fn flow_graph(graph: &semilla_normal::graph::RecordGraph) -> semilla_verify::flow::FlowGraph {
    let place = |range: &Option<[i64; 3]>| graph.place(range).map(|place| (place.file, place.start, place.end));
    semilla_verify::flow::FlowGraph {
        points: graph.p.iter().map(|point| semilla_verify::flow::FlowPoint { kind: point.k.clone(), text: point.t.clone(), mark: point.m.clone(), tokens: place(&point.r), call: place(&point.c), calls: point.x.clone(), value: point.v.clone() }).collect(),
        arrows: graph.a.iter().filter(|(a, b, _)| (*a as usize) < graph.p.len() && (*b as usize) < graph.p.len()).map(|(a, b, label)| (*a as usize, *b as usize, label.clone())).collect(),
        unwind: graph.n.unwind,
        cleanup: graph.n.cleanup,
        drops: graph.n.drops,
        unreachable: graph.n.unreachable,
        coroutine: graph.co,
        key: None,
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

/// The key of the variant of a query: the configuration and the files.
fn query_key(query: &semilla_verify::equiv::Query, files: &BTreeMap<String, String>) -> (String, BTreeMap<String, String>) {
    (query.config.clone(), files.clone())
}

/// [R66.11-3] The variant builds of a step in the order of the configurations: the variant of
/// each fact in the first configuration (`rank` 0), then each fact in the next one; in one
/// configuration, the order of the queries. So a stop of the budget leaves each fact a result in
/// at least one configuration when it can, as the bodies of the step (R61.14-5). Each item: the
/// first query of the variant, the configuration, and the files.
fn variant_order(queries: &[semilla_verify::equiv::Query], rank: &dyn Fn(&str) -> usize) -> Vec<(usize, String, BTreeMap<String, String>)> {
    let mut keys: Vec<(usize, String, BTreeMap<String, String>)> = Vec::new();
    for (index, query) in queries.iter().enumerate() {
        if let Some(files) = &query.variant
            && !keys.iter().any(|(_, config, known)| *config == query.config && known == files)
        {
            keys.push((index, query.config.clone(), files.clone()));
        }
    }
    keys.sort_by_key(|(index, config, _)| (rank(config), *index));
    keys
}

/// [R77.2-1] The safety time limit of one solver query of the equivalence check (design 61, C8):
/// the result of a query comes from its limit in solver steps.
pub(crate) const EQUIV_TIMEOUT: std::time::Duration = semilla_smt::budget::SAFETY_QUERY;
/// The budget of the equivalence check of one step: the most solver queries, and the most solver
/// steps (R77.2-2).
const EQUIV_QUERIES: usize = 400;

/// The query budget of a step: `EQUIV_QUERIES`, or `SEMILLA_EQUIV_QUERIES=N` (for a test of the
/// budget).
fn equiv_queries() -> usize {
    std::env::var("SEMILLA_EQUIV_QUERIES").ok().and_then(|text| text.trim().parse::<usize>().ok()).unwrap_or(EQUIV_QUERIES)
}
/// [R77.2-2] The budget of the equivalence check of a step in solver steps (the body checks of the
/// step and of the variants, in their order), or `SEMILLA_EQUIV_BUDGET=STEPS` (for a test of the
/// budget).
pub(crate) const EQUIV_STEPS: u64 = 200_000_000;
/// [R77.2-2] The budget of one body check in solver steps (R66.9-2 in solver steps).
pub(crate) const BODY_STEPS: u64 = 60_000_000;
/// [R77.2-2] The most builds of a variant (a tree with one edit of a fact) for each step, in the
/// order of R66.11-3. The case set uses at most 8 in a step.
pub(crate) const VARIANT_BUILDS: usize = 8;

/// The count of variant builds of a step: `VARIANT_BUILDS`, or `SEMILLA_VARIANT_BUILDS=N` (for a
/// test of the count).
fn variant_builds() -> usize {
    std::env::var("SEMILLA_VARIANT_BUILDS").ok().and_then(|text| text.trim().parse::<usize>().ok()).unwrap_or(VARIANT_BUILDS)
}
/// [R77.2-3] The safety time limit of the equivalence check of one step (the body checks, the
/// walk, and the builds of the variants). It never decides a result on a usual machine.
pub(crate) const SAFETY_STEP: std::time::Duration = std::time::Duration::from_secs(1200);

/// The budget of the equivalence check of a step in solver steps.
pub(crate) fn equiv_steps() -> u64 {
    std::env::var("SEMILLA_EQUIV_BUDGET").ok().and_then(|text| text.trim().parse::<u64>().ok()).unwrap_or(EQUIV_STEPS)
}

/// [R77.2-2] The budget of one body check: [`BODY_STEPS`], never more than the budget of the step,
/// with the safety time limit of the step.
pub(crate) fn body_budget(step: u64, safety: std::time::Instant) -> semilla_smt::budget::Budget {
    semilla_smt::budget::Budget::with_safety(BODY_STEPS.min(step), Some(safety))
}



/// The N0 records of one side of a configuration, for the equivalence check: by name.
pub(crate) struct EquivSide<'a> {
    pub(crate) records: BTreeMap<&'a str, Vec<&'a semilla_normal::BodyRecord>>,
}

impl<'a> EquivSide<'a> {
    pub(crate) fn new(records: &'a [semilla_normal::BodyRecord]) -> Self {
        let mut map: BTreeMap<&str, Vec<&semilla_normal::BodyRecord>> = BTreeMap::new();
        for record in records {
            map.entry(record.name.as_str()).or_default().push(record);
        }
        EquivSide { records: map }
    }

    /// The record of a name for a target (R61.16-2): the first record of the target; else the
    /// one target with the name of the target without the hash (a target with a new hash); else
    /// the one library target (each other target of a package links to it); else a record only
    /// if each record of the name has one file, one text, and one form. In the second and the
    /// third rule, a target whose unit has the build settings `unit` of the record that asks
    /// wins (cargo can build a library two times: with `panic="unwind"` and `panic="abort"`). Two packages can each
    /// have a target with one kind and one name, and then one name is two source bodies
    /// (R61.14-1): no record of the other one. The result does not depend on the order of the
    /// records.
    pub(crate) fn find(&self, name: &str, target: &str, unit: &str) -> Option<&'a semilla_normal::BodyRecord> {
        let list = self.records.get(name)?;
        if let Some(found) = list.iter().find(|record| record.target == target) {
            return Some(found);
        }
        let one_target = |keep: &dyn Fn(&str) -> bool| -> Option<&'a semilla_normal::BodyRecord> {
            let targets: std::collections::BTreeSet<&str> = list.iter().map(|record| record.target.as_str()).filter(|found| keep(found)).collect();
            let [only] = targets.into_iter().collect::<Vec<_>>()[..] else { return None };
            list.iter().find(|record| record.target == only).copied()
        };
        let stem = semilla_normal::target_stem(target);
        let units: BTreeMap<&str, &str> = list.iter().map(|record| (record.target.as_str(), record.unit.as_str())).collect();
        let same_unit = |found: &str| !unit.is_empty() && units.get(found) == Some(&unit);
        let rules: [&dyn Fn(&str) -> bool; 4] = [
            &|found| semilla_normal::target_stem(found) == stem && same_unit(found),
            &|found| semilla_normal::target_stem(found) == stem,
            &|found| semilla_normal::target_stem(found).ends_with(".lib") && same_unit(found),
            &|found| semilla_normal::target_stem(found).ends_with(".lib"),
        ];
        for rule in rules {
            if let Some(found) = one_target(rule) {
                return Some(found);
            }
        }
        let first = list.first()?;
        let file = |record: &semilla_normal::BodyRecord| record.span.as_ref().map(|span| span.file.clone());
        list.iter().all(|record| file(record) == file(first) && record.text == first.text && record.q == first.q).then_some(*first)
    }

    /// The records of a name, one for each source body and text (R61.14-1): a lib and its test
    /// build with one text give one record.
    pub(crate) fn bodies(&self, name: &str) -> Vec<&'a semilla_normal::BodyRecord> {
        let mut out: Vec<&semilla_normal::BodyRecord> = Vec::new();
        for record in self.records.get(name).into_iter().flatten() {
            let file = record.span.as_ref().map(|span| &span.file);
            if !out.iter().any(|known| known.span.as_ref().map(|span| &span.file) == file && known.text == record.text && known.q == record.q) {
                out.push(record);
            }
        }
        out
    }
}

/// The forms of the callees of a record (`qx`), each one read one time.
type StdForms = std::sync::Arc<Vec<(String, std::sync::Arc<semilla_smt::mir::Ir>)>>;

/// The forms that the process read, by the key of their text (design 66, part D). The body
/// checks of a step read the same texts again and again (each callee record of each check), and
/// a read of a large text costs more than its key. The key is the length and two hashes of the
/// text (128 bits): two texts with one key are equal texts.
static FORMS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<(usize, u64, u64), std::sync::Arc<semilla_smt::mir::Ir>>>> = std::sync::LazyLock::new(Default::default);
static STD_FORMS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<(usize, u64, u64), StdForms>>> = std::sync::LazyLock::new(Default::default);

/// The key of a text for the caches of the forms.
fn text_key(text: &str) -> (usize, u64, u64) {
    use std::hash::{Hash, Hasher};
    let mut first = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut first);
    let mut second = std::collections::hash_map::DefaultHasher::new();
    0x9e37_79b9_7f4a_7c15u64.hash(&mut second);
    text.hash(&mut second);
    (text.len(), first.finish(), second.finish())
}

/// The form of a record (`q`), if the record has one.
pub(crate) fn record_form(record: &semilla_normal::BodyRecord) -> Option<std::sync::Arc<semilla_smt::mir::Ir>> {
    let text = record.q.as_ref()?;
    let key = text_key(text);
    if let Some(found) = FORMS.lock().unwrap_or_else(|error| error.into_inner()).get(&key) {
        return Some(found.clone());
    }
    let form = std::sync::Arc::new(serde_json::from_str(text).unwrap_or_else(|error| semilla_smt::mir::Ir { u: Some(format!("the form cannot be read: {error}")), ..Default::default() }));
    FORMS.lock().unwrap_or_else(|error| error.into_inner()).insert(key, form.clone());
    Some(form)
}

/// The forms of the callees of one record (`qx`), read one time for each text.
fn record_std_forms(text: &str) -> StdForms {
    let key = text_key(text);
    if let Some(found) = STD_FORMS.lock().unwrap_or_else(|error| error.into_inner()).get(&key) {
        return found.clone();
    }
    let list: Vec<(String, std::sync::Arc<semilla_smt::mir::Ir>)> = match serde_json::from_str::<BTreeMap<String, semilla_smt::mir::Ir>>(text) {
        Ok(map) => map.into_iter().map(|(text, form)| (text, std::sync::Arc::new(form))).collect(),
        Err(_) => Vec::new(),
    };
    let found = std::sync::Arc::new(list);
    STD_FORMS.lock().unwrap_or_else(|error| error.into_inner()).insert(key, found.clone());
    found
}

/// The forms of the small callees of the standard library of a record (`qx`).
pub(crate) fn std_forms(records: &[&semilla_normal::BodyRecord]) -> BTreeMap<String, std::sync::Arc<semilla_smt::mir::Ir>> {
    let mut found = BTreeMap::new();
    for record in records {
        let Some(text) = record.qx.as_deref() else { continue };
        for (text, form) in record_std_forms(text).iter() {
            found.entry(text.clone()).or_insert_with(|| form.clone());
        }
    }
    found
}

/// The changed bodies of a pair of record sets: a different text, or a body of one side only
/// (the old name and the new name of each).
pub(crate) fn changed_names(old: &EquivSide, new: &EquivSide, renames: &[(String, String)]) -> std::collections::BTreeSet<String> {
    let rename_text = |text: &str| if renames.is_empty() { text.to_string() } else { semilla_normal::paths::rename(text, renames) };
    let mut changed = std::collections::BTreeSet::new();
    let mut seen = std::collections::BTreeSet::new();
    for (name, list) in &old.records {
        let renamed_name = rename_text(name);
        seen.insert(renamed_name.clone());
        let texts: std::collections::BTreeSet<String> = list.iter().map(|record| rename_text(&record.text)).collect();
        let other = new.records.get(renamed_name.as_str());
        if other.is_none_or(|other| other.iter().map(|record| record.text.clone()).collect::<std::collections::BTreeSet<_>>() != texts) {
            changed.insert(name.to_string());
            changed.insert(renamed_name);
        }
    }
    for name in new.records.keys() {
        if !seen.contains(*name) {
            changed.insert(name.to_string());
        }
    }
    changed
}

/// The changed callees of a pair of records (design 61.10, defect 1): each callee of the two
/// bodies that is in the set to inline, also through an instance and through the trait method
/// of a changed impl method.
fn changed_callees(a: &semilla_normal::BodyRecord, b: &semilla_normal::BodyRecord, changed: &std::collections::BTreeSet<String>, trait_methods: &BTreeMap<String, String>, rename: &dyn Fn(&str) -> String) -> Vec<String> {
    let mut found: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut note = |callee: String| {
        if changed.contains(&callee) {
            found.insert(callee.clone());
        }
        for (method, of) in trait_methods {
            if *of == callee && changed.contains(method) {
                found.insert(method.clone());
            }
        }
    };
    for (record, old) in [(a, true), (b, false)] {
        let map = |text: &str| if old { rename(text) } else { text.to_string() };
        for callee in &record.callees {
            note(map(callee));
        }
        for instance in &record.inst {
            note(map(&instance.g));
            for (_, resolved) in &instance.c {
                note(map(resolved));
            }
        }
    }
    found.into_iter().collect()
}

impl NormalOracle {
    /// Checks one query of the equivalence check (design 61). `old` and `new` are the records of
    /// the two sides of the configuration. `reach` is the walk of the configuration: the set of
    /// the bodies to inline and the trait methods of the impl methods.
    #[allow(clippy::too_many_arguments)]
    fn equivalence_one(&self, solver: &semilla_smt::Solver, query: &semilla_verify::equiv::Query, old: &EquivSide, new: &EquivSide, renames: &[(String, String)], renamed: bool, reach: &reach::ConfigReach, budget: Option<semilla_smt::budget::Budget>) -> Option<semilla_verify::equiv::Answer> {
        let a = old.find(&query.old.0, &query.old.1, "")?;
        let b = new.find(&query.new.0, &query.new.1, &a.unit)?;
        let rename_text = |text: &str| if renamed { semilla_normal::paths::rename(text, renames) } else { text.to_string() };
        let callees = changed_callees(a, b, &reach.must_inline, &reach.trait_methods, &rename_text);
        // An equal MIR text at N0 is a proof with no solver only if no callee changes: else the
        // body-local result has its own name, and the check inlines each changed callee.
        let mut answer = self.check_pair(solver, a, b, old, new, renames, renamed, &reach.must_inline, callees.is_empty(), budget)?;
        if !callees.is_empty() && rename_text(&a.text) == b.text {
            answer.local = Some(format!("same body code; changed callee: {}", callees.join(", ")));
        }
        Some(answer)
    }

    /// Checks two records with each body of `changed` inlined. `equal_proof`: an equal MIR text
    /// at N0 is a proof (only for a body with no changed callee).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn check_pair(&self, solver: &semilla_smt::Solver, a: &semilla_normal::BodyRecord, b: &semilla_normal::BodyRecord, old: &EquivSide, new: &EquivSide, renames: &[(String, String)], renamed: bool, changed: &std::sync::Arc<std::collections::BTreeSet<String>>, equal_proof: bool, budget: Option<semilla_smt::budget::Budget>) -> Option<semilla_verify::equiv::Answer> {
        use semilla_verify::equiv::{Answer, Verdict};
        let rename_text = |text: &str| if renamed { semilla_normal::paths::rename(text, renames) } else { text.to_string() };
        // An equal text is a proof only if each constant item that the two texts name by its
        // path has the same values (design 64, part D, R64.5-2).
        if equal_proof && rename_text(&a.text) == b.text && changed_consts(Some(&a.cv), Some(&b.cv)).is_empty() {
            return Some(Answer { verdict: Verdict::Equivalent("equal MIR".into()), inlined: Vec::new(), queries: 0, millis: 0, local: None });
        }
        // [R66.10-5] The budget stops only the costly part: the compare of two equal MIR texts
        // above needs no solver, so it gives its proof also past the budget (R77.2-2: a budget of
        // 0 solver steps).
        if let Some(budget) = budget {
            let cause = if budget.safety.is_some_and(|safety| std::time::Instant::now() >= safety) {
                Some(semilla_smt::budget::SAFETY_CAUSE)
            } else {
                (budget.steps == 0).then_some(semilla_verify::equiv::BUDGET_CAUSE)
            };
            if let Some(cause) = cause {
                return Some(Answer { verdict: Verdict::NotChecked(cause.into()), inlined: Vec::new(), queries: 0, millis: 0, local: None });
            }
        }
        let form_started = std::time::Instant::now();
        let forms = (record_form(a), record_form(b));
        FORM_TIME.add(form_started);
        let (Some(form_a), Some(form_b)) = forms else {
            return Some(Answer { verdict: Verdict::NotDecided("a record has no form (an old record)".into()), inlined: Vec::new(), queries: 0, millis: 0, local: None });
        };
        let changed_names: Vec<String> = changed.iter().cloned().collect();
        // The forms of the standard library and of the instances: of the body and of each
        // changed body that the check can inline (each changed callee, to the inline depth).
        let with_changed = |side: &EquivSide<'_>, record: &semilla_normal::BodyRecord| -> BTreeMap<String, std::sync::Arc<semilla_smt::mir::Ir>> {
            let mut list = vec![record];
            let mut at = 0;
            while at < list.len() && list.len() < 32 {
                let current = list[at];
                at += 1;
                // The callees of the body, changed or not: the second try of the check inlines
                // each callee with a form, and the callees of the standard library of an
                // inlined body need their forms too (design 64).
                for callee in &current.callees {
                    if let Some(found) = side.find(callee, &record.target, &record.unit)
                        && !list.iter().any(|known| std::ptr::eq(*known, found))
                    {
                        list.push(found);
                    }
                }
            }
            std_forms(&list)
        };
        let std_started = std::time::Instant::now();
        let std_a = with_changed(old, a);
        let std_b = with_changed(new, b);
        STD_FORM_TIME.add(std_started);
        let target_a = a.target.clone();
        let target_b = b.target.clone();
        let workspace_a = |name: &str| old.find(name, &target_a, &a.unit).and_then(record_form);
        let workspace_b = |name: &str| new.find(name, &target_b, &b.unit).and_then(record_form);
        let rename_a = |text: &str| rename_text(text);
        let same = |text: &str| text.to_string();
        let is_changed = |name: &str| changed.contains(name);
        let step = semilla_smt::mir::Step { changed: &is_changed, changed_names: &changed_names };
        let side_a = semilla_smt::mir::Side { ir: form_a, workspace: &workspace_a, std: &std_a, rename: &rename_a };
        let side_b = semilla_smt::mir::Side { ir: form_b, workspace: &workspace_b, std: &std_b, rename: &same };
        let solve_started = std::time::Instant::now();
        let checked = semilla_smt::mir::check_bodies_until(solver, &side_a, &side_b, &step, budget);
        SOLVE_TIME.add(solve_started);
        // A body check over 1 s (design 66, part D): one line with `SEMILLA_TIMING`.
        if std::env::var_os("SEMILLA_TIMING").is_some() && (solve_started.elapsed() > std::time::Duration::from_secs(1) || checked.steps > 10_000_000) {
            eprintln!("[timing] the body check of {}: {:.2} s ({} solver queries, {} solver steps)", b.name, solve_started.elapsed().as_secs_f64(), checked.queries, checked.steps);
        }
        let verdict = match checked.verdict {
            semilla_smt::mir::Verdict::Equivalent { method } => Verdict::Equivalent(method),
            // [R75.1-1] The calls of the suggested test get the record names, the targets, and
            // the places of the two bodies (design 75).
            semilla_smt::mir::Verdict::Different { input, before, after, run } => {
                let test = match run {
                    Some(mut case) => {
                        for call in &mut case.calls {
                            let record = if call.side == semilla_core::run::RunSide::Before { a } else { b };
                            call.name = record.name.clone();
                            call.target = record.target.clone();
                            call.value = record.value;
                            if let Some(span) = &record.span {
                                call.file = span.file.clone();
                                call.line = span.line_start;
                            }
                        }
                        semilla_verify::equiv::TestNote::Open(case)
                    }
                    None => semilla_verify::equiv::TestNote::None,
                };
                Verdict::Different { input, before, after, test }
            }
            // A check that its budget stops (R66.9-2, R77.2-2): not checked, as a check that the
            // budget of the step does not start. The safety time limit has its own cause.
            semilla_smt::mir::Verdict::NotDecided { cause } if semilla_smt::budget::is_stop(&cause) => Verdict::NotChecked(cause),
            semilla_smt::mir::Verdict::NotDecided { cause } => Verdict::NotDecided(cause),
        };
        Some(Answer { verdict, inlined: checked.inlined, queries: checked.queries, millis: checked.millis, local: None })
    }

    /// The walk of each checked configuration of a step, one time for the step (design 61.10,
    /// part A), in the time `limit`.
    fn reach_all(&self, step: &Step, renames: &BTreeMap<String, String>, limit: u64) -> Option<std::sync::Arc<Vec<(String, reach::ConfigReach)>>> {
        let key = (step.tree_before.clone(), step.tree_after.clone());
        if let Some(found) = self.added.lock().unwrap_or_else(|error| error.into_inner()).reached.get(&key).cloned() {
            return Some(found);
        }
        let solver = semilla_smt::Solver::find_for(&crate::store::tool_dirs(&self.repo.root), Some(&self.repo.root)).ok()?.with_timeout(EQUIV_TIMEOUT);
        let checked = self.checked(step).ok()?;
        let renames: Vec<(String, String)> = renames.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
        let safety = std::time::Instant::now() + SAFETY_STEP;
        let mut spent = 0u64;
        let mut out = Vec::new();
        for config in &checked {
            let Ok(sides) = self.sides(step, config) else { continue };
            let empty = Vec::new();
            let old = EquivSide::new(sides.before.get("n0").unwrap_or(&empty));
            let new = EquivSide::new(sides.after.get("n0").unwrap_or(&empty));
            out.push((config.name(), self.reach_config(&solver, &old, &new, &renames, &mut spent, limit, safety)));
        }
        let found = std::sync::Arc::new(out);
        self.added.lock().unwrap_or_else(|error| error.into_inner()).reached.insert(key, found.clone());
        Some(found)
    }
}

impl BodyOracle for NormalOracle {
    fn intent(&self, step: &Step, renames: &BTreeMap<String, String>, queries: &[semilla_verify::intent::Query]) -> Option<Vec<semilla_verify::intent::Answer>> {
        self.intent_claims(step, renames, queries)
    }

    fn intent_anchors(&self, step: &Step) -> Option<std::result::Result<semilla_verify::astmap::CompiledClaims, String>> {
        if step.agent.intent.is_empty() {
            return None;
        }
        let sources: Vec<semilla_normal::intent::Source> = step.agent.intent.iter().map(|file| semilla_normal::intent::Source { path: file.path.clone(), text: file.text.clone() }).collect();
        let check = self.intent_compile(step, &sources);
        Some(match check.as_ref() {
            Ok(check) => intent::compiled_claims(check, &sources),
            Err(cause) => Err(cause.clone()),
        })
    }

    fn compare(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String]) -> Result<Vec<BodyComparison>> {
        self.compare_with(step, renames, added, &AllOpaque)
    }

    fn instances(&self, step: &Step, renames: &BTreeMap<String, String>) -> Vec<semilla_verify::equiv::InstanceLine> {
        use semilla_verify::equiv::{InstanceLine, type_names};
        let Ok(checked) = self.checked(step) else { return Vec::new() };
        let renames: Vec<(String, String)> = renames.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
        let rename = |text: &str| semilla_normal::paths::rename(text, &renames);
        let mut out: Vec<InstanceLine> = Vec::new();
        let mut push = |line: InstanceLine| {
            if !out.iter().any(|known| known.text == line.text) {
                out.push(line);
            }
        };
        let short = |text: &str| text.rsplit_once("::<").map_or(text.to_string(), |(path, args)| format!("{}::<{args}", path.rsplit("::").next().unwrap_or(path)));
        for config in &checked {
            let Ok(sides) = self.sides(step, config) else { continue };
            let empty = Vec::new();
            let (old, new) = (sides.before.get("n0").unwrap_or(&empty), sides.after.get("n0").unwrap_or(&empty));
            // The instances by the root, the generic body (the names of the new tree), and the
            // source body of the root: its file, if the root name has more than one source
            // body (two packages can each have a target with one kind and one name, R61.14-1).
            type Instances = BTreeMap<(String, String, String), BTreeMap<String, Vec<(String, String)>>>;
            let collect = |records: &[semilla_normal::BodyRecord], map: &dyn Fn(&str) -> String| -> Instances {
                let file_of = |record: &semilla_normal::BodyRecord| record.span.as_ref().map(|span| span.file.clone()).unwrap_or_default();
                let mut files: BTreeMap<&str, std::collections::BTreeSet<String>> = BTreeMap::new();
                for record in records.iter().filter(|record| !record.inst.is_empty()) {
                    files.entry(record.name.as_str()).or_default().insert(file_of(record));
                }
                let mut found: Instances = BTreeMap::new();
                for record in records {
                    let file = if files.get(record.name.as_str()).is_some_and(|set| set.len() > 1) { file_of(record) } else { String::new() };
                    for instance in &record.inst {
                        found.entry((map(&record.name), file.clone(), map(&instance.g))).or_default().insert(map(&instance.t), instance.c.iter().map(|(a, b)| (map(a), map(b))).collect());
                    }
                }
                found
            };
            let (a, b) = (collect(old, &rename), collect(new, &|text: &str| text.to_string()));
            let keys: std::collections::BTreeSet<&(String, String, String)> = a.keys().chain(b.keys()).collect();
            let empty_map = BTreeMap::new();
            for key in keys {
                let (x, y) = (a.get(key).unwrap_or(&empty_map), b.get(key).unwrap_or(&empty_map));
                let generic = &key.2;
                // An instance on the two sides with a changed resolved callee.
                for (text, calls) in y {
                    if let Some(before) = x.get(text)
                        && before != calls
                    {
                        for (call, target) in calls.iter().filter(|call| !before.contains(call)) {
                            let was = before.iter().find(|(other, _)| other == call).map(|(_, target)| format!(" (before: `{target}`)")).unwrap_or_default();
                            push(InstanceLine { body: generic.clone(), text: format!("instance {}: call `{call}` resolves to `{target}`{was}", short(text)), names: [type_names(&before.iter().map(|(_, b)| b.as_str()).collect::<Vec<_>>().join(" ")), type_names(target)] });
                        }
                    }
                }
                let removed: Vec<&String> = x.keys().filter(|text| !y.contains_key(*text)).collect();
                let added: Vec<&String> = y.keys().filter(|text| !x.contains_key(*text)).collect();
                if removed.len() == 1 && added.len() == 1 {
                    // The instance of one root changes: name the old instance.
                    let (was, now) = (removed[0], added[0]);
                    let names = [type_names(was).into_iter().filter(|name| !type_names(now).contains(name)).collect(), type_names(now).into_iter().filter(|name| !type_names(was).contains(name)).collect()];
                    let calls = &y[now];
                    if calls.is_empty() {
                        push(InstanceLine { body: generic.clone(), text: format!("instance {} (before: the instance {})", short(now), short(was)), names: names.clone() });
                    }
                    for (call, target) in calls {
                        push(InstanceLine { body: generic.clone(), text: format!("instance {}: call `{call}` resolves to `{target}` (before: the instance {})", short(now), short(was)), names: names.clone() });
                    }
                } else {
                    for text in removed {
                        push(InstanceLine { body: generic.clone(), text: format!("− instance {}", short(text)), names: [type_names(text), Vec::new()] });
                    }
                    for text in added {
                        push(InstanceLine { body: generic.clone(), text: format!("+ instance {}", short(text)), names: [Vec::new(), type_names(text)] });
                    }
                }
            }
            // The candidates of the `dyn` calls (B3): the types that the workspace converts to
            // `dyn Trait`.
            let casts = |records: &[semilla_normal::BodyRecord], map: &dyn Fn(&str) -> String| -> BTreeMap<String, std::collections::BTreeSet<String>> {
                let mut found: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
                for record in records {
                    for (name, ty) in &record.dyn_casts {
                        found.entry(map(name)).or_default().insert(map(ty));
                    }
                }
                found
            };
            let (a, b) = (casts(old, &rename), casts(new, &|text: &str| text.to_string()));
            let traits: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            let none = std::collections::BTreeSet::new();
            for name in traits {
                let (x, y) = (a.get(name).unwrap_or(&none), b.get(name).unwrap_or(&none));
                if x == y {
                    continue;
                }
                let mut parts: Vec<String> = y.difference(x).map(|ty| format!("+`{ty}`")).collect();
                parts.extend(x.difference(y).map(|ty| format!("−`{ty}`")));
                let names = [x.difference(y).flat_map(|ty| type_names(ty)).collect(), y.difference(x).flat_map(|ty| type_names(ty)).collect()];
                push(InstanceLine { body: format!("dyn {name}"), text: format!("the candidates of a call through `dyn {name}`: {}", parts.join(", ")), names });
            }
        }
        out
    }

    fn equivalence(&self, step: &Step, renames: &BTreeMap<String, String>, queries: &[semilla_verify::equiv::Query]) -> Vec<Option<semilla_verify::equiv::Answer>> {
        use semilla_verify::equiv::{Answer, Verdict};
        let Ok(solver) = semilla_smt::Solver::find_for(&crate::store::tool_dirs(&self.repo.root), Some(&self.repo.root)) else {
            return queries.iter().map(|_| Some(Answer { verdict: Verdict::NotDecided("no SMT solver".into()), inlined: Vec::new(), queries: 0, millis: 0, local: None })).collect();
        };
        let solver = solver.with_timeout(EQUIV_TIMEOUT);
        let Ok(checked) = self.checked(step) else { return vec![None; queries.len()] };
        let renames: Vec<(String, String)> = renames.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
        // [R77.2-2] One budget in solver steps for the body checks of the step and of the
        // variants, and the query budget. A job is the check of one query; its result and its
        // cost (solver steps and queries) depend only on the trees, never on the order of the
        // threads. The jobs have one order (the bodies of the step, then the variants): a job gets
        // its own result only if the jobs before it in the order used less than each budget; else
        // it gets the result past the budget (R66.10-5: a proof by two equal MIR texts, else "not
        // checked (the budget of the step)"). So the same trees give the same results on each
        // machine and at each load. [R77.2-3] The safety time limit stops the rest of the work
        // with its own cause.
        let step_steps = equiv_steps();
        let queries_limit = equiv_queries();
        let started = std::time::Instant::now();
        let safety = started + SAFETY_STEP;
        let threads = std::thread::available_parallelism().map(|count| count.get()).unwrap_or(4).clamp(1, 8);
        type Walks = Option<std::sync::Arc<Vec<(String, reach::ConfigReach)>>>;
        // The cost of each job: the solver steps and the queries (`None` until it ends).
        type Cost = (u64, usize);
        // The prefix rule: the sum of the known costs of the jobs before a position.
        let over = |costs: &[Option<Cost>], at: usize| -> bool {
            let (steps, count) = costs[..at].iter().flatten().fold((0u64, 0usize), |(a, b), (x, y)| (a.saturating_add(*x), b + y));
            steps >= step_steps || count > queries_limit
        };
        // Runs the checks of some queries in up to 8 threads. `variant`: the records of the
        // variant tree of the queries. `walks`: the walk of each configuration (design 61.10),
        // for the queries of the step. `order`: the position of each job in the order of the
        // step; `costs`: the cost of each position. Each job gives its answer, its cost, and true
        // if it ran past the budget.
        let run = |jobs: &[(usize, usize)], variant: Option<&[semilla_normal::BodyRecord]>, walks: &Walks, costs: &std::sync::Mutex<Vec<Option<Cost>>>| -> Vec<(usize, Option<Answer>, bool)> {
            let walk_of = |config: &str| walks.as_ref().and_then(|walks| walks.iter().find(|(name, _)| name == config).map(|(_, reach)| reach.clone()));
            let next = std::sync::atomic::AtomicUsize::new(0);
            let results: std::sync::Mutex<Vec<(usize, Option<Answer>, bool)>> = std::sync::Mutex::new(Vec::new());
            // The changed bodies of the variant (one variant tree for each call).
            let changed: std::sync::OnceLock<reach::ConfigReach> = std::sync::OnceLock::new();
            std::thread::scope(|scope| {
                for _ in 0..threads.min(jobs.len().max(1)) {
                    scope.spawn(|| loop {
                        let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((index, position)) = jobs.get(at).copied() else { break };
                        let query = &queries[index];
                        // R66.10-5: past the budget (the known costs before this position use
                        // it), a check still gives the proof of two equal MIR texts (no solver).
                        let late = over(&costs.lock().unwrap_or_else(|error| error.into_inner()), position);
                        let budget = if late { semilla_smt::budget::Budget::with_safety(0, Some(safety)) } else { body_budget(step_steps, safety) };
                        let before = semilla_smt::budget::used();
                        let answer = {
                            let config = checked.iter().find(|config| config.name() == query.config);
                            let sides = config.and_then(|config| self.sides(step, config).ok());
                            sides.and_then(|sides| {
                                let empty = Vec::new();
                                let old = EquivSide::new(sides.before.get("n0").unwrap_or(&empty));
                                match variant {
                                    None => {
                                        let new = EquivSide::new(sides.after.get("n0").unwrap_or(&empty));
                                        let walk = walk_of(&query.config).unwrap_or_else(|| reach::ConfigReach { must_inline: std::sync::Arc::new(changed_names(&old, &new, &renames)), ..Default::default() });
                                        self.equivalence_one(&solver, query, &old, &new, &renames, true, &walk, Some(budget))
                                    }
                                    Some(records) => {
                                        let new = EquivSide::new(records);
                                        let walk = changed.get_or_init(|| reach::ConfigReach { must_inline: std::sync::Arc::new(changed_names(&old, &new, &[])), ..Default::default() });
                                        self.equivalence_one(&solver, query, &old, &new, &[], false, walk, Some(budget))
                                    }
                                }
                            })
                        };
                        let cost = (semilla_smt::budget::used() - before, answer.as_ref().map(|answer| answer.queries).unwrap_or(0));
                        costs.lock().unwrap_or_else(|error| error.into_inner())[position] = Some(cost);
                        results.lock().unwrap_or_else(|error| error.into_inner()).push((index, answer, late));
                    });
                }
            });
            results.into_inner().unwrap_or_else(|error| error.into_inner())
        };
        // The order of the value (R61.14-5): first each check that needs no build (each body of
        // the step), then the variants of the facts.
        // The bodies in the first configuration come first: a stop of the budget leaves a result
        // of each body in at least one configuration when it can.
        let rank = |config: &str| checked.iter().position(|known| known.name() == config).unwrap_or(usize::MAX);
        let mut plain: Vec<usize> = (0..queries.len()).filter(|index| queries[*index].variant.is_none()).collect();
        plain.sort_by_key(|index| (rank(&queries[*index].config), *index));
        let keys = variant_order(queries, &rank);
        // The position of each job in the one order: the plain jobs, then the jobs of each variant.
        let mut order: Vec<usize> = plain.clone();
        for (_, config_name, files) in &keys {
            let key = (config_name.clone(), files.clone());
            order.extend((0..queries.len()).filter(|index| queries[*index].variant.as_ref().is_some_and(|files| query_key(&queries[*index], files) == key)));
        }
        let position_of: BTreeMap<usize, usize> = order.iter().enumerate().map(|(position, index)| (*index, position)).collect();
        let costs: std::sync::Mutex<Vec<Option<Cost>>> = std::sync::Mutex::new(vec![None; order.len()]);
        let with_positions = |list: Vec<usize>| -> Vec<(usize, usize)> { list.into_iter().map(|index| (index, position_of[&index])).collect() };
        // A variant changes only files of the step. With the build code of the base in the two
        // trees of the step, the variant has it too, and it builds in the delta build folder of
        // the configuration: only the changed crates compile again (R61.14-5).
        let clean = self.code.clean.contains(&step.tree_before) && self.code.clean.contains(&step.tree_after);
        let safety_answer = || Some(Answer { verdict: Verdict::NotChecked(semilla_smt::budget::SAFETY_CAUSE.into()), inlined: Vec::new(), queries: 0, millis: 0, local: None });
        // [R77.2-2] The builds of the variants and their checks, one build at a time. A build is
        // not a solver step: a count of builds bounds them (`VARIANT_BUILDS`), and the safety time
        // limit stops them.
        let variants = || -> Vec<(usize, Option<Answer>, bool)> {
            let mut out = Vec::new();
            for (at, (_, config_name, files)) in keys.iter().enumerate() {
                let key = (config_name.clone(), files.clone());
                let jobs: Vec<usize> = (0..queries.len()).filter(|index| queries[*index].variant.as_ref().is_some_and(|files| query_key(&queries[*index], files) == key)).collect();
                let config = checked.iter().find(|config| config.name() == key.0);
                // [R77.2-2] At most `VARIANT_BUILDS` builds of a variant for each step, in the
                // order of R66.11-3 (each fact in the first configuration first): a count, so the
                // same step gets the same results on each machine.
                if at >= variant_builds() {
                    out.extend(jobs.into_iter().map(|index| (index, Some(Answer { verdict: Verdict::NotChecked(semilla_verify::equiv::BUDGET_CAUSE.into()), inlined: Vec::new(), queries: 0, millis: 0, local: None }), false)));
                    continue;
                }
                if std::time::Instant::now() >= safety {
                    out.extend(jobs.into_iter().map(|index| (index, safety_answer(), false)));
                    continue;
                }
                let found = config.filter(|_| clean).and_then(|config| {
                    let files: BTreeMap<String, Option<String>> = key.1.iter().map(|(path, text)| (path.clone(), Some(text.clone()))).collect();
                    let tree = self.repo.tree_with(&step.tree_before, &files).ok()?;
                    self.added.lock().unwrap_or_else(|error| error.into_inner()).variants.insert(tree.clone());
                    self.records_with(&tree, "n0", config).ok()
                });
                match found {
                    Some(records) => out.extend(run(&with_positions(jobs), Some(&records), &None, &costs).into_iter().map(|(index, answer, late)| (index, answer, late))),
                    None => out.extend(jobs.into_iter().map(|index| (index, Some(Answer { verdict: Verdict::NotDecided("the variant with only this edit does not compile".into()), inlined: Vec::new(), queries: 0, millis: 0, local: None }), false))),
                }
            }
            crate::timing(&format!("the builds and the checks of {} variants of the equivalence check", keys.len()), started);
            out
        };
        // [R66.11-3] The builds of the variants do not wait for the solver: they run in a thread
        // next to the walk and the body checks of the step.
        let (plain_answers, variant_answers, walks) = std::thread::scope(|scope| {
            let builds = scope.spawn(variants);
            // The walk of each configuration (design 61.10): the bodies that each check must
            // inline. It needs no build, so it runs first, with its own budget (R77.2-2).
            let walks: Walks = self.reach_all(step, &renames.iter().cloned().collect(), reach::WALK_STEPS.min(step_steps));
            crate::timing("the walk of the equivalence check", started);
            let found = run(&with_positions(plain.clone()), None, &walks, &costs);
            crate::timing(&format!("the walk and {} body checks of the equivalence check", plain.len()), started);
            (found, builds.join().unwrap_or_default(), walks)
        });
        // [R77.2-2] The prefix rule after each job ended: a job that ran with its own budget, but
        // that the costs of the jobs before it put past a budget, gets the result past the budget
        // (each cost is known now, so the result does not depend on the threads).
        let costs = costs.into_inner().unwrap_or_else(|error| error.into_inner());
        let mut answers: Vec<Option<Answer>> = vec![None; queries.len()];
        for (index, answer, late) in plain_answers.into_iter().chain(variant_answers) {
            let position = position_of.get(&index).copied();
            let past = position.is_some_and(|position| over(&costs, position));
            let answer = match (past, late, answer) {
                // The result past the budget is the result of `check_pair` with a budget of 0: the
                // proof by two equal MIR texts (the first test of `check_pair`), else "not checked".
                (true, false, Some(answer)) if !matches!(&answer.verdict, Verdict::NotChecked(cause) if cause == semilla_smt::budget::SAFETY_CAUSE) => Some(if matches!(&answer.verdict, Verdict::Equivalent(method) if method == "equal MIR") {
                    answer
                } else {
                    Answer { verdict: Verdict::NotChecked(semilla_verify::equiv::BUDGET_CAUSE.into()), inlined: Vec::new(), queries: 0, millis: 0, local: None }
                }),
                (_, _, answer) => answer,
            };
            answers[index] = answer;
        }
        let total: u64 = costs.iter().flatten().map(|(steps, _)| *steps).sum();
        let largest = costs.iter().flatten().map(|(steps, _)| *steps).max().unwrap_or(0);
        crate::timing(&format!("the equivalence check ({} queries, {total} solver steps, the largest job {largest}; a budget of {step_steps} solver steps)", queries.len()), started);
        // [R75.1-1] A suggested test for each sure difference of the step: the results of the
        // bodies, of the facts alone (in the tree of the variant), and of the walk.
        let variant_trees: Vec<Option<String>> = queries
            .iter()
            .map(|query| {
                let files = query.variant.as_ref()?;
                let files: BTreeMap<String, Option<String>> = files.iter().map(|(path, text)| (path.clone(), Some(text.clone()))).collect();
                self.repo.tree_with(&step.tree_before, &files).ok()
            })
            .collect();
        let mut walk_copy: Option<Vec<(String, reach::ConfigReach)>> = walks.as_ref().map(|walks| walks.as_ref().clone());
        {
            let mut items: Vec<(&str, &str, &str, &mut semilla_verify::equiv::Verdict)> = Vec::new();
            for ((query, answer), variant) in queries.iter().zip(answers.iter_mut()).zip(&variant_trees) {
                let Some(answer) = answer else { continue };
                let after = match (&query.variant, variant) {
                    (None, _) => step.tree_after.as_str(),
                    (Some(_), Some(tree)) => tree.as_str(),
                    (Some(_), None) => continue,
                };
                items.push((query.config.as_str(), step.tree_before.as_str(), after, &mut answer.verdict));
            }
            for (config, walk) in walk_copy.iter_mut().flatten() {
                for (_, _, _, verdict) in &mut walk.bodies {
                    items.push((config.as_str(), step.tree_before.as_str(), step.tree_after.as_str(), verdict));
                }
            }
            self.suggest(step, &mut items);
        }
        if let Some(copy) = walk_copy {
            let key = (step.tree_before.clone(), step.tree_after.clone());
            self.added.lock().unwrap_or_else(|error| error.into_inner()).reached.insert(key, std::sync::Arc::new(copy));
        }
        answers
    }

    fn equivalence_budget(&self, _step: &Step) -> Option<String> {
        Some(format!("{} solver steps, {} variant builds", equiv_steps(), variant_builds()))
    }

    fn reach(&self, step: &Step, renames: &BTreeMap<String, String>) -> Option<semilla_verify::equiv::ReachReport> {
        let renames: Vec<(String, String)> = renames.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
        let limit = reach::WALK_STEPS.min(equiv_steps());
        let mut walks = self.reach_all(step, &renames.into_iter().collect(), limit)?;
        // [R75.1-1] A walk with no test yet (a step with no other check): its tests.
        let open = walks.iter().any(|(_, walk)| walk.bodies.iter().any(|(_, _, _, verdict)| matches!(verdict, semilla_verify::equiv::Verdict::Different { test: semilla_verify::equiv::TestNote::Open(_), .. })));
        if open {
            let mut copy = walks.as_ref().clone();
            let mut items: Vec<(&str, &str, &str, &mut semilla_verify::equiv::Verdict)> = Vec::new();
            for (config, walk) in copy.iter_mut() {
                for (_, _, _, verdict) in &mut walk.bodies {
                    items.push((config.as_str(), step.tree_before.as_str(), step.tree_after.as_str(), verdict));
                }
            }
            self.suggest(step, &mut items);
            walks = std::sync::Arc::new(copy);
            let key = (step.tree_before.clone(), step.tree_after.clone());
            self.added.lock().unwrap_or_else(|error| error.into_inner()).reached.insert(key, walks.clone());
        }
        Some(reach::report(&walks))
    }

    fn defs(&self, step: &Step) -> Result<[semilla_verify::DefMap; 2]> {
        let checked = self.checked(step)?;
        let place = |place: Option<semilla_normal::elements::Place>| place.map(|place| (place.file, place.start, place.end));
        Ok([&step.tree_before, &step.tree_after].map(|tree| {
            let mut map = semilla_verify::DefMap::new();
            for config in &checked {
                for unit in self.units(tree, config).into_iter().flatten() {
                    for (own, named) in unit.defs.iter().flat_map(|defs| defs.entries()) {
                        let def = map.entry(own.path).or_default();
                        // A path of two targets (two units): the first place in the order of the
                        // places, not of the units (R61.16-2).
                        def.place = match (def.place.take(), place(own.place)) {
                            (Some(known), Some(found)) => Some(known.min(found)),
                            (known, found) => known.or(found),
                        };
                        for (symbol, flag) in named {
                            let entry = (semilla_verify::Sym { path: symbol.path, place: place(symbol.place) }, flag);
                            if !def.named.contains(&entry) {
                                def.named.push(entry);
                            }
                        }
                    }
                }
            }
            map
        }))
    }

    fn compare_with(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String], opaque: &dyn semilla_verify::OpaqueInputs) -> Result<Vec<BodyComparison>> {
        let checked = self.checked(step)?;
        self.read_sides(step, &checked);
        // N0 for each configuration, with the rename and move map (N1). Design 35.4: this is the
        // compare of a fact. The levels N2, N3, and N4 do not run.
        let mut states = Vec::new();
        for (index, config) in checked.iter().enumerate() {
            states.push(self.compare_n0(step, renames, added, config, index, opaque)?);
        }
        // The key of a body: its name and its place. Two definitions of one name under `cfg`
        // (a twin) have one name and two places, and one configuration compiles only one of
        // them. With the name as the key, the result of one definition went to the other
        // definition (design 35.8, batch 7b, part 4.4). A name with two places in one
        // configuration (two targets with one crate name) keeps the name as its key: the span
        // of the changed target is then the place of the body (batch 5n, A4).
        let mut many: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut prepared = Vec::new();
        for mut state in states {
            let own = match state.done.take() {
                Some(own) => own,
                None => {
                    let mut own = BTreeMap::new();
                    state.spans(&mut own);
                    let key: CompareKey = (
                        step.tree_before.clone(),
                        step.tree_after.clone(),
                        state.config.clone(),
                        state.renames.clone(),
                        added.to_vec(),
                        [state.before_marks.opaque.clone(), state.before_marks.stop.clone(), state.after_marks.opaque.clone(), state.after_marks.stop.clone()],
                    );
                    self.added.lock().unwrap_or_else(|error| error.into_inner()).compared.insert(key, (state.comparisons.clone(), own.clone()));
                    own
                }
            };
            many.extend(state.sides.two_places.iter().cloned());
            prepared.push((state, own));
        }
        Ok(merge_prepared(prepared, many))
    }

    fn truths(&self, step: &Step) -> Result<[Vec<semilla_verify::ConfigTruth>; 2]> {
        let checked = self.checked(step)?;
        Ok([&step.tree_before, &step.tree_after].map(|tree| checked.iter().map(|config| self.truth(tree, config)).collect()))
    }

    fn tests(&self, step: &Step) -> Result<Option<[std::collections::BTreeSet<String>; 2]>> {
        // A `#[test]` function has a constant with its name and the type `test::TestDescAndFn`
        // (batch 6a, B2). The N0 records of each checked configuration have it.
        let mut sides = [std::collections::BTreeSet::new(), std::collections::BTreeSet::new()];
        for config in self.checked(step)? {
            let records = self.sides(step, &config)?;
            for (side, levels) in sides.iter_mut().zip([&records.before, &records.after]) {
                side.extend(levels.get("n0").into_iter().flatten().filter(|record| record.text.starts_with("fn _() -> test::TestDescAndFn")).map(|record| record.name.clone()));
            }
        }
        Ok(Some(sides))
    }

    fn made(&self, step: &Step) -> Result<Option<[semilla_verify::made::Made; 2]>> {
        // The unit records of the N0 build of each checked configuration have the items of
        // the expanded code, and the body records have the macro call sites (batch 6b, part 2).
        let mut sides = [semilla_normal::Made::default(), semilla_normal::Made::default()];
        // Each cache folder has its own thread (design 37.20). The results join in the order of
        // the configurations and the trees, so the result is that of one thread.
        let checked = self.checked(step)?;
        let folders: Vec<(&Configuration, &String)> = checked.iter().flat_map(|config| [(config, &step.tree_before), (config, &step.tree_after)]).collect();
        let read = |config: &Configuration, tree: &str| -> Result<Option<semilla_normal::Made>> {
            self.records_or_build(tree, "n0", config, false)?;
            let (_, cache, _, _) = self.cache_folder(tree, "n0", config)?;
            let mut made = semilla_normal::Made::default();
            Ok(semilla_normal::read_made(&cache, &mut made).then_some(made))
        };
        let results: Vec<Result<Option<semilla_normal::Made>>> = std::thread::scope(|scope| {
            let handles: Vec<_> = folders.iter().map(|(config, tree)| scope.spawn(|| read(config, tree))).collect();
            handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a read thread failed")))).collect()
        });
        for (index, result) in results.into_iter().enumerate() {
            let Some(made) = result? else { return Ok(None) };
            sides[index % 2].items.extend(made.items);
            sides[index % 2].sites.extend(made.sites);
        }
        Ok(Some(sides.map(|side| semilla_verify::made::Made {
            items: side.items.into_iter().map(|item| semilla_verify::made::MadeItem { file: item.file, line: item.line, col: item.col, module: item.module, text: item.text, test: item.test, public: item.public, owner: item.owner }).collect(),
            sites: side.sites,
        })))
    }

    fn expansions(&self, step: &Step) -> Option<[Vec<semilla_verify::expand::ExpansionUnit>; 2]> {
        // The unit records of the N0 build of each checked configuration have the expansion of
        // each macro call (design 63). Each cache folder has its own thread.
        let checked = self.checked(step).ok()?;
        let folders: Vec<(&Configuration, &String)> = checked.iter().flat_map(|config| [(config, &step.tree_before), (config, &step.tree_after)]).collect();
        let read = |config: &Configuration, tree: &str| -> Option<Vec<semilla_verify::expand::ExpansionUnit>> {
            self.records_or_build(tree, "n0", config, false).ok()?;
            let (_, cache, _, _) = self.cache_folder(tree, "n0", config).ok()?;
            let units = semilla_normal::read_expansions(&cache)?;
            Some(units.into_iter().map(|unit| semilla_verify::expand::ExpansionUnit { config: config.name(), target: unit.target, package: unit.package, root: unit.root, cfg: unit.cfg, list: unit.list.into_iter().filter_map(|value| serde_json::from_value(value).ok()).collect(), names: unit.names.and_then(|value| serde_json::from_value(value).ok()).map(std::sync::Arc::new) }).collect())
        };
        let results: Vec<Option<Vec<semilla_verify::expand::ExpansionUnit>>> = std::thread::scope(|scope| {
            let handles: Vec<_> = folders.iter().map(|(config, tree)| scope.spawn(|| read(config, tree))).collect();
            handles.into_iter().map(|handle| handle.join().ok().flatten()).collect()
        });
        let mut sides: [Vec<semilla_verify::expand::ExpansionUnit>; 2] = Default::default();
        for (index, result) in results.into_iter().enumerate() {
            sides[index % 2].extend(result?);
        }
        Some(sides)
    }

    fn facts(&self, step: &Step) -> Result<Option<Vec<semilla_verify::BodyFacts>>> {
        // The facts of each checked configuration (batch 5s, A5). The verifier joins the facts
        // of the bodies of an item, so a property claim must hold in each configuration.
        // Each configuration has its own thread (design 37.20). The facts join in the order of
        // the configurations.
        let checked = self.checked(step)?;
        let results: Vec<Result<Vec<semilla_verify::BodyFacts>>> = std::thread::scope(|scope| {
            let handles: Vec<_> = checked.iter().map(|config| scope.spawn(move || self.facts_of(step, config))).collect();
            handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a read thread failed")))).collect()
        });
        let mut facts = Vec::new();
        for result in results {
            facts.extend(result?);
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
        let (listed, same) = self.configurations_with(&step.tree_before, &step.tree_after, &extra)?;
        // A feature that only one tree has (design 37.7): the configuration is a configuration
        // of that tree only. Semilla does not build it, and the output names the feature.
        let metadata = [self.metadata(&step.tree_before), self.metadata(&step.tree_after)];
        let one_side = |config: &Configuration| -> Option<String> {
            let missing: Vec<Vec<String>> = metadata.iter().map(|data| data.as_ref().map(|data| semilla_normal::cargo::missing_features(data, &config.features)).unwrap_or_default()).collect();
            one_side_cause(&missing[0], &missing[1])
        };
        let (listed, one_sided): (Vec<_>, Vec<_>) = listed.into_iter().partition(|(config, _)| one_side(config).is_none());
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
        let mut log = StepConfigs { same, ..StepConfigs::default() };
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
                Some(cause) => log.not_checked.push((config.name(), cause)),
            }
        }
        for (config, _) in &one_sided {
            log.not_checked.push((config.name(), one_side(config).unwrap_or_default()));
        }
        if checked.is_empty() && self.after_commit {
            // Design 62: no configuration builds on the two sides of the step. Each cause is in
            // the error of the step.
            let causes: Vec<String> = log.not_checked.iter().map(|(name, cause)| format!("`{name}`: {cause}")).collect();
            anyhow::bail!("no configuration of the default list builds before the step and after it ({})", causes.join("; "));
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
        let sides = self.sides(step, config)?;
        let records = sides.after.get("n0").cloned().unwrap_or_default();
        // The MIR signature of each body before the step, by its name and its target (batch 5n,
        // A1). A body with a new name has no entry: its signature is not a known cause.
        let signature = |text: &str| text.lines().next().unwrap_or("").to_string();
        let old: BTreeMap<(String, String), String> = sides.before.get("n0").into_iter().flatten().map(|record| ((record.name.clone(), record.target.clone()), signature(&record.text))).collect();
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
                    sure: record.sure_callees,
                    ends: record.sure_ends.unwrap_or_default(),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // Batch 7c, part 6.1 (design 37.20): the data of a record set is the data that each compare
    // made before: the span and the names of each record in the order of the records, and the
    // names with two places.
    #[test]
    fn a_record_set_has_the_span_and_the_names_of_each_record() {
        let record = |name: &str, file: &str, line: usize, names: serde_json::Value| serde_json::json!({ "name": name, "level": "n0", "text": "fn f() {}", "span": { "file": file, "line_start": line, "line_end": line + 2 }, "names": names }).to_string();
        let old = [record("demo::f", "src/lib.rs", 3, serde_json::json!([{ "word": "len", "member": true, "count": 0 }, { "word": "Vec", "prefix": true, "count": 2, "line": 4 }])), serde_json::json!({ "name": "demo::g", "level": "n0", "text": "fn g() {}" }).to_string()];
        let new = [record("demo::f", "src/lib.rs", 3, serde_json::json!([])), record("demo::main", "src/main.rs", 1, serde_json::json!([])), record("demo::main", "benches/demo.rs", 9, serde_json::json!([]))];
        let levels = |lines: &[String]| semilla_normal::Levels::from([("n0".to_string(), semilla_normal::parse_records(&lines.join("\n")).unwrap())]);
        let sides = Sides::new(levels(&old), levels(&new));
        let before: Vec<_> = sides.records(false).collect();
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].0.name, "demo::f");
        assert_eq!(before[0].1.0, Some(("src/lib.rs".to_string(), 3)));
        // A count of zero is one use, as in each compare before this batch.
        let names: Vec<(&str, bool, bool, usize, usize)> = before[0].1.1.iter().map(|name| (name.word.as_str(), name.member, name.prefix, name.count, name.line)).collect();
        assert_eq!(names, [("len", true, false, 1, 0), ("Vec", false, true, 2, 4)]);
        assert_eq!((before[1].0.name.as_str(), &before[1].1.0, before[1].1.1.len()), ("demo::g", &None, 0));
        let after: Vec<_> = sides.records(true).map(|(record, (span, _))| (record.name.clone(), span.clone())).collect();
        assert_eq!(after, [("demo::f".to_string(), Some(("src/lib.rs".to_string(), 3))), ("demo::main".to_string(), Some(("src/main.rs".to_string(), 1))), ("demo::main".to_string(), Some(("benches/demo.rs".to_string(), 9)))]);
        assert_eq!(sides.two_places, ["demo::main"]);
        assert_eq!(sides.two_places, sides.names_with_two_places());
    }

    // Batch 5x, part F: the records in memory are the records of the folder on the disk.
    #[test]
    fn the_records_in_memory_follow_the_cache_folder() {
        let folder = tempfile::tempdir().unwrap();
        let write = |text: &str, seconds: u64| {
            let line = serde_json::json!({ "name": "demo::f", "level": "n0", "text": text });
            std::fs::create_dir_all(folder.path()).unwrap();
            std::fs::write(folder.path().join("unit.jsonl"), line.to_string()).unwrap();
            let complete = std::fs::File::create(folder.path().join(".complete")).unwrap();
            complete.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds)).unwrap();
        };
        let in_memory = || RECORDS.lock().unwrap().keys().filter(|(path, _)| path == folder.path()).count();
        write("a", 1_000);
        let first = read_cached(folder.path()).unwrap();
        assert_eq!(first[0].text, "a");
        assert_eq!(in_memory(), 1);
        // The second read gives the same records, also if the record file is not there.
        std::fs::remove_file(folder.path().join("unit.jsonl")).unwrap();
        assert_eq!(read_cached(folder.path()).unwrap(), first);
        // A build made the folder again: the `.complete` file has a new time.
        write("b", 2_000);
        assert_eq!(read_cached(folder.path()).unwrap()[0].text, "b");
        // The removal of the folder removes its records in memory.
        remove_cache(folder.path());
        assert_eq!(in_memory(), 0);
        write("c", 2_000);
        assert_eq!(read_cached(folder.path()).unwrap()[0].text, "c");
    }

    fn equal(level: &str) -> BodyStatus {
        BodyStatus::Equal { level: level.into(), proof: Vec::new() }
    }

    /// Design 61.16 (R61.16-2): the record of a name for a target with no record of the name.
    /// The choice does not depend on the order of the records, and two source bodies (two files)
    /// or two texts give no record. The negative case: the record of the target itself, and
    /// one text in one file.
    // rule+ R61.16-2
    // rule- R61.16-2
    #[test]
    fn the_record_of_a_name_for_a_target_does_not_depend_on_the_order() {
        let record = |name: &str, target: &str, file: &str, text: &str| {
            let line = serde_json::json!({ "name": name, "level": "n0", "text": text, "target": target, "span": { "file": file, "line_start": 1, "line_end": 3 } });
            semilla_normal::parse_records(&line.to_string()).unwrap().remove(0)
        };
        let mut records = vec![
            record("k::f", "k.test.2222222222222222", "src/lib.rs", "b"),
            record("k::f", "k.lib.1111111111111111", "src/lib.rs", "a"),
            record("p::h", "p.test.3333333333333333", "tests/p.rs", "c"),
            record("p::h", "p.test.4444444444444444", "benches/p.rs", "d"),
            record("q::g", "q.bin.5555555555555555", "src/main.rs", "e"),
            record("q::g", "q.test.6666666666666666", "src/main.rs", "e"),
            semilla_normal::BodyRecord { unit: "panic=\"unwind\"".into(), ..record("w::f", "w.lib.7777777777777777", "src/lib.rs", "unwind continue") },
            semilla_normal::BodyRecord { unit: "panic=\"abort\"".into(), ..record("w::f", "w.lib.8888888888888888", "src/lib.rs", "unwind unreachable") },
        ];
        for _ in 0..2 {
            let side = EquivSide::new(&records);
            let text = |name: &str, target: &str| side.find(name, target, "").map(|record| record.text.clone());
            // Two builds of one library: the build with the settings of the record that asks.
            assert_eq!(side.find("w::f", "w.lib.9999999999999999", "panic=\"abort\"").map(|record| record.text.as_str()), Some("unwind unreachable"));
            assert_eq!(side.find("w::f", "z.test.9999999999999999", "panic=\"unwind\"").map(|record| record.text.as_str()), Some("unwind continue"));
            assert_eq!(text("w::f", "w.lib.9999999999999999"), None);
            // The record of the target.
            assert_eq!(text("k::f", "k.test.2222222222222222").as_deref(), Some("b"));
            // A new hash: the one target with the same name without the hash.
            assert_eq!(text("k::f", "k.test.9999999999999999").as_deref(), Some("b"));
            // A target of a different crate links to the library.
            assert_eq!(text("k::f", "z.test.9999999999999999").as_deref(), Some("a"));
            // Two source bodies, or two targets of one name without the hash: no record.
            assert_eq!(text("p::h", "z.test.9999999999999999"), None);
            assert_eq!(text("p::h", "p.test.9999999999999999"), None);
            // The negative case: one file and one text.
            assert_eq!(text("q::g", "z.test.9999999999999999").as_deref(), Some("e"));
            records.reverse();
        }
        // The span of a name with two places in one tree: the first place, in each order.
        let places = [("a", ("x.rs".to_string(), 9)), ("a", ("x.rs".to_string(), 2)), ("b", ("y.rs".to_string(), 1))];
        assert_eq!(first_places(places.iter().cloned()), first_places(places.iter().rev().cloned()));
        assert_eq!(first_places(places.iter().cloned())["a"], ("x.rs".to_string(), 2));
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

    // R66.11-3: the variant of each fact in the first configuration comes before each variant in
    // the next one. The negative case: in one configuration, the order of the queries stays, and
    // the queries of the step (no variant) build nothing.
    // rule+ R66.11-3
    // rule- R66.11-3
    #[test]
    fn the_variant_builds_give_each_fact_its_first_configuration_first() {
        let query = |config: &str, file: Option<&str>| semilla_verify::equiv::Query { config: config.into(), old: ("o".into(), "t".into()), new: ("n".into(), "t".into()), variant: file.map(|text| BTreeMap::from([("src/lib.rs".to_string(), text.to_string())])) };
        let rank = |config: &str| ["dev", "release"].iter().position(|known| *known == config).unwrap_or(usize::MAX);
        let queries = [query("dev", None), query("dev", Some("a")), query("release", Some("a")), query("dev", Some("b")), query("release", Some("b")), query("release", Some("b"))];
        let order: Vec<(String, String)> = variant_order(&queries, &rank).into_iter().map(|(_, config, files)| (config, files["src/lib.rs"].clone())).collect();
        let pair = |config: &str, file: &str| (config.to_string(), file.to_string());
        assert_eq!(order, vec![pair("dev", "a"), pair("dev", "b"), pair("release", "a"), pair("release", "b")]);
        let one = [query("dev", Some("b")), query("dev", Some("a")), query("dev", None)];
        let order: Vec<String> = variant_order(&one, &rank).into_iter().map(|(_, _, files)| files["src/lib.rs"].clone()).collect();
        assert_eq!(order, vec!["b", "a"]);
    }

    // Batch 10q, part D (R66.5-1): the process reads each form text one time. Two records with
    // one text of `q` (or of `qx`) give one parsed form (the same `Arc`); a record with another
    // text gives its own form. The forms are equal to a new read of the text.
    // rule+ R66.5-1
    // rule- R66.5-1
    #[test]
    fn a_form_text_is_read_one_time_and_another_text_gives_its_own_form() {
        let record = |q: &str, qx: &str| serde_json::from_value::<semilla_normal::BodyRecord>(serde_json::json!({ "name": "probe::f", "level": "n0", "text": "t", "q": q, "qx": qx })).unwrap();
        let form = r#"{"a":1,"l":[0,0],"t":[{"Int":[32,false]}],"b":[{"t":{"k":"Ret"}}]}"#;
        let other = r#"{"a":2,"l":[0,0,0],"t":[{"Int":[32,false]}],"b":[{"t":{"k":"Ret"}}]}"#;
        let std = format!("{{\"core::f\":{form}}}");
        let (a, b, c) = (record(form, &std), record(form, &std), record(other, "{}"));
        let (fa, fb, fc) = (record_form(&a).unwrap(), record_form(&b).unwrap(), record_form(&c).unwrap());
        assert!(std::sync::Arc::ptr_eq(&fa, &fb));
        assert!(!std::sync::Arc::ptr_eq(&fa, &fc));
        assert_eq!((fa.a, fc.a), (1, 2));
        let (sa, sb) = (std_forms(&[&a]), std_forms(&[&b]));
        assert!(std::sync::Arc::ptr_eq(&sa["core::f"], &sb["core::f"]));
        assert!(std_forms(&[&c]).is_empty());
        assert_eq!(sa["core::f"].b.len(), serde_json::from_str::<semilla_smt::mir::Ir>(form).unwrap().b.len());
    }

    // Batch 10o, part E (R61.16-1, `by_name` of `merge_prepared`): one body name has records
    // in two files in one tree of one configuration (a bench and a test, each a crate `probe`)
    // and one comparison with no split and no file. Its span is not the span of the name: that
    // span is the place of one of the two bodies. A name with one file keeps the span of the
    // name. With `by_name` always true, the first assert fails.
    // rule+ R61.16-1
    // rule- R61.16-1
    #[test]
    fn a_name_with_records_in_two_files_and_no_split_gets_no_span_by_its_name() {
        let record = |target: &str, file: &str, name: &str, text: &str| serde_json::json!({ "name": name, "level": "n0", "text": text, "target": target, "span": { "file": file, "line_start": 1, "line_end": 3 } }).to_string();
        let before = [record("probe.bench.1", "benches/probe.rs", "probe::hash", "a"), record("probe.test.2", "tests/probe.rs", "probe::hash", "b"), record("probe.test.2", "tests/probe.rs", "probe::only", "c")];
        let after = [record("probe.bench.1", "benches/probe.rs", "probe::hash", "a2"), record("probe.test.2", "tests/probe.rs", "probe::hash", "b"), record("probe.test.2", "tests/probe.rs", "probe::only", "c2")];
        let levels = |lines: &[String]| semilla_normal::Levels::from([("n0".to_string(), semilla_normal::parse_records(&lines.join("\n")).unwrap())]);
        let sides = Sides::new(levels(&before), levels(&after));
        assert_eq!(sides.two_files, ["probe::hash"]);
        let comparison = |name: &str| semilla_normal::BodyComparison { name: name.to_string(), status: semilla_normal::BodyStatus::Different { cfg_changed: false }, config: Vec::new(), elements: None, graphs: Vec::new(), place: None };
        let state = SetState {
            config: Configuration::DEV,
            renames: BTreeMap::new(),
            sides: std::sync::Arc::new(sides),
            before_marks: semilla_normal::Marks::default(),
            after_marks: semilla_normal::Marks::default(),
            comparisons: vec![comparison("probe::hash"), comparison("probe::only")],
            done: None,
        };
        let mut own = BTreeMap::new();
        state.spans(&mut own);
        assert!(own.contains_key("probe::hash"), "{own:?}");
        let merged = merge_prepared(vec![(state, own)], std::collections::BTreeSet::new());
        let span = |name: &str| merged.iter().find(|found| found.name == name).map(|found| found.span.clone());
        assert_eq!(span("probe::hash"), Some(None));
        assert_eq!(span("probe::only"), Some(Some(("tests/probe.rs".to_string(), 1))));
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

    // Batch 7c, part 3.2 (design 37.7).
    #[test]
    fn a_feature_of_one_tree_only_gives_a_configuration_with_no_compare() {
        use semilla_normal::cargo::missing_features;
        let metadata = serde_json::json!({ "packages": [
            { "name": "plain", "features": {}, "dependencies": [] },
            { "name": "rich", "features": { "default": ["std"], "std": [] }, "dependencies": [{ "name": "serde", "optional": true }, { "name": "real", "rename": "alias", "optional": true }, { "name": "log" }] },
        ]});
        let missing = |names: &[&str]| missing_features(&metadata, &Features::Listed(names.iter().map(|name| name.to_string()).collect()));
        // A feature of the table, and an optional dependency with its name in the manifest.
        assert!(missing(&["std", "serde", "alias"]).is_empty());
        assert_eq!(missing(&["std", "serde1", "real", "log"]), ["serde1", "real", "log"]);
        // `package/feature`: a feature of one package. A package of a different workspace is not known.
        assert_eq!(missing(&["rich/std", "plain/std", "other/std"]), ["plain/std"]);
        assert!(missing_features(&metadata, &Features::All).is_empty());
        assert_eq!(missing_features(&metadata, &Features::NoDefaultListed(vec!["none".into()])), ["none"]);

        let list = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<Vec<String>>();
        assert_eq!(one_side_cause(&[], &list(&["serde1"])).unwrap(), "only the code before the step has the feature `serde1`. The code after the step has no such configuration: no compare");
        assert_eq!(one_side_cause(&list(&["a", "b"]), &[]).unwrap(), "only the code after the step has the features `a`, `b`. The code before the step has no such configuration: no compare");
        // The two trees have each feature, or no tree has a feature: the usual build decides.
        assert_eq!(one_side_cause(&[], &[]), None);
        assert_eq!(one_side_cause(&list(&["a"]), &list(&["a"])), None);
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
