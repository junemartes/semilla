//! The workspace graph: crates, modules, items, and the calls between functions.
//!
//! The calls come from the syntax. A call resolves to a workspace function by its path, or to a
//! method by its name when few methods have that name. Other calls go to `external` leaves
//! (design option B). The badges of an external leaf come from its call sites: a `?` after the
//! call gives `err`.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::view::{GraphEdge, GraphEdgeKind, GraphNode, GraphNodeKind, Status, WorkspaceGraph};

use crate::load::{ItemKind, Workspace};
use crate::tree::Node;

/// Builds the workspace graph. `statuses` maps item paths to their status in the change set, and
/// `changes` maps item paths to their number of changes.
pub fn workspace_graph(workspace: &Workspace, statuses: &BTreeMap<String, Status>, changes: &BTreeMap<String, usize>) -> WorkspaceGraph {
    let mut graph = WorkspaceGraph::default();
    let mut modules = BTreeSet::new();
    for krate in &workspace.crates {
        graph.nodes.push(GraphNode {
            id: krate.name.clone(),
            label: krate.name.clone(),
            kind: GraphNodeKind::Crate,
            parent: None,
            status: Status::Same,
            changes: 0,
            badges: Vec::new(),
            entry: false,
            test: false,
            file: None,
        });
        modules.insert(krate.name.clone());
        for dep in &krate.deps {
            graph.edges.push(GraphEdge { from: krate.name.clone(), to: dep.clone(), kind: GraphEdgeKind::Depends, status: Status::Same });
        }
    }

    for item in workspace.items.values().filter(|item| item.kind == ItemKind::Mod) {
        let parent = item.path.rsplit_once("::").map(|(parent, _)| parent.to_string());
        modules.insert(item.path.clone());
        graph.nodes.push(GraphNode {
            id: item.path.clone(),
            label: item.path.rsplit("::").next().unwrap_or(&item.path).to_string(),
            kind: GraphNodeKind::Module,
            parent,
            status: Status::Same,
            changes: 0,
            badges: Vec::new(),
            entry: false,
            test: item.test,
            file: Some(item.file.clone()),
        });
    }

    let functions: Vec<&crate::Item> = workspace.items.values().filter(|item| item.kind == ItemKind::Fn).collect();
    for item in workspace.items.values() {
        let kind = match item.kind {
            ItemKind::Fn => GraphNodeKind::Function,
            ItemKind::Struct | ItemKind::Enum | ItemKind::Type => GraphNodeKind::Type,
            ItemKind::Trait => GraphNodeKind::Trait,
            _ => continue,
        };
        let parent = owner_module(&item.path, &modules);
        let name = item.path.strip_prefix(&format!("{parent}::")).unwrap_or(&item.path).to_string();
        let lib = workspace.crates.iter().any(|k| k.name == item.crate_name);
        let entry = item.kind == ItemKind::Fn && (name == "main" || item.test || (item.vis == "pub" && lib));
        graph.nodes.push(GraphNode {
            id: item.path.clone(),
            label: name,
            kind,
            parent: Some(parent),
            status: statuses.get(&item.path).copied().unwrap_or(Status::Same),
            changes: changes.get(&item.path).copied().unwrap_or(0),
            badges: Vec::new(),
            entry,
            test: item.test,
            file: Some(item.file.clone()),
        });
    }

    let mut externals: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut edges = BTreeSet::new();
    for item in &functions {
        let Some(body) = &item.body else { continue };
        let mut calls = Vec::new();
        collect_calls(body, false, &mut calls);
        for call in calls {
            let target = resolve_call(&call, item, &functions);
            match target {
                Some(target) => {
                    edges.insert((item.path.clone(), target));
                }
                None => {
                    let id = format!("ext::{}", call.name);
                    let badges = externals.entry(id.clone()).or_default();
                    if call.tried {
                        badges.insert("err".to_string());
                    }
                    if call.mut_arg {
                        badges.insert("mut".to_string());
                    }
                    if call.awaited {
                        badges.insert("async".to_string());
                    }
                    edges.insert((item.path.clone(), id));
                }
            }
        }
    }
    for (id, badges) in externals {
        graph.nodes.push(GraphNode {
            id: id.clone(),
            label: id.trim_start_matches("ext::").to_string(),
            kind: GraphNodeKind::External,
            parent: None,
            status: Status::Same,
            changes: 0,
            badges: badges.into_iter().collect(),
            entry: false,
            test: false,
            file: None,
        });
    }
    for (from, to) in edges {
        graph.edges.push(GraphEdge { from, to, kind: GraphEdgeKind::Call, status: Status::Same });
    }

    // Each module and crate counts the changes inside it.
    let counts: Vec<(String, usize)> = graph.nodes.iter().filter(|n| n.changes > 0).map(|n| (n.id.clone(), n.changes)).collect();
    for node in graph.nodes.iter_mut().filter(|n| matches!(n.kind, GraphNodeKind::Module | GraphNodeKind::Crate)) {
        let prefix = format!("{}::", node.id);
        node.changes = counts.iter().filter(|(id, _)| id.starts_with(&prefix)).map(|(_, count)| count).sum();
        if node.changes > 0 {
            node.status = Status::Changed;
        }
    }
    graph
}

