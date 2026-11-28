//! [R98.1-1] The pair of the points of two graphs through the MIR alignment (design 98, V1).
//!
//! The graph diff of a body is the MIR alignment of design 90, seen as points and arrows. The
//! driver gives a point and a MIR element the same two ranges (the tokens and the outermost
//! macro call, design 40.1), so each point finds its terminator elements in the element table
//! of the body pair (R97.2-1):
//!
//! - a point of one terminator (`branch`, `call`, `check`, `return`, `yield`, the direct call of
//!   a panic function) takes the terminator of its class with its two ranges; of two such
//!   terminators, a call takes the terminator whose callee has the last name of the text of the
//!   point, then the first one in the order of the text;
//! - a point of many terminators (a desugared construct, the panic of a macro, R60.2-2 and
//!   R60.2-12) takes each terminator inside its place that no other point has (the smallest such
//!   point first);
//! - the entry, a loop point, and the panic point of a check have no terminator of their own.
//!
//! Two points pair when the alignment pairs their terminators: for each point, the point of the
//! other side with the most pairs of terminators, when the choice is unique on the two sides and
//! the kinds are equal. The entries pair; the panic point of a check pairs with the panic point
//! of the partner of its check; a loop point pairs with the loop point whose target is the
//! partner of its target. No other rule pairs two points (R60.3-1 and R60.3-2 are gone): a
//! point whose terminators the alignment does not pair is an added or a removed point.

use std::collections::{BTreeMap, BTreeSet};

use crate::flow::FlowGraph;
use crate::MirPlace;

/// The class of a terminator element of the canonical text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    Switch,
    Call,
    Assert,
    Return,
}

/// The class of a terminator text. `None`: a terminator that is never a point: a `goto`, a
/// `drop`, and a terminator with no target (an `unreachable`, a `resume`, an `abort`).
pub fn term_class(text: &str) -> Option<Term> {
    let text = text.trim().trim_end_matches(';');
    if text.starts_with("switchInt(") {
        Some(Term::Switch)
    } else if text.starts_with("assert(") {
        Some(Term::Assert)
    } else if text == "return" {
        Some(Term::Return)
    } else if text.starts_with("goto ") || text.starts_with("drop(") {
        None
    } else if text.contains(" -> ") {
        // A call: its arguments, then its targets.
        Some(Term::Call)
    } else {
        None
    }
}

/// The last name of a path: the last segment that is not a list of generic arguments, with no
/// generic arguments and no mark (`core::slice::<impl [T]>::iter::<u32>` gives `iter`,
/// `<u32 as std::ops::AddAssign<&u32>>::add_assign` gives `add_assign`).
pub fn last_name(path: &str) -> String {
    let path = path.split(" (").next().unwrap_or(path).trim();
    let mut segments: Vec<&str> = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    let bytes = path.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            b'>' if at > 0 && bytes[at - 1] == b'-' => {}
            b'>' | b')' | b']' | b'}' => depth -= 1,
            b':' if depth == 0 && bytes.get(at + 1) == Some(&b':') => {
                segments.push(&path[start..at]);
                start = at + 2;
                at += 1;
            }
            _ => {}
        }
        at += 1;
    }
    segments.push(&path[start..]);
    let last = segments.iter().rev().find(|segment| !segment.starts_with('<') && !segment.is_empty()).copied().unwrap_or("");
    last.split('<').next().unwrap_or(last).to_string()
}

