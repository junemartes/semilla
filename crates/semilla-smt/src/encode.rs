//! Type inference and the SMT-LIB encoding of two expressions.
//!
//! The encoding follows design section 8.8:
//!
//! - A known integer type becomes a bitvector with the exact width. The
//!   operations use the sign of the type.
//! - An integer of an unknown type becomes a mathematical `Int`. See
//!   [the soundness argument](crate#soundness-of-the-int-encoding).
//! - `bool` becomes `Bool`. `char` becomes a 32-bit bitvector with the valid
//!   code point range as an assumption.
//! - An atom (a call, a field read, a constant path) becomes an uninterpreted constant.
//! - Each operation that can panic adds a "no panic" condition. The guard of the
//!   operation (the short-circuit conditions above it) limits the condition.

use std::collections::{BTreeMap, HashMap};

use crate::lower::{BinOp, Leaf, LeafKind, Node, NodeKind, Side, UnOp, Unsupported};
use crate::types::{RustType, TypeEnv};

/// The reason why a check cannot use the solver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Skip {
    Unsupported(String),
    Float,
}

impl From<Unsupported> for Skip {
    fn from(value: Unsupported) -> Self {
        Skip::Unsupported(value.0)
    }
}

impl Skip {
    pub(crate) fn reason(&self) -> String {
        match self {
            Skip::Unsupported(reason) => format!("the SMT encoding does not support {reason}"),
            Skip::Float => "the MVP does not model floats".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Known {
    Bool,
    Bv { bits: u32, signed: bool },
    Char,
    Float,
}

/// The SMT sort of a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sort {
    Bool,
    Bv {
        bits: u32,
        signed: bool,
        is_char: bool,
    },
    /// A mathematical integer. `class` names the unknown Rust type.
    Int {
        class: usize,
    },
}

impl Sort {
    fn smt(self) -> String {
        match self {
            Sort::Bool => "Bool".into(),
            Sort::Bv { bits, .. } => format!("(_ BitVec {bits})"),
            Sort::Int { .. } => "Int".into(),
        }
    }
}

struct Slot {
    parent: usize,
    known: Option<Known>,
    /// The value must be an integer (an integer literal or a shift operand).
    intlike: bool,
    /// A name for error messages.
    name: Option<String>,
}

/// Type inference with union-find over type variables.
#[derive(Default)]
struct Infer {
    slots: Vec<Slot>,
    leaf_tv: HashMap<String, usize>,
}

impl Infer {
    fn fresh(&mut self, known: Option<Known>) -> usize {
        self.slots.push(Slot {
            parent: self.slots.len(),
            known,
            intlike: false,
            name: None,
        });
        self.slots.len() - 1
    }

    fn find(&mut self, v: usize) -> usize {
        let parent = self.slots[v].parent;
        if parent == v {
            return v;
        }
        let root = self.find(parent);
        self.slots[v].parent = root;
        root
    }

    fn describe(&self, v: usize) -> String {
        match &self.slots[v].name {
            Some(name) => format!("`{name}`"),
            None => "an expression".into(),
        }
    }

    fn unify(&mut self, a: usize, b: usize) -> Result<(), Unsupported> {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return Ok(());
        }
        let known = match (self.slots[a].known, self.slots[b].known) {
            (Some(x), Some(y)) if x != y => {
                return Err(Unsupported(format!(
                    "operands of different types ({} and {})",
                    self.describe(a),
                    self.describe(b)
                )));
            }
            (x, y) => x.or(y),
        };
        let intlike = self.slots[a].intlike || self.slots[b].intlike;
        let name = self.slots[a]
            .name
            .clone()
            .or_else(|| self.slots[b].name.clone());
        self.slots[b].parent = a;
        self.slots[a].known = known;
        self.slots[a].intlike = intlike;
        self.slots[a].name = name;
        self.check(a)
    }

    fn set_known(&mut self, v: usize, known: Known) -> Result<(), Unsupported> {
        let other = self.fresh(Some(known));
        self.unify(v, other)
    }

    fn set_intlike(&mut self, v: usize) -> Result<(), Unsupported> {
        let root = self.find(v);
        self.slots[root].intlike = true;
        self.check(root)
    }

    fn check(&self, root: usize) -> Result<(), Unsupported> {
        let slot = &self.slots[root];
        if slot.intlike && matches!(slot.known, Some(Known::Bool | Known::Char)) {
            return Err(Unsupported(format!(
                "an integer operation on the non-integer value {}",
                self.describe(root)
            )));
        }
        Ok(())
    }

    fn leaf(&mut self, leaf: &Leaf, env: &TypeEnv) -> Result<usize, Unsupported> {
        if let Some(&tv) = self.leaf_tv.get(&leaf.text) {
            return Ok(tv);
        }
        let known = match env.get(&leaf.text) {
            RustType::Int { bits, signed } => Some(Known::Bv { bits, signed }),
            RustType::Bool => Some(Known::Bool),
            RustType::Char => Some(Known::Char),
            RustType::Float => Some(Known::Float),
            RustType::Unknown => None,
        };
        let tv = self.fresh(known);
        self.slots[tv].name = Some(leaf.text.clone());
        self.leaf_tv.insert(leaf.text.clone(), tv);
        if leaf.is_let {
            self.set_known(tv, Known::Bool)?;
        }
        Ok(tv)
    }

    fn node(&mut self, node: &mut Node, env: &TypeEnv) -> Result<usize, Unsupported> {
        let tv = match &mut node.kind {
            NodeKind::Bool(_) => self.fresh(Some(Known::Bool)),
            NodeKind::Int { suffix, .. } => match suffix {
                Some(RustType::Int { bits, signed }) => self.fresh(Some(Known::Bv {
                    bits: *bits,
                    signed: *signed,
                })),
                _ => {
                    let tv = self.fresh(None);
                    self.slots[tv].intlike = true;
                    tv
                }
            },
            NodeKind::Char(_) => self.fresh(Some(Known::Char)),
            NodeKind::Float => self.fresh(Some(Known::Float)),
            NodeKind::Leaf(leaf) => self.leaf(leaf, env)?,
            NodeKind::Unary(_, inner) => self.node(inner, env)?,
            NodeKind::Binary(op, lhs, rhs) => {
                let op = *op;
                let l = self.node(lhs, env)?;
                let r = self.node(rhs, env)?;
                match op {
                    BinOp::And | BinOp::Or => {
                        self.set_known(l, Known::Bool)?;
                        self.set_known(r, Known::Bool)?;
                        self.fresh(Some(Known::Bool))
                    }
                    BinOp::Shl | BinOp::Shr => {
                        self.set_intlike(l)?;
                        self.set_intlike(r)?;
                        l
                    }
                    _ if op.is_comparison() => {
                        self.unify(l, r)?;
                        self.fresh(Some(Known::Bool))
                    }
                    _ => {
                        self.unify(l, r)?;
                        l
                    }
                }
            }
            NodeKind::Cast(inner, target) => {
                self.node(inner, env)?;
                let known = match *target {
                    RustType::Int { bits, signed } => Known::Bv { bits, signed },
                    RustType::Bool => Known::Bool,
                    RustType::Char => Known::Char,
                    RustType::Float => Known::Float,
                    RustType::Unknown => {
                        return Err(Unsupported("a cast to an unknown type".into()));
                    }
                };
                self.fresh(Some(known))
            }
        };
        node.tv = tv;
        Ok(tv)
    }

    fn has_float(&mut self) -> bool {
        (0..self.slots.len()).any(|v| {
            let root = self.find(v);
            self.slots[root].known == Some(Known::Float)
        })
    }

    fn sort(&mut self, tv: usize) -> Result<Sort, Skip> {
        let root = self.find(tv);
        let slot = &self.slots[root];
        match slot.known {
            Some(Known::Bool) => Ok(Sort::Bool),
            Some(Known::Bv { bits, signed }) => Ok(Sort::Bv {
                bits,
                signed,
                is_char: false,
            }),
            Some(Known::Char) => Ok(Sort::Bv {
                bits: 32,
                signed: false,
                is_char: true,
            }),
            Some(Known::Float) => Err(Skip::Float),
            None if slot.intlike => Ok(Sort::Int { class: root }),
            None => Err(Skip::Unsupported(format!(
                "the unknown type of {} (give its type in the type environment)",
                self.describe(root)
            ))),
        }
    }
}

/// A constant in the query.
#[derive(Clone, Debug)]
pub(crate) struct Sym {
    /// The SMT name, for example `v0`.
    pub name: String,
    /// The name for the reviewer, for example `x` or `v.len()`.
    pub display: String,
    pub sort: Sort,
}

/// The encoding of one side.
#[derive(Clone, Debug, Default)]
pub(crate) struct SideOut {
    pub term: String,
    /// The conditions that must be true for the side to not panic.
    pub no_panic: Vec<String>,
    /// The guard of each call, in the order of evaluation.
    pub call_guards: Vec<String>,
    pub calls: Vec<String>,
    /// True if the side has an operation that can panic.
    pub can_panic: bool,
}

/// The shape of the two expressions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Top {
    /// Both sides are conditions (`bool`).
    Condition,
    /// Both sides have the same type.
    SameType,
}

