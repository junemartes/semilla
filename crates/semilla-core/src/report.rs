//! Verification results.

use serde::{Deserialize, Serialize};

use crate::anchor::Anchor;
use crate::ops::Class;

/// The view in which a difference was found (design section 4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Syntax,
    Signature,
    Body,
    File,
}

/// The result of one claim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ClaimStatus {
    /// The check passed. `proof` names the proof type, for example `N1` or `shape` or `smt`.
    Verified { proof: String },
    /// A neutral claim that no normal form could prove.
    Unproved { reason: String },
    /// Evidence of a false claim.
    Refuted { reason: String },
    /// The claim explains no difference.
    Empty,
    /// Bad syntax, or the anchor does not resolve.
    Malformed { reason: String },
    /// The body proof waits for the background check (phase 2, design section 19).
    Pending { reason: String },
    /// No check ran: the step uses build code that the change set changes, and the reviewer did
    /// not allow it (invariant I6, design 28).
    NotChecked { reason: String },
}

impl ClaimStatus {
    pub fn is_verified(&self) -> bool {
        matches!(self, ClaimStatus::Verified { .. })
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, ClaimStatus::Pending { .. })
    }

    /// The text form, for example "verified (N1)" or "unproved: …".
    pub fn text(&self) -> String {
        match self {
            ClaimStatus::Verified { proof } => format!("verified ({proof})"),
            ClaimStatus::Unproved { reason } => format!("unproved: {reason}"),
            ClaimStatus::Refuted { reason } => format!("refuted: {reason}"),
            ClaimStatus::Empty => "empty: the claim explains no change".to_string(),
            ClaimStatus::Malformed { reason } => format!("malformed: {reason}"),
            ClaimStatus::Pending { reason } => format!("pending: {reason}"),
            ClaimStatus::NotChecked { reason } => format!("not checked: {reason}"),
        }
    }
}

/// The condition effect labels (design section 8.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Narrow,
    Widen,
    Negate,
    Equivalent,
    Unrelated,
}

/// The result of one claim, with the data for the reviewer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimResult {
    pub claim: String,
    pub step: usize,
    #[serde(flatten)]
    pub status: ClaimStatus,
    /// The computed class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// The computed effect, for condition changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<Effect>,
    /// An example input for the effect, for example `i = 5, n = 5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<String>,
    /// A hint for the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// The source text before and after, for the reviewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// The bodies with a changed MIR that the claim owns: the bodies of its edits, and the bodies
    /// that it changes with no text edit (an expanded macro, a new method).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected: Vec<String>,
    /// For a refuted `calls-only` claim: the complete list of `calls` that makes the claim true
    /// (the entries of the agent, and the shortest module paths that cover the other external
    /// calls). The command to run again uses it. It is empty when Semilla has no such list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub complete_calls: Vec<String>,
}

/// A difference that no claim explains.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Uncovered {
    pub step: usize,
    pub view: View,
    /// The location, as an anchor when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Anchor>,
    /// The file, for differences outside Rust items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The result of one step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StepReport {
    pub step: usize,
    pub intent: String,
    pub declared_class: Option<Class>,
    /// The computed class of the step: the highest class of its claims.
    pub computed_class: Option<Class>,
    pub claims: Vec<ClaimResult>,
    pub uncovered: Vec<Uncovered>,
    /// Errors that stop the check of the step, for example a build failure.
    pub errors: Vec<String>,
    /// The change in the number of explicit panic sites (design 5.5).
    #[serde(default)]
    pub panic_delta: i64,
    /// The estimated semver level of the public API change: `major`, `minor`, or `patch`.
    /// It comes from the syntax view, not from `cargo-semver-checks`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_level: Option<String>,
    /// The public API changes that give the level.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_changes: Vec<String>,
    /// Changes that always need the reviewer's attention: `unsafe` code, `#[cfg]` attributes,
    /// features, and build scripts (design 5.5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spotlight: Vec<String>,
    /// The changed lines by the kind of claim that explains them.
    #[serde(default)]
    pub lines: LineCounts,
    /// The facts: the type-level changes that the diff determines (design 22.5). The verifier
    /// derives them; the agent writes no claim for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<Fact>,
    /// The notes for the agent. A note does not reject the step, for example "put the rename in
    /// its own step to get a proof".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// True if the step has a candidate pair for a derived identity (design 24.2, rule 4). Only
    /// phase 2 can prove it, so the `step` command runs phase 2 at once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub identity_candidates: bool,
}

