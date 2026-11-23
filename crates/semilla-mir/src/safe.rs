//! Safe reads of the compiler data for the extraction of the driver (design 61.9, part 0).
//!
//! An extraction of Semilla never changes the result of a build. A query of the compiler with
//! data that does not exist for the item (the MIR of a virtual call or of an intrinsic, the
//! normal form of a type with a projection of a generic parameter) stops the compiler with an
//! internal compiler error. A `catch_unwind` does not undo it: the compiler counts the error
//! before the panic. So the driver asks only for data that exists:
//!
//! - The body of an instance only for a user item with MIR and concrete arguments: not for a
//!   virtual call, an intrinsic, a shim, or drop glue.
//! - A type with arguments only through the `try_` form of the normalization, in the typing
//!   environment of the body. A type that still has a parameter or a projection after it gives
//!   no type: the caller then uses an opaque type.

use rustc_middle::ty::{self, TypeVisitableExt};
use rustc_public_bridge::IndexedVal;
use rustc_public::mir::Body;
use rustc_public::mir::mono::{Instance, InstanceKind};

/// The cause of the extraction step that failed, for the mark "no form: <cause>".
pub fn no_form(cause: &str) -> String {
    format!("no form: {cause}")
}

/// True if a value has no parameter, no projection, and no escaping bound variable.
fn concrete<'tcx, T: TypeVisitableExt<ty::TyCtxt<'tcx>>>(value: &T) -> bool {
    !value.has_param() && !value.has_aliases() && !value.has_escaping_bound_vars()
}

/// The body of an instance, only if the compiler has MIR for it: a user item (not a virtual
/// call, an intrinsic, a shim, or drop glue) with concrete arguments and available MIR.
pub fn instance_body(instance: &Instance) -> Option<Body> {
    if instance.kind != InstanceKind::Item || !instance.has_body() {
        return None;
    }
    let ok = rustc_middle::ty::tls::with(|tcx| {
        let internal = rustc_public::rustc_internal::internal(tcx, *instance);
        let def = internal.def_id();
        matches!(internal.def, ty::InstanceKind::Item(_)) && tcx.intrinsic(def).is_none() && tcx.is_mir_available(def) && concrete(&internal.args)
    });
    if !ok {
        return None;
    }
    instance.body()
}

/// The type of a field of an ADT variant with the arguments.
pub fn field_ty<'tcx>(tcx: ty::TyCtxt<'tcx>, typing: ty::TypingEnv<'tcx>, adt: rustc_public::ty::AdtDef, variant: rustc_public::ty::VariantIdx, field: usize, args: &rustc_public::ty::GenericArgs) -> Option<rustc_public::ty::Ty> {
    let adt = rustc_public::rustc_internal::internal(tcx, adt);
    let variant = adt.variants().iter().nth(variant.to_index())?;
    let did = variant.fields.iter().nth(field)?.did;
    normalized(tcx, typing, did, args)
}

/// The type of an ADT with the arguments.
pub fn def_ty_with_args<'tcx>(tcx: ty::TyCtxt<'tcx>, typing: ty::TypingEnv<'tcx>, def: rustc_public::DefId, args: &rustc_public::ty::GenericArgs) -> Option<rustc_public::ty::Ty> {
    normalized(tcx, typing, rustc_public::rustc_internal::internal(tcx, def), args)
}

/// The type of a definition with the arguments, normalized in the typing environment `typing`
/// with the `try_` form. No type if the normalization fails, or if the type still has a
/// projection, or a parameter that the environment does not know.
fn normalized<'tcx>(tcx: ty::TyCtxt<'tcx>, typing: ty::TypingEnv<'tcx>, def: rustc_hir::def_id::DefId, args: &rustc_public::ty::GenericArgs) -> Option<rustc_public::ty::Ty> {
    let args = rustc_public::rustc_internal::internal(tcx, args);
    let raw = tcx.type_of(def).instantiate(tcx, args);
    let found = tcx.try_normalize_erasing_regions(typing, raw).ok()?;
    if found.references_error() || found.has_aliases() || found.has_escaping_bound_vars() {
        return None;
    }
    // A parameter is a type only in the environment of a body that has it.
    if found.has_param() && typing == ty::TypingEnv::fully_monomorphized() {
        return None;
    }
    Some(rustc_public::rustc_internal::stable(found))
}

/// The type of a field with the arguments, as `FieldDef::ty_with_args` gives it, with no
/// internal compiler error (design 61.14, R61.14-3): `ty_with_args` normalizes in the fully
/// monomorphized environment, and a projection of a parameter (`<D as Data>::Id`) has no normal
/// form there. Such a type stays as the declaration gives it, with the arguments in place. The
/// text is then a function of the declaration and the arguments, as the text of a generic
/// layout (`declared_layout`).
pub fn field_ty_with_args(adt: rustc_public::ty::AdtDef, variant: rustc_public::ty::VariantIdx, field: usize, args: &rustc_public::ty::GenericArgs) -> Option<rustc_public::ty::Ty> {
    rustc_middle::ty::tls::with(|tcx| {
        let adt = rustc_public::rustc_internal::internal(tcx, adt);
        let did = adt.variants().iter().nth(variant.to_index())?.fields.iter().nth(field)?.did;
        let args = rustc_public::rustc_internal::internal(tcx, args);
        let raw = tcx.type_of(did).instantiate(tcx, args);
        let found = match tcx.try_normalize_erasing_regions(ty::TypingEnv::fully_monomorphized(), raw) {
            Ok(found) => found,
            Err(_) => tcx.erase_and_anonymize_regions(raw.skip_norm_wip()),
        };
        Some(rustc_public::rustc_internal::stable(found))
    })
}

/// `needs_drop` with no query for a type that the typing environment cannot know (a parameter
/// in the fully monomorphized environment): such a type counts as a type with a destructor.
pub fn needs_drop<'tcx>(tcx: ty::TyCtxt<'tcx>, typing: ty::TypingEnv<'tcx>, ty: ty::Ty<'tcx>) -> bool {
    if typing == ty::TypingEnv::fully_monomorphized() && !concrete(&ty) {
        return true;
    }
    if ty.references_error() {
        return true;
    }
    ty.needs_drop(tcx, typing)
}
