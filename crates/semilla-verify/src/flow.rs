//! The control flow graph of each body: the pair of the two graphs, the structural diff, and
//! the lines of the output (design 60, parts B and C).
//!
//! The driver gives the graph of each body (`semilla-mir/src/graph.rs`). This module pairs the
//! points of the old graph and of the new graph of a body:
//!
//! 1. through the node pairs of the tree compare: two points pair if the smallest node pair
//!    that holds their nodes is the same pair, and the kinds are equal. In one node pair, the
//!    points of one kind pair by their text (a longest common subsequence), then in source
//!    order;
//! 2. the points with no pair: equal kind and equal text, in source order (a longest common
//!    subsequence);
//! 3. the other points have no pair.
//!
//! An arrow is shared if its two ends pair and its labels are equal. The result of a body is
//! `same graph` (each point pairs, each arrow is shared, each point text is equal), `same
//! structure` (each point pairs and each arrow is shared, a point text changes), or `graph
//! changes` with the diff. "Same graph" is a proof: Semilla says it only when the pair exists.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::report::{FlowBody, FlowReport, Position};
use semilla_syntax::Workspace;

use crate::MirPlace;

/// One point of a graph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowPoint {
    /// `entry`, `branch`, `call`, `return`, `panic`, `check`, `loop`, or the name of a
    /// construct (`try`, `for`).
    pub kind: String,
    pub text: String,
    /// `closure`, `pointer`, or `dyn` for a call that the MIR does not resolve.
    pub mark: Option<String>,
    /// The range of the tokens.
    pub tokens: Option<MirPlace>,
    /// The range of the outermost macro call.
    pub call: Option<MirPlace>,
    /// The callees of the parts of a point with more than one terminator (a construct, the
    /// panic of a macro), each text one time, in order (R60.2-11).
    pub calls: Vec<String>,
    /// The value key of a check or a branch (design 61, A1).
    pub value: Option<String>,
}

impl FlowPoint {
    /// The place of the point in the tree (design 40.3): the tokens if they are inside the
    /// macro call (a macro input), else the range of the call.
    pub fn place(&self) -> Option<&MirPlace> {
        match (&self.tokens, &self.call) {
            (Some(tokens), Some(call)) if tokens.0 == call.0 && call.1 <= tokens.1 && tokens.2 <= call.2 => Some(tokens),
            (_, Some(call)) => Some(call),
            (tokens, None) => tokens.as_ref(),
        }
    }

    /// The name of the point in a line: the kind, the text, and the mark.
    pub fn label(&self) -> String {
        self.label_with(&self.text)
    }

    /// The name of the point in a fact line: the text of a callee in its short form
    /// ([`short_path`]).
    pub fn short_label(&self) -> String {
        self.label_with(&self.short_text())
    }

    fn label_with(&self, text: &str) -> String {
        let mark = self.mark.as_ref().map(|mark| format!(" [{mark}]")).unwrap_or_default();
        if text.is_empty() { format!("{}{mark}", self.kind) } else { format!("{} `{text}`{mark}", self.kind) }
    }

    /// True if the text of the point is the path of a callee (R60.2-11): a call that the MIR
    /// resolves, or a direct call of a panic function.
    fn callee(&self) -> bool {
        self.mark.is_none() && (self.kind == "call" || (self.kind == "panic" && self.calls.is_empty()))
    }

    /// The text for a fact line: the short form of a callee, else the text.
    pub fn short_text(&self) -> String {
        if self.callee() { short_path(&self.text) } else { self.text.clone() }
    }

    /// True if two points have the same text, the same mark, and the same callees.
    /// True if two points have the same text, mark, and callees. Two texts with equal value
    /// keys (design 61, A1: the same operation on the same values) are the same text.
    pub fn same_text(&self, other: &FlowPoint) -> bool {
        (self.text == other.text || (self.value.is_some() && self.value == other.value)) && self.mark == other.mark && self.calls == other.calls
    }
}

/// The short form of a path for a fact line (Decided by Claude): each path loses its module
/// part. A segment that starts with a lowercase letter or `_` and that is followed by `::` and
/// a name or `{` is a module part: `shop::price` → `price`, `<shop::Food as shop::Tax>::rate` →
/// `<Food as Tax>::rate`, `std::vec::Vec::<u32>::push` → `Vec::<u32>::push`,
/// `shop::total::{closure#0}` → `{closure#0}`. A fact line shows the short forms of a changed
/// text only if they differ (see `elements`): else it shows the full texts.
pub fn short_path(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let name_char = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < chars.len() {
        let starts = name_char(chars[at]) && !chars[at].is_ascii_digit() && (at == 0 || (!name_char(chars[at - 1]) && !(at >= 2 && chars[at - 1] == ':' && chars[at - 2] == ':')));
        if !starts {
            out.push(chars[at]);
            at += 1;
            continue;
        }
        // The module segments at the start of the path.
        let mut cut = at;
        loop {
            let mut end = cut;
            while end < chars.len() && name_char(chars[end]) {
                end += 1;
            }
            let lower = chars[cut].is_lowercase() || chars[cut] == '_';
            let next = chars.get(end + 2).copied();
            if end > cut && lower && chars.get(end) == Some(&':') && chars.get(end + 1) == Some(&':') && next.is_some_and(|c| (name_char(c) && !c.is_ascii_digit()) || c == '{') {
                cut = end + 2;
            } else {
                break;
            }
        }
        at = cut;
        // The rest of the name.
        while at < chars.len() && name_char(chars[at]) {
            out.push(chars[at]);
            at += 1;
        }
    }
    out
}

/// The graph of one body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowGraph {
    pub points: Vec<FlowPoint>,
    /// The first point, the second point, the label.
    pub arrows: Vec<(usize, usize, String)>,
    /// The counts of the parts that are not in the graph: unwind arrows, cleanup blocks,
    /// drops, arrows to `unreachable`.
    pub unwind: usize,
    pub cleanup: usize,
    pub drops: usize,
    pub unreachable: usize,
    /// True for a coroutine body: the driver shows no graph.
    pub coroutine: bool,
    /// The name and the target of the record (for the equivalence check, design 61).
    pub key: Option<(String, String)>,
}

/// One side of a pair of graphs.
#[derive(Clone, Debug, PartialEq)]
pub enum FlowSide {
    /// The side has no record of the body.
    NoBody,
    /// The record has no graph (an old record, a failure of the driver).
    NoGraph,
    Graph(FlowGraph),
}

/// The rank of a coroutine body with no graph: only an equal MIR text (level `n0`) shows that
/// the body keeps its control flow. Then a body with no edit needs no line. Else the body has a
/// line, because Semilla cannot show what changes (design 60, A6).
fn coroutine_rank(status: &crate::BodyStatus) -> FlowResult {
    match status {
        crate::BodyStatus::Equal { level, .. } if level == "n0" => FlowResult::Same,
        _ => FlowResult::Changes,
    }
}

/// The graphs of one body in one configuration and one pair of targets.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphPair {
    /// The name of the configuration.
    pub config: String,
    /// True if the two graphs have the same points in the same order and the same arrows: the
    /// map by the index is a pair of each point, so the graph is the same. The verifier computes
    /// it again after the pair of the nested bodies (R60.3-9).
    pub same: bool,
    pub old: FlowSide,
    pub new: FlowSide,
}

/// The result of a body (design 60, B4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowResult {
    Same,
    SameStructure,
    Changes,
}

/// The diff of two graphs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphDiff {
    /// The point pairs: the old index and the new index.
    pub pairs: Vec<(usize, usize)>,
    /// The new points with no pair, and the old points with no pair.
    pub added: Vec<usize>,
    pub removed: Vec<usize>,
    /// The pairs with a different text or mark.
    pub changed: Vec<(usize, usize)>,
    /// The arrows that are not shared: indexes into the arrows of the new graph and of the old
    /// graph.
    pub added_arrows: Vec<usize>,
    pub removed_arrows: Vec<usize>,
}

impl GraphDiff {
    pub fn result(&self) -> FlowResult {
        if !self.added.is_empty() || !self.removed.is_empty() || !self.added_arrows.is_empty() || !self.removed_arrows.is_empty() {
            FlowResult::Changes
        } else if !self.changed.is_empty() {
            FlowResult::SameStructure
        } else {
            FlowResult::Same
        }
    }
}

/// The most cells of one longest common subsequence. A larger list pairs equal texts in order.
const LCS_CELLS: usize = 4_000_000;

