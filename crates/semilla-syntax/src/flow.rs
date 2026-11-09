//! Flow views: the smallest part of a function's control flow that contains its changes.
//!
//! The graph comes from the source-level structure (design section 10.1): statements, `if`,
//! `match`, loops, and exits. It stops at statements; a changed expression shows in the label of
//! its statement.

use std::collections::BTreeMap;

use semilla_core::view::{FlowEdge, FlowGraph, FlowKind, FlowNode, Status};

use crate::load::Item;
use crate::tree::{Node, short};

/// The maximum number of visible nodes before unchanged compound statements fold.
pub const NODE_BUDGET: usize = 12;

/// Builds the flow view of a function.
///
/// `statuses` maps role paths inside the body (for example `S1/then/S0`) to a status. A path that
/// is not in the map, and has no changed descendant, is unchanged. `removed` counts the deleted
/// statements, which show as one summary node.
pub fn flow_view(item: &Item, statuses: &BTreeMap<String, Status>, removed: usize) -> Option<FlowGraph> {
    let body = item.body.as_ref()?;
    // The frame is the lowest block that contains all changes.
    let changed: Vec<&String> = statuses.keys().collect();
    let frame_path = common_block_prefix(body, &changed);
    let frame = descend(body, &frame_path)?;

    let mut builder = Builder { nodes: Vec::new(), edges: Vec::new(), statuses, fold_compound: false };
    let entry_label = if frame_path.is_empty() { "entry".to_string() } else { format!("from: {}", short(&parent_label(body, &frame_path), 40)) };
    let entry = builder.add(entry_label, if frame_path.is_empty() { FlowKind::Entry } else { FlowKind::Stub }, Status::Same, None);
    let exit = builder.add("exit".to_string(), FlowKind::Exit, Status::Same, None);
    let mut loops = Vec::new();
    let ends = builder.block(frame, &frame_path.join("/"), vec![(entry.clone(), None)], &exit, &mut loops);
    for (from, label) in ends {
        builder.edge(&from, &exit, label, Status::Same);
    }
    if removed > 0 {
        let node = builder.add(format!("− {removed} statement(s) removed"), FlowKind::Summary, Status::Removed, None);
        builder.edge(&entry, &node, None, Status::Removed);
    }
    if builder.nodes.len() > NODE_BUDGET {
        // Too large: build again and fold the unchanged compound statements.
        let mut folded = Builder { nodes: Vec::new(), edges: Vec::new(), statuses, fold_compound: true };
        let entry = folded.add(builder.nodes[0].label.clone(), builder.nodes[0].kind, Status::Same, None);
        let exit = folded.add("exit".to_string(), FlowKind::Exit, Status::Same, None);
        let ends = folded.block(frame, &frame_path.join("/"), vec![(entry.clone(), None)], &exit, &mut Vec::new());
        for (from, label) in ends {
            folded.edge(&from, &exit, label, Status::Same);
        }
        if removed > 0 {
            let node = folded.add(format!("− {removed} statement(s) removed"), FlowKind::Summary, Status::Removed, None);
            folded.edge(&entry, &node, None, Status::Removed);
        }
        builder = folded;
    }

    let mut breadcrumb: Vec<String> = crate::split_path(&item.path).into_iter().map(str::to_string).collect();
    let mut node = body;
    for role in &frame_path {
        if let Some(child) = node.child(role) {
            if child.stmt {
                breadcrumb.push(short(&header_label(child), 30));
            }
            node = child;
        }
    }
    let function = if frame_path.is_empty() { item.path.clone() } else { format!("{}/{}", item.path, frame_path.join("/")) };
    Some(FlowGraph { function, breadcrumb, nodes: builder.nodes, edges: builder.edges })
}

/// The longest role path of a block that is a prefix of all changed paths.
fn common_block_prefix(body: &Node, changed: &[&String]) -> Vec<String> {
    if changed.is_empty() {
        return Vec::new();
    }
    let split: Vec<Vec<&str>> = changed.iter().map(|path| path.split('/').collect()).collect();
    let mut prefix: Vec<String> = Vec::new();
    let mut best: Vec<String> = Vec::new();
    let mut node = body;
    for index in 0.. {
        let Some(role) = split[0].get(index) else { break };
        if !split.iter().all(|parts| parts.get(index) == Some(role)) {
            break;
        }
        let Some(child) = node.child(role) else { break };
        prefix.push(role.to_string());
        node = child;
        // A frame must be a block, and a change on the block's own statement is outside it.
        if node.kind == "block" && split.iter().all(|parts| parts.len() > index + 1) {
            best = prefix.clone();
        }
    }
    best
}

fn descend<'a>(body: &'a Node, path: &[String]) -> Option<&'a Node> {
    let mut node = body;
    for role in path {
        node = node.child(role)?;
    }
    Some(node)
}

