//! The AST pass of the intent language (design 79): the first of the three passes of the
//! compiler of Semilla (the AST pass, the control-flow pass, the proof pass).
//!
//! Each changed node of the syntax diff of a step (an item, a statement, an expression, a field,
//! an arm, an attribute, a `use` line) gets exactly one map entry (R79.1-1): from an anchor of a
//! claim (R79.2-1 to R79.2-4), from the matcher of G2 or a rule of Semilla (R79.3-1 to
//! R79.3-5), or "not mapped" (R79.5-1). The pass is polynomial: the check of the anchors and the
//! matcher. It never searches for a map (R79.4-1). The proof pass uses the map (R79.7-1).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use semilla_core::namerec::{NameIndex, Target};
use semilla_core::{AstEntry, AstMap, AstState};
use semilla_syntax::{Edit, EditKind, ItemKind, Refactor, Workspace};

use crate::{BodyOracle, StepInput, StepOutcome};

/// [R79.2-1] The data of the compiled claims for the AST pass: what rustc resolved in the
/// intent crate (design 78). Semilla never reads an anchor from the text of a path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompiledClaims {
    /// The definition of each scope link (`before::shop::f` → `before::shop::f`, the record
    /// name with the facade); `None`: the path names no item of the version.
    pub links: BTreeMap<String, Option<String>>,
    /// The functions of the intent crate, by the path of their file and their name.
    pub functions: BTreeMap<(String, String), ClaimCalls>,
}

/// [R79.2-1] What one function of the intent crate calls and reads in each version, also through
/// its closures and the functions of the intent crate that it calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaimCalls {
    /// The definitions of each version that the function calls or names (the record names, with
    /// no facade): `[before, after]`.
    pub calls: [BTreeSet<String>; 2],
    /// The fields that the function reads in each version: the type as rustc prints it (with no
    /// facade and no generic arguments), the variant (empty for a struct), and the field (empty
    /// for a read of the variant only).
    pub fields: [BTreeSet<(String, String, String)>; 2],
}

/// The prefix of the facade crate of a version (design 78): 0 is the old tree, 1 the new tree.
fn facade(side: usize) -> &'static str {
    if side == 0 { "before::" } else { "after::" }
}

/// The words of a version in a message.
fn version_words(side: usize) -> &'static str {
    if side == 0 { "the old version" } else { "the new version" }
}

/// The rules of the pass.
const RULE_NODES: &str = "R79.1-1";
const RULE_ANCHOR: &str = "R79.2-3";
const RULE_RELATION: &str = "R79.2-4";
const RULE_MATCHER: &str = "R79.3-1";
pub(crate) const RULE_NO_CODE: &str = "R79.3-2";
const RULE_NO_READER: &str = "R79.3-3";
const RULE_NOT_REACHED: &str = "R79.3-4";
const RULE_USE: &str = "R79.3-5";
const RULE_CONTAINER: &str = "R79.3-6";
const RULE_REQUEST: &str = "R79.5-1";

/// The cause of a step with intent files and no compiled claims (phase 1 of a step: the compiled
/// check gives them in phase 2).
pub const NO_COMPILED: &str = "the anchors come from the claims that rustc compiles: the compiled check of the step gives them (phase 2)";

/// [R79.1-1] True for an edit of a node of a syntax tree. A file edit (a file that the loader
/// does not parse: a file that is not Rust, a binary file, a build script, a new mode) has no
/// node of a tree: it keeps its fact, and it is not a part of the AST diff.
pub fn is_node(edit: &Edit) -> bool {
    !matches!(edit.kind, EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged)
}

/// One anchor of a claim: the items of the two trees (the syntax paths) that it maps.
#[derive(Clone, Debug, Default)]
struct ClaimAnchor {
    id: String,
    old: Option<String>,
    new: Option<String>,
    /// True: the anchor maps each node inside its items (R79.2-3). False: a relation maps the
    /// two types and only the parts that it reads (R79.2-4).
    whole: bool,
    /// The fields and the variants of each tree that a relation reads.
    parts: [BTreeSet<String>; 2],
    rule: &'static str,
}

impl ClaimAnchor {
    /// True if the anchor maps the item of a node on one side (R79.2-3, R79.2-4).
    fn covers(&self, side: usize, item: &str, tree: &Workspace) -> bool {
        let root = if side == 0 { &self.old } else { &self.new };
        if let Some(root) = root {
            if item == root {
                return true;
            }
            if self.whole && inside(tree, root, item) {
                return true;
            }
        }
        self.parts[side].contains(item)
    }
}

/// True if an item is inside the item `root` of one tree: a field or a variant of a type, or an
/// item in the body of a function, a constant, or a static.
fn inside(tree: &Workspace, root: &str, item: &str) -> bool {
    if item.starts_with(&format!("{root}.")) {
        return true;
    }
    if !item.starts_with(&format!("{root}::")) {
        return false;
    }
    let root_kind = tree.items.get(root).map(|found| found.kind);
    let item_kind = tree.items.get(item).map(|found| found.kind);
    matches!(root_kind, Some(ItemKind::Fn | ItemKind::Const | ItemKind::Static)) || matches!(item_kind, Some(ItemKind::Variant | ItemKind::Field))
}

/// [R79.1-1] Runs the AST pass of a step: one entry for each changed node of the syntax diff.
pub(crate) fn apply(input: &StepInput, bodies: &dyn BodyOracle, outcome: &mut StepOutcome) {
    let mut map = AstMap::default();
    let anchors = if input.step.agent.intent.is_empty() {
        Vec::new()
    } else {
        match bodies.intent_anchors(input.step) {
            Some(Ok(data)) => claim_anchors(input, outcome, &data, &mut map),
            Some(Err(cause)) => {
                map.gap = Some(format!("the claims give no anchor: {cause}"));
                Vec::new()
            }
            None => {
                map.gap = Some(format!("the claims give no anchor: {NO_COMPILED}"));
                Vec::new()
            }
        }
    };
    let pure = Pure::new(input, outcome, &anchors);
    // [R79.1-1] The count of the changed nodes, apart from the loop of the entries.
    map.nodes = outcome.edits.iter().filter(|edit| is_node(edit)).count();
    let nodes: Vec<&Edit> = outcome.edits.iter().filter(|edit| is_node(edit)).collect();
    // [R79.5-1] One template for each item of a side: the nodes of one item share it.
    let mut templates: BTreeMap<(bool, String), String> = BTreeMap::new();
    let mut pending = crate::template::Pending::default();
    for edit in &nodes {
        let mut found = entry(input, edit, &anchors, &pure);
        if found.state == AstState::NotMapped && found.template.is_empty() {
            let added = matches!(edit.kind, EditKind::ItemAdded | EditKind::Inserted);
            let item = if added { &edit.after } else { &edit.before }.as_ref().map(|anchor| anchor.item.clone()).unwrap_or_default();
            found.template = templates.entry((added, item)).or_insert_with(|| template(input, outcome, edit, &mut pending)).clone();
        }
        map.entries.push(found);
    }
    containers(input, &nodes, &mut map.entries);
    // [R90.2-1] The kind of each node from the MIR alignment (design 90). A node that the
    // projection decides is mapped; a node with no unique answer gets its request (R90.6-1).
    let indices: Vec<usize> = outcome.edits.iter().enumerate().filter(|(_, edit)| is_node(edit)).map(|(index, _)| index).collect();
    let trees = [input.before, input.after];
    for place in crate::diffmap::nodes(outcome, trees, &mut map, &indices) {
        let edit = nodes[place];
        map.entries[place].before = side_text(edit, 0, trees);
        map.entries[place].after = side_text(edit, 1, trees);
        let added = matches!(edit.kind, EditKind::ItemAdded | EditKind::Inserted);
        let item = if added { &edit.after } else { &edit.before }.as_ref().map(|anchor| anchor.item.clone()).unwrap_or_default();
        map.entries[place].template = templates.entry((added, item)).or_insert_with(|| template(input, outcome, edit, &mut pending)).clone();
    }
    // [R92.6-1] [R92.7-1] The facts of one change make one fact, and each fact has one kind.
    crate::diffmap::shape(outcome, trees, &mut map, &indices);
    let nodes: Vec<&Edit> = outcome.edits.iter().filter(|edit| is_node(edit)).collect();
    let mut not_mapped = BTreeSet::new();
    for (edit, entry) in nodes.iter().zip(&map.entries) {
        if entry.state == AstState::NotMapped {
            for anchor in [&edit.before, &edit.after].into_iter().flatten() {
                not_mapped.insert(anchor.item.clone());
            }
        }
    }
    // [R79.7-1] The items with a node that is not mapped, in the names of the two trees.
    let renamed: Vec<String> = not_mapped.iter().filter_map(|item| outcome.renames.items.get(item).cloned()).collect();
    not_mapped.extend(renamed);
    outcome.not_mapped = not_mapped;
    outcome.report.ast = Some(map);
    // [R87.1-1] The forms of each template for the gate.
    outcome.templates.asks.extend(pending.asks);
}

