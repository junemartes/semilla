//! The derived identity (design 24.2, rule 4).
//!
//! In a step with no identity claim, Semilla finds the candidate pairs: a removed and an added
//! entity of the same kind. A rename keeps the owner and changes the name. A move keeps the name
//! and changes the owner. The two texts must be similar ([`crate::PAIR_THRESHOLD`]): the score
//! does not count a doc comment, an attribute, and a visibility.
//!
//! [R90.8-1] (design 90, rule M8) The pairs come from the hints of the agent first (`#[rename]`,
//! `#[moved]`, and their short forms), then from the one map of the matcher of G2 over the other
//! candidates (a minimum-cost assignment by the score). Semilla makes the pairs into `rename` and
//! `move-item` claims and checks the step one time with them (R90.8-2). It does not try a second
//! map. A pair that passes gives the facts "renamed `A` → `B`" or "moved `A` → `B`" with the
//! compare result (design 35), and the forced edits fold under the fact.
//!
//! The step can have other changes (design 36.1). Each difference of a pair that the new name
//! does not force is an edit under the name map, and it has its own fact at the new path. A
//! changed body goes to the fact of its edit. A changed body with no such fact goes to the
//! identity fact, which then has `code different` and the mismatch mark.
//!
//! Limits: at most 4 removed and 4 added entities of one kind in one owner. Above the limit,
//! Semilla derives nothing and gives a note. A pair that does not pass shows as removed and
//! added, with a note that gives the cause and a template (R90.8-3).

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::matcher::{Key, Ladder, Thing};
use semilla_core::{Anchor, Category, Claim, ClaimStatus, Compare, Fact};
use semilla_syntax::rs::UseTree;
use semilla_syntax::{EditKind, ItemKind, Workspace};

use crate::{BodyOracle, ExprOracle, StepInput, StepOutcome};

/// The most entities of one kind in one owner for the search.
pub const MAX_GROUP: usize = 4;

/// One candidate pair.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Candidate {
    pub old: String,
    pub new: String,
    /// `rename` or `move-item`.
    pub op: &'static str,
}

/// The owner of an entity: the struct of a field, else the parent path.
pub(crate) fn owner(path: &str) -> &str {
    if let Some((owner, field)) = path.rsplit_once('.').filter(|(_, field)| !field.contains("::")) {
        let _ = field;
        return owner;
    }
    path.rsplit_once("::").map(|(parent, _)| parent).unwrap_or("")
}

pub(crate) fn name(path: &str) -> &str {
    let last = path.rsplit("::").next().unwrap_or(path);
    last.rsplit_once('.').map(|(_, field)| field).unwrap_or(last)
}

pub(crate) fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Fn => "function",
        ItemKind::Struct => "struct",
        ItemKind::Enum => "enum",
        ItemKind::Trait => "trait",
        ItemKind::Const => "const",
        ItemKind::Static => "static",
        ItemKind::Type => "type",
        ItemKind::Mod => "module",
        ItemKind::Field => "field",
        ItemKind::Variant => "variant",
        _ => "item",
    }
}

/// The entities of one side that the other side does not have. A child of such an entity (the
/// field of a renamed struct) follows its owner, so it is not a candidate of its own.
///
/// `open` (batch 5x, part B): the new modules that are not a renamed or moved module. An item of
/// such a module can be a moved item, so it is a candidate.
fn only_in<'a>(side: &'a Workspace, other: &Workspace, open: &BTreeSet<&str>) -> Vec<(&'a str, ItemKind)> {
    let paths: BTreeSet<&str> = side.items.keys().filter(|path| !other.items.contains_key(*path)).map(String::as_str).collect();
    side.items
        .values()
        .filter(|item| paths.contains(item.path.as_str()))
        .filter(|item| matches!(item.kind, ItemKind::Fn | ItemKind::Struct | ItemKind::Enum | ItemKind::Trait | ItemKind::Const | ItemKind::Static | ItemKind::Type | ItemKind::Mod | ItemKind::Field | ItemKind::Variant))
        // A tuple field has no name to change.
        .filter(|item| !(item.kind == ItemKind::Field && name(&item.path).chars().all(|c| c.is_ascii_digit())))
        .filter(|item| {
            let mut current = owner(&item.path);
            while !current.is_empty() {
                if paths.contains(current) && !open.contains(current) {
                    return false;
                }
                current = owner(current);
            }
            true
        })
        .map(|item| (item.path.as_str(), item.kind))
        .collect()
}

