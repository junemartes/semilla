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

/// The MIR facts of one body after a step, for the property claims.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BodyFacts {
    pub name: String,
    pub span: Option<(String, usize)>,
    /// The def paths of the local functions that the body calls.
    pub callees: Vec<String>,
    /// The def paths of the functions of other crates that the body calls.
    pub externals: Vec<String>,
}

/// The MIR facts of one item (all its bodies, with its closures).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemFacts {
    /// The items that the bodies call.
    pub callees: Vec<String>,
    pub externals: Vec<String>,
}

/// The access to the normal forms. The real one runs the MIR driver on both snapshots.
pub trait BodyOracle {
    /// Compares all bodies of the two snapshots of a step.
    ///
    /// `renames` maps item paths (before to after) from the identity claims. `added` lists the
    /// new bodies that claims declare, for example the new function of `extract-function`.
    fn compare(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String]) -> anyhow::Result<Vec<BodyComparison>>;

    /// The MIR facts of the bodies after a step. `None` when the MIR is not available (phase 1).
    fn facts(&self, _step: &Step) -> anyhow::Result<Option<Vec<BodyFacts>>> {
        Ok(None)
    }
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
    let oracle = bodies;
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

    // The data of the property claims: the local calls (syntax), and the MIR facts (phase 2).
    let properties: Vec<&semilla_core::Claim> = step.claims.iter().filter(|c| semilla_core::ops::op(&c.op).is_some_and(|i| i.kind == semilla_core::OpKind::Property)).collect();
    let mut calls: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if !properties.is_empty() {
        let graph = semilla_syntax::graph::workspace_graph(input.after, &BTreeMap::new(), &BTreeMap::new());
        for edge in graph.edges.iter().filter(|e| e.kind == semilla_core::view::GraphEdgeKind::Call) {
            calls.entry(edge.from.clone()).or_default().insert(edge.to.clone());
        }
    }
    let mut facts: Option<BTreeMap<String, ItemFacts>> = None;
    if properties.iter().any(|c| matches!(c.op.as_str(), "no-io" | "calls-only")) {
        match oracle.facts(step) {
            Ok(Some(found)) => facts = Some(item_facts(&found, input.after)),
            Ok(None) => {}
            Err(error) => report.errors.push(format!("the MIR facts failed: {error:#}")),
        }
    }

    // 3. Each claim.
    let context = checks::Context { step, before: input.before, after: input.after, edits: &edits, bodies: &body_index, exprs, renames: &renames, calls: &calls, facts: facts.as_ref() };
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

    // An empty claim near an uncovered change: tell the agent where the change is.
    for result in report.claims.iter_mut().filter(|r| r.status == ClaimStatus::Empty) {
        let Some(at) = step.claims.iter().find(|c| c.id == result.claim).and_then(|c| c.at.clone()) else { continue };
        let nearest = edits
            .iter()
            .zip(&owners)
            .filter(|(_, owner)| owner.is_none())
            .filter_map(|(edit, _)| edit.before.clone().or(edit.after.clone()))
            .filter(|anchor| anchor.item == at.item)
            .min_by_key(|anchor| {
                let common = anchor.path.iter().zip(&at.path).take_while(|(a, b)| a == b).count();
                (anchor.path.len() + at.path.len()) - 2 * common
            });
        if let Some(anchor) = nearest {
            result.hint = Some(format!("The change is at `{anchor}`: move the claim there."));
        }
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

    // An unclaimed rename shows as a removed item and an added item of the same kind, with the
    // same text after the name change. The hint says so (the claim is still necessary).
    for (old_name, new_name) in hints::likely_renames(&edits, input.before, input.after) {
        for uncovered in report.uncovered.iter_mut() {
            let item = uncovered.at.as_ref().map(|a| a.item.as_str());
            if item == Some(old_name.as_str()) || item == Some(new_name.as_str()) {
                uncovered.hint = Some(format!("rename (it looks like a rename of `{old_name}` to `{new_name}`)"));
            }
        }
    }

    for (item, old, new) in hints::likely_local_renames(&edits, input.before, input.after) {
        for uncovered in report.uncovered.iter_mut().filter(|u| u.at.as_ref().is_some_and(|a| a.item == item)) {
            uncovered.hint = Some(format!("rename-local (it looks like a rename of `{old}` to `{new}`: anchor `{item}`, from={old}, to={new})"));
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
        // rustc generates a `main` for a test build. No source item has it.
        if item.is_none() && is_test_harness_main(&comparison.name, input.after) {
            continue;
        }
        let item_name = item.clone().unwrap_or_else(|| comparison.name.clone());
        let affected = |name: &str| {
            affects.iter().any(|entry| entry == name || entry.strip_suffix("::*").is_some_and(|module| name.starts_with(&format!("{module}::"))))
        };
        if edited_items.contains(item_name.as_str()) || affected(&item_name) || affected(&comparison.name) {
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
            hint: Some(format!("`{item_name}` to the `affects` list of the claim that causes the change")),
        });
    }

    report.panic_delta = panic_sites(input.after) - panic_sites(input.before);
    report.spotlight = spotlight(&edits, input.before, input.after);
    for claim in step.claims.iter().filter(|claim| claim.op == "replace-macro-input") {
        if let Some(at) = &claim.at {
            report.spotlight.push(format!("macro input: {at}"));
        }
    }
    for claim in step.claims.iter().filter(|claim| claim.op == "rewrite-item") {
        if let Some(at) = &claim.at {
            report.spotlight.push(format!("rewritten item: {at} ({})", claim.arg_str("note").unwrap_or("")));
        }
    }
    report.lines = line_counts(step, &edits, &owners);
    let (level, api_changes) = api_level(&edits, input.before, input.after);
    report.api_level = Some(level.to_string());
    report.api_changes = api_changes;

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

/// Maps the MIR facts of the bodies to the items (a closure belongs to its function).
fn item_facts(found: &[BodyFacts], after: &Workspace) -> BTreeMap<String, ItemFacts> {
    let names: BTreeMap<&str, String> = found.iter().filter_map(|f| Some((f.name.as_str(), checks::find_item_by(&f.name, f.span.as_ref(), after)?))).collect();
    let mut facts: BTreeMap<String, ItemFacts> = BTreeMap::new();
    for body in found {
        let Some(item) = names.get(body.name.as_str()) else { continue };
        let entry = facts.entry(item.clone()).or_default();
        entry.externals.extend(body.externals.iter().cloned());
        for callee in &body.callees {
            let target = names.get(callee.as_str()).cloned().or_else(|| after.items.contains_key(callee.as_str()).then(|| callee.clone()));
            if let Some(target) = target.filter(|t| t != item) {
                entry.callees.push(target);
            }
        }
    }
    facts
}

/// The changed lines of a step, by the kind of claim that explains them.
pub fn line_counts(step: &Step, edits: &[Edit], owners: &[Option<usize>]) -> semilla_core::LineCounts {
    let mut counts = semilla_core::LineCounts::default();
    for (edit, owner) in edits.iter().zip(owners) {
        let Some(claim) = owner.and_then(|index| step.claims.get(index)) else { continue };
        // The member list of an impl repeats a new or removed method: count the method only.
        if edit.kind == semilla_syntax::EditKind::Members && matches!(claim.op.as_str(), "add-item" | "remove-item" | "rewrite-item") {
            continue;
        }
        let lines = |text: &Option<String>| text.as_deref().map(|t| t.lines().filter(|l| !l.trim().is_empty()).count()).unwrap_or(0);
        let count = lines(&edit.source_before).max(lines(&edit.source_after)).max(1);
        match claim.op.as_str() {
            "add-item" => counts.new += count,
            "remove-item" => counts.removed += count,
            "rewrite-item" => counts.rewritten += count,
            _ => counts.fine += count,
        }
    }
    counts
}

/// The pair threshold: a removed item and an added item with this similarity or more look like
/// one changed item.
pub const PAIR_THRESHOLD: f64 = 0.7;

/// Finds the pair warnings of a change set: a verified `remove-item` and a verified `add-item` of
/// the same kind, with similar text. `items` gives the removed item (in the tree before its step)
/// and the added item (in the tree after its step) of each claim.
pub fn pair_warnings(set: &ChangeSet, report: &Report, items: &BTreeMap<String, semilla_syntax::Item>) -> Vec<semilla_core::PairWarning> {
    let verified = |id: &str| report.steps.iter().flat_map(|s| &s.claims).any(|c| c.claim == id && c.status.is_verified());
    let claims: Vec<&semilla_core::Claim> = set.steps.iter().flat_map(|s| &s.claims).filter(|c| verified(&c.id)).collect();
    // All candidates, then a greedy match: each item is in at most one pair, the most similar first.
    let mut candidates = Vec::new();
    for removed in claims.iter().filter(|c| c.op == "remove-item") {
        let Some(old) = items.get(&removed.id) else { continue };
        for added in claims.iter().filter(|c| c.op == "add-item") {
            let Some(new) = items.get(&added.id) else { continue };
            if old.kind != new.kind || old.path == new.path {
                continue;
            }
            let similarity = hints::similarity(old, new);
            if similarity >= PAIR_THRESHOLD {
                candidates.push((similarity, old, new, removed.id.clone(), added.id.clone()));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut used = BTreeSet::new();
    let mut warnings = Vec::new();
    for (similarity, old, new, removed, added) in candidates {
        if used.contains(&old.path) || used.contains(&new.path) {
            continue;
        }
        used.insert(old.path.clone());
        used.insert(new.path.clone());
        let refusal = set.pair_refusals.iter().find(|r| r.old == old.path && r.new == new.path).map(|r| r.reason.clone());
        warnings.push(semilla_core::PairWarning { old: old.path.clone(), new: new.path.clone(), similarity, claims: vec![removed, added], refusal, reviewer: None });
    }
    warnings.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
    warnings
}

/// True for `<crate>::main` when the crate has no `main` function in the source: the `main` that
/// rustc generates for a test build.
fn is_test_harness_main(name: &str, workspace: &Workspace) -> bool {
    let Some((krate, "main")) = name.split_once("::") else { return false };
    !workspace.items.contains_key(&format!("{krate}::main"))
}

/// Counts the explicit panic sites in the non-test functions of a workspace (design 5.5):
/// `unwrap` and `expect` calls, `panic!`, `unreachable!`, `todo!`, `unimplemented!`, and
/// indexing. The implicit checks (arithmetic overflow) do not count.
pub fn panic_sites(workspace: &Workspace) -> i64 {
    let mut count = 0;
    for item in workspace.items.values().filter(|item| !item.test) {
        let Some(body) = &item.body else { continue };
        let mut path = Vec::new();
        body.walk(&mut path, &mut |_, node| {
            let attr = node.attr.as_deref().unwrap_or("");
            let site = match node.kind {
                "method_call" => matches!(attr, "unwrap" | "expect"),
                "macro" => matches!(attr, "panic" | "unreachable" | "todo" | "unimplemented"),
                "index" => true,
                _ => false,
            };
            if site {
                count += 1;
            }
        });
    }
    count
}

/// True if an item and all its parent modules are `pub` (the item is in the public API).
fn is_public(workspace: &Workspace, path: &str) -> bool {
    let Some(item) = workspace.items.get(path) else { return false };
    if item.vis != "pub" || item.test {
        return false;
    }
    // A `pub use` can re-export an item (or the owner type of a method) from a private module.
    let reexported = |name: &str| {
        workspace.items.values().any(|use_item| {
            use_item.kind == semilla_syntax::ItemKind::Use && use_item.vis == "pub" && use_item.signature.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| word == name)
        })
    };
    let segments: Vec<&str> = path.split("::").collect();
    if segments.len() >= 2 && (reexported(segments[segments.len() - 1]) || reexported(segments[segments.len() - 2])) {
        return true;
    }
    // Methods: the owner type is public. Items: each parent module is public.
    let mut current = path;
    while let Some((parent, _)) = current.rsplit_once("::") {
        if let Some(owner) = workspace.items.get(parent) {
            if owner.vis != "pub" && owner.kind != semilla_syntax::ItemKind::Impl {
                return false;
            }
        }
        current = parent;
    }
    true
}

/// Estimates the semver level of the public API change of a step, from the syntax edits.
///
/// - `major`: a public item is removed, or its signature or visibility changes.
/// - `minor`: a public item is added, or a public struct or enum gets a new member.
/// - `patch`: no change to the public API.
pub fn api_level(edits: &[Edit], before: &Workspace, after: &Workspace) -> (&'static str, Vec<String>) {
    use semilla_syntax::EditKind;
    let mut major = Vec::new();
    let mut minor = Vec::new();
    for edit in edits {
        let old = edit.before.as_ref().map(|a| a.item.as_str()).filter(|item| is_public(before, item));
        let new = edit.after.as_ref().map(|a| a.item.as_str()).filter(|item| is_public(after, item));
        let path_only = edit.anchor().is_some_and(|a| a.path.is_empty());
        if !path_only {
            continue;
        }
        match edit.kind {
            EditKind::ItemRemoved if old.is_some() => major.push(format!("removed `{}`", old.unwrap())),
            EditKind::ItemAdded if new.is_some() => minor.push(format!("added `{}`", new.unwrap())),
            EditKind::Signature if old.is_some() || new.is_some() => major.push(format!("signature of `{}`", old.or(new).unwrap())),
            EditKind::Visibility if old.is_some() => major.push(format!("visibility of `{}`", old.unwrap())),
            EditKind::Visibility if new.is_some() => minor.push(format!("`{}` is now public", new.unwrap())),
            EditKind::Members if old.is_some() => {
                let old_members: Vec<&str> = edit.text_before.as_deref().unwrap_or("").lines().collect();
                let new_members: Vec<&str> = edit.text_after.as_deref().unwrap_or("").lines().collect();
                if old_members.iter().all(|m| new_members.contains(m)) {
                    minor.push(format!("new members in `{}`", old.unwrap()));
                } else {
                    major.push(format!("changed members of `{}`", old.unwrap()));
                }
            }
            _ => {}
        }
    }
    let level = if !major.is_empty() { "major" } else if !minor.is_empty() { "minor" } else { "patch" };
    major.extend(minor);
    (level, major)
}

/// The edits that always need the reviewer's attention (design 5.5).
pub fn spotlight(edits: &[Edit], before: &Workspace, after: &Workspace) -> Vec<String> {
    let mut found = BTreeSet::new();
    for edit in edits {
        let place = edit.anchor().map(|a| a.to_string()).or_else(|| edit.file.clone()).unwrap_or_default();
        let texts = [edit.text_before.as_deref(), edit.text_after.as_deref()];
        if texts.iter().flatten().any(|text| text.split(' ').any(|token| token == "unsafe")) || in_unsafe(edit, before, after) {
            found.insert(format!("unsafe code: {place}"));
        }
        // `#[cfg(test)]` only selects test code, so it is not in the spotlight.
        let cfg = |text: &str| text.contains("cfg") && text.replace(' ', "").replace("#[cfg(test)]", "").contains("cfg");
        if texts.iter().flatten().any(|text| cfg(text)) && matches!(edit.kind, semilla_syntax::EditKind::Attrs | semilla_syntax::EditKind::ItemAdded | semilla_syntax::EditKind::ItemRemoved) {
            found.insert(format!("cfg attribute: {place}"));
        }
        if let Some(file) = &edit.file {
            if file.ends_with("build.rs") {
                found.insert(format!("build script: {file}"));
            }
            if file.ends_with("Cargo.toml") {
                let old = before.other_files.get(file).map(String::as_str).unwrap_or("");
                let new = after.other_files.get(file).map(String::as_str).unwrap_or("");
                let features = |text: &str| text.split("[features]").nth(1).map(|rest| rest.split("\n[").next().unwrap_or("").to_string());
                if features(old) != features(new) {
                    found.insert(format!("features: {file}"));
                }
            }
        }
    }
    found.into_iter().collect()
}

/// True if the edit is inside an `unsafe` block of a function body.
fn in_unsafe(edit: &Edit, before: &Workspace, after: &Workspace) -> bool {
    let check = |anchor: Option<&Anchor>, workspace: &Workspace| {
        let Some(anchor) = anchor else { return false };
        let mut current = anchor.clone();
        while let Some(parent) = current.parent() {
            if semilla_syntax::resolve_node(workspace, &parent).is_some_and(|node| node.kind == "unsafe") {
                return true;
            }
            current = parent;
        }
        false
    };
    check(edit.before.as_ref(), before) || check(edit.after.as_ref(), after)
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
    let mut pair_items: BTreeMap<String, semilla_syntax::Item> = BTreeMap::new();
    let mut previous_tree = set.base_tree.clone();
    for step in &set.steps {
        if step.tree_before != previous_tree {
            report.organization.push(format!("step {} does not start at the tree where the previous step ends", step.index));
        }
        previous_tree = step.tree_after.clone();
        let loaded = load(&step.tree_before).and_then(|before| Ok((before, load(&step.tree_after)?)));
        match loaded {
            Ok((before, after)) => {
                for claim in &step.claims {
                    let found = match (claim.op.as_str(), &claim.at) {
                        ("remove-item", Some(at)) => before.items.get(&at.item),
                        ("add-item", Some(at)) => after.items.get(&at.item),
                        _ => None,
                    };
                    if let Some(item) = found {
                        pair_items.insert(claim.id.clone(), item.clone());
                    }
                }
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
        // A difference in formatting only is allowed: the syntax diff ignores whitespace, but not
        // comments.
        let same = load(&previous_tree).and_then(|last| Ok((last, load(&set.head_tree)?))).is_ok_and(|(last, head)| {
            semilla_syntax::diff(&last, &head, &Default::default()).is_empty()
        });
        if same {
            report.warnings.push("the head tree differs from the last step only in formatting".to_string());
        } else {
            report.organization.push("the last step does not end at the head tree".to_string());
        }
    }
    report.steps = outcomes.iter().map(|outcome| outcome.report.clone()).collect();
    for step in &report.steps {
        for item in &step.spotlight {
            report.warnings.push(format!("spotlight (step {}): {item}", step.step));
        }
    }
    organize::check(set, &report.steps.clone(), &mut report, options);
    report.finish();
    report.pairs = pair_warnings(set, &report, &pair_items);
    (report, outcomes)
}

#[cfg(test)]
mod tests;
