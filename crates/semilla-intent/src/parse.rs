//! The parser of an intent file (design 67, part A).
//!
//! The parser reads the file with the syntax tree of rust-analyzer (`ra_ap_syntax`), so an
//! intent file is a valid Rust file. Then it makes the syntax tree of the language from the
//! Rust tree. A construct that the language of this batch does not have is not an error: it is
//! an `Other` node, and the check gives "claimed" with the construct as the cause. An error
//! names the file, the line, the column, and what the parser expected.

use ra_ap_syntax::ast::{self, HasArgList, HasAttrs, HasGenericArgs, HasGenericParams, HasModuleItem, HasName};
use ra_ap_syntax::{AstNode, Edition, SyntaxNode, TextRange};

use crate::prelude::{CLAIMS, EXCEPT};
use crate::{BinOp, Claim, ClaimKind, Error, Expr, ExprKind, File, Func, IPath, Item, Lit, Options, Place, Stmt, Target, Type, UnOp, Version};

/// The text of an attribute argument is parsed inside this text (the call `_a(…)`).
const WRAP: &str = "fn _f() { _a(";

/// The converter of one text: the file name, the text, the start of each line, and the offset
/// of the text in the file (for the arguments of an attribute).
struct Conv<'a> {
    file: &'a str,
    text: &'a str,
    lines: Vec<usize>,
    /// The offset in the file of the offset 0 of the parsed text, minus the wrap length.
    shift: isize,
}

impl Conv<'_> {
    fn new<'a>(file: &'a str, text: &'a str) -> Conv<'a> {
        let mut lines = vec![0];
        lines.extend(text.match_indices('\n').map(|(at, _)| at + 1));
        Conv { file, text, lines, shift: 0 }
    }

    fn at(&self, offset: usize) -> Place {
        let offset = (offset as isize + self.shift).max(0) as usize;
        let line = self.lines.partition_point(|start| *start <= offset).max(1);
        let start = self.lines[line - 1];
        let column = self.text.get(start..offset.min(self.text.len())).map(|part| part.chars().count()).unwrap_or(0) + 1;
        Place { file: self.file.to_string(), line, column }
    }

    fn place(&self, range: TextRange) -> Place {
        self.at(usize::from(range.start()))
    }

    fn node_place(&self, node: &SyntaxNode) -> Place {
        let range = node.text_range();
        let start = usize::from(range.start());
        // The first token that is not trivia (a comment before an item is a part of its node).
        let first = node.descendants_with_tokens().filter_map(|element| element.into_token()).find(|token| !token.kind().is_trivia()).map(|token| usize::from(token.text_range().start())).unwrap_or(start);
        self.at(first)
    }

    fn error(&self, node: &SyntaxNode, message: impl Into<String>) -> Error {
        Error { place: self.node_place(node), message: message.into() }
    }
}

/// Parses an intent file. Each error of the Rust parser and of the language is in the list.
pub fn parse_file(path: &str, text: &str) -> Result<File, Vec<Error>> {
    let parse = ast::SourceFile::parse(text, Edition::Edition2024);
    let conv = Conv::new(path, text);
    let mut errors: Vec<Error> = parse.errors().iter().map(|error| Error { place: conv.place(error.range()), message: format!("the Rust parser {}", rust_message(&error.to_string())) }).collect();
    if !errors.is_empty() {
        errors.sort();
        errors.dedup();
        return Err(errors);
    }
    let mut file = File { path: path.to_string(), items: Vec::new() };
    for item in parse.tree().items() {
        match item {
            ast::Item::Use(_) => {}
            ast::Item::MacroCall(call) => match intent_text(&conv, &call) {
                Ok(item) => file.items.push(item),
                Err(error) => errors.push(error),
            },
            ast::Item::Fn(function) => match function_items(&conv, &function) {
                Ok(items) => file.items.extend(items),
                Err(mut found) => errors.append(&mut found),
            },
            other => errors.push(conv.error(other.syntax(), "expected a claim (a function with an attribute), a relation (a function with no attribute), `intent!(\"TEXT\")`, or a `use` item")),
        }
    }
    if errors.is_empty() { Ok(file) } else { Err(errors) }
}

/// True if the Rust parser reads the text with no error.
pub fn rust_parses(text: &str) -> bool {
    ast::SourceFile::parse(text, Edition::Edition2024).errors().is_empty()
}

/// The message of the Rust parser, as "expected …" when it has that form.
fn rust_message(message: &str) -> String {
    match message.strip_prefix("expected ") {
        Some(rest) => format!("expected {rest}"),
        None => format!("gave the error: {message}"),
    }
}

/// `intent!("TEXT")`.
fn intent_text(conv: &Conv, call: &ast::MacroCall) -> Result<Item, Error> {
    let name = call.path().map(|path| path.syntax().text().to_string()).unwrap_or_default();
    if name != "intent" {
        return Err(conv.error(call.syntax(), format!("expected `intent!(\"TEXT\")`; `{name}!` is not an item of the intent language")));
    }
    let tree = call.token_tree().ok_or_else(|| conv.error(call.syntax(), "expected `intent!(\"TEXT\")`"))?;
    let inner = tree.syntax().text().to_string();
    let inner = inner.trim().trim_start_matches(['(', '[', '{']).trim_end_matches([')', ']', '}']).trim();
    let literal = ast::SourceFile::parse(&format!("const _: &str = {inner};"), Edition::Edition2024);
    let text = literal.tree().syntax().descendants().find_map(ast::Literal::cast).and_then(|literal| match literal.kind() {
        ast::LiteralKind::String(text) => text.value().ok().map(|value| value.to_string()),
        _ => None,
    });
    match text {
        Some(text) if literal.errors().is_empty() => Ok(Item::Intent(text, conv.node_place(call.syntax()))),
        _ => Err(conv.error(call.syntax(), "expected one string literal: `intent!(\"TEXT\")`")),
    }
}