/// Two encoded expressions, ready for queries.
pub(crate) struct Problem {
    pub syms: Vec<Sym>,
    /// Declarations and definitions.
    pub decls: String,
    pub before: SideOut,
    pub after: SideOut,
    /// True if each model is a real input: all types are known, and there are no atoms.
    pub exact: bool,
}

impl Problem {
    pub(crate) fn build(
        before: &Side,
        after: &Side,
        env: &TypeEnv,
        top: Top,
    ) -> Result<Problem, Skip> {
        let mut before_root = before.root.clone();
        let mut after_root = after.root.clone();
        let mut infer = Infer::default();
        let b = infer.node(&mut before_root, env);
        let a = infer.node(&mut after_root, env);
        if infer.has_float() {
            return Err(Skip::Float);
        }
        let (b, a) = (b?, a?);
        match top {
            Top::Condition => {
                infer.set_known(b, Known::Bool)?;
                infer.set_known(a, Known::Bool)?;
            }
            Top::SameType => infer.unify(b, a)?,
        }
        if infer.has_float() {
            return Err(Skip::Float);
        }
        let sort = infer.sort(b)?;
        let mut encoder = Encoder {
            infer,
            syms: Vec::new(),
            sym_map: HashMap::new(),
            domain: Vec::new(),
            defs: BTreeMap::new(),
            exact: true,
        };
        let mut before_out = SideOut {
            calls: before.calls.clone(),
            ..SideOut::default()
        };
        before_out.term = encoder.node(&before_root, "true", &mut before_out)?;
        let mut after_out = SideOut {
            calls: after.calls.clone(),
            ..SideOut::default()
        };
        after_out.term = encoder.node(&after_root, "true", &mut after_out)?;

        let mut decls = String::new();
        for def in encoder.defs.values() {
            decls.push_str(def);
            decls.push('\n');
        }
        for sym in &encoder.syms {
            decls.push_str(&format!("; {} = {}\n", sym.name, sym.display));
            decls.push_str(&format!(
                "(declare-const {} {})\n",
                sym.name,
                sym.sort.smt()
            ));
        }
        decls.push_str(&format!(
            "(define-fun inputs_valid () Bool {})\n",
            and(&encoder.domain)
        ));
        decls.push_str(&format!(
            "(define-fun before () {} {})\n",
            sort.smt(),
            before_out.term
        ));
        decls.push_str(&format!(
            "(define-fun after () {} {})\n",
            sort.smt(),
            after_out.term
        ));
        decls.push_str(&format!(
            "(define-fun no_panic_before () Bool {})\n",
            and(&before_out.no_panic)
        ));
        decls.push_str(&format!(
            "(define-fun no_panic_after () Bool {})\n",
            and(&after_out.no_panic)
        ));
        Ok(Problem {
            syms: encoder.syms,
            decls,
            before: before_out,
            after: after_out,
            exact: encoder.exact,
        })
    }

