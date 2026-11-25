//! The effect summary of a call (design 64, parts A and B).
//!
//! The equivalence check gives a call with no effect as a value: an uninterpreted function of
//! its arguments. A call has an order only if it can have an effect. The driver gives each call
//! of a form a summary from the MIR of the callee and of each body that it calls, to a fixed
//! depth:
//!
//! - `w` (the world): the call can read or write a place that the check does not know. A write
//!   or a read through a raw pointer, a raw pointer that the body makes (an address), a static
//!   that can change (`static mut`, a static with interior mutability, a thread local), inline
//!   assembly, an intrinsic outside the pure list, a call that the MIR does not resolve (`dyn`,
//!   a function pointer, a trait method on a type parameter), a callee with no MIR, a drop of a
//!   value with a destructor, and a callee past the depth limit give `w` with the cause.
//! - Interior mutability: a shared reference to a type that is not `Freeze` (the answer of the
//!   compiler) in an argument gives `w`. Two such references can point to one place, and the
//!   check does not know the aliases.
//! - `p`: the call can stop with no return value: a check (`assert`), a panic of the standard
//!   library, a loop, or a recursive call (the call can run forever).
//!
//! The summary is sound: each part that the walk does not know gives `w`. A missing part never
//! gives a smaller summary.

use std::cell::RefCell;
use std::collections::HashMap;

use rustc_public::mir::mono::{Instance, InstanceKind};
use rustc_public::mir::{Body, CastKind, ConstOperand, NonDivergingIntrinsic, Operand, Place, ProjectionElem, Rvalue, StatementKind, TerminatorKind};
use rustc_public::ty::{ConstantKind, RigidTy, Ty, TyKind};
use rustc_public::CrateDef;
use serde::Serialize;

/// The depth of the walk below a call (the callee is depth 1). Decided by Claude (design 64):
/// the MIR of the standard library is deep (the code of `fmt`), and each level costs a body of
/// an instance. A callee past the limit gives `w`.
pub const DEPTH: usize = 4;

/// The effect summary of a call.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct IrEff {
    /// The cause of an effect on the world (no cause: no such effect).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<String>,
    /// True if the call can stop with no return value (a panic, a loop).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub p: bool,
}

impl IrEff {
    fn world(cause: impl Into<String>) -> IrEff {
        IrEff { w: Some(cause.into()), p: true }
    }

    fn join(&mut self, other: &IrEff) {
        if self.w.is_none() {
            self.w = other.w.clone();
        }
        self.p |= other.p;
    }
}

thread_local! {
    /// The summary of each instance that the walk finished (with no open cycle).
    static MEMO: RefCell<HashMap<Instance, IrEff>> = RefCell::new(HashMap::new());
    /// The answer of `shared_interior` for each type.
    static INTERIOR: RefCell<HashMap<Ty, bool>> = RefCell::new(HashMap::new());
    /// The answer of `mut_inside` for each type.
    static MUT_INSIDE: RefCell<HashMap<Ty, bool>> = RefCell::new(HashMap::new());
}

/// The intrinsics with no effect and no panic: a value of their arguments (an unchecked
/// operation is undefined behavior on overflow, and the check assumes no undefined behavior).
const PURE_INTRINSICS: &[&str] = &[
    "add_with_overflow", "sub_with_overflow", "mul_with_overflow", "wrapping_add", "wrapping_sub", "wrapping_mul", "saturating_add",
    "saturating_sub", "unchecked_add", "unchecked_sub", "unchecked_mul", "unchecked_div", "unchecked_rem", "unchecked_shl", "unchecked_shr",
    "exact_div", "rotate_left", "rotate_right", "ctpop", "ctlz", "cttz", "ctlz_nonzero", "cttz_nonzero", "bswap", "bitreverse",
    "three_way_compare", "disjoint_bitor", "carrying_mul_add", "likely", "unlikely", "cold_path", "assume", "size_of", "align_of",
    "min_align_of", "pref_align_of", "needs_drop", "type_id", "type_name", "ub_checks", "black_box", "select_unpredictable", "integer_min",
    "integer_max", "discriminant_value",
];

