//! The build configuration rule (design 8.2, batch 5k).
//!
//! A check build is one build configuration: one target, one profile, one feature set, one
//! environment. A proof must hold in each build configuration. So no step of a proof can use a
//! value that the build configuration decides.
//!
//! A value is **configuration-free** only if it is equal in each build configuration. The allow
//! list (`source_of`, `const_is_free`, `body_is_free`):
//!
//! - a literal of the source text. Each macro of its expansion must be a `macro_rules!` macro, a
//!   built-in derive, or a built-in macro of the allow list `PURE_BUILTIN`;
//! - a named constant of the local crate whose initializer has only configuration-free values
//!   and operations with a fixed width, with no `#[cfg]` and no `#[cfg_attr]` on the item or on
//!   a parent;
//! - the associated constants `MIN`, `MAX`, and `BITS` of the integer types with a fixed width.
//!
//! Each other constant is opaque. Before the optimizer of rustc runs, `make_opaque` puts each
//! such constant through a call of the intrinsic `black_box`. The optimizer cannot see through
//! this call, so it cannot fold a condition or remove a branch with the value. The printer
//! adds the source of the value to its text (`@cfg!(debug_assertions)`), so that two different
//! sources are never equal. The optimizer removes the span and the name of a constant operand,
//! so the span of the call gives the source (`Mark`, `span_source`).
//!
//! A constant in a pattern (`match x { LIMIT => .. }`) has no operand in the MIR: its value is in
//! the list of a switch. `hir_sources` finds each pattern constant that is not
//! configuration-free. A body with such a pattern gets a line `config pattern: ..` in its text,
//! gets no term, and each switch on a value that the optimizer can know becomes opaque.
//!
//! The same rule applies to an operation that the target decides: an operation with a `usize`
//! or `isize` result on values that can be constants, a direct call of an intrinsic
//! (`size_of`, `transmute`), the length of a slice, and an unsize coercion of an array (the
//! length of the array type becomes a value).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use rustc_attr_ir::{Attribute, AttributeKind};
use rustc_data_structures::steal::Steal;
use rustc_hir::def::DefKind;
use rustc_middle::mir::visit::{MutVisitor, Visitor};
use rustc_middle::mir::{
    self, BasicBlock, BasicBlockData, Body, CallSource, Const, Local, LocalDecl, Location, Operand, Place, Rvalue,
    SourceInfo, StatementKind, Terminator, TerminatorKind, UnwindAction,
};
use rustc_middle::ty::{self, Ty, TyCtxt};
use rustc_span::def_id::{DefId, LocalDefId};
use rustc_span::hygiene::{ExpnKind, MacroKind};
use rustc_span::{DUMMY_SP, Span};

/// The built-in function-like macros that do not read the build configuration. Each other
/// built-in macro (`cfg!`, `env!`, `option_env!`, `file!`, `line!`, `column!`, `module_path!`,
/// `include_str!`, `compile_error!`, a new one) gives a value that is not configuration-free.
/// The names are the names of the compiler (`#[rustc_builtin_macro]`).
const PURE_BUILTIN: [&str; 12] = [
    "assert",
    "concat",
    "concat_bytes",
    "stringify",
    "format_args",
    "format_args_nl",
    "const_format_args",
    "core_panic",
    "std_panic",
    "unreachable",
    "panic",
    "pattern_type",
];

/// The macros of the standard library that use `cfg!(debug_assertions)` and say so in their
/// name. A use of the build configuration through them gets no spotlight.
const DEBUG_ASSERT: [&str; 3] = ["debug_assert", "debug_assert_eq", "debug_assert_ne"];

/// The built-in macros whose value is not in the source text of the use, and that do not read
/// the build configuration: the content of a file of the tree, and a source position. The
/// value is opaque (a proof does not use it), and the use gets no spotlight.
const NOT_CONFIG: [&str; 7] = ["include_str", "include_bytes", "include", "line", "column", "file", "module_path"];

/// Where the value of a constant comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Configuration-free.
    Free,
    /// A built-in macro that is not on the allow list, or a proc macro. `identity` is the
    /// text of the macro call. `quiet` is true for a use that gets no "build configuration"
    /// spotlight: through `debug_assert!`, a macro of `NOT_CONFIG`, and a proc macro (the code
    /// of a proc macro is build code, with its own rules).
    Macro { identity: String, quiet: bool },
    /// A named constant that is not configuration-free: its def path.
    Named(String),
    /// A static that is not configuration-free (batch 5l): its def path, and the text of its
    /// initializer (empty for a static of another crate).
    Static { path: String, text: String },
    /// A constant of the type system (the length of an array type).
    TypeLevel,
}

impl Source {
    /// The line of the body record for a use that gets the "build configuration" spotlight.
    pub fn config_line(&self) -> Option<String> {
        match self {
            Source::Macro { identity, quiet: false } => Some(identity.clone()),
            Source::Static { path, text } => {
                let name = path.rsplit("::").next().unwrap_or(path);
                Some(if text.is_empty() { format!("static `{name}`") } else { format!("static `{name}` (`{text}`)") })
            }
            _ => None,
        }
    }

    /// The text that the printer adds to the constant.
    pub fn suffix(&self) -> Option<String> {
        match self {
            Source::Free => None,
            Source::Macro { identity, .. } => Some(format!(" @{identity}")),
            Source::Named(path) => Some(format!(" @{path}")),
            Source::Static { path, .. } => Some(format!(" @static {path}")),
            Source::TypeLevel => Some(" @type-level".to_string()),
        }
    }
}

static DEFAULT_MIR: OnceLock<for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Steal<Body<'tcx>>> = OnceLock::new();

/// Puts the pass `make_opaque` between the borrow check and the optimizer of rustc.
pub fn install(config: &mut rustc_interface::interface::Config) {
    config.override_queries = Some(|_session, providers| {
        let _ = DEFAULT_MIR.set(providers.queries.mir_drops_elaborated_and_const_checked);
        providers.queries.mir_drops_elaborated_and_const_checked = opaque_mir;
    });
}

fn opaque_mir<'tcx>(tcx: TyCtxt<'tcx>, def: LocalDefId) -> &'tcx Steal<Body<'tcx>> {
    let default = DEFAULT_MIR.get().expect("the default provider is stored");
    let steal = default(tcx, def);
    // The body of a constant or a static runs only at compile time. Its value is the input of
    // `const_is_free`, so it stays as it is.
    if !matches!(
        tcx.def_kind(def),
        DefKind::Fn | DefKind::AssocFn | DefKind::Closure | DefKind::SyntheticCoroutineBody
    ) {
        return steal;
    }
    // The `main` function of the test harness has no code of the user: it gives the list of
    // the tests to the test runner. The driver reads its form (`harness.rs`).
    if tcx.sess.is_test_crate() && matches!(tcx.def_span(def).ctxt().outer_expn_data().kind, ExpnKind::AstPass(rustc_span::hygiene::AstPass::TestHarness)) {
        return steal;
    }
    let mut body = steal.steal();
    let _reader = Reader::set(Some(def));
    make_opaque(tcx, def, &mut body);
    tcx.alloc_steal_mir(body)
}

/// True for the intrinsic `core::intrinsics::black_box`.
pub fn is_black_box(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    tcx.intrinsic(def_id).is_some_and(|intrinsic| intrinsic.name.as_str() == "black_box")
}

fn black_box_def(tcx: TyCtxt<'_>) -> Option<DefId> {
    thread_local! {
        static FOUND: RefCell<Option<Option<DefId>>> = const { RefCell::new(None) };
    }
    if let Some(found) = FOUND.with(|found| *found.borrow()) {
        return found;
    }
    let found = (|| {
        let core = tcx.crates(()).iter().copied().find(|krate| tcx.crate_name(*krate).as_str() == "core")?;
        let root = DefId { krate: core, index: rustc_span::def_id::CRATE_DEF_INDEX };
        let child = |module: DefId, name: &str| {
            tcx.module_children(module)
                .iter()
                .find(|child| child.ident.as_str() == name)
                .and_then(|child| child.res.opt_def_id())
        };
        let intrinsics = child(root, "intrinsics")?;
        let def = child(intrinsics, "black_box")?;
        is_black_box(tcx, def).then_some(def)
    })();
    FOUND.with(|slot| *slot.borrow_mut() = Some(found));
    found
}

/// The built-in name of a macro (`#[rustc_builtin_macro]`), if it is a built-in macro.
fn builtin_name(tcx: TyCtxt<'_>, def_id: DefId, name: rustc_span::Symbol) -> Option<String> {
    #[allow(deprecated)]
    for attr in tcx.get_all_attrs(def_id) {
        if let Attribute::Parsed(AttributeKind::RustcBuiltinMacro { builtin_name, .. }) = attr {
            // `#[rustc_builtin_macro(core_panic)]` gives the name in the attribute.
            // Without it, the name of the item: the name of a use can be a path
            // (`$crate::format_args_nl`).
            let _ = name;
            return Some(builtin_name.unwrap_or_else(|| tcx.item_name(def_id)).to_string());
        }
    }
    None
}

