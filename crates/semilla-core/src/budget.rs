//! The size rule of each text of a line (design 71.1, R71.1-1).
//!
//! Each text that a line of an output shows (a macro input, an expansion, a constant value, an
//! array, a type, docs, a comment, a statement) has one budget for its unchanged parts:
//! [`BUDGET_CHARS`] characters and [`BUDGET_LINES`] lines. A text under the budget shows as
//! before. A text over the budget shows each changed part (the changed elements of two arrays,
//! the changed lines of two texts with lines, else the changed tokens) in full, with a short
//! context, the sizes, and the command that shows the full text. An unchanged text (a comment
//! with no edit) and a text that repeats a text of a fact (the name of a macro call in a
//! spotlight) show their first lines, the size, and the command.
//!
//! The rule never hides an edit: each changed part shows in full, an added or a deleted text
//! too, up to the limit of the user ([`CHANGED_MOST`], 50 kB of changed text). Only over that
//! limit, the line shows the first changed parts, the count of the others, the sizes, and the
//! command. Docs ask for no limit.

use crate::text::LINE_MARK;

/// The most characters of one text of a line (Decided by Claude).
pub const BUDGET_CHARS: usize = 2000;

/// The most lines of one text of a line (Decided by Claude).
pub const BUDGET_LINES: usize = 20;

/// The equal lines before and after a changed line.
pub const CONTEXT_LINES: usize = 2;

/// The equal tokens before and after a changed run of tokens.
pub const CONTEXT_TOKENS: usize = 4;

/// The most bytes of the changed part of a text that a line shows (the number of the user,
/// 2026-11-25: 50 kB). Over it, the line gives the first changed parts, the count of the
/// others, the sizes, and the command.
pub const CHANGED_MOST: usize = 50_000;

/// The mark of a text in the form of R71.1-1. A line with the mark has no cut of design 38.5:
/// the rule bounds its unchanged parts, and its changed parts show in full.
pub const LARGE_TEXT_MARK: &str = "(a large text: ";

/// The first tokens of a text before a changed part far from its start: they name the text.
pub const HEAD_TOKENS: usize = 3;

/// The first lines of a text with no other side.
pub const HEAD_LINES: usize = 3;

/// The first characters of a text of one line with no other side.
pub const HEAD_CHARS: usize = 80;

/// The command that shows each text of a fact in full (R71.1-1): `semilla expand fN --step S`.
pub fn full_text_command(id: &str, step: usize) -> String {
    format!("semilla expand {id} --step {step}")
}

/// The lines of a text: a line end is `\n` or the line mark of a fact text.
fn split_lines(text: &str) -> Vec<&str> {
    text.split(['\n', LINE_MARK]).collect()
}

/// The count of the lines of a text.
pub fn line_count(text: &str) -> usize {
    text.matches(['\n', LINE_MARK]).count() + 1
}

/// True if a text is over the budget: more characters or more lines.
pub fn over(text: &str) -> bool {
    text.chars().count() > BUDGET_CHARS || line_count(text) > BUDGET_LINES
}

/// The size of a text: `61.2 kB` for a text of one line, `3000 lines, 61.2 kB` else.
pub fn size(text: &str) -> String {
    let bytes = format!("{:.1} kB", text.len() as f64 / 1000.0);
    let lines = line_count(text);
    if lines > 1 { format!("{lines} lines, {bytes}") } else { bytes }
}

/// The sizes of two texts: `3000 → 3000 lines, 61.2 kB → 61.4 kB`.
pub fn sizes(old: &str, new: &str) -> String {
    let kb = |text: &str| format!("{:.1} kB", text.len() as f64 / 1000.0);
    let (a, b) = (line_count(old), line_count(new));
    if a > 1 || b > 1 { format!("{a} → {b} lines, {} → {}", kb(old), kb(new)) } else { format!("{} → {}", kb(old), kb(new)) }
}

