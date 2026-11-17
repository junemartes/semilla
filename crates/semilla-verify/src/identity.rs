//! The derived identity (design 24.2, rule 4).
//!
//! In a step with no identity claim, Semilla finds the candidate pairs: a removed and an added
//! entity of the same kind. A rename keeps the owner and changes the name. A move keeps the name
//! and changes the owner. The two texts must be similar ([`crate::PAIR_THRESHOLD`]): the score
//! does not count a doc comment, an attribute, and a visibility. Semilla makes each name map of
//! the pairs into `rename` and `move-item` claims, and checks the step with them. The map with
//! the highest sum of the scores gives the facts "renamed `A` → `B`" or "moved `A` → `B`" with
//! the compare result (design 35), and the forced edits fold under the fact.
//!
//! The step can have other changes (design 36.1). Each difference of a pair that the new name
//! does not force is an edit under the name map, and it has its own fact at the new path. A
//! changed body goes to the fact of its edit. A changed body with no such fact goes to the
//! identity fact, which then has `code different` and the mismatch mark.
//!
//! Limits: at most 4 removed and 4 added entities of one kind in one owner, and at most
//! [`MAX_MAPS`] maps. Above a limit, or when two maps have the same highest score and leave the same number of
//! other facts, Semilla
//! derives nothing and gives a note. A pair that does not pass shows as removed and added, with
//! a note that gives the cause.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{Anchor, Category, Claim, ClaimStatus, Compare, Fact};
use semilla_syntax::rs::UseTree;
use semilla_syntax::{EditKind, ItemKind, Workspace};

use crate::{BodyOracle, ExprOracle, StepInput, StepOutcome};

/// The most entities of one kind in one owner for the search.
pub const MAX_GROUP: usize = 4;
/// The most name maps that the search tries.
pub const MAX_MAPS: usize = 24;
/// Two map scores with a smaller difference are equal.
const SCORE_TIE: f64 = 1e-9;

/// One candidate pair.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub old: String,
    pub new: String,
    /// `rename` or `move-item`.
    pub op: &'static str,
}

/// The owner of an entity: the struct of a field, else the parent path.
fn owner(path: &str) -> &str {
    if let Some((owner, field)) = path.rsplit_once('.').filter(|(_, field)| !field.contains("::")) {
        let _ = field;
        return owner;
    }
    path.rsplit_once("::").map(|(parent, _)| parent).unwrap_or("")
}

fn name(path: &str) -> &str {
    let last = path.rsplit("::").next().unwrap_or(path);
    last.rsplit_once('.').map(|(_, field)| field).unwrap_or(last)
}

fn kind_name(kind: ItemKind) -> &'static str {
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
    let none = BTreeSet::new();
    let removed = only_in(before, after, &none);
    let added = only_in(after, before, &none);
    let first = pairs_of(before, after, &removed, &added);
    // A new module that is not the new side of a pair: its items can be moved items (batch 5x,
    // part B). The check of the step decides if the module is a part of the move.
    let open: BTreeSet<&str> = added.iter().filter(|(path, kind)| *kind == ItemKind::Mod && !first.0.iter().any(|pair| pair.new == *path)).map(|(path, _)| *path).collect();
    if open.is_empty() {
        return first;
    }
    let added = only_in(after, before, &open);
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

/// The name maps: the matchings of the pairs with the most pairs (each entity in one pair at most).
/// `None` when there are more than [`MAX_MAPS`].
fn maps(pairs: &[Candidate]) -> Option<Vec<Vec<Candidate>>> {
    fn walk(pairs: &[Candidate], at: usize, used: &mut BTreeSet<String>, current: &mut Vec<Candidate>, out: &mut Vec<Vec<Candidate>>, best: &mut usize) -> bool {
        if out.len() > MAX_MAPS * 4 {
            return false;
        }
        if at == pairs.len() {
            if current.len() > *best {
                *best = current.len();
                out.clear();
            }
            if current.len() == *best && !current.is_empty() {
                out.push(current.clone());
            }
            return true;
        }
        let pair = &pairs[at];
        if !used.contains(&pair.old) && !used.contains(&pair.new) {
            used.insert(pair.old.clone());
            used.insert(pair.new.clone());
            current.push(pair.clone());
            let ok = walk(pairs, at + 1, used, current, out, best);
            current.pop();
            used.remove(&pair.old);
            used.remove(&pair.new);
            if !ok {
                return false;
            }
        }
        walk(pairs, at + 1, used, current, out, best)
    }
    let mut out = Vec::new();
    let mut best = 0;
    if !walk(pairs, 0, &mut BTreeSet::new(), &mut Vec::new(), &mut out, &mut best) || out.len() > MAX_MAPS {
        return None;
    }
    Some(out)
}