/// The pairs of a longest common subsequence of two lists, by `equal`.
fn lcs(a: &[usize], b: &[usize], equal: &dyn Fn(usize, usize) -> bool) -> Vec<(usize, usize)> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    if a.len() * b.len() > LCS_CELLS {
        // A greedy pair in order.
        let mut out = Vec::new();
        let mut from = 0;
        for &x in a {
            if let Some(found) = (from..b.len()).find(|&j| equal(x, b[j])) {
                out.push((x, b[found]));
                from = found + 1;
            }
        }
        return out;
    }
    let (n, m) = (a.len(), b.len());
    let mut table = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * (m + 1) + j] = if equal(a[i], b[j]) { table[(i + 1) * (m + 1) + j + 1] + 1 } else { table[(i + 1) * (m + 1) + j].max(table[i * (m + 1) + j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if equal(a[i], b[j]) {
            out.push((a[i], b[j]));
            i += 1;
            j += 1;
        } else if table[(i + 1) * (m + 1) + j] >= table[i * (m + 1) + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// The source order of the points of a graph: by the place, the longer place first; a point
/// with no place keeps its index, after the points with a place.
fn source_order(graph: &FlowGraph, list: &mut [usize]) {
    list.sort_by_key(|&index| match graph.points[index].place() {
        Some(place) => (0, place.0.clone(), place.1, usize::MAX - place.2, index),
        None => (1, String::new(), 0, 0, index),
    });
}

/// The diff of two graphs. `holder(side, point)` gives the smallest node pair of the tree
/// compare that holds the node of a point (0: old, 1: new), or `None`.
pub fn diff(old: &FlowGraph, new: &FlowGraph, holder: &dyn Fn(usize, usize) -> Option<usize>) -> GraphDiff {
    diff_with(old, new, holder, &|_, _| None)
}

/// [`diff`] with the source text of the node of each point. In a set of candidates, the
/// points with an equal text and an equal source text pair first (Decided by Claude): the tree
/// compare does not keep a pair of each node (the arms of a `match`), and two calls of one
/// function in one node pair then pair by their source.
pub fn diff_with(old: &FlowGraph, new: &FlowGraph, holder: &dyn Fn(usize, usize) -> Option<usize>, source: &dyn Fn(usize, usize) -> Option<String>) -> GraphDiff {
    let graphs = [old, new];
    let sources: [Vec<Option<String>>; 2] = [(0..old.points.len()).map(|point| source(0, point)).collect(), (0..new.points.len()).map(|point| source(1, point)).collect()];
    let same_source = |a: usize, b: usize| sources[0][a].is_some() && sources[0][a] == sources[1][b];
    let mut partner: [Vec<Option<usize>>; 2] = [vec![None; old.points.len()], vec![None; new.points.len()]];
    // Rule 1: the node pairs.
    let mut buckets: BTreeMap<(usize, &str), [Vec<usize>; 2]> = BTreeMap::new();
    for side in 0..2 {
        for (index, point) in graphs[side].points.iter().enumerate() {
            if let Some(pair) = holder(side, index) {
                buckets.entry((pair, point.kind.as_str())).or_default()[side].push(index);
            }
        }
    }
    let set = |partner: &mut [Vec<Option<usize>>; 2], a: usize, b: usize| {
        partner[0][a] = Some(b);
        partner[1][b] = Some(a);
    };
    for lists in buckets.values_mut() {
        if lists[0].is_empty() || lists[1].is_empty() {
            continue;
        }
        source_order(old, &mut lists[0]);
        source_order(new, &mut lists[1]);
        let same_text = |a: usize, b: usize| old.points[a].same_text(&new.points[b]);
        for (a, b) in lcs(&lists[0], &lists[1], &|a, b| same_text(a, b) && same_source(a, b)) {
            set(&mut partner, a, b);
        }
        let open: [Vec<usize>; 2] = [lists[0].iter().copied().filter(|a| partner[0][*a].is_none()).collect(), lists[1].iter().copied().filter(|b| partner[1][*b].is_none()).collect()];
        for (a, b) in lcs(&open[0], &open[1], &same_text) {
            set(&mut partner, a, b);
        }
        let rest: [Vec<usize>; 2] = [lists[0].iter().copied().filter(|a| partner[0][*a].is_none()).collect(), lists[1].iter().copied().filter(|b| partner[1][*b].is_none()).collect()];
        for (a, b) in rest[0].iter().zip(&rest[1]) {
            set(&mut partner, *a, *b);
        }
    }
    // Rule 2: equal kind and equal text, in source order.
    let mut rest: [Vec<usize>; 2] = [(0..old.points.len()).filter(|a| partner[0][*a].is_none()).collect(), (0..new.points.len()).filter(|b| partner[1][*b].is_none()).collect()];
    source_order(old, &mut rest[0]);
    source_order(new, &mut rest[1]);
    let same_point = |a: usize, b: usize| old.points[a].kind == new.points[b].kind && old.points[a].same_text(&new.points[b]);
    for (a, b) in lcs(&rest[0], &rest[1], &|a, b| same_point(a, b) && same_source(a, b)) {
        set(&mut partner, a, b);
    }
    let rest: [Vec<usize>; 2] = [rest[0].iter().copied().filter(|a| partner[0][*a].is_none()).collect(), rest[1].iter().copied().filter(|b| partner[1][*b].is_none()).collect()];
    for (a, b) in lcs(&rest[0], &rest[1], &same_point) {
        set(&mut partner, a, b);
    }
    let mut out = GraphDiff::default();
    for (a, b) in partner[0].iter().enumerate() {
        match b {
            Some(b) => {
                out.pairs.push((a, *b));
                if !old.points[a].same_text(&new.points[*b]) {
                    out.changed.push((a, *b));
                }
            }
            None => out.removed.push(a),
        }
    }
    out.added = (0..new.points.len()).filter(|b| partner[1][*b].is_none()).collect();
    let new_arrows: BTreeSet<(usize, usize, &str)> = new.arrows.iter().map(|(a, b, label)| (*a, *b, label.as_str())).collect();
    let mut shared_new: BTreeSet<(usize, usize, &str)> = BTreeSet::new();
    for (index, (a, b, label)) in old.arrows.iter().enumerate() {
        match (partner[0][*a], partner[0][*b]) {
            (Some(x), Some(y)) if new_arrows.contains(&(x, y, label.as_str())) => {
                shared_new.insert((x, y, label.as_str()));
            }
            _ => out.removed_arrows.push(index),
        }
    }
    out.added_arrows = new.arrows.iter().enumerate().filter(|(_, (a, b, label))| !shared_new.contains(&(*a, *b, label.as_str()))).map(|(index, _)| index).collect();
    out
}

/// A count with its noun: `1 point`, `2 points`.
fn count(value: usize, noun: &str) -> String {
    format!("{value} {noun}{}", if value == 1 { "" } else { "s" })
}

/// The plural of a kind for a count.
fn kinds(kind: &str, count: usize) -> String {
    if count == 1 {
        return kind.to_string();
    }
    match kind {
        "branch" => "branches".into(),
        "entry" => "entries".into(),
        _ => format!("{kind}s"),
    }
}

/// The summary of a diff for the step section: `+1 branch, +2 arrows, −1 arrow`, or for the
/// same structure `1 changed condition`.
pub fn summary(old: &FlowGraph, new: &FlowGraph, diff: &GraphDiff) -> String {
    let mut parts = Vec::new();
    let by_kind = |graph: &FlowGraph, list: &[usize]| -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for index in list {
            *counts.entry(graph.points[*index].kind.clone()).or_insert(0) += 1;
        }
        counts
    };
    for (kind, count) in by_kind(new, &diff.added) {
        parts.push(format!("+{count} {}", kinds(&kind, count)));
    }
    for (kind, count) in by_kind(old, &diff.removed) {
        parts.push(format!("−{count} {}", kinds(&kind, count)));
    }
    let mut changed: BTreeMap<String, usize> = BTreeMap::new();
    for (_, b) in &diff.changed {
        *changed.entry(new.points[*b].kind.clone()).or_insert(0) += 1;
    }
    for (kind, count) in changed {
        let noun = match kind.as_str() {
            "branch" => "condition".to_string(),
            "call" => "call target".to_string(),
            "panic" => "panic call".to_string(),
            "check" => "check".to_string(),
            "loop" => "loop".to_string(),
            other => format!("`{other}` point"),
        };
        parts.push(format!("{count} changed {noun}{}", if count == 1 { "" } else { "s" }));
    }
    if !diff.added_arrows.is_empty() {
        parts.push(format!("+{} arrow{}", diff.added_arrows.len(), if diff.added_arrows.len() == 1 { "" } else { "s" }));
    }
    if !diff.removed_arrows.is_empty() {
        parts.push(format!("−{} arrow{}", diff.removed_arrows.len(), if diff.removed_arrows.len() == 1 { "" } else { "s" }));
    }
    parts.join(", ")
}

/// The line numbers of the files of the two sides.
pub struct Lines<'a> {
    sides: [&'a Workspace; 2],
    cache: BTreeMap<(usize, String), Option<semilla_syntax::rs::Lines>>,
}

impl<'a> Lines<'a> {
    pub fn new(before: &'a Workspace, after: &'a Workspace) -> Lines<'a> {
        Lines { sides: [before, after], cache: BTreeMap::new() }
    }

    /// The line of a byte offset (from 1).
    pub fn line(&mut self, side: usize, file: &str, offset: usize) -> Option<usize> {
        let sides = self.sides;
        let lines = self.cache.entry((side, file.to_string())).or_insert_with(|| sides[side].rust_files.get(file).map(|text| semilla_syntax::rs::Lines::new(text))).as_ref()?;
        Some(lines.pos(offset.min(lines.text().len())).0)
    }

    /// The first and the last line of a place.
    pub fn span(&mut self, side: usize, place: &MirPlace) -> Option<(String, usize, usize)> {
        let start = self.line(side, &place.0, place.1)?;
        let end = self.line(side, &place.0, place.2.max(place.1 + 1) - 1)?;
        Some((place.0.clone(), start, end.max(start)))
    }

    /// The text of the place of a point: `src/lib.rs:12`, or `src/lib.rs:-12` on the old side.
    pub fn text(&mut self, side: usize, point: &FlowPoint) -> String {
        match point.place().and_then(|place| self.span(side, place)) {
            Some((file, start, _)) => format!("{file}:{}{start}", if side == 0 { "-" } else { "" }),
            None => "no place".to_string(),
        }
    }
}

/// The name of a point with its place: `branch `x > 3` (src/lib.rs:6)`. `short`: the label of
/// a fact line.
fn named(lines: &mut Lines, side: usize, graph: &FlowGraph, point: usize, short: bool) -> String {
    let point = &graph.points[point];
    format!("{} ({})", if short { point.short_label() } else { point.label() }, lines.text(side, point))
}

/// The target of an arrow in a line: `return`, `call `shop::price``.
fn target(graph: &FlowGraph, point: usize, short: bool) -> String {
    if short { graph.points[point].short_label() } else { graph.points[point].label() }
}

/// One arrow in a line: `true → return`, `→ call `f`` for an arrow with no label.
fn arrow_text(graph: &FlowGraph, label: &str, to: usize, short: bool) -> String {
    if label.is_empty() { format!("→ {}", target(graph, to, short)) } else { format!("{label} → {}", target(graph, to, short)) }
}

/// One element of a diff, for the lines of a fact and of `status --verbose`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Element {
    /// The side of the place (0: old, 1: new) and the places of the element on its sides.
    places: Vec<(usize, MirPlace)>,
    /// The text of the line.
    text: String,
    /// The arrows that the line shows (to skip them as arrow lines): side and index.
    arrows: Vec<(usize, usize)>,
    /// The lines of `status --verbose`: one line for the point, and one line for each arrow.
    details: Vec<String>,
    /// True if a fact holds the element only when it holds each place (an arrow between two
    /// points that pair): else one place is enough.
    all: bool,
}

/// The line of one arrow: `+ arrow branch `x > 3` (src/lib.rs:6) → return (src/lib.rs:9) (true)`.
fn arrow_line(sign: &str, graph: &FlowGraph, side: usize, index: usize, lines: &mut Lines, short: bool) -> String {
    let (from, to, label) = &graph.arrows[index];
    let label = if label.is_empty() { String::new() } else { format!(" ({label})") };
    format!("{sign} arrow {} → {}{label}", named(lines, side, graph, *from, short), named(lines, side, graph, *to, short))
}

/// The element of one point with its arrows that are not shared. The text of the fact line has
/// the short labels; the lines of `status --verbose` have the full texts.
fn point_element(sign: &str, graph: &FlowGraph, side: usize, point: usize, arrows: Vec<usize>, lines: &mut Lines) -> Element {
    let shown: Vec<String> = arrows.iter().map(|index| arrow_text(graph, &graph.arrows[*index].2, graph.arrows[*index].1, true)).collect();
    let head = format!("{sign} {}", named(lines, side, graph, point, true));
    let text = if shown.is_empty() { head.clone() } else { format!("{head}: {}", shown.join(", ")) };
    // The line of `status --verbose` has the callees of a construct or of the panic of a macro.
    let calls = &graph.points[point].calls;
    let callees = if calls.is_empty() { String::new() } else { format!("; callees {}", calls.iter().map(|text| format!("`{text}`")).collect::<Vec<_>>().join(", ")) };
    let mut details = vec![format!("{sign} {}{callees}", named(lines, side, graph, point, false))];
    details.extend(arrows.iter().map(|index| arrow_line(sign, graph, side, *index, lines, false)));
    Element { places: graph.points[point].place().cloned().map(|at| (side, at)).into_iter().collect(), text, arrows: arrows.into_iter().map(|index| (side, index)).collect(), details, all: false }
}

/// The elements of a diff: each changed point and each arrow that is not shared.
fn elements(old: &FlowGraph, new: &FlowGraph, diff: &GraphDiff, lines: &mut Lines) -> Vec<Element> {
    let graphs = [old, new];
    let mut out = Vec::new();
    let place = |graph: &FlowGraph, point: usize| graph.points[point].place().cloned();
    for (side, list, sign) in [(1usize, &diff.added, "+"), (0usize, &diff.removed, "−")] {
        let graph = graphs[side];
        let changed_arrows = if side == 1 { &diff.added_arrows } else { &diff.removed_arrows };
        for &point in list {
            let arrows: Vec<usize> = changed_arrows.iter().copied().filter(|index| graph.arrows[*index].0 == point).collect();
            out.push(point_element(sign, graph, side, point, arrows, lines));
        }
    }
    for &(a, b) in &diff.changed {
        let (x, y) = (&old.points[a], &new.points[b]);
        // `short`: the line of a fact. It shows the short forms only if they differ, else the
        // full texts. The callees of a construct or of the panic of a macro that only one side
        // has: in a fact line only if the texts are equal, in `status --verbose` always.
        let places = format!("({}, {})", lines.text(0, x), lines.text(1, y));
        let changed = |short: bool| -> String {
            let mark = |point: &FlowPoint| point.mark.as_ref().map(|mark| format!(" [{mark}]")).unwrap_or_default();
            let (from, to) = if short && x.short_text() != y.short_text() { (x.short_text(), y.short_text()) } else { (x.text.clone(), y.text.clone()) };
            let form = |text: &String| if short { short_path(text) } else { text.clone() };
            let gone = x.calls.iter().filter(|text| !y.calls.contains(text)).map(|text| format!("−`{}`", form(text)));
            let added = y.calls.iter().filter(|text| !x.calls.contains(text)).map(|text| format!("+`{}`", form(text)));
            let callees = gone.chain(added).collect::<Vec<_>>().join(" ");
            let same = x.text == y.text && x.mark == y.mark;
            match (same, callees.is_empty()) {
                (true, _) => format!("~ {} `{from}`{} {places}: callees {callees}", y.kind, mark(y)),
                (false, false) if !short => format!("~ {} `{from}`{} → `{to}`{} {places}; callees {callees}", y.kind, mark(x), mark(y)),
                _ => format!("~ {} `{from}`{} → `{to}`{} {places}", y.kind, mark(x), mark(y)),
            }
        };
        let text = changed(true);
        let detail = changed(false);
        let places = [place(old, a).map(|at| (0, at)), place(new, b).map(|at| (1, at))].into_iter().flatten().collect();
        out.push(Element { places, details: vec![detail], text, arrows: Vec::new(), all: false });
    }
    let shown: BTreeSet<(usize, usize)> = out.iter().flat_map(|element| element.arrows.iter().copied()).collect();
    for (side, list, sign) in [(1usize, &diff.added_arrows, "+"), (0usize, &diff.removed_arrows, "−")] {
        let graph = graphs[side];
        for &index in list {
            if shown.contains(&(side, index)) {
                continue;
            }
            let (from, to, _) = &graph.arrows[index];
            let text = arrow_line(sign, graph, side, index, lines, true);
            let detail = arrow_line(sign, graph, side, index, lines, false);
            // An arrow with an end that has no pair goes with that end. An arrow between two
            // points that pair goes with a fact that holds the two ends: the edit between them.
            let single = if side == 1 { &diff.added } else { &diff.removed };
            let ends: Vec<usize> = [*from, *to].into_iter().filter(|end| single.contains(end)).collect();
            let all = ends.is_empty();
            let ends = if all { vec![*from, *to] } else { ends };
            let places = ends.into_iter().filter_map(|end| place(graph, end)).map(|at| (side, at)).collect();
            out.push(Element { places, details: vec![detail], text, arrows: vec![(side, index)], all });
        }
    }
    out
}

/// The lines of a full graph on one side (a body that only one tree has).
fn whole(graph: &FlowGraph, side: usize, lines: &mut Lines) -> Vec<Element> {
    let sign = if side == 1 { "+" } else { "−" };
    (0..graph.points.len())
        .map(|point| {
            let arrows: Vec<usize> = (0..graph.arrows.len()).filter(|index| graph.arrows[*index].0 == point).collect();
            point_element(sign, graph, side, point, arrows, lines)
        })
        .collect()
}

/// The most `control flow:` lines under one fact (design 60, C1).
pub const FACT_LINES: usize = 3;

/// The holder function of a body: the smallest node pair of the tree compare that holds the
/// node of a point.
struct Holder<'a> {
    pairs: &'a [semilla_syntax::NodePair],
    /// The file of the item on each side.
    files: [Option<String>; 2],
}