/// The first macro of the expansion of `span` that is not on the allow list.
fn macro_source(tcx: TyCtxt<'_>, span: Span) -> Option<Source> {
    let mut span = span;
    let mut found: Option<String> = None;
    let mut quiet = false;
    for _ in 0..64 {
        let ctxt = span.ctxt();
        if ctxt.is_root() {
            break;
        }
        let data = ctxt.outer_expn_data();
        match data.kind {
            ExpnKind::Root | ExpnKind::AstPass(_) | ExpnKind::Desugaring(_) => {}
            ExpnKind::Macro(kind, name) => {
                let mut no_spotlight = false;
                let free = match data.macro_def_id {
                    // A built-in derive makes its literals from the names of the item.
                    Some(def_id) => match builtin_name(tcx, def_id, name) {
                        Some(builtin) => {
                            no_spotlight = NOT_CONFIG.contains(&builtin.as_str());
                            kind != MacroKind::Bang || PURE_BUILTIN.contains(&builtin.as_str())
                        }
                        // A proc macro is code that runs in the build: its output is not source
                        // text. A `macro_rules!` macro only moves source text.
                        None => {
                            no_spotlight = true;
                            !is_proc_macro(tcx, def_id)
                        }
                    },
                    None => false,
                };
                if let Some(def_id) = data.macro_def_id {
                    if found.is_some() && tcx.crate_name(def_id.krate).as_str() == "core" && DEBUG_ASSERT.contains(&tcx.item_name(def_id).as_str()) {
                        quiet = true;
                    }
                }
                if !free && found.is_none() {
                    found = Some(call_text(tcx, data.call_site, name));
                    quiet = no_spotlight;
                }
            }
            #[allow(unreachable_patterns)]
            _ => {
                if found.is_none() {
                    found = Some("an unknown expansion".to_string());
                }
            }
        }
        span = data.call_site;
    }
    found.map(|identity| Source::Macro { identity, quiet })
}

fn is_proc_macro(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    // A crate cannot use its own proc macros: a local macro is a `macro_rules!` macro.
    if def_id.is_local() {
        return false;
    }
    tcx.crate_dep_kind(def_id.krate).macros_only()
}

/// The text of a macro call, on one line. Without the source text: the name of the macro.
fn call_text(tcx: TyCtxt<'_>, call_site: Span, name: rustc_span::Symbol) -> String {
    match tcx.sess.source_map().span_to_snippet(call_site) {
        Ok(text) => {
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let text = text.strip_prefix("$crate::").unwrap_or(&text).to_string();
            if text.chars().count() > 120 { format!("{}…", text.chars().take(120).collect::<String>()) } else { text }
        }
        Err(_) => format!("{name}!"),
    }
}

thread_local! {
    /// The source of the constant at a span, from the HIR (`hir_sources`): a path to a named
    /// constant that is not configuration-free, and a pattern constant.
    static NOTED: RefCell<HashMap<Span, Source>> = RefCell::new(HashMap::new());
    /// The pattern constants of each body that are not configuration-free.
    static PATTERNS: RefCell<HashMap<LocalDefId, std::rc::Rc<Vec<(Span, String)>>>> = RefCell::new(HashMap::new());
}

/// Reads the HIR of a body. It keeps the source of each path to a named constant that is not
/// configuration-free (`NOTED`). It returns the pattern constants that are not
/// configuration-free: the span and the text.
fn hir_sources(tcx: TyCtxt<'_>, def: LocalDefId) -> std::rc::Rc<Vec<(Span, String)>> {
    use rustc_hir::def::Res;
    use rustc_hir::intravisit::{self, Visitor as HirVisitor};
    use rustc_hir::{Expr, ExprKind, PatExpr, PatExprKind};
    if let Some(found) = PATTERNS.with(|patterns| patterns.borrow().get(&def).cloned()) {
        return found;
    }
    let _reader = Reader::set(Some(def));
    struct Sources<'tcx> {
        tcx: TyCtxt<'tcx>,
        typeck: &'tcx ty::TypeckResults<'tcx>,
        patterns: Vec<(Span, String)>,
        paths: Vec<(Span, Source)>,
    }
    impl<'tcx> Sources<'tcx> {
        /// The source of a path to a named constant or to a static that is not
        /// configuration-free.
        fn named(&self, path: &rustc_hir::QPath<'tcx>, hir_id: rustc_hir::HirId) -> Option<Source> {
            match self.typeck.qpath_res(path, hir_id) {
                Res::Def(DefKind::Const { .. } | DefKind::AssocConst { .. }, def_id) => {
                    (!def_is_free(self.tcx, def_id, !self.typeck.node_args(hir_id).is_empty())).then(|| Source::Named(def_path(self.tcx, def_id)))
                }
                Res::Def(DefKind::Static { .. }, def_id) => (!def_is_free(self.tcx, def_id, false)).then(|| static_source(self.tcx, def_id)),
                _ => None,
            }
        }
    }
    impl<'tcx> HirVisitor<'tcx> for Sources<'tcx> {
        fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
            if let ExprKind::Path(path) = &expr.kind {
                if let Some(source) = self.named(path, expr.hir_id) {
                    self.paths.push((expr.span, source));
                }
            }
            intravisit::walk_expr(self, expr);
        }

        fn visit_pat_expr(&mut self, expr: &'tcx PatExpr<'tcx>) {
            let free = macro_source(self.tcx, expr.span).is_none()
                && match &expr.kind {
                    PatExprKind::Lit { .. } => true,
                    PatExprKind::Path(path) => match self.typeck.qpath_res(path, expr.hir_id) {
                        Res::Def(DefKind::Const { .. } | DefKind::AssocConst { .. }, _) => self.named(path, expr.hir_id).is_none(),
                        // A unit struct or a unit variant.
                        Res::Def(DefKind::Ctor(..) | DefKind::Struct | DefKind::Variant, _) | Res::SelfCtor(_) => true,
                        _ => false,
                    },
                    #[allow(unreachable_patterns)]
                    _ => false,
                };
            if !free {
                let text = self.tcx.sess.source_map().span_to_snippet(expr.span).unwrap_or_else(|_| "a pattern".to_string());
                self.patterns.push((expr.span, text.split_whitespace().collect::<Vec<_>>().join(" ")));
            }
            intravisit::walk_pat_expr(self, expr);
        }
    }
    let mut patterns = Vec::new();
    if let Some(body) = tcx.hir_maybe_body_owned_by(def) {
        if tcx.has_typeck_results(def) {
            let mut sources = Sources { tcx, typeck: tcx.typeck(def), patterns: Vec::new(), paths: Vec::new() };
            sources.visit_body(body);
            NOTED.with(|noted| {
                let mut noted = noted.borrow_mut();
                for (span, source) in sources.paths {
                    noted.insert(span, source);
                }
                for (span, text) in &sources.patterns {
                    noted.insert(*span, Source::Named(format!("pattern {text}")));
                }
            });
            patterns = sources.patterns;
        }
    }
    let patterns = std::rc::Rc::new(patterns);
    PATTERNS.with(|found| found.borrow_mut().insert(def, patterns.clone()));
    patterns
}

/// The lines `config pattern: ..` of the body at `span`, for its text.
pub fn pattern_lines(span: Span) -> Vec<String> {
    let mut lines: Vec<String> = PATTERNS.with(|patterns| {
        patterns
            .borrow()
            .values()
            .flat_map(|found| found.iter())
            .filter(|(pattern, _)| span.contains(*pattern))
            .map(|(_, text)| format!("config pattern: {text}"))
            .collect()
    });
    lines.sort();
    lines
}

/// Reads the HIR of each body of the local crate (`hir_sources`). The printer needs the
/// result. Call it before the printer runs: the pass does not run for a body that rustc takes
/// from its incremental data.
pub fn note_local_bodies(tcx: TyCtxt<'_>) {
    for def in tcx.mir_keys(()).iter().copied() {
        hir_sources(tcx, def);
    }
    write_free_constants(tcx);
}

/// The extension of the file with the configuration-free constants of a crate. The file is
/// next to the metadata file of the crate.
const FREE_FILE: &str = "semilla-free";

/// Writes the list of the configuration-free constants of the local crate (the hash of each
/// def path). The driver reads the list when it compiles a crate that uses this crate: the
/// metadata of a crate does not tell where a literal comes from. The driver writes the file
/// before rustc writes the metadata file, so a user of the crate always finds it.
fn write_free_constants(tcx: TyCtxt<'_>) {
    let rustc_session::config::OutFileName::Real(metadata) = tcx.output_filenames(()).path(rustc_session::config::OutputType::Metadata) else { return };
    let mut lines = String::new();
    // The reader is in another crate: a constant below a `cfg` attribute is not in the list.
    let _reader = Reader::set(None);
    for def in tcx.hir_body_owners() {
        if !matches!(tcx.def_kind(def), DefKind::Const { .. } | DefKind::AssocConst { .. } | DefKind::Static { .. }) {
            continue;
        }
        if tcx.generics_of(def).count() == 0 && def_is_free(tcx, def.to_def_id(), false) {
            lines.push_str(&format!("{:?}\n", tcx.def_path_hash(def.to_def_id())));
        }
    }
    // Without the file, each constant of this crate is opaque for its users.
    let _ = std::fs::write(metadata.with_extension(FREE_FILE), lines);
}

thread_local! {
    /// The configuration-free constants of each other crate. `None`: the crate has no list.
    static FOREIGN_FREE: RefCell<HashMap<rustc_span::def_id::CrateNum, Option<std::collections::HashSet<String>>>> = RefCell::new(HashMap::new());
}

