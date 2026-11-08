//! The UI data with derived lookups. The router builds it one time.

use std::collections::{BTreeMap, HashMap, HashSet};

use semilla_core::view::{FlowGraph, FlowKind, GraphEdgeKind, GraphNode, TreeNode};
use semilla_core::{Claim, ClaimResult, Class, Group, Role, Step, Subgroup, ops};

use crate::{SourcePair, UiData, review};

/// The ID of a group for steps that no group contains.
pub const UNGROUPED: &str = "ungrouped";

/// A subgroup with its ID. The ID is `Subgroup.id` when it is not empty. Else it is
/// `<group>:<n>`, where `n` is the position of the subgroup in the group, in the order of the
/// change set.
#[derive(Clone, Debug)]
pub struct SubgroupRef {
    pub id: String,
    pub subgroup: Subgroup,
    /// True if the UI made this subgroup for claims without a subgroup.
    pub synthetic: bool,
}

/// The UI data and the lookups that the API uses.
pub struct Ctx {
    pub data: UiData,
    /// The groups, with a synthetic group for steps without a group.
    pub groups: Vec<Group>,
    pub synthetic_groups: Vec<String>,
    pub subgroups: Vec<SubgroupRef>,
    pub results: HashMap<String, ClaimResult>,
    pub claim_step: HashMap<String, usize>,
    pub step_group: HashMap<usize, String>,
    pub claim_subgroup: HashMap<String, String>,
    /// The review hash of each claim (design section 11.8).
    pub claim_hashes: HashMap<String, String>,
    pub graph_index: HashMap<String, usize>,
    /// Call edges: caller -> callees.
    pub callees: BTreeMap<String, Vec<String>>,
    /// Call edges: callee -> callers.
    pub callers: BTreeMap<String, Vec<String>>,
    /// Tree node ID -> the child indexes from the root.
    pub tree_index: HashMap<String, Vec<usize>>,
}

impl Ctx {
    pub fn new(data: UiData) -> Ctx {
        let mut results = HashMap::new();
        for step in &data.report.steps {
            for result in &step.claims {
                results.insert(result.claim.clone(), result.clone());
            }
        }
        let mut claim_step = HashMap::new();
        for step in &data.changeset.steps {
            for claim in &step.claims {
                claim_step.insert(claim.id.clone(), step.index);
            }
        }

        // Groups. Steps without a group go into a synthetic group.
        let mut groups = data.changeset.groups.clone();
        let mut step_group = HashMap::new();
        for group in &groups {
            for step in &group.steps {
                step_group.entry(*step).or_insert_with(|| group.id.clone());
            }
        }
        let loose: Vec<usize> =
            data.changeset.steps.iter().map(|step| step.index).filter(|index| !step_group.contains_key(index)).collect();
        let mut synthetic_groups = Vec::new();
        if !loose.is_empty() {
            for index in &loose {
                step_group.insert(*index, UNGROUPED.to_string());
            }
            groups.push(Group {
                id: UNGROUPED.to_string(),
                title: "Steps without a group".to_string(),
                kind: semilla_core::GroupKind::Behavior,
                why: "The agent did not put these steps in a group.".to_string(),
                steps: loose,
            });
            synthetic_groups.push(UNGROUPED.to_string());
        }

        // Subgroups. Each claim goes into its first subgroup. Claims without a subgroup go
        // into a synthetic subgroup of their group.
        let mut subgroups = Vec::new();
        let mut claim_subgroup = HashMap::new();
        let mut counters: HashMap<String, usize> = HashMap::new();
        let mut used: HashSet<String> = HashSet::new();
        for subgroup in &data.changeset.subgroups {
            let counter = counters.entry(subgroup.group.clone()).or_insert(0);
            let fallback = format!("{}:{}", subgroup.group, counter);
            *counter += 1;
            // The ID of the data, if it has one. Old data has no ID: then `<group>:<n>`.
            // A second subgroup with the same ID also gets the fallback.
            let mut id = if subgroup.id.is_empty() || used.contains(&subgroup.id) { fallback } else { subgroup.id.clone() };
            while used.contains(&id) {
                id.push('~');
            }
            used.insert(id.clone());
            for claim in &subgroup.claims {
                claim_subgroup.entry(claim.clone()).or_insert_with(|| id.clone());
            }
            subgroups.push(SubgroupRef { id, subgroup: subgroup.clone(), synthetic: false });
        }
        for group in &groups {
            let loose: Vec<String> = group
                .steps
                .iter()
                .filter_map(|index| data.changeset.steps.iter().find(|step| step.index == *index))
                .flat_map(|step| step.claims.iter())
                .filter(|claim| !claim_subgroup.contains_key(&claim.id))
                .map(|claim| claim.id.clone())
                .collect();
            if loose.is_empty() {
                continue;
            }
            let mut id = format!("{}:loose", group.id);
            while used.contains(&id) {
                id.push('~');
            }
            used.insert(id.clone());
            for claim in &loose {
                claim_subgroup.insert(claim.clone(), id.clone());
            }
            subgroups.push(SubgroupRef {
                id: id.clone(),
                subgroup: Subgroup {
                    id,
                    group: group.id.clone(),
                    role: Role::Custom,
                    title: "Claims without a subgroup".to_string(),
                    why: String::new(),
                    claims: loose,
                    edited: false,
                    new_claims: Vec::new(),
                },
                synthetic: true,
            });
        }

        let graph_index = data.graph.nodes.iter().enumerate().map(|(index, node)| (node.id.clone(), index)).collect();
        let mut callees: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut callers: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for edge in data.graph.edges.iter().filter(|edge| edge.kind == GraphEdgeKind::Call) {
            let out = callees.entry(edge.from.clone()).or_default();
            if !out.contains(&edge.to) {
                out.push(edge.to.clone());
            }
            let into = callers.entry(edge.to.clone()).or_default();
            if !into.contains(&edge.from) {
                into.push(edge.from.clone());
            }
        }

        let mut tree_index = HashMap::new();
        index_tree(&data.tree, &mut Vec::new(), &mut tree_index);

        let mut ctx = Ctx {
            data,
            groups,
            synthetic_groups,
            subgroups,
            results,
            claim_step,
            step_group,
            claim_subgroup,
            claim_hashes: HashMap::new(),
            graph_index,
            callees,
            callers,
            tree_index,
        };
        let hashes = ctx.all_claims().map(|(_, claim)| (claim.id.clone(), review::claim_hash(&ctx, claim))).collect();
        ctx.claim_hashes = hashes;
        ctx
    }

