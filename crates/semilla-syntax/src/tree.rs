//! The node tree inside an item: blocks, statements, and expressions.
//!
//! Each node has a role in its parent. The roles make the anchor path: `S2/cond/rhs` is the role
//! `S2` (statement 2), then `cond`, then `rhs`. The text of a node is its token stream, without
//! whitespace and comments, so two nodes with the same text are the same code.

use proc_macro2::Span;
use quote::ToTokens;
use serde::Serialize;
use syn::spanned::Spanned;

/// A line and a column (the line starts at 1, the column at 0).
pub type Pos = (usize, usize);

/// One node of the tree.
#[derive(Clone, Debug, Serialize)]
pub struct Node {
    /// The kind, for example `block`, `let`, `if`, `call`, `binary`, `lit`, `path`.
    pub kind: &'static str,
    /// The role in the parent, for example `S2`, `cond`, `args[1]`, `arm[0].body`.
    pub role: String,
    /// The token text of the node.
    pub text: String,
    /// An attribute of the node itself: the operator, the method name, the macro name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
    /// True for a statement in a block.
    pub stmt: bool,
    pub start: Pos,
    pub end: Pos,
    pub children: Vec<Node>,
}

/// The node kinds that contain statements. A free edit must not contain them (design 5.6).
pub const BLOCK_LIKE: &[&str] = &["block", "if", "match", "for", "while", "loop", "block_expr", "unsafe", "async", "closure_block", "const_block"];

impl Node {
    pub fn child(&self, role: &str) -> Option<&Node> {
        self.children.iter().find(|child| child.role == role)
    }

    /// True if the node or a descendant contains statements.
    pub fn has_block(&self) -> bool {
        BLOCK_LIKE.contains(&self.kind) || self.children.iter().any(Node::has_block)
    }

    /// True if the position is inside the node.
    pub fn contains_pos(&self, pos: Pos) -> bool {
        self.start <= pos && pos <= self.end
    }

    /// The number of nodes in the subtree.
    pub fn size(&self) -> usize {
        1 + self.children.iter().map(Node::size).sum::<usize>()
    }

    /// A short label for views: the first line of the text, at most 60 characters.
    pub fn label(&self) -> String {
        short(&self.text, 60)
    }

    /// Visits the node and all its descendants, with the role path from this node.
    pub fn walk<'a>(&'a self, path: &mut Vec<String>, visit: &mut dyn FnMut(&[String], &'a Node)) {
        visit(path, self);
        for child in &self.children {
            path.push(child.role.clone());
            child.walk(path, visit);
            path.pop();
        }
    }
}

