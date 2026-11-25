//! The instances of the generic code and the candidates of a `dyn` call (design 61, part B).
//!
//! The MIR of a generic body keeps each call that depends on a type parameter
//! (`<T as shop::Tax>::rate`). The driver finds the instances that the workspace uses: from each
//! body with no generic parameter (a root), it follows each call of a generic function outside
//! the standard library with concrete generic arguments, and then each such call in the body of
//! the instance, to depth [`MAX_DEPTH`]. For each instance, the record of the root has the
//! instance text and the resolved callee of each call that the instance resolves.
//!
//! The record of a body also has each conversion to `dyn Trait` in it: the trait and the source
//! type. The candidates of a call through `dyn Trait` are the source types of the workspace.

use rustc_public::mir::{Body, CastKind, PointerCoercion, Rvalue, StatementKind, TerminatorKind};
use rustc_public::ty::{RigidTy, TyKind};
use rustc_public::CrateDef;

/// The depth of the walk through the instances.
pub const MAX_DEPTH: usize = 3;
/// The most instances of one root.
const MAX_INSTANCES: usize = 64;

/// One instance of a generic body.
#[derive(serde::Serialize, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstanceRecord {
    /// The name of the generic body (its def path).
    pub g: String,
    /// The text of the instance: the path with each generic argument.
    pub t: String,
    /// Each call of the generic body that the instance resolves: the text in the generic body
    /// and the resolved callee, in the order of the blocks.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub c: Vec<(String, String)>,
}

fn std_crate(name: &str) -> bool {
    crate::stdlib::is_std_name(name)
}

/// The instances that a root body makes.
pub fn of(body: &Body) -> Vec<InstanceRecord> {
    let mut found: Vec<InstanceRecord> = Vec::new();
    let mut todo: Vec<(Body, usize)> = vec![(body.clone(), 0)];
    while let Some((current, depth)) = todo.pop() {
        for block in &current.blocks {
            let TerminatorKind::Call { func, .. } = &block.terminator.kind else { continue };
            let Ok(ty) = func.ty(current.locals()) else { continue };
            let TyKind::RigidTy(RigidTy::FnDef(def, args)) = ty.kind() else { continue };
            if args.0.is_empty() || std_crate(&def.krate().name) {
                continue;
            }
            // Only concrete arguments: an argument with a parameter is not an instance yet.
            if !crate::ir::concrete(&args) {
                continue;
            }
            let Ok(instance) = rustc_public::mir::mono::Instance::resolve(def, &args) else { continue };
            // An instance of a generic body: the resolved instance has generic arguments (a
            // trait method can resolve to an impl item with none).
            if !instance.has_body() || std_crate(&instance.def.krate().name) || instance.intrinsic_name().is_some() || instance.args().0.is_empty() {
                continue;
            }
            let generic = instance.def.name();
            let item = rustc_public::CrateItem(instance.def.def_id());
            let text = rustc_middle::ty::tls::with(|tcx| {
                let internal = rustc_public::rustc_internal::internal(tcx, instance);
                tcx.def_path_str_with_args(internal.def_id(), internal.args)
            });
            if found.iter().any(|known| known.t == text) || found.len() >= MAX_INSTANCES {
                continue;
            }
            let Some(mono) = crate::safe::instance_body(&instance) else { continue };
            let Some(poly) = item.body() else { continue };
            let calls = resolved_calls(&poly, &mono, item.def_id());
            found.push(InstanceRecord { g: generic, t: text, c: calls });
            if depth + 1 < MAX_DEPTH {
                todo.push((mono, depth + 1));
            }
        }
    }
    found.sort();
    found
}

/// The calls of a generic body that the instance resolves to a different text: the text in the
/// generic body and the resolved callee. The two bodies have the same blocks.
fn resolved_calls(poly: &Body, mono: &Body, item: rustc_public::DefId) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if poly.blocks.len() != mono.blocks.len() {
        return out;
    }
    rustc_middle::ty::tls::with(|tcx| {
        let local = rustc_public::rustc_internal::internal(tcx, item);
        let typing = rustc_middle::ty::TypingEnv::post_analysis(tcx, local);
        for (a, b) in poly.blocks.iter().zip(&mono.blocks) {
            let (TerminatorKind::Call { func: f, .. }, TerminatorKind::Call { func: g, .. }) = (&a.terminator.kind, &b.terminator.kind) else { continue };
            let span = rustc_public::rustc_internal::internal(tcx, a.terminator.source_info.span);
            let (generic, _) = crate::graph::callee_text(tcx, Some(typing), poly, f, span);
            let (resolved, _) = crate::graph::callee_text(tcx, Some(rustc_middle::ty::TypingEnv::fully_monomorphized()), mono, g, span);
            if generic != resolved {
                out.push((generic, resolved));
            }
        }
    });
    out
}

/// The target of a reference, of a raw pointer, or of a `Box` (the first type argument).
fn pointee<'tcx>(ty: rustc_middle::ty::Ty<'tcx>) -> Option<rustc_middle::ty::Ty<'tcx>> {
    match ty.kind() {
        rustc_middle::ty::Ref(_, inner, _) | rustc_middle::ty::RawPtr(inner, _) => Some(*inner),
        rustc_middle::ty::Adt(_, args) => args.types().next(),
        _ => None,
    }
}

/// The conversions to `dyn Trait` in a body: the trait and the source type, sorted.
pub fn dyn_casts(body: &Body) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for block in &body.blocks {
        for statement in &block.statements {
            let StatementKind::Assign(_, Rvalue::Cast(CastKind::PointerCoercion(PointerCoercion::Unsize), operand, target)) = &statement.kind else { continue };
            let Ok(source) = operand.ty(body.locals()) else { continue };
            let found = rustc_middle::ty::tls::with(|tcx| {
                let (source, target) = (rustc_public::rustc_internal::internal(tcx, source), rustc_public::rustc_internal::internal(tcx, *target));
                let (from, to) = (pointee(source)?, pointee(target)?);
                let rustc_middle::ty::Dynamic(predicates, ..) = to.kind() else { return None };
                let principal = predicates.principal_def_id()?;
                Some((tcx.def_path_str(principal), format!("{from}")))
            });
            if let Some(entry) = found
                && !out.contains(&entry)
            {
                out.push(entry);
            }
        }
    }
    out.sort();
    out
}
