//! The transformations of the map of the diff (design 94): extract, inline, and reorder.
//!
//! The MIR checks are in `semilla_normal::transform` (the callee put in at its call, and a new
//! order of the same parts). This module reads their results and the syntax tree, and gives
//! the nodes of each transformation their kind:
//!
//! - [R94.1-1] [R94.2-1] an extract or an inline of a function: the nodes at the call (the
//!   region) and the node of the callee are one entry "extract" ("inline"); the elements that
//!   the check does not explain are one real edit of their own (R94.1-2);
//! - [R94.1-3] a tie of two candidates for one region gives no pair, and a template of each;
//! - [R94.3-1] [R94.3-2] an extracted or an inlined variable;
//! - [R94.4-1] [R94.4-2] [R94.4-3] a new order of statements, arms, fields, variants, items;
//! - [R94.5-1] the cases where a call is not its inlined body (`#[track_caller]`);
//! - [R94.6-1] the statements `#[extract]` and `#[inline]` anchor a candidate.
//!
//! Each rule reads data that the step already has: no search, no solver. When the data is not
//! sure, the node is a real edit (never the reverse).

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{EntryKind, Fact};
use semilla_syntax::{Edit, EditKind, ItemKind, Workspace};

use crate::diffmap::NodeKind;
use crate::{BodyComparison, BodyStatus, MirPlace, MirTransform, StepInput, StepOutcome};

pub const RULE_EXTRACT: &str = "R94.1-1";
pub const RULE_LEFT: &str = "R94.1-2";
pub const RULE_TIE: &str = "R94.1-3";
pub const RULE_INLINE: &str = "R94.2-1";
pub const RULE_VARIABLE: &str = "R94.3-1";
pub const RULE_VARIABLE_ORDER: &str = "R94.3-2";
pub const RULE_SAME_ORDER: &str = "R94.4-1";
pub const RULE_INDEPENDENT: &str = "R94.4-2";
pub const RULE_ORDER: &str = "R94.4-3";
pub const RULE_REFUSED: &str = "R94.5-1";
pub const RULE_STATEMENT: &str = "R94.6-1";
pub const RULE_REGION_ORDER: &str = "R95.1-1";
pub const RULE_REGION_VARIABLE: &str = "R95.1-2";
pub const RULE_REGION_EXTRACT: &str = "R95.1-3";
pub const RULE_PURE_CALL: &str = "R95.2-1";
pub const RULE_GENERIC: &str = "R95.3-1";
pub const RULE_IMPL: &str = "R95.4-1";
pub const RULE_EARLY_RETURN: &str = "R95.5-1";
pub const RULE_WRITTEN_PARAMETER: &str = "R95.5-2";
pub const RULE_RESULT_PLACE: &str = "R95.5-3";
pub const RULE_CLOSURE: &str = "R95.6-1";

/// The mark of an order that can matter.
pub const ORDER_MATTERS: &str = "the order can matter";

/// One entry of the code that a transformation does not explain (R94.1-2).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Left {
    pub inline: bool,
    pub body: String,
    pub callee: String,
    /// The places of the elements that differ after the check: old (0) and new (1).
    pub places: [Vec<MirPlace>; 2],
    /// The edit of the node of the call.
    pub node: usize,
    /// [R98.6-1] The id of the entry of the code left (R94.1-2): the graph changes of its places
    /// go to it (design 98).
    #[serde(default)]
    pub fact: String,
}

/// One transformation of the step that the map applies (for the check of a statement).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Applied {
    pub inline: bool,
    pub body: String,
    pub callee: String,
    /// The edits of the nodes of the entry.
    pub edits: Vec<usize>,
    /// The count of the elements that the check explains, and of the elements left.
    pub explained: usize,
    pub left: usize,
}

/// The plan of the transformations of a step: the kind of each node that a transformation
/// gives, the nodes whose facts make one entry, and the entries of the code left.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    pub nodes: BTreeMap<usize, NodeKind>,
    pub joins: Vec<(usize, usize)>,
    pub left: Vec<Left>,
    pub applied: Vec<Applied>,
    /// [R94.6-1] The candidates of each statement of the agent that does not hold, with the
    /// cause: `(inline, body, callee)`.
    #[serde(with = "semilla_core::stored::pairs")]
    pub failed: BTreeMap<(bool, String, String), String>,
    /// [R94.1-3] The notes of the ties, and the templates of each candidate.
    pub notes: Vec<String>,
    pub templates: Vec<String>,
    /// [R97.2-2] [R97.7-1] The statements `#[rewrite]` of the closure form, the groups of the
    /// repeated edits, and the templates of the closure form (design 97).
    pub rewrites: crate::rewrites::Plan,
}

fn kind_of(inline: bool) -> EntryKind {
    if inline { EntryKind::Inline } else { EntryKind::Extract }
}

fn word(inline: bool) -> &'static str {
    if inline { "inline" } else { "extract" }
}

/// [R94.6-1] The statements of the agent: `--same 'extract BODY CALLEE'` and `--same 'inline
/// BODY CALLEE'` (the short forms of `#[extract]` and `#[inline]`): `(inline, body, callee)`.
pub fn statements(input: &StepInput) -> Vec<(bool, String, String)> {
    input
        .step
        .agent
        .same
        .iter()
        .filter_map(|text| {
            let mut words = text.split_whitespace();
            let inline = match words.next()? {
                "extract" => false,
                "inline" => true,
                _ => return None,
            };
            let (body, callee) = (words.next()?, words.next()?);
            words.next().is_none().then(|| (inline, body.to_string(), callee.to_string()))
        })
        .collect()
}