    /// The query text before `check-sat`: the declarations and the assertions.
    pub(crate) fn body(&self, assertions: &[&str]) -> String {
        let mut body = self.decls.clone();
        for assertion in assertions {
            body.push_str(&format!("(assert {assertion})\n"));
        }
        body
    }

    /// The inputs of an unknown integer type: a model can give them a width that Rust does not
    /// have, so a model with them is not a real example (B3).
    pub(crate) fn unknown_width_inputs(&self) -> Vec<String> {
        self.syms.iter().filter(|sym| matches!(sym.sort, Sort::Int { .. })).map(|sym| sym.display.clone()).collect()
    }

    pub(crate) fn sym_names(&self) -> Vec<String> {
        self.syms.iter().map(|sym| sym.name.clone()).collect()
    }

    /// Formats a model as `name = value` pairs.
    pub(crate) fn witness(&self, model: &[(String, String)]) -> String {
        let values: HashMap<&str, &str> = model
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let parts: Vec<String> = self
            .syms
            .iter()
            .filter_map(|sym| {
                let value = values.get(sym.name.as_str())?;
                Some(format!(
                    "{} = {}",
                    sym.display,
                    format_value(value, sym.sort)
                ))
            })
            .collect();
        if parts.is_empty() {
            "any input".to_string()
        } else {
            parts.join(", ")
        }
    }
}

