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
//! Each rule reads data that the step already has: no search, no solver.

use std::collections::BTreeSet;

use semilla_core::{AstEntry, AstMap, AstState, EntryKind, Fact, NodeMir};
use semilla_syntax::{Edit, EditKind, ItemKind, Workspace};

use crate::StepOutcome;

pub const RULE_SAME: &str = "R90.2-1";
pub const RULE_REPLACE: &str = "R90.2-2";
pub const RULE_ADD: &str = "R90.2-3";
pub const RULE_TYPE: &str = "R90.2-4";
pub const RULE_PAIR: &str = "R90.2-5";
pub const RULE_SYNTAX: &str = "R90.2-6";
pub const RULE_OPEN: &str = "R90.6-1";

/// The kind of one changed node, with the mark "syntax only" and the check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeKind {
    pub kind: EntryKind,
    pub syntax_only: bool,
    /// The rule of the check.
    pub rule: &'static str,
    /// What the check found.
    pub check: String,
}

impl NodeKind {
    fn new(kind: EntryKind, syntax_only: bool, rule: &'static str, check: String) -> NodeKind {
        NodeKind { kind, syntax_only, rule, check }
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
    match edit.kind {
        EditKind::Signature => !kinds.iter().flatten().any(|kind| *kind == ItemKind::Macro),
        EditKind::Members => kinds.iter().flatten().any(|kind| matches!(kind, ItemKind::Struct | ItemKind::Enum)),
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
        let found = claimed(node_kind(edit, outcome.report.node_mir.get(index), fact, trees), entry, edit, trees);
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
    // pass of design 79), also "not mapped" with its request. A claim keeps its anchor: it is
    // the source of the entry.
    if entry.state == AstState::NotMapped && !found.syntax_only {
        entry.state = AstState::Semilla;
        entry.by = format!("the map of the diff: {}", found.kind.name());
        entry.rule = found.rule.into();
        entry.template = String::new();
        entry.before = String::new();
        entry.after = String::new();
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
        EntryKind::Rename => 5,
        EntryKind::Move => 6,
        EntryKind::SameCode => 7,
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
    let mut held: Vec<Vec<(usize, NodeKind, &AstEntry)>> = vec![Vec::new(); count];
    for (place, (entry, index)) in map.entries.iter().zip(&indices).enumerate() {
        let Some(fact) = outcome.fact_of.get(*index).copied().flatten().filter(|fact| *fact < count) else { continue };
        let edit = &outcome.edits[*index];
        let found = claimed(node_kind(edit, outcome.report.node_mir.get(index), outcome.report.facts.get(fact), trees), entry, edit, trees);
        let found = match (entry.state, found.syntax_only) {
            // [R90.2-6] The matcher of today leaves a node with no MIR element with no partner.
            (AstState::NotMapped, true) => NodeKind::new(EntryKind::NotMapped, true, RULE_SYNTAX, format!("no MIR element; the matcher of today maps no partner ({})", entry.rule)),
            (AstState::NotMapped, false) => NodeKind::new(EntryKind::NotMapped, false, RULE_OPEN, "the alignment gives no unique answer".into()),
            _ => found,
        };
        held[fact].push((place + 1, found, entry));
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
    for (fact_index, fact) in outcome.report.facts.iter_mut().enumerate() {
        let nodes = &held[fact_index];
        fact.nodes = nodes.iter().map(|(number, ..)| *number).collect();
        let (kind, syntax_only, check) = match nodes.iter().min_by_key(|(_, found, _)| priority(found.kind)) {
            Some((_, found, _)) => {
                let more = if nodes.len() > 1 { format!("; {} nodes", nodes.len()) } else { String::new() };
                (found.kind, nodes.iter().all(|(_, found, _)| found.syntax_only), format!("{}, {}{more}", found.rule, found.check))
            }
            None => no_node_kind(fact, files[fact_index]),
        };
        fact.kind = Some(kind);
        fact.syntax_only = syntax_only;
        fact.check = check;
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
        // A new layout: the tokens are equal.
        (None, Category::Layout) => EntryKind::SameCode,
        (None, _) => EntryKind::Replace,
    };
    let check = match (file, kind) {
        (Some(_), _) => "R90.3-2, a file edit (no tree)".to_string(),
        // [R90.2-5] A pair of items: the fact exists only when the pair passes its check.
        (None, EntryKind::Rename | EntryKind::Move) => format!("{RULE_PAIR}, the pair check holds"),
        (None, EntryKind::SameCode) => "R90.3-2, equal tokens (a new layout)".to_string(),
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
