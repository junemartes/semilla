//! The closure form of `#[rewrite]`, the repeated edits, and the templates (design 97).
//!
//! - [R97.1-1] The closure form: rustc compiles the two closures against the two versions (the
//!   intent crate, design 78). A closure with a capture or with a call of an item of the intent
//!   file is an error of the statement.
//! - [R97.2-1] The element table of each changed body pair (the texts, the ranges, the pairs of
//!   the alignment, the map of the locals) comes from the element compare of design 90.
//! - [R97.2-2] A site: the old closure body fits the old body with each parameter bound, the new
//!   closure body fits the new body at the place that the alignment gives, and each parameter
//!   binds to the same place on the two sides under the map of the locals. The binding comes from
//!   the elements of the site: no search over the bindings.
//! - [R97.2-3] to [R97.2-7] The cases where a site can be false: each one gives no site.
//! - [R97.3-1] One statement gives one entry with its sites ("replace"; a site with equal
//!   compiled code is "same code", split by kind).
//! - [R97.4-1] A node that the pattern does not fit keeps its entry; a place with the old form
//!   and no edit is a count in a note.
//! - [R97.5-1] A statement with no site is false, with the nearest candidates.
//! - [R97.6-1] Two bindings at one site are a tie: no site, and a note.
//! - [R97.7-1] The real edits whose old and new MIR fragments are equal after a rename of the
//!   locals are one entry, "the same edit at N sites" (one pass over the regions: no search).
//! - [R97.8-1] A template of the closure form for each "not mapped" entry, each group, and each
//!   tie; the gate of R87.1-1 checks it before the step prints it.
//!
//! Each rule reads data that the step already has: no solver.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::report::EntryKind;
use semilla_syntax::Workspace;

use crate::diffmap::NodeKind;
use crate::{BodyStatus, MirPlace, MirTable, StepInput, StepOutcome};

/// The name of the phase of design 97 in the timing lines (`SEMILLA_TIMING=1`).
pub const PHASE: &str = "the closure statements, the repeated edits, and their templates (design 97)";

pub const RULE_FORM: &str = "R97.1-1";
pub const RULE_SITE: &str = "R97.2-2";
pub const RULE_TWO_PLACES: &str = "R97.2-3";
pub const RULE_CALLEE: &str = "R97.2-4";
pub const RULE_OPERAND: &str = "R97.2-5";
pub const RULE_MOVED: &str = "R97.2-6";
pub const RULE_MACRO: &str = "R97.2-7";
pub const RULE_ENTRY: &str = "R97.3-1";
pub const RULE_KEPT: &str = "R97.4-1";
pub const RULE_NO_SITE: &str = "R97.5-1";
pub const RULE_TIE: &str = "R97.6-1";
pub const RULE_GROUP: &str = "R97.7-1";
pub const RULE_TEMPLATE: &str = "R97.8-1";

/// The MIR operators whose two operands can swap: the printer gives them an order that does not
/// read the numbers of the locals (design 47), so the order of a closure text does not give the
/// binding of its two operands. The same list as `semilla_normal::elements::SYMMETRIC`.
const SYMMETRIC: &[&str] = &["Add(", "Mul(", "BitAnd(", "BitOr(", "BitXor(", "Eq(", "Ne(", "AddUnchecked(", "MulUnchecked(", "CheckedAdd(", "CheckedMul("];

/// The most symmetric operations with a hole in one closure: each one gives two orders.
const MOST_SYMMETRIC: usize = 4;

/// The most nearest candidates of a statement with no site (R97.5-1).
const MOST_NEAR: usize = 3;

// ---------------------------------------------------------------------------------------------
// The canonical text.

/// One canonical MIR text: the arguments, the return type, the type of each local, and the
/// elements (statements and terminators) with their blocks.
#[derive(Clone, Debug, Default)]
pub(crate) struct Mir {
    pub args: usize,
    pub ret: String,
    pub locals: BTreeMap<usize, String>,
    /// Each element in the order of the text: its text (with no `;`) and its block.
    pub elements: Vec<(String, usize)>,
    /// The elements of each block, in order.
    pub blocks: BTreeMap<usize, Vec<usize>>,
}

impl Mir {
    /// The block and the place in its block of each element.
    fn position(&self, index: usize) -> (usize, usize) {
        let block = self.elements[index].1;
        let at = self.blocks.get(&block).and_then(|list| list.iter().position(|known| *known == index)).unwrap_or(0);
        (block, at)
    }

    /// True for the last element of its block (the terminator).
    fn is_terminator(&self, index: usize) -> bool {
        let block = self.elements[index].1;
        self.blocks.get(&block).and_then(|list| list.last()) == Some(&index)
    }

    /// The type of a local.
    fn local_type(&self, local: usize) -> Option<&str> {
        self.locals.get(&local).map(String::as_str)
    }
}

/// Parses a canonical MIR text (the printer of the driver, design 40.1).
pub(crate) fn parse_mir(text: &str) -> Mir {
    let mut mir = Mir::default();
    let mut block = 0;
    for line in text.lines() {
        if let Some(element) = line.strip_prefix("        ") {
            let index = mir.elements.len();
            mir.elements.push((element.strip_suffix(';').unwrap_or(element).to_string(), block));
            mir.blocks.entry(block).or_default().push(index);
        } else if let Some(rest) = line.strip_prefix("    let _") {
            let (number, ty) = rest.split_once(": ").unwrap_or((rest, ""));
            if let Ok(number) = number.parse::<usize>() {
                mir.locals.insert(number, ty.strip_suffix(';').unwrap_or(ty).to_string());
            }
        } else if let Some(rest) = line.strip_prefix("    bb") {
            block = rest.trim_end_matches(": {").parse().unwrap_or(block);
        } else if let Some(rest) = line.strip_prefix("fn _(") {
            signature(rest, &mut mir);
        }
    }
    mir
}

/// The arguments and the return type of the signature line (after `fn _(`).
fn signature(rest: &str, mir: &mut Mir) {
    let mut depth = 1usize;
    let mut close = rest.len();
    for (at, c) in rest.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' if !(c == '>' && rest[..at].ends_with('-')) => {
                depth -= 1;
                if depth == 0 {
                    close = at;
                    break;
                }
            }
            _ => {}
        }
    }
    let list = &rest[..close];
    let mut count = 0;
    while list.contains(&format!("_{}: ", count + 1)) {
        count += 1;
    }
    mir.args = count;
    for arg in 1..=count {
        let start = format!("_{arg}: ");
        let Some(from) = list.find(&start).map(|at| at + start.len()) else { break };
        let next = format!(", _{}: ", arg + 1);
        let to = if arg < count { list[from..].find(&next).map_or(list.len(), |at| from + at) } else { list.len() };
        mir.locals.insert(arg, list[from..to].to_string());
    }
    let after = rest.get(close + 1..).unwrap_or("");
    let ret = after.strip_prefix(" -> ").unwrap_or("");
    let ret = ret.split(" generics <").next().unwrap_or(ret);
    mir.ret = ret.trim_end_matches(" {").trim().to_string();
    mir.locals.insert(0, mir.ret.clone());
}

fn ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// [R97.1-1] The text of the intent crate in the names of a tree: each version prefix of a path
/// (`before::shop::Cart`) goes (the record names of a tree have no facade).
pub(crate) fn strip_facade(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let start = at == 0 || !(ident_char(bytes[at - 1]) || bytes[at - 1] == b':');
        if start {
            if let Some(word) = ["before::", "after::"].iter().find(|word| text[at..].starts_with(**word)) {
                at += word.len();
                continue;
            }
        }
        let c = text[at..].chars().next().unwrap_or(' ');
        out.push(c);
        at += c.len_utf8();
    }
    out
}

/// [R97.2-4] The crates of the standard library. The printer of the driver gives an item of the
/// standard library the path that the printing crate sees: a crate with `std` prints
/// `std::ops::Range`, a crate with only `core` prints `core::ops::Range` for the same item (a
/// re-export). The checks of a site read each path of these crates with the one name `core`.
const STD_CRATES: &[&str] = &["std", "alloc", "core"];

/// [R97.2-4] A text with each path of a crate of the standard library under the name `core`.
fn std_paths(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let start = at == 0 || !(ident_char(bytes[at - 1]) || bytes[at - 1] == b':');
        if start && let Some(name) = STD_CRATES.iter().find(|name| text[at..].starts_with(&format!("{name}::"))) {
            out.push_str("core::");
            at += name.len() + 2;
            continue;
        }
        let c = text[at..].chars().next().unwrap_or(' ');
        out.push(c);
        at += c.len_utf8();
    }
    out
}

