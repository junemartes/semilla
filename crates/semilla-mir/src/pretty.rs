//! The text of a syntax tree from the pretty printer of the compiler, after a check of the tree
//! (R66.11-1, batch 10nq-fix2).
//!
//! The printer (`rustc_ast_pretty`) has asserts on some forms of the tree. A failed assert
//! panics while the printer has open boxes, and each open box (`BoxMarker`) panics again in its
//! drop during the unwind. A panic in a drop during an unwind aborts the process, so
//! `catch_unwind` (`guarded`) cannot catch it: the build of the step failed for code that
//! compiles. So the driver never calls the printer on a tree that it did not check first. Each
//! call of the printer is in this file (a source scan test checks it).
//!
//! The forms that the printer of the tool chain `nightly-2026-10-25` refuses (design 66.11):
//!
//! - an `if` whose `else` is not a block with no label and not an `if` (`print_else`: "print_if
//!   saw if with weird alternative");
//! - a missing pattern (`PatKind::Missing`) inside a tree, except the pattern of a parameter of
//!   a function (`print_pat`: `unreachable!`); the printer prints no pattern for such a
//!   parameter (`fn(u8)`), and a missing pattern alone (a node of an expansion) has the empty
//!   text; a parameter of a closure with no pattern;
//! - a lifetime parameter or a lifetime `where` bound with a bound that is not a lifetime
//!   (`print_lifetime_bounds`);
//! - a field of a struct with braces with no name (`print_variant_struct`: `field.ident.unwrap()`);
//! - a qualified path whose position is past the last segment (`print_qpath`: a slice index);
//! - a placeholder of `asm!` that names an operand that does not exist (`inline_asm_template_and_operands`);
//! - a tree deeper than `MAX_DEPTH` nodes: the printer has no guard of its stack, and a stack
//!   overflow aborts the process too.

use std::ops::ControlFlow;

use rustc_ast::visit::{self, AssocCtxt, Visitor};
use rustc_ast::{ExprKind, GenericBound, GenericParamKind, PatKind, TyKind, VariantData, WherePredicateKind};

/// The deepest nesting of nodes that the driver checks and prints (Decided by Claude). The thread
/// of the compiler has 8 MiB of stack. The printer of this tool chain printed 32000 levels of
/// parentheses in 8 MiB; the check (a debug build) needs more than 2 KiB for each level. 1000
/// levels keep the two far from the end of the stack.
pub(crate) const MAX_DEPTH: usize = 1000;

/// The check of a tree before a print. It stops at the first form that the printer refuses.
struct Check {
    depth: usize,
}

impl Check {
    fn deeper(&mut self, walk: impl FnOnce(&mut Self) -> ControlFlow<&'static str>) -> ControlFlow<&'static str> {
        if self.depth >= MAX_DEPTH {
            return ControlFlow::Break("a tree deeper than the limit of the driver");
        }
        self.depth += 1;
        let found = walk(self);
        self.depth -= 1;
        found
    }
}

/// True if a qualified path has its position in its segments.
fn qself_fits(qself: &Option<Box<rustc_ast::QSelf>>, path: &rustc_ast::Path) -> bool {
    qself.as_ref().is_none_or(|qself| qself.position <= path.segments.len())
}

/// The cause of a lifetime bound list with a bound that is not a lifetime.
fn lifetime_bounds(bounds: &[GenericBound]) -> ControlFlow<&'static str> {
    if bounds.iter().any(|bound| !matches!(bound, GenericBound::Outlives(_))) {
        return ControlFlow::Break("a lifetime bound that is not a lifetime");
    }
    ControlFlow::Continue(())
}

impl<'a> Visitor<'a> for Check {
    type Result = ControlFlow<&'static str>;

    fn visit_expr(&mut self, expr: &'a rustc_ast::Expr) -> Self::Result {
        match &expr.kind {
            ExprKind::If(_, _, Some(alternative)) if !matches!(alternative.kind, ExprKind::If(..) | ExprKind::Block(_, None)) => {
                return ControlFlow::Break("an `else` that is not a block");
            }
            ExprKind::Path(qself, path) if !qself_fits(qself, path) => return ControlFlow::Break("a qualified path with no segment at its position"),
            ExprKind::Struct(found) if !qself_fits(&found.qself, &found.path) => return ControlFlow::Break("a qualified path with no segment at its position"),
            // The printer prints the pattern of each parameter of a closure.
            ExprKind::Closure(closure) if closure.fn_decl.inputs.iter().any(|param| matches!(param.pat.kind, PatKind::Missing)) => {
                return ControlFlow::Break("a parameter of a closure with no pattern");
            }
            _ => {}
        }
        self.deeper(|this| visit::walk_expr(this, expr))
    }

