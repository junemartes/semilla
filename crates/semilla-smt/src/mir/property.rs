//! The check of a claim as MIR (design 78, part D).
//!
//! Rustc compiles the claim function of the intent crate, and the driver writes its form, as
//! the form of each body of a tree. The check runs that form on symbolic inputs with the machine
//! of the equivalence check:
//!
//! - [R78.4-1] a call `before::PATH(…)` runs the body of the old tree, and `after::PATH(…)` the
//!   body of the new tree: the record names, so each callee of an inlined body stays in its tree;
//!   a callee that the step changes is inlined (R67.3-5); one function that the step does not
//!   change is one function in the two trees;
//! - [R78.4-2] a function of the prelude has the meaning of its definition (`same`), or the
//!   claim is "claimed" with the cause;
//! - [R78.4-4] the claim is proved only if no path of the form returns `false`, panics, or
//!   aborts, for any input (an `unsat` answer); it is false only with an exact input (R61.4-2);
//!   each stop of a path makes it "claimed" with the cause;
//! - [R78.4-5] a proof of a claim on an item must depend on each version of the item (R69.4-1
//!   and R74.2-1): the claim calls the two versions, and it is not true for a free result of one
//!   version.

use std::cell::RefCell;

use super::claim::ClaimVerdict;
use super::*;

/// One tree for the check of a claim MIR.
pub struct Version<'a> {
    /// The form of a body of the tree by its record name (`shop::parse`).
    pub workspace: &'a dyn Fn(&str) -> Option<Arc<Ir>>,
    /// The forms of the small callees of the standard library and of the instances, by the
    /// callee text of the tree.
    pub std: &'a BTreeMap<String, Arc<Ir>>,
    /// True if the step changes the body with this record name.
    pub changed: &'a dyn Fn(&str) -> bool,
}

/// One claim function of the intent crate.
pub struct Property<'a> {
    /// The form of the claim function.
    pub ir: Arc<Ir>,
    /// The forms of the other bodies of the intent crate (closures, relations), by record name.
    pub own: &'a dyn Fn(&str) -> Option<Arc<Ir>>,
    /// The item of the claim: its record name in the old tree and in the new tree.
    pub item: (Option<String>, Option<String>),
    /// The forms of the small callees of the standard library of the claim (its own records).
    pub std: &'a BTreeMap<String, Arc<Ir>>,
}

/// The result of the check of one claim MIR.
#[derive(Clone, Debug)]
pub struct PropertyCheck {
    pub verdict: ClaimVerdict,
    /// The bodies of the trees that the claim calls (`before::shop::parse`).
    pub reads: Vec<String>,
    /// [R78.4-9] The bodies that a proof uses (R77.1-1): `before::NAME` and `after::NAME`.
    pub depends: Vec<String>,
    pub queries: usize,
    pub millis: u128,
    /// The solver steps of the check (R77.2-1).
    pub steps: u64,
}

/// The cause of a claim that can fail only for an input that the model does not give exactly.
pub const NO_EXACT: &str = "the claim can fail for an input that depends on the result of a call or on a value that the check does not model (no exact input)";

const BEFORE: &str = "before::";
const AFTER: &str = "after::";

/// A copy of a form of a tree whose callees have the prefix of the tree (`before::shop::g`).
fn prefixed(ir: &Ir, prefix: &str) -> Ir {
    let mut ir = ir.clone();
    for block in &mut ir.b {
        if let IrTermKind::Call(callee, ..) = &mut block.t.k {
            if let Some(name) = &mut callee.w {
                *name = format!("{prefix}{name}");
            }
            // An instance of a generic function of the tree has its form under its callee text.
            if callee.g {
                callee.p = format!("{prefix}{}", callee.p);
            }
            for (_, name) in &mut callee.q {
                *name = format!("{prefix}{name}");
            }
        }
    }
    ir
}

/// The record name of a callee text: no version prefix, no generic arguments.
fn record_name(text: &str) -> (&str, &str) {
    let (version, rest) = if let Some(rest) = text.strip_prefix(BEFORE) {
        (BEFORE, rest)
    } else if let Some(rest) = text.strip_prefix(AFTER) {
        (AFTER, rest)
    } else {
        ("", text)
    };
    (version, rest.split("::<").next().unwrap_or(rest))
}

