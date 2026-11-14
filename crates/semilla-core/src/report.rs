//! Verification results.

use serde::{Deserialize, Serialize};

use crate::anchor::Anchor;
use crate::ops::{Category, Compare};

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
    /// Evidence of a false claim.
    Refuted { reason: String },
    /// The claim explains no difference.
    Empty,
    /// Bad syntax, or the anchor does not resolve.
    Malformed { reason: String },
    /// The body proof waits for the background check (phase 2, design section 19).
    Pending { reason: String },
    /// No check ran: the step uses build code that the change set changes, and the command had
    /// `--no-build-code` (design 28).
    NotChecked { reason: String },
}

impl ClaimStatus {
    pub fn is_verified(&self) -> bool {
        matches!(self, ClaimStatus::Verified { .. })
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, ClaimStatus::Pending { .. })
    }

    /// The text form, for example "verified (mir)" or "refuted: …".
    pub fn text(&self) -> String {
        match self {
            ClaimStatus::Verified { proof } => format!("verified ({proof})"),
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
    /// The category of the change of the claim. A property claim has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
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
    /// The mark of design 28 (invariant I6): the check of this claim ran the build code that
    /// step N changes. Each output shows [`build_code_mark`] with a verified claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_code: Option<usize>,
}

impl ClaimResult {
    /// The mark for an output, with a space before it. It is empty for a claim with no proof,
    /// and for a proof that used only the build code of the base.
    pub fn mark(&self) -> String {
        match self.build_code {
            Some(step) if self.status.is_verified() => format!(" {}", build_code_mark(step)),
            _ => String::new(),
        }
    }
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
    /// The notes for the agent. A note does not reject the step.
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
    /// The category (design 35): the kind of change that the text shows. A record with no
    /// category is a record of an older Semilla: it gives an error.
    pub category: Category,
    /// The compare result (design 35): an observation of the compiled code. It is no verdict.
    pub compare: Compare,
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
    /// The key that the agent gave for the reason, when it is the path of the item and not the
    /// exact key of this fact (batch 5x, part D).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_key: Option<String>,
    /// The forced edits at other sites, grouped by the inserted or removed text (design 22.6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<SiteGroup>,
    /// The content change of a file fact: a line diff with a size limit, or "binary, N bytes".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// The bodies with a site of the fact whose MIR is equal. They are not in
    /// `affected`. So each body of a site is in one of the two lists (trial 4, D14).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub equal_sites: Vec<String>,
    /// A note for the reader: the cause of the result `no compare`, the checked configurations
    /// with equal and with different compiled code, a build condition that Semilla did not
    /// check (design 35.5). The note asks for no reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The mark of design 28 (invariant I6): the check of this fact ran the build code that
    /// step N changes. Each output shows [`build_code_mark`] with a result `code equal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_code: Option<usize>,
    /// True if the fact is a change of build code (design 28). Each output shows such a fact
    /// first in its step, with its diff.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub build_code_change: bool,
}

impl Fact {
    /// The label of the reason in each output. It has the key of the agent, when the key is
    /// the path of the item and not the exact key of the fact (batch 5x, part D).
    pub fn reason_label(&self) -> String {
        match &self.reason_key {
            Some(key) => format!("reason (key `{key}`)"),
            None => "reason".to_string(),
        }
    }

    /// The exact key of the reason of this fact: its anchor, or its part.
    pub fn reason_place(&self) -> String {
        self.at.as_ref().map(|at| at.to_string()).unwrap_or_else(|| self.part.clone())
    }

    /// The text in the brackets of the first line of the fact: "rename · code equal".
    pub fn label(&self) -> String {
        format!("{} · {}", self.category.name(), self.compare.name())
    }

    /// True if the fact has the mismatch mark (design 35.2): the text of the change gives no
    /// cause for different compiled code, and the compiled code is different.
    pub fn mismatch(&self) -> bool {
        self.category.expects_equal() && self.compare == Compare::CodeDifferent
    }

    /// The place of the fact in a group of the review report (design 35.7): a spotlight, a
    /// mismatch mark, then the compare results.
    pub fn order(&self) -> u8 {
        if self.spotlight {
            0
        } else if self.mismatch() {
            1
        } else {
            2 + self.compare.order()
        }
    }

    /// The mark for the first line of the fact in an output, with a space before it. Only the
    /// result `code equal` has it. It is empty for a compare that used only the build code of
    /// the base.
    pub fn mark(&self) -> String {
        match self.build_code {
            Some(step) if self.compare == Compare::CodeEqual => format!(" {}", build_code_mark(step)),
            _ => String::new(),
        }
    }

    /// The mark for a part of the fact that shows equal compiled code: the list of the bodies
    /// with an equal MIR, and the note of the configurations. It is empty for a compare that
    /// used only the build code of the base.
    pub fn part_mark(&self) -> String {
        self.build_code.map(|step| format!(" {}", build_code_mark(step))).unwrap_or_default()
    }

    /// True if the description names each place of a site group (a fold of equal type changes:
    /// "type `u32` → `u64` in 8 places: …"). A view does not show the sites of the group again.
    /// True for a fact of a doc comment change with no doc test. A doc comment has no MIR.
    pub fn is_doc(&self) -> bool {
        self.category == Category::Docs && self.part == "edit" && ["changed docs", "added docs", "removed docs"].iter().any(|start| self.description.starts_with(start))
    }

    /// True for the fact of a derived rename or move that changes the public API (design 31.8).
    /// The fact is text for the reviewer about the identity fact: it is no second change.
    pub fn is_api_note(&self) -> bool {
        self.part == "public api"
    }

    pub fn names_sites(&self, group: &SiteGroup) -> bool {
        self.description.starts_with(&format!("{} in ", group.edit))
    }

    /// True if the fact is the fold of the sites of the group: its description has the count
    /// and the text of the group ("3 site(s): changed path `a` → `b`"). A view shows the text
    /// one time, then the anchors (batch 5x, part E).
    pub fn folds(&self, group: &SiteGroup) -> bool {
        ["site(s)", "bodies"].iter().any(|what| self.description.starts_with(&format!("{} {what}: {}", group.sites.len(), group.edit)))
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
    /// The cause that stopped the derivation of the rename or the move, when the step of the pair has
    /// no other change (batch 5x, part C). The advice "own step" is then not true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
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
        let what = if parent(&self.old) == parent(&self.new) && last(&self.old) != last(&self.new) { "name" } else { "place" };
        let mut text = format!(
            "`{}` (removed) and `{}` (added) are {:.0}% similar: they can be one item with a new {what} and a second change. Semilla derives a rename or a move only in a step with no other change",
            self.old,
            self.new,
            self.similarity * 100.0
        );
        if let Some(cause) = &self.stopped {
            // The step has no second change: a new step gives the same result.
            text = format!(
                "`{}` (removed) and `{}` (added) are {:.0}% similar, and their step has no other change. Semilla does not derive one item: {cause}",
                self.old,
                self.new,
                self.similarity * 100.0
            );
        }
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
    /// `syntax` for a phase 1 report (no compare yet), `full` for a phase 2 report.
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
    /// The build configurations that the body checks used (design 8.12). `None` for a report
    /// with no body check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configs: Option<ConfigNote>,
    pub accepted: bool,
}

/// The build configurations of the body checks of a change set (design 8.12; batch 5s, B1).
///
/// A proof of Semilla holds in each checked configuration, and only there. Each output for the
/// reviewer shows this data near the base line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigNote {
    /// The target triple of the check build.
    pub target: String,
    /// The names of the configurations that Semilla checked in each step.
    pub checked: Vec<String>,
    /// Each configuration of the default list that Semilla did not check in a step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_checked: Vec<ConfigGap>,
    /// The `cfg` conditions of the source text that the checked configurations do not cover.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_covered: Vec<String>,
    /// The checked configurations that Semilla added for a joint state (batch 5u, part C).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<ConfigAdded>,
    /// The configurations of the default list that Semilla checked only in some steps (batch
    /// 5y, C4.4). In each other step, a different configuration has the same feature state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub in_steps: Vec<ConfigAdded>,
    /// The configurations of the default list that are not in the list of a step: a different
    /// configuration has the same feature state (batch 5y, C4.4). Only the `dev` names: the
    /// `release` configurations have the same relation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub same: Vec<ConfigSame>,
}

/// A configuration that is not in the list of a step, because a different configuration has
/// the same feature state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigSame {
    /// The name of the configuration.
    pub name: String,
    /// The name of the checked configuration with the same feature state.
    pub same_as: String,
    /// The steps where the two are the same.
    pub steps: Vec<usize>,
}

