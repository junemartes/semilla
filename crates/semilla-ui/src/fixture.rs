//! A hand-made data set for the demo and the tests.
//!
//! The workspace `shop` has two crates: `shop_core` (a library) and `shop_cli` (a binary).
//! The change set has three groups, as in the example of design section 11.3:
//!
//! - G1 `fix`: return 0 for an empty cart, with two renames and a new test.
//! - G2 `api`: return `Result` from `Cart::total`, with one uncovered difference.
//! - G3 `refactor`: extract the price of a line into a `billing` module, with one unproved claim.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use semilla_core::report::Effect;
use semilla_core::view::{
    FlowEdge, FlowGraph, FlowKind, FlowNode, GraphEdge, GraphEdgeKind, GraphNode, GraphNodeKind, Status, TreeNode, WorkspaceGraph,
};
use semilla_core::{
    ChangeSet, Claim, ClaimResult, ClaimStatus, Class, Group, GroupKind, Report, Role, Step, StepReport, Subgroup, Uncovered, View,
};

use crate::{SourcePair, UiData};

const TOTAL: &str = "shop_core::cart::Cart::total";

fn claim(id: &str, op: &str, at: Option<&str>, args: Value) -> Claim {
    Claim {
        id: id.to_string(),
        op: op.to_string(),
        at: at.map(|at| at.parse().expect("the fixture anchors are valid")),
        args: match args {
            Value::Object(map) => map,
            _ => Default::default(),
        },
    }
}

fn result(claim: &str, step: usize, status: ClaimStatus, class: Option<Class>) -> ClaimResult {
    ClaimResult { claim: claim.to_string(), step, status, class, effect: None, witness: None, hint: None, before: None, after: None }
}

fn verified(claim: &str, step: usize, proof: &str, class: Class) -> ClaimResult {
    result(claim, step, ClaimStatus::Verified { proof: proof.to_string() }, Some(class))
}

fn texts(mut result: ClaimResult, before: &str, after: &str) -> ClaimResult {
    result.before = (!before.is_empty()).then(|| before.to_string());
    result.after = (!after.is_empty()).then(|| after.to_string());
    result
}

fn step(index: usize, intent: &str, class: Option<Class>, claims: Vec<Claim>) -> Step {
    Step {
        index,
        intent: intent.to_string(),
        class,
        claims,
        tree_before: format!("{:040x}", index * 0x1111),
        tree_after: format!("{:040x}", (index + 1) * 0x1111),
    }
}

fn subgroup(group: &str, role: Role, title: &str, claims: &[&str], edited: bool, why: &str) -> Subgroup {
    // The engine gives each subgroup the ID `<group>:<role>`.
    let role_name = serde_json::to_value(role).ok().and_then(|value| value.as_str().map(str::to_string)).unwrap_or_default();
    Subgroup {
        id: format!("{group}:{role_name}"),
        group: group.to_string(),
        role,
        title: title.to_string(),
        why: why.to_string(),
        claims: claims.iter().map(|claim| claim.to_string()).collect(),
        edited,
        new_claims: Vec::new(),
    }
}

