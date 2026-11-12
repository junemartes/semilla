//! The derived identity (design 24.2, rule 4).
//!
//! In a step with no identity claim, Semilla finds the candidate pairs: a removed and an added
//! entity of the same kind. A rename keeps the owner and changes the name. A move keeps the name
//! and changes the owner. Semilla makes each name map of the pairs into `rename` and `move-item`
//! claims, and checks the step with them. This is the same check as for a claimed identity
//! (design 22.4). If exactly one map passes, the step gets the fact "renamed `A` → `B` (proved Nk)"
//! or "moved `A` → `B` (proved Nk)", and the forced edits fold under it.
//!
//! Limits: at most 4 removed and 4 added entities of one kind in one owner, and at most
//! [`MAX_MAPS`] maps. Above a limit, or when more than one map passes, Semilla derives nothing and
//! gives a note. A pair that does not pass shows as removed and added, with a note.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{Anchor, Claim, ClaimStatus, Class, Fact};
use semilla_syntax::{EditKind, ItemKind, Workspace};

use crate::{BodyOracle, ExprOracle, StepInput, StepOutcome};

/// The most entities of one kind in one owner for the search.
pub const MAX_GROUP: usize = 4;
/// The most name maps that the search tries.
pub const MAX_MAPS: usize = 24;

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
fn only_in<'a>(side: &'a Workspace, other: &Workspace) -> Vec<(&'a str, ItemKind)> {
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
                if paths.contains(current) {
                    return false;
                }
                current = owner(current);
            }
            true
        })
        .map(|item| (item.path.as_str(), item.kind))
        .collect()
}