fn owner_module(path: &str, modules: &BTreeSet<String>) -> String {
    let mut current = path;
    while let Some((parent, _)) = current.rsplit_once("::") {
        if modules.contains(parent) {
            return parent.to_string();
        }
        current = parent;
    }
    path.split("::").next().unwrap_or(path).to_string()
}

/// A call site in a body.
struct Call {
    /// The callee path text (`helper`, `crate::a::f`, `Vec::new`) or the method name.
    name: String,
    method: bool,
    tried: bool,
    mut_arg: bool,
    awaited: bool,
}

fn collect_calls(node: &Node, tried: bool, out: &mut Vec<Call>) {
    let child_tried = node.kind == "try";
    match node.kind {
        "call" => {
            if let Some(callee) = node.child("callee").filter(|callee| callee.kind == "path") {
                let mut_arg = node.children.iter().any(|child| child.kind == "ref" && child.attr.as_deref() == Some("&mut"));
                out.push(Call { name: callee.text.replace(' ', ""), method: false, tried, mut_arg, awaited: false });
            }
        }
        "method_call" => {
            if let Some(name) = &node.attr {
                out.push(Call { name: name.clone(), method: true, tried, mut_arg: false, awaited: false });
            }
        }
        "await" => {
            let before = out.len();
            for child in &node.children {
                collect_calls(child, tried, out);
            }
            if let Some(call) = out.get_mut(before) {
                call.awaited = true;
            }
            return;
        }
        _ => {}
    }
    for child in &node.children {
        collect_calls(child, child_tried, out);
    }
}

fn resolve_call(call: &Call, caller: &crate::Item, functions: &[&crate::Item]) -> Option<String> {
    if call.method {
        let suffix = format!("::{}", call.name);
        let candidates: Vec<&&crate::Item> = functions.iter().filter(|f| f.path.ends_with(&suffix) && f.path.matches("::").count() >= 2).collect();
        return match candidates.as_slice() {
            [single] => Some(single.path.clone()),
            many if !many.is_empty() && many.len() <= 3 => many.iter().find(|f| f.crate_name == caller.crate_name).map(|f| f.path.clone()),
            _ => None,
        };
    }
    let name = call.name.trim_start_matches("crate::").trim_start_matches("self::").trim_start_matches("super::");
    let suffix = format!("::{name}");
    let mut candidates: Vec<&&crate::Item> = functions.iter().filter(|f| f.path.ends_with(&suffix) || f.path == name).collect();
    candidates.sort_by_key(|f| (f.crate_name != caller.crate_name, f.path.len()));
    candidates.first().map(|f| f.path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::workspace;

    #[test]
    fn calls_and_externals() {
        let (_dir, ws) = workspace(&[
            ("Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n"),
            ("src/lib.rs", "pub mod a;\npub fn top(s: &str) -> Result<u32, std::num::ParseIntError> { let n = s.parse::<u32>()?; Ok(a::helper(n)) }\n"),
            ("src/a.rs", "pub fn helper(x: u32) -> u32 { x + 1 }\n#[test]\nfn t() { let _ = helper(1); }\n"),
        ]);
        let statuses = BTreeMap::from([("demo::a::helper".to_string(), Status::Changed)]);
        let changes = BTreeMap::from([("demo::a::helper".to_string(), 2)]);
        let graph = workspace_graph(&ws, &statuses, &changes);
        let has_edge = |from: &str, to: &str| graph.edges.iter().any(|e| e.from == from && e.to == to);
        assert!(has_edge("demo::top", "demo::a::helper"));
        assert!(has_edge("demo::a::t", "demo::a::helper"));
        let parse = graph.nodes.iter().find(|n| n.id == "ext::parse").unwrap();
        assert_eq!(parse.badges, ["err"]);
        let module = graph.nodes.iter().find(|n| n.id == "demo::a").unwrap();
        assert_eq!(module.changes, 2);
        assert!(graph.nodes.iter().find(|n| n.id == "demo::a::t").unwrap().test);
        assert!(graph.nodes.iter().find(|n| n.id == "demo::top").unwrap().entry);
    }
}
