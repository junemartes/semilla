//! The code that a body can run with no direct call (batch 5f, A1).
//!
//! A property claim (`no-io`, `no-panic`, …) must examine each body that can run. The call list
//! of a body does not show these routes:
//!
//! - **A trait impl.** `Label(1).to_string()` is a call to `alloc`. The code of `alloc` calls the
//!   local `Display::fmt` of `Label`. The call names `Label` in its generic arguments. So each
//!   type in the generic arguments of a call gives its trait impls. An unsize cast to a trait
//!   object gives the trait impls of its source type.
//! - **Drop glue.** A `Drop` terminator, or a value that goes to generic code, runs
//!   `Drop::drop` of the type and of its fields.
//! - **A function as a value.** `map(std::fs::remove_file)` has no call of that function.
//! - **A foreign function, inline assembly.** No body to examine.
//!
//! The driver gives the facts. The verifier decides what they mean.

use std::collections::{BTreeSet, HashSet};

use rustc_middle::ty::{self, Ty, TyCtxt};
use rustc_public::mir::visit::{Location, MirVisitor};
use rustc_public::mir::{Body, CastKind, PointerCoercion, Rvalue, StatementKind, TerminatorKind};
use rustc_public::ty::MirConst;
use rustc_public::rustc_internal;
use rustc_span::def_id::DefId;

/// The facts of one body.
#[derive(Default)]
pub struct Reach {
    /// The types and the traits whose trait impls can run: the def path and the crate. A type of
    /// the standard library is in the list only for a call to another crate.
    pub types: BTreeSet<(String, String)>,
    /// The types in the parameters of the body (see `note_inputs`).
    pub params: BTreeSet<(String, String)>,
    /// The types in the constants and the statics that the body reads.
    pub consts: BTreeSet<(String, String)>,
    /// The types with a destructor that the body drops or gives to generic code: the def path
    /// and the crate. No type of `core` or `alloc`.
    pub drops: BTreeSet<(String, String)>,
    /// For a type of `drops` that the body does not drop itself (batch 5k, D9): the first call
    /// that has the type in its generic arguments. The def path of the type, and the printed
    /// path of the function.
    pub drop_calls: std::collections::BTreeMap<String, String>,
    /// The functions of other crates that the body uses as a value: the def id.
    pub extern_values: Vec<DefId>,
    /// The local functions, closures, and coroutines in the types of the body.
    pub callees: BTreeSet<String>,
    /// The code that the driver cannot examine.
    pub gaps: Vec<String>,
}

/// The path of an item of a different crate, with the name of that crate first (batch 5y, C4).
/// For a crate that the code gets with an `extern crate` item (`#![no_std]` with `extern crate
/// std;`), the compiler prints the path of the item first: `data_encoding::std::fs::metadata`.
/// This function puts the name of the crate in the place of the path of the item.
pub fn extern_crate_path(tcx: TyCtxt<'_>, path: String) -> String {
    use rustc_crate_store::ExternCrateSource;
    for krate in tcx.crates(()) {
        let Some(ExternCrateSource::Extern(item)) = tcx.extern_crate(*krate).map(|found| found.src) else { continue };
        if !item.is_local() {
            continue;
        }
        let prefix = ty::print::with_no_trimmed_paths!(tcx.def_path_str(item));
        if let Some(rest) = path.strip_prefix(&prefix).filter(|rest| rest.starts_with("::")) {
            return format!("{}{rest}", tcx.crate_name(*krate));
        }
    }
    path
}

/// The def path of an item in the crate that defines it (no re-export path), with the crate
/// name first.
pub fn real_path(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    ty::print::with_no_visible_paths!(ty::print::with_resolve_crate_name!(ty::print::with_no_trimmed_paths!(tcx.def_path_str(def_id))))
}

/// The name of the crate that defines an item.
pub fn crate_name(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    tcx.crate_name(def_id.krate).to_string()
}

/// True for a crate of the standard library.
pub fn is_std(name: &str) -> bool {
    matches!(name, "core" | "alloc" | "std" | "proc_macro" | "test")
}

/// Where a type comes from.
#[derive(Clone, Copy, PartialEq)]
enum Site {
    /// A generic argument of a call to a function of this crate.
    LocalCall,
    /// A generic argument of a call to a function of another crate.
    ExternCall,
    /// The source or the target of an unsize cast.
    Cast,
    /// The type of a local or of a constant.
    Value,
}

/// Records the types in `ty` (through the generic arguments, not through the fields).
fn note_types<'tcx>(tcx: TyCtxt<'tcx>, ty: Ty<'tcx>, site: Site, reach: &mut Reach) {
    let gap = |reach: &mut Reach, text: &str| {
        if site == Site::ExternCall && !reach.gaps.iter().any(|g| g == text) {
            reach.gaps.push(text.to_string());
        }
    };
    let mut walker = ty.walk();
    while let Some(arg) = walker.next() {
        let Some(inner) = arg.as_type() else { continue };
        match inner.kind() {
            ty::Adt(def, _) if site != Site::Value => {
                let krate = crate_name(tcx, def.did());
                // The code of another crate can run the code that a value of a library type
                // holds (`core::fmt::Arguments`, `std::io::Error`). The verifier decides.
                if !is_std(&krate) || site == Site::ExternCall {
                    reach.types.insert((real_path(tcx, def.did()), krate));
                }
            }
            ty::Dynamic(predicates, ..) if site != Site::Value => {
                if let Some(principal) = predicates.principal_def_id() {
                    let krate = crate_name(tcx, principal);
                    if !is_std(&krate) {
                        reach.types.insert((real_path(tcx, principal), krate));
                    }
                }
                gap(reach, "a call to another crate with a trait object in its generic arguments");
            }
            ty::FnPtr(..) => gap(reach, "a call to another crate with a function pointer in its generic arguments"),
            ty::Param(_) | ty::Alias(..) | ty::Placeholder(_) | ty::Bound(..) | ty::Infer(_) => {
                gap(reach, "a call to another crate with a type parameter in its generic arguments")
            }
            ty::FnDef(def_id, _) => {
                if def_id.is_local() {
                    if !tcx.is_foreign_item(*def_id) {
                        reach.callees.insert(tcx.def_path_str(*def_id));
                    }
                } else if !reach.extern_values.contains(def_id) {
                    reach.extern_values.push(*def_id);
                }
            }
            ty::Closure(def_id, args) | ty::Coroutine(def_id, args) | ty::CoroutineClosure(def_id, args) => {
                if def_id.is_local() {
                    reach.callees.insert(tcx.def_path_str(*def_id));
                }
                // The arguments of a closure type have its signature as a function pointer
                // type. That is not a value. The values are the captures. The other arguments
                // are the arguments of the function that has the closure.
                walker.skip_current_subtree();
                let (parent, upvars) = match inner.kind() {
                    ty::Closure(..) => (args.as_closure().parent_args(), args.as_closure().upvar_tys()),
                    ty::Coroutine(..) => (args.as_coroutine().parent_args(), args.as_coroutine().upvar_tys()),
                    _ => (args.as_coroutine_closure().parent_args(), args.as_coroutine_closure().upvar_tys()),
                };
                for part in parent.iter().filter_map(|arg| arg.as_type()).chain(upvars) {
                    note_types(tcx, part, site, reach);
                }
            }
            _ => {}
        }
    }
}