/// True if the item of a body has the attribute `#[track_caller]` (R94.5-1).
fn track_caller(tree: &Workspace, path: &str) -> bool {
    tree.items.get(path).is_some_and(|item| item.attrs.iter().any(|attr| attr.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').any(|part| part == "track_caller")))
}

/// The source text of a place, on one line.
pub fn source_text(tree: &Workspace, place: &MirPlace) -> Option<String> {
    let text = tree.rust_files.get(&place.0)?;
    let part = text.get(place.1..place.2)?;
    Some(semilla_core::text::flat_text(part))
}

/// The line of a byte offset in a file of a tree (1 for the first line).
pub fn line_of(tree: &Workspace, file: &str, offset: usize) -> Option<usize> {
    let text = tree.rust_files.get(file)?;
    Some(text.get(..offset.min(text.len()))?.matches('\n').count() + 1)
}

/// The places of a list of elements.
fn places(list: &[crate::MirElement]) -> BTreeSet<MirPlace> {
    list.iter().filter_map(|element| element.tokens.clone()).collect()
}

/// The body comparison of a name.
fn body<'b>(bodies: &'b [BodyComparison], name: &str) -> Option<&'b BodyComparison> {
    bodies.iter().find(|body| body.name == name)
}

/// True if a place is inside the range of a body.
fn inside(place: &MirPlace, body: Option<&BodyComparison>) -> bool {
    match body.and_then(|body| Some((body.span.as_ref()?.0.clone(), body.range?))) {
        Some((file, (start, end))) => place.0 == file && place.1 >= start && place.2 <= end,
        None => false,
    }
}

/// [R94.1-1] The plan of the transformations of a step.
pub(crate) fn plan(input: &StepInput, outcome: &StepOutcome) -> Plan {
    let trees = [input.before, input.after];
    let mut out = Plan::default();
    let anchors = statements(input);
    let nodes: Vec<usize> = (0..outcome.edits.len()).filter(|index| crate::astmap::is_node(&outcome.edits[*index])).collect();
    for body in &outcome.bodies {
        let (BodyStatus::Different { .. }, Some(diff)) = (&body.status, &body.elements) else { continue };
        if diff.transforms.is_empty() {
            continue;
        }
        // [R94.5-1] The rules that only the syntax tree knows.
        let mut candidates: Vec<MirTransform> = diff.transforms.clone();
        for found in candidates.iter_mut().filter(|found| found.refused.is_none()) {
            let side = if found.inline { 0 } else { 1 };
            let cause = if track_caller(trees[side], &found.callee) {
                Some(format!("the callee `{}` has `#[track_caller]`: the caller location is a value that the call gives and the code in place does not", found.callee))
            } else if track_caller(trees[side], &body.name) || track_caller(trees[1 - side], &body.name) {
                Some(format!("the body `{}` has `#[track_caller]`: the caller location of the code in the callee is not the caller location of the code in place", body.name))
            } else {
                None
            };
            if let Some(cause) = cause {
                found.refused = Some(cause);
                found.holds = false;
            }
        }
        let anchored: Vec<&MirTransform> = candidates.iter().filter(|found| anchors.contains(&(found.inline, body.name.clone(), found.callee.clone()))).collect();
        // [R94.6-1] A statement anchors its candidate: Semilla checks the pair that it names and
        // looks for no other candidate of the body.
        for anchor in anchors.iter().filter(|(_, name, _)| *name == body.name) {
            let named: Vec<&&MirTransform> = anchored.iter().filter(|found| (found.inline, &found.callee) == (anchor.0, &anchor.2)).collect();
            if !named.iter().any(|found| found.holds) {
                let cause = match named.first() {
                    Some(found) => match &found.refused {
                        Some(cause) => format!("the callee cannot go in at the call: {cause} ({RULE_REFUSED})"),
                        None => format!("the MIR of `{}` put in at the call explains {} element(s); {} of {} element place(s) of the callee are equal, and the check needs at least one explained element, half of the callee, and a partner of each element of the callee that differs{}", found.callee, found.explained, found.callee_equal, found.callee_places, differ_text(trees, found)),
                    },
                    None => format!("no call of `{}` in the {} body of `{}` is new code of the step", anchor.2, if anchor.0 { "old" } else { "new" }, body.name),
                };
                out.failed.insert(anchor.clone(), cause);
            }
        }
        let mut chosen: Vec<&MirTransform> = if anchored.is_empty() { candidates.iter().filter(|found| found.holds && !found.tie).collect() } else { anchored.into_iter().filter(|found| found.holds).collect() };
        // Two calls of the callee of a statement that tie for one region: the statement does not
        // decide which call it is.
        let twice: Vec<(bool, String)> = chosen.iter().filter(|found| found.tie && chosen.iter().filter(|other| other.tie && (other.inline, &other.callee) == (found.inline, &found.callee)).count() > 1).map(|found| (found.inline, found.callee.clone())).collect();
        for (inline, callee) in &twice {
            out.failed.insert((*inline, body.name.clone(), callee.clone()), format!("two calls of `{callee}` fit one region ({RULE_TIE}): the statement does not decide which call it names"));
        }
        chosen.retain(|found| !twice.contains(&(found.inline, found.callee.clone())));
        // [R94.1-3] A tie of the candidates of one region: no pair, and a template of each.
        let tied: Vec<&MirTransform> = candidates.iter().filter(|found| found.holds && found.tie && !anchors.iter().any(|(_, name, _)| *name == body.name)).collect();
        if tied.len() > 1 {
            let pairs: Vec<String> = tied.iter().map(|found| format!("`{}` → `{}`", body.name, found.callee)).collect();
            out.notes.push(format!("{} show(s) as real edits: a tie of {} candidates of an {} for one region of `{}` ({RULE_TIE}); no statement decides it, and Semilla does not use the order", pairs.join(", "), tied.len(), word(tied[0].inline), body.name));
            for found in &tied {
                out.templates.push(template(found.inline, &body.name, &found.callee));
            }
        }
        for found in chosen {
            apply(trees, outcome, &nodes, body, diff, found, &mut out);
        }
        // [R94.5-1] A candidate of an added or a removed callee that the check cannot put in at
        // its call: the note gives the cause.
        for found in candidates.iter().filter(|found| found.refused.is_some()) {
            let side = if found.inline { 0 } else { 1 };
            let one_side = !trees[1 - side].items.contains_key(&found.callee) && trees[side].items.contains_key(&found.callee);
            if one_side && !anchors.iter().any(|(_, name, _)| *name == body.name) {
                let note = format!("`{}` → `{}` is no {}: the callee cannot go in at the call: {} ({RULE_REFUSED})", body.name, found.callee, word(found.inline), found.refused.clone().unwrap_or_default());
                if !out.notes.contains(&note) {
                    out.notes.push(note);
                }
            }
        }
    }
    for (anchor, _) in anchors.iter().map(|anchor| (anchor, ())) {
        let known = out.applied.iter().any(|applied| (applied.inline, &applied.body, &applied.callee) == (anchor.0, &anchor.1, &anchor.2));
        if !known && !out.failed.contains_key(anchor) {
            out.failed.insert(anchor.clone(), format!("the step has no changed body `{}` with a call of `{}` that the check can read", anchor.1, anchor.2));
        }
    }
    variables_and_orders(input, outcome, &nodes, &mut out);
    one_variable(outcome, &mut out);
    out
}

