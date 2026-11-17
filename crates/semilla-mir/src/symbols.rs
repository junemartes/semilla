//! The symbols that each MIR element names, and the symbols that each definition names (design
//! 44).
//!
//! At N0, rustc copies no body into a different body. So the compiled code of a body can change
//! with no edit of its text only through a symbol that the body names. The driver records these
//! symbols by the IDs of the compiler (`DefId`), not by the text:
//!
//! - the callee of a call, with the types of its generic arguments; for a trait method, also the
//!   method and the `impl` that rustc resolves for the concrete types;
//! - each named constant (`Unevaluated`), also an associated constant, before the printer
//!   evaluates it; each static; each function in a constant in memory;
//! - each field (`k::Pair.a`) and each variant of a place or an aggregate;
//! - each struct, enum, union, trait, closure (with its parent), and opaque type (with its
//!   function) in the types of the operands, the places, and the result;
//! - the `Drop` impl of the type of a dropped place, and of each type with drop glue in it; the
//!   same for the drops of the cleanup path that an unwind edge enters;
//! - the function of the body itself, for an element that reads an argument or writes the
//!   return place: the signature gives their types;
//! - the HIR supplement: each path of the source inside the range of the element (a type alias,
//!   a constant, a function, a type), and the `use` item that the first segment of a path comes
//!   through. MIR expands a type alias, and it does not show a `use` line.
//!
//! A symbol has the def path (the form of the MIR body names) and the place of its definition: a
//! file of the workspace and a byte range in the file as written. The verifier finds the changed
//! definitions by their places. A symbol outside the workspace is not recorded: no edit of the
//! step can change it.
//!
//! The record of a definition (a unit record at N0) lists the symbols that the definition names:
//! the types of a signature and its bounds, the type and the value of a constant or a static,
//! the field types of a struct, an enum, or a union, the aliased type, the header and the items
//! of an `impl`, the items of a trait. A symbol of the value of a constant has the flag
//! `EVALUATED` (rustc runs a `const fn` there at compile time); a symbol of the body of a
//! `const fn` has the flag `BODY`.

use std::collections::{BTreeSet, HashMap};

use rustc_hir::def::{DefKind, Res};
use rustc_hir::intravisit::{self, Visitor as HirVisitor};
use rustc_middle::ty::{self, TyCtxt};
use rustc_public::mir::visit::{Location, MirVisitor, PlaceContext};
use rustc_public::mir::{Body, Local, Place, ProjectionElem, TerminatorKind, UnwindAction};
use rustc_public::ty::{ConstantKind, MirConst, RigidTy, TyKind};
use rustc_public::CrateDef;
use rustc_public_bridge::IndexedVal;
use rustc_span::Span;
use rustc_span::def_id::{DefId, LocalDefId};

/// A symbol of the value of a constant or a static: rustc evaluates it at compile time.
pub const EVALUATED: u8 = 1;
/// A symbol of the body of a `const fn`: it counts only when a constant evaluates the function.
pub const BODY: u8 = 2;
/// The self type of an `impl`: an added or a removed `impl` changes the methods of the type.
pub const SELF_TYPE: u8 = 4;
/// A target of a `use` item.
pub const USE_TARGET: u8 = 8;
/// The name that a `use` item imports: a symbol with no place.
pub const USE_NAME: u8 = 16;

/// The most cleanup blocks that the walk of one unwind edge reads.
const MAX_CLEANUP: usize = 64;
/// The most types that the drop walk of one type reads.
const MAX_DROP_TYPES: usize = 256;

/// One symbol: the def path, the file (an index into the file list of the record), and the
/// start and the end of the definition.
#[derive(serde::Serialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Symbol(pub String, pub i64, pub i64, pub i64);

/// The symbol table of a record, with the IDs.
#[derive(Default)]
pub struct Table {
    pub symbols: Vec<Symbol>,
    index: HashMap<DefId, Option<u32>>,
}

impl Table {
    /// The index of a symbol with a name and no place (the name of a `use` item).
    pub fn name(&mut self, name: &str) -> u32 {
        let symbol = Symbol(name.to_string(), -1, 0, 0);
        match self.symbols.iter().position(|known| *known == symbol) {
            Some(index) => index as u32,
            None => {
                self.symbols.push(symbol);
                (self.symbols.len() - 1) as u32
            }
        }
    }