/// Builds the change set: 7 steps, 18 claims, 3 groups, and 10 subgroups.
pub fn demo_changeset() -> ChangeSet {
    let steps = vec![
        step(
            1,
            "Rename Cart::sum to Cart::total and Cart::count to Cart::len",
            Some(Class::Neutral),
            vec![
                claim("c1", "rename", Some("shop_core::cart::Cart::sum"), json!({ "to": "total" })),
                claim("c2", "rename", Some("shop_core::cart::Cart::count"), json!({ "to": "len" })),
            ],
        ),
        step(
            2,
            "Return 0 for an empty cart",
            Some(Class::ControlFlow),
            vec![
                claim("c3", "add-if", Some(&format!("{TOTAL}/S0")), json!({ "cond": "self.items.is_empty()", "then": "return 0;" })),
                claim("c4", "change-operator", Some(&format!("{TOTAL}/S1.0/cond")), json!({ "from": ">", "to": ">=" })),
            ],
        ),
        step(
            3,
            "Test the empty cart",
            None,
            vec![claim("c5", "add-item", Some("shop_core::cart::tests::empty_cart_total"), json!({ "kind": "fn", "test": true }))],
        ),
        step(
            4,
            "Extract the price of a line",
            Some(Class::Neutral),
            vec![claim("c6", "extract-function", Some(&format!("{TOTAL}/S2.1")), json!({ "new": "shop_core::cart::line_total" }))],
        ),
        step(
            5,
            "Move line_total to the billing module",
            Some(Class::Neutral),
            vec![
                claim("c7", "move-item", Some("shop_core::cart::line_total"), json!({ "to": "shop_core::billing::line_total" })),
                claim("c8", "invert-if", Some("shop_core::billing::line_total/S0"), json!({})),
            ],
        ),
        step(
            6,
            "Return Result from Cart::total",
            Some(Class::ControlFlow),
            vec![
                claim("c9", "add-item", Some("shop_core::cart::CartError"), json!({ "kind": "enum" })),
                claim("c10", "change-signature", Some(TOTAL), json!({ "from": "-> u32", "to": "-> Result<u32, CartError>" })),
                claim("c11", "wrap-expression", Some(&format!("{TOTAL}/tail")), json!({ "with": "Ok" })),
                claim("c12", "wrap-expression", Some(&format!("{TOTAL}/S0/then/S0/value")), json!({ "with": "Ok" })),
                claim("c13", "delete-statement", Some(&format!("{TOTAL}/S2.0/then/S0")), json!({})),
                claim("c14", "add-return", Some(&format!("{TOTAL}/S2.0/then/S0")), json!({ "value": "Err(CartError::TooMany(item.qty))" })),
                claim("c15", "bump-dependency", None, json!({ "crate": "thiserror", "to": "2.0", "file": "shop_core/Cargo.toml" })),
            ],
        ),
        step(
            7,
            "Update the callers of Cart::total",
            Some(Class::Logic),
            vec![
                claim(
                    "c16",
                    "rewrite-pattern",
                    Some("shop_cli::run/S1/init"),
                    json!({ "from": "$c.total()", "to": "$c.total()?", "sites": ["shop_cli::run/S1/init"] }),
                ),
                claim(
                    "c17",
                    "replace-expression",
                    Some("shop_cli::run/S0/init"),
                    json!({ "from": "load_cart(&path).unwrap()", "to": "load_cart(&path)?" }),
                ),
                claim("c18", "remove-item", Some("shop_core::report::legacy_total"), json!({})),
            ],
        ),
    ];
    let groups = vec![
        Group {
            id: "G1".into(),
            title: "Return 0 for an empty cart".into(),
            kind: GroupKind::Fix,
            why: "The report showed a wrong total for an empty cart.".into(),
            steps: vec![1, 2, 3],
        },
        Group {
            id: "G2".into(),
            title: "Return Result from Cart::total".into(),
            kind: GroupKind::Api,
            why: "A quantity over the limit is an error now. Before, the cart skipped the item without a message.".into(),
            steps: vec![6, 7],
        },
        Group {
            id: "G3".into(),
            title: "Extract pricing into billing module".into(),
            kind: GroupKind::Refactor,
            why: "The billing module gets the price rules, so the cart only adds the lines.".into(),
            steps: vec![4, 5],
        },
    ];
    // The order here is not the review order: the UI sorts the subgroups by role.
    let subgroups = vec![
        subgroup("G1", Role::Mechanical, "Renames", &["c1", "c2"], false, ""),
        subgroup("G1", Role::Core, "Return early for an empty cart", &["c3"], false, ""),
        subgroup(
            "G1",
            Role::Custom,
            "Quantity limit",
            &["c4"],
            true,
            "The agent moved this claim out of Core logic, because the limit is a separate concern.",
        ),
        subgroup("G1", Role::Tests, "Cover the empty cart", &["c5"], false, ""),
        subgroup("G2", Role::Interface, "Error type and signature", &["c9", "c10"], false, ""),
        subgroup("G2", Role::Core, "Return errors for bad quantities", &["c11", "c12", "c13", "c14", "c18"], false, ""),
        subgroup("G2", Role::Config, "Dependency for the error type", &["c15"], false, ""),
        subgroup("G2", Role::Propagation, "Call sites", &["c16", "c17"], false, ""),
        subgroup("G3", Role::Mechanical, "Extract and move line_total", &["c6", "c7"], false, ""),
        subgroup("G3", Role::Custom, "Discount check", &["c8"], true, "The agent says that the inverted check is neutral."),
    ];
    let mut subgroups = subgroups;
    for sub in &mut subgroups {
        // Old data: a subgroup without an ID. The UI gives it the ID `G1:2`.
        if sub.title == "Quantity limit" {
            sub.id.clear();
        }
        // The agent edited this subgroup. Then the engine added c18 (step 7) to it.
        if sub.title == "Return errors for bad quantities" {
            sub.edited = true;
            sub.new_claims = vec!["c18".into()];
        }
    }
    ChangeSet {
        base_tree: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
        head_tree: "9c1f0e2d3b4a5968778695a4b3c2d1e0f9a8b7c6".into(),
        steps,
        groups,
        subgroups,
        pair_refusals: Vec::new(),
    }
}