/// [R94.6-1] The text of the elements that differ after the check of a candidate.
fn differ_text(trees: [&Workspace; 2], found: &MirTransform) -> String {
    let mut parts = Vec::new();
    for side in 0..2 {
        for element in found.left[side].iter().take(3) {
            if let Some(text) = element.tokens.as_ref().and_then(|place| source_text(trees[side], place)) {
                parts.push(format!("{} `{text}`", if side == 0 { "old" } else { "new" }));
            }
        }
    }
    if parts.is_empty() { String::new() } else { format!("; the elements that differ: {}", parts.join(", ")) }
}

/// [R94.6-1] [R94.1-3] The template of the statement of a candidate.
pub fn template(inline: bool, body: &str, callee: &str) -> String {
    let short = |path: &str| path.rsplit("::").next().unwrap_or(path).to_string();
    if inline {
        format!("#[inlined(after::{body}, from = before::{callee})] fn {}_from_{}() {{}}", short(body), short(callee))
    } else {
        format!("#[extract(before::{body}, into = after::{callee})] fn {}_into_{}() {{}}", short(body), short(callee))
    }
}

/// [R94.1-1] [R94.1-2] [R94.2-1] Gives the nodes of one transformation that holds their kind.
fn apply(trees: [&Workspace; 2], outcome: &StepOutcome, nodes: &[usize], body: &BodyComparison, diff: &crate::MirDiff, found: &MirTransform, out: &mut Plan) {
    let side = if found.inline { 0 } else { 1 };
    let other = 1 - side;
    let Some(call) = &found.call else { return };
    let in_body = |edit: &usize| outcome.report.node_mir.get(edit).is_some_and(|mir| mir.bodies.contains(&body.name));
    // The region: the nodes that own the call.
    let region: Vec<usize> = nodes.iter().copied().filter(|edit| in_body(edit) && outcome.report.node_mir.get(edit).is_some_and(|mir| mir.places[side].contains(call))).collect();
    if region.is_empty() {
        return;
    }
    // The node of the callee: an added (an extract) or a removed (an inline) item.
    let wanted = if found.inline { EditKind::ItemRemoved } else { EditKind::ItemAdded };
    let callee_node = nodes.iter().copied().find(|edit| {
        let edit = &outcome.edits[*edit];
        let anchor = if found.inline { &edit.before } else { &edit.after };
        edit.kind == wanted && anchor.as_ref().is_some_and(|anchor| anchor.item == found.callee)
    });
    let callee_body = body_of(outcome, &found.callee);
    // The places that the check explains, on each side.
    let before = [places(&diff.old), places(&diff.new)];
    let after = [places(&found.left[0]), places(&found.left[1])];
    let mut explained: [BTreeSet<MirPlace>; 2] = [before[0].difference(&after[0]).cloned().collect(), before[1].difference(&after[1]).cloned().collect()];
    explained[side].insert(call.clone());
    // [R94.1-2] The code that the check does not explain: the elements of the callee that
    // differ, and the elements of the nodes of the call that differ.
    let owned = |edit: &usize, at: usize| -> BTreeSet<MirPlace> { outcome.report.node_mir.get(edit).map(|mir| mir.places[at].clone()).unwrap_or_default() };
    // Each element that differs after the check is code left of the transformation (an element
    // of the callee, or an element that no other node of the body owns), or the edit of an
    // other node of the body that owns it (that node keeps its kind and its entry). So no
    // element that differs is lost, and none is in two entries.
    let others: [BTreeSet<MirPlace>; 2] = [0, 1].map(|at| nodes.iter().filter(|edit| in_body(edit) && !region.contains(edit) && Some(**edit) != callee_node).flat_map(|edit| owned(edit, at)).collect());
    let mut left: [Vec<MirPlace>; 2] = [Vec::new(), Vec::new()];
    for at in [side, other] {
        for place in &after[at] {
            let of_callee = at == side && (found.callee_at.contains(place) || inside(place, callee_body));
            if (of_callee || !others[at].contains(place)) && !left[at].contains(place) {
                left[at].push(place.clone());
            }
        }
    }
    let counted = |list: &[MirPlace]| list.len();
    // [R95.3-1] [R95.4-1] [R95.5-1] [R95.5-2] [R95.6-1] The most specific rule of design 95 that
    // the check used is the rule of the entry; each one adds its text to the check.
    let rule = found.via.first().and_then(|(rule, _)| [RULE_GENERIC, RULE_IMPL, RULE_EARLY_RETURN, RULE_WRITTEN_PARAMETER, RULE_RESULT_PLACE, RULE_CLOSURE].into_iter().find(|known| known == rule)).unwrap_or(if found.inline { RULE_INLINE } else { RULE_EXTRACT });
    let via: String = found.via.iter().map(|(rule, text)| format!("; {text} ({rule})")).collect();
    let public = callee_node.is_some() && trees[side].items.get(&found.callee).is_some_and(|item| item.vis.starts_with("pub"));
    let api = if public { "; the callee is public: a different crate can call it" } else { "" };
    let check = if left[0].is_empty() && left[1].is_empty() {
        format!("the MIR of `{}` put in at the call is the MIR of the {} code ({} element place(s) explained){via}{api}", found.callee, if found.inline { "new" } else { "old" }, found.explained.max(1))
    } else {
        format!("the MIR of `{}` put in at the call explains {} element place(s); {} element place(s) differ: a real edit of its own ({RULE_LEFT}){via}{api}", found.callee, found.explained, counted(&left[0]) + counted(&left[1]))
    };
    let node = |kind: EntryKind, check: String| NodeKind { kind, syntax_only: false, rule, check, order: None };
    let mut edits = Vec::new();
    for edit in &region {
        out.nodes.insert(*edit, node(kind_of(found.inline), check.clone()));
        edits.push(*edit);
    }
    // The other nodes of the body whose elements the check explains are a part of the region.
    let open: Vec<usize> = nodes.iter().copied().filter(|edit| in_body(edit) && !region.contains(edit) && !out.nodes.contains_key(edit)).collect();
    for edit in open {
        let sides = [owned(&edit, 0), owned(&edit, 1)];
        if sides.iter().all(BTreeSet::is_empty) {
            continue;
        }
        let covered = (0..2).all(|at| sides[at].iter().all(|place| explained[at].contains(place)));
        if covered {
            out.nodes.insert(edit, node(kind_of(found.inline), format!("a part of the {} of `{}` ({rule})", word(found.inline), found.callee)));
            out.joins.push((edit, region[0]));
            edits.push(edit);
        }
    }
    // [R95.6-1] The nodes of a closure body (a closure has no item): each node whose elements are
    // elements of the closure body, with no element that the check does not explain.
    if callee_node.is_none() && found.callee.contains("{closure#") {
        // A node of the item with no element (its body, the closure, is on one side only) is a
        // node of the closure when the item has one such closure.
        let prefix = format!("{}::{{closure#", body.name);
        let lone = outcome.bodies.iter().filter(|other| other.name.starts_with(&prefix) && matches!(other.status, BodyStatus::BeforeOnly | BodyStatus::AfterOnly)).count() == 1;
        let item_of = |edit: &usize| outcome.edits[*edit].after.as_ref().or(outcome.edits[*edit].before.as_ref()).map(|anchor| anchor.item.clone());
        let closure_nodes: Vec<usize> = nodes.iter().copied().filter(|edit| !out.nodes.contains_key(edit) && !region.contains(edit)).filter(|edit| {
            outcome.report.node_mir.get(edit).is_some_and(|mir| {
                let elements = mir.bodies.contains(&found.callee) && mir.bodies.iter().all(|name| *name == found.callee || *name == body.name) && (0..2).all(|at| mir.places[at].iter().all(|place| explained[at].contains(place) || (at == side && found.callee_at.contains(place)))) && !left.iter().flatten().any(|place| mir.places.iter().any(|list| list.contains(place)));
                let empty = lone && mir.one_side && mir.places.iter().all(BTreeSet::is_empty) && item_of(edit).as_deref() == Some(body.name.as_str());
                elements || empty
            })
        }).collect();
        for edit in closure_nodes {
            out.nodes.insert(edit, node(kind_of(found.inline), check.clone()));
            out.joins.push((edit, region[0]));
            edits.push(edit);
        }
    }
    if let Some(edit) = callee_node {
        let what = if found.inline { "the removed callee" } else { "the new callee" };
        out.nodes.insert(edit, node(kind_of(found.inline), format!("{what} of the {} in `{}`{api}", word(found.inline), body.name)));
        out.joins.push((edit, region[0]));
        edits.push(edit);
    }
    for edit in region.iter().skip(1) {
        out.joins.push((*edit, region[0]));
    }
    if !left[0].is_empty() || !left[1].is_empty() {
        out.left.push(Left { inline: found.inline, body: body.name.clone(), callee: found.callee.clone(), places: left.clone(), node: region[0], fact: String::new() });
    }
    out.applied.push(Applied { inline: found.inline, body: body.name.clone(), callee: found.callee.clone(), edits, explained: found.explained, left: left[0].len() + left[1].len() });
}

