//! The change-set summary (design sections 11.2 and 11.3).
//!
//! - Groups: by risk, then by dependency. A group that adds an item comes before a group that
//!   uses it.
//! - Subgroups: by role order (`Role::order()`).
//! - Fold state: the contents decide it, not the label. A subgroup is open if a claim has a
//!   computed class that is not neutral, or if a claim is not verified.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use serde_json::Value;

use semilla_core::view::GraphNodeKind;
use semilla_core::{Claim, ClaimResult, ClaimStatus, Class, Group, GroupKind, Role, Uncovered, ops};

use crate::ctx::{Ctx, SubgroupRef, class_label, last_segment, role_label};
use crate::review::{ReviewFile, ReviewState};

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Counts {
    pub claims: usize,
    pub verified: usize,
    pub unproved: usize,
    pub refuted: usize,
    pub empty: usize,
    pub malformed: usize,
    /// Claims without a result in the report.
    pub no_result: usize,
    pub uncovered: usize,
    pub reviewed: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChangeSetSummary {
    pub base_tree: String,
    pub head_tree: String,
    pub accepted: bool,
    /// The coverage score of the report (design section 9.2), from 0 to 1.
    pub coverage: f64,
    /// `major`, `minor`, `patch`, or `none`. A hint only: the UI is not trusted.
    pub api_level: &'static str,
    /// The change in explicit panic sites from the verifier (`Report.panic_delta`).
    pub panic_delta: i64,
    pub group_count: usize,
    pub step_count: usize,
    pub counts: Counts,
    pub organization: Vec<String>,
    pub warnings: Vec<String>,
    pub groups: Vec<GroupSummary>,
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
    /// True if all claims are proved neutral and nothing is uncovered.
    pub all_neutral: bool,
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
    pub fold_reason: String,
    /// The locations of the claims: files, or modules when no file is known.
    pub locations: Vec<String>,
    /// The claims that the engine added after the agent's edit (design section 6.3, rule 5).
    pub new_claims: Vec<String>,
    /// The computed role badges of a custom subgroup, for example `2 ctrl flow`.
    pub badges: Vec<String>,
    /// The number of claims for each class label.
    pub class_counts: BTreeMap<&'static str, usize>,
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
    /// The computed class.
    pub class: Option<Class>,
    /// The role of the op in the rule table.
    pub op_role: Option<Role>,
    /// `verified`, `unproved`, `refuted`, `empty`, `malformed`, or `no_result`.
    pub status: &'static str,
    pub result: Option<ClaimResult>,
    pub review: ReviewState,
    /// The function of the flow view that shows this claim.
    pub flow: Option<String>,
    /// True if the engine added the claim to its subgroup after the agent's edit.
    pub new_in_subgroup: bool,
    /// The file of the anchor item, if the graph knows it.
    pub file: Option<String>,
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
        coverage: report.score,
        api_level: api_level(ctx),
        panic_delta: report.panic_delta,
        group_count: ctx.groups.len(),
        step_count: ctx.data.changeset.steps.len(),
        counts,
        organization: report.organization.clone(),
        warnings: report.warnings.clone(),
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
                "declared_class": step.and_then(|step| step.class),
                "computed_class": report.and_then(|report| report.computed_class),
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
        class: ctx.claim_class(claim),
        op_role: ops::op(&claim.op).map(|info| info.role),
        status: status_name(result.as_ref()),
        review: review.state(&claim.id, ctx.claim_hashes.get(&claim.id).map(String::as_str)),
        flow: at.as_deref().and_then(|at| ctx.flow_function(at)),
        at,
        result,
    }
}

fn status_name(result: Option<&ClaimResult>) -> &'static str {
    match result.map(|result| &result.status) {
        Some(ClaimStatus::Verified { .. }) => "verified",
        Some(ClaimStatus::Unproved { .. }) => "unproved",
        Some(ClaimStatus::Refuted { .. }) => "refuted",
        Some(ClaimStatus::Empty) => "empty",
        Some(ClaimStatus::Malformed { .. }) => "malformed",
        None => "no_result",
    }
}

fn add_status(counts: &mut Counts, view: &ClaimView) {
    counts.claims += 1;
    match view.status {
        "verified" => counts.verified += 1,
        "unproved" => counts.unproved += 1,
        "refuted" => counts.refuted += 1,
        "empty" => counts.empty += 1,
        "malformed" => counts.malformed += 1,
        _ => counts.no_result += 1,
    }
    if view.review.is_reviewed() {
        counts.reviewed += 1;
    }
}

