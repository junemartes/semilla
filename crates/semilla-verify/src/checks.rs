//! The claim checkers.
//!
//! Each checker selects the edits that its claim explains, and checks their shape. Three methods
//! exist (design section 8.5):
//! - **Predict:** the claim gives the expected result (renames). The diff applies it first.
//! - **Match the shape:** the edit at the anchor must have the shape of the claim.
//! - **Mask:** a free claim explains all edits inside its block-free subtree.
//!
//! The class of a claim comes from the rule table, or from the MIR when the table says
//! "computed". A neutral claim needs a body proof.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::ops::{self, OpKind, OpLevel};
use semilla_core::report::Effect;
use semilla_core::{Anchor, Class, Claim, ClaimStatus, Segment, Step};
use semilla_syntax::{Edit, EditKind, Node, Renames, Resolved, Workspace};

use crate::{BodyComparison, BodyStatus, ExprOracle};

/// The data that all checkers share.
pub struct Context<'a> {
    pub step: &'a Step,
    pub before: &'a Workspace,
    pub after: &'a Workspace,
    pub edits: &'a [Edit],
    pub bodies: &'a BodyIndex,
    pub exprs: &'a dyn ExprOracle,
    pub renames: &'a Renames,
    /// The local calls of each item after the step (from the syntax), for the property claims.
    pub calls: &'a BTreeMap<String, BTreeSet<String>>,
    /// The MIR facts of each item after the step, or `None` before phase 2.
    pub facts: Option<&'a BTreeMap<String, crate::ItemFacts>>,
}

/// The result of one checker.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub status: ClaimStatus,
    /// The indexes of the edits that the claim explains.
    pub explains: Vec<usize>,
    pub class: Option<Class>,
    pub effect: Option<Effect>,
    pub witness: Option<String>,
    pub hint: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    /// True if the claim needs a body proof of its whole function (neutral claims).
    pub needs_body_proof: bool,
    /// True if the claim is valid without an edit (a rename that the diff applied first).
    pub self_evident: bool,
}

impl Outcome {
    fn new(status: ClaimStatus) -> Outcome {
        Outcome { status, explains: Vec::new(), class: None, effect: None, witness: None, hint: None, before: None, after: None, needs_body_proof: false, self_evident: false }
    }

    fn malformed(reason: impl Into<String>) -> Outcome {
        Outcome::new(ClaimStatus::Malformed { reason: reason.into() })
    }

    fn verified(proof: &str, explains: Vec<usize>) -> Outcome {
        let mut outcome = Outcome::new(ClaimStatus::Verified { proof: proof.to_string() });
        outcome.explains = explains;
        outcome
    }

    fn refuted(reason: impl Into<String>) -> Outcome {
        Outcome::new(ClaimStatus::Refuted { reason: reason.into() })
    }
}

/// Finds the MIR bodies of each item.
pub struct BodyIndex {
    /// The item path → the statuses of its bodies (the function and its closures).
    by_item: BTreeMap<String, Vec<BodyStatus>>,
    /// The MIR name → the item path.
    items: BTreeMap<String, String>,
    pub available: bool,
}

impl BodyIndex {
    pub fn new(bodies: &[BodyComparison], after: &Workspace, before: &Workspace) -> BodyIndex {
        let mut index = BodyIndex { by_item: BTreeMap::new(), items: BTreeMap::new(), available: !bodies.is_empty() };
        for body in bodies {
            let item = find_item(body, after).or_else(|| find_item(body, before));
            if let Some(item) = item {
                index.items.insert(body.name.clone(), item.clone());
                index.by_item.entry(item).or_default().push(body.status.clone());
            }
        }
        index
    }

    pub fn item_of(&self, body: &BodyComparison) -> Option<String> {
        self.items.get(&body.name).cloned()
    }

    /// The combined status of all bodies of an item: equal only if all are equal.
    pub fn status(&self, item: &str) -> Option<BodyStatus> {
        let statuses = self.by_item.get(item)?;
        let mut cfg_changed = false;
        let mut different = false;
        let mut level = "n0".to_string();
        let mut proof = Vec::new();
        for status in statuses {
            match status {
                BodyStatus::Equal { level: found, proof: rules } => {
                    if found > &level {
                        level = found.clone();
                    }
                    proof.extend(rules.iter().cloned());
                }
                BodyStatus::Different { cfg_changed: changed } => {
                    different = true;
                    cfg_changed |= changed;
                }
                BodyStatus::BeforeOnly | BodyStatus::AfterOnly => different = true,
            }
        }
        Some(if different { BodyStatus::Different { cfg_changed } } else { BodyStatus::Equal { level, proof } })
    }
}

/// Finds the syntax item of a MIR body: by the span first, then by the name.
fn find_item(body: &BodyComparison, workspace: &Workspace) -> Option<String> {
    find_item_by(&body.name, body.span.as_ref(), workspace)
}

/// Finds the syntax item of a MIR body name and span.
pub fn find_item_by(name: &str, span: Option<&(String, usize)>, workspace: &Workspace) -> Option<String> {
    if let Some((file, line)) = span {
        let found = workspace
            .items
            .values()
            .filter(|item| item.kind == semilla_syntax::ItemKind::Fn && (file.ends_with(&item.file) || item.file.ends_with(file.as_str())) && item.line_start <= *line && *line <= item.line_end)
            .min_by_key(|item| item.line_end - item.line_start);
        if let Some(item) = found {
            return Some(item.path.clone());
        }
    }
    let name = name.split("::{").next().unwrap_or(name);
    workspace.items.contains_key(name).then(|| name.to_string())
}

/// Builds the renames of a step from its identity claims.
pub fn renames(step: &Step, before: &Workspace) -> Renames {
    let mut renames = Renames::default();
    for claim in &step.claims {
        let Some(at) = &claim.at else { continue };
        match claim.op.as_str() {
            "rename" | "move-item" => {
                let Some(to) = claim.arg_str("to") else { continue };
                let new_path = full_path(&at.item, to);
                let old_name = last_segment(&at.item);
                let new_name = last_segment(&new_path);
                renames.items.insert(at.item.clone(), new_path.clone());
                // The methods of a renamed type follow the type.
                let prefix = format!("{}::", at.item);
                for path in before.items.keys().filter(|path| path.starts_with(&prefix)) {
                    renames.items.insert(path.clone(), format!("{new_path}::{}", &path[prefix.len()..]));
                }
                if old_name != new_name {
                    renames.idents.push((None, old_name.to_string(), new_name.to_string()));
                }
            }
            // A rewrite under a new path: the references follow the new path, like a rename.
            "rewrite-item" if claim.arg_str("new").is_some() => {
                let new_path = full_path(&at.item, claim.arg_str("new").unwrap());
                if new_path != at.item {
                    renames.items.insert(at.item.clone(), new_path.clone());
                    let (old_name, new_name) = (last_segment(&at.item), last_segment(&new_path));
                    if old_name != new_name {
                        renames.idents.push((None, old_name.to_string(), new_name.to_string()));
                    }
                }
            }
            "rename-local" => {
                let Some(to) = claim.arg_str("to") else { continue };
                if let Some(old) = local_name(before, at) {
                    renames.idents.push((Some(at.item.clone()), old, to.to_string()));
                }
            }
            _ => {}
        }
    }
    renames
}