/// The summary of a call terminator of `body`: the summary of the callee, and the rule of
/// interior mutability for the arguments.
pub fn of_call(body: &Body, func: &Operand, args: &[Operand]) -> IrEff {
    let mut found = callee(body, func, 1, &mut Vec::new()).0;
    for arg in args {
        let Ok(ty) = arg.ty(body.locals()) else {
            found.join(&IrEff::world("an argument with no type"));
            continue;
        };
        if shared_interior(ty) {
            found.join(&IrEff::world("an argument with interior mutability (a shared reference to a type that is not `Freeze`)"));
        }
        // The check writes the target of a `&mut` argument. A `&mut` reference inside a value
        // of an argument (a capture of a closure, a field of a type that the check does not
        // model) points to a place that the check cannot see.
        if mut_inside(ty) {
            found.join(&IrEff::world("an argument with a `&mut` reference inside a value"));
        }
    }
    found
}

/// True if a type has a `&mut` reference inside it: in a field, an element, a type argument, or
/// a capture of a closure. A `&mut` reference at the top of the type does not count (the check
/// knows its target). A type with a parameter gives true.
fn mut_inside(ty: Ty) -> bool {
    if let Some(found) = MUT_INSIDE.with(|memo| memo.borrow().get(&ty).copied()) {
        return found;
    }
    let found = rustc_middle::ty::tls::with(|tcx| {
        use rustc_middle::ty as mty;
        let internal = rustc_public::rustc_internal::internal(tcx, ty);
        let inner = match internal.kind() {
            mty::Ref(_, inner, rustc_middle::mir::Mutability::Mut) => *inner,
            _ => internal,
        };
        mut_in(tcx, inner, 0)
    });
    MUT_INSIDE.with(|memo| memo.borrow_mut().insert(ty, found));
    found
}

fn mut_in<'tcx>(tcx: rustc_middle::ty::TyCtxt<'tcx>, ty: rustc_middle::ty::Ty<'tcx>, depth: usize) -> bool {
    use rustc_middle::ty::{self as mty, TypeVisitableExt};
    if depth > 6 || ty.has_param() || ty.has_aliases() {
        return true;
    }
    let typing = mty::TypingEnv::fully_monomorphized();
    for part in ty.walk() {
        let Some(part) = part.as_type() else { continue };
        match part.kind() {
            mty::Ref(_, _, rustc_middle::mir::Mutability::Mut) | mty::Dynamic(..) | mty::Foreign(..) => return true,
            mty::Adt(def, args) => {
                if def.is_union() {
                    return true;
                }
                for field in def.all_fields() {
                    let Ok(field_ty) = tcx.try_normalize_erasing_regions(typing, field.ty(tcx, args)) else { return true };
                    if mut_in(tcx, field_ty, depth + 1) {
                        return true;
                    }
                }
            }
            mty::Closure(_, args) => {
                if mut_in(tcx, args.as_closure().tupled_upvars_ty(), depth + 1) {
                    return true;
                }
            }
            mty::Coroutine(..) | mty::CoroutineClosure(..) => return true,
            _ => {}
        }
    }
    false
}

/// The summary of the callee of a call: the summary and the lowest stack index of an open
/// instance that it used (a cycle).
fn callee(body: &Body, func: &Operand, depth: usize, stack: &mut Vec<Instance>) -> (IrEff, usize) {
    let Ok(ty) = func.ty(body.locals()) else { return (IrEff::world("a callee with no type"), usize::MAX) };
    match ty.kind() {
        TyKind::RigidTy(RigidTy::FnDef(def, args)) => {
            if !crate::ir::concrete(&args) {
                return (IrEff::world("a call of a trait method on a type parameter"), usize::MAX);
            }
            match Instance::resolve(def, &args) {
                Ok(instance) => instance_summary(instance, depth, stack),
                Err(_) => (IrEff::world("a call that the compiler does not resolve"), usize::MAX),
            }
        }
        TyKind::RigidTy(RigidTy::FnPtr(_)) => (IrEff::world("a call through a function pointer"), usize::MAX),
        _ => (IrEff::world("a call of an unknown callee"), usize::MAX),
    }
}