fn pair_text(map: &[Candidate]) -> String {
    map.iter().map(|c| format!("`{}` → `{}`", c.old, c.new)).collect::<Vec<_>>().join(", ")
}

/// The claims of one map. The IDs cannot be the ID of a claim of the agent.
fn claims_of(map: &[Candidate]) -> Vec<Claim> {
    map.iter()
        .enumerate()
        .map(|(index, pair)| {
            let to = if pair.op == "rename" { name(&pair.new).to_string() } else { pair.new.clone() };
            let mut args = serde_json::Map::new();
            args.insert("to".into(), serde_json::Value::String(to));
            Claim { id: format!("derived-identity-{}", index + 1), op: pair.op.to_string(), at: Some(Anchor::item(&pair.old)), args }
        })
        .collect()
}

/// True if the check with the claims of a map proves the identity.
fn passes(outcome: &StepOutcome, claims: &[Claim]) -> Result<(), String> {
    let report = &outcome.report;
    for claim in claims {
        match report.claims.iter().find(|r| r.claim == claim.id).map(|r| &r.status) {
            Some(status) if status.is_verified() => {}
            Some(ClaimStatus::Refuted { reason } | ClaimStatus::Malformed { reason } | ClaimStatus::Pending { reason }) => return Err(reason.clone()),
            Some(other) => return Err(other.text()),
            None => return Err("no result".into()),
        }
    }
    if let Some(error) = report.errors.first() {
        return Err(error.clone());
    }
    if !report.uncovered.is_empty() {
        return Err("an edit has no fact".into());
    }
    Ok(())
}

