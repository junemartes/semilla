//! The node tree inside an item: blocks, statements, and expressions.
//!
//! Each node has a role in its parent. The roles make the anchor path: `S2/cond/rhs` is the role
//! `S2` (statement 2), then `cond`, then `rhs`. The text of a node is its compare text: its
//! tokens without spaces and comments (design 39.2), so two nodes with the same text are the
//! same code. The tree comes from the lossless tree of `ra_ap_syntax`.

use serde::Serialize;

use crate::rs::{AstNode, Edition, Lines, SyntaxNode, SyntaxToken, ast, code_range, compare, compare_opt};
use crate::tt::{Flat, compare_text, flat_text, flat_tokens};
use ast::{HasArgList, HasAttrs, HasGenericParams, HasLoopBody, HasName};

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

/// The tokens of a source text for a display: `(white space before, text)`. A string literal, a
/// char literal, and a comment are one token with their exact text. A word (a name, a number, a
/// keyword) is one token. Each other character is one token.
pub fn display_tokens(text: &str) -> Vec<(bool, String)> {
    let chars: Vec<char> = text.chars().collect();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    // The end of a string literal that starts with the quote mark at `at`, with `hashes` hash
    // marks (a raw string has no escapes).
    let string_end = |at: usize, raw: bool, hashes: usize| -> usize {
        let mut i = at + 1;
        while i < chars.len() {
            if chars[i] == '\\' && !raw {
                i += 2;
            } else if chars[i] == '"' && chars[i + 1..].iter().take(hashes).filter(|c| **c == '#').count() == hashes {
                return i + 1 + hashes;
            } else {
                i += 1;
            }
        }
        chars.len()
    };
    let mut tokens = Vec::new();
    let (mut i, mut space) = (0, false);
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            space = true;
            i += 1;
            continue;
        }
        let end = if c == '"' {
            string_end(i, false, 0)
        } else if c == '\'' {
            if chars.get(i + 1) == Some(&'\\') {
                // A char literal with an escape.
                (i + 3..chars.len()).find(|at| chars[*at] == '\'').map_or(chars.len(), |at| at + 1)
            } else if chars.get(i + 2) == Some(&'\'') {
                i + 3
            } else {
                // A lifetime or a label.
                (i + 1..chars.len()).find(|at| !word(chars[*at])).unwrap_or(chars.len())
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            (i..chars.len()).find(|at| chars[*at] == '\n').unwrap_or(chars.len())
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            (i + 2..chars.len()).find(|at| chars[*at] == '*' && chars.get(at + 1) == Some(&'/')).map_or(chars.len(), |at| at + 2)
        } else if word(c) {
            let end = (i..chars.len()).find(|at| !word(chars[*at])).unwrap_or(chars.len());
            let name: String = chars[i..end].iter().collect();
            let hashes = chars[end..].iter().take_while(|c| **c == '#').count();
            let raw = matches!(name.as_str(), "r" | "br" | "cr");
            if chars.get(end + hashes) == Some(&'"') && (raw || (hashes == 0 && matches!(name.as_str(), "b" | "c"))) {
                string_end(end + hashes, raw, hashes)
            } else {
                end
            }
        } else {
            i + 1
        };
        let end = end.clamp(i + 1, chars.len());
        tokens.push((space, chars[i..end].iter().collect::<String>().trim_end().to_string()));
        space = false;
        i = end;
    }
    tokens
}

/// A source text on one line: each run of white space between two tokens is one space. The
/// white space in a literal does not change (batch 5k, D6).
pub fn one_line(text: &str) -> String {
    let mut line = String::new();
    for (space, token) in display_tokens(text) {
        if space && !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&token);
    }
    line
}