/// True for a function that never returns (the return type `!`) and has the place of its caller
/// (`#[track_caller]`): a panic function (`core::panicking::panic_fmt`, `std::rt::begin_panic`,
/// `core::option::unwrap_failed`). This is the structural rule of R65.4-1: the compiler gives the
/// two marks, and no name decides it. An exit or an abort has no `#[track_caller]`, and
/// `std::panicking::catch_unwind`, `set_hook`, and `take_hook` return: their MIR decides.
fn stops_with_panic(instance: Instance) -> bool {
    if !matches!(instance.kind, InstanceKind::Item) {
        return false;
    }
    rustc_middle::ty::tls::with(|tcx| {
        let internal = rustc_public::rustc_internal::internal(tcx, instance);
        let def = internal.def_id();
        if !matches!(tcx.def_kind(def), rustc_hir::def::DefKind::Fn | rustc_hir::def::DefKind::AssocFn) {
            return false;
        }
        let never = tcx.fn_sig(def).skip_binder().output().skip_binder().is_never();
        let caller = tcx.codegen_fn_attrs(def).flags.contains(rustc_middle::middle::codegen_fn_attrs::CodegenFnAttrFlags::TRACK_CALLER);
        never && caller
    })
}

fn instance_summary(instance: Instance, depth: usize, stack: &mut Vec<Instance>) -> (IrEff, usize) {
    if let Some(name) = instance.intrinsic_name() {
        if PURE_INTRINSICS.contains(&name.as_str()) {
            return (IrEff::default(), usize::MAX);
        }
        return (IrEff::world(format!("the intrinsic `{name}`")), usize::MAX);
    }
    let name = instance.def.name();
    if stops_with_panic(instance) {
        return (IrEff { w: None, p: true }, usize::MAX);
    }
    match instance.kind {
        InstanceKind::Virtual { .. } => return (IrEff::world("a `dyn` call"), usize::MAX),
        InstanceKind::Item => {}
        _ => return (IrEff::world(format!("a callee with no MIR: `{name}`")), usize::MAX),
    }
    if let Some(found) = MEMO.with(|memo| memo.borrow().get(&instance).cloned()) {
        return (found, usize::MAX);
    }
    if let Some(at) = stack.iter().position(|open| *open == instance) {
        // A recursive call: it can run forever. The rest of its summary is the summary of the
        // open instance.
        return (IrEff { w: None, p: true }, at);
    }
    if depth > DEPTH {
        return (IrEff::world(format!("the depth limit of the effect summary ({DEPTH})")), usize::MAX);
    }
    let Some(body) = crate::guarded(|| crate::safe::instance_body(&instance)).flatten() else {
        return (IrEff::world(format!("a callee with no MIR: `{name}`")), usize::MAX);
    };
    stack.push(instance);
    let at = stack.len() - 1;
    let (found, low) = body_summary(&body, depth, stack);
    stack.pop();
    if low >= at {
        MEMO.with(|memo| memo.borrow_mut().insert(instance, found.clone()));
        (found, usize::MAX)
    } else {
        (found, low)
    }
}