/// The new items that a claim declares (they can exist on the "after" side only).
pub fn declared_new_items(claim: &Claim) -> Vec<String> {
    match claim.op.as_str() {
        "extract-function" => claim.arg_str("new").map(|new| vec![full_path(claim.at.as_ref().map(|a| a.item.as_str()).unwrap_or(""), new)]).unwrap_or_default(),
        "add-item" => claim.at.iter().map(|at| at.item.clone()).collect(),
        _ => Vec::new(),
    }
}

/// `to` can be a full path, or a new last segment for the same parent.
fn full_path(item: &str, to: &str) -> String {
    if to.contains("::") {
        to.to_string()
    } else {
        match item.rsplit_once("::") {
            Some((parent, _)) => format!("{parent}::{to}"),
            None => to.to_string(),
        }
    }
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// The variable name of a `let` statement (without `mut` and a type).
fn local_name(workspace: &Workspace, at: &Anchor) -> Option<String> {
    let node = semilla_syntax::resolve_node(workspace, at)?;
    let pattern = if node.kind == "let" { node.child("pat")? } else { node };
    pattern.text.split(' ').find(|token| *token != "mut" && *token != "ref" && token.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')).map(str::to_string)
}

/// Checks one claim.
pub fn check(context: &Context, claim: &Claim) -> Outcome {
    let Some(info) = ops::op(&claim.op) else {
        return Outcome::malformed(format!("unknown operation `{}`", claim.op));
    };
    if let Some(missing) = info.args.iter().find(|arg| !claim.args.contains_key(**arg)) {
        return Outcome::malformed(format!("`{}` needs the argument `{missing}`", claim.op));
    }
    // `rewrite-pattern` without an anchor applies to the whole workspace.
    if info.level != OpLevel::File && claim.at.is_none() && claim.op != "rewrite-pattern" {
        return Outcome::malformed(format!("`{}` needs an anchor (`at`)", claim.op));
    }

    if info.kind == OpKind::Property {
        let mut outcome = check_property(context, claim);
        outcome.self_evident = true;
        outcome.class = None;
        return outcome;
    }

    let mut outcome = match claim.op.as_str() {
        "rewrite-item" => check_rewrite(context, claim),
        "rename" | "move-item" => check_rename(context, claim),
        "add-item" => check_item_edit(context, claim, EditKind::ItemAdded, true),
        "remove-item" => check_item_edit(context, claim, EditKind::ItemRemoved, false),
        "change-signature" => check_attribute(context, claim, &[EditKind::Signature]),
        "change-visibility" => check_attribute(context, claim, &[EditKind::Visibility]),
        "edit-doc" => check_attribute(context, claim, &[EditKind::Docs]),
        "change-attribute" => check_attribute(context, claim, &[EditKind::Attrs]),
        "add-struct-field" => check_member(context, claim, "field"),
        "add-enum-variant" => check_member(context, claim, "variant"),
        "add-trait-impl" => check_trait_impl(context, claim),
        "extract-function" | "inline-function" => check_across(context, claim),
        "bump-dependency" | "edit-config" => check_file(context, claim),
        name if name.starts_with("add-") && info.level == OpLevel::Statement => check_add_statement(context, claim),
        "add-else" | "add-else-if" | "remove-else" | "remove-if" | "add-arm" | "remove-arm" | "move-arm" | "change-loop-kind" => check_compound(context, claim),
        "delete-statement" => check_statement_edit(context, claim, EditKind::Deleted),
        "move-statement" => check_move_statement(context, claim),
        "wrap-statements" => check_wrap(context, claim, true),
        "unwrap-statements" => check_wrap(context, claim, false),
        "rename-local" => check_rename_local(context, claim),
        "extract-variable" | "inline-variable" | "change-mutability" | "change-type-annotation" | "invert-if" | "swap-branches" | "if-to-match" | "match-to-if" | "guard-clause" | "reorder-statements" | "question-mark" => check_neutral_region(context, claim),
        "change-literal" => check_expression(context, claim, |b, a| b.node_before == Some("lit") && a.node_after == Some("lit")),
        "change-path" => check_expression(context, claim, |b, a| b.node_before == Some("path") && a.node_after == Some("path")),
        "change-operator" => check_operator(context, claim),
        "change-condition" => check_condition(context, claim),
        "negate" => check_expression(context, claim, |edit, _| {
            let (old, new) = (edit.text_before.as_deref().unwrap_or(""), edit.text_after.as_deref().unwrap_or(""));
            new.starts_with('!') && strip_paren(new.trim_start_matches('!').trim()) == strip_paren(old) || old.starts_with('!') && strip_paren(old.trim_start_matches('!').trim()) == strip_paren(new)
        }),
        "swap-operands" => check_swap(context, claim),
        "wrap-expression" | "chain-method" | "add-clone" => check_wrap_expression(context, claim, true),
        "unwrap-expression" => check_wrap_expression(context, claim, false),
        "add-argument" | "remove-argument" => check_argument(context, claim),
        "rewrite-pattern" => crate::pattern::check(context, claim),
        "replace-expression" | "replace-pattern" | "replace-type" => check_free(context, claim),
        "replace-macro-input" => check_macro_input(context, claim),
        "add-alternative" => check_expression(context, claim, |edit, _| edit.node_before == Some("pattern") && alternatives_added(edit.text_before.as_deref().unwrap_or(""), edit.text_after.as_deref().unwrap_or(""))),
        "remove-alternative" => check_expression(context, claim, |edit, _| edit.node_before == Some("pattern") && alternatives_added(edit.text_after.as_deref().unwrap_or(""), edit.text_before.as_deref().unwrap_or(""))),
        other => Outcome::malformed(format!("the verifier has no checker for `{other}`")),
    };

    if outcome.class.is_none() {
        outcome.class = info.class.or_else(|| computed_class(context, claim));
    }
    if info.class == Some(Class::Neutral) {
        outcome.needs_body_proof = true;
    }
    if info.kind == OpKind::Free && outcome.status.is_verified() && outcome.class == Some(Class::Neutral) && claim.op != "rewrite-item" {
        outcome.hint = Some("This free edit is neutral. An exact neutral op gives a stronger claim.".into());
    }
    outcome
}

/// The class from the MIR of the claim's function.
fn computed_class(context: &Context, claim: &Claim) -> Option<Class> {
    let item = claim.at.as_ref()?.item.as_str();
    match context.bodies.status(item)? {
        BodyStatus::Equal { .. } => Some(Class::Neutral),
        BodyStatus::Different { cfg_changed: true } => Some(Class::ControlFlow),
        _ => Some(Class::Logic),
    }
}

/// The body proof of a neutral claim on its function.
fn body_proof(context: &Context, item: &str, mut outcome: Outcome) -> Outcome {
    if !outcome.status.is_verified() {
        return outcome;
    }
    outcome.class = Some(Class::Neutral);
    match context.bodies.status(item) {
        Some(BodyStatus::Equal { level, proof }) => {
            let mut text = level.to_uppercase();
            if !proof.is_empty() {
                text = format!("{text}: {}", proof.join(", "));
            }
            outcome.status = ClaimStatus::Verified { proof: text };
        }
        // A changed control flow graph is not evidence of a behavior change (an extracted call
        // adds a block), so it gives "unproved", not "refuted".
        Some(BodyStatus::Different { cfg_changed }) => {
            let reason = if cfg_changed { "no normal form makes the bodies equal (the control flow graph differs)" } else { "no normal form makes the bodies equal" };
            outcome.status = ClaimStatus::Unproved { reason: reason.into() };
            outcome.hint = Some("Tag the change as logic, or use smaller neutral steps.".into());
        }
        Some(_) => {
            outcome.status = ClaimStatus::Unproved { reason: "no normal form makes the bodies equal".into() };
            outcome.hint = Some("Tag the change as logic, or use smaller neutral steps.".into());
        }
        None if !context.bodies.available => {
            outcome.status = ClaimStatus::Pending { reason: "the body proof runs in the background check (phase 2)".into() };
        }
        None => {
            // No body: a non-function item. The syntax check is the proof.
        }
    }
    outcome
}

fn texts(context: &Context, indexes: &[usize], mut outcome: Outcome) -> Outcome {
    if let Some(&first) = indexes.first() {
        let edit = &context.edits[first];
        // The source text is for the reviewer; the token text is the fallback.
        outcome.before = edit.source_before.clone().or_else(|| edit.text_before.clone());
        outcome.after = edit.source_after.clone().or_else(|| edit.text_after.clone());
    }
    outcome
}

/// The edits whose "before" anchor (or "after" anchor, when `after` is true) is inside `region`.
fn edits_inside(context: &Context, region: &Anchor, use_after: bool) -> Vec<usize> {
    context
        .edits
        .iter()
        .enumerate()
        .filter(|(_, edit)| {
            let anchor = if use_after { edit.after.as_ref().or(edit.before.as_ref()) } else { edit.before.as_ref().or(edit.after.as_ref()) };
            anchor.is_some_and(|anchor| region.contains(anchor))
        })
        .map(|(index, _)| index)
        .collect()
}

fn check_rename(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let to = full_path(&at.item, claim.arg_str("to").unwrap());
    if !context.before.items.contains_key(&at.item) {
        return Outcome::malformed(format!("`{}` does not exist before the step", at.item));
    }
    if !context.after.items.contains_key(&to) {
        return Outcome::malformed(format!("`{to}` does not exist after the step"));
    }
    // The diff applied the rename first. The claim also explains the `use` lines and the paths
    // that name the item.
    let (old_name, new_name) = (last_segment(&at.item), last_segment(&to));
    let explains: Vec<usize> = context
        .edits
        .iter()
        .enumerate()
        .filter(|(_, edit)| {
            let mentions = |text: &Option<String>, name: &str| text.as_deref().is_some_and(|text| text.split(' ').any(|token| token == name));
            let is_use = edit.attr_before.as_deref() == Some("use") || edit.attr_after.as_deref() == Some("use");
            let is_path = edit.kind == EditKind::Replaced && edit.node_before == Some("path") && edit.node_after == Some("path");
            (is_use || is_path) && (mentions(&edit.text_before, old_name) || mentions(&edit.text_after, new_name))
        })
        .map(|(index, _)| index)
        .collect();
    let mut outcome = Outcome::verified("N1", explains);
    outcome.self_evident = true;
    outcome.before = Some(at.item.clone());
    outcome.after = Some(to.clone());
    body_proof(context, &to, outcome)
}

fn check_rename_local(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let Some(old) = local_name(context.before, at) else {
        return Outcome::malformed("the anchor is not a `let` statement");
    };
    let to = claim.arg_str("to").unwrap();
    let before = context.before.items.get(&at.item).map(|item| item.text.clone()).unwrap_or_default();
    if context.renames.apply(&at.item, &before) == before {
        return Outcome::new(ClaimStatus::Empty);
    }
    let mut outcome = Outcome::verified("N0", Vec::new());
    outcome.self_evident = true;
    outcome.before = Some(old);
    outcome.after = Some(to.to_string());
    body_proof(context, &at.item, outcome)
}

fn check_item_edit(context: &Context, claim: &Claim, kind: EditKind, added: bool) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let explains: Vec<usize> = context
        .edits
        .iter()
        .enumerate()
        .filter(|(_, edit)| {
            let item = if added { edit.after.as_ref() } else { edit.before.as_ref() };
            let own = edit.kind == kind && item.is_some_and(|item| item.item == at.item || item.item.starts_with(&format!("{}::", at.item)));
            // A new method changes the member list of its impl.
            let impl_members = edit.kind == EditKind::Members && at.item.contains("::") && {
                let owner = at.item.rsplit_once("::").map(|(owner, _)| owner).unwrap_or("");
                edit.anchor().is_some_and(|anchor| anchor.item.ends_with(&format!("impl {}", last_segment(owner))) || anchor.item.contains(&format!("impl {owner}")))
            };
            own || impl_members
        })
        .map(|(index, _)| index)
        .collect();
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    // The text of the item itself comes first, before the member list of its impl.
    let mut ordered = explains.clone();
    ordered.sort_by_key(|&i| context.edits[i].kind != kind);
    let outcome = Outcome::verified("shape", explains);
    let mut outcome = texts(context, &ordered, outcome);
    outcome.class = Some(Class::Logic);
    outcome
}

fn check_attribute(context: &Context, claim: &Claim, kinds: &[EditKind]) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let explains: Vec<usize> = context
        .edits
        .iter()
        .enumerate()
        .filter(|(_, edit)| kinds.contains(&edit.kind) && edit.anchor().is_some_and(|anchor| anchor.item == at.item))
        .map(|(index, _)| index)
        .collect();
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    let outcome = Outcome::verified("shape", explains.clone());
    texts(context, &explains, outcome)
}

