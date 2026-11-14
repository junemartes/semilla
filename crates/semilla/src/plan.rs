//! The replay plan (design section 21.1): the agent submits all steps of a replay at once.
//!
//! - `replay plan` writes a draft plan. The engine splits the remaining hunks into steps (new
//!   items first, then renames and signature changes with their call sites, then the other
//!   changed items, then the removed items and the other files), and drafts the claims of each
//!   step from a simulation of the step.
//! - The agent edits the plan: the intents, the order, the claims, and the hand edits.
//! - `replay apply` starts again from the base, runs every step of the plan, and checks the whole
//!   change set. It reports all problems at once, so that the agent can fix the plan and apply it
//!   again.

use semilla_core::text::put_line;
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use semilla_core::{GroupKind, PairRefusal};
use semilla_syntax::Workspace;
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::git::{Hunk, apply_hunks};

/// A whole replay plan.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub steps: Vec<PlanStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<PlanGroup>,
    /// The pair warnings that the agent refuses, with a reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<PairRefusal>,
}

/// One step of a plan.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanStep {
    pub intent: String,
    /// The parts of the target to copy, as for `replay take`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub take: Vec<String>,
    /// Hand edits after the takes: each `find` must occur exactly once in its file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<PlanEdit>,
    #[serde(default)]
    pub claims: Vec<PlanClaim>,
    /// Notes of the draft for the agent (`apply` ignores them): the changes without a draft claim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todo: Vec<String>,
}

/// A hand edit. Without `word`, `find` must occur exactly once in `file`. With `word: true`,
/// every whole-word occurrence changes, and `file` can be `*` (all `.rs` files): use it for a
/// rename.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanEdit {
    pub file: String,
    pub find: String,
    pub replace: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub word: bool,
    /// With `field: true`, `find` is a field name: it changes only at the field positions
    /// (`x.find`, `find: …` in a struct, a struct literal, or a pattern), not as a local variable
    /// or in a comment. `file` can be `*`. Use it for the rename of a field.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub field: bool,
}

/// Replaces a field name at its field positions in a Rust source text: after `.` (not a method
/// call), and before `:` (not `::`) inside `{ }` after `{`, `,`, or `pub`. Comments and string
/// literals do not change. Returns the text and the count of changes.
pub fn replace_field(text: &str, field: &str, replace: &str) -> (String, usize) {
    let bytes = text.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut out = String::with_capacity(text.len());
    let mut count = 0;
    let mut brackets: Vec<u8> = Vec::new();
    let mut index = 0;
    let mut last = 0;
    let previous = |at: usize| -> (Option<u8>, &str) {
        let head = text[..at].trim_end();
        let word_start = head.rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '(' || c == ')')).map(|i| i + 1).unwrap_or(0);
        (head.bytes().last(), &head[word_start..])
    };
    while index < bytes.len() {
        let b = bytes[index];
        // A comment or a string literal: skip it.
        if b == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index = text[index..].find('\n').map(|i| index + i).unwrap_or(bytes.len());
            continue;
        }
        if b == b'"' {
            index += 1;
            while index < bytes.len() && bytes[index] != b'"' {
                index += if bytes[index] == b'\\' { 2 } else { 1 };
            }
            index += 1;
            continue;
        }
        match b {
            b'{' | b'(' | b'[' => brackets.push(b),
            b'}' | b')' | b']' => {
                brackets.pop();
            }
            _ => {}
        }
        let start_ok = index == 0 || !is_ident(bytes[index - 1]);
        if start_ok && text[index..].starts_with(field) && !bytes.get(index + field.len()).copied().is_some_and(is_ident) {
            let rest = text[index + field.len()..].trim_start();
            let (prev, prev_word) = previous(index);
            let access = prev == Some(b'.') && !text[..index].trim_end().ends_with("..") && !rest.starts_with('(') && !rest.starts_with("::");
            let member = brackets.last() == Some(&b'{') && rest.starts_with(':') && !rest.starts_with("::") && (matches!(prev, Some(b'{') | Some(b',')) || prev_word.starts_with("pub"));
            if access || member {
                out.push_str(&text[last..index]);
                out.push_str(replace);
                count += 1;
                index += field.len();
                last = index;
                continue;
            }
        }
        index += 1;
    }
    out.push_str(&text[last..]);
    (out, count)
}

/// Replaces each whole-word occurrence of `word` (an identifier) in `text`.
pub fn replace_word(text: &str, word: &str, replace: &str) -> (String, usize) {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut count = 0;
    let mut rest = text;
    let mut previous: Option<char> = None;
    while let Some(index) = rest.find(word) {
        let before = rest[..index].chars().last().or(previous);
        let after = rest[index + word.len()..].chars().next();
        out.push_str(&rest[..index]);
        if before.is_some_and(is_ident) || after.is_some_and(is_ident) {
            out.push_str(word);
        } else {
            out.push_str(replace);
            count += 1;
        }
        previous = word.chars().last();
        rest = &rest[index + word.len()..];
    }
    out.push_str(rest);
    (out, count)
}

