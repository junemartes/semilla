//! The adapters from the verifier traits to the real normal forms and the real SMT solver.

use std::collections::BTreeMap;

use anyhow::Result;
use semilla_core::Step;
use semilla_core::report::Effect;
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
/// 28). It has a separate folder set, `build-code/<base tree>/`, with its own build folders,
/// target folders, and records. Only a command with the permission builds such a tree or reads
/// its records (I6).
pub struct NormalOracle {
    pub repo: Repo,
    /// The build code state of each tree of the change set.
    pub code: crate::buildcode::BuildCode,
    pub mode: crate::buildcode::Mode,
}

/// The name of the file that marks the records of a tree with changed build code. The file
/// holds the base tree of the change set.
pub const BUILD_CODE_FILE: &str = ".build-code";

/// The separate folder set of the trees with changed build code of the change sets on one base.
pub fn build_code_root(repo: &Repo, base: &str) -> std::path::PathBuf {
    repo.cache_dir().join("build-code").join(base)
}

/// Why a build of the records that this process used ran with no sandbox (design 27).
static NO_SANDBOX: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The name of the file that marks the records of a build with no sandbox. The file holds the cause.
const NO_SANDBOX_FILE: &str = ".no-sandbox";

/// The warning for a result, if a build of its records ran with no sandbox: code of the change
/// set (a build script, a proc macro) could write each file of the user in that build.
pub fn no_sandbox_warning() -> Option<String> {
    let cause = NO_SANDBOX.lock().ok()?.clone()?;
    Some(format!("{} ({cause})", semilla_normal::sandbox::NO_SANDBOX))
}

