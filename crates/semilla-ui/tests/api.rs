//! Integration tests: build the router with the demo data and call every endpoint.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use semilla_ui::fixture::demo_data;
use semilla_ui::{UiData, router};

const TOTAL: &str = "shop_core::cart::Cart::total";

fn enc(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// A new review file path in the target directory of the tests.
fn review_path() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-ui-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("review-{}-{}.json", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_file(&path);
    path
}

fn app() -> Router {
    router(demo_data(), review_path())
}

struct Reply {
    status: StatusCode,
    content_type: String,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| panic!("bad JSON ({error}): {}", String::from_utf8_lossy(&self.body)))
    }
}

async fn call(app: &Router, method: &str, uri: &str) -> Reply {
    let request = Request::builder().method(method).uri(uri).body(Body::empty()).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).map(|value| value.to_str().unwrap().to_string()).unwrap_or_default();
    let body = response.into_body().collect().await.unwrap().to_bytes().to_vec();
    Reply { status, content_type, body }
}

async fn get(app: &Router, uri: &str) -> Value {
    let reply = call(app, "GET", uri).await;
    assert_eq!(reply.status, StatusCode::OK, "GET {uri}: {}", String::from_utf8_lossy(&reply.body));
    assert!(reply.content_type.starts_with("application/json"), "GET {uri}: {}", reply.content_type);
    reply.json()
}

fn ids(values: &Value) -> Vec<String> {
    values.as_array().unwrap().iter().map(|value| value["id"].as_str().unwrap().to_string()).collect()
}

fn group<'a>(changeset: &'a Value, id: &str) -> &'a Value {
    changeset["groups"].as_array().unwrap().iter().find(|group| group["id"] == id).unwrap()
}

fn subgroup<'a>(group: &'a Value, title: &str) -> &'a Value {
    group["subgroups"].as_array().unwrap().iter().find(|sub| sub["title"] == title).unwrap()
}

// ------------------------------------------------------------------ change set

