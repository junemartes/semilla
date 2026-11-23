//! The control flow graph of a body (design 60, part A).
//!
//! The graph comes from the MIR that the driver reads at N0: after the drop elaboration, before
//! the optimization passes. A point is a place where the control flow can choose, leave, or go
//! out and back:
//!
//! - `entry`: one for each body;
//! - `branch`: a `switchInt`. Its text is the source text of the condition or of the scrutinee;
//! - `call`: its text is the callee that the MIR calls, with each generic argument (the impl
//!   item of a resolved trait method; R60.2-11). A call of a closure, through a function
//!   pointer, or through `dyn` has the text of the operand and a mark (`closure`, `pointer`,
//!   `dyn`);
//! - `return`; `panic` (a call that never returns); `check` (an `Assert` terminator; its arrow
//!   `fail` goes to a `panic` point of its own);
//! - `loop`: the head of a loop (the target of a back edge).
//!
//! The terminators of one desugaring expansion (`?`, `for`, `.await`, each kind that the
//! compiler gives) are one point with the name of the construct (`try`, `for`). Its arrows get
//! the role of the arm of the desugared `match` in the HIR: an arm whose body is `return` is
//! `exit`, an arm whose body is `break` is `done`; each other arm is `next` for a loop head and
//! `continue` else. A construct point has the callees of its parts (`x`).
//!
//! The points of the expansion of one macro call from outside the workspace that lead only to a
//! panic of the same expansion are one `panic` point with the text of the macro call
//! (`panic!("too big")`, `assert!(x < 9)`; R60.2-12).
//!
//! An arrow goes from a point to the next point along the normal MIR edges. A chain of blocks
//! with no point is not in the graph. The unwind arrows, the cleanup blocks, the `drop`
//! terminators, and the arrows to `unreachable` are not in the graph; the record has their
//! counts. The record has no block number, no local number, and no hash, so two runs give the
//! same graph.

use std::collections::{BTreeMap, HashMap};

use rustc_public::mir::{AssertMessage, Body, Operand, Rvalue, StatementKind, TerminatorKind, UnwindAction};
use rustc_public::CrateDef;
use rustc_public::ty::{AdtKind, GenericArgKind, IntTy, RigidTy, TyKind};
use rustc_span::{ExpnKind, Span};

/// The graph of one body.
#[derive(serde::Serialize, Default)]
pub struct Graph {
    /// The points, in the order of a walk from the entry.
    pub p: Vec<Point>,
    /// The arrows: the index of the first point, the index of the second point, the label.
    pub a: Vec<(u32, u32, String)>,
    /// The files of the places (relative to the workspace root).
    pub f: Vec<String>,
    /// The counts of the parts that are not in the graph.
    pub n: Counts,
    /// True for the body of a coroutine (an `async` body): the MIR is the state machine after
    /// the coroutine transform, not the source. The graph has no point then.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub co: bool,
}

/// One point of the graph.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct Point {
    /// The kind: `entry`, `branch`, `call`, `return`, `panic`, `check`, `loop`, or the name of a
    /// construct (`try`, `for`, `await`, …).
    pub k: String,
    /// The text (none for an empty text).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub t: String,
    /// The mark of a call that the MIR does not resolve: `closure`, `pointer`, `dyn`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub m: Option<&'static str>,
    /// The range of the tokens: `[file, start, end]` (an index into `f`), as in `spans.rs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<[i64; 3]>,
    /// The range of the outermost macro call, for a point from a macro expansion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<[i64; 3]>,
    /// The callees of the parts of a point that holds more than one terminator (a construct, a
    /// panic of a macro): each text one time, in order (design 60, R60.2-11).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub x: Vec<String>,
    /// The value key of a check or a branch (design 61, A1): the operation and each operand as
    /// a value (a constant, or a parameter that the body never writes). Two points with equal
    /// keys do the same operation on the same values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<String>,
    /// The span of the terminator of the point (not in the record).
    #[serde(skip)]
    pub s: Option<Span>,
}

