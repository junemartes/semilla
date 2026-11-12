//! The rival rule (batch 5q).
//!
//! The method lookup of the compiler gives a rank to each associated item with the name of a
//! call. The item with the highest rank is the winner. A **rival** is a different item with
//! the same name that gets a higher rank if a trait fact changes: a bound of its `impl` starts
//! to hold in a different build configuration. Then the call has a different meaning, and the
//! text of the call does not show it.
//!
//! The rank order of a method call (`rustc_hir_typeck::method::probe`):
//!
//! - The lookup makes a list of steps: the type of the receiver, then each type that `Deref`
//!   gives, then the slice type if the last type is an array.
//! - Each step has three positions: the type itself, `&` of the type, `&mut` of the type. The
//!   slice step has no position for the type itself.
//! - At one position, an inherent item comes before an item of a trait in scope. Two traits at
//!   one position are an error, not a selection.
//! - A candidate matches a position if the type of its receiver can be equal to the type of
//!   the position, and the bounds of its `impl` hold.
//!
//! A path `Type::name` has one position: the type. An inherent item comes before a trait item.
//!
//! This module finds each candidate that matches a position before the position of the winner
//! (or the same position, for an inherent candidate against a trait winner). Such a candidate
//! does not hold now. The module then says which trait facts must change:
//!
//! - `closed:TRAIT:HEAD`: a new `impl` of the trait `TRAIT` for a type with the name `HEAD`
//!   (`*`: an unknown type).
//! - `auto`: an auto trait (`Send`) must start to hold.
//! - `unstable`: the candidate holds now, and it needs an unstable feature. The compiler puts
//!   it after each stable candidate. An unstable candidate that does not hold now is equal to
//!   a stable one: it needs a new trait fact. The entry is also the result for a winner that
//!   is unstable (`feature`): a build with no such feature can select a stable candidate.
//! - `foreign`: a crate outside the workspace (not the standard library) can add the `impl`.
//! - `open`: the module does not know the facts. Each change of a trait fact is a possible
//!   cause.
//!
//! The answer is an over-approximation: an unknown case is `open`.
//!
//! The orphan rule (`std_only`, part B4) removes a case that cannot occur. Only the standard
//! library can have an `impl` of its trait (with no parameter) for its type or for a type of
//! the language. Such a trait fact gives no `closed` entry: `usize: Iterator` holds in no
//! configuration. A generic `impl` of the standard library stays in the walk.
//!
//! The second analysis (`uses`, part C1) looks at the winner. The winner can hold because of
//! an `impl` that only the active configuration has. A different configuration removes the
//! `impl`, and a candidate with a lower rank gets the call. For a call and for a path to an
//! associated item, `uses` walks the trait fact of the winner (`Self: Trait`), or the bounds
//! of its inherent `impl`. Each `impl` of a workspace crate that can give the fact is an
//! entry `closed:TRAIT:HEAD`. An `impl` of a crate outside the workspace is an entry
//! `foreign`. The walk includes the bounds of that `impl`. The match is by
//! structure: it finds more `impl`s than the selection of the compiler uses, not fewer. A
//! bound of the method itself is not an input: without it the code does not compile.

use std::collections::BTreeSet;

use rustc_hir::def::DefKind;
use rustc_middle::ty::adjustment::{Adjust, AutoBorrow, AutoBorrowMutability, DerefAdjustKind};
use rustc_middle::ty::print::with_no_trimmed_paths;
use rustc_middle::ty::{self, GenericArg, Ty, TyCtxt, TypeVisitableExt};
use rustc_span::Symbol;
use rustc_span::def_id::DefId;

/// The trait facts that must change before a rival wins.
#[derive(Default)]
struct Need {
    open: bool,
    foreign: bool,
    auto: bool,
    unstable: bool,
    closed: BTreeSet<(String, String)>,
    /// The last walk of bounds found a trait fact that only the standard library can change.
    barred: bool,
    /// The first condition, for the message.
    why: Option<String>,
    /// The first rival, for the message.
    rival: Option<String>,
}

impl Need {
    fn is_empty(&self) -> bool {
        !self.open && !self.foreign && !self.auto && !self.unstable && self.closed.is_empty()
    }
    fn open(&mut self, why: impl FnOnce() -> String) {
        self.open = true;
        if self.why.is_none() {
            self.why = Some(why());
        }
    }
}

/// The trait facts that the meaning of a call uses now (batch 5q, part C1).
#[derive(Default)]
struct Via {
    open: bool,
    auto: bool,
    /// The selection uses an `impl` of a crate outside the workspace.
    foreign: bool,
    /// The trait and the type of each `impl` of the workspace that the selection uses.
    used: BTreeSet<(String, String)>,
    /// The first `impl` of the workspace, for the message.
    first: Option<String>,
    /// The first fact that the module cannot follow, for the message.
    why: Option<String>,
}

impl Via {
    fn open(&mut self, why: impl FnOnce() -> String) {
        self.open = true;
        if self.why.is_none() {
            self.why = Some(why());
        }
    }
}

/// The index of a type parameter of an `impl` that the match of the receiver does not bind.
const MARK: u32 = 0xFFF0_0000;

