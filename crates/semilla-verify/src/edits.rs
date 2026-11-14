//! The edit facts (design 24.2, rules 1 to 3): each edit that no claim and no type-level fact
//! explains gets a derived fact.
//!
//! - The shape rules of the hints and the checkers of the old ops are the machinery. The fact text
//!   uses the words of the op: "added early exit `…`", "changed literal `200` → `170`".
//! - An edit with no exact shape gets the general fact "changed NODE: `A` → `B`".
//! - Each edit is its own fact with its text and its category (design 35). The compare result
//!   comes later, from the MIR compare. An equal compare never hides the text of an edit.
//! - Only a rename of a local variable folds the edits of its body under one fact.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{Anchor, Category, Claim, Fact};
use semilla_syntax::tree::{display_tokens, one_line};
use semilla_syntax::{Edit, EditKind};

use crate::checks::{self, Context};
use crate::BodyStatus;

/// The words of an op for the fact text.
fn words(op: &str) -> String {
    match op {
        "add-early-exit" => "added early exit".into(),
        "add-let" => "added `let`".into(),
        "add-if" => "added `if`".into(),
        "add-match" => "added `match`".into(),
        "add-loop" => "added loop".into(),
        "add-return" => "added `return`".into(),
        "add-break" => "added `break`".into(),
        "add-continue" => "added `continue`".into(),
        "add-macro-statement" => "added macro statement".into(),
        "add-expression-statement" => "added statement".into(),
        "delete-statement" => "deleted statement".into(),
        "move-statement" => "moved statement".into(),
        "add-item" => "added".into(),
        "remove-item" => "removed".into(),
        "edit-doc" => "changed docs".into(),
        "change-literal" => "changed literal".into(),
        "change-path" => "changed path".into(),
        "change-operator" => "changed operator".into(),
        "add-arm" => "added match arm".into(),
        "add-alternative" => "added pattern alternative".into(),
        "change-signature" => "changed macro or signature".into(),
        "replace-expression" => "changed expression".into(),
        "replace-pattern" => "changed pattern".into(),
        "replace-type" => "changed type".into(),
        other => other.replace('-', " "),
    }
}

/// The text of an edit for a fact with no check of the op: the words of the suggested op (or
/// "changed NODE") and the two sides, with the place.
pub fn display_text(edit: &Edit) -> String {
    if let Some((text, _)) = edit.anchor().and_then(|anchor| crate::facts::doc_test_fact(&anchor.item, edit)) {
        return text;
    }
    let op = crate::hints::suggest(edit);
    let (old, new) = short_pair(edit);
    let sides = match (&edit.text_before, &edit.text_after) {
        (Some(_), Some(_)) => format!("`{old}` → `{new}`"),
        (None, Some(_)) => format!("`{new}`"),
        (Some(_), None) => format!("`{old}`"),
        _ => String::new(),
    };
    let what = if !op.contains(' ') && semilla_core::ops::op(op).is_some() { words(op) } else { format!("changed {}:", edit.node_after.or(edit.node_before).unwrap_or("code")) };
    let location = edit.anchor().filter(|a| !a.path.is_empty()).map(|a| format!(" at `{a}`")).unwrap_or_default();
    if let Some(note) = operand_note(edit) {
        return format!("{note}{location}");
    }
    if edit.kind == EditKind::Visibility {
        let side = |text: &Option<String>| text.clone().filter(|text| !text.trim().is_empty()).unwrap_or_else(|| "private".into());
        return format!("changed visibility: `{}` → `{}`", side(&edit.text_before), side(&edit.text_after));
    }
    if let Some(moved) = moved_text(edit) {
        return format!("moved {}: {moved}", edit.node_after.or(edit.node_before).unwrap_or("code"));
    }
    // A doc comment with no text on one side: the words of each other doc fact (batch 6a, A2).
    if edit.kind == EditKind::Docs && what == "changed docs" {
        let empty = |text: &Option<String>| text.as_deref().is_none_or(|text| text.trim().is_empty());
        match (empty(&edit.text_before), empty(&edit.text_after)) {
            (true, false) => return format!("added docs `{new}`{location}"),
            (false, true) => return format!("removed docs `{old}`{location}"),
            _ => {}
        }
    }
    format!("{what} {sides}{location}")
}

/// The note of an operand chain edit: "added operand `+ c`" (C2; the diff makes it).
fn operand_note(edit: &Edit) -> Option<&str> {
    edit.note.as_deref().filter(|_| edit.node_before == Some("binary") && edit.node_after == Some("binary"))
}

/// A short source text: the source when it exists (D9: the display shows the source, not a const
/// value), else the token text.
fn short(source: &Option<String>, text: &Option<String>) -> String {
    let text = source.clone().or_else(|| text.clone()).unwrap_or_default();
    let line = one_line(&text);
    let chars: Vec<char> = line.chars().collect();
    if chars.len() > 60 { format!("{}…", chars[..token_end(&chars, 60, 0)].iter().collect::<String>().trim_end()) } else { line }
}

/// True if a text can be cut before the character `at`: the position is not inside a token
/// (a name, a number, or a keyword).
fn token_boundary(chars: &[char], at: usize) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    at == 0 || at >= chars.len() || !(word(chars[at - 1]) && word(chars[at]))
}

/// The end of a cut near `end` that is not in the middle of a token (brief 5h, C7). The cut moves
/// back to the start of the token. If the cut then has `keep` characters or less, it moves to the
/// end of the token: the text of the first difference always stays.
fn token_end(chars: &[char], end: usize, keep: usize) -> usize {
    let end = end.min(chars.len());
    if token_boundary(chars, end) {
        return end;
    }
    match (keep + 1..end).rev().find(|at| token_boundary(chars, *at)) {
        Some(at) => at,
        None => (end..=chars.len()).find(|at| token_boundary(chars, *at)).unwrap_or(chars.len()),
    }
}

