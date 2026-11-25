//! The statement mapper of a body (design 41).
//!
//! The mapper works on the whole body of a pair of matched functions, not on one block at a
//! time. It follows the refactoring-aware AST diff of RefactoringMiner (Alikhanifard and
//! Tsantalis, 2024):
//! 1. Leaf statements first (a statement with no block), in rounds. Each round maps only the
//!    statements that no earlier round mapped:
//!    1. the same compare text, and the parents correspond (or both are the body);
//!    2. the same compare text at any depth of the body (a move across blocks);
//!    3. the same text after replacements, with the best score.
//! 2. Composite statements next (a statement with a block): the same kind, the same child roles,
//!    an equal or similar header, and a large part of the children mapped to each other.
//!
//! A consistent rename of a local applies in each round. A new `let v = E;` whose `E` is a
//! sub-expression that is `v` in a mapped statement is an extracted variable; the reverse is an
//! inlined variable. Each statement that the mapper does not map goes to the gap rules of the
//! syntax diff (the rules of batch 8b).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use semilla_core::matcher::{Key, Ladder, Pair, Thing};

use crate::tree::Node;

/// The most statement pairs that the mapper scores in one body (the rename votes, round 3, and
/// the composites). Above the limit, the body keeps the alignment of batch 8b. Decided by Claude.
pub const MAX_SCORED: usize = 20_000;
/// The smallest score of a pair in round 3: the share of the tokens that need no replacement.
/// Decided by Claude.
pub const MIN_SCORE: f64 = 0.6;
/// The smallest share of the children of two composites that map to each other. Decided by
/// Claude.
pub const MIN_SHARE: f64 = 0.5;
/// The smallest share of equal words (names and literals) of a pair in round 3: the punctuation
/// of a short statement does not make a pair. Decided by Claude.
pub const MIN_WORDS: f64 = 0.5;
/// The preference of round 3 for a pair whose parents correspond (the same block): a pair at a
/// different place wins only with a score that is higher by more than this value. Decided by
/// Claude.
pub const LOCAL_BONUS: f64 = 0.2;
/// The smallest score of a pair of round 3 at a different place (the parents do not
/// correspond). Decided by Claude.
pub const FAR_SCORE: f64 = 0.7;
/// The smallest similarity of two different headers of a composite pair. Decided by Claude.
pub const MIN_HEADER: f64 = 0.5;

/// One statement of a body.
pub(crate) struct Stmt<'a> {
    pub node: &'a Node,
    /// The role path from the body (`S1/then/S0`).
    pub path: Vec<String>,
    /// The enclosing statement. `None`: the statement is in the body.
    pub parent: Option<usize>,
    /// The role path from the parent statement to the block (`then`, `arm[0].body`).
    pub block: String,
    /// The position in the block.
    pub index: usize,
    /// The end of the descendants: the statements `index + 1 .. end` of the list are inside.
    pub end: usize,
    pub leaf: bool,
    pub depth: usize,
    /// The compare text (on the old side: with the renames of the claims and the local renames).
    pub key: String,
    /// The header of a composite (the text without the statements of its blocks).
    pub header: String,
    /// The words of the header for a similarity: the kind and the texts of the parts, with no
    /// markup.
    pub words: String,
}

/// A variable that the step extracts or inlines.
#[derive(Clone, Debug)]
pub(crate) struct Variable {
    pub var: String,
    /// The compare text of the expression (with the renames on the old side).
    pub expr: String,
    /// The statement of the `let`: on the new side for an extract, on the old side for an inline.
    pub stmt: usize,
    pub inline: bool,
}

/// The map of the statements of one body.
pub(crate) struct BodyMap<'a> {
    pub old: Vec<Stmt<'a>>,
    pub new: Vec<Stmt<'a>>,
    pub pair_old: Vec<Option<usize>>,
    pub pair_new: Vec<Option<usize>>,
    pub old_at: HashMap<*const Node, usize>,
    pub new_at: HashMap<*const Node, usize>,
    /// The local renames: old name → new name.
    pub renames: BTreeMap<String, String>,
    /// The extracted and the inlined variables with one use or more.
    pub variables: Vec<Variable>,
    /// The pairs of round 3 and of the composite step.
    pub round3: usize,
    pub composites: usize,
    /// R72.1-3: the score of each pair by similarity (round 3 and the composites), by the index
    /// of the old statement. A moved statement of such a pair says so.
    pub weak: HashMap<usize, f64>,
}

impl BodyMap<'_> {
    /// The old text of a node with the local renames (`text` has the renames of the claims).
    pub fn old_text(&self, text: &str) -> String {
        rename_tokens(text, &self.renames)
    }

    /// The variable of an extract use (`old` is `E`, `new` is `v`) or an inline use (`old` is
    /// `v`, `new` is `E`), if the pair is one.
    pub fn variable_use(&self, old_text: &str, old: &Node, new: &Node) -> Option<usize> {
        self.variables.iter().position(|variable| {
            if variable.inline {
                old.kind == "path" && same_expr(&old.text, &variable.var) && same_expr(&new.text, &variable.expr)
            } else {
                new.kind == "path" && same_expr(&new.text, &variable.var) && same_expr(old_text, &variable.expr)
            }
        })
    }
}

/// True if a text is the expression, also in parentheses.
fn same_expr(text: &str, expr: &str) -> bool {
    text == expr || text.strip_prefix('(').and_then(|rest| rest.strip_suffix(')')).is_some_and(|inner| inner.trim() == expr)
}

/// The tokens of a compare text: the parts between spaces, with each bracket as its own token
/// (the compare text has no space inside `()` and `[]`).
pub(crate) fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for piece in text.split(' ') {
        let mut start = 0;
        for (at, c) in piece.char_indices() {
            if matches!(c, '(' | ')' | '[' | ']') {
                if start < at {
                    out.push(&piece[start..at]);
                }
                out.push(&piece[at..at + 1]);
                start = at + 1;
            }
        }
        if start < piece.len() {
            out.push(&piece[start..]);
        }
    }
    out
}

fn is_ident(token: &str) -> bool {
    token.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') && token.chars().all(|c| c.is_alphanumeric() || c == '_')
}

const KEYWORDS: &[&str] = &["mut", "ref", "box", "true", "false", "self", "Self", "crate", "super", "let", "if", "else", "match", "in", "for", "while", "loop", "return", "break", "continue", "as", "move", "async", "await", "unsafe", "const", "static", "fn", "impl", "dyn", "where", "struct", "enum", "type", "use", "pub", "mod", "trait"];