/// The callees of the trees that a form and the forms of its own bodies call directly.
fn direct_calls(ir: &Ir, own: &dyn Fn(&str) -> Option<Arc<Ir>>, out: &mut BTreeSet<String>, depth: usize) {
    for block in &ir.b {
        if let IrTermKind::Call(callee, ..) = &block.t.k {
            let Some(name) = &callee.w else { continue };
            if name.starts_with(BEFORE) || name.starts_with(AFTER) {
                out.insert(record_name(name).0.to_string() + record_name(name).1);
            } else if depth < 4
                && let Some(form) = own(name)
            {
                direct_calls(&form, own, out, depth + 1);
            }
            // [R78.4-6] A closure that a call takes (a prelude function runs it).
            for (_, closure) in &callee.q {
                if depth < 4
                    && let Some(form) = own(closure)
                {
                    direct_calls(&form, own, out, depth + 1);
                }
            }
        }
    }
}

/// [R78.4-4] Checks one claim MIR against the two trees, in a budget of solver steps (R77.2-1:
/// no time decides a result).
pub fn check_property(solver: &Solver, property: &Property, old: &Version, new: &Version, budget: Option<crate::budget::Budget>) -> PropertyCheck {
    let (mut checked, steps) = crate::budget::with(budget, || check_in(solver, property, old, new));
    checked.steps = steps;
    checked
}

fn check_in(solver: &Solver, property: &Property, old: &Version, new: &Version) -> PropertyCheck {
    let started = Instant::now();
    let mut queries = 0;
    let mut reads = BTreeSet::new();
    direct_calls(&property.ir, property.own, &mut reads, 0);
    let attempt = |inline_all: bool, free: Option<String>, queries: &mut usize| match free {
        Some(name) => with_free(false, &name, || run_claim(solver, property, old, new, inline_all, queries)),
        None => run_claim(solver, property, old, new, inline_all, queries),
    };
    let (mut verdict, mut inlined) = attempt(false, None, &mut queries);
    let mut inline_all = false;
    // [R67.3-1] The second try inlines each callee with a form.
    if matches!(&verdict, ClaimVerdict::Claimed { cause } if !crate::budget::is_stop(cause)) {
        let second = attempt(true, None, &mut queries);
        if !matches!(second.0, ClaimVerdict::Claimed { .. }) {
            (verdict, inlined) = second;
            inline_all = true;
        }
    }
    // [R78.4-5] The dependence rules of a proof (R69.4-1, then R74.2-1).
    let versions = [(BEFORE, &property.item.0), (AFTER, &property.item.1)];
    if let ClaimVerdict::Proved { method } = verdict.clone() {
        let missing: Vec<String> = versions.iter().filter(|(prefix, name)| name.as_ref().is_none_or(|name| !reads.contains(&format!("{prefix}{name}")))).map(|(prefix, name)| format!("`{prefix}{}`", name.as_deref().unwrap_or("ITEM"))).collect();
        if !missing.is_empty() {
            verdict = ClaimVerdict::Claimed { cause: format!("the claim does not call {}: a proof of a claim on an item needs the two versions of the item (the body alone is true by {method})", missing.join(" and ")) };
        } else {
            for (prefix, name) in versions {
                let Some(name) = name else { continue };
                let (found, _) = attempt(inline_all, Some(format!("{prefix}{name}")), &mut queries);
                let cause = match found {
                    ClaimVerdict::False { .. } => continue,
                    ClaimVerdict::Claimed { cause } if cause == NO_EXACT => continue,
                    ClaimVerdict::Proved { .. } => format!("the claim is true for any result of `{prefix}{name}`: it stays proved when each call of that version returns a free value with a free effect (the body alone is true by {method})"),
                    ClaimVerdict::Claimed { cause } => format!("the check that the claim depends on `{prefix}{name}` gives no result: {cause}"),
                };
                verdict = ClaimVerdict::Claimed { cause };
                break;
            }
        }
    }
    // [R78.4-9] The dependence set of a proof (R77.1-1 in the MIR form): the two versions of the
    // item, and each other changed body of a tree that the proof inlines and whose free result
    // makes the claim fail. A body whose check gives no answer is not in the set.
    let mut depends: Vec<String> = Vec::new();
    if matches!(verdict, ClaimVerdict::Proved { .. }) {
        for (prefix, name) in versions {
            if let Some(name) = name {
                depends.push(format!("{prefix}{name}"));
            }
        }
        let mut candidates: Vec<String> = inlined
            .iter()
            .filter(|name| match record_name(name) {
                (BEFORE, rest) => (old.changed)(rest),
                (AFTER, rest) => (new.changed)(rest),
                _ => false,
            })
            .map(|name| format!("{}{}", record_name(name).0, record_name(name).1))
            .filter(|name| !depends.contains(name))
            .collect();
        // Each name, the old version first (the order of R77.1-1).
        candidates.sort_by_key(|name| (record_name(name).1.to_string(), record_name(name).0 != BEFORE));
        for name in candidates.into_iter().take(MAX_DEPENDENCE) {
            let (found, _) = attempt(inline_all, Some(name.clone()), &mut queries);
            let uses = match &found {
                ClaimVerdict::False { .. } => true,
                ClaimVerdict::Claimed { cause } => cause == NO_EXACT,
                ClaimVerdict::Proved { .. } => false,
            };
            if uses && !depends.contains(&name) {
                depends.push(name);
            }
        }
    }
    PropertyCheck { verdict, reads: reads.into_iter().collect(), depends, queries, millis: started.elapsed().as_millis(), steps: 0 }
}