/// A checked configuration that Semilla added for a joint state: a compare or a claim of a step needs a feature
/// state that no configuration of the default list has.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigAdded {
    /// The name of the configuration.
    pub name: String,
    /// The steps with this configuration. Empty: each compared step has it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<usize>,
}

/// A configuration of the default list that Semilla did not check.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigGap {
    /// The name of the configuration.
    pub name: String,
    /// The cause, with the first error line of the build.
    pub cause: String,
    /// The steps with no check in this configuration.
    pub steps: Vec<usize>,
}

impl ConfigGap {
    /// The kind of the cause: its text before the compiler error. The first error line of a
    /// build can be different in each run, because cargo compiles the targets in parallel.
    pub fn kind(&self) -> &str {
        self.cause.split(": error").next().unwrap_or(&self.cause)
    }
}

impl ConfigNote {
    /// The highest number of conditions that a line shows.
    pub const MOST_CONDITIONS: usize = 8;

    /// The end of the name of a configuration with a build of the libraries and the binaries
    /// only. The text is the text of `Configuration::NARROW` in `semilla-normal`.
    pub const NARROW: &'static str = " (libraries and binaries only)";

    /// The line of the checked configurations and the target.
    pub fn checked_line(&self) -> String {
        // The name of a smaller build ends with a note (batch 5t, part C).
        let show = |name: &String| match name.strip_suffix(Self::NARROW) {
            Some(short) => format!("`{short}` (libraries and binaries only: the other targets do not build)"),
            None => format!("`{name}`"),
        };
        // An added configuration has a mark (batch 5u, part C). A configuration that only
        // some steps have is not in `checked`: the mark names the steps.
        let mark = |added: &ConfigAdded| match added.steps.len() {
            0 => " (added for a joint state)".to_string(),
            1 => format!(" (added for a joint state in step {})", added.steps[0]),
            _ => format!(" (added for a joint state in steps {})", added.steps.iter().map(|step| step.to_string()).collect::<Vec<_>>().join(", ")),
        };
        let mut list: Vec<String> = self.checked.iter().map(|name| format!("{}{}", show(name), self.added.iter().find(|added| added.name == *name).map(mark).unwrap_or_default())).collect();
        list.extend(self.added.iter().filter(|added| !self.checked.contains(&added.name)).map(|added| format!("{}{}", show(&added.name), mark(added))));
        // A configuration of the default list that only some steps have (batch 5y, C4.4).
        let steps = |entry: &ConfigAdded| match entry.steps.len() {
            1 => format!(" (only in step {})", entry.steps[0]),
            _ => format!(" (only in steps {})", entry.steps.iter().map(|step| step.to_string()).collect::<Vec<_>>().join(", ")),
        };
        list.extend(self.in_steps.iter().map(|entry| format!("{}{}", show(&entry.name), steps(entry))));
        let list = if list.is_empty() { "none".to_string() } else { list.join(", ") };
        format!("checked configurations (target `{}`): {list}", self.target)
    }

