//! The views that the UI and the CLI show: the change tree, the flow views, the workspace graph,
//! and the source texts.

use std::collections::BTreeMap;

use semilla_core::view::{FlowGraph, Status, TreeNode, WorkspaceGraph};
use semilla_core::report::TagGroup;
use semilla_core::{text, Category, ClaimStatus, Compare, StepReport};

/// The category and the compare result of an edit or of a tree node (design 35).
pub type Mark = (Option<Category>, Option<Compare>);

/// Joins two marks. The category with the highest weight stays. The compare result that comes
/// first in the report order stays.
/// The most symbols of the "through:" line of a fact (design 44).
const THROUGH_LIMIT: usize = 3;

pub fn merge_marks(old: Mark, new: Mark) -> Mark {
    let category = match (old.0, new.0) {
        (Some(a), Some(b)) => Some(if b.weight() > a.weight() { b } else { a }),
        (a, b) => a.or(b),
    };
    let compare = match (old.1, new.1) {
        (Some(a), Some(b)) => Some(if b.order() < a.order() { b } else { a }),
        (a, b) => a.or(b),
    };
    (category, compare)
}
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
    /// The category and the compare result of the owners.
    pub mark: Mark,
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
    mark: Mark,
    head: Option<String>,
    base: Option<String>,
}

