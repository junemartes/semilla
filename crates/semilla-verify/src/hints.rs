//! Descriptions and claim suggestions for uncovered edits.
//!
//! The suggestions are untrusted help for the agent (design B21). The verifier never accepts a
//! suggestion without a claim.

use semilla_core::{Anchor, Claim, Segment};
use semilla_syntax::{Edit, EditKind};

use crate::checks;

/// A one-line description of an edit.
pub fn describe(edit: &Edit) -> String {
    let short = |text: &Option<String>| text.as_deref().map(|t| semilla_syntax::tree::short(t, 60)).unwrap_or_default();
    // The two sides of a change: the one function of batch 6a, A2.
    let pair = || crate::edits::sides_text(edit.text_before.as_deref().unwrap_or(""), edit.text_after.as_deref().unwrap_or(""));
    match edit.kind {
        EditKind::ItemAdded => format!("new {} `{}`", edit.attr_after.as_deref().unwrap_or("item"), edit.after.as_ref().map(|a| a.item.as_str()).unwrap_or("")),
        EditKind::ItemRemoved => format!("removed {} `{}`", edit.attr_before.as_deref().unwrap_or("item"), edit.before.as_ref().map(|a| a.item.as_str()).unwrap_or("")),
        EditKind::Visibility => format!("visibility: {}", pair()),
        EditKind::Signature => match edit.node_after.or(edit.node_before) {
            Some("param") => {
                let role = edit.anchor().and_then(|a| a.path.last()).map(|s| s.to_string()).unwrap_or_default();
                let name = role.trim_start_matches("param[").trim_end_matches(']');
                match (&edit.text_before, &edit.text_after) {
                    (None, Some(new)) => format!("parameter `{name}` added: `{}`", short(&Some(new.clone()))),
                    (Some(old), None) => format!("parameter `{name}` removed: `{}`", short(&Some(old.clone()))),
                    _ => format!("parameter `{name}` changed: {}", pair()),
                }
            }
            Some("params") => format!("parameter order: {}", pair()),
            Some("ret") => format!("return type: {}", pair()),
            Some("generics") => format!("generics: {}", pair()),
            Some("where") => format!("`where` clause: {}", pair()),
            Some("qualifiers") => format!("qualifiers: {}", pair()),
            // The view of the line is "signature" already.
            _ => format!("{}", pair()),
        },
        EditKind::Docs if crate::facts::doc_test_change(edit).is_some() => "doc test changed (a code block in a doc comment)".to_string(),
        EditKind::Docs => "doc comments changed".to_string(),
        EditKind::Comment => "a comment changed".to_string(),
        EditKind::Attrs => "attributes changed".to_string(),
        EditKind::Members if edit.anchor().is_some_and(|a| a.item.rsplit("::").next().is_some_and(|last| last.starts_with("impl "))) => "methods of the impl changed".to_string(),
        EditKind::Members => "fields or variants changed".to_string(),
        EditKind::Inserted => format!("new statement: `{}`", short(&edit.text_after)),
        EditKind::Deleted => format!("removed statement: `{}`", short(&edit.text_before)),
        EditKind::Moved => format!("moved statement: `{}`", short(&edit.text_before)),
        EditKind::Modified | EditKind::Replaced => format!("{}", pair()),
        EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged => {
            let what = match edit.kind {
                EditKind::FileAdded => "new",
                EditKind::FileRemoved => "removed",
                _ => "changed",
            };
            let note = edit.note.as_deref().map(|note| format!(" ({note})")).unwrap_or_default();
            format!("{what} file `{}`{note}", edit.file.as_deref().unwrap_or(""))
        }
    }
}