/// True if the list of another crate has the constant (see `write_free_constants`). Only a
/// crate that the driver compiles has a list: a workspace crate. A crate of a registry and
/// the standard library have no list.
fn foreign_is_free(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    let known = FOREIGN_FREE.with(|lists| lists.borrow().get(&def_id.krate).is_some());
    if !known {
        let list = tcx.used_crate_source(def_id.krate).paths().find_map(|path| std::fs::read_to_string(path.with_extension(FREE_FILE)).ok());
        let list = list.map(|text| text.lines().map(str::to_string).collect());
        FOREIGN_FREE.with(|lists| lists.borrow_mut().insert(def_id.krate, list));
    }
    let hash = format!("{:?}", tcx.def_path_hash(def_id));
    FOREIGN_FREE.with(|lists| lists.borrow().get(&def_id.krate).is_some_and(|list| list.as_ref().is_some_and(|list| list.contains(&hash))))
}

/// True for a crate that the driver compiles: the local crate or a workspace crate (it has a
/// list of free constants). The source rule of the verifier reads the source of such a crate.
pub fn is_workspace_crate(tcx: TyCtxt<'_>, krate: rustc_span::def_id::CrateNum) -> bool {
    if krate == rustc_span::def_id::LOCAL_CRATE {
        return true;
    }
    // The lookup of the list is the test for a workspace crate.
    let _ = foreign_is_free(tcx, krate.as_def_id());
    FOREIGN_FREE.with(|lists| lists.borrow().get(&krate).is_some_and(|list| list.is_some()))
}

/// Each local constant whose initializer is one literal from a macro that reads the build
/// configuration: the def path, the macro call, and the span of the item. rustc keeps no MIR
/// body for such a constant, so the driver makes its record from this list.
pub fn literal_constants(tcx: TyCtxt<'_>) -> Vec<(String, String, Span)> {
    let mut found = Vec::new();
    for def in tcx.hir_body_owners() {
        if !matches!(tcx.def_kind(def), DefKind::Const { .. } | DefKind::AssocConst { .. }) {
            continue;
        }
        let Some(body) = tcx.hir_maybe_body_owned_by(def) else { continue };
        if let Some(Source::Macro { identity, .. }) = macro_source(tcx, body.value.span) {
            found.push((def_path(tcx, def.to_def_id()), identity, tcx.def_span(def)));
        }
    }
    found
}

/// Where the value of a constant operand comes from.
pub fn source_of<'tcx>(tcx: TyCtxt<'tcx>, span: Span, constant: &Const<'tcx>) -> Source {
    // A function item and a unit value have no value that a configuration can change.
    if matches!(constant.ty().kind(), ty::FnDef(..)) {
        return Source::Free;
    }
    if let Const::Val(mir::ConstValue::ZeroSized, _) = constant {
        return Source::Free;
    }
    if let Some(source) = macro_source(tcx, span) {
        return source;
    }
    if let Some(source) = NOTED.with(|noted| noted.borrow().get(&span).cloned()) {
        return source;
    }
    match constant {
        // A pointer to a static: the optimizer reads the value of an immutable static.
        Const::Val(mir::ConstValue::Scalar(mir::interpret::Scalar::Ptr(pointer, _)), _) => {
            match tcx.try_get_global_alloc(pointer.provenance.alloc_id()) {
                Some(mir::interpret::GlobalAlloc::Static(def_id)) if !def_is_free(tcx, def_id, false) => static_source(tcx, def_id),
                _ => Source::Free,
            }
        }
        Const::Val(..) => Source::Free,
        Const::Ty(_, value) => match value.kind() {
            // A const parameter has no value before the monomorphization.
            ty::ConstKind::Param(_) => Source::Free,
            _ => Source::TypeLevel,
        },
        Const::Unevaluated(unevaluated, _) => {
            if unevaluated_is_free(tcx, unevaluated) {
                Source::Free
            } else if unevaluated.promoted.is_some() {
                Source::Named("promoted".to_string())
            } else {
                Source::Named(def_path(tcx, unevaluated.def))
            }
        }
    }
}

/// The source of a static that is not configuration-free. The text is the initializer of a
/// local static, with a limit on its length.
fn static_source(tcx: TyCtxt<'_>, def_id: DefId) -> Source {
    let text = def_id
        .as_local()
        .and_then(|local| tcx.hir_maybe_body_owned_by(local))
        .and_then(|body| tcx.sess.source_map().span_to_snippet(body.value.span).ok())
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|text| text.chars().count() <= 40 && !text.contains('`'))
        .unwrap_or_default();
    Source::Static { path: def_path(tcx, def_id), text }
}

fn def_path(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    ty::print::with_no_visible_paths!(ty::print::with_resolve_crate_name!(ty::print::with_no_trimmed_paths!(
        tcx.def_path_str(def_id)
    )))
}

thread_local! {
    /// The result for each constant. `None` while the check of the constant runs.
    static FREE: RefCell<HashMap<(DefId, Option<u32>), Option<bool>>> = RefCell::new(HashMap::new());
}

fn unevaluated_is_free<'tcx>(tcx: TyCtxt<'tcx>, unevaluated: &mir::UnevaluatedConst<'tcx>) -> bool {
    let Some(promoted) = unevaluated.promoted else {
        return def_is_free(tcx, unevaluated.def, !unevaluated.args.is_empty());
    };
    let key = (unevaluated.def, Some(promoted.as_u32()));
    if let Some(known) = FREE.with(|free| free.borrow().get(&key).copied()) {
        return known.unwrap_or(false);
    }
    FREE.with(|free| free.borrow_mut().insert(key, None));
    // A promoted constant of a local body: its MIR decides.
    let result = unevaluated.def.as_local().is_some_and(|local| {
        tcx.promoted_mir(local.to_def_id()).get(promoted).is_some_and(|body| body_is_free(tcx, body))
    });
    FREE.with(|free| free.borrow_mut().insert(key, Some(result)));
    result
}

/// The type aliases of a crate outside the workspace that are equal in each build (batch 5l).
/// This is an allow list (I3): the driver cannot see the `cfg` attributes of a different crate.
/// The reason for each entry: the standard library has one definition of the alias, with no
/// `cfg`, and its target is `Result` with an error type of the standard library. The paths are
/// the definition paths of the pinned toolchain. A path that a new toolchain moves is opaque.
const FREE_ALIASES: [&str; 5] =
    ["core::io::error::Result", "core::fmt::Result", "std::thread::Result", "std::sync::poison::LockResult", "std::sync::poison::TryLockResult"];

thread_local! {
    /// The result of `aliases` for each item.
    static ALIASES: RefCell<HashMap<LocalDefId, std::rc::Rc<Vec<String>>>> = RefCell::new(HashMap::new());
}

/// The type aliases that the item `def` names (in its signature, its type, and its body) and
/// that are not configuration-free (batch 5l). The build configuration can select the target
/// of such an alias (`core::ffi::c_long`), and the MIR shows only the target. An alias of the
/// local crate or of a workspace crate is free here: the source rule of the verifier reads its
/// text. A workspace crate is a crate with a list of free constants.
pub fn aliases(tcx: TyCtxt<'_>, def: LocalDefId) -> std::rc::Rc<Vec<String>> {
    use rustc_hir::def::Res;
    use rustc_hir::intravisit::{self, Visitor as HirVisitor};
    if let Some(found) = ALIASES.with(|aliases| aliases.borrow().get(&def).cloned()) {
        return found;
    }
    struct Paths<'tcx> {
        tcx: TyCtxt<'tcx>,
        found: std::collections::BTreeSet<String>,
    }
    impl<'tcx> HirVisitor<'tcx> for Paths<'tcx> {
        fn visit_path(&mut self, path: &rustc_hir::Path<'tcx>, _id: rustc_hir::HirId) {
            if let Res::Def(DefKind::TyAlias, def_id) = path.res {
                if !def_id.is_local() {
                    let text = def_path(self.tcx, def_id);
                    // The lookup of the list is the test for a workspace crate.
                    let _ = foreign_is_free(self.tcx, def_id);
                    let workspace = FOREIGN_FREE.with(|lists| lists.borrow().get(&def_id.krate).is_some_and(|list| list.is_some()));
                    if !workspace && !FREE_ALIASES.contains(&text.as_str()) {
                        self.found.insert(text);
                    }
                }
            }
            intravisit::walk_path(self, path);
        }
    }
    let mut paths = Paths { tcx, found: Default::default() };
    let node = tcx.hir_node_by_def_id(def);
    if let Some(decl) = node.fn_decl() {
        paths.visit_fn_decl(decl);
    }
    if let Some(ty) = node.ty() {
        paths.visit_ty_unambig(ty);
    }
    if let Some(body) = tcx.hir_maybe_body_owned_by(def) {
        paths.visit_body(body);
    }
    let found = std::rc::Rc::new(paths.found.into_iter().collect::<Vec<_>>());
    ALIASES.with(|aliases| aliases.borrow_mut().insert(def, found.clone()));
    found
}