/// [R79.3-6] An added or a removed container (an impl block, a trait, a module) and a member with
/// no body of an added or a removed impl block or trait (an associated type) take the state of
/// the members of the container with a body: each one mapped by a claim gives "mapped by claim"
/// (the ids of the members); each one mapped gives "mapped by Semilla"; one member that is not
/// mapped gives "not mapped". The head of a container has no behavior of its own: its members
/// have it. A container with no such member keeps its own entry.
fn containers(input: &StepInput, nodes: &[&Edit], entries: &mut [AstEntry]) {
    let trees = [input.before, input.after];
    // The side, the item, and the place of each node that adds or removes an item.
    let place = |at: usize| -> Option<(usize, String, (String, usize, usize))> {
        let edit = nodes[at];
        let side = match edit.kind {
            EditKind::ItemAdded => 1,
            EditKind::ItemRemoved => 0,
            _ => return None,
        };
        let anchor = edit.anchor()?;
        let item = trees[side].items.get(&anchor.item)?;
        Some((side, anchor.item.clone(), crate::records::item_place(trees[side], item)?))
    };
    let places: Vec<Option<(usize, String, (String, usize, usize))>> = (0..nodes.len()).map(place).collect();
    let kind_of = |at: usize| places[at].as_ref().and_then(|(side, item, _)| trees[*side].items.get(item)).map(|item| item.kind);
    // A member is in the range of its block; a member of a module is an item below its path (a
    // module file has its items in another file).
    let inside = |outer: usize, inner: usize| match (&places[outer], &places[inner]) {
        (Some((a, outer_item, (file, start, end))), Some((b, inner_item, (other, inner_start, inner_end)))) => {
            outer != inner && a == b && ((file == other && start <= inner_start && inner_end <= end) || (kind_of(outer) == Some(ItemKind::Mod) && inner_item.starts_with(&format!("{outer_item}::"))))
        }
        _ => false,
    };
    // The containers of an associated type: an impl block or a trait.
    let holder = |at: usize| (0..nodes.len()).filter(|outer| matches!(kind_of(*outer), Some(ItemKind::Impl | ItemKind::Trait)) && inside(*outer, at)).min_by_key(|outer| places[*outer].as_ref().map(|(_, _, (_, start, end))| end - start).unwrap_or(usize::MAX));
    // The holder of each associated type, one time for each node.
    let holders: Vec<Option<usize>> = (0..nodes.len()).map(|at| if matches!(kind_of(at), Some(ItemKind::Type)) { holder(at) } else { None }).collect();
    let bodiless = |at: usize| holders[at].is_some();
    let mut decided: Vec<(usize, AstState, String)> = Vec::new();
    for outer in (0..nodes.len()).filter(|at| matches!(kind_of(*at), Some(ItemKind::Impl | ItemKind::Trait | ItemKind::Mod))) {
        let members: Vec<usize> = (0..nodes.len()).filter(|inner| inside(outer, *inner) && !bodiless(*inner) && !matches!(kind_of(*inner), Some(ItemKind::Impl | ItemKind::Trait | ItemKind::Mod | ItemKind::Use))).collect();
        if members.is_empty() {
            continue;
        }
        let states: Vec<AstState> = members.iter().map(|at| entries[*at].state).collect();
        let decision = if states.contains(&AstState::NotMapped) {
            // [R92.3-1] Who reaches the head of the container: its members.
            (AstState::NotMapped, "the head of a container: its members reach it".to_string())
        } else if states.iter().all(|state| *state == AstState::Claim) {
            let mut ids: Vec<String> = members.iter().flat_map(|at| entries[*at].by.split(", ").map(str::to_string).collect::<Vec<_>>()).collect();
            ids.sort();
            ids.dedup();
            (AstState::Claim, ids.join(", "))
        } else {
            (AstState::Semilla, "a container: each member is mapped".to_string())
        };
        decided.push((outer, decision.0, decision.1));
    }
    let mut apply = |at: usize, state: AstState, by: &str| {
        let entry = &mut entries[at];
        if entry.state == AstState::Claim {
            return;
        }
        entry.state = state;
        entry.by = by.to_string();
        entry.rule = RULE_CONTAINER.to_string();
        if state == AstState::NotMapped {
            entry.template = "no template: the head of a container has no claim; a claim on each member that is not mapped maps it".into();
        } else {
            entry.before.clear();
            entry.after.clear();
            entry.template.clear();
        }
    };
    for (at, state, by) in &decided {
        apply(*at, *state, by);
    }
    for at in (0..nodes.len()).filter(|at| bodiless(*at)) {
        if let Some((_, state, by)) = holders[at].and_then(|outer| decided.iter().find(|(known, ..)| *known == outer)) {
            apply(at, *state, by);
        }
    }
}

/// The result of joining a definition of the compiler to a node of a syntax tree.
enum Joined {
    Item(String),
    /// The tree has a name record and no item of the definition.
    NoNode,
    /// The tree has no name record, and no item has the path of the definition.
    NoRecord,
}

/// The join of the definitions of the compiler to the items of one syntax tree (R70.1-1): the
/// name record gives the definition of each item; with no record, an item with the path of the
/// definition.
struct Join<'a> {
    tree: &'a Workspace,
    record: bool,
    by_def: BTreeMap<String, String>,
}