/// The counts of the parts that are not in the graph (design 60, A4).
#[derive(serde::Serialize, Default)]
pub struct Counts {
    /// The unwind arrows of the points and the drops that the walk reaches.
    #[serde(skip_serializing_if = "is_zero")]
    pub unwind: usize,
    /// The blocks that the walk from the entry does not reach (the cleanup blocks).
    #[serde(skip_serializing_if = "is_zero")]
    pub cleanup: usize,
    /// The `drop` terminators that the walk reaches.
    #[serde(skip_serializing_if = "is_zero")]
    pub drops: usize,
    /// The arrows to an `unreachable` terminator.
    #[serde(skip_serializing_if = "is_zero")]
    pub unreachable: usize,
    /// The checks whose condition is a known value: they can never fail, so they are not points
    /// (design 61, A1).
    #[serde(skip_serializing_if = "is_zero")]
    pub folded_checks: usize,
    /// The branches on a known value: the walk follows the one arrow that the value selects.
    #[serde(skip_serializing_if = "is_zero")]
    pub folded_branches: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// What a block gives to the graph.
#[derive(Clone)]
enum Raw {
    /// A block with no point: the next block.
    Pass(usize),
    /// A dead end that is not a point: `unreachable`, `resume`, `abort`.
    End,
    /// A point, with its out edges (the label and the target block).
    Point(Point, Vec<(String, Target)>),
}

/// The target of an out edge of a raw point.
#[derive(Clone, Copy)]
enum Target {
    Block(usize),
    /// The `panic` point of a check.
    Fail,
}

type Range = (i64, i64, i64);

/// Builds the graph of a body. `item` is the def of the body, for the HIR of its constructs.
pub fn build(body: &Body, item: Option<rustc_public::DefId>) -> Graph {
    rustc_middle::ty::tls::with(|tcx| build_with(tcx, body, item))
}

fn build_with(tcx: rustc_middle::ty::TyCtxt<'_>, body: &Body, item: Option<rustc_public::DefId>) -> Graph {
    let mut graph = Graph::default();
    let local = item.and_then(|item| rustc_public::rustc_internal::internal(tcx, item).as_local());
    if let Some(local) = local
        && tcx.coroutine_kind(local.to_def_id()).is_some()
    {
        graph.co = true;
        // Only the entry, for its place: a nested body pairs through the node of its place
        // (R60.3-9).
        let mut entry = Point { k: "entry".into(), t: String::new(), m: None, r: None, c: None, x: Vec::new(), v: None, s: None };
        if let Some((tokens, call)) = crate::guarded(|| body_place(tcx, body, &mut graph.f)) {
            entry.r = tokens;
            entry.c = call;
        }
        graph.p.push(entry);
        return graph;
    }
    let hir = local.map(|local| arm_roles(tcx, local)).unwrap_or_default();
    let typing = local.map(|local| rustc_middle::ty::TypingEnv::post_analysis(tcx, local.to_def_id()));
    let roles = &hir.roles;
    let discriminants = discriminant_tables(body);
    let consts = crate::consts::Consts::new(body, item);
    let blocks = body.blocks.len();
    // The raw form of each block, and the desugaring expansion of each raw point.
    let mut raws: Vec<Raw> = Vec::with_capacity(blocks);
    let mut groups: Vec<Option<(rustc_span::ExpnId, Span, rustc_span::DesugaringKind)>> = Vec::with_capacity(blocks);
    for block in &body.blocks {
        let span = rustc_public::rustc_internal::internal(tcx, block.terminator.source_info.span);
        let raw = raw_block(tcx, typing, body, &block.terminator.kind, span, &discriminants, &hir.lets, &consts, &mut graph);
        let group = match (&raw, span.desugaring_kind()) {
            (Raw::Point(..), Some(kind)) => Some((span.ctxt().outer_expn(), span.ctxt().outer_expn_data().call_site, kind)),
            _ => None,
        };
        raws.push(raw);
        groups.push(group);
    }
    // The back edges of a walk from the entry along the normal edges: their targets are loop
    // heads.
    let successors = |index: usize| -> Vec<usize> {
        match &raws[index] {
            Raw::Pass(next) => vec![*next],
            Raw::End => Vec::new(),
            Raw::Point(_, edges) => edges.iter().filter_map(|(_, target)| match target { Target::Block(block) => Some(*block), Target::Fail => None }).collect(),
        }
    };
    let mut state = vec![0u8; blocks];
    let mut heads = vec![false; blocks];
    if blocks > 0 {
        let mut stack: Vec<(usize, Vec<usize>, usize)> = vec![(0, successors(0), 0)];
        state[0] = 1;
        while let Some((block, next, at)) = stack.last_mut() {
            if *at < next.len() {
                let target = next[*at];
                *at += 1;
                match state[target] {
                    0 => {
                        state[target] = 1;
                        let list = successors(target);
                        stack.push((target, list, 0));
                    }
                    1 => heads[target] = true,
                    _ => {}
                }
            } else {
                state[*block] = 2;
                stack.pop();
            }
        }
    }
    graph.n.cleanup = state.iter().filter(|seen| **seen == 0).count();
    // The point of each raw point: a group of one desugaring expansion is one point.
    let mut point_of: Vec<Option<usize>> = vec![None; blocks];
    let mut points: Vec<Point> = vec![Point { k: "entry".into(), t: String::new(), m: None, r: None, c: None, x: Vec::new(), v: None, s: None }];
    if let Some((tokens, call)) = crate::guarded(|| body_place(tcx, body, &mut graph.f)) {
        points[0].r = tokens;
        points[0].c = call;
    }
    let mut group_point: HashMap<rustc_span::ExpnId, usize> = HashMap::new();
    let mut group_of_point: BTreeMap<usize, (rustc_span::ExpnId, bool)> = BTreeMap::new();
    for index in 0..blocks {
        if state[index] == 0 {
            continue;
        }
        let Raw::Point(point, _) = &raws[index] else { continue };
        match groups[index] {
            Some((expn, call_site, kind)) => {
                let id = *group_point.entry(expn).or_insert_with(|| {
                    let (tokens, call) = place(tcx, call_site, &mut graph.f);
                    let text = point_text(tcx, call_site, &head_span(tcx, &body.blocks, &groups, expn));
                    points.push(Point { k: construct_name(kind).to_string(), t: text, m: None, r: tokens.map(array), c: call.map(array), x: Vec::new(), v: None, s: Some(call_site) });
                    group_of_point.insert(points.len() - 1, (expn, false));
                    points.len() - 1
                });
                // A call of the construct (`IntoIterator::into_iter`, `Iterator::next`,
                // `Try::branch`) is a part of the point: a changed callee changes the point.
                if matches!(point.k.as_str(), "call" | "panic") {
                    add_callee(&mut points[id].x, point);
                }
                point_of[index] = Some(id);
            }
            None => {
                points.push(point.clone());
                point_of[index] = Some(points.len() - 1);
            }
        }
    }
    // The loop heads. A head inside a construct makes the construct a loop head; each other head
    // gets a `loop` point.
    let mut loop_of: Vec<Option<usize>> = vec![None; blocks];
    for index in 0..blocks {
        if !heads[index] {
            continue;
        }
        if let (Some(point), Some(_)) = (point_of[index], groups[index]) {
            if let Some(entry) = group_of_point.get_mut(&point) {
                entry.1 = true;
            }
            continue;
        }
        let span = back_edge_span(tcx, body, index, &state, &successors);
        let (tokens, call) = span.map(|span| place(tcx, span, &mut graph.f)).unwrap_or((None, None));
        let text = span.and_then(|span| tcx.sess.source_map().span_to_snippet(span).ok()).map(|text| loop_keyword(&text)).unwrap_or_default();
        points.push(Point { k: "loop".into(), t: text, m: None, r: tokens.map(array), c: call.map(array), x: Vec::new(), v: None, s: span });
        loop_of[index] = Some(points.len() - 1);
    }
    // The point that a block leads to: its loop point, its own point, or the point after a chain
    // of blocks with no point. `None`: a dead end (an `unreachable`).
    let resolve = |start: usize, from_group: Option<usize>| -> Option<usize> {
        let mut block = start;
        let mut seen = 0;
        loop {
            if let Some(head) = loop_of[block] {
                return Some(head);
            }
            match &raws[block] {
                Raw::Point(..) => {
                    let point = point_of[block]?;
                    // Inside the same construct: the walk goes on in the construct.
                    if from_group == Some(point) {
                        return None;
                    }
                    return Some(point);
                }
                Raw::Pass(next) => block = *next,
                Raw::End => return None,
            }
            seen += 1;
            if seen > blocks {
                return None;
            }
        }
    };
    let mut arrows: Vec<(u32, u32, String)> = Vec::new();
    let push = |arrows: &mut Vec<(u32, u32, String)>, from: usize, to: usize, label: String| {
        let arrow = (from as u32, to as u32, label);
        if !arrows.contains(&arrow) {
            arrows.push(arrow);
        }
    };
    // The entry.
    match resolve(0, None) {
        Some(first) => push(&mut arrows, 0, first, String::new()),
        None => graph.n.unreachable += usize::from(blocks > 0),
    }
    // The loop points: the arrow to the first point of the head block. A chain back to the
    // head itself (`loop {}`) gives an arrow to the loop point.
    for index in 0..blocks {
        let Some(head) = loop_of[index] else { continue };
        let mut block = index;
        let mut seen = 0;
        let target = loop {
            if block != index && let Some(other) = loop_of[block] {
                break Some(other);
            }
            match &raws[block] {
                Raw::Point(..) => break point_of[block],
                Raw::Pass(next) if *next == index => break Some(head),
                Raw::Pass(next) => block = *next,
                Raw::End => break None,
            }
            seen += 1;
            if seen > blocks {
                break Some(head);
            }
        };
        match target {
            Some(target) => push(&mut arrows, head, target, String::new()),
            None => graph.n.unreachable += 1,
        }
    }
    // The fail points of the checks, made in the order of the walk below.
    let mut fail_of: BTreeMap<usize, usize> = BTreeMap::new();
    // The out arrows of each point that is not a construct.
    for index in 0..blocks {
        if state[index] == 0 || groups[index].is_some() {
            continue;
        }
        let (Raw::Point(point, edges), Some(from)) = (&raws[index], point_of[index]) else { continue };
        for (label, target) in edges {
            match target {
                Target::Block(block) => match resolve(*block, None) {
                    Some(to) => push(&mut arrows, from, to, label.clone()),
                    None => graph.n.unreachable += 1,
                },
                Target::Fail => {
                    let fail = *fail_of.entry(index).or_insert_with(|| {
                        points.push(Point { k: "panic".into(), t: check_kind(&point.t), m: None, r: point.r, c: point.c, x: Vec::new(), v: None, s: point.s });
                        points.len() - 1
                    });
                    push(&mut arrows, from, fail, label.clone());
                }
            }
        }
    }
    // The out arrows of each construct: a walk from each raw point of the construct through
    // the other raw points of the construct. The label comes from the first arm of a switch of
    // the construct on the way, as its role; with no switch, from the edge.
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..blocks {
        if state[index] != 0 && groups[index].is_some() && let Some(point) = point_of[index] {
            members.entry(point).or_default().push(index);
        }
    }
    for (point, blocks_of) in &members {
        let (expn, is_loop) = group_of_point.get(point).copied().unwrap_or((rustc_span::ExpnId::root(), false));
        let role = |label: &str| -> String {
            match roles.get(&(expn, last_segment(label).to_string())) {
                Some(role) if !role.is_empty() => role.to_string(),
                Some(_) => if is_loop { "next".into() } else { "continue".into() },
                None => label.to_string(),
            }
        };
        // The first raw points of the construct: no other raw point of it leads to them.
        let inner_next = |block: usize| -> Vec<usize> {
            let mut found = Vec::new();
            let Raw::Point(_, edges) = &raws[block] else { return found };
            for (_, target) in edges {
                let Target::Block(inner) = target else { continue };
                let mut inner = *inner;
                let mut seen = 0;
                while let Raw::Pass(after) = &raws[inner] {
                    inner = *after;
                    seen += 1;
                    if seen > blocks {
                        break;
                    }
                }
                if point_of[inner] == Some(*point) && loop_of[inner].is_none() {
                    found.push(inner);
                }
            }
            found
        };
        let reached: Vec<usize> = blocks_of.iter().flat_map(|block| inner_next(*block)).collect();
        let mut starts: Vec<usize> = blocks_of.iter().copied().filter(|block| !reached.contains(block)).collect();
        let mut work: Vec<(usize, Option<String>)> = Vec::new();
        let mut visited: Vec<(usize, Option<String>)> = Vec::new();
        loop {
            if work.is_empty() {
                // A raw point that no walk reached (a cycle inside the construct) starts a walk.
                let next = starts.pop().or_else(|| blocks_of.iter().copied().find(|block| !visited.iter().any(|(seen, _)| seen == block)));
                match next {
                    Some(block) => work.push((block, None)),
                    None => break,
                }
            }
            let Some((block, carried)) = work.pop() else { break };
            if visited.contains(&(block, carried.clone())) {
                continue;
            }
            visited.push((block, carried.clone()));
            let Raw::Point(raw, edges) = &raws[block] else { continue };
            for (label, target) in edges {
                // A switch of the construct gives the label to the walk after it.
                let own = if raw.k == "branch" { role(label) } else { label.clone() };
                let next_carried = carried.clone().or_else(|| (raw.k == "branch").then(|| own.clone()));
                let label = carried.clone().unwrap_or(own);
                match target {
                    Target::Fail => {
                        let fail = *fail_of.entry(block).or_insert_with(|| {
                            points.push(Point { k: "panic".into(), t: check_kind(&raw.t), m: None, r: points[*point].r, c: points[*point].c, x: Vec::new(), v: None, s: raw.s });
                            points.len() - 1
                        });
                        push(&mut arrows, *point, fail, label);
                    }
                    Target::Block(next) => match resolve(*next, Some(*point)) {
                        Some(to) => push(&mut arrows, *point, to, label),
                        None => {
                            // A block of the same construct, or a dead end.
                            let mut inner = *next;
                            let mut seen = 0;
                            loop {
                                match &raws[inner] {
                                    Raw::Pass(after) => inner = *after,
                                    Raw::Point(..) => {
                                        work.push((inner, next_carried.clone()));
                                        break;
                                    }
                                    Raw::End => {
                                        graph.n.unreachable += 1;
                                        break;
                                    }
                                }
                                seen += 1;
                                if seen > blocks {
                                    break;
                                }
                            }
                        }
                    },
                }
            }
        }
    }
    // The unwind arrows and the drops that the walk reaches.
    for (index, block) in body.blocks.iter().enumerate() {
        if state[index] == 0 {
            continue;
        }
        if let TerminatorKind::Drop { .. } = &block.terminator.kind {
            graph.n.drops += 1;
        }
        if let Some(UnwindAction::Cleanup(_)) = block.terminator.kind.unwind() {
            graph.n.unwind += 1;
        }
    }
    let (points, arrows) = merge_macro_exits(tcx, points, arrows, &mut graph.f);
    // The order of a walk from the entry along the arrows: the same MIR gives the same order.
    let mut order: Vec<usize> = vec![0];
    let mut at = 0;
    while at < order.len() {
        let from = order[at] as u32;
        for (a, b, _) in &arrows {
            if *a == from && !order.contains(&(*b as usize)) {
                order.push(*b as usize);
            }
        }
        at += 1;
    }
    for index in 0..points.len() {
        if !order.contains(&index) {
            order.push(index);
        }
    }
    let mut new_index = vec![0u32; points.len()];
    for (new, old) in order.iter().enumerate() {
        new_index[*old] = new as u32;
    }
    graph.p = order.iter().map(|old| points[*old].clone()).collect();
    graph.a = arrows.into_iter().map(|(a, b, label)| (new_index[a as usize], new_index[b as usize], label)).collect();
    graph
}

/// The expansion of the outermost macro call of a span, if the macro is outside the workspace
/// (the standard library, a registry crate). `None` for a span with no macro, and for a macro of
/// the workspace (R60.2-1 keeps its rule).
fn foreign_macro(tcx: rustc_middle::ty::TyCtxt<'_>, span: Span) -> Option<(rustc_span::ExpnId, Span)> {
    let mut found = None;
    let mut current = span;
    while current.from_expansion() {
        let expn = current.ctxt().outer_expn();
        let data = current.ctxt().outer_expn_data();
        if matches!(data.kind, ExpnKind::Macro(..)) {
            found = Some((expn, data.call_site, data.macro_def_id));
        }
        if data.call_site == current {
            break;
        }
        current = data.call_site;
    }
    let (expn, call, def) = found?;
    let in_workspace = def.is_some_and(|def| crate::spans::range(tcx, tcx.def_span(def), &mut Vec::new()).is_some());
    (!in_workspace).then_some((expn, call.source_callsite()))
}

/// Merges the points of the expansion of one macro call from outside the workspace that lead
/// only to a panic of the same expansion into one `panic` point (design 60, R60.2-12). The point
/// has the text of the macro call (`panic!("too big")`, `assert!(x < 9)`) and the callees of its
/// parts. A point from the macro input (the tokens of the user) has no expansion of a macro, so
/// it stays a point: the branch `x < 9` of `assert!(x < 9)` has the arrow `false` to the panic
/// point `assert!(x < 9)`.
fn merge_macro_exits(tcx: rustc_middle::ty::TyCtxt<'_>, points: Vec<Point>, arrows: Vec<(u32, u32, String)>, files: &mut Vec<String>) -> (Vec<Point>, Vec<(u32, u32, String)>) {
    let groups: Vec<Option<(rustc_span::ExpnId, Span)>> = points.iter().enumerate().map(|(index, point)| if index == 0 { None } else { point.s.and_then(|span| foreign_macro(tcx, span)) }).collect();
    if groups.iter().all(Option::is_none) {
        return (points, arrows);
    }
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); points.len()];
    for (a, b, _) in &arrows {
        out[*a as usize].push(*b as usize);
    }
    // The points that lead only to a panic of the same expansion: a panic with no arrow, or a
    // point whose arrows all go to such points.
    let mut exit = vec![false; points.len()];
    loop {
        let mut changed = false;
        for index in 0..points.len() {
            let Some((expn, _)) = groups[index] else { continue };
            if exit[index] {
                continue;
            }
            let leads = if out[index].is_empty() { points[index].k == "panic" } else { out[index].iter().all(|to| exit[*to] && groups[*to].is_some_and(|(other, _)| other == expn)) };
            if leads {
                exit[index] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // One point for each expansion: the place of its first member.
    let mut merged: HashMap<rustc_span::ExpnId, usize> = HashMap::new();
    let mut new_index: Vec<usize> = vec![0; points.len()];
    let mut kept: Vec<Point> = Vec::with_capacity(points.len());
    for (index, point) in points.iter().enumerate() {
        match groups[index].filter(|_| exit[index]) {
            Some((expn, call)) => {
                let id = *merged.entry(expn).or_insert_with(|| {
                    let range = crate::spans::range(tcx, call, files).map(array);
                    kept.push(Point { k: "panic".into(), t: snippet(tcx, call), m: None, r: range, c: range, x: Vec::new(), v: None, s: Some(call) });
                    kept.len() - 1
                });
                if matches!(point.k.as_str(), "call" | "panic") {
                    add_callee(&mut kept[id].x, point);
                } else {
                    for text in &point.x {
                        if let Err(at) = kept[id].x.binary_search(text) {
                            kept[id].x.insert(at, text.clone());
                        }
                    }
                }
                new_index[index] = id;
            }
            None => {
                kept.push(point.clone());
                new_index[index] = kept.len() - 1;
            }
        }
    }
    let mut moved: Vec<(u32, u32, String)> = Vec::with_capacity(arrows.len());
    for (a, b, label) in arrows {
        let (from, to) = (new_index[a as usize], new_index[b as usize]);
        // An arrow inside the merged point is not in the graph.
        if from == to && exit[a as usize] && exit[b as usize] {
            continue;
        }
        let arrow = (from as u32, to as u32, label);
        if !moved.contains(&arrow) {
            moved.push(arrow);
        }
    }
    (kept, moved)
}

/// Adds the callee of a call or a panic point to a list of callees: each text one time, in
/// order. The text of a call that the MIR does not resolve has its mark.
fn add_callee(list: &mut Vec<String>, point: &Point) {
    let mut texts: Vec<String> = point.x.clone();
    texts.push(match point.m {
        Some(mark) => format!("{} [{mark}]", point.t),
        None => point.t.clone(),
    });
    for text in texts {
        if let Err(at) = list.binary_search(&text) {
            list.insert(at, text);
        }
    }
}

/// The raw form of a block.
#[allow(clippy::too_many_arguments)]
fn raw_block<'tcx>(tcx: rustc_middle::ty::TyCtxt<'tcx>, typing: Option<rustc_middle::ty::TypingEnv<'tcx>>, body: &Body, kind: &TerminatorKind, span: Span, discriminants: &HashMap<usize, Vec<(u128, String)>>, lets: &HashMap<Span, String>, consts: &crate::consts::Consts, graph: &mut Graph) -> Raw {
    let point = |k: &str, t: String, m: Option<&'static str>, graph: &mut Graph| {
        let (tokens, call) = place(tcx, span, &mut graph.f);
        Point { k: k.to_string(), t, m, r: tokens.map(array), c: call.map(array), x: Vec::new(), v: None, s: Some(span) }
    };
    match kind {
        TerminatorKind::Goto { target } => Raw::Pass(*target),
        TerminatorKind::Drop { target, .. } => Raw::Pass(*target),
        TerminatorKind::Resume | TerminatorKind::Abort | TerminatorKind::Unreachable => Raw::End,
        TerminatorKind::Return => Raw::Point(point("return", String::new(), None, graph), Vec::new()),
        TerminatorKind::SwitchInt { discr, targets } => {
            // A branch on a known value follows the one arrow that the value selects (design
            // 61, A1).
            if let Some(crate::consts::Val::Bits { v, .. }) = consts.operand(discr) {
                graph.n.folded_branches += 1;
                return Raw::Pass(targets.branches().find(|(value, _)| *value == v).map(|(_, target)| target).unwrap_or(targets.otherwise()));
            }
            let text = lets.get(&span).cloned().unwrap_or_else(|| condition_text(tcx, span));
            let labels = switch_labels(body, discr, targets.branches().map(|(value, _)| value).collect(), discriminants);
            let mut edges: Vec<(String, Target)> = targets.branches().zip(labels.0).map(|((_, target), label)| (label, Target::Block(target))).collect();
            edges.push((labels.1, Target::Block(targets.otherwise())));
            let mut found = point("branch", text, None, graph);
            found.v = consts.switch_key(body, discr);
            Raw::Point(found, edges)
        }
        TerminatorKind::Assert { cond, expected, msg, target, .. } => {
            // A check whose condition is a known value that passes can never fail: it is not a
            // point, and its panic is not a point (design 61, A1). A check that always fails
            // stays a point.
            if let Some(crate::consts::Val::Bits { v, .. }) = consts.operand(cond)
                && (v == 1) == *expected
            {
                graph.n.folded_checks += 1;
                return Raw::Pass(*target);
            }
            let snippet = snippet(tcx, span);
            let text = if snippet.is_empty() { assert_kind(msg).to_string() } else { format!("{}: {snippet}", assert_kind(msg)) };
            let mut found = point("check", text, None, graph);
            found.v = match msg {
                AssertMessage::Overflow(op, left, right) => consts.key(&format!("{op:?}"), &[left, right]),
                AssertMessage::OverflowNeg(value) => consts.key("Neg", &[value]),
                AssertMessage::DivisionByZero(value) => consts.key("DivZero", &[value]),
                AssertMessage::RemainderByZero(value) => consts.key("RemZero", &[value]),
                AssertMessage::BoundsCheck { len, index } => consts.key("Bounds", &[len, index]),
                _ => None,
            };
            Raw::Point(found, vec![("ok".into(), Target::Block(*target)), ("fail".into(), Target::Fail)])
        }
        TerminatorKind::Call { func, target, .. } => {
            if crate::is_opaque_wrap(func) {
                return match target {
                    Some(target) => Raw::Pass(*target),
                    None => Raw::End,
                };
            }
            let (text, mark) = callee_text(tcx, typing, body, func, span);
            match target {
                Some(target) => Raw::Point(point("call", text, mark, graph), vec![("return".into(), Target::Block(*target))]),
                None => Raw::Point(point("panic", text, mark, graph), Vec::new()),
            }
        }
        TerminatorKind::InlineAsm { destination, .. } => match destination {
            Some(target) => Raw::Point(point("call", "asm!".into(), None, graph), vec![("return".into(), Target::Block(*target))]),
            None => Raw::Point(point("panic", "asm!".into(), None, graph), Vec::new()),
        },
    }
}

