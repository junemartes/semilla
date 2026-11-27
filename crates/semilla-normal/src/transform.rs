//! The transformations of a changed body (design 94).
//!
//! - [R94.1-1] [R94.2-1] **Extract and inline of a function.** The check puts the canonical MIR
//!   of a callee in at its call (`inline`), then aligns the result again with the other version
//!   of the body (`elements::diff_keyed`). An element that differed before and is equal after it
//!   is explained by the transformation. The candidates come from the data of the step: a call
//!   of a changed body to a callee that the step adds (or that does not change), at the place
//!   of the changed elements (an extract); the reverse for an inline. Each candidate gets one
//!   check: no search over maps.
//! - [R94.4-2] [R94.4-3] **A new order of the same parts** (`permutation`): the two texts have
//!   the same elements under one map of the locals and of the straight runs of blocks, in
//!   another order. Each pair of elements whose order changes is checked for a dependence.
//!
//! The inline of this module works on the canonical text of the driver (the N0 normal form),
//! with the conditions of the inliner of the driver (`semilla-mir`, `inline.rs`): a direct call
//! of a local function with no generic arguments and a normal return, a callee that is not
//! recursive, an argument that the callee reads only (else a new local with a copy), and the
//! unwind edges of the callee to the unwind action of the call. The check of the verifier adds
//! the conditions that only the syntax tree knows (`#[track_caller]`).
//!
//! Each rule reads only the two texts and their element ranges. A doubt never gives "explained".

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::elements::{self, Element, ElementDiff, OrderKey, Place, RecordElements};

/// The kind of a transformation of a body (design 94).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A region of the old body is a new callee: the new body calls it (R94.1-1).
    #[default]
    Extract,
    /// The old body calls a callee, and the new body has its code (R94.2-1).
    Inline,
}

/// [R94.1-1] The check of one candidate of an extract or an inline: a callee put in at one call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transform {
    pub kind: Kind,
    /// The def path of the callee.
    pub callee: String,
    /// The place of the call: in the new body for an extract, in the old body for an inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<Place>,
    /// [R94.5-1] Why the check cannot put the callee in at the call. Then the other fields have
    /// no data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// True if the check holds: the callee can go in at the call, an element that differed
    /// before is equal after it, and at least half of the places of the callee are equal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub holds: bool,
    /// [R94.1-3] True if a different candidate of the body explains an element of the same
    /// region: a tie. A tie gives no pair (a statement of the agent decides it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tie: bool,
    /// The count of the element places of the other side that differ before the check and are
    /// equal after it.
    #[serde(default)]
    pub explained: usize,
    /// The places of the elements of the callee, and how many are equal after the check.
    #[serde(default)]
    pub callee_places: usize,
    #[serde(default)]
    pub callee_equal: usize,
    /// The places of the other side that the check explains (their tokens).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub region: Vec<Place>,
    /// The token places of the elements of the callee in the text with the callee (R94.1-2: an
    /// element of the callee that differs after the check is code that the check does not
    /// explain).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callee_at: Vec<Place>,
    /// The elements that differ after the check: old (0) and new (1).
    #[serde(default)]
    pub left: [Vec<Element>; 2],
    /// True if no element and no other part differs after the check.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub equal: bool,
}

/// [R94.4-2] A new order of the same parts of a body (design 94): the elements of the two texts
/// are equal under one map of the locals and of the straight runs of blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reorder {
    /// The count of the pairs of elements whose order changes.
    pub flips: usize,
    /// [R94.4-3] The pairs whose order changes and that depend on each other: the two places
    /// (old side) and the cause. Empty: the parts are independent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependent: Vec<(Option<Place>, Option<Place>, String)>,
    /// The places (old side) of the elements whose order changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moved: Vec<Place>,
}

/// The most candidates that one body pair checks.
pub const MOST_CANDIDATES: usize = 8;
/// The largest body (elements) that the checks of this module read.
pub const MOST_ELEMENTS: usize = 2000;
/// The most pairs of elements that the order check compares in one body.
pub const MOST_PAIRS: usize = 2_000_000;

// ---------------------------------------------------------------------------------------------
// The canonical text.

/// One element of a parsed text: its text (with no `;`) and its ranges.
#[derive(Clone, Debug)]
struct Elem {
    text: String,
    tokens: Option<Place>,
    call: Option<Place>,
    symbols: Vec<usize>,
    /// True for an element of the callee (an inline).
    callee: bool,
}

/// A parsed canonical text of the driver.
#[derive(Clone, Debug)]
struct Text {
    arg_types: Vec<String>,
    ret: String,
    /// The declared locals that are not arguments (also `_0`), with their types.
    locals: BTreeMap<usize, String>,
    blocks: Vec<(usize, Vec<Elem>)>,
}

/// The end of the bracket group that opens at `open` (a `(`, `[`, or `{`).
fn matching(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (at, c) in text[open..].char_indices() {
        if in_string {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => in_string = false,
                _ => escaped = false,
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + at);
                }
            }
            _ => {}
        }
    }
    None
}

/// The parts of a list at the top level, split at `, `.
fn split_list(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut start, mut in_string, mut escaped) = (0i32, 0usize, false, false);
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let c = bytes[at];
        if in_string {
            match c {
                b'\\' if !escaped => escaped = true,
                b'"' if !escaped => in_string = false,
                _ => escaped = false,
            }
            at += 1;
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'(' | b'[' | b'{' => depth += 1,
            b'<' => depth += 1,
            b'>' if at == 0 || bytes[at - 1] != b'-' => depth -= 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 && bytes.get(at + 1) == Some(&b' ') => {
                parts.push(text[start..at].to_string());
                start = at + 2;
                at += 1;
            }
            _ => {}
        }
        at += 1;
    }
    if start < text.len() {
        parts.push(text[start..].to_string());
    }
    parts
}

/// Parses a canonical text with the ranges of its elements. `None` for a text with a line that
/// this module does not read, or ranges that do not fit.
fn parse_text(text: &str, record: &RecordElements) -> Option<Text> {
    let ranges = record.ranges()?;
    let symbols = record_symbols(record);
    let mut lines = text.lines();
    let sig = lines.next()?;
    let inner = sig.strip_prefix("fn _")?;
    let close = matching(inner, 0)?;
    let args = &inner[1..close];
    let rest = inner[close + 1..].strip_prefix(" -> ")?.strip_suffix(" {")?;
    let mut arg_types = Vec::new();
    for (index, part) in split_list(args).into_iter().enumerate() {
        let (name, ty) = part.split_once(": ")?;
        if name != format!("_{}", index + 1) {
            return None;
        }
        arg_types.push(ty.to_string());
    }
    let mut out = Text { arg_types, ret: rest.to_string(), locals: BTreeMap::new(), blocks: Vec::new() };
    let mut index = 0usize;
    let mut ended = false;
    for line in lines {
        if ended {
            return None;
        }
        if let Some(element) = line.strip_prefix("        ") {
            let (_, list) = out.blocks.last_mut()?;
            let (tokens, call) = ranges.get(index)?.clone();
            let text = element.strip_suffix(';').unwrap_or(element).to_string();
            list.push(Elem { text, tokens, call, symbols: symbols.get(index).cloned().unwrap_or_default(), callee: false });
            index += 1;
        } else if let Some(rest) = line.strip_prefix("    let _") {
            let (number, ty) = rest.split_once(": ")?;
            out.locals.insert(number.parse().ok()?, ty.strip_suffix(';')?.to_string());
        } else if let Some(rest) = line.strip_prefix("    bb") {
            out.blocks.push((rest.strip_suffix(": {")?.parse().ok()?, Vec::new()));
        } else if line == "    }" {
        } else if line == "}" {
            ended = true;
        } else {
            return None;
        }
    }
    (ended && index == ranges.len()).then_some(out)
}

