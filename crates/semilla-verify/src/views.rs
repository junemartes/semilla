//! The views that the UI and the CLI show: the change tree, the flow views, the workspace graph,
//! and the source texts.

use std::collections::BTreeMap;

use semilla_core::view::{FlowGraph, Status, TreeNode, WorkspaceGraph};
use semilla_core::{text, Class, ClaimStatus, StepReport};
use semilla_syntax::{Edit, EditKind, Renames, Workspace};

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
    change_tree_with(outcomes, after, before, &[])
}

/// One edit of the diff from the base tree to the head tree, with the steps that made it.
#[derive(Clone, Debug)]
pub struct FinalEdit {
    pub edit: Edit,
    /// The numbers of the steps that made this edit. Empty for a change set of one step.
    pub steps: Vec<usize>,
    /// The claims and the facts that own the edit.
    pub owners: Vec<String>,
    pub class: Option<Class>,
    /// False if a step edit of this edit has no claim and no fact.
    pub covered: bool,
}

/// The item renames of each step. They map an item path of one step to the base tree or to the
/// head tree.
struct Names<'a>(Vec<&'a BTreeMap<String, String>>);

impl Names<'_> {
    /// The head path of an item path that is valid before the step at the position `from`.
    fn forward(&self, path: &str, from: usize) -> String {
        self.0.iter().skip(from).fold(path.to_string(), |path, map| map.get(&path).cloned().unwrap_or(path))
    }

    /// The base path of an item path that is valid before the step at the position `before`.
    /// `None` for a new item with a name that an earlier rename made free.
    fn backward(&self, path: &str, before: usize) -> Option<String> {
        let mut path = path.to_string();
        for map in self.0.iter().take(before).rev() {
            if let Some((old, _)) = map.iter().find(|(_, new)| **new == path) {
                path = old.clone();
            } else if map.contains_key(&path) {
                return None;
            }
        }
        Some(path)
    }
}

/// The renames of all steps as one set from the base tree to the head tree. The map gives the
/// step numbers of each renamed base item.
pub fn composed_renames(outcomes: &[StepOutcome]) -> (Renames, BTreeMap<String, Vec<usize>>) {
    let names = Names(outcomes.iter().map(|outcome| &outcome.renames.items).collect());
    let mut renames = Renames::default();
    let mut steps: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (position, outcome) in outcomes.iter().enumerate() {
        for (old, new) in &outcome.renames.items {
            let Some(base) = names.backward(old, position) else { continue };
            steps.entry(base.clone()).or_default().push(outcome.report.step);
            renames.items.insert(base, new.clone());
        }
        for (scope, old, new) in &outcome.renames.idents {
            let scope = match scope {
                Some(item) => match names.backward(item, position) {
                    Some(base) => Some(base),
                    None => continue,
                },
                None => None,
            };
            match renames.idents.iter_mut().find(|(other, _, current)| *other == scope && current == old) {
                Some(entry) => entry.2 = new.clone(),
                None => renames.idents.push((scope, old.clone(), new.clone())),
            }
        }
        for (old, new) in &outcome.renames.fields {
            match renames.fields.iter_mut().find(|(_, current)| current == old) {
                Some(entry) => entry.1 = new.clone(),
                None => renames.fields.push((old.clone(), new.clone())),
            }
        }
    }
    renames.items.retain(|old, new| old != new);
    renames.idents.retain(|(_, old, new)| old != new);
    renames.fields.retain(|(old, new)| old != new);
    (renames, steps)
}

/// One edit of one step, with its owner and its item path in the base tree and in the head tree.
struct StepEdit<'a> {
    edit: &'a Edit,
    /// The position of the outcome, and the step number.
    position: usize,
    step: usize,
    owner: Option<String>,
    class: Option<Class>,
    head: Option<String>,
    base: Option<String>,
}