/// The last name of the callee of a call element: the path in the braces before the arguments
/// (`{std::vec::Vec::<u32>::push}(…) -> …` gives `push`). `None`: a call through a local.
pub fn call_name(text: &str) -> Option<String> {
    let end = text.rfind(") -> ")?;
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut open = None;
    for at in (0..=end).rev() {
        match bytes[at] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(at);
                    break;
                }
            }
            _ => {}
        }
    }
    let head = text[..open?].trim_end();
    let inner = head.strip_suffix('}')?;
    let bytes = inner.as_bytes();
    let mut depth = 0i32;
    for at in (0..bytes.len()).rev() {
        match bytes[at] {
            b'}' => depth += 1,
            b'{' if depth == 0 => return Some(last_name(&inner[at + 1..])),
            b'{' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// How a point finds its terminators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Entry,
    Loop,
    /// The `panic` point of a check (the target of its arrow `fail`): no terminator.
    CheckPanic,
    /// A point of one terminator of the class.
    Single(Term),
    /// A point of many terminators: a desugared construct, the panic of a macro.
    Group,
}

/// The role of each point of a graph.
pub fn roles(graph: &FlowGraph) -> Vec<Role> {
    let fails: BTreeSet<usize> = graph.arrows.iter().filter(|(from, to, label)| label == "fail" && graph.points.get(*from).is_some_and(|point| point.kind == "check") && graph.points.get(*to).is_some_and(|point| point.kind == "panic")).map(|(_, to, _)| *to).collect();
    graph
        .points
        .iter()
        .enumerate()
        .map(|(index, point)| match point.kind.as_str() {
            "entry" => Role::Entry,
            "loop" => Role::Loop,
            "panic" if fails.contains(&index) => Role::CheckPanic,
            "panic" if !point.calls.is_empty() => Role::Group,
            "panic" | "call" => Role::Single(Term::Call),
            "branch" => Role::Single(Term::Switch),
            "check" => Role::Single(Term::Assert),
            "return" | "yield" => Role::Single(Term::Return),
            _ => Role::Group,
        })
        .collect()
}

/// One side of a body pair in the element table: the text of each element and the terminator of
/// each block.
pub struct TableSide<'a> {
    /// The text of each element, in the order of the text.
    pub texts: Vec<&'a str>,
    /// The terminator elements: the index and the class.
    pub terms: Vec<(usize, Term)>,
    /// True for each element that is the last element of its block (a terminator).
    pub last: Vec<bool>,
    /// The two ranges of each element.
    pub ranges: &'a [(Option<MirPlace>, Option<MirPlace>)],
    /// True for each element with an equal partner.
    pub same: &'a [bool],
}

impl<'a> TableSide<'a> {
    /// The side of a table. `None`: the text and the ranges do not have the same count of
    /// elements (the table does not fit).
    pub fn new(text: &'a str, ranges: &'a [(Option<MirPlace>, Option<MirPlace>)], same: &'a [bool]) -> Option<TableSide<'a>> {
        let mut texts: Vec<&str> = Vec::new();
        let mut blocks: Vec<usize> = Vec::new();
        let mut block = 0usize;
        for line in text.lines() {
            if let Some(element) = line.strip_prefix("        ") {
                texts.push(element.strip_suffix(';').unwrap_or(element));
                blocks.push(block);
            } else if line.starts_with("    bb") {
                block += 1;
            }
        }
        if texts.len() != ranges.len() {
            return None;
        }
        let mut terms = Vec::new();
        let mut last = vec![false; texts.len()];
        for (index, current) in blocks.iter().enumerate() {
            if blocks.get(index + 1) != Some(current) {
                last[index] = true;
                if let Some(class) = term_class(texts[index]) {
                    terms.push((index, class));
                }
            }
        }
        Some(TableSide { texts, terms, last, ranges, same })
    }
}

/// [R98.1-1] The elements of each point of one side.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Linked {
    /// The terminator elements of each point.
    pub terms: Vec<Vec<usize>>,
    /// The elements of each point: its terminators, and each element with the two ranges of a
    /// terminator of the point (the statements that compute its value: `_3 = Gt(_1, const 3)`
    /// for `switchInt(_3)`).
    pub elements: Vec<Vec<usize>>,
}

/// The place of two ranges by the rule of [`crate::flow::FlowPoint::place`].
pub fn place(ranges: &(Option<MirPlace>, Option<MirPlace>)) -> Option<&MirPlace> {
    match ranges {
        (Some(tokens), Some(call)) if tokens.0 == call.0 && call.1 <= tokens.1 && tokens.2 <= call.2 => Some(tokens),
        (_, Some(call)) => Some(call),
        (tokens, None) => tokens.as_ref(),
    }
}

fn inside(inner: &MirPlace, outer: &MirPlace) -> bool {
    inner.0 == outer.0 && outer.1 <= inner.1 && inner.2 <= outer.2
}