/// The token text of an item with no plain doc comment (`# [doc = "…"]`). A doc comment is not
/// a part of the identity of an item: a short item with a new doc comment stays a candidate
/// (batch 5x, round 2).
fn without_docs(text: &str) -> String {
    const START: &str = "# [doc = \"";
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(START) {
        // The end of the string literal: the first quote with no escape before it.
        let literal = &rest[at + START.len()..];
        let mut escaped = false;
        let end = literal.char_indices().find(|(_, c)| {
            let quote = !escaped && *c == '"';
            escaped = !escaped && *c == '\\';
            quote
        });
        let Some(tail) = end.and_then(|(end, _)| literal[end + 1..].strip_prefix(']')) else { break };
        out.push_str(&rest[..at]);
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// The token text of an item with no attribute and no visibility. An attribute and a visibility
/// are not a part of the identity of an item: each such difference of a pair is its own fact
/// (design 36.1). So a short type that moves with a wider visibility stays a candidate.
fn without_free_parts(text: &str) -> Vec<String> {
    // The token text has a group as `(crate)`. Each bracket becomes a token of its own. A
    // string literal stays one token.
    let mut spaced = String::new();
    let (mut in_string, mut escaped) = (false, false);
    for c in text.chars() {
        if in_string {
            spaced.push(if c == ' ' { '\u{a0}' } else { c });
            in_string = escaped || c != '"';
            escaped = !escaped && c == '\\';
        } else if matches!(c, '(' | ')' | '[' | ']' | '{' | '}') {
            spaced.extend([' ', c, ' ']);
        } else {
            in_string = c == '"';
            spaced.push(c);
        }
    }
    let tokens: Vec<&str> = spaced.split(' ').filter(|t| !t.is_empty()).collect();
    let mut out = Vec::new();
    let mut at = 0;
    // The end of a group that starts at `open`: the index after its last token.
    let group_end = |open: usize, left: &str, right: &str| -> usize {
        let mut depth = 0usize;
        for (index, token) in tokens.iter().enumerate().skip(open) {
            if *token == left {
                depth += 1;
            } else if *token == right {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index + 1;
                }
            }
        }
        tokens.len()
    };
    while at < tokens.len() {
        match tokens[at] {
            "#" if tokens.get(at + 1) == Some(&"[") => at = group_end(at + 1, "[", "]"),
            "#" if tokens.get(at + 1) == Some(&"!") && tokens.get(at + 2) == Some(&"[") => at = group_end(at + 2, "[", "]"),
            "pub" if tokens.get(at + 1) == Some(&"(") && matches!(tokens.get(at + 2), Some(&"crate" | &"super" | &"self" | &"in")) => at = group_end(at + 1, "(", ")"),
            "pub" => at += 1,
            token => {
                out.push(token.to_string());
                at += 1;
            }
        }
    }
    out
}

/// The similarity of two item texts, with the own name of each item as one token `$name`.
fn similarity(a: &semilla_syntax::Item, b: &semilla_syntax::Item) -> f64 {
    let tokens = |item: &semilla_syntax::Item| -> Vec<String> {
        let own = name(&item.path);
        without_free_parts(&without_docs(&item.text)).into_iter().map(|t| if t == own { "$name".to_string() } else { t }).collect()
    };
    let (x, y) = (tokens(a), tokens(b));
    if x.is_empty() || y.is_empty() || x.len() * y.len() > 4_000_000 {
        return if x == y { 1.0 } else { 0.0 };
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

/// The candidate pairs of a step, and the notes for the limits.
pub fn candidates(before: &Workspace, after: &Workspace) -> (Vec<Candidate>, Vec<String>) {
    candidates_without(before, after, &[])
}

/// The candidate pairs with no item of a stated pair (design 54.11): a stated pair makes the
/// search smaller. The limits of the search apply to the items that are left.
fn candidates_without(before: &Workspace, after: &Workspace, stated: &[Candidate]) -> (Vec<Candidate>, Vec<String>) {
    let none = BTreeSet::new();
    let mut removed = only_in(before, after, &none);
    removed.retain(|(path, _)| !stated.iter().any(|pair| pair.old == *path));
    let free = |list: &mut Vec<(&str, ItemKind)>| list.retain(|(path, _)| !stated.iter().any(|pair| pair.new == *path));
    let mut added = only_in(after, before, &none);
    free(&mut added);
    let first = pairs_of(before, after, &removed, &added);
    // A new module that is not the new side of a pair: its items can be moved items (batch 5x,
    // part B). The check of the step decides if the module is a part of the move.
    let open: BTreeSet<&str> = added.iter().filter(|(path, kind)| *kind == ItemKind::Mod && !first.0.iter().any(|pair| pair.new == *path)).map(|(path, _)| *path).collect();
    if open.is_empty() {
        return first;
    }
    let mut added = only_in(after, before, &open);
    free(&mut added);
    pairs_of(before, after, &removed, &added)
}

/// The pairs of the removed and the added entities, and the notes for the limits.
fn pairs_of(before: &Workspace, after: &Workspace, removed: &[(&str, ItemKind)], added: &[(&str, ItemKind)]) -> (Vec<Candidate>, Vec<String>) {
    let mut groups: BTreeMap<(&str, &str), (usize, usize)> = BTreeMap::new();
    for (path, kind) in removed {
        groups.entry((kind_name(*kind), owner(path))).or_default().0 += 1;
    }
    for (path, kind) in added {
        groups.entry((kind_name(*kind), owner(path))).or_default().1 += 1;
    }
    let mut notes = BTreeSet::new();
    let mut too_many = |kind: ItemKind, path: &str| {
        let (r, a) = groups.get(&(kind_name(kind), owner(path))).copied().unwrap_or_default();
        let over = r > MAX_GROUP || a > MAX_GROUP;
        if over {
            let place = if owner(path).is_empty() { "the crate root".to_string() } else { format!("`{}`", owner(path)) };
            notes.insert(format!("{r} removed and {a} added {}(s) in {place}: Semilla derives no rename or move above {MAX_GROUP}, so the items show as removed and added", kind_name(kind)));
        }
        over
    };
    let mut found = Vec::new();
    for (old, old_kind) in removed {
        for (new, new_kind) in added {
            if old_kind != new_kind {
                continue;
            }
            // A method of a trait impl follows the move of its type (batch 5x, part B:
            // `checks::moved_impls`). It is not a candidate of its own.
            if old.contains('<') || new.contains('<') {
                continue;
            }
            let op = if owner(old) == owner(new) && name(old) != name(new) {
                "rename"
            } else if owner(old) != owner(new) && name(old) == name(new) && !matches!(old_kind, ItemKind::Field | ItemKind::Variant) {
                "move-item"
            } else {
                continue;
            };
            if too_many(*old_kind, old) | too_many(*new_kind, new) {
                continue;
            }
            // A rename or a move keeps the text, except for the name. A removed item and an added
            // item with different texts are not a candidate (no proof, no note).
            if similarity(&before.items[*old], &after.items[*new]) < crate::PAIR_THRESHOLD {
                continue;
            }
            found.push(Candidate { old: old.to_string(), new: new.to_string(), op });
        }
    }
    (found, notes.into_iter().collect())
}

/// [R90.8-1] The names of the paths that the step changes (`a::two` → `a::dos`: (`two`, `dos`)):
/// a use that follows a pair (design 54.17) is an anchor of the one map.
fn followed_names(edits: &[semilla_syntax::Edit]) -> BTreeSet<(String, String)> {
    let last = |text: &str| text.rsplit("::").next().unwrap_or("").trim().to_string();
    edits
        .iter()
        .filter(|edit| edit.node_before == Some("path") && edit.node_after == Some("path"))
        .filter_map(|edit| Some((last(edit.text_before.as_deref()?), last(edit.text_after.as_deref()?))))
        .collect()
}

/// [R90.8-1] The one map of the matcher of G2 (design 72) over the candidate pairs: a
/// minimum-cost assignment by the similar score, ties by the source order. A pair whose uses
/// follow it (a changed path from the old name to the new name) gets a bonus of 1. Each item is
/// in one pair at most. Semilla does not try a second map (design 90).
fn one_map(before: &Workspace, after: &Workspace, pairs: &[Candidate], follows: &BTreeSet<(String, String)>) -> Vec<Candidate> {
    let olds: Vec<&str> = pairs.iter().map(|pair| pair.old.as_str()).collect::<BTreeSet<_>>().into_iter().collect();
    let news: Vec<&str> = pairs.iter().map(|pair| pair.new.as_str()).collect::<BTreeSet<_>>().into_iter().collect();
    let thing = |_: &&str| semilla_core::matcher::Thing { group: String::new(), identity: None, content: None };
    let (old_things, new_things): (Vec<_>, Vec<_>) = (olds.iter().map(thing).collect(), news.iter().map(thing).collect());
    let candidate = |i: usize, j: usize| pairs.iter().find(|pair| pair.old == olds[i] && pair.new == news[j]);
    let score = |i: usize, j: usize| candidate(i, j).map_or(0.0, |pair| similarity(&before.items[&pair.old], &after.items[&pair.new]) + if follows.contains(&(name(&pair.old).to_string(), name(&pair.new).to_string())) { 1.0 } else { 0.0 });
    let ladder = semilla_core::matcher::Ladder { similarity: &score, threshold: crate::PAIR_THRESHOLD, in_order: None };
    let mut map: Vec<Candidate> = semilla_core::matcher::pair(&old_things, &new_things, &ladder).into_iter().filter_map(|pair| candidate(pair.old, pair.new).cloned()).collect();
    map.sort_by_key(|pair| (depth(&pair.old), pair.old.clone()));
    map
}

fn pair_text(map: &[Candidate]) -> String {
    map.iter().map(|c| format!("`{}` → `{}`", c.old, c.new)).collect::<Vec<_>>().join(", ")
}

/// The claims of one map. The IDs cannot be the ID of a claim of the agent.
fn claims_of(map: &[Candidate]) -> Vec<Claim> {
    map.iter()
        .enumerate()
        .map(|(index, pair)| {
            // A rename under a renamed or moved owner (batch 9b2, F2) gives the full new path.
            let to = if pair.op == "rename" && owner(&pair.old) == owner(&pair.new) { name(&pair.new).to_string() } else { pair.new.clone() };
            let mut args = serde_json::Map::new();
            args.insert("to".into(), serde_json::Value::String(to));
            Claim { id: format!("derived-identity-{}", index + 1), op: pair.op.to_string(), at: Some(Anchor::item(&pair.old)), args }
        })
        .collect()
}

/// The compare result (true: `code different`) and the affected bodies of a "pair diff" fact
/// (design 44, FIX 3): the result of the fact of the edit under the name map, as for each other
/// edit. With no such fact, or a result that is not known, the result of the whole new item
/// (`item_changed`, the path `new`).
pub(crate) fn pair_diff_result(report: &semilla_core::StepReport, fact_of: &[Option<usize>], edit: usize, item_changed: bool, new: &str) -> (bool, Vec<String>) {
    let own = fact_of.get(edit).copied().flatten().and_then(|fact| report.facts.get(fact)).filter(|fact| matches!(fact.compare, Compare::CodeEqual | Compare::CodeDifferent));
    match own {
        Some(fact) if fact.compare == Compare::CodeDifferent => (true, fact.affected.clone()),
        Some(_) => (false, Vec::new()),
        None => (item_changed, if item_changed { vec![new.to_string()] } else { Vec::new() }),
    }
}

/// [R90.8-1] The pairs of the renamed and the moved items of a step (design 90, rule M8): the
/// hints of the agent first (`#[rename]`, `#[moved]`, and their short forms), then the one map
/// of the matcher over the items that no hint names. The compiler identity already pairs each
/// item whose definition keeps its path (design 70). Semilla checks each pair one time: one run
/// of the core check with the claims of all pairs, and one more run without the pairs that do
/// not pass (to give their entries "delete" and "add"). It does not try a second map.
///
/// Batch 9b2, F2: a stated pair has no limit of the search (the count of one kind in one owner,
/// the similar score). A stated pair that does not pass gives its cause.
pub(crate) fn derive<'a>(input: &StepInput<'_, 'a>, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle, plain: StepOutcome, bases: &crate::joint::OpaqueBases<'a>) -> StepOutcome {
    let (mut stated, mut causes) = crate::agent::stated_pairs(input);
    // The order of the options is not important: the pairs have the order of their paths.
    stated.sort_by(|a, b| (&a.1.old, &a.1.new).cmp(&(&b.1.old, &b.1.new)));
    // Phase 1 of a step has no MIR: phase 2 checks each pair.
    if plain.bodies.is_empty() && !input.step.syntax_only {
        stated.clear();
    }
    let mut outcome = pairs_once(input, bodies, exprs, plain, stated, &mut causes, bases);
    outcome.stated_causes = causes;
    outcome
}

/// The path of an item after the pairs of a map.
fn mapped(map: &[Candidate], path: &str) -> String {
    let under = |pair: &&Candidate| path == pair.old || path.strip_prefix(pair.old.as_str()).is_some_and(|rest| rest.starts_with("::") || rest.starts_with('.'));
    match map.iter().filter(under).max_by_key(|pair| pair.old.len()) {
        Some(pair) => format!("{}{}", pair.new, &path[pair.old.len()..]),
        None => path.to_string(),
    }
}

/// The count of the owners of a path. An owner comes before its children in a map.
fn depth(path: &str) -> usize {
    let mut count = 0;
    let mut current = owner(path);
    while !current.is_empty() {
        count += 1;
        current = owner(current);
    }
    count
}

/// [R90.8-2] The result of each pair of a map in one run of the core check: the index of each
/// pair that does not pass, with the cause. A stated pair needs its evidence too (design 54.17).
fn failed_pairs(input: &StepInput, map: &[Candidate], claims: &[Claim], outcome: &StepOutcome, stated: &[Candidate]) -> Vec<(usize, String)> {
    let report = &outcome.report;
    // A failure of the step (an error, an edit with no fact) is a failure of each pair.
    let whole = report.errors.first().cloned().or_else(|| (!report.uncovered.is_empty()).then(|| "an edit has no fact".to_string()));
    let mut failed = Vec::new();
    for (index, (pair, claim)) in map.iter().zip(claims).enumerate() {
        let cause = match report.claims.iter().find(|r| r.claim == claim.id).map(|r| &r.status) {
            Some(status) if status.is_verified() => whole.clone(),
            Some(ClaimStatus::Refuted { reason } | ClaimStatus::Malformed { reason } | ClaimStatus::Pending { reason }) => Some(reason.clone()),
            Some(other) => Some(other.text()),
            None => Some("no result".into()),
        };
        let cause = cause.or_else(|| {
            (stated.contains(pair) && !evidence(input, map, outcome, pair)).then(|| {
                let word = if pair.op == "rename" { "rename" } else { "move" };
                format!("{NO_EVIDENCE_START} {word}, and the declarations are not similar. A stated {word} needs one of the two")
            })
        });
        if let Some(cause) = cause {
            failed.push((index, cause));
        }
    }
    failed
}

/// [R90.8-3] The template of a pair that does not pass: `#[rename(before::OLD, after::NEW)]` or
/// `#[moved(…)]`, if the check of the intent files accepts it (the gate of R87.1-1).
fn pair_template(input: &StepInput, bodies: &dyn BodyOracle, pair: &Candidate) -> String {
    let word = if pair.op == "rename" { "rename" } else { "moved" };
    let function = format!("{}_to_{}", name(&pair.old), name(&pair.new)).to_lowercase();
    let template = format!("#[{word}(before::{}, after::{})] fn {function}() {{}}", pair.old, pair.new);
    let ask = crate::template::Ask { forms: vec![template.clone()], expect: Vec::new() };
    match bodies.check_templates(input.step, std::slice::from_ref(&ask)).and_then(|verdicts| verdicts.into_iter().next()) {
        Some(Ok(_)) => format!("template: `{template}`"),
        Some(Err(cause)) => format!("no template: {}", cause.lines().next().unwrap_or("")),
        None => "no template: the step has no compiled trees for the check of the intent files".into(),
    }
}

/// [R90.8-1] [R90.8-2] The one map of a step and its check (see [`derive`]).
fn pairs_once<'a>(input: &StepInput<'_, 'a>, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle, mut plain: StepOutcome, stated: Vec<(usize, Candidate)>, causes: &mut BTreeMap<usize, String>, bases: &crate::joint::OpaqueBases<'a>) -> StepOutcome {
    let hints: Vec<Candidate> = stated.iter().map(|(_, pair)| pair.clone()).collect();
    let (pairs, notes) = candidates_without(input.before, input.after, &hints);
    plain.report.notes.extend(notes);
    let follows = followed_names(&plain.edits);
    let own = if pairs.is_empty() { Vec::new() } else { one_map(input.before, input.after, &pairs, &follows) };
    if !pairs.is_empty() {
        plain.report.identity_candidates = true;
        // Phase 1 of a step has no MIR: the check of the pairs waits for phase 2. A syntax-only
        // check (the virtual crate of an expansion compare, R83.1-3) checks on the syntax tree.
        if plain.bodies.is_empty() && !input.step.syntax_only {
            plain.report.notes.push(format!("a rename or a move is possible ({}): phase 2 tries the derivation", pair_text(&own)));
            return plain;
        }
    }
    // The hints first: the checks of the form of a hint need no run.
    let mut map: Vec<Candidate> = own;
    let mut options: BTreeMap<Candidate, usize> = BTreeMap::new();
    let mut order: Vec<&(usize, Candidate)> = stated.iter().collect();
    order.sort_by_key(|(_, pair)| depth(&pair.old));
    for (option, pair) in order {
        let all: Vec<Candidate> = map.iter().chain(options.keys()).cloned().collect();
        let cause = if let Some(other) = all.iter().find(|other| other.old == pair.old || other.new == pair.new) {
            Some(format!("Semilla derives `{}` → `{}`, and an item is in one pair only", other.old, other.new))
        } else if pair.op == "rename" && mapped(&all, owner(&pair.old)) != owner(&pair.new) {
            Some(format!("a rename keeps the owner, and no rename or move gives the owner `{}` the new path `{}`", owner(&pair.old), owner(&pair.new)))
        } else if input.after.items.contains_key(&mapped(&all, &pair.old)) {
            Some(format!("the new tree has the item `{}` too: the old item stays", mapped(&all, &pair.old)))
        } else {
            input.before.items.keys().find(|path| **path != pair.old && mapped(&all, path) == pair.new).map(|other| format!("`{}` is the item `{other}` of the old tree: the item stays", pair.new))
        };
        match cause {
            Some(cause) => {
                causes.insert(*option, cause);
            }
            None => {
                options.insert(pair.clone(), *option);
            }
        }
    }
    map.extend(options.keys().cloned());
    map.sort_by_key(|pair| (depth(&pair.old), pair.old.clone()));
    if map.is_empty() {
        return plain;
    }
    let hinted: Vec<Candidate> = options.keys().cloned().collect();
    // [R90.8-2] Each pair is checked one time: one run with each pair.
    let (claims, first) = run_map(input, bodies, exprs, &map, bases);
    let failed = failed_pairs(input, &map, &claims, &first, &hinted);
    let (map, claims, outcome) = if failed.is_empty() {
        (map, claims, first)
    } else {
        // [R90.8-3] A pair that does not pass stays "delete" and "add": the cause, a template, and
        // each edit under the name map as its own fact (trial 3, A2).
        let failed_set: BTreeSet<usize> = failed.iter().map(|(index, _)| *index).collect();
        let known: BTreeSet<String> = plain.report.facts.iter().map(|f| f.description.clone()).collect();
        let mut extra = Vec::new();
        for (index, cause) in &failed {
            let pair = &map[*index];
            if let Some(option) = options.get(pair) {
                causes.insert(*option, if cause.starts_with(NO_EVIDENCE_START) { cause.clone() } else { format!("the check of the pair does not pass: {cause}") });
                continue;
            }
            let what = if pair.op == "move-item" { "move" } else { "rename" };
            plain.report.notes.push(format!("{} show(s) as removed and added: Semilla does not derive the {what} ({cause})", pair_text(std::slice::from_ref(pair))));
            plain.report.notes.push(format!("the hint of the pair {}: {}", pair_text(std::slice::from_ref(pair)), pair_template(input, bodies, pair)));
            extra.extend(pair_diff_facts(&first, pair, &known));
        }
        let start = plain.report.facts.len();
        for (index, mut fact) in extra.into_iter().enumerate() {
            fact.id = format!("f{}", start + index + 1);
            plain.report.facts.push(fact);
        }
        let kept: Vec<Candidate> = map.iter().enumerate().filter(|(index, _)| !failed_set.contains(index)).map(|(_, pair)| pair.clone()).collect();
        if kept.is_empty() {
            return plain;
        }
        // One more run with the pairs that pass: it checks no pair a second time.
        let (claims, outcome) = run_map(input, bodies, exprs, &kept, bases);
        let again = failed_pairs(input, &kept, &claims, &outcome, &hinted);
        if !again.is_empty() {
            for (index, cause) in again {
                if let Some(option) = options.get(&kept[index]) {
                    causes.insert(*option, if cause.starts_with(NO_EVIDENCE_START) { cause.clone() } else { format!("the check of the pair does not pass: {cause}") });
                } else {
                    plain.report.notes.push(format!("{} show(s) as removed and added: Semilla does not derive the {} ({cause})", pair_text(std::slice::from_ref(&kept[index])), if kept[index].op == "move-item" { "move" } else { "rename" }));
                }
            }
            return plain;
        }
        (kept, claims, outcome)
    };
    let (map, claims, mut outcome) = with_children(input, bodies, exprs, map, claims, outcome, bases);
    // The notes of the candidates and of the failed pairs stay.
    outcome.report.identity_candidates |= plain.report.identity_candidates;
    for note in plain.report.notes {
        if !outcome.report.notes.contains(&note) {
            outcome.report.notes.push(note);
        }
    }
    convert(input, &map, &claims, outcome)
}

/// The facts of a pair that does not pass: each edit inside the new item under the name map is
/// a real difference (trial 3, A2). The map hides the forced edits of the name.
fn pair_diff_facts(failed: &StepOutcome, pair: &Candidate, known: &BTreeSet<String>) -> Vec<Fact> {
    let inside = |item: &str| item == pair.new || item.starts_with(&format!("{}::", pair.new)) || item.starts_with(&format!("{}.", pair.new));
    let changed = failed.bodies.iter().any(|b| inside(&b.name) && !matches!(b.status, crate::BodyStatus::Equal { .. }));
    let mut extra = Vec::new();
    for (edit_index, edit) in failed.edits.iter().enumerate().filter(|(_, e)| e.anchor().is_some_and(|a| inside(&a.item)) && !matches!(e.kind, EditKind::ItemAdded | EditKind::ItemRemoved)) {
        let text = crate::edits::display_text(edit);
        if known.iter().any(|k| k.ends_with(&text)) {
            continue;
        }
        // A doc test is code (brief 5h, B1). Semilla does not compile it: the fact has no
        // compare, with the cause.
        let doc_test = edit.anchor().and_then(|anchor| crate::facts::doc_test_fact(&anchor.item, edit));
        let (changed, affected) = pair_diff_result(&failed.report, &failed.fact_of, edit_index, changed, &pair.new);
        extra.push(Fact {
            at: edit.anchor().cloned(),
            file: edit.file.clone(),
            part: "pair diff".into(),
            description: format!("`{}` differs from the removed `{}`: {text}", name(&pair.new), name(&pair.old)),
            status: "derived".into(),
            content: doc_test.as_ref().map(|(_, content)| content.clone()),
            category: if doc_test.is_some() { Category::Docs } else { edit_category(edit) },
            compare: if doc_test.is_some() { Compare::NoCompare } else if changed { Compare::CodeDifferent } else { Compare::CodeEqual },
            note: doc_test.as_ref().map(|_| format!("{} Semilla does not compile a doc test", crate::uncompiled::NO_COMPARE)),
            affected,
            ..Default::default()
        });
    }
    extra
}

/// The start of the cause of a stated pair with no evidence (design 54.17).
pub(crate) const NO_EVIDENCE_START: &str = "no reference follows the";

/// True if the step has evidence for a stated pair (design 54.17). A stated pair does not
/// have the limits of the search, so each removed item and each added item of one kind can
/// pass the check of the map. The evidence is one of these:
/// - the two declarations pass the similar score of the search;
/// - a reference follows a rename: a site of the site table that no edit has, or a site in
///   an edit whose new text has the new name;
/// - a reference follows a move: a forced edit that the check of the pair explains (a `use`
///   line, a path).
fn evidence(input: &StepInput, map: &[Candidate], outcome: &StepOutcome, pair: &Candidate) -> bool {
    if similarity(&input.before.items[&pair.old], &input.after.items[&pair.new]) >= crate::PAIR_THRESHOLD {
        return true;
    }
    // The sites that do not follow: the places with the name in the old text of each edit of
    // the item whose new text does not have the new name. A site follows if the item has more
    // sites than these.
    let (old_name, new_name) = (name(&pair.old), name(&pair.new));
    let lost = |item: &str| -> usize {
        let edits = outcome.edits.iter().filter(|edit| edit.before.as_ref().is_some_and(|at| at.item == item));
        let open = edits.filter(|edit| !edit.text_after.as_deref().is_some_and(|text| crate::sites::name_places(text, &[new_name]) > 0));
        open.filter_map(|edit| edit.text_before.as_deref()).map(|text| crate::sites::name_places(text, &[old_name, new_name])).sum()
    };
    if outcome.renames.followed.get(&pair.old).is_some_and(|items| items.iter().any(|(item, count)| *count > lost(item))) {
        return true;
    }
    let Some(claim) = map.iter().position(|other| other == pair).map(|at| input.step.claims.len() + at) else { return false };
    outcome.edits.iter().zip(&outcome.owners).any(|(edit, owner)| {
        let is_use = edit.attr_before.as_deref() == Some("use") || edit.attr_after.as_deref() == Some("use");
        let is_path = edit.kind == EditKind::Replaced && edit.node_before == Some("path") && edit.node_after == Some("path");
        *owner == Some(claim) && (is_use || is_path)
    })
}

/// True if an item of the kind can be one side of a pair.
pub(crate) fn pair_kind(kind: ItemKind) -> bool {
    matches!(kind, ItemKind::Fn | ItemKind::Struct | ItemKind::Enum | ItemKind::Trait | ItemKind::Const | ItemKind::Static | ItemKind::Type | ItemKind::Mod | ItemKind::Field | ItemKind::Variant)
}

/// The candidate pairs under the pairs of a map (batch 9b2, F2). The general rule: the owner of
/// an entity is its owner after the map. So a child of a renamed or moved entity is a candidate
/// of a rename when its old owner maps to its new owner (the variant `Mode::Idle` and the
/// variant `State::Rest`, with the pair `Mode` → `State`). A child with the same name on the
/// two sides follows its owner: it is not a candidate. The limits of the first search apply.
fn children(before: &Workspace, after: &Workspace, map: &[Candidate]) -> Vec<Candidate> {
    let mut found = Vec::new();
    for pair in map {
        // The children of `own` that have no item with the same name under `other_own`.
        fn side<'a>(workspace: &'a Workspace, own: &str, other: &Workspace, other_own: &str) -> Vec<(&'a str, ItemKind)> {
            workspace
                .items
                .values()
                .filter(|item| owner(&item.path) == own && pair_kind(item.kind) && !item.path[own.len()..].contains('<'))
                .filter(|item| !(item.kind == ItemKind::Field && name(&item.path).chars().all(|c| c.is_ascii_digit())))
                .filter(|item| !other.items.contains_key(&format!("{other_own}{}", &item.path[own.len()..])))
                .map(|item| (item.path.as_str(), item.kind))
                .collect()
        }
        let used = |path: &str| map.iter().any(|other| other.old == path || other.new == path);
        let removed: Vec<(&str, ItemKind)> = side(before, &pair.old, after, &pair.new).into_iter().filter(|(path, _)| !used(path)).collect();
        let added: Vec<(&str, ItemKind)> = side(after, &pair.new, before, &pair.old).into_iter().filter(|(path, _)| !used(path)).collect();
        let count = |list: &[(&str, ItemKind)], kind: ItemKind| list.iter().filter(|(_, other)| *other == kind).count();
        for (old, old_kind) in &removed {
            for (new, new_kind) in &added {
                if old_kind != new_kind || name(old) == name(new) || count(&removed, *old_kind) > MAX_GROUP || count(&added, *new_kind) > MAX_GROUP {
                    continue;
                }
                if similarity(&before.items[*old], &after.items[*new]) < crate::PAIR_THRESHOLD {
                    continue;
                }
                found.push(Candidate { old: old.to_string(), new: new.to_string(), op: "rename" });
            }
        }
    }
    found
}