/// Builds the verification report for [`demo_changeset`].
pub fn demo_report() -> Report {
    let mut c4 = texts(verified("c4", 2, "smt", Class::Logic), "item.qty > MAX_QTY", "item.qty >= MAX_QTY");
    c4.effect = Some(Effect::Widen);
    c4.witness = Some("item.qty = 100, MAX_QTY = 100".into());
    let mut c8 = texts(
        result("c8", 5, ClaimStatus::Unproved { reason: "N0 to N4 do not make the MIR equal: the branches compare a float.".into() }, None),
        "if item.discount == 0.0 { base } else { base - item.discount }",
        "if item.discount != 0.0 { base - item.discount } else { base }",
    );
    c8.hint = Some("Tag the claim as change-condition (logic), or keep the old order of the branches.".into());
    let steps = vec![
        StepReport {
            step: 1,
            intent: "Rename Cart::sum to Cart::total and Cart::count to Cart::len".into(),
            declared_class: Some(Class::Neutral),
            computed_class: Some(Class::Neutral),
            claims: vec![
                texts(verified("c1", 1, "N1", Class::Neutral), "fn sum(&self)", "fn total(&self)"),
                texts(verified("c2", 1, "N1", Class::Neutral), "fn count(&self)", "fn len(&self)"),
            ],
            ..Default::default()
        },
        StepReport {
            step: 2,
            intent: "Return 0 for an empty cart".into(),
            declared_class: Some(Class::ControlFlow),
            computed_class: Some(Class::ControlFlow),
            claims: vec![texts(verified("c3", 2, "shape", Class::ControlFlow), "", "if self.items.is_empty() {\n    return 0;\n}"), c4],
            ..Default::default()
        },
        StepReport {
            step: 3,
            intent: "Test the empty cart".into(),
            declared_class: None,
            computed_class: Some(Class::Logic),
            claims: vec![texts(
                verified("c5", 3, "shape", Class::Logic),
                "",
                "#[test]\nfn empty_cart_total() {\n    assert_eq!(Cart::default().total(), Ok(0));\n}",
            )],
            ..Default::default()
        },
        StepReport {
            step: 4,
            intent: "Extract the price of a line".into(),
            declared_class: Some(Class::Neutral),
            computed_class: Some(Class::Neutral),
            claims: vec![texts(verified("c6", 4, "N1", Class::Neutral), "sum += item.price * item.qty;", "sum += line_total(item);")],
            ..Default::default()
        },
        StepReport {
            step: 5,
            intent: "Move line_total to the billing module".into(),
            declared_class: Some(Class::Neutral),
            computed_class: None,
            claims: vec![texts(verified("c7", 5, "N1", Class::Neutral), "cart::line_total", "billing::line_total"), c8],
            ..Default::default()
        },
        StepReport {
            step: 6,
            intent: "Return Result from Cart::total".into(),
            declared_class: Some(Class::ControlFlow),
            computed_class: Some(Class::ControlFlow),
            claims: vec![
                texts(
                    verified("c9", 6, "shape", Class::Logic),
                    "",
                    "#[derive(Debug, PartialEq, thiserror::Error)]\npub enum CartError {\n    #[error(\"quantity {0} is over the limit\")]\n    TooMany(u32),\n}",
                ),
                texts(verified("c10", 6, "shape", Class::Logic), "-> u32", "-> Result<u32, CartError>"),
                texts(verified("c11", 6, "shape", Class::Logic), "sum", "Ok(sum)"),
                texts(verified("c12", 6, "shape", Class::Logic), "return 0;", "return Ok(0);"),
                texts(verified("c13", 6, "shape", Class::ControlFlow), "continue;", ""),
                texts(verified("c14", 6, "shape", Class::ControlFlow), "", "return Err(CartError::TooMany(item.qty));"),
                texts(verified("c15", 6, "file", Class::Other), "", "thiserror = \"2.0\""),
            ],
            ..Default::default()
        },
        StepReport {
            step: 7,
            // Removed: the unwrap in shop_cli::run and the expect in legacy_total.
            panic_delta: -2,
            intent: "Update the callers of Cart::total".into(),
            declared_class: Some(Class::Logic),
            computed_class: Some(Class::Logic),
            claims: vec![
                texts(verified("c16", 7, "shape", Class::Logic), "cart.total()", "cart.total()?"),
                texts(verified("c17", 7, "free", Class::Logic), "load_cart(&path).unwrap()", "load_cart(&path)?"),
                texts(
                    verified("c18", 7, "shape", Class::Logic),
                    "pub fn legacy_total(cart: &Cart) -> u32 {\n    cart.total().checked_add(0).expect(\"total\")\n}",
                    "",
                ),
            ],
            uncovered: vec![Uncovered {
                step: 7,
                view: View::Syntax,
                at: Some("shop_core::report::summary/S0/init".parse().expect("valid anchor")),
                file: None,
                description: "cart.total() → cart.total().unwrap_or(0)".into(),
                hint: Some("Add this site to the rewrite-pattern claim c16, or add a replace-expression claim.".into()),
            }],
            ..Default::default()
        },
    ];
    let mut report = Report {
        steps,
        organization: Vec::new(),
        warnings: vec!["G3 is a refactor group, but claim c8 is not proved neutral.".into()],
        score: 0.0,
        panic_delta: 0,
        api_level: None,
        accepted: false,
        phase: "full".into(),
        pending: 0,
        ..Default::default()
    };
    report.finish();
    report
}