    /// The lines of the configurations that Semilla did not check: one line for each cause.
    /// `all_steps`: the number of steps of the change set. A line starts with `!`: it is a
    /// warning.
    pub fn gap_lines(&self, all_steps: usize) -> Vec<String> {
        // The configurations with the same kind of cause in the same steps go into one line.
        // The line shows the compiler error of the first one.
        let mut groups: Vec<(&ConfigGap, Vec<&str>)> = Vec::new();
        for gap in &self.not_checked {
            match groups.iter_mut().find(|(first, _)| first.kind() == gap.kind() && first.steps == gap.steps) {
                Some((_, names)) => names.push(&gap.name),
                None => groups.push((gap, vec![&gap.name])),
            }
        }
        groups
            .into_iter()
            .map(|(gap, names)| {
                let steps: Vec<String> = gap.steps.iter().map(|step| step.to_string()).collect();
                let place = match gap.steps.len() {
                    count if count >= all_steps => String::new(),
                    1 => format!(" in step {}", steps[0]),
                    _ => format!(" in steps {}", steps.join(", ")),
                };
                let list: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
                let (noun, verb) = if names.len() == 1 { ("configuration", "is") } else { ("configurations", "are") };
                format!("! {noun} {} {verb} not checked{place}: {}", list.join(", "), gap.cause)
            })
            .collect()
    }

    /// The line of the configurations that Semilla added for a joint state, if one exists: the line
    /// of `step`.
    pub fn added_line(&self) -> Option<String> {
        if self.added.is_empty() {
            return None;
        }
        let list: Vec<String> = self.added.iter().map(|added| format!("`{}`", added.name)).collect();
        let noun = if list.len() == 1 { "configuration" } else { "configurations" };
        Some(format!("Semilla added the {noun} {} for a joint state", list.join(", ")))
    }

    /// The line of the conditions that the checked configurations do not cover, if one exists.
    pub fn condition_line(&self) -> Option<String> {
        if self.not_covered.is_empty() {
            return None;
        }
        let shown: Vec<String> = self.not_covered.iter().take(Self::MOST_CONDITIONS).map(|text| format!("`{text}`")).collect();
        let more = match self.not_covered.len().saturating_sub(Self::MOST_CONDITIONS) {
            0 => String::new(),
            count => format!(", and {count} more"),
        };
        Some(format!("! `cfg` conditions that Semilla did not check: {}{more}", shown.join(", ")))
    }

    /// The line of the conditions for an agent command (batch 5x, part E). The short line has
    /// the count and the first condition. The line of `--verbose` has each condition.
    pub fn agent_condition_line(&self, verbose: bool) -> Option<String> {
        let first = self.not_covered.first()?;
        let count = self.not_covered.len();
        if verbose {
            let all: Vec<String> = self.not_covered.iter().map(|text| format!("`{text}`")).collect();
            return Some(format!("! `cfg` conditions that Semilla did not check: {}", all.join(", ")));
        }
        if count == 1 {
            return self.condition_line();
        }
        Some(format!("! {count} `cfg` conditions that Semilla did not check: `{first}`, and {} more (`semilla status --verbose` shows each one)", count - 1))
    }

    /// The line of the configurations that are not in the list, because a different
    /// configuration has the same feature state (batch 5y, C4.4), if one exists.
    /// `all_steps`: the number of steps of the change set.
    pub fn same_line(&self, all_steps: usize) -> Option<String> {
        if self.same.is_empty() {
            return None;
        }
        let list: Vec<String> = self
            .same
            .iter()
            .map(|same| {
                let place = match same.steps.len() {
                    count if count >= all_steps => String::new(),
                    1 => format!(" in step {}", same.steps[0]),
                    _ => format!(" in steps {}", same.steps.iter().map(|step| step.to_string()).collect::<Vec<_>>().join(", ")),
                };
                format!("`{}` = `{}`{place}", same.name, same.same_as)
            })
            .collect();
        Some(format!("not in the list, because the features are the same (also for `release`): {}", list.join(", ")))
    }

    /// The lines at the top of an output for the reviewer. The last line gives the scope of
    /// each compare result (design 35.5).
    pub fn lines(&self, all_steps: usize) -> Vec<String> {
        let mut lines = vec![self.checked_line()];
        lines.extend(self.same_line(all_steps));
        lines.extend(self.gap_lines(all_steps));
        lines.extend(self.condition_line());
        lines.push(CONFIG_SCOPE.to_string());
        lines
    }

