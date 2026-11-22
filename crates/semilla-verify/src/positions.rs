//! The position of each fact in the line diff of the step, and the cover check (design 55.2
//! and 55.4). A position comes from the syntax tree: the range of the changed node, or the
//! lines of the changed own tokens. No part of this module searches a source text.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{Category, Compare, Fact, Position};
use semilla_syntax::cover::{self, LineToken, ModuleItem, Shape, TokenClass};
use semilla_syntax::tree::Pos;
use semilla_syntax::{Edit, EditKind, Item, ItemKind, Node, Refactor, Workspace};

use crate::{StepInput, StepOutcome};

/// The tokens that only close, open, or divide a part of the tree.
const DELIMITERS: [&str; 8] = ["{", "}", "(", ")", "[", "]", ",", ";"];

const OLD: usize = 0;
const NEW: usize = 1;

/// The limit of the edit distance of one diff. A diff above the limit has no equal line (or
/// token) between its equal start and its equal end: each line there is a changed line.
const DISTANCE_LIMIT: usize = 2000;

/// The pairs of equal elements of two lists, in order (the diff of Myers). `eq(i, j)` compares
/// element `i` of the first list with element `j` of the second list.
pub(crate) fn matches(n: usize, m: usize, eq: &dyn Fn(usize, usize) -> bool) -> Vec<(usize, usize)> {
    let mut start = 0;
    while start < n && start < m && eq(start, start) {
        start += 1;
    }
    let mut end = 0;
    while end < n - start && end < m - start && eq(n - 1 - end, m - 1 - end) {
        end += 1;
    }
    let mut out: Vec<(usize, usize)> = (0..start).map(|index| (index, index)).collect();
    out.extend(middle(n - start - end, m - start - end, &|i, j| eq(start + i, start + j)).into_iter().map(|(i, j)| (start + i, start + j)));
    out.extend((0..end).map(|index| (n - end + index, m - end + index)));
    out
}

fn middle(n: usize, m: usize, eq: &dyn Fn(usize, usize) -> bool) -> Vec<(usize, usize)> {
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let max = (n + m).min(DISTANCE_LIMIT) as isize;
    let offset = max + 1;
    // For each diagonal `k` (x - y): the last x of the path with the current distance.
    let mut ends = vec![0isize; 2 * offset as usize + 1];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let down = |row: &dyn Fn(isize) -> isize, k: isize, distance: isize| k == -distance || (k != distance && row(k - 1) < row(k + 1));
    for distance in 0..=max {
        // The row before this distance: the diagonals from -(distance + 1) to distance + 1.
        trace.push(ends[(offset - distance - 1) as usize..=(offset + distance + 1) as usize].to_vec());
        let mut k = -distance;
        while k <= distance {
            let get = |k: isize| ends[(offset + k) as usize];
            let mut x = if down(&get, k, distance) { get(k + 1) } else { get(k - 1) + 1 };
            let mut y = x - k;
            while (x as usize) < n && (y as usize) < m && eq(x as usize, y as usize) {
                x += 1;
                y += 1;
            }
            ends[(offset + k) as usize] = x;
            if x as usize >= n && y as usize >= m {
                // The walk back: each run on a diagonal gives pairs of equal elements.
                let mut out = Vec::new();
                let (mut x, mut y) = (n as isize, m as isize);
                for back in (0..=distance).rev() {
                    let k = x - y;
                    if back == 0 {
                        while x > 0 {
                            x -= 1;
                            y -= 1;
                            out.push((x as usize, y as usize));
                        }
                        break;
                    }
                    let row = &trace[back as usize];
                    let get = |k: isize| row[(k + back + 1) as usize];
                    // The path comes from the diagonal `before` with one step, then a run.
                    let before = if down(&get, k, back) { k + 1 } else { k - 1 };
                    let from_x = get(before);
                    let run_start = if before > k { from_x } else { from_x + 1 };
                    while x > run_start {
                        x -= 1;
                        y -= 1;
                        out.push((x as usize, y as usize));
                    }
                    (x, y) = (from_x, from_x - before);
                }
                out.reverse();
                return out;
            }
            k += 2;
        }
    }
    Vec::new()
}

/// One side of a changed file.
#[derive(Default)]
struct Side {
    /// Each line without its line ending.
    lines: Vec<String>,
    /// The line ending of each line: 0 (none, the end of the file), 1 (LF), 2 (CRLF).
    endings: Vec<u8>,
    /// The tokens, for a file that Semilla parses.
    shape: Option<Shape>,
    /// For each line (from 0): the indexes of its tokens with a meaning.
    tokens_at: Vec<Vec<usize>>,
}

impl Side {
    fn new(text: Option<&String>, parsed: bool, edition: Option<semilla_syntax::rs::Edition>) -> Side {
        let Some(text) = text else { return Side::default() };
        let mut side = Side::default();
        for line in text.split_inclusive('\n') {
            let (body, ending) = match line.strip_suffix("\r\n") {
                Some(body) => (body, 2),
                None => match line.strip_suffix('\n') {
                    Some(body) => (body, 1),
                    None => (line, 0),
                },
            };
            side.lines.push(body.to_string());
            side.endings.push(ending);
        }
        side.tokens_at = vec![Vec::new(); side.lines.len()];
        if parsed {
            let shape = cover::shape(text, edition.unwrap_or(semilla_syntax::rs::Edition::Edition2021));
            for (index, token) in shape.tokens.iter().enumerate() {
                if token.void {
                    continue;
                }
                for line in token.start.0..=token.end_line {
                    if let Some(list) = side.tokens_at.get_mut(line - 1) {
                        list.push(index);
                    }
                }
            }
            side.shape = Some(shape);
        }
        side
    }

    fn tokens(&self) -> &[LineToken] {
        self.shape.as_ref().map_or(&[], |shape| &shape.tokens)
    }

    /// The lines (from 1) of the tokens of a class between two places, without the tokens in
    /// a `skip` range.
    fn token_lines(&self, from: Pos, to: Pos, skip: &[(Pos, Pos)], accept: &dyn Fn(&LineToken) -> bool) -> BTreeSet<usize> {
        let tokens = self.tokens();
        let first = tokens.partition_point(|token| token.start < from);
        let mut out = BTreeSet::new();
        for token in tokens[first..].iter().take_while(|token| token.start < to) {
            if token.void || !accept(token) || skip.iter().any(|(start, end)| *start <= token.start && token.start < *end) {
                continue;
            }
            out.extend(token.start.0..=token.end_line);
        }
        out
    }
}

/// The line diff of one file of the step.
struct FileDiff {
    sides: [Side; 2],
    /// The pairs of equal lines (from 0), in order.
    pairs: Vec<(usize, usize)>,
    /// True for a Rust file that Semilla parses on each side that has it.
    parsed: bool,
}

/// One hunk of a line diff: the changed lines (from 1, the end is not in the range) of each
/// side.
#[derive(Clone, Copy, Debug)]
struct Hunk {
    range: [(usize, usize); 2],
}

