//! Tests of the verifier with fake oracles.

use std::collections::BTreeMap;

use semilla_core::report::Effect;
use semilla_core::{ChangeSet, Claim, ClaimStatus, Class, Group, GroupKind, Role, Step, Subgroup};
use semilla_syntax::Workspace;

use super::*;

const MANIFEST: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";

/// A body oracle that returns a fixed status for each item path.
struct FakeBodies(BTreeMap<String, BodyStatus>);

impl BodyOracle for FakeBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.iter().map(|(name, status)| BodyComparison { name: name.clone(), span: None, status: status.clone(), config: Vec::new() }).collect())
    }
}

/// An expression oracle that always returns one effect.
struct FakeExprs(Effect);

impl ExprOracle for FakeExprs {
    fn classify(&self, _: &str, _: &str, _: &BTreeMap<String, String>) -> Option<EffectCheck> {
        Some(EffectCheck { effect: self.0, witness: Some("i = 5, n = 5".into()) })
    }
    fn equivalent(&self, _: &str, _: &str, _: &BTreeMap<String, String>) -> Option<bool> {
        Some(self.0 == Effect::Equivalent)
    }
}

fn load(src: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), MANIFEST).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), src).unwrap();
    let ws = Workspace::load(dir.path()).unwrap();
    (dir, ws)
}

fn claim(id: &str, op: &str, at: Option<&str>, args: serde_json::Value) -> Claim {
    Claim { id: id.into(), op: op.into(), at: at.map(|a| a.parse().unwrap()), args: args.as_object().cloned().unwrap_or_default() }
}

fn step(claims: Vec<Claim>) -> Step {
    Step { index: 0, intent: "test".into(), class: None, claims, tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default() }
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
    assert_eq!(c2.class, Some(Class::ControlFlow));
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
fn neutral_claims_need_a_body_proof() {
    let extract = || vec![claim("c1", "extract-variable", Some("demo::f/tail"), serde_json::json!({"name": "product"}))];
    let proved = run(PLAIN, EXTRACTED, extract(), &[("demo::f", equal("n2"))], None);
    assert_eq!(status_of(&proved, "c1"), &ClaimStatus::Verified { proof: "N2".into() });
    assert!(proved.uncovered.is_empty(), "{:#?}", proved.uncovered);
    assert!(proved.accepted());

    let unproved = run(PLAIN, EXTRACTED, extract(), &[("demo::f", different(false))], None);
    assert!(matches!(status_of(&unproved, "c1"), ClaimStatus::Unproved { .. }));

    // A changed graph is not evidence of a behavior change: the claim is unproved, not refuted.
    let changed_graph = run(PLAIN, EXTRACTED, extract(), &[("demo::f", different(true))], None);
    assert!(matches!(status_of(&changed_graph, "c1"), ClaimStatus::Unproved { .. }));
}

#[test]
fn a_declared_class_must_match() {
    let (_d1, old) = load(PLAIN);
    let (_d2, new) = load(EXTRACTED);
    let mut step = step(vec![claim("c1", "replace-expression", Some("demo::f/tail/lhs"), serde_json::json!({}))]);
    step.class = Some(Class::Neutral);
    let _ = (&old, &new);
    let bodies = FakeBodies(BTreeMap::from([("demo::f".to_string(), different(false))]));
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &bodies, &NoOracle).report;
    assert!(!report.accepted());
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
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Unproved { reason } if reason.contains("split the step")), "{:?}", status_of(&report, "c1"));
    assert!(!report.accepted());
    // D10 (brief 5h, C5): the result line has the status text and the hint. The words "split the
    // step" occur one time.
    let result = report.claims.iter().find(|result| result.claim == "c1").unwrap();
    let line = format!("{}; {}", result.status.text(), result.hint.clone().unwrap_or_default());
    assert_eq!(line.to_lowercase().matches("split the step").count(), 1, "{line}");
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
    assert!(fact.spotlight && fact.class == Some(Class::Logic) && fact.affected == ["demo::g"], "{fact:#?}");
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
    assert!(!report.accepted);
    assert!(report.organization.iter().any(|e| e.contains("refactor group")), "{:#?}", report.organization);
    assert!(report.organization.iter().any(|e| e.contains("Mechanical")), "{:#?}", report.organization);
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
    assert!(!organize::is_open(&suggestion[0], &report.steps));
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
fn without_bodies_a_neutral_claim_is_pending() {
    let report = run(PLAIN, EXTRACTED, vec![claim("c1", "extract-variable", Some("demo::f/tail"), serde_json::json!({"name": "product"}))], &[], None);
    assert!(status_of(&report, "c1").is_pending(), "{:?}", status_of(&report, "c1"));
    assert!(report.uncovered.is_empty());
    assert!(!report.accepted(), "a pending claim is not accepted yet");
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
                    callees: local(calls, &["live", "dead"]),
                    live: Some(local(calls, &["live"])),
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
    assert_eq!(report.claims.iter().find(|c| c.claim == "c5").unwrap().class, None, "a property has no class");

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
    assert!(result(&report, "c7").after.as_deref().is_some_and(|t| t.contains("has a reachable call to `demo::wide`. Semilla does not run the test and does not check what the test asserts.")), "{:?}", result(&report, "c7"));
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
    let result = |claim: &str, status: ClaimStatus| ClaimResult { claim: claim.into(), step: 0, status, class: None, effect: None, witness: None, hint: Some("the hint".into()), before: None, after: None, affected: Vec::new(), complete_calls: Vec::new() };
    let mut report = semilla_core::StepReport::default();
    report.claims = vec![
        result("c1", ClaimStatus::Refuted { reason: "external calls outside the list of `calls`: `std::fs::write` in `demo::save` (calls to `core` and `alloc` are always allowed)".into() }),
        result("c2", ClaimStatus::Refuted { reason: "implicit panic sites: division by zero in `demo::share`".into() }),
        result("c3", ClaimStatus::Refuted { reason: "the test `demo::tests::t` does not call `demo::a::b`".into() }),
        result("c4", ClaimStatus::Malformed { reason: "the entry `` of `calls` is not a path. Each entry is a path of full segments".into() }),
        result("c5", ClaimStatus::Unproved { reason: "the MIR of `demo::f` is different (the first difference is in the block 2); split the step".into() }),
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
            "  ? c5 unproved: the MIR of `demo::f` is different",
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
    assert!(report.steps[0].notes.iter().any(|n| n.contains("phase 2 tries the proof")), "{:?}", report.steps[0].notes);
    assert!(report.pairs.is_empty(), "{:?}", report.pairs);
    // Phase 2 (the MIR facts are available) with no identity proof: the warning shows.
    let (report, _) = verify_changeset(&set, &mut loader, &MirFacts(Vec::new()), &NoOracle, options);
    assert_eq!(report.pairs.len(), 1, "{:?}", report.pairs);
    assert_eq!((report.pairs[0].old.as_str(), report.pairs[0].new.as_str()), ("demo::scale", "demo::multiply"));
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
fn a_neutral_claim_on_a_function_without_a_mir_body_is_unproved() {
    let after = TWINS.replace("    '\\\\'\n", "    let c = '\\\\';\n    c\n");
    let claims = vec![claim("c1", "extract-variable", Some("demo::sep#0/tail"), serde_json::json!({"name": "c"}))];
    let report = run(TWINS, &after, claims, &[("demo::other", equal("n0"))], None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Unproved { reason } if reason.contains("no MIR body")), "{:?}", status_of(&report, "c1"));
    assert!(!report.accepted());
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
    // The proof is the highest level of all bodies: `run` is equal at N1.
    assert_eq!(status_of(&report, "c1"), &ClaimStatus::Verified { proof: "N1".into() });
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
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Unproved { reason } if reason.contains("demo::get")), "{:?}", status_of(&report, "c1"));
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
    assert_eq!(status_of(&report, "c1"), &ClaimStatus::Verified { proof: "N0".into() });
    assert!(report.accepted());

    // The step is rejected: "split the step" when it has a second change, else the name of the body
    // whose MIR changed (design 22.4, rule 4).
    let split = |report: &StepReport| matches!(status_of(report, "c1"), ClaimStatus::Unproved { reason } if reason.contains("split the step") || reason.contains("is not neutral for it"));
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
        Ok(self.0.iter().map(|(name, status, _)| BodyComparison { name: name.to_string(), span: None, status: status.clone(), config: Vec::new() }).collect())
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
    assert_eq!(groups, [("add the field `y : 0`".to_string(), 1), ("add `..` or the field to the pattern".to_string(), 1)]);
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
    assert_eq!(c2.after.as_deref(), Some("the test `demo::tests::t` has a reachable call to `demo::f`. Semilla does not run the test and does not check what the test asserts."));
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
    // The derived `Debug` body shows the field name. `twice` has no text edit and is equal at N3.
    let bodies = [("demo::get", equal("n0")), ("demo::twice", equal("n3")), (debug, different(false)), ("<demo::Account as std::clone::Clone>::clone", equal("n0"))];
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
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Unproved { reason } if reason.contains("--reason")), "{:?}", status_of(&report, "c1"));
    // A hand-written body that differs and has no text edit: rejected, with the name of the body.
    let mut changed = bodies.to_vec();
    changed[1] = ("demo::twice", different(false));
    let report = run(before, &after, rename(), &changed, None);
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Unproved { reason } if reason.contains("demo::twice")), "{:?}", status_of(&report, "c1"));
}

