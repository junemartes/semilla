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
    /// A note for the reader (batch 5u, part A): the checked configurations with a proof of
    /// the changed bodies, and those with no proof. A fact with this note is not neutral. The
    /// note asks for no reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The mark of design 28 (invariant I6): the check of this fact ran the build code that
    /// step N changes. Each output shows [`build_code_mark`] with each proof of the fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_code: Option<usize>,
    /// True if the fact is a change of build code (design 28). Each output shows such a fact
    /// first in its step, with its diff.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub build_code_change: bool,
}

impl Fact {
    /// True if the first line of the fact in an output shows a proof: the class `neutral
    /// (proved)`, or a description with a proof (`renamed … (proved N1)`).
    pub fn has_proof(&self) -> bool {
        self.class == Some(Class::Neutral) || self.description.contains("(proved")
    }

    /// The mark for the first line of the fact in an output, with a space before it. It is
    /// empty for a fact with no proof, and for a proof that used only the build code of the base.
    pub fn mark(&self) -> String {
        match self.build_code {
            Some(step) if self.has_proof() => format!(" {}", build_code_mark(step)),
            _ => String::new(),
        }
    }

    /// The mark for a part of the fact that is a proof: the list of the bodies with an equal
    /// MIR, and the note of a partial proof. It is empty for a proof that used only the build
    /// code of the base.
    pub fn part_mark(&self) -> String {
        self.build_code.map(|step| format!(" {}", build_code_mark(step))).unwrap_or_default()
    }

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
    /// The checked configurations that Semilla added for a proof (batch 5u, part C).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<ConfigAdded>,
}

