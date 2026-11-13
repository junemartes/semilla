//! The derived facts (design 22.5).
//!
//! A fact is the difference in one part of a paired entity: the type of a field, a new member, a
//! parameter, the return type, a visibility, an attribute. The diff determines it, so the agent
//! writes no claim. The mechanism is generic: each edit of an entity part that no claim explains
//! becomes (a part of) one fact. A body edit is never a fact.

use std::collections::BTreeMap;

use semilla_core::{Anchor, Class, Fact, SiteGroup, Step};
use semilla_syntax::{Edit, EditKind, ItemKind, Workspace};

use crate::checks::BodyIndex;
use crate::{BodyComparison, BodyStatus};

/// The facts of a step, and the fact that owns each edit.
pub struct Derived {
    pub facts: Vec<Fact>,
    /// The index of the fact that owns each edit, or `None`.
    pub fact_of: Vec<Option<usize>>,
    /// The items whose changed MIR a fact explains (design 22.6, F5). They are not indirect
    /// changes.
    pub attributed: std::collections::BTreeSet<String>,
}

/// The key of a fact: the entity anchor and the part.
type Key = (String, String);

/// Derives the facts from the edits that no claim explains.
pub fn derive(step: &Step, edits: &[Edit], owners: &[Option<usize>], before: &Workspace, after: &Workspace, bodies: &[BodyComparison], body_index: &BodyIndex, callees: Option<&BTreeMap<String, crate::ItemFacts>>, renames: &semilla_syntax::Renames, syntax_sites: bool) -> Derived {
    let mut keys: Vec<Key> = Vec::new();
    let mut parts: BTreeMap<Key, (Anchor, Vec<usize>, Vec<String>)> = BTreeMap::new();
    let mut fact_key: Vec<Option<Key>> = vec![None; edits.len()];
    let mut add = |key: Key, anchor: Anchor, index: usize, description: String, keys: &mut Vec<Key>| {
        let entry = parts.entry(key.clone()).or_insert_with(|| {
            keys.push(key.clone());
            (anchor, Vec::new(), Vec::new())
        });
        entry.1.push(index);
        if !description.is_empty() && !entry.2.contains(&description) {
            entry.2.push(description);
        }
    };
    let free: Vec<usize> = (0..edits.len()).filter(|&i| owners[i].is_none()).collect();

    // 1. The member items that are new or removed (fields and variants), and the trait impls.
    for &index in &free {
        let edit = &edits[index];
        let added = edit.kind == EditKind::ItemAdded;
        if !added && edit.kind != EditKind::ItemRemoved {
            continue;
        }
        let (Some(anchor), ws) = (if added { (edit.after.as_ref(), after) } else { (edit.before.as_ref(), before) }) else { continue };
        let Some(item) = ws.items.get(&anchor.item) else { continue };
        let verb = if added { "added" } else { "removed" };
        match item.kind {
            ItemKind::Field | ItemKind::Variant => {
                let owner = owner_of(&item.path);
                // A member of a type that the same step adds or removes belongs to the fact
                // "added `T`" or "removed `T`" (trial 4, D6). It gets no fact of its own. The
                // type is the owner, or the owner of the variant for a field of a variant.
                let same_change = |path: &str| free.iter().any(|&other| other != index && edits[other].kind == edit.kind && edits[other].item() == Some(path));
                let variant_owner = ws.items.get(&owner).filter(|parent| parent.kind == ItemKind::Variant).map(|parent| owner_of(&parent.path));
                if same_change(&owner) && variant_owner.is_none() || variant_owner.as_deref().is_some_and(|root| same_change(root)) {
                    continue;
                }
                // A tuple field of a new or removed variant belongs to the variant fact.
                if let Some(tuple) = tuple_owner(&item.path, item.kind).filter(|tuple| !same_change(tuple)) {
                    let key = (tuple.clone(), "tuple fields".to_string());
                    fact_key[index] = Some(key.clone());
                    add(key, Anchor::item(&tuple), index, tuple_description(&tuple, before, after), &mut keys);
                    continue;
                }
                // A field of a new or removed variant belongs to the variant fact.
                let parent_edit = free.iter().find(|&&other| other != index && edits[other].kind == edit.kind && edits[other].item() == Some(owner.as_str()));
                let target = if parent_edit.is_some() { owner.clone() } else { item.path.clone() };
                let key = (target.clone(), format!("member {verb}"));
                fact_key[index] = Some(key.clone());
                let what = if item.kind == ItemKind::Variant { "variant" } else { "field" };
                // The signature of a tuple variant has no field types: add them.
                let fields = tuple_fields(&item.path, ws);
                let signature = if item.kind == ItemKind::Variant && fields != "()" { format!("{}{fields}", item.path.rsplit("::").next().unwrap_or(&item.path)) } else { item.signature.clone() };
                let description = if parent_edit.is_some() { String::new() } else { format!("{what} `{}` {verb}: `{signature}`", item.path) };
                add(key, Anchor::item(&target), index, description, &mut keys);
            }
            ItemKind::Impl if item.path.contains(" as ") => {
                let key = (item.path.clone(), format!("trait impl {verb}"));
                fact_key[index] = Some(key.clone());
                add(key, Anchor::item(&item.path), index, format!("trait impl `{}` {verb}", item.path.rsplit("::impl ").next().unwrap_or(&item.path)), &mut keys);
            }
            _ => {}
        }
    }
    // The methods of a new or removed trait impl belong to the impl fact.
    for &index in &free {
        let edit = &edits[index];
        if fact_key[index].is_some() || !matches!(edit.kind, EditKind::ItemAdded | EditKind::ItemRemoved) {
            continue;
        }
        let Some(path) = edit.item() else { continue };
        let impl_key = keys.iter().find(|(entity, part)| part.starts_with("trait impl") && edit.kind == if part.ends_with("added") { EditKind::ItemAdded } else { EditKind::ItemRemoved } && entity.split_once("::impl ").is_some_and(|(module, owner)| path.strip_prefix(&format!("{module}::{owner}::")).is_some_and(|rest| !rest.contains("::"))));
        if let Some(key) = impl_key.cloned() {
            fact_key[index] = Some(key.clone());
            let anchor = Anchor::item(&key.0);
            add(key, anchor, index, String::new(), &mut keys);
        }
    }

    // 2. The parts of paired entities.
    // The visibility facts that change the public API.
    let mut api_keys: std::collections::BTreeSet<Key> = std::collections::BTreeSet::new();
    for &index in &free {
        if fact_key[index].is_some() {
            continue;
        }
        let edit = &edits[index];
        let Some(anchor) = edit.anchor().cloned() else { continue };
        let item = after.items.get(&anchor.item).or_else(|| before.items.get(&anchor.item));
        let old_new = || format!("`{}` → `{}`", short(&edit.text_before), short(&edit.text_after));
        let found: Option<(Key, String)> = match edit.kind {
            EditKind::Visibility => {
                // An empty visibility shows as `private` (batch 5x).
                let side = |text: &Option<String>| if text.as_deref().is_none_or(|text| text.trim().is_empty()) { "private".to_string() } else { short(text) };
                // A change of the public API is a fact to read (batch 5x, round 2).
                let api = crate::visibility_api_change(edit, before, after).map(|text| format!(" ({text})")).unwrap_or_default();
                if !api.is_empty() {
                    api_keys.insert((anchor.item.clone(), "visibility".to_string()));
                }
                Some(((anchor.item.clone(), "visibility".into()), format!("visibility of `{}`: `{}` → `{}`{api}", anchor.item, side(&edit.text_before), side(&edit.text_after))))
            }
            EditKind::Attrs => Some(((anchor.item.clone(), "attribute".into()), attribute_description(&anchor.item, edit))),
            EditKind::Members => members_fact(edit, &anchor, item, &keys).map(|(key, text)| (key, text)),
            EditKind::Signature => match edit.node_after.or(edit.node_before) {
                Some(part) => {
                    let name = match part {
                        "param" => "parameter",
                        "params" => "parameter order",
                        "ret" => "return type",
                        "generics" => "generics",
                        "where" => "where clause",
                        "qualifiers" => "qualifiers",
                        _ => part,
                    };
                    Some(((anchor.to_string(), name.to_string()), parameter_description(&anchor, name, edit)))
                }
                None => signature_part(edit).map(|part| {
                    let key = match tuple_owner(&anchor.item, item.map(|i| i.kind).unwrap_or(ItemKind::Other)) {
                        Some(tuple) => (tuple, "tuple fields".to_string()),
                        None => (anchor.item.clone(), part.to_string()),
                    };
                    let text = match (part, const_parts(&edit.text_before), const_parts(&edit.text_after)) {
                        ("value", Some((_, old)), Some((_, new))) => format!("value of `{}` changed: {old} → {new}", anchor.item),
                        _ => format!("{part} of `{}`: {}", anchor.item, old_new()),
                    };
                    (key, text)
                }),
            },
            _ => None,
        };
        if let Some((key, description)) = found {
            fact_key[index] = Some(key.clone());
            let anchor = if key.1 == "tuple fields" { Anchor::item(&key.0) } else { anchor };
            let description = if key.1 == "tuple fields" { tuple_description(&key.0, before, after) } else { description };
            add(key, anchor, index, description, &mut keys);
        }
    }

    // A removed and an added parameter of one function with the same type at the same position:
    // one fact "parameter `a` renamed to `b`". The body proof gives the class.
    merge_parameter_renames(edits, before, after, &mut keys, &mut parts, &mut fact_key);

    // 3. The forced edits at other sites (design 22.6, F1 and F2). A site must have exactly the
    //    forced edit (F6). A call site needs the MIR callee list as the evidence that it calls the
    //    changed function; without it, the site stays uncovered.
    let mut sites: BTreeMap<Key, Vec<(String, Anchor)>> = BTreeMap::new();
    let mut side_effects: BTreeMap<Key, Vec<String>> = BTreeMap::new();
    // Phase 1 has no MIR callee list. In the new protocol (`syntax_sites`), the syntax call graph
    // groups the call sites under their fact for the display. The class stays pending, and phase 2 uses the MIR list.
    let syntax_calls: BTreeMap<String, std::collections::BTreeSet<String>> = if syntax_sites && callees.is_none() && !body_index.available {
        let graph = semilla_syntax::graph::workspace_graph(after, &BTreeMap::new(), &BTreeMap::new());
        let mut calls: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for edge in graph.edges.iter().filter(|e| e.kind == semilla_core::view::GraphEdgeKind::Call) {
            calls.entry(edge.from.clone()).or_default().insert(edge.to.clone());
        }
        calls
    } else {
        BTreeMap::new()
    };
    for &index in &free {
        if fact_key[index].is_some() {
            continue;
        }
        let edit = &edits[index];
        let (Some(old_anchor), Some(new_anchor)) = (&edit.before, &edit.after) else { continue };
        if !matches!(edit.kind, EditKind::Replaced | EditKind::Modified) {
            continue;
        }
        let old_text = renames.apply(&old_anchor.item, edit.text_before.as_deref().unwrap_or(""));
        let new_text = edit.text_after.clone().unwrap_or_default();
        let found = match (edit.node_before, edit.node_after) {
            (Some("call" | "method_call"), Some("call" | "method_call")) => call_site(&old_text, &new_text, &keys, &parts, edits, before, after).filter(|(key, _, _)| {
                // The evidence: the MIR of the site calls the changed function.
                // A call to another crate of the workspace is in the external calls (D4).
                let function = key.0.split('/').next().unwrap_or("");
                callees.and_then(|map| map.get(&new_anchor.item)).is_some_and(|facts| facts.callees.iter().chain(&facts.externals).any(|c| c == function))
                    || syntax_calls.get(&new_anchor.item).is_some_and(|calls| calls.contains(function))
            }),
            (Some("struct"), Some("struct")) => literal_site(&old_text, &new_text, &keys, after, before),
            (Some("pattern"), Some("pattern")) => pattern_site(&old_text, &new_text, &keys).map(|(key, label)| (key, label, None)),
            _ => None,
        };
        if let Some((key, label, effect)) = found {
            fact_key[index] = Some(key.clone());
            parts.get_mut(&key).unwrap().1.push(index);
            sites.entry(key.clone()).or_default().push((label, new_anchor.clone()));
            if let Some(effect) = effect {
                side_effects.entry(key).or_default().push(format!("`{effect}` at `{new_anchor}`"));
            }
        }
    }

    // 4. The facts, in the order of their first edit, with the affected bodies (F5) and the class.
    let changed: Vec<(String, String)> = bodies
        .iter()
        .filter(|b| !matches!(b.status, BodyStatus::Equal { .. }))
        .map(|b| (b.name.clone(), body_index.item_of(b).unwrap_or_else(|| b.name.clone())))
        .collect();
    let mut attributed = std::collections::BTreeSet::new();
    let mut facts = Vec::new();
    let mut index_of: BTreeMap<Key, usize> = BTreeMap::new();
    for key in &keys {
        let (anchor, edit_indexes, descriptions) = &parts[key];
        let first = &edits[edit_indexes[0]];
        let mut spotlight_why = spotlight(&key.1, anchor, edit_indexes.iter().map(|&i| &edits[i]).collect(), before, after, body_index, callees);
        if let Some(effects) = side_effects.get(key) {
            let text = format!("a removed argument with a possible side effect: {}", effects.join(", "));
            spotlight_why = Some(match spotlight_why {
                Some(why) => format!("{why}; {text}"),
                None => text,
            });
        }
        // The affected bodies: the changed bodies that use the entity.
        let mut affected: Vec<String> = changed.iter().filter(|(_, item)| uses(key, item, after, before, callees)).map(|(_, item)| item.clone()).collect();
        affected.sort();
        affected.dedup();
        attributed.extend(affected.iter().cloned());
        let mut groups: Vec<SiteGroup> = Vec::new();
        for (label, site) in sites.get(key).into_iter().flatten() {
            match groups.iter_mut().find(|g| g.edit == *label) {
                Some(group) => group.sites.push(site.clone()),
                None => groups.push(SiteGroup { edit: label.clone(), sites: vec![site.clone()] }),
            }
        }
        groups.sort_by(|a, b| b.sites.len().cmp(&a.sites.len()));
        // The bodies of the sites that the compare proves equal (for example after the inline
        // of the changed function). The fact names them, so that the list of the affected
        // bodies agrees with the list of the sites (trial 4, D14).
        let mut equal_sites: Vec<String> = groups
            .iter()
            .flat_map(|group| group.sites.iter().map(|site| site.item.clone()))
            .filter(|item| !affected.contains(item) && matches!(body_index.status(item), Some(crate::BodyStatus::Equal { .. })))
            .collect();
        equal_sites.sort();
        equal_sites.dedup();
        let place = anchor.to_string();
        index_of.insert(key.clone(), facts.len());
        facts.push(Fact { build_code: None, build_code_change: false, reason_key: None,
            id: format!("f{}", facts.len() + 1),
            at: Some(anchor.clone()),
            file: first.file.clone(),
            part: key.1.clone(),
            description: if descriptions.is_empty() { format!("{} of `{}`", key.1, key.0) } else { descriptions.join("; ") },
            status: "derived".into(),
            // A member order is never neutral: it has effects outside the MIR records (the drop
            // order of the fields, the derive output order, the discriminant values of an enum).
            // A visibility change of the public API has no MIR: the class is `other`, as for an
            // attribute. It is a fact to read (batch 5x, round 2).
            class: if !body_index.available {
                None
            } else if !affected.is_empty() || key.1 == "member order" {
                Some(Class::Logic)
            } else if api_keys.contains(key) {
                Some(Class::Other)
            } else {
                Some(Class::Neutral)
            },
            affected,
            spotlight: spotlight_why.is_some(),
            spotlight_why,
            reason: step.reasons.get(&place).cloned(),
            sites: groups,
            content: None,
            equal_sites,
            note: None,
        });
    }
    let mut fact_of: Vec<Option<usize>> = fact_key.into_iter().map(|key| key.and_then(|key| index_of.get(&key).copied())).collect();
    // The file facts and the manifest facts (design 23).
    let unowned: Vec<usize> = free.iter().copied().filter(|&i| fact_of[i].is_none()).collect();
    let (file_facts, file_owner) = crate::files::file_facts(step, edits, &unowned, before, after);
    let offset = facts.len();
    for (index, first) in file_owner {
        fact_of[index] = Some(offset + first);
    }
    for mut fact in file_facts {
        fact.id = format!("f{}", facts.len() + 1);
        facts.push(fact);
    }
    Derived { facts, fact_of, attributed }
}

