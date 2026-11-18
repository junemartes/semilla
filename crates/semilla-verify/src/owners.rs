//! The owner of each changed MIR body (design 8.16, invariants I1 and I2).
//!
//! Each MIR body with a different normal form, and each body on one side only, must have an
//! owner in the report:
//! 1. an edit of its item (or of an item that contains it) gives the owner of that edit: a claim,
//!    a fact, or an uncovered entry;
//! 2. else a fact or a claim of the step is the cause by evidence: the callee list, a named const,
//!    an included file, an expanded macro, or a new method;
//! 3. else each build input fact of the step that can reach the crate of the body owns the body
//!    (a build script, a manifest key, a cargo or toolchain configuration file, a file that a
//!    crate reads at build time). One reason on the cause is sufficient;
//! 4. else a new fact "indirect change, cause not known" owns the body. It has the spotlight, so
//!    the agent must give a reason.
//!
//! The agent cannot name the affected bodies (the `affects` argument is retired). The final
//! assertion [`check`] makes sure that no path removes a changed body from the report.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{Anchor, Category, ClaimStatus, Fact, Step, StepReport};
use semilla_syntax::{Edit, EditKind, ItemKind, Workspace};

use crate::checks::BodyIndex;
use crate::{BodyComparison, BodyStatus, ItemFacts};

/// The inputs of the owner assignment.
pub struct Input<'a> {
    pub step: &'a Step,
    pub edits: &'a [Edit],
    /// The claim of each edit.
    pub owners: &'a [Option<usize>],
    /// The fact of each edit.
    pub fact_of: &'a [Option<usize>],
    pub bodies: &'a [BodyComparison],
    pub body_index: &'a BodyIndex,
    pub before: &'a Workspace,
    pub after: &'a Workspace,
    /// The MIR names of the derived bodies that a verified identity claim owns (design 22.4).
    pub identity_owned: &'a BTreeSet<String>,
    /// The MIR names of the derived bodies of the identity claims, also when a claim is not
    /// verified. The unverified claim then owns them (it rejects the step).
    pub identity_derived: &'a BTreeSet<String>,
    /// The items that a rename claim names as targets.
    pub rename_targets: &'a BTreeSet<String>,
    /// The index of the first identity claim.
    pub identity_claim: Option<usize>,
    /// Each entry of the item rename map with the identity claim that gives it: the claim, the
    /// old path, the new path (design 36.1).
    pub identity_pairs: &'a [(usize, String, String)],
    pub callees: Option<&'a BTreeMap<String, ItemFacts>>,
    /// The def path of each `#[test]` function before the step and after it (batch 6a, B2).
    /// `None`: the oracle does not know the tests.
    pub tests: Option<&'a [BTreeSet<String>; 2]>,
    /// The expanded code before the step and after it (batch 6b, part 2). `None`: not known.
    pub made: Option<&'a [crate::made::Made; 2]>,
    /// The facts of the macro calls whose text lists the made items already.
    pub made_listed: &'a BTreeSet<usize>,
    /// The statement maps of the bodies (design 41), for the MIR check of each map.
    pub maps: &'a semilla_syntax::BodyMaps,
    /// The definitions of the two sides (design 44), for the symbol links.
    pub defs: &'a [crate::DefMap; 2],
    /// The old path → the new path of each item that N1 maps.
    pub renames: &'a BTreeMap<String, String>,
}

/// The owner of one changed body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Claim(usize),
    Fact(usize),
    /// An edit with no claim and no fact: the report has an uncovered entry for it.
    Uncovered,
}

/// True if the body is the `main` function of the test harness (see
/// [`crate::is_test_harness_main`]), and it exists on one side only.
///
/// Such a body needs no owner. It shows only that a test target starts or stops to exist. The
/// change of the files or of the manifest that causes this is a fact of the step, and each test
/// of the target has its own body with the full check. The record of the harness does not hold
/// the list of the tests, so an added or a removed test does not change it.
///
/// A harness with a different text on the two sides is not in this rule: its test runner
/// changed. It goes through the usual owner rules, and it gets the spotlight with no known cause.
fn is_harness(body: &BodyComparison) -> bool {
    crate::is_test_harness_main(&body.name) && matches!(body.status, BodyStatus::BeforeOnly | BodyStatus::AfterOnly)
}

/// The changed bodies: a different normal form, or a body on one side only.
fn changed(bodies: &[BodyComparison]) -> impl Iterator<Item = &BodyComparison> {
    bodies.iter().filter(|body| !matches!(body.status, BodyStatus::Equal { .. }))
}