struct Encoder {
    infer: Infer,
    syms: Vec<Sym>,
    sym_map: HashMap<(String, usize), usize>,
    /// Assumptions on the inputs, for example the valid range of a `char`.
    domain: Vec<String>,
    /// Declarations of helper functions, by name.
    defs: BTreeMap<String, String>,
    exact: bool,
}

impl Encoder {
    fn sort(&mut self, node: &Node) -> Result<Sort, Skip> {
        self.infer.sort(node.tv)
    }

    /// Declares the range predicate of an unknown integer type.
    fn in_range(&mut self, class: usize, term: &str) -> String {
        let name = format!("in_range_{class}");
        self.defs
            .entry(name.clone())
            .or_insert_with(|| format!("(declare-fun {name} (Int) Bool)"));
        self.exact = false;
        format!("({name} {term})")
    }

    /// Declares an uninterpreted function for an operation that the Int encoding cannot model.
    fn opaque(&mut self, op: &str, class: usize, args: &[&str]) -> String {
        let name = format!("int_{op}_{class}");
        let params = vec!["Int"; args.len()].join(" ");
        self.defs
            .entry(name.clone())
            .or_insert_with(|| format!("(declare-fun {name} ({params}) Int)"));
        self.exact = false;
        format!("({name} {})", args.join(" "))
    }

    fn truncating_division(&mut self) {
        self.defs.entry("tdiv".into()).or_insert_with(|| {
            "; Rust division rounds toward zero.\n\
             (define-fun tdiv ((a Int) (b Int)) Int (ite (>= a 0) (div a b) (- (div (- a) b))))\n\
             (define-fun trem ((a Int) (b Int)) Int (- a (* b (tdiv a b))))"
                .into()
        });
    }

    fn leaf(
        &mut self,
        node: &Node,
        leaf: &Leaf,
        guard: &str,
        out: &mut SideOut,
    ) -> Result<String, Skip> {
        let sort = self.sort(node)?;
        if leaf.kind == LeafKind::Call {
            out.call_guards.push(guard.to_string());
        }
        let key = (leaf.text.clone(), leaf.index);
        if let Some(&index) = self.sym_map.get(&key) {
            return Ok(self.syms[index].name.clone());
        }
        let name = format!("v{}", self.syms.len());
        let display = match leaf.kind {
            LeafKind::Var | LeafKind::Place if leaf.index > 0 => {
                format!("{} (after {} calls)", leaf.text, leaf.index)
            }
            LeafKind::Call if leaf.index > 0 => format!("{} (call {})", leaf.text, leaf.index + 1),
            _ => leaf.text.clone(),
        };
        if !(leaf.kind == LeafKind::Var && leaf.index == 0) {
            self.exact = false;
        }
        match sort {
            Sort::Bv { is_char: true, .. } => self.domain.push(format!(
                "(and (bvule {name} #x0010ffff) (not (and (bvuge {name} #x0000d800) (bvule {name} #x0000dfff))))"
            )),
            Sort::Int { class } => {
                let fact = self.in_range(class, &name);
                self.domain.push(fact);
            }
            _ => {}
        }
        self.sym_map.insert(key, self.syms.len());
        self.syms.push(Sym {
            name: name.clone(),
            display,
            sort,
        });
        Ok(name)
    }

    /// An overflow check. A build with no overflow checks does not panic: the operation wraps.
    /// Returns true if the build has overflow checks. If not, the caller gives the term of the
    /// wrapped result.
    fn overflow(&mut self, out: &mut SideOut, guard: &str, ok: String) -> bool {
        let checks = crate::build::current().overflow_checks;
        if checks {
            self.no_panic(out, guard, ok);
        }
        checks
    }

    /// The wrapped result of an operation on integers with no known width: a function of the
    /// true result. The encoding does not know the function.
    fn wrapped(&mut self, class: usize, result: String) -> String {
        self.exact = false;
        self.opaque("wrap", class, &[&result])
    }

    fn no_panic(&mut self, out: &mut SideOut, guard: &str, ok: String) {
        out.can_panic = true;
        out.no_panic.push(implies(guard, &ok));
    }

