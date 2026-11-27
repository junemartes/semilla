//! Tests of design 90: the map of the diff (batch 15a).

use semilla_core::{AgentFacts, AstEntry, AstMap, AstState, Category, EntryKind, Fact, IntentFile, NodeMir, Step, StepReport};
use semilla_syntax::{EditKind, Workspace};

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

/// The report of a step of the syntax-only check with the statements of the agent.
fn run(before: &str, after: &str, agent: AgentFacts) -> StepReport {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(agent);
    verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &NoOracle, &NoOracle, true).report
}

/// The edits of two texts.
fn edits(before: &str, after: &str) -> (Vec<semilla_syntax::Edit>, [Workspace; 2], [tempfile::TempDir; 2]) {
    let (d1, old) = load(before);
    let (d2, new) = load(after);
    let edits = semilla_syntax::diff::diff(&old, &new, &Default::default());
    (edits, [old, new], [d1, d2])
}

const LITERAL: (&str, &str) = ("pub fn total(a: u32) -> u32 {\n    a * 2\n}\n", "pub fn total(a: u32) -> u32 {\n    a * 3\n}\n");

// rule+ R90.6-1
// rule- R90.6-1
// rule+ R90.2-2
#[test]
fn a_node_with_no_unique_alignment_is_not_mapped_and_each_other_node_has_its_kind() {
    let (edits, trees, _dirs) = edits(LITERAL.0, LITERAL.1);
    let edit = edits.iter().find(|edit| matches!(edit.kind, EditKind::Replaced | EditKind::Modified)).expect("a changed literal");
    let trees = [&trees[0], &trees[1]];
    let doubt = NodeMir { doubt: true, ..Default::default() };
    assert_eq!(diffmap::node_kind(edit, Some(&doubt), None, trees).kind, EntryKind::NotMapped);
    let equal = NodeMir { equal: true, ..Default::default() };
    assert_eq!(diffmap::node_kind(edit, Some(&equal), None, trees).kind, EntryKind::SameCode);
    let changed = NodeMir { paired: [1, 1], ..Default::default() };
    let found = diffmap::node_kind(edit, Some(&changed), None, trees);
    assert_eq!((found.kind, found.syntax_only, found.rule), (EntryKind::Replace, false, diffmap::RULE_REPLACE));
    // A node with no MIR evidence keeps the matcher, with the mark "syntax only".
    let found = diffmap::node_kind(edit, None, None, trees);
    assert!(found.syntax_only && found.rule == diffmap::RULE_SYNTAX, "{found:?}");
}

// rule+ R90.2-3
#[test]
fn an_added_statement_with_new_elements_is_an_add_also_with_an_element_of_its_enclosing_node() {
    let (edits, trees, _dirs) = edits("pub fn total(v: &mut Vec<u32>) {\n    v.push(1);\n}\n", "pub fn total(v: &mut Vec<u32>) {\n    v.push(1);\n    v.push(2);\n}\n");
    let edit = edits.iter().find(|edit| edit.kind == EditKind::Inserted).expect("an added statement");
    let trees = [&trees[0], &trees[1]];
    let mir = NodeMir { paired: [1, 1], alone: [0, 3], ..Default::default() };
    assert_eq!(diffmap::node_kind(edit, Some(&mir), None, trees).kind, EntryKind::Add);
    // Only a changed element: the added node replaces old code.
    let mir = NodeMir { paired: [1, 1], ..Default::default() };
    assert_eq!(diffmap::node_kind(edit, Some(&mir), None, trees).kind, EntryKind::Replace);
}

fn fact(id: &str, kind: Option<EntryKind>, nodes: Vec<usize>, item: &str) -> Fact {
    Fact { id: id.into(), kind, nodes, check: if kind.is_some() { "R90.2-1, equal MIR".into() } else { String::new() }, at: Some(semilla_core::Anchor::item(item)), description: format!("the fact {id}"), category: Category::Semantics, ..Default::default() }
}

