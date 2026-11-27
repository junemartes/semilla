//! The claims of the intent files of a step (design 67, parts C and D).
//!
//! The verifier parses each intent file of the step. A claim of the forms of `--same`
//! (`#[rename]`, `#[moved]`, `#[rewrite]`) is a `same` fact of the agent with the same check
//! (design 48). Each other claim goes to the oracle, which resolves its references against the
//! compiler data of each version and checks it (`semilla_smt::mir::claim`). The result of a
//! claim is a line of the step: it never changes a fact, a gap, or a compare result.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use semilla_core::{BodyCover, CoverState, IntentClaim, IntentFile, IntentResult};
use semilla_intent::{ClaimKind, Item};

use crate::equiv::Verdict;

use crate::{BodyOracle, StepInput, StepOutcome};

/// One claim for the oracle.
#[derive(Clone, Debug)]
pub struct Query {
    /// The number of the claim in the step (`i3`).
    pub id: String,
    pub file: Arc<semilla_intent::File>,
    pub claim: semilla_intent::Claim,
    /// Resolve the references and check the types only (`semilla intent check`).
    pub dry: bool,
}

/// The answer of the oracle for one claim.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub result: Option<IntentResult>,
    /// The method of a proof, the input of a false claim, or the cause of a claimed result.
    pub detail: String,
    /// The result of each configuration when they do not agree.
    pub configs: Vec<String>,
    /// The bodies that the claim reads with the hash of each: `before::PATH HASH`.
    pub reads: Vec<String>,
    /// The reference errors and the type errors: `FILE:LINE:COLUMN: message`.
    pub errors: Vec<String>,
    /// [R77.1-1] The dependence set of a proved claim: each body whose result the proof uses,
    /// `before::NAME` or `after::NAME` (the record names). Empty for each other result.
    pub depends: Vec<String>,
    /// The record names of the item of the claim in each version that has it: `before::NAME`,
    /// `after::NAME` (the first target, after the rename map and the loose path).
    pub items: Vec<String>,
    /// [R75.1-3] The suggested test of a false claim (design 75).
    pub tests: Vec<semilla_core::run::SuggestedTest>,
    /// [R80.3-2] The line of each item of a scope of more than one item (`impl`, a glob,
    /// `callers`).
    pub scope: Vec<String>,
}

/// [R69.5-1] The result of a claim with no answer of the oracle. The oracle answered the other
/// claims: no answer for this one is a defect of Semilla, never a result that comes later. No
/// answer at all: phase 1, and phase 2 gives the result.
pub fn no_answer(answered: bool) -> (IntentResult, &'static str) {
    match answered {
        true => (IntentResult::Claimed, NO_ANSWER),
        false => (IntentResult::Pending, "the compiled check of the step gives the result (phase 2)"),
    }
}

/// [R69.5-1] The cause of a claim with no answer of the compiled check of its step.
pub const NO_ANSWER: &str = "the compiled check gave no result for this claim (a defect of Semilla)";

/// Parses the intent files of a step.
pub fn parse_files(files: &[IntentFile]) -> Result<Vec<semilla_intent::File>, Vec<semilla_intent::Error>> {
    let mut parsed = Vec::new();
    let mut errors = Vec::new();
    for file in files {
        match semilla_intent::parse_file(&file.path, &file.text) {
            Ok(found) => parsed.push(found),
            // [R78.3-5] A file in a form of the compiled language: its claims come from the
            // heads, and rustc checks it in the step (design 78). A file that the Rust parser
            // cannot read keeps its errors.
            Err(found) if ra_parse_clean(&file.text) => {
                let _ = found;
                parsed.push(semilla_intent::heads::as_file(&file.path, &file.text));
            }
            Err(mut found) => errors.append(&mut found),
        }
    }
    if errors.is_empty() { Ok(parsed) } else { Err(errors) }
}

/// True if the Rust parser reads the file with no error, and no head has an error of Semilla.
fn ra_parse_clean(text: &str) -> bool {
    let (_, findings) = semilla_intent::heads::check("f.rs", text);
    semilla_intent::parse::rust_parses(text) && findings.iter().all(|finding| !finding.error)
}

/// The text of the `--same` fact of a claim of the forms of `--same` (R67.4-1).
pub fn same_text(claim: &semilla_intent::Claim) -> Option<String> {
    let [from, to] = claim.targets.as_slice() else { return None };
    Some(match claim.kind {
        ClaimKind::Rename => format!("rename {} {}", from.name(), to.name()),
        ClaimKind::Moved => format!("move {} {}", from.name(), to.name()),
        ClaimKind::Rewrite => format!("rewrite {} => {}", from.name(), to.name()),
        _ => return None,
    })
}

/// The `--same` facts of the claims of the intent files of a step: the verifier adds them to
/// the facts of the agent, so each one gets the check of design 48.
pub(crate) fn same_facts(files: &[IntentFile]) -> Vec<String> {
    let Ok(parsed) = parse_files(files) else { return Vec::new() };
    parsed.iter().flat_map(|file| file.claims().filter_map(same_text).collect::<Vec<_>>()).collect()
}

/// The `--change` facts of the `#[change(ITEM)]` claims of a step (R67.4-3): a change claim
/// states the change of its item, so the item is not in the gap list, and the item must have
/// an edit that can change the behavior (design 54.2).
pub(crate) fn change_facts(files: &[IntentFile]) -> Vec<String> {
    let Ok(parsed) = parse_files(files) else { return Vec::new() };
    let mut out = Vec::new();
    for file in &parsed {
        for claim in file.claims().filter(|claim| claim.kind == ClaimKind::Change) {
            for target in &claim.targets {
                out.push(format!("{}=the claim `fn {}` of the intent file `{}`", target.name(), claim.func.name, file.path));
            }
        }
    }
    out
}