    fn node(&mut self, node: &Node, guard: &str, out: &mut SideOut) -> Result<String, Skip> {
        match &node.kind {
            NodeKind::Bool(value) => Ok(value.to_string()),
            NodeKind::Int { mag, neg, .. } => self.int_literal(node, *mag, *neg),
            NodeKind::Char(code) => Ok(bv(*code as u128, 32)),
            NodeKind::Float => Err(Skip::Float),
            NodeKind::Leaf(leaf) => self.leaf(node, leaf, guard, out),
            NodeKind::Unary(op, inner) => {
                let sort = self.sort(node)?;
                let term = self.node(inner, guard, out)?;
                match (op, sort) {
                    (UnOp::Not, Sort::Bool) => Ok(format!("(not {term})")),
                    (UnOp::Not, Sort::Bv { is_char: false, .. }) => Ok(format!("(bvnot {term})")),
                    (UnOp::Not, Sort::Int { class }) => Ok(self.opaque("not", class, &[&term])),
                    (
                        UnOp::Neg,
                        Sort::Bv {
                            bits,
                            signed: true,
                            is_char: false,
                        },
                    ) => {
                        self.overflow(out, guard, format!("(not (= {term} {}))", signed_min(bits)));
                        Ok(format!("(bvneg {term})"))
                    }
                    (UnOp::Neg, Sort::Int { class }) => {
                        let result = format!("(- {term})");
                        let ok = self.in_range(class, &result);
                        if self.overflow(out, guard, ok) { Ok(result) } else { Ok(self.wrapped(class, result)) }
                    }
                    _ => Err(Skip::Unsupported(format!(
                        "the operator {op:?} on the type {sort:?}"
                    ))),
                }
            }
            NodeKind::Binary(op, lhs, rhs) => self.binary(node, *op, lhs, rhs, guard, out),
            NodeKind::Cast(inner, target) => self.cast(inner, *target, guard, out),
        }
    }

    fn int_literal(&mut self, node: &Node, mag: u128, neg: bool) -> Result<String, Skip> {
        match self.sort(node)? {
            Sort::Bv {
                bits,
                signed,
                is_char: false,
            } => {
                let fits = match (signed, neg) {
                    (false, false) => bits == 128 || mag < (1u128 << bits),
                    (false, true) => false,
                    (true, false) => mag < (1u128 << (bits - 1)),
                    (true, true) => mag <= (1u128 << (bits - 1)),
                };
                if !fits {
                    return Err(Skip::Unsupported(format!(
                        "the literal {}{mag}, which does not fit in its type",
                        if neg { "-" } else { "" }
                    )));
                }
                let value = if neg { mag.wrapping_neg() } else { mag } & mask(bits);
                Ok(bv(value, bits))
            }
            Sort::Int { class } => {
                let term = if neg {
                    format!("(- {mag})")
                } else {
                    mag.to_string()
                };
                // The program compiles, so the literal fits in its type.
                let fact = self.in_range(class, &term);
                self.domain.push(fact);
                Ok(term)
            }
            sort => Err(Skip::Unsupported(format!(
                "an integer literal of the type {sort:?}"
            ))),
        }
    }