/// The fact text of changed tuple fields, with the types (D8): "the tuple fields of `E::A` changed:
/// `(u32)` → `(u64)`".
fn tuple_description(tuple: &str, before: &Workspace, after: &Workspace) -> String {
    format!("the tuple fields of `{tuple}` changed: `{}` → `{}`", tuple_fields(tuple, before), tuple_fields(tuple, after))
}

/// The types of the tuple fields of a struct or a variant: `(u32, u64)`, or `()` for none.
fn tuple_fields(tuple: &str, ws: &Workspace) -> String {
    let mut found: Vec<(usize, String)> = ws
        .items
        .values()
        .filter(|item| item.kind == ItemKind::Field)
        .filter_map(|item| {
            let index = item.path.strip_prefix(tuple)?.strip_prefix('.')?.parse::<usize>().ok()?;
            Some((index, item.signature.clone()))
        })
        .collect();
    found.sort();
    format!("({})", found.into_iter().map(|(_, ty)| ty).collect::<Vec<_>>().join(", "))
}

/// The name and the type text of a parameter text `name : Type`.
fn param_parts(text: &str) -> Option<(&str, &str)> {
    let (name, ty) = text.split_once(':')?;
    Some((name.trim(), ty.trim()))
}

/// The doc test change of a doc edit (brief 5h, B1), or `None` for a change of the text only. A
/// doc comment with a code block is a test that `cargo test` runs: it is code, not a doc text.
pub(crate) fn doc_test_change(edit: &Edit) -> Option<semilla_syntax::doc::DocTestChange> {
    if edit.kind != EditKind::Docs {
        return None;
    }
    semilla_syntax::doc::doc_test_change(edit.text_before.as_deref().unwrap_or(""), edit.text_after.as_deref().unwrap_or(""))
}