    fn visit_pat(&mut self, pat: &'a rustc_ast::Pat) -> Self::Result {
        match &pat.kind {
            PatKind::Missing => return ControlFlow::Break("a missing pattern"),
            PatKind::Path(qself, path) | PatKind::Struct(qself, path, ..) | PatKind::TupleStruct(qself, path, ..) if !qself_fits(qself, path) => {
                return ControlFlow::Break("a qualified path with no segment at its position");
            }
            _ => {}
        }
        self.deeper(|this| visit::walk_pat(this, pat))
    }

    fn visit_ty(&mut self, ty: &'a rustc_ast::Ty) -> Self::Result {
        if let TyKind::Path(qself, path) = &ty.kind
            && !qself_fits(qself, path)
        {
            return ControlFlow::Break("a qualified path with no segment at its position");
        }
        self.deeper(|this| visit::walk_ty(this, ty))
    }

    fn visit_item(&mut self, item: &'a rustc_ast::Item) -> Self::Result {
        self.deeper(|this| visit::walk_item(this, item))
    }

    fn visit_assoc_item(&mut self, item: &'a rustc_ast::AssocItem, ctxt: AssocCtxt) -> Self::Result {
        self.deeper(|this| visit::walk_assoc_item(this, item, ctxt))
    }

    fn visit_block(&mut self, block: &'a rustc_ast::Block) -> Self::Result {
        self.deeper(|this| visit::walk_block(this, block))
    }

    /// A parameter of a function with no name (`fn f(u8);` of the edition 2015, `fn(u8)`,
    /// `fn(_)`) has a missing pattern: the printer prints only its type.
    fn visit_param(&mut self, param: &'a rustc_ast::Param) -> Self::Result {
        if matches!(param.pat.kind, PatKind::Missing) {
            for attr in param.attrs.iter() {
                self.visit_attribute(attr)?;
            }
            return self.visit_ty(&param.ty);
        }
        visit::walk_param(self, param)
    }

    fn visit_generic_param(&mut self, param: &'a rustc_ast::GenericParam) -> Self::Result {
        if let GenericParamKind::Lifetime = param.kind {
            lifetime_bounds(&param.bounds)?;
        }
        visit::walk_generic_param(self, param)
    }

    fn visit_where_predicate(&mut self, predicate: &'a rustc_ast::WherePredicate) -> Self::Result {
        if let WherePredicateKind::RegionPredicate(region) = &predicate.kind {
            lifetime_bounds(&region.bounds)?;
        }
        visit::walk_where_predicate(self, predicate)
    }

    fn visit_variant_data(&mut self, data: &'a VariantData) -> Self::Result {
        if let VariantData::Struct { fields, .. } = data
            && fields.iter().any(|field| field.ident.is_none())
        {
            return ControlFlow::Break("a field with no name in braces");
        }
        visit::walk_variant_data(self, data)
    }

    fn visit_inline_asm(&mut self, asm: &'a rustc_ast::InlineAsm) -> Self::Result {
        let count = asm.operands.len();
        if asm.template.iter().any(|piece| matches!(piece, rustc_ast::InlineAsmTemplatePiece::Placeholder { operand_idx, .. } if *operand_idx >= count)) {
            return ControlFlow::Break("a placeholder of `asm!` with no operand");
        }
        visit::walk_inline_asm(self, asm)
    }
}