/// The summary of the parts of a body, with its calls to the depth limit.
fn body_summary(body: &Body, depth: usize, stack: &mut Vec<Instance>) -> (IrEff, usize) {
    let mut found = IrEff::default();
    let mut low = usize::MAX;
    for (index, block) in body.blocks.iter().enumerate() {
        for statement in &block.statements {
            match &statement.kind {
                StatementKind::Assign(place, value) => {
                    place_parts(body, place, &mut found);
                    rvalue_parts(body, value, &mut found);
                }
                StatementKind::SetDiscriminant { place, .. } => place_parts(body, place, &mut found),
                StatementKind::Intrinsic(NonDivergingIntrinsic::CopyNonOverlapping(_)) => found.join(&IrEff::world("a copy through a raw pointer")),
                _ => {}
            }
        }
        let targets: Vec<usize> = match &block.terminator.kind {
            TerminatorKind::Goto { target } => vec![*target],
            TerminatorKind::SwitchInt { discr, targets } => {
                operand_parts(body, discr, &mut found);
                targets.all_targets()
            }
            TerminatorKind::Resume | TerminatorKind::Return | TerminatorKind::Unreachable => Vec::new(),
            TerminatorKind::Abort => {
                found.p = true;
                Vec::new()
            }
            TerminatorKind::Drop { place, target, .. } => {
                place_parts(body, place, &mut found);
                if let Ok(ty) = place.ty(body.locals())
                    && needs_drop(ty)
                    && !drop_is_free(ty)
                {
                    found.join(&IrEff::world(format!("a drop of a value with a destructor (`{ty}`)")));
                }
                vec![*target]
            }
            TerminatorKind::Call { func, args, destination, target, .. } => {
                operand_parts(body, func, &mut found);
                for arg in args {
                    operand_parts(body, arg, &mut found);
                    if let Ok(ty) = arg.ty(body.locals())
                        && shared_interior(ty)
                    {
                        found.join(&IrEff::world("an argument with interior mutability (a shared reference to a type that is not `Freeze`)"));
                    }
                }
                place_parts(body, destination, &mut found);
                if seq_model(func, body).is_some() {
                    // An operation of the sequence model writes only through its `&mut`
                    // argument (the elements that the `Vec` owns) and can panic.
                    found.p = true;
                } else {
                    let (inner, at) = callee(body, func, depth + 1, stack);
                    found.join(&inner);
                    low = low.min(at);
                }
                if target.is_none() {
                    found.p = true;
                }
                target.iter().copied().collect()
            }
            TerminatorKind::Assert { cond, target, .. } => {
                operand_parts(body, cond, &mut found);
                found.p = true;
                vec![*target]
            }
            TerminatorKind::InlineAsm { .. } => {
                found.join(&IrEff::world("inline assembly"));
                Vec::new()
            }
        };
        // A jump back (to this block or to an earlier one) can be a loop: the call can run
        // forever. The order of the blocks is a reverse post order in most bodies; a jump back
        // that is not a loop only adds `p`.
        if targets.iter().any(|target| *target <= index) {
            found.p = true;
        }
        if found.w.is_some() {
            // The world is the largest summary: the rest of the body adds nothing.
            return (found, low);
        }
    }
    (found, low)
}

fn operand_parts(body: &Body, operand: &Operand, found: &mut IrEff) {
    match operand {
        Operand::Copy(place) | Operand::Move(place) => place_parts(body, place, found),
        Operand::Constant(constant) => constant_parts(constant, found),
        Operand::RuntimeChecks(_) => {}
    }
}

/// A static that can change gives `w`; a static that cannot change (not `mut`, no interior
/// mutability) is a constant.
fn constant_parts(constant: &ConstOperand, found: &mut IrEff) {
    let ConstantKind::Allocated(alloc) = constant.const_.kind() else { return };
    for (_, provenance) in &alloc.provenance.ptrs {
        if let rustc_public::mir::alloc::GlobalAlloc::Static(def) = rustc_public::mir::alloc::GlobalAlloc::from(provenance.0) {
            if !plain_static(&def) {
                found.join(&IrEff::world(format!("a static that can change (`{}`)", def.name())));
            }
        }
    }
}

