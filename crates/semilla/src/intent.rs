//! The check of the claims of the intent files of a step against the compiler data of each
//! version (design 67, parts B and C). A child module of the oracle: it reads the records of
//! the two trees of each checked configuration.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use semilla_core::{IntentResult, Step};
use semilla_intent::{ClaimKind, Target, Version};
use semilla_verify::intent::{Answer, Query};

use super::{EquivSide, NormalOracle, record_form, std_forms};

/// [R77.2-2] The budget of the check of one claim in one configuration in solver steps (with the
/// runs of its dependence check, R77.1-1).
const CLAIM_STEPS: u64 = 100_000_000;

/// [R77.2-3] The safety time limit of the check of one claim in one configuration.
const CLAIM_SAFETY: std::time::Duration = std::time::Duration::from_secs(300);

/// The budget of the check of one claim.
fn claim_budget() -> semilla_smt::budget::Budget {
    semilla_smt::budget::Budget::with_safety(CLAIM_STEPS, Some(std::time::Instant::now() + CLAIM_SAFETY))
}

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
    /// [R77.1-1] The dependence set of a proof (`before::NAME`, `after::NAME`).
    depends: Vec<String>,
    /// The record names of the item in each version that has it.
    items: Vec<String>,
    /// [R75.1-3] The suggested test of a false claim (design 75).
    tests: Vec<semilla_core::run::SuggestedTest>,
    /// [R80.3-2] The line of each item of a scope of more than one item.
    lines: Vec<String>,
}

