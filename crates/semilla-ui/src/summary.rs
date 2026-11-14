//! The change-set summary (design sections 11.2 and 11.3).
//!
//! - Groups: by risk, then by dependency. A group that adds an item comes before a group that
//!   uses it.
//! - Subgroups: by role order (`Role::order()`).
//! - Fold state: a subgroup with a claim is open (design 35.7). No result folds a claim.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use serde_json::Value;

use semilla_core::view::GraphNodeKind;
use semilla_core::{Category, Claim, ClaimResult, ClaimStatus, Compare, Group, GroupKind, Role, Uncovered, ops};

use crate::ctx::{Ctx, SubgroupRef, category_label, last_segment, role_label};
use crate::review::{ReviewFile, ReviewState};

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Counts {
    pub claims: usize,
    /// Claims that Semilla proved in full.
    pub verified: usize,
    /// Property claims that Semilla proved only in part (`verified (proof partial)`). The result
    /// line of `tag` counts them as "partial", and each report uses the same counts.
    pub partial: usize,
    pub refuted: usize,
    pub empty: usize,
    pub malformed: usize,
    /// Claims whose body proof waits for the background check (phase 2).
    pub pending: usize,
    /// Claims of a step that has no check: the step uses changed build code (design 28).
    pub not_checked: usize,
    /// Verified claims whose proof used changed build code (design 28, invariant I6), and the
    /// text for the count, for example ` (1 with the build code of step 1)`.
    #[serde(skip_serializing_if = "is_zero")]
    pub marked: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub marked_text: String,
    /// The same for the claims with a partial proof.
    #[serde(skip_serializing_if = "is_zero")]
    pub partial_marked: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub partial_marked_text: String,
    /// The steps of the marks.
    #[serde(skip)]
    pub mark_steps: Vec<usize>,
    /// Claims without a result in the report.
    pub no_result: usize,
    pub uncovered: usize,
    pub reviewed: usize,
}

/// The fact counts of the result line and of the summary table (design 35.6).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct FactCounts {
    pub total: usize,
    /// The count of each compare result, in the order of the result line. A count of zero is
    /// not in the list.
    pub compare: Vec<CountRow>,
    /// The count of each category, in the order of the summary table. A count of zero is not
    /// in the list.
    pub category: Vec<CountRow>,
    pub spotlight: usize,
    /// The facts with the mismatch mark (design 35.2).
    pub mismatch: usize,
    /// The facts with the result `code equal` and the mark of changed build code (design 28),
    /// and the text for the count: ` (1 with the build code of step 1)`, or an empty text.
    pub marked: usize,
    pub marked_text: String,
    /// The facts part of the result line, as the command line shows it.
    pub text: String,
}

/// One row of a count table: the name of a category or of a compare result, and its count.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct CountRow {
    pub name: &'static str,
    pub count: usize,
}

