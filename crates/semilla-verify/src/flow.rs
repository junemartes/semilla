//! The control flow graph of each body: the pair of the two graphs, the structural diff, and
//! the lines of the output (design 60, parts B and C; design 98).
//!
//! The driver gives the graph of each body (`semilla-mir/src/graph.rs`). [R98.1-1] The points of
//! the old graph and of the new graph of a body pair through the MIR alignment of design 90
//! (`flowalign.rs`): a point pairs with the point whose MIR elements the alignment pairs with its
//! elements. A point whose elements the alignment does not pair is an added or a removed point.
//! No other rule pairs two points (the rules R60.3-1 and R60.3-2 are gone).
//!
//! An arrow is shared if its two ends pair and its labels are equal. The result of a body is
//! `same graph` (each point pairs, each arrow is shared, each point text is equal), `same
//! structure` (each point pairs and each arrow is shared, a point text changes), or `graph
//! changes` with the diff. "Same graph" is a proof: Semilla says it only when the pair exists.
//!
//! [R98.2-1] Each changed point and each arrow that is not shared is a graph change. The step keeps
//! the graph changes of each body (`BodyChanges`) for the map of design 98 (`flowmap.rs`): each
//! graph change goes to one entry of the map.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::report::{FlowBody, FlowReport};
use semilla_syntax::Workspace;

use crate::MirPlace;

/// One point of a graph.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// True for a coroutine body whose MIR before the coroutine transform the driver does not
    /// have (R76.2-1): the record shows no graph. A coroutine body with that MIR has its full
    /// graph and no mark.
    pub coroutine: bool,
    /// The name and the target of the record (for the equivalence check, design 61).
    pub key: Option<(String, String)>,
}

/// The cause of a pair of graphs that is not compared (R61.16-2).
pub const NOT_COMPARED: &str = "two records of one body in one configuration";

/// One side of a pair of graphs.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FlowSide {
    /// The side has no record of the body.
    NoBody,
    /// The record has no graph (an old record, a failure of the driver).
    NoGraph,
    /// The side has two records of the body in one configuration with different texts, and no
    /// pair of them (R61.16-2): Semilla does not choose one.
    NotCompared,
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GraphPair {
    /// The name of the configuration.
    pub config: String,
    /// True if the two graphs have the same points in the same order and the same arrows: the
    /// map by the index is a pair of each point, so the graph is the same. The verifier computes
    /// it again after the pair of the nested bodies (R60.3-9).
    pub same: bool,
    pub old: FlowSide,
    pub new: FlowSide,
    /// Each constant item that the two records read with a different value: the path, the old
    /// value, and the new value (design 64, part D).
    pub consts: Vec<(String, String, String)>,
    /// R73.2-1: each static with interior mutability that the two records read with a different
    /// initial value: the path, the old value, and the new value. The check reads such a static
    /// from the world (R64.2), so the list gives a line and an affected body, not a result.
    pub statics: Vec<(String, String, String)>,
}

/// The result of a body (design 60, B4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
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

/// The words of a constant whose value has a part that the driver cannot compute (R66.11-4): the
/// check does not know if it changes.
pub const CANNOT_COMPARE: &str = "reads a constant that the check cannot compare";

/// [R98.1-1] The diff of two graphs from the point pairs of the alignment (`flowalign::align`).
/// [R98.6-2] A pair with a different text, mark, or callees is a changed point, also when its
/// MIR elements are equal (a renamed local in a condition, a changed value of an operand that
/// an element with no point computes): the check of a transformation explains it (R98.6-1).
pub fn diff_pairs(old: &FlowGraph, new: &FlowGraph, pairs: &[(usize, usize)]) -> GraphDiff {
    let mut partner: [Vec<Option<usize>>; 2] = [vec![None; old.points.len()], vec![None; new.points.len()]];
    for (a, b) in pairs {
        if *a < old.points.len() && *b < new.points.len() && partner[0][*a].is_none() && partner[1][*b].is_none() && old.points[*a].kind == new.points[*b].kind {
            partner[0][*a] = Some(*b);
            partner[1][*b] = Some(*a);
        }
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
pub(crate) struct Element {
    /// The side of the place (0: old, 1: new) and the places of the element on its sides.
    places: Vec<(usize, MirPlace)>,
    /// The text of the line.
    pub(crate) text: String,
    /// The arrows that the line shows (to skip them as arrow lines): side and index.
    arrows: Vec<(usize, usize)>,
    /// The lines of `status --verbose`: one line for the point, and one line for each arrow.
    pub(crate) details: Vec<String>,
    /// True if a fact holds the element only when it holds each place (an arrow between two
    /// points that pair): else one place is enough.
    all: bool,
    /// The side and the index of the point of a point element (R71.2-1: a point that moves to a
    /// different body of its item).
    point: Option<(usize, usize)>,
    /// [R98.2-1] The anchor of the element for its entry (design 98).
    pub(crate) anchor: Anchor,
    /// [R98.2-2] One part of the summary of an entry for each line of `details`: the point, then
    /// each arrow. Empty: no graph change (a line of a renamed text).
    pub(crate) parts: Vec<Part>,
}

/// [R98.2-1] The anchor of a graph change for its entry (design 98): the places of the MIR
/// elements of its point (the projection of design 90 reads them), the places of the point
/// itself (for an implicit change, R98.3-1), and the mark "implicit".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Anchor {
    /// The side (0: old, 1: new) and the place (the tokens, or the macro call) of each MIR element
    /// of the point.
    pub elements: Vec<(usize, MirPlace)>,
    /// The side and the place of each point of the change.
    pub points: Vec<(usize, MirPlace)>,
    /// [R98.3-1] True for a point that the source has no node for: a check, the panic of a check,
    /// a point of the expansion of a macro of another crate (no tokens in the workspace).
    pub implicit: bool,
}

/// [R98.2-2] One graph change in the summary of an entry: the group (0: an added point, 1: a
/// removed point, 2: a changed point, 3: an added arrow, 4: a removed arrow), the word of the
/// kind (`branch`, `bounds check`, `changed condition`), and the short text of the point.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Part {
    pub group: u8,
    pub label: String,
    pub text: String,
}

