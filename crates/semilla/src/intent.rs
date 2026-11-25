//! The check of the claims of the intent files of a step against the compiler data of each
//! version (design 67, parts B and C). A child module of the oracle: it reads the records of
//! the two trees of each checked configuration.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use semilla_core::{IntentResult, Step};
use semilla_intent::{ClaimKind, Target, Version};
use semilla_verify::intent::{Answer, Query};

use super::{EquivSide, NormalOracle, record_form, std_forms};

/// The time budget of the check of one claim in one configuration.
const CLAIM_TIME: std::time::Duration = std::time::Duration::from_secs(20);

/// The short hash of the text of a body.
fn text_hash(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The bodies that a check must inline (R67.3-5): the walk of the equivalence check, or, with
/// no walk, each changed body and each body that reaches one through its callees (the
/// reverse closure, with no proof of an equivalent caller).
fn must_inline(old: &EquivSide, new: &EquivSide, renames: &[(String, String)], walk: Option<&super::reach::ConfigReach>) -> Arc<BTreeSet<String>> {
    if let Some(walk) = walk {
        return walk.must_inline.clone();
    }
    let mut set = super::changed_names(old, new, renames);
    loop {
        let mut grew = false;
        for side in [old, new] {
            for (name, records) in &side.records {
                if !set.contains(*name) && records.iter().any(|record| record.callees.iter().any(|callee| set.contains(callee))) {
                    set.insert(name.to_string());
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    Arc::new(set)
}

/// The std forms of the bodies that a claim calls and of their callees (to a depth).
fn reached_std(side: &EquivSide, names: &[String]) -> BTreeMap<String, Arc<semilla_smt::mir::Ir>> {
    let mut list: Vec<&semilla_normal::BodyRecord> = names.iter().filter_map(|name| side.find(name, "", "")).collect();
    let mut at = 0;
    while at < list.len() && list.len() < 64 {
        let current = list[at];
        at += 1;
        for callee in &current.callees {
            if let Some(found) = side.find(callee, &current.target, &current.unit)
                && !list.iter().any(|known| std::ptr::eq(*known, found))
            {
                list.push(found);
            }
        }
    }
    std_forms(&list)
}

/// The result of one configuration: the result, the text, the reads, and the errors.
struct One {
    result: IntentResult,
    detail: String,
    reads: Vec<String>,
    errors: Vec<String>,
}

impl NormalOracle {
    /// The check of the claims of a step (design 67).
    pub(crate) fn intent_claims(&self, step: &Step, renames: &BTreeMap<String, String>, queries: &[Query]) -> Option<Vec<Answer>> {
        let checked = self.checked(step).ok()?;
        let Ok(solver) = semilla_smt::Solver::find_for(&crate::store::tool_dirs(&self.repo.root), Some(&self.repo.root)) else {
            return Some(queries.iter().map(|_| Answer { result: Some(IntentResult::Claimed), detail: "no SMT solver".into(), ..Default::default() }).collect());
        };
        let solver = solver.with_timeout(super::EQUIV_TIMEOUT);
        let pairs: Vec<(String, String)> = renames.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
        let dry = queries.iter().all(|query| query.dry);
        // The walk of the equivalence check (design 61.10): the bodies that each check must
        // inline. A dry check needs no walk.
        let walks = if dry || queries.is_empty() { None } else { self.reach_all(step, renames, self.equivalence_time(step).min(super::reach::MAX_TIME)) };
        let mut answers = Vec::new();
        for query in queries {
            let mut results: Vec<(String, One)> = Vec::new();
            let mut skipped = Vec::new();
            for config in &checked {
                // [R67.3-9] `cfg = "…"`: the claim is about the matching configurations only.
                if let Some(predicate) = &query.claim.options.cfg {
                    let truth = self.truth(&step.tree_after, config);
                    let atoms: BTreeSet<String> = truth.units.iter().flat_map(|unit| unit.cfg.iter().cloned()).collect();
                    match semilla_intent::cfg_holds(predicate, &atoms) {
                        Ok(true) => {}
                        Ok(false) => {
                            skipped.push(config.name());
                            continue;
                        }
                        Err(error) => {
                            results.push((config.name(), One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![format!("{}: {error}", query.claim.place)] }));
                            continue;
                        }
                    }
                }
                let Ok(sides) = self.sides(step, config) else { continue };
                let empty = Vec::new();
                let old = EquivSide::new(sides.before.get("n0").unwrap_or(&empty));
                let new = EquivSide::new(sides.after.get("n0").unwrap_or(&empty));
                let walk = walks.as_ref().and_then(|walks| walks.iter().find(|(name, _)| *name == config.name()).map(|(_, walk)| walk.clone()));
                let one = self.claim_in(&solver, query, &old, &new, &pairs, walk.as_ref(), dry);
                results.push((config.name(), one));
            }
            answers.push(join(results, skipped, query));
        }
        Some(answers)
    }

    /// The check of one claim in one configuration.
    #[allow(clippy::too_many_arguments)]
    fn claim_in(&self, solver: &semilla_smt::Solver, query: &Query, old: &EquivSide, new: &EquivSide, renames: &[(String, String)], walk: Option<&super::reach::ConfigReach>, dry: bool) -> One {
        let claim = &query.claim;
        // [R67.2-4] The names of the bodies, one for each source body: two names of one source
        // body (the same place and plain form, for example an impl of `core::iter::Iterator` and of
        // `std::iter::Iterator` in two targets) are one body for a loose path.
        let names = |side: &EquivSide| -> Vec<String> {
            let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
            let mut out = Vec::new();
            for (name, records) in &side.records {
                let Some(record) = records.first() else { continue };
                let key = (record.span.as_ref().map(|span| format!("{}:{}-{}", span.file, span.line_start, span.line_end)).unwrap_or_else(|| name.to_string()), semilla_intent::plain_name(name));
                if seen.insert(key) {
                    out.push(name.to_string());
                }
            }
            out
        };
        let (old_names, new_names) = (names(old), names(new));
        let reverse = |name: &str| renames.iter().find(|(_, to)| to == name).map(|(from, _)| from.clone()).unwrap_or_else(|| name.to_string());
        let forward = |name: &str| renames.iter().find(|(from, _)| from == name).map(|(_, to)| to.clone()).unwrap_or_else(|| name.to_string());
        // [R67.2-3] The item of the claim must resolve in a version.
        // A text target is the record name of a body (`"<shop::Cart as Clone>::clone"`): the
        // path form of Rust has no `<Type as Trait>` part.
        let item = match claim.targets.first() {
            Some(Target::Path(item)) => item.clone(),
            Some(Target::Text(text, place)) => semilla_intent::IPath { version: None, segments: vec![text.clone()], generics: Vec::new(), place: place.clone() },
            None => return One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![format!("{}: expected an item path", claim.place)] },
        };
        let item = &item;
        let (old_item, new_item) = match item.version {
            Some(Version::Before) => (item.name(), forward(&item.name())),
            Some(Version::After) => (reverse(&item.name()), item.name()),
            None => (reverse(&item.name()), item.name()),
        };
        // [R67.2-4] A loose path names the one body with the same plain form.
        let loose = |side: &EquivSide, names: &[String], wanted: String| -> Result<String, String> {
            if side.records.contains_key(wanted.as_str()) {
                return Ok(wanted);
            }
            match semilla_intent::loose_match(&wanted, names.iter()) {
                Ok(Some(found)) => Ok(found),
                Ok(None) => Ok(wanted),
                Err(list) => Err(format!("{}: `{}` names two or more bodies; write one of them: {}", item.place, item.text(), semilla_intent::quoted_list(&list))),
            }
        };
        let (old_item, new_item) = match (loose(old, &old_names, old_item), loose(new, &new_names, new_item)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(error), _) | (_, Err(error)) => return One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![error] },
        };
        let in_old = old.records.contains_key(old_item.as_str());
        let in_new = new.records.contains_key(new_item.as_str());
        if !in_old && !in_new {
            let mut all = new_names.clone();
            all.extend(old_names.iter().cloned());
            let candidates = semilla_intent::candidates(&item.name(), &all, 6);
            let tail = if candidates.is_empty() { String::new() } else { format!("; candidates: {}", semilla_intent::quoted_list(&candidates)) };
            return One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![format!("{}: `{}`: no version has a body `{}`{tail}", item.place, item.text(), item.name())] };
        }
        let hash_of = |version: Version, name: &str| -> Option<String> {
            let side = if version == Version::Before { old } else { new };
            side.find(name, "", "").map(|record| format!("{}::{name} {}", version.prefix(), text_hash(&record.text)))
        };
        let changed = must_inline(old, new, renames, walk);
        // An empty `#[same(ITEM)]`: the equivalence check of the step for the item (R67.4-2).
        if claim.kind == ClaimKind::Same && claim.func.body.is_none() {
            let reads: Vec<String> = [hash_of(Version::Before, &old_item), hash_of(Version::After, &new_item)].into_iter().flatten().collect();
            if dry {
                return One { result: IntentResult::Claimed, detail: semilla_smt::mir::claim::DRY_CAUSE.into(), reads, errors: Vec::new() };
            }
            let (Some(a), Some(b)) = (old.find(&old_item, "", ""), new.find(&new_item, "", "")) else {
                let only = if in_old { "the old version only" } else { "the new version only" };
                return One { result: IntentResult::Claimed, detail: format!("the item has a body in {only}: no compare"), reads, errors: Vec::new() };
            };
            let rename_text = |text: &str| semilla_normal::paths::rename(text, renames);
            let equal_proof = rename_text(&a.text) == b.text && !a.callees.iter().any(|callee| changed.contains(&rename_text(callee)) || changed.contains(callee));
            let deadline = Some(std::time::Instant::now() + CLAIM_TIME);
            let check = || self.check_pair(solver, a, b, old, new, renames, true, &changed, equal_proof, deadline);
            let answer = if claim.options.except.iter().any(|name| name == "panic_message") { semilla_smt::mir::claim::with_panic_messages_ignored(check) } else { check() };
            use semilla_verify::equiv::Verdict;
            let (result, detail) = match answer.map(|answer| answer.verdict) {
                Some(Verdict::Equivalent(method)) => (IntentResult::Proved, format!("equivalent by {method}")),
                Some(Verdict::Different { input, before, after }) => (IntentResult::False, format!("for {input}: before {before}, after {after}")),
                Some(Verdict::NotDecided(cause)) | Some(Verdict::NotChecked(cause)) => (IntentResult::Claimed, cause),
                None => (IntentResult::Claimed, "no check of the item".into()),
            };
            return One { result, detail, reads, errors: Vec::new() };
        }
        // A property claim (R67.3-1).
        let mut calls = Vec::new();
        if let Some(body) = &claim.func.body {
            semilla_intent::calls_of(body, &mut calls);
        }
        let called = |version: Version| -> Vec<String> { calls.iter().filter(|(ver, _)| *ver == version).map(|(_, name)| name.clone()).collect() };
        let (std_a, std_b) = (reached_std(old, &called(Version::Before)), reached_std(new, &called(Version::After)));
        let workspace_a = |name: &str| old.find(name, "", "").and_then(record_form);
        let workspace_b = |name: &str| new.find(name, "", "").and_then(record_form);
        let rename_a = |text: &str| semilla_normal::paths::rename(text, renames);
        let same = |text: &str| text.to_string();
        let tree_a = semilla_smt::mir::claim::Tree { workspace: &workspace_a, std: &std_a, rename: &rename_a, names: &old_names };
        let tree_b = semilla_smt::mir::claim::Tree { workspace: &workspace_b, std: &std_b, rename: &same, names: &new_names };
        let changed_names: Vec<String> = changed.iter().cloned().collect();
        let is_changed = |name: &str| changed.contains(name);
        let step = semilla_smt::mir::Step { changed: &is_changed, changed_names: &changed_names };
        let options = semilla_smt::mir::claim::ClaimOptions { except_panic_message: claim.options.except.iter().any(|name| name == "panic_message"), dry };
        let checked = semilla_smt::mir::claim::check_claim(solver, &query.file, claim, &tree_a, &tree_b, &step, options, Some(std::time::Instant::now() + CLAIM_TIME));
        let reads: Vec<String> = checked.reads.iter().filter_map(|(version, name)| hash_of(*version, name)).collect();
        let errors: Vec<String> = checked.errors.iter().map(|error| error.to_string()).collect();
        use semilla_smt::mir::claim::ClaimVerdict;
        let (result, detail) = match checked.verdict {
            ClaimVerdict::Proved { method } => (IntentResult::Proved, format!("by {method}")),
            ClaimVerdict::False { input, detail } => (IntentResult::False, format!("for {input}: {detail}")),
            ClaimVerdict::Claimed { cause } => (IntentResult::Claimed, cause),
        };
        One { result, detail, reads, errors }
    }
}