/// The similarity of two item texts, with the own name of each item as one token `$name`.
fn similarity(a: &semilla_syntax::Item, b: &semilla_syntax::Item) -> f64 {
    let tokens = |item: &semilla_syntax::Item| -> Vec<String> {
        let own = name(&item.path);
        item.text.split(' ').filter(|t| !t.is_empty()).map(|t| if t == own { "$name".to_string() } else { t.to_string() }).collect()
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
    let removed = only_in(before, after);
    let added = only_in(after, before);
    let mut groups: BTreeMap<(&str, &str), (usize, usize)> = BTreeMap::new();
    for (path, kind) in &removed {
        groups.entry((kind_name(*kind), owner(path))).or_default().0 += 1;
    }
    for (path, kind) in &added {
        groups.entry((kind_name(*kind), owner(path))).or_default().1 += 1;
    }
    let mut notes = BTreeSet::new();
    let mut too_many = |kind: ItemKind, path: &str| {
        let (r, a) = groups.get(&(kind_name(kind), owner(path))).copied().unwrap_or_default();
        let over = r > MAX_GROUP || a > MAX_GROUP;
        if over {
            let place = if owner(path).is_empty() { "the crate root".to_string() } else { format!("`{}`", owner(path)) };
            notes.insert(format!("{r} removed and {a} added {}(s) in {place}: Semilla derives no rename or move above {MAX_GROUP}; claim it with `--claim 'rename OLD to=NEW'` or `--claim 'move-item OLD to=NEW'`", kind_name(kind)));
        }
        over
    };
    let mut found = Vec::new();
    for (old, old_kind) in &removed {
        for (new, new_kind) in &added {
            if old_kind != new_kind {
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
            Some(ClaimStatus::Unproved { reason } | ClaimStatus::Refuted { reason } | ClaimStatus::Malformed { reason } | ClaimStatus::Pending { reason }) => return Err(reason.clone()),
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
    let Some(maps) = maps(&pairs) else {
        plain.report.notes.push(format!("more than {MAX_MAPS} name maps for the pairs {}: Semilla derives no rename or move; claim them with `--claim`", pair_text(&pairs)));
        return plain;
    };
    // Phase 1 has no MIR: the proof waits for phase 2.
    if plain.bodies.is_empty() {
        plain.report.notes.push(format!("a rename or a move is possible ({}): phase 2 tries the proof", pair_text(&maps[0])));
        return plain;
    }
    let mut passed = Vec::new();
    let mut failures = Vec::new();
    for map in &maps {
        let mut step = input.step.clone();
        let claims = claims_of(map);
        step.claims.extend(claims.iter().cloned());
        let outcome = crate::verify_core(&StepInput { step: &step, before: input.before, after: input.after }, bodies, exprs, true, true);
        match passes(&outcome, &claims) {
            Ok(()) => passed.push((map.clone(), claims, outcome)),
            Err(reason) => failures.push((map.clone(), reason, outcome)),
        }
    }
    if passed.len() > 1 {
        let list = passed.iter().map(|(map, _, _)| format!("({})", pair_text(map))).collect::<Vec<_>>().join(" or ");
        plain.report.notes.push(format!("more than one name map passes: {list}; Semilla derives no rename or move. Claim the one that you mean with `--claim 'rename OLD to=NEW'`"));
        return plain;
    }
    let Some((map, claims, outcome)) = passed.pop() else {
        // A4 (trial 3, A2): with no proof, the pair shows as removed and added. Semilla diffs the
        // two items under the name map and shows each edit as a fact under the pair, so that a
        // hidden change (a changed literal) has its own fact.
        // The differences of the pairs of the first map, for the note (trial 4, D8).
        let mut differences: Vec<String> = Vec::new();
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
                    if !differences.contains(&text) {
                        differences.push(text.clone());
                    }
                    if known.iter().any(|k| k.ends_with(&text)) {
                        continue;
                    }
                    // A doc test is code (brief 5h, B1): the class is `logic`, with the diff.
                    let doc_test = edit.anchor().and_then(|anchor| crate::facts::doc_test_fact(&anchor.item, edit));
                    extra.push(Fact {
                        at: edit.anchor().cloned(),
                        file: edit.file.clone(),
                        part: "pair diff".into(),
                        description: format!("`{}` differs from the removed `{}`: {text}", name(&pair.new), name(&pair.old)),
                        status: "derived".into(),
                        content: doc_test.as_ref().map(|(_, content)| content.clone()),
                        class: Some(if changed || doc_test.is_some() { Class::Logic } else { Class::Other }),
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
        for (map, reason, _) in failures.iter().take(1) {
            let what = if map.iter().all(|c| c.op == "move-item") { "move" } else { "rename" };
            if !differences.is_empty() {
                // The pair is not an identity. The note names the difference of the pair, not
                // the first changed body of the step.
                let more = if differences.len() > 3 { format!(", and {} more", differences.len() - 3) } else { String::new() };
                plain.report.notes.push(format!("{} show(s) as removed and added: the pair has a difference: {}{more}. A {what} with no other change in its step gets a proof", pair_text(map), differences.iter().take(3).cloned().collect::<Vec<_>>().join("; ")));
            } else if reason.contains("split the step") {
                let other = reason.strip_prefix("an identity step must be neutral, but ").and_then(|r| r.strip_suffix(": split the step")).unwrap_or("the step has other changes");
                plain.report.notes.push(format!("{} show(s) as removed and added: {other}. Put the {what} in its own step to get a proof", pair_text(map)));
            } else {
                plain.report.notes.push(format!("{} show(s) as removed and added: no proof of the {what} ({reason})", pair_text(map)));
            }
        }
        return plain;
    };
    convert(input, &map, &claims, outcome)
}

/// Makes the results of the derived claims into facts. The edits and the bodies of each claim go
/// to its fact.
fn convert(input: &StepInput, map: &[Candidate], claims: &[Claim], mut outcome: StepOutcome) -> StepOutcome {
    let first = input.step.claims.len();
    let report = &mut outcome.report;
    let mut identity_facts = Vec::new();
    for (pair, claim) in map.iter().zip(claims) {
        let result = report.claims.iter().find(|r| r.claim == claim.id).cloned();
        let Some(result) = result else { continue };
        let proof = match &result.status {
            ClaimStatus::Verified { proof } => proof.clone(),
            _ => String::new(),
        };
        let verb = if pair.op == "rename" { "renamed" } else { "moved" };
        let spotlight_why = outcome.identity_reason.clone().filter(|_| identity_facts.is_empty());
        identity_facts.push(Fact {
            at: Some(Anchor::item(&pair.old)),
            file: input.before.items.get(&pair.old).map(|item| item.file.clone()),
            part: "identity".into(),
            description: format!("{verb} `{}` → `{}` (proved {proof})", pair.old, pair.new),
            status: "derived".into(),
            class: Some(Class::Neutral),
            affected: result.affected.clone(),
            spotlight: spotlight_why.is_some(),
            spotlight_why,
            reason: input.step.reasons.get(&pair.old).cloned(),
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
fn use_names(tree: &syn::UseTree, prefix: &[String], out: &mut Vec<(String, Vec<String>)>) {
    let with = |ident: &syn::Ident| -> Vec<String> {
        // `a::b::{self}` is `a::b`.
        if ident == "self" { prefix.to_vec() } else { prefix.iter().cloned().chain([ident.to_string()]).collect() }
    };
    match tree {
        syn::UseTree::Path(path) => use_names(&path.tree, &with(&path.ident), out),
        syn::UseTree::Name(name) => {
            let path = with(&name.ident);
            if let Some(last) = path.last().cloned() {
                out.push((last, path));
            }
        }
        syn::UseTree::Rename(rename) => out.push((rename.rename.to_string(), with(&rename.ident))),
        syn::UseTree::Group(group) => group.items.iter().for_each(|tree| use_names(tree, prefix, out)),
        // A glob import gives no name that the text shows.
        syn::UseTree::Glob(_) => {}
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
        let Ok(parsed) = syn::parse_str::<syn::ItemUse>(&import.source) else { return None };
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
    use syn::visit::Visit;
    #[derive(Default)]
    struct Names {
        used: BTreeSet<String>,
        bound: BTreeSet<String>,
    }
    impl<'ast> Visit<'ast> for Names {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if path.leading_colon.is_none() {
                if let Some(first) = path.segments.first() {
                    self.used.insert(first.ident.to_string());
                }
            }
            syn::visit::visit_path(self, path);
        }
        fn visit_pat_ident(&mut self, pat: &'ast syn::PatIdent) {
            self.bound.insert(pat.ident.to_string());
            syn::visit::visit_pat_ident(self, pat);
        }
        fn visit_generic_param(&mut self, param: &'ast syn::GenericParam) {
            match param {
                syn::GenericParam::Type(param) => drop(self.bound.insert(param.ident.to_string())),
                syn::GenericParam::Const(param) => drop(self.bound.insert(param.ident.to_string())),
                syn::GenericParam::Lifetime(_) => {}
            }
            syn::visit::visit_generic_param(self, param);
        }
        // The name of a macro is in another name space. Its tokens are not parsed code.
        fn visit_macro(&mut self, _: &'ast syn::Macro) {}
    }
    let mut out = BTreeMap::new();
    let (Some(module), Ok(parsed)) = (module_of(&item.path, workspace), syn::parse_str::<syn::Item>(&item.source)) else { return out };
    let mut names = Names::default();
    names.visit_item(&parsed);
    for name in names.used.difference(&names.bound) {
        if matches!(name.as_str(), "self" | "Self" | "crate" | "super") {
            continue;
        }
        if let Some(target) = target_in(module, name, workspace, 8) {
            out.insert(name.clone(), target);
        }
    }
    out
}