/// A graph change for a unit test: its line, its anchor, and its parts (one line of `status
/// --verbose` for each part).
#[cfg(test)]
pub(crate) fn test_element(text: &str, anchor: Anchor, parts: Vec<Part>) -> Element {
    Element { places: Vec::new(), text: text.into(), arrows: Vec::new(), details: parts.iter().map(|_| text.to_string()).collect(), all: false, point: None, anchor, parts }
}

/// [R98.1-1] The places of the MIR elements of each point of each side (design 98): empty for a
/// point with no element in the table, and for a body with no table.
#[derive(Clone, Debug, Default)]
pub(crate) struct Links {
    places: [Vec<Vec<MirPlace>>; 2],
}

impl Links {
    fn of(&self, side: usize, point: usize) -> &[MirPlace] {
        self.places[side].get(point).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// [R98.3-1] True for a point that the source has no node for (design 98): a check (the compiler
/// adds it for an operation), the panic point of a check, and a point with no tokens in the
/// workspace in a macro call (the expansion of a macro of another crate).
fn implicit_point(graph: &FlowGraph, roles: &[crate::flowalign::Role], point: usize) -> bool {
    let found = &graph.points[point];
    found.kind == "check" || roles.get(point) == Some(&crate::flowalign::Role::CheckPanic) || (found.tokens.is_none() && found.call.is_some())
}

/// The anchor of one point of one side.
fn point_anchor(graph: &FlowGraph, roles: &[crate::flowalign::Role], links: &Links, side: usize, point: usize) -> Anchor {
    Anchor {
        elements: links.of(side, point).iter().map(|place| (side, place.clone())).collect(),
        points: graph.points[point].place().map(|place| (side, place.clone())).into_iter().collect(),
        implicit: implicit_point(graph, roles, point),
    }
}

/// The word of the kind of a point in the summary of an entry: `bounds check` for a check,
/// `panic` for the panic of a check, else the kind.
fn point_word(graph: &FlowGraph, point: usize) -> String {
    let found = &graph.points[point];
    match found.kind.as_str() {
        "check" => format!("{} check", found.text.split(':').next().unwrap_or("").trim()),
        // The entry of a body pairs with the entry of the other body (R98.1-1): only a body that
        // one tree has adds or removes an entry, and the summary names it as a body.
        "entry" => "body".to_string(),
        other => other.to_string(),
    }
}

/// [R98.2-2] The text of a point in the summary of an entry: the source text of a branch, a loop, a
/// construct, and the panic of a macro, and the short callee of a call. A check, the panic of a
/// check, the entry, and a return have their kind only.
fn summary_text(point: &FlowPoint) -> String {
    match point.kind.as_str() {
        "check" | "entry" | "return" => String::new(),
        "panic" if point.calls.is_empty() => String::new(),
        _ => point.short_text(),
    }
}

/// The noun of a changed point of one kind (design 60, R60.4-2).
fn changed_noun(kind: &str) -> String {
    match kind {
        "branch" => "condition".to_string(),
        "call" => "call target".to_string(),
        "panic" => "panic call".to_string(),
        "check" => "check".to_string(),
        "loop" => "loop".to_string(),
        other => format!("`{other}` point"),
    }
}

/// The line of one arrow: `+ arrow branch `x > 3` (src/lib.rs:6) → return (src/lib.rs:9) (true)`.
fn arrow_line(sign: &str, graph: &FlowGraph, side: usize, index: usize, lines: &mut Lines, short: bool) -> String {
    let (from, to, label) = &graph.arrows[index];
    let label = if label.is_empty() { String::new() } else { format!(" ({label})") };
    format!("{sign} arrow {} → {}{label}", named(lines, side, graph, *from, short), named(lines, side, graph, *to, short))
}

/// The element of one point with its arrows that are not shared. The text of the fact line has
/// the short labels; the lines of `status --verbose` have the full texts.
fn point_element(sign: &str, graph: &FlowGraph, side: usize, point: usize, arrows: Vec<usize>, lines: &mut Lines, anchor: Anchor) -> Element {
    let shown: Vec<String> = arrows.iter().map(|index| arrow_text(graph, &graph.arrows[*index].2, graph.arrows[*index].1, true)).collect();
    let head = format!("{sign} {}", named(lines, side, graph, point, true));
    let text = if shown.is_empty() { head.clone() } else { format!("{head}: {}", shown.join(", ")) };
    // The line of `status --verbose` has the callees of a construct or of the panic of a macro.
    let calls = &graph.points[point].calls;
    let callees = if calls.is_empty() { String::new() } else { format!("; callees {}", calls.iter().map(|text| format!("`{text}`")).collect::<Vec<_>>().join(", ")) };
    let mut details = vec![format!("{sign} {}{callees}", named(lines, side, graph, point, false))];
    details.extend(arrows.iter().map(|index| arrow_line(sign, graph, side, *index, lines, false)));
    let (group, arrow_group) = if side == 1 { (0, 3) } else { (1, 4) };
    let mut parts = vec![Part { group, label: point_word(graph, point), text: summary_text(&graph.points[point]) }];
    parts.extend(arrows.iter().map(|_| Part { group: arrow_group, label: "arrow".into(), text: String::new() }));
    Element { places: graph.points[point].place().cloned().map(|at| (side, at)).into_iter().collect(), text, arrows: arrows.into_iter().map(|index| (side, index)).collect(), details, all: false, point: Some((side, point)), anchor, parts }
}

/// The elements of a diff: each changed point and each arrow that is not shared.
fn elements(old: &FlowGraph, new: &FlowGraph, diff: &GraphDiff, lines: &mut Lines, links: &Links) -> Vec<Element> {
    let graphs = [old, new];
    let roles = [crate::flowalign::roles(old), crate::flowalign::roles(new)];
    let mut out = Vec::new();
    let place = |graph: &FlowGraph, point: usize| graph.points[point].place().cloned();
    for (side, list, sign) in [(1usize, &diff.added, "+"), (0usize, &diff.removed, "−")] {
        let graph = graphs[side];
        let changed_arrows = if side == 1 { &diff.added_arrows } else { &diff.removed_arrows };
        for &point in list {
            let arrows: Vec<usize> = changed_arrows.iter().copied().filter(|index| graph.arrows[*index].0 == point).collect();
            let anchor = point_anchor(graph, &roles[side], links, side, point);
            out.push(point_element(sign, graph, side, point, arrows, lines, anchor));
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
        let mut anchor = point_anchor(old, &roles[0], links, 0, a);
        let other = point_anchor(new, &roles[1], links, 1, b);
        anchor.elements.extend(other.elements);
        anchor.points.extend(other.points);
        anchor.implicit &= other.implicit;
        let words = if x.short_text() != y.short_text() { format!("`{}` → `{}`", x.short_text(), y.short_text()) } else { format!("`{}`", y.short_text()) };
        let parts = vec![Part { group: 2, label: format!("changed {}", changed_noun(&y.kind)), text: words }];
        out.push(Element { places, details: vec![detail], text, arrows: Vec::new(), all: false, point: None, anchor, parts });
    }
    let shown: BTreeSet<(usize, usize)> = out.iter().flat_map(|element| element.arrows.iter().copied()).collect();
    let paired: [BTreeSet<usize>; 2] = [diff.pairs.iter().map(|(a, _)| *a).collect(), diff.pairs.iter().map(|(_, b)| *b).collect()];
    let partner: [BTreeMap<usize, usize>; 2] = [diff.pairs.iter().copied().collect(), diff.pairs.iter().map(|(a, b)| (*b, *a)).collect()];
    // [R98.2-1] The anchor point of an arrow that is not shared: an end with no pair (the first
    // end first); else the new target of the same arrow on the other side (an arrow of the
    // partner of its first point with the same label, to a point with no pair); else the point
    // that selects the arrow (its first end), or its second end for an arrow of the entry.
    let arrow_anchor = |side: usize, index: usize| -> (usize, usize) {
        let (from, to, label) = &graphs[side].arrows[index];
        if !paired[side].contains(from) {
            return (side, *from);
        }
        if !paired[side].contains(to) {
            return (side, *to);
        }
        let other = 1 - side;
        let list = if other == 1 { &diff.added_arrows } else { &diff.removed_arrows };
        if let Some(start) = partner[side].get(from)
            && let Some(found) = list.iter().map(|at| &graphs[other].arrows[*at]).find(|(a, b, name)| a == start && name == label && !paired[other].contains(b))
        {
            return (other, found.1);
        }
        if graphs[side].points[*from].kind == "entry" { (side, *to) } else { (side, *from) }
    };
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
            let (anchor_side, anchor_point) = arrow_anchor(side, index);
            let anchor = point_anchor(graphs[anchor_side], &roles[anchor_side], links, anchor_side, anchor_point);
            let ends = if all { vec![*from, *to] } else { ends };
            let places = ends.into_iter().filter_map(|end| place(graph, end)).map(|at| (side, at)).collect();
            let parts = vec![Part { group: if side == 1 { 3 } else { 4 }, label: "arrow".into(), text: String::new() }];
            out.push(Element { places, details: vec![detail], text, arrows: vec![(side, index)], all, point: None, anchor, parts });
        }
    }
    out
}

/// The lines of a full graph on one side (a body that only one tree has).
fn whole(graph: &FlowGraph, side: usize, lines: &mut Lines) -> Vec<Element> {
    let sign = if side == 1 { "+" } else { "−" };
    let roles = crate::flowalign::roles(graph);
    let links = Links::default();
    (0..graph.points.len())
        .map(|point| {
            let arrows: Vec<usize> = (0..graph.arrows.len()).filter(|index| graph.arrows[*index].0 == point).collect();
            let anchor = point_anchor(graph, &roles, &links, side, point);
            point_element(sign, graph, side, point, arrows, lines, anchor)
        })
        .collect()
}

/// The most `control flow:` lines under one fact (design 60, C1).
pub const FACT_LINES: usize = 3;

/// [R98.2-1] The graph changes of one body (the first result, the worst configuration) for the
/// map of design 98: the step keeps them until the map is complete (`flowmap::apply`).
#[derive(Clone, Debug, Default)]
pub struct BodyChanges {
    /// The MIR name (the new name of a pair of nested bodies), and the name in the output.
    pub name: String,
    pub shown: String,
    pub place: String,
    pub item: Option<String>,
    pub old_item: Option<String>,
    /// True if an edit of the step is in the body.
    pub edit: bool,
    /// [R98.6-1] The level of the normal form where the MIR is equal: the equal MIR explains each
    /// graph change of the body.
    pub level: Option<String>,
    /// [R98.3-1] True if the body pair has an element table: a point with MIR elements and no
    /// owner is implicit.
    pub aligned: bool,
    /// The result word of the body: `graph changes`, `graph added`, …
    pub word: String,
    pub(crate) elements: Vec<Element>,
}

/// True if the item has no body node on each side that has it: a `const`, a `static`, an
/// associated constant, a type with a constant in its declaration. Its MIR bodies are parts of
/// its declaration (R71.2-1: the unit is the whole item; R61.15-1 before batch 11c).
fn no_body_node(item: &str, old_item: &str, sides: [&Workspace; 2]) -> bool {
    let found = [sides[0].items.get(old_item), sides[1].items.get(item)];
    found.iter().any(Option::is_some) && found.iter().flatten().all(|found| found.body.is_none())
}

/// True if an edit of the step is in the body of an item, on one side.
///
/// An item with no body node (a `const`, a `static`, an associated constant) has its MIR bodies
/// in its declaration: each edit of the item, except a doc, a comment, an attribute, and the
/// visibility, is an edit of each of its bodies (R71.2-1; R61.15-1 before batch 11c).
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
            // The declaration of an item with no body node holds its bodies (R71.2-1).
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
    /// [R98.6-1] The level of the normal form where the MIR of the body pair is equal.
    level: Option<String>,
    /// [R98.3-1] True if the body pair has an element table (the projection can find an owner).
    aligned: bool,
    /// [R98.1-1] True if the body has a line for its changed MIR (a body with no edit and the same
    /// graph): the line says "no edit in the body".
    kept: bool,
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
    /// The constants that the body reads with a changed value (design 64, part D).
    consts: Vec<(String, String, String)>,
    /// The statics with interior mutability that the body reads with a changed initial value
    /// (R73.2-1).
    statics: Vec<(String, String, String)>,
    /// R71.2-1: the graphs of the first result (its first configuration), for the moves of
    /// the points between the bodies of one item.
    first: Option<FirstGraphs>,
}