    fn binary(
        &mut self,
        node: &Node,
        op: BinOp,
        lhs: &Node,
        rhs: &Node,
        guard: &str,
        out: &mut SideOut,
    ) -> Result<String, Skip> {
        if matches!(op, BinOp::And | BinOp::Or) {
            let l = self.node(lhs, guard, out)?;
            // The right operand runs only when the left operand does not decide the result.
            let rhs_guard = match op {
                BinOp::And => and_guard(guard, &l),
                _ => and_guard(guard, &format!("(not {l})")),
            };
            let r = self.node(rhs, &rhs_guard, out)?;
            let smt_op = if op == BinOp::And { "and" } else { "or" };
            return Ok(format!("({smt_op} {l} {r})"));
        }
        if matches!(op, BinOp::Shl | BinOp::Shr) {
            return self.shift(node, op, lhs, rhs, guard, out);
        }
        let sort = self.sort(lhs)?;
        let l = self.node(lhs, guard, out)?;
        let r = self.node(rhs, guard, out)?;
        let unsupported = || {
            Err(Skip::Unsupported(format!(
                "the operator {op:?} on the type {sort:?}"
            )))
        };
        match sort {
            Sort::Bool => match op {
                BinOp::BitAnd => Ok(format!("(and {l} {r})")),
                BinOp::BitOr => Ok(format!("(or {l} {r})")),
                BinOp::BitXor => Ok(format!("(xor {l} {r})")),
                BinOp::Eq => Ok(format!("(= {l} {r})")),
                BinOp::Ne => Ok(format!("(distinct {l} {r})")),
                _ => unsupported(),
            },
            Sort::Bv { is_char: true, .. } => match op {
                BinOp::Eq => Ok(format!("(= {l} {r})")),
                BinOp::Ne => Ok(format!("(distinct {l} {r})")),
                BinOp::Lt => Ok(format!("(bvult {l} {r})")),
                BinOp::Le => Ok(format!("(bvule {l} {r})")),
                BinOp::Gt => Ok(format!("(bvugt {l} {r})")),
                BinOp::Ge => Ok(format!("(bvuge {l} {r})")),
                _ => unsupported(),
            },
            Sort::Bv { bits, signed, .. } => {
                let s = if signed { "s" } else { "u" };
                match op {
                    BinOp::Eq => Ok(format!("(= {l} {r})")),
                    BinOp::Ne => Ok(format!("(distinct {l} {r})")),
                    BinOp::Lt => Ok(format!("(bv{s}lt {l} {r})")),
                    BinOp::Le => Ok(format!("(bv{s}le {l} {r})")),
                    BinOp::Gt => Ok(format!("(bv{s}gt {l} {r})")),
                    BinOp::Ge => Ok(format!("(bv{s}ge {l} {r})")),
                    BinOp::BitAnd => Ok(format!("(bvand {l} {r})")),
                    BinOp::BitOr => Ok(format!("(bvor {l} {r})")),
                    BinOp::BitXor => Ok(format!("(bvxor {l} {r})")),
                    BinOp::Add | BinOp::Sub | BinOp::Mul => {
                        let smt_op = match op {
                            BinOp::Add => "bvadd",
                            BinOp::Sub => "bvsub",
                            _ => "bvmul",
                        };
                        // The operation does not overflow if the result is the same in a wider type.
                        let extra = if op == BinOp::Mul { bits } else { 1 };
                        let ext = if signed { "sign_extend" } else { "zero_extend" };
                        let result = format!("({smt_op} {l} {r})");
                        let ok = format!(
                            "(= ((_ {ext} {extra}) {result}) ({smt_op} ((_ {ext} {extra}) {l}) ((_ {ext} {extra}) {r})))"
                        );
                        // The bit vector operation gives the wrapped result.
                        self.overflow(out, guard, ok);
                        Ok(result)
                    }
                    BinOp::Div | BinOp::Rem => {
                        let mut ok = format!("(not (= {r} {}))", bv(0, bits));
                        if signed {
                            ok = format!(
                                "(and {ok} (not (and (= {l} {}) (= {r} {}))))",
                                signed_min(bits),
                                bv(mask(bits), bits)
                            );
                        }
                        self.no_panic(out, guard, ok);
                        let smt_op = match (op, signed) {
                            (BinOp::Div, true) => "bvsdiv",
                            (BinOp::Div, false) => "bvudiv",
                            (_, true) => "bvsrem",
                            (_, false) => "bvurem",
                        };
                        Ok(format!("({smt_op} {l} {r})"))
                    }
                    _ => unsupported(),
                }
            }
            Sort::Int { class } => match op {
                BinOp::Eq => Ok(format!("(= {l} {r})")),
                BinOp::Ne => Ok(format!("(distinct {l} {r})")),
                BinOp::Lt => Ok(format!("(< {l} {r})")),
                BinOp::Le => Ok(format!("(<= {l} {r})")),
                BinOp::Gt => Ok(format!("(> {l} {r})")),
                BinOp::Ge => Ok(format!("(>= {l} {r})")),
                BinOp::BitAnd => Ok(self.opaque("and", class, &[&l, &r])),
                BinOp::BitOr => Ok(self.opaque("or", class, &[&l, &r])),
                BinOp::BitXor => Ok(self.opaque("xor", class, &[&l, &r])),
                BinOp::Add | BinOp::Sub | BinOp::Mul => {
                    let smt_op = match op {
                        BinOp::Add => "+",
                        BinOp::Sub => "-",
                        _ => "*",
                    };
                    let result = format!("({smt_op} {l} {r})");
                    let ok = self.in_range(class, &result);
                    if self.overflow(out, guard, ok) { Ok(result) } else { Ok(self.wrapped(class, result)) }
                }
                BinOp::Div | BinOp::Rem => {
                    self.truncating_division();
                    // The quotient is out of range only for a divisor of -1: then
                    // `|quotient| <= |dividend|` is false only for `MIN / -1`.
                    // `MIN / -1` and `MIN % -1` panic.
                    let quotient = format!("(tdiv {l} {r})");
                    let negation = self.in_range(class, &format!("(- {l})"));
                    self.no_panic(
                        out,
                        guard,
                        format!("(and (not (= {r} 0)) (or (not (= {r} (- 1))) {negation}))"),
                    );
                    if op == BinOp::Div {
                        Ok(quotient)
                    } else {
                        Ok(format!("(trem {l} {r})"))
                    }
                }
                _ => unsupported(),
            },
        }
    }