/// Assigns an owner to each changed body. It adds the body to the `affected` list of its fact or
/// claim, and adds the facts for the bodies with no known cause.
pub fn assign(input: &Input, report: &mut StepReport) {
    // The items whose MIR changed, with the owner, for the evidence of the named consts.
    let mut owned: BTreeMap<String, Owner> = BTreeMap::new();
    let mut pending: Vec<(&BodyComparison, String)> = Vec::new();
    // The changed bodies that are associated constants: the name, the parent item, and the text
    // of the constant (batch 5n, A2).
    let assoc: BTreeMap<String, (String, String)> = changed(input.bodies).filter(|body| input.body_index.item_of(body).is_none()).filter_map(|body| Some((body.name.clone(), assoc_const(input, body)?))).collect();
    // The map from the MIR elements that differ to the edits (design 40.3).
    // The edits of the refactoring facts (design 41.7): a move, a variable, a rename of a local.
    let refactoring: BTreeSet<usize> = (0..input.edits.len()).filter(|&edit| input.fact_of[edit].and_then(|fact| report.facts.get(fact)).is_some_and(|fact| matches!(fact.category, Category::Move | Category::Rename) && fact.part != "indirect change")).collect();
    // The changed definitions of the step and the symbol links (design 44).
    // The edits with the same meaning change no definition (design 44): a new order of the members
    // of an `impl` or a trait, a new order of bounds, source text with no other fact.
    let mut no_change = refactoring.clone();
    for (index, edit) in input.edits.iter().enumerate() {
        let fact = input.fact_of[index].and_then(|fact| report.facts.get(fact));
        let same_bounds = fact.is_some_and(|fact| [": order only", ": moved to the `where` clause", ": moved out of the `where` clause", ": moved between the parameter list and the `where` clause", ": a duplicate bound only"].iter().any(|end| fact.description.ends_with(end) || fact.description.contains(&format!("{end} ("))));
        let text_only = fact.is_some_and(|fact| fact.category == Category::Text);
        let member_order = edit.kind == EditKind::Members && [(&edit.before, input.before), (&edit.after, input.after)].iter().all(|(anchor, ws)| anchor.as_ref().and_then(|anchor| ws.items.get(&anchor.item)).is_some_and(|item| matches!(item.kind, ItemKind::Impl | ItemKind::Trait)))
            && {
                let sorted = |anchor: &Option<semilla_core::Anchor>, ws: &Workspace| -> Option<Vec<String>> {
                    let mut list = ws.items.get(&anchor.as_ref()?.item)?.members.clone();
                    list.sort();
                    Some(list)
                };
                sorted(&edit.before, input.before).is_some() && sorted(&edit.before, input.before) == sorted(&edit.after, input.after)
            };
        if same_bounds || text_only || member_order {
            no_change.insert(index);
        }
    }
    // One key for each definition: the renames of N1 and the derived moves and renames.
    let mut renames = input.renames.clone();
    for (_, old, new) in input.identity_pairs {
        renames.entry(old.clone()).or_insert_with(|| new.clone());
    }
    let links = crate::links::Links::new(input.before, input.after, input.edits, &no_change, renames, [&input.defs[0], &input.defs[1]]);
    let mut map = crate::mirmap::MirMap::with_refactoring(input.before, input.after, input.edits, &refactoring).with_links(&links);
    // The symbols of the links of each fact, with the bodies (design 44).
    let mut through_of: BTreeMap<usize, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    // The elements that each fact owns, for the branch mark (design 40.4).
    let mut branch_of: BTreeMap<usize, [Vec<crate::MirElement>; 2]> = BTreeMap::new();
    // The nodes of the elements with no owner: the body, and true if the body has an edit.
    // The nodes of the elements with no owner of each body with an edit, and the edits of the body.
    let mut unowned_of: BTreeMap<String, (BTreeSet<(usize, crate::mirmap::Mapped)>, BTreeSet<usize>)> = BTreeMap::new();
    for body in changed(input.bodies) {
        let item = input.body_index.item_of(body);
        if is_harness(body) {
            continue;
        }
        let name = item.clone().unwrap_or_else(|| body.name.clone());
        // A derived body that shows the name of a renamed type (design 35): the identity owns
        // it. The identity fact lists it, so the fact has the result `code different`.
        if input.identity_owned.contains(&body.name) {
            if let Some(claim) = input.identity_claim {
                record(report, Owner::Claim(claim), &name);
            }
            continue;
        }
        // A body that a changed macro call makes belongs to the fact of the call (batch 6b,
        // part 2), also if the step changes the macro definition that has the text of the body.
        if let Some(index) = site_call(input, body, &name).flatten().and_then(|call| call_fact(report, call)).filter(|index| input.made_listed.contains(index)) {
            record(report, Owner::Fact(index), &name);
            owned.insert(name, Owner::Fact(index));
            continue;
        }
        let text_edits = text_edits(input, body, &name);
        let mut found: Vec<Owner> = Vec::new();
        for edit in &text_edits {
            let owner = owner_of_edit(input, *edit);
            if !found.contains(&owner) {
                found.push(owner);
            }
        }
        // An associated constant has no item of its own: an edit of its text is an edit of the
        // impl or of the trait that has it.
        if let Some((parent, _)) = assoc.get(&name).filter(|_| found.is_empty()) {
            found = assoc_text_owners(input, &name, parent);
        }
        // Design 40.3: each edit gets its own compare result. An edit that owns a MIR element
        // that differs owns the body. If each element that differs has an owner, an edit that
        // owns none does not own the body (`code equal`). Else each edit keeps the rule of
        // design 38 (the doubt rule): a doubt never gives `code equal`.
        // A body with no edit of its text keeps the rules of the evidence below (a changed macro
        // needs its reason); its elements with no owner name their nodes.
        if let (BodyStatus::Different { .. }, Some(diff), false) = (&body.status, &body.elements, text_edits.is_empty()) {
            map.check_maps(&name, input.maps, diff);
            let own = map.own(diff);
            let complete = own.complete(diff);
            let owning: Vec<Owner> = own.owns.keys().map(|edit| owner_of_edit(input, *edit)).fold(Vec::new(), |mut list, owner| {
                if !list.contains(&owner) {
                    list.push(owner);
                }
                list
            });
            for (edit, elements) in &own.owns {
                if let Some(fact) = input.fact_of[*edit].filter(|_| input.owners[*edit].is_none()) {
                    let entry = branch_of.entry(fact).or_default();
                    entry[0].extend(elements[0].iter().cloned());
                    entry[1].extend(elements[1].iter().cloned());
                }
            }
            map.owning.extend(own.owns.keys().copied());
            note_through(input, &own, &name, &mut through_of);
            map.counts.linked += own.linked;
            for (cause, count) in &own.causes {
                *map.counts.causes.entry(cause.clone()).or_default() += count;
            }
            let idle: Vec<usize> = text_edits.iter().copied().filter(|edit| !own.owns.contains_key(edit)).collect();
            // An element that a refactoring took by its region (design 41.7) can be the effect
            // of a different edit: such an edit keeps the doubt rule.
            let complete = complete && !(own.by_region && idle.iter().any(|edit| !refactoring.contains(edit)));
            if complete {
                // Safety (design 44): a link can take an element that an edit of a different
                // place changes (a type, a path, a pattern, a signature, a bound, a `use` line).
                // With a link in the body, such an edit keeps the doubt rule.
                // Safety (fix round 1 of design 46): rule 3c of the normal form moves the value
                // of a binding that copies a place into its uses, so the binding has no element,
                // and a use can have the cause of a different edit (rule 1 at the use, or a
                // dependence). So an idle edit of a binding keeps the doubt rule. A rename and a
                // move keep their rules.
                let (risky, safe): (Vec<usize>, Vec<usize>) = idle.into_iter().partition(|edit| (own.linked > 0 && remote_form(&input.edits[*edit])) || (binding_form(&input.edits[*edit]) && !refactoring.contains(edit)));
                map.idle_equal.extend(safe);
                map.idle_doubt.extend(risky.iter().copied());
                let mut kept = owning;
                for edit in risky {
                    let owner = owner_of_edit(input, edit);
                    if !kept.contains(&owner) {
                        kept.push(owner);
                    }
                }
                found = kept;
            } else {
                let idle = idle.into_iter();
                map.idle_doubt.extend(idle);
                for owner in owning {
                    if !found.contains(&owner) {
                        found.push(owner);
                    }
                }
            }
            if !own.unowned.is_empty() {
                let entry = unowned_of.entry(name.clone()).or_default();
                entry.0.extend(own.unowned);
                entry.1.extend(text_edits.iter().copied());
            }
        }
        // A body with no edit of its text (design 44): the symbol links own its elements. If each
        // element that differs has a link, the body needs no other owner.
        let symbols = matches!((&body.status, &body.elements), (BodyStatus::Different { .. }, Some(diff)) if !diff.no_symbols);
        // An element at the place of the edit of a macro (a `macro_rules` body) keeps the macro
        // rules. An element at the place of a different edit (a field that a derive reads) belongs
        // to that edit.
        let is_macro = |edit: &usize| [(&input.edits[*edit].before, input.before), (&input.edits[*edit].after, input.after)].iter().any(|(anchor, ws)| anchor.as_ref().and_then(|anchor| ws.items.get(&anchor.item)).is_some_and(|item| item.kind == ItemKind::Macro));
        let diff_own = (text_edits.is_empty() && symbols && found.is_empty()).then(|| body.elements.as_ref().map(|diff| (diff, map.own(diff)))).flatten().filter(|(_, own)| !own.positional.iter().any(is_macro));
        if let Some((diff, mut own)) = diff_own {
            map.counts.linked += own.linked;
            for (cause, count) in &own.causes {
                *map.counts.causes.entry(cause.clone()).or_default() += count;
            }
            // The signature line (`other`) with a changed symbol of the function.
            let other_owned = !(diff.other || (diff.old.is_empty() && diff.new.is_empty())) || !own.other_links.is_empty();
            for (edit, symbols) in std::mem::take(&mut own.other_links) {
                own.owns.entry(edit).or_default();
                own.through.entry(edit).or_default().extend(symbols);
            }
            note_through(input, &own, &name, &mut through_of);
            let owning: Vec<Owner> = own.owns.keys().map(|edit| owner_of_edit(input, *edit)).fold(Vec::new(), |mut list, owner| {
                if !list.contains(&owner) {
                    list.push(owner);
                }
                list
            });
            for (edit, elements) in &own.owns {
                if let Some(fact) = input.fact_of[*edit].filter(|_| input.owners[*edit].is_none()) {
                    let entry = branch_of.entry(fact).or_default();
                    entry[0].extend(elements[0].iter().cloned());
                    entry[1].extend(elements[1].iter().cloned());
                }
            }
            map.owning.extend(own.owns.keys().copied());
            for owner in &owning {
                record(report, *owner, &name);
            }
            if !owning.is_empty() && own.unowned.is_empty() && own.lost == 0 && other_owned {
                owned.insert(name, owning[0]);
                continue;
            }
        }
        // A new name of a local or a parameter does not change the code. When the body has a
        // different edit fact, the body goes to that fact (design 36.1, rule 7). With no such
        // fact, the rename fact keeps the body and gets the mismatch mark.
        // Since design 44, only a fact of an edit of the text of the body counts: a link is not
        // such a fact. A type change that the mapper sees as a new name (`u32` → `u16`) must not
        // get `code equal` from a link in the same body.
        let local_renames: Vec<Owner> = found.iter().copied().filter(|owner| matches!(owner, Owner::Fact(index) if report.facts.get(*index).is_some_and(|fact| fact.category == semilla_core::Category::Rename && matches!(fact.part.as_str(), "body" | "parameter rename")))).collect();
        let text_owners: Vec<Owner> = text_edits.iter().map(|edit| owner_of_edit(input, *edit)).collect();
        let other_text = found.iter().any(|owner| text_owners.contains(owner) && !local_renames.contains(owner));
        if !local_renames.is_empty() && other_text {
            for owner in &local_renames {
                if let Owner::Fact(index) = owner {
                    report.facts[*index].affected.retain(|body| *body != name);
                }
            }
            found.retain(|owner| !local_renames.contains(owner));
        }
        if !found.is_empty() {
            for owner in &found {
                record(report, *owner, &name);
            }
            owned.insert(name, found[0]);
            continue;
        }
        // A body of a renamed or moved item with no edit fact goes to its identity claim
        // (design 36.1). The identity fact then has `code different` and the mismatch mark.
        let target = input.identity_pairs.iter().find(|(_, _, new)| name == *new || name.strip_prefix(new.as_str()).is_some_and(|rest| rest.starts_with("::"))).map(|(claim, _, _)| *claim);
        if target.is_some() || input.rename_targets.contains(&name) || input.identity_derived.contains(&body.name) {
            if let Some(claim) = target.or(input.identity_claim) {
                record(report, Owner::Claim(claim), &name);
                owned.insert(name, Owner::Claim(claim));
                continue;
            }
        }
        // F5: a fact whose entity the body uses (computed in `facts::derive`). Since design 44,
        // only for a body with no symbol data: else the links decide.
        if let Some(index) = report.facts.iter().position(|fact| fact.affected.contains(&name)).filter(|_| !symbols) {
            owned.insert(name, Owner::Fact(index));
            continue;
        }
        // A body with no text edit that has the old name or the new name of a pair in its text
        // (a string, a macro call): the identity claim owns it (design 36.1).
        if let Some(claim) = names_pair(input, &name) {
            record(report, Owner::Claim(claim), &name);
            owned.insert(name, Owner::Claim(claim));
            continue;
        }
        pending.push((body, name));
    }

    // The evidence for the bodies with no text edit. A named const or static can get its owner in
    // this loop, so the loop runs until no new owner appears.
    // The build input facts that own bodies: the fact, the count of its bodies, the count of its
    // bodies that have more than one possible cause, and the group of facts that can be the cause.
    let mut caused: BTreeMap<usize, (usize, usize, BTreeSet<usize>)> = BTreeMap::new();
    loop {
        let mut progress = false;
        let mut rest = Vec::new();
        for (body, name) in pending {
            // Batch 5n, A1: with no other evidence, a callee whose MIR signature changed in this
            // step is the cause. The fact of the callee says so.
            // Since design 44, a body with symbol data has the links: the named constants, the
            // `const fn` chain, a new callee, and a changed MIR signature are symbol links.
            let symbols = matches!(&body.elements, Some(diff) if !diff.no_symbols) && matches!(body.status, BodyStatus::Different { .. });
            let found = evidence(input, report, body, &name, &owned, &assoc, symbols).or_else(|| {
                if symbols {
                    return None;
                }
                let (owner, callee) = changed_callee(input, &name, &owned)?;
                note_signature(report, owner, &callee);
                Some(owner)
            });
            match found {
                Some(owner) => {
                    // A manifest key that owns a changed body is a build input (brief 5h, B3):
                    // it has the same spotlight as each other build input, and it needs a reason
                    // (`package.description` that `env!` reads, as `package.version`).
                    if let Owner::Fact(index) = owner {
                        if report.facts.get(index).is_some_and(|fact| fact.file.as_deref().is_some_and(crate::files::is_manifest)) {
                            let entry = caused.entry(index).or_default();
                            entry.0 += 1;
                            entry.2.insert(index);
                        }
                    }
                    record(report, owner, &name);
                    owned.insert(name, owner);
                    progress = true;
                }
                None => rest.push((body, name)),
            }
        }
        pending = rest;
        if !progress || pending.is_empty() {
            break;
        }
    }

    // The bodies that a macro call makes (batch 6a, B2).
    let pending = macro_bodies(input, report, pending, &mut owned);

    // A build input fact that can reach the crate of the body is the cause (brief 5f, B2). With
    // more than one such fact, the body goes under each of them.
    let inputs = build_inputs(input, report);
    let crates: BTreeSet<&str> = input.before.crates.iter().chain(&input.after.crates).map(|krate| krate.name.as_str()).collect();
    let mut rest = Vec::new();
    for (body, name) in pending {
        // The crate of the body: the first path segment (after the `<` of a trait impl name).
        let krate = name.trim_start_matches('<').split("::").next().filter(|first| crates.contains(first));
        let causes: Vec<usize> = inputs.iter().filter(|(_, reach)| reach.as_ref().is_none_or(|reach| krate.is_some_and(|krate| reach.contains(krate)))).map(|(index, _)| *index).collect();
        if causes.is_empty() {
            rest.push((body, name));
            continue;
        }
        for cause in &causes {
            record(report, Owner::Fact(*cause), &name);
            let entry = caused.entry(*cause).or_default();
            entry.0 += 1;
            entry.1 += usize::from(causes.len() > 1);
            entry.2.extend(causes.iter().copied());
        }
        owned.insert(name, Owner::Fact(causes[0]));
    }
    let pending = rest;
    for (index, (count, shared, group)) in caused {
        let mut why = if count == 1 {
            "a build input: the MIR of 1 body changed with no edit of its text, and this change can reach its crate".to_string()
        } else {
            format!("a build input: the MIR of {count} bodies changed with no edit of their text, and this change can reach their crates")
        };
        let mut causes: Vec<String> = Vec::new();
        if group.len() > 1 {
            let list: Vec<String> = group.iter().filter_map(|other| report.facts.get(*other)).map(|fact| format!("`{}`", fact.description)).collect();
            // D10 of batch 5k: the facts of one group can have different counts. A body with
            // one possible cause (a body that reads this key with `env!`, a body of a crate
            // that only this change can reach) is not in the count of the other facts.
            // Design 37.14: the text shows at most `CAUSE_LIMIT` causes and a cut mark with the
            // count of the others. The fact keeps the full list in `causes`.
            let limit = semilla_core::report::CAUSE_LIMIT;
            let short = if list.len() > limit { format!("{}, and {} more (`semilla status --verbose` shows each cause)", list[..limit].join(", "), list.len() - limit) } else { list.join(", ") };
            if shared == count {
                why.push_str(&format!(". The bodies have one of these causes: {short}"));
            } else {
                why.push_str(&format!(". {shared} of the bodies have one of these causes: {short}. This change is the only cause of the other {}", count - shared));
            }
            if list.len() > limit {
                causes = list;
            }
        }
        let Some(fact) = report.facts.get_mut(index) else { continue };
        fact.causes = causes;
        fact.spotlight_why = Some(match fact.spotlight_why.take() {
            Some(known) => format!("{known}; {why}"),
            None => why,
        });
        // The cause needs a reason, also when its key is on the allow list (`package.version`).
        if !fact.spotlight {
            fact.spotlight = true;
            fact.reason = input.step.reasons.get(&fact.part).cloned();
        }
    }

    // No known cause: a new fact with the spotlight. Two bodies of one item (a function and
    // its closure) give one fact (batch 6b, part 1): before, each body gave the same fact again.
    let mut unknown: BTreeMap<String, usize> = BTreeMap::new();
    // Design 40.3: the elements with no owner in a body with an edit. The edits keep the doubt
    // rule, so the fact has no spotlight: it names the places of the change.
    for (name, (nodes, _)) in &unowned_of {
        body_fact(input, report, name, nodes, false);
    }
    // The bodies with no edit and with nodes: one fact for each body, at most `CRATE_BODIES`
    // in a crate; else one fact for the crate (design 40.3).
    let mut no_edit: BTreeMap<String, Vec<(String, BTreeSet<(usize, crate::mirmap::Mapped)>)>> = BTreeMap::new();
    for (body, name) in pending {
        let nodes = match (&body.status, &body.elements) {
            (BodyStatus::Different { .. }, Some(diff)) => Some(map.own(diff).unowned).filter(|nodes| !nodes.is_empty()),
            _ => None,
        };
        if let Some(nodes) = nodes {
            let krate = name.trim_start_matches('<').split("::").next().unwrap_or(&name).to_string();
            let list = no_edit.entry(krate).or_default();
            match list.iter_mut().find(|(known, _)| *known == name) {
                Some((_, known)) => known.extend(nodes),
                None => list.push((name, nodes)),
            }
            continue;
        }
        let anchor = Anchor::item(&name);
        let place = anchor.to_string();
        // A known origin: a syntax item, or a derive of a type of the workspace.
        let known = input.after.items.contains_key(&name) || input.before.items.contains_key(&name) || assoc.contains_key(&name) || is_derived(&name, input.before) || is_derived(&name, input.after);
        let side = match body.status {
            BodyStatus::BeforeOnly => "the body exists only before the step",
            BodyStatus::AfterOnly => "the body exists only after the step",
            _ => "the MIR changed, but the text did not",
        };
        let (description, why) = if crate::is_test_harness_main(&name) {
            let krate = name.split("::").next().unwrap_or(&name);
            (format!("the test harness of `{krate}` changed: the test runner, or the form of the generated `main`"), "the function that runs the tests changed, and Semilla does not know the cause: give the cause as the reason".to_string())
        } else if known {
            (format!("indirect change, cause not known: `{name}` ({side})"), "an indirect change that Semilla cannot explain: give the cause as the reason".to_string())
        } else {
            (format!("code that Semilla does not see: `{name}` ({side})"), "a MIR body with no source item (generated code): give the cause as the reason".to_string())
        };
        if unknown.contains_key(&description) {
            continue;
        }
        unknown.insert(description.clone(), report.facts.len());
        report.facts.push(Fact { build_code: None, build_code_change: false, causes: Vec::new(), reason_covers: 0, branches: None, through: Default::default(), unsafe_code: false, other_edits: 0, reason_key: None, reason_item: None,
            id: format!("f{}", report.facts.len() + 1),
            at: Some(anchor),
            file: body.span.as_ref().map(|(file, _)| file.clone()),
            part: "indirect change".into(),
            description,
            status: "derived".into(),
            category: Category::Indirect,
            compare: Default::default(),
            affected: vec![name.clone()],
            spotlight: true,
            spotlight_why: Some(why),
            reason: input.step.reasons.get(&place).cloned(),
            sites: Vec::new(),
            content: None,
            equal_sites: Vec::new(),
            note: None,
        });
    }
    for (krate, bodies) in no_edit {
        if bodies.len() <= CRATE_BODIES {
            for (name, nodes) in &bodies {
                body_fact(input, report, name, nodes, true);
            }
            continue;
        }
        let names: Vec<String> = bodies.iter().map(|(name, _)| name.clone()).collect();
        let shown: Vec<String> = names.iter().take(CRATE_BODIES).map(|name| format!("`{name}`")).collect();
        let anchor = Anchor::item(&krate);
        report.facts.push(Fact {
            id: format!("f{}", report.facts.len() + 1),
            at: Some(anchor.clone()),
            part: "indirect change".into(),
            description: format!("the compiled code of {} bodies of the crate `{krate}` changes with no edit of their text: {}, and {} more", names.len(), shown.join(", "), names.len() - CRATE_BODIES),
            status: "derived".into(),
            category: Category::Indirect,
            affected: names,
            spotlight: true,
            spotlight_why: Some("an indirect change that Semilla cannot explain: give the cause as the reason".to_string()),
            reason: input.step.reasons.get(&anchor.to_string()).cloned(),
            ..Default::default()
        });
    }
    // The symbols of the links (design 44): "through: `T` (2 bodies)".
    for (fact, symbols) in through_of {
        if let Some(fact) = report.facts.get_mut(fact) {
            for (symbol, bodies) in symbols {
                let list = fact.through.entry(symbol).or_default();
                list.extend(bodies.into_iter().filter(|body| fact.affected.contains(body)));
                list.sort();
                list.dedup();
            }
            fact.through.retain(|_, bodies| !bodies.is_empty());
        }
    }
    // The branch mark (design 40.4) of each fact whose elements change the branches.
    for (fact, elements) in branch_of {
        if let Some(fact) = report.facts.get_mut(fact) {
            fact.branches = crate::mirmap::branch_mark(&elements);
        }
    }
    // Each edit counts one time: an edit with the doubt rule in one body is a doubt.
    map.counts.owners = map.owning.len();
    map.counts.doubt = map.idle_doubt.difference(&map.owning).count();
    map.counts.equal = map.idle_equal.iter().filter(|edit| !map.owning.contains(edit) && !map.idle_doubt.contains(edit)).count();
    map.counts.errors = map.errors;
    map.counts.examples = std::mem::take(&mut map.examples);
    map.counts.mapper_skipped = input.maps.over_limit.clone();
    (map.counts.round3, map.counts.composites, map.counts.gap_pairs) = (input.maps.round3, input.maps.composites, input.maps.gap_pairs);
    report.mir_map = map.counts;
}