fn tree(id: &str, label: &str, status: Status) -> TreeNode {
    TreeNode {
        id: id.to_string(),
        label: label.to_string(),
        status,
        class: None,
        claims: Vec::new(),
        diff: None,
        summary: None,
        children: Vec::new(),
    }
}

trait TreeExt {
    fn class(self, class: Class) -> Self;
    fn claims(self, claims: &[&str]) -> Self;
    fn diff(self, diff: &str) -> Self;
    fn summary(self, summary: &str) -> Self;
    fn children(self, children: Vec<TreeNode>) -> Self;
}

impl TreeExt for TreeNode {
    fn class(mut self, class: Class) -> Self {
        self.class = Some(class);
        self
    }
    fn claims(mut self, claims: &[&str]) -> Self {
        self.claims = claims.iter().map(|claim| claim.to_string()).collect();
        self
    }
    fn diff(mut self, diff: &str) -> Self {
        self.diff = Some(diff.to_string());
        self
    }
    fn summary(mut self, summary: &str) -> Self {
        self.summary = Some(summary.to_string());
        self
    }
    fn children(mut self, children: Vec<TreeNode>) -> Self {
        self.children = children;
        self
    }
}

/// Builds the change tree of the whole change set (design section 11.4).
pub fn demo_tree() -> TreeNode {
    let t = |path: &str| format!("{TOTAL}/{path}");
    let total = tree(TOTAL, "fn Cart::sum → Cart::total", Status::Changed)
        .claims(&["c1"])
        .summary("3 ctrl flow · 5 logic · 1 extract · 1 rename")
        .children(vec![
            tree(&t("signature"), "signature", Status::Changed)
                .class(Class::Logic)
                .claims(&["c10"])
                .diff("⟦-> u32⟧→⟦-> Result<u32, CartError>⟧"),
            tree(&t("S0"), "if self.items.is_empty() { return Ok(0) }", Status::Added).class(Class::ControlFlow).claims(&["c3", "c12"]),
            tree(&t("S1"), "let mut sum = 0;", Status::Same),
            tree(&t("S2"), "for item in &self.items", Status::Changed).summary("2 ctrl flow · 1 logic · 1 extract").children(vec![
                tree(&t("S2/body/S0"), "if item.qty >= MAX_QTY …", Status::Changed).class(Class::ControlFlow).children(vec![
                    tree(&t("S2/body/S0/cond"), "cond", Status::Changed)
                        .class(Class::Logic)
                        .claims(&["c4"])
                        .diff("item.qty ⟦>⟧→⟦>=⟧ MAX_QTY"),
                    tree(&t("S2/body/S0/then/S0"), "continue;", Status::Removed).class(Class::ControlFlow).claims(&["c13"]),
                    tree(&t("S2/body/S0/then/S1"), "return Err(CartError::TooMany(item.qty));", Status::Added)
                        .class(Class::ControlFlow)
                        .claims(&["c14"]),
                ]),
                tree(&t("S2/body/S1"), "sum += item.price * item.qty  →  line_total(item)", Status::Extracted)
                    .class(Class::Neutral)
                    .claims(&["c6"]),
            ]),
            tree(&t("tail"), "tail", Status::Changed).class(Class::Logic).claims(&["c11"]).diff("⟦sum⟧→⟦Ok(sum)⟧"),
        ]);
    let cart = tree("shop_core::cart", "mod cart", Status::Changed).summary("3 ctrl flow · 7 logic · 3 neutral").children(vec![
        total,
        tree("shop_core::cart::Cart::len", "fn Cart::count → Cart::len", Status::Moved).class(Class::Neutral).claims(&["c2"]),
        tree("shop_core::cart::CartError", "enum CartError", Status::Added).class(Class::Logic).claims(&["c9"]),
        tree("shop_core::cart::tests", "mod tests", Status::Changed).summary("1 new test").children(vec![
            tree("shop_core::cart::tests::empty_cart_total", "fn empty_cart_total", Status::Added).class(Class::Logic).claims(&["c5"]),
            tree("shop_core::cart::tests::len_counts_items", "fn len_counts_items", Status::Same),
        ]),
        tree("shop_core::cart::Cart", "struct Cart", Status::Same),
        tree("shop_core::cart::MAX_QTY", "const MAX_QTY", Status::Same),
    ]);
    let billing = tree("shop_core::billing", "mod billing", Status::Added).children(vec![
        tree("shop_core::billing::line_total", "fn line_total   ⤴ from Cart::total", Status::Extracted)
            .class(Class::Neutral)
            .claims(&["c7"])
            .summary("1 unproved")
            .children(vec![
                tree("shop_core::billing::line_total/S0", "if item.discount …", Status::Changed)
                    .claims(&["c8"])
                    .diff("if ⟦item.discount == 0.0⟧→⟦item.discount != 0.0⟧, branches swapped"),
            ]),
    ]);
    let report = tree("shop_core::report", "mod report", Status::Changed).summary("1 uncovered · 1 logic").children(vec![
        tree("shop_core::report::summary", "fn summary", Status::Uncovered).children(vec![
            tree("shop_core::report::summary/S0", "let total = …", Status::Uncovered).diff("⟦cart.total()⟧→⟦cart.total().unwrap_or(0)⟧"),
        ]),
        tree("shop_core::report::legacy_total", "fn legacy_total", Status::Removed).class(Class::Logic).claims(&["c18"]),
        tree("shop_core::report::format_line", "fn format_line", Status::Same),
    ]);
    let core = tree("shop_core", "crate shop_core", Status::Changed)
        .summary("3 ctrl flow · 8 logic · 3 neutral · 1 other · 1 uncovered")
        .children(vec![
            cart,
            billing,
            report,
            tree("shop_core/Cargo.toml", "Cargo.toml", Status::Changed)
                .class(Class::Other)
                .claims(&["c15"])
                .diff("⟦⟧→⟦thiserror = \"2.0\"⟧"),
        ]);
    let cli = tree("shop_cli", "crate shop_cli", Status::Changed).summary("2 logic").children(vec![
        tree("shop_cli::run", "fn run", Status::Changed).summary("2 logic").children(vec![
            tree("shop_cli::run/S0", "let cart = …", Status::Changed)
                .class(Class::Logic)
                .claims(&["c17"])
                .diff("load_cart(&path)⟦.unwrap()⟧→⟦?⟧"),
            tree("shop_cli::run/S1", "let total = …", Status::Changed).class(Class::Logic).claims(&["c16"]).diff("cart.total()⟦⟧→⟦?⟧"),
            tree("shop_cli::run/S2", "println!(\"{}\", report::summary(&cart))", Status::Same),
        ]),
        tree("shop_cli::main", "fn main", Status::Same),
        tree("shop_cli::load_cart", "fn load_cart", Status::Same),
    ]);
    tree("shop", "workspace shop", Status::Changed).children(vec![core, cli])
}

