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

/// The cause of a group of changed lines with no fact (design 56.4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoFactCause {
    /// The smallest item of the tree with the lines, and the state of its tokens.
    pub cause: String,
    pub positions: Vec<Position>,
}

/// The result of one step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StepReport {
    pub step: usize,
    pub intent: String,
    /// Design 62: the step of a tag after the commit. A spotlight with no reason is an open
    /// item, not a reject of a step.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub after_commit: bool,
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
    /// The counts of the map from the MIR elements to the lossless tree (design 40.2 and
    /// 40.3). `semilla status --verbose` shows them.
    #[serde(default, skip_serializing_if = "MirMapCounts::is_empty")]
    pub mir_map: MirMapCounts,
    /// The facts of the agent with their check results (design 54).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_facts: Vec<AgentFact>,
    /// The gap list (design 54.4): the edits of existing code that can change the behavior and
    /// that no agent fact states. The list is never a cause of a reject.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<Gap>,
    /// The count of the changed lines of the line diff of the step (design 55.4): each removed
    /// line of the old tree and each added line of the new tree. A record of an earlier version
    /// has no cover check: the count is 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub changed_lines: usize,
    /// The changed lines that are in the position of no fact (design 55.4). Each one is a
    /// defect of Semilla. The list never rejects a step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_fact: Vec<Position>,
    /// The cause of each changed line with no fact (design 56.4), for the verbose output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_fact_causes: Vec<NoFactCause>,
    /// The changed lines that only the rule of the equal tokens (design 59, B-b) puts in a
    /// fact, by item pair, for the verbose output. The cause has the item and the lines of the
    /// two items of the pair.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub equal_token_lines: Vec<NoFactCause>,
    /// The tags of the agent (design 55.3), in the order of the command. A tag is a name for
    /// a part of the step. It is not a fact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
    /// The hunks of the line diff of the step (design 55.8): the hunks that the command
    /// [`LINE_DIFF_COMMAND`] shows between the old tree and the new tree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hunks: Vec<DiffHunk>,
    /// True if git did not give the line diff: the hunks are the hunks of the diff of Semilla.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_diff: bool,
    /// The count of the facts whose own lines are equal lines in the line diff (design 55.2,
    /// rule 5): each one has the changed lines of the smallest wider node with a changed line.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub wider_facts: usize,
    /// The old tree and the new tree of the step, for the line diff command (design 55.8).
    /// Empty: the step has no tree identifier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diff_trees: Vec<String>,
    /// The control flow graph diff of the step (design 60): only with the compiled check and a
    /// driver that writes the graphs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<FlowReport>,
    /// Why a step of the default mode has no expansion records (R65.8-2): one line of the step
    /// section, `expansions: not read: CAUSE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_gap: Option<String>,
}

/// The control flow graph diff of a step (design 60, part C).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowReport {
    /// Each body with a result: a body with an edit, and a body whose graph changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bodies: Vec<FlowBody>,
    /// The `control flow:` lines of each fact, by the id of the fact (at most three lines and
    /// a line `… N more`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub facts: std::collections::BTreeMap<String, Vec<String>>,
    /// The `equivalence:` lines of each fact, by the id of the fact (design 61, E1).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub equivalence: std::collections::BTreeMap<String, Vec<String>>,
    /// The bodies that a change of the step reaches through the reverse call graph (design
    /// 61.10, part A): each caller with its result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reached: Vec<ReachedBody>,
    /// The limits that stopped the walk (`the walk stopped at depth 4: 31 more callers not
    /// checked`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reach_stops: Vec<String>,
    /// The line of the budget of the equivalence check (design 61.14, R61.14-6): `the
    /// equivalence check stopped after 60 s: 41 bodies not checked; semilla status --verbose`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equivalence_stop: Option<String>,
    /// The `reaches:` line of each fact whose body is in the start set of the walk, by the id
    /// of the fact, and the reached public functions of the fact (name and place) for the
    /// Markdown review.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub reaches: std::collections::BTreeMap<String, (String, Vec<(String, String)>)>,
    /// The `expansion:` lines of each fact, by the id of the fact (design 63, R63.3-3): the
    /// changed expansions that the fact owns, folded, at most three lines and a line `… N more`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub expansion_lines: std::collections::BTreeMap<String, Vec<String>>,
    /// Each macro call whose expansion the step changes, adds, or removes (design 63).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expansions: Vec<ExpansionSite>,
    /// The count of the macro calls with an equal expansion (design 63).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub expansions_equal: usize,
}

/// One macro call whose expansion the step changes (design 63, part B).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpansionSite {
    /// The source text of the call: `fee!(v)`, or the name of a derive.
    pub call: String,
    /// The place of the call in the syntax tree: `shop::price/tail`.
    pub at: String,
    /// `FILE:LINE` of the call (a line `-N` is in the old tree).
    pub place: String,
    /// The definition: `macro_rules! fee` (src/lib.rs:2), or `derive PartialEq` (crate core).
    pub definition: String,
    /// `expansion changed`, `expansion added`, or `expansion removed`.
    pub result: String,
    /// The checked configurations with this result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configs: Vec<String>,
    /// The facts of the normal compare of the two expansions, each with its category.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<String>,
    /// The cause: the changed definitions of the chain, or the changed input.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cause: String,
    /// The ids of the facts that own the change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<String>,
    /// The line diff of the expanded code (`- `, `+ `, and `  ` lines), with a limit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diff: Vec<String>,
    /// The key of the fold: equal keys are one line in the short views.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// The line of the control flow section of each body at the call (the body that holds the
    /// call, and each body that the expansion makes): its graph result and its equivalence
    /// (design 61). The compiled check of the expanded code is the check of these bodies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bodies: Vec<String>,
}

/// One body that the walk of the reverse call graph reached (design 61.10, part A).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachedBody {
    /// The MIR name of the body.
    pub name: String,
    /// The place of the body: `FILE:LINE`, or `no place`.
    pub place: String,
    /// The result: `different through shop::tier: …`, `equivalent (proved after inlining
    /// shop::tier)`, or `reached through shop::tier, not decided (…)`.
    pub text: String,
    /// The chain from the body to a changed body of the step (`shop::bill → shop::cost →
    /// shop::tier`).
    pub chain: String,
    /// The fold key of the step section: the result with no input and no outcome.
    pub key: String,
}

/// The most bodies of one fold line of the reached bodies in the step section.
pub const REACH_FOLD_NAMES: usize = 3;

/// The result of one body in the control flow section of a step (design 60, C2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowBody {
    /// The MIR name of the body.
    pub name: String,
    /// The place of the body: `FILE:LINE`.
    pub place: String,
    /// The line of the step section after the place: the result and its summary.
    pub line: String,
    /// The full diff for `status --verbose`: a line for each result and a line for each
    /// changed point and each arrow that is not shared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
    /// The changed part of the graph for the flowchart of the Markdown review (design 60, C4):
    /// only for a body whose graph changes, with at most [`FLOW_CHART_POINTS`] points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart: Option<FlowChart>,
    /// The diff list of the Markdown review: each changed point and each arrow that is not
    /// shared, with the place of its first point (`FILE:LINE`, `FILE:-LINE` on the old side).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub list: Vec<(String, String)>,
}

/// The most points of the flowchart of one body (design 60, C4). A larger changed part has
/// only the list.
pub const FLOW_CHART_POINTS: usize = 40;

/// The changed part of the graph of a body: the changed points, the ends of the arrows that are
/// not shared, and the points next to them (design 60, C4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowChart {
    pub nodes: Vec<FlowChartNode>,
    pub edges: Vec<FlowChartEdge>,
}

/// A point of a flowchart.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowChartNode {
    pub id: String,
    /// The kind and the text of the point.
    pub label: String,
    /// `added`, `removed`, `changed`, or `kept`.
    pub mark: String,
    /// The place: `FILE:LINE`, or `FILE:-LINE` for a point of the old graph.
    pub place: String,
}

/// An arrow of a flowchart.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowChartEdge {
    pub from: String,
    pub to: String,
    pub label: String,
    /// `added`, `removed`, or `kept`.
    pub mark: String,
}

/// The line diff command of a step (design 55.8). The two last arguments are the old tree and
/// the new tree. The options make the result independent of the configuration of the user.
/// Decided by Claude: the algorithm and the indent heuristic are the defaults of git.
pub const LINE_DIFF_COMMAND: &str = "git diff -U0 --inter-hunk-context=0 --diff-algorithm=myers --indent-heuristic --no-renames --no-ext-diff --no-textconv --no-relative --text --no-color";

/// One hunk of the line diff of a step (design 55.8), with the numbers of the hunk header of
/// git: `@@ -OLD_START,OLD_COUNT +NEW_START,NEW_COUNT @@`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    pub file: String,
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
}

impl DiffHunk {
    /// The start and the count of one side.
    pub fn side(&self, old: bool) -> (usize, usize) {
        if old { (self.old_start, self.old_count) } else { (self.new_start, self.new_count) }
    }

    /// The lines of one side: the first line and the line after the last line.
    pub fn range(&self, old: bool) -> (usize, usize) {
        let (start, count) = self.side(old);
        if count == 0 { (start + 1, start + 1) } else { (start, start + count) }
    }

    /// The header as git prints it. Git does not print a count of 1.
    pub fn header(&self) -> String {
        let part = |start: usize, count: usize| if count == 1 { start.to_string() } else { format!("{start},{count}") };
        format!("@@ -{} +{} @@", part(self.old_start, self.old_count), part(self.new_start, self.new_count))
    }

