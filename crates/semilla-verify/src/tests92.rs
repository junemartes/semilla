//! Tests of design 92: the map of the diff, the defects and the open questions of batch 15a.

use std::cell::Cell;

use semilla_core::report::{ENTRY_SECTIONS, NO_EDIT_SECTION};
use semilla_core::{AgentFacts, AstState, Category, EntryKind, Fact, Gap, NodeMir, Position, Step, StepReport};
use semilla_syntax::Workspace;

use super::*;

const MANIFEST: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";

/// Loads a crate `demo` with one file `src/lib.rs`.
fn load(text: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), MANIFEST).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), text).unwrap();
    let ws = Workspace::load(dir.path()).unwrap();
    (dir, ws)
}

fn step(agent: AgentFacts) -> Step {
    Step { index: 0, intent: "test".into(), claims: Vec::new(), tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default(), syntax_only: true, build_check: None, agent, tags: Vec::new(), configurations: None }
}

/// A body oracle with no body that counts the runs of the body compare: one for each run of the
/// core check of a step.
#[derive(Default)]
struct Counting(Cell<usize>);

impl BodyOracle for Counting {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        self.0.set(self.0.get() + 1);
        Ok(Vec::new())
    }
}

/// The outcome of a step of the syntax-only check, the two trees, and the count of the core
/// checks.
fn run(before: &str, after: &str, agent: AgentFacts) -> (StepOutcome, [Workspace; 2], usize, [tempfile::TempDir; 2]) {
    let (d1, old) = load(before);
    let (d2, new) = load(after);
    let step = step(agent);
    let counting = Counting::default();
    let outcome = verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &counting, &NoOracle, true);
    let runs = counting.0.get();
    (outcome, [old, new], runs, [d1, d2])
}

fn texts(report: &StepReport) -> Vec<String> {
    report.facts.iter().map(|fact| fact.description.clone()).collect()
}

// rule+ R92.1-1
// rule- R92.1-1
#[test]
fn a_tie_of_the_one_map_gives_no_pair_and_a_use_that_follows_a_pair_decides_it() {
    let old = "pub fn one(x: u32) -> u32 {\n    x + 1\n}\n";
    let new = "pub fn uno(x: u32) -> u32 {\n    x + 1\n}\npub fn eins(x: u32) -> u32 {\n    x + 1\n}\n";
    let (tie, ..) = run(old, new, AgentFacts::default());
    assert!(!texts(&tie.report).iter().any(|text| text.starts_with("renamed ")), "{:#?}", texts(&tie.report));
    assert!(tie.report.notes.iter().any(|note| note.starts_with("`demo::one` → `demo::eins`, `demo::one` → `demo::uno` show(s) as removed and added: a tie of the one map")), "{:#?}", tie.report.notes);
    // The source order never decides: the other order of the two new items gives no pair too.
    let (other, ..) = run(old, "pub fn eins(x: u32) -> u32 {\n    x + 1\n}\npub fn uno(x: u32) -> u32 {\n    x + 1\n}\n", AgentFacts::default());
    assert!(!texts(&other.report).iter().any(|text| text.starts_with("renamed ")), "{:#?}", texts(&other.report));
    // Negative: a use that follows one pair is an anchor: one pair, no tie.
    let used = |text: &str, name: &str| format!("{text}pub fn two(x: u32) -> u32 {{\n    {name}(x) * 2\n}}\n");
    let (anchored, ..) = run(&used(old, "one"), &used(new, "uno"), AgentFacts::default());
    assert!(texts(&anchored.report).contains(&"renamed `demo::one` → `demo::uno`".to_string()), "{:#?}", texts(&anchored.report));
    assert!(!anchored.report.notes.iter().any(|note| note.contains("a tie of the one map")), "{:#?}", anchored.report.notes);
}