/// The text of an element with no block number of a normal edge and no unwind action: a site of
/// a closure continues at the block of the body, and the cleanup of a panic is the cleanup of
/// the body (as for the inline of R94.5-1).
fn masked(text: &str) -> String {
    let mut out = std_paths(text);
    for word in ["return: bb", "success: bb", "goto -> bb", "unwind: bb"] {
        let mut from = 0;
        while let Some(at) = out[from..].find(word).map(|at| from + at) {
            let digits = at + word.len();
            let end = out[digits..].find(|c: char| !c.is_ascii_digit()).map_or(out.len(), |len| digits + len);
            let replace = if word == "unwind: bb" { "unwind ?".to_string() } else { format!("{word}?") };
            out.replace_range(at..end, &replace);
            from = at + replace.len();
        }
    }
    out.replace("unwind continue", "unwind ?")
}

/// The block of the normal edge of a terminator (`return: bbN`, `success: bbN`, `goto -> bbN`).
fn normal_target(text: &str) -> Option<usize> {
    for word in ["return: bb", "success: bb", "goto -> bb"] {
        if let Some(at) = text.find(word) {
            let digits = &text[at + word.len()..];
            let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
            return digits[..end].parse().ok();
        }
    }
    None
}

/// The locals of a text in order: the start, the end, and the number of each `_N`.
pub(crate) fn locals_in(text: &str) -> Vec<(usize, usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'_' && (at == 0 || !ident_char(bytes[at - 1])) && bytes.get(at + 1).is_some_and(u8::is_ascii_digit) {
            let mut end = at + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end == bytes.len() || !ident_char(bytes[end]) {
                if let Ok(number) = text[at + 1..end].parse() {
                    out.push((at, end, number));
                }
                at = end;
                continue;
            }
        }
        at += 1;
    }
    out
}

/// The text with each local renamed by `map`; `None` if a local has no new name.
fn renamed(text: &str, map: &dyn Fn(usize) -> Option<String>) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, number) in locals_in(text) {
        out.push_str(&text[at..start]);
        out.push_str(&map(number)?);
        at = end;
    }
    out.push_str(&text[at..]);
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// The pattern of one closure.

/// One hole of a pattern: the result (`_0`), a parameter, or a temporary of the closure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Hole {
    Value(usize),
    Temp(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seg {
    Lit(String),
    Hole(Hole),
}

/// One element of a closure chain.
#[derive(Clone, Debug)]
struct Step {
    /// The forms of the element: the text, and each other order of the operands of a symmetric
    /// operation (R97.6-1).
    forms: Vec<Vec<Seg>>,
    text: String,
    terminator: bool,
    /// True if the normal edge goes to the end of the closure (a block with only `return`).
    ends: bool,
}

/// [R97.1-1] The pattern of one closure: its elements on the one path from the start to the
/// return, the types of its parameters, of its result, and of its temporaries.
#[derive(Clone, Debug, Default)]
pub(crate) struct Pattern {
    steps: Vec<Step>,
    /// The parameters (the local `_2` and up), with their types.
    params: BTreeMap<usize, String>,
    ret: String,
    temps: BTreeMap<usize, String>,
}

impl Pattern {
    /// The count of the parameters.
    pub fn arity(&self) -> usize {
        self.params.len()
    }
}

/// The segments of an element text: the text, and a hole for each local.
fn segments(text: &str, args: usize) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut at = 0;
    for (start, end, number) in locals_in(text) {
        if start > at {
            out.push(Seg::Lit(text[at..start].to_string()));
        }
        out.push(Seg::Hole(if number == 0 || (2..=args).contains(&number) { Hole::Value(number) } else { Hole::Temp(number) }));
        at = end;
    }
    if at < text.len() {
        out.push(Seg::Lit(text[at..].to_string()));
    }
    out
}

/// The top-level operands of the text inside a pair of parentheses that starts at `open`, and
/// the place of the closing parenthesis.
fn operands(text: &str, open: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let mut depth = 0usize;
    let mut start = open + 1;
    let mut list = Vec::new();
    for (at, c) in text[open..].char_indices().map(|(at, c)| (open + at, c)) {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    list.push((start, at));
                    return Some((list, at));
                }
            }
            ',' if depth == 1 => {
                list.push((start, at));
                start = at + 2;
            }
            _ => {}
        }
    }
    None
}

/// [R97.6-1] The forms of an element text: the text, and the text with the two operands of each
/// symmetric operation in the other order, when an operand has a local. The operands of MIR are
/// places and constants, so two operations of one element never overlap. Also the count of the
/// symmetric operations with a local.
fn forms(text: &str) -> (Vec<String>, usize) {
    let mut places: Vec<(usize, usize, usize, usize)> = Vec::new();
    for op in SYMMETRIC {
        let mut from = 0;
        while let Some(at) = text[from..].find(op).map(|at| from + at) {
            from = at + op.len();
            if at > 0 && ident_char(text.as_bytes()[at - 1]) {
                continue;
            }
            if let Some((list, _)) = operands(text, at + op.len() - 1)
                && let [(a, b), (c, d)] = list.as_slice()
                && (!locals_in(&text[*a..*b]).is_empty() || !locals_in(&text[*c..*d]).is_empty())
            {
                places.push((*a, *b, *c, *d));
            }
        }
    }
    places.sort_by(|x, y| y.0.cmp(&x.0));
    places.dedup();
    let count = places.len().min(MOST_SYMMETRIC + 1);
    if places.len() > MOST_SYMMETRIC {
        return (vec![text.to_string()], count);
    }
    let mut out = Vec::new();
    for mask in 0..(1usize << places.len()) {
        // From the right to the left: a swap keeps the places of the operations on its left.
        let mut form = text.to_string();
        for (bit, (a, b, c, d)) in places.iter().enumerate() {
            if mask & (1 << bit) != 0 {
                form = format!("{}{}, {}{}", &form[..*a], &form[*c..*d], &form[*a..*b], &form[*d..]);
            }
        }
        out.push(form);
    }
    out.sort();
    out.dedup();
    (out, count)
}

/// [R97.1-1] The pattern of a closure from its canonical text. `Err`: the cause why the closure
/// is no pattern (an error of the statement).
pub(crate) fn pattern(text: &str) -> Result<Pattern, String> {
    if text.contains(&format!("{CLAIMS_CRATE}::")) {
        return Err(format!("the closure calls an item of the intent file: a closure reads only its parameters ({RULE_FORM})"));
    }
    let stripped = strip_facade(text);
    let mir = parse_mir(&stripped);
    if mir.args == 0 {
        return Err("the text of the closure has no signature".into());
    }
    if mir.elements.iter().any(|(element, _)| locals_in(element).iter().any(|(_, _, local)| *local == 1)) {
        return Err(format!("the closure reads a capture: a closure reads only its parameters ({RULE_FORM})"));
    }
    let params: BTreeMap<usize, String> = (2..=mir.args).map(|local| (local, mir.local_type(local).unwrap_or("").to_string())).collect();
    let temps: BTreeMap<usize, String> = mir.locals.iter().filter(|(local, _)| **local > mir.args).map(|(local, ty)| (*local, ty.clone())).collect();
    let only_return = |block: usize| mir.blocks.get(&block).is_some_and(|list| list.len() == 1 && mir.elements[list[0]].0 == "return");
    let mut steps = Vec::new();
    let mut block = 0;
    let mut seen = BTreeSet::new();
    let mut symmetric = 0;
    loop {
        if !seen.insert(block) {
            return Err("the closure body has a loop".into());
        }
        let list = mir.blocks.get(&block).cloned().unwrap_or_default();
        let Some((&last, statements)) = list.split_last() else { return Err("the closure body has an empty block".into()) };
        for &index in statements {
            let text = &mir.elements[index].0;
            steps.push(step(text, false, false, mir.args, &mut symmetric)?);
        }
        let text = &mir.elements[last].0;
        if text == "return" {
            break;
        }
        if text.starts_with("goto -> ") {
            block = normal_target(text).ok_or("a jump with no target")?;
            continue;
        }
        if text.starts_with("switchInt(") || !text.contains(" -> [") {
            return Err(format!("the closure body has a branch or an end that is not a return (`{text}`): the closure form reads one path"));
        }
        let target = normal_target(text).ok_or_else(|| format!("the element `{text}` has no normal edge"))?;
        let ends = only_return(target);
        steps.push(step(text, true, ends, mir.args, &mut symmetric)?);
        if ends {
            break;
        }
        block = target;
    }
    if steps.is_empty() {
        return Err("the closure body has no element".into());
    }
    Ok(Pattern { steps, params, ret: mir.ret.clone(), temps })
}

