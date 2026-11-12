//! The data that the UI shows.

use serde::{Deserialize, Serialize};

use crate::ops::Class;

/// The status of a node or an edge in a diff view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Same,
    Added,
    Removed,
    Changed,
    Moved,
    /// Code that moved to another node (extract) or from another node (inline).
    Extracted,
    /// A difference without a claim.
    Uncovered,
}

impl Status {
    /// The marker in the change tree (design section 11.4).
    pub fn marker(self) -> &'static str {
        match self {
            Status::Same => "≡",
            Status::Added => "+",
            Status::Removed => "−",
            Status::Changed => "~",
            Status::Moved => "→",
            Status::Extracted => "⤴",
            Status::Uncovered => "✗",
        }
    }
}

/// One node of the change tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TreeNode {
    /// The anchor text, or a file path for non-Rust files.
    pub id: String,
    pub label: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// The claim IDs on this node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<String>,
    /// The diff of a changed leaf, as `⟦old⟧→⟦new⟧` text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// A summary for changed ancestors, for example `1 ctrl flow · 2 logic`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The numbers of the steps that made the change of this node. Empty for a change set of one
    /// step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    /// Prints the tree as text, as in design section 11.4.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out, "", true, true);
        out
    }

    fn render_into(&self, out: &mut String, prefix: &str, last: bool, root: bool) {
        let branch = if root { "" } else if last { "└─ " } else { "├─ " };
        out.push_str(prefix);
        out.push_str(branch);
        out.push_str(self.status.marker());
        out.push(' ');
        // A label can hold a file name or a literal text: no hidden character gets out (5k, A2).
        out.push_str(&crate::text::inline(&self.label));
        if let Some(diff) = &self.diff {
            out.push_str("   ");
            out.push_str(&crate::text::inline(diff));
        }
        if let Some(class) = self.class {
            out.push_str(&format!("   [{}]", serde_json::to_value(class).unwrap().as_str().unwrap()));
        }
        if let Some(summary) = &self.summary {
            out.push_str(&format!("   ({})", crate::text::inline(summary)));
        }
        if !self.steps.is_empty() {
            let numbers: Vec<String> = self.steps.iter().map(|step| step.to_string()).collect();
            out.push_str(&format!("   step{} {}", if numbers.len() == 1 { "" } else { "s" }, numbers.join(", ")));
        }
        out.push('\n');
        let child_prefix = if root { String::new() } else { format!("{prefix}{}", if last { "   " } else { "│  " }) };
        for (index, child) in self.children.iter().enumerate() {
            child.render_into(out, &child_prefix, index + 1 == self.children.len(), false);
        }
    }
}

/// The kind of a node in a flow graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowKind {
    Entry,
    Exit,
    Statement,
    Branch,
    Loop,
    /// A folded region, for example `… 4 statements unchanged`.
    Summary,
    /// A stub to code outside the frame.
    Stub,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlowNode {
    pub id: String,
    pub label: String,
    pub kind: FlowKind,
    pub status: Status,
    /// The anchor of the statement, for the link to the change tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlowEdge {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub status: Status,
}

/// A flow view: the smallest part of a control flow graph that contains a change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlowGraph {
    /// The function anchor.
    pub function: String,
    /// The breadcrumb, from the crate to the frame.
    pub breadcrumb: Vec<String>,
    pub nodes: Vec<FlowNode>,
    pub edges: Vec<FlowEdge>,
}

impl FlowGraph {
    /// Converts the graph to Mermaid text.
    pub fn to_mermaid(&self) -> String {
        let mut out = String::from("flowchart TD\n");
        for node in &self.nodes {
            let label = node.label.replace('"', "#quot;");
            let shape = match node.kind {
                FlowKind::Entry | FlowKind::Exit | FlowKind::Stub => format!("([\"{label}\"])"),
                FlowKind::Branch | FlowKind::Loop => format!("{{\"{label}\"}}"),
                FlowKind::Summary => format!("[/\"{label}\"/]"),
                FlowKind::Statement => format!("[\"{label}\"]"),
            };
            let class = match node.status {
                Status::Same => String::new(),
                status => format!(":::{}", serde_json::to_value(status).unwrap().as_str().unwrap()),
            };
            out.push_str(&format!("  {}{shape}{class}\n", node.id));
        }
        for edge in &self.edges {
            let arrow = match edge.status {
                Status::Removed => "-.->",
                Status::Added => "==>",
                _ => "-->",
            };
            match &edge.label {
                Some(label) => out.push_str(&format!("  {} {arrow}|{label}| {}\n", edge.from, edge.to)),
                None => out.push_str(&format!("  {} {arrow} {}\n", edge.from, edge.to)),
            }
        }
        out.push_str("  classDef added fill:#d4f8d4,stroke:#2a2\n");
        out.push_str("  classDef removed fill:#fdd,stroke:#a22,stroke-dasharray:5 5\n");
        out.push_str("  classDef changed fill:#fff3c4,stroke:#b80\n");
        out.push_str("  classDef moved fill:#e0ecff,stroke:#36c\n");
        out.push_str("  classDef extracted fill:#efe0ff,stroke:#83c\n");
        out.push_str("  classDef uncovered fill:#fbb,stroke:#c00,stroke-width:3px\n");
        out
    }
}

/// The kind of a node in the workspace graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeKind {
    Crate,
    Module,
    Function,
    Type,
    Trait,
    Impl,
    Other,
    /// A function outside the workspace (design option B).
    External,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    /// The item path.
    pub id: String,
    pub label: String,
    pub kind: GraphNodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub status: Status,
    /// The number of changes inside this node.
    #[serde(default)]
    pub changes: usize,
    /// For external leaves: badges from the signature (`err`, `mut`, `async`, `unsafe`, `never`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<String>,
    /// True for the entry points (`main`, `pub` items, tests).
    #[serde(default)]
    pub entry: bool,
    /// True for test functions.
    #[serde(default)]
    pub test: bool,
    /// The file of the item, relative to the workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeKind {
    /// A crate depends on another crate.
    Depends,
    /// A function calls another function.
    Call,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: GraphEdgeKind,
    pub status: Status,
}

/// The workspace graph (design section 10): crates, modules, items, and calls.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mermaid_output() {
        let graph = FlowGraph {
            function: "a::f".into(),
            breadcrumb: vec!["a".into(), "f".into()],
            nodes: vec![
                FlowNode { id: "n0".into(), label: "entry".into(), kind: FlowKind::Entry, status: Status::Same, anchor: None },
                FlowNode { id: "n1".into(), label: "x <= max".into(), kind: FlowKind::Branch, status: Status::Changed, anchor: None },
            ],
            edges: vec![FlowEdge { from: "n0".into(), to: "n1".into(), label: Some("yes".into()), status: Status::Same }],
        };
        let text = graph.to_mermaid();
        assert!(text.contains("n1{\"x <= max\"}:::changed"));
        assert!(text.contains("n0 -->|yes| n1"));
    }

    #[test]
    fn tree_render() {
        let tree = TreeNode {
            id: "a".into(),
            label: "crate a".into(),
            status: Status::Changed,
            class: None,
            claims: vec![],
            diff: None,
            steps: Vec::new(),
            summary: None,
            children: vec![TreeNode {
                id: "a::f".into(),
                label: "fn f".into(),
                status: Status::Added,
                class: Some(Class::Logic),
                claims: vec!["c1".into()],
                diff: None,
                steps: Vec::new(),
                summary: None,
                children: vec![],
            }],
        };
        let text = tree.render();
        assert!(text.contains("~ crate a"));
        assert!(text.contains("└─ + fn f   [logic]"));
    }
}
