//! The inner compare of a restructured body (design 38.2).
//!
//! A body can get a new form: the statements of a closure (`data.with_mut(|shared| { … })`)
//! become statements of the function body. The diff then gives one "deleted statement" fact and
//! some "added" facts. Each fact text shows only the head of a long statement, so the text does
//! not show if a statement in the cut part changed. This module compares the inner statements of
//! one side with the statements of the other side, and it adds the result to the fact texts.

use semilla_core::Fact;
use semilla_syntax::Edit;
use semilla_syntax::rs::{AstNode, SyntaxNode, WalkEvent, ast};
use semilla_syntax::tt::compare_text;

/// The smallest similarity of two statements (equal tokens / tokens of the longer statement)
/// for the text "except: …".
const SIMILAR: f64 = 0.6;

/// The most inner statements that a fact text names one by one.
const NAMES: usize = 3;

/// One statement: its token text (for the compare) and its source text on one line (for the
/// fact text).
#[derive(Clone)]
struct Stmt {
    tokens: Vec<String>,
    source: String,
}

/// A statement fact of one side: the fact, the statement, its inner statements, and its anchor.
struct Side {
    fact: usize,
    stmt: Stmt,
    inner: Vec<Stmt>,
    anchor: Option<semilla_core::Anchor>,
}

/// Parses one statement. `None` if the text is not exactly one statement.
fn parse(text: &str) -> Option<(Stmt, Vec<Stmt>)> {
    let wrapped = format!("{{\n{text}\n}}");
    let block = semilla_syntax::rs::parse_block(&wrapped)?;
    let [stmt] = &semilla_syntax::rs::statements(&block.stmt_list()?)[..] else {
        return None;
    };
    let make = |stmt: &SyntaxNode| -> Option<Stmt> {
        let mut words: Vec<String> = compare_text(stmt).split_whitespace().map(str::to_string).collect();
        // An assignment has the value `()`: with or without `;` at its end, it is the same
        // statement (a tail `acc = x` and `acc = x;` in a block).
        let assign = |expr: Option<ast::Expr>| matches!(expr, Some(ast::Expr::BinExpr(binary)) if matches!(binary.op_kind(), Some(ast::BinaryOp::Assignment { .. })));
        if let Some(expr_stmt) = ast::ExprStmt::cast(stmt.clone())
            && expr_stmt.semicolon_token().is_some()
            && assign(expr_stmt.expr())
        {
            words.pop();
        }
        let source = semilla_syntax::rs::display_syntax(stmt);
        (!source.is_empty()).then_some(Stmt { tokens: words, source })
    };
    // The inner statements: the statements of each block in the statement that is not in a
    // different inner block (a closure body, a plain block, an `unsafe` block, a branch), and
    // not in an inner item.
    let mut inner: Vec<SyntaxNode> = Vec::new();
    let mut walk = stmt.preorder();
    while let Some(event) = walk.next() {
        let WalkEvent::Enter(node) = event else { continue };
        if ast::Item::can_cast(node.kind()) {
            walk.skip_subtree();
        } else if let Some(list) = ast::StmtList::cast(node) {
            inner.extend(semilla_syntax::rs::statements(&list));
            walk.skip_subtree();
        }
    }
    let inner = inner.iter().map(make).collect::<Option<Vec<Stmt>>>()?;
    Some((make(stmt)?, inner))
}

/// The share of equal tokens of two statements (the longest common subsequence), from 0 to 1.
// A token count is at most 1_000_000 here, so each cast to `f64` is exact.
#[allow(clippy::cast_precision_loss)]
fn similarity(a: &[String], b: &[String]) -> f64 {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 || n * m > 1_000_000 {
        return 0.0;
    }
    let mut row = vec![0usize; m + 1];
    for left in a {
        let mut diagonal = 0;
        for j in 0..m {
            let up = row[j + 1];
            row[j + 1] = if *left == b[j] { diagonal + 1 } else { up.max(row[j]) };
            diagonal = up;
        }
    }
    row[m] as f64 / n.max(m) as f64
}

/// The head of a statement for a fact text: `data.with_mut(…)`, or a short text.
fn head(source: &str) -> String {
    let start: String = source.chars().take_while(|c| *c != '(' && *c != '{' && *c != '|').collect();
    let start = start.trim_end();
    if !start.is_empty() && start.chars().count() <= 40 && source[start.len()..].trim_start().starts_with('(') {
        return format!("{start}(…)");
    }
    semilla_syntax::tree::short(source, 40)
}

/// Inserts `text` in a fact description before the place (" at `…`"), or at the end.
fn insert(fact: &mut Fact, text: &str) {
    let place = fact
        .at
        .as_ref()
        .map(|at| format!(" at `{at}`"))
        .filter(|place| fact.description.ends_with(place.as_str()));
    match place {
        Some(place) => {
            let cut = fact.description.len() - place.len();
            fact.description = format!("{}{text}{place}", &fact.description[..cut]);
        }
        None => fact.description.push_str(text),
    }
}