/// Shortens a text to `max` characters on one line.
pub fn short(text: &str, max: usize) -> String {
    let line = one_line(text);
    if line.chars().count() <= max {
        line
    } else {
        let cut: String = line.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Builds the nodes of one tree: the line index of the file, the shift of the offsets (a macro
/// input that is parsed as a list of expressions is a separate text), and the edition.
#[derive(Clone, Copy)]
pub struct Builder<'a> {
    lines: &'a Lines,
    shift: isize,
    edition: Edition,
    /// The tokens of the file in order, for a fast compare text of a node (empty: the node
    /// gives its own tokens).
    tokens: &'a [Flat],
}

impl<'a> Builder<'a> {
    pub fn new(lines: &'a Lines, edition: Edition) -> Builder<'a> {
        Builder { lines, shift: 0, edition, tokens: &[] }
    }

    /// A builder that reads the compare texts from the tokens of the file.
    pub(crate) fn with_tokens(lines: &'a Lines, edition: Edition, tokens: &'a [Flat]) -> Builder<'a> {
        Builder { lines, shift: 0, edition, tokens }
    }

    /// The compare text of a node: the text of its slice of the tokens of the file.
    pub(crate) fn text(&self, value: &SyntaxNode) -> String {
        match tokens_in(self.tokens, value) {
            Some(slice) if self.shift == 0 => flat_text(slice),
            _ => compare_text(value),
        }
    }

    fn pos(&self, offset: usize) -> Pos {
        self.lines.pos((offset as isize + self.shift).max(0) as usize)
    }

    /// The start and the end of the code of a node (without its leading comments).
    pub fn range(&self, node: &SyntaxNode) -> (Pos, Pos) {
        let range = code_range(node);
        (self.pos(range.start), self.pos(range.end))
    }

    fn node(&self, kind: &'static str, role: impl Into<String>, value: &SyntaxNode, children: Vec<Node>) -> Node {
        let (start, end) = self.range(value);
        Node { kind, role: role.into(), text: self.text(value), attr: None, stmt: false, start, end, children }
    }

    /// The signature of a function as a node (design 22.3, A5). The children are its parts:
    /// `qualifiers` (`const`, `async`, `unsafe`, `extern`), `generics` (the parameters with their
    /// inline bounds), `where`, one `param[NAME]` for each parameter (`param[self]` for the
    /// receiver; a pattern parameter has its pattern text as the name), and `ret`.
    pub fn signature(&self, function: &ast::Fn) -> Node {
        let at_fn = function.fn_token().map(|token| usize::from(token.text_range().start())).unwrap_or_else(|| code_range(function.syntax()).start);
        let place = |node: Option<&SyntaxNode>| match node {
            Some(node) => self.range(node),
            None => (self.pos(at_fn), self.pos(at_fn)),
        };
        let part = |kind: &'static str, role: String, text: String, node: Option<&SyntaxNode>| {
            let (start, end) = place(node);
            Node { kind, role, text, attr: None, stmt: false, start, end, children: Vec::new() }
        };
        let word = |token: Option<SyntaxToken>| token.map(|token| token.text().to_string()).unwrap_or_default();
        let qualifiers = [word(function.const_token()), word(function.async_token()), word(function.unsafe_token()), compare_opt(function.abi().as_ref())].into_iter().filter(|q| !q.is_empty()).collect::<Vec<_>>().join(" ");
        let mut qualifier = part("qualifiers", "qualifiers".into(), qualifiers, None);
        (qualifier.start, qualifier.end) = (self.pos(at_fn), self.pos(at_fn + 2));
        let generics = function.generic_param_list();
        let where_clause = function.where_clause();
        let mut children = vec![
            qualifier,
            part("generics", "generics".into(), compare_opt(generics.as_ref()), generics.as_ref().map(|node| node.syntax())),
            part("where", "where".into(), compare_opt(where_clause.as_ref()), where_clause.as_ref().map(|node| node.syntax())),
        ];
        if let Some(list) = function.param_list() {
            if let Some(receiver) = list.self_param() {
                let mut node = part("param", "param[self]".into(), compare(&receiver), Some(receiver.syntax()));
                node.attr = Some(compare(&receiver));
                children.push(node);
            }
            for param in list.params().filter(|param| param.dotdotdot_token().is_none()) {
                let name = match param.pat() {
                    Some(ast::Pat::IdentPat(ident)) if ident.pat().is_none() || ident.at_token().is_some() => ident.name().map(|name| name.text().to_string()).unwrap_or_default(),
                    Some(other) => compare(&other).replace(' ', ""),
                    None => String::new(),
                };
                let mut node = part("param", format!("param[{name}]"), compare(&param), Some(param.syntax()));
                node.attr = Some(compare_opt(param.ty().as_ref()));
                children.push(node);
            }
        }
        let ret = function.ret_type();
        children.push(part("ret", "ret".into(), compare_opt(ret.as_ref()), ret.as_ref().map(|node| node.syntax())));
        let tokens = signature_tokens(function);
        let (start, end) = match (tokens.first(), tokens.last()) {
            (Some(first), Some(last)) => (self.pos(first.start), self.pos(last.end)),
            _ => self.range(function.syntax()),
        };
        Node { kind: "signature", role: "sig".into(), text: flat_text(&tokens), attr: None, stmt: false, start, end, children }
    }

    /// Builds the node of a block. Statements get the roles `S0`, `S1`, …; a final expression
    /// without a semicolon gets the role `tail`.
    pub fn block(&self, role: impl Into<String>, block: &ast::BlockExpr) -> Node {
        match block.stmt_list() {
            Some(list) => self.stmt_list(role, &list),
            None => self.node("block", role, block.syntax(), Vec::new()),
        }
    }

    fn stmt_list(&self, role: impl Into<String>, list: &ast::StmtList) -> Node {
        let mut children = Vec::new();
        let mut index = 0;
        for stmt in list.statements() {
            if let Some(node) = self.stmt_node(format!("S{index}"), &stmt) {
                children.push(node);
                index += 1;
            }
        }
        if let Some(expr) = list.tail_expr() {
            // A final block-like expression (`if`, `match`, a loop) is a statement in the design
            // anchors, also when it is syntactically the tail. So is a macro call with braces
            // (`m! { … }`): the parser of a macro statement does not know its kind.
            if is_block_like_expr(&expr) || brace_macro(&expr) {
                let mut node = self.expr_node(format!("S{index}"), &expr);
                node.stmt = true;
                children.push(node);
            } else {
                let mut tail = self.expr_node("tail", &expr);
                tail.stmt = true;
                children.push(tail);
            }
        }
        self.node("block", role, list.syntax(), children)
    }

    fn stmt_node(&self, role: String, stmt: &ast::Stmt) -> Option<Node> {
        let mut result = match stmt {
            ast::Stmt::LetStmt(local) => {
                let mut children = Vec::new();
                if let Some(pat) = local.pat() {
                    // The pattern with its type annotation (`x: u32`), as one node.
                    let end = local.ty().map_or(pat.syntax().text_range().end(), |ty| ty.syntax().text_range().end());
                    let start = pat.syntax().text_range().start();
                    let tokens: Vec<Flat> = flat_tokens(local.syntax()).into_iter().filter(|token| token.start >= usize::from(start) && token.end <= usize::from(end)).collect();
                    let (from, to) = (tokens.first().map_or(usize::from(start), |t| t.start), tokens.last().map_or(usize::from(end), |t| t.end));
                    children.push(Node { kind: "pattern", role: "pat".into(), text: flat_text(&tokens), attr: None, stmt: false, start: self.pos(from), end: self.pos(to), children: Vec::new() });
                }
                if let Some(init) = local.initializer() {
                    children.push(self.expr_node("init", &init));
                    if let Some(diverge) = local.let_else().and_then(|let_else| let_else.block_expr()) {
                        children.push(self.expr_node("else", &ast::Expr::BlockExpr(diverge)));
                    }
                }
                self.node("let", role, local.syntax(), children)
            }
            ast::Stmt::Item(ast::Item::MacroCall(call)) => self.macro_node(role, call.syntax(), call),
            ast::Stmt::Item(item) => self.node("item_stmt", role, item.syntax(), Vec::new()),
            ast::Stmt::ExprStmt(expr_stmt) => {
                let expr = expr_stmt.expr()?;
                // A macro call statement with `;` or with braces: the node is the full statement,
                // with its place (as in `syn`, a macro statement).
                if let ast::Expr::MacroExpr(mac) = &expr
                    && let Some(call) = mac.macro_call()
                    && (expr_stmt.semicolon_token().is_some() || brace_macro(&expr))
                {
                    let mut node = self.macro_node(role, expr_stmt.syntax(), &call);
                    node.stmt = true;
                    return Some(node);
                }
                let mut result = self.expr_node(role, &expr);
                // The statement text includes the semicolon, so that `x` and `x;` are different.
                result.text = self.text(expr_stmt.syntax());
                result
            }
        };
        result.stmt = true;
        Some(result)
    }

    fn pattern_node(&self, role: &str, pat: &ast::Pat) -> Node {
        self.node("pattern", role, pat.syntax(), Vec::new())
    }

    fn opt_expr(&self, role: &str, expr: Option<ast::Expr>) -> Option<Node> {
        expr.map(|expr| self.expr_node(role, &expr))
    }

    /// Builds the node of an expression. Parentheses are transparent.
    pub fn expr_node(&self, role: impl Into<String>, expr: &ast::Expr) -> Node {
        use ast::Expr;
        let role = role.into();
        let syntax = expr.syntax();
        let with_attr = |mut node: Node, attr: String| {
            node.attr = Some(attr);
            node
        };
        let list = |prefix: &str, exprs: &mut dyn Iterator<Item = ast::Expr>| -> Vec<Node> { exprs.enumerate().map(|(index, arg)| self.expr_node(format!("{prefix}[{index}]"), &arg)).collect() };
        let token_text = |token: Option<SyntaxToken>| token.map(|token| token.text().to_string()).unwrap_or_default();
        match expr {
            Expr::ParenExpr(inner) => match inner.expr() {
                Some(value) => {
                    let mut result = self.expr_node(role, &value);
                    result.text = self.text(syntax);
                    result
                }
                None => self.node("other", role, syntax, Vec::new()),
            },
            Expr::Literal(_) => self.node("lit", role, syntax, Vec::new()),
            Expr::PathExpr(_) => self.node("path", role, syntax, Vec::new()),
            Expr::BinExpr(binary) => {
                let children = [self.opt_expr("lhs", binary.lhs()), self.opt_expr("rhs", binary.rhs())].into_iter().flatten().collect();
                match binary.op_kind() {
                    Some(ast::BinaryOp::Assignment { op: None }) => self.node("assign", role, syntax, children),
                    _ => with_attr(self.node("binary", role, syntax, children), token_text(binary.op_token())),
                }
            }
            Expr::PrefixExpr(unary) => with_attr(self.node("unary", role, syntax, self.opt_expr("operand", unary.expr()).into_iter().collect()), token_text(unary.op_token())),
            Expr::TryExpr(inner) => self.node("try", role, syntax, self.opt_expr("operand", inner.expr()).into_iter().collect()),
            Expr::RefExpr(inner) if inner.raw_token().is_none() => {
                let attr = if inner.mut_token().is_some() { "&mut" } else { "&" };
                with_attr(self.node("ref", role, syntax, self.opt_expr("operand", inner.expr()).into_iter().collect()), attr.to_string())
            }
            Expr::CastExpr(cast) => with_attr(self.node("cast", role, syntax, self.opt_expr("operand", cast.expr()).into_iter().collect()), compare_opt(cast.ty().as_ref())),
            Expr::CallExpr(call) => {
                let mut children: Vec<Node> = self.opt_expr("callee", call.expr()).into_iter().collect();
                children.extend(list("args", &mut call.arg_list().into_iter().flat_map(|args| args.args())));
                self.node("call", role, syntax, children)
            }
            Expr::MethodCallExpr(call) => {
                let mut children: Vec<Node> = self.opt_expr("recv", call.receiver()).into_iter().collect();
                children.extend(list("args", &mut call.arg_list().into_iter().flat_map(|args| args.args())));
                with_attr(self.node("method_call", role, syntax, children), call.name_ref().map(|name| name.text().to_string()).unwrap_or_default())
            }
            Expr::FieldExpr(field) => {
                with_attr(self.node("field", role, syntax, self.opt_expr("base", field.expr()).into_iter().collect()), field.name_ref().map(|name| name.text().to_string()).unwrap_or_default())
            }
            Expr::IndexExpr(index) => self.node("index", role, syntax, [self.opt_expr("base", index.base()), self.opt_expr("index", index.index())].into_iter().flatten().collect()),
            Expr::RecordExpr(lit) => {
                let children = lit
                    .record_expr_field_list()
                    .into_iter()
                    .flat_map(|fields| fields.fields())
                    .filter_map(|field| {
                        let name = field.field_name().map(|name| name.text().to_string()).unwrap_or_default();
                        field.expr().map(|value| self.expr_node(format!("fields[{name}]"), &value))
                    })
                    .collect();
                with_attr(self.node("struct", role, syntax, children), compare_opt(lit.path().as_ref()))
            }
            Expr::TupleExpr(tuple) => self.node("tuple", role, syntax, list("elems", &mut tuple.fields())),
            Expr::ArrayExpr(array) if array.semicolon_token().is_none() => self.node("array", role, syntax, list("elems", &mut array.exprs())),
            Expr::IfExpr(expr_if) => {
                let mut children: Vec<Node> = self.opt_expr("cond", expr_if.condition()).into_iter().collect();
                children.extend(expr_if.then_branch().map(|then| self.block("then", &then)));
                match expr_if.else_branch() {
                    Some(ast::ElseBranch::Block(else_block)) => children.push(self.block("else", &else_block)),
                    Some(ast::ElseBranch::IfExpr(else_if)) => children.push(self.expr_node("else", &Expr::IfExpr(else_if))),
                    None => {}
                }
                self.node("if", role, syntax, children)
            }
            Expr::LetExpr(expr_let) => {
                let children = [expr_let.pat().map(|pat| self.pattern_node("pat", &pat)), self.opt_expr("scrutinee", expr_let.expr())].into_iter().flatten().collect();
                self.node("let_cond", role, syntax, children)
            }
            Expr::MatchExpr(expr_match) => {
                let mut children: Vec<Node> = self.opt_expr("scrutinee", expr_match.expr()).into_iter().collect();
                for (index, arm) in expr_match.match_arm_list().into_iter().flat_map(|arms| arms.arms()).enumerate() {
                    children.extend(arm.pat().map(|pat| self.pattern_node(&format!("arm[{index}].pat"), &pat)));
                    if let Some(guard) = arm.guard().and_then(|guard| guard.condition()) {
                        children.push(self.expr_node(format!("arm[{index}].guard"), &guard));
                    }
                    children.extend(arm.expr().map(|body| match &body {
                        Expr::BlockExpr(block) if block.modifier().is_none() && block.attrs().next().is_none() => self.block(format!("arm[{index}].body"), block),
                        other => self.expr_node(format!("arm[{index}].body"), other),
                    }));
                }
                self.node("match", role, syntax, children)
            }
            Expr::ForExpr(for_loop) => {
                let children = [for_loop.pat().map(|pat| self.pattern_node("pat", &pat)), self.opt_expr("iter", for_loop.iterable()), for_loop.loop_body().map(|body| self.block("body", &body))].into_iter().flatten().collect();
                self.node("for", role, syntax, children)
            }
            Expr::WhileExpr(while_loop) => {
                let children = [self.opt_expr("cond", while_loop.condition()), while_loop.loop_body().map(|body| self.block("body", &body))].into_iter().flatten().collect();
                self.node("while", role, syntax, children)
            }
            Expr::LoopExpr(loop_expr) => self.node("loop", role, syntax, loop_expr.loop_body().map(|body| self.block("body", &body)).into_iter().collect()),
            Expr::BlockExpr(block_expr) => {
                let kind = match block_expr.modifier() {
                    None | Some(ast::BlockModifier::Label(_)) => "block_expr",
                    Some(ast::BlockModifier::Unsafe(_)) => "unsafe",
                    Some(ast::BlockModifier::Async(_)) => "async",
                    Some(ast::BlockModifier::Const(_)) => "const_block",
                    Some(_) => return self.node("other", role, syntax, Vec::new()),
                };
                let body = block_expr.stmt_list().map(|list| self.stmt_list("body", &list));
                self.node(kind, role, syntax, body.into_iter().collect())
            }
            Expr::ClosureExpr(closure) => {
                let body = closure.body();
                let block = match &body {
                    Some(Expr::BlockExpr(block)) if matches!(block.modifier(), None | Some(ast::BlockModifier::Label(_))) => Some(block),
                    _ => None,
                };
                let kind = if block.is_some() { "closure_block" } else { "closure" };
                let child = match (block, &body) {
                    (Some(block), _) => Some(self.block("body", block)),
                    (None, Some(other)) => Some(self.expr_node("body", other)),
                    (None, None) => None,
                };
                let params: Vec<String> = closure.param_list().into_iter().flat_map(|list| list.params()).map(|param| compare(&param)).collect();
                with_attr(self.node(kind, role, syntax, child.into_iter().collect()), params.join(", "))
            }
            Expr::ReturnExpr(ret) => self.node("return", role, syntax, self.opt_expr("value", ret.expr()).into_iter().collect()),
            Expr::BreakExpr(brk) => self.node("break", role, syntax, self.opt_expr("value", brk.expr()).into_iter().collect()),
            Expr::ContinueExpr(_) => self.node("continue", role, syntax, Vec::new()),
            Expr::RangeExpr(range) => {
                use ast::RangeItem;
                let children = [self.opt_expr("start", range.start()), self.opt_expr("end", range.end())].into_iter().flatten().collect();
                with_attr(self.node("range", role, syntax, children), token_text(range.op_token()))
            }
            Expr::AwaitExpr(await_expr) => self.node("await", role, syntax, self.opt_expr("operand", await_expr.expr()).into_iter().collect()),
            Expr::MacroExpr(mac) => match mac.macro_call() {
                Some(call) => self.macro_node(role, syntax, &call),
                None => self.node("other", role, syntax, Vec::new()),
            },
            _ => self.node("other", role, syntax, Vec::new()),
        }
    }

    /// A macro call. The arguments of a known `std` macro become children (`args[0]`, …). Other
    /// macros are opaque leaves: their input is a token tree, not an expression.
    ///
    /// The input of a known macro is parsed as the argument list of a call (design 39.5): the
    /// text `_f(INPUT)` with the edition of the file. If it is not a list of expressions (for
    /// example `vec![0; n]`), the node has no children.
    fn macro_node(&self, role: impl Into<String>, value: &SyntaxNode, mac: &ast::MacroCall) -> Node {
        let name = compare_opt(mac.path().as_ref()).replace(' ', "");
        let short_name = name.rsplit("::").next().unwrap_or(&name);
        let mut children = Vec::new();
        if EXPRESSION_MACROS.contains(&short_name)
            && let Some(tree) = mac.token_tree()
        {
            children = self.macro_arguments(&tree).unwrap_or_default();
        }
        let mut node = self.node("macro", role, value, children);
        node.attr = Some(name);
        node
    }

    fn macro_arguments(&self, tree: &ast::TokenTree) -> Option<Vec<Node>> {
        const HEAD: &str = "const _: () = _f(";
        let text = tree.syntax().text().to_string();
        let inner = text.get(1..text.len().checked_sub(1)?)?;
        let wrapped = format!("{HEAD}{inner}\n);");
        let (file, errors) = crate::rs::parse_checked(&wrapped, self.edition);
        if !errors.is_empty() {
            return None;
        }
        let call = file.syntax().descendants().find_map(ast::CallExpr::cast)?;
        let args = call.arg_list()?;
        if usize::from(args.syntax().text_range().start()) != HEAD.len() - 1 {
            return None;
        }
        // The offsets of the new text: the input starts after the open delimiter.
        let inner_start = usize::from(tree.syntax().text_range().start()) + 1;
        let builder = Builder { lines: self.lines, shift: self.shift + inner_start as isize - HEAD.len() as isize, edition: self.edition, tokens: &[] };
        Some(args.args().enumerate().map(|(index, arg)| builder.expr_node(format!("args[{index}]"), &arg)).collect())
    }
}

fn is_block_like_expr(expr: &ast::Expr) -> bool {
    use ast::Expr;
    match expr {
        Expr::IfExpr(_) | Expr::MatchExpr(_) | Expr::ForExpr(_) | Expr::WhileExpr(_) | Expr::LoopExpr(_) => true,
        Expr::BlockExpr(block) => matches!(block.modifier(), None | Some(ast::BlockModifier::Label(_)) | Some(ast::BlockModifier::Unsafe(_))),
        _ => false,
    }
}

/// The slice of the tokens of a file that a node has. `None` if the list is empty.
pub(crate) fn tokens_in<'a>(tokens: &'a [Flat], node: &SyntaxNode) -> Option<&'a [Flat]> {
    if tokens.is_empty() {
        return None;
    }
    let range = node.text_range();
    let (start, end) = (usize::from(range.start()), usize::from(range.end()));
    let first = tokens.partition_point(|token| token.start < start);
    let last = tokens.partition_point(|token| token.end <= end);
    Some(&tokens[first..last.max(first)])
}

