//! The adapters from the verifier traits to the real normal forms and the real SMT solver.

use std::collections::BTreeMap;

use anyhow::Result;
use semilla_core::Step;
use semilla_core::report::Effect;
use semilla_verify::{BodyComparison, BodyOracle, BodyStatus, EffectCheck, ExprOracle};

use crate::git::Repo;

/// The MIR levels that the oracle builds for each tree.
const LEVELS: [&str; 3] = ["n0", "n2", "n3"];

/// Runs the MIR driver on the snapshots, and compares the bodies with the normal forms.
pub struct NormalOracle {
    pub repo: Repo,
}

impl NormalOracle {
    /// Builds (once) the MIR records of a tree at all levels.
    pub fn levels(&self, tree: &str) -> Result<semilla_normal::Levels> {
        let dir = self.repo.materialize(tree)?;
        let mut levels = semilla_normal::Levels::default();
        for level in LEVELS {
            let out = self.repo.state_dir().join("cache/mir").join(tree).join(level);
            let done = out.join(".complete");
            if !done.exists() {
                let _ = std::fs::remove_dir_all(&out);
                std::fs::create_dir_all(&out)?;
                let target = self.repo.state_dir().join("cache").join(format!("target-{level}"));
                // Cargo writes a `Cargo.lock` when the tree has none. Remove it again, so that the
                // cached folder stays equal to the git tree.
                let lock = dir.join("Cargo.lock");
                let had_lock = lock.exists();
                let result = semilla_normal::run_cargo_check(&dir, level, &out, &target);
                if !had_lock {
                    let _ = std::fs::remove_file(&lock);
                }
                result?;
                std::fs::write(&done, "")?;
            }
            levels.insert(level.to_string(), semilla_normal::read_records(&out)?);
        }
        Ok(levels)
    }
}

impl BodyOracle for NormalOracle {
    fn compare(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String]) -> Result<Vec<BodyComparison>> {
        let before = self.levels(&step.tree_before)?;
        let after = self.levels(&step.tree_after)?;
        let comparisons = semilla_normal::compare(&before, &after, renames, added);
        // The span of a body: from the "after" records first (the name is the "after" name).
        let span_of = |name: &str| {
            [&after, &before]
                .iter()
                .flat_map(|levels| levels.values().flatten())
                .find(|record| record.name == name)
                .and_then(|record| record.span.as_ref().map(|span| (span.file.clone(), span.line_start)))
        };
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