// rule+ R92.2-1
// rule- R92.2-1
#[test]
fn the_pairs_come_before_the_check_of_the_step_and_a_step_whose_pairs_pass_runs_one_core_check() {
    let old = "pub fn keep(x: u32) -> u32 {\n    x * 3\n}\npub fn run() -> u32 {\n    keep(2)\n}\n";
    let new = old.replace("keep", "hold");
    let (outcome, _, runs, _) = run(old, &new, AgentFacts::default());
    assert!(texts(&outcome.report).contains(&"renamed `demo::keep` → `demo::hold`".to_string()), "{:#?}", texts(&outcome.report));
    // Design 90 ran the check of the step, then the check with the pairs: two runs.
    assert_eq!(runs, 1);
    // Negative: a hint that does not pass (no evidence) gives one more run with no pair.
    let other = "pub fn alpha(v: &[u8]) -> usize {\n    v.len()\n}\n";
    let changed = "pub fn beta(x: u64, y: u64) -> u64 {\n    x.wrapping_mul(y) ^ 7\n}\n";
    let (hinted, _, runs, _) = run(other, changed, AgentFacts { same: vec!["rename alpha beta".into()], ..Default::default() });
    assert!(hinted.report.agent_facts.iter().any(|fact| !fact.holds), "{:#?}", hinted.report.agent_facts);
    assert_eq!(runs, 2);
}

const MOVES: (&str, &str) = (
    "pub mod a {\n    pub fn far(x: u32) -> u32 {\n        x + 7\n    }\n}\npub fn keep(x: u32) -> u32 {\n    x * 3\n}\n",
    "pub mod a {}\n#[cfg(not(test))]\npub mod b {\n    pub fn far(x: u32) -> u32 {\n        x + 7\n    }\n}\npub fn hold(x: u32) -> u32 {\n    x * 3\n}\n",
);

// rule+ R92.2-2
// rule- R92.2-2
#[test]
fn a_pair_that_does_not_pass_its_check_changes_no_other_pair() {
    let (outcome, ..) = run(MOVES.0, MOVES.1, AgentFacts::default());
    let texts = texts(&outcome.report);
    // The rename passes: the move that changes the `cfg` conditions on its path does not stop it.
    assert!(texts.contains(&"renamed `demo::keep` → `demo::hold`".to_string()), "{texts:#?}");
    // Negative: the move does not pass: removed and added, the cause, and the template.
    assert!(!texts.iter().any(|text| text.starts_with("moved ")), "{texts:#?}");
    assert!(outcome.report.notes.iter().any(|note| note.starts_with("`demo::a::far` → `demo::b::far` show(s) as removed and added: Semilla does not derive the move (the move of `demo::a::far` changes the `cfg` conditions on its path")), "{:#?}", outcome.report.notes);
    assert!(outcome.report.notes.iter().any(|note| note.starts_with("the hint of the pair `demo::a::far` → `demo::b::far`: ")), "{:#?}", outcome.report.notes);
    // With the same conditions on the path, the move passes too.
    let (same, ..) = run(MOVES.0, &MOVES.1.replace("#[cfg(not(test))]\n", ""), AgentFacts::default());
    assert!(texts_of(&same.report, "moved `demo::a::far` → `demo::b::far`"), "{:#?}", same.report.facts);
}

fn texts_of(report: &StepReport, start: &str) -> bool {
    report.facts.iter().any(|fact| fact.description.starts_with(start))
}

// rule+ R92.3-1
// rule- R92.3-1
#[test]
fn a_syntax_only_node_with_no_partner_is_an_addition_with_who_reaches_it() {
    let old = "pub fn f(x: u32) -> u32 {\n    x + 1\n}\n";
    let new = "pub fn f(x: u32) -> u32 {\n    helper(x) + 1\n}\nfn helper(x: u32) -> u32 {\n    x\n}\n";
    let (outcome, ..) = run(old, new, AgentFacts::default());
    let map = outcome.report.ast.clone().unwrap();
    let added = map.entries.iter().find(|entry| entry.at == "demo::helper").unwrap_or_else(|| panic!("{:#?}", map.entries));
    assert_eq!((added.state, added.rule.as_str(), added.kind, added.syntax_only), (AstState::Semilla, "R92.3-1", Some(EntryKind::Add), true), "{added:?}");
    assert!(added.by.contains("reached from `demo::f`"), "{added:?}");
    let fact = outcome.report.facts.iter().find(|fact| fact.description.starts_with("added `demo::helper`")).unwrap();
    assert!(fact.kind == Some(EntryKind::Add) && fact.check.starts_with("R92.3-1, no MIR element and no partner; reached from `demo::f`"), "{fact:#?}");
    assert_eq!(map.count(AstState::NotMapped), 0);
    // Negative: a changed node with no MIR element keeps the kind of its edit (R90.2-6).
    let changed = map.entries.iter().find(|entry| entry.change == "changed").unwrap_or_else(|| panic!("{:#?}", map.entries));
    assert_eq!((changed.rule.as_str(), changed.kind), ("R79.3-1", Some(EntryKind::Replace)), "{changed:?}");
}