fn flow_node(id: &str, label: &str, kind: FlowKind, status: Status, anchor: Option<&str>) -> FlowNode {
    FlowNode { id: id.to_string(), label: label.to_string(), kind, status, anchor: anchor.map(str::to_string) }
}

fn flow_edge(from: &str, to: &str, label: Option<&str>, status: Status) -> FlowEdge {
    FlowEdge { from: from.to_string(), to: to.to_string(), label: label.map(str::to_string), status }
}

/// Builds two flow views: `Cart::total` and `shop_cli::run`.
pub fn demo_flows() -> Vec<FlowGraph> {
    use FlowKind::*;
    use Status::*;
    let t = |path: &str| format!("{TOTAL}/{path}");
    let total = FlowGraph {
        function: TOTAL.to_string(),
        breadcrumb: vec!["shop_core".into(), "cart".into(), "Cart::total".into()],
        nodes: vec![
            flow_node("n0", "from: shop_cli::run", Stub, Same, Some("shop_cli::run")),
            flow_node("n1", "from: report::summary", Stub, Uncovered, Some("shop_core::report::summary")),
            flow_node("n2", "entry: Cart::total", Entry, Same, None),
            flow_node("n3", "if self.items.is_empty()", Branch, Added, Some(&t("S0"))),
            flow_node("n4", "return Ok(0)", Exit, Added, Some(&t("S0"))),
            flow_node("n5", "let mut sum = 0", Statement, Same, Some(&t("S1"))),
            flow_node("n6", "for item in &self.items", Loop, Changed, Some(&t("S2"))),
            flow_node("n7", "item.qty ⟦>⟧→⟦>=⟧ MAX_QTY", Branch, Changed, Some(&t("S2/body/S0"))),
            flow_node("n8", "continue", Statement, Removed, Some(&t("S2/body/S0/then/S0"))),
            flow_node("n9", "return Err(CartError::TooMany)", Exit, Added, Some(&t("S2/body/S0/then/S1"))),
            flow_node("n10", "sum += line_total(item)", Statement, Extracted, Some(&t("S2/body/S1"))),
            flow_node("n11", "to: billing::line_total", Stub, Extracted, Some("shop_core::billing::line_total")),
            flow_node("n12", "return Ok(sum)", Exit, Changed, Some(&t("tail"))),
        ],
        edges: vec![
            flow_edge("n0", "n2", None, Same),
            flow_edge("n1", "n2", None, Same),
            flow_edge("n2", "n3", None, Added),
            flow_edge("n3", "n4", Some("yes"), Added),
            flow_edge("n3", "n5", Some("no"), Added),
            flow_edge("n5", "n6", None, Same),
            flow_edge("n6", "n7", Some("next"), Same),
            flow_edge("n7", "n8", Some("yes"), Removed),
            flow_edge("n8", "n6", None, Removed),
            flow_edge("n7", "n9", Some("yes"), Added),
            flow_edge("n7", "n10", Some("no"), Same),
            flow_edge("n10", "n11", Some("call"), Added),
            flow_edge("n10", "n6", None, Same),
            flow_edge("n6", "n12", Some("done"), Same),
        ],
    };
    let run = FlowGraph {
        function: "shop_cli::run".to_string(),
        breadcrumb: vec!["shop_cli".into(), "run".into()],
        nodes: vec![
            flow_node("n0", "from: main", Stub, Same, Some("shop_cli::main")),
            flow_node("n1", "entry: run", Entry, Same, None),
            flow_node("n2", "let cart = load_cart(&path)⟦.unwrap()⟧→⟦?⟧", Statement, Changed, Some("shop_cli::run/S0")),
            flow_node("n3", "to: load_cart", Stub, Same, Some("shop_cli::load_cart")),
            flow_node("n4", "return Err (from ?)", Exit, Added, Some("shop_cli::run/S0")),
            flow_node("n5", "let total = cart.total()⟦⟧→⟦?⟧", Statement, Changed, Some("shop_cli::run/S1")),
            flow_node("n6", "to: Cart::total", Stub, Changed, Some(TOTAL)),
            flow_node("n7", "return Err (from ?)", Exit, Added, Some("shop_cli::run/S1")),
            flow_node("n8", "… 1 statement unchanged", Summary, Same, Some("shop_cli::run/S2")),
            flow_node("n9", "to: report::summary", Stub, Uncovered, Some("shop_core::report::summary")),
            flow_node("n10", "return Ok(())", Exit, Same, None),
        ],
        edges: vec![
            flow_edge("n0", "n1", None, Same),
            flow_edge("n1", "n2", None, Same),
            flow_edge("n2", "n3", Some("call"), Same),
            flow_edge("n2", "n4", Some("err"), Added),
            flow_edge("n2", "n5", Some("ok"), Same),
            flow_edge("n5", "n6", Some("call"), Changed),
            flow_edge("n5", "n7", Some("err"), Added),
            flow_edge("n5", "n8", Some("ok"), Same),
            flow_edge("n8", "n9", Some("call"), Same),
            flow_edge("n8", "n10", None, Same),
        ],
    };
    vec![total, run]
}