#[test]
fn a_move_gets_move_hints() {
    let report = run(MOVE_BEFORE, MOVE_AFTER, vec![], &[], None);
    assert!(report.uncovered.iter().any(|u| u.hint.as_deref().is_some_and(|h| h.starts_with("move-item"))), "{:#?}", report.uncovered);
    assert!(!report.uncovered.iter().any(|u| u.hint.as_deref().is_some_and(|h| h.starts_with("rename"))), "{:#?}", report.uncovered);
    let rewrite = vec![claim("c1", "rewrite-item", Some("demo::a::over"), serde_json::json!({"new": "demo::b::over", "note": "move it to the b module"}))];
    let report = run(MOVE_BEFORE, MOVE_AFTER, rewrite, &[], None);
    assert_eq!(status_of(&report, "c1"), &ClaimStatus::Empty);
    assert!(report.claims[0].hint.as_deref().is_some_and(|h| h.contains("move-item")), "{:?}", report.claims[0].hint);
    let pair = semilla_core::PairWarning { old: "a::x::f".into(), new: "b::f".into(), similarity: 1.0, claims: vec![], refusal: None, reviewer: None, resolves: vec![] };
    // C2: the claim syntax of `semilla step`, and no retired op.
    assert!(pair.text().contains("`--claim 'move-item a::x::f to=b::f'`") && !pair.text().contains("--at") && !pair.text().contains("rewrite-item"), "{}", pair.text());
    let pair = semilla_core::PairWarning { old: "a::percent".into(), new: "a::pct".into(), similarity: 0.9, claims: vec![], refusal: None, reviewer: None, resolves: vec![] };
    assert!(pair.text().contains("`--claim 'rename a::percent to=pct'`"), "{}", pair.text());
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
        Ok(self.0.iter().map(|(name, status, _)| BodyComparison { name: name.to_string(), span: None, status: status.clone(), config: Vec::new() }).collect())
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
    // This check has no MIR records (phase 1): the class of a file fact is pending (brief 5h, C2).
    assert_eq!(report.facts[0].class, None);
    let with_reason = run_files(&[("Cargo.toml", MANIFEST_V0), lib], &[("Cargo.toml", &profile), lib], &[("Cargo.toml:profile.dev.overflow-checks", "the release build needs the speed")]);
    assert!(with_reason.accepted(), "{:#?}", with_reason);
    // A version change of an existing dependency: a plain fact.
    let (found, report) = facts(&MANIFEST_V0.replace("1.0.1", "1.0.2"));
    assert_eq!(found, ["Cargo.toml: dependencies.serde \"1.0.1\" → \"1.0.2\""]);
    assert!(!report.facts[0].spotlight && report.accepted());
    // A new dependency: in the spotlight.
    let (_, report) = facts(&format!("{MANIFEST_V0}rand = \"0.8\"\n"));
    assert!(report.facts[0].spotlight);
    // A format change only: one neutral fact.
    let (found, report) = facts(&MANIFEST_V0.replace("serde = \"1.0.1\"", "serde = \"1.0.1\" # the serde crate"));
    assert_eq!(found, ["Cargo.toml: format only"]);
    assert_eq!(report.facts[0].class, Some(Class::Neutral));
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
    assert_eq!(report.facts[0].description, "value of `demo::BASE_FEE` changed: 25 → 250");
    assert_eq!(report.facts[0].affected, ["demo::BASE_FEE", "demo::DOUBLE", "demo::fee"]);
    assert_eq!(report.facts[0].class, Some(Class::Logic));
    // In an identity step, the value fact gives "split the step".
    let renamed = after.replace("pub fn other", "pub fn another");
    let (_d1, old) = load(before);
    let (_d2, new) = load(&renamed);
    let step = step(vec![claim("c1", "rename", Some("demo::other"), serde_json::json!({"to": "another"}))]);
    let oracle = CallBodies(vec![("demo::BASE_FEE", different(false), vec![]), ("demo::DOUBLE", different(false), vec![]), ("demo::fee", different(false), vec![]), ("demo::another", equal("n0"), vec![])]);
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle).report;
    assert!(matches!(status_of(&report, "c1"), ClaimStatus::Unproved { reason } if reason.contains("split the step")), "{:?}", status_of(&report, "c1"));
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
    let oracle = SpanBodies(vec![BodyComparison { name: "<demo::E as std::cmp::PartialEq>::eq".into(), span: Some(("src/lib.rs".into(), 8)), status: different(true), config: Vec::new() }]);
    let report = verify_step(&StepInput { step: &step, before: &old, after: &new }, &oracle, &NoOracle).report;
    let imp = report.facts.iter().find(|f| f.part == "trait impl added").expect("an impl fact");
    assert_ne!(imp.class, Some(Class::Neutral), "{imp:#?}");
    assert!(!imp.affected.is_empty(), "{imp:#?}");
    assert!(imp.spotlight_why.as_deref().is_some_and(|w| w.contains("replaces the derive")), "{imp:#?}");
    assert!(report.facts.iter().all(|f| f.class != Some(Class::Neutral)), "{:#?}", report.facts);
    assert!(!report.accepted());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    // A new impl with equal bodies elsewhere still has the class of new code.
    let report = run(before, after, vec![], &[("demo::other", equal("n0"))], None);
    let imp = report.facts.iter().find(|f| f.part == "trait impl added").unwrap();
    assert_eq!(imp.class, Some(Class::Logic), "{imp:#?}");
}