/// The short texts of a changed pair: see [`short_texts`].
/// The text of a moved node (batch 5n, B1): the node and its two places. The two sides of a move
/// have the same text, so "`a` → `a`" says nothing. `None` for an edit that is not a move.
fn moved_text(edit: &Edit) -> Option<String> {
    let (Some(before), Some(after)) = (&edit.before, &edit.after) else { return None };
    let (old, new) = full_pair(edit);
    if edit.kind != EditKind::Moved && (old.is_empty() || old != new) {
        return None;
    }
    let text = short(&edit.source_after, &edit.text_after);
    let place = |anchor: &Anchor| anchor.path.iter().map(|segment| segment.to_string()).collect::<Vec<_>>().join("/");
    Some(if before == after {
        format!("`{text}` in `{before}` (line {} after the step)", edit.line)
    } else if before.item == after.item {
        format!("`{text}` from `{}` to `{}` in `{}`", place(before), place(after), before.item)
    } else {
        format!("`{text}` from `{before}` to `{after}`")
    })
}

fn short_pair(edit: &Edit) -> (String, String) {
    let (old, new) = full_pair(edit);
    short_texts(old, new)
}

/// The short texts of a pair of texts with one line each (batch 5k, D6). A long pair shows each
/// changed token of the two sides. A cut is only in a part that is equal on the two sides, and
/// each cut has the mark `…`. So the two sides never look the same, and no cut hides a change.
pub(crate) fn short_texts(old: String, new: String) -> (String, String) {
    if old.chars().count() <= 60 && new.chars().count() <= 60 {
        return (old, new);
    }
    let (a, b) = (cut_tokens(&old), cut_tokens(&new));
    let (marks_a, marks_b) = changed_tokens(&a, &b);
    let (short_old, short_new) = (cut_equal(&a, &marks_a), cut_equal(&b, &marks_b));
    // The last check (batch 6a, A2): two different texts never show as two equal sides. If a
    // cut hides the difference, the full texts show.
    if short_old == short_new && old != new {
        return (old, new);
    }
    (short_old, short_new)
}

/// The two sides of an "old → new" fact text (batch 6a, A2). Each fact text with two sides
/// uses this function: the attributes, the generics, the where clause, the signature parts,
/// the `use` lines, the docs, and the values. Each side is on one line. A cut keeps each
/// part that differs, with some context, and each cut has the mark `…`.
pub(crate) fn sides(old: &str, new: &str) -> (String, String) {
    short_texts(one_line(old), one_line(new))
}

/// The text "`OLD` → `NEW`" of two sides: see [`sides`].
pub(crate) fn sides_text(old: &str, new: &str) -> String {
    let (old, new) = sides(old, new);
    format!("`{old}` → `{new}`")
}

/// The full text of the two sides of a pair for the diff block of a report, or `None` if the
/// sides are equal.
pub(crate) fn pair_content(old: &str, new: &str) -> Option<String> {
    let (old, new) = (one_line(old), one_line(new));
    (old != new).then(|| {
        let mut lines = Vec::new();
        if !old.is_empty() {
            lines.push(format!("- {old}"));
        }
        if !new.is_empty() {
            lines.push(format!("+ {new}"));
        }
        lines.join("\n")
    })
}

/// The tokens for a cut: the display tokens, but each word of a comment is one token. Then a
/// long doc comment with one changed word shows the changed word, not the full comment.
fn cut_tokens(text: &str) -> Vec<(bool, String)> {
    let mut tokens = Vec::new();
    for (space, token) in display_tokens(text) {
        if token.starts_with("//") || token.starts_with("/*") {
            tokens.extend(token.split_whitespace().enumerate().map(|(index, word)| (space || index > 0, word.to_string())));
        } else {
            tokens.push((space, token));
        }
    }
    tokens
}

/// The marks of one side of a token diff: `changed[i]` is true for a token that the other side
/// does not have, and `gap[i]` is true when the other side has changed tokens before token `i`
/// (`gap[len]`: after the last token).
struct Marks {
    changed: Vec<bool>,
    gap: Vec<bool>,
}