/// Adds the pairs of [`children`] to a map that passes the check (batch 9b2, F2): the one map of
/// the matcher over the children (R90.8-1), checked in one run. If it does not pass, the map
/// stays as it is: the children show as removed and added.
fn with_children<'a>(input: &StepInput<'_, 'a>, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle, mut map: Vec<Candidate>, mut claims: Vec<Claim>, mut outcome: StepOutcome, bases: &crate::joint::OpaqueBases<'a>) -> (Vec<Candidate>, Vec<Claim>, StepOutcome) {
    loop {
        let pairs = children(input.before, input.after, &map);
        if pairs.is_empty() {
            break;
        }
        let child = one_map(input.before, input.after, &pairs, &followed_names(&outcome.edits));
        if child.is_empty() {
            break;
        }
        let next: Vec<Candidate> = map.iter().chain(&child).cloned().collect();
        let (next_claims, result) = run_map(input, bodies, exprs, &next, bases);
        if !failed_pairs(input, &next, &next_claims, &result, &[]).is_empty() {
            break;
        }
        (map, claims, outcome) = (next, next_claims, result);
    }
    (map, claims, outcome)
}

/// Runs the core check of a step one time with the claims of the pairs of a map.
fn run_map<'a>(input: &StepInput<'_, 'a>, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle, map: &[Candidate], bases: &crate::joint::OpaqueBases<'a>) -> (Vec<Claim>, StepOutcome) {
    let mut step = input.step.clone();
    let claims = claims_of(map);
    step.claims.extend(claims.iter().cloned());
    let outcome = crate::verify_core(&StepInput { step: &step, before: input.before, after: input.after }, bodies, exprs, true, true, bases);
    (claims, outcome)
}