/// A claim of a plan. `at` is an anchor, or a position:
/// - `base:FILE:LINE[:COLUMN]`: old code, at its line in the base tree;
/// - `target:FILE:LINE[:COLUMN]`: new code, at its line in the target tree;
/// - `before:…` and `after:…`: a line in the tree before or after the step itself.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanClaim {
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Go this many levels up from the node at the position.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub up: usize,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub args: serde_json::Map<String, serde_json::Value>,
}

/// Merges draft expression claims under one parent: when one claim's anchor is an ancestor of
/// the others in the same item, one `replace-expression` on the ancestor replaces them all (if the
/// ancestor contains no block).
fn collapse_expression_claims(claims: &mut Vec<semilla_core::Claim>, after: &Workspace, before: &Workspace) {
    let expression = |op: &str| matches!(op, "replace-expression" | "change-operator" | "change-literal" | "change-path");
    let mut index = 0;
    while index < claims.len() {
        let Some(at) = claims[index].at.clone().filter(|_| expression(&claims[index].op)) else {
            index += 1;
            continue;
        };
        let inside: Vec<usize> = claims
            .iter()
            .enumerate()
            .filter(|(j, c)| *j != index && expression(&c.op) && c.at.as_ref().is_some_and(|a| a != &at && at.contains(a)))
            .map(|(j, _)| j)
            .collect();
        let block_free = semilla_syntax::resolve_node(before, &at).or_else(|| semilla_syntax::resolve_node(after, &at)).is_some_and(|node| !node.has_block());
        if inside.is_empty() || !block_free {
            index += 1;
            continue;
        }
        claims[index].op = "replace-expression".into();
        claims[index].args.clear();
        for j in inside.into_iter().rev() {
            claims.remove(j);
            if j < index {
                index -= 1;
            }
        }
        index += 1;
    }
}

