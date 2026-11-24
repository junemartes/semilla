//! The expansion of each macro call (batch 10c, design 63).
//!
//! The driver reads the syntax tree after the macro expansion. Each node of the tree has a
//! span, and the syntax context of the span names the macro expansion that made the node. A
//! node whose expansion is not the expansion of its parent node is a root of that expansion.
//! The roots of one expansion are its expanded code. The driver prints them with the pretty
//! printer of the compiler: the text has no span, no hygiene number, and one layout, so two
//! runs give equal text.
//!
//! The record of an expansion has the call site, the macro, the definition, the form of the
//! code (`expr`, `stmts`, `items`, `assoc`, `pat`, `ty`), the text, the items that the code
//! makes, and the macros that the code uses (the chain: `fee!` uses `base!`).
//!
//! The macros of the standard library (`format!`, `println!`, `vec!`, `assert!`, `#[test]`)
//! have no record (R63.1-3): their input is the code of the user, and the graph of 10a2 has
//! their rule. A derive of the standard library has a record: its code reads the fields of
//! the type.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use rustc_ast::mut_visit::{self, MutVisitor};
use rustc_ast::visit::{self, AssocCtxt, Visitor};
use rustc_middle::ty::TyCtxt;
use rustc_span::{ExpnId, ExpnKind, Span, Symbol, SyntaxContext};

/// One macro call and its expanded code.
#[derive(Clone, serde::Serialize)]
pub struct Expansion {
    /// The place of the call: the file relative to the workspace root, the byte range, and the
    /// first and the last line and column (lines from 1, columns from 0 in characters). A call
    /// inside an expansion has the place of its tokens (in the macro definition), or no place.
    pub file: String,
    pub r: [i64; 2],
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    /// The index of the expansion that has this call in its code (a call inside an expansion).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outer: Option<usize>,
    /// The name of the macro (`fee`, `PartialEq`).
    pub name: String,
    /// `bang`, `attr`, or `derive`.
    pub kind: &'static str,
    /// The definition: `src/lib.rs:2` for a macro of the workspace, else `crate serde`.
    pub def: String,
    /// The form of the code: `expr`, `stmts`, `items`, `assoc`, `pat`, or `ty`.
    pub form: &'static str,
    /// For the form `assoc`: the header of the impl or of the trait (`impl Cart`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub container: String,
    /// The source text of the call site on one line (at most 200 characters).
    pub call: String,
    /// The expanded code.
    pub text: String,
    /// The items that the code makes: a path, or the header of an impl.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
    /// The macros that the code uses (each call inside the expansion, at each depth), with
    /// their definitions: `[name, def]`, in the order of the first use.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<[String; 2]>,
    /// The module of the call (`a::b`; empty for the crate root).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub module: String,
    /// The source text of the call in a file that makes this call (R63.9-3), for a call whose
    /// tokens come from a macro whose expansion has no record (`sum!(1, 2)` makes
    /// `make!{@inner …}`): the place of the record is the place of that call.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub via: String,
    /// The files that the code reads with `include_str!` or `include_bytes!` (R63.9-6),
    /// relative to the workspace root.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
    /// The text with a mark for each literal that the compiler makes for the place of the call:
    /// an expansion of `line!()`, `column!()`, or `file!()`, or of a `concat!` whose input has
    /// one (R63.9-8). Empty if the code has no such literal.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub placeless: String,
}

/// The node kinds of an expansion.
#[derive(Clone)]
enum Node {
    Item(rustc_ast::Item),
    Assoc(rustc_ast::AssocItem),
    Stmt(rustc_ast::Stmt),
    Expr(rustc_ast::Expr),
    Pat(rustc_ast::Pat),
    Ty(rustc_ast::Ty),
}

/// An expansion while the walk runs.
struct Open {
    expn: ExpnId,
    nodes: Vec<Node>,
    module: String,
    container: String,
    self_ty: String,
    /// The macros that the code uses, at each depth (R63.9-4): `[name, def]`.
    uses: Vec<[String; 2]>,
    /// The files that the code reads (R63.9-6).
    reads: Vec<String>,
}

/// The texts of the expressions of the expanded code, by span (R63.1-5). The graph of a body
/// gives a point from a macro definition the text of its expanded code, so a renamed
/// metavariable does not change the text of a point.
static TEXTS: OnceLock<Mutex<HashMap<SpanKey, Option<String>>>> = OnceLock::new();

