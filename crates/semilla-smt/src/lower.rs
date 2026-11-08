//! Converts a `syn` expression into the small expression tree that the encoder reads.
//!
//! The lowering also records the order of evaluation:
//!
//! - Each call (an atom that can have side effects) gets the number of earlier
//!   calls with the same text. The verifier compares the sequence of call texts
//!   of the two sides.
//! - Each read of a variable or a place gets its epoch: the number of calls that
//!   the expression evaluates before the read. A call can change a variable, so
//!   reads in different epochs are different values.

use quote::ToTokens;
use std::collections::HashMap;

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
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
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
        Node {
            kind,
            tv: usize::MAX,
        }
    }
}

/// One side (before or after) after lowering.
#[derive(Clone, Debug)]
pub(crate) struct Side {
    pub root: Node,
    /// The texts of the calls, in the order of evaluation.
    pub calls: Vec<String>,
}

/// Gives the canonical text of an expression.
pub(crate) fn expr_text(expr: &syn::Expr) -> String {
    expr.to_token_stream().to_string()
}

/// Parses Rust expression source text.
pub(crate) fn parse(text: &str) -> anyhow::Result<syn::Expr> {
    syn::parse_str::<syn::Expr>(text)
        .map_err(|error| anyhow::anyhow!("cannot parse the Rust expression `{text}`: {error}"))
}

pub(crate) fn lower(expr: &syn::Expr) -> Lowered<Side> {
    let mut lowerer = Lowerer::default();
    let root = lowerer.expr(expr)?;
    Ok(Side {
        root,
        calls: lowerer.calls,
    })
}

#[derive(Default)]
struct Lowerer {
    calls: Vec<String>,
    call_counts: HashMap<String, usize>,
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
        Node::new(NodeKind::Leaf(Leaf {
            text,
            kind,
            index,
            is_let,
        }))
    }

    fn call(&mut self, expr: &syn::Expr) -> Node {
        let is_let = matches!(expr, syn::Expr::Let(_));
        self.leaf(expr_text(expr), LeafKind::Call, is_let)
    }

    fn expr(&mut self, expr: &syn::Expr) -> Lowered<Node> {
        use syn::Expr;
        match expr {
            Expr::Paren(inner) => self.expr(&inner.expr),
            Expr::Group(inner) => self.expr(&inner.expr),
            // A reference or a dereference has the same value for the comparisons.
            Expr::Reference(inner) => self.expr(&inner.expr),
            Expr::Lit(lit) => self.lit(&lit.lit, expr),
            Expr::Path(path) => Ok(self.path(path, expr)),
            Expr::Field(_) => {
                if is_pure_place(expr) {
                    Ok(self.leaf(expr_text(expr), LeafKind::Place, false))
                } else {
                    Ok(self.call(expr))
                }
            }
            Expr::Unary(unary) => match unary.op {
                syn::UnOp::Deref(_) => self.expr(&unary.expr),
                syn::UnOp::Not(_) => {
                    let inner = self.expr(&unary.expr)?;
                    Ok(Node::new(NodeKind::Unary(UnOp::Not, Box::new(inner))))
                }
                syn::UnOp::Neg(_) => {
                    let inner = self.expr(&unary.expr)?;
                    // Fold the sign into an integer literal, so that `-128i8` is valid.
                    if let NodeKind::Int { mag, neg, suffix } = inner.kind {
                        return Ok(Node::new(NodeKind::Int {
                            mag,
                            neg: !neg && mag != 0,
                            suffix,
                        }));
                    }
                    if inner.kind == NodeKind::Float {
                        return Ok(inner);
                    }
                    Ok(Node::new(NodeKind::Unary(UnOp::Neg, Box::new(inner))))
                }
                _ => Err(Unsupported(format!(
                    "the unary operator in `{}`",
                    expr_text(expr)
                ))),
            },
            Expr::Binary(binary) => {
                let op = match binary.op {
                    syn::BinOp::And(_) => BinOp::And,
                    syn::BinOp::Or(_) => BinOp::Or,
                    syn::BinOp::BitAnd(_) => BinOp::BitAnd,
                    syn::BinOp::BitOr(_) => BinOp::BitOr,
                    syn::BinOp::BitXor(_) => BinOp::BitXor,
                    syn::BinOp::Add(_) => BinOp::Add,
                    syn::BinOp::Sub(_) => BinOp::Sub,
                    syn::BinOp::Mul(_) => BinOp::Mul,
                    syn::BinOp::Div(_) => BinOp::Div,
                    syn::BinOp::Rem(_) => BinOp::Rem,
                    syn::BinOp::Shl(_) => BinOp::Shl,
                    syn::BinOp::Shr(_) => BinOp::Shr,
                    syn::BinOp::Eq(_) => BinOp::Eq,
                    syn::BinOp::Ne(_) => BinOp::Ne,
                    syn::BinOp::Lt(_) => BinOp::Lt,
                    syn::BinOp::Le(_) => BinOp::Le,
                    syn::BinOp::Gt(_) => BinOp::Gt,
                    syn::BinOp::Ge(_) => BinOp::Ge,
                    _ => {
                        return Err(Unsupported(format!(
                            "the assignment operator in `{}`",
                            expr_text(expr)
                        )));
                    }
                };
                // Rust evaluates the left operand first.
                let lhs = self.expr(&binary.left)?;
                let rhs = self.expr(&binary.right)?;
                Ok(Node::new(NodeKind::Binary(
                    op,
                    Box::new(lhs),
                    Box::new(rhs),
                )))
            }
            Expr::Cast(cast) => {
                let inner = self.expr(&cast.expr)?;
                let target = RustType::parse(&cast.ty.to_token_stream().to_string());
                if target == RustType::Unknown {
                    return Err(Unsupported(format!(
                        "the cast to the unknown type `{}`",
                        cast.ty.to_token_stream()
                    )));
                }
                Ok(Node::new(NodeKind::Cast(Box::new(inner), target)))
            }
            // All other forms are atoms that can have side effects.
            _ => Ok(self.call(expr)),
        }
    }

    fn lit(&mut self, lit: &syn::Lit, expr: &syn::Expr) -> Lowered<Node> {
        match lit {
            syn::Lit::Bool(b) => Ok(Node::new(NodeKind::Bool(b.value))),
            syn::Lit::Int(int) => {
                let suffix = int.suffix();
                let suffix = if suffix.is_empty() {
                    None
                } else {
                    match RustType::parse(suffix) {
                        RustType::Float => return Ok(Node::new(NodeKind::Float)),
                        ty @ RustType::Int { .. } => Some(ty),
                        _ => {
                            return Err(Unsupported(format!("the literal suffix in `{}`", int)));
                        }
                    }
                };
                let mag = int.base10_digits().parse::<u128>().map_err(|_| {
                    Unsupported(format!("the integer literal `{int}` is too large"))
                })?;
                Ok(Node::new(NodeKind::Int {
                    mag,
                    neg: false,
                    suffix,
                }))
            }
            syn::Lit::Byte(byte) => Ok(Node::new(NodeKind::Int {
                mag: byte.value() as u128,
                neg: false,
                suffix: Some(RustType::Int {
                    bits: 8,
                    signed: false,
                }),
            })),
            syn::Lit::Char(c) => Ok(Node::new(NodeKind::Char(c.value() as u32))),
            syn::Lit::Float(_) => Ok(Node::new(NodeKind::Float)),
            // Strings and other literals are constants without a model.
            _ => Ok(self.leaf(expr_text(expr), LeafKind::Const, false)),
        }
    }

    fn path(&mut self, path: &syn::ExprPath, expr: &syn::Expr) -> Node {
        let segments = &path.path.segments;
        let plain =
            path.qself.is_none() && segments.iter().all(|segment| segment.arguments.is_none());
        if plain && segments.len() == 1 && path.path.leading_colon.is_none() {
            let name = segments[0].ident.to_string();
            let kind = if is_constant_name(&name) {
                LeafKind::Const
            } else {
                LeafKind::Var
            };
            return self.leaf(name, kind, false);
        }
        if plain && segments.len() == 2 {
            let ty = RustType::parse(&segments[0].ident.to_string());
            if let Some(node) = int_constant(ty, &segments[1].ident.to_string()) {
                return node;
            }
        }
        self.leaf(expr_text(expr), LeafKind::Const, false)
    }
}

