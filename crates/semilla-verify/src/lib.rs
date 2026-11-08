//! The semilla verifier (the trusted core).
//!
//! The verifier checks each step on its own, against the two snapshots around it:
//! 1. It builds the claimed renames, and computes the syntax edits after the renames.
//! 2. It checks each claim against the edits (predict, match the shape, or mask).
//! 3. It checks the bodies with the normal forms (through a [`BodyOracle`]), and the condition
//!    effects with SMT (through an [`ExprOracle`]).
//! 4. It assigns each edit to one claim. An edit without a claim is uncovered. A claim without an
//!    edit is empty.
//! 5. It finds indirect changes: bodies whose MIR changed when their text did not.
//!
//! The change set check adds the group and subgroup rules, and the score.

pub mod checks;
pub mod hints;
pub mod organize;
pub mod pattern;
pub mod views;

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::report::Effect;
use semilla_core::{Anchor, ChangeSet, Class, ClaimResult, ClaimStatus, Report, Step, StepReport, Uncovered, View};
use semilla_syntax::{Edit, Workspace};

/// The MIR status of one body (design section 8.2).
#[derive(Clone, Debug, PartialEq)]
pub enum BodyStatus {
    /// Equal at a normal form level (`n0` to `n4`). `proof` holds the rule chain for N4.
    Equal { level: String, proof: Vec<String> },
    /// Different at every level.
    Different { cfg_changed: bool },
    BeforeOnly,
    AfterOnly,
}

/// The comparison of one MIR body between the two snapshots of a step.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyComparison {
    /// The MIR def path (after the renames), for example `demo::cart::Cart::total`.
    pub name: String,
    /// The file and the first line of the body on the "after" side (else the "before" side).
    pub span: Option<(String, usize)>,
    pub status: BodyStatus,
}

/// The access to the normal forms. The real one runs the MIR driver on both snapshots.
pub trait BodyOracle {
    /// Compares all bodies of the two snapshots of a step.
    ///
    /// `renames` maps item paths (before to after) from the identity claims. `added` lists the
    /// new bodies that claims declare, for example the new function of `extract-function`.
    fn compare(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String]) -> anyhow::Result<Vec<BodyComparison>>;
}

/// The result of an SMT check on a condition.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectCheck {
    pub effect: Effect,
    pub witness: Option<String>,
}

/// The access to the SMT checks.
pub trait ExprOracle {
    /// Classifies the change of a condition. `types` maps variable names to Rust type text.
    fn classify(&self, before: &str, after: &str, types: &BTreeMap<String, String>) -> Option<EffectCheck>;
    /// Returns `Some(true)` if the two expressions are proved equivalent.
    fn equivalent(&self, before: &str, after: &str, types: &BTreeMap<String, String>) -> Option<bool>;
}

/// An oracle that proves nothing. Every check gives "not proved".
pub struct NoOracle;

impl BodyOracle for NoOracle {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(Vec::new())
    }
}

impl ExprOracle for NoOracle {
    fn classify(&self, _: &str, _: &str, _: &BTreeMap<String, String>) -> Option<EffectCheck> {
        None
    }
    fn equivalent(&self, _: &str, _: &str, _: &BTreeMap<String, String>) -> Option<bool> {
        None
    }
}

/// The inputs of one step check.
pub struct StepInput<'a> {
    pub step: &'a Step,
    pub before: &'a Workspace,
    pub after: &'a Workspace,
}

/// Everything the verifier computed for one step. The views use it too.
pub struct StepOutcome {
    pub report: StepReport,
    pub edits: Vec<Edit>,
    /// The index of the claim that explains each edit, or `None` for an uncovered edit.
    pub owners: Vec<Option<usize>>,
    pub renames: semilla_syntax::Renames,
    pub bodies: Vec<BodyComparison>,
}