/// The options of a claim, as the result line shows them.
fn option_texts(claim: &semilla_intent::Claim) -> Vec<String> {
    let mut list = Vec::new();
    if let Some(cfg) = &claim.options.cfg {
        list.push(format!("cfg = {cfg:?}"));
    }
    if let Some(reason) = &claim.options.reason {
        list.push(format!("reason = {reason:?}"));
    }
    if !claim.options.except.is_empty() {
        list.push(format!("except = [{}]", claim.options.except.join(", ")));
    }
    list
}

/// [R69.5-1] The id of each item of the intent files that gets a line (`i1`, `i2`, …): an
/// `intent!` text and each claim. A relation (a function with no attribute) gets no id. The
/// step and `semilla intent check` use this one numbering, so an answer of the oracle is the
/// answer of the claim with its id.
pub fn item_ids(files: &[semilla_intent::File]) -> Vec<Vec<Option<String>>> {
    let mut number = 0;
    files
        .iter()
        .map(|file| {
            file.items
                .iter()
                .map(|item| {
                    if matches!(item, Item::Relation(_)) {
                        return None;
                    }
                    number += 1;
                    Some(format!("i{number}"))
                })
                .collect()
        })
        .collect()
}

/// The claims of a step: the ids and the queries for the oracle.
pub fn queries(files: &[semilla_intent::File], dry: bool) -> Vec<Query> {
    let mut out = Vec::new();
    let ids = item_ids(files);
    for (file, ids) in files.iter().zip(&ids) {
        let shared = Arc::new(file.clone());
        for (item, id) in file.items.iter().zip(ids) {
            let Some(id) = id else { continue };
            if let Item::Claim(claim) = item
                && matches!(claim.kind, ClaimKind::Same | ClaimKind::Change | ClaimKind::Different)
            {
                out.push(Query { id: id.clone(), file: shared.clone(), claim: claim.clone(), dry });
            }
        }
    }
    out
}

/// [R77.1-1] The check of the claims of a step, before the gap list: the oracle answers each
/// claim, and the coverage of the changed bodies gives the items that proved claims explain, so
/// they leave the gap list (R77.1-3).
pub(crate) fn check(input: &StepInput, bodies: &dyn BodyOracle, outcome: &mut StepOutcome) {
    let files = &input.step.agent.intent;
    // [R84.2-1] A step with no intent file has the coverage too: each changed body is "equal
    // MIR" or "MIR changes with no claim" with its template.
    if files.is_empty() {
        let (cover, pending) = coverage(input, outcome, &[], None);
        outcome.report.coverage = cover;
        outcome.templates.merge(pending);
        return;
    }
    let Ok(parsed) = parse_files(files) else { return };
    let renames: BTreeMap<String, String> = outcome.renames.items.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
    let asked = queries(&parsed, false);
    let answers = if asked.is_empty() { None } else { bodies.intent(input.step, &renames, &asked) };
    let later = later_claims(&parsed);
    let (cover, pending) = coverage_with(input, outcome, &asked, answers.as_deref(), &later);
    outcome.templates.merge(pending);
    let index = crate::checks::BodyIndex::new(&outcome.bodies, input.after, input.before);
    outcome.explained = cover.iter().filter(|body| body.state == CoverState::Claim).filter_map(|body| index.item_of_name(&body.name)).collect();
    outcome.report.coverage = cover;
    outcome.intent_answers = Some((asked, answers));
}

/// The final result of a claim from the answer of the oracle: an error of a reference or a type
/// is an error, and an `except` name that the check does not know makes a proof or a false
/// result "claimed" (R67.3-8).
fn final_result(claim: &semilla_intent::Claim, answer: Option<&Answer>) -> Option<IntentResult> {
    let answer = answer?;
    if !answer.errors.is_empty() {
        return Some(IntentResult::Error);
    }
    let result = answer.result.unwrap_or(IntentResult::Claimed);
    let unknown = claim.options.except.iter().any(|name| !semilla_intent::prelude::EXCEPT.contains(&name.as_str()));
    Some(if unknown && matches!(result, IntentResult::Proved | IntentResult::False) { IntentResult::Claimed } else { result })
}

/// The name of a body of the section "control flow" for a record name of a version
/// (`before::NAME`, `after::NAME`): an old name goes through the rename map of the step.
fn flow_name(text: &str, renames: &BTreeMap<String, String>, names: &std::collections::BTreeSet<&str>) -> String {
    if let Some(name) = text.strip_prefix("after::") {
        return name.to_string();
    }
    let name = text.strip_prefix("before::").unwrap_or(text);
    if names.contains(name) && !renames.contains_key(name) {
        return name.to_string();
    }
    renames.get(name).cloned().unwrap_or_else(|| name.to_string())
}

/// [R77.1-2] The coverage of the changed bodies of a step (design 77): the state of each body of
/// the section "control flow". A body is explained only by a proved claim that depends on it
/// (R77.1-1) or by an equivalence proof of Semilla; a disagreement explains nothing (R77.1-4).
fn coverage(input: &StepInput, outcome: &StepOutcome, asked: &[Query], answers: Option<&[Answer]>) -> (Vec<BodyCover>, crate::template::Pending) {
    coverage_with(input, outcome, asked, answers, &[])
}

/// [R87.4-1] The claims of the intent files that the model checks later and that name items
/// (`#[new]`, `#[removed]`): the id, the attribute, the batch, and the names of the targets.
fn later_claims(files: &[semilla_intent::File]) -> Vec<(String, String, String, Vec<String>)> {
    let mut out = Vec::new();
    for (file, ids) in files.iter().zip(item_ids(files)) {
        for (item, id) in file.items.iter().zip(ids) {
            let (Item::Claim(claim), Some(id)) = (item, id) else { continue };
            if claim.kind != ClaimKind::Later || !matches!(claim.attribute.as_str(), "new" | "removed") {
                continue;
            }
            let batch = semilla_intent::heads::KINDS.iter().find(|kind| kind.name == claim.attribute).and_then(|kind| kind.later).unwrap_or("of a later version");
            out.push((id, claim.attribute.clone(), batch.to_string(), claim.targets.iter().map(|target| semilla_intent::plain_name(&target.name())).collect()));
        }
    }
    out
}