    /// The index of the symbol of a definition, or `None` for a definition outside the workspace.
    pub fn add(&mut self, tcx: TyCtxt<'_>, def_id: DefId, files: &mut Vec<String>) -> Option<u32> {
        if let Some(found) = self.index.get(&def_id) {
            return *found;
        }
        let found = place(tcx, def_id, files).map(|(file, start, end)| {
            self.symbols.push(Symbol(path(tcx, def_id), file, start, end));
            (self.symbols.len() - 1) as u32
        });
        self.index.insert(def_id, found);
        found
    }
}

/// The def path of a symbol. A field is `TYPE.NAME` (`k::Pair.a`, `k::Ev::A.x`), as in the
/// canonical text and in the item keys.
fn path(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    if tcx.def_kind(def_id) == DefKind::Field
        && let Some(parent) = tcx.opt_parent(def_id)
    {
        let owner = if tcx.def_kind(parent) == DefKind::Variant || matches!(tcx.def_kind(parent), DefKind::Struct | DefKind::Union) { parent } else { def_id };
        return format!("{}.{}", crate::reach::real_path(tcx, owner), tcx.item_name(def_id));
    }
    crate::reach::real_path(tcx, def_id)
}

/// The place of the definition of a symbol in a file of the workspace. A local definition has
/// its full source range (a function with its body); a definition of a different crate of the
/// workspace has the range of its head (`def_span`).
fn place(tcx: TyCtxt<'_>, def_id: DefId, files: &mut Vec<String>) -> Option<(i64, i64, i64)> {
    if !crate::config::is_workspace_crate(tcx, def_id.krate) {
        return None;
    }
    let span = match def_id.as_local() {
        Some(local) => tcx.source_span(local),
        None => tcx.def_span(def_id),
    };
    let span = span.source_callsite();
    crate::spans::range(tcx, span, files)
}

/// The symbols of each element of a canonical body, in the order of `spans.rs`: the symbol
/// table and, for each element with a symbol, its index list.
pub fn elements(body: &Body, item: rustc_public::DefId, files: &mut Vec<String>) -> (Vec<Symbol>, Vec<Vec<u32>>, Option<u32>, bool) {
    rustc_middle::ty::tls::with(|tcx| {
        let item = rustc_public::rustc_internal::internal(tcx, item);
        let total = body.blocks.iter().map(|block| block.statements.len() + 1).sum();
        let mut collect = Elements { tcx, body, item, table: Table::default(), files, current: None, total, per: Vec::new() };
        collect.visit_body(body);
        let Elements { mut table, mut per, files, .. } = collect;
        // The symbol of the function of the body: the signature line names it too.
        let own = if matches!(tcx.def_kind(item), DefKind::Fn | DefKind::AssocFn) { table.add(tcx, item, files) } else { None };
        // The HIR supplement: the paths inside the range of each element.
        let paths = hir_paths(tcx, item, files);
        let element_spans: Vec<rustc_public::ty::Span> = body
            .blocks
            .iter()
            .flat_map(|block| block.statements.iter().map(|statement| statement.source_info.span).chain(std::iter::once(block.terminator.source_info.span)))
            .collect();
        per.resize(element_spans.len(), BTreeSet::new());
        for (index, span) in element_spans.into_iter().enumerate() {
            let span = rustc_public::rustc_internal::internal(tcx, span);
            let Some((file, start, end)) = crate::spans::range(tcx, span, files) else { continue };
            for (place, def_id) in &paths {
                if place.0 == file && start <= place.1 && place.2 <= end {
                    if let Some(found) = table.add(tcx, *def_id, files) {
                        per[index].insert(found);
                    }
                }
            }
        }
        // A body that rustc evaluates at compile time: a constant, a static, a constant block.
        let evaluated = matches!(tcx.def_kind(tcx.typeck_root_def_id(item)), DefKind::Const { .. } | DefKind::Static { .. } | DefKind::AssocConst { .. } | DefKind::AnonConst)
            || matches!(tcx.def_kind(item), DefKind::AnonConst);
        (table.symbols, per.into_iter().map(|set| set.into_iter().collect()).collect(), own, evaluated)
    })
}