/// The text with each local rename applied to each identifier token. The text keeps its form.
/// A name after `.` is a field or a method, not a local.
pub(crate) fn rename_tokens(text: &str, renames: &BTreeMap<String, String>) -> String {
    if renames.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut previous = "";
    for (index, piece) in text.split(' ').enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let field = previous == "." || previous.ends_with('.');
        let mut word = String::new();
        let mut first = true;
        let flush = |word: &mut String, out: &mut String, first: bool| {
            match renames.get(word.as_str()) {
                Some(new) if !(first && field) => out.push_str(new),
                _ => out.push_str(word),
            }
            word.clear();
        };
        for c in piece.chars() {
            if c.is_alphanumeric() || c == '_' {
                word.push(c);
            } else {
                if !word.is_empty() {
                    flush(&mut word, &mut out, first);
                }
                first = false;
                out.push(c);
            }
        }
        if !word.is_empty() {
            flush(&mut word, &mut out, first);
        }
        previous = piece;
    }
    out
}

/// The names that a pattern binds: each identifier that is not a keyword, not a path, not a
/// type, and not a field label.
fn pattern_names(text: &str, out: &mut BTreeSet<String>) {
    let tokens: Vec<&str> = tokens(text);
    let mut depth = 0i32;
    for (index, token) in tokens.iter().enumerate() {
        match *token {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            ":" if depth == 0 => break,
            _ => {}
        }
        let next = tokens.get(index + 1).copied().unwrap_or("");
        let prev = if index > 0 { tokens[index - 1] } else { "" };
        let lower = token.chars().next().is_some_and(|c| c.is_lowercase() || c == '_');
        if is_ident(token) && lower && *token != "_" && !KEYWORDS.contains(token) && !matches!(next, "(" | "{" | ":" | "::" | "!") && prev != "::" {
            out.insert(token.to_string());
        }
    }
}

/// The names that the patterns and the closure parameters of a body bind.
fn bindings(body: &Node) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    body.walk(&mut Vec::new(), &mut |_, node| {
        if node.kind == "pattern" {
            pattern_names(&node.text, &mut out);
        }
        if matches!(node.kind, "closure" | "closure_block")
            && let Some(params) = &node.attr
        {
            for param in params.split(", ") {
                pattern_names(param, &mut out);
            }
        }
    });
    out
}

/// The statements of a body in preorder.
fn collect(body: &Node) -> Vec<Stmt<'_>> {
    let mut out = Vec::new();
    block_stmts(body, &[], None, "", 0, &mut out);
    out
}

fn block_stmts<'a>(block: &'a Node, prefix: &[String], parent: Option<usize>, block_path: &str, depth: usize, out: &mut Vec<Stmt<'a>>) {
    for (index, child) in block.children.iter().enumerate() {
        let mut path = prefix.to_vec();
        path.push(child.role.clone());
        let at = out.len();
        out.push(Stmt { node: child, path: path.clone(), parent, block: block_path.to_string(), index, end: at + 1, leaf: !child.has_block(), depth, key: String::new(), header: String::new(), words: String::new() });
        inner_blocks(child, &path, &mut Vec::new(), at, depth + 1, out);
        out[at].end = out.len();
    }
}

fn inner_blocks<'a>(node: &'a Node, path: &[String], rel: &mut Vec<String>, owner: usize, depth: usize, out: &mut Vec<Stmt<'a>>) {
    for child in &node.children {
        rel.push(child.role.clone());
        if child.kind == "block" {
            let mut full = path.to_vec();
            full.extend(rel.iter().cloned());
            block_stmts(child, &full, Some(owner), &rel.join("/"), depth, out);
        } else {
            inner_blocks(child, path, rel, owner, depth, out);
        }
        rel.pop();
    }
}

/// The header of a node: its text without the statements of its blocks. The parts have spaces
/// around them, so a token replacement works on the header too.
fn header(node: &Node, text: &dyn Fn(&str) -> String) -> String {
    if node.kind == "block" {
        return "{▫}".into();
    }
    if !node.has_block() {
        return text(&node.text);
    }
    let parts: Vec<String> = node.children.iter().map(|child| format!("{} = {}", child.role, header(child, text))).collect();
    format!("{} < {} > ( {} )", node.kind, node.attr.as_deref().map(text).unwrap_or_default(), parts.join(" ; "))
}

/// The text of a node with the text of each block replaced by `{▫}`: the part of a composite
/// that the statements of its blocks do not have.
pub(crate) fn shell(node: &Node) -> String {
    if node.kind == "block" {
        return "{▫}".into();
    }
    let mut text = node.text.clone();
    let mut from = 0;
    for child in node.children.iter().filter(|child| child.has_block()) {
        if let Some(found) = text[from..].find(&child.text) {
            let inner = shell(child);
            let start = from + found;
            text.replace_range(start..start + child.text.len(), &inner);
            from = start + inner.len();
        }
    }
    text
}

/// The words of a header: the kind and the texts of the parts that have no block.
fn header_words(node: &Node, text: &dyn Fn(&str) -> String) -> String {
    if node.kind == "block" {
        return String::new();
    }
    if !node.has_block() {
        return text(&node.text);
    }
    let mut out = vec![node.kind.to_string()];
    out.extend(node.attr.as_deref().map(text));
    out.extend(node.children.iter().map(|child| header_words(child, text)).filter(|words| !words.is_empty()));
    out.join(" ")
}

/// The length of the longest common subsequence of two token lists.
fn lcs_len(a: &[&str], b: &[&str]) -> usize {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return 0;
    }
    if n * m > 250_000 {
        // A long pair: the common tokens as a multiset.
        let mut counts: HashMap<&str, i64> = HashMap::new();
        for token in a {
            *counts.entry(token).or_default() += 1;
        }
        return b.iter().filter(|token| counts.get_mut(*token).is_some_and(|count| { *count -= 1; *count >= 0 })).count();
    }
    let mut row = vec![0usize; m + 1];
    for token in a {
        let mut diagonal = 0;
        for j in 0..m {
            let up = row[j + 1];
            row[j + 1] = if *token == b[j] { diagonal + 1 } else { up.max(row[j]) };
            diagonal = up;
        }
    }
    row[m]
}

/// The share of tokens that need no replacement (0 to 1).
fn score(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<&str>, Vec<&str>) = (tokens(a), tokens(b));
    let longest = a.len().max(b.len());
    if longest == 0 { 1.0 } else { lcs_len(&a, &b) as f64 / longest as f64 }
}

/// The tokens of a node outside its children.
fn own_tokens(node: &Node) -> usize {
    let inner: usize = node.children.iter().map(|child| tokens(&child.text).len()).sum();
    tokens(&node.text).len().saturating_sub(inner)
}

