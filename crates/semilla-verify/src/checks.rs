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
    /// For a refuted `calls-only` claim: the complete list of `calls` that makes the claim true.
    pub complete_calls: Vec<String>,
}

impl Outcome {
    fn new(status: ClaimStatus) -> Outcome {
        Outcome { status, explains: Vec::new(), class: None, effect: None, witness: None, hint: None, before: None, after: None, needs_body_proof: false, self_evident: false, complete_calls: Vec::new() }
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
    /// The MIR name → its status.
    statuses: BTreeMap<String, BodyStatus>,
    /// The item path and the MIR name → the uses of the build configuration in the changed
    /// bodies.
    config: BTreeMap<String, std::collections::BTreeSet<String>>,
    /// The item path and the MIR name → the configurations in which a body is not equal, if
    /// the body is equal in a different checked configuration (batch 5s, A5).
    differs_in: BTreeMap<String, std::collections::BTreeSet<String>>,
    /// The item path and the MIR name → the configurations in which each body that is not
    /// equal has a proof (batch 5u, part A). The set is empty if one body has no proof in each
    /// configuration.
    equal_in: BTreeMap<String, std::collections::BTreeSet<String>>,
    /// The item path and the MIR name → the causes of a removed proof (batch 5u).
    stopped: BTreeMap<String, Vec<String>>,
    pub available: bool,
}

impl BodyIndex {
    pub fn new(bodies: &[BodyComparison], after: &Workspace, before: &Workspace) -> BodyIndex {
        let mut index = BodyIndex { by_item: BTreeMap::new(), items: BTreeMap::new(), statuses: BTreeMap::new(), config: BTreeMap::new(), differs_in: BTreeMap::new(), equal_in: BTreeMap::new(), stopped: BTreeMap::new(), available: !bodies.is_empty() };
        for body in bodies {
            index.statuses.insert(body.name.clone(), body.status.clone());
            // A body that exists only before has its span in the "before" tree.
            let item = if body.status == BodyStatus::BeforeOnly { find_item(body, before).or_else(|| find_item(body, after)) } else { find_item(body, after).or_else(|| find_item(body, before)) };
            let uses_config = !body.config.is_empty() && !matches!(body.status, BodyStatus::Equal { .. });
            if uses_config {
                index.config.entry(body.name.clone()).or_default().extend(body.config.iter().cloned());
            }
            if !matches!(body.status, BodyStatus::Equal { .. }) {
                // The intersection: a configuration has a proof only if each body has one.
                let equal: std::collections::BTreeSet<String> = body.equal_in.iter().cloned().collect();
                for key in std::iter::once(&body.name).chain(item.iter()) {
                    match index.equal_in.get_mut(key) {
                        Some(old) => old.retain(|config| equal.contains(config)),
                        None => {
                            index.equal_in.insert(key.clone(), equal.clone());
                        }
                    }
                }
            }
            if !body.stopped.is_empty() && !matches!(body.status, BodyStatus::Equal { .. }) {
                for key in std::iter::once(&body.name).chain(item.iter()) {
                    let list = index.stopped.entry(key.clone()).or_default();
                    list.extend(body.stopped.iter().filter(|text| !list.contains(text)).cloned().collect::<Vec<_>>());
                }
            }
            if !body.differs_in.is_empty() {
                index.differs_in.entry(body.name.clone()).or_default().extend(body.differs_in.iter().cloned());
                if let Some(item) = &item {
                    index.differs_in.entry(item.clone()).or_default().extend(body.differs_in.iter().cloned());
                }
            }
            if let Some(item) = item {
                if uses_config {
                    index.config.entry(item.clone()).or_default().extend(body.config.iter().cloned());
                }
                index.items.insert(body.name.clone(), item.clone());
                index.by_item.entry(item).or_default().push(body.status.clone());
            }
        }
        index
    }

    /// The uses of the build configuration in the changed bodies of an item (or a MIR name):
    /// the text of each macro call. Empty for a body that is equal on the two sides.
    pub fn config(&self, name: &str) -> Vec<String> {
        self.config.get(name).into_iter().flatten().cloned().collect()
    }

    /// The configurations in which a body of an item (or a MIR name) is not equal, if the body
    /// is equal in a different checked configuration.
    pub fn differs_in(&self, name: &str) -> Vec<String> {
        self.differs_in.get(name).into_iter().flatten().cloned().collect()
    }

    /// The configurations in which each body of an item (or a MIR name) that is not equal has
    /// a proof. `None`: the item has no body that is not equal.
    pub fn equal_in(&self, name: &str) -> Option<&std::collections::BTreeSet<String>> {
        self.equal_in.get(name)
    }

