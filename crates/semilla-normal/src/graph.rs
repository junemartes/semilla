//! The control flow graph of a body in a record of the driver (design 60, part A).
//!
//! The driver writes the graph of each body at N0 (`g` in the record). This module reads it and
//! pairs the graphs of a body over the targets of the two trees. The diff of two graphs needs
//! the tree compare, so it is in `semilla-verify`.

use serde::{Deserialize, Serialize};

/// The graph of one body, as the driver writes it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordGraph {
    /// The points, in the order of a walk from the entry.
    #[serde(default)]
    pub p: Vec<RecordPoint>,
    /// The arrows: the first point, the second point, the label.
    #[serde(default)]
    pub a: Vec<(u32, u32, String)>,
    /// The files of the places.
    #[serde(default)]
    pub f: Vec<String>,
    /// The counts of the parts that are not in the graph.
    #[serde(default)]
    pub n: GraphCounts,
    /// True for the body of a coroutine: the driver shows no graph (design 60, A6).
    #[serde(default)]
    pub co: bool,
}

/// One point of a graph.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordPoint {
    /// The kind: `entry`, `branch`, `call`, `return`, `panic`, `check`, `loop`, or the name of
    /// a construct (`try`, `for`).
    pub k: String,
    /// The text.
    #[serde(default)]
    pub t: String,
    /// The mark of a call that the MIR does not resolve: `closure`, `pointer`, `dyn`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub m: Option<String>,
    /// The range of the tokens: the file (an index into `f`), the start, the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<[i64; 3]>,
    /// The range of the outermost macro call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c: Option<[i64; 3]>,
    /// The callees of the parts of a point with more than one terminator (a construct, the panic
    /// of a macro), each text one time, in order (design 60, R60.2-11).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub x: Vec<String>,
    /// The value key of a check or a branch: the operation and each operand as a value (design
    /// 61, A1). Two points with equal keys do the same operation on the same values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<String>,
}

/// The counts of the parts of a body that are not in the graph.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphCounts {
    #[serde(default)]
    pub unwind: usize,
    #[serde(default)]
    pub cleanup: usize,
    #[serde(default)]
    pub drops: usize,
    #[serde(default)]
    pub unreachable: usize,
    /// The checks whose condition is a known value (design 61, A1): not points.
    #[serde(default)]
    pub folded_checks: usize,
    /// The branches on a known value: the walk follows one arrow.
    #[serde(default)]
    pub folded_branches: usize,
}

impl RecordGraph {
    /// True if the two graphs have the same points (kind, text, mark, callees) in the same order and
    /// the same arrows. The places can differ. Then the map of each point to the point with
    /// the same index pairs each point and shares each arrow: the graph is the same.
    pub fn same_shape(&self, other: &RecordGraph) -> bool {
        // A coroutine body has no points, so two coroutine graphs are never the same shape:
        // the empty lists prove nothing (design 60, A6).
        !self.co
            && !other.co
            && self.a == other.a
            && self.p.len() == other.p.len()
            && self.p.iter().zip(&other.p).all(|(a, b)| a.k == b.k && (a.t == b.t || (a.v.is_some() && a.v == b.v)) && a.m == b.m && a.x == b.x)
    }

    /// The place of a range of a point: the file, the start, and the end.
    pub fn place(&self, range: &Option<[i64; 3]>) -> Option<crate::elements::Place> {
        let [file, start, end] = (*range)?;
        let file = self.f.get(usize::try_from(file).ok()?)?;
        Some(crate::elements::Place { file: file.clone(), start: usize::try_from(start).ok()?, end: usize::try_from(end).ok()? })
    }
}

/// The graphs of one body on the two sides, for one pair of targets. `None`: the side has no
/// record of the body in the target, or the record has no graph.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphPair {
    /// True if the side has a record of the body.
    pub old_body: bool,
    pub new_body: bool,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "serialize_shared", deserialize_with = "deserialize_shared")]
    pub old: Option<std::sync::Arc<RecordGraph>>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "serialize_shared", deserialize_with = "deserialize_shared")]
    pub new: Option<std::sync::Arc<RecordGraph>>,
    /// The name and the target of the record of each side (for the equivalence check, design
    /// 61).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_key: Option<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_key: Option<(String, String)>,
    /// True if a side has two records of the body in targets of one name with different texts
    /// (R61.16-2): no pair, and no graph. The pair is "not compared".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not_compared: bool,
}