/// Reads the sandbox state of a records folder.
fn note_sandbox(cache: &std::path::Path) {
    if let Ok(cause) = std::fs::read_to_string(cache.join(NO_SANDBOX_FILE))
        && let Ok(mut state) = NO_SANDBOX.lock()
    {
        *state = Some(cause.trim().to_string());
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

    /// The folder suffix of a feature set: empty for the default features.
    fn set_suffix(features: &[String]) -> String {
        if features.is_empty() {
            return String::new();
        }
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        features.hash(&mut hasher);
        format!("-f{:012x}", hasher.finish() & 0xffff_ffff_ffff)
    }

    /// The folder set of a tree, and true if it is the separate set of the changed build code.
    ///
    /// The shared folders (the cache folder itself) serve only a tree with the build code of the
    /// base. A tree with no state is never built.
    pub fn root(&self, tree: &str) -> Result<(std::path::PathBuf, bool)> {
        if self.code.clean.contains(tree) {
            return Ok((self.repo.cache_dir(), false));
        }
        if !self.code.trees.contains(tree) {
            anyhow::bail!("an internal error: tree {tree} has no build code state, so Semilla does not build it");
        }
        if !self.mode.runs_build_code() {
            anyhow::bail!("tree {tree} has build code that the change set changes; a reviewer command builds it only with --trust-build-code (I6)");
        }
        Ok((build_code_root(&self.repo, &self.code.base), true))
    }

    /// The records of a tree at a level, with the default features.
    pub fn records(&self, tree: &str, level: &str) -> Result<Vec<semilla_normal::BodyRecord>> {
        self.records_with(tree, level, &[])
    }

    /// The records of a tree at a level and a feature set: from the cache, or from a delta build.
    fn records_with(&self, tree: &str, level: &str, features: &[String]) -> Result<Vec<semilla_normal::BodyRecord>> {
        // The cache key: the tree, the level, the feature set, and the build settings that are not
        // in the tree (the outer cargo configuration and the driver binary).
        let (root, marked) = self.root(tree)?;
        let base = root.join("build");
        let settings = semilla_normal::cargo::settings_key(&base, semilla_normal::cargo::find_driver().ok().as_deref());
        let name = format!("{level}{}", Self::set_suffix(features));
        let cache = root.join("mir").join(tree).join(format!("{name}-s{settings}"));
        // A record with the mark is never a record of the shared folders.
        if !marked && cache.join(BUILD_CODE_FILE).exists() {
            let _ = std::fs::remove_dir_all(&cache);
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
        semilla_normal::run_cargo_check_delta_features(&build, level, &records, &target, features)?;
        crate::timing(&format!("cargo check {name} of tree {}", &tree[..8.min(tree.len())]), started);
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache)?;
        for entry in std::fs::read_dir(&records)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                std::fs::copy(&path, cache.join(path.file_name().unwrap()))?;
            }
        }
        // The sandbox state of the build stays with the records: a later result that uses them
        // shows the same warning.
        if let Some(cause) = semilla_normal::sandbox::problem() {
            std::fs::write(cache.join(NO_SANDBOX_FILE), cause)?;
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
    fn levels(&self, before: &str, after: &str, levels: &[&str], features: &[String]) -> Result<(semilla_normal::Levels, semilla_normal::Levels)> {
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

    /// Compares the bodies of one feature set. N0 first; N2 and N3 only when a body is still
    /// different at N0.
    fn compare_set(
        &self,
        step: &Step,
        renames: &BTreeMap<String, String>,
        added: &[String],
        features: &[String],
        spans: &mut BTreeMap<String, (String, usize)>,
        opaque: &dyn semilla_verify::OpaqueInputs,
    ) -> Result<Vec<semilla_normal::BodyComparison>> {
        let (mut before, mut after) = self.levels(&step.tree_before, &step.tree_after, &["n0"], features)?;
        let renames = &with_moved_closures(renames, added, &before, &after);
        // The bodies that the build configuration selects, by the syntax tree (batch 5l). No
        // normal form looks into them.
        for (levels, is_after) in [(&before, false), (&after, true)] {
            for record in levels.get("n0").into_iter().flatten() {
                let span = record.span.as_ref().map(|span| (span.file.clone(), span.line_start));
                opaque.note(is_after, &record.name, span.as_ref(), &record.adts);
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
        let mut comparisons = semilla_normal::compare_with(&before, &after, renames, added, &before_marks, &after_marks);
        if comparisons.iter().any(|c| !matches!(c.status, semilla_normal::BodyStatus::Equal { .. })) {
            let (old, new) = self.levels(&step.tree_before, &step.tree_after, &["n2", "n3"], features)?;
            before.extend(old);
            after.extend(new);
            semilla_normal::drop_opaque_inlines(&mut before, &before_marks);
            semilla_normal::drop_opaque_inlines(&mut after, &after_marks);
            let started = std::time::Instant::now();
            comparisons = semilla_normal::compare_with(&before, &after, renames, added, &before_marks, &after_marks);
            crate::timing("compare the bodies (N0 to N4)", started);
        }
        // The span of each body: the "after" records win (the name is the "after" name).
        for record in [&before, &after].iter().flat_map(|levels| levels.values().flatten()) {
            if let Some(span) = &record.span {
                spans.insert(record.name.clone(), (span.file.clone(), span.line_start));
            }
        }
        if let (Some(old), Some(new)) = (before.get("n0"), after.get("n0")) {
            spans.extend(changed_target_spans(old, new));
        }
        Ok(comparisons)
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

/// Combines the status of one body over the feature sets: equal only if it is equal in every set.
fn combine(old: BodyStatus, new: BodyStatus) -> BodyStatus {
    match (old, new) {
        (BodyStatus::Equal { level: a, proof: mut p }, BodyStatus::Equal { level: b, proof: q }) => {
            p.extend(q.into_iter().filter(|rule| !p.contains(rule)).collect::<Vec<_>>());
            BodyStatus::Equal { level: a.max(b), proof: p }
        }
        (BodyStatus::Different { cfg_changed: a }, BodyStatus::Different { cfg_changed: b }) => BodyStatus::Different { cfg_changed: a || b },
        (BodyStatus::Different { cfg_changed }, _) | (_, BodyStatus::Different { cfg_changed }) => BodyStatus::Different { cfg_changed },
        (BodyStatus::Equal { .. }, other) | (other, BodyStatus::Equal { .. }) => other,
        (old, _) => old,
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
        let sets = self.feature_sets(&[&step.tree_before, &step.tree_after])?;
        let mut spans = BTreeMap::new();
        let mut combined: BTreeMap<String, BodyStatus> = BTreeMap::new();
        let mut config: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for features in &sets {
            for comparison in self.compare_set(step, renames, added, features, &mut spans, opaque)? {
                if !comparison.config.is_empty() {
                    config.entry(comparison.name.clone()).or_default().extend(comparison.config.iter().cloned());
                }
                let status = match comparison.status {
                    semilla_normal::BodyStatus::Equal { level, proof } => BodyStatus::Equal { level, proof },
                    semilla_normal::BodyStatus::Different { cfg_changed } => BodyStatus::Different { cfg_changed },
                    semilla_normal::BodyStatus::BeforeOnly => BodyStatus::BeforeOnly,
                    semilla_normal::BodyStatus::AfterOnly => BodyStatus::AfterOnly,
                };
                let merged = match combined.remove(&comparison.name) {
                    Some(old) => combine(old, status),
                    None => status,
                };
                combined.insert(comparison.name, merged);
            }
        }
        Ok(combined
            .into_iter()
            .map(|(name, status)| BodyComparison { span: spans.get(&name).cloned(), status, config: config.remove(&name).into_iter().flatten().collect(), name })
            .collect())
    }

    fn facts(&self, step: &Step) -> Result<Option<Vec<semilla_verify::BodyFacts>>> {
        let records = self.records(&step.tree_after, "n0")?;
        // The MIR signature of each body before the step, by its name and its target (batch 5n,
        // A1). A body with a new name has no entry: its signature is not a known cause.
        let signature = |text: &str| text.lines().next().unwrap_or("").to_string();
        let old: BTreeMap<(String, String), String> = self.records(&step.tree_before, "n0")?.into_iter().map(|record| ((record.name, record.target), signature(&record.text))).collect();
        Ok(Some(
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
                    target: record.target,
                    live: record.live_callees,
                    param_types: record.param_types.into_iter().map(|t| (t.path, t.krate)).collect(),
                    const_types: record.const_types.into_iter().map(|t| (t.path, t.krate)).collect(),
                    casts: semilla_verify::discriminant_casts(&record.text),
                    layouts: semilla_verify::layout_types(&record.text),
                    name: record.name,
                })
                .collect(),
        ))
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

    #[test]
    fn a_body_is_equal_only_in_every_feature_set() {
        assert_eq!(combine(equal("n0"), equal("n2")), equal("n2"));
        assert_eq!(combine(equal("n0"), BodyStatus::Different { cfg_changed: false }), BodyStatus::Different { cfg_changed: false });
        assert_eq!(combine(BodyStatus::Different { cfg_changed: true }, equal("n0")), BodyStatus::Different { cfg_changed: true });
        assert_eq!(combine(BodyStatus::Different { cfg_changed: false }, BodyStatus::Different { cfg_changed: true }), BodyStatus::Different { cfg_changed: true });
        assert_eq!(combine(equal("n0"), BodyStatus::AfterOnly), BodyStatus::AfterOnly);
    }

    #[test]
    fn the_default_set_has_no_suffix() {
        assert_eq!(NormalOracle::set_suffix(&[]), "");
        let a = NormalOracle::set_suffix(&["serde".to_string()]);
        assert!(a.starts_with("-f") && a != NormalOracle::set_suffix(&["std".to_string()]));
    }
}