#[allow(clippy::too_many_arguments)]
fn graph_node(
    id: &str,
    label: &str,
    kind: GraphNodeKind,
    parent: Option<&str>,
    status: Status,
    changes: usize,
    entry: bool,
    test: bool,
) -> GraphNode {
    GraphNode {
        file: file_of(id),
        id: id.to_string(),
        label: label.to_string(),
        kind,
        parent: parent.map(str::to_string),
        status,
        changes,
        badges: Vec::new(),
        entry,
        test,
    }
}

/// The file of a workspace item, relative to the workspace root.
fn file_of(id: &str) -> Option<String> {
    let file = if id.starts_with("shop_core::cart") {
        "shop_core/src/cart.rs"
    } else if id.starts_with("shop_core::billing") {
        "shop_core/src/billing.rs"
    } else if id.starts_with("shop_core::report") {
        "shop_core/src/report.rs"
    } else if id == "shop_core" {
        "shop_core/src/lib.rs"
    } else if id.starts_with("shop_cli") {
        "shop_cli/src/main.rs"
    } else {
        return None;
    };
    Some(file.to_string())
}

fn external(id: &str, badges: &[&str]) -> GraphNode {
    let mut node = graph_node(id, id, GraphNodeKind::External, None, Status::Same, 0, false, false);
    node.badges = badges.iter().map(|badge| badge.to_string()).collect();
    node
}

fn edge(from: &str, to: &str, kind: GraphEdgeKind, status: Status) -> GraphEdge {
    GraphEdge { from: from.to_string(), to: to.to_string(), kind, status }
}