/// The graphs of the first result of a body (R71.2-1): two graphs and their diff, or the full
/// graph of a body that one tree has (with its side).
#[derive(Clone)]
enum FirstGraphs {
    Diff(FlowGraph, FlowGraph, GraphDiff),
    Whole(FlowGraph, usize),
}

impl FirstGraphs {
    /// The points with no pair: `(side, index)`. A whole graph has each point. The entry and the
    /// return of a body are the borders of the body: they never move.
    fn single(&self) -> Vec<(usize, usize)> {
        let (list, graphs): (Vec<(usize, usize)>, [&FlowGraph; 2]) = match self {
            FirstGraphs::Diff(old, new, diff) => (diff.added.iter().map(|index| (1, *index)).chain(diff.removed.iter().map(|index| (0, *index))).collect(), [old, new]),
            FirstGraphs::Whole(graph, side) => ((0..graph.points.len()).map(|index| (*side, index)).collect(), [graph, graph]),
        };
        list.into_iter().filter(|(side, index)| !matches!(graphs[*side].points[*index].kind.as_str(), "entry" | "return")).collect()
    }

    fn point(&self, side: usize, index: usize) -> &FlowPoint {
        match self {
            FirstGraphs::Diff(old, new, _) => &[old, new][side].points[index],
            FirstGraphs::Whole(graph, _) => &graph.points[index],
        }
    }
}

