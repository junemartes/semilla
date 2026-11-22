//! The comments of the changed items (design 58). A fact with an edit in the body or the
//! declaration of an item gets the comments of each node from the changed node up to the
//! item. This is the last pass of a step. It adds the comments to the facts. It changes no
//! other part of a fact, it makes no fact, and it rejects no step.

use std::collections::BTreeMap;

use semilla_core::report::{CommentState, Fact, Position, RelatedComment};
use semilla_core::{Anchor, Category};
use semilla_syntax::comments::{Attach, FileComments, Step, UnitKind};
use semilla_syntax::rs::{Edition, SyntaxElement};
use semilla_syntax::{Item, Node, Resolved, Workspace};

use crate::{StepInput, StepOutcome};

const OLD: usize = 0;
const NEW: usize = 1;

/// One tree of the step, with the comments of the files that the pass reads.
struct Tree<'a> {
    workspace: &'a Workspace,
    files: BTreeMap<String, Option<FileComments>>,
    /// The path of the item that starts at a position: (file, line, column).
    names: BTreeMap<(&'a str, usize, usize), &'a str>,
}

impl<'a> Tree<'a> {
    fn new(workspace: &'a Workspace) -> Tree<'a> {
        let mut names: BTreeMap<(&str, usize, usize), &str> = BTreeMap::new();
        for item in workspace.items.values() {
            names.entry((item.file.as_str(), item.line_start, item.col_start)).or_insert(item.path.as_str());
        }
        Tree { workspace, files: BTreeMap::new(), names }
    }

    /// The comments of a Rust file of the tree.
    fn file(&mut self, name: &str) -> Option<&FileComments> {
        if !self.files.contains_key(name) {
            let comments = self.workspace.rust_files.get(name).map(|text| FileComments::new(text, self.workspace.editions.get(name).copied().unwrap_or(Edition::Edition2021)));
            self.files.insert(name.to_string(), comments);
        }
        self.files.get(name).and_then(Option::as_ref)
    }

    /// The path from a place of the tree (an item, or a node of an item) up to the item of
    /// the place.
    fn path(&mut self, item: &Item, node: Option<&Node>) -> Option<Vec<Step>> {
        let file = self.file(&item.file)?;
        let in_item = |element: &SyntaxElement| {
            let line = file.pos(usize::from(element.text_range().start())).0;
            item.line_start <= line && line <= item.line_end
        };
        // Design 58.3: the path goes up from the node to the item of the place.
        let end = (item.line_start, item.col_start);
        match node.and_then(|node| file.element(node.start, node.end)).filter(in_item) {
            Some(element) => file.path(&element, Some(end)),
            None => file.path(&file.node_at(end)?, Some(end)),
        }
    }
}

/// One comment of a path, before its state is known.
#[derive(Clone)]
struct Found {
    file: String,
    unit: usize,
    attach: Attach,
    /// The name of the node: the path of an item, or `None` for a node with no name.
    name: Option<String>,
    /// The first line of the code of the node.
    line: usize,
    /// True if the step removed the node (an edit with no new side).
    removed: bool,
}

/// The owner of a path: the item of the syntax tree that has the node, or the inner docs of
/// the file of a module. The units of the old owner and of the new owner are a pair list.
#[derive(Clone)]
struct Owner {
    file: String,
    /// The offset of the item, or `None` for the inner docs of a file.
    offset: Option<usize>,
    /// The path of the item, if Semilla has an item at the node.
    name: Option<String>,
    /// Each unit of the owner, in source order.
    units: Vec<usize>,
}

/// The comments of the paths of one fact on one side, from the item down to the changed node.
#[derive(Default)]
struct SidePaths {
    found: Vec<Found>,
    owners: Vec<Owner>,
}

impl SidePaths {
    fn add(&mut self, tree: &mut Tree, item: &Item, node: Option<&Node>, removed: bool) {
        let Some(path) = tree.path(item, node) else { return };
        let names = &tree.names;
        let Some(file) = tree.files.get(&item.file).and_then(Option::as_ref) else { return };
        let starts: Vec<(usize, usize)> = path.iter().map(|step| file.pos(step.offset)).collect();
        for (step, (line, column)) in path.iter().zip(&starts).rev() {
            let name = names.get(&(item.file.as_str(), *line, *column)).map(|name| name.to_string());
            for (attach, unit) in &step.units {
                if !self.found.iter().any(|known| known.file == item.file && known.unit == *unit) {
                    self.found.push(Found { file: item.file.clone(), unit: *unit, attach: *attach, name: name.clone(), line: step.line, removed });
                }
            }
        }
        if let Some((step, (line, column))) = path.last().zip(starts.last()).filter(|(step, _)| step.item) {
            if !self.owners.iter().any(|owner| owner.file == item.file && owner.offset == Some(step.offset)) {
                let name = names.get(&(item.file.as_str(), *line, *column)).map(|name| name.to_string());
                self.owners.push(Owner { file: item.file.clone(), offset: Some(step.offset), name, units: file.item_units(step) });
            }
        }
        // Design 58.1: a module with a file has the inner docs of the file.
        if node.is_none() && item.kind == semilla_syntax::ItemKind::Mod {
            self.add_module(tree, &item.path, removed);
        }
    }