/// The words after a text over the budget: the sizes, the count of the changed parts, and the
/// command.
fn note(sizes: &str, parts: &str, command: Option<&str>) -> String {
    let command = command.map(|command| format!("; the full text: {command}")).unwrap_or_default();
    let parts = if parts.is_empty() { String::new() } else { format!("; {parts}") };
    format!("(a large text: {sizes}{parts}{command})")
}

/// The elements of an array value (`[a, b]` or `&[a, b]`), as texts with no spaces. `None` if
/// the value is not an array, or if it is a repeat array (`[0; 4]`).
pub fn array_elements(value: &str) -> Option<Vec<String>> {
    let inner = value.trim().trim_start_matches('&').trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut elements = vec![String::new()];
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escape = false;
    for ch in inner.chars() {
        if let Some(open) = quote {
            elements.last_mut()?.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == open {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' => quote = Some(ch),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                elements.push(String::new());
                continue;
            }
            ';' if depth == 0 => return None,
            _ => {}
        }
        if !ch.is_whitespace() {
            elements.last_mut()?.push(ch);
        }
    }
    // A comma after the last element adds no element.
    if elements.last().is_some_and(String::is_empty) {
        elements.pop();
    }
    Some(elements)
}

/// The element form of the changed part of two values (R71.1-1; the form of R68.6-3 before
/// batch 11c): "[2] 3 → 9, [5] 6 → 1 (2 of 8 elements differ)". Each changed element has its
/// text, up to [`CHANGED_MOST`] bytes; past it, the text names the count of the others. `None`
/// if a value is not an array, the lengths differ, the arrays have fewer than two elements, or
/// each element differs: then the elements are not the changed part.
pub fn array_change(old: &str, new: &str) -> Option<String> {
    let (a, b) = (array_elements(old)?, array_elements(new)?);
    if a.len() != b.len() || a.len() < 2 {
        return None;
    }
    let changed: Vec<usize> = (0..a.len()).filter(|index| a[*index] != b[*index]).collect();
    if changed.is_empty() || changed.len() == a.len() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let mut length = 0;
    for index in &changed {
        let part = format!("[{index}] {} → {}", a[*index], b[*index]);
        length += part.len();
        if length > CHANGED_MOST && !parts.is_empty() {
            break;
        }
        parts.push(part);
    }
    if parts.len() < changed.len() {
        let more = changed.len() - parts.len();
        parts.push(format!("and {more} more (over {} kB of changed text)", CHANGED_MOST / 1000));
    }
    Some(format!("{} ({} of {} elements differ)", parts.join(", "), changed.len(), a.len()))
}

/// The text of a changed value in a line of the compiled check (a constant that a body reads,
/// R71.1-1): two arrays of one length show their changed elements (for an array the elements
/// are the changed part, also under the budget: the form of R68.6-3 before batch 11c); each
/// other pair shows `OLD → NEW`, or its changed tokens over the budget.
pub fn value_pair(old: &str, new: &str) -> String {
    if let Some(change) = array_change(old, new) {
        return change;
    }
    match pair(old, new, None, false) {
        Some(Form::Line(text)) => text,
        _ => format!("{old} → {new}"),
    }
}

/// The byte ranges of the tokens of a text: a word (letters, digits, `_`), a string literal, or
/// one other character. White space is no token.
fn tokens(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    while let Some((start, c)) = chars.next() {
        if c.is_whitespace() || c == LINE_MARK {
            continue;
        }
        let mut end = start + c.len_utf8();
        if word(c) {
            while let Some(&(at, next)) = chars.peek() {
                if !word(next) {
                    break;
                }
                end = at + next.len_utf8();
                chars.next();
            }
        } else if c == '"' {
            let mut escape = false;
            for (at, next) in chars.by_ref() {
                end = at + next.len_utf8();
                if escape {
                    escape = false;
                } else if next == '\\' {
                    escape = true;
                } else if next == '"' {
                    break;
                }
            }
        }
        out.push((start, end));
    }
    out
}

/// The tokens of a text, as texts (a word, a string literal, or one other character).
pub fn token_texts(text: &str) -> Vec<&str> {
    tokens(text).into_iter().map(|(start, end)| &text[start..end]).collect()
}