/// The items of one function: a relation, or one claim for each target of each attribute.
fn function_items(conv: &Conv, function: &ast::Fn) -> Result<Vec<Item>, Vec<Error>> {
    let func = func(conv, function)?;
    let attrs: Vec<ast::Attr> = function.attrs().filter(|attr| attr.excl_token().is_none()).collect();
    if attrs.is_empty() {
        return Ok(vec![Item::Relation(func)]);
    }
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let mut kinds: Vec<(String, Vec<Target>, Options, Place)> = Vec::new();
    for attr in &attrs {
        match attribute(conv, attr) {
            Ok(found) => kinds.push(found),
            Err(error) => errors.push(error),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    // A claim of a later batch next to a claim of this batch (`#[same] #[pair_loop]`): the
    // claim of this batch is the claim, and the later attribute makes it "claimed".
    let later: Vec<String> = kinds.iter().filter(|(name, ..)| claim_kind(name) == ClaimKind::Later).map(|(name, ..)| name.clone()).collect();
    let main: Vec<&(String, Vec<Target>, Options, Place)> = kinds.iter().filter(|(name, ..)| claim_kind(name) != ClaimKind::Later).collect();
    let chosen: Vec<&(String, Vec<Target>, Options, Place)> = if main.is_empty() { kinds.iter().take(1).collect() } else { main };
    for (name, targets, options, place) in chosen {
        let kind = claim_kind(name);
        let mut options = options.clone();
        if kind != ClaimKind::Later && !later.is_empty() {
            // The later attribute is an observation that the check does not know yet.
            options.except.extend(later.iter().map(|name| format!("#[{name}]")));
        }
        match kind {
            ClaimKind::Rename | ClaimKind::Moved | ClaimKind::Rewrite => {
                if targets.len() != 2 {
                    errors.push(Error { place: place.clone(), message: format!("expected two targets: `#[{name}(A, B)]`") });
                    continue;
                }
                if func.body.is_some() {
                    errors.push(Error { place: func.place.clone(), message: format!("expected an empty body: `#[{name}(A, B)] fn {}() {{}}`", func.name) });
                    continue;
                }
                items.push(Item::Claim(Claim { kind, attribute: name.clone(), targets: targets.clone(), options, func: func.clone(), place: place.clone() }));
            }
            ClaimKind::Later => items.push(Item::Claim(Claim { kind, attribute: name.clone(), targets: targets.clone(), options, func: func.clone(), place: place.clone() })),
            _ => {
                if targets.is_empty() {
                    errors.push(Error { place: place.clone(), message: format!("expected an item: `#[{name}(ITEM)]`") });
                    continue;
                }
                if kind == ClaimKind::Different && !func.params.is_empty() {
                    errors.push(Error { place: func.place.clone(), message: format!("expected no parameter: a `different` claim has one input, so `fn {}()`", func.name) });
                    continue;
                }
                if kind != ClaimKind::Same && func.body.is_none() {
                    errors.push(Error { place: func.place.clone(), message: format!("expected a body: a `{name}` claim is a property, a `bool` expression") });
                    continue;
                }
                // `#[same(A, B, C)]`: one claim for each item.
                for target in targets {
                    items.push(Item::Claim(Claim { kind, attribute: name.clone(), targets: vec![target.clone()], options: options.clone(), func: func.clone(), place: place.clone() }));
                }
            }
        }
    }
    if errors.is_empty() { Ok(items) } else { Err(errors) }
}

fn claim_kind(name: &str) -> ClaimKind {
    match name {
        "same" => ClaimKind::Same,
        "change" => ClaimKind::Change,
        "different" => ClaimKind::Different,
        "rename" => ClaimKind::Rename,
        "moved" => ClaimKind::Moved,
        "rewrite" => ClaimKind::Rewrite,
        _ => ClaimKind::Later,
    }
}

/// One claim attribute: the name, the targets, and the options.
fn attribute(conv: &Conv, attr: &ast::Attr) -> Result<(String, Vec<Target>, Options, Place), Error> {
    let name = attr.path().map(|path| path.syntax().text().to_string()).unwrap_or_default();
    let place = conv.node_place(attr.syntax());
    if !CLAIMS.iter().any(|(known, _)| *known == name) {
        let names: Vec<String> = CLAIMS.iter().map(|(name, _)| name.to_string()).collect();
        return Err(Error { place, message: format!("expected a claim attribute ({}); `#[{name}]` is not one", crate::quoted_list(&names)) });
    }
    let Some(tree) = attr.syntax().descendants().find_map(ast::TokenTree::cast) else {
        return Ok((name, Vec::new(), Options::default(), place));
    };
    // The arguments of a claim of a later batch have their own form (batch 10r2 and 10r3): the
    // claim keeps their text.
    if claim_kind(&name) == ClaimKind::Later {
        let inner = tree.syntax().text().to_string();
        let text = inner[1..inner.len().saturating_sub(1)].trim().to_string();
        let targets = if text.is_empty() { Vec::new() } else { vec![Target::Text(text, place.clone())] };
        return Ok((name, targets, Options::default(), place));
    }
    let inner = tree.syntax().text().to_string();
    let body = &inner[1..inner.len().saturating_sub(1)];
    let wrapped = format!("{WRAP}{body}); }}");
    let parse = ast::SourceFile::parse(&wrapped, Edition::Edition2024);
    let mut arg_conv = Conv::new(conv.file, conv.text);
    arg_conv.shift = usize::from(tree.syntax().text_range().start()) as isize + 1 - WRAP.len() as isize;
    if let Some(error) = parse.errors().first() {
        return Err(Error { place: arg_conv.place(error.range()), message: format!("expected the arguments of `#[{name}(…)]`: paths, text literals, and `key = value` options; the Rust parser {}", rust_message(&error.to_string())) });
    }
    let call = parse.tree().syntax().descendants().find_map(ast::CallExpr::cast).ok_or_else(|| Error { place: place.clone(), message: format!("expected the arguments of `#[{name}(…)]`") })?;
    let mut targets = Vec::new();
    let mut options = Options::default();
    for arg in call.arg_list().into_iter().flat_map(|list| list.args()) {
        let arg_place = arg_conv.node_place(arg.syntax());
        match &arg {
            ast::Expr::BinExpr(bin) if matches!(bin.op_kind(), Some(ast::BinaryOp::Assignment { op: None })) => {
                let key = bin.lhs().map(|lhs| lhs.syntax().text().to_string()).unwrap_or_default();
                let value = bin.rhs();
                match (key.as_str(), value) {
                    ("cfg", Some(ast::Expr::Literal(literal))) => options.cfg = Some(string_value(&literal).ok_or_else(|| Error { place: arg_place.clone(), message: "expected a text: `cfg = \"…\"`".into() })?),
                    ("reason", Some(ast::Expr::Literal(literal))) => options.reason = Some(string_value(&literal).ok_or_else(|| Error { place: arg_place.clone(), message: "expected a text: `reason = \"…\"`".into() })?),
                    ("except", Some(ast::Expr::ArrayExpr(list))) => {
                        for item in list.exprs() {
                            let text = item.syntax().text().to_string();
                            if !text.chars().all(|c| c.is_alphanumeric() || c == '_') {
                                return Err(Error { place: arg_conv.node_place(item.syntax()), message: format!("expected a name in `except = [...]` (for example {}), not `{text}`", crate::quoted_list(&EXCEPT.iter().map(|name| name.to_string()).collect::<Vec<_>>())) });
                            }
                            options.except.push(text);
                        }
                    }
                    _ => return Err(Error { place: arg_place, message: format!("expected an option `cfg = \"…\"`, `reason = \"…\"`, or `except = [NAME, …]`, not `{}`", arg.syntax().text()) }),
                }
            }
            ast::Expr::PathExpr(path) => match path.path() {
                Some(path) => targets.push(Target::Path(ipath(&arg_conv, &path)?)),
                None => return Err(Error { place: arg_place, message: "expected a path".into() }),
            },
            ast::Expr::Literal(literal) if string_value(literal).is_some() => targets.push(Target::Text(string_value(literal).unwrap_or_default(), arg_place)),
            // A pattern of `rewrite` (`.inner`) or a path with a generic argument: the text.
            _ if name == "rewrite" => targets.push(Target::Text(arg.syntax().text().to_string(), arg_place)),
            _ => return Err(Error { place: arg_place, message: format!("expected an item path (`shop::total`, `after::shop::Cart::len`) or an option, not `{}`", arg.syntax().text()) }),
        }
    }
    Ok((name, targets, options, place))
}

fn string_value(literal: &ast::Literal) -> Option<String> {
    match literal.kind() {
        ast::LiteralKind::String(text) => text.value().ok().map(|value| value.to_string()),
        _ => None,
    }
}

/// The function: its name, parameters, return type, and body.
fn func(conv: &Conv, function: &ast::Fn) -> Result<Func, Vec<Error>> {
    let place = conv.node_place(function.syntax());
    let name = function.name().map(|name| name.text().to_string()).unwrap_or_default();
    let mut errors = Vec::new();
    let mut params = Vec::new();
    if let Some(list) = function.param_list() {
        if let Some(param) = list.self_param() {
            errors.push(conv.error(param.syntax(), "expected a parameter `NAME: TYPE`; a claim has no `self`"));
        }
        for param in list.params() {
            let pat = param.pat().map(|pat| pat.syntax().text().to_string()).unwrap_or_default();
            let simple = pat.trim_start_matches("mut ").to_string();
            if !simple.chars().all(|c| c.is_alphanumeric() || c == '_') || simple.is_empty() {
                errors.push(conv.error(param.syntax(), format!("expected a parameter name, not the pattern `{pat}`")));
                continue;
            }
            match param.ty() {
                Some(ty) => match rtype(conv, &ty) {
                    Ok(ty) => params.push((simple, ty, conv.node_place(param.syntax()))),
                    Err(error) => errors.push(error),
                },
                None => errors.push(conv.error(param.syntax(), "expected a type: `NAME: TYPE`")),
            }
        }
    }
    let ret = match function.ret_type().and_then(|ret| ret.ty()) {
        Some(ty) => match rtype(conv, &ty) {
            Ok(ty) => Some(ty),
            Err(error) => {
                errors.push(error);
                None
            }
        },
        None => None,
    };
    let body = match function.body() {
        Some(block) => {
            let empty = block.stmt_list().is_none_or(|list| list.statements().next().is_none() && list.tail_expr().is_none());
            if empty {
                None
            } else {
                match expr(conv, &ast::Expr::BlockExpr(block)) {
                    Ok(body) => Some(body),
                    Err(error) => {
                        errors.push(error);
                        None
                    }
                }
            }
        }
        None => {
            errors.push(conv.error(function.syntax(), "expected a body `{ … }`"));
            None
        }
    };
    if errors.is_empty() { Ok(Func { name, params, ret, body, place }) } else { Err(errors) }
}

/// A path with its version prefix.
fn ipath(conv: &Conv, path: &ast::Path) -> Result<IPath, Error> {
    let place = conv.node_place(path.syntax());
    let mut segments = Vec::new();
    let mut generics = Vec::new();
    for segment in path.segments() {
        if segment.type_anchor().is_some() {
            return Err(Error { place, message: format!("expected a path with no `<T as Trait>` part; give the path of the function of the version as its record name (`semilla intent check` lists the candidates), not `{}`", path.syntax().text()) });
        }
        let name = segment.name_ref().map(|name| name.text().to_string()).unwrap_or_else(|| segment.syntax().text().to_string());
        generics.clear();
        if let Some(list) = segment.generic_arg_list() {
            for arg in list.generic_args() {
                match arg {
                    ast::GenericArg::TypeArg(arg) => match arg.ty() {
                        Some(ty) => generics.push(rtype(conv, &ty)?),
                        None => return Err(conv.error(arg.syntax(), "expected a type")),
                    },
                    other => return Err(conv.error(other.syntax(), "expected a type argument")),
                }
            }
        }
        segments.push(name);
    }
    let version = match segments.first().map(String::as_str) {
        Some("before") => Some(Version::Before),
        Some("after") => Some(Version::After),
        _ => None,
    };
    if version.is_some() {
        segments.remove(0);
        if segments.is_empty() {
            return Err(Error { place, message: "expected a path after the version prefix: `before::shop::f`".into() });
        }
    }
    Ok(IPath { version, segments, generics, place })
}

/// A type.
fn rtype(conv: &Conv, ty: &ast::Type) -> Result<Type, Error> {
    Ok(match ty {
        ast::Type::PathType(path) => match path.path() {
            Some(path) => Type::Path(ipath(conv, &path)?),
            None => Type::Other(ty.syntax().text().to_string()),
        },
        ast::Type::RefType(reference) => match reference.ty() {
            Some(inner) => Type::Ref(reference.mut_token().is_some(), Box::new(rtype(conv, &inner)?)),
            None => return Err(conv.error(ty.syntax(), "expected a type after `&`")),
        },
        ast::Type::TupleType(tuple) => Type::Tuple(tuple.fields().map(|item| rtype(conv, &item)).collect::<Result<_, _>>()?),
        ast::Type::SliceType(slice) => match slice.ty() {
            Some(inner) => Type::Slice(Box::new(rtype(conv, &inner)?)),
            None => return Err(conv.error(ty.syntax(), "expected a type in `[…]`")),
        },
        ast::Type::ArrayType(array) => {
            let length = array.const_arg().map(|arg| arg.syntax().text().to_string()).and_then(|text| text.replace('_', "").parse::<u64>().ok());
            match (array.ty(), length) {
                (Some(inner), Some(length)) => Type::Array(Box::new(rtype(conv, &inner)?), length),
                _ => Type::Other(ty.syntax().text().to_string()),
            }
        }
        ast::Type::ParenType(paren) => match paren.ty() {
            Some(inner) => rtype(conv, &inner)?,
            None => Type::Other(ty.syntax().text().to_string()),
        },
        _ => Type::Other(ty.syntax().text().to_string()),
    })
}

/// An expression.
fn expr(conv: &Conv, e: &ast::Expr) -> Result<Expr, Error> {
    let place = conv.node_place(e.syntax());
    let text = e.syntax().text().to_string();
    let boxed = |inner: Option<ast::Expr>, what: &str| -> Result<Box<Expr>, Error> {
        match inner {
            Some(inner) => Ok(Box::new(expr(conv, &inner)?)),
            None => Err(Error { place: place.clone(), message: format!("expected {what}") }),
        }
    };
    let kind = match e {
        ast::Expr::Literal(literal) => ExprKind::Lit(match literal.kind() {
            ast::LiteralKind::IntNumber(number) => match number.value() {
                Ok(value) => Lit::Int(value, number.suffix().map(str::to_string)),
                Err(_) => return Err(Error { place, message: format!("expected an integer that fits in 128 bits, not `{text}`") }),
            },
            ast::LiteralKind::Bool(value) => Lit::Bool(value),
            ast::LiteralKind::String(value) => Lit::Str(value.value().map(|value| value.to_string()).unwrap_or_default()),
            ast::LiteralKind::Char(value) => match value.value() {
                Ok(value) => Lit::Char(value),
                Err(_) => Lit::Other(text.clone()),
            },
            _ => Lit::Other(text.clone()),
        }),
        ast::Expr::PathExpr(path) => match path.path() {
            Some(path) => ExprKind::Path(ipath(conv, &path)?),
            None => ExprKind::Other(text.clone()),
        },
        ast::Expr::ParenExpr(paren) => return expr(conv, &paren.expr().ok_or_else(|| Error { place: place.clone(), message: "expected an expression in `(…)`".into() })?),
        ast::Expr::PrefixExpr(prefix) => {
            let op = match prefix.op_kind() {
                Some(ast::UnaryOp::Not) => UnOp::Not,
                Some(ast::UnaryOp::Neg) => UnOp::Neg,
                Some(ast::UnaryOp::Deref) => UnOp::Deref,
                None => return Err(Error { place, message: "expected an operator".into() }),
            };
            ExprKind::Unary(op, boxed(prefix.expr(), "an operand")?)
        }
        ast::Expr::BinExpr(bin) => {
            let op = match bin.op_kind() {
                Some(ast::BinaryOp::LogicOp(ast::LogicOp::And)) => BinOp::And,
                Some(ast::BinaryOp::LogicOp(ast::LogicOp::Or)) => BinOp::Or,
                Some(ast::BinaryOp::CmpOp(ast::CmpOp::Eq { negated: false })) => BinOp::Eq,
                Some(ast::BinaryOp::CmpOp(ast::CmpOp::Eq { negated: true })) => BinOp::Ne,
                Some(ast::BinaryOp::CmpOp(ast::CmpOp::Ord { ordering: ast::Ordering::Less, strict: true })) => BinOp::Lt,
                Some(ast::BinaryOp::CmpOp(ast::CmpOp::Ord { ordering: ast::Ordering::Less, strict: false })) => BinOp::Le,
                Some(ast::BinaryOp::CmpOp(ast::CmpOp::Ord { ordering: ast::Ordering::Greater, strict: true })) => BinOp::Gt,
                Some(ast::BinaryOp::CmpOp(ast::CmpOp::Ord { ordering: ast::Ordering::Greater, strict: false })) => BinOp::Ge,
                Some(ast::BinaryOp::ArithOp(op)) => match op {
                    ast::ArithOp::Add => BinOp::Add,
                    ast::ArithOp::Sub => BinOp::Sub,
                    ast::ArithOp::Mul => BinOp::Mul,
                    ast::ArithOp::Div => BinOp::Div,
                    ast::ArithOp::Rem => BinOp::Rem,
                    ast::ArithOp::Shl => BinOp::Shl,
                    ast::ArithOp::Shr => BinOp::Shr,
                    ast::ArithOp::BitXor => BinOp::BitXor,
                    ast::ArithOp::BitOr => BinOp::BitOr,
                    ast::ArithOp::BitAnd => BinOp::BitAnd,
                },
                Some(ast::BinaryOp::Assignment { .. }) | None => return Ok(Expr { kind: ExprKind::Other("an assignment".into()), place, text }),
            };
            ExprKind::Binary(op, boxed(bin.lhs(), "a left operand")?, boxed(bin.rhs(), "a right operand")?)
        }
        ast::Expr::CallExpr(call) => {
            let args = call.arg_list().into_iter().flat_map(|list| list.args()).map(|arg| expr(conv, &arg)).collect::<Result<Vec<_>, _>>()?;
            match call.expr() {
                Some(ast::Expr::PathExpr(path)) if path.path().is_some() => ExprKind::Call(ipath(conv, &path.path().unwrap())?, args),
                _ => ExprKind::Other("a call of a value that is not a path".into()),
            }
        }
        ast::Expr::MethodCallExpr(call) => {
            if call.generic_arg_list().is_some() {
                return Ok(Expr { kind: ExprKind::Other("a method call with generic arguments".into()), place, text });
            }
            let name = call.name_ref().map(|name| name.text().to_string()).unwrap_or_default();
            let args = call.arg_list().into_iter().flat_map(|list| list.args()).map(|arg| expr(conv, &arg)).collect::<Result<Vec<_>, _>>()?;
            ExprKind::Method(boxed(call.receiver(), "a receiver")?, name, args)
        }
        ast::Expr::FieldExpr(field) => {
            let name = match field.field_access() {
                Some(ast::FieldKind::Name(name)) => name.text().to_string(),
                Some(ast::FieldKind::Index(token)) => token.text().to_string(),
                None => return Err(Error { place, message: "expected a field name".into() }),
            };
            if name.contains('.') {
                // `x.0.1` is one float token: two field reads.
                let mut inner = boxed(field.expr(), "a value")?;
                for part in name.split('.') {
                    inner = Box::new(Expr { kind: ExprKind::Field(inner, part.to_string()), place: place.clone(), text: text.clone() });
                }
                return Ok(*inner);
            }
            ExprKind::Field(boxed(field.expr(), "a value")?, name)
        }
        ast::Expr::IndexExpr(index) => ExprKind::Index(boxed(index.base(), "a sequence")?, boxed(index.index(), "an index")?),
        ast::Expr::IfExpr(e) => {
            if matches!(e.condition(), Some(ast::Expr::LetExpr(_))) {
                return Ok(Expr { kind: ExprKind::Other("`if let`".into()), place, text });
            }
            let then = boxed(e.then_branch().map(ast::Expr::BlockExpr), "a block")?;
            let other = match e.else_branch() {
                Some(ast::ElseBranch::Block(block)) => Some(Box::new(expr(conv, &ast::Expr::BlockExpr(block))?)),
                Some(ast::ElseBranch::IfExpr(inner)) => Some(Box::new(expr(conv, &ast::Expr::IfExpr(inner))?)),
                None => None,
            };
            ExprKind::If(boxed(e.condition(), "a condition")?, then, other)
        }
        ast::Expr::BlockExpr(block) => {
            if block.modifier().is_some() || block.label().is_some() {
                return Ok(Expr { kind: ExprKind::Other("a block with a modifier or a label".into()), place, text });
            }
            let mut stmts = Vec::new();
            let list = block.stmt_list();
            for stmt in list.iter().flat_map(|list| list.statements()) {
                match stmt {
                    ast::Stmt::LetStmt(statement) => {
                        let name = statement.pat().map(|pat| pat.syntax().text().to_string()).unwrap_or_default();
                        let name = name.trim_start_matches("mut ").to_string();
                        if !name.chars().all(|c| c.is_alphanumeric() || c == '_') || statement.let_else().is_some() {
                            return Ok(Expr { kind: ExprKind::Other("a `let` with a pattern".into()), place, text });
                        }
                        let ty = match statement.ty() {
                            Some(ty) => Some(rtype(conv, &ty)?),
                            None => None,
                        };
                        let Some(init) = statement.initializer() else { return Ok(Expr { kind: ExprKind::Other("a `let` with no value".into()), place, text }) };
                        stmts.push(Stmt::Let(name, ty, expr(conv, &init)?));
                    }
                    ast::Stmt::ExprStmt(statement) => match statement.expr() {
                        Some(inner) => stmts.push(Stmt::Expr(expr(conv, &inner)?)),
                        None => {}
                    },
                    ast::Stmt::Item(_) => return Ok(Expr { kind: ExprKind::Other("an item in a block".into()), place, text }),
                }
            }
            let tail = match list.and_then(|list| list.tail_expr()) {
                Some(tail) => Some(Box::new(expr(conv, &tail)?)),
                None => None,
            };
            ExprKind::Block(stmts, tail)
        }
        ast::Expr::ClosureExpr(closure) => {
            let count = closure.param_list().map(|list| list.params().count()).unwrap_or(0);
            if count > 0 {
                return Ok(Expr { kind: ExprKind::Other("a closure with parameters".into()), place, text });
            }
            ExprKind::Closure(boxed(closure.body(), "a closure body")?)
        }
        ast::Expr::RefExpr(reference) => {
            if reference.raw_token().is_some() {
                return Ok(Expr { kind: ExprKind::Other("a raw reference".into()), place, text });
            }
            ExprKind::Ref(reference.mut_token().is_some(), boxed(reference.expr(), "a value")?)
        }
        ast::Expr::TupleExpr(tuple) => ExprKind::Tuple(tuple.fields().map(|item| expr(conv, &item)).collect::<Result<_, _>>()?),
        ast::Expr::CastExpr(cast) => match cast.ty() {
            Some(ty) => ExprKind::Cast(boxed(cast.expr(), "a value")?, rtype(conv, &ty)?),
            None => return Err(Error { place, message: "expected a type after `as`".into() }),
        },
        ast::Expr::RecordExpr(record) => {
            let Some(path) = record.path() else { return Ok(Expr { kind: ExprKind::Other("a struct expression with no path".into()), place, text }) };
            let list = record.record_expr_field_list();
            if list.as_ref().is_some_and(|list| list.dotdot_token().is_some()) {
                return Ok(Expr { kind: ExprKind::Other("a struct expression with `..`".into()), place, text });
            }
            let mut fields = Vec::new();
            for field in list.iter().flat_map(|list| list.fields()) {
                let name = field.field_name().map(|name| name.text().to_string()).unwrap_or_default();
                let value = match field.expr() {
                    Some(value) => expr(conv, &value)?,
                    None => Expr { kind: ExprKind::Path(IPath { version: None, segments: vec![name.clone()], generics: Vec::new(), place: conv.node_place(field.syntax()) }), place: conv.node_place(field.syntax()), text: name.clone() },
                };
                fields.push((name, value));
            }
            ExprKind::Struct(ipath(conv, &path)?, fields)
        }
        ast::Expr::ArrayExpr(array) => {
            if array.semicolon_token().is_some() {
                return Ok(Expr { kind: ExprKind::Other("an array with a repeat count".into()), place, text });
            }
            ExprKind::Array(array.exprs().map(|item| expr(conv, &item)).collect::<Result<_, _>>()?)
        }
        ast::Expr::MatchExpr(_) => ExprKind::Other("`match`".into()),
        ast::Expr::MacroExpr(_) => ExprKind::Other("a macro call".into()),
        ast::Expr::LoopExpr(_) | ast::Expr::WhileExpr(_) | ast::Expr::ForExpr(_) => ExprKind::Other("a loop".into()),
        ast::Expr::RangeExpr(_) => ExprKind::Other("a range".into()),
        ast::Expr::TryExpr(_) => ExprKind::Other("the operator `?`".into()),
        ast::Expr::ReturnExpr(_) => ExprKind::Other("`return`".into()),
        other => ExprKind::Other(format!("the construct `{}`", other.syntax().text().to_string().chars().take(40).collect::<String>())),
    };
    Ok(Expr { kind, place, text })
}

/// [R77.3-4] A template of a claim for a function whose parameters or return type change (design
/// 67, part E; design 77): a complete claim that `semilla intent check` accepts when the agent
/// puts it in the intent file. The parameters are the parameters of the old version, then each
/// parameter of the new version with another name; the old version gets its arguments, and the
/// new version gets its arguments. With the same return type the body states that the two
/// results are equal; with another return type the body names the two results, and the agent
/// writes their relation in place of `true`. `type_path` gives the path of a type of the crate
/// by its name (`Cart` → `shop::Cart`), or `None`. A type that a claim cannot name gives the
/// cause: a generic parameter, `Self`, `impl …`, `dyn …`, or a name that no type of the crate
/// has. A lifetime has no meaning in a claim, so the template has none.
pub fn same_template(path: &str, old_signature: &str, new_signature: &str, type_path: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let names = by_name(path, type_path);
    same_template_named(path, (old_signature, &names), (new_signature, &names))
}

/// [R87.2-1] What a written type path of a signature names, for a template (design 87).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named {
    /// The path that the claim writes, with the version prefix `before::` for a type of a
    /// member crate (`before::shop::Cart`), or the path of a type of another crate.
    Path(String),
    /// The written path as it is (a primitive type, a type of the prelude).
    Written,
    /// The path names no type that a claim can name: the cause.
    Gap(String),
}

/// [R87.2-1] The names of one signature for a template (design 87): the source of each path of
/// the template. The check of the intent files resolves the same names (the compiler data of
/// the two trees), so the template takes each path from here, never from the text of a record
/// name.
pub struct Names<'a> {
    /// The answer for each written type path of the signature: the text of the path, and its
    /// byte range in the signature text.
    pub path: Box<dyn Fn(&str, usize, usize) -> Named + 'a>,
    /// The claim type of the receiver (`before::shop::Cart`), or the cause why it has none.
    pub receiver: Result<String, String>,
    /// The call path of the item in a version (`before::shop::Cart::len`), or the cause.
    pub call: Box<dyn Fn(&str) -> Result<String, String> + 'a>,
    /// The generic parameters of the impl block of the item (a method of `impl<K, V> T<K, V>`).
    pub generic: Vec<String>,
}