/// The fact text and the content (a diff of the full doc lines) of a doc test change.
pub(crate) fn doc_test_fact(item: &str, edit: &Edit) -> Option<(String, String)> {
    let change = doc_test_change(edit)?;
    let attributes = if change.attributes.is_empty() { String::new() } else { format!(" (block attributes: {})", change.attributes.iter().map(|a| format!("`{a}`")).collect::<Vec<_>>().join(", ")) };
    let side = |text: &Option<String>| text.clone().filter(|text| !text.is_empty());
    let (old, new) = (side(&edit.text_before), side(&edit.text_after));
    Some((format!("doc test of `{item}` {}{attributes}", change.what), crate::files::content_change(old.as_deref(), new.as_deref(), None, None)))
}

/// True if the edit is one side of a parameter rename (B5): a removed and an added parameter of
/// one function with the same type at the same position. A parameter name is a local in the MIR,
/// so the body proof covers its uses.
pub(crate) fn is_parameter_rename(edits: &[Edit], index: usize, before: &Workspace, after: &Workspace) -> bool {
    let is_param = |edit: &Edit| edit.kind == EditKind::Signature && edit.node_after.or(edit.node_before) == Some("param");
    let position = |ws: &Workspace, anchor: &Anchor| {
        let sig = ws.items.get(&anchor.item)?.sig.as_ref()?;
        let role = anchor.path.last()?.to_string();
        sig.children.iter().filter(|c| c.kind == "param").position(|c| c.role == role)
    };
    let side = |edit: &Edit| -> Option<(bool, Anchor, String, Option<usize>)> {
        match (&edit.text_before, &edit.text_after, &edit.before, &edit.after) {
            (Some(text), None, Some(anchor), _) => Some((false, anchor.clone(), param_parts(text)?.1.to_string(), position(before, anchor))),
            (None, Some(text), _, Some(anchor)) => Some((true, anchor.clone(), param_parts(text)?.1.to_string(), position(after, anchor))),
            _ => None,
        }
    };
    let edit = &edits[index];
    if !is_param(edit) {
        return false;
    }
    let Some((added, anchor, ty, at)) = side(edit) else { return false };
    at.is_some() && edits.iter().enumerate().any(|(other, e)| other != index && is_param(e) && side(e).is_some_and(|(a, o, t, p)| a != added && o.item == anchor.item && t == ty && p == at))
}

/// Merges the facts of a removed and an added parameter into one rename fact, when the function,
/// the type, and the position are equal.
fn merge_parameter_renames(edits: &[Edit], before: &Workspace, after: &Workspace, keys: &mut Vec<Key>, parts: &mut BTreeMap<Key, (Anchor, Vec<usize>, Vec<String>)>, fact_key: &mut [Option<Key>]) {
    let position = |ws: &Workspace, anchor: &Anchor| {
        let sig = ws.items.get(&anchor.item)?.sig.as_ref()?;
        let role = anchor.path.last()?.to_string();
        sig.children.iter().filter(|c| c.kind == "param").position(|c| c.role == role)
    };
    let single = |parts: &BTreeMap<Key, (Anchor, Vec<usize>, Vec<String>)>, key: &Key| parts.get(key).filter(|(_, indexes, _)| indexes.len() == 1).map(|(anchor, indexes, _)| (anchor.clone(), indexes[0]));
    let params: Vec<Key> = keys.iter().filter(|(_, part)| part == "parameter").cloned().collect();
    for removed in &params {
        let Some((old_anchor, old_index)) = single(parts, removed) else { continue };
        let old_edit = &edits[old_index];
        let (Some(old_text), None) = (&old_edit.text_before, &old_edit.text_after) else { continue };
        let Some((old_name, old_type)) = param_parts(old_text) else { continue };
        for added in &params {
            let Some((new_anchor, new_index)) = single(parts, added) else { continue };
            let new_edit = &edits[new_index];
            let (None, Some(new_text)) = (&new_edit.text_before, &new_edit.text_after) else { continue };
            let Some((new_name, new_type)) = param_parts(new_text) else { continue };
            if new_anchor.item != old_anchor.item || new_type != old_type || position(before, &old_anchor).is_none() || position(before, &old_anchor) != position(after, &new_anchor) {
                continue;
            }
            let key: Key = (old_anchor.to_string(), "parameter rename".to_string());
            let description = format!("parameter `{old_name}` of `{}` renamed to `{new_name}`: `{old_type}`", old_anchor.item);
            let at = keys.iter().position(|k| k == removed).unwrap_or(keys.len());
            keys.insert(at, key.clone());
            keys.retain(|k| k != removed && k != added);
            parts.remove(removed);
            parts.remove(added);
            parts.insert(key.clone(), (old_anchor.clone(), vec![old_index, new_index], vec![description]));
            fact_key[old_index] = Some(key.clone());
            fact_key[new_index] = Some(key);
            break;
        }
    }
}

/// True if a changed body uses the entity of a fact (F5). For a function part, the MIR callee
/// list is the evidence. For a type part, the body text names the type.
fn uses(key: &Key, item: &str, after: &Workspace, before: &Workspace, callees: Option<&BTreeMap<String, crate::ItemFacts>>) -> bool {
    let entity = key.0.split('/').next().unwrap_or(&key.0);
    // A visibility or an attribute does not explain a MIR change (the rule of 8.4): a body
    // that changes with them stays an indirect-change error.
    if matches!(key.1.as_str(), "visibility" | "attribute") {
        return false;
    }
    if item == entity {
        return true;
    }
    let function = after.items.get(entity).or_else(|| before.items.get(entity)).is_some_and(|it| it.kind == ItemKind::Fn);
    if function {
        return callees.and_then(|map| map.get(item)).is_some_and(|facts| facts.callees.iter().chain(&facts.externals).any(|c| c == entity));
    }
    // The type that the fact changes: the owner of a member, or the item itself.
    let owner = if after.items.get(entity).or_else(|| before.items.get(entity)).is_some_and(|it| matches!(it.kind, ItemKind::Field | ItemKind::Variant)) { owner_of(entity) } else { entity.to_string() };
    let name = owner.rsplit("::").next().unwrap_or(&owner);
    let name = name.split('#').next().unwrap_or(name);
    // A body of the type itself (the constructor of a variant: `shop::Error::Unknown`) uses it.
    if item.strip_prefix(owner.as_str()).is_some_and(|rest| rest.starts_with("::") || rest.starts_with('.')) {
        return true;
    }
    // A derived body of the type (`<shop::Account as Clone>::clone`) uses it (C1). For a field of
    // a variant, the type of the derived impl is the enum.
    let variant = after.items.get(&owner).or_else(|| before.items.get(&owner)).is_some_and(|it| it.kind == ItemKind::Variant);
    let enum_path = variant.then(|| owner.rsplit_once("::").map(|(e, _)| e.to_string())).flatten();
    if let Some(inner) = item.strip_prefix('<').and_then(|rest| rest.split_once(" as ")) {
        let ty = inner.0.split('<').next().unwrap_or(inner.0);
        if ty == owner || ty.rsplit("::").next() == Some(name) || enum_path.as_deref().is_some_and(|e| ty == e || ty.rsplit("::").next() == e.rsplit("::").next()) {
            return true;
        }
    }
    // The MIR text depends on the layout or the discriminant values of the type (a constant of
    // the type, a cast of its discriminant), with no name of the type in the source (batch 5e).
    let facts = callees.and_then(|map| map.get(item));
    if facts.is_some_and(|facts| facts.layouts.iter().any(|ty| *ty == owner || enum_path.as_deref() == Some(ty.as_str()))) {
        return true;
    }
    let text = after.items.get(item).or_else(|| before.items.get(item)).map(|it| it.text.clone()).unwrap_or_default();
    crate::hints::words(&text).contains(&name)
}

/// The arguments of a call text, and the callee (the path, or the receiver and the method).
fn call_parts(text: &str) -> Option<(String, String, Vec<syn::Expr>)> {
    match syn::parse_str::<syn::Expr>(text).ok()? {
        syn::Expr::Call(call) => {
            let callee = tokens(&call.func);
            let name = callee.rsplit(' ').next().unwrap_or(&callee).to_string();
            Some((callee, name, call.args.into_iter().collect()))
        }
        syn::Expr::MethodCall(call) => Some((format!("{} . {}", tokens(&call.receiver), call.method), call.method.to_string(), call.args.into_iter().collect())),
        _ => None,
    }
}

fn tokens(value: &impl quote::ToTokens) -> String {
    quote::ToTokens::to_token_stream(value).to_string()
}