/// True if the named constant `def_id` is configuration-free. `generic` tells that the use has
/// generic arguments.
fn def_is_free(tcx: TyCtxt<'_>, def_id: DefId, generic: bool) -> bool {
    if generic {
        return false;
    }
    // The `cfg` check depends on the reader, so it is not in the memo.
    if def_id.as_local().is_some_and(|local| has_cfg(tcx, local)) {
        return false;
    }
    let key = (def_id, None);
    if let Some(known) = FREE.with(|free| free.borrow().get(&key).copied()) {
        // A constant that depends on itself is not free.
        return known.unwrap_or(false);
    }
    FREE.with(|free| free.borrow_mut().insert(key, None));
    let result = match tcx.def_kind(def_id) {
        DefKind::AssocConst { .. } if is_fixed_int_const(tcx, def_id) => true,
        DefKind::Const { .. } | DefKind::AssocConst { .. } => match def_id.as_local() {
            Some(local) => {
                // The constant is the reader of the constants in its initializer.
                let _reader = Reader::set(Some(local));
                aliases(tcx, local).is_empty() && body_is_free(tcx, tcx.mir_for_ctfe(local))
            }
            None => foreign_is_free(tcx, def_id),
        },
        // A static (batch 5l): the optimizer reads its value only if it is not `mut` and has
        // no interior mutability. Such a static is free by the rule of a constant.
        DefKind::Static { .. } => {
            let plain = !tcx.is_mutable_static(def_id)
                && !tcx.is_foreign_item(def_id)
                && !tcx.is_thread_local_static(def_id)
                && tcx.type_of(def_id).instantiate_identity().skip_norm_wip().is_freeze(tcx, ty::TypingEnv::fully_monomorphized());
            plain
                && match def_id.as_local() {
                    Some(local) => {
                        let _reader = Reader::set(Some(local));
                        aliases(tcx, local).is_empty() && body_is_free(tcx, tcx.mir_for_ctfe(local))
                    }
                    None => foreign_is_free(tcx, def_id),
                }
        }
        _ => false,
    };
    FREE.with(|free| free.borrow_mut().insert(key, Some(result)));
    result
}

/// True for `MIN`, `MAX`, and `BITS` of an integer type with a fixed width, in `core`.
fn is_fixed_int_const(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    if tcx.crate_name(def_id.krate).as_str() != "core" {
        return false;
    }
    if !matches!(tcx.item_name(def_id).as_str(), "MIN" | "MAX" | "BITS") {
        return false;
    }
    let parent = tcx.parent(def_id);
    if !matches!(tcx.def_kind(parent), DefKind::Impl { of_trait: false }) {
        return false;
    }
    is_fixed_int(tcx.type_of(parent).instantiate_identity().skip_norm_wip())
}

fn is_fixed_int(ty: Ty<'_>) -> bool {
    match ty.kind() {
        ty::Int(int) => !matches!(int, ty::IntTy::Isize),
        ty::Uint(uint) => !matches!(uint, ty::UintTy::Usize),
        _ => false,
    }
}

/// True for `usize`, `isize`, and a tuple with one of them (the result of a checked operation).
fn has_target_width(ty: Ty<'_>) -> bool {
    match ty.kind() {
        ty::Int(ty::IntTy::Isize) | ty::Uint(ty::UintTy::Usize) => true,
        ty::Tuple(parts) => parts.iter().any(has_target_width),
        _ => false,
    }
}

thread_local! {
    /// The body that reads the constants (see `has_cfg`). `None`: a reader of another crate.
    static READER: std::cell::Cell<Option<LocalDefId>> = const { std::cell::Cell::new(None) };
}

/// Sets the body that reads the constants, until the value goes out of scope.
pub struct Reader(Option<LocalDefId>);

impl Reader {
    pub fn set(reader: Option<LocalDefId>) -> Reader {
        Reader(READER.with(|cell| cell.replace(reader)))
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        READER.with(|cell| cell.set(self.0));
    }
}

/// True if a `#[cfg]` or a `#[cfg_attr]` attribute can give the reader a different constant:
/// the constant or one of its parents has such an attribute, and the reader is not inside the
/// item with the attribute. A reader inside that item exists only together with this
/// constant (a `#[cfg(test)]` module with a constant and its tests). The compiler keeps a
/// trace attribute for each `cfg` that is true in this configuration.
fn has_cfg(tcx: TyCtxt<'_>, def: LocalDefId) -> bool {
    has_trace(tcx, def, true)
}

/// See `has_cfg`. With `attr` false, a `#[cfg_attr]` attribute is not a cause.
fn has_trace(tcx: TyCtxt<'_>, def: LocalDefId, attr: bool) -> bool {
    let mut hir_id = tcx.local_def_id_to_hir_id(def);
    let scope = loop {
        let found = tcx.hir_attrs(hir_id).iter().any(|item| match item {
            Attribute::Parsed(AttributeKind::CfgTrace(..)) => true,
            Attribute::Parsed(AttributeKind::CfgAttrTrace(..)) => attr,
            _ => false,
        });
        if found {
            break hir_id;
        }
        if hir_id == rustc_hir::CRATE_HIR_ID {
            return false;
        }
        hir_id = tcx.parent_hir_id(hir_id);
    };
    let Some(reader) = READER.with(|cell| cell.get()) else { return true };
    let mut hir_id = tcx.local_def_id_to_hir_id(reader);
    loop {
        if hir_id == scope {
            return false;
        }
        if hir_id == rustc_hir::CRATE_HIR_ID {
            return true;
        }
        hir_id = tcx.parent_hir_id(hir_id);
    }
}

/// True if a `#[cfg]` attribute selects the item `def_id` of the local crate (the item or a
/// parent has the attribute), and the reader is not inside the item with the attribute
/// (batch 5o). A `#[cfg_attr]` is not a cause here: the verifier reads its content.
pub fn cfg_selects(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    def_id.as_local().is_some_and(|local| has_trace(tcx, local, false))
}