fn add_counts(total: &mut Counts, part: &Counts) {
    total.claims += part.claims;
    total.verified += part.verified;
    total.unproved += part.unproved;
    total.refuted += part.refuted;
    total.empty += part.empty;
    total.malformed += part.malformed;
    total.no_result += part.no_result;
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

/// The fold rule (design section 6.3, rule 2). The second value is the reason.
pub fn fold_state(ctx: &Ctx, claims: &[&Claim]) -> (bool, String) {
    if claims.is_empty() {
        return (true, "no claims".to_string());
    }
    let not_neutral = claims.iter().filter(|claim| ctx.claim_class(claim) != Some(Class::Neutral)).count();
    if not_neutral > 0 {
        return (false, format!("{not_neutral} claim(s) with a class that is not neutral"));
    }
    let not_verified = claims.iter().filter(|claim| !ctx.is_proved_neutral(claim)).count();
    if not_verified > 0 {
        return (false, format!("{not_verified} claim(s) not verified"));
    }
    (true, "all claims are proved neutral".to_string())
}

fn subgroup_summary(ctx: &Ctx, review: &ReviewFile, subgroup: &SubgroupRef) -> SubgroupSummary {
    let claims: Vec<&Claim> = subgroup.subgroup.claims.iter().filter_map(|id| ctx.claim(id)).collect();
    let views: Vec<ClaimView> = claims.iter().map(|claim| claim_view(ctx, review, claim)).collect();
    let mut counts = Counts::default();
    for view in &views {
        add_status(&mut counts, view);
    }
    let (folded, fold_reason) = fold_state(ctx, &claims);
    let mut class_counts = BTreeMap::new();
    for view in &views {
        *class_counts.entry(class_label(view.class)).or_insert(0) += 1;
    }
    let badges = if subgroup.subgroup.role == Role::Custom {
        ["ctrl flow", "logic", "neutral", "other", "unknown"]
            .iter()
            .filter_map(|label| class_counts.get(label).map(|count| format!("{count} {label}")))
            .collect()
    } else {
        Vec::new()
    };
    let label_mismatch = match subgroup.subgroup.role {
        Role::Mechanical if claims.iter().any(|claim| !ctx.is_proved_neutral(claim)) => {
            Some("Mechanical must contain only proved neutral claims.".to_string())
        }
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
        class_counts,
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
    // Priority: the highest risk first, then the order of the change set. A group comes only
    // after all the groups that it depends on. In a cycle, the priority decides.
    let mut priority: Vec<usize> = (0..summaries.len()).collect();
    priority.sort_by(|a, b| summaries[*b].risk_score.cmp(&summaries[*a].risk_score).then(a.cmp(b)));
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
    let (risk_score, risk_reasons) = risk(ctx, group, &claims, &subgroups, &counts, panic_delta);
    let risk = match risk_score {
        0 => 0,
        1..=2 => 1,
        3..=4 => 2,
        _ => 3,
    };
    GroupSummary {
        id: group.id.clone(),
        title: group.title.clone(),
        kind: group.kind,
        why: group.why.clone(),
        steps: group.steps.clone(),
        risk,
        risk_score,
        risk_reasons,
        depends_on: Vec::new(),
        all_neutral: !claims.is_empty() && uncovered.is_empty() && claims.iter().all(|claim| ctx.is_proved_neutral(claim)),
        panic_delta,
        split_hint: split_hint(ctx, &claims),
        synthetic: ctx.synthetic_groups.contains(&group.id),
        reviewed: !views.is_empty() && views.iter().all(|view| view.review.is_reviewed()),
        counts,
        subgroups,
        uncovered,
    }
}

/// The risk of a group (design section 11.2): control flow, `unsafe`, public API, panic delta,
/// and the claims that the verifier did not accept.
fn risk(
    ctx: &Ctx,
    group: &Group,
    claims: &[&Claim],
    subgroups: &[SubgroupSummary],
    counts: &Counts,
    panic_delta: i64,
) -> (u32, Vec<String>) {
    let mut score = 0;
    let mut reasons = Vec::new();
    let classes: Vec<Option<Class>> = claims.iter().map(|claim| ctx.claim_class(claim)).collect();
    let control = classes.iter().filter(|class| **class == Some(Class::ControlFlow)).count();
    if control > 0 {
        score += 2;
        reasons.push(format!("{control} ctrl flow"));
    }
    let has_unsafe = claims.iter().any(|claim| claim_texts(ctx, claim).iter().any(|text| text.contains("unsafe")));
    if has_unsafe {
        score += 2;
        reasons.push("unsafe".to_string());
    }
    let interface = subgroups
        .iter()
        .any(|subgroup| subgroup.role == Role::Interface && subgroup.claims.iter().any(|claim| claim.class != Some(Class::Neutral)));
    if group.kind == GroupKind::Api || interface {
        score += 2;
        reasons.push("public API".to_string());
    }
    if panic_delta > 0 {
        score += 1;
        reasons.push(format!("panics +{panic_delta}"));
    }
    let failed = counts.claims - counts.verified;
    if failed > 0 {
        score += 2;
        reasons.push(format!("{failed} not verified"));
    }
    if counts.uncovered > 0 {
        score += 2;
        reasons.push(format!("{} uncovered", counts.uncovered));
    }
    let logic = classes.iter().filter(|class| **class == Some(Class::Logic)).count();
    if logic > 0 {
        score += 1;
        reasons.push(format!("{logic} logic"));
    }
    (score, reasons)
}

/// The texts of a claim for the text checks: the result before and after, and the sources.
fn claim_texts<'a>(ctx: &'a Ctx, claim: &'a Claim) -> Vec<&'a str> {
    let mut texts = Vec::new();
    if let Some(result) = ctx.results.get(&claim.id) {
        texts.extend(result.before.as_deref());
        texts.extend(result.after.as_deref());
    }
    texts
}

/// The API level: `major` for a changed or removed public item, `minor` for a new public
/// item, `patch` for other changes, `none` without claims.
fn api_level(ctx: &Ctx) -> &'static str {
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
/// parent link, and no name of one item in the claims of the other.
fn split_hint(ctx: &Ctx, claims: &[&Claim]) -> Option<String> {
    let mut items: Vec<String> = Vec::new();
    for claim in claims {
        if let Some(at) = &claim.at
            && !items.contains(&at.item)
        {
            items.push(at.item.clone());
        }
    }
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
                || contains_word(&texts[b], last_segment(&items[a]));
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
    (parts.len() > 1).then(|| {
        let text: Vec<String> = parts.values().map(|part| part.join(", ")).collect();
        format!("This group has {} parts with no link in the call graph: {}. Think about a split.", parts.len(), text.join(" | "))
    })
}