/// The count of the tokens of a node without the statements of its blocks.
fn size(node: &Node) -> usize {
    if node.kind == "block" {
        return 0;
    }
    if node.children.is_empty() {
        return tokens(&node.text).len();
    }
    own_tokens(node) + node.children.iter().map(size).sum::<usize>()
}

/// A candidate of an extract or an inline for the structural score: `(var, expr, inline)`.
type Candidate = (String, String, bool);

/// The count of the tokens that need no replacement in a node pair (design 41.3), without the
/// statements of the blocks. The tokens inside a replaced sub-expression are replaced tokens.
/// Exceptions: a use of an extract or inline candidate (`E` and `v`) and a consistent rename of a
/// local (`old_text` applies it) count as equal.
fn matched(old: &Node, new: &Node, old_text: &dyn Fn(&str) -> String, variables: &[Candidate]) -> usize {
    if old.kind == "block" || new.kind == "block" {
        return 0;
    }
    let text = old_text(&old.text);
    if text == new.text {
        return size(old).min(size(new));
    }
    let variable = variables.iter().any(|(var, expr, inline)| {
        if *inline {
            old.kind == "path" && same_expr(&text, var) && same_expr(&new.text, expr)
        } else {
            new.kind == "path" && same_expr(&new.text, var) && same_expr(&text, expr)
        }
    });
    if variable {
        return size(old).max(size(new));
    }
    // A leaf with a different text, or a different form: each token is replaced.
    if old.kind != new.kind || old.children.is_empty() || old.children.len() != new.children.len() || old.children.iter().zip(&new.children).any(|(a, b)| a.role != b.role) {
        return 0;
    }
    let attr_changed = old.attr.as_deref().map(old_text) != new.attr;
    let own = own_tokens(old).min(own_tokens(new)).saturating_sub(usize::from(attr_changed));
    own + old.children.iter().zip(&new.children).map(|(a, b)| matched(a, b, old_text, variables)).sum::<usize>()
}

/// The structural score of a node pair: the share of the tokens that need no replacement (0 to
/// 1), without the statements of the blocks.
fn structural(old: &Node, new: &Node, old_text: &dyn Fn(&str) -> String, variables: &[Candidate]) -> f64 {
    let longest = size(old).max(size(new));
    if longest == 0 { 1.0 } else { matched(old, new, old_text, variables) as f64 / longest as f64 }
}

/// The share of equal words (names and literals, not keywords) of two texts (0 to 1). Each
/// word that is a candidate variable counts as the words of its expression (on the new side for
/// an extract, on the old side for an inline).
fn word_score(a: &str, b: &str, variables: &[Candidate]) -> f64 {
    let word = |token: &&str| token.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '"' || c == '\'') && !KEYWORDS.contains(token);
    let words = |text: &str, inline: bool| -> Vec<String> {
        let mut out = Vec::new();
        for token in tokens(text).into_iter().filter(word) {
            match variables.iter().find(|(var, _, side)| *side == inline && var == token) {
                Some((_, expr, _)) => out.extend(tokens(expr).into_iter().filter(word).map(str::to_string)),
                None => out.push(token.to_string()),
            }
        }
        out
    };
    let (a, b) = (words(a, true), words(b, false));
    let (a, b): (Vec<&str>, Vec<&str>) = (a.iter().map(String::as_str).collect(), b.iter().map(String::as_str).collect());
    let longest = a.len().max(b.len());
    if longest == 0 { 1.0 } else { lcs_len(&a, &b) as f64 / longest as f64 }
}

/// The address of a node: the key of the node maps.
pub(crate) fn at(node: &Node) -> *const Node {
    node as *const Node
}

struct Mapper<'a, 'f> {
    /// The old text of a node with the renames of the claims and the local renames.
    old_text: &'f dyn Fn(&str) -> String,
    old: Vec<Stmt<'a>>,
    new: Vec<Stmt<'a>>,
    pair_old: Vec<Option<usize>>,
    pair_new: Vec<Option<usize>>,
    budget: usize,
    /// The counts of the pairs of round 3 and of the composite step (design 41.8).
    round3: usize,
    composites: usize,
    /// The score of each pair by similarity, by the old index (R72.1-3).
    weak: HashMap<usize, f64>,
}

impl Mapper<'_, '_> {
    fn map(&mut self, o: usize, n: usize) {
        self.pair_old[o] = Some(n);
        self.pair_new[n] = Some(o);
        // Two composites with the same text: the statements inside map in order.
        let (old_inner, new_inner) = (o + 1..self.old[o].end, n + 1..self.new[n].end);
        if self.old[o].key == self.new[n].key && old_inner.len() == new_inner.len() {
            for (x, y) in old_inner.zip(new_inner) {
                if self.pair_old[x].is_none() && self.pair_new[y].is_none() {
                    self.pair_old[x] = Some(y);
                    self.pair_new[y] = Some(x);
                }
            }
        }
    }

    /// The parent key of a statement: the header of its parent and the block, for round 1.
    fn parent_key(stmts: &[Stmt], index: usize) -> String {
        let stmt = &stmts[index];
        match stmt.parent {
            Some(parent) => format!("{}|{}", stmts[parent].header, stmt.block),
            None => format!("|{}", stmt.block),
        }
    }

    /// True if the parents of two statements map to each other, or both are the body.
    fn parents_map(&self, o: usize, n: usize) -> bool {
        match (self.old[o].parent, self.new[n].parent) {
            (None, None) => true,
            (Some(p), Some(q)) => self.pair_old[p] == Some(q) && self.old[o].block == self.new[n].block,
            _ => false,
        }
    }

    fn distance(&self, o: usize, n: usize) -> usize {
        self.old[o].index.abs_diff(self.new[n].index) + self.old[o].depth.abs_diff(self.new[n].depth)
    }

    /// Round 1: the same text, with corresponding parents. The longest common subsequence of
    /// each pair of parent groups first, then the nearest pair.
    fn round_same_parent(&mut self) {
        let mut groups: BTreeMap<String, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
        for o in 0..self.old.len() {
            groups.entry(Self::parent_key(&self.old, o)).or_default().0.push(o);
        }
        for n in 0..self.new.len() {
            groups.entry(Self::parent_key(&self.new, n)).or_default().1.push(n);
        }
        for (old_list, new_list) in groups.values() {
            if old_list.is_empty() || new_list.is_empty() {
                continue;
            }
            if old_list.len() * new_list.len() <= 1_000_000 {
                let pairs = crate::diff::lcs(old_list, new_list, |a, b| self.old[*a].key == self.new[*b].key);
                for (i, j) in pairs {
                    let (o, n) = (old_list[i], new_list[j]);
                    if self.pair_old[o].is_none() && self.pair_new[n].is_none() {
                        self.map(o, n);
                    }
                }
            }
            self.nearest(old_list, new_list);
        }
    }

