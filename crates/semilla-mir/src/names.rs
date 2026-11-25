//! The name record of a crate (batch 11b, design 70, R70.1-1).
//!
//! The driver writes this record at the levels `ex` and `n0`, in the unit record. Each fact and
//! each spotlight of Semilla that needs a name, an identity, a visibility, a `cfg` state, or an
//! `unsafe` state reads it here: the compiler gives each value, not the text. The record has:
//!
//! - `d`: each definition of the crate (and each definition of another crate that the record
//!   names): the def path, the name, the kind, the place of the definition and of its name, the
//!   effective visibility, the `unsafe` state, the `cfg` state, for an impl its trait and its self
//!   type, for an item of a trait impl the item of the trait that it gives.
//! - `p`: each name of the source that the compiler resolves: the place of the name and the
//!   definition (`-1`: a local binding). A name in a `use` line resolves too.
//! - `u`: each `unsafe` block that the author wrote.
//! - `c`: each `cfg` and `cfg_attr` attribute that the compiler evaluated: its place, its value,
//!   and the text of its condition. A `cfg` that is false removes its item: the place of the
//!   condition, and the name of the item (`s`).
//!
//! A definition under a `cfg` that the configuration turns off is not in the record.

use std::collections::HashMap;

use rustc_attr_ir::{Attribute, AttributeKind};
use rustc_hir::def::{DefKind, Res};
use rustc_hir::intravisit::{self, Visitor as HirVisitor};
use rustc_middle::ty::{self, TyCtxt};
use rustc_span::Span;
use rustc_span::def_id::{DefId, LOCAL_CRATE, LocalDefId};

/// One definition.
#[derive(serde::Serialize, Default)]
pub struct Def {
    /// The def path, with the crate name first (the real path, not a re-export).
    pub p: String,
    /// The name, or empty (an impl).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub n: String,
    /// The kind (`struct`, `fn`, `impl`, `field`, …).
    pub k: String,
    /// The place of the definition: the file, the start, and the end. Only a definition of
    /// this crate in a file of the workspace has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s: Option<[i64; 3]>,
    /// The place of the name of the definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub i: Option<[i64; 3]>,
    /// The effective visibility (R70.2-1 (b)): `e` a different crate can name it, `r` a different
    /// crate can reach it through a public signature, `i` only through an `impl Trait` type.
    /// Empty: no different crate can use it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub v: String,
    /// True for an `unsafe fn`, an `unsafe impl`, and an `unsafe trait`.
    #[serde(skip_serializing_if = "is_false")]
    pub u: bool,
    /// True if a `cfg` attribute is on the definition or on a parent.
    #[serde(skip_serializing_if = "is_false")]
    pub c: bool,
    /// An impl of a trait: the trait reference with the self type (`<X as core::ops::Add>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
    /// An impl of a trait: the index of the trait. An item of a trait impl: the index of the
    /// item of the trait.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<usize>,
    /// An item of a trait impl: true if the item of the trait has a default. An item of a
    /// trait: true if it has a default.
    #[serde(skip_serializing_if = "is_false")]
    pub o: bool,
    /// The last name of each attribute that the compiler keeps on the definition with no
    /// meaning of its own (a derive helper attribute, a tool attribute): no macro call
    /// (R70.2-1 (f)). An attribute macro is not on the definition after the expansion.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub h: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A `cfg` or a `cfg_attr` attribute that the compiler evaluated.
#[derive(serde::Serialize)]
pub struct Cfg {
    /// The place of the attribute (a false `cfg`: the place of its condition).
    pub r: [i64; 3],
    /// The value of the condition in this configuration.
    pub v: bool,
    /// `cfg` or `cfg_attr`.
    pub k: &'static str,
    /// The text of the condition.
    pub t: String,
    /// A false `cfg`: the name of the item that it removes.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub s: String,
}

/// The name record of a crate.
#[derive(serde::Serialize, Default)]
pub struct Names {
    pub f: Vec<String>,
    pub d: Vec<Def>,
    /// The file, the start, the end, and the index of the definition (`-1`: a local binding).
    pub p: Vec<[i64; 4]>,
    pub u: Vec<[i64; 3]>,
    pub c: Vec<Cfg>,
}

struct Table<'tcx> {
    tcx: TyCtxt<'tcx>,
    names: Names,
    index: HashMap<DefId, usize>,
}

impl<'tcx> Table<'tcx> {
    fn range(&mut self, span: Span) -> Option<[i64; 3]> {
        crate::spans::range(self.tcx, span, &mut self.names.f).map(|(file, start, end)| [file, start, end])
    }

