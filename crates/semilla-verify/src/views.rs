//! The views that the UI and the CLI show: the change tree, the flow views, the workspace graph,
//! and the source texts.

use std::collections::BTreeMap;

use semilla_core::view::{FlowGraph, Status, TreeNode, WorkspaceGraph};
use semilla_core::{Class, ClaimStatus, StepReport};
use semilla_syntax::{EditKind, Workspace};

use crate::StepOutcome;

fn edit_status(kind: EditKind, covered: bool) -> Status {
    if !covered {
        return Status::Uncovered;
    }
    match kind {
        EditKind::ItemAdded | EditKind::Inserted | EditKind::FileAdded => Status::Added,
        EditKind::ItemRemoved | EditKind::Deleted | EditKind::FileRemoved => Status::Removed,
        EditKind::Moved => Status::Moved,
        _ => Status::Changed,
    }
}

/// Builds the change tree of the steps (design section 11.4).
pub fn change_tree(outcomes: &[StepOutcome], after: &Workspace, before: &Workspace) -> TreeNode {
    let mut root = TreeNode { id: "workspace".into(), label: "workspace".into(), status: Status::Changed, class: None, claims: Vec::new(), diff: None, summary: None, children: Vec::new() };
    for outcome in outcomes {
        let classes: BTreeMap<String, Option<Class>> = outcome.report.claims.iter().map(|c| (c.claim.clone(), c.class)).collect();
        for (index, edit) in outcome.edits.iter().enumerate() {
            let owner = outcome.owners[index].map(|claim| outcome_claim_id(outcome, claim));
            let status = edit_status(edit.kind, owner.is_some());
            let class = owner.as_ref().and_then(|id| classes.get(id).copied().flatten());
            let diff = match (edit.kind, &edit.text_before, &edit.text_after) {
                // An operator change marks only the operator: `a ⟦<⟧→⟦<=⟧ b`.
                (EditKind::Modified, Some(_), Some(new)) if edit.attr_before != edit.attr_after && edit.attr_after.as_ref().is_some_and(|a| new.contains(&format!(" {a} "))) => {
                    let (old_attr, new_attr) = (edit.attr_before.clone().unwrap_or_default(), edit.attr_after.clone().unwrap_or_default());
                    Some(short(&new.replacen(&format!(" {new_attr} "), &format!(" ⟦{old_attr}⟧→⟦{new_attr}⟧ "), 1)))
                }
                (EditKind::Modified | EditKind::Replaced, Some(old), Some(new)) => Some(format!("⟦{}⟧→⟦{}⟧", short(old), short(new))),
                _ => None,
            };
            let Some(anchor) = edit.anchor() else {
                let file = edit.file.clone().unwrap_or_default();
                insert(&mut root, &[("files".into(), "files".into()), (file.clone(), file)], status, class, owner, diff);
                continue;
            };
            let mut path: Vec<(String, String)> = Vec::new();
            let parts: Vec<&str> = anchor.item.split("::").collect();
            for depth in 1..=parts.len() {
                let id = parts[..depth].join("::");
                let label = if depth == 1 { format!("crate {}", parts[0]) } else { item_label(&id, after, before) };
                path.push((id, label));
            }
            let mut node_path = anchor.item.clone();
            for segment in &anchor.path {
                node_path = format!("{node_path}/{segment}");
                let ws = if edit.after.is_some() { after } else { before };
                let label = node_path
                    .parse()
                    .ok()
                    .and_then(|a| semilla_syntax::resolve_node(ws, &a))
                    .map(|n| format!("{}: {}", segment, short(&n.text)))
                    .unwrap_or_else(|| segment.to_string());
                path.push((node_path.clone(), label));
            }
            if let Some(old) = outcome.renames.items.iter().find(|(_, new)| *new == &anchor.item).map(|(old, _)| old.clone()) {
                if let Some(last) = path.iter_mut().find(|(id, _)| id == &anchor.item) {
                    last.1 = format!("{} → {}", old.rsplit("::").next().unwrap_or(&old), last.1);
                }
            }
            insert(&mut root, &path, status, class, owner, diff);
        }
    }
    summarize(&mut root);
    root
}