fn step_edits(outcomes: &[StepOutcome]) -> Vec<StepEdit<'_>> {
    let names = Names(outcomes.iter().map(|outcome| &outcome.renames.items).collect());
    let mut all = Vec::new();
    for (position, outcome) in outcomes.iter().enumerate() {
        let mut marks: BTreeMap<String, Mark> = outcome.report.claims.iter().map(|c| (c.claim.clone(), (c.category, None))).collect();
        marks.extend(outcome.report.facts.iter().map(|f| (f.id.clone(), (Some(f.category), (!f.compare.is_off()).then_some(f.compare)))));
        for (index, edit) in outcome.edits.iter().enumerate() {
            // A fact owns its edits like a claim: the tree shows the fact ID (`f1`).
            let owner = outcome.owners[index].map(|claim| outcome_claim_id(outcome, claim)).or_else(|| fact_id(outcome, index));
            let mark = owner.as_ref().and_then(|id| marks.get(id).copied()).unwrap_or_default();
            let head = edit.after.as_ref().map(|anchor| names.forward(&anchor.item, position + 1));
            let base = edit.before.as_ref().and_then(|anchor| names.backward(&anchor.item, position));
            all.push(StepEdit { edit, position, step: outcome.report.step, owner, mark, head, base });
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
        let finals = steps.iter().map(|step| FinalEdit { edit: step.edit.clone(), steps: Vec::new(), owners: step.owner.clone().into_iter().collect(), mark: step.mark, covered: step.owner.is_some() }).collect();
        return (finals, Vec::new());
    }
    let (mut renames, _) = composed_renames(outcomes);
    crate::sites::resolve(&mut renames, base, head);
    let all_covered = steps.iter().all(|step| step.owner.is_some());
    let mut shown = vec![false; steps.len()];
    let mut finals = Vec::new();
    for edit in semilla_syntax::diff(base, head, &renames) {
        let makers = makers(&edit, &steps);
        let mut numbers: Vec<usize> = makers.iter().map(|&index| steps[index].step).collect();
        numbers.sort_unstable();
        numbers.dedup();
        let mut owners: Vec<String> = Vec::new();
        let mut mark: Mark = (None, None);
        for &index in &makers {
            shown[index] = true;
            if let Some(owner) = &steps[index].owner {
                if !owners.contains(owner) {
                    owners.push(owner.clone());
                }
            }
            mark = merge_marks(mark, steps[index].mark);
        }
        let covered = if makers.is_empty() { all_covered } else { makers.iter().all(|&index| steps[index].owner.is_some()) };
        finals.push(FinalEdit { edit, steps: numbers, owners, mark, covered });
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
    TreeNode { id: id.into(), label: label.into(), status: Status::Changed, category: None, compare: None, build_code: None, claims: Vec::new(), diff: None, steps: Vec::new(), summary: None, children: Vec::new() }
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
        let mut mark = f.mark;
        if let Some(added) = pair {
            owners.extend(added.owners.iter().filter(|owner| !f.owners.contains(owner)).cloned());
            numbers.extend(added.steps.iter().copied());
            numbers.sort_unstable();
            numbers.dedup();
            if many {
                covered &= added.covered;
                mark = merge_marks(mark, added.mark);
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
            // A cut keeps each part that differs (batch 7b, part 5.6): the two sides never look
            // the same.
            (EditKind::Modified | EditKind::Replaced, Some(old), Some(new)) => {
                let (old, new) = crate::edits::sides(old, new);
                Some(format!("⟦{old}⟧→⟦{new}⟧"))
            }
            _ => None,
        };
        let Some(anchor) = edit.anchor() else {
            let file = edit.file.clone().unwrap_or_default();
            insert(&mut root, &[("files".into(), "files".into()), (file.clone(), file)], status, mark, &owners, &numbers, diff);
            continue;
        };
        let mut path = anchor_path(anchor, if edit.after.is_some() { after } else { before }, after, before, "");
        if let Some(old) = renames.items.iter().find(|(_, new)| *new == &anchor.item).map(|(old, _)| old.clone()) {
            if let Some(last) = path.iter_mut().find(|(id, _)| id == &anchor.item) {
                last.1 = format!("{} → {}", old.rsplit("::").next().unwrap_or(&old), last.1);
            }
        }
        insert(&mut root, &path, status, mark, &owners, &numbers, diff);
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
        insert(&mut root, &path, Status::Moved, (Some(if old.rsplit_once("::").map(|(p, _)| p) == new.rsplit_once("::").map(|(p, _)| p) { Category::Rename } else { Category::Move }), None), &[], numbers, None);
    }
    for (scope, old, new) in &renames.idents {
        if let Some(item) = scope {
            let item = renames.items.get(item).unwrap_or(item);
            let mut path = item_path(item, after, before);
            path.push((format!("{item}/rename:{old}"), format!("local {old} → {new}")));
            insert(&mut root, &path, Status::Moved, (Some(Category::Rename), None), &[], &[], None);
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
        insert(&mut root, &path, Status::Uncovered, (None, None), &[], &[outcome.report.step], None);
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

fn insert(root: &mut TreeNode, path: &[(String, String)], status: Status, mark: Mark, claims: &[String], steps: &[usize], diff: Option<String>) {
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
            (node.category, node.compare) = merge_marks((node.category, node.compare), mark);
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

/// Adds the counts of the compare results and the step numbers of the descendants to each node with children.
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
        let key = match (node.status, node.compare) {
            (Status::Uncovered, _) => "uncovered",
            (_, Some(compare)) => compare.name(),
            // A fact of a step with no compiled check (design 50).
            (_, None) if node.category.is_some() => "changed",
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
    text_report_lists(steps, false, true, false)
}

/// The short cause of a claim that is not verified, for a claim list below the full text: the
/// start of the reason, to the first `:`, `;`, `.`, or `(` that is not in a code span.
pub fn short_cause(status: &ClaimStatus) -> String {
    let (word, reason) = match status {
        ClaimStatus::Verified { proof } => return format!("verified ({proof})"),
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

/// A list of bodies for the text output (batch 5x, part E). A list of more than [`LIST_LIMIT`]
/// bodies shows the count and the scope, and `command`, which shows the names (R76.1-1). `names`
/// shows each name (a full text).
pub fn body_list(items: &[String], names: bool, command: &str) -> String {
    if names || items.len() <= LIST_LIMIT {
        return items.join(", ");
    }
    format!("{} bodies of {} (`{command}` shows the names)", items.len(), semilla_core::report::list_scope(items))
}

/// The shortest spotlight text that the text report prints one time for many facts, and the
/// shortest line below a fact that shows one time for many facts (R71.1-2).
const SHARE_MIN: usize = 120;

/// The words of a line below a fact that another fact shows (R71.1-2).
pub const SAME_LINE_AS: &str = "the same line as ";

/// The place of a fact in the list of a step (design 37.18). The fact numbers do not change.
/// - 0: a build code change that is no manifest fact (design 28.3: read this change first);
/// - 1: an edit of a body with `code different`;
/// - 2: a declaration, an added item, a removed item;
/// - 3: each other code fact;
/// - 4: a manifest fact;
/// - 5: a docs fact, a file fact, and each other fact with `no code`.
fn rank(fact: &semilla_core::report::Fact) -> u8 {
    fact.text_rank()
}

/// The fact numbers in the order of the numbers.
fn in_order<'a>(ids: &[&'a str]) -> Vec<&'a str> {
    let mut ids = ids.to_vec();
    ids.sort_by_key(|id| id.strip_prefix('f').and_then(|n| n.parse::<usize>().ok()));
    ids
}

/// A short list of fact numbers: "f3, f18 to f39". A run of three numbers or more is a range.
fn id_list(ids: &[&str]) -> String {
    let number = |id: &str| id.strip_prefix('f').and_then(|n| n.parse::<usize>().ok());
    let mut parts: Vec<String> = Vec::new();
    let mut index = 0;
    while index < ids.len() {
        let mut end = index;
        while end + 1 < ids.len() && number(ids[end]).zip(number(ids[end + 1])).is_some_and(|(a, b)| b == a + 1) {
            end += 1;
        }
        if end - index >= 2 {
            parts.push(format!("{} to {}", ids[index], ids[end]));
        } else {
            parts.extend(ids[index..=end].iter().map(|id| id.to_string()));
        }
        index = end + 1;
    }
    parts.join(", ")
}

/// The part of a spotlight text that many facts can have: the "build input" text, or the full
/// text. Returns the text before the part, the name of the part, and the part.
fn shared_text(why: &str) -> (&str, &'static str, &str) {
    const INPUT: &str = "a build input: ";
    if let Some(rest) = why.strip_prefix(INPUT) {
        return ("", INPUT, rest);
    }
    match why.find(&format!("; {INPUT}")) {
        Some(at) => (&why[..at + 2], INPUT, &why[at + 2 + INPUT.len()..]),
        None => ("", "", why),
    }
}

/// The texts that the text report prints one time for many facts of a step (design 37.13).
#[derive(Default)]
struct Shared<'a> {
    /// A spotlight text of two facts or more: the facts with the text, in the order of the list.
    /// The key has the full list of the causes (design 38.1).
    spotlights: BTreeMap<(&'a str, String), Vec<&'a str>>,
    /// A long list of bodies of two facts or more: the facts with the list. The key is the full
    /// list (design 38.1): two lists with the same short text and different names stay apart.
    bodies: BTreeMap<String, Vec<&'a str>>,
    /// The content change of a file with two facts or more: the facts, in the order of the list.
    diffs: BTreeMap<(&'a str, &'a str), Vec<&'a str>>,
    /// The control flow graph diff of the step (design 60). With it, the `control flow:` lines
    /// replace the `branches:` line of each fact.
    flow: Option<&'a semilla_core::report::FlowReport>,
    /// The step of a tag after the commit (design 62).
    after_commit: bool,
    /// The index of the step, for the command of a text over the budget (R71.1-1). `None`: the
    /// output shows each text in full.
    step: Option<usize>,
    /// The index of the step, for the command of a cut list (R76.1-1).
    index: usize,
    /// [R92.5-1] The report of the step: the gap list gives the statement of the line "not
    /// stated" of an entry.
    report: Option<&'a StepReport>,
}

impl<'a> Shared<'a> {
    fn new(facts: &[&'a semilla_core::report::Fact], names: bool, index: usize) -> Self {
        let mut shared = Shared { index, ..Shared::default() };
        for fact in facts {
            if Self::body_text(fact, names, index).len() >= SHARE_MIN {
                shared.bodies.entry(Self::body_key(fact)).or_default().push(&fact.id);
            }
            if let Some(why) = &fact.spotlight_why {
                let (_, _, part) = shared_text(why);
                if part.len() >= SHARE_MIN {
                    shared.spotlights.entry((part, fact.causes.join(", "))).or_default().push(&fact.id);
                }
            }
            if let Some(content) = fact.content.as_deref().filter(|_| fact.build_code_change) {
                shared.diffs.entry((fact.file.as_deref().unwrap_or(""), content)).or_default().push(&fact.id);
            }
        }
        shared.spotlights.retain(|_, ids| ids.len() > 1);
        shared.bodies.retain(|_, ids| ids.len() > 1);
        shared
    }

    /// The text of the list of the affected bodies of a fact. A cut list names the command of
    /// the fact (R76.1-1).
    fn body_text(fact: &semilla_core::report::Fact, names: bool, step: usize) -> String {
        format!("{}{}", body_list(&fact.affected, names, &full_text_command(&fact.id, step)), fact.other_edits_text())
    }

    /// The key of a list of bodies: each name, and the count of the other edits.
    fn body_key(fact: &semilla_core::report::Fact) -> String {
        Self::body_text(fact, true, 0)
    }

    /// The list of the affected bodies of a fact. The first fact of a shared list shows the
    /// list and the numbers of the facts. Each other fact names the first fact.
    fn bodies(&self, fact: &semilla_core::report::Fact, names: bool) -> String {
        let text = Self::body_text(fact, names, self.index);
        // A fact of a run of 38.4 is not in the map: it keeps its own list.
        match self.bodies.get(&Self::body_key(fact)).filter(|ids| ids.contains(&fact.id.as_str())) {
            Some(ids) if ids[0] == fact.id => format!("{text} (the same list for {})", id_list(&in_order(ids))),
            Some(ids) => format!("the same list as {}", ids[0]),
            None => text,
        }
    }

    /// The spotlight text of a fact. The first fact of a shared text shows the text and the
    /// numbers of the facts. Each other fact names the first fact.
    fn spotlight(&self, fact: &semilla_core::report::Fact, why: &str) -> String {
        let (before, name, part) = shared_text(why);
        match self.spotlights.get(&(part, fact.causes.join(", "))).filter(|ids| ids.contains(&fact.id.as_str())) {
            Some(ids) if ids[0] == fact.id => format!("{why} (the same text for {})", id_list(&in_order(ids))),
            Some(ids) => format!("{before}{name}the same text as {}", ids[0]),
            None => why.to_string(),
        }
    }
}

/// The lines of one fact of the text report: the first line without the mark and the number,
/// and the lines under it. `diff` adds the content change of the fact (a build code change,
/// design 28.3): a change of a file with many facts shows one time, after the last fact.
fn fact_lines(fact: &semilla_core::report::Fact, shared: &Shared, names: bool) -> (String, Vec<String>, Vec<String>) {
    // The origin of the fact (design 54.5): `derived` (Semilla filled it in) or `stated`.
    let origin = if fact.status.is_empty() { "derived" } else { fact.status.as_str() };
    // Design 54.7: a long text and a text with more than one line are in a block below the
    // fact line. No line of a block has a cut.
    // R71.1-1: a text over the budget shows the changed part, the sizes, and the command that
    // shows the full text. Docs show each changed line.
    let command = shared.step.map(|step| full_text_command(&fact.id, step));
    let budget = command.as_deref().map(|command| semilla_core::text::Budget { command: Some(command), keep: fact.category == semilla_core::Category::Docs });
    let (text, blocks) = semilla_core::text::fact_layout_with(&fact.description, budget);
    // [R90.5-1] The head of an entry: the kind, the source, and the check (design 90); a record
    // of an earlier version keeps its origin. [R90.5-2] A real edit that no statement names has
    // the mark "not stated".
    let origin = fact.entry_head().unwrap_or_else(|| origin.to_string());
    let head = format!("{origin}: {text} [{}]{}", fact.label(), fact.mark());
    let mut lines: Vec<String> = blocks.iter().map(|line| format!("        {line}")).collect();
    // [R92.5-1] The line "not stated" with the statement that states the entry (design 92).
    if let Some(line) = shared.report.and_then(|report| report.not_stated_line(fact)) {
        lines.push(format!("      {line}"));
    }
    // The line of each shown line before its cut (R76.1-1): the compare of R71.1-2 reads it, so
    // that two facts with numbers of a different length share an equal line.
    let mut raws: Vec<String> = lines.clone();
    // A line of a list (`cut`): the cut of design 38.5 first, then the texts over the budget
    // (R71.1-1). The cut names the command of the fact (R76.1-1).
    let fact_command = full_text_command(&fact.id, shared.index);
    let mut put = |line: &str, cut: bool| {
        let shown = if cut && !names { fit_line(line, &fact_command) } else { line.to_string() };
        lines.push(match budget {
            Some(budget) => semilla_core::budget::line(&shown, budget.command, budget.keep, is_spotlight_line(&shown)),
            None => shown,
        });
        raws.push(line.to_string());
    };
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
            put(&format!("      in: {}{more}", shown.join(", ")), false);
        } else {
            // The bodies of the sites (D15): a body with a site can be proved equal, so it
            // is not always in the affected bodies.
            let mut items: Vec<&str> = group.sites.iter().map(|s| s.item.as_str()).collect();
            items.dedup();
            let more = if items.len() > 4 { format!(", and {} more", items.len() - 4) } else { String::new() };
            items.truncate(4);
            put(&format!("      {} site(s): {} (in {}{more})", group.sites.len(), semilla_core::text::flat_text(&group.edit), items.join(", ")), false);
        }
    }
    if !fact.affected.is_empty() {
        // The mismatch mark (design 35.2) replaces the list name. It asks for no reason.
        // A step with no compiled check has no body list from a compare (design 50). Its list
        // comes from the syntax tree: the items with a use of the changed item or file.
        let name = if fact.mismatch() {
            semilla_core::report::MISMATCH
        } else if fact.compare == Compare::Off {
            "items with a use:"
        } else {
            "affected bodies:"
        };
        put(&format!("      {name} {}", shared.bodies(fact, names)), false);
    }
    if !fact.equal_sites.is_empty() {
        put(&format!("      {EQUAL_SITES}{}: {}", fact.part_mark(), body_list(&fact.equal_sites, names, &fact_command)), false);
    }
    // The symbol links (design 44): the line asks for no reason.
    if !fact.through.is_empty() {
        let list: Vec<String> = fact.through.iter().take(THROUGH_LIMIT).map(|(symbol, bodies)| format!("`{symbol}` ({} {})", bodies.len(), if bodies.len() == 1 { "body" } else { "bodies" })).collect();
        let more = if fact.through.len() > THROUGH_LIMIT { format!(", and {} more", fact.through.len() - THROUGH_LIMIT) } else { String::new() };
        put(&format!("      through: {}{more}", list.join(", ")), false);
    }
    // The branch mark (design 40.4): the line asks for no reason. With the graph of each body
    // (design 60, C1), the `control flow:` lines replace it.
    match shared.flow {
        Some(flow) => {
            // Design 38.5: a long line has a cut; `status --verbose` shows the full line.
            for line in flow.facts.get(&fact.id).into_iter().flatten() {
                put(&format!("      control flow: {line}"), true);
            }
            // The equivalence of the body (design 61, E1).
            for line in flow.equivalence.get(&fact.id).into_iter().flatten() {
                put(&format!("      equivalence: {line}"), true);
            }
            // The changed expansions that the fact owns (design 63, R63.3-3).
            for line in flow.expansion_lines.get(&fact.id).into_iter().flatten() {
                put(&format!("      expansion: {line}"), true);
            }
            // The callers that the change of the body reaches (design 61.10, A5).
            if let Some((line, _)) = flow.reaches.get(&fact.id) {
                put(&format!("      reaches: {line}"), true);
            }
        }
        None => {
            if let Some(branches) = &fact.branches {
                put(&format!("      branches: {branches}"), false);
            }
        }
    }
    // A public API change (design 36.3): the line asks for no reason. The line is about the
    // compare, so a fact with no compare result (design 50) does not have it: the fact text
    // tells the API change.
    if fact.api_change() && !fact.compare.is_off() {
        put(&format!("      {}", semilla_core::report::API_LINE), false);
    }
    // A changed body with `unsafe` code (design 36.5): the line asks for no reason.
    if fact.unsafe_code {
        put(&format!("      {}", semilla_core::report::UNSAFE_LINE), false);
    }
    // The note asks for no reason (batch 5u, part A).
    if let Some(note) = &fact.note {
        put(&format!("      note: {note}{}", fact.part_mark()), false);
    }
    // A stored reason of a fact with no spotlight is a note (batch 5x, part E).
    if let Some(reason) = fact.reason.as_ref().filter(|_| fact.spotlight_why.is_none()) {
        put(&format!("      note: no spotlight; the agent gave a reason: {}", text::quoted(reason)), false);
    }
    if let Some(why) = &fact.spotlight_why {
        let why = shared.spotlight(fact, why);
        match &fact.reason {
            Some(reason) => put(&format!("      spotlight: {why}; {}: {}", fact.reason_label(), text::quoted(reason)), false),
            // Design 62: in a tag after the commit, the spotlight is an open item.
            None if shared.after_commit => put(&format!("      ! spotlight: {why}; {}", semilla_core::report::OPEN_SPOTLIGHT), false),
            None => put(&format!("      ✗ spotlight: {why}; it needs a reason: --reason '{}=…'", fact.reason_place()), false),
        }
        // The full list of the causes (design 37.14), one time for each shared text.
        let first = self::shared_text(fact.spotlight_why.as_deref().unwrap_or("")).2;
        if names && !fact.causes.is_empty() && shared.spotlights.get(&(first, fact.causes.join(", "))).is_none_or(|ids| ids[0] == fact.id) {
            put(&format!("      each cause: {}", fact.causes.join(", ")), false);
        }
    }
    if let Some(content) = fact.content.as_deref().filter(|_| fact.build_code_change) {
        let ids = shared.diffs.get(&(fact.file.as_deref().unwrap_or(""), content)).map(Vec::as_slice).unwrap_or(&[]);
        if ids.last().is_none_or(|last| *last == fact.id) {
            if ids.len() > 1 {
                put(&format!("      diff of `{}` (one time, for {}):", fact.file.as_deref().unwrap_or(""), id_list(&in_order(ids))), false);
            } else {
                put("      diff:", false);
            }
            for line in content.lines() {
                put(&format!("        {line}"), false);
            }
        }
    }
    // Design 55.5, class 4: a doc test fact shows the changed doc lines. A code block of the
    // docs is code: `cargo test` compiles and runs it.
    if let Some(content) = fact.content.as_deref().filter(|_| fact.part == "doc test" && !fact.build_code_change) {
        put(&format!("      {DOC_TEST_LINES}"), false);
        for line in content.lines() {
            put(&format!("        {line}"), false);
        }
    }
    (head, lines, raws)
}

/// The command that shows each text of a fact in full (R71.1-1).
pub use semilla_core::budget::full_text_command;

/// The words of a line of a fact that name its full-text command (R76.1-1). The merge of
/// equal facts (design 38.1) and the lines shown one time (R71.1-2) compare the lines with
/// the command of the fact in the form of a mark, so that the command does not keep two facts
/// apart. The command of the size form of R71.1-1 keeps the fact number: two texts over the
/// budget with one short form can have different full texts.
fn command_key(text: &str, own: &str) -> String {
    let mut text = text.to_string();
    for words in [" shows the full line", " shows the names"] {
        text = text.replace(&format!("`{own}`{words}"), &format!("`\u{1}`{words}"));
    }
    text
}

/// The line above the changed doc lines of a doc test fact (design 55.5).
pub const DOC_TEST_LINES: &str = "changed doc lines (a code block of the docs is the code of a doc test; `-` old, `+` new):";

/// True for the spotlight line of a fact.
fn is_spotlight_line(line: &str) -> bool {
    line.starts_with("      spotlight: ") || line.starts_with("      ✗ spotlight: ") || line.starts_with("      ! spotlight: ")
}

/// The runs of facts (in the order of the output) with an equal spotlight line (design 38.4):
/// for each fact of a run of two facts or more, the first and the last index of the run.
fn spotlight_runs(order: &[&semilla_core::report::Fact], names: bool, place_of: &dyn Fn(&semilla_core::report::Fact) -> Result<usize, usize>) -> Vec<Option<(usize, usize)>> {
    let none = Shared::default();
    let lines: Vec<Option<String>> = order.iter().map(|fact| fact_lines(fact, &none, names).1.into_iter().find(|line| is_spotlight_line(line))).collect();
    let mut runs = vec![None; order.len()];
    let mut start = 0;
    while start < order.len() {
        let mut end = start;
        while lines[start].is_some() && end + 1 < order.len() && lines[end + 1] == lines[start] && rank(order[end + 1]) == rank(order[start]) && place_of(order[end + 1]) == place_of(order[start]) {
            end += 1;
        }
        if end > start {
            runs[start..=end].iter_mut().for_each(|run| *run = Some((start, end)));
        }
        start = end + 1;
    }
    runs
}

/// The longest line of the text output (design 38.5), in characters.
pub const LINE_MAX: usize = 500;

/// A line of the text output with at most [`LINE_MAX`] characters (design 38.5). A long list
/// (items with ", " between them) shows its first items and its last items, with the count of
/// the items that it does not show. Each other long line shows its start and its end. The cut
/// names `command`, which shows the full line (R76.1-1): the command of the fact for a line of
/// a fact, else the command of the step.
///
/// [R66.10-6] The cut keeps each result of a check: a result word ("equivalent", "different",
/// "not decided", "not checked", also with "alone") and the list of its configurations ("in dev,
/// release, dev, no default features (libraries and binaries only)"). If the cut above hides a
/// part of a result, the cut hides other text in its place, in this order: items in the middle
/// of the longest list of code names (items in backquotes, such as the inlined callees of a
/// proof); items in the middle of the longest list with no result; characters in the middle of
/// the longest text with no result. A cut that hides no part of a result stays as it was, so the
/// lines with no result (the lines of phase 1 and of the syntax-only check) keep their text.
pub fn fit_line(line: &str, command: &str) -> String {
    const HEAD: usize = 300;
    const TAIL: usize = 100;
    let count = |text: &str| text.chars().count();
    // R71.1-1: a line with a text in the form of the size rule has no cut: the rule bounds its
    // unchanged parts, and its changed parts show in full.
    if count(line) <= LINE_MAX || line.contains(semilla_core::budget::LARGE_TEXT_MARK) {
        return line.to_string();
    }
    let full = format!("`{command}` shows the full line");
    let full = full.as_str();
    let parts: Vec<&str> = line.split(", ").collect();
    // The byte range of each item in the line.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    for part in &parts {
        ranges.push((at, at + part.len()));
        at += part.len() + 2;
    }
    let results = result_ranges(line);
    let touches = |from: usize, to: usize| results.iter().any(|(a, b)| from < *b && *a < to);
    // The cut of a list: its first items and its last items.
    let mut old: Option<(String, (usize, usize))> = None;
    if parts.len() >= 6 {
        let mut head = 0;
        let mut size = 0;
        while head < parts.len() && size + count(parts[head]) + 2 <= HEAD {
            size += count(parts[head]) + 2;
            head += 1;
        }
        let mut tail = 0;
        let mut size = 0;
        while tail + head < parts.len() && size + count(parts[parts.len() - 1 - tail]) + 2 <= TAIL {
            size += count(parts[parts.len() - 1 - tail]) + 2;
            tail += 1;
        }
        if head >= 1 && tail >= 1 && head + tail < parts.len() {
            let hidden = parts.len() - head - tail;
            let text = format!("{}, … ({hidden} items more; {full}) …, {}", parts[..head].join(", "), parts[parts.len() - tail..].join(", "));
            old = Some((text, (ranges[head].0, ranges[parts.len() - tail - 1].1)));
        }
    }
    let old = old.unwrap_or_else(|| {
        let chars: Vec<char> = line.chars().collect();
        let start: String = chars[..HEAD].iter().collect();
        let end: String = chars[chars.len() - TAIL..].iter().collect();
        (format!("{start}… ({} characters more; {full}) …{end}", chars.len() - HEAD - TAIL), (start.len(), line.len() - end.len()))
    });
    if !touches(old.1.0, old.1.1) {
        return old.0;
    }
    // The longest run of items with a test: the cut hides items in its middle.
    let list_cut = |keep: &dyn Fn(usize) -> bool| -> Option<String> {
        let mut best: Option<(usize, usize)> = None;
        let mut index = 0;
        while index < parts.len() {
            if !keep(index) {
                index += 1;
                continue;
            }
            let mut end = index;
            while end < parts.len() && keep(end) {
                end += 1;
            }
            if best.is_none_or(|(a, b)| end - index > b - a) {
                best = Some((index, end));
            }
            index = end;
        }
        let (start, end) = best.filter(|(start, end)| end - start >= 3)?;
        // Keep the first items of the run and its last item; hide the fewest items.
        (1..end - start - 1).rev().map(|kept| (kept, end - start - 1 - kept)).map(|(kept, hidden)| format!("{}, … ({hidden} items more; {full}) …, {}", parts[..start + kept].join(", "), parts[end - 1..].join(", "))).find(|text| count(text) <= LINE_MAX)
    };
    let code = |index: usize| {
        let part = parts[index].trim_end_matches([')', ';', '.']);
        part.len() >= 2 && part.starts_with('`') && part.ends_with('`')
    };
    let free = |index: usize| !touches(ranges[index].0, ranges[index].1);
    if let Some(text) = list_cut(&|index| code(index) && free(index)) {
        return text;
    }
    if let Some(text) = list_cut(&free) {
        return text;
    }
    // The longest text with no result and no cut mark: the cut hides characters in its middle,
    // one text after another.
    let marker = |hidden: usize| format!("… ({hidden} characters more; {full}) …");
    let mut text = line.to_string();
    for _ in 0..4 {
        if count(&text) <= LINE_MAX {
            return text;
        }
        let mut kept = result_ranges(&text);
        let mut search = 0;
        while let Some(offset) = text[search..].find(full) {
            let at = search + offset;
            let from = text[..at].rfind("… (").unwrap_or(at);
            let to = text[at..].find(") …").map_or(at + full.len(), |end| at + end + ") …".len());
            kept.push((from, to));
            search = to;
        }
        kept.sort();
        let mut gaps: Vec<(usize, usize)> = Vec::new();
        let mut from = 0;
        for (a, b) in kept {
            if a > from {
                gaps.push((from, a));
            }
            from = from.max(b);
        }
        if from < text.len() {
            gaps.push((from, text.len()));
        }
        let Some((a, b)) = gaps.into_iter().max_by_key(|(a, b)| count(&text[*a..*b])) else { break };
        let gap: Vec<usize> = text[a..b].char_indices().map(|(offset, _)| a + offset).collect();
        let need = count(&text) - LINE_MAX + count(&marker(count(&text)));
        // Keep 10 characters at each end of the text.
        let Some(room) = gap.len().checked_sub(20).filter(|room| *room > count(&marker(gap.len()))) else { break };
        let hidden = need.min(room);
        let first = (gap.len() - hidden) / 2;
        let (cut_from, cut_to) = (gap[first], gap.get(first + hidden).copied().unwrap_or(b));
        text = format!("{}{}{}", &text[..cut_from], marker(count(&text[cut_from..cut_to])), &text[cut_to..]);
    }
    if count(&text) <= LINE_MAX {
        return text;
    }
    old.0
}

/// The byte ranges of the results of a check in a line (R66.10-6): a result word, "alone", and
/// the list of its configurations after " in " (to the start of the cause: ": ", "; ", or " (",
/// but not " (libraries and binaries only)").
fn result_ranges(line: &str) -> Vec<(usize, usize)> {
    // The mark of a configuration that builds only the libraries and the binaries.
    let only = semilla_core::report::ConfigNote::NARROW;
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    for word in crate::equiv::result_words() {
        let mut search = 0;
        while let Some(offset) = line[search..].find(word) {
            let start = search + offset;
            search = start + word.len();
            if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
                continue;
            }
            let mut end = start + word.len();
            if line[end..].starts_with(" alone") {
                end += " alone".len();
            }
            let rest = &line[end..];
            let ends = rest.is_empty() || [" in ", ": ", "; ", " (", ")", ",", ";"].iter().any(|next| rest.starts_with(next));
            if !ends {
                continue;
            }
            if rest.starts_with(" in ") {
                end += " in ".len();
                while end < line.len() {
                    let rest = &line[end..];
                    if rest.starts_with(only) {
                        end += only.len();
                    } else if rest.starts_with(": ") || rest.starts_with("; ") || rest.starts_with(" (") || rest.starts_with(" …") {
                        break;
                    } else {
                        end += rest.chars().next().map_or(1, char::len_utf8);
                    }
                }
            }
            found.push((start, end));
        }
    }
    found
}

/// The facts of one step of the text report (design 37.13 to 37.18).
/// - The order of the facts follows [`rank`]. A fact keeps its number.
/// - Facts with the same text, the same category, the same compare result, and the same lines
///   below are one line with the count, the numbers, and the places.
/// Returns the build code section (it comes before the claims) and the other facts. Each
/// other entry has the place of its lines (design 54.5) and the lines: the index of the fact
/// of the agent that the fact is below, or the group of a fact that Semilla filled in.
fn fact_list(step: &StepReport, names: bool, noted: &mut bool, full: bool) -> (String, Vec<Block>) {
    use text::put_line as put;
    let facts = &step.facts;
    let mut first = String::new();
    let mut rest: Vec<Block> = Vec::new();
    // Design 55.3: the group of a fact in a step with tags is its first tag.
    let tag_of = |fact: &semilla_core::report::Fact| step.tag_group(fact);
    let out = &mut first;
    // The place of a fact: below the fact of the agent that states it, or in a group.
    // [R90.5-1] An entry of the map has its section in the one list (design 90), grouped by
    // item. A record of an earlier version keeps the groups of today.
    // A step with tags has the same order in each tag (property 4 of the case set: a tag changes
    // no fact line).
    let sections = |fact: &semilla_core::report::Fact| fact.section();
    let place_of = |fact: &semilla_core::report::Fact| match (sections(fact), step.agent_facts.iter().position(|agent| agent.facts.contains(&fact.id))) {
        (Some(section), _) => Err(section),
        (None, Some(agent)) => Ok(agent),
        (None, None) => Err(fact.filled_group()),
    };
    let mut order: Vec<&semilla_core::report::Fact> = facts.iter().collect();
    order.sort_by_key(|fact| (place_of(fact).err().unwrap_or(0), sections(fact).map(|_| entry_item(fact)), rank(fact)));
    // Design 38.4: a spotlight line that is equal for a run of facts shows one time, after the
    // last fact of the run, with the fact numbers. The texts of such a run do not share.
    let runs = spotlight_runs(&order, names, &|fact| place_of(fact));
    let alone: Vec<&semilla_core::report::Fact> = order.iter().enumerate().filter(|(index, _)| runs[*index].is_none()).map(|(_, fact)| *fact).collect();
    let mut shared = Shared::new(&alone, names, step.step);
    shared.flow = step.flow.as_ref();
    shared.report = Some(step);
    shared.after_commit = step.after_commit;
    shared.step = (!full).then_some(step.step);
    // Design 28.3: the build code changes of the step come first, with the diff. The manifest
    // facts of the change are with the other manifest facts: one line names them here.
    if facts.iter().any(|fact| fact.build_code_change) {
        put(out, &format!("  ! {}", semilla_core::report::BUILD_CODE_SECTION));
        let mut files: Vec<(&str, Vec<&str>)> = Vec::new();
        for fact in order.iter().filter(|fact| fact.build_code_change && rank(fact) != 0) {
            let file = fact.file.as_deref().unwrap_or("");
            match files.iter_mut().find(|(name, _)| *name == file) {
                Some((_, ids)) => ids.push(&fact.id),
                None => files.push((file, vec![&fact.id])),
            }
        }
        for (file, ids) in files {
            put(out, &format!("      `{file}`: the manifest facts {} below, with the diff of the file", id_list(&in_order(&ids))));
        }
    }
    struct Line<'a> {
        ids: Vec<&'a str>,
        places: Vec<String>,
        rank: u8,
        head: String,
        lines: Vec<String>,
        /// The full texts (design 38.1): facts with a different content stay apart.
        content: Option<&'a str>,
        /// The fact of the agent that the facts are below. Facts below two facts stay apart.
        agent: Option<usize>,
        place: Result<usize, usize>,
        /// The first tag of the facts (design 55.3). Facts of different tags stay apart.
        tag: TagGroup,
        /// The positions of the facts (design 55.2).
        positions: Vec<semilla_core::Position>,
        /// The head and the lines with the full-text command of the fact as a mark, for the
        /// compare of two facts (`command_key`).
        keys: Vec<String>,
        /// Each line before its cut (R76.1-1), for the lines shown one time (R71.1-2).
        raws: Vec<String>,
    }
    let mut shown: Vec<Line> = Vec::new();
    for (index, fact) in order.iter().enumerate() {
        let fact = *fact;
        let (head, mut lines, mut raws) = fact_lines(fact, &shared, names);
        // R76.1-1 (design 38.1): the merge compares the full lines of the two facts (each list in
        // full, no cut), with the full-text command of the fact as a mark: a cut or a count of a
        // list never makes two different facts look equal, and the command does not keep two
        // equal facts apart.
        let (_, mut full_lines, _) = if names { (String::new(), lines.clone(), Vec::new()) } else { fact_lines(fact, &shared, true) };
        if let Some((start, end)) = runs[index] {
            let ids: Vec<&str> = order[start..=end].iter().map(|fact| fact.id.as_str()).collect();
            if index == end {
                for lines in [&mut lines, &mut raws, &mut full_lines] {
                    for line in lines.iter_mut().filter(|line| is_spotlight_line(line)) {
                        *line = line.replacen("spotlight: ", &format!("spotlight of {}: ", id_list(&ids)), 1);
                    }
                }
            } else {
                let keep = |line: &String| !is_spotlight_line(line) && !line.starts_with("      each cause: ");
                let kept: Vec<bool> = raws.iter().map(keep).collect();
                let mut at = kept.iter();
                lines.retain(|_| *at.next().unwrap_or(&true));
                raws.retain(keep);
                full_lines.retain(keep);
            }
        }
        let place = fact.at.as_ref().map(|at| at.to_string()).or_else(|| fact.file.clone()).unwrap_or_else(|| fact.part.clone());
        let agent = place_of(fact).ok();
        let tag = tag_of(fact);
        let own = full_text_command(&fact.id, step.step);
        let keys: Vec<String> = std::iter::once(&head).chain(full_lines.iter()).map(|text| command_key(text, &own)).collect();
        match shown.iter_mut().find(|line| line.rank == rank(fact) && line.keys == keys && line.content == fact.content.as_deref() && line.agent == agent && line.tag == tag) {
            Some(line) => {
                line.ids.push(&fact.id);
                line.places.push(place);
                line.positions.extend(fact.positions.iter().cloned());
            }
            None => shown.push(Line { ids: vec![&fact.id], places: vec![place], rank: rank(fact), head, lines, content: fact.content.as_deref(), agent, place: place_of(fact), tag, positions: fact.positions.clone(), keys, raws }),
        }
    }
    // Design 38.5: no line of the text output is longer than `LINE_MAX` characters. The merge
    // above compares the full lines, so a cut never makes two different facts look equal.
    // Design 54.7: the cut applies only to a list of places. A fact line and the lines below
    // it show the full text.
    let fit = |line: String, id: &str| if names { line } else { fit_line(&line, &full_text_command(id, step.step)) };
    // Design 58.6: a comment of a changed item shows one time, with the first fact in the
    // order of the output that has it. The order of the output: the build code section, the
    // facts below each fact of the agent, then the groups (in a step with tags: the tags).
    // The lines of the comments are not a part of the compare of two fact lines above.
    let tagged = !step.tags.is_empty();
    let output_key = |line: &Line| match (line.rank, tagged, line.tag, line.place) {
        (0, ..) => (0, 0),
        (_, true, TagGroup::Tag(tag), _) => (1, tag),
        (_, true, TagGroup::Outside, _) => (2, 0),
        (_, true, TagGroup::NoPosition, _) => (3, 0),
        (_, false, _, Ok(agent)) => (1, agent),
        (_, false, _, Err(group)) => (2, group),
    };
    let mut output: Vec<usize> = (0..shown.len()).collect();
    output.sort_by_key(|index| output_key(&shown[*index]));
    // R71.1-2: a line below a fact (not a line of a block, not a spotlight line, at least
    // `SHARE_MIN` characters) that two fact lines or more have shows one time, at the first
    // fact line in the order of the output, with the numbers of the other facts. Each other fact
    // line names that fact. A full text (`semilla expand fN`) keeps each line.
    if !full {
        let shareable = |line: &str| line.starts_with("      ") && !line.starts_with("        ") && line.chars().count() >= SHARE_MIN && !is_spotlight_line(line) && !line.contains(SAME_LINE_AS);
        // The key of a line: the line before its cut, with the command of its fact as a mark
        // (R76.1-1). Two facts with numbers of a different length cut one line at other places.
        let key = |index: usize, raw: &str| command_key(raw, &full_text_command(shown[index].ids[0], step.step));
        let mut holders: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for index in &output {
            for (_, raw) in shown[*index].lines.iter().zip(&shown[*index].raws).filter(|(line, _)| shareable(line)) {
                let list = holders.entry(key(*index, raw)).or_default();
                if !list.contains(index) {
                    list.push(*index);
                }
            }
        }
        holders.retain(|_, list| list.len() > 1);
        for (text, list) in &holders {
            let first = shown[list[0]].ids[0].to_string();
            let others: Vec<&str> = list[1..].iter().flat_map(|index| shown[*index].ids.iter().copied()).collect();
            let kind = text.trim_start().split(": ").next().unwrap_or("").to_string();
            for (place, index) in list.iter().enumerate() {
                let own = full_text_command(shown[*index].ids[0], step.step);
                let entry = &mut shown[*index];
                for (line, raw) in entry.lines.iter_mut().zip(&entry.raws) {
                    if command_key(raw, &own) == *text && shareable(line) {
                        *line = if place == 0 { format!("{line} (the same line for {})", id_list(&in_order(&others))) } else { format!("      {kind}: {SAME_LINE_AS}{first}") };
                    }
                }
            }
        }
    }
    let in_output: Vec<&semilla_core::report::Fact> = output.iter().flat_map(|index| shown[*index].ids.iter()).filter_map(|id| facts.iter().find(|fact| fact.id == *id)).collect();
    let mut of_fact = semilla_core::report::comments_shown(&in_output).into_iter();
    let mut comment_lines: Vec<Vec<String>> = vec![Vec::new(); shown.len()];
    for index in &output {
        let mut no_edit = false;
        for _ in &shown[*index].ids {
            for comment in of_fact.next().unwrap_or_default() {
                // R71.1-1: a comment text over the budget with no edit shows its first lines.
                let command = (!full).then(|| full_text_command(shown[*index].ids[0], step.step));
                comment_lines[*index].extend(comment.lines_with(command.as_deref()).into_iter().map(|line| format!("      {line}")));
                no_edit |= comment.state == semilla_core::report::CommentState::NoEdit;
            }
        }
        // Design 58.4: the note shows one time, below the comments of the first fact line
        // that has a comment with no edit.
        if no_edit && !*noted {
            *noted = true;
            comment_lines[*index].push(format!("      note: {}", semilla_core::report::COMMENT_NOTE));
        }
    }
    for (line, comment_lines) in shown.into_iter().zip(comment_lines) {
        let mut block = String::new();
        let out = if line.rank == 0 { &mut first } else { &mut block };
        if line.ids.len() == 1 {
            put(out, &format!("  ◆ {} {}", line.ids[0], line.head));
        } else {
            put(out, &format!("  ◆ {} {} × {} (also {})", line.ids[0], line.head, line.ids.len(), id_list(&line.ids[1..])));
            put(out, &fit(format!("      places: {}", line.places.join(", ")), line.ids[0]));
        }
        for text in &line.lines {
            put(out, text);
        }
        for text in &comment_lines {
            put(out, text);
        }
        // Design 55.2: `status --verbose` shows the lines of each fact, in the form of a
        // position of a tag. It is the last line of the fact: each other line keeps its place.
        if names && !line.positions.is_empty() {
            put(out, &format!("      lines: {}", semilla_core::Position::list(&semilla_core::Position::normal(line.positions.clone()))));
        }
        if line.rank != 0 {
            let item = facts.iter().find(|fact| fact.id == line.ids[0]).map(entry_item).unwrap_or_default();
            rest.push(Block { place: line.place, tag: line.tag, text: block, item });
        }
    }
    (first, rest)
}