/// The most bodies with no edit of one crate that get a fact each (design 40.3).
const CRATE_BODIES: usize = 3;

/// The most display texts in the fact of a body (design 40.3).
const BODY_TEXTS: usize = 3;

/// True for an edit of a form that can change the code at a different place (design 44, the
/// safety rule): a path, a type, a signature, a pattern, a callee, a parameter, a bound, a
/// generic parameter, or a `use` line. The old hint of design 40.3 used the same forms.
fn remote_form(edit: &Edit) -> bool {
    let kinds = [edit.node_before, edit.node_after];
    let role = edit.anchor().and_then(|anchor| anchor.path.last()).map(|segment| segment.to_string()).unwrap_or_default();
    let first = edit.anchor().and_then(|anchor| anchor.path.first()).map(|segment| segment.to_string()).unwrap_or_default();
    kinds.iter().flatten().any(|kind| matches!(*kind, "path" | "type" | "signature" | "pattern" | "use" | "generics" | "where"))
        || matches!(edit.kind, EditKind::Signature)
        || first == "sig"
        || role == "ty"
        || role == "pat"
        || role == "callee"
        || role.starts_with("param")
        || edit.attr_before.as_deref() == Some("use")
        || edit.attr_after.as_deref() == Some("use")
}

/// True for an edit of a binding whose whole value rule 3c (or 3d) of the normal form can move
/// into its uses, at other places (fix round 1 of design 46): a pattern (of a `let`, an `if
/// let`, a `for`), or a `let` statement or its init where the init is a place on one side (`t`,
/// `t.0`, `&t`). An init with a call or an operator keeps its own elements at its place.
fn binding_form(edit: &Edit) -> bool {
    let roles: Vec<String> = [edit.before.as_ref(), edit.after.as_ref()].into_iter().flatten().flat_map(|anchor| anchor.path.iter()).map(|segment| segment.to_string()).collect();
    if roles.iter().any(|role| role == "pat") {
        return true;
    }
    let statement = [edit.node_before, edit.node_after].into_iter().flatten().any(|kind| kind == "let");
    let texts = [edit.source_before.as_ref().or(edit.text_before.as_ref()), edit.source_after.as_ref().or(edit.text_after.as_ref())];
    (statement || roles.iter().any(|role| role == "init")) && texts.into_iter().flatten().any(|text| place_init(text))
}