/// The comparison of a body by its name.
fn body_of<'o>(outcome: &'o StepOutcome, name: &str) -> Option<&'o BodyComparison> {
    body(&outcome.bodies, name)
}

/// The block path of a statement anchor: the path without its last segment.
fn block_of(anchor: &semilla_core::Anchor) -> Option<(String, Vec<String>)> {
    let path: Vec<String> = anchor.path.iter().map(|segment| segment.to_string()).collect();
    let (_, parent) = path.split_last()?;
    Some((anchor.item.clone(), parent.to_vec()))
}

/// The text of the places of a dependence of a reorder, for its cause.
fn dependence_text(tree: &Workspace, dependent: &[(Option<MirPlace>, Option<MirPlace>, String)]) -> String {
    // The pair with the longest source texts names the parts best.
    let length = |place: &Option<MirPlace>| place.as_ref().map_or(0, |place| place.2.saturating_sub(place.1));
    match dependent.iter().max_by_key(|(a, b, _)| length(a).min(length(b))) {
        Some((a, b, cause)) => {
            let text = |place: &Option<MirPlace>| place.as_ref().and_then(|place| source_text(tree, place)).map(|text| format!("`{text}`"));
            match (text(a), text(b)) {
                (Some(a), Some(b)) if a != b => format!("{cause}: {a} and {b}"),
                (Some(a), _) | (_, Some(a)) => format!("{cause}: {a}"),
                _ => cause.clone(),
            }
        }
        None => String::new(),
    }
}