/// A plain path or a literal: an argument that has no side effect.
fn plain(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Path(_) | syn::Expr::Lit(_) => true,
        syn::Expr::Reference(reference) => plain(&reference.expr),
        syn::Expr::Paren(paren) => plain(&paren.expr),
        _ => false,
    }
}

/// A call site of a parameter fact (F1): the same callee, and one argument more (or less) at the
/// position of the parameter. Returns the fact key, the label, and a removed argument with a
/// possible side effect.
fn call_site(old: &str, new: &str, keys: &[Key], parts: &BTreeMap<Key, (Anchor, Vec<usize>, Vec<String>)>, edits: &[Edit], before: &Workspace, after: &Workspace) -> Option<(Key, String, Option<String>)> {
    let (old_callee, name, old_args) = call_parts(old)?;
    let (new_callee, _, new_args) = call_parts(new)?;
    if old_callee != new_callee {
        return None;
    }
    let (added, longer, shorter) = match new_args.len().checked_sub(old_args.len()) {
        Some(1) => (true, &new_args, &old_args),
        _ if old_args.len() == new_args.len() + 1 => (false, &old_args, &new_args),
        _ => return None,
    };
    for key in keys.iter().filter(|(_, part)| part == "parameter") {
        let (anchor, indexes, _) = &parts[key];
        let edit = &edits[indexes[0]];
        if added != edit.text_before.is_none() || edit.text_before.is_some() == edit.text_after.is_some() {
            continue;
        }
        let function = &anchor.item;
        let last = function.rsplit("::").next().unwrap_or(function);
        if last.split('#').next() != Some(name.as_str()) {
            continue;
        }
        let ws = if added { after } else { before };
        let Some(sig) = ws.items.get(function).and_then(|item| item.sig.as_ref()) else { continue };
        let params: Vec<&String> = sig.children.iter().filter(|c| c.kind == "param").map(|c| &c.role).collect();
        let role = anchor.path.last().map(|s| s.to_string()).unwrap_or_default();
        let Some(mut position) = params.iter().position(|p| **p == role) else { continue };
        // A method call has the receiver outside the arguments.
        if params.first().is_some_and(|p| *p == "param[self]") && old_callee.contains(" . ") {
            position = position.checked_sub(1)?;
        }
        if position >= longer.len() {
            continue;
        }
        let mut rest = longer.clone();
        let moved = rest.remove(position);
        if rest.iter().map(tokens).ne(shorter.iter().map(tokens)) {
            continue;
        }
        let text = tokens(&moved);
        let label = if added { format!("add the argument `{text}`") } else { format!("drop the argument `{text}`") };
        let effect = (!added && !plain(&moved)).then(|| text.clone());
        return Some((key.clone(), label, effect));
    }
    None
}

/// The struct name of a member fact key (`shop::Account.created` gives `Account`), and the
/// field name.
fn member_names(key: &Key) -> Option<(String, String)> {
    let (owner, field) = key.0.rsplit_once('.')?;
    let name = owner.rsplit("::").next()?.split('#').next()?.to_string();
    Some((name, field.to_string()))
}

/// A struct literal site of a field fact (F2): the same path, and one field more (or less).
fn literal_site(old: &str, new: &str, keys: &[Key], after: &Workspace, before: &Workspace) -> Option<(Key, String, Option<String>)> {
    let (syn::Expr::Struct(old), syn::Expr::Struct(new)) = (syn::parse_str::<syn::Expr>(old).ok()?, syn::parse_str::<syn::Expr>(new).ok()?) else { return None };
    if tokens(&old.path) != tokens(&new.path) || old.rest.is_some() != new.rest.is_some() {
        return None;
    }
    let path_name = old.path.segments.last()?.ident.to_string();
    let _ = (after, before);
    for key in keys.iter().filter(|(_, part)| part == "member added" || part == "member removed") {
        let Some((owner, field)) = member_names(key) else { continue };
        if path_name != owner && path_name != "Self" {
            continue;
        }
        let added = key.1 == "member added";
        let (longer, shorter) = if added { (&new, &old) } else { (&old, &new) };
        let member = |f: &syn::FieldValue| tokens(&f.member);
        let Some(position) = longer.fields.iter().position(|f| member(f) == field) else { continue };
        if shorter.fields.iter().any(|f| member(f) == field) {
            continue;
        }
        let rest: Vec<String> = longer.fields.iter().enumerate().filter(|(i, _)| *i != position).map(|(_, f)| tokens(f)).collect();
        if rest != shorter.fields.iter().map(tokens).collect::<Vec<_>>() {
            continue;
        }
        let value = &longer.fields[position];
        let label = if added { format!("add the field `{}`", tokens(value)) } else { format!("drop the field `{}`", tokens(value)) };
        let effect = (!added && !plain(&value.expr)).then(|| tokens(value));
        return Some((key.clone(), label, effect));
    }
    None
}

/// A struct pattern site of a field fact (F2): the pattern gets the field or `..` (a new field),
/// or drops the field (a removed field).
fn pattern_site(old: &str, new: &str, keys: &[Key]) -> Option<(Key, String)> {
    use syn::parse::Parser;
    let parse = |text: &str| syn::Pat::parse_single.parse_str(text).ok();
    let (Some(syn::Pat::Struct(old)), Some(syn::Pat::Struct(new))) = (parse(old), parse(new)) else { return None };
    if tokens(&old.path) != tokens(&new.path) {
        return None;
    }
    let path_name = old.path.segments.last()?.ident.to_string();
    let fields = |pat: &syn::PatStruct| pat.fields.iter().map(tokens).collect::<Vec<_>>();
    let names = |pat: &syn::PatStruct| pat.fields.iter().map(|f| tokens(&f.member)).collect::<Vec<_>>();
    for key in keys.iter().filter(|(_, part)| part == "member added" || part == "member removed") {
        let Some((owner, field)) = member_names(key) else { continue };
        if path_name != owner && path_name != "Self" {
            continue;
        }
        if key.1 == "member added" {
            // `..` added, and the fields are the same.
            if old.rest.is_none() && new.rest.is_some() && fields(&old) == fields(&new) {
                return Some((key.clone(), "add `..` or the field to the pattern".into()));
            }
            // The field added, and the rest is the same.
            if old.rest.is_some() == new.rest.is_some() && !names(&old).contains(&field) {
                let kept: Vec<String> = new.fields.iter().filter(|f| tokens(&f.member) != field).map(tokens).collect();
                if names(&new).contains(&field) && kept == fields(&old) {
                    return Some((key.clone(), "add `..` or the field to the pattern".into()));
                }
            }
        } else if old.rest.is_some() == new.rest.is_some() && names(&old).contains(&field) && !names(&new).contains(&field) {
            let kept: Vec<String> = old.fields.iter().filter(|f| tokens(&f.member) != field).map(tokens).collect();
            if kept == fields(&new) {
                return Some((key.clone(), "drop the field from the pattern".into()));
            }
        }
    }
    None
}

fn short(text: &Option<String>) -> String {
    text.as_deref().map(|t| semilla_syntax::tree::short(t, 60)).unwrap_or_default()
}

/// The owner of a member: `shop::Account` for `shop::Account.balance`, `shop::Event` for
/// `shop::Event::Open`.
fn owner_of(path: &str) -> String {
    match path.rsplit_once('.') {
        Some((owner, field)) if !field.contains("::") => owner.to_string(),
        _ => path.rsplit_once("::").map(|(owner, _)| owner.to_string()).unwrap_or_default(),
    }
}

/// The owner of a tuple field (`shop::Pair.1` gives `shop::Pair`): a position without a name.
fn tuple_owner(path: &str, kind: ItemKind) -> Option<String> {
    if kind != ItemKind::Field {
        return None;
    }
    let (owner, field) = path.rsplit_once('.')?;
    field.chars().all(|c| c.is_ascii_digit()).then(|| owner.to_string())
}

/// The fact of a member list: the order of the members, or a part of a member fact (a new or a
/// removed member changes the list). A tuple owner has one coarse fact.
fn members_fact(edit: &Edit, anchor: &Anchor, item: Option<&semilla_syntax::Item>, keys: &[Key]) -> Option<(Key, String)> {
    let kind = edit.attr_after.as_deref().or(edit.attr_before.as_deref()).unwrap_or("");
    if !matches!(kind, "struct" | "enum" | "variant") {
        // The member list of a trait or an impl repeats the methods, which are items with claims.
        return None;
    }
    let old: Vec<&str> = edit.text_before.as_deref().unwrap_or("").lines().collect();
    let new: Vec<&str> = edit.text_after.as_deref().unwrap_or("").lines().collect();
    if old.iter().chain(&new).all(|name| name.chars().all(|c| c.is_ascii_digit())) && kind != "enum" {
        return Some(((anchor.item.clone(), "tuple fields".into()), String::new()));
    }
    let separator = if kind == "enum" { "::" } else { "." };
    let kept_old: Vec<&&str> = old.iter().filter(|name| new.contains(name)).collect();
    let kept_new: Vec<&&str> = new.iter().filter(|name| old.contains(name)).collect();
    if kept_old != kept_new {
        let _ = item;
        // The discriminant of a variant with no explicit value is its position.
        let values = if kind == "enum" { "; the discriminant values of the variants with no explicit value change" } else { "" };
        return Some(((anchor.item.clone(), "member order".into()), format!("member order of `{}`: `{}` → `{}`{values}", anchor.item, old.join(", "), new.join(", "))));
    }
    // The list changed only by new or removed members: the edit belongs to their facts.
    let changed = new.iter().filter(|name| !old.contains(name)).map(|name| (format!("{}{separator}{name}", anchor.item), "member added")).chain(old.iter().filter(|name| !new.contains(name)).map(|name| (format!("{}{separator}{name}", anchor.item), "member removed")));
    for (member, part) in changed {
        if let Some(key) = keys.iter().find(|(entity, p)| *entity == member && p == part) {
            return Some((key.clone(), String::new()));
        }
    }
    None
}

