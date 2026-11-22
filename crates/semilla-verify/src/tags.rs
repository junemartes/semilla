//! The tags of the agent (design 55.3). A tag is a name for a part of the step. The agent
//! gives the part by positions in the line diff. A tag is not a fact: it makes no claim about
//! the behavior, and it changes no fact, no line of the gap list, and no result. A tag has each
//! fact whose edit is fully in its lines.

use std::collections::BTreeSet;

use semilla_core::report::{parse_tag, Fact, Place, Position, StepReport, Tag};
use semilla_syntax::Workspace;

use crate::{StepInput, StepOutcome};

/// The text of a file of a tree.
fn file_text<'a>(workspace: &'a Workspace, file: &str) -> Option<&'a str> {
    workspace.rust_files.get(file).or_else(|| workspace.other_files.get(file)).map(String::as_str)
}

/// The count of the lines of a file of a tree, or `None` if the tree does not have the file as
/// a text.
fn line_count(workspace: &Workspace, file: &str) -> Option<usize> {
    file_text(workspace, file).map(|text| text.lines().count())
}

/// The lines of a fact on one side.
fn side(fact: &Fact, old: bool) -> Vec<&Position> {
    fact.positions.iter().filter(|position| position.old == old).collect()
}

/// True if each line of the position is in a place of the list. The places can share the
/// lines of the position between them.
fn inside(position: &Position, places: &[Place]) -> bool {
    (position.start..=position.end).all(|line| {
        let one = Position { file: position.file.clone(), old: position.old, start: line, end: line };
        places.iter().any(|place| place.contains(&one))
    })
}

/// The rule of a tag (design 55.3): the fact is in the tag if each line of its edit is in the
/// places. The two sides of a fact are the two sides of one change, as the two sides of a hunk
/// header. Thus the fact is in the tag if each of its new-tree lines is in the places, or each
/// of its old-tree lines is. A deleted text has old-tree lines only.
pub(crate) fn in_tag(fact: &Fact, places: &[Place]) -> bool {
    [false, true].into_iter().any(|old| {
        let lines = side(fact, old);
        !lines.is_empty() && lines.iter().all(|position| inside(position, places))
    })
}

/// True if a line of the fact is in a place.
fn meets(fact: &Fact, places: &[Place]) -> bool {
    fact.positions.iter().any(|position| places.iter().any(|place| place.meets(position)))
}

/// The positions with an edit that are nearest to a place with no edit: the last one before
/// the place and the first one after it, in the tree of the place, then in the other tree.
fn nearest(report: &StepReport, place: &Place) -> Vec<String> {
    let Some((old, start, end)) = place.lines else { return Vec::new() };
    let mut found: Vec<Position> = Vec::new();
    for tree in [old, !old] {
        let all = Position::normal(report.facts.iter().flat_map(|fact| fact.positions.iter()).chain(&report.no_fact).filter(|position| position.file == place.file && position.old == tree).cloned().collect());
        // The line numbers of the other tree do not compare with the place: its first and
        // its last position help the agent as well as a nearer one.
        let before = if tree == old { all.iter().filter(|position| position.end < start).next_back() } else { all.first() };
        let after = if tree == old { all.iter().find(|position| position.start > end) } else { all.last().filter(|_| all.len() > 1) };
        found.extend(before.into_iter().chain(after).cloned());
        if !found.is_empty() {
            break;
        }
    }
    found.iter().map(Position::text).collect()
}

/// Checks one tag and finds its facts.
fn one_tag(input: &StepInput, report: &StepReport, text: &str) -> Tag {
    let (places, name) = match parse_tag(text) {
        Ok(found) => found,
        Err(error) => return Tag { name: text.to_string(), error: Some(error), form: true, ..Default::default() },
    };
    let mut tag = Tag { name, places: places.iter().map(Place::text).collect(), ..Default::default() };
    let changed: BTreeSet<&str> = report.facts.iter().flat_map(|fact| fact.positions.iter()).chain(&report.no_fact).map(|position| position.file.as_str()).collect();
    for place in &places {
        // The form: a line number after the last line of the file.
        if let Some((old, _, end)) = place.lines {
            let tree = if old { input.before } else { input.after };
            if let Some(count) = line_count(tree, &place.file).filter(|count| end > *count) {
                tag.error = Some(format!("the position `{}` of the tag `{}` is after the last line: `{}` has {count} line(s) in the {} tree", place.text(), tag.name, place.file, if old { "old" } else { "new" }));
                tag.form = true;
                return tag;
            }
        }
        // A place with no edit in its lines.
        let single = std::slice::from_ref(place);
        if report.facts.iter().any(|fact| meets(fact, single)) || report.no_fact.iter().any(|position| place.meets(position)) {
            continue;
        }
        let cause = if !changed.contains(place.file.as_str()) {
            let mut files: Vec<&str> = changed.iter().copied().collect();
            let more = if files.len() > 6 { format!(", and {} more", files.len() - 6) } else { String::new() };
            files.truncate(6);
            format!("the step does not change the file `{}`; the changed files: {}{more}", place.file, files.join(", "))
        } else {
            let near = nearest(report, place);
            format!("the step has no edit in `{}`; the nearest positions with an edit: {}", place.text(), near.join(", "))
        };
        tag.error = Some(format!("the tag `{}` has a position with no edit: {cause}", tag.name));
        return tag;
    }
    for fact in &report.facts {
        if in_tag(fact, &places) {
            tag.facts.push(fact.id.clone());
        } else if meets(fact, &places) {
            tag.notes.push(format!("{} is not in the tag: only a part of its lines is in the positions of the tag; the full fact: {}", fact.id, Position::list(&fact.positions)));
        }
    }
    // The count of a tag is the count of the changed lines of its facts, on the two sides: the
    // counts of the groups of a step with no fact in two tags give the count of the cover line.
    tag.lines = report.line_count(&tag.facts);
    tag
}

/// Finds the facts of each tag of the step, and the width of each `change` fact of the agent.
/// The function reads the facts and the gap list. It changes none of them.
pub(crate) fn apply(input: &StepInput, outcome: &mut StepOutcome) {
    let report = &mut outcome.report;
    report.tags = input.step.tags.iter().map(|text| one_tag(input, report, text)).collect();
    // The width of a `change` fact (design 55.3): what the fact covers.
    let widths: Vec<String> = report
        .agent_facts
        .iter()
        .map(|agent| {
            if agent.kind != "change" || !agent.holds || (agent.facts.is_empty() && agent.partial.is_empty()) {
                return String::new();
            }
            let facts: Vec<&Fact> = agent.facts.iter().filter_map(|id| report.fact(id)).collect();
            let items: BTreeSet<String> = facts.iter().map(|fact| fact.at.as_ref().map(|at| at.item.clone()).or_else(|| fact.file.clone()).unwrap_or_default()).collect();
            let part = if agent.partial.is_empty() { String::new() } else { format!(", and a part of {} more fact(s)", agent.partial.len()) };
            format!("covers {} fact(s) in {} item(s) with {} changed line(s){part}", facts.len(), items.len(), report.line_count(&agent.facts))
        })
        .collect();
    for (agent, width) in report.agent_facts.iter_mut().zip(widths) {
        agent.covers = width;
    }
}
