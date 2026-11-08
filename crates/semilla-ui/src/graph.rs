//! Queries on the workspace graph: call paths, impact, neighbors, and the flow view of a
//! function without a change.

use std::collections::{BTreeSet, VecDeque};

use semilla_core::view::{FlowEdge, FlowGraph, FlowKind, FlowNode, GraphNodeKind, Status};

use crate::ctx::{Ctx, last_segment};

/// The maximum number of paths that a path query returns.
pub const MAX_PATHS: usize = 50;
/// The maximum number of calls in one path.
pub const MAX_PATH_LEN: usize = 16;

/// The call paths from `from` to `to`, as lists of node IDs. The second value is true if the
/// search stopped at a limit.
pub fn call_paths(ctx: &Ctx, from: &str, to: &str) -> (Vec<Vec<String>>, bool) {
    let mut paths = Vec::new();
    let mut truncated = false;
    let mut stack = vec![from.to_string()];
    walk(ctx, to, &mut stack, &mut paths, &mut truncated);
    (paths, truncated)
}

fn walk(ctx: &Ctx, to: &str, stack: &mut Vec<String>, paths: &mut Vec<Vec<String>>, truncated: &mut bool) {
    let current = stack.last().cloned().unwrap_or_default();
    if current == to {
        paths.push(stack.clone());
        return;
    }
    if stack.len() > MAX_PATH_LEN {
        *truncated = true;
        return;
    }
    for next in ctx.callees.get(&current).into_iter().flatten() {
        if paths.len() >= MAX_PATHS {
            *truncated = true;
            return;
        }
        if stack.contains(next) {
            continue;
        }
        stack.push(next.clone());
        walk(ctx, to, stack, paths, truncated);
        stack.pop();
    }
}

/// The entry points of the graph: `main`, `pub` items, and (only with `tests`) test functions.
pub fn entry_points(ctx: &Ctx, tests: bool) -> Vec<String> {
    ctx.data
        .graph
        .nodes
        .iter()
        .filter(|node| node.entry && matches!(node.kind, GraphNodeKind::Function) && (tests || !node.test))
        .map(|node| node.id.clone())
        .collect()
}

/// All the functions that can reach `id` through call edges (the impact overlay).
pub fn transitive_callers(ctx: &Ctx, id: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([id.to_string()]);
    while let Some(current) = queue.pop_front() {
        for caller in ctx.callers.get(&current).into_iter().flatten() {
            if caller != id && seen.insert(caller.clone()) {
                queue.push_back(caller.clone());
            }
        }
    }
    seen
}

/// Finds a graph node for a symbol that the user typed: the exact ID, then an ID that ends with
/// `::symbol`, then a node with the same label.
pub fn resolve(ctx: &Ctx, symbol: &str) -> Option<String> {
    let symbol = symbol.trim();
    if ctx.graph_node(symbol).is_some() {
        return Some(symbol.to_string());
    }
    let suffix = format!("::{symbol}");
    let mut found: Vec<&str> = ctx.data.graph.nodes.iter().map(|node| node.id.as_str()).filter(|id| id.ends_with(&suffix)).collect();
    if found.is_empty() {
        found = ctx.data.graph.nodes.iter().filter(|node| node.label == symbol).map(|node| node.id.as_str()).collect();
    }
    found.sort_by_key(|id| id.len());
    found.first().map(|id| id.to_string())
}

/// A flow view for a function without a change: the callers as entry stubs, one summary node
/// for the body, and the callees as exit stubs. The reviewer can walk the unchanged flow with it.
pub fn synthetic_flow(ctx: &Ctx, id: &str) -> Option<FlowGraph> {
    let node = ctx.graph_node(id)?;
    if !matches!(node.kind, GraphNodeKind::Function | GraphNodeKind::External) {
        return None;
    }
    let breadcrumb = ctx
        .graph_ancestors(id)
        .iter()
        .map(|ancestor| ctx.graph_node(ancestor).map(|node| node.label.clone()).unwrap_or_default())
        .collect();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for (index, caller) in ctx.callers.get(id).into_iter().flatten().enumerate() {
        let stub = format!("in{index}");
        nodes.push(FlowNode {
            id: stub.clone(),
            label: format!("from: {}", last_label(ctx, caller)),
            kind: FlowKind::Stub,
            status: ctx.graph_node(caller).map(|node| node.status).unwrap_or(Status::Same),
            anchor: Some(caller.clone()),
        });
        edges.push(FlowEdge { from: stub, to: "entry".to_string(), label: None, status: Status::Same });
    }
    nodes.push(FlowNode {
        id: "entry".to_string(),
        label: format!("entry: {}", node.label),
        kind: FlowKind::Entry,
        status: Status::Same,
        anchor: None,
    });
    let body = if node.kind == GraphNodeKind::External {
        format!("external function {}", node.badges.join(" "))
    } else if node.status == Status::Same {
        "… function body unchanged".to_string()
    } else {
        "… no flow view in the data".to_string()
    };
    nodes.push(FlowNode {
        id: "body".to_string(),
        label: body.trim().to_string(),
        kind: FlowKind::Summary,
        status: node.status,
        anchor: Some(id.to_string()),
    });
    nodes.push(FlowNode { id: "exit".to_string(), label: "return".to_string(), kind: FlowKind::Exit, status: Status::Same, anchor: None });
    edges.push(FlowEdge { from: "entry".to_string(), to: "body".to_string(), label: None, status: Status::Same });
    edges.push(FlowEdge { from: "body".to_string(), to: "exit".to_string(), label: None, status: Status::Same });
    for (index, callee) in ctx.callees.get(id).into_iter().flatten().enumerate() {
        let stub = format!("out{index}");
        let status =
            ctx.data.graph.edges.iter().find(|edge| edge.from == id && &edge.to == callee).map(|edge| edge.status).unwrap_or(Status::Same);
        nodes.push(FlowNode {
            id: stub.clone(),
            label: format!("to: {}", last_label(ctx, callee)),
            kind: FlowKind::Stub,
            status: ctx.graph_node(callee).map(|node| node.status).unwrap_or(Status::Same),
            anchor: Some(callee.clone()),
        });
        edges.push(FlowEdge { from: "body".to_string(), to: stub, label: Some("call".to_string()), status });
    }
    Some(FlowGraph { function: id.to_string(), breadcrumb, nodes, edges })
}

fn last_label(ctx: &Ctx, id: &str) -> String {
    ctx.graph_node(id).map(|node| node.label.clone()).unwrap_or_else(|| last_segment(id).to_string())
}