fn step_edits(outcomes: &[StepOutcome]) -> Vec<StepEdit<'_>> {
    let names = Names(outcomes.iter().map(|outcome| &outcome.renames.items).collect());
    let mut all = Vec::new();
    for (position, outcome) in outcomes.iter().enumerate() {
        let mut classes: BTreeMap<String, Option<Class>> = outcome.report.claims.iter().map(|c| (c.claim.clone(), c.class)).collect();
        classes.extend(outcome.report.facts.iter().map(|f| (f.id.clone(), f.class)));
        for (index, edit) in outcome.edits.iter().enumerate() {
            // A fact owns its edits like a claim: the tree shows the fact ID (`f1`).
            let owner = outcome.owners[index].map(|claim| outcome_claim_id(outcome, claim)).or_else(|| fact_id(outcome, index));
            let class = owner.as_ref().and_then(|id| classes.get(id).copied().flatten());
            let head = edit.after.as_ref().map(|anchor| names.forward(&anchor.item, position + 1));
            let base = edit.before.as_ref().and_then(|anchor| names.backward(&anchor.item, position));
            all.push(StepEdit { edit, position, step: outcome.report.step, owner, class, head, base });
        }
    }
    all
}

/// The positions of the step edits that made a final edit: the edits of the same item with the
/// same new text or the same old text. One text can contain the other text, because a step edit
/// can be a part of the final edit. If no text agrees, the edits of the item on the same level
/// (item or body), then all edits of the item.
fn makers(edit: &Edit, steps: &[StepEdit]) -> Vec<usize> {
    let same_item = |step: &StepEdit| match edit.anchor() {
        Some(_) => {
            edit.after.as_ref().is_some_and(|anchor| step.head.as_ref() == Some(&anchor.item)) || edit.before.as_ref().is_some_and(|anchor| step.base.as_ref() == Some(&anchor.item))
        }
        None => step.edit.anchor().is_none() && step.edit.file == edit.file,
    };
    let candidates: Vec<usize> = (0..steps.len()).filter(|&index| same_item(&steps[index])).collect();
    if edit.anchor().is_none() {
        return candidates;
    }
    // A short text (`1`, `rate`) is in many texts, so only a long text can be a part.
    let agrees = |a: &Option<String>, b: &Option<String>| match (a, b) {
        (Some(a), Some(b)) => a == b || (a.len().min(b.len()) >= 12 && (a.contains(b.as_str()) || b.contains(a.as_str()))),
        _ => false,
    };
    let same_text = |step: &StepEdit| agrees(&edit.text_after, &step.edit.text_after) || agrees(&edit.text_before, &step.edit.text_before);
    let exact: Vec<usize> = candidates.iter().copied().filter(|&index| same_text(&steps[index])).collect();
    if !exact.is_empty() {
        return exact;
    }
    let in_body = |edit: &Edit| edit.anchor().is_some_and(|anchor| !anchor.path.is_empty());
    let level: Vec<usize> = candidates.iter().copied().filter(|&index| in_body(steps[index].edit) == in_body(edit)).collect();
    if level.is_empty() { candidates } else { level }
}

/// The edits of the change set as one diff from the base tree to the head tree (brief 5f, D3).
/// A statement path of one step can name a different statement in a different step, so the views
/// do not add the edits of the steps. Each edit gets the numbers of the steps that made it.
pub fn final_edits(outcomes: &[StepOutcome], base: &Workspace, head: &Workspace) -> Vec<FinalEdit> {
    final_edits_with(outcomes, base, head).0
}

/// The final edits, and the uncovered step edits that no final edit shows (position of the
/// outcome, index of the edit).
fn final_edits_with(outcomes: &[StepOutcome], base: &Workspace, head: &Workspace) -> (Vec<FinalEdit>, Vec<(usize, usize)>) {
    let steps = step_edits(outcomes);
    // One step: its edits are the diff, with the exact owners.
    if outcomes.len() <= 1 {
        let finals = steps.iter().map(|step| FinalEdit { edit: step.edit.clone(), steps: Vec::new(), owners: step.owner.clone().into_iter().collect(), class: step.class, covered: step.owner.is_some() }).collect();
        return (finals, Vec::new());
    }
    let (renames, _) = composed_renames(outcomes);
    let all_covered = steps.iter().all(|step| step.owner.is_some());
    let mut shown = vec![false; steps.len()];
    let mut finals = Vec::new();
    for edit in semilla_syntax::diff(base, head, &renames) {
        let makers = makers(&edit, &steps);
        let mut numbers: Vec<usize> = makers.iter().map(|&index| steps[index].step).collect();
        numbers.sort_unstable();
        numbers.dedup();
        let mut owners: Vec<String> = Vec::new();
        let mut class: Option<Class> = None;
        for &index in &makers {
            shown[index] = true;
            if let Some(owner) = &steps[index].owner {
                if !owners.contains(owner) {
                    owners.push(owner.clone());
                }
            }
            if class.is_none_or(|old| steps[index].class.is_some_and(|new| crate::class_rank(new) > crate::class_rank(old))) {
                class = steps[index].class.or(class);
            }
        }
        let covered = if makers.is_empty() { all_covered } else { makers.iter().all(|&index| steps[index].owner.is_some()) };
        finals.push(FinalEdit { edit, steps: numbers, owners, class, covered });
    }
    // An uncovered edit of a step stays in view, also if a later step removed it.
    let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
    let mut hidden = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let edit_index = *counts.entry(step.position).and_modify(|count| *count += 1).or_insert(0);
        if step.owner.is_none() && !shown[index] {
            hidden.push((step.position, edit_index));
        }
    }
    (finals, hidden)
}