impl FileDiff {
    fn hunks(&self) -> Vec<Hunk> {
        let mut out = Vec::new();
        let (mut old, mut new) = (0, 0);
        let end = (self.sides[OLD].lines.len(), self.sides[NEW].lines.len());
        for &(a, b) in self.pairs.iter().chain(std::iter::once(&end)) {
            if a > old || b > new {
                out.push(Hunk { range: [(old + 1, a + 1), (new + 1, b + 1)] });
            }
            (old, new) = (a + 1, b + 1);
        }
        out
    }

    /// The first rule of [`FileDiff::slide`], for each hunk: the hunk is between the pairs
    /// `gap - 1` and `gap`, from `start` to `after`. True if a pair of equal lines changed.
    fn pair_again(&mut self, gap: usize, start: (usize, usize), after: (usize, usize), good: &[BTreeSet<usize>; 2]) -> bool {
        for side in [NEW, OLD] {
            let pick = |pair: (usize, usize)| if side == OLD { pair.0 } else { pair.1 };
            let lines = &self.sides[side].lines;
            let is_good = |line: usize| good[side].contains(&(line + 1)) || self.sides[side].tokens_at.get(line).is_some_and(|tokens| tokens.is_empty());
            let free = |line: usize, pair: usize| lines[line] == lines[pair] && !is_good(line) && is_good(pair);
            // The equal line after the hunk: the last line of the hunk that can replace it.
            if gap < self.pairs.len() {
                let pair = pick(self.pairs[gap]);
                if let Some(line) = (pick(start)..pick(after)).rev().find(|line| free(*line, pair)) {
                    if side == OLD { self.pairs[gap].0 = line } else { self.pairs[gap].1 = line }
                    return true;
                }
            }
            // The equal line before the hunk: the first line of the hunk that can replace it.
            if gap > 0 {
                let pair = pick(self.pairs[gap - 1]);
                if let Some(line) = (pick(start)..pick(after)).find(|line| free(*line, pair)) {
                    if side == OLD { self.pairs[gap - 1].0 = line } else { self.pairs[gap - 1].1 = line }
                    return true;
                }
            }
        }
        false
    }

    /// Moves each hunk with lines on one side only to the place with the most `good` lines
    /// (design 55.2). A block of added lines can slide if the line before it is equal to its
    /// last line, or the line after it is equal to its first line: each place is a correct diff.
    fn slide(&mut self, good: &[BTreeSet<usize>; 2], exact: &[BTreeSet<usize>; 2]) {
        let mut gap = 0;
        while gap <= self.pairs.len() {
            let before = if gap == 0 { None } else { Some(self.pairs[gap - 1]) };
            let after = self.pairs.get(gap).copied().unwrap_or((self.sides[OLD].lines.len(), self.sides[NEW].lines.len()));
            let start = before.map_or((0, 0), |(a, b)| (a + 1, b + 1));
            let counts = [after.0 - start.0, after.1 - start.1];
            if counts == [0, 0] {
                gap += 1;
                continue;
            }
            // The equal line after the hunk (or before it) can be equal to a line of the hunk.
            // Then the pair of equal lines can use that line of the hunk: the count of
            // changed lines is the same, and each of the two places is a correct diff. Semilla
            // takes the place with one more line of the changed tokens of an edit (`exact`).
            // Each change adds one such line: the loop ends. A block of added lines with such
            // a line becomes two blocks.
            if self.pair_again(gap, start, after, exact) {
                continue;
            }
            let side = match counts {
                [0, _] => NEW,
                [_, 0] => OLD,
                _ => {
                    gap += 1;
                    continue;
                }
            };
            let pick = |pair: (usize, usize)| if side == OLD { pair.0 } else { pair.1 };
            let other = |pair: (usize, usize)| if side == OLD { pair.1 } else { pair.0 };
            let (first, count) = (pick(start), counts[side]);
            let lines = &self.sides[side].lines;
            let is_good = |line: usize| good[side].contains(&(line + 1)) || self.sides[side].tokens_at.get(line).is_some_and(|tokens| tokens.is_empty());
            // The count of the lines that the block can go up, and that it can go down.
            let mut up = 0;
            while gap > up && pick(self.pairs[gap - 1 - up]) + up + 1 == first && other(self.pairs[gap - 1 - up]) + up + 1 == other(start) && lines[first - 1 - up] == lines[first + count - 1 - up] {
                up += 1;
            }
            let mut down = 0;
            while gap + down < self.pairs.len() && pick(self.pairs[gap + down]) == first + count + down && other(self.pairs[gap + down]) == other(start) + down && lines[first + down] == lines[first + count + down] {
                down += 1;
            }
            let score = |shift: isize| (0..count).filter(|index| is_good((first as isize + shift) as usize + index)).count();
            let mut best = (score(0), 0isize);
            for shift in (1..=up as isize).map(|step| -step).chain(1..=down as isize) {
                let value = score(shift);
                if value > best.0 {
                    best = (value, shift);
                }
            }
            let shift = best.1;
            let set = |pair: &mut (usize, usize), value: usize| if side == OLD { pair.0 = value } else { pair.1 = value };
            if shift < 0 {
                // The equal lines before the block go after it.
                let steps = (-shift) as usize;
                for index in 0..steps {
                    let value = first + count - steps + index;
                    set(&mut self.pairs[gap - steps + index], value);
                }
                gap -= steps;
            } else if shift > 0 {
                let steps = shift as usize;
                for index in 0..steps {
                    set(&mut self.pairs[gap + index], first + index);
                }
                gap += steps;
            }
            gap += 1;
        }
    }
}

/// The lines of one edit on one side of one file.
struct Scope {
    file: String,
    side: usize,
    /// The lines of the changed tokens: the node, the own tokens, or the tokens of a class.
    lines: BTreeSet<usize>,
    /// The lines to use if no line of `lines` is a changed line.
    wide: BTreeSet<usize>,
}

/// The items of each file of one tree.
struct ItemIndex<'a> {
    by_file: BTreeMap<&'a str, Vec<&'a Item>>,
}