/// The text of a callee: the callee that the MIR calls (design 60, R60.2-11). The text has the
/// path and each generic argument of the call operand. If the MIR resolves the call to an impl
/// item, the text names the impl item (`<shop::Food as shop::Tax>::rate`); a provided method of a
/// trait has the mark `(trait default)`. A call that stays generic keeps the parameter
/// (`<T as shop::Tax>::rate`). A call that the MIR does not resolve gets the source text of the
/// operand and a mark.
pub(crate) fn callee_text<'tcx>(tcx: rustc_middle::ty::TyCtxt<'tcx>, typing: Option<rustc_middle::ty::TypingEnv<'tcx>>, body: &Body, func: &Operand, span: Span) -> (String, Option<&'static str>) {
    use rustc_public::ty::AssocContainer;
    let operand = || operand_text(&snippet(tcx, span));
    let Ok(ty) = func.ty(body.locals()) else { return (operand(), Some("pointer")) };
    match ty.kind() {
        TyKind::RigidTy(RigidTy::FnDef(def, args)) => {
            let trait_method = def.associated_item().is_some_and(|item| item.container == AssocContainer::Trait);
            if trait_method {
                let self_ty = args.0.first().and_then(|arg| match arg {
                    GenericArgKind::Type(ty) => Some(*ty),
                    _ => None,
                });
                let inner = |ty: rustc_public::ty::Ty| match ty.kind() {
                    TyKind::RigidTy(RigidTy::Ref(_, inner, _)) => inner,
                    _ => ty,
                };
                match self_ty.map(|ty| inner(ty).kind()) {
                    Some(TyKind::RigidTy(RigidTy::Closure(..))) => return (operand(), Some("closure")),
                    Some(TyKind::RigidTy(RigidTy::FnPtr(..))) => return (operand(), Some("pointer")),
                    Some(TyKind::RigidTy(RigidTy::Dynamic(..))) => return (operand(), Some("dyn")),
                    _ => {}
                }
            }
            let text = match rustc_public::rustc_internal::internal(tcx, ty).kind() {
                rustc_middle::ty::FnDef(def_id, args) => callee_path(tcx, typing, *def_id, args.skip_binder()),
                _ => def.name(),
            };
            // A path through an `extern crate` item names the crate (batch 5y, C4).
            (crate::reach::extern_crate_path(tcx, text), None)
        }
        _ => (operand(), Some("pointer")),
    }
}