/// The names of the earlier form: a type of the crate by its name, the receiver from the path
/// of the item, and the call `VERSION::PATH`.
fn by_name<'a>(path: &'a str, type_path: &'a dyn Fn(&str) -> Option<String>) -> Names<'a> {
    let owner = path.rsplit_once("::").map(|(owner, _)| owner).unwrap_or(path);
    Names {
        path: Box::new(move |written: &str, _: usize, _: usize| match type_path(written).filter(|_| !written.contains("::")) {
            Some(found) => Named::Path(format!("before::{found}")),
            None => Named::Written,
        }),
        receiver: Ok(format!("before::{owner}")),
        call: Box::new(move |version: &str| Ok(format!("{version}::{path}"))),
        generic: Vec::new(),
    }
}

/// [R87.2-1] The generic parameters (types and constants) of the first impl block of a text.
pub fn impl_generics(text: &str) -> Vec<String> {
    let parse = ast::SourceFile::parse(text, Edition::Edition2024);
    let Some(block) = parse.tree().syntax().descendants().find_map(ast::Impl::cast) else { return Vec::new() };
    block.generic_param_list().into_iter().flat_map(|list| list.type_or_const_params()).filter_map(|param| param.name().map(|name| name.text().to_string())).collect()
}

/// [R87.2-1] The template of a `same` claim for two signatures, with the names of each version
/// (design 87). `path` is the scope path of the item.
pub fn same_template_named(path: &str, (old_signature, old_names): (&str, &Names), (new_signature, new_names): (&str, &Names)) -> Result<String, String> {
    let old = read_signature(old_signature, old_names)?;
    let new = read_signature(new_signature, new_names)?;
    let calls = ((old_names.call)("before")?, (new_names.call)("after")?);
    same_body(path, &old, &new, &calls)
}