/// True if the init of a `let` statement text, or an init text, is a place: a name with fields
/// (`t`, `t.0`, `self.a`), with a borrow or not. A part of an init (`0` of `t.0`, also a literal
/// such as `2`) counts too: a doubt is safe.
pub(crate) fn place_init(text: &str) -> bool {
    let text = text.trim().trim_end_matches(';').trim();
    let init = match text.strip_prefix("let ") {
        Some(rest) => match rest.split_once('=') {
            Some((_, init)) => init,
            None => return false,
        },
        None => text,
    };
    let init = init.trim();
    let init = init.strip_prefix("&mut ").or_else(|| init.strip_prefix('&')).unwrap_or(init);
    let init: String = init.chars().filter(|c| !c.is_whitespace()).collect();
    !init.is_empty() && init.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
}

/// Adds the symbols of the links of a body to the facts of their edits (design 44).
fn note_through(input: &Input, own: &crate::mirmap::Ownership, body: &str, through_of: &mut BTreeMap<usize, BTreeMap<String, BTreeSet<String>>>) {
    for (edit, symbols) in &own.through {
        let Some(fact) = input.fact_of[*edit] else { continue };
        for symbol in symbols {
            through_of.entry(fact).or_default().entry(symbol.clone()).or_default().insert(body.to_string());
        }
    }
}