/// The path of the callee that the MIR calls, with each generic argument (see `callee_text`).
fn callee_path<'tcx>(tcx: rustc_middle::ty::TyCtxt<'tcx>, typing: Option<rustc_middle::ty::TypingEnv<'tcx>>, def_id: rustc_span::def_id::DefId, args: rustc_middle::ty::GenericArgsRef<'tcx>) -> String {
    use rustc_middle::ty::TypeFoldable;
    let resolved = typing.and_then(|typing| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustc_middle::ty::Instance::try_resolve(tcx, typing, def_id, args))).ok()?.ok()?);
    let (target, args, default) = match resolved {
        Some(instance) => {
            // The impl has no item of its own: the call goes to the provided method of the trait.
            let default = matches!(instance.def, rustc_middle::ty::InstanceKind::Item(_)) && matches!(tcx.def_kind(tcx.parent(instance.def_id())), rustc_hir::def::DefKind::Trait);
            (instance.def_id(), instance.args, default)
        }
        None => (def_id, args, false),
    };
    let args = args.fold_with(&mut Named { tcx });
    let path = tcx.def_path_str_with_args(target, args);
    if default { format!("{path} (trait default)") } else { path }
}

/// A type folder that gives a closure type and a coroutine type the def path of its body
/// (`shop::total::{closure#0}`). The printer of the compiler shows the place of a closure in its
/// type, and a text with a place changes when the body moves.
struct Named<'tcx> {
    tcx: rustc_middle::ty::TyCtxt<'tcx>,
}