/// Builds the workspace graph: 2 crates, 5 modules, 13 items, and 3 external functions.
pub fn demo_graph() -> WorkspaceGraph {
    use GraphNodeKind::*;
    use Status::*;
    let nodes = vec![
        graph_node("shop_core", "shop_core", Crate, None, Changed, 16, false, false),
        graph_node("shop_cli", "shop_cli", Crate, None, Changed, 2, false, false),
        graph_node("shop_core::cart", "cart", Module, Some("shop_core"), Changed, 13, false, false),
        graph_node("shop_core::cart::tests", "tests", Module, Some("shop_core::cart"), Changed, 1, false, true),
        graph_node("shop_core::billing", "billing", Module, Some("shop_core"), Added, 3, false, false),
        graph_node("shop_core::report", "report", Module, Some("shop_core"), Changed, 2, false, false),
        graph_node("shop_core::cart::Cart", "Cart", Type, Some("shop_core::cart"), Same, 0, true, false),
        graph_node("shop_core::cart::MAX_QTY", "MAX_QTY", Other, Some("shop_core::cart"), Same, 0, true, false),
        graph_node("shop_core::cart::CartError", "CartError", Type, Some("shop_core::cart"), Added, 1, true, false),
        graph_node(TOTAL, "Cart::total", Function, Some("shop_core::cart"), Changed, 9, true, false),
        graph_node("shop_core::cart::Cart::len", "Cart::len", Function, Some("shop_core::cart"), Moved, 1, true, false),
        graph_node(
            "shop_core::cart::tests::empty_cart_total",
            "empty_cart_total",
            Function,
            Some("shop_core::cart::tests"),
            Added,
            1,
            true,
            true,
        ),
        graph_node(
            "shop_core::cart::tests::len_counts_items",
            "len_counts_items",
            Function,
            Some("shop_core::cart::tests"),
            Same,
            0,
            true,
            true,
        ),
        graph_node("shop_core::billing::line_total", "line_total", Function, Some("shop_core::billing"), Extracted, 3, true, false),
        graph_node("shop_core::report::summary", "summary", Function, Some("shop_core::report"), Uncovered, 1, true, false),
        graph_node("shop_core::report::format_line", "format_line", Function, Some("shop_core::report"), Same, 0, false, false),
        graph_node("shop_core::report::legacy_total", "legacy_total", Function, Some("shop_core::report"), Removed, 1, true, false),
        graph_node("shop_cli::main", "main", Function, Some("shop_cli"), Same, 0, true, false),
        graph_node("shop_cli::run", "run", Function, Some("shop_cli"), Changed, 2, false, false),
        graph_node("shop_cli::load_cart", "load_cart", Function, Some("shop_cli"), Same, 0, false, false),
        external("std::fs::read_to_string", &["err"]),
        external("serde_json::from_str", &["err"]),
        external("std::process::exit", &["never"]),
    ];
    let edges = vec![
        edge("shop_cli", "shop_core", GraphEdgeKind::Depends, Same),
        edge("shop_cli::main", "shop_cli::run", GraphEdgeKind::Call, Same),
        edge("shop_cli::main", "std::process::exit", GraphEdgeKind::Call, Same),
        edge("shop_cli::run", "shop_cli::load_cart", GraphEdgeKind::Call, Same),
        edge("shop_cli::run", TOTAL, GraphEdgeKind::Call, Changed),
        edge("shop_cli::run", "shop_core::report::summary", GraphEdgeKind::Call, Same),
        edge("shop_cli::load_cart", "std::fs::read_to_string", GraphEdgeKind::Call, Same),
        edge("shop_cli::load_cart", "serde_json::from_str", GraphEdgeKind::Call, Same),
        edge("shop_core::report::summary", TOTAL, GraphEdgeKind::Call, Changed),
        edge("shop_core::report::summary", "shop_core::report::format_line", GraphEdgeKind::Call, Same),
        edge("shop_core::report::legacy_total", TOTAL, GraphEdgeKind::Call, Removed),
        edge(TOTAL, "shop_core::billing::line_total", GraphEdgeKind::Call, Added),
        edge("shop_core::cart::tests::empty_cart_total", TOTAL, GraphEdgeKind::Call, Added),
        edge("shop_core::cart::tests::len_counts_items", "shop_core::cart::Cart::len", GraphEdgeKind::Call, Same),
    ];
    WorkspaceGraph { nodes, edges }
}

fn pair(before: Option<&str>, after: Option<&str>) -> SourcePair {
    SourcePair { before: before.map(str::to_string), after: after.map(str::to_string) }
}