/// The fact of the nodes of the MIR elements with no owner in one body (design 40.3): the
/// outermost nodes, at most `BODY_TEXTS` display texts, and the lines in a note. With
/// `spotlight`, the fact needs a reason (a body with no edit).
fn body_fact(input: &Input, report: &mut StepReport, name: &str, nodes: &BTreeSet<(usize, crate::mirmap::Mapped)>, spotlight: bool) {
    let anchor = Anchor::item(name);
    let place = anchor.to_string();
    // The outermost nodes of each side, and a node with the same text on the two sides one time
    // (the new side).
    let inside = |(side, node): &(usize, crate::mirmap::Mapped), (other_side, other): &(usize, crate::mirmap::Mapped)| side == other_side && node != other && node.file == other.file && other.start <= node.start && node.end <= other.end;
    let outer: Vec<&(usize, crate::mirmap::Mapped)> = nodes.iter().filter(|node| !nodes.iter().any(|other| inside(node, other))).collect();
    let new_texts: BTreeSet<&str> = outer.iter().filter(|(side, _)| *side == 1).map(|(_, node)| node.text.as_str()).collect();
    let mut kept: Vec<&(usize, crate::mirmap::Mapped)> = outer.into_iter().filter(|(side, node)| *side == 1 || !new_texts.contains(node.text.as_str())).collect();
    kept.sort_by(|(a, x), (b, y)| b.cmp(a).then(x.start.cmp(&y.start)));
    let mut texts: Vec<String> = kept.iter().map(|(side, node)| format!("`{}`{}", node.text, if *side == 0 { " (before the step)" } else { "" })).collect();
    texts.dedup();
    let more = texts.len().saturating_sub(BODY_TEXTS);
    texts.truncate(BODY_TEXTS);
    let list = if more > 0 { format!("{}, and {more} more", texts.join(", ")) } else { texts.join(", ") };
    let lines = |side: usize| -> Vec<String> {
        let mut lines: Vec<usize> = kept.iter().filter(|(found, _)| *found == side).map(|(_, node)| node.start.0).collect();
        lines.sort();
        lines.dedup();
        lines.iter().map(usize::to_string).collect()
    };
    let (new_lines, old_lines) = (lines(1), lines(0));
    let mut note = Vec::new();
    if !new_lines.is_empty() {
        note.push(format!("lines {}", new_lines.join(", ")));
    }
    if !old_lines.is_empty() {
        note.push(format!("lines before the step {}", old_lines.join(", ")));
    }
    let why = spotlight.then(|| "an indirect change that Semilla cannot explain: give the cause as the reason".to_string());
    report.facts.push(Fact {
        id: format!("f{}", report.facts.len() + 1),
        at: Some(anchor),
        file: kept.first().map(|(_, node)| node.file.clone()),
        part: "indirect change".into(),
        description: format!("the compiled code of `{name}` changes with no edit at its place: {list}"),
        status: "derived".into(),
        category: Category::Indirect,
        affected: vec![name.to_string()],
        spotlight,
        spotlight_why: why,
        reason: if spotlight { input.step.reasons.get(&place).cloned() } else { None },
        note: Some(note.join("; ")).filter(|note| !note.is_empty()),
        ..Default::default()
    });
}

/// The cause of a body that a `macro_rules!` macro makes (batch 6a, B2).
enum MacroCause {
    /// The fact of a macro call that the step adds, removes, or changes, and the name of the
    /// function that the call makes.
    Call(usize, String),
    /// The fact of a changed macro definition.
    Definition(usize),
}

/// True for the item of a `macro_rules!` definition.
pub(crate) fn is_macro_definition(item: &semilla_syntax::Item) -> bool {
    item.kind == semilla_syntax::ItemKind::Macro && item.source.contains("macro_rules!")
}

/// The fact of a macro call item: the fact of the call, or the fact of an added or removed
/// module that has the call.
fn call_fact(report: &StepReport, item: &semilla_syntax::Item) -> Option<usize> {
    report.facts.iter().position(|fact| {
        let Some(at) = fact.at.as_ref().map(|at| at.item.as_str()) else { return false };
        let whole = fact.description.starts_with("added `") || fact.description.starts_with("removed `");
        fact.part == "edit" && (at == item.path || (whole && item.path.starts_with(&format!("{at}::"))))
    })
}

/// The macro call item that makes a body, by the place of the call from the compiler (batch
/// 6b, part 2). `None`: the compiler gives no place. `Some(None)`: no call item is at the place.
fn site_call<'a>(input: &'a Input, body: &BodyComparison, name: &str) -> Option<Option<&'a semilla_syntax::Item>> {
    let (ws, side) = if body.status == BodyStatus::BeforeOnly { (input.before, 0) } else { (input.after, 1) };
    let made = input.made?;
    let (file, line, col) = made[side].sites.get(&body.name).or_else(|| made[side].sites.get(name))?;
    Some(ws.items.values().filter(|item| crate::made::is_call(item) && crate::made::in_range(item, (file, *line, *col))).min_by_key(|item| item.line_end - item.line_start))
}

/// Finds the macro that makes a body with no source item, and the fact that is the cause.
///
/// - The span of the body is in the text of a `macro_rules!` definition.
/// - A call of this macro in a module of the body has the name of the function in its text.
/// - A call with a fact of this step (added, removed, or changed; also the fact of an added or
///   removed module that has the call) is the cause.
/// - If no call has a fact, a changed definition is the cause.
fn macro_cause(input: &Input, report: &StepReport, body: &BodyComparison, name: &str) -> Option<MacroCause> {
    let (file, line) = body.span.as_ref()?;
    let (ws, other) = if body.status == BodyStatus::BeforeOnly { (input.before, input.after) } else { (input.after, input.before) };
    // The function of the body: the name with no closure part.
    let function = name.split("::{").next()?;
    let ident = function.rsplit("::").next()?;
    let fact_of = |item: &semilla_syntax::Item| call_fact(report, item);
    // The compiler gives the place of the macro call of the body (batch 6b, part 2): the call
    // item at this place is the call, with no search for the name in the text.
    let site_call = site_call(input, body, name);
    if let Some(index) = site_call.flatten().and_then(fact_of) {
        return Some(MacroCause::Call(index, ident.to_string()));
    }
    let definition = ws.items.values().find(|item| is_macro_definition(item) && item.file == *file && (item.line_start..=item.line_end).contains(line))?;
    let macro_name = definition.path.rsplit("::").next()?.split('!').next()?;
    let has_ident = |text: &str| text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| word == ident);
    let is_call = |item: &&semilla_syntax::Item| {
        let last = item.path.rsplit("::").next().unwrap_or("");
        let module = item.path.rsplit_once("::").map(|(owner, _)| owner).unwrap_or("");
        item.kind == semilla_syntax::ItemKind::Macro && !is_macro_definition(item) && last.split('!').next() == Some(macro_name) && function.starts_with(&format!("{module}::")) && has_ident(&item.source)
    };
    // With no place from the compiler, the text of the call gives the call (batch 6a, B2).
    if site_call.is_none() && let Some(index) = ws.items.values().filter(is_call).find_map(fact_of) {
        return Some(MacroCause::Call(index, ident.to_string()));
    }
    let changed = other.items.get(&definition.path).is_none_or(|old| old.source != definition.source);
    let index = report.facts.iter().position(|fact| fact.at.as_ref().is_some_and(|at| at.item == definition.path))?;
    changed.then_some(MacroCause::Definition(index))
}

/// Gives the bodies that a macro call makes to the fact of the call, or to the fact of the
/// changed macro definition (batch 6a, B2). Returns the bodies with no such cause.
///
/// - A call that the step adds, removes, or changes: the fact of the call lists the bodies.
///   The bodies get no fact and no spotlight of their own.
/// - A changed definition: one fact and one spotlight for the macro. The text gives the count
///   of the bodies, the bodies that no longer exist, and the tests that no longer exist.
fn macro_bodies<'a>(input: &Input, report: &mut StepReport, pending: Vec<(&'a BodyComparison, String)>, owned: &mut BTreeMap<String, Owner>) -> Vec<(&'a BodyComparison, String)> {
    let mut rest = Vec::new();
    let mut made: BTreeMap<usize, (BTreeSet<String>, bool)> = BTreeMap::new();
    let mut definitions: BTreeMap<usize, usize> = BTreeMap::new();
    for (body, name) in pending {
        match macro_cause(input, report, body, &name) {
            Some(MacroCause::Call(index, function)) => {
                record(report, Owner::Fact(index), &name);
                let entry = made.entry(index).or_default();
                entry.0.insert(function);
                entry.1 |= body.status == BodyStatus::BeforeOnly;
                owned.insert(name, Owner::Fact(index));
            }
            Some(MacroCause::Definition(index)) => {
                record(report, Owner::Fact(index), &name);
                *definitions.entry(index).or_default() += 1;
                owned.insert(name, Owner::Fact(index));
            }
            None => rest.push((body, name)),
        }
    }
    let list = |names: &BTreeSet<String>| {
        let mut text = names.iter().take(5).map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ");
        if names.len() > 5 {
            text.push_str(&format!(", and {} more", names.len() - 5));
        }
        text
    };
    for (index, (names, removed)) in made {
        // The text of the fact of a call lists the items already (batch 6b, part 2).
        if input.made_listed.contains(&index) {
            continue;
        }
        if let Some(fact) = report.facts.get_mut(index) {
            fact.description.push_str(&format!(" ({}: {})", if removed { "made" } else { "makes" }, list(&names)));
        }
    }
    for (index, count) in definitions {
        let Some(fact) = report.facts.get(index) else { continue };
        // The functions of the fact: the bodies with no closure part.
        let functions: BTreeSet<String> = fact.affected.iter().filter_map(|name| name.split("::{").next()).map(str::to_string).collect();
        let short = |names: Vec<&String>| names.into_iter().map(|name| name.rsplit("::").next().unwrap_or(name).to_string()).collect::<BTreeSet<String>>();
        let gone = short(input.bodies.iter().filter(|body| body.status == BodyStatus::BeforeOnly && functions.contains(&body.name)).map(|body| &body.name).collect());
        let mut why = format!("a changed macro: the MIR of {count} {} that the macro makes changed with no edit of the text of a call", if count == 1 { "body" } else { "bodies" });
        if !gone.is_empty() {
            why.push_str(&format!("; {} of the bodies no longer exist: {}", gone.len(), list(&gone)));
        }
        if let Some([before, after]) = input.tests {
            let lost = short(functions.iter().filter(|name| before.contains(*name) && !after.contains(*name)).collect());
            let new = short(functions.iter().filter(|name| !before.contains(*name) && after.contains(*name)).collect());
            if !lost.is_empty() {
                why.push_str(&format!("; {} test(s) no longer exist: {}", lost.len(), list(&lost)));
            }
            if !new.is_empty() {
                why.push_str(&format!("; {} new test(s): {}", new.len(), list(&new)));
            }
        }
        let Some(fact) = report.facts.get_mut(index) else { continue };
        fact.spotlight_why = Some(match fact.spotlight_why.take() {
            Some(known) => format!("{known}; {why}"),
            None => why,
        });
        if !fact.spotlight {
            fact.spotlight = true;
            fact.reason = input.step.reasons.get(&fact.reason_place()).cloned();
        }
    }
    rest
}

