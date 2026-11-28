//! The map of the diff (design 90): the MIR alignment of each body pair, its projection onto
//! the syntax tree, and the one fact.
//!
//! - The alignment (R90.1-1) is the element compare of design 40.3 with one more step: the
//!   elements that differ pair inside each aligned region (`semilla_normal::elements`). The
//!   owner rule of design 40.3 gives each element that differs the smallest changed node that
//!   contains it. `owners::assign` keeps the result of each node (`StepReport::node_mir`).
//! - The projection (R90.2-1 to R90.2-6) gives each changed node of the syntax diff one kind.
//!   A node with no MIR element keeps the matcher of today, with the mark "syntax only".
//! - The one fact (R90.3-1): each entry of the map is a fact of the step with its kind, its
//!   nodes, its source, and its check. A fact of the agent is the source of its entries.
//!
//! - Design 92: a syntax-only node with no partner is an addition or a removal (R92.3-1), the
//!   nodes of one change make one entry (R92.6-1), and an entry has one kind (R92.7-1).
//!
//! Each rule reads data that the step already has: no search, no solver.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{Anchor, AstEntry, AstMap, AstState, Compare, EntryKind, Fact, NodeMir, Position};
use semilla_syntax::{Edit, EditKind, ItemKind, Workspace};

use crate::StepOutcome;

pub const RULE_SAME: &str = "R90.2-1";
pub const RULE_REPLACE: &str = "R90.2-2";
pub const RULE_ADD: &str = "R90.2-3";
pub const RULE_TYPE: &str = "R90.2-4";
pub const RULE_PAIR: &str = "R90.2-5";
pub const RULE_SYNTAX: &str = "R90.2-6";
pub const RULE_OPEN: &str = "R90.6-1";
pub const RULE_NO_PARTNER: &str = "R92.3-1";
pub const RULE_NO_EDIT: &str = "R92.4-1";

/// The kind of one changed node, with the mark "syntax only" and the check.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct NodeKind {
    pub kind: EntryKind,
    pub syntax_only: bool,
    /// The rule of the check.
    pub rule: &'static str,
    /// What the check found.
    pub check: String,
    /// [R94.3-2] [R94.4-3] The cause of the mark "the order can matter" (design 94).
    pub order: Option<String>,
}

/// [R93.1-1] The read form of a [`NodeKind`] (the stored form of a step result): the rule is
/// a text, and the read keeps each text one time.
#[derive(serde::Deserialize)]
struct NodeKindRead {
    kind: EntryKind,
    syntax_only: bool,
    rule: String,
    check: String,
    order: Option<String>,
}

impl<'de> serde::Deserialize<'de> for NodeKind {
    fn deserialize<D: serde::Deserializer<'de>>(reader: D) -> Result<NodeKind, D::Error> {
        let read = <NodeKindRead as serde::Deserialize>::deserialize(reader)?;
        Ok(NodeKind { kind: read.kind, syntax_only: read.syntax_only, rule: semilla_core::stored::static_text(&read.rule), check: read.check, order: read.order })
    }
}

impl NodeKind {
    fn new(kind: EntryKind, syntax_only: bool, rule: &'static str, check: String) -> NodeKind {
        NodeKind { kind, syntax_only, rule, check, order: None }
    }
}

/// [R94.1-1] The kind of a node from the transformations of the step (design 94), else `found`.
fn transformed(outcome: &StepOutcome, index: usize, found: NodeKind) -> NodeKind {
    match outcome.transforms.nodes.get(&index) {
        Some(kind) if !found.syntax_only => kind.clone(),
        _ => found,
    }
}

/// The kind of the item of an anchor of one side.
fn item_kind(anchor: &Option<semilla_core::Anchor>, tree: &Workspace) -> Option<ItemKind> {
    anchor.as_ref().and_then(|anchor| tree.items.get(&anchor.item)).map(|item| item.kind)
}

/// The type of a field (`T.x`) or the enum of a variant (`E::V`).
fn owner_type(path: &str, kind: ItemKind) -> &str {
    let cut = if kind == ItemKind::Field { path.rfind('.') } else { path.rfind("::") };
    cut.map_or(path, |at| &path[..at])
}

/// [R90.2-4] True for a node of a declaration whose change is a type change: a signature (not
/// of a macro definition: it has no MIR, R90.2-6), the member list of a struct or an enum, and
/// a field or a variant of a type that the two trees have (a field of an added type is a part
/// of the addition).
fn is_type_change(edit: &Edit, trees: [&Workspace; 2]) -> bool {
    let kinds = [item_kind(&edit.before, trees[0]), item_kind(&edit.after, trees[1])];
    // [R92.6-1] An added or a removed part of a signature (a parameter, a bound, a return
    // type): a part of the signature that changes.
    let in_signature = [&edit.before, &edit.after].iter().flat_map(|anchor| anchor.as_ref()).any(|anchor| anchor.path.first().is_some_and(|segment| segment.to_string() == "sig"));
    if in_signature && matches!(edit.kind, EditKind::Inserted | EditKind::Deleted) && !kinds.iter().flatten().any(|kind| *kind == ItemKind::Macro) {
        return true;
    }
    match edit.kind {
        EditKind::Signature => !kinds.iter().flatten().any(|kind| *kind == ItemKind::Macro),
        // [R92.6-1] The member list of a variant is a part of the definition of its enum.
        EditKind::Members => kinds.iter().flatten().any(|kind| matches!(kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Variant)),
        EditKind::ItemAdded | EditKind::ItemRemoved | EditKind::Modified | EditKind::Replaced => [&edit.before, &edit.after].iter().zip(kinds).any(|(anchor, kind)| match (anchor, kind) {
            (Some(anchor), Some(kind @ (ItemKind::Field | ItemKind::Variant))) => {
                let owner = owner_type(&anchor.item, kind);
                trees.iter().all(|tree| tree.items.contains_key(owner))
            }
            _ => false,
        }),
        _ => false,
    }
}

