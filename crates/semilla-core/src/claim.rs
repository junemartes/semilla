//! Claims, steps, groups, subgroups, and the change set.

use serde::{Deserialize, Serialize};

use crate::anchor::Anchor;

/// One semantic change that the agent declares.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    /// A unique ID in the change set, for example `c3`.
    pub id: String,
    /// The operation name from the rule table, for example `add-if`.
    pub op: String,
    /// The location. File-level ops (`bump-dependency`) have no anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Anchor>,
    /// The other arguments of the operation.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub args: serde_json::Map<String, serde_json::Value>,
}

impl Claim {
    /// Returns a string argument.
    pub fn arg_str(&self, name: &str) -> Option<&str> {
        self.args.get(name).and_then(|value| value.as_str())
    }

    /// Returns a list of strings argument (for example `affects`).
    pub fn arg_list(&self, name: &str) -> Vec<String> {
        match self.args.get(name) {
            Some(serde_json::Value::Array(values)) => {
                values.iter().filter_map(|value| value.as_str().map(str::to_string)).collect()
            }
            // The comma form `a,b,c` is the same list as `["a", "b", "c"]`.
            Some(serde_json::Value::String(value)) => value.split(',').map(|part| part.trim().to_string()).filter(|part| !part.is_empty()).collect(),
            _ => Vec::new(),
        }
    }
}

/// The verification unit: a set of simultaneous claims between two tree snapshots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub index: usize,
    pub intent: String,
    pub claims: Vec<Claim>,
    /// The git tree before the step.
    pub tree_before: String,
    /// The git tree after the step.
    pub tree_after: String,
    /// The reasons of the agent for the spotlight facts, by the anchor of the fact (`semilla
    /// reason`). Free text for the reviewer.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub reasons: std::collections::BTreeMap<String, String>,
    /// [R83.1-3] True only for the check of the virtual crate of an expansion compare (design
    /// 63): a check with no compiler data, so no fact gets a compare result. A step of the
    /// author has no mode (design 83): each step builds the two trees and has their MIR. The
    /// field is not stored. A stored step of an earlier version can have the field `compiled`
    /// (the mode of design 50); the reader ignores it.
    #[serde(skip)]
    pub syntax_only: bool,
    /// The result of the build check of the new tree of the step (design 53), from the machine
    /// of the author. A stored step with no field is a step of an earlier version: it has no
    /// result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_check: Option<BuildCheck>,
    /// The facts of the agent (design 54), as the agent gave them. Each check of the step
    /// checks them again. A stored step with no field is a step with no agent fact.
    #[serde(default, skip_serializing_if = "AgentFacts::is_empty")]
    pub agent: AgentFacts,
    /// The tags of the agent (design 55.3): each `--tag` text `PLACE[,PLACE]=NAME`, in the order
    /// of the command. A tag is not a fact. A stored step with no field has no tag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// The facts of the agent for one step (design 54): the texts of the options `--same` and
/// `--change`, in the order of the command.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentFacts {
    /// Each `--same` text: `rename OLD NEW`, `move FROM TO`, or `rewrite FROM => TO`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub same: Vec<String>,
    /// Each `--change` text: `ITEM[,ITEM]=TEXT`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub change: Vec<String>,
    /// Each `--intent FILE` (design 67, part D): the path and the text of the file. The step
    /// note keeps the text, so a later check reads the same claims.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intent: Vec<IntentFile>,
    /// The bodies that the claims of the intent files read at the last check, with the hash of
    /// each body (design 67, part D): `CLAIM_ID before::PATH HASH`. A claim whose bodies
    /// changed is stale.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intent_reads: Vec<String>,
}

/// An intent file of a step (design 67): the path as the agent gave it, and the text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentFile {
    pub path: String,
    pub text: String,
}

impl AgentFacts {
    pub fn is_empty(&self) -> bool {
        self.same.is_empty() && self.change.is_empty() && self.intent.is_empty()
    }