/// [R90.5-1] The item of an entry for the groups of the one list: the item of its place, else
/// its file.
fn entry_item(fact: &semilla_core::report::Fact) -> String {
    match (&fact.at, &fact.file) {
        (Some(at), _) => format!("item `{}`", at.item),
        (None, Some(file)) => format!("file `{file}`"),
        (None, None) => "the step".to_string(),
    }
}

/// The lines of one fact line of the text report, with the place of the block in the output.
struct Block {
    /// The fact of the agent that the facts are below, or the group of the filled facts.
    place: Result<usize, usize>,
    /// The first tag of the facts (design 55.3).
    tag: TagGroup,
    text: String,
    /// [R90.5-1] The item of the first fact, for the groups of the one list.
    item: String,
}

/// The lines of the tags of one step (design 55.3): each tag with its facts, in the order of
/// the agent, then the group of the facts in no tag, then the facts with no position.
/// `blocks` gives the lines of the facts of one group. A step with no tag gives no line.
pub fn tag_lines(step: &StepReport, blocks: &dyn Fn(TagGroup) -> Vec<String>) -> Vec<String> {
    let mut lines = Vec::new();
    if step.tags.is_empty() {
        return lines;
    }
    lines.push("  tags of the agent (a tag is a name for a part of the step; it is not a fact, and Semilla does not check the name):".to_string());
    for (index, tag) in step.tags.iter().enumerate() {
        if let Some(error) = &tag.error {
            lines.push(format!("  ✗ tag: {error}"));
            continue;
        }
        lines.push(format!("  {}", step.tag_head(tag)));
        lines.extend(blocks(TagGroup::Tag(index)));
        // A fact of two tags shows in its first tag (design 55.3).
        for (ids, place) in step.shown_in_other_tag(index).iter().zip(["above", "below"]).filter(|(ids, _)| !ids.is_empty()) {
            lines.push(format!("      also in this tag, shown in a tag {place}: {}", ids.join(", ")));
        }
        // Design 55.3, rule 5: a fact that is in the tag in part, with its other lines.
        lines.extend(tag.notes.iter().map(|note| format!("      {note}")));
    }
    let outside = step.not_in_a_tag();
    if !outside.is_empty() {
        lines.push(format!("  not in a tag: {}; {} changed line(s)", step.count_text(&outside), step.line_count(&outside)));
        lines.extend(blocks(TagGroup::Outside));
    }
    // Design 55.3, rule 6: a fact with no position is not a part of the count above.
    let unplaced = step.no_position();
    if !unplaced.is_empty() {
        lines.push(format!("  facts with no position (a tag selects changed lines; these facts have none): {}", step.count_text(&unplaced)));
        lines.extend(blocks(TagGroup::NoPosition));
    }
    lines
}