fn step(text: &str, terminator: bool, ends: bool, args: usize, symmetric: &mut usize) -> Result<Step, String> {
    let text = masked(text);
    let (forms, count) = forms(&text);
    *symmetric += count;
    if *symmetric > MOST_SYMMETRIC {
        return Err(format!("the closure has more than {MOST_SYMMETRIC} symmetric operations with a parameter or a temporary: the check of the binding of their operands is too large ({RULE_TIE})"));
    }
    Ok(Step { forms: forms.iter().map(|form| segments(form, args)).collect(), text, terminator, ends })
}

/// The name of the intent crate (design 78) in a record name.
const CLAIMS_CRATE: &str = "semilla_claims";

// ---------------------------------------------------------------------------------------------
// The binding and the match.

/// The binding of the holes of a pattern to the body: each value hole (the result and the
/// parameters) to one operand text, and each temporary to one local of the body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Binding {
    values: BTreeMap<usize, String>,
    temps: BTreeMap<usize, usize>,
}

/// The local of a body text at a place, and its length.
fn local_at(text: &str, at: usize) -> Option<(usize, usize)> {
    let (start, end, number) = locals_in(&text[at..]).into_iter().next()?;
    (start == 0).then_some((number, end))
}

/// [R97.2-5] The end of one operand that starts at `at`: the shortest text with balanced brackets
/// and no comma at its top level that the next literal follows (or the end of the text).
fn operand_end(text: &str, at: usize, next: Option<&str>) -> Option<usize> {
    let mut depth = 0i32;
    for (offset, c) in text[at..].char_indices() {
        let place = at + offset;
        if depth == 0 && offset > 0 && next.is_some_and(|literal| text[place..].starts_with(literal)) {
            return Some(place);
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            ',' if depth == 0 => return None,
            _ => {}
        }
    }
    (next.is_none() && depth == 0 && text.len() > at).then_some(text.len())
}

/// Unifies the segments of one closure element with one body element. No search: each hole takes
/// the text that the next literal gives.
fn unify(segs: &[Seg], text: &str, binding: &Binding) -> Option<Binding> {
    let mut out = binding.clone();
    let mut at = 0;
    for (k, seg) in segs.iter().enumerate() {
        match seg {
            Seg::Lit(literal) => {
                if !text[at..].starts_with(literal.as_str()) {
                    return None;
                }
                at += literal.len();
            }
            Seg::Hole(Hole::Temp(number)) => {
                let (local, len) = local_at(text, at)?;
                match out.temps.get(number) {
                    Some(known) if *known != local => return None,
                    Some(_) => {}
                    None => {
                        if out.temps.values().any(|known| *known == local) {
                            return None;
                        }
                        out.temps.insert(*number, local);
                    }
                }
                at += len;
            }
            Seg::Hole(Hole::Value(number)) => match out.values.get(number) {
                Some(known) => {
                    if !text[at..].starts_with(known.as_str()) {
                        return None;
                    }
                    let end = at + known.len();
                    if known.ends_with(|c: char| c.is_ascii_digit()) && text[end..].starts_with(|c: char| c.is_ascii_digit()) {
                        return None;
                    }
                    at = end;
                }
                None => {
                    let next = match segs.get(k + 1) {
                        Some(Seg::Lit(literal)) => Some(literal.as_str()),
                        None => None,
                        Some(Seg::Hole(_)) => return None,
                    };
                    let end = operand_end(text, at, next)?;
                    out.values.insert(*number, text[at..end].to_string());
                    at = end;
                }
            },
        }
    }
    (at == text.len()).then_some(out)
}

/// One match of a pattern in a body: the elements (in the order of the run) and the binding.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Found {
    elements: Vec<usize>,
    binding: Binding,
}

/// How far a pattern fits at one start: the count of its elements that fit, and the first
/// element of each side that differs (R97.5-1).
#[derive(Clone, Debug, Default)]
struct Near {
    fits: usize,
    body: String,
    closure: String,
}

/// The matches of a pattern that start at one element of a body: each form of the symmetric
/// operations gives at most one match. The run follows the normal edge of each terminator; a
/// `goto` of the body is glue.
fn matches_at(body: &Mir, pattern: &Pattern, start: usize, binding: &Binding, near: &mut Near) -> Vec<Found> {
    let mut out = Vec::new();
    let mut todo: Vec<(usize, Option<usize>, Binding, Vec<usize>)> = vec![(0, Some(start), binding.clone(), Vec::new())];
    while let Some((k, at, bound, mut elements)) = todo.pop() {
        if k == pattern.steps.len() {
            out.push(Found { elements, binding: bound });
            continue;
        }
        let Some(mut at) = at else { continue };
        // A `goto` of the body is glue between two elements of the run.
        let mut hops = 0;
        while body.elements[at].0.starts_with("goto -> ") && hops < 8 {
            let Some(first) = normal_target(&body.elements[at].0).and_then(|block| body.blocks.get(&block)).and_then(|list| list.first().copied()) else { break };
            elements.push(at);
            at = first;
            hops += 1;
        }
        let step = &pattern.steps[k];
        if body.is_terminator(at) != step.terminator {
            if k >= near.fits {
                *near = Near { fits: k, body: body.elements[at].0.clone(), closure: step.text.clone() };
            }
            continue;
        }
        let text = masked(&body.elements[at].0);
        let mut any = false;
        for form in &step.forms {
            let Some(next_binding) = unify(form, &text, &bound) else { continue };
            any = true;
            let mut next_elements = elements.clone();
            next_elements.push(at);
            let next = if !step.terminator {
                let (block, place) = body.position(at);
                body.blocks.get(&block).and_then(|list| list.get(place + 1)).copied()
            } else if step.ends {
                None
            } else {
                normal_target(&body.elements[at].0).and_then(|block| body.blocks.get(&block)).and_then(|list| list.first().copied())
            };
            if k + 1 < pattern.steps.len() && next.is_none() {
                continue;
            }
            todo.push((k + 1, next, next_binding, next_elements));
        }
        if !any && k >= near.fits {
            *near = Near { fits: k, body: body.elements[at].0.clone(), closure: step.text.clone() };
        }
    }
    out.sort_by(|a, b| a.elements.cmp(&b.elements).then_with(|| format!("{:?}", a.binding).cmp(&format!("{:?}", b.binding))));
    out.dedup();
    out
}

/// [R97.2-6] The type of an operand of a body: a local, a place with its type, or a constant.
fn operand_type(body: &Mir, text: &str) -> Option<String> {
    let text = text.strip_prefix("move ").unwrap_or(text);
    if let Some((local, len)) = local_at(text, 0)
        && len == text.len()
    {
        return body.local_type(local).map(str::to_string);
    }
    if let Some(rest) = text.strip_prefix("const ") {
        return rest.split_once(" = ").map(|(ty, _)| ty.to_string());
    }
    if text.starts_with('(') && text.ends_with(')') {
        // `((*_1).shop::Cart.items: std::vec::Vec<u32>)`: the type after the last `: ` of the
        // top level of the parentheses.
        let inner = &text[1..text.len() - 1];
        let mut depth = 0i32;
        let mut last = None;
        for (at, c) in inner.char_indices() {
            match c {
                '(' | '[' | '{' | '<' => depth += 1,
                ')' | ']' | '}' | '>' => depth -= 1,
                ':' if depth == 0 && inner[at..].starts_with(": ") && !inner[..at].ends_with(':') => last = Some(at),
                _ => {}
            }
        }
        return last.map(|at| inner[at + 2..].to_string());
    }
    None
}

