//! The adapters from the verifier traits to the real normal forms and the real SMT solver.

use std::collections::BTreeMap;

use anyhow::Result;
use semilla_core::Step;
use semilla_core::report::Effect;
use semilla_verify::{BodyComparison, BodyOracle, BodyStatus, EffectCheck, ExprOracle};

use crate::git::Repo;

/// Runs the MIR driver on the snapshots, and compares the bodies with the normal forms.
///
/// Each level has one fixed build folder. Git updates it to each tree by delta, so cargo compiles
/// only the changed crates and their dependents. The records of each tree and level are cached.
pub struct NormalOracle {
    pub repo: Repo,
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

    /// The records of a tree at a level, with the default features.
    pub fn records(&self, tree: &str, level: &str) -> Result<Vec<semilla_normal::BodyRecord>> {
        self.records_with(tree, level, &[])
    }

    /// The records of a tree at a level and a feature set: from the cache, or from a delta build.
    fn records_with(&self, tree: &str, level: &str, features: &[String]) -> Result<Vec<semilla_normal::BodyRecord>> {
        let name = format!("{level}{}", Self::set_suffix(features));
        let cache = self.repo.state_dir().join("cache/mir").join(tree).join(&name);
        if cache.join(".complete").exists() {
            return semilla_normal::read_records(&cache);
        }
        let base = self.repo.state_dir().join("cache/build");
        std::fs::create_dir_all(&base)?;
        // One build folder serves one build at a time, also across processes.
        let lock = std::fs::File::create(base.join(format!("{name}.lock")))?;
        lock.lock()?;
        // Another process can finish the same build while this one waits for the lock.
        if cache.join(".complete").exists() {
            lock.unlock()?;
            return semilla_normal::read_records(&cache);
        }
        let build = base.join(&name);
        let records = base.join(format!("{name}-records"));
        let target = self.repo.state_dir().join("cache").join(format!("target-{name}"));
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
        std::fs::write(cache.join(".complete"), "")?;
        lock.unlock()?;
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
    ) -> Result<Vec<semilla_normal::BodyComparison>> {
        let (mut before, mut after) = self.levels(&step.tree_before, &step.tree_after, &["n0"], features)?;
        let renames = &with_moved_closures(renames, added, &before, &after);
        let mut comparisons = semilla_normal::compare(&before, &after, renames, added);
        if comparisons.iter().any(|c| !matches!(c.status, semilla_normal::BodyStatus::Equal { .. })) {
            let (old, new) = self.levels(&step.tree_before, &step.tree_after, &["n2", "n3"], features)?;
            before.extend(old);
            after.extend(new);
            let started = std::time::Instant::now();
            comparisons = semilla_normal::compare(&before, &after, renames, added);
            crate::timing("compare the bodies (N0 to N4)", started);
        }
        // The span of each body: the "after" records win (the name is the "after" name).
        for record in [&before, &after].iter().flat_map(|levels| levels.values().flatten()) {
            if let Some(span) = &record.span {
                spans.insert(record.name.clone(), (span.file.clone(), span.line_start));
            }
        }
        Ok(comparisons)
    }
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

impl BodyOracle for NormalOracle {
    fn compare(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String]) -> Result<Vec<BodyComparison>> {
        let sets = self.feature_sets(&[&step.tree_before, &step.tree_after])?;
        let mut spans = BTreeMap::new();
        let mut combined: BTreeMap<String, BodyStatus> = BTreeMap::new();
        for features in &sets {
            for comparison in self.compare_set(step, renames, added, features, &mut spans)? {
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
        Ok(combined.into_iter().map(|(name, status)| BodyComparison { span: spans.get(&name).cloned(), status, name }).collect())
    }

    fn facts(&self, step: &Step) -> Result<Option<Vec<semilla_verify::BodyFacts>>> {
        let records = self.records(&step.tree_after, "n0")?;
        Ok(Some(
            records
                .into_iter()
                .map(|record| semilla_verify::BodyFacts {
                    span: record.span.as_ref().map(|span| (span.file.clone(), span.line_start)),
                    callees: record.callees,
                    externals: record.externals.into_iter().map(|call| call.path).collect(),
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
    pub fn new() -> SmtOracle {
        SmtOracle { solver: semilla_smt::Solver::find().ok() }
    }

    pub fn available(&self) -> bool {
        self.solver.is_some()
    }

    fn env(types: &BTreeMap<String, String>) -> semilla_smt::TypeEnv {
        let mut env = semilla_smt::TypeEnv::new();
        for (name, ty) in types {
            env.insert(name, semilla_smt::TypeEnv::parse_type(ty));
        }
        env
    }
}

impl Default for SmtOracle {
    fn default() -> Self {
        Self::new()
    }
}

impl ExprOracle for SmtOracle {
    fn classify(&self, before: &str, after: &str, types: &BTreeMap<String, String>) -> Option<EffectCheck> {
        let solver = self.solver.as_ref()?;
        let result = semilla_smt::classify_condition(solver, before, after, &Self::env(types)).ok()?;
        let mut witness = result.witness;
        if result.panic_conditions_differ {
            let note = result.panic_witness.map(|w| format!("the panic conditions differ, for example {w}")).unwrap_or_else(|| "the panic conditions differ".into());
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
