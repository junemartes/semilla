//! The claims of the intent files of a step (design 67, parts C and D).
//!
//! The verifier parses each intent file of the step. A claim of the forms of `--same`
//! (`#[rename]`, `#[moved]`, `#[rewrite]`) is a `same` fact of the agent with the same check
//! (design 48). Each other claim goes to the oracle, which resolves its references against the
//! compiler data of each version and checks it (`semilla_smt::mir::claim`). The result of a
//! claim is a line of the step: it never changes a fact, a gap, or a compare result.

use std::collections::BTreeMap;
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
}

/// [R69.5-1] The result of a claim with no answer of the oracle. The oracle answered the other
/// claims: no answer for this one is a defect of Semilla, never a result that comes later. No
/// answer at all in a compiled step: phase 1, and phase 2 gives the result. The default mode:
/// the check needs `--compiled`.
pub fn no_answer(answered: bool, compiled: bool) -> (IntentResult, &'static str) {
    match (answered, compiled) {
        (true, _) => (IntentResult::Claimed, NO_ANSWER),
        (false, true) => (IntentResult::Pending, "the compiled check of the step gives the result (phase 2)"),
        (false, false) => (IntentResult::Claimed, NEEDS_COMPILED),
    }
}

/// [R69.5-1] The cause of a claim with no answer of the compiled check of its step.
pub const NO_ANSWER: &str = "the compiled check gave no result for this claim (a defect of Semilla)";

/// The cause of a property claim in the default mode.
pub const NEEDS_COMPILED: &str = "the check needs `--compiled` (the default mode has no compiler data)";