fn check_member(context: &Context, claim: &Claim, arg: &str) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let name = claim.arg_str(arg).unwrap();
    let Some(index) = context.edits.iter().position(|edit| edit.kind == EditKind::Members && edit.anchor().is_some_and(|a| a.item == at.item)) else {
        return Outcome::new(ClaimStatus::Empty);
    };
    let edit = &context.edits[index];
    let old: Vec<&str> = edit.text_before.as_deref().unwrap_or("").lines().collect();
    let new: Vec<&str> = edit.text_after.as_deref().unwrap_or("").lines().collect();
    let added: Vec<&&str> = new.iter().filter(|member| !old.contains(member)).collect();
    let removed = old.iter().any(|member| !new.contains(member));
    if removed || added.len() != 1 || !added[0].split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| word == name) {
        return Outcome::refuted(format!("the change is not one new member `{name}`"));
    }
    texts(context, &[index], Outcome::verified("shape", vec![index]))
}

fn check_trait_impl(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let owner = at.item.split_once("impl ").map(|(module, owner)| (module.trim_end_matches("::"), owner));
    let explains: Vec<usize> = context
        .edits
        .iter()
        .enumerate()
        .filter(|(_, edit)| {
            edit.kind == EditKind::ItemAdded
                && edit.after.as_ref().is_some_and(|anchor| {
                    anchor.item == at.item || owner.is_some_and(|(module, owner)| anchor.item.starts_with(&format!("{module}::{owner}::")))
                })
        })
        .map(|(index, _)| index)
        .collect();
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    texts(context, &explains.clone(), Outcome::verified("shape", explains))
}