/// The arm op of a changed `match`: one new arm, one removed arm, or the same arms in another
/// order. `None` for another change.
fn arm_op(edit: &Edit) -> Option<&'static str> {
    let arms = |text: &Option<String>| -> Option<Vec<String>> {
        match semilla_syntax::rs::parse_expr(text.as_deref()?)? {
            semilla_syntax::rs::ast::Expr::MatchExpr(m) => Some(m.match_arm_list()?.arms().map(|arm| semilla_syntax::rs::compare(&arm).trim_end_matches(" ,").to_string()).collect()),
            _ => None,
        }
    };
    let (old, new) = (arms(&edit.text_before)?, arms(&edit.text_after)?);
    let one_more = |short: &[String], long: &[String]| long.len() == short.len() + 1 && (0..long.len()).any(|skip| {
        let mut rest = long.to_vec();
        rest.remove(skip);
        rest == short
    });
    if one_more(&old, &new) {
        return Some("add-arm");
    }
    if one_more(&new, &old) {
        return Some("remove-arm");
    }
    let (mut a, mut b) = (old.clone(), new.clone());
    a.sort();
    b.sort();
    (a == b && old != new).then_some("move-arm")
}

/// The name of the op that most likely explains an edit.
pub fn suggest(edit: &Edit) -> &'static str {
    let in_condition = edit.before.as_ref().is_some_and(|a| a.path.iter().any(|s| matches!(s, Segment::Role(r) if r == "cond" || r.ends_with(".guard"))));
    let owner = edit.attr_after.as_deref().or(edit.attr_before.as_deref()).unwrap_or("");
    if crate::facts::is_fact_edit(edit) {
        return "no claim: Semilla derives this fact";
    }
    match edit.kind {
        EditKind::ItemAdded => "add-item",
        EditKind::ItemRemoved => "remove-item",
        EditKind::Visibility | EditKind::Attrs => "no claim: Semilla derives this fact",
        EditKind::Signature => "change-signature",
        EditKind::Docs if crate::facts::doc_test_change(edit).is_some() => "no claim: Semilla derives this fact (a doc test is code)",
        EditKind::Docs => "edit-doc",
        EditKind::Comment => "no claim: Semilla derives this fact (a comment is not code)",
        EditKind::Members if edit.anchor().is_some_and(|a| a.item.rsplit("::").next().is_some_and(|last| last.starts_with("impl "))) => "add-item (for the new or removed method)",
        EditKind::Members if owner == "trait" => "add-item (for the new or removed trait method), or rewrite-item",
        EditKind::Members => "rewrite-item (no fact covers this member list)",
        EditKind::Inserted => match edit.node_after.unwrap_or("") {
            "let" => "add-let",
            "if" if edit.text_after.as_deref().is_some_and(|t| (t.contains("return") || t.contains("break") || t.contains("continue")) && !t.contains("else")) => "add-early-exit",
            "if" => "add-if",
            "match" => "add-match",
            "for" | "while" | "loop" => "add-loop",
            "return" => "add-return",
            "break" => "add-break",
            "continue" => "add-continue",
            "macro" => "add-macro-statement",
            _ => "add-expression-statement",
        },
        EditKind::Deleted => "delete-statement",
        EditKind::Moved => "move-statement",
        EditKind::Modified if matches!(edit.node_before, Some("binary" | "unary")) && edit.attr_before != edit.attr_after => "change-operator",
        EditKind::Modified if edit.node_before == Some("let") => "change-type-annotation",
        _ if in_condition && !edit.block_like => "change-condition",
        EditKind::Replaced if edit.node_before == Some("lit") && edit.node_after == Some("lit") => "change-literal",
        EditKind::Replaced if edit.node_before == Some("path") && edit.node_after == Some("path") => "change-path",
        EditKind::Replaced if edit.node_before == Some("pattern") && crate::checks::alternatives_added(edit.text_before.as_deref().unwrap_or(""), edit.text_after.as_deref().unwrap_or("")) => "add-alternative",
        EditKind::Replaced if edit.node_before == Some("pattern") && crate::checks::alternatives_added(edit.text_after.as_deref().unwrap_or(""), edit.text_before.as_deref().unwrap_or("")) => "remove-alternative",
        EditKind::Replaced if edit.node_before == Some("pattern") => "replace-pattern",
        EditKind::Replaced | EditKind::Modified if edit.node_before == Some("macro") && edit.node_after == Some("macro") => "replace-macro-input",
        _ if edit.node_before == Some("match") && edit.node_after == Some("match") && arm_op(edit).is_some() => arm_op(edit).unwrap(),
        _ if edit.block_like => "a structured statement op (add-if, add-arm, wrap-statements, …)",
        EditKind::Replaced | EditKind::Modified => "replace-expression",
        EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged => "no claim: Semilla derives this fact",
    }
}