impl Holder<'_> {
    fn of(&self, map: &mut crate::mirmap::MirMap, side: usize, point: &FlowPoint) -> Option<usize> {
        let place = point.place()?;
        if self.files[side].as_deref() != Some(place.0.as_str()) {
            return None;
        }
        let node = map.node(side, place)?;
        self.pairs
            .iter()
            .enumerate()
            .filter(|(_, pair)| pair.ranges[side].0 <= node.start && node.end <= pair.ranges[side].1)
            // The smallest node; of two pairs of one node, the pair with equal tokens (the tree
            // compare can try a pair first and find a better pair later).
            .max_by(|(_, a), (_, b)| a.ranges[side].0.cmp(&b.ranges[side].0).then(b.ranges[side].1.cmp(&a.ranges[side].1)).then(a.equal.cmp(&b.equal)))
            .map(|(index, _)| index)
    }
}

/// True if the item has no body node on each side that has it: a `const`, a `static`, an
/// associated constant, a type with a constant in its declaration. Its MIR bodies are parts of
/// its declaration (R61.15-1).
fn no_body_node(item: &str, old_item: &str, sides: [&Workspace; 2]) -> bool {
    let found = [sides[0].items.get(old_item), sides[1].items.get(item)];
    found.iter().any(Option::is_some) && found.iter().flatten().all(|found| found.body.is_none())
}

