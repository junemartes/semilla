//! The HTTP API (design section 11.10) and the static files.
//!
//! Node IDs contain `::` and `/`. The routes accept them URL-encoded (`%2F`) and also plain,
//! because the node routes take the rest of the path.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use semilla_core::view::{FlowGraph, TreeNode};

use crate::ctx::{Ctx, item_of};
use crate::review::{self, ReviewFile};
use crate::{assets, graph, search, summary};

struct AppState {
    ctx: Ctx,
    review: Mutex<ReviewFile>,
    review_file: PathBuf,
}

type Shared = Arc<AppState>;

/// A JSON response (batch 5k, A2). The JSON escape is sufficient for a control character. The
/// response also gives each other hidden character (a bidirectional control, a zero-width
/// character) as a `\uXXXX` escape of JSON, so the data has no raw hidden character. The UI
/// escapes each text again at output.
struct Json<T>(T);

impl<T: serde::Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        match serde_json::to_string(&self.0) {
            Ok(body) => ([(header::CONTENT_TYPE, "application/json")], json_hidden(&body)).into_response(),
            Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
        }
    }
}

/// A hidden character is legal only in a string of JSON: there, `\uXXXX` has the same value.
fn json_hidden(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for c in body.chars() {
        if semilla_core::text::is_hidden(c) && c != '\n' { out.push_str(&format!("\\u{:04x}", c as u32)) } else { out.push(c) }
    }
    out
}

/// An API error: a status code and a JSON body `{"error": "..."}`.
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn not_found(what: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("not found: {what}"))
}

type ApiResult = Result<Json<Value>, ApiError>;

pub fn router(ctx: Ctx, review_file: PathBuf) -> Router {
    let review = ReviewFile::load(&review_file);
    let state = Arc::new(AppState { ctx, review: Mutex::new(review), review_file });
    Router::new()
        .route("/api/changeset", get(changeset))
        .route("/api/group/{id}", get(group))
        .route("/api/search", get(search_handler))
        .route("/api/node/{*rest}", get(node))
        .route("/api/flow/{*node}", get(flow))
        .route("/api/path", get(path))
        .route("/api/source/{*id}", get(source))
        .route("/api/tree", get(tree))
        .route("/api/graph", get(workspace_graph))
        .route("/api/review", get(review_state))
        .route("/api/review/{*target}", post(review_mark).delete(review_unmark))
        .route("/api/pair", post(pair_decision))
        .fallback(static_file)
        .with_state(state)
}

fn review_of(state: &AppState) -> std::sync::MutexGuard<'_, ReviewFile> {
    state.review.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn changeset(State(state): State<Shared>) -> ApiResult {
    let review = review_of(&state);
    Ok(Json(serde_json::to_value(summary::changeset(&state.ctx, &review)).unwrap_or(Value::Null)))
}

async fn group(State(state): State<Shared>, Path(id): Path<String>) -> ApiResult {
    let review = review_of(&state);
    summary::group_detail(&state.ctx, &review, &id).map(Json).ok_or_else(|| not_found(format!("group {id}")))
}

#[derive(Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: String,
}

async fn search_handler(State(state): State<Shared>, Query(params): Query<SearchParams>) -> ApiResult {
    Ok(Json(serde_json::to_value(search::search(&state.ctx, &params.q)).unwrap_or(Value::Null)))
}

/// A tree node without its children, with the number of children.
fn shallow(node: &TreeNode) -> Value {
    json!({
        "id": node.id,
        "label": node.label,
        "status": node.status,
        "marker": node.status.marker(),
        "category": node.category,
        "compare": node.compare,
        "build_code": node.build_code,
        "claims": node.claims,
        "diff": node.diff,
        "summary": node.summary,
        "child_count": node.children.len(),
    })
}