/// One derived fact: the difference in one part of a paired entity (design 22.5).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    /// The fact ID in the step, for example `f1`.
    pub id: String,
    /// The entity and the part, for example `shop::Account.balance` or `shop::f/sig/param[a]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Anchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The part: `type`, `visibility`, `member added`, `parameter`, `return type`, …
    pub part: String,
    pub description: String,
    /// Always `derived`: Semilla computed the fact from the diff.
    pub status: String,
    /// The class from the MIR compare: neutral when every body is equal. `None` before phase 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// The bodies whose MIR changed (design 22.6, F5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected: Vec<String>,
    /// Why the fact needs the reviewer's attention (empty: no spotlight).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotlight_why: Option<String>,
    /// True if the fact is in the spotlight. Then it needs a reason from the agent.
    #[serde(default)]
    pub spotlight: bool,
    /// The reason of the agent (`semilla reason`), free text for the reviewer. It is not verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The forced edits at other sites, grouped by the inserted or removed text (design 22.6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<SiteGroup>,
    /// The content change of a file fact: a line diff with a size limit, or "binary, N bytes".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// The bodies with a site of the fact whose MIR is equal (proved). They are not in
    /// `affected`. So each body of a site is in one of the two lists (trial 4, D14).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub equal_sites: Vec<String>,
}

impl Fact {
    /// True if the description names each place of a site group (a fold of equal type changes:
    /// "type `u32` → `u64` in 8 places: …"). A view does not show the sites of the group again.
    /// True for a fact of a doc comment change. A doc comment has no MIR: its class is `other`.
    pub fn is_doc(&self) -> bool {
        self.class == Some(Class::Other) && self.part == "edit" && self.description.starts_with("changed docs")
    }

    pub fn names_sites(&self, group: &SiteGroup) -> bool {
        self.description.starts_with(&format!("{} in ", group.edit))
    }
}

/// The sites of a fact that have the same forced edit.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SiteGroup {
    /// The forced edit, for example "add the argument `None`".
    pub edit: String,
    pub sites: Vec<Anchor>,
}

/// The changed lines of a step or a change set, by the kind of claim that explains them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineCounts {
    /// Lines that fine-grained claims explain (everything except the four kinds below).
    pub fine: usize,
    /// Lines of new items (`add-item`).
    pub new: usize,
    /// Lines of removed items (`remove-item`).
    pub removed: usize,
    /// Lines of rewritten items (`rewrite-item`).
    pub rewritten: usize,
}

impl LineCounts {
    pub fn total(&self) -> usize {
        self.fine + self.new + self.removed + self.rewritten
    }

    pub fn add(&mut self, other: &LineCounts) {
        self.fine += other.fine;
        self.new += other.new;
        self.removed += other.removed;
        self.rewritten += other.rewritten;
    }

    /// The precision: the part of the lines that fine-grained claims explain.
    pub fn precision(&self) -> f64 {
        if self.total() == 0 { 1.0 } else { self.fine as f64 / self.total() as f64 }
    }

    /// One line of text for the agent and the reviewer.
    pub fn summary(&self) -> String {
        format!(
            "lines: {} fine-grained ({:.0}%), {} new, {} removed, {} rewritten",
            self.fine,
            self.precision() * 100.0,
            self.new,
            self.removed,
            self.rewritten
        )
    }
}