impl FactCounts {
    pub fn of(report: &semilla_core::Report) -> FactCounts {
        let counts = report.fact_counts();
        let marked = report.marked_count();
        FactCounts {
            total: counts.total,
            compare: counts.compare.iter().map(|(compare, count)| CountRow { name: compare.name(), count: *count }).collect(),
            category: counts.category.iter().map(|(category, count)| CountRow { name: category.name(), count: *count }).collect(),
            spotlight: counts.spotlight,
            mismatch: counts.mismatch,
            marked,
            marked_text: report.marked_text(marked),
            text: report.facts_text(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ChangeSetSummary {
    pub base_tree: String,
    pub head_tree: String,
    pub accepted: bool,
    /// True if a step has no check: it uses build code that the change set changes, and the
    /// command had `--no-build-code` (design 28). The result word is then `not complete`.
    pub not_complete: bool,
    /// The facts of all steps: the total, the count of each compare result and of each
    /// category, and the facts with a spotlight (design 35.6).
    pub fact_counts: FactCounts,
    /// `major`, `minor`, `patch`, or `none`. A hint only: the UI is not trusted.
    pub api_level: &'static str,
    /// The change in explicit panic sites from the verifier (`Report.panic_delta`).
    pub panic_delta: i64,
    pub group_count: usize,
    pub step_count: usize,
    pub counts: Counts,
    pub organization: Vec<String>,
    pub warnings: Vec<String>,
    /// The base commit of the change set and its Semilla check (design 25.2).
    #[serde(default)]
    pub notices: Vec<String>,
    /// The changed lines by claim kind, and the same as one line of text.
    pub lines: semilla_core::LineCounts,
    pub lines_text: String,
    /// The pair warnings, with the reviewer's current decision.
    pub pairs: Vec<PairView>,
    /// The derived facts of all steps (design 22.5), with their step.
    pub facts: Vec<FactView>,
    pub groups: Vec<GroupSummary>,
}

/// One derived fact for the review UI.
#[derive(Clone, Debug, Serialize)]
pub struct FactView {
    pub step: usize,
    #[serde(flatten)]
    pub fact: semilla_core::Fact,
    /// The mark of changed build code (design 28, I6) for the result `code equal` of the fact,
    /// with a space before it, or an empty text.
    pub mark: String,
    /// The same mark for a part of the fact that shows equal compiled code.
    pub part_mark: String,
    /// The text in the brackets of the fact: "rename · code equal" (design 35).
    pub label: String,
    /// True if the fact has the mismatch mark (design 35.2).
    pub mismatch: bool,
    /// The place of the fact in a list (design 35.7).
    pub order: u8,
}

impl FactView {
    pub fn new(step: usize, fact: &semilla_core::Fact) -> FactView {
        FactView { step, fact: fact.clone(), mark: fact.mark(), part_mark: fact.part_mark(), label: fact.label(), mismatch: fact.mismatch(), order: fact.order() }
    }
}

/// One pair warning for the review UI.
#[derive(Clone, Debug, Serialize)]
pub struct PairView {
    pub key: String,
    pub old: String,
    pub new: String,
    pub similarity: f64,
    pub claims: Vec<String>,
    pub refusal: Option<String>,
    /// `accepted`, `dismissed`, or `None`.
    pub reviewer: Option<String>,
    pub text: String,
}

/// The pair warnings of a report, with the decisions of the review file.
pub fn pair_views(report: &semilla_core::Report, review: &ReviewFile) -> Vec<PairView> {
    report
        .pairs
        .iter()
        .map(|pair| {
            let mut pair = pair.clone();
            pair.reviewer = review.pair(&pair.key()).map(str::to_string).or(pair.reviewer);
            PairView { key: pair.key(), text: pair.text(), old: pair.old, new: pair.new, similarity: pair.similarity, claims: pair.claims, refusal: pair.refusal, reviewer: pair.reviewer }
        })
        .collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupSummary {
    pub id: String,
    pub title: String,
    pub kind: GroupKind,
    pub why: String,
    pub steps: Vec<usize>,
    /// The risk level from 0 to 3 (the dots in the UI).
    pub risk: u8,
    pub risk_score: u32,
    pub risk_reasons: Vec<String>,
    /// The groups that this group uses. They come first in the order.
    pub depends_on: Vec<String>,
    /// True if each fact has the result `code equal` or `no code`, no fact has a spotlight,
    /// each claim is verified, and nothing is uncovered (design 35.7). It is a sum of the
    /// compare results. It is no verdict.
    pub all_equal: bool,
    /// The mark of changed build code for the text of `all_equal` (design 28, I6), with a
    /// space before it, or an empty text.
    pub mark: String,
    /// The same mark for the kind, if the kind comes from compare results (`refactor`,
    /// `rename`, `move`).
    pub kind_mark: String,
    /// The sum of `StepReport.panic_delta` over the steps of the group.
    pub panic_delta: i64,
    /// A warning if the group has parts with no link (design section 6.2).
    pub split_hint: Option<String>,
    pub synthetic: bool,
    pub reviewed: bool,
    pub counts: Counts,
    pub subgroups: Vec<SubgroupSummary>,
    pub uncovered: Vec<Uncovered>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SubgroupSummary {
    pub id: String,
    pub group: String,
    pub role: Role,
    pub role_label: &'static str,
    pub title: String,
    pub why: String,
    /// True if the agent changed the engine suggestion.
    pub edited: bool,
    /// True if the UI made this subgroup for claims without a subgroup.
    pub synthetic: bool,
    pub folded: bool,
    /// The cause.
    pub fold_reason: String,
    /// The locations of the claims: files, or modules when no file is known.
    pub locations: Vec<String>,
    /// The claims that the engine added after the agent's edit (design section 6.3, rule 5).
    pub new_claims: Vec<String>,
    /// The category badges of a custom subgroup, for example `2 control flow`.
    pub badges: Vec<String>,
    /// The number of claims for each category label.
    pub category_counts: BTreeMap<&'static str, usize>,
    /// A warning if the role label does not match the contents (design section 6.3, rule 1).
    pub label_mismatch: Option<String>,
    pub reviewed: bool,
    pub counts: Counts,
    pub claims: Vec<ClaimView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClaimView {
    pub id: String,
    pub op: String,
    pub at: Option<String>,
    pub args: serde_json::Map<String, Value>,
    pub step: Option<usize>,
    pub group: Option<String>,
    pub subgroup: Option<String>,
    /// A short text for the claim, from its op and arguments.
    pub summary: String,
    /// The category of the change of the claim (design 35). A property claim has none.
    pub category: Option<Category>,
    /// The role of the op in the rule table.
    pub op_role: Option<Role>,
    /// `verified`, `refuted`, `empty`, `malformed`, `pending`, or `no_result`.
    pub status: &'static str,
    /// The mark of changed build code (design 28, I6) for a verified claim, with a space
    /// before it, or an empty text.
    pub mark: String,
    pub result: Option<ClaimResult>,
    pub review: ReviewState,
    /// The function of the flow view that shows this claim.
    pub flow: Option<String>,
    /// True if the engine added the claim to its subgroup after the agent's edit.
    pub new_in_subgroup: bool,
    /// The file of the anchor item, if the graph knows it.
    pub file: Option<String>,
    /// True for a property claim (`no-panic`, `tested-by`, …): a checked fact, not a code change.
    pub property: bool,
}

/// Builds the change-set summary.
pub fn changeset(ctx: &Ctx, review: &ReviewFile) -> ChangeSetSummary {
    let groups = ordered_groups(ctx, review);
    let mut counts = Counts::default();
    for group in &groups {
        add_counts(&mut counts, &group.counts);
    }
    let report = &ctx.data.report;
    ChangeSetSummary {
        base_tree: ctx.data.changeset.base_tree.clone(),
        head_tree: ctx.data.changeset.head_tree.clone(),
        accepted: report.accepted,
        not_complete: report.not_complete(),
        fact_counts: FactCounts::of(report),
        api_level: api_level(ctx),
        panic_delta: report.panic_delta,
        group_count: ctx.groups.len(),
        step_count: ctx.data.changeset.steps.len(),
        counts,
        organization: report.organization.clone(),
        warnings: report.warnings.clone(),
        notices: report.notices.clone(),
        lines: report.lines,
        lines_text: report.lines_text().unwrap_or_default(),
        pairs: pair_views(report, review),
        // Design 28.3: the build code changes of a step come before its other facts.
        facts: report.steps.iter().flat_map(|step| step.facts.iter().filter(|fact| fact.build_code_change).chain(step.facts.iter().filter(|fact| !fact.build_code_change)).map(|fact| FactView::new(step.step, fact))).collect(),
        groups,
    }
}

/// The summary of one group, with the steps of the report.
pub fn group_detail(ctx: &Ctx, review: &ReviewFile, id: &str) -> Option<Value> {
    let group = ctx.group(id)?;
    let summary = ordered_groups(ctx, review).into_iter().find(|summary| summary.id == id)?;
    let steps: Vec<Value> = group
        .steps
        .iter()
        .map(|index| {
            let step = ctx.data.changeset.steps.iter().find(|step| step.index == *index);
            let report = ctx.data.report.steps.iter().find(|report| report.step == *index);
            serde_json::json!({
                "index": index,
                "intent": step.map(|step| step.intent.clone()).or_else(|| report.map(|report| report.intent.clone())),
                // Design 28, I6: the step whose build code change the checks of this step used.
                "build_code": ctx.data.report.build_code.as_ref().and_then(|note| note.mark_of(*index)),
                "accepted": report.map(|report| report.accepted()),
                "claims": step.map(|step| step.claims.iter().map(|claim| claim.id.clone()).collect::<Vec<_>>()).unwrap_or_default(),
                "uncovered": report.map(|report| report.uncovered.clone()).unwrap_or_default(),
                "errors": report.map(|report| report.errors.clone()).unwrap_or_default(),
            })
        })
        .collect();
    let mut value = serde_json::to_value(summary).ok()?;
    value["step_details"] = Value::Array(steps);
    Some(value)
}

/// The view of one claim.
pub fn claim_view(ctx: &Ctx, review: &ReviewFile, claim: &Claim) -> ClaimView {
    let result = ctx.results.get(&claim.id).cloned();
    let at = claim.at.as_ref().map(|at| at.to_string());
    let subgroup = ctx.claim_subgroup.get(&claim.id).and_then(|id| ctx.subgroup(id));
    ClaimView {
        new_in_subgroup: subgroup.is_some_and(|subgroup| subgroup.subgroup.new_claims.contains(&claim.id)),
        file: claim.at.as_ref().and_then(|at| ctx.item_file(&at.item)),
        id: claim.id.clone(),
        op: claim.op.clone(),
        args: claim.args.clone(),
        step: ctx.claim_step.get(&claim.id).copied(),
        group: ctx.claim_group(&claim.id).map(str::to_string),
        subgroup: ctx.claim_subgroup.get(&claim.id).cloned(),
        summary: claim_summary(claim),
        category: ctx.claim_category(claim),
        op_role: ops::op(&claim.op).map(|info| info.role),
        property: is_property(&claim.op),
        status: status_name(result.as_ref()),
        mark: result.as_ref().map(ClaimResult::mark).unwrap_or_default(),
        review: review.state(&claim.id, ctx.claim_hashes.get(&claim.id).map(String::as_str)),
        flow: at.as_deref().and_then(|at| ctx.flow_function(at)),
        at,
        result,
    }
}

/// True for a property op: the claim states a fact about the code, and changes no code.
pub fn is_property(op: &str) -> bool {
    ops::op(op).is_some_and(|info| info.kind == ops::OpKind::Property)
}

fn status_name(result: Option<&ClaimResult>) -> &'static str {
    match result.map(|result| &result.status) {
        Some(ClaimStatus::Verified { .. }) => "verified",
        Some(ClaimStatus::Refuted { .. }) => "refuted",
        Some(ClaimStatus::Empty) => "empty",
        Some(ClaimStatus::Malformed { .. }) => "malformed",
        Some(ClaimStatus::Pending { .. }) => "pending",
        Some(ClaimStatus::NotChecked { .. }) => "not_checked",
        None => "no_result",
    }
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

fn add_status(counts: &mut Counts, view: &ClaimView) {
    counts.claims += 1;
    // I6: the count of the proofs says how many used changed build code.
    if let Some((result, step)) = view.result.as_ref().and_then(|result| Some((result, result.build_code?))).filter(|(result, _)| result.status.is_verified()) {
        match &result.status {
            ClaimStatus::Verified { proof } if proof == "partial" => counts.partial_marked += 1,
            _ => counts.marked += 1,
        }
        if !counts.mark_steps.contains(&step) {
            counts.mark_steps.push(step);
        }
        counts.marked_text = semilla_core::report::count_mark(counts.marked, &counts.mark_steps);
        counts.partial_marked_text = semilla_core::report::count_mark(counts.partial_marked, &counts.mark_steps);
    }
    match view.status {
        "verified" if matches!(view.result.as_ref().map(|result| &result.status), Some(ClaimStatus::Verified { proof }) if proof == "partial") => counts.partial += 1,
        "verified" => counts.verified += 1,
        "refuted" => counts.refuted += 1,
        "empty" => counts.empty += 1,
        "malformed" => counts.malformed += 1,
        "pending" => counts.pending += 1,
        "not_checked" => counts.not_checked += 1,
        _ => counts.no_result += 1,
    }
    if view.review.is_reviewed() {
        counts.reviewed += 1;
    }
}

fn add_counts(total: &mut Counts, part: &Counts) {
    total.claims += part.claims;
    total.verified += part.verified;
    total.partial += part.partial;
    total.refuted += part.refuted;
    total.empty += part.empty;
    total.malformed += part.malformed;
    total.pending += part.pending;
    total.no_result += part.no_result;
    total.marked += part.marked;
    total.partial_marked += part.partial_marked;
    for step in &part.mark_steps {
        if !total.mark_steps.contains(step) {
            total.mark_steps.push(*step);
        }
    }
    total.marked_text = semilla_core::report::count_mark(total.marked, &total.mark_steps);
    total.partial_marked_text = semilla_core::report::count_mark(total.partial_marked, &total.mark_steps);
    total.uncovered += part.uncovered;
    total.reviewed += part.reviewed;
}

/// A short text from the op and the arguments.
pub fn claim_summary(claim: &Claim) -> String {
    let at = claim.at.as_ref().map(|at| at.to_string()).unwrap_or_default();
    let arg = |name: &str| claim.arg_str(name).map(str::to_string);
    match claim.op.as_str() {
        "rename" | "rename-local" => format!("{} → {}", last_segment(&at), arg("to").unwrap_or_default()),
        "move-item" => format!("{} → {}", at, arg("to").unwrap_or_default()),
        "extract-function" => format!("extract into {}", arg("new").unwrap_or_default()),
        "bump-dependency" => format!("{} → {}", arg("crate").unwrap_or_default(), arg("to").unwrap_or_default()),
        _ => {
            if let (Some(from), Some(to)) = (arg("from"), arg("to")) {
                return format!("{from} → {to}");
            }
            let parts: Vec<String> = claim
                .args
                .iter()
                .map(|(key, value)| match value {
                    Value::String(text) => format!("{key}: {text}"),
                    other => format!("{key}: {other}"),
                })
                .collect();
            parts.join(" · ")
        }
    }
}

/// The fold rule (design 35.7). The second value is the reason. A subgroup with a claim is
/// open: no result folds a claim. Only a subgroup with no claim is folded.
pub fn fold_state(ctx: &Ctx, claims: &[&Claim]) -> (bool, String) {
    if claims.is_empty() {
        return (true, "no claims".to_string());
    }
    // A pending claim waits for the background check.
    let pending = claims.iter().filter(|claim| ctx.results.get(&claim.id).is_some_and(|result| result.status.is_pending())).count();
    if pending > 0 {
        return (false, format!("{pending} claim(s) pending"));
    }
    let not_verified = claims.iter().filter(|claim| !ctx.is_verified(claim)).count();
    if not_verified > 0 {
        return (false, format!("{not_verified} claim(s) not verified"));
    }
    (false, format!("{} verified claim(s)", claims.len()))
}

/// The mark of changed build code for a text that sums results (design 28, I6): the mark of the
/// step, or a mark with no step when the checks used the build code of more than one step. It
/// has a space before it. It is empty when no check used changed build code.
fn proofs_mark(steps: impl Iterator<Item = usize>) -> String {
    let steps: BTreeSet<usize> = steps.collect();
    match (steps.len(), steps.first()) {
        (0, _) | (_, None) => String::new(),
        (1, Some(step)) => format!(" {}", semilla_core::report::build_code_mark(*step)),
        _ => " (with changed build code)".to_string(),
    }
}

fn subgroup_summary(ctx: &Ctx, review: &ReviewFile, subgroup: &SubgroupRef) -> SubgroupSummary {
    let claims: Vec<&Claim> = subgroup.subgroup.claims.iter().filter_map(|id| ctx.claim(id)).collect();
    let views: Vec<ClaimView> = claims.iter().map(|claim| claim_view(ctx, review, claim)).collect();
    let mut counts = Counts::default();
    for view in &views {
        add_status(&mut counts, view);
    }
    let (folded, fold_reason) = fold_state(ctx, &claims);
    let mut category_counts = BTreeMap::new();
    for view in &views {
        *category_counts.entry(category_label(view.category)).or_insert(0) += 1;
    }
    let badges = if subgroup.subgroup.role == Role::Custom {
        Category::ALL.iter().map(|category| category.name()).chain(["unknown"])
            .filter_map(|label| category_counts.get(label).map(|count| format!("{count} {label}")))
            .collect()
    } else {
        Vec::new()
    };
    // Design 35.7: the role `Mechanical` is a label with no rule.
    let label_mismatch = match subgroup.subgroup.role {
        Role::Tests if claims.iter().any(|claim| claim.at.as_ref().is_some_and(|at| !is_test_item(ctx, &at.item))) => {
            Some("Tests must contain only test code.".to_string())
        }
        _ => None,
    };
    let mut locations = Vec::new();
    for claim in &claims {
        for location in claim_locations(ctx, claim) {
            if !locations.contains(&location) {
                locations.push(location);
            }
        }
    }
    SubgroupSummary {
        id: subgroup.id.clone(),
        group: subgroup.subgroup.group.clone(),
        role: subgroup.subgroup.role,
        role_label: role_label(subgroup.subgroup.role),
        title: subgroup.subgroup.title.clone(),
        why: subgroup.subgroup.why.clone(),
        edited: subgroup.subgroup.edited,
        synthetic: subgroup.synthetic,
        folded,
        fold_reason,
        locations,
        new_claims: subgroup.subgroup.new_claims.iter().filter(|id| subgroup.subgroup.claims.contains(id)).cloned().collect(),
        badges,
        category_counts,
        label_mismatch,
        reviewed: !views.is_empty() && views.iter().all(|view| view.review.is_reviewed()),
        counts,
        claims: views,
    }
}

fn is_test_item(ctx: &Ctx, item: &str) -> bool {
    match ctx.graph_node(item) {
        Some(node) => node.test || ctx.graph_ancestors(item).iter().any(|id| ctx.graph_node(id).is_some_and(|node| node.test)),
        None => item.split("::").any(|segment| segment == "tests" || segment.ends_with("_tests")),
    }
}

/// The locations of a claim: the `file` argument, `Cargo.toml` for a dependency, or else the
/// file of the anchor item and of the item that the claim creates (the target of a rename, a
/// move, or an extract). Without a file in the graph, the module of the item.
fn claim_locations(ctx: &Ctx, claim: &Claim) -> Vec<String> {
    if let Some(file) = claim.arg_str("file") {
        return vec![file.to_string()];
    }
    if claim.op == "bump-dependency" {
        return vec!["Cargo.toml".to_string()];
    }
    let mut items: Vec<String> = claim.at.as_ref().map(|at| at.item.clone()).into_iter().collect();
    for item in provided_items(claim) {
        if !items.contains(&item) {
            items.push(item);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let location = ctx.item_file(&item).unwrap_or_else(|| module_of(ctx, &item));
        if !out.contains(&location) {
            out.push(location);
        }
    }
    out
}

/// The nearest module or crate of an item in the graph. Without a graph node, the item path
/// without its type and function segments.
pub fn module_of(ctx: &Ctx, item: &str) -> String {
    let ancestors = ctx.graph_ancestors(item);
    if let Some(module) = ancestors
        .iter()
        .rev()
        .skip(1)
        .find(|id| ctx.graph_node(id).is_some_and(|node| matches!(node.kind, GraphNodeKind::Module | GraphNodeKind::Crate)))
    {
        return module.clone();
    }
    let segments: Vec<&str> = item.split("::").collect();
    let lower: Vec<&str> = segments.iter().take_while(|segment| !segment.starts_with(|c: char| c.is_ascii_uppercase())).copied().collect();
    let keep = if lower.len() == segments.len() { lower.len().saturating_sub(1).max(1) } else { lower.len().max(1) };
    segments[..keep.min(segments.len())].join("::")
}

/// Builds the group summaries in the review order.
pub fn ordered_groups(ctx: &Ctx, review: &ReviewFile) -> Vec<GroupSummary> {
    let mut summaries: Vec<GroupSummary> = ctx.groups.iter().map(|group| group_summary(ctx, review, group)).collect();
    let deps = group_dependencies(ctx);
    for summary in &mut summaries {
        summary.depends_on = deps.get(&summary.id).map(|set| set.iter().cloned().collect()).unwrap_or_default();
    }
    // Priority: a group that changes build code first (design 28.3), then the highest risk,
    // then the order of the change set. A group comes only after all the groups that it
    // depends on. In a cycle, the priority decides.
    let code: Vec<bool> = ctx.groups.iter().map(|group| group_facts(ctx, group).iter().any(|fact| fact.build_code_change)).collect();
    let mut priority: Vec<usize> = (0..summaries.len()).collect();
    // The order of the change set is the order of the steps, as numbers (batch 5x, part E): the
    // list of the groups can have the order of the ID texts (`G1`, `G10`, `G11`, `G2`).
    let first_step = |index: usize| ctx.groups[index].steps.iter().min().copied().unwrap_or(usize::MAX);
    priority.sort_by(|a, b| code[*b].cmp(&code[*a]).then(summaries[*b].risk_score.cmp(&summaries[*a].risk_score)).then(first_step(*a).cmp(&first_step(*b))).then(a.cmp(b)));
    let mut placed: Vec<usize> = Vec::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    while placed.len() < summaries.len() {
        let remaining: Vec<usize> = priority.iter().copied().filter(|index| !placed.contains(index)).collect();
        let ready = remaining.iter().copied().find(|index| {
            summaries[*index].depends_on.iter().all(|dep| done.contains(dep) || !remaining.iter().any(|other| &summaries[*other].id == dep))
        });
        let next = ready.unwrap_or(remaining[0]);
        done.insert(summaries[next].id.clone());
        placed.push(next);
    }
    let mut slots: Vec<Option<GroupSummary>> = summaries.into_iter().map(Some).collect();
    placed.into_iter().filter_map(|index| slots[index].take()).collect()
}

fn group_summary(ctx: &Ctx, review: &ReviewFile, group: &Group) -> GroupSummary {
    let mut subgroups: Vec<SubgroupSummary> = ctx
        .subgroups
        .iter()
        .filter(|subgroup| subgroup.subgroup.group == group.id)
        .map(|subgroup| subgroup_summary(ctx, review, subgroup))
        .collect();
    subgroups.sort_by_key(|subgroup| subgroup.role.order());
    let claim_ids = ctx.group_claims(&group.id);
    let claims: Vec<&Claim> = claim_ids.iter().filter_map(|id| ctx.claim(id)).collect();
    let views: Vec<ClaimView> = claims.iter().map(|claim| claim_view(ctx, review, claim)).collect();
    let uncovered: Vec<Uncovered> = ctx
        .data
        .report
        .steps
        .iter()
        .filter(|step| group.steps.contains(&step.step))
        .flat_map(|step| step.uncovered.iter().cloned())
        .collect();
    let mut counts = Counts::default();
    for view in &views {
        add_status(&mut counts, view);
    }
    counts.uncovered = uncovered.len();
    let panic_delta: i64 = ctx.data.report.steps.iter().filter(|step| group.steps.contains(&step.step)).map(|step| step.panic_delta).sum();
    let (risk_score, risk_reasons) = risk(ctx, group, &subgroups, &claims, &counts, panic_delta);
    let risk = match risk_score {
        0 => 0,
        1..=2 => 1,
        3..=4 => 2,
        _ => 3,
    };
    let kind = computed_kind(ctx, group);
    let mark = proofs_mark(group_facts(ctx, group).iter().filter_map(|fact| fact.build_code).chain(claims.iter().filter_map(|claim| ctx.results.get(&claim.id)).filter_map(|result| result.build_code)));
    GroupSummary {
        id: group.id.clone(),
        title: group.title.clone(),
        kind_mark: if matches!(kind, GroupKind::Refactor | GroupKind::Rename | GroupKind::Move) { mark.clone() } else { String::new() },
        kind,
        why: group.why.clone(),
        steps: group.steps.clone(),
        risk,
        risk_score,
        risk_reasons,
        depends_on: Vec::new(),
        mark,
        all_equal: {
            let facts = group_facts(ctx, group);
            !facts.is_empty()
                && uncovered.is_empty()
                && claims.iter().all(|claim| ctx.is_verified(claim))
                && facts.iter().all(|fact| matches!(fact.compare, Compare::CodeEqual | Compare::NoCode) && !fact.spotlight)
        },
        panic_delta,
        split_hint: split_hint(ctx, group, &claims),
        synthetic: ctx.synthetic_groups.contains(&group.id),
        reviewed: !views.is_empty() && views.iter().all(|view| view.review.is_reviewed()),
        counts,
        subgroups,
        uncovered,
    }
}

/// The kind of a group in a reviewer output (batch 5k, A3). Semilla computes it from the facts
/// and the claim results of the steps of the group. The stored kind is a claim of the agent
/// (I5): no reviewer output reads it. While a compare is pending, the kind is `behavior`.
fn computed_kind(ctx: &Ctx, group: &Group) -> GroupKind {
    let steps: Vec<&semilla_core::StepReport> = ctx.data.report.steps.iter().filter(|step| group.steps.contains(&step.step)).collect();
    semilla_core::report::derived_kind(&steps).unwrap_or(GroupKind::Behavior)
}

/// The derived facts of the steps of a group.
fn group_facts<'a>(ctx: &'a Ctx, group: &Group) -> Vec<&'a semilla_core::Fact> {
    ctx.data.report.steps.iter().filter(|step| group.steps.contains(&step.step)).flat_map(|step| step.facts.iter()).collect()
}

/// The text of the `all_equal` mark of a group in each output.
pub const ALL_EQUAL: &str = "each fact: code equal or no code";

/// The risk of a group (design 35.7). It is a help for the order of the review. It is no
/// verdict.
///
/// - A fact with the result `code equal` or `no code` adds no point.
/// - 2 points: a `control flow` fact with `code different`.
/// - 1 point: one or more other facts with `code different`, `no compare`, `pending`, or
///   `not checked`.
/// - 2 points: a fact with the mismatch mark.
/// - 1 point each: a spotlight fact, a public API change, a panic delta above zero. A public
///   API change is an API level `major` or `minor` of a step, or an `interface` subgroup with
///   a claim that is not a rename and not a move.
/// - 2 points each: a claim that is not verified, an uncovered change, a pair warning.
/// - A pending claim waits for the background check. It gives a reason, but no points.
fn risk(ctx: &Ctx, group: &Group, subgroups: &[SubgroupSummary], claims: &[&Claim], counts: &Counts, panic_delta: i64) -> (u32, Vec<String>) {
    let mut score = 0;
    let mut reasons = Vec::new();
    let facts: Vec<&semilla_core::Fact> = group_facts(ctx, group);
    let control = facts.iter().filter(|fact| fact.category == Category::ControlFlow && fact.compare == Compare::CodeDifferent).count();
    if control > 0 {
        score += 2;
        reasons.push(format!("{control} control flow fact(s) with code different"));
    }
    let mut other = false;
    for compare in [Compare::CodeDifferent, Compare::NoCompare, Compare::Pending, Compare::NotChecked] {
        let count = facts.iter().filter(|fact| fact.compare == compare && !(compare == Compare::CodeDifferent && fact.category == Category::ControlFlow)).count();
        if count > 0 {
            other = true;
            reasons.push(format!("{count} fact(s) with {}", compare.name()));
        }
    }
    if other {
        score += 1;
    }
    let mismatch = facts.iter().filter(|fact| fact.mismatch()).count();
    if mismatch > 0 {
        score += 2;
        reasons.push(format!("{mismatch} mismatch mark(s)"));
    }
    let spotlight = facts.iter().filter(|fact| fact.spotlight).count();
    if spotlight > 0 {
        score += 1;
        reasons.push(format!("{spotlight} spotlight fact(s)"));
    }
    // The stored kind of the group does not count (A3): the API level comes from the report.
    let api_steps = ctx.data.report.steps.iter().any(|step| group.steps.contains(&step.step) && matches!(step.api_level.as_deref(), Some("major" | "minor")));
    // A subgroup with the role `interface` also gives the point, if it has a claim that is
    // not a rename and not a move.
    let interface = subgroups.iter().any(|subgroup| subgroup.role == Role::Interface && subgroup.claims.iter().any(|claim| !matches!(claim.category, Some(Category::Rename | Category::Move))));
    if interface || api_steps {
        score += 1;
        reasons.push("public API".to_string());
    }
    if panic_delta > 0 {
        score += 1;
        reasons.push(format!("panics +{panic_delta}"));
    }
    let failed = counts.claims.saturating_sub(counts.verified + counts.partial + counts.pending);
    if failed > 0 {
        score += 2;
        reasons.push(format!("{failed} not verified"));
    }
    if counts.pending > 0 {
        reasons.push(format!("{} pending", counts.pending));
    }
    if counts.uncovered > 0 {
        score += 2;
        reasons.push(format!("{} uncovered", counts.uncovered));
    }
    let fact_ids: Vec<String> = ctx.data.report.steps.iter().filter(|step| group.steps.contains(&step.step)).flat_map(|step| step.facts.iter().map(move |fact| format!("step {} {}", step.step, fact.id))).collect();
    let pairs = ctx.data.report.pairs.iter().filter(|pair| pair.claims.iter().any(|id| claims.iter().any(|claim| claim.id == *id) || fact_ids.contains(id))).count();
    if pairs > 0 {
        score += 2;
        reasons.push(format!("{pairs} pair warning(s)"));
    }
    (score, reasons)
}

/// The API level. The verifier's estimate comes first. Without it, the UI guesses from the
/// claims: `major` for a changed or removed public item, `minor` for a new public item, `patch`
/// for other changes, `none` without claims.
fn api_level(ctx: &Ctx) -> &'static str {
    match ctx.data.report.api_level.as_deref() {
        Some("major") => return "major",
        Some("minor") => return "minor",
        Some("patch") => return "patch",
        _ => {}
    }
    let mut level = 0;
    for (_, claim) in ctx.all_claims() {
        level = level.max(1);
        let Some(at) = &claim.at else { continue };
        if !at.path.is_empty() {
            continue;
        }
        let public = match ctx.graph_node(&at.item) {
            Some(node) => node.entry && !node.test,
            None => {
                ctx.claim_subgroup.get(&claim.id).and_then(|id| ctx.subgroup(id)).is_some_and(|sub| sub.subgroup.role == Role::Interface)
            }
        };
        if !public {
            continue;
        }
        match claim.op.as_str() {
            "change-signature" | "remove-item" | "rename" | "move-item" | "add-struct-field" | "add-enum-variant" | "change-visibility" => {
                level = level.max(3)
            }
            "add-item" | "add-trait-impl" => level = level.max(2),
            _ => {}
        }
    }
    ["none", "patch", "minor", "major"][level]
}

/// The items that a claim creates: the new name of a rename, the target of a move, a new
/// function, or a new item.
fn provided_items(claim: &Claim) -> Vec<String> {
    let item = claim.at.as_ref().map(|at| at.item.clone());
    match claim.op.as_str() {
        "rename" => match (&item, claim.arg_str("to")) {
            (Some(item), Some(to)) => {
                let parent = item.rsplit_once("::").map(|(parent, _)| parent.to_string());
                vec![match parent {
                    Some(parent) if !to.contains("::") => format!("{parent}::{to}"),
                    _ => to.to_string(),
                }]
            }
            _ => Vec::new(),
        },
        "move-item" => claim.arg_str("to").map(str::to_string).into_iter().collect(),
        "extract-function" => claim.arg_str("new").map(str::to_string).into_iter().collect(),
        "add-item" => item.into_iter().collect(),
        _ => Vec::new(),
    }
}

/// The items that a claim uses: its anchor item, and the items that its arguments name.
fn used_items(claim: &Claim, provided: &BTreeMap<String, (String, usize)>) -> Vec<String> {
    let mut used: Vec<String> = claim.at.as_ref().map(|at| at.item.clone()).into_iter().collect();
    let args = Value::Object(claim.args.clone()).to_string();
    for item in provided.keys() {
        let name = last_segment(item);
        if !name.is_empty() && contains_word(&args, name) {
            used.push(item.clone());
        }
    }
    used
}

fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + word.len()..].chars().next();
        let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        !ident(before) && !ident(after)
    })
}

/// For each group, the groups that create the items that it uses in a later step.
fn group_dependencies(ctx: &Ctx) -> HashMap<String, BTreeSet<String>> {
    // item -> (group, step) of the claim that creates it.
    let mut provided: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for (step, claim) in ctx.all_claims() {
        let Some(group) = ctx.step_group.get(&step.index) else { continue };
        for item in provided_items(claim) {
            provided.entry(item).or_insert_with(|| (group.clone(), step.index));
        }
    }
    let mut deps: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (step, claim) in ctx.all_claims() {
        let Some(group) = ctx.step_group.get(&step.index) else { continue };
        for item in used_items(claim, &provided) {
            if let Some((owner, owner_step)) = provided.get(&item)
                && owner != group
                && *owner_step < step.index
            {
                deps.entry(group.clone()).or_default().insert(owner.clone());
            }
        }
    }
    deps
}

/// A split warning if the items of a group fall into parts with no link: no call edge, no
/// parent link, no name of one item in the claims of the other, and no fact that links them (a
/// fact links its entity, its affected bodies, and its sites). The parts come from the claims only:
/// the facts of one intent are not a reason for a split.
fn split_hint(ctx: &Ctx, group: &Group, claims: &[&Claim]) -> Option<String> {
    let mut items: Vec<String> = Vec::new();
    for claim in claims {
        if let Some(at) = &claim.at
            && !items.contains(&at.item)
        {
            items.push(at.item.clone());
        }
    }
    let facts: Vec<&semilla_core::Fact> = ctx.data.report.steps.iter().filter(|step| group.steps.contains(&step.step)).flat_map(|step| &step.facts).collect();
    let fact_links: Vec<BTreeSet<String>> = facts
        .iter()
        .map(|fact| {
            let mut members: BTreeSet<String> = fact.affected.iter().cloned().collect();
            members.extend(fact.at.iter().map(|at| at.item.clone()));
            members.extend(fact.sites.iter().flat_map(|group| group.sites.iter().map(|site| site.item.clone())));
            members
        })
        .collect();
    if items.len() < 2 {
        return None;
    }
    let mut parent: Vec<usize> = (0..items.len()).collect();
    fn find(parent: &mut [usize], index: usize) -> usize {
        let mut root = index;
        while parent[root] != root {
            root = parent[root];
        }
        parent[index] = root;
        root
    }
    let texts: Vec<String> = items
        .iter()
        .map(|item| {
            claims
                .iter()
                .filter(|claim| claim.at.as_ref().is_some_and(|at| &at.item == item))
                .map(|claim| {
                    let result = ctx.results.get(&claim.id);
                    format!(
                        "{} {} {}",
                        Value::Object(claim.args.clone()),
                        result.and_then(|result| result.before.clone()).unwrap_or_default(),
                        result.and_then(|result| result.after.clone()).unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    for a in 0..items.len() {
        for b in (a + 1)..items.len() {
            let linked = ctx.callees.get(&items[a]).is_some_and(|out| out.contains(&items[b]))
                || ctx.callees.get(&items[b]).is_some_and(|out| out.contains(&items[a]))
                || items[b].starts_with(&format!("{}::", items[a]))
                || items[a].starts_with(&format!("{}::", items[b]))
                || contains_word(&texts[a], last_segment(&items[b]))
                || contains_word(&texts[b], last_segment(&items[a]))
                || fact_links.iter().any(|members| members.contains(&items[a]) && members.contains(&items[b]));
            if linked {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                parent[ra] = rb;
            }
        }
    }
    let mut parts: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for index in 0..items.len() {
        let root = find(&mut parent, index);
        parts.entry(root).or_default().push(items[index].clone());
    }
    // A part with only test code belongs to the change that it tests, so it does not count.
    parts.retain(|_, part| !part.iter().all(|item| is_test_item(ctx, item)));
    // The call graph does not follow trait calls (for example `.parse()` to `from_str`), so parts
    // in one module can be linked. Only parts in different modules give the hint.
    let module = |item: &String| {
        let segments = semilla_syntax_free_split(item);
        segments[..segments.len().saturating_sub(1).max(1)].iter().take(2).cloned().collect::<Vec<_>>().join("::")
    };
    let modules: Vec<BTreeSet<String>> = parts.values().map(|part| part.iter().map(module).collect()).collect();
    let separate = modules.iter().enumerate().any(|(a, left)| modules.iter().skip(a + 1).any(|right| left.is_disjoint(right)));
    (parts.len() > 1 && separate).then(|| {
        let text: Vec<String> = parts.values().map(|part| part.join(", ")).collect();
        format!("This group has {} parts with no link in the call graph: {}. Think about a split.", parts.len(), text.join(" | "))
    })
}

/// Splits an item path at `::`, but not inside `<…>`.
fn semilla_syntax_free_split(path: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut current) = (0i32, String::new());
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ':' if depth == 0 && chars.peek() == Some(&':') => {
                chars.next();
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts.push(current);
    parts
}