/// The items and the types that the code of the item `def` uses, by the name resolution and
/// the type check of the compiler (batch 5o). The entries:
///
/// - `cfg PATH`: an item of the local crate that a `#[cfg]` attribute selects. The body names
///   it, calls it (a method call and an operator too), or has a value of this type.
/// - `PATH`: a type of the workspace in the type of an expression or a pattern.
/// - `touch PATH`: the code reads a part of a value of this type: a pattern, a field, a cast.
/// - `std PATH`: a type of the standard library (no layout use; for the method name rule).
/// - `foreign PATH`: a type of a crate outside the workspace.
///
/// The walk includes each closure and each constant block of the item.
pub fn uses(tcx: TyCtxt<'_>, def: LocalDefId) -> Vec<String> {
    use rustc_hir::def::Res;
    use rustc_hir::intravisit::{self, Visitor as HirVisitor};
    use rustc_hir::{ExprKind, PatKind};
    struct Uses<'tcx> {
        tcx: TyCtxt<'tcx>,
        typeck: &'tcx ty::TypeckResults<'tcx>,
        out: std::collections::BTreeSet<String>,
    }
    impl<'tcx> Uses<'tcx> {
        fn def(&mut self, def_id: DefId) {
            if cfg_selects(self.tcx, def_id) {
                self.out.insert(format!("cfg {}", crate::reach::real_path(self.tcx, def_id)));
            }
            // A member of an `impl`: the body uses the self type of the `impl`, also if the
            // text names the type by an alias.
            if matches!(self.tcx.def_kind(def_id), DefKind::AssocFn | DefKind::AssocConst | DefKind::AssocTy) {
                if let Some(parent) = self.tcx.opt_parent(def_id).filter(|parent| matches!(self.tcx.def_kind(*parent), DefKind::Impl { .. })) {
                    let ty = self.tcx.type_of(parent).instantiate_identity().skip_norm_wip();
                    self.ty(ty);
                }
            }
        }
        fn adt(&mut self, def_id: DefId, touch: bool) {
            let path = crate::reach::real_path(self.tcx, def_id);
            if crate::reach::is_std(self.tcx.crate_name(def_id.krate).as_str()) {
                self.out.insert(format!("std {path}"));
                return;
            }
            self.def(def_id);
            if !def_id.is_local() && !is_workspace_crate(self.tcx, def_id.krate) {
                self.out.insert(format!("foreign {path}"));
            } else {
                self.out.insert(if touch { format!("touch {path}") } else { path });
            }
        }
        fn ty(&mut self, ty: Ty<'tcx>) {
            for arg in ty.walk() {
                if let Some(ty::Adt(adt, _)) = arg.as_type().map(|ty| ty.kind()) {
                    self.adt(adt.did(), false);
                }
            }
        }
        /// The code reads a part of a value of the type `ty`.
        fn touch(&mut self, ty: Ty<'tcx>) {
            if let ty::Adt(adt, _) = ty.peel_refs().kind() {
                self.adt(adt.did(), true);
            }
        }
        fn res(&mut self, res: Res) {
            match res {
                Res::Def(kind, def_id) => {
                    self.def(def_id);
                    let owner = match kind {
                        DefKind::Struct | DefKind::Enum | DefKind::Union => Some(def_id),
                        DefKind::Variant => self.tcx.opt_parent(def_id),
                        DefKind::Ctor(..) => self.tcx.opt_parent(def_id).map(|parent| if self.tcx.def_kind(parent) == DefKind::Variant { self.tcx.parent(parent) } else { parent }),
                        _ => None,
                    };
                    if let Some(owner) = owner {
                        self.adt(owner, false);
                    }
                }
                Res::SelfTyAlias { alias_to, .. } | Res::SelfCtor(alias_to) => {
                    let ty = self.tcx.type_of(alias_to).instantiate_identity().skip_norm_wip();
                    self.ty(ty);
                }
                _ => {}
            }
        }
    }
    impl<'tcx> HirVisitor<'tcx> for Uses<'tcx> {
        fn visit_nested_body(&mut self, id: rustc_hir::BodyId) {
            let old = std::mem::replace(&mut self.typeck, self.tcx.typeck_body(id));
            self.visit_body(self.tcx.hir_body(id));
            self.typeck = old;
        }
        fn visit_path(&mut self, path: &rustc_hir::Path<'tcx>, _id: rustc_hir::HirId) {
            self.res(path.res);
            intravisit::walk_path(self, path);
        }
        fn visit_expr(&mut self, expr: &'tcx rustc_hir::Expr<'tcx>) {
            for ty in [self.typeck.expr_ty_opt(expr), self.typeck.expr_ty_adjusted_opt(expr)].into_iter().flatten() {
                self.ty(ty);
            }
            // Each step of an adjustment: the method lookup tries the methods of each of these
            // types.
            for adjustment in self.typeck.expr_adjustments(expr) {
                self.ty(adjustment.target);
            }
            if let Some(args) = self.typeck.node_args_opt(expr.hir_id) {
                for ty in args.types() {
                    self.ty(ty);
                }
            }
            // A method call, an operator of a trait, an index.
            if let Some(def_id) = self.typeck.type_dependent_def_id(expr.hir_id) {
                self.def(def_id);
            }
            match expr.kind {
                ExprKind::Path(ref qpath) => {
                    let res = self.typeck.qpath_res(qpath, expr.hir_id);
                    self.res(res);
                }
                ExprKind::Struct(qpath, _, tail) => {
                    let res = self.typeck.qpath_res(qpath, expr.hir_id);
                    self.res(res);
                    // A base (`..base`) or a default copies fields that the text does not name.
                    if !matches!(tail, rustc_hir::StructTailExpr::None) {
                        if let Some(ty) = self.typeck.expr_ty_opt(expr) {
                            self.touch(ty);
                        }
                    }
                }
                // A field of a struct: the name selects the field in each configuration. A field
                // of a union reads the layout.
                ExprKind::Field(base, _) => {
                    if let Some(ty) = self.typeck.expr_ty_adjusted_opt(base).filter(|ty| !matches!(ty.peel_refs().kind(), ty::Adt(adt, _) if adt.is_struct())) {
                        self.touch(ty);
                    }
                }
                ExprKind::Cast(source, _) => {
                    if let Some(ty) = self.typeck.expr_ty_opt(source) {
                        self.touch(ty);
                    }
                }
                ExprKind::OffsetOf(..) => {
                    if let Some(args) = self.typeck.node_args_opt(expr.hir_id) {
                        for ty in args.types() {
                            self.touch(ty);
                        }
                    }
                    self.out.insert("?".to_string());
                }
                _ => {}
            }
            intravisit::walk_expr(self, expr);
        }
        fn visit_pat(&mut self, pat: &'tcx rustc_hir::Pat<'tcx>) {
            let ty = self.typeck.node_type_opt(pat.hir_id);
            if let Some(ty) = ty {
                self.ty(ty);
            }
            match pat.kind {
                PatKind::Wild | PatKind::Binding(..) | PatKind::Or(..) | PatKind::Tuple(..) | PatKind::Ref(..) => {}
                PatKind::Struct(ref qpath, ..) | PatKind::TupleStruct(ref qpath, ..) => {
                    let res = self.typeck.qpath_res(qpath, pat.hir_id);
                    self.res(res);
                    // A pattern of an enum reads the discriminant. A pattern of a struct only
                    // names fields.
                    if let Some(ty) = ty.filter(|ty| !matches!(ty.peel_refs().kind(), ty::Adt(adt, _) if adt.is_struct())) {
                        self.touch(ty);
                    }
                }
                // A constant, a range, a slice, and each other kind.
                _ => {
                    if let Some(ty) = ty {
                        self.touch(ty);
                    }
                }
            }
            intravisit::walk_pat(self, pat);
        }
    }
    // A constructor has a MIR body and no HIR body: `reach::adts` reports its types.
    if tcx.hir_maybe_body_owned_by(def).is_none() {
        return Vec::new();
    }
    let mut uses = Uses { tcx, typeck: tcx.typeck(def), out: Default::default() };
    let node = tcx.hir_node_by_def_id(def);
    if let Some(decl) = node.fn_decl() {
        uses.visit_fn_decl(decl);
    }
    if let Some(ty) = node.ty() {
        uses.visit_ty_unambig(ty);
    }
    if let Some(body) = tcx.hir_maybe_body_owned_by(def) {
        uses.visit_body(body);
    }
    uses.out.into_iter().collect()
}

/// One name of the expanded code of an item (batch 5p): an identifier that the name resolution
/// or the type check of the compiler gives a meaning. The form fields have the meaning of the
/// token scan of the verifier.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NameUse {
    pub word: String,
    /// After `.` or `::`: a method call, a field, a segment of a path that is not the first.
    pub member: bool,
    /// The segment before `::`. `crate`, `self`, `super`: the keyword (`$crate` of the local
    /// crate is `crate`; `$crate` of a workspace crate is the name of the crate). Empty: a path
    /// that starts with `::`. `$`: a module of a crate outside the workspace (no item of the
    /// workspace can be the meaning). `>`: a type that is not a path. `.`: the name of a field
    /// in a struct expression or a struct pattern.
    pub qual: Option<String>,
    /// Before `::`.
    pub prefix: bool,
    /// The name comes from a macro expansion: the name of the outermost macro call.
    pub mac: Option<String>,
    /// The line of the name in the source, or the line of the outermost macro call.
    pub line: usize,
    /// A method call or a path `Type::name` with a rival (batch 5q, see `rival`): the kinds
    /// of the trait facts that must change, then `|`, then the message.
    pub rival: Option<String>,
}

