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
    let (mut checked, steps) = crate::budget::with(budget, || with_intent_model(|| check_in(solver, property, old, new)));
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
    // [R80.1-3] The inputs of the two types of a relation: the claim is about each pair of them
    // that the relation holds for (the k-th old value with the k-th new value).
    let related = match related_inputs(&side, &step, inline_all, &mut decls, &slots, &types) {
        Ok(related) => related,
        Err(Stop(cause)) => {
            CLAIM_RUN.with(|cell| cell.set(outer));
            return claimed(cause);
        }
    };
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
    for (_, term) in &related {
        body.push_str(&format!("(assert {})\n", term.s));
    }
    // [R80.1-5] A relation is a hint, never an assumption: a relation that holds for no pair of
    // the inputs makes each claim true, so such a claim proves nothing.
    if let Some(cause) = vacuous(solver, &body, &related, queries) {
        return claimed(cause);
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
            // [R80.1-3] The relation of the inputs must be exact too: else the input of the
            // model can be a pair that the relation does not hold for.
            if !fail.x || !out.pc.x || related.iter().any(|(_, term)| !term.x) {
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

/// [R80.1-3] The condition of the relation of each pair of inputs of a claim: for each relation,
/// the k-th input of its old type (or a reference to it) and the k-th input of its new type.
/// Each item: the text of the pair and the condition.
fn related_inputs(side: &Side, step: &Step, inline_all: bool, decls: &mut Decls, slots: &[V], types: &[Option<Ty>]) -> Result<Vec<(String, T)>, Stop> {
    let mut related = Vec::new();
    let inputs = decls.inputs.clone();
    // The value of an input, or the target of a reference input, with its type.
    let target = |value: &V, ty: &Ty| -> Option<(V, Ty)> {
        match ty.get()? {
            IrTy::Ref(_, inner) => match value {
                V::Ptr(loc) if loc.path.is_empty() => Some((slots.get(loc.slot)?.clone(), ty.at(*inner))),
                _ => None,
            },
            _ => Some((value.clone(), ty.clone())),
        }
    };
    for relation in relations().iter() {
        let Some((old_ty, new_ty)) = relation.types() else { continue };
        if fits(&old_ty, &new_ty, 0) {
            continue;
        }
        let of = |wanted: &Ty| -> Vec<(String, V)> { inputs.iter().filter_map(|(name, value, ty)| target(value, ty).filter(|(_, ty)| fits(wanted, ty, 0)).map(|(value, _)| (name.clone(), value))).collect() };
        let (olds, news) = (of(&old_ty), of(&new_ty));
        for ((old_name, old_value), (new_name, new_value)) in olds.into_iter().zip(news) {
            let term = relation_run(side, step, inline_all, decls, slots, types, relation, old_value, new_value, true)?;
            related.push((format!("`{}({old_name}, {new_name})`", relation.short()), term));
        }
    }
    Ok(related)
}

/// [R80.1-5] The cause of a claim whose relations hold for no pair of its inputs (an `unsat`
/// answer for the declarations, the assumptions, and the relations alone). `None`: no relation,
/// or a pair exists, or the solver gives no answer.
fn vacuous(solver: &Solver, body: &str, related: &[(String, T)], queries: &mut usize) -> Option<String> {
    if related.is_empty() {
        return None;
    }
    *queries += 1;
    match solver.check(&format!("{body}(assert true)\n"), &[]) {
        Ok((_, Answer::Unsat)) => Some(format!("vacuous: the relation {} holds for no pair of the inputs, so the claim proves nothing (a relation is a hint, never an assumption)", related.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>().join(" and "))),
        _ => None,
    }
}

/// [R80.1-4] The check of an empty `#[same]` claim on one item whose parameter types or result
/// type differ in the two versions, with the relations of the intent files (`with_relations`).
/// For each pair of inputs that correspond (one input for a parameter of one type; an old value
/// and a new value that the relation of the two types holds for), the two outcomes must
/// correspond: the same exit and kind of panic, the same effects, equal results of one type, and
/// related results of two types. The check runs the two bodies as a claim MIR runs them
/// (R78.4-1: each callee in its tree). A `&mut` parameter of a type that changes gives "claimed"
/// (batch 13c). [R80.1-5] The proof must depend on each version: a check that stays true for a
/// free result of one version is "claimed".
pub fn check_related(solver: &Solver, (old_item, new_item): (&str, &str), own: &dyn Fn(&str) -> Option<Arc<Ir>>, own_std: &BTreeMap<String, Arc<Ir>>, old: &Version, new: &Version, budget: Option<crate::budget::Budget>) -> PropertyCheck {
    let (mut checked, steps) = crate::budget::with(budget, || with_intent_model(|| related_in(solver, (old_item, new_item), own, own_std, old, new)));
    checked.steps = steps;
    checked
}

fn related_in(solver: &Solver, (old_item, new_item): (&str, &str), own: &dyn Fn(&str) -> Option<Arc<Ir>>, own_std: &BTreeMap<String, Arc<Ir>>, old: &Version, new: &Version) -> PropertyCheck {
    let started = Instant::now();
    let reads = vec![format!("{BEFORE}{old_item}"), format!("{AFTER}{new_item}")];
    let done = |verdict: ClaimVerdict, depends: Vec<String>, queries: usize| PropertyCheck { verdict, reads: reads.clone(), depends, queries, millis: started.elapsed().as_millis(), steps: 0 };
    let (Some(old_ir), Some(new_ir)) = ((old.workspace)(old_item), (new.workspace)(new_item)) else {
        return done(ClaimVerdict::Claimed { cause: "the item has no form in each version".into() }, Vec::new(), 0);
    };
    for ir in [&old_ir, &new_ir] {
        if let Some(cause) = &ir.u {
            return done(ClaimVerdict::Claimed { cause: format!("Semilla has no form of the item ({cause})") }, Vec::new(), 0);
        }
    }
    // The forms of the trees with the prefixes (the rules of `run_claim_in`).
    let cache: RefCell<BTreeMap<String, Option<Arc<Ir>>>> = RefCell::new(BTreeMap::new());
    let workspace = |name: &str| -> Option<Arc<Ir>> {
        if let Some(found) = cache.borrow().get(name) {
            return found.clone();
        }
        let form = match record_name(name) {
            (BEFORE, rest) => (old.workspace)(rest).map(|ir| Arc::new(prefixed(&ir, BEFORE))),
            (AFTER, rest) => (new.workspace)(rest).map(|ir| Arc::new(prefixed(&ir, AFTER))),
            _ => own(name),
        };
        cache.borrow_mut().insert(name.to_string(), form.clone());
        form
    };
    let mut std: BTreeMap<String, Arc<Ir>> = own_std.clone();
    for (prefix, version) in [(BEFORE, old), (AFTER, new)] {
        for (key, form) in version.std {
            std.insert(format!("{prefix}{key}"), Arc::new(prefixed(form, prefix)));
            std.entry(key.clone()).or_insert_with(|| form.clone());
        }
    }
    let changed = |name: &str| match record_name(name) {
        (BEFORE, rest) => (old.changed)(rest),
        (AFTER, rest) => (new.changed)(rest),
        _ => false,
    };
    let rename = |text: &str| -> String {
        match record_name(text) {
            (BEFORE, rest) if !(old.changed)(rest) => text[BEFORE.len()..].to_string(),
            (AFTER, rest) if !(new.changed)(rest) => text[AFTER.len()..].to_string(),
            _ => text.to_string(),
        }
    };
    let old_side = Side { ir: Arc::new(prefixed(&old_ir, BEFORE)), workspace: &workspace, std: &std, rename: &rename };
    let new_side = Side { ir: Arc::new(prefixed(&new_ir, AFTER)), workspace: &workspace, std: &std, rename: &rename };
    let step = Step { changed: &changed, changed_names: &[] };
    let names = (format!("{BEFORE}{old_item}"), format!("{AFTER}{new_item}"));
    let mut queries = 0;
    let (mut verdict, mut inlined) = related_once(solver, &old_side, &new_side, &step, false, None, &names, &mut queries);
    let mut inline_all = false;
    // [R67.3-1] The second try inlines each callee with a form.
    if matches!(&verdict, ClaimVerdict::Claimed { cause } if !crate::budget::is_stop(cause)) {
        let second = related_once(solver, &old_side, &new_side, &step, true, None, &names, &mut queries);
        if !matches!(second.0, ClaimVerdict::Claimed { .. }) {
            (verdict, inlined) = second;
            inline_all = true;
        }
    }
    // [R80.1-5] A relation never makes a proof by itself: the proof must fail for a free result
    // of each version.
    if let ClaimVerdict::Proved { method } = verdict.clone() {
        for (old_side_free, name) in [(true, &names.0), (false, &names.1)] {
            let (found, _) = related_once(solver, &old_side, &new_side, &step, inline_all, Some(old_side_free), &names, &mut queries);
            let cause = match found {
                ClaimVerdict::False { .. } => continue,
                ClaimVerdict::Claimed { cause } if cause == NO_EXACT => continue,
                ClaimVerdict::Proved { .. } => format!("the check is true for any result of `{name}`: the relations alone make it true, and a relation is a hint, never an assumption (the check alone is true by {method})"),
                ClaimVerdict::Claimed { cause } => format!("the check that the proof depends on `{name}` gives no result: {cause}"),
            };
            verdict = ClaimVerdict::Claimed { cause };
            break;
        }
    }
    let _ = inlined;
    let depends = if matches!(verdict, ClaimVerdict::Proved { .. }) { vec![names.0.clone(), names.1.clone()] } else { Vec::new() };
    done(verdict, depends, queries)
}

/// True if a value has a reference to one of the slots.
fn points_into(value: &V, slots: &[usize]) -> bool {
    match value {
        V::Ptr(loc) => slots.contains(&loc.slot),
        V::Tup(items) => items.iter().any(|item| points_into(item, slots)),
        V::Enum { f, .. } => f.iter().flatten().any(|item| points_into(item, slots)),
        _ => false,
    }
}

/// [R80.1-4] One run of the two bodies on corresponding inputs, and the query. `free`: the side
/// whose outcome is a free function of its inputs (true: the old side), for the rule R80.1-5.
#[allow(clippy::too_many_arguments)]
fn related_once(solver: &Solver, old: &Side, new: &Side, step: &Step, inline_all: bool, free: Option<bool>, names: &(String, String), queries: &mut usize) -> (ClaimVerdict, BTreeSet<String>) {
    let claimed = |cause: String| (ClaimVerdict::Claimed { cause }, BTreeSet::new());
    let (fo, fnew) = (old.ir.clone(), new.ir.clone());
    if fo.a != fnew.a || fo.l.len() <= fo.a || fnew.l.len() <= fnew.a {
        return claimed("the count of the parameters changes: an empty `#[same]` claim pairs each parameter with the parameter at its place; a property claim states the new parameters".into());
    }
    let mut decls = Decls::default();
    let (mut slots, mut types, mut muts) = (Vec::new(), Vec::new(), Vec::new());
    let (mut old_inputs, mut new_inputs) = (Vec::new(), Vec::new());
    let mut pairs: Vec<(String, V, Ty, V, Ty)> = Vec::new();
    for local in 1..=fo.a {
        let (tb, ta) = (Ty { ir: fo.clone(), id: fo.l[local] }, Ty { ir: fnew.clone(), id: fnew.l[local] });
        let name = fnew.n.get(local - 1).cloned().unwrap_or_else(|| format!("_{local}"));
        if same_type(&tb, &ta, 0, old.rename) {
            match input(&mut decls, &ta, &name, &mut slots, &mut types, &mut muts, 0) {
                Ok(value) => {
                    decls.inputs.push((name, value.clone(), ta));
                    old_inputs.push(value.clone());
                    new_inputs.push(value);
                }
                Err(Stop(cause)) => return claimed(cause),
            }
            continue;
        }
        if matches!(tb.get(), Some(IrTy::Ref(true, _))) || matches!(ta.get(), Some(IrTy::Ref(true, _))) {
            return claimed(format!("`&mut` comes in 13c: the parameter `{name}` is a `&mut` reference to a type that changes, and the relation of the value after the call needs the effects of batch 13c"));
        }
        let mut one = |ty: &Ty, text: String, decls: &mut Decls| -> Result<V, Stop> {
            let value = input(decls, ty, &text, &mut slots, &mut types, &mut muts, 0)?;
            decls.inputs.push((text, value.clone(), ty.clone()));
            Ok(value)
        };
        let (vb, va) = match (one(&tb, format!("{name}_before"), &mut decls), one(&ta, format!("{name}_after"), &mut decls)) {
            (Ok(vb), Ok(va)) => (vb, va),
            (Err(Stop(cause)), _) | (_, Err(Stop(cause))) => return claimed(cause),
        };
        old_inputs.push(vb.clone());
        new_inputs.push(va.clone());
        pairs.push((name, vb, tb, va, ta));
    }
    // [R80.1-3] The inputs of two types: each pair that their relation holds for.
    let mut related: Vec<(String, T)> = Vec::new();
    for (name, vb, tb, va, ta) in &pairs {
        match relate_values(old, step, inline_all, &mut decls, &slots, &types, (vb, tb), (va, ta), true) {
            Ok(term) => related.push((format!("of the parameter `{name}`"), term)),
            Err(Stop(cause)) => return claimed(format!("the parameter `{name}`: {cause}")),
        }
    }
    let (rb, ra) = (Ty { ir: fo.clone(), id: fo.l[0] }, Ty { ir: fnew.clone(), id: fnew.l[0] });
    // [R80.1-5] A free outcome of one version: its result and the value behind each `&mut`
    // parameter are free functions of its inputs (a reference: a new place with a free value).
    // It has no effect: a free effect differs from each outcome of the other version, so the
    // test would pass for a check that uses no result of the version.
    let (mut slots, mut types) = (slots, types);
    let mut free_out: Option<Paths> = None;
    if let Some(is_old) = free {
        let inputs = if is_old { &old_inputs } else { &new_inputs };
        let mut deep: Vec<V> = inputs.clone();
        for value in inputs {
            if let V::Ptr(loc) = value
                && let Some(target) = slots.get(loc.slot)
            {
                deep.push(target.clone());
            }
        }
        let (name, ret) = if is_old { (&names.0, &rb) } else { (&names.1, &ra) };
        let flat: Vec<T> = deep.iter().flat_map(flatten).map(|(_, term)| term).collect();
        let sorts: Vec<String> = deep.iter().flat_map(flatten).map(|(sort, _)| sort).collect();
        let key = format!("free_{}_{}", if is_old { "before" } else { "after" }, sym_name(name));
        let value = match free_value(&mut decls, ret, &format!("r_{key}"), &flat, &sorts, &mut slots, &mut types, 0) {
            Ok(value) => value,
            Err(Stop(cause)) => return claimed(cause),
        };
        let mut after = Vec::new();
        for (index, slot) in muts.iter().enumerate() {
            let Some(ty) = types.get(*slot).cloned().flatten() else { return claimed("a `&mut` parameter with no type".into()) };
            match free_value(&mut decls, &ty, &format!("m{index}_{key}"), &flat, &sorts, &mut slots, &mut types, 0) {
                Ok(value) => after.push(value),
                Err(Stop(cause)) => return claimed(cause),
            }
        }
        free_out = Some(Paths { outs: vec![PathOut { pc: t("true"), effects: Vec::new(), calls: Vec::new(), exit: Exit::Return(value), after, cut: None }], stops: Vec::new(), inlined: BTreeSet::new() });
    }
    let a = match (free, free_out.as_ref()) {
        (Some(true), Some(found)) => Paths { outs: found.outs.clone(), stops: Vec::new(), inlined: BTreeSet::new() },
        _ => run_side(old, step, inline_all, &mut decls, &old_inputs, &slots, &types, &muts, true),
    };
    let b = match (free, free_out.as_ref()) {
        (Some(false), Some(found)) => Paths { outs: found.outs.clone(), stops: Vec::new(), inlined: BTreeSet::new() },
        _ => run_side(new, step, inline_all, &mut decls, &new_inputs, &slots, &types, &muts, false),
    };
    let mut inlined = a.inlined.clone();
    inlined.extend(b.inlined.iter().cloned());
    if let Some((_, cause)) = a.stops.first().or(b.stops.first()) {
        return (ClaimVerdict::Claimed { cause: cause.clone() }, inlined);
    }
    if let Some(cause) = crate::budget::stop() {
        return (ClaimVerdict::Claimed { cause: cause.into() }, inlined);
    }
    // The pairs of paths whose outcomes do not correspond.
    let same_result = same_type(&rb, &ra, 0, old.rename);
    let mut defines = Vec::new();
    for (i, x) in a.outs.iter().enumerate() {
        defines.push(format!("(define-fun |pa{i}| () Bool {})", x.pc.s));
    }
    for (j, y) in b.outs.iter().enumerate() {
        defines.push(format!("(define-fun |pb{j}| () Bool {})", y.pc.s));
    }
    let mut differs: Vec<Vec<T>> = Vec::new();
    let mut alternatives = Vec::new();
    for (i, x) in a.outs.iter().enumerate() {
        let mut row = Vec::new();
        for (j, y) in b.outs.iter().enumerate() {
            let d = match (&x.exit, &y.exit) {
                (Exit::Return(vx), Exit::Return(vy)) if !same_result => {
                    if points_into(vx, &muts) || points_into(vy, &muts) {
                        return (ClaimVerdict::Claimed { cause: "a result that refers to a `&mut` parameter, with a relation (batch 13c)".into() }, inlined);
                    }
                    let (mut xs, mut ys) = (x.clone(), y.clone());
                    xs.exit = Exit::Return(V::Unit);
                    ys.exit = Exit::Return(V::Unit);
                    let rest = outcome_differs(&xs, &ys);
                    match relate_values(old, step, inline_all, &mut decls, &slots, &types, (vx, &rb), (vy, &ra), true) {
                        Ok(holds) => or(&[rest, not(&holds)]),
                        Err(Stop(cause)) => return (ClaimVerdict::Claimed { cause: format!("the result: {cause}") }, inlined),
                    }
                }
                _ => outcome_differs(x, y),
            };
            if lit_bool(&d) != Some(false) {
                alternatives.push(format!("(and |pa{i}| |pb{j}| {})", d.s));
            }
            row.push(d);
        }
        differs.push(row);
    }
    let mut body = String::new();
    for line in decls.lines.iter().chain(&defines) {
        body.push_str(line);
        body.push('\n');
    }
    for line in &decls.assume {
        body.push_str(&format!("(assert {line})\n"));
    }
    if decls.consts.len() > 1 {
        body.push_str(&format!("(assert (distinct {}))\n", decls.consts.join(" ")));
    }
    for (_, term) in &related {
        body.push_str(&format!("(assert {})\n", term.s));
    }
    if let Some(cause) = vacuous(solver, &body, &related, queries) {
        return (ClaimVerdict::Claimed { cause }, inlined);
    }
    body.push_str(&format!("(assert (or {} false))\n", alternatives.join(" ")));
    let mut symbols: Vec<String> = Vec::new();
    for (_, value, _) in &decls.inputs {
        terms_of(value, &mut symbols);
        if let V::Ptr(loc) = value
            && let Some(target) = slots.get(loc.slot)
        {
            terms_of(target, &mut symbols);
        }
    }
    symbols.extend((0..a.outs.len()).map(|i| format!("|pa{i}|")));
    symbols.extend((0..b.outs.len()).map(|j| format!("|pb{j}|")));
    *queries += 1;
    let answer = match solver.check(&body, &symbols) {
        Ok((_, answer)) => answer,
        Err(error) => return (ClaimVerdict::Claimed { cause: format!("the solver failed: {error}") }, inlined),
    };
    match answer {
        Answer::Unsat => (ClaimVerdict::Proved { method: method(&inlined) }, inlined),
        Answer::Unknown(reason) if crate::budget::is_stop(&reason) => (ClaimVerdict::Claimed { cause: reason }, inlined),
        Answer::Unknown(reason) => (ClaimVerdict::Claimed { cause: format!("the solver gave no answer ({reason})") }, inlined),
        Answer::Sat(model) => {
            let model: BTreeMap<String, String> = model.into_iter().collect();
            let pick = |prefix: &str, count: usize| (0..count).find(|index| model.get(&format!("|{prefix}{index}|")).map(String::as_str) == Some("true"));
            let (Some(i), Some(j)) = (pick("pa", a.outs.len()), pick("pb", b.outs.len())) else { return (ClaimVerdict::Claimed { cause: NO_EXACT.into() }, inlined) };
            let (x, y) = (&a.outs[i], &b.outs[j]);
            // [R61.4-2] An exact input only: the two paths, the difference, and the relations of
            // the inputs use only the inputs and the literals.
            if !(x.pc.x && y.pc.x && differs[i][j].x && related.iter().all(|(_, term)| term.x)) {
                return (ClaimVerdict::Claimed { cause: NO_EXACT.into() }, inlined);
            }
            let shown = |value: &V, ty: &Ty| -> String {
                match (value, ty.get()) {
                    (V::Ptr(loc), Some(IrTy::Ref(_, inner))) if loc.path.is_empty() => format!("&{}", value_text(&slots[loc.slot], Some(&ty.at(*inner)), &model)),
                    _ => value_text(value, Some(ty), &model),
                }
            };
            let input = decls.inputs.iter().map(|(name, value, ty)| format!("{name} = {}", shown(value, ty))).collect::<Vec<_>>().join(", ");
            let input = if input.is_empty() { "each input".to_string() } else { input };
            let mut_names: Vec<(String, Option<Ty>)> = muts.iter().map(|slot| (decls.mut_names.iter().find(|(at, _)| at == slot).map(|(_, name)| name.clone()).unwrap_or_else(|| "*_".into()), types[*slot].clone())).collect();
            let values = outcome_values(solver, &decls, &related, &symbols, &model, [x, y], queries);
            let before = outcome_text(x, Some(&rb), &values, y, &mut_names);
            let after = outcome_text(y, Some(&ra), &values, x, &mut_names);
            // [R75.1-3] The counterexample for a suggested test: the call of each version with
            // its own inputs (an input of a type that changes has two values).
            let side_case = |drop: &str| {
                let mut own = decls.clone();
                own.inputs.retain(|(name, ..)| !name.ends_with(drop));
                for (name, ..) in &mut own.inputs {
                    if let Some(plain) = name.strip_suffix(if drop == "_after" { "_before" } else { "_after" }) {
                        *name = plain.to_string();
                    }
                }
                run_case(&own, &slots, &types, &muts, (x, &rb), (y, &ra), &values)
            };
            let (old_case, new_case) = (side_case("_after"), side_case("_before"));
            let mut calls = Vec::new();
            for (case, side, name) in [(&old_case, semilla_core::run::RunSide::Before, &names.0), (&new_case, semilla_core::run::RunSide::After, &names.1)] {
                if let Some(call) = case.calls.iter().find(|call| call.side == side) {
                    let mut call = call.clone();
                    call.name = record_name(name).1.to_string();
                    calls.push(call);
                }
            }
            let run = semilla_core::run::RunCase { calls, blind: old_case.blind.or(new_case.blind) };
            (ClaimVerdict::False { input, detail: format!("the outcomes do not correspond: before {before}, after {after}"), run: Some(Box::new(run)) }, inlined)
        }
    }
}

/// The values of the terms of two outcomes for the inputs of a model (a second query with the
/// inputs fixed), for the text of a false result.
fn outcome_values(solver: &Solver, decls: &Decls, related: &[(String, T)], symbols: &[String], model: &BTreeMap<String, String>, outs: [&PathOut; 2], queries: &mut usize) -> BTreeMap<String, String> {
    let mut values = model.clone();
    let mut wanted = Vec::new();
    for out in outs {
        if let Exit::Return(value) = &out.exit {
            terms_of(value, &mut wanted);
        }
        for value in &out.after {
            terms_of(value, &mut wanted);
        }
    }
    if wanted.is_empty() {
        return values;
    }
    let mut fixed = String::new();
    for line in &decls.lines {
        fixed.push_str(line);
        fixed.push('\n');
    }
    for line in &decls.assume {
        fixed.push_str(&format!("(assert {line})\n"));
    }
    for (_, term) in related {
        fixed.push_str(&format!("(assert {})\n", term.s));
    }
    for symbol in symbols {
        if let Some(value) = model.get(symbol)
            && symbol.starts_with("|in ")
            && !value.contains("!val!")
        {
            fixed.push_str(&format!("(assert (= {symbol} {value}))\n"));
        }
    }
    let mut names = Vec::new();
    for (index, term) in wanted.iter().enumerate() {
        let sort = outs.iter().find_map(|out| {
            let mut all: Vec<(String, T)> = out.after.iter().flat_map(flatten).collect();
            if let Exit::Return(value) = &out.exit {
                all.extend(flatten(value));
            }
            all.into_iter().find(|(_, found)| found.s == *term).map(|(sort, _)| sort)
        });
        let Some(sort) = sort else { continue };
        fixed.push_str(&format!("(define-fun |w{index}| () {sort} {term})\n"));
        names.push((format!("|w{index}|"), term.clone()));
    }
    *queries += 1;
    let request: Vec<String> = names.iter().map(|(name, _)| name.clone()).collect();
    if let Ok((_, Answer::Sat(found))) = solver.check(&fixed, &request) {
        let found: BTreeMap<String, String> = found.into_iter().collect();
        for (name, term) in &names {
            if let Some(value) = found.get(name) {
                values.insert(term.clone(), value.clone());
            }
        }
    }
    values
}

/// [R80.1-5] A free value of a type for the free outcome of a version: a free function of the
/// inputs (as `call_value`), and a reference is a new place with a free value (a returned
/// reference of a free outcome refers to a value that no input gives).
#[allow(clippy::too_many_arguments)]
fn free_value(decls: &mut Decls, ty: &Ty, name: &str, args: &[T], sorts: &[String], slots: &mut Vec<V>, types: &mut Vec<Option<Ty>>, depth: usize) -> Result<V, Stop> {
    if depth > 6 {
        return Err(Stop("a free result of a deep type".into()));
    }
    Ok(match ty.get().cloned() {
        Some(IrTy::Ref(_, inner)) => {
            let target = free_value(decls, &ty.at(inner), &format!("{name}t"), args, sorts, slots, types, depth + 1)?;
            slots.push(target);
            types.push(Some(ty.at(inner)));
            V::Ptr(Loc { slot: slots.len() - 1, path: Vec::new() })
        }
        Some(IrTy::Tuple(items)) => {
            let mut values = Vec::new();
            for (index, item) in items.iter().enumerate() {
                values.push(free_value(decls, &ty.at(*item), &format!("{name}f{index}"), args, sorts, slots, types, depth + 1)?);
            }
            V::Tup(values)
        }
        Some(IrTy::Adt { e: false, v, .. }) => {
            let mut values = Vec::new();
            for (index, item) in v.first().cloned().unwrap_or_default().iter().enumerate() {
                values.push(free_value(decls, &ty.at(*item), &format!("{name}f{index}"), args, sorts, slots, types, depth + 1)?);
            }
            V::Tup(values)
        }
        Some(IrTy::Adt { e: true, v, d, dt, .. }) => {
            let (w, ds) = discriminants(&ty.ir, &d, dt);
            let discriminant = Ty { ir: Arc::new(Ir { t: vec![IrTy::Int(w, false)], ..Ir::default() }), id: 0 };
            let V::Bv(discr, _) = call_value(decls, &discriminant, &format!("{name}d"), args, sorts, 0)? else { return Err(Stop("a free discriminant".into())) };
            let valid: Vec<String> = ds.iter().map(|value| format!("(= {} {})", discr.s, bv_lit(*value, w).s)).collect();
            if !valid.is_empty() {
                decls.assume.push(format!("(or {} false)", valid.join(" ")));
            }
            let mut fields = Vec::new();
            for (vi, variant) in v.iter().enumerate() {
                let mut values = Vec::new();
                for (index, item) in variant.iter().enumerate() {
                    values.push(free_value(decls, &ty.at(*item), &format!("{name}v{vi}f{index}"), args, sorts, slots, types, depth + 1)?);
                }
                fields.push(values);
            }
            V::Enum { d: discr, w, ds, f: fields }
        }
        _ => call_value(decls, ty, name, args, sorts, 0)?,
    })
}