/// The lines of the facts of the agent and of the gap list of one step (design 54.5, parts 1
/// and 2). `below` gives the lines of the facts that are below one fact of the agent. With
/// `limit`, the gap list shows its first lines and a count.
pub fn agent_lines(step: &StepReport, limit: Option<usize>, below: &dyn Fn(usize) -> Vec<String>) -> Vec<String> {
    let mut lines = Vec::new();
    if !step.agent_facts.is_empty() {
        lines.push(format!("  facts of the agent ({}):", semilla_core::report::AGENT_FACTS_NOTE));
    }
    for (index, fact) in step.agent_facts.iter().enumerate() {
        lines.push(format!("  {} {}", if fact.holds { "✓" } else { "✗" }, fact.line()));
        lines.extend(fact.lines().iter().map(|line| format!("      {line}")));
        lines.extend(below(index));
    }
    // Design 67, part D: the claims of the intent files.
    lines.extend(crate::intent::lines(&step.intent_claims));
    // [R77.1-2] The section "semantic changes" (design 77).
    lines.extend(crate::intent::semantic_lines(step, limit.map(|most| most * 2)));
    // [R90.5-3] The coverage line of the map, then (R90.6-1) the request of each node that is
    // not mapped (a step with no intent file: the section of the claims has none), then the
    // gap list.
    let mut gap = step.gap_lines(limit).into_iter();
    lines.extend(gap.next());
    if step.intent_claims.is_empty() && limit.is_some() {
        if let Some(map) = &step.ast {
            lines.extend(crate::astmap::lines(map, limit, step.step).into_iter().map(|line| line.replacen("    ", "  ", 1)));
        }
    }
    lines.extend(gap);
    if let Some(command) = step.gap_command(limit) {
        lines.push(format!("  {command}"));
    }
    lines
}

