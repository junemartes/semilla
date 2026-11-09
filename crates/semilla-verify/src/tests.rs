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
        Ok(self.0.iter().map(|(name, status)| BodyComparison { name: name.clone(), span: None, status: status.clone() }).collect())
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
    Step { index: 0, intent: "test".into(), class: None, claims, tree_before: "a".into(), tree_after: "b".into() }
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
    let report = run(
        before,
        after,
        vec![
            claim("c1", "rename", Some("demo::helper"), serde_json::json!({"to": "add_one"})),
            claim("c2", "rename-local", Some("demo::f/S0"), serde_json::json!({"to": "sum"})),
        ],
        &[("demo::add_one", equal("n0")), ("demo::f", equal("n1"))],
        None,
    );
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
    assert!(status_of(&report, "c1").is_verified(), "{:?}", status_of(&report, "c1"));
    assert!(status_of(&report, "c2").is_verified(), "{:?}", status_of(&report, "c2"));
    assert!(report.accepted());
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

#[test]
fn indirect_changes_need_affects() {
    let src = "pub fn f(x: u32) -> u32 { x + 1 }\npub fn g(x: u32) -> u32 { x * 2 }\n";
    let changed = "pub fn f(x: u32) -> u32 { x + 2 }\npub fn g(x: u32) -> u32 { x * 2 }\n";
    let base = || vec![claim("c1", "change-literal", Some("demo::f/tail/rhs"), serde_json::json!({}))];
    let bodies = [("demo::f", different(false)), ("demo::g", different(false))];
    let report = run(src, changed, base(), &bodies, None);
    assert_eq!(report.uncovered.len(), 1);
    assert_eq!(report.uncovered[0].view, View::Body);
    let mut claims = base();
    claims[0].args.insert("affects".into(), serde_json::json!(["demo::g"]));
    let report = run(src, changed, claims, &bodies, None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
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
    let (report, _) = verify_changeset(&set, &mut |tree| Ok(trees.remove(tree).unwrap()), &bodies, &NoOracle, Options { require_groups: true });
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
fn affects_can_name_a_module() {
    let src = "pub fn f(x: u32) -> u32 { x + 1 }\npub mod m { pub fn g(x: u32) -> u32 { x * 2 } }\n";
    let changed = "pub fn f(x: u32) -> u32 { x + 2 }\npub mod m { pub fn g(x: u32) -> u32 { x * 2 } }\n";
    let mut claims = vec![claim("c1", "change-literal", Some("demo::f/tail/rhs"), serde_json::json!({}))];
    claims[0].args.insert("affects".into(), serde_json::json!(["demo::m::*"]));
    let report = run(src, changed, claims, &[("demo::f", different(false)), ("demo::m::g", different(false))], None);
    assert!(report.uncovered.is_empty(), "{:#?}", report.uncovered);
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