fn new_node(id: &str, label: &str) -> TreeNode {
    TreeNode { id: id.into(), label: label.into(), status: Status::Changed, class: None, build_code: None, claims: Vec::new(), diff: None, steps: Vec::new(), summary: None, children: Vec::new() }
}

/// Builds the change tree: one diff from the base tree (`before`) to the head tree (`after`), with
/// the step numbers as labels (brief 5f, D3). `steps` gives the workspaces (before, after) of each
/// step, for the label of an uncovered edit that a later step removed.
pub fn change_tree_with(outcomes: &[StepOutcome], after: &Workspace, before: &Workspace, steps: &[(&Workspace, &Workspace)]) -> TreeNode {
    let (finals, hidden) = final_edits_with(outcomes, before, after);
    let (renames, rename_steps) = composed_renames(outcomes);
    let many = outcomes.len() > 1;
    let mut root = new_node("workspace", "workspace");
    // A removed and an added item with the same path (a changed `use` line) is one changed node
    // (D12).
    let removed_paths: std::collections::BTreeSet<&str> = finals.iter().filter(|f| f.edit.kind == EditKind::ItemRemoved).filter_map(|f| f.edit.item()).collect();
    let replaced: BTreeMap<&str, &FinalEdit> = finals.iter().filter(|f| f.edit.kind == EditKind::ItemAdded).filter_map(|f| Some((f.edit.item()?, f))).filter(|(item, _)| removed_paths.contains(item)).collect();
    let item_path = |item: &str, ws_after: &Workspace, ws_before: &Workspace| {
        let parts = semilla_syntax::split_path(item);
        (1..=parts.len())
            .map(|depth| {
                let id = parts[..depth].join("::");
                let label = if depth == 1 { format!("crate {}", parts[0]) } else { item_label(&id, ws_after, ws_before) };
                (id, label)
            })
            .collect::<Vec<_>>()
    };
    // The path of the nodes of an anchor. The statement labels come from the tree of their side.
    let anchor_path = |anchor: &semilla_core::Anchor, ws: &Workspace, ws_after: &Workspace, ws_before: &Workspace, suffix: &str| {
        let mut path = item_path(&anchor.item, ws_after, ws_before);
        let mut node_path = anchor.item.clone();
        for segment in &anchor.path {
            node_path = format!("{node_path}/{segment}");
            let label = node_path
                .parse()
                .ok()
                .and_then(|a| semilla_syntax::resolve_node(ws, &a))
                .map(|n| format!("{}: {}", segment, short(&n.text)))
                .unwrap_or_else(|| segment.to_string());
            path.push((format!("{node_path}{suffix}"), label));
        }
        path
    };
    for f in &finals {
        let edit = &f.edit;
        if edit.kind == EditKind::ItemAdded && edit.item().is_some_and(|item| replaced.contains_key(item)) {
            continue;
        }
        // The member list of an impl is bookkeeping: the new or removed method has its own node.
        if edit.kind == EditKind::Members && edit.anchor().is_some_and(|a| semilla_syntax::split_path(&a.item).last().is_some_and(|l| l.starts_with("impl "))) {
            continue;
        }
        let pair = edit.item().and_then(|item| replaced.get(item)).filter(|_| edit.kind == EditKind::ItemRemoved);
        // A replaced item has the owners and the steps of the two sides.
        let mut owners = f.owners.clone();
        let mut numbers = f.steps.clone();
        let mut covered = f.covered;
        let mut class = f.class;
        if let Some(added) = pair {
            owners.extend(added.owners.iter().filter(|owner| !f.owners.contains(owner)).cloned());
            numbers.extend(added.steps.iter().copied());
            numbers.sort_unstable();
            numbers.dedup();
            if many {
                covered &= added.covered;
                if class.is_none_or(|old| added.class.is_some_and(|new| crate::class_rank(new) > crate::class_rank(old))) {
                    class = added.class.or(class);
                }
            }
        }
        let status = match (edit.kind, &edit.text_before, &edit.text_after) {
            _ if pair.is_some() && covered => Status::Changed,
            // A new or a removed part of a signature. A renamed parameter gives one of each.
            (EditKind::Signature, None, Some(_)) if covered => Status::Added,
            (EditKind::Signature, Some(_), None) if covered => Status::Removed,
            _ => edit_status(edit.kind, covered),
        };
        // The source text reads better; one line only, else the token text.
        let one_line = |source: &Option<String>, tokens: &Option<String>| match source {
            Some(text) if !text.contains('\n') => Some(text.clone()),
            _ => tokens.clone(),
        };
        let (shown_before, shown_after) = (one_line(&edit.source_before, &edit.text_before), one_line(&edit.source_after, &edit.text_after));
        let shown_after = match pair {
            Some(added) => one_line(&added.edit.source_after, &added.edit.text_after),
            None => shown_after,
        };
        let kind = if pair.is_some() { EditKind::Replaced } else { edit.kind };
        let diff = match (kind, &shown_before, &shown_after) {
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
            insert(&mut root, &[("files".into(), "files".into()), (file.clone(), file)], status, class, &owners, &numbers, diff);
            continue;
        };
        let mut path = anchor_path(anchor, if edit.after.is_some() { after } else { before }, after, before, "");
        if let Some(old) = renames.items.iter().find(|(_, new)| *new == &anchor.item).map(|(old, _)| old.clone()) {
            if let Some(last) = path.iter_mut().find(|(id, _)| id == &anchor.item) {
                last.1 = format!("{} → {}", old.rsplit("::").next().unwrap_or(&old), last.1);
            }
        }
        insert(&mut root, &path, status, class, &owners, &numbers, diff);
    }
    // Renames give no edit (the diff applies them first), so they get their own nodes.
    let no_steps = Vec::new();
    for (old, new) in &renames.items {
        if renames.items.contains_key(&old.rsplit_once("::").map(|(p, _)| p.to_string()).unwrap_or_default()) {
            continue; // A method of a renamed type: the type node shows the rename.
        }
        let mut path = item_path(new, after, before);
        if let Some(last) = path.last_mut() {
            last.1 = format!("{} → {}", old.rsplit("::").next().unwrap_or(old), last.1);
        }
        let numbers = if many { rename_steps.get(old).unwrap_or(&no_steps) } else { &no_steps };
        insert(&mut root, &path, Status::Moved, Some(Class::Neutral), &[], numbers, None);
    }
    for (scope, old, new) in &renames.idents {
        if let Some(item) = scope {
            let item = renames.items.get(item).unwrap_or(item);
            let mut path = item_path(item, after, before);
            path.push((format!("{item}/rename:{old}"), format!("local {old} → {new}")));
            insert(&mut root, &path, Status::Moved, Some(Class::Neutral), &[], &[], None);
        }
    }
    // An uncovered edit that the final diff does not show: a node of its own step (`…/S1@3`).
    for (position, index) in hidden {
        let (outcome, edit) = (&outcomes[position], &outcomes[position].edits[index]);
        let (step_before, step_after) = steps.get(position).copied().unwrap_or((before, after));
        let suffix = format!("@{}", outcome.report.step);
        let path = match edit.anchor() {
            Some(anchor) => {
                let mut path = anchor_path(anchor, if edit.after.is_some() { step_after } else { step_before }, step_after, step_before, &suffix);
                if anchor.path.is_empty() {
                    path.push((format!("{}{suffix}", anchor.item), format!("{} (step {})", format!("{:?}", edit.kind).to_lowercase(), outcome.report.step)));
                }
                path
            }
            None => {
                let file = edit.file.clone().unwrap_or_default();
                vec![("files".into(), "files".into()), (format!("{file}{suffix}"), file)]
            }
        };
        insert(&mut root, &path, Status::Uncovered, None, &[], &[outcome.report.step], None);
    }
    summarize(&mut root);
    if !many {
        clear_steps(&mut root);
    }
    root.steps.clear();
    literal_forms(&mut root);
    root
}