impl<'a> ItemIndex<'a> {
    fn new(workspace: &'a Workspace) -> ItemIndex<'a> {
        let mut by_file: BTreeMap<&str, Vec<&Item>> = BTreeMap::new();
        for item in workspace.items.values() {
            by_file.entry(item.file.as_str()).or_default().push(item);
        }
        ItemIndex { by_file }
    }

    /// The ranges of the items in the lines of `item`.
    fn nested(&self, item: &Item) -> Vec<(Pos, Pos)> {
        let (start, end) = item_range(item);
        self.by_file
            .get(item.file.as_str())
            .into_iter()
            .flatten()
            .filter(|other| other.path != item.path)
            .map(|other| item_range(other))
            .filter(|(other_start, other_end)| start <= *other_start && *other_end <= end && (start, end) != (*other_start, *other_end))
            .collect()
    }
}

fn item_range(item: &Item) -> (Pos, Pos) {
    ((item.line_start, item.col_start), (item.line_end, item.col_end))
}

fn span(from: usize, to: usize) -> BTreeSet<usize> {
    (from..=to).collect()
}

/// The lines of an item-level edit: the tokens of the class of the edit in the head of the
/// item (the item without its body and without the items in it).
fn item_scope(item: &Item, kind: EditKind, side: usize, diffs: &BTreeMap<String, FileDiff>, index: &ItemIndex) -> Option<Scope> {
    let diff = diffs.get(&item.file)?;
    let source = &diff.sides[side];
    let (start, end) = item_range(item);
    let full = span(item.line_start, item.line_end);
    if matches!(kind, EditKind::ItemAdded | EditKind::ItemRemoved) || source.shape.is_none() {
        return Some(Scope { file: item.file.clone(), side, lines: full.clone(), wide: full });
    }
    let mut skip = index.nested(item);
    let body = item.body.as_ref().filter(|body| body.start.0 > 0).map(|body| (body.start, body.end));
    if matches!(kind, EditKind::Inserted | EditKind::Deleted | EditKind::Moved | EditKind::Modified | EditKind::Replaced) {
        // An edit of the full body, or of the value of an item with no body node.
        let lines = match body {
            Some((from, to)) => span(from.0, to.0),
            None => source.token_lines(start, end, &skip, &|token| token.class == TokenClass::Code),
        };
        return Some(Scope { file: item.file.clone(), side, lines, wide: full });
    }
    if kind == EditKind::Comment {
        // The plain comments of the lines of the item are in its `docs` (design 39.1, rule 4).
        let lines = source.token_lines((item.line_start, 0), (item.line_end + 1, 0), &[], &|token| token.class == TokenClass::Comment);
        return Some(Scope { file: item.file.clone(), side, lines, wide: full });
    }
    skip.extend(body);
    let class = match kind {
        EditKind::Visibility => TokenClass::Visibility,
        EditKind::Attrs => TokenClass::Attribute,
        EditKind::Docs => TokenClass::Doc,
        _ => TokenClass::Code,
    };
    let lines = source.token_lines(start, end, &skip, &|token| token.class == class);
    let wide = source.token_lines(start, end, &skip, &|_| true);
    Some(Scope { file: item.file.clone(), side, lines, wide })
}

/// The lines of a node edit: the range of the node, or the lines of its own tokens for an
/// edit of the own tokens (design 52.1).
fn node_scope(item: &Item, node: &Node, edit: &Edit, side: usize, diffs: &BTreeMap<String, FileDiff>, index: &ItemIndex) -> Option<Scope> {
    if node.start.0 == 0 {
        // A node that is not made from the tokens of one syntax node (a part of a signature).
        return item_scope(item, EditKind::Signature, side, diffs, index);
    }
    let diff = diffs.get(&item.file)?;
    let wide = span(node.start.0, node.end.0);
    let mut lines = if edit.kind == EditKind::Modified && diff.sides[side].shape.is_some() {
        let children: Vec<(Pos, Pos)> = node.children.iter().map(|child| (child.start, child.end)).collect();
        diff.sides[side].token_lines(node.start, node.end, &children, &|_| true)
    } else {
        wide.clone()
    };
    // The statements that a move passes over, and the region of an extract or an inline.
    match &edit.refactor {
        Some(Refactor::Moved { jumped: regions }) | Some(Refactor::Extract { region: regions, .. }) | Some(Refactor::Inline { region: regions, .. }) => {
            for (from, to) in &regions[side] {
                if from.0 > 0 {
                    lines.extend(from.0..=to.0);
                }
            }
        }
        None => {}
    }
    Some(Scope { file: item.file.clone(), side, lines, wide })
}

/// The lines of an edit on each side.
fn edit_scopes(edit: &Edit, input: &StepInput, diffs: &BTreeMap<String, FileDiff>, indexes: &[ItemIndex; 2]) -> Vec<Scope> {
    // The lines of an edit of a file: an inner attribute, an inner doc comment, or the full
    // file. `files` gives the file of each side.
    let file_scopes = |files: [Option<&String>; 2]| -> Vec<Scope> {
        let mut out = Vec::new();
        for side in [OLD, NEW] {
            let Some(file) = files[side] else { continue };
            let Some(diff) = diffs.get(file) else { continue };
            let source = &diff.sides[side];
            let full = span(1, source.lines.len());
            let class = match edit.kind {
                EditKind::Docs => Some(TokenClass::Doc),
                EditKind::Attrs => Some(TokenClass::Attribute),
                _ => None,
            };
            out.push(match class {
                Some(class) if source.shape.is_some() => {
                    let end = (source.lines.len() + 1, 0);
                    Scope { file: file.clone(), side, lines: source.token_lines((0, 0), end, &[], &|token| token.top && token.class == class), wide: source.token_lines((0, 0), end, &[], &|token| token.top) }
                }
                _ => Scope { file: file.clone(), side, lines: full.clone(), wide: full },
            });
        }
        out
    };
    // The inner attributes and the inner doc lines of a module file: the edit names the
    // module, and its lines are in the file of the module. The `mod` item of the module is in
    // a different file. An edit of the outer attributes of the `mod` item has the file of the
    // item.
    if matches!(edit.kind, EditKind::Docs | EditKind::Attrs) {
        let inner: [Option<&String>; 2] = [(input.before, &edit.before), (input.after, &edit.after)].map(|(workspace, anchor)| anchor.as_ref().filter(|anchor| anchor.path.is_empty()).and_then(|anchor| workspace.inner_attrs.get(&anchor.item)).map(|(file, _)| file));
        if edit.file.as_ref().is_some_and(|file| inner.iter().flatten().any(|inner| *inner == file)) {
            return file_scopes(inner.map(|file| file.or(edit.file.as_ref())));
        }
    }
    let mut out = Vec::new();
    for (side, (anchor, workspace)) in [(&edit.before, input.before), (&edit.after, input.after)].into_iter().enumerate() {
        let Some(anchor) = anchor else { continue };
        let scope = match semilla_syntax::resolve(workspace, anchor) {
            Some(semilla_syntax::Resolved::Item(item)) => item_scope(item, edit.kind, side, diffs, &indexes[side]),
            Some(semilla_syntax::Resolved::Node(item, node)) => node_scope(item, node, edit, side, diffs, &indexes[side]),
            // The node is not in the tree (a part of a signature with no node): the item.
            None => workspace.items.get(&anchor.item).and_then(|item| item_scope(item, if anchor.path.is_empty() { edit.kind } else { EditKind::Signature }, side, diffs, &indexes[side])),
        };
        out.extend(scope);
    }
    if out.is_empty() {
        out = file_scopes([edit.file.as_ref(), edit.file.as_ref()]);
    }
    out
}

/// The line diff of each file with a different text in the two trees.
fn file_diffs(input: &StepInput) -> BTreeMap<String, FileDiff> {
    let text = |workspace: &'_ Workspace, file: &str| -> Option<String> { workspace.rust_files.get(file).or_else(|| workspace.other_files.get(file)).cloned() };
    let mut files: BTreeSet<&String> = BTreeSet::new();
    for workspace in [input.before, input.after] {
        files.extend(workspace.rust_files.keys());
        files.extend(workspace.other_files.keys());
    }
    let mut out = BTreeMap::new();
    for file in files {
        let (old, new) = (text(input.before, file), text(input.after, file));
        if old == new {
            continue;
        }
        let parsed = [(&old, input.before), (&new, input.after)].iter().all(|(text, workspace)| text.is_none() || workspace.parsed_files.contains(file));
        let sides = [
            Side::new(old.as_ref(), parsed, input.before.editions.get(file).copied()),
            Side::new(new.as_ref(), parsed, input.after.editions.get(file).copied()),
        ];
        let pairs = {
            let (a, b) = (&sides[OLD].lines, &sides[NEW].lines);
            matches(a.len(), b.len(), &|i, j| a[i] == b[j])
        };
        out.insert(file.clone(), FileDiff { sides, pairs, parsed });
    }
    out
}

/// What the cover check knows about a changed line with no fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    /// The tokens of the line are equal on the two sides, or the line has no token.
    Equal,
    /// Each changed token of the line is a plain comment.
    Comment,
    /// A token of the line changes and no fact has the line.
    Token,
    /// The tokens of the line are equal, and its hunk has a changed token with no fact. A
    /// later rule can give that token a fact (a moved item, the same items in a new order).
    Pending,
}