impl<'a> Join<'a> {
    fn new(tree: &'a Workspace) -> Join<'a> {
        let names = tree.names.get();
        let mut by_def = BTreeMap::new();
        if let Some(index) = &names {
            for (path, item) in &tree.items {
                if item.kind == ItemKind::Use {
                    continue;
                }
                if let Some(def) = crate::records::item_def(tree, index, path) {
                    by_def.entry(def.path.clone()).or_insert_with(|| path.clone());
                }
            }
        }
        Join { tree, record: names.is_some(), by_def }
    }

    fn item(&self, def: &str) -> Joined {
        if let Some(path) = self.by_def.get(def) {
            return Joined::Item(path.clone());
        }
        if self.tree.items.contains_key(def) {
            return Joined::Item(def.to_string());
        }
        // A definition of a generic item names its parameters (`shop::Bag::<T>::new`); the path of
        // the syntax tree has none.
        let plain = no_args(def);
        if let Some((_, path)) = self.by_def.iter().find(|(known, _)| no_args(known) == plain) {
            return Joined::Item(path.clone());
        }
        if self.tree.items.contains_key(&plain) {
            return Joined::Item(plain);
        }
        if self.record { Joined::NoNode } else { Joined::NoRecord }
    }
}

/// The record name of a definition with no facade and no generic arguments.
fn plain(text: &str, side: usize) -> String {
    let text = text.strip_prefix(facade(side)).unwrap_or(text);
    let mut out = String::new();
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '<' if !out.is_empty() && !out.ends_with("::") && depth == 0 => depth = 1,
            '<' if depth > 0 => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// A path with no generic arguments (`shop::Bag::<T>::new` → `shop::Bag::new`,
/// `<shop::W<B> as core::iter::Iterator>::next` → `<shop::W as core::iter::Iterator>::next`): a
/// call of a generic claim names the instance, and its definition has the parameters.
pub fn no_args(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for (at, c) in text.char_indices() {
        if depth > 0 {
            match c {
                '<' => depth += 1,
                '>' => depth -= 1,
                _ => {}
            }
            continue;
        }
        // A `<` after a name (or after `::`) opens generic arguments; a `<` at the start or after
        // a space opens a qualified path.
        if c == '<' && at > 0 && !text[..at].ends_with(' ') && !text[..at].ends_with('<') && !text[..at].ends_with('&') {
            depth = 1;
            if out.ends_with("::") {
                out.truncate(out.len() - 2);
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// A scope of batch 13b (an `impl` block, a glob, the callers of an item): no anchor in this
/// batch.
fn wide(scope: &str) -> bool {
    let scope = scope.trim();
    scope.starts_with("impl ") || scope.starts_with("callers(") || scope.ends_with("::*")
}

/// The words of a kind of item in a message.
fn kind_words(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Fn => "a function",
        ItemKind::Struct => "a struct",
        ItemKind::Enum => "an enum",
        ItemKind::Trait => "a trait",
        ItemKind::Impl => "an impl block",
        ItemKind::Const => "a constant",
        ItemKind::Static => "a static",
        ItemKind::Type => "a type alias",
        ItemKind::Use => "a `use` line",
        ItemKind::Mod => "a module",
        ItemKind::Macro => "a macro",
        ItemKind::Field => "a field",
        ItemKind::Variant => "a variant",
        ItemKind::Other => "an item",
    }
}

/// True for a type of a relation (R79.2-2).
fn is_type(kind: ItemKind) -> bool {
    matches!(kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Type | ItemKind::Other)
}

/// [R79.2-1] [R79.2-2] The anchors of the claims of the intent files of a step, from the compiled
/// claims, with the check of each anchor against the two trees. An anchor that does not fit is
/// an error of its claim (R79.2-2).
fn claim_anchors(input: &StepInput, outcome: &StepOutcome, data: &CompiledClaims, map: &mut AstMap) -> Vec<ClaimAnchor> {
    let Ok(parsed) = crate::intent::parse_files(&input.step.agent.intent) else { return Vec::new() };
    let ids = crate::intent::item_ids(&parsed);
    let mut ids_of: BTreeMap<(String, String, String), Vec<String>> = BTreeMap::new();
    for (file, ids) in parsed.iter().zip(&ids) {
        for (item, id) in file.items.iter().zip(ids) {
            if let (semilla_intent::Item::Claim(claim), Some(id)) = (item, id) {
                let attribute = claim.attribute.trim_end_matches(" of a wide scope").to_string();
                ids_of.entry((file.path.clone(), claim.func.name.clone(), attribute)).or_default().push(id.clone());
            }
        }
    }
    let trees = [input.before, input.after];
    let joins = [Join::new(input.before), Join::new(input.after)];
    let added = |side: usize, path: &str| {
        let kind = if side == 1 { EditKind::ItemAdded } else { EditKind::ItemRemoved };
        outcome.edits.iter().any(|edit| edit.kind == kind && edit.anchor().is_some_and(|anchor| anchor.item == path && anchor.path.is_empty()))
    };
    let mut out = Vec::new();
    for file in &input.step.agent.intent {
        let (heads, _) = semilla_intent::heads::check(&file.path, &file.text);
        for head in &heads {
            let list = ids_of.get(&(file.path.clone(), head.function.clone(), head.kind.clone())).cloned().unwrap_or_default();
            let id_at = |k: usize| list.get(k).or(list.first()).cloned().unwrap_or_else(|| format!("fn {}", head.function));
            let place = |k: usize| head.scope_at.get(k).map(|(line, column, ..)| format!("{}:{line}:{column}", file.path)).unwrap_or_else(|| format!("{}:{}", file.path, head.line));
            let calls = data.functions.get(&(file.path.clone(), head.function.clone())).cloned().unwrap_or_default();
            let mut error = |k: usize, message: String| map.errors.push(format!("{} {}: {message}", id_at(k), place(k)));
            match head.kind.as_str() {
                "same" | "change" | "different" | "new" | "removed" => {
                    // One entry for each scope path: the item of each version that it anchors.
                    let mut sides: Vec<(usize, [Option<String>; 2])> = Vec::new();
                    for (k, scope) in head.scope.iter().enumerate() {
                        if wide(scope) {
                            map.anchors.push(format!("{}: the scope `{scope}` is a scope of batch 13b: no anchor in this batch", id_at(k)));
                            continue;
                        }
                        let mut found: [Option<String>; 2] = [None, None];
                        let mut stop = false;
                        for link in semilla_intent::heads::scope_links(&head.kind, k, scope) {
                            let side = usize::from(link.starts_with(facade(1)));
                            let Some(Some(def)) = data.links.get(&link) else { continue };
                            let def = def.strip_prefix(facade(side)).unwrap_or(def).to_string();
                            match joins[side].item(&def) {
                                Joined::Item(path) => {
                                    // [R79.2-1] A property anchors only a function of its scope that
                                    // it calls; an empty `same` claim is the full compare of its
                                    // scope. A type, a constant, or a static has no call: its scope
                                    // path is the anchor.
                                    let callable = trees[side].items.get(&path).is_some_and(|item| item.kind == ItemKind::Fn);
                                    let called = calls.calls[side].iter().any(|call| *call == def || no_args(call) == no_args(&def));
                                    if callable && !head.empty_body && !called {
                                        map.anchors.push(format!("{}: the claim does not call `{}{def}`: no anchor in {}", id_at(k), facade(side), version_words(side)));
                                        continue;
                                    }
                                    found[side] = Some(path);
                                }
                                Joined::NoNode => {
                                    error(k, format!("[S0120] the scope `{scope}` names `{def}`, and the syntax tree of {} has no node of it; expected an item of the source (an item that a macro makes has no node)", version_words(side)));
                                    stop = true;
                                }
                                Joined::NoRecord => map.anchors.push(format!("{}: the step has no name record, and no item of {} has the path `{def}`: no anchor", id_at(k), version_words(side))),
                            }
                        }
                        if stop {
                            continue;
                        }
                        let kinds = [0, 1].map(|side| found[side].as_ref().and_then(|path| trees[side].items.get(path)).map(|item| item.kind));
                        // [R79.2-2] The kinds fit.
                        match head.kind.as_str() {
                            "new" => {
                                if let Some(path) = &found[1]
                                    && !added(1, path)
                                {
                                    let partner = outcome.renames.items.iter().find(|(_, new)| *new == path).map(|(old, _)| format!(" (its partner in the old tree: `{old}`)")).unwrap_or_default();
                                    error(k, format!("[S0120] `#[new({scope})]` expects an item that the step adds, and `{path}` has a node in the two trees{partner}; write `#[change({scope})]` or `#[same({scope})]`"));
                                    continue;
                                }
                            }
                            "removed" => {
                                if let Some(path) = &found[0]
                                    && !added(0, path)
                                {
                                    let partner = outcome.renames.items.get(path).map(|new| format!(" (its partner in the new tree: `{new}`)")).unwrap_or_default();
                                    error(k, format!("[S0120] `#[removed({scope})]` expects an item that the step removes, and `{path}` has a node in the two trees{partner}; write `#[change({scope})]` or `#[same({scope})]`"));
                                    continue;
                                }
                            }
                            _ => {
                                if let [Some(old), Some(new)] = kinds
                                    && old != new
                                {
                                    error(k, format!("[S0120] the scope `{scope}` is {} in the old version and {} in the new version; expected one kind of item in the two versions", kind_words(old), kind_words(new)));
                                    continue;
                                }
                            }
                        }
                        if kinds.iter().flatten().any(|kind| matches!(kind, ItemKind::Mod | ItemKind::Impl | ItemKind::Trait)) {
                            map.anchors.push(format!("{}: the scope `{scope}` is a module, an impl block, or a trait: a scope of batch 13b, no anchor in this batch", id_at(k)));
                            continue;
                        }
                        if found.iter().any(Option::is_some) {
                            sides.push((k, found));
                        }
                    }
                    // A rename or a move: one scope path of each version gives one pair.
                    let olds: Vec<usize> = (0..sides.len()).filter(|at| sides[*at].1[0].is_some() && sides[*at].1[1].is_none()).collect();
                    let news: Vec<usize> = (0..sides.len()).filter(|at| sides[*at].1[1].is_some() && sides[*at].1[0].is_none()).collect();
                    if let ([old], [new]) = (olds.as_slice(), news.as_slice()) {
                        let item = sides[*new].1[1].take();
                        sides[*old].1[1] = item;
                        sides.remove(*new);
                    }
                    for (k, [old, new]) in sides {
                        map.anchors.push(format!("{}: {} {} → {}", id_at(k), head.kind, old.as_deref().map(|path| format!("`{path}`")).unwrap_or_else(|| "(none)".into()), new.as_deref().map(|path| format!("`{path}`")).unwrap_or_else(|| "(none)".into())));
                        out.push(ClaimAnchor { id: id_at(k), old, new, whole: true, parts: Default::default(), rule: RULE_ANCHOR });
                    }
                }
                "relation" => {
                    let mut found: [Option<String>; 2] = [None, None];
                    let mut texts: [BTreeSet<String>; 2] = Default::default();
                    let mut stop = false;
                    for (k, scope) in head.scope.iter().enumerate().take(2) {
                        for link in semilla_intent::heads::scope_links(&head.kind, k, scope) {
                            let side = usize::from(link.starts_with(facade(1)));
                            let Some(Some(def)) = data.links.get(&link) else { continue };
                            texts[side].insert(plain(def, side));
                            texts[side].insert(plain(&link, side));
                            let def = def.strip_prefix(facade(side)).unwrap_or(def).to_string();
                            match joins[side].item(&def) {
                                Joined::Item(path) => {
                                    let kind = trees[side].items.get(&path).map(|item| item.kind).unwrap_or(ItemKind::Other);
                                    if !is_type(kind) {
                                        error(k, format!("[S0120] `{scope}` is {}; a relation expects a type (a struct, an enum, a union, or a type alias)", kind_words(kind)));
                                        stop = true;
                                    } else {
                                        found[side] = Some(path);
                                    }
                                }
                                Joined::NoNode => {
                                    error(k, format!("[S0120] the scope `{scope}` names `{def}`, and the syntax tree of {} has no node of it; expected a type of the source", version_words(side)));
                                    stop = true;
                                }
                                Joined::NoRecord => map.anchors.push(format!("{}: the step has no name record, and no item of {} has the path `{def}`: no anchor", id_at(k), version_words(side))),
                            }
                        }
                    }
                    if stop {
                        continue;
                    }
                    // [R79.2-4] The fields and the variants that the relation reads.
                    let mut parts: [BTreeSet<String>; 2] = Default::default();
                    for side in 0..2 {
                        let Some(owner) = &found[side] else { continue };
                        for (ty, variant, field) in &calls.fields[side] {
                            if !texts[side].contains(ty) {
                                continue;
                            }
                            let base = if variant.is_empty() { owner.clone() } else { format!("{owner}::{variant}") };
                            let part = if field.is_empty() { base.clone() } else { format!("{base}.{field}") };
                            if !variant.is_empty() && trees[side].items.contains_key(&base) {
                                parts[side].insert(base.clone());
                            }
                            if trees[side].items.contains_key(&part) {
                                parts[side].insert(part);
                            } else if !field.is_empty() {
                                error(1, format!("[S0120] the relation reads the field `{field}` of `{owner}`, and the syntax tree of {} has no node of it; expected a field of the source", version_words(side)));
                            }
                        }
                    }
                    if found.iter().any(Option::is_some) {
                        let read = |side: usize| parts[side].iter().map(|part| format!("`{part}`")).collect::<Vec<_>>().join(", ");
                        map.anchors.push(format!("{}: relation {} → {}; reads {} | {}", id_at(0), found[0].as_deref().unwrap_or("(none)"), found[1].as_deref().unwrap_or("(none)"), read(0), read(1)));
                        out.push(ClaimAnchor { id: id_at(0), old: found[0].clone(), new: found[1].clone(), whole: false, parts, rule: RULE_RELATION });
                    }
                }
                // A case split, a lemma, an invariant, a contract, a loop relation, an
                // assumption, and a hint give no anchor of the AST pass (draft 3, the table "What
                // each claim gives each pass"): a hint pairs the code through the matcher.
                _ => {}
            }
        }
    }
    out
}

/// A place in a tree: the file and a byte offset.
type Spot = (String, usize);

/// The references to one item in one tree: the places that name it, or "always" when a part of
/// the code that the record does not show can read it.
enum Refs {
    Always,
    At(Vec<Spot>),
}

/// The data of one tree for the rules of Semilla on pure additions and removals.
struct Tree<'a> {
    tree: &'a Workspace,
    names: Option<Arc<NameIndex>>,
    /// The byte range of each item: the file, the start, and the end.
    ranges: BTreeMap<String, (String, usize, usize)>,
    /// The names of the record by their target definition.
    by_target: BTreeMap<usize, Vec<Spot>>,
    /// The words of the text by the word (the text rule).
    words: BTreeMap<String, Vec<Spot>>,
}

/// The word scan of a text: each identifier with its byte offset.
fn word_spots(text: &str) -> Vec<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let c = bytes[at];
        if c == b'_' || c.is_ascii_alphabetic() {
            let start = at;
            while at < bytes.len() && (bytes[at] == b'_' || bytes[at].is_ascii_alphanumeric()) {
                at += 1;
            }
            out.push((start, &text[start..at]));
        } else {
            at += 1;
        }
    }
    out
}

/// The own name of an item path (`m::T.f#1` → `f`).
fn own_name(path: &str) -> &str {
    let last = path.rsplit(['.', ':']).next().unwrap_or(path);
    last.split('#').next().unwrap_or(last)
}

impl<'a> Tree<'a> {
    fn new(tree: &'a Workspace) -> Tree<'a> {
        let names = tree.names.get();
        // The byte range of each item, from one table of line starts for each file (the same
        // place as `records::item_place`, with no scan of the file for each item).
        let starts: BTreeMap<&String, Vec<usize>> = tree.rust_files.iter().map(|(file, text)| (file, std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at + 1)).collect())).collect();
        let offset = |file: &String, line: usize, column: usize| -> Option<usize> {
            let text = tree.rust_files.get(file)?;
            let start = *starts.get(file)?.get(line.checked_sub(1)?)?;
            let line_text = text[start..].split('\n').next().unwrap_or("");
            match line_text.char_indices().nth(column) {
                Some((at, _)) => Some(start + at),
                None if line_text.chars().count() == column => Some(start + line_text.len()),
                None => None,
            }
        };
        let ranges = tree.items.iter().filter_map(|(path, item)| Some((path.clone(), (item.file.clone(), offset(&item.file, item.line_start, item.col_start)?, offset(&item.file, item.line_end, item.col_end)?)))).collect();
        let mut by_target: BTreeMap<usize, Vec<Spot>> = BTreeMap::new();
        if let Some(index) = &names {
            for file in tree.rust_files.keys() {
                for (start, _, target) in index.targets_in(file, 0, usize::MAX) {
                    if let Target::Def(def) = target {
                        by_target.entry(def).or_default().push((file.clone(), start));
                    }
                }
            }
        }
        let mut words: BTreeMap<String, Vec<Spot>> = BTreeMap::new();
        for (file, text) in &tree.rust_files {
            for (at, word) in word_spots(text) {
                words.entry(word.to_string()).or_default().push((file.clone(), at));
            }
        }
        Tree { tree, names, ranges, by_target, words }
    }

    /// For each place of the references: the items of a set whose range holds the place; `None`
    /// for a reference that is always there.
    fn held(&self, refs: &Refs, items: &BTreeSet<String>) -> Option<Vec<Vec<String>>> {
        let Refs::At(spots) = refs else { return None };
        let ranges: Vec<(&String, &(String, usize, usize))> = items.iter().filter_map(|item| self.ranges.get(item).map(|range| (item, range))).collect();
        Some(spots.iter().map(|spot| ranges.iter().filter(|(_, (file, start, end))| *file == spot.0 && *start <= spot.1 && spot.1 < *end).map(|(item, _)| (*item).clone()).collect()).collect())
    }

    /// True if a place is in the range of one item of a set.
    fn in_any(&self, spot: &Spot, items: &BTreeSet<String>) -> bool {
        items.iter().any(|item| self.ranges.get(item).is_some_and(|(file, start, end)| *file == spot.0 && *start <= spot.1 && spot.1 < *end))
    }

    /// True for an impl of a trait or an item of one: a use of the trait for its type reads it,
    /// and no name of the record shows such a use.
    fn of_trait(&self, path: &str) -> bool {
        // The record (the trait reference of an impl, the trait item of a member) or the tree (the
        // path `m::<T as Trait>::f` of a member, the head `impl Trait for T` of a block).
        let record = self.names.as_ref().and_then(|index| crate::records::item_def(self.tree, index, path)).is_some_and(|def| def.trait_ref.as_deref().is_some_and(|text| text.contains(" as ")) || def.of.is_some());
        record || path.contains(" as ") || self.tree.items.get(path).is_some_and(|item| item.kind == ItemKind::Impl && item.signature.split_whitespace().any(|word| word == "for"))
    }

    /// The type of a field (`m::T.f` → `m::T`; `m::T::V.f` → `m::T`).
    fn owner_type(&self, path: &str) -> Option<String> {
        let base = path.rsplit_once('.')?.0;
        let is_variant = self.tree.items.get(base).is_some_and(|item| item.kind == ItemKind::Variant);
        Some(if is_variant { base.rsplit_once("::").map(|(owner, _)| owner).unwrap_or(base) } else { base }.to_string())
    }

    /// The references to an item: the names of the record that resolve to its definition (or to
    /// an item of it: a method of a type, an item of a module), and the words of the text in a
    /// macro definition (the record has no name of an expansion). With no record: each word of
    /// the text with the name of the item. A field of a type with an attribute (a derive can
    /// read each field) is always read.
    fn refs(&self, path: &str) -> Refs {
        let Some(item) = self.tree.items.get(path) else { return Refs::At(Vec::new()) };
        if item.kind == ItemKind::Field
            && self.owner_type(path).and_then(|owner| self.tree.items.get(&owner)).is_some_and(|owner| !owner.attrs.is_empty())
        {
            return Refs::Always;
        }
        let name = own_name(path);
        let words = self.words.get(name).cloned().unwrap_or_default();
        let def = self.names.as_ref().and_then(|index| crate::records::item_def(self.tree, index, path).map(|def| (index, def)));
        let Some((index, def)) = def else { return Refs::At(words) };
        let prefix = format!("{}::", def.path);
        let mut spots: Vec<Spot> = Vec::new();
        for (at, known) in index.defs.iter().enumerate() {
            if known.path == def.path || known.path.starts_with(&prefix) {
                spots.extend(self.by_target.get(&at).cloned().unwrap_or_default());
            }
        }
        // A name in a macro definition: the record has no name of an expansion.
        let macros: BTreeSet<String> = self.tree.items.iter().filter(|(_, item)| item.macro_definition).map(|(path, _)| path.clone()).collect();
        spots.extend(words.into_iter().filter(|spot| self.in_any(spot, &macros)));
        Refs::At(spots)
    }

    /// True if a different crate can use the item (a public item of a library).
    fn public(&self, path: &str) -> bool {
        if let Some(index) = &self.names
            && let Some(def) = crate::records::item_def(self.tree, index, path)
        {
            return def.public;
        }
        self.tree.items.get(path).is_some_and(|item| item.vis.trim() == "pub" && !path.contains(" ("))
    }

    /// True for a container whose members decide its state: an `impl` block, a module, a trait.
    fn is_container(&self, path: &str) -> bool {
        self.tree.items.get(path).is_some_and(|item| container(item.kind))
    }

    /// True if the range of the item `inner` is inside the range of the item `outer`.
    fn within(&self, outer: &str, inner: &str) -> bool {
        match (self.ranges.get(outer), self.ranges.get(inner)) {
            (Some((file, start, end)), Some((other, inner_start, inner_end))) => outer != inner && file == other && start <= inner_start && inner_end <= end,
            _ => false,
        }
    }

    /// The items that hold a place: the smallest item of the file that contains it (not a `use`
    /// line, not a module).
    fn holder(&self, spot: &Spot) -> Option<String> {
        self.ranges
            .iter()
            .filter(|(path, (file, start, end))| *file == spot.0 && *start <= spot.1 && spot.1 < *end && self.tree.items.get(*path).is_some_and(|item| !matches!(item.kind, ItemKind::Use | ItemKind::Mod)))
            .min_by_key(|(_, (_, start, end))| end - start)
            .map(|(path, _)| path.clone())
    }
}

/// The containers whose members decide their state: an `impl` block, a module, a trait.
fn container(kind: ItemKind) -> bool {
    matches!(kind, ItemKind::Impl | ItemKind::Mod | ItemKind::Trait)
}

/// The state of a `use` line (R79.3-5): the state, what maps it, and the cause or the template.
type UseState = (AstState, String, String);

/// The rules of Semilla on the pure additions and removals of a step (R79.3-2 to R79.3-5).
struct Pure {
    /// [R79.3-3] The removed items with no reader.
    dead: BTreeSet<String>,
    /// [R79.3-4] The added items that no old code reaches.
    unreached: BTreeSet<String>,
    /// [R79.3-5] The state of each added (side 1) and removed (side 0) `use` line.
    uses: [BTreeMap<String, UseState>; 2],
    /// [R92.3-1] Who reaches each added (side 1) and removed (side 0) item that old code reaches
    /// (that new code reads): the items of the other tree that name it, from the name record.
    reach: [BTreeMap<String, String>; 2],
}

impl Pure {
    fn new(input: &StepInput, outcome: &StepOutcome, anchors: &[ClaimAnchor]) -> Pure {
        let pure_items = |kind: EditKind| -> BTreeSet<String> { outcome.edits.iter().filter(|edit| edit.kind == kind).filter_map(|edit| edit.anchor()).filter(|anchor| anchor.path.is_empty()).map(|anchor| anchor.item.clone()).collect() };
        let (removed, added) = (pure_items(EditKind::ItemRemoved), pure_items(EditKind::ItemAdded));
        if removed.is_empty() && added.is_empty() {
            return Pure { dead: BTreeSet::new(), unreached: BTreeSet::new(), uses: Default::default(), reach: Default::default() };
        }
        let trees = [Tree::new(input.before), Tree::new(input.after)];
        let is_use = |side: usize, path: &String| trees[side].tree.items.get(path).is_some_and(|item| item.kind == ItemKind::Use);
        // [R79.3-3] The removed items with no reader: a greatest fixed point, so a group of removed
        // items that read only each other has no reader.
        let candidates: BTreeSet<String> = removed.iter().filter(|path| !is_use(0, path)).cloned().collect();
        // For each place that names a candidate: the candidates whose range holds it (one time).
        // A place is outside the set when no candidate of the set holds it.
        let held: BTreeMap<&String, Option<Vec<Vec<String>>>> = candidates.iter().map(|path| (path, trees[0].held(&trees[0].refs(path), &candidates))).collect();
        let always: BTreeSet<&String> = candidates.iter().filter(|path| trees[0].public(path) || trees[0].of_trait(path)).collect();
        let mut dead = candidates.clone();
        loop {
            let read: Vec<String> = dead
                .iter()
                .filter(|path| {
                    always.contains(path)
                        || match &held[path] {
                            None => true,
                            Some(spots) => spots.iter().any(|holders| holders.iter().all(|holder| !dead.contains(holder))),
                        }
                })
                .cloned()
                .collect();
            if read.is_empty() {
                break;
            }
            for path in read {
                dead.remove(&path);
            }
        }
        // [R79.3-4] The added items that old code reaches: the least fixed point from the items
        // of the new tree that the step does not add.
        let candidates: BTreeSet<String> = added.iter().filter(|path| !is_use(1, path)).cloned().collect();
        let held: BTreeMap<&String, Option<Vec<Vec<String>>>> = candidates.iter().map(|path| (path, trees[1].held(&trees[1].refs(path), &added))).collect();
        let mut reached: BTreeSet<String> = candidates.iter().filter(|path| trees[1].of_trait(path) || trees[1].tree.items.get(*path).is_some_and(|item| item.kind == ItemKind::Macro && !item.macro_definition)).cloned().collect();
        loop {
            let open: BTreeSet<String> = added.iter().filter(|path| !reached.contains(*path)).cloned().collect();
            let found: Vec<String> = open
                .iter()
                .filter(|path| {
                    held.get(path).is_some_and(|held| match held {
                        None => true,
                        Some(spots) => spots.iter().any(|holders| holders.iter().all(|holder| !open.contains(holder))),
                    })
                })
                .cloned()
                .collect();
            if found.is_empty() {
                break;
            }
            reached.extend(found);
        }
        // A container takes the state of its members: a member with a reader (or that old code
        // reaches) makes its container read (reached).
        let alive: Vec<String> = dead.iter().filter(|path| trees[0].is_container(path) && removed.iter().filter(|member| !dead.contains(*member)).any(|member| trees[0].within(path, member))).cloned().collect();
        for path in alive {
            dead.remove(&path);
        }
        let more: Vec<String> = candidates.iter().filter(|path| !reached.contains(*path) && trees[1].is_container(path) && reached.iter().any(|member| trees[1].within(path, member))).cloned().collect();
        reached.extend(more);
        let unreached: BTreeSet<String> = candidates.iter().filter(|path| !reached.contains(*path)).cloned().collect();
        // [R79.3-5] The `use` lines.
        let mut edited: [BTreeSet<String>; 2] = Default::default();
        for edit in &outcome.edits {
            if let Some(anchor) = &edit.before {
                edited[0].insert(anchor.item.clone());
            }
            if let Some(anchor) = &edit.after {
                edited[1].insert(anchor.item.clone());
            }
        }
        let mut uses: [BTreeMap<String, UseState>; 2] = Default::default();
        for (side, list) in [(0, &removed), (1, &added)] {
            for path in list.iter().filter(|path| is_use(side, path)) {
                uses[side].insert(path.clone(), use_state(&trees[side], &trees[1 - side], side, path, anchors, &edited[side]));
            }
        }
        // [R92.3-1] Who reaches each item that the rules above leave with a reader.
        let mut reach: [BTreeMap<String, String>; 2] = Default::default();
        for path in removed.iter().filter(|path| !dead.contains(*path) && !is_use(0, path)) {
            reach[0].insert(path.clone(), reach_text(&trees[0], trees[1].tree, path));
        }
        for path in added.iter().filter(|path| !unreached.contains(*path) && !is_use(1, path)) {
            reach[1].insert(path.clone(), reach_text(&trees[1], trees[0].tree, path));
        }
        Pure { dead, unreached, uses, reach }
    }
}

/// [R92.3-1] Who reaches an added or a removed item (design 92): the items of the other tree that
/// name it (the name record; with no record, the words of the text), at most four with a count
/// of the others. A public item and an item of a trait impl have the cause of their reader.
fn reach_text(tree: &Tree, other: &Workspace, path: &str) -> String {
    if tree.of_trait(path) {
        return "a use of its trait reaches it".into();
    }
    let spots = match tree.refs(path) {
        Refs::Always => return "a derive of its type reads it".into(),
        Refs::At(spots) => spots,
    };
    let inner = |holder: &String| holder == path || holder.starts_with(&format!("{path}::")) || holder.starts_with(&format!("{path}."));
    let readers: BTreeSet<String> = spots.iter().take(REACH_SPOTS).filter_map(|spot| tree.holder(spot)).filter(|holder| !inner(holder) && other.items.contains_key(holder)).collect();
    let source = if tree.names.is_some() { "the name record" } else { "the words of the text" };
    if readers.is_empty() {
        return if tree.public(path) { "a different crate can use it (a public item)".into() } else { format!("no item of the other tree names it ({source})") };
    }
    let shown: Vec<String> = readers.iter().take(4).map(|reader| format!("`{reader}`")).collect();
    let more = if readers.len() > 4 { format!(", and {} more", readers.len() - 4) } else { String::new() };
    format!("reached from {}{more} ({source})", shown.join(", "))
}

/// [R92.3-1] The most references of an item that the text of who reaches it reads.
const REACH_SPOTS: usize = 64;

/// [R79.3-5] The state of an added or a removed `use` line: unused by the name record (Semilla),
/// or a part of the changes of its readers when each item that reads a name of the line has an
/// edit or an anchor of a claim. A line with a visibility (other modules can read it), a glob
/// line, and a line of a tree with no name record are not mapped by this rule.
/// The definitions that a `use` line gives (the name record): the last name of each path of the
/// line. `None`: the tree has no record or no place of the line.
fn leaves_of(tree: &Tree, path: &str) -> Option<Vec<String>> {
    let index = tree.names.as_ref()?;
    let (file, start, end) = tree.ranges.get(path).cloned()?;
    let text = tree.tree.rust_files.get(&file).map(String::as_str).unwrap_or("");
    let mut leaves: Vec<String> = Vec::new();
    for (_, name_end, target) in index.targets_in(&file, start, end) {
        let Target::Def(def) = target else { continue };
        let rest = text.get(name_end..end).unwrap_or("").trim_start();
        if rest.is_empty() || rest.starts_with(',') || rest.starts_with('}') || rest.starts_with(';') || rest.starts_with("as ") {
            leaves.push(index.defs[def].path.clone());
        }
    }
    Some(leaves)
}

fn use_state(tree: &Tree, other: &Tree, side: usize, path: &str, anchors: &[ClaimAnchor], edited: &BTreeSet<String>) -> UseState {
    // [R92.3-1] A line that is not mapped has who reaches it in the second part.
    let reached = |cause: &str, reach: String| (AstState::NotMapped, reach, cause.to_string());
    let not_mapped = |cause: &str| reached(cause, "its names: no data of the name record".to_string());
    let Some(item) = tree.tree.items.get(path) else { return not_mapped("no claim names a `use` line") };
    if item.signature.contains('*') {
        return not_mapped("a glob `use` line: Semilla does not know the names that it gives; a claim on each item that reads them maps it");
    }
    let Some(index) = &tree.names else { return not_mapped("the step has no name record: Semilla cannot find the readers of the line (the build check gives the record)") };
    let Some(leaves) = leaves_of(tree, path) else { return not_mapped("Semilla has no place of the line") };
    if leaves.is_empty() {
        return not_mapped("the name record has no definition for the names of the line");
    }
    // A name that a `use` line of the same module of the other tree (with the same visibility)
    // gives too stays imported: only the names that the step adds or removes count.
    let module = path.rsplit_once("::").map(|(module, _)| module).unwrap_or("");
    let kept: BTreeSet<String> = other
        .tree
        .items
        .iter()
        .filter(|(known, found)| found.kind == ItemKind::Use && known.rsplit_once("::").map(|(owner, _)| owner) == Some(module) && found.vis.replace(' ', "") == item.vis.replace(' ', ""))
        .flat_map(|(known, _)| leaves_of(other, known).unwrap_or_default())
        .collect();
    let leaves: Vec<String> = leaves.into_iter().filter(|leaf| !kept.contains(leaf)).collect();
    if leaves.is_empty() {
        return (AstState::Semilla, "a new form of the `use` lines: each name of the line stays imported in its module".into(), String::new());
    }
    let claims_of = |reader: &String| -> Vec<String> { anchors.iter().filter(|anchor| anchor.covers(side, reader, tree.tree)).map(|anchor| anchor.id.clone()).collect() };
    let list = |set: &BTreeSet<String>| set.iter().map(|reader| format!("`{reader}`")).collect::<Vec<_>>().join(", ");
    // A line with a visibility gives its names to other modules (or crates): the claims that
    // anchor each item that it names map it.
    if !item.vis.trim().is_empty() {
        let named: BTreeSet<String> = tree.tree.items.keys().filter(|named_item| crate::records::item_def(tree.tree, index, named_item).is_some_and(|def| leaves.contains(&def.path))).cloned().collect();
        if !named.is_empty() && named.iter().all(|other| !claims_of(other).is_empty()) {
            let mut ids: Vec<String> = named.iter().flat_map(claims_of).collect();
            ids.sort();
            ids.dedup();
            return (AstState::Claim, ids.join(", "), String::new());
        }
        let shown = if named.is_empty() { leaves.iter().map(|leaf| format!("`{leaf}`")).collect::<Vec<_>>().join(", ") } else { list(&named) };
        return reached(&format!("a `use` line with a visibility gives its names to other modules; a claim on each item that it names maps it: {shown}"), format!("its names reach other modules: {shown} (the name record)"));
    }
    // The names of the module of the line and of each module below it.
    let module = path.rsplit_once("::").map(|(module, _)| module).unwrap_or("");
    let mut files: BTreeSet<String> = tree.tree.module_files.iter().filter(|(name, _)| *name == module || name.starts_with(&format!("{module}::"))).map(|(_, file)| file.clone()).collect();
    files.insert(item.file.clone());
    let use_lines: BTreeSet<String> = tree.tree.items.iter().filter(|(_, item)| item.kind == ItemKind::Use).map(|(path, _)| path.clone()).collect();
    let mut readers: BTreeSet<String> = BTreeSet::new();
    for scope_file in &files {
        for (at, _, target) in index.targets_in(scope_file, 0, usize::MAX) {
            let Target::Def(def) = target else { continue };
            let found = &index.defs[def].path;
            if !leaves.iter().any(|leaf| found == leaf || found.starts_with(&format!("{leaf}::"))) {
                continue;
            }
            let spot = (scope_file.clone(), at);
            if tree.in_any(&spot, &use_lines) {
                continue;
            }
            if let Some(holder) = tree.holder(&spot) {
                readers.insert(holder);
            }
        }
    }
    if readers.is_empty() {
        return (AstState::Semilla, "an unused `use` line (the name record)".into(), String::new());
    }
    if readers.iter().all(|reader| !claims_of(reader).is_empty()) {
        let mut ids: Vec<String> = readers.iter().flat_map(claims_of).collect();
        ids.sort();
        ids.dedup();
        return (AstState::Claim, ids.join(", "), String::new());
    }
    let open: BTreeSet<String> = readers.iter().filter(|reader| !edited.contains(*reader) && claims_of(reader).is_empty()).cloned().collect();
    if open.is_empty() {
        return (AstState::Semilla, format!("a part of the changes of its readers: {}", list(&readers)), String::new());
    }
    reached(&format!("no claim names a `use` line; it reads into items that the step does not change: {}; a claim on each one maps the line", list(&open)), format!("reached from {} (the name record)", list(&open)))
}

/// The words of the change of an edit.
fn change_word(kind: EditKind) -> &'static str {
    match kind {
        EditKind::ItemAdded | EditKind::Inserted | EditKind::FileAdded => "added",
        EditKind::ItemRemoved | EditKind::Deleted | EditKind::FileRemoved => "removed",
        EditKind::Moved => "moved",
        _ => "changed",
    }
}