    /// The place of the tag that selects the hunk (design 55.3): the new side, or the old
    /// side for a hunk with no line in the new tree.
    pub fn place(&self) -> String {
        if self.new_count > 0 { format!("{}:+{},{}", self.file, self.new_start, self.new_count) } else { format!("{}:-{},{}", self.file, self.old_start, self.old_count) }
    }
}

/// A range of lines of one file (design 55.2): the place of an edit in the line diff.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Position {
    pub file: String,
    /// True for lines of the old tree (a deleted text). False for lines of the new tree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub old: bool,
    /// The first line, from 1.
    pub start: usize,
    /// The last line.
    pub end: usize,
}

impl Position {
    /// The text of the position: `FILE:LINE` or `FILE:START-END` for the new tree,
    /// `FILE:-LINE` or `FILE:-START-END` for the old tree.
    pub fn text(&self) -> String {
        let side = if self.old { "-" } else { "" };
        if self.start == self.end {
            format!("{}:{side}{}", self.file, self.start)
        } else {
            format!("{}:{side}{}-{}", self.file, self.start, self.end)
        }
    }

    /// The text of a list of positions, for example `a.rs:3-5, a.rs:-9, b.rs:1`.
    pub fn list(positions: &[Position]) -> String {
        positions.iter().map(Position::text).collect::<Vec<_>>().join(", ")
    }

    /// The count of the lines.
    pub fn lines(&self) -> usize {
        self.end + 1 - self.start.min(self.end + 1)
    }

    /// True if each line of `other` is a line of this position.
    pub fn contains(&self, other: &Position) -> bool {
        self.file == other.file && self.old == other.old && self.start <= other.start && other.end <= self.end
    }

    /// True if the two positions have a line in common.
    pub fn meets(&self, other: &Position) -> bool {
        self.file == other.file && self.old == other.old && self.start <= other.end && other.start <= self.end
    }

    /// The positions in order, with no line twice: ranges that touch are one range.
    pub fn normal(mut positions: Vec<Position>) -> Vec<Position> {
        positions.sort();
        let mut out: Vec<Position> = Vec::new();
        for position in positions {
            match out.last_mut() {
                Some(last) if last.file == position.file && last.old == position.old && position.start <= last.end + 1 => last.end = last.end.max(position.end),
                _ => out.push(position),
            }
        }
        out
    }
}

/// One place of a tag (design 55.3): a range of lines, or a full file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub file: String,
    /// `None`: the full file, the two trees. Else: (old tree, first line, last line).
    pub lines: Option<(bool, usize, usize)>,
    /// True for the side of a hunk header with the count 0 (`FILE:-START,0` or
    /// `FILE:+START,0`): the place has no line. It names the hunk with this header side.
    /// `lines` has the side and the start number of the header.
    pub empty: bool,
}

impl Place {
    /// Reads `FILE`, `FILE:LINE`, `FILE:START-END`, `FILE:-LINE`, `FILE:-START-END`, and the two
    /// forms of a hunk header of `git diff`: `FILE:-START,COUNT` and `FILE:+START,COUNT`.
    pub fn parse(text: &str) -> Result<Place, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("a position has no text".to_string());
        }
        // The lines come after the last `:`, if the text after it starts like a line range.
        let split = text.rsplit_once(':').filter(|(file, lines)| !file.is_empty() && lines.starts_with(|first: char| first.is_ascii_digit() || first == '-' || first == '+'));
        let Some((file, lines)) = split else {
            return Ok(Place { file: text.to_string(), lines: None, empty: false });
        };
        let error = || format!("`{text}` is not a position: write FILE, FILE:LINE, FILE:START-END, or FILE:-START-END for lines of the old tree");
        let (old, rest) = match lines.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, lines.strip_prefix('+').unwrap_or(lines)),
        };
        let number = |part: &str| part.parse::<usize>().ok().filter(|_| part.bytes().all(|byte| byte.is_ascii_digit()));
        let (start, end) = if let Some((start, count)) = rest.split_once(',') {
            // The form of a hunk header: the first line and the count of the lines.
            let (start, count) = (number(start).ok_or_else(error)?, number(count).ok_or_else(error)?);
            if count == 0 {
                // Design 55.3: the side of a hunk with no line in this tree, as git prints
                // it. The number is the line before the hunk: 0 is a valid number.
                return Ok(Place { file: file.to_string(), lines: Some((old, start, start)), empty: true });
            }
            (start, start + count - 1)
        } else if let Some((start, end)) = rest.split_once('-') {
            (number(start).ok_or_else(error)?, number(end).ok_or_else(error)?)
        } else {
            let line = number(rest).ok_or_else(error)?;
            (line, line)
        };
        if start == 0 || end < start {
            return Err(format!("`{text}` is not a line range: the first line is 1 or more, and the last line is not before the first line"));
        }
        Ok(Place { file: file.to_string(), lines: Some((old, start, end)), empty: false })
    }

    /// The text of the place, in the form of [`Position::text`].
    pub fn text(&self) -> String {
        match self.lines {
            None => self.file.clone(),
            Some((old, start, _)) if self.empty => format!("{}:{}{start},0", self.file, if old { '-' } else { '+' }),
            Some((old, start, end)) => Position { file: self.file.clone(), old, start, end }.text(),
        }
    }

    /// True if each line of the position is in the place.
    pub fn contains(&self, position: &Position) -> bool {
        !self.empty && self.file == position.file && self.lines.is_none_or(|(old, start, end)| old == position.old && start <= position.start && position.end <= end)
    }

    /// True if a line of the position is in the place.
    pub fn meets(&self, position: &Position) -> bool {
        !self.empty && self.file == position.file && self.lines.is_none_or(|(old, start, end)| old == position.old && start <= position.end && position.start <= end)
    }

    /// The lines that the place selects in a hunk of the line diff (design 55.3, rule 2):
    /// (old tree, first line, the line after the last line) for each side with a line.
    /// - A place `FILE` selects the two sides of each hunk of the file.
    /// - A place that has the full new side of a hunk, or the full old side, selects the two
    ///   sides of that hunk. A side with the count 0 is a full side: it names the hunk.
    /// - Each other place selects the changed lines of the hunk that are in its lines.
    pub fn selects(&self, hunk: &DiffHunk) -> Vec<(bool, usize, usize)> {
        if self.file != hunk.file {
            return Vec::new();
        }
        let both = || [false, true].into_iter().map(|old| (old, hunk.range(old).0, hunk.range(old).1)).filter(|(_, start, end)| start < end).collect();
        let Some((old, start, end)) = self.lines else { return both() };
        let (first, after) = hunk.range(old);
        if self.empty {
            return if hunk.side(old) == (start, 0) { both() } else { Vec::new() };
        }
        if first == after {
            return Vec::new();
        }
        if start <= first && after <= end + 1 {
            return both();
        }
        let (from, to) = (start.max(first), (end + 1).min(after));
        if from < to { vec![(old, from, to)] } else { Vec::new() }
    }
}

/// One tag of the agent with the facts in its lines (design 55.3). A tag makes no claim about
/// the behavior: it groups the facts for the reader.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    /// The places, as [`Place::text`] prints them.
    pub places: Vec<String>,
    /// The numbers of the facts of Semilla whose edit is fully in the lines of the tag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<String>,
    /// The numbers of the facts that are in the tag in part (design 55.3, rule 5): the tag
    /// selects a line of the fact, and it does not select each line of one side.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial: Vec<String>,
    /// A line for each fact of `partial`: the lines of the fact that are not in the tag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The count of the changed lines that the places of the tag select, in the two trees.
    #[serde(default)]
    pub lines: usize,
    /// The error of the tag: a place with no edit in its lines. A step with such a tag is not
    /// stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True if the error is an error of the form: the text of the tag is not a tag, or a line
    /// number is after the last line of the file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub form: bool,
}

/// The group of a fact in the output of a step with tags (design 55.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagGroup {
    /// The index of the tag of the fact.
    Tag(usize),
    /// A fact with a position that is in no tag, or a fact of a step with no tag.
    Outside,
    /// A fact with no position, in a step with tags.
    NoPosition,
}

/// Reads the text of a `--tag` option: `PLACE[,PLACE]=NAME` (design 55.3). The name is the
/// text after the first `=`. A `,` before a digit continues a hunk header form (`a.rs:-3,2`).
pub fn parse_tag(text: &str) -> Result<(Vec<Place>, String), String> {
    let Some((places, name)) = text.split_once('=') else {
        return Err(format!("`{text}` is not a tag: write PLACE[,PLACE]=NAME, for example src/net.rs:40-88,src/cfg.rs:12=retry option"));
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("the tag `{text}` has no name: write PLACE[,PLACE]=NAME"));
    }
    let mut parts: Vec<String> = Vec::new();
    for part in places.split(',') {
        let count = !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        match parts.last_mut() {
            Some(last) if count && last.rsplit_once(':').is_some_and(|(_, lines)| lines.starts_with(['-', '+']) && !lines.contains(',')) => {
                last.push(',');
                last.push_str(part);
            }
            _ => parts.push(part.to_string()),
        }
    }
    let places = parts.iter().map(|part| Place::parse(part)).collect::<Result<Vec<_>, _>>()?;
    Ok((places, name.to_string()))
}