    /// Maps the pairs with equal texts of two lists, the nearest pair first.
    fn nearest(&mut self, old_list: &[usize], new_list: &[usize]) {
        let mut by_key: BTreeMap<&str, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
        for &o in old_list.iter().filter(|&&o| self.pair_old[o].is_none()) {
            by_key.entry(self.old[o].key.as_str()).or_default().0.push(o);
        }
        for &n in new_list.iter().filter(|&&n| self.pair_new[n].is_none()) {
            if let Some(entry) = by_key.get_mut(self.new[n].key.as_str()) {
                entry.1.push(n);
            }
        }
        let mut candidates = Vec::new();
        for (olds, news) in by_key.values() {
            if olds.len() * news.len() > 10_000 {
                candidates.extend(olds.iter().zip(news).map(|(o, n)| (0, 0, *o, *n)));
                continue;
            }
            for &o in olds {
                for &n in news {
                    // A larger subtree first, then the pair with the more similar parents, then
                    // the nearest pair.
                    let size = self.old[o].end - o;
                    let parents = match (self.old[o].parent, self.new[n].parent) {
                        (None, None) => 1.0,
                        (Some(p), Some(q)) if self.pair_old[p] == Some(q) => 1.0,
                        (Some(p), Some(q)) => score(&self.old[p].words, &self.new[q].words),
                        _ => 0.0,
                    };
                    candidates.push((usize::MAX - size, 1000 - (parents * 1000.0) as usize, o, n));
                }
            }
        }
        candidates.sort_by_key(|&(size, parents, o, n)| (size, parents, self.distance(o, n), o, n));
        for (_, _, o, n) in candidates {
            if self.pair_old[o].is_none() && self.pair_new[n].is_none() {
                self.map(o, n);
            }
        }
    }

    /// Round 3: the leaf pairs with the best similarity after the replacements, by the one
    /// matcher (R72.1-1, part c): the pairs of rounds 1 and 2 (equal texts) are the stronger
    /// pairs, the score of design 41 is the similarity, and a pair that crosses a stronger pair
    /// of the same blocks or that goes to a different block needs a margin (R72.1-2). In a table
    /// of rows of one form, an equal row is as similar as the other row: a row at a new place with
    /// other data is a removed row and an added row, not a move. `variables`: the extract and
    /// inline candidates (`(var, expr, inline)`): a use counts as equal.
    fn round_replacements(&mut self, variables: &[Candidate], skip_old: &BTreeSet<usize>, skip_new: &BTreeSet<usize>) -> bool {
        let olds: Vec<usize> = (0..self.old.len()).filter(|&o| self.old[o].leaf && self.pair_old[o].is_none() && !skip_old.contains(&o)).collect();
        let news: Vec<usize> = (0..self.new.len()).filter(|&n| self.new[n].leaf && self.pair_new[n].is_none() && !skip_new.contains(&n)).collect();
        let pairs = olds.len() * news.len();
        if pairs > self.budget {
            return false;
        }
        self.budget -= pairs;
        if olds.is_empty() || news.is_empty() {
            return true;
        }
        let (free_old, free_new): (BTreeSet<usize>, BTreeSet<usize>) = (olds.iter().copied().collect(), news.iter().copied().collect());
        let group = |leaf: bool, free: bool, paired: bool| if leaf && (free || paired) { "leaf" } else { "other" };
        let old_things: Vec<Thing> = (0..self.old.len()).map(|o| Thing { group: group(self.old[o].leaf, free_old.contains(&o), self.pair_old[o].is_some()).into(), identity: None, content: None }).collect();
        let new_things: Vec<Thing> = (0..self.new.len()).map(|n| Thing { group: group(self.new[n].leaf, free_new.contains(&n), self.pair_new[n].is_some()).into(), identity: None, content: None }).collect();
        let fixed: Vec<Pair> = (0..self.old.len()).filter_map(|o| self.pair_old[o].map(|n| Pair { old: o, new: n, key: Key::Content })).collect();
        let cache: std::cell::RefCell<HashMap<(usize, usize), f64>> = Default::default();
        let similarity = |o: usize, n: usize| -> f64 {
            if let Some(found) = cache.borrow().get(&(o, n)) {
                return *found;
            }
            let found = if !self.old[o].leaf || !self.new[n].leaf || !crate::diff::same_kind(self.old[o].node, self.new[n].node) || word_score(&self.old[o].key, &self.new[n].key, variables) < MIN_WORDS {
                0.0
            } else {
                let score = structural(self.old[o].node, self.new[n].node, self.old_text, variables);
                // A pair whose parents correspond is preferred: a pair at a different place needs
                // a clearly higher score (design 41.2, D2 of fix round 1).
                let local = self.local(o, n);
                if score >= MIN_SCORE && (local || score >= FAR_SCORE) { score + if local { LOCAL_BONUS } else { 0.0 } } else { 0.0 }
            };
            cache.borrow_mut().insert((o, n), found);
            found
        };
        let in_order = |pair: (usize, usize), other: (usize, usize)| self.in_order(pair, other);
        let ladder = Ladder { similarity: &similarity, threshold: MIN_SCORE, in_order: Some(&in_order) };
        let found = semilla_core::matcher::pair_with(&old_things, &new_things, &ladder, &fixed);
        let made: Vec<(usize, usize, f64)> = found.iter().filter_map(|pair| if let Key::Similarity(score) = pair.key { Some((pair.old, pair.new, score)) } else { None }).collect();
        for (o, n, score) in made {
            if self.pair_old[o].is_none() && self.pair_new[n].is_none() {
                self.map(o, n);
                self.round3 += 1;
                // The score of design 41 without the bonus of a local pair.
                self.weak.insert(o, if self.local(o, n) { score - LOCAL_BONUS } else { score });
            }
        }
        true
    }

    /// R72.1-2: true if the pair `pair` keeps the order of the stronger pair `other`. A pair whose
    /// parents correspond crosses only a pair of the same two blocks in the other order. A pair
    /// at a different block (a move to that block) crosses each pair of the block that it leaves
    /// and of the block that it enters.
    fn in_order(&self, pair: (usize, usize), other: (usize, usize)) -> bool {
        let ((o, n), (a, b)) = (pair, other);
        let same_old = self.old[a].parent == self.old[o].parent && self.old[a].block == self.old[o].block;
        let same_new = self.new[b].parent == self.new[n].parent && self.new[b].block == self.new[n].block;
        if !self.local(o, n) {
            return !same_old && !same_new;
        }
        !(same_old && same_new) || (self.old[a].index < self.old[o].index) == (self.new[b].index < self.new[n].index)
    }