    /// The lines at the top of an output of an agent command (batch 5x, part E).
    pub fn agent_lines(&self, all_steps: usize, verbose: bool) -> Vec<String> {
        let mut lines = vec![self.checked_line()];
        lines.extend(self.same_line(all_steps));
        lines.extend(self.gap_lines(all_steps));
        lines.extend(self.agent_condition_line(verbose));
        lines.push(CONFIG_SCOPE.to_string());
        lines
    }
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

/// The mark of a result that used build code that step `step` changes (design 28, invariant I6).
pub fn build_code_mark(step: usize) -> String {
    format!("(with the build code of step {step})")
}

/// The part of a count of results that says how many have the mark, or an empty text. `steps`
/// are the steps of the marks.
pub fn count_mark(marked: usize, steps: &[usize]) -> String {
    match (marked, steps) {
        (0, _) => String::new(),
        (_, [step]) => format!(" ({marked} with the build code of step {step})"),
        _ => format!(" ({marked} with changed build code)"),
    }
}

/// The title of the section of a step that shows the build code changes of the step.
pub const BUILD_CODE_SECTION: &str = "build code changed in this step; read this change first";

/// The build code state of a result (design 28, invariant I6).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BuildCodeNote {
    pub changes: Vec<BuildCodeChange>,
    /// True if the check ran the changed build code in the sandbox (the default). False: the
    /// command had `--no-build-code`, and the steps of `changes` are not checked.
    pub ran: bool,
}

impl BuildCodeNote {
    /// The steps that have no check.
    pub fn not_checked(&self) -> Vec<usize> {
        if self.ran { Vec::new() } else { self.changes.iter().flat_map(|change| change.steps.iter().copied()).collect() }
    }

    /// The step whose build code change the check of `step` used, if the check ran it.
    pub fn mark_of(&self, step: usize) -> Option<usize> {
        if !self.ran {
            return None;
        }
        self.changes.iter().find(|change| change.steps.contains(&step)).map(|change| change.step)
    }

    /// The line of `step` for a step: the full line in the step that changes the build code, a
    /// short form in a later step. `None` if the step uses no changed build code.
    pub fn step_line(&self, step: usize) -> Option<String> {
        self.step_line_with(step, None)
    }

    /// [`Self::step_line`] for a process whose builds have a missing limit of the sandbox
    /// (batch 5w, D5). `missing` is the state, for example "the network limit is missing
    /// (the cause)". The line then does not say that the code runs in the sandbox.
    pub fn step_line_with(&self, step: usize, missing: Option<&str>) -> Option<String> {
        let change = self.changes.iter().find(|change| change.steps.contains(&step))?;
        let runs = match missing {
            None => "It runs in the sandbox during each check".to_string(),
            Some(missing) => format!("This check ran it with a sandbox that is not complete: {missing}. It runs during each check"),
        };
        Some(if change.step == step {
            format!(
                "! build code changed in step {step}: {}. {runs}, and it can change each result {}. Each result `code equal` and each verified claim that depends on it has the mark {}.",
                change.parts,
                change.span(),
                build_code_mark(step)
            )
        } else {
            format!("! build code changed in step {} ({}): each result `code equal` and each verified claim of this step has the mark {}.", change.step, change.parts, build_code_mark(change.step))
        })
    }

    /// The part of the result line, with no separator.
    pub fn tag_text(&self) -> String {
        let spans: Vec<String> = self.changes.iter().map(|change| if change.to_end { format!("from step {}", change.step) } else { change.span() }).collect();
        format!("build code changed {}", spans.join(", "))
    }

    /// The line at the top of a result that ran the changed build code.
    pub fn ran_line(&self) -> String {
        self.ran_line_with(None)
    }

    /// [`Self::ran_line`] for a process whose builds have a missing limit of the sandbox
    /// (batch 5w, D5). The line then does not say "in the sandbox": it names the missing limits.
    pub fn ran_line_with(&self, missing: Option<&str>) -> String {
        let ran = match missing {
            None => "The check ran it in the sandbox.".to_string(),
            Some(missing) => format!("The check ran it with a sandbox that is not complete: {missing}."),
        };
        let parts: Vec<String> = self.changes.iter().map(|change| format!("step {} ({})", change.step, change.parts)).collect();
        let spans: Vec<String> = self.changes.iter().map(BuildCodeChange::span).collect();
        let marks: Vec<String> = self.changes.iter().map(|change| build_code_mark(change.step)).collect();
        format!(
            "! build code changed in {}. {ran} It can change each result {}. Read that change first. Each result `code equal` and each verified claim that depends on it has the mark {}.",
            parts.join("; "),
            spans.join(", "),
            marks.join(" or ")
        )
    }

    /// The text of a result that is not complete, after the result of the checked steps.
    pub fn not_checked_text(&self) -> String {
        let causes: Vec<String> = self.changes.iter().map(|change| format!("build code changed in step {} ({})", change.step, change.parts)).collect();
        format!("{} not checked: {}. The command does not run it (the option --no-build-code, or a build sandbox that is not complete).", step_ranges(&self.not_checked()), causes.join("; "))
    }