/// The category of the fact for one difference of a pair, from the kind of the edit.
fn edit_category(edit: &semilla_syntax::Edit) -> Category {
    match edit.kind {
        EditKind::Visibility => Category::Visibility,
        EditKind::Docs | EditKind::Comment => Category::Docs,
        EditKind::Attrs => Category::Attribute,
        EditKind::Signature | EditKind::Members => Category::Declaration,
        EditKind::Inserted | EditKind::Deleted | EditKind::Moved | EditKind::Modified | EditKind::Replaced => Category::Semantics,
        _ => Category::Text,
    }
}

/// Makes the results of the derived claims into facts. The edits and the bodies of each claim go
/// to its fact.
fn convert(input: &StepInput, map: &[Candidate], claims: &[Claim], mut outcome: StepOutcome) -> StepOutcome {
    let first = input.step.claims.len();
    let report = &mut outcome.report;
    let mut identity_facts = Vec::new();
    // A rename or a move changes the text of each body that has the name. Each such body gives
    // its conditions to the note of the first fact of the rename or the move (design 35).
    let body_index = crate::checks::BodyIndex::new(&outcome.bodies, input.after, input.before);
    let mut note = crate::facts::stop_note(body_index.stopped_items(), &body_index, &Default::default());
    for (index, (pair, claim)) in map.iter().zip(claims).enumerate() {
        let result = report.claims.iter().find(|r| r.claim == claim.id).cloned();
        let Some(result) = result else { continue };
        // The `use` lines that the identity explains (batch 6a, B1). They are no facts to read.
        // The count is the larger of the removed lines and the added lines of each module.
        let mut lines: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for (edit, _) in outcome.edits.iter().zip(&outcome.owners).filter(|(edit, owner)| **owner == Some(first + index) && (edit.attr_before.as_deref() == Some("use") || edit.attr_after.as_deref() == Some("use"))) {
            if let Some(anchor) = edit.before.as_ref().or(edit.after.as_ref()) {
                let entry = lines.entry(owner(&anchor.item)).or_default();
                if edit.before.is_some() {
                    entry.0 += 1;
                } else {
                    entry.1 += 1;
                }
            }
        }
        let lines: usize = lines.values().map(|(removed, added)| (*removed).max(*added)).sum();
        // The `impl` blocks that move with the type (`checks::moved_impls`, design 36.1).
        let start = format!("{}::impl ", owner(&pair.old));
        let own = name(&pair.old);
        let impls = outcome
            .renames
            .items
            .keys()
            .filter(|_| pair.op == "move-item")
            .filter_map(|old| old.strip_prefix(&start))
            .filter(|tail| {
                // The name of a block: `Name`, or `<Name as Trait>` (the trait can have a path).
                let block = tail.rsplit_once('#').map_or(*tail, |(block, _)| block);
                block == own || (block.ends_with('>') && block.strip_prefix('<').and_then(|rest| rest.strip_prefix(own)).is_some_and(|rest| rest.starts_with(" as ")))
            })
            .count();
        let with_lines = match (impls, lines) {
            (0, 0) => String::new(),
            (0, _) => format!(", with {lines} `use` line(s)"),
            (_, 0) => format!(", with {impls} impl"),
            _ => format!(", with {impls} impl and {lines} `use` line(s)"),
        };
        let verb = if pair.op == "rename" { "renamed" } else { "moved" };
        let mut spotlight_why = outcome.identity_reason.clone().filter(|_| identity_facts.is_empty());
        // A new name with a character outside ASCII can look the same as a different name
        // (batch 5k, D7): the fact shows the escape form and needs a reason.
        let old_names: Vec<String> = crate::facts::non_ascii_names(&pair.old).into_iter().map(|(_, name)| name).collect();
        let mut odd: Vec<String> = Vec::new();
        for (_, name) in crate::facts::non_ascii_names(&pair.new) {
            if !old_names.contains(&name) && !odd.contains(&name) {
                odd.push(name);
            }
        }
        if !odd.is_empty() {
            let list: Vec<String> = odd.iter().map(|name| format!("`{name}` (`{}`)", semilla_core::text::ascii_form(name).unwrap_or_default())).collect();
            let cause = format!("{}: {}", crate::facts::NON_ASCII, list.join(", "));
            spotlight_why = Some(match spotlight_why.take() {
                Some(why) => format!("{why}; {cause}"),
                None => cause,
            });
        }
        let function = |ws: &Workspace, path: &str| ws.items.get(path).is_some_and(|item| item.kind == ItemKind::Fn && item.body.is_some());
        let uncompiled = body_index.available && function(input.before, &pair.old) && function(input.after, &pair.new) && !body_index.has_body(&pair.new) && !body_index.has_body(&pair.old);
        if uncompiled {
            let text = format!("{} no checked configuration compiles `{}`", crate::uncompiled::NO_COMPARE, pair.new);
            note = Some(match note.take() {
                Some(old) => format!("{old}; {text}"),
                None => text,
            });
        }
        identity_facts.push(Fact {
            at: Some(Anchor::item(&pair.old)),
            file: input.before.items.get(&pair.old).map(|item| item.file.clone()),
            part: "identity".into(),
            description: crate::facts::with_ascii_forms(&format!("{verb} `{}` → `{}`{with_lines}", pair.old, pair.new)),
            status: "derived".into(),
            category: if pair.op == "rename" { Category::Rename } else { Category::Move },
            // The bodies of the fact are the derived bodies that show the name (design 35). A
            // function that no checked configuration compiles has no compare (design 38.10).
            compare: if !result.affected.is_empty() { Compare::CodeDifferent } else if uncompiled { Compare::NoCompare } else { Compare::CodeEqual },
            affected: result.affected.clone(),
            spotlight: spotlight_why.is_some(),
            spotlight_why,
            reason: input.step.reasons.get(&pair.old).cloned(),
            // The count of the sites that the tree cannot resolve (design 51, rule R3).
            note: match (note.take(), crate::sites::count_line(&outcome.renames, &pair.old)) {
                (Some(note), Some(sites)) => Some(format!("{note}; {sites}")),
                (note, sites) => note.or(sites),
            },
            ..Default::default()
        });
    }
    let count = identity_facts.len();
    report.claims.retain(|r| !claims.iter().any(|c| c.id == r.claim));
    let mut facts = identity_facts;
    facts.append(&mut report.facts);
    for (index, fact) in facts.iter_mut().enumerate() {
        fact.id = format!("f{}", index + 1);
    }
    report.facts = facts;
    for (owner, fact) in outcome.owners.iter_mut().zip(outcome.fact_of.iter_mut()) {
        if let Some(index) = *fact {
            *fact = Some(index + count);
        }
        if let Some(claim) = owner.filter(|&c| c >= first) {
            *fact = Some(claim - first);
            *owner = None;
        }
    }
    outcome
}

