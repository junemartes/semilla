//! The element compare of a changed body (design 40.3).
//!
//! An element is a statement or a terminator of the canonical MIR text: one line in a block.
//! The driver records the source range of each element (`el` in the record). For a body that
//! is different, this module aligns the elements of the two sides and gives the elements that
//! differ, each with its source range.
//!
//! The alignment does not depend on the numbers of the locals and of the blocks:
//! 1. The key of an element is its text with each local number and each block number removed
//!    (`_3 = Add(_1, _2)` gives `_ = Add(_, _)`). The two locals of a symmetric operation go
//!    in the order of their numbers first: the printer sorts them as text (`_100` before `_99`).
//! 2. A copy into a temporary (`_t = _p`) that the next element of the same block uses one time,
//!    with no other use, folds into that element: the use reads `_p`. (One side can have the
//!    copy and the other not: the normal form forwards a copy only with no borrow of `_p`.)
//! 3. The elements go in the order of the source (the start of the place, the longer place
//!    first; an element with no place takes the place of an element of its own block), not in the
//!    order of the blocks. So a swap of two branch targets, which changes
//!    the order of the blocks, does not change the order of the alignment.
//! 4. The longest common subsequence of the keys aligns the elements, with a weight: a pair of
//!    places with the same length and the same offset from the start or from the end of the
//!    body counts two times.
//! 5. The pairs vote for the pairs of their locals; the pairs with the most votes give a
//!    one-to-one map of the locals with equal types. The return local and the arguments map to
//!    themselves.
//! 6. A second alignment uses the keys with the locals of the map (a local with no partner is
//!    `?`), and its pairs vote for the locals with no partner. A pair whose locals do not agree
//!    with the map differs.
//! 7. A block starts with its first element. Each target of a terminator pair must go to two
//!    blocks whose first elements are a pair, or one of whose first elements differs. A pair
//!    that breaks this rule differs. The check runs again until no pair changes.
//! 8. Each element with no partner differs. A folded copy differs if its use differs.
//!
//! A part of the text that is not an element (the signature line, the lines after the body)
//! that differs sets `other`. If all elements align, each pair keeps the rules, and the other
//! parts are equal, the two texts are equal after a renumber of the locals and the blocks and
//! after the forward of the folded copies (a copy next to its only use keeps the behavior): the
//! blocks are the runs of elements that end with a terminator, so the same order of the
//! elements gives the same blocks.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// A source range: the file (relative to the workspace root), and the start and end byte
/// offsets in the file as written.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Place {
    pub file: String,
    pub start: usize,
    pub end: usize,
}

/// One element that differs.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Element {
    /// The range of the tokens of the element in the source. `None`: the element has no
    /// source range (code of the standard library, of a different crate, or of the compiler).
    pub tokens: Option<Place>,
    /// The range of the outermost macro call, for an element from a macro expansion.
    pub call: Option<Place>,
    /// For a `switchInt` terminator: the count of its targets.
    pub targets: Option<usize>,
    /// True for an exit: `return`, or a call that does not return (a panic).
    pub exit: bool,
    /// True for a terminator with no value of the source: `goto`, `return`, `resume`,
    /// `unreachable`, and a `drop` (design 40.3). Such an element at the end of a block is the
    /// glue of the blocks.
    #[serde(default)]
    pub glue: bool,
    /// True for a jump with no value: `goto`, `return`, `resume`, `unreachable` (fix round 2).
    #[serde(default)]
    pub jump: bool,
    /// The symbols that the element names (design 44), with the place of each definition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<Symbol>,
    /// The sources of the difference of the element (design 45): one group for each changed
    /// local and each changed cleanup block of the same body that the difference needs. Empty
    /// if no exact test passed. The graph goes from an element back to its sources; a later
    /// batch can follow it forward for the effects of an edit in its body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Group>,
}

/// A group of sources (design 45): a changed local or a changed cleanup block. An element has a
/// cause only if each group that does not follow it has one; with no such group, one group
/// that follows it needs a cause.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Group {
    /// True for a local whose type follows the element: the local that it writes, and a local
    /// that it reads when the local gets its type by inference (a temporary, a user variable with
    /// no type annotation).
    pub follows: bool,
    pub sources: Vec<Source>,
}

/// A source of the difference of an element (design 45). The side is 0 (old) or 1 (new).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Source {
    /// A declaration range of a changed local: the edits at this place cause the change.
    Declaration(usize, Place),
    /// A changed element of the same body, by its place (its tokens and its macro call): the
    /// causes of the element cause the change.
    Element(usize, Option<Place>, Option<Place>),
}

/// The kinds of a local in the record of the driver (design 45, `spans.rs` of the driver).
pub const LOCAL_USER: i64 = 2;
pub const LOCAL_TEMPORARY: i64 = 3;

/// A symbol that an element or a definition names (design 44): the def path, and the place of
/// the definition in a file of the workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Symbol {
    pub path: String,
    pub place: Option<Place>,
}

/// A symbol of a record: the def path, the file (an index into the file list), the start, and
/// the end.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordSymbol(pub String, pub i64, pub i64, pub i64);

/// The symbol of a record entry, with its place. `None` for an entry that is not valid.
pub fn record_symbol(files: &[String], symbol: &RecordSymbol) -> Option<Symbol> {
    let RecordSymbol(path, file, start, end) = symbol;
    let place = match usize::try_from(*file) {
        Ok(file) => Some(Place { file: files.get(file)?.clone(), start: usize::try_from(*start).ok()?, end: usize::try_from(*end).ok()? }),
        Err(_) => None,
    };
    Some(Symbol { path: path.clone(), place })
}

/// The definitions of a crate and the symbols that each one names (design 44): the record of
/// the driver.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordDefs {
    pub f: Vec<String>,
    pub s: Vec<RecordSymbol>,
    /// Each definition: the index of its symbol, then pairs of a named symbol and its flags.
    pub d: Vec<Vec<u32>>,
}

impl RecordDefs {
    /// Each definition with the symbols that it names, and the flags of each one.
    pub fn entries(&self) -> Vec<(Symbol, Vec<(Symbol, u8)>)> {
        let symbol = |index: &u32| self.s.get(*index as usize).and_then(|found| record_symbol(&self.f, found));
        self.d
            .iter()
            .filter_map(|entry| {
                let own = symbol(entry.first()?)?;
                let named = entry[1..].chunks(2).filter_map(|pair| Some((symbol(pair.first()?)?, u8::try_from(*pair.get(1)?).ok()?))).collect();
                Some((own, named))
            })
            .collect()
    }
}

/// The elements that differ in a changed body, on each side.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementDiff {
    /// The elements of the old body with no equal partner.
    pub old: Vec<Element>,
    /// The elements of the new body with no equal partner.
    pub new: Vec<Element>,
    /// True if a part of the text that is not an element differs: the signature line, or a
    /// line after the body.
    pub other: bool,
    /// The token ranges of the aligned element pairs (old, new), for the MIR check of the
    /// statement map (design 41.8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<(Place, Place)>,
    /// True if a record of the body has no symbol data (design 44): a driver before 8f.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_symbols: bool,
    /// The symbol of the function of the body on each side (design 44): the signature line
    /// (`other`) names it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub own: Vec<(usize, Symbol)>,
    /// True for a body that rustc evaluates at compile time (design 44): the body of a
    /// `const fn` that it names counts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub evaluated: bool,
}

impl ElementDiff {
    /// Adds the elements of a different diff (an other target or configuration).
    pub fn merge(&mut self, other: ElementDiff) {
        self.old.extend(other.old);
        self.new.extend(other.new);
        unique(&mut self.old);
        unique(&mut self.new);
        self.other |= other.other;
        self.no_symbols |= other.no_symbols;
        self.evaluated |= other.evaluated;
        for own in other.own {
            if !self.own.contains(&own) {
                self.own.push(own);
            }
        }
        self.pairs.extend(other.pairs);
        self.pairs.sort();
        self.pairs.dedup();
    }
}

/// Sorts a list of elements and removes the copies. Two elements that differ only in their
/// sources (design 45: the locals of two configurations) are one element with each group.
fn unique(list: &mut Vec<Element>) {
    list.sort();
    let mut out: Vec<Element> = Vec::with_capacity(list.len());
    let same = |a: &Element, b: &Element| a.tokens == b.tokens && a.call == b.call && a.targets == b.targets && a.exit == b.exit && a.glue == b.glue && a.jump == b.jump && a.symbols == b.symbols;
    for element in list.drain(..) {
        match out.last_mut() {
            // An element with sources and the same element with none (a configuration where no
            // exact test passed) stay two elements: the second one keeps no cause.
            Some(last) if same(last, &element) && last.sources.is_empty() == element.sources.is_empty() => {
                last.sources.extend(element.sources);
                last.sources.sort();
                last.sources.dedup();
            }
            _ => out.push(element),
        }
    }
    *list = out;
}