/// One fact of the agent with its check result (design 54).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentFact {
    /// The number in the step, for example `a1`.
    pub id: String,
    /// `same` or `change`.
    pub kind: String,
    /// The claim of a `same` fact (`rename OLD NEW`), or the items of a `change` fact as the
    /// agent gave them.
    pub text: String,
    /// The sentences of a `change` fact. Semilla does not check them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sentences: Vec<String>,
    /// The check result: true if the tag agrees with the edit that Semilla sees.
    pub holds: bool,
    /// The text of the check result: what holds, or the cause of a false fact.
    pub result: String,
    /// The lines below the result: a note, a candidate, or what Semilla sees in an item.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The places that the fact covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<String>,
    /// The numbers of the facts that are fully below this fact. Each one has the origin `stated`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<String>,
    /// The facts with some places below this fact: the number and the count of the places.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial: Vec<String>,
    /// The options that give the fact again, for the command of a rejected step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// The width of a `change` fact (design 55.3): the count of the facts, of the items, and
    /// of the changed lines that it covers. The result line shows it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub covers: String,
}

impl AgentFact {
    /// The first line of the fact in an output, without the mark.
    pub fn line(&self) -> String {
        let sentences: String = self.sentences.iter().map(|text| format!(" {}", crate::text::quoted(text))).collect();
        let covers = if self.covers.is_empty() { String::new() } else { format!("; {}", self.covers) };
        format!("{} {}: {}{sentences} ({}{covers})", self.id, self.kind, self.text, self.result)
    }

    /// The lines below the first line: the places, the notes, and the facts with some places.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.places.is_empty() {
            lines.push(format!("places: {}", self.places.join(", ")));
        }
        if !self.partial.is_empty() {
            lines.push(format!("a part of: {}", self.partial.join(", ")));
        }
        lines.extend(self.notes.iter().cloned());
        lines
    }
}

/// One line of the gap list (design 54.4): the places with one equal edit that no agent fact
/// states.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Gap {
    /// The edit: the text of the fact without the place, or the pattern of the group.
    pub text: String,
    /// The places, in the order of the facts.
    pub places: Vec<String>,
    /// The items of the places.
    pub items: Vec<String>,
    /// The numbers of the facts with the edits.
    pub facts: Vec<String>,
    /// The option that states the line: `--same 'rewrite FROM => TO'` if one pattern covers
    /// each place, else `--change 'ITEMS=TEXT'`.
    pub option: String,
    /// R71.3-1 (R68.8-1 before batch 11c): the compiled check did not decide if the behavior of
    /// the places changes (each fact of the line has a result, and no result is "different").
    /// The suggestion offers the two forms in neutral words, and the one command of the step
    /// does not have the option.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub undecided: bool,
    /// R71.3-1: the `--same` options for the form that keeps the behavior: the option of the line
    /// (one pattern covers each place of a group), or the concrete pattern of each edit of the
    /// line, also of one place. `None`: Semilla has no pattern, and the line has the general
    /// words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same: Option<String>,
}

impl Gap {
    /// The text after "to state it: " (R71.3-1).
    pub fn suggestion(&self) -> String {
        if !self.undecided {
            return self.option.clone();
        }
        let same = match &self.same {
            Some(same) => same.clone(),
            None => "a `--same` fact (rename, move, or rewrite) that explains the edit".to_string(),
        };
        let change = if self.option.starts_with("--change ") { self.option.clone() } else { format!("--change '{}=TEXT'", self.items.join(",")) };
        format!("the check did not decide if the behavior changes: if it changes, {change}; if it does not change, {same}")
    }
}

impl Gap {
    /// The first line of the gap in an output, without the mark.
    pub fn line(&self) -> String {
        let places = if self.places.len() == 1 { "1 place".to_string() } else { format!("{} places", self.places.len()) };
        format!("{places}: {} ({})", self.text, self.facts.join(", "))
    }
}

/// The start of the line with the one command that states the gaps of a stored step.
pub const GAP_COMMAND: &str = "to state them, add facts to the stored step (no build check runs again): semilla fact --step ";

/// The start of the same line for a step that is not stored: the options go to the command of
/// the step.
pub const GAP_OPTIONS: &str = "to state them, add these options to the command of the step:";

/// The count of the first gap lines that the output of a step and the short status show.
pub const GAP_LIMIT: usize = 5;

/// The counts of the map from the MIR elements that differ to the nodes and the edits of a
/// step (design 40.2 and 40.3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirMapCounts {
    /// The ranges that do not start and end at a token boundary of the tree.
    #[serde(default)]
    pub errors: usize,
    /// The first map errors: `FILE:LINE:COLUMN (bytes START..END)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// The edits of a changed body that own no element that differs, when an element of the
    /// body has no owner: these edits keep the result of design 38 (the doubt rule).
    #[serde(default)]
    pub doubt: usize,
    /// The edits that own an element that differs.
    #[serde(default)]
    pub owners: usize,
    /// The edits of a changed body that own no element that differs, when each element has
    /// an owner: these edits get `code equal`.
    #[serde(default)]
    pub equal: usize,
    /// The statement pairs of the mapper (a move, or the same text after replacements) whose
    /// MIR elements do not align with each other (design 41.8).
    #[serde(default)]
    pub disagree: usize,
    /// The statement pairs of the mapper that the check compared.
    #[serde(default)]
    pub checked: usize,
    /// The checked pairs with no aligned element (no proof either way).
    #[serde(default)]
    pub unaligned: usize,
    /// The first pairs that do not agree: `OLD → NEW (cause)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disagree_examples: Vec<String>,
    /// The statement pairs of round 3, of the composite step, and of the gap rules of 8b in a
    /// mapped body (design 41.8).
    #[serde(default)]
    pub round3: usize,
    #[serde(default)]
    pub composites: usize,
    #[serde(default)]
    pub gap_pairs: usize,
    /// The MIR elements that a symbol link owns (design 44).
    #[serde(default)]
    pub linked: usize,
    /// The bodies above the limit of the statement mapper (design 41.9).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mapper_skipped: Vec<String>,
    /// The changed MIR elements of the bodies with an edit, by their cause (design 45): the
    /// names of [`CAUSES`].
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub causes: std::collections::BTreeMap<String, usize>,
}

/// The causes of a changed MIR element (design 45), in the order of the status line: the edit at
/// its place (rules 1 and 2 of 40.3), an edit inside its node (rule 1b), a symbol link (44), a
/// dependence in its body (45), the kept rules (a jump, glue, the region of a move, the
/// counterpart: the same node text with a link on the other side), and no cause.
pub const CAUSES: &[&str] = &["rule 1", "rule 1b", "link", "dependence", "jump", "glue", "region", "counterpart", "none"];

impl MirMapCounts {
    pub fn is_empty(&self) -> bool {
        *self == MirMapCounts::default()
    }

    /// The line of `semilla status --verbose`.
    pub fn line(&self) -> String {
        let mut line = format!("MIR map: {} map error(s); {} edit(s) own a changed MIR element; {} edit(s) with equal code in a changed body; {} edit(s) with the doubt rule", self.errors, self.owners, self.equal, self.doubt);
        if self.linked > 0 {
            line.push_str(&format!("; {} linked element(s)", self.linked));
        }
        if !self.causes.is_empty() {
            let parts: Vec<String> = CAUSES.iter().filter_map(|cause| self.causes.get(*cause).map(|count| format!("{cause} {count}"))).collect();
            line.push_str(&format!("; changed elements by cause: {}", parts.join(", ")));
        }
        if !self.examples.is_empty() {
            line.push_str(&format!(" (first map errors: {})", self.examples.join(", ")));
        }
        if self.checked > 0 {
            line.push_str(&format!("; statement map: {} pair(s) checked, {} do not agree with the MIR alignment, {} with no aligned element", self.checked, self.disagree, self.unaligned));
            if !self.disagree_examples.is_empty() {
                line.push_str(&format!(" (first: {})", self.disagree_examples.join("; ")));
            }
        }
        if self.round3 + self.composites + self.gap_pairs > 0 {
            line.push_str(&format!("; mapper: {} round 3 pair(s), {} composite pair(s), {} pair(s) of the 8b gap rules", self.round3, self.composites, self.gap_pairs));
        }
        if !self.mapper_skipped.is_empty() {
            line.push_str(&format!("; the statement mapper did not run for {} body(ies) above its limit: {}", self.mapper_skipped.len(), self.mapper_skipped.join(", ")));
        }
        line
    }
}