/// The module of an item: the nearest owner that is a module or a crate root.
pub(crate) fn module_of<'a>(path: &'a str, workspace: &Workspace) -> Option<&'a str> {
    let mut current = owner(path);
    while !current.is_empty() {
        if workspace.items.get(current).is_some_and(|item| item.kind == ItemKind::Mod) || workspace.crates.iter().any(|krate| krate.name == current) {
            return Some(current);
        }
        current = owner(current);
    }
    None
}

/// The names that a `use` item gives to its module: the name and the path segments.
pub(crate) fn use_names(tree: &UseTree, prefix: &[String], out: &mut Vec<(String, Vec<String>)>) {
    let with = |ident: &str| -> Vec<String> {
        // `a::b::{self}` is `a::b`.
        if ident == "self" { prefix.to_vec() } else { prefix.iter().cloned().chain([ident.to_string()]).collect() }
    };
    match tree {
        UseTree::Path(ident, tree) => use_names(tree, &with(ident), out),
        UseTree::Name(ident) => {
            let path = with(ident);
            if let Some(last) = path.last().cloned() {
                out.push((last, path));
            }
        }
        UseTree::Rename(ident, rename) => out.push((rename.clone(), with(ident))),
        UseTree::Group(items) => items.iter().for_each(|tree| use_names(tree, prefix, out)),
        // A glob import gives no name that the text shows.
        UseTree::Glob => {}
    }
}