/// The names in the expanded code of the item `def` (batch 5p): its signature, its body, each
/// closure, each constant block, and each item in the body. The source text of a body does not
/// have the names that a macro puts into it, so the name rule of the verifier reads this list.
/// Each entry has a count.
///
/// A name of a desugaring of the compiler (`for`, `?`, `format_args!` after the expansion) is
/// not in the list: the compiler makes that code after the name resolution, from language
/// items. No item of the workspace can change its meaning.
///
/// A closure and a constant block have no list of their own: the list of the item has them.
pub fn names(tcx: TyCtxt<'_>, def: LocalDefId) -> Vec<(NameUse, usize)> {
    use rustc_hir::def::Res;
    use rustc_hir::intravisit::{self, Visitor as HirVisitor};
    use rustc_hir::{ExprKind, PatKind, QPath, TyKind};
    use rustc_span::symbol::{Ident, kw};
    struct Names<'tcx> {
        tcx: TyCtxt<'tcx>,
        /// The type check results of the body that the walk is in.
        typeck: Option<&'tcx ty::TypeckResults<'tcx>>,
        out: std::collections::BTreeMap<NameUse, usize>,
        /// The path of the type before the last segment of a path `Type::name`: its last
        /// segment is before `::`.
        tail: Option<*const ()>,
    }
    impl<'tcx> Names<'tcx> {
        /// The type check results that have the node. A signature of a nested item is not
        /// a part of the body around it: it has no result.
        fn results(&self, id: rustc_hir::HirId) -> Option<&'tcx ty::TypeckResults<'tcx>> {
            self.typeck.filter(|typeck| typeck.hir_owner == id.owner)
        }
        /// The outermost macro call and the line of a name. `None`: the name comes from a
        /// desugaring.
        fn origin(&self, span: Span) -> Option<(Option<String>, usize)> {
            let (mut span, mut mac, mut first) = (span, None, true);
            for _ in 0..64 {
                let ctxt = span.ctxt();
                if ctxt.is_root() {
                    break;
                }
                let data = ctxt.outer_expn_data();
                match data.kind {
                    ExpnKind::Desugaring(_) if first => return None,
                    ExpnKind::Macro(_, name) => {
                        let name = name.to_string();
                        mac = Some(name.rsplit("::").next().unwrap_or(&name).to_string());
                    }
                    ExpnKind::Root | ExpnKind::Desugaring(_) => {}
                    // A change of the compiler to the syntax tree, and a new kind.
                    #[allow(unreachable_patterns)]
                    _ => mac = Some("?".to_string()),
                }
                first = false;
                span = data.call_site;
            }
            // An expansion with more than 64 levels: the name has no known place.
            if !span.ctxt().is_root() {
                mac = Some("?".to_string());
            }
            let line = if span.is_dummy() { 0 } else { self.tcx.sess.source_map().lookup_char_pos(span.lo()).line };
            Some((mac, line))
        }
        fn add(&mut self, ident: Ident, member: bool, qual: Option<String>, prefix: bool) {
            self.add_rival(ident, member, qual, prefix, None);
        }
        fn add_rival(&mut self, ident: Ident, member: bool, qual: Option<String>, prefix: bool, rival: Option<String>) {
            let word = ident.name.to_string();
            // A position of a tuple (`pair.0`) is not a name.
            if word.is_empty() || word.starts_with(|c: char| c.is_ascii_digit()) || word == "_" {
                return;
            }
            let Some((mac, line)) = self.origin(ident.span) else { return };
            let word = word.strip_prefix("r#").unwrap_or(&word).to_string();
            *self.out.entry(NameUse { word, member, qual, prefix, mac, line, rival }).or_default() += 1;
        }
        /// The qualifier that the segment gives to the next segment.
        fn qual_of(&self, segment: &rustc_hir::PathSegment<'tcx>) -> String {
            let name = segment.ident.name;
            if name == kw::PathRoot {
                return String::new();
            }
            let foreign = |krate| !is_workspace_crate(self.tcx, krate);
            if let Res::Def(DefKind::Mod, def_id) = segment.res {
                if name == kw::DollarCrate {
                    return match def_id.krate {
                        rustc_span::def_id::LOCAL_CRATE => "crate".to_string(),
                        krate if foreign(krate) => "$".to_string(),
                        krate => self.tcx.crate_name(krate).to_string(),
                    };
                }
                if !def_id.is_local() && foreign(def_id.krate) {
                    return "$".to_string();
                }
            }
            match name {
                kw::DollarCrate | kw::Crate => "crate".to_string(),
                kw::SelfLower => "self".to_string(),
                kw::Super => "super".to_string(),
                _ => name.to_string(),
            }
        }
    }
    /// Two entries of a name: one line for each.
    fn join(first: Option<String>, second: Option<String>) -> Option<String> {
        match (first, second) {
            (Some(first), Some(second)) => Some(format!("{first}\n{second}")),
            (first, second) => first.or(second),
        }
    }
    impl<'tcx> HirVisitor<'tcx> for Names<'tcx> {
        type NestedFilter = rustc_middle::hir::nested_filter::All;
        fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
            self.tcx
        }
        fn visit_nested_body(&mut self, id: rustc_hir::BodyId) {
            let old = std::mem::replace(&mut self.typeck, Some(self.tcx.typeck_body(id)));
            self.visit_body(self.tcx.hir_body(id));
            self.typeck = old;
        }
        fn visit_path(&mut self, path: &rustc_hir::Path<'tcx>, id: rustc_hir::HirId) {
            let tail = self.tail.take_if(|tail| std::ptr::eq(*tail, path as *const _ as *const ()));
            let count = path.segments.len();
            for (index, segment) in path.segments.iter().enumerate() {
                if segment.ident.is_path_segment_keyword() {
                    continue;
                }
                let qual = index.checked_sub(1).map(|before| self.qual_of(&path.segments[before]));
                // A full path to an item of a trait: the selection of the `impl` uses trait
                // facts (batch 5q, part C1).
                let uses = match (index + 1 == count && tail.is_none(), self.results(id), path.res) {
                    (true, Some(typeck), rustc_hir::def::Res::Def(DefKind::AssocFn | DefKind::AssocConst, item)) => {
                        crate::rival::Finder { tcx: self.tcx, typeck }.uses(id, "path", item)
                    }
                    _ => None,
                };
                self.add_rival(segment.ident, index > 0, qual, index + 1 < count || tail.is_some(), uses);
            }
            intravisit::walk_path(self, path);
        }
        fn visit_qpath(&mut self, qpath: &'tcx QPath<'tcx>, id: rustc_hir::HirId, span: Span) {
            if let QPath::TypeRelative(qself, segment) = qpath {
                let qual = match qself.kind {
                    TyKind::Path(QPath::Resolved(_, path)) => {
                        self.tail = Some(path as *const _ as *const ());
                        path.segments.last().map(|last| self.qual_of(last)).unwrap_or_else(|| ">".to_string())
                    }
                    TyKind::Path(QPath::TypeRelative(_, inner)) => inner.ident.name.to_string(),
                    _ => ">".to_string(),
                };
                // A function or a constant of a type: the lookup of the compiler gives the
                // meaning. A path in a type has no such meaning.
                let rival = self.results(id).and_then(|typeck| match typeck.type_dependent_def(id) {
                    Some((DefKind::AssocFn | DefKind::AssocConst, winner)) => {
                        let finder = crate::rival::Finder { tcx: self.tcx, typeck };
                        join(join(finder.path_item(id, segment.ident.name, winner), finder.uses(id, "path", winner)), finder.feature("path", winner))
                    }
                    _ => None,
                });
                self.add_rival(segment.ident, true, Some(qual), false, rival);
            }
            intravisit::walk_qpath(self, qpath, id);
            let _ = span;
            self.tail = None;
        }
        fn visit_expr(&mut self, expr: &'tcx rustc_hir::Expr<'tcx>) {
            match expr.kind {
                ExprKind::MethodCall(segment, receiver, ..) => {
                    let rival = match self.results(expr.hir_id) {
                        Some(typeck) => {
                            let finder = crate::rival::Finder { tcx: self.tcx, typeck };
                            let winner = typeck.type_dependent_def_id(expr.hir_id);
                            let uses = winner.and_then(|winner| finder.uses(expr.hir_id, "call", winner));
                            let feature = winner.and_then(|winner| finder.feature("call", winner));
                            join(join(finder.method(expr, segment.ident.name, receiver), uses), feature)
                        }
                        None => Some("open|the call has no type check result".to_string()),
                    };
                    self.add_rival(segment.ident, true, None, false, rival);
                }
                ExprKind::Field(_, ident) => self.add(ident, true, None, false),
                ExprKind::Struct(_, fields, _) => {
                    for field in fields {
                        self.add(field.ident, false, Some(".".to_string()), false);
                    }
                }
                _ => {}
            }
            intravisit::walk_expr(self, expr);
        }
        fn visit_pat(&mut self, pat: &'tcx rustc_hir::Pat<'tcx>) {
            match pat.kind {
                // An inactive constant with this name makes the binding a constant pattern.
                PatKind::Binding(_, _, ident, _) => self.add(ident, false, None, false),
                PatKind::Struct(_, fields, _) => {
                    for field in fields {
                        self.add(field.ident, false, Some(".".to_string()), false);
                    }
                }
                _ => {}
            }
            intravisit::walk_pat(self, pat);
        }
        fn visit_assoc_item_constraint(&mut self, constraint: &'tcx rustc_hir::AssocItemConstraint<'tcx>) {
            self.add(constraint.ident, true, Some(">".to_string()), false);
            intravisit::walk_assoc_item_constraint(self, constraint);
        }
    }
    if tcx.typeck_root_def_id(def.to_def_id()) != def.to_def_id() {
        return Vec::new();
    }
    let typeck = tcx.hir_maybe_body_owned_by(def).map(|_| tcx.typeck(def));
    let mut names = Names { tcx, typeck, out: Default::default(), tail: None };
    let node = tcx.hir_node_by_def_id(def);
    if let Some(decl) = node.fn_decl() {
        names.visit_fn_decl(decl);
    }
    if let Some(generics) = node.generics() {
        names.visit_generics(generics);
    }
    if let Some(ty) = node.ty() {
        names.visit_ty_unambig(ty);
    }
    if let Some(body) = tcx.hir_maybe_body_owned_by(def) {
        names.visit_body(body);
    }
    names.out.into_iter().collect()
}

/// True if the item or one of its parents has a `#[cfg]` or a `#[cfg_attr]` attribute (batch
/// 5l). The compiler selected this item by the build configuration, so no normal form looks
/// into it from a different body.
pub fn under_cfg(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    let _reader = Reader::set(None);
    def_id.as_local().is_some_and(|local| has_cfg(tcx, local))
}

/// True if each value and each operation of the body of a constant is configuration-free.
fn body_is_free<'tcx>(tcx: TyCtxt<'tcx>, body: &Body<'tcx>) -> bool {
    struct Check<'a, 'tcx> {
        tcx: TyCtxt<'tcx>,
        body: &'a Body<'tcx>,
        free: bool,
    }
    impl<'tcx> Visitor<'tcx> for Check<'_, 'tcx> {
        fn visit_operand(&mut self, operand: &Operand<'tcx>, _location: Location) {
            match operand {
                Operand::Copy(_) | Operand::Move(_) => {}
                Operand::Constant(constant) => {
                    if source_of(self.tcx, constant.span, &constant.const_) != Source::Free {
                        self.free = false;
                    }
                }
                #[allow(unreachable_patterns)]
                _ => self.free = false,
            }
        }
        fn visit_rvalue(&mut self, rvalue: &Rvalue<'tcx>, location: Location) {
            let allowed = match rvalue {
                Rvalue::Use(..) | Rvalue::Repeat(..) | Rvalue::Ref(..) | Rvalue::RawPtr(..) | Rvalue::Aggregate(..) => true,
                Rvalue::CopyForDeref(..) | Rvalue::Discriminant(..) => true,
                Rvalue::BinaryOp(..) | Rvalue::UnaryOp(..) => !has_target_width(rvalue.ty(self.body, self.tcx)),
                Rvalue::Cast(kind, _, ty) => {
                    matches!(kind, mir::CastKind::IntToInt | mir::CastKind::IntToFloat | mir::CastKind::FloatToInt | mir::CastKind::FloatToFloat)
                        && !has_target_width(*ty)
                }
                _ => false,
            };
            if !allowed {
                self.free = false;
            }
            self.super_rvalue(rvalue, location);
        }
        fn visit_terminator(&mut self, terminator: &Terminator<'tcx>, location: Location) {
            let allowed = matches!(
                terminator.kind,
                TerminatorKind::Goto { .. }
                    | TerminatorKind::Return
                    | TerminatorKind::Unreachable
                    | TerminatorKind::SwitchInt { .. }
                    | TerminatorKind::Assert { .. }
                    | TerminatorKind::FalseEdge { .. }
                    | TerminatorKind::FalseUnwind { .. }
            );
            if !allowed {
                self.free = false;
            }
            self.super_terminator(terminator, location);
        }
    }
    let mut check = Check { tcx, body, free: true };
    check.visit_body(body);
    check.free
}