impl NormalOracle {
    /// [R79.2-1] The compile of the intent crate of a step, one time for each set of intent files:
    /// the AST pass (the anchors) and the check of the claims read the same compile.
    pub(crate) fn intent_compile(&self, step: &Step, sources: &[semilla_normal::intent::Source]) -> Arc<std::result::Result<crate::intentcrate::Check, String>> {
        let texts: String = sources.iter().map(|source| format!("{}\n{}\n", source.path, source.text)).collect();
        let key = (step.tree_before.clone(), step.tree_after.clone(), semilla_normal::intent::fnv(texts.as_bytes()));
        if let Some(found) = self.added.lock().unwrap_or_else(|error| error.into_inner()).intent_checks.get(&key) {
            return found.clone();
        }
        let found = Arc::new(crate::intentcrate::check_shared(self, &step.tree_before, &step.tree_after, sources).map_err(|error| format!("the intent crate does not compile: {error:#}")));
        self.added.lock().unwrap_or_else(|error| error.into_inner()).intent_checks.insert(key, found.clone());
        found
    }

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
        let walks = if dry || queries.is_empty() { None } else { self.reach_all(step, renames, super::reach::WALK_STEPS.min(super::equiv_steps())) };
        // [R78.3-1] The intent crate: rustc compiles the files against the builds of the two
        // trees. An error of rustc or of Semilla is an error of each claim of its file, so such a
        // claim is never checked.
        let sources: Vec<semilla_normal::intent::Source> = step.agent.intent.iter().map(|file| semilla_normal::intent::Source { path: file.path.clone(), text: file.text.clone() }).collect();
        let compiled = (!dry && !sources.is_empty()).then(|| self.intent_compile(step, &sources));
        let mir = match compiled.as_deref() {
            None => None,
            Some(Ok(check)) => Some(check),
            Some(Err(text)) => return Some(queries.iter().map(|_| Answer { result: Some(IntentResult::Error), errors: vec![text.clone()], ..Default::default() }).collect()),
        };
        if let Some(check) = &mir {
            let mut errors: Vec<(Option<String>, String)> = check.compiled.diagnostics.iter().filter(|d| d.level == "error").map(|d| (d.file.clone(), format!("{}:{}:{}: {}{}", d.file.as_deref().unwrap_or("the intent crate"), d.line, d.column, d.code.as_ref().map(|code| format!("[{code}] ")).unwrap_or_default(), d.message))).collect();
            errors.extend(check.findings.iter().filter(|f| f.error).map(|f| (Some(f.file.clone()), format!("{}:{}:{}: [{}] {}", f.file, f.line, f.column, f.code, f.message))));
            if !errors.is_empty() {
                // Each error goes to the claim whose function has its line (the last claim that
                // starts at or before it); each other claim gets the place of the first error.
                let line_of = |text: &str| text.split(':').nth(1).and_then(|line| line.parse::<usize>().ok()).unwrap_or(0);
                let start = |query: &Query| query.claim.place.line.min(query.claim.func.place.line);
                return Some(
                    queries
                        .iter()
                        .map(|query| {
                            let next = queries.iter().filter(|other| other.file.path == query.file.path && start(other) > start(query)).map(start).min().unwrap_or(usize::MAX);
                            let own: Vec<String> = errors.iter().filter(|(file, text)| file.as_deref().is_none_or(|file| file == query.file.path && (start(query)..next).contains(&line_of(text)))).map(|(_, text)| text.clone()).collect();
                            let own = if own.is_empty() { vec![format!("not checked: the intent crate has an error at {} (rustc checks each claim of the crate together)", errors[0].1.split(": ").next().unwrap_or(""))] } else { own };
                            Answer { result: Some(IntentResult::Error), errors: own, ..Default::default() }
                        })
                        .collect(),
                );
            }
        }
        // [R80.1-1] The relations of the intent files: each claim check uses them where the
        // types of the two versions differ.
        let relations = mir.as_ref().map(|check| relations_of(&check.compiled, &sources)).unwrap_or_default();
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
                            results.push((config.name(), One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![format!("{}: {error}", query.claim.place)], depends: Vec::new(), items: Vec::new(), tests: Vec::new(), lines: Vec::new() }));
                            continue;
                        }
                    }
                }
                let Ok(sides) = self.sides(step, config) else { continue };
                let empty = semilla_normal::Records::default();
                let old = EquivSide::new(sides.before.get("n0").unwrap_or(&empty));
                let new = EquivSide::new(sides.after.get("n0").unwrap_or(&empty));
                let walk = walks.as_ref().and_then(|walks| walks.iter().find(|(name, _)| *name == config.name()).map(|(_, walk)| walk.clone()));
                let (one, used) = semilla_smt::mir::with_relations(&relations, || semilla_smt::mir::with_used_relations(|| self.claim_in(&solver, query, &old, &new, &pairs, walk.as_ref(), dry, (step, &config.name()), mir.as_ref().map(|check| (&check.compiled, sources.as_slice())))));
                // [R82.1-5] A proof that uses a relation holds only if each body that makes or
                // changes a value of its type keeps the relation.
                let one = match &mir {
                    Some(check) if !used.is_empty() => semilla_smt::mir::with_relations(&relations, || self.relation_kept(&solver, one, &used, &relations, (&old, &new), &pairs, walk.as_ref(), (step, &config.name()), &check.compiled)),
                    _ => one,
                };
                results.push((config.name(), one));
            }
            answers.push(join(results, skipped, query));
        }
        Some(answers)
    }

    /// The check of one claim in one configuration.
    #[allow(clippy::too_many_arguments)]
    fn claim_in(&self, solver: &semilla_smt::Solver, query: &Query, old: &EquivSide, new: &EquivSide, renames: &[(String, String)], walk: Option<&super::reach::ConfigReach>, dry: bool, (change, config): (&Step, &str), mir: Option<(&semilla_normal::intent::Compiled, &[semilla_normal::intent::Source])>) -> One {
        let claim = &query.claim;
        // [R80.3-1] A scope of more than one item (`impl TYPE`, `impl TRAIT for TYPE`, a glob
        // `MODULE::*`, `callers(ITEM)`): one check for each item, with one line for each
        // (R80.3-2).
        if let Some(Target::Text(text, _)) = claim.targets.first()
            && semilla_intent::heads::is_wide(text)
        {
            if dry {
                return One { result: IntentResult::Claimed, detail: semilla_smt::mir::claim::DRY_CAUSE.into(), reads: Vec::new(), errors: Vec::new(), depends: Vec::new(), items: Vec::new(), tests: Vec::new(), lines: Vec::new() };
            }
            let items = match scope_items(text, old, new, renames, mir.map(|(compiled, _)| compiled)) {
                Ok(items) => items,
                Err(error) => return One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![format!("{}: {error}", claim.place)], depends: Vec::new(), items: Vec::new(), tests: Vec::new(), lines: Vec::new() },
            };
            let mut ones = Vec::new();
            for name in items.iter().take(MAX_SCOPE_ITEMS) {
                let mut one = query.clone();
                one.claim.targets = vec![Target::Text(name.clone(), claim.place.clone())];
                ones.push((name.clone(), self.claim_in(solver, &one, old, new, renames, walk, dry, (change, config), mir)));
            }
            return scope_one(text, ones, items.len().saturating_sub(MAX_SCOPE_ITEMS));
        }
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
            None => return One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![format!("{}: expected an item path", claim.place)], depends: Vec::new(), items: Vec::new(), tests: Vec::new(), lines: Vec::new() },
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
            (Err(error), _) | (_, Err(error)) => return One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![error], depends: Vec::new(), items: Vec::new(), tests: Vec::new(), lines: Vec::new() },
        };
        // [R78.1-3] The definition that rustc resolved for the scope path (also through a
        // re-export or a method of a type): its record name in each tree.
        let (old_item, new_item) = match mir {
            Some((compiled, _)) => {
                let key = item.name();
                let resolved = |prefix: &str| compiled.links.iter().find(|link| link.path == format!("{prefix}{key}")).and_then(|link| link.resolved.as_ref()).and_then(|name| name.strip_prefix(prefix)).map(str::to_string);
                (resolved("before::").unwrap_or(old_item), resolved("after::").unwrap_or(new_item))
            }
            None => (old_item, new_item),
        };
        let in_old = old.records.contains_key(old_item.as_str());
        let in_new = new.records.contains_key(new_item.as_str());
        let items: Vec<String> = [(in_old, format!("before::{old_item}")), (in_new, format!("after::{new_item}"))].into_iter().filter(|(found, _)| *found).map(|(_, name)| name).collect();
        if !in_old && !in_new {
            let mut all = new_names.clone();
            all.extend(old_names.iter().cloned());
            let candidates = semilla_intent::candidates(&item.name(), &all, 6);
            let tail = if candidates.is_empty() { String::new() } else { format!("; candidates: {}", semilla_intent::quoted_list(&candidates)) };
            return One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![format!("{}: `{}`: no version has a body `{}`{tail}", item.place, item.text(), item.name())], depends: Vec::new(), items: Vec::new(), tests: Vec::new(), lines: Vec::new() };
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
                return One { result: IntentResult::Claimed, detail: semilla_smt::mir::claim::DRY_CAUSE.into(), reads, errors: Vec::new(), depends: Vec::new(), items, tests: Vec::new(), lines: Vec::new() };
            }
            let (Some(a), Some(b)) = (old.find(&old_item, "", ""), new.find(&new_item, "", "")) else {
                let only = if in_old { "the old version only" } else { "the new version only" };
                return One { result: IntentResult::Claimed, detail: format!("the item has a body in {only}: no compare"), reads, errors: Vec::new(), depends: Vec::new(), items, tests: Vec::new(), lines: Vec::new() };
            };
            // [R82.1-3] A relation that fits a type of the signature of the item (a parameter, a
            // `&mut` place, the result): the check of related inputs and outcomes (R80.1-4), never
            // a proof by equal MIR or by the equivalence check of one type alone.
            if let Some((compiled, _)) = mir
                && let (Some(form_a), Some(form_b)) = (record_form(a), record_form(b))
                && semilla_smt::mir::relation_applies(&form_a, &form_b)
            {
                return self.related_mir(solver, compiled, old, new, (&old_item, &new_item), &changed, reads, items, (change, config));
            }
            let rename_text = |text: &str| semilla_normal::paths::rename(text, renames);
            let equal_proof = rename_text(&a.text) == b.text && !a.callees.iter().any(|callee| changed.contains(&rename_text(callee)) || changed.contains(callee));
            let budget = Some(claim_budget());
            let check = || self.check_pair(solver, a, b, old, new, renames, true, &changed, equal_proof, budget);
            let answer = if claim.options.except.iter().any(|name| name == "panic_message") { semilla_smt::mir::claim::with_panic_messages_ignored(check) } else { check() };
            use semilla_verify::equiv::Verdict;
            // [R75.1-3] A difference gets its suggested test (design 75).
            let mut verdict = answer.map(|answer| answer.verdict);
            if let Some(found) = verdict.as_mut() {
                self.suggest(change, &mut [(config, change.tree_before.as_str(), change.tree_after.as_str(), found)]);
            }
            // [R80.1-4] An item whose parameter types or result type change: with the relations
            // of the intent files, the check of related inputs and outcomes.
            let types_change = matches!(&verdict, Some(Verdict::NotDecided(cause)) if cause.starts_with("the parameters change") || cause.starts_with("the return type changes"));
            if types_change
                && !semilla_smt::mir::relations_now().is_empty()
                && let Some((compiled, _)) = mir
            {
                return self.related_mir(solver, compiled, old, new, (&old_item, &new_item), &changed, reads, items, (change, config));
            }
            let mut tests = Vec::new();
            let (result, detail) = match verdict {
                Some(Verdict::Equivalent(method)) => (IntentResult::Proved, format!("equivalent by {method}")),
                Some(Verdict::Different { input, before, after, test }) => {
                    let note = test.text();
                    if let semilla_core::run::TestNote::Test(found) = test {
                        tests.push(found);
                    }
                    (IntentResult::False, if note.is_empty() { format!("for {input}: before {before}, after {after}") } else { format!("for {input}: before {before}, after {after} ({note})") })
                }
                Some(Verdict::NotDecided(cause)) | Some(Verdict::NotChecked(cause)) | Some(Verdict::MirChanges(cause)) => (IntentResult::Claimed, cause),
                None => (IntentResult::Claimed, "no check of the item".into()),
            };
            // [R77.1-1] An empty `#[same]` claim is the equivalence check of its item: a proof
            // depends on the two versions of the item.
            let depends = if result == IntentResult::Proved { vec![format!("before::{old_item}"), format!("after::{new_item}")] } else { Vec::new() };
            return One { result, detail, reads, errors: Vec::new(), depends, items, tests, lines: Vec::new() };
        }
        // [R78.4-4] A property claim: the engine checks the claim MIR of rustc.
        let mir_one = mir.map(|(compiled, sources)| self.claim_mir(solver, query, compiled, sources, old, new, (&old_item, &new_item), &changed, &hash_of, &items, (change, config)));
        // [R75.1-3] A false claim MIR names no call of a version: when the parser of 10r1 reads
        // the claim, its model runs too, and a false result of that model (an exact input too)
        // gives the line with the calls and their results, and the suggested test of the
        // counterexample (Semilla runs no code of the user). Each other result of the claim MIR
        // stays.
        let parsed = claim.func.body.as_ref().is_some_and(|body| !matches!(&body.kind, semilla_intent::ExprKind::Other(kind) if kind.starts_with("a body that rustc checks")));
        if let Some(one) = &mir_one
            && (one.result != IntentResult::False || !parsed)
        {
            return mir_one.unwrap_or_else(|| unreachable!());
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
        let checked = semilla_smt::mir::claim::check_claim(solver, &query.file, claim, &tree_a, &tree_b, &step, options, Some(claim_budget()));
        let reads: Vec<String> = checked.reads.iter().filter_map(|(version, name)| hash_of(*version, name)).collect();
        let errors: Vec<String> = checked.errors.iter().map(|error| error.to_string()).collect();
        use semilla_smt::mir::claim::ClaimVerdict;
        let mut claim_tests = Vec::new();
        let (result, detail) = match checked.verdict {
            ClaimVerdict::Proved { method } => (IntentResult::Proved, format!("by {method}")),
            ClaimVerdict::False { input, detail, run } => {
                // [R75.1-3] The suggested test of the counterexample (design 75): each call gets the
                // target and the place of its body (the library of a name first).
                match run {
                    Some(mut case) => {
                        for call in &mut case.calls {
                            let side = if call.side == semilla_core::run::RunSide::Before { old } else { new };
                            if let Some(record) = side.find(&call.name, "", "") {
                                call.target = record.target.clone();
                                call.value = record.value;
                                if let Some(span) = &record.span {
                                    call.file = span.file.clone();
                                    call.line = span.line_start;
                                }
                            }
                        }
                        let note = self.suggest_claim(change, config, &case, &format!("for {input}: {detail}"));
                        let text = note.text();
                        if let semilla_core::run::TestNote::Test(found) = note {
                            claim_tests.push(found);
                        }
                        (IntentResult::False, format!("for {input}: {detail} ({text})"))
                    }
                    None => (IntentResult::False, format!("for {input}: {detail}")),
                }
            }
            ClaimVerdict::Claimed { cause } => (IntentResult::Claimed, cause),
        };
        let depends = if result == IntentResult::Proved { checked.depends.iter().map(|(version, name)| format!("{}::{name}", version.prefix())).collect() } else { Vec::new() };
        let found = One { result, detail, reads, errors, depends, items, tests: claim_tests, lines: Vec::new() };
        match mir_one {
            Some(mir) if found.result != IntentResult::False || !found.errors.is_empty() => mir,
            _ => found,
        }
    }
}