/// The key of a span: its bounds and its syntax context. The lowered tree gives a span a parent,
/// so the span itself is not a key.
type SpanKey = (rustc_span::BytePos, rustc_span::BytePos, SyntaxContext);

fn key(span: Span) -> SpanKey {
    (span.lo(), span.hi(), span.ctxt())
}

/// The text of the expanded code of an expression with this span, if the span is from the
/// expansion of a macro with a record.
pub(crate) fn text_of(span: Span) -> Option<String> {
    TEXTS.get()?.lock().ok()?.get(&key(span)).cloned().flatten()
}

/// The most characters of the text of a point from the expanded code.
const POINT_TEXT: usize = 300;

/// True for a macro of the standard library that has no record (R63.1-3).
fn standard(tcx: TyCtxt<'_>, kind: rustc_span::hygiene::MacroKind, def: Option<rustc_span::def_id::DefId>) -> bool {
    let Some(def) = def else { return true };
    let name = tcx.crate_name(def.krate);
    matches!(name.as_str(), "core" | "std" | "alloc" | "proc_macro" | "test") && kind != rustc_span::hygiene::MacroKind::Derive
}

struct Walk<'tcx> {
    tcx: TyCtxt<'tcx>,
    open: Vec<Open>,
    index: HashMap<ExpnId, usize>,
    stack: Vec<usize>,
    module: Vec<String>,
    container: String,
    self_ty: String,
    /// The name and the definition of each expansion of a macro that is not of the standard
    /// library.
    names: HashMap<ExpnId, Option<[String; 2]>>,
    /// The syntax contexts that `uses` has seen with the top of the stack.
    seen: std::collections::HashSet<(SyntaxContext, usize)>,
    /// The files of the workspace that are not Rust files, by the hash of their bytes.
    data_files: Option<HashMap<u64, String>>,
}

impl<'tcx> Walk<'tcx> {
    /// The expansion with a record whose root is a node with this span, if the node starts it.
    fn root(&mut self, span: Span) -> Option<usize> {
        let ctxt = span.ctxt();
        if ctxt.is_root() {
            return None;
        }
        let expn = ctxt.outer_expn();
        let data = expn.expn_data();
        let ExpnKind::Macro(kind, _) = data.kind else { return None };
        if standard(self.tcx, kind, data.macro_def_id) {
            return None;
        }
        if let Some(index) = self.index.get(&expn)
            && self.stack.contains(index)
        {
            return None;
        }
        let index = *self.index.entry(expn).or_insert_with(|| {
            self.open.push(Open { expn, nodes: Vec::new(), module: self.module.join("::"), container: self.container.clone(), self_ty: self.self_ty.clone(), uses: Vec::new(), reads: Vec::new() });
            self.open.len() - 1
        });
        Some(index)
    }

    fn enter(&mut self, index: usize, node: Node) {
        self.open[index].nodes.push(node);
        self.stack.push(index);
    }

    /// A file that the code of each open record reads (R63.9-6).
    fn read(&mut self, file: String) {
        for at in self.stack.clone() {
            if !self.open[at].reads.contains(&file) {
                self.open[at].reads.push(file.clone());
            }
        }
    }

    /// The file that an expression of `include_str!` or `include_bytes!` reads. The literal of
    /// `include_str!` has the span of the whole file. The literal of `include_bytes!` has the
    /// span of the call, so the file is the file of the workspace with the same bytes: the
    /// compiler adds each file that it reads to the source map.
    fn included(&mut self, expr: &rustc_ast::Expr) -> Option<String> {
        let ctxt = expr.span.ctxt();
        if ctxt.is_root() {
            return None;
        }
        let ExpnKind::Macro(_, name) = ctxt.outer_expn().expn_data().kind else { return None };
        match (&expr.kind, name.as_str()) {
            (rustc_ast::ExprKind::Lit(_), "include_str") => {
                let mut files = Vec::new();
                crate::spans::range(self.tcx, expr.span.with_ctxt(SyntaxContext::root()), &mut files)?;
                files.pop()
            }
            (rustc_ast::ExprKind::IncludedBytes(bytes), "include_bytes") => {
                let tcx = self.tcx;
                let files = self.data_files.get_or_insert_with(|| {
                    let mut out = HashMap::new();
                    for file in tcx.sess.source_map().files().iter() {
                        let rustc_span::FileName::Real(name) = &file.name else { continue };
                        let Some(path) = name.local_path() else { continue };
                        let relative = crate::workspace_path(path);
                        if relative.is_empty() || std::path::Path::new(&relative).is_absolute() || relative.ends_with(".rs") {
                            continue;
                        }
                        if let Ok(data) = std::fs::read(path) {
                            out.insert(hash_bytes(&data), relative);
                        }
                    }
                    out
                });
                files.get(&hash_bytes(bytes.as_byte_str())).cloned()
            }
            _ => None,
        }
    }