async fn node(State(state): State<Shared>, Path(rest): Path<String>) -> ApiResult {
    let rest = rest.trim_start_matches('/');
    if let Some(id) = rest.strip_suffix("/children") {
        return node_children(&state.ctx, id);
    }
    if let Some(id) = rest.strip_suffix("/neighbors") {
        return node_neighbors(&state.ctx, id);
    }
    let review = review_of(&state);
    node_info(&state.ctx, &review, rest)
}

fn node_info(ctx: &Ctx, review: &ReviewFile, id: &str) -> ApiResult {
    let graph_node = ctx.graph_node(id);
    let tree_node = ctx.tree_node(id);
    let source = ctx.data.sources.get(id);
    let claims: Vec<Value> = ctx
        .all_claims()
        .filter(|(_, claim)| {
            claim.at.as_ref().is_some_and(|at| crate::ctx::contains(id, &at.to_string()))
                || tree_node.is_some_and(|node| node.claims.contains(&claim.id))
        })
        .map(|(_, claim)| serde_json::to_value(summary::claim_view(ctx, review, claim)).unwrap_or(Value::Null))
        .collect();
    if graph_node.is_none() && tree_node.is_none() && source.is_none() && claims.is_empty() {
        return Err(not_found(format!("node {id}")));
    }
    let (source_key, pair) = ctx.source(id).map(|(key, pair)| (Some(key.to_string()), Some(pair))).unwrap_or((None, None));
    let label = graph_node
        .map(|node| node.label.clone())
        .or_else(|| tree_node.map(|node| node.label.clone()))
        .unwrap_or_else(|| crate::ctx::last_segment(id).to_string());
    let kind = graph_node
        .and_then(|node| serde_json::to_value(node.kind).ok())
        .unwrap_or_else(|| Value::String(if id.contains('/') { "statement" } else { "item" }.to_string()));
    let parent = graph_node
        .and_then(|node| node.parent.clone())
        .or_else(|| ctx.tree_parent(id).map(|node| node.id.clone()))
        .or_else(|| id.rsplit_once('/').map(|(parent, _)| parent.to_string()));
    let flow = ctx.flow_function(id).or_else(|| graph::synthetic_flow(ctx, id).map(|flow| flow.function));
    Ok(Json(json!({
        "id": id,
        "label": label,
        "kind": kind,
        "status": graph_node.map(|node| node.status).or(tree_node.map(|node| node.status)),
        "category": tree_node.and_then(|node| node.category),
        "compare": tree_node.and_then(|node| node.compare),
        // Design 28, I6: the step whose build code change the compare used.
        "build_code": tree_node.and_then(|node| node.build_code),
        "graph": graph_node,
        "tree": tree_node.map(shallow),
        "claims": claims,
        "source": {
            "key": source_key,
            "before": pair.is_some_and(|pair| pair.before.is_some()),
            "after": pair.is_some_and(|pair| pair.after.is_some()),
        },
        "flow": flow,
        "parent": parent,
        "breadcrumb": ctx.graph_ancestors(item_of(id)),
        "module": summary::module_of(ctx, item_of(id)),
        "file": ctx.item_file(item_of(id)),
    })))
}

fn node_children(ctx: &Ctx, id: &str) -> ApiResult {
    let tree_node = ctx.tree_node(id);
    let graph_children: Vec<_> = ctx.data.graph.nodes.iter().filter(|node| node.parent.as_deref() == Some(id)).collect();
    if tree_node.is_none() && ctx.graph_node(id).is_none() {
        return Err(not_found(format!("node {id}")));
    }
    Ok(Json(json!({
        "id": id,
        "tree": tree_node.map(|node| node.children.iter().map(shallow).collect::<Vec<_>>()).unwrap_or_default(),
        "graph": graph_children,
    })))
}

