//! Claims, steps, groups, subgroups, and the change set.

use serde::{Deserialize, Serialize};

use crate::anchor::Anchor;
use crate::ops::Class;

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
    /// The class that the agent declares. `None` lets the verifier report the computed class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    pub claims: Vec<Claim>,
    /// The git tree before the step.
    pub tree_before: String,
    /// The git tree after the step.
    pub tree_after: String,
    /// The reasons of the agent for the spotlight facts, by the anchor of the fact (`semilla
    /// reason`). Free text for the reviewer.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub reasons: std::collections::BTreeMap<String, String>,
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

    #[test]
    fn next_claim_id_skips_used_ids() {
        let step = Step {
            index: 0,
            intent: String::new(),
            class: None,
            claims: vec![Claim { id: "c2".into(), op: "rename".into(), at: None, args: Default::default() }],
            tree_before: String::new(),
            tree_after: String::new(),
            reasons: Default::default(),
        };
        let set = ChangeSet { steps: vec![step], ..Default::default() };
        assert_eq!(set.next_claim_id(), "c3");
    }
}