/// A removed item and an added item that look like one item that changed (design 21.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairWarning {
    pub old: String,
    pub new: String,
    /// The similarity of the two items, from 0 to 1.
    pub similarity: f64,
    /// The `remove-item` and `add-item` claims.
    pub claims: Vec<String>,
    /// The reason of the agent, if the agent refused the warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// The decision of the reviewer: `accepted` (the agent must rewrite the pair) or `dismissed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// For a pair with equal texts: the names in the text that point to a different item of the
    /// workspace on the two sides (batch 5k, B5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolves: Vec<PairName>,
}

/// A name in the text of a pair, and the item that it points to on each side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairName {
    pub name: String,
    pub old: String,
    pub new: String,
}

impl PairWarning {
    /// The key of the pair in the review file and in the refusals.
    pub fn key(&self) -> String {
        format!("{} -> {}", self.old, self.new)
    }

    pub fn text(&self) -> String {
        let parent = |path: &str| path.rsplit_once("::").map(|(p, _)| p.to_string()).unwrap_or_default();
        let last = |path: &str| path.rsplit("::").next().unwrap_or(path).rsplit('.').next().unwrap_or(path).to_string();
        // C2: the claim syntax of `semilla step`, and no retired op.
        let claim = if parent(&self.old) == parent(&self.new) { format!("--claim 'rename {} to={}'", self.old, last(&self.new)) } else { format!("--claim 'move-item {} to={}'", self.old, self.new) };
        let mut text = format!(
            "`{}` (removed) and `{}` (added) are {:.0}% similar: if it is one item with a new name or place, put the rename or the move in its own step with no other change (Semilla proves it with no claim, or give `{claim}`), and change the body in the next step",
            self.old,
            self.new,
            self.similarity * 100.0
        );
        if !self.resolves.is_empty() {
            // B5 of batch 5k: the texts are equal, and a name has a different meaning in the new
            // place. The advice "own step" does not help: the move itself changes the code.
            let names: Vec<String> = self.resolves.iter().map(|name| format!("`{}` is `{}` in the removed item and `{}` in the added item", name.name, name.old, name.new)).collect();
            text = format!(
                "`{}` (removed) and `{}` (added) have the same text, but a name in the text points to a different item: {}. The move changes the meaning of the code: it is not one item in a new place",
                self.old,
                self.new,
                names.join("; ")
            );
        }
        if let Some(reason) = &self.refusal {
            text.push_str(&format!("; the agent refused: {}", crate::text::quoted(reason)));
        }
        match self.reviewer.as_deref() {
            Some("accepted") => text.push_str("; the reviewer accepted the warning: rewrite the pair"),
            Some("dismissed") => text.push_str("; the reviewer dismissed the warning"),
            _ => {}
        }
        text
    }
}

impl StepReport {
    pub fn accepted(&self) -> bool {
        self.errors.is_empty()
            && self.uncovered.is_empty()
            && self.facts.iter().all(|fact| !fact.spotlight || fact.reason.is_some())
            && self.claims.iter().all(|claim| claim.status.is_verified())
            && match (self.declared_class, self.computed_class) {
                (Some(declared), Some(computed)) => declared == computed,
                _ => true,
            }
    }
}

/// The result of the full change set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub steps: Vec<StepReport>,
    /// Errors in the groups and subgroups.
    pub organization: Vec<String>,
    /// Warnings that do not reject the change set.
    pub warnings: Vec<String>,
    /// The lines at the top of each output: the base commit of the change set and its Semilla
    /// check (design 25.2). The CLI computes them from git at the time of the output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
    /// `syntax` for a phase 1 report (no body proofs yet), `full` for a phase 2 report.
    #[serde(default)]
    pub phase: String,
    /// The number of claims that wait for phase 2.
    #[serde(default)]
    pub pending: usize,
    /// The change in the number of explicit panic sites over all steps.
    #[serde(default)]
    pub panic_delta: i64,
    /// The highest estimated API level of all steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_level: Option<String>,
    /// The changed lines of all steps, by claim kind.
    #[serde(default)]
    pub lines: LineCounts,
    /// The removed and added items that look like one changed item.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<PairWarning>,
    /// The build code that the change set changes, if it changes some (design 28).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_code: Option<BuildCodeNote>,
    pub accepted: bool,
}