/// The name, the parameters (the name and the claim type of each), and the claim type of the
/// result of a signature of a function of the syntax tree.
struct Sig {
    name: String,
    params: Vec<(String, String)>,
    /// [R82.3-2] The parameters that the function takes as `&mut` (a place).
    places: Vec<String>,
    ret: String,
}

/// [R77.3-4] Reads a signature for a template: a type that a claim cannot name gives the cause.
fn read_signature(signature: &str, names: &Names) -> Result<Sig, String> {
    {
        let parse = ast::SourceFile::parse(&format!("{signature} {{}}"), Edition::Edition2024);
        let function = parse.tree().syntax().descendants().find_map(ast::Fn::cast).ok_or_else(|| format!("the signature `{signature}` does not parse"))?;
        let name = function.name().map(|name| name.text().to_string()).ok_or_else(|| "the signature has no name".to_string())?;
        let mut generics: Vec<String> = function.generic_param_list().into_iter().flat_map(|list| list.type_or_const_params()).filter_map(|param| param.name().map(|name| name.text().to_string())).collect();
        generics.extend(names.generic.iter().cloned());
        let type_path = &*names.path;
        let list = function.param_list().ok_or_else(|| "the signature has no parameters".to_string())?;
        let ty_text = |ty: &ast::Type, what: &str| -> Result<String, String> { claim_type(ty, &generics, type_path).map_err(|cause| format!("the type `{}` of {what} has no form in a claim ({cause})", compact(&ty.syntax().text().to_string()))) };
        let mut params = Vec::new();
        let mut places = Vec::new();
        // [R69.5-3] The receiver of a method is a value of the type of the `impl` block.
        if let Some(receiver) = list.self_param() {
            // [R87.2-1] The type of the impl block, from the names of the item.
            if receiver.ty().is_some() {
                return Err("the receiver has a type of its own (`self: …`)".into());
            }
            let owner = names.receiver.clone().map_err(|cause| format!("the receiver has no form in a claim ({cause})"))?;
            let reference = if receiver.amp_token().is_some() { "&" } else { "" };
            // [R82.3-2] `&mut self`: a value of the claim that the call takes as `&mut`.
            if receiver.mut_token().is_some() && receiver.amp_token().is_some() {
                places.push("this".to_string());
                params.push(("this".to_string(), owner));
            } else {
                params.push(("this".to_string(), format!("{reference}{owner}")));
            }
        }
        for param in list.params() {
            let pat = param.pat().map(|pat| pat.syntax().text().to_string()).unwrap_or_default();
            let pat = pat.trim_start_matches("mut ").to_string();
            if !pat.chars().all(|c| c.is_alphanumeric() || c == '_') || pat.is_empty() {
                return Err(format!("the parameter `{pat}` is a pattern (a claim names each parameter)"));
            }
            let ty = param.ty().ok_or_else(|| format!("the parameter `{pat}` has no type"))?;
            // [R82.3-2] A `&mut T` parameter: a value of the type `T` that the call takes as
            // `&mut` (part A of design 82).
            if let ast::Type::RefType(reference) = &ty
                && reference.mut_token().is_some()
                && let Some(inner) = reference.ty()
            {
                let text = ty_text(&inner, &format!("the parameter `{pat}`"))?;
                places.push(pat.clone());
                params.push((pat, text));
                continue;
            }
            let text = ty_text(&ty, &format!("the parameter `{pat}`"))?;
            params.push((pat, text));
        }
        let ret = match function.ret_type().and_then(|ret| ret.ty()) {
            Some(ty) => ty_text(&ty, "the result")?,
            None => "()".to_string(),
        };
        Ok(Sig { name, params, places, ret })
    }
}