/// True if an edit of the step is in the body of an item, on one side.
///
/// An item with no body node (a `const`, a `static`, an associated constant) has its MIR bodies
/// in its declaration: each edit of the item, except a doc, a comment, an attribute, and the
/// visibility, is an edit of each of its bodies (R61.15-1).
fn edit_in_body(edits: &[semilla_syntax::Edit], item: &str, old_item: &str, sides: [&Workspace; 2]) -> bool {
    use semilla_syntax::EditKind;
    edits.iter().any(|edit| {
        if matches!(edit.kind, EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged | EditKind::Docs | EditKind::Comment | EditKind::Attrs) {
            return false;
        }
        [(&edit.before, 0usize, old_item), (&edit.after, 1usize, item)].into_iter().any(|(anchor, side, path)| {
            let Some(anchor) = anchor.as_ref().filter(|anchor| anchor.item == path) else { return false };
            // An added or a removed item is an edit of each body of the item.
            if matches!(edit.kind, EditKind::ItemAdded | EditKind::ItemRemoved) {
                return true;
            }
            let Some(found) = sides[side].items.get(path) else { return false };
            // The declaration of an item with no body node holds its bodies (R61.15-1).
            let Some(body) = &found.body else { return edit.kind != EditKind::Visibility };
            match semilla_syntax::resolve(sides[side], anchor) {
                Some(semilla_syntax::Resolved::Node(_, node)) => body.start <= node.start && node.end <= body.end,
                _ => false,
            }
        })
    })
}

/// One result of a body in one or more configurations.
struct Outcome {
    /// `same graph`, `same structure`, `graph changes`, `graph added`, …
    word: String,
    summary: String,
    elements: Vec<Element>,
    configs: Vec<String>,
    rank: FlowResult,
    chart: Option<semilla_core::report::FlowChart>,
}

/// The result of one body over its configurations.
struct BodyResult {
    /// The new item and the old item (for the facts of the body, design 61).
    item: Option<String>,
    old_item: Option<String>,
    /// The record keys of each configuration and pair of targets with a graph on each side.
    pairs: Vec<(String, (String, String), (String, String))>,
    /// True if the MIR texts are equal at `n0` (design 61: a proof with no solver).
    equal: bool,
    name: String,
    /// The name in the output (a nested body has its short source text).
    shown: String,
    place: String,
    edit: bool,
    /// The distinct outcomes, the worst first.
    outcomes: Vec<Outcome>,
}

/// The changed part of a diff for the flowchart (design 60, C4): the changed points, the ends
/// of the arrows that are not shared, and the points next to them. `None` for a part with more
/// than [`semilla_core::report::FLOW_CHART_POINTS`] points.
fn chart(old: &FlowGraph, new: &FlowGraph, diff: &GraphDiff, lines: &mut Lines) -> Option<semilla_core::report::FlowChart> {
    use semilla_core::report::{FlowChart, FlowChartEdge, FlowChartNode};
    let partner: BTreeMap<usize, usize> = diff.pairs.iter().copied().collect();
    let old_id = |point: usize| partner.get(&point).map(|new| format!("n{new}")).unwrap_or_else(|| format!("o{point}"));
    let mut part: BTreeSet<String> = BTreeSet::new();
    part.extend(diff.added.iter().map(|point| format!("n{point}")));
    part.extend(diff.removed.iter().map(|point| format!("o{point}")));
    part.extend(diff.changed.iter().map(|(_, point)| format!("n{point}")));
    for index in &diff.added_arrows {
        let (a, b, _) = &new.arrows[*index];
        part.extend([format!("n{a}"), format!("n{b}")]);
    }
    for index in &diff.removed_arrows {
        let (a, b, _) = &old.arrows[*index];
        part.extend([old_id(*a), old_id(*b)]);
    }
    // The points next to the changed part.
    let mut all = part.clone();
    for (a, b, _) in &new.arrows {
        let (x, y) = (format!("n{a}"), format!("n{b}"));
        if part.contains(&x) || part.contains(&y) {
            all.extend([x, y]);
        }
    }
    for (a, b, _) in &old.arrows {
        let (x, y) = (old_id(*a), old_id(*b));
        if part.contains(&x) || part.contains(&y) {
            all.extend([x, y]);
        }
    }
    if all.len() > semilla_core::report::FLOW_CHART_POINTS {
        return None;
    }
    let changed: BTreeMap<usize, usize> = diff.changed.iter().map(|(a, b)| (*b, *a)).collect();
    let mut nodes = Vec::new();
    for id in &all {
        let (side, index) = (if id.starts_with('n') { 1 } else { 0 }, id[1..].parse::<usize>().ok()?);
        let graph = if side == 1 { new } else { old };
        let point = &graph.points[index];
        let (label, mark) = if side == 0 {
            (point.label(), "removed")
        } else if diff.added.contains(&index) {
            (point.label(), "added")
        } else if let Some(before) = changed.get(&index) {
            (format!("{} `{}` → `{}`", point.kind, old.points[*before].text, point.text), "changed")
        } else {
            (point.label(), "kept")
        };
        nodes.push(FlowChartNode { id: id.clone(), label, mark: mark.to_string(), place: lines.text(side, point) });
    }
    let mut edges = Vec::new();
    for (index, (a, b, label)) in new.arrows.iter().enumerate() {
        let (x, y) = (format!("n{a}"), format!("n{b}"));
        if all.contains(&x) && all.contains(&y) {
            let mark = if diff.added_arrows.contains(&index) { "added" } else { "kept" };
            edges.push(FlowChartEdge { from: x, to: y, label: label.clone(), mark: mark.to_string() });
        }
    }
    for index in &diff.removed_arrows {
        let (a, b, label) = &old.arrows[*index];
        edges.push(FlowChartEdge { from: old_id(*a), to: old_id(*b), label: label.clone(), mark: "removed".to_string() });
    }
    Some(FlowChart { nodes, edges })
}

/// The flowchart of a full graph of one side (a body that only one tree has).
fn whole_chart(graph: &FlowGraph, side: usize, lines: &mut Lines) -> Option<semilla_core::report::FlowChart> {
    use semilla_core::report::{FlowChart, FlowChartEdge, FlowChartNode};
    if graph.points.len() > semilla_core::report::FLOW_CHART_POINTS {
        return None;
    }
    let (prefix, mark) = if side == 1 { ("n", "added") } else { ("o", "removed") };
    let nodes = graph.points.iter().enumerate().map(|(index, point)| FlowChartNode { id: format!("{prefix}{index}"), label: point.label(), mark: mark.to_string(), place: lines.text(side, point) }).collect();
    let edges = graph.arrows.iter().map(|(a, b, label)| FlowChartEdge { from: format!("{prefix}{a}"), to: format!("{prefix}{b}"), label: label.clone(), mark: mark.to_string() }).collect();
    Some(FlowChart { nodes, edges })
}

/// True if two graphs have the same points (kind, text, mark, callees) in the same order and
/// the same arrows (R60.3-7). A coroutine graph has no points, so it never has the same shape.
pub fn same_shape(old: &FlowGraph, new: &FlowGraph) -> bool {
    !old.coroutine && !new.coroutine && old.arrows == new.arrows && old.points.len() == new.points.len() && old.points.iter().zip(&new.points).all(|(a, b)| a.kind == b.kind && a.same_text(b))
}

/// One body for the graph compare: a body of the MIR compare, or the pair of two nested bodies
/// through the tree compare (R60.3-9).
struct Work {
    /// The MIR name (the new name of a pair of nested bodies).
    name: String,
    /// The name in the output: a nested body has its short source text after the name.
    shown: String,
    /// The place in the output: `src/lib.rs:12`.
    place: String,
    status: crate::BodyStatus,
    item: Option<String>,
    graphs: Vec<GraphPair>,
}

/// The kind of a nested body for its pair: the last segment of its name with no number
/// (`{closure#}`, `{constant#}`), or `fn` for a nested function.
fn nested_kind(name: &str) -> String {
    let last = name.rsplit("::").next().unwrap_or(name);
    if last.starts_with('{') { last.chars().filter(|c| !c.is_ascii_digit()).collect() } else { "fn".to_string() }
}