/// [R94.4-3] True if two arm patterns are disjoint by their text (Decided by Claude, a
/// conservative rule): two different literals, or two paths of variants with different last
/// segments, for each pair of their alternatives (`|`). A binding, a wildcard, a range, and
/// each other form can match each value.
pub fn disjoint(a: &str, b: &str) -> bool {
    fn alternatives(text: &str) -> Vec<String> {
        let mut parts = Vec::new();
        let (mut depth, mut start) = (0i32, 0);
        for (at, c) in text.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                '|' if depth == 0 => {
                    parts.push(text[start..at].trim().to_string());
                    start = at + 1;
                }
                _ => {}
            }
        }
        parts.push(text[start..].trim().to_string());
        parts.into_iter().filter(|part| !part.is_empty()).collect()
    }
    enum Form {
        Literal(String),
        Variant(String, String),
        Other,
    }
    fn form(text: &str) -> Form {
        let text = text.trim();
        let literal = text.strip_prefix('-').unwrap_or(text);
        let number = literal.chars().next().is_some_and(|c| c.is_ascii_digit()) && literal.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        if number || (text.starts_with('"') && text.ends_with('"') && text.len() >= 2) || (text.starts_with('\'') && text.ends_with('\'') && text.len() >= 3) || text == "true" || text == "false" {
            return Form::Literal(text.to_string());
        }
        let head = text.split(['(', '{']).next().unwrap_or(text).trim();
        let path_like = head.contains("::") && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
        if path_like && !text.contains("..=") && !text.contains(" @ ") {
            let (prefix, last) = head.rsplit_once("::").unwrap_or(("", head));
            return Form::Variant(prefix.to_string(), last.to_string());
        }
        Form::Other
    }
    let (left, right) = (alternatives(a), alternatives(b));
    !left.is_empty()
        && !right.is_empty()
        && left.iter().all(|x| {
            right.iter().all(|y| match (form(x), form(y)) {
                (Form::Literal(p), Form::Literal(q)) => p != q,
                (Form::Variant(p, x), Form::Variant(q, y)) => p == q && x != y,
                _ => false,
            })
        })
}

/// The index of an arm anchor (`…/arm[2].pat` gives 2).
fn arm_index(anchor: &semilla_core::Anchor) -> Option<usize> {
    anchor.path.last()?.to_string().strip_prefix("arm[")?.split(']').next()?.parse().ok()
}