    /// The options of a command that gives these facts: `--same 'TEXT'`, `--change 'TEXT'`, and
    /// `--intent 'FILE'`.
    pub fn options(&self) -> Vec<String> {
        let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
        self.same
            .iter()
            .map(|text| format!("--same {}", quote(text)))
            .chain(self.change.iter().map(|text| format!("--change {}", quote(text))))
            .chain(self.intent.iter().map(|file| format!("--intent {}", quote(&file.path))))
            .collect()
    }
}

/// The result of the build check of a stored step (design 53). A step of `semilla step` that
/// does not build is not stored, so only a step of a tag after the commit (design 62) can have
/// the results `fails` and `cannot_run`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum BuildCheck {
    /// Each target of the new tree builds.
    Passes,
    /// The check did not run. Only a stored step of an earlier version has this result: the
    /// option that turned the check off is gone (design 83).
    NotRun,
    /// Some targets of the new tree do not build, and Semilla found no failure that the step
    /// makes (rule 8 of design 53). The JSON name stays `base_fails`.
    BaseFails {
        /// The targets that do not build before the step and after it, in the words of cargo:
        /// `` `demo` (bench "speed") ``.
        #[serde(default)]
        targets: Vec<String>,
        /// The targets that do not build after the step, and that cargo did not check before
        /// the step: a different target did not build then. Semilla cannot tell if the step
        /// makes their errors.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        not_checked: Vec<String>,
    },
    /// Design 62: some targets build before the commit and do not build after it. The commit
    /// is already there, so the note keeps the step with this result.
    Fails {
        /// The targets that the commit breaks, in the words of cargo.
        #[serde(default)]
        targets: Vec<String>,
        /// The first errors of the compiler.
        #[serde(default)]
        errors: Vec<String>,
    },
    /// Design 62: the check cannot run on this machine (for example, a dependency is not in
    /// the cache of cargo). This is not a result of the code.
    CannotRun {
        /// The cause, in one line.
        #[serde(default)]
        cause: String,
    },
}

/// "the target A" or "the targets A, B", with the verb for one target or for many.
fn targets_text(targets: &[String], one: &str, many: &str) -> String {
    if targets.len() == 1 { format!("the target {} {one}", targets[0]) } else { format!("the targets {} {many}", targets.join(", ")) }
}

impl BuildCheck {
    /// The result, in the words of each output. Only a tree in which each target builds has
    /// the word `passes`.
    pub fn text(&self) -> &'static str {
        match self {
            BuildCheck::Passes => "passes",
            BuildCheck::NotRun => "not run",
            BuildCheck::BaseFails { .. } => "no new failure found",
            BuildCheck::Fails { .. } => "fails",
            BuildCheck::CannotRun { .. } => "cannot run",
        }
    }

    /// What Semilla did not check, in plain words: one sentence for each gap. A result
    /// `passes` has none. `base_failed` names the targets that did not build before the step,
    /// when the caller knows them (the command that records the step).
    pub fn limits(&self, base_failed: &[String]) -> Vec<String> {
        match self {
            BuildCheck::Passes => Vec::new(),
            BuildCheck::NotRun => vec!["the author turned the check off, so Semilla did not check that this code builds".to_string()],
            BuildCheck::BaseFails { targets, not_checked } => {
                let mut lines = Vec::new();
                if !targets.is_empty() {
                    let these = if targets.len() == 1 { "this target" } else { "these targets" };
                    let them = if targets.len() == 1 { "it" } else { "them" };
                    lines.push(format!("{} before the step and after it, so Semilla did not check a new error of the step in {these}, or in a target that needs {them}", targets_text(targets, "does not build", "do not build")));
                }
                if !not_checked.is_empty() {
                    let (it, error) = if not_checked.len() == 1 { ("it", "this error") } else { ("them", "these errors") };
                    let cause = if base_failed.is_empty() { "a different target did not build then".to_string() } else { format!("{} then", targets_text(base_failed, "did not build", "did not build")) };
                    lines.push(format!("{} after the step, and cargo did not check {it} before the step ({cause}), so Semilla cannot tell if the step makes {error}", targets_text(not_checked, "does not build", "do not build")));
                }
                lines
            }
            BuildCheck::Fails { targets, .. } => vec![format!("{} after the commit (they build before it)", targets_text(targets, "does not build", "do not build"))],
            BuildCheck::CannotRun { cause } => vec![format!("the check cannot run on the machine of the tag ({cause}), so Semilla did not check that this code builds")],
        }
    }

    /// True for a result that is an open item of a tag after the commit (design 62): the
    /// code after the commit does not build, or the check cannot run.
    pub fn is_open(&self) -> bool {
        matches!(self, BuildCheck::Fails { .. } | BuildCheck::CannotRun { .. })
    }

    /// True for a result that a reviewer must see with a mark: Semilla did not see that each
    /// target of the step builds.
    pub fn is_gap(&self) -> bool {
        !matches!(self, BuildCheck::Passes)
    }
}