fn node_neighbors(ctx: &Ctx, id: &str) -> ApiResult {
    let Some(node) = ctx.graph_node(id) else {
        return Err(not_found(format!("graph node {id}")));
    };
    let lite = |ids: &[String]| -> Vec<Value> {
        ids.iter()
            .map(|other| match ctx.graph_node(other) {
                Some(node) => json!({ "id": node.id, "label": node.label, "kind": node.kind, "status": node.status, "test": node.test }),
                None => json!({ "id": other, "label": crate::ctx::last_segment(other) }),
            })
            .collect()
    };
    let callers = ctx.callers.get(id).cloned().unwrap_or_default();
    let callees = ctx.callees.get(id).cloned().unwrap_or_default();
    let depends: Vec<String> = ctx
        .data
        .graph
        .edges
        .iter()
        .filter(|edge| edge.kind == semilla_core::view::GraphEdgeKind::Depends && edge.from == id)
        .map(|edge| edge.to.clone())
        .collect();
    let dependents: Vec<String> = ctx
        .data
        .graph
        .edges
        .iter()
        .filter(|edge| edge.kind == semilla_core::view::GraphEdgeKind::Depends && edge.to == id)
        .map(|edge| edge.from.clone())
        .collect();
    let children: Vec<String> =
        ctx.data.graph.nodes.iter().filter(|other| other.parent.as_deref() == Some(id)).map(|other| other.id.clone()).collect();
    let edges: Vec<_> = ctx.data.graph.edges.iter().filter(|edge| edge.from == id || edge.to == id).collect();
    let impact: Vec<String> = graph::transitive_callers(ctx, id).into_iter().collect();
    Ok(Json(json!({
        "id": id,
        "node": node,
        "parent": node.parent,
        "callers": lite(&callers),
        "callees": lite(&callees),
        "depends_on": lite(&depends),
        "dependents": lite(&dependents),
        "children": lite(&children),
        "impact": impact,
        "edges": edges,
    })))
}

/// The flow view response: the graph, its Mermaid text, and the click targets of its nodes.
fn flow_response(ctx: &Ctx, flow: &FlowGraph, synthetic: bool) -> Value {
    let links: BTreeMap<String, Value> = flow
        .nodes
        .iter()
        .filter_map(|node| {
            let target = node.anchor.clone()?;
            // The link opens another flow view only if the target is in another frame.
            let frame = ctx.flow_function(&target).or_else(|| graph::synthetic_flow(ctx, item_of(&target)).map(|flow| flow.function));
            let other_frame = frame.is_some_and(|frame| frame != flow.function);
            Some((node.id.clone(), json!({ "target": target, "flow": other_frame })))
        })
        .collect();
    // The breadcrumb as node IDs: each prefix of the breadcrumb, joined with `::`.
    let crumbs: Vec<Value> = flow
        .breadcrumb
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let id = flow.breadcrumb[..=index].join("::");
            let known = ctx.graph_node(&id).is_some();
            json!({ "label": label, "id": if known { Some(id) } else { None } })
        })
        .collect();
    json!({
        "function": flow.function,
        "flow": flow,
        "mermaid": flow.to_mermaid(),
        "links": links,
        "breadcrumb": crumbs,
        "synthetic": synthetic,
    })
}

async fn flow(State(state): State<Shared>, Path(node): Path<String>) -> ApiResult {
    let ctx = &state.ctx;
    let node = node.trim_start_matches('/');
    if let Some(flow) = ctx.flow_for(node) {
        return Ok(Json(flow_response(ctx, flow, false)));
    }
    match graph::synthetic_flow(ctx, item_of(node)) {
        Some(flow) => Ok(Json(flow_response(ctx, &flow, true))),
        None => Err(not_found(format!("flow view for {node}"))),
    }
}

#[derive(Deserialize)]
struct PathParams {
    #[serde(default)]
    from: Option<String>,
    to: Option<String>,
    #[serde(default)]
    tests: Option<String>,
}