/// The pattern of the arm of an arm anchor (`…/arm[2].pat`) in a tree, and the patterns of the
/// arms that it passes when it goes to the index `to` (the arms between the two places).
fn arm_patterns(tree: &Workspace, anchor: &semilla_core::Anchor, to: usize) -> Option<(String, Vec<String>)> {
    let item = tree.items.get(&anchor.item)?;
    let (last, parent) = anchor.path.split_last()?;
    let from = arm_index(anchor)?;
    let mut node = item.body.as_ref()?;
    for segment in parent {
        let role = segment.to_string();
        node = node.children.iter().find(|child| child.role == role)?;
    }
    let own = node.children.iter().find(|child| child.role == last.to_string())?.text.clone();
    let (low, high) = (from.min(to), from.max(to));
    let passed: Vec<String> = (low..=high).filter(|index| *index != from).filter_map(|index| node.children.iter().find(|child| child.role == format!("arm[{index}].pat")).map(|child| child.text.clone())).collect();
    Some((own, passed))
}

/// [R94.3-1] [R94.3-2] [R94.4-1] [R94.4-2] [R94.4-3] The kinds of the extracted and the inlined
/// variables, of the statements and the arms with a new place in their block, and of the
/// fields and the variants in another order.
fn variables_and_orders(input: &StepInput, outcome: &StepOutcome, nodes: &[usize], out: &mut Plan) {
    let trees = [input.before, input.after];
    let open: Vec<usize> = nodes.iter().copied().filter(|index| !out.nodes.contains_key(index)).collect();
    for index in open {
        let edit = &outcome.edits[index];
        let Some(anchor) = edit.after.as_ref().or(edit.before.as_ref()) else { continue };
        let compared = body_of(outcome, &anchor.item);
        let status = compared.map(|body| &body.status);
        // [R95.1-1] [R95.1-2] The new order of the region of the node: the node owns no element of
        // a real edit of the body (the residue), and each other edit keeps its own kind.
        let mir = outcome.report.node_mir.get(&index);
        let reorder = compared.and_then(|body| body.elements.as_ref()).and_then(|diff| diff.reorder.as_ref()).filter(|reorder| mir.is_none_or(|mir| (0..2).all(|side| mir.places[side].iter().all(|place| !reorder.residue[side].contains(place)))));
        let partial = reorder.is_some_and(|reorder| reorder.residue.iter().any(|list| !list.is_empty()));
        let pure = reorder.map_or(String::new(), |reorder| if reorder.pure_calls > 0 { format!("; the effect summary of {} call(s) has no effect ({RULE_PURE_CALL})", reorder.pure_calls) } else { String::new() });
        let region = if partial { "; the check reads the region: the other edits of the body are real edits" } else { "" };
        let node = |kind: EntryKind, rule: &'static str, check: String, order: Option<String>| NodeKind { kind, syntax_only: false, rule, check, order };
        match &edit.refactor {
            Some(semilla_syntax::Refactor::Extract { .. }) | Some(semilla_syntax::Refactor::Inline { .. }) => {
                let inline = matches!(edit.refactor, Some(semilla_syntax::Refactor::Inline { .. }));
                let kind = kind_of(inline);
                // A node of the variable that owns no element that differs, in a body whose
                // elements that differ each have an other owner (design 41.7: the edit is idle).
                let idle = outcome.report.node_mir.get(&index).is_some_and(|mir| mir.equal && !mir.doubt && mir.places.iter().all(BTreeSet::is_empty));
                let rule = if partial { RULE_REGION_VARIABLE } else if reorder.is_some_and(|reorder| reorder.pure_calls > 0 && reorder.flips > 0) { RULE_PURE_CALL } else { RULE_VARIABLE };
                let found = match (status, reorder) {
                    (Some(BodyStatus::Equal { .. }), _) => node(kind, RULE_VARIABLE, "equal MIR after the normal form (the copy and move chains of a local)".into(), None),
                    (Some(BodyStatus::Different { .. }), Some(reorder)) if reorder.flips == 0 => node(kind, rule, format!("equal MIR after the normal form, under one map of the locals and the blocks{region}"), None),
                    (Some(BodyStatus::Different { .. }), Some(reorder)) if reorder.dependent.is_empty() => node(kind, rule, format!("the MIR differs only in the order of {} pair(s) of independent parts{pure}{region}", reorder.flips), None),
                    (Some(BodyStatus::Different { .. }), Some(reorder)) => {
                        let cause = format!("the place where the value is evaluated moves: {}", dependence_text(trees[0], &reorder.dependent));
                        node(kind, RULE_VARIABLE_ORDER, cause.clone(), Some(cause))
                    }
                    (Some(BodyStatus::Different { .. }), None) if idle => node(kind, RULE_VARIABLE, "equal MIR at its place: each element that differs is an element of an other edit".into(), None),
                    // [R94.3-2] The MIR changes more than the order (an evaluation more or less):
                    // a real edit.
                    _ => node(EntryKind::Replace, RULE_VARIABLE_ORDER, format!("an {} variable: the MIR is not equal after the normal form, and it is not a new order of the same parts", if inline { "inlined" } else { "extracted" }), None),
                };
                out.nodes.insert(index, found);
            }
            Some(semilla_syntax::Refactor::Moved { .. }) | Some(semilla_syntax::Refactor::Arm { .. }) => {
                let (Some(old), Some(new)) = (edit.before.as_ref(), edit.after.as_ref()) else { continue };
                // A statement that moves to a different block keeps the kind "move".
                if block_of(old) != block_of(new) {
                    continue;
                }
                let arm = matches!(edit.refactor, Some(semilla_syntax::Refactor::Arm { .. }));
                // [R94.4-3] Two arms whose patterns can both match one value.
                let overlap = if arm {
                    match arm_index(new).and_then(|to| arm_patterns(trees[0], old, to)) {
                        Some((own, passed)) => passed.iter().find(|other| !disjoint(&own, other)).map(|other| format!("the patterns `{own}` and `{other}` can both match one value")),
                        None => Some("the patterns of the arms are not known".to_string()),
                    }
                } else {
                    None
                };
                let what = if arm { "arms" } else { "statements" };
                let pure_rule = if reorder.is_some_and(|reorder| reorder.pure_calls > 0) { RULE_PURE_CALL } else { RULE_INDEPENDENT };
                let (same_rule, independent_rule) = if partial { (RULE_REGION_ORDER, RULE_REGION_ORDER) } else { (RULE_SAME_ORDER, pure_rule) };
                let found = match (status, reorder, overlap) {
                    (_, _, Some(cause)) => node(EntryKind::Reorder, RULE_ORDER, cause.clone(), Some(cause)),
                    (Some(BodyStatus::Equal { .. }), _, None) => node(EntryKind::Reorder, RULE_SAME_ORDER, format!("the {what} in another order; the compiled code is the same"), None),
                    (Some(BodyStatus::Different { .. }), Some(reorder), None) if reorder.flips == 0 => node(EntryKind::Reorder, same_rule, format!("the {what} in another order; the MIR is equal under one map of the locals and the blocks: the compiled code is the same{region}"), None),
                    (Some(BodyStatus::Different { .. }), Some(reorder), None) if reorder.dependent.is_empty() => node(EntryKind::Reorder, independent_rule, format!("the {what} in another order; the parts are independent ({} pair(s) of MIR elements change order{pure}){region}", reorder.flips), None),
                    (Some(BodyStatus::Different { .. }), Some(reorder), None) => {
                        let cause = dependence_text(trees[0], &reorder.dependent);
                        node(EntryKind::Reorder, RULE_ORDER, cause.clone(), Some(cause))
                    }
                    // The MIR changes more than the order: the node keeps its kind.
                    _ => continue,
                };
                out.nodes.insert(index, found);
            }
            None if edit.kind == EditKind::Members => {
                if let Some(found) = members(input, outcome, edit) {
                    out.nodes.insert(index, found);
                }
            }
            _ => {}
        }
    }
}

