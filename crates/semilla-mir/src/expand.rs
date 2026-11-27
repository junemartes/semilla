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
//! Each macro call has a record (R65.2-1, batch 10n): also a macro of the standard library
//! (`format!`, `println!`, `vec!`, `assert!`, `#[test]`). Before batch 10n, a name list of
//! crates (R63.2-3) gave them no record.
//!
//! The origin of each token (R65.1-1): a token of the code comes from a definition of the
//! chain, from the input of the call, or from the context of the call (a value that the
//! compiler or a proc macro makes at the call). The span and the expansion data of the
//! compiler give it: a `macro_rules!` macro only copies tokens (each token keeps the span of
//! its definition or of the input), and a built-in macro or a proc macro makes new tokens. A
//! made token whose text is not the source text at its span is a token of the context.

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
    /// The text with a mark for each token of the context of the call (R65.1-1): a value that
    /// the compiler or a proc macro makes at the call (`line!()`, `module_path!()`,
    /// `env!(..)`, a read file, the output of a proc macro). Empty if the code has no such
    /// token.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub placeless: String,
    /// The tokens of the context, in the order of the text: the source and the value (R65.1-2).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<[String; 2]>,
    /// The identity of each definition of the chain that the facts of the step cannot show
    /// (R65.1-3): a macro of a crate outside the workspace (`str: crate snapbox 1a2b…`) and a
    /// proc macro. A `macro_rules!` macro of the workspace has no entry: a fact shows its change.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
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
    /// The identities of the definitions of the chain (R65.1-3).
    ids: Vec<String>,
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

/// True for a macro of a crate of the standard library (the sysroot of the tool chain, a fact of
/// the compiler). The text of a point of the graph does not come from its expansion
/// (R60.2-12: a panic macro is one point with the text of the call).
fn from_sysroot(tcx: TyCtxt<'_>, def: Option<rustc_span::def_id::DefId>) -> bool {
    def.is_some_and(|def| crate::stdlib::is_sysroot_crate(tcx, def.krate))
}

/// How a macro makes its code (R65.1-1): a `macro_rules!` macro copies tokens of its definition
/// and of its input; a built-in macro (`#[rustc_builtin_macro]`) and a proc macro make tokens.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Maker {
    Rules,
    Builtin,
    Proc,
    Unknown,
}

fn maker(tcx: TyCtxt<'_>, def: Option<rustc_span::def_id::DefId>, name: Symbol) -> Maker {
    let Some(def) = def else { return Maker::Unknown };
    // A macro of this crate is a `macro_rules!` macro: a crate cannot use its own proc macros,
    // and the attributes of a local item need the lowered tree, which the walk cannot read.
    if def.is_local() {
        return Maker::Rules;
    }
    if crate::config::builtin_name(tcx, def, name).is_some() {
        Maker::Builtin
    } else if crate::config::is_proc_macro(tcx, def) {
        Maker::Proc
    } else {
        Maker::Rules
    }
}

/// The identity of a definition that no fact of the step can show (R65.1-3). A `macro_rules!`
/// macro of a crate outside the workspace: the hash of the source text of its definition (its
/// code is only that text; a macro that it calls has its own identity in the chain). A proc
/// macro: the hash of its crate (the compiler changes the hash with each change of the crate,
/// and also with a change of the build of a crate below it, which is the safe side). The crate
/// hash is not the identity of a `macro_rules!` macro: a changed feature of a different crate
/// changes it (`snapbox` in the two trees of `winnow`). `None` for a `macro_rules!` macro of the
/// workspace: a fact of the step shows its change.
fn identity(tcx: TyCtxt<'_>, def: Option<rustc_span::def_id::DefId>, name: Symbol, def_text: &str, def_site: Span) -> Option<String> {
    let Some(def) = def else { return Some(format!("{name}: no definition")) };
    let kind = maker(tcx, Some(def), name);
    if kind == Maker::Rules && !def_text.starts_with("crate ") {
        return None;
    }
    if def.is_local() {
        return Some(format!("{name}: a macro of this crate that is not `macro_rules!`"));
    }
    if kind != Maker::Proc
        && !def_site.is_dummy()
        && let Ok(text) = tcx.sess.source_map().span_to_snippet(def_site)
        && !text.trim().is_empty()
    {
        return Some(format!("{name}: macro of crate {} (definition {:016x})", tcx.crate_name(def.krate), hash_bytes(text.as_bytes())));
    }
    // The crate is never the local crate here (the test above returns for a local macro): the
    // compiler has the hash of another crate from its metadata, but no hash of the local crate
    // before it writes the metadata (R66.10-1).
    let word = if kind == Maker::Proc { "proc macro" } else { "macro" };
    Some(format!("{name}: {word} of crate {} {}", tcx.crate_name(def.krate), tcx.crate_hash(def.krate)))
}