impl<'tcx> rustc_middle::ty::TypeFolder<rustc_middle::ty::TyCtxt<'tcx>> for Named<'tcx> {
    fn cx(&self) -> rustc_middle::ty::TyCtxt<'tcx> {
        self.tcx
    }

    fn fold_ty(&mut self, ty: rustc_middle::ty::Ty<'tcx>) -> rustc_middle::ty::Ty<'tcx> {
        use rustc_middle::ty::TypeSuperFoldable;
        match ty.kind() {
            rustc_middle::ty::Closure(def, _) | rustc_middle::ty::Coroutine(def, _) | rustc_middle::ty::CoroutineClosure(def, _) => {
                let name = rustc_span::Symbol::intern(&self.tcx.def_path_str(*def));
                rustc_middle::ty::Ty::new_param(self.tcx, u32::MAX / 2, name)
            }
            _ => ty.super_fold_with(self),
        }
    }
}

/// The source text of a call with its argument list removed: `add` for `add(2)`, `s.speak` for
/// `s.speak()`.
fn operand_text(text: &str) -> String {
    let text = text.trim();
    if text.ends_with(')') {
        let mut depth = 0i32;
        for (at, c) in text.char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' => {
                    depth -= 1;
                    if depth == 0 {
                        return flat(&text[..at]);
                    }
                }
                _ => {}
            }
        }
    }
    flat(text)
}