/// The answer of a claim from the results of each configuration (R67.3-10): a false result
/// in one configuration makes the claim false; the claim is proved only if each checked
/// configuration proves it.
impl NormalOracle {
    /// [R78.4-4] The check of one property claim as the MIR of rustc (design 78, part D).
    #[allow(clippy::too_many_arguments)]
    fn claim_mir(&self, solver: &semilla_smt::Solver, query: &Query, compiled: &semilla_normal::intent::Compiled, sources: &[semilla_normal::intent::Source], old: &EquivSide, new: &EquivSide, (old_item, new_item): (&str, &str), changed: &BTreeSet<String>, hash_of: &dyn Fn(Version, &str) -> Option<String>, items: &[String], (change, config): (&Step, &str)) -> One {
        let claim = &query.claim;
        let module = sources.iter().position(|source| source.path == query.file.path).and_then(|index| compiled.modules.get(index));
        let find = |name: &str| compiled.records.iter().find(|record| record.name == name && record.q.is_some()).and_then(record_form);
        let Some(ir) = module.and_then(|module| find(&format!("{}::{module}::{}", semilla_normal::intent::CLAIMS_CRATE, claim.func.name))) else {
            return One { result: IntentResult::Claimed, detail: format!("the intent crate has no form of `fn {}`", claim.func.name), reads: Vec::new(), errors: Vec::new(), depends: Vec::new(), items: items.to_vec(), tests: Vec::new(), lines: Vec::new() };
        };
        // The bodies of each tree that the claim calls: their small callees of the standard
        // library and their instances.
        // [R78.4-6] The calls of the claim and of its own bodies (its closures).
        let claim_name = format!("{}::{}::{}", semilla_normal::intent::CLAIMS_CRATE, module.cloned().unwrap_or_default(), claim.func.name);
        let own_forms: Vec<std::sync::Arc<semilla_smt::mir::Ir>> = compiled.records.iter().filter(|record| record.name.starts_with(&format!("{claim_name}::{{"))).filter_map(record_form).chain(std::iter::once(ir.clone())).collect();
        let mut calls: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for form in &own_forms {
            for block in &form.b {
                if let semilla_smt::mir::IrTermKind::Call(callee, ..) = &block.t.k
                    && let Some(name) = &callee.w
                {
                    if let Some(rest) = name.strip_prefix("before::") {
                        calls[0].push(rest.to_string());
                    } else if let Some(rest) = name.strip_prefix("after::") {
                        calls[1].push(rest.to_string());
                    }
                }
            }
        }
        let (std_a, std_b) = (reached_std(old, &calls[0]), reached_std(new, &calls[1]));
        let workspace_a = |name: &str| old.find(name, "", "").and_then(record_form);
        let workspace_b = |name: &str| new.find(name, "", "").and_then(record_form);
        let own = |name: &str| find(name);
        let is_changed = |name: &str| changed.contains(name);
        let version_a = semilla_smt::mir::property::Version { workspace: &workspace_a, std: &std_a, changed: &is_changed };
        let version_b = semilla_smt::mir::property::Version { workspace: &workspace_b, std: &std_b, changed: &is_changed };
        let own_std = std_forms(&compiled.records.iter().collect::<Vec<_>>());
        let property = semilla_smt::mir::property::Property { ir, own: &own, std: &own_std, item: (old.records.contains_key(old_item).then(|| old_item.to_string()), new.records.contains_key(new_item).then(|| new_item.to_string())) };
        // [R67.3-7] `except = [panic_message]`: two panics of one kind are the same panic.
        let check = || semilla_smt::mir::property::check_property(solver, &property, &version_a, &version_b, Some(claim_budget()));
        let checked = if claim.options.except.iter().any(|name| name == "panic_message") { semilla_smt::mir::claim::with_panic_messages_ignored(check) } else { check() };
        let reads: Vec<String> = checked.reads.iter().filter_map(|read| match read.split_once("::") {
            Some(("before", name)) => hash_of(Version::Before, name),
            Some(("after", name)) => hash_of(Version::After, name),
            _ => None,
        }).collect();
        use semilla_smt::mir::claim::ClaimVerdict;
        let mut tests = Vec::new();
        let (result, detail) = match checked.verdict {
            ClaimVerdict::Proved { method } => (IntentResult::Proved, format!("by {method} (the claim MIR of rustc)")),
            // [R82.3-1] The calls of a version on the failed path, in order: the suggested test
            // makes them and asserts the outcome of the model (Semilla runs no code of the user).
            ClaimVerdict::False { input, detail, run: Some(mut case) } => {
                for call in &mut case.calls {
                    let side = if call.side == semilla_core::run::RunSide::Before { old } else { new };
                    if let Some(record) = side.find(&call.name, "", "") {
                        call.target = record.target.clone();
                        call.value = record.value;
                        if let Some(span) = &record.span {
                            call.file = span.file.clone();
                            call.line = span.line_start;
                        }
                    }
                }
                let note = self.suggest_claim(change, config, &case, &format!("for {input}: {detail}"));
                let text = note.text();
                if let semilla_core::run::TestNote::Test(found) = note {
                    tests.push(found);
                }
                (IntentResult::False, format!("for {input}: {detail} ({text})"))
            }
            ClaimVerdict::False { input, detail, run: None } => (IntentResult::False, format!("for {input}: {detail} (no suggested test: the claim MIR gives no call of a version on its failed path)")),
            ClaimVerdict::Claimed { cause } => (IntentResult::Claimed, cause),
        };
        // [R78.4-9] A proved claim MIR explains each body of its dependence set (R77.1-1).
        let depends = if result == IntentResult::Proved { checked.depends.clone() } else { Vec::new() };
        One { result, detail, reads, errors: Vec::new(), depends, items: items.to_vec(), tests, lines: Vec::new() }
    }
}