/// A checked configuration that Semilla added for a proof: a proof of a step needs a feature
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
            0 => " (added for a proof)".to_string(),
            1 => format!(" (added for a proof in step {})", added.steps[0]),
            _ => format!(" (added for a proof in steps {})", added.steps.iter().map(|step| step.to_string()).collect::<Vec<_>>().join(", ")),
        };
        let mut list: Vec<String> = self.checked.iter().map(|name| format!("{}{}", show(name), self.added.iter().find(|added| added.name == *name).map(mark).unwrap_or_default())).collect();
        list.extend(self.added.iter().filter(|added| !self.checked.contains(&added.name)).map(|added| format!("{}{}", show(&added.name), mark(added))));
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

    /// The line of the configurations that Semilla added for a proof, if one exists: the line
    /// of `step`.
    pub fn added_line(&self) -> Option<String> {
        if self.added.is_empty() {
            return None;
        }
        let list: Vec<String> = self.added.iter().map(|added| format!("`{}`", added.name)).collect();
        let noun = if list.len() == 1 { "configuration" } else { "configurations" };
        Some(format!("Semilla added the {noun} {} for a proof", list.join(", ")))
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

    /// The lines at the top of an output for the reviewer.
    pub fn lines(&self, all_steps: usize) -> Vec<String> {
        let mut lines = vec![self.checked_line()];
        lines.extend(self.gap_lines(all_steps));
        lines.extend(self.condition_line());
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

/// The mark of a proof that used build code that step `step` changes (design 28, invariant I6).
pub fn build_code_mark(step: usize) -> String {
    format!("(with the build code of step {step})")
}

/// The part of a count of proofs that says how many have the mark, or an empty text. `steps`
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
        let change = self.changes.iter().find(|change| change.steps.contains(&step))?;
        Some(if change.step == step {
            format!(
                "! build code changed in step {step}: {}. It runs in the sandbox during each check, and it can change each result {}. Each proof that depends on it has the mark {}.",
                change.parts,
                change.span(),
                build_code_mark(step)
            )
        } else {
            format!("! build code changed in step {} ({}): each proof of this step has the mark {}.", change.step, change.parts, build_code_mark(change.step))
        })
    }

    /// The part of the result line, with no separator.
    pub fn tag_text(&self) -> String {
        let spans: Vec<String> = self.changes.iter().map(|change| if change.to_end { format!("from step {}", change.step) } else { change.span() }).collect();
        format!("build code changed {}", spans.join(", "))
    }

    /// The line at the top of a result that ran the changed build code.
    pub fn ran_line(&self) -> String {
        let parts: Vec<String> = self.changes.iter().map(|change| format!("step {} ({})", change.step, change.parts)).collect();
        let spans: Vec<String> = self.changes.iter().map(BuildCodeChange::span).collect();
        let marks: Vec<String> = self.changes.iter().map(|change| build_code_mark(change.step)).collect();
        format!(
            "! build code changed in {}. The check ran it in the sandbox. It can change each result {}. Read that change first. Each proof that depends on it has the mark {}.",
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

    /// The part of a count of proved facts that says how many have the mark, or an empty text.
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

    /// The counts for the result line (brief 5f, D1): the facts, the facts that phase 2 proved
    /// neutral and that have no spotlight, and the facts with a spotlight. The reviewer reads
    /// each fact that is not proved neutral.
    pub fn fact_counts(&self) -> (usize, usize, usize) {
        let facts = || self.steps.iter().flat_map(|step| &step.facts);
        let neutral = facts().filter(|fact| fact.class == Some(Class::Neutral) && !fact.spotlight).count();
        (facts().count(), neutral, facts().filter(|fact| fact.spotlight).count())
    }

    /// The count of the proved neutral facts that have the mark of changed build code (design
    /// 28). A fact with a spotlight is a fact to read: it is not in this count.
    pub fn marked_count(&self) -> usize {
        self.steps.iter().flat_map(|step| &step.facts).filter(|fact| fact.class == Some(Class::Neutral) && !fact.spotlight && fact.build_code.is_some()).count()
    }

    /// The text of [`BuildCodeNote::count_text`] for a count of marked facts.
    pub fn marked_text(&self, marked: usize) -> String {
        self.build_code.as_ref().map(|note| note.count_text(marked)).unwrap_or_default()
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
    fn the_build_code_lines_of_the_step_command() {
        // Design 28.4: the full line in the step of the change, a short form later.
        let note = BuildCodeNote { changes: vec![change(3, &[3, 4, 5, 6], true)], ran: true };
        assert_eq!(note.step_line(2), None);
        assert_eq!(
            note.step_line(3).unwrap(),
            "! build code changed in step 3: crates/core/build.rs. It runs in the sandbox during each check, and it can change each result from step 3 on. Each proof that depends on it has the mark (with the build code of step 3)."
        );
        assert_eq!(note.step_line(5).unwrap(), "! build code changed in step 3 (crates/core/build.rs): each proof of this step has the mark (with the build code of step 3).");
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
        assert_eq!(note.mark_of(4), None, "a step with no check has no proof and no mark");
        assert_eq!(note.not_checked_text(), "steps 3–6 not checked: build code changed in step 3 (crates/core/build.rs). The command does not run it (the option --no-build-code, or a build sandbox that is not complete).");
        note.ran = true;
        assert_eq!(
            note.ran_line(),
            "! build code changed in step 3 (crates/core/build.rs). The check ran it in the sandbox. It can change each result from step 3 on. Read that change first. Each proof that depends on it has the mark (with the build code of step 3)."
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
    fn the_mark_is_on_each_proof_of_a_fact_and_on_each_verified_claim() {
        // Invariant I6: a proof that used changed build code has the mark.
        let mut fact = Fact { class: Some(Class::Neutral), ..Default::default() };
        assert_eq!((fact.mark(), fact.part_mark()), (String::new(), String::new()), "a proof with the build code of the base has no mark");
        fact.build_code = Some(2);
        assert_eq!(fact.mark(), " (with the build code of step 2)");
        assert_eq!(fact.part_mark(), " (with the build code of step 2)");
        // A fact that is not neutral has no proof in its first line. A proof in its text has the mark.
        fact.class = Some(Class::Logic);
        assert_eq!(fact.mark(), "");
        fact.description = "renamed `a` → `b` (proved N1)".into();
        assert_eq!(fact.mark(), " (with the build code of step 2)");
        let mut claim = ClaimResult {
            claim: "c1".into(),
            step: 2,
            status: ClaimStatus::Verified { proof: "mir".into() },
            class: None,
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
        claim.status = ClaimStatus::Unproved { reason: "no".into() };
        assert_eq!(claim.mark(), "", "a claim with no proof has no mark");
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
            added: Vec::new(),
        };
        let lines = note.lines(2);
        assert_eq!(lines[0], "checked configurations (target `x86_64-unknown-linux-gnu`): `dev`, `release`");
        assert_eq!(lines[1], "! configurations `dev, all features`, `release, all features` are not checked: it does not build before the step: error: no");
        assert_eq!(lines[2], "! configuration `dev, no default features` is not checked in step 1: it does not build before the step: error: no");
        assert_eq!(lines[3], "! `cfg` conditions that Semilla did not check: `c0`, `c1`, `c2`, `c3`, `c4`, `c5`, `c6`, `c7`, and 2 more");
        // The first error line can be different in each build: one line for one kind of cause.
        let other = ConfigGap { name: "release, all features".into(), cause: "it does not build before the step: error[E0599]: other".into(), steps: vec![0, 1] };
        let note = ConfigNote { not_checked: vec![gap("dev, all features", &[0, 1]), other], ..note };
        assert_eq!(note.gap_lines(2), ["! configurations `dev, all features`, `release, all features` are not checked: it does not build before the step: error: no"]);
        // No gap and no condition: one line.
        let note = ConfigNote { not_checked: Vec::new(), not_covered: Vec::new(), ..note };
        assert_eq!(note.lines(2).len(), 1);
        // A smaller build (batch 5t, part C): the note is not a part of the name.
        let note = ConfigNote { checked: vec!["dev".into(), "dev, no default features (libraries and binaries only)".into()], ..note };
        assert_eq!(note.checked_line(), "checked configurations (target `x86_64-unknown-linux-gnu`): `dev`, `dev, no default features` (libraries and binaries only: the other targets do not build)");
        // An added configuration (batch 5u, part C) has a mark. A configuration that only some
        // steps have is not in `checked`: the mark names the steps.
        assert_eq!(note.added_line(), None);
        let added = |name: &str, steps: &[usize]| ConfigAdded { name: name.into(), steps: steps.to_vec() };
        let note = ConfigNote { checked: vec!["dev".into(), "dev, features fa".into()], added: vec![added("dev, features fa", &[]), added("dev, features fb", &[1]), added("dev, features fc", &[0, 2])], ..note };
        assert_eq!(note.checked_line(), "checked configurations (target `x86_64-unknown-linux-gnu`): `dev`, `dev, features fa` (added for a proof), `dev, features fb` (added for a proof in step 1), `dev, features fc` (added for a proof in steps 0, 2)");
        assert_eq!(note.added_line().unwrap(), "Semilla added the configurations `dev, features fa`, `dev, features fb`, `dev, features fc` for a proof");
        let note = ConfigNote { added: vec![added("dev, features fa", &[])], ..note };
        assert_eq!(note.added_line().unwrap(), "Semilla added the configuration `dev, features fa` for a proof");
        // A report of an older version has no data.
        let report: Report = serde_json::from_str(r#"{"steps":[],"organization":[],"warnings":[],"accepted":true}"#).unwrap();
        assert!(report.configs.is_none());
        let note: ConfigNote = serde_json::from_str(r#"{"target":"t","checked":["dev"],"not_checked":[],"not_covered":[]}"#).unwrap();
        assert!(note.added.is_empty());
    }
}