/// The part of a signature edit that is a fact, or `None` for a change that needs a claim (a
/// const or static value, a `macro_rules!` body, a changed field kind of a struct or a variant).
pub fn signature_part(edit: &Edit) -> Option<&'static str> {
    if edit.kind != EditKind::Signature {
        return None;
    }
    if let Some(part) = edit.node_after.or(edit.node_before) {
        return Some(match part {
            "param" => "parameter",
            "params" => "parameter order",
            "ret" => "return type",
            "generics" => "generics",
            "where" => "where clause",
            "qualifiers" => "qualifiers",
            _ => return None,
        });
    }
    let kind = edit.attr_after.as_deref().or(edit.attr_before.as_deref())?;
    match kind {
        "field" | "type" => Some("type"),
        "const" | "static" => {
            // The type, the value, or both (the value part of design 22.3, A6). The class of a value
            // fact comes from the MIR of the bodies that use the const.
            match (const_parts(&edit.text_before), const_parts(&edit.text_after)) {
                (Some((old_head, old_value)), Some((new_head, new_value))) => match (old_head == new_head, old_value == new_value) {
                    (false, true) => Some("type"),
                    (true, false) => Some("value"),
                    (false, false) => Some("type and value"),
                    (true, true) => None,
                },
                _ => None,
            }
        }
        "variant" => {
            // Only the discriminant is a fact. A changed field kind is a change of the item kind.
            let head = |text: &Option<String>| text.as_deref().map(|t| t.split(" = ").next().unwrap_or(t).to_string());
            (head(&edit.text_before) == head(&edit.text_after)).then_some("discriminant")
        }
        "struct" => {
            // The generics and the bounds. A changed field kind (named, tuple, unit) is not a fact.
            let delimiter = |text: &Option<String>| text.as_deref().map(|t| (t.ends_with("{ }"), t.ends_with(';')));
            (delimiter(&edit.text_before) == delimiter(&edit.text_after)).then_some("generics")
        }
        "enum" | "trait" | "impl" => Some("generics"),
        _ => None,
    }
}

/// The head (`const X : T`) and the value of a const or static signature text.
fn const_parts(text: &Option<String>) -> Option<(String, String)> {
    let (head, value) = text.as_deref()?.split_once(" = ")?;
    Some((head.to_string(), value.trim_end_matches(';').trim().to_string()))
}

/// True if an edit without a claim is (a part of) a fact. The hints and the claim suggestions skip
/// these edits.
pub fn is_fact_edit(edit: &Edit) -> bool {
    let kind = edit.attr_after.as_deref().or(edit.attr_before.as_deref()).unwrap_or("");
    match edit.kind {
        EditKind::Visibility | EditKind::Attrs => true,
        EditKind::Signature => signature_part(edit).is_some(),
        EditKind::Members => matches!(kind, "struct" | "enum" | "variant"),
        EditKind::ItemAdded | EditKind::ItemRemoved => matches!(kind, "field" | "variant"),
        EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged => true,
        _ => false,
    }
}

fn parameter_description(anchor: &Anchor, part: &str, edit: &Edit) -> String {
    let what = match (&edit.text_before, &edit.text_after) {
        (None, Some(new)) => format!("added: `{new}`"),
        (Some(old), None) => format!("removed: `{old}`"),
        _ => format!("`{}` → `{}`", short(&edit.text_before), short(&edit.text_after)),
    };
    format!("{part} of `{}` {what}", anchor.item)
}

/// The description of an attribute fact. A derive fact names the traits.
fn attribute_description(item: &str, edit: &Edit) -> String {
    let derives = |text: &Option<String>| -> Vec<String> {
        text.as_deref()
            .unwrap_or("")
            .lines()
            .filter(|line| line.contains("derive"))
            .flat_map(|line| line.split(['(', ')', ',']).skip(1).map(|t| t.trim().to_string()).filter(|t| !t.is_empty() && !t.starts_with(']')).collect::<Vec<_>>())
            .collect()
    };
    let (old, new) = (derives(&edit.text_before), derives(&edit.text_after));
    let added: Vec<&String> = new.iter().filter(|d| !old.contains(d)).collect();
    let removed: Vec<&String> = old.iter().filter(|d| !new.contains(d)).collect();
    let mut parts = Vec::new();
    if !added.is_empty() {
        parts.push(format!("derive added: {}", added.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }
    if !removed.is_empty() {
        parts.push(format!("derive removed: {}", removed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }
    if parts.is_empty() {
        format!("attributes of `{item}`: `{}` → `{}`", short(&edit.text_before), short(&edit.text_after))
    } else {
        format!("attributes of `{item}`: {}", parts.join("; "))
    }
}

/// For a new trait impl `module::impl <Type as Trait>`: the type and the trait, if the type
/// derived the trait before the step and does not derive it after the step.
fn replaced_derive(impl_path: &str, before: &Workspace, after: &Workspace) -> Option<(String, String)> {
    let (module, inner) = impl_path.split_once("::impl ")?;
    let (ty, trait_path) = inner.strip_prefix('<')?.strip_suffix('>')?.split_once(" as ")?;
    let ty = ty.split('<').next().unwrap_or(ty).trim();
    let trait_name = trait_path.rsplit("::").next().unwrap_or(trait_path).split('<').next().unwrap_or(trait_path).trim();
    let path = format!("{module}::{}", ty.rsplit("::").next().unwrap_or(ty));
    let derives = |ws: &Workspace| ws.items.get(&path).is_some_and(|item| item.attrs.iter().any(|attr| attr.contains("derive") && crate::hints::words(attr).contains(&trait_name)));
    (derives(before) && !derives(after)).then(|| (path.clone(), trait_name.to_string()))
}

/// The attributes that cannot change a MIR body or the behavior: docs, lint levels, and hints for
/// the compiler.
const NO_MIR: &[&str] = &["doc", "allow", "warn", "deny", "forbid", "expect", "must_use", "inline", "cold", "rustfmt::skip", "deprecated"];

/// The attributes whose change needs no reason. A derive can change bodies, but the MIR compare
/// shows these bodies, and the fact owns them (it is not neutral then).
const NO_SPOTLIGHT: &[&str] = &["doc", "allow", "warn", "deny", "forbid", "expect", "must_use", "inline", "cold", "rustfmt::skip", "deprecated", "derive"];

/// True for an attribute of a lint tool (`clippy::…`, `diagnostic::…`, `rustfmt::…`).
pub(crate) fn is_lint_tool(name: &str) -> bool {
    ["clippy::", "diagnostic::", "rustfmt::"].iter().any(|prefix| name.starts_with(prefix))
}

/// The name of one attribute text, for example `cfg` for `# [cfg (test)]` and `rustfmt::skip`
/// for `#[rustfmt::skip]`.
fn attribute_name(text: &str) -> Option<String> {
    let inner = text.trim().strip_prefix('#')?.trim_start().strip_prefix('!').unwrap_or_else(|| text.trim().strip_prefix('#').unwrap()).trim_start();
    let inner = inner.strip_prefix('[')?.trim_start();
    let end = inner.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':' || c == ' ')).unwrap_or(inner.len());
    let name: String = inner[..end].split_whitespace().collect();
    let name = name.trim_start_matches("::").to_string();
    (!name.is_empty()).then_some(name)
}

/// Splits an attribute text into the attributes, with no white space. One line can hold more than
/// one attribute: `# [test] # [ignore]`.
fn split_attributes(text: &str) -> Vec<String> {
    let compact: String = text.split_whitespace().collect();
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, None);
    let chars: Vec<char> = compact.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if depth == 0 && c == '#' {
            start.get_or_insert(index);
        } else if c == '[' {
            depth += 1;
        } else if c == ']' {
            depth -= 1;
            if depth == 0 {
                if let Some(from) = start.take() {
                    out.push(chars[from..=index].iter().collect());
                }
            }
        }
        index += 1;
    }
    out
}