fn is_zero(count: &usize) -> bool {
    *count == 0
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
    /// The lines of the edit of the fact in the line diff of the step (design 55.2), in order.
    /// Empty for a fact with no edit of its own (an `indirect` fact), and in a record of an
    /// earlier version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<Position>,
    /// The part: `type`, `visibility`, `member added`, `parameter`, `return type`, …
    pub part: String,
    pub description: String,
    /// The origin of the fact (design 54.5). `derived`: Semilla filled the fact in from the
    /// diff. `stated`: Semilla computed the fact from the diff, and a checked fact of the agent
    /// covers each place of it.
    pub status: String,
    /// The category (design 35): the kind of change that the text shows. A record with no
    /// category is a record of an older Semilla: it gives an error.
    pub category: Category,
    /// The compare result (design 35): an observation of the compiled code. It is no verdict.
    /// A fact of a step with no compiled check has no such field (design 50).
    #[serde(default = "Compare::off", skip_serializing_if = "Compare::is_off")]
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
    /// The path of the item of the fact, when a reason with this path as its key covers the
    /// fact (design 36.5). The command to run again gives this key one time for the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_item: Option<String>,
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
    /// The count of the other facts of the step with an edit in the body of this fact, when
    /// the compiled code of that body changed (design 36.5). The compare result is a result of
    /// the edits together. Each output shows [`Fact::other_edits_text`] after the bodies.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub other_edits: usize,
    /// True if a changed body of the fact has an `unsafe` block or is an `unsafe fn` (design
    /// 36.5). Each output shows [`UNSAFE_LINE`] with the fact. The line asks for no reason.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unsafe_code: bool,
    /// The mark of design 28 (invariant I6): the check of this fact ran the build code that
    /// step N changes. Each output shows [`build_code_mark`] with a result `code equal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_code: Option<usize>,
    /// True if the fact is a change of build code (design 28). Each output shows such a fact
    /// first in its step, with its diff.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub build_code_change: bool,
    /// The full list of the causes of a "build input" spotlight (design 37.14). The spotlight
    /// text shows at most [`CAUSE_LIMIT`] causes. `semilla status --verbose` shows each cause.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<String>,
    /// The count of the spotlights that the kind reason of this fact covers (design 37.19), or
    /// 0 for a fact with no kind reason. The key of the reason is `kind:KIND`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reason_covers: usize,
    /// The branch mark (design 40.4): the MIR elements that the edits of the fact own change
    /// the branch structure of the compiled code. Each output shows `branches: TEXT`. The mark
    /// is not a mismatch mark, and it asks for no reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branches: Option<String>,
    /// The symbol links of the fact (design 44): each changed symbol that a body of `affected`
    /// names, with the bodies. Each output shows "through: `T` (2 bodies)".
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub through: std::collections::BTreeMap<String, Vec<String>>,
    /// True if each place of the fact is in a test or in a bench (design 54.5). The agent
    /// writes no fact for such a place, and the gap list does not have it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
    /// The comments of the nodes of the fact (design 58): the comments of each node from the
    /// changed node up to its item. Empty for a fact that does not make its item a changed
    /// item, and in a record of an earlier version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<RelatedComment>,
}

/// The state of a comment of a changed item (design 58.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentState {
    /// The text of the comment is equal in the old item and in the new item.
    #[default]
    NoEdit,
    /// The step changed the text. A fact of the step has the lines of the comment.
    Changed,
    /// The comment is new in this step. A fact of the step has its lines.
    Added,
    /// The step removed the comment. A fact of the step has its lines.
    Removed,
    /// Semilla found no equal comment in the other tree and no fact with the lines.
    NoPair,
    /// The text of the comment is equal in the old item and in the new item, and a `docs`
    /// fact of the step has a line of the comment: the comment is at a different place.
    Moved,
}

/// One comment of a node of a fact (design 58).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedComment {
    /// The place of the comment at its node: ``docs of `shop::price` ``, `comment above line 4`.
    pub place: String,
    pub state: CommentState,
    /// The full text of the comment, from the tokens of the syntax tree. Each line of the
    /// comment is one line of the text.
    pub text: String,
    /// The lines of the comment: in the new tree, or in the old tree for a removed comment.
    pub position: Position,
    /// The facts of the step that have the lines of a comment with an edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<String>,
}

/// The mark of a comment with no edit (design 58.4).
pub const COMMENT_NO_EDIT: &str = "no edit in this step";

/// The note below the first comment with no edit of a step (design 58.4).
pub const COMMENT_NOTE: &str = "Semilla does not check that a comment agrees with the code. A comment with no edit can be out of date.";

/// The start of each line of a comment text in a block (design 58.7).
pub const COMMENT_BLOCK: &str = "| ";

impl RelatedComment {
    /// The key of the comment in a step: two facts of one item have the same comment, and
    /// each output shows it one time (design 58.6).
    pub fn key(&self) -> (&Position, &str) {
        (&self.position, &self.text)
    }

    /// True if the output shows the text of the comment (a comment with an edit shows the
    /// number of its fact).
    pub fn shows_text(&self) -> bool {
        matches!(self.state, CommentState::NoEdit | CommentState::NoPair)
    }

    /// The first line of the comment in an output, without its text.
    pub fn head(&self) -> String {
        match self.state {
            CommentState::NoEdit => format!("{}, {COMMENT_NO_EDIT} ({})", self.place, self.position.text()),
            CommentState::NoPair => format!("{}, with no pair in the {} tree and no fact ({})", self.place, if self.position.old { "new" } else { "old" }, self.position.text()),
            CommentState::Changed => format!("{}, changed in this step: see {}", self.place, self.facts.join(", ")),
            CommentState::Added => format!("{}, added in this step: see {}", self.place, self.facts.join(", ")),
            CommentState::Removed => format!("{}, removed in this step: see {}", self.place, self.facts.join(", ")),
            CommentState::Moved => format!("{}, moved in this step: see {}", self.place, self.facts.join(", ")),
        }
    }

    /// The lines of the comment below its fact, without the indent of the fact (design 58.7).
    /// A text of one line that is not long is in the line. Each other text is a block: each
    /// line of the comment is one line with the mark [`COMMENT_BLOCK`]. No text has a cut.
    pub fn lines(&self) -> Vec<String> {
        self.lines_with(None)
    }

    /// The head and the text of a comment for an output with the size rule of R71.1-1: a text
    /// over the budget gives the head with the note (the size and `command`) and its first
    /// lines. Else the head and the full text.
    pub fn head_and_text(&self, command: Option<&str>) -> (String, String) {
        match command.filter(|_| self.shows_text()).and_then(|command| crate::budget::single(&self.text, Some(command))) {
            Some(crate::budget::Form::Block(note, rows)) => (format!("{} {note}", self.head()), rows.join("\n")),
            Some(crate::budget::Form::Line(text)) => (format!("{}: {text}", self.head()), String::new()),
            None => (self.head(), self.text.clone()),
        }
    }

    /// The lines of the comment with the size rule of R71.1-1: a text over the budget shows its
    /// first lines, the size, and `command` (the command that shows the full text). A comment
    /// with no edit has no changed line, so the rule hides no edit. `None`: the full text.
    pub fn lines_with(&self, command: Option<&str>) -> Vec<String> {
        if !self.shows_text() {
            return vec![self.head()];
        }
        if !self.text.contains('\n') && self.text.chars().count() <= crate::text::BLOCK_MIN {
            return vec![format!("{}: `{}`", self.head(), self.text)];
        }
        let block = |line: &str| if line.is_empty() { format!("    {}", COMMENT_BLOCK.trim_end()) } else { format!("    {COMMENT_BLOCK}{line}") };
        if let Some(form) = command.and_then(|command| crate::budget::single(&self.text, Some(command))) {
            return match form {
                crate::budget::Form::Block(note, rows) => std::iter::once(format!("{} {note}:", self.head())).chain(rows.iter().map(|line| block(line))).collect(),
                crate::budget::Form::Line(text) => vec![format!("{}: {text}", self.head())],
            };
        }
        let mut lines = vec![format!("{}:", self.head())];
        // An empty line of the comment is the mark with no space after it.
        lines.extend(self.text.split('\n').map(block));
        lines
    }
}

/// The kinds of spotlight that one reason can cover (design 37.19): the name for a reason
/// with the key `kind:NAME`, and the start of the spotlight text. The kind of a spotlight is the
/// kind of the first part of its text.
pub const SPOTLIGHT_KINDS: &[(&str, &str)] = &[
    ("cfg-attribute", "a changed `cfg` attribute"),
    ("new-attribute", "a new item with an attribute that can change the behavior"),
    ("changed-attribute", "a changed attribute that can change the behavior"),
    ("inactive-code", "inactive code"),
    ("indirect-change", "an indirect change that Semilla cannot explain"),
    ("changed-macro", "a changed macro"),
    ("build-input", "a build input"),
    ("build-setting", "a build setting"),
    ("manifest-key", "a manifest key that is not in the allow list"),
    ("package-setting", "a package setting that changes the build"),
    ("new-dependency", "a new dependency"),
    ("dependency-features", "the features of a dependency"),
    ("dependency-setting", "a dependency setting other than the version"),
    ("other-file", "a file that is not on the allow list"),
    (BUILD_CODE_KIND, "build code"),
];

/// The kind of a changed file of build code. Each such file needs its own reason: no kind
/// reason covers it (design 37.19).
pub const BUILD_CODE_KIND: &str = "build-code";

/// The start of the key of a kind reason: `kind:NAME`.
pub const KIND_KEY: &str = "kind:";

/// The smallest count of open spotlights of one kind for which the command to run again
/// prints a kind reason (design 37.19).
pub const KIND_REASON_MIN: usize = 4;

/// The kind of a spotlight text, or `None` for a text with no kind name.
pub fn spotlight_kind(why: &str) -> Option<&'static str> {
    SPOTLIGHT_KINDS.iter().find(|(_, start)| why.starts_with(start)).map(|(name, _)| *name)
}

/// The names of the kinds that a kind reason can cover, for a message.
pub fn kind_names() -> String {
    SPOTLIGHT_KINDS.iter().filter(|(name, _)| *name != BUILD_CODE_KIND).map(|(name, _)| format!("`{name}`")).collect::<Vec<_>>().join(", ")
}

impl Fact {
    /// The label of the reason in each output. It has the key of the agent, when the key is
    /// the path of the item and not the exact key of the fact (batch 5x, part D).
    pub fn reason_label(&self) -> String {
        match &self.reason_key {
            Some(key) if self.reason_covers > 0 => format!("reason (key `{key}`; one reason covers {} spotlight{} of the kind {})", self.reason_covers, if self.reason_covers == 1 { "" } else { "s" }, key.strip_prefix(KIND_KEY).unwrap_or(key)),
            Some(key) => format!("reason (key `{key}`)"),
            None => "reason".to_string(),
        }
    }