/// Checks one step.
pub fn verify_step(input: &StepInput, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle) -> StepOutcome {
    let step = input.step;
    let mut report = StepReport { step: step.index, intent: step.intent.clone(), declared_class: step.class, ..Default::default() };

    // 1. The renames from the identity claims, then the syntax edits.
    let renames = checks::renames(step, input.before);
    let edits = semilla_syntax::diff(input.before, input.after, &renames);

    // 2. The MIR bodies.
    let added: Vec<String> = step.claims.iter().flat_map(checks::declared_new_items).collect();
    let bodies = match bodies.compare(step, &renames.items, &added) {
        Ok(bodies) => bodies,
        Err(error) => {
            report.errors.push(format!("the body check failed: {error:#}"));
            Vec::new()
        }
    };
    let body_index = checks::BodyIndex::new(&bodies, input.after, input.before);

    // 3. Each claim.
    let context = checks::Context { step, before: input.before, after: input.after, edits: &edits, bodies: &body_index, exprs, renames: &renames };
    let mut owners: Vec<Option<usize>> = vec![None; edits.len()];
    let mut results = Vec::new();
    for (index, claim) in step.claims.iter().enumerate() {
        let outcome = checks::check(&context, claim);
        for &edit in &outcome.explains {
            if owners[edit].is_none() {
                owners[edit] = Some(index);
            }
        }
        results.push((claim, outcome));
    }

    // A neutral claim must have a body proof for its whole function. Other non-neutral claims in
    // the same function make that impossible.
    let non_neutral_items: BTreeSet<String> = results
        .iter()
        .filter(|(_, outcome)| outcome.class.is_some_and(|class| class != Class::Neutral))
        .filter_map(|(claim, _)| claim.at.as_ref().map(|anchor| anchor.item.clone()))
        .collect();

    for (claim, outcome) in &results {
        let mut status = outcome.status.clone();
        let mut hint = outcome.hint.clone();
        if outcome.class == Some(Class::Neutral) && outcome.needs_body_proof {
            if let Some(item) = claim.at.as_ref().map(|anchor| anchor.item.as_str()) {
                if non_neutral_items.contains(item) && status.is_verified() {
                    status = ClaimStatus::Unproved { reason: "other claims change the same function in this step".into() };
                    hint = Some("Put the neutral claims of this function in their own step.".into());
                }
            }
        }
        if status.is_verified() && outcome.explains.is_empty() && !outcome.self_evident {
            status = ClaimStatus::Empty;
        }
        report.claims.push(ClaimResult {
            claim: claim.id.clone(),
            step: step.index,
            status,
            class: outcome.class,
            effect: outcome.effect,
            witness: outcome.witness.clone(),
            hint,
            before: outcome.before.clone(),
            after: outcome.after.clone(),
        });
    }

    // 4. Coverage of the syntax edits.
    for (edit, owner) in edits.iter().zip(&owners) {
        if owner.is_none() {
            report.uncovered.push(Uncovered {
                step: step.index,
                view: edit.view,
                // Claims on existing code use the "before" numbering; new statements use "after".
                at: if edit.kind == semilla_syntax::EditKind::Inserted { edit.after.clone() } else { edit.before.clone().or(edit.after.clone()) },
                file: edit.file.clone(),
                description: hints::describe(edit),
                hint: Some(hints::suggest(edit).to_string()),
            });
        }
    }

    // 5. Indirect changes: a body that changed without a text change in its item.
    let edited_items: BTreeSet<&str> = edits.iter().filter_map(|edit| edit.item()).collect();
    let affects: BTreeSet<String> = step.claims.iter().flat_map(|claim| claim.arg_list("affects")).collect();
    for comparison in &bodies {
        if !matches!(comparison.status, BodyStatus::Different { .. }) {
            continue;
        }
        let item = body_index.item_of(comparison);
        let item_name = item.clone().unwrap_or_else(|| comparison.name.clone());
        if edited_items.contains(item_name.as_str()) || affects.contains(&item_name) || affects.contains(&comparison.name) {
            continue;
        }
        if item.as_ref().is_some_and(|item| renames.items.values().any(|new| new == item)) {
            continue;
        }
        report.uncovered.push(Uncovered {
            step: step.index,
            view: View::Body,
            at: Some(Anchor::item(&item_name)),
            file: comparison.span.as_ref().map(|(file, _)| file.clone()),
            description: format!("the MIR of `{item_name}` changed, but its text did not (an indirect change)"),
            hint: Some(format!("Add `{item_name}` to the `affects` list of the claim that causes the change.")),
        });
    }

    report.computed_class = report.claims.iter().filter_map(|claim| claim.class).max_by_key(|class| class_rank(*class));
    if let (Some(declared), Some(computed)) = (step.class, report.computed_class) {
        if declared != computed {
            report.errors.push(format!(
                "the step declares the class `{}`, but the claims compute `{}`",
                class_name(declared),
                class_name(computed)
            ));
        }
    }

    StepOutcome { report, edits, owners, renames, bodies }
}

/// The rank of a class: a step has the highest class of its claims.
pub fn class_rank(class: Class) -> u8 {
    match class {
        Class::Neutral => 0,
        Class::Other => 1,
        Class::Logic => 2,
        Class::ControlFlow => 3,
    }
}

pub fn class_name(class: Class) -> &'static str {
    match class {
        Class::Neutral => "neutral",
        Class::Logic => "logic",
        Class::ControlFlow => "control_flow",
        Class::Other => "other",
    }
}

/// The options of a change set check.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Require groups (the final check before a review). Without it, missing groups are a warning.
    pub require_groups: bool,
}

/// Checks a full change set. `load` gives the workspace of a git tree.
pub fn verify_changeset(
    set: &ChangeSet,
    load: &mut dyn FnMut(&str) -> anyhow::Result<Workspace>,
    bodies: &dyn BodyOracle,
    exprs: &dyn ExprOracle,
    options: Options,
) -> (Report, Vec<StepOutcome>) {
    let mut report = Report::default();
    let mut outcomes = Vec::new();
    let mut previous_tree = set.base_tree.clone();
    for step in &set.steps {
        if step.tree_before != previous_tree {
            report.organization.push(format!("step {} does not start at the tree where the previous step ends", step.index));
        }
        previous_tree = step.tree_after.clone();
        let loaded = load(&step.tree_before).and_then(|before| Ok((before, load(&step.tree_after)?)));
        match loaded {
            Ok((before, after)) => {
                let outcome = verify_step(&StepInput { step, before: &before, after: &after }, bodies, exprs);
                outcomes.push(outcome);
            }
            Err(error) => {
                let step_report = StepReport { step: step.index, intent: step.intent.clone(), errors: vec![format!("{error:#}")], ..Default::default() };
                outcomes.push(StepOutcome { report: step_report, edits: Vec::new(), owners: Vec::new(), renames: Default::default(), bodies: Vec::new() });
            }
        }
    }
    if !set.steps.is_empty() && previous_tree != set.head_tree && !set.head_tree.is_empty() {
        report.organization.push("the last step does not end at the head tree".to_string());
    }
    report.steps = outcomes.iter().map(|outcome| outcome.report.clone()).collect();
    organize::check(set, &report.steps.clone(), &mut report, options);
    report.finish();
    (report, outcomes)
}

#[cfg(test)]
mod tests;
