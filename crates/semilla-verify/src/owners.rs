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

use semilla_core::{Anchor, Class, ClaimStatus, Fact, Step, StepReport};
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
    pub callees: Option<&'a BTreeMap<String, ItemFacts>>,
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
    for body in changed(input.bodies) {
        let item = input.body_index.item_of(body);
        if is_harness(body) || input.identity_owned.contains(&body.name) {
            continue;
        }
        let name = item.clone().unwrap_or_else(|| body.name.clone());
        let mut found = text_owners(input, body, &name);
        // An associated constant has no item of its own: an edit of its text is an edit of the
        // impl or of the trait that has it.
        if let Some((parent, _)) = assoc.get(&name).filter(|_| found.is_empty()) {
            found = assoc_text_owners(input, &name, parent);
        }
        if !found.is_empty() {
            for owner in &found {
                record(report, *owner, &name);
            }
            owned.insert(name, found[0]);
            continue;
        }
        if input.rename_targets.contains(&name) || input.identity_derived.contains(&body.name) {
            // The identity claim must prove the body equal. A different body makes it unproved.
            if let Some(claim) = input.identity_claim {
                record(report, Owner::Claim(claim), &name);
                owned.insert(name, Owner::Claim(claim));
                continue;
            }
        }
        // F5: a fact whose entity the body uses (computed in `facts::derive`).
        if let Some(index) = report.facts.iter().position(|fact| fact.affected.contains(&name)) {
            owned.insert(name, Owner::Fact(index));
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
            let found = evidence(input, report, body, &name, &owned, &assoc).or_else(|| {
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
        if group.len() > 1 {
            let list: Vec<String> = group.iter().filter_map(|other| report.facts.get(*other)).map(|fact| format!("`{}`", fact.description)).collect();
            // D10 of batch 5k: the facts of one group can have different counts. A body with
            // one possible cause (a body that reads this key with `env!`, a body of a crate
            // that only this change can reach) is not in the count of the other facts.
            if shared == count {
                why.push_str(&format!(". The bodies have one of these causes: {}", list.join(", ")));
            } else {
                why.push_str(&format!(". {shared} of the bodies have one of these causes: {}. This change is the only cause of the other {}", list.join(", "), count - shared));
            }
        }
        let Some(fact) = report.facts.get_mut(index) else { continue };
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

    // No known cause: a new fact with the spotlight.
    for (body, name) in pending {
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
        let class = if matches!(body.status, BodyStatus::Different { cfg_changed: false }) { Class::Logic } else { Class::ControlFlow };
        report.facts.push(Fact { build_code: None, build_code_change: false,
            id: format!("f{}", report.facts.len() + 1),
            at: Some(anchor),
            file: body.span.as_ref().map(|(file, _)| file.clone()),
            part: "indirect change".into(),
            description,
            status: "derived".into(),
            class: Some(class),
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
        report.facts.push(Fact { build_code: None, build_code_change: false,
            id: format!("f{}", report.facts.len() + 1),
            at: Some(anchor),
            file: Some(file.clone()),
            part: "source file".into(),
            description: format!("the compiler reads this item from another file: `{name}` ({file})"),
            status: "derived".into(),
            class: Some(Class::Logic),
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

/// The owners of the edits that can change the body: the edits of its item. For a body on one
/// side only, also the edits of an item that contains it (a removed impl, a `cfg` attribute on a
/// module, a new derive). A doc edit and a visibility edit never change a body. An attribute edit
/// counts only when the attribute is not in the allow list of harmless attributes.
fn text_owners(input: &Input, body: &BodyComparison, name: &str) -> Vec<Owner> {
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
        let own = item == name;
        let can_change = match edit.kind {
            EditKind::Docs | EditKind::Comment | EditKind::Visibility | EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged => false,
            EditKind::Attrs => !crate::facts::harmless_attribute_edit(edit) && items.contains(item),
            _ => own || (one_side && items.contains(item)),
        };
        if !can_change {
            continue;
        }
        let owner = match (input.owners[index], input.fact_of[index]) {
            (Some(claim), _) => Owner::Claim(claim),
            (None, Some(fact)) => Owner::Fact(fact),
            (None, None) => Owner::Uncovered,
        };
        if !found.contains(&owner) {
            found.push(owner);
        }
    }
    found
}

/// A fact or a claim that causes the change of a body with no text edit, by evidence.
fn evidence(input: &Input, report: &StepReport, body: &BodyComparison, name: &str, owned: &BTreeMap<String, Owner>, assoc: &BTreeMap<String, (String, String)>) -> Option<Owner> {
    let item = input.after.items.get(name).or_else(|| input.before.items.get(name));
    // An associated constant has no item: its text comes from the impl or the trait.
    let text = item.map(|item| item.text.as_str()).or_else(|| assoc.get(name).map(|(_, text)| text.as_str())).unwrap_or("");
    let words = crate::hints::words(text);

    // 0. The compiler reads the body from a changed file that the loader does not parse (a file
    //    that `include!` reads): the file fact owns it.
    if let Some((file, _)) = &body.span {
        if let Some(index) = report.facts.iter().position(|fact| fact.file.as_deref() == Some(file.as_str()) && fact.at.is_none()) {
            return Some(Owner::Fact(index));
        }
    }

    // 1. A named const or static whose MIR changed: its value is inside this body.
    for (other, owner) in owned {
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
    if let Some(map) = input.callees.filter(|_| constant) {
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
            if let Some(index) = report.facts.iter().position(|fact| fact.file.as_deref() == Some(file.as_str()) && fact.part != "indirect change") {
                return Some(Owner::Fact(index));
            }
            if let Some(index) = input.edits.iter().position(|edit| edit.file.as_deref() == Some(file.as_str())) {
                return Some(owner_of_edit(input, index));
            }
        }
        // A computed path (`concat!(env!("CARGO_MANIFEST_DIR"), "/notes.md")`) that has the name of
        // a changed file.
        if let Some(index) = report.facts.iter().position(|fact| fact.at.is_none() && fact.part != "indirect change" && fact.file.as_deref().is_some_and(|file| !file.ends_with(".rs") && reads_file(&item.text, &item.file, file))) {
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
    if let Some(facts) = input.callees.and_then(|map| map.get(name)) {
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
    let manifest = report.facts.iter().position(|fact| fact.file.as_deref().is_some_and(crate::files::is_manifest) && fact.class != Some(Class::Neutral) && reads_package && fact.description.contains("package."));
    manifest.map(Owner::Fact)
}

/// An associated constant (batch 5n, A2): a body with no item of its own whose MIR span is in an
/// impl or a trait that has a constant with the name. Returns the path of that parent item and
/// the text of the constant.
fn assoc_const(input: &Input, body: &BodyComparison) -> Option<(String, String)> {
    use quote::ToTokens;
    let (file, line) = body.span.as_ref()?;
    // A body in the constant (a closure, an inline constant) is not the constant.
    let short = body.name.rsplit("::").next().filter(|short| !short.starts_with('{'))?;
    let ws = if body.status == BodyStatus::BeforeOnly { input.before } else { input.after };
    let inside = |item: &&semilla_syntax::Item| (file.ends_with(&item.file) || item.file.ends_with(file.as_str())) && item.line_start <= *line && *line <= item.line_end;
    let parent = ws.items.values().filter(|item| matches!(item.kind, ItemKind::Impl | ItemKind::Trait)).filter(inside).min_by_key(|item| item.line_end - item.line_start)?;
    let text = if parent.kind == ItemKind::Impl {
        syn::parse_str::<syn::ItemImpl>(&parent.source).ok()?.items.iter().find_map(|member| match member {
            syn::ImplItem::Const(constant) if constant.ident == short => Some(constant.to_token_stream().to_string()),
            _ => None,
        })
    } else {
        syn::parse_str::<syn::ItemTrait>(&parent.source).ok()?.items.iter().find_map(|member| match member {
            syn::TraitItem::Const(constant) if constant.ident == short => Some(constant.to_token_stream().to_string()),
            _ => None,
        })
    }?;
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
        let name = file.rsplit('/').next().unwrap_or(file);
        if crate::files::is_manifest(file) {
            if fact.class == Some(Class::Neutral) || !crate::files::is_build_key(&fact.part) {
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
/// a fact or a claim that lists the body in its `affected` list and has a class that is not
/// neutral (a claim that is not verified also rejects the step), an uncovered entry, or a
/// verified identity claim that records the derived body. Returns the bodies with no owner.
pub fn check(input: &Input, report: &StepReport) -> Vec<String> {
    let mut missing = Vec::new();
    for body in changed(input.bodies) {
        let item = input.body_index.item_of(body);
        if is_harness(body) || input.identity_owned.contains(&body.name) {
            continue;
        }
        let name = item.unwrap_or_else(|| body.name.clone());
        let by_fact = report.facts.iter().any(|fact| fact.affected.contains(&name) && fact.class.is_some_and(|class| class != Class::Neutral));
        let by_claim = report.claims.iter().any(|claim| claim.affected.contains(&name) && (!claim.status.is_verified() || claim.class != Some(Class::Neutral)));
        // An uncovered entry, an error, or an unverified claim rejects the step.
        let rejected = !report.uncovered.is_empty() || report.claims.iter().any(|claim| matches!(claim.status, ClaimStatus::Refuted { .. } | ClaimStatus::Malformed { .. }));
        if !(by_fact || by_claim || rejected) {
            missing.push(name);
        }
    }
    missing
}