/// The pairs of equal entries of two lists in order (a shortest edit script, Myers 1986), after
/// the equal start and the equal end. Two lists with more than [`MOST_EDITS`] differences give
/// no pair in the middle: one changed part from the first difference to the last one.
fn common(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let start = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let end = a[start..].iter().rev().zip(b[start..].iter().rev()).take_while(|(x, y)| x == y).count();
    let mut pairs: Vec<(usize, usize)> = (0..start).map(|index| (index, index)).collect();
    let (x, y) = (&a[start..a.len() - end], &b[start..b.len() - end]);
    if let Some(middle) = myers(x, y) {
        pairs.extend(middle.into_iter().map(|(i, j)| (start + i, start + j)));
    }
    pairs.extend((0..end).map(|offset| (a.len() - end + offset, b.len() - end + offset)));
    pairs
}

/// The most differences of the compare of two lists.
const MOST_EDITS: usize = 2000;

/// The equal pairs of a shortest edit script of two lists, or `None` past [`MOST_EDITS`].
fn myers(x: &[&str], y: &[&str]) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (x.len() as isize, y.len() as isize);
    if n == 0 || m == 0 {
        return Some(Vec::new());
    }
    let most = (MOST_EDITS as isize).min(n + m);
    let offset = most + 1;
    let mut v = vec![0isize; (2 * most + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=most {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let index = (k + offset) as usize;
            let mut i = if k == -d || (k != d && v[index - 1] < v[index + 1]) { v[index + 1] } else { v[index - 1] + 1 };
            let mut j = i - k;
            while i < n && j < m && x[i as usize] == y[j as usize] {
                i += 1;
                j += 1;
            }
            v[index] = i;
            if i >= n && j >= m {
                found = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let last = found?;
    // The path back from the end: each diagonal run is a list of equal pairs.
    let mut pairs = Vec::new();
    let (mut i, mut j) = (n, m);
    for d in (0..=last).rev() {
        let v = &trace[d as usize];
        let k = i - j;
        let index = (k + offset) as usize;
        let previous = if d == 0 {
            0
        } else if k == -d || (k != d && v[index - 1] < v[index + 1]) {
            k + 1
        } else {
            k - 1
        };
        let (start_i, start_j) = if d == 0 { (0, 0) } else {
            let pi = v[(previous + offset) as usize];
            (pi, pi - previous)
        };
        // The snake of this step: from the end of the move to (i, j).
        let (mid_i, mid_j) = if d == 0 { (0, 0) } else if previous == k + 1 { (start_i, start_j + 1) } else { (start_i + 1, start_j) };
        let (mut a, mut b) = (i, j);
        while a > mid_i && b > mid_j {
            a -= 1;
            b -= 1;
            pairs.push((a as usize, b as usize));
        }
        (i, j) = (start_i, start_j);
    }
    pairs.reverse();
    Some(pairs)
}

/// The changed runs of two lists: each run is the range of the old entries and the range of
/// the new entries between two pairs.
fn runs(a: usize, b: usize, pairs: &[(usize, usize)]) -> Vec<((usize, usize), (usize, usize))> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    for &(x, y) in pairs.iter().chain(std::iter::once(&(a, b))) {
        if x > i || y > j {
            out.push(((i, x), (j, y)));
        }
        (i, j) = (x + 1, y + 1);
    }
    out
}

/// The changed parts of two texts of tokens, each with [`CONTEXT_TOKENS`] tokens of context:
/// the old text and the new text of each part. Two runs with a short equal part between them
/// are one part.
pub fn token_parts(old: &str, new: &str) -> Vec<(String, String)> {
    let (a, b) = (tokens(old), tokens(new));
    let text = |source: &'_ str, list: &[(usize, usize)]| -> Vec<String> { list.iter().map(|(s, e)| source[*s..*e].to_string()).collect() };
    let (x, y) = (text(old, &a), text(new, &b));
    let (xs, ys): (Vec<&str>, Vec<&str>) = (x.iter().map(String::as_str).collect(), y.iter().map(String::as_str).collect());
    let pairs = common(&xs, &ys);
    let mut found = runs(a.len(), b.len(), &pairs);
    // Join two runs with at most two times the context between them.
    let mut joined: Vec<((usize, usize), (usize, usize))> = Vec::new();
    for run in found.drain(..) {
        match joined.last_mut() {
            Some(last) if run.0.0 - last.0.1 <= 2 * CONTEXT_TOKENS && run.1.0 - last.1.1 <= 2 * CONTEXT_TOKENS => {
                last.0.1 = run.0.1;
                last.1.1 = run.1.1;
            }
            _ => joined.push(run),
        }
    }
    // The first tokens of the text name it (`table!(`), then the changed part with its context.
    let part = |source: &str, list: &[(usize, usize)], (from, to): (usize, usize)| -> String {
        let first = from.saturating_sub(CONTEXT_TOKENS);
        let last = (to + CONTEXT_TOKENS).min(list.len());
        if first >= last {
            return String::new();
        }
        let (start, end) = (list[first].0, list[last - 1].1);
        let before = match first {
            0 => String::new(),
            _ if first <= HEAD_TOKENS => one_row(&source[list[0].0..start]) + " ",
            _ => format!("{} … ", one_row(&source[list[0].0..list[HEAD_TOKENS - 1].1])),
        };
        let after = if last < list.len() { " …" } else { "" };
        format!("{before}{}{after}", one_row(&source[start..end]))
    };
    joined.into_iter().map(|(old_run, new_run)| (part(old, &a, old_run), part(new, &b, new_run))).collect()
}

/// A text on one row: each line mark and each line end is one space.
fn one_row(text: &str) -> String {
    text.split(['\n', LINE_MARK]).map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>().join(" ")
}

/// The changed lines of two texts with lines, each with [`CONTEXT_LINES`] lines of context:
/// rows with the mark ` ` (in the two texts), `-` (old only), `+` (new only), and a row
/// `… N equal lines` in place of the other equal lines. Also the count of the changed lines.
/// Each changed line shows in full; two changed lines of one place over the budget show their
/// changed tokens with context. A line of context over the budget shows its first characters.
pub fn line_parts(old: &str, new: &str) -> (Vec<String>, usize) {
    let (a, b) = (split_lines(old), split_lines(new));
    let pairs = common(&a, &b);
    let found = runs(a.len(), b.len(), &pairs);
    let changed: usize = found.iter().map(|((x0, x1), (y0, y1))| (x1 - x0) + (y1 - y0)).sum();
    // The rows of the old text to show: the changed lines and their context.
    let mut show_old = vec![false; a.len()];
    let mut show_new = vec![false; b.len()];
    for ((x0, x1), (y0, y1)) in &found {
        for index in x0.saturating_sub(CONTEXT_LINES)..(x1 + CONTEXT_LINES).min(a.len()) {
            show_old[index] = true;
        }
        for index in y0.saturating_sub(CONTEXT_LINES)..(y1 + CONTEXT_LINES).min(b.len()) {
            show_new[index] = true;
        }
    }
    let row = |mark: char, line: &str| -> String {
        if mark == ' ' && line.chars().count() > BUDGET_CHARS {
            format!("{mark} {} … {}", line.chars().take(HEAD_CHARS).collect::<String>(), note(&size(line), "a line of context", None))
        } else {
            format!("{mark} {line}").trim_end().to_string()
        }
    };
    let mut rows = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut hidden = 0usize;
    let flush = |rows: &mut Vec<String>, hidden: &mut usize| {
        if *hidden > 0 {
            rows.push(format!("  … {} equal line{}", *hidden, if *hidden == 1 { "" } else { "s" }));
            *hidden = 0;
        }
    };
    let mut pair_iter = pairs.iter().peekable();
    while i < a.len() || j < b.len() {
        match pair_iter.peek() {
            Some(&&(x, y)) if x == i && y == j => {
                if show_old[i] || show_new[j] {
                    flush(&mut rows, &mut hidden);
                    rows.push(row(' ', a[i]));
                } else {
                    hidden += 1;
                }
                pair_iter.next();
                (i, j) = (i + 1, j + 1);
            }
            next => {
                let (x, y) = next.map(|&&(x, y)| (x, y)).unwrap_or((a.len(), b.len()));
                flush(&mut rows, &mut hidden);
                // Two changed lines with one place: the changed tokens of a long line.
                let (olds, news) = (&a[i..x], &b[j..y]);
                if olds.len() == news.len() && olds.iter().zip(news).any(|(p, q)| p.chars().count() > BUDGET_CHARS || q.chars().count() > BUDGET_CHARS) {
                    for (p, q) in olds.iter().zip(news) {
                        for (from, to) in token_parts(p, q) {
                            rows.push(format!("- {from}"));
                            rows.push(format!("+ {to}"));
                        }
                    }
                } else {
                    rows.extend(olds.iter().map(|line| row('-', line)));
                    rows.extend(news.iter().map(|line| row('+', line)));
                }
                (i, j) = (x, y);
            }
        }
    }
    flush(&mut rows, &mut hidden);
    (rows, changed)
}

/// The form of a text over the budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Form {
    /// A text in the line.
    Line(String),
    /// A note for the line and the rows of a block below it.
    Block(String, Vec<String>),
}

/// The form of a changed pair of texts over the budget (R71.1-1). `None` if the two texts are
/// under the budget: they show as before. `keep` asks for each changed part also when the
/// changed part is over the budget (docs, a comment, design 58). Else the rows of a changed
/// part over the budget stop at the budget, and the note gives the count of the changed lines.
pub fn pair(old: &str, new: &str, command: Option<&str>, keep: bool) -> Option<Form> {
    if !over(old) && !over(new) {
        return None;
    }
    let sizes = sizes(old, new);
    if let Some(elements) = array_change(&one_row(old), &one_row(new)) {
        return Some(Form::Line(format!("{elements} {}", note(&sizes, "", command))));
    }
    if line_count(old) > 1 || line_count(new) > 1 {
        let (mut rows, changed) = line_parts(old, new);
        let words = format!("{changed} changed line{}, with {CONTEXT_LINES} lines of context", if changed == 1 { "" } else { "s" });
        // Each changed row shows; only past the limit of the user the rows stop.
        let mut cut = String::new();
        if !keep {
            let mut length = 0;
            if let Some(stop) = rows.iter().position(|row| {
                length += if row.starts_with('-') || row.starts_with('+') { row.len() } else { 0 };
                length > CHANGED_MOST
            }) {
                let shown = rows[..stop].iter().filter(|row| row.starts_with('-') || row.starts_with('+')).count();
                rows.truncate(stop);
                cut = format!("; over {} kB of changed text: the rows stop after {shown} of the {changed} changed lines", CHANGED_MOST / 1000);
            }
        }
        return Some(Form::Block(note(&sizes, &format!("{words}{cut}"), command), rows));
    }
    Some(Form::Line(inline_pair(old, new, command, keep)))
}

/// The changed parts of two texts in one line: for two texts with lines, each changed run of
/// lines with its first line number (a long run shows its changed tokens); else the changed
/// tokens. Each part is `` `OLD` → `NEW` ``. Each part shows; with `keep` false, the parts stop
/// past [`CHANGED_MOST`] bytes, and the note gives the count of the parts.
pub fn inline_pair(old: &str, new: &str, command: Option<&str>, keep: bool) -> String {
    let mut parts: Vec<(String, String, Option<usize>)> = Vec::new();
    if line_count(old) > 1 || line_count(new) > 1 {
        let (a, b) = (split_lines(old), split_lines(new));
        for ((x0, x1), (y0, y1)) in runs(a.len(), b.len(), &common(&a, &b)) {
            let (from, to) = (one_row(&a[x0..x1].join("\n")), one_row(&b[y0..y1].join("\n")));
            if from.chars().count() + to.chars().count() > 2 * HEAD_CHARS {
                parts.extend(token_parts(&from, &to).into_iter().map(|(p, q)| (p, q, Some(y0 + 1))));
            } else {
                parts.push((from, to, Some(y0 + 1)));
            }
        }
    } else {
        parts.extend(token_parts(old, new).into_iter().map(|(p, q)| (p, q, None)));
    }
    let count = parts.len();
    let mut texts: Vec<String> = Vec::new();
    let mut length = 0;
    for (from, to, line) in &parts {
        let at = line.map(|line| format!(" (line {line})")).unwrap_or_default();
        let text = format!("`{from}` → `{to}`{at}");
        length += text.len();
        if !keep && length > CHANGED_MOST && !texts.is_empty() {
            break;
        }
        texts.push(text);
    }
    let words = if texts.len() < count { format!("{count} changed parts, the first {} shown (over {} kB of changed text)", texts.len(), CHANGED_MOST / 1000) } else { format!("{count} changed part{}", if count == 1 { "" } else { "s" }) };
    format!("{} {}", texts.join(", "), note(&sizes(old, new), &words, command))
}

/// The form of an unchanged text over the budget (R71.1-1: a comment with no edit, or a text
/// that repeats a text of a fact, such as the name of a macro call in a spotlight): its first
/// lines (or its first characters), the size, and the command. `None` if the text is under the
/// budget.
pub fn single(text: &str, command: Option<&str>) -> Option<Form> {
    if !over(text) {
        return None;
    }
    head_form(text, command, "")
}

/// The form of a changed text with no other side (an added or a deleted text): `None`, so the
/// text shows in full, up to [`CHANGED_MOST`] bytes. Over it: the first lines, the size, and the
/// command (R71.1-1).
pub fn changed_single(text: &str, command: Option<&str>) -> Option<Form> {
    if text.len() <= CHANGED_MOST {
        return None;
    }
    head_form(text, command, &format!("over {} kB of changed text: ", CHANGED_MOST / 1000))
}

/// The first lines (or characters) of a text, with the note.
fn head_form(text: &str, command: Option<&str>, why: &str) -> Option<Form> {
    let lines = split_lines(text);
    if lines.len() > 1 {
        let rows: Vec<String> = lines.iter().take(HEAD_LINES).map(|line| line.to_string()).collect();
        let words = format!("{why}the first {} lines, {} more lines", rows.len(), lines.len() - rows.len());
        return Some(Form::Block(note(&size(text), &words, command), rows));
    }
    let head: String = text.chars().take(HEAD_CHARS).collect();
    Some(Form::Line(format!("`{head} …` {}", note(&size(text), &format!("{why}the first characters"), command))))
}

/// The code texts of a line: the byte ranges of each text between two backquotes.
fn spans(line: &str) -> Vec<(usize, usize)> {
    let marks: Vec<usize> = line.match_indices('`').map(|(at, _)| at).collect();
    marks.chunks(2).filter(|pair| pair.len() == 2).map(|pair| (pair[0] + 1, pair[1])).collect()
}

/// A line with each code text over the budget in its form (R71.1-1): a pair `` `A` → `B` ``
/// gives the form of a changed pair (each changed part in full). A text with no other side is a
/// changed text (an added or a deleted text of a fact line): it shows in full up to
/// [`CHANGED_MOST`]; with `repeat` (a spotlight line, which repeats a text of its fact) it gives
/// its first characters. A line with a change arrow next to a code text that is not a part of a
/// found pair keeps its texts: Semilla does not cut a text when it cannot find its other side
/// (the safe side).
pub fn line(line: &str, command: Option<&str>, keep: bool, repeat: bool) -> String {
    if line.chars().count() <= BUDGET_CHARS {
        return line.to_string();
    }
    let found = spans(line);
    let long: Vec<usize> = (0..found.len()).filter(|index| over(&line[found[*index].0..found[*index].1])).collect();
    if long.is_empty() {
        return line.to_string();
    }
    const ARROW: &str = " → ";
    let mut out = String::new();
    let mut at = 0;
    let mut index = 0;
    while index < found.len() {
        let (start, end) = found[index];
        let next = found.get(index + 1).copied();
        let paired = next.filter(|(next_start, _)| line.get(end + 1..next_start - 1) == Some(ARROW));
        let is_long = |range: (usize, usize)| over(&line[range.0..range.1]);
        if let Some(other) = paired.filter(|other| is_long((start, end)) || is_long(*other)) {
            match pair(&line[start..end], &line[other.0..other.1], command, keep) {
                Some(Form::Line(text)) => {
                    out.push_str(&line[at..start - 1]);
                    out.push_str(&text);
                    at = other.1 + 1;
                }
                Some(Form::Block(..)) => {
                    out.push_str(&line[at..start - 1]);
                    out.push_str(&inline_pair(&line[start..end], &line[other.0..other.1], command, keep));
                    at = other.1 + 1;
                }
                None => {}
            }
            index += 2;
            continue;
        }
        let near_arrow = line[..start - 1].ends_with(ARROW) || line.get(end + 1..).is_some_and(|rest| rest.starts_with(ARROW));
        if is_long((start, end)) && !near_arrow {
            let form = if repeat { single(&line[start..end], command) } else if keep { None } else { changed_single(&line[start..end], command) };
            if let Some(Form::Line(text)) = form {
                out.push_str(&line[at..start - 1]);
                out.push_str(&text);
                at = end + 1;
            }
        }
        index += 1;
    }
    out.push_str(&line[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(changes: &[(usize, &str)]) -> String {
        let mut numbers: Vec<String> = (0..9000).map(|n| (n * 7 % 10007).to_string()).collect();
        for (at, text) in changes {
            numbers[*at] = text.to_string();
        }
        format!("table!( {} )", numbers.join(", "))
    }

    // rule+ R71.1-1
    #[test]
    fn a_text_over_the_budget_shows_the_changed_part_the_sizes_and_the_command() {
        // A macro input of 9000 numbers with one changed number: the changed tokens with their
        // context, not the full input.
        let (old, new) = (table(&[]), table(&[(4321, "77777")]));
        let text = line(&format!("changed macro call `{old}` → `{new}` (makes items)"), Some("semilla expand f1 --step 0"), false, false);
        assert!(text.len() < 400, "{text}");
        assert!(text.starts_with("changed macro call `table!( … 212, 219, 226, 233, 240 …` → `table!( … 212, 219, 77777, 233, 240 …` (a large text: "), "{text}");
        assert!(text.ends_with(" kB; 1 changed part; the full text: semilla expand f1 --step 0) (makes items)"), "{text}");
        // Two arrays of one length: the changed elements (the form of R68.6-3).
        let (a, b) = (format!("[{}]", (0..3000).map(|n| n.to_string()).collect::<Vec<_>>().join(", ")), format!("[{}]", (0..3000).map(|n| if n == 7 { "9".to_string() } else { n.to_string() }).collect::<Vec<_>>().join(", ")));
        let Some(Form::Line(text)) = pair(&a, &b, None, false) else { panic!() };
        assert!(text.starts_with("[7] 7 → 9 (1 of 3000 elements differ) (a large text: "), "{text}");
        // Two texts with lines: the changed lines with two lines of context.
        let lines = |change: &str| (0..100).map(|n| if n == 50 { change.to_string() } else { format!("line {n}") }).collect::<Vec<_>>().join("\n");
        let Some(Form::Block(note, rows)) = pair(&lines("line 50"), &lines("changed"), Some("CMD"), false) else { panic!() };
        assert_eq!(note, "(a large text: 100 → 100 lines, 0.8 kB → 0.8 kB; 2 changed lines, with 2 lines of context; the full text: CMD)");
        assert_eq!(rows, ["  … 48 equal lines", "  line 48", "  line 49", "- line 50", "+ changed", "  line 51", "  line 52", "  … 47 equal lines"]);
        // A text with no other side: the first lines, the size, and the command.
        let Some(Form::Block(note, rows)) = single(&lines("x"), Some("CMD")) else { panic!() };
        assert_eq!(note, "(a large text: 100 lines, 0.8 kB; the first 3 lines, 97 more lines; the full text: CMD)");
        assert_eq!(rows, ["line 0", "line 1", "line 2"]);
    }

    // rule- R71.1-1
    #[test]
    fn a_text_under_the_budget_shows_as_before_and_a_pair_with_no_found_side_keeps_its_text() {
        let short = "changed literal `1` → `2` at `shop::f/tail`";
        assert_eq!(line(short, Some("CMD"), false, false), short);
        assert_eq!(pair("[1, 2, 3]", "[1, 5, 3]", None, false), None);
        assert_eq!(single("a short text", None), None);
        // A long text next to an arrow whose other side Semilla does not find stays whole.
        let long = "x".repeat(3000);
        let odd = format!("changed `a` b` → `{long}`");
        assert_eq!(line(&odd, None, false, false), odd);
        // Each changed part shows: two changes far apart are two parts.
        let parts = token_parts(&table(&[(10, "1"), (5000, "5")]), &table(&[(10, "2"), (5000, "6")]));
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0].0.contains(" 1,") && parts[0].1.contains(" 2,") && parts[1].0.contains(" 5,") && parts[1].1.contains(" 6,"), "{parts:?}");
        // Each changed line shows: 100 changed lines of 200 lines, with docs and with code.
        let lines = |change: usize| (0..200).map(|n| if n % 2 == 0 && change == 1 { format!("new {n}") } else { format!("line {n}") }).collect::<Vec<_>>().join("\n");
        for keep in [true, false] {
            let Some(Form::Block(note, kept)) = pair(&lines(0), &lines(1), None, keep) else { panic!() };
            assert_eq!(kept.iter().filter(|row| row.starts_with('+')).count(), 100, "{note}");
            assert_eq!(kept.iter().filter(|row| row.starts_with('-')).count(), 100, "{note}");
            assert!(!note.contains("stop"), "{note}");
        }
    }

    // R71.1-1: an added or a deleted text is a change: it shows in full, also over the budget,
    // up to the limit of the user (50 kB). A line of a spotlight repeats a text of its fact: it
    // gives the first characters. The negative case: a text over 50 kB gives its first lines.
    // rule+ R71.1-1
    #[test]
    fn an_added_or_a_deleted_text_over_the_budget_shows_in_full_up_to_the_limit_of_the_user() {
        let statement = (0..25).map(|n| format!("    let v{n} = {n};")).collect::<Vec<_>>().join("\n");
        assert_eq!(changed_single(&statement, Some("CMD")), None);
        let ten = format!("table!( {} )", (0..2000).map(|n| n.to_string()).collect::<Vec<_>>().join(", "));
        let long = format!("added macro call `{ten}` at `shop::table!#0`");
        assert!(ten.len() > BUDGET_CHARS && ten.len() < CHANGED_MOST);
        assert_eq!(line(&long, Some("CMD"), false, false), long);
        assert!(line(&format!("spotlight: the macro call `{}` makes items", table(&[])), Some("CMD"), false, true).contains("the first characters; the full text: CMD"));
        // Two arrays: each changed element.
        let (a, b) = (format!("[{}]", (0..3000).map(|n| n.to_string()).collect::<Vec<_>>().join(", ")), format!("[{}]", (0..3000).map(|n| if n % 100 == 0 { "1".to_string() } else { n.to_string() }).collect::<Vec<_>>().join(", ")));
        let changed = array_change(&a, &b).expect("the elements");
        assert!(changed.contains("[2900] 2900 → 1") && changed.ends_with("(30 of 3000 elements differ)"), "{changed}");
        // Over the limit of the user: the first lines, the size, and the command.
        let huge = (0..20_000).map(|n| format!("let v{n} = {n};")).collect::<Vec<_>>().join("\n");
        let Some(Form::Block(note, rows)) = changed_single(&huge, Some("CMD")) else { panic!() };
        assert!(note.contains("over 50 kB of changed text: the first 3 lines, 19997 more lines; the full text: CMD"), "{note}");
        assert_eq!(rows.len(), HEAD_LINES);
    }
}