/// The locals that can hold a value that the optimizer knows (see the module text). A local is
/// in the result if one of its writes can give a known value: each place that the write reads
/// is in the result, and the write is not an operation that this pass makes opaque.
fn maybe_const<'tcx>(tcx: TyCtxt<'tcx>, body: &Body<'tcx>) -> Vec<bool> {
    struct Reads<'a>(&'a [bool], bool);
    impl<'tcx> Visitor<'tcx> for Reads<'_> {
        fn visit_local(&mut self, local: Local, _context: mir::visit::PlaceContext, _location: Location) {
            if !self.0[local.as_usize()] {
                self.1 = false;
            }
        }
    }
    let count = body.local_decls.len();
    let mut maybe = vec![true; count];
    for local in body.args_iter() {
        maybe[local.as_usize()] = false;
    }
    // The writes of each local: the assignments, and `None` for a write that is never known (the
    // destination of a call).
    let mut writes: Vec<Vec<Option<&Rvalue<'tcx>>>> = vec![Vec::new(); count];
    for block in body.basic_blocks.iter() {
        for statement in &block.statements {
            if let StatementKind::Assign(assign) = &statement.kind {
                writes[assign.0.local.as_usize()].push(Some(&assign.1));
            }
        }
        if let Some(terminator) = &block.terminator {
            match &terminator.kind {
                TerminatorKind::Call { destination, .. } => writes[destination.local.as_usize()].push(None),
                TerminatorKind::Yield { resume_arg, .. } => writes[resume_arg.local.as_usize()].push(None),
                _ => {}
            }
        }
    }
    loop {
        let mut changed = false;
        for local in 0..count {
            if !maybe[local] || writes[local].is_empty() {
                continue;
            }
            let known = writes[local].iter().any(|write| {
                let Some(rvalue) = write else { return false };
                if is_target_operation(tcx, body, rvalue) {
                    return false;
                }
                let mut reads = Reads(&maybe, true);
                reads.visit_rvalue(rvalue, Location::START);
                reads.1
            });
            if !known {
                maybe[local] = false;
                changed = true;
            }
        }
        if !changed {
            return maybe;
        }
    }
}

/// True for an operation whose result the target decides: a binary or unary operation with a
/// `usize` or `isize` result, and a cast to `usize` or `isize` from another type.
fn is_target_operation<'tcx>(tcx: TyCtxt<'tcx>, body: &Body<'tcx>, rvalue: &Rvalue<'tcx>) -> bool {
    match rvalue {
        Rvalue::BinaryOp(..) | Rvalue::UnaryOp(..) => has_target_width(rvalue.ty(body, tcx)),
        Rvalue::Cast(_, operand, ty) => has_target_width(*ty) && !has_target_width(operand.ty(body, tcx)),
        Rvalue::ThreadLocalRef(..) => true,
        _ => false,
    }
}

/// True for a cast that makes a length of an array type a value: an unsize coercion from a type
/// with an array.
fn is_array_unsize<'tcx>(tcx: TyCtxt<'tcx>, body: &Body<'tcx>, rvalue: &Rvalue<'tcx>) -> bool {
    match rvalue {
        Rvalue::Cast(mir::CastKind::PointerCoercion(ty::adjustment::PointerCoercion::Unsize, _), operand, _) => {
            operand.ty(body, tcx).walk().any(|part| part.as_type().is_some_and(|ty| matches!(ty.kind(), ty::Array(..))))
        }
        _ => false,
    }
}

/// How the printer finds the source of an opaque value. The optimizer of rustc removes the span
/// and the name of a constant operand. It keeps the span of a call.
#[derive(Clone, Copy)]
enum Mark {
    /// The span of the call gives the source (`span_source`).
    At(Span),
    /// No source: the text of the value is sufficient. The call has no span.
    Plain,
    /// The source is not known. The call gets a span that gives no source, and the printer
    /// makes the value different from each other value. Without such a span, the value goes
    /// through two calls with no span.
    Unknown,
}

/// The mark of a constant operand, or `None` for a configuration-free constant.
fn mark_of<'tcx>(tcx: TyCtxt<'tcx>, constant: &mir::ConstOperand<'tcx>) -> Option<Mark> {
    let source = source_of(tcx, constant.span, &constant.const_);
    let (span, source) = match source {
        Source::Free => return None,
        // The type of the place names the array type.
        Source::TypeLevel => return Some(Mark::Plain),
        Source::Named(_) => match promoted_inner(tcx, &constant.const_) {
            Some(inner) => (inner.span, source_of(tcx, inner.span, &inner.const_)),
            None => (constant.span, source),
        },
        Source::Macro { .. } | Source::Static { .. } => (constant.span, source),
    };
    // The printer must find the same source from the span alone.
    if !span.is_dummy() && span_source(tcx, span).as_ref() == Some(&source) {
        Some(Mark::At(span))
    } else {
        Some(Mark::Unknown)
    }
}

/// For a promoted constant of the form `&CONSTANT`: the operand of the constant.
fn promoted_inner<'tcx>(tcx: TyCtxt<'tcx>, constant: &Const<'tcx>) -> Option<mir::ConstOperand<'tcx>> {
    let Const::Unevaluated(unevaluated, _) = constant else { return None };
    let promoted = unevaluated.promoted?;
    let body = tcx.promoted_mir(unevaluated.def.as_local()?.to_def_id()).get(promoted)?;
    let mut found = None;
    let mut count = 0;
    for block in body.basic_blocks.iter() {
        for statement in &block.statements {
            match &statement.kind {
                StatementKind::Assign(assign) => match &assign.1 {
                    Rvalue::Use(Operand::Constant(constant), ..) => {
                        found = Some(**constant);
                        count += 1;
                    }
                    Rvalue::Ref(..) => {}
                    _ => return None,
                },
                StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => {}
                _ => return None,
            }
        }
        if !matches!(block.terminator().kind, TerminatorKind::Return) {
            return None;
        }
    }
    if count == 1 { found } else { None }
}

/// The source that the span of an opaque call gives, for the printer.
pub fn span_source(tcx: TyCtxt<'_>, span: Span) -> Option<Source> {
    macro_source(tcx, span).or_else(|| NOTED.with(|noted| noted.borrow().get(&span).cloned()))
}

/// Replaces the first operand that it takes by a move of `local`.
struct Replace<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    locals: &'a rustc_index::IndexVec<Local, LocalDecl<'tcx>>,
    local: Local,
    /// Take the first operand of each kind. If not, take only a constant that is not free.
    any: bool,
    found: Option<(Operand<'tcx>, Ty<'tcx>, Mark)>,
}

impl<'tcx> MutVisitor<'tcx> for Replace<'_, 'tcx> {
    fn tcx(&self) -> TyCtxt<'tcx> {
        self.tcx
    }

    fn visit_operand(&mut self, operand: &mut Operand<'tcx>, _location: Location) {
        if self.found.is_some() {
            return;
        }
        let mark = match operand {
            Operand::Constant(constant) if matches!(constant.const_.ty().kind(), ty::FnDef(..)) => None,
            Operand::Constant(constant) => mark_of(self.tcx, constant).or(self.any.then_some(Mark::Plain)),
            Operand::Copy(_) | Operand::Move(_) => self.any.then_some(Mark::Plain),
            #[allow(unreachable_patterns)]
            _ => None,
        };
        let Some(mark) = mark else { return };
        let ty = operand.ty(self.locals, self.tcx);
        let old = std::mem::replace(operand, Operand::Move(Place::from(self.local)));
        self.found = Some((old, ty, mark));
    }
}