/// Records the destructors that a drop of a value of the type `ty` runs.
fn note_drops<'tcx>(tcx: TyCtxt<'tcx>, typing: ty::TypingEnv<'tcx>, ty: Ty<'tcx>, reach: &mut Reach, seen: &mut HashSet<Ty<'tcx>>) {
    if !seen.insert(ty) || seen.len() > 4096 {
        return;
    }
    let gap = |reach: &mut Reach, text: &str| {
        if !reach.gaps.iter().any(|g| g == text) {
            reach.gaps.push(text.to_string());
        }
    };
    match ty.kind() {
        ty::Adt(def, args) => {
            if def.is_manually_drop() || def.is_phantom_data() {
                return;
            }
            let krate = crate_name(tcx, def.did());
            // A container drops its contents: `Vec<T>` drops each `T` in its own `Drop` impl.
            for inner in args.types() {
                note_drops(tcx, typing, inner, reach, seen);
            }
            if matches!(krate.as_str(), "core" | "alloc") {
                return;
            }
            if is_std(&krate) {
                // The verifier decides with its list of the `std` modules. The fields of a
                // `std` type are private: the type itself is the fact.
                let needs_drop = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ty.needs_drop(tcx, typing))).unwrap_or(true);
                if needs_drop {
                    reach.drops.insert((real_path(tcx, def.did()), krate));
                }
                return;
            }
            if tcx.adt_destructor(def.did()).is_some() {
                reach.drops.insert((real_path(tcx, def.did()), krate));
            }
            for field in def.all_fields() {
                note_drops(tcx, typing, field.ty(tcx, args).skip_norm_wip(), reach, seen);
            }
        }
        ty::Tuple(items) => {
            for item in items.iter() {
                note_drops(tcx, typing, item, reach, seen);
            }
        }
        ty::Array(inner, _) | ty::Slice(inner) | ty::Pat(inner, _) => note_drops(tcx, typing, *inner, reach, seen),
        ty::Closure(_, args) => {
            for upvar in args.as_closure().upvar_tys() {
                note_drops(tcx, typing, upvar, reach, seen);
            }
        }
        ty::CoroutineClosure(_, args) => {
            for upvar in args.as_coroutine_closure().upvar_tys() {
                note_drops(tcx, typing, upvar, reach, seen);
            }
        }
        // The body of the coroutine has the drops of its own locals. The value has the captures.
        ty::Coroutine(_, args) => {
            for upvar in args.as_coroutine().upvar_tys() {
                note_drops(tcx, typing, upvar, reach, seen);
            }
        }
        ty::Dynamic(..) => gap(reach, "a drop of a trait object"),
        ty::Param(_) | ty::Alias(..) | ty::Placeholder(_) | ty::Bound(..) | ty::Infer(_) => gap(reach, "a drop of a value of a type parameter"),
        _ => {}
    }
}

/// Records the types of a value that comes into the body from the outside: a parameter, a
/// constant, a static. The walk goes through the generic arguments, and through the fields of
/// each type that is not of the standard library. The markers `dyn` and `fn` are for a trait
/// object and a function pointer: the value holds code that the body does not name.
fn note_inputs<'tcx>(tcx: TyCtxt<'tcx>, ty: Ty<'tcx>, out: &mut BTreeSet<(String, String)>, seen: &mut HashSet<Ty<'tcx>>) {
    if !seen.insert(ty) || seen.len() > 4096 {
        return;
    }
    match ty.kind() {
        ty::Adt(def, args) => {
            let krate = crate_name(tcx, def.did());
            out.insert((real_path(tcx, def.did()), krate.clone()));
            if crate::config::cfg_selects(tcx, def.did()) {
                out.insert((format!("cfg {}", real_path(tcx, def.did())), krate.clone()));
            }
            for inner in args.types() {
                note_inputs(tcx, inner, out, seen);
            }
            if !is_std(&krate) {
                for field in def.all_fields() {
                    note_inputs(tcx, field.ty(tcx, args).skip_norm_wip(), out, seen);
                }
            }
        }
        ty::Ref(_, inner, _) | ty::RawPtr(inner, _) | ty::Array(inner, _) | ty::Slice(inner) | ty::Pat(inner, _) => note_inputs(tcx, *inner, out, seen),
        ty::Tuple(items) => {
            for item in items.iter() {
                note_inputs(tcx, item, out, seen);
            }
        }
        ty::Dynamic(..) => {
            out.insert(("dyn".to_string(), String::new()));
        }
        ty::FnPtr(..) => {
            out.insert(("fn".to_string(), String::new()));
        }
        _ => {}
    }
}