/// The template of a `same` claim for two signatures (R77.3-4, R78.1-5).
fn same_body(path: &str, old: &Sig, new: &Sig, (old_path, new_path): &(String, String)) -> Result<String, String> {
    // [R78.1-5] Rustc checks a template: an old function takes old values and a new function
    // takes new values, so a parameter of a type of the workspace (also the receiver) is one
    // parameter of each version, and `same` relates the two.
    let mut declared: Vec<String> = Vec::new();
    let mut guards: Vec<String> = Vec::new();
    let mut old_args: Vec<String> = Vec::new();
    // [R82.3-2] A place of the old version is a place of the new version too.
    for name in old.places.iter().chain(&new.places) {
        if old.params.iter().any(|(known, _)| known == name) && new.params.iter().any(|(known, _)| known == name) && (old.places.contains(name) != new.places.contains(name)) {
            return Err(format!("the parameter `{name}` is `&mut` in one version only: write the claim with a value of each version"));
        }
    }
    for (name, ty) in &old.params {
        // [R82.3-2] A place: one value of each version (the start values are the same, or
        // related), the call of each version on its own value, and the values after the calls.
        if old.places.contains(name) {
            declared.push(format!("mut {name}_before: {ty}"));
            declared.push(format!("mut {name}_after: {}", ty.replace("before::", "after::")));
            guards.push(format!("same(&{name}_before, &{name}_after)"));
            old_args.push(format!("&mut {name}_before"));
            continue;
        }
        if ty.contains("before::") {
            declared.push(format!("{name}_before: {ty}"));
            declared.push(format!("{name}_after: {}", ty.replace("before::", "after::")));
            let (b, a) = if ty.starts_with('&') { (format!("{name}_before"), format!("{name}_after")) } else { (format!("&{name}_before"), format!("&{name}_after")) };
            guards.push(format!("same({b}, {a})"));
            old_args.push(format!("{name}_before"));
        } else {
            declared.push(format!("{name}: {ty}"));
            old_args.push(name.clone());
        }
    }
    let mut new_args: Vec<String> = Vec::new();
    for (name, ty) in &new.params {
        match old.params.iter().find(|(known, _)| known == name) {
            Some(_) if new.places.contains(name) => new_args.push(format!("&mut {name}_after")),
            None if new.places.contains(name) => {
                declared.push(format!("mut {name}: {}", ty.replace("before::", "after::")));
                new_args.push(format!("&mut {name}"));
            }
            Some((_, old_ty)) if old_ty.contains("before::") => new_args.push(format!("{name}_after")),
            Some(_) => new_args.push(name.clone()),
            None => {
                declared.push(format!("{name}: {}", ty.replace("before::", "after::")));
                new_args.push(name.clone());
            }
        }
    }
    let (old_call, new_call) = (format!("{old_path}({})", old_args.join(", ")), format!("{new_path}({})", new_args.join(", ")));
    let same_type = old.ret == new.ret && !old.ret.contains("before::");
    // [R82.3-2] With a place: the two calls, the two results, and the values of the places after
    // the calls.
    let places: Vec<String> = old.places.iter().filter(|name| new.places.contains(name)).map(|name| format!("same(&{name}_before, &{name}_after)")).collect();
    if !places.is_empty() {
        let result = if same_type { "new == old" } else { "true" };
        let body = format!("!({}) || {{ let old = {old_call}; let new = {new_call}; {result} && {} }}", guards.join(" && "), places.join(" && "));
        return Ok(format!("#[same({path})] fn {}({}) -> bool {{ {body} }}", old.name, declared.join(", ")));
    }
    let body = match (guards.is_empty(), same_type) {
        (true, true) => format!("{new_call} == {old_call}"),
        (true, false) => format!("let old = {old_call}; let new = {new_call}; true"),
        (false, true) => format!("!({}) || {new_call} == {old_call}", guards.join(" && ")),
        (false, false) => format!("!({}) || {{ let old = {old_call}; let new = {new_call}; true }}", guards.join(" && ")),
    };
    Ok(format!("#[same({path})] fn {}({}) -> bool {{ {body} }}", old.name, declared.join(", ")))
}