    /// The kind of the spotlight of this fact (design 37.19), if its text has a kind name.
    pub fn kind(&self) -> Option<&'static str> {
        self.spotlight_why.as_deref().and_then(spotlight_kind)
    }

    /// The exact key of the reason of this fact: its anchor, or its part.
    pub fn reason_place(&self) -> String {
        self.at.as_ref().map(|at| at.to_string()).unwrap_or_else(|| self.part.clone())
    }

    /// The text in the brackets of the first line of the fact: "rename · code equal". A fact
    /// of a step with no compiled check has the category only: "rename" (design 50).
    pub fn label(&self) -> String {
        if self.compare.is_off() {
            return self.category.name().to_string();
        }
        format!("{} · {}", self.category.name(), self.compare.name())
    }

    /// True if the fact has the mismatch mark (design 35.2): the text of the change gives no
    /// cause for different compiled code, and the compiled code is different.
    pub fn mismatch(&self) -> bool {
        self.category.expects_equal() && self.compare == Compare::CodeDifferent
    }

    /// The text after the list of the affected bodies: " (with N other edits of this body)".
    /// Empty if the fact is the only edit of its body.
    pub fn other_edits_text(&self) -> String {
        match self.other_edits {
            0 => String::new(),
            1 => " (with 1 other edit of this body)".to_string(),
            count => format!(" (with {count} other edits of this body)"),
        }
    }

    /// True if the text of the fact has the note of a public API change (design 36.3). The
    /// compare does not see the code of a different crate, so each output shows [`API_LINE`]
    /// with the fact.
    pub fn api_change(&self) -> bool {
        !self.is_api_note() && (self.description.contains("(the public API can change") || self.description.contains("(the public API changes"))
    }

    /// The place of the fact in the list of a step (design 37.18). The text output and the
    /// Markdown report (design 38.9) use it. The fact numbers do not change.
    /// - 0: a build code change that is no manifest fact (design 28.3: read this change first);
    /// - 1: an edit of a body with `code different`;
    /// - 2: a declaration, an added item, a removed item;
    /// - 3: each other code fact;
    /// - 4: a manifest fact;
    /// - 5: a docs fact, a file fact, and each other fact with `no code`.
    pub fn text_rank(&self) -> u8 {
        match self.category {
            Category::Manifest => 4,
            _ if self.build_code_change => 0,
            Category::Docs | Category::File => 5,
            _ if self.compare == Compare::NoCode => 5,
            Category::Semantics | Category::Macro | Category::Text if self.compare == Compare::CodeDifferent => 1,
            Category::Declaration | Category::Added | Category::Removed => 2,
            _ => 3,
        }
    }

    /// The place of the fact in a group of the review report (design 35.7, 36.3): a
    /// spotlight, a mismatch mark, a public API change, then the compare results.
    pub fn order(&self) -> u8 {
        if self.spotlight {
            0
        } else if self.mismatch() {
            1
        } else if self.api_change() {
            2
        } else {
            3 + self.compare.order()
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
    /// The cause that stopped the derivation of the rename or the move (batch 5x, part C, and
    /// design 36.1). Semilla gives a pair warning only for a pair that it does not derive.
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
        let mut text = format!("`{}` (removed) and `{}` (added) are {:.0}% similar: they can be one item with a new {what} and a second change", self.old, self.new, self.similarity * 100.0);
        if let Some(cause) = &self.stopped {
            // The cause that stopped the derivation (design 36.1).
            text.push_str(&format!(". Semilla does not derive one item: {cause}"));
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

/// The comments to show with each fact of a list (design 58.6): a comment shows one time, with
/// the first fact of the list that has it.
pub fn comments_shown<'a>(facts: &[&'a Fact]) -> Vec<Vec<&'a RelatedComment>> {
    let mut known: std::collections::BTreeSet<(&Position, &str)> = std::collections::BTreeSet::new();
    facts.iter().map(|fact| fact.comments.iter().filter(|comment| known.insert(comment.key())).collect()).collect()
}

/// The part `reached bodies:` of the control flow section (design 61.10, A5). The short form
/// folds the bodies with an equal result key: one line for each key, with the first
/// [`REACH_FOLD_NAMES`] names; a key of one body has its full line. `full` gives each body
/// with its chain.
fn reach_lines(flow: &FlowReport, full: bool) -> Vec<String> {
    if flow.reached.is_empty() && flow.reach_stops.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!("  reached bodies: {}", flow.reached.len())];
    if full {
        for body in &flow.reached {
            out.push(format!("    {} ({}): {}", body.name, body.place, body.text));
            out.push(format!("        chain: {}", body.chain));
        }
    } else {
        let mut keys: Vec<(&str, Vec<&ReachedBody>)> = Vec::new();
        for body in &flow.reached {
            match keys.iter_mut().find(|(key, _)| *key == body.key) {
                Some((_, list)) => list.push(body),
                None => keys.push((body.key.as_str(), vec![body])),
            }
        }
        for (key, list) in keys {
            if list.len() == 1 {
                out.push(format!("    {} ({}): {}", list[0].name, list[0].place, list[0].text));
                continue;
            }
            let names: Vec<&str> = list.iter().take(REACH_FOLD_NAMES).map(|body| body.name.as_str()).collect();
            let more = if list.len() > REACH_FOLD_NAMES { format!(", and {} more", list.len() - REACH_FOLD_NAMES) } else { String::new() };
            out.push(format!("    {} bodies: {key}: {}{more} (semilla status --verbose)", list.len(), names.join(", ")));
        }
    }
    out.extend(flow.reach_stops.iter().map(|stop| format!("    {stop}")));
    out
}

impl StepReport {
    /// The control flow section of the step (design 60, C2): `control flow: N bodies`, and one
    /// line for each body. `full` adds the full diff of each body (C3). A step with no graph
    /// data, or with no body with a result, has no section.
    pub fn flow_lines(&self, full: bool) -> Vec<String> {
        // Decided by Claude: a step with no body with a result has no section.
        let Some(flow) = self.flow.as_ref().filter(|flow| !flow.bodies.is_empty() || !flow.reached.is_empty()) else { return Vec::new() };
        let count = flow.bodies.len();
        let mut out = vec![format!("control flow: {count} bod{}", if count == 1 { "y" } else { "ies" })];
        for body in &flow.bodies {
            out.push(format!("  {} ({}): {}", body.name, body.place, body.line));
            if full {
                out.extend(body.details.iter().map(|line| format!("      {line}")));
            }
        }
        out.extend(flow.equivalence_stop.iter().map(|line| format!("  {line}")));
        out.extend(reach_lines(flow, full));
        out
    }

    /// The expansion section of the step (design 63, R63.3-5): `expansions: N changed`, and a
    /// line for each call (equal changes fold into one line in the short form). `full` gives
    /// each call with its configurations and each fact of its compare.
    pub fn expansion_section(&self, full: bool) -> Vec<String> {
        // R65.8-2: a step of the default mode with no expansion records says so in one line.
        if let Some(gap) = &self.expansion_gap {
            return vec![format!("expansions: not read: {gap}")];
        }
        let Some(flow) = self.flow.as_ref().filter(|flow| !flow.expansions.is_empty()) else { return Vec::new() };
        let count = |word: &str| flow.expansions.iter().filter(|site| site.result == word).count();
        let mut parts = Vec::new();
        for (word, label) in [("expansion changed", "changed"), ("expansion added", "added"), ("expansion removed", "removed")] {
            let n = count(word);
            if n > 0 {
                parts.push(format!("{n} {label}"));
            }
        }
        if flow.expansions_equal > 0 {
            parts.push(format!("{} equal", flow.expansions_equal));
        }
        let mut out = vec![format!("expansions: {}", parts.join(", "))];
        if full {
            for site in &flow.expansions {
                let configs = if site.configs.is_empty() { String::new() } else { format!(" (in {})", site.configs.join(", ")) };
                out.push(format!("  {} at `{}` ({}): {}{configs}; {}", site.call, site.at, site.place, site.result, site.definition));
                for fact in &site.facts {
                    out.push(format!("      {fact}"));
                }
                if !site.cause.is_empty() {
                    out.push(format!("      cause: {}", site.cause));
                }
                for body in &site.bodies {
                    out.push(format!("      body: {body}"));
                }
            }
            return out;
        }
        let mut keys: Vec<(&str, Vec<&ExpansionSite>)> = Vec::new();
        for site in &flow.expansions {
            match keys.iter_mut().find(|(key, _)| *key == site.key) {
                Some((_, list)) => list.push(site),
                None => keys.push((site.key.as_str(), vec![site])),
            }
        }
        for (key, list) in keys {
            if list.len() == 1 {
                out.push(format!("  {} at `{}` ({}): {key}", list[0].call, list[0].at, list[0].place));
                continue;
            }
            let names: Vec<String> = list.iter().take(REACH_FOLD_NAMES).map(|site| format!("`{}`", site.at)).collect();
            let more = if list.len() > REACH_FOLD_NAMES { format!(", and {} more", list.len() - REACH_FOLD_NAMES) } else { String::new() };
            out.push(format!("  {} calls: {key}: {}{more} (semilla status --verbose)", list.len(), names.join(", ")));
        }
        out
    }

    /// The count of the comments of the changed items that have no edit in this step (design
    /// 58.8): the count of the comment lines of the output with that mark. A comment of two
    /// facts counts one time.
    pub fn comments_no_edit(&self) -> usize {
        self.comment_list().iter().filter(|(_, comment)| comment.state == CommentState::NoEdit).count()
    }

