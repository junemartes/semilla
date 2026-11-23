//! The position of each fact in the line diff of the step, and the cover check (design 55.2
//! and 55.4). A position comes from the syntax tree: the range of the changed node, or the
//! lines of the changed own tokens. No part of this module searches a source text.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{Anchor, Category, Compare, Fact, Position};
use semilla_syntax::cover::{self, LineToken, ModuleItem, Shape, TokenClass};
use semilla_syntax::tree::Pos;
use semilla_syntax::{Edit, EditKind, Item, ItemKind, Node, Refactor, Workspace};

use crate::linediff::{self, GitHunk, Texts};
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

/// The macros whose result depends on the path of the file of the call (design 56.3).
const PATH_MACROS: &[&str] = &["include", "include_str", "include_bytes", "file"];

/// The source place of a module on one side (design 56.3).
struct ModulePlace {
    /// The file of the module, or the file of its block.
    file: String,
    /// True for a module with a block.
    inline: bool,
    /// The first and the last line of the place. A file has each of its lines.
    lines: (usize, usize),
    /// For a module with a file: the file and the lines of its `mod` line. A crate root has
    /// none.
    header: Option<(String, usize, usize)>,
    /// The name of the module.
    name: String,
}

impl ModulePlace {
    fn of(workspace: &Workspace, path: &str) -> Option<ModulePlace> {
        let item = workspace.items.get(path).filter(|item| item.kind == ItemKind::Mod);
        let name = path.rsplit("::").next().unwrap_or("").split('#').next().unwrap_or("").to_string();
        match workspace.module_files.get(path) {
            Some(file) => Some(ModulePlace { file: file.clone(), inline: false, lines: (1, usize::MAX), header: item.map(|item| (item.file.clone(), item.line_start, item.line_end)), name }),
            None => item.map(|item| ModulePlace { file: item.file.clone(), inline: true, lines: (item.line_start, item.line_end), header: None, name }),
        }
    }
}

/// The fact of a module with a new source place: the file of the fact, and its lines by file.
struct PlaceFact {
    file: String,
    category: Category,
    description: String,
    lines: BTreeMap<String, [BTreeSet<usize>; 2]>,
    /// The path of the module item in the old tree. A crate root has no item.
    item: Option<String>,
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

/// The line diff of one file of the step (design 55.8). The diff starts as the diff of the
/// line diff command. [`FileDiff::slide`] moves a block to the place that agrees with the
/// tree: the result is a diff with the same count of changed lines. Each changed line of the
/// result is a changed line of the command, or it has the same text as one changed line of
/// the command that is an equal line of the result (`alias`).
struct FileDiff {
    sides: [Side; 2],
    /// The pairs of equal lines (from 0), in order. A pair with two different line endings is
    /// a pair of changed lines of the command.
    pairs: Vec<(usize, usize)>,
    /// True for a Rust file that Semilla parses on each side that has it.
    parsed: bool,
    /// The hunks of the line diff command.
    git: Vec<GitHunk>,
    /// For each side: a changed line of this diff (from 1) that is an equal line of the
    /// command, to its changed line of the command. The two lines have the same text.
    alias: [BTreeMap<usize, usize>; 2],
}

/// Writes that the line `gained` (from 0) is now a changed line and the line `lost` is now an
/// equal line. The two lines have the same text: `gained` gets the line of the command of
/// `lost`.
fn swap(alias: &mut BTreeMap<usize, usize>, gained: usize, lost: usize) {
    let git = alias.remove(&(lost + 1)).unwrap_or(lost + 1);
    if git != gained + 1 {
        alias.insert(gained + 1, git);
    }
}

/// The start of one side of a hunk of the command, from 0. A side with the count 0 has the
/// number of the line before the hunk.
fn hunk_start(side: (usize, usize)) -> usize {
    if side.1 == 0 { side.0 } else { side.0.saturating_sub(1) }
}

/// The pairs of the equal lines of a diff with the hunks `hunks`. In a hunk, a pair of lines
/// with the same text and different line endings is a pair too: the two lines are changed
/// lines, and the line-ending fact has them.
fn pairs_of(sides: &[Side; 2], hunks: &[GitHunk]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let (mut a, mut b) = (0, 0);
    for hunk in hunks {
        let (next_a, next_b) = (hunk_start(hunk.old), hunk_start(hunk.new));
        pairs.extend((a..next_a).zip(b..next_b));
        let (n, m) = (hunk.old.1, hunk.new.1);
        if n > 0 && m > 0 {
            let same = |i: usize, j: usize| sides[OLD].lines[next_a + i] == sides[NEW].lines[next_b + j] && sides[OLD].endings[next_a + i] != sides[NEW].endings[next_b + j];
            pairs.extend(matches(n, m, &same).into_iter().map(|(i, j)| (next_a + i, next_b + j)));
        }
        (a, b) = (next_a + n, next_b + m);
    }
    pairs.extend((a..sides[OLD].lines.len()).zip(b..sides[NEW].lines.len()));
    pairs
}

/// The hunks of the diff of Semilla, in the form of the command: the result if git gives no
/// line diff. A line is equal only with an equal line ending, as in git.
fn own_hunks(texts: &Texts) -> Vec<GitHunk> {
    let (old, new) = (linediff::full_lines(texts.old), linediff::full_lines(texts.new));
    let mut out = Vec::new();
    let (mut a, mut b) = (0, 0);
    let end = (old.len(), new.len());
    for (i, j) in matches(old.len(), new.len(), &|i, j| old[i] == new[j]).into_iter().chain(std::iter::once(end)) {
        if i > a || j > b {
            let side = |from: usize, to: usize| if to > from { (from + 1, to - from) } else { (from, 0) };
            out.push(GitHunk { old: side(a, i), new: side(b, j) });
        }
        (a, b) = (i + 1, j + 1);
    }
    out
}

/// One hunk of a line diff: the changed lines (from 1, the end is not in the range) of each
/// side.
#[derive(Clone, Copy, Debug)]
struct Hunk {
    range: [(usize, usize); 2],
}

impl FileDiff {
    /// The line of the command (from 1) for a changed line of this diff.
    fn git_line(&self, side: usize, line: usize) -> usize {
        self.alias[side].get(&line).copied().unwrap_or(line)
    }