    /// The causes of a removed proof of a body of an item (or a MIR name) (batch 5u).
    pub fn stopped(&self, name: &str) -> &[String] {
        self.stopped.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Each changed body that reads the build configuration (the item path and the MIR name
    /// can be in the list for one body).
    pub fn config_names(&self) -> impl Iterator<Item = &String> {
        self.config.keys()
    }

    /// The MIR names of the compared bodies.
    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.statuses.keys()
    }

    /// True if the body of an item (or a MIR name) differs in the step.
    pub fn changed(&self, name: &str) -> bool {
        let item = self.status(name).is_some_and(|status| !matches!(status, BodyStatus::Equal { .. }));
        item || self.statuses.get(name).is_some_and(|status| !matches!(status, BodyStatus::Equal { .. }))
    }

    /// The names to show for an affected item (batch 5x, part E). A nested function is its own
    /// body: it has no source item, so its MIR body belongs to the function around it. The
    /// result has the name of each nested function with a changed body, and the item itself
    /// only if a body of its own (the function, a closure) changed.
    pub fn shown_bodies(&self, item: &str) -> Vec<String> {
        let mut own = false;
        let mut nested = std::collections::BTreeSet::new();
        for (name, _) in self.items.iter().filter(|(_, of)| of.as_str() == item) {
            if self.statuses.get(name).is_none_or(|status| matches!(status, BodyStatus::Equal { .. })) {
                continue;
            }
            let base = name.split("::{").next().unwrap_or(name);
            match base.strip_prefix(item).filter(|rest| rest.starts_with("::")) {
                Some(_) => {
                    nested.insert(base.to_string());
                }
                None => own = true,
            }
        }
        if nested.is_empty() {
            return vec![item.to_string()];
        }
        own.then(|| item.to_string()).into_iter().chain(nested).collect()
    }

    /// The item path of a MIR name.
    pub fn item_of_name(&self, name: &str) -> Option<String> {
        self.items.get(name).cloned()
    }

    pub fn item_of(&self, body: &BodyComparison) -> Option<String> {
        self.items.get(&body.name).cloned()
    }

    /// True if at least one MIR body belongs to the item: a checked configuration compiles it.
    pub fn has_body(&self, item: &str) -> bool {
        self.by_item.contains_key(item)
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
    // The `main` function of the test harness has no source item.
    if crate::is_test_harness_main(name) {
        return None;
    }
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
    if workspace.items.contains_key(name) {
        return Some(name.to_string());
    }
    // A constant or a static of a target that is not the library (batch 5n, A4). The MIR name
    // starts with the crate name of the target (`hex::DATA` for `benches/hex.rs`), and the item
    // path starts with the package and the target (`hex::hex::DATA`). The span and the last
    // name of the body find the item.
    let (file, line) = span?;
    let short = name.rsplit("::").next()?;
    let found = workspace.items.values().filter(|item| {
        matches!(item.kind, semilla_syntax::ItemKind::Const | semilla_syntax::ItemKind::Static)
            && (file.ends_with(&item.file) || item.file.ends_with(file.as_str()))
            && item.line_start <= *line
            && *line <= item.line_end
            && item.path.rsplit("::").next() == Some(short)
    });
    found.min_by_key(|item| item.line_end - item.line_start).map(|item| item.path.clone())
}

/// Adds the `impl` blocks of each moved type to the rename map (batch 5x, part B): a block of
/// the old module with the moved type as its self type, when the block is not in the old module
/// after the step and a block with the same name is in the new module. The methods of a trait
/// impl follow their block. The diff then compares each block with its moved block, and an
/// edit in a block stays an edit.
pub fn moved_impls(step: &Step, before: &Workspace, after: &Workspace, renames: &mut Renames) {
    use semilla_syntax::ItemKind;
    for claim in step.claims.iter().filter(|claim| claim.op == "move-item") {
        let Some((at, to)) = claim.at.as_ref().zip(claim.arg_str("to")) else { continue };
        let new_path = full_path(&at.item, to);
        let (old_module, new_module, name) = (parent(&at.item), parent(&new_path), last_segment(&at.item));
        let is_type = before.items.get(&at.item).is_some_and(|item| matches!(item.kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Type));
        if !is_type || old_module == new_module || name != last_segment(&new_path) {
            continue;
        }
        let start = format!("{old_module}::impl ");
        let blocks: Vec<(String, String)> = before
            .items
            .values()
            .filter(|item| item.kind == ItemKind::Impl)
            .filter_map(|item| {
                let tail = item.path.strip_prefix(&start)?;
                // The owner of the block: `Name`, or `<Name as Trait>`. A twin has a suffix `#N`.
                let owner = tail.rsplit_once('#').filter(|(_, number)| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())).map_or(tail, |(owner, _)| owner);
                let own = owner == name || owner.strip_prefix('<').and_then(|rest| rest.strip_prefix(name)).is_some_and(|rest| rest.starts_with(" as "));
                let moved = format!("{new_module}::impl {tail}");
                (own && !after.items.contains_key(&item.path) && after.items.get(&moved).is_some_and(|found| found.kind == ItemKind::Impl)).then(|| (owner.to_string(), tail.to_string()))
            })
            .collect();
        for (owner, tail) in blocks {
            renames.items.entry(format!("{start}{tail}")).or_insert_with(|| format!("{new_module}::impl {tail}"));
            if owner.starts_with('<') {
                let methods = format!("{old_module}::{owner}::");
                for path in before.items.keys().filter(|path| path.starts_with(&methods)) {
                    renames.items.entry(path.clone()).or_insert_with(|| format!("{new_module}::{}", &path[old_module.len() + 2..]));
                }
            }
        }
    }
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
                // A field: the uses follow at the field positions only (not a local variable).
                if before.items.get(&at.item).is_some_and(|item| item.kind == semilla_syntax::ItemKind::Field) {
                    if old_name != new_name {
                        renames.fields.push((old_name.to_string(), new_name.to_string()));
                    }
                    continue;
                }
                // A trait method: the methods of the impls of the trait follow. The MIR names of
                // the impl methods are `<Type as crate::Trait>::method`, so the map also has the
                // key `crate::Trait>::method` for the body compare.
                let owner = parent(&at.item);
                if claim.op == "rename" && before.items.get(owner).is_some_and(|item| item.kind == semilla_syntax::ItemKind::Trait) && owner == parent(&new_path) {
                    let trait_name = last_segment(owner);
                    for path in before.items.keys().filter(|path| impl_method_of(path, trait_name, old_name)) {
                        renames.items.insert(path.clone(), format!("{}{new_name}", &path[..path.len() - old_name.len()]));
                    }
                    renames.items.insert(format!("{owner}>::{old_name}"), format!("{owner}>::{new_name}"));
                }
                // The methods, the fields, and the variants of a renamed type follow the type.
                for path in before.items.keys().filter(|path| is_child(path, &at.item)) {
                    renames.items.insert(path.clone(), format!("{new_path}{}", &path[at.item.len()..]));
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
                if let Some(old) = rename_local_from(before, claim, at) {
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

/// True if `path` is the method `method` of an impl of a trait with the last name `trait_name`:
/// `module::<Type as path::Trait>::method`.
fn impl_method_of(path: &str, trait_name: &str, method: &str) -> bool {
    let Some(owner) = path.strip_suffix(method).and_then(|rest| rest.strip_suffix(">::")) else { return false };
    owner.rsplit_once(" as ").is_some_and(|(_, tr)| last_segment(tr.split('<').next().unwrap_or(tr)) == trait_name)
}

/// `to` can be a full path, or a new last segment for the same parent. For a field
/// (`shop::Account.balance`), a last name replaces the field name.
fn full_path(item: &str, to: &str) -> String {
    if to.contains("::") || to.contains('.') {
        to.to_string()
    } else if let Some((owner, _)) = item.rsplit_once('.').filter(|(_, field)| !field.contains("::")) {
        format!("{owner}.{to}")
    } else {
        match item.rsplit_once("::") {
            Some((parent, _)) => format!("{parent}::{to}"),
            None => to.to_string(),
        }
    }
}

/// True if `path` is a child of `parent`: a method or a variant (`parent::x`) or a field
/// (`parent.x`).
pub fn is_child(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent).is_some_and(|rest| rest.starts_with("::") || rest.starts_with('.'))
}

pub(crate) fn last_segment(path: &str) -> &str {
    let last = path.rsplit("::").next().unwrap_or(path);
    last.rsplit_once('.').map(|(_, field)| field).unwrap_or(last)
}

/// The parent path (the module of an item), or the empty text.
fn parent(path: &str) -> &str {
    path.rsplit_once("::").map(|(parent, _)| parent).unwrap_or("")
}

/// The path without the twin suffix (`demo::impl Foo#1` gives `demo::impl Foo`).
fn without_twin(path: &str) -> &str {
    match path.rsplit_once('#') {
        Some((base, index)) if !index.is_empty() && index.chars().all(|c| c.is_ascii_digit()) && !base.ends_with("::use") => base,
        _ => path,
    }
}

/// The variable name of a `let` statement (without `mut` and a type).
fn local_name(workspace: &Workspace, at: &Anchor) -> Option<String> {
    let node = semilla_syntax::resolve_node(workspace, at)?;
    let pattern = if node.kind == "let" { node.child("pat")? } else { node };
    pattern.text.split(' ').find(|token| *token != "mut" && *token != "ref" && token.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')).map(str::to_string)
}

/// The old name of a `rename-local` claim: the variable of the `let` at the anchor, or `from`
/// (a parameter, or a `let` or `for` variable of the item at the anchor).
fn rename_local_from(workspace: &Workspace, claim: &Claim, at: &Anchor) -> Option<String> {
    let Some(from) = claim.arg_str("from") else {
        return local_name(workspace, at);
    };
    let item = workspace.items.get(&at.item)?;
    is_binding(item, from).then(|| from.to_string())
}

/// True if `name` is a parameter, or a `let` or `for` variable, of the item.
pub fn is_binding(item: &semilla_syntax::Item, name: &str) -> bool {
    let signature = crate::hints::words(&item.signature);
    let parameter = signature.windows(2).any(|pair| pair[0] == name && pair[1] == ":") || signature.windows(3).any(|t| t[0] == "mut" && t[1] == name && t[2] == ":");
    let tokens = crate::hints::words(&item.text);
    let local = tokens.windows(3).any(|t| (t[0] == "let" || t[0] == "for") && (t[1] == name || (t[1] == "mut" && t[2] == name)));
    parameter || local
}

/// Checks one claim.
pub fn check(context: &Context, claim: &Claim) -> Outcome {
    if let Some(message) = ops::retired(&claim.op) {
        return Outcome::malformed(message);
    }
    let Some(info) = ops::op(&claim.op) else {
        return Outcome::malformed(format!("unknown operation `{}`", claim.op));
    };
    if let Some(missing) = info.args.iter().find(|arg| !claim.args.contains_key(**arg)) {
        return Outcome::malformed(format!("`{}` needs the argument `{missing}`", claim.op));
    }
    if let Some(message) = claim.args.keys().find_map(|key| ops::retired_arg(key)) {
        return Outcome::malformed(message);
    }
    // Each argument has a check. An argument that the op does not declare is an error.
    let optional = ops::optional_args(&claim.op);
    if let Some(unknown) = claim.args.keys().find(|key| !info.args.contains(&key.as_str()) && !optional.contains(&key.as_str())) {
        return Outcome::malformed(format!("`{}` has no argument `{unknown}`", claim.op));
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
        "change-signature" => check_signature(context, claim),
        "edit-doc" => check_attribute(context, claim, &[EditKind::Docs]),
        "extract-function" | "inline-function" => check_across(context, claim),
        // The compound ops first: `add-arm`, `add-else`, and `add-else-if` are not new statements.
        "remove-if" => check_remove_if(context, claim),
        "add-else" | "add-else-if" | "remove-else" | "add-arm" | "remove-arm" | "move-arm" | "change-loop-kind" => check_compound(context, claim),
        name if name.starts_with("add-") && info.level == OpLevel::Statement => check_add_statement(context, claim),
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
        // A function without a MIR body: no checked configuration compiles it (an inactive `cfg`).
        // Nothing proves its body, so the claim is unproved.
        None if is_function(context, item) => {
            outcome.status = ClaimStatus::Unproved { reason: format!("no MIR body for `{item}`: no checked configuration compiles it (an inactive `cfg`?)") };
            outcome.hint = Some("Add a feature set that compiles the function (design 8.12), or tag the change as logic.".into());
        }
        None => {
            // No body: a non-function item. The syntax check is the proof.
        }
    }
    outcome
}

/// True if the item is a function on either side of the step.
fn is_function(context: &Context, item: &str) -> bool {
    [context.after, context.before].iter().any(|ws| ws.items.get(item).is_some_and(|item| item.kind == semilla_syntax::ItemKind::Fn && item.body.is_some()))
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
    // The diff applied the rename first. The claim also explains the private `use` lines and the
    // paths that name the item. `move-item` also explains the private `use` lines of the source
    // module and the destination module (the moved body can need other imports). This is sound:
    // a `use` change that changes a meaning changes a MIR body, and the indirect-change rule
    // reports that body. A `pub use` line is API, so it needs its own claim.
    let (old_name, new_name) = (last_segment(&at.item), last_segment(&to));
    let modules = [parent(&at.item), parent(&to)];
    let explains: Vec<usize> = context
        .edits
        .iter()
        .enumerate()
        .filter(|(_, edit)| {
            let mentions = |text: &Option<String>, name: &str| text.as_deref().is_some_and(|text| text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| word == name));
            let is_use = edit.attr_before.as_deref() == Some("use") || edit.attr_after.as_deref() == Some("use");
            let is_path = edit.kind == EditKind::Replaced && edit.node_before == Some("path") && edit.node_after == Some("path");
            let names_item = mentions(&edit.text_before, old_name) || mentions(&edit.text_after, new_name);
            if is_use {
                let private = |anchor: &Option<Anchor>, ws: &Workspace| anchor.as_ref().is_none_or(|a| ws.items.get(&a.item).is_some_and(|item| item.vis.is_empty()));
                let module = edit.item().map(parent);
                let in_module = claim.op == "move-item" && module.is_some_and(|m| modules.contains(&m));
                private(&edit.before, context.before) && private(&edit.after, context.after) && (names_item || in_module)
            } else {
                is_path && names_item
            }
        })
        .map(|(index, _)| index)
        .collect();
    let mut outcome = Outcome::verified("N1", explains);
    outcome.self_evident = true;
    outcome.before = Some(at.item.clone());
    outcome.after = Some(to.clone());
    // The proof is the MIR compare of the whole workspace (design 22.4). The step check makes it.
    outcome.class = Some(Class::Neutral);
    outcome
}

fn check_rename_local(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let Some(old) = rename_local_from(context.before, claim, at) else {
        if let Some(from) = claim.arg_str("from") {
            return Outcome::malformed(format!("`{from}` is not a parameter or a `let` or `for` variable of `{}`", at.item));
        }
        return Outcome::malformed("the anchor is not a `let` statement (use `from` with the function as the anchor for a parameter)");
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
            let own = edit.kind == kind && item.is_some_and(|item| item.item == at.item || is_child(&item.item, &at.item));
            // A new or removed trait method changes the member list of its trait.
            let trait_members = edit.kind == EditKind::Members && edit.attr_before.as_deref() == Some("trait") && edit.anchor().is_some_and(|anchor| anchor.item == parent(&at.item));
            // A new method changes the member list of its impl.
            own || trait_members || is_owner_members(edit, &at.item)
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

/// `change-signature`: only the signature edits that no fact covers (a const or static value, a
/// `macro_rules!` body, a function signature without a changed part).
fn check_signature(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let explains: Vec<usize> = context
        .edits
        .iter()
        .enumerate()
        .filter(|(_, edit)| edit.kind == EditKind::Signature && edit.anchor().is_some_and(|anchor| anchor.item == at.item) && crate::facts::signature_part(edit).is_none())
        .map(|(index, _)| index)
        .collect();
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    // A doc test is code (brief 5h, B1): `edit-doc` explains a change of the doc text only.
    if explains.iter().any(|&index| crate::facts::doc_test_change(&context.edits[index]).is_some()) {
        let mut outcome = Outcome::new(ClaimStatus::Unproved { reason: format!("the doc change of `{}` adds, changes, or removes a code block: `cargo test` runs it as a doc test, so it is code", at.item) });
        outcome.hint = Some("Remove the claim `edit-doc`. Semilla shows the change as the fact \"doc test of … added\" (or \"changed\", \"removed\") with the class `logic`.".into());
        return outcome;
    }
    let outcome = Outcome::verified("shape", explains.clone());
    texts(context, &explains, outcome)
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

/// `remove-if keep=then|else`: the `if` goes, and the statements of the named branch take its
/// place in the block. The verifier compares the block after the step with the block before, with
/// the `if` replaced by the kept branch.
fn check_remove_if(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let keep = claim.arg_str("keep").unwrap_or("");
    if keep != "then" && keep != "else" {
        return Outcome::malformed(format!("`keep` must be `then` or `else`, not `{keep}`"));
    }
    let Some((parent, index)) = split_statement(at) else {
        // An `if` expression (`let x = if c { a } else { b };`): the node after the step is the
        // expression of the kept branch.
        let Some(node) = semilla_syntax::resolve_node(context.before, at).filter(|node| node.kind == "if") else {
            return Outcome::malformed(format!("no `if` at `{at}` before the step"));
        };
        let Some(kept) = node.child(keep).filter(|branch| branch.kind == "block" && branch.children.len() == 1) else {
            return Outcome::refuted(format!("the `if` has no `{keep}` block with one expression"));
        };
        let after_anchor = Anchor { item: context.renames.items.get(&at.item).cloned().unwrap_or_else(|| at.item.clone()), path: at.path.clone() };
        let actual = semilla_syntax::resolve_node(context.after, &after_anchor).map(|n| n.text.clone());
        if actual != Some(context.renames.apply(&at.item, &kept.children[0].text)) {
            return Outcome::refuted(format!("the expression after the step is not the `{keep}` branch"));
        }
        let explains = edits_inside(context, at, false);
        if explains.is_empty() {
            return Outcome::new(ClaimStatus::Empty);
        }
        return texts(context, &explains.clone(), Outcome::verified("shape", explains));
    };
    let Some(node) = semilla_syntax::resolve_node(context.before, at).filter(|node| node.kind == "if") else {
        return Outcome::malformed(format!("no `if` statement at `{at}` before the step"));
    };
    let Some(branch) = node.child(keep).filter(|branch| branch.kind == "block") else {
        return Outcome::refuted(format!("the `if` has no plain `{keep}` block"));
    };
    let block_anchor = Anchor { item: at.item.clone(), path: parent.clone() };
    let after_item = context.renames.items.get(&at.item).cloned().unwrap_or_else(|| at.item.clone());
    let (Some(old_block), Some(new_block)) = (semilla_syntax::resolve_node(context.before, &block_anchor), semilla_syntax::resolve_node(context.after, &Anchor { item: after_item, path: parent })) else {
        return Outcome::malformed(format!("no block around `{at}`"));
    };
    let apply = |text: &str| context.renames.apply(&at.item, text);
    let mut expected: Vec<String> = Vec::new();
    for (position, child) in old_block.children.iter().enumerate() {
        if position == index {
            expected.extend(branch.children.iter().map(|c| apply(&c.text)));
        } else {
            expected.push(apply(&child.text));
        }
    }
    let actual: Vec<String> = new_block.children.iter().map(|c| c.text.clone()).collect();
    if expected != actual {
        return Outcome::refuted(format!("the block after the step is not the block with the `{keep}` branch in place of the `if`"));
    }
    let explains = edits_inside(context, &block_anchor, false);
    if explains.is_empty() {
        return Outcome::new(ClaimStatus::Empty);
    }
    texts(context, &explains.clone(), Outcome::verified("shape", explains))
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
    // The arm ops are exact: the arm lists differ by one new arm, one removed arm, or the order.
    if matches!(claim.op.as_str(), "add-arm" | "remove-arm" | "move-arm") {
        let after_anchor = Anchor { item: context.renames.items.get(&at.item).cloned().unwrap_or_else(|| at.item.clone()), path: at.path.clone() };
        let arms = |text: &str| -> Option<Vec<String>> {
            match syn::parse_str::<syn::Expr>(text).ok()? {
                syn::Expr::Match(m) => Some(m.arms.iter().map(|arm| quote::ToTokens::to_token_stream(arm).to_string().trim_end_matches(" ,").to_string()).collect()),
                _ => None,
            }
        };
        let old = arms(&context.renames.apply(&at.item, &before.text));
        let new = semilla_syntax::resolve_node(context.after, &after_anchor).and_then(|node| arms(&node.text));
        let (Some(old), Some(new)) = (old, new) else {
            return Outcome::refuted(format!("no `match` at `{at}` on both sides"));
        };
        let one_more = |short: &[String], long: &[String]| long.len() == short.len() + 1 && (0..long.len()).any(|skip| {
            let mut rest = long.to_vec();
            rest.remove(skip);
            rest == short
        });
        let exact = match claim.op.as_str() {
            "add-arm" => one_more(&old, &new),
            "remove-arm" => one_more(&new, &old),
            _ => {
                let (mut a, mut b) = (old.clone(), new.clone());
                a.sort();
                b.sort();
                a == b && old != new
            }
        };
        if !exact {
            return Outcome::refuted(format!("the arms of the `match` changed in another way than `{}`: claim the other changes on their own", claim.op));
        }
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
    let count = claim.args.get("count").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
    if count.is_some_and(|count| count as usize != moved.len()) {
        return Outcome::refuted(format!("`count` is {}, but {} statements moved", count.unwrap_or(0), moved.len()));
    }
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
    // `extract-variable name`: the region has a new `let` with that name.
    if claim.op == "extract-variable" {
        let name = claim.arg_str("name").unwrap_or("");
        let named = explains.iter().any(|&i| {
            let edit = &context.edits[i];
            edit.node_after == Some("let") && edit.text_after.as_deref().is_some_and(|text| {
                let words: Vec<&str> = text.split(' ').collect();
                words.first() == Some(&"let") && (words.get(1) == Some(&name) || (words.get(1) == Some(&"mut") && words.get(2) == Some(&name)))
            })
        });
        if !named {
            return Outcome::refuted(format!("the region has no new `let {name}`"));
        }
    }
    let outcome = texts(context, &explains.clone(), Outcome::verified("shape", explains));
    body_proof(context, &at.item, outcome)
}

/// Expression ops that match one edit at the anchor (or one level below it).
fn check_expression(context: &Context, claim: &Claim, shape: impl Fn(&Edit, &Edit) -> bool) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let explains: Vec<usize> = edits_inside(context, at, false).into_iter().filter(|&i| context.edits[i].before.as_ref().is_some_and(|before| at.depth_to(before).is_some_and(|d| d <= 1))).collect();
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
    // A const with a literal value is not a free variable: put its value in the condition, so that
    // the counterexample is a real input.
    let (before, after) = (with_const_values(context.before, &before), with_const_values(context.after, &after));
    let types = local_types(context.before, &condition.item);
    // A plain negation (`C` to `!(C)`) is `negate` by its syntax. The solver is not necessary, so
    // the label does not change with the terms of the condition.
    let check = match plain_negation(&before, &after) {
        true => Some(crate::EffectCheck { effect: Effect::Negate, witness: None }),
        false => context.exprs.classify(&before, &after, &types),
    };
    if let Some(check) = check {
        // The effect is a hint for the reviewer, not a proof (design 8.8.1). Only a definite
        // answer shows: `narrow`, `widen`, and `negate` change which branch runs. `equivalent`
        // does not show: only the MIR proves that a body has no change. `unrelated` does not
        // show: it can be an equivalence that the solver cannot see (method calls, floats).
        match check.effect {
            Effect::Equivalent => outcome.class = Some(Class::Neutral),
            Effect::Narrow | Effect::Widen | Effect::Negate => {
                outcome.class = Some(Class::ControlFlow);
                outcome.effect = Some(check.effect);
                outcome.witness = check.witness;
            }
            Effect::Unrelated => {}
        }
    }
    outcome.before = Some(before);
    outcome.after = Some(after);
}

/// True if one side of a condition change is the `!` of the other side (`C` and `!(C)`), by the
/// tokens. Parentheses around a side are not a difference.
fn plain_negation(before: &str, after: &str) -> bool {
    fn bare(mut expr: syn::Expr) -> syn::Expr {
        loop {
            expr = match expr {
                syn::Expr::Paren(inner) => *inner.expr,
                syn::Expr::Group(inner) => *inner.expr,
                other => return other,
            };
        }
    }
    let side = |text: &str| syn::parse_str::<syn::Expr>(text).ok().map(bare);
    let (Some(before), Some(after)) = (side(before), side(after)) else { return false };
    let text = |expr: &syn::Expr| quote::ToTokens::to_token_stream(expr).to_string();
    let negates = |outer: &syn::Expr, inner: &syn::Expr| match outer {
        syn::Expr::Unary(syn::ExprUnary { op: syn::UnOp::Not(_), expr, .. }) => text(&bare((**expr).clone())) == text(inner),
        _ => false,
    };
    negates(&after, &before) || negates(&before, &after)
}

/// For the tests: [`with_const_values`].
#[doc(hidden)]
pub fn const_values_for_tests(workspace: &Workspace, text: &str) -> String {
    with_const_values(workspace, text)
}

/// Replaces each name of a const with a literal value (`DAILY_LIMIT` → `50_000`) in a token text.
fn with_const_values(workspace: &Workspace, text: &str) -> String {
    let values: BTreeMap<String, String> = workspace
        .items
        .values()
        .filter(|item| item.kind == semilla_syntax::ItemKind::Const)
        .filter_map(|item| {
            let item_const = syn::parse_str::<syn::ItemConst>(&item.text).ok()?;
            matches!(item_const.expr.as_ref(), syn::Expr::Lit(_)).then(|| (item_const.ident.to_string(), quote::ToTokens::to_token_stream(&item_const.expr).to_string()))
        })
        .collect();
    if values.is_empty() {
        return text.to_string();
    }
    text.split(' ').map(|word| values.get(word).cloned().unwrap_or_else(|| word.to_string())).collect::<Vec<_>>().join(" ")
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
    // B3: the fields of a parameter whose type is a struct of the crate (`account.cents: u64`),
    // so that the SMT check knows the integer width of a field read.
    let krate = item.path.split("::").next().unwrap_or("");
    let params: Vec<(String, String)> = types.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (name, ty) in params {
        let base = ty.trim_start_matches('&').trim_start_matches("mut").trim_start_matches('&');
        if base.is_empty() || !base.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':') {
            continue;
        }
        let last = base.rsplit("::").next().unwrap_or(base);
        let structs: Vec<&semilla_syntax::Item> = workspace.items.values().filter(|i| i.kind == semilla_syntax::ItemKind::Struct && i.path.starts_with(&format!("{krate}::")) && i.path.rsplit("::").next() == Some(last)).collect();
        let [owner] = structs.as_slice() else { continue };
        let prefix = format!("{}.", owner.path);
        for field in workspace.items.values().filter(|i| i.kind == semilla_syntax::ItemKind::Field && i.path.starts_with(&prefix)) {
            let field_name = &field.path[prefix.len()..];
            if field_name.contains('.') || field_name.contains("::") {
                continue;
            }
            let field_ty = field.signature.replace(' ', "");
            types.entry(format!("{name} . {field_name}")).or_insert_with(|| field_ty.clone());
            types.entry(format!("{name}.{field_name}")).or_insert(field_ty);
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
        // `wrap-expression with`: the wrapper is the named call, operator, or method.
        let named = claim.arg_str("with").is_none_or(|with| {
            let with = with.trim();
            outer.starts_with(&format!("{with} (")) || outer.starts_with(&format!("{with} ")) || (with == "?" && outer.ends_with(" ?")) || outer_attr.as_deref() == Some(with.trim_start_matches('.'))
        });
        wraps && named && method.as_ref().is_none_or(|m| outer_kind == Some("method_call") && outer_attr.as_deref() == Some(m.as_str()))
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
    anchor.is_some_and(|anchor| anchor.item == item || is_child(&anchor.item, item))
}

/// True if an edit is the member list of the impl that owns `item` (a new or removed method).
fn is_owner_members(edit: &Edit, item: &str) -> bool {
    edit.kind == EditKind::Members && item.contains("::") && {
        let owner = item.rsplit_once("::").map(|(owner, _)| owner).unwrap_or("");
        let owner = without_twin(owner);
        edit.anchor().is_some_and(|anchor| {
            let impl_path = without_twin(&anchor.item);
            impl_path.ends_with(&format!("impl {}", last_segment(owner))) || impl_path.contains(&format!("impl {owner}"))
        })
    }
}

/// `rewrite-item`: a free edit of a whole item, with an optional new path. It explains every edit
/// of the item. The reviewer sees the old and the new item side by side.
fn check_rewrite(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    let new = claim.arg_str("new").map(|new| full_path(&at.item, new)).unwrap_or_else(|| at.item.clone());
    let Some(old_item) = context.before.items.get(&at.item) else {
        return Outcome::malformed(format!("no item `{}` before the step{}", at.item, crate::name_hint(&at.item, context.before)));
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
        let mut outcome = Outcome::new(ClaimStatus::Empty);
        // With `new`, the name map pairs the old and the new item. Equal text means a pure move or
        // rename, which is an identity claim, not a rewrite.
        if new != at.item {
            outcome.hint = Some(format!("`{}` has the same text at `{new}`: this is a move or a rename, not a rewrite. Remove the claim, or claim `move-item` (another module or crate) or `rename`. A step with no other change gets the proof.", at.item));
        }
        return outcome;
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

/// The answer of [`live_call`].
enum LiveCall {
    /// A block that the test can reach has a call path to the function.
    Found,
    /// The MIR has a call path, but not through the reachable blocks.
    Dead,
    /// The MIR has no call path.
    None,
    /// The MIR has no body of the test.
    NoTest,
}

/// Finds a call path from the test item `test` to the item `target` in the MIR (batch 5f, A2).
///
/// The walk starts at the body of the test in each target that has it. It follows the live
/// callees of each body: the calls in the blocks that the start block reaches after the constant
/// conditions fold. The driver puts the calls of a closure in the body that makes the closure.
/// A callee that is not in the same target is a function of another crate of the workspace.
fn live_call(facts: &BTreeMap<String, crate::ItemFacts>, test: &str, target: &str) -> LiveCall {
    // The body of an item: a body with no other body of the item above it (a nested function
    // has its own calls).
    let top = |item: &str, name: &str| facts.get(item).is_some_and(|f| !f.live.iter().any(|(other, _, _)| name.starts_with(&format!("{other}::"))));
    let mut bodies: BTreeMap<(&str, &str), Vec<&[String]>> = BTreeMap::new();
    let mut items: BTreeMap<&str, &str> = BTreeMap::new();
    let mut targets: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (item, found) in facts {
        for (name, target, live) in &found.live {
            bodies.entry((target.as_str(), name.as_str())).or_default().push(live.as_slice());
            targets.entry(name.as_str()).or_default().insert(target.as_str());
            if top(item, name) {
                items.insert(name.as_str(), item.as_str());
            }
        }
    }
    let Some(start) = facts.get(test) else { return LiveCall::NoTest };
    let mut todo: Vec<(&str, &str)> = start.live.iter().filter(|(name, _, _)| top(test, name)).map(|(name, target, _)| (target.as_str(), name.as_str())).collect();
    if todo.is_empty() {
        return LiveCall::NoTest;
    }
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    while let Some(at) = todo.pop() {
        if !seen.insert(at) {
            continue;
        }
        for callee in bodies.get(&at).into_iter().flatten().flat_map(|live| live.iter()) {
            if callee == target || items.get(callee.as_str()) == Some(&target) {
                return LiveCall::Found;
            }
            if bodies.contains_key(&(at.0, callee.as_str())) {
                todo.push((at.0, callee.as_str()));
            } else {
                todo.extend(targets.get(callee.as_str()).into_iter().flatten().map(|other| (*other, callee.as_str())));
            }
        }
    }
    // The call graph of the MIR with each call, also the calls in code that cannot run.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut todo = vec![test];
    while let Some(item) = todo.pop() {
        if !seen.insert(item) {
            continue;
        }
        if item == target {
            return LiveCall::Dead;
        }
        todo.extend(facts.get(item).into_iter().flat_map(|f| f.callees.iter().map(String::as_str)));
    }
    LiveCall::None
}

/// True if each run of the test item `test` that gets to its end calls the item `target`
/// (batch 5w, part C). The driver gives the sure callees of each body: the calls on each path
/// from the start of the body to a normal return. The same rule holds at each level: the test
/// has a sure call to a local function, and that function has a sure call to the target.
///
/// A body name can exist in more than one target. A callee is sure only if each body with the
/// name has the call.
///
/// Batch 5y, A4: with `rule`, an end of the process that is not a panic is an end of the test
/// too (`std::process::exit`, a call with no return that is not panic code, a function that
/// can get to such a call). No such end can run before the sure call, at each level. With
/// `rule` true (a `#[should_panic]` test), a panic is an end too: no call and no assert can
/// run before the sure call. `cause` gets the first cause that stops a sure call.
fn sure_call(facts: &BTreeMap<String, crate::ItemFacts>, test: &str, target: &str, rule: Option<bool>, cause: &mut Option<String>) -> bool {
    type Ends = BTreeMap<String, Vec<String>>;
    let top = |item: &str, name: &str| facts.get(item).is_some_and(|f| !f.sure.iter().any(|(other, _, _)| name.starts_with(&format!("{other}::"))));
    let mut ends: BTreeMap<(&str, &str), Vec<&Ends>> = BTreeMap::new();
    let mut named: BTreeMap<&str, Vec<&Ends>> = BTreeMap::new();
    for found in facts.values() {
        for ((name, target, _), map) in found.sure.iter().zip(&found.ends) {
            ends.entry((target.as_str(), name.as_str())).or_default().push(map);
            named.entry(name.as_str()).or_default().push(map);
        }
    }
    // The first end of the process that the function `name` can get to: the call and the
    // function that has it.
    fn can_end<'a>(named: &BTreeMap<&'a str, Vec<&'a Ends>>, name: &'a str, seen: &mut BTreeSet<&'a str>) -> Option<(String, String)> {
        if !seen.insert(name) {
            return None;
        }
        let maps = named.get(name)?;
        if let Some(end) = maps.iter().find_map(|map| map.get("ends").and_then(|list| list.first())) {
            return Some((end.clone(), name.to_string()));
        }
        maps.iter().flat_map(|map| map.get("calls").into_iter().flatten()).find_map(|callee| can_end(named, callee, seen))
    }
    let mut bodies: BTreeMap<(&str, &str), Vec<&[String]>> = BTreeMap::new();
    let mut items: BTreeMap<&str, &str> = BTreeMap::new();
    let mut targets: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (item, found) in facts {
        for (name, target, sure) in &found.sure {
            bodies.entry((target.as_str(), name.as_str())).or_default().push(sure.as_slice());
            targets.entry(name.as_str()).or_default().insert(target.as_str());
            if top(item, name) {
                items.insert(name.as_str(), item.as_str());
            }
        }
    }
    struct Walk<'a> {
        bodies: BTreeMap<(&'a str, &'a str), Vec<&'a [String]>>,
        items: BTreeMap<&'a str, &'a str>,
        targets: BTreeMap<&'a str, BTreeSet<&'a str>>,
        target: &'a str,
        open: BTreeSet<(&'a str, &'a str)>,
        rule: Option<bool>,
        ends: BTreeMap<(&'a str, &'a str), Vec<&'a BTreeMap<String, Vec<String>>>>,
        named: BTreeMap<&'a str, Vec<&'a BTreeMap<String, Vec<String>>>>,
        cause: Option<String>,
    }
    impl<'a> Walk<'a> {
        /// The cause, if the body `at` can end the run before its sure call of `callee`.
        fn early_end(&self, at: (&'a str, &'a str), callee: &'a str) -> Option<String> {
            let panic_end = self.rule?;
            let maps = self.ends.get(&at).cloned().unwrap_or_default();
            for map in &maps {
                for entry in map.get(&format!("before {callee}")).into_iter().flatten() {
                    if let Some(end) = entry.strip_prefix("end ") {
                        return Some(format!("the test can end the process before the call: `{end}` in `{}`", at.1));
                    }
                    if entry != callee {
                        if let Some((end, function)) = can_end(&self.named, entry, &mut BTreeSet::new()) {
                            return Some(format!("the test can end the process before the call: `{end}` in `{function}`"));
                        }
                    }
                }
            }
            if panic_end && !(!maps.is_empty() && maps.iter().all(|map| map.get("first").is_some_and(|list| list.iter().any(|name| name == callee)))) {
                return Some(format!("the test has `#[should_panic]`, and a panic can end the test before the call (`{}` has a call or a check before its call of `{callee}`)", at.1));
            }
            None
        }

        fn sure(&mut self, at: (&'a str, &'a str)) -> bool {
            // A cycle of calls gives no proof.
            if !self.open.insert(at) {
                return false;
            }
            let lists = self.bodies.get(&at).cloned().unwrap_or_default();
            let callees: Vec<&'a str> = match lists.split_first() {
                Some((first, rest)) => first.iter().filter(|callee| rest.iter().all(|list| list.contains(callee))).map(String::as_str).collect(),
                None => Vec::new(),
            };
            let mut found = false;
            for callee in callees {
                if callee == self.target || self.items.get(callee) == Some(&self.target) {
                    found = true;
                } else if self.bodies.contains_key(&(at.0, callee)) {
                    found = self.sure((at.0, callee));
                } else {
                    // A function of a different target: each body with the name.
                    let others: Vec<&'a str> = self.targets.get(callee).into_iter().flatten().copied().collect();
                    found = !others.is_empty() && others.into_iter().all(|other| self.sure((other, callee)));
                }
                if found {
                    // 5y, A4: the call is sure only if no end of the run is before it.
                    match self.early_end(at, callee) {
                        Some(problem) => {
                            self.cause.get_or_insert(problem);
                            found = false;
                        }
                        None => break,
                    }
                }
            }
            self.open.remove(&at);
            found
        }
    }
    let Some(start) = facts.get(test) else { return false };
    let starts: Vec<(&str, &str)> = start.sure.iter().filter(|(name, _, _)| top(test, name)).map(|(name, target, _)| (target.as_str(), name.as_str())).collect();
    let mut walk = Walk { bodies, items, targets, target, open: BTreeSet::new(), rule, ends, named, cause: None };
    let sure = !starts.is_empty() && starts.into_iter().all(|at| walk.sure(at));
    *cause = walk.cause;
    sure
}

/// The property claims (design section 21.4): facts about the code after the step.
fn check_property(context: &Context, claim: &Claim) -> Outcome {
    let at = claim.at.as_ref().unwrap();
    if !at.path.is_empty() {
        return Outcome::malformed(format!("`{}` is a claim on an item; use the item anchor `{}`", claim.op, at.item));
    }
    let Some(item) = context.after.items.get(&at.item) else {
        return Outcome::malformed(format!("no item `{}` after the step{}", at.item, crate::name_hint(&at.item, context.after)));
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
    // The scope of the property (design 25): each local body that can run, with the trait
    // impls and the `Drop` impls that the code of another crate runs.
    let found = context.facts.map(|facts| (facts, crate::scope::scope(&at.item, &reached, facts, context.after)));
    let quoted = |found: &[String]| list(&found.iter().map(|path| format!("`{path}`")).collect::<Vec<_>>());
    let outcome = match claim.op.as_str() {
        "no-unsafe" => {
            let unsafe_in = |items: &BTreeSet<String>| -> Vec<String> {
                items.iter().filter_map(|path| context.after.items.get(path)).filter(|item| crate::hints::words(&item.text).contains(&"unsafe")).map(|item| item.path.clone()).collect()
            };
            let bad = unsafe_in(found.as_ref().map(|(_, scope)| &scope.items).unwrap_or(&reached));
            if !bad.is_empty() {
                return Outcome::refuted(format!("unsafe code in {}", list(&bad)));
            }
            let Some((facts, scope)) = &found else {
                return Outcome::new(ClaimStatus::Pending { reason: "the trait impls and the `Drop` impls that can run come from the MIR in phase 2".into() });
            };
            let mut outcome = Outcome::verified(if scope.gaps.is_empty() { "mir" } else { "partial" }, Vec::new());
            outcome.after = Some(format!("no `unsafe` in the local code that can run; {}; Semilla does not look for `unsafe` in the code of other crates", scope.text(facts, &[])));
            return outcome;
        }
        "no-panic" => {
            let sites_in = |items: &BTreeSet<String>| -> Vec<String> { items.iter().filter_map(|path| context.after.items.get(path)).flat_map(item_panic_sites).collect() };
            let explicit = sites_in(found.as_ref().map(|(_, scope)| &scope.items).unwrap_or(&reached));
            if !explicit.is_empty() {
                return Outcome::refuted(format!("explicit panic sites: {}", list(&explicit)));
            }
            // The implicit panic sites are MIR `Assert` terminators: a division, an index, an
            // overflow check. They come from phase 2.
            let Some((facts, scope)) = &found else {
                return Outcome::new(ClaimStatus::Pending { reason: "the implicit panic sites (division, bounds, overflow) come from the MIR in phase 2".into() });
            };
            let mut sites: Vec<String> = Vec::new();
            for item in &scope.items {
                let Some(item_facts) = facts.get(item) else { continue };
                let calls = item_facts.externals.iter().filter(|path| is_panic_call(path)).map(|path| format!("a call to `{path}`"));
                for kind in item_facts.panics.iter().cloned().chain(calls) {
                    // A body can be in more than one target (the lib and its test build).
                    let site = format!("{kind} {}", scope.place(item));
                    if !sites.contains(&site) {
                        sites.push(site);
                    }
                }
            }
            if !sites.is_empty() {
                let overflow = sites.iter().any(|site| site.starts_with("overflow"));
                let note = if overflow { " (the checked build is the dev profile of the tree: it has overflow checks)" } else { "" };
                return Outcome::refuted(format!("implicit panic sites: {}{note}", list(&sites)));
            }
            // A function of another crate can panic. Semilla does not examine it.
            // The source has the name `std::hint::black_box`. The MIR has the intrinsic that
            // the function calls. The text shows the name of the source (batch 5w, D6).
            let source_name = |path: &String| match path.as_str() {
                "std::intrinsics::black_box" => "std::hint::black_box".to_string(),
                "core::intrinsics::black_box" => "core::hint::black_box".to_string(),
                _ => path.clone(),
            };
            let externals: Vec<String> = scope.externals.keys().map(source_name).collect::<BTreeSet<_>>().into_iter().collect();
            let mut more = Vec::new();
            if !externals.is_empty() {
                more.push(format!("{} call(s) to other crates, and they can panic: {}", externals.len(), quoted(&externals)));
            }
            if !scope.std_drops.is_empty() {
                more.push(format!("the destructors of {}", quoted(&scope.std_drops.keys().cloned().collect::<Vec<_>>())));
            }
            let partial = !more.is_empty() || !scope.gaps.is_empty();
            let mut outcome = Outcome::verified(if partial { "partial" } else { "mir" }, Vec::new());
            let start = if partial { "only partly verified: no panic site in the local code that can run" } else { "no panic site in the code that can run: no explicit panic, no MIR assert (division, bounds, overflow in the dev build), and no call to another crate" };
            outcome.after = Some(format!("{start}; {}", scope.text(facts, &more)));
            return outcome;
        }
        "tested-by" => {
            fn attr_path(attr: &str) -> String {
                let inner = attr.trim_start_matches('#').trim_start().trim_start_matches('[');
                inner.split(['(', ']', '=']).next().unwrap_or("").replace(' ', "")
            }
            fn has_test_attr(item: &semilla_syntax::Item) -> bool {
                item.kind == semilla_syntax::ItemKind::Fn && item.attrs.iter().any(|attr| attr_path(attr).rsplit("::").next() == Some("test"))
            }
            let test = claim.arg_str("test").unwrap();
            let path = if context.after.items.contains_key(test) { test.to_string() } else {
                // A last name: one test of the same crate with that name, or one test of the
                // workspace with that name (trial 4, D10).
                let krate = at.item.split("::").next().unwrap_or("");
                let named: Vec<&String> = context.after.items.values().filter(|i| has_test_attr(i) && last_segment(&i.path) == test).map(|i| &i.path).collect();
                let local: Vec<&String> = named.iter().copied().filter(|path| path.starts_with(&format!("{krate}::"))).collect();
                match (local.as_slice(), named.as_slice()) {
                    ([one], _) | (_, [one]) => one.to_string(),
                    (_, []) => return Outcome::malformed(format!("no test `{test}` in the workspace after the step")),
                    _ => return Outcome::malformed(format!("more than one test is named `{test}`: {}. Use the full path", quoted(&named.iter().map(|path| path.to_string()).collect::<Vec<_>>()))),
                }
            };
            // A2 (trial 3, A10): the named function must be a test that runs: it has `#[test]`
            // (or a test attribute of a test framework) and no `#[ignore]`. A helper function in
            // a test module is not a test.
            let Some(item) = context.after.items.get(&path).filter(|i| i.kind == semilla_syntax::ItemKind::Fn) else {
                return Outcome::malformed(format!("`{path}` is not a test: it is not a function"));
            };
            if !has_test_attr(item) {
                return Outcome::malformed(format!("`{path}` is not a test: it has no `#[test]` attribute, so `cargo test` does not run it"));
            }
            if item.attrs.iter().any(|attr| attr_path(attr) == "ignore" || (attr_path(attr) == "cfg_attr" && attr.contains("ignore"))) {
                return Outcome::malformed(format!("the test `{path}` has `#[ignore]`, so it does not run; remove `#[ignore]`, or name a test that runs"));
            }
            let in_syntax = reach(context.calls, &path).contains(&at.item);
            // 5f, A2: the call must be in code that the test can reach. The MIR of the test
            // gives the calls in the blocks that the start block reaches after the constant
            // conditions fold (`if false { f() }` has no such call).
            let Some(facts) = context.facts else {
                if !in_syntax {
                    return Outcome::refuted(format!("the test `{path}` does not call `{}`", at.item));
                }
                return Outcome::new(ClaimStatus::Pending { reason: "the reachable calls of the test come from the MIR in the background check (phase 2)".into() });
            };
            match live_call(facts, &path, &at.item) {
                // 5w, part C: `verified` only if each path of the test from its start to its
                // normal return has the call. A call under a condition, in a loop, or after
                // an early return is a reachable call that a run of the test can miss.
                LiveCall::Found if sure_call(facts, &path, &at.item, None, &mut None) => {
                    // 5y, A4: an end of the process that is not a panic is an end of the test.
                    // In a `#[should_panic]` test, a panic is an end too.
                    let should_panic = item.attrs.iter().any(|attr| attr_path(attr) == "should_panic" || (attr_path(attr) == "cfg_attr" && attr.contains("should_panic")));
                    let mut cause = None;
                    if !sure_call(facts, &path, &at.item, Some(should_panic), &mut cause) {
                        let cause = cause.unwrap_or_else(|| "the test can end before the call".to_string());
                        let mut outcome = Outcome::verified("partial", Vec::new());
                        outcome.after = Some(format!("only partly verified: the test `{path}` calls `{}` on each path from its start to its normal return, but {cause}. Semilla did not prove that the test runs the call.", at.item));
                        return outcome;
                    }
                    let mut outcome = Outcome::verified("mir", Vec::new());
                    outcome.after = Some(format!("the test `{path}` calls `{}` on each path from its start to its normal return. Semilla does not run the test and does not check what the test asserts.", at.item));
                    return outcome;
                }
                LiveCall::Found => {
                    let mut outcome = Outcome::verified("partial", Vec::new());
                    outcome.after = Some(format!("only partly verified: the test `{path}` has a reachable call to `{}`, but not on each path from its start to its normal return (the call is under a condition, in a loop, after an early return, or in a closure that a different function calls). Semilla did not prove that the test runs the call. Semilla does not run the test and does not check what the test asserts.", at.item));
                    return outcome;
                }
                LiveCall::NoTest => Outcome::refuted(format!("the compiler gives no body for the test `{path}` (a condition such as `#[cfg(…)]` can remove it), so Semilla cannot show a reachable call to `{}`", at.item)),
                LiveCall::Dead => Outcome::refuted(format!("the test `{path}` has a call to `{}` only in code that cannot run", at.item)),
                LiveCall::None if in_syntax => Outcome::refuted(format!("the test `{path}` has no reachable direct call to `{}`. Semilla follows the direct calls and the closures of the test. It does not follow a call through a trait object or a function pointer", at.item)),
                LiveCall::None => Outcome::refuted(format!("the test `{path}` does not call `{}`", at.item)),
            }
        }
        "no-io" | "calls-only" => {
            // A4: each entry of `calls` is a path of full segments. An empty entry or an entry
            // that is not a path allows nothing clear: the claim is malformed.
            let mut allowed: Vec<String> = Vec::new();
            if claim.op == "calls-only" {
                let entries: Vec<String> = match claim.args.get("calls") {
                    Some(serde_json::Value::Array(values)) => values.iter().map(|value| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string())).collect(),
                    Some(serde_json::Value::String(value)) => value.split(',').map(|part| part.trim().to_string()).collect(),
                    Some(other) => vec![other.to_string()],
                    None => Vec::new(),
                };
                for entry in entries {
                    let path = entry.strip_suffix("::").unwrap_or(&entry);
                    let valid = !path.is_empty() && path.split("::").all(|segment| !segment.is_empty() && !segment.starts_with(|c: char| c.is_ascii_digit()) && segment.chars().all(|c| c.is_alphanumeric() || c == '_'));
                    if !valid {
                        return Outcome::malformed(format!("the entry `{entry}` of `calls` is not a path. Each entry is a path of full segments, for example `core` or `std::iter`. It allows the path itself and each path below it."));
                    }
                    allowed.push(path.to_string());
                }
            }
            let Some((facts, scope)) = &found else {
                return Outcome::new(ClaimStatus::Pending { reason: "the external calls come from the MIR in the background check (phase 2)".into() });
            };
            // `calls-only`: each path of the printed name (the type and the trait of
            // `<T as Trait>::f`, or the path of the function) must be below an allowed entry.
            // The printed path can be a re-export (`std::iter::Iterator::sum` is a function of
            // `core`). An entry can name the one path or the other: they are the same function.
            let allows = |entries: &[String], printed: &str, def: &str| calls_allow(entries, printed) || (!def.is_empty() && calls_allow(entries, def));
            let mut bad: Vec<String> = Vec::new();
            let mut unknown: Vec<String> = Vec::new();
            // `calls-only`: the entries that cover the calls and the drops outside the list.
            let mut missing: Option<BTreeSet<String>> = Some(BTreeSet::new());
            for (printed, (krate, def, item)) in &scope.externals {
                if claim.op == "calls-only" {
                    // `core` and `alloc` are always allowed (design 26.3). The crate that defines
                    // the function decides, as for `no-io`: a re-export in `std` changes nothing.
                    if matches!(krate.as_str(), "core" | "alloc") {
                        continue;
                    }
                    if !allows(&allowed, printed, def) {
                        bad.push(format!("`{printed}` {}", scope.place(item)));
                        let entries = calls_entries(printed);
                        match (&mut missing, entries.is_empty()) {
                            (Some(missing), false) => missing.extend(entries),
                            _ => missing = None,
                        }
                    }
                    continue;
                }
                match crate::scope::io_class(krate, def) {
                    crate::scope::Io::No => {}
                    crate::scope::Io::Yes => bad.push(format!("`{printed}` {}", scope.place(item))),
                    crate::scope::Io::NotExamined => unknown.push(printed.clone()),
                }
            }
            // A drop of a value of a `std` type runs the code of `std`: a `File` closes.
            for (ty, item) in &scope.std_drops {
                let fine = if claim.op == "calls-only" { allowed.iter().any(|entry| crate::scope::has_prefix(ty, entry)) } else { crate::scope::io_class("std", ty) == crate::scope::Io::No };
                if !fine {
                    // D9: a type that only a call gives to generic code has the call as its
                    // cause. The body does not drop a value of it.
                    match scope.std_drop_calls.get(ty).filter(|_| !scope.std_drop_real.contains(ty)) {
                        Some((call, item)) => bad.push(format!("a call of `{call}` with the type `{ty}` {} (the call can run the code of this type of `std`: a trait impl or the destructor)", scope.place(item))),
                        None => bad.push(format!("a drop of a value of `{ty}` {}", scope.place(item))),
                    }
                    if let Some(missing) = &mut missing {
                        missing.insert(module_path(ty, false));
                    }
                }
            }
            if !bad.is_empty() && claim.op == "calls-only" {
                let mut outcome = Outcome::refuted(format!("external calls outside the list of `calls`: {} (calls to `core` and `alloc` are always allowed)", list(&bad)));
                // The complete list: the entries of the claim, and the missing entries. An entry
                // below another entry adds nothing. The list must pass the same check as a list
                // of the agent: if it does not, Semilla gives no list.
                let mut complete = allowed.clone();
                complete.extend(missing.unwrap_or_default());
                let all = complete.clone();
                let mut seen = BTreeSet::new();
                complete.retain(|entry| seen.insert(entry.clone()) && !all.iter().any(|other| other != entry && crate::scope::has_prefix(entry, other)));
                // An entry that covers no call and no drop adds nothing (batch 5l, B2): an
                // entry of the agent for code that the function does not call, or for `core`.
                complete.retain(|entry| {
                    let covers = |path: &str| calls_heads(path).iter().any(|head| crate::scope::has_prefix(head, entry));
                    scope.externals.iter().any(|(printed, (krate, def, _))| !matches!(krate.as_str(), "core" | "alloc") && (covers(printed) || covers(def)))
                        || scope.std_drops.keys().any(|ty| crate::scope::has_prefix(ty, entry))
                });
                let covers = scope.externals.iter().all(|(printed, (krate, def, _))| matches!(krate.as_str(), "core" | "alloc") || allows(&complete, printed, def))
                    && scope.std_drops.keys().all(|ty| complete.iter().any(|entry| crate::scope::has_prefix(ty, entry)));
                if covers {
                    outcome.complete_calls = complete;
                }
                return outcome;
            }
            if !bad.is_empty() {
                return Outcome::refuted(format!("external calls: {} (a function of `std` outside the allow list of modules counts as I/O)", list(&bad)));
            }
            let mut more = Vec::new();
            if !unknown.is_empty() {
                more.push(format!("{} call(s) to crates that are not the standard library: {}", unknown.len(), quoted(&unknown)));
            }
            let partial = !more.is_empty() || !scope.gaps.is_empty();
            let mut outcome = Outcome::verified(if partial { "partial" } else { "mir" }, Vec::new());
            let what = if claim.op == "no-io" {
                "no call to an I/O, process, network, or environment API in the code that can run (`core` and `alloc` do no I/O; `std` has an allow list of modules)"
            } else {
                "each call to another crate is below an allowed path; calls to `core` and `alloc` are always allowed"
            };
            outcome.after = Some(format!("{what}; {}", scope.text(facts, &more)));
            return outcome;
        }
        other => Outcome::malformed(format!("the verifier has no checker for `{other}`")),
    };
    outcome
}

/// The paths of a printed function name that `calls-only` compares: the type and the trait of
/// `<T as Trait>::f`, or the path of the function.
fn calls_heads(printed: &str) -> Vec<&str> {
    match printed.strip_prefix('<').and_then(|rest| rest.split_once(" as ")) {
        Some((ty, rest)) => [crate::scope::path_tokens(ty).first().copied(), crate::scope::path_tokens(rest).first().copied()].into_iter().flatten().filter(|token| token.contains("::")).collect(),
        None => crate::scope::path_tokens(printed).first().copied().into_iter().collect(),
    }
}

/// True when each compared path of `printed` is below an entry of `calls`.
fn calls_allow(entries: &[String], printed: &str) -> bool {
    let heads = calls_heads(printed);
    !heads.is_empty() && heads.iter().all(|head| entries.iter().any(|entry| crate::scope::has_prefix(head, entry)))
}

/// The module path of a path. A type or a trait loses its name: `std::fs::File` gives `std::fs`.
/// A function also loses its name first: `std::fs::File::open` and `std::fs::write` give
/// `std::fs`. The crate always stays.
fn module_path(path: &str, function: bool) -> String {
    let mut segments: Vec<&str> = path.split("::").collect();
    if function && segments.len() > 1 && segments.last().is_some_and(|last| !last.starts_with(|c: char| c.is_uppercase())) {
        segments.pop();
    }
    while segments.len() > 1 && segments.last().is_some_and(|last| last.starts_with(|c: char| c.is_uppercase())) {
        segments.pop();
    }
    segments.join("::")
}

/// The entries of `calls` that cover one external call: the shortest module path of each
/// compared path. `<std::fs::File as std::io::Write>::write_all` gives `std::fs` and `std::io`.
/// The result is empty when the printed name has no path to compare.
fn calls_entries(printed: &str) -> Vec<String> {
    let trait_form = printed.starts_with('<');
    calls_heads(printed)
        .into_iter()
        .map(|head| {
            // `std::f64::<impl f64>::sqrt`: the path before the impl block is a module.
            let module = !trait_form && printed.strip_prefix(head).is_some_and(|rest| rest.starts_with("::<impl"));
            if module { head.to_string() } else { module_path(head, !trait_form) }
        })
        .collect()
}

/// True for a call to a function of the standard library that panics.
fn is_panic_call(path: &str) -> bool {
    ["core::panicking::", "std::panicking::", "std::rt::panic", "std::rt::begin_panic", "core::panic::", "std::process::abort", "core::intrinsics::abort"].iter().any(|prefix| path.starts_with(prefix))
}