/// The statement facts of one body. `added`: true for the added statements.
fn sides(edits: &[Edit], fact_of: &[Option<usize>], facts: &[Fact], start: usize, added: bool) -> Vec<(String, Side)> {
    let mut out = Vec::new();
    for (fact, record) in facts.iter().enumerate().skip(start) {
        if record.part != "edit" || record.spotlight {
            continue;
        }
        let indexes: Vec<usize> = (0..edits.len()).filter(|&i| fact_of[i] == Some(fact)).collect();
        let [index] = indexes[..] else { continue };
        let edit = &edits[index];
        let empty = |text: &Option<String>| text.as_deref().is_none_or(|text| text.trim().is_empty());
        let text = if added && empty(&edit.text_before) && !empty(&edit.text_after) {
            edit.source_after.as_ref().or(edit.text_after.as_ref())
        } else if !added && !empty(&edit.text_before) && empty(&edit.text_after) {
            edit.source_before.as_ref().or(edit.text_before.as_ref())
        } else {
            None
        };
        let Some((stmt, inner)) = text.and_then(|text| parse(text)) else {
            continue;
        };
        let Some(body) = record.at.as_ref().map(|at| at.item.clone()) else {
            continue;
        };
        let anchor = if added { edit.after.clone() } else { edit.before.clone() };
        out.push((body, Side { fact, stmt, inner, anchor }));
    }
    out
}

/// The match of each inner statement: (outer, inner) → the flat fact and the equal mark.
type Found = Vec<Vec<Option<(usize, bool)>>>;

/// One direction of the inner compare: the statements with inner statements on one side, and
/// the flat statements on the other side.
struct Pass<'a> {
    outer: &'a [(String, Side)],
    flat: &'a [(String, Side)],
    /// The side of the outer statements: "deleted" or "added".
    gone: &'static str,
    /// The text for a flat statement in a fact text.
    other: &'static str,
    /// The mark of each inner statement that the statement mapper moved to a different place.
    moved: Vec<Vec<bool>>,
    found: Found,
}

impl<'a> Pass<'a> {
    fn new(edits: &[Edit], outer: &'a [(String, Side)], flat: &'a [(String, Side)], gone: &'static str) -> Self {
        let other = if gone == "deleted" {
            "an added statement"
        } else {
            "a deleted statement"
        };
        let found = outer.iter().map(|(_, side)| vec![None; side.inner.len()]).collect();
        let mut pass = Pass {
            outer,
            flat,
            gone,
            other,
            moved: Vec::new(),
            found,
        };
        pass.moved = outer.iter().map(|(_, side)| pass.moved_marks(edits, side)).collect();
        pass
    }

    /// The moved mark of each inner statement of `side` (design 41): a move edit whose place on
    /// this side is inside the statement.
    fn moved_marks(&self, edits: &[Edit], side: &Side) -> Vec<bool> {
        let mut marks = vec![false; side.inner.len()];
        let Some(anchor) = &side.anchor else { return marks };
        let gone = self.gone;
        let mut texts: Vec<Vec<String>> = edits
            .iter()
            .filter(|edit| matches!(edit.refactor, Some(semilla_syntax::Refactor::Moved { .. })))
            .filter_map(|edit| {
                let place = if gone == "deleted" {
                    edit.before.as_ref()?
                } else {
                    edit.after.as_ref()?
                };
                let text = if gone == "deleted" {
                    edit.text_before.as_ref()?
                } else {
                    edit.text_after.as_ref()?
                };
                (place.item == anchor.item && place.path.len() > anchor.path.len() && place.path.starts_with(&anchor.path))
                    .then(|| text.split_whitespace().map(str::to_string).collect())
            })
            .collect();
        for (i, inner) in side.inner.iter().enumerate() {
            let bare = |tokens: &[String]| tokens.iter().filter(|token| *token != ";").cloned().collect::<Vec<_>>();
            if let Some(at) = texts.iter().position(|text| bare(text) == bare(&inner.tokens)) {
                texts.remove(at);
                marks[i] = true;
            }
        }
        marks
    }

    /// Matches the flat statements to the inner statements: first the equal texts, then the
    /// similar texts.
    fn match_flat(&mut self, done: &[bool]) {
        let mut used = vec![false; self.flat.len()];
        for equal in [true, false] {
            for (f, (body, side)) in self.flat.iter().enumerate() {
                if used[f] || done[side.fact] {
                    continue;
                }
                if let Some((o, i)) = self.best(body, side, equal, done) {
                    self.found[o][i] = Some((f, equal));
                    used[f] = true;
                }
            }
        }
    }