/// The symbols of each element, as indexes into the table `s` of the record.
fn record_symbols(record: &RecordElements) -> Vec<Vec<usize>> {
    let mut out = vec![Vec::new(); record.n];
    for run in &record.y {
        let (Some(first), Some(count)) = (run.first(), run.get(1)) else { continue };
        for index in *first as usize..(*first as usize).saturating_add(*count as usize) {
            if let Some(slot) = out.get_mut(index) {
                *slot = run[2..].iter().map(|symbol| *symbol as usize).collect();
            }
        }
    }
    out
}

impl Text {
    fn args(&self) -> usize {
        self.arg_types.len()
    }

    /// The largest local number.
    fn top_local(&self) -> usize {
        self.locals.keys().copied().max().unwrap_or(0).max(self.args())
    }

    fn top_block(&self) -> usize {
        self.blocks.iter().map(|(number, _)| *number).max().unwrap_or(0)
    }

    fn elements(&self) -> impl Iterator<Item = &Elem> {
        self.blocks.iter().flat_map(|(_, list)| list.iter())
    }

    /// The canonical text, with the blocks in the order of their numbers.
    fn print(&self) -> String {
        let mut out = String::new();
        let args: Vec<String> = self.arg_types.iter().enumerate().map(|(index, ty)| format!("_{}: {ty}", index + 1)).collect();
        out.push_str(&format!("fn _({}) -> {} {{\n", args.join(", "), self.ret));
        for (local, ty) in &self.locals {
            out.push_str(&format!("    let _{local}: {ty};\n"));
        }
        let mut blocks: Vec<&(usize, Vec<Elem>)> = self.blocks.iter().collect();
        blocks.sort_by_key(|(number, _)| *number);
        for (number, list) in blocks {
            out.push_str(&format!("    bb{number}: {{\n"));
            for element in list {
                out.push_str(&format!("        {};\n", element.text));
            }
            out.push_str("    }\n");
        }
        out.push_str("}\n");
        out
    }

    /// The elements in the order of the printed text.
    fn printed(&self) -> Vec<&Elem> {
        let mut blocks: Vec<&(usize, Vec<Elem>)> = self.blocks.iter().collect();
        blocks.sort_by_key(|(number, _)| *number);
        blocks.into_iter().flat_map(|(_, list)| list.iter()).collect()
    }

    /// The element ranges of the printed text, with the symbols of `table` (a list of symbol
    /// entries of the two records), the files, and the declarations of the locals.
    fn record(&self, files: &[String], table: &[elements::RecordSymbol], own: Option<u32>, version: u8, evaluated: bool, declarations: &BTreeMap<usize, Vec<i64>>) -> RecordElements {
        let mut out = RecordElements { f: files.to_vec(), s: table.to_vec(), v: version, o: own, c: evaluated, ..Default::default() };
        let file_index = |file: &str| files.iter().position(|known| known == file).map_or(-1, |at| at as i64);
        for (index, element) in self.printed().into_iter().enumerate() {
            let mut run = vec![index as i64, 1];
            match &element.tokens {
                Some(place) => run.extend([file_index(&place.file), place.start as i64, place.end as i64]),
                None => run.extend([-1, 0, 0]),
            }
            if let Some(call) = &element.call {
                run.extend([file_index(&call.file), call.start as i64, call.end as i64]);
            }
            out.r.push(run);
            if !element.symbols.is_empty() {
                let mut symbols = vec![index as u32, 1];
                symbols.extend(element.symbols.iter().map(|symbol| *symbol as u32));
                out.y.push(symbols);
            }
            out.n += 1;
        }
        let top = declarations.keys().copied().max().unwrap_or(0);
        out.l = (0..=top).map(|local| declarations.get(&local).cloned().unwrap_or_default()).collect();
        out
    }
}

// ---------------------------------------------------------------------------------------------
// The numbers and the uses of a local in an element text.

/// One number of an element text: a local (`_3`) or a block (`bb4`), outside the string
/// constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Num {
    Local(usize),
    Block(usize),
}