/// The kind of the node of an edit.
fn node_word(edit: &Edit, trees: [&Workspace; 2]) -> String {
    match edit.kind {
        EditKind::Docs => "docs".into(),
        EditKind::Comment => "comment".into(),
        EditKind::Attrs => "attributes".into(),
        EditKind::Visibility => "visibility".into(),
        EditKind::Signature => "signature".into(),
        EditKind::Members => "members".into(),
        EditKind::ItemAdded | EditKind::ItemRemoved => {
            let side = usize::from(edit.kind == EditKind::ItemAdded);
            let anchor = if side == 1 { &edit.after } else { &edit.before };
            match anchor.as_ref().and_then(|anchor| trees[side].items.get(&anchor.item)) {
                Some(item) if item.kind == ItemKind::Use => "`use` line".into(),
                Some(item) if item.macro_definition => "macro definition".into(),
                Some(item) => semilla_syntax::diff::kind_name(item.kind).to_string(),
                None => "item".into(),
            }
        }
        _ => {
            // The words of the facts: a statement (a node at the place of a statement, also the
            // tail), a `let`, a match arm, a literal, else an expression.
            let kind = edit.node_after.or(edit.node_before).unwrap_or("");
            let last = edit.anchor().and_then(|anchor| anchor.path.last()).map(|segment| segment.to_string()).unwrap_or_default();
            if matches!(edit.refactor, Some(Refactor::Arm { .. })) || kind == "arm" {
                "match arm".into()
            } else if kind == "let" {
                "`let`".into()
            } else if last == "tail" || (last.starts_with('S') && last[1..].chars().all(|c| c.is_ascii_digit()) && last.len() > 1) {
                "statement".into()
            } else if kind == "lit" {
                "literal".into()
            } else {
                "expression".into()
            }
        }
    }
}