#[test]
fn the_final_assertion_finds_a_changed_body_with_no_owner() {
    // Invariant I1, checked on a result that a defect in the verifier could make.
    let (_d, ws) = load("pub fn g() -> u32 {\n    1\n}\n");
    let step = step(vec![]);
    let bodies = vec![BodyComparison { name: "demo::g".into(), span: None, status: different(false), config: Vec::new() }];
    let index = checks::BodyIndex::new(&bodies, &ws, &ws);
    let none = BTreeSet::new();
    let input = owners::Input { step: &step, edits: &[], owners: &[], fact_of: &[], bodies: &bodies, body_index: &index, before: &ws, after: &ws, identity_owned: &none, identity_derived: &none, rename_targets: &none, identity_claim: None, callees: None };
    let mut report = StepReport::default();
    assert_eq!(owners::check(&input, &report), ["demo::g"]);
    // A neutral fact is not an owner.
    report.facts.push(semilla_core::Fact { id: "f1".into(), part: "visibility".into(), class: Some(Class::Neutral), affected: vec!["demo::g".into()], ..Default::default() });
    assert_eq!(owners::check(&input, &report), ["demo::g"]);
    report.facts[0].class = Some(Class::Logic);
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
    for harmless in ["#[inline]", "#[must_use]", "#[allow(dead_code)]", "#[rustfmt::skip]", "#[clippy::cognitive_complexity = \"100\"]"] {
        let report = run(base, &with(harmless), vec![], &bodies, None);
        let fact = &report.facts[0];
        assert!(!fact.spotlight && fact.class == Some(Class::Neutral), "{harmless}: {fact:#?}");
    }
    // T5 (trial 2, A5d) and others: in the spotlight, and not neutral by the kind.
    for loud in ["#[ignore]", "#[should_panic]", "#[cfg(test)]", "#[cfg(any())]", "#[test]", "#[no_mangle]", "#[track_caller]", "#[my_macro::instrument]", "#[path = \"x.rs\"]"] {
        let report = run(base, &with(loud), vec![], &bodies, None);
        let fact = &report.facts[0];
        assert!(fact.spotlight && fact.class != Some(Class::Neutral), "{loud}: {fact:#?}");
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
    BodyComparison { name: name.into(), span: Some((file.into(), line)), status, config: Vec::new() }
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
    assert!(cause.spotlight && cause.reason.is_none() && cause.class == Some(Class::Logic), "{cause:#?}");
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
    assert_ne!(file.class, Some(Class::Neutral));
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
    assert!(report.facts.iter().all(|f| f.class.is_none()), "{:#?}", report.facts);
    // Phase 2: a changed body gives the class of the op.
    let report = derive(&[("demo::total", different(true))]);
    assert!(report.facts.iter().all(|f| f.class == Some(Class::ControlFlow)), "{:#?}", report.facts);
    assert!(report.accepted(), "{report:#?}");
    // An equal body folds its edits under one neutral fact.
    let report = derive(&[("demo::total", equal("n2"))]);
    assert_eq!(report.facts.len(), 1, "{:#?}", report.facts);
    assert!(report.facts[0].description.contains("neutral rewrite (proved N2)") && report.facts[0].class == Some(Class::Neutral), "{:#?}", report.facts);
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
    assert!(texts[0].starts_with("renamed `demo::Account.balance` → `demo::Account.cents` (proved N0"), "{texts:?}");
    assert!(report.facts.iter().any(|f| f.description == "parameter `balance` of `demo::make` renamed to `cents`: `u32`" && f.class == Some(Class::Neutral)), "{texts:?}");
    assert!(report.facts.iter().any(|f| f.description.starts_with("changed docs") && f.class == Some(Class::Other)), "{texts:?}");
    assert_eq!(report.facts.len(), 3, "{texts:?}");
    // C1: a new module doc and a new inner doc attribute have the class `other`.
    for doc in ["//! The demo crate.\n", "#![doc = \"The demo crate.\"]\n"] {
        let report = run_derived(before, &format!("{doc}{before}"), &[("demo::get", equal("n0")), ("demo::make", equal("n0"))]);
        assert!(report.accepted() && !report.facts.is_empty() && report.facts.iter().all(|f| f.class == Some(Class::Other)), "{doc}: {:#?}", report.facts);
    }
    // The same step and a changed literal: no proof.
    let changed = after.replace("a.cents\n", "a.cents + 1\n");
    let bodies = [("demo::get", different(false)), ("demo::make", equal("n0")), (debug, different(false))];
    let report = run_derived(before, &changed, &bodies);
    assert!(!descriptions(&report).iter().any(|d| d.starts_with("renamed `demo::Account")), "{:?}", descriptions(&report));
    // A parameter rename in a body that changed is not allowed.
    let changed = after.replace("Account { cents: cents }", "Account { cents: cents + 1 }");
    let bodies = [("demo::get", equal("n0")), ("demo::make", different(false)), (debug, different(false))];
    let report = run_derived(before, &changed, &bodies);
    assert!(!descriptions(&report).iter().any(|d| d.starts_with("renamed `demo::Account")), "{:?}", descriptions(&report));
}

#[test]
fn a_new_type_gives_one_fact() {
    // Brief 5f, C1 (trial 4, D6): a new tuple struct gave "the tuple fields of `Label` changed:
    // `()` → `(u64)`", and a new struct or enum gave "member added of `T`".
    let before = "pub enum Kind {\n    A,\n}\npub fn f() -> u32 {\n    1\n}\n";
    let types = "pub struct Label(pub u64);\npub struct Shown {\n    pub id: u64,\n}\npub enum Mode {\n    Fast,\n    Slow(u8),\n    Named { a: u8 },\n}\n";
    let report = run_derived(before, &format!("{before}{types}"), &[("demo::f", equal("n0"))]);
    assert_eq!(descriptions(&report), ["added `demo::Label`", "added `demo::Mode`", "added `demo::Shown`"], "{:#?}", report.facts);
    assert!(report.uncovered.is_empty() && report.accepted(), "{report:#?}");
    // The removal is the same.
    let report = run_derived(&format!("{before}{types}"), before, &[("demo::f", equal("n0"))]);
    assert_eq!(descriptions(&report), ["removed `demo::Label`", "removed `demo::Mode`", "removed `demo::Shown`"], "{:#?}", report.facts);
    // A new tuple variant of an old enum: the variant fact has the fields. No tuple fact.
    let report = run_derived(before, &before.replace("    A,\n", "    A,\n    B(u8, u16),\n"), &[("demo::f", equal("n0"))]);
    assert_eq!(descriptions(&report), ["variant `demo::Kind::B` added: `B(u8, u16)`"], "{:#?}", report.facts);
    assert!(report.uncovered.is_empty(), "{report:#?}");
    // A changed tuple of an old type keeps its fact.
    let old = format!("{before}pub struct Pair(pub u32);\n");
    let report = run_derived(&old, &old.replace("Pair(pub u32)", "Pair(pub u32, pub u64)"), &[("demo::f", equal("n0"))]);
    assert_eq!(descriptions(&report), ["the tuple fields of `demo::Pair` changed: `(u32)` → `(u32, u64)`"], "{:#?}", report.facts);
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
    assert!(report.facts[0].description.starts_with("added operand `&& x > 0`") && report.facts[0].class == Some(Class::ControlFlow), "{:#?}", report.facts);
}

#[test]
fn the_note_of_a_pair_with_no_proof_names_the_difference_of_the_pair() {
    // Brief 5f, C3 (trial 4, D8): the note of a test rename with a changed literal named the
    // closure of another function.
    let before = "pub fn a_small_amount_has_no_fee() -> u32 {\n    let amount = 200;\n    amount + 1\n}\npub fn other(a: u32) -> u32 {\n    a + 1\n}\n";
    let after = before.replace("a_small_amount_has_no_fee", "a_small_amount_pays_the_fee").replace("200", "170").replace("a + 1", "a + 2");
    let bodies = [("demo::a_small_amount_pays_the_fee", different(false)), ("demo::other", different(false))];
    let report = run_derived(before, &after, &bodies);
    let note = report.notes.iter().find(|n| n.contains("as removed and added")).unwrap_or_else(|| panic!("{:#?}", report.notes));
    assert!(note.contains("the pair has a difference: changed literal `200` → `170`") && !note.contains("demo::other"), "{note}");
    // A pair with no difference: the note names the other change of the step.
    let after = before.replace("a_small_amount_has_no_fee", "a_small_amount_pays_the_fee").replace("a + 1", "a + 2");
    let bodies = [("demo::a_small_amount_pays_the_fee", equal("n0")), ("demo::other", different(false))];
    let report = run_derived(before, &after, &bodies);
    let note = report.notes.iter().find(|n| n.contains("as removed and added")).unwrap_or_else(|| panic!("{:#?}", report.notes));
    assert!(note.contains("the MIR of `demo::other` changed") && note.contains("Put the rename in its own step"), "{note}");
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
    assert_eq!(report.facts.len(), 1, "{:#?}", report.facts);
    let fact = &report.facts[0];
    assert!(fact.description.starts_with("renamed `demo::LedgerError::Frozen` → `demo::LedgerError::Locked` (proved N0; neutral, but 1 derived body shows the name (Debug for demo::LedgerError)"), "{}", fact.description);
    assert_eq!(fact.class, Some(Class::Neutral));
    // A field of a variant: the same rule.
    let report = run_derived(before, &before.replace("max", "limit"), &bodies);
    assert!(report.accepted() && report.facts.len() == 1 && report.facts[0].description.contains("1 derived body shows the name (Debug for demo::LedgerError)"), "{report:#?}");
    // A body that is not derived keeps the rule: no proof.
    let other = [("demo::check", different(false)), (debug, different(false))];
    let report = run_derived(before, &before.replace("Frozen", "Locked"), &other);
    assert!(!descriptions(&report).iter().any(|d| d.starts_with("renamed")), "{:?}", descriptions(&report));
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
    // One fact: the forced edits fold under it, and the derived body rule applies.
    assert_eq!(report.facts.len(), 1, "{:#?}", report.facts);
    let fact = &report.facts[0];
    assert!(fact.description.starts_with("renamed `demo::Account.balance` → `demo::Account.cents` (proved N0; neutral, but 1 derived body shows the name"), "{}", fact.description);
    assert_eq!(fact.class, Some(Class::Neutral));
    // Phase 1: no proof yet, a note, and the flag that makes `step` run phase 2.
    let report = run_derived(before, &after, &[]);
    assert!(report.identity_candidates && report.notes.iter().any(|n| n.contains("phase 2 tries the proof")), "{report:#?}");
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
    // A rename and a changed literal in the same body: no "renamed (proved)", the literal is a
    // logic fact, and the note says how to get the proof.
    let before = "#[derive(Debug)]\npub struct Account {\n    pub balance: u32,\n}\npub fn get(a: &Account) -> u32 {\n    a.balance + 1\n}\n";
    let after = before.replace("pub balance: u32", "pub cents: u32").replace("a.balance + 1", "a.cents + 2");
    let report = run_derived(before, &after, &[("demo::get", different(false)), ("<demo::Account as std::fmt::Debug>::fmt", different(false))]);
    let texts = descriptions(&report);
    assert!(!texts.iter().any(|d| d.starts_with("renamed")), "{texts:?}");
    assert!(report.facts.iter().any(|f| f.description.starts_with("changed literal `1` → `2`") && f.class == Some(Class::Logic)), "{:#?}", report.facts);
    assert!(report.notes.iter().any(|n| n.contains("Put the rename in its own step to get a proof")), "{:?}", report.notes);
    // A move: the derived fact with no claim.
    let bodies = [("demo::b::over", equal("n0")), ("demo::b::run", equal("n1"))];
    let report = run_derived(MOVE_BEFORE, MOVE_AFTER, &bodies);
    assert!(descriptions(&report).iter().any(|d| d.starts_with("moved `demo::a::over` → `demo::b::over` (proved N1")), "{report:#?}");
    // A move and a dropped `pub`: no proof, and the visibility shows.
    let narrow = MOVE_AFTER.replace("    pub fn over", "    fn over");
    let report = run_derived(MOVE_BEFORE, &narrow, &bodies);
    assert!(!descriptions(&report).iter().any(|d| d.starts_with("moved")), "{:?}", descriptions(&report));
    assert!(report.notes.iter().any(|n| n.contains("the pair has a difference: changed visibility: `pub` → `private`. A move with no other change in its step gets a proof")), "{:?}", report.notes);
    // A move and a body change: no proof, and the body is a logic fact.
    let changed = MOVE_AFTER.replace("x > LIMIT", "x >= LIMIT");
    let report = run_derived(MOVE_BEFORE, &changed, &[("demo::b::over", different(false)), ("demo::b::run", equal("n1"))]);
    assert!(!descriptions(&report).iter().any(|d| d.starts_with("moved")), "{:?}", descriptions(&report));
    assert!(report.facts.iter().any(|f| f.description.starts_with("added `demo::b::over`") && f.class == Some(Class::Logic)), "{:#?}", report.facts);
    // A4 (trial 3, A2): the edit inside the moved item is a fact under the pair.
    assert!(report.facts.iter().any(|f| f.description.starts_with("`over` differs from the removed `over`: changed operator `x > LIMIT` → `x >= LIMIT`") && f.class == Some(Class::Logic)), "{:#?}", report.facts);
    // A function rename and a changed literal in its body.
    let before = "pub fn percent(a: u64) -> u64 {\n    a / 100\n}\npub fn charge(a: u64) -> u64 {\n    percent(a) + 1\n}\n";
    let after = before.replace("percent", "pct").replace("a / 100", "a / 10");
    let report = run_derived(before, &after, &[("demo::pct", different(false)), ("demo::charge", equal("n0"))]);
    assert!(!descriptions(&report).iter().any(|d| d.starts_with("renamed")), "{:?}", descriptions(&report));
    assert!(report.facts.iter().any(|f| f.description.starts_with("`pct` differs from the removed `percent`: changed literal `100` → `10`") && f.class == Some(Class::Logic)), "{:#?}", report.facts);
}

#[test]
fn the_identity_search_has_limits() {
    // Two fields of the same type change their names: the wrong map fails the proof (the order of
    // the fields changes), so one map passes.
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
    // Phase 2: a fact with another class does not fold.
    let report = run_derived(before, &after, &[("demo::f", different(false)), ("demo::g", different(false)), ("demo::h", different(true))]);
    let texts = descriptions(&report);
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts.contains(&"2 site(s): changed literal `1` → `2`".to_string()), "{texts:?}");
    assert!(report.facts.iter().any(|f| f.class == Some(Class::ControlFlow) && f.description.contains("demo::h")), "{:#?}", report.facts);
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
    // fact has the class of the body, and its text does not say "inverted".
    let before = "pub fn f(a: bool) -> u32 {\n    if a {\n        1\n    } else {\n        2\n    }\n}\n";
    let after = "pub fn f(a: bool) -> u32 {\n    if a {\n        2\n    } else {\n        1\n    }\n}\n";
    let report = run_derived(before, after, &[("demo::f", different(true))]);
    assert!(report.accepted(), "{report:#?}");
    assert!(report.facts.iter().all(|f| f.class == Some(Class::ControlFlow) && !f.description.contains("invert")), "{:#?}", report.facts);
    let inverted = "pub fn f(a: bool) -> u32 {\n    if !a {\n        2\n    } else {\n        1\n    }\n}\n";
    let report = run_derived(before, inverted, &[("demo::f", equal("n2"))]);
    assert!(report.facts.iter().all(|f| f.class == Some(Class::Neutral)), "{:#?}", report.facts);
}