/// The key of a point that can move between two bodies of one item (R71.2-1): the kind, and
/// the kind of a check or a panic (`check bounds`), else the text of the point.
fn move_key(point: &FlowPoint) -> String {
    match point.kind.as_str() {
        "check" => format!("check {}", point.text.split(':').next().unwrap_or("")),
        _ => format!("{} {}", point.kind, point.text),
    }
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

/// [R98.1-2] The element tables of a body pair: each distinct table of its diffs, else its one
/// table.
fn body_tables(body: &crate::BodyComparison) -> Vec<std::sync::Arc<crate::MirTable>> {
    let Some(diff) = &body.elements else { return Vec::new() };
    if diff.graph_tables.is_empty() { diff.table.iter().cloned().collect() } else { diff.graph_tables.clone() }
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
/// the same arrows (R60.3-7). A coroutine graph with the mark (no MIR before the coroutine
/// transform, R76.2-1) has no points, so it never has the same shape.
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
    /// [R98.1-1] [R98.1-2] The element tables of the body pair (R97.2-1): one for each distinct
    /// text of its targets and its checked configurations. Empty: no table (a body that is not
    /// different, no element data, or a pair of nested bodies with two names).
    tables: Vec<std::sync::Arc<crate::MirTable>>,
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

/// The bodies of the step for the graph compare (R60.3-9 before batch 11d, now R72.1-1, part f).
/// A nested body (a closure, a coroutine, a constant block, a nested function: a body whose name
/// has the name of a body of the same item first) pairs by the one matcher, not by its number:
/// the identity is the smallest node pair of the tree compare that holds the node of the body;
/// then an equal source text and graph; then the best similarity of the source texts; the other
/// bodies are added or removed.
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
            out.push(Work { name: body.name.clone(), shown: body.name.clone(), place: body_place(body), status: body.status.clone(), item: items[at].clone(), graphs: body.graphs.clone(), tables: body_tables(body) });
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
        // R72.1-1 (part f): the nested bodies pair by the one matcher. The identity: a body with
        // no place on the two sides (a record with no graph) keeps its name: Semilla has no node
        // for it; else the smallest node pair of the tree compare that holds the node of the body
        // (the definition of the body in its item); an item with no edit in its body and no node
        // pair (a moved item), or with no body node (a `const`, a `static`: R71.2-1), is one
        // identity. Then an equal source text and an equal graph, then the best similarity of the
        // source texts, then the source order.
        let holder = |side: usize, found: &NestedSide| -> Option<usize> {
            let (start, end) = (found.node?, found.end?);
            pairs.iter().enumerate().filter(|(_, pair)| pair.ranges[side].0 <= start && end <= pair.ranges[side].1).max_by(|(_, a), (_, b)| a.ranges[side].0.cmp(&b.ranges[side].0).then(b.ranges[side].1.cmp(&a.ranges[side].1)).then(a.equal.cmp(&b.equal))).map(|(at, _)| at)
        };
        let no_node = match (&item, &old_item) {
            (Some(item), Some(old)) => no_body_node(item, old, sides),
            _ => false,
        };
        let one_identity = pairs.is_empty() && (!edit || no_node);
        let first_graph = |found: &NestedSide| -> Option<FlowGraph> {
            found.sides.iter().find_map(|(_, side)| match side {
                FlowSide::Graph(graph) => Some(graph.clone()),
                _ => None,
            })
        };
        let thing = |side: usize, found: &NestedSide| -> semilla_core::matcher::Thing {
            let identity = if found.place.is_none() {
                Some(format!("name\u{0}{}", bodies[found.body].name))
            } else if let Some(pair) = holder(side, found) {
                Some(format!("holder\u{0}{pair}"))
            } else {
                one_identity.then(|| "item".to_string())
            };
            let shape = first_graph(found).filter(|graph| !graph.coroutine).map(|graph| format!("{:?}", (graph.points.iter().map(|point| (point.kind.clone(), point.text.clone())).collect::<Vec<_>>(), graph.arrows)));
            let content = (!found.source.is_empty()).then(|| format!("{}\u{0}{:?}", found.source, shape));
            semilla_core::matcher::Thing { group: found.kind.clone(), identity, content }
        };
        let things: [Vec<semilla_core::matcher::Thing>; 2] = [lists[0].iter().map(|found| thing(0, found)).collect(), lists[1].iter().map(|found| thing(1, found)).collect()];
        let words = |text: &str| -> Vec<String> { text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|word| !word.is_empty()).map(str::to_string).collect() };
        let source_words: [Vec<Vec<String>>; 2] = [lists[0].iter().map(|found| words(&found.source)).collect(), lists[1].iter().map(|found| words(&found.source)).collect()];
        let similarity = |a: usize, b: usize| if lists[0][a].source.is_empty() || lists[1][b].source.is_empty() { 0.0 } else { semilla_core::matcher::token_similarity(&source_words[0][a], &source_words[1][b]) };
        let ladder = semilla_core::matcher::Ladder { similarity: &similarity, threshold: NESTED_THRESHOLD, in_order: None };
        let mut partner: [Vec<Option<usize>>; 2] = [vec![None; lists[0].len()], vec![None; lists[1].len()]];
        let mut weak: Vec<Option<semilla_core::matcher::Key>> = vec![None; lists[1].len()];
        for pair in semilla_core::matcher::pair(&things[0], &things[1], &ladder) {
            partner[0][pair.old] = Some(pair.new);
            partner[1][pair.new] = Some(pair.old);
            weak[pair.new] = pair.key.weak().then_some(pair.key);
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
                    found.push(GraphPair { config: config.clone(), same: false, old, new, consts: Vec::new(), statics: Vec::new() });
                }
            }
            found
        };
        for (b, found) in lists[1].iter().enumerate() {
            new_names.insert(bodies[found.body].name.clone());
            let old = partner[1][b].map(|a| &lists[0][a]);
            let (mut shown, place) = shown(lines, 1, found);
            // R72.1-3: a pair by a weak key says so.
            if let Some(key) = weak[b] {
                shown.push_str(&format!(" ({})", key.words()));
            }
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
            // [R98.1-1] The table of the MIR compare pairs the bodies by the name: a pair of two
            // names has no table.
            let tables = match old {
                Some(old) if old.body == found.body => body_tables(bodies[found.body]),
                _ => Vec::new(),
            };
            out.push(Work { name, shown, place, status, item: item.clone(), graphs: combine(old, Some(found)), tables });
        }
        for (a, found) in lists[0].iter().enumerate() {
            if partner[0][a].is_some() {
                continue;
            }
            unpaired_old.push(bodies[found.body].name.clone());
            let (shown, place) = shown(lines, 0, found);
            out.push(Work { name: bodies[found.body].name.clone(), shown, place, status: crate::BodyStatus::BeforeOnly, item: item.clone(), graphs: combine(Some(found), None), tables: Vec::new() });
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

/// The smallest similarity of a pair of two nested bodies by the similarity of their source texts
/// (Decided by Claude, design 72.4).
const NESTED_THRESHOLD: f64 = 0.5;

/// The text with the new name of each nested body (R72.1-1, part f): one pass, the longest name first,
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

/// [R98.1-1] The diff of the two graphs of a body (design 98), the places of the MIR elements of
/// each point, and the cause when the pairs do not come from the element table:
/// - with the element table of the body pair: the pairs of the alignment (`flowalign::align`);
/// - [R98.1-2] with no table (a body whose MIR is equal, a body pair with no element data): the
///   points pair by their index when the two graphs have one structure;
/// - else only the entries pair. The cause is in the summary of the body.
pub(crate) fn graph_diff(old: &FlowGraph, new: &FlowGraph, tables: &[std::sync::Arc<crate::MirTable>], status: &crate::BodyStatus) -> (GraphDiff, Links, Option<String>) {
    use crate::flowalign::{align, entries_only, identity, link, roles, Role, TableSide};
    let role = [roles(old), roles(new)];
    // [R98.1-2] Of two tables (two targets or two configurations with two texts), the table that
    // fits the two graphs: the most points of one terminator that find their terminator; a tie
    // takes the first table.
    let mut best: Option<(usize, [TableSide; 2], [crate::flowalign::Linked; 2], &crate::MirTable)> = None;
    for table in tables {
        let (Some(first), Some(second)) = (TableSide::new(&table.texts[0], &table.ranges[0], &table.same[0]), TableSide::new(&table.texts[1], &table.ranges[1], &table.same[1])) else { continue };
        let linked = [link(old, &first), link(new, &second)];
        let fit: usize = (0..2).map(|side| linked[side].terms.iter().zip(&role[side]).filter(|(terms, role)| matches!(role, Role::Single(_)) && !terms.is_empty()).count()).sum();
        if best.as_ref().is_none_or(|(known, ..)| fit > *known) {
            best = Some((fit, [first, second], linked, table));
        }
    }
    if let Some((_, sides, linked, table)) = best {
        let pairs = align([old, new], [&linked[0], &linked[1]], &table.pairs);
        let found = diff_pairs(old, new, &pairs);
        let places = [0usize, 1].map(|side| linked[side].elements.iter().map(|list| list.iter().filter_map(|at| crate::flowalign::place(&sides[side].ranges[*at]).cloned()).collect()).collect());
        return (found, Links { places }, None);
    }
    // With no table (an equal MIR, or a body pair with no element data): two graphs of one
    // structure (the same kinds in the same order, the arrows with the same ends) pair by the
    // index; the alignment of an equal text is the identity.
    if let Some(pairs) = identity(old, new) {
        return (diff_pairs(old, new, &pairs), Links::default(), None);
    }
    let cause = match status {
        crate::BodyStatus::Equal { level, .. } => format!("no element alignment: the MIR is equal at `{level}`, and the two graphs have two structures (R98.1-2)"),
        _ => "no element alignment: the body pair has no element table, and the two graphs have two structures (R98.1-2)".to_string(),
    };
    (diff_pairs(old, new, &entries_only(old, new)), Links::default(), Some(cause))
}

fn apply_graphs(input: &crate::StepInput, oracle: &dyn crate::BodyOracle, outcome: &mut crate::StepOutcome) {
    let (before, after) = (input.before, input.after);
    let index = crate::checks::BodyIndex::new(&outcome.bodies, after, before);
    let reverse: BTreeMap<&String, &String> = outcome.renames.items.iter().map(|(old, new)| (new, old)).collect();
    let mut map = crate::mirmap::MirMap::new(before, after, &outcome.edits);
    let mut lines = Lines::new(before, after);
    let mut results: Vec<BodyResult> = Vec::new();
    let works = works(outcome, &index, &mut map, &mut lines, [before, after], &reverse);
    for body in &works {
        let item = body.item.clone();
        let old_item = item.as_ref().map(|item| reverse.get(item).map(|old| (*old).clone()).unwrap_or_else(|| item.clone()));
        let edit = match (&item, &old_item) {
            (Some(item), Some(old)) => edit_in_body(&outcome.edits, item, old, [before, after]),
            _ => false,
        };
        let mut outcomes: Vec<Outcome> = Vec::new();
        // R71.2-1: the graphs of each configuration, for the first result.
        let mut firsts: BTreeMap<String, FirstGraphs> = BTreeMap::new();
        for pair in &body.graphs {
            let found = if pair.same {
                Outcome { word: "same graph".into(), summary: String::new(), elements: Vec::new(), configs: Vec::new(), rank: FlowResult::Same, chart: None }
            } else {
                match (&pair.old, &pair.new) {
                    (FlowSide::Graph(old), FlowSide::Graph(new)) if old.coroutine || new.coroutine => Outcome { word: "graph not shown: coroutine".into(), summary: "the MIR of the driver is the state machine of the coroutine, not the source".into(), elements: Vec::new(), configs: Vec::new(), rank: coroutine_rank(&body.status), chart: None },
                    (FlowSide::Graph(old), FlowSide::Graph(new)) => {
                        // [R98.1-1] The pairs of the points come from the MIR alignment.
                        let (found, links, cause) = graph_diff(old, new, &body.tables, &body.status);
                        let rank = found.result();
                        let word = match rank {
                            FlowResult::Same => "same graph",
                            FlowResult::SameStructure => "same structure",
                            FlowResult::Changes => "graph changes",
                        };
                        let chart = (rank == FlowResult::Changes).then(|| chart(old, new, &found, &mut lines)).flatten();
                        let elements = elements(old, new, &found, &mut lines, &links);
                        firsts.entry(pair.config.clone()).or_insert_with(|| FirstGraphs::Diff(old.clone(), new.clone(), found.clone()));
                        let mut summary = summary(old, new, &found);
                        if let Some(cause) = cause.filter(|_| rank != FlowResult::Same) {
                            summary.push_str(&format!("; {cause}"));
                        }
                        Outcome { word: word.into(), summary, elements, configs: Vec::new(), rank, chart }
                    }
                    (FlowSide::NoBody, FlowSide::Graph(graph)) | (FlowSide::Graph(graph), FlowSide::NoBody) => {
                        let side = if matches!(pair.old, FlowSide::NoBody) { 1 } else { 0 };
                        let word = if side == 1 { "graph added" } else { "graph removed" };
                        if graph.coroutine {
                            Outcome { word: format!("{word}; graph not shown: coroutine"), summary: String::new(), elements: Vec::new(), configs: Vec::new(), rank: FlowResult::Changes, chart: None }
                        } else {
                            firsts.entry(pair.config.clone()).or_insert_with(|| FirstGraphs::Whole(graph.clone(), side));
                            Outcome { word: word.into(), summary: format!("{}, {}", count(graph.points.len(), "point"), count(graph.arrows.len(), "arrow")), elements: whole(graph, side, &mut lines), configs: Vec::new(), rank: FlowResult::Changes, chart: whole_chart(graph, side, &mut lines) }
                        }
                    }
                    (FlowSide::NoBody, FlowSide::NoBody) => continue,
                    (FlowSide::NotCompared, _) | (_, FlowSide::NotCompared) => Outcome { word: "not compared".into(), summary: NOT_COMPARED.into(), elements: Vec::new(), configs: Vec::new(), rank: FlowResult::Changes, chart: None },
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
        // The constants that the body reads with a changed value (design 64, part D).
        let mut consts: Vec<(String, String, String)> = Vec::new();
        for pair in &body.graphs {
            for found in &pair.consts {
                if !consts.contains(found) {
                    consts.push(found.clone());
                }
            }
        }
        // R73.2-1: the statics with interior mutability that the body reads with a changed
        // initial value.
        let mut statics: Vec<(String, String, String)> = Vec::new();
        for pair in &body.graphs {
            for found in &pair.statics {
                if !statics.contains(found) {
                    statics.push(found.clone());
                }
            }
        }
        // A body with no edit and the same graph needs no line (design 60, B6), except a body
        // that reads a changed constant: its result can change (R64.5-1), or a static whose
        // initial value changes (R73.2-1).
        // [R98.1-1] The line of a body does not depend on the pairs of the graph compare (design
        // 98): a body whose MIR is not equal keeps its line and its check (the equal-MIR check of
        // design 84 and the coverage of the changed bodies read the line).
        let mir_changes = !matches!(&body.status, crate::BodyStatus::Equal { .. });
        let only_statics = !edit && consts.is_empty() && outcomes.iter().all(|known| known.rank == FlowResult::Same) && !mir_changes;
        if only_statics && statics.is_empty() {
            continue;
        }
        outcomes.sort_by(|a, b| b.rank.cmp(&a.rank).then(b.configs.len().cmp(&a.configs.len())));
        // The pairs for the equivalence check (design 61): two records with a graph that is not
        // a coroutine graph.
        let mut pairs = Vec::new();
        // A body whose only change is the initial value of a static that the check reads from
        // the world gets no check: the line names the static (R73.2-1).
        for pair in body.graphs.iter().filter(|_| !only_statics) {
            if let (FlowSide::Graph(old), FlowSide::Graph(new)) = (&pair.old, &pair.new)
                && !old.coroutine
                && !new.coroutine
                && let (Some(a), Some(b)) = (&old.key, &new.key)
            {
                pairs.push((pair.config.clone(), a.clone(), b.clone()));
            }
        }
        let equal = matches!(&body.status, crate::BodyStatus::Equal { level, .. } if level == "n0") && consts.is_empty();
        let first = outcomes[0].configs.first().and_then(|config| firsts.get(config)).cloned();
        let level = match &body.status {
            crate::BodyStatus::Equal { level, .. } => Some(level.clone()),
            _ => None,
        };
        // A body with no edit that has its line only for its changed MIR (no changed constant, no
        // changed static, the same graph).
        let kept = !edit && mir_changes && consts.is_empty() && statics.is_empty() && outcomes.iter().all(|known| known.rank == FlowResult::Same);
        results.push(BodyResult { level, aligned: !body.tables.is_empty(), kept, item: item.clone(), old_item: old_item.clone(), pairs, equal, name: body.name.clone(), shown: body.shown.clone(), place: body.place.clone(), edit, outcomes, consts, statics, first });
    }
    unit_moves(&mut results);
    let mut report = FlowReport::default();
    // R76.1-1: a count of lines or of results names the command of the step: the control flow
    // section of its full text has each line.
    let full_step = semilla_core::budget::step_text_command(outcome.report.step);
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
            line.push_str(&format!(" (in {}; {} more result(s) in other configurations: {full_step})", first.configs.join(", "), result.outcomes.len() - 1));
        }
        if !result.edit && (first.rank != FlowResult::Same || result.kept) {
            line.push_str("; no edit in the body");
        }
        if !result.edit && !result.consts.is_empty() {
            // [R66.11-4] Only two known values that differ are a changed constant. A value with a
            // part that the driver cannot compute (R66.10-1) is not known to change: the line
            // names the constant and the cause, with no unique word.
            let shown = semilla_core::text::without_unique;
            let (mut changed, mut unknown) = (Vec::new(), Vec::new());
            for (path, old, new) in &result.consts {
                let mut causes = semilla_core::text::unknown_causes(old);
                for cause in semilla_core::text::unknown_causes(new) {
                    if !causes.contains(&cause) {
                        causes.push(cause);
                    }
                }
                // One text for each constant: the values of each configuration have their own
                // unique words, and one cause.
                let (list, text) = if causes.is_empty() { (&mut changed, format!("`{path}` ({})", semilla_core::budget::value_pair(&shown(old), &shown(new)))) } else { (&mut unknown, format!("`{path}` ({})", causes.join(", "))) };
                if !list.contains(&text) {
                    list.push(text);
                }
            }
            if !changed.is_empty() {
                line.push_str(&format!("; reads a changed constant: {}", changed.join(", ")));
            }
            if !unknown.is_empty() {
                line.push_str(&format!("; {CANNOT_COMPARE}: {}", unknown.join(", ")));
            }
        }
        if !result.statics.is_empty() {
            let shown = semilla_core::text::without_unique;
            let list: Vec<String> = result.statics.iter().map(|(path, old, new)| format!("`{path}` ({})", semilla_core::budget::value_pair(&shown(old), &shown(new)))).collect();
            line.push_str(&format!("; {STATIC_READ}: {} (the check reads the static from the world)", list.join(", ")));
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
    // [R98.2-1] The graph changes of each body go to the entries of the map of design 98 after
    // the map is complete (`flowmap::apply`): the step keeps them here.
    outcome.flow_changes = results
        .iter()
        .map(|result| BodyChanges {
            name: result.name.clone(),
            shown: result.shown.clone(),
            place: result.place.clone(),
            item: result.item.clone(),
            old_item: result.old_item.clone(),
            edit: result.edit,
            level: result.level.clone(),
            aligned: result.aligned,
            word: result.outcomes[0].word.clone(),
            elements: result.outcomes[0].elements.clone(),
        })
        .collect();
    // R68.6-2: a body with no edit that reads a changed constant or a changed static (its
    // constant values, R64.5-1, R68.6-1) is an affected body of the value fact of that item, with
    // the link "through". The MIR text of a reader of a static names the static, not its value,
    // so the compare of the texts does not find it. The rule only adds a body to a list.
    for result in results.iter().filter(|result| !result.edit) {
        for (path, _, _) in result.consts.iter().chain(&result.statics) {
            for fact in outcome.report.facts.iter_mut().filter(|fact| fact.part == "value" && fact.at.as_ref().is_some_and(|at| at.item == *path)) {
                if !fact.affected.contains(&result.name) {
                    fact.affected.push(result.name.clone());
                    fact.affected.sort();
                }
                let list = fact.through.entry(path.clone()).or_default();
                if !list.contains(&result.name) {
                    list.push(result.name.clone());
                    list.sort();
                }
            }
        }
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

/// The words of the line of a body that reads a static with interior mutability whose initial
/// value changes (R73.2-1).
pub const STATIC_READ: &str = "reads a static whose initial value changes";

/// R71.2-1: the unit is the whole item. The bodies of one item (the body of the item and each
/// body nested in it: a closure, a constant block, a coroutine) are one unit. A point that one
/// body adds and a different body of the unit removes, with one key (`move_key`: the kind, the
/// kind of a check or a panic, else the text), is a move, not a new point: the counts of the
/// graph line of each body leave it out, the line names the moves ("moved in from `NAME`: 1
/// check, 1 panic"), and the line of the point below a fact says where it moves. Each point
/// keeps its line: the rule only changes the counts and adds the words of the move. Before
/// batch 11c, R68.7-1 gave only the counts of the checks and the panics of a body with its
/// closures.
fn unit_moves(results: &mut [BodyResult]) {
    // The units: the results of one item.
    let mut units: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, result) in results.iter().enumerate() {
        if let (Some(item), Some(_)) = (&result.item, &result.first) {
            units.entry(item.clone()).or_default().push(index);
        }
    }
    for members in units.values().filter(|members| members.len() > 1) {
        // The points with no pair of each body: (body, side, point, key).
        let mut open: Vec<(usize, usize, usize, String)> = Vec::new();
        for &body in members {
            let first = results[body].first.as_ref().expect("a member has graphs");
            for (side, point) in first.single() {
                open.push((body, side, point, move_key(first.point(side, point))));
            }
        }
        // The pairs: an added point of one body and a removed point of a different body, with
        // one key, in order.
        let mut used = vec![false; open.len()];
        let mut moves: Vec<(usize, usize)> = Vec::new();
        for a in 0..open.len() {
            if used[a] || open[a].1 != 1 {
                continue;
            }
            if let Some(b) = (0..open.len()).find(|b| !used[*b] && open[*b].1 == 0 && open[*b].0 != open[a].0 && open[*b].3 == open[a].3) {
                used[a] = true;
                used[b] = true;
                moves.push((a, b));
            }
        }
        if moves.is_empty() {
            continue;
        }
        // The words of each body: the kinds that move in or out, and the other body.
        for &body in members {
            let mut moved_in: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
            let mut moved_out: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
            let mut gone: Vec<(usize, usize)> = Vec::new();
            for &(a, b) in &moves {
                let (to, from) = (&open[a], &open[b]);
                for (point, other, list) in [(to, from, &mut moved_in), (from, to, &mut moved_out)] {
                    if point.0 != body {
                        continue;
                    }
                    let kind = results[body].first.as_ref().expect("graphs").point(point.1, point.2).kind.clone();
                    let entry = list.entry(kind).or_default();
                    entry.0 += 1;
                    let name = results[other.0].name.clone();
                    if !entry.1.contains(&name) {
                        entry.1.push(name);
                    }
                    gone.push((point.1, point.2));
                }
            }
            if gone.is_empty() {
                continue;
            }
            let words = |list: &BTreeMap<String, (usize, Vec<String>)>, word: &str, to: &str| -> Option<String> {
                if list.is_empty() {
                    return None;
                }
                let counts: Vec<String> = list.iter().map(|(kind, (count, _))| format!("{count} {}", kinds(kind, *count))).collect();
                let mut names: Vec<String> = list.values().flat_map(|(_, names)| names.iter().map(|name| format!("`{name}`"))).collect();
                names.dedup();
                Some(format!("moved {word} {to} {}: {}", names.join(", "), counts.join(", ")))
            };
            let text = [words(&moved_in, "in", "from"), words(&moved_out, "out", "to")].into_iter().flatten().collect::<Vec<_>>().join("; ");
            let result = &mut results[body];
            let first = result.first.clone().expect("graphs");
            let outcome = &mut result.outcomes[0];
            // The counts with no moved point.
            if let FirstGraphs::Diff(old, new, diff) = &first {
                let mut kept = diff.clone();
                kept.added.retain(|index| !gone.contains(&(1, *index)));
                kept.removed.retain(|index| !gone.contains(&(0, *index)));
                let rest = summary(old, new, &kept);
                outcome.summary = if rest.is_empty() { text.clone() } else { format!("{rest}; {text}") };
            } else {
                outcome.summary = format!("{}; {text}", outcome.summary);
            }
        }
        // The line of each moved point names the other body.
        let names: Vec<String> = results.iter().map(|result| result.name.clone()).collect();
        for &(a, b) in &moves {
            for (here, there, word) in [(&open[a], &open[b], "from"), (&open[b], &open[a], "to")] {
                let outcome = &mut results[here.0].outcomes[0];
                // "moved check `bounds: v[k]` (src/lib.rs:2) from `shop::pick::{closure#0}`": the
                // word `moved` is in place of the sign `+` or `−`.
                let mark = format!(" {word} `{}`", names[there.0]);
                let moved = |text: &str| -> String {
                    let text = text.strip_prefix("+ ").or_else(|| text.strip_prefix("− ")).map(|rest| format!("moved {rest}")).unwrap_or_else(|| text.to_string());
                    match text.split_once("): ") {
                        Some((head, arrows)) => format!("{head}){mark}: {arrows}"),
                        None => format!("{text}{mark}"),
                    }
                };
                for element in outcome.elements.iter_mut().filter(|element| element.point == Some((here.1, here.2))) {
                    if element.text.starts_with("moved ") {
                        continue;
                    }
                    element.text = moved(&element.text);
                    if let Some(detail) = element.details.first_mut() {
                        *detail = moved(detail);
                    }
                }
            }
        }
    }
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

    /// The pairs of the points of each index in a list (the pairs that an alignment gives).
    fn diff(old: &FlowGraph, new: &FlowGraph, pairs: &[(usize, usize)]) -> GraphDiff {
        diff_pairs(old, new, pairs)
    }

    fn same(count: usize) -> Vec<(usize, usize)> {
        (0..count).map(|index| (index, index)).collect()
    }

    // rule+ R60.3-3
    // rule- R60.3-3
    #[test]
    fn an_arrow_is_shared_only_if_its_two_ends_pair_and_its_labels_are_equal() {
        let old = graph(vec![point("entry", "", 0), point("branch", "x > 3", 5), point("return", "", 30)], &[(0, 1, ""), (1, 2, "true"), (1, 2, "false")]);
        let found = diff(&old, &old.clone(), &same(3));
        assert!(found.added_arrows.is_empty() && found.removed_arrows.is_empty());
        assert_eq!(found.result(), FlowResult::Same);
        // The negative case: the same two ends with a new label.
        let new = graph(old.points.clone(), &[(0, 1, ""), (1, 2, "true"), (1, 2, "otherwise")]);
        let found = diff(&old, &new, &same(3));
        assert_eq!((found.removed_arrows.clone(), found.added_arrows.clone()), (vec![2], vec![2]));
        assert_eq!(found.result(), FlowResult::Changes);
    }

    // rule+ R60.3-4
    // rule- R60.3-4
    #[test]
    fn the_result_of_a_body_is_same_graph_same_structure_or_graph_changes() {
        let old = graph(vec![point("entry", "", 0), point("branch", "x > 3", 5), point("return", "", 30)], &[(0, 1, ""), (1, 2, "true"), (1, 2, "false")]);
        let found = diff(&old, &old.clone(), &same(3));
        assert_eq!(found.result(), FlowResult::Same);
        let mut changed = old.clone();
        changed.points[1].text = "x >= 3".into();
        let found = diff(&old, &changed, &same(3));
        assert_eq!(found.result(), FlowResult::SameStructure);
        assert_eq!(summary(&old, &changed, &found), "1 changed condition");
        // The negative case: a point that the alignment does not pair is a removed point and an
        // added point, so the result is not `same structure`.
        let found = diff(&old, &changed, &[(0, 0), (2, 2)]);
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
        let found = diff(&old, &new, &[(0, 0), (1, 2), (2, 3)]);
        let texts: Vec<String> = elements(&old, &new, &found, &mut lines, &Links::default()).into_iter().map(|element| element.text).collect();
        assert_eq!(texts, ["+ branch `qty == 0` (no place): true → return, false → call `price`", "+ arrow entry (no place) → branch `qty == 0` (no place)", "− arrow entry (no place) → call `price` (no place)"]);
        // The negative case: a shared arrow and a pair with an equal text have no line.
        let found = diff(&old, &old.clone(), &same(3));
        assert!(elements(&old, &old.clone(), &found, &mut lines, &Links::default()).is_empty());
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
            let found = diff(&old, &new, &same(3));
            elements(&old, &new, &found, lines, &Links::default()).into_iter().map(|element| (element.text, element.details)).next().unwrap()
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
        let found = diff(&old, &new, &same(3));
        assert_eq!(found.result(), FlowResult::SameStructure);
        assert!(!same_shape(&old, &new));
        let ws = semilla_syntax::Workspace::default();
        let texts: Vec<String> = elements(&old, &new, &found, &mut Lines::new(&ws, &ws), &Links::default()).into_iter().map(|element| element.text).collect();
        assert_eq!(texts, ["~ for `make()` (no place, no place): callees −`<Food as Iterator>::next` +`<Fuel as Iterator>::next`"]);
        // The negative case: equal callees keep the graph.
        new.points[1].calls = old.points[1].calls.clone();
        assert!(same_shape(&old, &new));
        assert_eq!(diff(&old, &new, &same(3)).result(), FlowResult::Same);
    }

    // rule+ R72.1-1
    // rule- R72.1-1
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