fn map(count: usize, entries: &[&str]) -> AstMap {
    let entry = |id: &&str| AstEntry { change: "changed".into(), node: "expression".into(), at: "demo::f".into(), place: "src/lib.rs:1".into(), state: AstState::Semilla, by: String::new(), rule: "R90.2-1".into(), before: String::new(), after: String::new(), template: String::new(), kind: Some(EntryKind::SameCode), syntax_only: false, entry: id.to_string() };
    AstMap { nodes: count, entries: entries.iter().map(entry).collect(), ..Default::default() }
}

// rule+ R90.7-1 R90.7-2
// rule- R90.7-1 R90.7-2
#[test]
fn the_properties_of_the_map_find_a_node_in_two_entries_and_an_entry_with_no_check() {
    let facts = vec![fact("f1", Some(EntryKind::SameCode), vec![1], "demo::f"), fact("f2", Some(EntryKind::Replace), vec![2], "demo::g")];
    assert!(diffmap::problems(&map(2, &["f1", "f2"]), &facts).is_empty());
    let twice = vec![fact("f1", Some(EntryKind::SameCode), vec![1, 2], "demo::f"), fact("f2", Some(EntryKind::Replace), vec![2], "demo::g")];
    assert!(diffmap::problems(&map(2, &["f1", "f2"]), &twice).iter().any(|problem| problem.contains("n2 is in two entries")));
    let unchecked = vec![fact("f1", None, vec![1], "demo::f"), fact("f2", Some(EntryKind::Replace), vec![2], "demo::g")];
    assert!(diffmap::problems(&map(2, &["f1", "f2"]), &unchecked).iter().any(|problem| problem.contains("f1 has no kind or no check")));
}

// rule+ R90.5-1 R90.5-3
// rule- R90.5-1 R90.5-3
#[test]
fn the_one_list_has_the_real_edits_first_grouped_by_item_and_the_coverage_line() {
    let mut real = fact("f1", Some(EntryKind::Replace), vec![2], "demo::g");
    real.check = "R90.2-2, 1 changed MIR element".into();
    real.not_stated = true;
    let same = fact("f2", Some(EntryKind::SameCode), vec![1], "demo::f");
    let report = StepReport { step: 0, intent: "test".into(), facts: vec![same.clone(), real.clone()], ast: Some(map(2, &["f2", "f1"])), ..Default::default() };
    let text = views::text_report(std::slice::from_ref(&report));
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("no `{needle}` in:\n{text}"));
    assert!(at("changed nodes: 2 in 2 entries; 1 real edits (0 stated by the agent); 0 not mapped") < at("  real edits:"), "{text}");
    assert!(at("  real edits:") < at("  item `demo::g`:") && at("  item `demo::g`:") < at("◆ f1 replace (Semilla; R90.2-2, 1 changed MIR element): the fact f1") && at("◆ f1 replace") < at("  transformations:") && at("  transformations:") < at("◆ f2 same code (Semilla; R90.2-1, equal MIR)"), "{text}");
    // [R92.5-1] The line below the entry has the mark and the statement that states it.
    assert!(text.contains("\n      not stated; to state it: --change 'demo::g=TEXT'\n"), "{text}");
    // A record of an earlier version (no kind) keeps the groups and the line of today.
    let mut old_map = map(1, &["f1"]);
    old_map.entries[0].kind = None;
    let old = StepReport { facts: vec![fact("f1", None, Vec::new(), "demo::f")], ast: Some(old_map), ..report };
    let text = views::text_report(std::slice::from_ref(&old));
    assert!(text.contains("other facts, filled in by Semilla:") && !text.contains("real edits:") && !text.contains("changed nodes:"), "{text}");
}

