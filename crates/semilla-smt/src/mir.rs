//! The equivalence of two versions of a body (design 61, part C).
//!
//! The driver writes the MIR of each body at N0 in a small structured form (`q` of the record,
//! `semilla-mir/src/ir.rs`). This module runs each version of a body on symbolic inputs, one
//! path at a time, and asks Z3 if an input exists for which the two versions give different
//! outcomes.
//!
//! # The outcome
//!
//! The outcome of a body for one input is:
//!
//! - the exit: the return value, or a panic with its kind (`overflow add`, `division by zero`, the
//!   call of a function that never returns with its arguments), or an abort;
//! - the ordered list of the effects: each call that the check does not inline (the callee text
//!   and the argument values), and each drop of a value with a destructor;
//! - the value behind each `&mut` parameter at the exit, and the values that the cleanup path of
//!   a panic drops.
//!
//! Two bodies are equivalent if each input gives the same outcome.
//!
//! # Soundness
//!
//! - "Equivalent" comes only from an `unsat` answer: no input exists with a different outcome.
//! - "Different" comes only from an exact model: the path conditions and the difference of the
//!   selected pair of paths use only the inputs of modeled types (integers, `bool`, `char`, and
//!   tuples, structs, and enums of them). A call result, a value of an opaque type, and the
//!   identity of a reference make a model inexact.
//! - A part that the check does not model (a float operation, a raw pointer, an index, a loop,
//!   a closure value, inline assembly) on a path gives "not decided" with its cause. The check
//!   never skips a part.
//! - A call that the check does not inline has the effect summary of the driver (design 64). A
//!   call with no effect is a value: a function of the callee and of its inputs (each argument
//!   and the value behind each reference in it). A call with an effect on the world (or with no
//!   summary) is in the list of the effects: the k-th effect of the two sides has a result that
//!   is a function of the callee, of k, and of the inputs, so the two sides must make these calls
//!   in the same order. A call writes through each `&mut` argument: the target gets a new value
//!   that is a function of the call. A call or a drop that can stop (a panic, a loop) gives a
//!   second path that stops at it.
//! - A `Vec`, a slice, or an array of scalars is a sequence of the solver with a length of 64
//!   bits, and the check models the main operations of `Vec` and slices with their panics.
//! - A callee that the step changes must be inlined: else the result is "not decided".
//! - The check assumes no undefined behavior, as the compiler does: an `unreachable` terminator
//!   ends no path, and an unchecked operation wraps.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;

use crate::solver::{Answer, Solver};

// ---------------------------------------------------------------------------------------------
// The form of the driver.

/// The form of one body (see `semilla-mir/src/ir.rs`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Ir {
    #[serde(default)]
    pub a: usize,
    #[serde(default)]
    pub l: Vec<u32>,
    #[serde(default)]
    pub t: Vec<IrTy>,
    #[serde(default)]
    pub b: Vec<IrBlock>,
    #[serde(default)]
    pub f: Vec<String>,
    #[serde(default)]
    pub n: Vec<String>,
    #[serde(default)]
    pub u: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub enum IrTy {
    Bool,
    Char,
    Unit,
    Never,
    Int(u32, bool),
    Tuple(Vec<u32>),
    Adt {
        n: String,
        v: Vec<Vec<u32>>,
        #[serde(default)]
        vn: Vec<String>,
        #[serde(default)]
        d: Vec<String>,
        #[serde(default)]
        dt: Option<u32>,
        e: bool,
    },
    Ref(bool, u32),
    Opaque {
        n: String,
        d: bool,
    },
    /// A sequence of scalars (design 64, part C): a `Vec` (`k` = "v"), a slice ("s"), or an
    /// array ("a", with its length `n`). `e` is the type of an element.
    Seq {
        e: u32,
        #[serde(default)]
        n: Option<u64>,
        k: String,
    },
}