fn coverage_with(input: &StepInput, outcome: &StepOutcome, asked: &[Query], answers: Option<&[Answer]>, later: &[(String, String, String, Vec<String>)]) -> (Vec<BodyCover>, crate::template::Pending) {
    let mut pending = crate::template::Pending::default();
    let Some(flow) = &outcome.report.flow else { return (Vec::new(), pending) };
    let renames: BTreeMap<String, String> = outcome.renames.items.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
    let names: std::collections::BTreeSet<&str> = flow.bodies.iter().map(|body| body.name.as_str()).collect();
    // The proved claims: the id, the kind, the bodies that each one explains, and its item.
    struct Proved<'a> {
        id: &'a str,
        kind: ClaimKind,
        explains: Vec<String>,
        items: Vec<String>,
        /// The claim ignores an observation (`except = […]`): it states less than an equal
        /// behavior, so a difference of Semilla does not contradict it.
        except: bool,
    }
    let mut proved: Vec<Proved> = Vec::new();
    // [R84.2-1] The bodies of the items that a claim names (each result): a body of such an item
    // is not "MIR changes with no claim".
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for (at, _) in asked.iter().enumerate() {
        if let Some(answer) = answers.and_then(|answers| answers.get(at)) {
            claimed.extend(answer.items.iter().map(|name| flow_name(name, &renames, &names)));
        }
    }
    for (at, query) in asked.iter().enumerate() {
        let answer = answers.and_then(|answers| answers.get(at));
        // [R80.3-2] A claim on a scope with a claimed item: each proved item explains its bodies.
        let partial = answer.is_some_and(|answer| !answer.scope.is_empty() && !answer.depends.is_empty()) && final_result(&query.claim, answer) == Some(IntentResult::Claimed);
        if final_result(&query.claim, answer) != Some(IntentResult::Proved) && !partial {
            continue;
        }
        let Some(answer) = answer else { continue };
        let mut explains: Vec<String> = answer.depends.iter().map(|name| flow_name(name, &renames, &names)).filter(|name| names.contains(name.as_str())).collect();
        explains.sort();
        explains.dedup();
        let items: Vec<String> = answer.items.iter().map(|name| flow_name(name, &renames, &names)).collect();
        proved.push(Proved { id: &query.id, kind: query.claim.kind, explains, items, except: !query.claim.options.except.is_empty() });
    }
    let index = crate::checks::BodyIndex::new(&outcome.bodies, input.after, input.before);
    let claimed_items: BTreeSet<String> = claimed.iter().map(|name| index.item_of_name(name).unwrap_or_else(|| name.clone())).collect();
    let mut templates: BTreeMap<String, String> = BTreeMap::new();
    let mut out = Vec::new();
    for body in &flow.bodies {
        // [R79.7-1] The map is a review aid, not a gate of the proof: the state of the body comes
        // from the proof alone, and the fact of the map stands next to it.
        let item = index.item_of_name(&body.name).unwrap_or_else(|| body.name.clone());
        // G4: a nested body (a closure, a constant block) is a part of its item: the template
        // of the item covers it.
        let item = match item.split_once("::{") {
            // [R87.2-1] The item of the owner body in the syntax tree.
            Some((owner, _)) if !owner.is_empty() => index.item_of_name(owner).unwrap_or_else(|| owner.to_string()),
            _ => item,
        };
        let map = if outcome.not_mapped.contains(&item) {
            let count = outcome.report.ast.as_ref().map(|map| map.entries.iter().filter(|entry| entry.state == semilla_core::AstState::NotMapped && entry.at.split('/').next() == Some(item.as_str())).count()).unwrap_or(0);
            if count == 0 { "a node of its item is not mapped (complete the intent file)".to_string() } else { format!("{count} node(s) of its item not mapped (complete the intent file)") }
        } else {
            String::new()
        };
        let verdicts = outcome.verdicts.get(&body.name).cloned().unwrap_or_default();
        let equivalent = !verdicts.is_empty() && verdicts.iter().all(|(_, verdict)| matches!(verdict, Verdict::Equivalent(_)));
        let different = verdicts.iter().any(|(_, verdict)| matches!(verdict, Verdict::Different { .. }));
        let semilla = if verdicts.is_empty() {
            if body.line.starts_with("graph added") {
                "an added body: no compare of two versions".to_string()
            } else if body.line.starts_with("graph removed") {
                "a removed body: no compare of two versions".to_string()
            } else {
                "no result of the equivalence check".to_string()
            }
        } else {
            crate::equiv::joined(&verdicts, false, false)
        };
        let on_item = |kinds: &[ClaimKind]| -> Vec<&str> { proved.iter().filter(|claim| kinds.contains(&claim.kind) && claim.items.contains(&body.name) && !(claim.kind == ClaimKind::Same && claim.except)).map(|claim| claim.id).collect() };
        // [R77.1-4] The two results cannot both be true: a proved claim that states a change of
        // the body and an equivalence proof of Semilla, or a proved `same` claim on the body with
        // no claim of a change and a difference of Semilla.
        let mut disagree: Vec<&str> = Vec::new();
        if equivalent {
            disagree = on_item(&[ClaimKind::Change, ClaimKind::Different]);
        } else if different && on_item(&[ClaimKind::Change]).is_empty() {
            disagree = on_item(&[ClaimKind::Same]);
        }
        let explaining: Vec<String> = proved.iter().filter(|claim| claim.explains.contains(&body.name)).map(|claim| claim.id.to_string()).collect();
        let (state, claims, detail) = if !disagree.is_empty() {
            (CoverState::Disagree, disagree.iter().map(|id| id.to_string()).collect(), format!("Semilla: {semilla}; claim {}: proved", disagree.join(", ")))
        } else if !explaining.is_empty() {
            (CoverState::Claim, explaining, String::new())
        } else if equivalent {
            (CoverState::Equivalent, Vec::new(), String::new())
        } else if claimed.contains(&body.name) || claimed_items.contains(&item) {
            (CoverState::NotExplained, Vec::new(), semilla)
        } else if let Some((id, kind, batch, _)) = later.iter().find(|(_, _, _, targets)| {
            let names = [semilla_intent::plain_name(&item), semilla_intent::plain_name(body.name.split("::{").next().unwrap_or(&body.name))];
            targets.iter().any(|target| names.contains(target))
        }) {
            // [R87.4-1] A claim names the body, and the model checks its kind later.
            (CoverState::Later, vec![id.clone()], format!("claim {id} (`#[{kind}]`): the check of `#[{kind}]` comes in batch {batch}"))
        } else {
            // [R84.2-1] One line with the body and a claim template (design 84).
            let cause = if body.line.starts_with("graph added") || body.line.starts_with("graph removed") { String::new() } else if verdicts.is_empty() { semilla } else { crate::equiv::joined(&verdicts, false, false).trim_start_matches("MIR changes").trim_start_matches(": ").to_string() };
            let mut offer = match templates.get(&item) {
                Some(first) => crate::template::Offer { none: None, what: String::new(), groups: vec![crate::template::Group { label: String::new(), templates: Vec::new(), note: format!("the template of `{first}` covers its item `{item}`") }], ..Default::default() },
                None => {
                    templates.insert(item.clone(), body.name.clone());
                    body_template(input, outcome, &renames, &item, body, &mut pending)
                }
            };
            offer.cause = cause;
            let detail = offer.render(&|_| None, &|_| true);
            pending.offers.push((out.len(), offer));
            (CoverState::NoClaim, Vec::new(), detail)
        };
        out.push(BodyCover { name: body.name.clone(), place: body.place.clone(), state, claims, detail, map });
    }
    (out, pending)
}