/// [R79.5-1] The template of a claim on a function of one version: `#[new(PATH)]` for an added
/// function (it calls `after::PATH`), `#[removed(PATH)]` for a removed one (it calls
/// `before::PATH`). The parameters are the parameters of the signature, with the types of that
/// version; the agent writes the property in place of `true`. Rustc accepts the template as it
/// is (each name is a real name of the version).
pub fn version_template(kind: &str, path: &str, signature: &str, type_path: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    version_template_named(kind, path, signature, &by_name(path, type_path))
}

/// [R87.2-1] The template of a claim on a function of one version, with the names of the
/// version (design 87).
pub fn version_template_named(kind: &str, path: &str, signature: &str, names: &Names) -> Result<String, String> {
    let version = if kind == "removed" { "before" } else { "after" };
    let sig = read_signature(signature, names)?;
    let call = (names.call)(version)?;
    // [R82.3-2] A place (`&mut self`, a `&mut T` parameter) is a value of the claim that the
    // call takes as `&mut`.
    let place = |name: &String| sig.places.contains(name);
    let params: Vec<String> = sig.params.iter().map(|(name, ty)| format!("{}{name}: {}", if place(name) { "mut " } else { "" }, ty.replace("before::", &format!("{version}::")))).collect();
    let args: Vec<String> = sig.params.iter().map(|(name, _)| if place(name) { format!("&mut {name}") } else { name.clone() }).collect();
    let value = if kind == "removed" { "old" } else { "new" };
    Ok(format!("#[{kind}({path})] fn {}_{kind}({}) -> bool {{ let {value} = {call}({}); true }}", sig.name, params.join(", "), args.join(", ")))
}

/// [R79.5-1] The template of a claim on a type: a `#[relation]` of the two versions that reads
/// each field of the list (`b` is the old value, `a` the new value), or a `#[new]` or a
/// `#[removed]` claim on a type of one version. `generics` is the list of the generic
/// parameters of the type as the source has it (`<T: Clone>`, or empty), and `args` the
/// arguments (`<T>`). The agent writes the relation in place of `true`.
pub fn type_template(kind: &str, path: &str, generics: &str, args: &str, fields: &[(bool, String)]) -> String {
    let name = path.rsplit("::").next().unwrap_or(path).to_lowercase();
    let reads: String = fields.iter().map(|(new, field)| format!("let _ = &{}.{field}; ", if *new { "a" } else { "b" })).collect();
    match kind {
        "relation" => format!("#[relation(before::{path}, after::{path})] fn {name}_relation{generics}(b: &before::{path}{args}, a: &after::{path}{args}) -> bool {{ {reads}true }}"),
        "removed" => format!("#[removed({path})] fn {name}_removed{generics}(b: &before::{path}{args}) -> bool {{ {reads}let _ = b; true }}"),
        _ => format!("#[new({path})] fn {name}_new{generics}(a: &after::{path}{args}) -> bool {{ {reads}let _ = a; true }}"),
    }
}

/// [R79.5-1] The template of a claim on a constant or a static: `same` relates the two values;
/// a `new` or a `removed` claim reads the value of its version.
pub fn value_template(kind: &str, path: &str) -> String {
    let name = path.rsplit("::").next().unwrap_or(path).to_lowercase();
    match kind {
        "removed" => format!("#[removed({path})] fn {name}_removed() -> bool {{ let old = &before::{path}; true }}"),
        "new" => format!("#[new({path})] fn {name}_new() -> bool {{ let new = &after::{path}; true }}"),
        _ => format!("#[same({path})] fn {name}_same() -> bool {{ same(&before::{path}, &after::{path}) }}"),
    }
}