/// Shortens a text to `max` characters on one line.
pub fn short(text: &str, max: usize) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        line
    } else {
        let cut: String = line.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Returns the normalized token text of a syntax element.
pub fn tokens(value: &impl ToTokens) -> String {
    value.to_token_stream().to_string()
}

fn pos_range(span: Span) -> (Pos, Pos) {
    let start = span.start();
    let end = span.end();
    ((start.line, start.column), (end.line, end.column))
}

fn node(kind: &'static str, role: impl Into<String>, value: &(impl ToTokens + Spanned), children: Vec<Node>) -> Node {
    let (start, end) = pos_range(value.span());
    Node { kind, role: role.into(), text: tokens(value), attr: None, stmt: false, start, end, children }
}

fn with_attr(mut node: Node, attr: impl Into<String>) -> Node {
    node.attr = Some(attr.into());
    node
}

/// Builds the node of a block. Statements get the roles `S0`, `S1`, …; a final expression
/// without a semicolon gets the role `tail`.
pub fn block(role: impl Into<String>, block: &syn::Block) -> Node {
    let mut children = Vec::new();
    let mut index = 0;
    let count = block.stmts.len();
    for (position, stmt) in block.stmts.iter().enumerate() {
        // A final block-like expression (`if`, `match`, a loop) is a statement in the design anchors,
        // also when it is syntactically the tail.
        let is_tail = position + 1 == count && matches!(stmt, syn::Stmt::Expr(expr, None) if !is_block_like_expr(expr));
        if is_tail {
            if let syn::Stmt::Expr(expr, None) = stmt {
                let mut tail = expr_node("tail", expr);
                tail.stmt = true;
                children.push(tail);
            }
        } else {
            children.push(stmt_node(format!("S{index}"), stmt));
            index += 1;
        }
    }
    node("block", role, block, children)
}

fn is_block_like_expr(expr: &syn::Expr) -> bool {
    use syn::Expr;
    matches!(expr, Expr::If(_) | Expr::Match(_) | Expr::ForLoop(_) | Expr::While(_) | Expr::Loop(_) | Expr::Block(_) | Expr::Unsafe(_))
}

fn stmt_node(role: String, stmt: &syn::Stmt) -> Node {
    let mut result = match stmt {
        syn::Stmt::Local(local) => {
            let mut children = vec![pattern_node("pat", &local.pat)];
            if let Some(init) = &local.init {
                children.push(expr_node("init", &init.expr));
                if let Some((_, diverge)) = &init.diverge {
                    children.push(expr_node("else", diverge));
                }
            }
            node("let", role, local, children)
        }
        syn::Stmt::Item(item) => node("item_stmt", role, item, Vec::new()),
        syn::Stmt::Expr(expr, _) => {
            let mut result = expr_node(role, expr);
            // The statement text includes the semicolon, so that `x` and `x;` are different.
            result.text = tokens(stmt);
            result
        }
        syn::Stmt::Macro(mac) => with_attr(node("macro", role, mac, Vec::new()), tokens(&mac.mac.path)),
    };
    result.stmt = true;
    result
}

fn pattern_node(role: &str, pat: &syn::Pat) -> Node {
    node("pattern", role, pat, Vec::new())
}

/// Builds the node of an expression. Parentheses are transparent.
pub fn expr_node(role: impl Into<String>, expr: &syn::Expr) -> Node {
    use syn::Expr;
    let role = role.into();
    match expr {
        Expr::Paren(inner) => {
            let mut result = expr_node(role, &inner.expr);
            result.text = tokens(expr);
            result
        }
        Expr::Group(inner) => expr_node(role, &inner.expr),
        Expr::Lit(lit) => node("lit", role, lit, Vec::new()),
        Expr::Path(path) => node("path", role, path, Vec::new()),
        Expr::Binary(binary) => with_attr(
            node("binary", role, binary, vec![expr_node("lhs", &binary.left), expr_node("rhs", &binary.right)]),
            tokens(&binary.op),
        ),
        Expr::Unary(unary) => {
            with_attr(node("unary", role, unary, vec![expr_node("operand", &unary.expr)]), tokens(&unary.op))
        }
        Expr::Try(inner) => node("try", role, inner, vec![expr_node("operand", &inner.expr)]),
        Expr::Reference(inner) => with_attr(
            node("ref", role, inner, vec![expr_node("operand", &inner.expr)]),
            if inner.mutability.is_some() { "&mut" } else { "&" },
        ),
        Expr::Cast(cast) => with_attr(node("cast", role, cast, vec![expr_node("operand", &cast.expr)]), tokens(&cast.ty)),
        Expr::Call(call) => {
            let mut children = vec![expr_node("callee", &call.func)];
            children.extend(call.args.iter().enumerate().map(|(index, arg)| expr_node(format!("args[{index}]"), arg)));
            node("call", role, call, children)
        }
        Expr::MethodCall(call) => {
            let mut children = vec![expr_node("recv", &call.receiver)];
            children.extend(call.args.iter().enumerate().map(|(index, arg)| expr_node(format!("args[{index}]"), arg)));
            with_attr(node("method_call", role, call, children), call.method.to_string())
        }
        Expr::Field(field) => {
            with_attr(node("field", role, field, vec![expr_node("base", &field.base)]), tokens(&field.member))
        }
        Expr::Index(index) => {
            node("index", role, index, vec![expr_node("base", &index.expr), expr_node("index", &index.index)])
        }
        Expr::Struct(lit) => {
            let children = lit
                .fields
                .iter()
                .map(|field| expr_node(format!("fields[{}]", tokens(&field.member)), &field.expr))
                .collect();
            with_attr(node("struct", role, lit, children), tokens(&lit.path))
        }
        Expr::Tuple(tuple) => {
            let children = tuple.elems.iter().enumerate().map(|(index, elem)| expr_node(format!("elems[{index}]"), elem)).collect();
            node("tuple", role, tuple, children)
        }
        Expr::Array(array) => {
            let children = array.elems.iter().enumerate().map(|(index, elem)| expr_node(format!("elems[{index}]"), elem)).collect();
            node("array", role, array, children)
        }
        Expr::If(expr_if) => {
            let mut children = vec![expr_node("cond", &expr_if.cond), block("then", &expr_if.then_branch)];
            if let Some((_, else_branch)) = &expr_if.else_branch {
                children.push(match else_branch.as_ref() {
                    Expr::Block(else_block) => block("else", &else_block.block),
                    other => expr_node("else", other),
                });
            }
            node("if", role, expr_if, children)
        }
        Expr::Let(expr_let) => node("let_cond", role, expr_let, vec![pattern_node("pat", &expr_let.pat), expr_node("scrutinee", &expr_let.expr)]),
        Expr::Match(expr_match) => {
            let mut children = vec![expr_node("scrutinee", &expr_match.expr)];
            for (index, arm) in expr_match.arms.iter().enumerate() {
                children.push(pattern_node(&format!("arm[{index}].pat"), &arm.pat));
                if let Some((_, guard)) = &arm.guard {
                    children.push(expr_node(format!("arm[{index}].guard"), guard));
                }
                children.push(match arm.body.as_ref() {
                    Expr::Block(body) if body.label.is_none() && body.attrs.is_empty() => block(format!("arm[{index}].body"), &body.block),
                    other => expr_node(format!("arm[{index}].body"), other),
                });
            }
            node("match", role, expr_match, children)
        }
        Expr::ForLoop(for_loop) => node(
            "for",
            role,
            for_loop,
            vec![pattern_node("pat", &for_loop.pat), expr_node("iter", &for_loop.expr), block("body", &for_loop.body)],
        ),
        Expr::While(while_loop) => {
            node("while", role, while_loop, vec![expr_node("cond", &while_loop.cond), block("body", &while_loop.body)])
        }
        Expr::Loop(loop_expr) => node("loop", role, loop_expr, vec![block("body", &loop_expr.body)]),
        Expr::Block(block_expr) => node("block_expr", role, block_expr, vec![block("body", &block_expr.block)]),
        Expr::Unsafe(unsafe_expr) => node("unsafe", role, unsafe_expr, vec![block("body", &unsafe_expr.block)]),
        Expr::Async(async_expr) => node("async", role, async_expr, vec![block("body", &async_expr.block)]),
        Expr::Const(const_expr) => node("const_block", role, const_expr, vec![block("body", &const_expr.block)]),
        Expr::Closure(closure) => {
            let kind = if matches!(closure.body.as_ref(), Expr::Block(_)) { "closure_block" } else { "closure" };
            let body = match closure.body.as_ref() {
                Expr::Block(body) => block("body", &body.block),
                other => expr_node("body", other),
            };
            let params: Vec<String> = closure.inputs.iter().map(tokens).collect();
            with_attr(node(kind, role, closure, vec![body]), params.join(", "))
        }
        Expr::Return(ret) => node("return", role, ret, ret.expr.iter().map(|value| expr_node("value", value)).collect()),
        Expr::Break(brk) => node("break", role, brk, brk.expr.iter().map(|value| expr_node("value", value)).collect()),
        Expr::Continue(cont) => node("continue", role, cont, Vec::new()),
        Expr::Assign(assign) => node("assign", role, assign, vec![expr_node("lhs", &assign.left), expr_node("rhs", &assign.right)]),
        Expr::Range(range) => {
            let mut children = Vec::new();
            if let Some(start) = &range.start {
                children.push(expr_node("start", start));
            }
            if let Some(end) = &range.end {
                children.push(expr_node("end", end));
            }
            with_attr(node("range", role, range, children), tokens(&range.limits))
        }
        Expr::Await(await_expr) => node("await", role, await_expr, vec![expr_node("operand", &await_expr.base)]),
        Expr::Macro(mac) => with_attr(node("macro", role, mac, Vec::new()), tokens(&mac.mac.path)),
        other => node("other", role, other, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(code: &str) -> Node {
        let item: syn::ItemFn = syn::parse_str(code).unwrap();
        block("body", &item.block)
    }

    #[test]
    fn roles_follow_the_design() {
        let body = body_of("fn f(v: &[u32], n: u32) -> u32 { let mut s = 0; for x in v { if *x < n { s += x; } } s }");
        assert_eq!(body.children.len(), 3);
        assert_eq!(body.children[0].kind, "let");
        assert_eq!(body.children[1].role, "S1");
        assert_eq!(body.children[1].kind, "for");
        assert_eq!(body.children[2].role, "tail");
        let loop_body = body.children[1].child("body").unwrap();
        let cond = loop_body.children[0].child("cond").unwrap();
        assert_eq!(cond.attr.as_deref(), Some("<"));
        assert_eq!(cond.child("rhs").unwrap().text, "n");
    }

    #[test]
    fn statement_text_keeps_the_semicolon() {
        let body = body_of("fn f() { g(); h() }");
        assert_eq!(body.children[0].text, "g () ;");
        assert_eq!(body.children[1].text, "h ()");
    }

    #[test]
    fn block_like_detection() {
        let body = body_of("fn f(a: u32) -> u32 { let x = a + 1; let y = if a > 0 { 1 } else { 2 }; x + y }");
        assert!(!body.children[0].has_block());
        assert!(body.children[1].has_block());
        let closure = body_of("fn f(v: Vec<u32>) -> Vec<u32> { v.into_iter().map(|x| x + 1).collect() }");
        assert!(!closure.children[0].has_block(), "an expression closure is block-free");
    }

    #[test]
    fn positions_are_recorded() {
        let item: syn::ItemFn = syn::parse_str("fn f() {\n    let a = 1;\n}").unwrap();
        let body = block("body", &item.block);
        assert_eq!(body.children[0].start.0, 2);
    }
}