/// `extract-function` and `inline-function`: an item appears (or goes), and a region of the
/// function changes. The body proof shows that the function keeps its behavior.
fn check_across(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let count = claim.args.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let mut explains = Vec::new();
    if claim.op == "extract-function" {
        let new = full_path(&at.item, claim.arg_str("new").unwrap());
        match context.edits.iter().position(|edit| edit.kind == EditKind::ItemAdded && edit.after.as_ref().is_some_and(|a| a.item == new)) {
            Some(index) => explains.push(index),
            None => return Outcome::malformed(format!("the new function `{new}` does not exist after the step")),
        }
    } else if let Some(callee) = claim.arg_str("removes") {
        let callee = full_path(&at.item, callee);
        explains.extend(context.edits.iter().position(|edit| edit.kind == EditKind::ItemRemoved && edit.before.as_ref().is_some_and(|a| a.item == callee)));
    }
    let region = region_edits_with(context, at, count, false);
    if region.is_empty() {
        // The new item alone is not enough: the claim must also explain a change at its anchor.
        return Outcome::new(ClaimStatus::Empty);
    }
    explains.extend(region);
    let outcome = texts(context, &explains.clone(), Outcome::verified("shape", explains));
    body_proof(context, &at.item, outcome)
}

/// The edits in a statement region: `count` statements from the anchor, in the same block, on
/// the "before" side, and the statements next to them on the "after" side.
fn region_edits(context: &Context, at: &Anchor, count: usize) -> Vec<usize> {
    region_edits_with(context, at, count, true)
}

/// The edits in a statement region. With `slack`, the region also has the statement just before
/// the anchor (`extract-variable` inserts a `let` there).
fn region_edits_with(context: &Context, at: &Anchor, count: usize, slack: bool) -> Vec<usize> {
    let position = split_statement(at).or_else(|| tail_position(context.before, at));
    let Some((parent, first)) = position else {
        return edits_inside(context, at, false);
    };
    context
        .edits
        .iter()
        .enumerate()
        .filter(|(_, edit)| {
            let in_range = |anchor: Option<&Anchor>, extra: usize| {
                anchor.is_some_and(|anchor| {
                    anchor.item == at.item
                        && anchor.path.len() > parent.len()
                        && anchor.path.starts_with(&parent)
                        && match &anchor.path[parent.len()] {
                            Segment::Stmt(index) => *index + usize::from(slack) >= first && *index < first + count + extra,
                            Segment::Tail => true,
                            Segment::Role(_) => false,
                        }
                })
            };
            in_range(edit.before.as_ref(), 0) || in_range(edit.after.as_ref(), 1)
        })
        .map(|(index, _)| index)
        .collect()
}

/// For a `tail` anchor: the block path, and the index after the last statement.
fn tail_position(workspace: &Workspace, at: &Anchor) -> Option<(Vec<Segment>, usize)> {
    if at.path.last() != Some(&Segment::Tail) {
        return None;
    }
    let parent = at.path[..at.path.len() - 1].to_vec();
    let block = semilla_syntax::resolve_node(workspace, &Anchor { item: at.item.clone(), path: parent.clone() })?;
    let statements = block.children.iter().filter(|child| child.role.starts_with('S')).count();
    Some((parent, statements))
}

/// Splits a statement anchor into its block path and its statement index.
fn split_statement(at: &Anchor) -> Option<(Vec<Segment>, usize)> {
    match at.path.last()? {
        Segment::Stmt(index) => Some((at.path[..at.path.len() - 1].to_vec(), *index)),
        _ => None,
    }
}

fn check_file(context: &Context, claim: &Claim) -> Outcome {
    let file_edits = context.edits.iter().enumerate().filter(|(_, edit)| matches!(edit.kind, EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged));
    let explains: Vec<usize> = if claim.op == "edit-config" {
        let file = claim.arg_str("file").unwrap();
        file_edits.filter(|(_, edit)| edit.file.as_deref().is_some_and(|path| path == file || path.ends_with(&format!("/{file}")))).map(|(index, _)| index).collect()
    } else {
        let krate = claim.arg_str("crate").unwrap();
        let version = claim.arg_str("to").unwrap();
        let mut found = false;
        let explains = file_edits
            .filter(|(_, edit)| {
                let path = edit.file.as_deref().unwrap_or("");
                if path.ends_with("Cargo.lock") {
                    return true;
                }
                if !path.ends_with("Cargo.toml") {
                    return false;
                }
                let old = context.before.other_files.get(path).map(String::as_str).unwrap_or("");
                let new = context.after.other_files.get(path).map(String::as_str).unwrap_or("");
                let changed: Vec<&str> = new.lines().filter(|line| !old.lines().any(|o| o == *line)).chain(old.lines().filter(|line| !new.lines().any(|n| n == *line))).collect();
                let only_this = !changed.is_empty() && changed.iter().all(|line| line.contains(krate));
                found |= only_this && changed.iter().any(|line| line.contains(version));
                only_this
            })
            .map(|(index, _)| index)
            .collect();
        if !found {
            return Outcome::refuted(format!("no Cargo.toml line sets `{krate}` to `{version}`"));
        }
        explains
    };
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    Outcome::verified("shape", explains)
}

/// The node kinds that each `add-*` statement op creates.
fn add_kinds(op: &str) -> &'static [&'static str] {
    match op {
        "add-let" => &["let"],
        "add-if" | "add-early-exit" => &["if"],
        "add-match" => &["match"],
        "add-loop" => &["for", "while", "loop"],
        "add-return" => &["return"],
        "add-break" => &["break"],
        "add-continue" => &["continue"],
        "add-macro-statement" => &["macro"],
        _ => &[],
    }
}