/// The item of the workspace that the name `name` points to in the module `module`: an item of
/// the module, or a `use` of the module with the full path (also through a re-export with the
/// full path). `None` when these rules do not give one item of the workspace.
pub(crate) fn target_in(module: &str, name: &str, workspace: &Workspace, depth: usize) -> Option<String> {
    let direct = format!("{module}::{name}");
    if workspace.items.get(&direct).is_some_and(|item| !matches!(item.kind, ItemKind::Use | ItemKind::Impl)) {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    let mut found: Vec<Vec<String>> = Vec::new();
    for import in workspace.items.values().filter(|item| item.kind == ItemKind::Use && owner(&item.path) == module) {
        let Some(parsed) = semilla_syntax::rs::parse_use(&import.source) else { return None };
        let mut names = Vec::new();
        use_names(&parsed.tree, &[], &mut names);
        found.extend(names.into_iter().filter(|(local, _)| local == name).map(|(_, path)| path));
    }
    // More than one import of the name (two name spaces, or a `cfg`): no clear target.
    let [path] = found.as_slice() else { return None };
    let (first, rest) = path.split_first()?;
    let root = module.split("::").next()?.to_string();
    let mut start = match first.as_str() {
        "crate" => root,
        "self" => module.to_string(),
        "super" => owner(module).to_string(),
        other if workspace.crates.iter().any(|krate| krate.name == other) => other.to_string(),
        other if workspace.items.get(&format!("{module}::{other}")).is_some_and(|item| item.kind == ItemKind::Mod) => format!("{module}::{other}"),
        // A crate outside the workspace.
        _ => return None,
    };
    let (last, middle) = rest.split_last()?;
    for segment in middle {
        start = if segment == "super" { owner(&start).to_string() } else { format!("{start}::{segment}") };
        if start.is_empty() {
            return None;
        }
    }
    target_in(&start, last, workspace, depth - 1)
}

/// The names in the text of an item that point to an item of the workspace (batch 5k, B5): the
/// first segment of each path, with its item. A name that the item binds (a parameter, a `let`,
/// a generic parameter), a macro name, and a name with no clear target give no entry.
pub fn name_targets(item: &semilla_syntax::Item, workspace: &Workspace) -> BTreeMap<String, String> {
    use semilla_syntax::rs::{AstNode, SyntaxKind, ast};
    use ast::HasName;
    let mut out = BTreeMap::new();
    let (Some(module), Some(parsed)) = (module_of(&item.path, workspace), semilla_syntax::rs::parse_item(&item.source)) else { return out };
    let (mut used, mut bound) = (BTreeSet::new(), BTreeSet::new());
    for node in parsed.syntax().descendants() {
        if let Some(path) = ast::Path::cast(node.clone()) {
            // The name of a macro is in another name space. Its tokens are not parsed code.
            if node.ancestors().any(|ancestor| ancestor.kind() == SyntaxKind::MACRO_CALL) {
                continue;
            }
            let first = path.first_segment();
            if let Some(name) = first.as_ref().filter(|segment| segment.coloncolon_token().is_none()).and_then(|segment| segment.name_ref()) {
                used.insert(name.text().to_string());
            }
        } else if let Some(pat) = ast::IdentPat::cast(node.clone()) {
            bound.extend(pat.name().map(|name| name.text().to_string()));
        } else if let Some(param) = ast::TypeParam::cast(node.clone()) {
            bound.extend(param.name().map(|name| name.text().to_string()));
        } else if let Some(param) = ast::ConstParam::cast(node) {
            bound.extend(param.name().map(|name| name.text().to_string()));
        }
    }
    let names = (used, bound);
    let (used, bound) = (&names.0, &names.1);
    for name in used.difference(bound) {
        if matches!(name.as_str(), "self" | "Self" | "crate" | "super") {
            continue;
        }
        if let Some(target) = target_in(module, name, workspace, 8) {
            out.insert(name.clone(), target);
        }
    }
    out
}

/// The plain path of a twin key (`a::f#1` → `a::f`), or `None` for a path with no twin number.
pub(crate) fn twin_base(path: &str) -> Option<&str> {
    let (base, number) = path.rsplit_once('#')?;
    (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) && !base.is_empty()).then_some(base)
}

/// The new keys of the twins of a step (design 38.3). An item `P` of one tree that gets twins
/// (`P#0`, `P#1`) in the other tree keeps its key for the twin that pairs with it: the key `P#N`
/// of that twin becomes `P`. The one matcher chooses the twin (R72.1-1, part a): the `cfg`
/// condition, then equal tokens (the attributes, the docs, and the visibility do not count), then
/// the best similarity over the threshold. A key changes only for an item with no inner items.
/// `None` if no key changes: then the trees stay as they are.
pub(crate) fn twin_keys(before: &Workspace, after: &Workspace) -> Option<(Workspace, Workspace, Vec<TwinPair>)> {
    let text = |item: &semilla_syntax::Item| without_free_parts(&without_docs(&item.text));
    // The keys of `twins` that take the plain path: (plain path, twin key).
    let keys = |single: &Workspace, twins: &Workspace| -> Vec<(String, String, Key)> {
        let mut out = Vec::new();
        let mut bases: BTreeMap<&str, Vec<&semilla_syntax::Item>> = BTreeMap::new();
        for item in twins.items.values() {
            if let Some(base) = twin_base(&item.path) {
                bases.entry(base).or_default().push(item);
            }
        }
        for (base, mut found) in bases {
            let Some(old) = single.items.get(base) else { continue };
            if twins.items.contains_key(base) || found.len() < 2 || single.items.keys().any(|key| twin_base(key) == Some(base)) {
                continue;
            }
            let inner = |ws: &Workspace, path: &str| ws.items.keys().any(|key| key.len() > path.len() && key.starts_with(path) && matches!(key.as_bytes()[path.len()], b':' | b'.'));
            if inner(single, base) {
                continue;
            }
            found.retain(|item| item.kind == old.kind && !inner(twins, &item.path));
            found.sort_by_key(|item| twin_number(&item.path));
            let one = [Thing { group: String::new(), identity: twin_identities(single, &[base.to_string()]).pop(), content: Some(text(old).join(" ")) }];
            let paths: Vec<String> = found.iter().map(|item| item.path.clone()).collect();
            let many: Vec<Thing> = found.iter().zip(twin_identities(twins, &paths)).map(|(item, identity)| Thing { group: String::new(), identity: Some(identity), content: Some(text(item).join(" ")) }).collect();
            let old_tokens = text(old);
            let similarity = |_: usize, b: usize| semilla_core::matcher::token_similarity(&old_tokens, &text(found[b]));
            let ladder = Ladder { similarity: &similarity, threshold: TWIN_THRESHOLD, in_order: None };
            if let [pair] = semilla_core::matcher::pair(&one, &many, &ladder)[..] {
                out.push((base.to_string(), found[pair.new].path.clone(), pair.key));
            }
        }
        out
    };
    let (from_before, from_after) = (keys(before, after), keys(after, before));
    if from_before.is_empty() && from_after.is_empty() {
        return None;
    }
    let apply = |ws: &Workspace, keys: &[(String, String, Key)]| -> Workspace {
        let mut ws = ws.clone();
        for (base, key, _) in keys {
            if let Some(mut item) = ws.items.remove(key) {
                item.path = base.clone();
                ws.items.insert(base.clone(), item);
            }
        }
        ws.body_index = Default::default();
        ws
    };
    // R72.1-3: the note of each pair names the twin and the key of the ladder.
    let mut listed: Vec<TwinPair> = from_before.iter().map(|(base, twin, key)| (base.clone(), twin.clone(), base.clone(), Some(*key))).collect();
    listed.extend(from_after.iter().map(|(base, twin, key)| (twin.clone(), base.clone(), base.clone(), Some(*key))));
    Some((apply(before, &from_after), apply(after, &from_before), listed))
}

