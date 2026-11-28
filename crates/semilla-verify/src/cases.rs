//! The case table of a pair of code (design 101).
//!
//! - [R101.4-1] A small callee goes in for the cases: a callee whose canonical text the driver
//!   gives (at most 24 blocks, depth 3, no recursion) goes in before Semilla reads the cases. It
//!   changes only the case table: never the fit of a site or the compare of a body.
//! - [R101.5-1] A case is one path through the graph of a closure from the entry to one end. Its
//!   key is the list of its decisions on the holes (for example "`r` is `Err`"). The cases of the
//!   old closure and of the new closure pair by equal keys; a case with no partner is "removed"
//!   or "new".
//! - [R101.6-1] The label of a case comes from the code: "equal code" (the result, the effects,
//!   the drops, and the end are equal under the binding), "changed" (the line says what
//!   differs), "undefined on the two sides" (the two paths end in `unreachable`), "accepts
//!   more" and "accepts less" (one path ends in a panic or `unreachable`, the other returns),
//!   "new", "removed". Each old case and each new case has exactly one line.
//!
//! The table is a fact of the form: no solver, no proof. Only "equal code" is a guarantee, and
//! only for its case.

use std::collections::{BTreeMap, BTreeSet};

use crate::rewrites::{Mir, locals_in, parse_mir};

pub const RULE_CALLEE: &str = "R101.4-1";
pub const RULE_CASES: &str = "R101.5-1";
pub const RULE_LABEL: &str = "R101.6-1";

/// The most blocks of one path (a loop gives no table) and the most paths of one closure.
const MOST_PATHS: usize = 64;
/// The most levels of callees that go in.
const MOST_DEPTH: usize = 3;

/// The end of one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// A `return` with the value of the result.
    Return(String),
    /// `unreachable`, or [R101.5-2] the check of undefined behavior (`assume(UbChecks)`).
    Unreachable,
    /// A call that does not return (a panic), or a failed `assert`.
    Panic(String),
}

/// One choice of a decision: the values of the subject in this case.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Choice {
    In(BTreeSet<String>),
    NotIn(BTreeSet<String>),
}

/// One path of a closure.
#[derive(Clone, Debug, Default)]
pub(crate) struct Path {
    /// [R101.5-1] The decisions: the subject (a text in the names of the holes) and its choice.
    decisions: BTreeMap<String, Choice>,
    /// The universe of the values of a subject, when the code gives it (the table of a
    /// discriminant).
    universe: BTreeMap<String, BTreeSet<String>>,
    pub effects: Vec<String>,
    pub drops: Vec<String>,
    visits: BTreeMap<(usize, String, usize), usize>,
}

/// The callee texts by the normal key of their path.
pub(crate) struct Callees {
    texts: BTreeMap<String, String>,
}

impl Callees {
    pub(crate) fn new(texts: &BTreeMap<String, String>) -> Callees {
        Callees { texts: texts.iter().map(|(path, text)| (path_key(path), text.clone())).collect() }
    }
}

/// The key of a callee path: the paths of the standard library under one name, with no generic
/// arguments (`std::result::Result::<T, E>::unwrap_unchecked` is
/// `core::result::Result::unwrap_unchecked`).
fn path_key(path: &str) -> String {
    let text = crate::rewrites::std_paths(path);
    let mut out = String::new();
    let mut depth = 0usize;
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < text.len() {
        if depth == 0 && text[at..].starts_with("::<") {
            depth = 1;
            at += 3;
            continue;
        }
        if depth > 0 {
            match bytes[at] {
                b'<' => depth += 1,
                b'>' if !(at > 0 && bytes[at - 1] == b'-') => depth -= 1,
                _ => {}
            }
            at += 1;
            continue;
        }
        out.push(bytes[at] as char);
        at += 1;
    }
    out
}

/// The generic arguments of each `::<…>` of a path, in order.
fn generic_args(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = path[from..].find("::<").map(|at| from + at) {
        let start = at + 3;
        let mut depth = 1usize;
        let mut part = start;
        let bytes = path.as_bytes();
        let mut end = path.len();
        for (offset, c) in path[start..].char_indices() {
            let place = start + offset;
            match c {
                '<' | '(' | '[' => depth += 1,
                '>' if place > 0 && bytes[place - 1] == b'-' => {}
                '>' | ')' | ']' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(path[part..place].trim().to_string());
                        end = place;
                        break;
                    }
                }
                ',' if depth == 1 => {
                    out.push(path[part..place].trim().to_string());
                    part = place + 1;
                }
                _ => {}
            }
        }
        from = end;
    }
    out.retain(|arg| !arg.starts_with('\''));
    out
}