/// Parses the intent files of a step.
pub fn parse_files(files: &[IntentFile]) -> Result<Vec<semilla_intent::File>, Vec<semilla_intent::Error>> {
    let mut parsed = Vec::new();
    let mut errors = Vec::new();
    for file in files {
        match semilla_intent::parse_file(&file.path, &file.text) {
            Ok(found) => parsed.push(found),
            Err(mut found) => errors.append(&mut found),
        }
    }
    if errors.is_empty() { Ok(parsed) } else { Err(errors) }
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
    if files.is_empty() {
        return;
    }
    let Ok(parsed) = parse_files(files) else { return };
    let renames: BTreeMap<String, String> = outcome.renames.items.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
    let asked = queries(&parsed, false);
    let answers = if asked.is_empty() { None } else { bodies.intent(input.step, &renames, &asked) };
    let cover = coverage(input, outcome, &asked, answers.as_deref());
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
fn coverage(input: &StepInput, outcome: &StepOutcome, asked: &[Query], answers: Option<&[Answer]>) -> Vec<BodyCover> {
    let Some(flow) = &outcome.report.flow else { return Vec::new() };
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
    for (at, query) in asked.iter().enumerate() {
        let answer = answers.and_then(|answers| answers.get(at));
        if final_result(&query.claim, answer) != Some(IntentResult::Proved) {
            continue;
        }
        let Some(answer) = answer else { continue };
        let mut explains: Vec<String> = answer.depends.iter().map(|name| flow_name(name, &renames, &names)).filter(|name| names.contains(name.as_str())).collect();
        explains.sort();
        explains.dedup();
        let items: Vec<String> = answer.items.iter().map(|name| flow_name(name, &renames, &names)).collect();
        proved.push(Proved { id: &query.id, kind: query.claim.kind, explains, items, except: !query.claim.options.except.is_empty() });
    }
    let mut out = Vec::new();
    for body in &flow.bodies {
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
        } else {
            (CoverState::NotExplained, Vec::new(), semilla)
        };
        out.push(BodyCover { name: body.name.clone(), place: body.place.clone(), state, claims, detail });
    }
    let _ = input;
    out
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
    help_notes(input, &renames, outcome);
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
                        ClaimKind::Later => {
                            let batch = semilla_intent::prelude::CLAIMS.iter().find(|(name, _)| *name == claim.attribute).map(|(_, batch)| *batch).unwrap_or("later");
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
                                Some(answer) => (answer.result.unwrap_or(IntentResult::Claimed), answer.detail, answer.configs, answer.reads, answer.errors),
                                None => {
                                    let (result, detail) = no_answer(answers.is_some(), input.step.compiled);
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
    outcome.report.intent_claims = claims;
}

/// [R67.6-1] The help for a body whose equivalence check gives "not decided" (design 67, part
/// E): a cause that a property claim can close (the parameters or the return type change)
/// gives the two signatures and a template of a `#[same]` claim; a cause that no claim of
/// this batch can close says so. Only a step with an intent file gets the help: a step with no
/// intent file keeps its output.
fn help_notes(input: &StepInput, renames: &BTreeMap<String, String>, outcome: &mut StepOutcome) {
    let Some(flow) = &outcome.report.flow else { return };
    let index = crate::checks::BodyIndex::new(&outcome.bodies, input.after, input.before);
    // [R77.3-4] The path of a type of the crate by its name, for the template: the one struct,
    // enum, or type alias of the old version with that name.
    let type_path = |name: &str| -> Option<String> {
        let found: Vec<&String> = input.before.items.iter().filter(|(path, item)| matches!(item.kind, semilla_syntax::ItemKind::Struct | semilla_syntax::ItemKind::Enum | semilla_syntax::ItemKind::Type) && path.rsplit("::").next() == Some(name)).map(|(path, _)| path).collect();
        (found.len() == 1).then(|| found[0].clone())
    };
    let mut notes = Vec::new();
    for body in &flow.bodies {
        let line = &body.line;
        // [R77.3-4] The causes come from the results of the body (each configuration), not from
        // the text of its line; the item comes from the name of the body (the compiler name).
        let causes: Vec<String> = outcome.verdicts.get(&body.name).into_iter().flatten().filter_map(|(_, verdict)| match verdict {
            Verdict::NotDecided(cause) => Some(cause.clone()),
            _ => None,
        }).collect();
        let has = |text: &str| causes.iter().any(|cause| cause.starts_with(text)) || line.contains(&format!("not decided ({text})"));
        let item = index.item_of_name(&body.name).unwrap_or_else(|| body.name.clone());
        let reverse = renames.iter().find(|(_, new)| **new == item).map(|(old, _)| old.clone()).unwrap_or_else(|| item.clone());
        let signature = |workspace: &semilla_syntax::Workspace, path: &str| workspace.items.get(path).map(|item| item.signature.split_whitespace().collect::<Vec<_>>().join(" "));
        if has("the parameters change") || has("the return type changes") {
            let cause = if has("the parameters change") { "the parameters change" } else { "the return type changes" };
            let (Some(old), Some(new)) = (signature(input.before, &reverse), signature(input.after, &item)) else {
                notes.push(format!("intent help for `{}`: the equivalence check cannot compare the two versions ({cause}); Semilla has no signature of the item `{item}` in the two versions, so it gives no template", body.name));
                continue;
            };
            // [R69.5-3] The note names the part that changes. Two equal signature texts: a type
            // of the signature (or the type of `self`) changes its definition.
            let change = if old == new {
                let types = changed_types(input, &reverse, &item, &new);
                if types.is_empty() { format!("`{new}` (the same text; a type that it names changes)") } else { format!("`{new}`: {}", types.join("; ")) }
            } else {
                format!("`{old}` → `{new}`")
            };
            notes.push(format!("intent help for `{}`: the equivalence check cannot compare the two versions ({cause}): {change}. A property claim relates them", body.name));
            // [R69.5-3] The result is "not decided", so the help is neutral: a template for each
            // case, each on its own line. [R77.3-4] Each template is a complete claim that
            // `semilla intent check` accepts.
            match semilla_intent::parse::same_template(&item, &old, &new, &type_path) {
                Ok(template) => {
                    let change_form = template.replacen("#[same(", "#[change(", 1).replacen(")] fn", ", reason = \"…\")] fn", 1);
                    notes.push(format!("intent help for `{}`: if the behavior does not change: `{template}`", body.name));
                    notes.push(format!("intent help for `{}`: if it changes: `{change_form}` (a body with the new behavior)", body.name));
                }
                Err(cause) => notes.push(format!("intent help for `{}`: no template: {cause}", body.name)),
            }
        } else if has("a loop") || causes.iter().any(|cause| cause.contains("has no form") || cause.contains("no form of this body")) || line.contains("not decided (a loop") || line.contains("has no form") || line.contains("no form of this body") {
            notes.push(format!("intent help for `{}`: no claim of this batch can close this cause (a loop needs the loop relations of batch 10r2; a body with no MIR needs `#[assume]` of batch 10r3)", body.name));
        }
    }
    outcome.report.notes.extend(notes);
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
        return Vec::new();
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
    if step.coverage.is_empty() {
        let cause = if step.flow.is_some() { "no changed body" } else { "the coverage needs `--compiled` (the default mode has no compiler data)" };
        lines.push(format!("    changed bodies: {cause}"));
        return lines;
    }
    lines.push(format!("    {}", semilla_core::report::coverage_line(&step.coverage)));
    let rank = |state: CoverState| match state {
        CoverState::Disagree => 0,
        CoverState::NotExplained => 1,
        CoverState::Claim => 2,
        CoverState::Equivalent => 3,
    };
    let mut bodies: Vec<&BodyCover> = step.coverage.iter().collect();
    bodies.sort_by_key(|body| rank(body.state));
    let shown = limit.map(|most| most.max(bodies.iter().filter(|body| body.state == CoverState::Disagree).count())).unwrap_or(bodies.len());
    for body in bodies.iter().take(shown) {
        let mark = if body.state == CoverState::Disagree { "✗ " } else { "" };
        lines.push(format!("      {mark}{}", body.line()));
    }
    if bodies.len() > shown {
        lines.push(format!("      … {} more (semilla status --verbose)", bodies.len() - shown));
    }
    lines
}

/// [R77.4-1] True for the kind of a hint of an intent file (a form of `--same`).
pub fn is_hint(kind: &str) -> bool {
    [ClaimKind::Rename, ClaimKind::Moved, ClaimKind::Rewrite].iter().any(|hint| hint.name() == kind)
}

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
        assert_eq!(no_answer(true, true), (IntentResult::Claimed, NO_ANSWER));
    }

    // rule- R69.5-1
    #[test]
    fn phase_one_of_a_compiled_step_keeps_a_claim_pending() {
        assert_eq!(no_answer(false, true).0, IntentResult::Pending);
        assert_eq!(no_answer(false, false), (IntentResult::Claimed, NEEDS_COMPILED));
    }
}
