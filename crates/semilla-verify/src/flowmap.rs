//! The control-flow map (L2, design 98): each graph change of the step goes to exactly one entry
//! of the map of the diff (design 90).
//!
//! A graph change is an added, a removed, or a changed point, or an added or a removed arrow
//! (design 60). The graph compare (`flow.rs`) gives each change its anchor: the MIR elements of
//! its point. After the map of the diff is complete (the kinds, the merges, the splits, the
//! entries of the code left by a transformation), this pass gives each change its entry:
//!
//! 1. [R98.2-1] the projection of design 90: the entry of the node that owns a MIR element of the
//!    point (`StepReport::node_mir`, the owner rule of design 40.3);
//! 2. [R98.3-1] else the entry of the smallest changed node that holds the place of the point,
//!    with the mark "implicit"; a point that the source has no node for (a check, the panic of a
//!    check, a point of a macro of another crate) has the mark too;
//! 3. else the entry of the body (the entry with the most changes of the body, else its entry of
//!    the section "compiled code that changes with no edit"), with the mark "implicit";
//! 4. [R98.4-1] a body with no edit: its entry of the section "compiled code that changes with no
//!    edit" (R92.4-1), else a line of its own in that section;
//! 5. [R98.6-1] a change whose entry is a transformation is explained by the check of the
//!    transformation (the entry lists none), unless its place is code that the check does not
//!    explain (R94.1-2): then it goes to that real edit. The equal MIR of a body (at a level of
//!    the normal form) explains each change of the body.
//!
//! The map gives no proof (R98.9-1): it changes no compare result, no proof result, and no kind.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::report::{EntryGraph, FlowCover, NoEntryGraph};
use semilla_core::{EntryKind, Fact};

use crate::flow::{BodyChanges, Element, Part, FACT_LINES};
use crate::MirPlace;

/// The name of the phase in the timing of a step.
pub const PHASE: &str = "the control-flow map (design 98)";

/// The rules of design 98.
pub const RULE_PAIR: &str = "R98.1-1";
pub const RULE_ENTRY: &str = "R98.2-1";
pub const RULE_IMPLICIT: &str = "R98.3-1";
pub const RULE_NO_ENTRY: &str = "R98.4-1";
pub const RULE_EXPLAINED: &str = "R98.6-1";

/// The entry of one graph change.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Placed {
    /// The index of the fact, or `None`: no entry (a line of its own, R98.4-1).
    fact: Option<usize>,
    implicit: bool,
    /// [R98.6-1] The words of the check that explains the change.
    explained: Option<String>,
}

/// The order of the sections for the choice between two entries: a real edit, then "not
/// mapped", "compiled code that changes with no edit", a transformation, "syntax only".
fn rank(fact: &Fact) -> u8 {
    match fact.section() {
        Some(0) => 0,
        Some(2) => 1,
        Some(1) => 2,
        Some(3) => 3,
        Some(4) => 4,
        _ => 5,
    }
}

fn number(fact: &Fact) -> usize {
    fact.id.strip_prefix('f').and_then(|number| number.parse().ok()).unwrap_or(usize::MAX)
}

/// The entry of a list of candidates: the first section, then the lowest number.
fn best(facts: &[Fact], found: &[usize]) -> Option<usize> {
    found.iter().copied().filter(|at| *at < facts.len()).min_by_key(|at| (rank(&facts[*at]), number(&facts[*at])))
}

/// [R98.6-1] The words of the check of a transformation entry.
fn check_words(fact: &Fact) -> String {
    match fact.kind {
        Some(EntryKind::Extract) => "the check of the extract".into(),
        Some(EntryKind::Inline) => "the check of the inline".into(),
        Some(EntryKind::Reorder) => "the check of the new order".into(),
        Some(EntryKind::Rename) => "the check of the rename".into(),
        Some(EntryKind::Move) => "the check of the move".into(),
        _ => "the equal MIR of the node (same code)".into(),
    }
}

/// True for an entry of the section "compiled code that changes with no edit" (R92.4-1).
fn no_edit(fact: &Fact) -> bool {
    fact.section() == Some(1)
}