/// The count text of the elements of a node.
fn elements(count: usize, word: &str) -> String {
    format!("{count} {word} MIR element{}", if count == 1 { "" } else { "s" })
}

/// [R90.2-1 to R90.2-6, R90.6-1] The kind of one changed node from its MIR evidence (design
/// 90). `fact` is the fact of today that holds the node.
pub fn node_kind(edit: &Edit, mir: Option<&NodeMir>, fact: Option<&Fact>, trees: [&Workspace; 2]) -> NodeKind {
    // [R90.2-5] A rename or a move of an item (a pair of the hints, the compiler identity, or
    // the matcher): the pair passed its check, so the fact exists. A renamed local is not a
    // definition: its kind comes from the MIR.
    if let Some(fact) = fact.filter(|fact| matches!(fact.category, semilla_core::Category::Rename | semilla_core::Category::Move) && !matches!(fact.part.as_str(), "body" | "parameter rename" | "indirect change")) {
        let kind = if fact.category == semilla_core::Category::Rename { EntryKind::Rename } else { EntryKind::Move };
        return NodeKind::new(kind, false, RULE_PAIR, "the pair check holds".into());
    }
    // [R90.2-4] A signature, a field, or a variant.
    if is_type_change(edit, trees) {
        return NodeKind::new(EntryKind::TypeChange, false, RULE_TYPE, "a changed declaration; the uses with a MIR change are its bodies".into());
    }
    let owned = mir.map(|mir| (mir.paired, mir.alone)).filter(|(paired, alone)| paired.iter().chain(alone).any(|count| *count > 0));
    if let Some((paired, alone)) = owned {
        // [R90.2-3] Each element of an added node is new; each element of a removed node is
        // removed. An element of the other side that the node owns is an element of an
        // enclosing node (a branch of a `match` with a new arm).
        // A whole added or removed item also owns the changed elements of its uses (a symbol
        // link, design 44): it stays an addition or a removal.
        if edit.kind == EditKind::ItemAdded {
            return NodeKind::new(EntryKind::Add, false, RULE_ADD, elements(alone[1] + paired[1], "new or changed"));
        }
        if edit.kind == EditKind::ItemRemoved {
            return NodeKind::new(EntryKind::Delete, false, RULE_ADD, elements(alone[0] + paired[0], "removed or changed"));
        }
        let added = edit.kind == EditKind::Inserted;
        let removed = edit.kind == EditKind::Deleted;
        if added && alone[1] > 0 {
            return NodeKind::new(EntryKind::Add, false, RULE_ADD, elements(alone[1], "new"));
        }
        if removed && alone[0] > 0 {
            return NodeKind::new(EntryKind::Delete, false, RULE_ADD, elements(alone[0], "removed"));
        }
        // [R90.2-2] A node with a changed element.
        let count = paired[0].max(paired[1]) + alone[0] + alone[1];
        return NodeKind::new(EntryKind::Replace, false, RULE_REPLACE, elements(count, "changed"));
    }
    let mir = mir.cloned().unwrap_or_default();
    if mir.one_side {
        return match edit.kind {
            EditKind::ItemRemoved | EditKind::Deleted => NodeKind::new(EntryKind::Delete, false, RULE_ADD, "a body of the old MIR only".into()),
            _ => NodeKind::new(EntryKind::Add, false, RULE_ADD, "a body of the new MIR only".into()),
        };
    }
    // A doc text, a comment, a visibility, and an attribute have no MIR element of their own.
    let no_element = matches!(edit.kind, EditKind::Docs | EditKind::Comment | EditKind::Visibility | EditKind::Attrs);
    if !no_element {
        // [R90.6-1] No unique answer: an element that differs with no owner, or no element data.
        if mir.doubt {
            return NodeKind::new(EntryKind::NotMapped, false, RULE_OPEN, "the alignment gives no unique answer".into());
        }
        // [R90.2-1] Each element of the node aligns with an equal element.
        if mir.equal {
            return NodeKind::new(EntryKind::SameCode, false, RULE_SAME, "equal MIR".into());
        }
    }
    // [R90.2-6] No MIR element: the matcher of today.
    let kind = match edit.kind {
        EditKind::ItemAdded | EditKind::Inserted => EntryKind::Add,
        EditKind::ItemRemoved | EditKind::Deleted => EntryKind::Delete,
        EditKind::Moved => EntryKind::Move,
        _ => EntryKind::Replace,
    };
    NodeKind::new(kind, true, RULE_SYNTAX, "no MIR element; the matcher".into())
}