/// Writes a shared graph (for `BodyRecord::graph`).
pub fn serialize_shared<S: serde::Serializer>(value: &Option<std::sync::Arc<RecordGraph>>, serializer: S) -> Result<S::Ok, S::Error> {
    value.as_deref().serialize(serializer)
}

/// Reads a shared graph (for `BodyRecord::graph`).
pub fn deserialize_shared<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<std::sync::Arc<RecordGraph>>, D::Error> {
    Ok(Option::<RecordGraph>::deserialize(deserializer)?.map(std::sync::Arc::new))
}

/// Writes a shared text (for `BodyRecord::q`).
pub fn serialize_text<S: serde::Serializer>(value: &Option<std::sync::Arc<String>>, serializer: S) -> Result<S::Ok, S::Error> {
    value.as_deref().serialize(serializer)
}

/// Reads a shared text (for `BodyRecord::q`).
pub fn deserialize_text<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<std::sync::Arc<String>>, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.map(std::sync::Arc::new))
}

/// The graph with the rename map on the text of each call (a renamed callee is the same
/// callee).
pub fn renamed(graph: &std::sync::Arc<RecordGraph>, renames: &[(String, String)]) -> std::sync::Arc<RecordGraph> {
    if renames.is_empty() {
        return graph.clone();
    }
    // A fast test first: most call texts name no renamed path.
    let named = |text: &str| renames.iter().any(|(old, _)| text.contains(old.as_str()));
    if !graph.p.iter().any(|point| (matches!(point.k.as_str(), "call" | "panic") && point.m.is_none() && named(&point.t)) || point.x.iter().any(|text| named(text))) {
        return graph.clone();
    }
    // One replacer for the graph: its build is the larger part of the work.
    let replacer = crate::paths::Replacer::renames(renames);
    let mut out: Option<RecordGraph> = None;
    for (index, point) in graph.p.iter().enumerate() {
        if matches!(point.k.as_str(), "call" | "panic") && point.m.is_none() {
            let text = replacer.apply(&point.t);
            if text != point.t {
                out.get_or_insert_with(|| (**graph).clone()).p[index].t = text;
            }
        }
        // The callees of a construct and of the panic of a macro.
        let callees: Vec<String> = point.x.iter().map(|text| replacer.apply(text)).collect();
        if callees != point.x {
            out.get_or_insert_with(|| (**graph).clone()).p[index].x = callees;
        }
    }
    out.map_or_else(|| graph.clone(), std::sync::Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(kind: &str, text: &str, at: i64) -> RecordPoint {
        RecordPoint { k: kind.into(), t: text.into(), m: None, r: Some([0, at, at + 1]), c: None, x: Vec::new(), v: None }
    }

    // rule+ R60.3-7
    // rule- R60.3-7
    #[test]
    fn two_graphs_with_the_same_points_in_order_and_the_same_arrows_have_the_same_shape() {
        let old = RecordGraph { p: vec![point("entry", "", 0), point("branch", "x > 3", 10), point("return", "", 20)], a: vec![(0, 1, String::new()), (1, 2, "true".into()), (1, 2, "false".into())], f: vec!["src/lib.rs".into()], ..RecordGraph::default() };
        // A new place (the body moved down) keeps the shape.
        let mut moved = old.clone();
        moved.p.iter_mut().for_each(|point| point.r = point.r.map(|[file, start, end]| [file, start + 40, end + 40]));
        assert!(old.same_shape(&moved));
        // The negative cases: a new text, a new label.
        let mut text = old.clone();
        text.p[1].t = "x >= 3".into();
        assert!(!old.same_shape(&text));
        let mut label = old.clone();
        label.a[2].2 = "otherwise".into();
        assert!(!old.same_shape(&label));
        // Two coroutine graphs have no points. The empty lists are not a proof.
        let coroutine = RecordGraph { co: true, ..RecordGraph::default() };
        assert!(!coroutine.same_shape(&coroutine.clone()));
    }
}