/// [R98.1-1] The terminators and the elements of each point of a graph on one side.
pub fn link(graph: &FlowGraph, side: &TableSide) -> Linked {
    type Key = (Option<MirPlace>, Option<MirPlace>);
    let roles = roles(graph);
    let mut taken: BTreeSet<usize> = BTreeSet::new();
    let mut out = Linked { terms: vec![Vec::new(); graph.points.len()], elements: vec![Vec::new(); graph.points.len()] };
    // The terminators of each class and each pair of ranges, in the order of the text.
    let mut by_key: BTreeMap<(&Key, u8), Vec<usize>> = BTreeMap::new();
    for (at, class) in &side.terms {
        by_key.entry((&side.ranges[*at], *class as u8)).or_default().push(*at);
    }
    // The points of one terminator: the terminator of the class with the two ranges.
    for (index, point) in graph.points.iter().enumerate() {
        let Role::Single(class) = roles[index] else { continue };
        let key: Key = (point.tokens.clone(), point.call.clone());
        if key == (None, None) {
            continue;
        }
        let Some(list) = by_key.get(&(&key, class as u8)) else { continue };
        let found: Vec<usize> = list.iter().copied().filter(|at| !taken.contains(at)).collect();
        let name = (class == Term::Call && point.mark.is_none()).then(|| last_name(&point.text));
        let chosen = match &name {
            Some(name) if found.len() > 1 => found.iter().copied().find(|at| call_name(side.texts[*at]).as_deref() == Some(name.as_str())).or(found.first().copied()),
            _ => found.first().copied(),
        };
        if let Some(at) = chosen {
            taken.insert(at);
            out.terms[index].push(at);
        }
    }
    // The points of many terminators: each terminator inside the place, the smallest place
    // first.
    let mut groups: Vec<(usize, MirPlace)> = graph.points.iter().enumerate().filter(|(index, _)| roles[*index] == Role::Group).filter_map(|(index, point)| point.place().map(|place| (index, place.clone()))).collect();
    groups.sort_by_key(|(index, place)| (place.2.saturating_sub(place.1), *index));
    for (index, area) in &groups {
        for (at, _) in &side.terms {
            if !taken.contains(at) && place(&side.ranges[*at]).is_some_and(|found| inside(found, area)) {
                taken.insert(*at);
                out.terms[*index].push(*at);
            }
        }
    }
    // The elements of each point: its terminators, and each statement with the ranges of one.
    let mut statements: BTreeMap<&Key, Vec<usize>> = BTreeMap::new();
    for at in (0..side.ranges.len()).filter(|at| !side.last[*at]) {
        statements.entry(&side.ranges[at]).or_default().push(at);
    }
    for (index, terms) in out.terms.iter().enumerate() {
        let mut found: Vec<usize> = terms.clone();
        for at in terms {
            found.extend(statements.get(&side.ranges[*at]).into_iter().flatten().copied());
        }
        found.sort_unstable();
        found.dedup();
        out.elements[index] = found;
    }
    out
}

/// The target of the arrow of a loop point (no label), if it is another point.
fn loop_target(graph: &FlowGraph, point: usize) -> Option<usize> {
    graph.arrows.iter().find(|(from, to, label)| *from == point && label.is_empty() && *to != point).map(|(_, to, _)| *to)
}

/// The panic point of each check: the target of its arrow `fail`.
fn check_panic(graph: &FlowGraph, check: usize) -> Option<usize> {
    graph.arrows.iter().find(|(from, _, label)| *from == check && label == "fail").map(|(_, to, _)| *to)
}