    /// The part of a count of `code equal` facts that says how many have the mark, or an empty text.
    pub fn count_text(&self, marked: usize) -> String {
        if marked == 0 {
            return String::new();
        }
        match self.changes.as_slice() {
            [change] => format!(" ({marked} with the build code of step {})", change.step),
            _ => format!(" ({marked} with changed build code)"),
        }
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
        // A result with a step that is not checked is not complete: it accepts nothing.
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
    /// the command had `--no-build-code` (design 28).
    pub fn not_complete(&self) -> bool {
        self.build_code.as_ref().is_some_and(|note| !note.ran && !note.changes.is_empty())
    }

    /// The text of the changed lines by the kind of claim, or `None`. The count has only the
    /// lines that a claim explains. So it shows only for a change set with no fact: with a fact,
    /// the count does not agree with the change (trial 4, D9).
    pub fn lines_text(&self) -> Option<String> {
        (self.lines.total() > 0 && self.steps.iter().all(|step| step.facts.is_empty())).then(|| self.lines.summary())
    }

    /// The counts for the result line and for the summary table (design 35.6).
    pub fn fact_counts(&self) -> FactCounts {
        FactCounts::of(self.steps.iter().flat_map(|step| &step.facts))
    }

    /// The count of the facts with the result `code equal` that have the mark of changed
    /// build code (design 28).
    pub fn marked_count(&self) -> usize {
        self.steps.iter().flat_map(|step| &step.facts).filter(|fact| fact.compare == Compare::CodeEqual && fact.build_code.is_some()).count()
    }

    /// The facts part of the result line: "40 facts: 29 code different, 5 code equal (1 with
    /// a spotlight)". The count of `code equal` has the mark of changed build code.
    pub fn facts_text(&self) -> String {
        self.fact_counts().text(&self.marked_text(self.marked_count()))
    }

    /// The text of [`BuildCodeNote::count_text`] for a count of marked facts.
    pub fn marked_text(&self, marked: usize) -> String {
        self.build_code.as_ref().map(|note| note.count_text(marked)).unwrap_or_default()
    }
}

/// The title of the list of the bodies that have a forced edit and an equal MIR (brief 5h, C8).
pub const EQUAL_SITES: &str = "bodies with a changed call site and equal compiled code";

/// The start of the line of the mismatch mark (design 35.2).
pub const MISMATCH: &str = "! the compiled code changes:";

/// The line that each report shows one time (design 35.5).
pub const CONFIG_SCOPE: &str = "Each compare result holds for the checked configurations.";

/// The counts of the facts of a result (design 35.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FactCounts {
    pub total: usize,
    /// The count of each compare result, in the order of the result line. A count of zero is
    /// not in the list.
    pub compare: Vec<(Compare, usize)>,
    /// The count of each category, in the order of the summary table. A count of zero is not
    /// in the list.
    pub category: Vec<(Category, usize)>,
    pub spotlight: usize,
    pub mismatch: usize,
}

impl FactCounts {
    pub fn of<'a>(facts: impl Iterator<Item = &'a Fact>) -> FactCounts {
        let facts: Vec<&Fact> = facts.collect();
        let count = |test: &dyn Fn(&Fact) -> bool| facts.iter().filter(|fact| test(fact)).count();
        FactCounts {
            total: facts.len(),
            compare: Compare::ALL.iter().map(|&compare| (compare, count(&|fact| fact.compare == compare))).filter(|(_, count)| *count > 0).collect(),
            category: Category::ALL.iter().map(|&category| (category, count(&|fact| fact.category == category))).filter(|(_, count)| *count > 0).collect(),
            spotlight: count(&|fact| fact.spotlight),
            mismatch: count(&|fact| fact.mismatch()),
        }
    }

    /// The count of one compare result.
    pub fn count(&self, compare: Compare) -> usize {
        self.compare.iter().find(|(found, _)| *found == compare).map(|(_, count)| *count).unwrap_or(0)
    }

    /// The facts part of the result line. `marked` is the text of the build code mark for the
    /// count of `code equal`, or an empty text.
    pub fn text(&self, marked: &str) -> String {
        let noun = if self.total == 1 { "fact" } else { "facts" };
        if self.total == 0 {
            return format!("0 {noun}");
        }
        let parts: Vec<String> = self.compare.iter().map(|(compare, count)| format!("{count} {}{}", compare.name(), if *compare == Compare::CodeEqual { marked } else { "" })).collect();
        // A count of zero is not in the line.
        let mut marks = Vec::new();
        if self.spotlight > 0 {
            marks.push(format!("{} with a spotlight", self.spotlight));
        }
        if self.mismatch > 0 {
            marks.push(format!("{} with a mismatch mark", self.mismatch));
        }
        let marks = if marks.is_empty() { String::new() } else { format!(" ({})", marks.join(", ")) };
        format!("{} {noun}: {}{marks}", self.total, parts.join(", "))
    }
}

/// The largest number of bodies that a list shows with each name (batch 5x, part E). A longer
/// list shows the count and the scope ([`list_scope`]). `semilla status --verbose` and the
/// report with `--full` have each name.
pub const LIST_LIMIT: usize = 8;