    /// True if the parents of two statements correspond: they map to each other, or they have
    /// the same header and the same block (or both are the body).
    fn local(&self, o: usize, n: usize) -> bool {
        self.parents_map(o, n) || Self::parent_key(&self.old, o) == Self::parent_key(&self.new, n)
    }

    /// The composites: the same kind and child roles, an equal or similar header, and a large
    /// share of mapped children. The one matcher (R72.1-1, part c) takes the pairs with the best
    /// sum of the share and the header score.
    fn round_composites(&mut self, variables: &[Candidate]) -> bool {
        for _ in 0..2 {
            let olds: BTreeSet<usize> = (0..self.old.len()).filter(|&o| !self.old[o].leaf && self.pair_old[o].is_none()).collect();
            let news: BTreeSet<usize> = (0..self.new.len()).filter(|&n| !self.new[n].leaf && self.pair_new[n].is_none()).collect();
            let pairs = olds.len() * news.len();
            if pairs > self.budget {
                return false;
            }
            self.budget -= pairs;
            if olds.is_empty() || news.is_empty() {
                continue;
            }
            fn roles(node: &Node) -> Vec<&str> {
                node.children.iter().map(|child| child.role.as_str()).collect()
            }
            let group = |leaf: bool, free: bool, paired: bool| if !leaf && (free || paired) { "composite" } else { "other" };
            let old_things: Vec<Thing> = (0..self.old.len()).map(|o| Thing { group: group(self.old[o].leaf, olds.contains(&o), self.pair_old[o].is_some()).into(), identity: None, content: None }).collect();
            let new_things: Vec<Thing> = (0..self.new.len()).map(|n| Thing { group: group(self.new[n].leaf, news.contains(&n), self.pair_new[n].is_some()).into(), identity: None, content: None }).collect();
            let fixed: Vec<Pair> = (0..self.old.len()).filter_map(|o| self.pair_old[o].map(|n| Pair { old: o, new: n, key: Key::Content })).collect();
            let similarity = |o: usize, n: usize| -> f64 {
                let (a, b) = (&self.old[o], &self.new[n]);
                if a.leaf || b.leaf || a.node.kind != b.node.kind || roles(a.node) != roles(b.node) {
                    return 0.0;
                }
                let mapped = (o + 1..a.end).filter(|&x| self.pair_old[x].is_some_and(|y| n < y && y < b.end)).count();
                let size = (a.end - o - 1).max(b.end - n - 1);
                let share = if size == 0 { 0.0 } else { mapped as f64 / size as f64 };
                // The header score counts the tokens of a replaced part as replaced (design
                // 41.3), as in round 3.
                let similar = if a.header == b.header { 1.0 } else { structural(a.node, b.node, self.old_text, variables) };
                if share >= MIN_SHARE && similar >= MIN_HEADER { share + similar } else { 0.0 }
            };
            let in_order = |pair: (usize, usize), other: (usize, usize)| self.in_order(pair, other);
            let ladder = Ladder { similarity: &similarity, threshold: MIN_SHARE + MIN_HEADER, in_order: Some(&in_order) };
            let found = semilla_core::matcher::pair_with(&old_things, &new_things, &ladder, &fixed);
            for pair in found {
                let Key::Similarity(score) = pair.key else { continue };
                if self.pair_old[pair.old].is_none() && self.pair_new[pair.new].is_none() {
                    self.pair_old[pair.old] = Some(pair.new);
                    self.pair_new[pair.new] = Some(pair.old);
                    self.composites += 1;
                    // The share and the header score, each from 0 to 1.
                    self.weak.insert(pair.old, score / 2.0);
                }
            }
        }
        true
    }
}

/// The `let v = E;` of a statement: the name and the compare text of `E`. Only a plain name
/// with no type and no `else`.
fn simple_let(node: &Node) -> Option<(String, &Node)> {
    if node.kind != "let" || node.children.len() != 2 {
        return None;
    }
    let (pat, init) = (&node.children[0], &node.children[1]);
    (pat.role == "pat" && init.role == "init" && is_ident(&pat.text) && !KEYWORDS.contains(&pat.text.as_str()) && pat.text != "_").then(|| (pat.text.clone(), init))
}