/// Builds claim suggestions for the uncovered edits of a step (the `suggest_claims` helper).
pub fn suggest_claims(edits: &[Edit], owners: &[Option<usize>]) -> Vec<Claim> {
    suggest_claims_with_renames(edits, owners, &[])
}

/// Like [`suggest_claims`], with the likely renames: a rename claim replaces the pair of a
/// removed and an added item.
pub fn suggest_claims_with_renames(edits: &[Edit], owners: &[Option<usize>], renames: &[(String, String)]) -> Vec<Claim> {
    let mut claims = Vec::new();
    for (old, new) in renames {
        let mut args = serde_json::Map::new();
        args.insert("to".into(), new.clone().into());
        claims.push(Claim { id: format!("s{}", claims.len() + 1), op: "rename".into(), at: old.parse().ok(), args });
    }
    let in_rename = |edit: &Edit| {
        let item = edit.anchor().map(|a| a.item.as_str());
        renames.iter().any(|(old, new)| item == Some(old.as_str()) || item == Some(new.as_str()))
    };
    for (edit, owner) in edits.iter().zip(owners) {
        if owner.is_some() || in_rename(edit) {
            continue;
        }
        let op = suggest(edit);
        if op.contains(' ') {
            continue;
        }
        let at: Option<Anchor> = if op.starts_with("add-") && edit.kind == EditKind::Inserted { edit.after.clone() } else { edit.before.clone().or(edit.after.clone()) };
        let mut args = serde_json::Map::new();
        match op {
            "edit-config" | "bump-dependency or edit-config" => {
                args.insert("file".into(), edit.file.clone().unwrap_or_default().into());
            }
            "move-statement" => {
                args.insert("to".into(), edit.after.as_ref().map(|a| a.to_string()).unwrap_or_default().into());
            }
            _ => {}
        }
        let at = if checks_file_level(op) { None } else { at };
        claims.push(Claim { id: format!("s{}", claims.len() + 1), op: op.to_string(), at, args });
    }
    claims
}

fn checks_file_level(op: &str) -> bool {
    let _ = checks::local_types;
    matches!(op, "edit-config" | "bump-dependency")
}

/// Pairs of a removed item and an added item that look like a rename: the same kind, and the same
/// text after the old name becomes the new name.
pub fn likely_renames(edits: &[Edit], before: &semilla_syntax::Workspace, after: &semilla_syntax::Workspace) -> Vec<(String, String)> {
    let removed: Vec<&str> = edits.iter().filter(|e| e.kind == EditKind::ItemRemoved).filter_map(|e| e.before.as_ref().map(|a| a.item.as_str())).collect();
    let added: Vec<&str> = edits.iter().filter(|e| e.kind == EditKind::ItemAdded).filter_map(|e| e.after.as_ref().map(|a| a.item.as_str())).collect();
    let mut pairs = Vec::new();
    for old in &removed {
        let Some(old_item) = before.items.get(*old) else { continue };
        let old_name = old.rsplit("::").next().unwrap_or(old);
        for new in &added {
            let Some(new_item) = after.items.get(*new) else { continue };
            if old_item.kind != new_item.kind {
                continue;
            }
            let new_name = new.rsplit("::").next().unwrap_or(new);
            let renamed: String = old_item.text.split(' ').map(|token| if token == old_name { new_name } else { token }).collect::<Vec<_>>().join(" ");
            if renamed == new_item.text {
                pairs.push((old.to_string(), new.to_string()));
            }
        }
    }
    pairs
}