/// The lines of the build check for a reviewer of a change set (design 53): one line for each
/// result, with its steps. A step with no stored result (an earlier version) has no line.
pub fn build_check_lines(steps: &[Step]) -> Vec<String> {
    let mut groups: Vec<(&BuildCheck, Vec<usize>)> = Vec::new();
    for step in steps {
        let Some(check) = &step.build_check else { continue };
        match groups.iter_mut().find(|(known, _)| *known == check) {
            Some((_, list)) => list.push(step.index),
            None => groups.push((check, vec![step.index])),
        }
    }
    groups
        .into_iter()
        .map(|(check, list)| {
            let place = if list.len() == steps.len() && list.len() > 1 {
                "each step".to_string()
            } else {
                format!("step{} {}", if list.len() == 1 { "" } else { "s" }, list.iter().map(|index| index.to_string()).collect::<Vec<_>>().join(", "))
            };
            let why: String = check.limits(&[]).iter().enumerate().map(|(index, limit)| format!("{}{limit}", if index == 0 { ": " } else { "; " })).collect();
            format!("{}build check of {place}: {}{why}", if check.is_gap() { "! " } else { "" }, check.text())
        })
        .collect()
}


/// The kind of a group. Some kinds have rules that the verifier checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    Refactor,
    Rename,
    Move,
    Api,
    Tests,
    Deps,
    Fix,
    Feature,
    Behavior,
    /// The derived kind of a group that is not only renames and not only moves (design
    /// 36.3). The word gives no verdict.
    Change,
}

/// A set of steps with one intent. It is about the size of one commit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub title: String,
    pub kind: GroupKind,
    #[serde(default)]
    pub why: String,
    /// The step indexes.
    pub steps: Vec<usize>,
}

/// The role of a subgroup. The engine suggests roles; the agent can edit them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Interface,
    Core,
    Tests,
    Config,
    Propagation,
    Mechanical,
    /// Doc and comment changes (`edit-doc`): not neutral, so not in `mechanical`.
    Docs,
    Custom,
}

impl Role {
    /// The review order of the roles (design section 11.2).
    pub fn order(self) -> u8 {
        match self {
            Role::Interface => 0,
            Role::Core => 1,
            Role::Custom => 2,
            Role::Tests => 3,
            Role::Config => 4,
            Role::Propagation => 5,
            Role::Mechanical => 6,
            Role::Docs => 7,
        }
    }
}

/// A slice of the claims in a group.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subgroup {
    /// A stable ID, for example `G1:core`.
    #[serde(default)]
    pub id: String,
    pub group: String,
    pub role: Role,
    pub title: String,
    #[serde(default)]
    pub why: String,
    /// The claim IDs.
    pub claims: Vec<String>,
    /// True if the agent changed the engine suggestion.
    #[serde(default)]
    pub edited: bool,
    /// The claims that the engine added after the agent's edit (design 6.3, rule 5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_claims: Vec<String>,
}

/// All the claims for a range of commits.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangeSet {
    /// The git tree of the parent (the base of step 0).
    pub base_tree: String,
    /// The git tree of the commit (the result of the last step).
    pub head_tree: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub subgroups: Vec<Subgroup>,
    /// The pair warnings that the agent refused, with a reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pair_refusals: Vec<PairRefusal>,
    /// Design 62: the change set of a tag after the commit. Semilla made its one step from the
    /// tree of the first parent and the tree of the commit. `None`: the steps of the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_commit: Option<AfterCommit>,
}