#[tokio::test]
async fn changeset_summary() {
    let app = app();
    let cs = get(&app, "/api/changeset").await;
    let report = demo_data().report;
    assert_eq!(cs["coverage"].as_f64().unwrap(), report.score);
    assert_eq!(cs["accepted"], false);
    assert_eq!(cs["group_count"], 3);
    assert_eq!(cs["step_count"], 7);
    assert_eq!(cs["counts"]["claims"], 18);
    assert_eq!(cs["counts"]["verified"], 17);
    assert_eq!(cs["counts"]["unproved"], 1);
    assert_eq!(cs["counts"]["uncovered"], 1);
    assert_eq!(cs["counts"]["reviewed"], 0);
    assert_eq!(cs["api_level"], "major");
    // Before: run has one unwrap, legacy_total has one expect. After: none.
    assert_eq!(cs["panic_delta"], -2);
    assert_eq!(cs["warnings"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn groups_by_risk_then_dependency() {
    let app = app();
    let cs = get(&app, "/api/changeset").await;
    let order = ids(&cs["groups"]);
    // G2 has the highest risk, but it uses Cart::total, which G1 creates (the rename).
    assert_eq!(order, ["G1", "G2", "G3"]);
    let g1 = group(&cs, "G1");
    let g2 = group(&cs, "G2");
    let g3 = group(&cs, "G3");
    assert!(g2["risk_score"].as_u64() > g1["risk_score"].as_u64());
    assert!(g1["risk_score"].as_u64() > g3["risk_score"].as_u64());
    assert_eq!(g2["risk"], 3);
    assert_eq!(g2["depends_on"], serde_json::json!(["G1"]));
    assert_eq!(g3["depends_on"], serde_json::json!(["G1"]));
    assert!(g2["risk_reasons"].as_array().unwrap().iter().any(|reason| reason == "public API"));
    assert_eq!(g2["uncovered"].as_array().unwrap().len(), 1);
    assert_eq!(g2["panic_delta"], -2);
}

#[tokio::test]
async fn subgroups_in_role_order() {
    let app = app();
    let cs = get(&app, "/api/changeset").await;
    let roles: Vec<&str> = group(&cs, "G1")["subgroups"].as_array().unwrap().iter().map(|sub| sub["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["core", "custom", "tests", "mechanical"]);
    let roles: Vec<&str> = group(&cs, "G2")["subgroups"].as_array().unwrap().iter().map(|sub| sub["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["interface", "core", "config", "propagation"]);
    // Claim results are joined in.
    let core = subgroup(group(&cs, "G1"), "Return early for an empty cart");
    let claim = &core["claims"][0];
    assert_eq!(claim["id"], "c3");
    assert_eq!(claim["status"], "verified");
    assert_eq!(claim["class"], "control_flow");
    assert_eq!(claim["result"]["proof"], "shape");
    assert_eq!(claim["flow"], TOTAL);
    assert_eq!(claim["review"], "not_reviewed");
}

#[tokio::test]
async fn fold_state_follows_contents() {
    let app = app();
    let cs = get(&app, "/api/changeset").await;
    let g1 = group(&cs, "G1");
    let g2 = group(&cs, "G2");
    let g3 = group(&cs, "G3");
    // Proved neutral claims: folded.
    assert_eq!(subgroup(g1, "Renames")["folded"], true);
    assert_eq!(subgroup(g3, "Extract and move line_total")["folded"], true);
    // A logic claim opens the subgroup, also for a role that is folded by default.
    assert_eq!(subgroup(g1, "Quantity limit")["folded"], false);
    assert_eq!(subgroup(g2, "Call sites")["folded"], false);
    // An unproved claim opens the subgroup.
    assert_eq!(subgroup(g3, "Discount check")["folded"], false);
    // Badges for custom subgroups, and the agent marker.
    assert_eq!(subgroup(g1, "Quantity limit")["badges"], serde_json::json!(["1 logic"]));
    assert_eq!(subgroup(g1, "Quantity limit")["edited"], true);
    assert_eq!(subgroup(g3, "Discount check")["badges"], serde_json::json!(["1 unknown"]));
    assert_eq!(subgroup(g1, "Renames")["badges"], serde_json::json!([]));
    assert_eq!(group(&cs, "G3")["all_neutral"], false);
    // The location strip.
    assert_eq!(subgroup(g2, "Call sites")["locations"], serde_json::json!(["shop_cli/src/main.rs"]));
    assert_eq!(subgroup(g1, "Renames")["locations"], serde_json::json!(["shop_core/src/cart.rs"]));
    // A move shows the old and the new file.
    assert_eq!(
        subgroup(g3, "Extract and move line_total")["locations"],
        serde_json::json!(["shop_core/src/cart.rs", "shop_core/src/billing.rs"])
    );
    assert_eq!(subgroup(g2, "Dependency for the error type")["locations"], serde_json::json!(["shop_core/Cargo.toml"]));
}

#[tokio::test]
async fn fold_state_ignores_the_label() {
    let mut data = demo_data();
    // Put a logic claim into a Mechanical subgroup, and only neutral claims into a Core subgroup.
    for sub in &mut data.changeset.subgroups {
        if sub.title == "Renames" {
            sub.claims.push("c4".into());
        }
        if sub.title == "Quantity limit" {
            sub.claims.clear();
        }
        if sub.title == "Return early for an empty cart" {
            sub.role = semilla_core::Role::Core;
            sub.claims = vec!["c6".into()];
        }
    }
    for sub in &mut data.changeset.subgroups {
        if sub.title == "Extract and move line_total" {
            sub.claims.retain(|claim| claim != "c6");
        }
    }
    let app = router(data, review_path());
    let cs = get(&app, "/api/changeset").await;
    let g1 = group(&cs, "G1");
    let renames = subgroup(g1, "Renames");
    assert_eq!(renames["folded"], false);
    assert!(renames["label_mismatch"].is_string());
    let core = subgroup(g1, "Return early for an empty cart");
    assert_eq!(core["folded"], true, "{core}");
    // c3 has no subgroup now: the UI puts it into a synthetic subgroup.
    let loose = g1["subgroups"].as_array().unwrap().iter().find(|sub| sub["synthetic"] == true).expect("a synthetic subgroup");
    assert_eq!(ids(&loose["claims"]), ["c3"]);
}

#[tokio::test]
async fn group_endpoint() {
    let app = app();
    let g2 = get(&app, "/api/group/G2").await;
    assert_eq!(g2["id"], "G2");
    assert_eq!(g2["kind"], "api");
    let steps = g2["step_details"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[1]["index"], 7);
    assert_eq!(steps[1]["accepted"], false);
    assert_eq!(steps[1]["uncovered"][0]["at"], "shop_core::report::summary/S0/init");
    assert_eq!(call(&app, "GET", "/api/group/G9").await.status, StatusCode::NOT_FOUND);
}

// ------------------------------------------------------------------ nodes, flows, paths, sources

#[tokio::test]
async fn node_endpoints_accept_encoded_ids() {
    let app = app();
    let node = get(&app, &format!("/api/node/{}", enc(TOTAL))).await;
    assert_eq!(node["id"], TOTAL);
    assert_eq!(node["kind"], "function");
    assert_eq!(node["status"], "changed");
    assert_eq!(node["flow"], TOTAL);
    assert_eq!(node["tree"]["child_count"], 5);
    assert_eq!(node["breadcrumb"], serde_json::json!(["shop_core", "shop_core::cart", TOTAL]));
    // All the claims inside the function.
    let claims = ids(&node["claims"]);
    for id in ["c1", "c3", "c4", "c10", "c11", "c13", "c14"] {
        assert!(claims.contains(&id.to_string()), "{id} is missing in {claims:?}");
    }

    // A statement anchor, encoded and plain.
    let statement = format!("{TOTAL}/S2/body/S0/cond");
    let encoded = get(&app, &format!("/api/node/{}", enc(&statement))).await;
    let plain = get(&app, &format!("/api/node/{statement}")).await;
    assert_eq!(encoded["id"], statement);
    assert_eq!(encoded, plain);
    assert_eq!(encoded["kind"], "statement");
    assert_eq!(encoded["file"], "shop_core/src/cart.rs");
    assert_eq!(ids(&encoded["claims"]), ["c4"]);
    assert_eq!(encoded["parent"], format!("{TOTAL}/S2/body/S0"));

    let children = get(&app, &format!("/api/node/{}/children", enc(TOTAL))).await;
    assert_eq!(children["tree"].as_array().unwrap().len(), 5);
    assert_eq!(children["tree"][1]["marker"], "+");
    let module = get(&app, "/api/node/shop_core%3A%3Acart/children").await;
    assert!(ids(&module["graph"]).contains(&TOTAL.to_string()));

    let neighbors = get(&app, &format!("/api/node/{}/neighbors", enc(TOTAL))).await;
    let callers = ids(&neighbors["callers"]);
    assert!(callers.contains(&"shop_cli::run".to_string()));
    assert!(callers.contains(&"shop_core::report::summary".to_string()));
    assert_eq!(ids(&neighbors["callees"]), ["shop_core::billing::line_total"]);
    // The impact: all the functions that can reach Cart::total.
    let impact: Vec<&str> = neighbors["impact"].as_array().unwrap().iter().map(|id| id.as_str().unwrap()).collect();
    assert!(impact.contains(&"shop_cli::main"));
    assert!(impact.contains(&"shop_cli::run"));
    let crate_neighbors = get(&app, "/api/node/shop_cli/neighbors").await;
    assert_eq!(ids(&crate_neighbors["depends_on"]), ["shop_core"]);

    assert_eq!(call(&app, "GET", "/api/node/nope%3A%3Anothing").await.status, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "GET", "/api/node/nope/neighbors").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn flow_endpoint() {
    let app = app();
    let flow = get(&app, &format!("/api/flow/{}", enc(TOTAL))).await;
    assert_eq!(flow["function"], TOTAL);
    assert_eq!(flow["synthetic"], false);
    let mermaid = flow["mermaid"].as_str().unwrap();
    assert!(mermaid.starts_with("flowchart TD"));
    assert!(mermaid.contains(":::changed"));
    assert_eq!(flow["flow"]["nodes"].as_array().unwrap().len(), 13);
    // The stubs link to other flow views.
    assert_eq!(flow["links"]["n0"]["target"], "shop_cli::run");
    assert_eq!(flow["links"]["n0"]["flow"], true);
    // A statement inside the frame does not open a new flow view.
    assert_eq!(flow["links"]["n7"]["flow"], false);
    assert_eq!(flow["breadcrumb"][1]["id"], "shop_core::cart");

    // A statement anchor resolves to the frame that contains it.
    let inner = get(&app, &format!("/api/flow/{}", enc(&format!("{TOTAL}/S2/body/S0")))).await;
    assert_eq!(inner["function"], TOTAL);

    // A function without a flow view gets a synthetic one, with callers and callees as stubs.
    let main = get(&app, "/api/flow/shop_cli%3A%3Amain").await;
    assert_eq!(main["synthetic"], true);
    assert!(main["mermaid"].as_str().unwrap().contains("to: run"));
    assert_eq!(call(&app, "GET", "/api/flow/shop_core%3A%3Acart").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn path_endpoint() {
    let app = app();
    let paths = get(&app, &format!("/api/path?from=main&to={}", enc("Cart::total"))).await;
    assert_eq!(paths["to"], TOTAL);
    let list = paths["paths"].as_array().unwrap();
    assert!(list.contains(&serde_json::json!(["shop_cli::main", "shop_cli::run", TOTAL])));
    assert!(list.contains(&serde_json::json!(["shop_cli::main", "shop_cli::run", "shop_core::report::summary", TOTAL])));
    // Without `from`, the paths start at the entry points. The test layer is off by default.
    let all = get(&app, &format!("/api/path?to={}", enc(TOTAL))).await;
    let starts: Vec<&str> = all["paths"].as_array().unwrap().iter().map(|path| path[0].as_str().unwrap()).collect();
    assert!(starts.contains(&"shop_cli::main"));
    assert!(!starts.contains(&"shop_core::cart::tests::empty_cart_total"));
    let with_tests = get(&app, &format!("/api/path?tests=1&to={}", enc(TOTAL))).await;
    assert!(with_tests["paths"].as_array().unwrap().iter().any(|path| path[0] == "shop_core::cart::tests::empty_cart_total"));
    assert_eq!(call(&app, "GET", "/api/path?from=main").await.status, StatusCode::BAD_REQUEST);
    assert_eq!(call(&app, "GET", "/api/path?to=nothing_here").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn source_endpoint() {
    let app = app();
    let before = get(&app, &format!("/api/source/{}?v=before", enc(TOTAL))).await;
    let after = get(&app, &format!("/api/source/{}?v=after", enc(TOTAL))).await;
    assert!(before["text"].as_str().unwrap().contains("pub fn sum(&self) -> u32"));
    assert!(after["text"].as_str().unwrap().contains("pub fn total(&self) -> Result<u32, CartError>"));
    assert_eq!(after["from"], "sources");
    // The default version is "after".
    assert_eq!(get(&app, &format!("/api/source/{}", enc(TOTAL))).await["text"], after["text"]);
    // A new item has no before version.
    let new = get(&app, "/api/source/shop_core%3A%3Abilling%3A%3Aline_total?v=before").await;
    assert!(new["text"].is_null());
    // An exact statement key.
    let cond = get(&app, &format!("/api/source/{}?v=after", enc(&format!("{TOTAL}/S2/body/S0/cond")))).await;
    assert_eq!(cond["text"], "item.qty >= MAX_QTY");
    // A claim text, then the item.
    let claim = get(&app, &format!("/api/source/{}?v=after", enc(&format!("{TOTAL}/S0")))).await;
    assert_eq!(claim["from"], "claim");
    let item = get(&app, "/api/source/shop_cli%3A%3Arun%2FS2?v=after").await;
    assert_eq!(item["from"], "item");
    assert_eq!(item["key"], "shop_cli::run");
    assert_eq!(call(&app, "GET", &format!("/api/source/{}?v=middle", enc(TOTAL))).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(call(&app, "GET", "/api/source/nothing?v=after").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tree_and_graph_endpoints() {
    let app = app();
    let tree = get(&app, "/api/tree").await;
    let data = demo_data();
    assert_eq!(serde_json::from_value::<semilla_core::view::TreeNode>(tree).unwrap(), data.tree);
    let graph = get(&app, "/api/graph").await;
    assert_eq!(serde_json::from_value::<semilla_core::view::WorkspaceGraph>(graph).unwrap(), data.graph);
}

// ------------------------------------------------------------------ search

async fn search(app: &Router, query: &str) -> Value {
    get(app, &format!("/api/search?q={}", enc(query))).await
}

fn hit_ids(result: &Value) -> Vec<String> {
    ids(&result["results"])
}

#[tokio::test]
async fn search_fuzzy_and_text() {
    let app = app();
    let result = search(&app, "total").await;
    assert_eq!(hit_ids(&result)[0], TOTAL);
    let result = search(&app, "Cart::total").await;
    assert_eq!(hit_ids(&result)[0], TOTAL);
    // A subsequence finds the function too.
    assert!(hit_ids(&search(&app, "lntot").await).contains(&"shop_core::billing::line_total".to_string()));
    // Text search on labels and sources.
    let result = search(&app, "\"qty >= max\"").await;
    let found = hit_ids(&result);
    assert!(found.contains(&TOTAL.to_string()), "{found:?}");
    assert!(found.contains(&format!("{TOTAL}/S2/body/S0/cond")));
    let total_hit = result["results"].as_array().unwrap().iter().find(|hit| hit["id"] == TOTAL).unwrap();
    assert!(total_hit["matched"].as_array().unwrap().iter().any(|m| m == "source after"));
    assert!(search(&app, "").await["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn search_filters() {
    let app = app();
    let changed = hit_ids(&search(&app, "changed:fn").await);
    assert!(changed.contains(&"shop_cli::run".to_string()));
    assert!(!changed.contains(&"shop_cli::main".to_string()));
    assert!(!changed.contains(&"shop_core::cart".to_string()));

    let control = hit_ids(&search(&app, "class:ctrl_flow").await);
    assert!(control.contains(&TOTAL.to_string()));
    assert!(!control.contains(&"shop_cli::run".to_string()));
    let neutral = hit_ids(&search(&app, "class:neutral changed:fn").await);
    assert!(neutral.contains(&"shop_core::cart::Cart::len".to_string()));

    let step = hit_ids(&search(&app, "step:7").await);
    assert!(step.contains(&"shop_cli::run".to_string()));
    assert!(!step.contains(&"shop_core::billing::line_total".to_string()));

    let g3 = hit_ids(&search(&app, "group:G3 changed:fn").await);
    assert!(g3.contains(&"shop_core::billing::line_total".to_string()));
    assert!(!g3.contains(&"shop_cli::run".to_string()));

    // Fuzzy and a filter together.
    let combined = hit_ids(&search(&app, "line group:G3").await);
    assert_eq!(combined[0], "shop_core::billing::line_total");
}

#[tokio::test]
async fn search_site_kinds() {
    let app = app();
    let panic = hit_ids(&search(&app, "kind:panic").await);
    assert!(panic.contains(&"shop_core::report::legacy_total".to_string()), "{panic:?}");
    assert!(!panic.contains(&"shop_cli::main".to_string()), "unwrap_or_else is not a panic site");
    let unsafe_sites = hit_ids(&search(&app, "kind:unsafe").await);
    assert!(unsafe_sites.contains(&"shop_core::report::format_line".to_string()));
    assert!(!unsafe_sites.contains(&TOTAL.to_string()));
    let err = hit_ids(&search(&app, "kind:err").await);
    for id in ["shop_cli::load_cart", "shop_cli::run", TOTAL, "std::fs::read_to_string"] {
        assert!(err.contains(&id.to_string()), "{id} is missing in {err:?}");
    }
    assert!(!err.contains(&"shop_core::report::summary".to_string()));
}

#[tokio::test]
async fn search_graph_terms() {
    let app = app();
    let mut callers = hit_ids(&search(&app, "callers:Cart::total").await);
    callers.sort();
    assert_eq!(
        callers,
        ["shop_cli::run", "shop_core::cart::tests::empty_cart_total", "shop_core::report::legacy_total", "shop_core::report::summary"]
    );
    let mut callees = hit_ids(&search(&app, "callees:shop_cli::run").await);
    callees.sort();
    assert_eq!(callees, ["shop_cli::load_cart", TOTAL, "shop_core::report::summary"]);
    // Graph terms and other filters together.
    assert_eq!(hit_ids(&search(&app, "callers:Cart::total kind:err").await), ["shop_cli::run"]);

    let paths = search(&app, "path:main -> Cart::total").await;
    assert!(paths["results"].as_array().unwrap().is_empty());
    assert!(paths["paths"].as_array().unwrap().contains(&serde_json::json!(["shop_cli::main", "shop_cli::run", TOTAL])));
}

#[tokio::test]
async fn search_errors() {
    let app = app();
    for query in ["class:huge", "step:x", "kind:leak", "path:main", "callers:no_such_fn", "\"open quote"] {
        let result = search(&app, query).await;
        assert!(!result["query"]["errors"].as_array().unwrap().is_empty(), "no error for {query}");
    }
    assert!(hit_ids(&search(&app, "callers:no_such_fn").await).is_empty());
}

// ------------------------------------------------------------------ review state

fn claim_states(review: &Value) -> std::collections::BTreeMap<String, String> {
    review["claims"].as_object().unwrap().iter().map(|(key, value)| (key.clone(), value.as_str().unwrap().to_string())).collect()
}

#[tokio::test]
async fn review_marks_claims_groups_and_subgroups() {
    let file = review_path();
    let app = router(demo_data(), file.clone());

    let reply = call(&app, "POST", "/api/review/c4").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["kind"], "claim");
    assert_eq!(claim_states(&get(&app, "/api/review").await)["c4"], "reviewed");

    // A group marks all its claims.
    let reply = call(&app, "POST", "/api/review/G1").await.json();
    assert_eq!(reply["kind"], "group");
    assert_eq!(reply["claims"].as_array().unwrap().len(), 5);
    let cs = get(&app, "/api/changeset").await;
    assert_eq!(group(&cs, "G1")["reviewed"], true);
    assert_eq!(cs["counts"]["reviewed"], 5);

    // Unmark one claim: the group is not reviewed now, but its other subgroups are.
    assert_eq!(call(&app, "DELETE", "/api/review/c3").await.status, StatusCode::OK);
    let cs = get(&app, "/api/changeset").await;
    let g1 = group(&cs, "G1");
    assert_eq!(g1["reviewed"], false);
    assert_eq!(subgroup(g1, "Renames")["reviewed"], true);
    assert_eq!(subgroup(g1, "Return early for an empty cart")["reviewed"], false);

    // A subgroup by its ID.
    let mechanical = subgroup(group(&cs, "G3"), "Extract and move line_total")["id"].as_str().unwrap().to_string();
    let reply = call(&app, "POST", &format!("/api/review/{}", enc(&mechanical))).await.json();
    assert_eq!(reply["kind"], "subgroup");
    assert_eq!(reply["claims"], serde_json::json!(["c6", "c7"]));
    let cs = get(&app, "/api/changeset").await;
    assert_eq!(subgroup(group(&cs, "G3"), "Extract and move line_total")["reviewed"], true);
    assert_eq!(group(&cs, "G3")["reviewed"], false);

    assert_eq!(call(&app, "POST", "/api/review/nothing").await.status, StatusCode::NOT_FOUND);

    // The file has one hash for each reviewed claim.
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let mut stored: Vec<&String> = saved["claims"].as_object().unwrap().keys().collect();
    stored.sort();
    assert_eq!(stored, ["c1", "c2", "c4", "c5", "c6", "c7"]);
    assert_eq!(saved["claims"]["c1"]["hash"].as_str().unwrap().len(), 64);

    // A new router reads the same state.
    let again = router(demo_data(), file);
    assert_eq!(get(&again, "/api/changeset").await["counts"]["reviewed"], 6);
}

#[tokio::test]
async fn review_state_follows_the_claims() {
    let file = review_path();
    let app = router(demo_data(), file.clone());
    for target in ["G1", "G3"] {
        assert_eq!(call(&app, "POST", &format!("/api/review/{target}")).await.status, StatusCode::OK);
    }
    let states = claim_states(&get(&app, "/api/review").await);
    for id in ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"] {
        assert_eq!(states[id], "reviewed", "{id}");
    }

    // The agent changes its work: c4 gets other arguments, and the source of line_total changes
    // (that is the item of c8). It also adds a new step: the claims move, but they keep their
    // content.
    let mut data: UiData = demo_data();
    let c4 = data.changeset.steps.iter_mut().flat_map(|step| step.claims.iter_mut()).find(|claim| claim.id == "c4").unwrap();
    c4.args.insert("to".into(), Value::String(">".into()));
    data.sources.get_mut("shop_core::billing::line_total").unwrap().after = Some("pub fn line_total(item: &Item) -> u32 { 0 }".into());
    data.changeset.steps.rotate_left(1);
    let changed = router(data, file.clone());
    let states = claim_states(&get(&changed, "/api/review").await);
    assert_eq!(states["c4"], "changed");
    assert_eq!(states["c8"], "changed");
    for id in ["c1", "c2", "c3", "c5", "c6", "c7"] {
        assert_eq!(states[id], "reviewed", "{id} must keep its state");
    }
    let cs = get(&changed, "/api/changeset").await;
    assert_eq!(group(&cs, "G1")["reviewed"], false);
    assert_eq!(cs["counts"]["reviewed"], 6);
    let quantity = subgroup(group(&cs, "G1"), "Quantity limit");
    assert_eq!(quantity["claims"][0]["review"], "changed");

    // A changed claim text in the report also counts.
    let mut data = demo_data();
    for step in &mut data.report.steps {
        for result in &mut step.claims {
            if result.claim == "c1" {
                result.after = Some("fn total(&self) -> u64".into());
            }
        }
    }
    let states = claim_states(&get(&router(data, file.clone()), "/api/review").await);
    assert_eq!(states["c1"], "changed");

    // The original data gives the original state again.
    let states = claim_states(&get(&router(demo_data(), file), "/api/review").await);
    assert_eq!(states["c4"], "reviewed");
}

#[tokio::test]
async fn bad_review_file_starts_empty() {
    let file = review_path();
    std::fs::write(&file, "this is not JSON").unwrap();
    let app = router(demo_data(), file.clone());
    assert_eq!(get(&app, "/api/changeset").await["counts"]["reviewed"], 0);
    assert_eq!(call(&app, "POST", "/api/review/c1").await.status, StatusCode::OK);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(saved["claims"]["c1"].is_object());
}

// ------------------------------------------------------------------ static files

#[tokio::test]
async fn page_and_assets() {
    let app = app();
    let cases = [
        ("/", "text/html"),
        ("/index.html", "text/html"),
        ("/app.js", "text/javascript"),
        ("/app.css", "text/css"),
        ("/favicon.svg", "image/svg+xml"),
        ("/vendor/mermaid/mermaid.min.js", "text/javascript"),
        ("/vendor/cytoscape/cytoscape.min.js", "text/javascript"),
        ("/vendor/elkjs/elk.bundled.js", "text/javascript"),
        ("/vendor/cytoscape-elk/cytoscape-elk.js", "text/javascript"),
        ("/vendor/mermaid/LICENSE", "text/plain"),
        ("/vendor/cytoscape/LICENSE", "text/plain"),
        ("/vendor/elkjs/LICENSE.md", "text/plain"),
        ("/vendor/cytoscape-elk/LICENSE", "text/plain"),
        ("/vendor/VERSIONS.txt", "text/plain"),
    ];
    for (path, kind) in cases {
        let reply = call(&app, "GET", path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert!(reply.content_type.starts_with(kind), "{path}: {}", reply.content_type);
        assert!(!reply.body.is_empty(), "{path}");
    }
    let mermaid = call(&app, "GET", "/vendor/mermaid/mermaid.min.js").await;
    assert!(mermaid.body.len() > 1_000_000);
    let missing = call(&app, "GET", "/nothing.js").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let api_missing = call(&app, "GET", "/api/nothing").await;
    assert_eq!(api_missing.status, StatusCode::NOT_FOUND);
    assert!(api_missing.json()["error"].is_string());
}

#[tokio::test]
async fn page_works_offline() {
    let app = app();
    let html = String::from_utf8(call(&app, "GET", "/").await.body).unwrap();
    for script in [
        "/vendor/cytoscape/cytoscape.min.js",
        "/vendor/elkjs/elk.bundled.js",
        "/vendor/cytoscape-elk/cytoscape-elk.js",
        "/vendor/mermaid/mermaid.min.js",
        "/app.js",
    ] {
        assert!(html.contains(&format!("src=\"{script}\"")), "{script}");
    }
    let js = String::from_utf8(call(&app, "GET", "/app.js").await.body).unwrap();
    let css = String::from_utf8(call(&app, "GET", "/app.css").await.body).unwrap();
    for (name, text) in [("index.html", &html), ("app.js", &js), ("app.css", &css)] {
        // The SVG namespace is a name, not a remote resource.
        let text = text.replace("http://www.w3.org/2000/svg", "");
        assert!(!text.contains("http://") && !text.contains("https://"), "{name} refers to a remote URL");
    }
    // The UI uses the term "flow view".
    assert!(html.contains("Flow view"));
    assert!(css.contains("prefers-color-scheme: dark"));
}

#[test]
fn page_script_has_no_syntax_errors() {
    let has_node = std::process::Command::new("node").arg("--version").output().is_ok_and(|output| output.status.success());
    if !has_node {
        eprintln!("skip: node is not on this machine");
        return;
    }
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/app.js");
    let output = std::process::Command::new("node").arg("--check").arg(script).output().unwrap();
    assert!(output.status.success(), "node --check app.js:\n{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn ui_data_json_round_trip() {
    let data = demo_data();
    let text = serde_json::to_string(&data).unwrap();
    let back: UiData = serde_json::from_str(&text).unwrap();
    assert_eq!(back, data);
    // The CLI can leave out the optional parts.
    let minimal = serde_json::json!({
        "changeset": { "base_tree": "a", "head_tree": "b" },
        "report": { "steps": [], "organization": [], "warnings": [], "score": 1.0, "accepted": true },
        "tree": { "id": "w", "label": "workspace", "status": "same" },
    });
    let data: UiData = serde_json::from_value(minimal).unwrap();
    assert!(data.flows.is_empty() && data.sources.is_empty());
}

#[tokio::test]
async fn empty_data_works() {
    let data: UiData = serde_json::from_value(serde_json::json!({
        "changeset": { "base_tree": "a", "head_tree": "b" },
        "report": { "steps": [], "organization": [], "warnings": [], "score": 1.0, "accepted": true },
        "tree": { "id": "w", "label": "workspace", "status": "same" },
    }))
    .unwrap();
    let app = router(data, review_path());
    let cs = get(&app, "/api/changeset").await;
    assert_eq!(cs["groups"], serde_json::json!([]));
    assert_eq!(cs["api_level"], "none");
    assert_eq!(cs["panic_delta"], 0);
    assert!(search(&app, "total").await["results"].as_array().unwrap().is_empty());
    assert_eq!(get(&app, "/api/graph").await["nodes"], serde_json::json!([]));
}

#[tokio::test]
async fn locations_fall_back_to_modules() {
    let mut data = demo_data();
    for node in &mut data.graph.nodes {
        node.file = None;
    }
    let app = router(data, review_path());
    let cs = get(&app, "/api/changeset").await;
    assert_eq!(subgroup(group(&cs, "G2"), "Call sites")["locations"], serde_json::json!(["shop_cli"]));
    assert_eq!(
        subgroup(group(&cs, "G3"), "Extract and move line_total")["locations"],
        serde_json::json!(["shop_core::cart", "shop_core::billing"])
    );
}

#[tokio::test]
async fn subgroup_ids_and_new_claims() {
    let app = app();
    let cs = get(&app, "/api/changeset").await;
    let g1 = group(&cs, "G1");
    // The ID of the data.
    assert_eq!(subgroup(g1, "Renames")["id"], "G1:mechanical");
    // No ID in the data: the fallback `<group>:<n>` (the third subgroup of G1 in the change set).
    assert_eq!(subgroup(g1, "Quantity limit")["id"], "G1:2");
    let reply = call(&app, "POST", "/api/review/G1:core").await.json();
    assert_eq!(reply["kind"], "subgroup");
    assert_eq!(reply["claims"], serde_json::json!(["c3"]));
    assert_eq!(call(&app, "POST", "/api/review/G1%3A2").await.json()["claims"], serde_json::json!(["c4"]));

    // The engine added c18 after the agent's edit.
    let core = subgroup(group(&cs, "G2"), "Return errors for bad quantities");
    assert_eq!(core["edited"], true);
    assert_eq!(core["new_claims"], serde_json::json!(["c18"]));
    let claims = core["claims"].as_array().unwrap();
    let flag = |id: &str| claims.iter().find(|claim| claim["id"] == id).unwrap()["new_in_subgroup"].clone();
    assert_eq!(flag("c18"), true);
    assert_eq!(flag("c11"), false);
    assert_eq!(subgroup(g1, "Renames")["new_claims"], serde_json::json!([]));
}

#[tokio::test]
async fn duplicate_subgroup_ids_stay_unique() {
    let mut data = demo_data();
    for sub in &mut data.changeset.subgroups {
        if sub.group == "G3" {
            sub.id = "G3:same".into();
        }
    }
    let app = router(data, review_path());
    let cs = get(&app, "/api/changeset").await;
    let mut found: Vec<String> = ids(&group(&cs, "G3")["subgroups"]);
    found.sort();
    assert_eq!(found, ["G3:1", "G3:same"]);
}

#[tokio::test]
async fn panic_delta_comes_from_the_report() {
    let mut data = demo_data();
    // The sources do not change. Only the verifier values change.
    for step in &mut data.report.steps {
        step.panic_delta = match step.step {
            2 => 3,
            7 => -1,
            _ => 0,
        };
    }
    data.report.finish();
    let app = router(data, review_path());
    let cs = get(&app, "/api/changeset").await;
    assert_eq!(cs["panic_delta"], 2);
    let g1 = group(&cs, "G1");
    assert_eq!(g1["panic_delta"], 3);
    assert!(g1["risk_reasons"].as_array().unwrap().iter().any(|reason| reason == "panics +3"));
    assert_eq!(group(&cs, "G2")["panic_delta"], -1);
    assert_eq!(group(&cs, "G3")["panic_delta"], 0);
}