/// The smallest similarity of a pair of twins by similarity (Decided by Claude, design 72.4).
pub(crate) const TWIN_THRESHOLD: f64 = 0.5;

/// The number of a twin key (`a::f#1` gives 1), or 0.
fn twin_number(path: &str) -> usize {
    twin_base(path).and_then(|base| path[base.len() + 1..].parse().ok()).unwrap_or(0)
}

/// The `cfg` attributes of an item itself (not of the items in it), in their compare text,
/// sorted. A `cfg_attr` does not decide if the item exists, so it is not in the key. A part of
/// the identity of a twin (R72.1-1, part a).
fn cfg_key(ws: &Workspace, path: &str) -> Vec<String> {
    let mut key: Vec<String> = ws.items.get(path).map(|item| item.attrs.iter().filter(|text| crate::facts::attribute_name(text).as_deref() == Some("cfg")).map(|text| text.split_whitespace().collect::<String>()).collect()).unwrap_or_default();
    key.sort();
    key
}

/// The `impl` block of the syntax tree that holds a member: the smallest `impl` item of the file
/// whose lines hold the lines of the member.
fn impl_block_of<'a>(ws: &'a Workspace, item: &semilla_syntax::Item) -> Option<&'a semilla_syntax::Item> {
    ws.items
        .values()
        .filter(|block| block.kind == ItemKind::Impl && block.file == item.file && block.path != item.path)
        .filter(|block| (block.line_start, block.col_start) <= (item.line_start, item.col_start) && (item.line_end, item.col_end) <= (block.line_end, block.col_end))
        .min_by_key(|block| block.line_end - block.line_start)
}

/// R72.1-1 (part a): the identity of each twin of a list. The `cfg` condition of the twin; for an
/// `impl` block also its header: the trait reference or the self type of the compiler, else the
/// header text (the fall-back of R70.1-1, for each twin of the list if one twin has no record);
/// for a member of an `impl` block the identity of its block.
fn twin_identities(ws: &Workspace, paths: &[String]) -> Vec<String> {
    let blocks: Vec<Option<String>> = paths
        .iter()
        .map(|path| {
            let item = ws.items.get(path)?;
            if item.kind == ItemKind::Impl { Some(path.clone()) } else { impl_block_of(ws, item).map(|block| block.path.clone()) }
        })
        .collect();
    let records: Vec<Option<String>> = blocks.iter().map(|block| block.as_ref().and_then(|block| crate::records::impl_trait_ref(ws, block))).collect();
    let all_records = blocks.iter().zip(&records).all(|(block, record)| block.is_none() || record.is_some());
    paths
        .iter()
        .enumerate()
        .map(|(at, path)| {
            let header = blocks[at].as_ref().map(|block| match (&records[at], all_records) {
                (Some(record), true) => record.clone(),
                _ => ws.items.get(block).map(|item| without_free_parts(&item.signature).join(" ")).unwrap_or_default(),
            });
            let own = ws.items.get(path).is_some_and(|item| item.kind == ItemKind::Impl);
            match header {
                Some(header) if own => format!("{} | {header}", cfg_key(ws, path).join(" ")),
                Some(header) => format!("{} | in {header}", cfg_key(ws, path).join(" ")),
                None => cfg_key(ws, path).join(" "),
            }
        })
        .collect()
}

/// One pair of twins of the matcher: the old path, the new path, the key of the pair in the
/// compare, and the key of the ladder that made the pair. A twin with no pair and a free number
/// for the compare has an empty path on the other side and no key.
pub(crate) type TwinPair = (String, String, String, Option<Key>);