/// Maps the statements of two bodies. `old_text` applies the renames of the claims to an old
/// text. `None` if the body is above the limit of [`MAX_SCORED`].
pub(crate) fn map_body<'a>(old_body: &'a Node, new_body: &'a Node, old_text: &dyn Fn(&str) -> String) -> Option<BodyMap<'a>> {
    let mut old = collect(old_body);
    let mut new = collect(new_body);
    let mut budget = MAX_SCORED;

    // The local renames: the votes of the leaf pairs whose only differences are names that the
    // two sides bind. A rename holds only if the old name is not in the new body and the new
    // name is not in the old body: each use is renamed.
    let (old_bound, new_bound) = (bindings(old_body), bindings(new_body));
    let old_words: BTreeSet<String> = tokens(&old_text(&old_body.text)).into_iter().filter(|t| is_ident(t)).map(str::to_string).collect();
    let new_words: BTreeSet<&str> = tokens(&new_body.text).into_iter().filter(|t| is_ident(t)).collect();
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    let old_keys: Vec<String> = old.iter().map(|stmt| old_text(if stmt.leaf { &stmt.node.text } else { "" })).collect();
    {
        // Only the statements whose text the other side does not have can vote.
        let old_texts: BTreeSet<&str> = old_keys.iter().map(String::as_str).collect();
        let new_texts: BTreeSet<&str> = new.iter().filter(|stmt| stmt.leaf).map(|stmt| stmt.node.text.as_str()).collect();
        let mut groups: BTreeMap<(&str, usize), (Vec<Vec<&str>>, Vec<Vec<&str>>)> = BTreeMap::new();
        for (o, stmt) in old.iter().enumerate().filter(|(o, stmt)| stmt.leaf && !new_texts.contains(old_keys[*o].as_str())) {
            let list: Vec<&str> = tokens(&old_keys[o]);
            groups.entry((stmt.node.kind, list.len())).or_default().0.push(list);
        }
        for stmt in new.iter().filter(|stmt| stmt.leaf && !old_texts.contains(stmt.node.text.as_str())) {
            let list: Vec<&str> = tokens(&stmt.node.text);
            if let Some(entry) = groups.get_mut(&(stmt.node.kind, list.len())) {
                entry.1.push(list);
            }
        }
        let mut votes: BTreeMap<(String, String), usize> = BTreeMap::new();
        for (olds, news) in groups.values() {
            let pairs = olds.len() * news.len();
            if pairs > budget {
                return None;
            }
            budget -= pairs;
            for a in olds {
                for b in news {
                    let differ: Vec<(&str, &str)> = a.iter().zip(b).filter(|(x, y)| x != y).map(|(x, y)| (*x, *y)).collect();
                    if !differ.is_empty() && differ.iter().all(|(x, y)| old_bound.contains(*x) && new_bound.contains(*y)) {
                        for (x, y) in differ {
                            *votes.entry((x.to_string(), y.to_string())).or_default() += 1;
                        }
                    }
                }
            }
        }
        let mut ranked: Vec<((String, String), usize)> = votes.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut used: BTreeSet<String> = BTreeSet::new();
        for ((x, y), _) in ranked {
            if renames.contains_key(&x) || used.contains(&y) || new_words.contains(x.as_str()) || old_words.contains(&y) {
                continue;
            }
            used.insert(y.clone());
            renames.insert(x, y);
        }
    }
    let full_old = |text: &str| rename_tokens(&old_text(text), &renames);
    for stmt in old.iter_mut() {
        stmt.key = full_old(&stmt.node.text);
        stmt.header = header(stmt.node, &full_old);
        stmt.words = header_words(stmt.node, &full_old);
    }
    for stmt in new.iter_mut() {
        stmt.key = stmt.node.text.clone();
        stmt.header = header(stmt.node, &|text: &str| text.to_string());
        stmt.words = header_words(stmt.node, &|text: &str| text.to_string());
    }
    let (n_old, n_new) = (old.len(), new.len());
    let mut mapper = Mapper { old_text: &full_old, old, new, pair_old: vec![None; n_old], pair_new: vec![None; n_new], budget, round3: 0, composites: 0, weak: HashMap::new() };

    // Round 1 and round 2.
    mapper.round_same_parent();
    let (all_old, all_new): (Vec<usize>, Vec<usize>) = ((0..n_old).collect(), (0..n_new).collect());
    mapper.nearest(&all_old, &all_new);

    // The candidates of an extract and an inline: a `let v = E;` with no partner, whose name
    // the other side does not have.
    let mut candidates: Vec<Variable> = Vec::new();
    for (n, stmt) in mapper.new.iter().enumerate() {
        if let Some((var, init)) = simple_let(stmt.node).filter(|_| mapper.pair_new[n].is_none())
            && !old_words.contains(&var)
        {
            candidates.push(Variable { var, expr: init.text.clone(), stmt: n, inline: false });
        }
    }
    for (o, stmt) in mapper.old.iter().enumerate() {
        if let Some((var, init)) = simple_let(stmt.node).filter(|_| mapper.pair_old[o].is_none())
            && !new_words.contains(var.as_str())
            && !renames.contains_key(&var)
        {
            candidates.push(Variable { var, expr: full_old(&init.text), stmt: o, inline: true });
        }
    }
    // The `let` of a candidate takes part in round 3 as each other statement: a candidate that
    // maps to a statement is not a variable (fix round 1, D3: `let enum_name = &self.name;`
    // was left out, so `let name = &self.name;` mapped to a different statement).
    let substitution: Vec<Candidate> = candidates.iter().map(|v| (v.var.clone(), v.expr.clone(), v.inline)).collect();
    let none = BTreeSet::new();
    if !mapper.round_replacements(&substitution, &none, &none) || !mapper.round_composites(&substitution) {
        return None;
    }
    // A candidate with no partner and with a use in a mapped pair is an extracted or an inlined
    // variable. Each other statement with no partner goes to a second round 3.
    let mut variables = Vec::new();
    for variable in candidates.into_iter().filter(|v| if v.inline { mapper.pair_old[v.stmt].is_none() } else { mapper.pair_new[v.stmt].is_none() }) {
        let count: usize = (0..n_old)
            .filter_map(|o| mapper.pair_old[o].map(|n| (o, n)))
            // Only the parts of the statement itself: the statements inside have their own pairs.
            .map(|(o, n)| shallow_uses(mapper.old[o].node, mapper.new[n].node, &full_old, &variable))
            .sum();
        if count > 0 {
            variables.push(variable);
        }
    }
    let skip_old: BTreeSet<usize> = variables.iter().filter(|v| v.inline).map(|v| v.stmt).collect();
    let skip_new: BTreeSet<usize> = variables.iter().filter(|v| !v.inline).map(|v| v.stmt).collect();
    if !mapper.round_replacements(&[], &skip_old, &skip_new) || !mapper.round_composites(&[]) {
        return None;
    }
    let old_at = mapper.old.iter().enumerate().map(|(i, stmt)| (at(stmt.node), i)).collect();
    let new_at = mapper.new.iter().enumerate().map(|(i, stmt)| (at(stmt.node), i)).collect();
    let Mapper { old, new, pair_old, pair_new, round3, composites, weak, .. } = mapper;
    Some(BodyMap { old, new, pair_old, pair_new, old_at, new_at, renames, variables, round3, composites, weak })
}

/// The uses of a variable in a statement pair, without the statements of the inner blocks.
fn shallow_uses(old: &Node, new: &Node, old_text: &dyn Fn(&str) -> String, variable: &Variable) -> usize {
    if old.kind == "block" || new.kind == "block" {
        return 0;
    }
    let text = old_text(&old.text);
    if variable.inline {
        if old.kind == "path" && same_expr(&old.text, &variable.var) && same_expr(&new.text, &variable.expr) {
            return 1;
        }
    } else if new.kind == "path" && same_expr(&new.text, &variable.var) && same_expr(&text, &variable.expr) {
        return 1;
    }
    if text == new.text || old.kind != new.kind || old.children.len() != new.children.len() {
        return 0;
    }
    old.children.iter().zip(&new.children).filter(|(a, b)| a.role == b.role).map(|(a, b)| shallow_uses(a, b, old_text, variable)).sum()
}

#[cfg(test)]
mod tests {
    use crate::diff::{Refactor, Renames, diff_maps};

    const MANIFEST: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";

    /// The edits of a step: `KIND before → after` with the mark of the refactoring.
    fn edits(before: &str, after: &str) -> (Vec<String>, crate::BodyMaps) {
        let (_d1, old) = crate::tests::workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", before)]);
        let (_d2, new) = crate::tests::workspace(&[("Cargo.toml", MANIFEST), ("src/lib.rs", after)]);
        let (edits, maps) = diff_maps(&old, &new, &Renames::default());
        let text = |anchor: &Option<semilla_core::Anchor>| anchor.as_ref().map(|a| a.to_string()).unwrap_or_else(|| "-".into());
        let mark = |refactor: &Option<Refactor>| match refactor {
            Some(Refactor::Moved { .. }) => " (move)".to_string(),
            Some(Refactor::Extract { var, .. }) => format!(" (extract {var})"),
            Some(Refactor::Inline { var, .. }) => format!(" (inline {var})"),
            Some(Refactor::Arm { .. }) => " (arm)".to_string(),
            None => String::new(),
        };
        (edits.iter().map(|e| format!("{:?} {} → {}{}", e.kind, text(&e.before), text(&e.after), mark(&e.refactor))).collect(), maps)
    }