/// The count of the graph changes of an element: one for each part (the point and each arrow).
fn units(element: &Element) -> usize {
    element.parts.len()
}

/// [R98.2-2] The short form of the graph changes of an entry: the counts by kind in the order
/// added, removed, changed, then the arrows; a single point has its text; a kind whose changes
/// are each implicit has the mark.
pub fn summary(parts: &[(Part, bool)]) -> String {
    // The kinds of each group in the order of their first change.
    let mut order: Vec<(u8, String)> = Vec::new();
    let mut groups: BTreeMap<(u8, String), (usize, usize, Vec<String>)> = BTreeMap::new();
    for (part, implicit) in parts {
        let key = (part.group, part.label.clone());
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        let entry = groups.entry(key).or_default();
        entry.0 += 1;
        entry.1 += usize::from(*implicit);
        if !part.text.is_empty() && !entry.2.contains(&part.text) {
            entry.2.push(part.text.clone());
        }
    }
    order.sort_by_key(|(group, _)| *group);
    let groups: Vec<((u8, String), (usize, usize, Vec<String>))> = order.into_iter().map(|key| {
        let value = groups.remove(&key).unwrap_or_default();
        (key, value)
    }).collect();
    let plural = |label: &str, count: usize| -> String {
        if count == 1 {
            return label.to_string();
        }
        match label {
            "branch" => "branches".into(),
            "entry" => "entries".into(),
            label if label.starts_with("changed ") => match label {
                "changed condition" | "changed call target" | "changed panic call" | "changed check" | "changed loop" => format!("{label}s"),
                other => format!("{other}s"),
            },
            other => format!("{other}s"),
        }
    };
    let mut out = Vec::new();
    for ((group, label), (count, implicit, texts)) in groups {
        let sign = match group {
            0 | 3 => "+",
            1 | 4 => "−",
            _ => "",
        };
        let mut marks: Vec<String> = Vec::new();
        if count == 1 && texts.len() == 1 && group != 2 {
            marks.push(format!("`{}`", semilla_core::text::flat_text(&texts[0])));
        } else if count == 1 && texts.len() == 1 {
            marks.push(texts[0].clone());
        }
        if implicit == count {
            marks.push("implicit".into());
        } else if implicit > 0 {
            marks.push(format!("{implicit} implicit"));
        }
        let marks = if marks.is_empty() { String::new() } else { format!(" ({})", marks.join(", ")) };
        out.push(format!("{sign}{count} {}{marks}", plural(&label, count)));
    }
    out.join(", ")
}

/// The places of the anchor of one side.
fn anchor_places(element: &Element) -> impl Iterator<Item = &(usize, MirPlace)> {
    element.anchor.elements.iter().chain(element.anchor.points.iter())
}

