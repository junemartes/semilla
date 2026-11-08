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
        EditKind::Replaced if edit.node_before == Some("pattern") => "replace-pattern",
        _ if edit.block_like => "a structured statement op (add-if, add-arm, wrap-statements, …)",
        EditKind::Replaced | EditKind::Modified => "replace-expression",
        EditKind::FileChanged if edit.file.as_deref().is_some_and(|f| f.ends_with("Cargo.toml")) => "bump-dependency or edit-config",
        EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged => "edit-config",
    }
}

/// Builds claim suggestions for the uncovered edits of a step (the `suggest_claims` helper).
pub fn suggest_claims(edits: &[Edit], owners: &[Option<usize>]) -> Vec<Claim> {
    let mut claims = Vec::new();
    for (edit, owner) in edits.iter().zip(owners) {
        if owner.is_some() {
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