async fn path(State(state): State<Shared>, Query(params): Query<PathParams>) -> ApiResult {
    let ctx = &state.ctx;
    let Some(to_text) = params.to.filter(|to| !to.trim().is_empty()) else {
        return Err(ApiError(StatusCode::BAD_REQUEST, "the parameter `to` is necessary".to_string()));
    };
    let to = graph::resolve(ctx, &to_text).ok_or_else(|| not_found(format!("symbol {to_text}")))?;
    let tests = params.tests.as_deref().is_some_and(|value| matches!(value, "1" | "true" | "yes"));
    let sources = match params.from.filter(|from| !from.trim().is_empty()) {
        Some(from_text) => vec![graph::resolve(ctx, &from_text).ok_or_else(|| not_found(format!("symbol {from_text}")))?],
        None => graph::entry_points(ctx, tests),
    };
    let mut paths = Vec::new();
    let mut truncated = false;
    for from in &sources {
        let (found, cut) = graph::call_paths(ctx, from, &to);
        truncated |= cut;
        paths.extend(found);
        if paths.len() >= graph::MAX_PATHS {
            paths.truncate(graph::MAX_PATHS);
            truncated = true;
            break;
        }
    }
    Ok(Json(json!({ "from": sources, "to": to, "paths": paths, "truncated": truncated })))
}

#[derive(Deserialize)]
struct SourceParams {
    #[serde(default)]
    v: Option<String>,
}

async fn source(State(state): State<Shared>, Path(id): Path<String>, Query(params): Query<SourceParams>) -> ApiResult {
    let ctx = &state.ctx;
    let id = id.trim_start_matches('/');
    let version = params.v.unwrap_or_else(|| "after".to_string());
    if version != "before" && version != "after" {
        return Err(ApiError(StatusCode::BAD_REQUEST, format!("v must be `before` or `after`, not `{version}`")));
    }
    let pick = |before: Option<&String>, after: Option<&String>| if version == "before" { before.cloned() } else { after.cloned() };
    // 1. The exact anchor in the sources.
    if let Some(pair) = ctx.data.sources.get(id) {
        let text = pick(pair.before.as_ref(), pair.after.as_ref());
        return Ok(Json(json!({ "id": id, "version": version, "key": id, "from": "sources", "text": text })));
    }
    // 2. The text of a claim at this anchor.
    let claim = ctx
        .all_claims()
        .filter(|(_, claim)| claim.at.as_ref().is_some_and(|at| at.to_string() == id))
        .filter_map(|(_, claim)| ctx.results.get(&claim.id))
        .find(|result| result.before.is_some() || result.after.is_some());
    if let Some(result) = claim {
        let text = pick(result.before.as_ref(), result.after.as_ref());
        return Ok(Json(json!({ "id": id, "version": version, "key": id, "from": "claim", "claim": result.claim, "text": text })));
    }
    // 3. The item of the anchor.
    if let Some((key, pair)) = ctx.source(id) {
        let text = pick(pair.before.as_ref(), pair.after.as_ref());
        return Ok(Json(json!({ "id": id, "version": version, "key": key, "from": "item", "text": text })));
    }
    Err(not_found(format!("source of {id}")))
}

async fn tree(State(state): State<Shared>) -> ApiResult {
    Ok(Json(serde_json::to_value(&state.ctx.data.tree).unwrap_or(Value::Null)))
}

async fn workspace_graph(State(state): State<Shared>) -> ApiResult {
    Ok(Json(serde_json::to_value(&state.ctx.data.graph).unwrap_or(Value::Null)))
}

async fn review_state(State(state): State<Shared>) -> ApiResult {
    let review = review_of(&state);
    let ctx = &state.ctx;
    let claims: BTreeMap<&str, review::ReviewState> = ctx
        .all_claims()
        .map(|(_, claim)| (claim.id.as_str(), review.state(&claim.id, ctx.claim_hashes.get(&claim.id).map(String::as_str))))
        .collect();
    Ok(Json(json!({ "file": state.review_file.display().to_string(), "claims": claims })))
}