    fn body(lines: &str) -> String {
        format!("pub fn f(a: u32, b: u32, on: bool, v: &mut Vec<u32>) -> u32 {{\n{lines}}}\n")
    }

    #[test]
    fn a_statement_that_moves_into_or_out_of_a_block_is_one_move() {
        // Design 41.1 and 41.6: round 2 maps the same text at any depth. The `if` maps by its
        // header and its children, so it gets no edit of its own.
        let out = body("    let mut s = 0;\n    s += a * 2;\n    if b > 3 {\n        s += b;\n    }\n    s\n");
        let into = body("    let mut s = 0;\n    if b > 3 {\n        s += a * 2;\n        s += b;\n    }\n    s\n");
        assert_eq!(edits(&out, &into).0, ["Moved demo::f/S1 → demo::f/S1/then/S0 (move)"]);
        assert_eq!(edits(&into, &out).0, ["Moved demo::f/S1/then/S0 → demo::f/S1 (move)"]);
        // Into and out of a closure body, a loop, and a match arm.
        let closure = |inside: bool| if inside { body("    v.iter().map(|x| {\n        let k = a * 2;\n        x + k\n    }).sum()\n") } else { body("    let k = a * 2;\n    v.iter().map(|x| {\n        x + k\n    }).sum()\n") };
        assert_eq!(edits(&closure(false), &closure(true)).0, ["Moved demo::f/S0 → demo::f/tail/recv/args[0]/body/S0 (move)"]);
        assert_eq!(edits(&closure(true), &closure(false)).0, ["Moved demo::f/tail/recv/args[0]/body/S0 → demo::f/S0 (move)"]);
        let arm = |inside: bool| body(&format!("    let mut s = b;\n{}    match a % 3 {{\n        0 => {{\n{}            s *= 2;\n        }}\n        _ => {{\n            s += 1;\n        }}\n    }}\n    s\n", if inside { "" } else { "    s += a;\n" }, if inside { "            s += a;\n" } else { "" }));
        assert_eq!(edits(&arm(false), &arm(true)).0, ["Moved demo::f/S1 → demo::f/S1/arm[0].body/S0 (move)"]);
    }

    #[test]
    fn a_swap_and_a_move_down_are_one_move_each() {
        // Design 41.6: the statements in order are in place; the other mapped statement moved.
        let before = body("    let x = a + 1;\n    let y = b * 2;\n    x + y\n");
        let after = body("    let y = b * 2;\n    let x = a + 1;\n    x + y\n");
        assert_eq!(edits(&before, &after).0, ["Moved demo::f/S0 → demo::f/S1 (move)"]);
        let before = body("    let p = a + 1;\n    let q = b + 2;\n    let r = a * b;\n    let t = q + r;\n    p + t\n");
        let after = body("    let q = b + 2;\n    let r = a * b;\n    let t = q + r;\n    let p = a + 1;\n    p + t\n");
        let (found, maps) = edits(&before, &after);
        assert_eq!(found, ["Moved demo::f/S0 → demo::f/S3 (move)"]);
        assert_eq!(maps.maps.len(), 1);
        assert!(maps.maps[0].moved);
    }

    #[test]
    fn a_moved_statement_that_changes_gets_the_move_and_an_edit_for_the_change() {
        // Design 41.3 and 41.6: round 3 maps the statement with a score; the change is its own
        // edit, at the place before the step.
        let before = body("    let p = a + 1;\n    let q = b + 2;\n    let r = a * b;\n    p + q + r\n");
        let after = body("    let q = b + 2;\n    let r = a * b;\n    let p = a + 2;\n    p + q + r\n");
        assert_eq!(edits(&before, &after).0, ["Moved demo::f/S0 → demo::f/S2 (move)", "Replaced demo::f/S0/init/rhs → demo::f/S2/init/rhs"]);
    }

    #[test]
    fn a_consistent_rename_of_a_local_maps_in_each_round() {
        // Design 41.4: the rename map makes `let total = a + b;` and `let sum = a + b;` the
        // same statement. The move and the rename sites are edits.
        let before = body("    let total = a + b;\n    let m = a * 2;\n    total + m\n");
        let after = body("    let m = a * 2;\n    let sum = a + b;\n    sum + m\n");
        let (found, _) = edits(&before, &after);
        assert_eq!(found, ["Moved demo::f/S0 → demo::f/S1 (move)", "Replaced demo::f/S0/pat → demo::f/S1/pat", "Replaced demo::f/tail/lhs → demo::f/tail/lhs"]);
        // A rename with a different use of the old name is not consistent: no rename map.
        let before = body("    let total = a + b;\n    total * 2\n");
        let after = body("    let sum = a + b;\n    sum * 2 + total(a)\n");
        let (found, _) = edits(&before.replace("pub fn f", "pub fn total(x: u32) -> u32 { x }\npub fn f"), &after.replace("pub fn f", "pub fn total(x: u32) -> u32 { x }\npub fn f"));
        assert!(!found.iter().any(|e| e.contains("(move)")), "{found:?}");
    }

    #[test]
    fn an_extracted_and_an_inlined_variable_have_their_marks() {
        // Design 41.5: a new `let v = E;` whose `E` is `v` in a mapped statement.
        let before = body("    if a * b + 1 > 10 {\n        return a * b + 1;\n    }\n    0\n");
        let after = body("    let area = a * b + 1;\n    if area > 10 {\n        return area;\n    }\n    0\n");
        let extract = ["Inserted - → demo::f/S0 (extract area)", "Replaced demo::f/S0/cond/lhs → demo::f/S1/cond/lhs (extract area)", "Replaced demo::f/S0/then/S0/value → demo::f/S1/then/S0/value (extract area)"];
        assert_eq!(edits(&before, &after).0, extract);
        let inline = ["Replaced demo::f/S1/cond/lhs → demo::f/S0/cond/lhs (inline area)", "Deleted demo::f/S0 → - (inline area)", "Replaced demo::f/S1/then/S0/value → demo::f/S0/then/S0/value (inline area)"];
        assert_eq!(edits(&after, &before).0, inline);
        // A use in parentheses, and one use that also changes in a different way: the change
        // is its own edit at the condition.
        let after = body("    let area = a * b + 1;\n    if area >= 10 {\n        return (area);\n    }\n    0\n");
        let (found, _) = edits(&before.replace("return a * b + 1", "return (a * b + 1)"), &after);
        assert!(found.contains(&"Modified demo::f/S0/cond → demo::f/S1/cond".to_string()) && found.iter().filter(|e| e.contains("(extract area)")).count() == 3, "{found:?}");
    }