/// A fact of the one list with a kind, nodes, and lines.
fn entry(id: &str, kind: EntryKind, nodes: Vec<usize>, positions: Vec<Position>) -> Fact {
    Fact { id: id.into(), at: Some(semilla_core::Anchor::item("demo::f")), kind: Some(kind), nodes, positions, check: "R90.2-2, a check".into(), category: Category::Semantics, ..Default::default() }
}

fn line(file: &str) -> Position {
    Position { file: file.into(), old: false, start: 1, end: 1 }
}

// rule+ R92.4-1
// rule- R92.4-1
#[test]
fn an_entry_with_no_node_and_no_changed_line_is_in_its_own_section_and_no_real_edit() {
    let compiled = Fact { part: "indirect change".into(), ..entry("f2", EntryKind::Replace, Vec::new(), Vec::new()) };
    assert!(compiled.no_edit() && !compiled.counts_as_real());
    assert_eq!(compiled.section().map(|at| ENTRY_SECTIONS[at]), Some(NO_EDIT_SECTION));
    let api = Fact { part: "public api".into(), category: Category::Rename, ..entry("f3", EntryKind::Rename, Vec::new(), Vec::new()) };
    assert_eq!(api.section().map(|at| ENTRY_SECTIONS[at]), Some(NO_EDIT_SECTION));
    // Negative: a node, a changed line, a file edit, and the fact of a pair are no such entry.
    for fact in [
        entry("f1", EntryKind::Replace, vec![1], Vec::new()),
        entry("f4", EntryKind::Replace, Vec::new(), vec![line("src/lib.rs")]),
        Fact { at: None, file: Some("notes.txt".into()), ..entry("f5", EntryKind::Replace, Vec::new(), Vec::new()) },
    ] {
        assert!(!fact.no_edit() && fact.counts_as_real(), "{fact:#?}");
        assert_eq!(fact.section(), Some(0));
    }
    let pair = Fact { part: "identity".into(), ..entry("f6", EntryKind::Rename, Vec::new(), Vec::new()) };
    assert_eq!(pair.section().map(|at| ENTRY_SECTIONS[at]), Some("transformations"));
    // The coverage line counts no such entry as a real edit.
    let map = semilla_core::AstMap { nodes: 1, ..Default::default() };
    let line = semilla_core::report::map_coverage_line(&map, &[entry("f1", EntryKind::Replace, vec![1], Vec::new()), compiled]);
    assert_eq!(line, "changed nodes: 1 in 2 entries; 1 real edits (1 stated by the agent); 0 not mapped");
}

// rule+ R92.5-1
// rule- R92.5-1
#[test]
fn an_entry_that_no_statement_names_has_a_line_with_the_statement_that_states_it() {
    let mut open = entry("f1", EntryKind::Replace, vec![1], vec![line("src/lib.rs")]);
    open.not_stated = true;
    let mut grouped = entry("f2", EntryKind::Replace, vec![2], Vec::new());
    grouped.not_stated = true;
    let gap = Gap { text: "`$a.total` => `$a.total()`".into(), places: vec!["demo::a/tail".into(), "demo::b/tail".into()], items: vec!["demo::a".into(), "demo::b".into()], facts: vec!["f2".into()], option: "--same 'rewrite $a.total => $a.total()'".into(), ..Default::default() };
    let report = StepReport { facts: vec![open.clone(), grouped.clone()], gaps: vec![gap], ..Default::default() };
    assert_eq!(report.not_stated_line(&open).as_deref(), Some("not stated (src/lib.rs:1); to state it: --change 'demo::f=TEXT'"));
    assert_eq!(report.not_stated_line(&grouped).as_deref(), Some("not stated; to state it: --same 'rewrite $a.total => $a.total()' (one statement for 2 places: f2)"));
    // Negative: a stated entry, and an entry of an earlier version (no kind), have no line.
    let stated = entry("f3", EntryKind::Replace, vec![3], Vec::new());
    assert_eq!(report.not_stated_line(&stated), None);
    let old = Fact { kind: None, not_stated: true, ..open };
    assert_eq!(report.not_stated_line(&old), None);
}