    fn shift(
        &mut self,
        node: &Node,
        op: BinOp,
        lhs: &Node,
        rhs: &Node,
        guard: &str,
        out: &mut SideOut,
    ) -> Result<String, Skip> {
        let sort = self.sort(node)?;
        let l = self.node(lhs, guard, out)?;
        let literal = match rhs.kind {
            NodeKind::Int {
                mag, neg: false, ..
            } => Some(mag),
            NodeKind::Int { neg: true, .. } => {
                return Err(Skip::Unsupported("a shift by a negative amount".into()));
            }
            _ => None,
        };
        match sort {
            Sort::Bv {
                bits,
                signed,
                is_char: false,
            } => {
                let amount = match literal {
                    Some(k) => {
                        if k >= bits as u128 {
                            return Err(Skip::Unsupported(format!(
                                "a shift by {k} on a {bits}-bit value"
                            )));
                        }
                        // Rust checks the type of the literal. Do not encode it.
                        bv(k, bits)
                    }
                    None => {
                        let r = self.node(rhs, guard, out)?;
                        let (ok, amount) = match self.sort(rhs)? {
                            Sort::Bv {
                                bits: m,
                                signed: rs,
                                is_char: false,
                            } => {
                                let n = bits as u128;
                                let ok = if rs {
                                    let max = (1u128 << (m - 1)) - 1;
                                    if n <= max {
                                        format!(
                                            "(and (bvsge {r} {}) (bvslt {r} {}))",
                                            bv(0, m),
                                            bv(n, m)
                                        )
                                    } else {
                                        format!("(bvsge {r} {})", bv(0, m))
                                    }
                                } else if m >= 128 || n < (1u128 << m) {
                                    format!("(bvult {r} {})", bv(n, m))
                                } else {
                                    "true".to_string()
                                };
                                let amount = if m < bits {
                                    format!("((_ zero_extend {}) {r})", bits - m)
                                } else if m > bits {
                                    format!("((_ extract {} 0) {r})", bits - 1)
                                } else {
                                    r
                                };
                                (ok, amount)
                            }
                            Sort::Int { .. } => (
                                format!("(and (>= {r} 0) (< {r} {bits}))"),
                                format!("((_ int2bv {bits}) {r})"),
                            ),
                            other => {
                                return Err(Skip::Unsupported(format!(
                                    "a shift amount of the type {other:?}"
                                )));
                            }
                        };
                        if self.overflow(out, guard, ok) {
                            amount
                        } else {
                            // With no overflow checks, the shift uses the low bits of the amount.
                            format!("(bvand {amount} {})", bv(bits as u128 - 1, bits))
                        }
                    }
                };
                let smt_op = match (op, signed) {
                    (BinOp::Shl, _) => "bvshl",
                    (_, true) => "bvashr",
                    (_, false) => "bvlshr",
                };
                Ok(format!("({smt_op} {l} {amount})"))
            }
            Sort::Int { class } => {
                let Some(k) = literal else {
                    return Err(Skip::Unsupported(
                        "a shift by a variable amount on a value of an unknown type".into(),
                    ));
                };
                if k >= 128 {
                    return Err(Skip::Unsupported(format!("a shift by {k}")));
                }
                // The shift panics when the amount is not less than the unknown width.
                let name = format!("shift_ok_{class}");
                self.defs
                    .entry(name.clone())
                    .or_insert_with(|| format!("(declare-fun {name} (Int) Bool)"));
                let checks = self.overflow(out, guard, format!("({name} {k})"));
                if op == BinOp::Shr && checks {
                    // An arithmetic or logical right shift is a division by 2^k that rounds down.
                    Ok(format!("(div {l} {})", 1u128 << k))
                } else {
                    // A left shift drops high bits without a panic. The encoding cannot model that.
                    Ok(self.opaque("shl", class, &[&l, &k.to_string()]))
                }
            }
            other => Err(Skip::Unsupported(format!(
                "a shift of a value of the type {other:?}"
            ))),
        }
    }