/// `add-*` statement ops: one inserted statement at the anchor (an "after" anchor).
fn check_add_statement(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let Some(index) = context.edits.iter().position(|edit| edit.kind == EditKind::Inserted && edit.after.as_ref() == Some(at)) else {
        return match semilla_syntax::resolve_node(context.after, at) {
            Some(_) => Outcome::new(ClaimStatus::Empty),
            None => Outcome::malformed(format!("no statement at `{at}` after the step (`add-*` anchors use the new numbering)")),
        };
    };
    let edit = &context.edits[index];
    let kind = edit.node_after.unwrap_or("");
    let expected = add_kinds(&claim.op);
    let matches_kind = if expected.is_empty() {
        // `add-expression-statement`: any statement that is not a let, a macro, or block-like.
        !matches!(kind, "let" | "macro" | "item_stmt") && !edit.block_like
    } else {
        expected.contains(&kind)
    };
    if !matches_kind {
        return Outcome::refuted(format!("the new statement is a `{kind}`, not the shape of `{}`", claim.op));
    }
    let node = semilla_syntax::resolve_node(context.after, at);
    if claim.op == "add-early-exit" {
        let early = node.is_some_and(|node| {
            node.child("else").is_none()
                && node.child("then").is_some_and(|then| then.children.len() == 1 && matches!(then.children[0].kind, "return" | "break" | "continue"))
        });
        if !early {
            return Outcome::refuted("the new `if` is not an early exit (`if cond { return/break/continue }` without else)");
        }
    }
    if let (Some(cond), Some(node)) = (claim.arg_str("cond"), node) {
        let actual = node.child("cond").map(|c| c.text.clone()).unwrap_or_default();
        if normalize_expr(cond) != Some(actual.clone()) {
            return Outcome::refuted(format!("the condition is `{actual}`, not `{cond}`"));
        }
    }
    texts(context, &[index], Outcome::verified("shape", vec![index]))
}

/// Changes to the branches of an `if` or a `match`, or to the kind of a loop.
fn check_compound(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let before = semilla_syntax::resolve_node(context.before, at);
    let Some(before) = before else {
        return Outcome::malformed(format!("no statement at `{at}` before the step"));
    };
    let explains = edits_inside(context, at, false);
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    let after = context.edits[explains[0]].after.as_ref().and_then(|a| semilla_syntax::resolve_node(context.after, a.parent().as_ref().filter(|_| a.path.len() > at.path.len()).unwrap_or(a)));
    let ok = match claim.op.as_str() {
        "add-else" | "add-else-if" => before.kind == "if" && before.child("else").is_none(),
        "remove-else" => before.kind == "if" && before.child("else").is_some(),
        "remove-if" => before.kind == "if",
        "add-arm" | "remove-arm" | "move-arm" => before.kind == "match",
        "change-loop-kind" => matches!(before.kind, "for" | "while" | "loop"),
        _ => false,
    };
    if !ok {
        return Outcome::refuted(format!("`{}` does not fit a `{}` statement", claim.op, before.kind));
    }
    let _ = after;
    texts(context, &explains.clone(), Outcome::verified("shape", explains))
}

fn check_statement_edit(context: &Context, claim: &Claim, kind: EditKind) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    match context.edits.iter().position(|edit| edit.kind == kind && edit.before.as_ref() == Some(at)) {
        Some(index) => texts(context, &[index], Outcome::verified("shape", vec![index])),
        None if semilla_syntax::resolve_node(context.before, at).is_none() => Outcome::malformed(format!("no statement at `{at}` before the step")),
        None => Outcome::new(ClaimStatus::Empty),
    }
}

fn check_move_statement(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let to: Option<Anchor> = claim.arg_str("to").and_then(|to| to.parse().ok());
    let Some(to) = to else {
        return Outcome::malformed("`to` is not an anchor");
    };
    match context.edits.iter().position(|edit| edit.kind == EditKind::Moved && edit.before.as_ref() == Some(at) && edit.after.as_ref() == Some(&to)) {
        Some(index) => texts(context, &[index], Outcome::verified("shape", vec![index])),
        None => Outcome::new(ClaimStatus::Empty),
    }
}

/// `wrap-statements`: `count` statements go into a new compound statement. `unwrap-statements`:
/// the reverse.
fn check_wrap(context: &Context, claim: &Claim, wrap: bool) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let Some((parent, first)) = split_statement(at) else {
        return Outcome::malformed("the anchor is not a statement");
    };
    let block_anchor = Anchor { item: at.item.clone(), path: parent.clone() };
    let (removed_kind, added_kind) = if wrap { (EditKind::Deleted, EditKind::Inserted) } else { (EditKind::Inserted, EditKind::Deleted) };
    let (old_ws, new_ws) = if wrap { (context.before, context.after) } else { (context.after, context.before) };
    let side = |edit: &Edit, removed: bool| if removed == wrap { edit.before.clone() } else { edit.after.clone() };
    // The compound statement that holds the statements.
    let Some(holder) = context.edits.iter().position(|edit| {
        edit.kind == added_kind && side(edit, false).is_some_and(|a| a.path.len() == parent.len() + 1 && block_anchor.contains(&a)) && edit.block_like
    }) else {
        return Outcome::new(ClaimStatus::Empty);
    };
    let holder_anchor = side(&context.edits[holder], false).unwrap();
    let holder_node = semilla_syntax::resolve_node(new_ws, &holder_anchor);
    if let (Some(with), Some(node)) = (claim.arg_str("with"), holder_node) {
        let expected = match with {
            "block" => "block_expr",
            other => other,
        };
        if wrap && node.kind != expected {
            return Outcome::refuted(format!("the new statement is a `{}`, not `{with}`", node.kind));
        }
    }
    let inner: Vec<String> = holder_node
        .map(|node| node.children.iter().filter(|c| c.kind == "block").flat_map(|b| b.children.iter().map(|s| s.text.clone())).collect())
        .unwrap_or_default();
    let mut explains = vec![holder];
    let mut moved = Vec::new();
    for (index, edit) in context.edits.iter().enumerate() {
        if edit.kind != removed_kind {
            continue;
        }
        let Some(anchor) = side(edit, true) else { continue };
        if anchor.path.len() == parent.len() + 1 && block_anchor.contains(&anchor) {
            if let Some(Segment::Stmt(i)) = anchor.path.last() {
                if *i >= first {
                    let text = if wrap { edit.text_before.clone() } else { edit.text_after.clone() };
                    moved.push(text.unwrap_or_default());
                    explains.push(index);
                }
            }
        }
    }
    let _ = old_ws;
    if moved != inner || moved.is_empty() {
        return Outcome::refuted("the statements inside the new block are not the moved statements");
    }
    texts(context, &[holder], Outcome::verified("shape", explains))
}

/// Statement-level neutral ops: they explain the edits in their region, and the body proof
/// decides.
fn check_neutral_region(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    if semilla_syntax::resolve_node(context.before, at).is_none() {
        return Outcome::malformed(format!("no node at `{at}` before the step"));
    }
    let count = claim.args.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let explains = region_edits(context, at, count);
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    if explains.iter().any(|&index| context.edits[index].view != semilla_core::View::Syntax) {
        return Outcome::refuted("the region has changes outside the function body");
    }
    let outcome = texts(context, &explains.clone(), Outcome::verified("shape", explains));
    body_proof(context, &at.item, outcome)
}