/// [R90.2-1] Gives each node of the map its kind and its entry, after the AST pass of design 79
/// (`astmap::apply`). `indices` has the index of the edit of each entry. A node that the
/// projection decides is mapped (R90.6-1: only a node with no unique answer is not mapped).
/// Returns the entries that are not mapped now and were mapped by the AST pass: they need a
/// template.
pub(crate) fn nodes(outcome: &StepOutcome, trees: [&Workspace; 2], map: &mut AstMap, indices: &[usize]) -> Vec<usize> {
    let mut open = Vec::new();
    for (place, (entry, index)) in map.entries.iter_mut().zip(indices).enumerate() {
        let edit = &outcome.edits[*index];
        let fact = outcome.fact_of.get(*index).copied().flatten().and_then(|fact| outcome.report.facts.get(fact));
        let found = transformed(outcome, *index, no_partner(claimed(node_kind(edit, outcome.report.node_mir.get(index), fact, trees), entry, edit, trees), entry, edit));
        entry.entry = match (fact, outcome.owners.get(*index).copied().flatten()) {
            (Some(fact), _) => fact.id.clone(),
            (None, Some(claim)) => outcome.report.claims.get(claim).map(|claim| claim.claim.clone()).unwrap_or_default(),
            (None, None) => String::new(),
        };
        set_state(entry, &found, &mut open, place);
        // [R90.6-1] A node that stays "not mapped" (a syntax-only node of the matcher of today)
        // has the kind "not mapped".
        entry.kind = Some(if entry.state == AstState::NotMapped { EntryKind::NotMapped } else { found.kind });
        entry.syntax_only = found.syntax_only;
    }
    open
}

/// [R90.6-1] A node that an anchor of a claim maps is stated: with no unique alignment, it is a
/// changed node of the claim ("replace"), not a request.
/// A node of test, bench, or example code with no unique alignment keeps the state of the
/// matcher of today (R79.3-2): no claim can name it (the intent crate links the libraries), so
/// a request could never be met. Its kind comes from the edit.
fn claimed(found: NodeKind, entry: &AstEntry, edit: &Edit, trees: [&Workspace; 2]) -> NodeKind {
    if found.kind == EntryKind::NotMapped && entry.state == AstState::Claim {
        return NodeKind::new(EntryKind::Replace, found.syntax_only, RULE_REPLACE, "a node of a claim with no unique alignment".into());
    }
    let test = [(&edit.before, 0), (&edit.after, 1)].iter().any(|(anchor, side)| anchor.as_ref().is_some_and(|anchor| crate::astmap::test_code(trees[*side], &anchor.item)));
    if found.kind == EntryKind::NotMapped && entry.state == AstState::Semilla && (entry.rule == crate::astmap::RULE_NO_CODE || test) {
        let kind = match edit.kind {
            EditKind::ItemAdded | EditKind::Inserted => EntryKind::Add,
            EditKind::ItemRemoved | EditKind::Deleted => EntryKind::Delete,
            _ => EntryKind::Replace,
        };
        return NodeKind::new(kind, found.syntax_only, RULE_OPEN, "test code with no unique alignment; no claim names test code (R79.3-2)".into());
    }
    found
}

/// [R92.3-1] A syntax-only node that the matcher of today leaves with no partner (the AST pass
/// gives it no rule) is an addition or a removal (decision 13.6 of the draft of the intent
/// language): an added node "add", a removed node "delete". The check gives who reaches it in
/// the other tree (the name record). A syntax-only node of another edit keeps its answer.
fn no_partner(found: NodeKind, entry: &AstEntry, edit: &Edit) -> NodeKind {
    if !found.syntax_only || entry.state != AstState::NotMapped {
        return found;
    }
    let kind = match edit.kind {
        EditKind::ItemAdded | EditKind::Inserted => EntryKind::Add,
        EditKind::ItemRemoved | EditKind::Deleted => EntryKind::Delete,
        _ => return found,
    };
    // A statement has the item of its body as who reaches it.
    let item = edit.anchor().filter(|anchor| !anchor.path.is_empty()).map(|anchor| anchor.item.clone());
    let reach = match (entry.by.is_empty(), item) {
        (false, _) => entry.by.clone(),
        (true, Some(item)) => format!("a statement of `{item}`"),
        (true, None) => "who reaches it: no data".to_string(),
    };
    NodeKind::new(kind, true, RULE_NO_PARTNER, format!("no MIR element and no partner; {reach}"))
}

/// The state of one entry of the map from its kind.
fn set_state(entry: &mut AstEntry, found: &NodeKind, open: &mut Vec<usize>, place: usize) {
    if found.kind == EntryKind::NotMapped {
        if entry.state != AstState::NotMapped {
            entry.state = AstState::NotMapped;
            entry.by = String::new();
            entry.rule = RULE_OPEN.into();
            open.push(place);
        }
        return;
    }
    // [R90.2-6] A node with no MIR element keeps the answer of the matcher of today (the AST
    // pass of design 79). A claim keeps its anchor: it is the source of the entry.
    // [R92.3-1] A syntax-only node with no partner is an addition or a removal: it has who
    // reaches it, not a request.
    if entry.state == AstState::NotMapped && (!found.syntax_only || found.rule == RULE_NO_PARTNER) {
        entry.state = AstState::Semilla;
        entry.by = if found.rule == RULE_NO_PARTNER { found.check.clone() } else { format!("the map of the diff: {}", found.kind.name()) };
        entry.rule = found.rule.into();
        // [R92.5-1] A node with no partner keeps its texts and its template: the line "not
        // stated" of its entry gives the template as the statement that states the entry.
        if found.rule != RULE_NO_PARTNER {
            entry.template = String::new();
            entry.before = String::new();
            entry.after = String::new();
        }
    }
}

