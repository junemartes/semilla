//! Unit tests of design 98: the control-flow map (L2).

use semilla_core::report::{EntryGraph, FlowCover, FlowReport};
use semilla_core::{EntryKind, Fact, NodeMir, Position, Step};
use semilla_syntax::Workspace;

use super::*;
use crate::flow::{diff_pairs, graph_diff, test_element, Anchor, BodyChanges, FlowGraph, FlowPoint, FlowResult, Part};
use crate::flowalign::{align, entries_only, identity, link, TableSide};

fn at(start: usize, end: usize) -> MirPlace {
    ("src/lib.rs".to_string(), start, end)
}

fn point(kind: &str, text: &str, place: Option<MirPlace>) -> FlowPoint {
    FlowPoint { kind: kind.into(), text: text.into(), tokens: place, ..FlowPoint::default() }
}

fn graph(points: Vec<FlowPoint>, arrows: &[(usize, usize, &str)]) -> FlowGraph {
    FlowGraph { points, arrows: arrows.iter().map(|(a, b, label)| (*a, *b, label.to_string())).collect(), ..FlowGraph::default() }
}

/// The canonical text of a body `if x > 3 { g(x) } else { f(x) }`: a branch, two calls, and a
/// return, with the range of each element.
const TEXT: &str = "fn _(_1: u32) -> u32 {\n    let _0: u32;\n    let _2: bool;\n    bb0: {\n        _2 = Gt(_1, const u32 = 3);\n        switchInt(_2) -> [0: bb1, otherwise: bb2];\n    }\n    bb1: {\n        _0 = const fn(u32) -> u32 {demo::f}(_1) -> [return: bb3, unwind continue];\n    }\n    bb2: {\n        _0 = const fn(u32) -> u32 {demo::g}(_1) -> [return: bb3, unwind continue];\n    }\n    bb3: {\n        return;\n    }\n}\n";

fn ranges() -> Vec<(Option<MirPlace>, Option<MirPlace>)> {
    vec![(Some(at(10, 15)), None), (Some(at(10, 15)), None), (Some(at(20, 24)), None), (Some(at(30, 34)), None), (Some(at(40, 40)), None)]
}

/// The graph of [`TEXT`]: entry, branch, the call of `f`, the call of `g`, return.
fn body() -> FlowGraph {
    graph(
        vec![point("entry", "", Some(at(0, 40))), point("branch", "x > 3", Some(at(10, 15))), point("call", "demo::f", Some(at(20, 24))), point("call", "demo::g", Some(at(30, 34))), point("return", "", Some(at(40, 40)))],
        &[(0, 1, ""), (1, 2, "false"), (1, 3, "true"), (2, 4, "return"), (3, 4, "return")],
    )
}

// rule+ R98.1-1
// rule- R98.1-1
#[test]
fn the_points_pair_through_the_pairs_of_the_alignment() {
    let ranges = ranges();
    let same = vec![true; 5];
    let side = TableSide::new(TEXT, &ranges, &same).unwrap();
    let linked = link(&body(), &side);
    // Each point of one terminator has its terminator; the branch has its condition too.
    assert_eq!(linked.terms, vec![vec![], vec![1], vec![2], vec![3], vec![4]]);
    assert_eq!(linked.elements[1], vec![0, 1]);
    let all: Vec<(usize, usize, bool)> = (0..5).map(|index| (index, index, true)).collect();
    assert_eq!(align([&body(), &body()], [&linked, &linked], &all), vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4)]);
    // The negative cases: two calls with equal texts whose elements the alignment does not pair
    // are a removed point and an added point (no pair by the text, as R60.3-2 did); a pair of
    // elements of two points of two kinds pairs no point.
    let without: Vec<(usize, usize, bool)> = all.iter().copied().filter(|(old, _, _)| *old != 2).collect();
    let pairs = align([&body(), &body()], [&linked, &linked], &without);
    assert!(!pairs.contains(&(2, 2)), "{pairs:?}");
    let found = diff_pairs(&body(), &body(), &pairs);
    assert_eq!((found.removed.clone(), found.added.clone()), (vec![2], vec![2]));
    let crossed: Vec<(usize, usize, bool)> = vec![(1, 2, false), (0, 0, true), (4, 4, true)];
    let pairs = align([&body(), &body()], [&linked, &linked], &crossed);
    assert!(!pairs.iter().any(|(a, _)| *a == 1), "{pairs:?}");
    // The diff never pairs two points of two kinds, also when a pair is given.
    let mut panic = body();
    panic.points[2].kind = "panic".into();
    let found = diff_pairs(&body(), &panic, &[(0, 0), (2, 2)]);
    assert_eq!((found.removed.contains(&2), found.added.contains(&2), found.pairs.contains(&(2, 2))), (true, true, false));
}