/// The top-level parts of an argument list (no comma inside brackets).
fn top_parts(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut part = 0;
    let bytes = text.as_bytes();
    for (at, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            '>' if at > 0 && bytes[at - 1] == b'-' => {}
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(text[part..at].trim().to_string());
                part = at + 1;
            }
            _ => {}
        }
    }
    if !text[part..].trim().is_empty() {
        out.push(text[part..].trim().to_string());
    }
    out
}

/// A word replace: each name of `map` as a whole word.
fn replace_words(text: &str, map: &BTreeMap<String, String>) -> String {
    if map.is_empty() {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let c = bytes[at];
        if (c.is_ascii_alphabetic() || c == b'_') && (at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_' || bytes[at - 1] == b':' || bytes[at - 1] == b'#')) {
            let end = text[at..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(text.len(), |len| at + len);
            let word = &text[at..end];
            match map.get(word).filter(|_| !text[end..].starts_with("::") && !text[end..].starts_with('#')) {
                Some(new) => out.push_str(new),
                None => out.push_str(word),
            }
            at = end;
            continue;
        }
        let ch = text[at..].chars().next().unwrap_or(' ');
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

/// The text with each local replaced by its value in `env` (a local with no value: `?N`), then
/// simplified: `(*&shared X)` and `(*&mut X)` are `X`.
fn subst(text: &str, env: &BTreeMap<usize, String>, frame: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, number) in locals_in(text) {
        out.push_str(&text[at..start]);
        match env.get(&number) {
            Some(value) => out.push_str(value),
            None => out.push_str(&format!("?{frame}_{number}")),
        }
        at = end;
    }
    out.push_str(&text[at..]);
    simplify(&out)
}

fn simplify(text: &str) -> String {
    let mut out = text.to_string();
    loop {
        let before = out.len();
        for word in ["(*&shared ", "(*&mut ", "(*&raw Const ", "(*&raw Mut "] {
            while let Some(at) = out.find(word) {
                // The place after the word ends at the `)` that closes the `(*`.
                let start = at + word.len();
                let mut depth = 1i32;
                let mut close = None;
                for (offset, c) in out[start..].char_indices() {
                    match c {
                        '(' | '[' | '{' => depth += 1,
                        ')' | ']' | '}' => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(start + offset);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let Some(close) = close else { break };
                let inner = out[start..close].to_string();
                out.replace_range(at..=close, &inner);
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// [R101.5-1] The subject and the choice of a bool decision: `Not(X)` is the other value of `X`;
/// `Eq(D, const isize = k)` and `Ne(…)` on a discriminant `D` choose the variant of `k`.
fn bool_choice(expr: &str, truth: bool) -> (String, Choice, Option<BTreeSet<String>>) {
    if let Some(inner) = expr.strip_prefix("Not(").and_then(|rest| rest.strip_suffix(')')) {
        return bool_choice(inner, !truth);
    }
    for (op, equal) in [("Eq(", true), ("Ne(", false)] {
        if let Some(inner) = expr.strip_prefix(op).and_then(|rest| rest.strip_suffix(')'))
            && let [a, b] = top_parts(inner).as_slice()
        {
            let (disc, constant) = if a.starts_with("discriminant(") { (a, b) } else { (b, a) };
            if let Some((subject, table)) = discriminant(disc)
                && let Some(number) = constant.rsplit_once(" = ").map(|(_, n)| n.to_string())
            {
                let variant = table.as_ref().and_then(|table| table.iter().find(|(_, n)| *n == number).map(|(name, _)| name.clone())).unwrap_or(number);
                let universe: Option<BTreeSet<String>> = table.map(|table| table.into_iter().map(|(name, _)| name).collect());
                let set: BTreeSet<String> = [variant].into();
                return (subject, if truth == equal { Choice::In(set) } else { Choice::NotIn(set) }, universe);
            }
        }
    }
    // The arm `0` of a switch: the value is 0 (`false` for a bool); else it is not 0.
    let zero: BTreeSet<String> = ["0".to_string()].into();
    (expr.to_string(), if truth { Choice::NotIn(zero) } else { Choice::In(zero) }, None)
}

/// [R101.5-1] The subject and the choice of one arm of a switch. `value`: the text of the arm
/// (`core::result::Result::Ok`, `0`), or `None` for `otherwise` with the values of the other
/// arms. A switch with the arms `0` and `otherwise` is a bool decision.
fn choice(expr: &str, value: Option<&str>, others: &[String]) -> (String, Choice, Option<BTreeSet<String>>) {
    if others == ["0"] {
        return bool_choice(expr, value.is_none());
    }
    if let Some((subject, table)) = discriminant(expr) {
        let universe: Option<BTreeSet<String>> = table.as_ref().map(|table| table.iter().map(|(name, _)| name.clone()).collect());
        let name = |text: &str| -> String { table.as_ref().and_then(|table| table.iter().find(|(_, n)| n == text).map(|(name, _)| name.clone())).unwrap_or_else(|| text.to_string()) };
        return match value {
            Some(value) => (subject, Choice::In([name(value)].into()), universe),
            None => (subject, Choice::NotIn(others.iter().map(|other| name(other)).collect()), universe),
        };
    }
    match value {
        Some(value) => (expr.to_string(), Choice::In([value.to_string()].into()), None),
        None => (expr.to_string(), Choice::NotIn(others.iter().cloned().collect()), None),
    }
}

/// The subject and the table of a discriminant text: `discriminant(P)` or `discriminant(P) of T
/// values [A = 1, B = 0]`.
fn discriminant(text: &str) -> Option<(String, Option<Vec<(String, String)>>)> {
    let rest = text.strip_prefix("discriminant(")?;
    let mut depth = 1i32;
    let mut close = None;
    for (at, c) in rest.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(at);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let subject = rest[..close].to_string();
    let tail = &rest[close + 1..];
    let table = tail.split_once(" values [").and_then(|(_, list)| list.strip_suffix(']')).map(|list| list.split(", ").filter_map(|pair| pair.split_once(" = ").map(|(name, n)| (name.to_string(), n.to_string()))).collect());
    Some((subject, table))
}

impl Path {
    /// [R101.5-1] Adds a decision. `false`: no value meets the decisions of the path.
    fn decide(&mut self, subject: String, new: Choice, universe: Option<BTreeSet<String>>) -> bool {
        if let Some(universe) = universe {
            self.universe.entry(subject.clone()).or_insert(universe);
        }
        let merged = match (self.decisions.remove(&subject), new) {
            (None, new) => new,
            (Some(Choice::In(a)), Choice::In(b)) => Choice::In(a.intersection(&b).cloned().collect()),
            (Some(Choice::In(a)), Choice::NotIn(b)) | (Some(Choice::NotIn(b)), Choice::In(a)) => Choice::In(a.difference(&b).cloned().collect()),
            (Some(Choice::NotIn(a)), Choice::NotIn(b)) => Choice::NotIn(a.union(&b).cloned().collect()),
        };
        // A known universe makes a negative choice a list of values.
        let merged = match (&merged, self.universe.get(&subject)) {
            (Choice::NotIn(not), Some(universe)) => Choice::In(universe.difference(not).cloned().collect()),
            _ => merged,
        };
        let feasible = !matches!(&merged, Choice::In(set) if set.is_empty());
        self.decisions.insert(subject, merged);
        feasible
    }

    /// The key of the case: each decision in the order of its subject.
    pub fn key(&self) -> String {
        self.decisions.iter().map(|(subject, choice)| decision_text(subject, choice)).collect::<Vec<_>>().join(", ")
    }
}

fn decision_text(subject: &str, choice: &Choice) -> String {
    let list = |set: &BTreeSet<String>| set.iter().map(|value| format!("`{}`", short(value))).collect::<Vec<_>>().join(" or ");
    match choice {
        Choice::In(set) => format!("`{}` is {}", short(subject), list(set)),
        Choice::NotIn(set) => format!("`{}` is not {}", short(subject), list(set)),
    }
}

/// A text for the reader: each path with its last name only (`core::result::Result::Ok` is
/// `Ok`).
pub(crate) fn short(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let c = bytes[at];
        if c.is_ascii_alphabetic() || c == b'_' {
            let mut end = at;
            loop {
                let word_end = text[end..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(text.len(), |len| end + len);
                if text[word_end..].starts_with("::") && text[word_end + 2..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                    end = word_end + 2;
                    continue;
                }
                out.push_str(&text[end..word_end]);
                at = word_end;
                break;
            }
            continue;
        }
        let ch = text[at..].chars().next().unwrap_or(' ');
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

/// One end of a run of a body: the value of the result, or the end of the path.
enum Exit {
    Returned(String),
    Ended(End),
}

struct Ctx<'a> {
    callees: &'a Callees,
    error: Option<String>,
    frames: usize,
}

/// The call of a terminator: the destination local, the path in braces, the arguments, and the
/// normal target. `None`: no call.
fn call_parts(text: &str) -> Option<(Option<usize>, String, Vec<String>)> {
    let (left, right) = match text.split_once(" = const ") {
        Some((left, right)) => (Some(left), right),
        None => (None, text.strip_prefix("const ")?),
    };
    let destination = left.and_then(|left| {
        let locals = locals_in(left);
        (locals.len() == 1 && locals[0].0 == 0 && locals[0].1 == left.len()).then(|| locals[0].2)
    });
    let open = right.find('{')?;
    let mut depth = 0i32;
    let mut close = None;
    for (at, c) in right[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + at);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let path = right[open + 1..close].to_string();
    let args_text = right[close + 1..].strip_prefix('(')?;
    let mut depth = 1i32;
    let mut end = None;
    for (at, c) in args_text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(at);
                    break;
                }
            }
            _ => {}
        }
    }
    let args = top_parts(&args_text[..end?]);
    Some((destination, path, args))
}

/// [R101.5-1] The runs of one body from a block, with the values of the locals: each end with
/// its path.
#[allow(clippy::too_many_arguments)]
fn run(ctx: &mut Ctx<'_>, body: &Mir, stack: &[String], depth: usize, frame: usize, env: BTreeMap<usize, String>, path: Path, block: usize) -> Vec<(Path, Exit)> {
    let name = stack.last().map(String::as_str).unwrap_or("");
    let mut out = Vec::new();
    let mut todo: Vec<(BTreeMap<usize, String>, Path, usize)> = vec![(env, path, block)];
    while let Some((mut env, mut path, block)) = todo.pop() {
        if ctx.error.is_some() {
            return Vec::new();
        }
        let visit = path.visits.entry((frame, name.to_string(), block)).or_default();
        *visit += 1;
        if *visit > 1 {
            ctx.error = Some("the closure has a loop: Semilla does not read the cases of a loop".into());
            return Vec::new();
        }
        let Some(list) = body.blocks.get(&block) else {
            ctx.error = Some(format!("no block `bb{block}`"));
            return Vec::new();
        };
        for (place, index) in list.iter().enumerate() {
            let text = crate::rewrites::std_paths(&body.elements[*index].0);
            if place + 1 < list.len() {
                // A statement.
                if text.starts_with("assume(") {
                    // [R101.5-2] The check of undefined behavior: the path is undefined (a debug
                    // build panics here).
                    if text.contains("UbChecks") {
                        out.push((path.clone(), Exit::Ended(End::Unreachable)));
                        break;
                    }
                    path.effects.push(subst(&text, &env, frame));
                    continue;
                }
                match text.split_once(" = ") {
                    Some((left, right)) if locals_in(left).len() == 1 && locals_in(left)[0].0 == 0 && locals_in(left)[0].1 == left.len() => {
                        let local = locals_in(left)[0].2;
                        let value = subst(right, &env, frame);
                        env.insert(local, value);
                    }
                    Some((left, right)) => path.effects.push(format!("write `{}` = `{}`", subst(left, &env, frame), subst(right, &env, frame))),
                    None => path.effects.push(subst(&text, &env, frame)),
                }
                continue;
            }
            // The terminator.
            let kind = crate::rewrites::kind_of(&text, true);
            match kind {
                crate::rewrites::Kind::Return => out.push((path.clone(), Exit::Returned(env.get(&0).cloned().unwrap_or_else(|| "()".into())))),
                crate::rewrites::Kind::Unreachable => out.push((path.clone(), Exit::Ended(End::Unreachable))),
                crate::rewrites::Kind::Goto(target) => todo.push((env.clone(), path.clone(), target)),
                crate::rewrites::Kind::Switch(targets) => {
                    let Some(operand) = text.strip_prefix("switchInt(").and_then(|rest| rest.split_once(") -> [")).map(|(operand, _)| operand.to_string()) else {
                        ctx.error = Some(format!("a branch that Semilla does not read: `{text}`"));
                        return Vec::new();
                    };
                    let expr = subst(&operand, &env, frame);
                    let list = text.rsplit_once(" -> [").map(|(_, list)| list.trim_end_matches(']').to_string()).unwrap_or_default();
                    let values: Vec<String> = top_parts(&list).iter().filter_map(|arm| arm.rsplit_once(": bb").map(|(value, _)| value.to_string())).collect();
                    let listed: Vec<String> = values.iter().filter(|value| *value != "otherwise").cloned().collect();
                    for (value, target) in values.iter().zip(&targets) {
                        let otherwise = value == "otherwise";
                        // The `otherwise` arm of a full `match` on a discriminant (a block with
                        // only `unreachable`) is no case: no value takes it.
                        let only_unreachable = body.blocks.get(target).is_some_and(|list| list.len() == 1 && body.elements[list[0]].0 == "unreachable");
                        if otherwise && only_unreachable && discriminant(&expr).is_some() {
                            continue;
                        }
                        // [R101.5-2] The check of undefined behavior.
                        if expr.contains("UbChecks") {
                            if otherwise {
                                out.push((path.clone(), Exit::Ended(End::Unreachable)));
                            } else {
                                todo.push((env.clone(), path.clone(), *target));
                            }
                            continue;
                        }
                        let (subject, chosen, universe) = choice(&expr, (!otherwise).then_some(value.as_str()), &listed);
                        let mut next = path.clone();
                        if next.decide(subject, chosen, universe) {
                            todo.push((env.clone(), next, *target));
                        }
                    }
                }
                crate::rewrites::Kind::Normal(target) if text.starts_with("drop(") => {
                    let place = text.strip_prefix("drop(").and_then(|rest| rest.split_once(") -> ")).map(|(place, _)| place).unwrap_or("");
                    path.drops.push(subst(place, &env, frame));
                    todo.push((env.clone(), path.clone(), target));
                }
                crate::rewrites::Kind::Normal(target) if text.starts_with("assert(") => {
                    let cond = text.strip_prefix("assert(").and_then(|rest| rest.split_once(") -> ")).map(|(cond, _)| cond.to_string()).unwrap_or_default();
                    let cond = subst(&cond, &env, frame);
                    let mut fails = path.clone();
                    if fails.decide(format!("assert {cond}"), Choice::In(["fails".to_string()].into()), Some(["fails".to_string(), "holds".to_string()].into())) {
                        out.push((fails, Exit::Ended(End::Panic(format!("assert {}", short(&cond))))));
                    }
                    let mut holds = path.clone();
                    if holds.decide(format!("assert {cond}"), Choice::In(["holds".to_string()].into()), Some(["fails".to_string(), "holds".to_string()].into())) {
                        todo.push((env.clone(), holds, target));
                    }
                }
                crate::rewrites::Kind::Normal(_) | crate::rewrites::Kind::Diverge => {
                    let Some((destination, callee, args)) = call_parts(&text) else {
                        ctx.error = Some(format!("an element that Semilla does not read: `{text}`"));
                        return Vec::new();
                    };
                    let target = match kind {
                        crate::rewrites::Kind::Normal(target) => Some(target),
                        _ => None,
                    };
                    let args: Vec<String> = args.iter().map(|arg| subst(arg, &env, frame)).collect();
                    let key = path_key(&callee);
                    // [R101.4-1] A small callee goes in (no recursion, at most 3 levels).
                    let inline = (depth < MOST_DEPTH && !stack.contains(&key)).then(|| ctx.callees.texts.get(&key)).flatten().cloned();
                    match inline {
                        Some(callee_text) => {
                            // The generic parameters of the callee are the arguments of the call.
                            let names: Vec<String> = generic_names(&callee_text);
                            let given = generic_args(&callee);
                            let map: BTreeMap<String, String> = if names.len() == given.len() { names.into_iter().zip(given).collect() } else { BTreeMap::new() };
                            let callee_mir = parse_mir(&replace_words(&callee_text, &map));
                            ctx.frames += 1;
                            let inner_frame = ctx.frames;
                            let inner_env: BTreeMap<usize, String> = args.iter().enumerate().map(|(at, arg)| (at + 1, arg.clone())).collect();
                            let mut inner_stack = stack.to_vec();
                            inner_stack.push(key.clone());
                            for (inner_path, exit) in run(ctx, &callee_mir, &inner_stack, depth + 1, inner_frame, inner_env, path.clone(), 0) {
                                match (exit, target) {
                                    (Exit::Returned(value), Some(target)) => {
                                        let mut next_env = env.clone();
                                        if let Some(destination) = destination {
                                            next_env.insert(destination, value);
                                        }
                                        todo.push((next_env, inner_path, target));
                                    }
                                    (Exit::Returned(_), None) => out.push((inner_path, Exit::Ended(End::Panic(format!("{} returns", short(&callee)))))),
                                    (Exit::Ended(end), _) => out.push((inner_path, Exit::Ended(end))),
                                }
                            }
                        }
                        None => {
                            let call = format!("{}({})", short(&callee), args.join(", "));
                            match target {
                                Some(target) => {
                                    path.effects.push(format!("call `{call}`"));
                                    if let Some(destination) = destination {
                                        env.insert(destination, format!("{}({})", callee, args.join(", ")));
                                    }
                                    todo.push((env.clone(), path.clone(), target));
                                }
                                None => out.push((path.clone(), Exit::Ended(End::Panic(call)))),
                            }
                        }
                    }
                }
                crate::rewrites::Kind::Stmt | crate::rewrites::Kind::Other => {
                    ctx.error = Some(format!("an end that Semilla does not read: `{text}`"));
                    return Vec::new();
                }
            }
        }
        if out.len() > MOST_PATHS {
            ctx.error = Some(format!("more than {MOST_PATHS} paths"));
            return Vec::new();
        }
    }
    out
}

/// The names of the type parameters of a signature line.
fn generic_names(text: &str) -> Vec<String> {
    let Some(first) = text.lines().find(|line| line.starts_with("fn _(")) else { return Vec::new() };
    let Some(at) = first.rfind(" generics <") else { return Vec::new() };
    let list = first[at + " generics <".len()..].trim_end_matches(" {").trim_end_matches('>');
    list.split(", ").filter_map(|part| part.rsplit_once('#').map(|(name, _)| name.to_string())).filter(|name| !name.is_empty()).collect()
}

/// [R101.5-1] The cases of one closure (its canonical text with the facade removed): each path
/// with its end. `names`: the name of each parameter (`_2` and up). `Err`: the cause of no table.
pub(crate) fn cases(text: &str, names: &[String], callees: &Callees) -> Result<Vec<(Path, End)>, String> {
    let text = crate::rewrites::strip_facade(text);
    let mir = parse_mir(&text);
    let mut env: BTreeMap<usize, String> = BTreeMap::new();
    for arg in 2..=mir.args {
        env.insert(arg, names.get(arg - 2).cloned().unwrap_or_else(|| format!("p{}", arg - 1)));
    }
    let mut ctx = Ctx { callees, error: None, frames: 0 };
    let found = run(&mut ctx, &mir, &[String::new()], 0, 0, env, Path::default(), 0);
    if let Some(error) = ctx.error {
        return Err(error);
    }
    let mut out: Vec<(Path, End)> = found
        .into_iter()
        .map(|(path, exit)| {
            let end = match exit {
                Exit::Returned(value) => End::Return(value),
                Exit::Ended(end) => end,
            };
            (path, end)
        })
        .collect();
    out.sort_by_key(|(path, _)| path.key());
    // Two paths with one key (a decision that is not on a hole and that both paths take): the
    // key says it.
    let keys: Vec<String> = out.iter().map(|(path, _)| path.key()).collect();
    let unique: BTreeSet<&String> = keys.iter().collect();
    if unique.len() != keys.len() {
        return Err("two paths have the same decisions".into());
    }
    Ok(out)
}

/// [R101.6-1] The label of one case: the label and the line of what differs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    pub key: String,
    pub label: &'static str,
    pub text: String,
}

fn end_text(end: &End) -> String {
    match end {
        End::Return(value) => format!("returns `{}`", short(value)),
        End::Unreachable => "`unreachable`".into(),
        End::Panic(call) => format!("a panic (`{call}`)"),
    }
}

/// [R101.6-1] The label of an old path and a new path with one key.
fn label(old: &(Path, End), new: &(Path, End)) -> (&'static str, String) {
    let (old_path, old_end) = old;
    let (new_path, new_end) = new;
    let returns = |end: &End| matches!(end, End::Return(_));
    match (old_end, new_end) {
        (End::Unreachable, End::Unreachable) => return ("undefined on the two sides", "the two paths end in `unreachable`".into()),
        (End::Unreachable | End::Panic(_), End::Return(_)) => return ("accepts more", format!("before: {}; after: {}", end_text(old_end), end_text(new_end))),
        (End::Return(_), End::Unreachable | End::Panic(_)) => return ("accepts less", format!("before: {}; after: {}", end_text(old_end), end_text(new_end))),
        _ => {}
    }
    let mut differs: Vec<String> = Vec::new();
    if old_end != new_end {
        if returns(old_end) && returns(new_end) {
            differs.push(format!("the result: {} → {}", end_text(old_end).trim_start_matches("returns "), end_text(new_end).trim_start_matches("returns ")));
        } else {
            differs.push(format!("the end: {} → {}", end_text(old_end), end_text(new_end)));
        }
    }
    if old_path.effects != new_path.effects {
        let list = |effects: &[String]| if effects.is_empty() { "none".to_string() } else { effects.iter().map(|effect| short(effect)).collect::<Vec<_>>().join(", ") };
        differs.push(format!("the effects: {} → {}", list(&old_path.effects), list(&new_path.effects)));
    }
    if old_path.drops != new_path.drops {
        let list = |drops: &[String]| if drops.is_empty() { "none".to_string() } else { drops.iter().map(|drop| format!("`{}`", short(drop))).collect::<Vec<_>>().join(", ") };
        differs.push(format!("the drops: {} → {}", list(&old_path.drops), list(&new_path.drops)));
    }
    if differs.is_empty() {
        ("equal code", format!("both {}", end_text(old_end)))
    } else {
        ("changed", differs.join("; "))
    }
}

/// [R101.5-1] [R101.6-1] The case table of a pair: one line for each old case and each new case.
pub(crate) fn table(old: &[(Path, End)], new: &[(Path, End)]) -> Vec<Line> {
    let mut out = Vec::new();
    let new_keys: BTreeMap<String, usize> = new.iter().enumerate().map(|(at, (path, _))| (path.key(), at)).collect();
    let mut used: BTreeSet<usize> = BTreeSet::new();
    for case in old {
        let key = case.0.key();
        match new_keys.get(&key) {
            Some(at) => {
                used.insert(*at);
                let (label, text) = label(case, &new[*at]);
                out.push(Line { key, label, text });
            }
            None => out.push(Line { key, label: "removed", text: format!("before: {}", end_text(&case.1)) }),
        }
    }
    for (at, case) in new.iter().enumerate() {
        if !used.contains(&at) {
            out.push(Line { key: case.0.key(), label: "new", text: format!("after: {}", end_text(&case.1)) });
        }
    }
    out
}

/// The count of each label of a table, in the order of the first line of each label: `1 equal
/// code, 1 undefined on the two sides`.
pub(crate) fn counts(lines: &[Line]) -> String {
    let mut order: Vec<&str> = Vec::new();
    for line in lines {
        if !order.contains(&line.label) {
            order.push(line.label);
        }
    }
    order.iter().map(|label| format!("{} {label}", lines.iter().filter(|line| line.label == *label).count())).collect::<Vec<_>>().join(", ")
}

/// The line of one case in the output.
pub(crate) fn line_text(number: usize, line: &Line) -> String {
    let key = if line.key.is_empty() { "each input".to_string() } else { line.key.clone() };
    format!("case {number} ({key}): {} ({})", line.label, line.text)
}
