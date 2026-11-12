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
    match edit.kind {
        EditKind::ItemAdded => format!("new {} `{}`", edit.attr_after.as_deref().unwrap_or("item"), edit.after.as_ref().map(|a| a.item.as_str()).unwrap_or("")),
        EditKind::ItemRemoved => format!("removed {} `{}`", edit.attr_before.as_deref().unwrap_or("item"), edit.before.as_ref().map(|a| a.item.as_str()).unwrap_or("")),
        EditKind::Visibility => format!("visibility: `{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
        EditKind::Signature => match edit.node_after.or(edit.node_before) {
            Some("param") => {
                let role = edit.anchor().and_then(|a| a.path.last()).map(|s| s.to_string()).unwrap_or_default();
                let name = role.trim_start_matches("param[").trim_end_matches(']');
                match (&edit.text_before, &edit.text_after) {
                    (None, Some(new)) => format!("parameter `{name}` added: `{}`", short(&Some(new.clone()))),
                    (Some(old), None) => format!("parameter `{name}` removed: `{}`", short(&Some(old.clone()))),
                    _ => format!("parameter `{name}` changed: `{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
                }
            }
            Some("params") => format!("parameter order: `{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
            Some("ret") => format!("return type: `{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
            Some("generics") => format!("generics: `{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
            Some("where") => format!("`where` clause: `{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
            Some("qualifiers") => format!("qualifiers: `{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
            // The view of the line is "signature" already.
            _ => format!("`{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
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
        EditKind::Modified | EditKind::Replaced => format!("`{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
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
        match syn::parse_str::<syn::Expr>(text.as_deref()?).ok()? {
            syn::Expr::Match(m) => Some(m.arms.iter().map(|arm| quote::ToTokens::to_token_stream(arm).to_string().trim_end_matches(" ,").to_string()).collect()),
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
        let bindings = map.iter().all(|(old, new)| !olds.contains(new) && crate::checks::is_binding(old_item, old));
        if consistent && injective && bindings {
            found.extend(map.into_iter().map(|(old, new)| (path.to_string(), old.to_string(), new.to_string())));
        }
    }
    found
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
