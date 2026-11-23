//! The walk of the reverse call graph from the changed bodies of a step (design 61.10, part A).
//!
//! "Equivalent" is a statement about the program, not about the text of one body. A body whose
//! own MIR is equal can change its result through a callee that the step changes. So the walk
//! starts at each body that the step changes (the start set) and follows the reverse call graph
//! of the two trees: a direct call, a call of a generic instance, a trait method call that can
//! resolve to a changed impl (also through `dyn`), a closure and the body that makes it. The
//! walk stops at the workspace border: the records have only the bodies of the workspace.
//!
//! Each reached caller gets one check in each configuration, with each changed callee inlined
//! (the check of `semilla-smt`, `mir.rs`): `different through X`, `equivalent (proved after
//! inlining X)`, or `reached through X, not decided (…)`. A caller that is not equivalent is a
//! changed body for its own callers; an equivalent caller stops the walk on that path.
//!
//! The soundness rule: the check treats a callee as an effect (the same call with the same
//! arguments gives the same result) only if the callee and each body that it can reach are
//! unchanged, or proved equivalent. So each body of the full reverse closure of the start set
//! that the walk did not prove equivalent (also a body past a limit) is in the set of the
//! bodies that each check must inline (`must_inline`). A callee of that set with no form, too
//! deep, or recursive gives "not decided".

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use semilla_verify::equiv::{Reached, ReachReport, Verdict};

use super::{EquivSide, NormalOracle};

/// The depth limit of the walk: a caller further from the start set gets no check.
pub const MAX_DEPTH: usize = 4;
/// The most checked bodies of one configuration.
pub const MAX_BODIES: usize = 64;
/// The time budget of the walk of one step (each configuration).
pub const MAX_TIME: std::time::Duration = std::time::Duration::from_secs(20);
/// The most bodies of the reverse closure (a guard for a very large workspace).
const MAX_CLOSURE: usize = 20_000;

/// The walk of one configuration.
#[derive(Clone, Debug, Default)]
pub struct ConfigReach {
    /// The names that each check must inline: the start set and each body of the reverse
    /// closure that the walk did not prove equivalent. Both the old and the new names.
    pub must_inline: Arc<BTreeSet<String>>,
    /// The trait method of each impl method of the start set and of the closure: a call of the
    /// trait method through a type parameter or `dyn` can resolve to it.
    pub trait_methods: BTreeMap<String, String>,
    /// The reached bodies with a result, in the order of the walk.
    pub bodies: Vec<(String, Vec<String>, Vec<String>, Verdict)>,
    /// The limits that stopped the walk.
    pub stops: Vec<String>,
    /// For each body of the start set: the reached bodies that are not equivalent, through a
    /// chain of such bodies.
    pub from: BTreeMap<String, BTreeSet<String>>,
}

/// The trait method path of an impl method record: `<shop::Food as shop::Tax>::rate` with
/// `impl_of` = `[shop::Food, shop::Tax]` gives `shop::Tax::rate`.
fn trait_method(record: &semilla_normal::BodyRecord) -> Option<String> {
    if record.impl_of.len() != 2 || !record.name.starts_with('<') {
        return None;
    }
    let method = record.name.rsplit("::").next()?;
    Some(format!("{}::{method}", record.impl_of[1]))
}

/// The names of the bodies of one side, in the namespace of the new tree.
struct Graph {
    /// callee → callers.
    callers: BTreeMap<String, BTreeSet<String>>,
    /// caller → callees (the forward edges, for the order of the checks and the cause).
    callees: BTreeMap<String, BTreeSet<String>>,
    /// An impl method → its trait method.
    trait_of: BTreeMap<String, String>,
}

impl Graph {
    fn new(sides: [&EquivSide; 2], rename: &dyn Fn(&str) -> String) -> Graph {
        let mut graph = Graph { callers: BTreeMap::new(), callees: BTreeMap::new(), trait_of: BTreeMap::new() };
        for (index, side) in sides.iter().enumerate() {
            let map = |name: &str| if index == 0 { rename(name) } else { name.to_string() };
            for (name, list) in &side.records {
                let caller = map(name);
                for record in list {
                    for callee in &record.callees {
                        graph.add(map(callee), caller.clone());
                    }
                    // A generic body calls the callee that its instance resolves.
                    for instance in &record.inst {
                        for (_, resolved) in &instance.c {
                            graph.add(map(resolved), map(&instance.g));
                        }
                    }
                    if let Some(method) = trait_method(record) {
                        graph.trait_of.insert(caller.clone(), map(&method));
                    }
                }
            }
        }
        graph
    }