    /// The count line of the comments with no edit (design 58.8), without the indent. No line
    /// for the count 0.
    pub fn comment_count_line(&self) -> Option<String> {
        comment_count_line(self.comments_no_edit())
    }

    /// The facts in the order of the output (design 54.5, 55.3): the build code changes, the
    /// facts below each fact of the agent, then the groups of the facts that Semilla filled
    /// in. In a step with tags: the facts of each tag, the facts in no tag, then the facts
    /// with no position. The order in one group is the order of [`Fact::text_rank`].
    pub fn output_order(&self) -> Vec<&Fact> {
        let tagged = !self.tags.is_empty();
        let key = |fact: &Fact| -> (usize, usize, u8) {
            let rank = fact.text_rank();
            if rank == 0 {
                return (0, 0, 0);
            }
            let (part, group) = match (tagged, self.tag_group(fact), self.agent_facts.iter().position(|agent| agent.facts.contains(&fact.id))) {
                (true, TagGroup::Tag(tag), _) => (1, tag),
                (true, TagGroup::Outside, _) => (2, 0),
                (true, TagGroup::NoPosition, _) => (3, 0),
                (false, _, Some(agent)) => (1, agent),
                (false, _, None) => (2, fact.filled_group()),
            };
            (part, group, rank)
        };
        let mut order: Vec<&Fact> = self.facts.iter().collect();
        order.sort_by_key(|fact| key(fact));
        order
    }

    /// Each comment of the changed items one time (design 58.6), with the first fact in the
    /// order of the output that has it.
    pub fn comment_list(&self) -> Vec<(&Fact, &RelatedComment)> {
        let order = self.output_order();
        order.iter().zip(comments_shown(&order)).flat_map(|(fact, comments)| comments.into_iter().map(|comment| (*fact, comment))).collect()
    }

    pub fn accepted(&self) -> bool {
        self.errors.is_empty()
            && self.uncovered.is_empty()
            && self.facts.iter().all(|fact| !fact.spotlight || fact.reason.is_some())
            && self.claims.iter().all(|claim| claim.status.is_verified())
            && self.agent_facts.iter().all(|fact| fact.holds)
            && self.tags.iter().all(|tag| tag.error.is_none())
    }

    /// The lines of the cover check (design 55.4), without the indent. A record of an earlier
    /// version has no cover check and no line. A changed line with no fact is a defect of
    /// Semilla: its lines have the mark `!`.
    pub fn cover_lines(&self) -> Vec<String> {
        if self.changed_lines == 0 {
            return Vec::new();
        }
        if self.no_fact.is_empty() {
            return vec![format!("cover: each of the {} changed line(s) is in a fact", self.changed_lines)];
        }
        let missing: usize = self.no_fact.iter().map(Position::lines).sum();
        vec![format!("! cover: {missing} of the {} changed line(s) are in no fact (a defect of Semilla: read these lines in the diff)", self.changed_lines), format!("! changed lines with no fact: {}", Position::list(&self.no_fact))]
    }

    /// The lines that tell how to get the hunks of the step (design 55.8): the line diff
    /// command with the two trees of the step. An agent that runs the command gets the hunks
    /// that a place of a tag selects. If git gave no line diff, the hunks are the hunks of
    /// the diff of Semilla: `full` gives one line for each hunk.
    pub fn hunk_lines(&self, full: bool) -> Vec<String> {
        if self.hunks.is_empty() {
            return Vec::new();
        }
        if !self.own_diff {
            let trees = match &self.diff_trees[..] {
                [old, new] => format!("{old} {new}"),
                _ => "OLD_TREE NEW_TREE".to_string(),
            };
            return vec![format!("hunks: `{LINE_DIFF_COMMAND} {trees}`")];
        }
        let mut lines = vec![format!("hunks: git gave no line diff of this step; the {} hunk(s) are the hunks of the line diff of Semilla{}", self.hunks.len(), if full { ":" } else { " (`semilla status --verbose` prints them)" })];
        if full {
            lines.extend(self.hunks.iter().map(|hunk| format!("  {} {}", hunk.file, hunk.header())));
        }
        lines
    }

    /// The cause lines of the changed lines with no fact (design 56.4), without the indent.
    /// Each line has the smallest item of the tree with the lines, the state of its tokens,
    /// and the lines.
    pub fn cause_lines(&self) -> Vec<String> {
        self.no_fact_causes.iter().map(|cause| format!("! cause: {}: {}", cause.cause, Position::list(&cause.positions))).collect()
    }

    /// The lines that only the rule of the equal tokens puts in a fact (design 59, B-b),
    /// without the indent: "equal tokens: in `ITEM` (old lines A-B, new lines C-D): LINES".
    pub fn equal_token_lines(&self) -> Vec<String> {
        self.equal_token_lines.iter().map(|group| format!("equal tokens: {}: {}", group.cause, Position::list(&group.positions))).collect()
    }

    /// The lines of the cover check that need the reader: the lines with the mark `!`.
    pub fn no_fact_lines(&self) -> Vec<String> {
        self.cover_lines().into_iter().filter(|line| line.starts_with('!')).collect()
    }

    /// The fact with the number `id`.
    pub fn fact(&self, id: &str) -> Option<&Fact> {
        self.facts.iter().find(|fact| fact.id == id)
    }

    /// The positions of the facts with the numbers `ids`, in the normal form (design 55.2).
    pub fn positions_of(&self, ids: &[String]) -> Vec<Position> {
        Position::normal(ids.iter().filter_map(|id| self.fact(id)).flat_map(|fact| fact.positions.iter().cloned()).collect())
    }

    /// The text of the counts of a group of facts (design 55.3): "14 fact(s): 9 semantics, 3
    /// rename, 2 added". The largest category is first.
    pub fn count_text(&self, ids: &[String]) -> String {
        let mut counts: Vec<(usize, Category)> = Vec::new();
        for fact in ids.iter().filter_map(|id| self.fact(id)) {
            match counts.iter_mut().find(|(_, category)| *category == fact.category) {
                Some((count, _)) => *count += 1,
                None => counts.push((1, fact.category)),
            }
        }
        counts.sort_by_key(|(count, category)| (std::cmp::Reverse(*count), Category::ALL.iter().position(|other| other == category)));
        let list: Vec<String> = counts.iter().map(|(count, category)| format!("{count} {}", category.name())).collect();
        if list.is_empty() {
            "0 fact(s)".to_string()
        } else {
            format!("{} fact(s): {}", ids.len(), list.join(", "))
        }
    }

    /// The head line of a tag in an output (design 55.3), without the indent: the facts
    /// that are fully in the tag, the facts that are in the tag in part, and the changed
    /// lines that the places select.
    pub fn tag_head(&self, tag: &Tag) -> String {
        let part = if tag.partial.is_empty() { String::new() } else { format!("; {} fact(s) in part", tag.partial.len()) };
        format!("tag `{}` ({}): {}{part}; {} changed line(s)", tag.name, tag.places.join(", "), self.count_text(&tag.facts), tag.lines)
    }

    /// The group of a fact in the output of a step with tags (design 55.3): the first tag
    /// that has the full fact, else the first tag that has the fact in part. A fact with no
    /// position is in no tag and in its own group. A step with no tag has one group.
    pub fn tag_group(&self, fact: &Fact) -> TagGroup {
        if self.tags.is_empty() {
            return TagGroup::Outside;
        }
        if fact.positions.is_empty() {
            return TagGroup::NoPosition;
        }
        let full = self.tags.iter().position(|tag| tag.facts.contains(&fact.id));
        match full.or_else(|| self.tags.iter().position(|tag| tag.partial.contains(&fact.id))) {
            Some(tag) => TagGroup::Tag(tag),
            None => TagGroup::Outside,
        }
    }

    /// The numbers of the facts of the tag that show in a different tag (design 55.3): the
    /// facts that show in a tag above it, and the facts that show in a tag below it. A fact
    /// of two tags shows in the first tag that has the full fact.
    pub fn shown_in_other_tag(&self, index: usize) -> [Vec<String>; 2] {
        let tag = &self.tags[index];
        let mut out = [Vec::new(), Vec::new()];
        for id in tag.facts.iter().chain(&tag.partial) {
            match self.fact(id).map(|fact| self.tag_group(fact)) {
                Some(TagGroup::Tag(other)) if other < index => out[0].push(id.clone()),
                Some(TagGroup::Tag(other)) if other > index => out[1].push(id.clone()),
                _ => {}
            }
        }
        out
    }

    /// The numbers of the facts with a position that are in no tag, in the order of the
    /// facts. A fact that is in a tag in part shows with that tag: it is not in this list.
    pub fn not_in_a_tag(&self) -> Vec<String> {
        self.facts.iter().filter(|fact| self.tag_group(fact) == TagGroup::Outside).map(|fact| fact.id.clone()).collect()
    }

    /// The numbers of the facts with no position (design 55.2, rule 6) of a step with tags.
    /// A tag selects changed lines: such a fact cannot be in a tag.
    pub fn no_position(&self) -> Vec<String> {
        self.facts.iter().filter(|fact| self.tag_group(fact) == TagGroup::NoPosition).map(|fact| fact.id.clone()).collect()
    }

    /// The count of the changed lines of the positions of the facts `ids`: each line one time.
    pub fn line_count(&self, ids: &[String]) -> usize {
        self.positions_of(ids).iter().map(Position::lines).sum()
    }