/// The visitor of the elements of one body.
struct Elements<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    body: &'a Body,
    item: DefId,
    table: Table,
    files: &'a mut Vec<String>,
    /// The index of the element that the visitor reads, and the count of the elements. After
    /// the last element, the visitor reads the declarations of the locals: no element.
    current: Option<usize>,
    total: usize,
    per: Vec<BTreeSet<u32>>,
}

impl Elements<'_, '_> {
    /// Each element of a function names the function: the signature gives the types of the
    /// arguments and of the return place, and these types go into each element (design 44).
    fn own(&mut self) {
        if matches!(self.tcx.def_kind(self.item), DefKind::Fn | DefKind::AssocFn) {
            self.note(self.item);
        }
    }

    fn note(&mut self, def_id: DefId) {
        let Some(index) = self.current else { return };
        if let Some(found) = self.table.add(self.tcx, def_id, self.files) {
            if self.per.len() <= index {
                self.per.resize(index + 1, BTreeSet::new());
            }
            self.per[index].insert(found);
        }
    }

    /// The symbols of a type: each ADT, function, closure (and its parent), foreign type, trait
    /// object trait, and opaque type (and its function) in it.
    fn ty(&mut self, ty: rustc_public::ty::Ty) {
        let ty = rustc_public::rustc_internal::internal(self.tcx, ty);
        for arg in ty.walk() {
            let Some(ty) = arg.as_type() else { continue };
            match ty.kind() {
                ty::Adt(def, _) => self.note(def.did()),
                ty::FnDef(def_id, _) | ty::Foreign(def_id) => self.note(*def_id),
                ty::Closure(def_id, _) | ty::Coroutine(def_id, _) | ty::CoroutineClosure(def_id, _) => {
                    self.note(*def_id);
                    self.note(self.tcx.typeck_root_def_id(*def_id));
                }
                ty::Alias(_, alias) => {
                    if let ty::AliasTyKind::Opaque { def_id } = alias.kind {
                        self.note(def_id);
                        if let Some(parent) = self.tcx.opt_parent(def_id) {
                            self.note(parent);
                        }
                    }
                }
                ty::Dynamic(predicates, ..) => {
                    for predicate in predicates.iter() {
                        if let ty::ExistentialPredicate::Trait(trait_ref) = predicate.skip_binder() {
                            self.note(trait_ref.def_id);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The destructors that a drop of a value of the type runs: the `Drop` impl of each type
    /// with drop glue in it.
    fn drops(&mut self, ty: rustc_public::ty::Ty) {
        let ty = rustc_public::rustc_internal::internal(self.tcx, ty);
        let mut seen = std::collections::HashSet::new();
        let mut queue = vec![ty];
        while let Some(ty) = queue.pop() {
            if !seen.insert(ty) || seen.len() > MAX_DROP_TYPES {
                continue;
            }
            match ty.kind() {
                ty::Adt(def, args) => {
                    if def.is_manually_drop() || def.is_phantom_data() {
                        continue;
                    }
                    if !crate::config::is_workspace_crate(self.tcx, def.did().krate) {
                        queue.extend(args.types());
                        continue;
                    }
                    if let Some(destructor) = self.tcx.adt_destructor(def.did()) {
                        self.note(destructor.did);
                        if let Some(parent) = self.tcx.opt_parent(destructor.did) {
                            self.note(parent);
                        }
                    }
                    self.note(def.did());
                    for field in def.all_fields() {
                        queue.push(field.ty(self.tcx, args).skip_norm_wip());
                    }
                }
                ty::Tuple(list) => queue.extend(list.iter()),
                ty::Array(inner, _) | ty::Slice(inner) => queue.push(*inner),
                _ => {}
            }
        }
    }

    /// The drops of the cleanup path that an unwind edge enters.
    fn cleanup(&mut self, unwind: Option<&UnwindAction>) {
        let Some(UnwindAction::Cleanup(start)) = unwind else { return };
        let mut seen = BTreeSet::new();
        let mut queue = vec![*start];
        while let Some(block) = queue.pop() {
            if !seen.insert(block) || seen.len() > MAX_CLEANUP {
                continue;
            }
            let Some(found) = self.body.blocks.get(block) else { continue };
            match &found.terminator.kind {
                TerminatorKind::Drop { place, target, .. } => {
                    if let Ok(ty) = place.ty(self.body.locals()) {
                        self.drops(ty);
                    }
                    queue.push(*target);
                }
                TerminatorKind::Goto { target } => queue.push(*target),
                _ => {}
            }
        }
    }

    /// The symbols of a call: the callee, the generic arguments, and for a trait method the
    /// method and the `impl` that rustc resolves.
    fn callee(&mut self, func: &rustc_public::mir::Operand) {
        let Ok(ty) = func.ty(self.body.locals()) else { return };
        let TyKind::RigidTy(RigidTy::FnDef(def, args)) = ty.kind() else { return };
        // rustc stops with an internal error for the impl of a call with a generic argument.
        if crate::is_opaque_wrap(func) || crate::is_generic(&args) {
            return;
        }
        if let Ok(instance) = rustc_public::mir::mono::Instance::resolve(def, &args) {
            let resolved = rustc_public::rustc_internal::internal(self.tcx, instance.def.def_id());
            self.note(resolved);
            if let Some(parent) = self.tcx.opt_parent(resolved).filter(|parent| matches!(self.tcx.def_kind(*parent), DefKind::Impl { .. })) {
                self.note(parent);
            }
        }
        let def_id = rustc_public::rustc_internal::internal(self.tcx, def.def_id());
        let args = rustc_public::rustc_internal::internal(self.tcx, &args);
        for impl_id in projection_impls(self.tcx, def_id, args) {
            self.note(impl_id);
        }
    }
}

/// The impls of the workspace that normalize a projection of the signature of a callee for its
/// concrete generic arguments (`<Dog as Speak>::Out` in the return type of `call::<Dog>`).
fn projection_impls<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId, args: ty::GenericArgsRef<'tcx>) -> Vec<DefId> {
    let signature = tcx.fn_sig(def_id).instantiate(tcx, args).skip_norm_wip();
    let mut impls = Vec::new();
    for ty in signature.skip_binder().inputs_and_output.iter() {
        for arg in ty.walk() {
            let Some(ty) = arg.as_type() else { continue };
            let ty::Alias(_, alias) = ty.kind() else { continue };
            let ty::AliasTyKind::Projection { def_id: item } = alias.kind else { continue };
            let Some(trait_id) = tcx.opt_parent(item) else { continue };
            let self_ty = alias.args.type_at(0);
            for impl_id in tcx.all_impls(trait_id) {
                if crate::config::is_workspace_crate(tcx, impl_id.krate) && tcx.impl_trait_ref(impl_id).instantiate_identity().skip_norm_wip().self_ty() == self_ty && !impls.contains(&impl_id) {
                    impls.push(impl_id);
                }
            }
        }
    }
    impls
}

impl MirVisitor for Elements<'_, '_> {
    fn visit_statement(&mut self, statement: &rustc_public::mir::Statement, location: Location) {
        self.current = Some(self.current.map_or(0, |index| index + 1));
        self.own();
        self.super_statement(statement, location);
    }

    fn visit_terminator(&mut self, terminator: &rustc_public::mir::Terminator, location: Location) {
        self.current = Some(self.current.map_or(0, |index| index + 1));
        self.own();
        match &terminator.kind {
            TerminatorKind::Call { func, .. } => self.callee(func),
            TerminatorKind::Drop { place, .. } => {
                if let Ok(ty) = place.ty(self.body.locals()) {
                    self.drops(ty);
                }
            }
            _ => {}
        }
        self.cleanup(terminator.kind.unwind());
        self.super_terminator(terminator, location);
        if self.current.is_some_and(|index| index + 1 >= self.total) {
            self.current = None;
        }
    }

    fn visit_place(&mut self, place: &Place, context: PlaceContext, location: Location) {
        let mut base = self.body.locals().get(place.local).map(|decl| decl.ty);
        let mut variant = None;
        for elem in &place.projection {
            if let Some(ty) = base {
                self.ty(ty);
            }
            match elem {
                ProjectionElem::Field(index, _) => {
                    if let Some(TyKind::RigidTy(RigidTy::Adt(def, _))) = base.map(|ty| ty.kind()) {
                        let def = rustc_public::rustc_internal::internal(self.tcx, def);
                        let variant_index = variant.map_or(rustc_abi::FIRST_VARIANT, |index: rustc_public::ty::VariantIdx| rustc_abi::VariantIdx::from_usize(index.to_index()));
                        if let Some(field) = def.variants().get(variant_index).and_then(|found| found.fields.get(rustc_abi::FieldIdx::from_usize(*index))) {
                            self.note(field.did);
                        }
                    }
                }
                ProjectionElem::Downcast(index) => {
                    if let Some(TyKind::RigidTy(RigidTy::Adt(def, _))) = base.map(|ty| ty.kind()) {
                        let def = rustc_public::rustc_internal::internal(self.tcx, def);
                        if let Some(found) = def.variants().get(rustc_abi::VariantIdx::from_usize(index.to_index())) {
                            self.note(found.def_id);
                        }
                    }
                }
                _ => {}
            }
            variant = match elem {
                ProjectionElem::Downcast(index) => Some(*index),
                _ => None,
            };
            base = base.and_then(|ty| elem.ty(ty).ok());
        }
        if let Some(ty) = base {
            self.ty(ty);
        }
        self.super_place(place, context, location);
    }

    fn visit_local(&mut self, local: &Local, _context: PlaceContext, _location: Location) {
        if let Some(decl) = self.body.locals().get(*local) {
            self.ty(decl.ty);
        }
    }

    fn visit_rvalue(&mut self, rvalue: &rustc_public::mir::Rvalue, location: Location) {
        if let rustc_public::mir::Rvalue::Aggregate(rustc_public::mir::AggregateKind::Adt(def, variant, ..), _) = rvalue {
            let def = rustc_public::rustc_internal::internal(self.tcx, *def);
            if let Some(found) = def.variants().get(rustc_abi::VariantIdx::from_usize(variant.to_index())) {
                self.note(found.def_id);
                for field in &found.fields {
                    self.note(field.did);
                }
            }
        }
        if let rustc_public::mir::Rvalue::ThreadLocalRef(item) = rvalue {
            self.note(rustc_public::rustc_internal::internal(self.tcx, item.def_id()));
        }
        self.super_rvalue(rvalue, location);
    }

    fn visit_mir_const(&mut self, constant: &MirConst, location: Location) {
        match constant.kind() {
            ConstantKind::Unevaluated(unevaluated) => self.note(rustc_public::rustc_internal::internal(self.tcx, unevaluated.def.def_id())),
            ConstantKind::Ty(constant) => {
                if let rustc_public::ty::TyConstKind::Unevaluated(def, _) = constant.kind() {
                    self.note(rustc_public::rustc_internal::internal(self.tcx, def.def_id()));
                }
            }
            ConstantKind::Allocated(alloc) => {
                for (_, provenance) in &alloc.provenance.ptrs {
                    match rustc_public::mir::alloc::GlobalAlloc::from(provenance.0) {
                        rustc_public::mir::alloc::GlobalAlloc::Static(def) => self.note(rustc_public::rustc_internal::internal(self.tcx, def.def_id())),
                        rustc_public::mir::alloc::GlobalAlloc::Function(instance) => self.note(rustc_public::rustc_internal::internal(self.tcx, instance.def.def_id())),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        self.super_mir_const(constant, location);
    }

    fn visit_ty(&mut self, ty: &rustc_public::ty::Ty, _location: Location) {
        self.ty(*ty);
    }
}

/// The paths of the source of the item of a body (with its closures and constant blocks): the
/// source range of each path and its definition. A path through a `use` item of the module of
/// the item also gives the `use` item.
fn hir_paths(tcx: TyCtxt<'_>, item: DefId, files: &mut Vec<String>) -> Vec<((i64, i64, i64), DefId)> {
    let root = tcx.typeck_root_def_id(item);
    let Some(local) = root.as_local() else { return Vec::new() };
    let mut paths = Paths { tcx, found: Vec::new(), typeck: None, uses: uses_of_module(tcx, local) };
    let node = tcx.hir_node_by_def_id(local);
    if let Some(decl) = node.fn_decl() {
        paths.visit_fn_decl(decl);
    }
    if let Some(ty) = node.ty() {
        paths.visit_ty_unambig(ty);
    }
    if let Some(body) = tcx.hir_maybe_body_owned_by(local) {
        paths.typeck = Some(tcx.typeck(local));
        paths.visit_body(body);
    }
    let mut out = Vec::new();
    for (span, def_id) in paths.found {
        if let Some(range) = crate::spans::range(tcx, span, files) {
            out.push((range, def_id));
        }
    }
    out
}

/// The `use` items of the module of a definition: the name and the definitions of each single
/// import, and the `use` item.
fn uses_of_module(tcx: TyCtxt<'_>, def: LocalDefId) -> Vec<(rustc_span::Symbol, Vec<DefId>, DefId)> {
    let module = tcx.parent_module_from_def_id(def);
    let mut out = Vec::new();
    for id in tcx.hir_module_items(module).free_items() {
        let item = tcx.hir_item(id);
        if let rustc_hir::ItemKind::Use(tree) = item.kind {
            use_tree(&tree, item.owner_id.to_def_id(), &mut out);
        }
    }
    out
}

/// The single imports of a `use` tree, with the definition of each part (a list `use` has one
/// definition for each of its parts).
fn use_tree(tree: &rustc_hir::UseTree<'_>, def: DefId, out: &mut Vec<(rustc_span::Symbol, Vec<DefId>, DefId)>) {
    match tree.kind {
        rustc_hir::UseKind::Single(ident) => {
            let defs: Vec<DefId> = tree.prefix.res.present_items().filter_map(|res| match res {
                Res::Def(_, def_id) => Some(def_id),
                _ => None,
            }).collect();
            out.push((ident.name, defs, def));
        }
        rustc_hir::UseKind::Glob => {}
        rustc_hir::UseKind::Nested { items } => {
            for (inner, _, local) in items {
                use_tree(inner, local.to_def_id(), out);
            }
        }
    }
}

/// The HIR visitor of the paths of a body.
struct Paths<'tcx> {
    tcx: TyCtxt<'tcx>,
    found: Vec<(Span, DefId)>,
    typeck: Option<&'tcx ty::TypeckResults<'tcx>>,
    uses: Vec<(rustc_span::Symbol, Vec<DefId>, DefId)>,
}

impl<'tcx> Paths<'tcx> {
    /// The impls that normalize the projections of the signature of a call, at the span of the
    /// call expression.
    fn projections(&mut self, expr: &rustc_hir::Expr<'tcx>, id: rustc_hir::HirId, def_id: DefId) {
        let Some(args) = self.typeck.and_then(|typeck| typeck.node_args_opt(id)) else { return };
        if rustc_middle::ty::TypeVisitableExt::has_param(&args) || rustc_middle::ty::TypeVisitableExt::has_infer(&args) {
            return;
        }
        for impl_id in projection_impls(self.tcx, def_id, args) {
            self.found.push((expr.span, impl_id));
        }
    }
}

impl<'tcx> HirVisitor<'tcx> for Paths<'tcx> {
    fn visit_nested_body(&mut self, id: rustc_hir::BodyId) {
        let old = self.typeck.replace(self.tcx.typeck_body(id));
        self.visit_body(self.tcx.hir_body(id));
        self.typeck = old;
    }

    fn visit_path(&mut self, path: &rustc_hir::Path<'tcx>, _id: rustc_hir::HirId) {
        if let Res::Def(kind, def_id) = path.res {
            if is_symbol_kind(kind) {
                self.found.push((path.span, def_id));
            }
            // The `use` item that the first segment comes through.
            if let Some(first) = path.segments.first() {
                let first_def = match first.res {
                    Res::Def(_, found) => Some(found),
                    _ => None,
                };
                for (name, defs, use_def) in &self.uses {
                    if *name == first.ident.name && first_def.is_some_and(|found| defs.contains(&found)) {
                        self.found.push((path.span, *use_def));
                    }
                }
            }
        }
        intravisit::walk_path(self, path);
    }

    fn visit_expr(&mut self, expr: &'tcx rustc_hir::Expr<'tcx>) {
        match expr.kind {
            // A method call and a type-relative path (`P::MAX`, `T::new`): the definition of the
            // type check.
            rustc_hir::ExprKind::MethodCall(segment, ..) => {
                if let Some(def_id) = self.typeck.and_then(|typeck| typeck.type_dependent_def_id(expr.hir_id)) {
                    self.found.push((segment.ident.span, def_id));
                    self.projections(expr, expr.hir_id, def_id);
                }
            }
            // A call: the impls that normalize the projections of its signature (the value of
            // the call has such a type).
            rustc_hir::ExprKind::Call(callee, _) => {
                if let Some(typeck) = self.typeck
                    && let rustc_hir::ExprKind::Path(qpath) = &callee.kind
                    && let Res::Def(DefKind::Fn | DefKind::AssocFn, def_id) = typeck.qpath_res(qpath, callee.hir_id)
                {
                    self.projections(expr, callee.hir_id, def_id);
                }
            }
            rustc_hir::ExprKind::Path(rustc_hir::QPath::TypeRelative(..)) => {
                if let Some(def_id) = self.typeck.and_then(|typeck| typeck.type_dependent_def_id(expr.hir_id)) {
                    self.found.push((expr.span, def_id));
                }
            }
            // The fields of a struct expression (a derive gives each field the span of the field
            // in the definition).
            rustc_hir::ExprKind::Struct(_, fields, _) => {
                if let Some(typeck) = self.typeck
                    && let Some(ty) = typeck.expr_ty_opt(expr)
                    && let ty::Adt(def, _) = ty.kind()
                    && (def.is_struct() || def.is_union())
                {
                    for field in fields {
                        if let Some(index) = typeck.opt_field_index(field.hir_id)
                            && let Some(found) = def.non_enum_variant().fields.get(index)
                        {
                            self.found.push((field.span, found.did));
                        }
                    }
                }
            }
            // A field: the field of the type of the base.
            rustc_hir::ExprKind::Field(base, ident) => {
                if let Some(typeck) = self.typeck
                    && let Some(ty) = typeck.expr_ty_adjusted_opt(base)
                    && let ty::Adt(def, _) = ty.peel_refs().kind()
                    && let Some(index) = typeck.opt_field_index(expr.hir_id)
                {
                    let variant = def.non_enum_variant();
                    if def.is_struct() || def.is_union() {
                        if let Some(field) = variant.fields.get(index) {
                            self.found.push((ident.span, field.did));
                        }
                    }
                }
            }
            _ => {}
        }
        intravisit::walk_expr(self, expr);
    }
}

/// True for a definition that a symbol can name.
fn is_symbol_kind(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::TyAlias
            | DefKind::Const { .. }
            | DefKind::AssocConst { .. }
            | DefKind::Static { .. }
            | DefKind::Fn
            | DefKind::AssocFn
            | DefKind::Struct
            | DefKind::Enum
            | DefKind::Union
            | DefKind::Variant
            | DefKind::Ctor(..)
            | DefKind::Trait
            | DefKind::AssocTy
            | DefKind::ForeignTy
            | DefKind::TraitAlias
    )
}

/// The definitions of the local crate and the symbols that each one names (design 44, part 1.2).
#[derive(serde::Serialize, Default)]
pub struct Defs {
    /// The files of the places.
    pub f: Vec<String>,
    /// The symbol table.
    pub s: Vec<Symbol>,
    /// Each definition: the index of its symbol, then pairs of a named symbol and its flags.
    pub d: Vec<Vec<u32>>,
}

/// The definitions of the local crate (design 44, part 1.2).
pub fn defs(tcx: TyCtxt<'_>) -> Defs {
    let mut out = Defs::default();
    let mut table = Table::default();
    for local in tcx.hir_crate_items(()).definitions() {
        let kind = tcx.def_kind(local);
        let wanted = matches!(
            kind,
            DefKind::Const { .. } | DefKind::Static { .. } | DefKind::Fn | DefKind::AssocFn | DefKind::AssocConst { .. } | DefKind::Struct | DefKind::Enum | DefKind::Union | DefKind::TyAlias | DefKind::Trait | DefKind::Impl { .. } | DefKind::AssocTy
        );
        if !wanted {
            continue;
        }
        let Some(own) = table.add(tcx, local.to_def_id(), &mut out.f) else { continue };
        let mut paths = DefPaths { tcx, found: Vec::new(), flag: 0, typeck: None };
        let node = tcx.hir_node_by_def_id(local);
        match node {
            rustc_hir::Node::Item(item) => {
                // The self type of an `impl`.
                if let rustc_hir::ItemKind::Impl(imp) = &item.kind {
                    paths.flag = SELF_TYPE;
                    paths.visit_ty_unambig(imp.self_ty);
                    paths.flag = 0;
                }
                intravisit::walk_item(&mut paths, item);
            }
            rustc_hir::Node::ImplItem(item) => intravisit::walk_impl_item(&mut paths, item),
            rustc_hir::Node::TraitItem(item) => intravisit::walk_trait_item(&mut paths, item),
            _ => {}
        }
        // The value of a constant or a static: rustc evaluates it, with each `const fn` that
        // it calls. The body of a `const fn` counts only through such a value.
        let constant = matches!(kind, DefKind::Const { .. } | DefKind::Static { .. } | DefKind::AssocConst { .. });
        let const_fn = matches!(kind, DefKind::Fn | DefKind::AssocFn) && tcx.is_const_fn(local.to_def_id());
        if (constant || const_fn)
            && let Some(body) = tcx.hir_maybe_body_owned_by(local)
        {
            paths.flag = if constant { EVALUATED } else { BODY };
            // The type check resolves a type-relative path (`Self::make`) and a method call.
            paths.typeck = Some(tcx.typeck(local));
            paths.visit_body(body);
        }
        let mut entry = vec![own];
        let mut seen = BTreeSet::new();
        for (def_id, flag) in std::mem::take(&mut paths.found) {
            if def_id == local.to_def_id() {
                continue;
            }
            if let Some(index) = table.add(tcx, def_id, &mut out.f)
                && seen.insert((index, flag))
            {
                entry.push(index);
                entry.push(u32::from(flag));
            }
        }
        if entry.len() > 1 {
            out.d.push(entry);
        }
    }
    // The `use` items: the name and the targets of each single import (rule 5 of `links.rs`).
    for id in tcx.hir_crate_items(()).free_items() {
        let item = tcx.hir_item(id);
        let rustc_hir::ItemKind::Use(tree) = item.kind else { continue };
        let mut imports = Vec::new();
        use_tree(&tree, item.owner_id.to_def_id(), &mut imports);
        for (name, targets, use_def) in imports {
            let Some(own) = table.add(tcx, use_def, &mut out.f) else { continue };
            let mut entry = vec![own, table.name(name.as_str()), u32::from(USE_NAME)];
            for target in targets {
                // A target outside the workspace has its def path and no place.
                let index = table.add(tcx, target, &mut out.f).unwrap_or_else(|| table.name(&path(tcx, target)));
                entry.push(index);
                entry.push(u32::from(USE_TARGET));
            }
            out.d.push(entry);
        }
    }
    out.s = table.symbols;
    out
}

/// The HIR visitor of the paths of a definition. It does not enter a nested body: the caller
/// visits the value of a constant and the body of a `const fn`.
struct DefPaths<'tcx> {
    tcx: TyCtxt<'tcx>,
    found: Vec<(DefId, u8)>,
    flag: u8,
    typeck: Option<&'tcx ty::TypeckResults<'tcx>>,
}

impl<'tcx> HirVisitor<'tcx> for DefPaths<'tcx> {
    fn visit_path(&mut self, path: &rustc_hir::Path<'tcx>, _id: rustc_hir::HirId) {
        if let Res::Def(kind, def_id) = path.res
            && is_symbol_kind(kind)
        {
            self.found.push((def_id, self.flag));
        }
        intravisit::walk_path(self, path);
    }

    fn visit_nested_body(&mut self, id: rustc_hir::BodyId) {
        // Only a constant block in a type (an array length) is a part of a definition here.
        let _ = (id, self.tcx);
    }

    fn visit_expr(&mut self, expr: &'tcx rustc_hir::Expr<'tcx>) {
        // A type-relative path (`Self::make`, `Self::FIRST`) and a method call in a value or in
        // the body of a `const fn`: the definition of the type check (fix round 2, R1).
        if matches!(expr.kind, rustc_hir::ExprKind::MethodCall(..) | rustc_hir::ExprKind::Path(rustc_hir::QPath::TypeRelative(..)))
            && let Some(def_id) = self.typeck.and_then(|typeck| typeck.type_dependent_def_id(expr.hir_id))
        {
            self.found.push((def_id, self.flag));
        }
        intravisit::walk_expr(self, expr);
    }
}
