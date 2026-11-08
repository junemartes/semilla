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
    /// The records of a tree at a level: from the cache, or from a delta build.
    pub fn records(&self, tree: &str, level: &str) -> Result<Vec<semilla_normal::BodyRecord>> {
        let cache = self.repo.state_dir().join("cache/mir").join(tree).join(level);
        if cache.join(".complete").exists() {
            return semilla_normal::read_records(&cache);
        }
        let base = self.repo.state_dir().join("cache/build");
        std::fs::create_dir_all(&base)?;
        // One build folder serves one build at a time, also across processes.
        let lock = std::fs::File::create(base.join(format!("{level}.lock")))?;
        lock.lock()?;
        let build = base.join(level);
        let records = base.join(format!("{level}-records"));
        let target = self.repo.state_dir().join("cache").join(format!("target-{level}"));
        let started = std::time::Instant::now();
        self.repo.checkout_build(&build, tree)?;
        semilla_normal::run_cargo_check_delta(&build, level, &records, &target)?;
        crate::timing(&format!("cargo check {level} of tree {}", &tree[..8.min(tree.len())]), started);
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
    fn levels(&self, before: &str, after: &str, levels: &[&str]) -> Result<(semilla_normal::Levels, semilla_normal::Levels)> {
        let results: Vec<Result<(String, Vec<_>, Vec<_>)>> = std::thread::scope(|scope| {
            let handles: Vec<_> = levels
                .iter()
                .map(|level| scope.spawn(move || Ok((level.to_string(), self.records(before, level)?, self.records(after, level)?))))
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
}

impl BodyOracle for NormalOracle {
    fn compare(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String]) -> Result<Vec<BodyComparison>> {
        // N0 first. N2 and N3 are only necessary when a body is still different at N0.
        let (mut before, mut after) = self.levels(&step.tree_before, &step.tree_after, &["n0"])?;
        let mut comparisons = semilla_normal::compare(&before, &after, renames, added);
        if comparisons.iter().any(|c| !matches!(c.status, semilla_normal::BodyStatus::Equal { .. })) {
            let (old, new) = self.levels(&step.tree_before, &step.tree_after, &["n2", "n3"])?;
            before.extend(old);
            after.extend(new);
            let started = std::time::Instant::now();
            comparisons = semilla_normal::compare(&before, &after, renames, added);
            crate::timing("compare the bodies (N0 to N4)", started);
        }
        // The span of each body: from the "after" records first (the name is the "after" name).
        let mut spans: BTreeMap<&str, (String, usize)> = BTreeMap::new();
        for record in [&before, &after].iter().flat_map(|levels| levels.values().flatten()) {
            if let Some(span) = &record.span {
                spans.insert(&record.name, (span.file.clone(), span.line_start));
            }
        }
        let span_of = |name: &str| spans.get(name).cloned();
        Ok(comparisons
            .into_iter()
            .map(|comparison| BodyComparison {
                span: span_of(&comparison.name),
                status: match comparison.status {
                    semilla_normal::BodyStatus::Equal { level, proof } => BodyStatus::Equal { level, proof },
                    semilla_normal::BodyStatus::Different { cfg_changed } => BodyStatus::Different { cfg_changed },
                    semilla_normal::BodyStatus::BeforeOnly => BodyStatus::BeforeOnly,
                    semilla_normal::BodyStatus::AfterOnly => BodyStatus::AfterOnly,
                },
                name: comparison.name,
            })
            .collect())
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