/// The text of one side of a node: the source text of the node, else of its item.
fn side_text(edit: &Edit, side: usize, trees: [&Workspace; 2]) -> String {
    let (source, text, anchor) = if side == 0 { (&edit.source_before, &edit.text_before, &edit.before) } else { (&edit.source_after, &edit.text_after, &edit.after) };
    if let Some(found) = source.as_ref().or(text.as_ref()) {
        return found.clone();
    }
    match anchor {
        Some(anchor) if anchor.path.is_empty() => trees[side].items.get(&anchor.item).map(|item| item.source.clone()).unwrap_or_default(),
        _ => String::new(),
    }
}

/// True for code of a test, a bench, or an example: a `#[test]` function, a `cfg(test)` module,
/// an item of a test, bench, or example target (R79.3-2). The intent crate links the libraries
/// (design 78), so no claim names such an item.
pub(crate) fn test_code(tree: &Workspace, item: &str) -> bool {
    tree.items.get(item).is_some_and(|found| found.test) || [" (test \"", " (bench \"", " (example \""].iter().any(|root| item.contains(root))
}

/// [R79.1-1] The one map entry of one changed node.
fn entry(input: &StepInput, edit: &Edit, anchors: &[ClaimAnchor], pure: &Pure) -> AstEntry {
    let trees = [input.before, input.after];
    let items = [edit.before.as_ref().map(|anchor| anchor.item.clone()), edit.after.as_ref().map(|anchor| anchor.item.clone())];
    let at = edit.anchor().map(|anchor| anchor.to_string()).unwrap_or_default();
    let added = matches!(edit.kind, EditKind::ItemAdded | EditKind::Inserted);
    // A line of the old tree has the mark `-` (the form of a position, design 55).
    let line = if matches!(edit.kind, EditKind::ItemRemoved | EditKind::Deleted) { format!("-{}", edit.line) } else { edit.line.to_string() };
    let file = edit.file.clone().or_else(|| (0..2).rev().find_map(|side| items[side].as_ref().and_then(|item| trees[side].items.get(item)).map(|item| item.file.clone()))).unwrap_or_default();
    let mut entry = AstEntry { change: change_word(edit.kind).into(), node: node_word(edit, trees), at, place: format!("{file}:{line}"), state: AstState::Semilla, by: String::new(), rule: RULE_NODES.into(), before: String::new(), after: String::new(), template: String::new(), kind: None, syntax_only: false, entry: String::new() };
    let set = |entry: &mut AstEntry, state: AstState, by: &str, rule: &str| {
        entry.state = state;
        entry.by = by.to_string();
        entry.rule = rule.to_string();
    };
    // [R79.2-3] [R79.2-4] An anchor of a claim maps each node of its items.
    let mut ids: Vec<&str> = Vec::new();
    let mut rule = RULE_ANCHOR;
    for anchor in anchors {
        if (0..2).any(|side| items[side].as_ref().is_some_and(|item| anchor.covers(side, item, trees[side]))) && !ids.contains(&anchor.id.as_str()) {
            ids.push(&anchor.id);
            rule = anchor.rule;
        }
    }
    if !ids.is_empty() {
        set(&mut entry, AstState::Claim, &ids.join(", "), rule);
        return entry;
    }
    match edit.kind {
        // [R79.3-2] A doc text and a comment are not code.
        EditKind::Docs | EditKind::Comment => set(&mut entry, AstState::Semilla, "a doc or comment edit: not a part of the code", RULE_NO_CODE),
        // [R79.3-1] The matcher of G2: the pair of the node has one identity, equal tokens, a
        // similarity, or one place.
        EditKind::Visibility | EditKind::Signature | EditKind::Attrs | EditKind::Members => set(&mut entry, AstState::Semilla, "the identity of the item", RULE_MATCHER),
        EditKind::Moved => {
            let key = edit.note.as_deref().filter(|note| note.contains("a pair by")).map(|note| note.to_string()).unwrap_or_else(|| "equal tokens at a new place".into());
            set(&mut entry, AstState::Semilla, &key, RULE_MATCHER);
        }
        EditKind::Modified | EditKind::Replaced => set(&mut entry, AstState::Semilla, "a pair at one place", RULE_MATCHER),
        _ => {
            let side = usize::from(added);
            let item = items[side].clone().unwrap_or_default();
            let is_use = trees[side].items.get(&item).is_some_and(|found| found.kind == ItemKind::Use) && matches!(edit.kind, EditKind::ItemAdded | EditKind::ItemRemoved);
            if test_code(trees[side], &item) {
                set(&mut entry, AstState::Semilla, "test, bench, or example code: no claim names it", RULE_NO_CODE);
            } else if matches!(edit.refactor, Some(Refactor::Extract { .. } | Refactor::Inline { .. })) {
                let word = if matches!(edit.refactor, Some(Refactor::Extract { .. })) { "an extracted variable" } else { "an inlined variable" };
                set(&mut entry, AstState::Semilla, &format!("the statement mapper: a part of {word}"), RULE_MATCHER);
            } else if is_use {
                let (state, by, cause) = pure.uses[side].get(&item).cloned().unwrap_or((AstState::NotMapped, String::new(), "no claim names a `use` line".into()));
                set(&mut entry, state, &by, if state == AstState::NotMapped { RULE_REQUEST } else { RULE_USE });
                entry.template = cause;
            } else if edit.kind == EditKind::ItemRemoved && pure.dead.contains(&item) {
                set(&mut entry, AstState::Semilla, "a removed item with no reader", RULE_NO_READER);
            } else if edit.kind == EditKind::ItemAdded && pure.unreached.contains(&item) {
                set(&mut entry, AstState::Semilla, "an added item that no old code reaches", RULE_NOT_REACHED);
            } else {
                // [R92.3-1] Who reaches the item: the map of the diff gives it to the entry.
                let reach = pure.reach[side].get(&item).cloned().unwrap_or_default();
                set(&mut entry, AstState::NotMapped, &reach, RULE_REQUEST);
            }
        }
    }
    if entry.state == AstState::NotMapped {
        // [R79.5-1] The request: the texts of the node and a template.
        entry.before = side_text(edit, 0, trees);
        entry.after = side_text(edit, 1, trees);
    }
    entry
}