/// True if a signature edit changes the order of the generic parameters of an item (brief 5h,
/// B4). Generic arguments go by position. So a site with explicit arguments (`tier::<10, 2>`,
/// `Pair<u8, u16>`) keeps its text and its MIR text, and it changes its meaning.
///
/// - A function, a type, a trait, or a type alias: the parameters that the two sides have are in
///   another order.
/// - An impl: the self type or the trait of the header changed. The header gives the position of
///   each parameter of the impl in the arguments of the type.
/// - A text that does not parse: the words that the two sides have are in another order (the
///   conservative answer).
pub fn generic_order_changed(edit: &Edit) -> bool {
    if edit.kind != EditKind::Signature {
        return false;
    }
    let (Some(old), Some(new)) = (edit.text_before.as_deref(), edit.text_after.as_deref()) else { return false };
    let names = |generics: &syn::Generics| -> Vec<String> {
        generics
            .params
            .iter()
            .map(|param| match param {
                syn::GenericParam::Lifetime(lifetime) => format!("'{}", lifetime.lifetime.ident),
                syn::GenericParam::Type(ty) => ty.ident.to_string(),
                syn::GenericParam::Const(constant) => constant.ident.to_string(),
            })
            .collect()
    };
    // The common entries of two lists are in another order.
    fn reordered<T: PartialEq + Clone>(old: &[T], new: &[T]) -> bool {
        let common = |list: &[T], other: &[T]| -> Vec<T> { list.iter().filter(|name| other.contains(name)).cloned().collect() };
        common(old, new) != common(new, old)
    }
    let by_words = || reordered(&crate::hints::words(old), &crate::hints::words(new));
    match edit.node_after.or(edit.node_before) {
        Some("generics") => match (syn::parse_str::<syn::Generics>(old), syn::parse_str::<syn::Generics>(new)) {
            (Ok(old), Ok(new)) => reordered(&names(&old), &names(&new)),
            _ => by_words(),
        },
        Some(_) => false,
        None => {
            let kind = edit.attr_after.as_deref().or(edit.attr_before.as_deref()).unwrap_or("");
            if !matches!(kind, "struct" | "enum" | "union" | "trait" | "impl" | "type") {
                return false;
            }
            let generics = |item: &syn::Item| match item {
                syn::Item::Struct(item) => Some(names(&item.generics)),
                syn::Item::Enum(item) => Some(names(&item.generics)),
                syn::Item::Union(item) => Some(names(&item.generics)),
                syn::Item::Trait(item) => Some(names(&item.generics)),
                syn::Item::Type(item) => Some(names(&item.generics)),
                _ => None,
            };
            match (syn::parse_str::<syn::Item>(old), syn::parse_str::<syn::Item>(new)) {
                (Ok(syn::Item::Impl(old)), Ok(syn::Item::Impl(new))) => tokens(&old.self_ty) != tokens(&new.self_ty) || old.trait_.as_ref().map(|(_, path, _)| tokens(path)) != new.trait_.as_ref().map(|(_, path, _)| tokens(path)),
                (Ok(old_item), Ok(new_item)) => match (generics(&old_item), generics(&new_item)) {
                    (Some(old), Some(new)) => reordered(&old, &new),
                    _ => by_words(),
                },
                _ => by_words(),
            }
        }
    }
}

/// The sites of an item whose generic arguments go by position (brief 5h, B4): each item of the
/// "after" tree that names the item in its code, with the count of the names. A `use … as` alias
/// of the name is also a name. For a function, the MIR callee lists give more bodies (a call from a macro).
/// For an impl, the name is the name of its self type, and the MIR callee lists give the bodies
/// that call a method of the impl.
pub fn positional_sites(entity: &str, after: &Workspace, callees: Option<&BTreeMap<String, crate::ItemFacts>>) -> Vec<(String, usize)> {
    let Some(item) = after.items.get(entity) else { return Vec::new() };
    let last = |path: &str| -> String {
        let name = path.rsplit("::").next().unwrap_or(path);
        name.split('#').next().unwrap_or(name).to_string()
    };
    let mut targets: Vec<String> = vec![entity.to_string()];
    let name = if item.kind == ItemKind::Impl {
        // The methods of the impl: the function items in its lines.
        targets = after.items.values().filter(|other| other.kind == ItemKind::Fn && other.file == item.file && other.line_start >= item.line_start && other.line_end <= item.line_end).map(|other| other.path.clone()).collect();
        match syn::parse_str::<syn::ItemImpl>(&item.signature).ok().map(|parsed| *parsed.self_ty) {
            Some(syn::Type::Path(path)) => path.path.segments.last().map(|segment| segment.ident.to_string()).unwrap_or_default(),
            _ => String::new(),
        }
    } else {
        last(entity)
    };
    let mut names = vec![name.clone()];
    for import in after.items.values().filter(|other| other.kind == ItemKind::Use) {
        let words = crate::hints::words(&import.text);
        for (index, word) in words.iter().enumerate() {
            if *word == name && words.get(index + 1) == Some(&"as") {
                if let Some(alias) = words.get(index + 2).filter(|alias| !names.iter().any(|known| known == *alias)) {
                    names.push(alias.to_string());
                }
            }
        }
    }
    let mut sites: BTreeMap<String, usize> = BTreeMap::new();
    for (path, other) in &after.items {
        if matches!(other.kind, ItemKind::Mod | ItemKind::Use) || path == entity || name.is_empty() {
            continue;
        }
        // A type, a trait, and an impl have member items: only the header counts for the owner.
        let text = if matches!(other.kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Trait | ItemKind::Impl) { &other.signature } else { &other.text };
        // D10 of batch 5k: only a name in the code is a site. The word in a comment or in a
        // literal is not a site (a comment and a literal are one token each).
        let mut count = semilla_syntax::tree::display_tokens(text).iter().filter(|(_, token)| names.iter().any(|name| name == token)).count();
        // The name in the declaration of an item with the same name is not a site.
        if last(path) == name {
            count = count.saturating_sub(1);
        }
        if count > 0 {
            sites.insert(path.clone(), count);
        }
    }
    for (body, facts) in callees.into_iter().flatten() {
        if body != entity && !targets.contains(body) && facts.callees.iter().chain(&facts.externals).any(|callee| targets.contains(callee)) {
            sites.entry(body.clone()).or_insert(1);
        }
    }
    sites.into_iter().collect()
}

/// The attributes of a new item that need a reason (brief 5h, B2): the names, in source order.
///
/// A new item follows the allow list of a changed item (`NO_SPOTLIGHT` and the lint tools). Two
/// more attributes are allowed on a new item: `#[test]` and exactly `#[cfg(test)]`. They only
/// add test code. Each other attribute needs a reason, also an attribute that Semilla does not
/// know (invariant I3).
pub fn new_item_attributes(item: &semilla_syntax::Item) -> Vec<String> {
    let mut names = Vec::new();
    for attribute in item.attrs.iter().flat_map(|text| split_attributes(text)) {
        if attribute == "#[test]" || attribute == "#[cfg(test)]" {
            continue;
        }
        // An attribute that does not parse is unknown, so it is not harmless.
        let name = attribute_name(&attribute).unwrap_or_else(|| "unknown".into());
        if !NO_SPOTLIGHT.contains(&name.as_str()) && !is_lint_tool(&name) && !names.contains(&name) {
            names.push(name);
        }
    }
    // An attribute text that the split does not read is unknown.
    if names.is_empty() && item.attrs.iter().any(|text| split_attributes(text).is_empty() && !text.trim().is_empty()) {
        names.push("unknown".into());
    }
    names
}