/// The most bodies that the dependence check of one proof tries (other than the item).
const MAX_DEPENDENCE: usize = 16;

/// One run of the claim form and the solver query.
fn run_claim(solver: &Solver, property: &Property, old: &Version, new: &Version, inline_all: bool, queries: &mut usize) -> (ClaimVerdict, BTreeSet<String>) {
    let mut inlined = BTreeSet::new();
    let verdict = run_claim_in(solver, property, old, new, inline_all, queries, &mut inlined);
    (verdict, inlined)
}

#[allow(clippy::too_many_arguments)]
fn run_claim_in(solver: &Solver, property: &Property, old: &Version, new: &Version, inline_all: bool, queries: &mut usize, inlined: &mut BTreeSet<String>) -> ClaimVerdict {
    let claimed = |cause: String| ClaimVerdict::Claimed { cause };
    if let Some(cause) = &property.ir.u {
        return claimed(format!("Semilla has no form of the claim ({cause})"));
    }
    // The forms of the trees with the prefixes, made one time each.
    let cache: RefCell<BTreeMap<String, Option<Arc<Ir>>>> = RefCell::new(BTreeMap::new());
    let workspace = |name: &str| -> Option<Arc<Ir>> {
        if let Some(found) = cache.borrow().get(name) {
            return found.clone();
        }
        let form = match record_name(name) {
            (BEFORE, rest) => (old.workspace)(rest).map(|ir| Arc::new(prefixed(&ir, BEFORE))),
            (AFTER, rest) => (new.workspace)(rest).map(|ir| Arc::new(prefixed(&ir, AFTER))),
            _ => (property.own)(name),
        };
        cache.borrow_mut().insert(name.to_string(), form.clone());
        form
    };
    let mut std: BTreeMap<String, Arc<Ir>> = property.std.clone();
    for (prefix, version) in [(BEFORE, old), (AFTER, new)] {
        for (key, form) in version.std {
            std.insert(format!("{prefix}{key}"), Arc::new(prefixed(form, prefix)));
            // A small callee of the standard library: one form for the two trees (one toolchain).
            std.entry(key.clone()).or_insert_with(|| form.clone());
        }
    }
    let changed = |name: &str| match record_name(name) {
        (BEFORE, rest) => (old.changed)(rest),
        (AFTER, rest) => (new.changed)(rest),
        _ => false,
    };
    // One function that the step does not change is one function in the two trees.
    let rename = |text: &str| -> String {
        match record_name(text) {
            (BEFORE, rest) if !(old.changed)(rest) => text[BEFORE.len()..].to_string(),
            (AFTER, rest) if !(new.changed)(rest) => text[AFTER.len()..].to_string(),
            _ => text.to_string(),
        }
    };
    let side = Side { ir: property.ir.clone(), workspace: &workspace, std: &std, rename: &rename };
    let step = Step { changed: &changed, changed_names: &[] };
    let mut decls = Decls::default();
    let (mut slots, mut types, mut muts, mut inputs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for local in 1..=property.ir.a {
        let Some(id) = property.ir.l.get(local) else { return claimed("the form of the claim has no type for a parameter".into()) };
        let name = property.ir.n.get(local - 1).cloned().unwrap_or_else(|| format!("_{local}"));
        let ty = Ty { ir: property.ir.clone(), id: *id };
        match input(&mut decls, &ty, &name, &mut slots, &mut types, &mut muts, 0) {
            Ok(value) => {
                decls.inputs.push((name, value.clone(), ty));
                inputs.push(value);
            }
            Err(Stop(cause)) => return claimed(cause),
        }
    }
    let outer = CLAIM_RUN.with(|cell| cell.replace(true));
    let paths = run_side(&side, &step, inline_all, &mut decls, &inputs, &slots, &types, &muts, false);
    CLAIM_RUN.with(|cell| cell.set(outer));
    inlined.extend(paths.inlined.iter().cloned());
    if let Some((_, cause)) = paths.stops.first() {
        return claimed(cause.clone());
    }
    if let Some(cause) = crate::budget::stop() {
        return claimed(cause.into());
    }
    // The query: an input and a path that returns `false`, panics, or aborts.
    let mut fails = Vec::new();
    // [R78.4-7] A drop of a value of the claim is not a panic of the claim (as in the model of
    // 10r1, which has no drops): the engine forks an inexact panic for each drop of a value with
    // a destructor. A destructor that panics is outside the model.
    let outs: Vec<&PathOut> = paths.outs.iter().filter(|out| !matches!(&out.exit, Exit::Panic { kind, .. } if kind.starts_with("call `drop "))).collect();
    for out in outs.iter().copied() {
        let fail = match &out.exit {
            Exit::Return(V::Bool(value)) => and(&[out.pc.clone(), not(value)]),
            Exit::Return(_) => return claimed("the claim does not return a `bool` value that the check models".into()),
            Exit::Panic { .. } | Exit::Abort => out.pc.clone(),
            Exit::Cut => return claimed("a loop in the claim".into()),
        };
        fails.push(fail);
    }
    let mut body = String::new();
    for line in &decls.lines {
        body.push_str(line);
        body.push('\n');
    }
    for (index, fail) in fails.iter().enumerate() {
        body.push_str(&format!("(define-fun |f{index}| () Bool {})\n", fail.s));
    }
    for line in &decls.assume {
        body.push_str(&format!("(assert {line})\n"));
    }
    if decls.consts.len() > 1 {
        body.push_str(&format!("(assert (distinct {}))\n", decls.consts.join(" ")));
    }
    let names: Vec<String> = (0..fails.len()).map(|index| format!("|f{index}|")).collect();
    body.push_str(&format!("(assert (or {} false))\n", names.join(" ")));
    let mut symbols: Vec<String> = Vec::new();
    for (_, value, _) in &decls.inputs {
        terms_of(value, &mut symbols);
        if let V::Ptr(loc) = value
            && let Some(target) = slots.get(loc.slot)
        {
            terms_of(target, &mut symbols);
        }
    }
    symbols.extend(names.iter().cloned());
    *queries += 1;
    let answer = match solver.check(&body, &symbols) {
        Ok((_, answer)) => answer,
        Err(error) => return claimed(format!("the solver failed: {error}")),
    };
    match answer {
        Answer::Unsat => ClaimVerdict::Proved { method: if paths.inlined.is_empty() { "SMT".into() } else { format!("SMT after inlining {}", paths.inlined.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")) } },
        Answer::Unknown(reason) => claimed(format!("the solver gave no answer ({reason})")),
        Answer::Sat(model) => {
            let model: BTreeMap<String, String> = model.into_iter().collect();
            // [R61.4-2] The failed path must be exact: its condition and its failure use only
            // the inputs and the literals.
            let failed = outs.iter().copied().zip(&fails).enumerate().find(|(index, _)| model.get(&format!("|f{index}|")).map(String::as_str) == Some("true"));
            let Some((_, (out, fail))) = failed else { return claimed(NO_EXACT.into()) };
            if !fail.x || !out.pc.x {
                return claimed(NO_EXACT.into());
            }
            let input = decls.inputs.iter().map(|(name, value, ty)| format!("{name} = {}", value_text(value, Some(ty), &model))).collect::<Vec<_>>().join(", ");
            let detail = match &out.exit {
                Exit::Panic { .. } => format!("the claim panics ({})", panic_text(&out.exit)),
                Exit::Abort => "the claim aborts".to_string(),
                _ => "the claim returns `false`".to_string(),
            };
            ClaimVerdict::False { input, detail, run: None }
        }
    }
}