#[test]
fn the_group_kind_comes_from_the_facts() {
    let before = "#[derive(Debug)]\npub struct Account {\n    pub balance: u32,\n}\npub fn get(a: &Account) -> u32 {\n    a.balance\n}\n";
    let after = before.replace("balance", "cents");
    let renamed = run_derived(before, &after, &[("demo::get", equal("n0")), ("<demo::Account as std::fmt::Debug>::fmt", different(false))]);
    assert_eq!(organize::derived_kind(&[&renamed]), Some(GroupKind::Rename));
    // Phase 1: a class is pending, so no kind.
    assert_eq!(organize::derived_kind(&[&run_derived(before, &after, &[])]), None);
    // Only proved neutral facts: a refactor, not a behavior change.
    let refactor = run_derived(BEFORE, &BEFORE.replace("let mut s = 0;", "let mut s = 0u32;"), &[("demo::total", equal("n2"))]);
    assert_eq!(organize::derived_kind(&[&refactor]), Some(GroupKind::Refactor), "{refactor:#?}");
    let behavior = run_derived(BEFORE, AFTER, &[("demo::total", different(true))]);
    assert_eq!(organize::derived_kind(&[&behavior]), Some(GroupKind::Behavior));
    assert_eq!(organize::merge_kinds(GroupKind::Rename, GroupKind::Refactor), GroupKind::Refactor);
    assert_eq!(organize::merge_kinds(GroupKind::Rename, GroupKind::Behavior), GroupKind::Behavior);
}