    /// The chain (R63.9-4): each expansion that made a node inside the code of an open record
    /// is a macro that the code of each open record uses. The walk goes up from the expansion of
    /// the node through the call sites, to the expansion of an open record or to the root. So a
    /// call in the input of a call (`mkfn!(made, add!(4; 5))`) and a macro whose code is only a
    /// call of the standard library (`set!` gives `vec![…]`, so it has no record) are in the
    /// chain.
    fn uses(&mut self, span: Span) {
        let ctxt = span.ctxt();
        let Some(top) = self.stack.last().copied() else { return };
        if ctxt.is_root() || !self.seen.insert((ctxt, top)) {
            return;
        }
        let mut expn = ctxt.outer_expn();
        for _ in 0..256 {
            if expn == ExpnId::root() || self.index.get(&expn).is_some_and(|index| self.stack.contains(index)) {
                break;
            }
            let data = expn.expn_data();
            let tcx = self.tcx;
            let entry = self
                .names
                .entry(expn)
                .or_insert_with(|| match data.kind {
                    ExpnKind::Macro(kind, name) if !standard(tcx, kind, data.macro_def_id) => Some([name.to_string(), definition(tcx, data.macro_def_id, data.def_site)]),
                    _ => None,
                })
                .clone();
            if let Some(entry) = entry {
                for at in self.stack.clone() {
                    if !self.open[at].uses.contains(&entry) {
                        self.open[at].uses.push(entry.clone());
                    }
                }
            }
            expn = data.call_site.ctxt().outer_expn();
        }
    }
}

impl<'a, 'tcx> Visitor<'a> for Walk<'tcx> {
    fn visit_item(&mut self, item: &'a rustc_ast::Item) {
        self.uses(item.span);
        let root = self.root(item.span);
        if let Some(index) = root {
            self.enter(index, Node::Item(item.clone()));
        }
        let saved = (self.container.clone(), self.self_ty.clone());
        let mut pushed = false;
        match &item.kind {
            rustc_ast::ItemKind::Mod(_, ident, _) => {
                self.module.push(ident.to_string());
                pushed = true;
            }
            // The header is a copy of the item with no member: only a block with a member from
            // an expansion needs it.
            rustc_ast::ItemKind::Impl(value) if value.items.iter().any(|inner| inner.span.from_expansion()) => {
                self.container = header(item);
                self.self_ty = rustc_ast_pretty::pprust::ty_to_string(&value.self_ty);
            }
            rustc_ast::ItemKind::Trait(value) if value.items.iter().any(|inner| inner.span.from_expansion()) => {
                self.container = header(item);
                self.self_ty = value.ident.to_string();
            }
            _ => {}
        }
        visit::walk_item(self, item);
        if pushed {
            self.module.pop();
        }
        (self.container, self.self_ty) = saved;
        if root.is_some() {
            self.stack.pop();
        }
    }

    fn visit_assoc_item(&mut self, item: &'a rustc_ast::AssocItem, ctxt: AssocCtxt) {
        self.uses(item.span);
        let root = self.root(item.span);
        if let Some(index) = root {
            self.enter(index, Node::Assoc(item.clone()));
        }
        visit::walk_assoc_item(self, item, ctxt);
        if root.is_some() {
            self.stack.pop();
        }
    }

    fn visit_stmt(&mut self, stmt: &'a rustc_ast::Stmt) {
        self.uses(stmt.span);
        let root = self.root(stmt.span);
        if let Some(index) = root {
            self.enter(index, Node::Stmt(stmt.clone()));
        }
        visit::walk_stmt(self, stmt);
        if root.is_some() {
            self.stack.pop();
        }
    }