/// Tries the derived identity on the plain outcome of a step with no identity claim.
pub fn derive(input: &StepInput, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle, mut plain: StepOutcome) -> StepOutcome {
    let (pairs, notes) = candidates(input.before, input.after);
    plain.report.notes.extend(notes);
    if pairs.is_empty() {
        return plain;
    }
    plain.report.identity_candidates = true;
    let Some(mut maps) = maps(&pairs) else {
        plain.report.notes.push(format!("{} show(s) as removed and added: Semilla does not derive the rename or the move (the pairs have more than {MAX_MAPS} name maps)", pair_text(&pairs)));
        return plain;
    };
    // Phase 1 has no MIR: the derivation waits for phase 2.
    if plain.bodies.is_empty() {
        plain.report.notes.push(format!("a rename or a move is possible ({}): phase 2 tries the derivation", pair_text(&maps[0])));
        return plain;
    }
    // The score of a map: the sum of the similar scores of its pairs. The map with the highest
    // score that passes the check gives the facts (design 36.1).
    let score = |map: &[Candidate]| -> f64 { map.iter().map(|pair| similarity(&input.before.items[&pair.old], &input.after.items[&pair.new])).sum() };
    let mut scored: Vec<(f64, Vec<Candidate>)> = maps.drain(..).map(|map| (score(&map), map)).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut passed: Vec<(f64, Vec<Candidate>, Vec<Claim>, StepOutcome)> = Vec::new();
    let mut failures = Vec::new();
    for (score, map) in &scored {
        // A map with a lower score than a map that passes is not necessary.
        if passed.first().is_some_and(|(best, ..)| *score < *best - SCORE_TIE) {
            break;
        }
        let mut step = input.step.clone();
        let claims = claims_of(map);
        step.claims.extend(claims.iter().cloned());
        let outcome = crate::verify_core(&StepInput { step: &step, before: input.before, after: input.after }, bodies, exprs, true, true);
        match passes(&outcome, &claims) {
            Ok(()) => passed.push((*score, map.clone(), claims, outcome)),
            Err(reason) => failures.push((map.clone(), reason, outcome)),
        }
    }
    // Maps with the same score: the map that leaves the smallest number of other facts wins
    // (a wrong map of two fields leaves a change of the member order).
    let left = |outcome: &StepOutcome| outcome.report.facts.len() + outcome.report.uncovered.len();
    if let Some(least) = passed.iter().map(|(_, _, _, outcome)| left(outcome)).min() {
        if passed.iter().filter(|(_, _, _, outcome)| left(outcome) == least).count() == 1 {
            passed.retain(|(_, _, _, outcome)| left(outcome) == least);
        }
    }
    if passed.len() > 1 {
        let list = passed.iter().map(|(_, map, _, _)| format!("({})", pair_text(map))).collect::<Vec<_>>().join(" or ");
        plain.report.notes.push(format!("{} show(s) as removed and added: Semilla does not derive the rename or the move (more than one name map has the same similar score and the same number of other facts: {list})", pair_text(&passed[0].1)));
        return plain;
    }
    let Some((_, map, claims, outcome)) = passed.pop() else {
        // With no derivation, the pair shows as removed and added. Semilla diffs the two items
        // under the name map and shows each edit as a fact under the pair, so that a hidden
        // change (a changed literal) has its own fact (trial 3, A2).
        if let Some((map, _, failed)) = failures.first() {
            let known: BTreeSet<String> = plain.report.facts.iter().map(|f| f.description.clone()).collect();
            let mut extra = Vec::new();
            for pair in map {
                let inside = |item: &str| item == pair.new || item.starts_with(&format!("{}::", pair.new)) || item.starts_with(&format!("{}.", pair.new));
                // Under the name map, each edit that is left inside the new item is a real
                // difference: the map hides the forced edits of the name.
                let changed = failed.bodies.iter().any(|b| inside(&b.name) && !matches!(b.status, crate::BodyStatus::Equal { .. }));
                for edit in failed.edits.iter().filter(|e| e.anchor().is_some_and(|a| inside(&a.item)) && !matches!(e.kind, EditKind::ItemAdded | EditKind::ItemRemoved)) {
                    let text = crate::edits::display_text(edit);
                    if known.iter().any(|k| k.ends_with(&text)) {
                        continue;
                    }
                    // A doc test is code (brief 5h, B1). Semilla does not compile it: the fact
                    // has no compare, with the cause.
                    let doc_test = edit.anchor().and_then(|anchor| crate::facts::doc_test_fact(&anchor.item, edit));
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
                        affected: if changed { vec![pair.new.clone()] } else { Vec::new() },
                        ..Default::default()
                    });
                }
            }
            let start = plain.report.facts.len();
            for (index, mut fact) in extra.into_iter().enumerate() {
                fact.id = format!("f{}", start + index + 1);
                plain.report.facts.push(fact);
            }
        }
        // The note gives the cause (design 36.1). The pair warning of the change set reads it.
        for (map, reason, _) in failures.iter().take(1) {
            let what = if map.iter().all(|c| c.op == "move-item") { "move" } else { "rename" };
            plain.report.notes.push(format!("{} show(s) as removed and added: Semilla does not derive the {what} ({reason})", pair_text(map)));
        }
        return plain;
    };
    convert(input, &map, &claims, outcome)
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
            note: note.take(),
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
fn module_of<'a>(path: &'a str, workspace: &Workspace) -> Option<&'a str> {
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
fn use_names(tree: &UseTree, prefix: &[String], out: &mut Vec<(String, Vec<String>)>) {
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
fn target_in(module: &str, name: &str, workspace: &Workspace, depth: usize) -> Option<String> {
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
/// (`P#0`, `P#1`) in the other tree keeps its key for the twin with its text: the key `P#N` of
/// that twin becomes `P`. The compare ignores the attributes, the docs, and the visibility. A
/// key changes only for an item with no inner items, and only if exactly one twin has the
/// text. `None` if no key changes: then the trees stay as they are.
pub(crate) fn twin_keys(before: &Workspace, after: &Workspace) -> Option<(Workspace, Workspace)> {
    let text = |item: &semilla_syntax::Item| without_free_parts(&without_docs(&item.text));
    // The keys of `twins` that take the plain path: (plain path, twin key).
    let keys = |single: &Workspace, twins: &Workspace| -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut bases: BTreeMap<&str, Vec<&semilla_syntax::Item>> = BTreeMap::new();
        for item in twins.items.values() {
            if let Some(base) = twin_base(&item.path) {
                bases.entry(base).or_default().push(item);
            }
        }
        for (base, found) in bases {
            let Some(old) = single.items.get(base) else { continue };
            if twins.items.contains_key(base) || found.len() < 2 || single.items.keys().any(|key| twin_base(key) == Some(base)) {
                continue;
            }
            let inner = |ws: &Workspace, path: &str| ws.items.keys().any(|key| key.len() > path.len() && key.starts_with(path) && matches!(key.as_bytes()[path.len()], b':' | b'.'));
            if inner(single, base) {
                continue;
            }
            let same: Vec<&&semilla_syntax::Item> = found.iter().filter(|item| item.kind == old.kind && text(item) == text(old) && !inner(twins, &item.path)).collect();
            if let [twin] = same[..] {
                out.push((base.to_string(), twin.path.clone()));
            }
        }
        out
    };
    let (from_before, from_after) = (keys(before, after), keys(after, before));
    if from_before.is_empty() && from_after.is_empty() {
        return None;
    }
    let apply = |ws: &Workspace, keys: &[(String, String)]| -> Workspace {
        let mut ws = ws.clone();
        for (base, key) in keys {
            if let Some(mut item) = ws.items.remove(key) {
                item.path = base.clone();
                ws.items.insert(base.clone(), item);
            }
        }
        ws.body_index = Default::default();
        ws
    };
    Some((apply(before, &from_after), apply(after, &from_before)))
}

/// The text of a twin that a step adds or removes (design 38.3): "(a twin of `P` under
/// `cfg(…)`) with the body: `{ … }`". `None` if `path` is not such a twin: the tree must have
/// the plain path `P` too, which only [`twin_keys`] makes.
pub(crate) fn twin_text(ws: &Workspace, path: &str) -> Option<(String, Option<String>)> {
    let base = twin_base(path)?;
    ws.items.get(base)?;
    let item = ws.items.get(path)?;
    // The `cfg` attributes of the source, as the source writes them.
    let source = one_line_source(&item.source);
    let mut conditions = Vec::new();
    let mut rest = source.as_str();
    while let Some(at) = rest.find("#[cfg(") {
        let tail = &rest[at + 2..];
        let mut depth = 0usize;
        let end = tail.char_indices().find(|(_, c)| {
            match c {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => {}
            }
            *c == ']' && depth == 0
        });
        let Some((end, _)) = end else { break };
        conditions.push(format!("`{}`", &tail[..end]));
        rest = &tail[end..];
    }
    let under = if conditions.is_empty() { String::new() } else { format!(" under {}", conditions.join(" and ")) };
    let body = item.body.as_ref().map(|_| {
        let at = source.find('{').unwrap_or(0);
        source[at..].to_string()
    });
    let (body_text, content) = match body {
        Some(body) if body.chars().count() > BODY_TEXT => {
            let cut: String = body.chars().take(BODY_TEXT).collect();
            (format!(" with the body: `{}…`", cut.trim_end()), Some(format!("+ {body}")))
        }
        Some(body) => (format!(" with the body: `{body}`"), None),
        None => (String::new(), None),
    };
    Some((format!(" (a twin of `{base}`{under}){body_text}"), content))
}

/// The most characters of a twin body in a fact text. The report content has the full body.
const BODY_TEXT: usize = 200;

/// A source text on one line, with no doc comment line.
fn one_line_source(source: &str) -> String {
    let lines: Vec<&str> = source.lines().filter(|line| !line.trim_start().starts_with("///") && !line.trim_start().starts_with("//!")).collect();
    semilla_syntax::tree::one_line(&lines.join("\n"))
}