/// One run of steps whose check uses build code that the change set changes (design 28).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BuildCodeChange {
    /// The step that changes the build code.
    pub step: usize,
    /// The changed parts of the build code, for a message.
    pub parts: String,
    /// The steps whose check uses the changed build code: `step` is the first one.
    pub steps: Vec<usize>,
    /// True if each later step of the change set is in `steps`.
    pub to_end: bool,
}

impl BuildCodeChange {
    /// "from step 3 on", or "in steps 1–3" when a later step has the build code of the base again.
    pub fn span(&self) -> String {
        if self.to_end { format!("from step {} on", self.step) } else { format!("in {}", step_ranges(&self.steps)) }
    }
}

/// The build code state of a result (design 28, invariant I6).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BuildCodeNote {
    pub changes: Vec<BuildCodeChange>,
    /// True if the check ran the changed build code (an agent command, or a reviewer command
    /// with `--trust-build-code`). False: the steps of `changes` are not checked.
    pub ran: bool,
}

impl BuildCodeNote {
    /// The steps that have no check.
    pub fn not_checked(&self) -> Vec<usize> {
        if self.ran { Vec::new() } else { self.changes.iter().flat_map(|change| change.steps.iter().copied()).collect() }
    }

    /// The line of an agent command for a step (A4.1): the full line in the step that changes
    /// the build code, a short form in a later step. `None` if the step uses no changed build code.
    pub fn step_line(&self, step: usize) -> Option<String> {
        let change = self.changes.iter().find(|change| change.steps.contains(&step))?;
        Some(if change.step == step {
            format!(
                "! build code changed in step {step}: {}. It runs during the check and can change each result {}. A reviewer command checks these steps only with --trust-build-code.",
                change.parts,
                change.span()
            )
        } else {
            format!("! build code changed in step {} ({}): a reviewer command checks this step only with --trust-build-code.", change.step, change.parts)
        })
    }

    /// The part of the result line of `tag` (A4.2), with no separator.
    pub fn tag_text(&self) -> String {
        let spans: Vec<String> = self.changes.iter().map(|change| if change.to_end { format!("from step {}", change.step) } else { change.span() }).collect();
        format!("build code changed {}", spans.join(", "))
    }

    /// The line at the top of a result that ran the changed build code with the permission of
    /// the reviewer (A4.4).
    pub fn trusted_line(&self) -> String {
        let parts: Vec<String> = self.changes.iter().map(|change| format!("{} (step {})", change.parts, change.step)).collect();
        let spans: Vec<String> = self.changes.iter().map(BuildCodeChange::span).collect();
        format!("! this result ran the build code that the change set changes (--trust-build-code): {}. That code can change each result {}.", parts.join("; "), spans.join(", "))
    }

    /// The text of a result that is not complete (A4.3), after the result of the checked steps.
    pub fn not_checked_text(&self) -> String {
        let causes: Vec<String> = self.changes.iter().map(|change| format!("build code changed in step {} ({})", change.step, change.parts)).collect();
        let what = if self.changes.len() == 1 { "that change" } else { "these changes" };
        format!("{} not checked: {}. Read {what}. Then run the command again with --trust-build-code.", step_ranges(&self.not_checked()), causes.join("; "))
    }
}

/// A list of step numbers as ranges: `steps 0–2, 5`.
pub fn step_ranges(steps: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut index = 0;
    while index < steps.len() {
        let mut end = index;
        while end + 1 < steps.len() && steps[end + 1] == steps[end] + 1 {
            end += 1;
        }
        out.push(if end == index { steps[index].to_string() } else { format!("{}–{}", steps[index], steps[end]) });
        index = end + 1;
    }
    format!("{} {}", if steps.len() == 1 { "step" } else { "steps" }, out.join(", "))
}