/// The intent of a draft step from its claims: "add `a`; change `b`; remove `c`". A new item
/// inside a new item (a test in a new test module) is not named.
fn draft_intent(claims: &[semilla_core::Claim]) -> Option<String> {
    let item = |claim: &semilla_core::Claim| claim.at.as_ref().map(|a| a.item.clone());
    let short = |path: &str| path.rsplit("::").next().unwrap_or(path).to_string();
    let added: Vec<String> = claims.iter().filter(|c| c.op == "add-item").filter_map(item).filter(|p| !p.contains("use#")).collect();
    let added: Vec<String> = added.iter().filter(|p| !added.iter().any(|q| p.starts_with(&format!("{q}::")))).cloned().collect();
    let removed: Vec<String> = claims.iter().filter(|c| c.op == "remove-item").filter_map(item).collect();
    let mut changed: Vec<String> = Vec::new();
    for path in claims.iter().filter(|c| c.op != "add-item" && c.op != "remove-item").filter_map(item) {
        if !added.contains(&path) && !changed.contains(&path) {
            changed.push(path);
        }
    }
    let files: Vec<String> = claims.iter().filter(|c| c.at.is_none()).filter_map(|c| c.arg_str("file").map(str::to_string)).collect();
    let list = |items: &[String]| items.iter().map(|i| format!("`{}`", short(i))).collect::<Vec<_>>().join(", ");
    let mut parts = Vec::new();
    for (verb, items) in [("add", &added), ("change", &changed), ("remove", &removed), ("change", &files)] {
        if !items.is_empty() {
            parts.push(format!("{verb} {}", list(items)));
        }
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanGroup {
    pub id: String,
    pub kind: GroupKind,
    pub title: String,
    #[serde(default)]
    pub why: String,
    pub steps: Vec<usize>,
}

/// The result of `replay apply`.
pub struct Applied {
    pub text: String,
    /// True when every step applied, every claim was accepted, and phase 1 passed.
    pub ok: bool,
}

/// Maps a line through the hunks of one file (from the "old" to the "new" version). `None` when
/// the line itself changed.
pub fn map_line(hunks: &[Hunk], line: usize) -> Option<usize> {
    let mut offset: isize = 0;
    for hunk in hunks {
        if hunk.old_count > 0 && hunk.old_start <= line && line < hunk.old_start + hunk.old_count {
            return None;
        }
        let ends_before = if hunk.old_count == 0 { hunk.old_start < line } else { hunk.old_start + hunk.old_count <= line };
        if ends_before {
            offset += hunk.new_count as isize - hunk.old_count as isize;
        }
    }
    usize::try_from(line as isize + offset).ok()
}

/// Splits `FILE:LINE[:COLUMN]`.
fn parse_position(text: &str) -> Result<(String, usize, Option<usize>)> {
    let parts: Vec<&str> = text.rsplitn(3, ':').collect();
    match parts.as_slice() {
        [column, line, file] if line.parse::<usize>().is_ok() && column.parse::<usize>().is_ok() => Ok((file.to_string(), line.parse()?, Some(column.parse()?))),
        [line, rest @ ..] if line.parse::<usize>().is_ok() => {
            let file = rest.iter().rev().cloned().collect::<Vec<_>>().join(":");
            Ok((file, line.parse()?, None))
        }
        _ => bail!("`{text}` is not FILE:LINE[:COLUMN]"),
    }
}

impl App {
    /// Resolves the anchor of a plan claim in the step from `before` to `after`.
    fn plan_anchor(&mut self, claim: &PlanClaim, base: &str, target: &str, before: &str, after: &str) -> Result<Option<String>> {
        let Some(at) = &claim.at else { return Ok(None) };
        let (side, rest) = match at.split_once(':') {
            Some((side @ ("base" | "target" | "before" | "after"), rest)) => (side, rest),
            _ => {
                anyhow::ensure!(claim.up == 0, "`up` needs a position, not an anchor");
                return Ok(Some(at.clone()));
            }
        };
        let (file, line, column) = parse_position(rest)?;
        let (tree, line) = match side {
            "base" => {
                let hunks = self.repo.hunks(base, before, Some(&file))?;
                (before, map_line(&hunks, line).with_context(|| format!("{at}: an earlier step changed this line; use a `before:` position"))?)
            }
            "target" => {
                let hunks = self.repo.hunks(target, after, Some(&file))?;
                (after, map_line(&hunks, line).with_context(|| format!("{at}: this line is not in the tree after the step yet"))?)
            }
            "before" => (before, line),
            _ => (after, line),
        };
        let ws = self.workspace(tree)?;
        let mut anchor = semilla_syntax::anchor_at(&ws, &file, line, column.map(|c| c.saturating_sub(1))).with_context(|| format!("{at}: no item at {file}:{line}"))?;
        for _ in 0..claim.up {
            anchor = anchor.parent().with_context(|| format!("{at}: the anchor has no parent inside its item"))?;
        }
        Ok(Some(anchor.to_string()))
    }

    /// Writes a draft plan for the remaining replay: from the base to the target.
    pub fn replay_plan(&mut self) -> Result<Plan> {
        let session = self.load_session()?;
        let replay = session.replay.clone().context("no replay is in progress; start one with `semilla replay begin`")?;
        let base = session.changeset.base_tree.clone();
        let target = replay.target.clone();
        let (base_ws, target_ws) = (self.workspace(&base)?, self.workspace(&target)?);
        let hunks = self.repo.hunks(&base, &target, None)?;

        // 1. Each hunk touches the innermost item on each side (or its file). A hunk can replace
        //    one item with another, so the items of one hunk must go in one step.
        let innermost = |ws: &Workspace, file: &str, (lo, hi): (usize, usize)| -> Option<String> {
            ws.items
                .values()
                .filter(|item| item.file == file && item.line_start <= hi && lo <= item.line_end && item.kind != semilla_syntax::ItemKind::Use)
                .min_by_key(|item| item.line_end - item.line_start)
                .map(|item| item.path.clone())
        };
        let mut parent: BTreeMap<String, String> = BTreeMap::new();
        fn root(parent: &mut BTreeMap<String, String>, key: &str) -> String {
            let next = parent.entry(key.to_string()).or_insert_with(|| key.to_string()).clone();
            if next == key {
                return next;
            }
            let found = root(parent, &next);
            parent.insert(key.to_string(), found.clone());
            found
        }
        fn join(parent: &mut BTreeMap<String, String>, a: &str, b: &str) {
            let (ra, rb) = (root(parent, a), root(parent, b));
            if ra != rb {
                parent.insert(ra, rb);
            }
        }
        let mut unit_hunks: BTreeMap<String, Vec<&Hunk>> = BTreeMap::new();
        for hunk in &hunks {
            let mut keys = Vec::new();
            if hunk.file.ends_with(".rs") {
                if hunk.new_count > 0 {
                    keys.extend(innermost(&target_ws, &hunk.file, hunk.new_range()));
                }
                if hunk.old_count > 0 {
                    keys.extend(innermost(&base_ws, &hunk.file, hunk.old_range()));
                }
            }
            if keys.is_empty() {
                keys.push(format!("file:{}", hunk.file));
            }
            keys.dedup();
            for key in &keys {
                root(&mut parent, key);
                join(&mut parent, &keys[0], key);
            }
            unit_hunks.entry(keys[0].clone()).or_default().push(hunk);
        }

        // 2. The pairs: a removed item and an added item that look like one item. A pair and a
        //    signature change go with the changed items whose hunks use the name.
        let name = |path: &str| path.rsplit("::").next().unwrap_or(path).to_string();
        let words = |hunks: &[&Hunk]| -> BTreeSet<String> {
            hunks.iter().flat_map(|h| h.lines.iter()).flat_map(|line| line.split(|c: char| !(c.is_alphanumeric() || c == '_')).map(str::to_string).collect::<Vec<_>>()).collect()
        };
        let keys: Vec<String> = parent.keys().cloned().collect();
        let removed: Vec<String> = keys.iter().filter(|u| base_ws.items.contains_key(*u) && !target_ws.items.contains_key(*u)).cloned().collect();
        let added: Vec<String> = keys.iter().filter(|u| target_ws.items.contains_key(*u) && !base_ws.items.contains_key(*u)).cloned().collect();
        let mut candidates: Vec<(f64, String, String)> = Vec::new();
        for old in &removed {
            for new in added.iter().filter(|new| base_ws.items[old].kind == target_ws.items[*new].kind) {
                let score = semilla_verify::hints::similarity(&base_ws.items[old], &target_ws.items[new]);
                if score >= semilla_verify::PAIR_THRESHOLD {
                    candidates.push((score, old.clone(), new.clone()));
                }
            }
        }
        candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut pairs: Vec<(String, String, f64)> = Vec::new();
        for (score, old, new) in candidates {
            if !pairs.iter().any(|(o, n, _)| *o == old || *n == new) {
                pairs.push((old, new, score));
            }
        }
        let hunk_words: BTreeMap<String, BTreeSet<String>> = unit_hunks.iter().map(|(key, hunks)| (key.clone(), words(hunks))).collect();
        let users = |word: &str| -> Vec<String> { hunk_words.iter().filter(|(_, w)| w.contains(word)).map(|(key, _)| key.clone()).collect() };
        let owner = |path: &str| path.rsplit_once("::").map(|(p, _)| p.to_string()).unwrap_or_default();
        // A pair with one parent is a rename: its own step renames the item and all its users,
        // so the users do not join the pair, and the pair is one changed item (not new, not removed).
        let renamed: Vec<(String, String)> = pairs.iter().filter(|(old, new, _)| owner(old) == owner(new)).map(|(old, new, _)| (old.clone(), new.clone())).collect();
        let is_pair_old = |u: &String| renamed.iter().any(|(old, _)| old == u);
        let is_pair_new = |u: &String| renamed.iter().any(|(_, new)| new == u);
        for (old, new, _) in &pairs {
            join(&mut parent, old, new);
            if owner(old) != owner(new) {
                for user in users(&name(old)) {
                    join(&mut parent, old, &user);
                }
            }
        }
        let signature_changes: Vec<String> = keys.iter().filter(|u| base_ws.items.get(*u).zip(target_ws.items.get(*u)).is_some_and(|(a, b)| a.signature != b.signature)).cloned().collect();
        for item in &signature_changes {
            for user in users(&name(item)) {
                join(&mut parent, item, &user);
            }
        }
        // A unit that uses a new item goes in the step that adds it, so that each step compiles.
        for item in added.iter().filter(|item| !is_pair_new(item)) {
            for user in users(&name(item)) {
                join(&mut parent, item, &user);
            }
        }

        // 3. One step for each connected set of units, in a safe order: renames first (an identity
        //    step has no other change, design 22.4), then new items, moves, signature changes,
        //    other changes, removed items, and the other files.
        let mut components: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for key in &keys {
            let found = root(&mut parent, key);
            components.entry(found).or_default().push(key.clone());
        }
        let mut units: BTreeMap<String, Vec<&Hunk>> = BTreeMap::new();
        for (key, hunks) in &unit_hunks {
            units.insert(key.clone(), hunks.clone());
        }
        let mut drafts: Vec<(u8, String, Vec<String>, Vec<PlanEdit>)> = Vec::new();
        let mut new_by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let list = |items: &[String]| items.iter().map(|i| format!("`{}`", name(i))).collect::<Vec<_>>().join(", ");
        // The honest split of a pair: a rename (a neutral claim), then the other changes. The
        // components below take the hunks that remain after the renames.
        for (old, new) in &renamed {
            // A field changes at its field positions only, so a local variable with the same name
            // and the comments keep the old name (the rename step must have no other change).
            let field = base_ws.items.get(old).is_some_and(|item| item.kind == semilla_syntax::ItemKind::Field);
            let short = |path: &str| if field { path.rsplit_once('.').map(|(_, f)| f.to_string()).unwrap_or_else(|| name(path)) } else { name(path) };
            let rename = PlanEdit { file: "*".into(), find: short(old), replace: short(new), word: !field, field };
            drafts.push((0, format!("rename `{}` to `{}`", name(old), name(new)), Vec::new(), vec![rename]));
        }
        for members in components.into_values() {
            let is_new = |u: &String| target_ws.items.contains_key(u) && !base_ws.items.contains_key(u) && !is_pair_new(u);
            let is_removed = |u: &String| base_ws.items.contains_key(u) && !target_ws.items.contains_key(u) && !is_pair_old(u);
            if let Some((old, new, score)) = pairs.iter().find(|(old, new, _)| members.contains(old) && owner(old) != owner(new)) {
                // A pair with the same text has no change for a later step.
                let rest = if *score >= 0.9995 { "the move in a step with no other change" } else { "the move in its own step, then the changes in a later step" };
                drafts.push((1, format!("move `{old}` to `{new}` ({:.0}% similar: {rest})", score * 100.0), members, Vec::new()));
            } else if let Some(item) = members.iter().find(|m| signature_changes.contains(m)) {
                drafts.push((2, format!("change the signature of `{}` and its call sites", name(item)), members, Vec::new()));
            } else if members.iter().all(is_new) {
                let file = target_ws.items[&members[0]].file.clone();
                new_by_file.entry(file).or_default().extend(members);
            } else if members.iter().all(is_removed) {
                drafts.push((4, format!("remove {}", list(&members)), members, Vec::new()));
            } else if members.iter().all(|m| m.starts_with("file:") && !m.ends_with(".rs")) {
                drafts.push((5, format!("change {}", members.iter().map(|m| format!("`{}`", &m[5..])).collect::<Vec<_>>().join(", ")), members, Vec::new()));
            } else {
                let (new, gone): (Vec<String>, Vec<String>) = (members.iter().filter(|m| is_new(m)).cloned().collect(), members.iter().filter(|m| is_removed(m)).cloned().collect());
                // The old item of a renamed pair is the new item, under its old name.
                let changed: Vec<String> = members.iter().filter(|m| !is_new(m) && !is_removed(m) && !is_pair_old(m)).cloned().collect();
                let mut parts = Vec::new();
                if !new.is_empty() {
                    parts.push(format!("add {}", list(&new)));
                }
                if !changed.is_empty() {
                    parts.push(format!("change {}", list(&changed)));
                }
                if !gone.is_empty() {
                    parts.push(format!("remove {}", list(&gone)));
                }
                drafts.push((3, parts.join("; "), members, Vec::new()));
            }
        }
        for (file, items) in new_by_file {
            drafts.push((0, format!("add {} in `{file}`", list(&items)), items, Vec::new()));
        }
        drafts.sort_by_key(|(order, _, _, _)| *order);

        // 4. The takes of each step, and the draft claims from a simulation of the steps.
        let mut current_tree = base.clone();
        let mut contents: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut plan = Plan::default();
        for (_, intent, members, edits) in drafts {
            let step_hunks: Vec<&Hunk> = members.iter().flat_map(|unit| units.get(unit).cloned().unwrap_or_default()).collect();
            // Only the hunks that still remain: an earlier step (a rename) can make one equal.
            let mut take = Vec::new();
            for hunk in &step_hunks {
                let remaining = self.repo.hunks(&current_tree, &target, Some(&hunk.file))?;
                if remaining.iter().any(|r| overlaps(r.new_range(), hunk.new_range())) {
                    take.push(format!("{}:{}-{}", hunk.file, hunk.new_range().0, hunk.new_range().1));
                }
            }
            take.dedup();
            if take.is_empty() && edits.is_empty() {
                continue;
            }
            let step = PlanStep { intent, take, edits, ..Default::default() };
            let (changed, problems) = self.step_contents(&current_tree, &target, &step)?;
            if !problems.is_empty() {
                bail!("the draft of step `{}` failed: {}", step.intent, problems.join("; "));
            }
            contents.extend(changed);
            let after_tree = self.repo.tree_with(&base, &contents)?;
            let (before_ws, after_ws) = (self.workspace(&current_tree)?, self.workspace(&after_tree)?);
            // The renames first: the other drafts use the diff after the rename map.
            let plain = semilla_syntax::diff(&before_ws, &after_ws, &Default::default());
            let renames = semilla_verify::hints::likely_renames(&plain, &before_ws, &after_ws);
            let mut claims = semilla_verify::hints::suggest_claims_with_renames(&[], &[], &renames);
            claims.extend(semilla_verify::hints::local_rename_claims(&semilla_verify::hints::likely_local_renames(&plain, &before_ws, &after_ws)));
            let rename_step = semilla_core::Step { index: 0, intent: String::new(), claims: claims.clone(), tree_before: String::new(), tree_after: String::new(), reasons: Default::default() };
            let map = semilla_verify::checks::renames(&rename_step, &before_ws);
            let edits = semilla_syntax::diff(&before_ws, &after_ws, &map);
            let owners = vec![None; edits.len()];
            let mut rest = semilla_verify::hints::suggest_claims(&edits, &owners);
            collapse_expression_claims(&mut rest, &after_ws, &before_ws);
            claims.extend(rest);
            let mut todo: Vec<String> = edits
                .iter()
                .filter(|edit| { let hint = semilla_verify::hints::suggest(edit); hint.starts_with("rename (") || hint.starts_with("move-item (") })
                .map(|edit| format!("{}: {} (at {})", semilla_verify::hints::suggest(edit), semilla_verify::hints::describe(edit), edit.before.as_ref().or(edit.after.as_ref()).map(|a| a.to_string()).unwrap_or_default()))
                .collect();
            for edit in edits.iter().filter(|e| e.kind == semilla_syntax::EditKind::ItemAdded) {
                let Some(item) = edit.after.as_ref().and_then(|a| after_ws.items.get(&a.item)) else { continue };
                if item.body.is_some() && !item.test {
                    todo.push(format!("new function `{}`: add the property claims that are true (no-panic, no-unsafe, no-io, calls-only, tested-by)", item.path));
                }
            }
            // The intent names the items of the claims: the units name only the innermost items.
            let intent = if step.edits.is_empty() { draft_intent(&claims).unwrap_or(step.intent.clone()) } else { step.intent.clone() };
            plan.steps.push(PlanStep {
                intent,
                // Design 24.2: only the claims that stay go into the plan. Semilla derives the rest,
                // also the renames and the moves of a step with no other change.
                claims: claims.into_iter().filter(|c| crate::onestep::CLAIMS.contains(&c.op.as_str())).map(|c| PlanClaim { op: c.op, at: c.at.map(|a| a.to_string()), up: 0, args: c.args }).collect(),
                todo,
                ..step
            });
            current_tree = after_tree;
        }
        Ok(plan)
    }

    /// The new content of each file that a plan step changes, from the tree before the step:
    /// first the takes (all hunks of one file come from one diff), then the hand edits. Returns
    /// the problems too (a take that matches no hunk, a `find` text that is not unique).
    pub fn step_contents(&mut self, before: &str, target: &str, step: &PlanStep) -> Result<(BTreeMap<String, Option<String>>, Vec<String>)> {
        enum Range {
            New(usize, usize),
            Old(usize, usize),
            Whole,
        }
        let mut problems = Vec::new();
        let (before_ws, target_ws) = (self.workspace(before)?, self.workspace(target)?);
        let mut by_file: BTreeMap<String, Vec<(String, Range)>> = BTreeMap::new();
        for spec in &step.take {
            let spec = spec.trim_start_matches("./");
            let (file, range) = if spec.contains("::") && !spec.contains('/') {
                match (target_ws.items.get(spec), before_ws.items.get(spec)) {
                    (Some(item), _) => (item.file.clone(), Range::New(item.line_start, item.line_end)),
                    (None, Some(item)) => (item.file.clone(), Range::Old(item.line_start, item.line_end)),
                    (None, None) => {
                        problems.push(format!("take {spec}: no item {spec} before the step or in the target"));
                        continue;
                    }
                }
            } else {
                match spec.rsplit_once(':') {
                    Some((file, lines)) if lines.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
                        let parsed = match lines.split_once('-') {
                            Some((a, b)) => a.parse::<usize>().ok().zip(b.parse::<usize>().ok()),
                            None => lines.parse::<usize>().ok().map(|l| (l, l)),
                        };
                        let Some((lo, hi)) = parsed else {
                            problems.push(format!("take {spec}: use FILE:LINE or FILE:START-END"));
                            continue;
                        };
                        (file.to_string(), Range::New(lo, hi))
                    }
                    _ => (spec.to_string(), Range::Whole),
                }
            };
            by_file.entry(file).or_default().push((spec.to_string(), range));
        }
        let before_dir = self.repo.materialize(before)?;
        let target_dir = self.repo.materialize(target)?;
        let mut contents: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (file, specs) in by_file {
            let old = std::fs::read_to_string(before_dir.join(&file)).ok();
            let new = std::fs::read_to_string(target_dir.join(&file)).ok();
            if specs.iter().any(|(_, range)| matches!(range, Range::Whole)) {
                if old.is_none() && new.is_none() {
                    problems.push(format!("take {file}: no such file before the step or in the target"));
                }
                contents.insert(file, new);
                continue;
            }
            let hunks = self.repo.hunks(before, target, Some(&file))?;
            let mut chosen: Vec<&Hunk> = Vec::new();
            for (spec, range) in &specs {
                let matching: Vec<&Hunk> = hunks
                    .iter()
                    .filter(|h| match range {
                        Range::New(lo, hi) => overlaps(h.new_range(), (*lo, *hi)),
                        Range::Old(lo, hi) => overlaps(h.old_range(), (*lo, *hi)),
                        Range::Whole => false,
                    })
                    .collect();
                if matching.is_empty() {
                    problems.push(format!("take {spec}: no remaining hunk matches; `semilla replay status` lists the hunks"));
                }
                for hunk in matching {
                    if !chosen.contains(&hunk) {
                        chosen.push(hunk);
                    }
                }
            }
            if chosen.is_empty() {
                continue;
            }
            let content = if chosen.len() == hunks.len() { new } else { Some(apply_hunks(old.as_deref().unwrap_or(""), new.as_deref().unwrap_or(""), &chosen)) };
            contents.insert(file, content);
        }
        // The hand edits work on the result of the takes.
        for edit in &step.edits {
            let files: Vec<String> = if edit.file == "*" {
                before_ws.rust_files.keys().cloned().chain(contents.keys().filter(|f| f.ends_with(".rs")).cloned()).collect::<BTreeSet<_>>().into_iter().collect()
            } else {
                vec![edit.file.clone()]
            };
            let mut total = 0;
            for file in files {
                let current = match contents.get(&file) {
                    Some(content) => content.clone(),
                    None => std::fs::read_to_string(before_dir.join(&file)).ok(),
                };
                let Some(current) = current else {
                    if edit.file != "*" {
                        problems.push(format!("edit {file}: no such file"));
                    }
                    continue;
                };
                if edit.word || edit.field {
                    let (changed, count) = if edit.field { replace_field(&current, &edit.find, &edit.replace) } else { replace_word(&current, &edit.find, &edit.replace) };
                    if count > 0 {
                        total += count;
                        contents.insert(file, Some(changed));
                    }
                } else {
                    match current.matches(&edit.find).count() {
                        1 => {
                            total += 1;
                            contents.insert(file, Some(current.replacen(&edit.find, &edit.replace, 1)));
                        }
                        0 => problems.push(format!("edit {file}: the `find` text is not in the file")),
                        count => problems.push(format!("edit {file}: the `find` text occurs {count} times; make it unique, or set `word: true`")),
                    }
                }
            }
            if (edit.word || edit.field) && total == 0 {
                problems.push(format!("edit {}: no {} `{}`", edit.file, if edit.field { "field use of" } else { "whole-word" }, edit.find));
            }
        }
        Ok((contents, problems))
    }

    /// Applies a whole plan: starts again from the base of the replay, runs each step, and checks
    /// the change set (phase 1, or both phases with `full`).
    pub fn replay_apply(&mut self, plan: &Plan, full: bool) -> Result<Applied> {
        let mut session = self.load_session()?;
        let replay = session.replay.clone().context("no replay is in progress; start one with `semilla replay begin`")?;
        let base = session.changeset.base_tree.clone();
        // Start again from the base: the plan is the whole replay. A file that the target does not
        // have would be lost, so it stops the apply.
        let current = self.repo.write_tree()?;
        let foreign = self.repo.git(&["diff", "--name-only", "--no-renames", "--diff-filter=A", &replay.target, &current])?;
        if !foreign.is_empty() {
            bail!(
                "the working tree has files that the target does not have: {}; move them out of the repository or into `.semilla/` (for example `.semilla/plan.json`), because the apply starts again from the base",
                foreign.lines().collect::<Vec<_>>().join(", ")
            );
        }
        self.repo.switch_worktree(&current, &base)?;
        session.open = None;
        session.changeset.steps.clear();
        session.changeset.groups.clear();
        session.changeset.subgroups.clear();
        session.changeset.head_tree = base.clone();
        // The refusals of the plan, then the refusals from `pair refuse` (they stay across applies).
        let mut refusals = plan.refusals.clone();
        for refusal in &session.changeset.pair_refusals {
            if !refusals.iter().any(|r| r.old == refusal.old && r.new == refusal.new) {
                refusals.push(refusal.clone());
            }
        }
        session.changeset.pair_refusals = refusals;
        session.last_claim = 0;
        self.save_session(&session)?;

        let mut text = String::new();
        let mut problems = 0;
        for (index, step) in plan.steps.iter().enumerate() {
            let mut lines: Vec<String> = Vec::new();
            self.step_begin(&step.intent)?;
            let before = self.repo.write_tree()?;
            let (changed, step_problems) = self.step_contents(&before, &replay.target, step)?;
            lines.extend(step_problems.into_iter().map(|p| format!("✗ {p}")));
            for (file, content) in changed {
                let path = self.repo.root.join(&file);
                match content {
                    Some(text) => {
                        std::fs::create_dir_all(path.parent().unwrap())?;
                        std::fs::write(&path, text)?;
                    }
                    None => {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
            let after = self.repo.write_tree()?;
            for claim in &step.claims {
                let at = match self.plan_anchor(claim, &base, &replay.target, &before, &after) {
                    Ok(at) => at,
                    Err(error) => {
                        lines.push(format!("✗ claim {}: {error:#}", claim.op));
                        continue;
                    }
                };
                if let Err(error) = self.claim(&claim.op, at.as_deref(), claim.args.clone()) {
                    lines.push(format!("✗ claim {} at {}: {error:#}", claim.op, at.unwrap_or_default()));
                }
            }
            self.step_end_with(false)?;
            problems += lines.len();
            let mark = if lines.is_empty() { "✓" } else { "✗" };
            put_line(&mut text, &format!("{mark} plan step {index}: {}", semilla_core::text::quoted(&step.intent)));
            for line in lines {
                put_line(&mut text, &format!("    {line}"));
            }
        }
        let mut session = self.load_session()?;
        for group in &plan.groups {
            if let Err(error) = self.group_set(&group.id, group.kind, &group.title, &group.why, group.steps.clone()) {
                put_line(&mut text, &format!("✗ group {}: {error:#}", group.id));
                problems += 1;
            }
        }
        // A step that no plan group names is its own group, with its intent as the title, as with
        // `semilla step` (design 24.3).
        let mut number = plan.groups.len();
        for (index, step) in plan.steps.iter().enumerate() {
            if plan.groups.iter().any(|g| g.steps.contains(&index)) {
                continue;
            }
            number += 1;
            let mut id = format!("G{number}");
            while plan.groups.iter().any(|g| g.id == id) {
                number += 1;
                id = format!("G{number}");
            }
            self.group_set(&id, GroupKind::Behavior, &step.intent, "", vec![index])?;
        }
        session = self.load_session().unwrap_or(session);
        let remaining = self.replay_remaining(&session)?.unwrap_or_default();
        let at_target = self.repo.write_tree()? == replay.target;

        // Check the whole change set.
        let saved = self.no_bodies;
        self.no_bodies = saved || !full;
        let set = self.current_changeset()?;
        let result = self.check_set(&set, true);
        self.no_bodies = saved;
        let result = result?;
        if !full {
            self.queue_session(&set)?;
        }
        text.push('\n');
        text.push_str(&result.text);
        put_line(&mut text, &format!("replay: {remaining}"));
        let checked = if full { result.report.accepted } else { crate::phase1_ok(&result.report) };
        let ok = problems == 0 && at_target && checked;
        text.push_str(&if ok {
            if full { "the plan is accepted; run `semilla replay end`\n".to_string() } else { "phase 1 passed; `semilla status` shows phase 2. Then run `semilla replay end`\n".to_string() }
        } else {
            "fix the plan and run `semilla replay apply` again (it starts again from the base)\n".to_string()
        });
        Ok(Applied { text, ok })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(old_start: usize, old_count: usize, new_start: usize, new_count: usize) -> Hunk {
        Hunk { file: "f".into(), old_start, old_count, new_start, new_count, lines: Vec::new() }
    }

    #[test]
    fn lines_map_through_hunks() {
        // Two lines inserted after line 3, line 6 replaced by three lines.
        let hunks = [hunk(3, 0, 4, 2), hunk(6, 1, 8, 3)];
        assert_eq!(map_line(&hunks, 2), Some(2));
        assert_eq!(map_line(&hunks, 3), Some(3));
        assert_eq!(map_line(&hunks, 4), Some(6));
        assert_eq!(map_line(&hunks, 6), None);
        assert_eq!(map_line(&hunks, 7), Some(11));
    }

    #[test]
    fn words_replace_whole() {
        assert_eq!(replace_word("scale(v, 2); rescale(v); scale_x; x.scale", "scale", "multiply"), ("multiply(v, 2); rescale(v); scale_x; x.multiply".to_string(), 2));
    }

    #[test]
    fn positions_parse() {
        assert_eq!(parse_position("src/a.rs:12:5").unwrap(), ("src/a.rs".into(), 12, Some(5)));
        assert_eq!(parse_position("src/a.rs:12").unwrap(), ("src/a.rs".into(), 12, None));
        assert!(parse_position("src/a.rs").is_err());
    }
}

#[cfg(test)]
mod field_tests {
    use super::replace_field;

    #[test]
    fn a_field_rename_changes_only_the_field_positions() {
        let text = "pub struct A {\n    pub balance: u64,\n}\n/// The new balance.\nfn f(balance: u64, a: &mut A) -> u64 {\n    let b = A { balance: 0 };\n    a.balance += balance;\n    let A { balance: x } = b;\n    a.balance() + x + \"balance\".len() as u64\n}\n";
        let (out, count) = replace_field(text, "balance", "cents");
        assert_eq!(count, 4, "{out}");
        assert!(out.contains("pub cents: u64") && out.contains("A { cents: 0 }") && out.contains("a.cents += balance;") && out.contains("A { cents: x }"), "{out}");
        assert!(out.contains("fn f(balance: u64") && out.contains("The new balance.") && out.contains("a.balance()") && out.contains("\"balance\""), "{out}");
    }
}