/// The text of a condition or of a scrutinee: the source text of the tokens in the workspace,
/// else of the outermost macro call. A `match` scrutinee has its keyword first in the span.
fn condition_text(tcx: rustc_middle::ty::TyCtxt<'_>, span: Span) -> String {
    let text = point_text(tcx, span, &span);
    match text.strip_prefix("match ") {
        Some(rest) => rest.trim().to_string(),
        None => text,
    }
}

/// The text of a point: the source text of `head` if it is in a file of the workspace, else
/// the source text of the outermost macro call of `span`, else the text of `head`. A text from
/// the body of a macro definition in the workspace (`$v > 9`) has the text of the macro call
/// first (`guard!(v): $v > 9`): a changed macro input then changes the text.
fn point_text(tcx: rustc_middle::ty::TyCtxt<'_>, span: Span, head: &Span) -> String {
    let mut files = Vec::new();
    let (_, call) = macro_call(span);
    let call = call.map(|call| call.source_callsite()).filter(|call| crate::spans::range(tcx, *call, &mut files).is_some());
    let in_workspace = crate::spans::range(tcx, *head, &mut files).is_some();
    match call {
        Some(call) if in_workspace && !call.contains(*head) => format!("{}: {}", snippet(tcx, call), snippet(tcx, *head)),
        Some(call) if !in_workspace => snippet(tcx, call),
        _ => snippet(tcx, *head),
    }
}

/// The source text of a span on one line: each run of white space is one space.
fn snippet(tcx: rustc_middle::ty::TyCtxt<'_>, span: Span) -> String {
    flat(&tcx.sess.source_map().span_to_snippet(span).unwrap_or_default())
}

fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The outermost macro call of a span.
fn macro_call(span: Span) -> (Span, Option<Span>) {
    let mut call = None;
    let mut current = span;
    while current.from_expansion() {
        let data = current.ctxt().outer_expn_data();
        if matches!(data.kind, ExpnKind::Macro(..)) {
            call = Some(data.call_site);
        }
        if data.call_site == current {
            break;
        }
        current = data.call_site;
    }
    (span, call)
}

/// The two ranges of a span (design 40.1): its tokens, and its outermost macro call.
pub(crate) fn place(tcx: rustc_middle::ty::TyCtxt<'_>, span: Span, files: &mut Vec<String>) -> (Option<Range>, Option<Range>) {
    let tokens = crate::spans::range(tcx, span, files);
    let (_, call) = macro_call(span);
    let call = call.and_then(|call| crate::spans::range(tcx, call.source_callsite(), files));
    (tokens, call)
}

pub(crate) fn array(range: Range) -> [i64; 3] {
    [range.0, range.1, range.2]
}

/// The place of the entry: the span of the body.
fn body_place(tcx: rustc_middle::ty::TyCtxt<'_>, body: &Body, files: &mut Vec<String>) -> (Option<[i64; 3]>, Option<[i64; 3]>) {
    let span = rustc_public::rustc_internal::internal(tcx, body.span);
    let (tokens, call) = place(tcx, span, files);
    (tokens.map(array), call.map(array))
}

/// The span of the head of a construct: the span of its first raw point in block order.
fn head_span(tcx: rustc_middle::ty::TyCtxt<'_>, blocks: &[rustc_public::mir::BasicBlock], groups: &[Option<(rustc_span::ExpnId, Span, rustc_span::DesugaringKind)>], expn: rustc_span::ExpnId) -> Span {
    let first = groups.iter().position(|group| group.is_some_and(|(id, _, _)| id == expn)).unwrap_or(0);
    rustc_public::rustc_internal::internal(tcx, blocks[first].terminator.source_info.span)
}

/// The name of a construct from its desugaring kind.
fn construct_name(kind: rustc_span::DesugaringKind) -> &'static str {
    use rustc_span::DesugaringKind;
    match kind {
        DesugaringKind::QuestionMark => "try",
        DesugaringKind::ForLoop => "for",
        DesugaringKind::WhileLoop => "while",
        DesugaringKind::Await => "await",
        DesugaringKind::TryBlock => "try block",
        DesugaringKind::YeetExpr => "yeet",
        DesugaringKind::RangeExpr => "range",
        DesugaringKind::Async => "async",
        _ => "construct",
    }
}

/// The span of a back edge to a loop head: the terminator of a block that the walk reaches and
/// that goes to the head, with the largest span (the loop expression).
fn back_edge_span(tcx: rustc_middle::ty::TyCtxt<'_>, body: &Body, head: usize, state: &[u8], successors: &dyn Fn(usize) -> Vec<usize>) -> Option<Span> {
    let mut best: Option<Span> = None;
    for (index, block) in body.blocks.iter().enumerate() {
        if state[index] == 0 || !successors(index).contains(&head) || !matches!(block.terminator.kind, TerminatorKind::Goto { .. }) {
            continue;
        }
        let span = rustc_public::rustc_internal::internal(tcx, block.terminator.source_info.span);
        if span.is_dummy() {
            continue;
        }
        if best.is_none_or(|known| span.contains(known) && span != known) {
            best = Some(span);
        }
    }
    best
}

/// The keyword of a loop: `loop`, `while`, or a label with its keyword (`'outer: loop`).
fn loop_keyword(text: &str) -> String {
    let mut words = text.split_whitespace();
    match words.next() {
        Some(label) if label.starts_with('\'') && label.ends_with(':') => format!("{label} {}", words.next().unwrap_or("")),
        Some(word) => word.trim_end_matches('{').to_string(),
        None => String::new(),
    }
}

/// The name of an assert kind.
fn assert_kind(msg: &AssertMessage) -> &'static str {
    match msg {
        AssertMessage::BoundsCheck { .. } => "bounds",
        AssertMessage::Overflow(..) => "overflow",
        AssertMessage::OverflowNeg(..) => "overflow",
        AssertMessage::DivisionByZero(..) => "division by zero",
        AssertMessage::RemainderByZero(..) => "remainder by zero",
        AssertMessage::ResumedAfterReturn(..) | AssertMessage::ResumedAfterPanic(..) | AssertMessage::ResumedAfterDrop(..) => "resume",
        AssertMessage::MisalignedPointerDereference { .. } => "alignment",
        AssertMessage::NullPointerDereference | AssertMessage::NullReferenceConstructed => "null pointer",
        AssertMessage::InvalidEnumConstruction(..) => "enum value",
    }
}

/// The text of the panic point of a check: its kind.
fn check_kind(text: &str) -> String {
    text.split(':').next().unwrap_or(text).to_string()
}

/// The labels of a switch: one for each value, and one for `otherwise`.
fn switch_labels(body: &Body, discr: &Operand, values: Vec<u128>, discriminants: &HashMap<usize, Vec<(u128, String)>>) -> (Vec<String>, String) {
    let local = match discr {
        Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => Some(place.local),
        _ => None,
    };
    if let Some(table) = local.and_then(|local| discriminants.get(&local)) {
        let name = |value: u128| table.iter().find(|(known, _)| *known == value).map(|(_, path)| path.clone()).unwrap_or_else(|| value.to_string());
        let rest: Vec<&String> = table.iter().filter(|(known, _)| !values.contains(known)).map(|(_, path)| path).collect();
        let otherwise = if rest.len() == 1 { rest[0].clone() } else { "otherwise".to_string() };
        return (values.iter().map(|value| name(*value)).collect(), otherwise);
    }
    let ty = discr.ty(body.locals()).ok().map(|ty| ty.kind());
    match ty {
        Some(TyKind::RigidTy(RigidTy::Bool)) => {
            let label = |value: u128| if value == 0 { "false" } else { "true" }.to_string();
            let otherwise = if values == [0] { "true" } else if values == [1] { "false" } else { "otherwise" };
            (values.iter().map(|value| label(*value)).collect(), otherwise.to_string())
        }
        Some(TyKind::RigidTy(RigidTy::Int(int))) => {
            let bits = match int {
                IntTy::I8 => 8,
                IntTy::I16 => 16,
                IntTy::I32 => 32,
                IntTy::I64 => 64,
                IntTy::I128 => 128,
                IntTy::Isize => 64,
            };
            let signed = |value: u128| -> String {
                let shift = 128 - bits;
                (((value << shift) as i128) >> shift).to_string()
            };
            (values.iter().map(|value| signed(*value)).collect(), "otherwise".into())
        }
        Some(TyKind::RigidTy(RigidTy::Char)) => (values.iter().map(|value| char::from_u32(*value as u32).map(|c| format!("{c:?}")).unwrap_or_else(|| value.to_string())).collect(), "otherwise".into()),
        _ => (values.iter().map(|value| value.to_string()).collect(), "otherwise".into()),
    }
}