/// The numbers of an element text in order: the start, the end, and the number.
fn numbers(text: &str) -> Vec<(usize, usize, Num)> {
    let bytes = text.as_bytes();
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut out = Vec::new();
    let (mut at, mut in_string, mut escaped) = (0, false, false);
    while at < bytes.len() {
        let c = bytes[at];
        if in_string {
            match c {
                b'\\' if !escaped => escaped = true,
                b'"' if !escaped => in_string = false,
                _ => escaped = false,
            }
            at += 1;
            continue;
        }
        if c == b'"' {
            in_string = true;
            at += 1;
            continue;
        }
        let before_ok = at == 0 || !word(bytes[at - 1]);
        let prefix = if before_ok && c == b'_' {
            1
        } else if before_ok && text[at..].starts_with("bb") {
            2
        } else {
            0
        };
        if prefix > 0 {
            let digits = bytes[at + prefix..].iter().take_while(|byte| byte.is_ascii_digit()).count();
            let end = at + prefix + digits;
            if digits > 0 && (end == bytes.len() || !word(bytes[end])) {
                let value = text[at + prefix..end].parse().unwrap_or(usize::MAX);
                out.push((at, end, if prefix == 1 { Num::Local(value) } else { Num::Block(value) }));
                at = end;
                continue;
            }
        }
        at += text[at..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// The text with each number replaced.
fn renumber(text: &str, local: &dyn Fn(usize) -> String, block: &dyn Fn(usize) -> usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, number) in numbers(text) {
        out.push_str(&text[at..start]);
        match number {
            Num::Local(value) => out.push_str(&local(value)),
            Num::Block(value) => out.push_str(&format!("bb{}", block(value))),
        }
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// The kind of one mention of a local in an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    /// The element writes the local (or a field of it), or drops it.
    Write,
    /// A borrow or a raw address of the local (or of a field of it).
    Borrow,
    /// A read: an operand, a place through a deref (the value of a pointer), a projection.
    Read,
}

/// The place text before ` = ` at the start of an element, if the element is an assignment
/// (a statement or the destination of a call).
fn assigned(text: &str) -> Option<&str> {
    let first = text.as_bytes().first()?;
    if !(*first == b'_' || *first == b'(' || text.starts_with("discriminant(")) {
        return None;
    }
    let mut depth = 0i32;
    for (at, c) in text.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ' ' if depth == 0 && text[at..].starts_with(" = ") => return Some(&text[..at]),
            _ => {}
        }
    }
    None
}

/// The markers of a borrow in an element text.
const BORROWS: &[&str] = &["&shared ", "&mut ", "&fake ", "&raw Mut ", "&raw Not ", "reborrow Mut ", "reborrow Not "];

/// The mentions of each local in an element text.
fn uses(text: &str) -> Vec<(usize, Use)> {
    let place = assigned(text);
    let set_discriminant = text.starts_with("discriminant(") && place.is_some();
    let drop = text.starts_with("drop(");
    let mut out = Vec::new();
    for (start, _, number) in numbers(text) {
        let Num::Local(local) = number else { continue };
        let before = &text[..start];
        // A place through a deref of the local reads the pointer.
        let deref = before.ends_with("(*");
        let kind = if !deref && BORROWS.iter().any(|marker| before.trim_end_matches('(').ends_with(marker)) {
            Use::Borrow
        } else if !deref && (place.is_some_and(|place| start < place.len()) || set_discriminant || (drop && before == "drop(")) {
            Use::Write
        } else {
            Use::Read
        };
        out.push((local, kind));
    }
    out
}

/// True if an element writes through a pointer (a place with a deref before ` = `).
fn writes_memory(text: &str) -> bool {
    assigned(text).is_some_and(|place| place.contains("(*")) || text.starts_with("copy_nonoverlapping(")
}

/// True if an element reads through a pointer.
fn reads_memory(text: &str) -> bool {
    let value = assigned(text).map_or(text, |place| &text[place.len()..]);
    value.contains("(*")
}

// ---------------------------------------------------------------------------------------------
// The inline of a callee at a call (R94.1-1, R94.2-1).

/// The parts of a call element of a callee.
struct Call {
    dest: String,
    fn_ty: String,
    args: Vec<String>,
    target: usize,
    /// The unwind action: `None` for `unwind continue`, `Some(block)` for a cleanup block.
    unwind: Option<usize>,
}

/// The call of `path` in an element text, or the cause why it is not a call that the check can
/// put the callee in at.
fn parse_call(text: &str, path: &str) -> Result<Call, String> {
    let marker = format!(" {{{path}}}(");
    let at = text.find(&marker).ok_or_else(|| "no direct call".to_string())?;
    let head = &text[..at];
    let (dest, fn_ty) = head.split_once(" = const ").ok_or_else(|| "no direct call".to_string())?;
    let open = at + marker.len() - 1;
    let close = matching(text, open).ok_or_else(|| "no direct call".to_string())?;
    let rest = &text[close + 1..];
    let tail = rest.strip_prefix(" -> [return: bb").ok_or_else(|| "a call with no normal return".to_string())?;
    let (target, unwind) = tail.split_once(", ").ok_or_else(|| "a call with no normal return".to_string())?;
    let target = target.parse().map_err(|_| "a call with no normal return".to_string())?;
    let unwind = match unwind {
        "unwind continue]" => None,
        other => match other.strip_prefix("unwind: bb").and_then(|rest| rest.strip_suffix(']')).and_then(|number| number.parse().ok()) {
            Some(block) => Some(block),
            None => return Err(format!("a call with the unwind action `{}`", other.trim_end_matches(']'))),
        },
    };
    Ok(Call { dest: dest.to_string(), fn_ty: fn_ty.to_string(), args: split_list(&text[open + 1..close]), target, unwind })
}

/// A function type with no lifetimes: no binder `for<'a> ` and no lifetime name (`&'a T` is
/// `&T`). A lifetime has no part in the compiled code, and a late-bound lifetime is no generic
/// argument of the call.
fn plain_fn_type(text: &str) -> String {
    let mut text = text.trim().to_string();
    if let Some(rest) = text.strip_prefix("for<") {
        let mut depth = 1i32;
        let mut end = None;
        for (at, c) in rest.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(at);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(end) = end {
            text = rest[end + 1..].trim_start().to_string();
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' && chars.peek().is_some_and(|next| next.is_alphabetic() || *next == '_') {
            while chars.peek().is_some_and(|next| next.is_alphanumeric() || *next == '_') {
                chars.next();
            }
            if chars.peek() == Some(&',') {
                chars.next();
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
            } else if chars.peek() == Some(&' ') {
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// The local of a plain local text (`_3`).
fn plain_local(text: &str) -> Option<usize> {
    text.strip_prefix('_')?.parse().ok()
}

/// The local of a place text with no deref and no index: a local (`_3`), a field of a place
/// (`(_3.0: u32)`, `(_3.depot::Pair.a: u32)`), or a downcast of a place (`(_3 as depot::E::A)`).
/// `None` for each other text (an rvalue, a constant, a deref, an index).
fn place_local(text: &str) -> Option<usize> {
    if let Some(local) = plain_local(text) {
        return Some(local);
    }
    let inner = text.strip_prefix('(')?.strip_suffix(')')?;
    let (base, rest) = if inner.starts_with('(') {
        let close = matching(inner, 0)?;
        (&inner[..=close], &inner[close + 1..])
    } else {
        let end = inner.find(['.', ' ']).unwrap_or(inner.len());
        (&inner[..end], &inner[end..])
    };
    let local = place_local(base)?;
    let plain = |part: &str| !part.is_empty() && !part.contains([' ', '(', ')', ',', '*', '[']);
    if let Some(field) = rest.strip_prefix('.') {
        let (name, ty) = field.split_once(": ")?;
        (plain(name) && !ty.is_empty()).then_some(local)
    } else {
        rest.strip_prefix(" as ").filter(|variant| plain(variant)).map(|_| local)
    }
}

/// The place of an operand text that is a place (`_3`, `move _3`, `(_3.0: u32)`), with no deref
/// and no index, and its local. `None` for a constant, an rvalue, or a place through a deref.
fn operand_place(text: &str) -> Option<(String, usize)> {
    let place = text.strip_prefix("move ").unwrap_or(text);
    place_local(place).map(|local| (place.to_string(), local))
}

/// The locals of a text that something borrows.
fn borrowed(text: &Text) -> BTreeSet<usize> {
    text.elements().flat_map(|element| uses(&element.text)).filter(|(_, kind)| *kind == Use::Borrow).map(|(local, _)| local).collect()
}

/// True if a block of `text` calls `path` directly.
fn calls(text: &Text, path: &str) -> bool {
    let marker = format!(" {{{path}}}(");
    text.elements().any(|element| element.text.contains(&marker))
}

/// The order keys of the elements of a text in the order of the printed text: the place of each
/// element in a walk of the blocks from the entry (a reverse postorder), then in its block: the
/// order of the run. The code of a callee runs at the place of its call, also when the call is
/// a part of a larger expression (the order of the source puts the larger expression first).
fn order_keys(text: &Text) -> Vec<OrderKey> {
    let by_number: BTreeMap<usize, &Vec<Elem>> = text.blocks.iter().map(|(number, list)| (*number, list)).collect();
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    let mut post: Vec<usize> = Vec::new();
    // An iterative depth-first walk: (block, index of the next successor).
    let successors = |block: usize| -> Vec<usize> {
        by_number.get(&block).and_then(|list| list.last()).map(|last| numbers(&last.text).into_iter().filter_map(|(_, _, number)| match number {
            Num::Block(target) => Some(target),
            Num::Local(_) => None,
        }).collect()).unwrap_or_default()
    };
    if let Some((entry, _)) = text.blocks.first() {
        let mut stack: Vec<(usize, usize)> = vec![(*entry, 0)];
        seen.insert(*entry);
        while let Some((block, next)) = stack.pop() {
            let list = successors(block);
            match list.get(next) {
                Some(target) => {
                    stack.push((block, next + 1));
                    if by_number.contains_key(target) && seen.insert(*target) {
                        stack.push((*target, 0));
                    }
                }
                None => post.push(block),
            }
        }
    }
    let mut rank: BTreeMap<usize, usize> = post.iter().rev().enumerate().map(|(at, block)| (*block, at)).collect();
    for (number, _) in &text.blocks {
        let next = rank.len();
        rank.entry(*number).or_insert(next);
    }
    let mut blocks: Vec<&(usize, Vec<Elem>)> = text.blocks.iter().collect();
    blocks.sort_by_key(|(number, _)| *number);
    let mut out = Vec::new();
    for (number, list) in blocks {
        for index in 0..list.len() {
            out.push((String::new(), vec![(rank[number], index)]));
        }
    }
    out
}

/// The result of an inline: the new text, and the ranges of its elements.
struct Inlined {
    text: Text,
    files: Vec<String>,
    table: Vec<elements::RecordSymbol>,
    declarations: BTreeMap<usize, Vec<i64>>,
}

/// [R94.1-1] Puts the callee `path` (its text and ranges) in at the call element `at` (block
/// index, element index) of `caller`. The new elements of the callee take the place of the call
/// in the order of the alignment. `Err`: the cause why the check cannot put it in.
fn inline(caller: (&Text, &RecordElements), at: (usize, usize), callee: (&Text, &RecordElements), path: &str, caller_path: &str) -> Result<Inlined, String> {
    let (outer, outer_record) = caller;
    let (inner, inner_record) = callee;
    let call_element = &outer.blocks[at.0].1[at.1];
    let call = parse_call(&call_element.text, path)?;
    // The conditions of the driver inliner (`semilla-mir`, `inline.rs`).
    if path.contains('<') || path.contains("{closure") {
        return Err("a call of a trait method or with generic arguments".into());
    }
    let signature = format!("fn({}) -> {}", inner.arg_types.join(", "), inner.ret);
    if plain_fn_type(&call.fn_ty) != plain_fn_type(&signature) {
        return Err(format!("the type of the call `{}` is not the signature of the callee `{signature}` (a generic callee)", call.fn_ty));
    }
    if call.args.len() != inner.args() {
        return Err("the count of the arguments is not the count of the parameters".into());
    }
    if calls(inner, path) || calls(inner, caller_path) {
        return Err("a recursive callee".into());
    }
    let dest = plain_local(&call.dest).ok_or_else(|| "the call writes a place that is not a local".to_string())?;
    let outer_borrowed = borrowed(outer);
    if outer_borrowed.contains(&dest) || call.args.iter().any(|arg| operand_place(arg).is_some_and(|(_, local)| local == dest)) {
        return Err("the caller borrows the local of the result, or passes it to the call".into());
    }
    // The mentions of each parameter of the callee.
    let mut param_uses: BTreeMap<usize, Vec<Use>> = BTreeMap::new();
    for element in inner.elements() {
        for (local, kind) in uses(&element.text) {
            if (1..=inner.args()).contains(&local) {
                param_uses.entry(local).or_default().push(kind);
            }
        }
    }
    let base = outer.top_local() + 1;
    let block_base = outer.top_block() + 1;
    let mut next_local = base + inner.top_local() + 1;
    let mut fresh: Vec<(usize, String)> = Vec::new();
    let mut substitute: BTreeMap<usize, String> = BTreeMap::new();
    let mut locals = outer.locals.clone();
    for (index, arg) in call.args.iter().enumerate() {
        let param = index + 1;
        let read_only = param_uses.get(&param).is_none_or(|kinds| kinds.iter().all(|kind| *kind == Use::Read));
        match operand_place(arg) {
            Some((place, local)) if read_only && !outer_borrowed.contains(&local) => {
                substitute.insert(param, place);
            }
            _ => {
                let local = next_local;
                next_local += 1;
                locals.insert(local, inner.arg_types[index].clone());
                fresh.push((local, arg.clone()));
                substitute.insert(param, format!("_{local}"));
            }
        }
    }
    for (local, ty) in &inner.locals {
        if *local != 0 {
            locals.insert(base + local, ty.clone());
        }
    }
    let local_of = |local: usize| -> String {
        match local {
            0 => format!("_{dest}"),
            _ if local <= inner.args() => substitute[&local].clone(),
            _ => format!("_{}", base + local),
        }
    };
    let unwind_text = match call.unwind {
        None => "unwind continue".to_string(),
        Some(block) => format!("unwind: bb{block}"),
    };
    // The files and the symbols of the two records.
    let mut files = outer_record.f.clone();
    let mut file_map = Vec::new();
    for file in &inner_record.f {
        file_map.push(match files.iter().position(|known| known == file) {
            Some(at) => at,
            None => {
                files.push(file.clone());
                files.len() - 1
            }
        });
    }
    let mut table = outer_record.s.clone();
    let symbol_base = table.len();
    for symbol in &inner_record.s {
        let elements::RecordSymbol(path, file, start, end) = symbol.clone();
        let file = usize::try_from(file).ok().and_then(|file| file_map.get(file)).map_or(file, |file| *file as i64);
        table.push(elements::RecordSymbol(path, file, start, end));
    }
    let glue = |text: String| Elem { text, tokens: call_element.tokens.clone(), call: call_element.call.clone(), symbols: Vec::new(), callee: false };
    let mut out = Text { arg_types: outer.arg_types.clone(), ret: outer.ret.clone(), locals, blocks: Vec::new() };
    let mut declarations: BTreeMap<usize, Vec<i64>> = outer_record.l.iter().enumerate().map(|(local, entry)| (local, entry.clone())).collect();
    for (local, entry) in inner_record.l.iter().enumerate() {
        if local > inner.args() {
            let mut entry = entry.clone();
            // The file of each place of the declaration (`[kind, file, start, end, …]`).
            for part in entry.iter_mut().skip(1).step_by(3) {
                if let Some(file) = usize::try_from(*part).ok().and_then(|file| file_map.get(file)) {
                    *part = *file as i64;
                }
            }
            declarations.insert(base + local, entry);
        }
    }
    for (index, (number, list)) in outer.blocks.iter().enumerate() {
        let mut list = list.clone();
        if index == at.0 {
            list.pop();
            for (local, arg) in &fresh {
                list.push(glue(format!("_{local} = {arg}")));
            }
            list.push(glue(format!("goto -> bb{block_base}")));
        }
        out.blocks.push((*number, list));
    }
    for (number, list) in &inner.blocks {
        let mut translated = Vec::new();
        for element in list {
            let text = match element.text.as_str() {
                "return" => format!("goto -> bb{}", call.target),
                "resume" => match call.unwind {
                    None => "resume".to_string(),
                    Some(block) => format!("goto -> bb{block}"),
                },
                other => renumber(other, &local_of, &|block| block_base + block).replace("unwind continue", &unwind_text),
            };
            let symbols = element.symbols.iter().map(|symbol| symbol_base + symbol).collect();
            translated.push(Elem { text, tokens: element.tokens.clone(), call: element.call.clone(), symbols, callee: true });
        }
        out.blocks.push((block_base + number, translated));
    }
    let passed: Vec<usize> = call.args.iter().filter_map(|arg| plain_local(arg.strip_prefix("move ").unwrap_or(arg))).collect();
    normal_form(&mut out, dest, &fresh.iter().map(|(local, _)| *local).collect::<Vec<_>>());
    reborrows(&mut out, &passed);
    Ok(Inlined { text: out, files, table, declarations })
}

/// The blocks that each block names in its elements (the edges), and the count of the
/// references to each block.
fn references(text: &Text) -> BTreeMap<usize, usize> {
    let mut count: BTreeMap<usize, usize> = BTreeMap::new();
    for element in text.elements() {
        for (_, _, number) in numbers(&element.text) {
            if let Num::Block(block) = number {
                *count.entry(block).or_default() += 1;
            }
        }
    }
    count
}

/// The rules 3, 3b, and 3c of the normal form of the driver (`semilla-mir`, `normalize.rs`) at
/// the places that the inline makes: a jump over a block with only a `goto`, the merge of a
/// block with its only predecessor, and the forward of a copy into the local of the result and
/// into a new local of an argument, in one block.
fn normal_form(text: &mut Text, dest: usize, fresh: &[usize]) {
    let entry = text.blocks.first().map(|(number, _)| *number);
    loop {
        let mut changed = false;
        // Rule 3: a block with only a `goto` (not the entry block).
        if let Some(at) = text.blocks.iter().position(|(number, list)| Some(*number) != entry && list.len() == 1 && list[0].text.strip_prefix("goto -> bb").and_then(|rest| rest.parse::<usize>().ok()).is_some_and(|target| target != *number)) {
            let (from, list) = text.blocks.remove(at);
            let target: usize = list[0].text["goto -> bb".len()..].parse().unwrap_or(from);
            for (_, list) in text.blocks.iter_mut() {
                for element in list.iter_mut() {
                    element.text = renumber(&element.text, &|local| format!("_{local}"), &|block| if block == from { target } else { block });
                }
            }
            changed = true;
        }
        // Rule 3b: a block that ends with a `goto` to a block with no other predecessor.
        let counts = references(text);
        let merge = text.blocks.iter().enumerate().find_map(|(at, (number, list))| {
            let target: usize = list.last()?.text.strip_prefix("goto -> bb")?.parse().ok()?;
            (target != *number && Some(target) != entry && counts.get(&target) == Some(&1)).then_some((at, target))
        });
        if let Some((at, target)) = merge {
            if let Some(from) = text.blocks.iter().position(|(number, _)| *number == target) {
                let (_, moved) = text.blocks.remove(from);
                let at = if from < at { at - 1 } else { at };
                let list = &mut text.blocks[at].1;
                list.pop();
                list.extend(moved);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Rule 3c, in one block: a local with one write `_t = P` (P a local or fields of a local),
    // read only after the copy in the same block, with no write of the local of P between, and
    // no borrow of the two locals in the body.
    let borrowed = borrowed(text);
    for local in std::iter::once(dest).chain(fresh.iter().copied()) {
        let mentions: Vec<(usize, usize, Use)> = text.blocks.iter().enumerate().flat_map(|(block, (_, list))| list.iter().enumerate().flat_map(move |(index, element)| uses(&element.text).into_iter().filter(move |(found, _)| *found == local).map(move |(_, kind)| (block, index, kind)))).collect();
        let writes: Vec<&(usize, usize, Use)> = mentions.iter().filter(|(_, _, kind)| *kind != Use::Read).collect();
        let [(block, index, Use::Write)] = writes[..] else { continue };
        let (block, index) = (*block, *index);
        let element = &text.blocks[block].1[index].text;
        let Some(value) = element.strip_prefix(&format!("_{local} = ")) else { continue };
        let Some((place, source)) = operand_place(value) else { continue };
        if borrowed.contains(&local) || borrowed.contains(&source) || source == local {
            continue;
        }
        let reads: Vec<&(usize, usize, Use)> = mentions.iter().filter(|(_, _, kind)| *kind == Use::Read).collect();
        if reads.is_empty() || reads.iter().any(|(at_block, at, _)| *at_block != block || *at <= index) {
            continue;
        }
        let last = reads.iter().map(|(_, at, _)| *at).max().unwrap_or(index);
        let list = &text.blocks[block].1;
        // No element between the copy and the last read writes the local of P, or names it in a
        // call or a drop (the last read can be such an element: it reads P first).
        let blocked = (index + 1..last).any(|at| uses(&list[at].text).iter().any(|(found, kind)| *found == source && *kind != Use::Read)) || (index + 1..=last).any(|at| uses(&list[at].text).iter().any(|(found, kind)| *found == source && *kind == Use::Write));
        if blocked {
            continue;
        }
        let place = place.clone();
        let list = &mut text.blocks[block].1;
        for element in list.iter_mut().skip(index + 1).take(last - index) {
            element.text = renumber(&element.text, &|found| if found == local { place.clone() } else { format!("_{found}") }, &|block| block);
        }
        list.remove(index);
    }
}

/// The reborrow chain of an argument (Decided by Claude, design 94): a local `_r` of an argument
/// of the call with one write `_r = &shared (*_p)` (or `&mut`), each other mention a deref
/// `(*_r)`, and a local `_p` that the body never writes, is the place `*_p`: the two pointers
/// have one address, and no write changes `_p`. The deref reads `*_p`, and the reborrow goes.
fn reborrows(text: &mut Text, passed: &[usize]) {
    for local in passed {
        let mut write: Option<(usize, usize, usize)> = None;
        let mut fits = true;
        for (block, (_, list)) in text.blocks.iter().enumerate() {
            for (index, element) in list.iter().enumerate() {
                for (start, end, number) in numbers(&element.text) {
                    if number != Num::Local(*local) {
                        continue;
                    }
                    let text = &element.text;
                    if start == 0 && end < text.len() && text[end..].starts_with(" = &") {
                        let value = &text[end + 3..];
                        let source = ["&shared (*_", "&mut (*_"].iter().find_map(|marker| value.strip_prefix(marker)).and_then(|rest| rest.strip_suffix(')')).and_then(|digits| digits.parse::<usize>().ok());
                        match (source, write) {
                            (Some(source), None) => write = Some((block, index, source)),
                            _ => fits = false,
                        }
                    } else if !(text[..start].ends_with("(*") && text[end..].starts_with(')')) {
                        fits = false;
                    }
                }
            }
        }
        let Some((block, index, source)) = write.filter(|_| fits) else { continue };
        let written = text.elements().flat_map(|element| uses(&element.text)).any(|(found, kind)| found == source && kind == Use::Write);
        if written {
            continue;
        }
        for (_, list) in text.blocks.iter_mut() {
            for element in list.iter_mut() {
                element.text = element.text.replace(&format!("(*_{local})"), &format!("(*_{source})"));
            }
        }
        text.blocks[block].1.remove(index);
    }
}

// ---------------------------------------------------------------------------------------------
// The candidates of a body pair.

/// The data of one side of a body pair: the text, the element ranges, and the name.
pub struct Side<'a> {
    pub name: &'a str,
    pub text: &'a str,
    pub record: &'a RecordElements,
}

/// The places of a list of elements (the tokens and the call).
fn places(list: &[Element]) -> BTreeSet<(Option<Place>, Option<Place>)> {
    list.iter().map(|element| (element.tokens.clone(), element.call.clone())).collect()
}

/// [R94.1-1] [R94.2-1] Checks one candidate: the callee `callee` put in at its call in the side
/// `at` (1 for an extract: the new body; 0 for an inline: the old body), then the alignment with
/// the other side. `before` is the element diff of the body pair before the check.
pub fn check(sides: [&Side; 2], callee: &Side, kind: Kind, before: &ElementDiff) -> Vec<Transform> {
    let at = if kind == Kind::Extract { 1 } else { 0 };
    let other = 1 - at;
    let refused = |cause: String, call: Option<Place>| Transform { kind, callee: callee.name.to_string(), call, refused: Some(cause), ..Default::default() };
    let (Some(outer), Some(inner), Some(peer)) = (parse_text(sides[at].text, sides[at].record), parse_text(callee.text, callee.record), parse_text(sides[other].text, sides[other].record)) else {
        return vec![refused("a text that the check does not read".into(), None)];
    };
    if outer.elements().count() > MOST_ELEMENTS || peer.elements().count() > MOST_ELEMENTS || inner.elements().count() > MOST_ELEMENTS {
        return vec![refused(format!("a body with more than {MOST_ELEMENTS} elements"), None)];
    }
    // A quick test before each candidate (a necessary condition of "holds"): at least half of
    // the element places of the callee have each key of their elements in the other version.
    let key_of = |text: &str| -> String {
        let mut out = String::new();
        let mut at = 0;
        for (start, end, _) in numbers(text) {
            out.push_str(&text[at..start]);
            out.push('#');
            at = end;
        }
        out.push_str(&text[at..]);
        out.replace("unwind continue", "unwind: bb#")
    };
    let other_keys: BTreeSet<String> = peer.elements().map(|element| key_of(&element.text)).collect();
    // An element that names a parameter or the return place can change its key in the inline
    // (a parameter becomes a place of the caller): it counts as equal here.
    let open_key = |text: &str| matches!(text, "return" | "resume") || numbers(text).iter().any(|(_, _, number)| matches!(number, Num::Local(local) if *local <= inner.args()));
    let mut by_place: BTreeMap<(Option<Place>, Option<Place>), bool> = BTreeMap::new();
    for element in inner.elements().filter(|element| element.tokens.is_some() || element.call.is_some()) {
        let entry = by_place.entry((element.tokens.clone(), element.call.clone())).or_insert(true);
        *entry &= open_key(&element.text) || other_keys.contains(&key_of(&element.text));
    }
    if by_place.is_empty() || 2 * by_place.values().filter(|equal| **equal).count() < by_place.len() {
        return Vec::new();
    }
    let marker = format!(" {{{}}}(", callee.name);
    let calls: Vec<(usize, usize)> = outer.blocks.iter().enumerate().flat_map(|(block, (_, list))| list.iter().enumerate().filter(|(_, element)| element.text.contains(&marker)).map(move |(index, _)| (block, index))).collect();
    let mut out = Vec::new();
    let before_places = [places(&before.old), places(&before.new)];
    for (block, index) in calls.into_iter().take(MOST_CANDIDATES) {
        let element = &outer.blocks[block].1[index];
        let call_place = element.tokens.clone();
        // Only a call that is new code (an element that differs) is a candidate.
        if !before_places[at].contains(&(element.tokens.clone(), element.call.clone())) {
            continue;
        }
        let inlined = match inline((&outer, sides[at].record), (block, index), (&inner, callee.record), callee.name, sides[at].name) {
            Ok(inlined) => inlined,
            Err(cause) => {
                out.push(refused(cause, call_place));
                continue;
            }
        };
        let printed = inlined.text.print();
        let record = inlined.text.record(&inlined.files, &inlined.table, sides[at].record.o, sides[at].record.v.min(callee.record.v), sides[at].record.c, &inlined.declarations);
        let keys = order_keys(&inlined.text);
        let peer_keys = order_keys(&peer);
        let found = if at == 1 {
            elements::diff_keyed((sides[0].text, sides[0].record), (&printed, &record), [Some(&peer_keys), Some(&keys)])
        } else {
            elements::diff_keyed((&printed, &record), (sides[1].text, sides[1].record), [Some(&keys), Some(&peer_keys)])
        };
        let Some(after) = found else {
            out.push(refused("the alignment has no data for the text with the callee".into(), call_place));
            continue;
        };
        let after_places = [places(&after.old), places(&after.new)];
        let explained: Vec<Place> = before_places[other].difference(&after_places[other]).filter_map(|(tokens, _)| tokens.clone()).collect();
        let callee_places: BTreeSet<(Option<Place>, Option<Place>)> = inlined.text.elements().filter(|element| element.callee && (element.tokens.is_some() || element.call.is_some())).map(|element| (element.tokens.clone(), element.call.clone())).collect();
        let callee_equal = callee_places.iter().filter(|place| !after_places[at].contains(place)).count();
        // Each element of the callee that differs has an element of the same form that differs on
        // the other side (the form: the text with no number of a local, a block, or a value): a
        // changed operand, not new code. A callee with new code is no extract.
        let form = |text: &str| -> String { key_of(text).chars().filter(|c| !c.is_ascii_digit()).collect() };
        let other_forms: BTreeSet<String> = peer.elements().filter(|element| after_places[other].contains(&(element.tokens.clone(), element.call.clone()))).map(|element| form(&element.text)).collect();
        let alone = inlined.text.elements().filter(|element| element.callee && after_places[at].contains(&(element.tokens.clone(), element.call.clone()))).any(|element| !other_forms.contains(&form(&element.text)));
        let holds = !explained.is_empty() && !callee_places.is_empty() && 2 * callee_equal >= callee_places.len() && !alone;
        out.push(Transform {
            kind,
            callee: callee.name.to_string(),
            call: call_place,
            refused: None,
            holds,
            tie: false,
            explained: explained.len(),
            callee_places: callee_places.len(),
            callee_equal,
            region: explained,
            callee_at: callee_places.iter().filter_map(|(tokens, _)| tokens.clone()).collect(),
            left: [after.old.clone(), after.new.clone()],
            equal: after.old.is_empty() && after.new.is_empty() && !after.other,
        });
    }
    out
}

/// [R94.1-3] Marks the ties of the candidates of one body pair: two candidates that hold and
/// explain a place of the same region (a call of a different callee, or a different call).
pub fn mark_ties(list: &mut [Transform]) {
    let holding: Vec<usize> = (0..list.len()).filter(|at| list[*at].holds).collect();
    let mut tied = BTreeSet::new();
    for (i, a) in holding.iter().enumerate() {
        for b in &holding[i + 1..] {
            let region: BTreeSet<&Place> = list[*a].region.iter().collect();
            if list[*b].region.iter().any(|place| region.contains(place)) {
                tied.insert(*a);
                tied.insert(*b);
            }
        }
    }
    for at in tied {
        list[at].tie = true;
    }
}

// ---------------------------------------------------------------------------------------------
// A new order of the same parts (R94.4-2, R94.4-3).

/// The class of an element for the dependence rules.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Statement,
    Call,
    Assert,
    Drop,
    Jump,
}

fn class(text: &str) -> Class {
    if text.starts_with("assert(") {
        Class::Assert
    } else if text.starts_with("drop(") {
        Class::Drop
    } else if text.starts_with("goto ") || text.starts_with("switchInt(") || matches!(text, "return" | "resume" | "unreachable" | "abort") {
        Class::Jump
    } else if text.contains("-> [return: ") || text.contains("-> [no return") {
        Class::Call
    } else {
        Class::Statement
    }
}

/// The key of the order of the source of an element (design 40.3, rule 3): the file, the start,
/// and the end from the largest value (a longer place first).
fn source_key(place: &Place) -> OrderKey {
    (place.file.clone(), vec![(place.start, usize::MAX - place.end)])
}

/// The key of a `switchInt` element with its values in one order: a switch on distinct values
/// has the same meaning in each order of the values.
fn sorted_switch(text: &str) -> String {
    let Some((head, list)) = text.split_once(" -> [") else { return text.to_string() };
    if !head.starts_with("switchInt(") {
        return text.to_string();
    }
    let Some(list) = list.strip_suffix(']') else { return text.to_string() };
    let mut parts = split_list(list);
    let otherwise = parts.iter().position(|part| part.starts_with("otherwise: ")).map(|at| parts.remove(at));
    parts.sort_by(|a, b| a.split_once(": ").map(|(value, _)| value).cmp(&b.split_once(": ").map(|(value, _)| value)));
    parts.extend(otherwise);
    format!("{head} -> [{}]", parts.join(", "))
}

/// One element of a side of the order check.
struct Item {
    key: String,
    numbers: Vec<Num>,
    /// The chain of the element and its place in the chain.
    chain: usize,
    position: usize,
    place: Option<Place>,
    order: OrderKey,
    text: String,
    /// The pairs of numbers (indexes of `numbers`) that are the two operands of a symmetric
    /// operation: the printer gives them the order of their local numbers (design 47, G1).
    swaps: Vec<(usize, usize)>,
}

/// The pairs of the indexes of the local numbers of a text that are the two operands of a
/// symmetric operation (`elements::SYMMETRIC`), each operand with one local.
fn symmetric_pairs(text: &str) -> Vec<(usize, usize)> {
    let spans: Vec<(usize, usize, Num)> = numbers(text);
    let mut out = Vec::new();
    for op in elements::SYMMETRIC {
        let mut from = 0;
        while let Some(found) = text[from..].find(op) {
            let at = from + found;
            from = at + op.len();
            if at > 0 && text[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let open = at + op.len() - 1;
            let Some(close) = matching(text, open) else { continue };
            let inner = &text[open + 1..close];
            let parts = split_list(inner);
            let [a, b] = parts.as_slice() else { continue };
            let (a_start, b_start) = (open + 1, open + 1 + a.len() + 2);
            let locals_in = |start: usize, end: usize| -> Vec<usize> { spans.iter().enumerate().filter(|(_, (s, _, number))| *s >= start && *s < end && matches!(number, Num::Local(_))).map(|(index, _)| index).collect() };
            if let ([x], [y]) = (locals_in(a_start, a_start + a.len()).as_slice(), locals_in(b_start, b_start + b.len()).as_slice()) {
                // The index in the list of the numbers that the key keeps (each number).
                out.push((*x, *y));
            }
        }
    }
    out
}

/// The elements of a text for the order check: the straight runs of blocks (a chain: a block
/// and the block of its normal successor when that block has no other predecessor), the keys
/// with the edges in a chain as `bb>` and the other targets as their chains.
fn items(text: &Text) -> Vec<Item> {
    let counts = references(text);
    let entry = text.blocks.first().map(|(number, _)| *number);
    let successor = |list: &[Elem]| -> Option<usize> {
        let last = list.last()?;
        let text = &last.text;
        let target = if let Some(rest) = text.strip_prefix("goto -> bb") {
            rest.parse().ok()
        } else {
            ["-> [success: bb", "-> [return: bb"].iter().find_map(|marker| text.split_once(marker).map(|(_, rest)| rest.chars().take_while(char::is_ascii_digit).collect::<String>())).and_then(|digits| digits.parse().ok())
        };
        target.filter(|target| Some(*target) != entry && counts.get(target) == Some(&1))
    };
    let by_number: BTreeMap<usize, &Vec<Elem>> = text.blocks.iter().map(|(number, list)| (*number, list)).collect();
    let next: BTreeMap<usize, usize> = text.blocks.iter().filter_map(|(number, list)| successor(list).filter(|target| target != number && by_number.contains_key(target)).map(|target| (*number, target))).collect();
    let inner: BTreeSet<usize> = next.values().copied().collect();
    let mut chain_of: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    let mut starts: Vec<usize> = text.blocks.iter().map(|(number, _)| *number).filter(|number| !inner.contains(number)).collect();
    starts.extend(text.blocks.iter().map(|(number, _)| *number).filter(|number| inner.contains(number)));
    let mut position_of: BTreeMap<usize, usize> = BTreeMap::new();
    for start in starts {
        if chain_of.contains_key(&start) {
            continue;
        }
        let mut block = start;
        let mut position = 0;
        loop {
            chain_of.insert(block, (start, position));
            position += by_number.get(&block).map_or(0, |list| list.len());
            match next.get(&block) {
                Some(target) if !chain_of.contains_key(target) => block = *target,
                _ => break,
            }
        }
        position_of.insert(start, position);
    }
    let mut out = Vec::new();
    for (number, list) in &text.blocks {
        let (chain, first) = chain_of.get(number).copied().unwrap_or((*number, 0));
        let places: Vec<Option<OrderKey>> = list.iter().map(|element| elements::place_of(&(element.tokens.clone(), element.call.clone())).map(source_key)).collect();
        for (index, element) in list.iter().enumerate() {
            let text = sorted_switch(&element.text);
            let is_last = index + 1 == list.len();
            let mut key = String::with_capacity(text.len());
            let mut found = Vec::new();
            let mut at = 0;
            for (start, end, found_number) in numbers(&text) {
                key.push_str(&text[at..start]);
                match found_number {
                    Num::Local(local) => {
                        key.push('_');
                        found.push(Num::Local(local));
                    }
                    Num::Block(block) if is_last && next.get(number) == Some(&block) => key.push_str("bb>"),
                    Num::Block(block) => {
                        key.push_str("bb");
                        found.push(Num::Block(chain_of.get(&block).map_or(usize::MAX, |(chain, _)| *chain)));
                    }
                }
                at = end;
            }
            key.push_str(&text[at..]);
            let order = places[index].clone().or_else(|| places[index + 1..].iter().flatten().next().cloned()).or_else(|| places[..index].iter().rev().flatten().next().cloned()).unwrap_or_else(|| (String::new(), vec![(usize::MAX, 0)]));
            let swaps = symmetric_pairs(&text);
            out.push(Item { key, numbers: found, chain, position: first + index, place: element.tokens.clone(), order, text: element.text.clone(), swaps });
        }
    }
    out
}

/// [R94.4-3] The cause of a dependence of two elements, or `None` if they are independent.
fn depends(a: &str, b: &str) -> Option<String> {
    let (x, y) = (class(a), class(b));
    if x == Class::Jump || y == Class::Jump {
        return Some("a jump between the parts".into());
    }
    let sets = |text: &str| {
        let mut reads = BTreeSet::new();
        let mut writes = BTreeSet::new();
        for (local, kind) in uses(text) {
            match kind {
                Use::Read => {
                    reads.insert(local);
                }
                Use::Write => {
                    writes.insert(local);
                }
                Use::Borrow => {
                    reads.insert(local);
                    writes.insert(local);
                }
            }
        }
        (reads, writes)
    };
    let ((reads_a, writes_a), (reads_b, writes_b)) = (sets(a), sets(b));
    if writes_a.iter().any(|local| reads_b.contains(local) || writes_b.contains(local)) || writes_b.iter().any(|local| reads_a.contains(local)) {
        return Some("a place that one part writes and the other part reads or writes".into());
    }
    let effect = |class: Class, text: &str| matches!(class, Class::Call | Class::Drop) || writes_memory(text);
    let panics = |class: Class| matches!(class, Class::Call | Class::Assert | Class::Drop);
    let memory = |text: &str| reads_memory(text) || writes_memory(text);
    if effect(x, a) && effect(y, b) {
        return Some("two parts with an effect each (a call, a drop, or a write through a reference)".into());
    }
    if panics(x) && panics(y) {
        return Some("two parts that each can panic".into());
    }
    if (panics(x) && (effect(y, b) || memory(b))) || (panics(y) && (effect(x, a) || memory(a))) {
        return Some("a part that can panic and a part with an effect or a read through a reference".into());
    }
    if (effect(x, a) && memory(b)) || (effect(y, b) && memory(a)) {
        return Some("a part with an effect and a read through a reference".into());
    }
    None
}

/// [R94.4-2] [R94.4-3] The new order of the same parts of a body pair: `None` if the two texts
/// do not have the same elements under one map of the locals and of the chains of blocks (then
/// the change is not only a new order). The pairs of elements whose order changes are checked
/// for a dependence (R94.4-3).
pub fn permutation(old: (&str, &RecordElements), new: (&str, &RecordElements)) -> Option<Reorder> {
    let (a, b) = (parse_text(old.0, old.1)?, parse_text(new.0, new.1)?);
    if a.arg_types != b.arg_types || a.ret != b.ret {
        return None;
    }
    let (left, right) = (items(&a), items(&b));
    if left.len() != right.len() || left.len() > MOST_ELEMENTS {
        return None;
    }
    let mut sorted = [left.iter().map(|item| item.key.as_str()).collect::<Vec<_>>(), right.iter().map(|item| item.key.as_str()).collect::<Vec<_>>()];
    sorted[0].sort_unstable();
    sorted[1].sort_unstable();
    if sorted[0] != sorted[1] {
        return None;
    }
    // The order of the source, the alignment, and the pairs of the elements left by their keys.
    let order = |list: &[Item]| -> Vec<usize> {
        let mut indexes: Vec<usize> = (0..list.len()).collect();
        indexes.sort_by(|x, y| (&list[*x].order, *x).cmp(&(&list[*y].order, *y)));
        indexes
    };
    let (order_a, order_b) = (order(&left), order(&right));
    let keys_a: Vec<String> = order_a.iter().map(|at| left[*at].key.clone()).collect();
    let keys_b: Vec<String> = order_b.iter().map(|at| right[*at].key.clone()).collect();
    let mut partner: Vec<Option<usize>> = vec![None; left.len()];
    let mut taken = vec![false; right.len()];
    for (i, j) in elements::align(&keys_a, &keys_b, |_, _| false) {
        partner[order_a[i]] = Some(order_b[j]);
        taken[order_b[j]] = true;
    }
    let mut moved: Vec<usize> = Vec::new();
    let open: Vec<usize> = order_a.iter().copied().filter(|at| partner[*at].is_none()).collect();
    for at in open {
        let found = order_b.iter().copied().find(|other| !taken[*other] && right[*other].key == left[at].key)?;
        partner[at] = Some(found);
        taken[found] = true;
        moved.push(at);
    }
    // One map of the locals (the return local and the arguments map to themselves, with equal
    // types) and one map of the chains.
    let args = a.args();
    let mut locals: BTreeMap<usize, usize> = (0..=args).map(|local| (local, local)).collect();
    let mut back: BTreeMap<usize, usize> = locals.clone();
    let mut chains: BTreeMap<usize, usize> = BTreeMap::new();
    let mut chains_back: BTreeMap<usize, usize> = BTreeMap::new();
    fn bind(map: &mut BTreeMap<usize, usize>, back: &mut BTreeMap<usize, usize>, x: usize, y: usize) -> bool {
        match (map.get(&x), back.get(&y)) {
            (Some(found), _) if *found != y => false,
            (_, Some(found)) if *found != x => false,
            _ => {
                map.insert(x, y);
                back.insert(y, x);
                true
            }
        }
    }
    let entry = (a.blocks.first()?.0, b.blocks.first()?.0);
    if !bind(&mut chains, &mut chains_back, entry.0, entry.1) {
        return None;
    }
    let typed = |text: &Text, local: usize| -> Option<String> { text.locals.get(&local).cloned().or_else(|| local.checked_sub(1).and_then(|at| text.arg_types.get(at)).cloned()) };
    // The elements with a symmetric operation come last: their operands can go in two orders.
    let mut pairs: Vec<(usize, usize)> = partner.iter().enumerate().map(|(x, y)| Some((x, (*y)?))).collect::<Option<Vec<_>>>()?;
    pairs.sort_by_key(|(x, _)| !left[*x].swaps.is_empty());
    for (x, y) in pairs {
        let (p, q) = (&left[x], &right[y]);
        if p.numbers.len() != q.numbers.len() || !bind(&mut chains, &mut chains_back, p.chain, q.chain) {
            return None;
        }
        let attempt = |numbers: &[Num], maps: &mut [BTreeMap<usize, usize>; 4]| -> bool {
            let [locals, back, chains, chains_back] = maps;
            p.numbers.iter().zip(numbers).all(|(m, n)| match (m, n) {
                (Num::Local(m), Num::Local(n)) => typed(&a, *m) == typed(&b, *n) && bind(locals, back, *m, *n),
                (Num::Block(m), Num::Block(n)) => bind(chains, chains_back, *m, *n),
                _ => false,
            })
        };
        let mut maps = [locals.clone(), back.clone(), chains.clone(), chains_back.clone()];
        let mut fits = attempt(&q.numbers, &mut maps);
        if !fits && !q.swaps.is_empty() {
            let mut swapped = q.numbers.clone();
            for (i, j) in &q.swaps {
                swapped.swap(*i, *j);
            }
            maps = [locals.clone(), back.clone(), chains.clone(), chains_back.clone()];
            fits = attempt(&swapped, &mut maps);
        }
        if !fits {
            return None;
        }
        [locals, back, chains, chains_back] = maps;
    }
    // [R94.4-3] The pairs whose order changes in a chain, and their dependence. Each pair of
    // one chain is compared (the order of the source and the order of a run can differ), up to
    // `MOST_PAIRS` pairs; above it, only the pairs of an element that the alignment moved.
    let mut out = Reorder::default();
    let mut by_chain: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for at in 0..left.len() {
        by_chain.entry(left[at].chain).or_default().push(at);
    }
    let all = by_chain.values().map(|list| list.len() * list.len()).sum::<usize>() <= MOST_PAIRS;
    let moved: BTreeSet<usize> = moved.into_iter().collect();
    for list in by_chain.values() {
        for (i, x) in list.iter().enumerate() {
            for y in &list[i + 1..] {
                if !all && !moved.contains(x) && !moved.contains(y) {
                    continue;
                }
                let (x_new, y_new) = (partner[*x]?, partner[*y]?);
                if (left[*x].position < left[*y].position) == (right[x_new].position < right[y_new].position) {
                    continue;
                }
                out.flips += 1;
                for at in [x, y] {
                    if let Some(place) = &left[*at].place
                        && !out.moved.contains(place)
                    {
                        out.moved.push(place.clone());
                    }
                }
                if let Some(cause) = depends(&left[*x].text, &left[*y].text)
                    && out.dependent.len() < 4
                {
                    out.dependent.push((left[*x].place.clone(), left[*y].place.clone(), cause));
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;