/// The scope of a long list of bodies (batch 5x, part E): "the crate `humantime`", or
/// "2 crates: `a` (30), `b` (15)". The crate is the first segment of the name of a body.
pub fn list_scope(bodies: &[String]) -> String {
    let mut crates: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for body in bodies {
        let name = body.trim_start_matches(['<', '&']);
        *crates.entry(name.split("::").next().unwrap_or(name)).or_default() += 1;
    }
    match crates.len() {
        1 => format!("the crate `{}`", crates.keys().next().copied().unwrap_or_default()),
        count => format!("{count} crates: {}", crates.iter().map(|(name, count)| format!("`{name}` ({count})")).collect::<Vec<_>>().join(", ")),
    }
}

/// The kind of a group from its facts (design 35.7), or `None` while a compare is pending.
///
/// - Each fact is a derived rename: `rename`. Each fact is a derived move: `move`.
/// - Each fact has the result `code equal` or `no code`: `refactor`. The kind is a label of
///   the compare results. It is no verdict.
/// - Else: `behavior`.
///
/// A property claim changes no code, so it does not count. A doc fact has no code, so it does
/// not count (trial 4, D9): a rename with its doc comments is a `rename`. The API fact of a
/// rename or a move does not count: a rename of a public item is a rename.
pub fn derived_kind(steps: &[&StepReport]) -> Option<crate::GroupKind> {
    let all: Vec<&Fact> = steps.iter().flat_map(|s| &s.facts).filter(|f| !f.is_api_note()).collect();
    let facts: Vec<&Fact> = all.iter().copied().filter(|f| !f.is_doc()).collect();
    let claims: Vec<&ClaimResult> = steps.iter().flat_map(|s| &s.claims).filter(|c| c.category.is_some() || !c.status.is_verified()).collect();
    if all.is_empty() && claims.is_empty() {
        return None;
    }
    if facts.iter().any(|f| matches!(f.compare, Compare::Pending | Compare::NotChecked)) || claims.iter().any(|c| c.status.is_pending()) {
        return None;
    }
    if claims.is_empty() && !facts.is_empty() && facts.iter().all(|f| f.part == "identity") {
        return Some(if facts.iter().all(|f| f.category == Category::Move) { crate::GroupKind::Move } else { crate::GroupKind::Rename });
    }
    if !claims.is_empty() || facts.iter().any(|f| !matches!(f.compare, Compare::CodeEqual | Compare::NoCode)) {
        return Some(crate::GroupKind::Behavior);
    }
    Some(crate::GroupKind::Refactor)
}