/// The likely renames of parameters and local variables: (item, old name, new name). The item
/// text changes only by one consistent change of identifiers, and each old name is a parameter
/// or a `let` or `for` variable. The new name does not occur in the old text.
pub fn likely_local_renames(edits: &[Edit], before: &semilla_syntax::Workspace, after: &semilla_syntax::Workspace) -> Vec<(String, String, String)> {
    let items: std::collections::BTreeSet<&str> = edits.iter().filter_map(|e| e.before.as_ref().or(e.after.as_ref())).map(|a| a.item.as_str()).collect();
    let is_ident = |t: &str| t.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') && t.chars().all(|c| c.is_alphanumeric() || c == '_');
    let mut found = Vec::new();
    for path in items {
        let (Some(old_item), Some(new_item)) = (before.items.get(path), after.items.get(path)) else { continue };
        let (old_tokens, new_tokens) = (words(&old_item.text), words(&new_item.text));
        if old_tokens.len() != new_tokens.len() || old_tokens == new_tokens {
            continue;
        }
        let mut map: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
        let consistent = old_tokens.iter().zip(&new_tokens).filter(|(a, b)| a != b).all(|(a, b)| is_ident(a) && is_ident(b) && *map.entry(*a).or_insert(*b) == *b);
        let olds: std::collections::BTreeSet<&str> = old_tokens.iter().copied().collect();
        let injective = map.values().collect::<std::collections::BTreeSet<_>>().len() == map.len();
        // Design 52.2: a rename of a local needs a sure binding on the two sides.
        let sure = |old: &str, new: &str| crate::sites::sure_binding(before, old_item, old) && crate::sites::sure_binding(after, new_item, new);
        let bindings = map.iter().all(|(old, new)| !olds.contains(new) && crate::checks::is_binding(old_item, old) && sure(old, new));
        if consistent && injective && bindings {
            found.extend(map.into_iter().map(|(old, new)| (path.to_string(), old.to_string(), new.to_string())));
        }
    }
    found
}

/// True if the renames of the step and the local renames `pairs` of one item give the new
/// text: the old text is equal to the new text after the map of the sites (design 51, rule
/// R5). A site of a local is a bare name. A member after `.`, a path with `::`, and a literal
/// are not sites. `item` is the path of the item before the step.
pub(crate) fn locals_explain(renames: &semilla_syntax::Renames, pairs: &[(String, String)], item: &str, old: &str, new: &str) -> bool {
    let mut renames = renames.clone();
    renames.idents.extend(pairs.iter().map(|(old, new)| (Some(item.to_string()), old.clone(), new.clone())));
    renames.exact() && renames.apply(item, old) == new
}