/// The priority of a kind for the kind of an entry with more than one node: a real edit first.
fn priority(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::TypeChange => 0,
        EntryKind::Replace => 1,
        EntryKind::Add => 2,
        EntryKind::Delete => 3,
        EntryKind::NotMapped => 4,
        EntryKind::Extract => 5,
        EntryKind::Inline => 6,
        EntryKind::Reorder => 7,
        EntryKind::Rename => 8,
        EntryKind::Move => 9,
        EntryKind::SameCode => 10,
    }
}

/// The kind of each node of the map (in the order of the entries of the map), after the AST pass
/// and the projection (`nodes`).
fn node_kinds(outcome: &StepOutcome, trees: [&Workspace; 2], map: &AstMap, indices: &[usize]) -> Vec<NodeKind> {
    map.entries
        .iter()
        .zip(indices)
        .map(|(entry, index)| {
            let edit = &outcome.edits[*index];
            let fact = outcome.fact_of.get(*index).copied().flatten().and_then(|fact| outcome.report.facts.get(fact));
            let found = transformed(outcome, *index, claimed(node_kind(edit, outcome.report.node_mir.get(index), fact, trees), entry, edit, trees));
            // [R92.3-1] A syntax-only node with no partner: the projection gave its kind and who
            // reaches it.
            if entry.rule == RULE_NO_PARTNER {
                return NodeKind::new(found.kind, true, RULE_NO_PARTNER, entry.by.clone());
            }
            match (entry.state, found.syntax_only) {
                // [R90.2-6] The matcher of today leaves a node with no MIR element with no partner.
                (AstState::NotMapped, true) => NodeKind::new(EntryKind::NotMapped, true, RULE_SYNTAX, format!("no MIR element; the matcher of today maps no partner ({})", entry.rule)),
                (AstState::NotMapped, false) => NodeKind::new(EntryKind::NotMapped, false, RULE_OPEN, "the alignment gives no unique answer".into()),
                _ => found,
            }
        })
        .collect()
}

/// The mark in the check of a fact that [`shape`] made from two facts or more (R92.6-1), or from
/// a part of a fact (R92.7-1). [`entries`] writes the full check.
const MERGED: &str = "one change (R92.6-1)";
const SPLIT: &str = "split by kind (R92.7-1)";