/// Records the types whose layout and drop code are a part of a value of the type `ty`
/// (batch 5o). The walk goes through the generic arguments, and through the fields of each
/// type that is not of the standard library. `field`: the type is a part of a field. A
/// pointer in a field (a reference, a raw pointer, `NonNull`) stops the walk: the owner has
/// no part and no drop code of the pointee. The code that reads the pointee has an
/// expression or a local with the type of the pointee, and that type has its own entry
/// (`config::uses`, and the locals of the MIR).
fn note_layout<'tcx>(tcx: TyCtxt<'tcx>, ty: Ty<'tcx>, field: bool, out: &mut BTreeSet<(String, String)>, seen: &mut HashSet<(Ty<'tcx>, bool)>) {
    if !seen.insert((ty, field)) || seen.len() > 8192 {
        return;
    }
    match ty.kind() {
        ty::Adt(def, args) => {
            let krate = crate_name(tcx, def.did());
            let path = real_path(tcx, def.did());
            let pointer = field && path == "core::ptr::non_null::NonNull";
            out.insert((path.clone(), krate.clone()));
            if crate::config::cfg_selects(tcx, def.did()) {
                out.insert((format!("cfg {path}"), krate.clone()));
            }
            if pointer {
                return;
            }
            for inner in args.types() {
                note_layout(tcx, inner, field, out, seen);
            }
            if !is_std(&krate) {
                for part in def.all_fields() {
                    note_layout(tcx, part.ty(tcx, args).skip_norm_wip(), true, out, seen);
                }
            }
        }
        ty::Ref(_, inner, _) | ty::RawPtr(inner, _) => {
            if !field {
                note_layout(tcx, *inner, field, out, seen);
            }
        }
        ty::Array(inner, _) | ty::Slice(inner) | ty::Pat(inner, _) => note_layout(tcx, *inner, field, out, seen),
        ty::Tuple(items) => {
            for item in items.iter() {
                note_layout(tcx, item, field, out, seen);
            }
        }
        ty::Dynamic(..) => {
            out.insert(("dyn".to_string(), String::new()));
        }
        ty::FnPtr(..) => {
            out.insert(("fn".to_string(), String::new()));
        }
        _ => {}
    }
}

/// The callees that keep a value of a generic argument whole (batch 5o): the methods of
/// `Option` and `Result`, and the functions of the `?` operator. Such a call stays a call
/// node in each normal form (the inliner of rustc is off). The code of the callee can move
/// and drop a value of the type. It cannot read a part of it, and it cannot ask for its
/// layout. So the generic arguments of such a call are not a `touch` use.
const WHOLE_VALUE: [&str; 8] = [
    "core::result::Result::<T, E>::",
    "core::option::Option::<T>::",
    "core::ops::Try::branch",
    "core::ops::FromResidual::from_residual",
    "core::ops::try_trait::Try::branch",
    "core::ops::try_trait::FromResidual::from_residual",
    "core::convert::From::from",
    "core::convert::Into::into",
];

/// The types with a definition outside the standard library that the body uses (batch 5l): in
/// the type of a local, of a constant, and in the generic arguments of a callee. The walk
/// goes through the fields (`note_layout`). The layout of such a type is an input of a proof.
/// The standard library is free by this rule (an allow list of crates, see `is_std`): one
/// toolchain and one target give one definition of each of its types.
///
/// Batch 5o: an entry `touch PATH` for a type in the generic arguments of a callee that is
/// not in `WHOLE_VALUE` (the callee can be an intrinsic that reads the layout). An entry
/// `cfg PATH` for a type of the local crate that a `#[cfg]` attribute selects.
pub fn adts(body: &Body) -> Vec<String> {
    ty::tls::with(|tcx| {
        struct Consts<'a, 'tcx> {
            tcx: TyCtxt<'tcx>,
            out: &'a mut BTreeSet<(String, String)>,
            touch: &'a mut BTreeSet<(String, String)>,
            seen: &'a mut HashSet<(Ty<'tcx>, bool)>,
            seen_touch: &'a mut HashSet<(Ty<'tcx>, bool)>,
        }
        impl MirVisitor for Consts<'_, '_> {
            fn visit_mir_const(&mut self, constant: &MirConst, _location: Location) {
                let ty = rustc_internal::internal(self.tcx, constant.ty());
                match ty.kind() {
                    ty::FnDef(def_id, args) => {
                        let path = real_path(self.tcx, *def_id);
                        // A method of a trait is not an intrinsic, and no normal form looks into
                        // a callee with generic arguments.
                        let whole = self.tcx.trait_of_assoc(*def_id).is_some() || WHOLE_VALUE.iter().any(|entry| if entry.ends_with("::") { path.starts_with(entry) } else { path == *entry });
                        for inner in args.skip_binder().types() {
                            if whole {
                                note_layout(self.tcx, inner, false, self.out, self.seen);
                            } else {
                                note_layout(self.tcx, inner, false, self.touch, self.seen_touch);
                            }
                        }
                    }
                    _ => note_layout(self.tcx, ty, false, self.out, self.seen),
                }
            }
        }
        let mut out = BTreeSet::new();
        let mut touch = BTreeSet::new();
        let mut seen = HashSet::new();
        let mut seen_touch = HashSet::new();
        for local in body.locals() {
            note_layout(tcx, rustc_internal::internal(tcx, local.ty), false, &mut out, &mut seen);
        }
        Consts { tcx, out: &mut out, touch: &mut touch, seen: &mut seen, seen_touch: &mut seen_touch }.visit_body(body);
        // A crate outside the workspace (batch 5l, P10): the verifier cannot read its source, so
        // it cannot see a variant, a field, or a `Drop` impl under `cfg`. The entry `foreign PATH`
        // makes the type opaque. Two crates with one name: the type is foreign if one of them is.
        let foreign: HashSet<String> =
            tcx.crates(()).iter().filter(|krate| !crate::config::is_workspace_crate(tcx, **krate)).map(|krate| tcx.crate_name(*krate).to_string()).collect();
        let mut entries: BTreeSet<String> = BTreeSet::new();
        for (set, prefix) in [(out, ""), (touch, "touch ")] {
            for (path, krate) in set {
                if krate.is_empty() || is_std(&krate) {
                    continue;
                }
                entries.insert(if path.starts_with("cfg ") {
                    path
                } else if foreign.contains(&krate) {
                    format!("foreign {path}")
                } else {
                    format!("{prefix}{path}")
                });
            }
        }
        entries.into_iter().collect()
    })
}