/// The path of a type of the crate by its name, for a template (R77.3-4).
fn type_path(tree: &Workspace, name: &str) -> Option<String> {
    let found: Vec<&String> = tree.items.iter().filter(|(path, item)| matches!(item.kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Type) && path.rsplit("::").next() == Some(name)).map(|(path, _)| path).collect();
    (found.len() == 1).then(|| found[0].clone())
}

/// The generic parameters of a type of the syntax tree (`<T: Clone>`) and its arguments (`<T>`).
fn generics(signature: &str, name: &str) -> (String, String) {
    let words: Vec<&str> = signature.split_whitespace().collect();
    let Some(at) = words.iter().position(|word| *word == name) else { return (String::new(), String::new()) };
    if words.get(at + 1) != Some(&"<") {
        return (String::new(), String::new());
    }
    let mut depth = 0;
    let mut list: Vec<&str> = Vec::new();
    for word in &words[at + 1..] {
        match *word {
            "<" => depth += 1,
            ">" => depth -= 1,
            _ => {}
        }
        list.push(word);
        if depth == 0 {
            break;
        }
    }
    let text = list.join(" ").replace(" :: ", "::").replace("< ", "<").replace(" >", ">").replace(" ,", ",").replace("' ", "'").replace(" : ", ": ").replace(" :", ":");
    let inner = text.trim_start_matches('<').trim_end_matches('>');
    let mut args = Vec::new();
    let mut level = 0;
    let mut part = String::new();
    for c in inner.chars().chain(std::iter::once(',')) {
        match c {
            '<' | '(' | '[' => level += 1,
            '>' | ')' | ']' => level -= 1,
            _ => {}
        }
        if c == ',' && level == 0 {
            let piece = part.trim().trim_start_matches("const ");
            let head = piece.split([':', '=']).next().unwrap_or("").trim();
            if !head.is_empty() {
                args.push(head.to_string());
            }
            part.clear();
        } else {
            part.push(c);
        }
    }
    (text.clone(), format!("<{}>", args.join(", ")))
}