/// A macro call with braces as an expression: `m! { … }`.
fn brace_macro(expr: &ast::Expr) -> bool {
    let ast::Expr::MacroExpr(mac) = expr else { return false };
    mac.macro_call().and_then(|call| call.token_tree()).and_then(|tree| tree.l_curly_token()).is_some()
}

/// The tokens of the signature of a function: from the first qualifier (or `fn`) to the end of
/// the `where` clause, the return type, or the parameters.
pub(crate) fn signature_tokens(function: &ast::Fn) -> Vec<Flat> {
    let start = [function.const_token(), function.async_token(), function.unsafe_token(), function.fn_token()]
        .into_iter()
        .flatten()
        .map(|token| usize::from(token.text_range().start()))
        .chain(function.abi().map(|abi| usize::from(abi.syntax().text_range().start())))
        .min();
    let end = [function.where_clause().map(|n| n.syntax().clone()), function.ret_type().map(|n| n.syntax().clone()), function.param_list().map(|n| n.syntax().clone()), function.generic_param_list().map(|n| n.syntax().clone()), function.name().map(|n| n.syntax().clone())]
        .into_iter()
        .flatten()
        .map(|node| usize::from(node.text_range().end()))
        .max();
    let (Some(start), Some(end)) = (start, end) else { return Vec::new() };
    flat_tokens(function.syntax()).into_iter().filter(|token| token.start >= start && token.end <= end).collect()
}