    fn add(&mut self, callee: String, caller: String) {
        if callee == caller {
            return;
        }
        self.callees.entry(caller.clone()).or_default().insert(callee.clone());
        self.callers.entry(callee).or_default().insert(caller);
    }

    /// The direct callers of a body: the callers of its name, and for an impl method the
    /// callers of its trait method (a call through a type parameter or `dyn`).
    fn callers_of(&self, name: &str) -> BTreeSet<String> {
        let mut out = self.callers.get(name).cloned().unwrap_or_default();
        if let Some(method) = self.trait_of.get(name) {
            out.extend(self.callers.get(method).cloned().unwrap_or_default());
        }
        out.remove(name);
        out
    }

    /// The callees of a body that are in a set: a name of the set, or the trait method of an
    /// impl method of the set.
    fn callees_in(&self, name: &str, set: &BTreeSet<String>) -> Vec<String> {
        let mut out = BTreeSet::new();
        for callee in self.callees.get(name).into_iter().flatten() {
            if set.contains(callee) {
                out.insert(callee.clone());
            }
            for (method, of) in &self.trait_of {
                if of == callee && set.contains(method) {
                    out.insert(method.clone());
                }
            }
        }
        out.into_iter().collect()
    }
}

/// The start set of one configuration: each body with a different N0 text or of one side only
/// (an edit, a changed graph, a changed instance, a changed closure, a changed constant value
/// in the text of a body), each body with a different constant value (`cv`), and each impl
/// method of a type that a `dyn` candidate set adds or removes. The new names.
fn start_set(old: &EquivSide, new: &EquivSide, renames: &[(String, String)], rename: &dyn Fn(&str) -> String) -> BTreeSet<String> {
    let mut start: BTreeSet<String> = super::changed_names(old, new, renames).into_iter().map(|name| rename(&name)).collect();
    for (name, list) in &new.records {
        let old_list = old.records.get(rename_back(name, renames).as_str());
        let values = |list: &Vec<&semilla_normal::BodyRecord>| list.iter().map(|record| record.cv.clone()).collect::<BTreeSet<_>>();
        if let Some(old_list) = old_list
            && values(old_list) != values(list)
        {
            start.insert(name.to_string());
        }
    }
    // The candidates of a call through `dyn Trait` (R61.3-3).
    let casts = |side: &EquivSide, map: &dyn Fn(&str) -> String| -> BTreeSet<(String, String)> {
        side.records.values().flatten().flat_map(|record| record.dyn_casts.iter().map(|(t, ty)| (map(t), map(ty)))).collect()
    };
    let identity = |text: &str| text.to_string();
    let (before, after) = (casts(old, rename), casts(new, &identity));
    for (trait_path, ty) in before.symmetric_difference(&after) {
        for list in new.records.values().chain(old.records.values()) {
            for record in list {
                if record.impl_of.len() == 2 && rename(&record.impl_of[1]) == *trait_path && rename(&record.impl_of[0]) == *ty {
                    start.insert(rename(&record.name));
                }
            }
        }
    }
    start
}

/// The old name of a new name (the rename map of the step backward).
fn rename_back(name: &str, renames: &[(String, String)]) -> String {
    if renames.is_empty() {
        return name.to_string();
    }
    let back: Vec<(String, String)> = renames.iter().map(|(old, new)| (new.clone(), old.clone())).collect();
    semilla_normal::paths::rename(name, &back)
}