/// True if each use of `name` in the words of an item text can be a local variable or a
/// parameter (design 36.1, rule 7). A macro (`name!`), a part of a path (`a::name`), and a
/// field label of a struct literal or pattern are not such a use. A field after a dot
/// (`x.name`) is a different name: the rule does not count it. The rule is conservative: a
/// `false` result only keeps the edits as facts of their own.
pub(crate) fn only_local_uses(words: &[&str], name: &str) -> bool {
    // For each open bracket: true for a brace that can start a struct literal or pattern.
    let mut stack: Vec<bool> = Vec::new();
    for (index, word) in words.iter().enumerate() {
        let prev = |back: usize| index.checked_sub(back).map_or("", |at| words[at]);
        let next = |ahead: usize| words.get(index + ahead).copied().unwrap_or("");
        match *word {
            "{" => {
                let name_before = prev(1).chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_');
                stack.push(name_before || (prev(1) == ">" && prev(2) != "="));
            }
            "(" | "[" => stack.push(false),
            "}" | ")" | "]" => {
                stack.pop();
            }
            found if found == name => {
                if prev(1) == "." {
                    continue;
                }
                if next(1) == "!" || (prev(1) == ":" && prev(2) == ":") || (next(1) == ":" && next(2) == ":") {
                    return false;
                }
                let label = stack.last().copied().unwrap_or(false) && matches!(prev(1), "{" | ",") && matches!(next(1), ":" | "," | "}");
                if label {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// The words and the other characters of a token text, as [`words`], with each string literal
/// and each character literal as one word. So a text in a literal is never a name.
pub(crate) fn code_words(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let rest = &text[at..];
        // A raw string: `r"…"`, `r#"…"#`, with an optional `b` before.
        let raw = rest.strip_prefix("br").or_else(|| rest.strip_prefix('r')).filter(|_| at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_'));
        let hashes = raw.map(|tail| tail.len() - tail.trim_start_matches('#').len());
        let end = match (raw, hashes) {
            (Some(tail), Some(count)) if tail[count..].starts_with('"') => {
                let close = format!("\"{}", "#".repeat(count));
                let open = rest.len() - tail.len() + count + 1;
                Some(rest[open..].find(&close).map_or(rest.len(), |found| open + found + close.len()))
            }
            _ if rest.starts_with('"') => {
                let mut escaped = false;
                let close = rest[1..].char_indices().find(|(_, c)| {
                    let quote = !escaped && *c == '"';
                    escaped = !escaped && *c == '\\';
                    quote
                });
                Some(close.map_or(rest.len(), |(index, _)| index + 2))
            }
            _ if rest.starts_with('\'') => {
                // A character literal has its second quote after one character or one escape.
                let mut chars = rest[1..].char_indices();
                match chars.next() {
                    Some((_, '\\')) => rest[3..].find('\'').map(|found| found + 4),
                    Some((_, c)) => rest[1 + c.len_utf8()..].starts_with('\'').then_some(2 + c.len_utf8()),
                    None => None,
                }
            }
            _ => None,
        };
        if let Some(end) = end {
            out.push(&rest[..end]);
            at += end;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        if c.is_alphanumeric() || c == '_' {
            let len = rest.char_indices().find(|(_, c)| !(c.is_alphanumeric() || *c == '_')).map_or(rest.len(), |(index, _)| index);
            out.push(&rest[..len]);
            at += len;
        } else {
            if !c.is_whitespace() {
                out.push(&rest[..c.len_utf8()]);
            }
            at += c.len_utf8();
        }
    }
    out
}

/// The rename of a local variable or a parameter in an item that has other edits too (design
/// 36.1, rule 7). `pairs` are the name changes of the binding places: a parameter rename, or
/// an edit of a pattern that changes only one name. A pair counts when the old name is not in
/// the new text, the new name is not in the old text, and each use can be a local.
pub fn local_renames_with_edits(old_item: &semilla_syntax::Item, new_item: &semilla_syntax::Item, pairs: &[(String, String)]) -> Vec<(String, String)> {
    let (old_words, new_words) = (code_words(&old_item.text), code_words(&new_item.text));
    let mut found: Vec<(String, String)> = Vec::new();
    for (old, new) in pairs {
        // A use that is not a field after a dot.
        let used = |words: &[&str], name: &str| words.iter().enumerate().any(|(index, word)| *word == name && (index == 0 || words[index - 1] != "."));
        let fresh = !used(&new_words, old) && !used(&old_words, new);
        let plain = old != "_" && new != "_" && only_local_uses(&old_words, old) && only_local_uses(&new_words, new);
        let one_to_one = !found.iter().any(|(a, b)| a == old || b == new);
        if fresh && plain && one_to_one {
            found.push((old.clone(), new.clone()));
        }
    }
    found
}

/// The one name change of two texts: the words are equal except for one consistent change of
/// one identifier. `None` for each other difference.
pub(crate) fn one_name_change(before: &str, after: &str) -> Option<(String, String)> {
    let (old, new) = (code_words(before), code_words(after));
    if old.len() != new.len() {
        return None;
    }
    let is_ident = |t: &str| t.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') && t.chars().all(|c| c.is_alphanumeric() || c == '_');
    let mut pair: Option<(&str, &str)> = None;
    for (index, (a, b)) in old.iter().zip(&new).enumerate().filter(|(_, (a, b))| a != b) {
        // A name after a dot is a field or a method, not a local.
        if !is_ident(a) || !is_ident(b) || pair.is_some_and(|found| found != (*a, *b)) || (index > 0 && old[index - 1] == ".") {
            return None;
        }
        pair = Some((*a, *b));
    }
    pair.map(|(a, b)| (a.to_string(), b.to_string()))
}

/// The name change of a pattern edit (`x: u32` → `y: u32`): [`one_name_change`], when the old
/// name is in the binding part of the pattern, before its type annotation (design 45). A changed
/// type (`x: u32` → `x: u16`) is not a new name of a local: it changes the code.
pub(crate) fn pattern_name_change(before: &str, after: &str) -> Option<(String, String)> {
    let found = one_name_change(before, after)?;
    // The binding part: the text before the first `:` outside brackets that is not a part of `::`.
    let bytes = before.as_bytes();
    let mut depth = 0i32;
    let mut end = before.len();
    for (at, byte) in bytes.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b':' if depth == 0 && bytes.get(at + 1) != Some(&b':') && (at == 0 || bytes[at - 1] != b':') => {
                end = at;
                break;
            }
            _ => {}
        }
    }
    code_words(&before[..end]).contains(&found.0.as_str()).then_some(found)
}

/// True if the names of `pairs` are the only difference of two texts (and the texts differ).
pub(crate) fn only_names_change(before: &str, after: &str, pairs: &[(String, String)]) -> bool {
    let (old, new) = (code_words(before), code_words(after));
    // A name after a dot is a field or a method, not a local.
    old.len() == new.len() && old != new && old.iter().zip(&new).enumerate().all(|(index, (a, b))| a == b || (pairs.iter().any(|(from, to)| from == a && to == b) && (index == 0 || old[index - 1] != ".")))
}

/// The words and the other characters of a token text (a delimiter can touch a word: `(len)`).
pub(crate) fn words(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    for (index, c) in text.char_indices() {
        let word = c.is_alphanumeric() || c == '_';
        match (word, start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                out.push(&text[from..index]);
                start = None;
            }
            _ => {}
        }
        if !word && !c.is_whitespace() {
            out.push(&text[index..index + c.len_utf8()]);
        }
    }
    if let Some(from) = start {
        out.push(&text[from..]);
    }
    out
}

/// The `rename-local` claims of [`likely_local_renames`]: the function is the anchor, `from` is the old name.
pub fn local_rename_claims(renames: &[(String, String, String)]) -> Vec<Claim> {
    renames
        .iter()
        .enumerate()
        .map(|(index, (item, old, new))| {
            let mut args = serde_json::Map::new();
            args.insert("from".into(), old.clone().into());
            args.insert("to".into(), new.clone().into());
            Claim { id: format!("l{}", index + 1), op: "rename-local".into(), at: item.parse().ok(), args }
        })
        .collect()
}

/// The similarity of two items, from 0 to 1: the longest common token sequence of their texts,
/// with the item names replaced by one token. Large items use the token sets.
pub fn similarity(a: &semilla_syntax::Item, b: &semilla_syntax::Item) -> f64 {
    let name = |item: &semilla_syntax::Item| item.path.rsplit("::").next().unwrap_or("").to_string();
    let tokens = |item: &semilla_syntax::Item| -> Vec<String> {
        let own = name(item);
        item.text.split(' ').filter(|t| !t.is_empty()).map(|t| if t == own { "$name".to_string() } else { t.to_string() }).collect()
    };
    let (x, y) = (tokens(a), tokens(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    if x.len() * y.len() > 4_000_000 {
        let (sx, sy): (std::collections::BTreeSet<&String>, std::collections::BTreeSet<&String>) = (x.iter().collect(), y.iter().collect());
        return sx.intersection(&sy).count() as f64 / sx.union(&sy).count() as f64;
    }
    let mut previous = vec![0usize; y.len() + 1];
    for token in &x {
        let mut current = vec![0usize; y.len() + 1];
        for (j, other) in y.iter().enumerate() {
            current[j + 1] = if token == other { previous[j] + 1 } else { current[j].max(previous[j + 1]) };
        }
        previous = current;
    }
    2.0 * previous[y.len()] as f64 / (x.len() + y.len()) as f64
}