pub struct Finder<'tcx> {
    pub tcx: TyCtxt<'tcx>,
    pub typeck: &'tcx ty::TypeckResults<'tcx>,
}

impl<'tcx> Finder<'tcx> {
    fn text(&self, ty: Ty<'tcx>) -> String {
        with_no_trimmed_paths!(ty.to_string())
    }
    fn path(&self, def_id: DefId) -> String {
        with_no_trimmed_paths!(self.tcx.def_path_str(def_id))
    }
    fn outside(&self, def_id: DefId) -> bool {
        !def_id.is_local()
            && !crate::config::is_workspace_crate(self.tcx, def_id.krate)
            && !crate::reach::is_std(self.tcx.crate_name(def_id.krate).as_str())
    }
    /// True for a trait that the compiler decides from the type alone (`Sized`).
    fn sized_like(&self, trait_id: DefId) -> bool {
        use rustc_attr_ir::LangItem;
        [LangItem::Sized, LangItem::MetaSized, LangItem::PointeeSized].into_iter().any(|item| self.tcx.is_lang_item(trait_id, item))
    }
    /// True if only the standard library can have an `impl` of the trait for the type (the
    /// orphan rule). The trait is of the standard library and has no parameter: a type
    /// parameter of a trait can make an `impl` local. The type is not local for each other
    /// crate: a type of the language or of the standard library. `&T`, `&mut T`, `Box<T>`,
    /// and `Pin<T>` are local if `T` is local.
    fn std_only(&self, trait_id: DefId, self_ty: Ty<'tcx>) -> bool {
        let tcx = self.tcx;
        let std = |def_id: DefId| !def_id.is_local() && crate::reach::is_std(tcx.crate_name(def_id.krate).as_str());
        if !std(trait_id) || tcx.generics_of(trait_id).count() != 1 {
            return false;
        }
        let mut ty = self_ty;
        loop {
            ty = match ty.kind() {
                ty::Ref(_, inner, _) => *inner,
                ty::Adt(adt, args) if adt.is_fundamental() => match args.types().next() {
                    Some(inner) => inner,
                    None => return false,
                },
                ty::Adt(adt, _) => return std(adt.did()),
                ty::Bool | ty::Char | ty::Int(_) | ty::Uint(_) | ty::Float(_) | ty::Str | ty::Never | ty::Slice(_) | ty::Array(..) | ty::Tuple(_) | ty::RawPtr(..) | ty::FnPtr(..) => return true,
                ty::Dynamic(preds, ..) => return preds.principal_def_id().is_none_or(std),
                _ => return false,
            };
        }
    }

    /// The name of the type for the `impl` match of the verifier. `*`: no simple name.
    fn head(&self, ty: Ty<'tcx>) -> String {
        match ty.kind() {
            ty::Adt(adt, _) => self.tcx.item_name(adt.did()).to_string(),
            ty::Bool | ty::Char | ty::Int(_) | ty::Uint(_) | ty::Float(_) | ty::Str => self.text(ty),
            _ => "*".to_string(),
        }
    }

    /// Matches the pattern (a type with the parameters of an `impl`) with a type of the body.
    /// `Some(true)`: equal with the bindings in `map`. `Some(false)`: not equal in each
    /// configuration. `None`: not known.
    fn bind(&self, pattern: Ty<'tcx>, real: Ty<'tcx>, map: &mut Vec<Option<GenericArg<'tcx>>>) -> Option<bool> {
        self.bind_with(pattern, real, map, false)
    }