impl NormalOracle {
    /// [R80.1-4] The check of an empty `#[same]` claim on an item whose types change, with the
    /// relations of the intent files (the claim MIR machine of design 78, part D).
    #[allow(clippy::too_many_arguments)]
    fn related_mir(&self, solver: &semilla_smt::Solver, compiled: &semilla_normal::intent::Compiled, old: &EquivSide, new: &EquivSide, (old_item, new_item): (&str, &str), changed: &BTreeSet<String>, reads: Vec<String>, items: Vec<String>, (change, config): (&Step, &str)) -> One {
        let find = |name: &str| compiled.records.iter().find(|record| record.name == name && record.q.is_some()).and_then(record_form);
        let workspace_a = |name: &str| old.find(name, "", "").and_then(record_form);
        let workspace_b = |name: &str| new.find(name, "", "").and_then(record_form);
        let (std_a, std_b) = (reached_std(old, &[old_item.to_string()]), reached_std(new, &[new_item.to_string()]));
        let is_changed = |name: &str| changed.contains(name);
        let version_a = semilla_smt::mir::property::Version { workspace: &workspace_a, std: &std_a, changed: &is_changed };
        let version_b = semilla_smt::mir::property::Version { workspace: &workspace_b, std: &std_b, changed: &is_changed };
        let own_std = std_forms(&compiled.records.iter().collect::<Vec<_>>());
        let checked = semilla_smt::mir::property::check_related(solver, (old_item, new_item), &find, &own_std, &version_a, &version_b, Some(claim_budget()));
        use semilla_smt::mir::claim::ClaimVerdict;
        let mut tests = Vec::new();
        let (result, detail) = match checked.verdict {
            ClaimVerdict::Proved { method } => (IntentResult::Proved, format!("related inputs give related outcomes, by {method} (the relations of the intent file)")),
            // [R75.1-3] The suggested test of the counterexample (never a run): each call of a
            // version gets the target and the place of its body.
            ClaimVerdict::False { input, detail, run } => match run {
                Some(mut case) => {
                    for call in &mut case.calls {
                        let side = if call.side == semilla_core::run::RunSide::Before { old } else { new };
                        if let Some(record) = side.find(&call.name, "", "") {
                            call.target = record.target.clone();
                            call.value = record.value;
                            if let Some(span) = &record.span {
                                call.file = span.file.clone();
                                call.line = span.line_start;
                            }
                        }
                    }
                    let note = self.suggest_claim(change, config, &case, &format!("for {input}: {detail}"));
                    let text = note.text();
                    if let semilla_core::run::TestNote::Test(found) = note {
                        tests.push(found);
                    }
                    (IntentResult::False, if text.is_empty() { format!("for {input}: {detail}") } else { format!("for {input}: {detail} ({text})") })
                }
                None => (IntentResult::False, format!("for {input}: {detail}")),
            },
            ClaimVerdict::Claimed { cause } => (IntentResult::Claimed, cause),
        };
        let depends = if result == IntentResult::Proved { checked.depends.clone() } else { Vec::new() };
        One { result, detail, reads, errors: Vec::new(), depends, items, tests, lines: Vec::new() }
    }
}

/// [R82.1-5] The result of the check of one relation on the bodies that make or change a value of
/// its type: the count of the bodies that keep it, the bodies that the check does not prove (with
/// the cause), and the first body that breaks it (its name, the text, and the suggested tests).
#[derive(Clone, Default)]
struct Kept {
    kept: usize,
    unchecked: Vec<String>,
    broken: Option<(String, String, Vec<semilla_core::run::SuggestedTest>)>,
}

/// [R82.1-5] The most bodies of a type that the check of one relation checks.
const MAX_KEPT: usize = 32;