    /// True for a pair of equal lines of the command: the two line endings are equal.
    fn exact(&self, pair: (usize, usize)) -> bool {
        self.sides[OLD].endings[pair.0] == self.sides[NEW].endings[pair.1]
    }

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
            let (lines, ends) = (&self.sides[side].lines, &self.sides[side].endings);
            let is_good = |line: usize| good[side].contains(&(line + 1)) || self.sides[side].tokens_at.get(line).is_some_and(|tokens| tokens.is_empty());
            let free = |line: usize, pair: usize| lines[line] == lines[pair] && ends[line] == ends[pair] && !is_good(line) && is_good(pair);
            // The equal line after the hunk: the last line of the hunk that can replace it.
            if gap < self.pairs.len() && self.exact(self.pairs[gap]) {
                let pair = pick(self.pairs[gap]);
                if let Some(line) = (pick(start)..pick(after)).rev().find(|line| free(*line, pair)) {
                    if side == OLD { self.pairs[gap].0 = line } else { self.pairs[gap].1 = line }
                    swap(&mut self.alias[side], pair, line);
                    return true;
                }
            }
            // The equal line before the hunk: the first line of the hunk that can replace it.
            if gap > 0 && self.exact(self.pairs[gap - 1]) {
                let pair = pick(self.pairs[gap - 1]);
                if let Some(line) = (pick(start)..pick(after)).find(|line| free(*line, pair)) {
                    if side == OLD { self.pairs[gap - 1].0 = line } else { self.pairs[gap - 1].1 = line }
                    swap(&mut self.alias[side], pair, line);
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
            let (lines, ends) = (&self.sides[side].lines, &self.sides[side].endings);
            let twin = |a: usize, b: usize| lines[a] == lines[b] && ends[a] == ends[b];
            let is_good = |line: usize| good[side].contains(&(line + 1)) || self.sides[side].tokens_at.get(line).is_some_and(|tokens| tokens.is_empty());
            // The count of the lines that the block can go up, and that it can go down. The
            // block goes only past a pair of equal lines of the command.
            let mut up = 0;
            while gap > up && self.exact(self.pairs[gap - 1 - up]) && pick(self.pairs[gap - 1 - up]) + up + 1 == first && other(self.pairs[gap - 1 - up]) + up + 1 == other(start) && twin(first - 1 - up, first + count - 1 - up) {
                up += 1;
            }
            let mut down = 0;
            while gap + down < self.pairs.len() && self.exact(self.pairs[gap + down]) && pick(self.pairs[gap + down]) == first + count + down && other(self.pairs[gap + down]) == other(start) + down && twin(first + down, first + count + down) {
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
                // One line at a time: the line above the block joins it, and its last line
                // leaves it. The two lines have the same text.
                for step in 1..=steps {
                    swap(&mut self.alias[side], first - step, first + count - step);
                }
                gap -= steps;
            } else if shift > 0 {
                let steps = shift as usize;
                for index in 0..steps {
                    set(&mut self.pairs[gap + index], first + index);
                }
                for step in 1..=steps {
                    swap(&mut self.alias[side], first + count + step - 1, first + step - 1);
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
    // Design 59, B-a: the position has each line of each node that the text of the fact
    // shows. An edit of the own tokens shows the own tokens only (design 52.1). A `Modified`
    // edit with the full text of the node shows the full node.
    let text = if side == OLD { edit.text_before.as_ref() } else { edit.text_after.as_ref() };
    let full_text = text == Some(&node.text);
    let mut lines = if edit.kind == EditKind::Modified && !full_text && diff.sides[side].shape.is_some() {
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
            Some(semilla_syntax::Resolved::Item(item)) => item_scope(item, edit.kind, side, diffs, &indexes[side]).map(|mut scope| {
                // Design 59, B-a: a body that a step adds to a function or removes from it is
                // in the text of the fact ("body removed from …: `{ … }`"). The position has
                // each line of the body.
                let node = if side == OLD { edit.node_before } else { edit.node_after };
                if edit.kind == EditKind::Signature && node == Some("body") {
                    if let Some(body) = item.body.as_ref().filter(|body| body.start.0 > 0) {
                        scope.lines.extend(body.start.0..=body.end.0);
                    }
                }
                scope
            }),
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

/// The line diff of each file with a different text in the two trees (design 55.8): the
/// hunks of the line diff command. The second result is true if git gave no line diff: then
/// the hunks are the hunks of the diff of Semilla.
fn file_diffs(input: &StepInput) -> (BTreeMap<String, FileDiff>, bool) {
    fn text<'w>(workspace: &'w Workspace, file: &str) -> Option<&'w String> {
        workspace.rust_files.get(file).or_else(|| workspace.other_files.get(file))
    }
    let mut files: BTreeSet<&String> = BTreeSet::new();
    for workspace in [input.before, input.after] {
        files.extend(workspace.rust_files.keys());
        files.extend(workspace.other_files.keys());
    }
    let changed: Vec<(&String, Option<&String>, Option<&String>)> = files.into_iter().map(|file| (file, text(input.before, file), text(input.after, file))).filter(|(_, old, new)| old != new).collect();
    let texts: Vec<Texts> = changed.iter().map(|(_, old, new)| Texts { old: old.map(String::as_str), new: new.map(String::as_str) }).collect();
    let (hunks, own) = if texts.is_empty() {
        (Vec::new(), false)
    } else {
        match linediff::git_hunks(&texts) {
            Some(hunks) => (hunks, false),
            None => (texts.iter().map(own_hunks).collect(), true),
        }
    };
    let mut out = BTreeMap::new();
    for ((file, old, new), git) in changed.into_iter().zip(hunks) {
        let parsed = [(old, input.before), (new, input.after)].iter().all(|(text, workspace)| text.is_none() || workspace.parsed_files.contains(file));
        let sides = [Side::new(old, parsed, input.before.editions.get(file).copied()), Side::new(new, parsed, input.after.editions.get(file).copied())];
        let pairs = pairs_of(&sides, &git);
        out.insert(file.clone(), FileDiff { sides, pairs, parsed, git, alias: Default::default() });
    }
    (out, own)
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

/// The positions of the changed lines `lines` of a file, as lines of the line diff command
/// (design 55.8).
fn position_list(diffs: &BTreeMap<String, FileDiff>, file: &str, lines: &[BTreeSet<usize>; 2]) -> Vec<Position> {
    let diff = diffs.get(file);
    let mut out = Vec::new();
    for side in [NEW, OLD] {
        out.extend(lines[side].iter().map(|line| {
            let line = diff.map_or(*line, |diff| diff.git_line(side, *line));
            Position { file: file.to_string(), old: side == OLD, start: line, end: line }
        }));
    }
    Position::normal(out)
}

/// True if a `use` item can name a macro in textual scope: a path of the item has one
/// segment (`use two;`, `pub(crate) use two as three;`, `use {two, a::b};`). `key`: the tokens
/// of the item with one space between them. A key that the function cannot read gives true.
pub(crate) fn one_segment_use(key: &str) -> bool {
    let tokens: Vec<&str> = key.split(' ').collect();
    let Some(start) = tokens.iter().position(|token| *token == "use") else { return true };
    // For each open group: true if a path is before the group.
    let mut rooted = vec![false];
    for index in start + 1..tokens.len() {
        let (before, after) = (tokens[index - 1], tokens.get(index + 1).copied());
        match tokens[index] {
            "{" => rooted.push(rooted.last().copied().unwrap_or(false) || before == "::"),
            "}" => {
                rooted.pop();
            }
            "::" | "," | ";" | "as" | "*" | "self" | "super" | "crate" => {}
            _ => {
                if before != "::" && before != "as" && after != Some("::") && !rooted.last().copied().unwrap_or(false) {
                    return true;
                }
            }
        }
    }
    false
}

/// The ranges of lines of the nodes around an edit on one side, from the smallest one: each
/// node above the node of the edit, the item, each item around it, and the file.
fn wider_ranges(edit: &Edit, side: usize, input: &StepInput, diffs: &BTreeMap<String, FileDiff>, indexes: &[ItemIndex; 2]) -> Vec<(String, (usize, usize))> {
    let (anchor, workspace) = if side == OLD { (&edit.before, input.before) } else { (&edit.after, input.after) };
    let mut out: Vec<(String, (usize, usize))> = Vec::new();
    // An edit with a node on the other side only has no node on this side.
    if anchor.is_none() && (edit.before.is_some() || edit.after.is_some()) {
        return out;
    }
    let mut file = edit.file.clone();
    if let Some(item) = anchor.as_ref().and_then(|anchor| workspace.items.get(&anchor.item)) {
        let anchor = anchor.as_ref().unwrap();
        for depth in (1..anchor.path.len()).rev() {
            let above = semilla_core::Anchor { item: anchor.item.clone(), path: anchor.path[..depth].to_vec() };
            if let Some(semilla_syntax::Resolved::Node(_, node)) = semilla_syntax::resolve(workspace, &above) {
                if node.start.0 > 0 {
                    out.push((item.file.clone(), (node.start.0, node.end.0)));
                }
            }
        }
        out.push((item.file.clone(), (item.line_start, item.line_end)));
        let mut around: Vec<(usize, usize)> = indexes[side].by_file.get(item.file.as_str()).into_iter().flatten().filter(|other| other.line_start <= item.line_start && item.line_end <= other.line_end).map(|other| (other.line_start, other.line_end)).collect();
        around.sort_by_key(|(start, end)| end - start);
        out.extend(around.into_iter().map(|lines| (item.file.clone(), lines)));
        file = Some(item.file.clone());
    }
    if let Some(file) = file {
        if let Some(diff) = diffs.get(&file) {
            out.push((file, (1, diff.sides[side].lines.len())));
        }
    }
    out.dedup();
    out
}

/// True if the tree compare pairs a token with an equal token (design 59, B-b and B-d). The
/// owner of the token is the smallest item that has it, and the item needs a partner of its
/// kind. A token of the body is equal if the lowest node pair around it has equal tokens, or
/// if the token is an own token of that pair (in no child) and the own tokens of the pair are
/// equal. A token of the head of the item is equal if the head of the pair is equal: the
/// name, the signature, and the list of the members for a code token, the attributes, the docs (with the
/// plain comments of the item), or the visibility for a token of that class. The compare is
/// strict: a rename map makes no token equal here. Each other token is not equal.
fn equal_token<'w>(side: usize, file: &str, token: &LineToken, indexes: &[ItemIndex<'w>; 2], node_pairs: &BTreeMap<String, Vec<semilla_syntax::NodePair>>, partner: &dyn Fn(usize, &Item) -> Option<&'w Item>) -> bool {
    let at = token.start;
    let Some(item) = indexes[side].by_file.get(file).into_iter().flatten().copied().filter(|item| (item.line_start, item.col_start) <= at && at < (item.line_end, item.col_end)).max_by_key(|item| ((item.line_start, item.col_start), std::cmp::Reverse((item.line_end, item.col_end)))) else { return false };
    if item.kind == ItemKind::Use {
        return false;
    }
    let Some(other) = partner(side, item).filter(|other| other.kind == item.kind) else { return false };
    let old_item = if side == OLD { item } else { other };
    match token.class {
        TokenClass::Comment | TokenClass::Doc => return item.docs == other.docs,
        TokenClass::Attribute => return item.attrs == other.attrs,
        TokenClass::Visibility => return item.vis == other.vis,
        TokenClass::Code => {}
    }
    let Some(body) = item.body.as_ref().filter(|body| body.start.0 > 0 && body.start <= at && at < body.end) else {
        // The name of an item is not in the signature of each kind (a field): a pair by a
        // rename has two names.
        let name = |path: &str| path.rsplit(['.', ':']).next().unwrap_or("").to_string();
        return name(&item.path) == name(&other.path) && item.signature == other.signature && item.members == other.members;
    };
    if other.body.is_none() {
        return false;
    }
    // The nodes around the token, from the body down.
    let mut chain: Vec<&Node> = vec![body];
    while let Some(child) = chain.last().and_then(|node| node.children.iter().find(|child| child.start.0 > 0 && child.start <= at && at < child.end)) {
        chain.push(child);
    }
    let Some(pairs) = node_pairs.get(&old_item.path) else { return false };
    for (depth, node) in chain.iter().enumerate().rev() {
        // A pair only for the control flow graph (R60.3-10) does not count here.
        let Some(pair) = pairs.iter().find(|pair| !pair.graph_only && pair.ranges[side] == (node.start, node.end) && pair.kinds[side] == node.kind) else { continue };
        return pair.equal || (depth + 1 == chain.len() && pair.own_equal);
    }
    false
}

fn count(value: usize, noun: &str) -> String {
    format!("{value} {noun}{}", if value == 1 { "" } else { "s" })
}

/// Gives each fact its position, adds the facts of the cover check (layout, line endings, a
/// comment that is in no item), and writes the changed lines with no fact into the report.
pub(crate) fn assign(input: &StepInput, outcome: &mut StepOutcome) {
    let StepOutcome { report, edits, fact_of, renames, node_pairs, .. } = outcome;
    let (mut diffs, own) = file_diffs(input);
    if diffs.is_empty() {
        return;
    }
    // Design 55.8: the hunks of the line diff command are a part of the report. A tag place
    // selects these hunks.
    report.own_diff = own;
    if !input.step.tree_before.is_empty() && !input.step.tree_after.is_empty() {
        report.diff_trees = vec![input.step.tree_before.clone(), input.step.tree_after.clone()];
    }
    report.hunks = diffs
        .iter()
        .flat_map(|(file, diff)| diff.git.iter().map(move |hunk| semilla_core::report::DiffHunk { file: file.clone(), old_start: hunk.old.0, old_count: hunk.old.1, new_start: hunk.new.0, new_count: hunk.new.1 }))
        .collect();
    let indexes = [ItemIndex::new(input.before), ItemIndex::new(input.after)];
    let fact_count = report.facts.len();

    // 1. The lines of each edit that has a fact.
    let mut scopes: Vec<(usize, Vec<Scope>)> = Vec::new();
    let mut edits_of: Vec<Vec<usize>> = vec![Vec::new(); fact_count];
    for (index, edit) in edits.iter().enumerate() {
        let Some(fact) = fact_of.get(index).copied().flatten().filter(|fact| *fact < fact_count) else { continue };
        scopes.push((fact, edit_scopes(edit, input, &diffs, &indexes)));
        edits_of[fact].push(index);
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

    // 3. The changed lines of each file. A pair of lines with the same text and different line
    // endings is a pair of line-ending lines: the two lines are changed lines of the command.
    // The count is the count of the lines of the hunks of the command.
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
        for (scope, lines) in found {
            for line in lines {
                lines_of[*fact].entry(scope.file.clone()).or_default()[scope.side].insert(line);
                claim(&mut claims, &scope.file, scope.side, line, *fact);
            }
        }
    }
    // A fact with no changed line in its edits (a statement at a new place between equal
    // lines): the own lines of the edits are equal lines of the command. They are not a part
    // of the position (design 55.2, rule 5). They stay as the place of the fact for the rule
    // of the item pair (step 9a): a changed line of the item with equal tokens joins the
    // nearest fact of the item.
    for (fact, list) in &scopes {
        if !lines_of[*fact].is_empty() {
            continue;
        }
        for scope in list {
            for line in &scope.lines {
                claim(&mut claims, &scope.file, scope.side, *line, *fact);
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

    // 6a. A module whose source place changes (design 56.3): a new file, a block that is now
    // a file, or a file that is now a block. The identity of the module and of its items is
    // the path in the item tree. The tokens of each item pair of the tree compare, and the
    // tokens of the module that are in no item compare. A line whose tokens are all in a
    // pair with equal tokens keeps its tokens: it is a line of the fact of the source place.
    // A line with an edit stays in the fact of the edit. The step is before step 7: a line
    // of the place is not a line of the nearest edit of its hunk.
    let mut place_facts: Vec<PlaceFact> = Vec::new();
    let mut placed: BTreeSet<(String, usize, usize)> = BTreeSet::new();
    let module_paths: BTreeSet<&String> = input.before.module_files.keys().chain(input.before.items.iter().filter(|(_, item)| item.kind == ItemKind::Mod).map(|(path, _)| path)).collect();
    for old_path in module_paths {
        let new_path = renames.items.get(old_path).unwrap_or(old_path);
        let (Some(old_place), Some(new_place)) = (ModulePlace::of(input.before, old_path), ModulePlace::of(input.after, new_path)) else { continue };
        if old_place.inline == new_place.inline && (old_place.inline || old_place.file == new_place.file) {
            continue;
        }
        let (Some(old_diff), Some(new_diff)) = (diffs.get(&old_place.file), diffs.get(&new_place.file)) else { continue };
        let sides = [&old_diff.sides[OLD], &new_diff.sides[NEW]];
        if sides.iter().any(|side| side.shape.is_none()) {
            continue;
        }
        let places = [&old_place, &new_place];
        let range = [OLD, NEW].map(|side| (places[side].lines.0, places[side].lines.1.min(sides[side].lines.len())));
        let in_place = |side: usize, item: &Item| item.file == places[side].file && range[side].0 <= item.line_start && item.line_end <= range[side].1;
        // The `mod` line of the side with a file, if the other side has a block: its tokens
        // pair with the tokens of the start of the block.
        let file_side = if old_place.inline != new_place.inline { Some(if old_place.inline { NEW } else { OLD }) } else { None };
        let header: Option<(usize, &String, &Side, usize, usize)> = file_side.and_then(|side| {
            let (file, first, last) = places[side].header.as_ref()?;
            Some((side, file, &diffs.get(file)?.sides[side], *first, *last))
        });
        // A token: 0 and its index in the file of the place, or 1 and its index in the file
        // of the `mod` line.
        type Ref = (usize, usize);
        let token = |side: usize, at: Ref| -> &LineToken {
            match (at.0, header) {
                (1, Some((_, _, source, _, _))) => &source.tokens()[at.1],
                _ => &sides[side].tokens()[at.1],
            }
        };
        // The tokens of each pair of the tree: the key is the path of the old item. The
        // tokens that are in no item have the empty key.
        let mut groups: BTreeMap<String, [Vec<Ref>; 2]> = BTreeMap::new();
        if let Some((side, _, source, first, last)) = header {
            let list = &mut groups.entry(String::new()).or_default()[side];
            for line in first..=last {
                list.extend(source.tokens_at.get(line - 1).into_iter().flatten().filter(|index| source.tokens()[**index].start.0 == line).map(|index| (1, *index)));
            }
        }
        for side in [OLD, NEW] {
            for line in range[side].0..=range[side].1 {
                let key = match innermost(side, &places[side].file, line) {
                    None => Some(String::new()),
                    Some(item) => partner(side, item).filter(|other| other.kind == item.kind && in_place(1 - side, other)).map(|other| if side == OLD { item.path.clone() } else { other.path.clone() }),
                };
                let Some(key) = key else { continue };
                let here = sides[side].tokens_at[line - 1].iter().filter(|index| sides[side].tokens()[**index].start.0 == line).map(|index| (0, *index));
                groups.entry(key).or_default()[side].extend(here);
            }
        }
        let same = |a: &LineToken, b: &LineToken| (a.class == TokenClass::Comment) == (b.class == TokenClass::Comment) && (a.text == b.text || (a.class != TokenClass::Comment && words.contains_key(&(a.text.clone(), b.text.clone()))));
        let mut state: [BTreeMap<Ref, Option<usize>>; 2] = Default::default();
        for lists in groups.values() {
            for (i, j) in matches(lists[OLD].len(), lists[NEW].len(), &|i, j| same(token(OLD, lists[OLD][i]), token(NEW, lists[NEW][j]))) {
                let (a, b) = (token(OLD, lists[OLD][i]), token(NEW, lists[NEW][j]));
                let fact = if a.text == b.text { None } else { words.get(&(a.text.clone(), b.text.clone())).copied() };
                state[OLD].insert(lists[OLD][i], fact);
                state[NEW].insert(lists[NEW][j], fact);
            }
        }
        // The tokens that are the change of the place: the `{` and the `}` of the block, and
        // the `;` of the `mod` line.
        let mut frame: [BTreeSet<Ref>; 2] = Default::default();
        if let (Some(file_side), Some(lists)) = (file_side, groups.get("")) {
            let block_side = 1 - file_side;
            let list = &lists[block_side];
            let name = places[block_side].name.as_str();
            let open_at = (2..list.len()).find(|&at| token(block_side, list[at]).text == "{" && token(block_side, list[at - 1]).text == name && token(block_side, list[at - 2]).text == "mod");
            if let (Some(open_at), Some(last)) = (open_at, list.last().filter(|last| token(block_side, **last).text == "}")) {
                frame[block_side].extend([list[open_at], *last].into_iter().filter(|at| !state[block_side].contains_key(at)));
            }
            if let Some(last) = lists[file_side].iter().rfind(|at| at.0 == 1).filter(|last| token(file_side, **last).text == ";" && !state[file_side].contains_key(*last)) {
                frame[file_side].insert(*last);
            }
        }
        // The nodes whose meaning depends on the path of the file (rule 5). The fact names
        // each one. Such a node is not in the count of the lines that keep their tokens.
        let mut dependent: Vec<String> = Vec::new();
        let mut dependent_lines: [BTreeSet<usize>; 2] = Default::default();
        for side in [OLD, NEW] {
            let tokens: Vec<&LineToken> = sides[side].tokens().iter().filter(|token| !token.void && token.class != TokenClass::Comment && range[side].0 <= token.start.0 && token.start.0 <= range[side].1).collect();
            for (at, here) in tokens.iter().enumerate() {
                let text = |offset: usize| tokens.get(at + offset).map_or("", |token| token.text.as_str());
                let before = if at > 0 { tokens[at - 1].text.as_str() } else { "" };
                let end = if PATH_MACROS.contains(&here.text.as_str()) && text(1) == "!" {
                    // The macro call, to the end of its input.
                    let mut depth = 0usize;
                    let mut end = at + 1;
                    for (offset, other) in tokens[at + 2..].iter().enumerate() {
                        match other.text.as_str() {
                            "(" | "[" | "{" => depth += 1,
                            ")" | "]" | "}" => depth = depth.saturating_sub(1),
                            _ => {}
                        }
                        end = at + 2 + offset;
                        if depth == 0 {
                            break;
                        }
                    }
                    end
                } else if here.text == "path" && text(1) == "=" && matches!(before, "[" | "(" | ",") {
                    at + 2
                } else {
                    continue;
                };
                let end = end.min(tokens.len() - 1);
                dependent_lines[side].extend(here.start.0..=tokens[end].end_line);
                if side == NEW {
                    let mut label: String = tokens[at..=end].iter().map(|token| token.text.as_str()).collect::<Vec<_>>().join(if here.text == "path" { " " } else { "" });
                    if label.chars().count() > 60 {
                        label = format!("{}…", label.chars().take(60).collect::<String>());
                    }
                    dependent.push(format!("`{label}` (`{}:{}`)", places[NEW].file, here.start.0));
                }
            }
        }
        // The lines. A line with a name of a rename is a line of the fact of the rename.
        let mut lines: BTreeMap<String, [BTreeSet<usize>; 2]> = BTreeMap::new();
        let mut kept = [0usize; 2];
        let mut targets: Vec<(usize, &String, &Side, usize, usize, usize)> = [OLD, NEW].into_iter().map(|side| (side, &places[side].file, sides[side], range[side].0, range[side].1, 0)).collect();
        if let Some((side, file, source, first, last)) = header {
            targets.push((side, file, source, first, last, 1));
        }
        for (side, file, source, first, last, origin) in targets {
            // Each changed line of the place that no fact of an edit has.
            let keys: Vec<(String, usize, usize)> = diffs[file]
                .hunks()
                .iter()
                .flat_map(|hunk| hunk.range[side].0.max(first)..hunk.range[side].1.min(last.saturating_add(1)))
                .map(|line| (file.clone(), side, line))
                .filter(|key| !claims.contains_key(key) && !placed.contains(key))
                .collect();
            for key in keys {
                let here: Vec<Ref> = source.tokens_at[key.2 - 1].iter().map(|index| (origin, *index)).collect();
                if !here.iter().all(|at| state[side].contains_key(at) || frame[side].contains(at)) {
                    continue;
                }
                placed.insert(key.clone());
                match here.iter().find_map(|at| state[side].get(at).copied().flatten()) {
                    Some(fact) if fact < fact_count => {
                        lines_of[fact].entry(key.0.clone()).or_default()[side].insert(key.2);
                        claim(&mut claims, &key.0, side, key.2, fact);
                    }
                    _ => {
                        lines.entry(key.0.clone()).or_default()[side].insert(key.2);
                        if origin == 0 && !dependent_lines[side].contains(&key.2) {
                            kept[side] += 1;
                        }
                    }
                }
            }
        }
        // An inner doc line or an inner attribute of a file, with a change: the edit is an
        // edit of the module item if the other side has a block. The line is a line of the
        // fact of that edit.
        if let Some(side) = file_side {
            let source = sides[side];
            let file = &places[side].file;
            let edit_fact = |category: Category| (0..fact_count).find(|&fact| report.facts[fact].category == category && report.facts[fact].at.as_ref().is_some_and(|at| at.path.is_empty() && (&at.item == old_path || &at.item == new_path)));
            for line in diffs[file].hunks().iter().flat_map(|hunk| hunk.range[side].0..hunk.range[side].1) {
                let key = (file.clone(), side, line);
                let here = &source.tokens_at[line - 1];
                if here.is_empty() || claims.contains_key(&key) || placed.contains(&key) || !here.iter().all(|index| source.tokens()[*index].top) {
                    continue;
                }
                let docs = here.iter().all(|index| source.tokens()[*index].text.starts_with("//!") || source.tokens()[*index].text.starts_with("/*!"));
                let order = if docs { [Category::Docs, Category::Attribute] } else { [Category::Attribute, Category::Docs] };
                if let Some(fact) = order.into_iter().find_map(edit_fact) {
                    lines_of[fact].entry(file.clone()).or_default()[side].insert(line);
                    claim(&mut claims, file, side, line, fact);
                }
            }
        }
        if lines.is_empty() {
            continue;
        }
        // The items of the old place that are in no other item, and the ones that keep each
        // token. A module and a `use` line are not in the count.
        let all: Vec<&Item> = indexes[OLD].by_file.get(places[OLD].file.as_str()).into_iter().flatten().copied().filter(|item| in_place(OLD, item) && !matches!(item.kind, ItemKind::Use | ItemKind::Mod)).collect();
        let outer: Vec<&Item> = all.iter().copied().filter(|item| !all.iter().any(|other| other.path != item.path && other.line_start <= item.line_start && item.line_end <= other.line_end && (other.line_start, other.line_end) != (item.line_start, item.line_end))).filter(|item| !item.path.contains('.')).collect();
        let keeps = |item: &Item| -> bool {
            let Some(other) = partner(OLD, item).filter(|other| other.kind == item.kind && in_place(NEW, other)) else { return false };
            [(OLD, item), (NEW, other)].into_iter().all(|(side, item)| {
                (item.line_start..=item.line_end).all(|line| !dependent_lines[side].contains(&line) && sides[side].tokens_at[line - 1].iter().all(|index| state[side].get(&(0, *index)) == Some(&None)))
            })
        };
        let kept_items = outer.iter().filter(|item| keeps(item)).count();
        let place_text = |place: &ModulePlace| if place.inline { format!("a block in `{}`", place.file) } else { format!("file `{}`", place.file) };
        let name = if old_path == new_path { format!("`{old_path}`") } else { format!("`{old_path}` (now `{new_path}`)") };
        let line_text = if kept[OLD] == kept[NEW] { count(kept[OLD], "line") } else { format!("{} and {}", count(kept[OLD], "old line"), count(kept[NEW], "new line")) };
        let mut description = format!("source place of {name}: {} → {}; {kept_items} of {} and {line_text} keep their tokens", place_text(&old_place), place_text(&new_place), count(outer.len(), "item"));
        // The category (design 54.2): `move` is a new place with the same tokens. A node that
        // depends on the path of the file can change the behavior: a macro call gives
        // `macro`, a `path` attribute gives `attribute`.
        let mut category = Category::Move;
        if !dependent.is_empty() {
            category = if dependent.iter().any(|label| !label.starts_with("`path ")) { Category::Macro } else { Category::Attribute };
            description.push_str(&format!("; the meaning of {} depends on the path of the file: {}", count(dependent.len(), "node"), dependent.join(", ")));
        }
        let item = input.before.items.contains_key(old_path).then(|| old_path.clone());
        place_facts.push(PlaceFact { file: new_place.file.clone(), category, description, lines, item });
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
            let free_lines: BTreeSet<(usize, usize)> = [OLD, NEW].into_iter().flat_map(|side| (hunk.range[side].0..hunk.range[side].1).map(move |line| (side, line))).filter(|(side, line)| !claims.contains_key(&(file.clone(), *side, *line)) && !placed.contains(&(file.clone(), *side, *line))).collect();
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
    let mut scope_facts: Vec<(String, Option<String>, String, [BTreeSet<usize>; 2])> = Vec::new();
    // The module items with one identity on the two sides (file, side, first line): the order
    // check of design 56.5 has each of them.
    let mut checked: BTreeSet<(String, usize, usize)> = BTreeSet::new();
    let mut order: BTreeMap<String, [BTreeSet<usize>; 2]> = BTreeMap::new();
    for (file, diff) in &diffs {
        let (Some(old_shape), Some(new_shape)) = (&diff.sides[OLD].shape, &diff.sides[NEW].shape) else { continue };
        let has_open = |side: usize, lines: (usize, usize)| open.range((file.clone(), side, lines.0)..=(file.clone(), side, lines.1)).next().is_some();
        let mut used = vec![false; new_shape.items.len()];
        let mut pairs = Vec::new();
        // The identity of a module item: the path of its item in the old tree. A `use` item
        // has an identity only if it can name a macro in textual scope: then it has its text.
        let tree_item = |side: usize, lines: (usize, usize)| -> Option<&Item> {
            let items = indexes[side].by_file.get(file.as_str())?;
            items.iter().copied().filter(|item| lines.0 <= item.line_start && item.line_end <= lines.1).max_by_key(|item| (item.line_end - item.line_start, std::cmp::Reverse(item.path.len())))
        };
        let identity = |side: usize, item: &ModuleItem| -> Option<String> {
            if item.is_use {
                return one_segment_use(&item.key).then(|| item.key.replace(" :: ", "::").replace(" ;", ";").replace(" ( ", "(").replace(" ) ", ") "));
            }
            let path = &tree_item(side, item.lines)?.path;
            if side == OLD {
                return Some(path.clone());
            }
            match back.get(path) {
                Some(old) => Some((*old).clone()),
                None => (!renames.items.contains_key(path)).then(|| path.clone()),
            }
        };
        let ids: [Vec<Option<String>>; 2] = [OLD, NEW].map(|side| [old_shape, new_shape][side].items.iter().map(|item| identity(side, item)).collect());
        let mut new_at: BTreeMap<(&str, &str), usize> = BTreeMap::new();
        for (index, item) in new_shape.items.iter().enumerate().rev() {
            if let Some(id) = &ids[NEW][index] {
                new_at.insert((item.container.as_str(), id.as_str()), index);
            }
        }
        // The items with one identity on the two sides, in the old order: the index on each
        // side, and true if the item can give a macro on one side.
        let mut common: Vec<(usize, usize, bool)> = Vec::new();
        let mut partner_of: BTreeMap<usize, usize> = BTreeMap::new();
        for (index, item) in old_shape.items.iter().enumerate() {
            let Some(id) = &ids[OLD][index] else { continue };
            let Some(&other) = new_at.get(&(item.container.as_str(), id.as_str())) else { continue };
            if partner_of.values().all(|used| *used != other) {
                partner_of.insert(index, other);
                common.push((index, other, item.source || new_shape.items[other].source));
                checked.insert((file.clone(), OLD, item.lines.0));
                checked.insert((file.clone(), NEW, new_shape.items[other].lines.0));
            }
        }
        // Design 56.5: a macro has a textual scope. The order of an item and of an item that
        // can give a macro is a part of the meaning. For each pair with a different order on
        // the two sides, the item with no pair of equal lines is the item at a new place.
        let fixed = |entry: &(usize, usize, bool)| {
            let (old, new) = (old_shape.items[entry.0].lines, new_shape.items[entry.1].lines);
            diff.pairs.iter().any(|(a, b)| old.0 <= a + 1 && a + 1 <= old.1 && new.0 <= b + 1 && b + 1 <= new.1)
        };
        let mut moves: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (x, first) in common.iter().enumerate() {
            for (y, second) in common.iter().enumerate().skip(x + 1) {
                if !(first.2 || second.2) || first.1 < second.1 || old_shape.items[first.0].container != old_shape.items[second.0].container {
                    continue;
                }
                let (own, other) = match (fixed(first), fixed(second)) {
                    (true, true) => continue,
                    (true, false) => (y, x),
                    (false, true) => (x, y),
                    (false, false) => if first.2 && !second.2 { (y, x) } else { (x, y) },
                };
                moves.entry(own).or_default().push(other);
            }
        }
        let mut scoped: Vec<(Option<String>, String, (usize, usize), (usize, usize))> = Vec::new();
        let mut moved: BTreeSet<usize> = BTreeSet::new();
        for (own, others) in &moves {
            let (old_index, new_index, gives) = common[*own];
            let name = |entry: usize| format!("`{}`", ids[OLD][common[entry].0].as_deref().unwrap_or_default());
            let mut names: Vec<String> = others.iter().take(6).map(|other| name(*other)).collect();
            if others.len() > names.len() {
                names.push(format!("{} more", others.len() - names.len()));
            }
            let own_name = name(*own);
            let description = if gives {
                format!("moved {own_name} (it can give a macro) past {}: {}; a macro has a textual scope: a macro call in these items can have a new meaning", count(others.len(), "item"), names.join(", "))
            } else {
                format!("moved {own_name} past {} that can give a macro: {}; a macro has a textual scope: a macro call in {own_name} can have a new meaning", count(others.len(), "item"), names.join(", "))
            };
            used[new_index] = true;
            moved.insert(old_index);
            // The item of the fact: the item at a new place. For a `use` item: the first item
            // of the list that has a path.
            let anchor = std::iter::once(own).chain(others).map(|entry| common[*entry].0).find(|index| !old_shape.items[*index].is_use).and_then(|index| ids[OLD][index].clone());
            scoped.push((anchor, description, old_shape.items[old_index].lines, new_shape.items[new_index].lines));
        }
        for (old_index, old_item) in old_shape.items.iter().enumerate().filter(|(index, item)| !moved.contains(index) && has_open(OLD, item.lines)) {
            let same = |item: &ModuleItem| item.container == old_item.container && item.key == old_item.key && item.is_use == old_item.is_use;
            let found = match &ids[OLD][old_index] {
                // The order of the item to each item that can give a macro is the same.
                Some(_) => partner_of.get(&old_index).map(|index| (*index, &new_shape.items[*index])).filter(|(index, item)| !used[*index] && same(item) && has_open(NEW, item.lines)),
                // The order of `use` items with a path of two segments or more never has a
                // meaning. An item with no identity moves only between the same items that
                // can give a macro, and only if it cannot give a macro.
                None if !old_item.is_use && old_item.source => None,
                None => new_shape.items.iter().enumerate().find(|(index, item)| !used[*index] && ids[NEW][*index].is_none() && same(item) && (item.is_use || item.barrier == old_item.barrier) && has_open(NEW, item.lines)),
            };
            if let Some((index, new_item)) = found {
                used[index] = true;
                pairs.push((old_item.lines, new_item.lines));
            }
        }
        // The lines of a fact of the textual scope: the open lines of the item. An item with
        // no open line has a fact for each of its changed lines: the fact gets these lines.
        for (own, description, old_lines, new_lines) in scoped {
            let mut lines: [BTreeSet<usize>; 2] = Default::default();
            for (side, range) in [(OLD, old_lines), (NEW, new_lines)] {
                let keys: Vec<(String, usize, usize)> = open.range((file.clone(), side, range.0)..=(file.clone(), side, range.1)).map(|(key, _)| key.clone()).collect();
                for key in keys {
                    open.remove(&key);
                    lines[side].insert(key.2);
                }
            }
            if lines[OLD].is_empty() && lines[NEW].is_empty() {
                let equal: [BTreeSet<usize>; 2] = [diff.pairs.iter().map(|pair| pair.0 + 1).collect(), diff.pairs.iter().map(|pair| pair.1 + 1).collect()];
                for (side, range) in [(OLD, old_lines), (NEW, new_lines)] {
                    lines[side] = (range.0..=range.1).filter(|line| !equal[side].contains(line)).collect();
                }
            }
            scope_facts.push((file.clone(), own, description, lines));
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
        // The order check of step 9 (design 56.5) gives a fact to each item of the chain that
        // has a new order to an item that can give a macro. An item that the check does not
        // have keeps the strict rule: it cannot give a macro, and the same items that can
        // give a macro are before it.
        let in_check = |a: &ModuleItem, b: &ModuleItem| checked.contains(&(old_item.file.clone(), OLD, a.lines.0)) && checked.contains(&(new_item.file.clone(), NEW, b.lines.0));
        if old_chain.len() != new_chain.len() || old_chain.iter().zip(&new_chain).any(|(a, b)| a.container != b.container || (!in_check(a, b) && (a.source || b.source || a.barrier != b.barrier))) {
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

    // 9a2. A line with equal tokens in the pairs of the tree compare (design 59, B-b). Each
    // token of a changed line with no fact is equal if the tree compare pairs it with an
    // equal token. Then the line has no difference of the tree: it joins the nearest fact of
    // its item, else it is a line of the layout fact (R55.5-1). A line with one token that is
    // not equal stays open (B-c).
    let open_tokens: Vec<(String, usize, usize)> = open.iter().filter(|(_, state)| **state == Open::Token).map(|(key, _)| key.clone()).collect();
    let mut equal_lines: BTreeMap<(String, String), [BTreeSet<usize>; 2]> = BTreeMap::new();
    for key in open_tokens {
        let (file, side, line) = (&key.0, key.1, key.2);
        let Some(source) = diffs.get(file).map(|diff| &diff.sides[side]).filter(|source| source.shape.is_some()) else { continue };
        let here = &source.tokens_at[line - 1];
        if here.is_empty() || !here.iter().all(|index| equal_token(side, file, &source.tokens()[*index], &indexes, node_pairs, &partner)) {
            continue;
        }
        let Some(item) = innermost(side, file, line) else { continue };
        if let Some(other) = partner(side, item) {
            let (old_item, new_item) = if side == OLD { (item, other) } else { (other, item) };
            let cause = format!("in `{}` (old lines {}-{} of `{}`, new lines {}-{} of `{}`)", old_item.path, old_item.line_start, old_item.line_end, old_item.file, new_item.line_start, new_item.line_end, new_item.file);
            equal_lines.entry((file.clone(), cause)).or_default()[side].insert(line);
        }
        let near = |side: usize, line: usize| -> Option<usize> {
            claims.range((item.file.clone(), side, item.line_start)..=(item.file.clone(), side, item.line_end)).filter_map(|(at, facts)| facts.iter().copied().find(|fact| *fact < fact_count).map(|fact| (at.2.abs_diff(line), fact))).min().map(|(_, fact)| fact)
        };
        let other = partner(side, item);
        let fact = near(side, line).or_else(|| other.and_then(|other| claims.range((other.file.clone(), 1 - side, other.line_start)..=(other.file.clone(), 1 - side, other.line_end)).find_map(|(_, facts)| facts.iter().copied().find(|fact| *fact < fact_count))));
        match fact {
            Some(fact) => {
                lines_of[fact].entry(file.clone()).or_default()[side].insert(line);
            }
            None => {
                layout.entry(file.clone()).or_default()[side].insert(line);
            }
        }
        open.remove(&key);
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

    // 9c. A fact whose own lines are all equal lines of the command (design 55.2, rule 5; a
    // statement at a new place between equal lines). The fact gets the changed lines of the
    // smallest wider node that has a changed line: the node above the node of the edit, then
    // the item, then each item around it, then the file. The rule gives a position only. It
    // gives no line to the cover check: steps 7 to 9b already examined each of these lines.
    for (fact, list) in edits_of.iter().enumerate() {
        if list.is_empty() || !lines_of[fact].is_empty() {
            continue;
        }
        let ranges: Vec<[Vec<(String, (usize, usize))>; 2]> = list.iter().map(|index| [OLD, NEW].map(|side| wider_ranges(&edits[*index], side, input, &diffs, &indexes))).collect();
        let levels = ranges.iter().flat_map(|sides| sides.iter().map(Vec::len)).max().unwrap_or(0);
        for level in 0..levels {
            let mut found: BTreeMap<String, [BTreeSet<usize>; 2]> = BTreeMap::new();
            for sides in &ranges {
                for side in [OLD, NEW] {
                    let Some((file, (start, end))) = sides[side].get(level).or_else(|| sides[side].last()) else { continue };
                    // The file is the last range of a side. It waits for the last level.
                    if level + 1 < levels && sides[side].len() <= level {
                        continue;
                    }
                    let Some(lines) = changed.get(file.as_str()) else { continue };
                    let hit: Vec<usize> = lines[side].range(*start..=*end).copied().collect();
                    if !hit.is_empty() {
                        found.entry(file.clone()).or_default()[side].extend(hit);
                    }
                }
            }
            if !found.is_empty() {
                lines_of[fact] = found;
                report.wider_facts += 1;
                break;
            }
        }
    }

    // 10. The positions of the facts of the edits.
    for (index, files) in lines_of.iter().enumerate() {
        report.facts[index].positions = files.iter().flat_map(|(file, lines)| position_list(&diffs, file, lines)).collect();
    }
    let compare = if input.step.compiled { Compare::NoCode } else { Compare::Off };
    let add = |facts: &mut Vec<Fact>, file: &str, part: &str, category: Category, description: String, positions: Vec<Position>| {
        let id = format!("f{}", facts.len() + 1);
        facts.push(Fact { id, file: Some(file.to_string()), part: part.to_string(), description, status: "derived".into(), category, compare, positions, ..Default::default() });
    };

    // 10a. The fact of each module with a new source place (step 6a). The fact of each item
    // with a new order to an item that can give a macro (step 9): the category is `macro`,
    // because a macro call can get a different definition (design 54.2).
    for place in place_facts {
        let positions = place.lines.iter().flat_map(|(file, lines)| position_list(&diffs, file, lines)).collect();
        add(&mut report.facts, &place.file, "place", place.category, place.description, positions);
        if let (Some(item), Some(fact)) = (place.item, report.facts.last_mut()) {
            fact.at = Some(Anchor::item(&item));
        }
    }
    for (file, item, description, lines) in scope_facts {
        add(&mut report.facts, &file, "macro scope", Category::Macro, description, position_list(&diffs, &file, &lines));
        if let (Some(item), Some(fact)) = (item, report.facts.last_mut()) {
            fact.at = Some(Anchor::item(&item));
        }
    }

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
        add(&mut report.facts, file, "layout", Category::Layout, format!("layout of `{file}`: {}", proved.join("; ")), position_list(&diffs, file, &lines));
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
        add(&mut report.facts, file, "line endings", Category::Layout, format!("line endings of `{file}`: {}; the text of each line is equal", text.join(", ")), position_list(&diffs, file, &lines));
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
        add(&mut report.facts, &file, "comment", Category::Docs, description, position_list(&diffs, &file, &lines));
    }

    // 14. Each other open line is a changed line with no fact: a defect of Semilla.
    let mut no_fact: BTreeMap<String, [BTreeSet<usize>; 2]> = BTreeMap::new();
    for (file, side, line) in open.keys() {
        no_fact.entry(file.clone()).or_default()[*side].insert(*line);
    }
    report.no_fact = no_fact.iter().flat_map(|(file, lines)| position_list(&diffs, file, lines)).collect();

    // 14a. The cause of each open line (design 56.4): the smallest item of the tree that has
    // the line, and the state of its tokens.
    let texts = |side: usize, item: &Item| -> Vec<&str> {
        let Some(diff) = diffs.get(&item.file) else { return Vec::new() };
        diff.sides[side].tokens().iter().filter(|token| item.line_start <= token.start.0 && token.start.0 <= item.line_end && !token.void && token.class != TokenClass::Comment).map(|token| token.text.as_str()).collect()
    };
    let mut causes: BTreeMap<(String, String), [BTreeSet<usize>; 2]> = BTreeMap::new();
    for ((file, side, line), state) in &open {
        let items = indexes[*side].by_file.get(file.as_str()).filter(|items| !items.is_empty());
        let cause = match (diffs.get(file).is_some_and(|diff| diff.parsed), items) {
            (false, _) => "a file that Semilla does not parse".to_string(),
            (true, None) => "a file with no item in the tree (for example a module file with no item)".to_string(),
            (true, Some(items)) => {
                let line_state = match state {
                    Open::Equal | Open::Pending => "the tokens of the line are equal in the line diff",
                    Open::Comment => "the changed tokens of the line are comments",
                    Open::Token => "a token of the line changes",
                };
                match items.iter().filter(|item| item.line_start <= *line && *line <= item.line_end).min_by_key(|item| item.line_end - item.line_start) {
                    None => format!("a line in no item of the tree; {line_state}"),
                    Some(item) => {
                        let pair = match partner(*side, item) {
                            None => "its tokens are in no pair of the tree compare",
                            Some(other) if texts(*side, item) == texts(1 - *side, other) => "its tokens are in a pair with equal tokens",
                            Some(_) => "its tokens are in a pair with a difference",
                        };
                        format!("in `{}`: {pair}; {line_state}", item.path)
                    }
                }
            }
        };
        causes.entry((file.clone(), cause)).or_default()[*side].insert(*line);
    }
    report.equal_token_lines = equal_lines.iter().map(|((file, cause), lines)| semilla_core::report::NoFactCause { cause: cause.clone(), positions: position_list(&diffs, file, lines) }).collect();
    report.no_fact_causes = causes.iter().map(|((file, cause), lines)| semilla_core::report::NoFactCause { cause: cause.clone(), positions: position_list(&diffs, file, lines) }).collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(text: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), text).unwrap();
        let workspace = Workspace::load(dir.path()).unwrap();
        (dir, workspace)
    }

    /// Design 59, B-b and B-c: a token is equal only if the tree compare pairs it with an equal
    /// token. The own tokens of a call with equal own tokens are equal; a literal in a pair
    /// with a difference is not; a token of an item with no partner is not.
    // rule+ R59.2-2 R59.2-3 R59.2-4
    // rule- R59.2-2 R59.2-3 R59.2-4
    #[test]
    fn a_token_is_equal_only_in_a_pair_of_the_tree_compare() {
        let old_text = "pub fn f(a: u32) -> u32 {\n    g(\n        a,\n        1,\n    )\n}\nfn g(a: u32, b: u32) -> u32 { a + b }\npub fn gone() -> u32 { 5 }\n";
        let new_text = "pub fn f(a: u32) -> u32 {\n    g(a, 2)\n}\nfn g(a: u32, b: u32) -> u32 { a + b }\n";
        let ((_old_dir, old), (_new_dir, new)) = (tree(old_text), tree(new_text));
        let (_, maps) = semilla_syntax::diff_maps(&old, &new, &semilla_syntax::Renames::default());
        // The tree compare keeps the pairs of the body of `f`: the call with a difference, and
        // the argument `a` with equal tokens.
        let pairs = &maps.pairs["depot::f"];
        assert!(pairs.iter().any(|pair| pair.kinds == ["call", "call"] && !pair.equal && pair.own_equal), "{pairs:?}");
        assert!(pairs.iter().any(|pair| pair.equal && pair.ranges[OLD].0 == (3, 8)), "{pairs:?}");
        let indexes = [ItemIndex::new(&old), ItemIndex::new(&new)];
        let partner = |side: usize, item: &Item| -> Option<&Item> { [&new, &old][side].items.get(&item.path) };
        let shape = cover::shape(old_text, semilla_syntax::rs::Edition::Edition2021);
        let equal = |line: usize, text: &str| -> bool {
            let token = shape.tokens.iter().find(|token| token.start.0 == line && token.text == text).unwrap();
            equal_token(OLD, "src/lib.rs", token, &indexes, &maps.pairs, &partner)
        };
        // An own token of the call, and a token in a pair with equal tokens.
        assert!(equal(2, "g") && equal(2, "(") && equal(3, "a") && equal(5, ")"));
        // A token in a pair with a difference (B-c): `1` → `2`.
        assert!(!equal(4, "1"));
        // A token of an item with no partner.
        assert!(!equal(8, "5"));
        // A token of the head: equal for a pair with one name, not equal for a pair with two
        // names (a pair by a rename).
        assert!(equal(1, "f") && equal(1, "u32"));
        let renamed_item = |side: usize, item: &Item| -> Option<&Item> { [&new, &old][side].items.get(if item.path == "depot::f" { "depot::g" } else { item.path.as_str() }) };
        let head = shape.tokens.iter().find(|token| token.start.0 == 1 && token.text == "u32").unwrap();
        assert!(!equal_token(OLD, "src/lib.rs", head, &indexes, &maps.pairs, &renamed_item));
        // A rename map makes no token equal here: the compare of the pair is strict.
        let renamed = semilla_syntax::NodePair { ranges: [((3, 8), (3, 9)), ((2, 6), (2, 7))], kinds: ["path", "path"], equal: false, own_equal: false, graph_only: false };
        let strict: BTreeMap<String, Vec<semilla_syntax::NodePair>> = [("depot::f".to_string(), vec![renamed])].into_iter().collect();
        assert!(!equal_token(OLD, "src/lib.rs", shape.tokens.iter().find(|token| token.start.0 == 3 && token.text == "a").unwrap(), &indexes, &strict, &partner));
        // The own tokens of a pair count only for the lowest node of the token. A token in a
        // child with no pair is not equal, also when the pair of an ancestor has equal own tokens.
        let call = pairs.iter().find(|pair| pair.kinds == ["call", "call"]).unwrap().clone();
        let ancestor_only: BTreeMap<String, Vec<semilla_syntax::NodePair>> = [("depot::f".to_string(), vec![call])].into_iter().collect();
        assert!(!equal_token(OLD, "src/lib.rs", shape.tokens.iter().find(|token| token.start.0 == 3 && token.text == "a").unwrap(), &indexes, &ancestor_only, &partner));
        // A pair of two fields with an equal type and two names (a pair by a rename): the head
        // tokens are not equal. The signatures are equal, so only the name makes the difference.
        let (field_old, field_new) = ("pub struct P {\n    pub x: u32,\n}\n", "pub struct P {\n    pub y: u32,\n}\n");
        let ((_fo_dir, fo), (_fn_dir, fnew)) = (tree(field_old), tree(field_new));
        let field_of = |ws: &Workspace| ws.items.values().find(|item| item.kind == ItemKind::Field).unwrap().path.clone();
        let (x, y) = (field_of(&fo), field_of(&fnew));
        assert_eq!(fo.items[&x].signature, fnew.items[&y].signature);
        let field_indexes = [ItemIndex::new(&fo), ItemIndex::new(&fnew)];
        let field_pair = |side: usize, item: &Item| -> Option<&Item> { if side == OLD { fnew.items.get(if item.path == x { &y } else { &item.path }) } else { fo.items.get(if item.path == y { &x } else { &item.path }) } };
        let field_shape = cover::shape(field_old, semilla_syntax::rs::Edition::Edition2021);
        let field_type = field_shape.tokens.iter().find(|token| token.start.0 == 2 && token.text == "u32").unwrap();
        assert!(!equal_token(OLD, "src/lib.rs", field_type, &field_indexes, &BTreeMap::new(), &field_pair));
    }

    /// The tree compare keeps a pair of each arm of a replaced `match` for the control flow
    /// graph (design 60, R60.3-10): the arms with equal parts, then the arms with an equal
    /// pattern. The positions of the facts do not read these pairs.
    // rule+ R60.3-10
    // rule- R60.3-10
    #[test]
    fn the_arms_of_a_replaced_match_pair_only_for_the_graph() {
        let old_text = "pub enum S { A, B, C }\npub fn f(s: S) -> u32 {\n    match s {\n        S::A => g(1),\n        S::B => g(2),\n    }\n}\nfn g(a: u32) -> u32 { a }\n";
        let new_text = "pub enum S { A, B, C }\npub fn f(s: S) -> u32 {\n    match s {\n        S::C => g(9),\n        S::A => g(1),\n        S::B => g(3),\n    }\n}\nfn g(a: u32) -> u32 { a }\n";
        let ((_old_dir, old), (_new_dir, new)) = (tree(old_text), tree(new_text));
        let (_, maps) = semilla_syntax::diff_maps(&old, &new, &semilla_syntax::Renames::default());
        let arms: Vec<&semilla_syntax::NodePair> = maps.pairs["depot::f"].iter().filter(|pair| pair.graph_only).collect();
        let lines = |pair: &semilla_syntax::NodePair| (pair.ranges[OLD].0.0, pair.ranges[NEW].0.0, pair.equal);
        let found: Vec<(usize, usize, bool)> = arms.iter().map(|pair| lines(pair)).collect();
        // The scrutinee; the arm `S::A` (equal parts); the arm `S::B` (an equal pattern, a new
        // body `g(2)` → `g(3)`).
        assert!(found.contains(&(3, 3, true)) && found.contains(&(4, 5, true)) && found.contains(&(5, 6, true)) && found.contains(&(5, 6, false)), "{found:?}");
        // The negative cases: the new arm `S::C` has no pair, and a token of an equal arm is
        // not an equal token for the positions of the facts.
        assert!(!found.iter().any(|(_, line, _)| *line == 4), "{found:?}");
        let indexes = [ItemIndex::new(&old), ItemIndex::new(&new)];
        let partner = |side: usize, item: &Item| -> Option<&Item> { [&new, &old][side].items.get(&item.path) };
        let shape = cover::shape(old_text, semilla_syntax::rs::Edition::Edition2021);
        let token = shape.tokens.iter().find(|token| token.start.0 == 4 && token.text == "1").unwrap();
        assert!(!equal_token(OLD, "src/lib.rs", token, &indexes, &maps.pairs, &partner));
    }

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