    /// The tags of the facts `ids` as a text for a line of the gap list (design 55.3), or
    /// `None` for a step with no tag.
    pub fn tags_of(&self, ids: &[String]) -> Option<String> {
        if self.tags.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        for tag in &self.tags {
            let inside: Vec<&str> = ids.iter().filter(|id| tag.facts.contains(id) || tag.partial.contains(id)).map(String::as_str).collect();
            if inside.len() == ids.len() {
                parts.push(format!("`{}`", tag.name));
            } else if !inside.is_empty() {
                parts.push(format!("`{}` ({})", tag.name, inside.join(", ")));
            }
        }
        let outside: Vec<&str> = ids.iter().filter(|id| !self.tags.iter().any(|tag| tag.facts.contains(id) || tag.partial.contains(id))).map(String::as_str).collect();
        if outside.len() == ids.len() {
            return Some("not in a tag".to_string());
        }
        if !outside.is_empty() {
            parts.push(format!("not in a tag ({})", outside.join(", ")));
        }
        Some(format!("{}: {}", if parts.len() == 1 && outside.is_empty() { "tag" } else { "tags" }, parts.join(", ")))
    }

    /// The items of the gap list, with no item two times.
    pub fn gap_items(&self) -> Vec<&str> {
        let mut items: Vec<&str> = self.gaps.iter().flat_map(|gap| gap.items.iter().map(String::as_str)).collect();
        items.sort();
        items.dedup();
        items
    }

    /// The line with the count of the items that are not stated. A step with a fact of the
    /// agent always has the line (design 54.14). `None`: no gap and no fact of the agent.
    pub fn gap_line(&self) -> Option<String> {
        gap_count_line(self.gap_items().len(), self.gaps.iter().map(|gap| gap.places.len()).sum()).or_else(|| (!self.agent_facts.is_empty()).then(|| NO_GAP_LINE.to_string()))
    }

    /// The lines of the gap list (design 54.4), largest group first. With `limit`, the first
    /// lines and a count of the other lines. Each line starts with two spaces.
    pub fn gap_lines(&self, limit: Option<usize>) -> Vec<String> {
        let Some(head) = self.gap_line() else { return Vec::new() };
        let mut lines = vec![format!("  {head}")];
        let shown = limit.unwrap_or(self.gaps.len()).min(self.gaps.len());
        for gap in &self.gaps[..shown] {
            lines.push(format!("  ? {}", gap.line()));
            lines.push(format!("      in: {}", gap.items.join(", ")));
            // Design 55.2: the lines of the edits. Design 55.3: the tags of the facts.
            let positions = self.positions_of(&gap.facts);
            if !positions.is_empty() {
                lines.push(format!("      lines: {}", Position::list(&positions)));
            }
            if let Some(tags) = self.tags_of(&gap.facts) {
                lines.push(format!("      {tags}"));
            }
            lines.push(format!("      to state it: {}", gap.suggestion()));
        }
        if shown < self.gaps.len() {
            lines.push(format!("  and {} more line(s) with {} place(s): `semilla status --verbose` shows each line", self.gaps.len() - shown, self.gaps[shown..].iter().map(|gap| gap.places.len()).sum::<usize>()));
        }
        lines
    }

    /// The line with the one command that adds a fact for each shown gap line to the stored step.
    pub fn gap_command(&self, limit: Option<usize>) -> Option<String> {
        let shown = limit.unwrap_or(self.gaps.len()).min(self.gaps.len());
        let mut options: Vec<&str> = Vec::new();
        // R71.3-1: an undecided line has two forms, so the one command does not choose one.
        for gap in self.gaps[..shown].iter().filter(|gap| !gap.undecided) {
            if !options.contains(&gap.option.as_str()) {
                options.push(&gap.option);
            }
        }
        (!options.is_empty()).then(|| format!("{GAP_COMMAND}{} {}", self.step, options.join(" ")))
    }
}

impl Report {
    /// The count line of the comments with no edit of each step of the change set together
    /// (design 58.8), or `None` for the count 0.
    pub fn comment_count_line(&self) -> Option<String> {
        comment_count_line(self.steps.iter().map(StepReport::comments_no_edit).sum())
    }

    /// The line with the count of the items that are not stated, for each step of the change
    /// set together (design 54.4), or `None` for no gap.
    pub fn gap_line(&self) -> Option<String> {
        gap_count_line(self.steps.iter().map(|step| step.gap_items().len()).sum(), self.steps.iter().flat_map(|step| &step.gaps).map(|gap| gap.places.len()).sum())
            .or_else(|| self.steps.iter().any(|step| !step.agent_facts.is_empty()).then(|| NO_GAP_LINE.to_string()))
    }
}

/// The count line of a step or a change set with a fact of the agent and with no gap
/// (design 54.14).
pub const NO_GAP_LINE: &str = "not stated: 0 item(s)";

/// What Semilla checks for the facts of the agent (design 54.12). The text is true for each
/// kind of fact: a `change`, a `rename`, a `move`, and a `rewrite`.
pub const AGENT_FACTS_NOTE: &str = "Semilla checks each fact against the edit. It does not check the sentence of a `change`. It does not check the behavior after a `rewrite`";

/// The count line of the comments with no edit (design 58.8). No line for the count 0.
pub fn comment_count_line(count: usize) -> Option<String> {
    (count > 0).then(|| format!("comments: {count} comment(s) of the changed items have {COMMENT_NO_EDIT}"))
}

/// The line with the count of the items that are not stated (design 54.4).
pub fn gap_count_line(items: usize, places: usize) -> Option<String> {
    (items > 0).then(|| format!("not stated: {items} item(s) with {places} place(s) that can change the behavior of existing code and that no fact of the agent states"))
}

/// The groups of the facts that Semilla filled in (design 48.3), in the order of the output.
pub const FILLED_GROUPS: &[&str] = &["new and removed items", "declarations", "tests and benches", "other facts"];

impl Fact {
    /// The group of the fact in the list of the facts that Semilla filled in (design 48.3): an
    /// index of [`FILLED_GROUPS`].
    pub fn filled_group(&self) -> usize {
        if self.test {
            2
        } else {
            match self.category {
                Category::Added | Category::Removed => 0,
                Category::Declaration => 1,
                _ => 3,
            }
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
    /// Design 62: the open items of a change set of a tag after the commit, one text for each
    /// kind (`2 open spotlights`). `None`: the change set has the steps of the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_commit: Option<Vec<String>>,
    pub accepted: bool,
}

/// The first part of the result line of a tag after the commit (design 62).
pub const AFTER_COMMIT: &str = "tagged after the commit";

/// The line of a review of a commit that Semilla tagged after the commit (design 62).
pub const AFTER_COMMIT_HEADER: &str = "tagged after the commit (no steps of the agent)";

/// The text of a spotlight with no reason in a tag after the commit (design 62).
pub const OPEN_SPOTLIGHT: &str = "open: no reason given";

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

/// The line under a fact with a changed body that has `unsafe` code (design 36.5).
pub const UNSAFE_LINE: &str = "! unsafe code: the changed body has an `unsafe` block";

/// The sub line of a fact that changes the public API (design 36.3). It asks for no reason.
pub const API_LINE: &str = "! public API: the compare does not cover the code of a different crate";

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
    /// The count of the facts of the steps with no compiled check (design 50).
    pub off: usize,
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
            off: count(&|fact| fact.compare.is_off()),
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
        let mut parts: Vec<String> = self.compare.iter().map(|(compare, count)| format!("{count} {}{}", compare.name(), if *compare == Compare::CodeEqual { marked } else { "" })).collect();
        // A change set with the two modes (design 50): the facts with no compare result have
        // one count. A change set with no compiled step has no compare counts.
        if self.off > 0 && !parts.is_empty() {
            parts.push(format!("{} with no compiled check", self.off));
        }
        // A count of zero is not in the line.
        let mut marks = Vec::new();
        if self.spotlight > 0 {
            marks.push(format!("{} with a spotlight", self.spotlight));
        }
        if self.mismatch > 0 {
            marks.push(format!("{} with a mismatch mark", self.mismatch));
        }
        let marks = if marks.is_empty() { String::new() } else { format!(" ({})", marks.join(", ")) };
        if parts.is_empty() {
            return format!("{} {noun}{marks}", self.total);
        }
        format!("{} {noun}: {}{marks}", self.total, parts.join(", "))
    }
}

/// The largest number of bodies that a list shows with each name (batch 5x, part E). A longer
/// list shows the count and the scope ([`list_scope`]). `semilla status --verbose` and the
/// report with `--full` have each name.
pub const LIST_LIMIT: usize = 8;

/// The largest number of causes that the text of a "build input" spotlight shows (design 37.14).
pub const CAUSE_LIMIT: usize = 5;

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
/// - Else: `change` (design 36.3). The kinds `refactor` and `behavior` read as a verdict:
///   no derived group has them.
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
        if facts.iter().all(|f| f.category == Category::Move) {
            return Some(crate::GroupKind::Move);
        }
        if facts.iter().all(|f| f.category == Category::Rename) {
            return Some(crate::GroupKind::Rename);
        }
    }
    Some(crate::GroupKind::Change)
}