/// The token diff of two texts: the equal start, the equal end, and the longest common
/// subsequence of the middle part. A very long middle part is all changed.
fn changed_tokens(a: &[(bool, String)], b: &[(bool, String)]) -> (Marks, Marks) {
    let equal = |i: usize, j: usize| a[i].1 == b[j].1;
    let prefix = (0..a.len().min(b.len())).take_while(|i| equal(*i, *i)).count();
    let suffix = (0..a.len().min(b.len()) - prefix).take_while(|k| equal(a.len() - 1 - k, b.len() - 1 - k)).count();
    let (n, m) = (a.len() - prefix - suffix, b.len() - prefix - suffix);
    // `pairs`: the equal tokens of the middle part, as indexes of the two sides.
    let mut pairs = Vec::new();
    if n > 0 && m > 0 && n * m <= 1_000_000 {
        let mut table = vec![0u32; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[i * (m + 1) + j] = if equal(prefix + i, prefix + j) { table[(i + 1) * (m + 1) + j + 1] + 1 } else { table[(i + 1) * (m + 1) + j].max(table[i * (m + 1) + j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if equal(prefix + i, prefix + j) {
                pairs.push((prefix + i, prefix + j));
                i += 1;
                j += 1;
            } else if table[(i + 1) * (m + 1) + j] >= table[i * (m + 1) + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    let mut marks_a = Marks { changed: vec![true; a.len()], gap: vec![false; a.len() + 1] };
    let mut marks_b = Marks { changed: vec![true; b.len()], gap: vec![false; b.len() + 1] };
    let all = (0..prefix).map(|i| (i, i)).chain(pairs).chain((0..suffix).rev().map(|k| (a.len() - 1 - k, b.len() - 1 - k)));
    let (mut next_a, mut next_b) = (0, 0);
    for (i, j) in all.chain([(a.len(), b.len())]) {
        // Before this equal pair, the side A has the tokens `next_a..i` and the side B has the
        // tokens `next_b..j`. They are the changed tokens.
        marks_a.gap[i] = j > next_b;
        marks_b.gap[j] = i > next_a;
        if i < a.len() {
            marks_a.changed[i] = false;
            marks_b.changed[j] = false;
        }
        (next_a, next_b) = (i + 1, j + 1);
    }
    (marks_a, marks_b)
}

/// One side of a pair with a cut in each long equal part. An equal token stays when it starts
/// less than 15 characters from a change.
fn cut_equal(tokens: &[(bool, String)], marks: &Marks) -> String {
    const CONTEXT: usize = 15;
    let size = |index: usize| tokens[index].1.chars().count() + usize::from(tokens[index].0);
    let mut keep = marks.changed.clone();
    let mut distance = usize::MAX;
    for index in 0..tokens.len() {
        if marks.changed[index] {
            distance = 0;
            continue;
        }
        if marks.gap[index] {
            distance = 0;
        }
        keep[index] |= distance < CONTEXT;
        distance = distance.saturating_add(size(index));
    }
    distance = usize::MAX;
    for index in (0..tokens.len()).rev() {
        if marks.changed[index] {
            distance = 0;
            continue;
        }
        if marks.gap[index + 1] {
            distance = 0;
        }
        keep[index] |= distance < CONTEXT;
        distance = distance.saturating_add(size(index));
    }
    let mut out = String::new();
    let mut index = 0;
    while index < tokens.len() {
        if !keep[index] {
            let end = (index..tokens.len()).find(|at| keep[*at]).unwrap_or(tokens.len());
            // A cut of a very short part hides text and saves no space.
            if (index..end).map(size).sum::<usize>() > 3 {
                out.push('…');
                index = end;
                continue;
            }
        }
        if tokens[index].0 && !out.is_empty() && !out.ends_with('…') {
            out.push(' ');
        }
        out.push_str(&tokens[index].1);
        index += 1;
    }
    out
}

/// The full texts of a changed pair, on one line each.
fn full_pair(edit: &Edit) -> (String, String) {
    let full = |source: &Option<String>, text: &Option<String>| one_line(&source.clone().or_else(|| text.clone()).unwrap_or_default());
    (full(&edit.source_before, &edit.text_before), full(&edit.source_after, &edit.text_after))
}

/// The full text of a long pair for the report (C4), or `None` for a short pair.
fn long_content(edit: &Edit) -> Option<String> {
    let (old, new) = full_pair(edit);
    (old.chars().count() > 60 || new.chars().count() > 60).then(|| {
        let mut lines = Vec::new();
        if !old.is_empty() {
            lines.push(format!("- {old}"));
        }
        if !new.is_empty() {
            lines.push(format!("+ {new}"));
        }
        lines.join("\n")
    })
}

/// The inserted text of a pair whose old text is the new text without one part (a new match arm),
/// or `None`.
fn inserted(edit: &Edit) -> Option<String> {
    let full = |source: &Option<String>, text: &Option<String>| one_line(&source.clone().or_else(|| text.clone()).unwrap_or_default());
    let (old, new) = (full(&edit.source_before, &edit.text_before), full(&edit.source_after, &edit.text_after));
    let (a, b): (Vec<&str>, Vec<&str>) = (old.split(' ').collect(), new.split(' ').collect());
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    (prefix + suffix == a.len() && b.len() > a.len()).then(|| short(&Some(b[prefix..b.len() - suffix].join(" ")), &None))
}

/// The path of a `use` line before its last part (batch 6a, A4): `crate :: a ::` for
/// `use crate::a::{b, c};` and for `use crate::a::b;`. Two lines with the same text here are
/// the two sides of a changed line.
fn use_prefix(signature: &str) -> String {
    // The token `use`, not the letters: an attribute can have them (`unused_imports`).
    let tokens: Vec<&str> = signature.split_whitespace().collect();
    let start = tokens.iter().position(|token| *token == "use").map(|at| at + 1).unwrap_or(0);
    let text: String = tokens[start..].concat();
    let text = text.as_str();
    match text.split_once('{') {
        Some((prefix, _)) => prefix.to_string(),
        None => text.rsplit_once("::").map(|(prefix, _)| format!("{prefix}::")).unwrap_or_default(),
    }
}

/// The text of the public name changes of a `use` fact, or an empty text for a line that
/// gives no public name.
fn names_text(names: Option<Vec<crate::NameChange>>) -> String {
    match names {
        None => String::new(),
        Some(names) if names.is_empty() => " (the public names are the same)".into(),
        Some(names) => format!(": {}", names.iter().map(|change| change.text.clone()).collect::<Vec<_>>().join("; ")),
    }
}

/// The category of an edit fact from the category of its op and the body status (design 35).
/// An op with no fixed category, and a `logic` op, give `control flow` if the control flow
/// graph of the body changes. Each other op keeps its category.
fn category(context: &Context, item: &str, op: Option<Category>) -> Category {
    let graph_changed = matches!(context.bodies.status(item), Some(BodyStatus::Different { cfg_changed: true }));
    match op {
        Some(Category::Logic) | None if graph_changed => Category::ControlFlow,
        Some(category) => category,
        None => Category::Logic,
    }
}

/// Derives the edit facts. Sets `fact_of` for each edit that a new fact owns.
pub fn derive(context: &Context, owners: &[Option<usize>], fact_of: &mut [Option<usize>], facts: &mut Vec<Fact>) {
    let edits = context.edits;
    let free = |fact_of: &[Option<usize>], index: usize| owners[index].is_none() && fact_of[index].is_none();
    let item_of = |edit: &Edit| edit.anchor().map(|a| a.item.clone()).or_else(|| edit.item().map(str::to_string));

    // 1. A rename of a local variable: the edits of the body fold under one fact. The text of
    // the item changes only by the new names, so the fact text shows each edit (design 35).
    let mut by_item: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for index in (0..edits.len()).filter(|&i| free(fact_of, i)) {
        if let Some(item) = item_of(&edits[index]) {
            by_item.entry(item).or_default().push(index);
        }
    }
    for (item, indexes) in &by_item {
        // Only body edits fold: a doc or an item edit keeps its own fact.
        let body: Vec<usize> = indexes.iter().copied().filter(|&i| matches!(edits[i].kind, EditKind::Replaced | EditKind::Modified | EditKind::Inserted | EditKind::Deleted | EditKind::Moved)).collect();
        if body.is_empty() {
            continue;
        }
        let renamed = crate::hints::likely_local_renames(&body.iter().map(|&i| edits[i].clone()).collect::<Vec<_>>(), context.before, context.after);
        // The pattern `_` is not a local: `let _h = f();` → `let _ = f();` moves a drop. Such an
        // edit keeps its own fact ("changed pattern").
        if renamed.iter().any(|(owner, old, new)| owner == item && (old == "_" || new == "_")) {
            continue;
        }
        let pairs: Vec<String> = renamed.iter().filter(|(owner, _, _)| owner == item).map(|(_, old, new)| format!("`{old}` → `{new}`")).collect();
        if pairs.is_empty() {
            continue;
        }
        let what = if pairs.len() == 1 { "renamed local" } else { "renamed locals" };
        push(facts, fact_of, &body, Fact {
            at: Some(Anchor::item(item)),
            file: edits[body[0]].file.clone(),
            part: "body".into(),
            description: format!("`{item}`: {what} {}, {} edit(s)", pairs.join(", "), body.len()),
            category: Category::Rename,
            ..fact_base()
        });
    }

    // The `use` lines (batch 6a, A4). A changed line is the pair of a removed line and an
    // added line of one module with the same visibility and the same path before the braces.
    // Each other line is a removed line or an added line. A fact has the text of its line. A
    // `pub use` line that a different crate can use also gives the public names.
    let is_use = |edit: &Edit| matches!(edit.kind, EditKind::ItemRemoved | EditKind::ItemAdded) && (edit.attr_before.as_deref() == Some("use") || edit.attr_after.as_deref() == Some("use"));
    let use_item = |edit: &Edit| -> Option<&semilla_syntax::Item> {
        match edit.kind {
            EditKind::ItemRemoved => context.before.items.get(&edit.before.as_ref()?.item),
            _ => context.after.items.get(&edit.after.as_ref()?.item),
        }
    };
    let line_text = |edit: &Edit| one_line(edit.source_before.as_deref().or(edit.source_after.as_deref()).unwrap_or("")).trim().to_string();
    let use_edits: Vec<usize> = (0..edits.len()).filter(|&i| free(fact_of, i) && is_use(&edits[i]) && item_of(&edits[i]).is_some()).collect();
    let mut paired: BTreeSet<usize> = BTreeSet::new();
    for &index in use_edits.iter().filter(|&&i| edits[i].kind == EditKind::ItemRemoved) {
        let edit = &edits[index];
        let item = item_of(edit).unwrap_or_default();
        let module = item.rsplit_once("::").map(|(owner, _)| owner.to_string()).unwrap_or_default();
        let key = use_item(edit).map(|item| (item.vis.replace(' ', ""), use_prefix(&item.signature)));
        let same_module = |j: usize| edits[j].kind == EditKind::ItemAdded && !paired.contains(&j) && free(fact_of, j) && item_of(&edits[j]).is_some_and(|other| other.rsplit_once("::").map(|(owner, _)| owner) == Some(module.as_str()));
        // The same line with a different visibility or attribute is one changed line (batch 6a, A6).
        let signature = use_item(edit).map(|item| item.signature.clone());
        let added = use_edits.iter().copied().find(|&j| same_module(j) && signature.is_some() && use_item(&edits[j]).map(|item| item.signature.clone()) == signature).or_else(|| {
            use_edits.iter().copied().find(|&j| same_module(j) && key.is_some() && use_item(&edits[j]).map(|item| (item.vis.replace(' ', ""), use_prefix(&item.signature))) == key)
        });
        let names = crate::pub_use_changes(context.before, context.after, edit.before.as_ref().map(|a| a.item.as_str()), added.and_then(|j| edits[j].after.as_ref()).map(|a| a.item.as_str()));
        let (description, indexes) = match added {
            Some(added) => {
                paired.insert(added);
                (format!("changed `use` line: {}", sides_text(&line_text(edit), &line_text(&edits[added]))), vec![index, added])
            }
            None => (format!("removed `use` line: `{}`", semilla_syntax::tree::short(&line_text(edit), 100)), vec![index]),
        };
        let at = added.and_then(|j| item_of(&edits[j])).unwrap_or(item.clone());
        push(facts, fact_of, &indexes, Fact {
            at: Some(Anchor::item(&at)),
            file: edit.file.clone(),
            part: "edit".into(),
            description: format!("{description}{}", names_text(names)),
            category: Category::Import,
            reason: context.step.reasons.get(&at).cloned(),
            ..fact_base()
        });
    }
    for &index in use_edits.iter().filter(|&&i| edits[i].kind == EditKind::ItemAdded) {
        if !free(fact_of, index) {
            continue;
        }
        let edit = &edits[index];
        let item = item_of(edit).unwrap_or_default();
        let names = crate::pub_use_changes(context.before, context.after, None, edit.after.as_ref().map(|a| a.item.as_str()));
        push(facts, fact_of, &[index], Fact {
            at: Some(Anchor::item(&item)),
            file: edit.file.clone(),
            part: "edit".into(),
            description: format!("added `use` line: `{}`{}", semilla_syntax::tree::short(&line_text(edit), 100), names_text(names)),
            category: Category::Import,
            reason: context.step.reasons.get(&item).cloned(),
            ..fact_base()
        });
    }

    // 2. Each other free edit: the shape of an old op, else the general fact.
    for index in 0..edits.len() {
        if !free(fact_of, index) {
            continue;
        }
        let edit = &edits[index];
        let Some(item) = item_of(edit) else { continue };
        // A removed and an added item with the same path (a changed `use` line): one fact (D12).
        if edit.kind == EditKind::ItemRemoved {
            let added = (0..edits.len()).find(|&j| j != index && free(fact_of, j) && edits[j].kind == EditKind::ItemAdded && edits[j].item() == Some(item.as_str()));
            if let Some(added) = added {
                let full = |text: &Option<String>| one_line(&text.clone().unwrap_or_default());
                let is_use = edit.attr_before.as_deref() == Some("use") || item.contains("::use#");
                let what = if is_use { "`use` line" } else { "item" };
                let is_macro = [context.before, context.after].iter().any(|ws| ws.items.get(&item).is_some_and(|found| found.kind == semilla_syntax::ItemKind::Macro));
                push(facts, fact_of, &[index, added], Fact {
                    at: Some(Anchor::item(&item)),
                    file: edits[added].file.clone(),
                    part: "edit".into(),
                    description: format!("changed {what}: `{}` → `{}`", full(&edit.source_before).trim(), full(&edits[added].source_after).trim()),
                    category: if is_use { Category::Import } else if is_macro { Category::Macro } else { Category::Text },
                    reason: context.step.reasons.get(&item).cloned(),
                    ..fact_base()
                });
                continue;
            }
        }
        // A doc test is code (brief 5h, B1): `cargo test` compiles and runs each code block of a
        // doc comment. The fact has the category `docs`, the words "doc test" in its text, and
        // the diff of the full doc lines.
        if let Some((description, content)) = crate::facts::doc_test_fact(&item, edit) {
            push(facts, fact_of, &[index], Fact {
                at: Some(Anchor::item(&item)),
                file: edit.file.clone(),
                part: "doc test".into(),
                description,
                content: Some(content),
                category: Category::Docs,
                reason: context.step.reasons.get(&item).cloned(),
                ..fact_base()
            });
            continue;
        }
        // A plain `//` or `/* */` comment changed (Part B of batch 5j). A comment is not a doc:
        // the fact says "changed comment", category `docs`, and it needs no reason.
        if edit.kind == EditKind::Comment {
            let (old, new) = short_pair(edit);
            let text = match (edit.text_before.as_deref().unwrap_or(""), edit.text_after.as_deref().unwrap_or("")) {
                ("", _) => format!("added comment `{new}`"),
                (_, "") => format!("removed comment `{old}`"),
                _ => format!("changed comment `{old}` → `{new}`"),
            };
            push(facts, fact_of, &[index], Fact {
                at: edit.before.clone().or(edit.after.clone()).or_else(|| edit.anchor().map(|a| a.clone())),
                file: edit.file.clone(),
                part: "comment".into(),
                description: text,
                content: Some(crate::files::content_change(edit.text_before.as_deref(), edit.text_after.as_deref(), None, None)),
                category: Category::Docs,
                reason: edit.anchor().and_then(|a| context.step.reasons.get(&a.item).cloned()),
                ..fact_base()
            });
            continue;
        }
        let op = crate::hints::suggest(edit);
        let anchor = if edit.kind == EditKind::Inserted { edit.after.clone() } else { edit.before.clone().or(edit.after.clone()) };
        let mut explained = vec![index];
        let mut description = None;
        let mut op_category = None;
        if !op.contains(' ') && semilla_core::ops::op(op).is_some() {
            let mut claim = Claim { id: "derived".into(), op: op.to_string(), at: anchor.clone(), args: Default::default() };
            if matches!(op, "add-early-exit" | "add-if") {
                if let Some(cond) = edit.text_after.as_deref().and_then(condition) {
                    claim.args.insert("cond".into(), serde_json::Value::String(cond));
                }
            }
            let outcome = checks::check(context, &claim);
            if outcome.status.is_verified() && outcome.explains.contains(&index) {
                explained = outcome.explains.iter().copied().filter(|&i| free(fact_of, i)).collect();
                op_category = outcome.category;
                let text = match (edit.kind, &edit.text_before, &edit.text_after) {
                    (EditKind::ItemAdded | EditKind::ItemRemoved, _, _) => format!("`{item}`"),
                    (_, Some(_), Some(_)) if matches!(op, "add-arm" | "add-alternative") && inserted(edit).is_some() => format!("`{}`", inserted(edit).unwrap_or_default()),
                    (_, Some(_), Some(_)) if moved_text(edit).is_some() => format!("moved {}", moved_text(edit).unwrap_or_default()),
                    (_, Some(_), Some(_)) => {
                        let (old, new) = short_pair(edit);
                        format!("`{old}` → `{new}`")
                    }
                    (_, None, Some(_)) => format!("`{}`", short(&edit.source_after, &edit.text_after)),
                    (_, Some(_), None) => format!("`{}`", short(&edit.source_before, &edit.text_before)),
                    _ => String::new(),
                };
                let mut text = match operand_note(edit) {
                    Some(note) => note.to_string(),
                    None if moved_text(edit).is_some() => format!("{}: {text}", words(op)),
                    // A doc comment with no text on one side (batch 6a, A2).
                    None if op == "edit-doc" && edit.text_before.as_deref().is_none_or(|side| side.trim().is_empty()) => format!("added docs `{}`", short_pair(edit).1),
                    None if op == "edit-doc" && edit.text_after.as_deref().is_none_or(|side| side.trim().is_empty()) => format!("removed docs `{}`", short_pair(edit).0),
                    None => format!("{} {text}", words(op)),
                };
                // The effect is a hint, and the text says so (design 8.8.1).
                if let Some(effect) = outcome.effect {
                    let name = format!("{effect:?}").to_lowercase();
                    match &outcome.witness {
                        Some(witness) => text.push_str(&format!(" (effect hint: {name}, example: {witness})")),
                        None => text.push_str(&format!(" (effect hint: {name})")),
                    }
                }
                description = Some(text);
            }
        }
        // A removed `if` that keeps one branch (`remove-if`): try each branch.
        if description.is_none() && edit.text_before.as_deref().is_some_and(|t| t.trim_start().starts_with("if ")) {
            for keep in ["else", "then"] {
                let mut claim = Claim { id: "derived".into(), op: "remove-if".into(), at: anchor.clone(), args: Default::default() };
                claim.args.insert("keep".into(), serde_json::Value::String(keep.into()));
                let outcome = checks::check(context, &claim);
                if outcome.status.is_verified() && outcome.explains.contains(&index) {
                    explained = outcome.explains.iter().copied().filter(|&i| free(fact_of, i)).collect();
                    op_category = outcome.category;
                    let (old, new) = short_pair(edit);
                    description = Some(format!("removed `if`, kept the `{keep}` branch: `{old}` → `{new}`"));
                    break;
                }
            }
        }
        let description = description.unwrap_or_else(|| {
            let node = edit.node_after.or(edit.node_before).unwrap_or("code");
            let (old, new) = short_pair(edit);
            match operand_note(edit) {
                Some(note) => note.to_string(),
                None if moved_text(edit).is_some() => format!("moved {node}: {}", moved_text(edit).unwrap_or_default()),
                // An operator expression that is a statement has no op: use the words of
                // `replace-expression`.
                None if matches!(node, "binary" | "unary") => format!("changed expression `{old}` → `{new}`"),
                // A doc comment of a renamed or moved item: the words of each other doc fact.
                // A side with no text is no side (batch 6a, A2).
                None if edit.kind == EditKind::Docs && edit.text_before.as_deref().is_none_or(|text| text.trim().is_empty()) => format!("added docs `{new}`"),
                None if edit.kind == EditKind::Docs && edit.text_after.as_deref().is_none_or(|text| text.trim().is_empty()) => format!("removed docs `{old}`"),
                None if edit.kind == EditKind::Docs => format!("changed docs `{old}` → `{new}`"),
                None => format!("changed {node}: `{old}` → `{new}`"),
            }
        });
        let place = anchor.clone().map(|a| a.to_string()).unwrap_or_else(|| item.clone());
        // The text of a move has its two places.
        let location = anchor.as_ref().filter(|a| !a.path.is_empty() && moved_text(edit).is_none()).map(|a| format!(" at `{a}`")).unwrap_or_default();
        // C1: a doc edit has no MIR: its category is `docs`, also in a body that changed.
        let category = if matches!(edit.kind, EditKind::Docs | EditKind::Comment) { Category::Docs } else { category(context, &item, op_category) };
        push(facts, fact_of, &explained, Fact {
            at: anchor.clone().or_else(|| Some(Anchor::item(&item))),
            file: edit.file.clone(),
            part: "edit".into(),
            description: format!("{description}{location}"),
            content: long_content(edit),
            category,
            reason: context.step.reasons.get(&place).cloned(),
            ..fact_base()
        });
    }
}

/// The text of the condition of an inserted `if`.
fn condition(text: &str) -> Option<String> {
    let rest = text.trim().strip_prefix("if ")?;
    let end = rest.find('{')?;
    Some(rest[..end].trim().to_string())
}

fn fact_base() -> Fact {
    Fact { status: "derived".into(), ..Default::default() }
}

fn push(facts: &mut Vec<Fact>, fact_of: &mut [Option<usize>], indexes: &[usize], mut fact: Fact) {
    fact.id = format!("f{}", facts.len() + 1);
    for &index in indexes {
        fact_of[index] = Some(facts.len());
    }
    facts.push(fact);
}

/// The type change of a fact: the old type, the new type, and the text of each site. `None` for a
/// fact with another change (a new name, two different type changes, a text with a cut).
fn type_change(fact: &Fact) -> Option<(String, String, Vec<String>)> {
    let at = fact.at.as_ref()?;
    // The note of a public API change is not a part of the type change (batch 6a, A1).
    let fact = &Fact { description: api_note(&fact.description).0.to_string(), ..fact.clone() };
    // The two sides of "`old` → `new`". A side with a backtick is not one text; a side with a
    // cut (`…`) can hide a difference.
    let pair = |text: &str| -> Option<(String, String)> {
        let (old, new) = text.strip_prefix('`')?.strip_suffix('`')?.split_once("` → `")?;
        (!old.contains(['`', '…']) && !new.contains(['`', '…']) && old != new).then(|| (old.to_string(), new.to_string()))
    };
    match fact.part.as_str() {
        "type" => {
            let (old, new) = pair(fact.description.strip_prefix(&format!("type of `{}`: ", at.item))?)?;
            // The text of a const or a static has the name and the value.
            let plain = |text: &str| !text.starts_with("const ") && !text.starts_with("static ");
            (plain(&old) && plain(&new)).then(|| (old, new, vec![format!("`{}`", at.item)]))
        }
        "parameter" => {
            let (old, new) = pair(fact.description.strip_prefix(&format!("parameter of `{}` ", at.item))?)?;
            let ((old_name, old_type), (new_name, new_type)) = (old.split_once(" : ")?, new.split_once(" : ")?);
            (old_name == new_name).then(|| (old_type.to_string(), new_type.to_string(), vec![format!("parameter `{old_name}` of `{}`", at.item)]))
        }
        "return type" => {
            let (old, new) = pair(fact.description.strip_prefix(&format!("return type of `{}` ", at.item))?)?;
            let (old, new) = (old.strip_prefix("-> ").unwrap_or(&old), new.strip_prefix("-> ").unwrap_or(&new));
            Some((old.to_string(), new.to_string(), vec![format!("return type of `{}`", at.item)]))
        }
        "tuple fields" => {
            let (old, new) = pair(fact.description.strip_prefix(&format!("the tuple fields of `{}` changed: ", at.item))?)?;
            let types = |text: &str| -> Option<Vec<String>> {
                let inner = text.strip_prefix('(')?.strip_suffix(')')?;
                Some(split_types(inner))
            };
            let (old, new) = (types(&old)?, types(&new)?);
            if old.len() != new.len() {
                return None;
            }
            // Each changed position must have the same change.
            let changed: Vec<usize> = (0..old.len()).filter(|&index| old[index] != new[index]).collect();
            let first = *changed.first()?;
            changed
                .iter()
                .all(|&index| old[index] == old[first] && new[index] == new[first])
                .then(|| (old[first].clone(), new[first].clone(), changed.iter().map(|index| format!("`{}.{index}`", at.item)).collect()))
        }
        _ => None,
    }
}

/// The text of a fact without the note "(the public API can change: …)" at its end, and the
/// note (batch 6a, A1).
pub(crate) fn api_note(description: &str) -> (&str, Option<&str>) {
    match description.rfind(" (the public API can change: ") {
        Some(at) if description.ends_with(')') => (&description[..at], Some(&description[at..])),
        _ => (description, None),
    }
}

/// Splits a type list at the commas outside brackets (`<>`, `()`, `[]`). An arrow (`->`) is not
/// a bracket.
fn split_types(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut start, mut previous) = (0i32, 0, ' ');
    for (index, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' if previous == '-' => {}
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(text[start..index].trim().to_string());
                start = index + 1;
            }
            _ => {}
        }
        previous = c;
    }
    if !text[start..].trim().is_empty() {
        parts.push(text[start..].trim().to_string());
    }
    parts
}

/// Folds equal facts into one fact (design 24.2, the short output). Two edit facts fold when they
/// have the same text (without the place), the same category, the same compare result, and no
/// spotlight. Two body facts fold when they have the same text. Two type facts fold when they have the same
/// type change (trial 4, C5): "type `u32` → `u64` in 8 places: …". The places go to the `sites`
/// of the fact, and the text of a type fold names each place. A fact with a different text or a
/// different category or compare result never folds.
pub fn fold(facts: &mut Vec<Fact>, fact_of: &mut [Option<usize>]) {
    let key = |fact: &Fact| -> Option<(String, String)> {
        if fact.spotlight || fact.reason.is_some() || !fact.sites.is_empty() {
            return None;
        }
        // Facts with different notes stay apart: the note of one fact is not true for the
        // bodies of a different fact (batch 5u, part A).
        let class = format!("{:?} {:?} {:?}", fact.category, fact.compare, fact.note);
        if let Some((old, new, _)) = type_change(fact) {
            let text = format!("type `{old}` → `{new}`");
            // Facts with different API notes stay apart (batch 6a, A1).
            return Some((format!("type {class} {text} {}", api_note(&fact.description).1.unwrap_or("")), text));
        }
        let at = fact.at.as_ref()?;
        match fact.part.as_str() {
            "edit" => {
                let text = fact.description.strip_suffix(&format!(" at `{at}`")).unwrap_or(&fact.description);
                Some((format!("edit {class} {text}"), text.to_string()))
            }
            "body" => {
                let text = fact.description.strip_prefix(&format!("`{}`: ", at.item))?;
                let text = text.rsplit_once(", ").map(|(proof, _)| proof).unwrap_or(text);
                Some((format!("body {class} {text}"), text.to_string()))
            }
            _ => None,
        }
    };
    let mut groups: Vec<(String, String, Vec<usize>)> = Vec::new();
    for (index, fact) in facts.iter().enumerate() {
        let Some((k, text)) = key(fact) else { continue };
        match groups.iter_mut().find(|(g, _, _)| *g == k) {
            Some((_, _, members)) => members.push(index),
            None => groups.push((k, text, vec![index])),
        }
    }
    let mut target: Vec<usize> = (0..facts.len()).collect();
    let mut drop = vec![false; facts.len()];
    for (k, text, members) in groups.iter().filter(|(_, _, m)| m.len() > 1) {
        let first = members[0];
        let anchors: Vec<Anchor> = members.iter().filter_map(|&i| facts[i].at.clone()).collect();
        let mut affected: Vec<String> = members.iter().flat_map(|&i| facts[i].affected.clone()).collect();
        affected.sort();
        affected.dedup();
        let what = if k.starts_with("body") { "bodies" } else { "site(s)" };
        // A type fold names each place: the fold does not hide a site.
        let places: Vec<String> = if k.starts_with("type ") { members.iter().filter_map(|&i| type_change(&facts[i])).flat_map(|(_, _, places)| places).collect() } else { Vec::new() };
        let note = if places.is_empty() { String::new() } else { api_note(&facts[first].description).1.unwrap_or("").to_string() };
        let fact = &mut facts[first];
        fact.description = if places.is_empty() { format!("{} {what}: {text}", members.len()) } else { format!("{text} in {} places: {}{note}", places.len(), places.join(", ")) };
        fact.sites = vec![semilla_core::SiteGroup { edit: text.clone(), sites: anchors }];
        fact.affected = affected;
        for &other in &members[1..] {
            target[other] = first;
            drop[other] = true;
        }
    }
    // The new index of each kept fact.
    let mut new_index = vec![0; facts.len()];
    let mut next = 0;
    for index in 0..facts.len() {
        if !drop[index] {
            new_index[index] = next;
            next += 1;
        }
    }
    for slot in fact_of.iter_mut() {
        if let Some(index) = *slot {
            *slot = Some(new_index[target[index]]);
        }
    }
    let mut index = 0;
    facts.retain(|_| {
        index += 1;
        !drop[index - 1]
    });
    for (index, fact) in facts.iter_mut().enumerate() {
        fact.id = format!("f{}", index + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair_raw(old: &str, new: &str) -> (String, String) {
        short_texts(one_line(old), one_line(new))
    }

    fn pair(old: &str, new: &str) -> (String, String) {
        short_texts(one_line(old), one_line(new))
    }

    /// Brief 5h, C7: a short text of a fact has no cut in the middle of a token, and the cut of a
    /// pair stays around the first difference (batch 5d, C4).
    #[test]
    fn a_short_text_has_no_cut_in_a_token() {
        // The case of the trial: the old cut ended in the name `CARGO_MANIFEST_DIR`.
        let old = "/// The fee rules of the ledger, with each rate.\n/// # let x = 1;\n/// assert_eq!(fee(100), 1);";
        let new = "/// The fee rules of the ledger, with each rate.\n/// # std::fs::write(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../planted.txt\"), \"x\").unwrap();\n/// assert_eq!(fee(100), 1);";
        let (before, after) = pair(old, new);
        assert_ne!(before, after);
        assert!(after.contains("std::fs::write"), "the cut has the first difference: {after}");
        let whole = |text: &str, full: &str| {
            let full = full.split_whitespace().collect::<Vec<_>>().join(" ");
            let inner = text.trim_start_matches('…').trim_end_matches('…');
            let at = full.find(inner).unwrap_or_else(|| panic!("`{inner}` is not a part of `{full}`"));
            let chars = |text: &str, at: usize| text[..at].chars().count();
            let all: Vec<char> = full.chars().collect();
            assert!(token_boundary(&all, chars(&full, at)), "the start of `{text}` is in a token");
            assert!(token_boundary(&all, chars(&full, at + inner.len())), "the end of `{text}` is in a token");
        };
        whole(&before, old);
        whole(&after, new);
        assert!(!after.ends_with("CARGO_M…"), "{after}");

        // A difference in a long token: the cut keeps the full token.
        let (before, after) = pair(&format!("let total = {}_old + the_other_value_of_the_account_with_a_long_name;", "a".repeat(70)), &format!("let total = {}_new + the_other_value_of_the_account_with_a_long_name;", "a".repeat(70)));
        assert!(before.contains("_old") && after.contains("_new"), "{before} / {after}");

        // A long added text: the cut is before the token that does not fit.
        let text = short(&Some("pub fn a_function_with_a_long_name(first_parameter: u64, second_parameter_name: u64) -> u64".into()), &None);
        assert_eq!(text, "pub fn a_function_with_a_long_name(first_parameter: u64,…");
    }

    /// Batch 5k, D6 (trial 6, attempt 11): a cut text keeps each changed token. A cut is only in
    /// the equal parts, and each cut has the mark `…`. The white space in a literal does not
    /// change.
    #[test]
    fn a_cut_text_keeps_the_changed_tokens() {
        // The case of the trial: the change is `/ 2` at the end of a long expression.
        let (before, after) = pair("account.cents", "account.cents.saturating_sub(0).max(0).min(u64::MAX).checked_add(0).unwrap_or(0) / 2");
        assert_eq!(before, "account.cents");
        assert!(after.ends_with("/ 2") && after.contains("saturating_sub(0)"), "{after}");
        // A change at each end of a long text: the equal middle part is cut.
        let middle = "first_value_of_the_account + second_value_of_the_account + third_value_of_the_account + fourth_value";
        let (before, after) = pair(&format!("let a = 1 + {middle} / 2;"), &format!("let a = 7 + {middle} / 3;"));
        assert!(before.contains("1 +") && before.contains("/ 2") && after.contains("7 +") && after.contains("/ 3"), "{before} / {after}");
        assert!(before.matches('…').count() == 1 && after.matches('…').count() == 1 && before.chars().count() < 70, "the equal part has one cut: {before} / {after}");
        // A removed part: the side with no changed token shows the place of the change.
        let (before, after) = pair(&format!("{middle} + the_removed_value + the_last_value_of_the_account"), &format!("{middle} + the_last_value_of_the_account"));
        assert!(before.contains("the_removed_value") && after.contains("fourth_value + the_last_value"), "{before} / {after}");
        assert!(before.starts_with('…') && after.starts_with('…'), "{before} / {after}");

        // A long doc comment: the cut keeps the changed word, and it cuts the equal words.
        let (before, after) = pair("/// Takes `amount` and the fee from the account. Returns the new balance.", "/// Takes `amount` and the fee from the account. Returns the new cents.");
        assert_eq!((before.as_str(), after.as_str()), ("…Returns the new balance.", "…Returns the new cents."));

        // The white space in a literal.
        assert_eq!(one_line("let  a =\n    \"  \";"), "let a = \"  \";");
        assert_eq!(one_line("x = ' ' ; y = r#\"a  \"  b\"# ; z: &'a  str"), "x = ' ' ; y = r#\"a  \"  b\"# ; z: &'a str");
        assert_eq!(one_line("a // one  \"two\n  + b \"  \""), "a // one  \"two + b \"  \"");
        let (before, after) = pair_raw("pad(\" \")", "pad(\"  \")");
        assert_eq!((before.as_str(), after.as_str()), ("pad(\" \")", "pad(\"  \")"));
        assert_eq!(short(&Some("let pad = \"  \";".into()), &None), "let pad = \"  \";");
        assert_eq!(semilla_syntax::tree::short("let pad  = \"  \";", 60), "let pad = \"  \";");
    }
}