/// The form of a tree that the printer refuses, or `None` (R66.11-1).
fn refused(walk: impl FnOnce(&mut Check) -> ControlFlow<&'static str>) -> Option<&'static str> {
    match walk(&mut Check { depth: 0 }) {
        ControlFlow::Break(cause) => Some(cause),
        ControlFlow::Continue(()) => None,
    }
}

/// The cause of a tree that the driver cannot print: it names the form.
fn cause(form: &str) -> String {
    format!("the printer of the compiler cannot print {form}")
}

/// The text of a part that the driver cannot print (R66.11-1): "not known: CAUSE" with a word
/// that no other call returns (`print::unique`, R66.10-1), so the text is never equal to another
/// text, also in the other version of the step.
pub(crate) fn not_known(cause: &str) -> String {
    format!("not known: {cause} ({})", crate::print::unique("print"))
}

/// The text of an item, or the cause of a refusal.
pub(crate) fn item(item: &rustc_ast::Item) -> Result<String, String> {
    match refused(|check| check.visit_item(item)) {
        Some(form) => Err(cause(form)),
        None => Ok(rustc_ast_pretty::pprust::item_to_string(item)),
    }
}

/// The text of an item of an impl or of a trait, or the cause of a refusal.
pub(crate) fn assoc_item(item: &rustc_ast::AssocItem) -> Result<String, String> {
    match refused(|check| check.visit_assoc_item(item, AssocCtxt::Impl { of_trait: false })) {
        Some(form) => Err(cause(form)),
        None => Ok(rustc_ast_pretty::pprust::assoc_item_to_string(item)),
    }
}

/// The text of a statement, or the cause of a refusal.
pub(crate) fn stmt(stmt: &rustc_ast::Stmt) -> Result<String, String> {
    match refused(|check| check.visit_stmt(stmt)) {
        Some(form) => Err(cause(form)),
        None => Ok(rustc_ast_pretty::pprust::stmt_to_string(stmt)),
    }
}

/// The text of an expression, or the cause of a refusal.
pub(crate) fn expr(expr: &rustc_ast::Expr) -> Result<String, String> {
    match refused(|check| check.visit_expr(expr)) {
        Some(form) => Err(cause(form)),
        None => Ok(rustc_ast_pretty::pprust::expr_to_string(expr)),
    }
}

/// The text of a pattern, or the cause of a refusal. A missing pattern alone (the pattern of a
/// parameter with no name, a node of an expansion) has the empty text: the printer prints no
/// pattern for it in its parameter.
pub(crate) fn pat(pat: &rustc_ast::Pat) -> Result<String, String> {
    if matches!(pat.kind, PatKind::Missing) {
        return Ok(String::new());
    }
    match refused(|check| check.visit_pat(pat)) {
        Some(form) => Err(cause(form)),
        None => Ok(rustc_ast_pretty::pprust::pat_to_string(pat)),
    }
}

/// The text of a type, or the cause of a refusal.
pub(crate) fn ty(ty: &rustc_ast::Ty) -> Result<String, String> {
    match refused(|check| check.visit_ty(ty)) {
        Some(form) => Err(cause(form)),
        None => Ok(rustc_ast_pretty::pprust::ty_to_string(ty)),
    }
}

#[cfg(test)]
mod tests {
    use rustc_ast::{BlockCheckMode, DUMMY_NODE_ID, Expr, ExprKind, Stmt, StmtKind, token};
    use rustc_span::{DUMMY_SP, Symbol};

    use super::*;

    fn expr_of(kind: ExprKind) -> Expr {
        Expr { id: DUMMY_NODE_ID, kind, span: DUMMY_SP, attrs: Default::default(), tokens: None }
    }

    fn number(text: &str) -> Expr {
        expr_of(ExprKind::Lit(token::Lit { kind: token::LitKind::Integer, symbol: Symbol::intern(text), suffix: None }))
    }

    fn block_of(inner: Expr) -> rustc_ast::Block {
        let mut block = rustc_ast::Block { stmts: Default::default(), id: DUMMY_NODE_ID, rules: BlockCheckMode::Default, span: DUMMY_SP };
        block.stmts.push(Stmt { id: DUMMY_NODE_ID, kind: StmtKind::Expr(Box::new(inner)), span: DUMMY_SP });
        block
    }

    fn if_else(alternative: Expr) -> Expr {
        let cond = expr_of(ExprKind::Path(None, rustc_ast::Path::from_ident(rustc_span::Ident::from_str("c"))));
        expr_of(ExprKind::If(Box::new(cond), Box::new(block_of(number("1"))), Some(Box::new(alternative))))
    }

    // R66.11-1: the driver never gives the printer a form that it asserts on. An `else` with a
    // literal (the form that a mark of R65.1-1 made: the printer panics, and the drop of its
    // boxes aborts the process) is not printed: the result is the cause. The negative case: an
    // `else` block, an `else if`, and each normal form print as before, with the same text as
    // the printer.
    // rule+ R66.11-1
    // rule- R66.11-1
    #[test]
    fn the_driver_prints_no_tree_that_the_printer_refuses() {
        rustc_span::create_default_session_globals_then(|| {
            let weird = if_else(number("0"));
            assert_eq!(expr(&weird), Err("the printer of the compiler cannot print an `else` that is not a block".to_string()));
            // The form inside a statement and inside a block of another expression.
            let inner = Stmt { id: DUMMY_NODE_ID, kind: StmtKind::Expr(Box::new(expr_of(ExprKind::Block(Box::new(block_of(weird.clone())), None)))), span: DUMMY_SP };
            assert!(stmt(&inner).is_err());
            let block = if_else(expr_of(ExprKind::Block(Box::new(block_of(number("0"))), None)));
            assert_eq!(expr(&block).as_deref(), Ok(rustc_ast_pretty::pprust::expr_to_string(&block).as_str()));
            assert_eq!(expr(&block).unwrap(), "if c { 1 } else { 0 }");
            let chain = if_else(if_else(expr_of(ExprKind::Block(Box::new(block_of(number("0"))), None))));
            assert_eq!(expr(&chain).unwrap(), "if c { 1 } else if c { 1 } else { 0 }");
            // A missing pattern inside a tree is refused; alone (the pattern of a parameter with
            // no name, a node of an expansion) it has the empty text, as in its parameter.
            let missing = rustc_ast::Pat { id: DUMMY_NODE_ID, kind: PatKind::Missing, span: DUMMY_SP };
            assert_eq!(pat(&missing).unwrap(), "");
            let tuple = rustc_ast::Pat { id: DUMMY_NODE_ID, kind: PatKind::Tuple([missing.clone()].into_iter().collect()), span: DUMMY_SP };
            assert_eq!(pat(&tuple), Err("the printer of the compiler cannot print a missing pattern".to_string()));
            // A type of a function pointer with a parameter with no name (`fn(_)`) prints.
            let infer = rustc_ast::Ty { id: DUMMY_NODE_ID, kind: TyKind::Infer, span: DUMMY_SP };
            let param = rustc_ast::Param { attrs: Default::default(), ty: Box::new(infer), pat: Box::new(missing.clone()), id: DUMMY_NODE_ID, span: DUMMY_SP, is_placeholder: false, has_const_keyword: false };
            let decl = rustc_ast::FnDecl { inputs: [param].into_iter().collect(), output: rustc_ast::FnRetTy::Default(DUMMY_SP) };
            let pointer = rustc_ast::Ty { id: DUMMY_NODE_ID, kind: TyKind::FnPtr(Box::new(rustc_ast::FnPtrTy { safety: rustc_ast::Safety::Default, ext: rustc_ast::Extern::None, generic_params: Default::default(), decl: Box::new(decl), decl_span: DUMMY_SP })), span: DUMMY_SP };
            assert_eq!(ty(&pointer).unwrap(), "fn(_)");
            let wild = rustc_ast::Pat { id: DUMMY_NODE_ID, kind: PatKind::Wild, span: DUMMY_SP };
            assert_eq!(pat(&wild).unwrap(), "_");
        });
        // A tree deeper than the limit: the cause, and no print. A tree at the limit prints. The
        // check runs in a thread with the stack of the thread of the compiler (8 MiB).
        std::thread::Builder::new()
            .stack_size(8 << 20)
            .spawn(|| {
                rustc_span::create_default_session_globals_then(|| {
                    let mut deep = number("1");
                    for _ in 0..MAX_DEPTH {
                        deep = expr_of(ExprKind::Paren(Box::new(deep)));
                    }
                    assert_eq!(expr(&deep), Err("the printer of the compiler cannot print a tree deeper than the limit of the driver".to_string()));
                    let ExprKind::Paren(fits) = &deep.kind else { unreachable!() };
                    assert_eq!(expr(fits).unwrap().len(), 2 * (MAX_DEPTH - 1) + 1);
                    // A deep tree has a deep drop: it stays out of the stack of the test.
                    std::mem::forget(deep);
                });
            })
            .unwrap()
            .join()
            .unwrap();
    }

    // R66.11-1: the text of a part that the driver cannot print is never equal to another text.
    // rule+ R66.11-1
    #[test]
    fn a_part_that_the_driver_cannot_print_is_never_equal_to_another_part() {
        let (a, b) = (not_known("the printer of the compiler cannot print an `else` that is not a block"), not_known("the printer of the compiler cannot print an `else` that is not a block"));
        assert_ne!(a, b);
        assert!(a.starts_with("not known: the printer of the compiler cannot print an `else` that is not a block (unknown_print#"), "{a}");
    }

    /// The source scan of R66.11-1: no file of the driver but this one calls the printer.
    // rule+ R66.11-1
    #[test]
    fn only_this_file_calls_the_printer_of_the_compiler() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == "pretty.rs") || path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (number, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if code.contains("pprust") || code.contains("rustc_hir_pretty") {
                    found.push(format!("{}:{}", path.display(), number + 1));
                }
            }
        }
        assert!(found.is_empty(), "a call of the printer with no check: {found:?}");
    }
}