    fn cast(
        &mut self,
        inner: &Node,
        target: RustType,
        guard: &str,
        out: &mut SideOut,
    ) -> Result<String, Skip> {
        let source = self.sort(inner)?;
        let term = self.node(inner, guard, out)?;
        match target {
            RustType::Int { bits, .. } => match source {
                Sort::Bool => Ok(format!("(ite {term} {} {})", bv(1, bits), bv(0, bits))),
                Sort::Bv {
                    bits: m, signed, ..
                } => Ok(if m < bits {
                    let ext = if signed { "sign_extend" } else { "zero_extend" };
                    format!("((_ {ext} {}) {term})", bits - m)
                } else if m > bits {
                    format!("((_ extract {} 0) {term})", bits - 1)
                } else {
                    term
                }),
                // For any integer type, `x as uN` and `x as iN` keep the value modulo 2^N.
                Sort::Int { .. } => Ok(format!("((_ int2bv {bits}) {term})")),
            },
            RustType::Char => match source {
                Sort::Bv {
                    bits: 8,
                    signed: false,
                    is_char: false,
                } => Ok(format!("((_ zero_extend 24) {term})")),
                Sort::Bv { is_char: true, .. } => Ok(term),
                _ => Err(Skip::Unsupported(
                    "a cast to `char` from a type other than `u8`".into(),
                )),
            },
            RustType::Float => Err(Skip::Float),
            _ => Err(Skip::Unsupported(format!("a cast to {target:?}"))),
        }
    }
}

fn and(items: &[String]) -> String {
    match items.len() {
        0 => "true".into(),
        1 => items[0].clone(),
        _ => format!("(and {})", items.join(" ")),
    }
}

fn and_guard(guard: &str, cond: &str) -> String {
    if guard == "true" {
        cond.to_string()
    } else {
        format!("(and {guard} {cond})")
    }
}

fn implies(guard: &str, ok: &str) -> String {
    if guard == "true" {
        ok.to_string()
    } else {
        format!("(=> {guard} {ok})")
    }
}

fn mask(bits: u32) -> u128 {
    if bits >= 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    }
}

fn bv(value: u128, bits: u32) -> String {
    format!("(_ bv{value} {bits})")
}

fn signed_min(bits: u32) -> String {
    bv(1u128 << (bits - 1), bits)
}

/// Formats a value from a model in Rust syntax.
pub(crate) fn format_value(text: &str, sort: Sort) -> String {
    use crate::solver::sexp::{Sexp, parse_one};
    let Some(expr) = parse_one(text) else {
        return text.to_string();
    };
    match sort {
        Sort::Bv {
            bits,
            signed,
            is_char,
        } => {
            let raw = match &expr {
                Sexp::Atom(atom) if atom.starts_with("#x") => {
                    u128::from_str_radix(&atom[2..], 16).ok()
                }
                Sexp::Atom(atom) if atom.starts_with("#b") => {
                    u128::from_str_radix(&atom[2..], 2).ok()
                }
                Sexp::List(items) if items.len() == 3 => match &items[1] {
                    Sexp::Atom(atom) => atom.strip_prefix("bv").and_then(|d| d.parse().ok()),
                    _ => None,
                },
                _ => None,
            };
            let Some(raw) = raw else {
                return text.to_string();
            };
            if is_char {
                return match char::from_u32(raw as u32) {
                    Some(c) if !c.is_control() => format!("{c:?}"),
                    _ => format!("'\\u{{{raw:x}}}'"),
                };
            }
            if signed && bits < 128 && raw >= (1u128 << (bits - 1)) {
                ((raw as i128) - (1i128 << bits)).to_string()
            } else if signed {
                (raw as i128).to_string()
            } else {
                raw.to_string()
            }
        }
        Sort::Int { .. } => match &expr {
            Sexp::List(items) if items.len() == 2 && items[0] == Sexp::Atom("-".into()) => {
                format!("-{}", items[1])
            }
            other => other.to_string(),
        },
        Sort::Bool => expr.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_values() {
        let i8s = Sort::Bv {
            bits: 8,
            signed: true,
            is_char: false,
        };
        assert_eq!(format_value("#xff", i8s), "-1");
        assert_eq!(format_value("#x7f", i8s), "127");
        let u8s = Sort::Bv {
            bits: 8,
            signed: false,
            is_char: false,
        };
        assert_eq!(format_value("#xff", u8s), "255");
        assert_eq!(format_value("#b00000011", u8s), "3");
        let c = Sort::Bv {
            bits: 32,
            signed: false,
            is_char: true,
        };
        assert_eq!(format_value("#x00000061", c), "'a'");
        assert_eq!(format_value("(- 5)", Sort::Int { class: 0 }), "-5");
        let i128s = Sort::Bv {
            bits: 128,
            signed: true,
            is_char: false,
        };
        assert_eq!(
            format_value(&format!("(_ bv{} 128)", u128::MAX), i128s),
            "-1"
        );
    }
}