thread_local! {
    /// [R82.1-5] The result of each relation in one configuration of one step.
    static KEPT: std::cell::RefCell<BTreeMap<String, Kept>> = const { std::cell::RefCell::new(BTreeMap::new()) };
}

impl NormalOracle {
    /// [R82.1-5] A proof that uses a relation of a type is a proof only if the relation holds for
    /// each value of the type that a body of the two versions can make or change. The set of
    /// those bodies comes from the compiler forms (an aggregate of the type, a write into a place
    /// of the type, a `&mut` borrow of a part of one); the related check of R80.1-4 runs on each.
    /// All proved: the claim stays proved. A body that breaks the relation makes the claim false
    /// with its input. Else the claim is "claimed: proved if the relation holds; not checked: …",
    /// and it explains no body.
    #[allow(clippy::too_many_arguments)]
    fn relation_kept(&self, solver: &semilla_smt::Solver, mut one: One, used: &[String], relations: &[semilla_smt::mir::Relation], (old, new): (&EquivSide, &EquivSide), renames: &[(String, String)], walk: Option<&super::reach::ConfigReach>, (change, config): (&Step, &str), compiled: &semilla_normal::intent::Compiled) -> One {
        let explains = one.result == IntentResult::Proved || (one.result == IntentResult::Claimed && !one.depends.is_empty());
        if !explains {
            return one;
        }
        let forward = |name: &str| renames.iter().find(|(from, _)| from == name).map(|(_, to)| to.clone()).unwrap_or_else(|| name.to_string());
        let reverse = |name: &str| renames.iter().find(|(_, to)| to == name).map(|(from, _)| from.clone()).unwrap_or_else(|| name.to_string());
        let mut notes = Vec::new();
        let mut unchecked: Vec<String> = Vec::new();
        let mut broken: Option<(String, String, Vec<semilla_core::run::SuggestedTest>, String)> = None;
        let mut kept = 0;
        for name in used {
            let Some(relation) = relations.iter().find(|relation| &relation.name == name) else { continue };
            let key = format!("{}|{}|{config}|{name}", change.tree_before, change.tree_after);
            let status = match KEPT.with(|cell| cell.borrow().get(&key).cloned()) {
                Some(found) => found,
                None => {
                    let mut set: BTreeSet<String> = BTreeSet::new();
                    for (side, is_old) in [(old, true), (new, false)] {
                        for (body, records) in &side.records {
                            if records.iter().filter_map(|record| record_form(record)).any(|form| semilla_smt::mir::makes_or_changes(&form, relation, is_old)) {
                                set.insert(if is_old { forward(body) } else { body.to_string() });
                            }
                        }
                    }
                    let changed = must_inline(old, new, renames, walk);
                    let mut status = Kept::default();
                    for (index, body) in set.iter().enumerate() {
                        if index >= MAX_KEPT {
                            status.unchecked.push(format!("`{body}` (past the bound of {MAX_KEPT} bodies)"));
                            continue;
                        }
                        let old_body = reverse(body);
                        if !old.records.contains_key(old_body.as_str()) || !new.records.contains_key(body.as_str()) {
                            status.unchecked.push(format!("`{body}` (a body of one version only)"));
                            continue;
                        }
                        let found = self.related_mir(solver, compiled, old, new, (&old_body, body), &changed, Vec::new(), Vec::new(), (change, config));
                        match found.result {
                            IntentResult::Proved => status.kept += 1,
                            IntentResult::False if status.broken.is_none() => status.broken = Some((body.clone(), found.detail, found.tests)),
                            IntentResult::False => status.unchecked.push(format!("`{body}` (false too)")),
                            _ => status.unchecked.push(format!("`{body}` ({})", found.detail.chars().take(120).collect::<String>())),
                        }
                    }
                    KEPT.with(|cell| cell.borrow_mut().insert(key, status.clone()));
                    status
                }
            };
            kept += status.kept;
            unchecked.extend(status.unchecked.iter().map(|body| format!("{body} for `{}`", relation.short())));
            if broken.is_none()
                && let Some((body, detail, tests)) = status.broken
            {
                broken = Some((body, detail, tests, relation.short().to_string()));
            }
            notes.push(format!("`{}`", relation.short()));
        }
        unchecked.sort();
        if let Some((body, detail, tests, short)) = broken {
            one.result = IntentResult::False;
            one.detail = format!("the relation `{short}` does not hold after `{body}`: {detail}; the proof of this claim assumes it (R82.1-5; the proof alone: {})", one.detail);
            one.depends.clear();
            one.tests = tests;
        } else if !unchecked.is_empty() {
            one.result = IntentResult::Claimed;
            one.detail = format!("proved if the relation {} holds; not checked: {} (R82.1-5: a body that makes or changes a value of the type; the proof alone: {})", notes.join(", "), unchecked.join(", "), one.detail);
            one.depends.clear();
        } else {
            one.detail = format!("{} (the relation {} holds after each body that makes or changes a value of its type: {kept} checked)", one.detail, notes.join(", "));
        }
        one
    }
}

/// [R80.1-1] The relations of the intent files: the form of each `#[relation]` function of the
/// intent crate.
fn relations_of(compiled: &semilla_normal::intent::Compiled, sources: &[semilla_normal::intent::Source]) -> Vec<semilla_smt::mir::Relation> {
    let mut found = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let Some(module) = compiled.modules.get(index) else { continue };
        let (heads, _) = semilla_intent::heads::check(&source.path, &source.text);
        for head in heads.iter().filter(|head| head.kind == "relation") {
            let name = format!("{}::{module}::{}", semilla_normal::intent::CLAIMS_CRATE, head.function);
            if let Some(form) = compiled.records.iter().find(|record| record.name == name && record.q.is_some()).and_then(record_form) {
                found.push(semilla_smt::mir::Relation { name, form });
            }
        }
    }
    found
}

/// [R80.3-1] The most items of one scope that a claim checks (Decided by Claude: a bound of the
/// cost; each item past it gets no check, and the line of the claim gives the count).
const MAX_SCOPE_ITEMS: usize = 64;