/// The facts of a body. `owner` is the item of the body.
pub fn body_reach(owner: rustc_public::DefId, body: &Body) -> Reach {
    ty::tls::with(|tcx| {
        let owner = rustc_internal::internal(tcx, owner);
        let typing = ty::TypingEnv::post_analysis(tcx, owner);
        let mut reach = Reach::default();
        let mut seen = HashSet::new();
        let mut seen_real = HashSet::new();
        let mut real_drops: BTreeSet<String> = BTreeSet::new();
        let locals = body.locals();

        // A function or a closure as a value: in the type of a local or of a constant.
        struct Values<'a, 'tcx> {
            tcx: TyCtxt<'tcx>,
            reach: &'a mut Reach,
            seen: HashSet<Ty<'tcx>>,
        }
        impl MirVisitor for Values<'_, '_> {
            fn visit_mir_const(&mut self, constant: &MirConst, _location: Location) {
                let ty = rustc_internal::internal(self.tcx, constant.ty());
                note_types(self.tcx, ty, Site::Value, self.reach);
                // A function item or a closure as a constant is a callee, not an input.
                if !matches!(ty.kind(), ty::FnDef(..) | ty::Closure(..)) {
                    let mut found = std::mem::take(&mut self.reach.consts);
                    note_inputs(self.tcx, ty, &mut found, &mut self.seen);
                    self.reach.consts = found;
                }
            }
        }
        // The parameters of a function. The parameters of a closure come from its function or
        // from the code that calls the closure.
        if !tcx.is_closure_like(owner) {
            let mut seen_inputs = HashSet::new();
            let mut found = BTreeSet::new();
            for local in body.arg_locals() {
                note_inputs(tcx, rustc_internal::internal(tcx, local.ty), &mut found, &mut seen_inputs);
            }
            reach.params = found;
        }
        for local in locals {
            note_types(tcx, rustc_internal::internal(tcx, local.ty), Site::Value, &mut reach);
        }
        Values { tcx, reach: &mut reach, seen: HashSet::new() }.visit_body(body);

        for block in &body.blocks {
            for statement in &block.statements {
                let StatementKind::Assign(_, Rvalue::Cast(CastKind::PointerCoercion(PointerCoercion::Unsize), operand, target)) = &statement.kind else { continue };
                if let Ok(source) = operand.ty(locals) {
                    note_types(tcx, rustc_internal::internal(tcx, source), Site::Cast, &mut reach);
                }
                note_types(tcx, rustc_internal::internal(tcx, *target), Site::Cast, &mut reach);
            }
            match &block.terminator.kind {
                TerminatorKind::Call { func, .. } => {
                    let Ok(ty) = func.ty(locals) else { continue };
                    let ty = rustc_internal::internal(tcx, ty);
                    let ty::FnDef(def_id, args) = ty.kind() else { continue };
                    // The opaque value of `config.rs` is not a call of the source.
                    if crate::config::is_black_box(tcx, *def_id) {
                        continue;
                    }
                    if tcx.is_foreign_item(*def_id) {
                        let text = format!("a call of the foreign function `{}`", tcx.def_path_str(*def_id));
                        if !reach.gaps.contains(&text) {
                            reach.gaps.push(text);
                        }
                    }
                    let site = if def_id.is_local() { Site::LocalCall } else { Site::ExternCall };
                    let before = reach.drops.clone();
                    for inner in args.skip_binder().types() {
                        note_types(tcx, inner, site, &mut reach);
                        // Generic code can drop a value of each type that it knows.
                        note_drops(tcx, typing, inner, &mut reach, &mut seen);
                    }
                    // D9: the cause of these types is this call. It is not a drop in the body.
                    let new: Vec<String> = reach.drops.difference(&before).map(|(path, _)| path.clone()).collect();
                    for path in new {
                        reach.drop_calls.entry(path).or_insert_with(|| tcx.def_path_str(*def_id));
                    }
                }
                TerminatorKind::Drop { place, .. } => {
                    let Ok(ty) = place.ty(locals) else {
                        reach.gaps.push("a drop of a value with no known type".to_string());
                        continue;
                    };
                    // The drops of the body itself have their own list of seen types: a type
                    // that a call gave before is a real drop here.
                    let mut real = Reach::default();
                    note_drops(tcx, typing, rustc_internal::internal(tcx, ty), &mut real, &mut seen_real);
                    real_drops.extend(real.drops.iter().map(|(path, _)| path.clone()));
                    reach.drops.extend(real.drops);
                    for gap in real.gaps {
                        if !reach.gaps.contains(&gap) {
                            reach.gaps.push(gap);
                        }
                    }
                }
                TerminatorKind::InlineAsm { .. } => {
                    if !reach.gaps.iter().any(|g| g == "inline assembly") {
                        reach.gaps.push("inline assembly".to_string());
                    }
                }
                _ => {}
            }
        }
        reach.drop_calls.retain(|path, _| !real_drops.contains(path));
        reach
    })
}