/// The claim lines of the step (design 67, part D).
pub(crate) fn apply(input: &StepInput, bodies: &dyn BodyOracle, outcome: &mut StepOutcome) {
    let files = &input.step.agent.intent;
    if files.is_empty() {
        return;
    }
    let parsed = match parse_files(files) {
        Ok(parsed) => parsed,
        Err(errors) => {
            outcome.report.intent_claims = errors
                .iter()
                .enumerate()
                .map(|(index, error)| IntentClaim { id: format!("e{}", index + 1), kind: "file".into(), text: error.place.file.clone(), place: format!("{}:{}", error.place.file, error.place.line), result: IntentResult::Error, detail: error.to_string(), configs: Vec::new(), options: Vec::new(), reads: Vec::new(), stale: String::new(), explains: Vec::new() })
                .collect();
            return;
        }
    };
    let renames: BTreeMap<String, String> = outcome.renames.items.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
    let (asked, answers) = match outcome.intent_answers.take() {
        Some(found) => found,
        None => {
            let asked = queries(&parsed, false);
            let answers = if asked.is_empty() { None } else { bodies.intent(input.step, &renames, &asked) };
            (asked, answers)
        }
    };
    // The stored reads of the last check: `ID before::PATH HASH`.
    let stored: BTreeMap<&str, Vec<&str>> = input.step.agent.intent_reads.iter().filter_map(|line| line.split_once(' ')).fold(BTreeMap::new(), |mut map, (id, read)| {
        map.entry(id).or_insert_with(Vec::new).push(read);
        map
    });
    let mut claims = Vec::new();
    let ids = item_ids(&parsed);
    for (file, ids) in parsed.iter().zip(&ids) {
        for (item, id) in file.items.iter().zip(ids) {
            let id = id.clone().unwrap_or_default();
            let mut line = |kind: &str, text: String, place: &semilla_intent::Place, result: IntentResult, detail: String, options: Vec<String>| {
                claims.push(IntentClaim { id: id.clone(), kind: kind.into(), text, place: format!("{}:{}", place.file, place.line), result, detail, configs: Vec::new(), options, reads: Vec::new(), stale: String::new(), explains: Vec::new() });
            };
            match item {
                Item::Intent(text, place) => line("intent", text.clone(), place, IntentResult::Shown, String::new(), Vec::new()),
                Item::Relation(_) => {}
                Item::Claim(claim) => {
                    let options = option_texts(claim);
                    match claim.kind {
                        ClaimKind::Rename | ClaimKind::Moved | ClaimKind::Rewrite => {
                            let Some(text) = same_text(claim) else { continue };
                            let fact = outcome.report.agent_facts.iter().find(|fact| fact.kind == "same" && fact.options.iter().any(|option| *option == text));
                            // [R77.4-1] A form of `--same` is a hint for the pairing: its check holds or
                            // fails as before, and it explains no body.
                            let (result, detail) = match fact {
                                Some(fact) if fact.holds => (IntentResult::Hint, format!("the check of `--same '{text}'` holds (fact {} of the agent: {}); a hint pairs the old and the new code and explains no body: a claim states the behavior", fact.id, fact.result)),
                                Some(fact) => (IntentResult::False, format!("the check of `--same '{text}'` fails (fact {} of the agent: {})", fact.id, fact.result)),
                                None => (IntentResult::Claimed, format!("no result of `--same '{text}'`")),
                            };
                            line(claim.kind.name(), claim.text(), &claim.place, result, detail, options);
                        }
                        // [R80.1-1] A relation of two types is a hint: the claims use it where
                        // the types of the two versions differ, and it explains no body alone.
                        ClaimKind::Later if claim.attribute == RELATION => {
                            line(RELATION, claim.text(), &claim.place, IntentResult::Hint, "a relation of an old type and a new type: each claim uses it where the types of the two versions differ, and each use is checked; it proves nothing alone".into(), options);
                        }
                        ClaimKind::Later => {
                            // [R78.3-5] The batch of draft 3 of the language (the heads), else of 10r1.
                            let batch = None.or_else(|| semilla_intent::heads::KINDS.iter().find(|kind| kind.name == claim.attribute).and_then(|kind| kind.later)).or_else(|| semilla_intent::prelude::CLAIMS.iter().find(|(name, _)| *name == claim.attribute).map(|(_, batch)| *batch)).unwrap_or("later");
                            line(&claim.attribute, claim.text(), &claim.place, IntentResult::Claimed, format!("the check of `#[{}]` comes in batch {batch}", claim.attribute), options);
                        }
                        ClaimKind::Same | ClaimKind::Change | ClaimKind::Different => {
                            let at = asked.iter().position(|query| query.id == id);
                            let found = at.and_then(|at| answers.as_ref().and_then(|answers| answers.get(at)).cloned());
                            // [R67.3-8] An `except` name that the check does not know makes the
                            // claim "claimed": the check cannot ignore that observation.
                            let unknown: Vec<&String> = claim.options.except.iter().filter(|name| !semilla_intent::prelude::EXCEPT.contains(&name.as_str())).collect();
                            // [R77.1-1] The changed bodies that the proof explains (the coverage).
                            let explains: Vec<String> = outcome.report.coverage.iter().filter(|body| body.state == CoverState::Claim && body.claims.contains(&id)).map(|body| body.name.clone()).collect();
                            let (result, detail, configs, reads, errors) = match found {
                                Some(answer) => {
                                    // [R75.1-3] The test of a false claim goes to the list of the step.
                                    for test in &answer.tests {
                                        let flow = outcome.report.flow.get_or_insert_with(Default::default);
                                        crate::equiv::merge_test(&mut flow.tests, test);
                                    }
                                    // [R80.3-2] The line of each item of a scope comes first.
                                    (answer.result.unwrap_or(IntentResult::Claimed), answer.detail, answer.scope.into_iter().chain(answer.configs).collect(), answer.reads, answer.errors)
                                }
                                None => {
                                    let (result, detail) = no_answer(answers.is_some());
                                    (result, detail.into(), Vec::new(), Vec::new(), Vec::new())
                                }
                            };
                            // A cause with the place of a part in a body (design 61.10, C2).
                            let detail = crate::equiv::cause_text(&detail, input.before, input.after).0;
                            let configs: Vec<String> = configs.iter().map(|line| crate::equiv::cause_text(line, input.before, input.after).0).collect();
                            let (result, detail) = if !errors.is_empty() {
                                (IntentResult::Error, errors.join("; "))
                            } else if !unknown.is_empty() && matches!(result, IntentResult::Proved | IntentResult::False) {
                                // The check cannot ignore that observation, and a difference can
                                // be a difference of that observation.
                                (IntentResult::Claimed, format!("the check does not know the observation {} of `except`; the check without it: {} ({detail})", unknown.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", "), result.word()))
                            } else {
                                (result, detail)
                            };
                            // [R67.5-2] A claim whose bodies changed since the last check is
                            // stale before this check.
                            let before: Vec<&str> = stored.get(id.as_str()).cloned().unwrap_or_default();
                            let changed: Vec<String> = reads
                                .iter()
                                .filter(|read| {
                                    let path = read.rsplit_once(' ').map(|(path, _)| path).unwrap_or(read);
                                    before.iter().any(|old| old.rsplit_once(' ').map(|(known, _)| known) == Some(path)) && !before.contains(&read.as_str())
                                })
                                .map(|read| format!("`{}`", read.rsplit_once(' ').map(|(path, _)| path).unwrap_or(read)))
                                .collect();
                            claims.push(IntentClaim {
                                id: id.clone(),
                                kind: claim.kind.name().into(),
                                text: claim.text(),
                                place: format!("{}:{}", claim.place.file, claim.place.line),
                                result,
                                detail,
                                configs,
                                options,
                                reads,
                                stale: if changed.is_empty() { String::new() } else { format!("the body of {} changed", changed.join(", ")) },
                                explains,
                            });
                        }
                    }
                }
            }
        }
    }
    // [R79.2-2] An anchor that does not fit is an error of its claim (the place and the expected
    // kind), so the step rejects it as each other error of a claim (R67.5-2).
    if let Some(map) = &outcome.report.ast {
        for claim in &mut claims {
            let found: Vec<&str> = map.errors.iter().filter_map(|error| error.strip_prefix(&format!("{} ", claim.id))).collect();
            if !found.is_empty() {
                claim.result = IntentResult::Error;
                claim.detail = found.join("; ");
            }
        }
    }
    outcome.report.intent_claims = claims;
}

