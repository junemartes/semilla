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
//!    first; an element with no place goes with the next element that has one), not in the
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
}

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
        self.old.sort();
        self.old.dedup();
        self.new.sort();
        self.new.dedup();
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

/// The elements in the order of the source: by the start of their place, the longer place
/// first. An element with no place goes with the next element that has a place. A folded copy
/// is not in the list.
fn source_order(ranges: &[(Option<Place>, Option<Place>)], folded: &[Option<usize>]) -> Vec<usize> {
    // An element with no place takes the place of the next element with a place (the driver
    // wraps a value as opaque just before its use), else of the element before it.
    let mut next = (String::new(), usize::MAX, 0);
    let mut keys = vec![next.clone(); ranges.len()];
    for (index, range) in ranges.iter().enumerate().rev() {
        if let Some(place) = place_of(range) {
            next = (place.file.clone(), place.start, usize::MAX - place.end);
        }
        keys[index] = next.clone();
    }
    let mut keyed = Vec::new();
    for (index, key) in keys.into_iter().enumerate() {
        if folded[index].is_none() {
            keyed.push((key, index));
        }
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
    let old_order = source_order(&old_ranges, &old_folded);
    let new_order = source_order(&new_ranges, &new_folded);
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
    for (a, b) in pairs {
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
    let element = |parsed: &Parsed, ranges: &[(Option<Place>, Option<Place>)], symbols: &[Vec<Symbol>], index: usize| {
        let (targets, exit) = branch(parsed.elements[index].0);
        // An element with no range (a value that the driver wraps as opaque just before its
        // use) gets the range of the next element of its block with a range, else of the next
        // element with a range, else of the element before it.
        let block = parsed.elements[index].1;
        let in_block = |at: &usize| parsed.elements[*at].1 == block && (ranges[*at].0.is_some() || ranges[*at].1.is_some());
        let ranged = |at: &usize| ranges[*at].0.is_some() || ranges[*at].1.is_some();
        let near = (!ranged(&index)).then(|| (index + 1..ranges.len()).find(in_block).or_else(|| (index + 1..ranges.len()).find(ranged)).or_else(|| (0..index).rev().find(ranged))).flatten();
        let (tokens, call) = ranges[near.unwrap_or(index)].clone();
        let text = parsed.elements[index].0;
        let jump = text.starts_with("goto -> ") || matches!(text, "return" | "resume" | "unreachable");
        let glue = jump || text.starts_with("drop(");
        let symbols = symbols.get(index).cloned().unwrap_or_default();
        Element { tokens, call, targets, exit, glue, jump, symbols }
    };
    let mut out = ElementDiff {
        old: (0..old.elements.len()).filter(|i| !same[*i]).map(|i| element(&old, &old_ranges, &symbols[0], i)).collect(),
        new: (0..new.elements.len()).filter(|i| !same_new[*i]).map(|i| element(&new, &new_ranges, &symbols[1], i)).collect(),
        other: old.other != new.other || old.args != new.args,
        no_symbols,
        evaluated: old_record.c || new_record.c,
        own: [&old_record, &new_record].iter().enumerate().filter_map(|(side, record)| Some((side, record_symbol(&record.f, record.s.get(record.o? as usize)?)?))).collect(),
        pairs: partner.iter().enumerate().filter_map(|(a, b)| Some((old_ranges[a].0.clone()?, new_ranges[(*b)?].0.clone()?))).collect(),
    };
    out.old.sort();
    out.old.dedup();
    out.new.sort();
    out.new.dedup();
    out.pairs.sort();
    out.pairs.dedup();
    Some(out)
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

    #[test]
    fn the_key_has_no_numbers_of_locals_and_blocks() {
        let (key, numbers) = key("_12 = foo::bar_2(move _3) -> [return: bb4, unwind continue]");
        assert_eq!(key, "_ = foo::bar_2(move _) -> [return: bb, unwind continue]");
        assert_eq!(numbers.len(), 3);
    }
}



