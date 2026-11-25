//! The claims of the intent files of a step (design 67, parts C and D).
//!
//! The verifier parses each intent file of the step. A claim of the forms of `--same`
//! (`#[rename]`, `#[moved]`, `#[rewrite]`) is a `same` fact of the agent with the same check
//! (design 48). Each other claim goes to the oracle, which resolves its references against the
//! compiler data of each version and checks it (`semilla_smt::mir::claim`). The result of a
//! claim is a line of the step: it never changes a fact, a gap, or a compare result.

use std::collections::BTreeMap;
use std::sync::Arc;

use semilla_core::{IntentClaim, IntentFile, IntentResult};
use semilla_intent::{ClaimKind, Item};

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
}

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

/// The claims of a step: the ids and the queries for the oracle.
pub fn queries(files: &[semilla_intent::File], dry: bool) -> Vec<Query> {
    let mut out = Vec::new();
    let mut number = 0;
    for file in files {
        let shared = Arc::new(file.clone());
        for item in &file.items {
            number += 1;
            if let Item::Claim(claim) = item
                && matches!(claim.kind, ClaimKind::Same | ClaimKind::Change | ClaimKind::Different)
            {
                out.push(Query { id: format!("i{number}"), file: shared.clone(), claim: claim.clone(), dry });
            }
        }
    }
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
                .map(|(index, error)| IntentClaim { id: format!("e{}", index + 1), kind: "file".into(), text: error.place.file.clone(), place: format!("{}:{}", error.place.file, error.place.line), result: IntentResult::Error, detail: error.to_string(), configs: Vec::new(), options: Vec::new(), reads: Vec::new(), stale: String::new() })
                .collect();
            return;
        }
    };
    let renames: BTreeMap<String, String> = outcome.renames.items.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
    help_notes(input, &renames, outcome);
    let asked = queries(&parsed, false);
    let answers = if asked.is_empty() { None } else { bodies.intent(input.step, &renames, &asked) };
    // The stored reads of the last check: `ID before::PATH HASH`.
    let stored: BTreeMap<&str, Vec<&str>> = input.step.agent.intent_reads.iter().filter_map(|line| line.split_once(' ')).fold(BTreeMap::new(), |mut map, (id, read)| {
        map.entry(id).or_insert_with(Vec::new).push(read);
        map
    });
    let mut claims = Vec::new();
    let mut number = 0;
    for file in &parsed {
        for item in &file.items {
            number += 1;
            let id = format!("i{number}");
            let mut line = |kind: &str, text: String, place: &semilla_intent::Place, result: IntentResult, detail: String, options: Vec<String>| {
                claims.push(IntentClaim { id: id.clone(), kind: kind.into(), text, place: format!("{}:{}", place.file, place.line), result, detail, configs: Vec::new(), options, reads: Vec::new(), stale: String::new() });
            };
            match item {
                Item::Intent(text, place) => line("intent", text.clone(), place, IntentResult::Shown, String::new(), Vec::new()),
                Item::Relation(_) => number -= 1,
                Item::Claim(claim) => {
                    let options = option_texts(claim);
                    match claim.kind {
                        ClaimKind::Rename | ClaimKind::Moved | ClaimKind::Rewrite => {
                            let Some(text) = same_text(claim) else { continue };
                            let fact = outcome.report.agent_facts.iter().find(|fact| fact.kind == "same" && fact.options.iter().any(|option| *option == text));
                            let (result, detail) = match fact {
                                Some(fact) if fact.holds => (IntentResult::Proved, format!("the check of `--same '{text}'` holds (fact {} of the agent: {})", fact.id, fact.result)),
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
                            let (result, detail, configs, reads, errors) = match found {
                                Some(answer) => (answer.result.unwrap_or(IntentResult::Claimed), answer.detail, answer.configs, answer.reads, answer.errors),
                                None if input.step.compiled => (IntentResult::Pending, "the compiled check of the step gives the result (phase 2)".into(), Vec::new(), Vec::new(), Vec::new()),
                                None => (IntentResult::Claimed, NEEDS_COMPILED.into(), Vec::new(), Vec::new(), Vec::new()),
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
    let mut notes = Vec::new();
    for body in &flow.bodies {
        let line = &body.line;
        let reverse = renames.iter().find(|(_, new)| **new == body.name).map(|(old, _)| old.clone()).unwrap_or_else(|| body.name.clone());
        let signature = |workspace: &semilla_syntax::Workspace, path: &str| workspace.items.get(path).map(|item| item.signature.split_whitespace().collect::<Vec<_>>().join(" "));
        if line.contains("not decided (the parameters change)") || line.contains("not decided (the return type changes)") {
            let (Some(old), Some(new)) = (signature(input.before, &reverse), signature(input.after, &body.name)) else { continue };
            let cause = if line.contains("the parameters change") { "the parameters change" } else { "the return type changes" };
            let mut note = format!("intent help for `{}`: the equivalence check cannot compare the two versions ({cause}): `{old}` → `{new}`. A property claim relates them", body.name);
            if let Some(template) = semilla_intent::parse::same_template(&body.name, &old, &new) {
                note.push_str(&format!("; a template for the intent file: `{template}`"));
            }
            notes.push(note);
        } else if line.contains("not decided (a loop") || line.contains("has no form") || line.contains("no form of this body") {
            notes.push(format!("intent help for `{}`: no claim of this batch can close this cause (a loop needs the loop relations of batch 10r2; a body with no MIR needs `#[assume]` of batch 10r3)", body.name));
        }
    }
    outcome.report.notes.extend(notes);
}

/// The lines of the section "intent" of a step (design 67, part D).
pub fn lines(claims: &[IntentClaim]) -> Vec<String> {
    if claims.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!("  intent ({} claim(s); a claim is a proof only if it is proved):", claims.iter().filter(|claim| claim.kind != "intent").count())];
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

/// The help for a claim result that an agent can fix.
fn help(claim: &IntentClaim) -> Option<String> {
    match claim.result {
        IntentResult::Error => Some("correct the claim, then run `semilla intent check FILE` (it checks the references and the types)".into()),
        IntentResult::False => Some("the input is an exact input of the two versions: correct the claim or the code".into()),
        _ => None,
    }
}