#[derive(Clone, Debug, Deserialize)]
pub struct IrBlock {
    #[serde(default)]
    pub s: Vec<IrStmt>,
    pub t: IrTerm,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct IrPlace {
    pub l: usize,
    #[serde(default)]
    pub p: Vec<IrProj>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub enum IrProj {
    D,
    F(usize),
    V(usize),
    /// An index with a local (design 64, part C).
    I(usize),
    /// An index with a constant: the offset, and true if it counts from the end.
    K(u64, bool),
    U(String),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub enum IrOp {
    C(IrPlace),
    M(IrPlace),
    K(IrConst),
    /// A flag of the code generation: the same unknown `bool` on the two sides.
    R(String),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub enum IrConst {
    I(String, u32),
    Z(u32),
    Fn(String),
    O(String, u32, bool),
    /// A value of an ADT with scalar fields: the type, the variant, and each field (design
    /// 61.12, C4).
    A(u32, usize, Vec<(String, u32)>),
    /// A reference to an array of scalars (design 64, part C): the type of an element and the
    /// bits of each element.
    S(u32, Vec<String>),
    U(String),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub enum IrRvalue {
    Use(IrOp),
    Bin(String, IrOp, IrOp),
    Chk(String, IrOp, IrOp),
    Un(String, IrOp),
    Cast(IrOp, u32),
    Agg(IrAgg, Vec<IrOp>),
    Ref(bool, IrPlace),
    Disc(IrPlace),
    U(String),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub enum IrAgg {
    T,
    A(u32, usize),
    /// An array of scalars: the type of an element (design 64, part C).
    S(u32),
}

/// A place in the source: `[file, start, end]` (an index into `f`).
pub type Range = [i64; 3];

#[derive(Clone, Debug, Deserialize)]
pub struct IrStmt {
    pub k: IrStmtKind,
    #[serde(default)]
    pub r: Option<Range>,
    #[serde(default)]
    pub c: Option<Range>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub enum IrStmtKind {
    Set(IrPlace, IrRvalue),
    SetDisc(IrPlace, usize),
    U(String),
}

#[derive(Clone, Debug, Deserialize)]
pub struct IrTerm {
    pub k: IrTermKind,
    #[serde(default)]
    pub r: Option<Range>,
    #[serde(default)]
    pub c: Option<Range>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub enum IrUnwind {
    Drops(Vec<IrPlace>),
    Abort,
    U(String),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct IrCallee {
    pub p: String,
    #[serde(default)]
    pub w: Option<String>,
    #[serde(default)]
    pub k: Option<String>,
    #[serde(default)]
    pub m: Option<String>,
    /// An instance of a generic function of the workspace: the inline step takes the form of
    /// the instance (`std`, by the callee text), never the generic form.
    #[serde(default)]
    pub g: bool,
    /// The effect summary of the call (design 64, part B). No summary: the call reads and writes
    /// the world.
    #[serde(default)]
    pub e: Option<IrEff>,
    /// The operation of the sequence model (design 64, part C): `vec_push`, `slice_get`.
    #[serde(default)]
    pub s: Option<String>,
}

/// The effect summary of a call from the driver (design 64, part B).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct IrEff {
    /// The cause of an effect on the world (no cause: no such effect).
    #[serde(default)]
    pub w: Option<String>,
    /// True if the call can stop with no return value (a panic, a loop).
    #[serde(default)]
    pub p: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub enum IrTermKind {
    Goto(usize),
    Sw(IrOp, Vec<(String, usize)>, usize),
    Ret,
    Unreach,
    Assert(IrOp, bool, String, usize, IrUnwind),
    Call(IrCallee, Vec<IrOp>, IrPlace, Option<usize>, IrUnwind),
    Drop(IrPlace, usize, String, IrUnwind),
    U(String),
}

// ---------------------------------------------------------------------------------------------
// The inputs and the result of a check.

/// One version of a body, with the forms that the inline step can use.
pub struct Side<'a> {
    pub ir: Arc<Ir>,
    /// The form of a body of the workspace by its name (the same tree and configuration).
    pub workspace: &'a dyn Fn(&str) -> Option<Arc<Ir>>,
    /// The forms of the small callees of the standard library, by the callee text.
    pub std: &'a BTreeMap<String, Arc<Ir>>,
    /// The map of a callee text of this side to the text of the new side (the rename map of the
    /// step for the old side; the identity for the new side).
    pub rename: &'a dyn Fn(&str) -> String,
}

/// The facts of the step that the check needs.
pub struct Step<'a> {
    /// True if the step changes the body of the workspace with this name (an added or a removed
    /// body counts). Such a callee must be inlined.
    pub changed: &'a dyn Fn(&str) -> bool,
    /// The text of each changed body (to find a changed impl of a trait method).
    pub changed_names: &'a [String],
}

/// The result of a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The method: `SMT`, `SMT after inlining …`.
    Equivalent { method: String },
    /// One exact input and the two outcomes.
    Different { input: String, before: String, after: String },
    NotDecided { cause: String },
}

/// A check with its cost.
#[derive(Clone, Debug)]
pub struct Checked {
    pub verdict: Verdict,
    /// The callees that the check inlined.
    pub inlined: Vec<String>,
    pub queries: usize,
    pub millis: u128,
}

/// The most paths of one side.
pub const MAX_PATHS: usize = 256;
/// The mark of a place in a cause (design 61.10, C2).
pub const PLACE_MARK: &str = "@@";
/// The most steps (statements and terminators) of one side over all paths.
const MAX_STEPS: usize = 40_000;
/// The deepest inline step.
const MAX_DEPTH: usize = 3;

// ---------------------------------------------------------------------------------------------
// Terms and values.

/// An SMT term. `x` is false if the term uses a call result, an opaque value, or the identity of
/// a reference (a model of it can be impossible in Rust).
#[derive(Clone, Debug, PartialEq, Eq)]
struct T {
    s: String,
    x: bool,
}

fn t(s: impl Into<String>) -> T {
    T { s: s.into(), x: true }
}

fn tx(s: impl Into<String>, x: bool) -> T {
    T { s: s.into(), x }
}

fn app(op: &str, args: &[&T]) -> T {
    let mut s = format!("({op}");
    for arg in args {
        s.push(' ');
        s.push_str(&arg.s);
    }
    s.push(')');
    T { s, x: args.iter().all(|arg| arg.x) }
}

fn bv_lit(value: u128, w: u32) -> T {
    let v = if w >= 128 { value } else { value & ((1u128 << w) - 1) };
    t(format!("(_ bv{v} {w})"))
}

/// The value of a literal bitvector term.
fn lit_bv(term: &T) -> Option<u128> {
    let rest = term.s.strip_prefix("(_ bv")?;
    rest.split(' ').next()?.parse().ok()
}

fn lit_bool(term: &T) -> Option<bool> {
    match term.s.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

// Each fold keeps the flag `x` of the terms that decide the result: an inexact literal stays
// inexact (a mutation of this rule gave a false "different", design 61, R61.4-3).
fn not(a: &T) -> T {
    match lit_bool(a) {
        Some(value) => tx((!value).to_string(), a.x),
        None => app("not", &[a]),
    }
}

fn and(items: &[T]) -> T {
    let mut kept: Vec<&T> = Vec::new();
    for item in items {
        match lit_bool(item) {
            Some(true) if item.x => {}
            Some(false) => return item.clone(),
            _ => kept.push(item),
        }
    }
    match kept.len() {
        0 => t("true"),
        1 => kept[0].clone(),
        _ if kept.iter().all(|item| lit_bool(item) == Some(true)) => tx("true", false),
        _ => app("and", &kept),
    }
}

fn or(items: &[T]) -> T {
    let mut kept: Vec<&T> = Vec::new();
    for item in items {
        match lit_bool(item) {
            Some(false) if item.x => {}
            Some(true) => return item.clone(),
            _ => kept.push(item),
        }
    }
    match kept.len() {
        0 => t("false"),
        1 => kept[0].clone(),
        _ if kept.iter().all(|item| lit_bool(item) == Some(false)) => tx("false", false),
        _ => app("or", &kept),
    }
}

fn eq_t(a: &T, b: &T) -> T {
    // Two equal terms are equal for each value of each input: the result is exact also for a
    // term that the check does not model (an opaque value, the result of a call with equal
    // arguments). Design 61.10, C1: the closure state of a closure body is such a value.
    if a.s == b.s {
        return t("true");
    }
    if let (Some(x), Some(y)) = (lit_bv(a), lit_bv(b)) {
        return tx((x == y).to_string(), a.x && b.x);
    }
    if let (Some(x), Some(y)) = (lit_bool(a), lit_bool(b)) {
        return tx((x == y).to_string(), a.x && b.x);
    }
    app("=", &[a, b])
}

fn ite(c: &T, a: &T, b: &T) -> T {
    match lit_bool(c) {
        Some(true) => tx(a.s.clone(), a.x && c.x),
        Some(false) => tx(b.s.clone(), b.x && c.x),
        None if a.s == b.s => tx(a.s.clone(), a.x && b.x && c.x),
        None => app("ite", &[c, a, b]),
    }
}

/// A symbolic value.
#[derive(Clone, Debug, PartialEq)]
enum V {
    /// An integer or a `char`: the term and the width.
    Bv(T, u32),
    Bool(T),
    Unit,
    /// A tuple or a struct.
    Tup(Vec<V>),
    /// An enum: the discriminant (a bitvector of width `w`), the discriminant of each variant,
    /// and the fields of each variant.
    Enum { d: T, w: u32, ds: Vec<u128>, f: Vec<Vec<V>> },
    /// A reference to a place.
    Ptr(Loc),
    /// A value of a type that the check does not model.
    Opq(T),
    /// A sequence of scalars (design 64, part C): a term of the sort `(Seq E)`, and its length.
    Seq(Sq),
    /// No value (not written, or moved).
    Undef,
}

/// A place in the memory of a run: a slot and a path in its value.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Loc {
    slot: usize,
    path: Vec<Step2>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Step2 {
    Field(usize),
    Variant(usize),
    /// An element of a sequence: the index (a term of 64 bits).
    Elem(T),
}

/// The sort of an element of a sequence (design 64, part C).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeqE {
    Bv(u32),
    Bool,
}

impl SeqE {
    fn of(ty: &Ty) -> Option<SeqE> {
        match ty.get()? {
            IrTy::Int(w, _) => Some(SeqE::Bv(*w)),
            IrTy::Char => Some(SeqE::Bv(32)),
            IrTy::Bool => Some(SeqE::Bool),
            _ => None,
        }
    }

    fn elem_sort(self) -> String {
        match self {
            SeqE::Bv(w) => sort_of_bv(w),
            SeqE::Bool => "Bool".into(),
        }
    }

    fn sort(self) -> String {
        format!("(Seq {})", self.elem_sort())
    }

    fn value(self, term: T) -> V {
        match self {
            SeqE::Bv(w) => V::Bv(term, w),
            SeqE::Bool => V::Bool(term),
        }
    }

    fn term(self, value: &V) -> Result<T, Stop> {
        match (self, value) {
            (SeqE::Bv(w), V::Bv(term, v)) if w == *v => Ok(term.clone()),
            (SeqE::Bool, V::Bool(term)) => Ok(term.clone()),
            _ => Err(Stop("an element of a sequence that the check does not model".into())),
        }
    }

    fn empty(self) -> T {
        t(format!("(as seq.empty {})", self.sort()))
    }
}

/// A sequence value: the term of the sort `(Seq E)`, the sort of an element, and the length as a
/// term of 64 bits (`usize`). The length is a separate term: each compare of a length or an
/// index stays in bit vectors (the solver is slow with `int2bv`). Each value keeps
/// `(seq.len s) = (bv2nat n)`: an input and a call result assert it, and each operation keeps it.
#[derive(Clone, Debug, PartialEq)]
struct Sq {
    s: T,
    e: SeqE,
    n: T,
}

/// An index of 64 bits as an integer term.
fn nat(i: &T) -> T {
    app("bv2nat", &[i])
}

impl Sq {
    fn empty(e: SeqE) -> Sq {
        Sq { s: e.empty(), e, n: bv_lit(0, 64) }
    }

    /// True if an index of 64 bits is in the sequence.
    fn has(&self, i: &T) -> T {
        app("bvult", &[i, &self.n])
    }

    fn is_empty(&self) -> T {
        eq_t(&self.n, &bv_lit(0, 64))
    }

    fn at(&self, i: &T) -> V {
        self.e.value(app("seq.nth", &[&self.s, &nat(i)]))
    }

    /// The first `k` elements (`k` at most the length).
    fn prefix(&self, k: &T) -> Sq {
        Sq { s: app("seq.extract", &[&self.s, &t("0"), &nat(k)]), e: self.e, n: k.clone() }
    }

    /// The elements from the index `k` (`k` at most the length).
    fn suffix(&self, k: &T) -> Sq {
        Sq { s: app("seq.extract", &[&self.s, &nat(k), &app("seq.len", &[&self.s])]), e: self.e, n: app("bvsub", &[&self.n, k]) }
    }

    fn then(&self, other: &Sq) -> Sq {
        Sq { s: app("seq.++", &[&self.s, &other.s]), e: self.e, n: app("bvadd", &[&self.n, &other.n]) }
    }

    fn unit(e: SeqE, x: &T) -> Sq {
        Sq { s: app("seq.unit", &[x]), e, n: bv_lit(1, 64) }
    }

    /// The sequence with the element at the index `i` replaced (`i` in the sequence).
    fn update(&self, i: &T, x: &T) -> Sq {
        let after = app("bvadd", &[i, &bv_lit(1, 64)]);
        Sq { n: self.n.clone(), ..self.prefix(i).then(&Sq::unit(self.e, x)).then(&self.suffix(&after)) }
    }

    fn choose(c: &T, a: &Sq, b: &Sq) -> Sq {
        Sq { s: ite(c, &a.s, &b.s), e: a.e, n: ite(c, &a.n, &b.n) }
    }
}

/// The width of the discriminant of an enum type and the discriminant of each variant.
fn discriminants(ir: &Ir, d: &[String], dt: Option<u32>) -> (u32, Vec<u128>) {
    let w = dt.and_then(|dt| match ir.t.get(dt as usize) {
        Some(IrTy::Int(w, _)) => Some(*w),
        _ => None,
    });
    let w = w.unwrap_or(64);
    let m = if w >= 128 { u128::MAX } else { (1u128 << w) - 1 };
    (w, d.iter().enumerate().map(|(index, text)| text.parse::<u128>().map(|value| value & m).unwrap_or(index as u128)).collect())
}

/// A type of a run: the form that has it, and its index.
#[derive(Clone, Debug)]
struct Ty {
    ir: Arc<Ir>,
    id: u32,
}

impl Ty {
    fn get(&self) -> Option<&IrTy> {
        self.ir.t.get(self.id as usize)
    }

    fn at(&self, id: u32) -> Ty {
        Ty { ir: self.ir.clone(), id }
    }
}

// ---------------------------------------------------------------------------------------------
// The shared declarations of one query.

#[derive(Clone, Default)]
struct Decls {
    lines: Vec<String>,
    names: BTreeSet<String>,
    /// The assumptions on the inputs and on the call results (a valid enum discriminant).
    assume: Vec<String>,
    /// The inputs: the name of each parameter and its value (for the text of a difference).
    inputs: Vec<(String, V, Ty)>,
    /// The opaque constants: two different texts are two different values.
    consts: Vec<String>,
    /// The slot and the name (`*x`) of the target of each `&mut` parameter.
    mut_names: Vec<(usize, String)>,
    /// The symbol of each sequence input (design 64, part C): a small model has short
    /// sequences.
    seqs: Vec<(String, SeqE)>,
}

impl Decls {
    fn declare(&mut self, name: &str, line: String) {
        if self.names.insert(name.to_string()) {
            self.lines.push(line);
        }
    }
}

fn sort_of_bv(w: u32) -> String {
    format!("(_ BitVec {w})")
}

/// A short name for a text in a symbol: the letters and digits, with a hash.
fn sym_name(text: &str) -> String {
    let mut hash: u64 = 1469598103934665603;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    let short: String = text.chars().filter(|c| c.is_ascii_alphanumeric()).rev().take(16).collect::<String>().chars().rev().collect();
    format!("{short}_{hash:x}")
}

// ---------------------------------------------------------------------------------------------
// One run of one side.

/// Why a path stops with no outcome that the check can compare.
#[derive(Clone, Debug)]
struct Stop(String);

/// The exit of a path.
#[derive(Clone, Debug)]
enum Exit {
    Return(V),
    Panic { kind: String, args: Vec<V> },
    Abort,
    /// The path reaches the head of the loop (design 61.11): the end of a segment of the loop
    /// check.
    Cut,
}

/// The state of a path at the head of the loop (design 61.11).
#[derive(Clone, Debug)]
struct CutState {
    slots: Vec<V>,
    types: Vec<Option<Ty>>,
}

#[derive(Clone, Debug)]
struct Effect {
    callee: String,
    args: Vec<V>,
}

/// The outcome of one path.
#[derive(Clone, Debug)]
struct PathOut {
    pc: T,
    effects: Vec<Effect>,
    /// The calls with an order (`State::calls`): the callee and a key of its inputs.
    calls: Vec<(String, String)>,
    exit: Exit,
    /// The values behind the `&mut` parameters at the exit, and the values that the cleanup path
    /// of a panic drops.
    after: Vec<V>,
    /// The state at the head of the loop, for the exit `Cut`.
    cut: Option<Arc<CutState>>,
}

#[derive(Clone)]
struct Frame {
    ir: Arc<Ir>,
    base: usize,
    block: usize,
    stmt: usize,
    /// For an inlined callee: the destination in the caller, the target block, and the cleanup
    /// path of the call in the caller.
    ret: Option<(Loc, usize, IrUnwind)>,
    /// The name of the callee (for a recursion test).
    name: String,
    /// A number of the frame, unique in the run.
    id: usize,
}

#[derive(Clone)]
struct State {
    slots: Vec<V>,
    types: Vec<Option<Ty>>,
    frames: Vec<Frame>,
    pc: Vec<T>,
    effects: Vec<Effect>,
    /// Each call with an order (an effect on the world, a write, or a stop with no return value),
    /// and each drop of a value with a destructor, in order: the cause "the order of the calls
    /// changes" (design 64).
    calls: Vec<(String, String)>,
    /// The visits of each block of each frame: a second visit on one path is a loop.
    visits: Vec<(usize, usize)>,
    /// The number of the next frame.
    serial: usize,
}

struct Run<'a, 'b> {
    side: &'a Side<'b>,
    step: &'a Step<'b>,
    inline_all: bool,
    decls: &'a mut Decls,
    steps: usize,
    outs: Vec<PathOut>,
    stops: Vec<(T, String)>,
    inlined: BTreeSet<String>,
    /// The slots of the `&mut` parameters.
    muts: Vec<usize>,
    /// The kind of the side (for the call results: the same name on both sides).
    calls: usize,
    /// True for the old side (the mark of a place in a cause).
    old: bool,
    /// The mode of the run for the loop check (design 61.11).
    mode: LoopMode,
    /// The count of the paths past the bound of an `Unroll` run.
    dropped: usize,
}

/// The mode of a run (design 61.11, part B): a plain run (a second visit of a block is "a
/// loop"); a run that ends each path at the head of the loop of the top frame (`Cut`); a
/// bounded run that unrolls each loop at most N times and drops a longer path (`Unroll`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopMode {
    Plain,
    Cut(usize),
    Unroll(usize),
}

/// The cause of the end of a path at the loop head, and of a path past the bound.
const CUT: &str = "@cut";
const BOUND: &str = "@bound";

impl<'a, 'b> Run<'a, 'b> {
    fn read(&self, state: &State, loc: &Loc) -> Result<V, Stop> {
        let mut value = state.slots.get(loc.slot).cloned().ok_or_else(|| Stop("a place with no slot".into()))?;
        let mut variant: Option<usize> = None;
        for step in &loc.path {
            match (step.clone(), value) {
                (Step2::Variant(v), current) => {
                    variant = Some(v);
                    value = current;
                }
                (Step2::Elem(i), V::Seq(sq)) => {
                    value = sq.at(&i);
                    variant = None;
                }
                (Step2::Elem(_), _) => return Err(Stop("an index".into())),
                (Step2::Field(i), V::Tup(items)) => {
                    value = items.get(i).cloned().ok_or_else(|| Stop("a field that the value does not have".into()))?;
                    variant = None;
                }
                (Step2::Field(i), V::Enum { f, .. }) => {
                    let v = variant.ok_or_else(|| Stop("a field of an enum with no variant".into()))?;
                    value = f.get(v).and_then(|fields| fields.get(i)).cloned().unwrap_or(V::Undef);
                    variant = None;
                }
                (Step2::Field(_), V::Undef) => return Ok(V::Undef),
                (Step2::Field(_), _) => return Err(Stop("a field of a value that the check does not model".into())),
            }
        }
        Ok(value)
    }

    fn write(&self, state: &mut State, loc: &Loc, new: V) -> Result<(), Stop> {
        let ty = state.types.get(loc.slot).cloned().flatten();
        let slot = state.slots.get_mut(loc.slot).ok_or_else(|| Stop("a place with no slot".into()))?;
        write_path(slot, &loc.path, new, ty.as_ref())
    }

    fn loc(&self, state: &State, place: &IrPlace) -> Result<Loc, Stop> {
        let frame = state.frames.last().ok_or_else(|| Stop("no frame".into()))?;
        let mut loc = Loc { slot: frame.base + place.l, path: Vec::new() };
        for projection in &place.p {
            match projection {
                IrProj::D => match self.read(state, &loc)? {
                    V::Ptr(target) => loc = target,
                    _ => return Err(Stop("a deref of a value that is not a reference (a `Box`, a raw pointer)".into())),
                },
                IrProj::F(i) => loc.path.push(Step2::Field(*i)),
                IrProj::V(v) => loc.path.push(Step2::Variant(*v)),
                IrProj::I(local) => {
                    let index = self.read(state, &Loc { slot: frame.base + local, path: Vec::new() })?;
                    let V::Bv(index, 64) = index else { return Err(Stop("an index".into())) };
                    loc.path.push(Step2::Elem(index));
                }
                IrProj::K(offset, from_end) => {
                    let index = if *from_end {
                        let V::Seq(sq) = self.read(state, &loc)? else { return Err(Stop("an index".into())) };
                        app("bvsub", &[&sq.n, &bv_lit(u128::from(*offset), 64)])
                    } else {
                        bv_lit(u128::from(*offset), 64)
                    };
                    loc.path.push(Step2::Elem(index));
                }
                IrProj::U(cause) => return Err(Stop(cause.clone())),
            }
        }
        Ok(loc)
    }

    /// The type of a place of the current frame.
    fn place_ty(&self, state: &State, place: &IrPlace) -> Option<Ty> {
        let frame = state.frames.last()?;
        let mut ty = Ty { ir: frame.ir.clone(), id: *frame.ir.l.get(place.l)? };
        let mut variant: Option<usize> = None;
        for projection in &place.p {
            match projection {
                IrProj::D => match ty.get()? {
                    IrTy::Ref(_, inner) => ty = ty.at(*inner),
                    _ => return None,
                },
                IrProj::V(v) => variant = Some(*v),
                IrProj::I(_) | IrProj::K(..) => match ty.get()? {
                    IrTy::Seq { e, .. } => {
                        ty = ty.at(*e);
                        variant = None;
                    }
                    _ => return None,
                },
                IrProj::F(i) => {
                    let next = match ty.get()? {
                        IrTy::Tuple(items) => *items.get(*i)?,
                        IrTy::Adt { v, .. } => *v.get(variant.unwrap_or(0))?.get(*i)?,
                        _ => return None,
                    };
                    ty = ty.at(next);
                    variant = None;
                }
                IrProj::U(_) => return None,
            }
        }
        Some(ty)
    }

    fn operand(&mut self, state: &mut State, operand: &IrOp) -> Result<V, Stop> {
        match operand {
            IrOp::C(place) => {
                let loc = self.loc(state, place)?;
                self.read(state, &loc)
            }
            IrOp::M(place) => {
                let loc = self.loc(state, place)?;
                // A move keeps the value in the place: MIR never reads a moved place again, and a
                // drop of it is not on the path (drop elaboration).
                self.read(state, &loc)
            }
            IrOp::K(constant) => {
                let ir = state.frames.last().map(|frame| frame.ir.clone()).ok_or_else(|| Stop("no frame".into()))?;
                // A constant reference to a value of a type with no data (`&Food` of a unit
                // struct): a new place with the one value of the type (design 61.10).
                if let IrConst::O(_, ty, _) = constant
                    && let Some(IrTy::Ref(false, inner)) = ir.t.get(*ty as usize)
                    && let Some(value) = zero_sized(&ir, *inner)
                {
                    state.slots.push(value);
                    state.types.push(Some(Ty { ir: ir.clone(), id: *inner }));
                    return Ok(V::Ptr(Loc { slot: state.slots.len() - 1, path: Vec::new() }));
                }
                // A constant reference to an array of scalars: a new place with the sequence.
                if let IrConst::S(item, values) = constant {
                    let e = SeqE::of(&Ty { ir: ir.clone(), id: *item }).ok_or_else(|| Stop("an array".into()))?;
                    let mut sq = Sq::empty(e);
                    for value in values {
                        let element = self.constant(&ir, &IrConst::I(value.clone(), *item))?;
                        sq = sq.then(&Sq::unit(e, &e.term(&element)?));
                    }
                    state.slots.push(V::Seq(sq));
                    state.types.push(None);
                    return Ok(V::Ptr(Loc { slot: state.slots.len() - 1, path: Vec::new() }));
                }
                self.constant(&ir, constant)
            }
            IrOp::R(name) => {
                // The value of the flag is not known here, and it is the same on the two sides.
                let symbol = format!("|flag {}|", sym_name(name));
                self.decls.declare(&symbol, format!("(declare-const {symbol} Bool)"));
                Ok(V::Bool(tx(symbol, false)))
            }
        }
    }

    fn constant(&mut self, ir: &Arc<Ir>, constant: &IrConst) -> Result<V, Stop> {
        match constant {
            IrConst::I(value, ty) => {
                let value: u128 = value.parse().map_err(|_| Stop("a constant with no value".into()))?;
                match ir.t.get(*ty as usize) {
                    Some(IrTy::Bool) => Ok(V::Bool(t(if value == 1 { "true" } else { "false" }))),
                    Some(IrTy::Char) => Ok(V::Bv(bv_lit(value, 32), 32)),
                    Some(IrTy::Int(w, _)) => Ok(V::Bv(bv_lit(value, *w), *w)),
                    _ => Err(Stop("a constant of an unknown type".into())),
                }
            }
            IrConst::Z(ty) => Ok(match ir.t.get(*ty as usize) {
                Some(IrTy::Adt { e: false, v, .. }) if v.first().is_none_or(Vec::is_empty) => V::Tup(Vec::new()),
                Some(IrTy::Opaque { n, .. }) => V::Opq(tx(self.opaque_const(&format!("zst {n}")), true)),
                _ => V::Unit,
            }),
            IrConst::Fn(text) => Ok(V::Opq(tx(self.opaque_const(&format!("fn {}", (self.side.rename)(text))), true))),
            IrConst::O(text, _, exact) => Ok(V::Opq(tx(self.opaque_const(&(self.side.rename)(text)), *exact))),
            IrConst::A(ty, variant, fields) => {
                let mut values = Vec::new();
                for (value, field_ty) in fields {
                    values.push(self.constant(ir, &IrConst::I(value.clone(), *field_ty))?);
                }
                match ir.t.get(*ty as usize) {
                    Some(IrTy::Adt { e: true, v, d, dt, .. }) => {
                        let (w, ds) = discriminants(ir, d, *dt);
                        let value = ds.get(*variant).copied().unwrap_or(*variant as u128);
                        let mut all: Vec<Vec<V>> = v.iter().map(|fields| vec![V::Undef; fields.len()]).collect();
                        if let Some(slot) = all.get_mut(*variant) {
                            *slot = values;
                        }
                        Ok(V::Enum { d: bv_lit(value, w), w, ds, f: all })
                    }
                    Some(IrTy::Adt { e: false, .. }) => Ok(V::Tup(values)),
                    _ => Err(Stop("a constant of a type that the check does not model".into())),
                }
            }
            IrConst::S(..) => Err(Stop("an array".into())),
            IrConst::U(cause) => Err(Stop(cause.clone())),
        }
    }

    /// The symbol of an opaque constant: the same text gives the same symbol on the two sides,
    /// and two different texts are two different values.
    fn opaque_const(&mut self, text: &str) -> String {
        let name = format!("|k {}|", sym_name(text));
        self.decls.declare("Opq", "(declare-sort Opq 0)".into());
        if !self.decls.names.contains(&name) {
            self.decls.consts.push(name.clone());
        }
        self.decls.declare(&name, format!("(declare-const {name} Opq)"));
        name
    }

    fn run(&mut self, state: State) {
        // A depth-first walk; each switch gives a copy of the state for each arm.
        let mut work: Vec<State> = vec![state];
        while let Some(mut state) = work.pop() {
            if self.outs.len() + self.stops.len() > MAX_PATHS {
                self.stops.push((and(&state.pc), format!("more than {MAX_PATHS} paths")));
                return;
            }
            match self.advance(&mut state) {
                Ok(Next::Fork(states)) => work.extend(states.into_iter().rev()),
                Ok(Next::Done) => {}
                Ok(Next::Continue) => work.push(state),
                Err(Stop(cause)) => {
                    let pc = and(&state.pc);
                    if lit_bool(&pc) == Some(false) {
                        continue;
                    }
                    if cause.starts_with(CUT) {
                        let cut = CutState { slots: state.slots.clone(), types: state.types.clone() };
                        self.outs.push(PathOut { pc, effects: state.effects.clone(), calls: state.calls.clone(), exit: Exit::Cut, after: Vec::new(), cut: Some(Arc::new(cut)) });
                    } else if cause.starts_with(BOUND) {
                        self.dropped += 1;
                    } else {
                        self.stops.push((pc, cause));
                    }
                }
            }
        }
    }

    /// Runs the state to its next fork or end.
    fn advance(&mut self, state: &mut State) -> Result<Next, Stop> {
        loop {
            self.steps += 1;
            if self.steps > MAX_STEPS {
                return Err(Stop(format!("more than {MAX_STEPS} steps")));
            }
            let frame = state.frames.last().cloned().ok_or_else(|| Stop("no frame".into()))?;
            let block = frame.ir.b.get(frame.block).ok_or_else(|| Stop("a jump to a block that does not exist".into()))?;
            if frame.stmt < block.s.len() {
                let statement = block.s[frame.stmt].clone();
                if let Err(Stop(cause)) = self.statement(state, &statement.k) {
                    return Err(Stop(self.placed(cause, &frame.ir, statement.r)));
                }
                state.frames.last_mut().unwrap().stmt += 1;
                continue;
            }
            let term = block.t.k.clone();
            let range = block.t.r;
            return self.terminator(state, &term).map_err(|Stop(cause)| Stop(self.placed(cause, &frame.ir, range)));
        }
    }

    /// A cause with the mark of its place in the source (design 61.10, C2): ` @@SIDE|FILE|START|END`
    /// (the side `o` or `n`, the file, and the byte range). The verifier replaces the mark with
    /// the source text and the line. A cause keeps its first mark (the innermost part).
    fn placed(&self, cause: String, ir: &Ir, range: Option<Range>) -> String {
        if cause.contains(PLACE_MARK) {
            return cause;
        }
        let Some([file, start, end]) = range else { return cause };
        let Some(path) = ir.f.get(file.max(0) as usize) else { return cause };
        format!("{cause} {PLACE_MARK}{}|{path}|{start}|{end}", if self.old { "o" } else { "n" })
    }

    fn jump(&self, state: &mut State, target: usize) -> Result<(), Stop> {
        let frame = state.frames.last_mut().unwrap();
        if let LoopMode::Cut(head) = self.mode
            && frame.id == 0
            && target == head
        {
            frame.block = target;
            frame.stmt = 0;
            return Err(Stop(CUT.into()));
        }
        let key = (frame.id, target);
        let seen = state.visits.iter().filter(|known| **known == key).count();
        if seen > 0 {
            match self.mode {
                LoopMode::Unroll(bound) if seen < bound => {}
                LoopMode::Unroll(_) => return Err(Stop(BOUND.into())),
                _ => return Err(Stop("a loop".into())),
            }
        }
        state.visits.push(key);
        frame.block = target;
        frame.stmt = 0;
        Ok(())
    }

    fn statement(&mut self, state: &mut State, kind: &IrStmtKind) -> Result<(), Stop> {
        match kind {
            IrStmtKind::Set(place, value) => {
                let ty = self.place_ty(state, place);
                let found = self.rvalue(state, value, ty.as_ref())?;
                let loc = self.loc(state, place)?;
                self.write(state, &loc, found)
            }
            IrStmtKind::SetDisc(place, variant) => {
                let ty = self.place_ty(state, place).ok_or_else(|| Stop("a place with no type".into()))?;
                let loc = self.loc(state, place)?;
                let current = self.read(state, &loc)?;
                let Some(IrTy::Adt { d, dt, v, .. }) = ty.get() else { return Err(Stop("a discriminant of a value that is not an enum".into())) };
                let (w, ds) = discriminants(&ty.ir, d, *dt);
                let value = ds.get(*variant).copied().unwrap_or(*variant as u128);
                let fields = match current {
                    V::Enum { f, .. } => f,
                    _ => v.iter().map(|fields| vec![V::Undef; fields.len()]).collect(),
                };
                self.write(state, &loc, V::Enum { d: bv_lit(value, w), w, ds, f: fields })
            }
            IrStmtKind::U(cause) => Err(Stop(cause.clone())),
        }
    }

    fn rvalue(&mut self, state: &mut State, value: &IrRvalue, ty: Option<&Ty>) -> Result<V, Stop> {
        match value {
            IrRvalue::Use(operand) => self.operand(state, operand),
            IrRvalue::Bin(op, a, b) => {
                let signed = self.op_signed(state, a);
                let (a, b) = (self.operand(state, a)?, self.operand(state, b)?);
                binary(op, &a, &b, signed, ty)
            }
            IrRvalue::Chk(op, a, b) => {
                let (x, y) = (self.operand(state, a)?, self.operand(state, b)?);
                let signed = match ty.and_then(|ty| ty.get()) {
                    Some(IrTy::Tuple(items)) => matches!(ty.and_then(|ty| ty.ir.t.get(*items.first()? as usize)), Some(IrTy::Int(_, true))),
                    _ => false,
                };
                checked(op, &x, &y, signed)
            }
            // The length of a sequence (design 64, part C): through a reference (the metadata
            // of a pointer) or of a place.
            IrRvalue::Un(op, a) if op == "meta" || op == "len" => {
                let a = self.operand(state, a)?;
                let target = match (op.as_str(), a) {
                    ("meta", V::Ptr(loc)) => self.read(state, &loc)?,
                    ("len", value) => value,
                    _ => V::Undef,
                };
                match target {
                    V::Seq(sq) => Ok(V::Bv(sq.n, 64)),
                    _ if op == "meta" => Err(Stop("the length of a slice (the metadata of a pointer)".into())),
                    _ => Err(Stop("the length of a slice".into())),
                }
            }
            IrRvalue::Un(op, a) => {
                let a = self.operand(state, a)?;
                match (op.as_str(), a) {
                    ("not", V::Bool(x)) => Ok(V::Bool(not(&x))),
                    ("not", V::Bv(x, w)) => Ok(V::Bv(fold1("bvnot", &x, w), w)),
                    ("neg", V::Bv(x, w)) => Ok(V::Bv(fold1("bvneg", &x, w), w)),
                    _ => Err(Stop(format!("the operation `{op}` on a value that the check does not model (a float)"))),
                }
            }
            IrRvalue::Cast(a, to) => {
                let a = self.operand(state, a)?;
                let frame_ir = state.frames.last().unwrap().ir.clone();
                let target = frame_ir.t.get(*to as usize).cloned();
                let from_signed = self.operand_signed(state, value);
                cast(a, target.as_ref(), from_signed)
            }
            IrRvalue::Agg(kind, operands) => {
                let mut values = Vec::new();
                for operand in operands {
                    values.push(self.operand(state, operand)?);
                }
                match kind {
                    IrAgg::T => Ok(V::Tup(values)),
                    IrAgg::S(item) => {
                        let ir = state.frames.last().unwrap().ir.clone();
                        let e = SeqE::of(&Ty { ir, id: *item }).ok_or_else(|| Stop("an array".into()))?;
                        let mut terms = Vec::new();
                        for value in &values {
                            terms.push(app("seq.unit", &[&e.term(value)?]));
                        }
                        let n = bv_lit(terms.len() as u128, 64);
                        Ok(V::Seq(Sq {
                            s: match terms.len() {
                                0 => e.empty(),
                                1 => terms.remove(0),
                                _ => app("seq.++", &terms.iter().collect::<Vec<_>>()),
                            },
                            e,
                            n,
                        }))
                    }
                    IrAgg::A(ty_id, variant) => {
                        let ir = state.frames.last().unwrap().ir.clone();
                        match ir.t.get(*ty_id as usize) {
                            Some(IrTy::Adt { e: true, v, d, dt, .. }) => {
                                let (w, ds) = discriminants(&ir, d, *dt);
                                let value = ds.get(*variant).copied().unwrap_or(*variant as u128);
                                let mut fields: Vec<Vec<V>> = v.iter().map(|fields| vec![V::Undef; fields.len()]).collect();
                                if let Some(slot) = fields.get_mut(*variant) {
                                    *slot = values;
                                }
                                Ok(V::Enum { d: bv_lit(value, w), w, ds, f: fields })
                            }
                            Some(IrTy::Adt { e: false, .. }) => Ok(V::Tup(values)),
                            _ => Err(Stop("an aggregate of a type that the check does not model".into())),
                        }
                    }
                }
            }
            IrRvalue::Ref(_, place) => Ok(V::Ptr(self.loc(state, place)?)),
            IrRvalue::Disc(place) => {
                let loc = self.loc(state, place)?;
                match self.read(state, &loc)? {
                    V::Enum { d, w, .. } => Ok(V::Bv(d, w)),
                    _ => Err(Stop("the discriminant of a value that is not a modeled enum".into())),
                }
            }
            IrRvalue::U(cause) => Err(Stop(cause.clone())),
        }
    }

    /// True if an operand has a signed integer type.
    fn op_signed(&self, state: &State, operand: &IrOp) -> bool {
        match operand {
            IrOp::C(place) | IrOp::M(place) => matches!(self.place_ty(state, place).as_ref().and_then(|ty| ty.get()), Some(IrTy::Int(_, true))),
            IrOp::K(IrConst::I(_, ty)) => matches!(state.frames.last().and_then(|frame| frame.ir.t.get(*ty as usize)), Some(IrTy::Int(_, true))),
            IrOp::K(_) | IrOp::R(_) => false,
        }
    }

    /// The sign of the operand of a cast.
    fn operand_signed(&self, state: &State, value: &IrRvalue) -> bool {
        let IrRvalue::Cast(operand, _) = value else { return false };
        let place = match operand {
            IrOp::C(place) | IrOp::M(place) => place,
            IrOp::K(IrConst::I(_, ty)) => return matches!(state.frames.last().and_then(|frame| frame.ir.t.get(*ty as usize)), Some(IrTy::Int(_, true))),
            _ => return false,
        };
        matches!(self.place_ty(state, place).as_ref().and_then(|ty| ty.get()), Some(IrTy::Int(_, true)))
    }

    fn terminator(&mut self, state: &mut State, kind: &IrTermKind) -> Result<Next, Stop> {
        match kind {
            IrTermKind::Goto(target) => {
                self.jump(state, *target)?;
                Ok(Next::Continue)
            }
            IrTermKind::Sw(operand, arms, otherwise) => {
                let value = self.operand(state, operand)?;
                let mut conditions: Vec<(T, usize)> = Vec::new();
                for (text, target) in arms {
                    let arm: u128 = text.parse().map_err(|_| Stop("a switch arm with no value".into()))?;
                    let condition = match &value {
                        V::Bool(term) => if arm == 1 { term.clone() } else { not(term) },
                        V::Bv(term, w) => eq_t(term, &bv_lit(arm, *w)),
                        _ => return Err(Stop("a switch on a value that the check does not model".into())),
                    };
                    conditions.push((condition, *target));
                }
                let none = and(&conditions.iter().map(|(condition, _)| not(condition)).collect::<Vec<_>>());
                conditions.push((none, *otherwise));
                let mut states = Vec::new();
                for (condition, target) in conditions {
                    if lit_bool(&condition) == Some(false) {
                        continue;
                    }
                    let mut next = state.clone();
                    if lit_bool(&condition) != Some(true) {
                        next.pc.push(condition);
                    }
                    match self.jump(&mut next, target) {
                        Ok(()) => states.push(next),
                        Err(Stop(cause)) => {
                            let pc = and(&next.pc);
                            if lit_bool(&pc) != Some(false) {
                                self.stops.push((pc, cause));
                            }
                        }
                    }
                }
                Ok(Next::Fork(states))
            }
            IrTermKind::Ret => {
                let frame = state.frames.pop().unwrap();
                let value = self.read(state, &Loc { slot: frame.base, path: Vec::new() })?;
                match frame.ret {
                    None => {
                        let after = self.muts.iter().map(|slot| state.slots[*slot].clone()).collect();
                        self.outs.push(PathOut { pc: and(&state.pc), effects: state.effects.clone(), calls: state.calls.clone(), exit: Exit::Return(value), after, cut: None });
                        Ok(Next::Done)
                    }
                    Some((dest, target, _)) => {
                        self.write(state, &dest, value)?;
                        self.jump(state, target)?;
                        Ok(Next::Continue)
                    }
                }
            }
            IrTermKind::Unreach => Ok(Next::Done),
            IrTermKind::Assert(cond, expected, kind, target, unwind) => {
                let value = self.operand(state, cond)?;
                let V::Bool(c) = value else { return Err(Stop("a check of a value that is not a `bool`".into())) };
                let pass = if *expected { c.clone() } else { not(&c) };
                let mut states = Vec::new();
                if lit_bool(&pass) != Some(true) {
                    let mut failed = state.clone();
                    failed.pc.push(not(&pass));
                    self.panic(&mut failed, kind.clone(), Vec::new(), unwind)?;
                }
                if lit_bool(&pass) != Some(false) {
                    let mut next = state.clone();
                    if lit_bool(&pass) != Some(true) {
                        next.pc.push(pass);
                    }
                    self.jump(&mut next, *target)?;
                    states.push(next);
                }
                Ok(Next::Fork(states))
            }
            IrTermKind::Call(callee, args, dest, target, unwind) => self.call(state, callee, args, dest, *target, unwind),
            IrTermKind::Drop(place, target, ty, unwind) => {
                let loc = self.loc(state, place)?;
                let value = self.read(state, &loc)?;
                // A sequence of scalars only frees its memory: the check does not model an
                // allocation (design 64, part A).
                if matches!(value, V::Seq(_)) {
                    self.write(state, &loc, V::Undef)?;
                    self.jump(state, *target)?;
                    return Ok(Next::Continue);
                }
                let drops = self.unwind_values(state, unwind)?;
                let mut args = vec![value];
                args.extend(drops);
                let name = format!("drop {}", (self.side.rename)(ty));
                state.calls.push((name.clone(), flatten(&args[0]).into_iter().map(|(_, term)| term.s).collect::<Vec<_>>().join(";")));
                let index = state.effects.len();
                state.effects.push(Effect { callee: name.clone(), args: args.clone() });
                // A destructor can panic: a second path stops at the drop (R64.2-3), so a write
                // cannot move across it.
                let flat: Vec<T> = args.iter().flat_map(flatten).map(|(_, term)| term).collect();
                let sorts: Vec<String> = args.iter().flat_map(flatten).map(|(sort, _)| sort).collect();
                let condition = self.result_value(&Ty { ir: Arc::new(Ir { t: vec![IrTy::Bool], ..Ir::default() }), id: 0 }, &format!("s_{}_{index}", sym_name(&name)), &flat, &sorts, 0)?;
                let V::Bool(condition) = condition else { return Err(Stop("a condition with no `bool` value".into())) };
                let mut stopped = state.clone();
                stopped.pc.push(condition.clone());
                self.panic(&mut stopped, format!("call `{name}`"), args, unwind)?;
                state.pc.push(tx(not(&condition).s, true));
                self.write(state, &loc, V::Undef)?;
                self.jump(state, *target)?;
                Ok(Next::Continue)
            }
            IrTermKind::U(cause) => Err(Stop(cause.clone())),
        }
    }

    /// The values that the cleanup path of a panic drops, in this frame and in each caller frame.
    /// An abort on the way gives the marker value `abort` at the end.
    fn unwind_values(&self, state: &State, unwind: &IrUnwind) -> Result<Vec<V>, Stop> {
        let mut values = Vec::new();
        let mut current = unwind.clone();
        let mut depth = state.frames.len();
        loop {
            match &current {
                IrUnwind::Drops(places) => {
                    let mut probe = state.clone();
                    probe.frames.truncate(depth);
                    for place in places {
                        let loc = self.loc(&probe, place)?;
                        values.push(self.read(&probe, &loc)?);
                    }
                }
                IrUnwind::Abort => {
                    values.push(V::Opq(t("abort")));
                    return Ok(values);
                }
                IrUnwind::U(cause) => return Err(Stop(cause.clone())),
            }
            // The unwind goes on in the caller of an inlined callee.
            if depth <= 1 {
                return Ok(values);
            }
            let Some((_, _, outer)) = state.frames[depth - 1].ret.clone() else { return Ok(values) };
            current = outer;
            depth -= 1;
        }
    }

    fn panic(&mut self, state: &mut State, kind: String, args: Vec<V>, unwind: &IrUnwind) -> Result<(), Stop> {
        let pc = and(&state.pc);
        if lit_bool(&pc) == Some(false) {
            return Ok(());
        }
        let mut after: Vec<V> = self.muts.iter().map(|slot| state.slots[*slot].clone()).collect();
        let drops = match self.unwind_values(state, unwind) {
            Ok(drops) => drops,
            Err(Stop(cause)) => {
                self.stops.push((pc, cause));
                return Ok(());
            }
        };
        let exit = if drops.last() == Some(&V::Opq(t("abort"))) { Exit::Abort } else { Exit::Panic { kind, args } };
        after.extend(drops);
        self.outs.push(PathOut { pc, effects: state.effects.clone(), calls: state.calls.clone(), exit, after, cut: None });
        Ok(())
    }

    fn call(&mut self, state: &mut State, callee: &IrCallee, args: &[IrOp], dest: &IrPlace, target: Option<usize>, unwind: &IrUnwind) -> Result<Next, Stop> {
        let mut values = Vec::new();
        for arg in args {
            values.push(self.operand(state, arg)?);
        }
        let dest_ty = self.place_ty(state, dest);
        let dest_loc = self.loc(state, dest)?;
        // An intrinsic that the check models.
        if let Some(name) = &callee.k
            && let Some(found) = intrinsic(name, &values, dest_ty.as_ref())?
        {
            self.write(state, &dest_loc, found)?;
            return match target {
                Some(target) => {
                    self.jump(state, target)?;
                    Ok(Next::Continue)
                }
                None => Err(Stop("an intrinsic that never returns".into())),
            };
        }
        // An operation of the sequence model (design 64, part C).
        if let Some(model) = &callee.s {
            return self.seq_call(state, model, &values, &dest_loc, dest_ty.as_ref(), target, unwind);
        }
        // The inline step.
        let depth = state.frames.len();
        // A generic callee has the form of its instance (in `std`, by the callee text): the
        // generic form does not know the type arguments.
        let instance_form = if callee.g { self.side.std.get(&callee.p).cloned() } else { None };
        let workspace_form = if callee.g { instance_form } else { callee.w.as_ref().and_then(|name| (self.side.workspace)(name)) };
        let changed = callee.w.as_ref().is_some_and(|name| (self.step.changed)(name));
        let std_form = if callee.w.is_none() && callee.k.is_none() && callee.m.is_none() { self.side.std.get(&callee.p).cloned() } else { None };
        let form = if changed {
            workspace_form.clone()
        } else if self.inline_all {
            workspace_form.clone().or(std_form)
        } else {
            None
        };
        if let Some(form) = form {
            let name = callee.w.clone().unwrap_or_else(|| callee.p.clone());
            if let Some(cause) = &form.u {
                return Err(Stop(format!("the callee `{name}` has no form ({cause})")));
            }
            if depth >= MAX_DEPTH + 1 {
                if changed {
                    return Err(Stop(format!("the callee `{name}` changes in the step, and the inline step is too deep")));
                }
            } else if state.frames.iter().any(|frame| frame.name == name) {
                return Err(Stop(format!("a recursive callee `{name}`")));
            } else if let Some(target) = target {
                self.inlined.insert(name.clone());
                let base = state.slots.len();
                for (index, ty) in form.l.iter().enumerate() {
                    state.slots.push(V::Undef);
                    state.types.push(Some(Ty { ir: form.clone(), id: *ty }));
                    let _ = index;
                }
                for (index, value) in values.into_iter().enumerate() {
                    if let Some(slot) = state.slots.get_mut(base + 1 + index) {
                        *slot = value;
                    }
                }
                state.serial += 1;
                state.visits.push((state.serial, 0));
                state.frames.push(Frame { ir: form, base, block: 0, stmt: 0, ret: Some((dest_loc, target, unwind.clone())), name, id: state.serial });
                return Ok(Next::Continue);
            }
        }
        if changed {
            let name = callee.w.clone().unwrap_or_default();
            return Err(Stop(match workspace_form {
                None => format!("the callee `{name}` changes in the step and has no form"),
                Some(_) => format!("the callee `{name}` changes in the step and never returns"),
            }));
        }
        // A call of a trait method that the MIR does not resolve: an impl of the method can
        // change in the step.
        if matches!(callee.m.as_deref(), Some("param" | "dyn"))
            && let Some(method) = trait_method(&callee.p)
            && let Some(name) = self.step.changed_names.iter().find(|name| trait_method(name).is_some_and(|other| other == method))
        {
            return Err(Stop(format!("the impl `{name}` of the trait method changes in the step")));
        }
        // A call that the check does not inline: an effect, or a value (design 64, parts A and B).
        if values.iter().any(contains_mut_ref_inside) {
            return Err(Stop("a reference inside an argument of a call".into()));
        }
        let renamed = (self.side.rename)(&callee.p);
        // The inputs of the call: each argument, and the value behind each reference in it
        // (the read set of the call).
        let mut deep: Vec<V> = values.clone();
        for value in &values {
            self.behind(state, value, &mut deep, 0)?;
        }
        let world = match &callee.e {
            Some(summary) => summary.w.clone(),
            None => Some("no effect summary".to_string()),
        };
        let stops = callee.e.as_ref().is_none_or(|summary| summary.p);
        let index = state.effects.len();
        let mut effect_args = deep.clone();
        let key = if world.is_some() {
            // An effect on the world: the k-th effect of the two sides has a result that is a
            // function of the callee, of k, and of the inputs. The two sides must make the same
            // calls of this kind in the same order.
            let drops = self.unwind_values(state, unwind)?;
            effect_args.extend(drops);
            state.effects.push(Effect { callee: renamed.clone(), args: effect_args.clone() });
            format!("{}_{index}", sym_name(&renamed))
        } else {
            // A value: an uninterpreted function of the callee and of its inputs, the same on
            // the two sides and at each place of the call (R64.2-1).
            format!("{}_v", sym_name(&renamed))
        };
        self.calls += 1;
        let flat: Vec<T> = effect_args.iter().flat_map(flatten).map(|(_, term)| term).collect();
        let sorts: Vec<String> = effect_args.iter().flat_map(flatten).map(|(sort, _)| sort).collect();
        // The places that the call writes: the target of each `&mut` argument (R64.3-2).
        let mut writes: Vec<(usize, Ty, Loc)> = Vec::new();
        for (position, arg) in args.iter().enumerate() {
            let Some(arg_ty) = (match arg {
                IrOp::C(place) | IrOp::M(place) => self.place_ty(state, place),
                IrOp::K(_) | IrOp::R(_) => None,
            }) else { continue };
            let Some(IrTy::Ref(true, inner)) = arg_ty.get().cloned() else { continue };
            let Some(V::Ptr(target_loc)) = values.get(position).cloned() else { continue };
            writes.push((position, arg_ty.at(inner), target_loc));
        }
        if world.is_some() || !writes.is_empty() || stops {
            // The key: the direct values of the arguments (a value behind a reference can be
            // the write of an earlier call, so it differs in another order).
            state.calls.push((renamed.clone(), values.iter().flat_map(flatten).map(|(_, term)| term.s).collect::<Vec<_>>().join(";")));
        }
        // A call that can stop (a panic, a loop, an exit): a second path stops at the call. Its
        // condition and its payload are functions of the inputs (and of the history of the
        // effects on the world for such a call). A write of an earlier call is visible at the
        // stop, so a call that can stop has an order with each call that writes (R64.2-3).
        if stops && target.is_some() {
            let condition = self.result_value(&Ty { ir: Arc::new(Ir { t: vec![IrTy::Bool], ..Ir::default() }), id: 0 }, &format!("s_{key}"), &flat, &sorts, 0)?;
            let V::Bool(condition) = condition else { return Err(Stop("a condition with no `bool` value".into())) };
            let mut stopped = state.clone();
            stopped.pc.push(condition.clone());
            self.panic(&mut stopped, format!("call `{renamed}`"), deep.clone(), unwind)?;
            // The path on which the call returns keeps the reading of design 61.4: a model of
            // it is exact if its other parts are exact. The path that stops is never exact (the
            // check does not know if the callee stops for this input).
            state.pc.push(tx(not(&condition).s, true));
        }
        for (position, inner_ty, target_loc) in writes {
            let new = self.result_value(&inner_ty, &format!("w{position}_{key}"), &flat, &sorts, 0)?;
            self.write(state, &target_loc, new)?;
        }
        match target {
            None => {
                let mut probe = state.clone();
                if world.is_some() {
                    probe.effects.pop();
                }
                self.panic(&mut probe, format!("call `{renamed}`"), deep, unwind)?;
                Ok(Next::Done)
            }
            Some(target) => {
                let result = match &dest_ty {
                    Some(ty) => self.result_value(ty, &format!("r_{key}"), &flat, &sorts, 0)?,
                    None => return Err(Stop("a call result with no type".into())),
                };
                self.write(state, &dest_loc, result)?;
                self.jump(state, target)?;
                Ok(Next::Continue)
            }
        }
    }

    /// An operation of the sequence model of `Vec` and slices (design 64, part C), with its
    /// panics. A `Vec` and the slice of its elements are one sequence: `deref` gives the same
    /// place.
    #[allow(clippy::too_many_arguments)]
    fn seq_call(&mut self, state: &mut State, model: &str, args: &[V], dest: &Loc, dest_ty: Option<&Ty>, target: Option<usize>, unwind: &IrUnwind) -> Result<Next, Stop> {
        let Some(target) = target else { return Err(Stop(format!("the sequence operation `{model}` never returns"))) };
        let no_value = || Stop(format!("the sequence operation `{model}` on a value that the check does not model"));
        let behind = |run: &Self, state: &State, at: usize| -> Result<(Loc, Sq), Stop> {
            let Some(V::Ptr(loc)) = args.get(at) else { return Err(no_value()) };
            match run.read(state, loc)? {
                V::Seq(sq) => Ok((loc.clone(), sq)),
                _ => Err(no_value()),
            }
        };
        let number = |at: usize| -> Result<T, Stop> {
            match args.get(at) {
                Some(V::Bv(term, 64)) => Ok(term.clone()),
                _ => Err(no_value()),
            }
        };
        let elem_of_dest = || -> Result<SeqE, Stop> {
            match dest_ty.and_then(|ty| ty.get().cloned().map(|found| (ty, found))) {
                Some((ty, IrTy::Seq { e, .. })) => SeqE::of(&ty.at(e)).ok_or_else(no_value),
                _ => Err(no_value()),
            }
        };
        // An `Option` of the destination type: `Some(value)` if `some` holds, else `None`.
        let option = |some: &T, value: V| -> Result<V, Stop> {
            let Some(ty) = dest_ty else { return Err(no_value()) };
            let Some(IrTy::Adt { e: true, v, vn, d, dt, .. }) = ty.get() else { return Err(no_value()) };
            let (w, ds) = discriminants(&ty.ir, d, *dt);
            let index = |name: &str| vn.iter().position(|found| found == name);
            let (Some(none), Some(yes)) = (index("None"), index("Some")) else { return Err(no_value()) };
            let mut fields: Vec<Vec<V>> = v.iter().map(|fields| vec![V::Undef; fields.len()]).collect();
            fields[yes] = vec![value];
            let d = ite(some, &bv_lit(ds[yes], w), &bv_lit(ds[none], w));
            Ok(V::Enum { d, w, ds, f: fields })
        };
        let with_elem = |loc: &Loc, index: T| -> V {
            let mut found = loc.clone();
            found.path.push(Step2::Elem(index));
            V::Ptr(found)
        };
        let one = bv_lit(1, 64);
        let mut checks: Vec<(T, String, Vec<V>)> = Vec::new();
        let mut write: Option<(Loc, V)> = None;
        let result = match model {
            "vec_new" => V::Seq(Sq::empty(elem_of_dest()?)),
            "vec_with_capacity" => {
                let e = elem_of_dest()?;
                let size = match e {
                    SeqE::Bv(w) => u128::from(w.div_ceil(8)),
                    SeqE::Bool => 1,
                };
                let most = (i64::MAX as u128) / size;
                checks.push((app("bvugt", &[&number(0)?, &bv_lit(most, 64)]), "capacity overflow".into(), Vec::new()));
                V::Seq(Sq::empty(e))
            }
            "vec_push" => {
                let (loc, sq) = behind(self, state, 0)?;
                let x = sq.e.term(args.get(1).ok_or_else(no_value)?)?;
                write = Some((loc, V::Seq(sq.then(&Sq::unit(sq.e, &x)))));
                V::Unit
            }
            "vec_pop" => {
                let (loc, sq) = behind(self, state, 0)?;
                let some = not(&sq.is_empty());
                let last = app("bvsub", &[&sq.n, &one]);
                write = Some((loc, V::Seq(Sq::choose(&some, &sq.prefix(&last), &sq))));
                option(&some, sq.at(&last))?
            }
            "vec_len" | "slice_len" => V::Bv(behind(self, state, 0)?.1.n, 64),
            "vec_is_empty" | "slice_is_empty" => V::Bool(behind(self, state, 0)?.1.is_empty()),
            "vec_clear" => {
                let (loc, sq) = behind(self, state, 0)?;
                write = Some((loc, V::Seq(Sq::empty(sq.e))));
                V::Unit
            }
            "vec_truncate" => {
                let (loc, sq) = behind(self, state, 0)?;
                let k = number(1)?;
                write = Some((loc, V::Seq(Sq::choose(&sq.has(&k), &sq.prefix(&k), &sq))));
                V::Unit
            }
            "vec_insert" => {
                let (loc, sq) = behind(self, state, 0)?;
                let i = number(1)?;
                let x = sq.e.term(args.get(2).ok_or_else(no_value)?)?;
                checks.push((app("bvugt", &[&i, &sq.n]), "insertion index".into(), vec![V::Bv(i.clone(), 64), V::Bv(sq.n.clone(), 64)]));
                write = Some((loc, V::Seq(sq.prefix(&i).then(&Sq::unit(sq.e, &x)).then(&sq.suffix(&i)))));
                V::Unit
            }
            "vec_remove" => {
                let (loc, sq) = behind(self, state, 0)?;
                let i = number(1)?;
                checks.push((not(&sq.has(&i)), "removal index".into(), vec![V::Bv(i.clone(), 64), V::Bv(sq.n.clone(), 64)]));
                let after = app("bvadd", &[&i, &one]);
                write = Some((loc, V::Seq(sq.prefix(&i).then(&sq.suffix(&after)))));
                sq.at(&i)
            }
            "vec_extend_from_slice" => {
                let (loc, sq) = behind(self, state, 0)?;
                let (_, more) = behind(self, state, 1)?;
                if sq.e != more.e {
                    return Err(no_value());
                }
                write = Some((loc, V::Seq(sq.then(&more))));
                V::Unit
            }
            "vec_deref" => {
                behind(self, state, 0)?;
                args[0].clone()
            }
            "vec_index" => {
                let (loc, sq) = behind(self, state, 0)?;
                let i = number(1)?;
                checks.push((not(&sq.has(&i)), "bounds".into(), Vec::new()));
                with_elem(&loc, i)
            }
            "slice_swap" => {
                let (loc, sq) = behind(self, state, 0)?;
                let (a, b) = (number(1)?, number(2)?);
                checks.push((not(&sq.has(&a)), "bounds".into(), Vec::new()));
                checks.push((not(&sq.has(&b)), "bounds".into(), Vec::new()));
                let (x, y) = (sq.e.term(&sq.at(&a))?, sq.e.term(&sq.at(&b))?);
                write = Some((loc, V::Seq(sq.update(&a, &y).update(&b, &x))));
                V::Unit
            }
            "slice_get" => {
                let (loc, sq) = behind(self, state, 0)?;
                let i = number(1)?;
                option(&sq.has(&i), with_elem(&loc, i))?
            }
            "slice_first" => {
                let (loc, sq) = behind(self, state, 0)?;
                option(&not(&sq.is_empty()), with_elem(&loc, bv_lit(0, 64)))?
            }
            "slice_last" => {
                let (loc, sq) = behind(self, state, 0)?;
                let last = app("bvsub", &[&sq.n, &one]);
                option(&not(&sq.is_empty()), with_elem(&loc, last))?
            }
            _ => return Err(Stop(format!("the sequence operation `{model}` that the check does not model"))),
        };
        for (condition, kind, panic_args) in checks {
            let mut failed = state.clone();
            failed.pc.push(condition.clone());
            self.panic(&mut failed, kind, panic_args, unwind)?;
            state.pc.push(not(&condition));
        }
        if let Some((loc, value)) = write {
            self.write(state, &loc, value)?;
        }
        self.write(state, dest, result)?;
        self.jump(state, target)?;
        Ok(Next::Continue)
    }

    /// The values behind each reference in a value, in a fixed order (the read set of a call:
    /// a call can read each place that a reference of its arguments reaches).
    fn behind(&self, state: &State, value: &V, out: &mut Vec<V>, depth: usize) -> Result<(), Stop> {
        match value {
            V::Ptr(loc) => {
                if depth > 4 {
                    return Err(Stop("a chain of references in an argument of a call".into()));
                }
                let target = self.read(state, loc)?;
                out.push(target.clone());
                self.behind(state, &target, out, depth + 1)
            }
            V::Tup(items) => items.iter().try_for_each(|item| self.behind(state, item, out, depth)),
            V::Enum { f, .. } => f.iter().flatten().try_for_each(|item| self.behind(state, item, out, depth)),
            _ => Ok(()),
        }
    }

    /// A value of a type that is a function of the call (its name and its arguments).
    fn result_value(&mut self, ty: &Ty, name: &str, args: &[T], sorts: &[String], depth: usize) -> Result<V, Stop> {
        if depth > 6 {
            return Err(Stop("a call result of a deep type".into()));
        }
        let apply = |decls: &mut Decls, sort: String| -> T {
            let symbol = format!("{name}_{}", sort.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>());
            let signature = sorts.join(" ");
            decls.declare(&format!("{symbol}|{signature}"), format!("(declare-fun |{symbol}|{} ({signature}) {sort})", ""));
            if args.is_empty() { tx(format!("|{symbol}|"), false) } else { tx(format!("(|{symbol}| {})", args.iter().map(|arg| arg.s.as_str()).collect::<Vec<_>>().join(" ")), false) }
        };
        Ok(match ty.get().cloned() {
            Some(IrTy::Bool) => V::Bool(apply(self.decls, "Bool".into())),
            Some(IrTy::Int(w, _)) => V::Bv(apply(self.decls, sort_of_bv(w)), w),
            Some(IrTy::Char) => V::Bv(apply(self.decls, sort_of_bv(32)), 32),
            Some(IrTy::Unit) => V::Unit,
            Some(IrTy::Never) => V::Undef,
            Some(IrTy::Tuple(items)) => {
                let mut values = Vec::new();
                for (index, item) in items.iter().enumerate() {
                    values.push(self.result_value(&ty.at(*item), &format!("{name}f{index}"), args, sorts, depth + 1)?);
                }
                V::Tup(values)
            }
            Some(IrTy::Adt { e: false, v, .. }) => {
                let mut values = Vec::new();
                for (index, item) in v.first().cloned().unwrap_or_default().iter().enumerate() {
                    values.push(self.result_value(&ty.at(*item), &format!("{name}f{index}"), args, sorts, depth + 1)?);
                }
                V::Tup(values)
            }
            Some(IrTy::Adt { e: true, v, d, dt, .. }) => {
                let (w, ds) = discriminants(&ty.ir, &d, dt);
                let discr = apply(self.decls, sort_of_bv(w));
                let valid: Vec<String> = ds.iter().map(|value| format!("(= {} {})", discr.s, bv_lit(*value, w).s)).collect();
                if !valid.is_empty() {
                    self.decls.assume.push(format!("(or {} false)", valid.join(" ")));
                }
                let mut fields = Vec::new();
                for (vi, variant) in v.iter().enumerate() {
                    let mut values = Vec::new();
                    for (index, item) in variant.iter().enumerate() {
                        values.push(self.result_value(&ty.at(*item), &format!("{name}v{vi}f{index}"), args, sorts, depth + 1)?);
                    }
                    fields.push(values);
                }
                V::Enum { d: discr, w, ds, f: fields }
            }
            Some(IrTy::Ref(_, inner)) => {
                // A reference that a call returns: a new place whose value is a function of the
                // call.
                let value = self.result_value(&ty.at(inner), &format!("{name}t"), args, sorts, depth + 1)?;
                // The true part (design 61.10, C2): a result of a call, or a value that a call
                // writes through a `&mut` argument.
                let what = if name.starts_with('w') { "a call that can write a reference through its `&mut` argument" } else { "a call that returns a reference" };
                return Err(Stop(format!("{what} ({})", match value { V::Opq(_) => "to an opaque value", _ => "to a value" })));
            }
            Some(IrTy::Opaque { .. }) => {
                self.decls.declare("Opq", "(declare-sort Opq 0)".into());
                V::Opq(apply(self.decls, "Opq".into()))
            }
            Some(IrTy::Seq { e, n: fixed, .. }) => {
                let e = SeqE::of(&ty.at(e)).ok_or_else(|| Stop("a call result of a sequence that the check does not model".into()))?;
                let seq = apply(self.decls, e.sort());
                let n = match fixed {
                    Some(fixed) => bv_lit(u128::from(fixed), 64),
                    None => {
                        let name = format!("{name}n");
                        let symbol = format!("{name}_{}", "BitVec64");
                        let signature = sorts.join(" ");
                        self.decls.declare(&format!("{symbol}|{signature}"), format!("(declare-fun |{symbol}| ({signature}) (_ BitVec 64))"));
                        if args.is_empty() { tx(format!("|{symbol}|"), false) } else { tx(format!("(|{symbol}| {})", args.iter().map(|arg| arg.s.as_str()).collect::<Vec<_>>().join(" ")), false) }
                    }
                };
                self.decls.assume.push(format!("(and (= (seq.len {}) (bv2nat {})) (bvule {} {}))", seq.s, n.s, n.s, bv_lit(i64::MAX as u128, 64).s));
                V::Seq(Sq { s: seq, e, n })
            }
            None => return Err(Stop("a call result with no type".into())),
        })
    }
}

enum Next {
    Continue,
    Fork(Vec<State>),
    Done,
}

/// The trait and the method of a callee text `<X as Trait>::method`: ` as Trait>::method`.
fn trait_method(text: &str) -> Option<&str> {
    let start = text.find(" as ")?;
    let rest = &text[start..];
    rest.contains(">::").then_some(rest)
}

fn contains_mut_ref_inside(value: &V) -> bool {
    match value {
        V::Tup(items) => items.iter().any(|item| matches!(item, V::Ptr(_)) || contains_mut_ref_inside(item)),
        V::Enum { f, .. } => f.iter().flatten().any(|item| matches!(item, V::Ptr(_)) || contains_mut_ref_inside(item)),
        _ => false,
    }
}

fn write_path(slot: &mut V, path: &[Step2], new: V, ty: Option<&Ty>) -> Result<(), Stop> {
    let Some((first, rest)) = path.split_first() else {
        *slot = new;
        return Ok(());
    };
    match first {
        Step2::Elem(i) => {
            let V::Seq(sq) = slot else { return Err(Stop("an index".into())) };
            if !rest.is_empty() {
                return Err(Stop("a field of an element of a sequence".into()));
            }
            let x = sq.e.term(&new)?;
            *slot = V::Seq(sq.update(i, &x));
            Ok(())
        }
        Step2::Variant(v) => {
            if *slot == V::Undef {
                *slot = shape(ty).ok_or_else(|| Stop("a write into a value with no shape".into()))?;
            }
            let Some((Step2::Field(i), rest2)) = rest.split_first() else {
                return Err(Stop("a variant with no field".into()));
            };
            match slot {
                V::Enum { f, .. } => {
                    let fields = f.get_mut(*v).ok_or_else(|| Stop("a variant that the value does not have".into()))?;
                    let field_ty = field_ty(ty, Some(*v), *i);
                    let target = fields.get_mut(*i).ok_or_else(|| Stop("a field that the value does not have".into()))?;
                    write_path(target, rest2, new, field_ty.as_ref())
                }
                V::Tup(items) => {
                    let field_ty = field_ty(ty, Some(*v), *i);
                    let target = items.get_mut(*i).ok_or_else(|| Stop("a field that the value does not have".into()))?;
                    write_path(target, rest2, new, field_ty.as_ref())
                }
                _ => Err(Stop("a variant of a value that is not an enum".into())),
            }
        }
        Step2::Field(i) => {
            if *slot == V::Undef {
                *slot = shape(ty).ok_or_else(|| Stop("a write into a value with no shape".into()))?;
            }
            let field_ty = field_ty(ty, None, *i);
            match slot {
                V::Tup(items) => {
                    let target = items.get_mut(*i).ok_or_else(|| Stop("a field that the value does not have".into()))?;
                    write_path(target, rest, new, field_ty.as_ref())
                }
                _ => Err(Stop("a field of a value that the check does not model".into())),
            }
        }
    }
}

fn field_ty(ty: Option<&Ty>, variant: Option<usize>, index: usize) -> Option<Ty> {
    let ty = ty?;
    let id = match ty.get()? {
        IrTy::Tuple(items) => *items.get(index)?,
        IrTy::Adt { v, .. } => *v.get(variant.unwrap_or(0))?.get(index)?,
        _ => return None,
    };
    Some(ty.at(id))
}

/// An empty value of a type: a tuple or an enum with no value in its fields.
fn shape(ty: Option<&Ty>) -> Option<V> {
    let ty = ty?;
    match ty.get()? {
        IrTy::Tuple(items) => Some(V::Tup(vec![V::Undef; items.len()])),
        IrTy::Adt { e: false, v, .. } => Some(V::Tup(vec![V::Undef; v.first().map_or(0, Vec::len)])),
        IrTy::Adt { e: true, v, d, dt, .. } => {
            let (w, ds) = discriminants(&ty.ir, d, *dt);
            Some(V::Enum { d: bv_lit(ds.first().copied().unwrap_or(0), w), w, ds, f: v.iter().map(|fields| vec![V::Undef; fields.len()]).collect() })
        }
        _ => None,
    }
}

/// The SMT terms of a value, with their sorts, in a fixed order.
fn flatten(value: &V) -> Vec<(String, T)> {
    match value {
        V::Bv(term, w) => vec![(sort_of_bv(*w), term.clone())],
        V::Bool(term) => vec![("Bool".into(), term.clone())],
        V::Unit | V::Undef => Vec::new(),
        V::Tup(items) => items.iter().flat_map(flatten).collect(),
        V::Enum { d, w, f, .. } => {
            let mut out = vec![(sort_of_bv(*w), d.clone())];
            out.extend(f.iter().flatten().flat_map(flatten));
            out
        }
        V::Ptr(_) => Vec::new(),
        V::Opq(term) => vec![("Opq".into(), term.clone())],
        V::Seq(sq) => vec![(sq.e.sort(), sq.s.clone())],
    }
}

fn fold1(op: &str, a: &T, w: u32) -> T {
    if let Some(x) = lit_bv(a) {
        let m = if w >= 128 { u128::MAX } else { (1u128 << w) - 1 };
        return match op {
            "bvnot" => bv_lit(!x & m, w),
            "bvneg" => bv_lit(x.wrapping_neg() & m, w),
            _ => app(op, &[a]),
        };
    }
    app(op, &[a])
}

/// The value of a literal of width `w` as a signed number.
fn signed_lit(value: u128, w: u32) -> i128 {
    if w >= 128 { value as i128 } else { ((value << (128 - w)) as i128) >> (128 - w) }
}

/// A binary operation with a constant fold of two literals.
fn bin_bv(op: &str, a: &T, b: &T, w: u32, signed: bool) -> T {
    if let (Some(x), Some(y)) = (lit_bv(a), lit_bv(b)) {
        let m = if w >= 128 { u128::MAX } else { (1u128 << w) - 1 };
        let (sx, sy) = (signed_lit(x, w), signed_lit(y, w));
        let folded = match op {
            "bvadd" => Some(x.wrapping_add(y) & m),
            "bvsub" => Some(x.wrapping_sub(y) & m),
            "bvmul" => Some(x.wrapping_mul(y) & m),
            "bvand" => Some(x & y),
            "bvor" => Some(x | y),
            "bvxor" => Some(x ^ y),
            "bvudiv" if y != 0 => Some(x / y),
            "bvurem" if y != 0 => Some(x % y),
            "bvsdiv" if sy != 0 => Some((sx.wrapping_div(sy) as u128) & m),
            "bvsrem" if sy != 0 => Some((sx.wrapping_rem(sy) as u128) & m),
            _ => None,
        };
        if let Some(value) = folded {
            return bv_lit(value, w);
        }
        let flag = match op {
            "bvult" => Some(x < y),
            "bvule" => Some(x <= y),
            "bvugt" => Some(x > y),
            "bvuge" => Some(x >= y),
            "bvslt" => Some(sx < sy),
            "bvsle" => Some(sx <= sy),
            "bvsgt" => Some(sx > sy),
            "bvsge" => Some(sx >= sy),
            _ => None,
        };
        if let Some(flag) = flag {
            return t(flag.to_string());
        }
    }
    let _ = signed;
    app(op, &[a, b])
}

/// The shift amount `b` (of width `wb`) at the width `w`, masked to the width (the MIR rule).
fn shift_amount(b: &T, wb: u32, w: u32) -> T {
    let resized = if wb == w {
        b.clone()
    } else if wb > w {
        tx(format!("((_ extract {} 0) {})", w - 1, b.s), b.x)
    } else {
        tx(format!("((_ zero_extend {}) {})", w - wb, b.s), b.x)
    };
    // The mask: the width is a power of two.
    bin_bv("bvand", &resized, &bv_lit(u128::from(w) - 1, w), w, false)
}

fn binary(op: &str, a: &V, b: &V, signed_result: bool, _ty: Option<&Ty>) -> Result<V, Stop> {
    let op = op.trim_end_matches(" unchecked");
    match (a, b) {
        (V::Bv(x, w), V::Bv(y, wb)) => {
            let w = *w;
            // The sign of a comparison comes from the operands; the result type is `bool`.
            let signed = signed_result;
            let value = |name: &str| V::Bv(bin_bv(name, x, y, w, signed), w);
            Ok(match op {
                "add" => value("bvadd"),
                "sub" => value("bvsub"),
                "mul" => value("bvmul"),
                "div" => value(if signed { "bvsdiv" } else { "bvudiv" }),
                "rem" => value(if signed { "bvsrem" } else { "bvurem" }),
                "and" => value("bvand"),
                "or" => value("bvor"),
                "xor" => value("bvxor"),
                "shl" => V::Bv(bin_bv("bvshl", x, &shift_amount(y, *wb, w), w, false), w),
                "shr" => V::Bv(bin_bv(if signed { "bvashr" } else { "bvlshr" }, x, &shift_amount(y, *wb, w), w, false), w),
                "eq" => V::Bool(eq_t(x, y)),
                "ne" => V::Bool(not(&eq_t(x, y))),
                "lt" | "le" | "gt" | "ge" => compare(op, x, y, w, signed)?,
                "cmp" => {
                    let less = compare("lt", x, y, w, signed)?;
                    let V::Bool(less) = less else { unreachable!() };
                    let ordering = ite(&less, &bv_lit(0xff, 8), &ite(&eq_t(x, y), &bv_lit(0, 8), &bv_lit(1, 8)));
                    V::Enum { d: ordering, w: 8, ds: vec![0xff, 0, 1], f: vec![Vec::new(), Vec::new(), Vec::new()] }
                }
                _ => return Err(Stop(format!("the operation `{op}` on integers"))),
            })
        }
        (V::Bool(x), V::Bool(y)) => Ok(V::Bool(match op {
            "and" => and(&[x.clone(), y.clone()]),
            "or" => or(&[x.clone(), y.clone()]),
            "xor" | "ne" => not(&eq_t(x, y)),
            "eq" => eq_t(x, y),
            "lt" => and(&[not(x), y.clone()]),
            "le" => or(&[not(x), y.clone()]),
            "gt" => and(&[x.clone(), not(y)]),
            "ge" => or(&[x.clone(), not(y)]),
            _ => return Err(Stop(format!("the operation `{op}` on `bool`"))),
        })),
        (V::Opq(_), _) | (_, V::Opq(_)) => Err(Stop(format!("the operation `{op}` on a value that the check does not model (a float, a pointer)"))),
        _ => Err(Stop(format!("the operation `{op}` on values that the check does not model"))),
    }
}

/// A comparison of two integers with a known sign.
fn compare(op: &str, x: &T, y: &T, w: u32, signed: bool) -> Result<V, Stop> {
    let name = match (op, signed) {
        ("lt", false) => "bvult",
        ("le", false) => "bvule",
        ("gt", false) => "bvugt",
        ("ge", false) => "bvuge",
        ("lt", true) => "bvslt",
        ("le", true) => "bvsle",
        ("gt", true) => "bvsgt",
        ("ge", true) => "bvsge",
        _ => return Err(Stop(format!("the operation `{op}`"))),
    };
    Ok(V::Bool(bin_bv(name, x, y, w, signed)))
}

/// A checked operation: the wrapped value and the overflow flag.
fn checked(op: &str, a: &V, b: &V, signed: bool) -> Result<V, Stop> {
    let (V::Bv(x, w), V::Bv(y, wb)) = (a, b) else { return Err(Stop(format!("the checked operation `{op}` on values that are not integers"))) };
    let w = *w;
    let sign = |term: &T| tx(format!("((_ extract {} {}) {})", w - 1, w - 1, term.s), term.x);
    let (value, overflow) = match op {
        "add" => {
            let r = bin_bv("bvadd", x, y, w, signed);
            let o = if let (Some(_), Some(_)) = (lit_bv(x), lit_bv(y)) {
                let (sx, sy) = (lit_bv(x).unwrap(), lit_bv(y).unwrap());
                t(literal_overflow("add", sx, sy, w, signed).to_string())
            } else if signed {
                and(&[eq_t(&sign(x), &sign(y)), not(&eq_t(&sign(x), &sign(&r)))])
            } else {
                bin_bv("bvult", &r, x, w, false)
            };
            (r, o)
        }
        "sub" => {
            let r = bin_bv("bvsub", x, y, w, signed);
            let o = if let (Some(sx), Some(sy)) = (lit_bv(x), lit_bv(y)) {
                t(literal_overflow("sub", sx, sy, w, signed).to_string())
            } else if signed {
                and(&[not(&eq_t(&sign(x), &sign(y))), not(&eq_t(&sign(x), &sign(&r)))])
            } else {
                bin_bv("bvult", x, y, w, false)
            };
            (r, o)
        }
        "mul" => {
            let r = bin_bv("bvmul", x, y, w, signed);
            let o = if let (Some(sx), Some(sy)) = (lit_bv(x), lit_bv(y)) {
                t(literal_overflow("mul", sx, sy, w, signed).to_string())
            } else if signed {
                // Z3 has the overflow tests of a product as functions.
                or(&[not(&app("bvsmul_noovfl", &[x, y])), not(&app("bvsmul_noudfl", &[x, y]))])
            } else {
                not(&app("bvumul_noovfl", &[x, y]))
            };
            (r, o)
        }
        "shl" | "shr" => {
            let amount = if *wb == w { y.clone() } else if *wb > w { y.clone() } else { tx(format!("((_ zero_extend {}) {})", w - wb, y.s), y.x) };
            let width = if *wb > w { *wb } else { w };
            let o = bin_bv("bvuge", &amount, &bv_lit(u128::from(w), width), width, false);
            let V::Bv(r, _) = binary(op, a, b, signed, None)? else { unreachable!() };
            (r, o)
        }
        _ => return Err(Stop(format!("the checked operation `{op}`"))),
    };
    Ok(V::Tup(vec![V::Bv(value, w), V::Bool(overflow)]))
}

fn literal_overflow(op: &str, x: u128, y: u128, w: u32, signed: bool) -> bool {
    let (low, high): (i128, i128) = if signed {
        if w >= 128 { (i128::MIN, i128::MAX) } else { (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1) }
    } else if w >= 127 {
        (0, i128::MAX)
    } else {
        (0, (1i128 << w) - 1)
    };
    let (a, b) = if signed { (signed_lit(x, w), signed_lit(y, w)) } else { (x as i128, y as i128) };
    let result = match op {
        "add" => a.checked_add(b),
        "sub" => a.checked_sub(b),
        "mul" => a.checked_mul(b),
        _ => None,
    };
    match result {
        Some(value) => value < low || value > high,
        None => true,
    }
}

fn cast(value: V, target: Option<&IrTy>, from_signed: bool) -> Result<V, Stop> {
    let (to_w, _) = match target {
        Some(IrTy::Int(w, s)) => (*w, *s),
        Some(IrTy::Char) => (32, false),
        Some(IrTy::Bool) => (1, false),
        _ => return Err(Stop("a cast to a type that the check does not model".into())),
    };
    match value {
        V::Bool(term) => Ok(V::Bv(ite(&term, &bv_lit(1, to_w), &bv_lit(0, to_w)), to_w)),
        V::Bv(term, w) => {
            if let Some(x) = lit_bv(&term) {
                let wide = if from_signed { signed_lit(x, w) as u128 } else { x };
                return Ok(V::Bv(bv_lit(wide, to_w), to_w));
            }
            let found = if to_w == w {
                term
            } else if to_w < w {
                tx(format!("((_ extract {} 0) {})", to_w - 1, term.s), term.x)
            } else if from_signed {
                tx(format!("((_ sign_extend {}) {})", to_w - w, term.s), term.x)
            } else {
                tx(format!("((_ zero_extend {}) {})", to_w - w, term.s), term.x)
            };
            Ok(V::Bv(found, to_w))
        }
        _ => Err(Stop("a cast of a value that the check does not model".into())),
    }
}

/// An intrinsic that the check models. `Ok(None)`: the check does not model it, and the call is
/// an effect.
fn intrinsic(name: &str, args: &[V], dest: Option<&Ty>) -> Result<Option<V>, Stop> {
    let signed = match dest.and_then(|ty| ty.get()) {
        Some(IrTy::Int(_, s)) => *s,
        Some(IrTy::Tuple(items)) => matches!(dest.and_then(|ty| ty.ir.t.get(*items.first()? as usize)), Some(IrTy::Int(_, true))),
        _ => false,
    };
    Ok(Some(match (name, args) {
        ("black_box", [value]) => value.clone(),
        ("wrapping_add", [a, b]) => binary("add", a, b, signed, None)?,
        ("wrapping_sub", [a, b]) => binary("sub", a, b, signed, None)?,
        ("wrapping_mul", [a, b]) => binary("mul", a, b, signed, None)?,
        ("add_with_overflow", [a, b]) => checked("add", a, b, signed)?,
        ("sub_with_overflow", [a, b]) => checked("sub", a, b, signed)?,
        ("mul_with_overflow", [a, b]) => checked("mul", a, b, signed)?,
        // The small helpers of the standard library (design 61.12, C4): each intrinsic with its
        // exact meaning.
        ("unchecked_add", [a, b]) => binary("add", a, b, signed, None)?,
        ("unchecked_sub", [a, b]) => binary("sub", a, b, signed, None)?,
        ("unchecked_mul", [a, b]) => binary("mul", a, b, signed, None)?,
        ("saturating_add", [a, b]) => saturating("add", a, b, signed)?,
        ("saturating_sub", [a, b]) => saturating("sub", a, b, signed)?,
        ("integer_min" | "integer_max", [a, b]) => {
            let (V::Bv(x, w), V::Bv(y, _)) = (a, b) else { return Err(Stop(format!("the intrinsic `{name}` on values that are not integers"))) };
            let V::Bool(less) = compare("lt", x, y, *w, signed)? else { return Err(Stop(format!("the intrinsic `{name}`"))) };
            let (first, second) = if name == "integer_min" { (x, y) } else { (y, x) };
            V::Bv(ite(&less, first, second), *w)
        }
        ("likely" | "unlikely", [value]) => value.clone(),
        ("cold_path", []) => V::Unit,
        // A rotate (design 64): the amount is taken modulo the width, as the intrinsic does.
        ("rotate_left" | "rotate_right", [V::Bv(x, w), V::Bv(n, v)]) if w.is_power_of_two() => {
            let amount = if v > w { app(&format!("(_ extract {} 0)", w - 1), &[n]) } else if v < w { app(&format!("(_ zero_extend {})", w - v), &[n]) } else { n.clone() };
            let r = app("bvand", &[&amount, &bv_lit(u128::from(*w) - 1, *w)]);
            let back = app("bvsub", &[&bv_lit(u128::from(*w), *w), &r]);
            let (first, second) = if name == "rotate_left" { ("bvshl", "bvlshr") } else { ("bvlshr", "bvshl") };
            V::Bv(app("bvor", &[&app(first, &[x, &r]), &app(second, &[x, &back])]), *w)
        }
        _ => return Ok(None),
    }))
}

/// A saturating operation: the checked operation, and the bound of the type on an overflow
/// (an unsigned `add` gives the maximum, an unsigned `sub` gives 0; a signed operation gives the
/// bound on the side of the overflow, from the sign of the second operand).
fn saturating(op: &str, a: &V, b: &V, signed: bool) -> Result<V, Stop> {
    let V::Tup(parts) = checked(op, a, b, signed)? else { return Err(Stop(format!("the saturating operation `{op}`"))) };
    let (Some(V::Bv(value, w)), Some(V::Bool(overflow)), V::Bv(y, _)) = (parts.first(), parts.get(1), b) else { return Err(Stop(format!("the saturating operation `{op}` on values that are not integers"))) };
    let w = *w;
    let max = |signed: bool| if signed { bv_lit((1u128 << (w - 1)) - 1, w) } else { bv_lit(u128::MAX, w) };
    let min = |signed: bool| if signed { bv_lit(1u128 << (w - 1), w) } else { bv_lit(0, w) };
    let bound = if signed {
        let negative = eq_t(&tx(format!("((_ extract {} {}) {})", w - 1, w - 1, y.s), y.x), &t("#b1"));
        match op {
            "add" => ite(&negative, &min(true), &max(true)),
            _ => ite(&negative, &max(true), &min(true)),
        }
    } else if op == "add" {
        max(false)
    } else {
        min(false)
    };
    Ok(V::Bv(ite(overflow, &bound, value), w))
}

// ---------------------------------------------------------------------------------------------
// The input values.

fn input(decls: &mut Decls, ty: &Ty, name: &str, slots: &mut Vec<V>, types: &mut Vec<Option<Ty>>, muts: &mut Vec<usize>, depth: usize) -> Result<V, Stop> {
    if depth > 6 {
        return Err(Stop("an input of a deep type".into()));
    }
    let symbol = format!("|in {name}|");
    Ok(match ty.get().cloned() {
        Some(IrTy::Bool) => {
            decls.declare(&symbol, format!("(declare-const {symbol} Bool)"));
            V::Bool(t(symbol))
        }
        Some(IrTy::Int(w, _)) => {
            decls.declare(&symbol, format!("(declare-const {symbol} {})", sort_of_bv(w)));
            V::Bv(t(symbol), w)
        }
        Some(IrTy::Char) => {
            decls.declare(&symbol, format!("(declare-const {symbol} {})", sort_of_bv(32)));
            decls.assume.push(format!("(or (bvult {symbol} #x0000d800) (and (bvuge {symbol} #x0000e000) (bvule {symbol} #x0010ffff)))"));
            V::Bv(t(symbol), 32)
        }
        Some(IrTy::Unit) => V::Unit,
        Some(IrTy::Never) => return Err(Stop("an input of the type `!`".into())),
        Some(IrTy::Tuple(items)) => {
            let mut values = Vec::new();
            for (index, item) in items.iter().enumerate() {
                values.push(input(decls, &ty.at(*item), &format!("{name}.{index}"), slots, types, muts, depth + 1)?);
            }
            V::Tup(values)
        }
        Some(IrTy::Adt { e: false, v, .. }) => {
            let mut values = Vec::new();
            for (index, item) in v.first().cloned().unwrap_or_default().iter().enumerate() {
                values.push(input(decls, &ty.at(*item), &format!("{name}.{index}"), slots, types, muts, depth + 1)?);
            }
            V::Tup(values)
        }
        Some(IrTy::Adt { e: true, v, d, dt, .. }) => {
            let (w, ds) = discriminants(&ty.ir, &d, dt);
            let discr = format!("|in {name}.d|");
            decls.declare(&discr, format!("(declare-const {discr} {})", sort_of_bv(w)));
            let valid: Vec<String> = ds.iter().map(|value| format!("(= {discr} {})", bv_lit(*value, w).s)).collect();
            decls.assume.push(format!("(or {} false)", valid.join(" ")));
            let mut fields = Vec::new();
            for (vi, variant) in v.iter().enumerate() {
                let mut values = Vec::new();
                for (index, item) in variant.iter().enumerate() {
                    values.push(input(decls, &ty.at(*item), &format!("{name}.{vi}.{index}"), slots, types, muts, depth + 1)?);
                }
                fields.push(values);
            }
            V::Enum { d: t(discr), w, ds, f: fields }
        }
        Some(IrTy::Ref(mutable, inner)) => {
            let value = input(decls, &ty.at(inner), &format!("*{name}"), slots, types, muts, depth + 1)?;
            slots.push(value);
            types.push(Some(ty.at(inner)));
            if mutable {
                muts.push(slots.len() - 1);
                decls.mut_names.push((slots.len() - 1, format!("*{name}")));
            }
            V::Ptr(Loc { slot: slots.len() - 1, path: Vec::new() })
        }
        Some(IrTy::Opaque { .. }) => {
            decls.declare("Opq", "(declare-sort Opq 0)".into());
            decls.declare(&symbol, format!("(declare-const {symbol} Opq)"));
            V::Opq(tx(symbol, false))
        }
        // A sequence (design 64, part C): a `Vec` or a slice has at most `isize::MAX` elements;
        // an array has its length.
        Some(IrTy::Seq { e, n, .. }) => {
            let e = SeqE::of(&ty.at(e)).ok_or_else(|| Stop("an input of a sequence that the check does not model".into()))?;
            decls.declare(&symbol, format!("(declare-const {symbol} {})", e.sort()));
            let length = match n {
                Some(n) => bv_lit(u128::from(n), 64),
                None => {
                    let length = format!("|in {name}.len|");
                    decls.declare(&length, format!("(declare-const {length} (_ BitVec 64))"));
                    decls.assume.push(format!("(bvule {length} {})", bv_lit(i64::MAX as u128, 64).s));
                    t(length)
                }
            };
            decls.assume.push(format!("(= (seq.len {symbol}) (bv2nat {}))", length.s));
            decls.seqs.push((symbol.clone(), e));
            V::Seq(Sq { s: t(symbol), e, n: length })
        }
        None => return Err(Stop("an input with no type".into())),
    })
}

/// True if two types are the same type: the same structure, and the same names after the rename
/// map of the step (`rename` maps an old text to its new text).
fn same_type(a: &Ty, b: &Ty, depth: usize, rename: &dyn Fn(&str) -> String) -> bool {
    if depth > 8 {
        return true;
    }
    match (a.get(), b.get()) {
        (Some(IrTy::Tuple(x)), Some(IrTy::Tuple(y))) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_type(&a.at(*p), &b.at(*q), depth + 1, rename)),
        (Some(IrTy::Adt { n: nx, v: x, d: dx, e: ex, .. }), Some(IrTy::Adt { n: ny, v: y, d: dy, e: ey, .. })) => {
            rename(nx) == *ny && ex == ey && dx == dy && x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.len() == q.len() && p.iter().zip(q).all(|(r, s)| same_type(&a.at(*r), &b.at(*s), depth + 1, rename)))
        }
        (Some(IrTy::Ref(m, x)), Some(IrTy::Ref(n, y))) => m == n && same_type(&a.at(*x), &b.at(*y), depth + 1, rename),
        (Some(IrTy::Seq { e: x, n: m, k: p }), Some(IrTy::Seq { e: y, n, k: q })) => m == n && p == q && same_type(&a.at(*x), &b.at(*y), depth + 1, rename),
        (Some(IrTy::Opaque { n: x, .. }), Some(IrTy::Opaque { n: y, .. })) => without_place(&rename(x)) == without_place(y),
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// A type text with no source place in a closure or coroutine type (`{closure@src/lib.rs:2:18:
/// 2:21}` gives `{closure}`). Design 61.12, C1: a closure that moves to a different line keeps
/// its type for the check (the captures and the parameters are the inputs).
fn without_place(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let head = &rest[..at];
        if head.ends_with("{closure") || head.ends_with("{coroutine") || head.ends_with("block") || head.ends_with("{coroutine-closure") {
            out.push_str(head);
            match rest[at..].find('}') {
                Some(end) => rest = &rest[at + end..],
                None => {
                    rest = "";
                }
            }
        } else {
            out.push_str(&rest[..=at]);
            rest = &rest[at + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// The callees whose calls change their order (design 61.12, C3): the two lists have the same
/// calls (the callee and the count of each), in a different order. The callees in the old
/// order, each one time.
fn call_order(old: &[(String, String)], new: &[(String, String)]) -> Option<Vec<String>> {
    let (a, b) = (old.to_vec(), new.to_vec());
    let (mut sa, mut sb) = (a.clone(), b.clone());
    sa.sort();
    sb.sort();
    if a == b || sa != sb {
        return None;
    }
    let mut moved: Vec<String> = Vec::new();
    for (x, y) in a.iter().zip(&b) {
        for (name, _) in [x, y] {
            if x != y && !moved.contains(name) {
                moved.push(name.clone());
            }
        }
    }
    Some(moved)
}

/// The one value of a type with no data: the unit type, an empty tuple, and a struct with no
/// field (as a constant of such a type, `IrConst::Z`).
fn zero_sized(ir: &Ir, ty: u32) -> Option<V> {
    match ir.t.get(ty as usize)? {
        IrTy::Unit => Some(V::Unit),
        IrTy::Adt { e: false, v, .. } if v.len() == 1 && v[0].is_empty() => Some(V::Tup(Vec::new())),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// The loops (design 61.11, part B).

/// The bound of the bounded run: each loop at most this many times on one path.
pub const UNROLL: usize = 4;
/// The most maps of the locals at the loop heads that the check tries.
const MAX_MAPS: usize = 8;

/// The successors of a terminator (no cleanup path).
fn successors(kind: &IrTermKind) -> Vec<usize> {
    match kind {
        IrTermKind::Goto(target) => vec![*target],
        IrTermKind::Sw(_, arms, otherwise) => arms.iter().map(|(_, target)| *target).chain(std::iter::once(*otherwise)).collect(),
        IrTermKind::Assert(_, _, _, target, _) | IrTermKind::Drop(_, target, _, _) => vec![*target],
        IrTermKind::Call(_, _, _, target, _) => target.iter().copied().collect(),
        IrTermKind::Ret | IrTermKind::Unreach | IrTermKind::U(_) => Vec::new(),
    }
}

/// The heads of the loops of a body: the target of each back edge of a depth-first walk from
/// the entry.
fn loop_heads(ir: &Ir) -> BTreeSet<usize> {
    let mut heads = BTreeSet::new();
    let mut state = vec![0u8; ir.b.len()];
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    if ir.b.is_empty() {
        return heads;
    }
    state[0] = 1;
    while let Some((block, next)) = stack.pop() {
        let list = successors(&ir.b[block].t.k);
        if next < list.len() {
            stack.push((block, next + 1));
            let target = list[next];
            match state.get(target).copied() {
                Some(0) => {
                    state[target] = 1;
                    stack.push((target, 0));
                }
                Some(1) => {
                    heads.insert(target);
                }
                _ => {}
            }
        } else {
            state[block] = 2;
        }
    }
    heads
}

/// A key of a value for the map of the locals: the same key on the two sides for the same
/// value (the inputs and the call results have the same names on the two sides).
fn value_key(value: &V) -> String {
    format!("{value:?}")
}

/// The maps of the locals at the two loop heads (Decided by Claude, 61.11): the locals of the
/// old body in order; each one pairs with a local of the new body of the same type and the same
/// value at the first arrival (the first path of each side). A value with more than one
/// candidate gives more than one map (at most [`MAX_MAPS`]). The check proves or rejects each
/// map, so a wrong map never gives a wrong result.
fn local_maps(a: &CutState, b: &CutState, base: usize, rename: &dyn Fn(&str) -> String) -> Vec<Vec<(usize, usize)>> {
    let mut maps: Vec<Vec<(usize, usize)>> = vec![Vec::new()];
    // The shared input slots map to themselves.
    for slot in 0..base.min(a.slots.len()).min(b.slots.len()) {
        for map in maps.iter_mut() {
            map.push((slot, slot));
        }
    }
    for sa in base..a.slots.len() {
        let va = &a.slots[sa];
        if matches!(va, V::Undef) {
            continue;
        }
        let Some(Some(ta)) = a.types.get(sa) else { continue };
        let mut next: Vec<Vec<(usize, usize)>> = Vec::new();
        for map in &maps {
            let candidates: Vec<usize> = (base..b.slots.len())
                .filter(|sb| !map.iter().any(|(_, used)| used == sb))
                .filter(|sb| b.types.get(*sb).and_then(|ty| ty.as_ref()).is_some_and(|tb| same_type(ta, tb, 0, rename)))
                .filter(|sb| match (va, &b.slots[*sb]) {
                    (V::Ptr(x), V::Ptr(y)) => x.path == y.path && (x.slot == y.slot && x.slot < base || map.iter().any(|(p, q)| *p == x.slot && *q == y.slot)),
                    (_, vb) => value_key(va) == value_key(vb),
                })
                .collect();
            if candidates.is_empty() {
                // No partner: the local stays out of the map (each side gets its own value).
                next.push(map.clone());
                continue;
            }
            for sb in candidates {
                if next.len() >= MAX_MAPS {
                    break;
                }
                let mut found = map.clone();
                found.push((sa, sb));
                next.push(found);
            }
        }
        maps = next;
        maps.truncate(MAX_MAPS);
    }
    maps
}

/// The outcomes of a segment with the mapped values of each loop head state in the list
/// `after` (in the order of the map), so that [`outcome_differs`] compares them.
fn with_map(outs: &[PathOut], map: &[(usize, usize)], old: bool) -> Vec<PathOut> {
    outs.iter()
        .map(|out| {
            let mut out = out.clone();
            if let Some(cut) = &out.cut {
                out.after = map.iter().map(|(sa, sb)| cut.slots.get(if old { *sa } else { *sb }).cloned().unwrap_or(V::Undef)).collect();
            }
            out
        })
        .collect()
}

/// True if the solver finds a pair of paths with different outcomes (`None`: no answer).
fn pairs_differ(solver: &Solver, decls: &Decls, a: &[PathOut], b: &[PathOut], queries: &mut usize) -> Option<bool> {
    let mut body = String::new();
    for line in &decls.lines {
        body.push_str(line);
        body.push('\n');
    }
    for (i, x) in a.iter().enumerate() {
        body.push_str(&format!("(define-fun |pa{i}| () Bool {})\n", x.pc.s));
    }
    for (j, y) in b.iter().enumerate() {
        body.push_str(&format!("(define-fun |pb{j}| () Bool {})\n", y.pc.s));
    }
    for line in &decls.assume {
        body.push_str(&format!("(assert {line})\n"));
    }
    if decls.consts.len() > 1 {
        body.push_str(&format!("(assert (distinct {}))\n", decls.consts.join(" ")));
    }
    let mut pairs = Vec::new();
    for (i, x) in a.iter().enumerate() {
        for (j, y) in b.iter().enumerate() {
            let d = outcome_differs(x, y);
            if lit_bool(&d) != Some(false) {
                pairs.push(format!("(and |pa{i}| |pb{j}| {})", d.s));
            }
        }
    }
    body.push_str(&format!("(assert (or {} false))\n", pairs.join(" ")));
    *queries += 1;
    match solver.check(&body, &[]) {
        Ok((_, Answer::Unsat)) => Some(false),
        Ok((_, Answer::Sat(_))) => Some(true),
        _ => None,
    }
}

/// The state at the loop head for the step of the induction: each mapped pair of slots gets
/// one new value, each other slot its own new value; a reference keeps its place.
#[allow(clippy::too_many_arguments)]
fn fresh_state(side: &Side, cut: &CutState, head: usize, base: usize, map: &[(usize, usize)], old: bool, decls: &mut Decls, shared: &mut BTreeMap<usize, V>) -> Result<State, Stop> {
    let mut slots = cut.slots.clone();
    let mut types = cut.types.clone();
    let mut extra_slots = Vec::new();
    let mut extra_types = Vec::new();
    let mut muts = Vec::new();
    for slot in 0..slots.len() {
        if matches!(slots[slot], V::Undef | V::Ptr(_)) {
            continue;
        }
        let Some(Some(ty)) = types.get(slot).cloned() else { continue };
        let pair = map.iter().position(|(sa, sb)| if old { *sa == slot } else { *sb == slot });
        let value = match pair.and_then(|index| shared.get(&index).cloned()) {
            Some(value) => value,
            None => {
                let name = match pair {
                    Some(index) => format!("@loop {index}"),
                    None => format!("@loop {} {slot}", if old { "o" } else { "n" }),
                };
                let value = input(decls, &ty, &name, &mut extra_slots, &mut extra_types, &mut muts, 0)?;
                if matches!(value, V::Ptr(_)) || !extra_slots.is_empty() {
                    return Err(Stop("a reference in the state of the loop".into()));
                }
                if let Some(index) = pair {
                    shared.insert(index, value.clone());
                }
                value
            }
        };
        slots[slot] = value;
    }
    types.truncate(slots.len());
    // The locals of the top frame follow the input slots (`base`).
    let mut state = State { slots, types, frames: Vec::new(), pc: Vec::new(), effects: Vec::new(), calls: Vec::new(), visits: vec![(0, head)], serial: 1_000_000 };
    state.frames.push(Frame { ir: side.ir.clone(), base, block: head, stmt: 0, ret: None, name: String::new(), id: 0 });
    Ok(state)
}

/// The inductive check of two loops (design 61.11, part B). `None`: the bodies do not have one
/// loop each at the top level, and the cause of the plain run stays.
#[allow(clippy::too_many_arguments)]
fn check_loop(solver: &Solver, old: &Side, new: &Side, step: &Step, inline_all: bool, queries: &mut usize, start: &Decls, inputs: &[V], slots: &[V], types: &[Option<Ty>], muts: &[usize]) -> Option<(Verdict, BTreeSet<String>)> {
    let (heads_a, heads_b) = (loop_heads(&old.ir), loop_heads(&new.ir));
    if heads_a.len() != 1 || heads_b.len() != 1 {
        return None;
    }
    let (ha, hb) = (*heads_a.first()?, *heads_b.first()?);
    let base = slots.len();
    let not_decided = |cause: String, inlined: BTreeSet<String>| Some((Verdict::NotDecided { cause }, inlined));
    // The first segment: from the entry to the first arrival at the loop head.
    let mut decls = start.clone();
    let a = run_side_in(old, step, inline_all, &mut decls, inputs, slots, types, muts, true, LoopMode::Cut(ha));
    let b = run_side_in(new, step, inline_all, &mut decls, inputs, slots, types, muts, false, LoopMode::Cut(hb));
    let mut inlined = a.inlined.clone();
    inlined.extend(b.inlined.iter().cloned());
    if let Some((_, cause)) = a.stops.first().or(b.stops.first()) {
        return not_decided(cause.clone(), inlined);
    }
    let (Some(first_a), Some(first_b)) = (a.outs.iter().find_map(|out| out.cut.clone()), b.outs.iter().find_map(|out| out.cut.clone())) else {
        return None;
    };
    let maps = local_maps(&first_a, &first_b, base, old.rename);
    let mut unproved = "the states at the loop entry differ".to_string();
    for map in &maps {
        // (1) The states at the loop entry are equal.
        if pairs_differ(solver, &decls, &with_map(&a.outs, map, true), &with_map(&b.outs, map, false), queries) != Some(false) {
            continue;
        }
        // (2) One iteration from equal states gives equal states, equal effects, and the same
        // exit decision.
        let mut step_decls = start.clone();
        let mut shared = BTreeMap::new();
        let (Ok(sa), Ok(sb)) = (fresh_state(old, &first_a, ha, base, map, true, &mut step_decls, &mut shared), fresh_state(new, &first_b, hb, base, map, false, &mut step_decls, &mut shared)) else {
            unproved = "the state of the loop has a reference".into();
            continue;
        };
        let x = run_state(old, step, inline_all, &mut step_decls, sa, muts, true, LoopMode::Cut(ha));
        let y = run_state(new, step, inline_all, &mut step_decls, sb, muts, false, LoopMode::Cut(hb));
        inlined.extend(x.inlined.iter().cloned());
        inlined.extend(y.inlined.iter().cloned());
        if let Some((_, cause)) = x.stops.first().or(y.stops.first()) {
            return not_decided(cause.clone(), inlined);
        }
        match pairs_differ(solver, &step_decls, &with_map(&x.outs, map, true), &with_map(&y.outs, map, false), queries) {
            Some(false) => {
                let method = if inlined.is_empty() { "SMT, by induction over the loop".to_string() } else { format!("{}, by induction over the loop", method(&inlined)) };
                return Some((Verdict::Equivalent { method }, inlined));
            }
            _ => unproved = "the loop bodies differ for a state that Semilla cannot show is reachable".into(),
        }
    }
    // A bounded run from the entry: a real input if one exists in the bound (B2).
    let mut decls = start.clone();
    let a = run_side_in(old, step, inline_all, &mut decls, inputs, slots, types, muts, true, LoopMode::Unroll(UNROLL));
    let b = run_side_in(new, step, inline_all, &mut decls, inputs, slots, types, muts, false, LoopMode::Unroll(UNROLL));
    if a.stops.is_empty() && b.stops.is_empty() && !a.outs.is_empty() && !b.outs.is_empty() {
        let (verdict, more) = decide(solver, old, new, inline_all, queries, decls, a, b, slots, types, muts);
        inlined.extend(more);
        if let Verdict::Different { input, before, after } = verdict {
            return Some((Verdict::Different { input: format!("{input} (a bounded run of at most {UNROLL} loop iterations)"), before, after }, inlined));
        }
    }
    not_decided(format!("{unproved}; a bounded run of at most {UNROLL} loop iterations finds no input"), inlined)
}

// ---------------------------------------------------------------------------------------------
// The compare of two outcomes.

/// The condition that two values differ, with a flag: false if the condition uses the identity
/// of a reference.
fn differ(a: &V, b: &V) -> T {
    match (a, b) {
        (V::Bv(x, w), V::Bv(y, v)) if w == v => not(&eq_t(x, y)),
        (V::Bool(x), V::Bool(y)) => not(&eq_t(x, y)),
        (V::Opq(x), V::Opq(y)) => not(&eq_t(x, y)),
        (V::Seq(x), V::Seq(y)) if x.e == y.e => not(&eq_t(&x.s, &y.s)),
        (V::Unit, V::Unit) | (V::Undef, V::Undef) => t("false"),
        (V::Tup(x), V::Tup(y)) if x.len() == y.len() => or(&x.iter().zip(y).map(|(p, q)| differ(p, q)).collect::<Vec<_>>()),
        (V::Enum { d: dx, w: wx, ds, f: fx }, V::Enum { d: dy, w: wy, f: fy, .. }) if wx == wy && fx.len() == fy.len() => {
            let mut parts = vec![not(&eq_t(dx, dy))];
            for (index, (p, q)) in fx.iter().zip(fy).enumerate() {
                if p.len() != q.len() {
                    return tx("true", false);
                }
                let fields = or(&p.iter().zip(q).map(|(r, s)| differ(r, s)).collect::<Vec<_>>());
                if lit_bool(&fields) == Some(false) {
                    continue;
                }
                // Only the fields of the variant of the value count (the two discriminants are
                // equal here, or the first part holds).
                let d = eq_t(dx, &bv_lit(ds.get(index).copied().unwrap_or(index as u128), *wx));
                parts.push(and(&[d, fields]));
            }
            or(&parts)
        }
        (V::Ptr(x), V::Ptr(y)) if x == y => t("false"),
        // Two different places: the identity of a reference is not modeled.
        (V::Ptr(_), V::Ptr(_)) => tx("true", false),
        _ => tx("true", false),
    }
}

/// The condition that two paths have different outcomes. A different list of calls is not an
/// exact difference: a call can have no effect that a caller sees (a call of a pure function).
fn outcome_differs(a: &PathOut, b: &PathOut) -> T {
    if a.effects.len() != b.effects.len() || a.effects.iter().zip(&b.effects).any(|(x, y)| x.callee != y.callee || x.args.len() != y.args.len()) {
        return tx("true", false);
    }
    let mut parts = Vec::new();
    for (x, y) in a.effects.iter().zip(&b.effects) {
        for (p, q) in x.args.iter().zip(&y.args) {
            // Different arguments of a call: the callee can ignore them. Two arguments that are
            // equal for each input give no part (design 64: the call is the same on the two
            // sides).
            let d = differ(p, q);
            if lit_bool(&d) == Some(false) {
                continue;
            }
            parts.push(tx(d.s, false));
        }
    }
    match (&a.exit, &b.exit) {
        (Exit::Return(x), Exit::Return(y)) => parts.push(differ(x, y)),
        (Exit::Panic { kind: k1, args: a1 }, Exit::Panic { kind: k2, args: a2 }) => {
            if k1 != k2 || a1.len() != a2.len() {
                return t("true");
            }
            for (p, q) in a1.iter().zip(a2) {
                parts.push(differ(p, q));
            }
        }
        (Exit::Abort, Exit::Abort) | (Exit::Cut, Exit::Cut) => {}
        _ => return t("true"),
    }
    if a.after.len() != b.after.len() {
        return t("true");
    }
    for (p, q) in a.after.iter().zip(&b.after) {
        parts.push(differ(p, q));
    }
    or(&parts)
}

// ---------------------------------------------------------------------------------------------
// The check.

/// The paths of one side.
struct Paths {
    outs: Vec<PathOut>,
    stops: Vec<(T, String)>,
    inlined: BTreeSet<String>,
}

#[allow(clippy::too_many_arguments)]
fn run_side(side: &Side, step: &Step, inline_all: bool, decls: &mut Decls, inputs: &[V], slots: &[V], types: &[Option<Ty>], muts: &[usize], old: bool) -> Paths {
    run_side_in(side, step, inline_all, decls, inputs, slots, types, muts, old, LoopMode::Plain)
}

/// The start state of a side: the shared input slots, then the locals of the body.
fn start_state(side: &Side, inputs: &[V], slots: &[V], types: &[Option<Ty>]) -> State {
    let ir = side.ir.clone();
    let mut state = State { slots: slots.to_vec(), types: types.to_vec(), frames: Vec::new(), pc: Vec::new(), effects: Vec::new(), calls: Vec::new(), visits: Vec::new(), serial: 0 };
    let base = state.slots.len();
    for ty in &ir.l {
        state.slots.push(V::Undef);
        state.types.push(Some(Ty { ir: ir.clone(), id: *ty }));
    }
    for (index, value) in inputs.iter().enumerate() {
        state.slots[base + 1 + index] = value.clone();
    }
    state.frames.push(Frame { ir: ir.clone(), base, block: 0, stmt: 0, ret: None, name: String::new(), id: 0 });
    state.visits.push((0, 0));
    state
}

#[allow(clippy::too_many_arguments)]
fn run_side_in(side: &Side, step: &Step, inline_all: bool, decls: &mut Decls, inputs: &[V], slots: &[V], types: &[Option<Ty>], muts: &[usize], old: bool, mode: LoopMode) -> Paths {
    let state = start_state(side, inputs, slots, types);
    run_state(side, step, inline_all, decls, state, muts, old, mode)
}

/// Runs a side from a state.
#[allow(clippy::too_many_arguments)]
fn run_state(side: &Side, step: &Step, inline_all: bool, decls: &mut Decls, state: State, muts: &[usize], old: bool, mode: LoopMode) -> Paths {
    let mut run = Run { side, step, inline_all, decls, steps: 0, outs: Vec::new(), stops: Vec::new(), inlined: BTreeSet::new(), muts: muts.to_vec(), calls: 0, old, mode, dropped: 0 };
    run.run(state);
    Paths { outs: run.outs, stops: run.stops, inlined: run.inlined }
}

/// The text of a value in a model.
fn value_text(value: &V, ty: Option<&Ty>, model: &BTreeMap<String, String>) -> String {
    match value {
        V::Bv(term, w) => {
            let raw = model.get(&term.s).cloned().or_else(|| lit_bv(term).map(|v| format!("(_ bv{v} {w})"))).unwrap_or_else(|| "?".into());
            let bits = parse_bv(&raw);
            match (bits, ty.and_then(|ty| ty.get())) {
                (Some(v), Some(IrTy::Int(w, true))) => signed_lit(v, *w).to_string(),
                (Some(v), Some(IrTy::Char)) => char::from_u32(v as u32).map(|c| format!("{c:?}")).unwrap_or_else(|| v.to_string()),
                (Some(v), _) => v.to_string(),
                (None, _) => raw,
            }
        }
        V::Bool(term) => model.get(&term.s).cloned().or_else(|| lit_bool(term).map(|v| v.to_string())).unwrap_or_else(|| "?".into()),
        V::Unit => "()".into(),
        V::Tup(items) => {
            let types: Vec<Option<Ty>> = match ty.and_then(|ty| ty.get()) {
                Some(IrTy::Tuple(list)) => list.iter().map(|id| ty.map(|ty| ty.at(*id))).collect(),
                Some(IrTy::Adt { v, .. }) => v.first().map(|list| list.iter().map(|id| ty.map(|ty| ty.at(*id))).collect()).unwrap_or_default(),
                _ => Vec::new(),
            };
            let parts: Vec<String> = items.iter().enumerate().map(|(index, item)| value_text(item, types.get(index).and_then(|ty| ty.as_ref()), model)).collect();
            match ty.and_then(|ty| ty.get()) {
                Some(IrTy::Adt { n, .. }) if parts.is_empty() => short_type(n),
                Some(IrTy::Adt { n, .. }) => format!("{} {{ {} }}", short_type(n), parts.join(", ")),
                _ => format!("({})", parts.join(", ")),
            }
        }
        V::Enum { d, w, ds, f } => {
            let raw = model.get(&d.s).cloned().or_else(|| lit_bv(d).map(|v| format!("(_ bv{v} {w})"))).unwrap_or_default();
            let bits = parse_bv(&raw);
            let Some(IrTy::Adt { v, vn, .. }) = ty.and_then(|ty| ty.get()) else { return "?".into() };
            let index = ds.iter().position(|value| Some(*value) == bits);
            let Some(index) = index else { return "?".into() };
            let name = vn.get(index).cloned().unwrap_or_else(|| format!("variant {index}"));
            let fields = f.get(index).cloned().unwrap_or_default();
            if fields.is_empty() {
                return name;
            }
            let types = v.get(index).cloned().unwrap_or_default();
            let parts: Vec<String> = fields.iter().zip(types).map(|(item, id)| value_text(item, ty.map(|ty| ty.at(id)).as_ref(), model)).collect();
            format!("{name}({})", parts.join(", "))
        }
        V::Ptr(_) => "a reference".into(),
        V::Opq(_) => "a value".into(),
        V::Undef => "no value".into(),
        V::Seq(Sq { s: term, e, .. }) => {
            let item = match ty.and_then(|ty| ty.get()) {
                Some(IrTy::Seq { e, .. }) => ty.map(|ty| ty.at(*e)),
                _ => None,
            };
            match model.get(&term.s) {
                Some(raw) => {
                    let parts: Vec<String> = seq_units(raw).iter().map(|unit| {
                        let value = e.value(t(unit.clone()));
                        let mut known = BTreeMap::new();
                        known.insert(unit.clone(), unit.clone());
                        value_text(&value, item.as_ref(), &known)
                    }).collect();
                    format!("[{}]", parts.join(", "))
                }
                None => "?".into(),
            }
        }
    }
}

/// The elements of a sequence value of a model: `(seq.++ (seq.unit #x00000001) (seq.unit
/// #x00000002))` gives `#x00000001` and `#x00000002`; `(as seq.empty …)` gives none.
fn seq_units(raw: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = raw;
    while let Some(at) = rest.find("(seq.unit ") {
        rest = &rest[at + "(seq.unit ".len()..];
        let mut depth = 0usize;
        let mut end = rest.len();
        for (index, c) in rest.char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    end = index;
                    break;
                }
                ')' => depth -= 1,
                _ => {}
            }
        }
        found.push(rest[..end].trim().to_string());
        rest = &rest[end..];
    }
    found
}

fn short_type(text: &str) -> String {
    let base = text.split('<').next().unwrap_or(text);
    base.rsplit("::").next().unwrap_or(base).to_string()
}

fn parse_bv(text: &str) -> Option<u128> {
    if let Some(hex) = text.strip_prefix("#x") {
        return u128::from_str_radix(hex, 16).ok();
    }
    if let Some(bin) = text.strip_prefix("#b") {
        return u128::from_str_radix(bin, 2).ok();
    }
    let rest = text.strip_prefix("(_ bv")?;
    rest.split(' ').next()?.parse().ok()
}

/// The text of the outcome of a path in a model. `muts` names the targets of the `&mut`
/// parameters (the first values of `after`).
fn outcome_text(out: &PathOut, ret: Option<&Ty>, model: &BTreeMap<String, String>, other: &PathOut, muts: &[(String, Option<Ty>)]) -> String {
    let mut text = match &out.exit {
        Exit::Return(value) => format!("returns {}", value_text(value, ret, model)),
        Exit::Panic { kind, .. } if kind.starts_with("call ") => format!("panics ({})", kind.trim_start_matches("call ")),
        Exit::Panic { kind, .. } => format!("panics ({kind})"),
        Exit::Abort => "aborts".into(),
        Exit::Cut => "reaches the loop head".into(),
    };
    let calls: Vec<&str> = out.effects.iter().map(|effect| effect.callee.as_str()).collect();
    let others: Vec<&str> = other.effects.iter().map(|effect| effect.callee.as_str()).collect();
    if calls != others {
        let list = if calls.is_empty() { "no call".to_string() } else { calls.iter().map(|call| format!("`{call}`")).collect::<Vec<_>>().join(", ") };
        text = format!("{text} (calls: {list})");
    }
    for (index, (name, ty)) in muts.iter().enumerate() {
        let (Some(mine), Some(theirs)) = (out.after.get(index), other.after.get(index)) else { continue };
        let (a, b) = (value_text(mine, ty.as_ref(), model), value_text(theirs, ty.as_ref(), model));
        if a != b {
            text = format!("{text} with `{name}` = {a}");
        }
    }
    text
}

/// The terms of a value, for a request of their values in a model.
fn terms_of(value: &V, out: &mut Vec<String>) {
    for (_, term) in flatten(value) {
        if lit_bv(&term).is_none() && lit_bool(&term).is_none() && !out.contains(&term.s) {
            out.push(term.s);
        }
    }
}

/// Checks two versions of a body. The first try inlines only the callees that the step changes;
/// a second try inlines each callee with a form, when the first try proves nothing.
pub fn check_bodies(solver: &Solver, old: &Side, new: &Side, step: &Step) -> Checked {
    let started = Instant::now();
    let mut queries = 0;
    let mut result = check_once(solver, old, new, step, false, &mut queries);
    let first_inlined = result.1.clone();
    let retry = match &result.0 {
        Verdict::Equivalent { .. } | Verdict::Different { .. } => false,
        Verdict::NotDecided { .. } => true,
    };
    if retry {
        let second = check_once(solver, old, new, step, true, &mut queries);
        // The second try gives the result if it decides, or if it inlines more.
        let decides = !matches!(second.0, Verdict::NotDecided { .. });
        if decides || second.1.len() > first_inlined.len() {
            if decides || matches!(&result.0, Verdict::NotDecided { cause } if cause.contains("depends on")) {
                result = second;
            }
        }
    }
    let (verdict, inlined) = result;
    Checked { verdict, inlined: inlined.into_iter().collect(), queries, millis: started.elapsed().as_millis() }
}

/// The method text of an equivalence.
fn method(inlined: &BTreeSet<String>) -> String {
    if inlined.is_empty() { "SMT".into() } else { format!("SMT after inlining {}", inlined.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")) }
}

fn check_once(solver: &Solver, old: &Side, new: &Side, step: &Step, inline_all: bool, queries: &mut usize) -> (Verdict, BTreeSet<String>) {
    let not_decided = |cause: String| (Verdict::NotDecided { cause }, BTreeSet::new());
    for side in [old, new] {
        if let Some(cause) = &side.ir.u {
            // A failed extraction step of the driver (design 61.9, part 0).
            if let Some(rest) = cause.strip_prefix("no form: ") {
                return not_decided(format!("Semilla has no form of this body ({rest})"));
            }
            return not_decided(cause.clone());
        }
    }
    if old.ir.a != new.ir.a || old.ir.l.len().min(old.ir.a + 1) != old.ir.a + 1 || new.ir.l.len() < new.ir.a + 1 {
        return not_decided("the parameters change".into());
    }
    let ty_of = |side: &Side, local: usize| Ty { ir: side.ir.clone(), id: side.ir.l[local] };
    for local in 0..=old.ir.a {
        if !same_type(&ty_of(old, local), &ty_of(new, local), 0, old.rename) {
            return not_decided(if local == 0 { "the return type changes".into() } else { "the parameters change".into() });
        }
    }
    // The inputs: the same values for the two sides.
    let mut decls = Decls::default();
    let mut slots = Vec::new();
    let mut types = Vec::new();
    let mut muts = Vec::new();
    let mut inputs = Vec::new();
    for local in 1..=old.ir.a {
        let name = new.ir.n.get(local - 1).cloned().unwrap_or_else(|| format!("_{local}"));
        let ty = ty_of(new, local);
        match input(&mut decls, &ty, &name, &mut slots, &mut types, &mut muts, 0) {
            Ok(value) => {
                decls.inputs.push((name, value.clone(), ty));
                inputs.push(value);
            }
            Err(Stop(cause)) => return not_decided(cause),
        }
    }
    let start = decls.clone();
    let a = run_side(old, step, inline_all, &mut decls, &inputs, &slots, &types, &muts, true);
    let b = run_side(new, step, inline_all, &mut decls, &inputs, &slots, &types, &muts, false);
    if let Some((_, cause)) = a.stops.first().or(b.stops.first()) {
        // A loop: the inductive check of the two loops (design 61.11, part B).
        if cause.starts_with("a loop")
            && let Some(found) = check_loop(solver, old, new, step, inline_all, queries, &start, &inputs, &slots, &types, &muts)
        {
            return found;
        }
        let mut inlined = a.inlined.clone();
        inlined.extend(b.inlined.iter().cloned());
        return (Verdict::NotDecided { cause: cause.clone() }, inlined);
    }
    decide(solver, old, new, inline_all, queries, decls, a, b, &slots, &types, &muts)
}

/// The query of a check after the runs of the two sides: an input and a pair of paths with
/// different outcomes.
#[allow(clippy::too_many_arguments)]
fn decide(solver: &Solver, old: &Side, new: &Side, inline_all: bool, queries: &mut usize, decls: Decls, a: Paths, b: Paths, slots: &[V], types: &[Option<Ty>], muts: &[usize]) -> (Verdict, BTreeSet<String>) {
    let mut inlined = a.inlined.clone();
    inlined.extend(b.inlined.iter().cloned());
    // The query: an input and a pair of paths with different outcomes.
    let mut defines = Vec::new();
    let mut pairs = Vec::new();
    for (i, x) in a.outs.iter().enumerate() {
        defines.push(format!("(define-fun |pa{i}| () Bool {})", x.pc.s));
    }
    for (j, y) in b.outs.iter().enumerate() {
        defines.push(format!("(define-fun |pb{j}| () Bool {})", y.pc.s));
    }
    let mut differs: Vec<Vec<T>> = Vec::new();
    for (i, x) in a.outs.iter().enumerate() {
        let mut row = Vec::new();
        for (j, y) in b.outs.iter().enumerate() {
            let d = outcome_differs(x, y);
            if lit_bool(&d) != Some(false) {
                pairs.push(format!("(and |pa{i}| |pb{j}| {})", d.s));
            }
            row.push(d);
        }
        differs.push(row);
    }
    let mut body = String::new();
    for line in &decls.lines {
        body.push_str(line);
        body.push('\n');
    }
    for line in &defines {
        body.push_str(line);
        body.push('\n');
    }
    for line in &decls.assume {
        body.push_str(&format!("(assert {line})\n"));
    }
    if decls.consts.len() > 1 {
        body.push_str(&format!("(assert (distinct {}))\n", decls.consts.join(" ")));
    }
    body.push_str(&format!("(assert (or {} false))\n", pairs.join(" ")));
    let mut symbols: Vec<String> = Vec::new();
    for (_, value, _) in &decls.inputs {
        terms_of(value, &mut symbols);
        // The target of a reference input.
        if let V::Ptr(loc) = value
            && let Some(target) = slots.get(loc.slot)
        {
            terms_of(target, &mut symbols);
        }
    }
    symbols.extend((0..a.outs.len()).map(|i| format!("|pa{i}|")));
    symbols.extend((0..b.outs.len()).map(|j| format!("|pb{j}|")));
    *queries += 1;
    let answer = match solver.check(&body, &symbols) {
        Ok((_, answer)) => answer,
        Err(error) => return (Verdict::NotDecided { cause: format!("the solver failed: {error}") }, inlined),
    };
    match answer {
        Answer::Unsat => (Verdict::Equivalent { method: method(&inlined) }, inlined),
        Answer::Unknown(reason) => (Verdict::NotDecided { cause: format!("the solver gave no answer ({reason})") }, inlined),
        Answer::Sat(model) => {
            let mut model: BTreeMap<String, String> = model.into_iter().collect();
            // A small input if one exists (an input with small numbers is easier to read).
            // The numbers of the inputs, and of the target of each reference input.
            let targets: Vec<V> = decls.inputs.iter().filter_map(|(_, value, _)| if let V::Ptr(loc) = value { slots.get(loc.slot).cloned() } else { None }).collect();
            let numbers: Vec<(String, u32)> = decls
                .inputs
                .iter()
                .map(|(_, value, _)| value)
                .chain(targets.iter())
                .flat_map(flatten)
                .filter_map(|(sort, term)| {
                    let w: u32 = sort.strip_prefix("(_ BitVec ")?.trim_end_matches(')').parse().ok()?;
                    (w > 8 && !term.s.ends_with(".d|")).then_some((term.s, w))
                })
                .collect();
            // A sequence input has at most 2, 3, or 4 elements in the three narrow tries (design
            // 64, part C: a short list is easier to read).
            // A sequence input has at most `most` elements, each at most `bound` (design 64,
            // part C: a short list with small numbers is easier to read).
            let short = |most: usize, bound: u128| -> Vec<String> {
                let mut list = Vec::new();
                for (symbol, e) in &decls.seqs {
                    list.push(format!("(<= (seq.len {symbol}) {most})"));
                    if let SeqE::Bv(w) = e {
                        for index in 0..most {
                            list.push(format!("(=> (< {index} (seq.len {symbol})) (bvule (seq.nth {symbol} {index}) {}))", bv_lit(bound, *w).s));
                        }
                    }
                }
                list
            };
            let upto = |bound: u128, most: usize| -> Vec<String> {
                let mut list: Vec<String> = numbers.iter().map(|(term, w)| format!("(bvule {term} {})", bv_lit(bound, *w).s)).collect();
                list.extend(short(most, bound));
                list
            };
            let mut tries = vec![upto(1, 2), upto(10, 3), upto(1000, 4)];
            if !decls.seqs.is_empty() {
                // The shortest sequences first.
                tries.insert(0, upto(1, 1));
                tries.insert(0, upto(1, 0));
            }
            let mut small: Vec<String> = numbers.iter().map(|(term, w)| format!("(or (bvule {term} {}) (bvuge {term} {}))", bv_lit(1000, *w).s, bv_lit(u128::MAX - 999, *w).s)).collect();
            small.extend(short(4, 1000));
            tries.push(small.clone());
            if !small.is_empty() {
                // First the smallest numbers that are not negative (at most 1, 10, 1000), then
                // small numbers of each sign (Decided by Claude, 61.10: a small input is easier
                // to read).
                for list in &tries {
                    let narrow = format!("{body}(assert (and {}))\n", list.join(" "));
                    *queries += 1;
                    if let Ok((_, Answer::Sat(found))) = solver.check(&narrow, &symbols) {
                        model = found.into_iter().collect();
                        break;
                    }
                }
            }
            let pick = |prefix: &str, count: usize| (0..count).find(|index| model.get(&format!("|{prefix}{index}|")).map(String::as_str) == Some("true"));
            let (Some(i), Some(j)) = (pick("pa", a.outs.len()), pick("pb", b.outs.len())) else {
                return (Verdict::NotDecided { cause: "the model of the solver has no pair of paths".into() }, inlined);
            };
            let (x, y) = (&a.outs[i], &b.outs[j]);
            // The model is a real input only if the two path conditions and the difference use
            // only the modeled inputs.
            if !(x.pc.x && y.pc.x && differs[i][j].x) {
                // The same calls in a different order (design 61.12, C3).
                // The pair of the model first, then each pair of paths that return (the model can
                // select a pair of paths that stop at different calls).
                let returns = |paths: &[PathOut]| paths.iter().filter(|out| matches!(out.exit, Exit::Return(_))).map(|out| out.calls.clone()).collect::<Vec<_>>();
                let others = returns(&a.outs).into_iter().flat_map(|p| returns(&b.outs).into_iter().map(move |q| (p.clone(), q)));
                let names = |list: &[(String, String)]| list.iter().map(|(name, _)| (name.clone(), String::new())).collect::<Vec<_>>();
                let pairs: Vec<(Vec<(String, String)>, Vec<(String, String)>)> = std::iter::once((x.calls.clone(), y.calls.clone())).chain(others).collect();
                // The names first (two calls that read each other's write have other inputs on
                // the two sides), then the names with the inputs (one callee with other inputs).
                let by_name = pairs.iter().find_map(|(p, q)| call_order(&names(p), &names(q)));
                if let Some(moved) = by_name.or_else(|| pairs.iter().find_map(|(p, q)| call_order(p, q))) {
                    return (Verdict::NotDecided { cause: format!("the order of the calls changes: {}", moved.join(", ")) }, inlined);
                }
                let cause = if inline_all { "the difference depends on the result of a call or on a value that the check does not model" } else { "the difference depends on the result of a call or on a value that the check does not model (no callee inlined)" };
                return (Verdict::NotDecided { cause: cause.into() }, inlined);
            }
            // The values of the two outcomes: a second query with the inputs of the model.
            let mut wanted = Vec::new();
            for out in [x, y] {
                if let Exit::Return(value) = &out.exit {
                    terms_of(value, &mut wanted);
                }
                for value in &out.after {
                    terms_of(value, &mut wanted);
                }
            }
            let mut fixed = String::new();
            for line in &decls.lines {
                fixed.push_str(line);
                fixed.push('\n');
            }
            // The assumptions fix the parts of an input that the model does not give (the length
            // of a sequence, design 64).
            for line in &decls.assume {
                fixed.push_str(&format!("(assert {line})\n"));
            }
            for symbol in &symbols {
                // A value of an opaque sort (`Opaque!val!0`) has no text for an assertion.
                if let Some(value) = model.get(symbol)
                    && symbol.starts_with("|in ")
                    && !value.contains("!val!")
                {
                    fixed.push_str(&format!("(assert (= {symbol} {value}))\n"));
                }
            }
            let mut values = model.clone();
            if !wanted.is_empty() {
                // A name for each term: the solver gives the value of a name.
                let mut names = Vec::new();
                for (index, term) in wanted.iter().enumerate() {
                    let sort = [x, y].iter().find_map(|out| {
                        let mut all: Vec<(String, T)> = out.after.iter().flat_map(flatten).collect();
                        if let Exit::Return(value) = &out.exit {
                            all.extend(flatten(value));
                        }
                        all.into_iter().find(|(_, found)| found.s == *term).map(|(sort, _)| sort)
                    });
                    let Some(sort) = sort else { continue };
                    fixed.push_str(&format!("(define-fun |w{index}| () {sort} {term})\n"));
                    names.push((format!("|w{index}|"), term.clone()));
                }
                *queries += 1;
                let request: Vec<String> = names.iter().map(|(name, _)| name.clone()).collect();
                if let Ok((_, Answer::Sat(found))) = solver.check(&fixed, &request) {
                    let found: BTreeMap<String, String> = found.into_iter().collect();
                    for (name, term) in &names {
                        if let Some(value) = found.get(name) {
                            values.insert(term.clone(), value.clone());
                        }
                    }
                }
            }
            let ret_ty = Ty { ir: new.ir.clone(), id: new.ir.l[0] };
            // A reference input shows its target: `x = &5`.
            let shown = |value: &V, ty: &Ty| -> String {
                match (value, ty.get()) {
                    (V::Ptr(loc), Some(IrTy::Ref(_, inner))) if loc.path.is_empty() => format!("&{}", value_text(&slots[loc.slot], Some(&ty.at(*inner)), &values)),
                    _ => value_text(value, Some(ty), &values),
                }
            };
            let input = decls.inputs.iter().map(|(name, value, ty)| format!("{name} = {}", shown(value, ty))).collect::<Vec<_>>().join(", ");
            let input = if input.is_empty() { "each input".to_string() } else { input };
            let mut_names: Vec<(String, Option<Ty>)> = muts.iter().map(|slot| (decls.mut_names.iter().find(|(at, _)| at == slot).map(|(_, name)| name.clone()).unwrap_or_else(|| "*_".into()), types[*slot].clone())).collect();
            let before = outcome_text(x, Some(&Ty { ir: old.ir.clone(), id: old.ir.l[0] }), &values, y, &mut_names);
            let after = outcome_text(y, Some(&ret_ty), &values, x, &mut_names);
            let (before, after) = if before == after { (format!("{before} (a different effect or value behind a reference)"), after) } else { (before, after) };
            (Verdict::Different { input, before, after }, inlined)
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn int_ir(u: Option<&str>) -> Arc<Ir> {
        Arc::new(Ir { a: 1, l: vec![0, 0], t: vec![IrTy::Int(32, false)], b: Vec::new(), f: Vec::new(), n: vec!["x".into()], u: u.map(str::to_string) })
    }

    // rule+ R61.9-2
    // rule- R61.9-2
    #[test]
    fn a_failed_extraction_step_gives_no_form() {
        let std = BTreeMap::new();
        let none = |_: &str| None;
        let same = |text: &str| text.to_string();
        let changed = |_: &str| false;
        let step = Step { changed: &changed, changed_names: &[] };
        let solver = Solver::find_for(&[], None).expect("the solver of the workspace");
        for (cause, want) in [("no form: the driver failed to read the body", "Semilla has no form of this body (the driver failed to read the body)"), ("a coroutine body", "a coroutine body")] {
            let side = |ir: Arc<Ir>| Side { ir, workspace: &none, std: &std, rename: &same };
            let checked = check_bodies(&solver, &side(int_ir(Some(cause))), &side(int_ir(None)), &step);
            assert_eq!(checked.verdict, Verdict::NotDecided { cause: want.into() });
        }
    }

    // rule- R61.12-1
    #[test]
    fn two_equal_terms_are_an_exact_equality_and_two_different_ones_are_not_literal() {
        let opaque = tx("|k a|", false);
        assert_eq!(eq_t(&opaque, &opaque), t("true"));
        let other = tx("|k b|", false);
        let found = eq_t(&opaque, &other);
        assert!(lit_bool(&found).is_none() && !found.x, "{found:?}");
    }

    #[test]
    fn a_closure_type_has_no_source_place() {
        assert_eq!(without_place("&mut {closure@src/lib.rs:2:18: 2:21}"), "&mut {closure}");
        assert_eq!(without_place("a@b"), "a@b");
    }

    // rule- R61.12-3
    // rule- R64.3-3
    #[test]
    fn the_order_of_the_calls_needs_the_same_calls() {
        let call = |name: &str| (name.to_string(), String::new());
        assert_eq!(call_order(&[call("a"), call("b")], &[call("b"), call("a")]), Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(call_order(&[call("a"), call("b")], &[call("a"), call("c")]), None);
        assert_eq!(call_order(&[call("a")], &[call("a")]), None);
        // One callee with other inputs in another order (design 64): the key of the inputs.
        let with = |name: &str, key: &str| (name.to_string(), key.to_string());
        assert_eq!(call_order(&[with("a", "1"), with("a", "2")], &[with("a", "2"), with("a", "1")]), Some(vec!["a".to_string()]));
        assert_eq!(call_order(&[with("a", "1"), with("a", "2")], &[with("a", "1"), with("a", "3")]), None);
    }

    // rule- R64.4-2
    #[test]
    fn a_sequence_value_of_a_model_is_a_list() {
        assert_eq!(seq_units("(seq.++ (seq.unit #x00000001) (seq.unit (_ bv2 32)))"), ["#x00000001", "(_ bv2 32)"]);
        assert!(seq_units("(as seq.empty (Seq (_ BitVec 32)))").is_empty());
        let mut model = BTreeMap::new();
        model.insert("s".to_string(), "(seq.++ (seq.unit #x00000002) (seq.unit #x00000001))".to_string());
        let ty = Ty { ir: Arc::new(Ir { t: vec![IrTy::Seq { e: 1, n: None, k: "v".into() }, IrTy::Int(32, false)], ..Ir::default() }), id: 0 };
        let value = V::Seq(Sq { s: t("s"), e: SeqE::Bv(32), n: t("n") });
        assert_eq!(value_text(&value, Some(&ty), &model), "[2, 1]");
        model.insert("s".to_string(), "(as seq.empty (Seq (_ BitVec 32)))".to_string());
        assert_eq!(value_text(&value, Some(&ty), &model), "[]");
    }

    // rule- R61.12-4
    #[test]
    fn an_intrinsic_outside_the_list_is_an_effect_and_a_listed_one_has_its_meaning() {
        let a = V::Bv(bv_lit(250, 8), 8);
        let b = V::Bv(bv_lit(10, 8), 8);
        assert_eq!(intrinsic("ctpop", std::slice::from_ref(&a), None).unwrap(), None);
        // 250 + 10 saturates at 255 for `u8`; 250 - 10 does not.
        assert_eq!(intrinsic("saturating_add", &[a.clone(), b.clone()], None).unwrap(), Some(V::Bv(bv_lit(255, 8), 8)));
        assert_eq!(intrinsic("saturating_sub", &[a.clone(), b.clone()], None).unwrap(), Some(V::Bv(bv_lit(240, 8), 8)));
        assert_eq!(intrinsic("saturating_sub", &[b.clone(), a.clone()], None).unwrap(), Some(V::Bv(bv_lit(0, 8), 8)));
        assert_eq!(intrinsic("integer_min", &[a.clone(), b.clone()], None).unwrap(), Some(V::Bv(bv_lit(10, 8), 8)));
        assert_eq!(intrinsic("integer_max", &[a, b], None).unwrap(), Some(V::Bv(bv_lit(250, 8), 8)));
    }

    #[test]
    fn a_loop_head_is_the_target_of_a_back_edge() {
        let block = |k: IrTermKind| IrBlock { s: Vec::new(), t: IrTerm { k, r: None, c: None } };
        let ir = Ir { a: 0, l: vec![0], t: vec![IrTy::Unit], b: vec![block(IrTermKind::Goto(1)), block(IrTermKind::Sw(IrOp::R("f".into()), vec![("0".into(), 2)], 1)), block(IrTermKind::Ret)], f: Vec::new(), n: Vec::new(), u: None };
        assert_eq!(loop_heads(&ir), BTreeSet::from([1]));
    }
}