/// [R80.3-1] The items of a scope of more than one item, by the record name of the new version
/// (the old name of an item that only the old version has). rustc resolves each path of the
/// scope (R78.1-3), and the driver gives the items of the definition (`Link::members`); the
/// callers of an item are the bodies whose callees have its definition. Only an item with a
/// body record in a version is an item of the scope.
fn scope_items(text: &str, old: &EquivSide, new: &EquivSide, renames: &[(String, String)], compiled: Option<&semilla_normal::intent::Compiled>) -> Result<Vec<String>, String> {
    let Some(compiled) = compiled else { return Err(format!("the scope `{text}` needs the compiled intent crate (the intent crate did not compile)")) };
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let link = |path: &str| -> Option<&semilla_normal::intent::Link> {
        let path: String = path.chars().filter(|c| !c.is_whitespace()).collect();
        compiled.links.iter().find(|link| link.path == path)
    };
    // The link of each version of a path (a path with its own prefix: that version only).
    let versions = |path: &str| -> Vec<(&'static str, Option<&semilla_normal::intent::Link>)> {
        let path = path.trim();
        for prefix in ["before::", "after::"] {
            if path.starts_with(prefix) {
                return vec![(prefix, link(path))];
            }
        }
        vec![("before::", link(&format!("before::{path}"))), ("after::", link(&format!("after::{path}")))]
    };
    let forward = |name: &str| renames.iter().find(|(from, _)| from == name).map(|(_, to)| to.clone()).unwrap_or_else(|| name.to_string());
    let reverse = |name: &str| renames.iter().find(|(_, to)| to == name).map(|(from, _)| from.clone()).unwrap_or_else(|| name.to_string());
    let mut names: Vec<String> = Vec::new();
    // The name of a member that has a record in its tree: the definition path, else the name
    // with the visible paths (a trait of another crate).
    let has_record = |prefix: &str, name: &str| name.strip_prefix(prefix).is_some_and(|rest| (if prefix == "before::" { old } else { new }).records.contains_key(rest));
    let member_name = |prefix: &str, member: &semilla_normal::intent::Member| -> String {
        match &member.visible {
            Some(visible) if !has_record(prefix, &member.name) && has_record(prefix, visible) => visible.clone(),
            _ => member.name.clone(),
        }
    };
    let mut add = |prefix: &str, name: &str| {
        let Some(rest) = name.strip_prefix(prefix) else { return };
        let item = if prefix == "before::" { forward(rest) } else { rest.to_string() };
        if !names.contains(&item) {
            names.push(item);
        }
    };
    let unresolved = |path: &str| format!("the path `{}` of the scope `{text}` has no definition in a version", path.trim());
    if let Some(inner) = text.strip_prefix("callers(").and_then(|rest| rest.strip_suffix(')')) {
        let mut any = false;
        for (prefix, found) in versions(inner) {
            let Some(target) = found.and_then(|link| link.resolved.as_deref()).and_then(|name| name.strip_prefix(prefix)) else { continue };
            any = true;
            let side = if prefix == "before::" { old } else { new };
            for (name, records) in &side.records {
                if records.iter().any(|record| record.callees.iter().any(|callee| callee == target)) {
                    // A closure body that calls the item: the item that has the closure.
                    let owner = name.split("::{closure").next().unwrap_or(name);
                    if owner != target {
                        add(prefix, &format!("{prefix}{owner}"));
                    }
                }
            }
        }
        if !any {
            return Err(unresolved(inner));
        }
    } else if let Some(rest) = text.strip_prefix("impl ") {
        match rest.split_once(" for ") {
            Some((trait_path, type_path)) => {
                let Some(trait_link) = link(trait_path) else { return Err(unresolved(trait_path)) };
                let owners: Option<Vec<String>> = (type_path.trim() != "*").then(|| versions(type_path).into_iter().filter_map(|(_, found)| found.and_then(|link| link.resolved.clone())).collect());
                if owners.as_ref().is_some_and(Vec::is_empty) {
                    return Err(unresolved(type_path));
                }
                for member in &trait_link.members {
                    if owners.as_ref().is_some_and(|owners| !member.owner.as_ref().is_some_and(|owner| owners.contains(owner))) {
                        continue;
                    }
                    for prefix in ["before::", "after::"] {
                        add(prefix, &member_name(prefix, member));
                    }
                }
            }
            None => {
                let found = versions(rest);
                if found.iter().all(|(_, link)| link.and_then(|link| link.resolved.as_ref()).is_none()) {
                    return Err(unresolved(rest));
                }
                for (prefix, link) in found {
                    for member in link.map(|link| link.members.as_slice()).unwrap_or_default() {
                        add(prefix, &member_name(prefix, member));
                    }
                }
            }
        }
    } else if let Some(module) = text.strip_suffix("::*") {
        let found = versions(module);
        if found.iter().all(|(_, link)| link.and_then(|link| link.resolved.as_ref()).is_none()) {
            return Err(unresolved(module));
        }
        for (prefix, link) in found {
            for member in link.map(|link| link.members.as_slice()).unwrap_or_default() {
                add(prefix, &member_name(prefix, member));
            }
        }
    }
    names.retain(|name| new.records.contains_key(name.as_str()) || old.records.contains_key(reverse(name).as_str()));
    Ok(names)
}