/// [R92.6-1] [R92.7-1] The entries of the map are the facts of the step (design 92). Two facts
/// whose nodes come from one change make one fact: the nodes own one changed MIR element by its
/// place (rules 1 and 1b of design 40.3), and the nodes have one kind. Then a fact with nodes
/// of more than one kind splits into one fact for each kind: the fact keeps the nodes of its
/// strongest kind, and each other kind gets a new fact with the texts of its nodes. It runs in
/// the AST pass, after the projection and before the facts of the agent, the gap list, and
/// the tags: they read the facts of the result.
pub(crate) fn shape(outcome: &mut StepOutcome, trees: [&Workspace; 2], map: &mut AstMap, indices: &[usize]) {
    let kinds = node_kinds(outcome, trees, map, indices);
    let count = outcome.report.facts.len();
    let place_of: BTreeMap<usize, usize> = indices.iter().enumerate().map(|(place, index)| (*index, place)).collect();
    let fact_at = |fact_of: &[Option<usize>], index: usize| fact_of.get(index).copied().flatten().filter(|fact| *fact < count);
    // [R92.6-1] The groups of the facts of one change (the lowest fact of a group is its root).
    let mut parent: Vec<usize> = (0..count).collect();
    fn root(parent: &mut [usize], mut at: usize) -> usize {
        while parent[at] != at {
            parent[at] = parent[parent[at]];
            at = parent[at];
        }
        at
    }
    // [R94.1-1] The node of a callee and the nodes of its call make one entry (design 94).
    for (a, b) in outcome.transforms.joins.clone() {
        let (Some(x), Some(y)) = (fact_at(&outcome.fact_of, a), fact_at(&outcome.fact_of, b)) else { continue };
        let (x, y) = (root(&mut parent, x), root(&mut parent, y));
        if x != y {
            parent[x.max(y)] = x.min(y);
        }
    }
    for (place, index) in indices.iter().enumerate() {
        let (Some(mir), Some(own)) = (outcome.report.node_mir.get(index), fact_at(&outcome.fact_of, *index)) else { continue };
        for other in &mir.shared {
            let (Some(theirs), Some(other_place)) = (fact_at(&outcome.fact_of, *other), place_of.get(other)) else { continue };
            if theirs == own || kinds[place].kind != kinds[*other_place].kind {
                continue;
            }
            let (a, b) = (root(&mut parent, own), root(&mut parent, theirs));
            if a != b {
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    let roots: Vec<usize> = (0..count).map(|fact| root(&mut parent, fact)).collect();
    if roots.iter().enumerate().any(|(fact, root)| *root != fact) {
        for fact in 0..count {
            if roots[fact] != fact {
                let from = outcome.report.facts[fact].clone();
                let into_id = outcome.report.facts[roots[fact]].id.clone();
                absorb(&mut outcome.report.facts[roots[fact]], from.clone());
                if let Some(flow) = outcome.report.flow.as_mut() {
                    move_lines(flow, &from.id, &into_id);
                }
            }
        }
        let mut new_index: Vec<Option<usize>> = vec![None; count];
        let mut next = 0;
        for fact in 0..count {
            if roots[fact] == fact {
                new_index[fact] = Some(next);
                next += 1;
            }
        }
        for slot in outcome.fact_of.iter_mut() {
            if let Some(fact) = slot.filter(|fact| *fact < count) {
                *slot = new_index[roots[fact]];
            }
        }
        let mut at = 0;
        outcome.report.facts.retain(|_| {
            at += 1;
            roots[at - 1] == at - 1
        });
    }
    // [R92.7-1] One kind for each fact.
    let count = outcome.report.facts.len();
    let mut held: Vec<Vec<(usize, NodeKind)>> = vec![Vec::new(); count];
    for (place, index) in indices.iter().enumerate() {
        if let Some(fact) = outcome.fact_of.get(*index).copied().flatten().filter(|fact| *fact < count) {
            held[fact].push((place, kinds[place].clone()));
        }
    }
    let mut groups: Vec<BTreeMap<EntryKind, Vec<usize>>> = vec![BTreeMap::new(); count];
    for (fact, nodes) in held.iter_mut().enumerate() {
        one_change(nodes.iter_mut().map(|(_, found)| found));
        for (place, found) in nodes.iter() {
            groups[fact].entry(found.kind).or_default().push(*place);
            // [R92.6-1] A node that the change of its fact explains is mapped.
            let entry = &mut map.entries[*place];
            if found.kind != kinds[*place].kind && entry.state == AstState::NotMapped {
                entry.state = AstState::Semilla;
                entry.by = "a part of one change (the fact of the syntax diff)".into();
                entry.rule = RULE_ONE_CHANGE.into();
                entry.template = String::new();
                entry.before = String::new();
                entry.after = String::new();
            }
            if entry.kind.is_some() {
                entry.kind = Some(found.kind);
            }
        }
    }
    let mut next_id = outcome.report.facts.iter().filter_map(|fact| fact.id.strip_prefix('f').and_then(|number| number.parse::<usize>().ok())).max().unwrap_or(0) + 1;
    for (fact, by_kind) in groups.into_iter().enumerate() {
        let Some(main) = by_kind.keys().copied().min_by_key(|kind| priority(*kind)) else { continue };
        for (kind, places) in by_kind.into_iter().filter(|(kind, _)| *kind != main) {
            let _ = kind;
            let part = split_fact(&outcome.report.facts[fact], &places.iter().map(|place| (&map.entries[*place], &outcome.edits[indices[*place]])).collect::<Vec<_>>(), next_id, trees);
            next_id += 1;
            let at = outcome.report.facts.len();
            outcome.report.facts.push(part);
            for place in places {
                outcome.fact_of[indices[place]] = Some(at);
            }
        }
    }
    // [R94.1-2] The code that a transformation does not explain: a real edit of its own.
    for left in outcome.transforms.left.clone() {
        let fact = crate::transforms::left_fact(trees, &left, next_id);
        next_id += 1;
        outcome.report.facts.push(fact);
    }
    // The entry of each node after the changes.
    for (entry, index) in map.entries.iter_mut().zip(indices) {
        if let Some(fact) = outcome.fact_of.get(*index).copied().flatten().and_then(|fact| outcome.report.facts.get(fact)) {
            entry.entry = fact.id.clone();
        }
    }
}

/// [R92.6-1] The kinds of the nodes of one fact that come from one change (design 92):
/// - a forced edit at a use of a changed declaration (a site of design 22.6, an edit whose MIR
///   changes with the declaration) is a part of the type change (R90.2-4: the entry lists each
///   use whose MIR changes): each other node of a fact with a type change is a type change;
/// - an old node and a new node that one fact pairs (a removed and an added node of one fact of
///   the syntax diff) are one replace: the old node becomes the new node.
fn one_change<'a>(nodes: impl Iterator<Item = &'a mut NodeKind>) {
    let mut nodes: Vec<&mut NodeKind> = nodes.collect();
    let has = |nodes: &[&mut NodeKind], kind: EntryKind| nodes.iter().any(|found| found.kind == kind);
    let to = if has(&nodes, EntryKind::TypeChange) {
        // Each other node of the fact is a forced edit at a use of the declaration.
        for found in nodes.iter_mut().filter(|found| found.kind != EntryKind::TypeChange) {
            found.kind = EntryKind::TypeChange;
            found.check = format!("{}; a use of a changed declaration (R90.2-4, {RULE_ONE_CHANGE})", found.check);
        }
        return;
    } else if has(&nodes, EntryKind::Add) && has(&nodes, EntryKind::Delete) {
        Some((EntryKind::Replace, "an old node and a new node of one fact: the old node becomes the new node"))
    } else {
        None
    };
    let Some((kind, why)) = to else { return };
    for found in nodes.iter_mut().filter(|found| found.kind != kind && matches!(found.kind, EntryKind::Replace | EntryKind::Add | EntryKind::Delete)) {
        found.kind = kind;
        found.check = format!("{}; {why} ({RULE_ONE_CHANGE})", found.check);
    }
}

const RULE_ONE_CHANGE: &str = "R92.6-1";

/// [R92.6-1] Adds the fact `from` of one change to the fact `into`: the texts, the lines, the
/// bodies, and the marks of the two facts.
fn absorb(into: &mut Fact, from: Fact) {
    into.description = format!("{}; {}", into.description, from.description);
    into.positions = Position::normal(into.positions.iter().cloned().chain(from.positions).collect());
    for body in from.affected {
        if !into.affected.contains(&body) {
            into.affected.push(body);
        }
    }
    for body in from.equal_sites {
        if !into.equal_sites.contains(&body) && !into.affected.contains(&body) {
            into.equal_sites.push(body);
        }
    }
    into.equal_sites.retain(|body| !into.affected.contains(body));
    into.sites.extend(from.sites);
    for (symbol, bodies) in from.through {
        let known = into.through.entry(symbol).or_default();
        for body in bodies {
            if !known.contains(&body) {
                known.push(body);
            }
        }
    }
    if from.compare == Compare::CodeDifferent || into.compare == Compare::Off {
        into.compare = from.compare;
    }
    let join = |a: Option<String>, b: Option<String>| match (a, b) {
        (Some(a), Some(b)) if a != b => Some(format!("{a}; {b}")),
        (a, b) => a.or(b),
    };
    into.spotlight |= from.spotlight;
    into.spotlight_why = join(into.spotlight_why.take(), from.spotlight_why);
    into.note = join(into.note.take(), from.note);
    into.reason = into.reason.take().or(from.reason);
    into.branches = into.branches.take().or(from.branches);
    into.causes.extend(from.causes);
    into.unsafe_code |= from.unsafe_code;
    into.test &= from.test;
    into.other_edits = into.other_edits.max(from.other_edits).saturating_sub(1);
    into.check = MERGED.into();
}

/// [R92.6-1] The lines of the graph section of a fact go to the fact that holds it now.
fn move_lines(flow: &mut semilla_core::report::FlowReport, from: &str, into: &str) {
    for lines in [&mut flow.facts, &mut flow.equivalence, &mut flow.expansion_lines] {
        if let Some(moved) = lines.remove(from) {
            let known = lines.entry(into.to_string()).or_default();
            for line in moved {
                if !known.contains(&line) {
                    known.push(line);
                }
            }
        }
    }
    if let Some(moved) = flow.reaches.remove(from) {
        flow.reaches.entry(into.to_string()).or_insert(moved);
    }
}

/// [R92.7-1] The fact of the nodes of one kind of a fact: the texts of the nodes, their lines,
/// and the category and the compare result of the fact. The lines below the fact (the bodies,
/// the spotlight) stay with the fact.
fn split_fact(fact: &Fact, nodes: &[(&AstEntry, &Edit)], id: usize, trees: [&Workspace; 2]) -> Fact {
    let file_of = |edit: &Edit| edit.file.clone().or_else(|| [(&edit.after, 1), (&edit.before, 0)].iter().find_map(|(anchor, side)| anchor.as_ref().and_then(|anchor| trees[*side].items.get(&anchor.item)).map(|item| item.file.clone())));
    let positions = Position::normal(nodes.iter().filter_map(|(_, edit)| Some(Position { file: file_of(edit)?, old: edit.after.is_none(), start: edit.line, end: edit.line })).collect());
    let texts: Vec<String> = nodes
        .iter()
        .map(|(entry, edit)| {
            let side = |text: &Option<String>| text.as_deref().map(semilla_core::text::flat_text);
            let sides = match (side(&edit.text_before), side(&edit.text_after)) {
                (Some(old), Some(new)) => format!(": `{old}` → `{new}`"),
                (Some(old), None) => format!(": `{old}`"),
                (None, Some(new)) => format!(": `{new}`"),
                (None, None) => String::new(),
            };
            format!("{} {} `{}`{sides}", entry.change, entry.node, entry.at)
        })
        .collect();
    let first = nodes[0].1;
    Fact {
        id: format!("f{id}"),
        at: first.anchor().cloned().or_else(|| fact.at.clone()).map(|anchor| Anchor::item(&anchor.item)).or_else(|| fact.at.clone()),
        file: file_of(first).or_else(|| fact.file.clone()),
        positions,
        part: fact.part.clone(),
        description: texts.join("; "),
        status: fact.status.clone(),
        category: fact.category,
        compare: fact.compare,
        test: fact.test,
        check: SPLIT.into(),
        ..Default::default()
    }
}

/// [R97.4-1] Moves a set of nodes to a fact of their own: each fact with nodes of the set and
/// other nodes gives a new fact with the nodes of the set (design 97). The old fact keeps its
/// text and its other nodes, as a split by kind does (R92.7-1).
pub(crate) fn split_off(outcome: &mut StepOutcome, trees: [&Workspace; 2], map: &mut AstMap, indices: &[usize], moved: &BTreeSet<usize>) {
    let count = outcome.report.facts.len();
    let place_of: BTreeMap<usize, usize> = indices.iter().enumerate().map(|(place, index)| (*index, place)).collect();
    let mut next_id = outcome.report.facts.iter().filter_map(|fact| fact.id.strip_prefix('f').and_then(|number| number.parse::<usize>().ok())).max().unwrap_or(0) + 1;
    let mut target: Option<usize> = None;
    for fact in 0..count {
        let held: Vec<usize> = indices.iter().copied().filter(|index| outcome.fact_of.get(*index).copied().flatten() == Some(fact)).collect();
        let (inside, outside): (Vec<usize>, Vec<usize>) = held.iter().partition(|index| moved.contains(index));
        if inside.is_empty() || outside.is_empty() {
            continue;
        }
        let at = match target {
            Some(at) => at,
            None => {
                let nodes: Vec<(&AstEntry, &Edit)> = inside.iter().filter_map(|index| Some((map.entries.get(*place_of.get(index)?)?, &outcome.edits[*index]))).collect();
                if nodes.is_empty() {
                    continue;
                }
                let mut part = split_fact(&outcome.report.facts[fact], &nodes, next_id, trees);
                part.check = String::new();
                next_id += 1;
                outcome.report.facts.push(part);
                outcome.report.facts.len() - 1
            }
        };
        target = Some(at);
        for index in inside {
            outcome.fact_of[index] = Some(at);
        }
    }
    for (entry, index) in map.entries.iter_mut().zip(indices) {
        if let Some(fact) = outcome.fact_of.get(*index).copied().flatten().and_then(|fact| outcome.report.facts.get(fact)) {
            entry.entry = fact.id.clone();
        }
    }
}

/// [R90.3-1] Gives each fact of the step the fields of the one fact (design 90): its nodes, its
/// kind, its check, its source, and the mark "not stated" (R90.5-2). It runs after the facts of
/// the agent and the claims of the intent files have their results.
pub(crate) fn entries(outcome: &mut StepOutcome, trees: [&Workspace; 2]) {
    let Some(map) = outcome.report.ast.clone() else { return };
    let indices: Vec<usize> = outcome.edits.iter().enumerate().filter(|(_, edit)| crate::astmap::is_node(edit)).map(|(index, _)| index).collect();
    // The nodes of each fact, with their kinds and their claims.
    let count = outcome.report.facts.len();
    let kinds = node_kinds(outcome, trees, &map, &indices);
    let mut held: Vec<Vec<(usize, NodeKind, &AstEntry)>> = vec![Vec::new(); count];
    for (place, ((entry, index), found)) in map.entries.iter().zip(&indices).zip(kinds).enumerate() {
        let Some(fact) = outcome.fact_of.get(*index).copied().flatten().filter(|fact| *fact < count) else { continue };
        held[fact].push((place + 1, found, entry));
    }
    for nodes in held.iter_mut() {
        one_change(nodes.iter_mut().map(|(_, found, _)| found));
    }
    // The file edits of each fact (a file is not a node of a tree).
    let mut files: Vec<Option<EditKind>> = vec![None; count];
    for (index, edit) in outcome.edits.iter().enumerate().filter(|(_, edit)| !crate::astmap::is_node(edit)) {
        if let Some(fact) = outcome.fact_of.get(index).copied().flatten().filter(|fact| *fact < count) {
            files[fact] = Some(edit.kind);
        }
    }
    let agents = outcome.report.agent_facts.clone();
    let explained = outcome.explained.clone();
    // [R94.4-3] The types whose members get a new order, and the bodies that the order changes.
    let reordered = crate::transforms::reordered_types(outcome);
    let changed_bodies: Vec<(String, String, String)> = reordered.iter().flat_map(|(ty, id)| crate::transforms::changed_by(outcome, ty).into_iter().map(move |body| (body.name.clone(), ty.clone(), id.clone()))).collect();
    for (fact_index, fact) in outcome.report.facts.iter_mut().enumerate() {
        let nodes = &held[fact_index];
        fact.nodes = nodes.iter().map(|(number, ..)| *number).collect();
        // [R92.6-1] [R92.7-1] The mark of a fact that the shape of the entries made.
        let shaped = if fact.check == MERGED || fact.check == SPLIT { format!("; {}", fact.check) } else { String::new() };
        let (kind, syntax_only, check) = match nodes.iter().min_by_key(|(_, found, _)| (priority(found.kind), found.syntax_only)) {
            Some((_, found, _)) => {
                let more = if nodes.len() > 1 { format!("; {} nodes", nodes.len()) } else { String::new() };
                (found.kind, nodes.iter().all(|(_, found, _)| found.syntax_only), format!("{}, {}{more}{shaped}", found.rule, found.check))
            }
            // [R94.1-2] The entry of the code that a transformation does not explain keeps its
            // kind and its check.
            None if fact.part == "transformation" && fact.kind.is_some() => (fact.kind.unwrap_or(EntryKind::Replace), false, fact.check.clone()),
            None => no_node_kind(fact, files[fact_index]),
        };
        fact.kind = Some(kind);
        fact.syntax_only = syntax_only;
        fact.check = check;
        // [R94.3-2] [R94.4-3] A transformation whose parts depend on each other.
        fact.order = nodes.iter().find_map(|(_, found, _)| found.order.clone().filter(|_| found.kind == kind));
        if fact.order.is_none() && nodes.is_empty() {
            fact.order = no_node_order(fact, &reordered, &changed_bodies);
            if let Some(cause) = &fact.order
                && fact.no_edit()
            {
                fact.check = format!("{}, {cause}", crate::transforms::RULE_ORDER);
            }
        }
        // [R90.3-1] The source: the facts of the agent that state the entry, and the claims
        // whose anchors map its nodes.
        let mut source: Vec<String> = Vec::new();
        let item = fact.at.as_ref().map(|at| at.item.clone());
        for agent in agents.iter().filter(|agent| agent.holds) {
            let below = agent.facts.contains(&fact.id) || agent.partial.iter().any(|part| part.split([' ', ':']).next() == Some(fact.id.as_str()));
            let change = agent.kind == "change" && item.as_ref().is_some_and(|item| agent.places.iter().any(|place| place == item || item.starts_with(&format!("{place}::"))));
            if (below || change) && !source.contains(&agent.id) {
                source.push(agent.id.clone());
            }
        }
        for (_, _, entry) in nodes.iter().filter(|(_, _, entry)| entry.state == AstState::Claim) {
            for id in entry.by.split(", ").filter(|id| !id.is_empty()) {
                if !source.iter().any(|known| known == id) {
                    source.push(id.to_string());
                }
            }
        }
        let proved = item.as_ref().is_some_and(|item| explained.contains(item));
        fact.source = source;
        // [R90.5-2] A real edit that no statement names.
        fact.not_stated = fact.counts_as_real() && fact.source.is_empty() && !proved;
    }
}

/// [R94.4-3] The mark "the order can matter" of an entry with no node: an item with a new order
/// to an item that can give a macro (design 54.2), and the compiled code of a body with no edit
/// whose MIR a new order of the members of a type changes (a derived `Ord`, `Debug`, a read of
/// a discriminant).
fn no_node_order(fact: &Fact, reordered: &[(String, String)], changed: &[(String, String, String)]) -> Option<String> {
    if fact.part == "macro scope" {
        return Some("an item gets a new order to an item that can give a macro: a macro call can get a different definition (design 54.2)".into());
    }
    let _ = reordered;
    changed.iter().find(|(body, _, _)| fact.affected.contains(body)).map(|(body, ty, id)| {
        let id = if id.is_empty() { String::new() } else { format!(" ({id})") };
        format!("the new order of the members of `{ty}`{id} changes the MIR of `{body}`: it reads the order")
    })
}

/// [R90.3-2] The kind of an entry with no changed node: a file edit, the compiled code of a body
/// with no edit at its place, or a line of the public API. The entry keeps its fact of today.
fn no_node_kind(fact: &Fact, file: Option<EditKind>) -> (EntryKind, bool, String) {
    use semilla_core::Category;
    let kind = match (file, fact.category) {
        (Some(EditKind::FileAdded), _) => EntryKind::Add,
        (Some(EditKind::FileRemoved), _) => EntryKind::Delete,
        (Some(_), _) => EntryKind::Replace,
        (None, Category::Rename) => EntryKind::Rename,
        (None, Category::Move) => EntryKind::Move,
        (None, Category::Added) => EntryKind::Add,
        (None, Category::Removed) => EntryKind::Delete,
        // [R94.4-1] Items at a new place in their module: the same items in another order.
        (None, Category::Layout) if fact.description.contains("at a new place") => EntryKind::Reorder,
        // [R94.4-3] An item with a new order to an item that can give a macro.
        (None, Category::Macro) if fact.part == "macro scope" => EntryKind::Reorder,
        // A new layout: the tokens are equal.
        (None, Category::Layout) => EntryKind::SameCode,
        (None, _) => EntryKind::Replace,
    };
    // [R92.4-1] An entry with no node and no changed line of its own is not a real edit: the
    // compiled code of a body with no edit at its place, or a line of the public API of a pair.
    let no_edit = fact.positions.is_empty() && fact.at.is_some() && fact.part != "identity";
    let check = match (file, kind) {
        (Some(_), _) => "R90.3-2, a file edit (no tree)".to_string(),
        // [R90.2-5] A pair of items: the fact exists only when the pair passes its check.
        (None, EntryKind::Rename | EntryKind::Move) if no_edit => format!("{RULE_NO_EDIT}, a line of the public API of a pair; the pair check holds ({RULE_PAIR})"),
        (None, EntryKind::Rename | EntryKind::Move) => format!("{RULE_PAIR}, the pair check holds"),
        (None, EntryKind::SameCode) => "R90.3-2, equal tokens (a new layout)".to_string(),
        (None, EntryKind::Reorder) if fact.part == "macro scope" => format!("{}, the items in another order", crate::transforms::RULE_ORDER),
        (None, EntryKind::Reorder) => format!("{}, the items in another order with equal tokens; the compiled code is the same", crate::transforms::RULE_SAME_ORDER),
        (None, _) if no_edit => format!("{RULE_NO_EDIT}, no node and no changed line of its own"),
        (None, _) => "R90.3-2, no changed node (the fact of today)".to_string(),
    };
    (kind, false, check)
}

/// [R90.7-1] The problems of the properties of the map of a step: complete (each changed node
/// is in exactly one entry) and checked (each entry has a check). An empty list: the map
/// keeps the properties.
pub fn problems(map: &AstMap, facts: &[Fact]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    for fact in facts {
        if fact.kind.is_none() || fact.check.is_empty() {
            problems.push(format!("{} has no kind or no check", fact.id));
        }
        for node in &fact.nodes {
            if !seen.insert(*node) {
                problems.push(format!("n{node} is in two entries"));
            }
        }
    }
    for (index, entry) in map.entries.iter().enumerate() {
        if entry.kind.is_none() {
            problems.push(format!("n{} has no kind", index + 1));
        }
        if entry.entry.is_empty() && !seen.contains(&(index + 1)) {
            problems.push(format!("n{} is in no entry", index + 1));
        }
    }
    problems
}