/// The source text of a place on one side, on one line.
fn source_text(side: &Workspace, place: &MirPlace) -> Option<String> {
    let text = side.rust_files.get(&place.0)?.get(place.1..place.2)?;
    Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The short source text of a nested body for its name in the output: one line, at most 40
/// characters.
fn short_source(text: &str) -> String {
    const MOST: usize = 40;
    if text.chars().count() <= MOST { text.to_string() } else { format!("{}…", text.chars().take(MOST - 1).collect::<String>()) }
}

/// One nested body on one side: the index of its MIR body, its graphs by configuration, its
/// place, its node, its source text.
struct NestedSide {
    body: usize,
    sides: Vec<(String, FlowSide)>,
    place: Option<MirPlace>,
    node: Option<semilla_syntax::tree::Pos>,
    end: Option<semilla_syntax::tree::Pos>,
    source: String,
    kind: String,
}

/// The bodies of the step for the graph compare (R60.3-9). A nested body (a closure, a
/// coroutine, a constant block, a nested function: a body whose name has the name of a body of
/// the same item first) pairs through the tree compare, not by its number:
///
/// 1. the old body and the new body pair if the smallest node pair that holds the node of each
///    is the same pair, and that pair holds no other nested body of the same kind with no pair.
///    In a pair with equal tokens, the nested bodies pair in source order (an equal count). An
///    item with no edit in its body and no node pair (a moved item) is one such pair;
/// 2. the bodies with no pair: an equal graph and an equal source text, in source order;
/// 3. the other bodies are added or removed.
///
/// The old texts of each graph get the new names of the nested bodies (a closure type in a
/// generic argument), and an old nested body with no pair gets a name that no new text has.
fn works(outcome: &crate::StepOutcome, index: &crate::checks::BodyIndex, map: &mut crate::mirmap::MirMap, lines: &mut Lines, sides: [&Workspace; 2], reverse: &BTreeMap<&String, &String>) -> Vec<Work> {
    let bodies: Vec<&crate::BodyComparison> = outcome.bodies.iter().filter(|body| !body.graphs.is_empty()).collect();
    let items: Vec<Option<String>> = bodies.iter().map(|body| index.item_of(body)).collect();
    let by_name: BTreeMap<&str, usize> = bodies.iter().enumerate().map(|(at, body)| (body.name.as_str(), at)).collect();
    // The parent of a body: the longest name of a body of the same item that is a first part
    // of its name.
    let parent = |at: usize| -> Option<usize> {
        let name = &bodies[at].name;
        let mut end = name.len();
        while let Some(cut) = name[..end].rfind("::") {
            if let Some(&other) = by_name.get(&name[..cut]) {
                if items[other].is_some() && items[other] == items[at] {
                    return Some(other);
                }
            }
            end = cut;
        }
        None
    };
    let mut root: Vec<Option<usize>> = vec![None; bodies.len()];
    for at in 0..bodies.len() {
        let mut current = at;
        let mut depth = 0;
        while let Some(up) = parent(current) {
            current = up;
            depth += 1;
            if depth > bodies.len() {
                break;
            }
        }
        if current != at {
            root[at] = Some(current);
        }
    }
    let place_text = |lines: &mut Lines, side: usize, place: &Option<MirPlace>| place.as_ref().and_then(|place| lines.span(side, place)).map(|(file, start, _)| format!("{file}:{}{start}", if side == 0 { "-" } else { "" }));
    let body_place = |body: &crate::BodyComparison| body.span.as_ref().map(|(file, line)| format!("{file}:{line}")).unwrap_or_else(|| "no place".to_string());
    let mut out: Vec<Work> = Vec::new();
    for (at, body) in bodies.iter().enumerate() {
        if root[at].is_none() {
            out.push(Work { name: body.name.clone(), shown: body.name.clone(), place: body_place(body), status: body.status.clone(), item: items[at].clone(), graphs: body.graphs.clone() });
        }
    }
    // The nested bodies of each root body.
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (at, found) in root.iter().enumerate() {
        if let Some(found) = found {
            groups.entry(*found).or_default().push(at);
        }
    }
    let empty: Vec<semilla_syntax::NodePair> = Vec::new();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let mut new_names: BTreeSet<String> = BTreeSet::new();
    let mut unpaired_old: Vec<String> = Vec::new();
    for (top, members) in groups {
        let item = items[top].clone();
        let old_item = item.as_ref().map(|item| reverse.get(item).map(|old| (*old).clone()).unwrap_or_else(|| item.clone()));
        let pairs = old_item.as_ref().and_then(|old| outcome.node_pairs.get(old)).map(Vec::as_slice).unwrap_or(&empty);
        let edit = match (&item, &old_item) {
            (Some(item), Some(old)) => edit_in_body(&outcome.edits, item, old, sides),
            _ => true,
        };
        // The two sides of each nested body.
        let mut lists: [Vec<NestedSide>; 2] = [Vec::new(), Vec::new()];
        for &member in &members {
            for side in 0..2 {
                let found: Vec<(String, FlowSide)> = bodies[member].graphs.iter().map(|pair| (pair.config.clone(), if side == 0 { pair.old.clone() } else { pair.new.clone() })).filter(|(_, found)| !matches!(found, FlowSide::NoBody)).collect();
                if found.is_empty() {
                    continue;
                }
                let place = found.iter().find_map(|(_, found)| match found {
                    FlowSide::Graph(graph) => graph.points.first().and_then(|point| point.place().cloned()),
                    _ => None,
                });
                let node = place.as_ref().and_then(|place| map.node(side, place));
                let source = place.as_ref().and_then(|place| source_text(sides[side], place)).unwrap_or_default();
                lists[side].push(NestedSide { body: member, sides: found, place, node: node.as_ref().map(|node| node.start), end: node.as_ref().map(|node| node.end), source, kind: nested_kind(&bodies[member].name) });
            }
        }
        for list in lists.iter_mut() {
            list.sort_by(|a, b| a.place.cmp(&b.place).then(a.body.cmp(&b.body)));
        }
        let mut partner: [Vec<Option<usize>>; 2] = [vec![None; lists[0].len()], vec![None; lists[1].len()]];
        // A body with no place on the two sides (a record with no graph) keeps the pair by its
        // name: Semilla has no node for it.
        for a in 0..lists[0].len() {
            if lists[0][a].place.is_none()
                && let Some(b) = (0..lists[1].len()).find(|b| lists[1][*b].body == lists[0][a].body && lists[1][*b].place.is_none())
            {
                partner[0][a] = Some(b);
                partner[1][b] = Some(a);
            }
        }
        // Rule 1: the smallest node pair that holds the node of each body.
        let holder = |side: usize, found: &NestedSide| -> Option<usize> {
            let (start, end) = (found.node?, found.end?);
            pairs.iter().enumerate().filter(|(_, pair)| pair.ranges[side].0 <= start && end <= pair.ranges[side].1).max_by(|(_, a), (_, b)| a.ranges[side].0.cmp(&b.ranges[side].0).then(b.ranges[side].1.cmp(&a.ranges[side].1)).then(a.equal.cmp(&b.equal))).map(|(at, _)| at)
        };
        let holders: [Vec<Option<usize>>; 2] = [lists[0].iter().map(|found| holder(0, found)).collect(), lists[1].iter().map(|found| holder(1, found)).collect()];
        let mut by_holder: BTreeMap<(usize, &str), [Vec<usize>; 2]> = BTreeMap::new();
        for side in 0..2 {
            for (at, found) in lists[side].iter().enumerate() {
                if let Some(pair) = holders[side][at] {
                    by_holder.entry((pair, found.kind.as_str())).or_default()[side].push(at);
                }
            }
        }
        for ((pair, _), [old, new]) in &by_holder {
            if (old.len() == 1 && new.len() == 1) || (pairs[*pair].equal && old.len() == new.len()) {
                for (a, b) in old.iter().zip(new) {
                    partner[0][*a] = Some(*b);
                    partner[1][*b] = Some(*a);
                }
            }
        }
        // An item with no edit in its body and no node pair: the bodies pair in source order.
        // An item with no body node (a `const`, a `static`) has no node pair: its bodies also
        // pair in source order, as before R61.15-1.
        let no_node = match (&item, &old_item) {
            (Some(item), Some(old)) => no_body_node(item, old, sides),
            _ => false,
        };
        if pairs.is_empty() && (!edit || no_node) {
            for kind in lists[0].iter().map(|found| found.kind.clone()).collect::<BTreeSet<_>>() {
                let open = |side: usize, partner: &[Vec<Option<usize>>; 2]| -> Vec<usize> { (0..lists[side].len()).filter(|at| partner[side][*at].is_none() && lists[side][*at].kind == kind).collect() };
                let (old, new) = (open(0, &partner), open(1, &partner));
                if old.len() == new.len() {
                    for (a, b) in old.into_iter().zip(new) {
                        partner[0][a] = Some(b);
                        partner[1][b] = Some(a);
                    }
                }
            }
        }
        // Rule 2: an equal graph and an equal source text, in source order.
        let first_graph = |found: &NestedSide| -> Option<FlowGraph> {
            found.sides.iter().find_map(|(_, side)| match side {
                FlowSide::Graph(graph) => Some(graph.clone()),
                _ => None,
            })
        };
        let open: [Vec<usize>; 2] = [(0..lists[0].len()).filter(|at| partner[0][*at].is_none()).collect(), (0..lists[1].len()).filter(|at| partner[1][*at].is_none()).collect()];
        let graphs: [Vec<Option<FlowGraph>>; 2] = [lists[0].iter().map(first_graph).collect(), lists[1].iter().map(first_graph).collect()];
        let equal = |a: usize, b: usize| {
            let (x, y) = (&lists[0][a], &lists[1][b]);
            x.kind == y.kind && !x.source.is_empty() && x.source == y.source && matches!((&graphs[0][a], &graphs[1][b]), (Some(g), Some(h)) if same_shape(g, h))
        };
        for (a, b) in lcs(&open[0], &open[1], &equal) {
            partner[0][a] = Some(b);
            partner[1][b] = Some(a);
        }
        // The works of the group.
        let shown = |lines: &mut Lines, side: usize, found: &NestedSide| -> (String, String) {
            let name = &bodies[found.body].name;
            let shown = if found.source.is_empty() { name.clone() } else { format!("{name} {}", short_source(&found.source)) };
            (shown, place_text(lines, side, &found.place).unwrap_or_else(|| body_place(bodies[found.body])))
        };
        let configs = |list: &[(String, FlowSide)]| -> Vec<String> {
            let mut seen: Vec<String> = Vec::new();
            for (config, _) in list {
                if !seen.contains(config) {
                    seen.push(config.clone());
                }
            }
            seen
        };
        let combine = |old: Option<&NestedSide>, new: Option<&NestedSide>| -> Vec<GraphPair> {
            let empty: Vec<(String, FlowSide)> = Vec::new();
            let (a, b) = (old.map_or(&empty, |found| &found.sides), new.map_or(&empty, |found| &found.sides));
            let mut order = configs(a);
            order.extend(configs(b).into_iter().filter(|config| !configs(a).contains(config)));
            let mut found = Vec::new();
            for config in order {
                let x: Vec<&FlowSide> = a.iter().filter(|(name, _)| *name == config).map(|(_, side)| side).collect();
                let y: Vec<&FlowSide> = b.iter().filter(|(name, _)| *name == config).map(|(_, side)| side).collect();
                for at in 0..x.len().max(y.len()) {
                    let old = x.get(at).map_or(FlowSide::NoBody, |side| (*side).clone());
                    let new = y.get(at).map_or(FlowSide::NoBody, |side| (*side).clone());
                    found.push(GraphPair { config: config.clone(), same: false, old, new });
                }
            }
            found
        };
        for (b, found) in lists[1].iter().enumerate() {
            new_names.insert(bodies[found.body].name.clone());
            let old = partner[1][b].map(|a| &lists[0][a]);
            let (shown, place) = shown(lines, 1, found);
            let name = bodies[found.body].name.clone();
            // A pair by the name keeps the status of the MIR compare; a different pair has no
            // proof of an equal MIR text.
            let status = match old {
                Some(old) if old.body == found.body => bodies[found.body].status.clone(),
                Some(_) => crate::BodyStatus::Different { cfg_changed: false },
                None => crate::BodyStatus::AfterOnly,
            };
            if let Some(old) = old {
                if old.body != found.body {
                    names.insert(bodies[old.body].name.clone(), name.clone());
                }
            }
            out.push(Work { name, shown, place, status, item: item.clone(), graphs: combine(old, Some(found)) });
        }
        for (a, found) in lists[0].iter().enumerate() {
            if partner[0][a].is_some() {
                continue;
            }
            unpaired_old.push(bodies[found.body].name.clone());
            let (shown, place) = shown(lines, 0, found);
            out.push(Work { name: bodies[found.body].name.clone(), shown, place, status: crate::BodyStatus::BeforeOnly, item: item.clone(), graphs: combine(Some(found), None) });
        }
    }
    // An old nested body with no pair whose name a new body has: its old name in a text must
    // not be equal to the new name.
    for name in unpaired_old {
        if new_names.contains(&name) {
            names.insert(name.clone(), format!("{name} (old)"));
        }
    }
    if !names.is_empty() {
        for work in out.iter_mut() {
            for pair in work.graphs.iter_mut() {
                if let FlowSide::Graph(graph) = &mut pair.old {
                    for point in graph.points.iter_mut() {
                        point.text = nested_names(&point.text, &names);
                        for text in point.calls.iter_mut() {
                            *text = nested_names(text, &names);
                        }
                    }
                }
            }
        }
    }
    // The same shape after the new names (R60.3-7).
    for work in out.iter_mut() {
        for pair in work.graphs.iter_mut() {
            pair.same = matches!((&pair.old, &pair.new), (FlowSide::Graph(old), FlowSide::Graph(new)) if same_shape(old, new));
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.place.cmp(&b.place)));
    out
}

/// The text with the new name of each nested body (R60.3-9): one pass, the longest name first,
/// so a new name is never replaced again.
fn nested_names(text: &str, names: &BTreeMap<String, String>) -> String {
    if !text.contains('{') {
        return text.to_string();
    }
    let mut keys: Vec<&String> = names.keys().collect();
    keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    'scan: while at < text.len() {
        let boundary = at == 0 || !text[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == ':');
        if boundary {
            for key in &keys {
                if text[at..].starts_with(key.as_str()) {
                    out.push_str(&names[*key]);
                    at += key.len();
                    continue 'scan;
                }
            }
        }
        let next = text[at..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[at..at + next]);
        at += next;
    }
    out
}