/// The element ranges of a record, as the driver writes them (see the driver, `spans.rs`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordElements {
    /// The count of the elements.
    pub n: usize,
    /// The files of the ranges.
    pub f: Vec<String>,
    /// The runs of elements with the same ranges: `[first, count, file, start, end]`, and
    /// `[call file, call start, call end]` for a span from a macro call. `file` is `-1` if the
    /// tokens have no source range.
    pub r: Vec<Vec<i64>>,
    /// The symbol table of the body (design 44).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub s: Vec<RecordSymbol>,
    /// The symbols of the elements: runs `[first, count, symbol…]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub y: Vec<Vec<u32>>,
    /// The version of the symbol data: 1 since batch 8f, 0 for a record with no symbol data.
    #[serde(default)]
    pub v: u8,
    /// The index of the symbol of the function of the body in `s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub o: Option<u32>,
    /// True for a body that rustc evaluates at compile time (a constant, a static).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub c: bool,
    /// The declaration of each local (design 45): `[kind, file, start, end]`, then `[file,
    /// start, end]` of a type annotation. Empty for a record before 8g.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub l: Vec<Vec<i64>>,
}

impl RecordElements {
    /// The symbols of each element (design 44). `None` if the data is not valid.
    pub fn symbols(&self) -> Option<Vec<Vec<Symbol>>> {
        let table: Vec<Option<Symbol>> = self.s.iter().map(|symbol| record_symbol(&self.f, symbol)).collect();
        let mut out = vec![Vec::new(); self.n];
        for run in &self.y {
            let (first, count) = (*run.first()? as usize, *run.get(1)? as usize);
            let list: Vec<Symbol> = run[2..].iter().filter_map(|index| table.get(*index as usize).cloned().flatten()).collect();
            for index in first..first.checked_add(count)? {
                *out.get_mut(index)? = list.clone();
            }
        }
        Some(out)
    }

    /// The kind and the declaration ranges of each local (design 45). An entry that is not
    /// valid has the kind `-1` and no range.
    pub fn declarations(&self) -> Vec<(i64, Vec<Place>)> {
        let place = |file: &i64, start: &i64, end: &i64| -> Option<Place> {
            let file = self.f.get(usize::try_from(*file).ok()?)?.clone();
            let (start, end) = (usize::try_from(*start).ok()?, usize::try_from(*end).ok()?);
            (start <= end).then_some(Place { file, start, end })
        };
        self.l
            .iter()
            .map(|entry| {
                let kind = entry.first().copied().unwrap_or(-1);
                let places = entry.get(1..).unwrap_or_default().chunks(3).filter_map(|part| match part {
                    [file, start, end] => place(file, start, end),
                    _ => None,
                });
                (kind, places.collect())
            })
            .collect()
    }

    /// The ranges of each element: the tokens and the outermost macro call. `None` if the
    /// data is not valid.
    pub fn ranges(&self) -> Option<Vec<(Option<Place>, Option<Place>)>> {
        let mut out = vec![(None, None); self.n];
        let place = |file: i64, start: i64, end: i64| -> Option<Option<Place>> {
            if file < 0 {
                return Some(None);
            }
            let file = self.f.get(usize::try_from(file).ok()?)?.clone();
            let (start, end) = (usize::try_from(start).ok()?, usize::try_from(end).ok()?);
            (start <= end).then_some(Some(Place { file, start, end }))
        };
        for run in &self.r {
            let (first, count) = (usize::try_from(*run.first()?).ok()?, usize::try_from(*run.get(1)?).ok()?);
            let tokens = place(*run.get(2)?, *run.get(3)?, *run.get(4)?)?;
            let call = match run.get(5..8) {
                Some([file, start, end]) => place(*file, *start, *end)?,
                _ => None,
            };
            for index in first..first.checked_add(count)? {
                *out.get_mut(index)? = (tokens.clone(), call.clone());
            }
        }
        Some(out)
    }
}

/// Writes shared element ranges (for `BodyRecord::elements`).
pub fn serialize_shared<S: serde::Serializer>(value: &Option<std::sync::Arc<RecordElements>>, serializer: S) -> Result<S::Ok, S::Error> {
    value.as_deref().serialize(serializer)
}

/// Reads shared element ranges (for `BodyRecord::elements`).
pub fn deserialize_shared<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<std::sync::Arc<RecordElements>>, D::Error> {
    Ok(Option::<RecordElements>::deserialize(deserializer)?.map(std::sync::Arc::new))
}

/// The parts of a canonical text.
struct Parsed<'a> {
    /// The lines that are not an element or a local declaration.
    other: Vec<&'a str>,
    /// The type of each declared local.
    locals: BTreeMap<usize, &'a str>,
    /// The count of the arguments.
    args: usize,
    /// Each element: its text and the index of its block.
    elements: Vec<(&'a str, usize)>,
}

fn parse(text: &str) -> Parsed<'_> {
    let mut parsed = Parsed { other: Vec::new(), locals: BTreeMap::new(), args: 0, elements: Vec::new() };
    let mut block = 0;
    for line in text.lines() {
        if let Some(element) = line.strip_prefix("        ") {
            parsed.elements.push((element.strip_suffix(';').unwrap_or(element), block));
        } else if let Some(rest) = line.strip_prefix("    let _") {
            let (number, ty) = rest.split_once(": ").unwrap_or((rest, ""));
            let ty = ty.strip_suffix(';').unwrap_or(ty);
            match number.parse() {
                Ok(number) => {
                    parsed.locals.insert(number, ty);
                }
                Err(_) => parsed.other.push(line),
            }
        } else if let Some(rest) = line.strip_prefix("    bb") {
            block = rest.trim_end_matches(": {").parse().unwrap_or(block);
        } else if line != "    }" {
            if line.starts_with("fn _(") {
                parsed.args = arg_count(line);
                // The type of each argument: the text up to the next argument.
                for arg in 1..=parsed.args {
                    let Some(from) = line.find(&format!("_{arg}: ")).map(|at| at + format!("_{arg}: ").len()) else { break };
                    let next = if arg < parsed.args { format!(", _{}: ", arg + 1) } else { ") -> ".to_string() };
                    let to = line[from..].find(&next).map_or(line.len(), |at| from + at);
                    parsed.locals.insert(arg, &line[from..to]);
                }
            }
            parsed.other.push(line);
        }
    }
    parsed
}

/// The count of the arguments of the signature line `fn _(_1: T, _2: U) -> R {`.
fn arg_count(line: &str) -> usize {
    let mut count = 0;
    while line.contains(&format!("_{}: ", count + 1)) {
        count += 1;
    }
    count
}

/// One number in an element text: a local (`_3`) or a block (`bb4`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Number {
    Local(usize),
    Block(usize),
}

/// The key of an element (its text without the numbers of the locals and the blocks), and the
/// numbers in order.
fn key(text: &str) -> (String, Vec<Number>) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut numbers = Vec::new();
    let mut at = 0;
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    while at < bytes.len() {
        let before_ok = at == 0 || !word(bytes[at - 1]);
        let prefix = if before_ok && bytes[at] == b'_' {
            Some(1)
        } else if before_ok && text[at..].starts_with("bb") {
            Some(2)
        } else {
            None
        };
        if let Some(prefix) = prefix {
            let digits = bytes[at + prefix..].iter().take_while(|byte| byte.is_ascii_digit()).count();
            let end = at + prefix + digits;
            if digits > 0 && (end == bytes.len() || !word(bytes[end])) {
                let value = text[at + prefix..end].parse().unwrap_or(usize::MAX);
                numbers.push(if prefix == 1 { Number::Local(value) } else { Number::Block(value) });
                out.push_str(&text[at..at + prefix]);
                at = end;
                continue;
            }
        }
        let next = text[at..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[at..at + next]);
        at += next;
    }
    (out, numbers)
}

/// The operations whose operands the printer sorts by their text (see the driver, `print.rs`,
/// `binary`): the order of the operands has no effect.
const SYMMETRIC: &[&str] = &["Add(", "Mul(", "BitAnd(", "BitOr(", "BitXor(", "Eq(", "Ne(", "AddUnchecked(", "MulUnchecked(", "CheckedAdd(", "CheckedMul("];