    /// All claims with their steps, in step order.
    pub fn all_claims(&self) -> impl Iterator<Item = (&Step, &Claim)> {
        self.data.changeset.steps.iter().flat_map(|step| step.claims.iter().map(move |claim| (step, claim)))
    }

    pub fn claim(&self, id: &str) -> Option<&Claim> {
        self.data.changeset.claim(id).map(|(_, claim)| claim)
    }

    pub fn group(&self, id: &str) -> Option<&Group> {
        self.groups.iter().find(|group| group.id == id)
    }

    pub fn subgroup(&self, id: &str) -> Option<&SubgroupRef> {
        self.subgroups.iter().find(|subgroup| subgroup.id == id)
    }

    /// The claim IDs of a group, in step order.
    pub fn group_claims(&self, group: &str) -> Vec<String> {
        self.all_claims()
            .filter(|(step, _)| self.step_group.get(&step.index).map(String::as_str) == Some(group))
            .map(|(_, claim)| claim.id.clone())
            .collect()
    }

    /// The group of a claim.
    pub fn claim_group(&self, claim: &str) -> Option<&str> {
        self.claim_step.get(claim).and_then(|step| self.step_group.get(step)).map(String::as_str)
    }

    /// The computed class of a claim. The verifier result has priority. A verified claim without
    /// a computed class gets the class of the rule table. Other claims have no class.
    pub fn claim_class(&self, claim: &Claim) -> Option<Class> {
        let result = self.results.get(&claim.id)?;
        result.class.or_else(|| if result.status.is_verified() { ops::op(&claim.op).and_then(|info| info.class) } else { None })
    }

    /// True if the claim is verified and its computed class is neutral.
    pub fn is_proved_neutral(&self, claim: &Claim) -> bool {
        self.results.get(&claim.id).is_some_and(|result| result.status.is_verified()) && self.claim_class(claim) == Some(Class::Neutral)
    }

    pub fn graph_node(&self, id: &str) -> Option<&GraphNode> {
        self.graph_index.get(id).map(|index| &self.data.graph.nodes[*index])
    }

    pub fn tree_node(&self, id: &str) -> Option<&TreeNode> {
        let path = self.tree_index.get(id)?;
        let mut node = &self.data.tree;
        for index in path {
            node = &node.children[*index];
        }
        Some(node)
    }