/// [R92.5-1] The line "not stated" of each entry of a step, with the number of the entry: for an
/// output with no list of the entries (`semilla fact`, a tag after the commit), so that no value
/// of the gap list is lost (design 92).
pub fn not_stated_lines(step: &StepReport) -> Vec<String> {
    step.output_order().into_iter().filter_map(|fact| step.not_stated_line(fact).map(|line| format!("  {}: {line}", fact.id))).collect()
}

/// The text report. With `short_failures`, a claim that is not verified shows only its status
/// and a short cause: the caller prints the full text and the hint before the report.
pub fn text_report_with(steps: &[StepReport], short_failures: bool) -> String {
    text_report_lists(steps, short_failures, false, false)
}

/// The full text of one fact of a step (R71.1-1, the command `semilla expand fN`): the fact
/// line, each line below it, and its comments, with each text in full (no budget). `None` if
/// the step has no fact with the number.
pub fn fact_full_text(step: &StepReport, id: &str) -> Option<String> {
    let fact = step.facts.iter().find(|fact| fact.id == id)?;
    let mut one = step.clone();
    one.facts = vec![fact.clone()];
    one.agent_facts.clear();
    one.tags.clear();
    let mut noted = true;
    let (first, rest) = fact_list(&one, true, &mut noted, true);
    let mut out = first;
    for block in rest {
        out.push_str(&block.text);
    }
    Some(out)
}

