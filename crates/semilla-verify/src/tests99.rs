//! Design 99: the check of a change set with shared workspaces (R99.6-1).

use std::collections::BTreeMap;
use std::sync::Arc;

use semilla_core::{ChangeSet, Step};
use semilla_syntax::Workspace;

use crate::*;

const OLD: &str = "pub fn f(a: u32) -> u32 {\n    a + 1\n}\n";
const NEW: &str = "pub fn f(a: u32) -> u32 {\n    a + 2\n}\n";

/// An oracle that gives an empty name record for each tree: the check of a step puts it into
/// the name cell of its two workspaces (design 70).
struct WithNames;

impl BodyOracle for WithNames {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(Vec::new())
    }

    fn names(&self, _: &Step) -> NameState {
        let empty = || Arc::new(semilla_core::namerec::NameIndex::new(std::iter::empty()));
        NameState::Records([empty(), empty()])
    }
}

/// A cache of the step results by the index of the step.
#[derive(Default)]
struct Kept(BTreeMap<usize, StepOutcome>);

impl StepCache for Kept {
    fn get(&mut self, step: &Step) -> Option<StepOutcome> {
        self.0.get(&step.index).cloned()
    }

    fn put(&mut self, step: &Step, outcome: &StepOutcome) {
        self.0.insert(step.index, outcome.clone());
    }
}

fn change_set() -> ChangeSet {
    let step = Step { index: 0, intent: "test".into(), claims: Vec::new(), tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default(), syntax_only: false, build_check: None, agent: Default::default(), tags: Vec::new(), configurations: None };
    ChangeSet { base_tree: "a".into(), head_tree: "b".into(), steps: vec![step], ..Default::default() }
}

fn trees() -> (tempfile::TempDir, tempfile::TempDir, BTreeMap<String, Arc<Workspace>>) {
    let (one, old) = crate::tests::load(OLD);
    let (two, new) = crate::tests::load(NEW);
    (one, two, BTreeMap::from([("a".to_string(), Arc::new(old)), ("b".to_string(), Arc::new(new))]))
}

// [R99.6-1] The check with shared workspaces gives the result of the check with a copy of each
// workspace, also for a step whose result comes from the cache.
// rule+ R99.6-1
#[test]
fn the_check_with_shared_workspaces_gives_the_result_of_the_check_with_copies() {
    let (_one, _two, trees) = trees();
    let set = change_set();
    let options = Options { require_groups: false, derive_edits: true };
    let (copied, _) = verify_changeset(&set, &mut |tree| Ok((*trees[tree]).clone()), &WithNames, &NoOracle, options);
    let mut kept = Kept::default();
    let (shared, _) = verify_changeset_shared(&set, &mut |tree| Ok(trees[tree].clone()), &WithNames, &NoOracle, options, &mut kept);
    assert_eq!(format!("{shared:?}"), format!("{copied:?}"));
    assert_eq!(kept.0.len(), 1);
    // The second check takes the result from the cache and reads the shared workspaces.
    let (again, _) = verify_changeset_shared(&set, &mut |tree| Ok(trees[tree].clone()), &WithNames, &NoOracle, options, &mut kept);
    assert_eq!(format!("{again:?}"), format!("{copied:?}"));
}

// [R99.6-1] The check of a step puts the name records into its own copy: the shared workspaces
// keep an empty name cell, as a new copy has.
// rule- R99.6-1
#[test]
fn the_check_of_a_step_puts_its_name_records_into_its_own_copy() {
    let (_one, _two, trees) = trees();
    let set = change_set();
    let options = Options { require_groups: false, derive_edits: true };
    // The check sets the names of the workspaces that it gets (the control of the test).
    let (before, after) = ((*trees["a"]).clone(), (*trees["b"]).clone());
    let _ = verify_step_with(&StepInput { step: &set.steps[0], before: &before, after: &after }, &WithNames, &NoOracle, true);
    assert!(before.names.get().is_some() && after.names.get().is_some());
    let _ = verify_changeset_shared(&set, &mut |tree| Ok(trees[tree].clone()), &WithNames, &NoOracle, options, &mut NoStepCache);
    assert!(trees["a"].names.get().is_none() && trees["b"].names.get().is_none());
}