/// Adds the control flow lines of a step with the compiled check (design 60, part C): the
/// `control flow:` lines of each fact, the step section, and the full diff of each body for
/// `status --verbose`.
pub(crate) fn apply(input: &crate::StepInput, oracle: &dyn crate::BodyOracle, outcome: &mut crate::StepOutcome) {
    if !outcome.bodies.iter().any(|body| !body.graphs.is_empty()) {
        return;
    }
    apply_graphs(input, oracle, outcome);
    // The changed instances of the generic bodies (design 61, part B).
    let renames: BTreeMap<String, String> = outcome.renames.items.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
    let lines = oracle.instances(input.step, &renames);
    crate::equiv::instance_lines(outcome, lines);
}

fn apply_graphs(input: &crate::StepInput, oracle: &dyn crate::BodyOracle, outcome: &mut crate::StepOutcome) {
    let (before, after) = (input.before, input.after);
    let index = crate::checks::BodyIndex::new(&outcome.bodies, after, before);
    let reverse: BTreeMap<&String, &String> = outcome.renames.items.iter().map(|(old, new)| (new, old)).collect();
    let mut map = crate::mirmap::MirMap::new(before, after, &outcome.edits);
    let mut lines = Lines::new(before, after);
    let empty: Vec<semilla_syntax::NodePair> = Vec::new();
    let mut results: Vec<BodyResult> = Vec::new();
    let works = works(outcome, &index, &mut map, &mut lines, [before, after], &reverse);
    for body in &works {
        let item = body.item.clone();
        let old_item = item.as_ref().map(|item| reverse.get(item).map(|old| (*old).clone()).unwrap_or_else(|| item.clone()));
        let edit = match (&item, &old_item) {
            (Some(item), Some(old)) => edit_in_body(&outcome.edits, item, old, [before, after]),
            _ => false,
        };
        let holder = Holder {
            pairs: old_item.as_ref().and_then(|old| outcome.node_pairs.get(old)).map(Vec::as_slice).unwrap_or(&empty),
            files: [old_item.as_ref().and_then(|old| before.items.get(old)).map(|found| found.file.clone()), item.as_ref().and_then(|new| after.items.get(new)).map(|found| found.file.clone())],
        };
        let mut outcomes: Vec<Outcome> = Vec::new();
        for pair in &body.graphs {
            let found = if pair.same {
                Outcome { word: "same graph".into(), summary: String::new(), elements: Vec::new(), configs: Vec::new(), rank: FlowResult::Same, chart: None }
            } else {
                match (&pair.old, &pair.new) {
                    (FlowSide::Graph(old), FlowSide::Graph(new)) if old.coroutine || new.coroutine => Outcome { word: "graph not shown: coroutine".into(), summary: "the MIR of the driver is the state machine of the coroutine, not the source".into(), elements: Vec::new(), configs: Vec::new(), rank: coroutine_rank(&body.status), chart: None },
                    (FlowSide::Graph(old), FlowSide::Graph(new)) => {
                        let ids = [0usize, 1].map(|side| [old, new][side].points.iter().map(|point| holder.of(&mut map, side, point)).collect::<Vec<_>>());
                        let sources = [0usize, 1].map(|side| [old, new][side].points.iter().map(|point| point.place().and_then(|place| map.node(side, place)).map(|node| node.text)).collect::<Vec<_>>());
                        let mut found = diff_with(old, new, &|side, point| ids[side][point], &|side, point| sources[side][point].clone());
                        // A changed text that only facts of the category `rename` hold is the
                        // rename of the fact: such a fact covers a place only if the place is
                        // equal after its map (R49.3-1). The pair keeps its text (60.3, rule 8).
                        let mut renamed: Vec<String> = Vec::new();
                        found.changed.retain(|(a, b)| {
                            let (x, y) = (&old.points[*a], &new.points[*b]);
                            if x.mark != y.mark || x.calls != y.calls || !rename_only(&outcome.report.facts, [(0, x.place()), (1, y.place())], &mut lines) {
                                return true;
                            }
                            renamed.push(format!("= {} `{}` → `{}` ({}, {}): a rename", y.kind, x.text, y.text, lines.text(0, x), lines.text(1, y)));
                            false
                        });
                        let rank = found.result();
                        let word = match rank {
                            FlowResult::Same => "same graph",
                            FlowResult::SameStructure => "same structure",
                            FlowResult::Changes => "graph changes",
                        };
                        let chart = (rank == FlowResult::Changes).then(|| chart(old, new, &found, &mut lines)).flatten();
                        let mut elements = elements(old, new, &found, &mut lines);
                        // The renamed texts are lines of `status --verbose` only: no fact holds them.
                        elements.extend(renamed.into_iter().map(|text| Element { places: Vec::new(), details: vec![text.clone()], text, arrows: Vec::new(), all: true }));
                        Outcome { word: word.into(), summary: summary(old, new, &found), elements, configs: Vec::new(), rank, chart }
                    }
                    (FlowSide::NoBody, FlowSide::Graph(graph)) | (FlowSide::Graph(graph), FlowSide::NoBody) => {
                        let side = if matches!(pair.old, FlowSide::NoBody) { 1 } else { 0 };
                        let word = if side == 1 { "graph added" } else { "graph removed" };
                        if graph.coroutine {
                            Outcome { word: format!("{word}; graph not shown: coroutine"), summary: String::new(), elements: Vec::new(), configs: Vec::new(), rank: FlowResult::Changes, chart: None }
                        } else {
                            Outcome { word: word.into(), summary: format!("{}, {}", count(graph.points.len(), "point"), count(graph.arrows.len(), "arrow")), elements: whole(graph, side, &mut lines), configs: Vec::new(), rank: FlowResult::Changes, chart: whole_chart(graph, side, &mut lines) }
                        }
                    }
                    (FlowSide::NoBody, FlowSide::NoBody) => continue,
                    // Two records with no graph (the main function of a test harness, a
                    // constant of the build configuration) and an equal MIR text: the body
                    // needs no line, as a body with the same graph. With a different text, the
                    // body has a line.
                    (FlowSide::NoGraph, FlowSide::NoGraph) if matches!(body.status, crate::BodyStatus::Equal { .. }) => Outcome { word: "no graph data".into(), summary: "a record of the driver has no graph".into(), elements: Vec::new(), configs: Vec::new(), rank: FlowResult::Same, chart: None },
                    _ => Outcome { word: "no graph data".into(), summary: "a record of the driver has no graph".into(), elements: Vec::new(), configs: Vec::new(), rank: FlowResult::Changes, chart: None },
                }
            };
            // The full texts too: two short texts can be equal.
            let texts: Vec<(&String, &Vec<String>)> = found.elements.iter().map(|element| (&element.text, &element.details)).collect();
            match outcomes.iter_mut().find(|known| known.word == found.word && known.summary == found.summary && known.elements.iter().map(|element| (&element.text, &element.details)).collect::<Vec<_>>() == texts) {
                // A configuration with two targets (a library and its test build) names
                // the configuration one time.
                Some(known) if known.configs.contains(&pair.config) => {}
                Some(known) => known.configs.push(pair.config.clone()),
                None => outcomes.push(Outcome { configs: vec![pair.config.clone()], ..found }),
            }
        }
        if outcomes.is_empty() {
            continue;
        }
        // A body with no edit and the same graph needs no line (design 60, B6).
        if !edit && outcomes.iter().all(|known| known.rank == FlowResult::Same) {
            continue;
        }
        outcomes.sort_by(|a, b| b.rank.cmp(&a.rank).then(b.configs.len().cmp(&a.configs.len())));
        // The pairs for the equivalence check (design 61): two records with a graph that is not
        // a coroutine graph.
        let mut pairs = Vec::new();
        for pair in &body.graphs {
            if let (FlowSide::Graph(old), FlowSide::Graph(new)) = (&pair.old, &pair.new)
                && !old.coroutine
                && !new.coroutine
                && let (Some(a), Some(b)) = (&old.key, &new.key)
            {
                pairs.push((pair.config.clone(), a.clone(), b.clone()));
            }
        }
        let equal = matches!(&body.status, crate::BodyStatus::Equal { level, .. } if level == "n0");
        results.push(BodyResult { item: item.clone(), old_item: old_item.clone(), pairs, equal, name: body.name.clone(), shown: body.shown.clone(), place: body.place.clone(), edit, outcomes });
    }
    let mut report = FlowReport::default();
    let all_configs: BTreeSet<&String> = results.iter().flat_map(|result| result.outcomes.iter().flat_map(|known| known.configs.iter())).collect();
    // The bodies that do not keep their graph. A closure or a coroutine body `f::{closure#0}`
    // is a part of the item of `f`, so an edit in it is an edit of `f` too.
    let changed: Vec<&String> = results.iter().filter(|result| result.outcomes[0].rank != FlowResult::Same || result.outcomes[0].word != "same graph").map(|result| &result.name).collect();
    for result in &results {
        let first = &result.outcomes[0];
        // "The edit keeps the control flow" only if each body of the item keeps its graph.
        let inner_change = changed.iter().any(|name| name.starts_with(&format!("{}::{{", result.name)));
        let mut line = if first.summary.is_empty() {
            if first.rank == FlowResult::Same && first.word == "same graph" && result.edit && !inner_change { format!("{} (the edit keeps the control flow)", first.word) } else { first.word.clone() }
        } else {
            format!("{}: {}", first.word, first.summary)
        };
        if result.outcomes.len() > 1 {
            line.push_str(&format!(" (in {}; {} more result(s) in other configurations: semilla status --verbose)", first.configs.join(", "), result.outcomes.len() - 1));
        }
        if !result.edit && first.rank != FlowResult::Same {
            line.push_str("; no edit in the body");
        }
        let mut details = Vec::new();
        for known in &result.outcomes {
            let in_configs = if all_configs.len() > 1 { format!(" (in {})", known.configs.join(", ")) } else { String::new() };
            details.push(format!("{}{}{in_configs}", known.word, if known.summary.is_empty() { String::new() } else { format!(": {}", known.summary) }));
            details.extend(known.elements.iter().flat_map(|element| element.details.iter().map(|line| format!("  {line}"))));
        }
        let list = first
            .elements
            .iter()
            .map(|element| {
                let place = element.places.first().and_then(|(side, place)| lines.span(*side, place).map(|(file, start, _)| format!("{file}:{}{start}", if *side == 0 { "-" } else { "" }))).unwrap_or_else(|| "no place".to_string());
                (place, element.text.clone())
            })
            .collect();
        report.bodies.push(FlowBody { name: result.shown.clone(), place: result.place.clone(), line, details, chart: first.chart.clone(), list });
    }
    // The lines of each fact: each element whose place is in a position of the fact.
    for fact in &outcome.report.facts {
        if fact.positions.is_empty() {
            continue;
        }
        let mut found: Vec<String> = Vec::new();
        for result in &results {
            // The lines of the first result of the body (the worst; the step line names the
            // other configurations).
            let known = &result.outcomes[0];
            let mut held = |element: &Element| {
                let mut each = element.places.iter().map(|(side, place)| lines.span(*side, place).is_some_and(|(file, start, end)| holds(&fact.positions, *side, &file, start, end)));
                if element.all { !element.places.is_empty() && each.all(|held| held) } else { each.any(|held| held) }
            };
            // A full graph of a body that one tree has: one line with the counts (design 60,
            // B5).
            if known.word.starts_with("graph added") || known.word.starts_with("graph removed") {
                if known.elements.iter().any(&mut held) {
                    let sign = if known.word.starts_with("graph added") { "+" } else { "−" };
                    found.push(format!("{sign} graph of `{}`: {} (semilla status --verbose)", result.shown, known.summary));
                }
                continue;
            }
            for element in &known.elements {
                if held(element) && !found.contains(&element.text) {
                    found.push(element.text.clone());
                }
            }
        }
        if found.is_empty() {
            continue;
        }
        let more = found.len().saturating_sub(FACT_LINES);
        found.truncate(FACT_LINES);
        if more > 0 {
            found.push(format!("… {more} more (semilla status --verbose)"));
        }
        report.facts.insert(fact.id.clone(), found);
    }
    outcome.report.flow = Some(report);
    // The equivalence of the two versions of each body (design 61).
    let checks: Vec<crate::equiv::BodyCheck> = results
        .iter()
        .enumerate()
        .filter(|(_, result)| !result.pairs.is_empty())
        .map(|(index, result)| crate::equiv::BodyCheck { index, item: result.item.clone(), old_item: result.old_item.clone(), name: result.name.clone(), shown: result.shown.clone(), pairs: result.pairs.clone(), equal: result.equal })
        .collect();
    if !checks.is_empty() {
        crate::equiv::apply(input, oracle, outcome, &checks);
    }
}