/// For a method of a trait impl: the types of the impl header and its trait (full def paths, no
/// item of the standard library). For a provided method of a trait: the trait. The second
/// value is the type of a `Drop` impl, for its `drop` method.
pub fn impl_header(owner: rustc_public::DefId) -> (Vec<String>, Option<String>) {
    ty::tls::with(|tcx| {
        let def_id = rustc_internal::internal(tcx, owner);
        let Some(assoc) = tcx.opt_associated_item(def_id) else { return (Vec::new(), None) };
        let parent = tcx.parent(def_id);
        let mut found = BTreeSet::new();
        let mut drop_of = None;
        match assoc.container {
            ty::AssocContainer::Trait => {
                found.insert(real_path(tcx, parent));
            }
            ty::AssocContainer::TraitImpl(_) => {
                let trait_ref = tcx.impl_trait_ref(parent).instantiate_identity().skip_norm_wip();
                if !is_std(&crate_name(tcx, trait_ref.def_id)) {
                    found.insert(real_path(tcx, trait_ref.def_id));
                }
                for arg in trait_ref.args.types() {
                    for inner in arg.walk() {
                        let Some(inner) = inner.as_type() else { continue };
                        match inner.kind() {
                            ty::Adt(def, _) if !is_std(&crate_name(tcx, def.did())) => {
                                found.insert(real_path(tcx, def.did()));
                            }
                            // `impl Display for dyn LocalTrait`.
                            ty::Dynamic(predicates, ..) => {
                                if let Some(principal) = predicates.principal_def_id().filter(|p| !is_std(&crate_name(tcx, *p))) {
                                    found.insert(real_path(tcx, principal));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if tcx.lang_items().drop_trait() == Some(trait_ref.def_id) {
                    if let ty::Adt(def, _) = trait_ref.self_ty().kind() {
                        drop_of = Some(real_path(tcx, def.did()));
                    }
                }
            }
            ty::AssocContainer::InherentImpl => {}
        }
        (found.into_iter().collect(), drop_of)
    })
}

/// The crate and the def path of a function of another crate.
pub fn origin(def_id: DefId) -> (String, String) {
    ty::tls::with(|tcx| (crate_name(tcx, def_id), real_path(tcx, def_id)))
}

/// The printed path of a function of another crate, as the call list prints it.
pub fn printed_path(def_id: DefId) -> String {
    ty::tls::with(|tcx| extern_crate_path(tcx, tcx.def_path_str(def_id)))
}

/// The blocks that the start block can reach after the constant conditions fold (batch 5f, A2).
/// A `SwitchInt` on a known value has one successor: `if false { f() }` does not reach `f()`.
///
/// A value is known for a constant, and for a local with one write when that write assigns a
/// known value. The rule is not complete: a block that this function gives can be dead.
fn live_blocks(body: &Body) -> Vec<bool> {
    flow(body).0
}

/// The live blocks (see [`live_blocks`]), and the normal successors of each live block: the
/// successors after the constant conditions fold, with no unwind edge (batch 5w, part C).
fn flow(body: &Body) -> (Vec<bool>, Vec<Vec<usize>>) {
    use rustc_public::mir::visit::PlaceContext;
    use rustc_public::mir::{BinOp, Operand, Place, UnOp};
    use rustc_public::ty::{ConstantKind, RigidTy, TyKind};

    struct Writes(Vec<u32>);
    impl MirVisitor for Writes {
        fn visit_place(&mut self, place: &Place, context: PlaceContext, _location: Location) {
            if context.is_mutating() {
                if let Some(count) = self.0.get_mut(place.local) {
                    *count += 1;
                }
            }
        }
    }
    let locals = body.locals();
    let mut writes = Writes(vec![0; locals.len()]);
    writes.visit_body(body);
    // The caller writes a parameter.
    for index in 1..=body.arg_locals().len() {
        if let Some(count) = writes.0.get_mut(index) {
            *count += 1;
        }
    }
    let plain = |ty: rustc_public::ty::Ty| matches!(ty.kind(), TyKind::RigidTy(RigidTy::Bool | RigidTy::Uint(_) | RigidTy::Int(_) | RigidTy::Char));
    // The order of two values of a type: a signed type has the sign in its top bit.
    let order = |ty: rustc_public::ty::Ty, a: u128, b: u128| -> Option<std::cmp::Ordering> {
        match ty.kind() {
            TyKind::RigidTy(RigidTy::Bool | RigidTy::Uint(_) | RigidTy::Char) => Some(a.cmp(&b)),
            TyKind::RigidTy(RigidTy::Int(int)) => {
                let shift = 128 - 8 * int.num_bytes() as u32;
                Some((((a << shift) as i128) >> shift).cmp(&(((b << shift) as i128) >> shift)))
            }
            _ => None,
        }
    };
    let mut known: Vec<Option<u128>> = vec![None; locals.len()];
    let operand = |known: &[Option<u128>], operand: &Operand| -> Option<u128> {
        match operand {
            Operand::Constant(constant) if plain(constant.const_.ty()) => match constant.const_.kind() {
                ConstantKind::Allocated(alloc) => alloc.read_uint().ok(),
                _ => None,
            },
            Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => known.get(place.local).copied().flatten(),
            _ => None,
        }
    };
    let rvalue = |known: &[Option<u128>], rvalue: &Rvalue| -> Option<u128> {
        match rvalue {
            Rvalue::Use(value, _) => operand(known, value),
            Rvalue::UnaryOp(UnOp::Not, value) if matches!(value.ty(locals).ok()?.kind(), TyKind::RigidTy(RigidTy::Bool)) => Some(operand(known, value)? ^ 1),
            Rvalue::BinaryOp(op, left, right) => {
                let ty = left.ty(locals).ok()?;
                let (a, b) = (operand(known, left)?, operand(known, right)?);
                match op {
                    BinOp::Eq if plain(ty) => Some((a == b) as u128),
                    BinOp::Ne if plain(ty) => Some((a != b) as u128),
                    BinOp::Lt => Some(order(ty, a, b)?.is_lt() as u128),
                    BinOp::Le => Some(order(ty, a, b)?.is_le() as u128),
                    BinOp::Gt => Some(order(ty, a, b)?.is_gt() as u128),
                    BinOp::Ge => Some(order(ty, a, b)?.is_ge() as u128),
                    BinOp::BitAnd if plain(ty) => Some(a & b),
                    BinOp::BitOr if plain(ty) => Some(a | b),
                    BinOp::BitXor if plain(ty) => Some(a ^ b),
                    _ => None,
                }
            }
            _ => None,
        }
    };
    // A chain of known locals needs one more round for each link.
    for _ in 0..8 {
        let mut changed = false;
        for block in &body.blocks {
            for statement in &block.statements {
                let StatementKind::Assign(place, value) = &statement.kind else { continue };
                if !place.projection.is_empty() || writes.0.get(place.local) != Some(&1) || known[place.local].is_some() {
                    continue;
                }
                if let Some(found) = rvalue(&known, value) {
                    known[place.local] = Some(found);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut live = vec![false; body.blocks.len()];
    let mut next = vec![Vec::new(); body.blocks.len()];
    let mut todo = vec![0usize];
    while let Some(index) = todo.pop() {
        if index >= live.len() || live[index] {
            continue;
        }
        live[index] = true;
        let terminator = &body.blocks[index].terminator;
        match &terminator.kind {
            TerminatorKind::SwitchInt { discr, targets } => match operand(&known, discr) {
                Some(value) => {
                    let target = targets.branches().find(|(guard, _)| *guard == value).map(|(_, target)| target).unwrap_or(targets.otherwise());
                    next[index].push(target);
                    todo.push(target);
                }
                None => {
                    next[index] = terminator.successors();
                    todo.extend(terminator.successors());
                }
            },
            kind => {
                next[index] = match kind {
                    TerminatorKind::Goto { target } | TerminatorKind::Drop { target, .. } | TerminatorKind::Assert { target, .. } => vec![*target],
                    TerminatorKind::Call { target, .. } | TerminatorKind::InlineAsm { destination: target, .. } => target.iter().copied().collect(),
                    _ => Vec::new(),
                };
                todo.extend(terminator.successors());
            }
        }
    }
    (live, next)
}

/// The functions that a body calls on each path from its start to a normal return (batch 5w,
/// part C): the names, as [`live_callees`] gives them. A path that ends with a panic does not
/// count. If no path gets to a return, each path to an end of the body counts (a diverging
/// call, an abort).
///
/// Only a call terminator with a constant callee counts. A direct call of a local closure
/// gives the sure callees of the closure. A function that the body gives to a different
/// function (`map(f)`) is not a sure callee: Semilla does not know that the function runs.
///
/// The second value has the facts for the ends of a run that are not a return (batch 5y, A4).
/// The keys:
///
/// - `ends`: the calls of the body and of its closures that end the process and are not panic
///   code (`std::process::exit`, a call with no return, a foreign function).
/// - `calls`: the functions that the body and its closures can call and that can end the
///   process, as far as the driver knows. The verifier follows them.
/// - `before NAME`: the entries of `ends` (with `end ` first) and of `calls` that can run
///   before the sure callee `NAME`.
/// - `first`: the sure callees with no call, no assert, and no drop before them. In a
///   `#[should_panic]` test, only such a call runs in each run.
pub fn sure_callees(owner: rustc_public::DefId, body: &Body, bodies: &std::collections::HashMap<rustc_public::DefId, &Body>) -> Option<(Vec<String>, std::collections::BTreeMap<String, Vec<String>>)> {
    ty::tls::with(|tcx| {
        let owner = rustc_internal::internal(tcx, owner);
        // Only a function has the fact. The descriptor constant of a test has the name of the
        // test function, and it has no call.
        if !matches!(tcx.def_kind(owner), rustc_hir::def::DefKind::Fn | rustc_hir::def::DefKind::AssocFn) {
            return None;
        }
        let mut open = HashSet::new();
        let mut opens = Vec::new();
        let sure: Vec<String> = sure_walk(tcx, owner, body, bodies, &mut open, Some(&mut opens)).into_iter().collect();
        let facts = end_facts(tcx, bodies);
        let mut ends = std::collections::BTreeMap::new();
        let none: Vec<(usize, Ender)> = Vec::new();
        let own = facts.scans.get(&owner).unwrap_or(&none);
        let all = flat(tcx, &facts, own.iter().map(|(_, ender)| ender));
        let (end, calls): (Vec<String>, Vec<String>) = all.into_iter().partition(|entry| entry.starts_with("end "));
        let mut put = |key: String, list: Vec<String>| {
            if !list.is_empty() {
                ends.insert(key, list);
            }
        };
        put("ends".into(), end.iter().map(|entry| entry["end ".len()..].to_string()).collect());
        put("calls".into(), calls);
        let mut first = Vec::new();
        for (name, seen, closures) in opens {
            // The call can be in a closure that the body calls: each ender of the closure
            // counts (a conservative rule).
            let inner: Vec<Ender> = closures.into_iter().map(|def| Ender::Local(def, String::new())).collect();
            put(format!("before {name}"), flat(tcx, &facts, own.iter().filter(|(index, _)| seen[*index]).map(|(_, ender)| ender).chain(&inner)));
            let can_panic = |kind: &TerminatorKind| matches!(kind, TerminatorKind::Call { .. } | TerminatorKind::Assert { .. } | TerminatorKind::Drop { .. } | TerminatorKind::InlineAsm { .. });
            if !body.blocks.iter().enumerate().any(|(index, block)| seen[index] && can_panic(&block.terminator.kind)) {
                first.push(name);
            }
        }
        put("first".into(), first);
        Some((sure, ends))
    })
}

/// What a block can run that matters for the end of the process (batch 5y, A4).
#[derive(Clone)]
enum Ender {
    /// A call that ends the process and that is not panic code: the name.
    End(String),
    /// A function of a different crate that is not of the standard library: the driver does
    /// not know its body.
    Extern(String),
    /// A local function or a local closure.
    Local(DefId, String),
}

/// The enders of each local body, and the local bodies that can get to an end of the process
/// or to code that the driver does not know.
struct EndFacts {
    scans: std::collections::HashMap<DefId, Vec<(usize, Ender)>>,
    maybe: HashSet<DefId>,
}

thread_local! {
    static END_FACTS: std::cell::RefCell<Option<std::rc::Rc<EndFacts>>> = const { std::cell::RefCell::new(None) };
}

/// The facts of [`EndFacts`] for the crate. One driver process compiles one crate.
fn end_facts<'tcx>(tcx: TyCtxt<'tcx>, bodies: &std::collections::HashMap<rustc_public::DefId, &Body>) -> std::rc::Rc<EndFacts> {
    if let Some(found) = END_FACTS.with(|cell| cell.borrow().clone()) {
        return found;
    }
    let mut scans = std::collections::HashMap::new();
    for (def, body) in bodies {
        let def = rustc_internal::internal(tcx, *def);
        if tcx.is_closure_like(def) || matches!(tcx.def_kind(def), rustc_hir::def::DefKind::Fn | rustc_hir::def::DefKind::AssocFn) {
            scans.insert(def, enders(tcx, def, body));
        }
    }
    // A local function with no body here is code that the driver does not know.
    let unknown = |ender: &Ender, maybe: &HashSet<DefId>| match ender {
        Ender::Local(def, _) => maybe.contains(def) || !scans.contains_key(def),
        _ => true,
    };
    let mut maybe: HashSet<DefId> = HashSet::new();
    loop {
        let more: Vec<DefId> = scans.iter().filter(|(def, list)| !maybe.contains(*def) && list.iter().any(|(_, ender)| unknown(ender, &maybe))).map(|(def, _)| *def).collect();
        if more.is_empty() {
            break;
        }
        maybe.extend(more);
    }
    let facts = std::rc::Rc::new(EndFacts { scans, maybe });
    END_FACTS.with(|cell| *cell.borrow_mut() = Some(facts.clone()));
    facts
}

/// The entries of a list of enders for the verifier: `end NAME` for an end of the process, and
/// `NAME` for a function that can get to one. A closure gives the entries of its body.
fn flat<'a, 'tcx>(tcx: TyCtxt<'tcx>, facts: &'a EndFacts, enders: impl Iterator<Item = &'a Ender>) -> Vec<String> {
    let mut found = BTreeSet::new();
    let mut seen = HashSet::new();
    let mut todo: Vec<&Ender> = enders.collect();
    while let Some(ender) = todo.pop() {
        match ender {
            Ender::End(name) => drop(found.insert(format!("end {name}"))),
            Ender::Extern(name) => drop(found.insert(name.clone())),
            Ender::Local(def, name) => {
                if tcx.is_closure_like(*def) {
                    if seen.insert(*def) {
                        todo.extend(facts.scans.get(def).into_iter().flatten().map(|(_, ender)| ender));
                    }
                } else if facts.maybe.contains(def) || !facts.scans.contains_key(def) {
                    found.insert(name.clone());
                }
            }
        }
    }
    found.into_iter().collect()
}

/// True for a function of the standard library that is panic code. A call of it with no return
/// is a panic, not an end of the process with success.
fn panic_code(name: &str) -> bool {
    name.contains("panic") || name.contains("unwind") || name.ends_with("_failed") || name.ends_with("_fail")
}

/// The enders of the live blocks of a body, with the block index. The types of the constants
/// of a block give the functions that the block calls or gives to a different function, and
/// the closures.
fn enders<'tcx>(tcx: TyCtxt<'tcx>, owner: DefId, body: &Body) -> Vec<(usize, Ender)> {
    use rustc_public::mir::AggregateKind;
    use rustc_public::CrateDef;

    struct Types<'tcx> {
        tcx: TyCtxt<'tcx>,
        types: Vec<Ty<'tcx>>,
        made: Vec<DefId>,
    }
    impl MirVisitor for Types<'_> {
        fn visit_mir_const(&mut self, constant: &MirConst, _location: Location) {
            self.types.push(rustc_internal::internal(self.tcx, constant.ty()));
        }
        fn visit_rvalue(&mut self, rvalue: &Rvalue, location: Location) {
            match rvalue {
                Rvalue::Aggregate(AggregateKind::Closure(def, _), _) => self.made.push(rustc_internal::internal(self.tcx, def.def_id())),
                Rvalue::Aggregate(AggregateKind::Coroutine(def, _), _) => self.made.push(rustc_internal::internal(self.tcx, def.def_id())),
                _ => {}
            }
            self.super_rvalue(rvalue, location);
        }
    }
    let typing = ty::TypingEnv::post_analysis(tcx, owner);
    let live = live_blocks(body);
    let mut found = Vec::new();
    for (index, block) in body.blocks.iter().enumerate() {
        if !live[index] {
            continue;
        }
        let mut types = Types { tcx, types: Vec::new(), made: Vec::new() };
        types.visit_basic_block(block);
        let mut defs: Vec<DefId> = types.made;
        for ty in types.types {
            for arg in ty.walk() {
                let Some(inner) = arg.as_type() else { continue };
                match inner.kind() {
                    ty::FnDef(def_id, args) => {
                        let mut target = *def_id;
                        if matches!(tcx.def_kind(*def_id), rustc_hir::def::DefKind::Fn | rustc_hir::def::DefKind::AssocFn) {
                            let args = args.skip_binder();
                            let resolved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ty::Instance::try_resolve(tcx, typing, *def_id, args)));
                            if let Ok(Ok(Some(instance))) = resolved {
                                if let ty::InstanceKind::Item(item) = instance.def {
                                    target = item;
                                }
                            }
                        }
                        defs.push(target);
                    }
                    ty::Closure(def_id, _) | ty::Coroutine(def_id, _) | ty::CoroutineClosure(def_id, _) => defs.push(*def_id),
                    _ => {}
                }
            }
        }
        // A call with no return through a function pointer: Semilla does not know the function.
        if let TerminatorKind::Call { func, target: None, .. } = &block.terminator.kind {
            let direct = func.ty(body.locals()).ok().is_some_and(|ty| matches!(rustc_internal::internal(tcx, ty).kind(), ty::FnDef(..)));
            if !direct {
                found.push((index, Ender::End("a call with no return through a function pointer".to_string())));
            }
        }
        for def in defs {
            let local = def.is_local();
            if tcx.is_closure_like(def) {
                if local {
                    found.push((index, Ender::Local(def, tcx.def_path_str(def))));
                }
                continue;
            }
            if !matches!(tcx.def_kind(def), rustc_hir::def::DefKind::Fn | rustc_hir::def::DefKind::AssocFn) {
                continue;
            }
            let name = if local { tcx.def_path_str(def) } else { real_path(tcx, def) };
            let library = is_std(&crate_name(tcx, def));
            let ender = if tcx.is_foreign_item(def) {
                // A foreign function can do each thing (`exit` of the C library).
                (!library).then(|| Ender::End(name))
            } else if local {
                Some(Ender::Local(def, name))
            } else if name.ends_with("CommandExt>::exec") || name.ends_with("CommandExt::exec") {
                // `exec` puts a different program in the process.
                Some(Ender::End(name))
            } else if tcx.fn_sig(def).skip_binder().output().skip_binder().is_never() {
                (!(library && panic_code(&name))).then(|| Ender::End(name))
            } else {
                (!library).then(|| Ender::Extern(name))
            };
            found.extend(ender.map(|ender| (index, ender)));
        }
    }
    found
}

fn sure_walk<'tcx>(tcx: TyCtxt<'tcx>, owner: DefId, body: &Body, bodies: &std::collections::HashMap<rustc_public::DefId, &Body>, open: &mut HashSet<DefId>, mut opens: Option<&mut Vec<(String, Vec<bool>, Vec<DefId>)>>) -> BTreeSet<String> {
    // The closures that give a callee name through a direct call.
    let mut through: std::collections::BTreeMap<String, Vec<DefId>> = std::collections::BTreeMap::new();
    use rustc_public::mir::Operand;

    let typing = ty::TypingEnv::post_analysis(tcx, owner);
    let (live, next) = flow(body);
    // The blocks of each callee name.
    let mut sites: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
    for (index, block) in body.blocks.iter().enumerate() {
        if !live[index] {
            continue;
        }
        let TerminatorKind::Call { func: Operand::Constant(func), .. } = &block.terminator.kind else { continue };
        let ty::FnDef(def_id, args) = rustc_internal::internal(tcx, func.const_.ty()).kind() else { continue };
        let mut target = *def_id;
        if matches!(tcx.def_kind(*def_id), rustc_hir::def::DefKind::Fn | rustc_hir::def::DefKind::AssocFn) {
            let args = args.skip_binder();
            let resolved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ty::Instance::try_resolve(tcx, typing, *def_id, args)));
            if let Ok(Ok(Some(instance))) = resolved {
                if let ty::InstanceKind::Item(item) = instance.def {
                    target = item;
                }
            }
        }
        let names: Vec<String> = if tcx.is_closure_like(target) {
            // A direct call of a closure: the sure callees of the closure body.
            match bodies.get(&rustc_internal::stable(target)).filter(|_| target.is_local() && open.insert(target)) {
                Some(inner) => {
                    let found = sure_walk(tcx, target, inner, bodies, open, None);
                    open.remove(&target);
                    for name in &found {
                        through.entry(name.clone()).or_default().push(target);
                    }
                    found.into_iter().collect()
                }
                None => Vec::new(),
            }
        } else if target.is_local() {
            vec![tcx.def_path_str(target)]
        } else {
            vec![real_path(tcx, target)]
        };
        for name in names {
            sites.entry(name).or_default().push(index);
        }
    }
    // The ends of the body: the return blocks, or each block with no successor if no return
    // block can run. A `Resume` block is the end of a panic path.
    let reach = |closed: &[usize]| -> Vec<bool> {
        let mut seen = vec![false; body.blocks.len()];
        let mut todo = vec![0usize];
        while let Some(index) = todo.pop() {
            if index >= seen.len() || seen[index] || closed.contains(&index) {
                continue;
            }
            seen[index] = true;
            todo.extend(next[index].iter().copied());
        }
        seen
    };
    let all = reach(&[]);
    let is_end = |index: usize, kind: &TerminatorKind| all[index] && next[index].is_empty() && !matches!(kind, TerminatorKind::Resume);
    let returns: Vec<usize> = body.blocks.iter().enumerate().filter(|(index, block)| all[*index] && matches!(block.terminator.kind, TerminatorKind::Return)).map(|(index, _)| index).collect();
    let ends: Vec<usize> = if returns.is_empty() { body.blocks.iter().enumerate().filter(|(index, block)| is_end(*index, &block.terminator.kind)).map(|(index, _)| index).collect() } else { returns };
    let mut sure = BTreeSet::new();
    for (name, blocks) in sites {
        // The call is sure if no end can run when the call blocks are closed. A call block
        // that is an end (a diverging call) is closed too.
        let open_ends = reach(&blocks);
        if !ends.is_empty() && ends.iter().all(|end| !open_ends[*end]) {
            if let Some(opens) = opens.as_mut() {
                opens.push((name.clone(), open_ends, through.remove(&name).unwrap_or_default()));
            }
            sure.insert(name);
        }
    }
    sure
}

