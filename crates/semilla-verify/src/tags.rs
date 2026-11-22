//! The tags of the agent (design 55.3). A tag is a name for a part of the step. The agent
//! gives the part by positions in the line diff. A tag is not a fact: it makes no claim about
//! the behavior, and it changes no fact, no line of the gap list, and no result. A tag has each
//! fact whose edit is fully in its lines. The unit of a tag is the hunk of the line diff of the
//! step (design 55.8): a place selects changed lines of the hunks.

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

/// The changed lines that the places of a tag select (design 55.3, rule 2): (file, old tree,
/// line).
type Selected<'a> = BTreeSet<(&'a str, bool, usize)>;

/// The lines that the places select in the hunks of the line diff of the step.
fn selected<'a>(report: &'a StepReport, places: &[Place]) -> Selected<'a> {
    let mut out = BTreeSet::new();
    for hunk in &report.hunks {
        for place in places {
            for (old, start, end) in place.selects(hunk) {
                out.extend((start..end).map(|line| (hunk.file.as_str(), old, line)));
            }
        }
    }
    out
}

/// The lines of a fact that the tag does not select.
fn outside(fact: &Fact, selected: &Selected) -> Vec<Position> {
    let lines = fact.positions.iter().flat_map(|position| (position.start..=position.end).map(move |line| (position, line)));
    Position::normal(lines.filter(|(position, line)| !selected.contains(&(position.file.as_str(), position.old, *line))).map(|(position, line)| Position { file: position.file.clone(), old: position.old, start: line, end: line }).collect())
}

/// The place of a fact in a tag (design 55.3, rules 4 and 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InTag {
    /// The tag selects each line of the fact, or each line of one side of the fact.
    Full,
    /// The tag selects a line of the fact, and not each line of one side.
    Part,
    /// The tag selects no line of the fact. A fact with no position is in no tag.
    No,
}

/// The rule of a tag (design 55.3, rule 4): the fact is in the tag if each line of its
/// position is a selected line of the tag. The two sides of a fact are the two sides of one
/// change, as the two sides of a hunk: a full side stands for the two sides. Thus the fact is
/// also in the tag if each of its new-tree lines is a selected line, or each of its old-tree
/// lines is (Decided by Claude: the rule of batch 9b4 stays for a place that has a part of a
/// hunk).
pub(crate) fn in_tag(fact: &Fact, selected: &Selected) -> InTag {
    let has = |position: &Position, line: usize| selected.contains(&(position.file.as_str(), position.old, line));
    let mut counts = [(0, 0), (0, 0)];
    for position in &fact.positions {
        for line in position.start..=position.end {
            let side = &mut counts[position.old as usize];
            side.0 += 1;
            side.1 += has(position, line) as usize;
        }
    }
    if counts.iter().any(|(all, inside)| *all > 0 && all == inside) {
        InTag::Full
    } else if counts.iter().any(|(_, inside)| *inside > 0) {
        InTag::Part
    } else {
        InTag::No
    }
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
    let changed: BTreeSet<&str> = report.hunks.iter().map(|hunk| hunk.file.as_str()).collect();
    for place in &places {
        // The form: a line number after the last line of the file.
        if let Some((old, _, end)) = place.lines.filter(|_| !place.empty) {
            let tree = if old { input.before } else { input.after };
            if let Some(count) = line_count(tree, &place.file).filter(|count| end > *count) {
                tag.error = Some(format!("the position `{}` of the tag `{}` is after the last line: `{}` has {count} line(s) in the {} tree", place.text(), tag.name, place.file, if old { "old" } else { "new" }));
                tag.form = true;
                return tag;
            }
        }
        // A place that selects no changed line.
        if report.hunks.iter().any(|hunk| !place.selects(hunk).is_empty()) {
            continue;
        }
        let cause = if !changed.contains(place.file.as_str()) {
            let mut files: Vec<&str> = changed.iter().copied().collect();
            let more = if files.len() > 6 { format!(", and {} more", files.len() - 6) } else { String::new() };
            files.truncate(6);
            format!("the step does not change the file `{}`; the changed files: {}{more}", place.file, files.join(", "))
        } else if place.empty {
            let headers: Vec<String> = report.hunks.iter().filter(|hunk| hunk.file == place.file).map(|hunk| hunk.header()).collect();
            let more = if headers.len() > 6 { format!(", and {} more", headers.len() - 6) } else { String::new() };
            format!("no hunk of `{}` has the side `{}`; the hunks of the file: {}{more}", place.file, place.text().rsplit(':').next().unwrap_or_default(), headers[..headers.len().min(6)].join(", "))
        } else {
            let near = nearest(report, place);
            format!("the step has no edit in `{}`; the nearest positions with an edit: {}", place.text(), near.join(", "))
        };
        tag.error = Some(format!("the tag `{}` has a position with no edit: {cause}", tag.name));
        return tag;
    }
    let selected = selected(report, &places);
    for fact in &report.facts {
        match in_tag(fact, &selected) {
            InTag::Full => tag.facts.push(fact.id.clone()),
            InTag::Part => {
                tag.partial.push(fact.id.clone());
                tag.notes.push(format!("in part: {} has lines that are not in the tag: {}", fact.id, Position::list(&outside(fact, &selected))));
            }
            InTag::No => {}
        }
    }
    // The count of a tag is the count of the changed lines that its places select, on the two
    // sides (design 55.3): the counts of tags with no line in common, and the count of the
    // lines that no tag selects, give the count of the cover line.
    tag.lines = selected.len();
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