// rule+ R90.4-1
// rule- R90.4-1
#[test]
fn each_option_is_a_short_form_of_an_intent_statement_and_reads_on_its_path() {
    for (kind, text, statement) in [
        ("same", "rename limit cap", "#[rename(before::limit, after::cap)]"),
        ("same", "move demo::a::f demo::b::f", "#[moved(before::demo::a::f, after::demo::b::f)]"),
        ("same", "rewrite .inner => .as_raw()", "#[rewrite(\".inner\", \".as_raw()\")]"),
        ("change", "demo::f,demo::g=the fee grows", "#[change(demo::f, demo::g, reason = \"the fee grows\")]\nfn statement() -> bool { true }"),
    ] {
        let made = intent::option_statement(kind, text).unwrap();
        assert!(made.starts_with(statement), "{made}");
        assert_eq!(intent::read_option(kind, text), (text.to_string(), true), "{text}");
    }
    // A path that the parser of the intent language does not read keeps the text of the option
    // (its check and its error message stay).
    assert_eq!(intent::read_option("same", "rename <A as B>::f g"), ("rename <A as B>::f g".to_string(), false));
    assert_eq!(intent::option_statement("same", "inline a b"), None);
}

/// The facts of the agent and the entries of a report, with no number of a fact.
fn agent_view(report: &StepReport) -> Vec<String> {
    let mut out: Vec<String> = report.agent_facts.iter().map(|fact| format!("{} {} {} {}", fact.kind, fact.text, fact.holds, fact.result)).collect();
    out.extend(report.facts.iter().map(|fact| format!("{:?} {:?} {} {}", fact.kind, fact.source, fact.not_stated, fact.description)));
    out
}

// rule+ R90.4-1 R90.3-1 R90.5-2
// rule- R90.3-1 R90.5-2
#[test]
fn an_option_and_its_intent_statement_give_the_same_entry_and_the_same_result() {
    let renamed = ("pub fn limit() -> u32 {\n    10\n}\npub fn run() -> u32 {\n    limit()\n}\n", "pub fn cap() -> u32 {\n    10\n}\npub fn run() -> u32 {\n    cap()\n}\n");
    for (before, after, kind, option) in [(renamed.0, renamed.1, "same", "rename limit cap"), (LITERAL.0, LITERAL.1, "change", "demo::total=the factor grows")] {
        let statement = intent::option_statement(kind, option).unwrap();
        let by_option = if kind == "same" { AgentFacts { same: vec![option.into()], ..Default::default() } } else { AgentFacts { change: vec![option.into()], ..Default::default() } };
        let by_file = AgentFacts { intent: vec![IntentFile { path: "intent.rs".into(), text: statement }], ..Default::default() };
        let (a, b) = (run(before, after, by_option), run(before, after, by_file));
        assert_eq!(agent_view(&a), agent_view(&b), "{option}");
        assert!(a.agent_facts.iter().all(|fact| fact.holds), "{:#?}", a.agent_facts);
        // The statement is the source of its entries.
        assert!(a.facts.iter().any(|fact| fact.source == ["a1"] && !fact.not_stated), "{:#?}", a.facts);
    }
    // With no statement, Semilla is the source. (The syntax-only check has no MIR: the node is
    // syntax only, so it is no real edit and has no mark; the case `map90/edit_literal_negative`
    // has the mark.)
    let plain = run(LITERAL.0, LITERAL.1, AgentFacts::default());
    assert!(plain.facts.iter().all(|fact| fact.source.is_empty() && fact.syntax_only), "{:#?}", plain.facts);
}