    fn visit_expr(&mut self, expr: &'a rustc_ast::Expr) {
        self.uses(expr.span);
        if !self.stack.is_empty()
            && let Some(file) = self.included(expr)
        {
            self.read(file);
        }
        let root = self.root(expr.span);
        if let Some(index) = root {
            self.enter(index, Node::Expr(expr.clone()));
        }
        visit::walk_expr(self, expr);
        if root.is_some() {
            self.stack.pop();
        }
    }

    fn visit_pat(&mut self, pat: &'a rustc_ast::Pat) {
        self.uses(pat.span);
        let root = self.root(pat.span);
        if let Some(index) = root {
            self.enter(index, Node::Pat(pat.clone()));
        }
        visit::walk_pat(self, pat);
        if root.is_some() {
            self.stack.pop();
        }
    }

    fn visit_ty(&mut self, ty: &'a rustc_ast::Ty) {
        self.uses(ty.span);
        let root = self.root(ty.span);
        if let Some(index) = root {
            self.enter(index, Node::Ty(ty.clone()));
        }
        visit::walk_ty(self, ty);
        if root.is_some() {
            self.stack.pop();
        }
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// The header of an impl or of a trait: its text before the first `{`, with no attribute.
fn header(item: &rustc_ast::Item) -> String {
    let mut copy = item.clone();
    copy.attrs.clear();
    match &mut copy.kind {
        rustc_ast::ItemKind::Impl(value) => value.items.clear(),
        rustc_ast::ItemKind::Trait(value) => value.items.clear(),
        _ => {}
    }
    let text = rustc_ast_pretty::pprust::item_to_string(&copy);
    flat(text.split('{').next().unwrap_or(""))
}

fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Collects the identifiers of the expanded code in order, with their syntax contexts.
struct Idents(Vec<(Symbol, SyntaxContext)>);

impl<'a> Visitor<'a> for Idents {
    fn visit_ident(&mut self, ident: &'a rustc_span::Ident) {
        self.0.push((ident.name, ident.span.ctxt()));
    }
}

/// Gives a mark to each identifier whose name the code has with more than one syntax context
/// (R63.1-4): `x` and `x·1`. The first context of a name in the order of the text has no mark.
/// So a local of the macro and a local of the caller with one name have two texts.
struct Marks(HashMap<(Symbol, SyntaxContext), Symbol>);

impl MutVisitor for Marks {
    fn visit_ident(&mut self, ident: &mut rustc_span::Ident) {
        if let Some(name) = self.0.get(&(ident.name, ident.span.ctxt())) {
            ident.name = *name;
        }
    }
}

/// Puts a mark in place of each literal of the place of the call (R63.9-8). The evidence is
/// of the compiler: `line!()`, `column!()`, and `file!()` give their literal the span of the
/// call in the source that starts the chain (`top`), and a literal of the author has the span
/// of its own token. So a literal with the span of `top` is a line (an integer `N u32` equal to
/// the line of `top`), a column (equal to its column), or a file (a text). A literal of a
/// `concat!` of the standard library whose input calls one of them has a mark too. The mark is a
/// text with the character NUL, which no literal of a source file has.
struct Places<'tcx> {
    tcx: TyCtxt<'tcx>,
    top: Span,
    found: bool,
}

impl<'tcx> Places<'tcx> {
    fn mark(&self, expr: &rustc_ast::Expr) -> Option<String> {
        let rustc_ast::ExprKind::Lit(lit) = &expr.kind else { return None };
        if !self.top.is_dummy() && expr.span.lo() == self.top.lo() && expr.span.hi() == self.top.hi() {
            let at = self.tcx.sess.source_map().lookup_char_pos(self.top.lo());
            return match lit.kind {
                rustc_ast::token::LitKind::Integer if lit.suffix.is_some_and(|suffix| suffix.as_str() == "u32") => {
                    let value = lit.symbol.as_str();
                    if value == at.line.to_string() {
                        Some("line".to_string())
                    } else if value == (at.col_display + 1).to_string() {
                        Some("column".to_string())
                    } else {
                        None
                    }
                }
                // The text must be the name of the file of `top`, as `file!()` gives it.
                rustc_ast::token::LitKind::Str => {
                    let name = at.file.name.prefer_local_unconditionally().to_string();
                    (lit.symbol.as_str() == name).then(|| "file".to_string())
                }
                _ => None,
            };
        }
        self.concat(expr.span)
    }