/// Invariant I4: the compiler and the syntax tree must read the same code. A changed body whose
/// MIR span is in a Rust file of the tree that the loader did not parse (for example a file that
/// `include!` reads), or in another file than its syntax item, gets a fact in the spotlight.
pub fn source_files(input: &Input, report: &mut StepReport) {
    for body in changed(input.bodies) {
        let Some((file, _)) = &body.span else { continue };
        let ws = if body.status == BodyStatus::BeforeOnly { input.before } else { input.after };
        // The file name does not decide: each file of the tree that the compiler reads is code.
        if !(ws.files.contains_key(file) || ws.rust_files.contains_key(file)) {
            continue;
        }
        let item = input.body_index.item_of(body);
        let name = item.clone().unwrap_or_else(|| body.name.clone());
        let why = if !ws.parsed_files.contains(file) {
            format!("the compiler reads `{name}` from `{file}`, a file that Semilla does not parse")
        } else if let Some(other) = item.as_ref().and_then(|item| ws.items.get(item)).filter(|it| it.file != *file) {
            format!("the compiler reads `{name}` from `{file}`, but the syntax item is in `{}`", other.file)
        } else {
            continue;
        };
        let anchor = Anchor::item(&name);
        let place = anchor.to_string();
        report.facts.push(Fact { build_code: None, build_code_change: false, causes: Vec::new(), reason_covers: 0, branches: None, through: Default::default(), unsafe_code: false, other_edits: 0, reason_key: None, reason_item: None,
            id: format!("f{}", report.facts.len() + 1),
            at: Some(anchor),
            file: Some(file.clone()),
            part: "source file".into(),
            description: format!("the compiler reads this item from another file: `{name}` ({file})"),
            status: "derived".into(),
            category: Category::File,
            compare: Default::default(),
            affected: vec![name.clone()],
            spotlight: true,
            spotlight_why: Some(why),
            reason: input.step.reasons.get(&place).cloned(),
            sites: Vec::new(),
            content: None,
            equal_sites: Vec::new(),
            note: None,
        });
    }
}

/// True for a body `<TYPE as TRAIT>::method` of a type that derives the trait.
fn is_derived(name: &str, ws: &Workspace) -> bool {
    let Some((ty, trait_path)) = name.strip_prefix('<').and_then(|rest| rest.split_once(">::")).and_then(|(inner, _)| inner.split_once(" as ")) else { return false };
    let trait_name = trait_path.rsplit("::").next().unwrap_or(trait_path);
    let ty = ty.split('<').next().unwrap_or(ty);
    ws.items.get(ty).is_some_and(|item| item.attrs.iter().any(|attr| attr.contains("derive") && crate::hints::words(attr).contains(&trait_name)))
}

/// Adds a body to the `affected` list of its owner.
fn record(report: &mut StepReport, owner: Owner, name: &str) {
    let list = match owner {
        Owner::Claim(index) => match report.claims.get_mut(index) {
            Some(claim) => &mut claim.affected,
            None => return,
        },
        Owner::Fact(index) => match report.facts.get_mut(index) {
            Some(fact) => &mut fact.affected,
            None => return,
        },
        Owner::Uncovered => return,
    };
    if !list.iter().any(|known| known == name) {
        list.push(name.to_string());
        list.sort();
    }
}

/// The identity claim of a pair whose old name is in the old text of an item, or whose new name
/// is in its new text, or whose type has the derive that makes the body. Only a rename counts: a
/// move keeps the name.
fn names_pair(input: &Input, name: &str) -> Option<usize> {
    let last = |path: &str| path.rsplit("::").next().unwrap_or(path).rsplit('.').next().unwrap_or(path).to_string();
    let has = |ws: &Workspace, word: &str| ws.items.get(name).is_some_and(|item| crate::hints::words(&item.text).contains(&word));
    // A body of a derive on the type of the pair (design 37.12): the derive puts the name of the
    // type in the code (`Debug` prints it), and the body has no item of its own in the source.
    let derived = |new: &str| {
        let ty = name.strip_prefix('<').and_then(|rest| rest.split_once(" as ")).map(|(ty, _)| ty.split('<').next().unwrap_or(ty));
        ty == Some(new) && is_derived(name, input.after)
    };
    input.identity_pairs.iter().find(|(_, old, new)| last(old) != last(new) && (has(input.before, &last(old)) || has(input.after, &last(new)) || derived(new))).map(|(claim, _, _)| *claim)
}

/// The edits that can change the body: the edits of its item. For a body on one
/// side only, also the edits of an item that contains it (a removed impl, a `cfg` attribute on a
/// module, a new derive). A doc edit and a visibility edit never change a body. An attribute edit
/// counts only when the attribute is not in the allow list of harmless attributes.
fn text_edits(input: &Input, body: &BodyComparison, name: &str) -> Vec<usize> {
    let one_side = matches!(body.status, BodyStatus::BeforeOnly | BodyStatus::AfterOnly);
    let ws = if body.status == BodyStatus::BeforeOnly { input.before } else { input.after };
    let mut items: BTreeSet<String> = crate::containers(ws, body.span.as_ref()).into_iter().collect();
    items.insert(name.to_string());
    let mut prefix = name;
    while let Some((parent, _)) = prefix.rsplit_once("::") {
        items.insert(parent.to_string());
        prefix = parent;
    }
    let mut found = Vec::new();
    for (index, edit) in input.edits.iter().enumerate() {
        let Some(item) = edit.item() else { continue };
        // An edit with a place only before the step: its item can have a new path (design 36.1).
        let item = if edit.after.is_none() { input.identity_pairs.iter().find(|(_, old, _)| old == item).map_or(item, |(_, _, new)| new.as_str()) } else { item };
        let own = item == name;
        let can_change = match edit.kind {
            EditKind::Docs | EditKind::Comment | EditKind::Visibility | EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged => false,
            EditKind::Attrs => !crate::facts::harmless_attribute_edit(edit) && items.contains(item),
            _ => own || (one_side && items.contains(item)),
        };
        if can_change {
            found.push(index);
        }
    }
    found
}