/// True if each fact that holds a place of a point pair is a fact of the category `rename`,
/// and one fact holds a place.
fn rename_only(facts: &[semilla_core::Fact], places: [(usize, Option<&MirPlace>); 2], lines: &mut Lines) -> bool {
    let spans: Vec<(usize, (String, usize, usize))> = places.iter().filter_map(|(side, place)| Some((*side, lines.span(*side, (*place)?)?))).collect();
    let mut any = false;
    for fact in facts {
        if spans.iter().any(|(side, (file, start, end))| holds(&fact.positions, *side, file, *start, *end)) {
            if fact.category != semilla_core::Category::Rename {
                return false;
            }
            any = true;
        }
    }
    any
}

/// True if a position of a fact holds a line range of one side.
fn holds(positions: &[Position], side: usize, file: &str, start: usize, end: usize) -> bool {
    positions.iter().any(|position| position.file == file && position.old == (side == 0) && position.start <= end && start <= position.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A point at a place of `src/lib.rs`.
    fn point(kind: &str, text: &str, at: usize) -> FlowPoint {
        FlowPoint { kind: kind.into(), text: text.into(), mark: None, tokens: Some(("src/lib.rs".into(), at, at + 1)), call: None, calls: Vec::new(), value: None }
    }

    fn graph(points: Vec<FlowPoint>, arrows: &[(usize, usize, &str)]) -> FlowGraph {
        FlowGraph { points, arrows: arrows.iter().map(|(a, b, label)| (*a, *b, label.to_string())).collect(), ..FlowGraph::default() }
    }

    /// A holder function from a list for each side: the node pair of each point.
    fn holders(old: Vec<Option<usize>>, new: Vec<Option<usize>>) -> impl Fn(usize, usize) -> Option<usize> {
        move |side, point| if side == 0 { old[point] } else { new[point] }
    }

    // rule+ R60.3-1
    // rule- R60.3-1
    #[test]
    fn two_points_pair_through_the_smallest_node_pair_and_not_by_an_equal_text() {
        // Two statements keep their node pairs (0 ↔ 0, 1 ↔ 1). The calls swap their callees:
        // each call pairs with the call of its own statement, so two call targets change. A pair
        // by the text would give two equal calls and hide the swap of the order.
        let old = graph(vec![point("entry", "", 0), point("call", "demo::f", 10), point("call", "demo::g", 20), point("return", "", 30)], &[(0, 1, ""), (1, 2, "return"), (2, 3, "return")]);
        let new = graph(vec![point("entry", "", 0), point("call", "demo::g", 10), point("call", "demo::f", 20), point("return", "", 30)], &[(0, 1, ""), (1, 2, "return"), (2, 3, "return")]);
        let found = diff(&old, &new, &holders(vec![None, Some(0), Some(1), None], vec![None, Some(0), Some(1), None]));
        assert_eq!(found.changed, vec![(1, 1), (2, 2)]);
        assert_eq!(found.result(), FlowResult::SameStructure);
        // The negative case: with no node pair, the equal texts pair (rule 2), and the arrows
        // show the new order.
        let found = diff(&old, &new, &holders(vec![None; 4], vec![None; 4]));
        assert!(found.changed.is_empty(), "{found:?}");
        assert_eq!(found.result(), FlowResult::Changes);
        // A node pair needs equal kinds: a call and a panic in one node pair do not pair.
        let panic = graph(vec![point("entry", "", 0), point("panic", "demo::f", 10), point("call", "demo::g", 20), point("return", "", 30)], &[(0, 1, "")]);
        let found = diff(&old, &panic, &holders(vec![None, Some(0), Some(1), None], vec![None, Some(0), Some(1), None]));
        assert_eq!((found.removed.clone(), found.added.clone()), (vec![1], vec![1]), "{found:?}");
    }

    // rule+ R60.3-2
    // rule- R60.3-2
    #[test]
    fn a_point_with_no_node_pair_pairs_by_its_kind_and_its_text_in_source_order() {
        let old = graph(vec![point("entry", "", 0), point("call", "demo::f", 10), point("return", "", 30)], &[(0, 1, ""), (1, 2, "return")]);
        let new = graph(vec![point("entry", "", 0), point("branch", "x > 3", 5), point("call", "demo::f", 10), point("return", "", 30)], &[(0, 1, ""), (1, 2, "true"), (1, 3, "false"), (2, 3, "return")]);
        let found = diff(&old, &new, &holders(vec![None; 3], vec![None; 4]));
        assert_eq!(found.pairs, vec![(0, 0), (1, 2), (2, 3)]);
        assert_eq!(found.added, vec![1]);
        // The negative case: a different text gives no pair.
        let other = graph(vec![point("entry", "", 0), point("call", "demo::h", 10), point("return", "", 30)], &[(0, 1, ""), (1, 2, "return")]);
        let found = diff(&old, &other, &holders(vec![None; 3], vec![None; 3]));
        assert_eq!((found.removed.clone(), found.added.clone()), (vec![1], vec![1]));
    }

    // rule+ R60.3-3
    // rule- R60.3-3
    #[test]
    fn an_arrow_is_shared_only_if_its_two_ends_pair_and_its_labels_are_equal() {
        let old = graph(vec![point("entry", "", 0), point("branch", "x > 3", 5), point("return", "", 30)], &[(0, 1, ""), (1, 2, "true"), (1, 2, "false")]);
        let found = diff(&old, &old.clone(), &holders(vec![None; 3], vec![None; 3]));
        assert!(found.added_arrows.is_empty() && found.removed_arrows.is_empty());
        assert_eq!(found.result(), FlowResult::Same);
        // The negative case: the same two ends with a new label.
        let new = graph(old.points.clone(), &[(0, 1, ""), (1, 2, "true"), (1, 2, "otherwise")]);
        let found = diff(&old, &new, &holders(vec![None; 3], vec![None; 3]));
        assert_eq!((found.removed_arrows.clone(), found.added_arrows.clone()), (vec![2], vec![2]));
        assert_eq!(found.result(), FlowResult::Changes);
    }

    // rule+ R60.3-4
    // rule- R60.3-4
    #[test]
    fn the_result_of_a_body_is_same_graph_same_structure_or_graph_changes() {
        let old = graph(vec![point("entry", "", 0), point("branch", "x > 3", 5), point("return", "", 30)], &[(0, 1, ""), (1, 2, "true"), (1, 2, "false")]);
        let same = diff(&old, &old.clone(), &holders(vec![None; 3], vec![None; 3]));
        assert_eq!(same.result(), FlowResult::Same);
        let mut changed = old.clone();
        changed.points[1].text = "x >= 3".into();
        let found = diff(&old, &changed, &holders(vec![None, Some(0), None], vec![None, Some(0), None]));
        assert_eq!(found.result(), FlowResult::SameStructure);
        assert_eq!(summary(&old, &changed, &found), "1 changed condition");
        // The negative case: a changed text with no node pair is a removed point and an added
        // point, so the result is not `same structure`.
        let found = diff(&old, &changed, &holders(vec![None; 3], vec![None; 3]));
        assert_eq!(found.result(), FlowResult::Changes);
        assert_eq!(summary(&old, &changed, &found), "+1 branch, −1 branch, +3 arrows, −3 arrows");
    }

    // rule+ R60.4-3
    // rule- R60.4-3
    #[test]
    fn each_changed_point_and_each_arrow_that_is_not_shared_has_a_line() {
        let ws = semilla_syntax::Workspace::default();
        let mut lines = Lines::new(&ws, &ws);
        let old = graph(vec![point("entry", "", 0), point("call", "demo::price", 10), point("return", "", 30)], &[(0, 1, ""), (1, 2, "return")]);
        let new = graph(vec![point("entry", "", 0), point("branch", "qty == 0", 5), point("call", "demo::price", 10), point("return", "", 30)], &[(0, 1, ""), (1, 3, "true"), (1, 2, "false"), (2, 3, "return")]);
        let found = diff(&old, &new, &holders(vec![None; 3], vec![None; 4]));
        let texts: Vec<String> = elements(&old, &new, &found, &mut lines).into_iter().map(|element| element.text).collect();
        assert_eq!(texts, ["+ branch `qty == 0` (no place): true → return, false → call `price`", "+ arrow entry (no place) → branch `qty == 0` (no place)", "− arrow entry (no place) → call `price` (no place)"]);
        // The negative case: a shared arrow and a pair with an equal text have no line.
        let found = diff(&old, &old.clone(), &holders(vec![None; 3], vec![None; 3]));
        assert!(elements(&old, &old.clone(), &found, &mut lines).is_empty());
    }

    // rule+ R60.4-7
    // rule- R60.4-7
    #[test]
    fn a_fact_line_shows_the_short_form_of_a_callee_only_if_the_short_forms_differ() {
        assert_eq!(short_path("<shop::Food as shop::Tax>::rate"), "<Food as Tax>::rate");
        assert_eq!(short_path("std::vec::Vec::<u32>::push"), "Vec::<u32>::push");
        assert_eq!(short_path("shop::parse::<u32>"), "parse::<u32>");
        assert_eq!(short_path("<std::iter::Map<std::slice::Iter<'_, u32>, shop::total::{closure#1}> as std::iter::Iterator>::sum::<u32>"), "<Map<Iter<'_, u32>, {closure#1}> as Iterator>::sum::<u32>");
        let ws = semilla_syntax::Workspace::default();
        let mut lines = Lines::new(&ws, &ws);
        let changed = |from: &str, to: &str, lines: &mut Lines| {
            let old = graph(vec![point("entry", "", 0), point("call", from, 10), point("return", "", 30)], &[(0, 1, ""), (1, 2, "return")]);
            let new = graph(vec![point("entry", "", 0), point("call", to, 10), point("return", "", 30)], &[(0, 1, ""), (1, 2, "return")]);
            let found = diff(&old, &new, &holders(vec![None, Some(0), None], vec![None, Some(0), None]));
            elements(&old, &new, &found, lines).into_iter().map(|element| (element.text, element.details)).next().unwrap()
        };
        let (text, details) = changed("<shop::Food as shop::Tax>::rate", "<shop::Fuel as shop::Tax>::rate", &mut lines);
        assert_eq!(text, "~ call `<Food as Tax>::rate` → `<Fuel as Tax>::rate` (no place, no place)");
        assert_eq!(details, ["~ call `<shop::Food as shop::Tax>::rate` → `<shop::Fuel as shop::Tax>::rate` (no place, no place)"]);
        // The negative case: two paths with equal short forms show the full forms.
        let (text, _) = changed("a::Food::new", "b::Food::new", &mut lines);
        assert_eq!(text, "~ call `a::Food::new` → `b::Food::new` (no place, no place)");
    }

    // rule+ R60.2-11
    // rule- R60.2-11
    #[test]
    fn a_changed_callee_of_a_construct_is_a_changed_point() {
        let mut old = graph(vec![point("entry", "", 0), point("for", "make()", 10), point("return", "", 30)], &[(0, 1, ""), (1, 2, "done"), (1, 1, "next")]);
        let mut new = old.clone();
        old.points[1].calls = vec!["<shop::Food as std::iter::Iterator>::next".into()];
        new.points[1].calls = vec!["<shop::Fuel as std::iter::Iterator>::next".into()];
        let found = diff(&old, &new, &holders(vec![None, Some(0), None], vec![None, Some(0), None]));
        assert_eq!(found.result(), FlowResult::SameStructure);
        assert!(!same_shape(&old, &new));
        let ws = semilla_syntax::Workspace::default();
        let texts: Vec<String> = elements(&old, &new, &found, &mut Lines::new(&ws, &ws)).into_iter().map(|element| element.text).collect();
        assert_eq!(texts, ["~ for `make()` (no place, no place): callees −`<Food as Iterator>::next` +`<Fuel as Iterator>::next`"]);
        // The negative case: equal callees keep the graph.
        new.points[1].calls = old.points[1].calls.clone();
        assert!(same_shape(&old, &new));
        assert_eq!(diff(&old, &new, &holders(vec![None; 3], vec![None; 3])).result(), FlowResult::Same);
    }

    // rule+ R60.3-9
    // rule- R60.3-9
    #[test]
    fn the_old_texts_get_the_new_names_of_the_nested_bodies_in_one_pass() {
        let names: BTreeMap<String, String> = [("shop::f::{closure#0}".to_string(), "shop::f::{closure#1}".to_string()), ("shop::f::{closure#1}".to_string(), "shop::f::{closure#1} (old)".to_string())].into_iter().collect();
        assert_eq!(nested_names("<I as Iterator>::map::<u32, shop::f::{closure#0}>", &names), "<I as Iterator>::map::<u32, shop::f::{closure#1}>");
        assert_eq!(nested_names("map::<u32, shop::f::{closure#1}>", &names), "map::<u32, shop::f::{closure#1} (old)>");
        // The negative cases: a longer number and a name with a different start stay.
        assert_eq!(nested_names("map::<u32, shop::f::{closure#10}>", &names), "map::<u32, shop::f::{closure#10}>");
        assert_eq!(nested_names("xshop::f::{closure#0}", &names), "xshop::f::{closure#0}");
    }

}