/// Expression ops that match one edit at the anchor (or one level below it).
fn check_expression(context: &Context, claim: &Claim, shape: impl Fn(&Edit, &Edit) -> bool) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let explains: Vec<usize> = edits_inside(context, at, false).into_iter().filter(|&i| at.depth_to(context.edits[i].before.as_ref().unwrap()).is_some_and(|d| d <= 1)).collect();
    let all_inside = edits_inside(context, at, false);
    if all_inside.is_empty() {
        return match semilla_syntax::resolve_node(context.before, at) {
            Some(_) => Outcome::new(ClaimStatus::Empty),
            None => Outcome::malformed(format!("no node at `{at}` before the step")),
        };
    }
    if explains.len() != all_inside.len() {
        return Outcome::refuted(format!("`{}` explains one change at the anchor, but the anchor has deeper changes", claim.op));
    }
    if !explains.iter().all(|&i| shape(&context.edits[i], &context.edits[i])) {
        return Outcome::refuted(format!("the change at `{at}` does not have the shape of `{}`", claim.op));
    }
    texts(context, &explains.clone(), Outcome::verified("shape", explains))
}

fn check_operator(context: &Context, claim: &Claim) -> Outcome {
    let mut outcome = check_expression(context, claim, |edit, _| edit.kind == EditKind::Modified && edit.attr_before != edit.attr_after && matches!(edit.node_before, Some("binary" | "unary")));
    if outcome.status.is_verified() {
        add_effect(context, claim.at.as_ref().unwrap(), &mut outcome);
    }
    outcome
}

fn check_condition(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let is_condition = at.path.iter().any(|segment| matches!(segment, Segment::Role(role) if role == "cond" || role.ends_with(".guard")));
    if !is_condition {
        return Outcome::malformed("`change-condition` needs an anchor on a condition (`cond` or a guard)");
    }
    let explains = edits_inside(context, at, false);
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    if explains.iter().any(|&i| context.edits[i].block_like) {
        return Outcome::refuted("the condition change contains statements; use structured ops");
    }
    let mut outcome = texts(context, &explains.clone(), Outcome::verified("shape", explains));
    add_effect(context, at, &mut outcome);
    outcome
}

/// Computes the effect of a change inside a condition, with SMT.
fn add_effect(context: &Context, at: &Anchor, outcome: &mut Outcome) {
    // The condition is the nearest ancestor with the role `cond` or a guard.
    let mut condition = at.clone();
    while !matches!(condition.path.last(), Some(Segment::Role(role)) if role == "cond" || role.ends_with(".guard")) {
        match condition.parent() {
            Some(parent) => condition = parent,
            None => return,
        }
    }
    let before = semilla_syntax::resolve_node(context.before, &condition).map(|n| n.text.clone());
    let after_anchor = context.edits.iter().find(|e| e.before.as_ref().is_some_and(|b| condition.contains(b))).and_then(|e| e.after.clone());
    let after = after_anchor
        .and_then(|anchor| {
            let mut current = anchor;
            while current.path.len() > condition.path.len() {
                current = current.parent()?;
            }
            semilla_syntax::resolve_node(context.after, &current).map(|n| n.text.clone())
        });
    let (Some(before), Some(after)) = (before, after) else { return };
    let types = local_types(context.before, &condition.item);
    if let Some(check) = context.exprs.classify(&before, &after, &types) {
        outcome.effect = Some(check.effect);
        outcome.witness = check.witness;
        // `narrow`, `widen`, and `negate` change which branch runs. `unrelated` can be an
        // equivalence that the solver cannot see (method calls): the MIR decides the class.
        match check.effect {
            Effect::Equivalent => outcome.class = Some(Class::Neutral),
            Effect::Narrow | Effect::Widen | Effect::Negate => outcome.class = Some(Class::ControlFlow),
            Effect::Unrelated => {}
        }
    }
    outcome.before = Some(before);
    outcome.after = Some(after);
}

/// The types of the parameters and the annotated `let` variables of a function.
pub fn local_types(workspace: &Workspace, item: &str) -> BTreeMap<String, String> {
    let mut types = BTreeMap::new();
    let Some(item) = workspace.items.get(item) else { return types };
    if let Ok(signature) = syn::parse_str::<syn::Signature>(&item.signature) {
        for input in &signature.inputs {
            if let syn::FnArg::Typed(typed) = input {
                if let syn::Pat::Ident(ident) = typed.pat.as_ref() {
                    types.insert(ident.ident.to_string(), quote::ToTokens::to_token_stream(&typed.ty).to_string().replace(' ', ""));
                }
            }
        }
    }
    if let Some(body) = &item.body {
        let mut path = Vec::new();
        body.walk(&mut path, &mut |_, node| {
            if node.kind == "let" {
                if let Some(pattern) = node.child("pat") {
                    if let Some((name, ty)) = pattern.text.split_once(" : ") {
                        types.insert(name.trim_start_matches("mut ").trim().to_string(), ty.replace(' ', ""));
                    }
                }
            }
        });
    }
    types
}

fn check_swap(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let before = semilla_syntax::resolve_node(context.before, at);
    let explains = edits_inside(context, at, false);
    let after_anchor = explains.first().and_then(|&i| context.edits[i].after.clone()).map(|mut a| {
        while a.path.len() > at.path.len() {
            a = a.parent().unwrap();
        }
        a
    });
    let after = after_anchor.as_ref().and_then(|a| semilla_syntax::resolve_node(context.after, a));
    let (Some(before), Some(after)) = (before, after) else {
        return Outcome::new(ClaimStatus::Empty);
    };
    let swapped = before.kind == "binary"
        && after.kind == "binary"
        && before.attr == after.attr
        && before.child("lhs").map(|n| &n.text) == after.child("rhs").map(|n| &n.text)
        && before.child("rhs").map(|n| &n.text) == after.child("lhs").map(|n| &n.text);
    if !swapped {
        return Outcome::refuted("the operands are not swapped");
    }
    let mut outcome = texts(context, &explains.clone(), Outcome::verified("shape", explains));
    outcome.before = Some(before.text.clone());
    outcome.after = Some(after.text.clone());
    let types = local_types(context.before, &at.item);
    if context.exprs.equivalent(&before.text, &after.text, &types) == Some(true) {
        outcome.class = Some(Class::Neutral);
        outcome.status = ClaimStatus::Verified { proof: "smt".into() };
    }
    outcome
}