// rule+ R90.8-1
// rule- R90.8-1
#[test]
fn the_pairs_come_from_the_hints_then_from_the_one_map_of_the_matcher() {
    // Two twins and the use of one of them that follows a pair: the anchor decides the map,
    // checked one time (two equal twins with no anchor are a tie: R92.1-1).
    let twins = "pub fn one(a: u32) -> u32 {\n    a + 1\n}\npub fn two(a: u32) -> u32 {\n    a + 1\n}\npub fn run() -> u32 {\n    one(1)\n}\n";
    let report = run(twins, &twins.replace("fn one", "fn first").replace("fn two", "fn second").replace("one(1)", "first(1)"), AgentFacts::default());
    let texts: Vec<&str> = report.facts.iter().map(|fact| fact.description.as_str()).collect();
    assert!(texts.contains(&"renamed `demo::one` → `demo::first`") && texts.contains(&"renamed `demo::two` → `demo::second`"), "{texts:#?}");
    // A hint wins over the matcher: the agent pairs the twins the other way.
    let hinted = AgentFacts { same: vec!["rename one second".into(), "rename two first".into()], ..Default::default() };
    let report = run(twins, &twins.replace("fn one", "fn first").replace("fn two", "fn second").replace("one(1)", "second(1)"), hinted);
    let texts: Vec<&str> = report.facts.iter().map(|fact| fact.description.as_str()).collect();
    assert!(texts.contains(&"renamed `demo::one` → `demo::second`") && texts.contains(&"renamed `demo::two` → `demo::first`"), "{texts:#?}");
    // Two texts that are not similar are no pair: a delete and an add.
    let report = run("pub fn one(a: u32) -> u32 {\n    a + 1\n}\n", "pub fn other(v: &[u8]) -> usize {\n    v.iter().filter(|b| **b > 3).count()\n}\n", AgentFacts::default());
    assert!(report.facts.iter().all(|fact| fact.category != Category::Rename), "{:#?}", report.facts);
    assert!(report.facts.iter().any(|fact| fact.description.starts_with("removed `demo::one`")) && report.facts.iter().any(|fact| fact.description.starts_with("added `demo::other`")), "{:#?}", report.facts);
}

// rule+ R90.8-2 R90.8-3
// rule- R90.8-2
#[test]
fn a_hint_that_does_not_pass_stays_a_delete_and_an_add_and_the_other_pairs_keep_their_check() {
    // The agent states a rename with no evidence: it does not pass, and its items stay removed
    // and added. The pair of the matcher in the same check passes.
    let before = "pub fn one(a: u32) -> u32 {\n    a + 1\n}\npub fn alpha(v: &[u8]) -> usize {\n    v.len()\n}\n";
    let after = "pub fn first(a: u32) -> u32 {\n    a + 1\n}\npub fn beta(x: u64, y: u64) -> u64 {\n    x.wrapping_mul(y) ^ 7\n}\n";
    let hinted = AgentFacts { same: vec!["rename alpha beta".into()], ..Default::default() };
    let report = run(before, after, hinted);
    assert!(report.agent_facts.iter().any(|fact| !fact.holds), "{:#?}", report.agent_facts);
    let texts: Vec<&str> = report.facts.iter().map(|fact| fact.description.as_str()).collect();
    assert!(texts.contains(&"renamed `demo::one` → `demo::first`"), "{texts:#?}");
    assert!(texts.iter().any(|text| text.starts_with("removed `demo::alpha`")) && texts.iter().any(|text| text.starts_with("added `demo::beta`")), "{texts:#?}");
    // With no hint, the pair of the matcher alone passes too.
    let report = run(before, after, AgentFacts::default());
    assert!(report.facts.iter().any(|fact| fact.description == "renamed `demo::one` → `demo::first`"), "{:#?}", report.facts);
}


// rule+ R90.2-5
// rule- R90.2-5
#[test]
fn a_node_of_a_rename_fact_is_a_rename_and_a_renamed_local_is_not() {
    let (edits, trees, _dirs) = edits(LITERAL.0, LITERAL.1);
    let edit = &edits[0];
    let trees = [&trees[0], &trees[1]];
    let renamed = Fact { category: Category::Rename, part: "rename".into(), ..Default::default() };
    let found = diffmap::node_kind(edit, None, Some(&renamed), trees);
    assert_eq!((found.kind, found.rule), (EntryKind::Rename, diffmap::RULE_PAIR));
    let moved = Fact { category: Category::Move, part: "move".into(), ..Default::default() };
    assert_eq!(diffmap::node_kind(edit, None, Some(&moved), trees).kind, EntryKind::Move);
    // A renamed local is no definition: the MIR gives its kind.
    let local = Fact { category: Category::Rename, part: "body".into(), ..Default::default() };
    let equal = NodeMir { equal: true, ..Default::default() };
    assert_eq!(diffmap::node_kind(edit, Some(&equal), Some(&local), trees).kind, EntryKind::SameCode);
}