    /// The input of a `concat!` of the standard library that made a span, if the input calls
    /// `line!()`, `column!()`, or `file!()`.
    fn concat(&self, span: Span) -> Option<String> {
        let ctxt = span.ctxt();
        if ctxt.is_root() {
            return None;
        }
        let data = ctxt.outer_expn().expn_data();
        let ExpnKind::Macro(kind, name) = data.kind else { return None };
        if !standard(self.tcx, kind, data.macro_def_id) || name.as_str() != "concat" {
            return None;
        }
        let input = self.tcx.sess.source_map().span_to_snippet(data.call_site).ok()?;
        ["line!", "column!", "file!"].iter().any(|word| input.contains(word)).then(|| flat(&input))
    }
}

impl<'tcx> MutVisitor for Places<'tcx> {
    fn visit_expr(&mut self, expr: &mut rustc_ast::Expr) {
        // A format string from `concat!` (`panic!(concat!("at ", file!(), ":", line!()))`): the
        // format arguments keep the span of the `concat!` literal.
        if let rustc_ast::ExprKind::FormatArgs(format) = &mut expr.kind
            && !format.is_source_literal
            && let Some(word) = self.concat(format.span)
        {
            self.found = true;
            format.template = vec![rustc_ast::FormatArgsPiece::Literal(Symbol::intern(&format!("\u{0}{word}\u{0}")))];
        }
        if let Some(word) = self.mark(expr) {
            self.found = true;
            expr.kind = rustc_ast::ExprKind::Lit(rustc_ast::token::Lit { kind: rustc_ast::token::LitKind::Str, symbol: Symbol::intern(&format!("\u{0}{word}\u{0}")), suffix: None });
            return;
        }
        mut_visit::walk_expr(self, expr);
    }
}

/// Collects the texts of the expressions of the expanded code (R63.1-5).
struct Texts<'m>(&'m mut HashMap<SpanKey, Option<String>>);

impl<'a, 'm> Visitor<'a> for Texts<'m> {
    fn visit_expr(&mut self, expr: &'a rustc_ast::Expr) {
        use rustc_ast::ExprKind;
        if expr.span.from_expansion() && matches!(expr.kind, ExprKind::Binary(..) | ExprKind::Unary(..) | ExprKind::AssignOp(..) | ExprKind::Index(..) | ExprKind::Let(..) | ExprKind::Path(..) | ExprKind::Lit(..) | ExprKind::Field(..) | ExprKind::Paren(..) | ExprKind::MethodCall(..) | ExprKind::Call(..) | ExprKind::Cast(..)) {
            let text = flat(&rustc_ast_pretty::pprust::expr_to_string(expr));
            if text.chars().count() <= POINT_TEXT {
                // The tokens of a procedural macro can share one span: a span with two texts has
                // no text (the point keeps its source text).
                match self.0.get(&key(expr.span)) {
                    Some(known) if known.as_deref() != Some(text.as_str()) => {
                        self.0.insert(key(expr.span), None);
                    }
                    Some(_) => {}
                    None => {
                        self.0.insert(key(expr.span), Some(text));
                    }
                }
            }
        }
        visit::walk_expr(self, expr);
    }
}

fn visit_node<'a, V: Visitor<'a>>(visitor: &mut V, node: &'a Node) {
    let _ = match node {
        Node::Item(item) => visitor.visit_item(item),
        Node::Assoc(item) => visitor.visit_assoc_item(item, AssocCtxt::Impl { of_trait: false }),
        Node::Stmt(stmt) => visitor.visit_stmt(stmt),
        Node::Expr(expr) => visitor.visit_expr(expr),
        Node::Pat(pat) => visitor.visit_pat(pat),
        Node::Ty(ty) => visitor.visit_ty(ty),
    };
}

fn mark_node<V: MutVisitor>(marks: &mut V, node: &mut Node) {
    match node {
        Node::Item(item) => mut_visit::walk_item(marks, item),
        Node::Assoc(item) => mut_visit::walk_assoc_item(marks, item, AssocCtxt::Impl { of_trait: false }),
        Node::Stmt(stmt) => {
            let mut copy = mut_visit::walk_flat_map_stmt(marks, stmt.clone());
            if let Some(first) = copy.pop() {
                *stmt = first;
            }
        }
        Node::Expr(expr) => marks.visit_expr(expr),
        Node::Pat(pat) => marks.visit_pat(pat),
        Node::Ty(ty) => marks.visit_ty(ty),
    }
}