// rule+ R98.1-2
// rule- R98.1-2
#[test]
fn an_equal_mir_pairs_by_the_index_and_no_table_pairs_the_entries_only() {
    let old = body();
    let mut new = body();
    new.points[1].text = "level > 3".into();
    // An equal MIR text: the identity; a renamed text is a changed point (R98.6-2).
    assert_eq!(identity(&old, &new), Some((0..5).map(|index| (index, index)).collect()));
    let equal = BodyStatus::Equal { level: "n0".into(), proof: Vec::new() };
    let (found, _, cause) = graph_diff(&old, &new, &[], &equal);
    assert_eq!((found.changed.clone(), found.result(), cause), (vec![(1, 1)], FlowResult::SameStructure, None));
    // A new label of an arrow (a renamed variant) keeps the identity: the arrow is not shared.
    let mut label = body();
    label.arrows[1].2 = "otherwise".into();
    assert!(identity(&old, &label).is_some());
    // The negative cases: two structures with an equal MIR, and a changed body with no table:
    // only the entries pair, and the cause says so.
    let mut other = body();
    other.arrows.push((1, 4, "otherwise".into()));
    assert_eq!(identity(&old, &other), None);
    assert_eq!(entries_only(&old, &other), vec![(0, 0)]);
    // A changed body with no table and one structure pairs by the index too; with two
    // structures, only the entries pair, and the cause says so.
    let (found, _, cause) = graph_diff(&old, &new, &[], &BodyStatus::Different { cfg_changed: false });
    assert_eq!((found.result(), cause), (FlowResult::SameStructure, None));
    let (found, _, cause) = graph_diff(&old, &other, &[], &BodyStatus::Different { cfg_changed: false });
    assert_eq!(found.result(), FlowResult::Changes);
    assert!(cause.is_some_and(|cause| cause.contains("no element table, and the two graphs have two structures (R98.1-2)")));
    // Two tables (two configurations with two texts): the table that fits the two graphs.
    let fits = MirTable { texts: [TEXT.into(), TEXT.into()], ranges: [ranges(), ranges()], pairs: (0..5).map(|index| (index, index, true)).collect(), locals: Vec::new(), decls: [Vec::new(), Vec::new()], same: [vec![true; 5], vec![true; 5]], moved: Vec::new() };
    let mut other_ranges = ranges();
    other_ranges[2] = (Some(at(90, 94)), None);
    other_ranges[3] = (Some(at(95, 99)), None);
    let poor = MirTable { ranges: [other_ranges.clone(), other_ranges], ..fits.clone() };
    let (found, _, cause) = graph_diff(&old, &body(), &[std::sync::Arc::new(poor), std::sync::Arc::new(fits)], &BodyStatus::Different { cfg_changed: false });
    assert_eq!((found.pairs.len(), found.result(), cause), (5, FlowResult::Same, None));
}