    /// The index of a definition. A definition of another crate has its path and its kind.
    fn def(&mut self, def_id: DefId) -> usize {
        if let Some(found) = self.index.get(&def_id) {
            return *found;
        }
        let tcx = self.tcx;
        let kind = tcx.def_kind(def_id);
        let entry = Def { p: path(tcx, def_id), n: tcx.opt_item_name(def_id).map(|name| name.to_string()).unwrap_or_default(), k: kind.descr(def_id).to_string(), ..Def::default() };
        self.names.d.push(entry);
        let at = self.names.d.len() - 1;
        self.index.insert(def_id, at);
        at
    }

    fn path(&mut self, span: Span, target: i64) {
        // A name that a macro makes has no place of its own in the source of the call.
        if span.from_expansion() {
            return;
        }
        if let Some([file, start, end]) = self.range(span) {
            self.names.p.push([file, start, end, target]);
        }
    }
}

/// The def path of a definition: the real path, with the crate name first. A field is
/// `TYPE::NAME`.
fn path(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    crate::reach::real_path(tcx, def_id)
}

/// True for a definition of the record: an item, a member, an associated item. A closure, a
/// constant block, a generic parameter, and an opaque type are not.
fn wanted(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Mod
            | DefKind::Struct
            | DefKind::Enum
            | DefKind::Union
            | DefKind::Variant
            | DefKind::Field
            | DefKind::Trait
            | DefKind::TraitAlias
            | DefKind::TyAlias
            | DefKind::ForeignTy
            | DefKind::Fn
            | DefKind::AssocFn
            | DefKind::AssocConst { .. }
            | DefKind::AssocTy
            | DefKind::Const { .. }
            | DefKind::Static { .. }
            | DefKind::Impl { .. }
            | DefKind::Macro(_)
            | DefKind::ExternCrate
    )
}

/// True if a `cfg` attribute is on the definition or on a parent: the definition exists only
/// in some configurations. A `cfg_attr` does not decide if the definition exists.
fn under_cfg(tcx: TyCtxt<'_>, def: LocalDefId) -> bool {
    let mut hir_id = tcx.local_def_id_to_hir_id(def);
    loop {
        if tcx.hir_attrs(hir_id).iter().any(|item| matches!(item, Attribute::Parsed(AttributeKind::CfgTrace(..)))) {
            return true;
        }
        if hir_id == rustc_hir::CRATE_HIR_ID {
            return false;
        }
        hir_id = tcx.parent_hir_id(hir_id);
    }
}