/// A fact or a claim that causes the change of a body with no text edit, by evidence.
fn evidence(input: &Input, report: &StepReport, body: &BodyComparison, name: &str, owned: &BTreeMap<String, Owner>, assoc: &BTreeMap<String, (String, String)>, symbols: bool) -> Option<Owner> {
    let item = input.after.items.get(name).or_else(|| input.before.items.get(name));
    // An associated constant has no item: its text comes from the impl or the trait.
    let text = item.map(|item| item.text.as_str()).or_else(|| assoc.get(name).map(|(_, text)| text.as_str())).unwrap_or("");
    let words = crate::hints::words(text);

    // 0. The compiler reads the body from a changed file that the loader does not parse (a file
    //    that `include!` reads): the file fact owns it.
    if let Some((file, _)) = &body.span {
        if let Some(index) = report.facts.iter().position(|fact| fact.file.as_deref() == Some(file.as_str()) && fact.at.is_none() && !crate::files::is_mode_fact(input.before, input.after, fact)) {
            return Some(Owner::Fact(index));
        }
    }

    // 1. A named const or static whose MIR changed: its value is inside this body. Since design
    //    44, only for a body with no symbol data (rules 1, 1b, and 4 are symbol links).
    for (other, owner) in owned.iter().filter(|_| !symbols) {
        let kind = input.after.items.get(other).or_else(|| input.before.items.get(other)).map(|item| item.kind);
        if matches!(kind, Some(ItemKind::Const | ItemKind::Static)) || assoc.contains_key(other) {
            let short = other.rsplit("::").next().unwrap_or(other);
            if words.contains(&short) {
                return Some(*owner);
            }
        }
    }

    // 1b. A constant or a static whose initializer runs a `const fn` that has an owner in this
    //     step (batch 5n, A2). The compiler evaluates the initializer, so the MIR of the constant
    //     holds the value, and not the call. The path is: the text of the constant names a
    //     `const fn`, and the MIR callee lists go from that function to the changed function.
    let constant = item.is_some_and(|item| matches!(item.kind, ItemKind::Const | ItemKind::Static)) || assoc.contains_key(name);
    if let Some(map) = input.callees.filter(|_| constant && !symbols) {
        let is_const_fn = |it: &semilla_syntax::Item| it.kind == ItemKind::Fn && it.text.split("fn ").next().is_some_and(|head| crate::hints::words(head).contains(&"const"));
        let mut reached: Vec<&str> = input.after.items.values().filter(|it| is_const_fn(it) && words.contains(&it.path.rsplit("::").next().unwrap_or(&it.path))).map(|it| it.path.as_str()).collect();
        let mut next = 0;
        while let Some(current) = reached.get(next).copied() {
            if let Some(owner) = owned.get(current) {
                return Some(*owner);
            }
            for callee in map.get(current).into_iter().flat_map(|facts| &facts.callees) {
                if !reached.contains(&callee.as_str()) {
                    reached.push(callee);
                }
            }
            next += 1;
        }
    }

    // 2. An included file that changed: `include_str!`, `include_bytes!`, `include!`.
    if let Some(item) = item {
        for file in included_files(&item.text, &item.file) {
            if let Some(index) = report.facts.iter().position(|fact| fact.file.as_deref() == Some(file.as_str()) && fact.part != "indirect change" && !crate::files::is_mode_fact(input.before, input.after, fact)) {
                return Some(Owner::Fact(index));
            }
            if let Some(index) = input.edits.iter().position(|edit| edit.file.as_deref() == Some(file.as_str())) {
                return Some(owner_of_edit(input, index));
            }
        }
        // A computed path (`concat!(env!("CARGO_MANIFEST_DIR"), "/notes.md")`) that has the name of
        // a changed file.
        if let Some(index) = report.facts.iter().position(|fact| fact.at.is_none() && fact.part != "indirect change" && !crate::files::is_mode_fact(input.before, input.after, fact) && fact.file.as_deref().is_some_and(|file| !file.ends_with(".rs") && reads_file(&item.text, &item.file, file))) {
            return Some(Owner::Fact(index));
        }
    }

    // 3. A changed `macro_rules!` macro or a changed proc macro (a function of a proc-macro
    //    crate) that the body expands: `name!(…)`, or an attribute `#[name]` of the item.
    let attrs: Vec<&str> = item.map(|item| item.attrs.iter().flat_map(|attr| crate::hints::words(attr)).collect()).unwrap_or_default();
    for (index, edit) in input.edits.iter().enumerate() {
        let Some(macro_path) = edit.item() else { continue };
        let found = [input.after, input.before].iter().find_map(|ws| ws.items.get(macro_path).filter(|it| it.kind == ItemKind::Macro || (it.kind == ItemKind::Fn && it.attrs.iter().any(|attr| attr.contains("proc_macro")))));
        let Some(macro_item) = found else { continue };
        if matches!(edit.kind, EditKind::Docs | EditKind::Comment | EditKind::Visibility) {
            continue;
        }
        let short = macro_path.rsplit("::").next().unwrap_or(macro_path).trim_end_matches('!');
        let attribute = macro_item.kind == ItemKind::Fn && attrs.contains(&short);
        if (words.contains(&short) && text_has_bang(text, short)) || attribute {
            return Some(owner_of_edit(input, index));
        }
    }

    // 4. A new item that the body now calls (a new method that method selection picks, a new
    //    trait impl). The MIR callee list is the evidence.
    if let Some(facts) = input.callees.and_then(|map| map.get(name)).filter(|_| !symbols) {
        for callee in facts.callees.iter().chain(&facts.externals) {
            let new = input.edits.iter().position(|edit| edit.kind == EditKind::ItemAdded && edit.item().is_some_and(|added| added == callee || callee.starts_with(&format!("{added}::"))));
            if let Some(index) = new {
                return Some(owner_of_edit(input, index));
            }
        }
    }

    // 5. A package key fact owns the bodies that read the package data with `env!`. The other
    //    build inputs come after the evidence, with their reach (`build_inputs`).
    let reads_package = text.contains("CARGO_PKG_");
    let manifest = report.facts.iter().position(|fact| fact.file.as_deref().is_some_and(crate::files::is_manifest) && !fact.description.ends_with(": format only") && reads_package && fact.description.contains("package."));
    manifest.map(Owner::Fact)
}

/// An associated constant (batch 5n, A2): a body with no item of its own whose MIR span is in an
/// impl or a trait that has a constant with the name. Returns the path of that parent item and
/// the text of the constant.
fn assoc_const(input: &Input, body: &BodyComparison) -> Option<(String, String)> {
    use semilla_syntax::rs::{ast, compare, parse_item};
    use ast::HasName;
    let (file, line) = body.span.as_ref()?;
    // A body in the constant (a closure, an inline constant) is not the constant.
    let short = body.name.rsplit("::").next().filter(|short| !short.starts_with('{'))?;
    let ws = if body.status == BodyStatus::BeforeOnly { input.before } else { input.after };
    let inside = |item: &&semilla_syntax::Item| (file.ends_with(&item.file) || item.file.ends_with(file.as_str())) && item.line_start <= *line && *line <= item.line_end;
    let parent = ws.items.values().filter(|item| matches!(item.kind, ItemKind::Impl | ItemKind::Trait)).filter(inside).min_by_key(|item| item.line_end - item.line_start)?;
    let list = match parse_item(&parent.source)? {
        ast::Item::Impl(value) if parent.kind == ItemKind::Impl => value.assoc_item_list()?,
        ast::Item::Trait(value) if parent.kind == ItemKind::Trait => value.assoc_item_list()?,
        _ => return None,
    };
    let text = list.assoc_items().find_map(|member| match member {
        ast::AssocItem::Const(constant) if constant.name().is_some_and(|name| name.text() == short) => Some(compare(&constant)),
        _ => None,
    })?;
    Some((parent.path.clone(), text))
}

/// The owners of the edits of the text of an associated constant: the edits of its parent item
/// (the impl or the trait) that have the name of the constant in a changed text. An edit of
/// another member of the parent is not a cause.
fn assoc_text_owners(input: &Input, name: &str, parent: &str) -> Vec<Owner> {
    let short = name.rsplit("::").next().unwrap_or(name);
    let mut found = Vec::new();
    for (index, edit) in input.edits.iter().enumerate() {
        let names = |text: &Option<String>| text.as_deref().is_some_and(|text| crate::hints::words(text).contains(&short));
        let named = names(&edit.source_before) || names(&edit.source_after) || names(&edit.text_before) || names(&edit.text_after);
        if edit.item() != Some(parent) || matches!(edit.kind, EditKind::Docs | EditKind::Comment | EditKind::Visibility) || !named {
            continue;
        }
        let owner = owner_of_edit(input, index);
        if !found.contains(&owner) {
            found.push(owner);
        }
    }
    found
}

/// A callee of the body that has an owner in this step and a changed MIR signature (batch 5n,
/// A1). The MIR signature of a function shows the type behind an `impl Trait` return type. A
/// change of that type changes the MIR of each caller, with no edit of the text of the caller.
/// The MIR callee list of the body is the path from the body to the changed item.
///
/// A callee with the same MIR signature is not a cause: its body is not a part of the MIR of
/// the caller, so the body gets the fact "indirect change, cause not known".
fn changed_callee(input: &Input, name: &str, owned: &BTreeMap<String, Owner>) -> Option<(Owner, String)> {
    let map = input.callees?;
    map.get(name)?.callees.iter().find_map(|callee| {
        let owner = owned.get(callee)?;
        map.get(callee).filter(|facts| facts.signature_changed).map(|_| (*owner, callee.clone()))
    })
}