/// Adds the escape form after each literal and each name with a character outside ASCII in
/// the texts of the change tree (batch 5w, part B): `"μs"` and `"µs"` look the same.
fn literal_forms(node: &mut TreeNode) {
    if !node.label.is_ascii() {
        node.label = crate::facts::with_ascii_forms(&node.label);
    }
    if let Some(diff) = node.diff.as_mut().filter(|diff| !diff.replace(['⟦', '⟧', '→', '…'], "").is_ascii()) {
        *diff = crate::facts::with_ascii_forms(diff);
    }
    node.children.iter_mut().for_each(literal_forms);
}

fn clear_steps(node: &mut TreeNode) {
    node.steps.clear();
    node.children.iter_mut().for_each(clear_steps);
}

fn outcome_claim_id(outcome: &StepOutcome, index: usize) -> String {
    outcome.report.claims.get(index).map(|c| c.claim.clone()).unwrap_or_default()
}

fn item_label(path: &str, after: &Workspace, before: &Workspace) -> String {
    let name = path.rsplit("::").next().unwrap_or(path);
    match after.items.get(path).or_else(|| before.items.get(path)) {
        Some(_) if name.starts_with("impl ") => name.to_string(),
        Some(item) => format!("{} {name}", semilla_syntax::diff::kind_name(item.kind)),
        None => name.to_string(),
    }
}