/// True for a static that cannot change: not `mut`, not foreign, not a thread local, and of a
/// type with no interior mutability (`Freeze`). Its value is a constant (R64.2, R68.6-1).
pub fn plain_static(def: &rustc_public::mir::mono::StaticDef) -> bool {
    rustc_middle::ty::tls::with(|tcx| {
        let id = rustc_public::rustc_internal::internal(tcx, def.def_id());
        !tcx.is_mutable_static(id)
            && !tcx.is_foreign_item(id)
            && !tcx.is_thread_local_static(id)
            && tcx.type_of(id).instantiate_identity().skip_norm_wip().is_freeze(tcx, rustc_middle::ty::TypingEnv::fully_monomorphized())
    })
}

/// A deref of a raw pointer in a place gives `w`.
fn place_parts(body: &Body, place: &Place, found: &mut IrEff) {
    let Some(decl) = body.locals().get(place.local) else {
        found.join(&IrEff::world("a place with no type"));
        return;
    };
    let mut ty = decl.ty;
    for projection in &place.projection {
        if matches!(projection, ProjectionElem::Deref) && matches!(ty.kind(), TyKind::RigidTy(RigidTy::RawPtr(..))) {
            found.join(&IrEff::world("a read or a write through a raw pointer"));
            return;
        }
        match projection.ty(ty) {
            Ok(next) => ty = next,
            Err(_) => {
                found.join(&IrEff::world("a place with no type"));
                return;
            }
        }
    }
}

fn rvalue_parts(body: &Body, value: &Rvalue, found: &mut IrEff) {
    match value {
        Rvalue::AddressOf(..) => found.join(&IrEff::world("a raw pointer (an address)")),
        Rvalue::ThreadLocalRef(_) => found.join(&IrEff::world("a thread local")),
        Rvalue::Cast(kind, operand, to) => {
            operand_parts(body, operand, found);
            let raw = matches!(to.kind(), TyKind::RigidTy(RigidTy::RawPtr(..)));
            if raw || matches!(kind, CastKind::PointerExposeAddress | CastKind::PointerWithExposedProvenance | CastKind::Transmute | CastKind::FnPtrToPtr) {
                found.join(&IrEff::world("a raw pointer or an address (a cast)"));
            }
        }
        Rvalue::Use(operand, _) | Rvalue::UnaryOp(_, operand) | Rvalue::Repeat(operand, _) => operand_parts(body, operand, found),
        Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
            operand_parts(body, a, found);
            operand_parts(body, b, found);
        }
        Rvalue::Aggregate(kind, operands) => {
            if matches!(kind, rustc_public::mir::AggregateKind::RawPtr(..)) {
                found.join(&IrEff::world("a raw pointer (an address)"));
            }
            for operand in operands {
                operand_parts(body, operand, found);
            }
        }
        Rvalue::CopyForDeref(place) | Rvalue::Discriminant(place) | Rvalue::Len(place) | Rvalue::Ref(_, _, place) | Rvalue::Reborrow(_, _, place) => place_parts(body, place, found),
    }
}

fn needs_drop(ty: Ty) -> bool {
    rustc_middle::ty::tls::with(|tcx| {
        let internal = rustc_public::rustc_internal::internal(tcx, ty);
        crate::safe::needs_drop(tcx, rustc_middle::ty::TypingEnv::fully_monomorphized(), internal)
    })
}

/// True for a `Vec`, a `String`, or a `Box` (of the global allocator) of a type with no
/// destructor: its drop only frees memory, and the check does not model an allocation.
fn drop_is_free(ty: Ty) -> bool {
    let TyKind::RigidTy(RigidTy::Adt(def, args)) = ty.kind() else { return false };
    let name = def.name();
    let known = matches!(name.as_str(), "std::vec::Vec" | "alloc::vec::Vec" | "std::string::String" | "alloc::string::String" | "std::boxed::Box" | "alloc::boxed::Box");
    if !known {
        return false;
    }
    args.0.iter().all(|arg| match arg {
        rustc_public::ty::GenericArgKind::Type(inner) => match inner.kind() {
            TyKind::RigidTy(RigidTy::Adt(allocator, _)) if allocator.name().ends_with("alloc::Global") => true,
            _ => !needs_drop(*inner) && !matches!(inner.kind(), TyKind::RigidTy(RigidTy::Dynamic(..) | RigidTy::Str | RigidTy::Slice(_))),
        },
        _ => true,
    })
}