/// A body oracle with statuses and full MIR facts.
struct MemberBodies(Vec<(&'static str, BodyStatus, BodyFacts)>);

impl BodyOracle for MemberBodies {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(self.0.iter().map(|(name, status, _)| BodyComparison { name: name.to_string(), span: None, status: status.clone(), config: Vec::new() }).collect())
    }
    fn facts(&self, _: &Step) -> anyhow::Result<Option<Vec<BodyFacts>>> {
        Ok(Some(self.0.iter().map(|(name, _, facts)| BodyFacts { name: name.to_string(), ..facts.clone() }).collect()))
    }
}

#[test]
fn a_member_order_is_never_neutral_and_a_cast_puts_it_in_the_spotlight() {
    // Batch 5e: a member order has effects outside the MIR records (the drop order, the derive
    // output order, the discriminant values). With no changed body it is still `logic`.
    let src = "pub enum Kind {\n    Small,\n    Large,\n}\npub fn code(k: Kind) -> u8 {\n    k as u8\n}\npub struct Pair {\n    pub a: u64,\n    pub b: u64,\n}\npub const P: Pair = Pair { a: 1, b: 2 };\npub fn first() -> u64 {\n    P.a\n}\n";
    let check = |after: &str, bodies: Vec<(&'static str, BodyStatus, BodyFacts)>| {
        let (_d1, old) = load(src);
        let (_d2, new) = load(after);
        let step = step(vec![]);
        verify_step(&StepInput { step: &step, before: &old, after: &new }, &MemberBodies(bodies), &NoOracle).report
    };
    let kinds = src.replace("    Small,\n    Large,\n", "    Large,\n    Small,\n");
    // No body changes: the fact is `logic`, and the text names the discriminant values.
    let report = check(&kinds, vec![("demo::code", equal("n0"), BodyFacts::default())]);
    let fact = report.facts.iter().find(|f| f.part == "member order").expect("a member order fact");
    assert_eq!(fact.class, Some(Class::Logic), "{fact:?}");
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
    assert_eq!((fact.class, fact.affected.clone()), (Some(Class::Logic), vec!["demo::first".to_string()]), "{:#?}", report.facts);
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
        ["type `u32` → `u64` in 5 places: `demo::Account.id`, `demo::Event::Close.account`, `demo::Event::Unknown.0`, parameter `id` of `demo::find`, return type of `demo::find`"],
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
    assert!(texts.iter().any(|text| text == "type of `demo::Account.size`: `u32` → `u16`"), "{texts:#?}");
    assert!(texts.iter().any(|text| text == "parameter of `demo::other` `count : u8` → `count : u16`"), "{texts:#?}");
    assert!(texts.iter().any(|text| text.starts_with("type `u32` → `u64` in 5 places: ")), "{texts:#?}");

    // A different class does not fold: the type of the parameter changes the body of `find`.
    let report = run_derived(before, &after, &[("demo::find", different(false)), ("demo::other", equal("n0"))]);
    let texts = descriptions(&report);
    let classes: BTreeSet<String> = report.facts.iter().map(|fact| format!("{:?}", fact.class)).collect();
    assert!(texts.len() >= 2 && classes.len() == 2, "{:#?}", report.facts);
    for place in ["`demo::Account.id`", "`demo::Event::Close.account`", "`demo::Event::Unknown.0`", "`id`", "return type of `demo::find`"] {
        assert_eq!(texts.iter().filter(|text| text.contains(place)).count(), 1, "the fold shows the place {place} one time: {texts:#?}");
    }

    // Two different changes in one tuple, and a new parameter name, keep their own facts.
    let tuple = before.replace("Unknown(u32, u8)", "Unknown(u64, u16)").replace("pub id: u32", "pub id: u64");
    let texts = descriptions(&run_derived(before, &tuple, &bodies));
    assert_eq!(texts, ["type of `demo::Account.id`: `u32` → `u64`", "the tuple fields of `demo::Event::Unknown` changed: `(u32, u8)` → `(u64, u16)`"]);
}

#[test]
fn each_body_of_a_site_is_affected_or_proved_equal() {
    // Brief 5f, C6 (trial 4, D14): the fact of a new parameter named the sites in `schedule`
    // and `withdraw`, but the affected bodies did not have them, with no cause. The compare
    // proves these bodies equal (after the inline of the callee). The fact says so.
    let before = "pub fn charge(amount: u64) -> u64 {\n    amount + 1\n}\npub fn schedule(a: u64) -> u64 {\n    charge(a)\n}\npub fn withdraw(a: u64) -> u64 {\n    charge(a) + 2\n}\n";
    let after = before.replace("charge(amount: u64)", "charge(amount: u64, rate: u64)").replace("amount + 1", "amount + rate").replace("charge(a)", "charge(a, 1)");
    let report = run_calls(before, &after, vec![("demo::charge", different(false), vec![]), ("demo::schedule", equal("n3"), vec!["demo::charge"]), ("demo::withdraw", different(false), vec!["demo::charge"])]);
    let fact = report.facts.iter().find(|fact| fact.description == "parameter of `demo::charge` added: `rate : u64`").unwrap_or_else(|| panic!("{:#?}", report.facts));
    let sites: BTreeSet<String> = fact.sites.iter().flat_map(|group| group.sites.iter().map(|site| site.item.clone())).collect();
    assert_eq!(sites, ["demo::schedule".to_string(), "demo::withdraw".to_string()].into(), "{fact:#?}");
    assert_eq!(fact.affected, ["demo::charge", "demo::withdraw"], "{fact:#?}");
    assert_eq!(fact.equal_sites, ["demo::schedule"], "{fact:#?}");
    // Each body of a site is in one of the two lists.
    assert!(sites.iter().all(|site| fact.affected.contains(site) != fact.equal_sites.contains(site)), "{fact:#?}");
    let text = views::text_report(std::slice::from_ref(&report));
    assert!(text.contains("      affected bodies: demo::charge, demo::withdraw\n      bodies with a changed call site and no change of behavior (proved): demo::schedule\n"), "{text}");
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
fn a_group_of_a_rename_with_its_docs_is_not_behavior() {
    let before = "#[derive(Debug)]\npub struct Account {\n    pub balance: u32,\n}\n/// Returns the balance.\npub fn get(a: &Account) -> u32 {\n    a.balance\n}\n";
    let after = before.replace("balance", "cents");
    let bodies = [("demo::get", equal("n0")), ("<demo::Account as std::fmt::Debug>::fmt", different(false))];
    let report = run_derived(before, &after, &bodies);
    assert!(report.facts.iter().any(|f| f.is_doc()), "{:?}", descriptions(&report));
    assert_eq!(organize::derived_kind(&[&report]), Some(GroupKind::Rename), "{:?}", descriptions(&report));
    // Only a doc change: no code changed.
    let docs = run_derived(before, &before.replace("Returns the balance.", "Gives the balance."), &[]);
    assert_eq!(organize::derived_kind(&[&docs]), Some(GroupKind::Refactor), "{:?}", descriptions(&docs));
    // A doc change with a logic change stays `behavior`.
    let logic = run_derived(before, &before.replace("Returns the balance.", "Gives the balance.").replace("a.balance", "a.balance + 1"), &[("demo::get", different(false))]);
    assert_eq!(organize::derived_kind(&[&logic]), Some(GroupKind::Behavior), "{:?}", descriptions(&logic));
}

#[test]
fn the_text_output_shows_six_bodies_of_a_fact() {
    // Brief 5h, C6: the `step` and `status` output shows at most 6 bodies of a list, then
    // "and N more". The report with `--full` has the full list.
    let bodies: Vec<String> = (0..21).map(|index| format!("demo::body_{index:02}")).collect();
    let fact = semilla_core::Fact { id: "f1".into(), part: "type".into(), description: "type of `demo::Cart::total` changed: u32 → u64".into(), status: "derived".into(), class: Some(Class::Logic), affected: bodies.clone(), equal_sites: bodies[..8].to_vec(), ..Default::default() };
    let report = StepReport { facts: vec![fact], ..Default::default() };
    let text = crate::views::text_report_with(std::slice::from_ref(&report), false);
    assert!(text.contains("      affected bodies: demo::body_00, demo::body_01, demo::body_02, demo::body_03, demo::body_04, demo::body_05, and 15 more\n"), "{text}");
    assert!(text.contains("      bodies with a changed call site and no change of behavior (proved): demo::body_00, demo::body_01, demo::body_02, demo::body_03, demo::body_04, demo::body_05, and 2 more\n"), "{text}");
    assert!(!text.contains("demo::body_06"), "{text}");
}

/// The facts of a step whose MIR oracle says that `demo::f` is equal (N0).
fn source_config_facts(before: &str, after: &str) -> Vec<semilla_core::report::Fact> {
    run_derived(before, after, &[("demo::f", equal("n0"))]).facts
}

#[test]
fn a_keyword_before_the_operator_not_is_not_a_macro_name() {
    // Batch 5l, B4: the token reader took `if !(a)` as a call of a macro `if!`. A keyword and
    // the name of a label are not macro names.
    let base = "pub fn f(a: u64) -> u64 {\n    let b = a;\n    b\n}\n";
    let proved = |facts: &[semilla_core::report::Fact]| facts.iter().any(|f| f.class == Some(Class::Neutral));
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
        let facts = with(form);
        assert!(proved(&facts) && spotlight(&facts).is_empty(), "{form}: {facts:#?}");
    }
    // A macro call stays a macro call: a raw identifier, a weak keyword, a keyword of a later
    // edition only, and a space before the `!`.
    for (form, name) in [("let _ = r#try!(Ok::<u64, ()>(c));", "try!"), ("other::r#if!(c);", "if!"), ("union!(c);", "union!"), ("gate !(c);", "gate!")] {
        let facts = with(form);
        assert!(!proved(&facts) && spotlight(&facts).contains(&format!("`{name}` in `demo::f` (Semilla cannot see into it)")), "{form}: {facts:#?}");
    }
}

#[test]
fn a_cfg_attribute_in_a_changed_body_stops_the_proof() {
    // Batch 5k, A1: the compiler removes the code of a false `cfg` attribute, so the MIR of the
    // check configuration is equal. The proof does not hold for each build.
    let base = "pub fn f(a: u64) -> u64 {\n    let b = a;\n    b\n}\n";
    let proved = |facts: &[semilla_core::report::Fact]| facts.iter().any(|f| f.class == Some(Class::Neutral));
    let spotlight = |facts: &[semilla_core::report::Fact]| facts.iter().filter_map(|f| f.spotlight_why.clone()).collect::<Vec<_>>().join("\n");

    // An honest rewrite, with a free attribute and macros of the standard library.
    let facts = source_config_facts(base, "pub fn f(a: u64) -> u64 {\n    #[allow(unused)]\n    let c = a;\n    debug_assert!(c == a, \"{}\", format!(\"{c}\"));\n    c\n}\n");
    assert!(proved(&facts) && spotlight(&facts).is_empty(), "{facts:#?}");

    // A statement with a `cfg` attribute.
    let facts = source_config_facts(base, "pub fn f(a: u64) -> u64 {\n    #[cfg(not(debug_assertions))]\n    return 0;\n    let b = a;\n    b\n}\n");
    assert!(!proved(&facts), "{facts:#?}");
    assert!(spotlight(&facts).contains("this code depends on the build configuration") && spotlight(&facts).contains("Uses: `#[cfg(not(debug_assertions))]` in `demo::f`"), "{facts:#?}");

    // A `cfg` attribute with no edit, in a body with an edit: the proof of the edit is for one
    // configuration only.
    let old = "pub fn f(a: u64) -> u64 {\n    let mut b = a;\n    #[cfg(windows)]\n    {\n        b += 1;\n    }\n    b\n}\n";
    let facts = source_config_facts(old, &old.replace("    b\n}", "    a\n}"));
    assert!(!proved(&facts) && spotlight(&facts).is_empty(), "{facts:#?}");
    // The spotlight is only for an edit in the statement with the attribute (batch 5o, A4).
    let facts = source_config_facts(old, &old.replace("b += 1;", "b += 2;"));
    assert!(!proved(&facts) && spotlight(&facts).contains("`#[cfg(windows)]` in `demo::f`"), "{facts:#?}");
    let facts = source_config_facts(old, &old.replace("    #[cfg(windows)]\n", "    b += 0;\n    #[cfg(windows)]\n"));
    assert!(!proved(&facts) && spotlight(&facts).is_empty(), "{facts:#?}");
    // An attribute above the changed block. Not an attribute in a different statement.
    let old = "pub fn f(a: u64) -> u64 {\n    #[cfg(unix)]\n    let quick = 1;\n    #[cfg(windows)]\n    {\n        let b = a;\n        return b;\n    }\n    let c = a;\n    c\n}\n";
    let facts = source_config_facts(old, &old.replace("        let b = a;\n        return b;\n", "        return a;\n"));
    assert!(!proved(&facts) && spotlight(&facts).contains("`#[cfg(windows)]` in `demo::f`") && !spotlight(&facts).contains("unix"), "{facts:#?}");
    let facts = source_config_facts(old, &old.replace("    let c = a;\n    c\n", "    a\n"));
    assert!(!proved(&facts) && spotlight(&facts).is_empty(), "{facts:#?}");
    // An edit only in a comment: the tokens are the same, so the proof stays.
    let facts = source_config_facts(old, &old.replace("    let c = a;\n", "    // A note.\n    let c = a;\n"));
    assert!(spotlight(&facts).is_empty(), "{facts:#?}");

    // A `cfg` attribute on the function selects the full function: each build with the body has
    // the same body.
    let facts = source_config_facts(&format!("#[cfg(unix)]\n{base}"), "#[cfg(unix)]\npub fn f(a: u64) -> u64 {\n    let c = a;\n    c\n}\n");
    assert!(proved(&facts), "{facts:#?}");
    let facts = source_config_facts(&format!("#[cfg_attr(unix, inline)]\n{base}"), "#[cfg_attr(unix, inline)]\npub fn f(a: u64) -> u64 {\n    let c = a;\n    c\n}\n");
    // `inline` under `cfg_attr` is in the allow list (batch 5o, A5). `no_mangle` is not.
    assert!(proved(&facts) && spotlight(&facts).is_empty(), "{facts:#?}");
    let facts = source_config_facts(&format!("#[cfg_attr(unix, no_mangle)]\n{base}"), "#[cfg_attr(unix, no_mangle)]\npub fn f(a: u64) -> u64 {\n    let c = a;\n    c\n}\n");
    assert!(!proved(&facts), "{facts:#?}");

    // A macro of the workspace with a `cfg` attribute in its definition, also through a second
    // macro.
    let macros = "macro_rules! rel { ($e:expr) => { #[cfg(not(debug_assertions))] { $e } }; }\nmacro_rules! outer { ($e:expr) => { rel!($e) }; }\n";
    let facts = source_config_facts(&format!("{macros}{base}"), &format!("{macros}pub fn f(a: u64) -> u64 {{\n    outer!(return 0);\n    let b = a;\n    b\n}}\n"));
    assert!(!proved(&facts) && spotlight(&facts).contains("`outer! (rel! (#[cfg(not(debug_assertions))]))` in `demo::f`"), "{facts:#?}");
    // A free macro of the workspace does not stop the proof.
    let free = "macro_rules! twice { ($e:expr) => { $e + $e }; }\n";
    let facts = source_config_facts(&format!("{free}pub fn f(a: u64) -> u64 {{\n    let b = twice!(a);\n    b\n}}\n"), &format!("{free}pub fn f(a: u64) -> u64 {{\n    let c = twice!(a);\n    c\n}}\n"));
    assert!(proved(&facts), "{facts:#?}");

    // Semilla cannot see into a macro or an attribute of another crate.
    let facts = source_config_facts(base, "pub fn f(a: u64) -> u64 {\n    other::gate!(return 0);\n    let b = a;\n    b\n}\n");
    assert!(!proved(&facts) && spotlight(&facts).contains("Semilla cannot see if this code depends on the build configuration") && spotlight(&facts).contains("`gate!` in `demo::f` (Semilla cannot see into it)"), "{facts:#?}");
    let facts = source_config_facts(base, "pub fn f(a: u64) -> u64 {\n    cfg_select! { debug_assertions => {} _ => { return 0; } }\n    let b = a;\n    b\n}\n");
    assert!(!proved(&facts) && spotlight(&facts).contains("`cfg_select!` in `demo::f`"), "{facts:#?}");
    let facts = source_config_facts(base, "pub fn f(a: u64) -> u64 {\n    #[other::gate]\n    let c = a;\n    c\n}\n");
    assert!(!proved(&facts) && spotlight(&facts).contains("`#[other::gate]` in `demo::f` (Semilla cannot see into it)"), "{facts:#?}");
    // A macro of another crate can have the name of a standard macro.
    let glob = "use other::*;\n";
    let facts = source_config_facts(&format!("{glob}{base}"), &format!("{glob}pub fn f(a: u64) -> u64 {{\n    let c = a;\n    println!(\"{{c}}\");\n    c\n}}\n"));
    assert!(!proved(&facts) && spotlight(&facts).contains("`println!` in `demo::f` (Semilla cannot see into it)"), "{facts:#?}");
    let import = "use other::gate as vec;\n";
    let facts = source_config_facts(&format!("{import}{base}"), &format!("{import}pub fn f(a: u64) -> u64 {{\n    let c = a;\n    let _ = vec![c];\n    c\n}}\n"));
    assert!(!proved(&facts) && spotlight(&facts).contains("`vec!` in `demo::f` (Semilla cannot see into it)"), "{facts:#?}");

    // A body with no text edit keeps its status.
    let old = "pub fn f(a: u64) -> u64 {\n    #[cfg(windows)]\n    return 0;\n    a\n}\npub fn g() {}\n";
    let facts = source_config_facts(old, &old.replace("pub fn g() {}", "pub fn g() {\n}"));
    assert!(spotlight(&facts).is_empty(), "{facts:#?}");
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
    // An ASCII rename has no such spotlight.
    let report = run_derived(before, &before.replace("charge", "charge_fee"), &[]);
    assert!(!report.facts.iter().any(|fact| why(fact).contains(NON_ASCII)), "{:#?}", report.facts);
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
    assert!(!text.contains("own step") && !text.contains("similar"), "{text}");
    // To `c`: each name points to the same item. The usual text.
    let found = pairs(&format!("{}{}{}", module("a", a, false), module("b", b, false), module("c", c, true)), "demo::c::over");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].resolves.is_empty() && found[0].text().contains("are 100% similar") && found[0].text().contains("in its own step"), "{:?}", found[0]);
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
    let body = |name: &str, status: BodyStatus| BodyComparison { name: name.to_string(), span: None, status, config: Vec::new() };
    // The call of `strict` is a node (batch 5o, A3): the driver gives the item under `cfg`,
    // and the new call makes the MIR different.
    opaque.note(true, "demo::user", None, &["cfg demo::strict".to_string()]);
    let mut bodies = vec![body("demo::user", different(false)), body("demo::outer", equal("n0")), body("demo::helper", equal("n0"))];
    opaque.apply(&mut bodies, &BTreeMap::new());
    assert_eq!(bodies[0].status, different(false));
    assert_eq!(bodies[0].config, vec!["`wide` (`#[cfg(target_pointer_width=\"64\")]`)".to_string(), "`strict` (2 definitions under `cfg`)".to_string()]);
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
    assert_eq!(bodies[0].config, vec!["`DEEP` (`#[cfg(unix)]`)".to_string(), "`probe` (an `impl` under `cfg`)".to_string()]);

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
    let body = |name: &str, status: BodyStatus| BodyComparison { name: name.to_string(), span: None, status, config: Vec::new() };
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
    let body = |name: &str, status: BodyStatus| BodyComparison { name: name.to_string(), span: None, status, config: Vec::new() };
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
    let body = |name: &str| BodyComparison { name: name.to_string(), span: None, status: equal("n2"), config: Vec::new() };
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
        Ok(self.0.iter().map(|(name, status)| BodyComparison { name: name.to_string(), span: span(name), status: status.clone(), config: Vec::new() }).collect())
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
    assert!(fact.class.is_some_and(|class| class != Class::Neutral) && !fact.spotlight, "{fact:#?}");
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
    assert!(!fact.spotlight && fact.class.is_some_and(|class| class != Class::Neutral), "{fact:#?}");
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
    // A proved rewrite of the library function: the bodies of the bench still change (they read
    // the text of the file). The fact is not neutral, and its text says why.
    let (_d3, new) = tree(&lib.replace("x + 1", "1 + x"));
    let facts = vec![BodyFacts { name: "demo::DATA".into(), span: at(1), ..Default::default() }, BodyFacts { name: "demo::main".into(), span: at(2), ..Default::default() }];
    let bodies = vec![("demo::f", equal("n2")), ("demo::DATA", different(false)), ("demo::main", different(false))];
    let report = crate::verify_step_with(&StepInput { step: &step, before: &old, after: &new }, &StatusFacts(bodies, facts), &NoOracle, true).report;
    assert_eq!(report.facts.len(), 1, "{:#?}", report.facts);
    let fact = &report.facts[0];
    assert!(!fact.spotlight && fact.class == Some(Class::Logic), "{fact:#?}");
    assert_eq!(fact.description, "`demo::f`: neutral rewrite (proved N2), 2 edit(s); the edit changed other bodies (the affected bodies), so the fact is not neutral");
    assert_eq!(fact.affected, ["demo::demo::DATA", "demo::demo::main"], "{fact:#?}");
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
    assert!(texts.contains(&"moved let: `let data = v.len() as u32;` from `S0` to `S1` in `demo::f`"), "{texts:#?}");
    assert!(texts.contains(&"moved path: `None` from `S0/then/tail` to `S0/else/tail` in `demo::h`"), "{texts:#?}");
    assert!(texts.contains(&"moved call: `Some(0)` from `S0/else/tail` to `S0/then/tail` in `demo::h`"), "{texts:#?}");
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