    /// `marked`: the pattern has the types of the body, and only a parameter with a mark
    /// (`MARK`) can get a binding.
    fn bind_with(&self, pattern: Ty<'tcx>, real: Ty<'tcx>, map: &mut Vec<Option<GenericArg<'tcx>>>, marked: bool) -> Option<bool> {
        if let ty::Param(param) = pattern.kind()
            && (!marked || param.index >= MARK)
        {
            let index = if marked { param.index - MARK } else { param.index };
            let slot = map.get_mut(index as usize)?;
            return match slot {
                Some(old) => Some(*old == GenericArg::from(real)),
                None => {
                    *slot = Some(real.into());
                    Some(true)
                }
            };
        }
        if matches!(real.kind(), ty::Alias(..) | ty::Infer(..) | ty::Error(..) | ty::Bound(..) | ty::Placeholder(..)) {
            return None;
        }
        if matches!(real.kind(), ty::Param(param) if param.index >= MARK) {
            return None;
        }
        match (pattern.kind(), real.kind()) {
            (ty::Alias(..) | ty::Infer(..) | ty::Error(..) | ty::Bound(..) | ty::Placeholder(..), _) => None,
            (ty::Ref(_, a, ma), ty::Ref(_, b, mb)) => if ma != mb { Some(false) } else { self.bind_with(*a, *b, map, marked) },
            (ty::RawPtr(a, ma), ty::RawPtr(b, mb)) => if ma != mb { Some(false) } else { self.bind_with(*a, *b, map, marked) },
            (ty::Slice(a), ty::Slice(b)) => self.bind_with(*a, *b, map, marked),
            (ty::Array(a, la), ty::Array(b, lb)) => {
                let first = self.bind_with(*a, *b, map, marked)?;
                Some(first && self.bind_const(*la, *lb, map)?)
            }
            (ty::Tuple(a), ty::Tuple(b)) => {
                if a.len() != b.len() {
                    return Some(false);
                }
                let mut all = true;
                for (a, b) in a.iter().zip(b.iter()) {
                    all &= self.bind_with(a, b, map, marked)?;
                }
                Some(all)
            }
            (ty::Adt(da, aa), ty::Adt(db, ab)) => {
                if da.did() != db.did() {
                    return Some(false);
                }
                let mut all = true;
                for (a, b) in aa.iter().zip(ab.iter()) {
                    match (a.kind(), b.kind()) {
                        (ty::GenericArgKind::Type(a), ty::GenericArgKind::Type(b)) => all &= self.bind_with(a, b, map, marked)?,
                        (ty::GenericArgKind::Const(a), ty::GenericArgKind::Const(b)) => all &= self.bind_const(a, b, map)?,
                        (ty::GenericArgKind::Lifetime(_), ty::GenericArgKind::Lifetime(_)) => {}
                        _ => return None,
                    }
                }
                Some(all)
            }
            (ty::Bool, ty::Bool) | (ty::Char, ty::Char) | (ty::Str, ty::Str) | (ty::Never, ty::Never) => Some(true),
            (ty::Int(a), ty::Int(b)) => Some(a == b),
            (ty::Uint(a), ty::Uint(b)) => Some(a == b),
            (ty::Float(a), ty::Float(b)) => Some(a == b),
            (a, b) => {
                if std::mem::discriminant(a) != std::mem::discriminant(b) {
                    Some(false)
                } else if pattern == real {
                    Some(true)
                } else {
                    None
                }
            }
        }
    }

    fn bind_const(&self, pattern: ty::Const<'tcx>, real: ty::Const<'tcx>, map: &mut Vec<Option<GenericArg<'tcx>>>) -> Option<bool> {
        if let ty::ConstKind::Param(param) = pattern.kind() {
            let slot = map.get_mut(param.index as usize)?;
            return match slot {
                Some(old) => (*old == GenericArg::from(real)).then_some(true),
                None => {
                    *slot = Some(real.into());
                    Some(true)
                }
            };
        }
        (pattern == real).then_some(true)
    }