fn print_node(node: &Node) -> String {
    use rustc_ast_pretty::pprust;
    match node {
        Node::Item(item) => pprust::item_to_string(item),
        Node::Assoc(item) => pprust::assoc_item_to_string(item),
        Node::Stmt(stmt) => pprust::stmt_to_string(stmt),
        Node::Expr(expr) => pprust::expr_to_string(expr),
        Node::Pat(pat) => pprust::pat_to_string(pat),
        Node::Ty(ty) => pprust::ty_to_string(ty),
    }
}

/// The definition of a macro: `src/lib.rs:2` for a macro of the workspace, else `crate NAME`.
/// The place comes from the definition span of the expansion: the query `def_span` of a local
/// definition needs the lowered tree, and the walk runs before the lowering.
fn definition(tcx: TyCtxt<'_>, def: Option<rustc_span::def_id::DefId>, def_site: Span) -> String {
    let Some(def) = def else { return "no definition".to_string() };
    let mut files = Vec::new();
    if crate::spans::range(tcx, def_site, &mut files).is_some() {
        let position = tcx.sess.source_map().lookup_char_pos(def_site.lo());
        return format!("{}:{}", files[0], position.line);
    }
    format!("crate {}", tcx.crate_name(def.krate))
}

/// The place of a span: the file, the byte range, the first and the last line and column.
fn place(tcx: TyCtxt<'_>, span: Span) -> (String, [i64; 2], usize, usize, usize, usize) {
    let mut files = Vec::new();
    let Some((_, start, end)) = crate::spans::range(tcx, span, &mut files) else { return (String::new(), [0, 0], 0, 0, 0, 0) };
    let map = tcx.sess.source_map();
    let (lo, hi) = (map.lookup_char_pos(span.lo()), map.lookup_char_pos(span.hi()));
    (files.remove(0), [start, end], lo.line, lo.col.0, hi.line, hi.col.0)
}

/// The text with the absolute path of the build folder as `$WORKSPACE` (R63.2-2). A macro that
/// reads `file!()` or `env!("CARGO_MANIFEST_DIR")` (`snapbox::str!`) puts the path of the folder
/// in its code. The two trees of a step build in two folders, so the path is not code of the
/// author. Cargo starts the compiler in the workspace root.
fn stable_paths(text: &str) -> String {
    let Ok(root) = std::env::current_dir() else { return text.to_string() };
    let root = root.to_string_lossy().into_owned();
    if root.len() < 2 || !text.contains(&root) {
        return text.to_string();
    }
    text.replace(&root, "$WORKSPACE")
}

