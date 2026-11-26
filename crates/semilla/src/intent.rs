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
        let walks = if dry || queries.is_empty() { None } else { self.reach_all(step, renames, super::reach::WALK_STEPS.min(super::equiv_steps())) };
        // [R78.3-1] The intent crate: rustc compiles the files against the builds of the two
        // trees. An error of rustc or of Semilla is an error of each claim of its file, so such a
        // claim is never checked.
        let sources: Vec<semilla_normal::intent::Source> = step.agent.intent.iter().map(|file| semilla_normal::intent::Source { path: file.path.clone(), text: file.text.clone() }).collect();
        let mir = if dry || sources.is_empty() {
            None
        } else {
            match crate::intentcrate::check(self, &step.tree_before, &step.tree_after, &sources) {
                Ok(check) => Some(check),
                Err(error) => {
                    let text = format!("the intent crate does not compile: {error:#}");
                    return Some(queries.iter().map(|_| Answer { result: Some(IntentResult::Error), errors: vec![text.clone()], ..Default::default() }).collect());
                }
            }
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
                            results.push((config.name(), One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![format!("{}: {error}", query.claim.place)], depends: Vec::new(), items: Vec::new(), tests: Vec::new() }));
                            continue;
                        }
                    }
                }
                let Ok(sides) = self.sides(step, config) else { continue };
                let empty = Vec::new();
                let old = EquivSide::new(sides.before.get("n0").unwrap_or(&empty));
                let new = EquivSide::new(sides.after.get("n0").unwrap_or(&empty));
                let walk = walks.as_ref().and_then(|walks| walks.iter().find(|(name, _)| *name == config.name()).map(|(_, walk)| walk.clone()));
                let one = self.claim_in(&solver, query, &old, &new, &pairs, walk.as_ref(), dry, (step, &config.name()), mir.as_ref().map(|check| (&check.compiled, sources.as_slice())));
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
            None => return One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![format!("{}: expected an item path", claim.place)], depends: Vec::new(), items: Vec::new(), tests: Vec::new() },
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
            (Err(error), _) | (_, Err(error)) => return One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![error], depends: Vec::new(), items: Vec::new(), tests: Vec::new() },
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
            return One { result: IntentResult::Error, detail: String::new(), reads: Vec::new(), errors: vec![format!("{}: `{}`: no version has a body `{}`{tail}", item.place, item.text(), item.name())], depends: Vec::new(), items: Vec::new(), tests: Vec::new() };
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
                return One { result: IntentResult::Claimed, detail: semilla_smt::mir::claim::DRY_CAUSE.into(), reads, errors: Vec::new(), depends: Vec::new(), items, tests: Vec::new() };
            }
            let (Some(a), Some(b)) = (old.find(&old_item, "", ""), new.find(&new_item, "", "")) else {
                let only = if in_old { "the old version only" } else { "the new version only" };
                return One { result: IntentResult::Claimed, detail: format!("the item has a body in {only}: no compare"), reads, errors: Vec::new(), depends: Vec::new(), items, tests: Vec::new() };
            };
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
                Some(Verdict::NotDecided(cause)) | Some(Verdict::NotChecked(cause)) => (IntentResult::Claimed, cause),
                None => (IntentResult::Claimed, "no check of the item".into()),
            };
            // [R77.1-1] An empty `#[same]` claim is the equivalence check of its item: a proof
            // depends on the two versions of the item.
            let depends = if result == IntentResult::Proved { vec![format!("before::{old_item}"), format!("after::{new_item}")] } else { Vec::new() };
            return One { result, detail, reads, errors: Vec::new(), depends, items, tests };
        }
        // [R78.4-4] A property claim: the engine checks the claim MIR of rustc.
        let mir_one = mir.map(|(compiled, sources)| self.claim_mir(solver, query, compiled, sources, old, new, (&old_item, &new_item), &changed, &hash_of, &items));
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
        let found = One { result, detail, reads, errors, depends, items, tests: claim_tests };
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
    fn claim_mir(&self, solver: &semilla_smt::Solver, query: &Query, compiled: &semilla_normal::intent::Compiled, sources: &[semilla_normal::intent::Source], old: &EquivSide, new: &EquivSide, (old_item, new_item): (&str, &str), changed: &BTreeSet<String>, hash_of: &dyn Fn(Version, &str) -> Option<String>, items: &[String]) -> One {
        let claim = &query.claim;
        let module = sources.iter().position(|source| source.path == query.file.path).and_then(|index| compiled.modules.get(index));
        let find = |name: &str| compiled.records.iter().find(|record| record.name == name && record.q.is_some()).and_then(record_form);
        let Some(ir) = module.and_then(|module| find(&format!("{}::{module}::{}", semilla_normal::intent::CLAIMS_CRATE, claim.func.name))) else {
            return One { result: IntentResult::Claimed, detail: format!("the intent crate has no form of `fn {}`", claim.func.name), reads: Vec::new(), errors: Vec::new(), depends: Vec::new(), items: items.to_vec(), tests: Vec::new() };
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
        let (result, detail) = match checked.verdict {
            ClaimVerdict::Proved { method } => (IntentResult::Proved, format!("by {method} (the claim MIR of rustc)")),
            // [R75.1-3] The claim MIR gives the input of the claim, not the calls of a version on
            // its failed path: no test of the code of the user (the model of 10r1 gives one when it
            // reads the claim).
            ClaimVerdict::False { input, detail, .. } => (IntentResult::False, format!("for {input}: {detail} (no suggested test: the claim MIR gives no call of a version on its failed path)")),
            ClaimVerdict::Claimed { cause } => (IntentResult::Claimed, cause),
        };
        // [R78.4-9] A proved claim MIR explains each body of its dependence set (R77.1-1).
        let depends = if result == IntentResult::Proved { checked.depends.clone() } else { Vec::new() };
        One { result, detail, reads, errors: Vec::new(), depends, items: items.to_vec(), tests: Vec::new() }
    }
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
        return Answer { result: Some(IntentResult::Claimed), detail, configs: Vec::new(), reads, errors, depends: Vec::new(), items: Vec::new(), tests: Vec::new() };
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
    // [R75.1-3] The suggested tests of the false results, once for each name.
    let mut tests: Vec<semilla_core::run::SuggestedTest> = Vec::new();
    for (_, one) in &results {
        for test in &one.tests {
            semilla_verify::equiv::merge_test(&mut tests, test);
        }
    }
    Answer { result: Some(result), detail, configs, reads, errors, depends, items, tests }
}
