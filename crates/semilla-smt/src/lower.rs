//! Converts a Rust expression (a tree of `ra_ap_syntax`) into the small expression tree that the encoder reads.
//!
//! The lowering also records the order of evaluation:
//!
//! - Each call (an atom that can have side effects) gets the number of earlier
//!   calls with the same text. The verifier compares the sequence of call texts
//!   of the two sides.
//! - Each read of a variable or a place gets its epoch: the number of calls that
//!   the expression evaluates before the read. A call can change a variable, so
//!   reads in different epochs are different values.

use std::collections::HashMap;

use ast::HasGenericArgs;
use semilla_syntax::rs::AstToken;
use semilla_syntax::rs::ast;

use crate::types::RustType;

/// The reason why the encoding cannot model an expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unsupported(pub String);

pub(crate) type Lowered<T> = Result<T, Unsupported>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnOp {
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    /// `&&`
    And,
    /// `||`
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub(crate) fn is_comparison(self) -> bool {
        matches!(self, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LeafKind {
    /// A local variable, for example `x`.
    Var,
    /// A constant path, for example `Self::LIMIT` or `MAX_LEN`.
    Const,
    /// A read of a place without calls, for example `self.len` or `p.x.y`.
    Place,
    /// An expression that can have side effects or can panic: a call, a
    /// method call, indexing, a macro, `?`, a block, and other forms.
    Call,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Leaf {
    /// The canonical source text. The type environment uses this text as the key.
    pub text: String,
    pub kind: LeafKind,
    /// For `Var` and `Place`: the epoch. For `Call`: the occurrence number of the text.
    pub index: usize,
    /// True for a `let` expression in a condition. Its value is a `bool`.
    pub is_let: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Bool(bool),
    /// An integer literal: the magnitude, the sign, and the type suffix.
    Int {
        mag: u128,
        neg: bool,
        suffix: Option<RustType>,
    },
    Char(u32),
    Float,
    Leaf(Leaf),
    Unary(UnOp, Box<Node>),
    Binary(BinOp, Box<Node>, Box<Node>),
    Cast(Box<Node>, RustType),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub kind: NodeKind,
    /// The type variable. The type inference sets it.
    pub tv: usize,
}

impl Node {
    fn new(kind: NodeKind) -> Node {
        Node { kind, tv: usize::MAX }
    }
}

/// One side (before or after) after lowering.
#[derive(Clone, Debug)]
pub(crate) struct Side {
    pub root: Node,
    /// The texts of the calls, in the order of evaluation.
    pub calls: Vec<String>,
}

/// Gives the canonical text of an expression: its compare text (design 39.2).
pub(crate) fn expr_text(expr: &ast::Expr) -> String {
    semilla_syntax::rs::compare(expr)
}

/// Parses Rust expression source text.
pub(crate) fn parse(text: &str) -> anyhow::Result<ast::Expr> {
    semilla_syntax::rs::parse_expr(text).ok_or_else(|| anyhow::anyhow!("cannot parse the Rust expression `{text}`: it is not one expression"))
}

pub(crate) fn lower(expr: &ast::Expr) -> Lowered<Side> {
    let mut lowerer = Lowerer::default();
    let root = lowerer.expr(expr)?;
    Ok(Side { root, calls: lowerer.calls })
}

#[derive(Default)]
struct Lowerer {
    calls: Vec<String>,
    call_counts: HashMap<String, usize>,
}

/// The inner expression of a node, or the reason why the encoding cannot model it.
fn inner(expr: Option<ast::Expr>, outer: &ast::Expr) -> Lowered<ast::Expr> {
    expr.ok_or_else(|| Unsupported(format!("the expression `{}`", expr_text(outer))))
}

impl Lowerer {
    fn leaf(&mut self, text: String, kind: LeafKind, is_let: bool) -> Node {
        let index = match kind {
            LeafKind::Var | LeafKind::Place => self.calls.len(),
            LeafKind::Const => 0,
            LeafKind::Call => {
                let count = self.call_counts.entry(text.clone()).or_insert(0);
                let index = *count;
                *count += 1;
                self.calls.push(text.clone());
                index
            }
        };
        Node::new(NodeKind::Leaf(Leaf { text, kind, index, is_let }))
    }

    fn call(&mut self, expr: &ast::Expr) -> Node {
        let is_let = matches!(expr, ast::Expr::LetExpr(_));
        self.leaf(expr_text(expr), LeafKind::Call, is_let)
    }

    fn expr(&mut self, expr: &ast::Expr) -> Lowered<Node> {
        use ast::Expr;
        match expr {
            Expr::ParenExpr(value) => self.expr(&inner(value.expr(), expr)?),
            // A reference or a dereference has the same value for the comparisons.
            Expr::RefExpr(value) => self.expr(&inner(value.expr(), expr)?),
            Expr::Literal(lit) => self.lit(lit, expr),
            Expr::PathExpr(path) => Ok(self.path(path, expr)),
            Expr::FieldExpr(_) => {
                if is_pure_place(expr) {
                    Ok(self.leaf(expr_text(expr), LeafKind::Place, false))
                } else {
                    Ok(self.call(expr))
                }
            }
            Expr::PrefixExpr(unary) => match unary.op_kind() {
                Some(ast::UnaryOp::Deref) => self.expr(&inner(unary.expr(), expr)?),
                Some(ast::UnaryOp::Not) => {
                    let value = self.expr(&inner(unary.expr(), expr)?)?;
                    Ok(Node::new(NodeKind::Unary(UnOp::Not, Box::new(value))))
                }
                Some(ast::UnaryOp::Neg) => {
                    let value = self.expr(&inner(unary.expr(), expr)?)?;
                    // Fold the sign into an integer literal, so that `-128i8` is valid.
                    if let NodeKind::Int { mag, neg, suffix } = value.kind {
                        return Ok(Node::new(NodeKind::Int {
                            mag,
                            neg: !neg && mag != 0,
                            suffix,
                        }));
                    }
                    if value.kind == NodeKind::Float {
                        return Ok(value);
                    }
                    Ok(Node::new(NodeKind::Unary(UnOp::Neg, Box::new(value))))
                }
                None => Err(Unsupported(format!("the unary operator in `{}`", expr_text(expr)))),
            },
            Expr::BinExpr(binary) => {
                use ast::{ArithOp, BinaryOp, CmpOp, LogicOp, Ordering};
                let op = match binary.op_kind() {
                    Some(BinaryOp::LogicOp(LogicOp::And)) => BinOp::And,
                    Some(BinaryOp::LogicOp(LogicOp::Or)) => BinOp::Or,
                    Some(BinaryOp::ArithOp(ArithOp::BitAnd)) => BinOp::BitAnd,
                    Some(BinaryOp::ArithOp(ArithOp::BitOr)) => BinOp::BitOr,
                    Some(BinaryOp::ArithOp(ArithOp::BitXor)) => BinOp::BitXor,
                    Some(BinaryOp::ArithOp(ArithOp::Add)) => BinOp::Add,
                    Some(BinaryOp::ArithOp(ArithOp::Sub)) => BinOp::Sub,
                    Some(BinaryOp::ArithOp(ArithOp::Mul)) => BinOp::Mul,
                    Some(BinaryOp::ArithOp(ArithOp::Div)) => BinOp::Div,
                    Some(BinaryOp::ArithOp(ArithOp::Rem)) => BinOp::Rem,
                    Some(BinaryOp::ArithOp(ArithOp::Shl)) => BinOp::Shl,
                    Some(BinaryOp::ArithOp(ArithOp::Shr)) => BinOp::Shr,
                    Some(BinaryOp::CmpOp(CmpOp::Eq { negated: false })) => BinOp::Eq,
                    Some(BinaryOp::CmpOp(CmpOp::Eq { negated: true })) => BinOp::Ne,
                    Some(BinaryOp::CmpOp(CmpOp::Ord {
                        ordering: Ordering::Less,
                        strict: true,
                    })) => BinOp::Lt,
                    Some(BinaryOp::CmpOp(CmpOp::Ord {
                        ordering: Ordering::Less,
                        strict: false,
                    })) => BinOp::Le,
                    Some(BinaryOp::CmpOp(CmpOp::Ord {
                        ordering: Ordering::Greater,
                        strict: true,
                    })) => BinOp::Gt,
                    Some(BinaryOp::CmpOp(CmpOp::Ord {
                        ordering: Ordering::Greater,
                        strict: false,
                    })) => BinOp::Ge,
                    _ => {
                        return Err(Unsupported(format!("the assignment operator in `{}`", expr_text(expr))));
                    }
                };
                // Rust evaluates the left operand first.
                let lhs = self.expr(&inner(binary.lhs(), expr)?)?;
                let rhs = self.expr(&inner(binary.rhs(), expr)?)?;
                Ok(Node::new(NodeKind::Binary(op, Box::new(lhs), Box::new(rhs))))
            }
            Expr::CastExpr(cast) => {
                let value = self.expr(&inner(cast.expr(), expr)?)?;
                let ty = cast.ty().map_or_default(|ty| semilla_syntax::rs::compare(&ty));
                let target = RustType::parse(&ty);
                if target == RustType::Unknown {
                    return Err(Unsupported(format!("the cast to the unknown type `{ty}`")));
                }
                Ok(Node::new(NodeKind::Cast(Box::new(value), target)))
            }
            // All other forms are atoms that can have side effects.
            _ => Ok(self.call(expr)),
        }
    }

    fn lit(&mut self, lit: &ast::Literal, expr: &ast::Expr) -> Lowered<Node> {
        match lit.kind() {
            ast::LiteralKind::Bool(value) => Ok(Node::new(NodeKind::Bool(value))),
            ast::LiteralKind::IntNumber(int) => {
                let text = int.text().to_string();
                let suffix = match int.suffix() {
                    None | Some("") => None,
                    Some(suffix) => match RustType::parse(suffix) {
                        RustType::Float => return Ok(Node::new(NodeKind::Float)),
                        ty @ RustType::Int { .. } => Some(ty),
                        _ => {
                            return Err(Unsupported(format!("the literal suffix in `{text}`")));
                        }
                    },
                };
                let mag = int
                    .value()
                    .map_err(|_| Unsupported(format!("the integer literal `{text}` is too large")))?;
                Ok(Node::new(NodeKind::Int { mag, neg: false, suffix }))
            }
            ast::LiteralKind::Byte(byte) => match byte.value() {
                Ok(value) => Ok(Node::new(NodeKind::Int {
                    mag: u128::from(value),
                    neg: false,
                    suffix: Some(RustType::Int { bits: 8, signed: false }),
                })),
                Err(_) => Err(Unsupported(format!("the literal `{}`", expr_text(expr)))),
            },
            ast::LiteralKind::Char(c) => match c.value() {
                Ok(value) => Ok(Node::new(NodeKind::Char(value as u32))),
                Err(_) => Err(Unsupported(format!("the literal `{}`", expr_text(expr)))),
            },
            ast::LiteralKind::FloatNumber(_) => Ok(Node::new(NodeKind::Float)),
            // Strings and other literals are constants without a model.
            _ => Ok(self.leaf(expr_text(expr), LeafKind::Const, false)),
        }
    }

    fn path(&mut self, path: &ast::PathExpr, expr: &ast::Expr) -> Node {
        let segments = path.path().map_or_default(|path| path_segments(&path));
        let plain = !segments.is_empty()
            && segments
                .iter()
                .all(|segment| segment.type_anchor().is_none() && segment.generic_arg_list().is_none() && segment.name_ref().is_some());
        let rooted = segments.first().is_some_and(|segment| segment.coloncolon_token().is_some());
        let name = |segment: &ast::PathSegment| segment.name_ref().map_or_default(|name| name.text().to_string());
        if plain && segments.len() == 1 && !rooted {
            let name = name(&segments[0]);
            let kind = if is_constant_name(&name) { LeafKind::Const } else { LeafKind::Var };
            return self.leaf(name, kind, false);
        }
        if plain && segments.len() == 2 {
            let ty = RustType::parse(&name(&segments[0]));
            if let Some(node) = int_constant(ty, &name(&segments[1])) {
                return node;
            }
        }
        self.leaf(expr_text(expr), LeafKind::Const, false)
    }
}

/// The segments of a path, from the first one.
fn path_segments(path: &ast::Path) -> Vec<ast::PathSegment> {
    let mut out = Vec::new();
    let mut next = Some(path.clone());
    while let Some(path) = next {
        out.extend(path.segment());
        next = path.qualifier();
    }
    out.reverse();
    out
}

/// True for a name in upper case, for example `MAX_LEN`. Rust uses these names for constants.
fn is_constant_name(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_uppercase()) && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Gives `u32::MAX`, `i8::MIN`, `u64::BITS`, and similar constants as literals.
fn int_constant(ty: RustType, name: &str) -> Option<Node> {
    let RustType::Int { bits, signed } = ty else {
        return None;
    };
    let (mag, neg, suffix) = match (name, signed) {
        ("MAX", false) => (u128::MAX >> (128 - bits), false, ty),
        ("MAX", true) => (u128::MAX >> (129 - bits), false, ty),
        ("MIN", false) => (0, false, ty),
        ("MIN", true) => (1u128 << (bits - 1), true, ty),
        ("BITS", _) => (u128::from(bits), false, RustType::Int { bits: 32, signed: false }),
        _ => return None,
    };
    Some(Node::new(NodeKind::Int {
        mag,
        neg,
        suffix: Some(suffix),
    }))
}

/// True if the expression reads a place and has no call: `a`, `a.b.c`, `(*a).b`.
fn is_pure_place(expr: &ast::Expr) -> bool {
    use ast::Expr;
    match expr {
        Expr::PathExpr(path) => path
            .path()
            .is_some_and(|path| path_segments(&path).iter().all(|segment| segment.type_anchor().is_none())),
        Expr::FieldExpr(field) => field.expr().is_some_and(|base| is_pure_place(&base)),
        Expr::ParenExpr(inner) => inner.expr().is_some_and(|value| is_pure_place(&value)),
        Expr::RefExpr(inner) => inner.expr().is_some_and(|value| is_pure_place(&value)),
        Expr::PrefixExpr(unary) => unary.op_kind() == Some(ast::UnaryOp::Deref) && unary.expr().is_some_and(|value| is_pure_place(&value)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(text: &str) -> Side {
        lower(&parse(text).unwrap()).unwrap()
    }

    #[test]
    fn calls_and_epochs() {
        let s = side("x < f() && x > g(x) && f() > 0");
        assert_eq!(s.calls, vec!["f ()", "g (x)", "f ()"]);
        let NodeKind::Binary(_, lhs, rhs) = &s.root.kind else { panic!() };
        // The last call to `f` has the occurrence number 1.
        let NodeKind::Binary(_, last, _) = &rhs.kind else { panic!() };
        assert!(matches!(
            &last.kind,
            NodeKind::Leaf(Leaf {
                index: 1,
                kind: LeafKind::Call,
                ..
            })
        ));
        // The second read of `x` comes after one call.
        let NodeKind::Binary(_, _, second) = &lhs.kind else { panic!() };
        let NodeKind::Binary(_, x, _) = &second.kind else { panic!() };
        assert!(matches!(
            &x.kind,
            NodeKind::Leaf(Leaf {
                index: 1,
                kind: LeafKind::Var,
                ..
            })
        ));
    }

    #[test]
    fn constants() {
        let s = side("u8::MAX");
        assert_eq!(
            s.root.kind,
            NodeKind::Int {
                mag: 255,
                neg: false,
                suffix: Some(RustType::Int { bits: 8, signed: false })
            }
        );
        let s = side("-128i8");
        assert!(matches!(s.root.kind, NodeKind::Int { mag: 128, neg: true, .. }));
        let s = side("self.len");
        assert!(matches!(&s.root.kind, NodeKind::Leaf(Leaf { kind: LeafKind::Place, .. })));
        let s = side("self.items().len");
        assert!(matches!(&s.root.kind, NodeKind::Leaf(Leaf { kind: LeafKind::Call, .. })));
    }
}