/// [R98.2-1] Gives each graph change of the step its entry (design 98), and the graph part of
/// each entry. It runs after the map of the diff is complete (`diffmap::entries`).
pub(crate) fn apply(input: &crate::StepInput, outcome: &mut crate::StepOutcome) {
    let bodies: Vec<BodyChanges> = std::mem::take(&mut outcome.flow_changes);
    if bodies.is_empty() || outcome.report.flow.is_none() {
        return;
    }
    let facts = &outcome.report.facts;
    let full_step = semilla_core::budget::step_text_command(outcome.report.step);
    // The owners of each element place: the edits whose nodes own it (design 90).
    let mut owners: BTreeMap<(usize, &MirPlace), Vec<usize>> = BTreeMap::new();
    for (edit, mir) in &outcome.report.node_mir {
        for side in 0..2 {
            for place in &mir.places[side] {
                owners.entry((side, place)).or_default().push(*edit);
            }
        }
    }
    let fact_of = |edit: usize| outcome.fact_of.get(edit).copied().flatten().filter(|fact| *fact < facts.len());
    let mut map = crate::mirmap::MirMap::new(input.before, input.after, &outcome.edits);
    let by_id: BTreeMap<&str, usize> = facts.iter().enumerate().map(|(at, fact)| (fact.id.as_str(), at)).collect();
    // [R98.6-1] The places of the code that a transformation does not explain, with its entry.
    let lefts: Vec<(usize, &MirPlace, usize)> = outcome.transforms.left.iter().filter_map(|left| by_id.get(left.fact.as_str()).map(|fact| (left, *fact))).flat_map(|(left, fact)| (0..2).flat_map(move |side| left.places[side].iter().map(move |place| (side, place, fact)))).collect();
    let left_of = |element: &Element| -> Option<usize> { anchor_places(element).find_map(|(side, place)| lefts.iter().find(|(found, at, _)| found == side && (*at == place || (at.0 == place.0 && at.1 <= place.1 && place.2 <= at.2))).map(|(_, _, fact)| *fact)) };
    // The entry of each element of each body.
    let mut placed: Vec<Vec<Option<Placed>>> = Vec::with_capacity(bodies.len());
    for body in &bodies {
        let mut list: Vec<Option<Placed>> = Vec::with_capacity(body.elements.len());
        for element in &body.elements {
            if element.parts.is_empty() {
                list.push(None);
                continue;
            }
            let implicit = element.anchor.implicit;
            // [R98.4-1] A body with no edit has no node: its entry of the section of R92.4-1.
            if !body.edit {
                list.push(Some(Placed { fact: None, implicit, explained: None }));
                continue;
            }
            // 1. The projection of design 90.
            let projected: Vec<usize> = element.anchor.elements.iter().filter_map(|(side, place)| owners.get(&(*side, place))).flatten().filter_map(|edit| fact_of(*edit)).collect();
            let mut found = best(facts, &projected).map(|fact| Placed { fact: Some(fact), implicit, explained: None });
            // 2. [R98.3-1] The smallest changed node that holds the place. A point whose MIR
            // elements have no owner in an aligned body has no source node of its own: the
            // change is implicit. With no table (a body that one tree has, an equal MIR), the
            // place gives the node.
            let unowned = implicit || (body.aligned && !element.anchor.elements.is_empty());
            if found.is_none() {
                found = anchor_places(element).find_map(|(side, place)| map.holder(*side, place).and_then(fact_of)).map(|fact| Placed { fact: Some(fact), implicit: unowned, explained: None });
            }
            list.push(Some(found.unwrap_or(Placed { fact: None, implicit: true, explained: None })));
        }
        placed.push(list);
    }
    // 3. The entry of the body for a change with no entry yet, and 4. the section of R92.4-1.
    for (body, list) in bodies.iter().zip(placed.iter_mut()) {
        let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
        for found in list.iter().flatten() {
            if let Some(fact) = found.fact {
                *counts.entry(fact).or_default() += 1;
            }
        }
        let own_no_edit = facts.iter().position(|fact| no_edit(fact) && fact.affected.contains(&body.name));
        let most = counts.iter().max_by_key(|(fact, count)| (**count, std::cmp::Reverse(number(&facts[**fact])))).map(|(fact, _)| *fact);
        for found in list.iter_mut().flatten().filter(|found| found.fact.is_none()) {
            found.fact = if body.edit { own_no_edit.or(most) } else { own_no_edit };
        }
    }
    // 5. [R98.6-1] A transformation explains its changes, except the code that its check does not
    // explain (R94.1-2). The equal MIR of a body explains each change of the body.
    for (body, list) in bodies.iter().zip(placed.iter_mut()) {
        for (element, found) in body.elements.iter().zip(list.iter_mut()) {
            let Some(found) = found.as_mut() else { continue };
            // An equal MIR after the deeper normal form (a level above `n1`: the inlined helpers
            // and callees) explains the graph of the MIR at N0. An equal text at `n0` or `n1` (equal
            // under the rename map) explains a changed source text and a changed label of an arrow
            // (a rename), not a changed call target (a new resolved callee, design 61.10) and not
            // an added or a removed point.
            if let Some(level) = &body.level {
                let deep = !matches!(level.as_str(), "n0" | "n1");
                let textual = element.parts.iter().all(|part| match part.group {
                    2 => !matches!(part.label.as_str(), "changed call target" | "changed panic call"),
                    3 | 4 => true,
                    _ => false,
                });
                if deep || textual {
                    found.explained = Some(format!("the equal MIR at `{level}`"));
                    continue;
                }
            }
            let Some(fact) = found.fact else { continue };
            if rank(&facts[fact]) != 3 {
                continue;
            }
            match left_of(element) {
                Some(left) => found.fact = Some(left),
                None => found.explained = Some(check_words(&facts[fact])),
            }
        }
    }
    // The graph part of each entry, the lines of each fact, the bodies with no entry, and the
    // counts.
    let mut parts: BTreeMap<usize, Vec<(Part, bool)>> = BTreeMap::new();
    let mut explained: BTreeMap<usize, (usize, String)> = BTreeMap::new();
    let mut fact_lines: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut whole: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut no_entry: Vec<NoEntryGraph> = Vec::new();
    let mut cover = FlowCover::default();
    for (body, list) in bodies.iter().zip(&placed) {
        let mut alone: Vec<(Part, bool)> = Vec::new();
        let mut alone_lines: Vec<String> = Vec::new();
        let whole_body = body.word.starts_with("graph added") || body.word.starts_with("graph removed");
        for (element, found) in body.elements.iter().zip(list) {
            let Some(found) = found else { continue };
            let count = units(element);
            cover.total += count;
            let section_one = found.fact.is_none_or(|fact| no_edit(&facts[fact]));
            if section_one {
                cover.no_edit += count;
            } else {
                cover.entries += count;
                if found.implicit {
                    cover.implicit += count;
                }
                if found.explained.is_some() {
                    cover.explained += count;
                }
            }
            let entry = match found.fact {
                Some(fact) => {
                    let mut words = format!("in {}", facts[fact].id);
                    if no_edit(&facts[fact]) {
                        words.push_str(" (R92.4-1)");
                    }
                    if found.implicit {
                        words.push_str(", implicit");
                    }
                    if let Some(by) = &found.explained {
                        words.push_str(&format!(", explained by {by}"));
                    }
                    words
                }
                None => match &found.explained {
                    Some(by) => format!("no entry ({RULE_NO_ENTRY}), in the section of R92.4-1, explained by {by}"),
                    None => format!("no entry ({RULE_NO_ENTRY}), in the section of R92.4-1"),
                },
            };
            for detail in &element.details {
                cover.list.push(format!("g{} {detail} in `{}`: {entry}", cover.list.len() + 1, body.shown));
            }
            match (found.fact, &found.explained) {
                (Some(fact), Some(by)) => {
                    let slot = explained.entry(fact).or_insert((0, by.clone()));
                    slot.0 += count;
                }
                (Some(fact), None) => {
                    parts.entry(fact).or_default().extend(element.parts.iter().map(|part| (part.clone(), found.implicit)));
                    // A full graph of a body that one tree has is one line with the counts when
                    // the entry has each of its changes (design 60, B5).
                    let all = whole_body && list.iter().flatten().all(|other| other.fact == Some(fact) && other.explained.is_none());
                    if all {
                        whole.entry(fact).or_default().push(body.name.clone());
                    } else {
                        fact_lines.entry(fact).or_default().push(element.text.clone());
                    }
                }
                // A change that the equal MIR explains needs no line of its own.
                (None, Some(_)) => {}
                (None, None) => {
                    alone.extend(element.parts.iter().map(|part| (part.clone(), found.implicit)));
                    alone_lines.push(element.text.clone());
                }
            }
        }
        if !alone.is_empty() {
            let owners: Vec<String> = facts.iter().filter(|fact| fact.affected.contains(&body.name)).map(|fact| fact.id.clone()).collect();
            let changes = alone.len();
            alone_lines.truncate(semilla_core::report::FLOW_NO_ENTRY_LINES);
            no_entry.push(NoEntryGraph { body: body.shown.clone(), place: body.place.clone(), summary: summary(&alone), changes, owners, lines: alone_lines });
        }
    }
    // The bodies of each fact for "same graph".
    let with_graph: BTreeMap<&str, &str> = bodies.iter().map(|body| (body.name.as_str(), body.word.as_str())).collect();
    let flow = outcome.report.flow.as_mut().expect("the flow data");
    // The lines of the fact (design 60, C1): the graph lines first, then the lines that the
    // step gave before (the changed instances of a generic body, design 61).
    let earlier: BTreeMap<String, Vec<String>> = std::mem::take(&mut flow.facts);
    for (at, fact) in facts.iter().enumerate() {
        let mut found: Vec<String> = Vec::new();
        for line in fact_lines.get(&at).into_iter().flatten() {
            if !found.contains(line) {
                found.push(line.clone());
            }
        }
        let more = found.len().saturating_sub(FACT_LINES);
        found.truncate(FACT_LINES);
        if more > 0 {
            found.push(format!("… {more} more ({full_step})"));
        }
        // A full graph of a body that one tree has: one line with the counts (design 60, B5).
        let mut seen: BTreeSet<&String> = BTreeSet::new();
        for name in whole.get(&at).into_iter().flatten() {
            if !seen.insert(name) {
                continue;
            }
            if let Some(body) = flow.bodies.iter().find(|body| body.name == *name || body.name.starts_with(&format!("{name} "))) {
                let sign = if body.line.starts_with("graph removed") { "−" } else { "+" };
                let counts = body.line.split_once(": ").map(|(_, rest)| rest.split("; ").next().unwrap_or(rest).to_string()).unwrap_or_default();
                found.push(format!("{sign} graph of `{}`: {counts} ({full_step})", body.name));
            }
        }
        if let Some(lines) = earlier.get(&fact.id) {
            for line in lines {
                if !found.contains(line) {
                    found.push(line.clone());
                }
            }
        }
        if !found.is_empty() {
            flow.facts.insert(fact.id.clone(), found);
        }
        // [R98.2-2] The graph part of the entry.
        let own = parts.get(&at);
        let (count, by) = explained.get(&at).cloned().unwrap_or((0, String::new()));
        let implicit = own.map_or(0, |list| list.iter().filter(|(_, implicit)| *implicit).count());
        let changes = own.map_or(0, Vec::len);
        let graph = if changes > 0 {
            let only_changed = own.is_some_and(|list| list.iter().all(|(part, _)| part.group == 2));
            Some(EntryGraph { word: if only_changed { "same structure".into() } else { "graph changes".into() }, summary: summary(own.expect("parts")), changes, implicit, explained: count, by })
        } else if count > 0 {
            Some(EntryGraph { word: "no change after the check".into(), summary: String::new(), changes: 0, implicit: 0, explained: count, by })
        } else if matches!(rank(fact), 0..=2) {
            // An entry with no graph change of its own in a body with graph data.
            // A closure or a coroutine body `f::{closure#0}` is a part of `f` (R60.3-6).
            let words: Vec<&str> = with_graph.iter().filter(|(name, _)| fact.affected.iter().any(|affected| *name == affected || name.starts_with(&format!("{affected}::{{")))).map(|(_, word)| *word).collect();
            if words.is_empty() {
                None
            } else if words.iter().all(|word| *word == "same graph") {
                Some(EntryGraph { word: "same graph (the edit keeps the control flow)".into(), ..Default::default() })
            } else if let Some(word) = words.iter().find(|word| word.starts_with("graph not shown") || word.starts_with("not compared") || word.starts_with("no graph data")) {
                Some(EntryGraph { word: (*word).to_string(), ..Default::default() })
            } else {
                Some(EntryGraph { word: "no graph change of its own".into(), ..Default::default() })
            }
        } else {
            None
        };
        if let Some(graph) = graph {
            flow.entries.insert(fact.id.clone(), graph);
        }
    }
    flow.no_entry = no_entry;
    flow.cover = Some(cover);
}

/// [R98.5-1] The lines of the full text: each graph change with its entry (design 98).
pub fn lines(flow: &semilla_core::report::FlowReport) -> Vec<String> {
    let Some(cover) = &flow.cover else { return Vec::new() };
    let mut out = vec![format!("    each control-flow change (of {}):", cover.total)];
    out.extend(cover.list.iter().map(|line| format!("      {line}")));
    out
}
