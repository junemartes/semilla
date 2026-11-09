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
        EditKind::Signature => format!("signature: `{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
        EditKind::Docs => "doc comments or comments changed".to_string(),
        EditKind::Attrs => "attributes changed".to_string(),
        EditKind::Members if edit.anchor().is_some_and(|a| a.item.rsplit("::").next().is_some_and(|last| last.starts_with("impl "))) => "methods of the impl changed".to_string(),
        EditKind::Members => "fields or variants changed".to_string(),
        EditKind::Inserted => format!("new statement: `{}`", short(&edit.text_after)),
        EditKind::Deleted => format!("removed statement: `{}`", short(&edit.text_before)),
        EditKind::Moved => format!("moved statement: `{}`", short(&edit.text_before)),
        EditKind::Modified | EditKind::Replaced => format!("`{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
        EditKind::FileAdded => format!("new file `{}`", edit.file.as_deref().unwrap_or("")),
        EditKind::FileRemoved => format!("removed file `{}`", edit.file.as_deref().unwrap_or("")),
        EditKind::FileChanged => format!("changed file `{}`", edit.file.as_deref().unwrap_or("")),
    }
}

/// The name of the op that most likely explains an edit.
pub fn suggest(edit: &Edit) -> &'static str {
    let in_condition = edit.before.as_ref().is_some_and(|a| a.path.iter().any(|s| matches!(s, Segment::Role(r) if r == "cond" || r.ends_with(".guard"))));
    match edit.kind {
        EditKind::ItemAdded => "add-item",
        EditKind::ItemRemoved => "remove-item",
        EditKind::Visibility => "change-visibility",
        EditKind::Signature => "change-signature",
        EditKind::Docs => "edit-doc",
        EditKind::Attrs => "change-attribute",
        EditKind::Members if edit.anchor().is_some_and(|a| a.item.rsplit("::").next().is_some_and(|last| last.starts_with("impl "))) => "add-item (for the new or removed method)",
        EditKind::Members => "add-struct-field",
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
        _ if edit.block_like => "a structured statement op (add-if, add-arm, wrap-statements, …)",
        EditKind::Replaced | EditKind::Modified => "replace-expression",
        EditKind::FileChanged if edit.file.as_deref().is_some_and(|f| f.ends_with("Cargo.toml")) => "bump-dependency or edit-config",
        EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged => "edit-config",
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
            "add-struct-field" => {
                args.insert("field".into(), "?".into());
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
