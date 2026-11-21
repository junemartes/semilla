//! Tests of the verifier with fake oracles.

use std::collections::BTreeMap;

use semilla_core::report::Effect;
use semilla_core::{Category, ChangeSet, Claim, ClaimStatus, Compare, Fact, Group, GroupKind, Role, Step, Subgroup};
use semilla_syntax::Workspace;

use super::*;

const MANIFEST: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";

/// A body oracle that returns a fixed status for each item path.
struct FakeBodies(BTreeMap<String, BodyStatus>);

impl BodyOracle for FakeBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.iter().map(|(name, status)| BodyComparison { name: name.clone(), span: None, status: status.clone(), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }).collect())
    }
}

/// An expression oracle that always returns one effect.
struct FakeExprs(Effect);

impl ExprOracle for FakeExprs {
    fn classify(&self, _: &str, _: &str, _: &BTreeMap<String, String>) -> Option<EffectCheck> {
        Some(EffectCheck { effect: self.0, witness: Some("i = 5, n = 5".into()) })
    }
}

fn load(src: &str) -> (tempfile::TempDir, Workspace) {
    load_with(MANIFEST, src)
}

fn load_with(manifest: &str, src: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), manifest).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), src).unwrap();
    let ws = Workspace::load(dir.path()).unwrap();
    // Design 52.1: each source text of the tests has the parts of each node.
    for item in ws.items.values() {
        assert_eq!(item.body.as_ref().and_then(|body| body.parts_fault(&mut 0)), None, "{}", item.path);
    }
    (dir, ws)
}

fn claim(id: &str, op: &str, at: Option<&str>, args: serde_json::Value) -> Claim {
    Claim { id: id.into(), op: op.into(), at: at.map(|a| a.parse().unwrap()), args: args.as_object().cloned().unwrap_or_default() }
}

fn step(claims: Vec<Claim>) -> Step {
    // The tests of the compare results need a step with the compiled check (design 50).
    Step { index: 0, intent: "test".into(), claims, tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default(), compiled: true, build_check: None }
}

fn run(before: &str, after: &str, claims: Vec<Claim>, bodies: &[(&str, BodyStatus)], effect: Option<Effect>) -> StepReport {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(claims);
    let bodies = FakeBodies(bodies.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
    let outcome = match effect {
        Some(effect) => verify_step(&StepInput { step: &step, before: &old, after: &new }, &bodies, &FakeExprs(effect)),
        None => verify_step(&StepInput { step: &step, before: &old, after: &new }, &bodies, &NoOracle),
    };
    outcome.report
}

/// Runs a step with an oracle that gives MIR facts (phase 2).
fn run_facts(before: &str, after: &str, claims: Vec<Claim>, oracle: &dyn BodyOracle) -> StepReport {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(claims);
    verify_step(&StepInput { step: &step, before: &old, after: &new }, oracle, &NoOracle).report
}

fn equal(level: &str) -> BodyStatus {
    BodyStatus::Equal { level: level.into(), proof: Vec::new() }
}

fn different(cfg: bool) -> BodyStatus {
    BodyStatus::Different { cfg_changed: cfg }
}

fn status_of<'a>(report: &'a StepReport, id: &str) -> &'a ClaimStatus {
    &report.claims.iter().find(|c| c.claim == id).unwrap().status
}

const BEFORE: &str = "pub fn total(v: &[u32], max: u32) -> u32 {\n    let mut s = 0;\n    for x in v {\n        if *x < max {\n            s += x;\n        }\n    }\n    s\n}\n";
const AFTER: &str = "pub fn total(v: &[u32], max: u32) -> u32 {\n    if v.is_empty() {\n        return 0;\n    }\n    let mut s = 0;\n    for x in v {\n        if *x <= max {\n            s += x;\n        }\n    }\n    s\n}\n";

#[test]
fn structured_claims_cover_all_edits() {
    let report = run(
        BEFORE,
        AFTER,
        vec![
            claim("c1", "add-early-exit", Some("demo::total/S0"), serde_json::json!({"cond": "v.is_empty()"})),
            claim("c2", "change-operator", Some("demo::total/S1/body/S0/cond"), serde_json::json!({})),
        ],
        &[("demo::total", different(true))],
        Some(Effect::Widen),
    );
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    assert!(status_of(&report, "c2").is_verified(), "{:?}", status_of(&report, "c2"));
    let c2 = report.claims.iter().find(|c| c.claim == "c2").unwrap();
    assert_eq!(c2.effect, Some(Effect::Widen));
    assert_eq!(c2.category, Some(Category::Semantics));
    assert_eq!(c2.witness.as_deref(), Some("i = 5, n = 5"));
    assert!(report.accepted());
}

#[test]
fn a_missing_claim_is_uncovered_with_a_hint() {
    let report = run(BEFORE, AFTER, vec![claim("c1", "add-early-exit", Some("demo::total/S0"), serde_json::json!({}))], &[], None);
    assert_eq!(report.uncovered.len(), 1, "{:#?}", report.uncovered);
    let uncovered = &report.uncovered[0];
    assert_eq!(uncovered.at.as_ref().unwrap().to_string(), "demo::total/S1/body/S0/cond");
    assert_eq!(uncovered.hint.as_deref(), Some("change-operator"));
    assert!(!report.accepted());
}

#[test]
fn a_wrong_condition_is_refuted() {
    let report = run(BEFORE, AFTER, vec![claim("c1", "add-early-exit", Some("demo::total/S0"), serde_json::json!({"cond": "v.len() == 0"}))], &[], None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Refuted { .. }));
}

#[test]
fn an_empty_claim_fails() {
    let report = run(BEFORE, BEFORE, vec![claim("c1", "change-literal", Some("demo::total/S0/init"), serde_json::json!({}))], &[], None);
    assert_eq!(status_of(&report, "c1"), &ClaimStatus::Empty);
    assert!(!report.accepted());
}

const PLAIN: &str = "pub fn f(a: u32, b: u32, c: u32) -> u32 {\n    a * b + c\n}\n";
const EXTRACTED: &str = "pub fn f(a: u32, b: u32, c: u32) -> u32 {\n    let product = a * b;\n    product + c\n}\n";

#[test]
fn a_claim_of_a_rewrite_checks_the_shape_only() {
    // Design 35: a claim of a rewrite checks the shape of the edit only. No rule asks for an
    // equal body. The category comes from the op: a different control flow graph does not
    // change it (design 40.5).
    let extract = || vec![claim("c1", "extract-variable", Some("demo::f/tail"), serde_json::json!({"name": "product"}))];
    let category = |report: &StepReport| report.claims.iter().find(|result| result.claim == "c1").unwrap().category;
    // An equal result above N1 is not a result of the compare.
    // Design 44 (FIX 4): the claim of an extracted variable is `move`, as the derived fact.
    for (status, expected) in [(equal("n0"), Category::Move), (equal("n2"), Category::Move), (different(false), Category::Move), (different(true), Category::Move)] {
        let report = run(PLAIN, EXTRACTED, extract(), &[("demo::f", status)], None);
        assert_eq!(status_of(&report, "c1"), &ClaimStatus::Verified { proof: "shape".into() });
        assert_eq!(category(&report), Some(expected), "{report:#?}");
        assert!(report.uncovered.is_empty() && report.accepted(), "{report:#?}");
    }
}

#[test]
fn renames_are_predicted() {
    let before = "fn helper(x: u32) -> u32 { x + 1 }\npub fn f(x: u32) -> u32 { let total = helper(x); total * 2 }\n";
    let after = "fn add_one(x: u32) -> u32 { x + 1 }\npub fn f(x: u32) -> u32 { let sum = add_one(x); sum * 2 }\n";
    let bodies = [("demo::add_one", equal("n0")), ("demo::f", equal("n1"))];
    // The rename alone: accepted, and the local rename in the next step.
    let renamed = "fn add_one(x: u32) -> u32 { x + 1 }\npub fn f(x: u32) -> u32 { let total = add_one(x); total * 2 }\n";
    let report = run(before, renamed, vec![claim("c1", "rename", Some("demo::helper"), serde_json::json!({"to": "add_one"}))], &bodies, None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    assert!(report.accepted());
    let report = run(renamed, after, vec![claim("c2", "rename-local", Some("demo::f/S0"), serde_json::json!({"to": "sum"}))], &[("demo::f", equal("n0"))], None);
    assert!(status_of(&report, "c2").is_verified(), "{:?}", status_of(&report, "c2"));
    assert!(report.accepted());
    // Both in one step: an identity step has no other claim (design 22.4).
    let report = run(
        before,
        after,
        vec![claim("c1", "rename", Some("demo::helper"), serde_json::json!({"to": "add_one"})), claim("c2", "rename-local", Some("demo::f/S0"), serde_json::json!({"to": "sum"}))],
        &bodies,
        None,
    );
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Refuted { reason } if reason.contains("the step has a second change")), "{:?}", status_of(&report, "c1"));
    assert!(!report.accepted());
}

#[test]
fn free_edits_are_block_free_and_tight() {
    let before = "pub fn f(a: u32, b: u32) -> u32 {\n    let x = (a + 1) * (b + 2);\n    x\n}\n";
    let after = "pub fn f(a: u32, b: u32) -> u32 {\n    let x = (a + 1) * (b + 3);\n    x\n}\n";
    // On the statement: not allowed.
    let on_statement = run(before, after, vec![claim("c1", "replace-expression", Some("demo::f/S0"), serde_json::json!({}))], &[], None);
    assert!(matches!(status_of(&on_statement, "c1"), ClaimStatus::Malformed { .. }));
    // Too wide: the change is only in `rhs/rhs`.
    let wide = run(before, after, vec![claim("c1", "replace-expression", Some("demo::f/S0/init"), serde_json::json!({}))], &[], None);
    assert!(matches!(status_of(&wide, "c1"), ClaimStatus::Malformed { reason } if reason.contains("too wide")));
    // Tight: accepted.
    let tight = run(before, after, vec![claim("c1", "replace-expression", Some("demo::f/S0/init/rhs"), serde_json::json!({}))], &[("demo::f", different(false))], None);
    assert_eq!(status_of(&tight, "c1"), &ClaimStatus::Verified { proof: "free".into() });
    assert!(tight.accepted());

    // Block-like: a free edit cannot add an `if`.
    let block = run(PLAIN, "pub fn f(a: u32, b: u32, c: u32) -> u32 {\n    if a > 0 { a * b + c } else { c }\n}\n", vec![claim("c1", "replace-expression", Some("demo::f/tail"), serde_json::json!({}))], &[], None);
    assert!(!block.accepted());
}

/// The items of the facts "indirect change, cause not known".
fn unknown_causes(report: &StepReport) -> Vec<String> {
    report.facts.iter().filter(|f| f.part == "indirect change").filter_map(|f| f.at.as_ref().map(|a| a.item.clone())).collect()
}

#[test]
fn an_indirect_change_with_no_cause_is_a_fact_in_the_spotlight() {
    let src = "pub fn f(x: u32) -> u32 { x + 1 }\npub fn g(x: u32) -> u32 { x * 2 }\n";
    let changed = "pub fn f(x: u32) -> u32 { x + 2 }\npub fn g(x: u32) -> u32 { x * 2 }\n";
    let base = || vec![claim("c1", "change-literal", Some("demo::f/tail/rhs"), serde_json::json!({}))];
    let bodies = [("demo::f", different(false)), ("demo::g", different(false))];
    let report = run(src, changed, base(), &bodies, None);
    assert_eq!(unknown_causes(&report), ["demo::g"], "{:#?}", report.facts);
    let fact = report.facts.iter().find(|f| f.part == "indirect change").unwrap();
    assert!(fact.spotlight && fact.compare == Compare::CodeDifferent && fact.affected == ["demo::g"], "{fact:#?}");
    assert!(!report.accepted(), "the spotlight needs a reason");
    // The claim that owns the edit lists its body.
    assert_eq!(report.claims[0].affected, ["demo::f"]);
    // The `affects` argument is retired: the agent cannot take the body.
    let mut claims = base();
    claims[0].args.insert("affects".into(), serde_json::json!(["demo::g"]));
    let report = run(src, changed, claims, &bodies, None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Malformed { reason } if reason.contains("retired")), "{:?}", status_of(&report, "c1"));
}

#[test]
fn the_test_harness_main_is_not_a_body_of_the_user() {
    // Batch 5g, item 1. The MIR driver gives the `main` function of the test harness the name
    // `<crate>::{test-harness-main}`. Its record does not hold the list of the tests.
    let src = "pub fn f(x: u32) -> u32 { x + 1 }\n";
    let harness = "demo::{test-harness-main}";
    assert!(crate::is_test_harness_main(harness));
    // A function `main` of the user, and a path with no crate, are not the harness.
    assert!(!crate::is_test_harness_main("demo::main") && !crate::is_test_harness_main("{test-harness-main}"));
    // A harness on one side only: a test target starts or stops to exist. No fact.
    for status in [BodyStatus::AfterOnly, BodyStatus::BeforeOnly] {
        let report = run(src, src, Vec::new(), &[(harness, status)], None);
        assert!(report.facts.is_empty() && report.accepted(), "{:#?}", report.facts);
    }
    // A harness with a different text: the test runner changed. The spotlight, with its own text.
    let report = run(src, src, Vec::new(), &[(harness, different(false))], None);
    assert_eq!(unknown_causes(&report), [harness], "{:#?}", report.facts);
    let fact = &report.facts[0];
    assert!(fact.spotlight && fact.description.starts_with("the test harness of `demo` changed"), "{fact:#?}");
    assert!(!report.accepted(), "the spotlight needs a reason");
}

#[test]
fn a_main_body_with_no_source_item_is_not_the_test_harness() {
    // Before batch 5g, a changed body `<crate>::main` of a crate with no `main` function in the
    // source had no owner check (a rule by the name only). A macro can generate such a body.
    let src = "pub fn f(x: u32) -> u32 { x + 1 }\n";
    for status in [different(false), BodyStatus::AfterOnly, BodyStatus::BeforeOnly] {
        let report = run(src, src, Vec::new(), &[("demo::main", status)], None);
        assert_eq!(unknown_causes(&report), ["demo::main"], "{:#?}", report.facts);
        assert!(report.facts[0].spotlight && report.facts[0].description.starts_with("code that Semilla does not see"), "{:#?}", report.facts);
        assert!(!report.accepted());
    }
}

#[test]
fn rewrite_pattern_needs_all_sites() {
    let before = "pub struct C;\nimpl C { pub fn total(&self) -> u32 { 1 } }\npub fn a(c: &C) -> u32 { c.total() + 1 }\npub fn b(c: &C) -> u32 { c.total() * 2 }\n";
    let after_all = "pub struct C;\nimpl C { pub fn total(&self) -> u32 { 1 } }\npub fn a(c: &C) -> u32 { c.total().min(9) + 1 }\npub fn b(c: &C) -> u32 { c.total().min(9) * 2 }\n";
    let after_one = "pub struct C;\nimpl C { pub fn total(&self) -> u32 { 1 } }\npub fn a(c: &C) -> u32 { c.total().min(9) + 1 }\npub fn b(c: &C) -> u32 { c.total() * 2 }\n";
    let rule = || vec![claim("c1", "rewrite-pattern", None, serde_json::json!({"from": "$c.total()", "to": "$c.total().min(9)"}))];
    let all = run(before, after_all, rule(), &[], None);
    assert!(status_of(&all, "c1").is_verified(), "{:?}", status_of(&all, "c1"));
    assert!(all.uncovered.is_empty(), "{:#?}", all.uncovered);
    let one = run(before, after_one, rule(), &[], None);
    assert!(matches!(status_of(&one, "c1"), ClaimStatus::Refuted { reason } if reason.contains("demo::b")), "{:?}", status_of(&one, "c1"));
}

#[test]
fn wrap_statements_moves_the_statements() {
    let before = "pub fn f(a: u32) -> u32 {\n    let mut s = 0;\n    s += a;\n    s += 1;\n    s\n}\n";
    let after = "pub fn f(a: u32) -> u32 {\n    let mut s = 0;\n    if a > 0 {\n        s += a;\n        s += 1;\n    }\n    s\n}\n";
    let report = run(before, after, vec![claim("c1", "wrap-statements", Some("demo::f/S1"), serde_json::json!({"count": 2, "with": "if"}))], &[("demo::f", different(true))], None);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
}

#[test]
fn unknown_ops_and_missing_args_are_malformed() {
    let report = run(BEFORE, AFTER, vec![claim("c1", "edit-fn-body", Some("demo::total"), serde_json::json!({})), claim("c2", "rename", Some("demo::total"), serde_json::json!({}))], &[], None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Malformed { .. }));
    assert!(matches!(status_of(&report, "c2"), ClaimStatus::Malformed { reason } if reason.contains("`to`")));
}

#[test]
fn group_and_subgroup_rules() {
    let (_d1, old) = load(BEFORE);
    let (_d2, new) = load(AFTER);
    let claims = vec![
        claim("c1", "add-early-exit", Some("demo::total/S0"), serde_json::json!({})),
        claim("c2", "change-operator", Some("demo::total/S1/body/S0/cond"), serde_json::json!({})),
    ];
    let set = ChangeSet {
        base_tree: "a".into(),
        head_tree: "b".into(),
        steps: vec![step(claims)],
        groups: vec![Group { id: "G1".into(), title: "t".into(), kind: GroupKind::Refactor, why: String::new(), steps: vec![0] }],
        subgroups: vec![Subgroup { id: "G1:m".into(), new_claims: Vec::new(), group: "G1".into(), role: Role::Mechanical, title: "m".into(), why: String::new(), claims: vec!["c1".into(), "c2".into()], edited: true }],
        ..Default::default()
    };
    let bodies = FakeBodies(BTreeMap::from([("demo::total".to_string(), different(true))]));
    let mut trees = BTreeMap::from([("a".to_string(), old), ("b".to_string(), new)]);
    let (report, _) = verify_changeset(&set, &mut |tree| Ok(trees.remove(tree).unwrap()), &bodies, &FakeExprs(Effect::Widen), Options::default());
    // Design 35: a group kind and the role `Mechanical` are labels. No rule compares them with
    // a category or with a compare result.
    assert!(report.organization.is_empty(), "{:#?}", report.organization);
}

#[test]
fn subgroup_suggestions_follow_the_roles() {
    let (_d1, old) = load(PLAIN);
    let (_d2, new) = load(EXTRACTED);
    let set = ChangeSet {
        steps: vec![step(vec![claim("c1", "extract-variable", Some("demo::f/tail"), serde_json::json!({"name": "product"}))])],
        groups: vec![Group { id: "G1".into(), title: "t".into(), kind: GroupKind::Refactor, why: String::new(), steps: vec![0] }],
        base_tree: "a".into(),
        head_tree: "b".into(),
        ..Default::default()
    };
    let bodies = FakeBodies(BTreeMap::from([("demo::f".to_string(), equal("n2"))]));
    let mut trees = BTreeMap::from([("a".to_string(), old), ("b".to_string(), new)]);
    let (report, _) = verify_changeset(&set, &mut |tree| Ok(trees.remove(tree).unwrap()), &bodies, &NoOracle, Options { require_groups: true, derive_edits: false });
    assert!(report.accepted, "{report:#?}");
    let suggestion = organize::suggest(&set, "G1", &report.steps, &Default::default());
    assert_eq!(suggestion.len(), 1);
    assert_eq!(suggestion[0].role, Role::Mechanical);
    // No result folds a subgroup (design 35).
    assert!(organize::is_open(&suggestion[0], &report.steps));
}

#[test]
fn panic_delta_counts_explicit_sites() {
    let before = "pub fn f(v: &[u32], o: Option<u32>) -> u32 { o.unwrap() + v[0] }\n";
    let after = "pub fn f(v: &[u32], o: Option<u32>) -> u32 { o.unwrap_or(0) + v.first().copied().unwrap_or(0) }\n";
    let report = run(before, after, vec![claim("c1", "replace-expression", Some("demo::f/tail"), serde_json::json!({}))], &[], None);
    assert_eq!(report.panic_delta, -2);
}

#[test]
fn new_claims_go_to_their_suggested_subgroup() {
    let mut set = ChangeSet {
        steps: vec![step(vec![claim("c1", "add-if", Some("demo::f/S0"), serde_json::json!({})), claim("c2", "rename", Some("demo::g"), serde_json::json!({"to": "h"}))])],
        groups: vec![Group { id: "G1".into(), title: "t".into(), kind: GroupKind::Fix, why: String::new(), steps: vec![0] }],
        subgroups: vec![Subgroup { id: "G1:x".into(), group: "G1".into(), role: Role::Custom, title: "Mine".into(), why: String::new(), claims: vec!["c1".into()], edited: true, new_claims: Vec::new() }],
        ..Default::default()
    };
    organize::place_new_claims(&mut set, "G1", &Default::default());
    assert_eq!(set.subgroups.len(), 2);
    assert_eq!(set.subgroups[0].claims, ["c1"], "the agent's subgroup stays the same");
    assert_eq!(set.subgroups[1].role, Role::Mechanical);
    assert_eq!(set.subgroups[1].new_claims, ["c2"]);
}

#[test]
fn unsafe_changes_are_in_the_spotlight() {
    let before = "pub fn f(p: *const u32) -> u32 {\n    unsafe { *p }\n}\n";
    let after = "pub fn f(p: *const u32) -> u32 {\n    unsafe { *p.add(1) }\n}\n";
    let report = run(before, after, vec![claim("c1", "replace-expression", Some("demo::f/tail/body/tail/operand"), serde_json::json!({}))], &[], None);
    assert!(report.spotlight.iter().any(|s| s.starts_with("unsafe code")), "{:?}", report.spotlight);
}

#[test]
fn api_level_estimate() {
    let before = "pub fn f(x: u32) -> u32 { x }\nfn private() {}\npub mod m { pub fn g() {} }\n";
    let minor = "pub fn f(x: u32) -> u32 { x }\nfn private() {}\npub fn new_one() {}\npub mod m { pub fn g() {} }\n";
    let major = "pub fn f(x: u64) -> u32 { x as u32 }\nfn private() {}\npub mod m { pub fn g() {} }\n";
    let patch = "pub fn f(x: u32) -> u32 { x }\nfn private(_: u8) {}\npub mod m { pub fn g() {} }\n";
    assert_eq!(run(before, minor, vec![], &[], None).api_level.as_deref(), Some("minor"));
    assert_eq!(run(before, major, vec![], &[], None).api_level.as_deref(), Some("major"));
    assert_eq!(run(before, patch, vec![], &[], None).api_level.as_deref(), Some("patch"));
}

#[test]
fn without_bodies_a_claim_of_a_rewrite_is_verified_by_its_shape() {
    let report = run(PLAIN, EXTRACTED, vec![claim("c1", "extract-variable", Some("demo::f/tail"), serde_json::json!({"name": "product"}))], &[], None);
    assert_eq!(status_of(&report, "c1"), &ClaimStatus::Verified { proof: "shape".into() });
    assert!(report.uncovered.is_empty());
    assert!(report.accepted(), "{report:#?}");
}

#[test]
fn macro_input_changes_are_free_and_in_the_spotlight() {
    let before = "macro_rules! table { ($($x:expr),*) => { [$($x),*] }; }\npub fn f() -> [u32; 2] {\n    table!(1, 2)\n}\n";
    let after = "macro_rules! table { ($($x:expr),*) => { [$($x),*] }; }\npub fn f() -> [u32; 2] {\n    table!(1, 3)\n}\n";
    let missing = run(before, after, vec![], &[], None);
    assert_eq!(missing.uncovered[0].hint.as_deref(), Some("replace-macro-input"));
    let report = run(before, after, vec![claim("c1", "replace-macro-input", Some("demo::f/tail"), serde_json::json!({}))], &[("demo::f", different(false))], None);
    assert_eq!(status_of(&report, "c1"), &ClaimStatus::Verified { proof: "free".into() });
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert!(report.spotlight.iter().any(|s| s.starts_with("macro input")), "{:?}", report.spotlight);
    let wrong = run(PLAIN, EXTRACTED, vec![claim("c1", "replace-macro-input", Some("demo::f/tail"), serde_json::json!({}))], &[], None);
    assert!(matches!(status_of(&wrong, "c1"), ClaimStatus::Malformed { .. }));
}

#[test]
fn an_unclaimed_rename_gets_a_rename_hint() {
    let before = "pub fn helper(x: u32) -> u32 { x + 1 }\n";
    let after = "pub fn add_one(x: u32) -> u32 { x + 1 }\n";
    let report = run(before, after, vec![], &[], None);
    assert!(report.uncovered.iter().all(|u| u.hint.as_deref().is_some_and(|h| h.starts_with("rename"))), "{:#?}", report.uncovered);
}


#[test]
fn a_formatting_only_head_tree_is_accepted() {
    let (_d1, old) = load(PLAIN);
    let (_d2, new) = load(EXTRACTED);
    let (_d3, head) = load("pub fn f(a: u32, b: u32, c: u32) -> u32 {\n        let product = a * b;\n    product +\n c\n}\n");
    let set = ChangeSet {
        base_tree: "a".into(),
        head_tree: "c".into(),
        steps: vec![step(vec![claim("c1", "extract-variable", Some("demo::f/tail"), serde_json::json!({"name": "product"}))])],
        ..Default::default()
    };
    let bodies = FakeBodies(BTreeMap::from([("demo::f".to_string(), equal("n0"))]));
    let trees = BTreeMap::from([("a".to_string(), old), ("b".to_string(), new), ("c".to_string(), head)]);
    let (report, _) = verify_changeset(&set, &mut |tree| Ok(trees.get(tree).cloned().unwrap()), &bodies, &NoOracle, Options::default());
    assert!(report.organization.is_empty(), "{:#?}", report.organization);
    assert!(report.warnings.iter().any(|w| w.contains("only in formatting")));
}

#[test]
fn an_empty_claim_points_to_the_change() {
    let report = run(BEFORE, AFTER, vec![
        claim("c1", "add-early-exit", Some("demo::total/S0"), serde_json::json!({})),
        claim("c2", "change-operator", Some("demo::total/S1/body/S0/then"), serde_json::json!({})),
    ], &[], None);
    let c2 = report.claims.iter().find(|c| c.claim == "c2").unwrap();
    assert_eq!(c2.status, ClaimStatus::Empty);
    assert_eq!(c2.hint.as_deref(), Some("The change is at `demo::total/S1/body/S0/cond`: move the claim there."), "{c2:?}");
}

#[test]
fn an_extract_without_a_change_at_its_anchor_is_empty() {
    let before = "pub fn f(a: u32, b: u32) -> u32 {\n    let x = a + b;\n    x * 2\n}\n";
    let after = "fn sum(a: u32, b: u32) -> u32 {\n    a + b\n}\npub fn f(a: u32, b: u32) -> u32 {\n    let x = sum(a, b);\n    x * 2\n}\n";
    let wrong = run(before, after, vec![claim("c1", "extract-function", Some("demo::f/tail"), serde_json::json!({"new": "sum"}))], &[("demo::f", equal("n3"))], None);
    assert_eq!(status_of(&wrong, "c1"), &ClaimStatus::Empty);
    let right = run(before, after, vec![claim("c1", "extract-function", Some("demo::f/S0/init"), serde_json::json!({"new": "sum"}))], &[("demo::f", equal("n3"))], None);
    assert!(status_of(&right, "c1").is_verified(), "{:?}", status_of(&right, "c1"));
}

#[test]
fn or_pattern_alternatives() {
    let before = "pub fn unit(c: char) -> u32 {\n    match c {\n        'a' | 'b' => 1,\n        _ => 0,\n    }\n}\n";
    let after = "pub fn unit(c: char) -> u32 {\n    match c {\n        'a' | 'b' | 'c' => 1,\n        _ => 0,\n    }\n}\n";
    let missing = run(before, after, vec![], &[], None);
    assert_eq!(missing.uncovered[0].hint.as_deref(), Some("add-alternative"));
    let report = run(before, after, vec![claim("c1", "add-alternative", Some("demo::unit/S0/arm[0].pat"), serde_json::json!({}))], &[("demo::unit", different(true))], None);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    let wrong = run(before, after, vec![claim("c1", "remove-alternative", Some("demo::unit/S0/arm[0].pat"), serde_json::json!({}))], &[], None);
    assert!(matches!(status_of(&wrong, "c1"), ClaimStatus::Refuted { .. }));
}

#[test]
fn a_reexported_method_is_public_api() {
    let before = "mod inner {\n    pub struct T;\n    impl T {\n        pub fn a(&self) {}\n    }\n}\npub use inner::T;\n";
    let after = "mod inner {\n    pub struct T;\n    impl T {\n        pub fn a(&self) {}\n        pub fn b(&self) {}\n    }\n}\npub use inner::T;\n";
    assert_eq!(run(before, after, vec![], &[], None).api_level.as_deref(), Some("minor"));
}

/// A body oracle with MIR facts: each body name gets its external calls. An entry `live X` is a
/// call of the local function `X` in a block that can run. An entry `dead X` is a call of `X`
/// in a block that cannot run.
struct FactBodies(Vec<(&'static str, Vec<&'static str>)>);

impl BodyOracle for FactBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(Vec::new())
    }
    fn facts(&self, _: &Step) -> anyhow::Result<Option<Vec<BodyFacts>>> {
        // The driver gives the crate that defines each callee: `std::iter` is a re-export of `core`.
        let origin = |call: &&str| call.strip_prefix("std::iter::").map(|rest| (call.to_string(), ("core".to_string(), format!("core::iter::traits::iterator::{rest}"))));
        let local = |calls: &[&str], kinds: &[&str]| -> Vec<String> { calls.iter().filter_map(|call| call.split_once(' ').filter(|(kind, _)| kinds.contains(kind)).map(|(_, path)| path.to_string())).collect() };
        Ok(Some(
            self.0
                .iter()
                .map(|(name, calls)| BodyFacts {
                    name: name.to_string(),
                    externals: calls.iter().filter(|c| !c.contains(' ')).map(|c| c.to_string()).collect(),
                    origins: calls.iter().filter_map(origin).collect(),
                    callees: local(calls, &["live", "maybe", "dead"]),
                    live: Some(local(calls, &["live", "maybe"])),
                    sure: Some(local(calls, &["live"])),
                    ..Default::default()
                })
                .collect(),
        ))
    }
}

const PROPS_BEFORE: &str = "pub fn a() -> u32 { 1 }\n";
const PROPS_AFTER: &str = "pub fn a() -> u32 { 1 }\n\npub fn wide(v: &[u32]) -> u64 {\n    helper(v)\n}\n\nfn helper(v: &[u32]) -> u64 {\n    v.iter().map(|x| *x as u64).sum()\n}\n\npub fn risky(v: &[u32]) -> u32 {\n    v[0]\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t_wide() {\n        assert_eq!(super::wide(&[1]), 1);\n    }\n}\n";

#[test]
fn property_claims_check_the_code_after_the_step() {
    let claims = vec![
        claim("c1", "add-item", Some("demo::wide"), serde_json::json!({})),
        claim("c2", "add-item", Some("demo::helper"), serde_json::json!({})),
        claim("c3", "add-item", Some("demo::risky"), serde_json::json!({})),
        claim("c4", "add-item", Some("demo::tests"), serde_json::json!({})),
        claim("c5", "no-panic", Some("demo::wide"), serde_json::json!({})),
        claim("c6", "no-panic", Some("demo::risky"), serde_json::json!({})),
        claim("c7", "tested-by", Some("demo::wide"), serde_json::json!({"test": "t_wide"})),
        claim("c8", "tested-by", Some("demo::risky"), serde_json::json!({"test": "t_wide"})),
        claim("c9", "no-unsafe", Some("demo::wide"), serde_json::json!({})),
        claim("c10", "no-io", Some("demo::wide"), serde_json::json!({})),
    ];
    // Phase 1: the MIR facts are not there, so `no-io` and the implicit panic sites wait.
    let report = run(PROPS_BEFORE, PROPS_AFTER, claims.clone(), &[], None);
    assert!(status_of(&report, "c5").is_pending(), "{:?}", status_of(&report, "c5"));
    assert!(matches!(status_of(&report, "c6"), ClaimStatus::Refuted { reason } if reason.contains("panic")));
    // 5f, A2: the reachable calls of the test come from the MIR.
    assert!(status_of(&report, "c7").is_pending(), "the test calls `wide`");
    assert!(matches!(status_of(&report, "c8"), ClaimStatus::Refuted { .. }), "the test does not call `risky`");
    // 5f, A1: the scope of `no-unsafe` has the trait impls and the `Drop` impls from the MIR.
    assert!(status_of(&report, "c9").is_pending());
    assert!(status_of(&report, "c10").is_pending());
    assert!(report.uncovered.is_empty(), "{:?}", report.uncovered);
    assert_eq!(report.claims.iter().find(|c| c.claim == "c5").unwrap().category, None, "a property has no category");

    // Phase 2: the facts of the local callee count too.
    let (_d1, old) = load(PROPS_BEFORE);
    let (_d2, new) = load(PROPS_AFTER);
    let step = step(claims);
    let clean = FactBodies(vec![("demo::wide", vec![]), ("demo::helper", vec!["std::iter::Iterator::sum"]), ("demo::tests::t_wide", vec!["live demo::wide"])]);
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &clean, &NoOracle).report;
    assert!(status_of(&report, "c10").is_verified(), "{:?}", status_of(&report, "c10"));
    let io = FactBodies(vec![("demo::wide", vec![]), ("demo::helper", vec!["std::fs::read"])]);
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &io, &NoOracle).report;
    assert!(matches!(status_of(&report, "c10"), ClaimStatus::Refuted { reason } if reason.contains("std::fs::read")));

    // T7 (trial 2, A5a): each result states its scope. A call to another crate makes `no-panic`
    // only partly verified, and a MIR `Assert` (a division) refutes it.
    let result = |report: &StepReport, id: &str| report.claims.iter().find(|c| c.claim == id).unwrap().clone();
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &clean, &NoOracle).report;
    let c5 = result(&report, "c5");
    assert_eq!(c5.status, ClaimStatus::Verified { proof: "partial".into() });
    assert!(c5.after.as_deref().is_some_and(|t| t.contains("only partly verified") && t.contains("std::iter::Iterator::sum")), "{c5:?}");
    assert_eq!(result(&report, "c7").status, ClaimStatus::Verified { proof: "mir".into() });
    assert!(result(&report, "c7").after.as_deref().is_some_and(|t| t.contains("calls `demo::wide` on each path from its start to its normal return. Semilla does not run the test and does not check what the test asserts.")), "{:?}", result(&report, "c7"));
    assert!(result(&report, "c10").after.as_deref().is_some_and(|t| t.contains("examined: 2 local bodies (0 through trait impls, 0 drop bodies)")), "{:?}", result(&report, "c10"));
    assert_eq!(result(&report, "c9").status, ClaimStatus::Verified { proof: "mir".into() });
    let text = views::text_report(&[report.clone()]);
    assert!(text.contains("verified (partial)") && text.contains("scope: only partly verified"), "{text}");
    let division = FactBodies(vec![("demo::wide", vec![]), ("demo::helper", vec![])]);
    let oracle = PanicBodies(vec![("demo::wide", vec![]), ("demo::helper", vec!["division by zero"])]);
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle).report;
    assert!(matches!(status_of(&report, "c5"), ClaimStatus::Refuted { reason } if reason.contains("division by zero in `demo::helper`")), "{:?}", status_of(&report, "c5"));
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &division, &NoOracle).report;
    assert_eq!(result(&report, "c5").status, ClaimStatus::Verified { proof: "mir".into() });
}

/// A body oracle with MIR facts: each body name gets its implicit panic sites.
struct PanicBodies(Vec<(&'static str, Vec<&'static str>)>);

impl BodyOracle for PanicBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(Vec::new())
    }
    fn facts(&self, _: &Step) -> anyhow::Result<Option<Vec<BodyFacts>>> {
        Ok(Some(self.0.iter().map(|(name, panics)| BodyFacts { name: name.to_string(), panics: panics.iter().map(|p| p.to_string()).collect(), ..Default::default() }).collect()))
    }
}

/// A body oracle that gives the MIR facts as they are.
struct MirFacts(Vec<BodyFacts>);

impl BodyOracle for MirFacts {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(Vec::new())
    }
    fn facts(&self, _: &Step) -> anyhow::Result<Option<Vec<BodyFacts>>> {
        Ok(Some(self.0.clone()))
    }
}

#[test]
fn calls_only_matches_full_segments() {
    // Trial 4, cheat C4: `calls=[""]` and `calls=["s"]` allowed each call.
    let before = "pub fn other() {}\n";
    let after = "pub fn other() {}\npub fn save() -> u64 {\n    1\n}\n";
    let check = |calls: serde_json::Value| {
        let (_d1, old) = load(before);
        let (_d2, new) = load(after);
        let step = step(vec![claim("c1", "calls-only", Some("demo::save"), serde_json::json!({ "calls": calls }))]);
        let oracle = FactBodies(vec![("demo::save", vec!["std::fs::write", "std::iter::Iterator::sum"])]);
        verify_step(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle).report.claims[0].status.clone()
    };
    for bad in [serde_json::json!([""]), serde_json::json!(["std::fs", ""]), serde_json::json!(["std:"]), serde_json::json!(["::"]), serde_json::json!(["std fs"]), serde_json::json!(["std::fs::*"]), serde_json::json!([1]), serde_json::json!("std::fs,,core")] {
        assert!(matches!(check(bad.clone()), ClaimStatus::Malformed { reason } if reason.contains("is not a path")), "{bad}");
    }
    for narrow in [serde_json::json!(["s"]), serde_json::json!(["std::f", "std::iter"]), serde_json::json!(["st", "std::i", "core"]), serde_json::json!(["std::iter"]), serde_json::json!([])] {
        assert!(matches!(check(narrow.clone()), ClaimStatus::Refuted { reason } if reason.contains("std::fs::write")), "{narrow}");
    }
    // A full path of segments allows the path and each path below it. The defining crate of
    // `std::iter::Iterator::sum` is `core`: the two names are the same function.
    // The end `::` of the old form (`"core::"`) is the same entry as `core`.
    for fine in [serde_json::json!(["std"]), serde_json::json!(["std::"]), serde_json::json!(["std::fs", "std::iter"]), serde_json::json!(["std::fs::", "core::"]), serde_json::json!(["std::fs::write", "core::iter"]), serde_json::json!("std::fs, core")] {
        assert_eq!(check(fine.clone()), ClaimStatus::Verified { proof: "mir".into() }, "{fine}");
    }
}

#[test]
fn calls_only_always_allows_core_and_alloc() {
    // Brief 5g, item 3: a call to a function that `core` or `alloc` defines needs no entry.
    let before = "pub fn other() {}\n";
    let after = "pub fn other() {}\npub fn save() -> u64 {\n    1\n}\n";
    let check = |externals: &[&str], drops: &[&str], calls: serde_json::Value| {
        let (_d1, old) = load(before);
        let (_d2, new) = load(after);
        let step = step(vec![claim("c1", "calls-only", Some("demo::save"), serde_json::json!({ "calls": calls }))]);
        // The driver gives the crate that defines each callee: `std::iter` is a re-export of `core`.
        let origins = externals.iter().filter_map(|call| call.strip_prefix("std::iter::").map(|rest| (call.to_string(), ("core".to_string(), format!("core::iter::traits::iterator::{rest}"))))).collect();
        let facts = BodyFacts {
            name: "demo::save".into(),
            externals: externals.iter().map(|call| call.to_string()).collect(),
            origins,
            drop_types: drops.iter().map(|ty| (ty.to_string(), "std".to_string())).collect(),
            ..Default::default()
        };
        verify_step(&StepInput { step: &step, before: &old, after: &new }, &MirFacts(vec![facts]), &NoOracle).report.claims[0].clone()
    };
    // `std::iter::Iterator::sum` is a re-export of `core`. `alloc::fmt::format` and
    // `core::mem::swap` have the crate in the printed path.
    let library = ["std::iter::Iterator::sum", "alloc::fmt::format", "core::mem::swap::<u8>", "<u8 as core::fmt::Display>::fmt"];
    let result = check(&library, &[], serde_json::json!([]));
    assert_eq!(result.status, ClaimStatus::Verified { proof: "mir".into() });
    assert!(result.after.as_deref().unwrap().contains("calls to `core` and `alloc` are always allowed"), "{:?}", result.after);
    assert!(result.complete_calls.is_empty());
    // A function that `std` or another crate defines needs an entry. A path that starts with
    // the text `core` is not the crate `core`.
    for (call, entry) in [("std::fs::write", "std::fs"), ("corex::run", "corex"), ("std::alloc::dealloc", "std::alloc"), ("serde_json::to_string::<u8>", "serde_json")] {
        let result = check(&[call], &[], serde_json::json!([]));
        assert!(matches!(&result.status, ClaimStatus::Refuted { reason } if reason.contains(call) && reason.contains("calls to `core` and `alloc` are always allowed")), "{call}: {:?}", result.status);
        assert_eq!(result.complete_calls, vec![entry.to_string()], "{call}");
    }
    // The complete list: the shortest module path of each path that the list does not cover.
    let mut all = library.to_vec();
    all.extend(["std::fs::write", "std::fs::File::open", "<std::net::TcpStream as std::io::Write>::write_all", "std::f64::<impl f64>::sqrt", "std::mem::swap::<u8>", "std::collections::HashMap::<K, V>::new"]);
    let drops = ["std::process::Child"];
    let result = check(&all, &drops, serde_json::json!([]));
    let ClaimStatus::Refuted { reason } = &result.status else { panic!("{:?}", result.status) };
    assert!(!reason.contains("Iterator::sum") && !reason.contains("alloc::fmt::format") && !reason.contains("core::"), "{reason}");
    assert_eq!(result.complete_calls, ["std::collections", "std::f64", "std::fs", "std::io", "std::mem", "std::net", "std::process"]);
    // The entries of the agent that cover a call stay. An entry below another entry goes. An
    // entry that covers no call goes (batch 5l, B2): `core` is always allowed, and the function
    // calls no code of `std::thread`.
    let result = check(&all, &drops, serde_json::json!(["std::io", "std::fs::File", "core", "std::thread"]));
    assert_eq!(result.complete_calls, ["std::io", "std::collections", "std::f64", "std::fs", "std::mem", "std::net", "std::process"]);
    // The complete list is a true claim.
    let complete = result.complete_calls.clone();
    assert_eq!(check(&all, &drops, serde_json::json!(complete)).status, ClaimStatus::Verified { proof: "mir".into() });
    // One entry less is a false claim.
    assert!(matches!(check(&all, &drops, serde_json::json!(complete[..complete.len() - 1])).status, ClaimStatus::Refuted { .. }));
}

#[test]
fn the_claim_list_of_a_rejected_step_has_a_short_cause() {
    // Brief 5g, item 4: the first lines of a rejected step have the full text of a failed claim.
    // The claim list has only the status and the short cause.
    use semilla_core::ClaimResult;
    let result = |claim: &str, status: ClaimStatus| ClaimResult { build_code: None, claim: claim.into(), step: 0, status, category: None, effect: None, witness: None, hint: Some("the hint".into()), before: None, after: None, affected: Vec::new(), complete_calls: Vec::new() };
    let mut report = semilla_core::StepReport::default();
    report.claims = vec![
        result("c1", ClaimStatus::Refuted { reason: "external calls outside the list of `calls`: `std::fs::write` in `demo::save` (calls to `core` and `alloc` are always allowed)".into() }),
        result("c2", ClaimStatus::Refuted { reason: "implicit panic sites: division by zero in `demo::share`".into() }),
        result("c3", ClaimStatus::Refuted { reason: "the test `demo::tests::t` does not call `demo::a::b`".into() }),
        result("c4", ClaimStatus::Malformed { reason: "the entry `` of `calls` is not a path. Each entry is a path of full segments".into() }),
        result("c5", ClaimStatus::Refuted { reason: "the MIR of `demo::f` is different (the first difference is in the block 2); split the step".into() }),
        result("c6", ClaimStatus::Verified { proof: "mir".into() }),
        result("c7", ClaimStatus::Empty),
    ];
    let short = views::text_report_with(std::slice::from_ref(&report), true);
    let lines: Vec<&str> = short.lines().skip(1).filter(|line| !line.is_empty()).collect();
    assert_eq!(
        lines,
        [
            "  ✗ c1 refuted: external calls outside the list of `calls`",
            "  ✗ c2 refuted: implicit panic sites",
            "  ✗ c3 refuted: the test `demo::tests::t` does not call `demo::a::b`",
            "  ✗ c4 malformed: the entry `` of `calls` is not a path",
            "  ✗ c5 refuted: the MIR of `demo::f` is different",
            "  ✓ c6 verified (mir)",
            "      hint: the hint",
            "  ✗ c7 empty",
        ]
    );
    // The usual report keeps the full text.
    let full = views::text_report(std::slice::from_ref(&report));
    assert!(full.contains("  ✗ c1 refuted: external calls outside the list of `calls`: `std::fs::write` in `demo::save`"), "{full}");
    assert!(full.contains("  ✗ c7 empty: the claim explains no change"), "{full}");
}

#[test]
fn each_file_spotlight_text_fits_its_fact() {
    // Brief 5g, item 4: a mode change of a parsed `.rs` file had the text for Rust code that
    // Semilla does not parse.
    use crate::files::file_spotlight;
    let why = |file: &str, note: &str, content_changed: bool, parsed: bool| file_spotlight(file, note, content_changed, parsed, file.ends_with(".rs"), None, || false).unwrap_or_default();
    let (mode, link, submodule) = ("a file mode", "a symbolic link", "a submodule");
    let (unparsed, manifest, list) = ("Rust code that Semilla does not parse", "a manifest whose keys Semilla cannot compare", "not on the allow list of files");
    let texts = [mode, link, submodule, unparsed, manifest, list];
    // The file, the note, "the content changed", "the loader parses the file", the causes.
    let cases: [(&str, &str, bool, bool, &[&str]); 14] = [
        ("src/lib.rs", "mode 100644 → 100755", false, true, &[mode]),
        ("build.rs", "", true, false, &[unparsed]),
        ("build.rs", "mode 100644 → 100755", false, false, &[mode]),
        ("build.rs", "mode 100644 → 100755", true, false, &[mode, unparsed]),
        ("src/gen.rs", "new symbolic link", true, false, &[link, unparsed]),
        ("run.sh", "mode 100644 → 100755", false, false, &[mode]),
        ("run.sh", "mode 100644 → 100755", true, false, &[mode, list]),
        ("run.sh", "", true, false, &[list]),
        ("logo.bin", "binary file changed", true, false, &[list]),
        ("README.md", "mode 100755 → 100644", false, false, &[mode]),
        ("vendor/lib", "new submodule", true, false, &[submodule, list]),
        ("docs/link.md", "symbolic link changed", true, false, &[link]),
        ("Cargo.toml", "", true, false, &[manifest]),
        ("Cargo.toml", "new symbolic link", true, false, &[link, manifest]),
    ];
    for (file, note, content_changed, parsed, causes) in cases {
        let text = why(file, note, content_changed, parsed);
        for cause in texts {
            assert_eq!(text.contains(cause), causes.contains(&cause), "{file} ({note}): `{cause}` in `{text}`");
        }
    }
    // A file on the allow list with no special note: no spotlight, but for a doc include.
    assert_eq!(file_spotlight("README.md", "", true, false, false, None, || false), None);
    assert!(file_spotlight("README.md", "", true, false, false, None, || true).is_some_and(|text| text.starts_with("a doc attribute includes this file")));
    // A Rust file never goes without a text.
    assert!(file_spotlight("src/lib.rs", "", true, true, true, None, || false).is_some());
}

#[test]
fn no_io_decides_by_the_crate_that_defines_the_function() {
    use crate::scope::{Io, io_class};
    // `core` and `alloc`: no I/O. The printed path of a re-export does not count.
    assert_eq!(io_class("core", "core::iter::traits::iterator::Iterator::sum"), Io::No);
    assert_eq!(io_class("alloc", "alloc::fmt::format"), Io::No);
    // `std`: an allow list of modules. Each `std` path of the name must be in the list.
    assert_eq!(io_class("std", "std::collections::hash::map::HashMap::<K, V>::new"), Io::No);
    assert_eq!(io_class("std", "std::fs::write"), Io::Yes);
    assert_eq!(io_class("std", "std::io::stdio::_print"), Io::Yes);
    assert_eq!(io_class("std", "std::time::Instant::now"), Io::Yes);
    assert_eq!(io_class("std", "std::collectionsx::f"), Io::Yes, "a full segment");
    assert_eq!(io_class("std", "<std::fs::File as core::fmt::Debug>::fmt"), Io::Yes);
    assert_eq!(io_class("std", "<std::collections::hash::map::HashMap<K, V> as std::io::Write>::write"), Io::Yes, "each path counts");
    // Each other crate: not examined.
    assert_eq!(io_class("serde_json", "serde_json::ser::to_string"), Io::NotExamined);
}

#[test]
fn the_scope_of_a_property_has_the_trait_impls_and_the_drop_impls() {
    // Trial 4, cheat C1: `Label(1).to_string()` runs the local `Display::fmt` through `alloc`.
    let before = "pub fn other() {}\n";
    let after = "pub fn other() {}\npub struct Label(pub u64);\npub fn label() -> u64 {\n    1\n}\n";
    let ty = |path: &str, krate: &str| (path.to_string(), krate.to_string());
    let label = |reach: Vec<(String, String)>, drops: Vec<(String, String)>| BodyFacts { name: "demo::label".into(), reach_types: reach, drop_types: drops, ..Default::default() };
    let fmt = BodyFacts { name: "<demo::Label as std::fmt::Display>::fmt".into(), externals: vec!["std::fs::write".into()], impl_of: vec!["demo::Label".into()], ..Default::default() };
    let drop_body = BodyFacts { name: "<demo::Label as std::ops::Drop>::drop".into(), externals: vec!["std::fs::write".into()], impl_of: vec!["demo::Label".into()], drop_of: Some("demo::Label".into()), ..Default::default() };
    let check = |facts: Vec<BodyFacts>, op: &str| {
        let (_d1, old) = load(before);
        let (_d2, new) = load(after);
        let step = step(vec![claim("c1", op, Some("demo::label"), serde_json::json!({}))]);
        verify_step(&StepInput { step: &step, before: &old, after: &new }, &MirFacts(facts), &NoOracle).report.claims[0].clone()
    };
    // The type does not go to another crate: the impl is not in the scope.
    let result = check(vec![label(vec![], vec![]), fmt.clone()], "no-io");
    assert_eq!(result.status, ClaimStatus::Verified { proof: "mir".into() }, "{result:?}");
    // The type is a generic argument of a call: the impl is in the scope.
    let result = check(vec![label(vec![ty("demo::Label", "demo")], vec![]), fmt.clone()], "no-io");
    assert!(matches!(&result.status, ClaimStatus::Refuted { reason } if reason.contains("`std::fs::write` in `<demo::Label as std::fmt::Display>::fmt` (a trait impl of `demo::Label`")), "{result:?}");
    // The drop glue: the `Drop` body is in the scope.
    let result = check(vec![label(vec![], vec![ty("demo::Label", "demo")]), drop_body.clone()], "no-io");
    assert!(matches!(&result.status, ClaimStatus::Refuted { reason } if reason.contains("(the code can drop a value of `demo::Label`)")), "{result:?}");
    // A dropped type with no `Drop` body in the MIR is a gap.
    let result = check(vec![label(vec![], vec![ty("demo::Label", "demo")])], "no-io");
    assert_eq!(result.status, ClaimStatus::Verified { proof: "partial".into() });
    assert!(result.after.as_deref().is_some_and(|t| t.contains("not examined: the `Drop` impl of `demo::Label` has no MIR")), "{result:?}");
    // A drop of a `std` type runs the code of `std`: a file closes.
    let result = check(vec![label(vec![], vec![ty("std::fs::File", "std")])], "no-io");
    assert!(matches!(&result.status, ClaimStatus::Refuted { reason } if reason.contains("a drop of a value of `std::fs::File`")), "{result:?}");
    // A type of another crate: its impls and its destructor are not examined.
    let result = check(vec![label(vec![ty("serde_json::Value", "serde_json")], vec![ty("other::Guard", "other")])], "no-unsafe");
    assert_eq!(result.status, ClaimStatus::Verified { proof: "partial".into() });
    assert!(result.after.as_deref().is_some_and(|t| t.contains("the trait impls of `serde_json::Value`") && t.contains("the destructor of `other::Guard`")), "{result:?}");
    // A library type that can hold code (not on the list of plain types) is a gap.
    let result = check(vec![label(vec![ty("core::fmt::Arguments", "core"), ty("alloc::vec::Vec", "alloc")], vec![])], "no-io");
    assert_eq!(result.status, ClaimStatus::Verified { proof: "partial".into() });
    assert!(result.after.as_deref().is_some_and(|t| t.contains("a value of `core::fmt::Arguments` goes to the code of another crate") && !t.contains("alloc::vec::Vec")), "{result:?}");
    // Batch 7b, part 6: a panic site in a trait impl has the route from the item of the claim.
    let result = check(vec![label(vec![ty("demo::Label", "demo")], vec![]), BodyFacts { externals: vec![], panics: vec!["division by zero".into()], ..fmt.clone() }], "no-panic");
    let route = "; the route to `<demo::Label as std::fmt::Display>::fmt`: `demo::label` → a generic function of a different crate → each trait impl of `demo::Label` → `<demo::Label as std::fmt::Display>::fmt`";
    assert!(matches!(&result.status, ClaimStatus::Refuted { reason } if reason.ends_with(route)), "{result:?}");
    // The counts.
    let result = check(vec![label(vec![ty("demo::Label", "demo")], vec![ty("demo::Label", "demo")]), BodyFacts { externals: vec![], ..fmt.clone() }, BodyFacts { externals: vec![], ..drop_body.clone() }], "no-panic");
    assert_eq!(result.status, ClaimStatus::Verified { proof: "mir".into() }, "{result:?}");
    assert!(result.after.as_deref().is_some_and(|t| t.contains("examined: 3 local bodies (1 through trait impls, 1 drop bodies)")), "{result:?}");
}

#[test]
fn a_function_used_as_a_value_is_a_callee_of_a_property_claim() {
    // Trial 3, cheat A12: `let f: fn(u64) -> u64 = boom; f(n)`. The syntax call graph does not
    // see `boom`, and the explicit panic sites were checked only in the syntax scope.
    let before = "pub fn other() {}\n";
    let after = |boom: &str| format!("{before}fn boom(n: u64) -> u64 {{\n    if n > 5 {{\n        {boom}\n    }}\n    n\n}}\npub fn checked(n: u64) -> u64 {{\n    let f: fn(u64) -> u64 = boom;\n    f(n)\n}}\n");
    let facts = |boom_externals: Vec<&str>| {
        MirFacts(vec![
            BodyFacts { name: "demo::checked".into(), callees: vec!["demo::boom".into()], indirect: vec!["a call through a function pointer".into()], ..Default::default() },
            BodyFacts { name: "demo::boom".into(), externals: boom_externals.into_iter().map(str::to_string).collect(), ..Default::default() },
        ])
    };
    let check = |after: &str, oracle: &MirFacts, op: &str| {
        let (_d1, old) = load(before);
        let (_d2, new) = load(after);
        let step = step(vec![claim("c1", op, Some("demo::checked"), serde_json::json!({}))]);
        verify_step(&StepInput { step: &step, before: &old, after: &new }, oracle, &NoOracle).report.claims[0].clone()
    };
    let result = check(&after("panic!(\"boom\")"), &facts(vec!["std::rt::panic_fmt"]), "no-panic");
    assert!(matches!(&result.status, ClaimStatus::Refuted { reason } if reason.contains("demo::boom")), "{result:?}");
    // With no explicit site, the MIR call to a panic function refutes the claim too.
    let result = check(&after("return 0;"), &facts(vec!["std::rt::panic_fmt"]), "no-panic");
    assert!(matches!(&result.status, ClaimStatus::Refuted { reason } if reason.contains("a call to `std::rt::panic_fmt` in `demo::boom`")), "{result:?}");
    // No panic in `boom`: the call through the pointer makes the result partial, with the count.
    let result = check(&after("return 0;"), &facts(vec![]), "no-panic");
    assert_eq!(result.status, ClaimStatus::Verified { proof: "partial".into() });
    assert!(result.after.as_deref().is_some_and(|t| t.contains("examined: 2 local bodies") && t.contains("not examined: a call through a function pointer")), "{result:?}");
    // Batch 5w, D6: the source has `std::hint::black_box`. The MIR has the intrinsic. The
    // scope text shows the name of the source.
    let result = check(&after("return 0;"), &facts(vec!["std::intrinsics::black_box"]), "no-panic");
    assert!(result.after.as_deref().is_some_and(|t| t.contains("`std::hint::black_box`") && !t.contains("intrinsics")), "{result:?}");
    // The same rule for `no-io`, `calls-only`, and `no-unsafe`.
    for op in ["no-io", "no-unsafe"] {
        let result = check(&after("return 0;"), &facts(vec![]), op);
        assert_eq!(result.status, ClaimStatus::Verified { proof: "partial".into() }, "{op}");
        assert!(result.after.as_deref().is_some_and(|t| t.contains("not examined: a call through a function pointer")), "{op}: {result:?}");
    }
    let result = check(&after("return 0;"), &facts(vec!["std::fs::read"]), "no-io");
    assert!(matches!(&result.status, ClaimStatus::Refuted { reason } if reason.contains("std::fs::read")), "{result:?}");
    let result = check(&after("unsafe { std::hint::unreachable_unchecked() }"), &facts(vec![]), "no-unsafe");
    assert!(matches!(&result.status, ClaimStatus::Refuted { reason } if reason.contains("demo::boom")), "{result:?}");
    // A callee with no MIR facts is not named as examined.
    let oracle = MirFacts(vec![BodyFacts { name: "demo::checked".into(), callees: vec!["demo::boom".into()], ..Default::default() }]);
    let result = check(&after("return 0;"), &oracle, "no-panic");
    assert_eq!(result.status, ClaimStatus::Verified { proof: "partial".into() });
    let text = result.after.unwrap();
    assert!(text.contains("examined: 1 local bodies") && text.contains("not examined: `demo::boom` has no MIR"), "{text}");
}

#[test]
fn the_mir_asserts_are_panic_sites() {
    let text = "fn _(_1: u64, _2: u64) -> u64 {\n    bb0: {\n        assert(!move _3, division_by_zero(_1)) -> [success: bb1, unwind continue];\n    }\n    bb1: {\n        assert(move _4, bounds_check(len: _5, index: _6)) -> [success: bb2, unwind continue];\n        assert(!move _7, overflow Add(_1, _2)) -> [success: bb3, unwind continue];\n    }\n}";
    assert_eq!(assert_sites(text), ["division by zero", "bounds check", "overflow"]);
    // B3 (trial 3, D6): the zero check of a division by a constant that is not zero never fails.
    let by_100 = "fn _(_1: u64) -> u64 {\n    let _7: bool;\n    bb1: {\n        _7 = Eq(const u64 = 100, const u64 = 0);\n        assert(!_7, division_by_zero(_1)) -> [success: bb2, unwind continue];\n    }\n}";
    assert!(assert_sites(by_100).is_empty());
    let by_zero = by_100.replace("const u64 = 100", "const u64 = 0");
    assert_eq!(assert_sites(&by_zero), ["division by zero"]);
    // `x / -1` on `i64`: the zero check never fails, the overflow check stays.
    let by_minus_one = "fn _(_1: i64) -> i64 {\n    bb1: {\n        _3 = Eq(const i64 = -1, const i64 = 0);\n        assert(!_3, division_by_zero(_1)) -> [success: bb2, unwind continue];\n    }\n    bb2: {\n        _4 = Eq(const i64 = -1, const i64 = -1);\n        _5 = Eq(_1, const i64 = -9223372036854775808);\n        _6 = BitAnd(move _4, move _5);\n        assert(!move _6, overflow Div(_1, const i64 = -1)) -> [success: bb3, unwind continue];\n    }\n}";
    assert_eq!(assert_sites(by_minus_one), ["overflow"]);
}

const PAIR_BEFORE: &str = "pub fn scale(v: &[u32], k: u32) -> Vec<u32> {\n    let out: Vec<u32> = v.iter().map(|x| x * k).collect();\n    out\n}\n\npub fn twice(v: &[u32]) -> Vec<u32> {\n    scale(v, 2)\n}\n";
const PAIR_AFTER: &str = "pub fn multiply(v: &[u32], k: u32) -> Vec<u32> {\n    let out: Vec<u32> = v.iter().map(|x| x * k + 1).collect();\n    out\n}\n\npub fn twice(v: &[u32]) -> Vec<u32> {\n    multiply(v, 2)\n}\n";

#[test]
fn rewrite_item_covers_the_whole_item_and_its_references() {
    let claims = vec![claim("c1", "rewrite-item", Some("demo::scale"), serde_json::json!({"new": "multiply", "note": "the factor adds one now"}))];
    let report = run(PAIR_BEFORE, PAIR_AFTER, claims, &[], None);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    assert!(report.uncovered.is_empty(), "the call site follows the new name: {:?}", report.uncovered);
    assert!(report.spotlight.iter().any(|s| s.contains("rewritten item")), "{:?}", report.spotlight);
    assert!(report.lines.rewritten > 0 && report.lines.fine == 0, "{:?}", report.lines);
    // A short note is not enough.
    let claims = vec![claim("c1", "rewrite-item", Some("demo::scale"), serde_json::json!({"new": "multiply", "note": "x"}))];
    assert!(matches!(status_of(&run(PAIR_BEFORE, PAIR_AFTER, claims, &[], None), "c1"), ClaimStatus::Malformed { .. }));
}

#[test]
fn a_similar_removed_and_added_item_gives_a_pair_warning() {
    let (_d1, old) = load(PAIR_BEFORE);
    let (_d2, new) = load(PAIR_AFTER);
    let mut set = ChangeSet {
        base_tree: "a".into(),
        head_tree: "b".into(),
        steps: vec![step(vec![
            claim("c1", "remove-item", Some("demo::scale"), serde_json::json!({})),
            claim("c2", "add-item", Some("demo::multiply"), serde_json::json!({})),
            claim("c3", "change-path", Some("demo::twice/tail/callee"), serde_json::json!({})),
        ])],
        ..Default::default()
    };
    let mut loader = |tree: &str| Ok(if tree == "a" { old.clone() } else { new.clone() });
    let (report, _) = verify_changeset(&set, &mut loader, &NoOracle, &NoOracle, Options::default());
    assert!(report.accepted, "a pair warning does not reject: {:?}", report.organization);
    assert_eq!(report.pairs.len(), 1, "{:?}", report.pairs);
    assert_eq!((report.pairs[0].old.as_str(), report.pairs[0].new.as_str()), ("demo::scale", "demo::multiply"));
    assert!(report.pairs[0].similarity > 0.9);
    // The refusal goes with the warning.
    set.pair_refusals.push(semilla_core::PairRefusal { old: "demo::scale".into(), new: "demo::multiply".into(), reason: "a new contract".into() });
    let (report, _) = verify_changeset(&set, &mut loader, &NoOracle, &NoOracle, Options::default());
    assert_eq!(report.pairs[0].refusal.as_deref(), Some("a new contract"));
}

#[test]
fn phase_1_gives_no_pair_warning_for_a_pair_that_waits_for_its_identity_proof() {
    // Brief 5f, B1 (trial 4, D2): phase 1 of `status` gave a pair warning for a move that phase 2
    // proves.
    let (_d1, old) = load(PAIR_BEFORE);
    let (_d2, new) = load(PAIR_AFTER);
    let set = ChangeSet { base_tree: "a".into(), head_tree: "b".into(), steps: vec![step(Vec::new())], ..Default::default() };
    let mut loader = |tree: &str| Ok(if tree == "a" { old.clone() } else { new.clone() });
    let options = Options { derive_edits: true, ..Default::default() };
    // Phase 1: the identity proof waits for phase 2, and no warning shows.
    let (report, _) = verify_changeset(&set, &mut loader, &NoOracle, &NoOracle, options);
    assert!(report.steps[0].notes.iter().any(|n| n.contains("phase 2 tries the derivation")), "{:?}", report.steps[0].notes);
    assert!(report.pairs.is_empty(), "{:?}", report.pairs);
    // Phase 2 (the MIR facts are available) with no identity proof: the warning shows.
    let (report, _) = verify_changeset(&set, &mut loader, &MirFacts(Vec::new()), &NoOracle, options);
    assert_eq!(report.pairs.len(), 1, "{:?}", report.pairs);
    assert_eq!((report.pairs[0].old.as_str(), report.pairs[0].new.as_str()), ("demo::scale", "demo::multiply"));
    // Design 36.1: the warning is only for a pair that Semilla does not derive, with the cause.
    assert_eq!(report.pairs[0].stopped.as_deref(), Some("Semilla has no compare result for the step"), "{:?}", report.pairs);
    // A new name in a new module: the cause is the limit of the derivation.
    let (_d3, far) = load(&format!("pub mod b {{\n{}}}\n", PAIR_AFTER.replace("pub fn multiply", "pub fn multiply_all")));
    let mut loader = |tree: &str| Ok(if tree == "a" { old.clone() } else { far.clone() });
    let (report, _) = verify_changeset(&set, &mut loader, &MirFacts(Vec::new()), &NoOracle, options);
    let pair = report.pairs.iter().find(|pair| pair.old == "demo::scale").unwrap_or_else(|| panic!("{:?}", report.pairs));
    assert!(pair.text().ends_with("they can be one item with a new place and a second change. Semilla does not derive one item: the name and the module are new: Semilla derives a rename in the same module, or a move with the same name"), "{}", pair.text());
}

#[test]
fn similarity_ignores_the_item_name() {
    let (_d1, old) = load(PAIR_BEFORE);
    let (_d2, new) = load("pub fn multiply(v: &[u32], k: u32) -> Vec<u32> {\n    let out: Vec<u32> = v.iter().map(|x| x * k).collect();\n    out\n}\n");
    let score = hints::similarity(&old.items["demo::scale"], &new.items["demo::multiply"]);
    assert!((score - 1.0).abs() < 1e-9, "{score}");
    let (_d3, other) = load("pub fn unrelated(a: &str) -> bool {\n    a.is_empty() || a.starts_with('#')\n}\n");
    assert!(hints::similarity(&old.items["demo::scale"], &other.items["demo::unrelated"]) < PAIR_THRESHOLD);
}

#[test]
fn parameter_renames_are_claimed_with_from() {
    let before = "pub fn f(c: u64) -> u64 {\n    let whole = c / 100;\n    whole + c\n}\n";
    let after = "pub fn f(cents: u64) -> u64 {\n    let whole = cents / 100;\n    whole + cents\n}\n";
    // The draft finds the rename of the parameter.
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let edits = semilla_syntax::diff(&old, &new, &Default::default());
    assert_eq!(crate::hints::likely_local_renames(&edits, &old, &new), vec![("demo::f".to_string(), "c".to_string(), "cents".to_string())]);
    // The claim on the function, with `from`, explains all changes.
    let report = run(before, after, vec![claim("c1", "rename-local", Some("demo::f"), serde_json::json!({"from": "c", "to": "cents"}))], &[("demo::f", equal("n0"))], None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    // `from` must be a parameter or a local variable of the function.
    let wrong = run(before, after, vec![claim("c1", "rename-local", Some("demo::f"), serde_json::json!({"from": "u64", "to": "cents"}))], &[("demo::f", equal("n0"))], None);
    assert!(matches!(status_of(&wrong, "c1"), ClaimStatus::Malformed { reason } if reason.contains("not a parameter")), "{:?}", status_of(&wrong, "c1"));
}

#[test]
fn a_changed_callee_is_not_a_local_rename() {
    let before = "fn g() -> u64 { 1 }\nfn h() -> u64 { 2 }\npub fn f(x: u64) -> u64 {\n    x + g()\n}\n";
    let after = "fn g() -> u64 { 1 }\nfn h() -> u64 { 2 }\npub fn f(x: u64) -> u64 {\n    x + h()\n}\n";
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let edits = semilla_syntax::diff(&old, &new, &Default::default());
    assert!(crate::hints::likely_local_renames(&edits, &old, &new).is_empty());
}

const TWINS: &str = "#[cfg(windows)]\npub fn sep() -> char {\n    '\\\\'\n}\n\n#[cfg(not(windows))]\npub fn sep() -> char {\n    '/'\n}\n\npub fn other(a: u32, b: u32, c: u32) -> u32 {\n    a * b + c\n}\n";

#[test]
fn a_change_in_an_inactive_cfg_twin_is_uncovered_and_in_the_spotlight() {
    let changed = TWINS.replace("'\\\\'", "';'");
    // Phase 2: the MIR has `other` and the active `sep`, but not the `cfg(windows)` twin.
    let bodies = [("demo::other", equal("n0")), ("demo::sep#1", equal("n0"))];
    let report = run(TWINS, &changed, vec![], &bodies, None);
    assert_eq!(report.uncovered.len(), 1, "{:#?}", report.uncovered);
    assert_eq!(report.uncovered[0].at.as_ref().unwrap().to_string(), "demo::sep#0/tail");
    assert!(report.spotlight.iter().any(|s| s.starts_with("inactive code: demo::sep#0")), "{:?}", report.spotlight);
    // A change in an item that a checked configuration compiles is not in this spotlight.
    let other = TWINS.replace("a * b + c", "a * b + c + 1");
    let report = run(TWINS, &other, vec![], &bodies, None);
    assert!(!report.spotlight.iter().any(|s| s.starts_with("inactive code")), "{:?}", report.spotlight);
}

#[test]
fn an_edit_in_a_function_without_a_mir_body_gets_no_compare() {
    // Design 35: the edit is a fact with the result `no compare` and the cause.
    let after = TWINS.replace("    '\\\\'\n", "    let c = '\\\\';\n    c\n");
    let report = run_derived(TWINS, &after, &[("demo::other", equal("n0"))]);
    assert!(!report.facts.is_empty() && report.facts.iter().all(|f| f.compare == Compare::NoCompare), "{:#?}", report.facts);
    assert!(report.facts.iter().all(|f| f.note.as_deref().is_some_and(|n| n.contains("no compare: no checked configuration compiles `demo::sep#0`"))), "{:#?}", report.facts);
}

#[test]
fn a_doc_or_attribute_edit_does_not_hide_an_indirect_change() {
    let src = "pub fn f(x: u32) -> u32 { x + 1 }\npub fn g(x: u32) -> u32 { x * 2 }\n";
    let changed = "pub fn f(x: u32) -> u32 { x + 2 }\n/// Doubles.\n#[inline]\npub fn g(x: u32) -> u32 { x * 2 }\n";
    let claims = || {
        vec![
            claim("c1", "change-literal", Some("demo::f/tail/rhs"), serde_json::json!({})),
            claim("c2", "edit-doc", Some("demo::g"), serde_json::json!({})),
        ]
    };
    let bodies = [("demo::f", different(false)), ("demo::g", different(false))];
    let report = run(src, changed, claims(), &bodies, None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert_eq!(unknown_causes(&report), ["demo::g"], "{:#?}", report.facts);
    // The attribute is a fact, and the fact does not explain the MIR change.
    assert_eq!(facts_of(&report), ["attribute demo::g", "indirect change demo::g"]);
    assert!(report.facts[0].affected.is_empty() && !report.facts[0].spotlight);
    // An edit that can change the body still exempts the item: the claim of that edit explains it.
    let body_edit = "pub fn f(x: u32) -> u32 { x + 2 }\npub fn g(x: u32) -> u32 { x * 3 }\n";
    let claims = vec![claim("c1", "change-literal", Some("demo::f/tail/rhs"), serde_json::json!({})), claim("c2", "change-literal", Some("demo::g/tail/rhs"), serde_json::json!({}))];
    assert!(run(src, body_edit, claims, &bodies, None).uncovered.is_empty());
}

const MOVE_BEFORE: &str = "pub mod a {\n    use crate::LIMIT;\n    pub fn over(x: u32) -> bool {\n        x > LIMIT\n    }\n}\npub mod b {\n    use crate::a::over;\n    pub fn run() -> bool {\n        over(3)\n    }\n}\npub mod c {\n    pub use crate::a::over;\n}\npub const LIMIT: u32 = 10;\n";
const MOVE_AFTER: &str = "pub mod a {\n}\npub mod b {\n    use crate::LIMIT;\n    pub fn run() -> bool {\n        over(3)\n    }\n    pub fn over(x: u32) -> bool {\n        x > LIMIT\n    }\n}\npub mod c {\n    pub use crate::a::over;\n}\npub const LIMIT: u32 = 10;\n";

#[test]
fn move_item_explains_the_private_use_edits() {
    let claims = || vec![claim("c1", "move-item", Some("demo::a::over"), serde_json::json!({"to": "demo::b::over"}))];
    let bodies = [("demo::b::over", equal("n0")), ("demo::b::run", equal("n1"))];
    let report = run(MOVE_BEFORE, MOVE_AFTER, claims(), &bodies, None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    // `run` is equal after the move map (N1).
    assert_eq!(status_of(&report, "c1"), &ClaimStatus::Verified { proof: "compare".into() });
    assert!(report.accepted());

    // A `pub use` edit needs its own claim, also when it names the item.
    let reexport = MOVE_AFTER.replace("pub use crate::a::over;", "pub use crate::b::over;");
    let report = run(MOVE_BEFORE, &reexport, claims(), &bodies, None);
    assert_eq!(report.uncovered.len(), 2, "{:#?}", report.uncovered);
    assert!(report.uncovered.iter().all(|u| u.at.as_ref().is_some_and(|a| a.item == "demo::c::use#0")), "{:#?}", report.uncovered);

    // A changed body is not a move.
    let changed = MOVE_AFTER.replace("x > LIMIT", "x >= LIMIT");
    let report = run(MOVE_BEFORE, &changed, claims(), &[("demo::b::over", different(false)), ("demo::b::run", equal("n1"))], None);
    assert!(!status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    assert!(!report.accepted());
}

#[test]
fn a_changed_body_in_a_moved_function_is_not_inactive_code() {
    let changed = MOVE_AFTER.replace("x > LIMIT", "x >= LIMIT");
    let claims = vec![claim("c1", "move-item", Some("demo::a::over"), serde_json::json!({"to": "demo::b::over"}))];
    let report = run(MOVE_BEFORE, &changed, claims, &[("demo::b::over", different(false)), ("demo::b::run", equal("n1"))], None);
    assert!(!report.spotlight.iter().any(|s| s.starts_with("inactive code")), "{:?}", report.spotlight);
    assert_eq!(report.uncovered.len(), 1, "{:#?}", report.uncovered);
}

const TYPES: &str = "pub struct Account {\n    pub id: u32,\n    pub balance: u32,\n}\npub enum Event {\n    Open,\n    Close { reason: String },\n}\npub trait Price {\n    fn price(&self) -> u32;\n    fn label(&self) -> String {\n        String::from(\"item\")\n    }\n}\n";

#[test]
fn member_entities_have_exact_claims() {
    let ok = |after: String, claims: Vec<Claim>| {
        let report = run(TYPES, &after, claims, &[], None);
        assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
        assert!(report.claims.iter().all(|c| c.status.is_verified()), "{:#?}", report.claims);
    };
    // A new field and a new variant (with its fields) are facts: no claim.
    ok(TYPES.replace("    pub balance: u32,\n", "    pub balance: u32,\n    pub created: u64,\n"), vec![]);
    ok(TYPES.replace("    Open,\n", "    Open,\n    Deposit { amount: u32 },\n"), vec![]);
    let report = run(TYPES, &TYPES.replace("    Open,\n", "    Open,\n    Deposit { amount: u32 },\n"), vec![], &[], None);
    assert_eq!(facts_of(&report), ["member added demo::Event::Deposit"]);
    // A change in a default body of a trait method is a statement edit.
    ok(TYPES.replace("\"item\"", "\"thing\""), vec![claim("c1", "change-literal", Some("demo::Price::label/tail/args[0]"), serde_json::json!({}))]);
    // A new trait method: the method and the member list of the trait.
    ok(TYPES.replace("    fn price(&self) -> u32;\n", "    fn price(&self) -> u32;\n    fn name(&self) -> String;\n"), vec![claim("c1", "add-item", Some("demo::Price::name"), serde_json::json!({}))]);
    // A renamed struct keeps its fields.
    let renamed = TYPES.replace("pub struct Account", "pub struct Customer");
    let report = run(TYPES, &renamed, vec![claim("c1", "rename", Some("demo::Account"), serde_json::json!({"to": "Customer"}))], &[], None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);

    // The hint of a new trait method names `add-item`, not `add-struct-field`.
    let report = run(TYPES, &TYPES.replace("    fn price(&self) -> u32;\n", "    fn price(&self) -> u32;\n    fn name(&self) -> String;\n"), vec![], &[], None);
    let members = report.uncovered.iter().find(|u| u.at.as_ref().is_some_and(|a| a.item == "demo::Price")).unwrap();
    assert!(members.hint.as_deref().is_some_and(|h| h.starts_with("add-item")), "{:?}", members.hint);
    // A claim with a retired op is malformed.
    let report = run(TYPES, &TYPES.replace("    pub balance: u32,\n", "    pub balance: u32,\n    pub created: u64,\n"), vec![claim("c1", "add-struct-field", Some("demo::Account"), serde_json::json!({"field": "created"}))], &[], None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Malformed { reason } if reason.contains("retired")));
}

const FIELDS: &str = "pub struct Account {\n    pub id: u32,\n    pub balance: u32,\n}\npub fn get(a: &Account) -> u32 {\n    a.balance\n}\npub fn make(balance: u32) -> Account {\n    Account { id: 1, balance }\n}\npub fn take(a: Account) -> u32 {\n    let Account { balance, .. } = a;\n    balance\n}\npub fn unrelated(balance: u32) -> u32 {\n    let x: u32 = balance;\n    x\n}\n";
const FIELDS_RENAMED: &str = "pub struct Account {\n    pub id: u32,\n    pub amount: u32,\n}\npub fn get(a: &Account) -> u32 {\n    a.amount\n}\npub fn make(balance: u32) -> Account {\n    Account { id: 1, amount: balance }\n}\npub fn take(a: Account) -> u32 {\n    let Account { amount: balance, .. } = a;\n    balance\n}\npub fn unrelated(balance: u32) -> u32 {\n    let x: u32 = balance;\n    x\n}\n";

#[test]
fn a_field_rename_covers_all_uses() {
    let rename = || vec![claim("c1", "rename", Some("demo::Account.balance"), serde_json::json!({"to": "amount"}))];
    let bodies = |get: BodyStatus| vec![("demo::get", get), ("demo::make", equal("n0")), ("demo::take", equal("n0")), ("demo::unrelated", equal("n0"))];
    let report = run(FIELDS, FIELDS_RENAMED, rename(), &bodies(equal("n0")), None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    assert!(report.accepted());
    // A body that the rename touches and whose MIR changed: unproved.
    let report = run(FIELDS, FIELDS_RENAMED, rename(), &bodies(different(false)), None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Refuted { reason } if reason.contains("demo::get")), "{:?}", status_of(&report, "c1"));
    // A second change in a use is not hidden.
    let more = FIELDS_RENAMED.replace("    a.amount\n", "    a.amount + 1\n");
    let report = run(FIELDS, &more, rename(), &bodies(different(false)), None);
    assert!(report.uncovered.iter().any(|u| u.at.as_ref().is_some_and(|a| a.item == "demo::get")), "{:#?}", report.uncovered);
    // A claim on the wrong member: the new name does not exist.
    let wrong = vec![claim("c1", "rename", Some("demo::Account.id"), serde_json::json!({"to": "amount"}))];
    let report = run(FIELDS, FIELDS_RENAMED, wrong, &bodies(equal("n0")), None);
    assert!(!report.accepted());
    // Without the MIR, the claim waits for the body proof.
    let report = run(FIELDS, FIELDS_RENAMED, rename(), &[], None);
    assert!(status_of(&report, "c1").is_pending(), "{:?}", status_of(&report, "c1"));
}

#[test]
fn a_variant_and_a_trait_method_rename_cover_all_uses() {
    let before = "pub enum Event {\n    Open,\n    Close,\n}\npub fn code(e: &Event) -> u32 {\n    match e {\n        Event::Open => 1,\n        Event::Close => 2,\n    }\n}\n";
    let after = before.replace("Open", "Opened");
    let claims = vec![claim("c1", "rename", Some("demo::Event::Open"), serde_json::json!({"to": "Opened"}))];
    let report = run(before, &after, claims, &[("demo::code", equal("n0"))], None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));

    let before = "pub struct A;\npub trait Price {\n    fn price(&self) -> u32;\n    fn twice(&self) -> u32 {\n        self.price() * 2\n    }\n}\nimpl Price for A {\n    fn price(&self) -> u32 {\n        3\n    }\n}\npub fn total(a: &A) -> u32 {\n    a.price()\n}\n";
    let after = before.replace("price", "cost");
    let claims = || vec![claim("c1", "rename", Some("demo::Price::price"), serde_json::json!({"to": "cost"}))];
    let bodies = |status: BodyStatus| vec![("demo::Price::twice", equal("n0")), ("demo::<A as Price>::cost", status), ("demo::total", equal("n0"))];
    let report = run(before, &after, claims(), &bodies(equal("n0")), None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    // The impl method follows the rename, and its body must stay the same.
    let changed = after.replace("        3\n", "        4\n");
    let report = run(before, &changed, claims(), &bodies(different(false)), None);
    assert!(!report.accepted());
    assert!(report.uncovered.iter().any(|u| u.at.as_ref().is_some_and(|a| a.item == "demo::<A as Price>::price")), "{:#?}", report.uncovered);
}

#[test]
fn a_body_on_one_side_only_needs_an_item_edit() {
    let src = "pub fn f() -> u32 {\n    1\n}\npub fn g() -> u32 {\n    2\n}\n";
    // The body of `f` exists only before, but no edit removes `f` (for example a `cfg` that a
    // feature turns off). This is an indirect change.
    let report = run(src, src, vec![], &[("demo::f", BodyStatus::BeforeOnly), ("demo::g", BodyStatus::AfterOnly)], None);
    assert_eq!(unknown_causes(&report), ["demo::f", "demo::g"], "{:#?}", report.facts);
    assert!(!report.accepted());
    // A removed item explains its body.
    let removed = "pub fn g() -> u32 {\n    2\n}\n";
    let report = run(src, removed, vec![claim("c1", "remove-item", Some("demo::f"), serde_json::json!({}))], &[("demo::f", BodyStatus::BeforeOnly), ("demo::g", equal("n0"))], None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
}

const VREN: &str = "pub struct Account {\n    pub id: u32,\n    pub balance: u32,\n}\npub struct Other {\n    pub balance: u32,\n    pub amount: u32,\n}\npub const BASE: Other = Other { balance: 1, amount: 2 };\npub const PICK: u32 = BASE.balance;\npub fn over(account: &Account) -> bool {\n    account.balance > 10\n}\npub fn pick(other: &Other) -> u32 {\n    other.balance\n}\n";

fn vren_after() -> String {
    VREN.replace("    pub balance: u32,\n}\npub struct Other", "    pub amount: u32,\n}\npub struct Other").replace("account.balance", "account.amount")
}

fn vren_bodies(changed: &str) -> Vec<(&'static str, BodyStatus)> {
    ["demo::BASE", "demo::PICK", "demo::over", "demo::pick"].into_iter().map(|name| (name, if name == changed { different(false) } else { equal("n0") })).collect()
}

#[test]
fn an_identity_step_is_proved_by_the_mir_of_the_whole_workspace() {
    let rename = || vec![claim("c1", "rename", Some("demo::Account.balance"), serde_json::json!({"to": "amount"}))];
    // A second type with a field of the same name causes no false report.
    let report = run(VREN, &vren_after(), rename(), &vren_bodies(""), None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert_eq!(status_of(&report, "c1"), &ClaimStatus::Verified { proof: "compare".into() });
    assert!(report.accepted());

    // The step is rejected: "split the step" when it has a second change, else the name of the body
    // whose MIR changed (design 22.4, rule 4).
    let split = |report: &StepReport| matches!(status_of(report, "c1"), ClaimStatus::Refuted { reason } if reason.contains("the step has a second change") || reason.contains("is not a derived body"));
    // A second change hidden in a body.
    let hidden = vren_after().replace("    other.balance\n", "    other.amount\n");
    let report = run(VREN, &hidden, rename(), &vren_bodies("demo::pick"), None);
    assert!(split(&report) && !report.accepted(), "{:?}", status_of(&report, "c1"));
    // A second change in a const initializer.
    let constant = vren_after().replace("BASE.balance", "BASE.amount");
    let report = run(VREN, &constant, rename(), &vren_bodies("demo::PICK"), None);
    assert!(split(&report) && !report.accepted(), "{:?}", status_of(&report, "c1"));
    // A fact in the same step: the type of another field.
    let fact = vren_after().replace("pub id: u32", "pub id: u64");
    let report = run(VREN, &fact, rename(), &vren_bodies(""), None);
    assert!(split(&report) && !report.accepted(), "{:?}", status_of(&report, "c1"));
    assert_eq!(facts_of(&report), ["type demo::Account.id"]);
    // Another claim in the same step.
    let literal = vren_after().replace("> 10", "> 11");
    let mut claims = rename();
    claims.push(claim("c2", "change-literal", Some("demo::over/tail/rhs"), serde_json::json!({})));
    let report = run(VREN, &literal, claims, &vren_bodies("demo::over"), None);
    assert!(split(&report) && !report.accepted(), "{:?}", status_of(&report, "c1"));
    // The wrong member.
    let wrong = vec![claim("c1", "rename", Some("demo::Account.id"), serde_json::json!({"to": "amount"}))];
    let report = run(VREN, &vren_after(), wrong, &vren_bodies(""), None);
    assert!(!report.accepted());
    // Without the MIR, the claim waits for the proof.
    let report = run(VREN, &vren_after(), rename(), &[], None);
    assert!(status_of(&report, "c1").is_pending(), "{:?}", status_of(&report, "c1"));
}

#[test]
fn a_declared_new_function_is_not_inactive_code() {
    let before = "pub fn f() -> u32 {\n    1\n}\n";
    let after = "pub fn f() -> u32 {\n    1\n}\npub fn g() -> u32 {\n    2\n}\n";
    let report = run(before, after, vec![claim("c1", "add-item", Some("demo::g"), serde_json::json!({}))], &[("demo::f", equal("n0"))], None);
    assert!(report.spotlight.is_empty(), "{:?}", report.spotlight);
}

fn facts_of(report: &StepReport) -> Vec<String> {
    report.facts.iter().map(|f| format!("{} {}", f.part, f.at.as_ref().map(|a| a.to_string()).unwrap_or_default())).collect()
}

#[test]
fn type_changes_are_derived_facts() {
    let src = "pub struct Account {\n    pub id: u32,\n    pub balance: u32,\n}\npub struct Pair(pub u32, pub u32);\npub fn f(a: u32) -> u32 {\n    a\n}\npub fn g(a: u32) -> u32 {\n    f(a)\n}\n";
    let facts = |after: &str| {
        let report = run(src, after, vec![], &[], None);
        (facts_of(&report), report)
    };
    // A field type: one fact, no claim, nothing uncovered.
    let (found, report) = facts(&src.replace("pub balance: u32", "pub balance: u64"));
    assert_eq!(found, ["type demo::Account.balance"]);
    assert!(report.uncovered.is_empty() && report.accepted(), "{:#?}", report);
    assert_eq!(report.facts[0].status, "derived");
    // A visibility and a type of one field: two facts.
    let (found, _) = facts(&src.replace("pub balance: u32", "balance: u64"));
    assert_eq!(found, ["visibility demo::Account.balance", "type demo::Account.balance"]);
    // A new field: the field and the member list are one fact.
    let (found, report) = facts(&src.replace("    pub balance: u32,\n", "    pub balance: u32,\n    pub created: u64,\n"));
    assert_eq!(found, ["member added demo::Account.created"], "{:#?}", report.facts);
    // A new order of named fields.
    let (found, _) = facts(&src.replace("    pub id: u32,\n    pub balance: u32,\n", "    pub balance: u32,\n    pub id: u32,\n"));
    assert_eq!(found, ["member order demo::Account"]);
    // Tuple fields have no names: one coarse fact, no guess.
    let (found, _) = facts(&src.replace("Pair(pub u32, pub u32)", "Pair(pub u32, pub u32, pub u32)"));
    assert_eq!(found, ["tuple fields demo::Pair"]);
    // A parameter and a return type: two facts. The call site is not a fact.
    let (found, report) = facts(&src.replace("pub fn f(a: u32) -> u32", "pub fn f(a: u32, b: bool) -> u64"));
    assert_eq!(found, ["parameter demo::f/sig/param[b]", "return type demo::f/sig/ret"]);
    assert_eq!(report.uncovered.len(), 0, "{:#?}", report.uncovered);
    // A body edit is never a fact.
    let (found, report) = facts(&src.replace("    f(a)\n", "    f(a) + 1\n"));
    assert!(found.is_empty());
    assert_eq!(report.uncovered.len(), 1);
}

#[test]
fn a_spotlight_fact_needs_a_reason() {
    let src = "pub fn f(a: u32) -> u32 {\n    a\n}\n";
    let after = src.replace("pub fn f", "pub unsafe fn f");
    let report = run(src, &after, vec![], &[], None);
    assert_eq!(facts_of(&report), ["qualifiers demo::f/sig/qualifiers"]);
    assert!(report.facts[0].spotlight && report.facts[0].reason.is_none());
    assert!(!report.accepted(), "a spotlight fact without a reason is rejected");
    // With a reason from the agent (stored with the step), the step is accepted.
    let (_d1, old) = load(src);
    let (_d2, new) = load(&after);
    let mut with_reason = step(vec![]);
    with_reason.reasons.insert("demo::f/sig/qualifiers".into(), "the caller must check the bounds".into());
    let report = verify_step(&StepInput { step: &with_reason, before: &old, after: &new }, &NoOracle, &NoOracle).report;
    assert_eq!(report.facts[0].reason.as_deref(), Some("the caller must check the bounds"));
    assert!(report.accepted());
}

/// A body oracle with statuses and MIR callee lists.
struct CallBodies(Vec<(&'static str, BodyStatus, Vec<&'static str>)>);

impl BodyOracle for CallBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.iter().map(|(name, status, _)| BodyComparison { name: name.to_string(), span: None, status: status.clone(), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }).collect())
    }
    fn facts(&self, _: &Step) -> anyhow::Result<Option<Vec<BodyFacts>>> {
        Ok(Some(self.0.iter().map(|(name, _, callees)| BodyFacts { name: name.to_string(), callees: callees.iter().map(|c| c.to_string()).collect(), ..Default::default() }).collect()))
    }
}

fn run_calls(before: &str, after: &str, bodies: Vec<(&'static str, BodyStatus, Vec<&'static str>)>) -> StepReport {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(vec![]);
    verify_step(&StepInput { step: &step, before: &old, after: &new }, &CallBodies(bodies), &NoOracle).report
}

const CALLS: &str = "pub fn f(a: u32) -> u32 {\n    a\n}\npub fn g() -> u32 {\n    f(1)\n}\npub fn h() -> u32 {\n    f(2) + 1\n}\npub fn k() -> u32 {\n    f(3)\n}\n";

fn calls_after() -> String {
    CALLS.replace("pub fn f(a: u32)", "pub fn f(a: u32, b: Option<u32>)").replace("f(1)", "f(1, None)").replace("f(2)", "f(2, None)").replace("f(3)", "f(3, Some(4))")
}

fn call_bodies(k_calls: bool) -> Vec<(&'static str, BodyStatus, Vec<&'static str>)> {
    vec![("demo::f", different(false), vec![]), ("demo::g", different(false), vec!["demo::f"]), ("demo::h", different(false), vec!["demo::f"]), ("demo::k", different(false), if k_calls { vec!["demo::f"] } else { vec![] })]
}

#[test]
fn a_parameter_fact_owns_its_call_sites() {
    let report = run_calls(CALLS, &calls_after(), call_bodies(true));
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert_eq!(facts_of(&report), ["parameter demo::f/sig/param[b]"]);
    let groups: Vec<(String, usize)> = report.facts[0].sites.iter().map(|g| (g.edit.clone(), g.sites.len())).collect();
    assert_eq!(groups, [("add the argument `None`".to_string(), 2), ("add the argument `Some (4)`".to_string(), 1)]);
    assert!(["demo::g", "demo::h", "demo::k"].iter().all(|b| report.facts[0].affected.contains(&b.to_string())), "{:?}", report.facts[0].affected);
    assert!(report.accepted(), "{:#?}", report);
    // A site with more than the forced edit is an ordinary edit (F6).
    let more = calls_after().replace("f(1, None)", "f(5, None)");
    let report = run_calls(CALLS, &more, call_bodies(true));
    assert_eq!(report.uncovered.len(), 1, "{:#?}", report.uncovered);
    assert_eq!(report.uncovered[0].at.as_ref().unwrap().item, "demo::g");
    // Without MIR evidence that the site calls the function, the site stays uncovered.
    let report = run_calls(CALLS, &calls_after(), call_bodies(false));
    assert!(report.uncovered.iter().any(|u| u.at.as_ref().is_some_and(|a| a.item == "demo::k")), "{:#?}", report.uncovered);
    // Without the MIR (phase 1), the sites stay uncovered.
    let report = run(CALLS, &calls_after(), vec![], &[], None);
    assert_eq!(report.uncovered.len(), 3, "{:#?}", report.uncovered);
}

#[test]
fn a_removed_argument_with_a_side_effect_is_in_the_spotlight() {
    let before = "pub fn load() -> u32 {\n    1\n}\npub fn p(a: u32, b: u32) -> u32 {\n    b\n}\npub fn q() -> u32 {\n    p(load(), 2)\n}\npub fn r(x: u32) -> u32 {\n    p(x, 3)\n}\n";
    let after = before.replace("pub fn p(a: u32, b: u32)", "pub fn p(b: u32)").replace("p(load(), 2)", "p(2)").replace("p(x, 3)", "p(3)");
    let bodies = vec![("demo::load", equal("n0"), vec![]), ("demo::p", different(false), vec![]), ("demo::q", different(false), vec!["demo::p", "demo::load"]), ("demo::r", different(false), vec!["demo::p"])];
    let report = run_calls(before, &after, bodies);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    let fact = &report.facts[0];
    assert_eq!(fact.sites.len(), 2, "{:?}", fact.sites);
    assert!(fact.spotlight && fact.spotlight_why.as_deref().is_some_and(|w| w.contains("demo::q")), "{fact:?}");
    assert!(!report.accepted(), "the spotlight needs a reason");
}

#[test]
fn a_field_fact_owns_its_literals_and_patterns() {
    let before = "pub struct A {\n    pub x: u32,\n}\npub fn make() -> A {\n    A { x: 1 }\n}\npub fn read(a: A) -> u32 {\n    let A { x } = a;\n    x\n}\npub fn other() -> u32 {\n    7\n}\n";
    let after = before.replace("    pub x: u32,\n}", "    pub x: u32,\n    pub y: u8,\n}").replace("A { x: 1 }", "A { x: 1, y: 0 }").replace("let A { x } = a;", "let A { x, .. } = a;");
    let bodies = vec![("demo::make", different(false), vec![]), ("demo::read", different(false), vec![]), ("demo::other", equal("n0"), vec![])];
    let report = run_calls(before, &after, bodies.clone());
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert_eq!(facts_of(&report), ["member added demo::A.y"]);
    let groups: Vec<(String, usize)> = report.facts[0].sites.iter().map(|g| (g.edit.clone(), g.sites.len())).collect();
    assert_eq!(groups, [("add the field `y : 0`".to_string(), 1), ("add `..` to the pattern".to_string(), 1)]);
    assert!(report.accepted(), "{:#?}", report);
    // A changed body that does not use the type stays an indirect-change error.
    let mut changed = bodies;
    changed[2].1 = different(false);
    let report = run_calls(before, &after, changed);
    assert_eq!(unknown_causes(&report), ["demo::other"], "{:#?}", report.facts);
}

#[test]
fn a_retired_op_is_malformed_and_change_signature_covers_only_the_edits_without_a_fact() {
    let src = "pub const LIMIT: u32 = 10;\npub fn f(a: u32) -> u32 {\n    a\n}\n";
    let report = run(src, &src.replace("pub fn f", "fn f"), vec![claim("c1", "change-visibility", Some("demo::f"), serde_json::json!({}))], &[], None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Malformed { reason } if reason.contains("retired")), "{:?}", status_of(&report, "c1"));
    // A const value is a fact (batch 4b): no claim.
    let value = src.replace("= 10", "= 11");
    let report = run(src, &value, vec![], &[], None);
    assert!(report.uncovered.is_empty() && report.facts.len() == 1 && report.accepted(), "{:#?}", report);
    // A `macro_rules!` body has no fact: `change-signature` covers it.
    let mac = format!("{src}macro_rules! m {{\n    () => {{ 1 }};\n}}\n");
    let mac_after = mac.replace("{ 1 }", "{ 2 }");
    let report = run(&mac, &mac_after, vec![claim("c1", "change-signature", Some("demo::m!"), serde_json::json!({}))], &[], None);
    assert!(report.uncovered.is_empty() && report.facts.is_empty() && report.accepted(), "{:#?}", report);
    // A parameter is a fact: `change-signature` does not take it, and the claim is empty.
    let param = src.replace("pub fn f(a: u32)", "pub fn f(a: u32, b: u8)");
    let report = run(src, &param, vec![claim("c1", "change-signature", Some("demo::f"), serde_json::json!({}))], &[], None);
    assert_eq!(status_of(&report, "c1"), &ClaimStatus::Empty);
    assert_eq!(facts_of(&report), ["parameter demo::f/sig/param[b]"]);
    // The hint of an edit with no fact names `change-signature`.
    let report = run(&mac, &mac_after, vec![], &[], None);
    assert_eq!(report.uncovered[0].hint.as_deref(), Some("change-signature"));
}

#[test]
fn each_op_argument_is_checked() {
    let malformed = |report: &StepReport| matches!(status_of(report, "c1"), ClaimStatus::Malformed { .. });
    let refuted = |report: &StepReport| matches!(status_of(report, "c1"), ClaimStatus::Refuted { .. });
    // `remove-if keep`: the kept branch must be the named branch.
    let before = "pub fn f(c: bool) -> u32 {\n    let mut x = 0;\n    if c {\n        x += 1;\n    } else {\n        x += 2;\n    }\n    x\n}\n";
    let kept_else = before.replace("    if c {\n        x += 1;\n    } else {\n        x += 2;\n    }\n", "    x += 2;\n");
    let remove = |keep: &str| vec![claim("c1", "remove-if", Some("demo::f/S1"), serde_json::json!({"keep": keep}))];
    let report = run(before, &kept_else, remove("else"), &[], None);
    assert!(status_of(&report, "c1").is_verified() && report.uncovered.is_empty(), "{:#?}", report);
    assert!(refuted(&run(before, &kept_else, remove("then"), &[], None)), "the then-branch was not kept");
    assert!(malformed(&run(before, &kept_else, remove("banana"), &[], None)));
    // An argument that the op does not declare.
    let report = run(before, &kept_else, vec![claim("c1", "remove-if", Some("demo::f/S1"), serde_json::json!({"keep": "else", "colour": "red"}))], &[], None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Malformed { reason } if reason.contains("colour")), "{:?}", status_of(&report, "c1"));
    // `note` is free text only for `rewrite-item`.
    let report = run(before, &kept_else, vec![claim("c1", "remove-if", Some("demo::f/S1"), serde_json::json!({"keep": "else", "note": "same"}))], &[], None);
    assert!(malformed(&report));
    // `extract-variable name`: the new `let` has that name.
    let extract = |name: &str| vec![claim("c1", "extract-variable", Some("demo::f/tail"), serde_json::json!({"name": name}))];
    assert!(status_of(&run(PLAIN, EXTRACTED, extract("product"), &[("demo::f", equal("n0"))], None), "c1").is_verified());
    assert!(refuted(&run(PLAIN, EXTRACTED, extract("other"), &[("demo::f", equal("n0"))], None)));
    // `wrap-expression with`: the wrapper is the named one.
    let (plain, wrapped) = ("pub fn f(a: u32) -> u32 {\n    g(a)\n}\n", "pub fn f(a: u32) -> u32 {\n    g(&a)\n}\n");
    let wrap = |with: &str| vec![claim("c1", "wrap-expression", Some("demo::f/tail/args[0]"), serde_json::json!({"with": with}))];
    let report = run(plain, wrapped, wrap("&"), &[], None);
    assert!(status_of(&report, "c1").is_verified(), "{:?} {:#?}", status_of(&report, "c1"), report.uncovered);
    assert!(refuted(&run(plain, wrapped, wrap("Some"), &[], None)));
}


#[test]
fn an_ignored_test_proves_nothing() {
    let before = "pub fn f() -> u32 {\n    1\n}\n";
    let after = |attrs: &str, body: &str| format!("{before}#[cfg(test)]\nmod tests {{\n    #[test]\n    {attrs}fn t() {{\n        {body}\n    }}\n}}\n");
    let claims = || vec![claim("c1", "add-item", Some("demo::tests"), serde_json::json!({})), claim("c2", "tested-by", Some("demo::f"), serde_json::json!({"test": "t"}))];
    let report = run(before, &after("#[ignore]\n    ", "let _ = super::f();"), claims(), &[], None);
    assert!(matches!(status_of(&report, "c2"), ClaimStatus::Malformed { reason } if reason.contains("ignore")), "{:?}", status_of(&report, "c2"));
    // A test that checks nothing: verified (it has a reachable call), with a spotlight note.
    let live = FactBodies(vec![("demo::f", vec![]), ("demo::tests::t", vec!["live demo::f"])]);
    let report = run_facts(before, &after("", "let _ = super::f();"), claims(), &live);
    assert!(status_of(&report, "c2").is_verified(), "{:?}", status_of(&report, "c2"));
    assert!(report.spotlight.iter().any(|s| s.starts_with("test without an assertion")), "{:?}", report.spotlight);
    let report = run(before, &after("", "assert_eq!(super::f(), 1);"), claims(), &[], None);
    assert!(!report.spotlight.iter().any(|s| s.starts_with("test without an assertion")));
}

#[test]
fn tested_by_needs_a_call_in_code_that_can_run() {
    // Trial 4, cheat C2: `if false { f() }` in the test. The call is in a block that the start
    // block does not reach.
    let before = "pub fn f() -> u32 {\n    1\n}\n";
    let after = format!("{before}#[cfg(test)]\nmod tests {{\n    #[test]\n    fn t() {{\n        if false {{\n            let _ = super::f();\n        }}\n    }}\n}}\n");
    let claims = || vec![claim("c1", "add-item", Some("demo::tests"), serde_json::json!({})), claim("c2", "tested-by", Some("demo::f"), serde_json::json!({"test": "t"}))];
    // Phase 1: the syntax has the call, the MIR decides.
    assert!(status_of(&run(before, &after, claims(), &[], None), "c2").is_pending());
    let dead = FactBodies(vec![("demo::f", vec![]), ("demo::tests::t", vec!["dead demo::f"])]);
    let report = run_facts(before, &after, claims(), &dead);
    assert!(matches!(status_of(&report, "c2"), ClaimStatus::Refuted { reason } if reason.contains("has a call to `demo::f` only in code that cannot run")), "{:?}", status_of(&report, "c2"));
    // The call is in a nested function that the test calls only in dead code.
    let nested = FactBodies(vec![("demo::f", vec![]), ("demo::tests::t", vec!["dead demo::tests::t::inner"]), ("demo::tests::t::inner", vec!["live demo::f"])]);
    let report = run_facts(before, &after, claims(), &nested);
    assert!(matches!(status_of(&report, "c2"), ClaimStatus::Refuted { .. }), "{:?}", status_of(&report, "c2"));
    // The compiler gives no body of the test.
    let report = run_facts(before, &after, claims(), &FactBodies(vec![("demo::f", vec![])]));
    assert!(matches!(status_of(&report, "c2"), ClaimStatus::Refuted { reason } if reason.contains("gives no body for the test")), "{:?}", status_of(&report, "c2"));
    // A reachable call through a local helper.
    let live = FactBodies(vec![("demo::f", vec![]), ("demo::g", vec!["live demo::f"]), ("demo::tests::t", vec!["live demo::g", "dead demo::f"])]);
    let report = run_facts(before, &after, claims(), &live);
    let c2 = report.claims.iter().find(|c| c.claim == "c2").unwrap();
    assert_eq!(c2.status, ClaimStatus::Verified { proof: "mir".into() });
    assert_eq!(c2.after.as_deref(), Some("the test `demo::tests::t` calls `demo::f` on each path from its start to its normal return. Semilla does not run the test and does not check what the test asserts."));
}

#[test]
fn tested_by_is_verified_only_if_each_run_of_the_test_has_the_call() {
    // Batch 5w, part C (trial 7 on humantime): `if black_box(false) { f() }` in the test. The
    // call can run for the compiler, but no run of the test has it. An entry `maybe X` is a
    // reachable call of `X` that is not on each path to the normal return.
    let before = "pub fn f() -> u32 {\n    1\n}\npub fn g() -> u32 {\n    f()\n}\n";
    let after = format!("{before}#[cfg(test)]\nmod tests {{\n    #[test]\n    fn t() {{\n        if std::hint::black_box(false) {{\n            assert_eq!(super::g(), 1);\n        }}\n    }}\n}}\n");
    let claims = || vec![claim("c1", "add-item", Some("demo::tests"), serde_json::json!({})), claim("c2", "tested-by", Some("demo::f"), serde_json::json!({"test": "t"}))];
    let result = |facts: Vec<(&'static str, Vec<&'static str>)>| {
        let report = run_facts(before, &after, claims(), &FactBodies(facts));
        let c2 = report.claims.iter().find(|c| c.claim == "c2").unwrap().clone();
        (c2.status, c2.after.unwrap_or_default(), report.accepted())
    };
    let partial = ClaimStatus::Verified { proof: "partial".into() };
    let verified = ClaimStatus::Verified { proof: "mir".into() };
    // The call is not on each path: `partial`, and the step is accepted.
    let (status, text, accepted) = result(vec![("demo::f", vec![]), ("demo::tests::t", vec!["maybe demo::f"])]);
    assert_eq!(status, partial);
    assert!(accepted);
    assert_eq!(text, "only partly verified: the test `demo::tests::t` has a reachable call to `demo::f`, but not on each path from its start to its normal return (the call is under a condition, in a loop, after an early return, or in a closure that a different function calls). Semilla did not prove that the test runs the call. Semilla does not run the test and does not check what the test asserts.");
    // The same rule holds at each level: a sure call of a helper that has a sure call.
    assert_eq!(result(vec![("demo::f", vec![]), ("demo::g", vec!["live demo::f"]), ("demo::tests::t", vec!["live demo::g"])]).0, verified);
    assert_eq!(result(vec![("demo::f", vec![]), ("demo::g", vec!["maybe demo::f"]), ("demo::tests::t", vec!["live demo::g"])]).0, partial);
    assert_eq!(result(vec![("demo::f", vec![]), ("demo::g", vec!["live demo::f"]), ("demo::tests::t", vec!["maybe demo::g"])]).0, partial);
    // One sure path is sufficient: a second call under a condition changes nothing.
    assert_eq!(result(vec![("demo::f", vec![]), ("demo::g", vec!["maybe demo::f"]), ("demo::tests::t", vec!["maybe demo::g", "live demo::f"])]).0, verified);
    // A cycle of sure calls gives no proof.
    assert_eq!(result(vec![("demo::f", vec![]), ("demo::g", vec!["live demo::g", "maybe demo::f"]), ("demo::tests::t", vec!["live demo::g"])]).0, partial);
    // No reachable call: refuted, as before.
    assert!(matches!(result(vec![("demo::f", vec![]), ("demo::tests::t", vec!["dead demo::f"])]).0, ClaimStatus::Refuted { .. }));
}

#[test]
fn a_helper_in_a_test_module_is_not_a_test() {
    // Trial 3, cheat A10: `test=ledger_core::tests::ledger_with` names a helper with no `#[test]`.
    let before = "pub fn f() -> u32 {\n    1\n}\n";
    let after = format!("{before}#[cfg(test)]\nmod tests {{\n    pub fn helper() -> u32 {{\n        super::f()\n    }}\n    #[test]\n    fn t() {{\n        assert_eq!(helper(), 1);\n    }}\n    #[tokio::test]\n    fn t2() {{\n        assert_eq!(super::f(), 1);\n    }}\n}}\n");
    let claims = |test: &str| vec![claim("c1", "add-item", Some("demo::tests"), serde_json::json!({})), claim("c2", "tested-by", Some("demo::f"), serde_json::json!({"test": test}))];
    for test in ["demo::tests::helper", "helper"] {
        let report = run(before, &after, claims(test), &[], None);
        assert!(matches!(status_of(&report, "c2"), ClaimStatus::Malformed { reason } if reason.contains("no `#[test]` attribute") || reason.contains("no test `helper`")), "{test}: {:?}", status_of(&report, "c2"));
    }
    // A real test through the helper, and a test of a test framework, are verified.
    let live = FactBodies(vec![("demo::f", vec![]), ("demo::tests::helper", vec!["live demo::f"]), ("demo::tests::t", vec!["live demo::tests::helper"]), ("demo::tests::t2", vec!["live demo::f"])]);
    for test in ["demo::tests::t", "t2"] {
        let report = run_facts(before, &after, claims(test), &live);
        assert!(status_of(&report, "c2").is_verified(), "{test}: {:?}", status_of(&report, "c2"));
    }
}

#[test]
fn an_identity_step_allows_derived_bodies_and_inlined_callers() {
    let before = "#[derive(Debug, Clone)]\npub struct Account {\n    pub balance: u32,\n}\npub fn get(a: &Account) -> u32 {\n    a.balance\n}\npub fn twice(a: &Account) -> u32 {\n    get(a) * 2\n}\n";
    let after = before.replace("pub balance: u32", "pub cents: u32").replace("a.balance", "a.cents");
    let rename = || vec![claim("c1", "rename", Some("demo::Account.balance"), serde_json::json!({"to": "cents"}))];
    let debug = "<demo::Account as std::fmt::Debug>::fmt";
    // The derived `Debug` body shows the field name. `twice` has no text edit and is equal at N1.
    let bodies = [("demo::get", equal("n0")), ("demo::twice", equal("n1")), (debug, different(false)), ("<demo::Account as std::clone::Clone>::clone", equal("n0"))];
    let report = run(before, &after, rename(), &bodies, None);
    assert!(report.accepted(), "{:#?}", report);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Verified { proof } if proof.contains("1 derived body shows the name")), "{:?}", status_of(&report, "c1"));
    // A body with a text edit must be equal at N0 or N1. `pick` reads the field of another type with
    // the same name, so the name map gives it a text edit.
    let other = format!("{before}pub struct Other {{\n    pub balance: u32,\n}}\npub fn pick(o: &Other) -> u32 {{\n    o.balance\n}}\n");
    let other_after = other.replace("pub balance: u32,\n}\npub fn get", "pub cents: u32,\n}\npub fn get").replace("a.balance", "a.cents");
    let mut strict = bodies.to_vec();
    strict.push(("demo::pick", equal("n3")));
    assert!(!run(&other, &other_after, rename(), &strict, None).accepted());
    strict.pop();
    strict.push(("demo::pick", equal("n1")));
    assert!(run(&other, &other_after, rename(), &strict, None).accepted());
    // A derived body of another trait (serde and others) needs a reason.
    let custom = before.replace("Debug, Clone", "Debug, Clone, Hash");
    let mut hashed = bodies.to_vec();
    hashed.push(("<demo::Account as std::hash::Hash>::hash", different(false)));
    let report = run(&custom, &custom.replace("pub balance: u32", "pub cents: u32").replace("a.balance", "a.cents"), rename(), &hashed, None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Refuted { reason } if reason.contains("--reason")), "{:?}", status_of(&report, "c1"));
    // A hand-written body that differs and has no text edit: rejected, with the name of the body.
    let mut changed = bodies.to_vec();
    changed[1] = ("demo::twice", different(false));
    let report = run(before, &after, rename(), &changed, None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Refuted { reason } if reason.contains("demo::twice")), "{:?}", status_of(&report, "c1"));
}

#[test]
fn a_move_gets_move_hints() {
    let report = run(MOVE_BEFORE, MOVE_AFTER, vec![], &[], None);
    assert!(report.uncovered.iter().any(|u| u.hint.as_deref().is_some_and(|h| h.starts_with("move-item"))), "{:#?}", report.uncovered);
    assert!(!report.uncovered.iter().any(|u| u.hint.as_deref().is_some_and(|h| h.starts_with("rename"))), "{:#?}", report.uncovered);
    let rewrite = vec![claim("c1", "rewrite-item", Some("demo::a::over"), serde_json::json!({"new": "demo::b::over", "note": "move it to the b module"}))];
    let report = run(MOVE_BEFORE, MOVE_AFTER, rewrite, &[], None);
    assert_eq!(status_of(&report, "c1"), &ClaimStatus::Empty);
    assert!(report.claims[0].hint.as_deref().is_some_and(|h| h.contains("Semilla derives the move or the rename")), "{:?}", report.claims[0].hint);
    let pair = semilla_core::PairWarning { old: "a::x::f".into(), new: "b::f".into(), similarity: 1.0, claims: vec![], refusal: None, reviewer: None, resolves: vec![], stopped: None };
    // Design 35: the text names no claim. Design 36.1: the text gives the cause when one is known.
    assert_eq!(pair.text(), "`a::x::f` (removed) and `b::f` (added) are 100% similar: they can be one item with a new place and a second change");
    let stopped = semilla_core::PairWarning { stopped: Some("the name and the module are new: Semilla derives a rename in the same module, or a move with the same name".into()), ..pair.clone() };
    assert!(stopped.text().ends_with("a second change. Semilla does not derive one item: the name and the module are new: Semilla derives a rename in the same module, or a move with the same name") && !stopped.text().contains("--claim"), "{}", stopped.text());
    let pair = semilla_core::PairWarning { old: "a::percent".into(), new: "a::pct".into(), similarity: 0.9, claims: vec![], refusal: None, reviewer: None, resolves: vec![], stopped: None };
    assert!(pair.text().starts_with("`a::percent` (removed) and `a::pct` (added) are 90% similar") && !pair.text().contains("--claim"), "{}", pair.text());
}

#[test]
fn a_type_fact_owns_its_derived_bodies_and_cross_crate_sites() {
    let before = "#[derive(Clone, Debug)]\npub struct A {\n    pub x: u32,\n}\npub fn make() -> A {\n    A { x: 1 }\n}\n";
    let after = before.replace("    pub x: u32,\n}", "    pub x: u32,\n    pub y: u8,\n}").replace("A { x: 1 }", "A { x: 1, y: 0 }");
    let bodies = vec![("demo::make", different(false), vec![]), ("<demo::A as std::clone::Clone>::clone", different(false), vec![]), ("<demo::A as std::fmt::Debug>::fmt", different(false), vec![])];
    let report = run_calls(before, &after, bodies);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    let affected = &report.facts[0].affected;
    assert!(affected.iter().any(|b| b.contains("Clone")) && affected.iter().any(|b| b.contains("Debug")), "{affected:?}");
    let mut sorted = affected.clone();
    sorted.dedup();
    assert_eq!(&sorted, affected, "no duplicates");
}

/// A body oracle whose callee evidence is in the external calls (a call to another crate).
struct ExternalCalls(Vec<(&'static str, BodyStatus, Vec<&'static str>)>);

impl BodyOracle for ExternalCalls {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.iter().map(|(name, status, _)| BodyComparison { name: name.to_string(), span: None, status: status.clone(), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }).collect())
    }
    fn facts(&self, _: &Step) -> anyhow::Result<Option<Vec<BodyFacts>>> {
        Ok(Some(self.0.iter().map(|(name, _, calls)| BodyFacts { name: name.to_string(), externals: calls.iter().map(|c| c.to_string()).collect(), ..Default::default() }).collect()))
    }
}

#[test]
fn a_call_site_in_another_crate_belongs_to_the_parameter_fact() {
    let (_d1, old) = load(CALLS);
    let (_d2, new) = load(&calls_after());
    let step = step(vec![]);
    let bodies = ExternalCalls(vec![("demo::f", different(false), vec![]), ("demo::g", different(false), vec!["demo::f"]), ("demo::h", different(false), vec!["demo::f"]), ("demo::k", different(false), vec!["demo::f"])]);
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &bodies, &NoOracle).report;
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
}

fn load_files(files: &[(&str, &str)]) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let ws = Workspace::load(dir.path()).unwrap();
    (dir, ws)
}

fn run_files(before: &[(&str, &str)], after: &[(&str, &str)], reasons: &[(&str, &str)]) -> StepReport {
    let (_d1, old) = load_files(before);
    let (_d2, new) = load_files(after);
    let mut step = step(vec![]);
    for (at, text) in reasons {
        step.reasons.insert(at.to_string(), text.to_string());
    }
    verify_step(&StepInput { step: &step, before: &old, after: &new }, &NoOracle, &NoOracle).report
}

#[test]
fn a_reason_with_no_text_is_not_a_reason() {
    // Brief 5f, A4: an empty value proves nothing.
    let lib = ("src/lib.rs", "pub fn f() {}\n");
    let manifest = ("Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n");
    for (text, accepted) in [("", false), ("  \n", false), ("the CI runs it", true)] {
        let report = run_files(&[manifest, lib], &[manifest, lib, ("ci.sh", "echo\n")], &[("ci.sh:", text)]);
        assert!(report.facts[0].spotlight, "{:#?}", report.facts);
        assert_eq!(report.accepted(), accepted, "reason {text:?}: {:#?}", report.facts);
        assert_eq!(report.facts[0].reason.is_some(), accepted);
    }
}

const MANIFEST_V0: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0.1\"\n";

#[test]
fn manifest_changes_are_facts_with_a_spotlight() {
    let lib = ("src/lib.rs", "pub fn f() {}\n");
    let facts = |manifest: &str| {
        let report = run_files(&[("Cargo.toml", MANIFEST_V0), lib], &[("Cargo.toml", manifest), lib], &[]);
        (report.facts.iter().map(|f| f.description.clone()).collect::<Vec<_>>(), report)
    };
    // A profile key: a fact in the spotlight, rejected until a reason exists.
    let profile = format!("{MANIFEST_V0}\n[profile.dev]\noverflow-checks = false\n");
    let (found, report) = facts(&profile);
    assert_eq!(found, ["Cargo.toml: profile.dev.overflow-checks added (false)"]);
    assert!(report.facts[0].spotlight && !report.accepted() && report.uncovered.is_empty(), "{:#?}", report);
    // A manifest key can change a body: phase 1 does not know, so the compare result is pending.
    assert_eq!(report.facts[0].compare, Compare::Pending);
    let with_reason = run_files(&[("Cargo.toml", MANIFEST_V0), lib], &[("Cargo.toml", &profile), lib], &[("Cargo.toml:profile.dev.overflow-checks", "the release build needs the speed")]);
    assert!(with_reason.accepted(), "{:#?}", with_reason);
    // A version change of an existing dependency: a plain fact.
    let (found, report) = facts(&MANIFEST_V0.replace("1.0.1", "1.0.2"));
    assert_eq!(found, ["Cargo.toml: dependencies.serde \"1.0.1\" → \"1.0.2\""]);
    assert!(!report.facts[0].spotlight && report.accepted());
    // A new dependency: in the spotlight.
    let (_, report) = facts(&format!("{MANIFEST_V0}rand = \"0.8\"\n"));
    assert!(report.facts[0].spotlight);
    // A format change only: one fact with no code.
    let (found, report) = facts(&MANIFEST_V0.replace("serde = \"1.0.1\"", "serde = \"1.0.1\" # the serde crate"));
    assert_eq!(found, ["Cargo.toml: format only"]);
    assert_eq!((report.facts[0].category, report.facts[0].compare), (Category::Manifest, Compare::NoCode));
    // A retired op.
    let report = run(PLAIN, PLAIN, vec![claim("c1", "edit-config", None, serde_json::json!({"file": "Cargo.toml"}))], &[], None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Malformed { reason } if reason.contains("retired")));
}

#[test]
fn other_files_are_file_facts() {
    let lib = ("src/lib.rs", "pub fn f() {}\n");
    let report = run_files(&[("Cargo.toml", MANIFEST_V0), lib, ("NOTES.txt", "a\n")], &[("Cargo.toml", MANIFEST_V0), lib, ("NOTES.txt", "b\n"), ("ci.yml", "x: 1\n")], &[]);
    let found: Vec<String> = report.facts.iter().map(|f| f.description.clone()).collect();
    assert_eq!(found, ["NOTES.txt: changed", "ci.yml: added"], "{:#?}", report.facts);
    // A1: a text file is on the allow list; a CI file is not, so it needs a reason.
    assert!(!report.facts[0].spotlight && report.facts[1].spotlight && !report.accepted() && report.uncovered.is_empty(), "{:#?}", report.facts);
    assert_eq!(report.facts[0].content.as_deref(), Some("- a\n+ b"));
    assert_eq!(report.facts[1].content.as_deref(), Some("+ x: 1"));
    // A build script is Rust code that the loader does not parse: a file fact in the spotlight.
    let report = run_files(&[("Cargo.toml", MANIFEST_V0), lib, ("build.rs", "fn main() {}\n")], &[("Cargo.toml", MANIFEST_V0), lib, ("build.rs", "fn main() { println!(\"x\"); }\n")], &[]);
    assert!(report.facts[0].spotlight && !report.accepted(), "{:#?}", report.facts);
}

#[test]
fn a_file_has_no_spotlight_only_on_the_allow_list() {
    // Trial 3, cheats A6e and A7: the legacy names `.cargo/config` and `rust-toolchain` are the
    // configuration of cargo and rustup in each folder. Each key needs a reason.
    let lib = ("src/lib.rs", "pub fn f() {}\n");
    let base = [("Cargo.toml", MANIFEST_V0), lib];
    let added = |extra: &[(&str, &str)]| {
        let after: Vec<(&str, &str)> = base.iter().copied().chain(extra.iter().copied()).collect();
        run_files(&base, &after, &[])
    };
    let report = added(&[(".cargo/config", "[target.x86_64-unknown-linux-gnu]\nrunner = \"true\"\n")]);
    assert_eq!(report.facts.len(), 1, "{:#?}", report.facts);
    assert_eq!(report.facts[0].description, ".cargo/config: target.x86_64-unknown-linux-gnu.runner added (\"true\")");
    assert!(report.facts[0].spotlight && !report.accepted());
    assert!(report.facts[0].content.as_deref().is_some_and(|c| c.contains("+ runner = \"true\"")), "{:?}", report.facts[0].content);
    for file in ["rust-toolchain", "crates/x/rust-toolchain"] {
        let report = added(&[(file, "beta\n")]);
        assert_eq!(report.facts[0].description, format!("{file}: toolchain.channel added (\"beta\")"));
        assert!(report.facts[0].spotlight && !report.accepted(), "{:#?}", report.facts);
    }
    let report = added(&[("sub/.cargo/config.toml", "[build]\nrustflags = [\"-Cdebug-assertions=off\"]\n")]);
    assert!(report.facts.iter().all(|f| f.spotlight) && !report.accepted(), "{:#?}", report.facts);
    // Each file outside the allow list needs a reason: a script, a lint setting, an unknown file.
    for file in ["scripts/test.sh", "clippy.toml", "rustfmt.toml", ".env", "link.x", ".gitignore", ".gitattributes", "data.bin"] {
        let report = added(&[(file, "x\n")]);
        assert!(report.facts[0].spotlight && !report.accepted(), "{file}: {:#?}", report.facts);
    }
    // Docs, license texts, and images need no reason, and the report shows the content.
    for file in ["README.md", "docs/notes.txt", "LICENSE-MIT", "CHANGELOG", "logo.png"] {
        let report = added(&[(file, "text\n")]);
        assert!(!report.facts[0].spotlight && report.accepted(), "{file}: {:#?}", report.facts);
        assert_eq!(report.facts[0].content.as_deref(), Some("+ text"));
    }
    // A doc file that a doc attribute includes has doc tests: it needs a reason.
    let doc_lib = ("src/lib.rs", "#![doc = include_str!(\"../README.md\")]\npub fn f() {}\n");
    let report = run_files(&[("Cargo.toml", MANIFEST_V0), doc_lib, ("README.md", "a\n")], &[("Cargo.toml", MANIFEST_V0), doc_lib, ("README.md", "b\n")], &[]);
    assert!(report.facts.iter().any(|f| f.description == "README.md: changed" && f.spotlight), "{:#?}", report.facts);
    // A binary file shows its size.
    assert_eq!(crate::files::content_change(None, None, None, Some(1234)), "binary, 1234 bytes");
    assert_eq!(crate::files::content_change(None, None, Some(10), Some(1234)), "binary, 10 bytes → binary, 1234 bytes");
    let long: String = (0..100).map(|i| format!("{i}\n")).collect();
    assert!(crate::files::content_change(None, Some(&long), None, Some(1)).ends_with("… 60 more line(s)"));
}

#[test]
fn add_arm_covers_a_new_arm() {
    let before = "pub enum E {\n    A,\n    B,\n    C,\n}\npub fn explain(e: &E) -> String {\n    match e {\n        E::A => \"a\".to_string(),\n        E::B => \"b\".to_string(),\n    }\n}\n";
    let before = before.replace("        E::B => \"b\".to_string(),\n", "        E::B => \"b\".to_string(),\n        _ => String::new(),\n");
    let after = before.replace("        _ => String::new(),\n", "        E::C => \"c\".to_string(),\n        _ => String::new(),\n");
    let report = run(&before, &after, vec![claim("c1", "add-arm", Some("demo::explain/S0"), serde_json::json!({}))], &[], None);
    assert!(status_of(&report, "c1").is_verified() && report.uncovered.is_empty(), "{:#?}", report);
    // A second change in another arm is not hidden by `add-arm`.
    let hidden = after.replace("\"a\".to_string()", "\"x\".to_string()");
    let report = run(&before, &hidden, vec![claim("c1", "add-arm", Some("demo::explain/S0"), serde_json::json!({}))], &[], None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Refuted { .. }), "{:?}", status_of(&report, "c1"));
    // `remove-arm` and `move-arm` are exact too.
    let report = run(&after, &before, vec![claim("c1", "remove-arm", Some("demo::explain/S0"), serde_json::json!({}))], &[], None);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    let moved = before.replace("        E::A => \"a\".to_string(),\n        E::B => \"b\".to_string(),\n", "        E::B => \"b\".to_string(),\n        E::A => \"a\".to_string(),\n");
    let report = run(&before, &moved, vec![claim("c1", "move-arm", Some("demo::explain/S0"), serde_json::json!({}))], &[], None);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    let report = run(&before, &moved, vec![claim("c1", "add-arm", Some("demo::explain/S0"), serde_json::json!({}))], &[], None);
    assert!(!status_of(&report, "c1").is_verified());
}

#[test]
fn a_const_is_not_a_free_variable_of_a_condition() {
    let src = "pub const LIMIT: u64 = 50_000;\npub fn over(a: u64) -> bool {\n    if a > LIMIT {\n        return true;\n    }\n    false\n}\n";
    let (_d, ws) = load(src);
    assert_eq!(checks_const(&ws, "a > LIMIT"), "a > 50_000");
}

#[test]
fn the_type_map_has_the_fields_of_a_struct_parameter() {
    // B3 (trial 3, D7): the SMT check needs the width of `account.cents`.
    let src = "pub struct Account {\n    pub cents: u64,\n    pub id: u32,\n}\npub fn over(account: &mut Account, cost: u64) -> bool {\n    cost > account.cents + 100\n}\n";
    let (_d, ws) = load(src);
    let types = checks::local_types(&ws, "demo::over");
    assert_eq!(types.get("account . cents").map(String::as_str), Some("u64"), "{types:?}");
    assert_eq!(types.get("account . id").map(String::as_str), Some("u32"), "{types:?}");
}

fn checks_const(ws: &Workspace, text: &str) -> String {
    checks::const_values_for_tests(ws, text)
}

#[test]
fn remove_if_on_an_if_expression() {
    let before = "pub fn f(c: bool, a: u32) -> u32 {\n    let fee = if c { 0 } else { a / 10 };\n    fee\n}\n";
    let after = before.replace("if c { 0 } else { a / 10 }", "a / 10");
    let claim_keep = |keep: &str| vec![claim("c1", "remove-if", Some("demo::f/S0/init"), serde_json::json!({"keep": keep}))];
    let report = run(before, &after, claim_keep("else"), &[], None);
    assert!(status_of(&report, "c1").is_verified() && report.uncovered.is_empty(), "{:#?}", report);
    assert!(matches!(status_of(&run(before, &after, claim_keep("then"), &[], None), "c1"), ClaimStatus::Refuted { .. }));
}

#[test]
fn a_variant_fact_owns_the_constructors_of_its_enum() {
    let before = "pub enum E {\n    A(u32),\n    B,\n}\n";
    let after = before.replace("    B,\n", "    B,\n    C,\n");
    let report = run_calls(before, &after, vec![("demo::E::A", different(false), vec![])]);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert_eq!(report.facts[0].affected, ["demo::E::A"]);
}

#[test]
fn a_variant_field_fact_owns_the_derived_impls_of_the_enum() {
    let before = "#[derive(Debug)]\npub enum E {\n    A { x: u32 },\n}\n";
    let after = before.replace("x: u32", "x: u64");
    let report = run_calls(before, &after, vec![("<demo::E as std::fmt::Debug>::fmt", different(false), vec![])]);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
}

#[test]
fn rewrite_pattern_with_a_crate_anchor_covers_all_items() {
    let before = "pub struct A {\n    pub b: u32,\n}\npub fn f(a: &A) -> u32 {\n    a.b + 1\n}\npub fn g(a: &A) -> u32 {\n    a.b * 2\n}\n";
    let after = before.replace("a.b + 1", "a.b.min(9) + 1").replace("a.b * 2", "a.b.min(9) * 2");
    let report = run(before, &after, vec![claim("c1", "rewrite-pattern", Some("demo"), serde_json::json!({"from": "$x.b", "to": "$x.b.min(9)"}))], &[], None);
    assert!(status_of(&report, "c1").is_verified() && report.uncovered.is_empty(), "{:#?}", report);
}

#[test]
fn a_const_value_is_a_fact_that_owns_its_users() {
    let before = "pub const BASE_FEE: u64 = 25;\npub const DOUBLE: u64 = BASE_FEE * 2;\npub fn fee() -> u64 {\n    BASE_FEE + 1\n}\npub fn other() -> u64 {\n    7\n}\n";
    let after = before.replace("= 25;", "= 250;");
    let bodies = vec![("demo::BASE_FEE", different(false), vec![]), ("demo::DOUBLE", different(false), vec![]), ("demo::fee", different(false), vec![]), ("demo::other", equal("n0"), vec![])];
    let report = run_calls(before, &after, bodies);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert_eq!(report.facts.len(), 1, "{:#?}", report.facts);
    assert_eq!(report.facts[0].description, "value of `demo::BASE_FEE` changed: 25 → 250 (the public API can change: a different crate can use the item)");
    assert_eq!(report.facts[0].affected, ["demo::BASE_FEE", "demo::DOUBLE", "demo::fee"]);
    assert_eq!(report.facts[0].compare, Compare::CodeDifferent);
    // In an identity step, the value fact gives "split the step".
    let renamed = after.replace("pub fn other", "pub fn another");
    let (_d1, old) = load(before);
    let (_d2, new) = load(&renamed);
    let step = step(vec![claim("c1", "rename", Some("demo::other"), serde_json::json!({"to": "another"}))]);
    let oracle = CallBodies(vec![("demo::BASE_FEE", different(false), vec![]), ("demo::DOUBLE", different(false), vec![]), ("demo::fee", different(false), vec![]), ("demo::another", equal("n0"), vec![])]);
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle).report;
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Refuted { reason } if reason.contains("the step has a second change")), "{:?}", status_of(&report, "c1"));
}

#[test]
fn uncovered_lines_have_correct_texts() {
    let mac = "macro_rules! m {\n    () => { 1 };\n}\n";
    let report = run(mac, &mac.replace("{ 1 }", "{ 2 }"), vec![], &[], None);
    assert!(!report.uncovered[0].description.starts_with("signature:"), "{}", report.uncovered[0].description);
    // An unproved identity claim owns the derived body.
    let before = "#[derive(Debug)]\npub struct A {\n    pub b: u32,\n}\npub fn g() -> u32 {\n    1\n}\n";
    let after = before.replace("pub b: u32", "pub c: u32").replace("    1\n", "    2\n");
    let claims = vec![claim("c1", "rename", Some("demo::A.b"), serde_json::json!({"to": "c"}))];
    let report = run(before, &after, claims, &[("<demo::A as std::fmt::Debug>::fmt", different(false)), ("demo::g", different(false))], None);
    assert!(!status_of(&report, "c1").is_verified());
    assert!(report.claims[0].affected.iter().any(|a| a.contains("Debug")), "{:#?}", report.claims);
}

#[test]
fn a_new_match_arm_gets_the_add_arm_suggestion() {
    let before = "pub fn f(n: u32) -> u32 {\n    match n {\n        0 => 1,\n        _ => 2,\n    }\n}\n";
    let after = before.replace("        _ => 2,\n", "        1 => 5,\n        _ => 2,\n");
    let report = run(before, &after, vec![], &[], None);
    assert_eq!(report.uncovered[0].hint.as_deref(), Some("add-arm"), "{:#?}", report.uncovered);
    let (_d1, old) = load(before);
    let (_d2, new) = load(&after);
    let edits = semilla_syntax::diff(&old, &new, &Default::default());
    let claims = hints::suggest_claims(&edits, &vec![None; edits.len()]);
    assert!(claims.iter().any(|c| c.op == "add-arm" && c.at.as_ref().is_some_and(|a| a.to_string() == "demo::f/S0")), "{claims:?}");
}

/// A body oracle with fixed comparisons (with spans).
struct SpanBodies(Vec<BodyComparison>);

impl BodyOracle for SpanBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.clone())
    }
}

#[test]
fn a_hand_written_impl_that_replaces_a_derive_is_not_neutral() {
    // T1 (trial 2, A4c): the derive of `PartialEq` is removed, and a new impl always returns true.
    let before = "#[derive(Debug, PartialEq)]\npub enum E {\n    A,\n    B,\n}\n";
    let after = "#[derive(Debug)]\npub enum E {\n    A,\n    B,\n}\nimpl PartialEq for E {\n    fn eq(&self, other: &Self) -> bool {\n        std::mem::discriminant(self) == std::mem::discriminant(other) || true\n    }\n}\n";
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(vec![]);
    let oracle = SpanBodies(vec![BodyComparison { name: "<demo::E as std::cmp::PartialEq>::eq".into(), span: Some(("src/lib.rs".into(), 8)), status: different(true), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }]);
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle).report;
    let imp = report.facts.iter().find(|f| f.part == "trait impl added").expect("an impl fact");
    assert_eq!(imp.compare, Compare::CodeDifferent, "{imp:#?}");
    assert!(!imp.affected.is_empty(), "{imp:#?}");
    assert!(imp.spotlight_why.as_deref().is_some_and(|w| w.contains("replaces the derive")), "{imp:#?}");
    assert!(!report.accepted());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    // A new impl with no MIR record of its body: the result is not `code equal`.
    let report = run(before, after, vec![], &[("demo::other", equal("n0"))], None);
    let imp = report.facts.iter().find(|f| f.part == "trait impl added").unwrap();
    assert_eq!(imp.compare, Compare::NoCompare, "{imp:#?}");
}

#[test]
fn each_twin_under_cfg_has_its_own_item_and_its_own_status() {
    // Batch 7b, part 4.4: two definitions of one name under `cfg` have one MIR name at two
    // places. The body at each place belongs to the item at that place.
    let (_d, ws) = load("#[cfg(unix)]\npub fn cut(a: u64) -> u64 {\n    a + 1\n}\n#[cfg(not(unix))]\npub fn cut(a: u64) -> u64 {\n    a\n}\n");
    let bodies = vec![body("demo::cut", "src/lib.rs", 2, different(false)), body("demo::cut", "src/lib.rs", 6, equal("n0"))];
    let index = checks::BodyIndex::new(&bodies, &ws, &ws);
    assert_eq!(index.item_of(&bodies[0]).as_deref(), Some("demo::cut#0"));
    assert_eq!(index.item_of(&bodies[1]).as_deref(), Some("demo::cut#1"));
    assert!(index.changed("demo::cut#0") && !index.changed("demo::cut#1"));
    // The MIR name is the name of the two twins: a twin that is not equal wins.
    assert!(index.changed("demo::cut"));
    let swapped = vec![bodies[1].clone(), bodies[0].clone()];
    let index = checks::BodyIndex::new(&swapped, &ws, &ws);
    assert!(index.changed("demo::cut") && index.changed("demo::cut#0") && !index.changed("demo::cut#1"));
}

#[test]
fn the_final_assertion_finds_a_changed_body_with_no_owner() {
    // Invariant I1, checked on a result that a defect in the verifier could make.
    let (_d, ws) = load("pub fn g() -> u32 {\n    1\n}\n");
    let step = step(vec![]);
    let bodies = vec![BodyComparison { name: "demo::g".into(), span: None, status: different(false), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }];
    let index = checks::BodyIndex::new(&bodies, &ws, &ws);
    let none = BTreeSet::new();
    let maps = semilla_syntax::BodyMaps::default();
    let input = owners::Input { maps: &maps, made: None, made_listed: &BTreeSet::new(), tests: None, step: &step, edits: &[], owners: &[], fact_of: &[], bodies: &bodies, body_index: &index, before: &ws, after: &ws, identity_owned: &none, identity_derived: &none, rename_targets: &none, identity_claim: None, identity_pairs: &[], callees: None, defs: &Default::default(), renames: &BTreeMap::new() };
    let mut report = StepReport::default();
    assert_eq!(owners::check(&input, &report), ["demo::g"]);
    // A fact that does not list the body is not an owner.
    report.facts.push(semilla_core::Fact { id: "f1".into(), part: "visibility".into(), category: Category::Visibility, compare: Compare::CodeEqual, ..Default::default() });
    assert_eq!(owners::check(&input, &report), ["demo::g"]);
    report.facts[0].affected = vec!["demo::g".into()];
    assert!(owners::check(&input, &report).is_empty());
    // The assignment always gives an owner.
    let mut report = StepReport::default();
    owners::assign(&input, &mut report);
    assert!(owners::check(&input, &report).is_empty(), "{:#?}", report.facts);
}

#[test]
fn attributes_use_an_allow_list() {
    let base = "pub fn f() -> u32 {\n    1\n}\n";
    let with = |attr: &str| format!("{attr}\npub fn f() -> u32 {{\n    1\n}}\n");
    let bodies = [("demo::f", equal("n0"))];
    for harmless in ["#[inline]", "#[allow(dead_code)]", "#[rustfmt::skip]", "#[clippy::cognitive_complexity = \"100\"]"] {
        let report = run(base, &with(harmless), vec![], &bodies, None);
        let fact = &report.facts[0];
        assert!(!fact.spotlight && fact.compare == Compare::CodeEqual, "{harmless}: {fact:#?}");
    }
    // A warning attribute (batch 6a, A1): it needs no reason. On an item that a different crate
    // can use, the text of the fact says so. The compiled code is equal.
    for warning in ["#[must_use]", "#[deprecated]"] {
        let report = run(base, &with(warning), vec![], &bodies, None);
        let fact = &report.facts[0];
        assert!(!fact.spotlight && (fact.category, fact.compare) == (Category::Attribute, Compare::CodeEqual), "{warning}: {fact:#?}");
        assert!(fact.description.ends_with("(a different crate that uses the item can get a different warning)"), "{warning}: {fact:#?}");
        let report = run(&base.replace("pub ", ""), &with(warning).replace("pub ", ""), vec![], &bodies, None);
        let fact = &report.facts[0];
        assert!(!fact.spotlight && fact.compare == Compare::CodeEqual, "{warning}: {fact:#?}");
    }
    // T5 (trial 2, A5d) and others: in the spotlight.
    for loud in ["#[ignore]", "#[should_panic]", "#[cfg(test)]", "#[cfg(any())]", "#[test]", "#[no_mangle]", "#[track_caller]", "#[my_macro::instrument]", "#[path = \"x.rs\"]"] {
        let report = run(base, &with(loud), vec![], &bodies, None);
        let fact = &report.facts[0];
        assert!(fact.spotlight && fact.category == Category::Attribute, "{loud}: {fact:#?}");
        assert!(!report.accepted(), "{loud}");
    }
}

#[test]
fn manifest_keys_use_an_allow_list() {
    let lib = ("src/lib.rs", "pub fn f() {}\n");
    let spotlight = |manifest: &str| {
        let report = run_files(&[("Cargo.toml", MANIFEST_V0), lib], &[("Cargo.toml", manifest), lib], &[]);
        assert!(!report.facts.is_empty(), "{manifest}");
        report.facts.iter().any(|f| f.spotlight)
    };
    assert!(!spotlight(&MANIFEST_V0.replace("version = \"0.1.0\"", "version = \"0.2.0\"")));
    assert!(!spotlight(&MANIFEST_V0.replace("[package]\n", "[package]\ndescription = \"a ledger\"\nlicense = \"MIT\"\n")));
    assert!(!spotlight(&MANIFEST_V0.replace("1.0.1", "1.0.2")));
    // T4 (trial 2, A3a): `default-members` decides which tests run.
    let workspace = |extra: &str| format!("{MANIFEST_V0}\n[workspace]\nmembers = [\".\"]\n{extra}");
    let (_d1, _) = ((), ());
    let report = run_files(&[("Cargo.toml", &workspace("")), lib], &[("Cargo.toml", &workspace("default-members = [\".\"]\n")), lib], &[]);
    assert!(report.facts.iter().any(|f| f.spotlight && f.description.contains("default-members")), "{:#?}", report.facts);
    let report = run_files(&[("Cargo.toml", &workspace("")), lib], &[("Cargo.toml", &workspace("exclude = [\"old\"]\n")), lib], &[]);
    assert!(report.facts.iter().all(|f| f.spotlight), "{:#?}", report.facts);
    // An unknown key, a path dependency, and `package.publish` are in the spotlight.
    assert!(spotlight(&MANIFEST_V0.replace("[package]\n", "[package]\npublish = false\n")));
    assert!(spotlight(&format!("{MANIFEST_V0}[badges]\nx = 1\n")));
    let path_dep = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = { path = \"../serde\", version = \"1.0.1\" }\n";
    let report = run_files(&[("Cargo.toml", path_dep), lib], &[("Cargo.toml", &path_dep.replace("1.0.1", "1.0.2")), lib], &[]);
    assert!(report.facts.iter().all(|f| f.spotlight), "{:#?}", report.facts);
}

fn run_spans(before: &[(&str, &str)], after: &[(&str, &str)], claims: Vec<Claim>, bodies: Vec<BodyComparison>, reasons: &[(&str, &str)]) -> StepReport {
    let (_d1, old) = load_files(before);
    let (_d2, new) = load_files(after);
    let mut step = step(claims);
    for (at, text) in reasons {
        step.reasons.insert(at.to_string(), text.to_string());
    }
    verify_step(&StepInput { step: &step, before: &old, after: &new }, &SpanBodies(bodies), &NoOracle).report
}

fn body(name: &str, file: &str, line: usize, status: BodyStatus) -> BodyComparison {
    BodyComparison { name: name.into(), span: Some((file.into(), line)), status, config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }
}

#[test]
fn a_build_input_owns_the_indirect_changes_in_the_crates_that_it_reaches() {
    // Brief 5f, B2 (trial 4, D13): a new `build.rs` or a changed `package.version` gave one fact
    // "indirect change, cause not known" for each body.
    let root = "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n";
    let manifest = |name: &str, more: &str| format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{more}").leak() as &str;
    let (a, b, c) = (manifest("a", ""), manifest("b", "\n[dependencies]\na = { path = \"../a\" }\n"), manifest("c", ""));
    let base = vec![
        ("Cargo.toml", root),
        ("crates/a/Cargo.toml", a),
        ("crates/a/src/lib.rs", "pub fn f() -> u32 {\n    1\n}\n"),
        ("crates/b/Cargo.toml", b),
        ("crates/b/src/lib.rs", "pub fn g() -> u32 {\n    a::f()\n}\n"),
        ("crates/c/Cargo.toml", c),
        ("crates/c/src/lib.rs", "pub fn h() -> u32 {\n    3\n}\n"),
    ];
    let bodies = || vec![body("a::f", "crates/a/src/lib.rs", 1, different(false)), body("b::g", "crates/b/src/lib.rs", 1, different(false)), body("c::h", "crates/c/src/lib.rs", 1, different(false))];
    let with = |path: &'static str, text: &'static str| -> Vec<(&str, &str)> { base.iter().copied().filter(|(known, _)| *known != path).chain([(path, text)]).collect() };
    let fact = |report: &StepReport, text: &str| report.facts.iter().find(|f| f.description.contains(text)).cloned().unwrap_or_else(|| panic!("no fact {text}: {:#?}", report.facts));

    // The version key of `a` reaches `a` and `b` (it depends on `a`), not `c`.
    let version = with("crates/a/Cargo.toml", manifest("a", "").replace("0.1.0", "0.2.0").leak());
    let report = run_spans(&base, &version, vec![], bodies(), &[]);
    let cause = fact(&report, "package.version");
    assert_eq!(cause.affected, ["a::f", "b::g"], "{:#?}", report.facts);
    assert!(cause.spotlight && cause.reason.is_none() && cause.compare == Compare::CodeDifferent, "{cause:#?}");
    assert!(cause.spotlight_why.as_deref().is_some_and(|why| why.contains("a build input: the MIR of 2 bodies changed") && !why.contains("one of these causes")), "{cause:#?}");
    assert_eq!(unknown_causes(&report), ["c::h"], "a crate that the input does not reach: {:#?}", report.facts);
    // One reason on the cause is sufficient for its bodies.
    let report = run_spans(&base, &version, vec![], bodies(), &[("crates/a/Cargo.toml:package.version", "the release 0.2.0"), ("c::h", "a new toolchain build")]);
    assert!(report.accepted(), "{:#?}", report.facts);
    // With no changed body, the version key needs no reason.
    let report = run_spans(&base, &version, vec![], vec![], &[]);
    assert!(report.accepted() && !fact(&report, "package.version").spotlight, "{:#?}", report.facts);

    // Brief 5h, B3 (trial 5, D6): a descriptive key that `env!` reads owns the body. It has the
    // same spotlight, and it needs a reason.
    let reads = "pub fn f() -> &'static str {\n    env!(\"CARGO_PKG_DESCRIPTION\")\n}\n";
    let described = |text: &str| manifest("a", &format!("description = \"{text}\"\n"));
    let old: Vec<(&str, &str)> = with("crates/a/Cargo.toml", described("1")).into_iter().filter(|(path, _)| *path != "crates/a/src/lib.rs").chain([("crates/a/src/lib.rs", reads)]).collect();
    let new: Vec<(&str, &str)> = old.iter().copied().filter(|(path, _)| *path != "crates/a/Cargo.toml").chain([("crates/a/Cargo.toml", described("The core"))]).collect();
    let report = run_spans(&old, &new, vec![], vec![body("a::f", "crates/a/src/lib.rs", 1, different(false))], &[]);
    let cause = fact(&report, "package.description");
    assert_eq!(cause.affected, ["a::f"], "{:#?}", report.facts);
    assert!(cause.spotlight && cause.reason.is_none() && !report.accepted(), "{cause:#?}");
    assert!(cause.spotlight_why.as_deref().is_some_and(|why| why.contains("a build input: the MIR of 1 body changed")), "{cause:#?}");
    let report = run_spans(&old, &new, vec![], vec![body("a::f", "crates/a/src/lib.rs", 1, different(false))], &[("crates/a/Cargo.toml:package.description", "the new text of the package")]);
    assert!(report.accepted(), "{:#?}", report.facts);
    // With no changed body, the description key needs no reason.
    let report = run_spans(&old, &new, vec![], vec![], &[]);
    assert!(report.accepted() && !fact(&report, "package.description").spotlight, "{:#?}", report.facts);

    // A new build script of `a`, and a profile key of the workspace (it reaches each crate).
    let mut both = with("crates/a/build.rs", "fn main() {}\n");
    both.retain(|(path, _)| *path != "Cargo.toml");
    both.push(("Cargo.toml", format!("{root}\n[profile.dev]\nopt-level = 1\n").leak()));
    let report = run_spans(&base, &both, vec![], bodies(), &[]);
    let (script, profile) = (fact(&report, "crates/a/build.rs: added"), fact(&report, "profile.dev.opt-level"));
    assert_eq!(script.affected, ["a::f", "b::g"], "{:#?}", report.facts);
    assert_eq!(profile.affected, ["a::f", "b::g", "c::h"], "{:#?}", report.facts);
    // Batch 5k, D10: the count of each cause agrees with its list. `c::h` has one possible
    // cause (the profile key), and the text of the profile key says so.
    for (cause, count, text) in [(&script, "the MIR of 2 bodies", ". The bodies have one of these causes: "), (&profile, "the MIR of 3 bodies", ". 2 of the bodies have one of these causes: ")] {
        assert!(cause.spotlight && cause.spotlight_why.as_deref().is_some_and(|why| why.contains(count) && why.contains(text) && why.contains("`crates/a/build.rs: added`") && why.contains("profile.dev.opt-level")), "{cause:#?}");
    }
    assert!(profile.spotlight_why.as_deref().is_some_and(|why| why.ends_with("This change is the only cause of the other 1")), "{profile:#?}");
    assert!(script.spotlight_why.as_deref().is_some_and(|why| !why.contains("the only cause")), "{script:#?}");
    assert!(unknown_causes(&report).is_empty() && !report.accepted(), "{:#?}", report.facts);
    let report = run_spans(&base, &both, vec![], bodies(), &[("crates/a/build.rs:", "the script sets no flag"), ("Cargo.toml:profile.dev.opt-level", "faster tests")]);
    assert!(report.accepted(), "{:#?}", report.facts);

    // A descriptive package key is not a build input.
    let described = with("crates/a/Cargo.toml", manifest("a", "description = \"the crate a\"\n"));
    let report = run_spans(&base, &described, vec![], bodies(), &[]);
    assert!(fact(&report, "package.description").affected.is_empty(), "{:#?}", report.facts);
    assert_eq!(unknown_causes(&report), ["a::f", "b::g", "c::h"]);
}

#[test]
fn an_included_file_owns_the_bodies_that_include_it() {
    // T3 (trial 2, A3b): a data file that `include_str!` reads. The file fact owns the const and
    // the function that names the const; no claim can take them.
    let lib = |n: &str| format!("pub const BASE: &str = include_str!(\"../data/base.txt\");\npub fn fee() -> usize {{\n    BASE.len() + {n}\n}}\npub fn other() -> u32 {{\n    1\n}}\n");
    let before = [("Cargo.toml", MANIFEST), ("src/lib.rs", lib("0").leak() as &str), ("data/base.txt", "25")];
    let after = [("Cargo.toml", MANIFEST), ("src/lib.rs", lib("0").leak() as &str), ("data/base.txt", "2500")];
    let bodies = vec![body("demo::BASE", "src/lib.rs", 1, different(false)), body("demo::fee", "src/lib.rs", 2, different(false)), body("demo::other", "src/lib.rs", 5, equal("n0"))];
    let report = run_spans(&before, &after, vec![], bodies.clone(), &[]);
    let file = report.facts.iter().find(|f| f.file.as_deref() == Some("data/base.txt")).expect("a file fact");
    assert_eq!(file.affected, ["demo::BASE", "demo::fee"], "{:#?}", report.facts);
    assert_ne!(file.compare, Compare::CodeEqual);
    assert!(unknown_causes(&report).is_empty(), "{:#?}", report.facts);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    // `affects` on a doc claim cannot take the bodies (it is retired).
    let docs = after.iter().map(|(p, t)| (*p, if *p == "src/lib.rs" { format!("/// The fee.\n{t}").leak() as &str } else { *t })).collect::<Vec<_>>();
    let claims = vec![claim("c1", "edit-doc", Some("demo::BASE"), serde_json::json!({"affects": ["demo::fee"]}))];
    let report = run_spans(&before, &docs, claims, bodies, &[]);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Malformed { .. }));
}

#[test]
fn a_body_from_a_file_that_semilla_does_not_parse_is_in_the_spotlight() {
    // Invariant I4: `include!` of Rust code. The compiler reads the function from `gen.rs`.
    let lib = "include!(\"gen.rs\");\npub fn other() -> u32 {\n    1\n}\n";
    let before = [("Cargo.toml", MANIFEST), ("src/lib.rs", lib), ("src/gen.rs", "pub fn hidden() -> u32 {\n    1\n}\n")];
    let after = [("Cargo.toml", MANIFEST), ("src/lib.rs", lib), ("src/gen.rs", "pub fn hidden() -> u32 {\n    2\n}\n")];
    let report = run_spans(&before, &after, vec![], vec![body("demo::hidden", "src/gen.rs", 1, different(false))], &[]);
    let source = report.facts.iter().find(|f| f.part == "source file").expect("a source file fact");
    assert!(source.spotlight && source.spotlight_why.as_deref().is_some_and(|w| w.contains("does not parse")), "{source:#?}");
    assert!(!report.accepted());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn a_changed_macro_owns_the_bodies_that_expand_it() {
    let before = "macro_rules! fee {\n    () => { 1 };\n}\npub fn f() -> u32 {\n    fee!()\n}\npub fn g() -> u32 {\n    2\n}\n";
    let after = before.replace("{ 1 }", "{ 5 }");
    let claims = vec![claim("c1", "change-signature", Some("demo::fee!"), serde_json::json!({}))];
    let bodies = vec![body("demo::f", "src/lib.rs", 4, different(false)), body("demo::g", "src/lib.rs", 7, different(false))];
    let report = run_spans(&[("Cargo.toml", MANIFEST), ("src/lib.rs", before)], &[("Cargo.toml", MANIFEST), ("src/lib.rs", &after)], claims, bodies, &[]);
    assert_eq!(report.claims[0].affected, ["demo::f"], "{:#?}", report);
    // `g` does not expand the macro: no known cause.
    assert_eq!(unknown_causes(&report), ["demo::g"]);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn a_new_drop_impl_is_in_the_spotlight() {
    // Probe 7 of batch 5a: the MIR of the callers does not show a new `Drop`.
    let before = "pub struct S {\n    pub v: Vec<u32>,\n}\n";
    let after = format!("{before}impl Drop for S {{\n    fn drop(&mut self) {{\n        self.v.clear();\n    }}\n}}\n");
    let report = run(before, &after, vec![], &[], None);
    let imp = report.facts.iter().find(|f| f.part == "trait impl added").unwrap();
    assert!(imp.spotlight_why.as_deref().is_some_and(|w| w.contains("implicit call")), "{imp:#?}");
    // With a claim instead of the fact, the step spotlight still names it.
    let report = run(before, &after, vec![claim("c1", "add-item", Some("demo::impl <S as Drop>"), serde_json::json!({}))], &[], None);
    assert!(report.spotlight.iter().any(|s| s.starts_with("implicit call")), "{:?}", report.spotlight);
}

#[test]
fn a_new_callee_and_a_changed_proc_macro_own_the_changed_bodies() {
    // Probe 5: a new inherent method that method selection now picks. The MIR callee list of the
    // caller names the new method, so the edit that adds it owns the caller.
    let before = "pub struct S;\npub trait T {\n    fn label(&self) -> u32 {\n        1\n    }\n}\nimpl T for S {}\npub fn user(s: &S) -> u32 {\n    s.label()\n}\n";
    let after = format!("{before}impl S {{\n    pub fn label(&self) -> u32 {{\n        2\n    }}\n}}\n");
    let claims = vec![claim("c1", "add-item", Some("demo::impl S"), serde_json::json!({})), claim("c2", "add-item", Some("demo::S::label"), serde_json::json!({}))];
    let (_d1, old) = load(before);
    let (_d2, new) = load(&after);
    let step = step(claims);
    let oracle = CallBodies(vec![("demo::user", different(false), vec!["demo::S::label"])]);
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle).report;
    assert!(unknown_causes(&report).is_empty(), "{:#?}", report.facts);
    assert!(report.claims.iter().any(|c| c.affected.contains(&"demo::user".to_string())), "{:#?}", report.claims);
    assert!(report.errors.is_empty() && report.accepted(), "{report:#?}");
    // Probe 3: a changed proc macro (a function with `#[proc_macro]`) that the body expands.
    let pm = |n: &str| format!("#[proc_macro]\npub fn rate(_input: TokenStream) -> TokenStream {{\n    \"{n}\".parse().unwrap()\n}}\npub fn user() -> u32 {{\n    rate!()\n}}\n");
    let claims = vec![claim("c1", "change-literal", Some("demo::rate/tail/recv/recv"), serde_json::json!({}))];
    let report = run(&pm("1"), &pm("5"), claims, &[("demo::user", different(false))], None);
    assert!(unknown_causes(&report).is_empty(), "{:#?}", report.facts);
    assert_eq!(report.claims[0].affected, ["demo::user"], "{:#?}", report.claims);
}

#[test]
fn each_edit_gets_a_derived_fact() {
    // Design 24.2: no claim, no uncovered edit.
    let derive = |bodies: &[(&str, BodyStatus)]| {
        let (_d1, old) = load(BEFORE);
        let (_d2, new) = load(AFTER);
        let step = step(vec![]);
        let oracle = FakeBodies(bodies.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
        verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &oracle, &FakeExprs(Effect::Widen), true).report
    };
    // Phase 1: the class is pending (`None`), never neutral.
    let report = derive(&[]);
    assert!(report.uncovered.is_empty() && report.errors.is_empty(), "{report:#?}");
    let texts: Vec<&str> = report.facts.iter().map(|f| f.description.as_str()).collect();
    assert!(texts.iter().any(|t| t.starts_with("added early exit `if v.is_empty()")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("changed operator `*x < max` → `*x <= max` (effect hint: widen, example: i = 5, n = 5)")), "{texts:?}");
    assert!(report.facts.iter().all(|f| f.compare == Compare::Pending), "{:#?}", report.facts);
    // Phase 2: a changed body gives the class of the op.
    let report = derive(&[("demo::total", different(true))]);
    assert!(report.facts.iter().all(|f| f.category == Category::Semantics), "{:#?}", report.facts);
    assert!(report.accepted(), "{report:#?}");
    // An equal body hides no edit (design 35): each edit is a fact with `code equal`.
    let count = report.facts.len();
    let report = derive(&[("demo::total", equal("n0"))]);
    assert_eq!(report.facts.len(), count, "{:#?}", report.facts);
    assert!(report.facts.iter().all(|f| f.compare == Compare::CodeEqual && !f.description.contains("neutral")), "{:#?}", report.facts);
    // An equal result above N1 is not a result of the compare.
    let report = derive(&[("demo::total", equal("n2"))]);
    assert!(report.facts.iter().all(|f| f.compare == Compare::CodeDifferent), "{:#?}", report.facts);
}

/// A step check with the new protocol (design 24): derived facts and a derived identity.
fn run_derived(before: &str, after: &str, bodies: &[(&str, BodyStatus)]) -> StepReport {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(vec![]);
    let oracle = FakeBodies(bodies.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
    verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle, true).report
}

fn descriptions(report: &StepReport) -> Vec<String> {
    report.facts.iter().map(|f| f.description.clone()).collect()
}

#[test]
fn a_doc_change_and_a_parameter_rename_do_not_stop_an_identity() {
    // B5: `sed s/\bbalance\b/cents/` changes the field, a doc line, and a parameter name.
    let before = "#[derive(Debug)]\npub struct Account {\n    pub balance: u32,\n}\n/// Returns the balance.\npub fn get(a: &Account) -> u32 {\n    a.balance\n}\npub fn make(balance: u32) -> Account {\n    Account { balance: balance }\n}\n";
    let after = before.replace("balance", "cents");
    let debug = "<demo::Account as std::fmt::Debug>::fmt";
    let bodies = [("demo::get", equal("n0")), ("demo::make", equal("n0")), (debug, different(false))];
    let report = run_derived(before, &after, &bodies);
    assert!(report.accepted() && report.claims.is_empty(), "{report:#?}");
    let texts = descriptions(&report);
    assert_eq!(texts[0], "renamed `demo::Account.balance` → `demo::Account.cents`");
    // The derived `Debug` body shows the name: the rename has the mismatch mark (design 35).
    assert!(report.facts[0].mismatch() && report.facts[0].affected == [debug], "{:#?}", report.facts[0]);
    assert!(report.facts.iter().any(|f| f.description == "parameter `balance` of `demo::make` renamed to `cents`: `u32`" && f.compare == Compare::CodeEqual), "{texts:?}");
    assert!(report.facts.iter().any(|f| f.description.starts_with("changed docs") && f.compare == Compare::NoCode), "{texts:?}");
    // The field is a part of the public API: the rename has one more fact to read (design 31.8).
    assert_eq!(texts[4], "public API: `demo::Account.balance` is now `demo::Account.cents` (a different crate can no longer use the old path)", "{texts:?}");
    // The use of the parameter in the body is a local rename fact (design 36.1, rule 7).
    assert!(report.facts.iter().any(|f| f.description == "`demo::make`: renamed local `balance` → `cents`, 1 edit(s)" && f.category == Category::Rename && f.compare == Compare::CodeEqual), "{texts:?}");
    assert_eq!(report.facts.len(), 5, "{texts:?}");
    // C1: a new module doc and a new inner doc attribute have the class `other`.
    for doc in ["//! The demo crate.\n", "#![doc = \"The demo crate.\"]\n"] {
        let report = run_derived(before, &format!("{doc}{before}"), &[("demo::get", equal("n0")), ("demo::make", equal("n0"))]);
        assert!(report.accepted() && !report.facts.is_empty() && report.facts.iter().all(|f| f.compare == Compare::NoCode), "{doc}: {:#?}", report.facts);
    }
    // The same step and a second edit (design 36.1): the rename stays, and the edit is a fact
    // with its body.
    let changed = after.replace("a.cents\n", "a.cents + 1\n");
    let bodies = [("demo::get", different(false)), ("demo::make", equal("n0")), (debug, different(false))];
    let report = run_derived(before, &changed, &bodies);
    assert_eq!(report.facts[0].description, "renamed `demo::Account.balance` → `demo::Account.cents`");
    assert!(report.facts[0].affected == [debug] && report.facts.iter().any(|f| f.description == "changed expression `a.balance` → `a.cents + 1` at `demo::get/tail`" && f.compare == Compare::CodeDifferent && f.affected == ["demo::get"]), "{report:#?}");
    // A parameter rename in a body with a second edit: the parameter fact, and the edit fact
    // with the body.
    let changed = after.replace("Account { cents: cents }", "Account { cents: cents + 1 }");
    let bodies = [("demo::get", equal("n0")), ("demo::make", different(false)), (debug, different(false))];
    let report = run_derived(before, &changed, &bodies);
    assert!(report.facts.iter().any(|f| f.description == "parameter `balance` of `demo::make` renamed to `cents`: `u32`" && f.compare == Compare::CodeEqual && f.affected.is_empty()), "{report:#?}");
    assert!(report.facts.iter().any(|f| f.description == "changed struct: `Account { balance: balance }` → `Account { cents: cents + 1 }` at `demo::make/tail`" && f.compare == Compare::CodeDifferent && f.affected == ["demo::make"]), "{report:#?}");
}

#[test]
fn a_new_type_gives_one_fact() {
    // Brief 5f, C1 (trial 4, D6): a new tuple struct gave "the tuple fields of `Label` changed:
    // `()` → `(u64)`", and a new struct or enum gave "member added of `T`".
    let before = "pub enum Kind {\n    A,\n}\npub fn f() -> u32 {\n    1\n}\n";
    let types = "pub struct Label(pub u64);\npub struct Shown {\n    pub id: u64,\n}\npub enum Mode {\n    Fast,\n    Slow(u8),\n    Named { a: u8 },\n}\n";
    let report = run_derived(before, &format!("{before}{types}"), &[("demo::f", equal("n0"))]);
    assert_eq!(descriptions(&report), ["added `demo::Label` (the public API changes: a different crate can now use the item)", "added `demo::Mode` (the public API changes: a different crate can now use the item)", "added `demo::Shown` (the public API changes: a different crate can now use the item)"], "{:#?}", report.facts);
    assert!(report.uncovered.is_empty() && report.accepted(), "{report:#?}");
    // The removal is the same.
    let report = run_derived(&format!("{before}{types}"), before, &[("demo::f", equal("n0"))]);
    assert_eq!(descriptions(&report), ["removed `demo::Label` (the public API changes: a different crate can no longer use the item)", "removed `demo::Mode` (the public API changes: a different crate can no longer use the item)", "removed `demo::Shown` (the public API changes: a different crate can no longer use the item)"], "{:#?}", report.facts);
    // A new tuple variant of an old enum: the variant fact has the fields. No tuple fact.
    let report = run_derived(before, &before.replace("    A,\n", "    A,\n    B(u8, u16),\n"), &[("demo::f", equal("n0"))]);
    // A different crate can use the enum: the new variant can change the public API (batch 6a, A1).
    assert_eq!(descriptions(&report), ["variant `demo::Kind::B` added: `B(u8, u16)` (the public API can change: a different crate can use the item)"], "{:#?}", report.facts);
    assert!(report.uncovered.is_empty(), "{report:#?}");
    // A changed tuple of an old type keeps its fact.
    let old = format!("{before}pub struct Pair(pub u32);\n");
    let report = run_derived(&old, &old.replace("Pair(pub u32)", "Pair(pub u32, pub u64)"), &[("demo::f", equal("n0"))]);
    assert_eq!(descriptions(&report), ["the tuple fields of `demo::Pair` changed: `(u32)` → `(u32, u64)` (the public API can change: a different crate can use the item)"], "{:#?}", report.facts);
}

#[test]
fn one_expression_edit_gives_one_fact() {
    // Brief 5f, C2 (trial 4, D7): `a + b` → `a + b + c()` gave the facts "`a` → `a + b`" and
    // "`b` → `c()`", and `a * b / 100` → `a / 100 * b` gave four facts that overlap.
    let source = |tail: &str| format!("pub fn c() -> u32 {{\n    1\n}}\npub fn f(a: u32, b: u32, x: u32) -> u32 {{\n    {tail}\n}}\npub fn g(a: u32, b: u32, x: u32) -> u32 {{\n    if a > 0 && b > 0 {{\n        return 1;\n    }}\n    x\n}}\n");
    let facts = |old: &str, new: &str| descriptions(&run_derived(&source(old), &source(new), &[("demo::f", different(false)), ("demo::g", equal("n0")), ("demo::c", equal("n0"))]));
    assert_eq!(facts("a + b", "a + b + c()"), ["added operand `+ c()` at `demo::f/tail`"]);
    assert_eq!(facts("a + b", "x + a + b"), ["added operand `x +` at `demo::f/tail`"]);
    assert_eq!(facts("a + b + x", "a + x"), ["removed operand `+ b` at `demo::f/tail`"]);
    assert_eq!(facts("a + b", "a + c() + b + x"), ["added operands `+ c()`, `+ x` at `demo::f/tail`"]);
    // A changed operand and a new operand: the full expression is the change.
    assert_eq!(facts("a + b", "x + b + c()"), ["changed expression `a + b` → `x + b + c()` at `demo::f/tail`"]);
    // The right side does not continue a chain: `a - (b - x)` is not `a - b - x`.
    assert_eq!(facts("a - b", "a - (b - x)"), ["changed expression `b` → `b - x` at `demo::f/tail/rhs`"]);
    // An operator and an operand change: one fact, not one for each node.
    assert_eq!(facts("a * b / 100", "a / 100 * b"), ["changed expression `a * b / 100` → `a / 100 * b` at `demo::f/tail`"]);
    // Two operands change, and no operator changes: two facts that do not overlap.
    assert_eq!(facts("a + b", "x + c()").len(), 2);
    // A condition: the fact names the new operand.
    let before = source("a");
    let report = run_derived(&before, &before.replace("a > 0 && b > 0", "a > 0 && b > 0 && x > 0"), &[("demo::f", equal("n0")), ("demo::g", different(true)), ("demo::c", equal("n0"))]);
    assert_eq!(report.facts.len(), 1, "{:#?}", report.facts);
    assert!(report.facts[0].description.starts_with("added operand `&& x > 0`") && report.facts[0].category == Category::Semantics, "{:#?}", report.facts);
}

#[test]
fn the_note_of_a_pair_with_no_proof_names_the_difference_of_the_pair() {
    // Brief 5f, C3 (trial 4, D8): the note of a test rename with a changed literal named the
    // closure of another function.
    let before = "pub fn a_small_amount_has_no_fee() -> u32 {\n    let amount = 200;\n    amount + 1\n}\npub fn other(a: u32) -> u32 {\n    a + 1\n}\n";
    let after = before.replace("a_small_amount_has_no_fee", "a_small_amount_pays_the_fee").replace("200", "170").replace("a + 1", "a + 2");
    let bodies = [("demo::a_small_amount_pays_the_fee", different(false)), ("demo::other", different(false))];
    let report = run_derived(before, &after, &bodies);
    // Design 36.1: the rename is one fact, and each literal is a fact with its own body.
    let texts = descriptions(&report);
    assert_eq!(texts[..3], ["renamed `demo::a_small_amount_has_no_fee` → `demo::a_small_amount_pays_the_fee`", "changed literal `200` → `170` at `demo::a_small_amount_pays_the_fee/S0/init`", "changed literal `1` → `2` at `demo::other/tail/rhs`"], "{texts:?}");
    assert!(report.facts[0].compare == Compare::CodeEqual && report.facts[1].affected == ["demo::a_small_amount_pays_the_fee"] && report.facts[2].affected == ["demo::other"] && report.notes.is_empty(), "{report:#?}");
    // A pair with no difference, and a second change in a different function.
    let after = before.replace("a_small_amount_has_no_fee", "a_small_amount_pays_the_fee").replace("a + 1", "a + 2");
    let bodies = [("demo::a_small_amount_pays_the_fee", equal("n0")), ("demo::other", different(false))];
    let report = run_derived(before, &after, &bodies);
    let texts = descriptions(&report);
    assert_eq!(texts[..2], ["renamed `demo::a_small_amount_has_no_fee` → `demo::a_small_amount_pays_the_fee`", "changed literal `1` → `2` at `demo::other/tail/rhs`"], "{texts:?}");
    assert!(report.facts[0].compare == Compare::CodeEqual && report.facts[1].compare == Compare::CodeDifferent && report.notes.is_empty(), "{report:#?}");
}

#[test]
fn a_variant_rename_with_a_derived_debug_is_a_derived_identity() {
    // Brief 5f, C4 (trial 4, D11): the note said that the derived `Debug` impl of the enum "is
    // not a derived body". The cause: the type of a variant claim was the variant, not the enum.
    let before = "#[derive(Debug, PartialEq)]\npub enum LedgerError {\n    Frozen,\n    OverLimit { max: u32 },\n}\npub fn check(frozen: bool) -> Result<u32, LedgerError> {\n    if frozen {\n        return Err(LedgerError::Frozen);\n    }\n    Ok(1)\n}\n";
    let debug = "<demo::LedgerError as std::fmt::Debug>::fmt";
    let bodies = [("demo::check", equal("n0")), (debug, different(false)), ("<demo::LedgerError as std::cmp::PartialEq>::eq", equal("n0"))];
    let report = run_derived(before, &before.replace("Frozen", "Locked"), &bodies);
    assert!(report.accepted() && report.notes.is_empty(), "{report:#?}");
    // Two facts: the rename, and the fact for the public API (design 31.8).
    assert_eq!(report.facts.len(), 2, "{:#?}", report.facts);
    assert_eq!(report.facts[1].description, "public API: `demo::LedgerError::Frozen` is now `demo::LedgerError::Locked` (a different crate can no longer use the old path)");
    let fact = &report.facts[0];
    assert_eq!(fact.description, "renamed `demo::LedgerError::Frozen` → `demo::LedgerError::Locked`");
    // The derived body that shows the name is the body of the fact.
    assert!(fact.category == Category::Rename && fact.mismatch() && fact.affected == [debug], "{fact:#?}");
    // A field of a variant: the same rule.
    let report = run_derived(before, &before.replace("max", "limit"), &bodies);
    assert!(report.accepted() && report.facts.len() == 2 && report.facts[0].affected == [debug], "{report:#?}");
    assert_eq!(report.facts[1].description, "public API: `demo::LedgerError::OverLimit.max` is now `demo::LedgerError::OverLimit.limit` (a different crate can no longer use the old path)");
    // A changed body that is not derived and has no edit fact (design 36.1, rule 4): it is a
    // body of the rename fact, with the mismatch mark.
    let other = [("demo::check", different(false)), (debug, different(false))];
    let report = run_derived(before, &before.replace("Frozen", "Locked"), &other);
    let fact = &report.facts[0];
    assert!(fact.description.starts_with("renamed") && fact.compare == Compare::CodeDifferent && fact.mismatch() && fact.affected == [debug, "demo::check"], "{fact:#?}");
}

#[test]
fn a_short_fact_text_shows_the_difference() {
    // C4 (trial 3, D10): the two sides of a cut text were the same text.
    let before = "pub fn text(a: u32) -> &'static str {\n    match a {\n        0 => \"there is not enough money in the account for this withdrawal\",\n        _ => \"the account is frozen and cannot pay\",\n    }\n}\n";
    let after = before.replace("not enough money in the account for this withdrawal", "not enough money in the account for this deposit");
    let report = run_derived(before, &after, &[("demo::text", different(false))]);
    let fact = report.facts.iter().find(|f| f.part == "edit").unwrap_or_else(|| panic!("{:#?}", report.facts));
    let (old, new) = fact.description.split_once(" → ").unwrap();
    assert!(old.contains("withdrawal") && new.contains("deposit"), "{}", fact.description);
    assert!(fact.content.as_deref().is_some_and(|c| c.contains("- 0 => \"there is not enough money in the account for this withdrawal\"") || c.contains("withdrawal\"")), "{:?}", fact.content);
}

#[test]
fn a_rename_with_no_claim_gets_a_derived_proof() {
    let before = "#[derive(Debug, Clone)]\npub struct Account {\n    pub balance: u32,\n}\npub fn get(a: &Account) -> u32 {\n    a.balance\n}\n";
    let after = before.replace("pub balance: u32", "pub cents: u32").replace("a.balance", "a.cents");
    let debug = "<demo::Account as std::fmt::Debug>::fmt";
    let bodies = [("demo::get", equal("n0")), (debug, different(false)), ("<demo::Account as std::clone::Clone>::clone", equal("n0"))];
    let report = run_derived(before, &after, &bodies);
    assert!(report.accepted(), "{report:#?}");
    assert!(report.claims.is_empty(), "{:#?}", report.claims);
    // One identity fact: the forced edits fold under it, and the derived body rule applies.
    // The field is a part of the public API: the second fact says so (design 31.8).
    assert_eq!(report.facts.len(), 2, "{:#?}", report.facts);
    assert_eq!(report.facts[1].description, "public API: `demo::Account.balance` is now `demo::Account.cents` (a different crate can no longer use the old path)");
    let fact = &report.facts[0];
    assert_eq!(fact.description, "renamed `demo::Account.balance` → `demo::Account.cents`");
    assert!(fact.mismatch() && fact.affected == [debug], "{fact:#?}");
    // Phase 1: no compare yet, a note, and the flag that makes `step` run phase 2.
    let report = run_derived(before, &after, &[]);
    assert!(report.identity_candidates && report.notes.iter().any(|n| n.contains("phase 2 tries the derivation")), "{report:#?}");
    assert!(!descriptions(&report).iter().any(|d| d.starts_with("renamed")));
    // A derived body of another trait than `Debug`: the fact needs a reason, as a claim does.
    let hashed = before.replace("Debug, Clone", "Debug, Clone, Hash");
    let mut with_hash = bodies.to_vec();
    with_hash.push(("<demo::Account as std::hash::Hash>::hash", different(false)));
    let report = run_derived(&hashed, &hashed.replace("pub balance: u32", "pub cents: u32").replace("a.balance", "a.cents"), &with_hash);
    assert!(!report.accepted() && report.facts[0].spotlight && report.facts[0].spotlight_why.as_deref().is_some_and(|w| w.contains("Hash for demo::Account")), "{report:#?}");
}

#[test]
fn a_hidden_change_next_to_a_derived_identity_is_shown() {
    // Design 36.1: a rename and a changed literal in the same body. The rename is one fact, and
    // the literal is a second fact with the changed body.
    let before = "#[derive(Debug)]\npub struct Account {\n    pub balance: u32,\n}\npub fn get(a: &Account) -> u32 {\n    a.balance + 1\n}\n";
    let after = before.replace("pub balance: u32", "pub cents: u32").replace("a.balance + 1", "a.cents + 2");
    let debug = "<demo::Account as std::fmt::Debug>::fmt";
    let report = run_derived(before, &after, &[("demo::get", different(false)), (debug, different(false))]);
    let texts = descriptions(&report);
    assert_eq!(texts[0], "renamed `demo::Account.balance` → `demo::Account.cents`", "{texts:?}");
    // The body with the literal is not a body of the rename fact.
    assert!(report.facts[0].category == Category::Rename && report.facts[0].affected == [debug], "{:#?}", report.facts[0]);
    assert!(report.facts.iter().any(|f| f.description == "changed literal `1` → `2` at `demo::get/tail/rhs`" && f.compare == Compare::CodeDifferent && f.affected == ["demo::get"]), "{:#?}", report.facts);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    // A move: the derived fact with no claim.
    let bodies = [("demo::b::over", equal("n0")), ("demo::b::run", equal("n1"))];
    let report = run_derived(MOVE_BEFORE, MOVE_AFTER, &bodies);
    assert!(descriptions(&report).iter().any(|d| d.starts_with("moved `demo::a::over` → `demo::b::over`")), "{report:#?}");
    // A move and a dropped `pub`: the move is one fact, and the visibility is a second fact.
    let narrow = MOVE_AFTER.replace("    pub fn over", "    fn over");
    let report = run_derived(MOVE_BEFORE, &narrow, &bodies);
    let texts = descriptions(&report);
    assert_eq!(texts, ["moved `demo::a::over` → `demo::b::over`, with 2 `use` line(s)", "visibility of `demo::b::over`: `pub` → `private` (the public API changes: a different crate can no longer use the item)"]);
    assert!(report.facts[0].compare == Compare::CodeEqual && report.facts[1].category == Category::Visibility && report.notes.is_empty(), "{report:#?}");
    // A move and a body change: the edit is a fact at the new path, with the changed body.
    let changed = MOVE_AFTER.replace("x > LIMIT", "x >= LIMIT");
    let report = run_derived(MOVE_BEFORE, &changed, &[("demo::b::over", different(false)), ("demo::b::run", equal("n1"))]);
    let moved = report.facts.iter().find(|f| f.description.starts_with("moved `demo::a::over` → `demo::b::over`")).unwrap();
    assert!(moved.compare == Compare::CodeEqual && moved.affected.is_empty() && !moved.mismatch(), "{moved:#?}");
    assert!(report.facts.iter().any(|f| f.description == "changed operator `x > LIMIT` → `x >= LIMIT` at `demo::b::over/tail`" && f.category == Category::Semantics && f.compare == Compare::CodeDifferent && f.affected == ["demo::b::over"]), "{:#?}", report.facts);
    assert!(!descriptions(&report).iter().any(|d| d.starts_with("added `") || d.starts_with("removed `")), "{:?}", descriptions(&report));
    // A function rename and a changed literal in its body.
    let before = "pub fn percent(a: u64) -> u64 {\n    a / 100\n}\npub fn charge(a: u64) -> u64 {\n    percent(a) + 1\n}\n";
    let after = before.replace("percent", "pct").replace("a / 100", "a / 10");
    let report = run_derived(before, &after, &[("demo::pct", different(false)), ("demo::charge", equal("n0"))]);
    let renamed = report.facts.iter().find(|f| f.description == "renamed `demo::percent` → `demo::pct`").unwrap();
    assert!(renamed.compare == Compare::CodeEqual && !renamed.mismatch(), "{renamed:#?}");
    assert!(report.facts.iter().any(|f| f.description == "changed literal `100` → `10` at `demo::pct/tail/rhs`" && f.compare == Compare::CodeDifferent && f.affected == ["demo::pct"]), "{:#?}", report.facts);
    // Rule 4: the body of the renamed item changes, and no edit explains it. The rename fact
    // has the result `code different` and the mismatch mark.
    let after = before.replace("percent", "pct");
    let report = run_derived(before, &after, &[("demo::pct", different(false)), ("demo::charge", equal("n0"))]);
    let renamed = report.facts.iter().find(|f| f.description == "renamed `demo::percent` → `demo::pct`").unwrap();
    assert!(renamed.compare == Compare::CodeDifferent && renamed.mismatch() && renamed.affected == ["demo::pct"], "{renamed:#?}");
}

#[test]
fn the_identity_search_has_limits() {
    // Two fields of the same type change their names. The two maps have the same score: the wrong
    // map leaves a change of the field order, so the map with no other fact wins.
    let before = "pub struct P {\n    pub a: u32,\n    pub b: u32,\n}\npub fn f(p: &P) -> u32 {\n    p.a - p.b\n}\n";
    let after = before.replace("pub a: u32", "pub x: u32").replace("pub b: u32", "pub y: u32").replace("p.a - p.b", "p.x - p.y");
    let report = run_derived(before, &after, &[("demo::f", equal("n0"))]);
    let texts = descriptions(&report);
    assert!(texts.iter().any(|d| d.starts_with("renamed `demo::P.a` → `demo::P.x`")) && texts.iter().any(|d| d.starts_with("renamed `demo::P.b` → `demo::P.y`")), "{report:#?}");
    // More than 4 fields of one owner: no search, and a note.
    let many = "pub struct P {\n    pub a: u32,\n    pub b: u32,\n    pub c: u32,\n    pub d: u32,\n    pub e: u32,\n}\n";
    let renamed = many.replace("pub ", "pub n");
    let report = run_derived(many, &renamed.replace("pub nstruct", "pub struct"), &[]);
    assert!(!report.identity_candidates && report.notes.iter().any(|n| n.contains("above 4")), "{report:#?}");
}

#[test]
fn equal_facts_fold_and_different_facts_do_not() {
    let before = "pub fn f(x: u32) -> u32 {\n    x + 1\n}\npub fn g(x: u32) -> u32 {\n    x + 1\n}\npub fn h(x: u32) -> u32 {\n    x + 1\n}\n";
    let after = before.replace("x + 1", "x + 2");
    // Phase 1: one fact for the three sites, with the bodies.
    let report = run_derived(before, &after, &[]);
    assert_eq!(descriptions(&report), ["3 site(s): changed literal `1` → `2`"], "{report:#?}");
    assert_eq!(report.facts[0].sites[0].sites.len(), 3);
    let text = views::text_report(std::slice::from_ref(&report));
    assert!(text.contains("in: demo::f, demo::g, demo::h"), "{text}");
    // Phase 2: a fact with another compare result does not fold.
    let report = run_derived(before, &after, &[("demo::f", different(false)), ("demo::g", different(false)), ("demo::h", equal("n0"))]);
    let texts = descriptions(&report);
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts.contains(&"2 site(s): changed literal `1` → `2`".to_string()), "{texts:?}");
    assert!(report.facts.iter().any(|f| f.compare == Compare::CodeEqual && f.description.contains("demo::h")), "{:#?}", report.facts);
    // A fact with another text does not fold.
    let mixed = before.replacen("x + 1", "x + 2", 2).replacen("x + 1", "x + 3", 1);
    let texts = descriptions(&run_derived(before, &mixed, &[]));
    assert!(texts.contains(&"2 site(s): changed literal `1` → `2`".to_string()) && texts.iter().any(|t| t.starts_with("changed literal `1` → `3`")), "{texts:?}");
    // Phase 1 of the new protocol: the syntax call graph puts the call sites under the parameter.
    let report = run_derived(CALLS, &calls_after(), &[]);
    assert_eq!(report.facts.len(), 1, "{:#?}", report.facts);
    assert_eq!(report.facts[0].sites.iter().map(|g| g.sites.len()).sum::<usize>(), 3);
}

#[test]
fn a_renamed_parameter_is_one_fact() {
    let before = "pub fn f(a: u32, n: u8) -> u32 {\n    a + n as u32\n}\n";
    let report = run_derived(before, &before.replace("a: u32", "b: u32").replace("a + n", "b + n"), &[]);
    assert!(descriptions(&report).contains(&"parameter `a` of `demo::f` renamed to `b`: `u32`".to_string()), "{report:#?}");
    // Another type, or another position: a removed and an added parameter.
    let report = run_derived(before, &before.replace("a: u32", "b: u64").replace("a + n", "b as u32 + n"), &[]);
    assert!(!descriptions(&report).iter().any(|d| d.contains("renamed to")), "{:?}", descriptions(&report));
    let report = run_derived(before, &before.replace("a: u32, n: u8", "n: u8, b: u32").replace("a + n", "b + n"), &[]);
    assert!(!descriptions(&report).iter().any(|d| d.contains("renamed to")), "{:?}", descriptions(&report));
}

#[test]
fn swapped_branches_are_not_a_neutral_inversion() {
    // `invert-if` is retired as a claim. Swapped branches with no negation change the behavior: the
    // fact has the compare result of the body, and its text does not say "inverted".
    let before = "pub fn f(a: bool) -> u32 {\n    if a {\n        1\n    } else {\n        2\n    }\n}\n";
    let after = "pub fn f(a: bool) -> u32 {\n    if a {\n        2\n    } else {\n        1\n    }\n}\n";
    let report = run_derived(before, after, &[("demo::f", different(true))]);
    assert!(report.accepted(), "{report:#?}");
    assert!(report.facts.iter().all(|f| f.compare == Compare::CodeDifferent && !f.description.contains("invert")), "{:#?}", report.facts);
    // An inversion is equal at N2 only: this is not a result of the compare (design 35).
    let inverted = "pub fn f(a: bool) -> u32 {\n    if !a {\n        2\n    } else {\n        1\n    }\n}\n";
    let report = run_derived(before, inverted, &[("demo::f", equal("n2"))]);
    assert!(!report.facts.is_empty() && report.facts.iter().all(|f| f.compare == Compare::CodeDifferent), "{:#?}", report.facts);
}

#[test]
fn the_group_kind_comes_from_the_facts() {
    let before = "#[derive(Debug)]\npub struct Account {\n    pub balance: u32,\n}\npub fn get(a: &Account) -> u32 {\n    a.balance\n}\n";
    let after = before.replace("balance", "cents");
    let renamed = run_derived(before, &after, &[("demo::get", equal("n0")), ("<demo::Account as std::fmt::Debug>::fmt", different(false))]);
    assert_eq!(organize::derived_kind(&[&renamed]), Some(GroupKind::Rename));
    // Phase 1: the compare is pending, so no kind.
    assert_eq!(organize::derived_kind(&[&run_derived(before, &after, &[])]), None);
    // Batch 7b, part 3: `refactor` and `behavior` are verdict words. Each other derived group
    // has the kind `change`, also when each fact has the result `code equal`.
    let equal_code = run_derived(BEFORE, &BEFORE.replace("let mut s = 0;", "let mut s = 0u32;"), &[("demo::total", equal("n0"))]);
    assert_eq!(organize::derived_kind(&[&equal_code]), Some(GroupKind::Change), "{equal_code:#?}");
    let different_code = run_derived(BEFORE, AFTER, &[("demo::total", different(true))]);
    assert_eq!(organize::derived_kind(&[&different_code]), Some(GroupKind::Change));
    // A rename and a second change in one group: `change`.
    assert_eq!(organize::derived_kind(&[&renamed, &equal_code]), Some(GroupKind::Change));
    assert_eq!(organize::merge_kinds(GroupKind::Rename, GroupKind::Rename), GroupKind::Rename);
    assert_eq!(organize::merge_kinds(GroupKind::Rename, GroupKind::Change), GroupKind::Change);
    assert_eq!(organize::merge_kinds(GroupKind::Rename, GroupKind::Move), GroupKind::Change);
    // Only derived moves: `move`.
    let moved = "pub mod a {\n    pub fn get() -> u32 {\n        1\n    }\n}\npub mod b {\n}\n";
    let after = "pub mod a {\n}\npub mod b {\n    pub fn get() -> u32 {\n        1\n    }\n}\n";
    let report = run_derived(moved, after, &[("demo::b::get", equal("n0"))]);
    assert_eq!(organize::derived_kind(&[&report]), Some(GroupKind::Move), "{report:#?}");
    // No derived kind is a verdict word.
    for kind in [GroupKind::Rename, GroupKind::Move, GroupKind::Change] {
        let word = serde_json::to_string(&kind).unwrap();
        assert!(!word.contains("refactor") && !word.contains("behavior"), "{word}");
    }
}

/// A body oracle with statuses and full MIR facts.
struct MemberBodies(Vec<(&'static str, BodyStatus, BodyFacts)>);

impl BodyOracle for MemberBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.iter().map(|(name, status, _)| BodyComparison { name: name.to_string(), span: None, status: status.clone(), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }).collect())
    }
    fn facts(&self, _: &Step) -> anyhow::Result<Option<Vec<BodyFacts>>> {
        Ok(Some(self.0.iter().map(|(name, _, facts)| BodyFacts { name: name.to_string(), ..facts.clone() }).collect()))
    }
}

#[test]
fn a_member_order_is_never_neutral_and_a_cast_puts_it_in_the_spotlight() {
    // Batch 5e: a member order has effects outside the MIR records (the drop order, the derive
    // output order, the discriminant values). With no changed body it has the result `no compare`.
    let src = "pub enum Kind {\n    Small,\n    Large,\n}\npub fn code(k: Kind) -> u8 {\n    k as u8\n}\npub struct Pair {\n    pub a: u64,\n    pub b: u64,\n}\npub const P: Pair = Pair { a: 1, b: 2 };\npub fn first() -> u64 {\n    P.a\n}\n";
    let check = |after: &str, bodies: Vec<(&'static str, BodyStatus, BodyFacts)>| {
        let (_d1, old) = load(src);
        let (_d2, new) = load(after);
        let step = step(vec![]);
        verify_step(&StepInput { step: &step, before: &old, after: &new }, &MemberBodies(bodies), &NoOracle).report
    };
    let kinds = src.replace("    Small,\n    Large,\n", "    Large,\n    Small,\n");
    // No body changes: the result is `no compare`, and the text names the discriminant values.
    let report = check(&kinds, vec![("demo::code", equal("n0"), BodyFacts::default())]);
    let fact = report.facts.iter().find(|f| f.part == "member order").expect("a member order fact");
    assert_eq!((fact.category, fact.compare), (Category::Declaration, Compare::NoCompare), "{fact:?}");
    assert!(fact.note.as_deref().is_some_and(|n| n.contains("no compare: the MIR bodies do not show each effect of a member order")), "{fact:?}");
    assert!(fact.description.contains("the discriminant values") && !fact.spotlight, "{fact:?}");
    // A cast reads the discriminant: the body changes (its text has the values), it goes under
    // the fact, and the fact is in the spotlight.
    let cast = BodyFacts { casts: vec!["demo::Kind".into()], layouts: vec!["demo::Kind".into()], ..Default::default() };
    let report = check(&kinds, vec![("demo::code", different(false), cast)]);
    let fact = report.facts.iter().find(|f| f.part == "member order").expect("a member order fact");
    assert!(fact.spotlight && fact.spotlight_why.as_deref().is_some_and(|why| why.contains("a cast (`as`) reads (in `demo::code`)")), "{fact:?}");
    assert_eq!(fact.affected, ["demo::code"], "{fact:?}");
    // A constant of the struct: the source of `first` does not name `Pair`, but its MIR text has
    // the layout of `Pair`. The body goes under the fact, not under "indirect change".
    let pairs = src.replace("    pub a: u64,\n    pub b: u64,\n", "    pub b: u64,\n    pub a: u64,\n");
    let layout = BodyFacts { layouts: vec!["demo::Pair".into()], ..Default::default() };
    let report = check(&pairs, vec![("demo::first", different(false), layout), ("demo::code", equal("n0"), BodyFacts::default())]);
    let fact = report.facts.iter().find(|f| f.part == "member order").expect("a member order fact");
    assert_eq!((fact.compare, fact.affected.clone()), (Compare::CodeDifferent, vec!["demo::first".to_string()]), "{:#?}", report.facts);
    assert!(!report.facts.iter().any(|f| f.part == "indirect change"), "{:#?}", report.facts);
}

#[test]
fn the_mir_text_gives_the_casts_and_the_layouts() {
    let text = "    _3 = discriminant(_2) cast of k::Kind values [k::Kind::Large = 1, k::Kind::Small = 0];\n    _4 = discriminant((*_1)) of k::Other values [k::Other::A = 0];\n    _5 = const k::Pair = [01] align 8 Not layout {layout-of k::Pair: k::Pair (a, b) LayoutShape { .. }; layout-of k::Wrap<u32>: k::Wrap (0) LayoutShape { .. }};\n    _6 = f(_1) reads the values of k::Third [k::Third::X = 0] -> [return: bb1, unwind continue];\n";
    assert_eq!(crate::discriminant_casts(text), ["k::Kind"]);
    assert_eq!(crate::layout_types(text), ["k::Kind", "k::Other", "k::Pair", "k::Third", "k::Wrap"]);
    assert!(crate::discriminant_casts("_3 = discriminant(_2);").is_empty() && crate::layout_types("_3 = discriminant(_2);").is_empty());
}

#[test]
fn equal_type_changes_fold_into_one_fact_with_each_place() {
    // Brief 5f, C5 (trial 4): the `u32` → `u64` change of an identifier gave 8 facts. The facts
    // with the same type change and the same class fold into one fact that names each place.
    let before = "pub struct Account {\n    pub id: u32,\n    pub size: u32,\n}\npub enum Event {\n    Close { account: u32 },\n    Unknown(u32, u8),\n}\npub fn find(id: u32) -> u32 {\n    id\n}\npub fn other(count: u8) -> u8 {\n    count\n}\n";
    let after = before.replace("pub id: u32", "pub id: u64").replace("account: u32", "account: u64").replace("Unknown(u32, u8)", "Unknown(u64, u8)").replace("find(id: u32) -> u32", "find(id: u64) -> u64");
    let bodies = [("demo::find", equal("n0")), ("demo::other", equal("n0"))];
    let report = run_derived(before, &after, &bodies);
    assert_eq!(
        descriptions(&report),
        ["type `u32` → `u64` in 5 places: `demo::Account.id`, `demo::Event::Close.account`, `demo::Event::Unknown.0`, parameter `id` of `demo::find`, return type of `demo::find` (the public API can change: a different crate can use the item)"],
        "{:#?}",
        report.facts
    );
    assert!(report.uncovered.is_empty() && report.accepted(), "{report:#?}");
    // The fact keeps the anchor of each place.
    assert_eq!(report.facts[0].sites.iter().map(|group| group.sites.len()).sum::<usize>(), 5, "{:#?}", report.facts);
    // The text shows each place one time: no second line with the sites.
    let text = views::text_report(std::slice::from_ref(&report));
    assert!(text.contains("in 5 places: ") && !text.contains("site(s)"), "{text}");

    // A different type change does not fold with the others.
    let mixed = after.replace("pub size: u32", "pub size: u16").replace("other(count: u8)", "other(count: u16)");
    let texts = descriptions(&run_derived(before, &mixed, &bodies));
    assert_eq!(texts.len(), 3, "{texts:#?}");
    assert!(texts.iter().any(|text| text == "type of `demo::Account.size`: `u32` → `u16` (the public API can change: a different crate can use the item)"), "{texts:#?}");
    assert!(texts.iter().any(|text| text == "parameter of `demo::other` `count: u8` → `count: u16` (the public API can change: a different crate can use the item)"), "{texts:#?}");
    assert!(texts.iter().any(|text| text.starts_with("type `u32` → `u64` in 5 places: ")), "{texts:#?}");

    // A different class does not fold: the type of the parameter changes the body of `find`.
    // The items are private here: a declaration change of an item that a different crate can
    // use is a fact to read also with no changed body (batch 6a, A1).
    let report = run_derived(&before.replace("pub ", ""), &after.replace("pub ", ""), &[("demo::find", different(false)), ("demo::other", equal("n0"))]);
    let texts = descriptions(&report);
    let classes: BTreeSet<String> = report.facts.iter().map(|fact| format!("{:?}", fact.compare)).collect();
    assert!(texts.len() >= 2 && classes.len() == 2, "{:#?}", report.facts);
    for place in ["`demo::Account.id`", "`demo::Event::Close.account`", "`demo::Event::Unknown.0`", "`id`", "return type of `demo::find`"] {
        assert_eq!(texts.iter().filter(|text| text.contains(place)).count(), 1, "the fold shows the place {place} one time: {texts:#?}");
    }

    // Two different changes in one tuple, and a new parameter name, keep their own facts.
    let tuple = before.replace("Unknown(u32, u8)", "Unknown(u64, u16)").replace("pub id: u32", "pub id: u64");
    let texts = descriptions(&run_derived(before, &tuple, &bodies));
    assert_eq!(texts, ["type of `demo::Account.id`: `u32` → `u64` (the public API can change: a different crate can use the item)", "the tuple fields of `demo::Event::Unknown` changed: `(u32, u8)` → `(u64, u16)` (the public API can change: a different crate can use the item)"]);
}

#[test]
fn each_body_of_a_site_is_affected_or_equal() {
    // Brief 5f, C6 (trial 4, D14): the fact of a new parameter named the sites in `schedule`
    // and `withdraw`, but the affected bodies did not have them, with no cause. The compare
    // proves these bodies equal (after the inline of the callee). The fact says so.
    let before = "pub fn charge(amount: u64) -> u64 {\n    amount + 1\n}\npub fn schedule(a: u64) -> u64 {\n    charge(a)\n}\npub fn withdraw(a: u64) -> u64 {\n    charge(a) + 2\n}\n";
    let after = before.replace("charge(amount: u64)", "charge(amount: u64, rate: u64)").replace("amount + 1", "amount + rate").replace("charge(a)", "charge(a, 1)");
    let report = run_calls(before, &after, vec![("demo::charge", different(false), vec![]), ("demo::schedule", equal("n0"), vec!["demo::charge"]), ("demo::withdraw", different(false), vec!["demo::charge"])]);
    let fact = report.facts.iter().find(|fact| fact.description.starts_with("parameter of `demo::charge` added: `rate: u64`")).unwrap_or_else(|| panic!("{:#?}", report.facts));
    let sites: BTreeSet<String> = fact.sites.iter().flat_map(|group| group.sites.iter().map(|site| site.item.clone())).collect();
    assert_eq!(sites, ["demo::schedule".to_string(), "demo::withdraw".to_string()].into(), "{fact:#?}");
    assert_eq!(fact.affected, ["demo::charge", "demo::withdraw"], "{fact:#?}");
    assert_eq!(fact.equal_sites, ["demo::schedule"], "{fact:#?}");
    // Each body of a site is in one of the two lists.
    assert!(sites.iter().all(|site| fact.affected.contains(site) != fact.equal_sites.contains(site)), "{fact:#?}");
    let text = views::text_report(std::slice::from_ref(&report));
    assert!(text.contains("      affected bodies: demo::charge, demo::withdraw\n      bodies with a changed call site and equal compiled code: demo::schedule\n"), "{text}");
}

const TREE_V0: &str = "pub fn withdraw(balance: u32, amount: u32) -> Result<u32, ()> {\n    if amount > 100 {\n        return Err(());\n    }\n    let fee = 1;\n    Ok(balance - amount - fee)\n}\n";

/// The outcomes of the steps between the given versions, with the workspaces.
fn outcomes_of(versions: &[&str]) -> (Vec<Workspace>, Vec<StepOutcome>) {
    let loaded: Vec<(tempfile::TempDir, Workspace)> = versions.iter().map(|text| load(text)).collect();
    let mut outcomes = Vec::new();
    for index in 1..loaded.len() {
        let mut step = step(vec![]);
        step.index = index - 1;
        let mut outcome = verify_step_with(&StepInput { step: &step, before: &loaded[index - 1].1, after: &loaded[index].1 }, &FakeBodies(Default::default()), &NoOracle, true);
        outcome.report.step = index - 1;
        outcomes.push(outcome);
    }
    (loaded.into_iter().map(|(_, ws)| ws).collect(), outcomes)
}

#[test]
fn the_change_tree_of_two_steps_is_one_diff_with_the_step_numbers() {
    // Trial 4, D9: step 0 changes `S1`. Step 1 adds a statement before `S0` and changes the
    // condition. The statement paths of the two steps name different statements.
    let v1 = TREE_V0.replace("let fee = 1;", "let fee = 2;");
    let v2 = v1.replace("    if amount > 100 {", "    if balance == 0 {\n        return Err(());\n    }\n    if amount >= 100 {");
    let (trees, outcomes) = outcomes_of(&[TREE_V0, &v1, &v2]);
    let tree = views::change_tree_with(&outcomes, &trees[2], &trees[0], &[(&trees[0], &trees[1]), (&trees[1], &trees[2])]);
    let text = tree.render();
    let function = &tree.children[0].children[0];
    assert_eq!(function.id, "demo::withdraw", "{text}");
    // Each statement of the head tree shows one time, with its own path.
    let starts: Vec<&str> = function.children.iter().map(|child| child.label.split(':').next().unwrap()).collect();
    assert_eq!(starts, ["S0", "S1", "S2"], "{text}");
    assert_eq!(function.children[0].status, semilla_core::view::Status::Added, "{text}");
    assert!(function.children[0].label.starts_with("S0: if balance == 0"), "{text}");
    assert!(function.children[2].label.starts_with("S2: let fee = 2"), "{text}");
    // The step numbers are the labels.
    assert!(text.contains("steps 0, 1") && text.contains("step 1") && text.contains("step 0"), "{text}");
    assert_eq!(function.children[0].steps, [1], "{text}");
    assert_eq!(function.children[2].children[0].steps, [0], "{text}");
    // The flow view marks the new statement as added, and the changed statements as changed.
    let finals = views::final_edits(&outcomes, &trees[0], &trees[2]);
    let inputs = views::final_flow_inputs(&finals);
    let statuses = &inputs["demo::withdraw"].0;
    assert_eq!(statuses.get("S0"), Some(&semilla_core::view::Status::Added), "{statuses:?}");
    assert_eq!(statuses.get("S1/cond"), Some(&semilla_core::view::Status::Changed), "{statuses:?}");
    assert!(statuses.keys().any(|path| path.starts_with("S2")), "{statuses:?}");
}

#[test]
fn a_group_of_a_rename_with_its_docs_has_the_kind_rename() {
    let before = "#[derive(Debug)]\npub struct Account {\n    pub balance: u32,\n}\n/// Returns the balance.\npub fn get(a: &Account) -> u32 {\n    a.balance\n}\n";
    let after = before.replace("balance", "cents");
    let bodies = [("demo::get", equal("n0")), ("<demo::Account as std::fmt::Debug>::fmt", different(false))];
    let report = run_derived(before, &after, &bodies);
    assert!(report.facts.iter().any(|f| f.is_doc()), "{:?}", descriptions(&report));
    assert_eq!(organize::derived_kind(&[&report]), Some(GroupKind::Rename), "{:?}", descriptions(&report));
    // Only a doc change: no code changed.
    let docs = run_derived(before, &before.replace("Returns the balance.", "Gives the balance."), &[]);
    assert_eq!(organize::derived_kind(&[&docs]), Some(GroupKind::Change), "{:?}", descriptions(&docs));
    // A doc change with a logic change is `change`.
    let logic = run_derived(before, &before.replace("Returns the balance.", "Gives the balance.").replace("a.balance", "a.balance + 1"), &[("demo::get", different(false))]);
    assert_eq!(organize::derived_kind(&[&logic]), Some(GroupKind::Change), "{:?}", descriptions(&logic));
}

#[test]
fn a_public_api_fact_has_the_public_api_line() {
    // Batch 7b, part 3: the compare does not see the code of a different crate. A fact that
    // changes the public API has a sub line that says so, also with the result `code equal`.
    let before = "fn hidden(a: u32) -> u32 {\n    a + 1\n}\npub fn get(a: u32) -> u32 {\n    hidden(a)\n}\n";
    let report = run_derived(before, &before.replace("fn hidden", "pub fn hidden"), &[("demo::hidden", equal("n0")), ("demo::get", equal("n0"))]);
    let fact = &report.facts[0];
    assert!(fact.api_change() && fact.compare == Compare::CodeEqual, "{fact:?}");
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert!(text.contains(&format!("\n      {}\n", semilla_core::report::API_LINE)), "{text}");
    // The line asks for no reason.
    assert!(report.accepted(), "{report:#?}");
    // A change of a private item has no such line.
    let report = run_derived(before, &before.replace("a + 1", "a + 2"), &[("demo::hidden", different(false))]);
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert!(!text.contains("! public API"), "{text}");
}

#[test]
fn two_edits_of_one_body_say_so_on_the_affected_line() {
    // Batch 7b, part 5.11: the compare of a body is a result of each edit of the body.
    let before = "pub fn sum(a: u32) -> u32 {\n    let low = 1;\n    let high = 2;\n    a + low + high\n}\npub fn one(a: u32) -> u32 {\n    a + 1\n}\n";
    let report = run_derived(before, &before.replace("let low = 1;", "let low = 3;").replace("let high = 2;", "let high = 4;").replace("    a + 1\n", "    a + 5\n"), &[("demo::sum", different(false)), ("demo::one", different(false))]);
    let in_sum: Vec<_> = report.facts.iter().filter(|fact| fact.affected == ["demo::sum"]).collect();
    assert!(in_sum.len() == 2 && in_sum.iter().all(|fact| fact.other_edits == 1), "{report:#?}");
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert_eq!(text.matches("      affected bodies: demo::sum (with 1 other edit of this body)\n").count(), 2, "{text}");
    // One edit in a body: no added text.
    assert!(text.contains("      affected bodies: demo::one\n"), "{text}");
    let three = semilla_core::Fact { other_edits: 2, ..Default::default() };
    assert_eq!(three.other_edits_text(), " (with 2 other edits of this body)");
}

#[test]
fn a_new_local_const_that_reads_a_file_needs_a_reason() {
    // Batch 7b, part 5.7: `include_str!` in a new `const` of a function body.
    let before = "pub fn limit(a: usize) -> usize {\n    a\n}\n";
    let after = before.replace("    a\n", "    const LIMIT: &str = include_str!(\"limit.txt\");\n    a + LIMIT.len()\n");
    // The use comes from the MIR records.
    let call = "include_str!(\"limit.txt\")";
    let bodies = SpanBodies(vec![BodyComparison { name: "demo::limit".into(), span: None, status: different(false), config: vec![call.to_string()], differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }]);
    let report = run_derived_with(before, &after, &bodies);
    let fact = report.facts.iter().find(|fact| fact.description.contains("const LIMIT")).unwrap_or_else(|| panic!("{report:#?}"));
    assert!(fact.description.starts_with("added item `const LIMIT: &str = include_str!(\"limit.txt\");`"), "{fact:#?}");
    assert!(fact.spotlight && fact.spotlight_why.as_deref().is_some_and(|why| why.contains("a file that the code reads at build time: `limit.txt` (`include_str!` in `demo::limit`)")), "{fact:#?}");
    assert!(!report.accepted(), "{report:#?}");
}

#[test]
fn a_member_order_of_a_local_type_follows_the_rule_of_a_module_type() {
    // Batch 7b, part 5.10: an `enum` in a function body with a different order of its members.
    let before = "pub fn rank(a: u8) -> u8 {\n    enum Side { Low, High }\n    let _ = Side::Low;\n    let _ = Side::High;\n    a\n}\n";
    let check = |after: &str, status: BodyStatus| run_derived(before, after, &[("demo::rank", status)]);
    // Equal bodies are not a result: `no compare`, as for an enum of a module.
    let report = check(&before.replace("Low, High", "High, Low"), equal("n0"));
    let fact = &report.facts[0];
    assert_eq!(fact.description, "member order of `Side` (in `demo::rank`): `Low, High` → `High, Low`; the discriminant values of the variants with no explicit value change");
    assert_eq!((fact.part.as_str(), fact.category, fact.compare, fact.spotlight), ("member order", Category::Declaration, Compare::NoCompare, false), "{fact:#?}");
    // A changed body goes under the fact.
    let report = check(&before.replace("Low, High", "High, Low"), different(false));
    assert_eq!((report.facts[0].compare, report.facts[0].affected.clone()), (Compare::CodeDifferent, vec!["demo::rank".to_string()]), "{:#?}", report.facts);
    // An order derive: the fact needs a reason.
    let derive = before.replace("    enum Side", "    #[derive(PartialEq, PartialOrd)]\n    enum Side");
    let report = run_derived(&derive, &derive.replace("Low, High", "High, Low"), &[("demo::rank", equal("n0"))]);
    let fact = &report.facts[0];
    assert!(fact.spotlight && fact.spotlight_why.as_deref().is_some_and(|why| why.contains("an order derive")) && !report.accepted(), "{fact:#?}");
    // A second change in the same item shows in the text. A new member only is not an order change.
    let report = check(&before.replace("Low, High", "High, Low = 4"), equal("n0"));
    assert!(report.facts[0].description.contains("; the item has a second change: `enum Side { Low, High }` → `enum Side { High, Low = 4 }`"), "{:#?}", report.facts);
    let report = check(&before.replace("Low, High", "Low, High, Top"), equal("n0"));
    assert_eq!(report.facts[0].description, "changed item: `enum Side { Low, High }` → `enum Side { Low, High, Top }` at `demo::rank/S0`");
    // The fields of a struct.
    let before = "pub fn rank(a: u8) -> u8 {\n    struct Pair { a: u8, b: u16 }\n    let _ = Pair { a, b: 2 };\n    a\n}\n";
    let report = run_derived(before, &before.replace("a: u8, b: u16 }", "b: u16, a: u8 }"), &[("demo::rank", equal("n0"))]);
    assert_eq!(report.facts[0].description, "member order of `Pair` (in `demo::rank`): `a, b` → `b, a`");
}

/// Batch 7b, part 5.6: the diff of a node of the change tree shows the changed part.
#[test]
fn the_tree_diff_of_a_long_line_shows_the_changed_part() {
    let before = "pub fn a() {}\npub fn b() {}\npub fn c() {}\npub fn top() {\n    use crate::{a as first_name, b as second_name, c as third_name};\n    let _ = (first_name, second_name, third_name);\n}\n";
    let after = before.replace("third_name", "last_name");
    let (_d1, old) = load(before);
    let (_d2, new) = load(&after);
    let oracle = FakeBodies([("demo::top".to_string(), equal("n0"))].into_iter().collect());
    let outcome = verify_step_with(&StepInput { step: &step(vec![]), before: &old, after: &new }, &oracle, &NoOracle, true);
    let tree = views::change_tree(&[outcome], &new, &old).render();
    assert!(tree.contains("⟦…second_name, c as third_name};⟧→⟦…second_name, c as last_name};⟧"), "{tree}");
}

#[test]
fn a_removed_test_target_has_no_attribute_fact() {
    // Batch 7b, part 5.2: the inner attributes of a removed target go with the target.
    const MANIFEST: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";
    let lib = "pub fn one() -> u8 {\n    1\n}\n";
    let target = "#![allow(dead_code)]\n#[test]\nfn first() {\n    assert_eq!(demo::one(), 1);\n}\n";
    let report = run_derived_files(&[("Cargo.toml", MANIFEST), ("src/lib.rs", lib), ("tests/extra.rs", target)], &[("Cargo.toml", MANIFEST), ("src/lib.rs", lib)], &[("demo::one", equal("n0"))]);
    let texts = descriptions(&report);
    assert!(texts.iter().any(|text| text.starts_with("removed `demo::extra::first`")) && !texts.iter().any(|text| text.starts_with("attributes of")), "{texts:#?}");
    // A target with no item: the attribute fact is the only fact of the removal, and it stays.
    let report = run_derived_files(&[("Cargo.toml", MANIFEST), ("src/lib.rs", lib), ("tests/extra.rs", "#![allow(dead_code)]\n")], &[("Cargo.toml", MANIFEST), ("src/lib.rs", lib)], &[("demo::one", equal("n0"))]);
    assert!(!report.facts.is_empty(), "{report:#?}");
}

#[test]
fn a_changed_body_with_unsafe_code_has_the_unsafe_line() {
    // Batch 7b, part 5.3: an edit in a body with an `unsafe` block, or in an `unsafe fn`.
    let before = "pub fn read(p: &u32) -> u32 {\n    let step = 2;\n    unsafe { *(p as *const u32) + step }\n}\npub unsafe fn raw(a: u32) -> u32 {\n    a + 1\n}\npub fn plain(a: u32) -> u32 {\n    a + 1\n}\n";
    let line = format!("\n      {}\n", semilla_core::report::UNSAFE_LINE);
    for (old, new, body) in [("let step = 2;", "let step = 1;", "demo::read"), ("pub unsafe fn raw(a: u32) -> u32 {\n    a + 1", "pub unsafe fn raw(a: u32) -> u32 {\n    a + 2", "demo::raw")] {
        let report = run_derived(before, &before.replace(old, new), &[(body, different(false))]);
        assert!(report.facts.iter().all(|fact| fact.unsafe_code), "{report:#?}");
        let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
        assert!(text.contains(&line), "{text}");
        // The line asks for no reason.
        assert!(report.accepted(), "{report:#?}");
    }
    // A body with no `unsafe` code has no such line.
    let report = run_derived(before, &before.replace("pub fn plain(a: u32) -> u32 {\n    a + 1", "pub fn plain(a: u32) -> u32 {\n    a + 2"), &[("demo::plain", different(false))]);
    assert!(report.facts.iter().all(|fact| !fact.unsafe_code), "{report:#?}");
    assert!(!crate::views::text_report_with(std::slice::from_ref(&report), false).contains("! unsafe code"));
}

#[test]
fn the_text_output_shows_the_scope_of_a_long_list() {
    // Batch 5x, part E (before: brief 5h, C6): the `step` output shows a list of more than 8
    // bodies as a count and the scope. `semilla status --verbose` shows each name.
    let bodies: Vec<String> = (0..21).map(|index| format!("demo::body_{index:02}")).collect();
    let fact = semilla_core::Fact { id: "f1".into(), part: "type".into(), description: "type of `demo::Cart::total` changed: u32 → u64".into(), status: "derived".into(), category: Category::Declaration, compare: Compare::CodeDifferent, affected: bodies.clone(), equal_sites: bodies[..8].to_vec(), ..Default::default() };
    let mut report = StepReport { facts: vec![fact], ..Default::default() };
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert!(text.contains("      affected bodies: 21 bodies of the crate `demo` (`semilla status --verbose` shows the names)\n"), "{text}");
    // A list of 8 bodies shows each name.
    assert!(text.contains(&format!("      bodies with a changed call site and equal compiled code: {}\n", bodies[..8].join(", "))), "{text}");
    assert!(!text.contains("demo::body_08"), "{text}");
    let verbose = crate::views::text_report(std::slice::from_ref(&report));
    assert!(verbose.contains(&format!("      affected bodies: {}\n", bodies.join(", "))), "{verbose}");
    // Two crates: the scope names each crate with its count.
    report.facts[0].affected.push("<other::T as demo::Show>::show".into());
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert!(text.contains("      affected bodies: 22 bodies of 2 crates: `demo` (21), `other` (1) ("), "{text}");
}

#[test]
fn a_renamed_macro_with_equal_bodies_is_one_rename_fact_with_its_call_sites() {
    // Design 37.17: the name of a `macro_rules` macro changes, and the body stays equal.
    let before = "macro_rules! map_either {\n    ($value:expr, $pattern:pat => $result:expr) => {\n        match $value {\n            $pattern => $result,\n        }\n    };\n}\npub fn one(a: u32) -> u32 {\n    map_either!(a, x => x + 1)\n}\npub fn two(a: u32) -> u32 {\n    map_either!(a, y => y * 2)\n}\n";
    let after = before.replace("map_either", "map_both");
    let report = run_derived(before, &after, &[("demo::one", equal("n0")), ("demo::two", equal("n0"))]);
    let texts = descriptions(&report);
    assert_eq!(texts, ["renamed macro `demo::map_either` → `demo::map_both`"], "{report:#?}");
    let fact = &report.facts[0];
    assert_eq!((fact.category, fact.compare), (Category::Rename, Compare::CodeEqual), "{fact:#?}");
    assert!(report.accepted(), "{report:#?}");
    // The call sites are under the rename fact.
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert!(text.contains("  ◆ f1 derived: renamed macro `demo::map_either` → `demo::map_both` [rename · code equal]\n      2 site(s): changed macro call `map_either!` → `map_both!` (in demo::one, demo::two)\n"), "{text}");
    // A call site with one more change keeps its own fact. A changed body shows under the fact.
    let more = after.replace("map_both!(a, y => y * 2)", "map_both!(a, y => y * 3)");
    let report = run_derived(before, &more, &[("demo::one", different(false)), ("demo::two", different(false))]);
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert!(text.contains("renamed macro `demo::map_either` → `demo::map_both` [rename · code different]\n      1 site(s): changed macro call `map_either!` → `map_both!` (in demo::one)\n      ! the compiled code changes: demo::one\n"), "{text}");
    assert!(text.contains("derived: changed macro: `map_either!(a, y => y * 2)` → `map_both!(a, y => y * 3)` at `demo::two/tail` [semantics · code different]"), "{text}");
    // A rename with a different body keeps the removed fact and the added fact.
    let other = after.replace("$pattern => $result,", "$pattern => $result,\n            #[allow(unreachable_patterns)]\n            _ => unreachable!(),");
    let report = run_derived(before, &other, &[("demo::one", different(false)), ("demo::two", different(false))]);
    let texts = descriptions(&report);
    assert!(texts.iter().any(|text| text.starts_with("removed macro `map_either`")) && texts.iter().any(|text| text.starts_with("added macro `map_both`")), "{texts:#?}");
    assert!(!texts.iter().any(|text| text.starts_with("renamed ")), "{texts:#?}");
}

/// A fact for a test of the text output.
fn shown_fact(id: &str, file: &str, description: &str, category: Category, compare: Compare) -> semilla_core::Fact {
    semilla_core::Fact { id: id.into(), file: Some(file.into()), part: format!("{file}:{id}"), description: description.into(), status: "derived".into(), category, compare, ..Default::default() }
}

#[test]
fn the_text_output_shows_a_manifest_diff_and_an_equal_spotlight_text_one_time() {
    // Design 37.13: three manifest facts of one file have the same diff and the same long
    // "build input" text. The diff shows one time, after the last fact. The text shows one
    // time, with the fact numbers. Each fact keeps its line and its reason key.
    let input = "a build input: the MIR of 7 bodies changed with no edit of their text, and this change can reach their crates. The bodies have one of these causes: `one`, `two`";
    let mut facts: Vec<semilla_core::Fact> = [("f2", "a new dependency; "), ("f3", ""), ("f4", "a package setting that changes the build; ")]
        .iter()
        .map(|(id, kind)| {
            let mut fact = shown_fact(id, "Cargo.toml", &format!("Cargo.toml: key {id} changed"), Category::Manifest, Compare::CodeDifferent);
            fact.spotlight = true;
            fact.spotlight_why = Some(format!("{kind}{input}"));
            fact.content = Some("@@ line 1 @@\n- old\n+ new".into());
            fact.build_code_change = true;
            fact.affected = (0..7).map(|index| format!("demo::long_module_name::body_{index}")).collect();
            fact
        })
        .collect();
    facts.insert(0, shown_fact("f1", "src/lib.rs", "changed literal `1` → `2` at `demo::f/S0`", Category::Semantics, Compare::CodeDifferent));
    let report = StepReport { facts, ..Default::default() };
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert_eq!(text.matches("- old").count(), 1, "{text}");
    assert!(text.contains("      diff of `Cargo.toml` (one time, for f2 to f4):\n        @@ line 1 @@\n        - old\n        + new\n"), "{text}");
    assert_eq!(text.matches("The bodies have one of these causes").count(), 1, "{text}");
    assert!(text.contains(&format!("      ✗ spotlight: a new dependency; {input} (the same text for f2 to f4); it needs a reason: --reason 'Cargo.toml:f2=…'\n")), "{text}");
    assert!(text.contains("      ✗ spotlight: a build input: the same text as f2; it needs a reason: --reason 'Cargo.toml:f3=…'\n"), "{text}");
    assert!(text.contains("      ✗ spotlight: a package setting that changes the build; a build input: the same text as f2; it needs a reason: --reason 'Cargo.toml:f4=…'\n"), "{text}");
    // A long list of bodies that is equal for the facts shows one time.
    assert!(text.contains("demo::long_module_name::body_6 (the same list for f2 to f4)\n"), "{text}");
    assert_eq!(text.matches("      affected bodies: the same list as f2\n").count(), 2, "{text}");
    for id in ["f1", "f2", "f3", "f4"] {
        assert_eq!(text.matches(&format!("  ◆ {id} derived: ")).count(), 1, "{text}");
    }
    // The build code line names the manifest facts. The facts are below the code fact.
    assert!(text.contains(&format!("  ! {}\n      `Cargo.toml`: the manifest facts f2 to f4 below, with the diff of the file\n  ◆ f1 ", semilla_core::report::BUILD_CODE_SECTION)), "{text}");
    // A short text that is equal for two facts shows at each fact.
    let mut short = report.clone();
    for fact in &mut short.facts[1..] {
        fact.spotlight_why = Some("a new dependency".into());
    }
    let text = crate::views::text_report_with(std::slice::from_ref(&short), false);
    assert_eq!(text.matches("✗ spotlight: a new dependency; it needs a reason").count(), 3, "{text}");
}

#[test]
fn the_text_output_merges_equal_facts_and_keeps_each_number_and_place() {
    // Design 37.16: facts with the same text, category, and compare result are one line with
    // the count, the numbers, and the places. A different compare result keeps its own line.
    let same = "changed `use` line: `use a::b;` → `use a::c;`";
    let mut facts = vec![
        shown_fact("f1", "src/one.rs", same, Category::Import, Compare::CodeEqual),
        shown_fact("f2", "src/lib.rs", "removed `demo::gone`", Category::Removed, Compare::CodeDifferent),
        shown_fact("f3", "src/two.rs", same, Category::Import, Compare::CodeEqual),
        shown_fact("f4", "src/three.rs", same, Category::Import, Compare::CodeDifferent),
        shown_fact("f5", "src/four.rs", same, Category::Import, Compare::CodeEqual),
    ];
    let report = StepReport { facts: facts.clone(), ..Default::default() };
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert!(text.contains(&format!("  ◆ f1 derived: {same} [import · code equal] × 3 (also f3, f5)\n      places: src/one.rs, src/two.rs, src/four.rs\n")), "{text}");
    assert!(text.contains(&format!("  ◆ f4 derived: {same} [import · code different]\n")), "{text}");
    assert_eq!(text.matches("  ◆ ").count(), 3, "{text}");
    // A fact with a different line below it (a spotlight with its own key) keeps its own line.
    facts[2].spotlight = true;
    facts[2].spotlight_why = Some("a new attribute".into());
    let report = StepReport { facts, ..Default::default() };
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert!(text.contains(&format!("  ◆ f1 derived: {same} [import · code equal] × 2 (also f5)\n")), "{text}");
    assert!(text.contains(&format!("  ◆ f3 derived: {same} [import · code equal]\n      ✗ spotlight: a new attribute; it needs a reason: --reason 'src/two.rs:f3=…'\n")), "{text}");
}

#[test]
fn the_text_output_puts_the_body_edits_with_different_code_first() {
    // Design 37.18: the order of the facts. Each fact keeps its number.
    let facts = vec![
        shown_fact("f1", "Cargo.toml", "Cargo.toml: package.version \"1\" → \"2\"", Category::Manifest, Compare::NoCode),
        shown_fact("f2", "README.md", "README.md: changed", Category::File, Compare::NoCode),
        shown_fact("f3", "src/lib.rs", "renamed `demo::a` → `demo::b`", Category::Rename, Compare::CodeEqual),
        shown_fact("f4", "src/lib.rs", "added `demo::new`", Category::Added, Compare::CodeDifferent),
        shown_fact("f5", "src/lib.rs", "changed docs of `demo::b`", Category::Docs, Compare::NoCode),
        shown_fact("f6", "src/lib.rs", "changed literal `1` → `2` at `demo::f/S0`", Category::Semantics, Compare::CodeDifferent),
        shown_fact("f7", "src/lib.rs", "changed literal `3` → `3u8` at `demo::g/S0`", Category::Semantics, Compare::CodeEqual),
        shown_fact("f8", "src/lib.rs", "added statement `x += 1;` at `demo::f/S1`", Category::Semantics, Compare::CodeDifferent),
    ];
    let report = StepReport { facts, ..Default::default() };
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    let ids: Vec<&str> = text.lines().filter_map(|line| line.strip_prefix("  ◆ ")).map(|line| line.split(' ').next().unwrap()).collect();
    assert_eq!(ids, ["f6", "f8", "f4", "f3", "f7", "f1", "f2", "f5"], "{text}");
}

#[test]
fn a_build_input_spotlight_shows_five_causes_and_the_verbose_output_shows_each_cause() {
    // Design 37.14: eight changed dependency keys can be the cause of one changed body.
    let key = |index: usize, version: &str| format!("dep{index} = \"{version}\"\n");
    let manifest = |version: &str| format!("{MANIFEST}\n[dependencies]\n{}", (0..8).map(|index| key(index, version)).collect::<String>());
    let lib = "pub fn one() -> u32 {\n    1\n}\n";
    let (old, new) = (manifest("1.0.0"), manifest("1.1.0"));
    let report = run_derived_files(&[("Cargo.toml", &old), ("src/lib.rs", lib)], &[("Cargo.toml", &new), ("src/lib.rs", lib)], &[("demo::one", different(false))]);
    let fact = report.facts.iter().find(|fact| fact.spotlight_why.as_ref().is_some_and(|why| why.contains("a build input"))).unwrap_or_else(|| panic!("{report:#?}"));
    let why = fact.spotlight_why.as_ref().unwrap();
    assert!(why.ends_with(", and 3 more (`semilla status --verbose` shows each cause)"), "{why}");
    assert_eq!(why.split("have one of these causes: ").nth(1).unwrap().matches("`Cargo.toml: ").count(), 5, "{why}");
    assert_eq!(fact.causes.len(), 8, "{fact:#?}");
    let short = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert!(!short.contains("each cause: "), "{short}");
    let verbose = crate::views::text_report(std::slice::from_ref(&report));
    assert_eq!(verbose.matches("      each cause: `Cargo.toml: ").count(), 1, "{verbose}");
    let line = verbose.lines().find(|line| line.starts_with("      each cause: ")).unwrap();
    assert_eq!(line.matches("`Cargo.toml: dependencies.dep").count(), 8, "{line}");
}

/// The facts of a step whose MIR oracle says that `demo::f` is equal (N0).
/// The notes of the facts, in one text.
fn fact_notes(facts: &[semilla_core::report::Fact]) -> String {
    facts.iter().filter_map(|f| f.note.clone()).collect::<Vec<_>>().join("\n")
}

/// The causes that the rules of the build configuration record for the body `demo::f`, in the
/// form "`write!` in `demo::f` (Semilla cannot see into it)". Design 35: a use that Semilla
/// sees goes to the note of a fact. A call that Semilla cannot see into goes to no output.
fn config_causes(old: &Workspace, new: &Workspace) -> String {
    let mut bodies = vec![BodyComparison { name: "demo::f".into(), span: None, status: equal("n0"), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }];
    crate::srcconfig::apply(&mut bodies, old, new, &BTreeMap::new());
    let show = |text: &String| match text.strip_suffix(crate::srcconfig::UNSEEN) {
        Some(call) => format!("`{call}` in `demo::f` (Semilla cannot see into it)"),
        None if text.ends_with(')') && text.starts_with('`') => text.clone(),
        None if text.starts_with('`') || text.starts_with("static `") || text.starts_with("code that") => format!("{text} in `demo::f`"),
        None => format!("`{text}` in `demo::f`"),
    };
    bodies[0].stopped.iter().chain(&bodies[0].config).map(show).collect::<Vec<_>>().join("\n")
}

/// The facts of a rewrite of `demo::f` with equal compiled code, and the causes of
/// [`config_causes`].
fn source_config_facts(before: &str, after: &str) -> (Vec<semilla_core::report::Fact>, String) {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    (run_derived(before, after, &[("demo::f", equal("n0"))]).facts, config_causes(&old, &new))
}

#[test]
fn a_keyword_before_the_operator_not_is_not_a_macro_name() {
    // Batch 5l, B4: the token reader took `if !(a)` as a call of a macro `if!`. A keyword and
    // the name of a label are not macro names.
    let base = "pub fn f(a: u64) -> u64 {\n    let b = a;\n    b\n}\n";
    let proved = |facts: &[semilla_core::report::Fact]| facts.iter().any(|f| f.compare == Compare::CodeEqual);
    let spotlight = |facts: &[semilla_core::report::Fact]| facts.iter().filter_map(|f| f.spotlight_why.clone()).collect::<Vec<_>>().join("\n");
    let with = |form: &str| source_config_facts(base, &format!("pub fn f(a: u64) -> u64 {{\n    let c = a;\n    {form}\n    c\n}}\n"));
    for form in [
        "if !(c > 1) { return 2; }",
        "while !(c > 1) { break; }",
        "if c > 9 { return !(c); }",
        "match !(c > 1) { _ => {} }",
        "for _ in !(c)..!(c) {}",
        "let _ = loop { break !(c > 1); };",
        "let _ = 'a: { break 'a !(c > 1); };",
        "if c != (1) { return 2; }",
        "if ![c].is_empty() { return 2; }",
        "if !{ c > 1 } { return 2; }",
        "let _ = if c > 1 { 1 } else { 2 } & !(c);",
        "let _ = move || !(c > 1);",
        "let _ = c as u64 & !(c);",
        "let _ = [!(c), !(c)];",
    ] {
        let (facts, causes) = with(form);
        assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.is_empty() && fact_notes(&facts).is_empty(), "{form}: {facts:#?}");
    }
    // A macro call stays a macro call: a raw identifier, a weak keyword, a keyword of a later
    // edition only, and a space before the `!`.
    for (form, name) in [("let _ = r#try!(Ok::<u64, ()>(c));", "try!"), ("other::r#if!(c);", "if!"), ("union!(c);", "union!"), ("gate !(c);", "gate!")] {
        let (facts, causes) = with(form);
        assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains(&format!("`{name}` in `demo::f` (Semilla cannot see into it)")), "{form}: {facts:#?}");
    }
}

#[test]
fn a_cfg_attribute_in_a_changed_body_is_a_note() {
    // Batch 5k, A1: the compiler removes the code of a false `cfg` attribute, so the MIR of the
    // check configuration is equal. Design 35: the result stays `code equal`, and a note names
    // the condition. The fact has no spotlight for this cause.
    let base = "pub fn f(a: u64) -> u64 {\n    let b = a;\n    b\n}\n";
    let proved = |facts: &[semilla_core::report::Fact]| facts.iter().any(|f| f.compare == Compare::CodeEqual);
    let spotlight = |facts: &[semilla_core::report::Fact]| facts.iter().filter_map(|f| f.spotlight_why.clone()).collect::<Vec<_>>().join("\n");

    // An honest rewrite, with a free attribute and macros of the standard library.
    let (facts, causes) = source_config_facts(base, "pub fn f(a: u64) -> u64 {\n    #[allow(unused)]\n    let c = a;\n    debug_assert!(c == a, \"{}\", format!(\"{c}\"));\n    c\n}\n");
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.is_empty() && fact_notes(&facts).is_empty(), "{facts:#?}");

    // A statement with a `cfg` attribute.
    let (facts, causes) = source_config_facts(base, "pub fn f(a: u64) -> u64 {\n    #[cfg(not(debug_assertions))]\n    return 0;\n    let b = a;\n    b\n}\n");
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains("`#[cfg(not(debug_assertions))]` in `demo::f`"), "{facts:#?}");
    // The use is a note under each fact of the body. The note asks for no reason.
    assert!(facts.iter().all(|f| f.note.as_deref() == Some("this body uses `#[cfg(not(debug_assertions))]`, which Semilla did not check (`demo::f`)")), "{facts:#?}");

    // A `cfg` attribute with no edit, in a body with an edit: the proof of the edit is for one
    // configuration only.
    let old = "pub fn f(a: u64) -> u64 {\n    let mut b = a;\n    #[cfg(windows)]\n    {\n        b += 1;\n    }\n    b\n}\n";
    let (facts, causes) = source_config_facts(old, &old.replace("    b\n}", "    a\n}"));
    assert!(proved(&facts) && spotlight(&facts).is_empty() && !causes.is_empty(), "{facts:#?}");
    // The spotlight is only for an edit in the statement with the attribute (batch 5o, A4).
    let (facts, causes) = source_config_facts(old, &old.replace("b += 1;", "b += 2;"));
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains("`#[cfg(windows)]` in `demo::f`"), "{facts:#?}");
    let (facts, causes) = source_config_facts(old, &old.replace("    #[cfg(windows)]\n", "    b += 0;\n    #[cfg(windows)]\n"));
    assert!(proved(&facts) && spotlight(&facts).is_empty() && !causes.is_empty(), "{facts:#?}");
    // An attribute above the changed block. The note names each condition of the body.
    let old = "pub fn f(a: u64) -> u64 {\n    #[cfg(unix)]\n    let quick = 1;\n    #[cfg(windows)]\n    {\n        let b = a;\n        return b;\n    }\n    let c = a;\n    c\n}\n";
    let (facts, causes) = source_config_facts(old, &old.replace("        let b = a;\n        return b;\n", "        return a;\n"));
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains("`#[cfg(windows)]` in `demo::f`"), "{facts:#?}");
    assert!(facts.iter().all(|f| f.note.as_deref() == Some("this body uses `#[cfg(unix)]`, `#[cfg(windows)]`, which Semilla did not check (`demo::f`)")), "{facts:#?}");
    let (facts, causes) = source_config_facts(old, &old.replace("    let c = a;\n    c\n", "    a\n"));
    assert!(proved(&facts) && spotlight(&facts).is_empty() && !causes.is_empty(), "{facts:#?}");
    // An edit only in a comment: the tokens are the same, so the proof stays.
    let (facts, causes) = source_config_facts(old, &old.replace("    let c = a;\n", "    // A note.\n    let c = a;\n"));
    assert!(spotlight(&facts).is_empty() && causes.is_empty() && fact_notes(&facts).is_empty(), "{facts:#?}");

    // A `cfg` attribute on the function selects the full function: each build with the body has
    // the same body.
    let (facts, causes) = source_config_facts(&format!("#[cfg(unix)]\n{base}"), "#[cfg(unix)]\npub fn f(a: u64) -> u64 {\n    let c = a;\n    c\n}\n");
    assert!(proved(&facts) && causes.is_empty() && fact_notes(&facts).is_empty(), "{facts:#?}");
    let (facts, causes) = source_config_facts(&format!("#[cfg_attr(unix, inline)]\n{base}"), "#[cfg_attr(unix, inline)]\npub fn f(a: u64) -> u64 {\n    let c = a;\n    c\n}\n");
    // `inline` under `cfg_attr` is in the allow list (batch 5o, A5). `no_mangle` is not.
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.is_empty() && fact_notes(&facts).is_empty(), "{facts:#?}");
    let (facts, causes) = source_config_facts(&format!("#[cfg_attr(unix, no_mangle)]\n{base}"), "#[cfg_attr(unix, no_mangle)]\npub fn f(a: u64) -> u64 {\n    let c = a;\n    c\n}\n");
    assert!(proved(&facts) && !causes.is_empty(), "{facts:#?}");

    // A macro of the workspace with a `cfg` attribute in its definition, also through a second
    // macro.
    let macros = "macro_rules! rel { ($e:expr) => { #[cfg(not(debug_assertions))] { $e } }; }\nmacro_rules! outer { ($e:expr) => { rel!($e) }; }\n";
    let (facts, causes) = source_config_facts(&format!("{macros}{base}"), &format!("{macros}pub fn f(a: u64) -> u64 {{\n    outer!(return 0);\n    let b = a;\n    b\n}}\n"));
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains("`outer! (rel! (#[cfg(not(debug_assertions))]))` in `demo::f`"), "{facts:#?}");
    // A free macro of the workspace does not stop the proof.
    let free = "macro_rules! twice { ($e:expr) => { $e + $e }; }\n";
    let (facts, causes) = source_config_facts(&format!("{free}pub fn f(a: u64) -> u64 {{\n    let b = twice!(a);\n    b\n}}\n"), &format!("{free}pub fn f(a: u64) -> u64 {{\n    let c = twice!(a);\n    c\n}}\n"));
    assert!(proved(&facts) && causes.is_empty() && fact_notes(&facts).is_empty(), "{facts:#?}");

    // Semilla cannot see into a macro or an attribute of another crate.
    let (facts, causes) = source_config_facts(base, "pub fn f(a: u64) -> u64 {\n    other::gate!(return 0);\n    let b = a;\n    b\n}\n");
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains("`gate!` in `demo::f` (Semilla cannot see into it)") && fact_notes(&facts).is_empty(), "{facts:#?}");
    let (facts, causes) = source_config_facts(base, "pub fn f(a: u64) -> u64 {\n    cfg_select! { debug_assertions => {} _ => { return 0; } }\n    let b = a;\n    b\n}\n");
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains("`cfg_select!` in `demo::f`"), "{facts:#?}");
    let (facts, causes) = source_config_facts(base, "pub fn f(a: u64) -> u64 {\n    #[other::gate]\n    let c = a;\n    c\n}\n");
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains("`#[other::gate]` in `demo::f` (Semilla cannot see into it)"), "{facts:#?}");
    // A macro of another crate can have the name of a standard macro.
    let glob = "use other::*;\n";
    let (facts, causes) = source_config_facts(&format!("{glob}{base}"), &format!("{glob}pub fn f(a: u64) -> u64 {{\n    let c = a;\n    println!(\"{{c}}\");\n    c\n}}\n"));
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains("`println!` in `demo::f` (Semilla cannot see into it)"), "{facts:#?}");
    let import = "use other::gate as vec;\n";
    let (facts, causes) = source_config_facts(&format!("{import}{base}"), &format!("{import}pub fn f(a: u64) -> u64 {{\n    let c = a;\n    let _ = vec![c];\n    c\n}}\n"));
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains("`vec!` in `demo::f` (Semilla cannot see into it)"), "{facts:#?}");

    // A body with no text edit keeps its status.
    let old = "pub fn f(a: u64) -> u64 {\n    #[cfg(windows)]\n    return 0;\n    a\n}\npub fn g() {}\n";
    let (facts, causes) = source_config_facts(old, &old.replace("pub fn g() {}", "pub fn g() {\n}"));
    assert!(spotlight(&facts).is_empty() && causes.is_empty() && fact_notes(&facts).is_empty(), "{facts:#?}");
}

#[test]
fn an_identifier_with_a_character_outside_ascii_shows_its_escape_form() {
    // Batch 5k, D7 (trial 6, attempt 20): `charge` and `сharge` (a Cyrillic first letter) look
    // equal. The fact shows the escape form of the name and is in the spotlight.
    use crate::facts::{non_ascii_names, with_ascii_forms, NON_ASCII};
    let names = |text: &str| non_ascii_names(text).into_iter().map(|(_, name)| name).collect::<Vec<_>>();
    assert_eq!(names("let \u{441}harge = charge + na\u{ef}ve;"), ["\u{441}harge", "na\u{ef}ve"]);
    assert!(names("let a = \"\u{441}harge\"; // \u{441}harge").is_empty(), "a string and a comment are not names");
    assert_eq!(with_ascii_forms("changed path `charge` \u{2192} `\u{441}harge` at `demo::f`"), "changed path `charge` \u{2192} `\u{441}harge` (`\\u{441}harge`) at `demo::f`");
    assert_eq!(with_ascii_forms("added `demo::charge`"), "added `demo::charge`");

    let before = "pub fn charge(a: u64) -> u64 {\n    a + 1\n}\npub fn total(a: u64) -> u64 {\n    charge(a)\n}\n";
    let after = before.replace("charge", "\u{441}harge");
    let report = run_derived(before, &after, &[]);
    let why = |fact: &semilla_core::report::Fact| fact.spotlight_why.clone().unwrap_or_default();
    let marked: Vec<_> = report.facts.iter().filter(|fact| why(fact).contains(NON_ASCII)).collect();
    assert!(!marked.is_empty(), "{:#?}", report.facts);
    for fact in &marked {
        assert!(fact.spotlight && why(fact).contains(&format!("{NON_ASCII}: `\u{441}harge` (`\\u{{441}}harge`)")), "{fact:#?}");
    }
    assert!(report.facts.iter().any(|fact| fact.description.contains("`\u{441}harge` (`\\u{441}harge`)")), "{:#?}", report.facts);
    // Batch 7b: the derived rename fact has the escape form and the spotlight.
    let report = run_derived(before, &after, &[("demo::\u{441}harge", equal("n0")), ("demo::total", equal("n0"))]);
    let renamed = report.facts.iter().find(|fact| fact.description.starts_with("renamed `demo::charge`")).unwrap_or_else(|| panic!("{:#?}", report.facts));
    assert!(renamed.spotlight && why(renamed).contains(&format!("{NON_ASCII}: `\u{441}harge` (`\\u{{441}}harge`)")) && renamed.description.contains("\\u{441}harge`)"), "{renamed:#?}");
    assert!(!report.accepted(), "the spotlight needs a reason");
    // An ASCII rename has no such spotlight.
    let report = run_derived(before, &before.replace("charge", "charge_fee"), &[]);
    assert!(!report.facts.iter().any(|fact| why(fact).contains(NON_ASCII)), "{:#?}", report.facts);
}

#[test]
fn a_changed_literal_with_a_new_character_outside_ascii_needs_a_reason() {
    // Batch 5w, part B (trial 7 on humantime): `"µs"` (U+00B5) became `"μs"` (U+03BC). The
    // two texts look equal. The fact showed no escape form and had no spotlight.
    use crate::facts::{change_sides, marks, new_literals, non_ascii_names, with_ascii_forms, NewLiteral, NON_ASCII, NON_ASCII_LITERAL, NON_ASCII_NOTE};
    let literals = |text: &str| marks(text).into_iter().filter(|mark| mark.literal).map(|mark| mark.text).collect::<Vec<_>>();
    assert_eq!(literals("f(\"a\\\"b\", 'x', '\\n', '\\u{3bc}', b\"q\", r#\"a \" b\"#, br\"z\") + g::<'a>(&'b str)"), ["\"a\\\"b\"", "'x'", "'\\n'", "'\\u{3bc}'", "\"q\"", "r#\"a \" b\"#", "br\"z\""]);
    assert_eq!(literals("the test's call of `'\u{3bc}'`"), ["'\u{3bc}'"]);
    assert!(non_ascii_names("match c { '\u{3bc}' => 1, _ => r\"\u{3bc}s\".len() }").is_empty(), "the text of a literal is not a name");
    assert_eq!(with_ascii_forms("changed pattern `\"us\" | \"\u{b5}s\"` \u{2192} `\"us\" | \"\u{3bc}s\"` at `demo::f`"), "changed pattern `\"us\" | \"\u{b5}s\"` (`\"\\u{b5}s\"`) \u{2192} `\"us\" | \"\u{3bc}s\"` (`\"\\u{3bc}s\"`) at `demo::f`");
    assert_eq!(with_ascii_forms("added the arm `'\u{3bc}' => 1`"), "added the arm `'\u{3bc}' => 1` (`'\\u{3bc}'`)");
    // Batch 5y, C4: the cut mark of Semilla gets no escape. The text before the cut gets it.
    assert_eq!(with_ascii_forms("added the statement `let a = \"abc def\u{2026}` at `demo::f`"), "added the statement `let a = \"abc def\u{2026}` at `demo::f`");
    assert_eq!(with_ascii_forms("added the statement `let a = \"\u{3bc}s\u{2026}` at `demo::f`"), "added the statement `let a = \"\u{3bc}s\u{2026}` (`\"\\u{3bc}s`) at `demo::f`");
    assert_eq!(change_sides("value changed: \"a \u{2192} b\" \u{2192} \"c\""), Some(("value changed: \"a \u{2192} b\"", "\"c\"")));
    let new = |text: &str, chars: &[char]| NewLiteral { text: text.to_string(), chars: chars.to_vec() };
    assert_eq!(new_literals("\"usec\" | \"us\" | \"\u{b5}s\"", "\"usec\" | \"us\" | \"\u{3bc}s\""), [new("\"\u{3bc}s\"", &['\u{3bc}'])]);
    assert_eq!(new_literals("\"ns\"", "\"n\u{455}\""), [new("\"n\u{455}\"", &['\u{455}'])]);
    // The old literal has the character at that place: no new character.
    assert_eq!(new_literals("\"caf\u{e9}\"", "\"caf\u{e9}s\""), [new("\"caf\u{e9}s\"", &[])]);
    // The order of the literals changes nothing.
    assert_eq!(new_literals("\"\u{b5}s\" | \"us\"", "\"us\" | \"\u{3bc}s\""), [new("\"\u{3bc}s\"", &['\u{3bc}'])]);
    // A new literal with no old literal: no character list.
    assert_eq!(new_literals("\"us\"", "\"us\" | \"\u{3bc}s\""), [new("\"\u{3bc}s\"", &[])]);
    assert!(new_literals("\"\u{3bc}s\"", "\"\u{3bc}s\" | \"us\"").is_empty());

    let why = |fact: &semilla_core::report::Fact| fact.spotlight_why.clone().unwrap_or_default();
    let base = "pub const UNIT: &str = \"\u{b5}s\";\npub const MARK: char = 'u';\npub fn unit(text: &str) -> u32 {\n    match text {\n        \"usec\" | \"us\" | \"\u{b5}s\" => 1,\n        \"nsec\" | \"ns\" => 2,\n        _ => 0,\n    }\n}\n";
    let check = |after: &str, literal: &str, c: char| {
        let report = run_derived(base, after, &[]);
        let marked: Vec<_> = report.facts.iter().filter(|fact| why(fact).contains(NON_ASCII_LITERAL)).collect();
        assert_eq!(marked.len(), 1, "{:#?}", report.facts);
        let form = semilla_core::text::ascii_form(literal).unwrap();
        assert!(marked[0].spotlight && why(marked[0]).contains(&format!("{NON_ASCII_LITERAL}: `{c}` (`\\u{{{:x}}}`) in `{literal}` (`{form}`)", c as u32)), "{:#?}", marked[0]);
        assert!(marked[0].description.contains(&format!("{literal}` (`{form}`)")) || marked[0].description.contains(&format!("{literal} (`{form}`)")), "{:#?}", marked[0]);
        assert!(!report.accepted() && !why(marked[0]).contains(NON_ASCII), "{:#?}", report.facts);
        // One reason makes the step accepted.
        let (_d1, old) = load(base);
        let (_d2, new) = load(after);
        let mut step = step(vec![]);
        step.reasons = [(marked[0].at.as_ref().unwrap().to_string(), "the Greek letter is the SI form".to_string())].into();
        let report = verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &FakeBodies(Default::default()), &NoOracle, true).report;
        assert!(report.accepted(), "{:#?}", report.facts);
    };
    // A pattern in a body, a second pattern with a Cyrillic letter, a constant value, a char.
    check(&base.replace("\"us\" | \"\u{b5}s\"", "\"us\" | \"\u{3bc}s\""), "\"\u{3bc}s\"", '\u{3bc}');
    check(&base.replace("\"nsec\" | \"ns\"", "\"nsec\" | \"n\u{455}\""), "\"n\u{455}\"", '\u{455}');
    check(&base.replace("UNIT: &str = \"\u{b5}s\"", "UNIT: &str = \"\u{3bc}s\""), "\"\u{3bc}s\"", '\u{3bc}');
    check(&base.replace("MARK: char = 'u'", "MARK: char = '\u{3bc}'"), "'\u{3bc}'", '\u{3bc}');
    // A literal that keeps its characters outside ASCII: the escape form, and no spotlight.
    let report = run_derived(base, &base.replace("UNIT: &str = \"\u{b5}s\"", "UNIT: &str = \"\u{b5}sec\""), &[]);
    assert!(report.facts.iter().all(|fact| !fact.spotlight), "{:#?}", report.facts);
    assert!(report.facts.iter().any(|fact| fact.description.contains("\"\u{b5}sec\" (`\"\\u{b5}sec\"`)")), "{:#?}", report.facts);
    // A new literal in new code: a note with the escape form, no spotlight, also for a char.
    let after = format!("{base}pub fn sign(c: char) -> &'static str {{\n    match c {{\n        '\u{3bc}' => \"\u{3bc}s\",\n        _ => \"s\",\n    }}\n}}\n");
    let report = run_derived(base, &after, &[]);
    assert!(report.accepted(), "{:#?}", report.facts);
    assert!(report.facts.iter().all(|fact| !fact.spotlight), "{:#?}", report.facts);
    let note = report.facts.iter().find_map(|fact| fact.note.clone()).unwrap_or_default();
    assert_eq!(note, format!("{NON_ASCII_NOTE} `'\u{3bc}'` (`'\\u{{3bc}}'`), `\"\u{3bc}s\"` (`\"\\u{{3bc}}s\"`)"), "{:#?}", report.facts);
    // An edit with ASCII literals only has no note and no spotlight.
    let report = run_derived(base, &base.replace("\"nsec\" | \"ns\"", "\"nsec\" | \"nanos\""), &[]);
    assert!(report.facts.iter().all(|fact| !fact.spotlight && fact.note.is_none() && !fact.description.contains("\\u{")), "{:#?}", report.facts);
}

#[test]
fn a_std_type_of_a_call_names_the_call_and_not_a_drop() {
    // Batch 5k, D9 (trial 6): `*RATE` with `static RATE: LazyLock<u64>` is a call of
    // `Deref::deref` with the type `LazyLock`. The body drops no `LazyLock`.
    let before = "pub fn other() {}\n";
    let after = "pub fn other() {}\npub fn save() -> u64 {\n    1\n}\n";
    let check = |drop_calls: &[(&str, &str)]| {
        let (_d1, old) = load(before);
        let (_d2, new) = load(after);
        let step = step(vec![claim("c1", "calls-only", Some("demo::save"), serde_json::json!({ "calls": [] }))]);
        let facts = BodyFacts {
            name: "demo::save".into(),
            externals: vec!["std::ops::Deref::deref".to_string()],
            origins: [("std::ops::Deref::deref".to_string(), ("core".to_string(), "core::ops::deref::Deref::deref".to_string()))].into(),
            drop_types: vec![("std::sync::lazy_lock::LazyLock".to_string(), "std".to_string())],
            drop_calls: drop_calls.iter().map(|(ty, call)| (ty.to_string(), call.to_string())).collect(),
            ..Default::default()
        };
        let result = verify_step(&StepInput { step: &step, before: &old, after: &new }, &MirFacts(vec![facts]), &NoOracle).report.claims[0].clone();
        let ClaimStatus::Refuted { reason } = result.status else { panic!("{:?}", result.status) };
        (reason, result.complete_calls)
    };
    // The type comes only from the generic arguments of a call: the text names the call.
    let (reason, complete) = check(&[("std::sync::lazy_lock::LazyLock", "std::ops::Deref::deref")]);
    assert!(reason.contains("a call of `std::ops::Deref::deref` with the type `std::sync::lazy_lock::LazyLock` in `demo::save` (the call can run the code of this type of `std`: a trait impl or the destructor)"), "{reason}");
    assert!(!reason.contains("a drop of"), "{reason}");
    assert_eq!(complete, vec!["std::sync::lazy_lock".to_string()], "the same list of `calls` for the two causes");
    // The body drops a value of the type itself: the text names the drop.
    let (reason, complete) = check(&[]);
    assert!(reason.contains("a drop of a value of `std::sync::lazy_lock::LazyLock` in `demo::save`") && !reason.contains("a call of"), "{reason}");
    assert_eq!(complete, vec!["std::sync::lazy_lock".to_string()]);
}

#[test]
fn a_pair_with_equal_texts_names_the_path_that_points_to_a_different_item() {
    // Batch 5k, B5 (trial 6, attempt 8): the body of `over` moves to a module where `LIMIT` is
    // a different constant. The warning names the path and the two items. The advice "own
    // step" does not help: the step has only the move.
    let over = "    pub fn over(amount: u64) -> bool {\n        let scale = SCALE;\n        amount * scale >= LIMIT\n    }\n";
    let module = |name: &str, head: &str, with: bool| format!("pub mod {name} {{\n{head}{}}}\n", if with { over } else { "" });
    let a = "    pub const LIMIT: u64 = 50;\n    pub const SCALE: u64 = 1;\n";
    let b = "    pub const LIMIT: u64 = 5;\n    pub use crate::a::SCALE;\n";
    let c = "    use crate::a::{LIMIT, SCALE};\n";
    let before = format!("{}{}{}", module("a", a, true), module("b", b, false), module("c", c, false));
    let pairs = |after: &str, to: &str| {
        let (_d1, old) = load(&before);
        let (_d2, new) = load(after);
        let set = ChangeSet {
            base_tree: "a".into(),
            head_tree: "b".into(),
            steps: vec![step(vec![claim("c1", "remove-item", Some("demo::a::over"), serde_json::json!({})), claim("c2", "add-item", Some(to), serde_json::json!({}))])],
            ..Default::default()
        };
        let mut loader = |tree: &str| Ok(if tree == "a" { old.clone() } else { new.clone() });
        verify_changeset(&set, &mut loader, &NoOracle, &NoOracle, Options::default()).0.pairs
    };
    // To `b`: `LIMIT` is a different item. `SCALE` is the same item through the `pub use`.
    let found = pairs(&format!("{}{}{}", module("a", a, false), module("b", b, true), module("c", c, false)), "demo::b::over");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].resolves, [semilla_core::report::PairName { name: "LIMIT".into(), old: "demo::a::LIMIT".into(), new: "demo::b::LIMIT".into() }]);
    let text = found[0].text();
    assert_eq!(text, "`demo::a::over` (removed) and `demo::b::over` (added) have the same text, but a name in the text points to a different item: `LIMIT` is `demo::a::LIMIT` in the removed item and `demo::b::LIMIT` in the added item. The move changes the meaning of the code: it is not one item in a new place");
    assert!(!text.contains("no other change") && !text.contains("similar"), "{text}");
    // To `c`: each name points to the same item. The usual text.
    let found = pairs(&format!("{}{}{}", module("a", a, false), module("b", b, false), module("c", c, true)), "demo::c::over");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].resolves.is_empty() && found[0].text() == "`demo::a::over` (removed) and `demo::c::over` (added) are 100% similar: they can be one item with a new place and a second change", "{:?}", found[0]);
}

/// The source of the opaque input tests (batch 5l).
const OPAQUE_SRC: &str = "#[cfg(debug_assertions)]
pub fn strict() -> bool { true }
#[cfg(not(debug_assertions))]
pub fn strict() -> bool { false }
pub fn wide() -> bool { #[cfg(target_pointer_width = \"64\")] { return true; } false }
pub fn plain() -> bool { true }
pub fn outer() -> bool { strict() }
#[cfg(unix)]
pub type Word = u64;
pub fn word() -> Word { 0 }
pub struct Pad { pub a: u8, #[cfg(unix)] pub b: u64 }
pub struct Wrap(pub Pad);
pub struct Tool;
#[cfg(test)]
impl Tool { pub fn probe(&self) -> bool { true } }
pub struct Lock;
#[cfg(unix)]
impl Drop for Lock { fn drop(&mut self) {} }
pub const DEEP: u64 = { let v = 1; #[cfg(unix)] let v = 2; v };
pub struct Bits<const N: usize = { usize::BITS as usize }>;
pub struct Byte<const N: usize = 8>;
pub fn helper() -> bool { true }
pub fn user() -> bool { plain() }
#[cfg(test)]
mod tests {
    pub fn check() -> bool { super::helper() }
    #[test]
    fn t() { assert!(check()); }
}
";

#[test]
fn the_items_of_the_build_configuration_are_opaque_inputs() {
    let (_d0, before) = load(OPAQUE_SRC);
    let (_d1, after) = load(&OPAQUE_SRC.replace("pub fn user() -> bool { plain() }", "pub fn user() -> bool { strict() && wide() && plain() }"));
    let opaque = crate::opaque::Opaque::new(&before, &after);
    let callee = |name: &str| opaque.callee(true, name, None, &[]);
    // A callee: free only with no attribute above it, with a text that passes the source rule,
    // and with no name of the build configuration in its text.
    assert!(!callee("demo::plain") && !callee("demo::helper"));
    assert!(callee("demo::wide"), "a `cfg` attribute on a statement");
    // The call of `strict` stays a node (batch 5o, A3): the two definitions have the same
    // signature, and no other item can get the name.
    assert!(!callee("demo::outer"), "a call of a function under `cfg` with one signature");
    assert!(callee("demo::word"), "an alias under `cfg` in the signature");
    assert!(callee("demo::strict"), "two definitions: the name has no item");
    assert!(callee("demo::not_in_the_tree"), "the default is opaque");
    assert!(callee("demo::tests::check"), "a module under `cfg`");
    // A type: opaque with a field or a trait `impl` under `cfg`, also inside a different type.
    // An inherent `impl` under `cfg` makes only its members opaque.
    let layout = |name: &str| opaque.layout(&[name.to_string()]);
    // `Pad` is in the hold set (batch 5o): a move of the value is free, a read of its layout
    // is not.
    assert!(!layout("demo::Pad") && layout("touch demo::Pad") && layout("demo::Wrap") && layout("demo::Lock") && layout("?"));
    assert!(!layout("demo::Tool") && !layout("std::string::String"));
    // A type of a crate outside the workspace (P10): the verifier cannot read its definition.
    assert!(layout("foreign dep::Tag") && layout("foreign dep::Wrap<u8>"));
    assert!(opaque.callee(true, "demo::plain", None, &["demo::Wrap".to_string()]));
    assert!(!opaque.callee(true, "demo::plain", None, &["demo::Pad".to_string()]));

    // A body with a text edit: a hard name stops the proof. Each new use is in the list.
    let body = |name: &str, status: BodyStatus| BodyComparison { name: name.to_string(), span: None, status, config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None };
    // The call of `strict` is a node (batch 5o, A3): the driver gives the item under `cfg`,
    // and the new call makes the MIR different.
    opaque.note(true, "demo::user", None, &["cfg demo::strict".to_string()]);
    let mut bodies = vec![body("demo::user", different(false)), body("demo::outer", equal("n0")), body("demo::helper", equal("n0"))];
    opaque.apply(&mut bodies, &BTreeMap::new());
    assert_eq!(bodies[0].status, different(false));
    assert_eq!(bodies[0].config, vec!["`wide` (`#[cfg(target_pointer_width=\"64\")]`)".to_string(), "`strict` (2 definitions, one under `#[cfg(not(debug_assertions))]`)".to_string()]);
    // No text edit: the status stays.
    assert_eq!(bodies[1].status, equal("n0"));
    assert_eq!(bodies[2].status, equal("n0"));

    // A use in code with no change: no proof, and no entry for the spotlight. The module
    // `tests` under `#[cfg(test)]` does not stop a body outside it.
    // The call of `strict` stays a node, so an edit in a different part keeps the proof.
    let (_d2, after) = load(&OPAQUE_SRC.replace("pub fn outer() -> bool { strict() }", "pub fn outer() -> bool { strict() || false }").replace("pub fn helper() -> bool { true }", "pub fn helper() -> bool { !false }").replace("pub fn word() -> Word { 0 }", "pub fn word() -> Word { 0 + 0 }"));
    let opaque = crate::opaque::Opaque::new(&before, &after);
    let mut bodies = vec![body("demo::outer", equal("n2")), body("demo::helper", equal("n2")), body("demo::tests::check", equal("n0")), body("demo::word", equal("n2"))];
    opaque.apply(&mut bodies, &BTreeMap::new());
    assert_eq!(bodies[0].status, equal("n2"));
    assert!(bodies[0].config.is_empty(), "{:?}", bodies[0].config);
    assert_eq!(bodies[1].status, equal("n2"));
    assert_eq!(bodies[2].status, equal("n0"));
    assert_eq!(bodies[3].status, different(true));
    assert!(bodies[3].config.is_empty(), "{:?}", bodies[3].config);

    // A constant with a `cfg` attribute in its initializer, and a member of an `impl` under
    // `cfg`: a body that names one gets no proof.
    let (_d3, after) = load(&OPAQUE_SRC.replace("pub fn user() -> bool { plain() }", "pub fn user() -> bool { DEEP == 1 && Tool.probe() }"));
    let opaque = crate::opaque::Opaque::new(&before, &after);
    let mut bodies = vec![body("demo::user", equal("n2"))];
    opaque.apply(&mut bodies, &BTreeMap::new());
    assert_eq!(bodies[0].status, different(true));
    assert_eq!(bodies[0].config, vec!["`DEEP` (`#[cfg(unix)]`)".to_string(), "`probe` (an `impl` under `#[cfg(test)]`)".to_string()]);

    // The default of a `const` parameter is free only if it is a literal.
    assert!(layout("demo::Bits") && !layout("demo::Byte"));
    let (_d4, after) = load(&OPAQUE_SRC.replace("pub fn user() -> bool { plain() }", "pub fn user() -> bool { let _ = (Bits::<64>, Byte::<8>); plain() }"));
    let (_d5, last) = load(&OPAQUE_SRC.replace("pub fn user() -> bool { plain() }", "pub fn user() -> bool { let _ = (<Bits>::default(), <Byte>::default()); plain() }"));
    let opaque = crate::opaque::Opaque::new(&after, &last);
    let mut bodies = vec![body("demo::user", equal("n0"))];
    opaque.apply(&mut bodies, &BTreeMap::new());
    assert_eq!(bodies[0].status, different(true));
}

/// The source of the exact rule tests (batch 5o).
const EXACT_SRC: &str = "mod block {
    #[cfg(unix)]
    mod default { pub fn make() -> u64 { 1 } }
    #[cfg(not(unix))]
    mod default { pub fn make() -> u64 { 2 } }
    pub fn pick() -> u64 { default::make() }
    pub fn fresh() -> u64 { Default::default() }
}
pub fn outside() -> u64 { Default::default() }
pub struct Tool;
#[cfg(test)]
impl Tool { pub fn probe(&self) -> bool { true } }
pub struct Other;
impl Other { pub fn probe(&self) -> bool { false } }
pub fn with_tool(t: &Tool) -> bool { t.probe() }
pub fn with_other(o: &Other) -> bool { o.probe() }
#[cfg(unix)]
pub fn strict() -> bool { true }
#[cfg(not(unix))]
pub fn strict() -> bool { false }
pub fn guarded() -> u64 { if !strict() { return 0; } 1 }
#[cfg(unix)]
pub fn lim() -> u64 { 1 }
#[cfg(not(unix))]
pub fn lim() -> u32 { 1 }
pub fn limited() -> bool { lim() == 1 }
#[cfg_attr(unix, inline)]
pub fn quick() -> bool { true }
#[cfg_attr(unix, repr(C))]
pub struct Raw { pub a: u8 }
#[cfg_attr(feature = \"serde\", derive(Serialize))]
pub struct Heap(pub Vec<u8>);
pub fn heap_len(h: &Heap) -> usize { h.0.len() }
pub fn heap_text(h: &Heap) -> usize { h.serialize(); 0 }
pub fn no_heap() -> usize { serialize() }
pub fn serialize() -> usize { 0 }
pub struct Builder<'a> { pub alpha: &'a u8, #[cfg(unix)] pub extra: u64 }
pub struct Bare { #[cfg(unix)] pub extra: u64 }
pub trait Takes { fn take(self) -> u8; }
impl Takes for u8 { fn take(self) -> u8 { 1 } }
#[cfg(unix)]
impl Takes for u16 { fn take(self) -> u8 { 2 } }
pub fn taker() -> u8 { 5.take() }
pub struct Guard;
#[cfg(unix)]
impl Drop for Guard { fn drop(&mut self) {} }
pub fn holds(g: Guard) -> u8 { let _g = g; 1 }
";

#[test]
fn the_exact_rule_uses_the_kind_and_the_scope_of_a_name() {
    let (_d0, before) = load(EXACT_SRC);
    let opaque = crate::opaque::Opaque::new(&before, &before);
    let types = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
    let note = |name: &str, list: &[&str]| opaque.note(true, name, None, &types(list));
    note("demo::with_tool", &["demo::Tool"]);
    note("demo::with_other", &["demo::Other"]);
    note("demo::heap_len", &["demo::Heap"]);
    note("demo::heap_text", &["demo::Heap"]);
    note("demo::no_heap", &["usize"]);
    note("demo::outside", &["u64"]);
    note("demo::block::fresh", &["u64"]);
    note("demo::block::pick", &["u64"]);
    note("demo::guarded", &["cfg demo::strict"]);
    note("demo::limited", &["cfg demo::lim"]);
    note("demo::taker", &["u8"]);
    note("demo::holds", &["demo::Guard"]);
    let callee = |name: &str, list: &[&str]| opaque.callee(true, name, None, &types(list));
    let layout = |name: &str| opaque.layout(&[name.to_string()]);
    // A name of a module under `cfg` catches only a path that starts with the name, in a
    // body of that module. `Default::default()` does not use the module `default`.
    assert!(!callee("demo::outside", &[]) && !callee("demo::block::fresh", &[]));
    assert!(callee("demo::block::pick", &[]), "the path `default::make` in the module");
    // A member of an inactive `impl`: only a body that uses the type.
    assert!(callee("demo::with_tool", &[]), "`probe` on a `Tool`");
    assert!(!callee("demo::with_other", &[]), "`probe` on a different type");
    // A3: two definitions with the same signature stay a call node. Not with two signatures.
    assert!(!callee("demo::guarded", &["cfg demo::strict"]));
    assert!(callee("demo::limited", &["cfg demo::lim"]));
    assert!(callee("demo::strict", &[]) && callee("demo::lim", &[]), "no form looks into the function");
    // A5: `inline` under `cfg_attr` is free. `repr` is not. `derive` makes only the names
    // of the trait hard, for a body that uses the type.
    assert!(!callee("demo::quick", &[]));
    assert!(layout("demo::Raw") && !layout("demo::Heap"));
    assert!(!callee("demo::heap_len", &[]) && !callee("demo::no_heap", &[]));
    assert!(callee("demo::heap_text", &[]), "`serialize` on a `Heap`");
    // The hold set: a struct with a field of plain data under `cfg` and an anchor field. A
    // body that moves the value is free. A body that reads the layout is not.
    assert!(!layout("demo::Builder") && layout("touch demo::Builder") && layout("demo::Bare"));
    // An `impl` under `cfg` with a numeric sibling: the literal `5` can get a different type.
    assert!(callee("demo::taker", &[]));
    // A `Drop` impl under `cfg`: each body with a value of the type.
    assert!(layout("demo::Guard") && callee("demo::holds", &["demo::Guard"]));
    assert!(!layout("demo::Tool") && !layout("demo::Other"));

    // A4: an edit in a body with no use of the build configuration keeps the proof and gets
    // no spotlight. An edit in a different statement of a body with an old use gets no proof
    // and no spotlight. A new use, or an edit in the tokens of the use, gets the spotlight.
    let (_d1, after) = load(&EXACT_SRC.replace("pub fn outside() -> u64 { Default::default() }", "pub fn outside() -> u64 { let v = Default::default(); v }").replace("pub fn pick() -> u64 { default::make() }", "pub fn pick() -> u64 { let _v = 1; default::make() }").replace("pub fn no_heap() -> usize { serialize() }", "pub fn no_heap() -> usize { default::make(); serialize() }"));
    let opaque = crate::opaque::Opaque::new(&before, &after);
    let body = |name: &str, status: BodyStatus| BodyComparison { name: name.to_string(), span: None, status, config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None };
    let mut bodies = vec![body("demo::outside", equal("n2")), body("demo::block::pick", equal("n2")), body("demo::no_heap", equal("n2"))];
    opaque.apply(&mut bodies, &BTreeMap::new());
    assert_eq!(bodies[0].status, equal("n2"));
    assert!(bodies[0].config.is_empty(), "{:?}", bodies[0].config);
    assert_eq!(bodies[1].status, different(true));
    assert!(bodies[1].config.is_empty(), "{:?}", bodies[1].config);
    // `default::make` in the crate root does not name the module `block::default`.
    assert_eq!(bodies[2].status, equal("n2"));
    let (_d2, after) = load(&EXACT_SRC.replace("pub fn fresh() -> u64 { Default::default() }", "pub fn fresh() -> u64 { default::make() }"));
    let opaque = crate::opaque::Opaque::new(&before, &after);
    let mut bodies = vec![body("demo::block::fresh", equal("n2"))];
    opaque.apply(&mut bodies, &BTreeMap::new());
    assert_eq!(bodies[0].status, different(true));
    assert_eq!(bodies[0].config.len(), 1, "{:?}", bodies[0].config);
    assert!(bodies[0].config[0].starts_with("`default` ("), "{:?}", bodies[0].config);
    // A new opaque type in the types of a body, by the driver.
    let (_d3, after) = load(&EXACT_SRC.replace("pub fn serialize() -> usize { 0 }", "pub fn serialize() -> usize { std::mem::size_of::<Builder>() }"));
    let opaque = crate::opaque::Opaque::new(&before, &after);
    opaque.note(true, "demo::serialize", None, &types(&["touch demo::Builder"]));
    let mut bodies = vec![body("demo::serialize", different(false))];
    opaque.apply(&mut bodies, &BTreeMap::new());
    assert_eq!(bodies[0].config, vec!["`Builder` (a type with a part under `cfg`)".to_string()]);
}

#[test]
fn an_inner_cfg_attr_puts_no_item_under_cfg_and_a_note_has_a_name() {
    // Batch 7b, part 4.2: `#![cfg_attr(not(feature = "std"), no_std)]` of the crate root
    // removes no item. Before, each item of the crate was an item under `cfg`, and each body
    // with a text edit got the note "this body uses code that a `cfg` attribute can change".
    let body = |name: &str, status: BodyStatus| BodyComparison { name: name.to_string(), span: None, status, config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None };
    let src = "#![cfg_attr(not(feature = \"std\"), no_std)]\npub struct Held(u8);\npub fn hold() -> Held {\n    Held(1)\n}\npub fn keep() -> u8 {\n    let _h = hold();\n    1\n}\n";
    let edit = |src: &str| src.replace("let _h = hold();", "let _ = hold();");
    let run = |src: &str, types: &[&str]| {
        let (_d1, before) = load(src);
        let (_d2, after) = load(&edit(src));
        let opaque = crate::opaque::Opaque::new(&before, &after);
        let types: Vec<String> = types.iter().map(|text| text.to_string()).collect();
        opaque.note(false, "demo::keep", None, &types);
        opaque.note(true, "demo::keep", None, &types);
        let stop = opaque.callee(true, "demo::keep", None, &types);
        let mut bodies = vec![body("demo::keep", equal("n0"))];
        opaque.apply(&mut bodies, &BTreeMap::new());
        (stop, bodies.remove(0))
    };
    let (stop, found) = run(src, &["demo::Held"]);
    assert!(!stop && found.status == equal("n0") && found.stopped.is_empty(), "{found:?}");
    assert!(crate::opaque::inner_keeps_items("#![cfg_attr(not(feature = \"std\"), no_std)]"));
    // An inner `cfg`, and a `cfg_attr` with a content `cfg`, can remove the module.
    assert!(!crate::opaque::inner_keeps_items("#![cfg(unix)]") && !crate::opaque::inner_keeps_items("#![cfg_attr(a, cfg(unix))]"));
    // An outer attribute of an item keeps its rule.
    assert!(!crate::opaque::inner_keeps_items("#[cfg_attr(unix, repr(C))]"));
    // Part 4.1: a stop with no hard name gets the name of the type from the driver. The text
    // "code that a `cfg` attribute can change" is gone.
    let with_drop = format!("{src}#[cfg(unix)]\nimpl Drop for Held {{\n    fn drop(&mut self) {{}}\n}}\n");
    let (stop, found) = run(&with_drop, &["demo::Held"]);
    assert!(stop, "{found:?}");
    assert_eq!(found.stopped, ["`demo::Held` (a type with an `impl` under `#[cfg(unix)]`)"], "{found:?}");
    let (_, found) = run(src, &["foreign dep::Loud"]);
    assert_eq!(found.stopped, ["`dep::Loud` (a type of a different crate, and a feature of that crate can change its parts)"], "{found:?}");
    // An entry with no name gives no cause, and thus no note.
    let (_, found) = run(src, &["?"]);
    assert!(found.stopped.is_empty() && found.status == different(true), "{found:?}");
}

const SCOPE_SRC: &str = "mod block {
    use core::hash::Hash;
    #[cfg(feature = \"wide\")]
    mod wide { pub struct Block(pub u64); }
    #[cfg(feature = \"wide\")]
    pub use self::wide::*;
    #[cfg(not(feature = \"wide\"))]
    mod narrow { use core::mem::*; pub struct Block(pub u32); }
    #[cfg(not(feature = \"wide\"))]
    pub use self::narrow::*;
    impl Block { pub const BITS: usize = 1; pub fn hash(&self) -> u64 { 0 } }
    impl Default for Block { fn default() -> Self { loop {} } }
}
pub type Block = usize;
use block::Block as Wide;
pub struct Set { data: core::ptr::NonNull<Wide>, length: usize }
impl Set {
    pub fn contains(&self, bit: usize) -> bool { bit < self.length }
    pub fn hash(&self) -> u64 { 0 }
}
impl Hash for Set { fn hash<H: core::hash::Hasher>(&self, _state: &mut H) {} }
use core::hash::Hash;
pub fn hashed(set: &Set) -> u64 { set.hash() }
pub fn fresh() -> u64 { Default::default() }
pub fn wide_hash(block: &Wide) -> u64 { block.hash() }
pub enum Error { Short, Odd, #[cfg(feature = \"std\")] Long(u8) }
pub type Result<T> = core::result::Result<T, Error>;
pub fn run() -> Result<u8> { Ok(1) }
#[cfg(feature = \"serde\")]
const BYTES: usize = 8;
#[cfg(feature = \"serde\")]
mod ser {
    use crate::{Set, BYTES};
    impl Set { pub fn bytes(&self) -> usize { BYTES } }
}
pub fn sized(set: &Set) -> usize { set.bytes() }
mod low {
    #[cfg(unix)]
    pub const LIMIT: u8 = 1;
    #[cfg(not(unix))]
    pub const LIMIT: u8 = 2;
    pub fn here() -> u8 { self::LIMIT }
    pub mod deep {
        pub fn up() -> u8 { super::LIMIT }
        pub fn root() -> u8 { crate::LIMIT }
        pub fn other() -> u8 { 3 }
    }
}
pub const LIMIT: u8 = 0;
pub fn root_limit() -> u8 { crate::low::LIMIT }
";

/// Batch 5o, the scope of a name: the qualifiers, an alias, an `impl` with a self type that
/// a glob import gives, and a `use` in a module under `cfg`.
#[test]
fn the_scope_of_a_name_uses_the_module_and_the_self_type() {
    let (_d0, before) = load(SCOPE_SRC);
    let opaque = crate::opaque::Opaque::new(&before, &before);
    let types = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
    let note = |name: &str, list: &[&str]| opaque.note(true, name, None, &types(list));
    note("demo::hashed", &["demo::Set"]);
    note("demo::sized", &["demo::Set"]);
    note("demo::fresh", &["u64"]);
    note("demo::wide_hash", &["demo::block::narrow::Block"]);
    note("demo::run", &["demo::Error"]);
    let callee = |name: &str| opaque.callee(true, name, None, &[]);
    let layout = |name: &str| opaque.layout(&[name.to_string()]);
    // The qualifiers: `self::` is the module of the body, `super::` is a module above it,
    // and `crate::` is the root. The root constant `LIMIT` has one definition.
    assert!(callee("demo::low::here") && callee("demo::low::deep::up") && callee("demo::root_limit"));
    assert!(!callee("demo::low::deep::root") && !callee("demo::low::deep::other"));
    // The self type of `impl Block` in the module `block` comes from a glob import. The
    // alias `Block` of the root is not in the scope of the module. So a member of the
    // `impl` catches only a body with a type that has the name `Block`.
    assert!(!callee("demo::hashed") && !callee("demo::fresh") && !callee("demo::Set::contains"));
    assert!(callee("demo::wide_hash"), "the import `Wide` of a module with a glob `use` under `cfg`");
    assert!(layout("cfg demo::block::narrow::Block") && layout("demo::block::narrow::Block"));
    // A pointer field keeps the type behind it out of the owner. A `use` in a module under
    // `cfg` makes no type opaque. A member of the `impl` in that module is a hard name.
    assert!(!layout("demo::Set"));
    assert!(callee("demo::sized"), "`bytes` on a `Set`");
    // An alias with one definition and a target of the hold set is not a hard name. The
    // compiler gives the target type to the body, and the hold rule applies to that type.
    assert!(!callee("demo::run"));
    assert!(!layout("demo::Error") && layout("touch demo::Error"));
}

/// Batch 5o: the coarse rule stays where the scope does not show the meaning of a name.
#[test]
fn the_coarse_rule_stays_for_a_name_with_no_clear_scope() {
    let callee = |src: &str, name: &str| {
        let (_dir, ws) = load(src);
        crate::opaque::Opaque::new(&ws, &ws).callee(true, name, None, &[])
    };
    // An alias with the name of the self type in the module of the `impl` hides the glob
    // import in each configuration: the name is not a hard name.
    let alias = SCOPE_SRC.replace("    impl Block { pub const BITS", "    pub type Block = usize;\n    impl Block { pub const BITS");
    assert!(!callee(&alias, "demo::hashed"));
    // An import with the name can bring the alias.
    let import = SCOPE_SRC.replace("    impl Block { pub const BITS", "    #[cfg(feature = \"plain\")]\n    use crate::Block;\n    impl Block { pub const BITS");
    assert!(callee(&import, "demo::hashed"));
    // A glob import of a module that Semilla does not know.
    let glob = SCOPE_SRC.replace("    impl Block { pub const BITS", "    use super::*;\n    impl Block { pub const BITS");
    assert!(callee(&glob, "demo::hashed"));
    // A glob import that is not private in a source module gives its names to the importer.
    let public = SCOPE_SRC.replace("mod narrow { use core::mem::*;", "mod narrow { pub use core::mem::*;");
    assert!(callee(&public, "demo::hashed"));
}

/// Batch 5o: the file of a different target (a test) has no `mod` item. Its root is a module,
/// so a function under `cfg_attr` in that file has the scope of that module.
#[test]
fn a_file_of_a_test_target_is_a_module() {
    let lib = "pub fn fresh() -> u64 { Default::default() }\n";
    let test = "#[cfg_attr(unix, demo_macro::wrap)]\nfn default() {}\nfn local() { default() }\n";
    let (_dir, ws) = load_files(&[("Cargo.toml", MANIFEST), ("src/lib.rs", lib), ("tests/checks.rs", test)]);
    let opaque = crate::opaque::Opaque::new(&ws, &ws);
    assert!(!opaque.callee(true, "demo::fresh", None, &[]), "`Default::default()` in the library");
    let local = ws.items.keys().find(|path| path.ends_with("::local")).expect("the test file is in the workspace").clone();
    assert!(opaque.callee(true, &local, None, &[]), "a call of the function in its file");
}

const EXPANDED_SRC: &str = "mod m { pub fn lim() -> u64 { 1 } pub const X: u64 = 1; }
use m::*;
#[cfg(not(debug_assertions))]
fn lim() -> u64 { 2 }
#[cfg(not(debug_assertions))]
const X: u64 = 2;
macro_rules! call { () => { lim() } }
macro_rules! twice { () => { lim() } }
macro_rules! plain { () => { 1u64 } }
pub fn by_macro(a: u64) -> u64 { if call!() != 1 { return 0; } a }
pub fn by_plain(a: u64) -> u64 { let v = plain!() + a; v }
pub fn fresh(a: u64) -> u64 { let v = a; v }
pub fn keeps(a: u64) -> u64 { let v = call!(); let w = a; v + w }
pub fn counts(a: u64) -> u64 { let v = twice!(); let w = a; v + w }
pub fn between(a: u64) -> u64 { if a > 1 { lim() } else { 0 } }
pub fn moved(a: u64) -> u64 { let v = lim(); let w = a + a + a + a; v + w }
pub const BY_VALUE: u64 = X + 1;
pub const BY_MACRO: u64 = call!();
pub const PLAIN: u64 = plain!();
pub fn reads_value() -> u64 { BY_VALUE }
pub fn reads_macro() -> u64 { BY_MACRO }
pub fn reads_plain() -> u64 { PLAIN }
pub struct Holder;
impl Holder { pub const BY_MEMBER: u64 = X; pub const FREE: u64 = 3; }
pub fn reads_member() -> u64 { Holder::BY_MEMBER }
pub fn reads_free() -> u64 { Holder::FREE }
pub struct Sized { pub bytes: [u8; call!() as usize] }
pub struct Unknown { pub bytes: [u8; other::size!()] }
pub struct Known { pub bytes: [u8; plain!() as usize] }
";

/// A name of the expanded code, as the driver gives it.
fn expanded(word: &str, mac: Option<&str>, count: usize) -> crate::ExpandedName {
    crate::ExpandedName { word: word.to_string(), member: false, qual: None, prefix: false, mac: mac.map(str::to_string), line: 1, count, rival: None }
}

/// Batch 5p, B1: a macro puts a name into a body. The text of the body does not have the
/// name. The driver gives the names of the expanded code, and the name rule uses them.
#[test]
fn a_name_from_a_macro_expansion_is_a_use() {
    let (_dir, ws) = load(EXPANDED_SRC);
    let opaque = crate::opaque::Opaque::new(&ws, &ws);
    // The token scan does not see the name `lim` in the body of `by_macro`.
    assert!(!opaque.callee(true, "demo::by_macro", None, &[]));
    assert!(!opaque.callee(true, "demo::by_plain", None, &[]));
    opaque.names(true, "demo::by_macro", None, &[expanded("a", None, 2), expanded("lim", Some("call"), 1)]);
    opaque.names(true, "demo::by_plain", None, &[expanded("a", None, 2), expanded("v", None, 2)]);
    assert!(opaque.callee(true, "demo::by_macro", None, &[]), "the driver name `lim` is a hard name");
    assert!(!opaque.callee(true, "demo::by_plain", None, &[]), "a macro with no hard name");
    // The other side has no driver names for the body.
    assert!(!opaque.callee(false, "demo::by_macro", None, &[]));
    // The union: a name of the text is a use with no driver name for it.
    opaque.names(true, "demo::between", None, &[expanded("a", None, 1)]);
    assert!(opaque.callee(true, "demo::between", None, &[]));
    // A member form with a known receiver module is not the function `lim` of the root.
    let foreign = crate::ExpandedName { member: true, qual: Some("$".to_string()), ..expanded("lim", Some("plain"), 1) };
    opaque.names(true, "demo::by_plain", None, &[foreign]);
    assert!(!opaque.callee(true, "demo::by_plain", None, &[]), "`lim` after a module of a different crate");
}

/// Batch 5p, B2: the changed-code rule for a name from an expansion.
#[test]
fn a_name_from_an_expansion_belongs_to_its_macro_call() {
    let (_d0, before) = load(EXPANDED_SRC);
    let body = |name: &str, status: BodyStatus| BodyComparison { name: name.to_string(), span: None, status, config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None };
    let lim = |mac: &str, count: usize| [expanded("lim", Some(mac), count)];
    let edits = EXPANDED_SRC
        .replace("{ if call!() != 1 { return 0; } a }", "{ a }")
        .replace("pub fn fresh(a: u64) -> u64 { let v = a; v }", "pub fn fresh(a: u64) -> u64 { let v = a + call!(); v }")
        .replace("{ let v = call!(); let w = a; v + w }", "{ let v = call!(); v + a }")
        .replace("macro_rules! twice { () => { lim() } }", "macro_rules! twice { () => { lim() + lim() } }")
        .replace("{ let v = twice!(); let w = a; v + w }", "{ let v = twice!(); v + a }")
        .replace("{ let v = plain!() + a; v }", "{ plain!() + a }");
    let (_d1, after) = load(&edits);
    let opaque = crate::opaque::Opaque::new(&before, &after);
    opaque.names(false, "demo::by_macro", None, &lim("call", 1));
    opaque.names(true, "demo::fresh", None, &lim("call", 1));
    opaque.names(false, "demo::keeps", None, &lim("call", 1));
    opaque.names(true, "demo::keeps", None, &lim("call", 1));
    opaque.names(false, "demo::counts", None, &lim("twice", 1));
    opaque.names(true, "demo::counts", None, &lim("twice", 2));
    let mut bodies = vec![body("demo::by_macro", equal("n2")), body("demo::fresh", equal("n0")), body("demo::keeps", equal("n2")), body("demo::counts", equal("n2")), body("demo::by_plain", equal("n2"))];
    opaque.apply(&mut bodies, &BTreeMap::new());
    // The edit removes the macro call (the probe w3): no proof. The new code has no use.
    assert_eq!(bodies[0].status, different(true));
    assert!(bodies[0].config.is_empty(), "{:?}", bodies[0].config);
    // A new macro call (the probe s3): no proof, and the spotlight names the macro.
    assert_eq!(bodies[1].status, different(true));
    assert_eq!(bodies[1].config.len(), 1, "{:?}", bodies[1].config);
    assert!(bodies[1].config[0].starts_with("`lim` (") && bodies[1].config[0].contains("`call!`"), "{:?}", bodies[1].config);
    // A macro call with no change, and an edit in a different statement: no proof, no spotlight.
    assert_eq!(bodies[2].status, different(true));
    assert!(bodies[2].config.is_empty(), "{:?}", bodies[2].config);
    // The call has no change, but the count of the name is higher: the spotlight.
    assert_eq!(bodies[3].status, different(true));
    assert_eq!(bodies[3].config.len(), 1, "{:?}", bodies[3].config);
    // The control: an honest edit in a body with a macro that gives no hard name.
    assert_eq!(bodies[4].status, equal("n2"));
    assert!(bodies[4].config.is_empty(), "{:?}", bodies[4].config);
}

/// Batch 5p, B3: a hard name in the value of a constant or of an associated constant makes
/// the constant a hard name. A macro call in a text with no driver names gives the words of
/// the macro definition. A macro that is not a macro of the workspace can give each name.
#[test]
fn the_value_of_a_constant_and_a_macro_in_a_declaration_are_inputs() {
    let (_dir, ws) = load(EXPANDED_SRC);
    let opaque = crate::opaque::Opaque::new(&ws, &ws);
    let callee = |name: &str| opaque.callee(true, name, None, &[]);
    assert!(callee("demo::reads_value"), "the value of `BY_VALUE` uses `X`");
    assert!(callee("demo::reads_macro"), "the value of `BY_MACRO` calls a macro with `lim`");
    assert!(!callee("demo::reads_plain"), "the value of `PLAIN` has no hard name");
    assert!(callee("demo::reads_member"), "the value of `Holder::BY_MEMBER` uses `X`");
    assert!(!callee("demo::reads_free"), "a different member of the same `impl`");
    let layout = |name: &str| opaque.layout(&[name.to_string()]);
    assert!(layout("demo::Sized"), "a field type calls a macro with `lim`");
    assert!(layout("demo::Unknown"), "a field type calls a macro of a different crate");
    assert!(!layout("demo::Known") && !layout("demo::Holder"));
}

/// Batch 5p, C1: the spotlight rule uses a token diff. A use between two changes has no
/// change. A use that the edit moves is changed code.
#[test]
fn the_spotlight_rule_uses_the_token_diff() {
    let same = |a: &&str, b: &&str| a == b;
    let diff = |old: &str, new: &str| {
        let (old, new): (Vec<&str>, Vec<&str>) = (old.split(' ').collect(), new.split(' ').collect());
        let (gone, added) = crate::opaque::token_diff(&old, &new, same);
        let pick = |list: &[&str], flags: &[bool]| list.iter().zip(flags).filter(|(_, flag)| **flag).map(|(word, _)| *word).collect::<Vec<_>>().join(" ");
        (pick(&old, &gone), pick(&new, &added))
    };
    assert_eq!(diff("if c { f ( ) } else { 0 }", "match c { true => f ( ) , false => 0 }"), ("if } else {".to_string(), "match true => , false =>".to_string()));
    assert_eq!(diff("a b c", "a b c"), (String::new(), String::new()));
    assert_eq!(diff("a b c d e", "d e a b c"), ("d e".to_string(), "d e".to_string()));
    assert_eq!(diff("x", "y"), ("x".to_string(), "y".to_string()));

    let (_d0, before) = load(EXPANDED_SRC);
    let edits = EXPANDED_SRC
        .replace("{ if a > 1 { lim() } else { 0 } }", "{ match a > 1 { true => lim(), false => 0 } }")
        .replace("{ let v = lim(); let w = a + a + a + a; v + w }", "{ let w = a + a + a + a; let v = lim(); v + w }");
    let (_d1, after) = load(&edits);
    let opaque = crate::opaque::Opaque::new(&before, &after);
    let body = |name: &str| BodyComparison { name: name.to_string(), span: None, status: equal("n2"), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None };
    let mut bodies = vec![body("demo::between"), body("demo::moved")];
    opaque.apply(&mut bodies, &BTreeMap::new());
    // The stop rule has no change: the body has a hard name, so the proof stops.
    assert_eq!(bodies[0].status, different(true));
    assert!(bodies[0].config.is_empty(), "{:?}", bodies[0].config);
    assert_eq!(bodies[1].status, different(true));
    assert_eq!(bodies[1].config.len(), 1, "{:?}", bodies[1].config);
}

/// Batch 5p, C2: a `derive` in a `cfg_attr` is free only for a `derive` that the test
/// `trait_table` compiles. A derive macro of the workspace with such a name is not known.
#[test]
fn a_derive_that_semilla_does_not_know_makes_the_type_opaque() {
    let src = "#[cfg_attr(feature = \"x\", derive(Clone, Serialize))]\npub struct Known(u8);\n#[cfg_attr(feature = \"x\", derive(Display))]\npub struct Shown(u8);\n#[cfg_attr(feature = \"x\", derive(Error))]\npub struct Failed(u8);\n";
    let (_dir, ws) = load(src);
    let opaque = crate::opaque::Opaque::new(&ws, &ws);
    let layout = |opaque: &crate::opaque::Opaque, name: &str| opaque.layout(&[name.to_string()]);
    assert!(!layout(&opaque, "demo::Known"));
    assert!(layout(&opaque, "demo::Shown") && layout(&opaque, "demo::Failed"), "no built-in `derive` has the name");
    let pm = "use proc_macro::TokenStream;\n#[proc_macro_derive(Clone, attributes(skip))]\npub fn clone(input: TokenStream) -> TokenStream { input }\n";
    let manifest = format!("{MANIFEST}\n[workspace]\nmembers = [\"pm\"]\n");
    let (_dir, ws) = load_files(&[("Cargo.toml", &manifest), ("src/lib.rs", src), ("pm/Cargo.toml", "[package]\nname = \"pm\"\nversion = \"0.1.0\"\n[lib]\nproc-macro = true\n"), ("pm/src/lib.rs", pm)]);
    let opaque = crate::opaque::Opaque::new(&ws, &ws);
    assert!(layout(&opaque, "demo::Known"), "the workspace defines a derive macro `Clone`");
}

/// Batch 5q, part B: a call with a rival is an input only if a fact of the kind of the
/// rival can change in a different configuration.
#[test]
fn a_call_with_a_rival_is_an_input_only_with_a_fact_of_its_kind() {
    let body = "pub fn body(a: u64) -> u64 { a.count_ones() as u64 }\npub trait Mark {}\npub trait Else {}\npub struct Wrap;\npub struct Rest { pub a: u8 }\n";
    let stops = |extra: &str, rival: &str| {
        let (_dir, ws) = load(&format!("{body}{extra}"));
        let opaque = crate::opaque::Opaque::new(&ws, &ws);
        opaque.names(true, "demo::body", None, &[crate::ExpandedName { member: true, rival: Some(rival.to_string()), ..expanded("count_ones", None, 1) }]);
        opaque.callee(true, "demo::body", None, &[])
    };
    // No item of the build configuration: only a crate outside the workspace and an
    // unstable item are inputs.
    for free in ["open|text", "auto|text", "closed:Mark:Wrap|text", "closed:Mark:*|text", "open auto|text"] {
        assert!(!stops("", free), "{free}");
    }
    for hard in ["foreign|text", "unstable|text", "open foreign|text", "new_kind|text", "text with no kind"] {
        assert!(stops("", hard), "{hard}");
    }
    // A trait fact: `Wrap: Mark` holds only in one configuration.
    let fact = "#[cfg(feature = \"x\")]\nimpl Mark for Wrap {}\n";
    for hard in ["open|text", "auto|text", "closed:Mark:Wrap|text", "closed:Mark:*|text", "closed:Else:Wrap|one\nclosed:Mark:Wrap|two"] {
        assert!(stops(fact, hard), "{hard}");
    }
    for free in ["closed:Mark:Rest|text", "closed:Else:Wrap|text", "closed:Else:*|text", "closed:Else:Wrap|one\nclosed:Mark:Rest|two"] {
        assert!(!stops(fact, free), "{free}");
    }
    // A generic `impl` under `cfg`: the fact has no head.
    let generic = "#[cfg(feature = \"x\")]\nimpl<T> Mark for T {}\n";
    assert!(stops(generic, "closed:Mark:Rest|text") && !stops(generic, "closed:Else:Rest|text"));
    // A known `derive` in a `cfg_attr` gives the fact `Data: Clone`.
    let derive = "#[cfg_attr(feature = \"x\", derive(Clone))]\npub struct Data(u8);\n";
    assert!(stops(derive, "closed:Clone:Data|text") && stops(derive, "open|text") && !stops(derive, "closed:Else:Wrap|text") && !stops(derive, "closed:Clone:Wrap|text"));
    // A `derive` name that an import can hide (part C2): Semilla cannot list the facts.
    assert!(stops(&format!("use fake::Clone;\n{derive}"), "closed:Else:Wrap|text"));
    // A field under `cfg` changes only the auto traits.
    let field = "pub struct Cell { #[cfg(feature = \"x\")] pub ptr: *const u8, pub len: u8 }\n";
    assert_eq!([stops(field, "auto|text"), stops(field, "open|text"), stops(field, "closed:Mark:Wrap|text")], [true, false, false]);
    // Part C3: an `impl` with a macro of a different crate in its header.
    assert!(stops("impl Mark for ext::ty!() {}\n", "closed:Else:Wrap|text"));
    // A trait under `cfg`: its bounds are facts for each type parameter with the trait.
    // Semilla follows the bounds through the traits of the workspace.
    let above = "#[cfg(feature = \"x\")]\npub trait Tr: Mid {}\n#[cfg(not(feature = \"x\"))]\npub trait Tr {}\npub trait Mid: Mark {}\n";
    assert_eq!([stops(above, "closed:Mark:*|text"), stops(above, "closed:Mid:T|text"), stops(above, "open|text"), stops(above, "closed:Else:*|text")], [true, true, true, false]);
    // A member under `cfg`: the bound of an associated type is a fact.
    let member = "pub trait Src { #[cfg(feature = \"x\")] type It: Mark; #[cfg(not(feature = \"x\"))] type It; }\n";
    assert_eq!([stops(member, "closed:Mark:*|text"), stops(member, "closed:Else:*|text")], [true, false]);
    // A bound with a trait of a different crate: Semilla cannot list the facts.
    assert!(stops("#[cfg(feature = \"x\")]\npub trait Tr: ext::Far {}\n", "closed:Else:Wrap|text"));
    assert!(stops("#[cfg(feature = \"x\")]\npub trait Tr { type It: Iterator; }\n", "closed:Else:Wrap|text"));
    // A trait under `cfg` with no bound changes only its own facts.
    assert!(!stops("#[cfg(feature = \"x\")]\npub trait Tr { fn m(&self) where Self: Sized; }\n", "closed:Else:Wrap|text"));
}

/// Batch 5q, C2: the name of a derive macro is a short name. A `use`, a glob `use`, or a
/// `macro_use` can give the name `Clone` to a macro of a different crate.
#[test]
fn a_derive_name_that_an_import_can_hide_makes_the_type_opaque() {
    let base = "#[cfg_attr(feature = \"x\", derive(Clone, Serialize))]\npub struct Known(u8);\n";
    let known = |head: &str| {
        let (_dir, ws) = load(&format!("{head}\n{base}"));
        !crate::opaque::Opaque::new(&ws, &ws).layout(&["demo::Known".to_string()])
    };
    for free in [
        "",
        "use std::fmt::Debug;\nuse serde::{Deserialize, Serialize};\nuse std::collections::*;",
        "pub mod m { pub struct Thing; }\nuse crate::m::Thing;\nuse m::*;",
        "#[macro_use]\nextern crate serde;",
        "extern crate fake;\nuse fake::Other;",
        "pub mod m { pub enum Kind { A } }\nuse m::Kind;\nuse Kind::*;",
        "use serde::{self, Deserialize, Serialize};",
        // A glob `use` gives its names only to its module.
        "pub mod m { use fake::*; }",
        "pub mod m { use fake::*; }\npub mod n { pub fn f() {} }\nuse n::*;\nuse fake::Other;",
        "pub mod m { use fake::*; pub mod n { pub fn f() {} } }\nuse m::n::*;\nuse crate::m::n::f;",
        "pub mod m { pub mod n { pub fn f() {} } }\nuse m::n as p;\nuse p::*;\nuse fake::Other;",
    ] {
        assert!(known(free), "{free}");
    }
    for hidden in [
        "use fake::Clone;",
        "use fake::Other as Clone;",
        "use fake::{inner::{Serialize}, Other};",
        "use fake::*;",
        "use fake::prelude::*;",
        "pub mod m { pub use fake::Clone; }",
        // A glob `use` and an import of a name pass on the names of a module.
        "pub mod m { pub use fake::*; }\nuse m::*;",
        "pub mod m { pub use fake::*; }\nuse crate::m::*;",
        "pub mod m { pub use fake::*; }\nuse self::m::Clone;",
        "pub mod m { pub use fake::*; pub mod n { pub use super::*; } }\nuse m::n::*;",
        "pub mod m { pub use fake::*; }\nuse m as p;\nuse p::*;",
        "pub mod m { pub use fake::*; }\nuse m as p;\nuse p::Clone;",
        // A path through an `extern crate` item, and a path that starts at the crates.
        "pub mod m { pub extern crate fake; }\nuse m::fake::Clone;",
        "pub mod m { pub extern crate fake; }\nuse self::m::fake::*;",
        "pub mod fake { pub fn f() {} }\nuse ::fake::Clone;",
        "#[macro_use]\nextern crate fake;",
        "#[macro_use(Clone)] extern crate fake;",
        "extern crate fake as serde;",
        // The module `inner` has no item `m`. Thus `m` can be a crate.
        "pub mod m { pub struct Clone; }\npub mod inner { use m::Clone; }",
        "use fake::derives;\nuse derives::Clone;",
        "use fake as std;\nuse std::Clone;",
        "pub mod a { pub mod serde { pub use fake::Fake; } }\npub mod b { use super::a::*;\nuse serde::Fake as Clone; }",
    ] {
        assert!(!known(hidden), "{hidden}");
    }
    // A glob `use` of a workspace crate that has a glob `use` of a different crate.
    let manifest = format!("{MANIFEST}\n[workspace]\nmembers = [\"other\"]\n");
    let other = "[package]\nname = \"other\"\nversion = \"0.1.0\"\n";
    let chain = |other_src: &str| {
        let (_dir, ws) = load_files(&[("Cargo.toml", &manifest), ("src/lib.rs", &format!("use other::*;\n{base}")), ("other/Cargo.toml", other), ("other/src/lib.rs", other_src)]);
        !crate::opaque::Opaque::new(&ws, &ws).layout(&["demo::Known".to_string()])
    };
    assert!(chain("pub fn f() {}\n"));
    assert!(!chain("pub use fake::*;\n"));
}

/// Batch 5q, C3: a macro call in a type position of a signature, a field, or an alias. A
/// macro that is not a macro of the workspace can give each type: the item is opaque.
#[test]
fn a_macro_of_a_different_crate_in_a_type_position_makes_the_item_opaque() {
    let src = "macro_rules! own { () => { u8 } }
pub struct Field { pub a: ext::ty!() }
pub struct Plain { pub a: own!() }
pub type Alias = ext::ty!();
pub type OwnAlias = own!();
pub fn sig(a: ext::ty!()) -> u8 { 0 }
pub fn ret() -> ext::ty!() { 0 }
pub fn bound<T>(a: T) -> u8 where T: Into<ext::ty!()> { 0 }
pub fn hidden() -> impl Iterator<Item = ext::ty!()> { std::iter::empty() }
pub fn own_sig(a: own!()) -> u8 { 0 }
pub fn in_body() -> u8 { 0 }
pub struct Holder;
impl Holder { pub fn method(&self, a: ext::ty!()) -> u8 { 0 } pub fn free(&self) -> u8 { 0 } }
pub trait Tr { fn declared(&self) -> ext::ty!(); }
pub fn calls_sig() -> u8 { sig(0) }
pub fn calls_ret() -> u8 { ret() }
pub fn calls_bound() -> u8 { bound(0u8) }
pub fn calls_hidden() -> usize { hidden().count() }
pub fn calls_own() -> u8 { own_sig(0) }
pub fn calls_body() -> u8 { in_body() }
pub fn calls_method(h: &Holder) -> u8 { h.method(0) }
pub fn calls_free(h: &Holder) -> u8 { h.free() }
pub fn calls_declared<T: Tr>(t: &T) { t.declared(); }
pub fn uses_alias(a: Alias) -> u8 { 0 }
pub fn uses_own_alias(a: OwnAlias) -> u8 { 0 }
pub fn made() -> impl Sized { ext::make!() }
pub fn calls_made() { made(); }
pub trait Other { fn other(&self) -> u8 { 0 } }
impl Other for ext::ty!() {}
pub fn calls_other(a: u8) -> u8 { a.other() }
pub const LIMIT: ext::ty!() = 0;
pub fn reads_limit() -> u8 { LIMIT }
";
    let (_dir, ws) = load(src);
    let opaque = crate::opaque::Opaque::new(&ws, &ws);
    let layout = |name: &str| opaque.layout(&[name.to_string()]);
    assert!(layout("demo::Field"));
    assert!(!layout("demo::Plain") && !layout("demo::Holder"));
    let callee = |name: &str| opaque.callee(true, name, None, &[]);
    for hard in ["calls_sig", "calls_ret", "calls_bound", "calls_hidden", "calls_method", "calls_declared", "uses_alias", "calls_other", "reads_limit"] {
        assert!(callee(&format!("demo::{hard}")), "{hard}");
    }
    for free in ["calls_own", "calls_body", "calls_free", "uses_own_alias"] {
        assert!(!callee(&format!("demo::{free}")), "{free}");
    }
    // A hidden type from a macro call in the body. A caller sees only the bounds of the
    // signature. No normal form can look into the body: the source rule stops that.
    assert!(callee("demo::made"));
    assert!(!callee("demo::calls_made"));
}

#[test]
fn the_content_of_a_cfg_attr_decides_its_kind() {
    use crate::opaque::{attr_kind, Attr};
    let kind = |text: &str| attr_kind(text.parse().unwrap());
    for free in ["cfg_attr(unix, inline)", "cfg_attr(unix, inline(always))", "cfg_attr(unix, allow(dead_code), doc = \"text\")", "cfg_attr(docsrs, doc(cfg(feature = \"x\")))", "cfg_attr(unix, must_use, cold, deprecated)", "cfg_attr(docsrs, feature(doc_auto_cfg))"] {
        assert!(matches!(kind(free), Some(Attr::Free)), "{free}");
    }
    for select in ["cfg(unix)", "cfg_attr(unix, repr(C))", "cfg_attr(unix, path = \"a.rs\")", "cfg_attr(unix, macro_use)", "cfg_attr(unix, my_macro)", "cfg_attr(unix, inline, no_mangle)", "cfg_attr(unix, cfg_attr(test, inline))", "cfg_attr(unix, doc = include_str!(\"a.md\"))", "cfg_attr(unix, feature(specialization))"] {
        assert!(matches!(kind(select), Some(Attr::Select)), "{select}");
    }
    assert!(matches!(kind("cfg_attr(feature = \"serde\", derive(Serialize, serde::Deserialize))"), Some(Attr::Derive(names)) if names == ["Serialize", "Deserialize"]));
    assert!(kind("inline").is_none() && kind("derive(Clone)").is_none());
}

/// An oracle with a fixed status and fixed MIR facts for each body (batch 5n).
struct StatusFacts(Vec<(&'static str, BodyStatus)>, Vec<BodyFacts>);

impl BodyOracle for StatusFacts {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        // The span of a body is the span of its MIR facts.
        let span = |name: &str| self.1.iter().find(|facts| facts.name == name).and_then(|facts| facts.span.clone());
        Ok(self.0.iter().map(|(name, status)| BodyComparison { name: name.to_string(), span: span(name), status: status.clone(), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }).collect())
    }
    fn facts(&self, _: &Step) -> anyhow::Result<Option<Vec<BodyFacts>>> {
        Ok(Some(self.1.clone()))
    }
}

/// Batch 5n, A1: a caller of a function whose MIR signature changed (the type behind an
/// `impl Trait` return type) is an affected body of the fact of that function. A callee with the
/// same MIR signature is not a cause.
#[test]
fn a_caller_of_a_changed_mir_signature_goes_under_the_fact_of_the_callee() {
    let src = "pub fn pairs(s: &str) -> impl Iterator<Item = char> + '_ { s.chars() }\npub fn count(s: &str) -> usize { pairs(s).count() }\n";
    let changed = "pub fn pairs(s: &str) -> impl Iterator<Item = char> + '_ { s.chars().rev() }\npub fn count(s: &str) -> usize { pairs(s).count() }\n";
    let bodies = vec![("demo::pairs", different(false)), ("demo::count", different(false))];
    let facts = |signature_changed: bool| vec![BodyFacts { name: "demo::pairs".into(), signature_changed, ..Default::default() }, BodyFacts { name: "demo::count".into(), callees: vec!["demo::pairs".into()], ..Default::default() }];
    let report = run_derived_with(src, changed, &StatusFacts(bodies.clone(), facts(true)));
    assert!(unknown_causes(&report).is_empty(), "{:#?}", report.facts);
    let fact = report.facts.iter().find(|f| f.affected.contains(&"demo::count".to_string())).unwrap_or_else(|| panic!("the fact of the callee owns the caller: {report:#?}"));
    assert!(fact.affected.contains(&"demo::pairs".to_string()) && fact.description.contains("the MIR signature of `demo::pairs` changed"), "{fact:#?}");
    assert!(fact.compare != Compare::CodeEqual && !fact.spotlight, "{fact:#?}");
    // The same MIR signature: the body of the callee is not a part of the MIR of the caller.
    let report = run_derived_with(src, changed, &StatusFacts(bodies, facts(false)));
    assert_eq!(unknown_causes(&report), ["demo::count"], "{:#?}", report.facts);
}

/// Runs a step as the app does: each edit with no claim gets a derived fact.
fn run_derived_with(before: &str, after: &str, oracle: &dyn BodyOracle) -> StepReport {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(Vec::new());
    crate::verify_step_with(&StepInput { step: &step, before: &old, after: &new }, oracle, &NoOracle, true).report
}

/// Batch 5n, A2: an associated constant is a source item. A change in a `const fn` that its
/// initializer runs is the cause of its changed value, and the constant is an affected body of
/// the fact of that function. An edit of the text of the constant owns its body.
#[test]
fn an_associated_constant_goes_under_the_fact_of_its_const_fn() {
    let src = "pub struct Table(pub u8);\nimpl Table {\n    pub const fn new(x: u8) -> Table { Table(x + 1) }\n    pub const fn wrap(x: u8) -> Table { Table::new(x) }\n    pub const FIRST: Table = Table::wrap(1);\n    pub const DEFAULT: Table = Table::FIRST;\n}\npub fn start() -> u8 { Table::DEFAULT.0 }\n";
    let at = |line: usize| Some(("src/lib.rs".to_string(), line));
    let facts = |callees: Vec<String>| {
        vec![
            BodyFacts { name: "demo::Table::new".into(), span: at(3), ..Default::default() },
            BodyFacts { name: "demo::Table::wrap".into(), span: at(4), callees, ..Default::default() },
            BodyFacts { name: "demo::Table::FIRST".into(), span: at(5), ..Default::default() },
            BodyFacts { name: "demo::Table::DEFAULT".into(), span: at(6), ..Default::default() },
            BodyFacts { name: "demo::start".into(), span: at(8), ..Default::default() },
        ]
    };
    let constants = [("demo::Table::FIRST", different(false)), ("demo::Table::DEFAULT", different(false)), ("demo::start", different(false))];
    let bodies: Vec<(&str, BodyStatus)> = [("demo::Table::new", different(false))].into_iter().chain(constants.clone()).collect();
    let changed = src.replace("x + 1", "x + 2");
    let report = run_derived_with(src, &changed, &StatusFacts(bodies.clone(), facts(vec!["demo::Table::new".into()])));
    assert!(unknown_causes(&report).is_empty(), "{:#?}", report.facts);
    let fact = report.facts.iter().find(|f| f.at.as_ref().is_some_and(|at| at.item == "demo::Table::new")).unwrap();
    assert_eq!(fact.affected, ["demo::Table::DEFAULT", "demo::Table::FIRST", "demo::Table::new", "demo::start"], "{fact:#?}");
    assert!(!fact.spotlight && fact.compare != Compare::CodeEqual, "{fact:#?}");
    // No path from the constant to the changed function: the cause is not known. The constant
    // is a source item, so the fact does not say "code that Semilla does not see".
    let report = run_derived_with(src, &changed, &StatusFacts(bodies, facts(Vec::new())));
    assert_eq!(unknown_causes(&report), ["demo::Table::FIRST", "demo::Table::DEFAULT", "demo::start"], "{:#?}", report.facts);
    assert!(report.facts.iter().filter(|f| f.part == "indirect change").all(|f| f.description.starts_with("indirect change, cause not known")), "{:#?}", report.facts);
    // An edit of the text of the constant.
    let changed = src.replace("Table::wrap(1)", "Table::wrap(2)");
    let report = run_derived_with(src, &changed, &StatusFacts(constants.to_vec(), facts(vec!["demo::Table::new".into()])));
    assert!(unknown_causes(&report).is_empty(), "{:#?}", report.facts);
    let fact = report.facts.iter().find(|f| f.affected.contains(&"demo::Table::FIRST".to_string())).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!(fact.affected, ["demo::Table::DEFAULT", "demo::Table::FIRST", "demo::start"], "{fact:#?}");
}

/// A changed default value of a constant of a trait: the edit of the member list of the trait
/// owns the body of the constant and the bodies that read it.
#[test]
fn an_edit_of_a_trait_constant_owns_its_body() {
    let src = "pub trait Size {\n    const BYTES: usize = 4;\n    fn size(&self) -> usize { Self::BYTES }\n}\n";
    let changed = src.replace("= 4", "= 8");
    let facts = vec![BodyFacts { name: "demo::Size::BYTES".into(), span: Some(("src/lib.rs".into(), 2)), ..Default::default() }];
    let report = run_derived_with(src, &changed, &StatusFacts(vec![("demo::Size::BYTES", different(false)), ("demo::Size::size", different(false))], facts));
    assert!(unknown_causes(&report).is_empty(), "{:#?}", report.facts);
    let fact = report.facts.iter().find(|f| f.description.contains("const BYTES")).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!(fact.affected, ["demo::Size::BYTES", "demo::Size::size"], "{fact:#?}");
}

/// Batch 5n, A4: a bench target with the name of the package. Its bodies have source items, and
/// a constant of the bench that reads a changed file is an affected body of the edit of that
/// file. There is no fact "a file that Semilla does not parse".
#[test]
fn a_bench_target_with_the_name_of_the_package_has_source_items() {
    let lib = "pub fn f(x: u32) -> u32 { x + 1 }\n";
    let bench = "const DATA: &[u8] = include_bytes!(\"../src/lib.rs\");\nfn main() {\n    let _ = (DATA.len(), demo::f(1));\n}\n";
    let tree = |lib: &str| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("benches")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), format!("{MANIFEST}\n[[bench]]\nname = \"demo\"\nharness = false\n")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), lib).unwrap();
        std::fs::write(dir.path().join("benches/demo.rs"), bench).unwrap();
        let ws = Workspace::load(dir.path()).unwrap();
        (dir, ws)
    };
    let (_d1, old) = tree(lib);
    let (_d2, new) = tree(&lib.replace("x + 1", "x + 2"));
    let at = |line: usize| Some(("benches/demo.rs".to_string(), line));
    // The MIR names of the bench start with its crate name, which is the name of the library.
    let facts = vec![BodyFacts { name: "demo::DATA".into(), span: at(1), ..Default::default() }, BodyFacts { name: "demo::main".into(), span: at(2), ..Default::default() }];
    let bodies = vec![("demo::f", different(false)), ("demo::DATA", different(false)), ("demo::main", different(false))];
    let step = step(Vec::new());
    let report = crate::verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &StatusFacts(bodies, facts), &NoOracle, true).report;
    assert!(report.facts.iter().all(|f| !f.spotlight), "{:#?}", report.facts);
    assert_eq!(report.facts.len(), 1, "{:#?}", report.facts);
    assert_eq!(report.facts[0].affected, ["demo::demo::DATA", "demo::demo::main", "demo::f"], "{:#?}", report.facts);
    // A rewrite of the library function with equal compiled code: the bodies of the bench
    // still change (they read the text of the file). Each edit fact lists them.
    let (_d3, new) = tree(&lib.replace("x + 1", "1 + x"));
    let facts = vec![BodyFacts { name: "demo::DATA".into(), span: at(1), ..Default::default() }, BodyFacts { name: "demo::main".into(), span: at(2), ..Default::default() }];
    let bodies = vec![("demo::f", equal("n0")), ("demo::DATA", different(false)), ("demo::main", different(false))];
    let report = crate::verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &StatusFacts(bodies, facts), &NoOracle, true).report;
    assert_eq!(descriptions(&report), ["changed expression `x` → `1` at `demo::f/tail/lhs`", "changed expression `1` → `x` at `demo::f/tail/rhs`"], "{:#?}", report.facts);
    // The first fact of the file has the bodies of the bench.
    assert!(report.facts.iter().all(|fact| !fact.spotlight), "{:#?}", report.facts);
    assert_eq!((report.facts[0].compare, &report.facts[0].affected[..]), (Compare::CodeDifferent, &["demo::demo::DATA".to_string(), "demo::demo::main".to_string()][..]), "{:#?}", report.facts);
    assert_eq!((report.facts[1].compare, report.facts[1].affected.len()), (Compare::CodeEqual, 0), "{:#?}", report.facts);
}

/// The two texts of each "`a` → `b`" pair in a fact text.
fn arrow_pairs(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let parts: Vec<&str> = text.split("` → `").collect();
    for pair in parts.windows(2) {
        let old = pair[0].rsplit('`').next().unwrap_or_default();
        let new = pair[1].split('`').next().unwrap_or_default();
        pairs.push((old.to_string(), new.to_string()));
    }
    pairs
}

#[test]
fn a_moved_node_shows_its_two_places() {
    // Batch 5n, B1: a swap of two statements, of two operands, and of two branches. The two
    // sides of a move have the same text: the fact gives the node and its two places.
    let before = "pub fn f(v: &[u32]) -> u32 {\n    let data = v.len() as u32;\n    let here = v[0];\n    data + here\n}\npub fn g(a: bool, b: bool) -> u32 {\n    if a && b {\n        1\n    } else {\n        2\n    }\n}\npub fn h(a: bool) -> Option<u32> {\n    if a {\n        None\n    } else {\n        Some(0)\n    }\n}\n";
    let after = "pub fn f(v: &[u32]) -> u32 {\n    let here = v[0];\n    let data = v.len() as u32;\n    data + here\n}\npub fn g(a: bool, b: bool) -> u32 {\n    if b && a {\n        1\n    } else {\n        2\n    }\n}\npub fn h(a: bool) -> Option<u32> {\n    if !a {\n        Some(0)\n    } else {\n        None\n    }\n}\n";
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(vec![]);
    let bodies: Vec<(String, BodyStatus)> = ["demo::f", "demo::g", "demo::h"].iter().map(|name| (name.to_string(), different(false))).collect();
    let oracle = FakeBodies(bodies.into_iter().collect());
    let report = verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle, true).report;
    let texts: Vec<&str> = report.facts.iter().map(|f| f.description.as_str()).collect();
    for text in &texts {
        for (old, new) in arrow_pairs(text) {
            assert_ne!(old, new, "a fact shows two equal texts: {texts:#?}");
        }
    }
    // Design 41.6: a moved statement shows its two full places.
    assert!(texts.contains(&"moved statement `let data = v.len() as u32;` from `demo::f/S0` to `demo::f/S1`"), "{texts:#?}");
    assert!(texts.contains(&"moved statement `None` from `demo::h/S0/then/tail` to `demo::h/S0/else/tail`"), "{texts:#?}");
    assert!(texts.contains(&"moved statement `Some(0)` from `demo::h/S0/else/tail` to `demo::h/S0/then/tail`"), "{texts:#?}");
    // Fix round 2 (design 41.7): a moved statement has the category of its statement form.
    assert!(report.facts.iter().filter(|f| f.description.starts_with("moved statement")).all(|f| f.category == Category::Semantics), "{texts:#?}");
}

#[test]
fn an_effect_label_is_a_marked_hint_of_a_definite_answer() {
    // Batch 5n, B2: the label of a changed condition is a hint, not a proof. A plain negation gets
    // `negate` for each answer of the solver. `equivalent` and `unrelated` do not show.
    let before = "pub fn f(v: &[u32], sim: f64, n: usize) -> u32 {\n    if sim > 0.7 {\n        return 1;\n    }\n    if v.len() % 2 != 0 {\n        return 2;\n    }\n    if v.len() < n {\n        return 3;\n    }\n    4\n}\n";
    let after = "pub fn f(v: &[u32], sim: f64, n: usize) -> u32 {\n    if !(sim > 0.7) {\n        return 1;\n    }\n    if !(v.len() % 2 == 0) {\n        return 2;\n    }\n    if v.len() <= n {\n        return 3;\n    }\n    4\n}\n";
    let facts = |effect: Effect| -> Vec<String> {
        let (_d1, old) = load(before);
        let (_d2, new) = load(after);
        let step = step(vec![]);
        let oracle = FakeBodies([("demo::f".to_string(), different(true))].into_iter().collect());
        let report = verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &oracle, &FakeExprs(effect), true).report;
        report.facts.iter().map(|f| f.description.clone()).collect()
    };
    for effect in [Effect::Equivalent, Effect::Unrelated, Effect::Narrow, Effect::Widen, Effect::Negate] {
        let texts = facts(effect);
        assert!(texts.iter().all(|t| !t.contains("equivalent") && !t.contains("unrelated")), "{effect:?}: {texts:#?}");
        assert!(texts.iter().any(|t| t.contains("`sim > 0.7` → `!(sim > 0.7)` (effect hint: negate) at ")), "{effect:?}: {texts:#?}");
        assert!(texts.iter().all(|t| !t.contains("(effect: ")), "{effect:?}: {texts:#?}");
    }
    // A definite answer of the solver shows as a hint with the example.
    let texts = facts(Effect::Widen);
    assert!(texts.iter().any(|t| t.contains("`v.len() < n` → `v.len() <= n` (effect hint: widen, example: i = 5, n = 5) at ")), "{texts:#?}");
    // No definite answer: no label.
    let texts = facts(Effect::Equivalent);
    assert!(texts.iter().filter(|t| t.contains("effect")).count() == 1, "{texts:#?}");
}

/// A body oracle that gives each comparison as it is.
struct Comparisons(Vec<BodyComparison>);

impl BodyOracle for Comparisons {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.clone())
    }
}

#[test]
fn equal_code_in_only_some_configurations_is_a_note() {
    // Batch 5u, part A. The body of `f` has a proof in `dev` and no proof in `release`. The
    // fact gets a note. It has no spotlight, it needs no reason, and it is not neutral.
    let before = "pub fn f(a: u64) -> u64 {\n    let b = a;\n    b\n}\npub fn g(a: u64) -> u64 {\n    let b = a;\n    b\n}\n";
    let after = before.replace("let b = a;\n    b\n}\npub fn g", "a\n}\npub fn g");
    let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
    let partial = |name: &str| BodyComparison { name: name.into(), span: None, status: different(false), config: Vec::new(), differs_in: names(&["release"]), equal_in: names(&["dev", "dev, all features"]), stopped: Vec::new(), range: None, elements: None };
    let report = run_derived_with(before, &after, &Comparisons(vec![partial("demo::f")]));
    let fact = report.facts.iter().find(|fact| fact.affected.contains(&"demo::f".to_string())).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!(fact.note.as_deref(), Some("the compiled code is equal in `dev`, `dev, all features`; different in `release`"), "{fact:#?}");
    assert!(report.accepted() && !fact.spotlight && fact.spotlight_why.is_none() && fact.compare != Compare::CodeEqual, "{report:#?}");
    let text = crate::views::text_report(std::slice::from_ref(&report));
    assert!(text.contains("      note: the compiled code is equal in `dev`") && !text.contains("neutral") && !text.contains("proved"), "{text}");

    // Negative: a fact with a second body that has no proof in each configuration gets no
    // note. "Proved" is the intersection of the bodies of the fact.
    let all = before.replace("    let b = a;\n    b\n", "    a\n");
    let none = BodyComparison { name: "demo::g".into(), span: None, status: different(false), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None };
    let report = run_derived_with(before, &all, &Comparisons(vec![partial("demo::f"), none]));
    for fact in &report.facts {
        let both = fact.affected.contains(&"demo::f".to_string()) && fact.affected.contains(&"demo::g".to_string());
        let only_g = fact.affected == ["demo::g".to_string()];
        assert!(!(both || only_g) || fact.note.is_none(), "{fact:#?}");
        assert!(fact.compare != Compare::CodeEqual, "{fact:#?}");
    }
    // Negative: a body with a proof in each configuration gets no note.
    let report = run_derived_with(before, &after, &Comparisons(vec![BodyComparison { name: "demo::f".into(), span: None, status: equal("n2"), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }]));
    assert!(report.facts.iter().all(|fact| fact.note.is_none()), "{report:#?}");
}

#[test]
fn a_condition_that_semilla_did_not_check_is_a_note() {
    // Batch 5u (an addition to part A). Semilla removed the proof of `f` because of a build
    // condition, and the edit does not touch the code that reads the condition. The fact gets
    // a note. It has no spotlight, it needs no reason, and it is not neutral.
    let before = "pub fn f(a: u64) -> u64 {\n    let b = a;\n    b\n}\n";
    let after = before.replace("let b = a;\n    b\n", "a\n");
    let body = |status: BodyStatus, config: &[&str], stopped: &[&str]| BodyComparison { name: "demo::f".into(), span: None, status, config: config.iter().map(|text| text.to_string()).collect(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: stopped.iter().map(|text| text.to_string()).collect(), range: None, elements: None };
    let cause = "`l0` (under `#[cfg(feature=\"fa\")]`)";
    let report = run_derived_with(before, &after, &Comparisons(vec![body(different(true), &[], &[cause, "cfg!(unix)"])]));
    let fact = report.facts.iter().find(|fact| fact.affected.contains(&"demo::f".to_string())).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!(fact.note.as_deref(), Some("this body uses `l0` (under `#[cfg(feature=\"fa\")]`), `cfg!(unix)`, which Semilla did not check (`demo::f`)"), "{fact:#?}");
    assert!(report.accepted() && !fact.spotlight && fact.compare != Compare::CodeEqual, "{report:#?}");
    let text = crate::views::text_report(std::slice::from_ref(&report));
    assert!(text.contains("      note: this body uses `l0`") && !text.contains("neutral") && !text.contains("proof"), "{text}");
    // A use in a changed statement is in the same note. It gives no spotlight (design 35).
    let report = run_derived_with(before, &after, &Comparisons(vec![body(different(true), &["cfg!(unix)"], &[cause])]));
    let fact = report.facts.iter().find(|fact| fact.affected.contains(&"demo::f".to_string())).unwrap_or_else(|| panic!("{report:#?}"));
    assert!(!fact.spotlight && report.accepted(), "{fact:#?}");
    assert_eq!(fact.note.as_deref(), Some("this body uses `l0` (under `#[cfg(feature=\"fa\")]`), `cfg!(unix)`, which Semilla did not check (`demo::f`)"), "{fact:#?}");
    // The data of the note has no part in a decision. A body with equal code keeps its
    // compare result, and it gets the note.
    let report = run_derived_with(before, &after, &Comparisons(vec![body(equal("n0"), &[], &[cause])]));
    assert!(!report.facts.is_empty() && report.facts.iter().all(|fact| fact.compare == Compare::CodeEqual && fact.note.as_deref().is_some_and(|note| note.starts_with("this body uses `l0`"))), "{report:#?}");
    let report = run_derived_with(before, &after, &Comparisons(vec![body(different(true), &[], &[])]));
    assert!(report.facts.iter().all(|fact| fact.note.is_none() && fact.compare != Compare::CodeEqual), "{report:#?}");
}

/// A body oracle with the units of one checked configuration (batch 5w, part A).
struct UnitBodies {
    bodies: Vec<(String, BodyStatus)>,
    /// The true `cfg` names of the unit, and the files that it reads.
    cfg: Vec<&'static str>,
    files: Vec<&'static str>,
    known: bool,
}

impl BodyOracle for UnitBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.bodies.iter().map(|(name, status)| BodyComparison { name: name.clone(), span: None, status: status.clone(), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }).collect())
    }
    fn truths(&self, _: &Step) -> anyhow::Result<[Vec<ConfigTruth>; 2]> {
        let unit = crate::Unit { target: "demo".into(), package: String::new(), cfg: self.cfg.iter().map(|name| name.to_string()).collect(), files: self.files.iter().map(|file| file.to_string()).collect(), root: "src/lib.rs".into() };
        let truth = ConfigTruth { name: "dev".into(), units: if self.known { vec![unit] } else { Vec::new() }, known: self.known };
        Ok([vec![truth.clone()], vec![truth]])
    }
}

fn run_units(before: &[(&str, &str)], after: &[(&str, &str)], oracle: &UnitBodies, reasons: &[(&str, &str)]) -> StepReport {
    let (_d1, old) = load_files(before);
    let (_d2, new) = load_files(after);
    let mut step = step(vec![]);
    step.reasons = reasons.iter().map(|(key, text)| (key.to_string(), text.to_string())).collect();
    verify_step_with(&StepInput { step: &step, before: &old, after: &new }, oracle, &NoOracle, true).report
}

#[test]
fn an_edit_in_code_that_no_checked_configuration_compiles_gets_no_compare() {
    // Batch 5w, part A (trial 7 on humantime). A constant in a module with a false `cfg`
    // condition got `neutral (proved)`: no body changed, and no rule asked if a checked
    // configuration compiles the code. Each edit below is in such code.
    let module = "#[cfg(windows)]\nmod m {\n    pub const LIMIT: u64 = 7;\n    pub static NAME: &str = \"small\";\n    pub type Word = u16;\n    pub fn twice(a: u64) -> u64 {\n        a * 2\n    }\n    pub struct Pair {\n        pub a: u32,\n    }\n    pub enum Kind {\n        One,\n    }\n    impl Pair {\n        pub fn sum(&self) -> u32 {\n            self.a + 1\n        }\n    }\n    use std::collections::BTreeMap;\n    macro_rules! double {\n        ($e:expr) => {\n            $e * 2\n        };\n    }\n    pub mod inner {\n        pub fn deep() -> u32 {\n            3\n        }\n    }\n}\npub struct Shape {\n    pub side: u32,\n    #[cfg(windows)]\n    pub extra: u32,\n}\n#[cfg(windows)]\nimpl Shape {\n    pub const SIDES: u32 = 4;\n}\n#[cfg(windows)]\npub trait Named {\n    const COUNT: u32 = 1;\n}\n#[cfg_attr(unix, cfg(windows))]\npub const BARE: u32 = 11;\npub fn live() -> u32 {\n    1\n}\n";
    let manifest = ("Cargo.toml", MANIFEST);
    let oracle = UnitBodies { bodies: vec![("demo::live".into(), equal("n0"))], cfg: vec!["unix"], files: vec!["src/lib.rs"], known: true };
    let cases: &[(&str, &str, &str)] = &[
        ("a constant value", "LIMIT: u64 = 7", "LIMIT: u64 = 8"),
        ("a static value", "\"small\"", "\"tiny\""),
        ("a type alias", "Word = u16", "Word = u8"),
        ("a function body", "a * 2", "a * 3"),
        ("a function signature", "twice(a: u64) -> u64", "twice(a: u64) -> u32"),
        ("a struct field", "pub a: u32", "pub a: u64"),
        ("an enum variant", "        One,\n", "        One,\n        Two,\n"),
        ("an impl method", "self.a + 1", "self.a + 2"),
        ("a `use` line", "use std::collections::BTreeMap;", "use std::collections::BTreeSet;"),
        ("a `macro_rules!` definition", "$e * 2", "$e * 3"),
        ("a nested module", "            3\n", "            4\n"),
        ("a field with its own condition", "pub extra: u32", "pub extra: u64"),
        ("a constant of an impl block with a condition", "SIDES: u32 = 4", "SIDES: u32 = 5"),
        ("a constant of a trait with a condition", "COUNT: u32 = 1", "COUNT: u32 = 2"),
        ("a `cfg` in a `cfg_attr`", "BARE: u32 = 11", "BARE: u32 = 12"),
        ("an added item", "    pub type Word = u16;\n", "    pub type Word = u16;\n    pub const MORE: u64 = 9;\n"),
        ("a removed item", "    pub static NAME: &str = \"small\";\n", ""),
    ];
    for (name, old, new) in cases {
        assert!(module.contains(old), "{name}");
        let after = module.replacen(old, new, 1);
        let report = run_units(&[manifest, ("src/lib.rs", module)], &[manifest, ("src/lib.rs", &after)], &oracle, &[]);
        assert!(!report.facts.is_empty() && !report.accepted(), "{name}: {report:#?}");
        assert!(report.facts.iter().all(|fact| fact.compare != Compare::CodeEqual), "{name}: {report:#?}");
        let marked: Vec<&semilla_core::report::Fact> = report.facts.iter().filter(|fact| fact.spotlight && fact.spotlight_why.as_deref().is_some_and(|why| why.contains("inactive code: no checked configuration compiles the changed code (`#[cfg") && why.contains("(windows)") && why.contains("at src/lib.rs:"))).collect();
        assert!(!marked.is_empty(), "{name}: {report:#?}");
        // One reason for each fact is sufficient. The key is the key that the output shows.
        let keys: Vec<String> = report.facts.iter().filter(|fact| fact.spotlight).map(|fact| fact.at.as_ref().map(|at| at.to_string()).unwrap_or_else(|| fact.part.clone())).collect();
        let reasons: Vec<(&str, &str)> = keys.iter().map(|key| (key.as_str(), "the edit is for a different target")).collect();
        let report = run_units(&[manifest, ("src/lib.rs", module)], &[manifest, ("src/lib.rs", &after)], &oracle, &reasons);
        assert!(report.accepted() && report.facts.iter().all(|fact| fact.compare != Compare::CodeEqual), "{name}: {report:#?}");
    }
    // Negative: a comment has no token. The edit is free.
    let after = module.replacen("    pub const LIMIT", "    // The limit.\n    pub const LIMIT", 1);
    let report = run_units(&[manifest, ("src/lib.rs", module)], &[manifest, ("src/lib.rs", &after)], &oracle, &[]);
    assert!(report.accepted() && report.facts.iter().all(|fact| !fact.spotlight), "{report:#?}");
    // Negative: the same edits with a condition that is true in the checked unit.
    let live = module.replace("windows", "unix");
    for (name, old, new) in cases {
        let after = live.replacen(old, new, 1);
        let report = run_units(&[manifest, ("src/lib.rs", &live)], &[manifest, ("src/lib.rs", &after)], &oracle, &[]);
        assert!(report.facts.iter().all(|fact| !fact.spotlight_why.as_deref().unwrap_or_default().contains("no checked configuration compiles the changed code")), "{name}: {report:#?}");
    }
    // Negative: an oracle with no configuration list gives no decision.
    let after = module.replacen("LIMIT: u64 = 7", "LIMIT: u64 = 8", 1);
    let report = run_derived(module, &after, &[("demo::live", equal("n0"))]);
    assert!(report.facts.iter().all(|fact| !fact.spotlight), "{report:#?}");
    // A configuration with no unit record: the careful rule. The constant has no body
    // record, so the edit is dead.
    let unknown = UnitBodies { bodies: vec![("demo::live".into(), equal("n0"))], cfg: vec![], files: vec![], known: false };
    let report = run_units(&[manifest, ("src/lib.rs", module)], &[manifest, ("src/lib.rs", &after)], &unknown, &[]);
    assert!(!report.accepted() && report.facts.iter().all(|fact| fact.spotlight && fact.compare != Compare::CodeEqual), "{report:#?}");
}

#[test]
fn an_edit_in_a_file_that_no_checked_unit_reads_is_never_neutral() {
    // Batch 5w, part A. `#[cfg(windows)] mod win;`: the edit is in `win.rs`, and the
    // attribute is in a different file. The record of the compiler has the files of each unit.
    let manifest = ("Cargo.toml", MANIFEST);
    let lib = ("src/lib.rs", "#[cfg(windows)]\nmod win;\n#[cfg(unix)]\nmod nix;\n");
    let file = "pub const SEP: char = '/';\npub fn join(a: u32, b: u32) -> u32 {\n    a + b\n}\n";
    let oracle = UnitBodies { bodies: vec![("demo::nix::join".into(), equal("n0"))], cfg: vec!["unix"], files: vec!["src/lib.rs", "src/nix.rs"], known: true };
    for (old, new) in [("'/'", "':'"), ("a + b", "b + a")] {
        let after = file.replace(old, new);
        let report = run_units(&[manifest, lib, ("src/win.rs", file), ("src/nix.rs", file)], &[manifest, lib, ("src/win.rs", &after), ("src/nix.rs", file)], &oracle, &[]);
        // The edit fact has its text and no compare. The file has the one spotlight (design 37.3).
        let fact = report.facts.first().unwrap_or_else(|| panic!("{report:#?}"));
        assert!(!report.accepted() && !fact.spotlight && fact.compare == Compare::NoCompare, "{report:#?}");
        let fact = report.facts.iter().find(|fact| fact.spotlight).unwrap_or_else(|| panic!("{report:#?}"));
        assert!(report.facts.iter().all(|fact| fact.compare != Compare::CodeEqual) && report.facts.iter().filter(|fact| fact.spotlight).count() == 1, "{report:#?}");
        assert!(fact.spotlight_why.as_deref().unwrap().contains("no checked unit reads the file `src/win.rs`: `#[cfg(windows)]` at `demo::win`"), "{fact:#?}");
        // Negative: the same edit in the file that the unit reads.
        let report = run_units(&[manifest, lib, ("src/win.rs", file), ("src/nix.rs", file)], &[manifest, lib, ("src/win.rs", file), ("src/nix.rs", &after)], &oracle, &[]);
        assert!(report.facts.iter().all(|fact| !fact.spotlight), "{report:#?}");
    }
}

#[test]
fn an_edit_in_a_macro_input_with_a_condition_is_never_neutral() {
    // Batch 5w, part A. The syntax tree does not go into the input of a macro call. One
    // condition that the checked units make true and false is the one free case.
    let manifest = ("Cargo.toml", MANIFEST);
    let source = "macro_rules! pick { ($($t:tt)*) => {}; }\npick! {\n    if #[cfg(windows)] {\n        pub const A: u32 = 1;\n    }\n}\npub fn live() -> u32 {\n    1\n}\n";
    let after = source.replace("A: u32 = 1", "A: u32 = 2");
    let oracle = UnitBodies { bodies: vec![("demo::live".into(), equal("n0"))], cfg: vec!["unix"], files: vec!["src/lib.rs"], known: true };
    let report = run_units(&[manifest, ("src/lib.rs", source)], &[manifest, ("src/lib.rs", &after)], &oracle, &[]);
    let fact = report.facts.first().unwrap_or_else(|| panic!("{report:#?}"));
    assert!(!report.accepted() && fact.spotlight && fact.compare != Compare::CodeEqual, "{report:#?}");
    assert!(fact.spotlight_why.as_deref().unwrap().contains("Semilla cannot see if a checked configuration compiles the changed code (the input of `pick!` at src/lib.rs:2 has a `cfg` condition)"), "{fact:#?}");
}

#[test]
fn an_identity_in_code_that_no_checked_configuration_compiles_has_no_compare() {
    // Batch 5w, part A. A rename in dead code has no body evidence. The claimed identity is
    // unproved. The derived identity does not show, and the facts have the spotlight.
    let manifest = ("Cargo.toml", MANIFEST);
    let source = "#[cfg(windows)]\nmod m {\n    pub fn twice(a: u64) -> u64 {\n        a * 2\n    }\n}\npub fn live() -> u32 {\n    1\n}\n";
    let after = source.replace("twice", "thrice");
    let oracle = UnitBodies { bodies: vec![("demo::live".into(), equal("n0"))], cfg: vec!["unix"], files: vec!["src/lib.rs"], known: true };
    let report = run_units(&[manifest, ("src/lib.rs", source)], &[manifest, ("src/lib.rs", &after)], &oracle, &[]);
    assert!(!report.accepted() && !report.facts.is_empty(), "{report:#?}");
    // Design 38.10: the derived rename shows, with no compare. The edits keep the spotlight.
    assert!(report.facts.iter().all(|fact| fact.compare != Compare::CodeEqual) && report.facts.iter().any(|fact| fact.spotlight), "{report:#?}");
    let rename = report.facts.iter().find(|fact| fact.description == "renamed `demo::m::twice` → `demo::m::thrice`").unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!((rename.compare, rename.note.as_deref()), (Compare::NoCompare, Some("no compare: no checked configuration compiles `demo::m::thrice`")));
    let (_d1, old) = load_files(&[manifest, ("src/lib.rs", source)]);
    let (_d2, new) = load_files(&[manifest, ("src/lib.rs", &after)]);
    let step = step(vec![claim("c1", "rename", Some("demo::m::twice"), serde_json::json!({"to": "thrice"}))]);
    let report = verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle, true).report;
    assert!(matches!(&report.claims[0].status, ClaimStatus::Refuted { reason } if reason.contains("so Semilla cannot compare the code of `demo::m::twice`")), "{report:#?}");
}

/// A step with files, a MIR oracle with a fixed status for each body, and derived facts.
fn run_derived_files(before: &[(&str, &str)], after: &[(&str, &str)], bodies: &[(&str, BodyStatus)]) -> StepReport {
    let (_d1, old) = load_files(before);
    let (_d2, new) = load_files(after);
    let step = step(Vec::new());
    let oracle = FakeBodies(bodies.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
    verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle, true).report
}

/// Batch 5x, part A (trial 7 on `humantime`): a macro import has the scope of its crate root,
/// a `use` line shadows a standard macro only with a name that it brings, and a text that
/// differs only by the names of the rename map has no edit.
#[test]
fn a_macro_import_of_a_different_target_does_not_stop_a_rename() {
    const MANIFEST_OTHER: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nother = \"1\"\n";
    let lib = "use std::fmt::Write;\npub fn show(out: &mut String, n: u32) {\n    item(out, n);\n}\nfn item(out: &mut String, n: u32) {\n    let _ = write!(out, \"{n}\");\n}\n";
    let bodies = [("demo::piece", equal("n1")), ("demo::show", equal("n1"))];
    let run = |head: &str, body: &str, extra: &[(&str, &str)]| {
        let old = format!("{head}{body}");
        let new = old.replace("item(", "piece(");
        let mut before = vec![("Cargo.toml", MANIFEST_OTHER), ("src/lib.rs", old.as_str())];
        let mut after = vec![("Cargo.toml", MANIFEST_OTHER), ("src/lib.rs", new.as_str())];
        before.extend(extra.iter().copied());
        after.extend(extra.iter().copied());
        run_derived_files(&before, &after, &bodies)
    };
    let renamed = |report: &StepReport| descriptions(report).iter().any(|d| d.starts_with("renamed `demo::item` → `demo::piece`"));
    let notes = |report: &StepReport| report.notes.join("\n");
    let macro_use = "#[macro_use]\nextern crate other;\n";

    // The trial: `#[macro_use] extern crate` in a bench target, and a path segment `format`.
    let bench = [("benches/speed.rs", "#[macro_use]\nextern crate other;\nuse other::format::Fixed::*;\nfn main() {}\n")];
    let report = run("", lib, &bench);
    assert!(renamed(&report), "{report:#?}");
    // The same in a test target, an example, and a binary.
    for file in ["tests/it.rs", "examples/demo.rs", "src/bin/tool.rs"] {
        let report = run("", lib, &[(file, "#[macro_use]\nextern crate other;\nuse other::write;\nfn main() {}\n")]);
        assert!(renamed(&report), "{file}: {report:#?}");
    }
    // A segment in the middle of a path brings no name. An alias and a group name bring one.
    let report = run("use other::format::Thing;\nuse other::write::{self as w, vec as list};\n", lib, &[]);
    assert!(renamed(&report), "{report:#?}");

    // A real shadow in the library continues to stop the proof of an edit, with the cause. An
    // import of a different target does not.
    let edit_old = "use std::fmt::Write;\npub fn f(out: &mut String, a: u32) -> u32 {\n    let b = a;\n    let _ = write!(out, \"{b}\");\n    b\n}\n";
    let edit_new = edit_old.replace("let b = a;", "let c = a;").replace("{b}", "{c}").replace("    b\n", "    c\n");
    let edit = |head: &str, extra: &[(&str, &str)]| {
        let (old, new) = (format!("{head}{edit_old}"), format!("{head}{edit_new}"));
        let mut before = vec![("Cargo.toml", MANIFEST_OTHER), ("src/lib.rs", old.as_str())];
        let mut after = vec![("Cargo.toml", MANIFEST_OTHER), ("src/lib.rs", new.as_str())];
        before.extend(extra.iter().copied());
        after.extend(extra.iter().copied());
        let (_d1, old_tree) = load_files(&before);
        let (_d2, new_tree) = load_files(&after);
        (run_derived_files(&before, &after, &[("demo::f", equal("n0"))]).facts, config_causes(&old_tree, &new_tree))
    };
    let proved = |facts: &[semilla_core::report::Fact]| facts.iter().any(|f| f.compare == Compare::CodeEqual);
    let spotlight = |facts: &[semilla_core::report::Fact]| facts.iter().filter_map(|f| f.spotlight_why.clone()).collect::<Vec<_>>().join("\n");
    let shadows = ["use other::write;\n", "use other::{gate, put as write};\n", "pub(crate) use other::write;\n", "mod inner {\n    use other::write;\n}\n", "use other::*;\n", macro_use];
    for head in shadows {
        let (facts, causes) = edit(head, &[]);
        assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.contains("`write!` in `demo::f` (Semilla cannot see into it)"), "{head}: {facts:#?}");
    }
    let (facts, causes) = edit("use other::format::Thing;\n", &bench);
    assert!(proved(&facts) && spotlight(&facts).is_empty() && causes.is_empty(), "{facts:#?}");
    // A `pub use` of a different workspace target can give the name to each crate.
    let (facts, causes) = edit("", &[("src/bin/tool.rs", "pub use other::write;\nfn main() {}\n")]);
    assert!(proved(&facts) && !causes.is_empty(), "{facts:#?}");
    // A rename with a real shadow. Design 35: the compare with the rename map is the result
    // for the checked configurations. A macro call that Semilla cannot see into, with the name
    // in its tokens, does not stop the derivation.
    for head in shadows {
        let report = run(head, lib, &[]);
        assert!(renamed(&report), "{head}: {report:#?}");
        let report = run(head, &lib.replace("    item(out, n);\n", "    let _ = write!(out, \"{}\", item(out, n) == ());\n"), &[]);
        assert!(renamed(&report) && notes(&report).is_empty(), "{head}: {report:#?}");
    }
    // Batch 5x, part C: a pair with a known cause and no second change in its step does not
    // say "own step". It says the cause.
    let pair = semilla_core::PairWarning { old: "demo::item".into(), new: "demo::piece".into(), similarity: 0.9, claims: vec![], refusal: None, reviewer: None, resolves: vec![], stopped: Some("the cause".into()) };
    assert_eq!(pair.text(), "`demo::item` (removed) and `demo::piece` (added) are 90% similar: they can be one item with a new name and a second change. Semilla does not derive one item: the cause");
    // A module file of two crate roots has the imports of the two roots.
    let shared = "use std::fmt::Write;\npub fn show(out: &mut String, n: u32) {\n    item(out, n);\n}\nfn item(out: &mut String, n: u32) {\n    let _ = write!(out, \"{n}\");\n}\n";
    let files = |text: &str| -> Vec<(String, String)> {
        vec![
            ("Cargo.toml".to_string(), MANIFEST_OTHER.to_string()),
            ("src/lib.rs".to_string(), "pub mod util;\n".to_string()),
            ("src/main.rs".to_string(), format!("{macro_use}mod util;\nfn main() {{}}\n")),
            ("src/util.rs".to_string(), text.to_string()),
        ]
    };
    let (_dir, both) = load_files(&files(shared).iter().map(|(a, b)| (a.as_str(), b.as_str())).collect::<Vec<_>>());
    let mut roots = BTreeMap::new();
    crate::srcconfig::file_roots(&both, &mut roots);
    let names = |file: &str| roots.get(file).map(|set: &std::collections::BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    assert_eq!((names("src/lib.rs"), names("src/main.rs"), names("src/util.rs")), ("src/lib.rs".to_string(), "src/main.rs".to_string(), "src/lib.rs src/main.rs".to_string()));

    // A caller with a macro call that Semilla cannot see into: a text with only the new name
    // has no edit. A renamed name in the tokens of the call does not stop the rename: the
    // compare of the compiled code decides (design 35).
    let caller = lib.replace("    item(out, n);\n", "    other::gate!(n);\n    item(out, n);\n");
    let report = run("", &caller, &[]);
    assert!(renamed(&report), "{report:#?}");
    let inside = lib.replace("    item(out, n);\n", "    other::gate!(item(out, n));\n");
    let report = run("", &inside, &[]);
    assert!(renamed(&report) && notes(&report).is_empty(), "{report:#?}");
    // An attribute that Semilla cannot see into, and a `cfg` attribute in the body: the
    // compare decides (design 35), so Semilla derives the rename.
    for form in ["    #[other::watch]\n    item(out, n);\n", "    #[cfg(windows)]\n    let piece = |_: &mut String, _: u32| {};\n    item(out, n);\n"] {
        let report = run("", &lib.replace("    item(out, n);\n", form), &[]);
        assert!(renamed(&report), "{form}: {report:#?}");
    }
    // A condition in a body that has the name: the note of the rename fact names it.
    let before = "fn item() {}\npub fn run() {\n    #[cfg(windows)]\n    let _piece = 1;\n    item();\n}\n";
    let body = |name: &str, stopped: &[&str]| BodyComparison { name: name.into(), span: None, status: equal("n1"), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: stopped.iter().map(|text| text.to_string()).collect(), range: None, elements: None };
    let report = run_derived_with(before, &before.replace("item", "piece"), &Comparisons(vec![body("demo::piece", &[]), body("demo::run", &["#[cfg(windows)]"])]));
    assert_eq!(descriptions(&report), ["renamed `demo::item` → `demo::piece`"], "{report:#?}");
    assert_eq!((report.facts[0].compare, report.facts[0].note.as_deref()), (Compare::CodeEqual, Some("this body uses `#[cfg(windows)]`, which Semilla did not check (`demo::run`)")), "{report:#?}");
}

/// The files of a small crate with the module `date` (batch 5x, part B).
fn move_files<'a>(lib: &'a str, date: &'a str, calendar: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut files = vec![("Cargo.toml", MANIFEST), ("src/lib.rs", lib), ("src/date.rs", date)];
    files.extend(calendar.map(|text| ("src/calendar.rs", text)));
    files
}

fn run_claim_files(before: &[(&str, &str)], after: &[(&str, &str)], claims: Vec<Claim>, bodies: &[(&str, BodyStatus)]) -> StepReport {
    let (_d1, old) = load_files(before);
    let (_d2, new) = load_files(after);
    let step = step(claims);
    let oracle = FakeBodies(bodies.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
    verify_step(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle).report
}

/// Batch 5x, part B (trial 7 on `humantime`): a move into a new module gets its proof in one
/// step, with the claim and also derived. The parts of the move: the item, the `mod` line and
/// the file of the new module, the `use` lines, and a wider visibility (a fact to read).
#[test]
fn a_move_into_a_new_module_gets_its_proof_in_one_step() {
    const DATE: &str = "pub fn parse(y: u64) -> bool {\n    is_leap(y)\n}\nfn is_leap(y: u64) -> bool {\n    y % 4 == 0\n}\n";
    const DATE_AFTER: &str = "use crate::calendar::is_leap;\n\npub fn parse(y: u64) -> bool {\n    is_leap(y)\n}\n";
    const CALENDAR: &str = "pub(crate) fn is_leap(y: u64) -> bool {\n    y % 4 == 0\n}\n";
    let before = move_files("mod date;\n", DATE, None);
    let bodies = [("demo::date::parse", equal("n1")), ("demo::calendar::is_leap", equal("n0"))];
    let claims = || vec![claim("c1", "move-item", Some("demo::date::is_leap"), serde_json::json!({"to": "demo::calendar::is_leap"}))];
    let moved = "moved `demo::date::is_leap` → `demo::calendar::is_leap`";
    let proved = |report: &StepReport| report.accepted() && descriptions(report).iter().any(|text| text.starts_with(moved));

    // 1. The honest move: derived, and with the claim.
    let after = move_files("mod calendar;\nmod date;\n", DATE_AFTER, Some(CALENDAR));
    let report = run_derived_files(&before, &after, &bodies);
    assert!(proved(&report), "{report:#?}");
    let texts = descriptions(&report);
    assert!(!texts.iter().any(|text| text.contains("added") || text.contains("removed")), "{texts:?}");
    // The visibility is a fact to read, with no spotlight.
    let visibility = report.facts.iter().find(|fact| fact.description == "visibility of `demo::calendar::is_leap`: `private` → `pub(crate)`").unwrap_or_else(|| panic!("{texts:?}"));
    assert!(!visibility.spotlight && report.spotlight.is_empty(), "{report:#?}");
    let report = run_claim_files(&before, &after, claims(), &bodies);
    assert!(report.accepted() && status_of(&report, "c1").is_verified() && report.uncovered.is_empty(), "{report:#?}");
    assert!(descriptions(&report).iter().any(|text| text.starts_with("visibility of `demo::calendar::is_leap`")), "{report:#?}");
    // `pub(super)` and a module with a `use` line of its own are also parts of a move.
    let with_use = CALENDAR.replace("pub(crate) fn", "use std::fmt;\npub(super) fn");
    let report = run_derived_files(&before, &move_files("mod calendar;\nmod date;\n", DATE_AFTER, Some(&with_use)), &bodies);
    assert!(proved(&report), "{report:#?}");

    // 2. A second change in the step: the explicit claim gets no proof. The derived move has its rule below.
    let cases: Vec<(&str, String, String, Vec<(&str, BodyStatus)>)> = vec![
        // A different callee after the move: the new module has a different item of the same
        // name in scope. The MIR of the moved body is not equal.
        ("a different callee", "mod calendar;\nmod date;\n".into(), CALENDAR.into(), vec![("demo::date::parse", equal("n1")), ("demo::calendar::is_leap", different(false))]),
        // An edit in the body.
        ("a body edit", "mod calendar;\nmod date;\n".into(), CALENDAR.replace("== 0", "== 0 && y != 0"), vec![("demo::date::parse", equal("n1")), ("demo::calendar::is_leap", different(false))]),
        // A caller that changes.
        ("a changed caller", "mod calendar;\nmod date;\n".into(), CALENDAR.into(), vec![("demo::date::parse", different(false)), ("demo::calendar::is_leap", equal("n0"))]),
        // A `cfg` on the path: the new module has a `cfg` attribute.
        ("a cfg on the module", "#[cfg(not(windows))]\nmod calendar;\nmod date;\n".into(), CALENDAR.into(), bodies.to_vec()),
        ("a cfg in the file", "mod calendar;\nmod date;\n".into(), format!("#![cfg(not(windows))]\n{CALENDAR}"), bodies.to_vec()),
        ("a path attribute", "#[path = \"calendar.rs\"]\nmod calendar;\nmod date;\n".into(), CALENDAR.into(), bodies.to_vec()),
        // A public module is a change of the API.
        ("a public module", "pub mod calendar;\nmod date;\n".into(), CALENDAR.into(), bodies.to_vec()),
        // A second item in the new module.
        ("a second item", "mod calendar;\nmod date;\n".into(), format!("{CALENDAR}pub(crate) fn other() -> u64 {{\n    1\n}}\n"), bodies.to_vec()),
        ("a public import", "mod calendar;\nmod date;\n".into(), format!("pub use std::fmt;\n{CALENDAR}"), bodies.to_vec()),
    ];
    for (name, lib, calendar, bodies) in &cases {
        let after = move_files(lib, DATE_AFTER, Some(calendar));
        let report = run_derived_files(&before, &after, bodies);
        // Design 36.1: a second change does not stop the derived move. Each other change has
        // its own fact, or the move fact has the result `code different`.
        let texts = descriptions(&report);
        let moved = report.facts.iter().find(|fact| fact.description.starts_with("moved `demo::date::is_leap` → `demo::calendar::is_leap`"));
        let has = |start: &str| report.facts.iter().any(|fact| fact.description.starts_with(start));
        match *name {
            "a different callee" => assert!(moved.is_some_and(|fact| fact.compare == Compare::CodeDifferent && fact.mismatch()), "{name}: {report:#?}"),
            "a body edit" => assert!(moved.is_some_and(|fact| fact.compare == Compare::CodeEqual) && report.facts.iter().any(|fact| fact.description.starts_with("changed expression `y % 4 == 0` → `y % 4 == 0 && y != 0` at `demo::calendar::is_leap/") && fact.compare == Compare::CodeDifferent), "{name}: {report:#?}"),
            "a changed caller" => assert!(moved.is_some() && !report.accepted() && has("indirect change, cause not known: `demo::date::parse`"), "{name}: {report:#?}"),
            "a cfg on the module" | "a cfg in the file" => assert!(moved.is_none() && has("removed `demo::date::is_leap`") && report.notes.iter().any(|note| note.contains("changes the `cfg` conditions on its path")), "{name}: {report:#?}"),
            "a path attribute" => assert!(moved.is_some() && !report.accepted() && report.facts.iter().any(|fact| fact.description == "added `demo::calendar`" && fact.spotlight), "{name}: {report:#?}"),
            _ => assert!(moved.is_some() && has("added `demo::calendar`"), "{name}: {texts:?}"),
        }
        let report = run_claim_files(&before, &after, claims(), bodies);
        assert!(!report.accepted() && !status_of(&report, "c1").is_verified(), "{name}: {report:#?}");
    }

    // 3. A move into a module that is not new, with a `cfg` attribute: the path of the item
    // gets a condition, so the move has no proof.
    let old = move_files("#[cfg(not(windows))]\nmod calendar;\nmod date;\n", DATE, Some(""));
    let new = move_files("#[cfg(not(windows))]\nmod calendar;\nmod date;\n", DATE_AFTER, Some(CALENDAR));
    let report = run_derived_files(&old, &new, &bodies);
    assert!(!descriptions(&report).iter().any(|text| text.starts_with("moved ")), "{report:#?}");
    let report = run_claim_files(&old, &new, claims(), &bodies);
    assert!(!report.accepted() && !status_of(&report, "c1").is_verified(), "{report:#?}");
    // The same condition on the two paths: the move has its proof.
    let old = move_files("#[cfg(not(windows))]\nmod calendar;\n#[cfg(not(windows))]\nmod date;\n", DATE, Some(""));
    let new = move_files("#[cfg(not(windows))]\nmod calendar;\n#[cfg(not(windows))]\nmod date;\n", DATE_AFTER, Some(CALENDAR));
    assert!(proved(&run_derived_files(&old, &new, &bodies)));
}

/// Batch 5x, part B, rule 5: a moved constant and a moved struct with its `impl` block.
#[test]
fn a_moved_constant_and_a_moved_struct_get_their_proof() {
    // A constant.
    let before = move_files("mod date;\n", "const LIMIT: u64 = 4;\npub fn parse(y: u64) -> bool {\n    y % LIMIT == 0\n}\n", None);
    let after = move_files("mod calendar;\nmod date;\n", "use crate::calendar::LIMIT;\npub fn parse(y: u64) -> bool {\n    y % LIMIT == 0\n}\n", Some("pub(crate) const LIMIT: u64 = 4;\n"));
    let bodies = [("demo::date::parse", equal("n1")), ("demo::calendar::LIMIT", equal("n0"))];
    let report = run_derived_files(&before, &after, &bodies);
    assert!(report.accepted() && descriptions(&report).iter().any(|text| text.starts_with("moved `demo::date::LIMIT` → `demo::calendar::LIMIT`")), "{report:#?}");
    let claims = vec![claim("c1", "move-item", Some("demo::date::LIMIT"), serde_json::json!({"to": "demo::calendar::LIMIT"}))];
    let report = run_claim_files(&before, &after, claims, &bodies);
    assert!(report.accepted() && status_of(&report, "c1").is_verified(), "{report:#?}");
    // A different value is not a move.
    let changed = move_files("mod calendar;\nmod date;\n", "use crate::calendar::LIMIT;\npub fn parse(y: u64) -> bool {\n    y % LIMIT == 0\n}\n", Some("pub(crate) const LIMIT: u64 = 5;\n"));
    let report = run_derived_files(&before, &changed, &[("demo::date::parse", different(false)), ("demo::calendar::LIMIT", different(false))]);
    // Design 36.1: the move is one fact, and the value is a second fact with the changed bodies.
    let texts = descriptions(&report);
    assert_eq!(texts, ["moved `demo::date::LIMIT` → `demo::calendar::LIMIT`, with 1 `use` line(s)", "visibility of `demo::calendar::LIMIT`: `private` → `pub(crate)`", "value of `demo::calendar::LIMIT` changed: 4 → 5"]);
    assert!(report.facts[0].compare == Compare::CodeEqual && report.facts[2].category == Category::Semantics && report.facts[2].affected == ["demo::calendar::LIMIT", "demo::date::parse"], "{report:#?}");

    // A struct with an `impl` block.
    const YEAR: &str = "pub struct Year(pub u64);\nimpl Year {\n    pub fn leap(&self) -> bool {\n        self.0 % 4 == 0\n    }\n}\n";
    const PARSE: &str = "pub fn parse(y: u64) -> bool {\n    Year(y).leap()\n}\n";
    let date = format!("{YEAR}{PARSE}");
    let date_after = format!("use crate::calendar::Year;\n{PARSE}");
    let before = move_files("mod date;\n", &date, None);
    let after = move_files("mod calendar;\nmod date;\n", &date_after, Some(YEAR));
    let bodies = [("demo::date::parse", equal("n1")), ("demo::calendar::Year::leap", equal("n1"))];
    let report = run_derived_files(&before, &after, &bodies);
    assert!(report.accepted() && descriptions(&report).iter().any(|text| text.starts_with("moved `demo::date::Year` → `demo::calendar::Year`")), "{report:#?}");
    assert!(!descriptions(&report).iter().any(|text| text.contains("added") || text.contains("removed")), "{report:#?}");
    let claims = vec![claim("c1", "move-item", Some("demo::date::Year"), serde_json::json!({"to": "demo::calendar::Year"}))];
    let report = run_claim_files(&before, &after, claims, &bodies);
    assert!(report.accepted() && status_of(&report, "c1").is_verified(), "{report:#?}");
    // An edit in a method of the moved type (design 36.1): the edit is a fact at the new path.
    let edited = YEAR.replace("% 4", "% 400");
    let after = move_files("mod calendar;\nmod date;\n", &date_after, Some(&edited));
    let report = run_derived_files(&before, &after, &[("demo::date::parse", equal("n1")), ("demo::calendar::Year::leap", different(false))]);
    assert_eq!(descriptions(&report), ["moved `demo::date::Year` → `demo::calendar::Year`, with 1 impl and 1 `use` line(s)", "changed literal `4` → `400` at `demo::calendar::Year::leap/tail/lhs/rhs`"]);
    assert!(report.facts[0].compare == Compare::CodeEqual && report.facts[1].compare == Compare::CodeDifferent && report.facts[1].affected == ["demo::calendar::Year::leap"], "{report:#?}");
}

#[test]
fn a_reason_with_the_path_of_an_item_covers_each_spotlight_in_the_item() {
    // Batch 5x, part D (trial 7 on humantime): only a rejection gave the exact key of a reason,
    // so each step with a spotlight was two commands. The agent knows the path of the item.
    let src = "pub mod m {\n    pub fn f(a: u32) -> u32 {\n        a\n    }\n    pub fn g(a: u32) -> u32 {\n        a\n    }\n}\n";
    let after = src.replace("pub fn f", "pub unsafe fn f").replace("pub fn g", "pub unsafe fn g");
    let check = |reasons: &[(&str, &str)]| {
        let (_d1, old) = load(src);
        let (_d2, new) = load(&after);
        let mut step = step(vec![]);
        for (at, text) in reasons {
            step.reasons.insert(at.to_string(), text.to_string());
        }
        verify_step(&StepInput { step: &step, before: &old, after: &new }, &NoOracle, &NoOracle).report
    };
    let reason = |report: &StepReport, item: &str| {
        let fact = report.facts.iter().find(|fact| fact.at.as_ref().is_some_and(|at| at.item == item)).unwrap();
        assert!(fact.spotlight, "{fact:#?}");
        (fact.reason.clone().unwrap_or_default(), fact.reason_key.clone().unwrap_or_default())
    };
    let report = check(&[]);
    assert_eq!(facts_of(&report), ["qualifiers demo::m::f/sig/qualifiers", "qualifiers demo::m::g/sig/qualifiers"]);
    assert!(!report.accepted());
    // The path of the function covers the spotlight in it, and no other spotlight.
    let report = check(&[("demo::m::f", "the caller checks")]);
    assert_eq!(reason(&report, "demo::m::f"), ("the caller checks".to_string(), "demo::m::f".to_string()));
    assert_eq!(reason(&report, "demo::m::g"), (String::new(), String::new()));
    assert!(!report.accepted());
    // Each output shows the reason with the key that the agent gave.
    assert!(views::text_report(std::slice::from_ref(&report)).contains("; reason (key `demo::m::f`): \"the caller checks\""), "{}", views::text_report(std::slice::from_ref(&report)));
    // The exact key continues to work, and it goes first.
    let report = check(&[("demo::m::f", "the caller checks"), ("demo::m::g/sig/qualifiers", "exact"), ("demo::m::f/sig/qualifiers", "exact f")]);
    assert_eq!(reason(&report, "demo::m::f"), ("exact f".to_string(), String::new()));
    assert_eq!(reason(&report, "demo::m::g"), ("exact".to_string(), String::new()));
    assert!(report.accepted());
    assert!(views::text_report(std::slice::from_ref(&report)).contains("; reason: \"exact f\""));
    // A module path and a crate name are not a key of this form. A name that only starts with
    // the path of an item is not in the item.
    for key in ["demo::m", "demo", "demo::m::f/sig", "demo::m::", "demo::m::f/"] {
        let report = check(&[(key, "wide")]);
        assert_eq!((reason(&report, "demo::m::f"), reason(&report, "demo::m::g")), ((String::new(), String::new()), (String::new(), String::new())), "{key}");
    }
    let (_d1, old) = load(src);
    let (_d2, new) = load(&format!("{after}pub struct S;\npub const C: u32 = 1;\npub trait T {{}}\nimpl S {{\n    pub fn new() -> S {{\n        S\n    }}\n}}\n"));
    for key in ["demo::m::f", "demo::S", "demo::C", "demo::T", "demo::S::new"] {
        assert!(is_item_reason_key(key, &old, &new), "{key}");
    }
    for key in ["demo::m", "demo", "demo::impl S", "demo::none"] {
        assert!(!is_item_reason_key(key, &old, &new), "{key}");
    }
}

#[test]
fn a_nested_function_is_its_own_body() {
    // Batch 5x, part E (trial 7): an edit in a nested function showed the outer function as the
    // affected body. The outer function is affected only if its MIR changes.
    let before = "pub fn outer(n: u32) -> u32 {\n    fn inner() -> u32 {\n        7\n    }\n    n + inner()\n}\n";
    let after = before.replace("7", "8");
    let outer = |status| body("demo::outer", "src/lib.rs", 1, status);
    let inner = |name: &str| body(name, "src/lib.rs", 2, different(false));
    let affected = |bodies: Vec<BodyComparison>| -> Vec<String> {
        let report = run_derived_with(before, &after, &SpanBodies(bodies));
        assert!(report.accepted() && report.facts.len() == 1 && report.facts[0].compare == Compare::CodeDifferent, "{report:#?}");
        report.facts[0].affected.clone()
    };
    assert_eq!(affected(vec![outer(equal("n0")), inner("demo::outer::inner")]), ["demo::outer::inner"]);
    // The outer function shows when its MIR changes, also through a closure.
    assert_eq!(affected(vec![outer(different(false)), inner("demo::outer::inner")]), ["demo::outer", "demo::outer::inner"]);
    assert_eq!(affected(vec![outer(equal("n0")), inner("demo::outer::{closure#0}"), inner("demo::outer::inner::{closure#0}")]), ["demo::outer", "demo::outer::inner"]);
    // With no nested function, the list has the item.
    assert_eq!(affected(vec![outer(different(false))]), ["demo::outer"]);
}

/// Batch 5x, part E: a reason with the exact key of a fact with no spotlight shows as a note.
/// The class of the fact stays.
#[test]
fn a_stored_reason_of_a_fact_with_no_spotlight_is_a_note() {
    let before = "pub fn f(x: u32) -> u32 {\n    x + 1\n}\n";
    let after = "pub fn f(x: u32) -> u32 {\n    x + 2\n}\n";
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let oracle = FakeBodies([("demo::f".to_string(), different(false))].into_iter().collect());
    let run = |step: &Step| crate::verify_step_with(&StepInput { step, before: &old, after: &new }, &oracle, &NoOracle, true).report;
    let mut step = step(Vec::new());
    let plain = run(&step);
    assert_eq!(plain.facts.len(), 1, "{:?}", plain.facts);
    assert!(!plain.facts[0].spotlight && plain.facts[0].reason.is_none(), "{:?}", plain.facts);
    assert!(!views::text_report(std::slice::from_ref(&plain)).contains("gave a reason"));
    step.reasons.insert(plain.facts[0].reason_place(), "the limit is new".into());
    let report = run(&step);
    assert_eq!(report.facts[0].reason.as_deref(), Some("the limit is new"));
    assert_eq!((report.facts[0].compare, report.facts[0].spotlight), (plain.facts[0].compare, false));
    let text = views::text_report(std::slice::from_ref(&report));
    assert!(text.contains("      note: no spotlight; the agent gave a reason: \"the limit is new\""), "{text}");
}

/// A step with claims, and a derived fact for each edit with no claim (as the command `step`).
fn run_claim_facts(before: &[(&str, &str)], after: &[(&str, &str)], claims: Vec<Claim>, bodies: &[(&str, BodyStatus)]) -> StepReport {
    let (_d1, old) = load_files(before);
    let (_d2, new) = load_files(after);
    let step = step(claims);
    let oracle = FakeBodies(bodies.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
    verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle, true).report
}

/// Batch 5x, round 2, part 1: a plain doc comment in the file of the new module, or on the
/// moved item, does not stop the proof of a move. Each doc change stays a fact to read. Each
/// other attribute stops the proof as before.
#[test]
fn a_plain_doc_comment_does_not_stop_the_proof_of_a_move() {
    const DATE: &str = "pub fn parse(y: u64) -> bool {\n    is_leap(y)\n}\nfn is_leap(y: u64) -> bool {\n    y % 4 == 0\n}\n";
    const DATE_AFTER: &str = "use crate::calendar::is_leap;\n\npub fn parse(y: u64) -> bool {\n    is_leap(y)\n}\n";
    const CALENDAR: &str = "pub(crate) fn is_leap(y: u64) -> bool {\n    y % 4 == 0\n}\n";
    const LIB: &str = "mod calendar;\nmod date;\n";
    const HEAD: &str = "//! Calendar computations.\n\n";
    let before = move_files("mod date;\n", DATE, None);
    let bodies = [("demo::date::parse", equal("n1")), ("demo::calendar::is_leap", equal("n0"))];
    let claims = || vec![claim("c1", "move-item", Some("demo::date::is_leap"), serde_json::json!({"to": "demo::calendar::is_leap"}))];
    let proved = |report: &StepReport| report.accepted() && descriptions(report).iter().any(|text| text.starts_with("moved `demo::date::is_leap` → `demo::calendar::is_leap`"));
    let doc_facts = |report: &StepReport| report.facts.iter().filter(|fact| ["changed docs", "added docs", "removed docs"].iter().any(|start| fact.description.starts_with(start)) && fact.compare == Compare::NoCode).count();

    // 1. Each form of a plain doc comment: in the file, on the item, and the two together.
    let forms = [
        (HEAD, ""),
        ("/*! Calendar computations. */\n", ""),
        ("#![doc = \"Calendar computations.\"]\n", ""),
        ("", "/// True for a leap year.\n"),
        ("", "/** True for a leap year. */\n"),
        ("", "#[doc = \"True for a leap year.\"]\n"),
        (HEAD, "/// True for a leap year.\n"),
        // A long doc comment on a short item: the doc text is not a part of the pair test.
        ("", "/// True for a leap year.\n///\n/// A year is a leap year if 4 divides it. This function does not know the rule of\n/// the centuries, and it does not know the calendar before the year 1582.\n"),
    ];
    for (head, doc) in forms {
        let calendar = format!("{head}{doc}{CALENDAR}");
        let after = move_files(LIB, DATE_AFTER, Some(&calendar));
        let count = [head, doc].iter().filter(|text| !text.is_empty()).count();
        let report = run_derived_files(&before, &after, &bodies);
        assert!(proved(&report), "{calendar}: {report:#?}");
        assert!(!descriptions(&report).iter().any(|text| !text.starts_with("added docs") && !text.starts_with("removed docs") && (text.contains("added") || text.contains("removed"))), "{calendar}: {report:#?}");
        // Each doc change is its own fact to read, with the class `other`.
        assert_eq!(doc_facts(&report), count, "{calendar}: {report:#?}");
        let report = run_claim_facts(&before, &after, claims(), &bodies);
        assert!(report.accepted() && status_of(&report, "c1").is_verified() && report.uncovered.is_empty(), "{calendar}: {report:#?}");
        assert_eq!(doc_facts(&report), count, "{calendar}: {report:#?}");
    }

    // 2. A second change, also with a doc comment in the new file. The explicit claim gets no
    // proof. The derived move (design 36.1) stays, and the second change has its own fact: the
    // last part of each case is the start of that fact. `None`: Semilla derives no move.
    let changed = [("demo::date::parse", equal("n1")), ("demo::calendar::is_leap", different(false))];
    let cases: Vec<(&str, String, String, &[(&str, BodyStatus)], Option<&str>)> = vec![
        ("a body edit", LIB.into(), format!("{HEAD}{}", CALENDAR.replace("== 0", "== 0 && y != 0")), &changed, Some("changed expression `y % 4 == 0` → `y % 4 == 0 && y != 0` at `demo::calendar::is_leap/tail`")),
        // A bare name of the body is a different item in the new module: the MIR differs.
        ("a different callee", LIB.into(), format!("{HEAD}{CALENDAR}"), &changed, Some("moved ")),
        ("a cfg on the module", "#[cfg(not(windows))]\nmod calendar;\nmod date;\n".into(), format!("{HEAD}{CALENDAR}"), &bodies, None),
        ("an allow attribute", LIB.into(), format!("{HEAD}#![allow(dead_code)]\n{CALENDAR}"), &bodies, Some("attributes of `demo::calendar`: added `#![allow(dead_code)]`")),
        ("an allow attribute only", LIB.into(), format!("#![allow(dead_code)]\n{CALENDAR}"), &bodies, Some("attributes of `demo::calendar`: added `#![allow(dead_code)]`")),
        ("a doc in a cfg_attr", LIB.into(), format!("#![cfg_attr(windows, doc = \"Calendar.\")]\n{CALENDAR}"), &bodies, None),
        ("a computed doc in the file", LIB.into(), format!("#![doc = include_str!(\"../README.md\")]\n{CALENDAR}"), &bodies, Some("doc test of `demo::calendar` added")),
        ("a doc test in the file", LIB.into(), format!("//! Calendar.\n//!\n//! ```\n//! assert!(false);\n//! ```\n{CALENDAR}"), &bodies, Some("doc test of `demo::calendar` added")),
        ("a hidden item", LIB.into(), format!("{HEAD}#[doc(hidden)]\n{CALENDAR}"), &bodies, Some("attributes of `demo::calendar::is_leap`: added `#[doc(hidden)]`")),
        ("a computed doc on the item", LIB.into(), format!("{HEAD}#[doc = include_str!(\"../README.md\")]\n{CALENDAR}"), &bodies, Some("doc test of `demo::calendar::is_leap` added")),
        ("a cfg on the item", LIB.into(), format!("{HEAD}#[cfg(not(windows))]\n{CALENDAR}"), &bodies, Some("attributes of `demo::calendar::is_leap`: added `#[cfg(not(windows))]`")),
        ("a doc test on the item", LIB.into(), format!("{HEAD}/// ```\n/// assert!(false);\n/// ```\n{CALENDAR}"), &bodies, Some("doc test of `demo::calendar::is_leap` added")),
        // A doc comment on the `mod` line has no fact of its own, so it is not a part of a move.
        ("a doc on the mod line", "/// Calendar computations.\nmod calendar;\nmod date;\n".into(), CALENDAR.into(), &bodies, Some("added `demo::calendar`")),
        ("a doc test on the mod line", "/// ```\n/// assert!(false);\n/// ```\nmod calendar;\nmod date;\n".into(), CALENDAR.into(), &bodies, Some("added `demo::calendar`")),
    ];
    for (name, lib, calendar, bodies, fact) in &cases {
        let after = move_files(lib, DATE_AFTER, Some(calendar));
        let report = run_derived_files(&before, &after, bodies);
        let moved = report.facts.iter().find(|fact| fact.description.starts_with("moved `demo::date::is_leap` → `demo::calendar::is_leap`"));
        match fact {
            None => assert!(moved.is_none() && descriptions(&report).contains(&"removed `demo::date::is_leap`".to_string()), "{name}: {report:#?}"),
            // A changed body with no edit: the move fact has the mismatch mark.
            Some("moved ") => assert!(moved.is_some_and(|fact| fact.compare == Compare::CodeDifferent && fact.mismatch() && fact.affected == ["demo::calendar::is_leap"]), "{name}: {report:#?}"),
            Some(start) => assert!(moved.is_some_and(|fact| fact.compare == Compare::CodeEqual) && descriptions(&report).iter().any(|text| text.starts_with(start)), "{name}: {report:#?}"),
        }
        let report = run_claim_facts(&before, &after, claims(), bodies);
        assert!(!report.accepted() && !status_of(&report, "c1").is_verified(), "{name}: {report:#?}");
    }
    // A moved constant with a different value, and a doc comment in the new file.
    let before = move_files("mod date;\n", "const LIMIT: u64 = 4;\npub fn parse(y: u64) -> bool {\n    y % LIMIT == 0\n}\n", None);
    let date = "use crate::calendar::LIMIT;\npub fn parse(y: u64) -> bool {\n    y % LIMIT == 0\n}\n";
    let same = move_files(LIB, date, Some("//! Calendar computations.\n\n/// The cycle.\npub(crate) const LIMIT: u64 = 4;\n"));
    let report = run_derived_files(&before, &same, &[("demo::date::parse", equal("n1")), ("demo::calendar::LIMIT", equal("n0"))]);
    assert!(report.accepted() && descriptions(&report).iter().any(|text| text.starts_with("moved `demo::date::LIMIT` → `demo::calendar::LIMIT`")), "{report:#?}");
    assert_eq!(doc_facts(&report), 2, "{report:#?}");
    let other = move_files(LIB, date, Some("//! Calendar computations.\n\npub(crate) const LIMIT: u64 = 5;\n"));
    let report = run_derived_files(&before, &other, &[("demo::date::parse", different(false)), ("demo::calendar::LIMIT", different(false))]);
    // Design 36.1: the move is one fact, and the value is a second fact.
    let texts = descriptions(&report);
    assert!(texts[0].starts_with("moved `demo::date::LIMIT` → `demo::calendar::LIMIT`") && report.facts[0].compare == Compare::CodeEqual && texts.contains(&"value of `demo::calendar::LIMIT` changed: 4 → 5".to_string()), "{report:#?}");
}

/// Batch 5x, round 2, part 1, rule 7: a rename step with a doc comment change keeps its proof.
/// A doc test and an attribute of the `doc` family with no text stop the proof of an explicit
/// claim. With a derived rename (design 36.1), each of them is a second fact.
#[test]
fn a_plain_doc_comment_does_not_stop_the_proof_of_a_rename() {
    const LIB: &str = "pub fn show(n: u32) -> u32 {\n    item(n)\n}\n/// The item.\nfn item(n: u32) -> u32 {\n    n + 1\n}\n";
    let bodies = [("demo::show", equal("n1")), ("demo::piece", equal("n1"))];
    let renamed = |report: &StepReport| report.accepted() && descriptions(report).iter().any(|text| text.starts_with("renamed `demo::item` → `demo::piece`"));
    let claims = || vec![claim("c1", "rename", Some("demo::item"), serde_json::json!({"to": "piece"}))];
    let after = LIB.replace("item(", "piece(");
    for (doc, proof, second) in [
        ("/// The piece.\n", true, ""),
        ("/// The piece.\n/// It adds one.\n", true, ""),
        ("", true, ""),
        ("/** The piece. */\n", true, ""),
        ("#[doc = \"The piece.\"]\n", true, ""),
        ("/// The piece.\n///\n/// ```\n/// assert!(false);\n/// ```\n", false, "doc test of `demo::piece` added"),
        ("/// The item.\n#[doc(hidden)]\n", false, "attributes of `demo::piece`: added `#[doc(hidden)]`"),
        ("#[doc = include_str!(\"../README.md\")]\n", false, "doc test of `demo::piece` added"),
    ] {
        let after = after.replace("/// The item.\n", doc);
        let report = run_derived_files(&[("Cargo.toml", MANIFEST), ("src/lib.rs", LIB)], &[("Cargo.toml", MANIFEST), ("src/lib.rs", &after)], &bodies);
        assert!(renamed(&report) && (proof || descriptions(&report).iter().any(|text| text.starts_with(second))), "{doc}: {report:#?}");
        let report = run_claim_facts(&[("Cargo.toml", MANIFEST), ("src/lib.rs", LIB)], &[("Cargo.toml", MANIFEST), ("src/lib.rs", &after)], claims(), &bodies);
        assert_eq!(report.accepted() && status_of(&report, "c1").is_verified(), proof, "{doc}: {report:#?}");
        if proof {
            // The doc change is a fact to read with the class `other`.
            let fact = report.facts.iter().find(|fact| ["changed docs", "added docs", "removed docs"].iter().any(|start| fact.description.starts_with(start))).unwrap_or_else(|| panic!("{doc}: {report:#?}"));
            assert_eq!(fact.compare, Compare::NoCode, "{doc}: {report:#?}");
        } else {
            // The message does not name a plain doc change: the cause is the other change.
            assert!(status_of(&report, "c1").text().contains("the step has a second change: the step also has the change at `demo::piece`"), "{doc}: {report:#?}");
        }
    }
}

/// Batch 5x, round 2, part 2: a visibility change that changes the set of the items that a
/// different crate can name is a fact to read (the class `other`), and its text says so. A
/// change with no such effect stays proved neutral.
#[test]
fn a_visibility_change_of_the_public_api_is_a_fact_to_read() {
    const DATE: &str = "pub fn parse(y: u64) -> bool {\n    is_leap(y)\n}\nfn is_leap(y: u64) -> bool {\n    y % 4 == 0\n}\n";
    const CALENDAR: &str = "fn leap(y: u64) -> bool {\n    y % 4 == 0\n}\npub fn days(y: u64) -> u64 {\n    if leap(y) { 366 } else { 365 }\n}\n";
    const LIB: &str = "pub mod calendar;\nmod date;\n";
    const NOW: &str = " (the public API changes: a different crate can now use the item)";
    const GONE: &str = " (the public API changes: a different crate can no longer use the item)";
    let bodies = [("demo::date::parse", equal("n0")), ("demo::date::is_leap", equal("n0")), ("demo::calendar::leap", equal("n0")), ("demo::calendar::days", equal("n0")), ("demo::calendar::is_leap", equal("n0")), ("demo::time::parse", equal("n0")), ("demo::time::is_leap", equal("n0")), ("demo::date::Year::leap", equal("n0"))];
    // The visibility fact of a step: its text and its class.
    let visibility = |before: &[(&str, &str)], after: &[(&str, &str)]| -> (String, Compare) {
        let report = run_derived_files(before, after, &bodies);
        assert!(report.accepted(), "{report:#?}");
        let fact = report.facts.iter().find(|fact| fact.description.starts_with("visibility of ")).unwrap_or_else(|| panic!("{report:#?}"));
        assert!(!fact.spotlight, "{report:#?}");
        (fact.description.clone(), fact.compare)
    };
    let files = |lib: &'static str, date: &'static str, calendar: &'static str| move_files(lib, date, Some(calendar));
    let base = files(LIB, DATE, CALENDAR);
    let leaked: &str = Box::leak(CALENDAR.replace("fn leap", "pub fn leap").into_boxed_str());
    let in_crate: &str = Box::leak(CALENDAR.replace("fn leap", "pub(crate) fn leap").into_boxed_str());
    let public_date: &str = Box::leak(DATE.replace("fn is_leap", "pub fn is_leap").into_boxed_str());

    // V1: `fn` → `pub fn` in a public module.
    let (text, class) = visibility(&base, &files(LIB, DATE, leaked));
    assert_eq!((text, class), (format!("visibility of `demo::calendar::leap`: `private` → `pub`{NOW}"), Compare::CodeEqual));
    // The reverse of V1.
    let (text, class) = visibility(&files(LIB, DATE, leaked), &base);
    assert_eq!((text, class), (format!("visibility of `demo::calendar::leap`: `pub` → `private`{GONE}"), Compare::CodeEqual));
    // `private` → `pub(crate)`: no crate other than this one sees a difference.
    let (text, class) = visibility(&base, &files(LIB, DATE, in_crate));
    assert_eq!((text, class), ("visibility of `demo::calendar::leap`: `private` → `pub(crate)`".to_string(), Compare::CodeEqual));
    // `pub(crate)` → `pub` in a public module.
    let (text, class) = visibility(&files(LIB, DATE, in_crate), &files(LIB, DATE, leaked));
    assert_eq!((text, class), (format!("visibility of `demo::calendar::leap`: `pub(crate)` → `pub`{NOW}"), Compare::CodeEqual));
    // `pub` in a private module with no re-export. A `pub use` line of a different item of
    // the module does not bring the item.
    for lib in [LIB, "pub mod calendar;\nmod date;\npub use self::date::parse;\n", "pub mod calendar;\nmod date;\npub use self::date::{parse as read};\n"] {
        let (text, class) = visibility(&files(lib, DATE, CALENDAR), &files(lib, public_date, CALENDAR));
        assert_eq!((text, class), ("visibility of `demo::date::is_leap`: `private` → `pub`".to_string(), Compare::CodeEqual), "{lib}");
    }
    // `pub` in a private module, and a `pub use` line that can bring the item: by its name, by
    // the name of the module, with a glob, and through an `as` name of a private `use` line.
    for (lib, cause) in [
        ("pub mod calendar;\nmod date;\npub use date::is_leap;\n", "a `pub use` line names `is_leap`"),
        ("pub mod calendar;\nmod date;\npub use self::date::{parse, is_leap as leap};\n", "a `pub use` line names `is_leap`"),
        ("pub mod calendar;\nmod date;\npub use crate::date as time;\n", "a `pub use` line names `date`"),
        ("pub mod calendar;\nmod date;\npub use date::*;\n", "a glob `pub use` line names `date`"),
        ("pub mod calendar;\nmod date;\nuse date as time;\npub use time::*;\n", "a glob `pub use` line names `time`"),
        ("pub mod calendar;\nmod date;\npub mod all {\n    pub use super::*;\n}\n", "a glob `pub use` line with a relative path"),
        ("pub mod calendar;\nmod date;\nmacro_rules! export {\n    ($name:ident) => {\n        pub use $name::*;\n    };\n}\nexport!(date);\n", "the macro `export!` has a `pub use` line"),
        ("pub mod calendar;\nmod date;\nother::export!(date);\n", "a macro call names `date`"),
        ("pub mod calendar;\nmod date;\n#[path = \"date.rs\"]\npub mod time;\n", "the module `demo::time` has a `#[path]` attribute"),
    ] {
        let (text, class) = visibility(&files(lib, DATE, CALENDAR), &files(lib, public_date, CALENDAR));
        assert!(text.starts_with("visibility of `demo::date::is_leap`: `private` → `pub` (the public API can change: ") && text.contains(cause), "{lib}: {text}");
        assert_eq!(class, Compare::CodeEqual, "{lib}");
    }
    // A method of a `pub` type in a private module: a public signature can give the type to a
    // different crate. A method of a type that is not `pub` stays neutral.
    const YEAR: &str = "pub struct Year(pub u64);\nimpl Year {\n    fn leap(&self) -> bool {\n        self.0 % 4 == 0\n    }\n}\npub fn parse(y: u64) -> bool {\n    Year(y).leap()\n}\n";
    let year_public: &str = Box::leak(YEAR.replace("    fn leap", "    pub fn leap").into_boxed_str());
    let (text, class) = visibility(&files(LIB, YEAR, CALENDAR), &files(LIB, year_public, CALENDAR));
    assert_eq!(class, Compare::CodeEqual, "{text}");
    assert!(text.contains("(the public API can change: the type `Year` is `pub`, and a public signature can give it to a different crate)"), "{text}");
    let hidden: &str = Box::leak(YEAR.replace("pub struct Year", "pub(crate) struct Year").into_boxed_str());
    let hidden_public: &str = Box::leak(hidden.replace("    fn leap", "    pub fn leap").into_boxed_str());
    let (text, class) = visibility(&files(LIB, hidden, CALENDAR), &files(LIB, hidden_public, CALENDAR));
    assert_eq!((text, class), ("visibility of `demo::date::Year::leap`: `private` → `pub`".to_string(), Compare::CodeEqual));
    // A module that becomes public.
    let (text, class) = visibility(&files(LIB, DATE, CALENDAR), &files("pub mod calendar;\npub mod date;\n", DATE, CALENDAR));
    assert_eq!((text, class), (format!("visibility of `demo::date`: `private` → `pub`{NOW}"), Compare::CodeEqual));

    // V2: a move into the public module, with `pub`. The move stays proved neutral, and the
    // visibility is the fact to read.
    let moved_date = "use crate::calendar::is_leap;\n\npub fn parse(y: u64) -> bool {\n    is_leap(y)\n}\n";
    let moved_calendar: &str = Box::leak(format!("{CALENDAR}pub fn is_leap(y: u64) -> bool {{\n    y % 4 == 0\n}}\n").into_boxed_str());
    let v2_bodies = [("demo::date::parse", equal("n1")), ("demo::calendar::is_leap", equal("n0")), ("demo::calendar::leap", equal("n0")), ("demo::calendar::days", equal("n0"))];
    let report = run_derived_files(&base, &files(LIB, moved_date, moved_calendar), &v2_bodies);
    assert!(report.accepted(), "{report:#?}");
    let moved = report.facts.iter().find(|fact| fact.description.starts_with("moved `demo::date::is_leap` → `demo::calendar::is_leap`")).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!(moved.compare, Compare::CodeEqual);
    let fact = report.facts.iter().find(|fact| fact.description.starts_with("visibility of ")).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!((fact.description.clone(), fact.compare), (format!("visibility of `demo::calendar::is_leap`: `private` → `pub`{NOW}"), Compare::CodeEqual));
    let claims = vec![claim("c1", "move-item", Some("demo::date::is_leap"), serde_json::json!({"to": "demo::calendar::is_leap"}))];
    let report = run_claim_facts(&base, &files(LIB, moved_date, moved_calendar), claims, &v2_bodies);
    assert!(report.accepted() && status_of(&report, "c1").is_verified(), "{report:#?}");
    let fact = report.facts.iter().find(|fact| fact.description.starts_with("visibility of ")).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!(fact.compare, Compare::CodeEqual, "{report:#?}");

    // A binary crate has no API for a different crate: the change stays proved neutral.
    let main = "pub mod calendar;\nfn main() {\n    let _ = calendar::days(4);\n}\n";
    let binary = |calendar: &'static str| vec![("Cargo.toml", MANIFEST), ("src/main.rs", main), ("src/calendar.rs", calendar)];
    let (text, class) = visibility(&binary(CALENDAR), &binary(leaked));
    assert_eq!((text, class), ("visibility of `demo::calendar::leap`: `private` → `pub`".to_string(), Compare::CodeEqual));
}

/// Batch 5x, round 3: a proved rename or move that changes the set of the paths that a
/// different crate can use gets one fact to read (the class `other`). The identity fact stays
/// proved neutral, and an identity claim stays verified. A private item gets no such fact.
#[test]
fn a_rename_or_a_move_that_changes_the_public_api_gets_a_fact_to_read() {
    const DATE: &str = "pub fn parse(y: u64) -> bool {\n    is_leap(y)\n}\npub fn is_leap(y: u64) -> bool {\n    y % 4 == 0\n}\n";
    const DATE_MOVED: &str = "use crate::calendar::is_leap;\n\npub fn parse(y: u64) -> bool {\n    is_leap(y)\n}\n";
    const CALENDAR: &str = "fn leap(y: u64) -> bool {\n    y % 4 == 0\n}\npub fn days(y: u64) -> u64 {\n    if leap(y) { 366 } else { 365 }\n}\n";
    const LIB: &str = "pub mod calendar;\nmod date;\n";
    const PRIVATE: &str = "mod calendar;\nmod date;\n";
    let moved_calendar = format!("{CALENDAR}pub fn is_leap(y: u64) -> bool {{\n    y % 4 == 0\n}}\n");
    let bodies = [
        ("demo::date::parse", equal("n1")),
        ("demo::date::is_leap", equal("n0")),
        ("demo::calendar::is_leap", equal("n0")),
        ("demo::calendar::leap", equal("n0")),
        ("demo::calendar::leap_year", equal("n0")),
        ("demo::calendar::days", equal("n1")),
        ("demo::calendar::day_count", equal("n1")),
    ];
    // The API facts of a report. Each one has the class `other`, no spotlight, and no reason.
    let api = |report: &StepReport| -> Vec<String> {
        let facts: Vec<&semilla_core::Fact> = report.facts.iter().filter(|fact| fact.description.contains("public API")).collect();
        assert!(facts.iter().all(|fact| matches!(fact.compare, Compare::NoCode | Compare::CodeEqual) && !fact.spotlight && fact.reason.is_none()), "{report:#?}");
        facts.iter().map(|fact| fact.description.clone()).collect()
    };
    // The identity fact of a derived rename or move: it stays proved neutral.
    let proved = |report: &StepReport, start: &str| {
        assert!(report.accepted(), "{report:#?}");
        let fact = report.facts.iter().find(|fact| fact.description.starts_with(start)).unwrap_or_else(|| panic!("{start}: {report:#?}"));
        assert_eq!(fact.compare, Compare::CodeEqual, "{report:#?}");
    };

    // V3: a `pub fn` moves from a private module into a public module, with no visibility edit.
    let before = move_files(LIB, DATE, Some(CALENDAR));
    let after = move_files(LIB, DATE_MOVED, Some(&moved_calendar));
    let report = run_derived_files(&before, &after, &bodies);
    proved(&report, "moved `demo::date::is_leap` → `demo::calendar::is_leap`");
    assert_eq!(api(&report), ["public API: a different crate can now use `demo::calendar::is_leap`"]);
    assert_eq!(report.facts.len(), 2, "{report:#?}");
    // The text report shows the new fact with its class, and no line asks for a reason.
    let text = views::text_report(std::slice::from_ref(&report));
    assert!(text.contains("public API: a different crate can now use `demo::calendar::is_leap` [move · no code]") && !text.contains("needs a reason"), "{text}");
    // The kind of the group stays `move`: the new fact is no second change.
    assert_eq!(semilla_core::report::derived_kind(&[&report]), Some(semilla_core::GroupKind::Move));
    // The reverse of V3: the item goes out of the public API.
    let report = run_derived_files(&after, &before, &bodies);
    proved(&report, "moved `demo::calendar::is_leap` → `demo::date::is_leap`");
    assert_eq!(api(&report), ["public API: a different crate can no longer use `demo::calendar::is_leap`"]);
    // V3 with the claim `move-item`: the claim stays verified.
    let claims = vec![claim("c1", "move-item", Some("demo::date::is_leap"), serde_json::json!({"to": "demo::calendar::is_leap"}))];
    let report = run_claim_facts(&before, &after, claims, &bodies);
    assert!(report.accepted() && status_of(&report, "c1").is_verified(), "{report:#?}");
    assert_eq!(api(&report), ["public API: a different crate can now use `demo::calendar::is_leap`"]);

    // A rename of a `pub fn` in a public module: the old public name stops to exist.
    let renamed = CALENDAR.replace("days", "day_count");
    let after = move_files(LIB, DATE, Some(&renamed));
    const RENAME: &str = "public API: `demo::calendar::days` is now `demo::calendar::day_count` (a different crate can no longer use the old path)";
    let report = run_derived_files(&before, &after, &bodies);
    proved(&report, "renamed `demo::calendar::days` → `demo::calendar::day_count`");
    assert_eq!(api(&report), [RENAME]);
    assert_eq!(semilla_core::report::derived_kind(&[&report]), Some(semilla_core::GroupKind::Rename));
    // The same rename with the claim `rename`: accepted, the claim is verified.
    let claims = || vec![claim("c1", "rename", Some("demo::calendar::days"), serde_json::json!({"to": "day_count"}))];
    let report = run_claim_facts(&before, &after, claims(), &bodies);
    assert!(report.accepted() && status_of(&report, "c1").is_verified(), "{report:#?}");
    assert_eq!(api(&report), [RENAME]);
    // The public module is a private module: no different crate can use the name.
    let report = run_derived_files(&move_files(PRIVATE, DATE, Some(CALENDAR)), &move_files(PRIVATE, DATE, Some(&renamed)), &bodies);
    proved(&report, "renamed `demo::calendar::days` → `demo::calendar::day_count`");
    assert_eq!((api(&report).len(), report.facts.len()), (0, 1), "{report:#?}");

    // A rename of a private function: no new fact.
    let private = CALENDAR.replace("leap", "leap_year");
    let report = run_derived_files(&before, &move_files(LIB, DATE, Some(&private)), &bodies);
    proved(&report, "renamed `demo::calendar::leap` → `demo::calendar::leap_year`");
    assert_eq!((api(&report).len(), report.facts.len()), (0, 1), "{report:#?}");
    let claims = vec![claim("c1", "rename", Some("demo::calendar::leap"), serde_json::json!({"to": "leap_year"}))];
    let report = run_claim_facts(&before, &move_files(LIB, DATE, Some(&private)), claims, &bodies);
    assert!(report.accepted() && status_of(&report, "c1").is_verified(), "{report:#?}");
    assert_eq!((api(&report).len(), report.facts.len()), (0, 0), "{report:#?}");

    // A move of a private function between two private modules: no new fact.
    let date = DATE.replace("pub fn is_leap", "fn is_leap");
    let calendar = moved_calendar.replace("pub fn is_leap", "pub(crate) fn is_leap");
    let report = run_derived_files(&move_files(PRIVATE, &date, Some(CALENDAR)), &move_files(PRIVATE, DATE_MOVED, Some(&calendar)), &bodies);
    proved(&report, "moved `demo::date::is_leap` → `demo::calendar::is_leap`");
    assert_eq!(api(&report).len(), 0, "{report:#?}");
    assert_eq!(descriptions(&report).len(), 2, "{report:#?}");

    // Rule 6 (V2): the step has a visibility fact with an API text for the same item. One
    // fact is sufficient.
    let report = run_derived_files(&move_files(LIB, &date, Some(CALENDAR)), &move_files(LIB, DATE_MOVED, Some(&moved_calendar)), &bodies);
    proved(&report, "moved `demo::date::is_leap` → `demo::calendar::is_leap`");
    assert_eq!(api(&report), ["visibility of `demo::calendar::is_leap`: `private` → `pub` (the public API changes: a different crate can now use the item)"]);

    // A "possibly" side: a glob `pub use` line can bring the old path.
    let glob = "mod calendar;\nmod date;\npub use date::*;\n";
    let report = run_derived_files(&move_files(glob, DATE, Some(CALENDAR)), &move_files(glob, DATE_MOVED, Some(&moved_calendar)), &bodies);
    proved(&report, "moved `demo::date::is_leap` → `demo::calendar::is_leap`");
    assert_eq!(api(&report), ["public API can change: a glob `pub use` line names `date` (`demo::date::is_leap` is now `demo::calendar::is_leap`)"]);

    // A move between two public modules. A `pub use` line can keep the old path: then the
    // text does not say that the old path stops.
    let public = "pub mod calendar;\npub mod date;\n";
    let report = run_derived_files(&move_files(public, DATE, Some(CALENDAR)), &move_files(public, DATE_MOVED, Some(&moved_calendar)), &bodies);
    proved(&report, "moved `demo::date::is_leap` → `demo::calendar::is_leap`");
    assert_eq!(api(&report), ["public API: `demo::date::is_leap` is now `demo::calendar::is_leap` (a different crate can no longer use the old path)"]);
    let glob = |date: &str| format!("pub use crate::calendar::*;\n\n{}", date.replace("use crate::calendar::is_leap;\n\n", ""));
    let report = run_derived_files(&move_files(public, &glob(DATE), Some(CALENDAR)), &move_files(public, &glob(DATE_MOVED), Some(&moved_calendar)), &bodies);
    proved(&report, "moved `demo::date::is_leap` → `demo::calendar::is_leap`");
    assert_eq!(api(&report), ["public API: `demo::date::is_leap` is now `demo::calendar::is_leap` (a `pub use` line can keep the old path)"]);

    // A variant has no visibility of its own: the enum gives the answer.
    for (vis, expected) in [("pub ", 1), ("", 0)] {
        let before = format!("{vis}enum Event {{\n    Open,\n    Close,\n}}\npub fn code(e: &Event) -> u32 {{\n    match e {{\n        Event::Open => 1,\n        Event::Close => 2,\n    }}\n}}\n");
        let after = before.replace("Open", "Opened");
        let claims = vec![claim("c1", "rename", Some("demo::Event::Open"), serde_json::json!({"to": "Opened"}))];
        let report = run_claim_facts(&[("Cargo.toml", MANIFEST), ("src/lib.rs", &before)], &[("Cargo.toml", MANIFEST), ("src/lib.rs", &after)], claims, &[("demo::code", equal("n1"))]);
        assert!(report.accepted() && status_of(&report, "c1").is_verified(), "{report:#?}");
        let facts = api(&report);
        assert_eq!(facts.len(), expected, "{report:#?}");
        assert!(facts.iter().all(|text| text == "public API: `demo::Event::Open` is now `demo::Event::Opened` (a different crate can no longer use the old path)"), "{facts:?}");
    }
    // A method of a trait has no visibility of its own: the trait gives the answer.
    for (vis, expected) in [("pub ", 1), ("", 0)] {
        let before = format!("pub struct A;\n{vis}trait Price {{\n    fn price(&self) -> u32;\n    fn twice(&self) -> u32 {{\n        self.price() * 2\n    }}\n}}\nimpl Price for A {{\n    fn price(&self) -> u32 {{\n        3\n    }}\n}}\npub fn total(a: &A) -> u32 {{\n    a.price()\n}}\n");
        let after = before.replace("price", "cost");
        let claims = vec![claim("c1", "rename", Some("demo::Price::price"), serde_json::json!({"to": "cost"}))];
        let bodies = [("demo::Price::twice", equal("n0")), ("demo::<A as Price>::cost", equal("n0")), ("demo::total", equal("n0"))];
        let report = run_claim_facts(&[("Cargo.toml", MANIFEST), ("src/lib.rs", &before)], &[("Cargo.toml", MANIFEST), ("src/lib.rs", &after)], claims, &bodies);
        assert!(report.accepted() && status_of(&report, "c1").is_verified(), "{report:#?}");
        let facts = api(&report);
        assert_eq!(facts.len(), expected, "{report:#?}");
        assert!(facts.iter().all(|text| text == "public API: `demo::Price::price` is now `demo::Price::cost` (a different crate can no longer use the old path)"), "{facts:?}");
    }
}

#[test]
fn the_name_of_a_trait_impl_item_accepts_the_forms_of_the_source() {
    // Batch 5y, B1 (trial 8): the agent wrote the full trait path and the generic arguments.
    let (_dir, ws) = load("pub struct A;\npub struct B;\npub struct Label(pub u8);\nimpl From<A> for Label {\n    fn from(_: A) -> Label { Label(1) }\n}\nimpl core::convert::From<B> for Label {\n    fn from(_: B) -> Label { Label(2) }\n}\nimpl std::fmt::Display for Label {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, \"l\") }\n}\n");
    let names: Vec<&String> = ws.items.keys().filter(|path| path.contains(" as ")).collect();
    let one = |name: &str| crate::impl_name_matches(name, &ws);
    // A full trait path, and a short one: the lookup removes the path.
    assert_eq!(one("demo::<Label as core::fmt::Display>::fmt"), ["demo::<Label as std::fmt::Display>::fmt"], "{names:?}");
    assert_eq!(one("demo::impl <Label as Display>"), ["demo::impl <Label as std::fmt::Display>"], "{names:?}");
    // The generic arguments select between the twins.
    assert_eq!(one("demo::<Label as core::convert::From<A>>::from"), ["demo::<Label as From>::from"], "{names:?}");
    assert_eq!(one("demo::<Label as From<B>>::from"), ["demo::<Label as core::convert::From>::from"], "{names:?}");
    assert_eq!(one("demo::impl <demo::Label as From< A >>"), ["demo::impl <Label as From>"], "{names:?}");
    assert!(one("demo::<Label as Into<A>>::from").is_empty());
    // A name with no `impl ` word is the impl block.
    assert_eq!(one("demo::<Label as From<B>>"), ["demo::impl <Label as core::convert::From>"], "{names:?}");
    assert_eq!(one("demo::<Label as core::fmt::Display>"), ["demo::impl <Label as std::fmt::Display>"], "{names:?}");
    // No arguments: each match.
    assert_eq!(one("demo::<Label as From>::from").len(), 2);
    // The text of the fact shows the arguments.
    let block = &ws.items["demo::impl <Label as From>"];
    assert_eq!(crate::trait_arguments(block).as_deref(), Some("A"));
    assert_eq!(crate::trait_arguments(&ws.items["demo::impl <Label as std::fmt::Display>"]), None);
    // No match: the message lists the names with the same last segment.
    assert_eq!(crate::name_hint("demo::Label::from", &ws), ". The nearest names: `demo::<Label as From>::from`, `demo::<Label as core::convert::From>::from`");
}

#[test]
fn a_long_constant_value_shows_the_changed_part_and_the_index() {
    // Batch 5y, C1 (trial 8): one changed element of a table gave one line with the full table.
    let table = |value: u32| {
        let elements: Vec<String> = (0..120u32).map(|index| if index == 67 { value.to_string() } else { (index % 50 + 60).to_string() }).collect();
        format!("pub const TABLE: &[u8] = &[\n    {},\n];\npub fn first() -> u8 {{\n    TABLE[0]\n}}\n", elements.join(", "))
    };
    let (before, after) = (table(128), table(129));
    let bodies = vec![("demo::TABLE", different(false), vec![]), ("demo::first", different(false), vec![])];
    let report = run_calls(&before, &after, bodies);
    assert_eq!(report.facts.len(), 1, "{:#?}", report.facts);
    let text = &report.facts[0].description;
    assert!(text.starts_with("value of `demo::TABLE` changed at the index 67 (of 120 elements): `"), "{text}");
    assert!(text.contains("128") && text.contains("129") && text.contains('…') && text.chars().count() < 280, "{text}");
    // A short value has the full text of the two sides, and an array with a new length has the counts.
    assert_eq!(crate::facts::long_value_description("demo::A", "[1, 2]", "[1, 3]"), None);
    let long = "[10000000, 20000000, 30000000, 40000000, 50000000, 60000000, 70000000]";
    let text = crate::facts::long_value_description("demo::A", long, &long.replace(", 70000000", "")).unwrap();
    assert!(text.contains("(7 elements → 6 elements)"), "{text}");
}

#[test]
fn the_note_of_a_move_with_no_proof_gives_the_cause_that_stops_the_proof() {
    // Batch 5y, C3 (trial 8): a moved function that reads `module_path!()` has a different MIR.
    // The note gave the wider visibility as the cause, but a wider visibility stops no proof.
    let before = MOVE_BEFORE.replace("    pub fn over", "    pub(crate) fn over");
    let bodies = [("demo::b::over", different(false)), ("demo::b::run", equal("n1"))];
    let report = run_derived(&before, MOVE_AFTER, &bodies);
    // Design 36.1, rule 4: the changed body has no edit fact. It is a body of the move fact,
    // which has the result `code different` and the mismatch mark. The visibility is its own
    // fact, with no body.
    let moved = &report.facts[0];
    assert!(moved.description.starts_with("moved `demo::a::over` → `demo::b::over`") && moved.compare == Compare::CodeDifferent && moved.mismatch() && moved.affected == ["demo::b::over"], "{report:#?}");
    assert!(report.facts[1].description.starts_with("visibility of `demo::b::over`: `pub(crate)` → `pub`") && report.facts[1].compare == Compare::CodeEqual && report.notes.is_empty(), "{report:#?}");
    // The same move with an equal MIR has a proof.
    let report = run_derived(&before, MOVE_AFTER, &[("demo::b::over", equal("n0")), ("demo::b::run", equal("n1"))]);
    assert!(descriptions(&report).iter().any(|d| d.starts_with("moved `demo::a::over` → `demo::b::over`")), "{report:#?}");
}

/// The two sides of each "`A` → `B`" part of a fact text are different (batch 6a, A2).
fn sides_differ(description: &str) -> bool {
    let parts: Vec<&str> = description.split("` → `").collect();
    parts.windows(2).all(|pair| pair[0].rsplit('`').next() != pair[1].split('`').next())
}

/// Batch 6a, A1 (trial 9 on `heck`): a declaration change of an item that a different crate can
/// use is a fact to read. An equal MIR of the own crate is no proof for a different crate.
#[test]
fn a_declaration_change_of_a_public_item_names_the_public_api() {
    const BASE: &str = "use std::fmt;\npub struct Show<T: AsRef<str>>(pub T);\nimpl<T: AsRef<str>> fmt::Display for Show<T> {\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {\n        f.write_str(self.0.as_ref())\n    }\n}\npub trait Snake: ToOwned {\n    fn snake(&self) -> Self::Owned;\n}\npub trait Snek: Snake {}\nimpl<T: ?Sized + Snake> Snek for T {}\npub fn limit(n: u32) -> u32 {\n    n\n}\npub struct Pair {\n    pub count: u32,\n}\npub const LIMIT: u32 = 4;\npub enum Kind {\n    One = 1,\n}\n";
    let bodies = [("demo::<Show as fmt::Display>::fmt", equal("n0")), ("demo::limit", equal("n0"))];
    // (the old text, the new text, the start of the fact text)
    let cases = [
        // G1: a `where` clause on an impl.
        ("for Show<T> {", "for Show<T>\nwhere\n    for<'a> &'a T: Sized,\n    T: Copy,\n{", "generics of `demo::impl <Show as fmt::Display>`"),
        // G2: `?Sized` removed from a blanket impl.
        ("impl<T: ?Sized + Snake> Snek for T {}", "impl<T: Snake> Snek for T {}", "generics of `demo::impl <T as Snek>`"),
        // G3: a new supertrait.
        ("pub trait Snake: ToOwned {", "pub trait Snake: ToOwned + AsRef<str> {", "generics of `demo::Snake`"),
        // G4: a new bound on a struct.
        ("pub struct Show<T: AsRef<str>>(pub T);", "pub struct Show<T: AsRef<str> + Copy>(pub T);", "generics of `demo::Show`"),
        // G6: a `where` clause on a trait method.
        ("fn snake(&self) -> Self::Owned;", "fn snake(&self) -> Self::Owned\n    where\n        Self: AsRef<str>;", "where clause of `demo::Snake::snake`"),
        // Each other part of a declaration.
        ("pub fn limit(n: u32) -> u32 {", "pub fn limit(n: u64) -> u32 {", "parameter"),
        ("pub fn limit(n: u32) -> u32 {", "pub fn limit(n: u32) -> u64 {", "return type"),
        ("pub fn limit(n: u32) -> u32 {", "pub const fn limit(n: u32) -> u32 {", "qualifiers"),
        ("pub count: u32,", "pub count: u64,", "type"),
        ("pub const LIMIT: u32 = 4;", "pub const LIMIT: u32 = 5;", "value"),
        ("One = 1,", "One = 2,", "discriminant"),
    ];
    for (old, new, start) in cases {
        assert!(BASE.contains(old), "{old}");
        let report = run_derived(BASE, &BASE.replace(old, new), &bodies);
        let fact = report.facts.iter().find(|fact| fact.description.contains(start)).unwrap_or_else(|| panic!("{new}: {:#?}", descriptions(&report)));
        assert!(matches!(fact.category, Category::Declaration | Category::Semantics), "{new}: {fact:#?}");
        assert!(fact.description.contains("(the public API can change: a different crate can use the item)"), "{new}: {}", fact.description);
        assert!(report.facts.iter().all(|fact| sides_differ(&fact.description)), "{new}: {:#?}", descriptions(&report));
        // The API level of the report is `major`.
        let (_d1, before) = load(BASE);
        let (_d2, after) = load(&BASE.replace(old, new));
        let level = crate::api_level(&semilla_syntax::diff(&before, &after, &semilla_syntax::Renames::default()), &before, &after);
        assert_eq!(level.0, "major", "{new}: {level:?}");
    }
    // A bound change of a private item with an equal MIR: a declaration fact with no API text.
    let private = BASE.replace("pub ", "");
    let report = run_derived(&private, &private.replace("struct Show<T: AsRef<str>>(T);", "struct Show<T: AsRef<str> + Copy>(T);"), &bodies);
    let fact = report.facts.iter().find(|fact| fact.description.starts_with("generics of `demo::Show`")).unwrap_or_else(|| panic!("{:#?}", descriptions(&report)));
    assert!(fact.category == Category::Declaration && fact.compare == Compare::CodeEqual && !fact.description.contains("public API"), "{fact:#?}");
    // Each other declaration part of a private item with an equal MIR stays neutral.
    let report = run_derived(&private, &private.replace("fn limit(n: u32) -> u32 {", "const fn limit(n: u32) -> u32 {"), &bodies);
    assert!(report.facts.iter().all(|fact| fact.compare == Compare::CodeEqual), "{:#?}", report.facts);
    // `private` → `pub(crate)` stays neutral (batch 5x).
    let report = run_derived(&private, &private.replace("fn limit(n: u32) -> u32 {", "pub(crate) fn limit(n: u32) -> u32 {"), &bodies);
    assert_eq!(descriptions(&report), ["visibility of `demo::limit`: `private` → `pub(crate)`"]);
    assert_eq!(report.facts[0].compare, Compare::CodeEqual, "{:#?}", report.facts);
}

/// Batch 6a, A2: a fact text never has two equal sides. Each kind of fact with two sides gets a
/// long text with a small change at the end.
#[test]
fn a_fact_text_never_has_two_equal_sides() {
    const LONG: &str = "std::collections::BTreeMap<std::string::String, std::vec::Vec<std::option::Option<std::string::String>>>";
    let base = format!(
        "#![allow(clippy::alpha, clippy::beta, clippy::gamma, clippy::delta, clippy::epsilon, clippy::zeta, clippy::eta)]\nuse std::collections::{{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet, LinkedList, VecDeque, btree_map, hash_map}};\n/// A long doc line with many words that tells the reader each small fact of the item below, one.\n#[derive(Debug)]\n#[allow(clippy::alpha, clippy::beta, clippy::gamma, clippy::delta, clippy::epsilon, clippy::zeta, clippy::eta)]\npub struct Table<K: Clone + Ord + std::fmt::Debug + std::fmt::Display + Send + Sync + 'static + Default> {{\n    pub map: {LONG},\n    pub key: K,\n}}\npub trait Reader: Clone + Ord + std::fmt::Debug + std::fmt::Display + Send + Sync + 'static + Default {{}}\npub fn read<F>(table: {LONG}, f: F) -> {LONG}\nwhere\n    F: Fn(&std::string::String, &std::vec::Vec<std::option::Option<std::string::String>>) -> std::option::Option<u32>,\n{{\n    let _ = f;\n    table\n}}\npub const NAMES: [&str; 9] = [\"alpha\", \"beta\", \"gamma\", \"delta\", \"epsilon\", \"zeta\", \"eta\", \"theta\", \"iota\"];\npub static TEXT: &str = \"a long text with many words that goes on and on to the end of the line and then stops: one\";\npub type Map = {LONG};\npub fn value() -> u32 {{\n    let total = 1000 + 2000 + 3000 + 4000 + 5000 + 6000 + 7000 + 8000 + 9000 + 10000 + 11000 + 12000 + 13000 + 14000 + 15000 + 16000 + 17;\n    total\n}}\n"
    );
    let cases: [(&str, &str); 14] = [
        ("clippy::eta)]\nuse", "clippy::theta)]\nuse"),
        ("btree_map, hash_map};", "btree_map, hash_set};"),
        ("of the item below, one.", "of the item below, two."),
        ("clippy::eta)]\npub struct", "clippy::theta)]\npub struct"),
        ("+ 'static + Default> {", "+ 'static + Default + Copy> {"),
        ("std::string::String>>>,\n    pub key", "std::string::Str>>>,\n    pub key"),
        ("+ 'static + Default {}", "+ 'static + Default + Copy {}"),
        ("std::string::String>>>, f: F)", "std::string::Str>>>, f: F)"),
        ("f: F) -> std::collections::BTreeMap<std::string::String, std::vec::Vec<std::option::Option<std::string::String>>>", "f: F) -> std::collections::BTreeMap<std::string::String, std::vec::Vec<std::option::Option<std::string::Str>>>"),
        ("-> std::option::Option<u32>,", "-> std::option::Option<u64>,"),
        ("\"theta\", \"iota\"];", "\"theta\", \"kappa\"];"),
        ("and then stops: one\";", "and then stops: two\";"),
        ("std::string::String>>>;\npub fn value", "std::string::Str>>>;\npub fn value"),
        ("16000 + 17;", "16000 + 18;"),
    ];
    for bodies in [vec![], vec![("demo::read", equal("n0")), ("demo::value", different(false))]] {
        for (old, new) in cases {
            assert!(base.contains(old), "{old}");
            let report = run_derived(&base, &base.replace(old, new), &bodies);
            assert!(!report.facts.is_empty(), "{new}");
            for fact in &report.facts {
                assert!(sides_differ(&fact.description), "{new}: {}", fact.description);
                assert!(fact.description.contains("` → `") || fact.description.contains("added `") || fact.description.contains("removed `") || fact.description.contains("changed"), "{new}: {}", fact.description);
            }
        }
    }
}

/// Batch 6a, A3 (trial 9): `doc(test(…))` is no doc comment. It changes each doc test of the
/// crate, so it needs a reason. An attribute that changes only the doc pages needs no reason.
#[test]
fn a_doc_test_attribute_needs_a_reason() {
    let base = "#![doc(html_root_url = \"https://docs.rs/demo/1\")]\npub fn f() -> u32 {\n    1\n}\n";
    let bodies = [("demo::f", equal("n0"))];
    for (old, new, needs) in [
        ("#![doc(html_root_url", "#![doc(test(attr(cfg(any()))))]\n#![doc(html_root_url", true),
        ("#![doc(html_root_url", "#![doc(test(no_crate_inject))]\n#![doc(html_root_url", true),
        ("#![doc(html_root_url", "#![doc(unknown_key)]\n#![doc(html_root_url", true),
        ("demo/1", "demo/2", false),
        ("#![doc(html_root_url", "#![doc(html_logo_url = \"https://example.org/logo.png\")]\n#![doc(html_root_url", false),
        ("#![doc(html_root_url", "#![doc(cfg(feature = \"a\"))]\n#![doc(html_root_url", false),
    ] {
        let report = run_derived(base, &base.replace(old, new), &bodies);
        let fact = report.facts.iter().find(|fact| fact.description.starts_with("attributes of `demo`") || fact.description.starts_with("doc test of `demo`")).unwrap_or_else(|| panic!("{new}: {:#?}", descriptions(&report)));
        assert_eq!(fact.spotlight, needs, "{new}: {fact:#?}");
        if new.contains("doc(test(") {
            assert_eq!((fact.compare, fact.note.as_deref()), (Compare::NoCompare, Some("no compare: Semilla does not compile a doc test")), "{new}: {fact:#?}");
            assert!(fact.spotlight_why.as_deref().is_some_and(|why| why.contains("it changes each doc test of the crate")), "{new}: {fact:#?}");
            assert!(!report.accepted(), "{new}");
        }
    }
}

/// A `doc` attribute with a macro can bring doc tests: the fact is a doc test fact with no compare.
#[test]
fn a_doc_attribute_with_a_macro_is_a_doc_test_fact() {
    let base = "#![doc(html_root_url = \"https://docs.rs/demo/1\")]\npub fn f() -> u32 {\n    1\n}\n";
    let report = run_derived(base, &format!("#![doc = include_str!(\"../README.md\")]\n{base}"), &[("demo::f", equal("n0"))]);
    assert_eq!(descriptions(&report), ["doc test of `demo` added"], "{report:#?}");
    assert_eq!((report.facts[0].category, report.facts[0].compare), (Category::Docs, Compare::NoCompare), "{report:#?}");
}

/// Batch 6a, A4 (trial 9): a `use` fact has the text of its line. A changed line pairs with the
/// line that has the same path before the braces. A `pub use` line gives the public names.
#[test]
fn a_use_fact_shows_its_text_and_the_public_names() {
    const LIB: &str = "mod camel;\nmod snake;\npub use camel::{AsCamel, ToCamel, ToPascal};\npub use snake::ToSnake;\n";
    const CAMEL: &str = "use std::fmt;\npub struct AsCamel;\npub trait ToCamel {}\npub trait ToPascal {}\npub fn show(_: &dyn fmt::Debug) {}\n";
    const SNAKE: &str = "pub trait ToSnake {}\npub trait Other {}\n";
    let files = |lib: &'static str, camel: &'static str| [("Cargo.toml", MANIFEST), ("src/lib.rs", lib), ("src/camel.rs", camel), ("src/snake.rs", SNAKE)];
    let run = |lib: &'static str, camel: &'static str| {
        let (_d1, before) = load_files(&files(LIB, CAMEL));
        let (_d2, after) = load_files(&files(lib, camel));
        let level = crate::api_level(&semilla_syntax::diff(&before, &after, &semilla_syntax::Renames::default()), &before, &after);
        (run_derived_files(&files(LIB, CAMEL), &files(lib, camel), &[]), level)
    };
    // The defect of the trial: the public name `demo::ToPascal` goes to a different trait.
    let (report, level) = run("mod camel;\nmod snake;\npub use camel::{AsCamel, ToCamel};\npub use snake::ToSnake;\npub use snake::ToSnake as ToPascal;\n", CAMEL);
    let moved = "the public name `demo::ToPascal` now is `demo::snake::ToSnake` (before: `demo::camel::ToPascal`)";
    assert_eq!(
        descriptions(&report),
        [
            format!("changed `use` line: `pub use camel::{{AsCamel, ToCamel, ToPascal}};` → `pub use camel::{{AsCamel, ToCamel}};` in `demo`: {moved}"),
            format!("added `use` line: `pub use snake::ToSnake as ToPascal;` in `demo`: {moved}"),
        ],
        "{report:#?}"
    );
    assert!(report.facts.iter().all(|fact| fact.compare != Compare::CodeEqual), "{report:#?}");
    assert_eq!(level, ("major", vec![moved.to_string()]), "{level:?}");
    // An honest new line, and a removed name.
    let (report, level) = run("mod camel;\nmod snake;\npub use camel::{AsCamel, ToCamel};\npub use snake::{Other, ToSnake};\n", CAMEL);
    assert_eq!(
        descriptions(&report),
        [
            "changed `use` line: `pub use camel::{AsCamel, ToCamel, ToPascal};` → `pub use camel::{AsCamel, ToCamel};` in `demo`: removed the public name `demo::ToPascal` (`demo::camel::ToPascal`)",
            "changed `use` line: `pub use snake::ToSnake;` → `pub use snake::{Other, ToSnake};` in `demo`: added the public name `demo::Other` (`demo::snake::Other`)",
        ],
        "{report:#?}"
    );
    assert_eq!(level, ("major", vec!["removed the public name `demo::ToPascal` (`demo::camel::ToPascal`)".to_string(), "added the public name `demo::Other` (`demo::snake::Other`)".to_string()]), "{level:?}");
    // A glob: the text says "glob" and names the module.
    let (report, level) = run("mod camel;\nmod snake;\npub use camel::{AsCamel, ToCamel, ToPascal};\npub use snake::ToSnake;\npub use snake::*;\n", CAMEL);
    assert_eq!(descriptions(&report), ["added `use` line: `pub use snake::*;` in `demo`: added a glob: each public name of `demo::snake` now is a public name of `demo`"], "{report:#?}");
    assert_eq!(level.0, "minor", "{level:?}");
    // The same names in a different form: the text says that the names are the same.
    let (report, level) = run("mod camel;\nmod snake;\npub use camel::{AsCamel, ToPascal};\npub use camel::ToCamel;\npub use snake::ToSnake;\n", CAMEL);
    assert_eq!(
        descriptions(&report),
        [
            "changed `use` line: `pub use camel::{AsCamel, ToCamel, ToPascal};` → `pub use camel::{AsCamel, ToPascal};` in `demo` (the public names are the same)",
            "added `use` line: `pub use camel::ToCamel;` in `demo` (the public names are the same)",
        ],
        "{report:#?}"
    );
    assert_eq!(level, ("patch", Vec::new()), "{level:?}");
    // A private line has its text and no public name.
    let (report, _) = run(LIB, "use std::fmt;\nuse std::collections::BTreeMap;\npub struct AsCamel;\npub trait ToCamel {}\npub trait ToPascal {}\npub fn show(_: &dyn fmt::Debug) {}\n");
    assert_eq!(descriptions(&report), ["added `use` line: `use std::collections::BTreeMap;` in `demo::camel`"], "{report:#?}");
    let (report, _) = run(LIB, "pub struct AsCamel;\npub trait ToCamel {}\npub trait ToPascal {}\npub fn show(_: &dyn std::fmt::Debug) {}\n");
    assert!(descriptions(&report).contains(&"removed `use` line: `use std::fmt;` in `demo::camel`".to_string()), "{report:#?}");
}

/// Batch 6a, B1 (trial 9): a move keeps its proof when the `use` lines change in the usual
/// ways. The names that each module imports must be the same after the map of the move.
#[test]
fn a_move_keeps_its_proof_when_the_use_lines_change() {
    const LIB: &str = "mod a;\nmod b;\nmod other;\nfn helper(n: u32) -> u32 {\n    n + 1\n}\nfn keep(n: u32) -> u32 {\n    n\n}\n";
    const LIB_AFTER: &str = "mod a;\nmod b;\nmod other;\nmod words;\nfn keep(n: u32) -> u32 {\n    n\n}\n";
    const A: &str = "use crate::{helper, keep};\n\npub(crate) fn one(n: u32) -> u32 {\n    helper(keep(n))\n}\n";
    const B: &str = "use crate::{helper, keep};\n\npub(crate) fn two(n: u32) -> u32 {\n    helper(keep(n)) + 1\n}\n";
    const OTHER: &str = "pub(crate) fn helper(n: u32) -> u32 {\n    n + 2\n}\npub(crate) fn third(n: u32) -> u32 {\n    n\n}\n";
    const WORDS: &str = "pub(crate) fn helper(n: u32) -> u32 {\n    n + 1\n}\n";
    const A_SPLIT: &str = "use crate::keep;\nuse crate::words::helper;\n\npub(crate) fn one(n: u32) -> u32 {\n    helper(keep(n))\n}\n";
    const B_JOINED: &str = "use crate::{keep, words::helper};\n\npub(crate) fn two(n: u32) -> u32 {\n    helper(keep(n)) + 1\n}\n";
    let bodies = [("demo::a::one", equal("n1")), ("demo::b::two", equal("n1")), ("demo::words::helper", equal("n0"))];
    let run = |a_before: &str, a: &str, words: &str| {
        let before = [("Cargo.toml", MANIFEST), ("src/lib.rs", LIB), ("src/a.rs", a_before), ("src/b.rs", B), ("src/other.rs", OTHER)];
        let after = [("Cargo.toml", MANIFEST), ("src/lib.rs", LIB_AFTER), ("src/a.rs", a), ("src/b.rs", B_JOINED), ("src/other.rs", OTHER), ("src/words.rs", words)];
        // A body edit of the moved item gives a different MIR.
        let moved = if words == WORDS { equal("n0") } else { different(false) };
        run_derived_files(&before, &after, &[bodies[0].clone(), bodies[1].clone(), ("demo::words::helper", moved)])
    };
    // A step is clean if the move and its visibility are the only facts (design 36.1: each
    // other change has its own fact), and each fact has the result `code equal`.
    let clean = |report: &StepReport| report.accepted() && report.facts.len() == 2 && descriptions(report).iter().any(|text| text.starts_with("moved `demo::helper` → `demo::words::helper`")) && report.facts.iter().all(|fact| fact.compare == Compare::CodeEqual && !fact.spotlight);
    // The honest move: a split line in `a` and a joined line in `b`.
    let report = run(A, A_SPLIT, WORDS);
    assert!(clean(&report), "{report:#?}");
    assert_eq!(descriptions(&report), ["moved `demo::helper` → `demo::words::helper`, with 3 `use` line(s)", "visibility of `demo::words::helper`: `private` → `pub(crate)`"], "{report:#?}");
    // Each other honest form of the line in `a`.
    for a in ["use crate::{keep, words::helper};\n", "use crate::words::helper;\nuse crate::keep;\n", "use crate::{keep, words};\nuse words::helper;\n", "use crate::keep;\nuse super::words::helper;\n"] {
        let report = run(A, &A_SPLIT.replace("use crate::keep;\nuse crate::words::helper;\n", a), WORDS);
        assert_eq!(clean(&report), !a.contains("words}"), "{a}: {report:#?}");
    }
    // Must stay a fact to read, or no proof:
    let not_clean = [
        // An import of a different item with the same name.
        (A, A_SPLIT.replace("crate::words::helper", "crate::other::helper"), WORDS.to_string()),
        // A new `pub use` line.
        (A, A_SPLIT.replace("use crate::words::helper;", "pub use crate::words::helper;"), WORDS.to_string()),
        (A, format!("pub(crate) use crate::other::third;\n{A_SPLIT}"), WORDS.to_string()),
        // A new glob import.
        (A, A_SPLIT.replace("use crate::words::helper;", "use crate::words::*;"), WORDS.to_string()),
        (A, format!("use crate::other::*;\n{A_SPLIT}"), WORDS.to_string()),
        // A new import of a different name.
        (A, format!("use crate::other::third;\n{A_SPLIT}"), WORDS.to_string()),
        // A move with a body edit.
        (A, A_SPLIT.to_string(), WORDS.replace("n + 1", "n + 3")),
    ];
    for (a_before, a, words) in &not_clean {
        let report = run(a_before, a, words);
        assert!(!clean(&report), "{a}: {report:#?}");
    }
    // The import of a different item with the same name is a fact, not a part of the move.
    let report = run(A, &A_SPLIT.replace("crate::words::helper", "crate::other::helper"), WORDS);
    assert!(descriptions(&report).contains(&"added `use` line: `use crate::other::helper;` in `demo::a`".to_string()), "{report:#?}");
    // A removed import that a different item with the same name replaces through a glob.
    let glob_before = format!("use crate::other::*;\n{A}");
    let report = run(&glob_before, &format!("use crate::other::*;\n{}", A_SPLIT.replace("use crate::words::helper;\n", "")), WORDS);
    assert!(!clean(&report), "{report:#?}");
    // The same glob with the honest import stays clean.
    let report = run(&glob_before, &format!("use crate::other::*;\n{A_SPLIT}"), WORDS);
    assert!(clean(&report), "{report:#?}");
}

/// A body oracle with fixed comparisons (with spans), with the tests of the two trees, and with
/// the expanded code of the two trees.
struct MacroBodies(Vec<BodyComparison>, [BTreeSet<String>; 2], Option<[crate::made::Made; 2]>);

/// One macro call of a test tree: the start of the text of the call, the module of the call,
/// the items that the call makes (the expanded text, and true for an item that a different
/// crate can reach), and the bodies that the call makes.
type MadeCall<'a> = (&'a str, &'a str, &'a [(&'a str, bool)], &'a [&'a str]);

/// The expanded code of `src/lib.rs` of a test tree, in the form of the compiler data. A call
/// that the source does not have gives no data.
fn made_data(source: &str, calls: &[MadeCall]) -> crate::made::Made {
    let mut made = crate::made::Made::default();
    for (needle, module, items, bodies) in calls {
        let Some(at) = source.find(needle) else { continue };
        let line = source[..at].matches('\n').count() + 1;
        let col = source[..at].rsplit('\n').next().unwrap().chars().count();
        for (text, public) in *items {
            made.items.push(crate::made::MadeItem { file: "src/lib.rs".into(), line, col, module: module.to_string(), text: text.to_string(), test: text.starts_with("#[test]"), public: *public });
        }
        for body in *bodies {
            made.sites.insert(body.to_string(), ("src/lib.rs".into(), line, col));
        }
    }
    made
}

impl BodyOracle for MacroBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.clone())
    }

    fn tests(&self, _: &Step) -> anyhow::Result<Option<[BTreeSet<String>; 2]>> {
        Ok(Some(self.1.clone()))
    }

    fn made(&self, _: &Step) -> anyhow::Result<Option<[crate::made::Made; 2]>> {
        Ok(self.2.clone())
    }
}

/// Batch 7b, part 5.1: a function that an identity macro call wraps is not a removed item.
#[test]
fn a_function_in_an_identity_macro_call_is_a_macro_fact_with_a_compare() {
    const BASE: &str = "macro_rules! same {\n    ($($t:tt)*) => { $($t)* };\n}\n\nfn tilde(a: u32) -> bool {\n    a > 1\n}\n\npub fn top(a: u32) -> bool {\n    tilde(a)\n}\n";
    let after = BASE.replace("fn tilde(a: u32) -> bool {\n    a > 1\n}\n", "same! {\nfn tilde(a: u32) -> bool {\n    a > 1\n}\n}\n");
    let items = [("fn tilde(a: u32) -> bool { a > 1 }", false)];
    // The tokens of the function keep their places in the source: the compiler data has no
    // body with a place in the macro definition.
    let data = |source: &str| made_data(source, &[("same! {", "", &items[..], &[][..])]);
    let tests = [BTreeSet::new(), BTreeSet::new()];
    let wrapped = "`demo::tilde` comes from the macro call `same!` now";
    // The body is equal: the fact has the category `macro` and the result `code equal`.
    let oracle = MacroBodies(vec![body("demo::tilde", "src/lib.rs", 6, equal("n0")), body("demo::top", "src/lib.rs", 10, equal("n0"))], tests.clone(), Some([data(BASE), data(&after)]));
    let report = run_derived_with(BASE, &after, &oracle);
    let fact = report.facts.iter().find(|fact| fact.description == wrapped).unwrap_or_else(|| panic!("{report:#?}"));
    assert!(fact.category == Category::Macro && fact.compare == Compare::CodeEqual && report.accepted(), "{report:#?}");
    assert!(report.facts.iter().all(|fact| !fact.description.starts_with("removed `demo::tilde`")), "{report:#?}");
    // The body is not equal: the same fact has the result `code different`.
    let oracle = MacroBodies(vec![body("demo::tilde", "src/lib.rs", 6, different(false)), body("demo::top", "src/lib.rs", 10, equal("n0"))], tests.clone(), Some([data(BASE), data(&after)]));
    let report = run_derived_with(BASE, &after, &oracle);
    let fact = report.facts.iter().find(|fact| fact.description == wrapped).unwrap_or_else(|| panic!("{report:#?}"));
    assert!(fact.compare == Compare::CodeDifferent && fact.affected == ["demo::tilde"], "{report:#?}");
    // The reverse step: the function is an item of the source again.
    let oracle = MacroBodies(vec![body("demo::tilde", "src/lib.rs", 6, equal("n0")), body("demo::top", "src/lib.rs", 10, equal("n0"))], tests, Some([data(&after), data(BASE)]));
    let report = run_derived_with(&after, BASE, &oracle);
    let fact = report.facts.iter().find(|fact| fact.description == "`demo::tilde` came from the macro call `same!` before, and it is an item of the source now").unwrap_or_else(|| panic!("{report:#?}"));
    assert!(fact.category == Category::Macro && fact.compare == Compare::CodeEqual, "{report:#?}");
    // An added function with a body that no macro makes: the result is not `no code`.
    let plain = format!("{BASE}pub fn more(a: u32) -> u32 {{\n    a\n}}\n");
    let report = run_derived(BASE, &plain, &[("demo::more", BodyStatus::AfterOnly)]);
    assert!(report.facts.iter().all(|fact| fact.compare != Compare::NoCode), "{report:#?}");
    // An added type has no body: `no code`.
    let report = run_derived(BASE, &format!("{BASE}pub struct Unit;\n"), &[("demo::top", equal("n0"))]);
    assert!(report.facts.iter().all(|fact| fact.compare == Compare::NoCode), "{report:#?}");
}

/// Batch 6a, B2 (trial 9): a body that a new macro call makes needs no reason of its own. A
/// changed macro definition gets one fact and one spotlight for each body that it makes.
#[test]
fn a_body_of_a_macro_call_goes_under_the_fact_of_the_call() {
    const BASE: &str = "pub fn snake(text: &str) -> String {\n    text.to_lowercase()\n}\n#[cfg(test)]\nmod tests {\n    macro_rules! t {\n        ($t:ident : $a:expr => $b:expr) => {\n            #[test]\n            fn $t() {\n                assert_eq!(super::snake($a), $b)\n            }\n        };\n    }\n\n    t!(test1: \"A\" => \"a\");\n    t!(test2: \"B\" => \"b\");\n}\n";
    let names = |list: &[&str]| list.iter().map(|name| format!("demo::tests::{name}")).collect::<BTreeSet<String>>();
    // The span of a body from a macro is in the definition of the macro.
    let made = |name: &str, status: BodyStatus| body(&format!("demo::tests::{name}"), "src/lib.rs", 9, status);
    // The compiler data: each call `t!(testN: …)` makes the test function `testN`.
    let data = |source: &str| {
        let mut calls: Vec<(String, String, String)> = Vec::new();
        for n in 1..=3 {
            calls.push((format!("t!(test{n}:"), format!("#[test] fn test{n}() {{ }}"), format!("demo::tests::test{n}")));
        }
        let closures: Vec<String> = calls.iter().map(|call| format!("{}::{{closure#0}}", call.2)).collect();
        let items: Vec<[(&str, bool); 1]> = calls.iter().map(|call| [(call.1.as_str(), false)]).collect();
        let bodies: Vec<[&str; 2]> = calls.iter().zip(&closures).map(|(call, closure)| [call.2.as_str(), closure.as_str()]).collect();
        let list: Vec<MadeCall> = calls.iter().enumerate().map(|(index, call)| (call.0.as_str(), "tests", &items[index][..], &bodies[index][..])).collect();
        made_data(source, &list)
    };
    let both = |before: &str, after: &str| Some([data(before), data(after)]);
    // 1. A new call: the fact of the call lists the body. No spotlight.
    let after = BASE.replace("    t!(test2: \"B\" => \"b\");\n", "    t!(test2: \"B\" => \"b\");\n    t!(test3: \"C\" => \"c\");\n");
    let oracle = MacroBodies(vec![made("test3", BodyStatus::AfterOnly), made("test3::{closure#0}", BodyStatus::AfterOnly)], [names(&["test1", "test2"]), names(&["test1", "test2", "test3"])], both(BASE, &after));
    let report = run_derived_with(BASE, &after, &oracle);
    assert_eq!(descriptions(&report), ["added macro call `t!(test3: \"C\" => \"c\")` (makes: `test3`)"], "{report:#?}");
    assert!(report.accepted() && !report.facts[0].spotlight && report.facts[0].compare == Compare::CodeDifferent, "{report:#?}");
    assert_eq!(report.facts[0].affected, ["demo::tests::test3", "demo::tests::test3::{closure#0}"]);
    // A removed call: the same rule.
    let oracle = MacroBodies(vec![made("test3", BodyStatus::BeforeOnly), made("test3::{closure#0}", BodyStatus::BeforeOnly)], [names(&["test1", "test2", "test3"]), names(&["test1", "test2"])], both(&after, BASE));
    let report = run_derived_with(&after, BASE, &oracle);
    assert_eq!(descriptions(&report), ["removed macro call `t!(test3: \"C\" => \"c\")` (made: `test3`)"], "{report:#?}");
    assert!(report.accepted() && report.facts.iter().all(|fact| !fact.spotlight), "{report:#?}");
    // 2. A changed call: the fact of the call covers the body.
    let oracle = MacroBodies(vec![made("test2", different(false))], [names(&["test1", "test2"]), names(&["test1", "test2"])], both(BASE, BASE));
    let report = run_derived_with(BASE, &BASE.replace("\"B\" => \"b\"", "\"B\" => \"c\""), &oracle);
    assert_eq!(report.facts.len(), 1, "{report:#?}");
    assert_eq!(report.facts[0].description, "changed macro call `t!(test2: \"B\" => \"b\")` → `t!(test2: \"B\" => \"c\")` (makes: `test2`)");
    assert!(report.accepted() && !report.facts[0].spotlight && report.facts[0].affected == ["demo::tests::test2"], "{report:#?}");
    assert!(report.facts[0].description.ends_with("(makes: `test2`)"), "{report:#?}");
    // A new module with a new macro and new calls needs no reason.
    let (head, module) = BASE.split_at(BASE.find("#[cfg(test)]").unwrap());
    let oracle = MacroBodies(vec![made("test1", BodyStatus::AfterOnly), made("test2", BodyStatus::AfterOnly)], [BTreeSet::new(), names(&["test1", "test2"])], both(head, BASE));
    let report = run_derived_with(head, &format!("{head}{module}"), &oracle);
    assert!(report.accepted() && report.facts.iter().all(|fact| !fact.spotlight), "{report:#?}");
    assert!(!descriptions(&report).iter().any(|text| text.contains("does not see")), "{report:#?}");
    // 3. A changed definition: one fact and one spotlight. The text names the lost tests.
    let after = BASE.replace("            #[test]\n", "");
    let oracle = MacroBodies(vec![body("demo::tests::test1", "src/lib.rs", 8, different(true)), body("demo::tests::test2", "src/lib.rs", 8, different(true))], [names(&["test1", "test2"]), BTreeSet::new()], both(BASE, &after));
    let report = run_derived_with(BASE, &after, &oracle);
    assert_eq!(report.facts.len(), 1, "{report:#?}");
    let fact = &report.facts[0];
    assert!(!report.accepted() && fact.spotlight && fact.description.starts_with("changed macro `t`: "), "{report:#?}");
    assert_eq!(fact.spotlight_why.as_deref(), Some("a changed macro: the MIR of 2 bodies that the macro makes changed with no edit of the text of a call; 2 test(s) no longer exist: `test1`, `test2`"), "{fact:#?}");
    // One reason with the key of the macro is sufficient.
    let (_d1, old) = load(BASE);
    let (_d2, new) = load(&after);
    let mut step = step(Vec::new());
    step.reasons.insert(fact.reason_place(), "the tests go to a different runner".into());
    let report = crate::verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle, true).report;
    assert!(report.accepted() && report.facts.len() == 1, "{report:#?}");
    // A body with no macro call that has a fact keeps the old rule: a fact with a spotlight.
    let oracle = MacroBodies(vec![made("test9", BodyStatus::AfterOnly)], [names(&["test1", "test2"]), names(&["test1", "test2"])], both(BASE, BASE));
    let report = run_derived_with(BASE, &BASE.replace("text.to_lowercase()", "text.to_uppercase()"), &oracle);
    assert!(!report.accepted() && descriptions(&report).iter().any(|text| text.starts_with("code that Semilla does not see: `demo::tests::test9`")), "{report:#?}");
}

/// Batch 6a, A5 (trial 9): the fact of a trait impl shows the self type as the source has it.
/// The key of the item stays as it is.
#[test]
fn a_trait_impl_fact_shows_the_self_type_of_the_source() {
    let before = "pub struct As<T>(pub T);\npub trait Render {\n    fn render(&self) -> u32;\n}\n";
    let after = format!("{before}impl Render for As<&str> {{\n    fn render(&self) -> u32 {{\n        1\n    }}\n}}\nimpl From<u8> for As<Vec<u8>> {{\n    fn from(n: u8) -> Self {{\n        As(vec![n])\n    }}\n}}\n");
    let report = run_derived(before, &after, &[]);
    let texts = descriptions(&report);
    assert!(texts.iter().any(|text| text.starts_with("trait impl `<As<&str> as Render>` added with: `fn render`")) && texts.iter().any(|text| text.starts_with("trait impl `<As<Vec<u8>> as From<u8>>` added with: `fn from`")), "{texts:#?}");
    let fact = report.facts.iter().find(|fact| fact.description.contains("as Render")).unwrap();
    assert_eq!(fact.at.as_ref().map(|at| at.item.as_str()), Some("demo::impl <As as Render>"), "{fact:#?}");
}

/// Batch 6a, B4 (trial 9), and design 36.1: a move into a new `pub` module is a derived move.
/// The new public module is its own fact. An explicit claim keeps the cause of batch 6a.
#[test]
fn a_move_into_a_new_public_module_gives_the_real_cause() {
    let lib = "mod a;\nfn helper(n: u32) -> u32 {\n    n + 1\n}\n";
    let a = "use crate::helper;\n\npub(crate) fn one(n: u32) -> u32 {\n    helper(n)\n}\n";
    let a_after = a.replace("crate::helper", "crate::words::helper");
    let words = "pub(crate) fn helper(n: u32) -> u32 {\n    n + 1\n}\n";
    let run = |module: &str, lib_more: &str| {
        let lib_after = format!("mod a;\n{module} words;\n{lib_more}");
        let before = [("Cargo.toml", MANIFEST), ("src/lib.rs", lib), ("src/a.rs", a)];
        let after = [("Cargo.toml", MANIFEST), ("src/lib.rs", lib_after.as_str()), ("src/a.rs", a_after.as_str()), ("src/words.rs", words)];
        run_derived_files(&before, &after, &[("demo::a::one", equal("n1")), ("demo::words::helper", equal("n0"))])
    };
    let report = run("pub mod", "");
    let texts = descriptions(&report);
    assert_eq!(texts, ["moved `demo::helper` → `demo::words::helper`, with 1 `use` line(s)", "visibility of `demo::words::helper`: `private` → `pub(crate)`", "added `demo::words` (the public API changes: a different crate can now use the item)"]);
    assert!(report.notes.is_empty() && report.facts[0].compare == Compare::CodeEqual && report.facts[2].category == Category::Added, "{report:#?}");
    // A second change in the step: one more fact.
    let report = run("pub mod", "pub fn more() -> u32 {\n    2\n}\n");
    let texts = descriptions(&report);
    assert!(texts[0].starts_with("moved `demo::helper` → `demo::words::helper`") && texts.iter().any(|text| text.starts_with("added `demo::more`")) && texts.iter().any(|text| text.starts_with("added `demo::words`")) && report.notes.is_empty(), "{report:#?}");
    // A module that is not `pub` is a part of the move: it has no fact of its own.
    let report = run("mod", "");
    assert_eq!(descriptions(&report), ["moved `demo::helper` → `demo::words::helper`, with 1 `use` line(s)", "visibility of `demo::words::helper`: `private` → `pub(crate)`"]);
}

/// Batch 6a, B4 (trial 9): a file that the code reads at build time (`include_str!`,
/// `include_bytes!`, `include!`) is not a build condition. The spotlight names the file.
#[test]
fn the_spotlight_of_an_include_call_names_the_file() {
    let base = "pub fn f() -> usize {\n    1\n}\n";
    let spotlight = |facts: &[semilla_core::report::Fact]| facts.iter().filter_map(|f| f.spotlight_why.clone()).collect::<Vec<_>>().join("\n");
    // The uses come from the MIR records.
    let body = |uses: &[&str]| SpanBodies(vec![BodyComparison { name: "demo::f".into(), span: None, status: different(false), config: uses.iter().map(|text| text.to_string()).collect(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None }]);
    for (call, name, path) in [("include_str!(\"note.txt\")", "include_str!", "note.txt"), ("include_bytes!(\"../data/table.bin\")", "include_bytes!", "../data/table.bin")] {
        let report = run_derived_with(base, &format!("pub fn f() -> usize {{\n    {call}.len()\n}}\n"), &body(&[call]));
        let text = spotlight(&report.facts);
        assert!(text.contains(&format!("a file that the code reads at build time: `{path}` (`{name}` in `demo::f`)")), "{call}: {report:#?}");
        assert!(!text.contains("build condition") && !text.contains("build configuration"), "{call}: {text}");
        assert!(report.spotlight.iter().any(|line| line.starts_with("a file that the code reads at build time: demo::f")), "{call}: {:?}", report.spotlight);
    }
    // A build condition in the same body is a note, and the file has its spotlight.
    let report = run_derived_with(base, "pub fn f() -> usize {\n    if cfg!(unix) { 1 } else { include_str!(\"note.txt\").len() }\n}\n", &body(&["cfg!(unix)", "include_str!(\"note.txt\")"]));
    let text = spotlight(&report.facts);
    assert!(text.contains("a file that the code reads at build time: `note.txt` (`include_str!` in `demo::f`)") && !text.contains("build condition"), "{report:#?}");
    assert!(report.facts.iter().all(|fact| fact.note.as_deref() == Some("this body uses `cfg!(unix)`, which Semilla did not check (`demo::f`)")), "{report:#?}");
    assert!(report.spotlight.iter().all(|line| line.starts_with("a file that the code reads at build time: demo::f")), "{:?}", report.spotlight);
}

/// Batch 6a, A6: each changed token of a Rust file is in a fact. A part that the diff does not
/// know gives the net fact "changed source text with no other fact" with the class `other`.
#[test]
fn a_changed_source_text_with_no_other_fact_gets_the_net_fact() {
    const BASE: &str = "/// A table.\npub struct Table(pub u8);\nimpl Table {\n    /// The first table.\n    pub const FIRST: Table = Table(1);\n    /// Makes a table.\n    pub fn new(x: u8) -> Table {\n        Table(x)\n    }\n}\n/// A set.\npub trait Set {\n    /// The item.\n    type Item;\n}\nimpl Set for Table {\n    type Item = u8;\n}\nmod inner {\n    pub fn helper() -> u8 {\n        1\n    }\n}\npub use inner::helper;\nfn private_user() -> u8 {\n    2\n}\nstruct Hidden;\nimpl Hidden {\n    fn go(&self) -> u8 {\n        3\n    }\n}\n";
    let bodies = [("demo::Hidden::go", equal("n0")), ("demo::Table::new", equal("n0")), ("demo::inner::helper", equal("n0")), ("demo::private_user", equal("n0")), ("demo::Table::FIRST", equal("n0"))];
    let facts = |old: &str, new: &str| {
        assert!(BASE.contains(old), "{old}");
        let report = run_derived(BASE, &BASE.replacen(old, new, 1), &bodies);
        // A `cfg` attribute of an associated item has the spotlight of each `cfg` attribute.
        assert!(report.accepted() || new.contains("#[cfg("), "{old}: {:#?}", report.facts);
        // The category of a part of an associated item is the category of its change (36.2).
        for fact in &report.facts {
            let category = if fact.description.starts_with("visibility of the associated") { Some(Category::Visibility) } else if fact.description.starts_with("attributes of the associated") { Some(Category::Attribute) } else { None };
            assert!(category.is_none_or(|category| fact.category == category), "{fact:#?}");
        }
        report.facts.iter().map(|fact| (fact.description.clone(), fact.compare, fact.spotlight)).collect::<Vec<_>>()
    };
    let one = |old: &str, new: &str| {
        let list = facts(old, new);
        assert_eq!(list.len(), 1, "{old}: {list:#?}");
        list[0].clone()
    };
    let api = "(the public API can change: a different crate can use the item)";

    // The visibility of an associated constant.
    let (text, class, spotlight) = one("pub const FIRST", "pub(crate) const FIRST");
    assert_eq!(text, format!("visibility of the associated constant `FIRST` in `demo::impl Table`: `pub` → `pub(crate)` {api}"));
    assert_eq!((class, spotlight), (Compare::CodeEqual, false));
    // An attribute of an associated constant, and of an associated type.
    let (text, class, _) = one("    pub const FIRST", "    #[deprecated]\n    pub const FIRST");
    assert_eq!(text, format!("attributes of the associated constant `FIRST` in `demo::impl Table`: added `#[deprecated]` {api}"));
    assert_eq!(class, Compare::CodeEqual);
    let (text, class, _) = one("    type Item = u8;", "    #[cfg(all())]\n    type Item = u8;");
    assert!(text.starts_with("attributes of the associated type `Item` in `demo::impl <Table as Set>`: added `#[cfg(all())]`"), "{text}");
    assert_eq!(class, Compare::CodeEqual);
    // The visibility of a `use` line: one changed line, and the public name goes.
    for new in ["use inner::helper;", "pub(crate) use inner::helper;"] {
        let (text, class, _) = one("pub use inner::helper;", new);
        assert_eq!(text, format!("changed `use` line: `pub use inner::helper;` → `{new}` in `demo`: removed the public name `demo::helper` (`demo::inner::helper`)"));
        assert_eq!(class, Compare::CodeEqual);
    }
    // An attribute of a `use` line.
    let list = facts("pub use inner::helper;", "#[allow(unused_imports)]\npub use inner::helper;");
    assert!(list.iter().any(|(text, _, _)| text.starts_with("changed `use` line: `pub use inner::helper;` → `#[allow(unused_imports)] pub use inner::helper;`")), "{list:#?}");

    // The net: a kind of text with no fact kind (`default` on a method of an impl).
    let (text, class, spotlight) = one("    pub fn new(x: u8)", "    pub default fn new(x: u8)");
    assert_eq!(text, format!("changed source text with no other fact, in `demo::Table::new`: `pub fn new(x: u8) -> Table …` → `pub default fn new(x: u8) -> Table …` {api}"));
    // Design 37.1, rule 3: the text has no body, thus each side has a cut mark.
    assert_eq!((class, spotlight), (Compare::CodeEqual, false));
    // The net fact of an item that no different crate can use has the class `other` too.
    let (text, class, _) = one("    fn go(&self) -> u8 {", "    default fn go(&self) -> u8 {");
    assert!(text.starts_with("changed source text with no other fact, in `demo::Hidden::go`: ") && !text.contains("public API"), "{text}");
    assert_eq!(class, Compare::CodeEqual);

    // No net fact: a change of the format, a comma at the end, a known part.
    assert_eq!(facts("pub struct Table(pub u8);", "pub struct Table(\n    pub u8,\n);"), []);
    assert_eq!(facts("    type Item = u8;", "    type Item  =\n        u8;"), []);
    let (text, _, _) = one("pub fn new(x: u8) -> Table {", "pub const fn new(x: u8) -> Table {");
    assert!(text.starts_with("qualifiers of `demo::Table::new`"), "{text}");
    let (text, _, _) = one("pub struct Table(pub u8);", "pub(crate) struct Table(pub u8);");
    assert!(text.starts_with("visibility of `demo::Table`"), "{text}");
    // An attribute of the `doc` family has its own fact: the net adds no second fact.
    let (text, _, _) = one("pub struct Table(pub u8);", "#[doc(hidden)]\npub struct Table(pub u8);");
    assert!(!text.starts_with("changed source text") && text.contains("#[doc(hidden)]"), "{text}");
}

/// Batch 6b, part 1: a method that uses code under `cfg`. A text edit of its item gives the
/// note of the conditions (design 35).
const UNDER_CFG: &str = "#[cfg(unix)]\npub type Word = u64;\n#[cfg(not(unix))]\npub type Word = u32;\nfn width() -> Word { 8 }\npub const BASE: Word = 2;\npub struct S;\nimpl S {\n    /// The size.\n    #[inline]\n    pub fn size(&self) -> Word {\n        // a note\n        (|| width() + BASE)()\n    }\n}\n";

/// The start of the note of a fact with a text edit of the method of `UNDER_CFG`.
const UNDER_CFG_NOTE: &str = "this body uses `BASE` (its signature uses `Word`), `Word` (2 definitions, one under `#[cfg(unix)]`)";

/// The bodies of `UNDER_CFG`: the method and its closure, with one status.
/// The first test below makes sure that an edit of the body gets the note with this source.
fn under_cfg_bodies(status: BodyStatus) -> Comparisons {
    let body = |name: &str| BodyComparison { name: name.into(), span: Some(("src/lib.rs".into(), 11)), status: status.clone(), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: None };
    Comparisons(vec![body("demo::S::size"), body("demo::S::size::{closure#0}")])
}

#[test]
fn a_free_declaration_change_is_no_text_edit_of_the_body() {
    // Batch 6b, part 1. The visibility, a doc comment, `doc(hidden)`, `must_use`, and
    // `deprecated` do not change the code of the body. Before, such a change was a text edit
    // of the item, the proof of a body that uses code under `cfg` stopped, and the body got
    // the fact "indirect change, cause not known" (two times: the method and its closure).
    let cases = [
        ("    pub fn size", "    pub(crate) fn size", "visibility of `demo::S::size`: `pub` → `pub(crate)`"),
        ("    #[inline]\n", "    #[inline]\n    #[doc(hidden)]\n", "attributes of `demo::S::size`: added `#[doc(hidden)]`"),
        ("    #[inline]\n", "    #[must_use]\n    #[inline]\n", "attributes of `demo::S::size`: added `#[must_use]`"),
        ("    #[inline]\n", "    #[inline]\n    #[deprecated(note = \"old\")]\n", "attributes of `demo::S::size`: added `#[deprecated(note = \"old\")]`"),
        ("    /// The size.\n", "    /// The size in bytes.\n", "changed docs"),
    ];
    // The control: an edit of the body gets the note (the type `Word` is under `cfg`).
    let report = run_derived_with(UNDER_CFG, &UNDER_CFG.replace("width() + BASE", "BASE + width()"), &under_cfg_bodies(equal("n0")));
    assert!(!report.facts.is_empty() && report.facts.iter().all(|fact| fact.compare == Compare::CodeEqual && fact.note.as_deref().is_some_and(|note| note.starts_with(UNDER_CFG_NOTE))), "{:#?}", report.facts);
    for (old, new, text) in cases {
        let report = run_derived_with(UNDER_CFG, &UNDER_CFG.replace(old, new), &under_cfg_bodies(equal("n0")));
        assert!(unknown_causes(&report).is_empty(), "{new}: {:#?}", report.facts);
        assert_eq!(report.facts.len(), 1, "{new}: one change gives one fact: {:#?}", report.facts);
        let fact = &report.facts[0];
        assert!(fact.description.starts_with(text), "{new}: {fact:#?}");
        assert!(fact.note.is_none() && !fact.spotlight && fact.affected.is_empty() && fact.part != "comment", "{new}: {fact:#?}");
        assert!(report.accepted(), "{new}: {report:#?}");
    }
}

#[test]
fn an_attribute_that_can_change_the_body_is_a_text_edit_of_the_body() {
    // Batch 6b, part 1, item 3. Each attribute outside the list of the free parts stays a text
    // edit of the item: a body that uses code under `cfg` gets the note.
    for attribute in ["#[track_caller]", "#[cold]", "#[cfg(feature = \"x\")]", "#[target_feature(enable = \"avx2\")]", "#[doc(alias = \"len\")]", "#[other::thing]"] {
        let after = UNDER_CFG.replace("    #[inline]\n", &format!("    #[inline]\n    {attribute}\n"));
        let report = run_derived_with(UNDER_CFG, &after, &under_cfg_bodies(equal("n0")));
        // An attribute that Semilla cannot see into gives no note. It has a spotlight.
        let noted = report.facts.iter().any(|fact| fact.note.as_deref().is_some_and(|note| note.starts_with(UNDER_CFG_NOTE)) || (fact.spotlight && ["target_feature", "other::thing"].iter().any(|name| attribute.contains(name))));
        assert!(noted, "{attribute}: {:#?}", report.facts);
    }
    // A removed `inline` is in the same rule.
    let report = run_derived_with(UNDER_CFG, &UNDER_CFG.replace("    #[inline]\n", ""), &under_cfg_bodies(equal("n0")));
    assert!(report.facts.iter().any(|fact| fact.note.as_deref().is_some_and(|note| note.starts_with(UNDER_CFG_NOTE))), "{:#?}", report.facts);
    // An attribute macro on the item or on the `impl` block can read the visibility. The
    // compare result stays (design 35).
    for (place, with_macro) in [("    #[inline]\n", "    #[inline]\n    #[other::thing]\n"), ("impl S {", "#[other::thing]\nimpl S {")] {
        let before = UNDER_CFG.replace(place, with_macro);
        let report = run_derived_with(&before, &before.replace("    pub fn size", "    pub(crate) fn size"), &under_cfg_bodies(equal("n0")));
        assert!(report.accepted() && report.facts.iter().all(|fact| fact.compare == Compare::CodeEqual), "{with_macro}: {:#?}", report.facts);
    }
}

#[test]
fn a_real_indirect_change_next_to_a_free_declaration_change_keeps_the_spotlight() {
    // Batch 6b, part 1, item 5. The MIR of the method is different, and the step has only a
    // free declaration change of the method: the cause is not known. One item gives one fact.
    for (old, new) in [("    #[inline]\n", "    #[inline]\n    #[doc(hidden)]\n"), ("    pub fn size", "    pub(crate) fn size"), ("    #[inline]\n", "    #[inline]\n    #[must_use]\n")] {
        let report = run_derived_with(UNDER_CFG, &UNDER_CFG.replace(old, new), &under_cfg_bodies(different(false)));
        assert_eq!(unknown_causes(&report), ["demo::S::size"], "{new}: {:#?}", report.facts);
        let fact = report.facts.iter().find(|fact| fact.part == "indirect change").unwrap();
        assert!(fact.spotlight && fact.reason.is_none() && !report.accepted(), "{new}: {fact:#?}");
    }
    // A changed constant that the method reads is the cause: the body goes under its fact.
    let after = UNDER_CFG.replace("    #[inline]\n", "    #[inline]\n    #[doc(hidden)]\n").replace("BASE: Word = 2", "BASE: Word = 3");
    let report = run_derived_with(UNDER_CFG, &after, &under_cfg_bodies(different(false)));
    let cause = report.facts.iter().find(|fact| fact.affected.iter().any(|body| body == "demo::S::size")).unwrap_or_else(|| panic!("{:#?}", report.facts));
    assert!(cause.description.contains("BASE") && cause.compare == Compare::CodeDifferent, "{cause:#?}");
    assert!(report.facts.iter().any(|fact| fact.description == "attributes of `demo::S::size`: added `#[doc(hidden)]`" && fact.affected.is_empty()), "{:#?}", report.facts);
    // A visibility change together with an edit of the body: the body edit is a fact to read.
    let after = UNDER_CFG.replace("    pub fn size", "    pub(crate) fn size").replace("width() + BASE", "width() * BASE");
    let report = run_derived_with(UNDER_CFG, &after, &under_cfg_bodies(different(false)));
    let edit = report.facts.iter().find(|fact| fact.affected.iter().any(|body| body == "demo::S::size")).unwrap_or_else(|| panic!("{:#?}", report.facts));
    assert!(edit.description.contains("changed operator") && edit.compare == Compare::CodeDifferent, "{edit:#?}");
    assert!(report.facts.iter().any(|fact| fact.description.starts_with("visibility of `demo::S::size`")), "{:#?}", report.facts);
    // The same edit with an equal MIR in the checked configuration: the body uses code under
    // `cfg`, so each fact of the method has the note. The compare result stays.
    let report = run_derived_with(UNDER_CFG, &after, &under_cfg_bodies(equal("n0")));
    assert!(report.facts.iter().all(|fact| fact.compare == Compare::CodeEqual && fact.note.as_deref().is_some_and(|note| note.starts_with(UNDER_CFG_NOTE))), "{:#?}", report.facts);
}

/// The source of the macro tests of batch 6b: a public type with one trait impl.
const KEBAB: &str = "use std::fmt;\npub struct AsKebabCase<T>(pub T);\nimpl<T: AsRef<str>> fmt::Display for AsKebabCase<T> {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {\n        f.write_str(self.0.as_ref())\n    }\n}\n";
const SAME: &str = "macro_rules! same {\n    ($t:ty) => {\n        impl PartialEq for AsKebabCase<$t> {\n            fn eq(&self, other: &Self) -> bool {\n                self.0 == other.0\n            }\n        }\n    };\n}\n";

/// The compiler data of a tree with calls of `same!`: each call makes one impl with one body.
fn same_data(source: &str) -> crate::made::Made {
    let mut calls: Vec<(String, String, String)> = Vec::new();
    for ty in ["&str", "String"] {
        calls.push((format!("same!({ty});"), format!("impl PartialEq for AsKebabCase<{ty}> {{\n    fn eq(&self, other: &Self) -> bool {{ self.0 == other.0 }}\n}}"), format!("<demo::AsKebabCase<{ty}> as core::cmp::PartialEq>::eq")));
    }
    let items: Vec<[(&str, bool); 1]> = calls.iter().map(|call| [(call.1.as_str(), true)]).collect();
    let bodies: Vec<[&str; 1]> = calls.iter().map(|call| [call.2.as_str()]).collect();
    let list: Vec<MadeCall> = calls.iter().enumerate().map(|(index, call)| (call.0.as_str(), "", &items[index][..], &bodies[index][..])).collect();
    made_data(source, &list)
}

/// Batch 6b, part 2 (trial 10): the fact of a macro call lists the items that the call makes.
/// The data comes from the compiler. The public API list has an item that a different crate
/// can use. The affected bodies are only the bodies that the step changes.
#[test]
fn a_macro_call_fact_lists_the_items_that_the_call_makes() {
    let eq = |ty: &str, status: BodyStatus| body(&format!("<demo::AsKebabCase<{ty}> as core::cmp::PartialEq>::eq"), "src/lib.rs", 11, status);
    let none = || [BTreeSet::new(), BTreeSet::new()];
    let both = |before: &str, after: &str| Some([same_data(before), same_data(after)]);
    // 1. One step adds the definition and the call.
    let with_str = format!("{KEBAB}{SAME}same!(&str);\n");
    let report = run_derived_with(KEBAB, &with_str, &MacroBodies(vec![eq("&str", BodyStatus::AfterOnly)], none(), both(KEBAB, &with_str)));
    assert_eq!(descriptions(&report), ["added macro `same`", "added macro call `same!(&str)` (makes: `impl PartialEq for AsKebabCase<&str>`)"], "{report:#?}");
    assert!(report.accepted() && report.facts.iter().all(|fact| !fact.spotlight && fact.category == Category::Added), "{report:#?}");
    assert_eq!((report.facts[0].compare, report.facts[1].compare), (Compare::NoCode, Compare::CodeDifferent), "{report:#?}");
    // The body of the made impl is under the fact of the call. The step does not change `fmt`.
    assert!(report.facts[0].affected.is_empty(), "{report:#?}");
    assert_eq!(report.facts[1].affected, ["<demo::AsKebabCase<&str> as core::cmp::PartialEq>::eq"], "{report:#?}");
    // The API rule of the same impl with no macro: a new trait impl of a public type.
    assert_eq!(report.api_level.as_deref(), Some("minor"), "{report:#?}");
    assert_eq!(report.api_changes, ["added `impl PartialEq for AsKebabCase<&str>` (the macro call `same!(&str)` makes it)"], "{report:#?}");
    let direct = format!("{KEBAB}impl PartialEq for AsKebabCase<&str> {{\n    fn eq(&self, other: &Self) -> bool {{\n        self.0 == other.0\n    }}\n}}\n");
    let report = run_derived(KEBAB, &direct, &[]);
    assert_eq!(report.api_level.as_deref(), Some("minor"), "{report:#?}");
    assert_eq!(report.api_changes, ["`AsKebabCase<&str>` now has `PartialEq`"], "{report:#?}");
    let report = run_derived(&direct, KEBAB, &[]);
    assert_eq!((report.api_level.as_deref(), &report.api_changes[..]), (Some("major"), &["`AsKebabCase<&str>` no longer has `PartialEq` (the public API can change)".to_string()][..]), "{report:#?}");
    // 2. A changed call: the fact shows the two calls and the two lists.
    let with_string = format!("{KEBAB}{SAME}same!(String);\n");
    let oracle = MacroBodies(vec![eq("&str", BodyStatus::BeforeOnly), eq("String", BodyStatus::AfterOnly)], none(), both(&with_str, &with_string));
    let report = run_derived_with(&with_str, &with_string, &oracle);
    assert_eq!(descriptions(&report), ["changed macro call `same!(&str)` → `same!(String)` (made: `impl PartialEq for AsKebabCase<&str>`; makes: `impl PartialEq for AsKebabCase<String>`)"], "{report:#?}");
    assert!(report.accepted() && !report.facts[0].spotlight, "{report:#?}");
    assert_eq!(report.facts[0].affected, ["<demo::AsKebabCase<&str> as core::cmp::PartialEq>::eq", "<demo::AsKebabCase<String> as core::cmp::PartialEq>::eq"], "{report:#?}");
    assert_eq!(report.api_level.as_deref(), Some("major"), "{report:#?}");
    assert_eq!(report.api_changes, ["removed `impl PartialEq for AsKebabCase<&str>` (the macro call `same!(&str)` made it)", "added `impl PartialEq for AsKebabCase<String>` (the macro call `same!(String)` makes it)"], "{report:#?}");
    // 3. A removed call.
    let no_call = format!("{KEBAB}{SAME}");
    let report = run_derived_with(&with_str, &no_call, &MacroBodies(vec![eq("&str", BodyStatus::BeforeOnly)], none(), both(&with_str, &no_call)));
    assert_eq!(descriptions(&report), ["removed macro call `same!(&str)` (made: `impl PartialEq for AsKebabCase<&str>`)"], "{report:#?}");
    assert_eq!(report.api_level.as_deref(), Some("major"), "{report:#?}");
    // 4. A second call: the names of the calls have an index, and each fact has its own body.
    let two = format!("{KEBAB}{SAME}same!(&str);\nsame!(String);\n");
    let report = run_derived_with(&with_str, &two, &MacroBodies(vec![eq("String", BodyStatus::AfterOnly)], none(), both(&with_str, &two)));
    assert_eq!(descriptions(&report), ["added macro call `same!(String)` (makes: `impl PartialEq for AsKebabCase<String>`)"], "{report:#?}");
    assert_eq!(report.facts[0].at.as_ref().map(|at| at.item.as_str()), Some("demo::same!#1"), "{report:#?}");
    // 5. A changed definition with no changed call keeps the rule of batch 6a.
    let changed = with_str.replace("self.0 == other.0", "self.0 != other.0");
    let report = run_derived_with(&with_str, &changed, &MacroBodies(vec![eq("&str", different(false))], none(), both(&with_str, &changed)));
    assert_eq!(report.facts.len(), 1, "{report:#?}");
    assert!(!report.accepted() && report.facts[0].spotlight && report.facts[0].description.starts_with("changed macro `same`: "), "{report:#?}");
}

/// Batch 6b, part 2: an item that a macro call makes follows the rules of the same item with
/// no macro. If Semilla cannot list the items, the fact says so and the API level is `major`.
#[test]
fn a_made_item_follows_the_rules_of_an_item_with_no_macro() {
    const BASE: &str = "pub struct Guard;\npub fn keep() {}\nmacro_rules! make {\n    ($($t:tt)*) => {\n        $($t)*\n    };\n}\n";
    let none = || [BTreeSet::new(), BTreeSet::new()];
    // One call `make! { … }` at the end of the file makes the items of `items`.
    let run = |items: &[(&str, bool)], bodies: &[&str], reason: Option<&str>| {
        let after = format!("{BASE}make! {{ {} }}\n", items.iter().map(|item| item.0.replace("#[test] ", "#[test]\n")).collect::<Vec<_>>().join(" "));
        let made = Some([crate::made::Made::default(), made_data(&after, &[("make! {", "", items, bodies)])]);
        // The body of `keep` is equal: the body check has data in each case.
        let mut list: Vec<BodyComparison> = bodies.iter().map(|name| body(name, "src/lib.rs", 5, BodyStatus::AfterOnly)).collect();
        list.push(body("demo::keep", "src/lib.rs", 2, equal("n0")));
        let oracle = MacroBodies(list, none(), made);
        let (_d1, old) = load(BASE);
        let (_d2, new) = load(&after);
        let mut step = step(Vec::new());
        if let Some(reason) = reason {
            step.reasons.insert("demo::make!#0".into(), reason.into());
        }
        crate::verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle, true).report
    };
    // A public function that a different crate can use: a line in the public API list.
    let report = run(&[("pub fn made_one() -> u32 { 1 }", true)], &["demo::made_one"], None);
    assert!(report.facts[0].description.ends_with("(makes: `pub fn made_one`)"), "{report:#?}");
    assert!(report.accepted() && !report.facts[0].spotlight && report.facts[0].affected == ["demo::made_one"], "{report:#?}");
    assert_eq!(report.api_level.as_deref(), Some("minor"), "{report:#?}");
    assert!(report.api_changes[0].starts_with("added `pub fn made_one` (the macro call `make! {"), "{report:#?}");
    // The compiler says that no different crate can use the function: no API line.
    let report = run(&[("pub fn made_one() -> u32 { 1 }", false)], &["demo::made_one"], None);
    assert!(report.accepted() && report.api_level.as_deref() == Some("patch") && report.api_changes.is_empty(), "{report:#?}");
    // A test function: the text of batch 6a stays.
    let report = run(&[("#[test] fn test26() { }", false)], &["demo::test26"], None);
    assert!(report.facts[0].description.ends_with("(makes: `test26`)"), "{report:#?}");
    assert!(report.accepted() && !report.facts[0].spotlight && report.api_level.as_deref() == Some("patch"), "{report:#?}");
    // Each item kind is in the list.
    let report = run(&[("pub struct Made;", true), ("pub(crate) const LIMIT: u32 = 1;", false), ("impl Guard { pub fn get(&self) -> u32 { 1 } }", true), ("pub use std::fmt::Debug;", true)], &["demo::Guard::get"], None);
    assert!(report.facts[0].description.ends_with("(makes: `impl Guard`, `pub struct Made`, `pub use std::fmt::Debug`, `pub(crate) const LIMIT`)"), "{report:#?}");
    assert!(report.accepted() && report.api_level.as_deref() == Some("minor") && report.api_changes.len() == 3, "{report:#?}");
    // An impl of `Drop`: the compiler calls it with no call in the text. The fact needs a reason.
    let drop_impl = [("impl Drop for Guard { fn drop(&mut self) { } }", true)];
    let report = run(&drop_impl, &["<demo::Guard as core::ops::Drop>::drop"], None);
    assert!(report.facts[0].description.ends_with("(makes: `impl Drop for Guard`)"), "{report:#?}");
    assert!(!report.accepted() && report.facts[0].spotlight && report.facts[0].spotlight_why.as_deref().is_some_and(|why| why.starts_with("an implicit call: the macro call `make! {") && why.contains("makes `impl Drop for Guard`")), "{report:#?}");
    assert!(report.spotlight.iter().any(|line| line.starts_with("implicit call: demo::make!#0 (")), "{report:#?}");
    let report = run(&drop_impl, &["<demo::Guard as core::ops::Drop>::drop"], Some("the guard writes a log line"));
    assert!(report.accepted() && report.facts[0].reason.is_some(), "{report:#?}");
    // An `unsafe impl`: a line in the spotlight list of the step.
    let report = run(&[("unsafe impl Send for Guard { }", true)], &[], None);
    assert!(report.spotlight.iter().any(|line| line.starts_with("unsafe code: demo::make!#0 (the macro call `make! {") && line.ends_with("makes `unsafe impl Send for Guard`)")), "{report:#?}");
    // An attribute that can change the behavior: the rule of a new item.
    let report = run(&[("#[no_mangle] pub extern \"C\" fn made_fn() { }", true)], &["demo::made_fn"], None);
    assert!(!report.accepted() && report.facts[0].spotlight_why.as_deref().is_some_and(|why| why.starts_with("a new item with an attribute that can change the behavior: `no_mangle` on `pub fn made_fn`, which the macro call")), "{report:#?}");
    // An impl of a derive has the attributes of the compiler: no spotlight.
    let report = run(&[("#[automatically_derived] impl ::core::clone::Clone for Guard { #[inline] fn clone(&self) -> Guard { Guard } }", true)], &[], None);
    assert!(report.accepted() && !report.facts[0].spotlight, "{report:#?}");
    // A text that Semilla cannot read: the fact says so, and the API level is `major`.
    let report = run(&[("fn broken(a b) { }", false)], &[], None);
    assert!(report.facts[0].description.ends_with("(makes items that Semilla cannot list)"), "{report:#?}");
    assert!(!report.accepted() && report.facts[0].spotlight_why.as_deref().is_some_and(|why| why.contains("makes items that Semilla cannot list")), "{report:#?}");
    assert_eq!(report.api_level.as_deref(), Some("major"), "{report:#?}");
    assert!(report.api_changes[0].ends_with("it makes items that Semilla cannot list (the public API can change)"), "{report:#?}");
    // The oracle gives no compiler data: the same rule.
    let after = format!("{BASE}make! {{ pub fn made_one() -> u32 {{ 1 }} }}\n");
    let report = run_derived_with(BASE, &after, &MacroBodies(vec![body("demo::made_one", "src/lib.rs", 5, BodyStatus::AfterOnly)], none(), None));
    assert!(report.facts[0].description.ends_with("(makes items that Semilla cannot list)") && !report.accepted() && report.api_level.as_deref() == Some("major"), "{report:#?}");
    // A call that makes no item.
    let report = run(&[], &[], None);
    assert!(report.facts[0].description.ends_with("(makes no item in the checked configurations)") && report.accepted() && report.api_level.as_deref() == Some("patch"), "{report:#?}");
}

/// Batch 6b, part 2: a macro call in a function body is a statement or an expression of the
/// body, not an item. The rule for it does not change: the edit is an edit of the body.
#[test]
fn a_macro_call_in_a_function_body_keeps_its_rule() {
    let before = "macro_rules! table { ($($x:expr),*) => { [$($x),*] }; }\npub fn f() -> [u32; 2] {\n    table!(1, 2)\n}\n";
    let after = before.replace("table!(1, 2)", "table!(1, 3)");
    let bodies = || vec![body("demo::f", "src/lib.rs", 2, different(false))];
    let none = || [BTreeSet::new(), BTreeSet::new()];
    let with_data = run_derived_with(before, &after, &MacroBodies(bodies(), none(), Some([crate::made::Made::default(), crate::made::Made::default()])));
    let with_no_data = run_derived_with(before, &after, &MacroBodies(bodies(), none(), None));
    assert_eq!(descriptions(&with_data), descriptions(&with_no_data));
    assert_eq!(with_data.uncovered.len(), with_no_data.uncovered.len());
    assert!(!descriptions(&with_data).iter().any(|text| text.contains("macro call `")), "{with_data:#?}");
    assert!(with_data.facts.iter().any(|fact| fact.affected == ["demo::f"]), "{with_data:#?}");
    assert_eq!(with_data.api_level, with_no_data.api_level);
}

const BOUNDS: &str = "use std::fmt::{self, Debug, Display};\npub trait Snake: ToOwned + Debug {\n    fn snake<T: Clone + Debug>(&self, t: T) -> T;\n    fn each<I>(&self, it: I) -> usize\n    where\n        I: Iterator,\n        I::Item: Debug;\n}\npub trait Walk: Iterator\nwhere\n    Self::Item: Debug,\n{\n}\npub trait Keep {\n    type It: Debug;\n}\npub struct Show<T: AsRef<str> + Clone>(pub T);\npub enum Pick<A: Clone + Debug, B = u8> {\n    One(A),\n    Two(B),\n}\nimpl<T: AsRef<str> + Clone> Show<T> {\n    pub fn get<U: Debug + Clone>(&self, u: U) -> U {\n        u\n    }\n}\nimpl<T: AsRef<str> + Clone> Display for Show<T> {\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {\n        f.write_str(self.0.as_ref())\n    }\n}\npub fn limit<A: Clone + Debug, B: Debug>(a: A, b: B) -> A {\n    drop(b);\n    a\n}\npub fn lives<'a, 'b: 'a, T: 'a + ?Sized>(x: &'a T, y: &'b u8) -> &'a T {\n    drop(y);\n    x\n}\npub fn calls<F>(f: F)\nwhere\n    F: for<'a> Fn(&'a u8) + Clone,\n{\n    f(&1)\n}\npub fn dynamic(x: &(dyn Debug + Send)) {\n    drop(x)\n}\npub fn takes(x: impl Debug) {\n    drop(x)\n}\npub fn gives() -> impl Debug + Clone {\n    1u8\n}\npub fn walk<F, G>(s: &str, mut f: F, mut g: G, n: &mut fmt::Formatter) -> fmt::Result\nwhere\n    F: FnMut(&str, &mut fmt::Formatter) -> fmt::Result,\n    G: FnMut(&mut fmt::Formatter) -> fmt::Result,\n{\n    f(s, n)?;\n    g(n)\n}\n";

fn bounds_bodies() -> Vec<(&'static str, BodyStatus)> {
    ["demo::limit", "demo::lives", "demo::calls", "demo::walk", "demo::dynamic", "demo::takes", "demo::gives", "demo::<Show as Display>::fmt", "demo::Show::get"].into_iter().map(|name| (name, equal("n0"))).collect()
}

/// Batch 6b, part 3: a change of the bounds with an equal set of predicates, and with each
/// other part of the declaration equal, is neutral if each body has an equal MIR.
#[test]
fn a_bound_change_with_an_equal_set_of_predicates_is_neutral() {
    // (the old text, the new text, the fact text)
    let cases = [
        // A function: the order, and the move to the `where` clause.
        ("pub fn limit<A: Clone + Debug, B: Debug>(a: A, b: B) -> A {", "pub fn limit<A: Debug + Clone, B: Debug>(a: A, b: B) -> A {", "bounds of `demo::limit`: order only"),
        ("pub fn limit<A: Clone + Debug, B: Debug>(a: A, b: B) -> A {", "pub fn limit<A, B>(a: A, b: B) -> A\nwhere\n    A: Clone + Debug,\n    B: Debug,\n{", "bounds of `demo::limit`: moved to the `where` clause"),
        // A method.
        ("pub fn get<U: Debug + Clone>(&self, u: U) -> U {", "pub fn get<U: Clone + Debug>(&self, u: U) -> U {", "bounds of `demo::Show::get`: order only"),
        ("pub fn get<U: Debug + Clone>(&self, u: U) -> U {", "pub fn get<U>(&self, u: U) -> U\n    where\n        U: Debug,\n        U: Clone,\n    {", "bounds of `demo::Show::get`: moved to the `where` clause"),
        // A struct and an enum.
        ("pub struct Show<T: AsRef<str> + Clone>(pub T);", "pub struct Show<T: Clone + AsRef<str>>(pub T);", "bounds of `demo::Show`: order only"),
        ("pub struct Show<T: AsRef<str> + Clone>(pub T);", "pub struct Show<T>(pub T)\nwhere\n    T: AsRef<str> + Clone;", "bounds of `demo::Show`: moved to the `where` clause"),
        ("pub enum Pick<A: Clone + Debug, B = u8> {", "pub enum Pick<A: Debug + Clone, B = u8> {", "bounds of `demo::Pick`: order only"),
        // An impl and a trait impl.
        ("impl<T: AsRef<str> + Clone> Show<T> {", "impl<T: Clone + AsRef<str>> Show<T> {", "bounds of `demo::impl Show`: order only"),
        ("impl<T: AsRef<str> + Clone> Show<T> {", "impl<T> Show<T>\nwhere\n    T: AsRef<str> + Clone,\n{", "bounds of `demo::impl Show`: moved to the `where` clause"),
        ("impl<T: AsRef<str> + Clone> Display for Show<T> {", "impl<T: Clone + AsRef<str>> Display for Show<T> {", "bounds of `demo::impl <Show as Display>`: order only"),
        ("impl<T: AsRef<str> + Clone> Display for Show<T> {", "impl<T> Display for Show<T>\nwhere\n    T: AsRef<str>,\n    T: Clone,\n{", "bounds of `demo::impl <Show as Display>`: moved to the `where` clause"),
        // A trait method.
        ("fn snake<T: Clone + Debug>(&self, t: T) -> T;", "fn snake<T: Debug + Clone>(&self, t: T) -> T;", "bounds of `demo::Snake::snake`: order only"),
        ("fn snake<T: Clone + Debug>(&self, t: T) -> T;", "fn snake<T>(&self, t: T) -> T\n    where\n        T: Clone + Debug;", "bounds of `demo::Snake::snake`: moved to the `where` clause"),
        // The order of the predicates of a `where` clause, and a move out of the clause.
        ("        I: Iterator,\n        I::Item: Debug;", "        I::Item: Debug,\n        I: Iterator;", "bounds of `demo::Snake::each`: order only"),
        ("fn each<I>(&self, it: I) -> usize\n    where\n        I: Iterator,\n        I::Item: Debug;", "fn each<I: Iterator>(&self, it: I) -> usize\n    where\n        I::Item: Debug;", "bounds of `demo::Snake::each`: moved out of the `where` clause"),
        // A lifetime bound, `?Sized`, and a `for<'a>` binder stay a part of the bound.
        ("pub fn lives<'a, 'b: 'a, T: 'a + ?Sized>(x: &'a T, y: &'b u8) -> &'a T {", "pub fn lives<'a, 'b, T: ?Sized + 'a>(x: &'a T, y: &'b u8) -> &'a T\nwhere\n    'b: 'a,\n{", "bounds of `demo::lives`: moved to the `where` clause"),
        ("    F: for<'a> Fn(&'a u8) + Clone,", "    F: Clone + for<'a> Fn(&'a u8),", "bounds of `demo::calls`: order only"),
        // One of two bounds with the same tokens at the end goes out of the clause. The net of
        // the changed text must not make a second fact.
        ("pub fn walk<F, G>(s: &str, mut f: F, mut g: G, n: &mut fmt::Formatter) -> fmt::Result\nwhere\n    F: FnMut(&str, &mut fmt::Formatter) -> fmt::Result,\n", "pub fn walk<F: FnMut(&str, &mut fmt::Formatter) -> fmt::Result, G>(s: &str, mut f: F, mut g: G, n: &mut fmt::Formatter) -> fmt::Result\nwhere\n", "bounds of `demo::walk`: moved out of the `where` clause"),
        // The same predicate two times: the set is equal.
        ("pub fn limit<A: Clone + Debug, B: Debug>(a: A, b: B) -> A {", "pub fn limit<A: Clone + Debug, B: Debug + Debug>(a: A, b: B) -> A {", "bounds of `demo::limit`: a duplicate bound only"),
    ];
    for (old, new, text) in cases {
        assert!(BOUNDS.contains(old), "{old}");
        let changed = BOUNDS.replace(old, new);
        let report = run_derived(BOUNDS, &changed, &bounds_bodies());
        assert_eq!(descriptions(&report), [text], "{new}");
        assert_eq!(report.facts[0].compare, Compare::CodeEqual, "{new}: {:#?}", report.facts[0]);
        assert!(!report.facts[0].spotlight && report.facts[0].part == "bounds", "{new}: {:#?}", report.facts[0]);
        // The order and the place of a bound are not a part of the public API.
        let ((_d1, before), (_d2, after)) = (load(BOUNDS), load(&changed));
        let level = crate::api_level(&semilla_syntax::diff(&before, &after, &semilla_syntax::Renames::default()), &before, &after);
        assert_eq!(level, ("patch", Vec::new()), "{new}");
        // The same change of an item that no different crate can use.
        let (private, changed) = (BOUNDS.replace("pub ", ""), changed.replace("pub ", ""));
        let report = run_derived(&private, &changed, &bounds_bodies());
        assert_eq!(descriptions(&report), [text], "{new}");
        assert_eq!(report.facts[0].compare, Compare::CodeEqual, "{new}: {:#?}", report.facts[0]);
    }
}

/// Batch 6b, part 3: each change that is not only the order or the place of a bound stays a
/// fact to read, also with an equal MIR of each body.
#[test]
fn a_bound_change_with_a_different_set_or_a_different_rest_is_a_generics_fact() {
    // (the old text, the new text)
    let cases = [
        // The order of the generic parameters: the arguments go by position.
        ("pub fn limit<A: Clone + Debug, B: Debug>(a: A, b: B) -> A {", "pub fn limit<B: Debug, A: Clone + Debug>(a: A, b: B) -> A {"),
        // A default of a parameter.
        ("pub enum Pick<A: Clone + Debug, B = u8> {", "pub enum Pick<A: Debug + Clone, B = u16> {"),
        // An `impl Trait` argument and a parameter with a name.
        ("pub fn takes(x: impl Debug) {", "pub fn takes<T: Debug>(x: T) {"),
        // A bound on an associated type goes into the header of the trait, and out of it.
        ("pub trait Walk: Iterator\nwhere\n    Self::Item: Debug,\n{", "pub trait Walk: Iterator<Item: Debug> {"),
        ("pub trait Keep {\n    type It: Debug;", "pub trait Keep\nwhere\n    Self::It: Debug,\n{\n    type It;"),
        // The order of the supertraits, and a supertrait that goes to the `where` clause.
        ("pub trait Snake: ToOwned + Debug {", "pub trait Snake: Debug + ToOwned {"),
        ("pub trait Snake: ToOwned + Debug {", "pub trait Snake\nwhere\n    Self: ToOwned + Debug,\n{"),
        // A bound in `dyn A + B`, and a bound in a type of the signature.
        ("pub fn dynamic(x: &(dyn Debug + Send)) {", "pub fn dynamic(x: &(dyn Send + Debug)) {"),
        ("pub fn gives() -> impl Debug + Clone {", "pub fn gives() -> impl Clone + Debug {"),
        // A different order and one new bound. A removed bound.
        ("pub fn limit<A: Clone + Debug, B: Debug>(a: A, b: B) -> A {", "pub fn limit<A: Debug + Clone + Send, B: Debug>(a: A, b: B) -> A {"),
        ("pub fn limit<A: Clone + Debug, B: Debug>(a: A, b: B) -> A {", "pub fn limit<A: Debug, B: Debug>(a: A, b: B) -> A\nwhere\n    A: Debug,\n{"),
        // There is no name resolution: a different path is a different bound.
        ("pub fn limit<A: Clone + Debug, B: Debug>(a: A, b: B) -> A {", "pub fn limit<A: Clone + Debug, B: fmt::Debug>(a: A, b: B) -> A {"),
        // A `for<'a>` binder that goes from the bound to the bounded type.
        ("    F: for<'a> Fn(&'a u8) + Clone,", "    for<'a> F: Fn(&'a u8) + Clone,"),
        // A predicate with no bound, removed (`where T:` says that the type is well formed).
        ("pub fn takes(x: impl Debug) {", "pub fn takes(x: impl Debug)\nwhere\n    Vec<u8>:,\n{"),
        // A macro that can read the tokens: a `derive` that is not of the standard library,
        // and an attribute macro.
        ("pub struct Show<T: AsRef<str> + Clone>(pub T);", "#[derive(Custom)]\npub struct Show<T: Clone + AsRef<str>>(pub T);"),
        ("pub fn limit<A: Clone + Debug, B: Debug>(a: A, b: B) -> A {", "#[other::rewrite]\npub fn limit<A: Debug + Clone, B: Debug>(a: A, b: B) -> A {"),
    ];
    for (old, new) in cases {
        assert!(BOUNDS.contains(old), "{old}");
        let changed = BOUNDS.replace(old, new);
        let report = run_derived(BOUNDS, &changed, &bounds_bodies());
        assert!(!report.facts.is_empty() && !report.facts.iter().any(|fact| fact.part == "bounds"), "{new}: {:#?}", descriptions(&report));
        let ((_d1, before), (_d2, after)) = (load(BOUNDS), load(&changed));
        let level = crate::api_level(&semilla_syntax::diff(&before, &after, &semilla_syntax::Renames::default()), &before, &after);
        assert_eq!(level.0, "major", "{new}: {level:?}");
    }
    // The macro that reads the tokens is there on the two sides.
    let base = BOUNDS.replace("pub struct Show<", "#[derive(Clone, Custom)]\npub struct Show<");
    let report = run_derived(&base, &base.replace("<T: AsRef<str> + Clone>(pub T);", "<T: Clone + AsRef<str>>(pub T);"), &bounds_bodies());
    assert!(report.facts[0].part != "bounds" && report.facts[0].description.starts_with("generics of `demo::Show`"), "{:#?}", report.facts);
    // A `derive` of the standard library keeps the rule.
    let base = BOUNDS.replace("pub struct Show<", "#[derive(Clone, Debug)]\npub struct Show<");
    let report = run_derived(&base, &base.replace("<T: AsRef<str> + Clone>(pub T);", "<T: Clone + AsRef<str>>(pub T);"), &bounds_bodies());
    assert_eq!((descriptions(&report), report.facts[0].compare), (vec!["bounds of `demo::Show`: order only".to_string()], Compare::CodeEqual), "{:#?}", report.facts);
}

/// Batch 6b, part 3: a different order of the bounds and an edit of the body in one step. The
/// fact of the body stays a fact to read, and no fact of the step is neutral.
#[test]
fn a_bound_order_change_with_a_body_edit_keeps_the_body_fact() {
    let changed = BOUNDS.replace("pub fn limit<A: Clone + Debug, B: Debug>(a: A, b: B) -> A {\n    drop(b);\n    a\n}", "pub fn limit<A: Debug + Clone, B: Debug>(a: A, b: B) -> A {\n    drop(b);\n    a.clone()\n}");
    assert_ne!(changed, BOUNDS);
    let bodies: Vec<(&str, BodyStatus)> = bounds_bodies().into_iter().map(|(name, status)| if name == "demo::limit" { (name, different(false)) } else { (name, status) }).collect();
    let report = run_derived(BOUNDS, &changed, &bodies);
    assert!(descriptions(&report).iter().any(|text| text == "bounds of `demo::limit`: order only"), "{:#?}", descriptions(&report));
    assert!(report.facts.iter().any(|fact| fact.part != "bounds" && fact.affected.iter().any(|body| body == "demo::limit")), "{:#?}", report.facts);
    assert!(!report.facts.iter().any(|fact| fact.compare == Compare::CodeEqual), "{:#?}", report.facts);
}

/// Batch 6b, part 4: `no-panic` names an index site "index panic sites", not "explicit panic
/// sites". A step with the two kinds names each kind.
#[test]
fn no_panic_names_an_index_site_and_an_explicit_site_with_different_words() {
    let check = |body: &str| {
        let source = format!("pub fn pick(list: &[u8], at: usize, more: Option<u8>) -> u8 {{\n    {body}\n}}\n");
        let (_d1, old) = load(&source);
        let (_d2, new) = load(&source);
        let step = step(vec![claim("c1", "no-panic", Some("demo::pick"), serde_json::json!({}))]);
        match verify_step(&StepInput { step: &step, before: &old, after: &new }, &NoOracle, &NoOracle).report.claims[0].status.clone() {
            ClaimStatus::Refuted { reason } => reason,
            other => panic!("{other:?}"),
        }
    };
    let reason = check("list[at]");
    assert!(reason.starts_with("index panic sites (an index out of bounds): demo::pick/") && !reason.contains("explicit"), "{reason}");
    let reason = check("more.unwrap()");
    assert!(reason.starts_with("explicit panic sites: demo::pick/") && !reason.contains("index"), "{reason}");
    let reason = check("list[at] + more.unwrap()");
    assert!(reason.starts_with("explicit panic sites: demo::pick/") && reason.contains("; index panic sites (an index out of bounds): demo::pick/"), "{reason}");
}

/// Batch 6b, part 4: a new `derive` on a type that a different crate can use adds its impls to
/// the public API changes. A removed `derive` says that the public API can change.
#[test]
fn a_changed_derive_list_of_a_public_type_is_a_public_api_change() {
    let level = |old: &str, new: &str| {
        let ((_d1, before), (_d2, after)) = (load(old), load(new));
        let (level, lines) = crate::api_level(&semilla_syntax::diff(&before, &after, &semilla_syntax::Renames::default()), &before, &after);
        (level, lines)
    };
    let plain = "pub struct AsSnakeCase(pub u8);\n";
    let more = "#[derive(Clone, Copy, core::fmt::Debug)]\npub struct AsSnakeCase(pub u8);\n";
    assert_eq!(level(plain, more), ("minor", vec!["`AsSnakeCase` now has `Clone`, `Copy`, `Debug`".to_string()]));
    assert_eq!(level(more, plain), ("major", vec!["`AsSnakeCase` no longer has `Clone`, `Copy`, `Debug` (the public API can change)".to_string()]));
    let other = "#[derive(Clone, PartialEq)]\npub struct AsSnakeCase(pub u8);\n";
    assert_eq!(level(more, other), ("major", vec!["`AsSnakeCase` no longer has `Copy`, `Debug` (the public API can change)".to_string(), "`AsSnakeCase` now has `PartialEq`".to_string()]));
    // A type that no different crate can use, and an attribute that is not a `derive`.
    assert_eq!(level(&plain.replace("pub struct", "struct"), &more.replace("pub struct", "struct")), ("patch", Vec::new()));
    assert_eq!(level(more, &more.replace("#[derive", "#[repr(transparent)]\n#[derive")), ("patch", Vec::new()));
}

/// Design 36.1 (batch 7b, part 1): a moved type with an impl, a wider field, and other changes.
#[test]
fn a_moved_type_with_a_second_change_is_a_move_and_one_fact_for_each_change() {
    const ERROR: &str = "use std::fmt;\n#[derive(Debug)]\nstruct Quoted(char);\nimpl fmt::Display for Quoted {\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {\n        if self.0 == '\\0' {\n            f.write_str(\"nul\")\n        } else {\n            write!(f, \"{}\", self.0)\n        }\n    }\n}\npub fn show(ch: char) -> String {\n    Quoted(ch).to_string()\n}\n";
    const QUOTED: &str = "use std::fmt;\n#[derive(Debug)]\npub(crate) struct Quoted(pub(crate) char);\nimpl fmt::Display for Quoted {\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {\n        if self.0 == '\\0' {\n            f.write_str(\"nul\")\n        } else {\n            write!(f, \"{}\", self.0)\n        }\n    }\n}\n";
    const ERROR_AFTER: &str = "use crate::quoted::Quoted;\npub fn show(ch: char) -> String {\n    Quoted(ch).to_string()\n}\n";
    let files = |lib: &'static str, error: &'static str, quoted: Option<&'static str>| {
        let mut files = vec![("Cargo.toml", MANIFEST), ("src/lib.rs", lib), ("src/error.rs", error)];
        files.extend(quoted.map(|text| ("src/quoted.rs", text)));
        files
    };
    let before = files("mod error;\n", ERROR, None);
    let fmt = "demo::quoted::<Quoted as fmt::Display>::fmt";
    let bodies = [("demo::error::show", equal("n1")), (fmt, equal("n1"))];
    let report = run_derived_files(&before, &files("mod error;\nmod quoted;\n", ERROR_AFTER, Some(QUOTED)), &bodies);
    let moved = "moved `demo::error::Quoted` → `demo::quoted::Quoted`, with 1 impl and 2 `use` line(s)";
    let wider = ["visibility of `demo::quoted::Quoted`: `private` → `pub(crate)`", "visibility of `demo::quoted::Quoted.0`: `private` → `pub(crate)`"];
    assert_eq!(descriptions(&report), [moved, wider[0], wider[1]]);
    assert!(report.facts.iter().all(|fact| fact.compare == Compare::CodeEqual && !fact.mismatch()) && report.notes.is_empty(), "{report:#?}");
    assert!(report.facts[0].category == Category::Move && report.facts[2].category == Category::Visibility, "{report:#?}");
    // A body edit in the moved impl.
    let edited: &'static str = Box::leak(QUOTED.replace("'\\0'", "'0'").into_boxed_str());
    let report = run_derived_files(&before, &files("mod error;\nmod quoted;\n", ERROR_AFTER, Some(edited)), &[("demo::error::show", equal("n1")), (fmt, different(false))]);
    assert_eq!(descriptions(&report), [moved, wider[0], wider[1], "change condition `'\\0'` → `'0'` at `demo::quoted::<Quoted as fmt::Display>::fmt/S0/cond/rhs`"]);
    assert!(report.facts[0].compare == Compare::CodeEqual && report.facts[3].compare == Compare::CodeDifferent && report.facts[3].affected == [fmt], "{report:#?}");
    // A second field, and the edit of each use.
    let field: &'static str = Box::leak(QUOTED.replace("Quoted(pub(crate) char)", "Quoted(pub(crate) char, pub(crate) u8)").into_boxed_str());
    let error: &'static str = Box::leak(ERROR_AFTER.replace("Quoted(ch)", "Quoted(ch, 0)").into_boxed_str());
    let report = run_derived_files(&before, &files("mod error;\nmod quoted;\n", error, Some(field)), &[("demo::error::show", different(false)), (fmt, equal("n1"))]);
    let texts = descriptions(&report);
    assert_eq!(texts, [moved, "the tuple fields of `demo::quoted::Quoted` changed: `(char)` → `(char, u8)`", wider[0], wider[1], "changed expression `Quoted(ch)` → `Quoted(ch, 0)` at `demo::error::show/tail/recv`"]);
    assert!(report.facts[1].category == Category::Declaration && report.facts[1].compare == Compare::CodeDifferent && report.facts[0].compare == Compare::CodeEqual, "{report:#?}");
    // A different derive list.
    let derive: &'static str = Box::leak(QUOTED.replace("#[derive(Debug)]", "#[derive(Debug, Clone, Copy)]").into_boxed_str());
    let report = run_derived_files(&before, &files("mod error;\nmod quoted;\n", ERROR_AFTER, Some(derive)), &bodies);
    assert_eq!(descriptions(&report), [moved, wider[0], "attributes of `demo::quoted::Quoted`: derive added: Clone, Copy", wider[1]]);
    assert_eq!(report.facts[2].category, Category::Declaration, "{report:#?}");
}

/// Design 36.1: the cases with no derived pair.
#[test]
fn a_pair_that_is_not_one_item_shows_as_removed_and_added() {
    const BASE: &str = "pub fn run(a: u32, b: u32) -> bool {\n    caret(a, b) || tilde(a, b)\n}\nfn caret(a: u32, b: u32) -> bool {\n    if a == 0 {\n        return b == 0;\n    }\n    let low = a * 2;\n    let high = a * 3;\n    b >= low && b < high\n}\nfn tilde(a: u32, b: u32) -> bool {\n    if a > 100 {\n        return false;\n    }\n    let low = a + 1;\n    b > low\n}\n";
    // A second function with the old name: the old path stays, so the new item is "added".
    let after = format!("{}fn caret(a: u32, b: u32) -> bool {{\n    caret_op(a, b)\n}}\n", BASE.replace("fn caret(", "fn caret_op("));
    let report = run_derived(BASE, &after, &[("demo::run", equal("n0")), ("demo::caret", different(true)), ("demo::caret_op", different(false))]);
    let texts = descriptions(&report);
    assert!(!texts.iter().any(|text| text.starts_with("renamed ")) && texts.contains(&"added `demo::caret_op`".to_string()) && texts.contains(&"added statement `caret_op(a, b)` at `demo::caret/tail`".to_string()), "{texts:?}");
    assert!(report.notes.is_empty() && report.facts.iter().filter(|fact| fact.affected == ["demo::caret"]).all(|fact| fact.compare == Compare::CodeDifferent), "{report:#?}");
    // A new name and a new body: below the limit.
    let after = BASE.replace("tilde", "tilde_op").replace("    if a > 100 {\n        return false;\n    }\n    let low = a + 1;\n    b > low\n", "    a.checked_add(1).is_some_and(|low| b > low)\n");
    let report = run_derived(BASE, &after, &[("demo::run", equal("n1")), ("demo::tilde_op", different(true))]);
    assert_eq!(descriptions(&report), ["changed path `tilde` → `tilde_op` at `demo::run/tail/rhs/callee`", "removed `demo::tilde`", "added `demo::tilde_op`"]);
    assert!(report.facts[2].compare == Compare::CodeDifferent && report.notes.is_empty(), "{report:#?}");
    // Two functions swap their names.
    let after = BASE.replace("fn caret(", "fn swap(").replace("fn tilde(", "fn caret(").replace("fn swap(", "fn tilde(");
    let report = run_derived(BASE, &after, &[("demo::run", equal("n0")), ("demo::caret", different(true)), ("demo::tilde", different(true))]);
    let texts = descriptions(&report);
    assert!(!texts.iter().any(|text| text.starts_with("renamed ")) && texts.contains(&"change condition `a == 0` → `a > 100` at `demo::caret/S0/cond`".to_string()) && texts.contains(&"change condition `a > 100` → `a == 0` at `demo::tilde/S0/cond`".to_string()), "{texts:?}");
    assert!(report.facts.iter().all(|fact| fact.compare == Compare::CodeDifferent), "{report:#?}");
    // Two pairs with equal texts: the maps have the same score.
    let twins = "pub fn one(a: u32) -> u32 {\n    a + 1\n}\npub fn two(a: u32) -> u32 {\n    a + 1\n}\n";
    let after = twins.replace("fn one", "fn first").replace("fn two", "fn second");
    let report = run_derived(twins, &after, &[("demo::first", equal("n0")), ("demo::second", equal("n0"))]);
    assert_eq!(descriptions(&report), ["removed `demo::one` (the public API changes: a different crate can no longer use the item)", "removed `demo::two` (the public API changes: a different crate can no longer use the item)", "added `demo::first` (the public API changes: a different crate can now use the item)", "added `demo::second` (the public API changes: a different crate can now use the item)"]);
    assert_eq!(report.notes, ["`demo::one` → `demo::first`, `demo::two` → `demo::second` show(s) as removed and added: Semilla does not derive the rename or the move (more than one name map has the same similar score and the same number of other facts: (`demo::one` → `demo::first`, `demo::two` → `demo::second`) or (`demo::one` → `demo::second`, `demo::two` → `demo::first`))"]);
}

/// Design 36.1, rule 7: a local rename with a second edit in the same body.
#[test]
fn a_local_rename_with_a_second_edit_is_a_rename_and_one_fact_for_each_edit() {
    const BASE: &str = "pub struct Ver {\n    pub minor: u32,\n    pub patch: u32,\n}\npub fn caret(ver: &Ver, cmp: &Ver) -> bool {\n    let minor = cmp.minor;\n    if ver.minor > minor {\n        return true;\n    }\n    ver.patch > cmp.patch\n}\npub fn unit(text: &str) -> u32 {\n    match text {\n        \"us\" => 1,\n        _ => 0,\n    }\n}\n";
    // A local, with a second edit. `minor` is also the name of a field.
    let after = BASE.replace("let minor = cmp.minor;", "let min = cmp.minor;").replace("ver.minor > minor", "ver.minor >= min");
    let report = run_derived(BASE, &after, &[("demo::caret", different(false)), ("demo::unit", equal("n0"))]);
    assert_eq!(descriptions(&report), ["`demo::caret`: renamed local `minor` → `min`, 1 edit(s)", "change condition `ver.minor > minor` → `ver.minor >= min` at `demo::caret/S1/cond`"]);
    // The rename fact has no body: the changed body is a body of the edit fact.
    assert!(report.facts[0].category == Category::Rename && report.facts[0].compare == Compare::CodeEqual && report.facts[0].affected.is_empty(), "{report:#?}");
    assert!(report.facts[1].compare == Compare::CodeDifferent && report.facts[1].affected == ["demo::caret"], "{report:#?}");
    // A parameter, with a second edit.
    let after = BASE.replace("cmp", "other").replace("ver.patch > other.patch", "ver.patch >= other.patch");
    let report = run_derived(BASE, &after, &[("demo::caret", different(false)), ("demo::unit", equal("n0"))]);
    assert_eq!(descriptions(&report), ["parameter `cmp` of `demo::caret` renamed to `other`: `& Ver`", "`demo::caret`: renamed local `cmp` → `other`, 1 edit(s)", "changed expression `ver.patch > cmp.patch` → `ver.patch >= other.patch` at `demo::caret/tail`"]);
    assert!(report.facts[..2].iter().all(|fact| fact.category == Category::Rename && fact.compare == Compare::CodeEqual && fact.affected.is_empty()) && report.facts[2].affected == ["demo::caret"], "{report:#?}");
    // A changed string in a pattern is not a rename of a local.
    let after = BASE.replace("\"us\"", "\"ms\"");
    let report = run_derived(BASE, &after, &[("demo::caret", equal("n0")), ("demo::unit", different(false))]);
    assert_eq!(descriptions(&report), ["changed pattern `\"us\"` → `\"ms\"` at `demo::unit/S0/arm[0].pat`"]);
    assert_eq!(report.facts[0].compare, Compare::CodeDifferent, "{report:#?}");
}

/// Batch 7b, part 2 (design 36.2): each associated item of an impl has its own fact, with the
/// category of its change, the public API note, and a line in the list of the API changes.
#[test]
fn an_associated_item_has_its_own_fact_and_its_api_line() {
    const BASE: &str = "#[derive(Clone)]\npub struct Kind;\npub struct Op(pub u8);\nimpl Op {\n    pub const DEFAULT: Op = Op(1);\n    const HIDDEN: u8 = 1;\n    pub fn get(&self) -> u8 {\n        self.0\n    }\n}\npub trait Named {\n    const NAME: &'static str;\n    type Out;\n}\nimpl Named for Op {\n    const NAME: &'static str = \"op\";\n    type Out = u8;\n}\nunsafe impl Sync for Op {}\npub fn limit() -> usize {\n    const MAX: usize = 32;\n    MAX\n}\n";
    const API: &str = "(the public API can change: a different crate can use the item)";
    let bodies = [("demo::Op::get", equal("n0")), ("demo::limit", equal("n0"))];
    let run = |old: &str, new: &str| {
        assert!(BASE.contains(old), "{old}");
        run_derived(BASE, &BASE.replacen(old, new, 1), &bodies)
    };
    let one = |old: &str, new: &str| {
        let report = run(old, new);
        assert_eq!(report.facts.len(), 1, "{old}: {:#?}", report.facts);
        (report.facts[0].description.clone(), report.facts[0].category, report.api_level.clone().unwrap_or_default(), report.api_changes.clone())
    };
    // Rule 1 and rule 3: a new constant with an attribute and a visibility is one fact. No
    // general fact shows its tokens.
    let (text, category, level, api) = one("    const HIDDEN", "    #[doc(hidden)]\n    pub const ANY: Op = Op(0);\n    const HIDDEN");
    assert_eq!((text, category), (format!("added `demo::Op::ANY` {API}"), Category::Added));
    assert_eq!((level.as_str(), api), ("minor", vec!["added `demo::Op::ANY`".to_string()]));
    // A changed value, a removed constant: the level is `major`.
    let (text, category, level, api) = one("DEFAULT: Op = Op(1)", "DEFAULT: Op = Op(2)");
    assert_eq!((text, category), (format!("value of `demo::Op::DEFAULT` changed: Op(1) → Op(2) {API}"), Category::Semantics));
    assert_eq!((level.as_str(), api), ("major", vec!["changed `demo::Op::DEFAULT` (the type or the value)".to_string()]));
    let (text, category, level, api) = one("    pub const DEFAULT: Op = Op(1);\n", "");
    assert_eq!((text, category), (format!("removed `demo::Op::DEFAULT` {API}"), Category::Removed));
    assert_eq!((level.as_str(), api), ("major", vec!["removed `demo::Op::DEFAULT`".to_string()]));
    // A private constant of an inherent impl is not a part of the public API.
    let (text, category, level, api) = one("HIDDEN: u8 = 1", "HIDDEN: u8 = 2");
    assert_eq!((text.as_str(), category), ("value of `demo::Op::HIDDEN` changed: 1 → 2", Category::Semantics));
    assert_eq!((level.as_str(), api.len()), ("patch", 0));
    // A member of a trait impl has no visibility: the impl gives the answer.
    let (text, category, level, _) = one("= \"op\"", "= \"operator\"");
    assert_eq!((text, category, level.as_str()), (format!("value of `demo::<Op as Named>::NAME` changed: \"op\" → \"operator\" {API}"), Category::Semantics, "major"));
    let (text, category, level, _) = one("type Out = u8", "type Out = u16");
    assert_eq!((text, category, level.as_str()), (format!("associated type `demo::<Op as Named>::Out`: `type Out = u8;` → `type Out = u16;` {API}"), Category::Declaration, "major"));
    // New methods: one fact for each method, and no fact for the member list.
    let report = run("    pub fn get", "    pub fn a(&self) -> u8 {\n        1\n    }\n    pub fn b(&self) -> u8 {\n        2\n    }\n    pub fn get");
    assert_eq!(descriptions(&report), ["added `demo::Op::a` (the public API changes: a different crate can now use the item)", "added `demo::Op::b` (the public API changes: a different crate can now use the item)"]);
    assert_eq!((report.api_level.as_deref(), report.api_changes.len()), (Some("minor"), 2), "{report:#?}");
    // The fact of a new impl block lists its members.
    let (text, category, level, api) = one("pub struct Op(pub u8);", "impl Kind {\n    /// Each kind.\n    pub const ALL: [u8; 1] = [1];\n}\npub struct Op(pub u8);");
    assert_eq!((text.as_str(), category), ("added `demo::impl Kind` with: `const ALL` (the public API changes: a different crate can now use the item)", Category::Added));
    assert_eq!((level.as_str(), api), ("minor", vec!["added `demo::Kind::ALL`".to_string()]));
    // Rule 2: a constant in a function body follows the rule of a constant of a module.
    let (text, category, level, _) = one("MAX: usize = 32", "MAX: usize = 64");
    assert_eq!((text.as_str(), category, level.as_str()), ("value of `MAX` (in `demo::limit`) changed: 32 → 64", Category::Semantics, "patch"));
    // Rule 4: a new trait impl, a removed `Sync` impl, a removed `derive` name.
    let (text, _, level, api) = one("pub fn limit", "impl From<u8> for Op {\n    fn from(x: u8) -> Op {\n        Op(x)\n    }\n}\npub fn limit");
    assert_eq!(text, format!("trait impl `<Op as From<u8>>` added with: `fn from` {API}"));
    assert_eq!((level.as_str(), api), ("minor", vec!["`Op` now has `From<u8>`".to_string()]));
    let (text, category, level, api) = one("unsafe impl Sync for Op {}\n", "");
    assert!(text.starts_with("trait impl `<Op as Sync>` removed (the public API can change: ") && text.ends_with("; each type with a field of this type can change too)"), "{text}");
    assert_eq!((category, level.as_str(), api), (Category::Removed, "major", vec!["`Op` no longer has `Sync` (the public API can change): each type with a field of this type can change too".to_string()]));
    let (text, _, level, api) = one("#[derive(Clone)]\n", "");
    assert_eq!(text, format!("attributes of `demo::Kind`: derive removed: Clone {API}"));
    assert_eq!((level.as_str(), api), ("major", vec!["`Kind` no longer has `Clone` (the public API can change)".to_string()]));
}

/// The fact texts of a report, with the category.
fn fact_lines(report: &StepReport) -> Vec<String> {
    report.facts.iter().map(|fact| format!("{} [{}]", fact.description, fact.category.name())).collect()
}

// Batch 7c, part 1.1 (design 37.1).

#[test]
fn a_trait_method_and_an_inherent_method_with_one_path_do_not_pair() {
    let before = "pub trait Gen {\n    fn size(&self) -> usize;\n}\n";
    let after = "pub struct Gen {\n    size: usize,\n}\n\nimpl Gen {\n    pub fn size(&self) -> usize {\n        self.size\n    }\n}\n";
    let report = run_derived(before, after, &[("demo::Gen::size", BodyStatus::AfterOnly)]);
    let lines = fact_lines(&report);
    assert!(lines.iter().any(|line| line.starts_with("removed `demo::Gen` (a trait)") && line.ends_with("[removed]")), "{lines:#?}");
    assert!(lines.iter().any(|line| line.starts_with("added `demo::Gen` (a struct)") && line.ends_with("[added]")), "{lines:#?}");
    // No fact pairs the method of the trait with the method of the impl.
    assert!(!lines.iter().any(|line| line.contains("visibility of") || line.contains("changed source text") || line.contains("changed item")), "{lines:#?}");
    // The same rule with no type of the name: the methods are two entities.
    let after = "pub struct Other;\n\nimpl Gen {\n    pub fn size(&self) -> usize {\n        1\n    }\n}\n";
    let lines = fact_lines(&run_derived(before, after, &[("demo::Gen::size", BodyStatus::AfterOnly)]));
    assert!(lines.iter().any(|line| line.starts_with("added `demo::Gen::size` (a method of an impl)")), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("changed source text")), "{lines:#?}");
}

#[test]
fn a_body_on_one_side_only_is_a_fact_with_the_body_text() {
    let before = "pub trait Price {\n    fn label(&self) -> u32;\n    fn rate(&self) -> u32 {\n        7\n    }\n}\n";
    let after = "pub trait Price {\n    fn label(&self) -> u32 {\n        41 + 1\n    }\n    fn rate(&self) -> u32;\n}\n";
    let report = run(before, after, vec![], &[("demo::Price::label", BodyStatus::AfterOnly), ("demo::Price::rate", BodyStatus::BeforeOnly)], None);
    let lines = fact_lines(&report);
    assert!(lines.contains(&"body added to `demo::Price::label`: `{ 41 + 1 }` [added]".to_string()), "{lines:#?}");
    assert!(lines.contains(&"body removed from `demo::Price::rate`: `{ 7 }` [removed]".to_string()), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("changed source text")), "{lines:#?}");
    for fact in &report.facts {
        assert_eq!(fact.compare, Compare::CodeDifferent, "{fact:#?}");
    }
    // A different body gives a different text.
    let other = after.replace("41 + 1", "41 + 2");
    assert_ne!(fact_lines(&run(before, &other, vec![], &[("demo::Price::label", BodyStatus::AfterOnly), ("demo::Price::rate", BodyStatus::BeforeOnly)], None)), lines);
    // A long body has a cut mark, and the content has the full text.
    let long = after.replace("41 + 1", &vec!["41 + 1"; 30].join(" + "));
    let report = run(before, &long, vec![], &[("demo::Price::label", BodyStatus::AfterOnly), ("demo::Price::rate", BodyStatus::BeforeOnly)], None);
    let fact = report.facts.iter().find(|fact| fact.description.starts_with("body added")).unwrap();
    assert!(fact.description.contains('…'), "{}", fact.description);
    assert_eq!(fact.content.as_deref().map(|content| content.matches("41 + 1").count()), Some(30));
}

// Batch 7c, part 1.2 (design 37.2).

#[test]
fn the_impl_blocks_of_a_renamed_type_pair_with_a_changed_header() {
    let before = "pub struct Walk<'a> {\n    pub at: &'a [u8],\n}\n\nimpl<'a> Walk<'a> {\n    pub fn len(&self) -> usize {\n        self.at.len()\n    }\n}\n\nimpl<'a> Iterator for Walk<'a> {\n    type Item = u8;\n    fn next(&mut self) -> Option<u8> {\n        let (first, rest) = self.at.split_first()?;\n        self.at = rest;\n        Some(*first)\n    }\n}\n";
    let after = "pub struct WalkWith<'a, H> {\n    pub at: &'a [u8],\n    pub h: H,\n}\n\nimpl<'a, H: Copy> WalkWith<'a, H> {\n    pub fn len(&self) -> usize {\n        self.at.len()\n    }\n}\n\nimpl<'a, H: Copy> Iterator for WalkWith<'a, H> {\n    type Item = u8;\n    fn next(&mut self) -> Option<u8> {\n        let (first, rest) = self.at.split_first()?;\n        self.at = rest;\n        Some(*first)\n    }\n}\n";
    let rename = || vec![claim("c1", "rename", Some("demo::Walk"), serde_json::json!({"to": "WalkWith"}))];
    let bodies = [("demo::WalkWith::len", equal("n0")), ("<demo::WalkWith<'a, H> as core::iter::Iterator>::next", equal("n0"))];
    let report = run(before, after, rename(), &bodies, None);
    let lines = fact_lines(&report);
    // One declaration fact for each changed header: the old header and the new header.
    assert!(lines.iter().any(|line| line.starts_with("generics of `demo::impl <WalkWith as Iterator>`: `impl<'a> Iterator for Walk<'a> { }` → `impl<'a, H: Copy> Iterator for WalkWith<'a, H> { }`") && line.ends_with("[declaration]")), "{lines:#?}");
    assert!(lines.iter().any(|line| line.starts_with("generics of `demo::impl WalkWith`: `impl<'a> Walk<'a> { }` → `impl<'a, H: Copy> WalkWith<'a, H> { }`")), "{lines:#?}");
    // No impl shows as removed or added.
    assert!(!lines.iter().any(|line| line.contains("trait impl") || line.starts_with("removed") || line.starts_with("added `demo::impl")), "{lines:#?}");
    assert!(report.uncovered.iter().all(|open| !open.description.contains("impl")), "{report:#?}");

    // A needle in a member body is an edit of that member, with its text.
    let needle = after.replace("Some(*first)", "Some(*first ^ 1)");
    let report = run_derived_claims(before, &needle, rename(), &bodies);
    let lines = fact_lines(&report);
    assert!(lines.iter().any(|line| line.contains("`*first` → `*first ^ 1`") && line.contains("demo::<WalkWith as Iterator>::next")), "{lines:#?}");

    // Two new blocks fit the old block: no pair, and a note.
    let two = after.replace("impl<'a, H: Copy> Iterator for WalkWith<'a, H> {", "impl<'a> Iterator for WalkWith<'a, u16> {\n    type Item = u8;\n    fn next(&mut self) -> Option<u8> {\n        None\n    }\n}\n\nimpl<'a> Iterator for WalkWith<'a, u8> {");
    let report = run_derived_claims(before, &two, rename(), &bodies);
    let lines = fact_lines(&report);
    assert!(report.notes.iter().any(|note| note.starts_with("Semilla does not pair the impl blocks `impl <Walk as Iterator>` and `impl <WalkWith as Iterator>`")), "{report:#?}");
    assert!(lines.iter().any(|line| line.starts_with("trait impl `<Walk<'a> as Iterator>` removed")), "{lines:#?}");
    assert_eq!(lines.iter().filter(|line| line.starts_with("trait impl `<WalkWith<'a, u") && line.contains(" added")).count(), 2, "{lines:#?}");
}

#[test]
fn two_impl_blocks_that_differ_only_in_lifetimes_are_one_impl() {
    // Design 37.2 (addendum): the header loses its lifetime, and two match arms change places.
    let before = "pub struct Attrs;\npub struct Node {\n    pub kind: u8,\n}\npub trait Parse<A> {\n    fn parse(self, attrs: A) -> u32;\n}\npub trait Shape {}\n\nimpl<'a> Parse<Attrs> for &'a mut Node {\n    fn parse(self, _attrs: Attrs) -> u32 {\n        match self.kind {\n            0 => 10,\n            1 => 20,\n            _ => 30,\n        }\n    }\n}\n\nimpl<'a> dyn Shape + 'a {\n    pub fn area(&self) -> u32 {\n        1\n    }\n}\n";
    let header = before.replace("impl<'a> Parse<Attrs> for &'a mut Node {", "impl Parse<Attrs> for &mut Node {").replace("impl<'a> dyn Shape + 'a {", "impl dyn Shape {");
    let after = header.replace("            0 => 10,\n            1 => 20,\n", "            1 => 20,\n            0 => 10,\n").replace("        1\n    }\n}\n", "        2\n    }\n}\n");
    assert!(after != header && header != before);
    let bodies = [("demo::<&mutNode as Parse>::parse", different(false)), ("demo::dynShape::area", different(false))];
    let report = run_derived(before, &after, &bodies);
    let lines = fact_lines(&report);
    // One declaration fact for each changed header.
    let headers: Vec<&String> = lines.iter().filter(|line| line.ends_with("[declaration]")).collect();
    assert_eq!(headers.len(), 2, "{lines:#?}");
    assert!(headers[0].starts_with("generics of `demo::impl <&mutNode as Parse>`: `impl<'a> Parse<Attrs> for &'a mut Node { }` → `impl Parse<Attrs> for &mut Node { }`"), "{lines:#?}");
    assert!(headers[1].starts_with("generics of `demo::impl dynShape`: `impl<'a> dyn Shape + 'a { }` → `impl dyn Shape { }`"), "{lines:#?}");
    // Each edit in a member is a fact with its text.
    for text in [
        "changed pattern `0` → `1` at `demo::<&mutNode as Parse>::parse/S0/arm[0].pat` [semantics]",
        "changed literal `10` → `20` at `demo::<&mutNode as Parse>::parse/S0/arm[0].body` [semantics]",
        "changed pattern `1` → `0` at `demo::<&mutNode as Parse>::parse/S0/arm[1].pat` [semantics]",
        "changed literal `20` → `10` at `demo::<&mutNode as Parse>::parse/S0/arm[1].body` [semantics]",
        "changed literal `1` → `2` at `demo::dynShape::area/tail` [semantics]",
    ] {
        assert!(lines.contains(&text.to_string()), "{text}\n{lines:#?}");
    }
    // No impl shows as removed or added.
    assert_eq!(lines.len(), 7, "{lines:#?}");
    assert!(report.uncovered.is_empty(), "{report:#?}");

    // A reason key with the lifetime of the old header finds the item (design 37.2).
    let (_dir, new) = load(&after);
    assert_eq!(crate::impl_name_matches("demo::<&'a mut Node as Parse<Attrs>>::parse", &new), ["demo::<&mutNode as Parse>::parse"]);

    // Two blocks of the new tree fit the old block: no pair.
    let two = format!("{after}\nimpl Parse<Attrs> for &'static mut Node {{\n    fn parse(self, _attrs: Attrs) -> u32 {{\n        0\n    }}\n}}\n");
    let report = run_derived(before, &two, &bodies);
    let lines = fact_lines(&report);
    assert!(lines.iter().any(|line| line.starts_with("trait impl `<&'a mut Node as Parse<Attrs>>` removed with: `fn parse`")), "{lines:#?}");
    assert_eq!(lines.iter().filter(|line| line.starts_with("trait impl `<&") && line.contains(" added with: `fn parse`")).count(), 2, "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("demo::impl <&mutNode as Parse>")), "{lines:#?}");
}

/// Runs a step with claims, and derives the facts of the edits.
fn run_derived_claims(before: &str, after: &str, claims: Vec<Claim>, bodies: &[(&str, BodyStatus)]) -> StepReport {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(claims);
    let oracle = FakeBodies(bodies.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
    verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle, true).report
}

// Batch 7c, part 1.3 (design 37.3).

#[test]
fn a_module_file_under_a_condition_that_is_off_gets_the_usual_facts() {
    let root = "#[cfg_attr(unix, path = \"sys/unix.rs\")]\n#[cfg_attr(windows, path = \"sys/windows.rs\")]\nmod sys;\n#[cfg(windows)]\nmod win;\npub fn live() -> u32 {\n    1\n}\n";
    let unix = "pub fn open(a: u32) -> u32 {\n    a + 1\n}\n";
    let windows = "pub fn open(a: u32) -> u32 {\n    a + 2\n}\npub fn close(a: u32) -> u32 {\n    a\n}\n";
    let win = "pub fn handle(a: u32) -> u32 {\n    a * 4\n}\n";
    let before = [("Cargo.toml", MANIFEST), ("src/lib.rs", root), ("src/sys/unix.rs", unix), ("src/sys/windows.rs", windows), ("src/win.rs", win), ("src/free.rs", "pub fn free() {}\n")];
    let windows_new = windows.replace("a + 2", "a + 3").replace("pub fn close(a: u32) -> u32 {\n    a\n}\n", "pub fn shut(a: u32) -> u32 {\n    a\n}\n");
    let win_new = win.replace("a * 4", "a * 5");
    let after = [("Cargo.toml", MANIFEST), ("src/lib.rs", root), ("src/sys/unix.rs", unix), ("src/sys/windows.rs", windows_new.as_str()), ("src/win.rs", win_new.as_str()), ("src/free.rs", "pub fn free() { let _ = 1; }\n")];
    let oracle = UnitBodies { bodies: vec![("demo::live".into(), equal("n0")), ("demo::sys::open".into(), equal("n0"))], cfg: vec!["unix"], files: vec!["src/lib.rs", "src/sys/unix.rs"], known: true };
    let report = run_units(&before, &after, &oracle, &[]);
    let lines: Vec<String> = report.facts.iter().map(|fact| format!("{} [{} · {}]{}", fact.description, fact.category.name(), fact.compare.name(), if fact.spotlight { " spotlight" } else { "" })).collect();
    // Each file of the module is a twin module. An edit in a file that is off has its text.
    assert!(lines.contains(&"changed literal `2` → `3` at `demo::sys#1::open/tail/rhs` [semantics · no compare]".to_string()), "{lines:#?}");
    assert!(lines.contains(&"changed literal `4` → `5` at `demo::win::handle/tail/rhs` [semantics · no compare]".to_string()), "{lines:#?}");
    // Design 38.10: the derived rename in the file that is off has no compare.
    assert!(lines.contains(&"renamed `demo::sys#1::close` → `demo::sys#1::shut` [rename · no compare]".to_string()), "{lines:#?}");
    // One spotlight for each file, with the file as the key of the reason.
    for file in ["src/sys/windows.rs", "src/win.rs"] {
        let facts: Vec<&Fact> = report.facts.iter().filter(|fact| fact.spotlight && fact.part == file).collect();
        assert_eq!(facts.len(), 1, "{file}: {lines:#?}");
        assert!(facts[0].description.starts_with(&format!("{file}: each edit of this file is in code that no checked configuration compiles")), "{lines:#?}");
        assert!(facts[0].spotlight_why.as_deref().unwrap_or("").contains(&format!("no checked unit reads the file `{file}`")), "{:#?}", facts[0]);
    }
    assert_eq!(report.facts.iter().filter(|fact| fact.spotlight && !fact.description.starts_with("src/free.rs")).count(), 2, "{lines:#?}");
    assert!(report.spotlight.contains(&"inactive code: src/sys/windows.rs (no checked unit reads the file)".to_string()), "{:#?}", report.spotlight);
    // A file that no `mod` declaration names keeps the file fact.
    assert!(lines.iter().any(|line| line.starts_with("src/free.rs: changed [file")), "{lines:#?}");
    // The reason of the file accepts the spotlight.
    let report = run_units(&before, &after, &oracle, &[("src/sys/windows.rs", "the code for Windows"), ("src/win.rs", "the code for Windows"), ("src/free.rs:", "no module")]);
    assert!(report.facts.iter().filter(|fact| fact.spotlight).all(|fact| fact.reason.is_some()), "{report:#?}");

    // A file of the module that does not parse keeps the file fact and the spotlight.
    let broken = windows_new.replace("pub fn shut(a: u32)", "pub fn shut(a: u32");
    let mut bad = after;
    bad[3] = ("src/sys/windows.rs", broken.as_str());
    let report = run_units(&before, &bad, &oracle, &[]);
    let fact = report.facts.iter().find(|fact| fact.description.starts_with("src/sys/windows.rs: changed")).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!(fact.category, Category::File);
    assert!(fact.spotlight && fact.spotlight_why.as_deref().unwrap_or("").starts_with("Semilla cannot parse this file: "), "{fact:#?}");
    // No item of the file shows as removed.
    assert!(!report.facts.iter().any(|fact| fact.description.contains("demo::sys#1")), "{report:#?}");
}

// Batch 7c, part 3.3 (design 37.5).

#[test]
fn a_file_that_does_not_parse_does_not_stop_the_step() {
    let root = "mod a;\nmod b;\npub fn live() -> u32 {\n    1\n}\n";
    let a = "pub fn first() -> u32 {\n    1\n}\n";
    let b = "pub fn second() -> u32 {\n    2\n}\n";
    let before = [("Cargo.toml", MANIFEST), ("src/lib.rs", root), ("src/a.rs", a), ("src/b.rs", b)];
    let after = [("Cargo.toml", MANIFEST), ("src/lib.rs", root), ("src/a.rs", "pub fn first() -> u32 {\n    1 +\n}\n"), ("src/b.rs", "pub fn second() -> u32 {\n    3\n}\n")];
    let report = run_derived_files(&before, &after, &[("demo::b::second", different(false))]);
    let lines = fact_lines(&report);
    assert!(report.errors.is_empty(), "{report:#?}");
    let fact = report.facts.iter().find(|fact| fact.description == "src/a.rs: changed").unwrap_or_else(|| panic!("{lines:#?}"));
    assert_eq!(fact.category, Category::File);
    let why = fact.spotlight_why.as_deref().unwrap_or("");
    // The first error of the parser is at the end of `1 +` (design 39.4).
    assert!(fact.spotlight && why.starts_with("Semilla cannot parse this file: ") && why.contains("(line 2, column 8)") && why.ends_with("in the tree after the step"), "{fact:#?}");
    // The diff of the file is the content of the fact.
    assert!(fact.content.as_deref().is_some_and(|content| content.contains("1 +")), "{fact:#?}");
    // The other files have their usual facts, and no item of the file shows as removed.
    assert!(lines.iter().any(|line| line.starts_with("changed literal `2` → `3` at `demo::b::second/tail`")), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("demo::a::first")), "{lines:#?}");

    // The 2015 edition: a parameter of a trait method with no name. The package has no
    // edition, so the parser reads the file as code of the 2015 edition (design 39.1). The file
    // parses with its own text, and no note is necessary.
    let old_form = "pub trait Shrink {\n    fn shrink(&self, u8, bool) -> u32;\n    fn size(&self) -> u32 {\n        1\n    }\n}\n";
    let before = [("Cargo.toml", MANIFEST), ("src/lib.rs", root), ("src/a.rs", old_form), ("src/b.rs", b)];
    let changed = old_form.replace("        1\n", "        2\n");
    let after = [("Cargo.toml", MANIFEST), ("src/lib.rs", root), ("src/a.rs", changed.as_str()), ("src/b.rs", b)];
    let report = run_derived_files(&before, &after, &[("demo::a::Shrink::size", different(false))]);
    let lines = fact_lines(&report);
    assert!(lines.iter().any(|line| line.starts_with("changed literal `1` → `2` at `demo::a::Shrink::size/tail`")), "{lines:#?}");
    assert!(!report.notes.iter().any(|note| note.contains("2015 edition")), "{report:#?}");
    assert!(!lines.iter().any(|line| line.starts_with("src/a.rs")), "{lines:#?}");
    // A package of the 2018 edition: the same file does not parse.
    let manifest = format!("{MANIFEST}edition = \"2018\"\n");
    let before = [("Cargo.toml", manifest.as_str()), ("src/lib.rs", root), ("src/a.rs", old_form), ("src/b.rs", b)];
    let after = [("Cargo.toml", manifest.as_str()), ("src/lib.rs", root), ("src/a.rs", changed.as_str()), ("src/b.rs", b)];
    let report = run_derived_files(&before, &after, &[]);
    assert!(fact_lines(&report).iter().any(|line| line.starts_with("src/a.rs: changed")), "{report:#?}");
}

// Batch 7c, part 2 (design 37.4).

#[test]
fn a_rename_key_with_a_twin_number_gets_the_name_of_the_body() {
    // A module and a function have the name `encode`: the items of the module have a number.
    let module = "pub mod encode {\n    pub fn max_len(a: usize) -> usize {\n        a + 1\n    }\n    pub fn other() {}\n}\npub fn encode(a: usize) -> usize {\n    encode::max_len(a)\n}\n#[cfg(unix)]\npub fn twin() -> u8 {\n    1\n}\n#[cfg(windows)]\npub fn twin() -> u8 {\n    2\n}\n";
    let (_a, before) = load_files(&[("Cargo.toml", MANIFEST), ("src/lib.rs", module)]);
    let (_b, after) = load_files(&[("Cargo.toml", MANIFEST), ("src/lib.rs", &module.replace("max_len", "max_len_x"))]);
    assert!(before.items.contains_key("demo::encode#0::max_len"), "{:?}", before.items.keys().collect::<Vec<_>>());
    let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> { pairs.iter().map(|(old, new)| (old.to_string(), new.to_string())).collect() };
    let renames = map(&[("demo::encode#0::max_len", "demo::encode#0::max_len_x"), ("demo::plain", "demo::plain_x")]);
    assert_eq!(checks::body_renames(&renames, &before, &after), map(&[("demo::encode::max_len", "demo::encode::max_len_x"), ("demo::plain", "demo::plain_x")]));
    // Two twins have one body name (`demo::twin`): the key is not unique, and it stays as it is.
    let renames = map(&[("demo::twin#0", "demo::twin_x")]);
    assert_eq!(checks::body_renames(&renames, &before, &before), renames);
    // The module twin itself: the function `encode` has the same body name.
    let renames = map(&[("demo::encode#0", "demo::enc")]);
    assert_eq!(checks::body_renames(&renames, &before, &before), renames);
    // The function twin declares `max_len` in its body: the body name is not unique, so the
    // key stays as it is.
    let inner = module.replace("    encode::max_len(a)\n", "    fn max_len(a: usize) -> usize {\n        a\n    }\n    max_len(a)\n");
    let (_c, before) = load_files(&[("Cargo.toml", MANIFEST), ("src/lib.rs", &inner)]);
    let renames = map(&[("demo::encode#0::other", "demo::encode#0::other_x")]);
    assert_eq!(checks::body_renames(&renames, &before, &before), map(&[("demo::encode::other", "demo::encode::other_x")]));
    let renames = map(&[("demo::encode#0::max_len", "demo::encode#0::max_len_x")]);
    assert_eq!(checks::body_renames(&renames, &before, &before), renames);
    assert_eq!(checks::no_twin_numbers("a::b#0::c#12.d"), "a::b::c.d");
    assert_eq!(checks::no_twin_numbers("a::use#0x::b#"), "a::use#0x::b#");
}

/// Batch 7c, part 6.1 (design 37.20): the index of the items by file gives the answer of the
/// full search for each name and each line. The fixture has a nested function, two files with
/// an equal last name, two functions on one line, and a constant of a bench target.
#[test]
fn the_item_index_gives_the_answer_of_the_full_search() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bench]]\nname = \"hex\"\nharness = false\n"),
        ("src/lib.rs", "pub mod a;\npub mod b;\npub fn outer(x: u32) -> u32 {\n    fn inner(y: u32) -> u32 {\n        y + 1\n    }\n    inner(x)\n}\npub fn one() -> u32 { 1 } pub fn two() -> u32 { 2 }\npub const LIMIT: u32 = 3;\npub static NAME: &str = \"n\";\n"),
        ("src/a/mod.rs", "pub fn get() -> u32 {\n    1\n}\npub const LIMIT: u32 = 4;\n"),
        ("src/b/mod.rs", "pub fn get() -> u32 {\n    2\n}\npub struct S;\nimpl S {\n    pub fn get(&self) -> u32 {\n        3\n    }\n}\n"),
        ("benches/hex.rs", "const DATA: [u8; 2] = [1, 2];\nfn main() {\n    let _ = DATA;\n}\n"),
    ];
    let (_dir, workspace) = load_files(&files);
    let index = &workspace.body_items().expect("the items did not change").by_file;
    assert!(index.values().flatten().count() >= 10, "{index:?}");
    let mut names: Vec<String> = workspace.items.keys().cloned().collect();
    names.extend(["hex::DATA", "demo::outer::{closure#0}", "demo::LIMIT", "demo::none", "LIMIT", "main"].map(String::from));
    let mut spans: Vec<Option<(String, usize)>> = vec![None];
    for (file, text) in &files[1..] {
        for line in 0..text.lines().count() + 3 {
            spans.push(Some((file.to_string(), line)));
            spans.push(Some((format!("/abs/demo/{file}"), line)));
        }
    }
    spans.push(Some(("mod.rs".to_string(), 1)));
    spans.push(Some(("other.rs".to_string(), 1)));
    let mut found = 0;
    for name in &names {
        for span in &spans {
            let scan = crate::checks::find_scan(name, span.as_ref(), &workspace);
            assert_eq!(crate::checks::find_indexed(name, span.as_ref(), &workspace, index), scan, "{name} {span:?}");
            // The first answer comes from the index, and the second answer from the kept answers.
            for _ in 0..2 {
                assert_eq!(crate::checks::find_item_by(name, span.as_ref(), &workspace), if crate::is_test_harness_main(name) { None } else { scan.clone() }, "{name} {span:?}");
            }
            found += usize::from(scan.is_some());
        }
    }
    assert!(found > 100, "{found}");
    // A nested function is not an item. Its line gives the function around it.
    assert_eq!(crate::checks::find_item_by("x", Some(&("src/lib.rs".to_string(), 5)), &workspace).as_deref(), Some("demo::outer"));
    // A method is smaller than its `impl` block and has its own line range.
    assert_eq!(crate::checks::find_item_by("x", Some(&("src/b/mod.rs".to_string(), 7)), &workspace).as_deref(), Some("demo::b::S::get"));
    // The constant of the bench target: the name has no item, the span and the last name do.
    assert_eq!(crate::checks::find_item_by("hex::DATA", Some(&("benches/hex.rs".to_string(), 1)), &workspace), crate::checks::find_scan("hex::DATA", Some(&("benches/hex.rs".to_string(), 1)), &workspace));
    // A file name that fits two files: the first path of the smallest items.
    assert_eq!(crate::checks::find_item_by("x", Some(&("mod.rs".to_string(), 1)), &workspace), crate::checks::find_scan("x", Some(&("mod.rs".to_string(), 1)), &workspace));
    // A workspace with a different count of items does not use the old index.
    let mut changed = workspace.clone();
    let _ = changed.body_items();
    let item = changed.items.remove("demo::outer").expect("the outer function");
    assert!(changed.body_items().is_none());
    assert_eq!(crate::checks::find_item_by("x", Some(&("src/lib.rs".to_string(), 5)), &changed), None, "{}", item.path);
}

/// Batch 7c, part 6.1 (design 37.20): the rules of the build configuration read a list of an
/// item one time. An equal list of a different configuration adds nothing, so the facts of a
/// body that two configurations compile are those of one configuration.
#[test]
fn an_equal_list_of_a_body_is_read_one_time() {
    let mut seen = std::collections::HashMap::new();
    let list = |words: &[&str]| words.iter().map(|word| word.to_string()).collect::<Vec<String>>();
    assert!(crate::opaque::first_list(&mut seen, "demo::f", &list(&["cfg demo::A", "u8"])));
    assert!(!crate::opaque::first_list(&mut seen, "demo::f", &list(&["cfg demo::A", "u8"])));
    // A different list of the item, and the list of a different item, are new.
    assert!(crate::opaque::first_list(&mut seen, "demo::f", &list(&["u8"])));
    assert!(crate::opaque::first_list(&mut seen, "demo::g", &list(&["cfg demo::A", "u8"])));
    assert!(crate::opaque::first_list(&mut seen, "demo::f", &list(&[])));
    assert!(!crate::opaque::first_list(&mut seen, "demo::f", &list(&[])));
    assert!(!crate::opaque::first_list(&mut seen, "demo::f", &list(&["u8"])));
    // The rule itself: a second call of `note` and of `names` with the list changes no answer.
    let (_dir, workspace) = load("#[cfg(unix)]\npub struct A(u8);\n#[cfg(not(unix))]\npub struct A(u16);\npub fn f(a: A) -> A { a }\n");
    let state = |times: usize| {
        let opaque = crate::opaque::Opaque::new(&workspace, &workspace);
        let span = ("src/lib.rs".to_string(), 5);
        let names = [crate::ExpandedName { word: "A".into(), count: 1, line: 5, ..Default::default() }];
        for _ in 0..times {
            opaque.note(true, "demo::f", Some(&span), &list(&["touch demo::A"]));
            crate::OpaqueInputs::names(&opaque, true, "demo::f", Some(&span), &names);
        }
        let callee = crate::OpaqueInputs::callee(&opaque, true, "demo::f", Some(&span), &list(&["touch demo::A"]));
        let layout = crate::OpaqueInputs::layout(&opaque, true, "demo::f", Some(&span), &list(&["touch demo::A"]));
        (callee, layout)
    };
    assert_eq!(state(1), state(3));
}

// Batch 7d, part 1.1 (design 38.1): two facts fold only if their full texts are equal.
#[test]
fn a_change_after_the_cut_point_splits_the_fold() {
    let before = "pub fn f(x: u32) -> u32 {\n    x\n}\npub fn g(x: u32) -> u32 {\n    x\n}\n";
    let long = "if x == 1 { return x.wrapping_add(1).wrapping_add(2).wrapping_add(3).wrapping_add(4).wrapping_add(5); }";
    let after = before.replace("{\n    x\n}", &format!("{{\n    {long}\n    x\n}}"));
    let texts = descriptions(&run_derived(before, &after, &[]));
    assert!(texts.iter().any(|t| t.starts_with("2 site(s): added early exit `if x == 1") && t.contains('…')), "{texts:?}");
    // The needle is after the cut point of the text: the two facts stay apart.
    let needle = after.replacen("wrapping_add(5)", "wrapping_sub(5)", 1);
    let report = run_derived(before, &needle, &[]);
    let texts = descriptions(&report);
    assert!(texts.iter().all(|t| !t.contains("site(s)")), "{texts:?}");
    assert_eq!(texts.iter().filter(|t| t.starts_with("added early exit `if x == 1")).count(), 2, "{texts:?}");
    // The text output does not merge them either ("× 2").
    let text = views::text_report(std::slice::from_ref(&report));
    assert!(!text.contains("× 2"), "{text}");
}

// Batch 7d, part 1.1: the merge of the text output ("× N") compares the full texts.
#[test]
fn the_merge_of_the_text_output_compares_the_full_texts() {
    let fact = |id: &str, content: &str| semilla_core::Fact {
        id: id.into(),
        at: Some(Anchor::item(&format!("demo::{id}"))),
        part: "comment".into(),
        description: "changed comment `a…` → `b…`".into(),
        content: Some(content.into()),
        status: "derived".into(),
        category: Category::Docs,
        compare: Compare::NoCode,
        ..Default::default()
    };
    let mut report = run_derived("pub fn f() {}\n", "pub fn f() {}\n", &[]);
    report.facts = vec![fact("f1", "- a x\n+ b x"), fact("f2", "- a y\n+ b y"), fact("f3", "- a x\n+ b x")];
    let text = views::text_report(std::slice::from_ref(&report));
    assert!(text.contains("f1 derived: changed comment `a…` → `b…` [docs · no code] × 2 (also f3)"), "{text}");
    assert!(text.contains("  ◆ f2 derived: changed comment `a…` → `b…` [docs · no code]\n"), "{text}");
}

// Batch 7d, part 1.2 (design 38.2): the inner statements of a deleted statement and the added
// statements of the body.
#[test]
fn a_restructured_body_gives_the_inner_compare() {
    let with = "fn with(c: &core::cell::Cell<u32>, f: impl FnOnce(&core::cell::Cell<u32>)) {\n    f(c)\n}\n";
    let before = format!("pub fn run(c: &core::cell::Cell<u32>) {{\n    with(c, |c| {{\n        let k = c.get();\n        if k == 1 {{ c.set(k.wrapping_add(2).wrapping_mul(7)); }} else {{ c.set(k & 3); }}\n    }});\n}}\n{with}");
    let after = format!("pub fn run(c: &core::cell::Cell<u32>) {{\n    let k = c.get();\n    if k == 1 {{ c.set(k.wrapping_add(2).wrapping_mul(7)); }} else {{ c.set(k & 3); }}\n}}\n{with}");
    // Design 41: the statement mapper maps each inner statement to its new place. The deleted
    // statement says that each inner statement moved (design 38.2 with 41).
    let texts = descriptions(&run_derived(&before, &after, &[]));
    assert!(texts.contains(&"moved statement `let k = c.get();` from `demo::run/S0/args[1]/body/S0` to `demo::run/S0`".to_string()), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("moved statement `if k == 1") && t.ends_with("from `demo::run/S0/args[1]/body/S1` to `demo::run/S1`")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("deleted statement `with(c, |c|") && t.contains("(each inner statement is a moved statement of the body) at `demo::run/S0`")), "{texts:?}");
    assert_eq!(texts.len(), 3, "{texts:?}");
    // A needle in the moved statement keeps its own fact.
    let needle = after.replace("k & 3", "k | 3");
    let texts = descriptions(&run_derived(&before, &needle, &[]));
    assert!(texts.iter().any(|t| t.starts_with("moved statement `if k == 1")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("`k & 3` → `k | 3`")), "{texts:?}");
    // The reverse step: the statements go into a closure.
    let texts = descriptions(&run_derived(&needle, &before, &[]));
    assert!(texts.iter().any(|t| t.starts_with("added") && t.contains("with(c, |c|") && t.contains("(each inner statement is a moved statement of the body)")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("`k | 3` → `k & 3`")), "{texts:?}");
}

// Batch 7d, part 1.3 (design 38.3): an item that gets a twin keeps its key for the twin with
// the old text. No rename fact and no fact "can no longer use the old path" shows.
#[test]
fn a_new_cfg_twin_keeps_the_key_of_the_item() {
    let before = "pub struct Slab {\n    entries: Vec<u32>,\n}\n\nimpl Slab {\n    pub fn capacity(&self) -> usize {\n        self.entries.capacity()\n    }\n}\n";
    let after = before.replace("    pub fn capacity", "    #[cfg(feature = \"std\")]\n    pub fn capacity").replace("    }\n}\n", "    }\n\n    /// Return the capacity.\n    #[cfg(not(feature = \"std\"))]\n    pub fn capacity(&self) -> usize {\n        self.entries.capacity() + 0\n    }\n}\n");
    let twin = "`demo::Slab::capacity#1` (a twin of `demo::Slab::capacity` under `cfg(not(feature = \"std\"))`) with the body: `{ self.entries.capacity() + 0 }`";
    for (old, new, words) in [(before, after.as_str(), ["added", "added"]), (after.as_str(), before, ["removed", "removed"])] {
        let texts = descriptions(&run_derived(old, new, &[]));
        assert!(texts.iter().all(|t| !t.starts_with("renamed") && !t.contains("is now") && !t.starts_with("changed code")), "{texts:?}");
        assert!(texts.contains(&format!("attributes of `demo::Slab::capacity`: {} `#[cfg(feature = \"std\")]`", words[0])), "{texts:?}");
        assert!(texts.contains(&format!("{} {twin}", words[1])), "{texts:?}");
        assert_eq!(texts.len(), 2, "{texts:?}");
    }
}

// Batch 7d, part 4.3 (design 38.6): a new version of a dependency has no compare.
#[test]
fn a_dependency_version_has_no_compare() {
    let lib = ("src/lib.rs", "pub fn f() {}\n");
    let lock = |version: &str| format!("version = 4\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\ndependencies = [\"serde\"]\n\n[[package]]\nname = \"serde\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n");
    let before = [("Cargo.toml", MANIFEST_V0.to_string()), ("Cargo.lock", lock("1.0.1")), (lib.0, lib.1.to_string())];
    let after = [("Cargo.toml", MANIFEST_V0.replace("1.0.1", "1.0.2")), ("Cargo.lock", lock("1.0.2")), (lib.0, lib.1.to_string())];
    let as_refs = |files: &[(&'static str, String)]| files.iter().map(|(path, text)| (*path, text.clone())).collect::<Vec<_>>();
    let (b, a) = (as_refs(&before), as_refs(&after));
    let b: Vec<(&str, &str)> = b.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let a: Vec<(&str, &str)> = a.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let report = run_derived_files(&b, &a, &[("demo::f", equal("n0"))]);
    let manifest: Vec<&semilla_core::Fact> = report.facts.iter().filter(|fact| fact.category == Category::Manifest).collect();
    assert_eq!(manifest.len(), 2, "{:#?}", report.facts);
    for fact in manifest {
        assert_eq!(fact.compare, Compare::NoCompare, "{fact:#?}");
        assert_eq!(fact.note.as_deref(), Some("no compare: Semilla does not compare the code of a dependency"), "{fact:#?}");
    }
}

// Batch 7d, part 4.4 (design 38.7): a local macro with the name of a standard macro.
#[test]
fn a_local_macro_with_a_standard_name_says_what_it_replaces() {
    let before = "pub fn f(x: u32) -> bool {\n    matches!(x, 1)\n}\npub fn g(x: u32) -> bool {\n    std::matches!(x, 2)\n}\n";
    let after = format!("macro_rules! matches {{\n    ($e:expr, $p:pat) => {{ true }};\n}}\n{before}");
    let texts = descriptions(&run_derived(before, &after, &[]));
    assert!(texts.contains(&"added macro `matches` (it replaces the standard macro `matches!`; each call in its scope: `demo::f`)".to_string()), "{texts:?}");
    let texts = descriptions(&run_derived(&after, before, &[]));
    assert!(texts.contains(&"removed macro `matches` (the standard macro `matches!` is used again; each call in its scope: `demo::f`)".to_string()), "{texts:?}");
    // A different name: the old text.
    let other = after.replace("macro_rules! matches", "macro_rules! same");
    assert!(descriptions(&run_derived(before, &other, &[])).contains(&"added macro `same`".to_string()));
}

// Batch 7d, part 4.1 (design 38.5): no line of the text output has more than 500 characters.
#[test]
fn a_long_line_is_cut_and_says_so() {
    let list: Vec<String> = (0..80).map(|n| format!("demo::item_{n}")).collect();
    let line = format!("      note: this body uses {}, which Semilla did not check", list.join(", "));
    let cut = views::fit_line(&line);
    assert!(cut.chars().count() <= views::LINE_MAX, "{cut}");
    assert!(cut.starts_with("      note: this body uses demo::item_0, demo::item_1,") && cut.contains("items more; `semilla status --verbose` shows the full line)") && cut.ends_with("demo::item_79, which Semilla did not check"), "{cut}");
    let hidden: usize = cut.split(" (").nth(1).and_then(|rest| rest.split(' ').next()).and_then(|n| n.parse().ok()).unwrap();
    assert_eq!(hidden + cut.matches("demo::item_").count(), 80, "{cut}");
    let text = "x".repeat(900);
    let cut = views::fit_line(&text);
    assert!(cut.chars().count() <= views::LINE_MAX && cut.contains("(500 characters more; `semilla status --verbose` shows the full line)"), "{cut}");
    assert_eq!(views::fit_line("short"), "short");
}

// Batch 7d, part 4.2 (design 38.4): a spotlight line that is equal for a run of facts shows one
// time, after the last fact of the run.
#[test]
fn an_equal_spotlight_line_of_a_run_shows_one_time() {
    let why = "a build setting: the key can change the code of each body; Semilla compares the bodies of the checked configurations only, and a reviewer reads each such key";
    let fact = |n: usize| semilla_core::Fact {
        id: format!("f{n}"),
        file: Some("Cargo.toml".into()),
        part: format!("Cargo.toml:lints.clippy.lint_{n}"),
        description: format!("Cargo.toml: lints.clippy.lint_{n} added (\"warn\")"),
        status: "derived".into(),
        category: Category::Manifest,
        compare: Compare::NoCode,
        spotlight: true,
        spotlight_why: Some(why.into()),
        reason: Some("the lints of the project".into()),
        reason_key: Some("kind:build-setting".into()),
        reason_covers: 7,
        ..Default::default()
    };
    let mut report = run_derived("pub fn f() {}\n", "pub fn f() {}\n", &[]);
    report.facts = (1..=7).map(fact).collect();
    let text = views::text_report(std::slice::from_ref(&report));
    assert_eq!(text.matches(why).count(), 1, "{text}");
    let at = text.find("◆ f7 derived").unwrap();
    assert!(text[at..].contains("      spotlight of f1 to f7: a build setting"), "{text}");
    assert!(!text[..at].contains("spotlight"), "{text}");
    // A different reason breaks the run.
    report.facts[3].reason = Some("a different reason".into());
    let text = views::text_report(std::slice::from_ref(&report));
    assert!(text.contains("spotlight of f1 to f3: ") && text.contains("spotlight of f5 to f7: ") && text.contains("      spotlight: a build setting"), "{text}");
}

// Batch 7d, part 5.1 (design 38.10): a rename in a module with two files. A twin in a file that
// no checked unit reads gives no compiled body, so the key of the checked twin gets its name.
#[test]
fn a_rename_in_twin_module_files_maps_the_checked_twin() {
    let lib = "#[cfg(unix)]\n#[path = \"sys/unix.rs\"]\nmod sys;\n#[cfg(windows)]\n#[path = \"sys/windows.rs\"]\nmod sys;\npub fn page() -> usize {\n    sys::page_size()\n}\n";
    let files = |name: &str| vec![("Cargo.toml", MANIFEST.to_string()), ("src/lib.rs", lib.replace("page_size", name)), ("src/sys/unix.rs", format!("pub fn {name}() -> usize {{\n    4096\n}}\n")), ("src/sys/windows.rs", format!("pub fn {name}() -> usize {{\n    8192\n}}\n"))];
    let load = |name: &str| {
        let owned = files(name);
        let refs: Vec<(&str, &str)> = owned.iter().map(|(path, text)| (*path, text.as_str())).collect();
        load_files(&refs)
    };
    let ((_a, before), (_b, after)) = (load("page_size"), load("page_bytes"));
    assert!(before.items.contains_key("demo::sys#0::page_size") && before.items.contains_key("demo::sys#1::page_size"), "{:?}", before.items.keys().collect::<Vec<_>>());
    let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> { pairs.iter().map(|(old, new)| (old.to_string(), new.to_string())).collect() };
    let renames = map(&[("demo::sys#0::page_size", "demo::sys#0::page_bytes"), ("demo::sys#1::page_size", "demo::sys#1::page_bytes")]);
    // With no list of the read files, each twin can give the body: the keys stay.
    assert_eq!(checks::body_renames(&renames, &before, &after), renames);
    let read: BTreeSet<String> = ["src/lib.rs", "src/sys/unix.rs"].iter().map(|file| file.to_string()).collect();
    assert_eq!(checks::body_renames_read(&renames, &before, &after, [Some(&read), Some(&read)]), map(&[("demo::sys::page_size", "demo::sys::page_bytes"), ("demo::sys#1::page_size", "demo::sys#1::page_bytes")]));
}

// Batch 8a, part 3 (design 39.2): a fact text shows the display text, the source as written.
#[test]
fn a_fact_text_shows_the_source_text_of_a_changed_attribute() {
    let before = "#[doc(alias = \"a long alias for the function that is the same\", alias = \"version one\")]\npub fn f() -> u32 {\n    1\n}\n";
    let after = "#[doc(\n    alias = \"a long alias for the function that is the same\",\n    alias = \"version two\"\n)]\npub fn f() -> u32 {\n    1\n}\n";
    let report = run_derived(before, after, &[("demo::f", equal("n0"))]);
    let texts = descriptions(&report);
    // The two sides have a different format: the cut part of each side must show the change.
    assert!(texts.iter().any(|text| text.starts_with("attributes of `demo::f`: ") && text.contains("version one") && text.contains("version two")), "{texts:#?}");
    assert!(!texts.iter().any(|text| text.contains("# [")), "{texts:#?}");
    // A change of the format only gives no fact.
    let report = run_derived(before, &before.replace("alias = \"version one\"", "alias=\"version one\""), &[("demo::f", equal("n0"))]);
    assert!(report.facts.is_empty(), "{:#?}", report.facts);
}

// Batch 8a fix round, D2 (design 39.2): the compare text of a token tree does not depend on
// spaces or line breaks. A change of spaces only gives no fact; a change of one token gives one.
#[test]
fn a_change_of_spaces_in_a_token_tree_gives_no_fact() {
    let base = "#[cfg(feature=\"std\")]\n#[cfg_attr(feature=\"serde1\", derive(Clone, Debug))]\npub struct S;\n\nmacro_rules! pick {\n    ($a:expr=>$b:expr) => { $a+-$b };\n}\n\npub fn f(a: u32) -> u32 {\n    let v = vec![a=>1];\n    pick!(a=>1)\n}\n";
    let spaced = "#[cfg( feature = \"std\" )]\n#[cfg_attr(feature = \"serde1\", derive(Clone,\n    Debug))]\npub struct S;\n\nmacro_rules! pick {\n    ($a:expr => $b:expr) => {\n        $a + - $b\n    };\n}\n\npub fn f(a: u32) -> u32 {\n    let v = vec![a => 1];\n    pick!(a => 1)\n}\n";
    let bodies = [("demo::f", equal("n0"))];
    let report = run_derived(base, spaced, &bodies);
    assert!(report.facts.is_empty(), "{:#?}", report.facts);
    // One token changes in each kind: one fact each, and the fact text shows the change.
    let cases = [
        ("feature = \"std\" )", "feature = \"alloc\" )", "attributes of `demo::S`"),
        ("derive(Clone,", "derive(Copy,", "attributes of `demo::S`: derive added: Copy; derive removed: Clone"),
        ("$a + - $b", "$a - - $b", "changed macro `pick`"),
        ("pick!(a => 1)", "pick!(a => 2)", "replace macro input"),
        ("vec![a => 1]", "vec![a => 3]", "replace macro input `vec![a=>1]` → `vec![a => 3]`"),
    ];
    // An added attribute next to a spaced one shows only the new attribute.
    let added = spaced.replace("pub struct S;", "#[cfg_attr(doc_cfg, doc(cfg(feature = \"std\")))]\npub struct S;");
    let texts = descriptions(&run_derived(base, &added, &bodies));
    assert_eq!(texts, ["attributes of `demo::S`: added `#[cfg_attr(doc_cfg, doc(cfg(feature = \"std\")))]`"], "{texts:#?}");
    for (from, to, start) in cases {
        let report = run_derived(base, &spaced.replace(from, to), &bodies);
        let texts = descriptions(&report);
        assert_eq!(texts.len(), 1, "{from}: {texts:#?}");
        assert!(texts[0].starts_with(start), "{from}: {texts:#?}");
    }
}

// Batch 8a fix round, D1 (design 39.4): `rustc` reads a CRLF file with LF. A `\` before a CRLF
// continues a string. A change of the line ends only gives no fact and no parse error.
#[test]
fn a_crlf_file_with_a_string_continuation_parses_and_a_line_end_change_gives_no_fact() {
    let lf = "/// The text.\npub fn text() -> &'static str {\n    \"enums must have an explicit \\\n     repr\"\n}\n";
    let crlf = lf.replace('\n', "\r\n");
    let report = run_derived(lf, &crlf, &[("demo::text", equal("n0"))]);
    assert!(report.facts.is_empty(), "{:#?}", report.facts);
    let report = run_derived(&crlf, lf, &[("demo::text", equal("n0"))]);
    assert!(report.facts.is_empty(), "{:#?}", report.facts);
    // A change of the literal in the CRLF file is one value fact.
    let report = run_derived(&crlf, &crlf.replace("explicit", "exact"), &[("demo::text", different(false))]);
    let texts = descriptions(&report);
    assert!(texts.len() == 1 && texts[0].starts_with("changed literal"), "{texts:#?}");
}


// Batch 8b (design 40): the exact map from each edit to the compiled code.

/// A body oracle that returns fixed comparisons, with their MIR elements.
struct ElementBodies(Vec<BodyComparison>);

impl BodyOracle for ElementBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.clone())
    }
}

/// A MIR element with the byte range of the first `part` in `text`.
fn element_at(text: &str, part: &str) -> MirElement {
    let start = text.find(part).unwrap_or_else(|| panic!("no `{part}`"));
    MirElement { tokens: Some(("src/lib.rs".into(), start, start + part.len())), ..Default::default() }
}

/// Runs a step with derived facts and fixed comparisons.
fn run_elements(before: &str, after: &str, claims: Vec<Claim>, oracle: &ElementBodies) -> StepReport {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(claims);
    verify_step_with(&StepInput { step: &step, before: &old, after: &new }, oracle, &NoOracle, true).report
}

fn changed_body(name: &str, old: Vec<MirElement>, new: Vec<MirElement>) -> BodyComparison {
    BodyComparison { name: name.into(), span: None, status: different(false), config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), range: None, elements: Some(MirDiff { old, new, other: false, pairs: Vec::new(), no_symbols: true, own: Vec::new(), evaluated: false }) }
}

const COUNT_BEFORE: &str = "pub fn f(v: &Vec<u32>) -> usize {\n    let n = v.len();\n    n + 1\n}\n";

fn count_after() -> String {
    COUNT_BEFORE.replace("v.len()", "Vec::len(v)").replace("n + 1", "n + 2")
}

#[test]
fn an_idle_edit_of_a_binding_that_copies_a_place_keeps_the_doubt_rule() {
    // Fix round 1 of design 46: rule 3c moves `let s = t;` into its use, so the binding has no
    // element, and the use belongs to a different edit. The edit of the binding keeps the
    // doubt rule. The negative is the test below: an init with a call has its own element.
    let before = "pub fn f(a: u32, t: u32) -> u32 {\n    let s = a;\n    s + 1\n}\n";
    let after = before.replace("let s = a;", "let s = t;").replace("s + 1", "s + 2");
    let body = changed_body("demo::f", vec![element_at(before, "s + 1")], vec![element_at(&after, "s + 2")]);
    let report = run_elements(before, &after, Vec::new(), &ElementBodies(vec![body]));
    let compare = |text: &str| report.facts.iter().find(|fact| fact.description.contains(text)).map(|fact| fact.compare.clone()).unwrap_or_else(|| panic!("{text}: {report:#?}"));
    assert_eq!((compare("`a` → `t`"), compare("`1` → `2`")), (Compare::CodeDifferent, Compare::CodeDifferent));
    assert_eq!((report.mir_map.owners, report.mir_map.equal, report.mir_map.doubt), (1, 0, 1));
    assert!(super::owners::place_init("let s = t.0;") && super::owners::place_init("&mut t") && !super::owners::place_init("let n = v.len();") && !super::owners::place_init("a / 2"));
}

#[test]
fn each_edit_gets_the_compare_result_of_its_own_mir_elements() {
    // Design 40.3: the call in a new form owns no element that differs, and each element that
    // differs has an owner: `code equal`. The literal owns the changed addition.
    let after = count_after();
    let body = changed_body("demo::f", vec![element_at(COUNT_BEFORE, "n + 1")], vec![element_at(&after, "n + 2")]);
    let report = run_elements(COUNT_BEFORE, &after, Vec::new(), &ElementBodies(vec![body.clone()]));
    let fact = |text: &str| report.facts.iter().find(|fact| fact.description.contains(text)).unwrap_or_else(|| panic!("{text}: {report:#?}"));
    assert_eq!((fact("`v.len()` → `Vec::len(v)`").compare, fact("`1` → `2`").compare), (Compare::CodeEqual, Compare::CodeDifferent));
    assert!(fact("`v.len()` → `Vec::len(v)`").affected.is_empty() && fact("`1` → `2`").affected == ["demo::f"]);
    assert_eq!((report.mir_map.owners, report.mir_map.equal, report.mir_map.doubt, report.mir_map.errors), (1, 1, 0, 0));
    assert!(report.facts.iter().all(|fact| fact.category != Category::Indirect), "{report:#?}");

    // An element with no owner (its node is the whole body): each edit that owns nothing keeps
    // the doubt rule, and a fact names the node. The edits of the body give the cause, so the
    // fact has no spotlight.
    let mut doubt = body.clone();
    // The element is the `n` of the tail: no edit contains it, and it contains no edit.
    let at = after.find("    n + 2").unwrap() + 4;
    doubt.elements.as_mut().unwrap().new.push(MirElement { tokens: Some(("src/lib.rs".into(), at, at + 1)), ..Default::default() });
    let report = run_elements(COUNT_BEFORE, &after, Vec::new(), &ElementBodies(vec![doubt]));
    let fact = |text: &str| report.facts.iter().find(|fact| fact.description.contains(text)).unwrap_or_else(|| panic!("{text}: {report:#?}"));
    assert_eq!(fact("`v.len()` → `Vec::len(v)`").compare, Compare::CodeDifferent);
    assert_eq!(report.mir_map.doubt, 1);
    let node = fact("the compiled code of `demo::f` changes with no edit at its place: `n`");
    assert!(node.category == Category::Indirect && !node.spotlight && node.compare == Compare::CodeDifferent, "{node:#?}");
    assert_eq!(node.note.as_deref(), Some("lines 3"));

    // Fix round 2: a jump with no value (a `goto`, a `return`) at a place with no edit needs no
    // owner. Its target follows a change with its own elements.
    let mut jump = body.clone();
    jump.elements.as_mut().unwrap().new.push(MirElement { tokens: Some(("src/lib.rs".into(), at, at + 1)), glue: true, jump: true, ..Default::default() });
    let report = run_elements(COUNT_BEFORE, &after, Vec::new(), &ElementBodies(vec![jump]));
    assert!(report.facts.iter().all(|fact| fact.category != Category::Indirect), "{report:#?}");
    assert_eq!(report.facts.iter().find(|fact| fact.description.contains("`v.len()` → `Vec::len(v)`")).unwrap().compare, Compare::CodeEqual);

    // Design 40.3 (fix round 1, D2): an element whose node is a block that contains an edit
    // belongs to the edits in the block. It never gives "no edit at its place".
    let mut block = body.clone();
    block.elements.as_mut().unwrap().new.push(element_at(&after, "{\n    let n"));
    let report = run_elements(COUNT_BEFORE, &after, Vec::new(), &ElementBodies(vec![block]));
    assert!(report.facts.iter().all(|fact| fact.category != Category::Indirect), "{report:#?}");
    assert!(report.facts.iter().filter(|fact| fact.part == "edit").all(|fact| fact.compare == Compare::CodeDifferent), "{report:#?}");

    // No element data: the rule of design 38 for each edit.
    let mut unknown = body;
    unknown.elements = None;
    let report = run_elements(COUNT_BEFORE, &after, Vec::new(), &ElementBodies(vec![unknown]));
    assert!(report.facts.iter().filter(|fact| fact.part == "edit").all(|fact| fact.compare == Compare::CodeDifferent), "{report:#?}");
}

#[test]
fn a_body_with_no_edit_names_the_node_of_each_changed_element() {
    // Design 40.3, rule 6: an element with no owner in a body with no edit. The fact names the
    // node, and it needs a reason as before.
    let before = "pub fn f(a: u32) -> u32 {\n    a * 2\n}\npub fn g() -> u32 {\n    1\n}\n";
    let after = before.replace("    1\n", "    3\n");
    let body = changed_body("demo::f", vec![element_at(before, "a * 2")], vec![element_at(&after, "a * 2")]);
    let report = run_elements(before, &after, Vec::new(), &ElementBodies(vec![body, BodyComparison { name: "demo::g".into(), ..changed_body("demo::g", Vec::new(), Vec::new()) }]));
    let facts: Vec<&Fact> = report.facts.iter().filter(|fact| fact.category == Category::Indirect).collect();
    assert_eq!(facts.len(), 1, "{report:#?}");
    assert_eq!(facts[0].description, "the compiled code of `demo::f` changes with no edit at its place: `a * 2`");
    assert!(facts[0].spotlight && facts[0].affected == ["demo::f"], "{report:#?}");
}

#[test]
fn a_body_gets_one_fact_with_its_outermost_nodes() {
    // Fix round 1, D3: one fact for each body. A node inside a different node of the list
    // does not show; at most three texts show, then "and N more". The lines go to the note.
    // Since design 44, the fact names no possible cause: a path edit is a cause (a link), or not.
    let before = "pub fn f(a: u32, b: u32) -> u32 {\n    let x = a * 2;\n    let y = b * 3;\n    let z = x + y;\n    let w = z * 5;\n    g(w)\n}\npub fn g(a: u32) -> u32 {\n    a\n}\npub fn h(a: u32) -> u32 {\n    a\n}\n";
    let after = before.replace("    g(w)\n}\npub fn g", "    h(w)\n}\npub fn g");
    let new = |part: &str| element_at(&after, part);
    let at = after.find("a * 2").unwrap();
    let inner = MirElement { tokens: Some(("src/lib.rs".into(), at, at + 1)), ..Default::default() };
    let body = changed_body("demo::f", Vec::new(), vec![new("a * 2"), inner, new("b * 3"), new("x + y"), new("z * 5")]);
    let report = run_elements(before, &after, Vec::new(), &ElementBodies(vec![body]));
    let facts: Vec<&Fact> = report.facts.iter().filter(|fact| fact.category == Category::Indirect).collect();
    assert_eq!(facts.len(), 1, "{report:#?}");
    assert!(report.facts.iter().any(|fact| fact.description.contains("`g` → `h`")), "{report:#?}");
    assert_eq!(facts[0].description, "the compiled code of `demo::f` changes with no edit at its place: `a * 2`, `b * 3`, `x + y`, and 1 more");
    assert_eq!(facts[0].note.as_deref(), Some("lines 2, 3, 4, 5"));
}

#[test]
fn the_bodies_with_no_edit_of_a_crate_fold_above_three() {
    // Fix round 1, D3: up to three bodies with no edit of one crate get a fact each; more give
    // one fact for the crate. The fact lists each body, and it needs one reason.
    let before = "pub fn f1(a: u32) -> u32 {\n    a * 2\n}\npub fn f2(a: u32) -> u32 {\n    a * 2\n}\npub fn f3(a: u32) -> u32 {\n    a * 2\n}\npub fn f4(a: u32) -> u32 {\n    a * 2\n}\npub fn g() -> u32 {\n    1\n}\n";
    let after = before.replace("    1\n", "    3\n");
    let places: Vec<usize> = after.match_indices("a * 2").map(|(at, _)| at).collect();
    let bodies = |count: usize| -> Vec<BodyComparison> { (1..=count).map(|n| changed_body(&format!("demo::f{n}"), Vec::new(), vec![MirElement { tokens: Some(("src/lib.rs".into(), places[n - 1], places[n - 1] + 5)), ..Default::default() }])).collect() };
    let indirect = |report: &StepReport| -> Vec<String> { report.facts.iter().filter(|fact| fact.category == Category::Indirect).map(|fact| fact.description.clone()).collect() };
    let report = run_elements(before, &after, Vec::new(), &ElementBodies(bodies(3)));
    assert_eq!(indirect(&report).len(), 3, "{report:#?}");
    let report = run_elements(before, &after, Vec::new(), &ElementBodies(bodies(4)));
    assert_eq!(indirect(&report), ["the compiled code of 4 bodies of the crate `demo` changes with no edit of their text: `demo::f1`, `demo::f2`, `demo::f3`, and 1 more"], "{report:#?}");
    let fact = report.facts.iter().find(|fact| fact.category == Category::Indirect).unwrap();
    assert!(fact.spotlight && fact.affected.len() == 4 && fact.reason_place() == "demo", "{fact:#?}");
}

#[test]
fn a_range_maps_to_the_smallest_node_at_token_boundaries() {
    // Design 40.2: the node of a range, and a map error for a range in a token.
    let (_d1, old) = load(COUNT_BEFORE);
    let (_d2, new) = load(&count_after());
    let edits = semilla_syntax::diff(&old, &new, &Default::default());
    let mut map = mirmap::MirMap::new(&old, &new, &edits);
    let place = |text: &str, part: &str| element_at(text, part).tokens.unwrap();
    let node = map.node(1, &place(&count_after(), "Vec::len(v)")).unwrap();
    assert_eq!((node.text.as_str(), node.start, node.block_free), ("Vec::len(v)", (2, 12), true));
    // The range of two tokens inside a node gives the smallest node that contains both.
    assert_eq!(map.node(0, &place(COUNT_BEFORE, "n = v")).unwrap().text, "let n = v.len();");
    let (file, start, end) = place(COUNT_BEFORE, "len");
    assert!(map.node(0, &(file.clone(), start + 1, end)).is_none());
    assert_eq!((map.errors, map.examples.len()), (1, 1));
    // The last character of a token (the `end_point` of a span) is the token.
    assert_eq!(map.node(0, &(file, end - 1, end)).unwrap().text, "len");
    assert_eq!(map.errors, 1);
}

#[test]
fn a_body_range_finds_its_item_on_a_line_with_two_items() {
    // Design 40.2: the byte range replaces the search by lines.
    let src = "pub fn a() -> u32 { 1 } pub fn b() -> u32 { 2 }\n";
    let (_d, ws) = load(src);
    let start = src.find("pub fn b").unwrap();
    assert_eq!(checks::find_item_by_range("src/lib.rs", (start, src.len() - 1), &ws).as_deref(), Some("demo::b"));
    assert_eq!(checks::find_item_by_range("src/lib.rs", (0, 23), &ws).as_deref(), Some("demo::a"));
    assert_eq!(checks::find_item_by_range("src/other.rs", (0, 23), &ws), None);
}

#[test]
fn the_branch_mark_counts_the_branch_points_the_targets_and_the_exits() {
    // Design 40.4.
    let switch = |targets: usize| MirElement { targets: Some(targets), ..Default::default() };
    let exit = MirElement { exit: true, ..Default::default() };
    assert_eq!(mirmap::branch_mark(&[vec![switch(2)], vec![switch(3)]]).as_deref(), Some("the compiled code of this edit changes its branches (1 → 1 branch point(s), 2 → 3 targets)"));
    assert_eq!(mirmap::branch_mark(&[vec![], vec![switch(2), exit]]).as_deref(), Some("the compiled code of this edit changes its branches (0 → 1 branch point(s), 0 → 2 targets; 0 → 1 exit(s))"));
    assert_eq!(mirmap::branch_mark(&[vec![switch(2)], vec![switch(2)]]), None);
}

#[test]
fn each_form_of_a_body_edit_is_semantics_and_keeps_its_text() {
    // Design 42: the table of 40.5 gives the forms of `semantics`, one row for each form. The
    // status of the body (with a changed control flow graph or not) does not change a
    // category. The text keeps the form of the edit.
    let base = "pub fn f(a: u32, o: Option<u32>) -> u32 {\n    let b = a + 1;\n    if a > b {\n        return 0;\n    }\n    let Some(c) = o else { return 1; };\n    g(b);\n    panic!(\"no\");\n}\nfn g(_x: u32) {}\n";
    let rows = [
        ("a + 1", "a + 7", "changed literal `1` → `7`"),
        ("if a > b", "if a >= b", "changed operator `a > b` → `a >= b`"),
        ("        return 0;\n", "", "deleted statement `return 0`"),
        ("    let Some(c) = o else { return 1; };\n", "", "deleted statement `let Some(c) = o else { return 1; }"),
        ("    g(b);\n", "", "deleted statement `g(b)`"),
        ("    panic!(\"no\");\n", "", "deleted statement `panic!(\"no\")"),
        ("let b = a + 1;", "let b = a.wrapping_add(1);", "changed expression `a + 1` → `a.wrapping_add(1)`"),
    ];
    for (old, new, text) in rows {
        let after = base.replacen(old, new, 1);
        for cfg in [false, true] {
            let report = run_derived(base, &after, &[("demo::f", different(cfg))]);
            let lines = fact_lines(&report);
            let edit: Vec<&String> = lines.iter().filter(|line| line.contains("demo::f")).collect();
            assert!(!edit.is_empty() && edit.iter().all(|line| line.ends_with("[semantics]")), "{old} → {new} ({cfg}): {lines:#?}");
            assert!(edit.iter().any(|line| line.starts_with(text)), "{old} → {new} ({cfg}): {lines:#?}");
        }
    }
}

// Batch 8c (design 41): the statement mapper, the refactoring facts, and their compare results.

fn refactor_body(lines: &str) -> String {
    format!("pub fn f(a: u32, b: u32, v: &mut Vec<u32>) -> u32 {{\n{lines}}}\n")
}

#[test]
fn an_extracted_and_an_inlined_variable_are_one_move_fact() {
    // Design 41.5 and 41.7.
    let before = refactor_body("    if a * b + 1 > 10 {\n        return a * b + 1;\n    }\n    0\n");
    let after = refactor_body("    let area = a * b + 1;\n    if area > 10 {\n        return area;\n    }\n    0\n");
    for (old, new, text) in [(&before, &after, "extracted variable `area` = `a * b + 1` (2 uses) at `demo::f/S0`"), (&after, &before, "inlined variable `area` = `a * b + 1` (2 uses) at `demo::f/S0`")] {
        let report = run_derived(old, new, &[]);
        let facts: Vec<&Fact> = report.facts.iter().collect();
        assert_eq!(facts.len(), 1, "{:?}", descriptions(&report));
        assert_eq!((facts[0].description.as_str(), facts[0].category), (text, Category::Move));
    }
    // A rename of a local and a move: one fact each, the move does not fold into the rename.
    let before = refactor_body("    let total = a + b;\n    let m = a * 2;\n    total + m\n");
    let after = refactor_body("    let m = a * 2;\n    let sum = a + b;\n    sum + m\n");
    let report = run_derived(&before, &after, &[]);
    let texts = descriptions(&report);
    assert!(texts.contains(&"moved statement `let total = a + b;` from `demo::f/S0` to `demo::f/S1`".to_string()), "{texts:?}");
    assert!(texts.contains(&"`demo::f`: renamed local `total` → `sum`, 2 edit(s)".to_string()), "{texts:?}");
    assert_eq!(texts.len(), 2, "{texts:?}");
}

#[test]
fn a_move_owns_the_elements_of_its_region_and_gives_no_indirect_fact() {
    // Design 41.7: the swap renumbers the locals, so the alignment of the tail fails. The move
    // owns the elements of its statement and of its region.
    let before = refactor_body("    let x = a + 1;\n    let y = b * 2;\n    x + y\n");
    let after = refactor_body("    let y = b * 2;\n    let x = a + 1;\n    x + y\n");
    let body = changed_body("demo::f", vec![element_at(&before, "a + 1"), element_at(&before, "x + y")], vec![element_at(&after, "a + 1"), element_at(&after, "x + y")]);
    let report = run_elements(&before, &after, Vec::new(), &ElementBodies(vec![body]));
    assert_eq!(descriptions(&report), ["moved statement `let x = a + 1;` from `demo::f/S0` to `demo::f/S1`"]);
    let fact = &report.facts[0];
    // Fix round 2 (design 41.7, 42): a swap of two statements is `semantics`, with no mismatch mark.
    assert!(fact.category == Category::Semantics && fact.compare == Compare::CodeDifferent && !fact.mismatch(), "{fact:#?}");
}

#[test]
fn an_element_that_a_move_takes_by_its_region_never_makes_a_different_edit_code_equal() {
    // Design 41.7, rule 3 (the needle `n_swap_del`): the alignment pairs the deleted `v.push(y)`
    // with the new `v.push(x)`, so the old `v.push(x)` differs. The move takes it by its region,
    // and the deleted statement keeps the doubt rule.
    let before = refactor_body("    let x = a + 1;\n    let y = b * 2;\n    v.push(x);\n    v.push(y);\n    x + y\n");
    let after = refactor_body("    let y = b * 2;\n    let x = a + 1;\n    v.push(x);\n    x + y\n");
    let body = changed_body("demo::f", vec![element_at(&before, "v.push(x)")], Vec::new());
    let report = run_elements(&before, &after, Vec::new(), &ElementBodies(vec![body]));
    let deleted = report.facts.iter().find(|fact| fact.description.starts_with("deleted statement `v.push(y)`")).unwrap();
    assert_ne!(deleted.compare, Compare::CodeEqual, "{:#?}", report.facts);
    assert!(!report.facts.iter().any(|fact| fact.category == Category::Indirect), "{:#?}", report.facts);
}

#[test]
fn a_change_inside_a_moved_statement_owns_its_elements() {
    // Design 41.6 and 41.7, rule 1: the element of `a + 2` goes to the move and to the literal.
    let before = refactor_body("    let p = a + 1;\n    let q = b + 2;\n    p + q\n");
    let after = refactor_body("    let q = b + 2;\n    let p = a + 2;\n    p + q\n");
    let body = changed_body("demo::f", vec![element_at(&before, "a + 1")], vec![element_at(&after, "a + 2")]);
    let report = run_elements(&before, &after, Vec::new(), &ElementBodies(vec![body]));
    let literal = report.facts.iter().find(|fact| fact.description.starts_with("changed literal `1` → `2`")).unwrap();
    assert!(literal.compare == Compare::CodeDifferent && literal.category == Category::Semantics, "{literal:#?}");
    let moved = report.facts.iter().find(|fact| fact.description.starts_with("moved statement `let p = a + 1;`")).unwrap();
    assert_eq!(moved.compare, Compare::CodeDifferent, "{moved:#?}");
}

#[test]
fn the_map_check_counts_the_pairs_that_do_not_agree_with_the_mir_alignment() {
    // Design 41.8: the old `a + 1` (the moved statement) aligns with the new `b * 2` (a
    // different statement): one map disagreement. The facts do not change.
    let before = refactor_body("    let x = a + 1;\n    let y = b * 2;\n    x + y\n");
    let after = refactor_body("    let y = b * 2;\n    let x = a + 1;\n    x + y\n");
    let place = |text: &str, part: &str| element_at(text, part).tokens.unwrap();
    let mut body = changed_body("demo::f", vec![element_at(&before, "x + y")], vec![element_at(&after, "x + y")]);
    if let Some(diff) = body.elements.as_mut() {
        diff.pairs = vec![(place(&before, "a + 1"), place(&after, "b * 2"))];
    }
    let report = run_elements(&before, &after, Vec::new(), &ElementBodies(vec![body]));
    assert_eq!((report.mir_map.checked, report.mir_map.disagree), (1, 1), "{:#?}", report.mir_map);
    assert!(report.mir_map.line().contains("statement map: 1 pair(s) checked, 1 do not agree with the MIR alignment, 0 with no aligned element (first: `demo::f/S0` → `demo::f/S1` (moved))"), "{}", report.mir_map.line());
    assert_eq!(descriptions(&report), ["moved statement `let x = a + 1;` from `demo::f/S0` to `demo::f/S1`"]);
}

#[test]
fn an_edit_inside_a_moved_statement_names_its_new_place() {
    // Fix round 1, D1 (design 41.6): the reviewer reads the new code, so a change inside a
    // moved statement names the new path. Also inside a moved composite.
    let before = "pub fn jaro(a: u32, mut transpositions: u32) -> u32 {\n    let m = a + 1;\n    transpositions /= 2;\n    m + transpositions\n}\n";
    let after = "pub fn jaro(a: u32, mut transpositions: u32) -> u32 {\n    let m = a + 1;\n    if a == 0 {\n        transpositions = 0;\n    } else {\n        transpositions /= 3;\n    }\n    m + transpositions\n}\n";
    let texts = descriptions(&run_derived(before, after, &[]));
    assert!(texts.contains(&"changed literal `2` → `3` at `demo::jaro/S1/else/S0/rhs`".to_string()), "{texts:?}");
    assert!(texts.contains(&"moved statement `transpositions /= 2` from `demo::jaro/S1` to `demo::jaro/S1/else/S0`".to_string()), "{texts:?}");
    let before = refactor_body("    let mut s = a;\n    if a > 1 {\n        s += 2;\n        s *= b;\n    }\n    s += b;\n    v.push(s);\n    s\n");
    let after = refactor_body("    let mut s = a;\n    s += b;\n    v.push(s);\n    if a > 1 {\n        s += 3;\n        s *= b;\n    }\n    s\n");
    let texts = descriptions(&run_derived(&before, &after, &[]));
    assert!(texts.iter().any(|t| t.starts_with("changed literal `2` → `3` at `demo::f/S3/then/S0/rhs`")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("moved statement `if a > 1")), "{texts:?}");
    // Fix round 2 (design 41.7, 42): a moved `if` and a moved assignment are `semantics`.
    let report = run_derived(&before, &after, &[]);
    let moved = report.facts.iter().find(|fact| fact.description.starts_with("moved statement `if a > 1")).unwrap();
    assert_eq!(moved.category, Category::Semantics, "{moved:#?}");
}

#[test]
fn a_swap_of_two_statements_is_semantics_with_no_mismatch_mark() {
    // Fix round 2 (design 41.7, 42): a move changes the order of execution. The fact keeps its
    // text, and it has the category `semantics`, so the mismatch mark does not show.
    let before = refactor_body("    let x = a + 1;\n    let y = b * 2;\n    x + y\n");
    let after = refactor_body("    let y = b * 2;\n    let x = a + 1;\n    x + y\n");
    let body = changed_body("demo::f", vec![element_at(&before, "a + 1")], vec![element_at(&after, "a + 1")]);
    let report = run_elements(&before, &after, Vec::new(), &ElementBodies(vec![body]));
    assert_eq!(descriptions(&report), ["moved statement `let x = a + 1;` from `demo::f/S0` to `demo::f/S1`"]);
    let fact = &report.facts[0];
    assert!(fact.category == Category::Semantics && fact.compare == Compare::CodeDifferent && !fact.mismatch(), "{fact:#?}");
}

#[test]
fn a_fact_with_changed_branches_does_not_fold_with_a_fact_whose_branches_stay() {
    // Design 42: the three old categories are one, so the "branches:" line is a part of the
    // fold key. The line of each fact stays.
    let branches = "the compiled code of this edit changes its branches (1 → 1 branch point(s), 2 → 3 targets)";
    let fact = |item: &str, branch: bool| Fact {
        at: Some(format!("{item}/S0/cond").parse().unwrap()),
        part: "edit".into(),
        description: format!("changed operator `a > b` → `a >= b` at `{item}/S0/cond`"),
        status: "derived".into(),
        category: Category::Semantics,
        compare: Compare::CodeDifferent,
        branches: branch.then(|| branches.to_string()),
        ..Default::default()
    };
    let mut facts = vec![fact("demo::f", false), fact("demo::g", true), fact("demo::h", false)];
    let mut fact_of = vec![Some(0), Some(1), Some(2)];
    edits::fold(&mut facts, &mut fact_of);
    let texts: Vec<(&str, Option<&str>)> = facts.iter().map(|fact| (fact.description.as_str(), fact.branches.as_deref())).collect();
    assert_eq!(texts, [("2 site(s): changed operator `a > b` → `a >= b`", None), ("changed operator `a > b` → `a >= b` at `demo::g/S0/cond`", Some(branches))], "{texts:#?}");
    assert_eq!(fact_of, [Some(0), Some(1), Some(0)]);
}

// Batch 8f (design 44): the symbol links.

/// A body oracle with fixed comparisons and the definitions of the two sides.
struct LinkBodies(Vec<BodyComparison>, [DefMap; 2]);

impl BodyOracle for LinkBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.clone())
    }

    fn defs(&self, _: &Step) -> anyhow::Result<[DefMap; 2]> {
        Ok(self.1.clone())
    }
}

/// A symbol whose definition is the first `part` of `text`.
fn sym_at(text: &str, path: &str, part: &str) -> Sym {
    let start = text.find(part).unwrap_or_else(|| panic!("no `{part}`"));
    Sym { path: path.into(), place: Some(("src/lib.rs".into(), start, start + part.len())) }
}

/// A MIR element at the first `part` of `text` that names the symbols.
fn named_at(text: &str, part: &str, symbols: Vec<Sym>) -> MirElement {
    MirElement { symbols, ..element_at(text, part) }
}

/// A changed body with symbol data.
fn linked_body(name: &str, old: Vec<MirElement>, new: Vec<MirElement>) -> BodyComparison {
    let mut body = changed_body(name, old, new);
    body.elements.as_mut().unwrap().no_symbols = false;
    body
}

fn run_links(before: &str, after: &str, oracle: &LinkBodies) -> StepReport {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let step = step(Vec::new());
    verify_step_with(&StepInput { step: &step, before: &old, after: &new }, oracle, &NoOracle, true).report
}

const LIMIT_BEFORE: &str = "pub const LIMIT: u32 = 10;\n\npub fn a(x: u32) -> u32 {\n    x + LIMIT\n}\n\npub fn b(x: u32, y: u32) -> u32 {\n    let s = x + LIMIT;\n    s * y\n}\n";

#[test]
fn an_element_that_names_a_changed_constant_is_linked_to_the_edit_of_the_constant() {
    // Design 44, part 3: `a` has no edit; its element names `LIMIT`. The link owns it: the
    // value fact is `code different` with `a` in its affected bodies, and no `indirect` fact.
    // The needle: `b` has its own change of behavior (`*` → `+`); its edit keeps its own
    // element and its own result. The element of `b` that names `LIMIT` goes to the constant.
    let after = LIMIT_BEFORE.replace("= 10;", "= 12;").replace("s * y", "s + y");
    let limit = |text: &str| sym_at(text, "demo::LIMIT", "pub const LIMIT: u32 = 1");
    let a = linked_body("demo::a", vec![named_at(LIMIT_BEFORE, "x + LIMIT", vec![limit(LIMIT_BEFORE)])], vec![named_at(&after, "x + LIMIT", vec![limit(&after)])]);
    let b_use = |text: &str| named_at(text, "x + LIMIT;", vec![limit(text)]);
    let b = linked_body("demo::b", vec![b_use(LIMIT_BEFORE), element_at(LIMIT_BEFORE, "s * y")], vec![b_use(&after), element_at(&after, "s + y")]);
    let report = run_links(LIMIT_BEFORE, &after, &LinkBodies(vec![a, b], Default::default()));
    let fact = |text: &str| report.facts.iter().find(|fact| fact.description.contains(text)).unwrap_or_else(|| panic!("{text}: {report:#?}"));
    let value = fact("value of `demo::LIMIT` changed");
    assert_eq!((value.compare, value.affected.clone()), (Compare::CodeDifferent, vec!["demo::a".to_string(), "demo::b".to_string()]), "{report:#?}");
    assert_eq!(value.through.get("demo::LIMIT").cloned(), Some(vec!["demo::a".to_string(), "demo::b".to_string()]));
    let operator = fact("`s * y` → `s + y`");
    assert_eq!((operator.compare, operator.affected.clone()), (Compare::CodeDifferent, vec!["demo::b".to_string()]));
    assert!(report.facts.iter().all(|fact| fact.category != Category::Indirect), "{report:#?}");
    assert_eq!((report.mir_map.linked, report.mir_map.doubt), (4, 0));

    // With no symbol data (a driver before 8f), the element has no link: the body of `a` keeps
    // the rules of the evidence.
    let a = changed_body("demo::a", vec![element_at(LIMIT_BEFORE, "x + LIMIT")], vec![element_at(&after, "x + LIMIT")]);
    let report = run_links(LIMIT_BEFORE, &after, &LinkBodies(vec![a], Default::default()));
    assert_eq!(report.mir_map.linked, 0);
    assert!(report.facts.iter().any(|fact| fact.description.contains("value of `demo::LIMIT` changed") && fact.affected == ["demo::a"]), "{report:#?}");
}

#[test]
fn a_definition_that_names_a_changed_definition_changes_through_it() {
    // Design 44, part 2.4: `A = B + 1` with a changed `B`. The element of `f` names `A`; the
    // record of `A` names `B` (a value: rustc evaluates it). The cause is the edit of `B`.
    let before = "pub const B: u32 = 4;\npub const A: u32 = B + 1;\n\npub fn f(x: u32) -> u32 {\n    x * A\n}\n";
    let after = before.replace("= 4;", "= 5;");
    let defs = |text: &str| DefMap::from([("demo::A".to_string(), Def { place: sym_at(text, "demo::A", "pub const A").place, named: vec![(sym_at(text, "demo::B", "pub const B: u32 = "), links::EVALUATED)] })]);
    let f = |text: &str| named_at(text, "x * A", vec![sym_at(text, "demo::A", "pub const A: u32 = B + 1;")]);
    let body = linked_body("demo::f", vec![f(before)], vec![f(&after)]);
    let report = run_links(before, &after, &LinkBodies(vec![body.clone()], [defs(before), defs(&after)]));
    let value = report.facts.iter().find(|fact| fact.description.contains("value of `demo::B` changed")).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!((value.compare, value.affected.clone()), (Compare::CodeDifferent, vec!["demo::f".to_string()]), "{report:#?}");
    assert!(report.facts.iter().all(|fact| fact.category != Category::Indirect), "{report:#?}");
    // With no record of the definitions, the chain stops: the body has an element with no cause.
    let report = run_links(before, &after, &LinkBodies(vec![body], Default::default()));
    assert!(report.facts.iter().any(|fact| fact.category == Category::Indirect && fact.spotlight), "{report:#?}");
}

#[test]
fn a_function_body_edit_is_not_a_change_of_the_function_for_its_callers() {
    // Design 44, part 2.3: at N0 a caller does not change with the body of its callee. An
    // element that differs and names the callee has no link from a body edit of the callee.
    let before = "pub fn helper(x: u32) -> u32 {\n    x + 1\n}\n\npub fn one() -> u32 {\n    helper(1)\n}\n";
    let after = before.replace("x + 1", "x + 2");
    let call = |text: &str| named_at(text, "helper(1)", vec![sym_at(text, "demo::helper", "pub fn helper")]);
    let body = linked_body("demo::one", vec![call(before)], vec![call(&after)]);
    let report = run_links(before, &after, &LinkBodies(vec![body], Default::default()));
    assert_eq!(report.mir_map.linked, 0, "{report:#?}");
    assert!(report.facts.iter().any(|fact| fact.category == Category::Indirect && fact.description.contains("demo::one")), "{report:#?}");
}

#[test]
fn a_link_never_gives_code_equal_to_an_edit_of_a_type_or_a_pattern_in_the_body() {
    // Design 44, part 3.4 (the safety rule): `conv` returns `u16`, and the `let` of `f` gets
    // the type `u16`. The element of the call names `conv`: the link takes it. The type edit of
    // the `let` owns no element. Each element has an owner, but the type of the `let` can change
    // the code at a different place: the edit keeps the doubt rule, never `code equal`.
    let before = "pub fn conv(a: u8) -> u32 {\n    a.into()\n}\n\npub fn f(a: u8) -> u64 {\n    let x: u32 = conv(a);\n    x as u64\n}\n";
    let after = before.replace("-> u32 {", "-> u16 {").replace("let x: u32", "let x: u16");
    let call = |text: &str| named_at(text, "conv(a)", vec![sym_at(text, "demo::conv", "pub fn conv")]);
    let body = linked_body("demo::f", vec![call(before)], vec![call(&after)]);
    let report = run_links(before, &after, &LinkBodies(vec![body], Default::default()));
    let annotation = report.facts.iter().find(|fact| fact.at.as_ref().is_some_and(|at| at.item == "demo::f")).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!(annotation.compare, Compare::CodeDifferent, "{report:#?}");
    assert_eq!(report.mir_map.doubt, 1, "{report:#?}");
    let ret = report.facts.iter().find(|fact| fact.description.contains("return type of `demo::conv`")).unwrap_or_else(|| panic!("{report:#?}"));
    assert!(ret.affected.contains(&"demo::f".to_string()), "{report:#?}");
}

#[test]
fn the_signature_line_of_a_body_with_no_edit_follows_a_changed_alias() {
    // Design 44: `dbl` names the alias `Num` in its signature (the record of `dbl`). Its
    // element and its signature line (`other`) name the function itself: the link goes through
    // the record to the alias edit.
    let before = "pub type Num = u32;\n\npub fn dbl(a: Num) -> Num {\n    a * 2\n}\n";
    let after = before.replace("= u32;", "= u64;");
    let own = |text: &str| sym_at(text, "demo::dbl", "pub fn dbl");
    let defs = |text: &str| DefMap::from([("demo::dbl".to_string(), Def { place: own(text).place, named: vec![(sym_at(text, "demo::Num", "pub type Num"), 0)] })]);
    let mut body = linked_body("demo::dbl", vec![named_at(before, "a * 2", vec![own(before)])], vec![named_at(&after, "a * 2", vec![own(&after)])]);
    let diff = body.elements.as_mut().unwrap();
    diff.other = true;
    diff.own = vec![(0, own(before)), (1, own(&after))];
    let report = run_links(before, &after, &LinkBodies(vec![body], [defs(before), defs(&after)]));
    let alias = report.facts.iter().find(|fact| fact.description.contains("`demo::Num`")).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!((alias.compare, alias.affected.clone()), (Compare::CodeDifferent, vec!["demo::dbl".to_string()]), "{report:#?}");
    assert!(report.facts.iter().all(|fact| fact.category != Category::Indirect), "{report:#?}");
}

#[test]
fn the_mir_map_parses_a_file_with_the_edition_of_its_package() {
    // Design 44, FIX 1: in a 2015 file, `async` is a name. The 2021 parse makes it a keyword,
    // and the node of `async + 5` is a different node.
    let text = "pub fn g(async: u32) -> u32 {\n    async + 5\n}\n";
    let start = text.find("async + 5").unwrap();
    let node = |edition: semilla_syntax::rs::Edition| {
        let mut ws = Workspace::default();
        ws.rust_files.insert("src/lib.rs".into(), text.into());
        ws.editions.insert("src/lib.rs".into(), edition);
        let mut map = mirmap::MirMap::new(&ws, &ws, &[]);
        map.node(1, &("src/lib.rs".to_string(), start, start + "async + 5".len())).map(|node| node.text)
    };
    assert_eq!(node(semilla_syntax::rs::Edition::Edition2015).as_deref(), Some("async + 5"));
    assert_ne!(node(semilla_syntax::rs::Edition::Edition2015), node(semilla_syntax::rs::Edition::Edition2021));
}

#[test]
fn the_claims_of_an_extracted_or_an_inlined_variable_or_function_are_move() {
    // Design 44, FIX 4: one form gets one category. The derived facts of these forms are
    // `move` (design 41.7), so the claim ops are `move` too.
    for name in ["extract-variable", "inline-variable", "extract-function", "inline-function"] {
        assert_eq!(semilla_core::ops::op(name).and_then(|op| op.category), Some(Category::Move), "{name}");
    }
}

// Batch 8f fix round 1 (design 44).

/// The chain `c1` … `cN` of `const fn` calls: `c1` calls `c2`, …; the last one returns `x + 1`.
fn const_fn_chain(count: usize) -> String {
    let mut text = String::new();
    for index in 1..count {
        text.push_str(&format!("pub const fn c{index}(x: u32) -> u32 {{\n    c{}(x) * 2\n}}\n\n", index + 1));
    }
    text.push_str(&format!("pub const fn c{count}(x: u32) -> u32 {{\n    x + 1\n}}\n\npub const A: u32 = c1(3);\n\npub fn f(y: u32) -> u32 {{\n    y + A\n}}\n"));
    text
}

/// The definition records of a chain: `A` evaluates `c1`, and `cN` calls `cN+1` in its body.
fn chain_defs(text: &str, count: usize) -> DefMap {
    let mut defs = DefMap::from([("demo::A".to_string(), Def { place: sym_at(text, "demo::A", "pub const A").place, named: vec![(sym_at(text, "demo::c1", "pub const fn c1("), links::EVALUATED)] })]);
    for index in 1..count {
        let next = format!("pub const fn c{}(", index + 1);
        defs.insert(format!("demo::c{index}"), Def { place: sym_at(text, &format!("demo::c{index}"), &format!("pub const fn c{index}(")).place, named: vec![(sym_at(text, &format!("demo::c{}", index + 1), &next), links::BODY)] });
    }
    defs
}

#[test]
fn a_constant_follows_each_const_fn_of_its_value_to_the_edit_of_a_body() {
    // Fix round 1, D1: `A = c1(3)`; `c1` calls `c2` in its body; the step changes only the body
    // of `c2`. The users of `A` and the body of `A` itself (rustc evaluates it) link to the edit.
    let before = const_fn_chain(2);
    let after = before.replace("x + 1", "x + 2");
    let user = |text: &str| named_at(text, "y + A", vec![sym_at(text, "demo::A", "pub const A")]);
    let f = linked_body("demo::f", vec![user(&before)], vec![user(&after)]);
    let call = |text: &str| named_at(text, "c1(3)", vec![sym_at(text, "demo::c1", "pub const fn c1(")]);
    let mut a = linked_body("demo::A", vec![call(&before)], vec![call(&after)]);
    a.elements.as_mut().unwrap().evaluated = true;
    let report = run_links(&before, &after, &LinkBodies(vec![f, a], [chain_defs(&before, 2), chain_defs(&after, 2)]));
    let literal = report.facts.iter().find(|fact| fact.description.contains("`1` → `2`")).unwrap_or_else(|| panic!("{report:#?}"));
    assert_eq!((literal.compare, literal.affected.clone()), (Compare::CodeDifferent, vec!["demo::A".to_string(), "demo::f".to_string()]), "{report:#?}");
    assert!(report.facts.iter().all(|fact| fact.category != Category::Indirect), "{report:#?}");
}

#[test]
fn a_chain_of_const_fn_longer_than_the_limit_keeps_the_indirect_fact() {
    // Fix round 1, D1: the chain from a user of `A` has at most `MAX_DEPTH` steps. An edit of
    // `c{MAX_DEPTH}` is in reach; an edit of `c{MAX_DEPTH + 1}` is not: the fact stays.
    let count = links::MAX_DEPTH + 1;
    let base = const_fn_chain(count);
    let user = |text: &str| named_at(text, "y + A", vec![sym_at(text, "demo::A", "pub const A")]);
    for (edited, reached) in [(links::MAX_DEPTH, true), (links::MAX_DEPTH + 1, false)] {
        let mark = format!("pub const fn c{edited}(x: u32) -> u32 {{\n    ");
        let at = base.find(&mark).unwrap() + mark.len();
        let end = at + base[at..].find('\n').unwrap();
        // The new body: `x + 2` for the last function, `cN+1(x) * 3` for each other one.
        let new_body = if edited == count { "x + 2".to_string() } else { format!("c{}(x) * 3", edited + 1) };
        let after = format!("{}{new_body}{}", &base[..at], &base[end..]);
        let body = linked_body("demo::f", vec![user(&base)], vec![user(&after)]);
        let report = run_links(&base, &after, &LinkBodies(vec![body], [chain_defs(&base, count), chain_defs(&after, count)]));
        let indirect = report.facts.iter().any(|fact| fact.category == Category::Indirect);
        assert_eq!(indirect, !reached, "an edit of c{edited}: {report:#?}");
    }
}

#[test]
fn a_pair_diff_fact_gets_the_result_of_its_own_edit() {
    // Design 44, FIX 3: each edit of a pair diff has the result of its fact under the name map.
    // With no fact of its own, the result of the whole new item.
    let mut report = StepReport::default();
    report.facts.push(Fact { compare: Compare::CodeEqual, ..Default::default() });
    report.facts.push(Fact { compare: Compare::CodeDifferent, affected: vec!["demo::g".into()], ..Default::default() });
    report.facts.push(Fact { compare: Compare::Pending, ..Default::default() });
    let fact_of = [Some(0), Some(1), None, Some(2)];
    assert_eq!(identity::pair_diff_result(&report, &fact_of, 0, true, "demo::g"), (false, Vec::new()));
    assert_eq!(identity::pair_diff_result(&report, &fact_of, 1, true, "demo::g"), (true, vec!["demo::g".to_string()]));
    assert_eq!(identity::pair_diff_result(&report, &fact_of, 2, true, "demo::g"), (true, vec!["demo::g".to_string()]));
    assert_eq!(identity::pair_diff_result(&report, &fact_of, 3, false, "demo::g"), (false, Vec::new()));
}

// Batch 8g (design 45): the dependence in a body.

/// The source of a declaration at the first `part` of `text`, on a side.
fn declared_at(side: usize, text: &str, part: &str) -> MirSource {
    let start = text.find(part).unwrap_or_else(|| panic!("no `{part}`"));
    MirSource::Declaration(side, ("src/lib.rs".into(), start, start + part.len()))
}

/// The source of a changed element at the first `part` of `text`, on a side.
fn source_at(side: usize, text: &str, part: &str) -> MirSource {
    let start = text.find(part).unwrap_or_else(|| panic!("no `{part}`"));
    MirSource::Element(side, Some(("src/lib.rs".into(), start, start + part.len())), None)
}

const LET_BEFORE: &str = "pub fn f(a: u8) -> u64 {\n    let x: u32 = a.into();\n    let p = x as u64 + 1;\n    p * 2\n}\n";

/// The elements of `f` on one side: the call `a.into()` and the cast `x as u64` depend on `x`
/// (its type annotation and its writer, the call); `p * N` has no sources unless `needle`.
fn let_elements(side: usize, text: &str, ty: &str, needle: bool) -> Vec<MirElement> {
    let sources = vec![MirGroup { follows: false, sources: vec![declared_at(side, text, ty), source_at(side, text, "a.into()")] }];
    let mut tail = element_at(text, "p * ");
    if needle {
        tail.sources = sources.clone();
    }
    vec![MirElement { sources: sources.clone(), ..element_at(text, "a.into()") }, MirElement { sources, ..element_at(text, "x as u64") }, tail]
}

#[test]
fn a_changed_type_of_a_local_owns_the_elements_that_depend_on_it() {
    // Design 45: `let x: u32` → `let x: u16`. The call and the cast have no edit at their place;
    // they depend on `x`, whose declaration has the type edit. The type edit owns them: `code
    // different`, no `indirect` fact, no doubt. The needle: the changed literal of `p * 2` keeps
    // its own element and result, also when its element has sources (a dependence never
    // removes a cause of rule 1).
    let after = LET_BEFORE.replace("x: u32", "x: u16").replace("p * 2", "p * 3");
    for needle in [false, true] {
        let body = changed_body("demo::f", let_elements(0, LET_BEFORE, "u32", needle), let_elements(1, &after, "u16", needle));
        let report = run_links(LET_BEFORE, &after, &LinkBodies(vec![body], Default::default()));
        let fact = |text: &str| report.facts.iter().find(|fact| fact.description.contains(text)).unwrap_or_else(|| panic!("{text}: {report:#?}"));
        let pattern = fact("`x: u32` → `x: u16`");
        assert_eq!((pattern.category, pattern.compare, pattern.affected.clone()), (Category::Semantics, Compare::CodeDifferent, vec!["demo::f".to_string()]), "{report:#?}");
        let literal = fact("`2` → `3`");
        assert_eq!((literal.compare, literal.affected.clone()), (Compare::CodeDifferent, vec!["demo::f".to_string()]), "{report:#?}");
        assert!(report.facts.iter().all(|fact| fact.category != Category::Indirect), "{report:#?}");
        assert_eq!((report.mir_map.doubt, report.mir_map.causes.get("dependence").copied(), report.mir_map.causes.get("rule 1b").copied()), (0, Some(4), Some(2)), "{report:#?}");
    }
    // With no sources (a record before 8g), the elements have no cause: the doubt rule.
    let strip = |list: Vec<MirElement>| list.into_iter().map(|element| MirElement { sources: Vec::new(), ..element }).collect();
    let body = changed_body("demo::f", strip(let_elements(0, LET_BEFORE, "u32", false)), strip(let_elements(1, &after, "u16", false)));
    let report = run_links(LET_BEFORE, &after, &LinkBodies(vec![body], Default::default()));
    assert!(report.facts.iter().any(|fact| fact.category == Category::Indirect), "{report:#?}");
}

#[test]
fn a_dependence_takes_the_causes_of_a_chain_to_a_fixed_point() {
    // Design 45, part 2.5: the cast depends on the call, and the call depends on the declaration
    // of `x`. The sources come in an order where the cast comes first: the fixed point still
    // gives the cast the type edit. Each group needs a cause: an element whose second changed
    // local has no cause stays with no cause, also when its first local has one.
    let after = LET_BEFORE.replace("x: u32", "x: u16");
    let group = |sources: Vec<MirSource>| MirGroup { follows: false, sources };
    let elements = |side: usize, text: &str, ty: &str| vec![MirElement { sources: vec![group(vec![source_at(side, text, "a.into()")])], ..element_at(text, "x as u64") }, MirElement { sources: vec![group(vec![declared_at(side, text, ty)])], ..element_at(text, "a.into()") }];
    let body = changed_body("demo::f", elements(0, LET_BEFORE, "u32"), elements(1, &after, "u16"));
    let report = run_links(LET_BEFORE, &after, &LinkBodies(vec![body], Default::default()));
    assert!(report.facts.iter().all(|fact| fact.category != Category::Indirect) && report.mir_map.causes.get("dependence") == Some(&4), "{report:#?}");
    // Two groups: the declaration of `x` has a cause, the second local has none.
    let lone = |side: usize, text: &str, ty: &str| vec![MirElement { sources: vec![group(vec![declared_at(side, text, ty)]), group(vec![source_at(side, text, "p * 2")])], ..element_at(text, "x as u64") }, element_at(text, "p * 2")];
    let body = changed_body("demo::f", lone(0, LET_BEFORE, "u32"), lone(1, &after, "u16"));
    let report = run_links(LET_BEFORE, &after, &LinkBodies(vec![body], Default::default()));
    assert!(report.facts.iter().any(|fact| fact.category == Category::Indirect && fact.description.contains("x as u64")), "{report:#?}");
}

#[test]
fn a_changed_type_in_a_pattern_is_no_new_name_of_a_local() {
    // Design 45, safety 1: `let x: u32` → `let x: u16` gave "renamed local `u32` → `u16`" (8f),
    // and rule 7 of 36.1 gave it `code equal`. Only a name of the binding part is a new name.
    assert_eq!(hints::pattern_name_change("x: u32", "x: u16"), None);
    assert_eq!(hints::pattern_name_change("x: u32", "y: u32"), Some(("x".to_string(), "y".to_string())));
    assert_eq!(hints::pattern_name_change("(a, b): (u32, u32)", "(a, c): (u32, u32)"), Some(("b".to_string(), "c".to_string())));
    assert_eq!(hints::pattern_name_change("mut x: Vec<T>", "mut x: Vec<U>"), None);
}

// ---- Batch 9a (design 50): the syntax tree model is the default ----

/// A step check in the default mode (design 50): no compiled check and no body oracle.
fn run_tree(before: &str, after: &str) -> StepReport {
    let (_d1, old) = load(before);
    let (_d2, new) = load(after);
    let mut step = step(Vec::new());
    step.compiled = false;
    verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &NoOracle, &NoOracle, true).report
}

/// The fact texts with no note about the public API.
fn plain_texts(report: &StepReport) -> Vec<String> {
    report.facts.iter().map(|fact| fact.description.split(" (the public API").next().unwrap_or_default().to_string()).collect()
}

const CONDITION: &str = "pub fn f(x: u32) -> u32 {\n    if x > 3 {\n        1\n    } else {\n        0\n    }\n}\n";

/// Design 50: a fact of the default mode has no compare result, also not in the JSON form.
#[test]
fn a_fact_of_the_default_mode_has_no_compare_result() {
    let after = CONDITION.replace("x > 3", "x > 3 && x < 9");
    let report = run_tree(CONDITION, &after);
    assert!(report.accepted() && report.errors.is_empty(), "{report:#?}");
    assert_eq!(report.facts.len(), 1, "{report:#?}");
    let fact = &report.facts[0];
    assert!(fact.compare.is_off() && fact.label() == "semantics" && fact.affected.is_empty(), "{fact:#?}");
    let json = serde_json::to_value(fact).unwrap();
    assert!(json.get("compare").is_none(), "{json}");
    // The step with the compiled check keeps the compare result.
    let report = run_derived(CONDITION, &after, &[("demo::f", different(false))]);
    let fact = &report.facts[0];
    assert!(fact.compare == Compare::CodeDifferent && fact.label() == "semantics · code different", "{fact:#?}");
    assert_eq!(serde_json::to_value(fact).unwrap()["compare"], "code_different");
}

/// Design 50: no note gives a cause for a compare that Semilla did not make.
#[test]
fn a_doc_test_fact_of_the_default_mode_has_no_note_about_the_compare() {
    let before = "/// Adds one.\n///\n/// ```\n/// assert_eq!(demo::inc(1), 2);\n/// ```\npub fn inc(x: u32) -> u32 {\n    x + 1\n}\n";
    let after = before.replace("inc(1), 2", "inc(2), 3");
    let report = run_tree(before, &after);
    assert_eq!(plain_texts(&report), ["doc test of `demo::inc` changed"]);
    assert!(report.facts[0].note.is_none() && report.facts[0].compare.is_off(), "{report:#?}");
    let report = run_derived(before, &after, &[("demo::inc", equal("n0"))]);
    assert!(report.facts[0].note.as_deref().is_some_and(|note| note.starts_with("no compare:")), "{report:#?}");
}

const ENUM_USES: &str = "enum Fault {\n    Low,\n    High,\n}\nfn first() -> Fault {\n    Fault::Low\n}\nfn second() -> Fault {\n    Fault::High\n}\nfn third() -> Fault {\n    Fault::Low\n}\n";

/// Design 50, part 3: the proof of a rename uses only the syntax tree. The rename fact covers a
/// use only if the use is equal after the map.
#[test]
fn a_rename_from_the_syntax_tree_and_a_use_with_a_second_difference() {
    let renamed = ENUM_USES.replace("Fault", "Problem");
    let report = run_tree(ENUM_USES, &renamed);
    assert!(report.accepted() && report.uncovered.is_empty(), "{report:#?}");
    assert_eq!(plain_texts(&report), ["renamed `demo::Fault` → `demo::Problem`"]);
    assert!(report.facts[0].compare.is_off(), "{report:#?}");
    // The use in `third` changes to a different variant: it gets its own fact with the full
    // old text and the full new text.
    let second = renamed.replace("fn third() -> Problem {\n    Problem::Low", "fn third() -> Problem {\n    Problem::High");
    let report = run_tree(ENUM_USES, &second);
    assert!(report.accepted() && report.uncovered.is_empty(), "{report:#?}");
    assert_eq!(plain_texts(&report), ["renamed `demo::Fault` → `demo::Problem`", "changed path `Fault::Low` → `Problem::High` at `demo::third/tail`"]);
    // The same rule with the compiled check.
    let report = run_derived(ENUM_USES, &second, &[("demo::first", equal("n0")), ("demo::second", equal("n0")), ("demo::third", different(false))]);
    assert_eq!(plain_texts(&report), ["renamed `demo::Fault` → `demo::Problem`", "changed path `Fault::Low` → `Problem::High` at `demo::third/tail`"], "{report:#?}");
}

const MOVE_PATHS: &str = "mod cart {\n    pub fn over(x: u32, limit: u32) -> u32 {\n        if x > limit { x - limit } else { 0 }\n    }\n    pub fn keep(x: u32) -> u32 {\n        x\n    }\n}\nmod other {\n    pub fn over(x: u32, limit: u32) -> u32 {\n        x + limit\n    }\n}\nmod util {\n    pub fn half(x: u32) -> u32 {\n        x / 2\n    }\n}\npub fn a(x: u32) -> u32 {\n    cart::over(x, 3) + cart::keep(x) + util::half(x)\n}\npub fn c(x: u32) -> u32 {\n    other::over(x, 5)\n}\n";

fn move_paths_after() -> String {
    let item = "    pub fn over(x: u32, limit: u32) -> u32 {\n        if x > limit { x - limit } else { 0 }\n    }\n";
    MOVE_PATHS.replacen(item, "", 1).replace("        x / 2\n    }\n", &format!("        x / 2\n    }}\n{item}")).replace("cart::over(x, 3)", "util::over(x, 3)")
}

/// Design 50, part 3: a path edit is a forced edit of a move only if the path names the item on
/// each side.
#[test]
fn a_path_to_a_different_item_is_not_a_forced_edit_of_a_move() {
    let after = move_paths_after();
    let report = run_tree(MOVE_PATHS, &after);
    assert!(report.accepted() && report.uncovered.is_empty(), "{report:#?}");
    assert_eq!(plain_texts(&report), ["moved `demo::cart::over` → `demo::util::over`"]);
    // `other::over` is a different item. The edit to `util::over` is a decision of the author.
    let report = run_tree(MOVE_PATHS, &after.replace("other::over(x, 5)", "util::over(x, 5)"));
    assert!(report.accepted() && report.uncovered.is_empty(), "{report:#?}");
    assert_eq!(plain_texts(&report), ["moved `demo::cart::over` → `demo::util::over`", "changed path `other::over` → `util::over` at `demo::c/tail/callee`"]);
}

const MOVE_USES: &str = "mod cart {\n    pub fn over(x: u32, limit: u32) -> u32 {\n        if x > limit { x - limit } else { 0 }\n    }\n    pub mod a {\n        pub fn helper(x: u32) -> u32 {\n            x + 1\n        }\n    }\n    pub mod b {\n        pub fn helper(x: u32) -> u32 {\n            x + 2\n        }\n    }\n}\nmod util {\n    pub fn half(x: u32) -> u32 {\n        x / 2\n    }\n}\nuse cart::a::helper;\nuse cart::over;\npub fn a(x: u32) -> u32 {\n    over(x, 3) + helper(x) + util::half(x)\n}\n";

/// Design 50, part 3: a `use` line is a forced edit of a move only if the map gives the new line.
#[test]
fn a_use_line_with_a_second_difference_gets_its_own_fact() {
    let item = "    pub fn over(x: u32, limit: u32) -> u32 {\n        if x > limit { x - limit } else { 0 }\n    }\n";
    let moved = MOVE_USES.replacen(item, "", 1).replace("        x / 2\n    }\n", &format!("        x / 2\n    }}\n{item}")).replace("use cart::over;", "use util::over;");
    let report = run_tree(MOVE_USES, &moved);
    assert!(report.accepted() && report.uncovered.is_empty(), "{report:#?}");
    assert_eq!(plain_texts(&report), ["moved `demo::cart::over` → `demo::util::over`, with 1 `use` line(s)"]);
    // The second `use` line changes to a different item: the move fact does not take it.
    let report = run_tree(MOVE_USES, &moved.replace("use cart::a::helper;", "use cart::b::helper;"));
    assert!(report.accepted() && report.uncovered.is_empty(), "{report:#?}");
    let texts = plain_texts(&report);
    assert!(texts[0].starts_with("moved `demo::cart::over` → `demo::util::over`"), "{texts:#?}");
    assert!(texts.iter().any(|text| text.contains("`use` line") && text.contains("use cart::a::helper;") && text.contains("use cart::b::helper;")) || (texts.iter().any(|text| text.starts_with("removed `use` line: `use cart::a::helper;`")) && texts.iter().any(|text| text.starts_with("added `use` line: `use cart::b::helper;`"))), "{texts:#?}");
}

/// Design 50, part 3: two maps pass with the same score. The default mode gives no rename fact.
#[test]
fn a_tie_of_two_maps_gives_removed_and_added_in_the_default_mode() {
    let twins = "fn one(a: u32) -> u32 {\n    a + 1\n}\nfn two(a: u32) -> u32 {\n    a + 1\n}\n";
    let report = run_tree(twins, &twins.replace("fn one", "fn first").replace("fn two", "fn second"));
    assert_eq!(plain_texts(&report), ["removed `demo::one`", "removed `demo::two`", "added `demo::first`", "added `demo::second`"]);
    assert!(report.notes.len() == 1 && report.notes[0].contains("show(s) as removed and added: Semilla does not derive the rename or the move (more than one name map"), "{:#?}", report.notes);
    // One pair has one map: the rename fact shows.
    let report = run_tree(twins, &twins.replace("fn one", "fn first"));
    assert_eq!(plain_texts(&report), ["renamed `demo::one` → `demo::first`"]);
    assert!(report.notes.is_empty(), "{:#?}", report.notes);
}

/// Design 50, part 3: an item with a new name and an edit in its body. The rename fact stays,
/// and the edit gets its own fact at the new path.
#[test]
fn a_renamed_item_with_a_body_edit_keeps_the_rename_fact() {
    let before = "fn scale(x: u32) -> u32 {\n    x * 2 + 1\n}\npub fn a(x: u32) -> u32 {\n    scale(x)\n}\n";
    let report = run_tree(before, &before.replace("scale", "double").replace("x * 2 + 1", "x * 2 + 2"));
    assert!(report.accepted() && report.uncovered.is_empty(), "{report:#?}");
    assert_eq!(plain_texts(&report), ["renamed `demo::scale` → `demo::double`", "changed literal `1` → `2` at `demo::double/tail/rhs`"]);
}

const PATTERNS: &str = "pub struct Tank {\n    pub level: u32,\n}\npub fn a(t: &Tank) -> u32 {\n    let Tank { level } = t;\n    *level\n}\npub fn b(t: &Tank) -> u32 {\n    let Tank { level } = t;\n    *level + 1\n}\npub fn c(t: &Tank) -> u32 {\n    let Tank { level } = t;\n    *level + 2\n}\n";

/// Design 50, part 4: the site line of a pattern names the exact edit (`..` or the field text).
#[test]
fn a_pattern_site_names_the_exact_edit() {
    let after = PATTERNS.replace("    pub level: u32,\n}", "    pub level: u32,\n    pub cap: u32,\n}").replacen("let Tank { level } = t;\n    *level\n", "let Tank { level, .. } = t;\n    *level\n", 1).replace("let Tank { level } = t;\n    *level + 1", "let Tank { level, cap } = t;\n    *level + 1").replace("let Tank { level } = t;\n    *level + 2", "let Tank { level, cap: _c } = t;\n    *level + 2");
    let report = run_tree(PATTERNS, &after);
    assert!(report.uncovered.is_empty(), "{report:#?}");
    assert_eq!(plain_texts(&report), ["field `demo::Tank.cap` added: `u32`"]);
    let groups: Vec<(String, usize)> = report.facts[0].sites.iter().map(|group| (group.edit.clone(), group.sites.len())).collect();
    assert_eq!(groups, [("add `..` to the pattern".to_string(), 1), ("add the field `cap` to the pattern".to_string(), 1), ("add the field `cap : _c` to the pattern".to_string(), 1)]);
    // A pattern with a second difference is not a site of the field fact.
    let second = PATTERNS.replace("    pub level: u32,\n}", "    pub level: u32,\n    pub cap: u32,\n}").replace("let Tank { level } = t;\n    *level + 2", "let Tank { level: other, .. } = t;\n    *other + 2");
    let report = run_tree(PATTERNS, &second);
    assert!(report.facts[0].sites.iter().all(|group| group.sites.iter().all(|site| site.item != "demo::c")), "{report:#?}");
    assert!(report.facts.len() > 1 && report.facts[1..].iter().any(|fact| fact.at.as_ref().is_some_and(|at| at.item == "demo::c")), "{report:#?}");
}

/// Design 50, part 4: a use that changes to a different variant is a decision of the author.
/// The fact of the removed variant does not take it.
#[test]
fn a_use_that_changes_to_a_different_variant_gets_its_own_fact() {
    let before = "pub enum Mode {\n    Slow,\n    Quick,\n    Off,\n}\npub fn a() -> Mode {\n    Mode::Slow\n}\npub fn b() -> Mode {\n    Mode::Slow\n}\npub fn c() -> Mode {\n    Mode::Slow\n}\n";
    let after = before.replace("    Slow,\n", "").replace("pub fn c() -> Mode {\n    Mode::Slow", "pub fn c() -> Mode {\n    Mode::Off").replace("Mode::Slow", "Mode::Quick");
    let report = run_tree(before, &after);
    assert!(report.uncovered.is_empty(), "{report:#?}");
    assert_eq!(plain_texts(&report), ["variant `demo::Mode::Slow` removed: `Slow`", "2 site(s): changed path `Mode::Slow` → `Mode::Quick`", "changed path `Mode::Slow` → `Mode::Off` at `demo::c/tail`"]);
    assert!(report.facts[0].sites.is_empty(), "{report:#?}");
}

/// Design 50, part 4: the cut mark `…` of a one-side text is a label. The fact has the full text
/// of the node in `content`.
#[test]
fn a_fact_with_a_cut_text_has_the_full_text() {
    let before = "pub fn f(a: u32, b: u32) -> u32 {\n    let s = a + b;\n    s\n}\n";
    let after = before.replace("    s\n", "    let t = s.wrapping_mul(31).wrapping_add(a).wrapping_mul(17).wrapping_add(b).rotate_left(5) ^ 0x5bd1e995;\n    s + t\n");
    let report = run_tree(before, &after);
    let fact = report.facts.iter().find(|fact| fact.description.starts_with("added `let`")).expect("the fact of the added statement");
    assert!(fact.description.contains('…'), "{fact:#?}");
    assert!(fact.content.as_deref().is_some_and(|content| content.contains("rotate_left(5) ^ 0x5bd1e995")), "{fact:#?}");
    // A pair of texts has a cut only in an equal part: each different part shows.
    let pair = report.facts.iter().find(|fact| fact.description.starts_with("changed expression")).expect("the fact of the tail");
    assert_eq!(pair.description, "changed expression `s` → `s + t` at `demo::f/tail`");
}

/// Design 50, part 4: a type change in N places is one fact in the default mode, and the fact
/// names each place.
#[test]
fn a_type_change_in_the_default_mode_names_each_place() {
    let before = "pub struct Pair {\n    pub a: u32,\n    pub b: u32,\n}\npub fn one(x: u32) -> u32 {\n    x\n}\n";
    let report = run_tree(before, &before.replace("u32", "u64"));
    assert_eq!(report.facts.len(), 1, "{report:#?}");
    let text = &report.facts[0].description;
    assert!(text.starts_with("type `u32` → `u64` in 4 places: ") && !text.contains('…'), "{text}");
    for place in ["`demo::Pair.a`", "`demo::Pair.b`", "parameter `x` of `demo::one`", "return type of `demo::one`"] {
        assert!(text.contains(place), "{place}: {text}");
    }
    // A place with a different new type is not in the fold.
    let report = run_tree(before, &before.replace("u32", "u64").replace("pub b: u64", "pub b: u16"));
    assert!(report.facts.iter().any(|fact| fact.description.contains("`u32` → `u16`")) && report.facts.iter().any(|fact| fact.description.starts_with("type `u32` → `u64` in 3 places: ")), "{:#?}", descriptions(&report));
}

/// Design 50, part 3 (the scope rule): the import that a moved item needs goes under the move
/// fact. An import that the map does not explain keeps its own fact.
#[test]
fn an_import_that_a_moved_item_needs_goes_under_the_move_fact() {
    let before = "mod audit {\n    pub struct Holder(pub u32);\n    pub fn limit() -> u32 { 9 }\n    pub fn check(h: &Holder) -> bool { h.0 < limit() }\n}\nmod quota {\n    pub fn half(x: u32) -> u32 { x / 2 }\n}\nmod stock {\n    use std::fmt;\n    pub fn error() -> fmt::Error { fmt::Error }\n}\npub fn a(h: &audit::Holder) -> bool { audit::check(h) }\n";
    let after = "mod audit {\n    pub struct Holder(pub u32);\n    pub fn limit() -> u32 { 9 }\n}\nmod quota {\n    use crate::audit::{limit, Holder};\n    pub fn half(x: u32) -> u32 { x / 2 }\n    pub fn check(h: &Holder) -> bool { h.0 < limit() }\n}\nmod stock {\n    use std::fmt;\n    pub fn error() -> fmt::Error { fmt::Error }\n}\npub fn a(h: &audit::Holder) -> bool { quota::check(h) }\n";
    let report = run_tree(before, after);
    assert!(report.accepted() && report.uncovered.is_empty(), "{report:#?}");
    assert_eq!(plain_texts(&report), ["moved `demo::audit::check` → `demo::quota::check`, with 1 `use` line(s)"]);
    // The moved item does not use the name `fmt`: the second new line keeps its own fact.
    let report = run_tree(before, &after.replace("    use crate::audit::{limit, Holder};\n", "    use crate::audit::{limit, Holder};\n    use std::fmt;\n"));
    let texts = plain_texts(&report);
    assert!(texts[0].ends_with("with 1 `use` line(s)") && texts[1..] == ["added `use` line: `use std::fmt;` in `demo::quota`"], "{texts:#?}");
    // A module with no item of the map: the map is not the cause of a `use` edit there.
    let report = run_tree(before, &after.replace("    use std::fmt;\n", "    use std::fmt::{self, Formatter};\n"));
    let texts = plain_texts(&report);
    assert!(texts[0].ends_with("with 1 `use` line(s)") && texts[1..] == ["removed `use` line: `use std::fmt;` in `demo::stock`", "added `use` line: `use std::fmt::{self, Formatter};` in `demo::stock`"], "{texts:#?}");
}

/// Design 50, part 3: a `use` line with a new order of its names is not an effect of a rename.
#[test]
fn a_use_line_that_the_map_does_not_change_keeps_its_fact() {
    let before = "use std::collections::{BTreeMap, BTreeSet};\nfn one() -> (BTreeMap<u32, u32>, BTreeSet<u32>) {\n    (BTreeMap::new(), BTreeSet::new())\n}\n";
    let report = run_tree(before, &before.replace("fn one", "fn first").replace("{BTreeMap, BTreeSet}", "{BTreeSet, BTreeMap}"));
    let texts = plain_texts(&report);
    assert_eq!(texts.len(), 2, "{texts:#?}");
    assert!(texts[0] == "renamed `demo::one` → `demo::first`" && texts[1].contains("`use` line") && texts[1].contains("{BTreeSet, BTreeMap}"), "{texts:#?}");
}

// ---- Batch 9a2 (design 51): the map of a rename is a map of the references to the item ----

/// The text with each pair replaced. Each old text must be in the text.
fn swap(text: &str, pairs: &[(&str, &str)]) -> String {
    pairs.iter().fold(text.to_string(), |text, (old, new)| {
        assert!(text.contains(old), "the text has no `{old}`");
        text.replace(old, new)
    })
}

/// The rename and move facts of a report, and each other fact. A fact of the public API does
/// not count.
fn rename_and_other(report: &StepReport) -> (Vec<String>, Vec<String>) {
    plain_texts(report).into_iter().filter(|text| !text.starts_with("public API: ")).partition(|text| text.starts_with("renamed `") || text.starts_with("moved `"))
}

/// The note of the fact with this text.
fn note_of<'a>(report: &'a StepReport, text: &str) -> Option<&'a str> {
    report.facts.iter().find(|fact| fact.description.starts_with(text)).unwrap_or_else(|| panic!("no fact `{text}`: {report:#?}")).note.as_deref()
}

/// A pure rename: the facts are the rename facts, and no fact has a note.
fn pure_rename(before: &str, after: &str, renames: &[&str]) {
    let report = run_tree(before, after);
    assert_eq!(rename_and_other(&report), (renames.iter().map(|text| text.to_string()).collect(), Vec::new()), "{report:#?}");
    assert!(report.facts.iter().all(|fact| fact.note.is_none()), "{report:#?}");
}

/// A rename with one more edit: the rename fact stays, and the second edit has its own fact.
fn rename_with_second_edit(before: &str, after: &str, rename: &str, second: &[&str]) {
    let report = run_tree(before, after);
    let (renames, other) = rename_and_other(&report);
    assert_eq!(renames, [rename], "{report:#?}");
    assert_eq!(other, second, "{report:#?}");
}

/// The example crate of design 51. The step renames `disk::load` to `disk::fetch`.
const DEPOT: &str = r#"
pub mod disk {
    pub fn load(k: u32) -> u32 { k + 1 }
    pub fn twice(k: u32) -> u32 { load(k) * 2 }
}
pub mod net {
    pub fn load(k: u32) -> u32 { k }
    pub fn fetch(k: u32) -> u32 { k + 100 }
    pub struct Link { pub load: u32, pub fetch: u32 }
    impl Link {
        pub fn load(&self) -> u32 { self.load }
        pub fn fetch(&self) -> u32 { self.fetch }
    }
    pub fn a() -> u32 { crate::net::load(1) }
    pub fn b() -> u32 { load(2) }
    pub fn c(l: &Link) -> u32 { l.load() }
    pub fn d(l: &Link) -> u32 { l.load }
    pub fn e(l: &Link) -> u32 { Link::load(l) }
    pub fn name() -> &'static str { "load" }
    pub fn local() -> u32 { let load = 5; load + 1 }
}
pub mod ui {
    use crate::disk::load;
    use crate::net::load as net_load;
    pub fn show() -> String { format!("load {}", load(3)) }
    pub fn other() -> u32 { net_load(4) }
}
"#;
const DEPOT_RENAME: [(&str, &str); 4] = [("pub fn load(k: u32) -> u32 { k + 1 }", "pub fn fetch(k: u32) -> u32 { k + 1 }"), ("{ load(k) * 2 }", "{ fetch(k) * 2 }"), ("use crate::disk::load;", "use crate::disk::fetch;"), (", load(3))", ", fetch(3))")];
const DEPOT_FACT: &str = "renamed `demo::disk::load` → `demo::disk::fetch`";

/// The rename of the example crate with one more edit.
fn depot_second_edit(pair: (&str, &str), second: &str) {
    rename_with_second_edit(DEPOT, &swap(&swap(DEPOT, &DEPOT_RENAME), &[pair]), DEPOT_FACT, &[second]);
}

/// Design 51, D10: a pure rename gives only the facts of the rename. The old word in a string,
/// in a different module, and as a member of a different type is not a site. Before: three
/// false facts (a `use` line with two equal texts, "moved code" in `Link`, a macro input).
#[test]
fn a_pure_rename_gives_no_fact_for_a_place_with_the_old_word_that_is_not_a_site() {
    pure_rename(DEPOT, &swap(DEPOT, &DEPOT_RENAME), &[DEPOT_FACT]);
}

/// Design 51, D1: a path with a qualifier that resolves to a different item (class 3).
#[test]
fn a_changed_path_to_a_different_module_is_not_under_the_rename_fact() {
    depot_second_edit(("crate::net::load(1)", "crate::net::fetch(1)"), "changed path `crate::net::load` → `crate::net::fetch` at `demo::net::a/tail/callee`");
}

/// Design 51, D2: a bare name that the module scope resolves to a different item (class 3).
#[test]
fn a_changed_bare_name_of_a_module_with_its_own_item_is_not_under_the_rename_fact() {
    depot_second_edit(("{ load(2) }", "{ fetch(2) }"), "changed path `load` → `fetch` at `demo::net::b/tail/callee`");
}

/// Design 51, D3: a method call. A different declaration has the name, so the name is not free.
#[test]
fn a_changed_method_call_with_the_name_of_a_renamed_function_is_not_under_the_rename_fact() {
    depot_second_edit(("l.load()", "l.fetch()"), "changed method_call: `l.load()` → `l.fetch()` at `demo::net::c/tail`");
}

/// Design 51, D4: a field after `.`.
#[test]
fn a_changed_field_use_with_the_name_of_a_renamed_function_is_not_under_the_rename_fact() {
    depot_second_edit(("{ l.load }", "{ l.fetch }"), "changed field: `l.load` → `l.fetch` at `demo::net::d/tail`");
}

/// Design 51, D5: a path to a method of a type (class 3).
#[test]
fn a_changed_path_to_a_method_of_a_type_is_not_under_the_rename_fact() {
    depot_second_edit(("Link::load(l)", "Link::fetch(l)"), "changed path `Link::load` → `Link::fetch` at `demo::net::e/tail/callee`");
}

/// Design 51, D6: a `use` line of a different item.
#[test]
fn a_changed_use_line_of_a_different_item_is_not_under_the_rename_fact() {
    depot_second_edit(("use crate::net::load as net_load;", "use crate::net::fetch as net_load;"), "changed `use` line: `use crate::net::load as net_load;` → `use crate::net::fetch as net_load;` in `demo::ui`");
}

/// Design 51, D7: a string literal is never a site (class 1).
#[test]
fn a_changed_string_literal_with_the_old_name_is_not_under_the_rename_fact() {
    depot_second_edit((r#"{ "load" }"#, r#"{ "fetch" }"#), r#"changed literal `"load"` → `"fetch"` at `demo::net::name/tail`"#);
}

/// Design 51, D8: the format string of a macro call is never a site (class 1).
#[test]
fn a_changed_format_string_with_the_old_name_is_not_under_the_rename_fact() {
    depot_second_edit((r#"format!("load {}""#, r#"format!("fetch {}""#), r#"changed literal `"load {}"` → `"fetch {}"` at `demo::ui::show/tail/args[0]`"#);
}

/// Design 51, D9: a local declares a different thing (class 1), and its uses resolve to the
/// local (class 3). The rename of the local has its own fact.
#[test]
fn a_renamed_local_with_the_name_of_a_renamed_function_has_its_own_fact() {
    depot_second_edit(("let load = 5; load + 1", "let fetch = 5; fetch + 1"), "`demo::net::local`: renamed local `load` → `fetch`, 2 edit(s)");
}

/// Design 51, rule R6: the syntax phase makes the facts in the two modes.
#[test]
fn the_second_edit_has_its_own_fact_with_the_compiled_check() {
    let after = swap(&swap(DEPOT, &DEPOT_RENAME), &[("crate::net::load(1)", "crate::net::fetch(1)")]);
    let report = run_derived(DEPOT, &after, &[("demo::net::a", different(false))]);
    let texts = plain_texts(&report);
    assert!(texts.iter().any(|text| text == DEPOT_FACT) && texts.iter().any(|text| text.starts_with("changed path `crate::net::load` → `crate::net::fetch`")), "{report:#?}");
}

const TANK: &str = r#"
pub struct Tank { pub level: u32, pub cap: u32 }
pub fn a(t: &Tank) -> u32 { t.level + 1 }
pub fn b(t: &Tank) -> bool { t.level < t.cap }
pub fn c() -> Tank { Tank { level: 1, cap: 2 } }
"#;
const TANK_RENAME: [(&str, &str); 4] = [("pub level", "pub depth"), ("t.level + 1", "t.depth + 1"), ("t.level < t.cap", "t.depth < t.cap"), ("Tank { level: 1", "Tank { depth: 1")];
const TANK_FACT: &str = "renamed `demo::Tank.level` → `demo::Tank.depth`";
const PUMP: &str = r#"pub struct Pump { pub level: u32, pub depth: u32 }
pub fn p(p: &Pump) -> u32 { p.level }
pub fn q() -> Pump { Pump { level: 1, depth: 2 } }
"#;

/// Design 51, rules R2 and R3: a member after `.` is a site by name. With a free name, the
/// sites stay under the rename fact, and the fact gives their count. The label of a struct
/// literal with the path of the type is a sure site.
#[test]
fn a_field_rename_with_a_free_name_keeps_its_sites_and_gives_their_count() {
    let report = run_tree(TANK, &swap(TANK, &TANK_RENAME));
    assert_eq!(rename_and_other(&report), (vec![TANK_FACT.to_string()], Vec::new()), "{report:#?}");
    assert_eq!(note_of(&report, TANK_FACT), Some("2 site(s) by name (the syntax tree does not give the type of the receiver or the target of the path)"), "{report:#?}");
}

/// Design 51, rule R2: a second type has the member name. So the name is not free, and each
/// site by name is an edit with its own fact (equal edits fold). The rename fact has no count.
#[test]
fn a_field_rename_with_a_name_of_a_second_type_gives_each_site_by_name_its_own_fact() {
    let vat = "pub struct Vat { pub level: u32 }\n";
    let (before, after) = (format!("{TANK}{vat}"), format!("{}{vat}", swap(TANK, &TANK_RENAME)));
    rename_with_second_edit(&before, &after, TANK_FACT, &["2 site(s): changed expression `t.level` → `t.depth`"]);
    assert_eq!(note_of(&run_tree(&before, &after), TANK_FACT), None);
    // The new name of a second type also takes the name.
    let vat = "pub struct Vat { pub depth: u32 }\n";
    rename_with_second_edit(&format!("{TANK}{vat}"), &format!("{}{vat}", swap(TANK, &TANK_RENAME)), TANK_FACT, &["2 site(s): changed expression `t.level` → `t.depth`"]);
}

/// Design 51, D11, a field: a use of the member of a different type, the labels of its
/// literal, and a string are not under the rename fact of the field.
#[test]
fn a_second_edit_at_a_field_rename_has_its_own_fact() {
    let (before, pure) = (format!("{TANK}{PUMP}"), format!("{}{PUMP}", swap(TANK, &TANK_RENAME)));
    let sites = "2 site(s): changed expression `t.level` → `t.depth`";
    rename_with_second_edit(&before, &pure, TANK_FACT, &[sites]);
    rename_with_second_edit(&before, &swap(&pure, &[("{ p.level }", "{ p.depth }")]), TANK_FACT, &[sites, "changed field: `p.level` → `p.depth` at `demo::p/tail`"]);
    rename_with_second_edit(&before, &swap(&pure, &[("Pump { level: 1, depth: 2 }", "Pump { depth: 1, level: 2 }")]), TANK_FACT, &[sites, "changed struct: `Pump { level: 1, depth: 2 }` → `Pump { depth: 1, level: 2 }` at `demo::q/tail`"]);
}

const CART_TOTAL: &str = r#"
pub struct Cart { pub items: Vec<u32> }
impl Cart {
    pub fn total(&self) -> u32 { self.items.iter().sum() }
}
pub fn a(c: &Cart) -> u32 { c.total() }
pub fn b(c: &Cart) -> u32 { c.total() + 1 }
pub fn c(c: &Cart) -> bool { Cart::total(c) > 9 }
"#;
const CART_TOTAL_RENAME: [(&str, &str); 4] = [("pub fn total", "pub fn sum"), ("{ c.total() }", "{ c.sum() }"), ("c.total() + 1", "c.sum() + 1"), ("Cart::total(c)", "Cart::sum(c)")];
const CART_TOTAL_FACT: &str = "renamed `demo::Cart::total` → `demo::Cart::sum`";

/// Design 51, rules R2 and R3 for a method: the two calls after `.` are sites by name, and the
/// path `Cart::total` is a sure site.
#[test]
fn a_method_rename_with_a_free_name_keeps_its_sites_and_gives_their_count() {
    let report = run_tree(CART_TOTAL, &swap(CART_TOTAL, &CART_TOTAL_RENAME));
    assert_eq!(rename_and_other(&report), (vec![CART_TOTAL_FACT.to_string()], Vec::new()), "{report:#?}");
    assert!(note_of(&report, CART_TOTAL_FACT).is_some_and(|note| note.starts_with("2 site(s) by name (")), "{report:#?}");
}

/// Design 51, rule R2 for a method: a method of a second type has the old name or the new name.
#[test]
fn a_method_rename_with_a_name_of_a_second_type_gives_each_call_its_own_fact() {
    let calls = ["changed method_call: `c.total()` → `c.sum()` at `demo::a/tail`", "changed expression `c.total()` → `c.sum()` at `demo::b/tail/lhs`"];
    for name in ["total", "sum"] {
        let bill = format!("pub struct Bill {{ pub lines: Vec<u32> }}\nimpl Bill {{\n    pub fn {name}(&self) -> u32 {{ self.lines.len() as u32 }}\n}}\n");
        let (before, after) = (format!("{CART_TOTAL}{bill}"), format!("{}{bill}", swap(CART_TOTAL, &CART_TOTAL_RENAME)));
        rename_with_second_edit(&before, &after, CART_TOTAL_FACT, &calls);
        assert_eq!(note_of(&run_tree(&before, &after), CART_TOTAL_FACT), None);
    }
}

const CART: &str = r#"
pub struct Cart { pub items: Vec<u32> }
impl Cart { pub fn new() -> Cart { Cart { items: Vec::new() } } }
pub mod old {
    pub struct Cart;
    pub struct Basket;
    impl Cart { pub fn size() -> u32 { 1 } }
    impl Basket { pub fn size() -> u32 { 2 } }
    pub fn inner() -> u32 { Cart::size() }
}
pub fn a(c: &Cart) -> usize { c.items.len() }
pub fn b() -> Cart { Cart::new() }
pub fn size() -> u32 { old::Cart::size() }
pub fn name() -> &'static str { "Cart" }
"#;
const CART_RENAME: [(&str, &str); 4] = [("pub struct Cart { pub items", "pub struct Basket { pub items"), ("impl Cart { pub fn new() -> Cart { Cart { items", "impl Basket { pub fn new() -> Basket { Basket { items"), ("c: &Cart", "c: &Basket"), ("-> Cart { Cart::new", "-> Basket { Basket::new")];
const CART_FACT: &str = "renamed `demo::Cart` → `demo::Basket`";

/// Design 51, D11, a type: a path to a type of a different module with the same two names, the
/// bare name in that module, and a string are not under the rename fact of the type.
#[test]
fn a_second_edit_at_a_type_rename_has_its_own_fact() {
    let pure = swap(CART, &CART_RENAME);
    pure_rename(CART, &pure, &[CART_FACT]);
    rename_with_second_edit(CART, &swap(&pure, &[("old::Cart::size()", "old::Basket::size()")]), CART_FACT, &["changed path `old::Cart::size` → `old::Basket::size` at `demo::size/tail/callee`"]);
    rename_with_second_edit(CART, &swap(&pure, &[("{ Cart::size() }", "{ Basket::size() }")]), CART_FACT, &["changed path `Cart::size` → `Basket::size` at `demo::old::inner/tail/callee`"]);
    rename_with_second_edit(CART, &swap(&pure, &[(r#""Cart""#, r#""Basket""#)]), CART_FACT, &[r#"changed literal `"Cart"` → `"Basket"` at `demo::name/tail`"#]);
}

/// Design 51, D11, a variant: a path to a variant of a different enum with the same two names.
#[test]
fn a_second_edit_at_a_variant_rename_has_its_own_fact() {
    let before = "pub enum Mode { Slow, Quick }\npub enum Gear { Slow, Careful }\npub fn a() -> Mode { Mode::Slow }\npub fn c(m: &Mode) -> u32 { match m { Mode::Slow => 1, Mode::Quick => 2 } }\npub fn g() -> Gear { Gear::Slow }\n";
    let pure = swap(before, &[("Mode { Slow,", "Mode { Careful,"), ("{ Mode::Slow }", "{ Mode::Careful }"), ("Mode::Slow => 1", "Mode::Careful => 1")]);
    let fact = "renamed `demo::Mode::Slow` → `demo::Mode::Careful`";
    pure_rename(before, &pure, &[fact]);
    rename_with_second_edit(before, &swap(&pure, &[("Gear::Slow }", "Gear::Careful }")]), fact, &["changed path `Gear::Slow` → `Gear::Careful` at `demo::g/tail`"]);
}

/// Design 51, D11, a constant: a path to a constant of a different module, and the bare name in
/// that module.
#[test]
fn a_second_edit_at_a_constant_rename_has_its_own_fact() {
    let before = "const LIMIT: u32 = 10;\npub mod m {\n    pub const LIMIT: u32 = 1;\n    pub const CEILING: u32 = 2;\n    pub fn inner() -> u32 { LIMIT }\n}\npub fn a(x: u32) -> bool { x > LIMIT }\npub fn b(x: u32) -> u32 { x + LIMIT }\npub fn c() -> u32 { m::LIMIT * 2 }\n";
    let pure = swap(before, &[("const LIMIT: u32 = 10;", "const CEILING: u32 = 10;"), ("x > LIMIT", "x > CEILING"), ("x + LIMIT", "x + CEILING")]);
    let fact = "renamed `demo::LIMIT` → `demo::CEILING`";
    pure_rename(before, &pure, &[fact]);
    rename_with_second_edit(before, &swap(&pure, &[("m::LIMIT * 2", "m::CEILING * 2")]), fact, &["changed path `m::LIMIT` → `m::CEILING` at `demo::c/tail/lhs`"]);
    rename_with_second_edit(before, &swap(&pure, &[("{ LIMIT }", "{ CEILING }")]), fact, &["changed path `LIMIT` → `CEILING` at `demo::m::inner/tail`"]);
}

/// Design 51, D11, a module: a path through a different module with the same two names, and a
/// string.
#[test]
fn a_second_edit_at_a_module_rename_has_its_own_fact() {
    let before = "mod fees {\n    pub fn charge(x: u32) -> u32 { x + 2 }\n}\nmod other {\n    pub mod fees { pub fn charge(x: u32) -> u32 { x + 3 } }\n    pub mod costs { pub fn charge(x: u32) -> u32 { x + 4 } }\n}\npub fn a(x: u32) -> u32 { fees::charge(x) }\npub fn c(x: u32) -> u32 { other::fees::charge(x) }\npub fn name() -> &'static str { \"fees\" }\n";
    let pure = swap(before, &[("mod fees {\n    pub fn charge(x: u32) -> u32 { x + 2 }", "mod costs {\n    pub fn charge(x: u32) -> u32 { x + 2 }"), ("{ fees::charge(x) }", "{ costs::charge(x) }")]);
    let fact = "renamed `demo::fees` → `demo::costs`";
    pure_rename(before, &pure, &[fact]);
    rename_with_second_edit(before, &swap(&pure, &[("other::fees::charge(x)", "other::costs::charge(x)")]), fact, &["changed path `other::fees::charge` → `other::costs::charge` at `demo::c/tail/callee`"]);
    rename_with_second_edit(before, &swap(&pure, &[("\"fees\"", "\"costs\"")]), fact, &[r#"changed literal `"fees"` → `"costs"` at `demo::name/tail`"#]);
}

/// Design 51, D11, a trait method with its impl methods: a call of a method of a different type
/// and a path to that method are not under the rename fact. The call `s.area()` is a site by
/// name, and the name is not free here. The path `Shape::area` is a sure site.
#[test]
fn a_second_edit_at_a_trait_method_rename_has_its_own_fact() {
    let before = "pub trait Shape { fn area(&self) -> u32; }\npub struct Sq(pub u32);\nimpl Shape for Sq { fn area(&self) -> u32 { self.0 * self.0 } }\npub struct Plot { pub area: u32, pub size: u32 }\nimpl Plot {\n    pub fn area(&self) -> u32 { self.area }\n    pub fn size(&self) -> u32 { self.size }\n}\npub fn a(s: &Sq) -> u32 { s.area() }\npub fn b(s: &Sq) -> u32 { Shape::area(s) + 1 }\npub fn p(p: &Plot) -> u32 { p.area() }\npub fn q(p: &Plot) -> u32 { Plot::area(p) }\n";
    let pure = swap(before, &[("Shape { fn area(&self)", "Shape { fn size(&self)"), ("Sq { fn area(&self)", "Sq { fn size(&self)"), ("s.area()", "s.size()"), ("Shape::area(s)", "Shape::size(s)")]);
    let fact = "renamed `demo::Shape::area` → `demo::Shape::size`";
    let site = "changed method_call: `s.area()` → `s.size()` at `demo::a/tail`";
    rename_with_second_edit(before, &pure, fact, &[site]);
    rename_with_second_edit(before, &swap(&pure, &[("p.area()", "p.size()")]), fact, &[site, "changed method_call: `p.area()` → `p.size()` at `demo::p/tail`"]);
    rename_with_second_edit(before, &swap(&pure, &[("Plot::area(p)", "Plot::size(p)")]), fact, &[site, "changed path `Plot::area` → `Plot::size` at `demo::q/tail/callee`"]);
}

/// Design 51, D11 and rule R5, a local: the scope of the map is the body. A literal and a
/// member after `.` are never sites of a local.
#[test]
fn a_second_edit_at_a_local_rename_has_its_own_fact() {
    let before = "pub struct P { pub n: u32, pub m: u32 }\npub fn f(a: u32, p: &P) -> String {\n    let n = a + 1;\n    let twice = n * 2;\n    format!(\"{} {} {}\", \"n\", n + twice, p.n)\n}\n";
    let pure = swap(before, &[("let n = a + 1", "let m = a + 1"), ("twice = n * 2", "twice = m * 2"), ("n + twice", "m + twice")]);
    let fact = "`demo::f`: renamed local `n` → `m`, 3 edit(s)";
    assert_eq!(plain_texts(&run_tree(before, &pure)), [fact]);
    assert_eq!(plain_texts(&run_tree(before, &swap(&pure, &[("\"n\"", "\"m\"")]))), [fact, r#"changed literal `"n"` → `"m"` at `demo::f/tail/args[1]`"#]);
    assert_eq!(plain_texts(&run_tree(before, &swap(&pure, &[("p.n)", "p.m)")]))), [fact, "changed expression `p.n` → `p.m` at `demo::f/tail/args[3]`"]);
}

/// Design 51, rule R5, a parameter: the same rule as a local.
#[test]
fn a_second_edit_at_a_parameter_rename_has_its_own_fact() {
    let before = "pub struct P { pub count: u32, pub n: u32 }\npub fn f(count: u32, p: &P) -> u32 {\n    let twice = count * 2;\n    twice + count + p.count\n}\n";
    let pure = swap(before, &[("f(count: u32", "f(n: u32"), ("count * 2", "n * 2"), ("twice + count", "twice + n")]);
    let facts = ["parameter `count` of `demo::f` renamed to `n`: `u32`", "`demo::f`: renamed local `count` → `n`, 2 edit(s)"];
    assert_eq!(plain_texts(&run_tree(before, &pure)), facts);
    let found = plain_texts(&run_tree(before, &swap(&pure, &[("p.count", "p.n")])));
    assert_eq!(found[..2], facts, "{found:#?}");
    assert!(found.len() == 3 && found[2].contains("`p.count` → `p.n`"), "{found:#?}");
}

const MOVE: &str = "pub mod a {\n    pub fn cost(k: u32) -> u32 { k * 3 }\n    pub fn other(k: u32) -> u32 { k }\n}\npub mod b {\n    pub fn other(k: u32) -> u32 { k + 9 }\n}\npub fn one() -> u32 { a::cost(1) }\npub fn two() -> u32 { a::other(2) }\npub fn name() -> &'static str { \"a::cost\" }\n";
const MOVE_NEW: &str = "pub mod a {\n    pub fn other(k: u32) -> u32 { k }\n}\npub mod b {\n    pub fn cost(k: u32) -> u32 { k * 3 }\n    pub fn other(k: u32) -> u32 { k + 9 }\n}\npub fn one() -> u32 { b::cost(1) }\npub fn two() -> u32 { a::other(2) }\npub fn name() -> &'static str { \"a::cost\" }\n";

/// Design 51, rule R5, a move: the map of a move changes only a path to the moved item.
#[test]
fn a_second_edit_at_a_move_has_its_own_fact() {
    let fact = "moved `demo::a::cost` → `demo::b::cost`";
    pure_rename(MOVE, MOVE_NEW, &[fact]);
    let after = swap(MOVE_NEW, &[("a::other(2)", "b::other(2)"), ("\"a::cost\"", "\"b::cost\"")]);
    rename_with_second_edit(MOVE, &after, fact, &[r#"changed literal `"a::cost"` → `"b::cost"` at `demo::name/tail`"#, "changed path `a::other` → `b::other` at `demo::two/tail/callee`"]);
}

const SCOPE: &str = r#"
pub mod disk {
    pub struct Disk { pub size: u32 }
    impl Disk {
        pub fn load(&self) -> u32 { self.size }
        pub fn twice(&self) -> u32 { Self::load(self) * 2 + self.load() }
    }
    pub fn load(k: u32) -> u32 { k + 1 }
    pub fn shadow(k: u32) -> u32 { let load = k; load + 1 }
    pub fn param(load: u32) -> u32 { load + 2 }
    pub mod deep {
        pub fn up(k: u32) -> u32 { super::load(k) }
    }
}
pub mod glob {
    use crate::disk::*;
    pub fn all(k: u32) -> u32 { load(k) }
}
pub mod far {
    pub fn full(k: u32) -> u32 { crate::disk::load(k) + self::near() }
    pub fn near() -> u32 { demo::disk::load(1) }
}
"#;
const SCOPE_RENAME: [(&str, &str); 5] = [("pub fn load(k: u32)", "pub fn fetch(k: u32)"), ("super::load(k)", "super::fetch(k)"), ("{ load(k) }", "{ fetch(k) }"), ("crate::disk::load(k)", "crate::disk::fetch(k)"), ("demo::disk::load(1)", "demo::disk::fetch(1)")];

/// Design 51, class 2: a path through `super`, `crate`, the name of the crate, and a glob import
/// of a module of the workspace is a sure site. Class 1 and class 3: a local, a parameter, and
/// a method with the old name stay. No site is a site by name, so the fact has no count.
#[test]
fn a_path_that_the_syntax_tree_resolves_to_the_item_is_a_sure_site() {
    pure_rename(SCOPE, &swap(SCOPE, &SCOPE_RENAME), &[DEPOT_FACT]);
}

/// Design 51, class 3: a use of a local with the old name resolves to the local. A change of the
/// local is not under the rename fact of the item.
#[test]
fn a_local_with_the_old_name_is_not_a_site_of_the_item_rename() {
    let after = swap(&swap(SCOPE, &SCOPE_RENAME), &[("let load = k; load + 1", "let fetch = k; fetch + 1")]);
    rename_with_second_edit(SCOPE, &after, DEPOT_FACT, &["`demo::disk::shadow`: renamed local `load` → `fetch`, 2 edit(s)"]);
}

const MACRO_INPUT: &str = "macro_rules! twice { ($f:ident, $x:expr) => { $f($f($x)) }; }\npub fn load(k: u32) -> u32 { k + 1 }\npub fn a(k: u32) -> u32 { twice!(load, k) }\npub fn b(k: u32) -> u32 { load(k) }\n";
const MACRO_INPUT_RENAME: [(&str, &str); 3] = [("pub fn load", "pub fn fetch"), ("twice!(load, k)", "twice!(fetch, k)"), ("{ load(k) }", "{ fetch(k) }")];

/// Design 51, class 4: a word in a macro input that the syntax phase cannot read is a site by
/// name. A free name keeps the site under the rename fact, with the count. A name that is not
/// free gives the macro call its own fact.
#[test]
fn a_word_in_a_macro_input_is_a_site_by_name() {
    let fact = "renamed `demo::load` → `demo::fetch`";
    let report = run_tree(MACRO_INPUT, &swap(MACRO_INPUT, &MACRO_INPUT_RENAME));
    assert_eq!(rename_and_other(&report), (vec![fact.to_string()], Vec::new()), "{report:#?}");
    assert!(note_of(&report, fact).is_some_and(|note| note.starts_with("1 site(s) by name (")), "{report:#?}");
    let taken = "pub mod m { pub fn fetch() {} }\n";
    let (before, after) = (format!("{MACRO_INPUT}{taken}"), format!("{}{taken}", swap(MACRO_INPUT, &MACRO_INPUT_RENAME)));
    rename_with_second_edit(&before, &after, fact, &["replace macro input `twice!(load, k)` → `twice!(fetch, k)` at `demo::a/tail`"]);
    assert_eq!(note_of(&run_tree(&before, &after), fact), None);
}

/// Design 51, rule R4: a sure site that the author did not change still names the old item (the
/// tree does not build). The fact says so, and it does not show two equal texts.
#[test]
fn a_sure_site_that_did_not_follow_the_rename_says_that_it_names_the_old_item() {
    let pure = swap(DEPOT, &DEPOT_RENAME);
    // A path in a body.
    rename_with_second_edit(DEPOT, &swap(&pure, &[("{ fetch(k) * 2 }", "{ load(k) * 2 }")]), DEPOT_FACT, &["changed path: `load` still names the old item `load` (the place did not follow the rename to `fetch`; the syntax tree shows no other item with the old name for the place) at `demo::disk::twice/tail/lhs/callee`"]);
    // A `use` line.
    rename_with_second_edit(DEPOT, &swap(&pure, &[("use crate::disk::fetch;", "use crate::disk::load;")]), DEPOT_FACT, &["the `use` line still names the old item (the line did not follow the rename): `use crate::disk::load;` in `demo::ui`"]);
    // A type in a signature and in a field.
    let pure = swap(CART, &CART_RENAME);
    rename_with_second_edit(CART, &swap(&pure, &[("c: &Basket", "c: &Cart")]), CART_FACT, &["parameter of `demo::a` `c: &Cart` still names the old item `Cart` (the place did not follow the rename to `Basket`; the syntax tree shows no other item with the old name for the place)"]);
    let holder = "pub struct Holder { pub c: Cart }\n";
    rename_with_second_edit(&format!("{CART}{holder}"), &format!("{pure}{holder}"), CART_FACT, &["type of `demo::Holder.c`: `Cart` still names the old item `Cart` (the place did not follow the rename to `Basket`; the syntax tree shows no other item with the old name for the place)"]);
}

/// Design 51, rule R4: the fact of a place that did not follow the rename does not say "the old
/// item" if the syntax tree gives a different target for the old name after the step.
#[test]
fn a_place_that_did_not_follow_the_rename_names_the_item_that_the_old_name_gives() {
    let pure = swap(DEPOT, &DEPOT_RENAME);
    // A new `use` line gives the old name to a different item.
    let other = swap(&pure, &[("{ fetch(k) * 2 }", "{ load(k) * 2 }"), ("pub mod disk {\n", "pub mod disk {\n    use crate::net::load;\n")]);
    let (rename, facts) = rename_and_other(&run_tree(DEPOT, &other));
    assert_eq!(rename, [DEPOT_FACT]);
    assert!(facts.contains(&"changed path: `load` now names a different item: `load` is `demo::net::load` after the step (before the step: the item with the new name `fetch`) at `demo::disk::twice/tail/lhs/callee`".to_string()), "{facts:#?}");
    // A `use` line gives the old name to the renamed item.
    let alias = swap(&pure, &[("use crate::disk::fetch;", "use crate::disk::fetch as load;"), ("fetch(3)", "load(3)")]);
    let (_, facts) = rename_and_other(&run_tree(DEPOT, &alias));
    assert_eq!(facts, ["changed path: `load` did not follow the rename to `fetch`: a `use` line gives the old name `load` to the renamed item `demo::disk::fetch` at `demo::ui::show/tail/args[1]/callee`"]);
    // A macro call can make an item with the old name. The syntax phase cannot list this item.
    let alias = "macro_rules! alias { ($n:ident) => { pub type $n = Basket; }; }\nalias!(Cart);\n";
    let made = format!("{}{alias}", swap(&swap(CART, &CART_RENAME), &[("c: &Basket", "c: &Cart")]));
    let (_, facts) = rename_and_other(&run_tree(CART, &made));
    assert!(facts.iter().any(|text| text.starts_with("parameter of `demo::a` `c: &Cart` did not follow the rename to `Basket`: a call of the macro `alias` has the old name `Cart` in its input and can make an item with this name")), "{facts:#?}");
}

/// Design 51, rule R4: no fact has two equal texts, and a place with no difference has no fact.
#[test]
fn no_fact_of_a_rename_step_has_two_equal_texts() {
    let pure = swap(DEPOT, &DEPOT_RENAME);
    let steps = [pure.clone(), swap(&pure, &[("{ fetch(k) * 2 }", "{ load(k) * 2 }")]), swap(&pure, &[("use crate::disk::fetch;", "use crate::disk::load;")]), swap(&pure, &[("l.load()", "l.fetch()")])];
    for after in steps {
        let report = run_tree(DEPOT, &after);
        for text in plain_texts(&report) {
            let equal = text.split_once("` → `").is_some_and(|(old, new)| old.rsplit_once('`').is_some_and(|(_, old)| new.split('`').next() == Some(old)));
            assert!(!equal, "{text}");
        }
    }
}

/// Design 51, the negative tests: each pure rename of the cases of batch 9a keeps its facts. A
/// rename with only sure sites has no count. A member rename with a free name has the count,
/// and the input of `matches!` is one site by name.
#[test]
fn the_pure_renames_of_batch_9a_keep_their_facts() {
    let by_name = |count: usize| format!("{count} site(s) by name (the syntax tree does not give the type of the receiver or the target of the path)");
    let cases: [(&str, &str, &[&str], Option<usize>); 7] = [
        ("fn scale(x: u32) -> u32 { x * 2 }\npub fn a(x: u32) -> u32 { scale(x) }\npub fn b(x: u32) -> u32 { scale(x) + 1 }\npub fn c(x: u32) -> u32 { scale(scale(x)) }\n", "scale=double", &["renamed `demo::scale` → `demo::double`"], None),
        ("#[derive(Debug, Clone)]\npub struct Cart { pub items: Vec<u32> }\nimpl Cart { pub fn new() -> Cart { Cart { items: Vec::new() } } }\npub fn a(c: &Cart) -> usize { c.items.len() }\npub fn b() -> Cart { Cart::new() }\npub fn c(list: Vec<Cart>) -> usize { list.len() }\n", "Cart=Basket", &["renamed `demo::Cart` → `demo::Basket`"], None),
        ("pub struct Tank { pub level: u32, pub cap: u32 }\npub fn a(t: &Tank) -> u32 { t.level + 1 }\npub fn b(t: &Tank) -> bool { t.level < t.cap }\npub fn c(t: &mut Tank) { t.level = 0; }\npub fn d() -> Tank { Tank { level: 1, cap: 2 } }\n", "level=depth", &["renamed `demo::Tank.level` → `demo::Tank.depth`"], Some(3)),
        ("pub enum Mode { Slow, Quick, Off }\npub fn a() -> Mode { Mode::Slow }\npub fn b(m: &Mode) -> bool { matches!(m, Mode::Slow) }\npub fn c(x: u32) -> Mode { if x > 1 { Mode::Slow } else { Mode::Off } }\n", "Slow=Careful", &["renamed `demo::Mode::Slow` → `demo::Mode::Careful`"], Some(1)),
        ("pub struct Cart { pub items: Vec<u32> }\nimpl Cart {\n    pub fn total(&self) -> u32 { self.items.iter().sum() }\n}\npub fn a(c: &Cart) -> u32 { c.total() }\npub fn b(c: &Cart) -> u32 { c.total() + 1 }\npub fn c(c: &Cart) -> bool { c.total() > 9 }\n", "total=sum", &["renamed `demo::Cart::total` → `demo::Cart::sum`"], Some(3)),
        ("const LIMIT: u32 = 10;\npub fn a(x: u32) -> bool { x > LIMIT }\npub fn b(x: u32) -> u32 { x + LIMIT }\npub fn c() -> u32 { LIMIT * 2 }\n", "LIMIT=CEILING", &["renamed `demo::LIMIT` → `demo::CEILING`"], None),
        ("mod fees {\n    pub fn charge(x: u32) -> u32 { x + 2 }\n    pub fn refund(x: u32) -> u32 { x - 2 }\n}\npub fn a(x: u32) -> u32 { fees::charge(x) }\npub fn b(x: u32) -> u32 { fees::refund(x) }\npub fn c(x: u32) -> u32 { fees::charge(fees::refund(x)) }\n", "fees=costs", &["renamed `demo::fees` → `demo::costs`"], None),
    ];
    for (before, words, renames, count) in cases {
        let (old, new) = words.split_once('=').unwrap();
        let report = run_tree(before, &before.replace(old, new));
        let (found, other) = rename_and_other(&report);
        assert_eq!((found.iter().map(String::as_str).collect::<Vec<_>>(), other), (renames.to_vec(), Vec::new()), "{report:#?}");
        assert_eq!(note_of(&report, renames[0]).map(str::to_string), count.map(by_name), "{report:#?}");
    }
    // A local and a parameter.
    let local = "pub fn f(a: u32, b: u32) -> u32 {\n    let sum = a + b;\n    let twice = sum * 2;\n    let more = sum + twice;\n    more + sum\n}\n";
    assert_eq!(plain_texts(&run_tree(local, &local.replace("sum", "total"))), ["`demo::f`: renamed local `sum` → `total`, 4 edit(s)"]);
    let param = "pub fn f(count: u32, b: u32) -> u32 {\n    let twice = count * 2;\n    let more = count + twice;\n    more + count + b\n}\n";
    assert_eq!(plain_texts(&run_tree(param, &param.replace("count", "n"))), ["parameter `count` of `demo::f` renamed to `n`: `u32`", "`demo::f`: renamed local `count` → `n`, 3 edit(s)"]);
}

// ---- Batch 9a3 (design 52): the own tokens of a node, a sure binding, a `use` line in a block ----

/// The example crate of design 52.
const YARD: &str = r#"pub mod gauge {
    pub const LOW: u32 = 1;
    pub const HIGH: u32 = 9;
    #[derive(Clone, Copy)]
    pub enum Mode { Idle, Busy, Halt }
    pub struct Meter { pub level: u32 }
    pub fn read(m: &Meter) -> u32 { m.level }
}
pub mod spare {
    pub fn read(k: u32) -> u32 { k + 50 }
    pub fn probe(k: u32) -> u32 { k + 70 }
}
pub mod work {
    use crate::gauge::{read, Meter, Mode, Mode::*, HIGH, LOW};
    pub fn n1a(n: u32) -> u32 { 'p: loop { let _k: u32 = 'q: loop { if n > 1 { break 'p 1; } else { break 'q 5; } }; } }
    pub fn n1b(n: u32) -> u32 { let s = (0..n).map(u64::from).sum::<u64>(); s as u32 }
    pub fn n1c(n: u32) -> u32 { let v = vec![n]; let f = move || v.len() as u32 + 1; f() }
    pub fn n1d(n: u32) -> u32 { let f = |k: u32| -> u64 { (k + 1).into() }; f(n) as u32 }
    pub fn n1e(n: u32) -> u32 { let t = async move { n + 1 }; drop(t); n }
    pub fn n2a(m: Mode) -> u32 { match m { Idle => 0, Busy => 1, _ => 2 } }
    pub fn n2b(k: u32) -> u32 { match k { LOW => 0, _ => 1 } }
    pub fn n2d(o: Option<u32>) -> u32 { match o { None => 0, Some(k) => k } }
    pub fn n2e(n: u32) -> u32 { let total = n + 1; total * 2 }
    pub fn n2f(o: Option<u32>) -> u32 { match o { Some(k) => k, _ => HIGH } }
    pub fn w1(m: &Meter) -> u32 { read(m) }
    pub fn w2() -> u32 { use crate::spare::read; read(4) }
    pub fn w3() -> u32 { use crate::spare::*; read(5) }
}
"#;

/// The fact lines of a step in the default mode: the text and the category of each fact. A
/// fact of the public API does not count.
fn tree_lines(before: &str, after: &str) -> Vec<String> {
    report_lines(&run_tree(before, after))
}

fn report_lines(report: &StepReport) -> Vec<String> {
    let lines = report.facts.iter().zip(plain_texts(report)).filter(|(_, text)| !text.starts_with("public API: "));
    lines.map(|(fact, text)| format!("{text} [{}]", format!("{:?}", fact.category).to_lowercase())).collect()
}

/// The report of the example crate with each pair replaced. The crate has the edition 2021
/// (an `async` block).
fn yard_report(pairs: &[(&str, &str)]) -> StepReport {
    let manifest = format!("{MANIFEST}edition = \"2021\"\n");
    let (_d1, old) = load_with(&manifest, YARD);
    let (_d2, new) = load_with(&manifest, &swap(YARD, pairs));
    let mut step = step(Vec::new());
    step.compiled = false;
    verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &NoOracle, &NoOracle, true).report
}

/// The fact lines of the example crate with each pair replaced.
fn yard_lines(pairs: &[(&str, &str)]) -> Vec<String> {
    report_lines(&yard_report(pairs))
}

/// Design 52.1, the cases of defect N1: an own token and a child of a node change in one step.
/// Before: the fact had only the child, and the own token was in no fact.
#[test]
fn a_changed_own_token_with_a_changed_child_is_in_a_fact() {
    // A node with no block: one fact with the full old node and the full new node.
    assert_eq!(yard_lines(&[("break 'p 1;", "break 'q 2;")]), ["changed break: `break 'p 1` → `break 'q 2` at `demo::work::n1a/S0/body/S0/init/body/S0/then/S0` [semantics]"]);
    assert_eq!(yard_lines(&[("(0..n).map(u64::from).sum::<u64>()", "(1..n).map(u64::from).sum::<u128>()")]), ["changed expression `(0..n).map(u64::from).sum::<u64>()` → `(1..n).map(u64::from).sum::<u128>()` at `demo::work::n1b/S0/init` [semantics]"]);
    assert_eq!(yard_lines(&[("move || v.len() as u32 + 1", "|| v.len() as u32 + 2")]), ["changed expression `move || v.len() as u32 + 1` → `|| v.len() as u32 + 2` at `demo::work::n1c/S1/init` [semantics]"]);
    // A node with a block: the child keeps its fact, and the node has one more fact with its
    // own tokens.
    assert_eq!(
        yard_lines(&[("|k: u32| -> u64 { (k + 1).into() }", "|k: u32| -> u128 { (k + 2).into() }")]),
        ["changed literal `1` → `2` at `demo::work::n1d/S0/init/body/tail/recv/rhs` [semantics]", "changed closure_block: `|k: u32| -> u64 {…}` → `|k: u32| -> u128 {…}` at `demo::work::n1d/S0/init` [semantics]"]
    );
    assert_eq!(
        yard_lines(&[("async move { n + 1 }", "async { n + 2 }")]),
        ["changed literal `1` → `2` at `demo::work::n1e/S0/init/body/tail/rhs` [semantics]", "changed async: `async move {…}` → `async {…}` at `demo::work::n1e/S0/init` [semantics]"]
    );
}

/// Design 52.1: the rule of the own tokens holds for each kind of node. Four more forms: a
/// label of a loop, a label of a `for` loop, an attribute of a `match` arm, and an attribute
/// of a statement.
#[test]
fn the_rule_of_the_own_tokens_holds_for_each_kind_of_node() {
    let lines = |before: &str, after: &str| tree_lines(before, after);
    assert_eq!(
        lines("pub fn f(a: u32) -> u32 { let mut k = a; 'p: loop { k += 1; if k > 9 { break; } } k }\n", "pub fn f(a: u32) -> u32 { let mut k = a; 'q: loop { k += 2; if k > 9 { break; } } k }\n"),
        ["changed literal `1` → `2` at `demo::f/S1/body/S0/rhs` [semantics]", "changed loop: `'p: loop {…}` → `'q: loop {…}` at `demo::f/S1` [semantics]"]
    );
    assert_eq!(
        lines("pub fn f(v: &[u32]) -> u32 { let mut s = 0; 'a: for x in v { s += x + 1; } s }\n", "pub fn f(v: &[u32]) -> u32 { let mut s = 0; for x in v { s += x + 2; } s }\n"),
        ["changed literal `1` → `2` at `demo::f/S1/body/S0/rhs/rhs` [semantics]", "changed for: `'a: for … in … {…}` → `for … in … {…}` at `demo::f/S1` [semantics]"]
    );
    assert_eq!(
        lines("pub fn f(a: u32) -> u32 { match a { #[cfg(all())] 1 => 2, _ => 3 } }\n", "pub fn f(a: u32) -> u32 { match a { #[cfg(any())] 1 => 4, _ => 3 } }\n"),
        ["changed literal `2` → `4` at `demo::f/S0/arm[0].body` [semantics]", "changed match: `match … { #[cfg(all())] … => …, … => … }` → `match … { #[cfg(any())] … => …, … => … }` at `demo::f/S0` [semantics]"]
    );
    assert_eq!(
        lines("pub fn f(a: u32) -> u32 { #[allow(unused)] let b = a + 1; b }\n", "pub fn f(a: u32) -> u32 { #[allow(dead_code)] let b = a + 2; b }\n"),
        ["changed let: `#[allow(unused)] let b = a + 1;` → `#[allow(dead_code)] let b = a + 2;` at `demo::f/S0` [semantics]"]
    );
}

/// Design 52.1, the negative tests: an edit of one own token has the fact of batch 9a2, and
/// an edit of one child with equal own tokens has the narrow fact.
#[test]
fn one_changed_own_token_or_one_changed_child_keeps_its_fact() {
    assert_eq!(yard_lines(&[("break 'p 1;", "break 'q 1;")]), ["changed break: `break 'p 1` → `break 'q 1` at `demo::work::n1a/S0/body/S0/init/body/S0/then/S0` [semantics]"]);
    assert_eq!(tree_lines("pub fn f(a: u32) -> u32 { a + 1 }\n", "pub fn f(a: u32) -> u32 { a + 2 }\n"), ["changed literal `1` → `2` at `demo::f/tail/rhs` [semantics]"]);
    // One changed own token of a node with a block: the full text of the node.
    assert_eq!(
        tree_lines("pub fn f(a: u32) -> u32 { let v = vec![a]; let f = move || { v.len() as u32 + 1 }; f() }\n", "pub fn f(a: u32) -> u32 { let v = vec![a]; let f = || { v.len() as u32 + 1 }; f() }\n"),
        ["changed closure_block: `move || { v.len() as u32 + 1 }` → `|| { v.len() as u32 + 1 }` at `demo::f/S1/init` [semantics]"]
    );
    // An inner attribute of the body of an item is in the fact of the attributes of the item.
    assert_eq!(
        tree_lines("pub fn f(a: u32) -> u32 { #![allow(unused)] let b = a + 1; b }\n", "pub fn f(a: u32) -> u32 { #![allow(dead_code)] let b = a + 2; b }\n"),
        ["attributes of `demo::f`: changed `#![allow(unused)]` → `#![allow(dead_code)]` [attribute]", "changed literal `1` → `2` at `demo::f/S0/init/rhs` [semantics]"]
    );
}

/// Design 52.1: the `;` of a statement is an own token. A fact with a second difference shows
/// the `;` that only one side has. Before: the fact had only the child.
#[test]
fn the_semicolon_of_a_statement_shows_in_a_fact_with_a_second_difference() {
    let g = "pub fn g(k: u32) -> u32 { k }\n";
    let lines = |before: &str, after: &str| tree_lines(&format!("{g}{before}\n"), &format!("{g}{after}\n"));
    assert_eq!(lines("pub fn f() { let a = 1; g(a) }", "pub fn f() { let a = 1; g(a + 1); }"), ["changed call: `g(a)` → `g(a + 1);` at `demo::f/tail` [semantics]"]);
    assert_eq!(lines("pub fn f() -> u32 { let a = 1; g(a); }", "pub fn f() -> u32 { let a = 1; g(a + 1) }"), ["changed call: `g(a);` → `g(a + 1)` at `demo::f/S1` [semantics]"]);
    // A node with a block: the child keeps its fact.
    assert_eq!(
        lines("pub fn f(c: bool) { let a = 1; if c { g(a); } }", "pub fn f(c: bool) { let a = 1; if c { g(a + 1); }; }"),
        ["changed expression `a` → `a + 1` at `demo::f/S1/then/S0/args[0]` [semantics]", "changed if: `if … {…}` → `if … {…};` at `demo::f/S1` [semantics]"]
    );
    // Negative: the `;` on the two sides does not show, and the `;` alone has the fact of
    // batch 9a2.
    assert_eq!(lines("pub fn f() { let a = 1; g(a); }", "pub fn f() { let a = 1; g(a + 1); }"), ["changed expression `a` → `a + 1` at `demo::f/S1/args[0]` [semantics]"]);
    assert_eq!(lines("pub fn f() { let a = 1; g(a) }", "pub fn f() { let a = 1; g(a); }"), ["moved call: `g(a)` from `tail` to `S1` in `demo::f` [semantics]"]);
}

/// Design 52.2: a token difference with no meaning in the grammar gives no fact. Before: each
/// of these steps gave a fact.
#[test]
fn a_token_with_no_meaning_in_the_grammar_gives_no_fact() {
    let cases = [
        // E1: the comma before the closing delimiter of a list.
        ("pub fn g(a: u32, b: u32) -> u32 { a + b }\npub fn f(a: u32) -> u32 { g(a, 1) }\n", "pub fn g(a: u32, b: u32,) -> u32 { a + b }\npub fn f(a: u32) -> u32 { g(a, 1,) }\n"),
        ("pub struct S { pub a: u32, pub b: u32 }\npub enum E { A, B }\npub fn f() -> S { S { a: 1, b: 2 } }\n", "pub struct S { pub a: u32, pub b: u32, }\npub enum E { A, B, }\npub fn f() -> S { S { a: 1, b: 2, } }\n"),
        ("pub fn f<T, U>(a: T, _b: U) -> T { a }\npub fn g() -> Vec<u8> { Vec::<u8>::new() }\n", "pub fn f<T, U,>(a: T, _b: U,) -> T { a }\npub fn g() -> Vec<u8,> { Vec::<u8,>::new() }\n"),
        // E1 for the last arm, and E2: the comma after an arm with a block.
        ("pub fn f(a: u32) -> u32 { match a { 1 => { 2 } 2 => unsafe { 3 }, _ => 3 } }\n", "pub fn f(a: u32) -> u32 { match a { 1 => { 2 }, 2 => unsafe { 3 } _ => 3, } }\n"),
        // E3: an empty statement.
        ("pub fn f(a: u32) -> u32 { let b = a + 1; b }\n", "pub fn f(a: u32) -> u32 { let b = a + 1;; b }\n"),
        // E4: the comma after the last predicate of a `where` clause.
        ("pub fn f<T>(a: T) -> T where T: Clone { a }\n", "pub fn f<T>(a: T) -> T where T: Clone, { a }\n"),
    ];
    for (before, after) in cases {
        assert_eq!(tree_lines(before, after), Vec::<String>::new(), "{after}");
        assert_eq!(tree_lines(after, before), Vec::<String>::new(), "{after}");
    }
    // The comma after an arm is in the part of the body of the arm. A body gets a block and
    // its comma goes: the `match` has no fact of its own tokens.
    assert_eq!(
        tree_lines("pub fn f(a: u32) -> u32 { match a { 1 => a + 2, _ => 3 } }\n", "pub fn f(a: u32) -> u32 { match a { 1 => { a + 2 } _ => 3 } }\n"),
        ["changed block: `a + 2` → `{ a + 2 }` at `demo::f/S0/arm[0].body` [semantics]"]
    );
    assert_eq!(tree_lines("pub fn f(a: u32) -> (u32,) { (a,) }\n", "pub fn f(a: u32) -> (u32,) { ((a),) }\n").len(), 1);
    assert_eq!(
        tree_lines("pub fn f(a: u32) -> String { format!(\"{} {}\", a, a) }\n", "pub fn f(a: u32) -> String { format!(\"{} {}\", a, a,) }\n"),
        ["replace macro input `format!(\"{} {}\", a, a)` → `format!(\"{} {}\", a, a,)` at `demo::f/tail` [macro]"]
    );
}

/// The rename of the example crate of design 52: `gauge::read` is now `gauge::take`.
const YARD_RENAME: [(&str, &str); 3] = [("pub fn read(m: &Meter) -> u32 { m.level }", "pub fn take(m: &Meter) -> u32 { m.level }"), ("use crate::gauge::{read, Meter,", "use crate::gauge::{take, Meter,"), ("{ read(m) }", "{ take(m) }")];
const YARD_FACT: &str = "renamed `demo::gauge::read` → `demo::gauge::take` [rename]";

/// The fact lines of the rename of the example crate, with more pairs before the rename
/// (`both`: in the two trees) and after it (`only_after`: only in the tree after the step).
fn yard_rename_lines(both: &[(&str, &str)], only_after: &[(&str, &str)]) -> Vec<String> {
    let manifest = format!("{MANIFEST}edition = \"2021\"\n");
    let before = swap(YARD, both);
    let (_d1, old) = load_with(&manifest, &before);
    let (_d2, new) = load_with(&manifest, &swap(&swap(&before, &YARD_RENAME), only_after));
    let mut run = step(Vec::new());
    run.compiled = false;
    report_lines(&verify_step_with(&StepInput { step: &run, before: &old, after: &new }, &NoOracle, &NoOracle, true).report)
}

/// Design 52.3, the cases of defect N3: a name from a `use` line in a block of a body is not
/// the item with the same name of the module. Before: the pure rename gave two false facts
/// (`read` still names the old item), and the two edits were under the rename fact.
#[test]
fn a_name_from_a_use_line_in_a_block_is_not_a_site_of_a_rename_of_a_module_item() {
    assert_eq!(yard_rename_lines(&[], &[]), [YARD_FACT]);
    assert_eq!(
        yard_rename_lines(&[], &[("use crate::spare::read; read(4)", "use crate::spare::{read, probe as take}; take(4)")]),
        [YARD_FACT, "changed path `read` → `take` at `demo::work::w2/tail/callee` [semantics]", "changed item: `use crate::spare::read;` → `use crate::spare::{read, probe as take};` at `demo::work::w2/S0` [semantics]"]
    );
    // A glob import of a module of the workspace: the tool lists the names of the module.
    assert_eq!(yard_rename_lines(&[], &[("use crate::spare::*; read(5)", "use crate::spare::*; take(5)")]), [YARD_FACT, "changed path `read` → `take` at `demo::work::w3/tail/callee` [semantics]"]);
}

/// Design 52.3: an alias and a list in a `use` line of a block, a `use` line in an outer
/// block of the place, and a path through a module that a `use` line of a block gives.
#[test]
fn each_form_of_a_use_line_in_a_block_gives_the_scope_of_its_names() {
    // An alias with the old name for a different item.
    let alias = [("use crate::spare::read; read(4)", "use crate::spare::{probe as read, read as other}; read(4) + other(1)")];
    assert_eq!(yard_rename_lines(&alias, &[]), [YARD_FACT]);
    assert_eq!(yard_rename_lines(&alias, &[("read(4) + other(1)", "take(4) + other(1)")]), [YARD_FACT, "changed path `read` → `take` at `demo::work::w2/tail/lhs/callee` [semantics]"]);
    // The `use` line is in an outer block of the place.
    let outer = [("use crate::spare::read; read(4)", "use crate::spare::read; if true { let k = { read(4) }; k } else { 0 }")];
    assert_eq!(yard_rename_lines(&outer, &[]), [YARD_FACT]);
    assert_eq!(yard_rename_lines(&outer, &[("{ read(4) }", "{ take(4) }")]), [YARD_FACT, "changed path `read` → `take` at `demo::work::w2/S1/then/S0/init/body/tail/callee` [semantics]"]);
    // An inner block with its own `use` line comes before the outer block.
    let inner = [("use crate::spare::read; read(4)", "use crate::spare::read; { use crate::gauge::Meter; let m = Meter { level: 1 }; { use crate::gauge::read; read(&m) } }")];
    assert_eq!(yard_rename_lines(&inner, &[("use crate::gauge::read; read(&m)", "use crate::gauge::take; take(&m)")]), [YARD_FACT]);
    // Two places of the name with different scopes in one item: the site table has no place
    // of a token, so the place of the renamed item has its own fact (a limit, design 52.3).
    let mixed = [("use crate::spare::read; read(4)", "use crate::spare::read; read(4) + { use crate::gauge::{read, Meter}; read(&Meter { level: 1 }) }")];
    assert_eq!(
        yard_rename_lines(&mixed, &[("use crate::gauge::{read, Meter}; read(&Meter", "use crate::gauge::{take, Meter}; take(&Meter")]),
        [YARD_FACT, "changed path `read` → `take` at `demo::work::w2/tail/rhs/body/tail/callee` [semantics]"]
    );
    // A module with the name of the owner of the renamed item, through a `use` line of a block.
    let module = [("use crate::spare::read; read(4)", "use crate::spare as gauge; gauge::read(4)")];
    assert_eq!(yard_rename_lines(&module, &[]), [YARD_FACT]);
    assert_eq!(yard_rename_lines(&module, &[("gauge::read(4)", "gauge::take(4)")]), [YARD_FACT, "changed path `gauge::read` → `gauge::take` at `demo::work::w2/tail/callee` [semantics]"]);
}

/// Design 52.3: a glob import of an external crate in a block. The tool cannot list its
/// names, so a bare name of the block is a site by name (class 4): it follows the rename only
/// if the name is free.
#[test]
fn a_glob_import_of_an_external_crate_in_a_block_makes_a_bare_name_a_site_by_name() {
    let before = "pub mod gauge {\n    pub fn read(k: u32) -> u32 { k }\n}\npub mod work {\n    use crate::gauge::read;\n    pub fn w() -> u32 { use std::cmp::*; read(5) }\n}\n";
    let report = run_tree(before, &before.replace("read", "take"));
    assert_eq!(report_lines(&report), ["renamed `demo::gauge::read` → `demo::gauge::take` [rename]"]);
    assert_eq!(note_of(&report, "renamed `demo::gauge::read`"), Some("1 site(s) by name (the syntax tree does not give the type of the receiver or the target of the path)"));
    // A second declaration has the name: the name is not free, and the place has its own fact.
    let taken = format!("{before}pub mod spare {{\n    pub fn read(k: u32) -> u32 {{ k + 1 }}\n}}\n");
    let after = taken.replacen("pub fn read(k: u32) -> u32 { k }", "pub fn take(k: u32) -> u32 { k }", 1).replace("use crate::gauge::read;", "use crate::gauge::take;").replace("read(5)", "take(5)");
    assert_eq!(tree_lines(&taken, &after), ["renamed `demo::gauge::read` → `demo::gauge::take` [rename]", "changed path `read` → `take` at `demo::work::w/tail/callee` [semantics]"]);
}

/// Design 52.3, the negative test: a `use` line of the renamed item in a block is a sure site,
/// and the name that it gives is a sure site. The rename has no site by name.
#[test]
fn a_use_line_of_the_renamed_item_in_a_block_is_a_sure_site() {
    let before = "pub mod gauge {\n    pub fn read(k: u32) -> u32 { k }\n    pub fn other(k: u32) -> u32 { k }\n}\npub mod spare {\n    pub fn read(k: u32) -> u32 { k + 1 }\n}\npub mod work {\n    pub fn a() -> u32 { use crate::gauge::read; read(5) }\n    pub fn b() -> u32 { use crate::gauge::{other, read as get}; get(5) + other(1) }\n    pub fn c() -> u32 { use crate::gauge::*; if true { read(5) } else { 0 } }\n}\n";
    let after = before.replacen("pub fn read(k: u32) -> u32 { k }", "pub fn take(k: u32) -> u32 { k }", 1).replace("use crate::gauge::read; read(5)", "use crate::gauge::take; take(5)").replace("read as get", "take as get").replace("{ read(5) } else", "{ take(5) } else");
    let report = run_tree(before, &after);
    assert_eq!(report_lines(&report), ["renamed `demo::gauge::read` → `demo::gauge::take` [rename]"]);
    assert_eq!(note_of(&report, "renamed `demo::gauge::read`"), None);
}

/// Design 52.2, the cases of defect N2: an identifier pattern that names a variant or a
/// constant is a path. Its change is a fact of the code, not a rename of a local. Before:
/// each of the four edits was "renamed local … [rename]".
#[test]
fn an_identifier_pattern_that_is_not_a_sure_binding_is_not_a_renamed_local() {
    assert_eq!(yard_lines(&[("Idle => 0, Busy => 1", "Halt => 0, Busy => 1")]), ["changed pattern `Idle` → `Halt` at `demo::work::n2a/S0/arm[0].pat` [semantics]"]);
    assert_eq!(yard_lines(&[("LOW => 0, _ => 1", "HIGH => 0, _ => 1")]), ["changed pattern `LOW` → `HIGH` at `demo::work::n2b/S0/arm[0].pat` [semantics]"]);
    // A constant becomes a binding that matches each value.
    assert_eq!(yard_lines(&[("LOW => 0, _ => 1", "low => 0, _ => 1")]), ["changed pattern `LOW` → `low` at `demo::work::n2b/S0/arm[0].pat` [semantics]"]);
    assert_eq!(yard_lines(&[("None => 0, Some(k) => k", "Nil => 0, Some(k) => k")]), ["changed pattern `None` → `Nil` at `demo::work::n2d/S0/arm[0].pat` [semantics]"]);
    // The controls: a rename of a local stays one fact.
    assert_eq!(yard_lines(&[("let total = n + 1; total * 2", "let sum = n + 1; sum * 2")]), ["`demo::work::n2e`: renamed local `total` → `sum`, 2 edit(s) [rename]"]);
    assert_eq!(yard_lines(&[("Some(k) => k, _ => HIGH", "Some(j) => j, _ => HIGH")]), ["`demo::work::n2f`: renamed local `k` → `j`, 2 edit(s) [rename]"]);
    // A second edit in the body does not make the pattern a local.
    assert_eq!(
        yard_lines(&[("LOW => 0, _ => 1", "HIGH => 0, _ => 2")]),
        ["changed pattern `LOW` → `HIGH` at `demo::work::n2b/S0/arm[0].pat` [semantics]", "changed literal `1` → `2` at `demo::work::n2b/S0/arm[1].body` [semantics]"]
    );
}

/// Design 52.2, each condition of a sure binding.
#[test]
fn each_condition_of_a_sure_binding_decides_the_fact_of_a_local() {
    let lines = |before: &str, after: &str| tree_lines(before, after);
    // Condition 1: `ref`, `mut`, or `@` with the identifier. The name can start with an
    // upper-case letter.
    let with_ref = "pub fn f(o: Option<u32>) -> u32 { match o { Some(ref V) => *V, _ => 0 } }\n";
    assert_eq!(lines(with_ref, &with_ref.replace('V', "W")), ["`demo::f`: renamed local `V` → `W`, 2 edit(s) [rename]"]);
    let with_mut = "pub fn f(o: Option<u32>) -> u32 { match o { Some(mut V) => { V += 1; V } _ => 0 } }\n";
    assert_eq!(lines(with_mut, &with_mut.replace('V', "W")), ["`demo::f`: renamed local `V` → `W`, 3 edit(s) [rename]"]);
    let with_at = "pub fn f(k: u32) -> u32 { match k { V @ 1..=9 => V, _ => 0 } }\n";
    assert_eq!(lines(with_at, &with_at.replace('V', "W")), ["`demo::f`: renamed local `V` → `W`, 2 edit(s) [rename]"]);
    // Condition 2: an upper-case name with no known item is not a sure binding.
    let plain = "pub fn f(k: u32) -> u32 { match k { 0 => 1, V => V } }\n";
    assert_eq!(lines(plain, &plain.replace('V', "W")), ["changed pattern `V` → `W` at `demo::f/S0/arm[1].pat` [semantics]", "changed path `V` → `W` at `demo::f/S0/arm[1].body` [semantics]"]);
    let local = "pub fn f(n: u32) -> u32 { let total = n + 1; total * 2 }\n";
    assert_eq!(lines(local, &local.replace("total", "Total")), ["changed pattern `total` → `Total` at `demo::f/S0/pat` [semantics]", "changed path `total` → `Total` at `demo::f/tail/lhs` [semantics]"]);
    // Condition 2: a constant of the module has the new name. The new pattern is a use of it.
    let scope = "#[allow(non_upper_case_globals)]\npub const low: u32 = 3;\npub fn f(k: u32) -> u32 { match k { other => other + 1 } }\n";
    assert_eq!(
        lines(scope, &scope.replace("other", "low")),
        ["changed pattern `other` → `low` at `demo::f/S0/arm[0].pat` [semantics]", "changed path `other` → `low` at `demo::f/S0/arm[0].body/lhs` [semantics]"]
    );
    // Condition 2: the resolution is not sure, and a constant of the workspace has the new name.
    let far = format!("pub mod far {{\n    #[allow(non_upper_case_globals)]\n    pub const sum: u32 = 3;\n}}\n{local}");
    assert_eq!(lines(&far, &far.replace("total", "sum")), ["changed pattern `total` → `sum` at `demo::f/S0/pat` [semantics]", "changed path `total` → `sum` at `demo::f/tail/lhs` [semantics]"]);
    // A constant in the body has the new name.
    let inner = "pub fn f(k: u32) -> u32 { const low: u32 = 3; match k { other => other + 1 } }\n";
    assert_eq!(lines(inner, &inner.replace("other", "low")).len(), 2);
    // A function is not an item that a pattern can name: the local hides it.
    let function = format!("pub fn read() -> u32 {{ 1 }}\n{local}");
    assert_eq!(lines(&function, &function.replace("total", "read")), ["`demo::f`: renamed local `total` → `read`, 2 edit(s) [rename]"]);
}

/// Design 52.2: an identifier pattern that is a sure site of a rename of an item is under the
/// rename fact, as each other path. A unit struct is an item that a pattern can name.
#[test]
fn a_pattern_with_the_name_of_a_renamed_item_is_under_the_rename_fact() {
    let constant = "pub const LOW: u32 = 0;\npub fn f(k: u32) -> u32 { match k { LOW => 0, _ => 1 } }\n";
    assert_eq!(tree_lines(constant, &constant.replace("LOW", "FLOOR")), ["renamed `demo::LOW` → `demo::FLOOR` [rename]"]);
    let variant = "pub enum Mode { Idle, Busy }\nuse Mode::*;\npub fn f(m: Mode) -> u32 { match m { Idle => 0, Busy => 1 } }\n";
    assert_eq!(tree_lines(variant, &variant.replace("Idle", "Halt")), ["renamed `demo::Mode::Idle` → `demo::Mode::Halt` [rename]"]);
    let unit = "pub struct Unit;\npub fn f(u: Unit) -> u32 { let k = Unit; match k { Unit => 1 } }\n";
    assert_eq!(tree_lines(unit, &unit.replace("Unit", "Solo")), ["renamed `demo::Unit` → `demo::Solo` [rename]"]);
}

/// Design 52.2: a `rename-local` claim needs a sure binding on the two sides.
#[test]
fn a_rename_local_claim_needs_a_sure_binding() {
    let before = "pub const LOW: u32 = 0;\npub const HIGH: u32 = 9;\npub fn f(k: u32) -> u32 { let LOW = k else { return 1 }; 0 }\n";
    let after = before.replace("let LOW", "let HIGH");
    let report = run(before, &after, vec![claim("c1", "rename-local", Some("demo::f"), serde_json::json!({"from": "LOW", "to": "HIGH"}))], &[("demo::f", equal("n0"))], None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Malformed { reason } if reason.contains("`LOW` is not a sure binding")), "{:?}", status_of(&report, "c1"));
    // The control: a local with a sure binding.
    let before = "pub fn f(k: u32) -> u32 { let total = k + 1; total * 2 }\n";
    let report = run(before, &before.replace("total", "sum"), vec![claim("c1", "rename-local", Some("demo::f"), serde_json::json!({"from": "total", "to": "sum"}))], &[("demo::f", equal("n0"))], None);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
}