// rule+ R98.6-2
// rule- R98.6-2
#[test]
fn a_changed_text_is_a_changed_point_also_with_equal_elements() {
    let table = MirTable { texts: [TEXT.into(), TEXT.into()], ranges: [ranges(), ranges()], pairs: (0..5).map(|index| (index, index, true)).collect(), locals: Vec::new(), decls: [Vec::new(), Vec::new()], same: [vec![true; 5], vec![true; 5]], moved: Vec::new() };
    let old = body();
    let mut new = body();
    new.points[1].text = "level > 3".into();
    let status = BodyStatus::Different { cfg_changed: false };
    // The elements of the branch are equal (a renamed local, or a changed operand value that an
    // element with no point computes): the changed text is a changed point; a same-code entry
    // explains it (R98.6-1).
    let (found, ..) = graph_diff(&old, &new, &[std::sync::Arc::new(table.clone())], &status);
    assert_eq!((found.changed.clone(), found.result()), (vec![(1, 1)], FlowResult::SameStructure));
    // The negative case: an equal text with equal elements is no change.
    let (found, ..) = graph_diff(&old, &body(), &[std::sync::Arc::new(table)], &status);
    assert_eq!((found.changed.clone(), found.result()), (Vec::new(), FlowResult::Same));
}

// rule+ R98.2-2
// rule- R98.2-2
#[test]
fn the_summary_of_an_entry_counts_the_changes_by_kind_with_the_text_of_one_point() {
    let part = |group: u8, label: &str, text: &str| Part { group, label: label.into(), text: text.into() };
    let parts = vec![(part(0, "branch", "x > 0"), false), (part(1, "bounds check", ""), true), (part(3, "arrow", ""), false), (part(3, "arrow", ""), false)];
    assert_eq!(flowmap::summary(&parts), "+1 branch (`x > 0`), −1 bounds check (implicit), +2 arrows");
    // The negative cases: two points of one kind have no text, and a kind with some implicit
    // changes gives their count.
    let parts = vec![(part(0, "branch", "a"), false), (part(0, "branch", "b"), true)];
    assert_eq!(flowmap::summary(&parts), "+2 branches (1 implicit)");
    let parts = vec![(part(2, "changed condition", "`v > 3` → `v >= 3`"), false)];
    assert_eq!(flowmap::summary(&parts), "1 changed condition (`v > 3` → `v >= 3`)");
}

// rule+ R98.8-1
// rule- R98.8-1
#[test]
fn the_graph_part_of_an_entry_has_the_words_of_design_60() {
    let changes = EntryGraph { word: "graph changes".into(), summary: "+1 branch (`x > 0`), +2 arrows".into(), changes: 3, ..Default::default() };
    assert_eq!(changes.line(), "graph: +1 branch (`x > 0`), +2 arrows");
    let same = EntryGraph { word: "same graph (the edit keeps the control flow)".into(), ..Default::default() };
    assert_eq!(same.line(), "graph: same graph (the edit keeps the control flow)");
    let explained = EntryGraph { word: "no change after the check".into(), explained: 12, by: "the check of the extract".into(), ..Default::default() };
    assert_eq!(explained.line(), "graph: no change after the check: the check of the extract explains 12 graph change(s)");
    // The negative case: a structure with changed points says `same structure`, not `graph
    // changes`.
    let structure = EntryGraph { word: "same structure".into(), summary: "1 changed condition".into(), changes: 1, ..Default::default() };
    assert_eq!(structure.line(), "graph: same structure: 1 changed condition");
}

// rule+ R98.5-1
// rule- R98.5-1
#[test]
fn the_coverage_line_has_the_part_of_the_control_flow_changes() {
    let cover = FlowCover { total: 9, entries: 7, implicit: 3, explained: 0, no_edit: 2, list: Vec::new() };
    assert_eq!(cover.line(), "control-flow changes: 7 in entries (3 implicit), 2 in the section of R92.4-1");
    let map = semilla_core::AstMap { nodes: 1, ..Default::default() };
    let flow = FlowReport { cover: Some(cover), ..Default::default() };
    let line = semilla_core::report::map_coverage_line(&map, &[], Some(&flow));
    assert!(line.ends_with("; control-flow changes: 7 in entries (3 implicit), 2 in the section of R92.4-1"), "{line}");
    // The negative case: a step with no graph data has the line of design 90 only.
    assert!(semilla_core::report::map_coverage_line(&map, &[], None).ends_with("not mapped"));
}