    #[test]
    fn a_composite_maps_by_its_header_and_its_children() {
        // Design 41.2: a new `if` before an `if` with a changed condition. The `return` maps to
        // the child of the `if` with the more similar header, so the old `if` maps to the
        // second new `if`, and the first new `if` is new.
        let before = body("    if a > 100 {\n        return 1;\n    }\n    a\n");
        let after = body("    if b == 0 {\n        return 1;\n    }\n    if a >= 100 {\n        return 1;\n    }\n    a\n");
        let (found, _) = edits(&before, &after);
        assert_eq!(found, ["Inserted - → demo::f/S0", "Modified demo::f/S0/cond → demo::f/S1/cond"]);
    }

    #[test]
    fn a_body_above_the_limit_keeps_the_rules_of_batch_8b() {
        // Design 41.9: more than `MAX_SCORED` statement pairs to score.
        let lines = |offset: u32| (0..150).map(|i| format!("    let x{i} = a + {};\n", i + offset)).collect::<String>();
        let (_, maps) = edits(&body(&format!("{}    0\n", lines(0))), &body(&format!("{}    0\n", lines(1))));
        assert_eq!(maps.over_limit, ["demo::f"]);
        let (found, maps) = edits(&body("    let x = a + 1;\n    x\n"), &body("    let x = a + 2;\n    x\n"));
        assert!(maps.over_limit.is_empty() && found == ["Replaced demo::f/S0/init/rhs → demo::f/S0/init/rhs"], "{found:?}");
    }

    #[test]
    fn an_edit_inside_a_moved_statement_has_its_new_place() {
        // Fix round 1, D1: `transpositions /= 2;` moves into the `else` of a new `if`, and its
        // literal changes. The edit of the literal has the new place on its new side.
        let before = "pub fn jaro(a: u32, mut transpositions: u32) -> u32 {\n    let m = a + 1;\n    transpositions /= 2;\n    m + transpositions\n}\n";
        let after = "pub fn jaro(a: u32, mut transpositions: u32) -> u32 {\n    let m = a + 1;\n    if a == 0 {\n        transpositions = 0;\n    } else {\n        transpositions /= 3;\n    }\n    m + transpositions\n}\n";
        assert_eq!(edits(before, after).0, ["Inserted - → demo::jaro/S1", "Moved demo::jaro/S1 → demo::jaro/S1/else/S0 (move)", "Replaced demo::jaro/S1/rhs → demo::jaro/S1/else/S0/rhs"]);
    }

    #[test]
    fn round_three_prefers_the_changed_twin_at_the_same_place() {
        // Fix round 1, D2: the old `return` keeps its place with two changed parts. A similar new
        // `return` in a different block has a higher score, but not by more than `LOCAL_BONUS`.
        let source = |after: bool| format!("pub enum E {{ Invalid {{ name: &'static str, value: String, span_start: Option<u32>, span_end: Option<u32> }} }}\npub fn parse(year: i32, year_span: u32, sign: Option<u32>, week: u8, w_span: u32) -> Result<i32, E> {{\n    let x = 1;\n    if year > 9999 {{\n        return Err(E::Invalid {{ name: \"year\", value: {}, span_start: Some({}), span_end: Some(year_span) }});\n    }}\n{}    Ok(year + x)\n}}\n",
            if after { "format!(\"{}\", year)" } else { "year.to_string()" },
            if after { "year_span.min(1)" } else { "sign.unwrap_or(year_span)" },
            if after { "    if week > 53 {\n        return Err(E::Invalid { name: \"week\", value: week.to_string(), span_start: Some(sign.unwrap_or(w_span)), span_end: Some(w_span) });\n    }\n" } else { "" });
        let (found, maps) = edits(&source(false), &source(true));
        assert_eq!(found, ["Replaced demo::parse/S1/then/S0/value/args[0]/fields[value] → demo::parse/S1/then/S0/value/args[0]/fields[value]", "Replaced demo::parse/S1/then/S0/value/args[0]/fields[span_start]/args[0] → demo::parse/S1/then/S0/value/args[0]/fields[span_start]/args[0]", "Inserted - → demo::parse/S2"]);
        assert_eq!((maps.round3, maps.composites), (1, 1));
    }

    #[test]
    fn a_replaced_part_counts_each_of_its_tokens_as_replaced() {
        // Fix round 1, D3: two `let` statements that share only `let`, `=`, and `;` are no pair of
        // round 3. A candidate of an extract takes part in round 3: `let name = &self.name;` maps
        // to `let enum_name = &self.name;`, not to `let variant_values = &self.variant_values;`.
        let head = "pub struct S { name: String, variant_values: Vec<String> }\nimpl S {\n    pub fn to_tokens(&self) -> String {\n";
        let before = format!("{head}        let name = &self.name;\n        let expect_string = format!(\"attempted to convert invalid {{}} into JSValue\", name);\n        format!(\"{{}}{{}}\", name, expect_string)\n    }}\n}}\n");
        let after = format!("{head}        let enum_name = &self.name;\n        let variant_values = &self.variant_values;\n        format!(\"{{}}{{}}\", enum_name, variant_values.len())\n    }}\n}}\n");
        let (found, maps) = edits(&before, &after);
        assert_eq!(found[0], "Replaced demo::S::to_tokens/S0/pat → demo::S::to_tokens/S0/pat", "{found:?}");
        // The round 3 pair is `name` → `enum_name` only. The other `let` pair comes from the gap
        // rules of 8b (`Context::gap`), which stay as the fallback.
        assert_eq!((maps.round3, maps.gap_pairs), (1, 2), "{found:?}");
        // The header score of a composite counts a replaced condition as replaced: no composite
        // pair (the gap rules of 8b pair the two `if` statements in place).
        let before = body("    let mut x = 0;\n    if !v.contains(&a) {\n        x += 1;\n    }\n    x\n");
        let after = body("    let mut x = 0;\n    if b > 2 {\n        x += 1;\n    }\n    x\n");
        let (found, maps) = edits(&before, &after);
        assert_eq!(maps.composites, 0, "{found:?}");
        assert_eq!(found, ["Replaced demo::f/S1/cond → demo::f/S1/cond"]);
    }
}