/// The functions that a body can call from the blocks that its start block reaches (batch 5f,
/// A2): the def paths, with the crate name first. A trait method resolves to the method of the
/// impl when the types are known. The walk goes into each local closure and coroutine that a
/// call of a live block names in its types, and into the coroutine of an `async fn`. `bodies`
/// gives the body of a local item.
pub fn live_callees(owner: rustc_public::DefId, body: &Body, bodies: &std::collections::HashMap<rustc_public::DefId, &Body>) -> Option<Vec<String>> {
    ty::tls::with(|tcx| {
        let owner = rustc_internal::internal(tcx, owner);
        // The body that makes a closure has the calls of the closure.
        if tcx.is_closure_like(owner) {
            return None;
        }
        let mut found = BTreeSet::new();
        let mut seen = HashSet::new();
        live_walk(tcx, owner, body, bodies, &mut found, &mut seen);
        Some(found.into_iter().collect())
    })
}

fn live_walk<'tcx>(tcx: TyCtxt<'tcx>, owner: DefId, body: &Body, bodies: &std::collections::HashMap<rustc_public::DefId, &Body>, found: &mut BTreeSet<String>, seen: &mut HashSet<DefId>) {
    use rustc_public::mir::AggregateKind;
    use rustc_public::CrateDef;

    struct Types<'tcx> {
        tcx: TyCtxt<'tcx>,
        types: Vec<Ty<'tcx>>,
        made: Vec<DefId>,
    }
    impl MirVisitor for Types<'_> {
        fn visit_mir_const(&mut self, constant: &MirConst, _location: Location) {
            self.types.push(rustc_internal::internal(self.tcx, constant.ty()));
        }
        fn visit_rvalue(&mut self, rvalue: &Rvalue, location: Location) {
            // The body of an `async fn` is a coroutine: the function makes it and gives it to
            // its caller. A closure or an `async` block that a body only makes does not run: a
            // call names its type.
            if let Rvalue::Aggregate(AggregateKind::Coroutine(def, _), _) = rvalue {
                let def = rustc_internal::internal(self.tcx, def.def_id());
                if matches!(self.tcx.coroutine_kind(def), Some(rustc_hir::CoroutineKind::Desugared(_, rustc_hir::CoroutineSource::Fn))) {
                    self.made.push(def);
                }
            }
            self.super_rvalue(rvalue, location);
        }
    }
    let typing = ty::TypingEnv::post_analysis(tcx, owner);
    let live = live_blocks(body);
    let mut types = Types { tcx, types: Vec::new(), made: Vec::new() };
    for (block, live) in body.blocks.iter().zip(&live) {
        if *live {
            types.visit_basic_block(block);
        }
    }
    let mut nested: Vec<DefId> = types.made;
    for ty in types.types {
        for arg in ty.walk() {
            let Some(inner) = arg.as_type() else { continue };
            match inner.kind() {
                ty::FnDef(def_id, args) => {
                    let mut target = *def_id;
                    if matches!(tcx.def_kind(*def_id), rustc_hir::def::DefKind::Fn | rustc_hir::def::DefKind::AssocFn) {
                        let args = args.skip_binder();
                        let resolved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ty::Instance::try_resolve(tcx, typing, *def_id, args)));
                        if let Ok(Ok(Some(instance))) = resolved {
                            if let ty::InstanceKind::Item(item) = instance.def {
                                target = item;
                            }
                        }
                    }
                    if tcx.is_closure_like(target) {
                        nested.push(target);
                    } else if target.is_local() {
                        found.insert(tcx.def_path_str(target));
                    } else {
                        found.insert(real_path(tcx, target));
                    }
                }
                ty::Closure(def_id, _) | ty::Coroutine(def_id, _) | ty::CoroutineClosure(def_id, _) => nested.push(*def_id),
                _ => {}
            }
        }
    }
    for def_id in nested {
        if !def_id.is_local() || !seen.insert(def_id) {
            continue;
        }
        if let Some(body) = bodies.get(&rustc_internal::stable(def_id)) {
            live_walk(tcx, def_id, body, bodies, found, seen);
        }
    }
}