fn outcome_claim_id(outcome: &StepOutcome, index: usize) -> String {
    outcome.report.claims.get(index).map(|c| c.claim.clone()).unwrap_or_default()
}

fn item_label(path: &str, after: &Workspace, before: &Workspace) -> String {
    let name = path.rsplit("::").next().unwrap_or(path);
    match after.items.get(path).or_else(|| before.items.get(path)) {
        Some(item) => format!("{} {name}", semilla_syntax::diff::kind_name(item.kind)),
        None => name.to_string(),
    }
}

fn short(text: &str) -> String {
    semilla_syntax::tree::short(text, 50)
}

fn insert(root: &mut TreeNode, path: &[(String, String)], status: Status, class: Option<Class>, claim: Option<String>, diff: Option<String>) {
    let mut node = root;
    for (depth, (id, label)) in path.iter().enumerate() {
        let position = match node.children.iter().position(|child| &child.id == id) {
            Some(position) => position,
            None => {
                node.children.push(TreeNode { id: id.clone(), label: label.clone(), status: Status::Changed, class: None, claims: Vec::new(), diff: None, summary: None, children: Vec::new() });
                node.children.len() - 1
            }
        };
        node = &mut node.children[position];
        if depth + 1 == path.len() {
            // The worst status wins: uncovered, then added/removed/moved, then changed.
            if node.status == Status::Changed || status == Status::Uncovered {
                node.status = status;
            }
            if node.class.is_none_or(|old| class.is_some_and(|new| crate::class_rank(new) > crate::class_rank(old))) {
                node.class = class.or(node.class);
            }
            if let Some(claim) = &claim {
                if !node.claims.contains(claim) {
                    node.claims.push(claim.clone());
                }
            }
            if diff.is_some() {
                node.diff = diff.clone();
            }
        }
    }
}

/// Adds the class counts of the descendants to each node with children.
fn summarize(node: &mut TreeNode) -> BTreeMap<&'static str, usize> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for child in &mut node.children {
        for (key, value) in summarize(child) {
            *counts.entry(key).or_default() += value;
        }
    }
    if node.children.is_empty() {
        let key = match (node.status, node.class) {
            (Status::Uncovered, _) => "uncovered",
            (_, Some(Class::ControlFlow)) => "ctrl flow",
            (_, Some(Class::Logic)) => "logic",
            (_, Some(Class::Neutral)) => "neutral",
            _ => "other",
        };
        *counts.entry(key).or_default() += 1;
    } else {
        let text: Vec<String> = counts.iter().map(|(key, value)| format!("{value} {key}")).collect();
        node.summary = Some(text.join(" · "));
        if node.children.iter().any(|child| child.status == Status::Uncovered) {
            node.status = Status::Uncovered;
        }
    }
    counts
}

/// Builds a flow view for each changed function.
pub fn flows(outcomes: &[StepOutcome], after: &Workspace) -> Vec<FlowGraph> {
    let mut per_item: BTreeMap<String, (BTreeMap<String, Status>, usize)> = BTreeMap::new();
    for outcome in outcomes {
        for (index, edit) in outcome.edits.iter().enumerate() {
            let covered = outcome.owners[index].is_some();
            if edit.kind == EditKind::Deleted {
                if let Some(anchor) = &edit.before {
                    per_item.entry(anchor.item.clone()).or_default().1 += 1;
                }
                continue;
            }
            let Some(anchor) = &edit.after else { continue };
            if anchor.path.is_empty() {
                continue;
            }
            let path: Vec<String> = anchor.path.iter().map(|s| s.to_string()).collect();
            per_item.entry(anchor.item.clone()).or_default().0.insert(path.join("/"), edit_status(edit.kind, covered));
        }
    }
    per_item
        .into_iter()
        .filter_map(|(item, (statuses, removed))| after.items.get(&item).and_then(|item| semilla_syntax::flow::flow_view(item, &statuses, removed)))
        .collect()
}