/// The outcome of a syntax-only step with the MIR evidence of its nodes, and the facts after
/// the shape of the entries.
fn shaped(before: &str, after: &str, evidence: &dyn Fn(usize) -> NodeMir) -> StepOutcome {
    let (mut outcome, trees, _, _dirs) = run(before, after, AgentFacts::default());
    let indices: Vec<usize> = outcome.edits.iter().enumerate().filter(|(_, edit)| astmap::is_node(edit)).map(|(index, _)| index).collect();
    for (place, index) in indices.iter().enumerate() {
        outcome.report.node_mir.insert(*index, evidence(place));
    }
    let mut map = outcome.report.ast.clone().unwrap();
    diffmap::shape(&mut outcome, [&trees[0], &trees[1]], &mut map, &indices);
    outcome.report.ast = Some(map);
    outcome
}

// rule+ R92.6-1
// rule- R92.6-1
#[test]
fn two_nodes_that_own_one_changed_element_are_one_entry() {
    let old = "pub fn total(a: u32, b: u32) -> u32 {\n    let x = a * 2;\n    x + b\n}\n";
    let new = "pub fn total(a: u32, b: u32) -> u32 {\n    let x = b * 3;\n    x + b\n}\n";
    let (plain, ..) = run(old, new, AgentFacts::default());
    let edits: Vec<usize> = plain.edits.iter().enumerate().filter(|(_, edit)| astmap::is_node(edit)).map(|(index, _)| index).collect();
    assert_eq!((edits.len(), plain.report.facts.len()), (2, 2), "{:#?}", plain.report.facts);
    let paired = NodeMir { paired: [1, 1], ..Default::default() };
    let one = shaped(old, new, &|place| NodeMir { shared: [edits[1 - place]].into_iter().collect(), ..paired.clone() });
    assert_eq!(one.report.facts.len(), 1, "{:#?}", one.report.facts);
    let fact = &one.report.facts[0];
    assert!(fact.description.contains("`a` → `b`") && fact.description.contains("`2` → `3`") && fact.check.contains("R92.6-1"), "{fact:#?}");
    assert!(one.fact_of.iter().flatten().all(|at| *at == 0));
    // Negative: two nodes with their own elements stay two entries.
    let two = shaped(old, new, &|_| paired.clone());
    assert_eq!(two.report.facts.len(), 2, "{:#?}", two.report.facts);
}

// rule+ R92.7-1
// rule- R92.7-1
#[test]
fn an_entry_with_nodes_of_two_kinds_splits_into_one_entry_for_each_kind() {
    let old = "pub fn a(x: u32) -> u32 {\n    x + 2\n}\npub fn b(x: u32) -> u32 {\n    x + 2\n}\n";
    let new = old.replace("x + 2", "x + 3");
    let (plain, ..) = run(old, &new, AgentFacts::default());
    assert_eq!(plain.report.facts.len(), 1, "one fold fact: {:#?}", plain.report.facts);
    // The first node changes the MIR, the second does not: two kinds.
    let split = shaped(old, &new, &|place| if place == 0 { NodeMir { paired: [1, 1], ..Default::default() } } else { NodeMir { equal: true, ..Default::default() } });
    assert_eq!(split.report.facts.len(), 2, "{:#?}", split.report.facts);
    let map = split.report.ast.clone().unwrap();
    let kinds: Vec<Option<EntryKind>> = map.entries.iter().map(|entry| entry.kind).collect();
    assert_eq!(kinds, [Some(EntryKind::Replace), Some(EntryKind::SameCode)], "{:#?}", map.entries);
    assert_eq!((map.entries[0].entry.as_str(), map.entries[1].entry.as_str()), ("f1", "f2"));
    assert!(split.report.facts[1].description.contains("demo::b") && split.report.facts[1].check.contains("R92.7-1"), "{:#?}", split.report.facts[1]);
    // Negative: nodes of one kind stay one entry.
    let one = shaped(old, &new, &|_| NodeMir { paired: [1, 1], ..Default::default() });
    assert_eq!(one.report.facts.len(), 1, "{:#?}", one.report.facts);
}