/// The text of an element with the two locals of each symmetric operation in the order of their
/// numbers. The printer sorts them by their text, and `_100` comes before `_99`: a new local
/// before a statement changed the order of the operands of an unchanged statement (fix
/// round 2). The numbers follow the order of the first use, as on the other side.
fn symmetric_order(text: &str) -> std::borrow::Cow<'_, str> {
    if !SYMMETRIC.iter().any(|op| text.contains(op)) {
        return std::borrow::Cow::Borrowed(text);
    }
    let local = |part: &str| -> Option<usize> { part.strip_prefix('_')?.parse().ok() };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, op)) = SYMMETRIC.iter().filter_map(|op| rest.find(op).map(|at| (at, *op))).min() {
        let before_ok = at == 0 || !rest[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        let open = at + op.len();
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        if !before_ok {
            continue;
        }
        let Some(close) = rest.find(')') else { continue };
        let inner = &rest[..close];
        if let Some((a, b)) = inner.split_once(", ")
            && let (Some(x), Some(y)) = (local(a), local(b))
        {
            out.push_str(&if x <= y { format!("{a}, {b}") } else { format!("{b}, {a}") });
            rest = &rest[close..];
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

/// The most cells of the table of the longest common subsequence. A larger middle part is not
/// aligned: each of its elements differs (a doubt gives "different", never "equal").
const MOST_CELLS: usize = 4_000_000;

/// The pairs of the longest common subsequence of two key lists, with a weight: a pair with
/// `anchored` counts two times (the two places have the same length and the same offset from
/// the start or from the end of the body). The common start and end with anchored pairs align first.
fn align(old: &[String], new: &[String], anchored: impl Fn(usize, usize) -> bool) -> Vec<(usize, usize)> {
    let start = (0..old.len().min(new.len())).take_while(|&i| old[i] == new[i] && anchored(i, i)).count();
    let end = (0..(old.len() - start).min(new.len() - start)).take_while(|&k| old[old.len() - 1 - k] == new[new.len() - 1 - k] && anchored(old.len() - 1 - k, new.len() - 1 - k)).count();
    let mut pairs: Vec<(usize, usize)> = (0..start).map(|i| (i, i)).collect();
    let (a, b) = (&old[start..old.len() - end], &new[start..new.len() - end]);
    if !a.is_empty() && !b.is_empty() && (a.len() + 1).saturating_mul(b.len() + 1) <= MOST_CELLS {
        let width = b.len() + 1;
        let weight = |i: usize, j: usize| if anchored(start + i, start + j) { 2 } else { 1 };
        let mut table = vec![0u32; (a.len() + 1) * width];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                let skip = table[(i + 1) * width + j].max(table[i * width + j + 1]);
                table[i * width + j] = if a[i] == b[j] { skip.max(table[(i + 1) * width + j + 1] + weight(i, j)) } else { skip };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if a[i] == b[j] && table[i * width + j] == table[(i + 1) * width + j + 1] + weight(i, j) {
                pairs.push((start + i, start + j));
                i += 1;
                j += 1;
            } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    pairs.extend((0..end).map(|k| (old.len() - end + k, new.len() - end + k)));
    pairs
}

/// Folds each copy into a temporary (`_t = _p`) that the next element of the same block uses,
/// when the temporary has no other use: the use reads `_p` in its key. Returns the use of each
/// folded copy. The copy and its use are next to each other, so no write is between them.
fn fold_copies(parsed: &Parsed, keys: &mut [(String, Vec<Number>)], args: usize) -> Vec<Option<usize>> {
    let mut uses: BTreeMap<usize, usize> = BTreeMap::new();
    for (_, numbers) in keys.iter() {
        for number in numbers {
            if let Number::Local(local) = number {
                *uses.entry(*local).or_default() += 1;
            }
        }
    }
    let mut folded = vec![None; keys.len()];
    for index in 0..keys.len().saturating_sub(1) {
        let (Number::Local(temp), Number::Local(source)) = (match keys[index].1[..] {
            [first, second] if keys[index].0 == "_ = _" => (first, second),
            _ => continue,
        }) else { continue };
        let next = index + 1;
        let typed = parsed.locals.get(&temp).is_some_and(|ty| parsed.locals.get(&source) == Some(ty));
        if temp <= args || !typed || parsed.elements[next].1 != parsed.elements[index].1 || uses.get(&temp) != Some(&2) || !keys[next].1.contains(&Number::Local(temp)) {
            continue;
        }
        for number in keys[next].1.iter_mut() {
            if *number == Number::Local(temp) {
                *number = Number::Local(source);
            }
        }
        folded[index] = Some(next);
    }
    folded
}

/// The place of an element for its order: the outermost macro call, else its tokens.
fn place_of(ranges: &(Option<Place>, Option<Place>)) -> Option<&Place> {
    ranges.1.as_ref().or(ranges.0.as_ref())
}

/// The file, the first start, and the last end of the places of a body (in the file of its
/// first place).
fn bounds(ranges: &[(Option<Place>, Option<Place>)]) -> Option<(String, usize, usize)> {
    let file = ranges.iter().find_map(place_of)?.file.clone();
    let places = ranges.iter().filter_map(place_of).filter(|place| place.file == file);
    let (start, end) = places.fold((usize::MAX, 0), |(start, end), place| (start.min(place.start), end.max(place.end)));
    Some((file, start, end))
}

/// The element whose place an element with no range takes (design 40.3, alignment 9; FIX 2 of
/// design 45): the next element of its own block with a range (the driver wraps a value as
/// opaque just before its use), else the next element with a range (an `opaque` call ends its
/// block, and its value goes to the next block), else the element before it. An element with a
/// range takes its own place. The order of the alignment and the owner rules use the same place.
fn near(blocks: &[usize], ranges: &[(Option<Place>, Option<Place>)], index: usize) -> usize {
    let ranged = |at: &usize| ranges[*at].0.is_some() || ranges[*at].1.is_some();
    if ranged(&index) {
        return index;
    }
    let block = blocks[index];
    let in_block = |at: &usize| blocks[*at] == block && ranged(at);
    (index + 1..ranges.len()).find(in_block).or_else(|| (index + 1..ranges.len()).find(ranged)).or_else(|| (0..index).rev().find(ranged)).unwrap_or(index)
}

/// The elements in the order of the source: by the start of their place, the longer place
/// first. An element with no place takes the place of [`near`]. A folded copy is not in the
/// list.
fn source_order(blocks: &[usize], ranges: &[(Option<Place>, Option<Place>)], folded: &[Option<usize>]) -> Vec<usize> {
    let mut keyed = Vec::new();
    for index in 0..ranges.len() {
        if folded[index].is_some() {
            continue;
        }
        let key = place_of(&ranges[near(blocks, ranges, index)]).map_or((String::new(), usize::MAX, 0), |place| (place.file.clone(), place.start, usize::MAX - place.end));
        keyed.push((key, index));
    }
    keyed.sort();
    keyed.into_iter().map(|(_, index)| index).collect()
}

/// A one-to-one map of numbers.
#[derive(Default)]
struct Bijection {
    forward: BTreeMap<usize, usize>,
    back: BTreeMap<usize, usize>,
}

impl Bijection {
    fn add(&mut self, a: usize, b: usize) {
        self.forward.insert(a, b);
        self.back.insert(b, a);
    }
}

/// The branch data of a terminator text.
fn branch(text: &str) -> (Option<usize>, bool) {
    if text.starts_with("switchInt(") {
        let targets = text.rsplit_once(" -> [").map_or(0, |(_, list)| list.matches("bb").count());
        return (Some(targets), false);
    }
    (None, text == "return" || text.contains("-> [no return"))
}

/// The diffs that this process computed (design 40.6): the configurations of a step often
/// have the same two texts. The key is a hash of the two texts and their ranges.
static DIFFS: std::sync::Mutex<Option<std::collections::HashMap<u64, Option<ElementDiff>>>> = std::sync::Mutex::new(None);

/// The most diffs in memory. Then the memory starts again.
const MOST_DIFFS: usize = 20_000;

/// [`diff`] with the diffs in memory.
pub fn diff_cached(old: (&str, &RecordElements), new: (&str, &RecordElements)) -> Option<ElementDiff> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (old, new).hash(&mut hasher);
    let key = hasher.finish();
    if let Some(found) = DIFFS.lock().unwrap_or_else(|error| error.into_inner()).as_ref().and_then(|known| known.get(&key)) {
        return found.clone();
    }
    let found = diff(old, new);
    let mut known = DIFFS.lock().unwrap_or_else(|error| error.into_inner());
    let known = known.get_or_insert_with(Default::default);
    if known.len() >= MOST_DIFFS {
        known.clear();
    }
    known.insert(key, found.clone());
    found
}

/// Compares two canonical texts with their element ranges. `None` if the ranges do not fit the
/// texts (an old record, or a text with a line that is not an element).
pub fn diff(old: (&str, &RecordElements), new: (&str, &RecordElements)) -> Option<ElementDiff> {
    let no_symbols = old.1.v == 0 || new.1.v == 0;
    let (old_record, new_record) = (old.1, new.1);
    let symbols = [old.1.symbols().unwrap_or_default(), new.1.symbols().unwrap_or_default()];
    let sides = [old, new].map(|(text, elements)| (parse(text), elements.ranges()));
    for (parsed, ranges) in &sides {
        if ranges.as_ref().is_none_or(|ranges| ranges.len() != parsed.elements.len()) {
            return None;
        }
    }
    let [(old, old_ranges), (new, new_ranges)] = sides;
    let (old_ranges, new_ranges) = (old_ranges?, new_ranges?);
    let keys = |parsed: &Parsed| -> Vec<(String, Vec<Number>)> { parsed.elements.iter().map(|(text, _)| key(&symmetric_order(text))).collect() };
    let (mut old_keys, mut new_keys) = (keys(&old), keys(&new));
    let args = old.args.min(new.args);
    // Step 1: a copy into a temporary that the next element uses one time folds into that
    // element.
    let old_folded = fold_copies(&old, &mut old_keys, args);
    let new_folded = fold_copies(&new, &mut new_keys, args);
    // Step 2: the order of the source, and the weighted alignment.
    let block_of = |parsed: &Parsed| -> Vec<usize> { parsed.elements.iter().map(|(_, block)| *block).collect() };
    let (old_blocks, new_blocks) = (block_of(&old), block_of(&new));
    let old_order = source_order(&old_blocks, &old_ranges, &old_folded);
    let new_order = source_order(&new_blocks, &new_ranges, &new_folded);
    let (old_bounds, new_bounds) = (bounds(&old_ranges), bounds(&new_ranges));
    let anchored = |a: usize, b: usize| -> bool {
        let (Some(x), Some(y), Some((file_a, start_a, end_a)), Some((file_b, start_b, end_b))) = (place_of(&old_ranges[a]), place_of(&new_ranges[b]), &old_bounds, &new_bounds) else { return false };
        let same_length = x.end - x.start == y.end - y.start;
        x.file == *file_a && y.file == *file_b && same_length && (x.start.checked_sub(*start_a) == y.start.checked_sub(*start_b) || end_a.checked_sub(x.end) == end_b.checked_sub(y.end))
    };
    let ordered = |order: &[usize], keys: &[(String, Vec<Number>)]| -> Vec<String> { order.iter().map(|index| keys[*index].0.clone()).collect() };
    let pairs: Vec<(usize, usize)> = align(&ordered(&old_order, &old_keys), &ordered(&new_order, &new_keys), |i, j| anchored(old_order[i], new_order[j])).into_iter().map(|(i, j)| (old_order[i], new_order[j])).collect();
    // Step 3: the map of the locals by votes. Each pair votes for the pairs of its locals; the
    // pairs with the most votes win, one-to-one, with equal types.
    let mut locals = Bijection::default();
    for local in 0..=args {
        locals.add(local, local);
    }
    let vote = |pairs: &[(usize, usize)], locals: &mut Bijection| {
        let mut votes: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        for (a, b) in pairs {
            for (x, y) in old_keys[*a].1.iter().zip(&new_keys[*b].1) {
                if let (Number::Local(x), Number::Local(y)) = (x, y) {
                    *votes.entry((*x, *y)).or_default() += 1;
                }
            }
        }
        let mut ranked: Vec<((usize, usize), usize)> = votes.into_iter().collect();
        ranked.sort_by(|(p, a), (q, b)| b.cmp(a).then(p.cmp(q)));
        for ((x, y), _) in ranked {
            let typed = old.locals.get(&x).is_some_and(|ty| new.locals.get(&y) == Some(ty));
            if typed && !locals.forward.contains_key(&x) && !locals.back.contains_key(&y) {
                locals.add(x, y);
            }
        }
    };
    vote(&pairs, &mut locals);
    // Step 4: a second alignment with the locals of the map: two elements with equal keys and
    // locals that do not agree with the map do not pair. A local with no partner is `?` on the
    // two sides. Then the new pairs vote for the locals with no partner.
    let concrete = |order: &[usize], keys: &[(String, Vec<Number>)], map: &dyn Fn(usize) -> Option<usize>| -> Vec<String> {
        order
            .iter()
            .map(|index| {
                let (key, numbers) = &keys[*index];
                let numbers: Vec<String> = numbers.iter().map(|number| match number {
                    Number::Local(local) => map(*local).map_or("?".to_string(), |found| found.to_string()),
                    Number::Block(_) => "b".to_string(),
                }).collect();
                format!("{key}|{}", numbers.join(","))
            })
            .collect()
    };
    let old_concrete = concrete(&old_order, &old_keys, &|local| locals.forward.get(&local).copied());
    let new_concrete = concrete(&new_order, &new_keys, &|local| locals.back.contains_key(&local).then_some(local));
    let pairs: Vec<(usize, usize)> = align(&old_concrete, &new_concrete, |i, j| anchored(old_order[i], new_order[j])).into_iter().map(|(i, j)| (old_order[i], new_order[j])).collect();
    vote(&pairs, &mut locals);
    // Pass 1: a pair fits if each of its locals has its partner in the map. The partner of
    // each element of the old side, and the block targets of the pair.
    let mut partner: Vec<Option<usize>> = vec![None; old.elements.len()];
    let mut targets: Vec<Vec<(usize, usize)>> = vec![Vec::new(); old.elements.len()];
    for &(a, b) in &pairs {
        let (old_numbers, new_numbers) = (&old_keys[a].1, &new_keys[b].1);
        let mut fits = old_numbers.len() == new_numbers.len();
        let mut block_pairs = Vec::new();
        for (x, y) in old_numbers.iter().zip(new_numbers) {
            match (x, y) {
                (Number::Local(x), Number::Local(y)) => fits &= locals.forward.get(x) == Some(y),
                (Number::Block(x), Number::Block(y)) => block_pairs.push((*x, *y)),
                _ => fits = false,
            }
        }
        if fits {
            partner[a] = Some(b);
            targets[a] = block_pairs;
        }
    }
    // Pass 2: a target goes to a block, and a block starts with its first element. Each
    // target of a pair must go to the first elements of two partner blocks. A pair that breaks
    // this rule differs; the check runs again until no pair changes.
    let heads = |parsed: &Parsed| -> BTreeMap<usize, usize> {
        let mut heads = BTreeMap::new();
        for (index, (_, block)) in parsed.elements.iter().enumerate() {
            heads.entry(*block).or_insert(index);
        }
        heads
    };
    let (old_heads, new_heads) = (heads(&old), heads(&new));
    loop {
        let mut changed = false;
        let taken: BTreeSet<usize> = partner.iter().flatten().copied().collect();
        for a in 0..old.elements.len() {
            if partner[a].is_none() {
                continue;
            }
            // A first element that differs is the place of a change with its own elements (code
            // that a step adds before the old first element): the target fits.
            let fits = targets[a].iter().all(|(x, y)| matches!((old_heads.get(x), new_heads.get(y)), (Some(hx), Some(hy)) if partner[*hx] == Some(*hy) || partner[*hx].is_none() || !taken.contains(hy)));
            if !fits {
                partner[a] = None;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut same: Vec<bool> = partner.iter().map(Option::is_some).collect();
    let mut same_new = vec![false; new.elements.len()];
    for b in partner.iter().flatten() {
        same_new[*b] = true;
    }
    // A folded copy is the same as its use (the use comes later in the list).
    for (same, folded) in [(&mut same, &old_folded), (&mut same_new, &new_folded)] {
        for index in (0..folded.len()).rev() {
            if let Some(user) = folded[index] {
                same[index] = same[user];
            }
        }
    }
    // Design 45: the changed locals and cleanup blocks that explain each element that differs.
    let sources = {
        let side = |parsed, keys, same, blocks, ranges, heads, record| Side { parsed, keys, same, blocks, ranges, heads, record };
        let sides = [side(&old, &old_keys, &same, &old_blocks, &old_ranges, &old_heads, old_record), side(&new, &new_keys, &same_new, &new_blocks, &new_ranges, &new_heads, new_record)];
        dependence(&sides, &locals, &partner, &pairs)
    };
    let element = |parsed: &Parsed, blocks: &[usize], ranges: &[(Option<Place>, Option<Place>)], symbols: &[Vec<Symbol>], sources: &[Vec<Group>], index: usize| {
        let (targets, exit) = branch(parsed.elements[index].0);
        // An element with no range takes the range of [`near`].
        let (tokens, call) = ranges[near(blocks, ranges, index)].clone();
        let text = parsed.elements[index].0;
        let jump = text.starts_with("goto -> ") || matches!(text, "return" | "resume" | "unreachable");
        let glue = jump || text.starts_with("drop(");
        let symbols = symbols.get(index).cloned().unwrap_or_default();
        let sources = sources.get(index).cloned().unwrap_or_default();
        Element { tokens, call, targets, exit, glue, jump, symbols, sources }
    };
    let mut out = ElementDiff {
        old: (0..old.elements.len()).filter(|i| !same[*i]).map(|i| element(&old, &old_blocks, &old_ranges, &symbols[0], &sources[0], i)).collect(),
        new: (0..new.elements.len()).filter(|i| !same_new[*i]).map(|i| element(&new, &new_blocks, &new_ranges, &symbols[1], &sources[1], i)).collect(),
        other: old.other != new.other || old.args != new.args,
        no_symbols,
        evaluated: old_record.c || new_record.c,
        own: [&old_record, &new_record].iter().enumerate().filter_map(|(side, record)| Some((side, record_symbol(&record.f, record.s.get(record.o? as usize)?)?))).collect(),
        pairs: partner.iter().enumerate().filter_map(|(a, b)| Some((old_ranges[a].0.clone()?, new_ranges[(*b)?].0.clone()?))).collect(),
    };
    unique(&mut out.old);
    unique(&mut out.new);
    out.pairs.sort();
    out.pairs.dedup();
    Some(out)
}

/// The data of one side of a diff for the dependence rule (design 45).
struct Side<'a> {
    parsed: &'a Parsed<'a>,
    keys: &'a [(String, Vec<Number>)],
    same: &'a [bool],
    blocks: &'a [usize],
    ranges: &'a [(Option<Place>, Option<Place>)],
    heads: &'a BTreeMap<usize, usize>,
    record: &'a RecordElements,
}

impl Side<'_> {
    /// The source that names an element of this side by its place.
    fn element(&self, side: usize, index: usize) -> Source {
        let (tokens, call) = self.ranges[near(self.blocks, self.ranges, index)].clone();
        Source::Element(side, tokens, call)
    }

    /// The locals of an element, in the order of its text.
    fn locals(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        self.keys[index].1.iter().filter_map(|number| match number {
            Number::Local(local) => Some(*local),
            Number::Block(_) => None,
        })
    }
}

/// The local that an element writes: the local of the place before ` = ` (`_3 = …`,
/// `(_3.0: u32) = …`, a call `_3 = f(…) -> …`).
fn written(text: &str) -> Option<usize> {
    let (place, _) = text.split_once(" = ")?;
    if !(place.starts_with('_') || place.starts_with('(')) {
        return None;
    }
    let at = place.find('_')?;
    let digits: String = place[at + 1..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The unwind action of a terminator text: `Some(Some(block))` for `unwind: bbN`, `Some(None)`
/// for an other action (`continue`, `unreachable`, `terminate`), `None` with no unwind action.
fn unwind_of(text: &str) -> Option<Option<usize>> {
    let at = text.rfind("unwind")?;
    let rest = &text[at..];
    Some(rest.strip_prefix("unwind: bb").and_then(|rest| rest.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()))
}

/// The text of an element with the type of each field projection of a changed local replaced by
/// `?T` (`(_3.0: u32)` gives `(_3.0: ?T)`): the type of the field follows the type of the local
/// (design 45). A projection through a deref (`((*_3).0: u32)`) counts too.
fn projected(text: &str, changed: &BTreeSet<usize>) -> String {
    let mut out = text.to_string();
    for local in changed {
        for open in [format!("(_{local}."), format!("((*_{local})."), format!("((_{local} as ")] {
            let mut from = 0;
            while let Some(found) = out[from..].find(&open) {
                let start = from + found;
                // The matching parenthesis of the projection.
                let mut depth = 0i32;
                let mut close = None;
                for (at, c) in out[start..].char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(start + at);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let Some(close) = close else { break };
                match out[start..close].rfind(": ") {
                    Some(colon) => {
                        out.replace_range(start + colon + 2..close, "?T");
                        from = start + colon + 2;
                    }
                    None => from = close,
                }
            }
        }
    }
    out
}

/// The key of an element with the types of its changed locals replaced by `?T`, and each
/// unwind action replaced by `unwind ?` (design 45, the exact test of an element with no
/// partner). A type of a local goes with its references removed (`&Wrapping<u32>` gives
/// `Wrapping<u32>` too): an operator and a method call name the type of the value. A type
/// matches at a word boundary; a literal suffix (`3_u32`) and the form in a path
/// (`Vec::<u8>::new`, fix round 1) count.
fn shape(key: &str, types: &[&str]) -> String {
    let mut words: Vec<String> = Vec::new();
    for ty in types {
        let mut ty = *ty;
        loop {
            if !words.iter().any(|word| word == ty) && !ty.is_empty() && ty != "()" {
                words.push(ty.to_string());
                // The form of the type in a path: `std::vec::Vec::<u8>::new` (fix round 1).
                if let Some(at) = ty.find('<').filter(|at| *at > 0 && !ty[..*at].ends_with("::") && !ty.starts_with(['<', '(', '['])) {
                    words.push(format!("{}::{}", &ty[..at], &ty[at..]));
                }
            }
            match ty.strip_prefix("&mut ").or_else(|| ty.strip_prefix('&')).or_else(|| ty.strip_prefix("*const ")).or_else(|| ty.strip_prefix("*mut ")) {
                Some(inner) => ty = inner,
                None => break,
            }
        }
    }
    words.sort_by_key(|word| std::cmp::Reverse(word.len()));
    let word_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(key.len());
    let mut at = 0;
    'scan: while at < key.len() {
        for word in &words {
            if key[at..].starts_with(word) {
                let before = key[..at].chars().next_back();
                let suffix = before == Some('_') && key[..at].chars().rev().nth(1).is_some_and(|c| c.is_ascii_digit());
                let left = before.is_none_or(|c| !word_char(c)) || suffix || !word.starts_with(word_char);
                let right = key[at + word.len()..].chars().next().is_none_or(|c| !word_char(c)) || !word.ends_with(word_char);
                if left && right {
                    out.push_str("?T");
                    at += word.len();
                    continue 'scan;
                }
            }
        }
        let next = key[at..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&key[at..at + next]);
        at += next;
    }
    for action in ["unwind: bb", "unwind continue", "unwind unreachable", "unwind terminate"] {
        out = out.replace(action, "unwind ?");
    }
    out
}

/// The sources of each element that differs (design 45). A local is changed if it has no
/// partner in the map of the locals, or a partner of a different type (a parameter and the
/// return place map to themselves). The sources of a changed local (one group): its declaration
/// ranges, the changed elements that write it, and for a local that gets its type by inference
/// (a temporary, a user variable with no type annotation) also the changed elements that read
/// it (fix round 1).
/// An element gets the sources only if one exact test passes:
/// 1. the alignment paired it with an element of the same key, and each local that does not
///    agree with the map is changed on one side (its targets follow rule 7);
/// 2. it has no such pair, and its key is equal to the key of an element that differs on the
///    other side after the types of the changed locals of each are replaced (and the unwind
///    action: an unwind edge that one side has and the other side has not needs a first
///    element of the cleanup block that differs, and takes its causes);
/// 3. it has no counterpart, and it writes a changed temporary that a changed element reads,
///    or it drops a changed local, or it borrows or dereferences a changed local (an auto-ref):
///    the element exists for that local.
fn dependence(sides: &[Side; 2], locals: &Bijection, partner: &[Option<usize>], candidates: &[(usize, usize)]) -> [Vec<Vec<Group>>; 2] {
    let mut out: [Vec<Vec<Group>>; 2] = [vec![Vec::new(); sides[0].keys.len()], vec![Vec::new(); sides[1].keys.len()]];
    if sides.iter().all(|side| side.same.iter().all(|same| *same)) {
        return out;
    }
    // The changed locals of each side.
    let typed = |side: usize, local: usize| sides[side].parsed.locals.get(&local).copied();
    let changed: [BTreeSet<usize>; 2] = [
        sides[0].parsed.locals.keys().copied().filter(|x| locals.forward.get(x).is_none_or(|y| typed(0, *x) != typed(1, *y))).collect(),
        sides[1].parsed.locals.keys().copied().filter(|y| locals.back.get(y).is_none_or(|x| typed(0, *x) != typed(1, *y))).collect(),
    ];
    let declarations = [sides[0].record.declarations(), sides[1].record.declarations()];
    let written: [Vec<Option<usize>>; 2] = [0, 1].map(|side| sides[side].parsed.elements.iter().map(|(text, _)| written(text)).collect());
    // The changed elements that write each changed local, and that read it (one pass).
    let mut users: [BTreeMap<usize, (Vec<usize>, Vec<usize>)>; 2] = [BTreeMap::new(), BTreeMap::new()];
    for side in 0..2 {
        let data = &sides[side];
        for index in (0..data.keys.len()).filter(|index| !data.same[*index]) {
            for local in data.locals(index).filter(|local| changed[side].contains(local)) {
                let entry = users[side].entry(local).or_default();
                let list = if written[side][index] == Some(local) { &mut entry.0 } else { &mut entry.1 };
                if list.last() != Some(&index) {
                    list.push(index);
                }
            }
        }
    }
    // A local whose type can come from the elements that read it (fix round 1): a temporary,
    // and a user variable with no type annotation (one declaration range at most).
    let inferred = |side: usize, local: usize| declarations[side].get(local).is_some_and(|(kind, places)| *kind == LOCAL_TEMPORARY || (*kind == LOCAL_USER && places.len() < 2));
    // The sources of each changed local.
    let local_sources = |side: usize, local: usize| -> Vec<Source> {
        let data = &sides[side];
        let places = declarations[side].get(local).map(|(_, places)| places.clone()).unwrap_or_default();
        let mut found: Vec<Source> = places.into_iter().map(|place| Source::Declaration(side, place)).collect();
        if let Some((writers, readers)) = users[side].get(&local) {
            let readers = readers.iter().filter(|index| inferred(side, local) && !writers.contains(index));
            found.extend(writers.iter().chain(readers).map(|index| data.element(side, *index)));
        }
        found
    };
    // The type of a local follows an element that writes it, or that reads it when the local
    // gets its type by inference (a temporary, a user variable with no type annotation).
    let follows = |side: usize, index: usize, local: usize| written[side][index] == Some(local) || inferred(side, local);
    let mut memo: [BTreeMap<usize, Vec<Source>>; 2] = [BTreeMap::new(), BTreeMap::new()];
    let mut of_local = |side: usize, local: usize| -> Vec<Source> { memo[side].entry(local).or_insert_with(|| local_sources(side, local)).clone() };
    // A target of a pair fits if the first elements of the two blocks are a pair, or if one of
    // them differs (rule 7).
    let partner_new: BTreeMap<usize, usize> = partner.iter().enumerate().filter_map(|(a, b)| Some(((*b)?, a))).collect();
    let target_fits = |x: usize, y: usize| -> bool {
        match (sides[0].heads.get(&x), sides[1].heads.get(&y)) {
            (Some(hx), Some(hy)) => partner[*hx] == Some(*hy) || partner[*hx].is_none() || !partner_new.contains_key(hy),
            _ => false,
        }
    };
    // The check of a counterpart pair (old `a`, new `b`): each local that does not agree with
    // the map is changed on the two sides, each target fits, and an unwind edge of one side
    // only goes to a cleanup block whose first element differs. The groups, or `None`.
    let mut check = |a: usize, b: usize| -> Option<Vec<Group>> {
        let (old_numbers, new_numbers) = (&sides[0].keys[a].1, &sides[1].keys[b].1);
        let (old_text, new_text) = (sides[0].parsed.elements[a].0, sides[1].parsed.elements[b].0);
        let (old_unwind, new_unwind) = (unwind_of(old_text), unwind_of(new_text));
        let strip = |numbers: &[Number], unwind: Option<Option<usize>>| -> Vec<Number> {
            let mut list = numbers.to_vec();
            if let Some(Some(_)) = unwind
                && let Some(at) = list.iter().rposition(|number| matches!(number, Number::Block(_)))
            {
                list.remove(at);
            }
            list
        };
        let (old_list, new_list) = (strip(old_numbers, old_unwind), strip(new_numbers, new_unwind));
        if old_list.len() != new_list.len() {
            return None;
        }
        let mut found = Vec::new();
        let mut explained = false;
        for (x, y) in old_list.iter().zip(&new_list) {
            match (x, y) {
                (Number::Local(x), Number::Local(y)) => {
                    let (old_changed, new_changed) = (changed[0].contains(x), changed[1].contains(y));
                    if locals.forward.get(x) == Some(y) && !old_changed && !new_changed {
                        continue;
                    }
                    // A read of a different value (a local of the map on one side) is no
                    // dependence: the locals of the two sides must both be changed, or be
                    // partners of a different type.
                    if !(old_changed && new_changed) {
                        return None;
                    }
                    explained = true;
                    found.push(Group { follows: follows(0, a, *x), sources: of_local(0, *x) });
                    found.push(Group { follows: follows(1, b, *y), sources: of_local(1, *y) });
                }
                (Number::Block(x), Number::Block(y)) => {
                    if !target_fits(*x, *y) {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        // Each other changed local of the two elements (a type that the key shows).
        for local in sides[0].locals(a).filter(|local| changed[0].contains(local)).collect::<Vec<_>>() {
            explained = true;
            found.push(Group { follows: follows(0, a, local), sources: of_local(0, local) });
        }
        for local in sides[1].locals(b).filter(|local| changed[1].contains(local)).collect::<Vec<_>>() {
            explained = true;
            found.push(Group { follows: follows(1, b, local), sources: of_local(1, local) });
        }
        match (old_unwind, new_unwind) {
            (Some(Some(x)), Some(Some(y))) => {
                if !target_fits(x, y) {
                    return None;
                }
            }
            (Some(Some(x)), Some(None)) => {
                let head = *sides[0].heads.get(&x)?;
                if sides[0].same[head] {
                    return None;
                }
                explained = true;
                found.push(Group { follows: false, sources: vec![sides[0].element(0, head)] });
            }
            (Some(None), Some(Some(y))) => {
                let head = *sides[1].heads.get(&y)?;
                if sides[1].same[head] {
                    return None;
                }
                explained = true;
                found.push(Group { follows: false, sources: vec![sides[1].element(1, head)] });
            }
            (x, y) if x == y => {}
            _ => return None,
        }
        explained.then_some(found)
    };
    let mut done = [vec![false; sides[0].keys.len()], vec![false; sides[1].keys.len()]];
    // Test 1: the pairs of the alignment that differ.
    for &(a, b) in candidates {
        if sides[0].same[a] || sides[1].same[b] || partner[a] == Some(b) {
            continue;
        }
        done[0][a] = true;
        done[1][b] = true;
        if let Some(found) = check(a, b) {
            out[0][a].extend(found.iter().cloned());
            out[1][b].extend(found);
        }
    }
    // Test 2: the elements with no pair, by the key with the types of the changed locals
    // replaced, in the order of the source.
    let shaped = |side: usize| -> Vec<(usize, String)> {
        let data = &sides[side];
        let mut list: Vec<(usize, String)> = (0..data.keys.len())
            .filter(|index| !data.same[*index] && !done[side][*index])
            .map(|index| {
                let types: Vec<&str> = data.locals(index).filter(|local| changed[side].contains(local)).filter_map(|local| typed(side, local)).collect();
                let text = data.parsed.elements[index].0;
                let key = if types.is_empty() { data.keys[index].0.clone() } else { key(&symmetric_order(&projected(text, &changed[side]))).0 };
                (index, shape(&key, &types))
            })
            .collect();
        list.sort_by_key(|(index, _)| (place_of(&data.ranges[near(data.blocks, data.ranges, *index)]).map(|place| (place.file.clone(), place.start, usize::MAX - place.end)), *index));
        list
    };
    let (old_list, new_list) = (shaped(0), shaped(1));
    let old_shapes: Vec<String> = old_list.iter().map(|(_, shape)| shape.clone()).collect();
    let new_shapes: Vec<String> = new_list.iter().map(|(_, shape)| shape.clone()).collect();
    for (i, j) in align(&old_shapes, &new_shapes, |_, _| false) {
        let (a, b) = (old_list[i].0, new_list[j].0);
        done[0][a] = true;
        done[1][b] = true;
        if let Some(found) = check(a, b) {
            out[0][a].extend(found.iter().cloned());
            out[1][b].extend(found);
        }
    }
    // Test 3: an element with no counterpart that writes a changed temporary for a changed
    // reader, or that drops a changed local.
    for side in 0..2 {
        let data = &sides[side];
        for index in (0..data.keys.len()).filter(|index| !data.same[*index] && !done[side][*index]) {
            let text = data.parsed.elements[index].0;
            let mine: Vec<usize> = data.locals(index).collect();
            let temporary = written[side][index].filter(|local| {
                changed[side].contains(local)
                    && declarations[side].get(*local).is_some_and(|(kind, _)| *kind == LOCAL_TEMPORARY)
                    && (0..data.keys.len()).any(|reader| reader != index && !data.same[reader] && written[side][reader] != Some(*local) && data.locals(reader).any(|other| other == *local))
            });
            let dropped = text.starts_with("drop(").then(|| mine.first().copied()).flatten().filter(|local| changed[side].contains(local));
            // An auto-ref or an auto-deref (`_t = &_x`, `_t = &(*_x)`, `_t = (*_x)`) of a changed
            // local: the adjustment follows the type of the local.
            let adjusted = text.split_once(" = ").filter(|(_, value)| value.starts_with('&') || value.starts_with("(*_")).and_then(|_| mine.get(1).copied()).filter(|local| changed[side].contains(local));
            if temporary.is_none() && dropped.is_none() && adjusted.is_none() {
                continue;
            }
            for local in mine.into_iter().filter(|local| changed[side].contains(local)) {
                out[side][index].push(Group { follows: follows(side, index, local), sources: of_local(side, local) });
            }
        }
    }
    for groups in out.iter_mut().flatten() {
        for group in groups.iter_mut() {
            group.sources.sort();
            group.sources.dedup();
        }
        groups.sort();
        groups.dedup();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(n: usize) -> RecordElements {
        RecordElements { n, f: vec!["src/lib.rs".into()], r: (0..n).map(|i| vec![i as i64, 1, 0, i as i64 * 10, i as i64 * 10 + 5]).collect(), ..Default::default() }
    }

    fn text(elements: &[&str]) -> String {
        // A terminator ends its block.
        let mut out = "fn _(_1: u32) -> u32 {\n    let _0: u32;\n    let _2: u32;\n    let _3: u32;\n    bb0: {\n".to_string();
        let mut block = 0;
        for (index, element) in elements.iter().enumerate() {
            out.push_str(&format!("        {element};\n"));
            if (*element == "return" || element.starts_with("switchInt")) && index + 1 < elements.len() {
                block += 1;
                out.push_str(&format!("    }}\n    bb{block}: {{\n"));
            }
        }
        out.push_str("    }\n}\n");
        out
    }

    #[test]
    fn a_new_statement_shifts_no_other_element() {
        let old = text(&["_2 = Add(_1, const u32 = 1)", "_0 = _2", "return"]);
        let new = text(&["_2 = Mul(_1, const u32 = 3)", "_3 = Add(_1, const u32 = 1)", "_0 = _3", "return"]);
        let diff = super::diff((&old, &record(3)), (&new, &record(4))).unwrap();
        assert!(diff.old.is_empty(), "{diff:?}");
        assert_eq!(diff.new.len(), 1);
        assert_eq!(diff.new[0].tokens.as_ref().unwrap().start, 0);
        assert!(!diff.other);
    }

    #[test]
    fn a_swap_of_two_operands_differs() {
        let old = text(&["_2 = Sub(_1, _3)", "_0 = _2", "return"]);
        let new = text(&["_2 = Sub(_3, _1)", "_0 = _2", "return"]);
        let diff = super::diff((&old, &record(3)), (&new, &record(3))).unwrap();
        assert_eq!((diff.old.len(), diff.new.len()), (1, 1), "{diff:?}");
    }

    #[test]
    fn a_swap_of_two_targets_differs_and_a_switch_has_its_targets() {
        let old = text(&["switchInt(_1) -> [0: bb1, otherwise: bb2]", "_0 = const u32 = 1", "return", "_0 = const u32 = 2", "return"]);
        let new = text(&["switchInt(_1) -> [0: bb2, otherwise: bb1]", "_0 = const u32 = 1", "return", "_0 = const u32 = 2", "return"]);
        let diff = super::diff((&old, &record(5)), (&new, &record(5))).unwrap();
        assert!(!diff.old.is_empty() && !diff.new.is_empty(), "{diff:?}");
        let old = text(&["_0 = _1", "return"]);
        let new = text(&["switchInt(_1) -> [0: bb1, otherwise: bb2]", "_0 = _1", "return"]);
        let diff = super::diff((&old, &record(2)), (&new, &record(3))).unwrap();
        assert_eq!(diff.new[0].targets, Some(2));
        assert!(diff.new.iter().all(|element| !element.exit));
    }

    #[test]
    fn a_changed_local_type_and_a_changed_signature_differ() {
        let old = text(&["_2 = _1", "_0 = _2", "return"]);
        let new = old.replace("let _2: u32", "let _2: u64");
        let found = diff((&old, &record(3)), (&new, &record(3))).unwrap();
        assert!(!found.old.is_empty());
        let new = old.replace("fn _(_1: u32)", "fn _(_1: u64)");
        assert!(diff_other(&old, &new));
    }

    fn diff_other(old: &str, new: &str) -> bool {
        diff((old, &record(3)), (new, &record(3))).unwrap().other
    }

    #[test]
    fn ranges_that_do_not_fit_give_no_diff() {
        let old = text(&["_0 = _1", "return"]);
        assert!(diff((&old, &record(3)), (&old, &record(2))).is_none());
        let runs = RecordElements { n: 2, f: vec!["a.rs".into()], r: vec![vec![0, 2, 0, 4, 9, 0, 1, 20]], ..Default::default() };
        let ranges = runs.ranges().unwrap();
        assert_eq!(ranges[1].0.as_ref().unwrap().start, 4);
        assert_eq!(ranges[1].1.as_ref().unwrap().end, 20);
    }

    /// The element ranges with the given starts (each range has the length 5).
    fn record_at(starts: &[usize]) -> RecordElements {
        RecordElements { n: starts.len(), f: vec!["src/lib.rs".into()], r: starts.iter().enumerate().map(|(i, start)| vec![i as i64, 1, 0, *start as i64, *start as i64 + 5]).collect(), ..Default::default() }
    }

    /// A text with explicit blocks.
    fn blocks(blocks: &[&[&str]]) -> String {
        let mut out = "fn _(_1: u32) -> u32 {\n    let _0: u32;\n    let _2: u32;\n    let _3: u32;\n".to_string();
        for (index, block) in blocks.iter().enumerate() {
            out.push_str(&format!("    bb{index}: {{\n"));
            for element in *block {
                out.push_str(&format!("        {element};\n"));
            }
            out.push_str("    }\n");
        }
        out.push_str("}\n");
        out
    }

    #[test]
    fn a_copy_into_a_temporary_folds_into_its_use() {
        // Fix round 1, D1: a deleted borrow lets the normal form forward a copy on one side.
        let old = text(&["_2 = _1", "_0 = Add(_2, const u32 = 1)", "return"]);
        let new = text(&["_0 = Add(_1, const u32 = 1)", "return"]);
        let found = super::diff((&old, &record(3)), (&new, &record(2))).unwrap();
        assert!(found.old.is_empty() && found.new.is_empty(), "{found:?}");
    }

    #[test]
    fn the_order_of_the_source_aligns_blocks_in_a_new_order() {
        // Fix round 1, D1: a negated condition swaps the targets, and the blocks come in a new
        // order. The order of the source pairs them; only the switch differs.
        let old = blocks(&[&["switchInt(_1) -> [0: bb1, otherwise: bb2]"], &["_0 = const u32 = 1", "return"], &["_0 = Add(_1, const u32 = 2)", "return"]]);
        let new = blocks(&[&["_2 = Not(_1)", "switchInt(_2) -> [0: bb1, otherwise: bb2]"], &["_0 = Add(_1, const u32 = 2)", "return"], &["_0 = const u32 = 1", "return"]]);
        let found = super::diff((&old, &record_at(&[0, 10, 12, 30, 32])), (&new, &record_at(&[0, 0, 30, 32, 10, 12]))).unwrap();
        let starts = |list: &[Element]| -> Vec<usize> { list.iter().filter_map(|element| element.tokens.as_ref().map(|place| place.start)).collect() };
        assert_eq!((starts(&found.old), starts(&found.new)), (vec![0], vec![0, 0]), "{found:?}");
        // The same switch with the old targets now goes to the other value: it differs.
        let swapped = blocks(&[&["switchInt(_1) -> [0: bb1, otherwise: bb2]"], &["_0 = Add(_1, const u32 = 2)", "return"], &["_0 = const u32 = 1", "return"]]);
        let found = super::diff((&old, &record_at(&[0, 10, 12, 30, 32])), (&swapped, &record_at(&[0, 30, 32, 10, 12]))).unwrap();
        assert_eq!((starts(&found.old), starts(&found.new)), (vec![0], vec![0]), "{found:?}");
    }

    #[test]
    fn the_map_of_the_locals_follows_the_votes() {
        // Fix round 1, D1: a deleted statement uses a local of the same type before the kept
        // statements. The kept statements pair by their place at the end of the body, and the
        // map of the locals follows the most pairs.
        let old = text(&["_2 = Add(_1, const u32 = 1)", "_3 = Add(_1, const u32 = 1)", "_0 = Mul(_3, _3)", "return"]);
        let new = text(&["_2 = Add(_1, const u32 = 1)", "_0 = Mul(_2, _2)", "return"]);
        let found = super::diff((&old, &record_at(&[0, 10, 20, 30])), (&new, &record_at(&[0, 10, 20]))).unwrap();
        let starts = |list: &[Element]| -> Vec<usize> { list.iter().filter_map(|element| element.tokens.as_ref().map(|place| place.start)).collect() };
        assert_eq!((starts(&found.old), found.new.len()), (vec![0], 0), "{found:?}");
    }

    #[test]
    fn the_operands_of_a_symmetric_operation_go_in_the_order_of_their_numbers() {
        // Fix round 2: the printer sorts `_100` before `_99`. A removed local renumbers the
        // locals, and an unchanged addition got a new order of its operands.
        assert_eq!(symmetric_order("_101 = CheckedAdd(_100, _99)"), "_101 = CheckedAdd(_99, _100)");
        assert_eq!(symmetric_order("assert(!(_101.1: bool), overflow Add(_100, _99)) -> [success: bb72]"), "assert(!(_101.1: bool), overflow Add(_99, _100)) -> [success: bb72]");
        assert_eq!(symmetric_order("_3 = Sub(_10, _9)"), "_3 = Sub(_10, _9)");
        assert_eq!(symmetric_order("_3 = Add(_10, const u32 = 1)"), "_3 = Add(_10, const u32 = 1)");
        let old = text(&["_2 = Add(_3, _1)", "_0 = _2", "return"]);
        let new = text(&["_3 = Add(_2, _1)", "_0 = _3", "return"]).replace("let _2: u32;\n    let _3: u32;", "let _2: u32;\n    let _3: u32;");
        let found = super::diff((&old, &record(3)), (&new, &record(3))).unwrap();
        assert!(found.old.is_empty() && found.new.is_empty(), "{found:?}");
    }

    /// A body with the given signature line, locals, and blocks.
    fn typed_body(signature: &str, locals: &[(usize, &str)], blocks: &[&[&str]]) -> String {
        let mut out = format!("{signature} {{\n");
        for (local, ty) in locals {
            out.push_str(&format!("    let _{local}: {ty};\n"));
        }
        for (index, block) in blocks.iter().enumerate() {
            out.push_str(&format!("    bb{index}: {{\n"));
            for element in *block {
                out.push_str(&format!("        {element};\n"));
            }
            out.push_str("    }\n");
        }
        out.push_str("}\n");
        out
    }

    /// A record with an element at each start (length 5) and the declarations of the locals.
    fn record_with(starts: &[usize], declarations: Vec<Vec<i64>>) -> RecordElements {
        RecordElements { l: declarations, ..record_at(starts) }
    }

    #[test]
    fn the_order_and_the_owner_place_of_an_element_with_no_range_agree() {
        // FIX 2 of design 45: the order key took the next element with a range in any block,
        // and the last element with no range took an empty key and sorted first. Now the order
        // key takes the place of the owner rule: the next element of its own block, else the
        // next element (an `opaque` call ends its block), else the element before it.
        let at = |start: usize| (Some(Place { file: "a.rs".into(), start, end: start + 2 }), None);
        let ranges = vec![(None, None), at(30), at(10), (None, None), (None, None)];
        let blocks = [0, 1, 1, 1, 2];
        assert_eq!((near(&blocks, &ranges, 0), near(&blocks, &ranges, 3), near(&blocks, &ranges, 4)), (1, 2, 2));
        assert_eq!(source_order(&blocks, &ranges, &[None; 5]), vec![2, 3, 4, 0, 1]);
        let within = vec![(None, None), at(30), at(10)];
        assert_eq!(near(&[0, 1, 0], &within, 0), 2);
    }

    #[test]
    fn a_changed_type_of_a_local_explains_the_elements_that_read_it() {
        // Design 45: `let x: u32 = a.into();` → `let x: u16 = a.into();`. The local `_2` has no
        // partner (its type differs). The call has the same key after the type of `_2` is
        // replaced (test 2); the cast reads `_2` (test 1). Each takes the sources of `_2`: the
        // declaration and the writer. The element that reads the cast is the same.
        let call = |ty: &str| format!("_2 = const fn(u8) -> {ty} {{<u8 as std::convert::Into<{ty}>>::into}}(_1) -> [return: bb1, unwind continue]");
        let body = |ty: &str| typed_body("fn _(_1: u8) -> u64", &[(0, "u64"), (2, ty), (3, "u64")], &[&[&call(ty)], &["_3 = _2 as u64 (IntToInt)", "_0 = Add(_3, const u64 = 1)", "return"]]);
        let declarations = vec![vec![0], vec![1], vec![2, 0, 40, 41, 0, 43, 46], vec![3, 0, 60, 70]];
        let (old, new) = (body("u32"), body("u16"));
        let found = super::diff((&old, &record_with(&[50, 60, 80, 90], declarations.clone())), (&new, &record_with(&[50, 60, 80, 90], declarations))).unwrap();
        assert_eq!((found.old.len(), found.new.len()), (2, 2), "{found:?}");
        let declaration = Source::Declaration(0, Place { file: "src/lib.rs".into(), start: 43, end: 46 });
        for element in found.old.iter().chain(&found.new) {
            assert!(element.sources.iter().flat_map(|group| &group.sources).any(|source| *source == declaration), "{element:?}");
            assert!(element.sources.iter().flat_map(|group| &group.sources).any(|source| matches!(source, Source::Element(1, Some(place), _) if place.start == 50)), "{element:?}");
        }
    }

    #[test]
    fn a_read_of_a_different_value_is_no_dependence() {
        // Design 45: `t - a` → `t - k` with a new local `k`. The local of the old side has its
        // partner: the element reads a different value, and no changed local explains it.
        let old = typed_body("fn _(_1: u32) -> u32", &[(0, "u32"), (2, "u32")], &[&["_2 = Add(_1, const u32 = 1)", "_0 = Sub(_2, _1)", "return"]]);
        let new = typed_body("fn _(_1: u32) -> u32", &[(0, "u32"), (2, "u32"), (3, "u32")], &[&["_2 = Add(_1, const u32 = 1)", "_3 = Add(_1, const u32 = 7)", "_0 = Sub(_2, _3)", "return"]]);
        let found = super::diff((&old, &record_at(&[0, 10, 20])), (&new, &record_at(&[0, 5, 10, 20]))).unwrap();
        assert!(found.old.iter().chain(&found.new).all(|element| element.sources.is_empty()), "{found:?}");
        assert!(!found.new.is_empty());
    }

    #[test]
    fn a_temporary_for_a_changed_reader_and_a_cleanup_block_are_sources() {
        // Design 45, test 3: `x.get()` with `x: W` → `x: &W`. The old auto-ref `_4 = &_2`
        // has no counterpart; it writes a changed temporary that the changed call reads.
        // `_2` has a type annotation: its group does not follow its reader.
        let declarations = vec![vec![0], vec![1], vec![2, 0, 1, 2, 0, 3, 4], vec![3, 0, 30, 37], vec![3, 0, 30, 31]];
        let old = typed_body("fn _(_1: u32) -> u32", &[(0, "u32"), (2, "W"), (3, "u32"), (4, "&W")], &[&["_2 = adt W fields {W.0: _1}", "_4 = &shared _2", "_3 = const fn(&W) -> u32 {W::get}(_4) -> [return: bb1, unwind continue]"], &["_0 = _3", "return"]]);
        let new = typed_body("fn _(_1: u32) -> u32", &[(0, "u32"), (2, "&W"), (3, "u32")], &[&["_2 = make(_1) -> [return: bb1, unwind continue]"], &["_3 = const fn(&W) -> u32 {W::get}(_2) -> [return: bb2, unwind continue]"], &["_0 = _3", "return"]]);
        let found = super::diff((&old, &record_with(&[0, 30, 32, 40, 45], declarations.clone())), (&new, &record_with(&[0, 32, 40, 45], declarations))).unwrap();
        let at = |list: &[Element], start: usize| list.iter().find(|element| element.tokens.as_ref().is_some_and(|place| place.start == start)).cloned().unwrap_or_else(|| panic!("{start}: {found:?}"));
        // The temporary `_4` pairs with the new `_2` (the same type): the call is the same, and
        // the borrow takes the sources of the borrowed local `_2` of the old side.
        let borrow = at(&found.old, 30);
        assert_eq!(borrow.sources.len(), 1, "{borrow:?}");
        assert!(!borrow.sources[0].follows && borrow.sources[0].sources.contains(&Source::Declaration(0, Place { file: "src/lib.rs".into(), start: 1, end: 2 })), "{borrow:?}");
        assert!(borrow.sources[0].sources.contains(&Source::Element(0, Some(Place { file: "src/lib.rs".into(), start: 0, end: 5 }), None)), "{borrow:?}");
        // An unwind edge of one side only goes to a cleanup block whose first element differs.
        let old = blocks(&[&["_2 = Add(_1, const u32 = 1)", "assert(!_2, \"x\") -> [success: bb1, unwind: bb2]"], &["_0 = _2", "return"], &["drop(_4) -> [return: bb3, unwind terminate]"], &["resume"]]);
        let new = blocks(&[&["_2 = Add(_1, const u32 = 1)", "assert(!_2, \"x\") -> [success: bb1, unwind continue]"], &["_0 = _2", "return"]]);
        let found = super::diff((&old, &record_at(&[0, 10, 20, 30, 40, 50])), (&new, &record_at(&[0, 10, 20, 30]))).unwrap();
        let check = at(&found.old, 10);
        assert_eq!(check.sources, vec![Group { follows: false, sources: vec![Source::Element(0, Some(Place { file: "src/lib.rs".into(), start: 40, end: 45 }), None)] }], "{found:?}");
    }

    #[test]
    fn the_shape_of_an_element_replaces_the_types_of_its_changed_locals() {
        assert_eq!(shape("_ = <u8 as Into<u32>>::into(copy _, const 3_u32)", &["u32"]), "_ = <u8 as Into<?T>>::into(copy _, const 3_?T)");
        assert_eq!(shape("_ = <Wrapping<u32> as PartialEq>::eq(_, _) -> [return: bb, unwind: bb]", &["&Wrapping<u32>"]), "_ = <?T as PartialEq>::eq(_, _) -> [return: bb, unwind ?]");
        // A word that only contains the type is not the type.
        assert_eq!(shape("_ = u320(_)", &["u32"]), "_ = u320(_)");
        // The form of a type in a path (fix round 1).
        assert_eq!(shape("_ = std::vec::Vec::<u8>::new() -> [return: bb, unwind continue]", &["std::vec::Vec<u8>"]), "_ = ?T::new() -> [return: bb, unwind ?]");
        assert_eq!(projected("_0 = (_9.std::num::Wrapping.0: u32) as u64 (IntToInt)", &BTreeSet::from([9])), "_0 = (_9.std::num::Wrapping.0: ?T) as u64 (IntToInt)");
        assert_eq!(written("(_3.0: u32) = Add(_1, _2)"), Some(3));
        assert_eq!(written("drop(_3) -> [return: bb1, unwind continue]"), None);
    }

    #[test]
    fn the_record_gives_the_kind_and_the_declaration_ranges_of_each_local() {
        // Design 45, part 1: `[kind, file, start, end]`, then the type annotation.
        let record = RecordElements { f: vec!["src/lib.rs".into()], l: vec![vec![0], vec![2, 0, 4, 5, 0, 7, 10], vec![3, 0, 12, 20], vec![3, 9, 1, 2]], ..Default::default() };
        let found = record.declarations();
        assert_eq!(found[0], (0, Vec::new()));
        assert_eq!(found[1].1.iter().map(|place| (place.start, place.end)).collect::<Vec<_>>(), vec![(4, 5), (7, 10)]);
        assert_eq!((found[2].0, found[2].1.len()), (LOCAL_TEMPORARY, 1));
        // A file index that is not valid gives no range.
        assert!(found[3].1.is_empty());
    }

    #[test]
    fn the_key_has_no_numbers_of_locals_and_blocks() {
        let (key, numbers) = key("_12 = foo::bar_2(move _3) -> [return: bb4, unwind continue]");
        assert_eq!(key, "_ = foo::bar_2(move _) -> [return: bb, unwind continue]");
        assert_eq!(numbers.len(), 3);
    }
}