/// The names of the attributes that an attribute edit adds, removes, or changes.
pub fn changed_attributes(edit: &Edit) -> Vec<String> {
    let lines = |text: &Option<String>| -> Vec<String> { split_attributes(text.as_deref().unwrap_or("")) };
    let (old, new) = (lines(&edit.text_before), lines(&edit.text_after));
    let mut names = Vec::new();
    for line in old.iter().filter(|line| !new.contains(line)).chain(new.iter().filter(|line| !old.contains(line))) {
        // An attribute that does not parse is unknown, so it is not harmless.
        let name = attribute_name(line).unwrap_or_else(|| "unknown".into());
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// True if an attribute edit cannot change a MIR body: each changed attribute is a doc, a lint
/// level, or a compiler hint.
pub fn harmless_attribute_edit(edit: &Edit) -> bool {
    edit.kind == EditKind::Attrs && changed_attributes(edit).iter().all(|name| NO_MIR.contains(&name.as_str()) || is_lint_tool(name))
}

/// True if a fact with no changed body can be neutral: its kind cannot change the behavior
/// outside the MIR (invariant I3). A trait impl, an attribute outside the allow list, a manifest
/// key, a file, and an indirect change can.
pub fn neutral_kind(fact: &Fact) -> bool {
    if fact.description.ends_with(": format only") {
        return true;
    }
    match fact.part.as_str() {
        "trait impl added" | "trait impl removed" | "indirect change" | "source file" | "member order" => false,
        // C1: a doc attribute has no MIR: its class is `other`.
        "attribute" => !fact.spotlight && !fact.description.contains("derive") && !fact.description.contains("doc"),
        _ => fact.file.as_deref().is_none_or(|file| file.ends_with(".rs")),
    }
}

/// The spotlight of a fact in a function with no body record.
pub(crate) const INACTIVE: &str = "a fact in inactive code (no checked configuration compiles it)";

/// The spotlight text of the build configuration (batch 5k, A1).
pub(crate) const CONFIG_SPOTLIGHT: &str = "build configuration: this code depends on a build condition that Semilla did not check. A proof holds only in the checked configurations.";

/// The spotlight of a fact whose changed bodies read the build configuration, or `None`. The
/// uses come from the MIR records (the expansion data of the compiler), not from the text.
/// The spotlight text of a changed body with a macro or an attribute that Semilla cannot see
/// into: the proof of the body holds only for the check configuration.
pub(crate) const CONFIG_UNSEEN: &str = "build configuration: Semilla cannot see if this code depends on a build condition that it did not check. A proof holds only in the checked configurations.";

/// The note of a fact whose changed bodies have a proof in only some of the checked
/// configurations (batch 5u, part A), or `None`. The note is not a spotlight: the fact needs no
/// reason. It cannot make a fact neutral: the status of such a body is not `Equal`.
///
/// "Proved" is the intersection: a configuration is in the list only if each body of the fact
/// that is not equal has a proof in it. "Not proved" is the union.
pub(crate) fn config_note<'a>(items: impl Iterator<Item = &'a String>, body_index: &BodyIndex) -> Option<String> {
    let mut proved: Option<std::collections::BTreeSet<String>> = None;
    let mut not_proved: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for item in items {
        let Some(equal) = body_index.equal_in(item) else { continue };
        proved = Some(match proved {
            Some(old) => old.intersection(equal).cloned().collect(),
            None => equal.clone(),
        });
        not_proved.extend(body_index.differs_in(item));
    }
    let proved = proved.filter(|proved| !proved.is_empty())?;
    if not_proved.is_empty() {
        return None;
    }
    let list = |names: &std::collections::BTreeSet<String>| names.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ");
    Some(format!("{PROVED_IN} {}; not proved in {}", list(&proved), list(&not_proved)))
}

/// The note of a fact with a body whose proof Semilla removed because of a build condition
/// (batch 5u), or `None`. The caller gives the note only to a fact with no spotlight of the
/// build configuration: the reader sees why an honest rewrite has no proof. The note changes
/// no class and no decision.
pub(crate) fn stop_note<'a>(items: impl Iterator<Item = &'a String>, body_index: &BodyIndex) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for item in items {
        let causes = body_index.stopped(item);
        if causes.is_empty() {
            continue;
        }
        // The forms of the entries are the forms of `config_spotlight`.
        let show = |text: &String| match text.strip_suffix(crate::srcconfig::UNSEEN) {
            Some(call) => format!("`{call}` (Semilla cannot see into it)"),
            None if text.starts_with('`') || text.starts_with("static `") || text.starts_with("code that") => text.clone(),
            None => format!("`{text}`"),
        };
        let uses: Vec<String> = causes.iter().take(3).map(show).collect();
        let more = if causes.len() > 3 { format!(" and {} more", causes.len() - 3) } else { String::new() };
        let noun = if causes.len() == 1 { "that condition" } else { "these conditions" };
        let part = format!("{NO_PROOF} `{item}` uses {}{more}, and Semilla did not check {noun}", uses.join(", "));
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
    if parts.is_empty() { None } else { Some(parts.join("; ")) }
}

/// The start of the note of [`stop_note`].
pub(crate) const NO_PROOF: &str = "no proof:";

/// The start of the note of [`config_note`].
pub(crate) const PROVED_IN: &str = "proved neutral in";

/// The spotlight text of a name with characters outside ASCII (batch 5k, D7).
pub(crate) const NON_ASCII: &str = "an identifier with characters outside ASCII";

/// The spotlight text of a changed literal with a new character outside ASCII (batch 5w, part B).
pub(crate) const NON_ASCII_LITERAL: &str = "a changed literal has a new character outside ASCII";

/// The start of the note for a new literal with a character outside ASCII (batch 5w, part B).
pub(crate) const NON_ASCII_NOTE: &str = "new literals with characters outside ASCII:";

/// A part of a text with code: a literal, or a name with a character outside ASCII.
pub(crate) struct Mark {
    pub range: std::ops::Range<usize>,
    pub text: String,
    /// True for a string, a raw string, a byte string, or a char literal.
    pub literal: bool,
}

/// The literals of a text with code, and its identifiers with a character outside ASCII
/// (batch 5k, D7, and batch 5w, part B). A literal is a string, a raw string, a byte string, or
/// a char. The text of a literal and of a line comment is not a name. A text that is not clear
/// counts as a name: the reviewer then reads it.
pub(crate) fn marks(text: &str) -> Vec<Mark> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let at = |index: usize| chars.get(index).map(|(_, c)| *c);
    let offset = |index: usize| chars.get(index).map(|(offset, _)| *offset).unwrap_or(text.len());
    let mut found = Vec::new();
    let mut literal = |from: usize, to: usize| found.push(Mark { range: offset(from)..offset(to), text: text[offset(from)..offset(to)].to_string(), literal: true });
    // The index after the end of a string that starts after the quote mark at `from - 1`.
    let string_end = |from: usize| {
        let mut index = from;
        while let Some(c) = at(index) {
            index += 1;
            match c {
                '\\' => index += 1,
                '"' => break,
                _ => {}
            }
        }
        index.min(chars.len())
    };
    let mut names = Vec::new();
    let mut index = 0;
    while let Some(c) = at(index) {
        if c == '"' {
            let end = string_end(index + 1);
            literal(index, end);
            index = end;
        } else if c == '\'' {
            // A char literal: `'x'` or `'\…'`. A lifetime and an apostrophe have no end mark.
            let end = match (at(index + 1), at(index + 2)) {
                (Some('\\'), _) => (index + 3..index + 13).find(|end| at(*end) == Some('\'')).map(|end| end + 1),
                (Some(inner), Some('\'')) if inner != '\'' => Some(index + 3),
                _ => None,
            };
            match end {
                Some(end) => {
                    literal(index, end);
                    index = end;
                }
                None => index += 1,
            }
        } else if c == '/' && at(index + 1) == Some('/') {
            while at(index).is_some_and(|c| c != '\n') {
                index += 1;
            }
        } else if c.is_alphanumeric() || c == '_' {
            let mut end = index + 1;
            while at(end).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                end += 1;
            }
            let word = &text[offset(index)..offset(end)];
            // A raw string: `r"…"`, `r#"…"#`, also with the prefix `b` or `c`.
            let hashes = (end..).take_while(|at_hash| at(*at_hash) == Some('#')).count();
            if matches!(word, "r" | "br" | "cr") && at(end + hashes) == Some('"') {
                let close: String = std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
                let from = offset(end + hashes + 1);
                let to = text[from..].find(&close).map(|found| from + found + close.len()).unwrap_or(text.len());
                let stop = chars.partition_point(|(offset, _)| *offset < to);
                literal(index, stop);
                index = stop;
                continue;
            }
            if !word.is_ascii() {
                names.push(Mark { range: offset(index)..offset(end), text: word.to_string(), literal: false });
            }
            index = end;
        } else {
            index += 1;
        }
    }
    found.extend(names);
    found.sort_by_key(|mark| mark.range.start);
    found
}

/// The identifiers with a character outside ASCII in a text with code (batch 5k, D7), with the
/// byte range of each one. Such a name can look the same as a different name (`charge` and
/// `сharge` with a Cyrillic first letter). The text of a literal is not a name (see [`marks`]).
pub(crate) fn non_ascii_names(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    marks(text).into_iter().filter(|mark| !mark.literal).map(|mark| (mark.range, mark.text)).collect()
}

/// The text with the escape form after each name and each literal with characters outside
/// ASCII: `сharge` becomes `сharge` (`\u{441}harge`), and `"μs"` becomes `"μs"` (`"\u{3bc}s"`).
/// The form comes after the code span of the name or the literal.
pub(crate) fn with_ascii_forms(text: &str) -> String {
    let mut out = text.to_string();
    for mark in marks(text).into_iter().rev() {
        let Some(form) = semilla_core::text::ascii_form(&mark.text) else { continue };
        // Each output shows a hidden character as its escape. A second form adds nothing.
        if mark.text.chars().all(|c| c.is_ascii() || semilla_core::text::is_hidden(c)) {
            continue;
        }
        // Inside a code span: after the backtick that closes the span.
        let inside = text[..mark.range.start].matches('`').count() % 2 == 1;
        let at = match text[mark.range.end..].find('`').filter(|_| inside) {
            Some(close) => mark.range.end + close + 1,
            None => mark.range.end,
        };
        out.insert_str(at, &format!(" (`{form}`)"));
    }
    out
}

/// A literal of a new text that is not in the old text, with a character outside ASCII.
#[derive(Debug, PartialEq)]
pub(crate) struct NewLiteral {
    pub text: String,
    /// The characters outside ASCII that the old literal does not have at that place. Empty
    /// if the old text has no literal for this one (a new literal in new code).
    pub chars: Vec<char>,
}