/// [R80.3-2] The result of a claim on a scope of more than one item: the worst result of its
/// items (an error, then a false result, then a claimed one), one line for each item, and each
/// body that a proof of an item depends on.
fn scope_one(text: &str, ones: Vec<(String, One)>, past: usize) -> One {
    let rank = |result: IntentResult| match result {
        IntentResult::Error => 0,
        IntentResult::False => 1,
        IntentResult::Claimed | IntentResult::Pending | IntentResult::Shown | IntentResult::Hint => 2,
        IntentResult::Proved => 3,
    };
    if ones.is_empty() {
        return One { result: IntentResult::Claimed, detail: format!("the scope `{text}` has no item with a body in a version"), reads: Vec::new(), errors: Vec::new(), depends: Vec::new(), items: Vec::new(), tests: Vec::new(), lines: Vec::new() };
    }
    let result = if past > 0 { IntentResult::Claimed } else { IntentResult::Proved };
    let result = ones.iter().map(|(_, one)| one.result).chain(std::iter::once(result)).min_by_key(|result| rank(*result)).unwrap_or(IntentResult::Claimed);
    let count = |wanted: IntentResult| ones.iter().filter(|(_, one)| one.result == wanted).count();
    let mut detail = format!("{} item(s) of the scope `{text}`: {} proved, {} false, {} claimed", ones.len(), count(IntentResult::Proved), count(IntentResult::False), ones.len() - count(IntentResult::Proved) - count(IntentResult::False));
    if past > 0 {
        detail.push_str(&format!("; {past} more item(s) not checked (at most {MAX_SCOPE_ITEMS} for each scope)"));
    }
    let mut out = One { result, detail, reads: Vec::new(), errors: Vec::new(), depends: Vec::new(), items: Vec::new(), tests: Vec::new(), lines: Vec::new() };
    for (name, one) in ones {
        let text = if one.errors.is_empty() { one.detail.clone() } else { one.errors.join("; ") };
        out.lines.push(format!("`{name}`: {}{}", if one.errors.is_empty() { one.result.word() } else { "error" }, if text.is_empty() { String::new() } else { format!(": {text}") }));
        out.reads.extend(one.reads);
        out.errors.extend(one.errors);
        // [R75.1-3] The suggested test of each false item.
        for test in &one.tests {
            semilla_verify::equiv::merge_test(&mut out.tests, test);
        }
        if one.result == IntentResult::Proved {
            out.depends.extend(one.depends);
            out.items.extend(one.items);
        }
    }
    for list in [&mut out.reads, &mut out.errors, &mut out.depends, &mut out.items] {
        list.sort();
        list.dedup();
    }
    out
}

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
        return Answer { result: Some(IntentResult::Claimed), detail, configs: Vec::new(), reads, errors, depends: Vec::new(), items: Vec::new(), tests: Vec::new(), scope: Vec::new() };
    }
    let rank = |result: IntentResult| match result {
        IntentResult::Error => 0,
        IntentResult::False => 1,
        IntentResult::Claimed | IntentResult::Pending | IntentResult::Shown | IntentResult::Hint => 2,
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
    // [R77.1-1] A proof in each configuration explains each body that one of its proofs uses.
    let mut depends: Vec<String> = if result == IntentResult::Proved { results.iter().flat_map(|(_, one)| one.depends.iter().cloned()).collect() } else { Vec::new() };
    depends.sort();
    depends.dedup();
    let mut items: Vec<String> = results.iter().flat_map(|(_, one)| one.items.iter().cloned()).collect();
    items.sort();
    items.dedup();
    // [R80.3-2] A claim on a scope: a proved item explains its bodies also when another item is
    // not proved; an item counts only if each checked configuration proves it.
    let scope_claim = results.iter().any(|(_, one)| !one.lines.is_empty());
    if scope_claim && result == IntentResult::Claimed {
        let common = |list: &dyn Fn(&One) -> &Vec<String>| -> Vec<String> { list(&results[0].1).iter().filter(|name| results.iter().all(|(_, one)| list(one).contains(name))).cloned().collect() };
        depends = common(&|one: &One| &one.depends);
        items = common(&|one: &One| &one.items);
    } else if scope_claim && result != IntentResult::Proved {
        items = Vec::new();
    }
    let mut scope: Vec<String> = results.first().map(|(_, one)| one.lines.clone()).unwrap_or_default();
    for (name, one) in results.iter().skip(1) {
        for line in &one.lines {
            if !scope.contains(line) {
                scope.push(format!("in {name}: {line}"));
            }
        }
    }
    // [R75.1-3] The suggested tests of the false results, once for each name.
    let mut tests: Vec<semilla_core::run::SuggestedTest> = Vec::new();
    for (_, one) in &results {
        for test in &one.tests {
            semilla_verify::equiv::merge_test(&mut tests, test);
        }
    }
    Answer { result: Some(result), detail, configs, reads, errors, depends, items, tests, scope }
}

/// [R79.2-1] The compiled claims of the intent files for the AST pass (design 79): the definition
/// of each scope path (the links of rustc), and for each function of the intent crate the
/// definitions of each version that it calls or names and the fields that it reads, also
/// through its closures and the functions of the intent crate that it calls. An error of rustc
/// or of Semilla gives no data: the claims are errors.
pub(crate) fn compiled_claims(check: &crate::intentcrate::Check, sources: &[semilla_normal::intent::Source]) -> std::result::Result<semilla_verify::astmap::CompiledClaims, String> {
    let (rustc, semilla) = check.errors();
    if rustc > 0 || semilla > 0 {
        return Err(format!("the intent crate has {rustc} error(s) of rustc and {semilla} error(s) of Semilla (`semilla intent check` shows them)"));
    }
    let compiled = &check.compiled;
    let mut out = semilla_verify::astmap::CompiledClaims { links: compiled.links.iter().map(|link| (link.path.clone(), link.resolved.clone())).collect(), functions: BTreeMap::new(), rewrites: BTreeMap::new() };
    let forms: BTreeMap<&str, Arc<semilla_smt::mir::Ir>> = compiled.records.iter().filter_map(|record| record_form(record).map(|form| (record.name.as_str(), form))).collect();
    let prefix = format!("{}::", semilla_normal::intent::CLAIMS_CRATE);
    for (source, module) in sources.iter().zip(&compiled.modules) {
        let start = format!("{prefix}{module}::");
        for record in compiled.records.iter().filter(|record| record.name.starts_with(&start)) {
            let name = &record.name[start.len()..];
            if name.contains("::") || name.starts_with("__semilla_scope_") {
                continue;
            }
            // The bodies of the function: its form, its closures, and each function of the
            // intent crate that it calls (with their closures).
            let mut todo: Vec<String> = vec![record.name.clone()];
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut calls = semilla_verify::astmap::ClaimCalls::default();
            while let Some(body) = todo.pop() {
                if !seen.insert(body.clone()) {
                    continue;
                }
                let closures = format!("{body}::{{");
                todo.extend(compiled.records.iter().map(|known| known.name.as_str()).filter(|known| known.starts_with(&closures)).map(str::to_string));
                // The callees of the form, and of the MIR text (a body with no form, for example
                // a generic claim function: its text names each callee `{after::…}`).
                let text = compiled.records.iter().find(|known| known.name == body).map(|known| known.text.as_str()).unwrap_or("");
                let mut called_all = text_calls(text);
                if let Some(form) = forms.get(body.as_str()) {
                    called_all.extend(ir_calls(form));
                }
                for called in called_all {
                    if let Some(rest) = called.strip_prefix("before::") {
                        calls.calls[0].insert(rest.to_string());
                    } else if let Some(rest) = called.strip_prefix("after::") {
                        calls.calls[1].insert(rest.to_string());
                    } else if called.starts_with(&prefix) {
                        todo.push(called);
                    }
                }
                if let Some(form) = forms.get(body.as_str()) {
                    ir_fields(form, &mut calls.fields);
                }
            }
            out.functions.insert((source.path.clone(), name.to_string()), calls);
        }
        // [R97.1-1] The two closures of each closure form of `#[rewrite]`: the first and the
        // second closure of the function, in the order of the source.
        let (heads, _) = semilla_intent::heads::check(&source.path, &source.text);
        for head in heads.iter().filter(|head| head.closure && head.kind == "rewrite") {
            let text = |number: usize| compiled.records.iter().find(|record| record.name == format!("{start}{}::{{closure#{number}}}", head.function)).map(|record| record.text.clone());
            if let (Some(old), Some(new)) = (text(0), text(1)) {
                out.rewrites.insert((source.path.clone(), head.function.clone()), [old, new]);
            }
        }
    }
    Ok(out)
}