/// Builds the workspace graph of the "after" side, with the statuses of the change set.
pub fn graph(outcomes: &[StepOutcome], after: &Workspace) -> WorkspaceGraph {
    let mut statuses: BTreeMap<String, Status> = BTreeMap::new();
    let mut changes: BTreeMap<String, usize> = BTreeMap::new();
    for outcome in outcomes {
        for (index, edit) in outcome.edits.iter().enumerate() {
            let Some(item) = edit.item() else { continue };
            *changes.entry(item.to_string()).or_default() += 1;
            let status = edit_status(edit.kind, outcome.owners[index].is_some());
            let status = if edit.anchor().is_some_and(|a| !a.path.is_empty()) && status != Status::Uncovered { Status::Changed } else { status };
            let entry = statuses.entry(item.to_string()).or_insert(status);
            if status == Status::Uncovered {
                *entry = status;
            }
        }
    }
    semilla_syntax::graph::workspace_graph(after, &statuses, &changes)
}

/// The source texts of the changed anchors, before and after.
pub fn sources(outcomes: &[StepOutcome], before: &Workspace, after: &Workspace) -> BTreeMap<String, (Option<String>, Option<String>)> {
    let mut map = BTreeMap::new();
    for outcome in outcomes {
        for edit in &outcome.edits {
            let key = edit.anchor().map(|a| a.to_string()).or_else(|| edit.file.clone()).unwrap_or_default();
            let entry = map.entry(key).or_insert((None, None));
            if entry.0.is_none() {
                entry.0 = edit.text_before.clone();
            }
            entry.1 = edit.text_after.clone();
        }
    }
    for item in after.items.values().filter(|i| i.kind == semilla_syntax::ItemKind::Fn) {
        let entry = map.entry(item.path.clone()).or_insert((None, None));
        entry.1 = Some(item.source.clone());
        if entry.0.is_none() {
            entry.0 = before.items.get(&item.path).map(|i| i.source.clone());
        }
    }
    map
}

/// A text report of the steps, for the CLI and the agent tools (design section 8.15).
pub fn text_report(steps: &[StepReport]) -> String {
    let mut out = String::new();
    for step in steps {
        out.push_str(&format!("step {}  \"{}\"\n", step.step, step.intent));
        for claim in &step.claims {
            let (mark, detail) = match &claim.status {
                ClaimStatus::Verified { proof } => ("✓", format!("verified ({proof})")),
                ClaimStatus::Unproved { reason } => ("?", format!("unproved: {reason}")),
                ClaimStatus::Refuted { reason } => ("✗", format!("refuted: {reason}")),
                ClaimStatus::Empty => ("✗", "empty: the claim explains no change".to_string()),
                ClaimStatus::Malformed { reason } => ("✗", format!("malformed: {reason}")),
            };
            let class = claim.class.map(|c| format!(" [{}]", crate::class_name(c))).unwrap_or_default();
            let effect = claim.effect.map(|e| format!(" effect: {e:?}").to_lowercase()).unwrap_or_default();
            let witness = claim.witness.as_ref().map(|w| format!(" (example: {w})")).unwrap_or_default();
            out.push_str(&format!("  {mark} {} {detail}{class}{effect}{witness}\n", claim.claim));
            if let Some(hint) = &claim.hint {
                out.push_str(&format!("      hint: {hint}\n"));
            }
        }
        for uncovered in &step.uncovered {
            let at = uncovered.at.as_ref().map(|a| a.to_string()).or_else(|| uncovered.file.clone()).unwrap_or_default();
            out.push_str(&format!("  ✗ uncovered  {at}\n       {:?}: {}\n", uncovered.view, uncovered.description).replace("Syntax", "syntax").replace("Body", "body").replace("Signature", "signature").replace("File", "file"));
            if let Some(hint) = &uncovered.hint {
                out.push_str(&format!("       hint: add {hint}\n"));
            }
        }
        for item in &step.spotlight {
            out.push_str(&format!("  ! spotlight: {item}\n"));
        }
        for error in &step.errors {
            out.push_str(&format!("  ✗ error: {error}\n"));
        }
        out.push('\n');
    }
    out
}