/// Builds the source text before and after, by anchor text.
pub fn demo_sources() -> BTreeMap<String, SourcePair> {
    let mut sources = BTreeMap::new();
    let mut add = |key: &str, value: SourcePair| {
        sources.insert(key.to_string(), value);
    };
    add(
        TOTAL,
        pair(
            Some(
                "/// The sum of all line prices.\npub fn sum(&self) -> u32 {\n    let mut sum = 0;\n    for item in &self.items {\n        if item.qty > MAX_QTY {\n            continue;\n        }\n        sum += item.price * item.qty;\n    }\n    sum\n}",
            ),
            Some(
                "/// The sum of all line prices.\npub fn total(&self) -> Result<u32, CartError> {\n    if self.items.is_empty() {\n        return Ok(0);\n    }\n    let mut sum = 0;\n    for item in &self.items {\n        if item.qty >= MAX_QTY {\n            return Err(CartError::TooMany(item.qty));\n        }\n        sum += billing::line_total(item);\n    }\n    Ok(sum)\n}",
            ),
        ),
    );
    add(&format!("{TOTAL}/S2/body/S0/cond"), pair(Some("item.qty > MAX_QTY"), Some("item.qty >= MAX_QTY")));
    add(
        "shop_core::cart::Cart::len",
        pair(
            Some("pub fn count(&self) -> usize {\n    self.items.len()\n}"),
            Some("pub fn len(&self) -> usize {\n    self.items.len()\n}"),
        ),
    );
    add(
        "shop_core::cart::CartError",
        pair(
            None,
            Some(
                "#[derive(Debug, PartialEq, thiserror::Error)]\npub enum CartError {\n    #[error(\"quantity {0} is over the limit\")]\n    TooMany(u32),\n}",
            ),
        ),
    );
    add(
        "shop_core::cart::tests::empty_cart_total",
        pair(None, Some("#[test]\nfn empty_cart_total() {\n    assert_eq!(Cart::default().total(), Ok(0));\n}")),
    );
    let len_test =
        "#[test]\nfn len_counts_items() {\n    let cart = Cart::with_items(vec![Item::new(10, 2)]);\n    assert_eq!(cart.len(), 1);\n}";
    add("shop_core::cart::tests::len_counts_items", pair(Some(len_test), Some(len_test)));
    add(
        "shop_core::billing::line_total",
        pair(
            None,
            Some(
                "pub fn line_total(item: &Item) -> u32 {\n    let base = item.price * item.qty;\n    if item.discount != 0.0 {\n        base - item.discount\n    } else {\n        base\n    }\n}",
            ),
        ),
    );
    add(
        "shop_core::report::summary",
        pair(
            Some("pub fn summary(cart: &Cart) -> String {\n    let total = cart.total();\n    format_line(\"total\", total)\n}"),
            Some(
                "pub fn summary(cart: &Cart) -> String {\n    let total = cart.total().unwrap_or(0);\n    format_line(\"total\", total)\n}",
            ),
        ),
    );
    add(
        "shop_core::report::legacy_total",
        pair(Some("pub fn legacy_total(cart: &Cart) -> u32 {\n    cart.total().checked_add(0).expect(\"total\")\n}"), None),
    );
    let format_line = "fn format_line(name: &str, value: u32) -> String {\n    let bytes = format!(\"{name}: {value}\").into_bytes();\n    // SAFETY: format! gives valid UTF-8.\n    unsafe { String::from_utf8_unchecked(bytes) }\n}";
    add("shop_core::report::format_line", pair(Some(format_line), Some(format_line)));
    let main = "fn main() {\n    let path = std::env::args().nth(1).unwrap_or_else(|| \"cart.json\".into());\n    if let Err(error) = run(&path) {\n        eprintln!(\"error: {error}\");\n        std::process::exit(1);\n    }\n}";
    add("shop_cli::main", pair(Some(main), Some(main)));
    add(
        "shop_cli::run",
        pair(
            Some(
                "fn run(path: &str) -> Result<(), Box<dyn Error>> {\n    let cart = load_cart(&path).unwrap();\n    let total = cart.total();\n    println!(\"{}\", report::summary(&cart));\n    Ok(())\n}",
            ),
            Some(
                "fn run(path: &str) -> Result<(), Box<dyn Error>> {\n    let cart = load_cart(&path)?;\n    let total = cart.total()?;\n    println!(\"{}\", report::summary(&cart));\n    Ok(())\n}",
            ),
        ),
    );
    let load = "fn load_cart(path: &str) -> Result<Cart, Box<dyn Error>> {\n    let text = std::fs::read_to_string(path)?;\n    Ok(serde_json::from_str(&text)?)\n}";
    add("shop_cli::load_cart", pair(Some(load), Some(load)));
    add("shop_core/Cargo.toml", pair(Some("[dependencies]\nserde = \"1\""), Some("[dependencies]\nserde = \"1\"\nthiserror = \"2.0\"")));
    sources
}

/// Builds the full demo data set.
pub fn demo_data() -> UiData {
    UiData {
        changeset: demo_changeset(),
        report: demo_report(),
        tree: demo_tree(),
        flows: demo_flows(),
        graph: demo_graph(),
        sources: demo_sources(),
    }
}