/// The pass: makes each value opaque that is not configuration-free (see the module text).
fn make_opaque<'tcx>(tcx: TyCtxt<'tcx>, def: LocalDefId, body: &mut Body<'tcx>) {
    let Some(black_box) = black_box_def(tcx) else {
        // A crate with no `core` has no code that this pass must change.
        return;
    };
    // A pattern constant that is not free: each switch of the body can use its value.
    let switches = !hir_sources(tcx, def).is_empty();
    let slice_len = tcx.lang_items().slice_len_fn();
    let mut pass = Pass { tcx, black_box };

    // Step 1: each constant operand that is not configuration-free.
    let mut index = 0;
    while index < body.basic_blocks.len() {
        let block = BasicBlock::from_usize(index);
        index += 1;
        if pass.is_opaque_call(body, block) {
            continue;
        }
        let local = Local::from_usize(body.local_decls.len());
        let count = body.basic_blocks[block].statements.len();
        for at in 0..=count {
            let (blocks, locals) = (body.basic_blocks.as_mut(), &body.local_decls);
            let mut replace = Replace { tcx, locals, local, any: false, found: None };
            match blocks[block].statements.get_mut(at) {
                Some(statement) => replace.visit_statement(statement, Location::START),
                None => replace.visit_terminator(blocks[block].terminator_mut(), Location::START),
            }
            if let Some((operand, ty, mark)) = replace.found {
                pass.before(body, block, at, operand, ty, mark);
                break;
            }
        }
    }

    // Step 2: each unsize coercion of an array. The length of the array type becomes a value
    // here: the result is opaque.
    let mut index = 0;
    while index < body.basic_blocks.len() {
        let block = BasicBlock::from_usize(index);
        index += 1;
        let found = body.basic_blocks[block].statements.iter().position(|statement| match &statement.kind {
            StatementKind::Assign(assign) => is_array_unsize(tcx, body, &assign.1),
            _ => false,
        });
        let Some(at) = found else { continue };
        let source_info = body.basic_blocks[block].statements[at].source_info;
        let StatementKind::Assign(assign) = &body.basic_blocks[block].statements[at].kind else { unreachable!() };
        let ty = assign.0.ty(&body.local_decls, tcx).ty;
        let local = body.local_decls.push(LocalDecl::new(ty, source_info.span));
        let StatementKind::Assign(assign) = &mut body.basic_blocks.as_mut()[block].statements[at].kind else { unreachable!() };
        let destination = std::mem::replace(&mut assign.0, Place::from(local));
        // The rest of the block goes to a new block. The loop reads it later.
        pass.call(body, block, at + 1, Operand::Move(Place::from(local)), ty, destination, DUMMY_SP);
    }

    // Step 3: each operation of the target on values that the optimizer can know. The result
    // of an opaque call is not such a value, so no value goes through two opaque calls here.
    let maybe = maybe_const(tcx, body);
    let known = |operand: &Operand<'tcx>| match operand {
        Operand::Copy(place) | Operand::Move(place) => maybe.get(place.local.as_usize()).copied().unwrap_or(false),
        _ => true,
    };
    let mut index = 0;
    while index < body.basic_blocks.len() {
        let block = BasicBlock::from_usize(index);
        index += 1;
        if pass.is_opaque_call(body, block) {
            continue;
        }
        let local = Local::from_usize(body.local_decls.len());
        let count = body.basic_blocks[block].statements.len();
        let mut done = false;
        for at in 0..count {
            let StatementKind::Assign(assign) = &body.basic_blocks[block].statements[at].kind else { continue };
            if !is_target_operation(tcx, body, &assign.1) {
                continue;
            }
            let mut all = true;
            let mut some = false;
            Operands(&mut |operand| {
                some = true;
                all &= known(operand);
            })
            .visit_rvalue(&assign.1, Location::START);
            if !(all && some) {
                continue;
            }
            let (blocks, locals) = (body.basic_blocks.as_mut(), &body.local_decls);
            let mut replace = Replace { tcx, locals, local, any: true, found: None };
            replace.visit_statement(&mut blocks[block].statements[at], Location::START);
            if let Some((operand, ty, _)) = replace.found {
                pass.before(body, block, at, operand, ty, Mark::Plain);
                done = true;
                break;
            }
        }
        if done {
            continue;
        }
        // The terminator. A call of an intrinsic or of the slice length is an operation of the
        // target: the optimizer replaces it by an operation that it can fold.
        let first_or_result = match &body.basic_blocks[block].terminator().kind {
            TerminatorKind::Call { func, args, target, .. } => match func.const_fn_def() {
                Some((def_id, _)) if tcx.intrinsic(def_id).is_some() || Some(def_id) == slice_len => {
                    (target.is_some() && args.iter().all(|arg| known(&arg.node))).then_some(!args.is_empty())
                }
                _ => None,
            },
            TerminatorKind::SwitchInt { discr, .. } if switches && known(discr) => Some(true),
            _ => None,
        };
        match first_or_result {
            Some(true) => {
                let (blocks, locals) = (body.basic_blocks.as_mut(), &body.local_decls);
                let mut replace = Replace { tcx, locals, local, any: true, found: None };
                match &mut blocks[block].terminator_mut().kind {
                    // Not the function of the call.
                    TerminatorKind::Call { args, .. } => {
                        for arg in args.iter_mut() {
                            replace.visit_operand(&mut arg.node, Location::START);
                        }
                    }
                    TerminatorKind::SwitchInt { discr, .. } => replace.visit_operand(discr, Location::START),
                    _ => {}
                }
                if let Some((operand, ty, _)) = replace.found {
                    pass.before(body, block, count, operand, ty, Mark::Plain);
                }
            }
            Some(false) => pass.after(body, block, local),
            None => {}
        }
    }
}

/// The operands of an rvalue.
struct Operands<'a, 'tcx>(&'a mut dyn FnMut(&Operand<'tcx>));

impl<'tcx> Visitor<'tcx> for Operands<'_, 'tcx> {
    fn visit_operand(&mut self, operand: &Operand<'tcx>, _location: Location) {
        (self.0)(operand)
    }
}

struct Pass<'tcx> {
    tcx: TyCtxt<'tcx>,
    black_box: DefId,
}

impl<'tcx> Pass<'tcx> {
    /// True if the block ends with a call of `black_box`.
    fn is_opaque_call(&self, body: &Body<'tcx>, block: BasicBlock) -> bool {
        match &body.basic_blocks[block].terminator().kind {
            TerminatorKind::Call { func, .. } => func.const_fn_def().is_some_and(|(def_id, _)| is_black_box(self.tcx, def_id)),
            _ => false,
        }
    }

    /// Puts `destination = black_box(operand)` before the statement `at` of the block (before
    /// the terminator if `at` is the number of statements). The statements from `at` and the
    /// terminator go to a new block. Returns the new block.
    fn call(
        &mut self,
        body: &mut Body<'tcx>,
        block: BasicBlock,
        at: usize,
        operand: Operand<'tcx>,
        ty: Ty<'tcx>,
        destination: Place<'tcx>,
        span: Span,
    ) -> BasicBlock {
        let function = Operand::function_handle(self.tcx, self.black_box, &[ty.into()], span);
        let blocks = body.basic_blocks.as_mut();
        let scope = blocks[block].terminator().source_info.scope;
        let is_cleanup = blocks[block].is_cleanup;
        let new_block = BasicBlock::from_usize(blocks.len());
        let rest = blocks[block].statements.split_off(at);
        let terminator = blocks[block].terminator.replace(Terminator {
            source_info: SourceInfo { span, scope },
            kind: TerminatorKind::Call {
                func: function,
                args: Box::new([rustc_span::dummy_spanned(operand)]),
                destination,
                target: Some(new_block),
                unwind: UnwindAction::Unreachable,
                call_source: CallSource::Misc,
                fn_span: span,
            },
        });
        let mut data = BasicBlockData::new(terminator, is_cleanup);
        data.statements = rest;
        blocks.push(data);
        new_block
    }

    /// Makes `operand` opaque before the statement `at`. The code at `at` already reads the next
    /// new local in its place.
    fn before(&mut self, body: &mut Body<'tcx>, block: BasicBlock, at: usize, operand: Operand<'tcx>, ty: Ty<'tcx>, mark: Mark) {
        let span = body.basic_blocks[block].terminator().source_info.span;
        let result = body.local_decls.push(LocalDecl::new(ty, span));
        match mark {
            Mark::At(span) => {
                self.call(body, block, at, operand, ty, Place::from(result), span);
            }
            Mark::Plain => {
                self.call(body, block, at, operand, ty, Place::from(result), DUMMY_SP);
            }
            Mark::Unknown => {
                let operand_span = match &operand {
                    Operand::Constant(constant) => constant.span,
                    _ => DUMMY_SP,
                };
                let candidates = [operand_span, span, body.span];
                if let Some(free) = candidates.into_iter().find(|span| !span.is_dummy() && span_source(self.tcx, *span).is_none()) {
                    self.call(body, block, at, operand, ty, Place::from(result), free);
                    return;
                }
                let first = body.local_decls.push(LocalDecl::new(ty, span));
                let next = self.call(body, block, at, operand, ty, Place::from(first), DUMMY_SP);
                self.call(body, next, 0, Operand::Move(Place::from(first)), ty, Place::from(result), DUMMY_SP);
            }
        }
    }

    /// Makes the result of the call opaque that ends the block. `local` is the next new local.
    fn after(&mut self, body: &mut Body<'tcx>, block: BasicBlock, local: Local) {
        let blocks = body.basic_blocks.as_mut();
        let source_info = blocks[block].terminator().source_info;
        let is_cleanup = blocks[block].is_cleanup;
        let new_block = BasicBlock::from_usize(blocks.len());
        let TerminatorKind::Call { destination, target, .. } = &mut blocks[block].terminator_mut().kind else { return };
        let old_destination = std::mem::replace(destination, Place::from(local));
        let old_target = target.replace(new_block);
        let ty = old_destination.ty(&body.local_decls, self.tcx).ty;
        let function = Operand::function_handle(self.tcx, self.black_box, &[ty.into()], DUMMY_SP);
        let terminator = Terminator {
            source_info: SourceInfo { span: DUMMY_SP, scope: source_info.scope },
            kind: TerminatorKind::Call {
                func: function,
                args: Box::new([rustc_span::dummy_spanned(Operand::Move(Place::from(local)))]),
                destination: old_destination,
                target: old_target,
                unwind: UnwindAction::Unreachable,
                call_source: CallSource::Misc,
                fn_span: DUMMY_SP,
            },
        };
        body.basic_blocks.as_mut().push(BasicBlockData::new(Some(terminator), is_cleanup));
        let decl = LocalDecl::new(ty, source_info.span);
        let pushed = body.local_decls.push(decl);
        debug_assert_eq!(pushed, local);
    }
}