/// True for a name in upper case, for example `MAX_LEN`. Rust uses these names for constants.
fn is_constant_name(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
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
        ("BITS", _) => (
            bits as u128,
            false,
            RustType::Int {
                bits: 32,
                signed: false,
            },
        ),
        _ => return None,
    };
    Some(Node::new(NodeKind::Int {
        mag,
        neg,
        suffix: Some(suffix),
    }))
}

/// True if the expression reads a place and has no call: `a`, `a.b.c`, `(*a).b`.
fn is_pure_place(expr: &syn::Expr) -> bool {
    use syn::Expr;
    match expr {
        Expr::Path(path) => path.qself.is_none(),
        Expr::Field(field) => is_pure_place(&field.base),
        Expr::Paren(inner) => is_pure_place(&inner.expr),
        Expr::Group(inner) => is_pure_place(&inner.expr),
        Expr::Reference(inner) => is_pure_place(&inner.expr),
        Expr::Unary(unary) => matches!(unary.op, syn::UnOp::Deref(_)) && is_pure_place(&unary.expr),
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
        let NodeKind::Binary(_, lhs, rhs) = &s.root.kind else {
            panic!()
        };
        // The last call to `f` has the occurrence number 1.
        let NodeKind::Binary(_, last, _) = &rhs.kind else {
            panic!()
        };
        assert!(matches!(
            &last.kind,
            NodeKind::Leaf(Leaf {
                index: 1,
                kind: LeafKind::Call,
                ..
            })
        ));
        // The second read of `x` comes after one call.
        let NodeKind::Binary(_, _, second) = &lhs.kind else {
            panic!()
        };
        let NodeKind::Binary(_, x, _) = &second.kind else {
            panic!()
        };
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
                suffix: Some(RustType::Int {
                    bits: 8,
                    signed: false
                })
            }
        );
        let s = side("-128i8");
        assert!(matches!(
            s.root.kind,
            NodeKind::Int {
                mag: 128,
                neg: true,
                ..
            }
        ));
        let s = side("self.len");
        assert!(matches!(
            &s.root.kind,
            NodeKind::Leaf(Leaf {
                kind: LeafKind::Place,
                ..
            })
        ));
        let s = side("self.items().len");
        assert!(matches!(
            &s.root.kind,
            NodeKind::Leaf(Leaf {
                kind: LeafKind::Call,
                ..
            })
        ));
    }
}