/// [R84.2-2] The claim template of a body with "MIR changes" and no claim (design 84): an empty
/// `#[same(ITEM)]` claim asks for the full compare of the item (design 78), and a `#[change]` or
/// `#[different]` claim states a difference; the agent writes its body in place of `…`. An item
/// whose signature changes gets the property form of 13a in place of the empty claim (an empty
/// claim cannot compare two signatures); an added or a removed body gets `#[new]` or
/// `#[removed]`. If the AST pass prints a template for a node of the item (R79.5-1), the line
/// names that node and gives no second copy. [R87.2-1] Each path comes from the names of the
/// item (`template::item_names`), and [R87.1-1] the gate checks each template before the step
/// prints it.
fn body_template(input: &StepInput, outcome: &StepOutcome, renames: &BTreeMap<String, String>, item: &str, body: &semilla_core::report::FlowBody, pending: &mut crate::template::Pending) -> crate::template::Offer {
    use crate::template::{Group, Offer};
    let none = |cause: String| Offer { none: Some(cause), ..Default::default() };
    // The template of the AST pass for a node of the item, if the pass gives one.
    let ast = outcome.report.ast.as_ref().and_then(|map| {
        let at = map.entries.iter().position(|entry| entry.state == semilla_core::AstState::NotMapped && entry.at.split('/').next() == Some(item) && !entry.template.is_empty() && !entry.template.starts_with("no template"))?;
        Some((at, format!("the claim template of the node n{} of the AST map ({})", at + 1, semilla_core::budget::step_text_command(outcome.report.step))))
    });
    if item.contains(" (bin \"") {
        return none("the item is in a binary target, and the intent crate links the libraries only (design 78)".into());
    }
    // R79.3-2: no claim names test, bench, or example code (the intent crate links the libraries).
    if crate::astmap::test_code(input.after, item) || crate::astmap::test_code(input.before, item) {
        return none("test, bench, or example code (the intent crate links the libraries only, design 78)".into());
    }
    let reverse = renames.iter().find(|(_, new)| new.as_str() == item).map(|(old, _)| old.clone()).unwrap_or_else(|| item.to_string());
    let found = [input.before.items.get(&reverse), input.after.items.get(item)];
    // [R87.2-1] The names of the item in each tree that has it: the name record of the compiler.
    let record = body.name.split("::{").next().unwrap_or(&body.name);
    let names = match found {
        [None, None] => [Some(crate::template::made_names(input.before, record)), Some(crate::template::made_names(input.after, record))],
        _ => [found[0].map(|_| crate::template::item_names(input.before, &reverse)), found[1].map(|_| crate::template::item_names(input.after, item))],
    };
    let usable: Vec<&crate::template::ItemNames> = names.iter().flatten().filter_map(|found| found.as_ref().ok()).collect();
    let Some(first) = usable.last().copied() else {
        // [R87.2-2] No path of the language names the body: a gap of the intent language.
        let cause = names.iter().flatten().find_map(|found| found.as_ref().err().cloned()).unwrap_or_else(|| format!("`{}` is no item of the syntax tree (a body that the compiler makes)", body.name));
        return match ast {
            Some(ast) => Offer { ast: Some(ast), none: Some(cause), ..Default::default() },
            None => none(cause),
        };
    };
    let path = first.scope.clone();
    // [R87.2-2] A pair of two paths (a rename, a twin pair): one scope path names one item in
    // the two versions, so no scope path names the pair.
    if let [Some(Ok(old)), Some(Ok(new))] = &names
        && old.scope != new.scope
    {
        let cause = crate::template::pair_gap(&old.scope, &new.scope);
        return match ast {
            Some(ast) => Offer { ast: Some(ast), none: Some(cause), ..Default::default() },
            None => none(cause),
        };
    }
    // The record names that the scope path must resolve to (the property of R87.2-1).
    let expect = |kinds: &[usize]| -> Vec<String> { kinds.iter().filter_map(|&side| names[side].as_ref().and_then(|found| found.as_ref().ok()).map(|found| format!("{}::{}", if side == 0 { "before" } else { "after" }, found.expect))).collect() };
    let both = expect(&[0, 1]);
    let short: String = path.rsplit("::").next().unwrap_or(&path).chars().map(|c| if c.is_alphanumeric() || c == '_' { c.to_ascii_lowercase() } else { '_' }).collect();
    let signature = |item: &semilla_syntax::Item| item.signature.split_whitespace().collect::<Vec<_>>().join(" ");
    let is_fn = |item: Option<&semilla_syntax::Item>| item.is_some_and(|item| item.kind == semilla_syntax::ItemKind::Fn);
    let plain_change = format!("#[change({path})] fn {short}_change() -> bool {{ … }}");
    let different = format!("#[different({path})] fn {short}_different() -> bool {{ … }}");
    let full = format!("#[same({path})] fn {short}_same() {{}}");
    // [R87.1-1] Each template with its forms: the written paths of the types of other crates
    // first, then their definition paths.
    let forms = |make: &dyn Fn(bool) -> Result<String, String>| -> Result<Vec<String>, String> {
        let first = make(false)?;
        let mut list = vec![first];
        if let Ok(second) = make(true)
            && !list.contains(&second)
        {
            list.push(second);
        }
        Ok(list)
    };
    let old_names = names[0].as_ref().and_then(|found| found.as_ref().ok());
    let new_names = names[1].as_ref().and_then(|found| found.as_ref().ok());
    let mut note = |forms: Vec<String>, expect: Vec<String>| pending.note(forms, expect);
    let difference = |note: &mut dyn FnMut(Vec<String>, Vec<String>) -> String, change: Vec<String>| Group { label: "a difference".into(), templates: vec![note(change, both.clone()), note(vec![different.clone()], both.clone())], note: String::new() };
    let mut offer = Offer { ast: ast.clone(), ..Default::default() };
    match (found[0], found[1]) {
        (Some(a), Some(b)) if is_fn(Some(a)) && is_fn(Some(b)) => {
            let (old, new) = (signature(a), signature(b));
            // [R69.5-3] Two equal signature texts: a type of the signature (or the type of
            // `self`) can change its definition.
            let types = if old == new { changed_types(input, &reverse, item, &new) } else { Vec::new() };
            offer.what = if old != new {
                format!("the signature changes (`{old}` → `{new}`)")
            } else if !types.is_empty() {
                format!("a type of the signature changes ({})", types.join("; "))
            } else {
                String::new()
            };
            let property = |real: bool| -> Result<String, String> {
                let (Some(old_names), Some(new_names)) = (old_names, new_names) else { return Err("the item has no names in one version".into()) };
                let (on, nn) = (crate::template::signature_names(input.before, old_names, real), crate::template::signature_names(input.after, new_names, real));
                semilla_intent::parse::same_template_named(&path, (&old_names.text, &on), (&new_names.text, &nn))
            };
            if offer.what.is_empty() {
                // A change form with the parameters of the item: the template of 13a with the
                // body of the agent.
                let change = forms(&|real| {
                    let text = property(real)?;
                    let head = &text[..text.find(" -> bool {").ok_or("no head")?];
                    let open = head.find('(').ok_or("no scope")? + 1;
                    let params = &head[head[open..].find('(').map(|next| open + next).ok_or("no parameters")?..];
                    Ok(format!("#[change({path})] fn {short}_change{params} -> bool {{ … }}"))
                })
                .unwrap_or_else(|_| vec![plain_change.clone()]);
                offer.groups.push(Group { label: "the full compare".into(), templates: vec![note(vec![full.clone()], both.clone())], note: String::new() });
                offer.groups.push(difference(&mut note, change));
            } else {
                // An empty claim cannot compare two signatures: the property form of 13a.
                match forms(&property) {
                    Ok(list) => offer.groups.push(Group { label: String::new(), templates: vec![note(list, both.clone())], note: String::new() }),
                    Err(cause) => offer.groups.push(Group { label: String::new(), templates: Vec::new(), note: format!("no property template ({cause})") }),
                }
                offer.groups.push(difference(&mut note, vec![plain_change.clone()]));
            }
        }
        (None, Some(b)) if is_fn(Some(b)) => {
            offer.what = "an added body".into();
            let list = new_names.map(|found| forms(&|real| semilla_intent::parse::version_template_named("new", &path, &found.text, &crate::template::signature_names(input.after, found, real)))).and_then(Result::ok).unwrap_or_else(|| vec![format!("#[new({path})] fn {short}_new() -> bool {{ … }}")]);
            let _ = signature(b);
            offer.groups.push(Group { label: String::new(), templates: vec![note(list, expect(&[1]))], note: String::new() });
        }
        (Some(a), None) if is_fn(Some(a)) => {
            offer.what = "a removed body".into();
            let path = old_names.map(|found| found.scope.clone()).unwrap_or(path.clone());
            let list = old_names.map(|found| forms(&|real| semilla_intent::parse::version_template_named("removed", &path, &found.text, &crate::template::signature_names(input.before, found, real)))).and_then(Result::ok).unwrap_or_else(|| vec![format!("#[removed({path})] fn {short}_removed() -> bool {{ … }}")]);
            let _ = signature(a);
            offer.groups.push(Group { label: String::new(), templates: vec![note(list, expect(&[0]))], note: String::new() });
        }
        _ => {
            offer.groups.push(Group { label: "the full compare".into(), templates: vec![note(vec![full.clone()], both.clone())], note: String::new() });
            offer.groups.push(difference(&mut note, vec![plain_change.clone()]));
        }
    }
    offer
}