fn check_wrap_expression(context: &Context, claim: &Claim, wrap: bool) -> Outcome {
    let method = match claim.op.as_str() {
        "add-clone" => Some("clone".to_string()),
        "chain-method" => claim.arg_str("method").map(str::to_string),
        _ => None,
    };
    check_expression(context, claim, |edit, _| {
        let (inner, outer) = if wrap { (&edit.text_before, &edit.text_after) } else { (&edit.text_after, &edit.text_before) };
        let (Some(inner), Some(outer)) = (inner, outer) else { return false };
        let outer_kind = if wrap { edit.node_after } else { edit.node_before };
        let outer_attr = if wrap { &edit.attr_after } else { &edit.attr_before };
        let wraps = outer.contains(inner.as_str()) && outer.len() > inner.len() && matches!(outer_kind, Some("call" | "method_call" | "ref" | "try" | "cast" | "struct" | "unary"));
        wraps && method.as_ref().is_none_or(|m| outer_kind == Some("method_call") && outer_attr.as_deref() == Some(m.as_str()))
    })
}

fn check_argument(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let before = semilla_syntax::resolve_node(context.before, at);
    let Some(before) = before.filter(|n| matches!(n.kind, "call" | "method_call")) else {
        return Outcome::malformed("the anchor is not a call");
    };
    let explains = edits_inside(context, at, false);
    let Some(after) = explains.first().and_then(|&i| context.edits[i].after.as_ref()).and_then(|a| semilla_syntax::resolve_node(context.after, a)) else {
        return Outcome::new(ClaimStatus::Empty);
    };
    let args = |node: &Node| node.children.iter().filter(|c| c.role.starts_with("args[")).map(|c| c.text.clone()).collect::<Vec<_>>();
    let (old, new) = (args(before), args(after));
    let (short, long) = if claim.op == "add-argument" { (&old, &new) } else { (&new, &old) };
    let one_more = long.len() == short.len() + 1 && (0..long.len()).any(|skip| {
        let mut rest = long.clone();
        rest.remove(skip);
        &rest == short
    });
    let same_target = before.attr == after.attr && before.child("callee").map(|c| &c.text) == after.child("callee").map(|c| &c.text);
    if !(one_more && same_target) {
        return Outcome::refuted(format!("the call does not have one {} argument", if claim.op == "add-argument" { "more" } else { "less" }));
    }
    texts(context, &explains.clone(), Outcome::verified("shape", explains))
}

/// Free edits: the edits inside a block-free subtree, with the tightness rule.
fn check_free(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let Some(node) = semilla_syntax::resolve_node(context.before, at) else {
        return Outcome::malformed(format!("no node at `{at}` before the step"));
    };
    if node.stmt || node.kind == "block" || matches!(semilla_syntax::resolve(context.before, at), Some(Resolved::Item(_))) {
        return Outcome::malformed("a free edit must be on an expression, a pattern, or a type, not on a statement or a block");
    }
    let explains = edits_inside(context, at, false);
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    if node.has_block() || explains.iter().any(|&i| context.edits[i].block_like) {
        return Outcome::malformed("a free edit must be block-free (no if, match, loop, or block inside); use structured ops");
    }
    // The tightness rule: the claim is at most one level above the lowest common node.
    let anchors: Vec<&Anchor> = explains.iter().filter_map(|&i| context.edits[i].before.as_ref()).collect();
    let mut common = anchors[0].clone();
    while !anchors.iter().all(|a| common.contains(a)) {
        common = common.parent().unwrap_or(common.clone());
    }
    let depth = at.depth_to(&common).unwrap_or(0);
    if depth > 1 {
        let mut outcome = Outcome::malformed(format!("the free edit is too wide: the change is only in `{common}`"));
        outcome.hint = Some(format!("Put the claim on `{common}`."));
        return outcome;
    }
    let mut outcome = texts(context, &explains.clone(), Outcome::verified("free", explains));
    outcome.before = Some(node.text.clone());
    outcome
}

/// `replace-macro-input`: the edits inside one macro call. The input of a macro is a token tree,
/// so this is the only free edit that can be on a statement: a macro call statement.
fn check_macro_input(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let Some(node) = semilla_syntax::resolve_node(context.before, at) else {
        return Outcome::malformed(format!("no node at `{at}` before the step"));
    };
    if node.kind != "macro" {
        return Outcome::malformed(format!("`replace-macro-input` needs a macro call, not a `{}`", node.kind));
    }
    let explains = edits_inside(context, at, false);
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    // The macro must stay the same macro: only its input changes.
    if explains.iter().any(|&i| context.edits[i].before.as_ref() == Some(at) && context.edits[i].attr_before != context.edits[i].attr_after) {
        return Outcome::refuted("the macro name changed, not only its input");
    }
    let mut outcome = texts(context, &explains.clone(), Outcome::verified("free", explains));
    outcome.before = Some(node.text.clone());
    outcome
}