/// The data of a tag after the commit (design 62).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AfterCommit {
    /// True for a merge commit: the step compares the commit with its first parent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merge: bool,
    /// Each configuration of the compiled check that does not build, with its side and its
    /// first error (rule R62.5-3). Empty with no compiled check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_configs: Vec<FailedConfig>,
    /// The error that stopped the compiled check (for example a lock file that does not agree
    /// with the manifests). The step then has the facts of the syntax tree only, and the error
    /// is an open item (rule R62.5-3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiled_error: Option<String>,
    /// R68.10-1: the error of a tool that stopped the tag of the commit in a range (`semilla
    /// tag A..B`). The note has no step; the range continues with the next commit, and the
    /// error is an open item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_error: Option<String>,
}

/// A configuration of the compiled check that does not build in a tag after the commit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedConfig {
    /// The name of the configuration, for example `dev, all features`.
    pub name: String,
    /// The cause: the side (before, after, or both) and the first error.
    pub cause: String,
}

/// The agent's refusal of a pair warning: the removed item and the added item are really two
/// different items.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairRefusal {
    pub old: String,
    pub new: String,
    pub reason: String,
}

impl ChangeSet {
    /// Finds a claim and its step by claim ID.
    pub fn claim(&self, id: &str) -> Option<(&Step, &Claim)> {
        self.steps.iter().find_map(|step| step.claims.iter().find(|claim| claim.id == id).map(|claim| (step, claim)))
    }