/// The text of a type with no space next to the marks of a path (a signature of the syntax
/// tree has a space between each two tokens).
fn compact(text: &str) -> String {
    let mut out = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for (from, to) in [("& ", "&"), (" (", "("), ("( ", "("), (" )", ")"), (" <", "<"), ("< ", "<"), (" >", ">"), (" ,", ","), (" ::", "::"), (":: ", "::"), (" [", "["), ("[ ", "["), (" ]", "]"), (" ;", ";"), (")->", ") ->")] {
        out = out.replace(from, to);
    }
    out
}

/// [R77.3-4] The text of a type of a signature in a claim: a lifetime goes away, a type of the
/// crate gets its path with the prefix `before::`, and a type that a claim cannot name gives the
/// cause.
fn claim_type(ty: &ast::Type, generics: &[String], type_path: &dyn Fn(&str, usize, usize) -> Named) -> Result<String, String> {
    Ok(match ty {
        ast::Type::RefType(reference) => {
            let inner = reference.ty().ok_or_else(|| "a reference with no type".to_string())?;
            if reference.mut_token().is_some() {
                return Err("`&mut` inside a type (a claim takes a `&mut` place as a parameter or a value of the claim only)".into());
            }
            format!("&{}", claim_type(&inner, generics, type_path)?)
        }
        ast::Type::TupleType(tuple) => format!("({})", tuple.fields().map(|item| claim_type(&item, generics, type_path)).collect::<Result<Vec<_>, _>>()?.join(", ")),
        ast::Type::SliceType(slice) => format!("[{}]", claim_type(&slice.ty().ok_or_else(|| "a slice with no type".to_string())?, generics, type_path)?),
        ast::Type::ArrayType(array) => format!("[{}; {}]", claim_type(&array.ty().ok_or_else(|| "an array with no type".to_string())?, generics, type_path)?, array.const_arg().map(|arg| arg.syntax().text().to_string()).unwrap_or_default()),
        ast::Type::ParenType(paren) => claim_type(&paren.ty().ok_or_else(|| "no type".to_string())?, generics, type_path)?,
        ast::Type::PathType(path_type) => {
            let path = path_type.path().ok_or_else(|| "no path".to_string())?;
            let segments: Vec<ast::PathSegment> = path.segments().collect();
            let last = segments.last().ok_or_else(|| "no path".to_string())?;
            let name = last.name_ref().map(|name| name.text().to_string()).unwrap_or_default();
            if name == "Self" || segments.iter().any(|segment| segment.name_ref().is_some_and(|name| name.text() == "Self")) {
                return Err("`Self`".into());
            }
            if generics.contains(&name) {
                return Err("a generic parameter".into());
            }
            let mut args = Vec::new();
            if let Some(list) = last.generic_arg_list() {
                for arg in list.generic_args() {
                    match arg {
                        ast::GenericArg::TypeArg(arg) => args.push(claim_type(&arg.ty().ok_or_else(|| "no type".to_string())?, generics, type_path)?),
                        ast::GenericArg::LifetimeArg(_) => {}
                        other => return Err(format!("the generic argument `{}`", other.syntax().text())),
                    }
                }
            }
            let tail = if args.is_empty() { String::new() } else { format!("<{}>", args.join(", ")) };
            // [R87.2-1] The names of the item give the path: a type of a member crate gets its
            // path with the version prefix; a primitive type keeps its path.
            let written = segments.iter().map(|segment| segment.name_ref().map(|name| name.text().to_string()).unwrap_or_default()).collect::<Vec<_>>().join("::");
            let range = last.name_ref().map(|name| name.syntax().text_range()).unwrap_or(path.syntax().text_range());
            match type_path(&written, usize::from(range.start()), usize::from(range.end())) {
                Named::Path(found) => format!("{found}{tail}"),
                Named::Written => format!("{written}{tail}"),
                Named::Gap(cause) => return Err(cause),
            }
        }
        ast::Type::ImplTraitType(_) => return Err("`impl …`".into()),
        ast::Type::DynTraitType(_) => return Err("`dyn …`".into()),
        other => return Err(format!("the form `{}`", other.syntax().text())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"use semilla_intent::prelude::*;

intent!("Move the non-empty check of `average` into the type `NonEmpty`");

#[same(shop::report)]
fn report() {}

#[same(shop::parse, cfg = "debug_assertions", reason = "the flag is new")]
fn parse(s: u32) -> bool {
    after::shop::parse(s, false) == before::shop::parse(s)
}

#[change(shop::average)]
fn average(v: Vec<u32>) -> bool {
    if v.is_empty() {
        panics(|| before::shop::average(&v)) && after::shop::average(&v).is_none()
    } else {
        after::shop::average(&v) == Some(before::shop::average(&v))
    }
}

#[different(shop::total)]
fn total_rounds_down() -> bool {
    before::shop::total(&[1, 2]) != after::shop::total(&[1, 2])
}

#[rename(before::shop::sum, after::shop::total)]
fn sum_to_total() {}

#[rewrite(".inner", ".as_raw()")]
fn raw() {}

fn same_items(b: &before::shop::Cart, a: &after::shop::Cart) -> bool {
    a.entries.len() == b.len && a.entries == b.items
}

#[same(depot::a, depot::b, except = [panic_message])]
fn two() {}
"#;

    // rule+ R67.1-1
    #[test]
    fn the_example_of_the_language_parses_to_its_claims() {
        let file = parse_file("x.rs", EXAMPLE).unwrap();
        let claims: Vec<&Claim> = file.claims().collect();
        let texts: Vec<String> = claims.iter().map(|claim| claim.text()).collect();
        assert_eq!(
            texts,
            vec![
                "#[same(shop::report)] fn report()",
                "#[same(shop::parse)] fn parse(s: u32)",
                "#[change(shop::average)] fn average(v: Vec<u32>)",
                "#[different(shop::total)] fn total_rounds_down()",
                "#[rename(before::shop::sum, after::shop::total)] fn sum_to_total()",
                "#[rewrite(.inner, .as_raw())] fn raw()",
                "#[same(depot::a)] fn two()",
                "#[same(depot::b)] fn two()",
            ]
        );
        assert!(matches!(&file.items[0], Item::Intent(text, place) if text.starts_with("Move the") && place.line == 3));
        assert_eq!(claims[1].options.cfg.as_deref(), Some("debug_assertions"));
        assert_eq!(claims[1].options.reason.as_deref(), Some("the flag is new"));
        assert_eq!(claims[6].options.except, vec!["panic_message".to_string()]);
        assert!(file.relation("same_items").is_some());
        assert!(!claims[0].has_body() && claims[1].has_body());
        // The place of a claim is the place of its attribute.
        assert_eq!((claims[1].place.line, claims[1].place.column), (8, 1));
        // A version prefix of a call.
        let Some(Expr { kind: ExprKind::Block(_, Some(tail)), .. }) = &claims[1].func.body else { panic!() };
        let ExprKind::Binary(BinOp::Eq, left, _) = &tail.kind else { panic!() };
        let ExprKind::Call(path, args) = &left.kind else { panic!() };
        assert_eq!((path.version, path.name(), args.len()), (Some(Version::After), "shop::parse".to_string(), 2));
        assert_eq!((path.place.line, path.place.column), (10, 5));
    }

    // rule- R67.1-1
    #[test]
    fn an_error_names_the_place_and_what_the_parser_expected() {
        let errors = parse_file("i.rs", "#[same(shop::f)]\nfn f( -> bool { true }\n").unwrap_err();
        assert!(errors[0].to_string().starts_with("i.rs:2:"), "{errors:?}");
        assert!(errors[0].message.contains("expected"), "{errors:?}");
        let errors = parse_file("i.rs", "#[sme(shop::f)]\nfn f() {}\n").unwrap_err();
        assert_eq!(errors[0].place.line, 1);
        assert!(errors[0].message.contains("expected a claim attribute") && errors[0].message.contains("`same`"), "{errors:?}");
        let errors = parse_file("i.rs", "struct S;\n").unwrap_err();
        assert!(errors[0].message.starts_with("expected a claim"), "{errors:?}");
        let errors = parse_file("i.rs", "#[different(shop::f)]\nfn f(x: u8) -> bool { true }\n").unwrap_err();
        assert!(errors[0].message.contains("expected no parameter"), "{errors:?}");
        let errors = parse_file("i.rs", "#[same(shop::f, cfg = 3)]\nfn f() {}\n").unwrap_err();
        assert_eq!((errors[0].place.line, errors[0].place.column), (1, 17), "{errors:?}");
        let errors = parse_file("i.rs", "#[rename(shop::f)]\nfn f() {}\n").unwrap_err();
        assert!(errors[0].message.contains("two targets"), "{errors:?}");
        let errors = parse_file("i.rs", "#[change(shop::f)]\nfn f() {}\n").unwrap_err();
        assert!(errors[0].message.contains("expected a body"), "{errors:?}");
    }

    // rule+ R77.3-4
    // rule- R77.3-4
    // rule- R78.1-5
    #[test]
    fn a_template_is_a_complete_claim_or_names_the_type_that_a_claim_cannot_name() {
        let types = |name: &str| (name == "Date").then(|| "cal::naive::Date".to_string());
        // Another return type: the body names the two results, and `true` waits for the relation.
        let found = same_template("cal::first", "fn first(v: &Vec<u32>) -> u32", "fn first(v: &Vec<u32>) -> Option<u32>", &types).unwrap();
        assert_eq!(found, "#[same(cal::first)] fn first(v: &Vec<u32>) -> bool { let old = before::cal::first(v); let new = after::cal::first(v); true }");
        // A lifetime goes away; a type of the crate gets its path with the version prefix.
        let found = same_template("cal::show", "fn show<'a>(d: &'a Date) -> u8", "fn show<'a>(d: &'a Date, wide: bool) -> u8", &types).unwrap();
        // [R78.1-5] A value of a type of the workspace is one parameter of each version.
        assert_eq!(found, "#[same(cal::show)] fn show(d_before: &before::cal::naive::Date, d_after: &after::cal::naive::Date, wide: bool) -> bool { !(same(d_before, d_after)) || after::cal::show(d_after, wide) == before::cal::show(d_before) }");
        // The negative cases: a type that a claim cannot name gives the cause, never a template.
        let error = same_template("cal::sum", "fn sum<I: IntoIterator>(it: I) -> u8", "fn sum<I: IntoIterator>(it: I, k: u8) -> u8", &types).unwrap_err();
        assert_eq!(error, "the type `I` of the parameter `it` has no form in a claim (a generic parameter)");
        let error = same_template("cal::sum", "fn sum(it: impl Iterator<Item = u8>) -> u8", "fn sum(it: impl Iterator<Item = u8>, k: u8) -> u8", &types).unwrap_err();
        assert!(error.contains("`impl …`"), "{error}");
        // A type that is not a type of the crate keeps its path.
        let found = same_template("cal::put", "fn put(t: std::time::Duration) -> u8", "fn put(t: std::time::Duration, k: u8) -> u8", &types).unwrap();
        assert_eq!(found, "#[same(cal::put)] fn put(t: std::time::Duration, k: u8) -> bool { after::cal::put(t, k) == before::cal::put(t) }");
    }

    // rule+ R82.3-2
    // rule- R82.3-2
    #[test]
    fn a_template_of_an_item_with_mut_has_a_place_of_each_version() {
        let types = |name: &str| (name == "Doc").then(|| "shop::Doc".to_string());
        // R82.3-2 (positive): `&mut self` is a value of each version that the call takes as
        // `&mut`; the claim compares the results and the values after the calls.
        let found = same_template("shop::Doc::push", "fn push(&mut self, x: u8)", "fn push(&mut self, x: u8)", &types).unwrap();
        assert_eq!(found, "#[same(shop::Doc::push)] fn push(mut this_before: before::shop::Doc, mut this_after: after::shop::Doc, x: u8) -> bool { !(same(&this_before, &this_after)) || { let old = before::shop::Doc::push(&mut this_before, x); let new = after::shop::Doc::push(&mut this_after, x); new == old && same(&this_before, &this_after) } }");
        // A `&mut` parameter of a type of another crate: the same form.
        let found = same_template("shop::add_to", "fn add_to(v: &mut u64, x: u64)", "fn add_to(v: &mut u64, x: u64, twice: bool)", &types).unwrap();
        assert_eq!(found, "#[same(shop::add_to)] fn add_to(mut v_before: u64, mut v_after: u64, x: u64, twice: bool) -> bool { !(same(&v_before, &v_after)) || { let old = before::shop::add_to(&mut v_before, x); let new = after::shop::add_to(&mut v_after, x, twice); new == old && same(&v_before, &v_after) } }");
        // R82.3-2 (negative): a parameter that is `&mut` in one version only, and a `&mut`
        // inside a type, give the cause, never a template.
        let error = same_template("shop::get", "fn get(v: &u64) -> u64", "fn get(v: &mut u64) -> u64", &types).unwrap_err();
        assert!(error.contains("`&mut` in one version only"), "{error}");
        let error = same_template("shop::pick", "fn pick(v: Option<&mut u64>) -> u64", "fn pick(v: Option<&mut u64>, k: u8) -> u64", &types).unwrap_err();
        assert!(error.contains("`&mut` inside a type"), "{error}");
        // [R79.5-1] The template of an added or a removed item has the same form of a place:
        // rustc accepts the call with `&mut` of a value of the claim.
        let found = version_template("new", "shop::Doc::push", "fn push(&mut self, x: u8)", &types).unwrap();
        assert_eq!(found, "#[new(shop::Doc::push)] fn push_new(mut this: after::shop::Doc, x: u8) -> bool { let new = after::shop::Doc::push(&mut this, x); true }");
        let found = version_template("removed", "shop::add_to", "fn add_to(v: &mut u64, x: u64)", &types).unwrap();
        assert_eq!(found, "#[removed(shop::add_to)] fn add_to_removed(mut v: u64, x: u64) -> bool { let old = before::shop::add_to(&mut v, x); true }");
        // A plain reference stays a parameter of the claim.
        let found = version_template("new", "shop::Doc::len", "fn len(&self) -> usize", &types).unwrap();
        assert_eq!(found, "#[new(shop::Doc::len)] fn len_new(this: &after::shop::Doc) -> bool { let new = after::shop::Doc::len(this); true }");
    }

    // rule+ R67.6-1
    #[test]
    fn a_template_has_the_parameters_of_the_old_version() {
        let none = |_: &str| None;
        let found = same_template("shop::parse", "fn parse(x: u32) -> bool", "fn parse(x: u32, strict: bool) -> bool", &none).unwrap();
        assert_eq!(found, "#[same(shop::parse)] fn parse(x: u32, strict: bool) -> bool { after::shop::parse(x, strict) == before::shop::parse(x) }");
        assert!(same_template("shop::parse", "struct S;", "fn f() {}", &none).is_err());
    }

    #[test]
    fn a_construct_outside_the_language_is_a_node_and_not_an_error() {
        let file = parse_file("i.rs", "#[same(shop::f)]\nfn f(x: u8) -> bool { match x { _ => true } }\n#[pair_loop(before = \"src/lib.rs:4\")]\nfn g() {}\n").unwrap();
        let claims: Vec<&Claim> = file.claims().collect();
        let Some(Expr { kind: ExprKind::Block(_, Some(tail)), .. }) = &claims[0].func.body else { panic!() };
        assert_eq!(tail.kind, ExprKind::Other("`match`".into()));
        assert_eq!(claims[1].kind, ClaimKind::Later);
    }
}