/// The names in braces of a canonical MIR text that start with a facade or with the intent crate:
/// `const fn(u32) -> u32 {after::shop::f}` gives `after::shop::f`.
fn text_calls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices('{') {
        let rest = &text[at + 1..];
        if !(rest.starts_with("before::") || rest.starts_with("after::") || rest.starts_with(&format!("{}::", semilla_normal::intent::CLAIMS_CRATE)) || rest.starts_with("<before::") || rest.starts_with("<after::")) {
            continue;
        }
        // The name ends at the brace that closes the one at `at` (a name can have `<…>` with
        // no brace).
        let Some(end) = rest.find('}') else { continue };
        let name = &rest[..end];
        // A qualified name of the text (`<after::shop::W as after::shop::T>::m`): the facade of
        // the self type gives the version, as in the name of a callee (R78.4-1).
        let name = match name.strip_prefix('<') {
            Some(inner) if inner.starts_with("before::") => format!("before::<{}", inner.replacen("before::", "", 1)),
            Some(inner) if inner.starts_with("after::") => format!("after::<{}", inner.replacen("after::", "", 1)),
            _ => name.to_string(),
        };
        out.push(name);
    }
    out
}

/// The names of the callees of a form and of the functions that it names as a value.
fn ir_calls(form: &semilla_smt::mir::Ir) -> Vec<String> {
    use semilla_smt::mir::{IrConst, IrOp, IrRvalue, IrStmtKind, IrTermKind};
    let mut out = Vec::new();
    let op = |operand: &IrOp, out: &mut Vec<String>| {
        if let IrOp::K(IrConst::Fn(name) | IrConst::G(name, ..)) = operand {
            out.push(name.clone());
        }
    };
    for block in &form.b {
        for statement in &block.s {
            if let IrStmtKind::Set(_, value) = &statement.k {
                match value {
                    IrRvalue::Use(a) | IrRvalue::Un(_, a) | IrRvalue::Cast(a, _) => op(a, &mut out),
                    IrRvalue::Bin(_, a, b) | IrRvalue::Chk(_, a, b) => {
                        op(a, &mut out);
                        op(b, &mut out);
                    }
                    IrRvalue::Agg(_, list) => list.iter().for_each(|a| op(a, &mut out)),
                    _ => {}
                }
            }
        }
        if let IrTermKind::Call(callee, args, ..) = &block.t.k {
            if let Some(name) = &callee.w {
                out.push(name.clone());
            }
            args.iter().for_each(|a| op(a, &mut out));
        }
    }
    out
}

/// [R79.2-4] The fields that a form reads: each projection of a field of an ADT, with the type as
/// rustc prints it (no facade, no generic arguments), the variant of a downcast, and the field.
fn ir_fields(form: &semilla_smt::mir::Ir, out: &mut [BTreeSet<(String, String, String)>; 2]) {
    use semilla_smt::mir::{IrOp, IrPlace, IrProj, IrRvalue, IrStmtKind, IrTermKind, IrTy};
    let plain = |name: &str| -> Option<(usize, String)> {
        let (side, rest) = if let Some(rest) = name.strip_prefix("before::") { (0, rest) } else { (1, name.strip_prefix("after::")?) };
        Some((side, rest.split('<').next().unwrap_or(rest).to_string()))
    };
    let place = |at: &IrPlace, out: &mut [BTreeSet<(String, String, String)>; 2]| {
        let Some(mut ty) = form.l.get(at.l).and_then(|index| form.t.get(*index as usize)) else { return };
        let mut variant: Option<usize> = None;
        for step in &at.p {
            match (step, ty) {
                (IrProj::D, IrTy::Ref(_, inner)) => match form.t.get(*inner as usize) {
                    Some(next) => ty = next,
                    None => return,
                },
                (IrProj::V(index), IrTy::Adt { n, vn, .. }) => {
                    if let (Some((side, name)), Some(word)) = (plain(n), vn.get(*index)) {
                        out[side].insert((name, word.clone(), String::new()));
                    }
                    variant = Some(*index);
                }
                (IrProj::F(index), IrTy::Adt { n, v, vn, fs, .. }) => {
                    let at_variant = variant.unwrap_or(0);
                    if let (Some((side, name)), Some(field)) = (plain(n), fs.get(at_variant).and_then(|fields| fields.get(*index))) {
                        let word = if variant.is_some() { vn.get(at_variant).cloned().unwrap_or_default() } else { String::new() };
                        out[side].insert((name, word, field.clone()));
                    }
                    match v.get(at_variant).and_then(|types| types.get(*index)).and_then(|next| form.t.get(*next as usize)) {
                        Some(next) => ty = next,
                        None => return,
                    }
                    variant = None;
                }
                _ => return,
            }
        }
    };
    let op = |operand: &IrOp, out: &mut [BTreeSet<(String, String, String)>; 2]| {
        if let IrOp::C(at) | IrOp::M(at) = operand {
            place(at, out);
        }
    };
    for block in &form.b {
        for statement in &block.s {
            if let IrStmtKind::Set(_, value) = &statement.k {
                match value {
                    IrRvalue::Use(a) | IrRvalue::Un(_, a) | IrRvalue::Cast(a, _) => op(a, out),
                    IrRvalue::Bin(_, a, b) | IrRvalue::Chk(_, a, b) => {
                        op(a, out);
                        op(b, out);
                    }
                    IrRvalue::Agg(_, list) => list.iter().for_each(|a| op(a, out)),
                    IrRvalue::Ref(_, at) | IrRvalue::Disc(at) => op(&IrOp::C(at.clone()), out),
                    _ => {}
                }
            }
        }
        match &block.t.k {
            IrTermKind::Call(_, args, ..) => args.iter().for_each(|a| op(a, out)),
            IrTermKind::Sw(a, ..) | IrTermKind::Assert(a, ..) => op(a, out),
            _ => {}
        }
    }
}

/// [R97.2-1] The element table of a body pair in the form of the verifier (design 97).
pub(crate) fn mir_table(table: Option<Arc<semilla_normal::elements::Table>>) -> Option<Arc<semilla_verify::MirTable>> {
    let table = table?;
    let place = |place: &semilla_normal::elements::Place| (place.file.clone(), place.start, place.end);
    let ranges = |list: &[(Option<semilla_normal::elements::Place>, Option<semilla_normal::elements::Place>)]| list.iter().map(|(tokens, call)| (tokens.as_ref().map(place), call.as_ref().map(place))).collect();
    let decls = |list: &[(i64, Option<semilla_normal::elements::Place>)]| list.iter().map(|(kind, at)| (*kind, at.as_ref().map(place))).collect();
    Some(Arc::new(semilla_verify::MirTable {
        texts: table.texts.clone(),
        ranges: [ranges(&table.ranges[0]), ranges(&table.ranges[1])],
        pairs: table.pairs.clone(),
        locals: table.locals.clone(),
        decls: [decls(&table.decls[0]), decls(&table.decls[1])],
        same: table.same.clone(),
        moved: table.moved.clone(),
    }))
}