    /// The bounds of the `impl` with the bindings of `map`: what must change before each of
    /// them holds. No entry: each bound holds in each configuration.
    fn bounds(&self, imp: DefId, map: &[Option<GenericArg<'tcx>>], depth: usize, need: &mut Need) {
        let tcx = self.tcx;
        let mut map = map.to_vec();
        self.solve(imp, &mut map);
        let Some(args) = self.marked_args(imp, &map) else {
            need.open(|| format!("a bound of `{}` holds", self.path(imp)));
            return;
        };
        for clause in tcx.clauses_of(imp).instantiate(tcx, args).clauses {
            let clause = clause.skip_norm_wip();
            match clause.kind().skip_binder() {
                ty::ClauseKind::Trait(pred) if self.sized_like(pred.def_id()) => {}
                ty::ClauseKind::Trait(pred) => {
                    let self_ty = pred.self_ty();
                    let marked = self.has_mark(self_ty.into());
                    if marked {
                        need.open(|| format!("a bound of `{}` holds", self.path(imp)));
                    } else {
                        self.newly(pred.def_id(), self_ty, depth + 1, need);
                    }
                }
                ty::ClauseKind::Projection(_) => need.open(|| format!("a bound of `{}` holds", self.path(imp))),
                _ => {}
            }
        }
    }

    /// What must change before `self_ty: trait_id` starts to hold.
    fn newly(&self, trait_id: DefId, self_ty: Ty<'tcx>, depth: usize, need: &mut Need) {
        let tcx = self.tcx;
        if self.sized_like(trait_id) {
            return;
        }
        let why = || format!("`{}: {}`", self.text(self_ty), self.path(trait_id));
        if tcx.trait_is_auto(trait_id) {
            need.auto = true;
            need.why.get_or_insert_with(why);
            return;
        }
        if depth > 3 {
            need.open(why);
            return;
        }
        // A new `impl` of the trait for this type. The orphan rule permits no such `impl`
        // out of the standard library for a trait and a type of the standard library.
        if self.std_only(trait_id, self_ty) {
            need.barred = true;
        } else {
            need.closed.insert((tcx.item_name(trait_id).to_string(), self.head(self_ty)));
            need.why.get_or_insert_with(why);
            if self.outside(trait_id) || matches!(self_ty.kind(), ty::Adt(adt, _) if self.outside(adt.did())) {
                need.foreign = true;
            }
        }
        if matches!(self_ty.kind(), ty::Alias(..) | ty::Infer(..) | ty::Error(..) | ty::Bound(..) | ty::Placeholder(..)) {
            need.open(why);
            return;
        }
        // An `impl` that the compiler has: its bounds can start to hold.
        let mut impls = Vec::new();
        tcx.for_each_relevant_impl(trait_id, self_ty, |imp| impls.push(imp));
        for imp in impls {
            let header = tcx.type_of(imp).instantiate_identity().skip_norm_wip();
            let mut map = vec![None; tcx.generics_of(imp).count()];
            match self.bind(header, self_ty, &mut map) {
                Some(false) => {}
                Some(true) => self.bounds(imp, &map, depth, need),
                None => need.open(why),
            }
        }
    }

    /// The trait facts that the item `item` of the node `hir_id` uses now: each `impl` that
    /// the selection of the compiler can use, with the bounds of that `impl`. An `impl` of
    /// the workspace is an entry `closed:TRAIT:HEAD`: a different configuration can remove
    /// it, and then a candidate with a lower rank gets the call.
    pub fn uses(&self, hir_id: rustc_hir::HirId, what: &str, item: DefId) -> Option<String> {
        let tcx = self.tcx;
        let parent = tcx.opt_parent(item)?;
        let args = self.typeck.node_args_opt(hir_id)?;
        let mut via = Via::default();
        if args.has_infer() || args.references_error() {
            via.open(|| "the types of the call".to_string());
        } else {
            let count = tcx.generics_of(parent).count();
            match tcx.def_kind(parent) {
                DefKind::Trait if args.len() >= count => self.holds(ty::TraitRef::new(tcx, parent, args.iter().take(count)), 0, &mut via),
                DefKind::Impl { of_trait: false } if args.len() >= count => {
                    let map: Vec<Option<GenericArg<'tcx>>> = args.iter().take(count).map(Some).collect();
                    self.holds_bounds(parent, &map, 0, &mut via);
                }
                _ => return None,
            }
        }
        if !via.open && !via.auto && !via.foreign && via.used.is_empty() {
            return None;
        }
        let mut kinds = Vec::new();
        for (flag, word) in [(via.open, "open"), (via.auto, "auto"), (via.foreign, "foreign")] {
            if flag {
                kinds.push(word.to_string());
            }
        }
        kinds.extend(via.used.iter().map(|(name, head)| format!("closed:{name}:{head}")));
        let fact = match (via.first, via.why) {
            (Some(first), _) => format!("`{first}`"),
            (None, Some(why)) => format!("a trait fact ({why})"),
            (None, None) => "a trait fact".to_string(),
        };
        let message = format!("the {what} means `{}` now because of {fact}; a different configuration can change that fact", self.path(item));
        Some(format!("{}|{}", kinds.join(" "), short(&message)))
    }

    /// The facts that make `trait_ref` hold now.
    fn holds(&self, trait_ref: ty::TraitRef<'tcx>, depth: usize, via: &mut Via) {
        let tcx = self.tcx;
        let trait_id = trait_ref.def_id;
        if self.sized_like(trait_id) {
            return;
        }
        let env = ty::TypingEnv::post_analysis(tcx, self.typeck.hir_owner.def_id);
        let trait_ref = if trait_ref.has_aliases() {
            tcx.try_normalize_erasing_regions(env, ty::Unnormalized::new(trait_ref)).unwrap_or(trait_ref)
        } else {
            trait_ref
        };
        let self_ty = trait_ref.self_ty();
        let why = || format!("`{}: {}`", self.text(self_ty), self.path(trait_id));
        if tcx.trait_is_auto(trait_id) {
            via.auto = true;
            via.why.get_or_insert_with(why);
            return;
        }
        // A bound in the declaration of an associated type of the standard library
        // (`type IntoIter: Iterator`) holds in each configuration.
        if let ty::Alias(_, alias) = self_ty.kind()
            && let ty::AliasTyKind::Projection { def_id } = alias.kind
            && crate::reach::is_std(tcx.crate_name(def_id.krate).as_str())
            && tcx.generics_of(trait_id).count() == 1
            && tcx.item_self_bounds(def_id).skip_binder().iter().any(|clause| {
                matches!(clause.kind().skip_binder(), ty::ClauseKind::Trait(pred)
                    if pred.def_id() == trait_id || ty::Interner::supertrait_def_ids(tcx, pred.def_id()).any(|above| above == trait_id))
            })
        {
            return;
        }
        if depth > 4 || matches!(self_ty.kind(), ty::Alias(..) | ty::Infer(..) | ty::Error(..) | ty::Bound(..) | ty::Placeholder(..)) {
            via.open(why);
            return;
        }
        let mut impls = Vec::new();
        tcx.for_each_relevant_impl(trait_id, self_ty, |imp| impls.push(imp));
        for imp in impls {
            let header = tcx.impl_trait_ref(imp).instantiate_identity().skip_norm_wip();
            let mut map = vec![None; tcx.generics_of(imp).count()];
            let mut equal = Some(true);
            for (pattern, real) in header.args.iter().zip(trait_ref.args.iter()) {
                let step = match (pattern.kind(), real.kind()) {
                    (ty::GenericArgKind::Type(a), ty::GenericArgKind::Type(b)) => self.bind(a, b, &mut map),
                    (ty::GenericArgKind::Const(a), ty::GenericArgKind::Const(b)) => self.bind_const(a, b, &mut map),
                    (ty::GenericArgKind::Lifetime(_), ty::GenericArgKind::Lifetime(_)) => Some(true),
                    _ => None,
                };
                match step {
                    Some(true) => {}
                    Some(false) => {
                        equal = Some(false);
                        break;
                    }
                    None => equal = None,
                }
            }
            match equal {
                Some(false) => {}
                None => via.open(why),
                Some(true) => {
                    if imp.is_local() || crate::config::is_workspace_crate(tcx, imp.krate) {
                        via.used.insert((tcx.item_name(trait_id).to_string(), self.head(self_ty)));
                        via.first.get_or_insert_with(|| format!("impl {} for {}", self.path(trait_id), self.text(self_ty)));
                    } else if self.outside(imp) {
                        // A feature of that crate can remove the `impl`. Semilla does not
                        // read that crate.
                        via.foreign = true;
                        via.first.get_or_insert_with(|| format!("impl {} for {}", self.path(trait_id), self.text(self_ty)));
                    }
                    self.holds_bounds(imp, &map, depth, via);
                }
            }
        }
    }

    /// The arguments of the `impl` for the bindings of `map`. A parameter with no binding
    /// gets a mark. `None`: a constant parameter has no binding.
    fn marked_args(&self, imp: DefId, map: &[Option<GenericArg<'tcx>>]) -> Option<ty::GenericArgsRef<'tcx>> {
        let tcx = self.tcx;
        let mut unknown = false;
        let args = ty::GenericArgs::for_item(tcx, imp, |param, _| match map.get(param.index as usize).copied().flatten() {
            Some(arg) => arg,
            None => match param.kind {
                ty::GenericParamDefKind::Lifetime => tcx.lifetimes.re_erased.into(),
                ty::GenericParamDefKind::Type { .. } => Ty::new_param(tcx, MARK + param.index, param.name).into(),
                ty::GenericParamDefKind::Const { .. } => {
                    unknown = true;
                    tcx.mk_param_from_def(param)
                }
            },
        });
        (!unknown).then_some(args)
    }

    fn has_mark(&self, arg: GenericArg<'tcx>) -> bool {
        arg.walk().any(|part| matches!(part.as_type().map(|ty| ty.kind()), Some(ty::Param(param)) if param.index >= MARK))
    }

    /// Gives a binding to each parameter of the `impl` that a bound of the form
    /// `<I as Iterator>::Item = &T` decides (`Copied<I>`, `Map<I, F>`).
    fn solve(&self, imp: DefId, map: &mut Vec<Option<GenericArg<'tcx>>>) {
        let tcx = self.tcx;
        let env = ty::TypingEnv::post_analysis(tcx, self.typeck.hir_owner.def_id);
        for _ in 0..3 {
            if map.iter().all(|slot| slot.is_some()) {
                return;
            }
            let Some(args) = self.marked_args(imp, map) else { return };
            let mut progress = false;
            for clause in tcx.clauses_of(imp).instantiate(tcx, args).clauses {
                let clause = clause.skip_norm_wip();
                let ty::ClauseKind::Projection(pred) = clause.kind().skip_binder() else { continue };
                let Some(pattern) = pred.term.as_type() else { continue };
                if !self.has_mark(pattern.into()) || pred.projection_term.args.iter().any(|arg| self.has_mark(arg)) {
                    continue;
                }
                let ty::AliasTermKind::ProjectionTy { def_id } = pred.projection_term.kind else { continue };
                let alias = Ty::new_projection(tcx, ty::IsRigid::No, def_id, pred.projection_term.args);
                let Ok(real) = tcx.try_normalize_erasing_regions(env, ty::Unnormalized::new(alias)) else { continue };
                if real.has_aliases() || real.references_error() {
                    continue;
                }
                let mut next = map.clone();
                if self.bind_with(pattern, real, &mut next, true) == Some(true) {
                    *map = next;
                    progress = true;
                }
            }
            if !progress {
                return;
            }
        }
    }

    /// The facts that make the bounds of the `impl` hold now, with the bindings of `map`.
    fn holds_bounds(&self, imp: DefId, map: &[Option<GenericArg<'tcx>>], depth: usize, via: &mut Via) {
        let tcx = self.tcx;
        let mut map = map.to_vec();
        self.solve(imp, &mut map);
        let Some(args) = self.marked_args(imp, &map) else {
            via.open(|| format!("a bound of `{}`", self.path(imp)));
            return;
        };
        for clause in tcx.clauses_of(imp).instantiate(tcx, args).clauses {
            let clause = clause.skip_norm_wip();
            if let ty::ClauseKind::Trait(pred) = clause.kind().skip_binder()
                && !self.sized_like(pred.def_id())
            {
                let marked = pred.trait_ref.args.iter().any(|arg| self.has_mark(arg));
                if marked {
                    via.open(|| format!("a bound of `{}`", self.path(imp)));
                } else {
                    self.holds(pred.trait_ref, depth + 1, via);
                }
            }
        }
    }

    /// The `impl` blocks with inherent items for a type.
    fn inherent(&self, ty: Ty<'tcx>, out: &mut Vec<DefId>) {
        use rustc_middle::ty::fast_reject::{TreatParams, simplify_type};
        let tcx = self.tcx;
        let owner = match ty.kind() {
            ty::Adt(adt, _) => Some(adt.did()),
            ty::Foreign(def_id) => Some(*def_id),
            ty::Dynamic(data, ..) => data.principal_def_id(),
            _ => None,
        };
        if let Some(owner) = owner {
            out.extend(tcx.inherent_impls(owner).iter().copied());
        }
        if let Some(simple) = simplify_type(tcx, ty, TreatParams::InstantiateWithInfer) {
            out.extend(tcx.incoherent_impls(simple).iter().copied());
        }
    }

    fn unstable(&self, def_id: DefId) -> bool {
        self.tcx.lookup_stability(def_id).is_some_and(|stability| stability.is_unstable() && !self.tcx.features().enabled(stability.feature))
    }

    /// False only if the compiler says that `self_ty: Trait` does not hold now.
    fn may_hold(&self, trait_id: DefId, self_ty: Ty<'tcx>) -> bool {
        use rustc_infer::infer::TyCtxtInferExt;
        use rustc_trait_selection::infer::InferCtxtExt;
        let tcx = self.tcx;
        if tcx.generics_of(trait_id).count() != 1 || self_ty.has_infer() || self_ty.references_error() || self_ty.has_escaping_bound_vars() {
            return true;
        }
        let env = ty::TypingEnv::post_analysis(tcx, self.typeck.hir_owner.def_id);
        let (infcx, param_env) = tcx.infer_ctxt().build_with_typing_env(env);
        infcx.type_implements_trait(trait_id, [self_ty], param_env).may_apply()
    }

    /// An unstable winner (batch 5q). The crate has the feature now. A build with no such
    /// feature puts the winner after each stable candidate, so a stable candidate with a
    /// lower rank can get the call. A `cfg_attr` at the crate root can select the feature.
    pub fn feature(&self, what: &str, winner: DefId) -> Option<String> {
        let tcx = self.tcx;
        // An unstable item in the code of a macro of the standard library needs no feature of
        // the crate: each configuration permits it.
        let gate = |def_id: DefId| {
            tcx.lookup_stability(def_id).filter(|stability| stability.is_unstable() && tcx.features().enabled(stability.feature)).map(|stability| stability.feature)
        };
        let feature = gate(winner).or_else(|| tcx.opt_parent(winner).filter(|parent| matches!(tcx.def_kind(*parent), DefKind::Trait)).and_then(gate))?;
        let message = format!("the {what} means `{}` now by the feature `{feature}`; a configuration with no such feature can select a different item", self.path(winner));
        Some(format!("unstable|{}", short(&message)))
    }

    fn visible(&self, item: DefId, from: rustc_hir::HirId) -> bool {
        self.tcx.visibility(item).is_accessible_from(self.tcx.parent_module(from).to_def_id(), self.tcx)
    }

    /// The next step of the list of steps.
    fn deref(&self, ty: Ty<'tcx>) -> Option<Ty<'tcx>> {
        let tcx = self.tcx;
        if let Some(inner) = ty.builtin_deref(true) {
            return Some(inner);
        }
        if !matches!(ty.kind(), ty::Adt(..) | ty::Param(_) | ty::Alias(..)) {
            return None;
        }
        let target = tcx.lang_items().deref_target()?;
        let env = ty::TypingEnv::post_analysis(tcx, self.typeck.hir_owner.def_id);
        let projection = Ty::new_projection(tcx, ty::IsRigid::No, target, [ty]);
        let next = tcx.try_normalize_erasing_regions(env, ty::Unnormalized::new(projection)).ok()?;
        // No `impl` of `Deref`: the projection stays.
        (next != projection && !next.references_error()).then_some(next)
    }

    fn finish(&self, what: &str, now: Option<DefId>, need: Need) -> Option<String> {
        if need.is_empty() {
            return None;
        }
        let mut kinds = Vec::new();
        for (flag, word) in [(need.open, "open"), (need.foreign, "foreign"), (need.auto, "auto"), (need.unstable, "unstable")] {
            if flag {
                kinds.push(word.to_string());
            }
        }
        kinds.extend(need.closed.iter().map(|(name, head)| format!("closed:{name}:{head}")));
        let now = now.map(|def_id| self.path(def_id)).unwrap_or_else(|| "an unknown item".to_string());
        let rival = need.rival.unwrap_or_else(|| "a different item".to_string());
        let why = match need.why {
            Some(why) => format!("{why} holds"),
            None if need.unstable => "the crate gets its unstable feature".to_string(),
            None => "a trait fact changes".to_string(),
        };
        let message = format!("the {what} means `{now}` now; `{rival}` wins if {why} in a different configuration");
        Some(format!("{}|{}", kinds.join(" "), short(&message)))
    }

    /// The rival of the method call `expr` with the receiver `receiver`.
    pub fn method(&self, expr: &rustc_hir::Expr<'tcx>, name: Symbol, receiver: &rustc_hir::Expr<'tcx>) -> Option<String> {
        let tcx = self.tcx;
        let mut need = Need::default();
        let Some(winner) = self.typeck.type_dependent_def_id(expr.hir_id) else {
            need.open(|| "the call gets a meaning".to_string());
            return self.finish("call", None, need);
        };
        let unknown = |mut need: Need, why: &str| {
            need.open(|| why.to_string());
            self.finish("call", Some(winner), need)
        };
        let features = tcx.features();
        if features.arbitrary_self_types() || features.arbitrary_self_types_pointers() {
            return unknown(need, "a receiver of an unstable form matches");
        }
        let winner_inherent = tcx.opt_parent(winner).is_some_and(|parent| matches!(tcx.def_kind(parent), DefKind::Impl { of_trait: false }));
        // The steps to the winner, from the adjustments of the receiver.
        let Some(start) = self.typeck.expr_ty_opt(receiver) else { return unknown(need, "the receiver has a type") };
        let mut steps = vec![start];
        let (mut borrow, mut unsize, mut state) = (None, false, 0);
        for adjustment in self.typeck.expr_adjustments(receiver) {
            match adjustment.kind {
                Adjust::Deref(DerefAdjustKind::Builtin | DerefAdjustKind::Overloaded(_)) if state == 0 => steps.push(adjustment.target),
                Adjust::Borrow(AutoBorrow::Ref(mutability)) if state == 0 => {
                    borrow = Some(matches!(mutability, AutoBorrowMutability::Mut { .. }));
                    state = 1;
                }
                Adjust::Pointer(ty::adjustment::PointerCoercion::Unsize) if state == 1 => {
                    unsize = true;
                    state = 2;
                }
                _ => return unknown(need, "the receiver has an adjustment of a different kind"),
            }
        }
        if steps.iter().any(|ty| ty.has_infer() || ty.references_error()) {
            return unknown(need, "the receiver has a known type");
        }
        let derefs = steps.len() - 1;
        // The other steps.
        while let Some(next) = self.deref(*steps.last().unwrap()) {
            if steps.len() > 12 {
                return unknown(need, "the receiver has a short list of steps");
            }
            if unsize {
                return unknown(need, "the receiver has an array type");
            }
            steps.push(next);
        }
        let array = match steps.last().unwrap().kind() {
            ty::Array(item, _) => Some(Ty::new_slice(tcx, *item)),
            _ => None,
        };
        if unsize && array.is_none() {
            return unknown(need, "the receiver has an array type");
        }
        // The positions. Each has an index: three for each step.
        let erased = tcx.lifetimes.re_erased;
        let mut positions: Vec<Option<Ty<'tcx>>> = Vec::new();
        for step in steps.iter().copied() {
            positions.extend([Some(step), Some(Ty::new_imm_ref(tcx, erased, step)), Some(Ty::new_mut_ref(tcx, erased, step))]);
        }
        if let Some(slice) = array {
            positions.extend([None, Some(Ty::new_imm_ref(tcx, erased, slice)), Some(Ty::new_mut_ref(tcx, erased, slice))]);
        }
        // The position of the winner. A selection at a step with a `&` type has the
        // adjustments "deref, then borrow": the position is the step before, with no borrow.
        let winner_at = match (borrow, unsize) {
            (Some(mutable), true) => 3 * steps.len() + 1 + mutable as usize,
            (Some(mutable), false) => match derefs.checked_sub(1).map(|before| steps[before].kind()) {
                Some(ty::Ref(_, _, old)) if old.is_mut() == mutable => 3 * (derefs - 1),
                _ => 3 * derefs + 1 + mutable as usize,
            },
            (None, _) => 3 * derefs,
        };
        if winner_at >= positions.len() {
            return unknown(need, "the winner has a known position");
        }
        if winner_inherent && winner_at == 0 {
            return None;
        }
        // The inherent candidates.
        let mut impls = Vec::new();
        for step in steps.iter().copied().chain(array) {
            self.inherent(step, &mut impls);
        }
        let mut seen = std::collections::HashSet::new();
        impls.retain(|imp| seen.insert(*imp));
        let last = if winner_inherent { winner_at.saturating_sub(1) } else { winner_at };
        for imp in impls {
            for item in tcx.associated_items(imp).filter_by_name_unhygienic(name) {
                if !item.is_method() || item.def_id == winner || !self.visible(item.def_id, expr.hir_id) {
                    continue;
                }
                let form = tcx.fn_sig(item.def_id).instantiate_identity().skip_norm_wip().skip_binder().inputs()[0];
                for position in positions[..=last].iter().copied().flatten() {
                    let mut map = vec![None; tcx.generics_of(item.def_id).count()];
                    let before = need.is_empty();
                    match self.bind(form, position, &mut map) {
                        Some(false) => continue,
                        Some(true) if self.unstable(item.def_id) => need.unstable = true,
                        Some(true) => {
                            need.barred = false;
                            self.bounds(imp, &map, 0, &mut need);
                            // The candidate holds now, and the compiler does not select it.
                            // A bound that only the standard library can change is not such
                            // a case: the bound is equal in each configuration.
                            if before && need.is_empty() && !need.barred {
                                need.open(|| format!("the compiler selects `{}`", self.path(item.def_id)));
                            }
                        }
                        None => need.open(|| format!("the receiver of `{}` matches", self.path(item.def_id))),
                    }
                    need.rival.get_or_insert_with(|| self.path(item.def_id));
                }
            }
        }
        // The candidates of the traits in scope. At the position of the winner, a second trait
        // is an error (or the winner stays).
        let mut traits: Vec<DefId> = tcx.in_scope_traits(expr.hir_id).unwrap_or_default().iter().map(|candidate| candidate.def_id).collect();
        let mut seen = std::collections::HashSet::new();
        traits.retain(|trait_id| seen.insert(*trait_id));
        let last = if features.supertrait_item_shadowing() { winner_at + 1 } else { winner_at };
        for trait_id in traits {
            for item in tcx.associated_items(trait_id).filter_by_name_unhygienic(name) {
                if !item.is_method() {
                    continue;
                }
                let form = tcx.fn_sig(item.def_id).instantiate_identity().skip_norm_wip().skip_binder().inputs()[0];
                for (index, position) in positions[..last.min(positions.len())].iter().copied().enumerate() {
                    let Some(position) = position else { continue };
                    if index == winner_at && item.def_id == winner {
                        continue;
                    }
                    let mut map = vec![None; tcx.generics_of(item.def_id).count()];
                    match (self.bind(form, position, &mut map), map[0].and_then(|arg| arg.as_type())) {
                        (Some(false), _) => continue,
                        // The compiler puts an unstable candidate that holds after each stable
                        // one. A build with the feature gives it its rank. An unstable candidate
                        // that does not hold needs a new trait fact first, as a stable one does.
                        (Some(true), Some(self_ty)) if (self.unstable(item.def_id) || self.unstable(trait_id)) && self.may_hold(trait_id, self_ty) => {
                            need.unstable = true;
                        }
                        (Some(true), Some(self_ty)) => self.newly(trait_id, self_ty, 0, &mut need),
                        _ => need.open(|| format!("the receiver of `{}` matches", self.path(item.def_id))),
                    }
                    need.rival.get_or_insert_with(|| self.path(item.def_id));
                }
            }
        }
        self.finish("call", Some(winner), need)
    }

    /// The rival of the path `Type::name` with the meaning `winner` (a function or a constant).
    pub fn path_item(&self, hir_id: rustc_hir::HirId, name: Symbol, winner: DefId) -> Option<String> {
        let tcx = self.tcx;
        let Some(parent) = tcx.opt_parent(winner) else { return None };
        // An inherent item has the highest rank. An item of a different kind is not a result
        // of this lookup.
        if !matches!(tcx.def_kind(parent), DefKind::Trait) {
            return None;
        }
        let mut need = Need::default();
        let self_ty = self.typeck.node_args_opt(hir_id).and_then(|args| args.types().next());
        let Some(self_ty) = self_ty.filter(|ty| !ty.has_infer() && !ty.references_error()) else {
            need.open(|| "the type of the path changes".to_string());
            return self.finish("path", Some(winner), need);
        };
        let mut impls = Vec::new();
        self.inherent(self_ty, &mut impls);
        for imp in impls {
            for item in tcx.associated_items(imp).filter_by_name_unhygienic(name) {
                if !matches!(tcx.def_kind(item.def_id), DefKind::AssocFn | DefKind::AssocConst) || !self.visible(item.def_id, hir_id) {
                    continue;
                }
                let header = tcx.type_of(imp).instantiate_identity().skip_norm_wip();
                let mut map = vec![None; tcx.generics_of(imp).count()];
                let before = need.is_empty();
                match self.bind(header, self_ty, &mut map) {
                    Some(false) => continue,
                    Some(true) if self.unstable(item.def_id) => need.unstable = true,
                    Some(true) => {
                        need.barred = false;
                        self.bounds(imp, &map, 0, &mut need);
                        if before && need.is_empty() && !need.barred {
                            need.open(|| format!("the compiler selects `{}`", self.path(item.def_id)));
                        }
                    }
                    None => need.open(|| format!("the type of `{}` matches", self.path(item.def_id))),
                }
                need.rival.get_or_insert_with(|| self.path(item.def_id));
            }
        }
        self.finish("path", Some(winner), need)
    }
}

/// The message without the module paths: `a::b::Tr::name` becomes `Tr::name`.
/// The name of a primitive type stays (`str::len`).
fn short(text: &str) -> String {
    const KEEP: [&str; 19] = [
        "str", "bool", "char", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64", "crate",
        "dyn",
    ];
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if !(c.is_alphabetic() || c == '_') {
            out.push(c);
            at += 1;
            continue;
        }
        let mut end = at;
        while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        let word: String = chars[at..end].iter().collect();
        let module = chars.get(end) == Some(&':')
            && chars.get(end + 1) == Some(&':')
            && chars.get(end + 2).is_some_and(|next| next.is_alphabetic() || *next == '_')
            && word.chars().next().is_some_and(|first| first.is_lowercase() || first == '_')
            && !KEEP.contains(&word.as_str());
        if module {
            at = end + 2;
        } else {
            out.push_str(&word);
            at = end;
        }
    }
    out
}