/// An entry of the map with a kind and a node.
fn entry(id: &str, kind: EntryKind, nodes: Vec<usize>, affected: &[&str]) -> Fact {
    let positions = if nodes.is_empty() { Vec::new() } else { vec![Position { file: "src/lib.rs".into(), old: false, start: 1, end: 1 }] };
    Fact { id: id.into(), kind: Some(kind), nodes, positions, affected: affected.iter().map(|name| name.to_string()).collect(), ..Default::default() }
}

fn change(text: &str, elements: Vec<(usize, MirPlace)>, implicit: bool) -> crate::flow::Element {
    test_element(text, Anchor { elements, points: Vec::new(), implicit, macro_tokens: Vec::new() }, vec![Part { group: 0, label: "branch".into(), text: text.into() }])
}

/// The outcome of a step with three entries (a real edit, an extract, and the compiled code of
/// the body with no edit at its place) and the graph changes of one body.
fn mapped(elements: Vec<crate::flow::Element>, level: Option<String>) -> (StepOutcome, Vec<Fact>) {
    let report = semilla_core::StepReport { facts: vec![entry("f1", EntryKind::Replace, vec![1], &["demo::f"]), entry("f2", EntryKind::Extract, vec![2], &["demo::f"]), entry("f3", EntryKind::Replace, Vec::new(), &["demo::f"])], flow: Some(FlowReport::default()), ..Default::default() };
    let mut report = report;
    report.node_mir.insert(0, NodeMir { places: [Default::default(), [at(10, 15)].into_iter().collect()], ..Default::default() });
    report.node_mir.insert(1, NodeMir { places: [Default::default(), [at(20, 24)].into_iter().collect()], ..Default::default() });
    let facts = report.facts.clone();
    let mut outcome = verify_empty();
    outcome.report = report;
    outcome.fact_of = vec![Some(0), Some(1)];
    outcome.flow_changes = vec![BodyChanges { name: "demo::f".into(), shown: "demo::f".into(), place: "src/lib.rs:1".into(), edit: true, aligned: true, level, word: "graph changes".into(), elements, ..Default::default() }];
    (outcome, facts)
}

/// An outcome with no data.
fn verify_empty() -> StepOutcome {
    StepOutcome { report: Default::default(), edits: Vec::new(), owners: Vec::new(), fact_of: Vec::new(), renames: Default::default(), bodies: Vec::new(), identity_reason: None, stated_causes: Default::default(), selections: Vec::new(), node_pairs: Default::default(), verdicts: Default::default(), intent_answers: None, explained: Default::default(), not_mapped: Default::default(), templates: Default::default(), transforms: Default::default(), flow_changes: Vec::new() }
}

fn apply(outcome: &mut StepOutcome) {
    let step = Step { index: 0, intent: "test".into(), claims: Vec::new(), tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default(), syntax_only: true, build_check: None, agent: Default::default(), tags: Vec::new(), configurations: None };
    let (before, after) = (Workspace::default(), Workspace::default());
    flowmap::apply(&StepInput { step: &step, before: &before, after: &after }, outcome);
}