/// The variant table of each local that holds the discriminant of an enum.
fn discriminant_tables(body: &Body) -> HashMap<usize, Vec<(u128, String)>> {
    let mut found = HashMap::new();
    for block in &body.blocks {
        for statement in &block.statements {
            let StatementKind::Assign(place, Rvalue::Discriminant(source)) = &statement.kind else { continue };
            if !place.projection.is_empty() {
                continue;
            }
            let Ok(ty) = source.ty(body.locals()) else { continue };
            if let TyKind::RigidTy(RigidTy::Adt(def, _)) = ty.kind()
                && def.kind() == AdtKind::Enum
            {
                found.insert(place.local, crate::print::discriminant_table(def));
            }
        }
    }
    found
}

/// The last segment of a path.
fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// The role of each arm of each desugared `match` of a body, by the expansion of the `match`
/// and the name of the variant of the arm pattern: `exit` for an arm whose body is `return`,
/// `done` for an arm whose body is `break`, empty for each other arm. Also the text of each
/// `let` condition.
fn arm_roles(tcx: rustc_middle::ty::TyCtxt<'_>, local: rustc_span::def_id::LocalDefId) -> Hir {
    struct Arms<'tcx> {
        tcx: rustc_middle::ty::TyCtxt<'tcx>,
        typeck: &'tcx rustc_middle::ty::TypeckResults<'tcx>,
        out: HashMap<(rustc_span::ExpnId, String), &'static str>,
        lets: HashMap<Span, String>,
    }
    impl<'tcx> Arms<'tcx> {
        fn variant(&self, pat: &rustc_hir::Pat<'_>) -> Option<String> {
            use rustc_hir::def::{DefKind, Res};
            let qpath = match &pat.kind {
                rustc_hir::PatKind::TupleStruct(qpath, ..) | rustc_hir::PatKind::Struct(qpath, ..) => qpath,
                rustc_hir::PatKind::Expr(expr) => match &expr.kind {
                    rustc_hir::PatExprKind::Path(qpath) => qpath,
                    _ => return None,
                },
                rustc_hir::PatKind::Binding(_, _, _, Some(inner)) => return self.variant(inner),
                _ => return None,
            };
            let hir_id = match &pat.kind {
                rustc_hir::PatKind::Expr(expr) => expr.hir_id,
                _ => pat.hir_id,
            };
            match self.typeck.qpath_res(qpath, hir_id) {
                Res::Def(DefKind::Variant, def) => Some(self.tcx.item_name(def).to_string()),
                Res::Def(DefKind::Ctor(..), def) => Some(self.tcx.item_name(self.tcx.parent(def)).to_string()),
                _ => None,
            }
        }
    }
    impl<'tcx> rustc_hir::intravisit::Visitor<'tcx> for Arms<'tcx> {
        fn visit_expr(&mut self, expr: &'tcx rustc_hir::Expr<'tcx>) {
            // The key of a desugared `match` is the expansion of its scrutinee: the `match` of a
            // `?` has the span of the source expression.
            if let rustc_hir::ExprKind::Match(scrutinee, arms, source) = &expr.kind
                && *source != rustc_hir::MatchSource::Normal
                && scrutinee.span.desugaring_kind().is_some()
            {
                let expn = scrutinee.span.ctxt().outer_expn();
                for arm in arms.iter() {
                    let role = match &arm.body.kind {
                        rustc_hir::ExprKind::Ret(_) => "exit",
                        rustc_hir::ExprKind::Break(..) => "done",
                        _ => "",
                    };
                    if let Some(name) = self.variant(arm.pat) {
                        self.out.insert((expn, name), role);
                    }
                }
            }
            // The condition of an `if let` or a `while let`: the switch has the span of the
            // pattern, and the text is the full `let`.
            if let rustc_hir::ExprKind::Let(found) = &expr.kind {
                self.lets.insert(found.pat.span, snippet(self.tcx, found.span));
            }
            rustc_hir::intravisit::walk_expr(self, expr);
        }

        fn visit_local(&mut self, statement: &'tcx rustc_hir::LetStmt<'tcx>) {
            if statement.els.is_some()
                && let Some(init) = statement.init
            {
                self.lets.insert(statement.pat.span, format!("let {}", snippet(self.tcx, statement.pat.span.to(init.span))));
            }
            rustc_hir::intravisit::walk_local(self, statement);
        }
    }
    let Some(body) = tcx.hir_maybe_body_owned_by(local) else { return Hir::default() };
    if !tcx.has_typeck_results(local) {
        return Hir::default();
    }
    let mut arms = Arms { tcx, typeck: tcx.typeck(local), out: HashMap::new(), lets: HashMap::new() };
    rustc_hir::intravisit::Visitor::visit_body(&mut arms, body);
    Hir { roles: arms.out, lets: arms.lets }
}

/// The data of the HIR of a body for its graph.
#[derive(Default)]
struct Hir {
    /// The role of each arm of a desugared `match` (see `arm_roles`).
    roles: HashMap<(rustc_span::ExpnId, String), &'static str>,
    /// The text of each `let` condition, by the span of its pattern.
    lets: HashMap<Span, String>,
}