    /// The free inner statement of the same body that best matches the flat statement `side`.
    fn best(&self, body: &str, side: &Side, equal: bool, done: &[bool]) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize, f64)> = None;
        for (o, (outer_body, outer_side)) in self.outer.iter().enumerate() {
            if outer_body != body || done[outer_side.fact] {
                continue;
            }
            for (i, inner) in outer_side.inner.iter().enumerate() {
                if self.found[o][i].is_some() || self.moved[o][i] {
                    continue;
                }
                let score = if inner.tokens == side.stmt.tokens {
                    1.0
                } else if equal || inner.tokens.first() != side.stmt.tokens.first() {
                    continue;
                } else {
                    similarity(&inner.tokens, &side.stmt.tokens)
                };
                if (equal || score >= SIMILAR) && best.is_none_or(|(_, _, s)| score > s) {
                    best = Some((o, i, score));
                }
            }
        }
        best.map(|(o, i, _)| (o, i))
    }

    /// Adds the result of the pass to the fact texts.
    fn describe(&self, facts: &mut [Fact], done: &mut [bool]) {
        for (o, (_, side)) in self.outer.iter().enumerate() {
            if self.found[o].iter().all(Option::is_none) && self.moved[o].iter().all(|m| !m) {
                continue;
            }
            self.describe_side(o, side, facts, done);
        }
    }

    /// Adds the result for the outer statement `o` to its fact and to the facts of its matches.
    fn describe_side(&self, o: usize, side: &Side, facts: &mut [Fact], done: &mut [bool]) {
        let (gone, other) = (self.gone, self.other);
        let name = head(&side.stmt.source);
        for (i, matched) in self.found[o].iter().enumerate() {
            let Some((f, equal)) = matched else { continue };
            let target = self.flat[*f].1.fact;
            let inner = &side.inner[i];
            let text = if *equal {
                format!(" (the same text as an inner statement of the {gone} `{name}`)")
            } else {
                let (old, new) = crate::edits::sides(&inner.source, &self.flat[*f].1.stmt.source);
                if old.contains('…') || new.contains('…') {
                    let line = format!("= the inner statement of the {gone} `{name}`: {}", inner.source);
                    let content = facts[target].content.take().map(|content| format!("{content}\n{line}")).unwrap_or(line);
                    facts[target].content = Some(content);
                }
                if gone == "deleted" {
                    format!(" (an inner statement of the {gone} `{name}`, except: `{old}` → `{new}`)")
                } else {
                    format!(" (an inner statement of the {gone} `{name}`, except: `{new}` → `{old}`)")
                }
            };
            insert(&mut facts[target], &text);
            done[target] = true;
        }
        let equal = self.found[o].iter().filter(|m| m.is_some_and(|(_, equal)| equal)).count();
        let changed = self.found[o].iter().filter(|m| m.is_some_and(|(_, equal)| !equal)).count();
        let move_count = self.moved[o].iter().filter(|m| **m).count();
        let missing: Vec<&Stmt> = side
            .inner
            .iter()
            .zip(&self.found[o])
            .zip(&self.moved[o])
            .filter(|((_, m), is_moved)| m.is_none() && !**is_moved)
            .map(|((stmt, _), _)| stmt)
            .collect();
        let text = if changed == 0 && missing.is_empty() && move_count == 0 {
            format!(" (each inner statement is {other} of the body, with the same text)")
        } else if changed == 0 && missing.is_empty() && equal == 0 {
            " (each inner statement is a moved statement of the body)".to_string()
        } else {
            let mut parts = Vec::new();
            if move_count > 0 {
                parts.push(format!("{move_count} moved to a different place"));
            }
            if equal > 0 {
                parts.push(format!("{equal} with the same text as {other}"));
            }
            if changed > 0 {
                parts.push(format!("{changed} with a change in {other}"));
            }
            if !missing.is_empty() {
                let mut names: Vec<String> = missing
                    .iter()
                    .take(NAMES)
                    .map(|stmt| format!("`{}`", semilla_syntax::tree::short(&stmt.source, 40)))
                    .collect();
                if missing.len() > NAMES {
                    names.push(format!("and {} more", missing.len() - NAMES));
                }
                parts.push(format!("{} not in {other}: {}", missing.len(), names.join(", ")));
            }
            format!(" (inner statements: {})", parts.join("; "))
        };
        insert(&mut facts[side.fact], &text);
        done[side.fact] = true;
    }
}

/// Adds the inner compare to the statement facts of each body (design 38.2). The facts from
/// `start` on are the edit facts of this step.
pub(crate) fn compare(edits: &[Edit], fact_of: &[Option<usize>], facts: &mut [Fact], start: usize) {
    let added = sides(edits, fact_of, facts, start, true);
    let deleted = sides(edits, fact_of, facts, start, false);
    let mut done = vec![false; facts.len()];
    // The deleted side with inner statements first (the body of a closure becomes the body of
    // the function), then the added side (the statements go into a new closure).
    for (outer, flat, gone) in [(&deleted, &added, "deleted"), (&added, &deleted, "added")] {
        let mut pass = Pass::new(edits, outer, flat, gone);
        pass.match_flat(&done);
        pass.describe(facts, &mut done);
    }
}