struct Walk<'tcx> {
    tcx: TyCtxt<'tcx>,
    open: Vec<Open>,
    index: HashMap<ExpnId, usize>,
    stack: Vec<usize>,
    module: Vec<String>,
    container: String,
    self_ty: String,
    /// The name, the definition, and the identity (R65.1-3) of each macro expansion.
    names: HashMap<ExpnId, Option<([String; 2], Option<String>)>>,
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
        let ExpnKind::Macro(..) = data.kind else { return None };
        if let Some(index) = self.index.get(&expn)
            && self.stack.contains(index)
        {
            return None;
        }
        let index = *self.index.entry(expn).or_insert_with(|| {
            self.open.push(Open { expn, nodes: Vec::new(), module: self.module.join("::"), container: self.container.clone(), self_ty: self.self_ty.clone(), uses: Vec::new(), reads: Vec::new(), ids: Vec::new() });
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

    /// R73.1-1: a node with no syntax context inside an open record whose file is not the file
    /// of the source call of that record is code that `include!` reads (or a module file that the
    /// code of the call loads): the parser gives each token the span of its file and no mark of
    /// the expansion. The file is a read file of each such record. A record whose source call is
    /// in the file of the node (a call inside the included code) does not read it. A file outside
    /// the workspace keeps its absolute path (an input that Semilla does not see).
    fn included_code(&mut self, span: Span) {
        if self.stack.is_empty() || span.is_dummy() || !span.ctxt().is_root() {
            return;
        }
        let map = self.tcx.sess.source_map();
        let file = map.lookup_source_file(span.lo());
        let rustc_span::FileName::Real(name) = &file.name else { return };
        let Some(local) = name.local_path() else { return };
        let path = crate::workspace_path(local);
        if path.is_empty() {
            return;
        }
        for at in self.stack.clone() {
            let call = self.open[at].expn.expn_data().call_site.source_callsite();
            if call.is_dummy() || map.lookup_source_file(call.lo()).name == file.name {
                continue;
            }
            if !self.open[at].reads.contains(&path) {
                self.open[at].reads.push(path.clone());
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
                    ExpnKind::Macro(_, name) => {
                        let def = definition(tcx, data.macro_def_id, data.def_site);
                        let id = identity(tcx, data.macro_def_id, name, &def, data.def_site);
                        Some(([name.to_string(), def], id))
                    }
                    _ => None,
                })
                .clone();
            if let Some((entry, id)) = entry {
                for at in self.stack.clone() {
                    if !self.open[at].uses.contains(&entry) {
                        self.open[at].uses.push(entry.clone());
                    }
                    if let Some(id) = &id
                        && !self.open[at].ids.contains(id)
                    {
                        self.open[at].ids.push(id.clone());
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
        self.included_code(item.span);
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
                self.self_ty = crate::pretty::ty(&value.self_ty).unwrap_or_else(|cause| crate::pretty::not_known(&cause));
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
        self.included_code(item.span);
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
        self.included_code(stmt.span);
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
        self.included_code(expr.span);
        let root = self.root(expr.span);
        if let Some(index) = root {
            self.enter(index, Node::Expr(expr.clone()));
        }
        // After `enter`: the record of `include_str!` itself reads the file too (R65.2-1).
        if !self.stack.is_empty()
            && let Some(file) = self.included(expr)
        {
            self.read(file);
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
    match crate::pretty::item(&copy) {
        Ok(text) => flat(text.split('{').next().unwrap_or("")),
        Err(cause) => crate::pretty::not_known(&cause),
    }
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

/// Puts a mark in place of each token of the context of the call (R65.1-1, R65.1-2). The
/// evidence is of the compiler:
///
/// - `line!()`, `column!()`, and `file!()` give their literal the span of the call in the source
///   that starts the chain (`top`); a literal of the author has the span of its own token;
/// - a built-in macro (`#[rustc_builtin_macro]`) and a proc macro make new tokens; a token that
///   they copy from their input or from a definition keeps its span, so its text is the source
///   text at its span. A made token has a span whose source text is not its text;
/// - a `macro_rules!` macro makes no token: it copies the tokens of its definition and of its
///   input.
///
/// The mark is a text with the character NUL (no literal of a source file has it) and the
/// source of the token. `context` gets the source and the value of each mark, in the order of
/// the text.
struct Origins<'tcx> {
    tcx: TyCtxt<'tcx>,
    top: Span,
    /// The module path of the call (`shop::a`), for the source "the module path of the call".
    module: String,
    /// The values of the environment variables that the compiler read (`env!`, `option_env!`).
    env: Vec<(String, Option<String>)>,
    /// The files that the compiler read (`include_str!`, `include_bytes!`, a proc macro).
    read: Vec<String>,
    /// The data files of the workspace by the hash of their bytes (`include_bytes!` gives bytes
    /// with the span of the call).
    data_files: HashMap<u64, String>,
    /// The bytes of the expression that `origin` reads now (`include_bytes!`).
    bytes: Option<Vec<u8>>,
    context: Vec<[String; 2]>,
}

impl<'tcx> Origins<'tcx> {
    /// The source of a value that the compiler or a macro makes (R65.1-2). `span` is the span
    /// of the made token and `value` its printed text. `maker` is the macro that made it, with
    /// its name and its call site; `None` for a literal with the span of the call in the source
    /// and no expansion data (`line!()`, `column!()`, `file!()`). The source of a made token
    /// ends with the text of the call of its macro: a change of that text (a changed
    /// definition) changes the mark, so the mark never hides it.
    fn source(&self, span: Span, value: &str, maker: Option<(Maker, Symbol, Span)>) -> String {
        let map = self.tcx.sess.source_map();
        let quoted = value.starts_with('"') && value.ends_with('"') && value.len() >= 2;
        let text = if quoted { &value[1..value.len() - 1] } else { value };
        let call = maker.map(|(_, _, call)| flat(&map.span_to_snippet(call).unwrap_or_default()).replace("$crate::", "")).filter(|call| !call.is_empty());
        let with_call = |source: String| match &call {
            Some(call) if call.chars().count() <= 120 => format!("{source} (`{call}`)"),
            Some(_) => format!("{source} (a call of `{}`)", maker.map(|(_, name, _)| name.to_string()).unwrap_or_default()),
            None => source,
        };
        // A string with the value of an environment variable that the compiler read.
        if quoted && let Some((variable, _)) = self.env.iter().find(|(_, known)| known.as_deref() == Some(text)) {
            return with_call(format!("the environment variable `{variable}`"));
        }
        // A call whose input names a variable that the compiler read (`option_env!("X")` gives
        // `None` or `Some("…")`).
        if let Some(call) = &call
            && let Some((variable, _)) = self.env.iter().find(|(name, _)| call.contains(&format!("\"{name}\"")))
        {
            return with_call(format!("the environment variable `{variable}`"));
        }
        if quoted && !self.module.is_empty() && text == self.module {
            return with_call("the module path of the call".to_string());
        }
        // `include_bytes!` gives bytes with the span of the call: the file of the workspace with
        // the same bytes (the compiler adds each file that it reads to the source map).
        if let Some(path) = value.strip_prefix("b\"").and_then(|_| self.bytes_file(span)) {
            return with_call(format!("the file `{path}`"));
        }
        // A literal of a read file has the span of the file (`include_str!`).
        if !span.is_dummy() {
            let file = map.lookup_source_file(span.lo());
            if let rustc_span::FileName::Real(real) = &file.name
                && let Some(path) = real.local_path()
            {
                let full = path.to_string_lossy().into_owned();
                if self.read.iter().any(|known| *known == full || std::path::Path::new(known) == path) {
                    return with_call(format!("the file `{}`", crate::workspace_path(path)));
                }
            }
        }
        // A number or a text of the place of the call: the call in the source, the call of the
        // macro, or the made token.
        let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
        let suffix_only = text[digits.len()..].chars().all(|c| c.is_ascii_alphanumeric());
        let call_span = maker.map(|(_, _, call)| call).unwrap_or(self.top);
        for place in [self.top, call_span, span].iter().filter(|place| !place.is_dummy()) {
            let at = map.lookup_char_pos(place.lo());
            if !quoted && !digits.is_empty() && suffix_only {
                if digits == at.line.to_string() {
                    return with_call("the line of the call".to_string());
                }
                if digits == (at.col_display + 1).to_string() || digits == at.col_display.to_string() {
                    return with_call("the column of the call".to_string());
                }
            }
            if quoted && text == at.file.name.prefer_local_unconditionally().to_string() {
                return with_call("the file of the call".to_string());
            }
        }
        match maker {
            Some((Maker::Proc, name, _)) => format!("the output of the proc macro `{name}`"),
            Some(_) => with_call("a value that the macro makes at the call".to_string()),
            None => "a value that the compiler makes at the place of the call".to_string(),
        }
    }

    /// The data file whose bytes a made byte string at `span` has (`include_bytes!`).
    fn bytes_file(&self, span: Span) -> Option<String> {
        let _ = span;
        let bytes = self.bytes.as_ref()?;
        self.data_files.get(&hash_bytes(bytes)).cloned()
    }

    /// True if each expression inside `expr` is a token of the context too.
    fn fully_made(&self, expr: &rustc_ast::Expr) -> bool {
        struct Parts<'o, 'tcx>(&'o Origins<'tcx>, bool);
        impl<'a, 'o, 'tcx> Visitor<'a> for Parts<'o, 'tcx> {
            fn visit_expr(&mut self, expr: &'a rustc_ast::Expr) {
                if self.1 && self.0.origin(expr).is_none() {
                    self.1 = false;
                }
                if self.1 {
                    visit::walk_expr(self, expr);
                }
            }
        }
        let mut parts = Parts(self, true);
        visit::walk_expr(&mut parts, expr);
        parts.1
    }

    /// The mark of a token of the context in the text with the marks (R65.1-1): the maker and the
    /// text of its call, or the place of the call. It has no part that the value decides (the
    /// source "the line of the call" comes from a compare of the value with the place), so two
    /// calls whose tokens of the context have other values have equal marks. The text of the
    /// call is in the mark: a changed definition that changes it changes the mark.
    fn mark_of(&self, span: Span, maker: Option<(Maker, Symbol, Span)>) -> String {
        let map = self.tcx.sess.source_map();
        if !span.is_dummy() {
            let file = map.lookup_source_file(span.lo());
            if let rustc_span::FileName::Real(real) = &file.name
                && let Some(path) = real.local_path()
            {
                let full = path.to_string_lossy().into_owned();
                if self.read.iter().any(|known| *known == full || std::path::Path::new(known) == path) {
                    return format!("the file `{}`", crate::workspace_path(path));
                }
            }
        }
        match maker {
            None => "the place of the call".to_string(),
            Some((Maker::Proc, name, _)) => format!("made by the proc macro `{name}`"),
            Some((_, name, call)) => {
                let snippet = flat(&map.span_to_snippet(call).unwrap_or_default()).replace("$crate::", "");
                if snippet.is_empty() { format!("made by `{name}`") } else { format!("made by `{snippet}`") }
            }
        }
    }

    /// The source of an expression of the context, or `None` for a token of a definition or of
    /// the input (R65.1-1).
    fn origin(&self, expr: &rustc_ast::Expr) -> Option<(String, String)> {
        let span = expr.span;
        if span.is_dummy() {
            return None;
        }
        let snippet = flat(&self.tcx.sess.source_map().span_to_snippet(span).unwrap_or_default());
        if let rustc_ast::ExprKind::IncludedBytes(bytes) = &expr.kind {
            let mut copy = Origins { tcx: self.tcx, top: self.top, module: self.module.clone(), env: self.env.clone(), read: self.read.clone(), data_files: self.data_files.clone(), bytes: Some(bytes.as_byte_str().to_vec()), context: Vec::new() };
            copy.bytes = Some(bytes.as_byte_str().to_vec());
            return copy.origin_inner(expr, snippet);
        }
        self.origin_inner(expr, snippet)
    }

    fn origin_inner(&self, expr: &rustc_ast::Expr, snippet: String) -> Option<(String, String)> {
        let span = expr.span;
        // R66.11-1: an expression that the driver cannot print has no known origin. It stays in
        // the text, so the text of its node is not known either. The print runs only when the
        // origin needs the value.
        let printed = || crate::pretty::expr(expr).ok().map(|text| flat(&text));
        let snippet = || snippet.clone();
        // A literal with the span of the call in the source and a different text: the compiler
        // made it for the place of the call (`line!()`, `column!()`, `file!()`).
        let top_literal = matches!(expr.kind, rustc_ast::ExprKind::Lit(_)) && !self.top.is_dummy() && span.lo() == self.top.lo() && span.hi() == self.top.hi();
        let ctxt = span.ctxt();
        let made_by = if ctxt.is_root() {
            None
        } else {
            let data = ctxt.outer_expn_data();
            match data.kind {
                ExpnKind::Macro(_, name) => Some((maker(self.tcx, data.macro_def_id, name), name, data.call_site)),
                _ => None,
            }
        };
        match made_by {
            // A `macro_rules!` macro copies tokens: a token of its code comes from its
            // definition or from its input.
            Some((Maker::Rules, ..)) | None if !top_literal => None,
            Some((Maker::Rules, ..)) | None => {
                let value = printed()?;
                (snippet() != value).then(|| (self.source(span, &value, None), self.mark_of(span, None)))
            }
            Some(found) => {
                let value = printed()?;
                (snippet() != value).then(|| (self.source(span, &value, Some(found)), self.mark_of(span, Some(found))))
            }
        }
    }
}

impl<'tcx> MutVisitor for Origins<'tcx> {
    fn visit_expr(&mut self, expr: &mut rustc_ast::Expr) {
        // A format string from a macro (`panic!(concat!("at ", file!(), ":", line!()))`): the
        // format arguments keep the span of the made literal. The arguments are visited.
        if let rustc_ast::ExprKind::FormatArgs(format) = &mut expr.kind {
            if !format.is_source_literal && !format.span.ctxt().is_root() {
                let data = format.span.ctxt().outer_expn_data();
                if let ExpnKind::Macro(_, name) = data.kind {
                    let kind = maker(self.tcx, data.macro_def_id, name);
                    if kind != Maker::Rules {
                        let value: String = format
                            .template
                            .iter()
                            .map(|piece| match piece {
                                rustc_ast::FormatArgsPiece::Literal(text) => text.to_string(),
                                rustc_ast::FormatArgsPiece::Placeholder(_) => "{}".to_string(),
                            })
                            .collect();
                        let source = self.source(format.span, &format!("\"{value}\""), Some((kind, name, data.call_site)));
                        let mark = self.mark_of(format.span, Some((kind, name, data.call_site)));
                        format.template = vec![rustc_ast::FormatArgsPiece::Literal(Symbol::intern(&format!("\u{0}{mark}\u{0}")))];
                        self.context.push([source, cut(&stable_paths(&value))]);
                    }
                }
            }
            mut_visit::walk_expr(self, expr);
            return;
        }
        // A made expression with a token of the input or of a definition inside it (a proc
        // macro that makes `(1, 4u32)` from the input `1`) is not one mark: its parts are.
        if let Some((source, mark)) = self.origin(expr)
            && self.fully_made(expr)
        {
            let value = cut(&stable_paths(&flat(&crate::pretty::expr(expr).unwrap_or_else(|cause| crate::pretty::not_known(&cause)))));
            expr.kind = rustc_ast::ExprKind::Lit(rustc_ast::token::Lit { kind: rustc_ast::token::LitKind::Str, symbol: Symbol::intern(&format!("\u{0}{mark}\u{0}")), suffix: None });
            self.context.push([source, value]);
            return;
        }
        mut_visit::walk_expr(self, expr);
        keep_else_block(expr);
    }
}

/// A value of the context on one line, at most 200 characters. A longer value ends with the hash
/// of its full text, so two values with an equal start are not equal (R65.1-4 compares them).
fn cut(text: &str) -> String {
    if text.chars().count() > 200 { format!("{}… (hash {:016x})", text.chars().take(200).collect::<String>(), hash_bytes(text.as_bytes())) } else { text.to_string() }
}

/// Collects the texts of the expressions of the expanded code (R63.1-5).
struct Texts<'m>(&'m mut HashMap<SpanKey, Option<String>>);

impl<'a, 'm> Visitor<'a> for Texts<'m> {
    fn visit_expr(&mut self, expr: &'a rustc_ast::Expr) {
        use rustc_ast::ExprKind;
        if expr.span.from_expansion() && matches!(expr.kind, ExprKind::Binary(..) | ExprKind::Unary(..) | ExprKind::AssignOp(..) | ExprKind::Index(..) | ExprKind::Let(..) | ExprKind::Path(..) | ExprKind::Lit(..) | ExprKind::Field(..) | ExprKind::Paren(..) | ExprKind::MethodCall(..) | ExprKind::Call(..) | ExprKind::Cast(..)) {
            // R66.11-1: an expression that the driver cannot print gives no text (the point keeps
            // its source text, as a span with two texts).
            let text = match crate::pretty::expr(expr) {
                Ok(text) => flat(&text),
                Err(_) => {
                    self.0.insert(key(expr.span), None);
                    visit::walk_expr(self, expr);
                    return;
                }
            };
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

/// The text of a node (R63.1-1). A node that the printer of the compiler cannot print gets a
/// text that is never equal to another text, with the cause (R66.11-1): the expansion is not
/// known, and it never gives a proof.
fn print_node(node: &Node) -> String {
    use crate::pretty;
    let text = match node {
        Node::Item(item) => pretty::item(item),
        Node::Assoc(item) => pretty::assoc_item(item),
        Node::Stmt(stmt) => pretty::stmt(stmt),
        Node::Expr(expr) => pretty::expr(expr),
        Node::Pat(pat) => pretty::pat(pat),
        Node::Ty(ty) => pretty::ty(ty),
    };
    text.unwrap_or_else(|cause| pretty::not_known(&cause))
}

/// R66.11-2: a mark of the context keeps the `else` of an `if` a block. The printer of the
/// compiler asserts that an `else` is a block or an `if`, and a mark (a literal) in place of a
/// made `else` block broke that form: the print panicked, and the drop of its boxes aborted the
/// driver. The marked `else` becomes a block with the mark (`else { "…" }`).
fn keep_else_block(expr: &mut rustc_ast::Expr) {
    use rustc_ast::ExprKind;
    if let ExprKind::If(_, _, Some(alternative)) = &mut expr.kind
        && !matches!(alternative.kind, ExprKind::If(..) | ExprKind::Block(_, None))
    {
        let span = alternative.span;
        let inner = (**alternative).clone();
        let mut block = rustc_ast::Block { stmts: Default::default(), id: rustc_ast::DUMMY_NODE_ID, rules: rustc_ast::BlockCheckMode::Default, span };
        block.stmts.push(rustc_ast::Stmt { id: rustc_ast::DUMMY_NODE_ID, kind: rustc_ast::StmtKind::Expr(Box::new(inner)), span });
        alternative.kind = ExprKind::Block(Box::new(block), None);
        alternative.attrs = Default::default();
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
    let root = crate::current_folder().to_string_lossy().into_owned();
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
    // The environment variables and the files that the compiler read (R65.1-2).
    let env: Vec<(String, Option<String>)> = tcx.sess.env_depinfo.lock().iter().map(|(name, value)| (name.to_string(), value.map(|value| value.to_string()))).collect();
    let read: Vec<String> = tcx.sess.file_depinfo.lock().iter().map(|path| path.to_string()).collect();
    let index = walk.index.clone();
    let data_files = walk.data_files.take().unwrap_or_default();
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
        // R60.2-12: a point of the graph in a macro of the standard library keeps the text of
        // the call, so the texts of its expansion are not kept.
        if !from_sysroot(tcx, data.macro_def_id) {
            for node in &open.nodes {
                visit_node(&mut Texts(&mut texts), node);
            }
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
        // R65.1-1: the text with a mark for each token of the context of the call.
        let (placeless, context) = {
            let module = if open.module.is_empty() { crate_name.clone() } else { format!("{crate_name}::{}", open.module) };
            let mut origins = Origins { tcx, top: data.call_site.source_callsite(), module, env: env.clone(), read: read.clone(), data_files: data_files.clone(), bytes: None, context: Vec::new() };
            let mut copy = open.nodes.clone();
            for node in &mut copy {
                mark_node(&mut origins, node);
            }
            if origins.context.is_empty() {
                (String::new(), Vec::new())
            } else {
                let text: Vec<String> = copy.iter().map(print_node).collect();
                let context = origins.context.into_iter().map(|[source, value]| [source, stable_paths(&value)]).collect();
                (stable_paths(&text.join(separator).replace("$crate", "crate")), context)
            }
        };
        // R65.1-3: the identity of the macro of the call, then of each macro of its chain.
        let def_text = definition(tcx, data.macro_def_id, data.def_site);
        let mut ids: Vec<String> = identity(tcx, data.macro_def_id, name, &def_text, data.def_site).into_iter().collect();
        for id in std::mem::take(&mut open.ids) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
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
            def: def_text,
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
            context,
            ids,
        });
    }
    if let Ok(mut shared) = TEXTS.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        shared.extend(texts);
    }
    out
}