fn parent_label(body: &Node, path: &[String]) -> String {
    let mut node = body;
    let mut label = "entry".to_string();
    for role in path {
        if let Some(child) = node.child(role) {
            if child.stmt {
                label = header_label(child);
            }
            node = child;
        }
    }
    label
}

/// The header of a compound statement: `if cond`, `for x in v`, `match m`, `while c`, `loop`.
fn header_label(node: &Node) -> String {
    let child_text = |role: &str| node.child(role).map(|child| child.text.clone()).unwrap_or_default();
    match node.kind {
        "if" => format!("if {}", child_text("cond")),
        "for" => format!("for {} in {}", child_text("pat"), child_text("iter")),
        "while" => format!("while {}", child_text("cond")),
        "loop" => "loop".to_string(),
        "match" => format!("match {}", child_text("scrutinee")),
        _ => node.text.clone(),
    }
}

impl Builder<'_> {
    /// Adds a child node for each closure that the statement passes to a call (design 10.2).
    /// The callee can call a closure any number of times, so the edge says "called 0..n times".
    fn closures(&mut self, node: &Node, path: &str, statement: &str) {
        let mut found: Vec<(String, &Node)> = Vec::new();
        collect_closure_args(node, path, &mut found);
        for (closure_path, closure) in found {
            let status = self.status(&closure_path);
            let label = format!("|{}| {}", closure.attr.as_deref().unwrap_or(""), short(closure.child("body").map(|b| b.text.as_str()).unwrap_or(""), 40));
            let id = self.add(label, FlowKind::Statement, status, Some(closure_path));
            self.edge(statement, &id, Some("called 0..n times".into()), Status::Same);
            self.edge(&id, statement, Some("returns".into()), Status::Same);
        }
    }
}

/// The closures that are direct arguments of a call or a method call, with their paths.
fn collect_closure_args<'a>(node: &'a Node, path: &str, out: &mut Vec<(String, &'a Node)>) {
    for child in &node.children {
        let child_path = format!("{path}/{}", child.role);
        let is_arg = matches!(node.kind, "call" | "method_call") && child.role.starts_with("args[");
        if is_arg && matches!(child.kind, "closure" | "closure_block") {
            out.push((child_path, child));
        } else if !matches!(child.kind, "closure" | "closure_block") {
            collect_closure_args(child, &child_path, out);
        }
    }
}

/// True if a `?` in the statement can return from the function. A `?` inside a closure or an
/// `async` block returns from that closure or block, not from the function.
fn returns_error(node: &Node) -> bool {
    node.kind == "try" || node.children.iter().any(|child| !matches!(child.kind, "closure" | "closure_block" | "async") && returns_error(child))
}

/// A loose end: the node and the label of the edge that leaves it.
type End = (String, Option<String>);

struct Builder<'a> {
    nodes: Vec<FlowNode>,
    edges: Vec<FlowEdge>,
    statuses: &'a BTreeMap<String, Status>,
    fold_compound: bool,
}