/// [R94.3-2] The nodes of one extracted or inlined variable (its `let` and its uses, in one
/// item) have one result: a node that is a real edit makes each node of the variable one.
fn one_variable(outcome: &StepOutcome, out: &mut Plan) {
    let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (index, found) in &out.nodes {
        let Some(edit) = outcome.edits.get(*index) else { continue };
        let var = match &edit.refactor {
            Some(semilla_syntax::Refactor::Extract { var, .. }) | Some(semilla_syntax::Refactor::Inline { var, .. }) => var.clone(),
            _ => continue,
        };
        let item = edit.after.as_ref().or(edit.before.as_ref()).map(|anchor| anchor.item.clone()).unwrap_or_default();
        let _ = found;
        groups.entry((item, var)).or_default().push(*index);
    }
    for indexes in groups.values() {
        let strong = indexes.iter().filter_map(|index| out.nodes.get(index)).find(|found| found.kind.is_real() || found.order.is_some()).cloned();
        if let Some(strong) = strong {
            for index in indexes {
                out.nodes.insert(*index, strong.clone());
            }
        }
    }
}

/// [R94.4-1] [R94.4-3] A new order of the fields of a struct with names, or of the variants of an
/// enum: the same members in another order. `None`: not a reorder (a tuple struct keeps the
/// type change: its fields have no names).
fn members(input: &StepInput, outcome: &StepOutcome, edit: &Edit) -> Option<NodeKind> {
    let (old, new) = (edit.before.as_ref()?, edit.after.as_ref()?);
    let (a, b) = (input.before.items.get(&old.item)?, input.after.items.get(&new.item)?);
    if !matches!(a.kind, ItemKind::Struct | ItemKind::Enum) || a.kind != b.kind {
        return None;
    }
    let (mut x, mut y) = (a.members.clone(), b.members.clone());
    if x == y {
        return None;
    }
    x.sort();
    y.sort();
    // A field with no name (a tuple struct) has a number as its member.
    let unnamed = a.members.iter().any(|member| member.rsplit(['.', ':']).next().is_some_and(|last| last.chars().all(|c| c.is_ascii_digit())));
    if x != y || unnamed {
        return None;
    }
    let what = if a.kind == ItemKind::Struct { "fields" } else { "variants" };
    // [R94.4-3] A `repr` fixes the layout; a derive or an attribute macro makes code that reads
    // the members in order (R65.3-3, `facts::order_reader`).
    let attributes = |item: &semilla_syntax::Item| item.attrs.iter().map(|attr| if attr.trim_start().starts_with('#') { attr.clone() } else { format!("#[{attr}]") }).collect::<Vec<_>>().join(" ");
    let texts = [attributes(a), attributes(b)];
    let repr = texts.iter().any(|text| text.split_whitespace().collect::<String>().contains("#[repr("));
    let reader = crate::facts::order_reader(&texts[1]).or_else(|| crate::facts::order_reader(&texts[0]));
    let changed: Vec<String> = changed_by(outcome, &new.item).iter().map(|body| format!("`{}`", body.name)).collect();
    let listed = |list: &[String]| if list.len() > 3 { format!("{} and {} more", list[..3].join(", "), list.len() - 3) } else { list.join(", ") };
    // The variants of a public enum with no explicit discriminant: the values change, and a
    // different crate can read them (`as`).
    let public_values = a.kind == ItemKind::Enum && b.vis.trim() == "pub" && !b.text.contains('=');
    let cause = if repr {
        Some(format!("a `#[repr]` of the type fixes the layout: the order of the {what} is a part of it"))
    } else if !changed.is_empty() {
        Some(format!("the MIR of {} body(ies) reads the order of the {what}: {}", changed.len(), listed(&changed)))
    } else if let Some(reader) = reader {
        Some(reader)
    } else if public_values {
        Some("the discriminant values of the variants change (no explicit value), and a different crate can read them (`as`)".to_string())
    } else {
        None
    };
    if let Some(cause) = cause {
        return Some(NodeKind { kind: EntryKind::Reorder, syntax_only: false, rule: RULE_ORDER, check: cause.clone(), order: Some(cause) });
    }
    let check = format!("the {what} in another order; the MIR of each body is equal under the map of the {what} by name; the compiled code is the same");
    Some(NodeKind { kind: EntryKind::Reorder, syntax_only: false, rule: RULE_SAME_ORDER, check, order: None })
}