impl NormalOracle {
    /// The walk of one configuration (design 61.10, part A).
    pub(super) fn reach_config(&self, solver: &semilla_smt::Solver, old: &EquivSide, new: &EquivSide, renames: &[(String, String)], started: std::time::Instant, limit: std::time::Duration) -> ConfigReach {
        let rename = |text: &str| if renames.is_empty() { text.to_string() } else { semilla_normal::paths::rename(text, renames) };
        let start = start_set(old, new, renames, &rename);
        let graph = Graph::new([old, new], &rename);
        // The full reverse closure, with the depth and the first parent of each body.
        let mut depth: BTreeMap<String, usize> = BTreeMap::new();
        let mut parent: BTreeMap<String, String> = BTreeMap::new();
        let mut queue: std::collections::VecDeque<String> = start.iter().cloned().collect();
        for name in &start {
            depth.insert(name.clone(), 0);
        }
        while let Some(name) = queue.pop_front() {
            if depth.len() > MAX_CLOSURE {
                break;
            }
            let next = depth[&name] + 1;
            for caller in graph.callers_of(&name) {
                if depth.contains_key(&caller) {
                    continue;
                }
                depth.insert(caller.clone(), next);
                parent.insert(caller.clone(), name.clone());
                queue.push_back(caller);
            }
        }
        let closure: Vec<String> = depth.keys().filter(|name| !start.contains(*name)).cloned().collect();
        // Each body of the closure is in the set to inline until the walk proves it equivalent.
        let mut must: BTreeSet<String> = start.iter().cloned().collect();
        must.extend(closure.iter().cloned());
        for name in start.iter().chain(&closure) {
            let back = rename_back(name, renames);
            if back != *name {
                must.insert(back);
            }
        }
        let mut out = ConfigReach { trait_methods: graph.trait_of.clone(), ..ConfigReach::default() };
        // The order of the checks: a body after its callees in the closure; in a cycle, the
        // body nearest to the start set first.
        let mut pending: BTreeSet<String> = closure.iter().cloned().collect();
        let mut proved: BTreeSet<String> = BTreeSet::new();
        let (mut deep, mut over_count, mut over_time) = (0usize, 0usize, 0usize);
        let mut checked = 0usize;
        while !pending.is_empty() {
            let mut ready: Vec<String> = pending.iter().filter(|name| graph.callees.get(*name).is_none_or(|callees| callees.iter().all(|callee| !pending.contains(callee) || callee == *name))).cloned().collect();
            if ready.is_empty() {
                let first = pending.iter().min_by_key(|name| (depth[*name], (*name).clone())).cloned().unwrap_or_default();
                ready.push(first);
            }
            ready.sort_by_key(|name| (depth[name], name.clone()));
            // The checks of one round run in parallel; each sees the set of the round before.
            let snapshot: Arc<BTreeSet<String>> = Arc::new(must.clone());
            let mut jobs: Vec<(String, Vec<String>)> = Vec::new();
            for name in &ready {
                pending.remove(name);
                let through = graph.callees_in(name, &snapshot);
                if through.is_empty() {
                    // Each changed callee is proved equivalent: the change does not reach it.
                    proved.insert(name.clone());
                    must.remove(name);
                    must.remove(&rename_back(name, renames));
                    continue;
                }
                if depth[name] > MAX_DEPTH {
                    deep += 1;
                    continue;
                }
                if checked >= MAX_BODIES {
                    over_count += 1;
                    continue;
                }
                if started.elapsed() > limit {
                    over_time += 1;
                    continue;
                }
                checked += 1;
                jobs.push((name.clone(), through));
            }
            let results = self.reach_checks(solver, old, new, renames, &snapshot, &jobs);
            for ((name, through), verdict) in jobs.into_iter().zip(results) {
                if matches!(verdict, Verdict::Equivalent(_)) {
                    proved.insert(name.clone());
                    must.remove(&name);
                    must.remove(&rename_back(&name, renames));
                }
                let mut chain = vec![name.clone()];
                while let Some(next) = parent.get(chain.last().map(String::as_str).unwrap_or_default()) {
                    chain.push(next.clone());
                }
                out.bodies.push((name, through, chain, verdict));
            }
        }
        if deep > 0 {
            out.stops.push(format!("the walk stopped at depth {MAX_DEPTH}: {deep} more caller(s) not checked"));
        }
        if over_count > 0 {
            out.stops.push(format!("the walk stopped at {MAX_BODIES} checked bodies: {over_count} more caller(s) not checked"));
        }
        if over_time > 0 {
            out.stops.push(format!("the walk stopped at the time budget ({} s): {over_time} more caller(s) not checked", limit.as_secs()));
        }
        // The bodies that each changed body reaches: a chain of reached bodies that are not
        // equivalent.
        let open: BTreeSet<&str> = out.bodies.iter().filter(|(_, _, _, verdict)| !matches!(verdict, Verdict::Equivalent(_))).map(|(name, _, _, _)| name.as_str()).collect();
        for name in &start {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut todo = vec![name.clone()];
            while let Some(next) = todo.pop() {
                for caller in graph.callers_of(&next) {
                    if open.contains(caller.as_str()) && seen.insert(caller.clone()) {
                        todo.push(caller);
                    }
                }
            }
            if !seen.is_empty() {
                out.from.insert(name.clone(), seen);
            }
        }
        out.must_inline = Arc::new(must);
        out
    }