    /// The parent of a tree node.
    pub fn tree_parent(&self, id: &str) -> Option<&TreeNode> {
        let path = self.tree_index.get(id)?;
        let (_, parent) = path.split_last()?;
        let mut node = &self.data.tree;
        for index in parent {
            node = &node.children[*index];
        }
        Some(node)
    }

    /// The source pair of a node: the exact anchor text, then the item of the anchor.
    pub fn source(&self, id: &str) -> Option<(&str, &SourcePair)> {
        if let Some((key, pair)) = self.data.sources.get_key_value(id) {
            return Some((key.as_str(), pair));
        }
        let item = item_of(id);
        self.data.sources.get_key_value(item).map(|(key, pair)| (key.as_str(), pair))
    }

    /// The flow view that shows a node: the frame of the function, then a frame that contains
    /// the anchor, then a frame with a node at the anchor.
    pub fn flow_for(&self, id: &str) -> Option<&FlowGraph> {
        let flows = &self.data.flows;
        flows
            .iter()
            .find(|flow| flow.function == id)
            .or_else(|| flows.iter().filter(|flow| id.starts_with(&format!("{}/", flow.function))).max_by_key(|flow| flow.function.len()))
            .or_else(|| {
                // A stub points to code outside its frame, so it does not count.
                flows.iter().find(|flow| flow.nodes.iter().any(|node| node.kind != FlowKind::Stub && node.anchor.as_deref() == Some(id)))
            })
    }

    /// The function that has a flow view for this node, if one exists.
    pub fn flow_function(&self, id: &str) -> Option<String> {
        self.flow_for(id).map(|flow| flow.function.clone())
    }

    /// The file of an item: the file of its graph node, or of the nearest ancestor with a file.
    /// An item that is not in the graph (for example the old name of a rename) uses the nearest
    /// prefix of its path that is in the graph.
    pub fn item_file(&self, item: &str) -> Option<String> {
        let mut path = item_of(item);
        loop {
            if self.graph_node(path).is_some() {
                return self.graph_ancestors(path).iter().rev().find_map(|id| self.graph_node(id).and_then(|node| node.file.clone()));
            }
            path = path.rsplit_once("::")?.0;
        }
    }

    /// The breadcrumb of graph node IDs, from the crate to the node.
    pub fn graph_ancestors(&self, id: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut current = self.graph_node(id);
        while let Some(node) = current {
            if chain.contains(&node.id) {
                break;
            }
            chain.push(node.id.clone());
            current = node.parent.as_deref().and_then(|parent| self.graph_node(parent));
        }
        chain.reverse();
        chain
    }
}

fn index_tree(node: &TreeNode, path: &mut Vec<usize>, out: &mut HashMap<String, Vec<usize>>) {
    out.entry(node.id.clone()).or_insert_with(|| path.clone());
    for (index, child) in node.children.iter().enumerate() {
        path.push(index);
        index_tree(child, path, out);
        path.pop();
    }
}

/// The item part of an anchor text: the text before the first `/`.
pub fn item_of(id: &str) -> &str {
    id.split('/').next().unwrap_or(id)
}

/// The last segment of an item path, for example `total` for `cart::Cart::total`.
pub fn last_segment(id: &str) -> &str {
    item_of(id).rsplit("::").next().unwrap_or(id)
}

/// True if the node `id` contains the anchor text `anchor`: the same node, a node inside the
/// item, or an item inside the module or crate.
pub fn contains(id: &str, anchor: &str) -> bool {
    anchor == id || anchor.strip_prefix(id).is_some_and(|rest| rest.starts_with('/') || (!id.contains('/') && rest.starts_with("::")))
}

/// The display label of a role.
pub fn role_label(role: Role) -> &'static str {
    match role {
        Role::Interface => "Interface",
        Role::Core => "Core logic",
        Role::Tests => "Tests",
        Role::Config => "Config",
        Role::Propagation => "Propagation",
        Role::Mechanical => "Mechanical",
        Role::Custom => "Custom",
    }
}

/// The short label of a class, as in the badges of design section 6.3.
pub fn class_label(class: Option<Class>) -> &'static str {
    match class {
        Some(Class::Neutral) => "neutral",
        Some(Class::Logic) => "logic",
        Some(Class::ControlFlow) => "ctrl flow",
        Some(Class::Other) => "other",
        None => "unknown",
    }
}