fn short(text: &str) -> String {
    semilla_syntax::tree::short(text, 50)
}

fn insert(root: &mut TreeNode, path: &[(String, String)], status: Status, class: Option<Class>, claims: &[String], steps: &[usize], diff: Option<String>) {
    let mut node = root;
    for (depth, (id, label)) in path.iter().enumerate() {
        let position = match node.children.iter().position(|child| &child.id == id) {
            Some(position) => position,
            None => {
                node.children.push(new_node(id, label));
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
            for claim in claims {
                if !node.claims.contains(claim) {
                    node.claims.push(claim.clone());
                }
            }
            for step in steps {
                if !node.steps.contains(step) {
                    node.steps.push(*step);
                }
            }
            node.steps.sort_unstable();
            if diff.is_some() {
                node.diff = diff.clone();
            }
        }
    }
}

/// Adds the class counts and the step numbers of the descendants to each node with children.
fn summarize(node: &mut TreeNode) -> BTreeMap<&'static str, usize> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for index in 0..node.children.len() {
        for (key, value) in summarize(&mut node.children[index]) {
            *counts.entry(key).or_default() += value;
        }
        for step in node.children[index].steps.clone() {
            if !node.steps.contains(&step) {
                node.steps.push(step);
            }
        }
    }
    node.steps.sort_unstable();
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
    flow_inputs(outcomes)
        .into_iter()
        .filter_map(|(item, (statuses, removed))| after.items.get(&item).and_then(|item| semilla_syntax::flow::flow_view(item, &statuses, removed)))
        .collect()
}