    /// The checks of one round of the walk, in up to 8 threads: each caller with each body of
    /// `must` inlined, and no proof by an equal MIR text.
    fn reach_checks(&self, solver: &semilla_smt::Solver, old: &EquivSide, new: &EquivSide, renames: &[(String, String)], must: &Arc<BTreeSet<String>>, jobs: &[(String, Vec<String>)]) -> Vec<Verdict> {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: std::sync::Mutex<Vec<(usize, Verdict)>> = std::sync::Mutex::new(Vec::new());
        let threads = std::thread::available_parallelism().map(|count| count.get()).unwrap_or(4).clamp(1, 8).min(jobs.len().max(1));
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| loop {
                    let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((name, _)) = jobs.get(at) else { break };
                    let verdict = self.reach_one(solver, old, new, renames, must, name);
                    results.lock().unwrap_or_else(|error| error.into_inner()).push((at, verdict));
                });
            }
        });
        let mut results = results.into_inner().unwrap_or_else(|error| error.into_inner());
        results.sort_by_key(|(at, _)| *at);
        results.into_iter().map(|(_, verdict)| verdict).collect()
    }

    /// The check of one reached caller. A name with more than one source body (two packages
    /// with a target of one kind and one name, R61.14-1) has a check for each: the name is
    /// equivalent only if each one is, and a difference of one is the result.
    fn reach_one(&self, solver: &semilla_smt::Solver, old: &EquivSide, new: &EquivSide, renames: &[(String, String)], must: &Arc<BTreeSet<String>>, name: &str) -> Verdict {
        let bodies = new.bodies(name);
        if bodies.is_empty() {
            return Verdict::NotDecided("the new tree has no record of the body".into());
        }
        let mut result: Option<Verdict> = None;
        for b in bodies {
            let verdict = match old.find(&rename_back(name, renames), &b.target) {
                None => Verdict::NotDecided("the old tree has no record of the body".into()),
                Some(a) => match self.check_pair(solver, a, b, old, new, renames, true, must, false) {
                    Some(answer) => answer.verdict,
                    None => Verdict::NotDecided("no check".into()),
                },
            };
            let rank = |verdict: &Verdict| match verdict {
                Verdict::Different { .. } => 2,
                Verdict::NotDecided(_) | Verdict::NotChecked(_) => 1,
                Verdict::Equivalent(_) => 0,
            };
            if result.as_ref().is_none_or(|known| rank(&verdict) > rank(known)) {
                result = Some(verdict);
            }
        }
        result.unwrap_or_else(|| Verdict::NotDecided("no check".into()))
    }
}

/// The report of the walk over the configurations: one entry for each reached body, with the
/// result of each configuration (design 61.10, A3).
pub fn report(configs: &[(String, ConfigReach)]) -> ReachReport {
    let mut bodies: Vec<Reached> = Vec::new();
    let mut stops: Vec<String> = Vec::new();
    let mut stop_configs: Vec<(String, Vec<String>)> = Vec::new();
    for (config, reach) in configs {
        for (name, through, chain, verdict) in &reach.bodies {
            match bodies.iter_mut().find(|known| &known.name == name) {
                Some(known) => {
                    for callee in through {
                        if !known.through.contains(callee) {
                            known.through.push(callee.clone());
                        }
                    }
                    known.results.push((config.clone(), verdict.clone()));
                }
                None => bodies.push(Reached { name: name.clone(), through: through.clone(), chain: chain.clone(), results: vec![(config.clone(), verdict.clone())] }),
            }
        }
        for stop in &reach.stops {
            match stop_configs.iter_mut().find(|(known, _)| known == stop) {
                Some((_, list)) => list.push(config.clone()),
                None => stop_configs.push((stop.clone(), vec![config.clone()])),
            }
        }
    }
    // A limit of each configuration has one line.
    for (stop, list) in stop_configs {
        stops.push(if list.len() == configs.len() { stop } else { format!("{stop} (in {})", list.join(", ")) });
    }
    let mut from: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, reach) in configs {
        for (name, list) in &reach.from {
            let known = from.entry(name.clone()).or_default();
            for body in list {
                if !known.contains(body) {
                    known.push(body.clone());
                }
            }
        }
    }
    // The order of the walk.
    let order: BTreeMap<&str, usize> = bodies.iter().enumerate().map(|(index, body)| (body.name.as_str(), index)).collect();
    for list in from.values_mut() {
        list.sort_by_key(|name| order.get(name.as_str()).copied().unwrap_or(usize::MAX));
    }
    ReachReport { bodies, stops, from }
}