/// The pairs of the twins of a step (design 59, part C, and R72.1-1, part a). Twins are the
/// items of one parent with one path and one kind (the suffix `#N` in source order). The twins of
/// one path pair by the one matcher: the identity (the `cfg` condition; the header of an `impl`
/// block; the block of a member), then equal tokens (the tokens of the item and of each item in
/// it), then the best similarity of the tokens, then the source order for a tie. Twins of one
/// identity always pair. A twin with no partner is an added or a removed item.
///
/// The compare pairs items by their keys. If a pair has two different numbers, one tree gets
/// new keys: the tree with fewer twins of the path takes the numbers of the other tree for its
/// paired twins. Thus each twin with no partner keeps the number of its own tree, and the keys
/// of each tree stay different. The items in a twin (the items of a twin module) get the new
/// key of the twin. The list has each pair with two different numbers or with a weak key (the
/// note of R72.1-3).
pub(crate) fn twin_pairs(before: &Workspace, after: &Workspace) -> (Option<(Workspace, Workspace)>, Vec<TwinPair>) {
    // The groups: the base path, the kind, and the flag of a macro definition.
    type Group = (String, &'static str, bool);
    let groups = |ws: &Workspace| -> BTreeMap<Group, Vec<(usize, String)>> {
        let mut out: BTreeMap<Group, Vec<(usize, String)>> = BTreeMap::new();
        for (path, item) in &ws.items {
            let Some(base) = twin_base(path) else { continue };
            // A use line has no twin: the `use` lines of a module compare as a set. A twin in a
            // twin keeps the rule of the source order.
            if item.kind == ItemKind::Use || base.contains('#') {
                continue;
            }
            let Ok(number) = path[base.len() + 1..].parse::<usize>() else { continue };
            out.entry((base.to_string(), kind_name(item.kind), item.macro_definition)).or_default().push((number, path.clone()));
        }
        for list in out.values_mut() {
            list.sort();
        }
        out
    };
    // The tokens of a twin, and of each item in it.
    let text = |ws: &Workspace, path: &str| -> Vec<(String, String)> {
        let inside = |key: &str| key.len() > path.len() && key.starts_with(path) && matches!(key.as_bytes()[path.len()], b':' | b'.');
        ws.items.range(path.to_string()..).take_while(|(key, _)| key.starts_with(path)).filter(|(key, _)| key.as_str() == path || inside(key)).map(|(key, item)| (key[path.len()..].to_string(), item.text.clone())).collect()
    };
    let (old_groups, new_groups) = (groups(before), groups(after));
    // For each tree: the twin keys that change (the old key to the new key).
    let mut keys: [BTreeMap<String, String>; 2] = Default::default();
    let mut listed: Vec<TwinPair> = Vec::new();
    for (group, old_list) in &old_groups {
        let Some(new_list) = new_groups.get(group) else { continue };
        let paths = |list: &Vec<(usize, String)>| list.iter().map(|(_, path)| path.clone()).collect::<Vec<_>>();
        let (old_ids, new_ids) = (twin_identities(before, &paths(old_list)), twin_identities(after, &paths(new_list)));
        let content = |ws: &Workspace, path: &str| text(ws, path).into_iter().map(|(key, text)| format!("{key}\u{0}{text}")).collect::<Vec<_>>().join("\u{1}");
        let old_things: Vec<Thing> = old_list.iter().zip(&old_ids).map(|((_, path), id)| Thing { group: String::new(), identity: Some(id.clone()), content: Some(content(before, path)) }).collect();
        let new_things: Vec<Thing> = new_list.iter().zip(&new_ids).map(|((_, path), id)| Thing { group: String::new(), identity: Some(id.clone()), content: Some(content(after, path)) }).collect();
        let tokens = |ws: &Workspace, path: &str| ws.items.get(path).map(|item| item.text.split(' ').map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
        let (old_tokens, new_tokens): (Vec<Vec<String>>, Vec<Vec<String>>) = (old_list.iter().map(|(_, path)| tokens(before, path)).collect(), new_list.iter().map(|(_, path)| tokens(after, path)).collect());
        let similarity = |a: usize, b: usize| semilla_core::matcher::token_similarity(&old_tokens[a], &new_tokens[b]);
        let ladder = Ladder { similarity: &similarity, threshold: TWIN_THRESHOLD, in_order: None };
        let found = semilla_core::matcher::pair(&old_things, &new_things, &ladder);
        let pairs: Vec<(usize, usize, Key)> = found.iter().map(|pair| (pair.old, pair.new, pair.key)).collect();
        // The tree with fewer twins takes the numbers of the other tree.
        let (side, from, to): (usize, &Vec<(usize, String)>, &Vec<(usize, String)>) = if old_list.len() <= new_list.len() { (0, old_list, new_list) } else { (1, new_list, old_list) };
        let own_pairs: Vec<(usize, usize, Key)> = pairs.iter().map(|(i, j, key)| if side == 0 { (*i, *j, *key) } else { (*j, *i, *key) }).collect();
        let mut map = BTreeMap::new();
        for (own, other, _) in &own_pairs {
            if from[*own].1 != to[*other].1 {
                map.insert(from[*own].1.clone(), to[*other].1.clone());
            }
        }
        // R72.1-1: a twin with no partner is added or removed. A twin of `from` with no partner
        // whose key a twin of the other tree has would pair by its key in the compare (the source
        // order): it gets a free number for the compare, and a note.
        let paired_from: BTreeSet<usize> = own_pairs.iter().map(|(own, _, _)| *own).collect();
        let base = &group.0;
        let mut next = from.iter().chain(to.iter()).map(|(number, _)| number + 1).max().unwrap_or(0);
        let mut lone: Vec<(String, String)> = Vec::new();
        for (at, (_, path)) in from.iter().enumerate() {
            if paired_from.contains(&at) || !to.iter().any(|(_, other)| other == path) {
                continue;
            }
            let ws = if side == 0 { before } else { after };
            while ws.items.contains_key(&format!("{base}#{next}")) || [before, after].iter().any(|tree| tree.items.contains_key(&format!("{base}#{next}"))) {
                next += 1;
            }
            let fresh = format!("{base}#{next}");
            next += 1;
            map.insert(path.clone(), fresh.clone());
            lone.push((path.clone(), fresh));
        }
        if map.is_empty() {
            continue;
        }
        // The keys of a tree stay different: else the group keeps the source order.
        let ws = if side == 0 { before } else { after };
        let result: BTreeSet<&String> = from.iter().map(|(_, path)| map.get(path).unwrap_or(path)).collect();
        if result.len() != from.len() || map.values().any(|new| !from.iter().any(|(_, other)| other == new) && !lone.iter().any(|(_, fresh)| fresh == new) && ws.items.contains_key(new)) {
            continue;
        }
        listed.extend(pairs.iter().filter(|(i, j, _)| old_list[*i].0 != new_list[*j].0).map(|(i, j, key)| (old_list[*i].1.clone(), new_list[*j].1.clone(), if side == 0 { new_list[*j].1.clone() } else { old_list[*i].1.clone() }, Some(*key))));
        listed.extend(lone.into_iter().map(|(path, fresh)| if side == 0 { (path, String::new(), fresh, None) } else { (String::new(), path, fresh, None) }));
        keys[side].extend(map);
    }
    if keys.iter().all(BTreeMap::is_empty) {
        return (None, listed);
    }
    (Some((rekeyed(before, &keys[0]), rekeyed(after, &keys[1]))), listed)
}

/// A copy of a workspace with new keys: each key of the map, and each key in it (an item of a
/// twin module, a method of an impl).
fn rekeyed(ws: &Workspace, map: &BTreeMap<String, String>) -> Workspace {
    let rekey = |key: &str| -> String {
        for (from, to) in map {
            if key == from {
                return to.clone();
            }
            if key.len() > from.len() && key.starts_with(from.as_str()) && matches!(key.as_bytes()[from.len()], b':' | b'.') {
                return format!("{to}{}", &key[from.len()..]);
            }
        }
        key.to_string()
    };
    let mut ws = ws.clone();
    if map.is_empty() {
        return ws;
    }
    ws.items = std::mem::take(&mut ws.items)
        .into_values()
        .map(|mut item| {
            item.path = rekey(&item.path);
            (item.path.clone(), item)
        })
        .collect();
    ws.module_files = std::mem::take(&mut ws.module_files).into_iter().map(|(key, value)| (rekey(&key), value)).collect();
    ws.inner_attrs = std::mem::take(&mut ws.inner_attrs).into_iter().map(|(key, value)| (rekey(&key), value)).collect();
    ws.inner_attrs_display = std::mem::take(&mut ws.inner_attrs_display).into_iter().map(|(key, value)| (rekey(&key), value)).collect();
    ws.macro_modules = std::mem::take(&mut ws.macro_modules).into_iter().map(|(key, value)| (rekey(&key), value)).collect();
    ws.body_index = Default::default();
    ws
}

/// R70.2-1 (d): an impl is identified by the definition of its trait and its self type, by the
/// compiler (the trait reference `<X as core::ops::arith::Add>` of the name record), not by the
/// text of the paths: `impl core::ops::Add for X` and `impl std::ops::Add for X` are one impl. An
/// impl of the old tree with no partner takes the key of the one impl of the new tree with no
/// partner and the same trait reference; each item of the impl takes the key too. The header
/// edit stays a signature fact of the impl: the rule hides no edit. `None` if no key changes, or
/// if a tree has no name record (the text keys stay: a removed and an added impl).
pub(crate) fn trait_path_pairs(before: &Workspace, after: &Workspace) -> Option<(Workspace, Workspace)> {
    before.names.get()?;
    after.names.get()?;
    // The impls of a tree that the other tree does not have, with the identity of the compiler.
    let lone = |ws: &Workspace, other: &Workspace| -> Vec<(String, Thing)> {
        ws.items
            .iter()
            .filter(|(path, item)| item.kind == ItemKind::Impl && !other.items.contains_key(*path))
            .filter_map(|(path, _)| Some((path.clone(), Thing { group: String::new(), identity: Some(crate::records::impl_trait_ref(ws, path)?), content: None })))
            .collect()
    };
    let (old, new) = (lone(before, after), lone(after, before));
    // R72.1-1 (part b): only the identity pairs two impls: an impl with a different trait or
    // self type is a different impl (no similarity).
    let none = |_: usize, _: usize| 0.0;
    let ladder = Ladder { similarity: &none, threshold: 1.0, in_order: None };
    let things = |list: &[(String, Thing)]| list.iter().map(|(_, thing)| thing.clone()).collect::<Vec<_>>();
    let mut map = BTreeMap::new();
    for pair in semilla_core::matcher::pair(&things(&old), &things(&new), &ladder).into_iter().filter(|pair| pair.key == Key::Identity) {
        let (from, to) = (&old[pair.old].0, &new[pair.new].0);
        if from != to && !before.items.contains_key(to) {
            map.insert(from.clone(), to.clone());
            // The items of an impl have the key `m::<X as T>::name`.
            if let (Some(from_items), Some(to_items)) = (impl_items_key(from), impl_items_key(to)) {
                map.insert(from_items, to_items);
            }
        }
    }
    (!map.is_empty()).then(|| (rekeyed(before, &map), after.clone()))
}

/// The start of the keys of the items of an impl: `m::impl <X as T>` gives `m::<X as T>`.
fn impl_items_key(path: &str) -> Option<String> {
    let (module, header) = path.split_once("::impl <")?;
    Some(format!("{module}::<{header}"))
}

/// The text of a twin that a step adds or removes (design 38.3): "(a twin of `P` under
/// `cfg(…)`) with the body: `{ … }`". `None` if `path` is not such a twin: the tree must have
/// the plain path `P` too, which only [`twin_keys`] makes.
pub(crate) fn twin_text(ws: &Workspace, path: &str) -> Option<(String, Option<String>)> {
    let base = twin_base(path)?;
    ws.items.get(base)?;
    let item = ws.items.get(path)?;
    // The `cfg` attributes of the item, as the source writes them. They come from the
    // attribute nodes of the item, and the body comes from the body node (design 57.2): a
    // filter of the source lines is never the text of a fact.
    let condition = |display: &String| {
        let inner = display.trim().strip_prefix('#')?.trim_start().strip_prefix('[')?.strip_suffix(']')?;
        Some(format!("`{}`", inner.trim()))
    };
    let conditions: Vec<String> = item.attrs.iter().zip(&item.display.attrs).filter(|(text, _)| crate::facts::attribute_name(text).as_deref() == Some("cfg")).filter_map(|(_, display)| condition(display)).collect();
    let under = if conditions.is_empty() { String::new() } else { format!(" under {}", conditions.join(" and ")) };
    let body = item.body.as_ref().map(|body| {
        let source = ws.rust_files.get(&item.file).and_then(|file| file.get(body.span.0..body.span.1));
        source.map_or_else(|| body.text.clone(), semilla_syntax::tree::one_line)
    });
    let (body_text, content) = match body {
        // Design 54.7: a long body shows its full text, with its lines.
        Some(body) if body.chars().count() > BODY_TEXT => (format!(" with the body: `{}`", semilla_syntax::tree::full(&body)), None),
        Some(body) => (format!(" with the body: `{body}`"), None),
        None => (String::new(), None),
    };
    Some((format!(" (a twin of `{base}`{under}){body_text}"), content))
}

/// The most characters of a twin body in a fact text. The report content has the full body.
const BODY_TEXT: usize = 200;