impl Builder<'_> {
    fn add(&mut self, label: String, kind: FlowKind, status: Status, anchor: Option<String>) -> String {
        let id = format!("n{}", self.nodes.len());
        self.nodes.push(FlowNode { id: id.clone(), label, kind, status, anchor });
        id
    }

    fn edge(&mut self, from: &str, to: &str, label: Option<String>, status: Status) {
        self.edges.push(FlowEdge { from: from.to_string(), to: to.to_string(), label, status });
    }

    fn connect(&mut self, ends: Vec<End>, to: &str) {
        for (from, label) in ends {
            self.edge(&from, to, label, Status::Same);
        }
    }

    /// The status of a path: its own status, else `changed` if a descendant changed.
    fn status(&self, path: &str) -> Status {
        if let Some(status) = self.statuses.get(path) {
            return *status;
        }
        let prefix = format!("{path}/");
        if self.statuses.keys().any(|key| key.starts_with(&prefix)) { Status::Changed } else { Status::Same }
    }

    fn has_change(&self, path: &str) -> bool {
        self.status(path) != Status::Same
    }

    /// Adds the statements of a block. Returns the loose ends after the block.
    fn block(&mut self, block: &Node, path: &str, mut ends: Vec<End>, exit: &str, loops: &mut Vec<(String, Vec<End>)>) -> Vec<End> {
        let join = |path: &str, role: &str| if path.is_empty() { role.to_string() } else { format!("{path}/{role}") };
        let mut unchanged_run: Vec<&Node> = Vec::new();
        for statement in &block.children {
            let statement_path = join(path, &statement.role);
            let compound = matches!(statement.kind, "if" | "match" | "for" | "while" | "loop" | "block_expr" | "unsafe");
            let exits = matches!(statement.kind, "return" | "break" | "continue") || returns_error(statement);
            let changed = self.has_change(&statement_path);
            if !changed && !exits && (!compound || self.fold_compound) {
                unchanged_run.push(statement);
                continue;
            }
            ends = self.flush_run(&mut unchanged_run, ends);
            ends = self.statement(statement, &statement_path, ends, exit, loops);
        }
        self.flush_run(&mut unchanged_run, ends)
    }

    fn flush_run(&mut self, run: &mut Vec<&Node>, ends: Vec<End>) -> Vec<End> {
        if run.is_empty() {
            return ends;
        }
        let label = if run.len() == 1 { short(&header_label(run[0]), 50) } else { format!("… {} statements unchanged", run.len()) };
        let kind = if run.len() == 1 { FlowKind::Statement } else { FlowKind::Summary };
        let id = self.add(label, kind, Status::Same, None);
        self.connect(ends, &id);
        run.clear();
        vec![(id, None)]
    }

    fn statement(&mut self, node: &Node, path: &str, ends: Vec<End>, exit: &str, loops: &mut Vec<(String, Vec<End>)>) -> Vec<End> {
        let status = self.status(path);
        let anchor = Some(path.to_string());
        let child_path = |role: &str| format!("{path}/{role}");
        match node.kind {
            "if" => {
                let id = self.add(short(&header_label(node), 50), FlowKind::Branch, status, anchor);
                self.connect(ends, &id);
                let mut out = Vec::new();
                if let Some(then) = node.child("then") {
                    out.extend(self.block(then, &child_path("then"), vec![(id.clone(), Some("yes".into()))], exit, loops));
                }
                match node.child("else") {
                    Some(other) if other.kind == "block" => out.extend(self.block(other, &child_path("else"), vec![(id.clone(), Some("no".into()))], exit, loops)),
                    Some(other) => out.extend(self.statement(other, &child_path("else"), vec![(id.clone(), Some("no".into()))], exit, loops)),
                    None => out.push((id, Some("no".into()))),
                }
                out
            }
            "match" => {
                let id = self.add(short(&header_label(node), 50), FlowKind::Branch, status, anchor);
                self.connect(ends, &id);
                let mut out = Vec::new();
                let mut index = 0;
                while let Some(pattern) = node.child(&format!("arm[{index}].pat")) {
                    let label = Some(short(&pattern.text, 20));
                    let role = format!("arm[{index}].body");
                    if let Some(body) = node.child(&role) {
                        if body.kind == "block" {
                            out.extend(self.block(body, &child_path(&role), vec![(id.clone(), label)], exit, loops));
                        } else {
                            let arm_status = self.status(&child_path(&role));
                            let arm = self.add(short(&body.text, 40), FlowKind::Statement, arm_status, Some(child_path(&role)));
                            self.edge(&id, &arm, label, Status::Same);
                            if matches!(body.kind, "return") {
                                self.edge(&arm, exit, Some("return".into()), Status::Same);
                            } else {
                                out.push((arm, None));
                            }
                        }
                    }
                    index += 1;
                }
                out
            }
            "for" | "while" | "loop" => {
                let id = self.add(short(&header_label(node), 50), FlowKind::Loop, status, anchor);
                self.connect(ends, &id);
                loops.push((id.clone(), Vec::new()));
                if let Some(body) = node.child("body") {
                    let body_ends = self.block(body, &child_path("body"), vec![(id.clone(), Some(if node.kind == "loop" { "next".into() } else { "each".into() }))], exit, loops);
                    for (from, label) in body_ends {
                        self.edge(&from, &id, label.or(Some("again".into())), Status::Same);
                    }
                }
                let (_, breaks) = loops.pop().unwrap_or_default();
                let mut out = breaks;
                if node.kind != "loop" {
                    out.push((id, Some("done".into())));
                }
                out
            }
            "return" => {
                let id = self.add(short(&node.text, 50), FlowKind::Statement, status, anchor);
                self.connect(ends, &id);
                self.edge(&id, exit, Some("return".into()), Status::Same);
                Vec::new()
            }
            "break" => {
                let id = self.add(short(&node.text, 50), FlowKind::Statement, status, anchor);
                self.connect(ends, &id);
                if let Some((_, breaks)) = loops.last_mut() {
                    breaks.push((id, Some("break".into())));
                }
                Vec::new()
            }
            "continue" => {
                let id = self.add(short(&node.text, 50), FlowKind::Statement, status, anchor);
                self.connect(ends, &id);
                if let Some((loop_id, _)) = loops.last() {
                    let loop_id = loop_id.clone();
                    self.edge(&id, &loop_id, Some("continue".into()), Status::Same);
                }
                Vec::new()
            }
            _ => {
                let label = short(&node.text, 50);
                let id = self.add(label, FlowKind::Statement, status, anchor);
                self.connect(ends, &id);
                self.closures(node, path, &id);
                if returns_error(node) {
                    self.edge(&id, exit, Some("err".into()), Status::Same);
                }
                if node.role == "tail" {
                    self.edge(&id, exit, Some("return".into()), Status::Same);
                    return Vec::new();
                }
                vec![(id, None)]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::workspace;

    const MANIFEST: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";

    #[test]
    fn frame_is_the_lowest_changed_block() {
        let src = "pub fn total(v: &[u32], max: u32) -> u32 {\n    let mut s = 0;\n    let a = 1;\n    let b = 2;\n    for x in v {\n        if *x <= max {\n            s += x;\n        }\n        s += a + b;\n    }\n    s\n}\n";
        let (_dir, ws) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", src)]);
        let item = &ws.items["demo::total"];
        let statuses = BTreeMap::from([("S3/body/S0/cond".to_string(), Status::Changed)]);
        let graph = flow_view(item, &statuses, 0).unwrap();
        assert_eq!(graph.function, "demo::total/S3/body");
        assert!(graph.breadcrumb.iter().any(|part| part.starts_with("for x in v")));
        let changed: Vec<_> = graph.nodes.iter().filter(|n| n.status == Status::Changed).map(|n| n.label.as_str()).collect();
        assert_eq!(changed, ["if * x <= max"]);
        assert!(graph.nodes.iter().any(|n| n.kind == FlowKind::Stub));
        let mermaid = graph.to_mermaid();
        assert!(mermaid.contains(":::changed"), "{mermaid}");
    }

    #[test]
    fn question_mark_in_a_closure_is_not_an_exit() {
        let src = "pub fn f(v: &[&str]) -> Result<u32, std::num::ParseIntError> {\n    let a: Vec<Option<u32>> = v.iter().map(|s| Some(s.len() as u32)).collect();\n    let b = v.iter().map(|s| -> Option<u32> { s.parse().ok()? .checked_add(1) }).count();\n    let c: u32 = v[0].parse()?;\n    Ok(c + b as u32 + a.len() as u32)\n}\n";
        let (_dir, ws) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", src)]);
        let item = &ws.items["demo::f"];
        let statuses = BTreeMap::from([("S1".to_string(), Status::Changed), ("S2".to_string(), Status::Changed)]);
        let graph = flow_view(item, &statuses, 0).unwrap();
        let exit = graph.nodes.iter().find(|n| n.kind == FlowKind::Exit).unwrap().id.clone();
        let err_sources: Vec<&str> = graph.edges.iter().filter(|e| e.to == exit && e.label.as_deref() == Some("err")).map(|e| e.from.as_str()).collect();
        let labels: Vec<&str> = err_sources.iter().map(|id| graph.nodes.iter().find(|n| &n.id == id).unwrap().label.as_str()).collect();
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert!(labels[0].starts_with("let c"), "{labels:?}");
    }

    #[test]
    fn closures_passed_to_calls_are_child_nodes() {
        let src = "pub fn f(v: &[u32]) -> u32 {\n    let total: u32 = v.iter().map(|x| x * 2).filter(|x| *x > 3).sum();\n    total\n}\n";
        let (_dir, ws) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", src)]);
        let item = &ws.items["demo::f"];
        let statuses = BTreeMap::from([("S0/init/recv/args[0]/body".to_string(), Status::Changed)]);
        let graph = flow_view(item, &statuses, 0).unwrap();
        let closures: Vec<&FlowNode> = graph.nodes.iter().filter(|n| n.label.starts_with('|')).collect();
        assert_eq!(closures.len(), 2, "{:#?}", graph.nodes);
        assert_eq!(closures.iter().filter(|n| n.status == Status::Changed).count(), 1);
        assert_eq!(graph.edges.iter().filter(|e| e.label.as_deref() == Some("called 0..n times")).count(), 2);
    }

    #[test]
    fn whole_body_with_unchanged_runs_folded() {
        let src = "pub fn f(a: u32) -> Result<u32, ()> {\n    let x = a + 1;\n    let y = x * 2;\n    let z = y - 1;\n    if a == 0 {\n        return Err(());\n    }\n    Ok(z)\n}\n";
        let (_dir, ws) = workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", src)]);
        let item = &ws.items["demo::f"];
        let statuses = BTreeMap::from([("S3".to_string(), Status::Added)]);
        let graph = flow_view(item, &statuses, 0).unwrap();
        assert_eq!(graph.function, "demo::f");
        assert!(graph.nodes.iter().any(|n| n.label == "… 3 statements unchanged"), "{:#?}", graph.nodes);
        let added = graph.nodes.iter().find(|n| n.status == Status::Added).unwrap();
        assert_eq!(added.kind, FlowKind::Branch);
        assert!(graph.edges.iter().any(|e| e.label.as_deref() == Some("return")));
    }
}