/// [R94.4-3] The changed bodies of a step with no edit at their place that a new order of the
/// members of `ty` changes: a trait impl of the type (a derived `Debug`, `PartialOrd`, `Ord`,
/// `Hash`), or a body whose changed elements name the type.
pub fn changed_by<'o>(outcome: &'o StepOutcome, ty: &str) -> Vec<&'o BodyComparison> {
    let impl_of = format!("<{ty} as ");
    outcome
        .bodies
        .iter()
        .filter(|body| matches!(body.status, BodyStatus::Different { .. }))
        .filter(|body| body.name.starts_with(&impl_of) || body.elements.as_ref().is_some_and(|diff| diff.old.iter().chain(&diff.new).any(|element| element.symbols.iter().any(|symbol| symbol.path == ty || symbol.path.starts_with(&format!("{ty}::"))))))
        .collect()
}

/// [R94.4-3] The types of the step whose members get a new order (a reorder entry).
pub fn reordered_types(outcome: &StepOutcome) -> Vec<(String, String)> {
    outcome
        .transforms
        .nodes
        .iter()
        .filter(|(_, found)| found.kind == EntryKind::Reorder)
        .filter_map(|(index, _)| {
            let edit = outcome.edits.get(*index)?;
            (edit.kind == EditKind::Members).then(|| edit.after.as_ref().map(|anchor| (anchor.item.clone(), outcome.fact_of.get(*index).copied().flatten().and_then(|fact| outcome.report.facts.get(fact)).map(|fact| fact.id.clone()).unwrap_or_default())))?
        })
        .collect()
}

/// [R94.1-2] The fact of the code that a transformation does not explain: a real edit with the
/// lines of its elements.
pub(crate) fn left_fact(trees: [&Workspace; 2], left: &Left, id: usize) -> Fact {
    let mut positions = Vec::new();
    let mut texts: [Vec<String>; 2] = [Vec::new(), Vec::new()];
    for side in 0..2 {
        for place in &left.places[side] {
            if let Some(line) = line_of(trees[side], &place.0, place.1) {
                positions.push(semilla_core::Position { file: place.0.clone(), old: side == 0, start: line, end: line_of(trees[side], &place.0, place.2).unwrap_or(line) });
            }
            if let Some(text) = source_text(trees[side], place)
                && !texts[side].contains(&text)
            {
                texts[side].push(text);
            }
        }
    }
    let quote = |list: &[String]| list.iter().map(|text| format!("`{text}`")).collect::<Vec<_>>().join(", ");
    let sides = match (texts[0].is_empty(), texts[1].is_empty()) {
        (false, false) => format!("{} → {}", quote(&texts[0]), quote(&texts[1])),
        (false, true) => format!("old {}", quote(&texts[0])),
        (true, false) => format!("new {}", quote(&texts[1])),
        (true, true) => "no source text".into(),
    };
    let kind = match (left.places[0].is_empty(), left.places[1].is_empty()) {
        (true, false) => EntryKind::Add,
        (false, true) => EntryKind::Delete,
        _ => EntryKind::Replace,
    };
    let count = left.places[0].len() + left.places[1].len();
    Fact {
        id: format!("f{id}"),
        at: Some(semilla_core::Anchor::item(&left.body)),
        file: positions.first().map(|position: &semilla_core::Position| position.file.clone()),
        positions: semilla_core::Position::normal(positions),
        part: "transformation".into(),
        description: format!("the code of the {} of `{}` that the MIR does not explain: {sides}", word(left.inline), left.callee),
        status: "derived".into(),
        category: semilla_core::Category::Semantics,
        compare: semilla_core::Compare::CodeDifferent,
        affected: vec![left.body.clone()],
        kind: Some(kind),
        check: format!("{RULE_LEFT}, {count} MIR element place(s) that differ after the callee is put in at the call"),
        ..Default::default()
    }
}