/// The lines of the facts: (file, side, line) to the indexes of the facts.
type Claims = BTreeMap<(String, usize, usize), Vec<usize>>;

/// True if the pair of paths (`old`, `new`) of the map follows the pair (`from`, `to`) of a
/// fact: the same pair, or a path with the same change of the module and of the name.
fn follows(old: &str, new: &str, from: &str, to: &str) -> bool {
    if old == from {
        return new == to;
    }
    let (from_module, from_name) = from.rsplit_once("::").unwrap_or(("", from));
    let (to_module, to_name) = to.rsplit_once("::").unwrap_or(("", to));
    let Some(rest) = old.strip_prefix(from_module).filter(|_| !from_module.is_empty()) else { return false };
    format!("{to_module}{}", rest.replace(from_name, to_name)) == new
}

fn claim(claims: &mut Claims, file: &str, side: usize, line: usize, fact: usize) {
    let list = claims.entry((file.to_string(), side, line)).or_default();
    if !list.contains(&fact) {
        list.push(fact);
    }
}

fn position_list(file: &str, lines: &[BTreeSet<usize>; 2]) -> Vec<Position> {
    let mut out = Vec::new();
    for side in [NEW, OLD] {
        out.extend(lines[side].iter().map(|line| Position { file: file.to_string(), old: side == OLD, start: *line, end: *line }));
    }
    Position::normal(out)
}

fn count(value: usize, noun: &str) -> String {
    format!("{value} {noun}{}", if value == 1 { "" } else { "s" })
}