/// Adds the cause of the callers to the fact of a callee with a changed MIR signature.
fn note_signature(report: &mut StepReport, owner: Owner, callee: &str) {
    let Owner::Fact(index) = owner else { return };
    let Some(fact) = report.facts.get_mut(index) else { return };
    let note = format!("the MIR signature of `{callee}` changed (the type behind `impl Trait`), so the MIR of each caller changed");
    if !fact.description.contains(&note) {
        fact.description.push_str(&format!("; {note}"));
    }
}

/// The build input facts of the step (brief 5f, B2), each with the crates that it can reach.
/// `None` is each crate. This is an allow list of fact kinds (invariant I3): a fact of another
/// kind is not a cause, and the body gets the fact "indirect change, cause not known".
///
/// - A manifest key that is not a descriptive package key: the package and the crates that depend
///   on it. A workspace manifest, `Cargo.lock`, a cargo configuration file, and a toolchain file
///   reach each crate.
/// - A build script: its package and the crates that depend on it.
/// - A file that a crate reads at build time: that crate and the crates that depend on it.
fn build_inputs(input: &Input, report: &StepReport) -> Vec<(usize, Option<BTreeSet<String>>)> {
    let crates: Vec<&semilla_syntax::CrateInfo> = input.before.crates.iter().chain(&input.after.crates).collect();
    // A crate and the crates that depend on it.
    let dependents = |start: String| -> BTreeSet<String> {
        let mut found = BTreeSet::from([start]);
        loop {
            let more: Vec<String> = crates.iter().filter(|krate| !found.contains(&krate.name) && krate.deps.iter().any(|dep| found.contains(dep))).map(|krate| krate.name.clone()).collect();
            if more.is_empty() {
                return found;
            }
            found.extend(more);
        }
    };
    // The package of a file: the member crate with the longest folder that contains the file. A
    // file of no member folder (the workspace root) has no package.
    let package = |file: &str| crates.iter().filter(|krate| !krate.dir.is_empty() && krate.dir != "." && file.starts_with(&format!("{}/", krate.dir))).max_by_key(|krate| krate.dir.len()).map(|krate| krate.name.clone());
    let mut out = Vec::new();
    for (index, fact) in report.facts.iter().enumerate() {
        let Some(file) = fact.file.as_deref().filter(|_| fact.at.is_none() && fact.part != "indirect change") else { continue };
        // A file mode changes no body (batch 6a, B4).
        if crate::files::is_mode_fact(input.before, input.after, fact) {
            continue;
        }
        let name = file.rsplit('/').next().unwrap_or(file);
        if crate::files::is_manifest(file) {
            if fact.description.ends_with(": format only") || !crate::files::is_build_key(&fact.part) {
                continue;
            }
            match package(file).filter(|_| name == "Cargo.toml") {
                Some(krate) => out.push((index, Some(dependents(krate)))),
                None => out.push((index, None)),
            }
        } else if let Some(krate) = crate::files::build_script_of(input.after, file).or_else(|| crate::files::build_script_of(input.before, file)) {
            out.push((index, Some(dependents(krate))));
        } else if !file.ends_with(".rs") {
            if let Some((reader, _)) = build_reader(input.before, input.after, file) {
                out.push((index, package(&reader).map(dependents)));
            }
        }
    }
    out
}

/// True if the name is followed by `!` in the text (a macro call).
fn text_has_bang(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let boundary = before.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        boundary && text[start + name.len()..].trim_start().starts_with('!')
    })
}

/// The owner of one edit.
fn owner_of_edit(input: &Input, index: usize) -> Owner {
    match (input.owners[index], input.fact_of[index]) {
        (Some(claim), _) => Owner::Claim(claim),
        (None, Some(fact)) => Owner::Fact(fact),
        (None, None) => Owner::Uncovered,
    }
}

/// The files that an item includes, relative to the workspace root. The path of the macro is
/// relative to the folder of the file that contains the call.
pub fn included_files(text: &str, file: &str) -> Vec<String> {
    let mut found = Vec::new();
    for name in ["include_str", "include_bytes", "include"] {
        for (start, _) in text.match_indices(name) {
            let before = text[..start].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let rest = text[start + name.len()..].trim_start();
            let Some(rest) = rest.strip_prefix('!') else { continue };
            let rest = rest.trim_start().trim_start_matches(['(', '[', '{']).trim_start();
            let Some(rest) = rest.strip_prefix('"') else { continue };
            let Some(end) = rest.find('"') else { continue };
            let relative = &rest[..end];
            let dir = file.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
            found.push(normalize(&if dir.is_empty() { relative.to_string() } else { format!("{dir}/{relative}") }));
        }
    }
    found
}

/// True if the Rust text reads `file` at build time with `include_str!`, `include_bytes!`, or
/// `include!`: a literal path that resolves to the file, or a macro call that has the file name
/// in a computed path (`concat!(env!("CARGO_MANIFEST_DIR"), "/notes.md")`).
fn reads_file(text: &str, path: &str, file: &str) -> bool {
    if included_files(text, path).iter().any(|found| found == file) {
        return true;
    }
    let name = file.rsplit('/').next().unwrap_or(file);
    text.match_indices("include").any(|(start, _)| {
        let rest = &text[start..];
        // The item text has a space between the tokens: `include_str ! (…)`.
        let Some(open) = rest.find('!').filter(|bang| matches!(rest[..*bang].trim_end(), "include" | "include_str" | "include_bytes")) else { return false };
        let call: String = rest[open..].chars().take(400).collect();
        let call = call.split(';').next().unwrap_or("");
        call.contains(&format!("/{name}\"")) || call.contains(&format!("\"{name}\""))
    })
}

/// The Rust file that reads `file` at build time, and the items that read it (brief 5f, A6). The
/// source is the text of the Rust files of the two trees.
pub fn build_reader(before: &Workspace, after: &Workspace, file: &str) -> Option<(String, BTreeSet<String>)> {
    let reader = [after, before].iter().find_map(|ws| ws.rust_files.iter().find(|(path, text)| reads_file(text, path, file)).map(|(path, _)| path.clone()))?;
    let items = [after, before].iter().flat_map(|ws| ws.items.values()).filter(|item| reads_file(&item.text, &item.file, file)).map(|item| item.path.clone()).collect();
    Some((reader, items))
}

/// The items that read `file` as Rust code with `include!` and a literal path, or `None` if no
/// Rust file of the two trees has such a call (batch 5w, D3). The compiler reads such a file as
/// code of the item, whatever the name of the file is. Semilla does not parse it.
pub fn code_readers(before: &Workspace, after: &Workspace, file: &str) -> Option<BTreeSet<String>> {
    fn reads(text: &str, path: &str, file: &str) -> bool {
        text.match_indices("include").any(|(start, _)| {
            if text[..start].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                return false;
            }
            // `include_str!` and `include_bytes!` read data, not code: the `!` must be the next token.
            let Some(rest) = text[start + "include".len()..].trim_start().strip_prefix('!') else { return false };
            let rest = rest.trim_start().trim_start_matches(['(', '[', '{']).trim_start();
            let Some(relative) = rest.strip_prefix('"').and_then(|rest| rest.split('"').next()) else { return false };
            let dir = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
            normalize(&if dir.is_empty() { relative.to_string() } else { format!("{dir}/{relative}") }) == file
        })
    }
    [after, before].iter().find(|ws| ws.rust_files.iter().any(|(path, text)| reads(text, path, file)))?;
    Some([after, before].iter().flat_map(|ws| ws.items.values()).filter(|item| reads(&item.text, &item.file, file)).map(|item| item.path.clone()).collect())
}

/// Removes the `.` and `..` parts of a relative path.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// The final assertion (invariant I1): each changed body has an owner in the report. An owner is
/// a fact or a claim that lists the body in its `affected` list, an uncovered entry, or a
/// rejected claim. Returns the bodies with no owner.
pub fn check(input: &Input, report: &StepReport) -> Vec<String> {
    let mut missing = Vec::new();
    for body in changed(input.bodies) {
        let item = input.body_index.item_of(body);
        if is_harness(body) || input.identity_owned.contains(&body.name) {
            continue;
        }
        let name = item.unwrap_or_else(|| body.name.clone());
        let by_fact = report.facts.iter().any(|fact| fact.affected.contains(&name));
        let by_claim = report.claims.iter().any(|claim| claim.affected.contains(&name));
        // An uncovered entry, an error, or an unverified claim rejects the step.
        let rejected = !report.uncovered.is_empty() || report.claims.iter().any(|claim| matches!(claim.status, ClaimStatus::Refuted { .. } | ClaimStatus::Malformed { .. }));
        if !(by_fact || by_claim || rejected) {
            missing.push(name);
        }
    }
    missing
}