/// The node of an expression text (for a claim pattern). The places are in the text.
pub fn expr_text_node(role: &str, text: &str) -> Option<Node> {
    let expr = crate::rs::parse_expr(text)?;
    // The text is at an offset in the wrapper of the parse.
    let start = usize::from(expr.syntax().text_range().start()) - (text.len() - text.trim_start().len());
    let lines = Lines::new(text);
    let builder = Builder { lines: &lines, shift: -(start as isize), edition: Edition::Edition2021, tokens: &[] };
    Some(builder.expr_node(role, &expr))
}

/// The `std` macros whose input is a list of expressions (design 4.1).
pub(crate) const EXPRESSION_MACROS: &[&str] = &[
    "println", "print", "eprintln", "eprint", "format", "format_args", "write", "writeln", "panic", "assert", "assert_eq",
    "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne", "vec", "dbg", "todo", "unimplemented", "unreachable",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(code: &str) -> Node {
        let file = crate::rs::parse_file(code, Edition::Edition2021).unwrap();
        let function = file.syntax().descendants().find_map(ast::Fn::cast).unwrap();
        let lines = Lines::new(code);
        Builder::new(&lines, Edition::Edition2021).block("body", &function.body().unwrap())
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
    fn known_macros_have_argument_children() {
        let body = body_of("fn f(x: u32) { println!(\"{}\", x + 1); my_macro!(a b c); }");
        let print = &body.children[0];
        assert_eq!(print.attr.as_deref(), Some("println"));
        assert_eq!(print.child("args[1]").unwrap().attr.as_deref(), Some("+"));
        assert!(body.children[1].children.is_empty(), "an unknown macro stays opaque");
        // The places of an argument are places of the file.
        let body = body_of("fn f(x: u32) {\n    println!(\n        \"{}\", x + 1);\n}");
        assert_eq!(body.children[0].child("args[1]").unwrap().start, (3, 14));
        // `vec![0; n]` is not a list of expressions.
        assert!(body_of("fn f(n: usize) { vec![0; n]; }").children[0].children.is_empty());
    }

    #[test]
    fn a_macro_with_braces_at_the_end_is_a_statement() {
        let body = body_of("fn f() { m! { a } }");
        assert_eq!(body.children[0].role, "S0");
        let body = body_of("fn f() { m!(a) }");
        assert_eq!(body.children[0].role, "tail");
        // A macro statement with `;` ends at the `;`.
        let body = body_of("fn f() { m!(a); }");
        assert_eq!((body.children[0].start, body.children[0].end), ((1, 9), (1, 15)));
    }

    #[test]
    fn positions_are_recorded() {
        let body = body_of("fn f() {\n    let a = 1;\n}");
        assert_eq!(body.children[0].start, (2, 4));
        assert_eq!(body.children[0].end, (2, 14));
    }
}