/// The kind of a group after one more step: equal kinds stay; each mix gives `change`.
pub fn merge_kinds(old: crate::GroupKind, new: crate::GroupKind) -> crate::GroupKind {
    if old == new { old } else { crate::GroupKind::Change }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(place: &str, state: CommentState, text: &str, line: usize, facts: &[&str]) -> RelatedComment {
        let lines = text.matches('\n').count();
        RelatedComment { place: place.into(), state, text: text.into(), position: Position { file: "src/lib.rs".into(), old: state == CommentState::Removed, start: line, end: line + lines }, facts: facts.iter().map(|fact| fact.to_string()).collect() }
    }

    fn fact_with(id: &str, comments: Vec<RelatedComment>) -> Fact {
        Fact { id: id.into(), part: "edit".into(), description: format!("changed {id}"), status: "derived".into(), category: Category::Semantics, comments, ..Default::default() }
    }

    /// Design 58.4 and 58.7: the lines of a comment. A text of one line of 80 characters or
    /// less is in the line. Each other text is a block with each line of the comment, and no
    /// line has a cut. A comment with an edit shows the number of its fact, and no text.
    // rule+ R58.7-1
    // rule- R58.7-1
    // rule- R58.4-2
    #[test]
    fn the_lines_of_a_comment_have_the_full_text_or_the_number_of_a_fact() {
        let short = comment("docs of `shop::price`", CommentState::NoEdit, "/// Gives the price.", 1, &[]);
        assert_eq!(short.lines(), ["docs of `shop::price`, no edit in this step (src/lib.rs:1): `/// Gives the price.`"]);
        let long_line = format!("// {}", "word ".repeat(40));
        let long = comment("comment above line 5", CommentState::NoEdit, &long_line, 4, &[]);
        assert_eq!(long.lines(), ["comment above line 5, no edit in this step (src/lib.rs:4):".to_string(), format!("    | {long_line}")]);
        let block = comment("comment above `shop::price`", CommentState::NoEdit, "/* One.\n\n   Two.  */", 1, &[]);
        assert_eq!(block.lines(), ["comment above `shop::price`, no edit in this step (src/lib.rs:1-3):", "    | /* One.", "    |", "    |    Two.  */"]);
        for (state, word) in [(CommentState::Changed, "changed"), (CommentState::Added, "added"), (CommentState::Removed, "removed"), (CommentState::Moved, "moved")] {
            let edited = comment("docs of `shop::price`", state, "/// Gives the price.", 1, &["f2", "f3"]);
            assert_eq!(edited.lines(), [format!("docs of `shop::price`, {word} in this step: see f2, f3")]);
            assert!(!edited.shows_text());
        }
    }

    /// Design 58.4: a comment with no pair in the other tree and no fact never gets the mark
    /// "no edit in this step". Its line tells the state, and it shows the full text. Negative
    /// test: a comment with a pair of equal text has the mark.
    // rule+ R58.4-3
    // rule- R58.4-3
    #[test]
    fn a_comment_with_no_pair_and_no_fact_tells_its_state() {
        let open = comment("docs of `shop::price`", CommentState::NoPair, "/// Gives the price.", 1, &[]);
        assert_eq!(open.lines(), ["docs of `shop::price`, with no pair in the old tree and no fact (src/lib.rs:1): `/// Gives the price.`"]);
        assert!(!open.head().contains(COMMENT_NO_EDIT));
        let paired = comment("docs of `shop::fee`", CommentState::NoEdit, "/// Gives the fee.", 5, &[]);
        assert!(paired.head().contains(COMMENT_NO_EDIT) && !paired.head().contains("with no pair"));
        // The count line has only the comments with no edit.
        let step = StepReport { facts: vec![fact_with("f1", vec![open, paired])], ..Default::default() };
        assert_eq!(step.comments_no_edit(), 1);
    }

    /// Design 58.8: the count line has each comment with no edit one time. Negative test: a
    /// step with no such comment has no count line.
    // rule+ R58.8-1
    // rule- R58.8-1
    #[test]
    fn the_count_line_of_the_comments_with_no_edit() {
        let docs = comment("docs of `shop::price`", CommentState::NoEdit, "/// Gives the price.", 1, &[]);
        let above = comment("comment above line 5", CommentState::NoEdit, "// The fee.", 4, &[]);
        let edited = comment("docs of `shop::count`", CommentState::Changed, "/// Gives the count.", 9, &["f3"]);
        let step = StepReport { facts: vec![fact_with("f1", vec![docs.clone()]), fact_with("f2", vec![docs.clone(), above]), fact_with("f4", vec![edited.clone()])], ..Default::default() };
        assert_eq!(step.comment_count_line().as_deref(), Some("comments: 2 comment(s) of the changed items have no edit in this step"));
        let list: Vec<(&str, &str)> = step.comment_list().iter().map(|(fact, comment)| (fact.id.as_str(), comment.place.as_str())).collect();
        assert_eq!(list, [("f1", "docs of `shop::price`"), ("f2", "comment above line 5"), ("f4", "docs of `shop::count`")]);
        let report = Report { steps: vec![step.clone(), StepReport { step: 1, facts: vec![fact_with("f1", vec![docs])], ..Default::default() }], ..Default::default() };
        assert_eq!(report.comment_count_line().as_deref(), Some("comments: 3 comment(s) of the changed items have no edit in this step"));
        let none = StepReport { facts: vec![fact_with("f4", vec![edited]), fact_with("f5", Vec::new())], ..Default::default() };
        assert_eq!(none.comment_count_line(), None);
        assert_eq!(Report { steps: vec![none], ..Default::default() }.comment_count_line(), None);
        assert_eq!(comment_count_line(0), None);
    }

    /// Design 58.9: the record of a fact keeps its comments (the place, the state, the text,
    /// the position, the facts). Negative test: a fact with no comment has no key `comments`,
    /// thus the record of a step with no comment is the record of the version before.
    // rule+ R58.9-1
    // rule- R58.9-1
    #[test]
    fn the_record_of_a_fact_keeps_its_comments() {
        let fact = fact_with("f1", vec![comment("docs of `shop::price`", CommentState::NoEdit, "/// One.\n/// Two.", 1, &[]), comment("comment above line 5", CommentState::Removed, "// The fee.", 4, &["f2"])]);
        let record = serde_json::to_string(&fact).unwrap();
        assert!(record.contains("\"comments\"") && record.contains("/// One.\\n/// Two."), "{record}");
        let read: Fact = serde_json::from_str(&record).unwrap();
        assert_eq!(read, fact);
        assert_eq!(read.comments[0].lines(), fact.comments[0].lines());
        let record = serde_json::to_string(&fact_with("f1", Vec::new())).unwrap();
        assert!(!record.contains("comments"), "{record}");
    }

    /// Design 58.9: a record of a version before batch 9m has no key `comments`. It reads,
    /// and its fact has no comment and no count line.
    // rule+ R58.9-4
    #[test]
    fn a_record_with_no_comments_key_reads() {
        let record = r#"{"id":"f1","part":"edit","description":"changed literal `10` → `5` at `shop::price/tail/rhs/rhs`","status":"derived","category":"semantics","positions":[{"file":"src/lib.rs","start":3,"end":3}]}"#;
        let fact: Fact = serde_json::from_str(record).unwrap();
        assert!(fact.comments.is_empty() && fact.positions.len() == 1 && fact.category == Category::Semantics);
        let step = StepReport { facts: vec![fact], ..Default::default() };
        assert_eq!((step.comment_count_line(), step.comment_list().len()), (None, 0));
    }

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
        let mut fact = Fact { category: Category::Semantics, compare: Compare::CodeEqual, ..Default::default() };
        assert_eq!((fact.mark(), fact.part_mark()), (String::new(), String::new()), "a compare with the build code of the base has no mark");
        fact.build_code = Some(2);
        assert_eq!(fact.mark(), " (with the build code of step 2)");
        assert_eq!(fact.part_mark(), " (with the build code of step 2)");
        // A fact with a different result has no mark in its first line.
        fact.compare = Compare::CodeDifferent;
        assert_eq!(fact.mark(), "");
        assert_eq!(fact.label(), "semantics · code different");
        assert!(!fact.mismatch());
        fact.category = Category::Visibility;
        assert!(fact.mismatch(), "the text of a visibility change gives no cause for different code");
        assert_eq!(fact.order(), 1);
        // Batch 7b, part 3: a public API change is after a mismatch mark and before each
        // fact with `code different`, also when its compare result is `code equal`.
        let api = Fact { category: Category::Visibility, compare: Compare::CodeEqual, description: "visibility of `m::f`: `private` → `pub` (the public API changes: a different crate can now use the item)".into(), ..Default::default() };
        let different = Fact { category: Category::Semantics, compare: Compare::CodeDifferent, ..Default::default() };
        let equal = Fact { category: Category::Semantics, compare: Compare::CodeEqual, ..Default::default() };
        assert!(api.api_change() && !equal.api_change());
        assert!(fact.order() < api.order() && api.order() < different.order() && different.order() < equal.order());
        let possible = Fact { description: "qualifiers of `m::f` removed: `const` (the public API can change: a different crate can use the item)".into(), ..equal.clone() };
        assert!(possible.api_change() && possible.order() == api.order());
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
            fact(Category::Semantics, Compare::CodeDifferent, false),
            fact(Category::Semantics, Compare::CodeDifferent, true),
            fact(Category::Rename, Compare::CodeEqual, false),
            fact(Category::Docs, Compare::NoCode, false),
        ];
        let counts = FactCounts::of(facts.iter());
        assert_eq!(counts.text(""), "4 facts: 2 code different, 1 code equal, 1 no code (1 with a spotlight)");
        assert_eq!(counts.text(" (1 with the build code of step 3)"), "4 facts: 2 code different, 1 code equal (1 with the build code of step 3), 1 no code (1 with a spotlight)");
        assert_eq!(counts.category, vec![(Category::Rename, 1), (Category::Semantics, 2), (Category::Docs, 1)]);
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