impl Report {
    /// Sets `accepted` and the totals from the step reports.
    pub fn finish(&mut self) {
        // A result with a step that is not checked is not complete: it accepts nothing (I6).
        self.accepted = self.organization.is_empty() && self.steps.iter().all(StepReport::accepted) && !self.not_complete();
        self.panic_delta = self.steps.iter().map(|step| step.panic_delta).sum();
        self.lines = LineCounts::default();
        for step in &self.steps {
            self.lines.add(&step.lines);
        }
        self.pending = self.steps.iter().flat_map(|step| &step.claims).filter(|claim| claim.status.is_pending()).count();
        let rank = |level: &str| match level {
            "major" => 2,
            "minor" => 1,
            _ => 0,
        };
        self.api_level = self.steps.iter().filter_map(|step| step.api_level.clone()).max_by_key(|level| rank(level));
    }

    /// True if a step has no check, because it uses build code that the change set changes and
    /// the reviewer did not allow it (design 28).
    pub fn not_complete(&self) -> bool {
        self.build_code.as_ref().is_some_and(|note| !note.ran && !note.changes.is_empty())
    }

    /// The text of the changed lines by the kind of claim, or `None`. The count has only the
    /// lines that a claim explains. So it shows only for a change set with no fact: with a fact,
    /// the count does not agree with the change (trial 4, D9).
    pub fn lines_text(&self) -> Option<String> {
        (self.lines.total() > 0 && self.steps.iter().all(|step| step.facts.is_empty())).then(|| self.lines.summary())
    }

    /// The counts for the result line (brief 5f, D1): the facts, the facts that phase 2 proved
    /// neutral and that have no spotlight, and the facts with a spotlight. The reviewer reads
    /// each fact that is not proved neutral.
    pub fn fact_counts(&self) -> (usize, usize, usize) {
        let facts = || self.steps.iter().flat_map(|step| &step.facts);
        let neutral = facts().filter(|fact| fact.class == Some(Class::Neutral) && !fact.spotlight).count();
        (facts().count(), neutral, facts().filter(|fact| fact.spotlight).count())
    }
}

/// The title of the list of the bodies that have a forced edit and an equal MIR (brief 5h, C8).
pub const EQUAL_SITES: &str = "bodies with a changed call site and no change of behavior (proved)";

/// The largest number of bodies that a list of the `step` and `status` output shows (brief 5h,
/// C6). The report with `--full` has the full list.
pub const LIST_LIMIT: usize = 6;

/// The kind of a group from its facts and proofs (design 24.3), or `None` while a class is pending.
///
/// - Only identity facts (derived renames and moves): `rename` or `move`.
/// - Only proved neutral facts and claims: `refactor`.
/// - Else: `behavior`.
///
/// A property claim changes no code, so it does not count. A doc fact changes no behavior, so it
/// does not count (trial 4, D9): a rename with its doc comments is not `behavior`.
pub fn derived_kind(steps: &[&StepReport]) -> Option<crate::GroupKind> {
    let all: Vec<&Fact> = steps.iter().flat_map(|s| &s.facts).collect();
    let facts: Vec<&Fact> = all.iter().copied().filter(|f| !f.is_doc()).collect();
    if facts.is_empty() && !all.is_empty() && all.iter().all(|f| f.is_doc()) && steps.iter().all(|s| s.claims.iter().all(|c| c.class.is_none() && c.status.is_verified())) {
        // Only doc facts: no code changed.
        return Some(crate::GroupKind::Refactor);
    }
    let claims: Vec<&ClaimResult> = steps.iter().flat_map(|s| &s.claims).filter(|c| c.class.is_some() || !c.status.is_verified()).collect();
    if facts.is_empty() && claims.is_empty() {
        return None;
    }
    if facts.iter().any(|f| f.class.is_none()) || claims.iter().any(|c| c.status.is_pending()) {
        return None;
    }
    let neutral = facts.iter().all(|f| f.class == Some(Class::Neutral)) && claims.iter().all(|c| c.class == Some(Class::Neutral) && c.status.is_verified());
    if !neutral {
        return Some(crate::GroupKind::Behavior);
    }
    if claims.is_empty() && !facts.is_empty() && facts.iter().all(|f| f.part == "identity") {
        return Some(if facts.iter().all(|f| f.description.starts_with("moved ")) { crate::GroupKind::Move } else { crate::GroupKind::Rename });
    }
    Some(crate::GroupKind::Refactor)
}