/// [R79.5-1] The claim template for a node that is not mapped (13a's template rule: each
/// template passes `semilla intent check` as it is), or the cause why the node has none.
fn template(input: &StepInput, outcome: &StepOutcome, edit: &Edit, pending: &mut crate::template::Pending) -> String {
    let side = usize::from(matches!(edit.kind, EditKind::ItemAdded | EditKind::Inserted));
    let Some(item) = (if side == 1 { &edit.after } else { &edit.before }).as_ref().map(|anchor| anchor.item.clone()) else { return "no template: the node has no item".into() };
    // The names of the item in the two trees (the rename map of the step).
    let old_name = |new: &str| outcome.renames.items.iter().find(|(_, to)| *to == new).map(|(from, _)| from.clone()).unwrap_or_else(|| new.to_string());
    let new_name = |old: &str| outcome.renames.items.get(old).cloned().unwrap_or_else(|| old.to_string());
    let (old, new) = if side == 1 { (old_name(&item), item.clone()) } else { (item.clone(), new_name(&item)) };
    let found = [input.before.items.get(&old), input.after.items.get(&new)];
    let Some(kind) = found[side].map(|item| item.kind) else { return "no template: Semilla has no item of the node".into() };
    if item.contains(" (bin \"") {
        return "no template: the item is in a binary target, and the intent crate links the libraries only (design 78)".into();
    }
    if item.contains('<') && !item.contains(" (") {
        // An item of a trait impl: the name record gives its qualified path.
        let tree = if side == 1 { input.after } else { input.before };
        let def = tree.names.get().and_then(|index| crate::records::item_def(tree, &index, &item)).map(|def| def.path);
        return match def {
            Some(path) => format!("no template: an item of a trait impl has no plain path; name it by its qualified path in the scope (`#[same({path})]`, or `new`, `removed`), with a body that calls it with the types of an instance"),
            None => format!("no template: `{item}` is an item of a trait impl, and the step has no name record that gives its qualified path"),
        };
    }
    if item.contains(" (") || item.contains('#') || item.contains('!') || item.contains('{') {
        return format!("no template: `{item}` has no plain path for a claim (a twin, a macro call, an item of a target that is not a library); a claim on the items that it changes maps it");
    }
    // A function of a trait, or of a generic type: a call needs the types of an instance.
    if kind == ItemKind::Fn
        && let Some((parent, _)) = item.rsplit_once("::")
        && let Some(owner) = found[side].and_then(|_| if side == 1 { input.after.items.get(parent) } else { input.before.items.get(parent) })
    {
        let generic = generics(&owner.signature, own_name(parent)).0;
        if owner.kind == ItemKind::Trait || !generic.is_empty() {
            let what = if owner.kind == ItemKind::Trait { "a function of a trait" } else { "a function of a generic type" };
            return format!("no template: `{item}` is {what}; a claim calls it with the types of an instance (`#[same({item})]`, or `new`, `removed`, with a body that calls it in each version)");
        }
    }
    let lookup = |name: &str| type_path(input.before, name);
    match kind {
        ItemKind::Field | ItemKind::Variant => {
            // The owner type: `m::T.f`, `m::T::V.f`, `m::T::V`.
            let owner_of = |path: &str| -> String {
                let base = path.split('.').next().unwrap_or(path).to_string();
                let is_variant = |tree: &Workspace| tree.items.get(&base).is_some_and(|item| item.kind == ItemKind::Variant);
                if kind == ItemKind::Variant || (path.contains('.') && (is_variant(input.before) || is_variant(input.after))) { base.rsplit_once("::").map(|(owner, _)| owner.to_string()).unwrap_or(base) } else { base }
            };
            let (owner_old, owner_new) = (owner_of(&old), owner_of(&new));
            let field = if kind == ItemKind::Field && !item.split('.').next_back().unwrap_or("").chars().all(|c| c.is_ascii_digit()) { item.rsplit('.').next().map(|name| vec![(side == 1, name.to_string())]).unwrap_or_default() } else { Vec::new() };
            let types = [input.before.items.get(&owner_old), input.after.items.get(&owner_new)];
            // A part of a type of one version: the claim of the type maps each part inside it.
            match types {
                [Some(a), Some(_)] => {
                    let (params, args) = generics(&a.signature, own_name(&owner_old));
                    semilla_intent::parse::type_template("relation", &owner_new, &params, &args, &field)
                }
                [None, Some(b)] => {
                    let (params, args) = generics(&b.signature, own_name(&owner_new));
                    semilla_intent::parse::type_template("new", &owner_new, &params, &args, &[])
                }
                [Some(a), None] => {
                    let (params, args) = generics(&a.signature, own_name(&owner_old));
                    semilla_intent::parse::type_template("removed", &owner_old, &params, &args, &[])
                }
                [None, None] => "no template: the type of the part has no node in the trees".into(),
            }
        }
        // [R87.2-1] The names of the item come from the name record of each tree.
        ItemKind::Fn => {
            let names = [found[0].map(|_| crate::template::item_names(input.before, &old)), found[1].map(|_| crate::template::item_names(input.after, &new))];
            let named = |side: usize| names[side].as_ref().and_then(|found| found.as_ref().ok());
            let expect: Vec<String> = (0..2).filter_map(|side| named(side).map(|found| format!("{}::{}", if side == 0 { "before" } else { "after" }, found.expect))).collect();
            let forms = |make: &dyn Fn(bool) -> Result<String, String>| -> Result<Vec<String>, String> {
                let mut list = vec![make(false)?];
                if let Ok(second) = make(true)
                    && !list.contains(&second)
                {
                    list.push(second);
                }
                Ok(list)
            };
            let made = match (named(0), named(1)) {
                // [R87.2-2] A pair of two paths: no scope path names the pair.
                (Some(a), Some(b)) if a.scope != b.scope => Err(crate::template::pair_gap(&a.scope, &b.scope)),
                (Some(a), Some(b)) => forms(&|real| semilla_intent::parse::same_template_named(&b.scope, (&a.text, &crate::template::signature_names(input.before, a, real)), (&b.text, &crate::template::signature_names(input.after, b, real)))),
                (Some(a), None) if found[1].is_none() => forms(&|real| semilla_intent::parse::version_template_named("removed", &a.scope, &a.text, &crate::template::signature_names(input.before, a, real))),
                (None, Some(b)) if found[0].is_none() => forms(&|real| semilla_intent::parse::version_template_named("new", &b.scope, &b.text, &crate::template::signature_names(input.after, b, real))),
                _ => Err(names.iter().flatten().find_map(|found| found.as_ref().err().cloned()).unwrap_or_else(|| "Semilla has no item of the node".into())),
            };
            let _ = &lookup;
            match made {
                Ok(list) => pending.note(list, expect),
                Err(cause) => format!("no template: {cause}"),
            }
        }
        ItemKind::Const | ItemKind::Static => match (found[0], found[1]) {
            (Some(_), Some(_)) => semilla_intent::parse::value_template("same", &new),
            (Some(_), None) => semilla_intent::parse::value_template("removed", &old),
            _ => semilla_intent::parse::value_template("new", &new),
        },
        ItemKind::Struct | ItemKind::Enum | ItemKind::Type => {
            let any = found[side].or(found[1 - side]).expect("one side");
            let (params, args) = generics(&any.signature, own_name(&item));
            match (found[0], found[1]) {
                (Some(_), Some(_)) => semilla_intent::parse::type_template("relation", &new, &params, &args, &[]),
                (Some(_), None) => semilla_intent::parse::type_template("removed", &old, &params, &args, &[]),
                _ => semilla_intent::parse::type_template("new", &new, &params, &args, &[]),
            }
        }
        other => format!("no template: a claim names a function, a constant, or a type, and the node is in {}; a claim on the items that it changes maps it", kind_words(other)),
    }
}