// rule+ R98.2-1
// rule- R98.2-1
// rule+ R98.6-1
// rule- R98.6-1
// rule+ R98.9-1
#[test]
fn each_graph_change_goes_to_the_entry_of_the_node_that_owns_its_element() {
    let elements = vec![change("+ branch `a`", vec![(1, at(10, 15))], false), change("+ branch `b`", vec![(1, at(20, 24))], false), change("+ branch `c`", vec![(1, at(50, 55))], false)];
    let (mut outcome, facts) = mapped(elements, None);
    apply(&mut outcome);
    let flow = outcome.report.flow.clone().unwrap();
    // The element of the first change has its owner in f1 (the projection); the second change
    // is in the extract f2, whose check explains it; the third has no owner and no node: the
    // entry of the body in the section of R92.4-1 (implicit).
    assert_eq!(flow.entries.get("f1").map(|graph| (graph.changes, graph.explained)), Some((1, 0)), "{:#?}", flow.entries);
    assert_eq!(flow.entries.get("f2").map(|graph| (graph.changes, graph.explained, graph.word.as_str())), Some((0, 1, "no change after the check")));
    assert_eq!(flow.facts.get("f1"), Some(&vec!["+ branch `a`".to_string()]));
    let cover = flow.cover.clone().unwrap();
    assert_eq!((cover.total, cover.entries, cover.explained, cover.no_edit), (3, 2, 1, 1));
    assert!(cover.list[2].ends_with(": in f3 (R92.4-1), implicit"), "{:?}", cover.list);
    // [R98.9-1] The map changes no fact: each kind, compare result, and text stays.
    assert_eq!(outcome.report.facts, facts);
    // The negative cases: a change never goes to the entry of another node, and the equal MIR
    // of a body explains each of its changes (no entry lists one).
    assert!(!flow.facts.get("f1").unwrap().contains(&"+ branch `b`".to_string()));
    let (mut equal, _) = mapped(vec![change("+ branch `a`", vec![(1, at(10, 15))], false)], Some("n2".into()));
    apply(&mut equal);
    let flow = equal.report.flow.unwrap();
    assert_eq!(flow.entries.get("f1").map(|graph| (graph.changes, graph.explained)), Some((0, 1)));
    assert!(flow.facts.get("f1").is_none());
}

// rule+ R98.4-1
// rule- R98.4-1
#[test]
fn a_change_of_a_body_with_no_edit_goes_to_the_section_of_the_compiled_code_with_no_edit() {
    let (mut outcome, _) = mapped(vec![change("+ branch `a`", vec![(1, at(10, 15))], false)], None);
    outcome.flow_changes[0].edit = false;
    // With no entry of the section for the body: a line of its own.
    outcome.report.facts.retain(|fact| fact.id != "f3");
    apply(&mut outcome);
    let flow = outcome.report.flow.clone().unwrap();
    assert_eq!(flow.no_entry.len(), 1);
    assert!(flow.no_entry[0].line().contains("(no entry: no changed node, R98.4-1)"), "{}", flow.no_entry[0].line());
    assert_eq!(flow.cover.as_ref().map(|cover| (cover.entries, cover.no_edit)), Some((0, 1)));
    // The negative case: with an edit in the body, the change goes to the entry of its node.
    let (mut edited, _) = mapped(vec![change("+ branch `a`", vec![(1, at(10, 15))], false)], None);
    apply(&mut edited);
    let flow = edited.report.flow.unwrap();
    assert!(flow.no_entry.is_empty());
    assert_eq!(flow.cover.map(|cover| (cover.entries, cover.no_edit)), Some((1, 0)));
}

// rule+ R98.3-1
// rule- R98.3-1
#[test]
fn a_change_with_no_owner_in_an_aligned_body_is_implicit() {
    let (mut outcome, _) = mapped(vec![change("+ check `bounds: v[i]`", vec![(1, at(10, 15))], true), change("+ branch `c`", vec![(1, at(50, 55))], false)], None);
    apply(&mut outcome);
    let cover = outcome.report.flow.unwrap().cover.unwrap();
    // A check is implicit, also when its element has an owner; a point whose element has no
    // owner is implicit too.
    assert!(cover.list[0].ends_with(": in f1, implicit"), "{:?}", cover.list);
    assert!(cover.list[1].ends_with(", implicit"), "{:?}", cover.list);
    // The negative case: a branch whose element has an owner is not implicit.
    let (mut plain, _) = mapped(vec![change("+ branch `a`", vec![(1, at(10, 15))], false)], None);
    apply(&mut plain);
    assert!(plain.report.flow.unwrap().cover.unwrap().list[0].ends_with(": in f1"));
}