/// [R69.5-3] The types that a signature names (and the type of `self` of a method) whose
/// definition changes in the step, with the fields that change: "the type of `self` changes:
/// `shop::Pt`: `x: u32` → `x: u64`".
fn changed_types(input: &StepInput, old_path: &str, new_path: &str, signature: &str) -> Vec<String> {
    let words: std::collections::BTreeSet<&str> = signature.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|word| !word.is_empty()).collect();
    let owner = new_path.rsplit_once("::").map(|(owner, _)| owner).unwrap_or("");
    let old_owner = old_path.rsplit_once("::").map(|(owner, _)| owner).unwrap_or("");
    let mut out = Vec::new();
    for (path, item) in &input.after.items {
        if !matches!(item.kind, semilla_syntax::ItemKind::Struct | semilla_syntax::ItemKind::Enum | semilla_syntax::ItemKind::Type) {
            continue;
        }
        let short = path.rsplit("::").next().unwrap_or(path);
        let is_self = path == owner && signature.contains("self");
        if !is_self && !words.contains(short) {
            continue;
        }
        let before_path = if is_self { old_owner } else { path.as_str() };
        let Some(before) = input.before.items.get(before_path) else { continue };
        if before.text == item.text {
            continue;
        }
        // The parts that change: each field with another declaration, each added field, and
        // each removed field.
        let field = |workspace: &semilla_syntax::Workspace, owner: &str, member: &str| workspace.items.get(&format!("{owner}.{member}")).map(|field| format!("{member}: {}", field.signature.split_whitespace().collect::<Vec<_>>().join(" ")));
        let mut parts = Vec::new();
        for member in &before.members {
            match (field(input.before, before_path, member), item.members.contains(member).then(|| field(input.after, path, member)).flatten()) {
                (Some(old), Some(new)) if old != new => parts.push(format!("`{old}` → `{new}`")),
                (Some(old), None) => parts.push(format!("− `{old}`")),
                _ => {}
            }
        }
        for member in item.members.iter().filter(|member| !before.members.contains(member)) {
            if let Some(new) = field(input.after, path, member) {
                parts.push(format!("+ `{new}`"));
            }
        }
        if parts.is_empty() {
            parts.push(format!("`{}` → `{}`", before.signature.split_whitespace().collect::<Vec<_>>().join(" "), item.signature.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
        let role = if is_self { "the type of `self`".to_string() } else { format!("the type `{short}`") };
        out.push(format!("{role} changes: `{path}`: {}", parts.join(", ")));
    }
    out
}

/// The lines of the section "intent" of a step (design 67, part D).
pub fn lines(claims: &[IntentClaim]) -> Vec<String> {
    if claims.is_empty() {
        return Vec::new();
    }
    // [R77.4-1] A hint is not a claim: the count has the claims only.
    let hints = claims.iter().filter(|claim| is_hint(&claim.kind)).count();
    let hint_text = if hints == 0 { String::new() } else { format!(", {hints} hint(s)") };
    let mut lines = vec![format!("  intent ({} claim(s){hint_text}; a claim is a proof only if it is proved):", claims.iter().filter(|claim| claim.kind != "intent" && !is_hint(&claim.kind)).count())];
    for claim in claims {
        lines.push(format!("  {} {}", claim.result.mark(), claim.line()));
        for config in &claim.configs {
            lines.push(format!("      {config}"));
        }
        if let Some(help) = help(claim) {
            lines.push(format!("      {help}"));
        }
    }
    lines
}

/// [R77.1-2] The lines of the section "semantic changes" of a step with an intent file (design
/// 77): the codified pull request. The `intent!` texts, each proved `change` and `different`
/// claim (what changes), each proved `same` claim (what stays the same), each false claim with
/// its input, and the coverage of the changed bodies. With `limit`, the body lines show the
/// first lines (a disagreement first) and a count.
pub fn semantic_lines(step: &semilla_core::StepReport, limit: Option<usize>) -> Vec<String> {
    let claims = &step.intent_claims;
    if claims.is_empty() {
        // [R79.6-2] A step with no intent file: the AST pass runs (the map of Semilla), and the
        // output has its coverage line; the full text lists each node.
        let Some(map) = &step.ast else { return Vec::new() };
        if limit.is_some() {
            let mut lines = vec![format!("  {} (no intent file; the full text of the step lists each node)", map.line())];
            lines.extend(cover_lines(step, limit, "  "));
            return lines;
        }
        let mut lines = vec!["  semantic changes (no intent file: the map of the AST pass and the equal-MIR check):".to_string()];
        lines.extend(crate::astmap::lines(map, None, step.step));
        lines.extend(cover_lines(step, None, "    "));
        return lines;
    }
    let mut lines = vec!["  semantic changes (the claims that Semilla proves or refutes; a proved claim explains each changed body that its proof uses):".to_string()];
    for claim in claims.iter().filter(|claim| claim.kind == "intent") {
        lines.push(format!("    intent: {}", semilla_core::text::quoted(&claim.text)));
    }
    let explains = |claim: &IntentClaim| if claim.explains.is_empty() { " (explains no changed body)".to_string() } else { format!(" (explains {})", claim.explains.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")) };
    for claim in claims.iter().filter(|claim| claim.result == IntentResult::Proved && matches!(claim.kind.as_str(), "change" | "different")) {
        lines.push(format!("    ✓ changes: {} `{}`{}", claim.id, claim.text, explains(claim)));
    }
    for claim in claims.iter().filter(|claim| claim.result == IntentResult::Proved && claim.kind == "same") {
        lines.push(format!("    ✓ stays the same: {} `{}`{}", claim.id, claim.text, explains(claim)));
    }
    for claim in claims.iter().filter(|claim| claim.result == IntentResult::False) {
        lines.push(format!("    ✗ false: {} `{}`: {}", claim.id, claim.text, claim.detail));
    }
    // [R79.6-1] The coverage line of the AST pass, before the line of the changed bodies.
    if let Some(map) = &step.ast {
        lines.extend(crate::astmap::lines(map, limit, step.step));
    }
    if step.coverage.is_empty() {
        let cause = if step.flow.is_some() { "no changed body" } else { "the coverage needs the compiled check of the step (phase 2)" };
        lines.push(format!("    changed bodies: {cause}"));
        return lines;
    }
    lines.extend(cover_lines(step, limit, "    "));
    lines
}

/// [R84.6-1] The coverage line of the changed bodies and the line of each body (design 84), the
/// worst state first. With `limit`, the first lines (each disagreement) and a count.
fn cover_lines(step: &semilla_core::StepReport, limit: Option<usize>, indent: &str) -> Vec<String> {
    if step.coverage.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!("{indent}{}", semilla_core::report::coverage_line(&step.coverage))];
    let rank = |state: CoverState| match state {
        CoverState::Disagree => 0,
        CoverState::NotExplained => 1,
        CoverState::NoClaim => 2,
        CoverState::Later => 2,
        CoverState::Claim => 3,
        CoverState::Equivalent => 4,
    };
    let mut bodies: Vec<&BodyCover> = step.coverage.iter().collect();
    bodies.sort_by_key(|body| rank(body.state));
    let shown = limit.map(|most| most.max(bodies.iter().filter(|body| body.state == CoverState::Disagree).count())).unwrap_or(bodies.len());
    // R76.1-1: a long line of the short output is cut, and it names the full text of the step.
    let command = semilla_core::budget::step_text_command(step.step);
    for body in bodies.iter().take(shown) {
        let mark = if body.state == CoverState::Disagree { "✗ " } else { "" };
        let line = format!("{indent}  {mark}{}", body.line());
        lines.push(if limit.is_some() { crate::views::fit_line(&line, &command) } else { line });
    }
    if bodies.len() > shown {
        lines.push(format!("{indent}  … {} more (semilla status --verbose)", bodies.len() - shown));
    }
    lines
}

/// [R77.4-1] True for the kind of a hint of an intent file (a form of `--same`).
pub fn is_hint(kind: &str) -> bool {
    // [R80.1-1] A relation is a hint too: claims use it, and it proves nothing alone.
    [ClaimKind::Rename, ClaimKind::Moved, ClaimKind::Rewrite].iter().any(|hint| hint.name() == kind) || kind == RELATION
}

/// [R80.1-1] The attribute of a relation of two types.
pub const RELATION: &str = "relation";

/// The help for a claim result that an agent can fix.
fn help(claim: &IntentClaim) -> Option<String> {
    match claim.result {
        IntentResult::Error => Some("correct the claim, then run `semilla intent check FILE` (it checks the references and the types)".into()),
        IntentResult::False => Some("the input is an exact input of the two versions: correct the claim or the code".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "use semilla_intent::prelude::*;\n\nintent!(\"a text\");\n\nfn relation(x: u32) -> bool { before::shop::f(x) == after::shop::f(x) }\n\n#[same(shop::f)]\nfn claim(x: u32) -> bool { relation(x) }\n";

    // rule+ R69.5-1
    #[test]
    fn a_relation_gets_no_id_and_the_step_and_the_check_use_one_numbering() {
        let file = semilla_intent::parse_file("step.rs", FILE).unwrap();
        let ids = item_ids(std::slice::from_ref(&file));
        assert_eq!(ids, vec![vec![Some("i1".to_string()), None, Some("i2".to_string())]]);
        let asked = queries(std::slice::from_ref(&file), false);
        assert_eq!(asked.iter().map(|query| query.id.as_str()).collect::<Vec<_>>(), ["i2"]);
        // The oracle answered: a claim with no answer is never "pending".
        assert_eq!(no_answer(true), (IntentResult::Claimed, NO_ANSWER));
    }

    // rule- R69.5-1
    #[test]
    fn phase_one_of_a_compiled_step_keeps_a_claim_pending() {
        assert_eq!(no_answer(false).0, IntentResult::Pending);
    }
}