/// [R79.6-1] The lines of the AST pass in the section "semantic changes": the coverage line,
/// the errors of the anchors, the cause of a step with no anchors, and each node that is not
/// mapped with its texts and its template. With `limit`, the lines of the nodes stop after
/// `limit`; with no limit (the full text), each entry shows with its rule.
pub fn lines(map: &AstMap, limit: Option<usize>, step: usize) -> Vec<String> {
    // The full text gives the count of the changed nodes, and each entry its node number `nI`
    // (the order of the syntax diff): each number from 1 to the count is in exactly one line.
    // [R90.5-3] The coverage line of the map of the diff (design 90) replaces the line "AST
    // changes": the line of the gap list has it. The full text lists each node.
    let mut lines = if limit.is_none() { vec![format!("    each changed node (of {}):", map.nodes)] } else { Vec::new() };
    for error in &map.errors {
        lines.push(format!("      ✗ anchor: {error}"));
    }
    if let Some(gap) = &map.gap {
        lines.push(format!("      note: {gap}"));
    }
    if limit.is_none() {
        for anchor in &map.anchors {
            lines.push(format!("      anchor: {anchor}"));
        }
    }
    let command = semilla_core::budget::step_text_command(step);
    let open: Vec<(usize, &AstEntry)> = map.entries.iter().enumerate().filter(|(_, entry)| entry.state == AstState::NotMapped).collect();
    let shown = limit.unwrap_or(open.len()).min(open.len());
    for (index, entry) in &open[..shown] {
        let texts = match (entry.before.is_empty(), entry.after.is_empty()) {
            (false, false) => format!("`{}` → `{}`", semilla_core::text::flat_text(&entry.before), semilla_core::text::flat_text(&entry.after)),
            (true, false) => format!("`{}`", semilla_core::text::flat_text(&entry.after)),
            (false, true) => format!("`{}`", semilla_core::text::flat_text(&entry.before)),
            (true, true) => String::new(),
        };
        let text = if texts.is_empty() { String::new() } else { format!(": {texts}") };
        lines.push(semilla_core::budget::line(&format!("      n{} {}{text}", index + 1, entry.line()), Some(&command), false, false));
        if !entry.template.is_empty() {
            let word = if entry.template.starts_with("no template") || entry.template.starts_with("no claim") || entry.template.starts_with("a ") || entry.template.starts_with("the ") { "" } else { "template: " };
            lines.push(format!("          {word}{}", entry.template.trim_end_matches('.')));
        }
    }
    if open.len() > shown {
        lines.push(format!("      … {} more not mapped (semilla status --verbose)", open.len() - shown));
    }
    if limit.is_none() {
        for (index, entry) in map.entries.iter().enumerate().filter(|(_, entry)| entry.state != AstState::NotMapped) {
            lines.push(format!("      n{} {}", index + 1, entry.line()));
        }
    }
    lines
}