/// The check of one match: the types of the binding (R97.2-6), and the temporaries of the match
/// that the body reads only inside the match (R97.2-5). `Err`: the cause.
fn check_found(body: &Mir, pattern: &Pattern, found: &Found) -> Result<(), String> {
    for (hole, value) in &found.binding.values {
        let want = if *hole == 0 { Some(&pattern.ret) } else { pattern.params.get(hole) };
        let have = operand_type(body, value);
        match (want, have) {
            (Some(want), Some(have)) if std_paths(want) == std_paths(&have) => {}
            (Some(want), Some(have)) => return Err(format!("the binding `{value}` has the type `{have}`, and the closure has `{want}` ({RULE_MOVED})")),
            (_, None) => return Err(format!("the type of the binding `{value}` is not known ({RULE_MOVED})")),
            (None, _) => return Err(format!("the binding `{value}` has no hole of the closure")),
        }
    }
    let inside: BTreeSet<usize> = found.elements.iter().copied().collect();
    let in_values: BTreeSet<usize> = found.binding.values.values().flat_map(|value| locals_in(value).into_iter().map(|(_, _, local)| local)).collect();
    for (temp, local) in &found.binding.temps {
        if in_values.contains(local) {
            return Err(format!("the local `_{local}` of the body is a temporary of the closure and a part of a binding ({RULE_OPERAND})"));
        }
        if pattern.temps.get(temp).map(|ty| std_paths(ty)) != body.local_type(*local).map(std_paths) {
            return Err(format!("the temporary `_{local}` of the body has another type than the temporary of the closure ({RULE_MOVED})"));
        }
        let outside = body.elements.iter().enumerate().any(|(index, (text, _))| !inside.contains(&index) && locals_in(text).iter().any(|(_, _, number)| number == local));
        if outside {
            return Err(format!("the body reads the temporary `_{local}` of the site at an other place ({RULE_OPERAND})"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The sites of a statement.

/// One site of a statement (R97.2-2).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Site {
    pub body: String,
    /// The file and the line of the site in the old tree.
    pub file: String,
    pub line: usize,
    /// True if the compiled code of the site is equal under the binding (R97.3-1).
    pub same: bool,
    /// The binding of each parameter: the name in the closure and the text of the old body.
    pub binding: Vec<(String, String)>,
    /// The element places of the site on each side.
    pub places: [BTreeSet<MirPlace>; 2],
}

/// The result of the check of one statement.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Checked {
    pub file: String,
    pub function: String,
    /// The scope (a record name), if the statement has one.
    pub scope: Option<String>,
    pub sites: Vec<Site>,
    /// An error of the statement (R97.1-1), or the cause of no site (R97.5-1).
    pub error: Option<String>,
    /// The nearest candidates of a statement with no site (R97.5-1).
    pub nearest: Vec<String>,
    /// The places of the scope that keep the old form with no edit (R97.4-1).
    pub kept: Vec<String>,
    /// The ties (R97.6-1) and the sites in a macro expansion (R97.2-7), and the other places that
    /// the check refuses, with the cause.
    pub refused: Vec<String>,
    /// The edits of the nodes of the entry of the statement.
    pub edits: Vec<usize>,
}

impl Checked {
    /// The text of the fact of the agent.
    pub fn text(&self) -> String {
        format!("rewrite `fn {}` (the closure form, {})", self.function, self.file)
    }

    /// The key of the fact of the agent: the statement in its file.
    pub fn option(&self) -> String {
        format!("#[rewrite] fn {} ({})", self.function, self.file)
    }
}

/// The plan of the statements and of the repeated edits of a step (design 97).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Plan {
    pub statements: Vec<Checked>,
    /// [R97.7-1] The groups of the repeated edits: the edits of the nodes of each site.
    pub groups: Vec<Group>,
    /// [R97.8-1] The template of the closure form for an edit (the first node of an entry), or
    /// the cause of no template.
    pub templates: Vec<(usize, Result<String, String>)>,
}

/// One group of the repeated edits (R97.7-1).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Group {
    /// The sites: the body, the file and the line, and the edits of the nodes.
    pub sites: Vec<(String, String, usize, Vec<usize>)>,
    pub template: Option<Result<String, String>>,
}

/// The source order of the elements of one side of a table: the start of the place, the longer
/// place first, then the order of the text.
fn source_order(table: &MirTable, side: usize) -> Vec<usize> {
    let mut list: Vec<usize> = (0..table.ranges[side].len()).collect();
    list.sort_by_key(|index| match &table.ranges[side][*index].0 {
        Some((file, start, end)) => (file.clone(), *start, usize::MAX - *end, *index),
        None => (String::new(), usize::MAX, 0, *index),
    });
    list
}

/// The body pairs of a step with a table: the comparison and its table.
fn tables(outcome: &StepOutcome) -> Vec<(BodyName, std::sync::Arc<MirTable>)> {
    outcome
        .bodies
        .iter()
        .filter(|body| matches!(body.status, BodyStatus::Different { .. }))
        .filter_map(|body| Some((BodyName { name: body.name.clone() }, body.elements.as_ref()?.table.clone()?)))
        .collect()
}

/// The name of a changed body with a table.
struct BodyName {
    name: String,
}

/// The line of the first place of a list of elements of one side.
fn line_at(tree: &Workspace, table: &MirTable, side: usize, elements: &[usize]) -> (String, usize) {
    for index in elements {
        if let Some((file, start, _)) = table.ranges[side].get(*index).and_then(|(tokens, _)| tokens.clone()) {
            return (file.clone(), crate::transforms::line_of(tree, &file, start).unwrap_or(0));
        }
    }
    (String::new(), 0)
}

/// The text of a binding for the reader: the name of a variable of the source, else the MIR
/// text.
fn binding_text(tree: &Workspace, table: &MirTable, side: usize, value: &str) -> String {
    if let Some((local, len)) = local_at(value, 0)
        && len == value.len()
        && let Some((kind, Some((file, start, end)))) = table.decls[side].get(local)
        && (*kind == 1 || *kind == 2)
        && let Some(text) = tree.rust_files.get(file).and_then(|text| text.get(*start..*end))
    {
        return format!("`{}`", text.trim_start_matches('&').trim_start_matches("mut ").trim());
    }
    format!("`{value}`")
}

/// The old range and the new range of the aligned region of a run of old elements: the new
/// elements whose places are between the partners of the equal pairs around the run.
fn new_range(table: &MirTable, old: &[usize]) -> Vec<usize> {
    let order = [source_order(table, 0), source_order(table, 1)];
    let rank = |side: usize| -> BTreeMap<usize, usize> { order[side].iter().enumerate().map(|(rank, index)| (*index, rank)).collect() };
    let (old_rank, new_rank) = (rank(0), rank(1));
    let inside: BTreeSet<usize> = old.iter().copied().collect();
    let low = old.iter().filter_map(|index| old_rank.get(index)).min().copied().unwrap_or(0);
    let high = old.iter().filter_map(|index| old_rank.get(index)).max().copied().unwrap_or(0);
    let equal: Vec<(usize, usize)> = table.pairs.iter().filter(|(a, _, same)| *same && !inside.contains(a)).map(|(a, b, _)| (old_rank[a], new_rank[b])).collect();
    let before = equal.iter().filter(|(a, _)| *a < low).map(|(_, b)| *b).max();
    let after = equal.iter().filter(|(a, _)| *a > high).map(|(_, b)| *b).min();
    order[1].iter().enumerate().filter(|(rank, _)| before.is_none_or(|b| *rank > b) && after.is_none_or(|a| *rank < a)).map(|(_, index)| *index).collect()
}

/// [R97.2-3] The binding of the new side: each value of the old binding with the locals of the
/// old body renamed by the map of the body. `Err`: a local with no partner.
fn mapped_binding(table: &MirTable, old: &Binding) -> Result<Binding, String> {
    let map: BTreeMap<usize, usize> = table.locals.iter().copied().collect();
    let mut out = Binding::default();
    for (hole, value) in &old.values {
        let renamed = renamed(value, &|local| map.get(&local).map(|new| format!("_{new}"))).ok_or_else(|| format!("the binding `{value}` names a local of the old body with no partner in the new body ({RULE_TWO_PLACES})"))?;
        out.values.insert(*hole, renamed);
    }
    Ok(out)
}

/// True if an element of a list comes from a macro expansion (R97.2-7).
fn in_macro(table: &MirTable, side: usize, elements: &[usize]) -> bool {
    elements.iter().any(|index| table.ranges[side].get(*index).is_some_and(|(_, call)| call.is_some()))
}

/// The places of a list of elements of one side.
fn places_of(table: &MirTable, side: usize, elements: &[usize]) -> BTreeSet<MirPlace> {
    elements.iter().filter_map(|index| table.ranges[side].get(*index).and_then(|(tokens, _)| tokens.clone())).collect()
}

/// The closure forms of `#[rewrite]` of a step: the file, the function, the scope (a record
/// name), and the closure parameter names.
fn statements(input: &StepInput) -> Vec<(String, String, Option<String>, Vec<String>)> {
    let mut out = Vec::new();
    for file in &input.step.agent.intent {
        let (heads, _) = semilla_intent::heads::check(&file.path, &file.text);
        for head in heads.iter().filter(|head| head.closure && head.kind == "rewrite") {
            let scope = head.scope.first().map(|scope| semilla_intent::heads::record_text(scope));
            let names = head.closures.first().map(|(names, _)| names.clone()).unwrap_or_default();
            out.push((file.path.clone(), head.function.clone(), scope, names));
        }
    }
    out
}

/// [R97.2-2] [R97.3-1] [R97.4-1] [R97.5-1] [R97.6-1] The check of each closure form of a step,
/// and the kinds of the nodes of its sites.
pub(crate) fn plan(input: &StepInput, outcome: &mut StepOutcome, closures: &BTreeMap<(String, String), [String; 2]>) {
    let trees = [input.before, input.after];
    let mut checked = Vec::new();
    // [R97.8-1] The templates of the ties: the statement, the body, and the template.
    let mut ties: Vec<(String, String, Result<String, String>)> = Vec::new();
    for (file, function, scope, names) in statements(input) {
        let mut result = Checked { file: file.clone(), function: function.clone(), scope: scope.clone(), ..Default::default() };
        let Some(texts) = closures.get(&(file.clone(), function.clone())) else {
            result.error = Some(format!("the intent crate gives no compiled closures for `fn {function}` ({RULE_FORM})"));
            checked.push(result);
            continue;
        };
        let patterns = match (pattern(&texts[0]), pattern(&texts[1])) {
            (Ok(old), Ok(new)) if old.arity() == new.arity() => [old, new],
            (Ok(old), Ok(new)) => {
                result.error = Some(format!("the old closure has {} parameter(s) and the new closure has {} ({RULE_FORM})", old.arity(), new.arity()));
                checked.push(result);
                continue;
            }
            (Err(cause), _) => {
                result.error = Some(format!("the old closure: {cause}"));
                checked.push(result);
                continue;
            }
            (_, Err(cause)) => {
                result.error = Some(format!("the new closure: {cause}"));
                checked.push(result);
                continue;
            }
        };
        let bodies: Vec<(BodyName, std::sync::Arc<MirTable>)> = tables(outcome).into_iter().filter(|(body, _)| scope.as_ref().is_none_or(|scope| body.name == *scope)).collect();
        if bodies.is_empty() {
            result.error = Some(match &scope {
                Some(scope) => format!("the scope `{scope}` names no changed body with element data ({RULE_NO_SITE})"),
                None => format!("the step has no changed body with element data ({RULE_NO_SITE})"),
            });
            checked.push(result);
            continue;
        }
        let mut nearest: Vec<(usize, String)> = Vec::new();
        for (body, table) in bodies {
            let table: &MirTable = &table;
            let mirs = [parse_mir(&table.texts[0]), parse_mir(&table.texts[1])];
            let mut used: BTreeSet<usize> = BTreeSet::new();
            for start in 0..mirs[0].elements.len() {
                if used.contains(&start) {
                    continue;
                }
                let mut near = Near::default();
                let found: Vec<Found> = matches_at(&mirs[0], &patterns[0], start, &Binding::default(), &mut near).into_iter().filter(|found| check_found(&mirs[0], &patterns[0], found).is_ok()).collect();
                let (file_at, line) = line_at(trees[0], table, 0, &[start]);
                let place = format!("`{}` ({file_at}:{line})", body.name);
                if found.is_empty() {
                    if near.fits > 0 {
                        nearest.push((near.fits, format!("{place}: {} of {} element(s) fit; the element that differs: the body has `{}`, the closure has `{}`", near.fits, patterns[0].steps.len(), near.body, near.closure)));
                    }
                    continue;
                }
                // The elements of the matches: a later start inside them is no new site.
                used.extend(found.iter().flat_map(|found| found.elements.iter().copied()));
                // [R97.2-7] A site in a macro expansion.
                if found.iter().any(|found| in_macro(table, 0, &found.elements)) {
                    result.refused.push(format!("{place}: the old code is a part of a macro expansion ({RULE_MACRO}): no site"));
                    continue;
                }
                // [R97.2-3] Each binding of the old side, and the matches of the new closure with
                // that binding at the aligned place of the new body.
                let partners: BTreeSet<usize> = table.locals.iter().map(|(_, new)| *new).collect();
                let mut fits: Vec<(Found, Vec<Found>)> = Vec::new();
                let mut new_near = Near::default();
                let mut unmapped: Option<String> = None;
                for old in &found {
                    let bound = match mapped_binding(table, &old.binding) {
                        Ok(bound) => bound,
                        Err(cause) => {
                            unmapped.get_or_insert(cause);
                            continue;
                        }
                    };
                    let mut new_found: Vec<Found> = Vec::new();
                    for start in new_range(table, &old.elements) {
                        for found in matches_at(&mirs[1], &patterns[1], start, &bound, &mut new_near) {
                            // [R97.2-3] A parameter that only the new closure reads binds to a place
                            // of the new body whose locals each have an old partner: the place is in
                            // the two bodies.
                            let in_both = |value: &String| locals_in(value).iter().all(|(_, _, local)| partners.contains(local));
                            // (The match keeps each value of the given binding.)
                            let fits = found.binding.values.iter().all(|(hole, value)| bound.values.contains_key(hole) || in_both(value));
                            if check_found(&mirs[1], &patterns[1], &found).is_ok() && fits && !new_found.contains(&found) {
                                new_found.push(found);
                            }
                        }
                    }
                    fits.push((old.clone(), new_found));
                }
                let winners: Vec<&(Found, Vec<Found>)> = fits.iter().filter(|(_, new)| !new.is_empty()).collect();
                let all_equal = found[0].elements.iter().all(|index| table.same[0].get(*index).copied().unwrap_or(false));
                // [R97.6-1] Two bindings that fit the two sides at one site are a tie.
                let bindings: BTreeSet<Vec<(usize, String)>> = winners.iter().map(|(old, _)| old.binding.values.iter().filter(|(hole, _)| **hole != 0).map(|(hole, value)| (*hole, value.clone())).collect()).collect();
                if bindings.len() > 1 || winners.iter().any(|(_, new)| new.len() > 1) {
                    let count = bindings.len().max(winners.iter().map(|(_, new)| new.len()).max().unwrap_or(0));
                    let what = if bindings.len() > 1 { "bindings of the closure pair" } else { "places of the new closure in the aligned region" };
                    result.refused.push(format!("{place}: a tie of {count} {what} at one site ({RULE_TIE}): no site, and Semilla does not use the order"));
                    let (old, new) = winners[0];
                    ties.push((function.clone(), body.name.clone(), closure_template(input, table, &body.name, [&old.elements, &new[0].elements], Some(&body.name))));
                    continue;
                }
                let Some((old, new)) = winners.first().map(|(old, new)| (old.clone(), new[0].clone())) else {
                    if all_equal {
                        result.kept.push(place);
                    } else if let Some(cause) = unmapped {
                        nearest.push((patterns[0].steps.len(), format!("{place}: the old closure fits; {cause}")));
                    } else {
                        nearest.push((patterns[0].steps.len() + new_near.fits, format!("{place}: the old closure fits; at the aligned place of the new body {} of {} element(s) of the new closure fit; the element that differs: the body has `{}`, the closure has `{}` ({RULE_TWO_PLACES})", new_near.fits, patterns[1].steps.len(), new_near.body, new_near.closure)));
                    }
                    continue;
                };
                if in_macro(table, 1, &new.elements) {
                    result.refused.push(format!("{place}: the new code is a part of a macro expansion ({RULE_MACRO}): no site"));
                    continue;
                }
                let equal_pairs: BTreeSet<(usize, usize)> = table.pairs.iter().filter(|(_, _, same)| *same).map(|(a, b, _)| (*a, *b)).collect();
                let same = old.elements.len() == new.elements.len() && old.elements.iter().all(|a| new.elements.iter().any(|b| equal_pairs.contains(&(*a, *b))));
                let places = [places_of(table, 0, &old.elements), places_of(table, 1, &new.elements)];
                let binding: Vec<(String, String)> = patterns[0].params.keys().filter_map(|hole| Some((names.get(hole - 2).cloned().unwrap_or_else(|| format!("p{}", hole - 1)), binding_text(trees[0], table, 0, old.binding.values.get(hole)?)))).collect();
                let site = Site { body: body.name.clone(), file: file_at.clone(), line, same, binding, places };
                if same && !site_has_edit(outcome, trees, &site) {
                    result.kept.push(place);
                    continue;
                }
                result.sites.push(site);
            }
        }
        if result.sites.is_empty() && result.error.is_none() {
            nearest.sort_by(|a, b| b.0.cmp(&a.0));
            result.nearest = nearest.into_iter().take(MOST_NEAR).map(|(_, text)| text).collect();
            let refused = match result.refused.first() {
                Some(first) => format!("; the check refuses {} place(s): {first}", result.refused.len()),
                None => String::new(),
            };
            result.error = Some(format!("no site: no changed body of the scope has the old closure body with a binding whose new code is the new closure body ({RULE_NO_SITE}){refused}"));
        }
        checked.push(result);
    }
    // [R97.3-1] The nodes of the sites of each statement: one entry, one kind for each site.
    let nodes: Vec<usize> = (0..outcome.edits.len()).filter(|index| crate::astmap::is_node(&outcome.edits[*index])).collect();
    for (number, result) in checked.iter_mut().enumerate() {
        let mut edits: Vec<(usize, bool)> = Vec::new();
        for site in &result.sites {
            for edit in &nodes {
                if outcome.transforms.nodes.contains_key(edit) || edits.iter().any(|(known, _)| known == edit) {
                    continue;
                }
                if !in_body(outcome, *edit, &site.body) {
                    continue;
                }
                let empty = [BTreeSet::new(), BTreeSet::new()];
                let places = outcome.report.node_mir.get(edit).map_or(&empty, |mir| &mir.places);
                let owned = !places[0].is_empty() || !places[1].is_empty();
                // A node of a site is an edit of code ("replace", or "same code" for a site with
                // equal code): a type change, an addition, and a removal keep their own entries.
                let fact = outcome.fact_of.get(*edit).copied().flatten().and_then(|fact| outcome.report.facts.get(fact));
                let kind = crate::diffmap::node_kind(&outcome.edits[*edit], outcome.report.node_mir.get(edit), fact, trees);
                let inside = owned && kind.kind == EntryKind::Replace && !kind.syntax_only && (0..2).all(|side| places[side].is_subset(&site.places[side]));
                let same_node = site.same && !owned && kind.kind == EntryKind::SameCode && node_holds(outcome, trees, *edit, site);
                if inside || same_node {
                    edits.push((*edit, site.same && !owned));
                }
            }
        }
        let list: Vec<String> = result.sites.iter().map(|site| format!("`{}` ({}:{})", site.body, site.file, site.line)).collect();
        let binding: Vec<String> = result.sites.first().map(|site| site.binding.iter().map(|(name, value)| format!("the parameter `{name}` binds to {value}")).collect()).unwrap_or_default();
        let binding = if binding.is_empty() { String::new() } else { format!("; at the first site {}", binding.join(", ")) };
        for (edit, same) in &edits {
            let (kind, words) = if *same { (EntryKind::SameCode, "the compiled code of the site is equal under the binding") } else { (EntryKind::Replace, "a real edit that the statement states") };
            let check = format!("the closure pair of `fn {}` fits {} site(s): {}{binding}; {words} (statement {})", result.function, result.sites.len(), list.join(", "), number + 1);
            outcome.transforms.nodes.insert(*edit, NodeKind { kind, syntax_only: false, rule: RULE_ENTRY, check, order: None });
        }
        if let Some((first, _)) = edits.first() {
            for (edit, _) in edits.iter().skip(1) {
                outcome.transforms.joins.push((*edit, *first));
            }
        }
        result.edits = edits.into_iter().map(|(edit, _)| edit).collect();
        for kept in [&result.kept] {
            if !kept.is_empty() {
                let note = format!("`#[rewrite] fn {}`: {} place(s) in the scope keep the old form with no edit ({RULE_KEPT}): {}", result.function, kept.len(), kept.iter().take(5).cloned().collect::<Vec<_>>().join(", "));
                outcome.report.notes.push(note);
            }
        }
        for refused in &result.refused {
            outcome.report.notes.push(format!("`#[rewrite] fn {}`: {refused}", result.function));
        }
    }
    for (function, body, template) in ties {
        let text = match template {
            Ok(template) => {
                outcome.templates.notes.push(template.clone());
                crate::template::note_template(&template)
            }
            Err(cause) => format!("no template: {cause}"),
        };
        outcome.report.notes.push(format!("`#[rewrite] fn {function}`: the statement of the tie at `{body}` ({RULE_TEMPLATE}): {text}"));
    }
    outcome.transforms.rewrites.statements = checked;
}

/// True if a node of the body has a syntax edit at the old places of a site with equal compiled
/// code (R97.3-1): the site is a "same code" site, not a place that keeps the old form.
fn site_has_edit(outcome: &StepOutcome, trees: [&Workspace; 2], site: &Site) -> bool {
    (0..outcome.edits.len()).filter(|index| crate::astmap::is_node(&outcome.edits[*index])).any(|edit| in_body(outcome, edit, &site.body) && node_holds(outcome, trees, edit, site))
}

/// True if a node is a node of a body: its MIR elements are in the body, or (a node with no
/// element that differs) its item is the body.
fn in_body(outcome: &StepOutcome, edit: usize, body: &str) -> bool {
    outcome.report.node_mir.get(&edit).is_some_and(|mir| mir.bodies.contains(body)) || outcome.edits[edit].before.as_ref().is_some_and(|anchor| anchor.item == body)
}

/// True if the old range of a node holds each old place of a site.
fn node_holds(outcome: &StepOutcome, trees: [&Workspace; 2], edit: usize, site: &Site) -> bool {
    let Some(anchor) = outcome.edits[edit].before.as_ref() else { return false };
    let Some((file, start, end)) = crate::records::anchor_place(trees[0], anchor) else { return false };
    !site.places[0].is_empty() && site.places[0].iter().all(|(at, from, to)| *at == file && start <= *from && *to <= end)
}

/// [R97.3-1] [R97.4-1] The nodes of each statement make an entry of their own: a fact of the
/// syntax diff with nodes of a statement and other nodes splits, so a node that the pattern does
/// not fit keeps its own entry (after the shape of the entries).
pub(crate) fn split_statements(outcome: &mut StepOutcome, trees: [&Workspace; 2], map: &mut crate::astmap::AstMapRef<'_>, indices: &[usize]) {
    let moved: Vec<BTreeSet<usize>> = outcome.transforms.rewrites.statements.iter().map(|statement| statement.edits.iter().copied().collect()).filter(|set: &BTreeSet<usize>| !set.is_empty()).collect();
    for set in moved {
        crate::diffmap::split_off(outcome, trees, map.0, indices, &set);
    }
}

// ---------------------------------------------------------------------------------------------
// The repeated edits.

/// [R97.7-1] The key of a fragment pair: the old elements with the locals in the order of their
/// first use, the new elements with the partner of each old local under the map of the body and
/// a new name for each other local, and the types of the locals. Two fragments with one key are
/// the same edit (the callees, the constants, the fields, and the types are a part of the text).
pub(crate) fn fragment_key(mirs: &[Mir; 2], table: &MirTable, old: &[usize], new: &[usize]) -> String {
    let map: BTreeMap<usize, usize> = table.locals.iter().copied().collect();
    let mut names: [BTreeMap<usize, String>; 2] = Default::default();
    let mut types: Vec<String> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    for (side, list) in [(0, old), (1, new)] {
        for index in list {
            let text = masked(&mirs[side].elements[*index].0);
            for (_, _, local) in locals_in(&text) {
                if names[side].contains_key(&local) {
                    continue;
                }
                let name = if side == 1
                    && let Some(old_local) = map.iter().find(|(_, b)| **b == local).map(|(a, _)| *a)
                    && let Some(known) = names[0].get(&old_local)
                {
                    known.clone()
                } else {
                    let name = format!("{}{}", if side == 0 { "L" } else { "N" }, names[side].len());
                    types.push(format!("{name}: {}", mirs[side].local_type(local).unwrap_or("?")));
                    name
                };
                names[side].insert(local, name);
            }
            let renamed = renamed(&text, &|local| names[side].get(&local).cloned()).unwrap_or(text.clone());
            texts.push(format!("{side}|{renamed}"));
        }
    }
    format!("{}\n{}", texts.join("\n"), types.join(", "))
}

/// [R97.7-1] The regions of a body pair: the elements that differ between two equal pairs of the
/// alignment (in the order of the source), with elements on the two sides.
fn regions(table: &MirTable) -> Vec<[Vec<usize>; 2]> {
    let order = [source_order(table, 0), source_order(table, 1)];
    let rank: [BTreeMap<usize, usize>; 2] = [0, 1].map(|side| order[side].iter().enumerate().map(|(rank, index)| (*index, rank)).collect());
    let mut anchors: Vec<(usize, usize)> = table.pairs.iter().filter(|(_, _, same)| *same).map(|(a, b, _)| (rank[0][a], rank[1][b])).collect();
    anchors.sort();
    let mut out = Vec::new();
    let mut last = [0usize, 0usize];
    let mut first = true;
    for (a, b) in anchors.into_iter().chain(std::iter::once((order[0].len(), order[1].len()))) {
        let from = if first { [0, 0] } else { [last[0] + 1, last[1] + 1] };
        let part = |side: usize, from: usize, to: usize| -> Vec<usize> { (from..to.min(order[side].len())).map(|rank| order[side][rank]).filter(|index| !table.same[side].get(*index).copied().unwrap_or(false)).collect() };
        let region = [part(0, from[0], a), part(1, from[1], b)];
        if !region[0].is_empty() && !region[1].is_empty() {
            let mut sorted = region;
            sorted[0].sort();
            sorted[1].sort();
            out.push(sorted);
        }
        last = [a, b];
        first = false;
    }
    out
}

/// [R97.7-1] The groups of the repeated edits of a step: the regions of the real edits ("replace")
/// whose fragment pairs have one key. Each node of a region is a node of the group only if its
/// fact has no node outside the group, so each node stays in exactly one entry. One pass: the
/// key of each region, then the groups of the keys.
pub(crate) fn groups(input: &StepInput, outcome: &mut StepOutcome) {
    let trees = [input.before, input.after];
    let nodes: Vec<usize> = (0..outcome.edits.len()).filter(|index| crate::astmap::is_node(&outcome.edits[*index])).collect();
    let mut by_key: BTreeMap<String, Vec<(String, std::sync::Arc<MirTable>, [Vec<usize>; 2], Vec<usize>)>> = BTreeMap::new();
    for (body, shared) in tables(outcome) {
        let table: &MirTable = &shared;
        let mut mirs: Option<[Mir; 2]> = None;
        for region in regions(table) {
            if in_macro(table, 0, &region[0]) || in_macro(table, 1, &region[1]) {
                continue;
            }
            let places = [places_of(table, 0, &region[0]), places_of(table, 1, &region[1])];
            let mut members = Vec::new();
            let mut whole = true;
            for edit in &nodes {
                let Some(mir) = outcome.report.node_mir.get(edit) else { continue };
                if !mir.bodies.contains(&body.name) || (mir.places[0].is_empty() && mir.places[1].is_empty()) {
                    continue;
                }
                let inside = (0..2).all(|side| mir.places[side].is_subset(&places[side]));
                let touches = (0..2).any(|side| !mir.places[side].is_disjoint(&places[side]));
                if inside {
                    let fact = outcome.fact_of.get(*edit).copied().flatten().and_then(|fact| outcome.report.facts.get(fact));
                    let kind = crate::diffmap::node_kind(&outcome.edits[*edit], Some(mir), fact, trees);
                    if kind.kind != EntryKind::Replace || kind.syntax_only || outcome.transforms.nodes.contains_key(edit) {
                        whole = false;
                    }
                    members.push(*edit);
                } else if touches {
                    whole = false;
                }
            }
            if !whole || members.is_empty() {
                continue;
            }
            let mirs = mirs.get_or_insert_with(|| [parse_mir(&table.texts[0]), parse_mir(&table.texts[1])]);
            let key = fragment_key(mirs, table, &region[0], &region[1]);
            by_key.entry(key).or_default().push((body.name.clone(), shared.clone(), region, members));
        }
    }
    for (_, mut sites) in by_key {
        // Two regions of one node (one body that rustc compiles two times, for example the
        // synthetic body of an async closure) are one edit.
        let mut seen: Vec<Vec<usize>> = Vec::new();
        sites.retain(|(_, _, _, edits)| {
            let mut key = edits.clone();
            key.sort();
            let new = !seen.contains(&key);
            seen.push(key);
            new
        });
        if sites.len() < 2 {
            continue;
        }
        // The facts of the group: each one must have its nodes in the group only.
        let members: BTreeSet<usize> = sites.iter().flat_map(|(_, _, _, edits)| edits.iter().copied()).collect();
        let facts: BTreeSet<usize> = members.iter().filter_map(|edit| outcome.fact_of.get(*edit).copied().flatten()).collect();
        let outside = (0..outcome.edits.len()).any(|edit| !members.contains(&edit) && outcome.fact_of.get(edit).copied().flatten().is_some_and(|fact| facts.contains(&fact)));
        if outside {
            continue;
        }
        let list: Vec<(String, String, usize, Vec<usize>)> = sites
            .iter()
            .map(|(body, table, region, edits)| {
                let (file, line) = line_at(trees[0], table, 0, &region[0]);
                (body.clone(), file, line, edits.clone())
            })
            .collect();
        let text: Vec<String> = list.iter().map(|(body, file, line, _)| format!("`{body}` ({file}:{line})")).collect();
        let check = format!("the same edit at {} sites: the old and the new MIR are equal after a rename of the locals: {}", list.len(), text.join(", "));
        let first = list[0].3[0];
        for edit in &members {
            outcome.transforms.nodes.insert(*edit, NodeKind { kind: EntryKind::Replace, syntax_only: false, rule: RULE_GROUP, check: check.clone(), order: None });
            if *edit != first {
                outcome.transforms.joins.push((*edit, first));
            }
        }
        let bodies: BTreeSet<&String> = sites.iter().map(|(body, ..)| body).collect();
        let scope = if bodies.len() == 1 { Some(sites[0].0.as_str()) } else { None };
        let (_, table, region, _) = &sites[0];
        let template = closure_template(input, table, &sites[0].0, [&region[0], &region[1]], scope);
        outcome.transforms.rewrites.templates.push((first, template.clone()));
        outcome.transforms.rewrites.groups.push(Group { sites: list, template: Some(template) });
    }
}

// ---------------------------------------------------------------------------------------------
// The templates.

/// [R97.8-1] The type of a local in the form of the intent crate: each path of a crate of the
/// workspace gets the prefix of its version. `Err`: a type that no path of the intent crate can
/// name: a closure type or an opaque type (the printer of the driver marks them). Each other type
/// that the intent crate cannot name (a generic parameter) gets its cause from the gate.
pub(crate) fn intent_type(ty: &str, side: usize, crates: &BTreeSet<String>) -> Result<String, String> {
    if ty.contains("{closure") || ty.contains("{coroutine") {
        return Err(format!("a closure type `{ty}`"));
    }
    if ty.contains("impl ") || ty.contains("Alias(") || ty.contains("{opaque") {
        return Err(format!("an opaque type `{ty}`"));
    }
    let prefix = if side == 0 { "before::" } else { "after::" };
    let bytes = ty.as_bytes();
    let mut out = String::with_capacity(ty.len() + 16);
    let mut at = 0;
    while at < ty.len() {
        let c = bytes[at];
        if (c.is_ascii_alphabetic() || c == b'_') && (at == 0 || !(ident_char(bytes[at - 1]) || bytes[at - 1] == b':')) {
            let end = ty[at..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':')).map_or(ty.len(), |len| at + len);
            let path = &ty[at..end];
            let first = path.split("::").next().unwrap_or("");
            // A generic parameter or a type that the intent crate cannot name: the check of the
            // intent files (the gate of R87.1-1) gives the cause.
            if path.contains("::") && crates.contains(first) {
                out.push_str(prefix);
            }
            out.push_str(path);
            at = end;
            continue;
        }
        if c == b'\'' {
            // A lifetime of a type: a closure parameter has the elided lifetime `'_`.
            let end = ty[at + 1..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(ty.len(), |len| at + 1 + len);
            out.push_str("'_");
            at = end;
            continue;
        }
        out.push(c as char);
        at += 1;
    }
    Ok(out)
}

/// [R97.8-1] A template of the closure form for the elements of one site (old and new): the old
/// and the new code texts, with a parameter for each free local (a local that the elements read
/// and do not write). The type of a parameter is the MIR type of its local. `Err`: the cause of
/// no template.
pub(crate) fn closure_template(input: &StepInput, table: &MirTable, body: &str, elements: [&[usize]; 2], scope: Option<&str>) -> Result<String, String> {
    let trees = [input.before, input.after];
    let mirs = [parse_mir(&table.texts[0]), parse_mir(&table.texts[1])];
    let crates: [BTreeSet<String>; 2] = [0, 1].map(|side| trees[side].crates.iter().map(|info| info.name.clone()).collect());
    let map: BTreeMap<usize, usize> = table.locals.iter().copied().collect();
    let mut texts: [String; 2] = Default::default();
    let mut ranges: [(String, usize, usize); 2] = Default::default();
    let mut free: [Vec<usize>; 2] = Default::default();
    for side in 0..2 {
        let list = elements[side];
        if list.is_empty() {
            return Err("a site with no code on one side".into());
        }
        let places: Vec<MirPlace> = list.iter().filter_map(|index| table.ranges[side].get(*index).and_then(|(tokens, _)| tokens.clone())).collect();
        let Some(file) = places.first().map(|place| place.0.clone()) else { return Err("an element with no source range".into()) };
        if places.iter().any(|place| place.0 != file) {
            return Err("the elements of the site are in two files".into());
        }
        let start = places.iter().map(|place| place.1).min().unwrap_or(0);
        let end = places.iter().map(|place| place.2).max().unwrap_or(0);
        let text = trees[side].rust_files.get(&file).and_then(|text| text.get(start..end)).ok_or("no source text of the site")?;
        texts[side] = text.to_string();
        ranges[side] = (file, start, end);
        let written: BTreeSet<usize> = list.iter().filter_map(|index| local_at(&mirs[side].elements[*index].0, 0).filter(|(_, len)| mirs[side].elements[*index].0[*len..].starts_with(" = ")).map(|(local, _)| local)).collect();
        for index in list {
            for (_, _, local) in locals_in(&mirs[side].elements[*index].0) {
                if !written.contains(&local) && local != 0 && !free[side].contains(&local) {
                    free[side].push(local);
                }
            }
        }
    }
    // The parameters: each free old local with its new partner, then each free new local with no
    // old partner.
    let mut params: Vec<(Option<usize>, Option<usize>)> = free[0].iter().map(|local| (Some(*local), map.get(local).copied().filter(|new| free[1].contains(new)))).collect();
    for local in &free[1] {
        if !params.iter().any(|(_, new)| *new == Some(*local)) {
            params.push((None, Some(*local)));
        }
    }
    let mut names: Vec<String> = Vec::new();
    let mut edits: [Vec<(usize, usize, String)>; 2] = Default::default();
    for (old, new) in &params {
        let mut name = None;
        for (side, local) in [(0, old), (1, new)] {
            let Some(local) = local else { continue };
            let (kind, at) = table.decls[side].get(*local).cloned().unwrap_or((-1, None));
            let (file, start, end) = &ranges[side];
            match (kind, at) {
                (1 | 2, Some((_, from, to))) => {
                    let text = trees[side].rust_files.get(file).and_then(|text| text.get(from..to)).unwrap_or("");
                    let plain = text.split(':').next().unwrap_or("").trim().trim_start_matches('&').trim_start_matches("mut ").trim();
                    if !plain.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || plain.is_empty() {
                        return Err(format!("a variable with the pattern `{text}`"));
                    }
                    let chosen = name.get_or_insert_with(|| if plain == "self" { "this".to_string() } else { plain.to_string() }).clone();
                    if chosen != plain {
                        let local_text = &texts[side];
                        for (at, _) in local_text.match_indices(plain) {
                            let before_ok = at == 0 || !ident_char(local_text.as_bytes()[at - 1]);
                            let after_ok = local_text.as_bytes().get(at + plain.len()).is_none_or(|c| !ident_char(*c));
                            if before_ok && after_ok {
                                edits[side].push((at, at + plain.len(), chosen.clone()));
                            }
                        }
                    }
                }
                (3, Some((at_file, from, to))) if at_file == *file && *start <= from && to <= *end => {
                    let chosen = name.get_or_insert_with(|| format!("v{}", names.len() + 1)).clone();
                    edits[side].push((from - start, to - start, chosen));
                }
                _ => return Err(format!("a value of the site with no source text (the local `_{local}`)")),
            }
        }
        names.push(name.unwrap_or_else(|| format!("v{}", names.len() + 1)));
    }
    for side in 0..2 {
        let mut list = std::mem::take(&mut edits[side]);
        list.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut text = texts[side].clone();
        let mut floor = usize::MAX;
        for (from, to, name) in list {
            if to <= floor && from <= to && to <= text.len() {
                text.replace_range(from..to, &name);
                floor = from;
            }
        }
        texts[side] = semilla_core::text::flat_text(&text);
    }
    let mut lists: [Vec<String>; 2] = Default::default();
    for ((old, new), name) in params.iter().zip(&names) {
        for (side, local) in [(0, old), (1, new)] {
            let local = local.or_else(|| if side == 0 { map.iter().find(|(_, b)| Some(**b) == *new).map(|(a, _)| *a) } else { old.and_then(|old| map.get(&old).copied()) });
            let ty = match local.and_then(|local| mirs[side].local_type(local)) {
                Some(ty) => intent_type(ty, side, &crates[side])?,
                None => {
                    // A parameter of one side only: the type of the other side, in this version.
                    let other = if side == 0 { new } else { old };
                    let ty = other.and_then(|local| mirs[1 - side].local_type(local)).ok_or("a parameter with no type")?;
                    intent_type(ty, side, &crates[side])?
                }
            };
            lists[side].push(format!("{name}: {ty}"));
        }
    }
    let short = body.rsplit("::").next().unwrap_or(body).chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect::<String>();
    let short = if short.is_empty() || short.starts_with(|c: char| c.is_ascii_digit()) { "site".to_string() } else { short.to_lowercase() };
    let scope = match scope {
        Some(path) if path.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':') => format!("(after::{path})"),
        _ => String::new(),
    };
    Ok(format!("#[rewrite{scope}] fn {short}_rewrite() {{ (|{}| {}, |{}| {}) }}", lists[0].join(", "), texts[0], lists[1].join(", "), texts[1]))
}