/// True if a type has a shared reference to a type that is not `Freeze` (the answer of the
/// compiler), also in a field, an element, or a type argument. A type with a parameter gives
/// true (the check cannot know it).
pub fn shared_interior(ty: Ty) -> bool {
    if let Some(found) = INTERIOR.with(|memo| memo.borrow().get(&ty).copied()) {
        return found;
    }
    let found = rustc_middle::ty::tls::with(|tcx| {
        let internal = rustc_public::rustc_internal::internal(tcx, ty);
        interior_in(tcx, internal, 0)
    });
    INTERIOR.with(|memo| memo.borrow_mut().insert(ty, found));
    found
}

fn interior_in<'tcx>(tcx: rustc_middle::ty::TyCtxt<'tcx>, ty: rustc_middle::ty::Ty<'tcx>, depth: usize) -> bool {
    use rustc_middle::ty::{self as mty, TypeVisitableExt};
    if depth > 6 {
        return true;
    }
    if ty.has_param() || ty.has_aliases() {
        return true;
    }
    let typing = mty::TypingEnv::fully_monomorphized();
    for part in ty.walk() {
        let Some(part) = part.as_type() else { continue };
        match part.kind() {
            mty::Ref(_, inner, rustc_middle::mir::Mutability::Not) if !inner.is_freeze(tcx, typing) => return true,
            mty::Adt(def, args) => {
                if def.is_union() {
                    return true;
                }
                for field in def.all_fields() {
                    let Ok(field_ty) = tcx.try_normalize_erasing_regions(typing, field.ty(tcx, args)) else { return true };
                    if interior_in(tcx, field_ty, depth + 1) {
                        return true;
                    }
                }
            }
            mty::Closure(..) | mty::Coroutine(..) | mty::CoroutineClosure(..) => {
                // The captures of a closure: a capture by a shared reference of a type that is
                // not `Freeze` is in the tuple of the captures.
                if let mty::Closure(_, args) = part.kind()
                    && interior_in(tcx, args.as_closure().tupled_upvars_ty(), depth + 1)
                {
                    return true;
                }
                if !matches!(part.kind(), mty::Closure(..)) {
                    return true;
                }
            }
            mty::Dynamic(..) | mty::Foreign(..) => return true,
            _ => {}
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------
// The sequence model (design 64, part C).

/// True for an integer, a `bool`, or a `char`: the elements of the sequence model.
pub fn is_scalar(ty: Ty) -> bool {
    matches!(ty.kind(), TyKind::RigidTy(RigidTy::Int(_) | RigidTy::Uint(_) | RigidTy::Bool | RigidTy::Char))
}

/// The element type of a `Vec` of scalars with the global allocator.
pub fn scalar_vec(def: rustc_public::ty::AdtDef, args: &rustc_public::ty::GenericArgs) -> Option<Ty> {
    if !matches!(def.name().as_str(), "std::vec::Vec" | "alloc::vec::Vec") {
        return None;
    }
    let mut types = args.0.iter().filter_map(|arg| match arg {
        rustc_public::ty::GenericArgKind::Type(ty) => Some(*ty),
        _ => None,
    });
    let item = types.next()?;
    let allocator = types.next()?;
    let global = matches!(allocator.kind(), TyKind::RigidTy(RigidTy::Adt(def, _)) if def.name().ends_with("alloc::Global"));
    (global && is_scalar(item)).then_some(item)
}

/// True for `&[T]` or `&mut [T]` with a scalar `T`.
pub fn scalar_slice_ref(ty: Ty) -> bool {
    matches!(ty.kind(), TyKind::RigidTy(RigidTy::Ref(_, inner, _)) if matches!(inner.kind(), TyKind::RigidTy(RigidTy::Slice(item)) if is_scalar(item)))
}

/// The operation of the sequence model of a call: the callee of the standard library and its
/// arguments (a `Vec` of scalars with the global allocator, a slice of scalars, an index of the
/// type `usize`).
pub fn seq_model(func: &Operand, body: &Body) -> Option<String> {
    let ty = func.ty(body.locals()).ok()?;
    let TyKind::RigidTy(RigidTy::FnDef(def, args)) = ty.kind() else { return None };
    if !crate::ir::concrete(&args) {
        return None;
    }
    let instance = Instance::resolve(def, &args).ok()?;
    let name = instance.def.name();
    let name = name.as_str();
    let types: Vec<Ty> = instance.args().0.iter().filter_map(|arg| match arg {
        rustc_public::ty::GenericArgKind::Type(ty) => Some(*ty),
        _ => None,
    }).collect();
    let usize_index = |at: usize| types.get(at).is_none_or(|ty| matches!(ty.kind(), TyKind::RigidTy(RigidTy::Uint(rustc_public::ty::UintTy::Usize))));
    let vec_name = |rest: &str| ["std::vec::Vec::<T, A>::", "alloc::vec::Vec::<T, A>::", "std::vec::Vec::<T>::", "alloc::vec::Vec::<T>::"].iter().any(|head| name.strip_prefix(head) == Some(rest));
    let vec_ok = || {
        let item = types.first().copied();
        let allocator_ok = types.get(1).is_none_or(|allocator| matches!(allocator.kind(), TyKind::RigidTy(RigidTy::Adt(def, _)) if def.name().ends_with("alloc::Global")));
        item.is_some_and(is_scalar) && allocator_ok
    };
    let slice_ok = || types.first().is_some_and(|item| is_scalar(*item));
    for op in ["new", "with_capacity", "push", "pop", "len", "is_empty", "clear", "truncate", "insert", "remove", "extend_from_slice"] {
        if vec_name(op) && vec_ok() {
            return Some(format!("vec_{op}"));
        }
    }
    let trait_vec = |head: &str| name.starts_with(&format!("<std::vec::Vec<T, A> as {head}")) || name.starts_with(&format!("<alloc::vec::Vec<T, A> as {head}"));
    if trait_vec("std::ops::Deref>::deref") || trait_vec("core::ops::Deref>::deref") {
        return vec_ok().then(|| "vec_deref".into());
    }
    if trait_vec("std::ops::DerefMut>::deref_mut") || trait_vec("core::ops::DerefMut>::deref_mut") {
        return vec_ok().then(|| "vec_deref".into());
    }
    // `impl<T, I, A> Index<I> for Vec<T, A>`: the arguments are `T`, `I`, `A`.
    let index_ok = || {
        let allocator_ok = types.get(2).is_some_and(|allocator| matches!(allocator.kind(), TyKind::RigidTy(RigidTy::Adt(def, _)) if def.name().ends_with("alloc::Global")));
        types.first().is_some_and(|item| is_scalar(*item)) && usize_index(1) && allocator_ok
    };
    if (trait_vec("std::ops::Index<I>>::index") || trait_vec("core::ops::Index<I>>::index")) && index_ok() {
        return Some("vec_index".into());
    }
    if (trait_vec("std::ops::IndexMut<I>>::index_mut") || trait_vec("core::ops::IndexMut<I>>::index_mut")) && index_ok() {
        return Some("vec_index".into());
    }
    if let Some(rest) = name.strip_prefix("core::slice::<impl [T]>::") {
        let op = rest.split("::<").next().unwrap_or(rest);
        let ok = slice_ok() && usize_index(1);
        return match op {
            "len" | "is_empty" | "swap" | "first" | "last" | "get" if ok => Some(format!("slice_{op}")),
            _ => None,
        };
    }
    None
}