/// The inputs of the flow views: for each changed function, the status of each changed path and
/// the number of removed statements.
pub fn flow_inputs(outcomes: &[StepOutcome]) -> BTreeMap<String, (BTreeMap<String, Status>, usize)> {
    let mut per_item: BTreeMap<String, (BTreeMap<String, Status>, usize)> = BTreeMap::new();
    for outcome in outcomes {
        for (index, edit) in outcome.edits.iter().enumerate() {
            let covered = outcome.owners[index].is_some() || fact_id(outcome, index).is_some();
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
}

/// The inputs of the flow views from the final edits: the paths are paths of the head tree, so a
/// new statement of an early step keeps its `added` status (brief 5f, D3).
pub fn final_flow_inputs(finals: &[FinalEdit]) -> BTreeMap<String, (BTreeMap<String, Status>, usize)> {
    let mut per_item: BTreeMap<String, (BTreeMap<String, Status>, usize)> = BTreeMap::new();
    for f in finals {
        if f.edit.kind == EditKind::Deleted {
            // The view shows the head item, so the count goes to the head path of the item.
            if let Some(anchor) = &f.edit.before {
                per_item.entry(anchor.item.clone()).or_default().1 += 1;
            }
            continue;
        }
        let Some(anchor) = &f.edit.after else { continue };
        if anchor.path.is_empty() {
            continue;
        }
        let path: Vec<String> = anchor.path.iter().map(|s| s.to_string()).collect();
        per_item.entry(anchor.item.clone()).or_default().0.insert(path.join("/"), edit_status(f.edit.kind, f.covered));
    }
    per_item
}

/// The source texts of the anchors of the final edits, before and after.
pub fn final_sources(finals: &[FinalEdit], before: &Workspace, after: &Workspace) -> BTreeMap<String, (Option<String>, Option<String>)> {
    sources_of(&finals.iter().map(|f| &f.edit).collect::<Vec<_>>(), before, after)
}

/// Builds the workspace graph of the "after" side, with the statuses of the change set.
pub fn graph(outcomes: &[StepOutcome], after: &Workspace) -> WorkspaceGraph {
    let mut statuses: BTreeMap<String, Status> = BTreeMap::new();
    let mut changes: BTreeMap<String, usize> = BTreeMap::new();
    for outcome in outcomes {
        for (index, edit) in outcome.edits.iter().enumerate() {
            let Some(item) = edit.item() else { continue };
            *changes.entry(item.to_string()).or_default() += 1;
            let status = edit_status(edit.kind, outcome.owners[index].is_some() || fact_id(outcome, index).is_some());
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
    sources_of(&outcomes.iter().flat_map(|outcome| &outcome.edits).collect::<Vec<_>>(), before, after)
}

fn sources_of(edits: &[&Edit], before: &Workspace, after: &Workspace) -> BTreeMap<String, (Option<String>, Option<String>)> {
    let mut map = BTreeMap::new();
    {
        for edit in edits {
            let key = edit.anchor().map(|a| a.to_string()).or_else(|| edit.file.clone()).unwrap_or_default();
            // An item-level change (docs, signature, members): the whole item on both sides, so that
            // both sides have the same form.
            if let Some(anchor) = edit.anchor().filter(|a| a.path.is_empty()) {
                let old = edit.before.as_ref().and_then(|a| before.items.get(&a.item)).map(|i| i.source.clone());
                let new = edit.after.as_ref().and_then(|a| after.items.get(&a.item)).map(|i| i.source.clone());
                map.insert(anchor.to_string(), (old, new));
                continue;
            }
            let entry = map.entry(key).or_insert((None, None));
            if entry.0.is_none() {
                entry.0 = edit.source_before.clone().or_else(|| edit.text_before.clone());
            }
            entry.1 = edit.source_after.clone().or_else(|| edit.text_after.clone());
        }
    }
    // The full source of each changed function, for the flow views. Unchanged functions are not
    // in the map.
    let changed: std::collections::BTreeSet<String> = edits.iter().filter_map(|e| e.item().map(str::to_string)).collect();
    for item in after.items.values().filter(|i| i.kind == semilla_syntax::ItemKind::Fn && changed.contains(&i.path)) {
        let entry = map.entry(item.path.clone()).or_insert((None, None));
        entry.1 = Some(item.source.clone());
        if entry.0.is_none() {
            entry.0 = before.items.get(&item.path).map(|i| i.source.clone());
        }
    }
    map.retain(|_, (old, new)| old != new);
    map
}

/// The ID of the fact that owns an edit, if any.
fn fact_id(outcome: &StepOutcome, index: usize) -> Option<String> {
    let fact = outcome.fact_of.get(index).copied().flatten()?;
    outcome.report.facts.get(fact).map(|f| f.id.clone())
}

/// A text report of the steps, for the CLI and the agent tools (design section 8.15).
pub fn text_report(steps: &[StepReport]) -> String {
    text_report_with(steps, false)
}

/// The short cause of a claim that is not verified, for a claim list below the full text: the
/// start of the reason, to the first `:`, `;`, `.`, or `(` that is not in a code span.
pub fn short_cause(status: &ClaimStatus) -> String {
    let (word, reason) = match status {
        ClaimStatus::Verified { proof } => return format!("verified ({proof})"),
        ClaimStatus::Unproved { reason } => ("unproved", reason.as_str()),
        ClaimStatus::Refuted { reason } => ("refuted", reason.as_str()),
        ClaimStatus::Malformed { reason } => ("malformed", reason.as_str()),
        ClaimStatus::Pending { reason } => ("pending", reason.as_str()),
        ClaimStatus::NotChecked { .. } => return "not checked".to_string(),
        ClaimStatus::Empty => return "empty".to_string(),
    };
    let mut code = false;
    let mut end = reason.len();
    let mut chars = reason.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let space_next = chars.peek().is_none_or(|(_, next)| next.is_whitespace());
        match c {
            '`' => code = !code,
            ':' | ';' | '.' if !code && space_next => {
                end = at;
                break;
            }
            '(' if !code && reason[..at].ends_with(' ') => {
                end = at;
                break;
            }
            _ => {}
        }
    }
    let mut cause = reason[..end].trim().to_string();
    if cause.chars().count() > 80 {
        // A long first clause: cut it at a word border.
        let cut: String = cause.chars().take(80).collect();
        cause = format!("{} …", cut.rsplit_once(' ').map(|(start, _)| start).unwrap_or(&cut));
    }
    if cause.is_empty() { word.to_string() } else { format!("{word}: {cause}") }
}

pub use semilla_core::report::{EQUAL_SITES, LIST_LIMIT};

/// A list of bodies for the text output: the first [`LIST_LIMIT`] names, then "and N more".
pub fn short_list(items: &[String]) -> String {
    let shown: Vec<&str> = items.iter().take(LIST_LIMIT).map(String::as_str).collect();
    let more = if items.len() > LIST_LIMIT { format!(", and {} more", items.len() - LIST_LIMIT) } else { String::new() };
    format!("{}{more}", shown.join(", "))
}

/// Writes one fact of the text report. `diff` adds the content change of the fact (a build code
/// change, design 28.3).
fn put_fact(out: &mut String, fact: &semilla_core::report::Fact, diff: bool) {
    use text::put_line as put;
        let class = match (fact.class, fact.affected.is_empty()) {
            (Some(Class::Neutral), _) => " [neutral (proved)]".to_string(),
            (Some(class), _) => format!(" [{}]", crate::class_name(class)),
            (None, _) => " [pending]".to_string(),
        };
        put(out, &format!("  ◆ {} derived: {}{class}{}", fact.id, fact.description, fact.mark()));
        for group in fact.sites.iter().filter(|group| !fact.names_sites(group)) {
            if matches!(fact.part.as_str(), "edit" | "body") {
                // A fold of equal facts: the bodies, with the count of sites in each.
                let mut items: Vec<(String, usize)> = Vec::new();
                for site in &group.sites {
                    match items.iter_mut().find(|(item, _)| *item == site.item) {
                        Some((_, count)) => *count += 1,
                        None => items.push((site.item.clone(), 1)),
                    }
                }
                let shown: Vec<String> = items.iter().take(6).map(|(item, count)| if *count > 1 { format!("{item} ({count})") } else { item.clone() }).collect();
                let more = if items.len() > 6 { format!(", and {} more", items.len() - 6) } else { String::new() };
                put(out, &format!("      in: {}{more}", shown.join(", ")));
            } else {
                // The bodies of the sites (D15): a body with a site can be proved equal, so it
                // is not always in the affected bodies.
                let mut items: Vec<&str> = group.sites.iter().map(|s| s.item.as_str()).collect();
                items.dedup();
                let more = if items.len() > 4 { format!(", and {} more", items.len() - 4) } else { String::new() };
                items.truncate(4);
                put(out, &format!("      {} site(s): {} (in {}{more})", group.sites.len(), group.edit, items.join(", ")));
            }
        }
        if !fact.affected.is_empty() {
            put(out, &format!("      affected bodies: {}", short_list(&fact.affected)));
        }
        if !fact.equal_sites.is_empty() {
            put(out, &format!("      {EQUAL_SITES}{}: {}", fact.part_mark(), short_list(&fact.equal_sites)));
        }
        // The note asks for no reason (batch 5u, part A).
        if let Some(note) = &fact.note {
            put(out, &format!("      note: {note}{}", fact.part_mark()));
        }
        if let Some(why) = &fact.spotlight_why {
            match &fact.reason {
                Some(reason) => put(out, &format!("      spotlight: {why}; reason: {}", text::quoted(reason))),
                None => put(out, &format!("      ✗ spotlight: {why}; it needs a reason: --reason '{}=…'", fact.at.as_ref().map(|a| a.to_string()).unwrap_or_else(|| fact.part.clone()))),
            }
        }
    if let Some(content) = fact.content.as_ref().filter(|_| diff) {
        put(out, "      diff:");
        for line in content.lines() {
            put(out, &format!("        {line}"));
        }
    }
}

/// The text report. With `short_failures`, a claim that is not verified shows only its status
/// and a short cause: the caller prints the full text and the hint before the report.
pub fn text_report_with(steps: &[StepReport], short_failures: bool) -> String {
    // Each line of the report can hold text of the agent or of the repository: `put` shows
    // each hidden character of a line as an escape, so no text starts a new line (5k, A2).
    use text::put_line as put;
    let mut out = String::new();
    for step in steps {
        put(&mut out, &format!("step {}  {}", step.step, text::quoted(&step.intent)));
        // Design 28.3: the build code changes of the step come first, with the diff.
        if step.facts.iter().any(|fact| fact.build_code_change) {
            put(&mut out, &format!("  ! {}", semilla_core::report::BUILD_CODE_SECTION));
            for fact in step.facts.iter().filter(|fact| fact.build_code_change) {
                put_fact(&mut out, fact, true);
            }
        }
        for claim in &step.claims {
            let mark = match &claim.status {
                ClaimStatus::Verified { .. } => "✓",
                ClaimStatus::Unproved { .. } => "?",
                ClaimStatus::Pending { .. } => "…",
                ClaimStatus::NotChecked { .. } => "–",
                _ => "✗",
            };
            if short_failures && !claim.status.is_verified() {
                put(&mut out, &format!("  {mark} {} {}", claim.claim, short_cause(&claim.status)));
                continue;
            }
            let detail = claim.status.text();
            let class = claim.class.map(|c| format!(" [{}]", crate::class_name(c))).unwrap_or_default();
            let effect = claim.effect.map(|e| format!(" effect hint: {e:?}").to_lowercase()).unwrap_or_default();
            let witness = claim.witness.as_ref().map(|w| format!(" (example: {w})")).unwrap_or_default();
            put(&mut out, &format!("  {mark} {} {detail}{}{class}{effect}{witness}", claim.claim, claim.mark()));
            if !claim.affected.is_empty() {
                put(&mut out, &format!("      changed bodies: {}", short_list(&claim.affected)));
            }
            // A property claim states its scope (what Semilla examined, and what it did not).
            let property = matches!(&claim.status, ClaimStatus::Verified { proof } if matches!(proof.as_str(), "partial" | "mir" | "graph" | "syntax"));
            if let Some(scope) = claim.after.as_ref().filter(|_| property) {
                put(&mut out, &format!("      scope: {scope}"));
            }
            if let Some(hint) = &claim.hint {
                put(&mut out, &format!("      hint: {hint}"));
            }
        }
        // The facts: Semilla derives them, the agent writes no claim (design 22.5).
        for fact in step.facts.iter().filter(|fact| !fact.build_code_change) {
            put_fact(&mut out, fact, false);
        }
        for uncovered in &step.uncovered {
            let at = uncovered.at.as_ref().map(|a| a.to_string()).or_else(|| uncovered.file.clone()).unwrap_or_default();
            put(&mut out, &format!("  ✗ uncovered  {at}"));
            put(&mut out, &format!("       {}: {}", format!("{:?}", uncovered.view).to_lowercase(), uncovered.description));
            if let Some(hint) = &uncovered.hint {
                put(&mut out, &format!("       hint: add {hint}"));
            }
        }
        for item in &step.spotlight {
            put(&mut out, &format!("  ! spotlight: {item}"));
        }
        for error in &step.errors {
            // The output of a tool has more than one line: each next line gets an indent.
            out.push_str(&format!("  ✗ error: {}\n", text::block_indent(error, "      ")));
        }
        for note in &step.notes {
            put(&mut out, &format!("  note: {note}"));
        }
        out.push('\n');
    }
    out
}
