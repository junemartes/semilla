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
    /// [R95.3-1] [R95.4-1] [R95.5-1] [R95.5-2] [R95.6-1] The rules of design 95 that the check
    /// used, with a short text each (the most specific rule first).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<(String, String)>,
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
    /// [R95.1-1] The places of the elements of each side (old 0, new 1) that are not a part of
    /// the new order: the real edits of the body (each one an element that differs in the map
    /// of design 90). Empty: the change is only a new order.
    #[serde(default, skip_serializing_if = "residue_empty")]
    pub residue: [Vec<Place>; 2],
    /// [R95.2-1] The count of the calls of the moved parts whose effect summary has no effect.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pure_calls: usize,
}

fn residue_empty(residue: &[Vec<Place>; 2]) -> bool {
    residue.iter().all(Vec::is_empty)
}

fn is_zero(count: &usize) -> bool {
    *count == 0
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
    /// [R95.3-1] The generic parameters of the signature line (` generics <T#0, N#1>`): the
    /// name and the index of each one, in order. An unnamed parameter (of a closure) has an
    /// empty name.
    generics: Vec<(String, usize)>,
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
    let (rest, generics) = match rest.split_once(" generics <") {
        Some((ret, list)) => {
            let mut generics = Vec::new();
            for part in split_list(list.strip_suffix('>')?) {
                let (name, index) = part.rsplit_once('#')?;
                generics.push((name.to_string(), index.parse().ok()?));
            }
            (ret, generics)
        }
        None => (rest, Vec::new()),
    };
    let mut arg_types = Vec::new();
    for (index, part) in split_list(args).into_iter().enumerate() {
        let (name, ty) = part.split_once(": ")?;
        if name != format!("_{}", index + 1) {
            return None;
        }
        arg_types.push(ty.to_string());
    }
    let mut out = Text { arg_types, ret: rest.to_string(), generics, locals: BTreeMap::new(), blocks: Vec::new() };
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
        let generics: Vec<String> = self.generics.iter().map(|(name, index)| format!("{name}#{index}")).collect();
        let generics = if generics.is_empty() { String::new() } else { format!(" generics <{}>", generics.join(", ")) };
        out.push_str(&format!("fn _({}) -> {}{generics} {{\n", args.join(", "), self.ret));
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
// [R95.2-1] The effect summary of a call, from the records of the driver.

/// [R95.2-1] The facts of one call of a body from the form of the equivalence check (the field
/// `q` of a record, design 61): the effect summary of R64.3-1 and the body that rustc resolves
/// the call to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallFact {
    /// False if the call has no summary (the call reads and writes the world).
    pub summary: bool,
    /// The cause of an effect on the world. `None`: the summary has no such effect.
    pub world: Option<String>,
    /// True if the call can stop with no return value (a panic, a loop).
    pub panics: bool,
    /// [R95.4-1] The name of the body of the workspace that the call runs (the driver resolves
    /// the instance of the call): an impl method for a trait method on a concrete type.
    pub resolved: Option<String>,
}

impl CallFact {
    /// True if the summary is complete and has no effect on the world: no write, no I/O, and
    /// no other effect (R64.3-1). It can still panic.
    pub fn pure(&self) -> bool {
        self.summary && self.world.is_none()
    }
}

/// The facts of the calls of a body, by the source place of the call.
pub type CallFacts = BTreeMap<Place, Vec<CallFact>>;