/// The kind of a group after one more step: equal kinds stay; neutral kinds give `refactor`; any
/// other mix gives `behavior`.
pub fn merge_kinds(old: crate::GroupKind, new: crate::GroupKind) -> crate::GroupKind {
    let neutral = |k: crate::GroupKind| matches!(k, crate::GroupKind::Rename | crate::GroupKind::Move | crate::GroupKind::Refactor);
    if old == new {
        old
    } else if neutral(old) && neutral(new) {
        crate::GroupKind::Refactor
    } else {
        crate::GroupKind::Behavior
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(step: usize, steps: &[usize], to_end: bool) -> BuildCodeChange {
        BuildCodeChange { step, parts: "crates/core/build.rs".into(), steps: steps.to_vec(), to_end }
    }

    #[test]
    fn the_build_code_lines_of_an_agent_command() {
        // Design 28, A4.1 and A4.2: the full line in the step of the change, a short form later.
        let note = BuildCodeNote { changes: vec![change(3, &[3, 4, 5, 6], true)], ran: true };
        assert_eq!(note.step_line(2), None);
        assert_eq!(
            note.step_line(3).unwrap(),
            "! build code changed in step 3: crates/core/build.rs. It runs during the check and can change each result from step 3 on. A reviewer command checks these steps only with --trust-build-code."
        );
        assert_eq!(note.step_line(5).unwrap(), "! build code changed in step 3 (crates/core/build.rs): a reviewer command checks this step only with --trust-build-code.");
        assert_eq!(note.tag_text(), "build code changed from step 3");
        assert!(note.not_checked().is_empty());
    }

    #[test]
    fn the_build_code_lines_of_a_reviewer_command() {
        // Design 28, A4.3 and A4.4.
        let mut note = BuildCodeNote { changes: vec![change(3, &[3, 4, 5, 6], true)], ran: false };
        assert_eq!(note.not_checked(), vec![3, 4, 5, 6]);
        assert_eq!(
            note.not_checked_text(),
            "steps 3–6 not checked: build code changed in step 3 (crates/core/build.rs). Read that change. Then run the command again with --trust-build-code."
        );
        note.ran = true;
        assert_eq!(
            note.trusted_line(),
            "! this result ran the build code that the change set changes (--trust-build-code): crates/core/build.rs (step 3). That code can change each result from step 3 on."
        );
        // Case 7: the build code goes back to the base in step 3.
        let note = BuildCodeNote { changes: vec![change(1, &[1, 2, 3], false)], ran: false };
        assert_eq!(note.not_checked(), vec![1, 2, 3]);
        assert!(note.not_checked_text().starts_with("steps 1–3 not checked: build code changed in step 1 (crates/core/build.rs)."));
        assert_eq!(note.tag_text(), "build code changed in steps 1–3");
    }

    #[test]
    fn a_result_with_a_step_that_is_not_checked_is_not_complete_and_not_accepted() {
        let mut report = Report::default();
        assert!(!report.not_complete());
        report.build_code = Some(BuildCodeNote { changes: vec![change(0, &[0], true)], ran: true });
        assert!(!report.not_complete(), "an agent result ran the build code: it is complete");
        report.build_code = Some(BuildCodeNote { changes: vec![change(0, &[0], true)], ran: false });
        assert!(report.not_complete());
        assert_eq!(step_ranges(&[0, 1, 2, 5]), "steps 0–2, 5");
        assert_eq!(step_ranges(&[4]), "step 4");
    }
}