/// Gives each fact its position, adds the facts of the cover check (layout, line endings, a
/// comment that is in no item), and writes the changed lines with no fact into the report.
pub(crate) fn assign(input: &StepInput, outcome: &mut StepOutcome) {
    let StepOutcome { report, edits, fact_of, renames, .. } = outcome;
    let mut diffs = file_diffs(input);
    if diffs.is_empty() {
        return;
    }
    let indexes = [ItemIndex::new(input.before), ItemIndex::new(input.after)];
    let fact_count = report.facts.len();

    // 1. The lines of each edit that has a fact.
    let mut scopes: Vec<(usize, Vec<Scope>)> = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let Some(fact) = fact_of.get(index).copied().flatten().filter(|fact| *fact < fact_count) else { continue };
        scopes.push((fact, edit_scopes(edit, input, &diffs, &indexes)));
    }

    // 2. A block of added or removed lines goes to the place that agrees with the tree.
    // `good` has each line of an edit. `exact` has only the lines of the changed tokens of an
    // edit: a changed line in `exact` gets a fact.
    let mut good: BTreeMap<&str, [BTreeSet<usize>; 2]> = BTreeMap::new();
    let mut exact: BTreeMap<&str, [BTreeSet<usize>; 2]> = BTreeMap::new();
    for scope in scopes.iter().flat_map(|(_, list)| list) {
        good.entry(scope.file.as_str()).or_default()[scope.side].extend(scope.lines.iter().chain(&scope.wide).copied());
        exact.entry(scope.file.as_str()).or_default()[scope.side].extend(scope.lines.iter().copied());
    }
    // The items of each move fact: the item of the fact, and each item that the map of the
    // paths moves with it (an `impl` of a moved type). Their lines are lines of the move.
    let mut moves: Vec<(usize, &Item, &Item)> = Vec::new();
    for (index, fact) in report.facts.iter().enumerate().filter(|(_, fact)| matches!(fact.category, Category::Move | Category::Rename)) {
        let Some(at) = fact.at.as_ref().filter(|at| at.path.is_empty()) else { continue };
        let Some(to) = renames.items.get(&at.item) else { continue };
        for (old, new) in renames.items.iter().filter(|(old, new)| old != new && follows(old, new, &at.item, to)) {
            if let (Some(old_item), Some(new_item)) = (input.before.items.get(old), input.after.items.get(new)) {
                moves.push((index, old_item, new_item));
            }
        }
    }
    for (_, old_item, new_item) in &moves {
        for (side, item) in [(OLD, old_item), (NEW, new_item)] {
            good.entry(item.file.as_str()).or_default()[side].extend(item.line_start..=item.line_end);
        }
    }
    let none: [BTreeSet<usize>; 2] = Default::default();
    for (file, diff) in diffs.iter_mut() {
        if diff.parsed {
            diff.slide(good.get(file.as_str()).unwrap_or(&none), exact.get(file.as_str()).unwrap_or(&none));
        }
    }

    // 3. The changed lines of each file. A pair of equal lines with different line endings is
    // a pair of line-ending lines.
    let mut changed: BTreeMap<&str, [BTreeSet<usize>; 2]> = BTreeMap::new();
    let mut endings: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for (file, diff) in &diffs {
        let entry = changed.entry(file.as_str()).or_default();
        for hunk in diff.hunks() {
            for side in [OLD, NEW] {
                entry[side].extend(hunk.range[side].0..hunk.range[side].1);
            }
        }
        for &(old, new) in &diff.pairs {
            if diff.sides[OLD].endings[old] != diff.sides[NEW].endings[new] {
                endings.entry(file.as_str()).or_default().push((old, new));
            }
        }
    }
    report.changed_lines = changed.values().map(|sides| sides[OLD].len() + sides[NEW].len()).sum::<usize>() + endings.values().map(|pairs| 2 * pairs.len()).sum::<usize>();

    // 4. The position of an edit: the changed lines in its lines.
    let mut claims: Claims = BTreeMap::new();
    let mut lines_of: Vec<BTreeMap<String, [BTreeSet<usize>; 2]>> = vec![BTreeMap::new(); fact_count];
    let mut spare: Vec<Vec<&Scope>> = vec![Vec::new(); fact_count];
    for (fact, list) in &scopes {
        let pick = |wide: bool| -> Vec<(&Scope, Vec<usize>)> {
            list.iter()
                .map(|scope| {
                    let lines = if wide { &scope.wide } else { &scope.lines };
                    let hit = changed.get(scope.file.as_str()).map(|sides| lines.intersection(&sides[scope.side]).copied().collect()).unwrap_or_default();
                    (scope, hit)
                })
                .collect()
        };
        let mut found = pick(false);
        if found.iter().all(|(_, lines)| lines.is_empty()) {
            found = pick(true);
        }
        spare[*fact].extend(list.iter());
        for (scope, lines) in found {
            for line in lines {
                lines_of[*fact].entry(scope.file.clone()).or_default()[scope.side].insert(line);
                claim(&mut claims, &scope.file, scope.side, line, *fact);
            }
        }
    }
    // A fact with no changed line in its edits (a statement at a new place between equal
    // lines): the lines of the edits.
    for (fact, list) in spare.iter().enumerate() {
        if !lines_of[fact].is_empty() {
            continue;
        }
        for scope in list {
            for line in &scope.lines {
                lines_of[fact].entry(scope.file.clone()).or_default()[scope.side].insert(*line);
                claim(&mut claims, &scope.file, scope.side, *line, fact);
            }
        }
    }

    // 5. A fact of a file that Semilla does not parse has each changed line of the file.
    for (index, fact) in report.facts.iter().enumerate() {
        let Some(file) = fact.file.as_ref().filter(|_| lines_of[index].is_empty()) else { continue };
        let Some(sides) = changed.get(file.as_str()).filter(|_| diffs.get(file).is_some_and(|diff| !diff.parsed)) else { continue };
        for side in [OLD, NEW] {
            for line in &sides[side] {
                lines_of[index].entry(file.clone()).or_default()[side].insert(*line);
                claim(&mut claims, file, side, *line, index);
            }
        }
    }

    // 5b. A removed file (design 55.5): its inner attributes and its inner doc lines go with
    // it, and they have no fact of their own (the syntax diff, part 5.2 of batch 7b). The
    // facts of the removed items of the file show the removal: each such line joins the
    // nearest of these facts.
    for (module, (file, _)) in &input.before.inner_attrs {
        if input.after.inner_attrs.contains_key(module) || input.after.parsed_files.contains(file) {
            continue;
        }
        let Some(source) = diffs.get(file).map(|diff| &diff.sides[OLD]).filter(|source| source.shape.is_some()) else { continue };
        let removed: Vec<(usize, usize)> = claims
            .range((file.clone(), OLD, 0)..=(file.clone(), OLD, usize::MAX))
            .filter_map(|((_, _, line), facts)| facts.iter().find(|fact| report.facts[**fact].category == Category::Removed).map(|fact| (*line, *fact)))
            .collect();
        for line in source.token_lines((0, 0), (source.lines.len() + 1, 0), &[], &|token| token.top) {
            if claims.contains_key(&(file.clone(), OLD, line)) {
                continue;
            }
            if let Some((_, fact)) = removed.iter().map(|(other, fact)| (other.abs_diff(line), *fact)).min() {
                lines_of[fact].entry(file.clone()).or_default()[OLD].insert(line);
                claim(&mut claims, file, OLD, line, fact);
            }
        }
    }

    // 6. The names of each rename: (old word, new word) to the fact.
    let mut words: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (index, fact) in report.facts.iter().enumerate() {
        if !matches!(fact.category, Category::Rename | Category::Move) {
            continue;
        }
        let Some(at) = &fact.at else { continue };
        let Some(new) = renames.items.get(&at.item).filter(|_| at.path.is_empty()) else { continue };
        let (old_parts, new_parts): (Vec<&str>, Vec<&str>) = (at.item.split("::").collect(), new.split("::").collect());
        if let (Some(old_name), Some(new_name)) = (old_parts.last(), new_parts.last()) {
            if old_name != new_name {
                words.entry((old_name.to_string(), new_name.to_string())).or_insert(index);
            }
        }
        if old_parts.len() == new_parts.len() {
            for (old_part, new_part) in old_parts.iter().zip(&new_parts).filter(|(old_part, new_part)| old_part != new_part) {
                words.entry((old_part.to_string(), new_part.to_string())).or_insert(index);
            }
        }
    }
    let rename_fact = |scope: Option<&String>| -> Option<usize> {
        report.facts.iter().position(|fact| fact.category == Category::Rename && fact.at.as_ref().is_some_and(|at| scope.is_none_or(|scope| &at.item == scope || renames.items.get(scope) == Some(&at.item) || renames.items.get(&at.item) == Some(scope))))
    };
    for (scope, old, new) in &renames.idents {
        if let Some(fact) = rename_fact(scope.as_ref()) {
            words.entry((old.clone(), new.clone())).or_insert(fact);
        }
    }
    for (old, new) in &renames.fields {
        if let Some(fact) = rename_fact(None) {
            words.entry((old.clone(), new.clone())).or_insert(fact);
        }
    }

    // 7. Each changed line with no fact: the tokens of its hunk on the two sides.
    let mut open: BTreeMap<(String, usize, usize), Open> = BTreeMap::new();
    let mut layout: BTreeMap<String, [BTreeSet<usize>; 2]> = BTreeMap::new();
    let mut hunks_of: BTreeMap<&str, Vec<Hunk>> = BTreeMap::new();
    for (file, diff) in &diffs {
        let hunks = diff.hunks();
        hunks_of.insert(file.as_str(), hunks.clone());
        if !diff.parsed {
            for hunk in &hunks {
                for side in [OLD, NEW] {
                    for line in hunk.range[side].0..hunk.range[side].1 {
                        if !claims.contains_key(&(file.clone(), side, line)) {
                            open.insert((file.clone(), side, line), Open::Token);
                        }
                    }
                }
            }
            continue;
        }
        for hunk in &hunks {
            let free_lines: BTreeSet<(usize, usize)> = [OLD, NEW].into_iter().flat_map(|side| (hunk.range[side].0..hunk.range[side].1).map(move |line| (side, line))).filter(|(side, line)| !claims.contains_key(&(file.clone(), *side, *line))).collect();
            if free_lines.is_empty() {
                continue;
            }
            let free = |side: usize, line: usize| free_lines.contains(&(side, line));
            // The tokens of the hunk, in order, on each side.
            let lists: [Vec<usize>; 2] = [OLD, NEW].map(|side| {
                let mut list: Vec<usize> = (hunk.range[side].0..hunk.range[side].1).flat_map(|line| diff.sides[side].tokens_at[line - 1].iter().copied()).collect();
                list.dedup();
                list
            });
            let (old_tokens, new_tokens) = (diff.sides[OLD].tokens(), diff.sides[NEW].tokens());
            let same = |old: usize, new: usize| {
                let (a, b) = (&old_tokens[old], &new_tokens[new]);
                (a.class == TokenClass::Comment) == (b.class == TokenClass::Comment) && (a.text == b.text || (a.class != TokenClass::Comment && words.contains_key(&(a.text.clone(), b.text.clone()))))
            };
            // For each token of the hunk: the fact of its rename, or no fact for an equal token.
            // The lines with no fact pair first: a token of a line with a fact can be equal to
            // a token of a line with no fact, and then it takes the pair of a different line.
            let mut state: [BTreeMap<usize, Option<usize>>; 2] = Default::default();
            let free_lists: [Vec<usize>; 2] = [OLD, NEW].map(|side| {
                let mut list: Vec<usize> = (hunk.range[side].0..hunk.range[side].1).filter(|line| free(side, *line)).flat_map(|line| diff.sides[side].tokens_at[line - 1].iter().copied()).collect();
                list.dedup();
                list
            });
            for lists in [&free_lists, &lists] {
                let same = |i: usize, j: usize| same(lists[OLD][i], lists[NEW][j]);
                for (i, j) in matches(lists[OLD].len(), lists[NEW].len(), &same) {
                    let (old, new) = (lists[OLD][i], lists[NEW][j]);
                    if state[OLD].contains_key(&old) || state[NEW].contains_key(&new) {
                        continue;
                    }
                    let (a, b) = (&old_tokens[old], &new_tokens[new]);
                    let fact = if a.text == b.text { None } else { words.get(&(a.text.clone(), b.text.clone())).copied() };
                    state[OLD].insert(old, fact);
                    state[NEW].insert(new, fact);
                }
            }
            let mut equal: Vec<(usize, usize)> = Vec::new();
            let mut delimiters: Vec<(usize, usize)> = Vec::new();
            let mut changed_token = false;
            for side in [OLD, NEW] {
                let tokens = diff.sides[side].tokens();
                for line in hunk.range[side].0..hunk.range[side].1 {
                    if !free(side, line) {
                        continue;
                    }
                    let here = &diff.sides[side].tokens_at[line - 1];
                    let unmatched: Vec<&LineToken> = here.iter().filter(|index| !state[side].contains_key(index)).map(|index| &tokens[*index]).collect();
                    if let Some(fact) = here.iter().find_map(|index| state[side].get(index).copied().flatten()).filter(|_| unmatched.is_empty()) {
                        // Each token of the line is equal, or it is a name of the rename.
                        lines_of[fact].entry(file.clone()).or_default()[side].insert(line);
                        claim(&mut claims, file, side, line, fact);
                    } else if unmatched.is_empty() {
                        equal.push((side, line));
                    } else if unmatched.iter().all(|token| token.class == TokenClass::Comment) {
                        open.insert((file.clone(), side, line), Open::Comment);
                    } else if unmatched.iter().all(|token| token.class == TokenClass::Code && DELIMITERS.contains(&token.text.as_str())) {
                        delimiters.push((side, line));
                    } else {
                        open.insert((file.clone(), side, line), Open::Token);
                        changed_token = true;
                    }
                }
            }
            // A delimiter has no meaning alone: the tree has an edit for each change of its
            // shape. A line whose other tokens are equal is a part of the edit next to it in
            // the hunk. With no fact in the hunk, the line stays open.
            for (side, line) in delimiters {
                let near = (hunk.range[side].0..hunk.range[side].1).filter_map(|other| claims.get(&(file.clone(), side, other)).map(|facts| (other.abs_diff(line), facts[0]))).min();
                match near {
                    Some((_, fact)) if fact < fact_count => {
                        lines_of[fact].entry(file.clone()).or_default()[side].insert(line);
                        open.insert((file.clone(), side, line), Open::Equal);
                    }
                    _ => {
                        open.insert((file.clone(), side, line), Open::Token);
                        changed_token = true;
                    }
                }
            }
            // A line with equal tokens is a part of the edit next to it in the hunk. A hunk
            // with no fact and no changed token has a new layout only. A hunk with a changed
            // token that has no fact proves nothing for its other lines: they stay open.
            for (side, line) in equal {
                let near = (hunk.range[side].0..hunk.range[side].1)
                    .filter_map(|other| claims.get(&(file.clone(), side, other)).map(|facts| (other.abs_diff(line), facts[0])))
                    .min()
                    .or_else(|| (hunk.range[1 - side].0..hunk.range[1 - side].1).find_map(|other| claims.get(&(file.clone(), 1 - side, other)).map(|facts| (0, facts[0]))));
                match near {
                    Some((_, fact)) if fact < fact_count => {
                        lines_of[fact].entry(file.clone()).or_default()[side].insert(line);
                        open.insert((file.clone(), side, line), Open::Equal);
                    }
                    _ if changed_token => {
                        open.insert((file.clone(), side, line), Open::Pending);
                    }
                    _ => {
                        layout.entry(file.clone()).or_default()[side].insert(line);
                    }
                }
            }
        }
    }
    // The lines that joined a fact are not open.
    open.retain(|_, state| *state != Open::Equal);

    // 8. A moved item has each changed line of its old and its new range that no other fact
    // has: the diff of the pair gives a fact for each token that differs (design 49.3).
    for (fact, old_item, new_item) in moves {
        for (side, item) in [(OLD, old_item), (NEW, new_item)] {
            let keys: Vec<(String, usize, usize)> = open.range((item.file.clone(), side, item.line_start)..=(item.file.clone(), side, item.line_end)).map(|(key, _)| key.clone()).collect();
            for key in keys {
                open.remove(&key);
                lines_of[fact].entry(key.0.clone()).or_default()[side].insert(key.2);
                claim(&mut claims, &key.0, side, key.2, fact);
            }
        }
    }

    // 9. The same items in a new order (design 55.5): a pair of module items with equal
    // tokens and equal comments. The order of the items has no meaning if the file has no
    // macro in textual scope; the order of `use` items never has a meaning.
    let mut reordered: BTreeMap<String, usize> = BTreeMap::new();
    let mut order: BTreeMap<String, [BTreeSet<usize>; 2]> = BTreeMap::new();
    for (file, diff) in &diffs {
        let (Some(old_shape), Some(new_shape)) = (&diff.sides[OLD].shape, &diff.sides[NEW].shape) else { continue };
        let has_open = |side: usize, lines: (usize, usize)| open.range((file.clone(), side, lines.0)..=(file.clone(), side, lines.1)).next().is_some();
        let mut used = vec![false; new_shape.items.len()];
        let mut pairs = Vec::new();
        for old_item in old_shape.items.iter().filter(|item| has_open(OLD, item.lines)) {
            // A macro has a textual scope: an item that can give a macro does not move, and
            // an other item moves only between the same items that can give a macro.
            if !old_item.is_use && old_item.source {
                continue;
            }
            let found = new_shape.items.iter().enumerate().find(|(index, item)| {
                !used[*index] && item.container == old_item.container && item.key == old_item.key && item.is_use == old_item.is_use && (item.is_use || item.barrier == old_item.barrier) && has_open(NEW, item.lines)
            });
            if let Some((index, new_item)) = found {
                used[index] = true;
                pairs.push((old_item.lines, new_item.lines));
            }
        }
        for (old_lines, new_lines) in pairs {
            *reordered.entry(file.clone()).or_default() += 1;
            for (side, lines) in [(OLD, old_lines), (NEW, new_lines)] {
                let keys: Vec<(String, usize, usize)> = open.range((file.clone(), side, lines.0)..=(file.clone(), side, lines.1)).map(|(key, _)| key.clone()).collect();
                for key in keys {
                    open.remove(&key);
                    order.entry(file.clone()).or_default()[side].insert(key.2);
                }
            }
        }
    }

    // 9a. The item pair (design 55.5). The tree pairs an item of the old tree with an item of
    // the new tree. The line diff can pair a line of the item with a line of a different
    // item: then the line is a changed line, and its tokens are equal in the item pair. The
    // tokens of the two items give the changed tokens. A line with no changed token is a
    // part of the nearest fact of the item. An item with no fact, whose open lines have no
    // changed token, has a new layout only. A macro has a textual scope: the rule does not
    // apply to an item that can give a macro, or that moved across such an item.
    let back: BTreeMap<&String, &String> = renames.items.iter().map(|(old, new)| (new, old)).collect();
    let innermost = |side: usize, file: &str, line: usize| -> Option<&Item> {
        let items = indexes[side].by_file.get(file)?;
        items.iter().filter(|item| item.line_start <= line && line <= item.line_end && !matches!(item.kind, ItemKind::Use | ItemKind::Mod)).min_by_key(|item| item.line_end - item.line_start).copied()
    };
    let partner = |side: usize, item: &Item| -> Option<&Item> {
        if side == OLD {
            return input.after.items.get(renames.items.get(&item.path).unwrap_or(&item.path));
        }
        match back.get(&item.path) {
            Some(old) => input.before.items.get(*old),
            None => input.before.items.get(&item.path).filter(|_| !renames.items.contains_key(&item.path)),
        }
    };
    let mut groups: BTreeMap<&str, (&Item, &Item, Vec<(String, usize, usize)>)> = BTreeMap::new();
    for (key, state) in &open {
        if !matches!(state, Open::Token | Open::Pending) {
            continue;
        }
        let Some(item) = innermost(key.1, &key.0, key.2) else { continue };
        let Some(other) = partner(key.1, item).filter(|other| other.kind == item.kind) else { continue };
        let (old_item, new_item) = if key.1 == OLD { (item, other) } else { (other, item) };
        // An item in a different file is a part of a move: the rule of step 8.
        if old_item.file != new_item.file {
            continue;
        }
        groups.entry(old_item.path.as_str()).or_insert((old_item, new_item, Vec::new())).2.push(key.clone());
    }
    for (old_item, new_item, keys) in groups.into_values() {
        let items = [old_item, new_item];
        let (Some(old_diff), Some(new_diff)) = (diffs.get(&old_item.file), diffs.get(&new_item.file)) else { continue };
        let sides = [&old_diff.sides[OLD], &new_diff.sides[NEW]];
        // The module items around the item, from the file to the item.
        let chain = |side: usize| -> Option<Vec<&ModuleItem>> {
            let line = items[side].line_end;
            Some(sides[side].shape.as_ref()?.items.iter().filter(|item| item.lines.0 <= line && line <= item.lines.1).collect())
        };
        let (Some(old_chain), Some(new_chain)) = (chain(OLD), chain(NEW)) else { continue };
        if old_chain.len() != new_chain.len() || old_chain.iter().zip(&new_chain).any(|(a, b)| a.source || b.source || a.container != b.container || a.barrier != b.barrier) {
            continue;
        }
        let lists: [Vec<usize>; 2] = [OLD, NEW].map(|side| {
            let mut list: Vec<usize> = (items[side].line_start..=items[side].line_end).flat_map(|line| sides[side].tokens_at.get(line - 1).into_iter().flatten().copied()).collect();
            list.dedup();
            list
        });
        let tokens = [sides[OLD].tokens(), sides[NEW].tokens()];
        let same = |i: usize, j: usize| {
            let (a, b) = (&tokens[OLD][lists[OLD][i]], &tokens[NEW][lists[NEW][j]]);
            (a.class == TokenClass::Comment) == (b.class == TokenClass::Comment) && (a.text == b.text || (a.class != TokenClass::Comment && words.contains_key(&(a.text.clone(), b.text.clone()))))
        };
        let mut state: [BTreeMap<usize, Option<usize>>; 2] = Default::default();
        for (i, j) in matches(lists[OLD].len(), lists[NEW].len(), &same) {
            let (a, b) = (&tokens[OLD][lists[OLD][i]], &tokens[NEW][lists[NEW][j]]);
            let fact = if a.text == b.text { None } else { words.get(&(a.text.clone(), b.text.clone())).copied() };
            state[OLD].insert(lists[OLD][i], fact);
            state[NEW].insert(lists[NEW][j], fact);
        }
        // For each open line with no changed token: the fact of a rename on the line.
        let mut equal: Vec<(&(String, usize, usize), Option<usize>)> = Vec::new();
        for key in &keys {
            let here = &sides[key.1].tokens_at[key.2 - 1];
            if here.iter().all(|index| state[key.1].contains_key(index)) {
                equal.push((key, here.iter().find_map(|index| state[key.1].get(index).copied().flatten())));
            }
        }
        let in_item = |side: usize, line: usize| -> Option<(usize, usize)> {
            let item = items[side];
            claims.range((item.file.clone(), side, item.line_start)..=(item.file.clone(), side, item.line_end)).filter_map(|(at, facts)| facts.iter().copied().find(|fact| *fact < fact_count).map(|fact| (at.2.abs_diff(line), fact))).min()
        };
        let has_fact = in_item(OLD, 0).is_some() || in_item(NEW, 0).is_some();
        let all_equal = equal.len() == keys.len();
        for (key, rename) in equal {
            let fact = rename.filter(|fact| *fact < fact_count).or_else(|| in_item(key.1, key.2).or_else(|| in_item(1 - key.1, 0)).map(|(_, fact)| fact));
            match fact {
                Some(fact) => {
                    lines_of[fact].entry(key.0.clone()).or_default()[key.1].insert(key.2);
                }
                None if !has_fact && all_equal => {
                    layout.entry(key.0.clone()).or_default()[key.1].insert(key.2);
                }
                None => continue,
            }
            open.remove(key);
        }
    }

    // 9b. A line with equal tokens in a hunk that had a changed token with no fact: the rule
    // of step 7 again, after the rules of steps 8 and 9. The line is a part of the edit next
    // to it. With no fact in the hunk and no changed token with no fact, the hunk has a new
    // layout only. If a changed token of the hunk has no fact, the line stays open.
    let pending: Vec<(String, usize, usize)> = open.iter().filter(|(_, state)| **state == Open::Pending).map(|(key, _)| key.clone()).collect();
    for key in pending {
        let (file, side, line) = (&key.0, key.1, key.2);
        let Some(hunk) = hunks_of.get(file.as_str()).and_then(|hunks| hunks.iter().find(|hunk| hunk.range[side].0 <= line && line < hunk.range[side].1)) else { continue };
        let near = (hunk.range[side].0..hunk.range[side].1)
            .filter_map(|other| claims.get(&(file.clone(), side, other)).map(|facts| (other.abs_diff(line), facts[0])))
            .min()
            .or_else(|| (hunk.range[1 - side].0..hunk.range[1 - side].1).find_map(|other| claims.get(&(file.clone(), 1 - side, other)).map(|facts| (0, facts[0]))));
        let token_left = [OLD, NEW].into_iter().any(|other_side| (hunk.range[other_side].0..hunk.range[other_side].1).any(|other| open.get(&(file.clone(), other_side, other)) == Some(&Open::Token)));
        match near {
            Some((_, fact)) if fact < fact_count => {
                lines_of[fact].entry(file.clone()).or_default()[side].insert(line);
                open.remove(&key);
            }
            _ if token_left => {
                open.insert(key, Open::Token);
            }
            _ => {
                layout.entry(file.clone()).or_default()[side].insert(line);
                open.remove(&key);
            }
        }
    }

    // 10. The positions of the facts of the edits.
    for (index, files) in lines_of.iter().enumerate() {
        report.facts[index].positions = files.iter().flat_map(|(file, lines)| position_list(file, lines)).collect();
    }
    let compare = if input.step.compiled { Compare::NoCode } else { Compare::Off };
    let add = |facts: &mut Vec<Fact>, file: &str, part: &str, category: Category, description: String, positions: Vec<Position>| {
        let id = format!("f{}", facts.len() + 1);
        facts.push(Fact { id, file: Some(file.to_string()), part: part.to_string(), description, status: "derived".into(), category, compare, positions, ..Default::default() });
    };

    // 11. The layout fact of each file: what Semilla proved for the lines.
    let files: BTreeSet<&String> = layout.keys().chain(order.keys()).collect();
    for file in files {
        let mut proved = Vec::new();
        let mut lines: [BTreeSet<usize>; 2] = Default::default();
        if let Some(equal) = layout.get(file) {
            proved.push(format!("{} with equal tokens", count(equal[OLD].len() + equal[NEW].len(), "changed line")));
            for side in [OLD, NEW] {
                lines[side].extend(&equal[side]);
            }
        }
        if let Some(moved) = order.get(file) {
            let items = reordered.get(file).copied().unwrap_or_default();
            proved.push(format!("{} of {} with equal tokens at a new place", count(moved[OLD].len() + moved[NEW].len(), "changed line"), count(items, "item")));
            for side in [OLD, NEW] {
                lines[side].extend(&moved[side]);
            }
        }
        add(&mut report.facts, file, "layout", Category::Layout, format!("layout of `{file}`: {}", proved.join("; ")), position_list(file, &lines));
    }

    // 12. The line-ending fact of each file.
    for (file, pairs) in &endings {
        let diff = &diffs[*file];
        let name = |ending: u8| ["no line break", "LF", "CRLF"][ending as usize];
        let mut kinds: BTreeMap<(u8, u8), usize> = BTreeMap::new();
        let mut lines: [BTreeSet<usize>; 2] = Default::default();
        for &(old, new) in pairs {
            *kinds.entry((diff.sides[OLD].endings[old], diff.sides[NEW].endings[new])).or_default() += 1;
            lines[OLD].insert(old + 1);
            lines[NEW].insert(new + 1);
        }
        let text: Vec<String> = kinds.iter().map(|((old, new), lines)| format!("{} → {} in {}", name(*old), name(*new), count(*lines, "line"))).collect();
        add(&mut report.facts, file, "line endings", Category::Layout, format!("line endings of `{file}`: {}; the text of each line is equal", text.join(", ")), position_list(file, &lines));
    }

    // 13. A comment that is in no item: one fact for each block of comment lines of a hunk.
    let mut comment_facts: Vec<(String, [BTreeSet<usize>; 2])> = Vec::new();
    for (file, hunks) in &hunks_of {
        for hunk in hunks {
            let blocks: [Vec<BTreeSet<usize>>; 2] = [OLD, NEW].map(|side| {
                let mut blocks: Vec<BTreeSet<usize>> = Vec::new();
                for line in hunk.range[side].0..hunk.range[side].1 {
                    if open.get(&(file.to_string(), side, line)) != Some(&Open::Comment) {
                        continue;
                    }
                    match blocks.last_mut() {
                        Some(last) if last.contains(&(line - 1)) => {
                            last.insert(line);
                        }
                        _ => blocks.push(BTreeSet::from([line])),
                    }
                }
                blocks
            });
            for index in 0..blocks[OLD].len().max(blocks[NEW].len()) {
                let lines = [blocks[OLD].get(index).cloned().unwrap_or_default(), blocks[NEW].get(index).cloned().unwrap_or_default()];
                comment_facts.push((file.to_string(), lines));
            }
        }
    }
    for (file, lines) in comment_facts {
        let diff = &diffs[&file];
        let text = |side: usize| -> String {
            let tokens = diff.sides[side].tokens();
            let mut indexes: Vec<usize> = lines[side].iter().flat_map(|line| diff.sides[side].tokens_at[line - 1].iter().copied()).filter(|index| tokens[*index].class == TokenClass::Comment).collect();
            indexes.dedup();
            indexes.iter().map(|index| tokens[*index].text.as_str()).collect::<Vec<_>>().join(&semilla_core::text::LINE_MARK.to_string())
        };
        let description = match (lines[OLD].is_empty(), lines[NEW].is_empty()) {
            (true, _) => format!("comment in `{file}`: added `{}`", text(NEW)),
            (_, true) => format!("comment in `{file}`: removed `{}`", text(OLD)),
            _ => format!("comment in `{file}`: changed `{}` → `{}`", text(OLD), text(NEW)),
        };
        for side in [OLD, NEW] {
            for line in &lines[side] {
                open.remove(&(file.clone(), side, *line));
            }
        }
        add(&mut report.facts, &file, "comment", Category::Docs, description, position_list(&file, &lines));
    }

    // 14. Each other open line is a changed line with no fact: a defect of Semilla.
    let mut no_fact: BTreeMap<String, [BTreeSet<usize>; 2]> = BTreeMap::new();
    for (file, side, line) in open.keys() {
        no_fact.entry(file.clone()).or_default()[*side].insert(*line);
    }
    report.no_fact = no_fact.iter().flat_map(|(file, lines)| position_list(file, lines)).collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(a: &str, b: &str) -> Vec<(usize, usize)> {
        let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
        matches(a.len(), b.len(), &|i, j| a[i] == b[j])
    }

    #[test]
    fn the_diff_gives_the_pairs_of_equal_elements() {
        assert_eq!(pairs("abc", "abc"), [(0, 0), (1, 1), (2, 2)]);
        assert_eq!(pairs("abc", "axc"), [(0, 0), (2, 2)]);
        assert_eq!(pairs("abcd", "ad"), [(0, 0), (3, 1)]);
        assert_eq!(pairs("ad", "abcd"), [(0, 0), (1, 3)]);
        assert_eq!(pairs("", "ab"), []);
        // Each pair is a pair of equal elements, in order, and the count is the longest.
        for (a, b, length) in [("abcabba", "cbabac", 4), ("xaxbxcx", "abc", 3), ("abcdef", "badcfe", 3), ("aaaa", "aa", 2), ("abc", "def", 0)] {
            let found = pairs(a, b);
            assert_eq!(found.len(), length, "{a} {b}");
            assert!(found.windows(2).all(|pair| pair[0].0 < pair[1].0 && pair[0].1 < pair[1].1), "{a} {b}");
            assert!(found.iter().all(|(i, j)| a.as_bytes()[*i] == b.as_bytes()[*j]), "{a} {b}");
        }
    }
}