/// [R97.8-1] The templates of the "not mapped" entries: a template of the closure form for each
/// node with no unique alignment in a body with a table (its elements are the elements inside
/// the range of the node).
pub(crate) fn open_templates(input: &StepInput, outcome: &mut StepOutcome) {
    let trees = [input.before, input.after];
    let Some(map) = outcome.report.ast.as_ref() else { return };
    let indices: Vec<usize> = (0..outcome.edits.len()).filter(|index| crate::astmap::is_node(&outcome.edits[*index])).collect();
    let mut found = Vec::new();
    for (entry, edit) in map.entries.iter().zip(&indices) {
        if entry.kind != Some(EntryKind::NotMapped) || entry.syntax_only {
            continue;
        }
        let item = outcome.edits[*edit].anchor().map(|anchor| anchor.item.clone()).unwrap_or_default();
        let Some((body, shared)) = tables(outcome).into_iter().find(|(body, _)| body.name == item) else { continue };
        let table: &MirTable = &shared;
        let mut lists: [Vec<usize>; 2] = Default::default();
        for (side, anchor) in [(0, &outcome.edits[*edit].before), (1, &outcome.edits[*edit].after)] {
            let Some((file, start, end)) = anchor.as_ref().and_then(|anchor| crate::records::anchor_place(trees[side], anchor)) else { continue };
            lists[side] = (0..table.ranges[side].len()).filter(|index| table.ranges[side][*index].0.as_ref().is_some_and(|(at, from, to)| *at == file && start <= *from && *to <= end)).collect();
            // A node inside one element (a path, a literal): the elements with the smallest range
            // that holds the node.
            if lists[side].is_empty() {
                let holds: Vec<(usize, usize)> = (0..table.ranges[side].len()).filter_map(|index| table.ranges[side][index].0.as_ref().filter(|(at, from, to)| *at == file && *from <= start && end <= *to).map(|(_, from, to)| (index, to - from))).collect();
                if let Some(least) = holds.iter().map(|(_, len)| *len).min() {
                    lists[side] = holds.iter().filter(|(_, len)| *len == least).map(|(index, _)| *index).collect();
                }
            }
        }
        let template = if lists.iter().any(Vec::is_empty) {
            Err("the node has no MIR element inside it or around it on each side (a pattern or a binding is not an expression of the closure form)".to_string())
        } else {
            closure_template(input, table, &body.name, [&lists[0], &lists[1]], Some(&body.name))
        };
        found.push((*edit, template));
    }
    outcome.transforms.rewrites.templates.extend(found);
}

/// [R97.8-1] Writes the template of each group and of each "not mapped" entry on the fact of its
/// first node, for the gate (after the shape of the entries).
pub(crate) fn attach(outcome: &mut StepOutcome) {
    for (edit, template) in outcome.transforms.rewrites.templates.clone() {
        let Some(fact) = outcome.fact_of.get(edit).copied().flatten() else { continue };
        let Some(fact) = outcome.report.facts.get_mut(fact) else { continue };
        if fact.template.is_some() {
            continue;
        }
        fact.template = Some(match template {
            Ok(template) => {
                outcome.templates.notes.push(template.clone());
                crate::template::note_template(&template)
            }
            Err(cause) => format!("no template: {cause}"),
        });
    }
}


#[cfg(test)]
#[path = "tests97.rs"]
mod tests;
