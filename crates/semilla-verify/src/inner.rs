//! The inner compare of a restructured body (design 38.2).
//!
//! A body can get a new form: the statements of a closure (`data.with_mut(|shared| { … })`)
//! become statements of the function body. The diff then gives one "deleted statement" fact and
//! some "added" facts. Each fact text shows only the head of a long statement, so the text does
//! not show if a statement in the cut part changed. This module compares the inner statements of
//! one side with the statements of the other side, and it adds the result to the fact texts.

use semilla_core::Fact;
use semilla_syntax::tree::{one_line, tokens};
use semilla_syntax::Edit;
use syn::spanned::Spanned;

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

/// A statement fact of one side: the fact, the statement, and its inner statements.
struct Side {
    fact: usize,
    stmt: Stmt,
    inner: Vec<Stmt>,
}

/// Parses one statement. `None` if the text is not exactly one statement.
fn parse(text: &str) -> Option<(Stmt, Vec<Stmt>)> {
    let wrapped = format!("{{\n{text}\n}}");
    let block: syn::Block = syn::parse_str(&wrapped).ok()?;
    let [stmt] = &block.stmts[..] else { return None };
    let lines: Vec<&str> = wrapped.split('\n').collect();
    let slice = |span: proc_macro2::Span| -> Option<String> {
        let (start, end) = (span.start(), span.end());
        let offset = |line: usize, column: usize| -> Option<usize> {
            let before: usize = lines.get(..line.checked_sub(1)?)?.iter().map(|text| text.chars().count() + 1).sum();
            Some(before + column)
        };
        let (from, to) = (offset(start.line, start.column)?, offset(end.line, end.column)?);
        let chars: Vec<char> = wrapped.chars().collect();
        (from < to && to <= chars.len()).then(|| one_line(&chars[from..to].iter().collect::<String>()))
    };
    let make = |stmt: &syn::Stmt| -> Option<Stmt> {
        let mut words: Vec<String> = tokens(stmt).split_whitespace().map(str::to_string).collect();
        // An assignment has the value `()`: with or without `;` at its end, it is the same
        // statement (a tail `acc = x` and `acc = x;` in a block).
        let assign = |expr: &syn::Expr| matches!(expr, syn::Expr::Assign(_)) || matches!(expr, syn::Expr::Binary(binary) if matches!(binary.op, syn::BinOp::AddAssign(_) | syn::BinOp::SubAssign(_) | syn::BinOp::MulAssign(_) | syn::BinOp::DivAssign(_) | syn::BinOp::RemAssign(_) | syn::BinOp::BitXorAssign(_) | syn::BinOp::BitAndAssign(_) | syn::BinOp::BitOrAssign(_) | syn::BinOp::ShlAssign(_) | syn::BinOp::ShrAssign(_)));
        if matches!(stmt, syn::Stmt::Expr(expr, Some(_)) if assign(expr)) {
            words.pop();
        }
        Some(Stmt { tokens: words, source: slice(stmt.span())? })
    };
    // The inner statements: the statements of each block in the statement that is not in a
    // different inner block (a closure body, a plain block, an `unsafe` block, a branch).
    struct Blocks<'a>(Vec<&'a syn::Stmt>);
    impl<'a> syn::visit::Visit<'a> for Blocks<'a> {
        fn visit_block(&mut self, block: &'a syn::Block) {
            self.0.extend(block.stmts.iter());
        }
        fn visit_item(&mut self, _: &'a syn::Item) {}
    }
    let mut blocks = Blocks(Vec::new());
    syn::visit::visit_stmt(&mut blocks, stmt);
    let inner = blocks.0.into_iter().map(make).collect::<Option<Vec<Stmt>>>()?;
    Some((make(stmt)?, inner))
}

/// The share of equal tokens of two statements (the longest common subsequence), from 0 to 1.
fn similarity(a: &[String], b: &[String]) -> f64 {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 || n * m > 1_000_000 {
        return 0.0;
    }
    let mut row = vec![0usize; m + 1];
    for i in 0..n {
        let mut diagonal = 0;
        for j in 0..m {
            let up = row[j + 1];
            row[j + 1] = if a[i] == b[j] { diagonal + 1 } else { up.max(row[j]) };
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
    let place = fact.at.as_ref().map(|at| format!(" at `{at}`")).filter(|place| fact.description.ends_with(place.as_str()));
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
    for fact in start..facts.len() {
        if facts[fact].part != "edit" || facts[fact].spotlight {
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
        let Some((stmt, inner)) = text.and_then(|text| parse(text)) else { continue };
        let Some(body) = facts[fact].at.as_ref().map(|at| at.item.clone()) else { continue };
        out.push((body, Side { fact, stmt, inner }));
    }
    out
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
        let other = if gone == "deleted" { "an added statement" } else { "a deleted statement" };
        // The match of each inner statement: (outer, inner) → the flat fact and the equal mark.
        let mut found: Vec<Vec<Option<(usize, bool)>>> = outer.iter().map(|(_, side)| vec![None; side.inner.len()]).collect();
        let mut used = vec![false; flat.len()];
        for equal in [true, false] {
            for (f, (body, side)) in flat.iter().enumerate() {
                if used[f] || done[side.fact] {
                    continue;
                }
                let mut best: Option<(usize, usize, f64)> = None;
                for (o, (outer_body, outer_side)) in outer.iter().enumerate() {
                    if outer_body != body || done[outer_side.fact] {
                        continue;
                    }
                    for (i, inner) in outer_side.inner.iter().enumerate() {
                        if found[o][i].is_some() {
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
                if let Some((o, i, _)) = best {
                    found[o][i] = Some((f, equal));
                    used[f] = true;
                }
            }
        }
        for (o, (_, side)) in outer.iter().enumerate() {
            if found[o].iter().all(Option::is_none) {
                continue;
            }
            let name = head(&side.stmt.source);
            for (i, matched) in found[o].iter().enumerate() {
                let Some((f, equal)) = matched else { continue };
                let target = flat[*f].1.fact;
                let inner = &side.inner[i];
                let text = if *equal {
                    format!(" (the same text as an inner statement of the {gone} `{name}`)")
                } else {
                    let (old, new) = crate::edits::sides(&inner.source, &flat[*f].1.stmt.source);
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
            let equal = found[o].iter().filter(|m| m.is_some_and(|(_, equal)| equal)).count();
            let changed = found[o].iter().filter(|m| m.is_some_and(|(_, equal)| !equal)).count();
            let missing: Vec<&Stmt> = side.inner.iter().zip(&found[o]).filter(|(_, m)| m.is_none()).map(|(stmt, _)| stmt).collect();
            let text = if changed == 0 && missing.is_empty() {
                format!(" (each inner statement is {other} of the body, with the same text)")
            } else {
                let mut parts = Vec::new();
                if equal > 0 {
                    parts.push(format!("{equal} with the same text as {other}"));
                }
                if changed > 0 {
                    parts.push(format!("{changed} with a change in {other}"));
                }
                if !missing.is_empty() {
                    let mut names: Vec<String> = missing.iter().take(NAMES).map(|stmt| format!("`{}`", semilla_syntax::tree::short(&stmt.source, 40))).collect();
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
}