    /// Adds the inner docs of the file of a module (design 58.1). A crate root is a module
    /// with a file and with no `mod` line.
    fn add_module(&mut self, tree: &mut Tree, module: &str, removed: bool) {
        let Some(file) = tree.workspace.module_files.get(module).cloned() else { return };
        let Some(unit) = tree.file(&file).and_then(FileComments::inner_docs) else { return };
        if !self.found.iter().any(|known| known.file == file && known.unit == unit) {
            self.found.push(Found { file: file.clone(), unit, attach: Attach::Docs, name: Some(module.to_string()), line: 1, removed });
            self.owners.push(Owner { file, offset: None, name: Some(module.to_string()), units: vec![unit] });
        }
    }
}

/// The place text of a comment: ``docs of `shop::price` ``, `comment above line 4`.
fn place(found: &Found, unit_line: usize, old: bool, was: bool) -> String {
    let word = if found.attach == Attach::Docs { "docs" } else { "comment" };
    let was = if was { " that was" } else { "" };
    let line = |line: usize| if found.removed { format!("the removed code at old line {line}") } else if old { format!("old line {line}") } else { format!("line {line}") };
    let node = match &found.name {
        Some(name) => format!("`{name}`"),
        None => line(found.line),
    };
    match found.attach {
        Attach::Docs if found.name.is_some() => format!("{word}{was} of {node}"),
        Attach::Docs => format!("{word}{was} at {node}"),
        Attach::Attributes if found.name.is_some() => format!("{word}{was} in the attributes of {node}"),
        Attach::Attributes => format!("{word}{was} in the attributes at {node}"),
        Attach::Above => format!("{word}{was} above {node}"),
        Attach::LineEnd => format!("{word}{was} at the end of {}", line(unit_line)),
    }
}

/// A unit of a file: the file and the index of the unit.
type UnitKey = (String, usize);
/// A part of a unit: the file, the index of the unit, and the index of the part.
type PartKey = (String, usize, usize);

/// The pairs of the parts of the comments of one fact (design 58.4): `to[OLD]` gives the new
/// part of an old part, `to[NEW]` gives the old part of a new part.
#[derive(Default)]
struct Pairs {
    to: [BTreeMap<PartKey, PartKey>; 2],
}

/// The comments that the pairs of their parts link with one comment, on the two sides.
struct Group {
    units: [Vec<UnitKey>; 2],
    /// True if each part of each comment of the group has a pair: the step has no edit in a
    /// comment of the group.
    full: bool,
}

impl Pairs {
    /// The group of a comment.
    fn group(&self, side: usize, key: &UnitKey, part_count: &dyn Fn(usize, &UnitKey) -> usize) -> Group {
        let mut group = Group { units: [Vec::new(), Vec::new()], full: true };
        group.units[side].push(key.clone());
        let mut next = vec![(side, key.clone())];
        while let Some((side, key)) = next.pop() {
            for part in 0..part_count(side, &key) {
                match self.to[side].get(&(key.0.clone(), key.1, part)) {
                    Some((file, unit, _)) => {
                        let other = (file.clone(), *unit);
                        if !group.units[1 - side].contains(&other) {
                            group.units[1 - side].push(other.clone());
                            next.push((1 - side, other));
                        }
                    }
                    None => group.full = false,
                }
            }
        }
        group
    }
}

/// The pass. It runs after the positions and the facts of the agent.
pub(crate) fn apply(input: &StepInput, outcome: &mut StepOutcome) {
    if !outcome.report.facts.iter().any(|fact| fact.category.changes_item()) {
        return;
    }
    let mut trees = [Tree::new(input.before), Tree::new(input.after)];
    let renames = &outcome.renames;
    // The old path of each new item path, and the new path of each old item path: from the
    // edits with two sides, then from the renames of the step.
    let mut pairs: [BTreeMap<&str, &str>; 2] = [BTreeMap::new(), BTreeMap::new()];
    for (old, new) in &renames.items {
        pairs[OLD].insert(old.as_str(), new.as_str());
        pairs[NEW].insert(new.as_str(), old.as_str());
    }
    for edit in &outcome.edits {
        if let (Some(before), Some(after)) = (&edit.before, &edit.after) {
            pairs[OLD].entry(before.item.as_str()).or_insert(after.item.as_str());
            pairs[NEW].entry(after.item.as_str()).or_insert(before.item.as_str());
        }
    }
    let partner = |side: usize, item: &str| -> String { pairs[side].get(item).map_or(item, |other| *other).to_string() };
    // The pairs of the equal parts of the comments of an old owner and a new owner (design
    // 58.4), by the files and the offsets of the two owners.
    let mut paired: BTreeMap<(String, Option<usize>, String, Option<usize>), Vec<((usize, usize), (usize, usize))>> = BTreeMap::new();
    let mut comments: Vec<Vec<RelatedComment>> = Vec::with_capacity(outcome.report.facts.len());
    for (index, fact) in outcome.report.facts.iter().enumerate() {
        let mut out: Vec<RelatedComment> = Vec::new();
        if !fact.category.changes_item() {
            comments.push(out);
            continue;
        }
        // The places of the fact (design 58.2): the place of the fact, and each edit of the
        // fact in the item of that place. An edit in a different item is a site of the fact.
        // A `rename` fact has only its place: the item that declares the name.
        let mut places: [Vec<(Anchor, bool)>; 2] = [Vec::new(), Vec::new()];
        // A fact that is the fold of equal facts has the place of each of them.
        let mut ats: Vec<&Anchor> = fact.at.iter().collect();
        for group in fact.sites.iter().filter(|group| fact.folds(group) || fact.names_sites(group)) {
            for site in &group.sites {
                if !ats.contains(&site) {
                    ats.push(site);
                }
            }
        }
        let own: Option<Vec<String>> = (!ats.is_empty()).then(|| ats.iter().flat_map(|at| [at.item.clone(), partner(OLD, &at.item), partner(NEW, &at.item)]).collect());
        // Design 58.2: an item that is in one tree only (a new item, a removed item) gives no
        // comment. Its fact is the full item. A fact with a removed item and a new item is
        // the fact of a changed item (a changed `use` line): the item is in the two trees.
        let mut one_tree: Vec<&str> = Vec::new();
        let edits = || outcome.edits.iter().zip(&outcome.fact_of).filter(|(_, owner)| **owner == Some(index)).map(|(edit, _)| edit);
        let full_item = |side: usize| edits().any(|edit| [&edit.before, &edit.after][side].as_ref().is_some_and(|anchor| anchor.path.is_empty()) && [&edit.before, &edit.after][1 - side].is_none());
        let pair = full_item(OLD) && full_item(NEW);
        if fact.category != Category::Rename {
            for edit in edits() {
                if let (Some(anchor), None) | (None, Some(anchor)) = (&edit.before, &edit.after) {
                    if anchor.path.is_empty() && !pair {
                        one_tree.push(anchor.item.as_str());
                        continue;
                    }
                }
                for (side, anchor) in [&edit.before, &edit.after].into_iter().enumerate() {
                    let Some(anchor) = anchor else { continue };
                    if own.as_ref().is_none_or(|own| own.contains(&anchor.item)) {
                        places[side].push((anchor.clone(), side == OLD && edit.after.is_none()));
                    }
                }
            }
        }
        for at in ats {
            if at.path.is_empty() && one_tree.contains(&at.item.as_str()) {
                continue;
            }
            for side in [OLD, NEW] {
                let workspace = trees[side].workspace;
                let mut anchor = at.clone();
                if !workspace.items.contains_key(&anchor.item) {
                    anchor.item = partner(1 - side, &at.item);
                }
                if places[side].iter().any(|(known, _)| known.item == anchor.item) {
                    continue;
                }
                // The edits of the fact have a node only in the other tree (an added or a
                // removed node): the path of the place is a path of that tree. In this tree
                // the place is the item.
                let other = partner(side, &anchor.item);
                if places[1 - side].iter().any(|(known, _)| known.item == other || known.item == anchor.item) {
                    anchor.path.clear();
                }
                places[side].push((anchor, false));
            }
        }
        let mut sides = [SidePaths::default(), SidePaths::default()];
        for side in [OLD, NEW] {
            for (anchor, removed) in &places[side] {
                let workspace = trees[side].workspace;
                match semilla_syntax::resolve(workspace, anchor) {
                    Some(Resolved::Item(item)) => sides[side].add(&mut trees[side], item, None, *removed),
                    Some(Resolved::Node(item, node)) => sides[side].add(&mut trees[side], item, Some(node), *removed),
                    None => match workspace.items.get(&anchor.item) {
                        Some(item) => sides[side].add(&mut trees[side], item, None, *removed),
                        None => sides[side].add_module(&mut trees[side], &anchor.item, *removed),
                    },
                }
            }
        }
        // The owner of the other side for a fact with one side: the item with the paired path.
        for side in [OLD, NEW] {
            if sides[side].owners.is_empty() {
                let items: Vec<String> = places[1 - side].iter().map(|(anchor, _)| partner(1 - side, &anchor.item)).collect();
                for item in items {
                    if let Some(item) = trees[side].workspace.items.get(&item) {
                        let mut other = SidePaths::default();
                        other.add(&mut trees[side], item, None, false);
                        sides[side].owners.extend(other.owners);
                    }
                }
            }
        }
        // Design 58.4: the pairs of the parts of the comments of the old owners and the new
        // owners of the fact.
        let mut pairs_of = Pairs::default();
        let alone = sides[OLD].owners.iter().filter(|owner| owner.offset.is_some()).count() == 1 && sides[NEW].owners.iter().filter(|owner| owner.offset.is_some()).count() == 1;
        for old in &sides[OLD].owners {
            for new in &sides[NEW].owners {
                // The two owners are the old form and the new form of one item.
                let same = match (&old.name, &new.name) {
                    (Some(old), Some(new)) => partner(OLD, old) == *new || partner(NEW, new) == *old,
                    _ => false,
                };
                if old.offset.is_some() != new.offset.is_some() || !(same || (alone && old.offset.is_some())) {
                    continue;
                }
                let key = (old.file.clone(), old.offset, new.file.clone(), new.offset);
                if !paired.contains_key(&key) {
                    let (Some(old_file), Some(new_file)) = (trees[OLD].files.get(&old.file).and_then(Option::as_ref), trees[NEW].files.get(&new.file).and_then(Option::as_ref)) else { continue };
                    let parts = |file: &FileComments, units: &[usize]| -> Vec<(usize, usize)> { units.iter().flat_map(|unit| (0..file.units[*unit].parts.len()).map(|part| (*unit, part))).collect() };
                    let (old_parts, new_parts) = (parts(old_file, &old.units), parts(new_file, &new.units));
                    let part = |file: &FileComments, (unit, part): (usize, usize)| (file.units[unit].kind == UnitKind::Docs, file.units[unit].parts[part].text.clone());
                    let found = crate::positions::matches(old_parts.len(), new_parts.len(), &|a, b| part(old_file, old_parts[a]) == part(new_file, new_parts[b]));
                    paired.insert(key.clone(), found.into_iter().map(|(a, b)| (old_parts[a], new_parts[b])).collect());
                }
                for ((old_unit, old_part), (new_unit, new_part)) in &paired[&key] {
                    let (a, b) = ((old.file.clone(), *old_unit, *old_part), (new.file.clone(), *new_unit, *new_part));
                    if !pairs_of.to[OLD].contains_key(&a) && !pairs_of.to[NEW].contains_key(&b) {
                        pairs_of.to[OLD].insert(a.clone(), b.clone());
                        pairs_of.to[NEW].insert(b, a);
                    }
                }
            }
        }
        let unit_of = |trees: &[Tree; 2], side: usize, key: &UnitKey| -> Option<(String, Position)> {
            let unit = trees[side].files.get(&key.0).and_then(Option::as_ref)?.units.get(key.1)?;
            Some((unit.text.clone(), Position { file: key.0.clone(), old: side == OLD, start: unit.lines.0, end: unit.lines.1 }))
        };
        let part_count = |side: usize, key: &UnitKey| trees[side].files.get(&key.0).and_then(Option::as_ref).and_then(|file| file.units.get(key.1)).map_or(0, |unit| unit.parts.len());
        // The facts of a group of comments: the facts with a line of one of them.
        let facts_of = |group: &Group| -> Vec<String> {
            let mut ids: Vec<&Fact> = Vec::new();
            for side in [NEW, OLD] {
                for key in &group.units[side] {
                    if let Some((_, position)) = unit_of(&trees, side, key) {
                        ids.extend(outcome.report.facts.iter().filter(|fact| fact.positions.iter().any(|known| known.file == position.file && known.old == position.old && known.start <= position.end && position.start <= known.end)));
                    }
                }
            }
            // A `docs` fact is the fact of a comment: with one, the result has only such facts.
            if ids.iter().any(|fact| fact.category == Category::Docs) {
                ids.retain(|fact| fact.category == Category::Docs);
            }
            let mut out: Vec<String> = Vec::new();
            for fact in ids {
                if !out.contains(&fact.id) {
                    out.push(fact.id.clone());
                }
            }
            out
        };
        // Design 58.4, item 5: true if a `docs` fact of the step has a line of the position.
        // The comment then has an edit of its place, and it never gets the mark "no edit".
        let in_docs_fact = |position: &Position| outcome.report.facts.iter().any(|fact| fact.category == Category::Docs && fact.positions.iter().any(|known| known.file == position.file && known.old == position.old && known.start <= position.end && position.start <= known.end));
        // The new side: each comment is a comment with no edit, or it has a fact.
        let mut open: Vec<(usize, Attach, bool)> = Vec::new();
        for found in &sides[NEW].found {
            let key = (found.file.clone(), found.unit);
            let Some((text, position)) = unit_of(&trees, NEW, &key) else { continue };
            let place = place(found, position.start, false, false);
            let group = pairs_of.group(NEW, &key, &part_count);
            if group.full {
                if in_docs_fact(&position) {
                    out.push(RelatedComment { place, state: CommentState::Moved, text, position, facts: facts_of(&group) });
                } else {
                    out.push(RelatedComment { place, state: CommentState::NoEdit, text, position, facts: Vec::new() });
                }
                continue;
            }
            let facts = facts_of(&group);
            let state = match (facts.is_empty(), group.units[OLD].is_empty()) {
                (true, _) => CommentState::NoPair,
                (false, true) => CommentState::Added,
                (false, false) => CommentState::Changed,
            };
            if state == CommentState::Added {
                open.push((out.len(), found.attach, found.name.is_some()));
            }
            out.push(RelatedComment { place, state, text, position, facts });
        }
        // The old side: a comment that is not at a node of the fact in the new tree. The step
        // removed its node (design 58.5), or the step removed or changed the comment.
        for found in &sides[OLD].found {
            let key = (found.file.clone(), found.unit);
            let Some((text, position)) = unit_of(&trees, OLD, &key) else { continue };
            let group = pairs_of.group(OLD, &key, &part_count);
            if group.units[NEW].iter().any(|new| sides[NEW].found.iter().any(|found| found.file == new.0 && found.unit == new.1)) {
                continue;
            }
            if group.full {
                // The lines of the comment in the new tree: the lines of the pairs of its parts.
                let lines: Vec<(&String, (usize, usize))> = (0..part_count(OLD, &key))
                    .filter_map(|part| pairs_of.to[OLD].get(&(key.0.clone(), key.1, part)))
                    .filter_map(|(file, unit, part)| Some((file, trees[NEW].files.get(file).and_then(Option::as_ref)?.units.get(*unit)?.parts.get(*part)?.lines)))
                    .collect();
                let (Some((file, first)), Some((_, last))) = (lines.first(), lines.last()) else { continue };
                let new_position = Position { file: file.to_string(), old: false, start: first.0, end: last.1 };
                if in_docs_fact(&new_position) {
                    out.push(RelatedComment { place: place(found, position.start, true, true), state: CommentState::Moved, text, position: new_position, facts: facts_of(&group) });
                } else {
                    out.push(RelatedComment { place: place(found, position.start, true, true), state: CommentState::NoEdit, text, position: new_position, facts: Vec::new() });
                }
                continue;
            }
            let facts = facts_of(&group);
            // An old comment and a new comment at the same kind of place with one fact: the
            // step changed the comment.
            let changed = open.iter().position(|(entry, attach, named)| group.units[NEW].is_empty() && *attach == found.attach && *named == found.name.is_some() && out[*entry].facts.iter().any(|fact| facts.contains(fact)));
            if let Some(at) = changed {
                let (entry, _, _) = open.remove(at);
                out[entry].state = CommentState::Changed;
                for fact in facts {
                    if !out[entry].facts.contains(&fact) {
                        out[entry].facts.push(fact);
                    }
                }
                continue;
            }
            let state = match (facts.is_empty(), group.units[NEW].is_empty()) {
                (true, _) => CommentState::NoPair,
                (false, true) => CommentState::Removed,
                (false, false) => CommentState::Changed,
            };
            out.push(RelatedComment { place: place(found, position.start, true, false), state, text, position, facts });
        }
        comments.push(out);
    }
    for (fact, comments) in outcome.report.facts.iter_mut().zip(comments) {
        fact.comments = comments;
    }
}