/// The kind of a group after one more step: equal kinds stay; the kinds `rename`, `move`, and
/// `refactor` give `refactor`; any other mix gives `behavior`.
pub fn merge_kinds(old: crate::GroupKind, new: crate::GroupKind) -> crate::GroupKind {
    let equal_code = |k: crate::GroupKind| matches!(k, crate::GroupKind::Rename | crate::GroupKind::Move | crate::GroupKind::Refactor);
    if old == new {
        old
    } else if equal_code(old) && equal_code(new) {
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
    fn the_build_code_lines_of_the_step_command() {
        // Design 28.4: the full line in the step of the change, a short form later.
        let note = BuildCodeNote { changes: vec![change(3, &[3, 4, 5, 6], true)], ran: true };
        assert_eq!(note.step_line(2), None);
        assert_eq!(
            note.step_line(3).unwrap(),
            "! build code changed in step 3: crates/core/build.rs. It runs in the sandbox during each check, and it can change each result from step 3 on. Each result `code equal` and each verified claim that depends on it has the mark (with the build code of step 3)."
        );
        assert_eq!(note.step_line(5).unwrap(), "! build code changed in step 3 (crates/core/build.rs): each result `code equal` and each verified claim of this step has the mark (with the build code of step 3).");
        assert_eq!(note.tag_text(), "build code changed from step 3");
        assert!(note.not_checked().is_empty());
        assert_eq!((note.mark_of(2), note.mark_of(3), note.mark_of(6)), (None, Some(3), Some(3)));
        assert_eq!(note.count_text(0), "");
        assert_eq!(note.count_text(2), " (2 with the build code of step 3)");
        // The count of the verified claims has the same text.
        assert_eq!((count_mark(0, &[3]), count_mark(2, &[3]), count_mark(2, &[1, 3])), (String::new(), " (2 with the build code of step 3)".to_string(), " (2 with changed build code)".to_string()));
    }

    #[test]
    fn the_build_code_lines_of_a_result() {
        // Design 28.4: the result with `--no-build-code`, and the default result.
        let mut note = BuildCodeNote { changes: vec![change(3, &[3, 4, 5, 6], true)], ran: false };
        assert_eq!(note.not_checked(), vec![3, 4, 5, 6]);
        assert_eq!(note.mark_of(4), None, "a step with no check has no mark");
        assert_eq!(note.not_checked_text(), "steps 3–6 not checked: build code changed in step 3 (crates/core/build.rs). The command does not run it (the option --no-build-code, or a build sandbox that is not complete).");
        note.ran = true;
        // Batch 5w, D5: with a missing limit, the two lines do not say "in the sandbox".
        let missing = "the network limit is missing (a test)";
        assert_eq!(
            note.ran_line_with(Some(missing)),
            "! build code changed in step 3 (crates/core/build.rs). The check ran it with a sandbox that is not complete: the network limit is missing (a test). It can change each result from step 3 on. Read that change first. Each result `code equal` and each verified claim that depends on it has the mark (with the build code of step 3)."
        );
        assert!(note.step_line_with(3, Some(missing)).unwrap().starts_with("! build code changed in step 3: crates/core/build.rs. This check ran it with a sandbox that is not complete: the network limit is missing (a test). It runs during each check, and it can change each result from step 3 on."));
        assert_eq!(note.step_line_with(3, None), note.step_line(3));
        assert_eq!(
            note.ran_line(),
            "! build code changed in step 3 (crates/core/build.rs). The check ran it in the sandbox. It can change each result from step 3 on. Read that change first. Each result `code equal` and each verified claim that depends on it has the mark (with the build code of step 3)."
        );
        // Case 7: the build code goes back to the base in step 3.
        let note = BuildCodeNote { changes: vec![change(1, &[1, 2, 3], false)], ran: false };
        assert_eq!(note.not_checked(), vec![1, 2, 3]);
        assert!(note.not_checked_text().starts_with("steps 1–3 not checked: build code changed in step 1 (crates/core/build.rs)."));
        assert_eq!(note.tag_text(), "build code changed in steps 1–3");
        // Two changes: the count names no step.
        let note = BuildCodeNote { changes: vec![change(1, &[1, 2], false), change(4, &[4], true)], ran: true };
        assert_eq!((note.mark_of(2), note.mark_of(3), note.mark_of(4)), (Some(1), None, Some(4)));
        assert_eq!(note.count_text(3), " (3 with changed build code)");
    }

    #[test]
    fn the_mark_is_on_each_equal_result_of_a_fact_and_on_each_verified_claim() {
        // Invariant I6: a result `code equal` that used changed build code has the mark.
        let mut fact = Fact { category: Category::Logic, compare: Compare::CodeEqual, ..Default::default() };
        assert_eq!((fact.mark(), fact.part_mark()), (String::new(), String::new()), "a compare with the build code of the base has no mark");
        fact.build_code = Some(2);
        assert_eq!(fact.mark(), " (with the build code of step 2)");
        assert_eq!(fact.part_mark(), " (with the build code of step 2)");
        // A fact with a different result has no mark in its first line.
        fact.compare = Compare::CodeDifferent;
        assert_eq!(fact.mark(), "");
        assert_eq!(fact.label(), "logic · code different");
        assert!(!fact.mismatch());
        fact.category = Category::Visibility;
        assert!(fact.mismatch(), "the text of a visibility change gives no cause for different code");
        assert_eq!(fact.order(), 1);
        let mut claim = ClaimResult {
            claim: "c1".into(),
            step: 2,
            status: ClaimStatus::Verified { proof: "mir".into() },
            category: None,
            effect: None,
            witness: None,
            hint: None,
            before: None,
            after: None,
            affected: Vec::new(),
            complete_calls: Vec::new(),
            build_code: Some(2),
        };
        assert_eq!(claim.mark(), " (with the build code of step 2)");
        claim.status = ClaimStatus::Refuted { reason: "no".into() };
        assert_eq!(claim.mark(), "", "a claim that is not verified has no mark");
    }

    #[test]
    fn the_facts_part_of_the_result_line_leaves_out_a_count_of_zero() {
        let fact = |category, compare, spotlight| Fact { category, compare, spotlight, ..Default::default() };
        let facts = vec![
            fact(Category::Logic, Compare::CodeDifferent, false),
            fact(Category::Value, Compare::CodeDifferent, true),
            fact(Category::Rename, Compare::CodeEqual, false),
            fact(Category::Docs, Compare::NoCode, false),
        ];
        let counts = FactCounts::of(facts.iter());
        assert_eq!(counts.text(""), "4 facts: 2 code different, 1 code equal, 1 no code (1 with a spotlight)");
        assert_eq!(counts.text(" (1 with the build code of step 3)"), "4 facts: 2 code different, 1 code equal (1 with the build code of step 3), 1 no code (1 with a spotlight)");
        assert_eq!(counts.category, vec![(Category::Rename, 1), (Category::Value, 1), (Category::Logic, 1), (Category::Docs, 1)]);
        assert_eq!(FactCounts::of([].iter()).text(""), "0 facts");
        let one = vec![fact(Category::Import, Compare::CodeDifferent, false)];
        assert_eq!(FactCounts::of(one.iter()).text(""), "1 fact: 1 code different (1 with a mismatch mark)");
    }

    #[test]
    fn an_older_record_gives_an_error() {
        // A fact of an older Semilla has `class` and no `category`.
        let old = r#"{"id":"f1","part":"edit","description":"x","status":"derived","class":"neutral"}"#;
        let error = serde_json::from_str::<Fact>(old).unwrap_err().to_string();
        assert!(error.contains("missing field `category`"), "{error}");
    }

    #[test]
    fn a_result_with_a_step_that_is_not_checked_is_not_complete_and_not_accepted() {
        let mut report = Report::default();
        assert!(!report.not_complete());
        report.build_code = Some(BuildCodeNote { changes: vec![change(0, &[0], true)], ran: true });
        assert!(!report.not_complete(), "the result ran the build code: it is complete");
        report.build_code = Some(BuildCodeNote { changes: vec![change(0, &[0], true)], ran: false });
        assert!(report.not_complete());
        assert_eq!(step_ranges(&[0, 1, 2, 5]), "steps 0–2, 5");
        assert_eq!(step_ranges(&[4]), "step 4");
    }
}

#[cfg(test)]
mod config_note_tests {
    use super::*;

    #[test]
    fn the_lines_of_the_configurations_name_each_gap_and_at_most_eight_conditions() {
        let gap = |name: &str, steps: &[usize]| ConfigGap { name: name.into(), cause: "it does not build before the step: error: no".into(), steps: steps.to_vec() };
        let note = ConfigNote {
            target: "x86_64-unknown-linux-gnu".into(),
            checked: vec!["dev".into(), "release".into()],
            not_checked: vec![gap("dev, all features", &[0, 1]), gap("release, all features", &[0, 1]), gap("dev, no default features", &[1])],
            not_covered: (0..10).map(|index| format!("c{index}")).collect(),
            ..ConfigNote::default()
        };
        let lines = note.lines(2);
        assert_eq!(lines[0], "checked configurations (target `x86_64-unknown-linux-gnu`): `dev`, `release`");
        assert_eq!(lines[1], "! configurations `dev, all features`, `release, all features` are not checked: it does not build before the step: error: no");
        assert_eq!(lines[2], "! configuration `dev, no default features` is not checked in step 1: it does not build before the step: error: no");
        assert_eq!(lines[3], "! `cfg` conditions that Semilla did not check: `c0`, `c1`, `c2`, `c3`, `c4`, `c5`, `c6`, `c7`, and 2 more");
        // An agent command has the count and the first condition (batch 5x, part E).
        assert_eq!(note.agent_lines(2, false)[3], "! 10 `cfg` conditions that Semilla did not check: `c0`, and 9 more (`semilla status --verbose` shows each one)");
        assert!(note.agent_lines(2, true)[3].ends_with("`c7`, `c8`, `c9`"));
        let one = ConfigNote { not_covered: vec!["c0".into()], ..note.clone() };
        assert_eq!(one.agent_condition_line(false), one.condition_line());
        // The first error line can be different in each build: one line for one kind of cause.
        let other = ConfigGap { name: "release, all features".into(), cause: "it does not build before the step: error[E0599]: other".into(), steps: vec![0, 1] };
        let note = ConfigNote { not_checked: vec![gap("dev, all features", &[0, 1]), other], ..note };
        assert_eq!(note.gap_lines(2), ["! configurations `dev, all features`, `release, all features` are not checked: it does not build before the step: error: no"]);
        // No gap and no condition: one line, and the line of the scope (design 35.5).
        let note = ConfigNote { not_checked: Vec::new(), not_covered: Vec::new(), ..note };
        assert_eq!(note.lines(2).len(), 2);
        assert_eq!(note.lines(2)[1], CONFIG_SCOPE);
        assert_eq!(note.agent_lines(2, false)[1], CONFIG_SCOPE);
        // A smaller build (batch 5t, part C): the note is not a part of the name.
        let note = ConfigNote { checked: vec!["dev".into(), "dev, no default features (libraries and binaries only)".into()], ..note };
        assert_eq!(note.checked_line(), "checked configurations (target `x86_64-unknown-linux-gnu`): `dev`, `dev, no default features` (libraries and binaries only: the other targets do not build)");
        // An added configuration (batch 5u, part C) has a mark. A configuration that only some
        // steps have is not in `checked`: the mark names the steps.
        assert_eq!(note.added_line(), None);
        let added = |name: &str, steps: &[usize]| ConfigAdded { name: name.into(), steps: steps.to_vec() };
        let note = ConfigNote { checked: vec!["dev".into(), "dev, features fa".into()], added: vec![added("dev, features fa", &[]), added("dev, features fb", &[1]), added("dev, features fc", &[0, 2])], ..note };
        assert_eq!(note.checked_line(), "checked configurations (target `x86_64-unknown-linux-gnu`): `dev`, `dev, features fa` (added for a joint state), `dev, features fb` (added for a joint state in step 1), `dev, features fc` (added for a joint state in steps 0, 2)");
        assert_eq!(note.added_line().unwrap(), "Semilla added the configurations `dev, features fa`, `dev, features fb`, `dev, features fc` for a joint state");
        let note = ConfigNote { added: vec![added("dev, features fa", &[])], ..note };
        assert_eq!(note.added_line().unwrap(), "Semilla added the configuration `dev, features fa` for a joint state");
        // A report of an older version has no data.
        let report: Report = serde_json::from_str(r#"{"steps":[],"organization":[],"warnings":[],"accepted":true}"#).unwrap();
        assert!(report.configs.is_none());
        // Batch 5y, C4.4: a configuration of only some steps, and the equal configurations.
        let part = ConfigNote {
            target: "t".into(),
            checked: vec!["dev".into()],
            in_steps: vec![added("dev, all features", &[1])],
            same: vec![ConfigSame { name: "dev, no default features".into(), same_as: "dev".into(), steps: vec![0, 1] }, ConfigSame { name: "dev, all features".into(), same_as: "dev".into(), steps: vec![0] }],
            ..ConfigNote::default()
        };
        assert_eq!(part.checked_line(), "checked configurations (target `t`): `dev`, `dev, all features` (only in step 1)");
        assert_eq!(part.lines(2)[1], "not in the list, because the features are the same (also for `release`): `dev, no default features` = `dev`, `dev, all features` = `dev` in step 0");
        let note: ConfigNote = serde_json::from_str(r#"{"target":"t","checked":["dev"],"not_checked":[],"not_covered":[]}"#).unwrap();
        assert!(note.added.is_empty());
    }
}