/// The top-level alternatives of an or-pattern: `a | (b | c) | d` gives `a`, `(b | c)`, `d`.
fn alternatives(pattern: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    let mut quote = false;
    for c in pattern.chars() {
        match c {
            '\'' | '"' => quote = !quote,
            '(' | '[' | '{' if !quote => depth += 1,
            ')' | ']' | '}' if !quote => depth -= 1,
            '|' if depth == 0 && !quote => {
                parts.push(current.trim().to_string());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts.push(current.trim().to_string());
    parts.retain(|p| !p.is_empty());
    parts
}

/// True if `after` keeps all alternatives of `before`, in order, and adds at least one.
pub fn alternatives_added(before: &str, after: &str) -> bool {
    let (old, new) = (alternatives(before), alternatives(after));
    let mut rest = new.iter();
    new.len() > old.len() && old.iter().all(|alt| rest.any(|n| n == alt))
}

fn strip_paren(text: &str) -> &str {
    let text = text.trim();
    if text.starts_with('(') && text.ends_with(')') { text[1..text.len() - 1].trim() } else { text }
}

/// Parses an expression and returns its token text, so that the spacing is the same.
fn normalize_expr(text: &str) -> Option<String> {
    syn::parse_str::<syn::Expr>(text).ok().map(|expr| quote::ToTokens::to_token_stream(&expr).to_string())
}


/// True if an anchor is in an item or in one of its members.
fn in_item(anchor: Option<&Anchor>, item: &str) -> bool {
    anchor.is_some_and(|anchor| anchor.item == item || anchor.item.starts_with(&format!("{item}::")))
}

/// True if an edit is the member list of the impl that owns `item` (a new or removed method).
fn is_owner_members(edit: &Edit, item: &str) -> bool {
    edit.kind == EditKind::Members && item.contains("::") && {
        let owner = item.rsplit_once("::").map(|(owner, _)| owner).unwrap_or("");
        edit.anchor().is_some_and(|anchor| anchor.item.ends_with(&format!("impl {}", last_segment(owner))) || anchor.item.contains(&format!("impl {owner}")))
    }
}

/// `rewrite-item`: a free edit of a whole item, with an optional new path. It explains every edit
/// of the item. The reviewer sees the old and the new item side by side.
fn check_rewrite(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let new = claim.arg_str("new").map(|new| full_path(&at.item, new)).unwrap_or_else(|| at.item.clone());
    let Some(old_item) = context.before.items.get(&at.item) else {
        return Outcome::malformed(format!("no item `{}` before the step", at.item));
    };
    let Some(new_item) = context.after.items.get(&new) else {
        return Outcome::malformed(format!("no item `{new}` after the step; `new` names the path after the rewrite"));
    };
    if claim.arg_str("note").is_none_or(|note| note.trim().len() < 10) {
        return Outcome::malformed("`rewrite-item` needs a `note` that says why the item is rewritten as a whole");
    }
    let explains: Vec<usize> = context
        .edits
        .iter()
        .enumerate()
        .filter(|(_, edit)| in_item(edit.before.as_ref(), &at.item) || in_item(edit.after.as_ref(), &new) || is_owner_members(edit, &at.item) || is_owner_members(edit, &new))
        .map(|(index, _)| index)
        .collect();
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    let mut outcome = Outcome::verified("rewrite", explains);
    outcome.before = Some(old_item.source.clone());
    outcome.after = Some(new_item.source.clone());
    outcome.class = match context.bodies.status(&new).or_else(|| context.bodies.status(&at.item)) {
        Some(BodyStatus::Equal { .. }) => Some(Class::Neutral),
        Some(BodyStatus::Different { cfg_changed: true }) => Some(Class::ControlFlow),
        Some(_) => Some(Class::Logic),
        None => None,
    };
    outcome
}

/// The prefixes of the external APIs that `no-io` forbids.
pub const IO_PREFIXES: &[&str] = &["std::fs::", "std::io::", "std::net::", "std::process::", "std::env::", "std::os::", "std::thread::", "tokio::", "async_std::", "reqwest::", "hyper::"];

/// An item and the local items that it calls, directly or indirectly (from the syntax calls).
pub fn reach(calls: &BTreeMap<String, BTreeSet<String>>, item: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::from([item.to_string()]);
    let mut todo = vec![item.to_string()];
    while let Some(current) = todo.pop() {
        for next in calls.get(&current).into_iter().flatten() {
            if !next.starts_with("ext::") && seen.insert(next.clone()) {
                todo.push(next.clone());
            }
        }
    }
    seen
}

/// The explicit panic sites of one item: `unwrap`, `expect`, the panic macros, and indexing.
pub fn item_panic_sites(item: &semilla_syntax::Item) -> Vec<String> {
    let mut sites = Vec::new();
    let Some(body) = &item.body else { return sites };
    let mut path = Vec::new();
    body.walk(&mut path, &mut |roles, node| {
        let attr = node.attr.as_deref().unwrap_or("");
        let site = match node.kind {
            "method_call" => matches!(attr, "unwrap" | "expect"),
            "macro" => matches!(attr, "panic" | "unreachable" | "todo" | "unimplemented" | "assert" | "assert_eq" | "assert_ne"),
            "index" => true,
            _ => false,
        };
        if site {
            sites.push(format!("{}/{}", item.path, roles.join("/")));
        }
    });
    sites
}

/// The property claims (design section 21.4): facts about the code after the step.
fn check_property(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    if !at.path.is_empty() {
        return Outcome::malformed(format!("`{}` is a claim on an item; use the item anchor `{}`", claim.op, at.item));
    }
    let Some(item) = context.after.items.get(&at.item) else {
        return Outcome::malformed(format!("no item `{}` after the step", at.item));
    };
    if item.body.is_none() {
        return Outcome::malformed(format!("`{}` is not a function", at.item));
    }
    let reached = reach(context.calls, &at.item);
    let list = |found: &[String]| {
        let mut text = found.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
        if found.len() > 5 {
            text.push_str(&format!(", and {} more", found.len() - 5));
        }
        text
    };
    let mut outcome = match claim.op.as_str() {
        "no-unsafe" => {
            let found: Vec<String> = reached
                .iter()
                .filter_map(|path| context.after.items.get(path))
                .filter(|item| item.text.split(' ').any(|token| token == "unsafe"))
                .map(|item| item.path.clone())
                .collect();
            if found.is_empty() { Outcome::verified("syntax", Vec::new()) } else { Outcome::refuted(format!("unsafe code in {}", list(&found))) }
        }
        "no-panic" => {
            let found: Vec<String> = reached.iter().filter_map(|path| context.after.items.get(path)).flat_map(item_panic_sites).collect();
            if found.is_empty() { Outcome::verified("syntax", Vec::new()) } else { Outcome::refuted(format!("explicit panic sites: {}", list(&found))) }
        }
        "tested-by" => {
            let test = claim.arg_str("test").unwrap();
            let path = if context.after.items.contains_key(test) { test.to_string() } else {
                // A last name: the test items of the same crate with that name.
                let krate = at.item.split("::").next().unwrap_or("");
                match context.after.items.values().filter(|i| i.test && i.path.starts_with(&format!("{krate}::")) && last_segment(&i.path) == test).map(|i| i.path.clone()).collect::<Vec<_>>().as_slice() {
                    [one] => one.clone(),
                    [] => return Outcome::malformed(format!("no test `{test}` after the step")),
                    _ => return Outcome::malformed(format!("more than one test is named `{test}`; use the full path")),
                }
            };
            if !context.after.items.get(&path).is_some_and(|i| i.test) {
                return Outcome::malformed(format!("`{path}` is not a test"));
            }
            if reach(context.calls, &path).contains(&at.item) {
                Outcome::verified("graph", Vec::new())
            } else {
                Outcome::refuted(format!("the test `{path}` does not call `{}`", at.item))
            }
        }
        "no-io" | "calls-only" => {
            let Some(facts) = context.facts else {
                return Outcome::new(ClaimStatus::Pending { reason: "the external calls come from the MIR in the background check (phase 2)".into() });
            };
            // The MIR knows the real local calls: follow them too.
            let mut items = reached.clone();
            let mut todo: Vec<String> = items.iter().cloned().collect();
            while let Some(current) = todo.pop() {
                for callee in facts.get(&current).map(|f| f.callees.clone()).unwrap_or_default() {
                    if items.insert(callee.clone()) {
                        todo.push(callee);
                    }
                }
            }
            let externals: BTreeSet<String> = items.iter().filter_map(|item| facts.get(item)).flat_map(|f| f.externals.iter().cloned()).collect();
            let allowed = claim.arg_list("calls");
            let bad: Vec<String> = externals
                .into_iter()
                .filter(|path| {
                    let plain = path.trim_start_matches('<');
                    if claim.op == "no-io" { IO_PREFIXES.iter().any(|prefix| plain.starts_with(prefix)) } else { !allowed.iter().any(|prefix| plain.starts_with(prefix.as_str())) }
                })
                .collect();
            if bad.is_empty() { Outcome::verified("mir", Vec::new()) } else { Outcome::refuted(format!("external calls: {}", list(&bad))) }
        }
        other => Outcome::malformed(format!("the verifier has no checker for `{other}`")),
    };
    if outcome.status.is_verified() {
        outcome.after = Some(format!("{} ({} item(s) checked)", claim.op, reached.len()));
    }
    outcome
}