/// The effective visibility of a definition (R70.2-1 (b)).
fn visibility(tcx: TyCtxt<'_>, def: LocalDefId) -> &'static str {
    use rustc_middle::middle::privacy::Level;
    let table = tcx.effective_visibilities(());
    if table.is_public_at_level(def, Level::Reexported) {
        "e"
    } else if table.is_public_at_level(def, Level::Reachable) {
        "r"
    } else if table.is_public_at_level(def, Level::ReachableThroughImplTrait) {
        "i"
    } else {
        ""
    }
}

/// The text of the condition of a `cfg`.
fn cfg_text(entry: &rustc_ast::attr::data_structures::CfgEntry) -> String {
    use rustc_ast::attr::data_structures::CfgEntry;
    let list = |items: &[CfgEntry]| items.iter().map(cfg_text).collect::<Vec<_>>().join(", ");
    match entry {
        CfgEntry::All(items, _) => format!("all({})", list(items)),
        CfgEntry::Any(items, _) => format!("any({})", list(items)),
        CfgEntry::Not(item, _) => format!("not({})", cfg_text(item)),
        CfgEntry::Bool(value, _) => value.to_string(),
        CfgEntry::NameValue { name, value: Some(value), .. } => format!("{name} = \"{value}\""),
        CfgEntry::NameValue { name, value: None, .. } => name.to_string(),
        CfgEntry::Version(..) => "version(…)".to_string(),
    }
}

/// The name record of the local crate (R70.1-1).
pub fn collect(tcx: TyCtxt<'_>) -> Names {
    let mut table = Table { tcx, names: Names::default(), index: HashMap::new() };
    // The definitions of the crate, with the variants and the fields of each type.
    let mut locals: Vec<LocalDefId> = Vec::new();
    for local in tcx.hir_crate_items(()).definitions() {
        locals.push(local);
        if matches!(tcx.def_kind(local), DefKind::Struct | DefKind::Enum | DefKind::Union) {
            for variant in tcx.adt_def(local).variants() {
                if tcx.def_kind(variant.def_id) == DefKind::Variant {
                    locals.extend(variant.def_id.as_local());
                }
                locals.extend(variant.fields.iter().filter_map(|field| field.did.as_local()));
            }
        }
    }
    for local in locals {
        let kind = tcx.def_kind(local);
        if !wanted(kind) {
            continue;
        }
        let def_id = local.to_def_id();
        let at = table.def(def_id);
        let span = tcx.source_span(local);
        let s = if span.from_expansion() { None } else { table.range(span) };
        let i = tcx.def_ident_span(def_id).filter(|span| !span.from_expansion()).and_then(|span| table.range(span));
        let unsafe_def = match kind {
            DefKind::Fn | DefKind::AssocFn => tcx.fn_sig(def_id).skip_binder().safety().is_unsafe(),
            DefKind::Impl { of_trait: true } => tcx.impl_trait_header(def_id).safety.is_unsafe(),
            DefKind::Trait => tcx.trait_def(def_id).safety.is_unsafe(),
            _ => false,
        };
        let mut trait_ref = None;
        let mut of = None;
        let mut default = false;
        if let DefKind::Impl { of_trait: true } = kind {
            let header = tcx.impl_trait_ref(def_id).instantiate_identity().skip_norm_wip();
            trait_ref = Some(ty::print::with_no_visible_paths!(ty::print::with_resolve_crate_name!(ty::print::with_no_trimmed_paths!(format!("{header}")))));
            of = Some(table.def(header.def_id));
        }
        if matches!(kind, DefKind::AssocFn | DefKind::AssocConst { .. } | DefKind::AssocTy) {
            let item = tcx.associated_item(def_id);
            if let Some(trait_item) = item.trait_item_def_id() {
                of = Some(table.def(trait_item));
                default = tcx.defaultness(trait_item).has_value();
            } else if item.trait_container(tcx).is_some() {
                // An item of a trait: true if it has a default.
                default = tcx.defaultness(def_id).has_value();
            }
        }
        let mut inert: Vec<String> = tcx
            .hir_attrs(tcx.local_def_id_to_hir_id(local))
            .iter()
            .filter_map(|attr| match attr {
                Attribute::Unparsed(item) => item.path.segments.last().map(|name| name.to_string()),
                _ => None,
            })
            .collect();
        inert.sort();
        inert.dedup();
        let entry = &mut table.names.d[at];
        entry.h = inert;
        entry.s = s;
        entry.i = i;
        entry.v = visibility(tcx, local).to_string();
        entry.u = unsafe_def;
        entry.c = under_cfg(tcx, local);
        entry.t = trait_ref;
        entry.r = of;
        entry.o = default;
    }
    // The names of the source, the `unsafe` blocks, the bodies.
    let mut visitor = Visit { table: &mut table, typeck: None };
    tcx.hir_visit_all_item_likes_in_crate(&mut visitor);
    // The `cfg` and `cfg_attr` attributes that the compiler evaluated.
    for owner in tcx.hir_crate_items(()).owners() {
        for (_, attrs) in tcx.hir_attr_map(owner).map.iter() {
            for attr in attrs.iter() {
                let (list, kind) = match attr {
                    Attribute::Parsed(AttributeKind::CfgTrace(list)) => (list, "cfg"),
                    Attribute::Parsed(AttributeKind::CfgAttrTrace(list)) => (list, "cfg_attr"),
                    _ => continue,
                };
                for (entry, span) in list.iter() {
                    if let Some(r) = table.range(*span) {
                        // A `cfg_attr` has a trace with either value: the compiler evaluates
                        // its condition again for the value.
                        let value = kind == "cfg" || cfg_value(tcx, entry);
                        table.names.c.push(Cfg { r, v: value, k: kind, t: cfg_text(entry), s: String::new() });
                    }
                }
            }
        }
    }
    for item in tcx.stripped_cfg_items(LOCAL_CRATE) {
        let (entry, span) = &item.cfg;
        if let Some(r) = table.range(*span) {
            table.names.c.push(Cfg { r, v: false, k: "cfg", t: cfg_text(entry), s: item.ident.name.to_string() });
        }
    }
    table.names.c.sort_by(|a, b| (a.r, &a.t, &a.s, a.k).cmp(&(b.r, &b.t, &b.s, b.k)));
    table.names.c.dedup_by(|a, b| a.r == b.r && a.t == b.t && a.s == b.s && a.k == b.k);
    table.names.p.sort();
    table.names.p.dedup();
    table.names.u.sort();
    table.names.u.dedup();
    table.names
}

/// The value of the condition of a `cfg_attr` in this configuration.
fn cfg_value(tcx: TyCtxt<'_>, entry: &rustc_ast::attr::data_structures::CfgEntry) -> bool {
    use rustc_ast::attr::data_structures::CfgEntry;
    match entry {
        CfgEntry::All(items, _) => items.iter().all(|item| cfg_value(tcx, item)),
        CfgEntry::Any(items, _) => items.iter().any(|item| cfg_value(tcx, item)),
        CfgEntry::Not(item, _) => !cfg_value(tcx, item),
        CfgEntry::Bool(value, _) => *value,
        CfgEntry::NameValue { name, value, .. } => tcx.sess.config.contains(&(*name, *value)),
        // The compiler version: the record keeps the safe side (the attribute can apply).
        CfgEntry::Version(..) => true,
    }
}

/// The HIR visitor of the names, the `unsafe` blocks, and the local bindings.
struct Visit<'a, 'tcx> {
    table: &'a mut Table<'tcx>,
    typeck: Option<&'tcx ty::TypeckResults<'tcx>>,
}

impl<'tcx> HirVisitor<'tcx> for Visit<'_, 'tcx> {
    type NestedFilter = rustc_middle::hir::nested_filter::All;

    fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
        self.table.tcx
    }

    fn visit_nested_body(&mut self, id: rustc_hir::BodyId) {
        let tcx = self.table.tcx;
        let old = self.typeck.replace(tcx.typeck_body(id));
        self.visit_body(tcx.hir_body(id));
        self.typeck = old;
    }

    fn visit_path(&mut self, path: &rustc_hir::Path<'tcx>, _id: rustc_hir::HirId) {
        for segment in path.segments {
            match segment.res {
                Res::Def(kind, def_id) if wanted(kind) || matches!(kind, DefKind::Ctor(..)) => {
                    // A constructor names its struct or its variant.
                    let target = if matches!(kind, DefKind::Ctor(..)) { self.table.tcx.opt_parent(def_id).unwrap_or(def_id) } else { def_id };
                    let index = self.table.def(target) as i64;
                    self.table.path(segment.ident.span, index);
                }
                Res::Local(_) => self.table.path(segment.ident.span, -1),
                _ => {}
            }
        }
        // The last segment of a path of a `use` line has the resolution of the path (the HIR
        // visits the path of a `use` line once for each namespace).
        if path.segments.last().is_some_and(|last| matches!(last.res, Res::Err)) {
            if let Res::Def(kind, def_id) = path.res
                && let Some(last) = path.segments.last()
                && wanted(kind)
            {
                let index = self.table.def(def_id) as i64;
                self.table.path(last.ident.span, index);
            }
        }
        intravisit::walk_path(self, path);
    }

    fn visit_pat(&mut self, pat: &'tcx rustc_hir::Pat<'tcx>) {
        if let rustc_hir::PatKind::Binding(_, _, ident, _) = pat.kind {
            self.table.path(ident.span, -1);
        }
        intravisit::walk_pat(self, pat);
    }

    fn visit_block(&mut self, block: &'tcx rustc_hir::Block<'tcx>) {
        if let rustc_hir::BlockCheckMode::UnsafeBlock(rustc_hir::UnsafeSource::UserProvided) = block.rules
            && !block.span.from_expansion()
            && let Some(range) = self.table.range(block.span)
        {
            self.table.names.u.push(range);
        }
        intravisit::walk_block(self, block);
    }

    fn visit_expr(&mut self, expr: &'tcx rustc_hir::Expr<'tcx>) {
        let found = match expr.kind {
            // A method call and a type-relative path (`T::new`): the definition of the type check.
            rustc_hir::ExprKind::MethodCall(segment, ..) => self.typeck.and_then(|typeck| typeck.type_dependent_def_id(expr.hir_id)).map(|def_id| (segment.ident.span, def_id)),
            rustc_hir::ExprKind::Path(rustc_hir::QPath::TypeRelative(_, segment)) => self.typeck.and_then(|typeck| typeck.type_dependent_def_id(expr.hir_id)).map(|def_id| (segment.ident.span, def_id)),
            // A field: the field of the type of the base.
            rustc_hir::ExprKind::Field(base, ident) => self
                .typeck
                .and_then(|typeck| {
                    let ty = typeck.expr_ty_adjusted_opt(base)?;
                    let ty::Adt(def, _) = ty.peel_refs().kind() else { return None };
                    let index = typeck.opt_field_index(expr.hir_id)?;
                    (def.is_struct() || def.is_union()).then(|| def.non_enum_variant().fields.get(index).map(|field| field.did)).flatten()
                })
                .map(|did| (ident.span, did)),
            _ => None,
        };
        if let Some((span, def_id)) = found
            && wanted(self.table.tcx.def_kind(def_id))
        {
            let index = self.table.def(def_id) as i64;
            self.table.path(span, index);
        }
        intravisit::walk_expr(self, expr);
    }
}