/// The full text of one step (R76.1-1, the command `semilla expand --step S`): each line of the
/// step with no cut, each list in full, and each text in full (no budget).
pub fn step_full_text(step: &StepReport) -> String {
    text_report_lists(std::slice::from_ref(step), false, true, true)
}

/// The text report. `names` shows each name of a long list of bodies (batch 5x, part E).
fn text_report_lists(steps: &[StepReport], short_failures: bool, names: bool, full: bool) -> String {
    // Each line of the report can hold text of the agent or of the repository: `put` shows
    // each hidden character of a line as an escape, so no text starts a new line (5k, A2).
    // Design 38.5: each line has at most `LINE_MAX` characters (`status --verbose`: each
    // full line).
    let mut out = String::new();
    // Design 58.4: the note about the comments shows one time in a report.
    let mut noted = false;
    for step in steps {
        // R76.1-1: a cut line of the step names the command of the step.
        let command = semilla_core::budget::step_text_command(step.step);
        let put = |out: &mut String, line: &str| text::put_line(out, &if names { line.to_string() } else { fit_line(line, &command) });
        let start = out.len();
        put(&mut out, &format!("step {}  {}", step.step, text::quoted(&step.intent)));
        let (build_code, facts) = fact_list(step, names, &mut noted, full);
        out.push_str(&build_code);
        for claim in &step.claims {
            let mark = match &claim.status {
                ClaimStatus::Verified { .. } => "✓",
                ClaimStatus::Pending { .. } => "…",
                ClaimStatus::NotChecked { .. } => "–",
                _ => "✗",
            };
            if short_failures && !claim.status.is_verified() {
                put(&mut out, &format!("  {mark} {} {}", claim.claim, short_cause(&claim.status)));
                continue;
            }
            let detail = claim.status.text();
            let class = claim.category.map(|c| format!(" [{}]", c.name())).unwrap_or_default();
            let effect = claim.effect.map(|e| format!(" effect hint: {e:?}").to_lowercase()).unwrap_or_default();
            let witness = claim.witness.as_ref().map(|w| format!(" (example: {w})")).unwrap_or_default();
            put(&mut out, &format!("  {mark} {} {detail}{}{class}{effect}{witness}", claim.claim, claim.mark()));
            if !claim.affected.is_empty() {
                put(&mut out, &format!("      changed bodies: {}", body_list(&claim.affected, names, &command)));
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
        // Design 54.5, the order of the result: the facts of the agent with the facts below
        // them, the gap list, then the facts that Semilla filled in, in groups.
        // Design 55.3: in a step with tags, each fact shows in the group of its tag. The fact
        // of the agent names its facts.
        let tagged = !step.tags.is_empty();
        // [R90.5-1] A fact of the agent is the source of its entries: the entries are in the one
        // list below, and the fact names them.
        let sectioned = step.facts.iter().any(|fact| fact.section().is_some());
        let below = |agent: usize| -> Vec<String> {
            if tagged {
                let ids = &step.agent_facts[agent].facts;
                return if ids.is_empty() { Vec::new() } else { vec![format!("      facts, in the groups of the tags below: {}", ids.join(", "))] };
            }
            if sectioned {
                let ids = &step.agent_facts[agent].facts;
                return if ids.is_empty() { Vec::new() } else { vec![format!("      entries, in the list below: {}", ids.join(", "))] };
            }
            facts.iter().filter(|block| block.place == Ok(agent)).flat_map(|block| block.text.lines().map(|line| format!("  {line}"))).collect()
        };
        for line in agent_lines(step, if names { None } else { Some(semilla_core::report::GAP_LIMIT) }, &below) {
            // Design 54.7: a line with a fact text has no cut.
            text::put_line(&mut out, &line);
        }
        // [R90.5-1] The one list of the entries (design 90): the real edits first, grouped by
        // item. A record of an earlier version keeps the groups of today.
        let groups = if sectioned { semilla_core::report::ENTRY_SECTIONS } else { semilla_core::report::FILLED_GROUPS };
        for (group, name) in groups.iter().enumerate().filter(|_| !tagged) {
            let blocks: Vec<&Block> = facts.iter().filter(|block| block.place == Err(group)).collect();
            if blocks.is_empty() {
                continue;
            }
            if !sectioned {
                put(&mut out, &format!("  {name}, filled in by Semilla:"));
                blocks.iter().for_each(|block| out.push_str(&block.text));
                continue;
            }
            put(&mut out, &format!("  {name}:"));
            let mut last = "";
            for block in blocks {
                if block.item != last {
                    put(&mut out, &format!("  {}:", block.item));
                    last = &block.item;
                }
                out.push_str(&block.text);
            }
        }
        // Design 55.3: the facts in the groups of the tags. Each fact line is the line of a
        // step with no tag.
        let of_tag = |tag: TagGroup| -> Vec<String> { facts.iter().filter(|block| block.tag == tag).flat_map(|block| block.text.lines().map(str::to_string)).collect() };
        for line in tag_lines(step, &of_tag) {
            text::put_line(&mut out, &line);
        }
        // Design 55.4: the cover check of the step.
        // The list of the lines with no fact has no cut.
        for line in step.cover_lines() {
            text::put_line(&mut out, &format!("  {line}"));
        }
        // Design 56.4: the cause of each line with no fact, in the full text only.
        if names {
            for line in step.cause_lines() {
                text::put_line(&mut out, &format!("  {line}"));
            }
            // Design 59, B-e: the lines of the rule of the equal tokens.
            for line in step.equal_token_lines() {
                text::put_line(&mut out, &format!("  {line}"));
            }
        }
        // Design 60, C2 and C3: the result of the graph of each body; `status --verbose` adds
        // the full diff.
        for line in step.flow_lines(names) {
            put(&mut out, &format!("  {line}"));
        }
        // Design 63: the changed expansions of the macro calls.
        for line in step.expansion_section(names) {
            put(&mut out, &format!("  {line}"));
        }
        // Design 55.1: the command that gives the hunks of the step.
        for line in step.hunk_lines(names) {
            text::put_line(&mut out, &format!("  {line}"));
        }
        // Design 58.8: the count of the comments of the changed items that have no edit.
        if let Some(line) = step.comment_count_line() {
            text::put_line(&mut out, &format!("  {line}"));
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
        if out[start..].contains(text::CHILD_MARK) {
            put(&mut out, &format!("  note: {}", text::CHILD_NOTE));
        }
        out.push('\n');
    }
    out
}