/// [R95.2-1] The call facts of a form of the equivalence check. `None` for a form that the
/// check does not read (no form, or a body with no form: the mark `u`).
pub fn call_facts(form: &str) -> Option<CallFacts> {
    let value: serde_json::Value = serde_json::from_str(form).ok()?;
    if value.get("u").is_some_and(|cause| !cause.is_null()) {
        return None;
    }
    let files: Vec<String> = value.get("f")?.as_array().map(|list| list.iter().filter_map(|file| file.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let mut out: CallFacts = BTreeMap::new();
    for block in value.get("b")?.as_array()? {
        let Some(term) = block.get("t") else { continue };
        let Some(call) = term.get("k").and_then(|kind| kind.get("Call")).and_then(|call| call.get(0)) else { continue };
        let Some([file, start, end]) = term.get("r").and_then(|place| place.as_array()).map(|place| place.iter().map(|part| part.as_i64()).collect::<Vec<_>>()).as_deref().map(|place| [place.first().copied().flatten(), place.get(1).copied().flatten(), place.get(2).copied().flatten()]) else { continue };
        let (Some(file), Some(start), Some(end)) = (file.and_then(|file| usize::try_from(file).ok()).and_then(|file| files.get(file)), start.and_then(|start| usize::try_from(start).ok()), end.and_then(|end| usize::try_from(end).ok())) else { continue };
        let summary = call.get("e").filter(|summary| summary.is_object());
        let fact = CallFact {
            summary: summary.is_some(),
            world: summary.and_then(|summary| summary.get("w")).and_then(|cause| cause.as_str()).map(str::to_string),
            panics: summary.is_none_or(|summary| summary.get("p").and_then(|stop| stop.as_bool()).unwrap_or(false)),
            resolved: call.get("w").and_then(|name| name.as_str()).map(str::to_string),
        };
        out.entry(Place { file: file.clone(), start, end }).or_default().push(fact);
    }
    Some(out)
}

/// [R95.2-1] The summary of the call element at a place: `Some((pure, panics))` if each call of
/// the form at the place has a complete summary; `None` if a call has no summary or the form has
/// no call at the place (then the call is an effect, as before).
fn summary_at(facts: Option<&CallFacts>, place: Option<&Place>) -> Option<(bool, bool)> {
    let list = facts?.get(place?)?;
    if list.is_empty() || list.iter().any(|fact| !fact.summary) {
        return None;
    }
    Some((list.iter().all(CallFact::pure), list.iter().any(|fact| fact.panics)))
}

// ---------------------------------------------------------------------------------------------
// [R95.3-1] [R95.4-1] The generic arguments of a call.

/// The primitive types of the language (a language fact): a generic parameter with such a name
/// hides the type in the text, so the check does not replace it.
const PRIMITIVES: &[&str] = &["bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f16", "f32", "f64", "f128"];

fn ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The end of the type text that starts at `at`: the first `,`, `;`, ` as `, or closing bracket
/// at the depth 0.
fn type_end(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut index = at;
    while index < bytes.len() {
        match bytes[index] {
            b'>' if index > 0 && bytes[index - 1] == b'-' => {}
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            b'>' | b')' | b']' | b'}' if depth == 0 => return index,
            b'>' | b')' | b']' | b'}' => depth -= 1,
            b',' | b';' if depth == 0 => return index,
            b' ' if depth == 0 && text[index..].starts_with(" as ") => return index,
            _ => {}
        }
        index += 1;
    }
    index
}

/// The path of a call text with no generic arguments (`depot::Wrap::<u32>::get` gives
/// `depot::Wrap::get`, `<depot::Wrap<u32> as depot::Tax>::rate` gives `<depot::Wrap as
/// depot::Tax>::rate`): a bracket group `<…>` after a name or after `::` goes.
pub fn skeleton(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    let mut index = 0;
    while index < bytes.len() {
        let after_name = index > 0 && (ident_char(bytes[index - 1]) || bytes[index - 1] == b':');
        if bytes[index] == b'<' && after_name {
            let mut depth = 0i32;
            let mut end = index;
            while end < bytes.len() {
                match bytes[end] {
                    b'>' if end > 0 && bytes[end - 1] == b'-' => {}
                    b'<' => depth += 1,
                    b'>' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                end += 1;
            }
            while out.ends_with(':') {
                out.pop();
            }
            index = end + 1;
            continue;
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    out
}

/// [R95.3-1] The generic arguments of a call for a body with the name `name` and the generic
/// parameters `params`, in the order of the parameters. A parameter that the name shows
/// (`depot::Wrap::<T>::get`, `<depot::Wrap<T> as depot::Tax>::rate`) takes the type at its place
/// in the call path; the other parameters take the arguments after the name
/// (`depot::part::<u32>`), in order. A lifetime argument is not a parameter of the text. `None`
/// if the two paths do not fit.
pub fn bind_args(call: &str, name: &str, params: &[String]) -> Option<Vec<String>> {
    let mut bound: BTreeMap<&str, String> = BTreeMap::new();
    let (c, n) = (call.as_bytes(), name.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);
    while j < n.len() {
        let starts = j == 0 || !(ident_char(n[j - 1]) || n[j - 1] == b'\'' || n[j - 1] == b':' || n[j - 1] == b'.');
        if starts && ident_char(n[j]) {
            let end = name[j..].bytes().position(|byte| !ident_char(byte)).map_or(n.len(), |found| j + found);
            let word = &name[j..end];
            if params.iter().any(|param| param == word) {
                let stop = type_end(call, i);
                let value = &call[i..stop];
                if value.is_empty() {
                    return None;
                }
                match bound.get(word) {
                    Some(known) if known != value => return None,
                    _ => {
                        bound.insert(word, value.to_string());
                    }
                }
                (i, j) = (stop, end);
                continue;
            }
        }
        if i >= c.len() || c[i] != n[j] {
            return None;
        }
        i += 1;
        j += 1;
    }
    let rest = &call[i..];
    let trailing: Vec<String> = if rest.is_empty() { Vec::new() } else { split_list(rest.strip_prefix("::<")?.strip_suffix('>')?).into_iter().filter(|arg| !arg.starts_with('\'')).collect() };
    let open = params.iter().filter(|param| !bound.contains_key(param.as_str())).count();
    if open != trailing.len() {
        return None;
    }
    let mut trailing = trailing.into_iter();
    params.iter().map(|param| bound.get(param.as_str()).cloned().or_else(|| trailing.next())).collect()
}

/// The text with each whole word `from` replaced by `to` (not a part of a path, a field, a
/// lifetime, or a string constant), and each const parameter `param FROM#K` by `to`.
fn replace_param(text: &str, from: &str, index: usize, to: &str) -> String {
    let marker = format!("param {from}#{index}");
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut at, mut in_string, mut escaped) = (0usize, false, false);
    while at < bytes.len() {
        let c = bytes[at];
        if in_string {
            match c {
                b'\\' if !escaped => escaped = true,
                b'"' if !escaped => in_string = false,
                _ => escaped = false,
            }
            out.push(c as char);
            at += 1;
            continue;
        }
        if c == b'"' {
            in_string = true;
            out.push('"');
            at += 1;
            continue;
        }
        let starts = at == 0 || !(ident_char(bytes[at - 1]) || matches!(bytes[at - 1], b'\'' | b':' | b'.' | b'#'));
        if starts && text[at..].starts_with(&marker) && bytes.get(at + marker.len()).is_none_or(|next| !ident_char(*next)) {
            out.push_str(to);
            at += marker.len();
            continue;
        }
        if starts && text[at..].starts_with(from) && bytes.get(at + from.len()).is_none_or(|next| !ident_char(*next) && *next != b'#') {
            out.push_str(to);
            at += from.len();
            continue;
        }
        let width = text[at..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[at..at + width]);
        at += width;
    }
    out
}

/// [R95.3-1] The text of a generic callee with each generic parameter replaced by the argument
/// of the call: a type parameter at each place of its name, and a const parameter at each place
/// `param N#k`. `Err`: the cause why the check does not replace them.
fn substitute(text: &Text, args: &[String]) -> Result<Text, String> {
    if args.len() != text.generics.len() {
        return Err(format!("the call has {} generic argument(s), and the callee has {} generic parameter(s)", args.len(), text.generics.len()));
    }
    for (name, _) in &text.generics {
        if name.is_empty() {
            return Err("a generic parameter with no name".into());
        }
        if PRIMITIVES.contains(&name.as_str()) {
            return Err(format!("the generic parameter `{name}` has the name of a primitive type"));
        }
    }
    let all = |line: &str| -> String { text.generics.iter().zip(args).fold(line.to_string(), |line, ((name, index), arg)| replace_param(&line, name, *index, arg)) };
    let mut out = text.clone();
    out.generics.clear();
    out.arg_types = text.arg_types.iter().map(|ty| all(ty)).collect();
    out.ret = all(&text.ret);
    for ty in out.locals.values_mut() {
        *ty = all(ty);
    }
    for (_, list) in out.blocks.iter_mut() {
        for element in list.iter_mut() {
            element.text = all(&element.text);
        }
    }
    instance_form(&mut out);
    Ok(out)
}

/// True if a type text has no destructor by the rules of the language: a scalar, `()`, `!`, a
/// reference, a raw pointer, a function pointer, and a tuple or an array of such types.
fn no_drop_glue(ty: &str) -> bool {
    let ty = ty.trim();
    if PRIMITIVES.contains(&ty) && ty != "str" || matches!(ty, "()" | "!") || ty.starts_with('&') || ty.starts_with("*const ") || ty.starts_with("*mut ") || ty.starts_with("fn(") {
        return true;
    }
    if let Some(inner) = ty.strip_prefix('(').and_then(|rest| rest.strip_suffix(')')) {
        return split_list(inner.trim_end_matches(',')).iter().all(|part| no_drop_glue(part));
    }
    if let Some(inner) = ty.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
        return inner.rsplit_once("; ").is_some_and(|(item, _)| no_drop_glue(item));
    }
    false
}

/// [R95.3-1] The parts of the text of a generic body that the instance of the call does not have
/// (the compiler removes them in the MIR of an instance): an unwind edge to a block with only
/// `resume` is `unwind continue`, and a drop of a local whose type has no destructor (after the
/// replacement) is a jump to its target.
fn instance_form(text: &mut Text) {
    let resumes: BTreeSet<usize> = text.blocks.iter().filter(|(_, list)| list.len() == 1 && list[0].text == "resume").map(|(number, _)| *number).collect();
    let types: BTreeMap<usize, String> = text.locals.clone().into_iter().chain(text.arg_types.iter().enumerate().map(|(index, ty)| (index + 1, ty.clone()))).collect();
    for (_, list) in text.blocks.iter_mut() {
        for element in list.iter_mut() {
            for block in &resumes {
                element.text = element.text.replace(&format!("unwind: bb{block}]"), "unwind continue]");
            }
            if let Some(rest) = element.text.strip_prefix("drop(_")
                && let Some((local, tail)) = rest.split_once(") -> [return: bb")
                && let Ok(local) = local.parse::<usize>()
                && let Some(target) = tail.split(',').next().and_then(|digits| digits.parse::<usize>().ok())
                && types.get(&local).is_some_and(|ty| no_drop_glue(ty))
            {
                element.text = format!("goto -> bb{target}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The inline of a callee at a call (R94.1-1, R94.2-1).

/// [R95.3-1] [R95.4-1] The callee of a candidate: the text of the call (`depot::part::<u32>`,
/// `<depot::Food as depot::Tax>::rate`), the name of the body, and the generic arguments of the
/// call for the generic parameters of the body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    pub path: String,
    pub name: String,
    pub args: Vec<String>,
    /// [R95.6-1] True for a closure body that the body defines and calls.
    pub closure: bool,
}

/// The def paths of the direct calls of an element text: the text between ` {` and `}(` (a
/// qualified path `<A as B>::m` has spaces).
pub fn called_paths(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("}(") {
        let end = from + found;
        // The start: the ` {` whose group closes at `end`.
        let bytes = text.as_bytes();
        let (mut depth, mut start) = (0i32, None);
        for index in (0..end).rev() {
            match bytes[index] {
                b'}' => depth += 1,
                b'{' if depth == 0 => {
                    start = Some(index);
                    break;
                }
                b'{' => depth -= 1,
                _ => {}
            }
        }
        if let Some(start) = start.filter(|start| *start > 0 && bytes[start - 1] == b' ') {
            let path = &text[start + 1..end];
            let plain = (!path.contains(['(', ')', ' '])) || (path.starts_with('<') && path.contains(" as "));
            if !path.is_empty() && plain {
                out.push(path);
            }
        }
        from = end + 2;
    }
    out
}

/// The blocks that a walk of the edges from `entry` reaches.
fn reached_blocks(text: &Text, entry: usize) -> BTreeSet<usize> {
    let by_number: BTreeMap<usize, &Vec<Elem>> = text.blocks.iter().map(|(number, list)| (*number, list)).collect();
    let mut seen = BTreeSet::new();
    let mut stack = vec![entry];
    while let Some(block) = stack.pop() {
        if !seen.insert(block) {
            continue;
        }
        for element in by_number.get(&block).into_iter().flat_map(|list| list.iter()) {
            stack.extend(numbers(&element.text).into_iter().filter_map(|(_, _, number)| match number {
                Num::Block(next) => Some(next),
                Num::Local(_) => None,
            }));
        }
    }
    seen
}

/// [R95.5-2] The locals that an element of the blocks that `from` reaches names.
fn mentioned_after(text: &Text, from: &[usize]) -> BTreeSet<usize> {
    let by_number: BTreeMap<usize, &Vec<Elem>> = text.blocks.iter().map(|(number, list)| (*number, list)).collect();
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    let mut stack: Vec<usize> = from.to_vec();
    let mut out = BTreeSet::new();
    while let Some(block) = stack.pop() {
        if !seen.insert(block) {
            continue;
        }
        for element in by_number.get(&block).into_iter().flat_map(|list| list.iter()) {
            for (_, _, number) in numbers(&element.text) {
                match number {
                    Num::Local(local) => {
                        out.insert(local);
                    }
                    Num::Block(next) => stack.push(next),
                }
            }
        }
    }
    out
}

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
    /// [R95.5-2] The parameters that take the local of their argument with no copy.
    written: Vec<(usize, usize)>,
    /// [R95.5-3] The count of the values that go to their place with no local.
    folded: usize,
    /// [R95.5-1] The count of the writes of `()` of the callee with no reader that go.
    units: usize,
    text: Text,
    files: Vec<String>,
    table: Vec<elements::RecordSymbol>,
    declarations: BTreeMap<usize, Vec<i64>>,
}

/// [R94.1-1] Puts the callee `path` (its text and ranges) in at the call element `at` (block
/// index, element index) of `caller`. The new elements of the callee take the place of the call
/// in the order of the alignment. `Err`: the cause why the check cannot put it in.
fn inline(caller: (&Text, &RecordElements), at: (usize, usize), callee: (&Text, &RecordElements), target: &Target, caller_path: &str) -> Result<Inlined, String> {
    let (outer, outer_record) = caller;
    let (generic, inner_record) = callee;
    let path = target.path.as_str();
    let call_element = &outer.blocks[at.0].1[at.1];
    let call = parse_call(&call_element.text, path)?;
    // The conditions of the driver inliner (`semilla-mir`, `inline.rs`). A closure body comes
    // here only as a plain function (`prepare_closure`, R95.6-1).
    // [R95.3-1] A generic callee: each generic parameter of its text takes the argument of the
    // call. [R94.5-1] A call with generic arguments of a callee with no generic parameters does
    // not fit.
    let substituted;
    let inner = if generic.generics.is_empty() && target.args.is_empty() {
        generic
    } else {
        substituted = substitute(generic, &target.args)?;
        &substituted
    };
    let signature = format!("fn({}) -> {}", inner.arg_types.join(", "), inner.ret);
    if plain_fn_type(&call.fn_ty) != plain_fn_type(&signature) {
        return Err(format!("the type of the call `{}` is not the signature of the callee `{signature}`", call.fn_ty));
    }
    if call.args.len() != inner.args() {
        return Err("the count of the arguments is not the count of the parameters".into());
    }
    // A recursive callee: a call of the callee or of the caller, also with other generic
    // arguments (the same path with no generic arguments).
    let own = [skeleton(&target.name), skeleton(path), skeleton(caller_path)];
    if calls(inner, path) || calls(inner, caller_path) || inner.elements().any(|element| called_paths(&element.text).iter().any(|called| own.contains(&skeleton(called)))) {
        return Err("a recursive callee".into());
    }
    let dest = plain_local(&call.dest).ok_or_else(|| "the call writes a place that is not a local".to_string())?;
    let outer_borrowed = borrowed(outer);
    if outer_borrowed.contains(&dest) || call.args.iter().any(|arg| operand_place(arg).is_some_and(|(_, local)| local == dest)) {
        return Err("the caller borrows the local of the result, or passes it to the call".into());
    }
    // [R95.5-2] The locals of the caller that an element after the call can read (the blocks
    // that the return edge and the unwind edge of the call reach).
    let after_call = mentioned_after(outer, &[Some(call.target), call.unwind].into_iter().flatten().collect::<Vec<_>>());
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
    let mut written: Vec<(usize, usize)> = Vec::new();
    let mut locals = outer.locals.clone();
    for (index, arg) in call.args.iter().enumerate() {
        let param = index + 1;
        let read_only = param_uses.get(&param).is_none_or(|kinds| kinds.iter().all(|kind| *kind == Use::Read));
        // [R95.5-2] A parameter that the callee writes takes the local of the argument with no
        // copy when the caller does not read the local after the call: the argument is a plain
        // local, the caller does not borrow it, no other operand of the call names it, and no
        // element that the call reaches names it. The write is then not visible to the caller.
        let dead = plain_local(arg.strip_prefix("move ").unwrap_or(arg)).filter(|local| !outer_borrowed.contains(local) && *local != dest && !after_call.contains(local) && call.args.iter().filter(|other| numbers(other).iter().any(|(_, _, number)| *number == Num::Local(*local))).count() == 1);
        match operand_place(arg) {
            Some((place, local)) if read_only && !outer_borrowed.contains(&local) => {
                substitute.insert(param, place);
            }
            _ if dead.is_some() => {
                substitute.insert(param, format!("_{}", dead.unwrap_or_default()));
                written.push((param, dead.unwrap_or_default()));
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
    let mut out = Text { arg_types: outer.arg_types.clone(), ret: outer.ret.clone(), generics: outer.generics.clone(), locals, blocks: Vec::new() };
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
    let (folded, units) = normal_form(&mut out, dest, &fresh.iter().map(|(local, _)| *local).collect::<Vec<_>>());
    reborrows(&mut out, &passed);
    canonical_symmetric(&mut out);
    Ok(Inlined { written, folded, units, text: out, files, table, declarations })
}

// ---------------------------------------------------------------------------------------------
// [R95.6-1] A closure that the body defines and calls.

/// [R95.6-1] The names of the closure bodies that a canonical text makes (`closure NAME::<…>(…)`
/// or `closure NAME(…)`).
pub fn closure_bodies(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((_, rest)) = line.split_once(" = closure ") else { continue };
        let end = rest.find("::<").into_iter().chain(rest.find('(')).min().unwrap_or(rest.len());
        let name = rest[..end].to_string();
        if name.contains("{closure#") && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// The elements of a text that name a local: (block index, element index).
fn mentions_of(text: &Text, local: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (block, (_, list)) in text.blocks.iter().enumerate() {
        for (index, element) in list.iter().enumerate() {
            if numbers(&element.text).iter().any(|(_, _, number)| *number == Num::Local(local)) {
                out.push((block, index));
            }
        }
    }
    out
}

/// The one element that writes a local, with its value text (after ` = `).
fn one_write(text: &Text, local: usize) -> Option<((usize, usize), String)> {
    let head = format!("_{local} = ");
    let writes: Vec<(usize, usize)> = mentions_of(text, local).into_iter().filter(|(block, index)| uses(&text.blocks[*block].1[*index].text).iter().any(|(found, kind)| *found == local && *kind == Use::Write)).collect();
    let [at] = writes[..] else { return None };
    let value = text.blocks[at.0].1[at.1].text.strip_prefix(&head)?.to_string();
    Some((at, value))
}

/// [R95.6-1] True if an element after `at` (in its block, or in a block that its block reaches)
/// writes or borrows the local.
fn written_after(text: &Text, at: (usize, usize), local: usize) -> bool {
    let touched = |element: &Elem| uses(&element.text).iter().any(|(found, kind)| *found == local && *kind != Use::Read);
    let (_, list) = &text.blocks[at.0];
    if list.iter().skip(at.1 + 1).any(touched) {
        return true;
    }
    let successors: Vec<usize> = list.last().map(|last| numbers(&last.text).into_iter().filter_map(|(_, _, number)| match number {
        Num::Block(next) => Some(next),
        Num::Local(_) => None,
    }).collect()).unwrap_or_default();
    let mut reached = BTreeSet::new();
    for next in successors {
        reached.extend(reached_blocks(text, next));
    }
    text.blocks.iter().filter(|(number, _)| reached.contains(number)).any(|(_, list)| list.iter().any(touched))
}

/// The cause of a closure call of an other closure body (not a candidate).
const OTHER_CLOSURE: &str = "a call of an other closure";

/// [R95.6-1] The text of a closure call as a call of a plain function, and the closure body as
/// that function: the captures and the elements of the argument tuple are the parameters, in this
/// order. A capture access of the body (`((*_1).0: &u32)`, or `(_1.0: u32)` for a closure by
/// value) is the parameter of the capture. `Err`: the cause why the check does not put the closure
/// in at its call (the closure leaves the body, a capture by value that the body writes before
/// the call, a use of the closure value in its body).
fn prepare_closure(outer: &Text, at: (usize, usize), inner: &Text, name: &str, path: &str) -> Result<(Text, Text), String> {
    let element = &outer.blocks[at.0].1[at.1];
    let call = parse_call(&element.text, path)?;
    let by_ref = if path.ends_with(">::call") {
        Some("&shared ")
    } else if path.ends_with(">::call_mut") {
        Some("&mut ")
    } else if path.ends_with(">::call_once") {
        None
    } else {
        return Err("a call of a closure through a trait method that is not `call`, `call_mut`, or `call_once`".into());
    };
    let [self_arg, tuple_arg] = &call.args[..] else { return Err("a closure call with no argument tuple".into()) };
    let leaves = || "the closure leaves the body: a use of the closure value that is not its call (R95.6-1)".to_string();
    let self_local = plain_local(self_arg.strip_prefix("move ").unwrap_or(self_arg)).ok_or_else(leaves)?;
    let closure = match by_ref {
        Some(marker) => {
            let (_, value) = one_write(outer, self_local).ok_or_else(leaves)?;
            if mentions_of(outer, self_local).len() != 2 {
                return Err(leaves());
            }
            plain_local(value.strip_prefix(marker).ok_or_else(leaves)?).ok_or_else(leaves)?
        }
        None => self_local,
    };
    let (made, value) = one_write(outer, closure).ok_or_else(leaves)?;
    let rest = value.strip_prefix("closure ").ok_or_else(leaves)?;
    if !rest.starts_with(&format!("{name}::<")) && !rest.starts_with(&format!("{name}(")) {
        return Err(OTHER_CLOSURE.into());
    }
    // Each other mention of the closure value: its borrow for the call (or the call), and drops.
    for (block, index) in mentions_of(outer, closure) {
        let text = &outer.blocks[block].1[index].text;
        let fits = (block, index) == made || text.starts_with(&format!("drop(_{closure})")) || (by_ref.is_some() && text.starts_with(&format!("_{self_local} = "))) || (by_ref.is_none() && (block, index) == at);
        if !fits {
            return Err(leaves());
        }
    }
    let open = rest.rfind('(').ok_or_else(leaves)?;
    let close = matching(rest, open).ok_or_else(leaves)?;
    let captures = split_list(&rest[open + 1..close]);
    // The argument tuple.
    let tuple_local = plain_local(tuple_arg.strip_prefix("move ").unwrap_or(tuple_arg)).ok_or_else(|| "an argument tuple that is not a local".to_string())?;
    let (_, tuple) = one_write(outer, tuple_local).ok_or_else(|| "an argument tuple with more than one write".to_string())?;
    if mentions_of(outer, tuple_local).len() != 2 {
        return Err("an argument tuple with an other use".into());
    }
    let fields = if tuple == "const ()" { Vec::new() } else { split_list(tuple.strip_prefix("tuple(").and_then(|rest| rest.strip_suffix(')')).ok_or_else(|| "an argument tuple that is not a tuple value".to_string())?) };
    if inner.args() != fields.len() + 1 {
        return Err("the count of the elements of the argument tuple is not the count of the parameters of the closure".into());
    }
    // The capture types, from the locals of the caller. A capture by value (no reference) is a
    // copy at the place of the closure: the body must not write it before the call.
    let typed = |local: usize| -> Option<String> { outer.locals.get(&local).cloned().or_else(|| local.checked_sub(1).and_then(|index| outer.arg_types.get(index)).cloned()) };
    let mut capture_types = Vec::new();
    for capture in &captures {
        let local = plain_local(capture.strip_prefix("move ").unwrap_or(capture)).ok_or_else(|| format!("a capture `{capture}` that is not a local"))?;
        let ty = typed(local).ok_or_else(|| format!("a capture `{capture}` with no type"))?;
        if !ty.starts_with('&') && written_after(outer, made, local) {
            return Err(format!("a capture by value of `_{local}` that the body writes or borrows after the closure is made (the closure has the old value)"));
        }
        capture_types.push(ty);
    }
    let (count, params) = (captures.len(), fields.len());
    // The body: each capture access is the parameter of the capture.
    const CAPTURE: usize = 1 << 40;
    let access = if by_ref.is_some() { "((*_1)." } else { "(_1." };
    let mut body = inner.clone();
    body.generics.clear();
    for (_, list) in body.blocks.iter_mut() {
        for element in list.iter_mut() {
            let mut text = element.text.clone();
            while let Some(found) = text.find(access) {
                let close = matching(&text, found).ok_or("a capture access that the check does not read")?;
                let inside = &text[found + access.len()..close];
                let index: usize = inside.split(':').next().and_then(|digits| digits.parse().ok()).ok_or("a capture access that the check does not read")?;
                if index >= count {
                    return Err("a capture access with no capture".into());
                }
                text = format!("{}_{}{}", &text[..found], CAPTURE + index, &text[close + 1..]);
            }
            element.text = renumber(&text, &|local| match local {
                0 => "_0".to_string(),
                1 => "_SELF".to_string(),
                _ if local >= CAPTURE => format!("_{}", local - CAPTURE + 1),
                _ if local <= params + 1 => format!("_{}", count + local - 1),
                _ => format!("_{}", local + count - 1),
            }, &|block| block);
            if element.text.contains("_SELF") {
                return Err("a use of the closure value in its body that is not a capture access".into());
            }
        }
    }
    body.arg_types = capture_types.iter().cloned().chain(inner.arg_types.iter().skip(1).cloned()).collect();
    body.locals = inner.locals.iter().map(|(local, ty)| (if *local == 0 { 0 } else { local + count - 1 }, ty.clone())).filter(|(local, _)| *local == 0 || *local > count + params).collect();
    // The caller: the call of the closure is a call of the body with the captures and the
    // elements of the tuple.
    let mut caller = outer.clone();
    let unwind = match call.unwind {
        None => "unwind continue".to_string(),
        Some(block) => format!("unwind: bb{block}"),
    };
    let operands: Vec<String> = captures.iter().cloned().chain(fields.iter().cloned()).collect();
    caller.blocks[at.0].1[at.1].text = format!("{} = const fn({}) -> {} {{{name}}}({}) -> [return: bb{}, {unwind}]", call.dest, body.arg_types.join(", "), body.ret, operands.join(", "), call.target);
    Ok((caller, body))
}

/// [R95.6-1] The parts of a text that the closure call leaves after the inline: a write of a
/// local that no element reads, with a value with no effect (a borrow, a closure value, a tuple,
/// a copy of a reference) goes; a copy of a reference `_x = no_retag P` forwards into its uses;
/// a deref `(*_r)` of a borrow `_r = &shared P` or `&mut P` with one write, whose other uses are
/// such derefs, is the place `P` (the borrow rules keep `P` from a change while the borrow
/// lives).
fn closure_cleanup(text: &mut Text) {
    let quiet = |value: &str| value.starts_with("&shared ") || value.starts_with("&mut ") || value.starts_with("closure ") || value.starts_with("tuple(") || value.starts_with("no_retag ") || value == "const ()";
    loop {
        let mut changed = false;
        let top = text.top_local();
        for local in 1..=top {
            if local <= text.args() {
                continue;
            }
            let found = mentions_of(text, local);
            let Some((at, value)) = one_write(text, local) else { continue };
            if found.len() == 1 && quiet(&value) {
                text.blocks[at.0].1.remove(at.1);
                changed = true;
                break;
            }
            if let Some(source) = value.strip_prefix("no_retag ").and_then(plain_local)
                && !borrowed(text).contains(&local)
            {
                text.blocks[at.0].1.remove(at.1);
                for (_, list) in text.blocks.iter_mut() {
                    for element in list.iter_mut() {
                        element.text = renumber(&element.text, &|found| if found == local { format!("_{source}") } else { format!("_{found}") }, &|block| block);
                    }
                }
                changed = true;
                break;
            }
            let place = value.strip_prefix("&shared ").or_else(|| value.strip_prefix("&mut ")).filter(|place| place_local(place).is_some() || place.strip_prefix("(*_").and_then(|rest| rest.strip_suffix(')')).is_some_and(|digits| digits.parse::<usize>().is_ok()));
            if let Some(place) = place {
                let deref = format!("(*_{local})");
                let only_derefs = found.iter().filter(|spot| **spot != at).all(|(block, index)| {
                    let element = &text.blocks[*block].1[*index].text;
                    numbers(element).iter().filter(|(_, _, number)| *number == Num::Local(local)).count() == element.matches(&deref).count()
                });
                if found.len() > 1 && only_derefs {
                    let place = place.to_string();
                    text.blocks[at.0].1.remove(at.1);
                    for (_, list) in text.blocks.iter_mut() {
                        for element in list.iter_mut() {
                            element.text = element.text.replace(&deref, &place);
                        }
                    }
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            changed = (text.args() + 1..=text.top_local()).any(|local| forward_copy(text, local));
        }
        if !changed {
            break;
        }
    }
}

/// [R95.6-1] The order of the two operands of each symmetric operation of a text after an inline,
/// by the rule of the printer of the driver (design 47, G1; `semilla-mir`, `print.rs`): for two
/// operands of an integer, `bool`, or `char` type, the operand with the smaller key goes first;
/// the key is the text with a mark in place of each local, then the numbers of the locals in the
/// order of their first use (the blocks in a depth-first walk from the entry, the first successor
/// first; the arguments and the return local keep their numbers). The inline gives the locals of
/// the callee new numbers, so the order of the callee text can differ from the order that the
/// driver gives the same code. A symmetric operation has the same value in each order.
fn canonical_symmetric(text: &mut Text) {
    let Some(entry) = text.blocks.first().map(|(number, _)| *number) else { return };
    let by_number: BTreeMap<usize, usize> = text.blocks.iter().enumerate().map(|(at, (number, _))| (*number, at)).collect();
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    let mut stack = vec![entry];
    while let Some(block) = stack.pop() {
        if !seen.insert(block) {
            continue;
        }
        order.push(block);
        let Some(at) = by_number.get(&block) else { continue };
        let successors: Vec<usize> = text.blocks[*at].1.last().map(|last| numbers(&last.text).into_iter().filter_map(|(_, _, number)| match number {
            Num::Block(next) => Some(next),
            Num::Local(_) => None,
        }).collect()).unwrap_or_default();
        for next in successors.into_iter().rev() {
            if !seen.contains(&next) {
                stack.push(next);
            }
        }
    }
    let mut rank: BTreeMap<usize, usize> = (0..=text.args()).map(|local| (local, local)).collect();
    for block in &order {
        let Some(at) = by_number.get(block) else { continue };
        for element in &text.blocks[*at].1 {
            // A call visits its operands before its destination.
            let (head, tail) = match element.text.split_once(" = const ") {
                Some((head, tail)) if element.text.contains("-> [return: ") => (tail, head),
                _ => (element.text.as_str(), ""),
            };
            for part in [head, tail] {
                for (_, _, number) in numbers(part) {
                    if let Num::Local(local) = number {
                        let next = rank.len();
                        rank.entry(local).or_insert(next);
                    }
                }
            }
        }
    }
    // The integers, `bool`, and `char`: a primitive type that is not `str` and not a float.
    let exact = |ty: &str| PRIMITIVES.contains(&ty) && ty != "str" && !ty.starts_with('f');
    let types: BTreeMap<usize, String> = text.locals.clone().into_iter().chain(text.arg_types.iter().enumerate().map(|(index, ty)| (index + 1, ty.clone()))).collect();
    let operand_type = |operand: &str| -> Option<String> {
        if let Some(local) = plain_local(operand) {
            return types.get(&local).cloned();
        }
        if let Some(rest) = operand.strip_prefix("const ") {
            return rest.split_once(" = ").map(|(ty, _)| ty.to_string());
        }
        place_local(operand)?;
        operand.strip_suffix(')')?.rsplit_once(": ").map(|(_, ty)| ty.to_string())
    };
    let key = |operand: &str| -> (String, Vec<usize>) {
        let mut shape = String::new();
        let mut list = Vec::new();
        let mut at = 0;
        for (start, end, number) in numbers(operand) {
            shape.push_str(&operand[at..start]);
            match number {
                Num::Local(local) => {
                    shape.push_str("_#");
                    list.push(rank.get(&local).copied().unwrap_or(usize::MAX));
                }
                Num::Block(_) => shape.push_str(&operand[start..end]),
            }
            at = end;
        }
        shape.push_str(&operand[at..]);
        (shape, list)
    };
    for (_, list) in text.blocks.iter_mut() {
        for element in list.iter_mut() {
            let mut out = element.text.clone();
            for op in elements::SYMMETRIC {
                let mut from = 0;
                while let Some(found) = out[from..].find(op) {
                    let at = from + found;
                    from = at + op.len();
                    if at > 0 && out[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                        continue;
                    }
                    let open = at + op.len() - 1;
                    let Some(close) = matching(&out, open) else { continue };
                    let parts = split_list(&out[open + 1..close]);
                    let [a, b] = parts.as_slice() else { continue };
                    let both = operand_type(a).zip(operand_type(b)).is_some_and(|(x, y)| exact(&x) && exact(&y));
                    if both && key(a) > key(b) {
                        out = format!("{}{b}, {a}{}", &out[..open + 1], &out[close..]);
                    }
                }
            }
            element.text = out;
        }
    }
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
fn normal_form(text: &mut Text, dest: usize, fresh: &[usize]) -> (usize, usize) {
    let entry = text.blocks.first().map(|(number, _)| *number);
    // [R95.3-1] A block of the callee that no edge reaches (a cleanup block of a generic body
    // with no unwind edge left) never runs.
    if let Some(entry) = entry {
        let reached = reached_blocks(text, entry);
        text.blocks.retain(|(number, _)| reached.contains(number));
    }
    // [R95.5-1] A write of `()` into a local of the callee that no element reads has no effect
    // (the value of an `if` with no `else` before an early `return`).
    let mentions = |text: &Text, local: usize| text.elements().flat_map(|element| numbers(&element.text)).filter(|(_, _, number)| *number == Num::Local(local)).count();
    let units: Vec<usize> = text.elements().filter(|element| element.callee).filter_map(|element| element.text.strip_suffix(" = const ()").and_then(plain_local)).filter(|local| *local != dest && mentions(text, *local) == 1).collect();
    let before = text.elements().count();
    for (_, list) in text.blocks.iter_mut() {
        list.retain(|element| !(element.callee && element.text.strip_suffix(" = const ()").and_then(plain_local).is_some_and(|local| units.contains(&local))));
    }
    let units_removed = before - text.elements().count();
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
    for local in std::iter::once(dest).chain(fresh.iter().copied()) {
        forward_copy(text, local);
    }
    // [R95.5-3] A value that goes to a place through a local: `_t = V` and, as the next element
    // of the block, `P = _t`, with no other mention of `_t`, is `P = V`.
    let mut folded = 0;
    while (text.args() + 1..=text.top_local()).any(|local| fold_into_place(text, local)) {
        folded += 1;
    }
    (folded, units_removed)
}

/// [R95.5-3] The result of a call in a place that is not a local: the old code writes the value
/// into the place (`P = V`), and the code in place writes it into a local that the next element
/// copies into the place (`_t = V; P = _t`). With no other mention of `_t` and no borrow of it,
/// the two elements are `P = V`: the value is evaluated first in each form, and no element is
/// between them. True if the two elements fold.
fn fold_into_place(text: &mut Text, local: usize) -> bool {
    let found = mentions_of(text, local);
    let [(block, first), (other, second)] = found[..] else { return false };
    if block != other || second != first + 1 || borrowed(text).contains(&local) {
        return false;
    }
    let list = &text.blocks[block].1;
    let Some(value) = list[first].text.strip_prefix(&format!("_{local} = ")) else { return false };
    let Some(place) = list[second].text.strip_suffix(&format!(" = _{local}")) else { return false };
    // A value with an edge (a call) is a terminator, and a place that names the local is no fold.
    if value.contains("-> [") || numbers(place).iter().any(|(_, _, number)| *number == Num::Local(local)) || assigned(&list[second].text).is_none() {
        return false;
    }
    let folded = format!("{place} = {value}");
    // The folded element keeps the place of the value (the code of the callee).
    let list = &mut text.blocks[block].1;
    list[first].text = folded;
    list.remove(second);
    true
}

/// Rule 3c of the normal form for one local (see `normal_form`): true if the copy went forward.
fn forward_copy(text: &mut Text, local: usize) -> bool {
    let borrowed = borrowed(text);
    let mentions: Vec<(usize, usize, Use)> = text.blocks.iter().enumerate().flat_map(|(block, (_, list))| list.iter().enumerate().flat_map(move |(index, element)| uses(&element.text).into_iter().filter(move |(found, _)| *found == local).map(move |(_, kind)| (block, index, kind)))).collect();
    let writes: Vec<&(usize, usize, Use)> = mentions.iter().filter(|(_, _, kind)| *kind != Use::Read).collect();
    let [(block, index, Use::Write)] = writes[..] else { return false };
    let (block, index) = (*block, *index);
    let element = &text.blocks[block].1[index].text;
    let Some(value) = element.strip_prefix(&format!("_{local} = ")) else { return false };
    let Some((place, source)) = operand_place(value) else { return false };
    if borrowed.contains(&local) || borrowed.contains(&source) || source == local {
        return false;
    }
    let reads: Vec<&(usize, usize, Use)> = mentions.iter().filter(|(_, _, kind)| *kind == Use::Read).collect();
    if reads.is_empty() || reads.iter().any(|(at_block, at, _)| *at_block != block || *at <= index) {
        return false;
    }
    let last = reads.iter().map(|(_, at, _)| *at).max().unwrap_or(index);
    let list = &text.blocks[block].1;
    // No element between the copy and the last read writes the local of P, or names it in a
    // call or a drop (the last read can be such an element: it reads P first).
    let blocked = (index + 1..last).any(|at| uses(&list[at].text).iter().any(|(found, kind)| *found == source && *kind != Use::Read)) || (index + 1..=last).any(|at| uses(&list[at].text).iter().any(|(found, kind)| *found == source && *kind == Use::Write));
    if blocked {
        return false;
    }
    let place = place.clone();
    let list = &mut text.blocks[block].1;
    for element in list.iter_mut().skip(index + 1).take(last - index) {
        element.text = renumber(&element.text, &|found| if found == local { place.clone() } else { format!("_{found}") }, &|block| block);
    }
    list.remove(index);
    true
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
    check_target(sides, callee, &Target { path: callee.name.to_string(), name: callee.name.to_string(), args: Vec::new(), closure: false }, kind, before, None)
}

/// [R95.3-1] The names of the generic parameters of the signature line of a canonical text.
pub fn generic_names(text: &str) -> Vec<String> {
    let Some(list) = text.lines().next().and_then(|line| line.split_once(" generics <")).and_then(|(_, list)| list.strip_suffix("> {")) else { return Vec::new() };
    split_list(list).into_iter().map(|part| part.rsplit_once('#').map_or(part.clone(), |(name, _)| name.to_string())).collect()
}

/// [R95.3-1] [R95.4-1] [`check`] for a callee whose call text is not its name: a generic callee
/// with the generic arguments of the call, or an impl method that rustc resolves for a trait
/// method on a concrete type.
pub fn check_target(sides: [&Side; 2], callee: &Side, target: &Target, kind: Kind, before: &ElementDiff, facts: Option<&CallFacts>) -> Vec<Transform> {
    let at = if kind == Kind::Extract { 1 } else { 0 };
    let other = 1 - at;
    let refused = |cause: String, call: Option<Place>| Transform { kind, callee: callee.name.to_string(), call, refused: Some(cause), ..Default::default() };
    let (Some(outer), Some(generic), Some(peer)) = (parse_text(sides[at].text, sides[at].record), parse_text(callee.text, callee.record), parse_text(sides[other].text, sides[other].record)) else {
        return vec![refused("a text that the check does not read".into(), None)];
    };
    // [R95.3-1] The quick test reads the text of the instance of the call.
    let inner = if generic.generics.is_empty() || target.args.is_empty() {
        generic.clone()
    } else {
        match substitute(&generic, &target.args) {
            Ok(text) => text,
            Err(cause) => return vec![refused(cause, None)],
        }
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
    let marker = format!(" {{{}}}(", target.path);
    // [R95.6-1] The calls of a closure: a call of `Fn::call`, `FnMut::call_mut`, or
    // `FnOnce::call_once` on a closure type (the closure value names the body).
    let is_call = |text: &str| if target.closure { called_paths(text).iter().any(|path| path.starts_with("<{closure#") && path.contains(" as std::ops::Fn")) } else { text.contains(&marker) };
    let calls: Vec<(usize, usize)> = outer.blocks.iter().enumerate().flat_map(|(block, (_, list))| list.iter().enumerate().filter(|(_, element)| is_call(&element.text)).map(move |(index, _)| (block, index))).collect();
    let mut out = Vec::new();
    let before_places = [places(&before.old), places(&before.new)];
    for (block, index) in calls.into_iter().take(MOST_CANDIDATES) {
        let element = &outer.blocks[block].1[index];
        let call_place = element.tokens.clone();
        // Only a call that is new code (an element that differs) is a candidate.
        if !before_places[at].contains(&(element.tokens.clone(), element.call.clone())) {
            continue;
        }
        // [R95.4-1] A call of a trait method (`<A as B>::m`) goes to the body of the impl method
        // only if the driver resolves the instance of this call to that body (a concrete self
        // type: no trait object, no type parameter of the caller).
        if target.path.starts_with('<') {
            let resolved: Vec<&CallFact> = element.tokens.as_ref().and_then(|place| facts?.get(place)).map(|list| list.iter().collect()).unwrap_or_default();
            if resolved.is_empty() || resolved.iter().any(|fact| fact.resolved.as_deref() != Some(target.name.as_str())) {
                out.push(refused(format!("the records do not resolve the call of the trait method `{}` to the impl method `{}`", target.path, target.name), call_place));
                continue;
            }
        }
        // [R95.6-1] A closure call is a call of the closure body with the captures and the
        // elements of the argument tuple; the parts that the closure call leaves go after it.
        let prepared = if target.closure {
            let path = called_paths(&element.text).into_iter().find(|path| path.starts_with("<{closure#")).unwrap_or_default().to_string();
            match prepare_closure(&outer, (block, index), &generic, &target.name, &path) {
                Ok(pair) => Some(pair),
                Err(cause) if cause == OTHER_CLOSURE => continue,
                Err(cause) => {
                    out.push(refused(cause, call_place));
                    continue;
                }
            }
        } else {
            None
        };
        let closure_target = Target { path: target.name.clone(), name: target.name.clone(), args: Vec::new(), closure: false };
        let result = match &prepared {
            Some((caller, body)) => inline((caller, sides[at].record), (block, index), (body, callee.record), &closure_target, sides[at].name).map(|mut inlined| {
                closure_cleanup(&mut inlined.text);
                canonical_symmetric(&mut inlined.text);
                inlined
            }),
            None => inline((&outer, sides[at].record), (block, index), (&generic, callee.record), target, sides[at].name),
        };
        let inlined = match result {
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
            via: via(target, &generic, prepared.as_ref().map(|(_, body)| body), &inlined.written, inlined.folded, inlined.units),
        });
    }
    out
}

/// [R95.3-1] [R95.4-1] [R95.5-1] [R95.5-2] [R95.6-1] The rules of design 95 that a check used,
/// the most specific first, each with a short text.
fn via(target: &Target, callee: &Text, closure: Option<&Text>, written: &[(usize, usize)], folded: usize, units: usize) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(body) = closure {
        let by_ref = body.arg_types.iter().take(body.args().saturating_sub(callee.args().saturating_sub(1))).filter(|ty| ty.starts_with('&')).count();
        let captures = body.args().saturating_sub(callee.args().saturating_sub(1));
        out.push(("R95.6-1".to_string(), format!("the closure with {captures} capture(s), {by_ref} by reference, called in the body")));
    }
    if target.path.starts_with('<') && !target.closure {
        out.push(("R95.4-1".to_string(), format!("the impl method that rustc resolves for the call of `{}`", target.path)));
    }
    if !target.args.is_empty() {
        let names = callee.generics.iter().map(|(name, _)| name.as_str());
        let pairs: Vec<String> = names.zip(&target.args).map(|(name, arg)| format!("`{name}` = `{arg}`")).collect();
        out.push(("R95.3-1".to_string(), format!("the generic arguments of the call: {}", pairs.join(", "))));
    }
    for (param, local) in written {
        out.push(("R95.5-2".to_string(), format!("the callee writes its parameter {param}, and the caller does not read `_{local}` after the call (no copy)")));
    }
    // An early `return`: the edges into the blocks with only `return` are the ways out of the
    // callee, and the value of the `if` before the `return` is a write of `()` with no reader.
    let counts = references(callee);
    let ways: usize = callee.blocks.iter().filter(|(_, list)| list.len() == 1 && list[0].text == "return").map(|(number, _)| counts.get(number).copied().unwrap_or(0)).sum();
    if units > 0 {
        out.push(("R95.5-1".to_string(), format!("an early `return`: {} way(s) out of the callee, {units} write(s) of `()` with no reader", ways.max(1))));
    }
    if folded > 0 {
        out.push(("R95.5-3".to_string(), format!("{folded} value(s) go to their place with no local")));
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
    call: Option<Place>,
    order: OrderKey,
    text: String,
    /// [R95.2-1] The facts of the dependence rules: an effect (a call with no summary or with an
    /// effect, a drop, a write through a reference), a panic (a call that can panic, an
    /// `assert`, a drop), a read or a write through a reference, and a call whose summary has
    /// no effect.
    effect: bool,
    panics: bool,
    memory: bool,
    pure_call: bool,
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
fn items(text: &Text, facts: Option<&CallFacts>) -> Vec<Item> {
    let text_of = text;
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
            let (effect, panics, memory, pure_call) = effect_facts(text_of, &element.text, facts, element.tokens.as_ref());
            out.push(Item { key, numbers: found, chain, position: first + index, place: element.tokens.clone(), call: element.call.clone(), order, text: element.text.clone(), swaps, effect, panics, memory, pure_call });
        }
    }
    out
}

/// [R94.4-3] [R95.2-1] The dependence facts of one element: `(effect, panics, memory, pure
/// call)`. A call whose effect summary (R64.3-1) is complete and has no effect on the world is
/// no effect part, unless an argument is a `&mut` reference or a raw pointer (the call writes
/// its target); it reads through each reference argument; it can panic if its summary says so.
/// A call with no summary in the records is an effect that can panic, as before.
fn effect_facts(text: &Text, element: &str, facts: Option<&CallFacts>, place: Option<&Place>) -> (bool, bool, bool, bool) {
    let memory = reads_memory(element) || writes_memory(element);
    match class(element) {
        Class::Call => {
            let typed = |local: usize| -> Option<&String> { text.locals.get(&local).or_else(|| local.checked_sub(1).and_then(|at| text.arg_types.get(at))) };
            let args: Vec<usize> = element.find("}(").and_then(|at| matching(element, at + 1).map(|close| &element[at + 2..close])).map(|list| split_list(list).iter().filter_map(|arg| plain_local(arg.strip_prefix("move ").unwrap_or(arg))).collect()).unwrap_or_default();
            let types: Vec<&String> = args.iter().filter_map(|local| typed(*local)).collect();
            let writes = types.iter().any(|ty| ty.starts_with("&mut ") || ty.starts_with("*mut ") || ty.starts_with("*const "));
            let reads = types.iter().any(|ty| ty.starts_with('&'));
            match summary_at(facts, place) {
                Some((true, panics)) if !writes && types.len() == args.len() => (false, panics, memory || reads, true),
                _ => (true, true, memory, false),
            }
        }
        Class::Drop => (true, true, memory, false),
        Class::Assert => (writes_memory(element), true, memory, false),
        Class::Jump => (false, false, memory, false),
        Class::Statement => (writes_memory(element), false, memory, false),
    }
}

/// [R94.4-3] The cause of a dependence of two elements, or `None` if they are independent.
fn depends(a: &Item, b: &Item) -> Option<String> {
    let (x, y) = (class(&a.text), class(&b.text));
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
    let ((reads_a, writes_a), (reads_b, writes_b)) = (sets(&a.text), sets(&b.text));
    if writes_a.iter().any(|local| reads_b.contains(local) || writes_b.contains(local)) || writes_b.iter().any(|local| reads_a.contains(local)) {
        return Some("a place that one part writes and the other part reads or writes".into());
    }
    if a.effect && b.effect {
        return Some("two parts with an effect each (a call, a drop, or a write through a reference)".into());
    }
    if a.panics && b.panics {
        return Some("two parts that each can panic".into());
    }
    if (a.panics && (b.effect || b.memory)) || (b.panics && (a.effect || a.memory)) {
        return Some("a part that can panic and a part with an effect or a read through a reference".into());
    }
    if (a.effect && b.memory) || (b.effect && a.memory) {
        return Some("a part with an effect and a read through a reference".into());
    }
    None
}

/// [R94.4-2] [R94.4-3] The new order of the same parts of a body pair: `None` if the two texts
/// do not have the same elements under one map of the locals and of the chains of blocks (then
/// the change is not only a new order). The pairs of elements whose order changes are checked
/// for a dependence (R94.4-3).
pub fn permutation(old: (&str, &RecordElements), new: (&str, &RecordElements)) -> Option<Reorder> {
    permutation_with(old, new, [None, None], None)
}

/// [R95.1-1] [R95.2-1] [`permutation`] that reads the region, not the whole body: with the element
/// diff of design 90 (`diff`), an element that differs and has no partner of its key is a part
/// of a real edit (the residue, `Reorder::residue`), and the rest of the body keeps the map. The
/// check reads the moved parts, the parts between their two places (also a part of a real edit
/// between them), and the dependences of these parts. `facts` gives the effect summary of each
/// call of the two sides (R95.2-1). With no diff, the whole body must be a new order.
pub fn permutation_with(old: (&str, &RecordElements), new: (&str, &RecordElements), facts: [Option<&CallFacts>; 2], diff: Option<&ElementDiff>) -> Option<Reorder> {
    // A change that is only a new order keeps the check of the whole body (design 94).
    if diff.is_some()
        && let Some(found) = permutation_with(old, new, facts, None)
    {
        return Some(found);
    }
    let (a, b) = (parse_text(old.0, old.1)?, parse_text(new.0, new.1)?);
    if a.arg_types != b.arg_types || a.ret != b.ret || a.generics != b.generics {
        return None;
    }
    let (left, right) = (items(&a, facts[0]), items(&b, facts[1]));
    if left.len() > MOST_ELEMENTS || right.len() > MOST_ELEMENTS {
        return None;
    }
    if diff.is_none() {
        if left.len() != right.len() {
            return None;
        }
        let mut sorted = [left.iter().map(|item| item.key.as_str()).collect::<Vec<_>>(), right.iter().map(|item| item.key.as_str()).collect::<Vec<_>>()];
        sorted[0].sort_unstable();
        sorted[1].sort_unstable();
        if sorted[0] != sorted[1] {
            return None;
        }
    }
    // [R95.1-1] The elements that differ in the map of design 90, on each side.
    let differ: [BTreeSet<(Option<Place>, Option<Place>)>; 2] = match diff {
        Some(diff) => [places(&diff.old), places(&diff.new)],
        None => [BTreeSet::new(), BTreeSet::new()],
    };
    let differs = |side: usize, item: &Item| diff.is_some() && differ[side].contains(&(item.place.clone(), item.call.clone()));
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
    let mut moved: BTreeSet<usize> = BTreeSet::new();
    let open: Vec<usize> = order_a.iter().copied().filter(|at| partner[*at].is_none()).collect();
    for at in open {
        // An element that differs pairs with an element of its key that differs (a moved part).
        let found = order_b.iter().copied().find(|other| !taken[*other] && right[*other].key == left[at].key && (diff.is_none() || differs(1, &right[*other])));
        match found {
            Some(found) if diff.is_none() || differs(0, &left[at]) => {
                partner[at] = Some(found);
                taken[found] = true;
                moved.insert(at);
            }
            _ if diff.is_none() => return None,
            _ => {}
        }
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
    // Before them, the pairs that the map of design 90 keeps come first (their locals agree with
    // its map), then the pairs of the elements that differ. [R95.1-1] A pair of elements that differ whose
    // locals do not agree with the map is a part of a real edit (with a diff), else no new order.
    let mut pairs: Vec<(usize, usize)> = partner.iter().enumerate().filter_map(|(x, y)| Some((x, (*y)?))).collect();
    pairs.sort_by_key(|(x, y)| (!left[*x].swaps.is_empty(), differs(0, &left[*x]) || differs(1, &right[*y])));
    for (x, y) in pairs {
        let (p, q) = (&left[x], &right[y]);
        let attempt = |numbers: &[Num], maps: &mut [BTreeMap<usize, usize>; 4]| -> bool {
            let [locals, back, chains, chains_back] = maps;
            bind(chains, chains_back, p.chain, q.chain)
                && p.numbers.iter().zip(numbers).all(|(m, n)| match (m, n) {
                    (Num::Local(m), Num::Local(n)) => typed(&a, *m) == typed(&b, *n) && bind(locals, back, *m, *n),
                    (Num::Block(m), Num::Block(n)) => bind(chains, chains_back, *m, *n),
                    _ => false,
                })
        };
        let mut maps = [locals.clone(), back.clone(), chains.clone(), chains_back.clone()];
        let mut fits = p.numbers.len() == q.numbers.len() && attempt(&q.numbers, &mut maps);
        if !fits && p.numbers.len() == q.numbers.len() && !q.swaps.is_empty() {
            let mut swapped = q.numbers.clone();
            for (i, j) in &q.swaps {
                swapped.swap(*i, *j);
            }
            maps = [locals.clone(), back.clone(), chains.clone(), chains_back.clone()];
            fits = attempt(&swapped, &mut maps);
        }
        if !fits {
            if diff.is_none() {
                return None;
            }
            partner[x] = None;
            taken[y] = false;
            moved.remove(&x);
            continue;
        }
        [locals, back, chains, chains_back] = maps;
    }
    // [R95.1-1] The residue: each element with no partner. Each one must be an element that
    // differs in the map of design 90 (else the two alignments do not agree: no new order).
    let residue: [Vec<usize>; 2] = [(0..left.len()).filter(|at| partner[*at].is_none()).collect(), (0..right.len()).filter(|at| !taken[*at]).collect()];
    if residue[0].iter().any(|at| !differs(0, &left[*at])) || residue[1].iter().any(|at| !differs(1, &right[*at])) {
        return None;
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
    let mut flipped: BTreeSet<usize> = BTreeSet::new();
    let note = |out: &mut Reorder, place: &Option<Place>| {
        if let Some(place) = place
            && !out.moved.contains(place)
        {
            out.moved.push(place.clone());
        }
    };
    for list in by_chain.values() {
        for (i, x) in list.iter().enumerate() {
            for y in &list[i + 1..] {
                if !all && !moved.contains(x) && !moved.contains(y) {
                    continue;
                }
                let (Some(x_new), Some(y_new)) = (partner[*x], partner[*y]) else { continue };
                if (left[*x].position < left[*y].position) == (right[x_new].position < right[y_new].position) {
                    continue;
                }
                out.flips += 1;
                flipped.insert(*x);
                flipped.insert(*y);
                note(&mut out, &left[*x].place);
                note(&mut out, &left[*y].place);
                if let Some(cause) = depends(&left[*x], &left[*y])
                    && out.dependent.len() < 4
                {
                    out.dependent.push((left[*x].place.clone(), left[*y].place.clone(), cause));
                }
            }
        }
    }
    // [R95.1-1] A part that passes the code of a real edit: the residue elements of a chain on
    // the two sides (old 0, new 1) are the edit. A part with a partner that is before each edit
    // element on the two sides, or after each one on the two sides, keeps its order to the edit;
    // each other part can pass it (a doubt counts as a pass), and it must be independent of each
    // edit element of the two chains. An edit that only removes code (no new residue element in
    // the chain) or only adds code (no old one) leaves the order of the other parts to their
    // pairs: the removal or the addition is the real edit.
    let mut residue_of: [BTreeMap<usize, Vec<usize>>; 2] = [BTreeMap::new(), BTreeMap::new()];
    for (side, own) in [(0usize, &left), (1usize, &right)] {
        for at in &residue[side] {
            residue_of[side].entry(own[*at].chain).or_default().push(*at);
        }
    }
    // A part with a partner at the place of a residue element is a part of the edit (an element
    // of a changed statement whose key did not change): its order to the moved parts is checked
    // by its pairs.
    let edit_places: [BTreeSet<&Place>; 2] = [residue[0].iter().filter_map(|at| left[*at].place.as_ref()).collect(), residue[1].iter().filter_map(|at| right[*at].place.as_ref()).collect()];
    let mut budget = MOST_PAIRS;
    for x in 0..left.len() {
        let Some(x_new) = partner[x] else { continue };
        if left[x].place.as_ref().is_some_and(|place| edit_places[0].contains(place)) || right[x_new].place.as_ref().is_some_and(|place| edit_places[1].contains(place)) {
            continue;
        }
        let (Some(old_edit), Some(new_edit)) = (residue_of[0].get(&left[x].chain), residue_of[1].get(&right[x_new].chain)) else { continue };
        let before_old = old_edit.iter().all(|r| left[x].position < left[*r].position);
        let after_old = old_edit.iter().all(|r| left[x].position > left[*r].position);
        let before_new = new_edit.iter().all(|r| right[x_new].position < right[*r].position);
        let after_new = new_edit.iter().all(|r| right[x_new].position > right[*r].position);
        if (before_old && before_new) || (after_old && after_new) {
            continue;
        }
        out.flips += 1;
        flipped.insert(x);
        note(&mut out, &left[x].place);
        budget = budget.saturating_sub(old_edit.len() + new_edit.len());
        if budget == 0 {
            out.dependent.push((left[x].place.clone(), None, "a part of an other edit, and the check reads no more pairs (the limit of the order check)".into()));
            break;
        }
        // The places of `dependent` are places of the old side.
        let causes: Vec<(Option<Place>, String)> = old_edit.iter().filter_map(|r| depends(&left[x], &left[*r]).map(|cause| (left[*r].place.clone(), cause))).chain(new_edit.iter().filter_map(|r| depends(&right[x_new], &right[*r]).map(|cause| (None, cause)))).collect();
        for (place, cause) in causes {
            if out.dependent.len() < 4 {
                out.dependent.push((left[x].place.clone(), place, format!("{cause}, with a part of an other edit")));
            }
        }
    }
    out.pure_calls = flipped.iter().filter(|at| left[**at].pure_call).count();
    out.residue = [0, 1].map(|side| {
        let own: &[Item] = if side == 0 { &left } else { &right };
        let mut list: Vec<Place> = residue[side].iter().filter_map(|at| own[*at].place.clone()).collect();
        list.sort();
        list.dedup();
        list
    });
    Some(out)
}

#[cfg(test)]
mod tests;