    /// Returns the next free claim ID.
    pub fn next_claim_id(&self) -> String {
        let count: usize = self.steps.iter().map(|step| step.claims.len()).sum();
        let mut number = count + 1;
        while self.claim(&format!("c{number}")).is_some() {
            number += 1;
        }
        format!("c{number}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_json_shape() {
        let json = r#"{"id":"c1","op":"add-if","at":"a::f/S0","args":{"cond":"x > 0"}}"#;
        let claim: Claim = serde_json::from_str(json).unwrap();
        assert_eq!(claim.arg_str("cond"), Some("x > 0"));
        assert_eq!(claim.at.unwrap().to_string(), "a::f/S0");
    }

    // rule+ R83.1-2
    #[test]
    fn a_stored_step_of_the_mode_of_design_50_stays_readable() {
        // A step of the default mode of design 50: `"compiled": false` and a build check that
        // did not run. It reads as a step of the one mode, and it keeps its record.
        let json = r#"{"index":0,"intent":"a","claims":[],"tree_before":"x","tree_after":"y","compiled":false,"build_check":{"result":"not_run"}}"#;
        let old: Step = serde_json::from_str(json).unwrap();
        assert!(!old.syntax_only);
        assert_eq!(old.build_check, Some(BuildCheck::NotRun));
        assert_eq!(build_check_lines(std::slice::from_ref(&old)), ["! build check of step 0: not run: the author turned the check off, so Semilla did not check that this code builds"]);
        let set: ChangeSet = serde_json::from_str(&format!(r#"{{"base_tree":"x","head_tree":"y","steps":[{json}]}}"#)).unwrap();
        assert_eq!(set.steps.len(), 1);
    }

    // rule- R83.1-2
    #[test]
    fn a_new_step_has_no_field_of_a_mode() {
        let mut step: Step = serde_json::from_str(r#"{"index":0,"intent":"a","claims":[],"tree_before":"x","tree_after":"y"}"#).unwrap();
        // The syntax-only check of R83.1-3 is not stored.
        step.syntax_only = true;
        let stored = serde_json::to_string(&step).unwrap();
        assert!(!stored.contains("compiled") && !stored.contains("syntax_only"), "{stored}");
    }

    // rule+ R53.4-8 R53.6-1 R53.6-3 R53.6-4
    // rule- R53.6-1 R53.6-3 R53.6-4
    #[test]
    fn the_build_check_record_and_its_lines() {
        // Design 53: a stored step of an earlier version has no result, and no line.
        let json = r#"{"index":0,"intent":"a","claims":[],"tree_before":"x","tree_after":"y"}"#;
        let old: Step = serde_json::from_str(json).unwrap();
        assert_eq!(old.build_check, None);
        assert_eq!(serde_json::to_string(&old).unwrap(), json);
        assert!(build_check_lines(std::slice::from_ref(&old)).is_empty());
        let step = |index: usize, check: BuildCheck| Step { index, build_check: Some(check), ..old.clone() };
        let stored = serde_json::to_string(&step(0, BuildCheck::NotRun)).unwrap();
        assert!(stored.ends_with(r#""build_check":{"result":"not_run"}}"#), "{stored}");
        assert_eq!(serde_json::from_str::<Step>(&stored).unwrap().build_check, Some(BuildCheck::NotRun));
        assert_eq!(build_check_lines(&[step(0, BuildCheck::Passes), step(1, BuildCheck::Passes)]), ["build check of each step: passes"]);
        let base = BuildCheck::BaseFails { targets: vec!["`demo` (bench \"speed\")".to_string()], not_checked: Vec::new() };
        assert_eq!(
            build_check_lines(&[step(0, BuildCheck::Passes), step(1, BuildCheck::NotRun), step(2, base), step(3, BuildCheck::NotRun)]),
            [
                "build check of step 0: passes",
                "! build check of steps 1, 3: not run: the author turned the check off, so Semilla did not check that this code builds",
                "! build check of step 2: no new failure found: the target `demo` (bench \"speed\") does not build before the step and after it, so Semilla did not check a new error of the step in this target, or in a target that needs it",
            ]
        );
        // Only a tree in which each target builds has the word `passes`.
        let api = vec!["`shop` (test \"api\")".to_string()];
        let unknown = BuildCheck::BaseFails { targets: Vec::new(), not_checked: api.clone() };
        assert!(!unknown.text().contains("passes") && unknown.is_gap());
        let stored = serde_json::to_string(&unknown).unwrap();
        assert_eq!(stored, r#"{"result":"base_fails","targets":[],"not_checked":["`shop` (test \"api\")"]}"#);
        assert_eq!(serde_json::from_str::<BuildCheck>(&stored).unwrap(), unknown);
        // A record of batch 9a4 has no second list.
        assert_eq!(serde_json::from_str::<BuildCheck>(r#"{"result":"base_fails","targets":["a"]}"#).unwrap(), BuildCheck::BaseFails { targets: vec!["a".to_string()], not_checked: Vec::new() });
        assert_eq!(
            build_check_lines(&[step(0, unknown.clone())]),
            ["! build check of step 0: no new failure found: the target `shop` (test \"api\") does not build after the step, and cargo did not check it before the step (a different target did not build then), so Semilla cannot tell if the step makes this error"]
        );
        // The command that records the step names the targets that did not build before it.
        assert_eq!(
            unknown.limits(&["`shop` (lib)".to_string(), "`shop` (lib test)".to_string()]),
            ["the target `shop` (test \"api\") does not build after the step, and cargo did not check it before the step (the targets `shop` (lib), `shop` (lib test) did not build then), so Semilla cannot tell if the step makes this error"]
        );
        assert!(BuildCheck::Passes.limits(&[]).is_empty() && !BuildCheck::Passes.is_gap());
    }

    #[test]
    fn next_claim_id_skips_used_ids() {
        let step = Step {
            index: 0,
            intent: String::new(),
            claims: vec![Claim { id: "c2".into(), op: "rename".into(), at: None, args: Default::default() }],
            tree_before: String::new(),
            tree_after: String::new(),
            reasons: Default::default(),
            syntax_only: false,
            build_check: None,
            agent: Default::default(),
            tags: Vec::new(),
        };
        let set = ChangeSet { steps: vec![step], ..Default::default() };
        assert_eq!(set.next_claim_id(), "c3");
    }
}