/// The claims of a review target: a claim, a group, or a subgroup, in this order.
fn target_claims(ctx: &Ctx, target: &str) -> Option<(&'static str, Vec<String>)> {
    if ctx.claim(target).is_some() {
        return Some(("claim", vec![target.to_string()]));
    }
    if ctx.group(target).is_some() {
        let mut claims = ctx.group_claims(target);
        for subgroup in ctx.subgroups.iter().filter(|subgroup| subgroup.subgroup.group == target) {
            for claim in &subgroup.subgroup.claims {
                if !claims.contains(claim) && ctx.claim(claim).is_some() {
                    claims.push(claim.clone());
                }
            }
        }
        return Some(("group", claims));
    }
    ctx.subgroup(target)
        .map(|subgroup| ("subgroup", subgroup.subgroup.claims.iter().filter(|claim| ctx.claim(claim).is_some()).cloned().collect()))
}

async fn review_mark(State(state): State<Shared>, Path(target): Path<String>) -> ApiResult {
    review_change(&state, &target, true)
}

async fn review_unmark(State(state): State<Shared>, Path(target): Path<String>) -> ApiResult {
    review_change(&state, &target, false)
}

fn review_change(state: &AppState, target: &str, mark: bool) -> ApiResult {
    let ctx = &state.ctx;
    let target = target.trim_start_matches('/');
    let (kind, claims) = target_claims(ctx, target).ok_or_else(|| not_found(format!("claim, group, or subgroup {target}")))?;
    let mut review = review_of(state);
    let now = review::now();
    for claim in &claims {
        if mark {
            if let Some(hash) = ctx.claim_hashes.get(claim) {
                review.mark(claim, hash, now);
            }
        } else {
            review.unmark(claim);
        }
    }
    review
        .save(&state.review_file)
        .map_err(|error| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("cannot write {}: {error}", state.review_file.display())))?;
    Ok(Json(json!({ "target": target, "kind": kind, "claims": claims, "reviewed": mark })))
}

#[derive(Deserialize)]
struct PairParams {
    old: String,
    new: String,
    /// `accepted`, `dismissed`, or `clear`.
    decision: String,
}

/// Stores the reviewer's decision on a pair warning. `accepted` means that the agent must rewrite
/// the pair as one changed item.
async fn pair_decision(State(state): State<Shared>, Query(params): Query<PairParams>) -> ApiResult {
    let key = format!("{} -> {}", params.old, params.new);
    if !state.ctx.data.report.pairs.iter().any(|pair| pair.key() == key) {
        return Err(not_found(format!("pair warning {key}")));
    }
    let decision = match params.decision.as_str() {
        "accepted" | "dismissed" => Some(params.decision.as_str()),
        "clear" => None,
        other => return Err(ApiError(StatusCode::BAD_REQUEST, format!("unknown decision `{other}`; use accepted, dismissed, or clear"))),
    };
    let mut review = review_of(&state);
    review.set_pair(&key, decision, review::now());
    review
        .save(&state.review_file)
        .map_err(|error| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("cannot write {}: {error}", state.review_file.display())))?;
    Ok(Json(json!({ "pair": key, "decision": decision })))
}

async fn static_file(uri: Uri) -> Response {
    let path = uri.path();
    if path.starts_with("/api/") {
        return not_found(format!("API route {path}")).into_response();
    }
    match assets::find(path) {
        Some(asset) => ([(header::CONTENT_TYPE, asset.content_type), (header::CACHE_CONTROL, "no-cache")], asset.bytes).into_response(),
        None => (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "not found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_json_response_has_no_raw_hidden_character() {
        // Batch 5k, A2: serde gives a control character as an escape. The response gives each
        // other hidden character as an escape too. The value of the string does not change.
        let value = serde_json::json!({ "intent": "a\nb\u{1b}[8m \u{202e}c\u{200b}\u{2028}\u{feff} é" });
        let body = super::json_hidden(&serde_json::to_string(&value).unwrap());
        assert_eq!(body, r#"{"intent":"a\nb\u001b[8m \u202ec\u200b\u2028\ufeff é"}"#);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap(), value);
    }
}