/// Reads the expansion of each macro call (R63.1-1). Call it after the macro expansion.
pub fn collect(tcx: TyCtxt<'_>) -> Vec<Expansion> {
    let (_, krate) = tcx.resolver_for_lowering();
    let krate = krate.borrow();
    let mut walk = Walk { tcx, open: Vec::new(), index: HashMap::new(), stack: Vec::new(), module: Vec::new(), container: String::new(), self_ty: String::new(), names: HashMap::new(), seen: Default::default(), data_files: None };
    visit::walk_crate(&mut walk, &krate);
    let crate_name = tcx.crate_name(rustc_span::def_id::LOCAL_CRATE).to_string();
    let mut texts: HashMap<SpanKey, Option<String>> = HashMap::new();
    let index = walk.index.clone();
    let mut out: Vec<Expansion> = Vec::with_capacity(walk.open.len());
    for mut open in walk.open {
        let data = open.expn.expn_data();
        let ExpnKind::Macro(kind, name) = data.kind else { continue };
        // The marks of the identifiers (R63.1-4).
        let mut idents = Idents(Vec::new());
        for node in &open.nodes {
            visit_node(&mut idents, node);
        }
        let mut contexts: HashMap<Symbol, Vec<SyntaxContext>> = HashMap::new();
        for (name, ctxt) in idents.0 {
            let list = contexts.entry(name).or_default();
            if !list.contains(&ctxt) {
                list.push(ctxt);
            }
        }
        let mut marks = HashMap::new();
        for (name, list) in contexts {
            let text = name.as_str();
            if list.len() < 2 || text.starts_with('\'') || rustc_span::Ident::with_dummy_span(name).is_reserved() || text.is_empty() {
                continue;
            }
            for (number, ctxt) in list.iter().enumerate().skip(1) {
                marks.insert((name, *ctxt), Symbol::intern(&format!("{text}\u{b7}{number}")));
            }
        }
        if !marks.is_empty() {
            let mut marks = Marks(marks);
            for node in &mut open.nodes {
                mark_node(&mut marks, node);
            }
        }
        for node in &open.nodes {
            visit_node(&mut Texts(&mut texts), node);
        }
        let form = match open.nodes.first() {
            Some(Node::Item(_)) => "items",
            Some(Node::Assoc(_)) => "assoc",
            Some(Node::Stmt(_)) => "stmts",
            Some(Node::Expr(_)) => "expr",
            Some(Node::Pat(_)) => "pat",
            Some(Node::Ty(_)) => "ty",
            None => continue,
        };
        let module = |name: &str| if open.module.is_empty() { format!("{crate_name}::{name}") } else { format!("{crate_name}::{}::{name}", open.module) };
        let mut items = Vec::new();
        for node in &open.nodes {
            match node {
                Node::Item(item) => match (&item.kind, item.kind.ident()) {
                    (rustc_ast::ItemKind::Impl(_), _) => items.push(header(item)),
                    (_, Some(ident)) => items.push(module(ident.as_str())),
                    _ => {}
                },
                Node::Assoc(item) => {
                    if let Some(ident) = item.kind.ident() {
                        items.push(module(&format!("{}::{ident}", open.self_ty)));
                    }
                }
                _ => {}
            }
        }
        let separator = if form == "items" || form == "assoc" { "\n\n" } else { "\n" };
        let text: Vec<String> = open.nodes.iter().map(print_node).collect();
        let text = stable_paths(&text.join(separator).replace("$crate", "crate"));
        // R63.9-8: the text with a mark for each literal of the place of the call.
        let placeless = {
            let mut places = Places { tcx, top: data.call_site.source_callsite(), found: false };
            let mut copy = open.nodes.clone();
            for node in &mut copy {
                mark_node(&mut places, node);
            }
            if places.found {
                let text: Vec<String> = copy.iter().map(print_node).collect();
                stable_paths(&text.join(separator).replace("$crate", "crate"))
            } else {
                String::new()
            }
        };
        let call_site = data.call_site;
        // The expansion that has this call in its code: the nearest one with a record.
        let mut outer = None;
        let mut current = call_site;
        while current.from_expansion() {
            let parent = current.ctxt().outer_expn();
            if let Some(found) = index.get(&parent) {
                outer = Some(*found);
                break;
            }
            current = parent.expn_data().call_site;
        }
        // R63.9-3: a call whose tokens come from an expansion with no record (a macro that a
        // macro defines: `sum!(1, 2)` makes `make!{@inner …}`) is an outer call. Its place is
        // the place of the call in a file that makes it (the call site in the source), and
        // `via` has the text of that call. A call inside an expansion with a record keeps the
        // place of its tokens.
        let mut via = String::new();
        let at = if call_site.from_expansion() && outer.is_none() {
            let source = call_site.source_callsite();
            via = flat(&tcx.sess.source_map().span_to_snippet(source).unwrap_or_default());
            if via.chars().count() > 200 {
                via = via.chars().take(200).collect::<String>() + "…";
            }
            source
        } else if call_site.from_expansion() {
            call_site
        } else {
            call_site.source_callsite()
        };
        let (file, r, line, col, end_line, end_col) = place(tcx, at);
        let mut call = flat(&tcx.sess.source_map().span_to_snippet(call_site).unwrap_or_default());
        if call.chars().count() > 200 {
            call = call.chars().take(200).collect::<String>() + "…";
        }
        out.push(Expansion {
            file,
            r,
            line,
            col,
            end_line,
            end_col,
            outer,
            name: name.to_string(),
            kind: match kind {
                rustc_span::hygiene::MacroKind::Bang => "bang",
                rustc_span::hygiene::MacroKind::Attr => "attr",
                rustc_span::hygiene::MacroKind::Derive => "derive",
            },
            def: definition(tcx, data.macro_def_id, data.def_site),
            form,
            container: if form == "assoc" { open.container.clone() } else { String::new() },
            call,
            text,
            items,
            uses: std::mem::take(&mut open.uses),
            module: open.module.clone(),
            via,
            reads: std::mem::take(&mut open.reads),
            placeless,
        });
    }
    if let Ok(mut shared) = TEXTS.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        shared.extend(texts);
    }
    out
}