/// [R98.1-1] The point pairs of two graphs through the pairs of the alignment (old element, new
/// element). The kinds of a pair are equal.
pub fn align(graphs: [&FlowGraph; 2], links: [&Linked; 2], pairs: &[(usize, usize, bool)]) -> Vec<(usize, usize)> {
    let owners: [BTreeMap<usize, usize>; 2] = [0, 1].map(|side| links[side].terms.iter().enumerate().flat_map(|(point, terms)| terms.iter().map(move |at| (*at, point))).collect());
    let mut votes: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (old, new, _) in pairs {
        if let (Some(a), Some(b)) = (owners[0].get(old), owners[1].get(new)) {
            *votes.entry((*a, *b)).or_default() += 1;
        }
    }
    // The best partner of each point on each side: the most votes, and `None` for a tie.
    let mut best: [BTreeMap<usize, (usize, Option<usize>)>; 2] = [BTreeMap::new(), BTreeMap::new()];
    for ((a, b), count) in &votes {
        for (side, point, other) in [(0usize, *a, *b), (1usize, *b, *a)] {
            let entry = best[side].entry(point).or_insert((0, None));
            if *count > entry.0 {
                *entry = (*count, Some(other));
            } else if *count == entry.0 {
                entry.1 = None;
            }
        }
    }
    let mut partner: [Vec<Option<usize>>; 2] = [vec![None; graphs[0].points.len()], vec![None; graphs[1].points.len()]];
    for (a, (_, b)) in &best[0] {
        let Some(b) = b else { continue };
        if best[1].get(b).and_then(|(_, back)| *back) == Some(*a) && graphs[0].points[*a].kind == graphs[1].points[*b].kind {
            partner[0][*a] = Some(*b);
            partner[1][*b] = Some(*a);
        }
    }
    derived(graphs, &mut partner);
    partner[0].iter().enumerate().filter_map(|(a, b)| b.map(|b| (a, b))).collect()
}

/// The pairs of the points with no terminator of their own: the entries, the panic point of a
/// pair of checks, and the loop points whose targets pair (to a fixed point).
fn derived(graphs: [&FlowGraph; 2], partner: &mut [Vec<Option<usize>>; 2]) {
    let set = |partner: &mut [Vec<Option<usize>>; 2], a: usize, b: usize| {
        if partner[0][a].is_none() && partner[1][b].is_none() && graphs[0].points[a].kind == graphs[1].points[b].kind {
            partner[0][a] = Some(b);
            partner[1][b] = Some(a);
        }
    };
    let entry = |graph: &FlowGraph| graph.points.iter().position(|point| point.kind == "entry");
    if let (Some(a), Some(b)) = (entry(graphs[0]), entry(graphs[1])) {
        set(partner, a, b);
    }
    for a in 0..graphs[0].points.len() {
        if graphs[0].points[a].kind != "check" {
            continue;
        }
        if let Some(b) = partner[0][a]
            && let (Some(x), Some(y)) = (check_panic(graphs[0], a), check_panic(graphs[1], b))
        {
            set(partner, x, y);
        }
    }
    loop {
        let mut changed = false;
        for a in 0..graphs[0].points.len() {
            if graphs[0].points[a].kind != "loop" || partner[0][a].is_some() {
                continue;
            }
            let Some(target) = loop_target(graphs[0], a).and_then(|target| partner[0][target]) else { continue };
            let found = (0..graphs[1].points.len()).find(|b| graphs[1].points[*b].kind == "loop" && partner[1][*b].is_none() && loop_target(graphs[1], *b) == Some(target));
            if let Some(b) = found {
                set(partner, a, b);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// [R98.1-2] The pairs of the points of two graphs of a body whose MIR text is equal: the
/// alignment of an equal text is the identity, so the point of each index pairs with the point
/// of the same index, when the two graphs have the same kinds in the same order and arrows with
/// the same ends. `None`: the graphs differ in their structure.
pub fn identity(old: &FlowGraph, new: &FlowGraph) -> Option<Vec<(usize, usize)>> {
    // The labels can change with an equal MIR (a renamed variant): a changed label is an arrow
    // that is not shared (R60.3-3).
    let same = old.points.len() == new.points.len() && old.arrows.len() == new.arrows.len() && old.arrows.iter().zip(&new.arrows).all(|(a, b)| (a.0, a.1) == (b.0, b.1)) && old.points.iter().zip(&new.points).all(|(a, b)| a.kind == b.kind);
    same.then(|| (0..old.points.len()).map(|index| (index, index)).collect())
}

/// [R98.1-2] The pairs with no alignment (no element table): the two entries only, and the
/// points that follow them.
pub fn entries_only(old: &FlowGraph, new: &FlowGraph) -> Vec<(usize, usize)> {
    let mut partner: [Vec<Option<usize>>; 2] = [vec![None; old.points.len()], vec![None; new.points.len()]];
    derived([old, new], &mut partner);
    partner[0].iter().enumerate().filter_map(|(a, b)| b.map(|b| (a, b))).collect()
}