/// The literals of `new` with a character outside ASCII that `old` does not have (batch 5w,
/// part B). A literal of `new` that replaces a literal of `old` gets the characters outside
/// ASCII that the old literal does not have at that place: a letter that looks the same as the
/// old letter (`µ` U+00B5 and `μ` U+03BC) is such a character.
///
/// The old literal of a new literal is the literal with the longest common sequence of
/// characters, from the literals of `old` that `new` does not have.
pub(crate) fn new_literals(old: &str, new: &str) -> Vec<NewLiteral> {
    let literals = |text: &str| -> Vec<String> { marks(text).into_iter().filter(|mark| mark.literal).map(|mark| mark.text).collect() };
    let (mut old, new) = (literals(old), literals(new));
    // The literals that are equal on the two sides are not a change.
    let mut added = Vec::new();
    for literal in new {
        match old.iter().position(|other| *other == literal) {
            Some(index) => {
                old.remove(index);
            }
            None => added.push(literal),
        }
    }
    // The characters of `new` outside ASCII that are not in the longest common sequence.
    let extra = |old: &str, new: &str| -> (usize, Vec<char>) {
        let (a, b): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
        if a.len().saturating_mul(b.len()) > 4_000_000 {
            return (0, b.into_iter().filter(|c| !c.is_ascii()).collect());
        }
        let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                table[i][j] = if a[i] == b[j] { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
            }
        }
        let (mut i, mut j, mut found) = (0, 0, Vec::new());
        while j < b.len() {
            if i < a.len() && a[i] == b[j] {
                i += 1;
                j += 1;
            } else if i < a.len() && table[i + 1][j] >= table[i][j + 1] {
                i += 1;
            } else {
                if !b[j].is_ascii() {
                    found.push(b[j]);
                }
                j += 1;
            }
        }
        (table[0][0], found)
    };
    let mut found = Vec::new();
    for literal in added.into_iter().filter(|literal| !literal.is_ascii()) {
        // The best old literal. With two equal sequences, the one that gives more characters.
        let chars = old.iter().map(|other| extra(other, &literal)).max_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len()))).map(|(_, chars)| chars).unwrap_or_default();
        let mut unique = Vec::new();
        for c in chars {
            if !unique.contains(&c) {
                unique.push(c);
            }
        }
        if !found.iter().any(|other: &NewLiteral| other.text == literal) {
            found.push(NewLiteral { text: literal, chars: unique });
        }
    }
    found
}

/// The two sides of a text with a change (`OLD → NEW`): the text before the first arrow that
/// is not in a literal, and the text after it.
pub(crate) fn change_sides(text: &str) -> Option<(&str, &str)> {
    let literals: Vec<std::ops::Range<usize>> = marks(text).into_iter().filter(|mark| mark.literal).map(|mark| mark.range).collect();
    let at = text.match_indices(" → ").map(|(at, _)| at).find(|at| !literals.iter().any(|range| range.contains(at)))?;
    Some((&text[..at], &text[at + " → ".len()..]))
}

///
/// `carried` has the items whose `include!` file has a fact with a spotlight in this step
/// (batch 5w, D3). That fact asks for the reason about the code that Semilla does not parse,
/// so the `include!` call of such an item is not a second cause here.
pub(crate) fn config_spotlight<'a>(items: impl Iterator<Item = &'a String>, body_index: &BodyIndex, carried: &std::collections::BTreeSet<String>) -> Option<String> {
    let mut uses: Vec<String> = Vec::new();
    let mut seen = false;
    let mut gaps: Vec<String> = Vec::new();
    for item in items {
        for call in body_index.config(item) {
            if carried.contains(item) && call.strip_suffix(crate::srcconfig::UNSEEN) == Some("include!") {
                continue;
            }
            // The state that no checked unit has (batch 5t, part B).
            if call.starts_with(crate::joint::GAP) {
                if !gaps.contains(&call) {
                    gaps.push(call);
                }
                continue;
            }
            // A macro or an attribute of another crate: Semilla does not see its content.
            let text = match call.strip_suffix(crate::srcconfig::UNSEEN) {
                Some(call) => format!("`{call}` in `{item}` (Semilla cannot see into it)"),
                None => {
                    seen = true;
                    // An entry that names an item has its own code marks (batch 5l).
                    let named = call.starts_with('`') || call.starts_with("static `");
                    if named { format!("{call} in `{item}`") } else { format!("`{call}` in `{item}`") }
                }
            };
            if !uses.contains(&text) {
                uses.push(text);
            }
        }
    }
    if uses.is_empty() && gaps.is_empty() {
        return None;
    }
    let more = if uses.len() > 3 { format!(" and {} more", uses.len() - 3) } else { String::new() };
    uses.truncate(3);
    let start = if seen || uses.is_empty() { CONFIG_SPOTLIGHT } else { CONFIG_UNSEEN };
    let uses = if uses.is_empty() { String::new() } else { format!(" Uses: {}{more}", uses.join(", ")) };
    // One gap is the cause. The fix of the first gap shows the next gap, if there is one.
    let gap = match gaps.first() {
        Some(gap) if uses.is_empty() => format!(" Not{}", gap.strip_prefix("not").unwrap_or(gap)),
        Some(gap) => format!("; {gap}"),
        None => String::new(),
    };
    Some(format!("{start}{uses}{gap}"))
}

/// Why a fact needs the reviewer's attention (design 22.5, rule 4), or `None`.
fn spotlight(part: &str, anchor: &Anchor, edits: Vec<&Edit>, before: &Workspace, after: &Workspace, body_index: &BodyIndex, callees: Option<&BTreeMap<String, crate::ItemFacts>>) -> Option<String> {
    if part == "qualifiers" {
        let has = |text: &str, word: &str| text.split(' ').any(|t| t == word);
        let edit = edits[0];
        let (old, new) = (edit.text_before.clone().unwrap_or_default(), edit.text_after.clone().unwrap_or_default());
        if ["unsafe", "extern"].iter().any(|word| has(&old, word) != has(&new, word)) {
            return Some("a changed `unsafe` or `extern` qualifier".into());
        }
    }
    if part == "member order" {
        let item = after.items.get(&anchor.item).or_else(|| before.items.get(&anchor.item));
        if item.is_some_and(|item| item.attrs.iter().any(|attr| attr.contains("repr") || attr.contains("PartialOrd") || attr.split(|c: char| !c.is_alphanumeric()).any(|w| w == "Ord" || w == "Hash"))) {
            return Some("an order change in a type with `#[repr]` or an order derive (`PartialOrd`, `Ord`, `Hash`)".into());
        }
        // A cast (`as`) reads the discriminant value of the enum (batch 5e). The MIR facts of the
        // "after" bodies give the casts.
        let mut readers: Vec<&String> = callees.into_iter().flatten().filter(|(_, facts)| facts.casts.contains(&anchor.item)).map(|(item, _)| item).collect();
        readers.sort();
        if let Some(first) = readers.first() {
            let more = if readers.len() > 1 { format!(" and {} more", readers.len() - 1) } else { String::new() };
            return Some(format!("an order change in an enum whose discriminant value a cast (`as`) reads (in `{first}`{more})"));
        }
    }
    if part == "trait impl added" {
        if let Some((ty, trait_name)) = replaced_derive(&anchor.item, before, after) {
            return Some(format!("a hand-written impl of `{trait_name}` replaces the derive on `{ty}`: the behavior can differ"));
        }
    }
    // The compiler calls these traits with no call in the text (probe 7 of batch 5a: a new `Drop`
    // runs at each scope end of the type, and the MIR of the callers does not change).
    if part.starts_with("trait impl") {
        let inner = anchor.item.split_once("::impl ").map(|(_, inner)| inner).unwrap_or("");
        let trait_name = inner.split_once(" as ").map(|(_, t)| t.trim_end_matches('>').rsplit("::").next().unwrap_or("").split('<').next().unwrap_or("")).unwrap_or("");
        if matches!(trait_name, "Drop" | "Deref" | "DerefMut") {
            return Some(format!("an implicit call: the compiler calls `{trait_name}` with no call in the text, so the MIR of the callers does not show the change"));
        }
    }
    if part == "attribute" {
        // An allow list (invariant I3): each attribute that is not known as harmless needs a
        // reason. A deny list misses the next attribute.
        let changed: Vec<String> = edits.iter().flat_map(|edit| changed_attributes(edit)).filter(|name| !NO_SPOTLIGHT.contains(&name.as_str()) && !is_lint_tool(name)).collect();
        if !changed.is_empty() {
            let mut names = changed.clone();
            names.sort();
            names.dedup();
            let cfg = names.iter().any(|name| name == "cfg" || name == "cfg_attr");
            let text = names.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ");
            return Some(if cfg { format!("a changed `cfg` attribute ({text})") } else { format!("a changed attribute that can change the behavior: {text}") });
        }
    }
    // A fact in a function that no checked configuration compiles.
    if body_index.available {
        let function = after.items.get(&anchor.item).is_some_and(|item| item.kind == ItemKind::Fn && item.body.is_some());
        if function && !body_index.has_body(&anchor.item) {
            return Some(INACTIVE.into());
        }
    }
    None
}