/// The answer of a claim from the results of each configuration (R67.3-10): a false result
/// in one configuration makes the claim false; the claim is proved only if each checked
/// configuration proves it.
fn join(results: Vec<(String, One)>, skipped: Vec<String>, query: &Query) -> Answer {
    let mut reads: Vec<String> = results.iter().flat_map(|(_, one)| one.reads.iter().cloned()).collect();
    reads.sort();
    reads.dedup();
    let mut errors: Vec<String> = results.iter().flat_map(|(_, one)| one.errors.iter().cloned()).collect();
    errors.sort();
    errors.dedup();
    if results.is_empty() {
        let detail = match &query.claim.options.cfg {
            Some(cfg) => format!("no checked configuration matches `cfg = {cfg:?}` (checked: {})", if skipped.is_empty() { "none".to_string() } else { skipped.join("; ") }),
            None => "no checked configuration has the records of the two trees".into(),
        };
        return Answer { result: Some(IntentResult::Claimed), detail, configs: Vec::new(), reads, errors };
    }
    let rank = |result: IntentResult| match result {
        IntentResult::Error => 0,
        IntentResult::False => 1,
        IntentResult::Claimed | IntentResult::Pending | IntentResult::Shown => 2,
        IntentResult::Proved => 3,
    };
    let worst = results.iter().min_by_key(|(_, one)| rank(one.result)).map(|(_, one)| one.result).unwrap_or(IntentResult::Claimed);
    let agree = results.iter().all(|(_, one)| one.result == results[0].1.result && one.detail == results[0].1.detail);
    let first = results.iter().find(|(_, one)| one.result == worst).map(|(name, one)| (name.clone(), one.detail.clone())).unwrap_or_default();
    let mut configs = Vec::new();
    if !agree {
        configs = results.iter().map(|(name, one)| format!("in {name}: {}{}", one.result.word(), if one.detail.is_empty() { String::new() } else { format!(": {}", one.detail) })).collect();
    }
    if !skipped.is_empty() {
        configs.push(format!("not about: {} (`cfg`)", skipped.join("; ")));
    }
    let detail = if agree { first.1 } else { format!("in {}: {}", first.0, first.1) };
    // A claimed or an error part of one configuration keeps a proof of another one out.
    let result = if worst == IntentResult::Shown || worst == IntentResult::Pending { IntentResult::Claimed } else { worst };
    Answer { result: Some(result), detail, configs, reads, errors }
}
