//! The body in the form of the equivalence check (design 61, part C).
//!
//! The driver writes the MIR of each body at N0 (after the drop elaboration, before the
//! optimization passes: the MIR of the graph) as a small structured form (`q` in the record).
//! The check of `semilla-smt` (`mir.rs`) reads it. The form keeps the order of the MIR and each
//! check of the MIR. It never drops a part that it does not model: such a part is `U` with its
//! cause, and the check gives "not decided" for a path that reaches it.
//!
//! - Each type is in a table: `Bool`, `Char`, `Unit`, `Never`, `Int` (the width and the sign),
//!   `Tuple`, `Adt` (a struct or an enum of the workspace, `Option`, `Result`, `Ordering`: the
//!   fields of each variant and the discriminants), `Ref` (a reference), and `Opaque` (each
//!   other type: the check only moves its values).
//! - A constant of a scalar type has its evaluated value. A different constant has the text of
//!   the printer (its full value) as its identity.
//! - A call has the callee that the MIR calls (the text of R60.2-11), the name of its body for
//!   a function of the workspace, the name of an intrinsic, and the places that its cleanup
//!   path drops.

use std::collections::{BTreeMap, HashMap};

use rustc_public::mir::{
    AggregateKind, AssertMessage, BinOp, Body, BorrowKind, CastKind, Operand, Place, ProjectionElem, Rvalue, StatementKind,
    TerminatorKind, UnOp, UnwindAction,
};
use rustc_public::ty::{AdtKind, ConstantKind, GenericArgKind, RigidTy, Ty, TyKind};
use rustc_public::CrateDef;
use rustc_public_bridge::IndexedVal;
use serde::Serialize;

/// The most blocks of a body with a form.
pub const MAX_BLOCKS: usize = 400;
/// The most types in the table of one body.
const MAX_TYPES: usize = 200;
/// The most blocks of a callee of the standard library with a form in the record.
pub const MAX_STD_BLOCKS: usize = 24;

/// The form of one body.
#[derive(Serialize, Default, Clone)]
pub struct Ir {
    /// The count of the parameters.
    pub a: usize,
    /// The type of each local (an index into `t`).
    pub l: Vec<u32>,
    pub t: Vec<IrTy>,
    pub b: Vec<IrBlock>,
    /// The files of the places.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub f: Vec<String>,
    /// The name of each parameter (for the input of a difference).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub n: Vec<String>,
    /// The cause if the body has no form (too large, a coroutine).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub u: Option<String>,
}

#[derive(Serialize, Clone, PartialEq)]
pub enum IrTy {
    Bool,
    Char,
    Unit,
    Never,
    Int(u32, bool),
    Tuple(Vec<u32>),
    /// A struct or an enum: the name, the field types of each variant, the discriminant of
    /// each variant, the type of the discriminant, and true for an enum.
    Adt {
        n: String,
        v: Vec<Vec<u32>>,
        /// The name of each variant of an enum.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        vn: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        d: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        dt: Option<u32>,
        e: bool,
        /// The name of each field of each variant (design 67, part B: a claim reads a field by
        /// its name, also a private field). A field of a tuple struct has its index as name.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        fs: Vec<Vec<String>>,
    },
    /// A reference: true for `&mut`, the type of the target.
    Ref(bool, u32),
    /// A type that the check does not model: the name, and true if a value needs a drop.
    Opaque { n: String, d: bool },
    /// A sequence of scalars (design 64, part C): a `Vec` (`k` = "v"), a slice ("s"), or an
    /// array ("a", with its length `n`). `e` is the type of an element.
    Seq {
        e: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        n: Option<u64>,
        k: String,
    },
    /// A closure (design 66, part A): the name of its body, the type of each capture (a capture
    /// by reference is a reference), and the key of the form of the body in the forms of the
    /// instances (`qx`) for a closure of a generic item with concrete arguments or of the
    /// standard library. The check also gives a closure value the identity of its body.
    Clo {
        n: String,
        v: Vec<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        g: Option<String>,
        /// True if a value needs a drop (a capture with a destructor).
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        d: bool,
        /// True for a closure of the standard library: the same body in each version of the
        /// workspace (one tool chain).
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        s: bool,
        /// The key and the body name of each closure type in the generic arguments of the parent
        /// item.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        q: Vec<(String, String)>,
    },
}

#[derive(Serialize, Clone)]
pub struct IrBlock {
    pub s: Vec<IrStmt>,
    pub t: IrTerm,
}

/// A place: the local and the projections.
#[derive(Serialize, Clone)]
pub struct IrPlace {
    pub l: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub p: Vec<IrProj>,
}

#[derive(Serialize, Clone)]
pub enum IrProj {
    /// A deref.
    D,
    /// A field.
    F(usize),
    /// A downcast to a variant.
    V(usize),
    /// An index with a local (design 64, part C).
    I(usize),
    /// An index with a constant: the offset, and true if it counts from the end.
    K(u64, bool),
    /// A projection that the check does not model.
    U(String),
}

#[derive(Serialize, Clone)]
pub enum IrOp {
    C(IrPlace),
    M(IrPlace),
    K(IrConst),
    /// A flag of the code generation (`cfg!(ub_checks)` at codegen time): the name.
    R(String),
}

#[derive(Serialize, Clone)]
pub enum IrConst {
    /// An integer, a `bool` (0 or 1), or a `char`: the bits as a decimal text, and the type.
    I(String, u32),
    /// A value with no bytes (the unit value, a unit struct): its type.
    Z(u32),
    /// A function item: the callee text.
    Fn(String),
    /// A constant of a different type: the text of the printer (its full value), and the type.
    /// `x` is true if two different texts are two different values.
    O(String, u32, bool),
    /// A value of an ADT with scalar fields: the type, the variant, and each field (the bits
    /// and the type). Design 61.12, C4: `const Option::<u32>::None`.
    A(u32, usize, Vec<(String, u32)>),
    /// A reference to an array of scalars (design 64, part C): the type of an element and the
    /// bits of each element.
    S(u32, Vec<String>),
    /// A constant that the compiler does not evaluate here (design 66, part B): a generic or an
    /// associated constant, a promoted constant of a generic body, or a const parameter. The
    /// path with the generic arguments, the type, and the digest of each item that its value
    /// depends on (`consts::definition_digest`). The check gives it an uninterpreted value of
    /// the path and the digest: one value in the two versions only if each item is equal.
    G(String, u32, Vec<(String, String)>),
    /// A constant that the check does not model.
    U(String),
}

#[derive(Serialize, Clone)]
pub enum IrRvalue {
    Use(IrOp),
    Bin(String, IrOp, IrOp),
    Chk(String, IrOp, IrOp),
    Un(String, IrOp),
    /// An integer cast: the operand and the target type.
    Cast(IrOp, u32),
    /// A tuple (`T`) or a variant of an ADT (`A`, the type and the variant).
    Agg(IrAgg, Vec<IrOp>),
    /// A reference: true for `&mut`.
    Ref(bool, IrPlace),
    Disc(IrPlace),
    U(String),
}

#[derive(Serialize, Clone)]
pub enum IrAgg {
    T,
    A(u32, usize),
    /// An array of scalars: the type of an element (design 64, part C).
    S(u32),
    /// A closure: its type (an `IrTy::Clo`), with the captures as operands (design 66, part A).
    C(u32),
}

/// A place in the source: the tokens and the outermost macro call (as in the graph).
#[derive(Serialize, Clone)]
pub struct IrStmt {
    pub k: IrStmtKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<[i64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<[i64; 3]>,
}

#[derive(Serialize, Clone)]
pub enum IrStmtKind {
    Set(IrPlace, IrRvalue),
    SetDisc(IrPlace, usize),
    U(String),
}

#[derive(Serialize, Clone)]
pub struct IrTerm {
    pub k: IrTermKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<[i64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<[i64; 3]>,
    /// [R69.1-2] An optional overflow check (the compiler: `is_optional_overflow_check`) of a
    /// body whose machine code an upstream crate builds: the driver does not know the settings
    /// of that crate, so the check does not know if the code generation keeps the check.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub o: bool,
}

/// [R69.1-1] The settings of the code generation of the session that builds the machine code of
/// a body (design 69, G5). The code generation reads them, not the MIR: it removes an optional
/// overflow check when the session has no overflow checks, it gives each runtime check operand
/// (`RuntimeChecks`) the value of the session, and with the panic strategy `abort` a panic runs
/// no cleanup path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gen {
    pub overflow_checks: bool,
    pub abort: bool,
}

impl Gen {
    /// The settings of the session of the compiler.
    pub fn of_session(tcx: rustc_middle::ty::TyCtxt<'_>) -> Gen {
        Gen { overflow_checks: tcx.sess.overflow_checks(), abort: !tcx.sess.panic_strategy().unwinds() }
    }

    /// The settings of the code of an instance: the session, or no settings if the compiler takes
    /// the machine code of the instance from an upstream crate (a shared generic instance): the
    /// driver does not know the settings of that crate.
    pub fn of_instance(instance: &rustc_public::mir::mono::Instance) -> Option<Gen> {
        rustc_middle::ty::tls::with(|tcx| {
            let internal = rustc_public::rustc_internal::internal(tcx, *instance);
            internal.upstream_monomorphization(tcx).is_none().then(|| Gen::of_session(tcx))
        })
    }
}

/// [R69.1-1] True if the code generation removes the check when the session has no overflow
/// checks: the predicate of the compiler on the kind of the check.
fn optional_overflow_check(msg: &AssertMessage) -> bool {
    use rustc_middle::mir::AssertKind;
    let internal: AssertKind<()> = match msg {
        AssertMessage::Overflow(op, ..) => {
            let op = rustc_middle::ty::tls::with(|tcx| rustc_public::rustc_internal::internal(tcx, *op));
            AssertKind::Overflow(op, (), ())
        }
        AssertMessage::OverflowNeg(_) => AssertKind::OverflowNeg(()),
        AssertMessage::DivisionByZero(_) => AssertKind::DivisionByZero(()),
        AssertMessage::RemainderByZero(_) => AssertKind::RemainderByZero(()),
        AssertMessage::BoundsCheck { .. } => AssertKind::BoundsCheck { len: (), index: () },
        // A kind with data of another type: the code generation always keeps it.
        _ => return false,
    };
    internal.is_optional_overflow_check()
}

/// [R69.1-1] The value of a runtime check operand in a session: the function of the compiler.
fn runtime_check(tcx: rustc_middle::ty::TyCtxt<'_>, kind: &rustc_public::mir::RuntimeChecks) -> bool {
    use rustc_middle::mir::RuntimeChecks as Internal;
    let internal = match kind {
        rustc_public::mir::RuntimeChecks::UbChecks => Internal::UbChecks,
        rustc_public::mir::RuntimeChecks::ContractChecks => Internal::ContractChecks,
        rustc_public::mir::RuntimeChecks::OverflowChecks => Internal::OverflowChecks,
    };
    internal.value(tcx.sess)
}

/// What a panic or an unwind on the cleanup path does.
#[derive(Serialize, Clone)]
pub enum IrUnwind {
    /// The places that the cleanup path drops, in order (no place: no drop).
    Drops(Vec<IrPlace>),
    /// The program stops (`abort`).
    Abort,
    /// The cleanup path has a part that the check does not model.
    U(String),
}

#[derive(Serialize, Clone)]
pub struct IrCallee {
    /// The callee text (R60.2-11).
    pub p: String,
    /// The name of the body of a function outside the standard library (for the inline step).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<String>,
    /// The name of an intrinsic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k: Option<String>,
    /// The mark of a call that the MIR does not resolve: `closure`, `pointer`, `dyn`, `param`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub m: Option<String>,
    /// True for an instance of a generic function: the form of the body in the record of the
    /// callee has the parameters, so the inline step takes the form of the instance (`qx`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub g: bool,
    /// The effect summary of the call (design 64, part B). No summary: the call reads and writes
    /// the world.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub e: Option<crate::effects::IrEff>,
    /// The operation of the sequence model of a callee of the standard library (design 64,
    /// part C): `vec_push`, `slice_get`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
    /// A call of a closure whose body the check knows (design 66, part A): 1 if the first
    /// argument goes to the body as it is, 2 if the call gives the closure by value to a body
    /// that takes a reference (`FnOnce::call_once` of an `Fn` or `FnMut` closure); 10 more for
    /// a closure of the standard library. The last argument is the tuple of the arguments of
    /// the body. `w` is the name of the body, `g` is true if the form is the form of an
    /// instance (by the key `p`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<u8>,
    /// The key of each closure type in the generic arguments of the callee, with the name of
    /// its body (design 66, part A): the check gives the identity of each closure to a call
    /// that it does not inline.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub q: Vec<(String, String)>,
    /// The function pointer of a call through a pointer (design 66, part A): an input of the
    /// call, and the closure that the check inlines if the value is a closure of the body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub f: Option<IrOp>,
    /// [R69.3-1] The call makes the message of a panic from its literal pieces: a `const fn`
    /// whose result is a `fmt::Arguments` (the lang item `format_arguments`) and whose each
    /// argument is a constant. A `const fn` with constant arguments has no effect, and its value
    /// is the list of the constant pieces.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub a: bool,
}

#[derive(Serialize, Clone)]
pub enum IrTermKind {
    Goto(usize),
    /// A switch: the operand, the value and the target of each arm, the target of `otherwise`.
    Sw(IrOp, Vec<(String, usize)>, usize),
    Ret,
    Unreach,
    /// A check: the condition, the expected value, the kind (`overflow add`), the target, and
    /// the cleanup path of the panic.
    Assert(IrOp, bool, String, usize, IrUnwind),
    /// A call: the callee, the arguments, the destination, the target (none: the callee never
    /// returns), and the cleanup path of an unwind.
    Call(IrCallee, Vec<IrOp>, IrPlace, Option<usize>, IrUnwind),
    /// A drop of a value with a destructor: the place, the target, the type text, and the cleanup
    /// path.
    Drop(IrPlace, usize, String, IrUnwind),
    U(String),
}

/// The builder of one form.
struct Builder<'a, 'tcx> {
    body: &'a Body,
    tcx: rustc_middle::ty::TyCtxt<'tcx>,
    typing: rustc_middle::ty::TypingEnv<'tcx>,
    types: Vec<IrTy>,
    ids: HashMap<Ty, u32>,
    files: Vec<String>,
    item: Option<rustc_public::DefId>,
    /// The settings of the code generation of the body (R69.1-1); `None`: not known.
    settings: Option<Gen>,
}

/// Builds the form of a body. `item` is the def of the body (for the typing environment). `settings`
/// is the settings of the session that builds the machine code of the body (R69.1-1): the form
/// is the MIR as the code generation reads it.
pub fn build(body: &Body, item: Option<rustc_public::DefId>, settings: Option<Gen>) -> Ir {
    rustc_middle::ty::tls::with(|tcx| {
        let local = item.map(|item| rustc_public::rustc_internal::internal(tcx, item));
        if let Some(def) = local
            && def.as_local().is_some_and(|local| tcx.coroutine_kind(local.to_def_id()).is_some())
        {
            return Ir { u: Some("a coroutine body".into()), ..Ir::default() };
        }
        if body.blocks.len() > MAX_BLOCKS {
            return Ir { u: Some(format!("a body with more than {MAX_BLOCKS} blocks")), ..Ir::default() };
        }
        let typing = match local {
            Some(def) => rustc_middle::ty::TypingEnv::post_analysis(tcx, def),
            None => rustc_middle::ty::TypingEnv::fully_monomorphized(),
        };
        let mut builder = Builder { body, tcx, typing, types: Vec::new(), ids: HashMap::new(), files: Vec::new(), item, settings };
        let locals: Vec<u32> = body.locals().iter().map(|decl| builder.ty(decl.ty, 0)).collect();
        let blocks = body.blocks.iter().map(|block| builder.block(block)).collect();
        let args = body.arg_locals().len();
        let mut names: Vec<String> = (1..=args).map(|index| format!("_{index}")).collect();
        for info in &body.var_debug_info {
            if let rustc_public::mir::VarDebugInfoContents::Place(place) = &info.value
                && place.projection.is_empty()
                && (1..=args).contains(&place.local)
                && info.composite.is_none()
            {
                names[place.local - 1] = info.name.clone();
            }
        }
        // The first parameter of a closure body is the state of its captures (design 61.12,
        // C1): the check compares a closure with its captures and its parameters as inputs.
        if local.is_some_and(|def| tcx.is_closure_like(def)) && names.first().is_some_and(|name| name == "_1") {
            names[0] = "captures".to_string();
        }
        Ir { a: args, l: locals, t: builder.types, b: blocks, f: builder.files, n: names, u: None }
    })
}

impl<'a, 'tcx> Builder<'a, 'tcx> {
    fn ty(&mut self, ty: Ty, depth: usize) -> u32 {
        if let Some(id) = self.ids.get(&ty) {
            return *id;
        }
        let id = self.types.len() as u32;
        self.ids.insert(ty, id);
        self.types.push(IrTy::Unit);
        let found = self.ty_kind(ty, depth);
        self.types[id as usize] = found;
        id
    }

    fn opaque(&self, ty: Ty) -> IrTy {
        let internal = rustc_public::rustc_internal::internal(self.tcx, ty);
        let drop = crate::safe::needs_drop(self.tcx, self.typing, internal);
        IrTy::Opaque { n: format!("{internal}"), d: drop }
    }

    fn ty_kind(&mut self, ty: Ty, depth: usize) -> IrTy {
        if depth > 6 || self.types.len() > MAX_TYPES {
            return self.opaque(ty);
        }
        match ty.kind() {
            TyKind::RigidTy(RigidTy::Bool) => IrTy::Bool,
            TyKind::RigidTy(RigidTy::Char) => IrTy::Char,
            TyKind::RigidTy(RigidTy::Never) => IrTy::Never,
            TyKind::RigidTy(RigidTy::Uint(_) | RigidTy::Int(_)) => {
                let (w, s) = crate::consts::int_of(ty).unwrap_or((64, false));
                IrTy::Int(w, s)
            }
            TyKind::RigidTy(RigidTy::Tuple(items)) if items.is_empty() => IrTy::Unit,
            TyKind::RigidTy(RigidTy::Tuple(items)) => IrTy::Tuple(items.iter().map(|item| self.ty(*item, depth + 1)).collect()),
            TyKind::RigidTy(RigidTy::Ref(_, inner, mutability)) => IrTy::Ref(matches!(mutability, rustc_public::mir::Mutability::Mut), self.ty(inner, depth + 1)),
            // A sequence of scalars (design 64, part C).
            TyKind::RigidTy(RigidTy::Slice(item)) if crate::effects::is_scalar(item) => IrTy::Seq { e: self.ty(item, depth + 1), n: None, k: "s".into() },
            TyKind::RigidTy(RigidTy::Array(item, length)) if crate::effects::is_scalar(item) && length.eval_target_usize().is_ok() => {
                IrTy::Seq { e: self.ty(item, depth + 1), n: length.eval_target_usize().ok(), k: "a".into() }
            }
            TyKind::RigidTy(RigidTy::Adt(def, args)) if crate::effects::scalar_vec(def, &args).is_some() => {
                let item = crate::effects::scalar_vec(def, &args).unwrap();
                IrTy::Seq { e: self.ty(item, depth + 1), n: None, k: "v".into() }
            }
            // A closure (design 66, part A): the captures, and the name and the key of its body.
            TyKind::RigidTy(RigidTy::Closure(..)) => match closure_info(ty) {
                Some(info) if info.known => {
                    let v = info.captures.iter().map(|capture| self.ty(*capture, depth + 1)).collect();
                    let internal = rustc_public::rustc_internal::internal(self.tcx, ty);
                    let d = crate::safe::needs_drop(self.tcx, self.typing, internal);
                    let s = info.std;
                    let q = info.inner.clone();
                    IrTy::Clo { n: info.name, v, g: info.instance.then_some(info.key), d, s, q }
                }
                _ => self.opaque(ty),
            },
            TyKind::RigidTy(RigidTy::Adt(def, args)) if self.modeled(def) => {
                let enum_like = def.kind() == AdtKind::Enum;
                let mut variants = Vec::new();
                let mut discriminants = Vec::new();
                let mut discr_ty = None;
                let mut names = Vec::new();
                let mut field_names = Vec::new();
                for variant in def.variants_iter() {
                    field_names.push(variant.fields().iter().map(|field| field.name.to_string()).collect::<Vec<String>>());
                    // A field type with a projection that the environment cannot normalize
                    // makes the type opaque (design 61.9, part 0).
                    let Some(types) = (0..variant.fields().len()).map(|field_index| crate::safe::field_ty(self.tcx, self.typing, def, variant.idx(), field_index, &args)).collect::<Option<Vec<_>>>() else {
                        return self.opaque(ty);
                    };
                    let fields: Vec<u32> = types.into_iter().map(|field| self.ty(field, depth + 1)).collect();
                    variants.push(fields);
                    if enum_like {
                        names.push(variant.name());
                        let discr = def.discriminant_for_variant(variant.idx());
                        discriminants.push(discr.val.to_string());
                        discr_ty = Some(discr.ty);
                    }
                }
                let dt = discr_ty.map(|ty| self.ty(ty, depth + 1));
                IrTy::Adt { n: format!("{}", rustc_public::rustc_internal::internal(self.tcx, ty)), v: variants, vn: names, d: discriminants, dt, e: enum_like, fs: field_names }
            }
            _ => self.opaque(ty),
        }
    }

    /// True for an ADT with a model: a struct or an enum outside the standard library, and
    /// `Option`, `Result`, `Ordering`, `ControlFlow`, and `Range` (design 61.11: the state of a
    /// `for` loop over a range).
    fn modeled(&self, def: rustc_public::ty::AdtDef) -> bool {
        if def.kind() == AdtKind::Union {
            return false;
        }
        let name = def.name();
        if matches!(name.as_str(), "core::option::Option" | "std::option::Option" | "core::result::Result" | "std::result::Result" | "core::cmp::Ordering" | "std::cmp::Ordering" | "core::ops::ControlFlow" | "std::ops::ControlFlow" | "core::ops::Range" | "std::ops::Range") {
            return true;
        }
        let krate = def.krate().name;
        !crate::stdlib::is_std_name(krate.as_str())
    }

    fn place_of(&mut self, span: rustc_public::ty::Span) -> (Option<[i64; 3]>, Option<[i64; 3]>) {
        let span = rustc_public::rustc_internal::internal(self.tcx, span);
        let (tokens, call) = crate::graph::place(self.tcx, span, &mut self.files);
        (tokens.map(crate::graph::array), call.map(crate::graph::array))
    }

    fn block(&mut self, block: &rustc_public::mir::BasicBlock) -> IrBlock {
        let mut statements = Vec::new();
        for statement in &block.statements {
            let kind = match &statement.kind {
                StatementKind::Assign(place, value) => IrStmtKind::Set(self.place(place), self.rvalue(value)),
                StatementKind::SetDiscriminant { place, variant_index } => IrStmtKind::SetDisc(self.place(place), variant_index.to_index()),
                StatementKind::Intrinsic(_) => IrStmtKind::U("an intrinsic statement".into()),
                _ => continue,
            };
            let (r, c) = self.place_of(statement.source_info.span);
            statements.push(IrStmt { k: kind, r, c });
        }
        let (r, c) = self.place_of(block.terminator.source_info.span);
        // [R69.1-2] An optional overflow check whose settings the driver does not know.
        let o = self.settings.is_none() && matches!(&block.terminator.kind, TerminatorKind::Assert { msg, .. } if optional_overflow_check(msg));
        IrBlock { s: statements, t: IrTerm { k: self.terminator(&block.terminator.kind), r, c, o } }
    }

    fn place(&mut self, place: &Place) -> IrPlace {
        let p = place
            .projection
            .iter()
            .map(|projection| match projection {
                ProjectionElem::Deref => IrProj::D,
                ProjectionElem::Field(index, _) => IrProj::F(*index),
                ProjectionElem::Downcast(variant) => IrProj::V(variant.to_index()),
                ProjectionElem::Index(local) => IrProj::I(*local),
                ProjectionElem::ConstantIndex { offset, from_end, .. } => IrProj::K(*offset, *from_end),
                ProjectionElem::Subslice { .. } => IrProj::U("a subslice".into()),
                ProjectionElem::OpaqueCast(_) => IrProj::U("an opaque cast".into()),
            })
            .collect();
        IrPlace { l: place.local, p }
    }

    fn operand(&mut self, operand: &Operand) -> IrOp {
        match operand {
            Operand::Copy(place) => IrOp::C(self.place(place)),
            Operand::Move(place) => IrOp::M(self.place(place)),
            Operand::Constant(constant) => IrOp::K(self.constant(&constant.const_)),
            // [R69.1-1] A runtime check has the value of the session that builds the code.
            Operand::RuntimeChecks(kind) => match self.settings {
                Some(_) => IrOp::K(IrConst::I(u8::from(runtime_check(self.tcx, kind)).to_string(), self.ty(Ty::bool_ty(), 0))),
                None => IrOp::R(format!("{kind:?}")),
            },
        }
    }

    fn constant(&mut self, constant: &rustc_public::ty::MirConst) -> IrConst {
        let ty = constant.ty();
        if let Some(crate::consts::Val::Bits { v, .. }) = crate::consts::const_bits(constant, self.item) {
            let id = self.ty(ty, 0);
            return IrConst::I(v.to_string(), id);
        }
        if let Some(found) = self.generic_constant(constant) {
            return found;
        }
        if crate::consts::int_of(ty).is_some() {
            return IrConst::U("a constant that the compiler does not evaluate here".into());
        }
        if let TyKind::RigidTy(RigidTy::FnDef(..)) = ty.kind() {
            let text = crate::print::Printer::default().ty(ty);
            return IrConst::Fn(text);
        }
        if let Some(found) = self.adt_constant(constant) {
            return found;
        }
        if let Some(found) = self.array_constant(constant) {
            return found;
        }
        let id = self.ty(ty, 0);
        match constant.kind() {
            ConstantKind::ZeroSized => IrConst::Z(id),
            ConstantKind::Allocated(_) => {
                let text = crate::print::Printer::for_body(self.body).mir_const(constant);
                // A reference in the value (a `&str`, a slice) is printed with its target.
                IrConst::O(text, id, true)
            }
            ConstantKind::Unevaluated(_) => {
                let text = crate::print::Printer::for_body(self.body).mir_const(constant);
                if text.contains("unevaluated") || text.contains("promoted") || text.contains("param ") {
                    IrConst::U("a constant that the compiler does not evaluate here".into())
                } else {
                    IrConst::O(text, id, true)
                }
            }
            _ => IrConst::U("a constant that the compiler does not evaluate here".into()),
        }
    }

    /// A constant that the compiler does not evaluate here (design 66, part B): an unevaluated
    /// constant (with a parameter in its arguments, or a promoted constant of a generic body),
    /// or a const parameter. No result for a constant that the compiler evaluates.
    fn generic_constant(&mut self, constant: &rustc_public::ty::MirConst) -> Option<IrConst> {
        let ty = constant.ty();
        match constant.kind() {
            ConstantKind::Unevaluated(unevaluated) => {
                let evaluated = rustc_middle::ty::tls::with(|tcx| {
                    let internal = rustc_public::rustc_internal::internal(tcx, constant);
                    if rustc_middle::ty::TypeVisitableExt::has_param(&internal) || rustc_middle::ty::TypeVisitableExt::has_aliases(&internal) {
                        return false;
                    }
                    let typing = match self.item.map(|item| rustc_public::rustc_internal::internal(tcx, item)) {
                        Some(def) => rustc_middle::ty::TypingEnv::post_analysis(tcx, def),
                        None => rustc_middle::ty::TypingEnv::fully_monomorphized(),
                    };
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| internal.eval(tcx, typing, rustc_span::DUMMY_SP).is_ok())).unwrap_or(false)
                });
                if evaluated {
                    return None;
                }
                let def = rustc_public::CrateDef::def_id(&unevaluated.def);
                let digest = crate::guarded(|| crate::consts::definition_digest(def, unevaluated.promoted.map(|index| index as u32)))?;
                let args = crate::print::Printer::for_body(self.body).generic_args(&unevaluated.args);
                let path = match unevaluated.promoted {
                    Some(index) => format!("{}::promoted[{index}]{args}", unevaluated.def.name()),
                    None => format!("{}{args}", unevaluated.def.name()),
                };
                let id = self.ty(ty, 0);
                Some(IrConst::G(path, id, digest))
            }
            ConstantKind::Param(param) => {
                let id = self.ty(ty, 0);
                Some(IrConst::G(format!("const parameter {}#{}", param.name, param.index), id, Vec::new()))
            }
            ConstantKind::Ty(ty_const) => match ty_const.kind() {
                rustc_public::ty::TyConstKind::Param(param) => {
                    let id = self.ty(ty, 0);
                    Some(IrConst::G(format!("const parameter {}#{}", param.name, param.index), id, Vec::new()))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// A constant reference to an array of scalars (`&[1, 2]`, design 64, part C): the bits of
    /// each element, from the bytes of the allocation that the reference points to.
    fn array_constant(&mut self, constant: &rustc_public::ty::MirConst) -> Option<IrConst> {
        let TyKind::RigidTy(RigidTy::Ref(_, inner, _)) = constant.ty().kind() else { return None };
        let TyKind::RigidTy(RigidTy::Array(item, length)) = inner.kind() else { return None };
        if !crate::effects::is_scalar(item) {
            return None;
        }
        let length = length.eval_target_usize().ok()? as usize;
        let ConstantKind::Allocated(alloc) = constant.kind() else { return None };
        let [(0, provenance)] = alloc.provenance.ptrs.as_slice() else { return None };
        let rustc_public::mir::alloc::GlobalAlloc::Memory(target) = rustc_public::mir::alloc::GlobalAlloc::from(provenance.0) else { return None };
        let (width, _) = match item.kind() {
            TyKind::RigidTy(RigidTy::Bool) => (8, false),
            TyKind::RigidTy(RigidTy::Char) => (32, false),
            _ => crate::consts::int_of(item)?,
        };
        let size = (width as usize).div_ceil(8);
        if target.bytes.len() != size * length || !target.provenance.ptrs.is_empty() || length > 64 {
            return None;
        }
        let little = self.tcx.data_layout.endian == rustc_abi::Endian::Little;
        let mut values = Vec::new();
        for chunk in target.bytes.chunks(size) {
            let bytes: Vec<u8> = chunk.iter().copied().collect::<Option<Vec<u8>>>()?;
            let mut value: u128 = 0;
            for (index, byte) in bytes.iter().enumerate() {
                let shift = if little { index } else { size - 1 - index };
                value |= u128::from(*byte) << (8 * shift);
            }
            values.push(value.to_string());
        }
        let id = self.ty(item, 0);
        Some(IrConst::S(id, values))
    }

    /// The value of a constant of a modeled ADT whose fields are scalars, from the evaluated
    /// constant (design 61.12, C4). No value for a type with a parameter or a projection, or a
    /// field that is not a scalar.
    fn adt_constant(&mut self, constant: &rustc_public::ty::MirConst) -> Option<IrConst> {
        let TyKind::RigidTy(RigidTy::Adt(def, _)) = constant.ty().kind() else { return None };
        if !self.modeled(def) || matches!(constant.kind(), ConstantKind::ZeroSized) {
            return None;
        }
        let tcx = self.tcx;
        let internal = rustc_public::rustc_internal::internal(tcx, constant);
        let ty = internal.ty();
        if rustc_middle::ty::TypeVisitableExt::has_param(&ty) || rustc_middle::ty::TypeVisitableExt::has_aliases(&ty) {
            return None;
        }
        let value = internal.eval(tcx, self.typing, rustc_span::DUMMY_SP).ok()?;
        let found = tcx.try_destructure_mir_constant_for_user_output(value, ty)?;
        let variant = found.variant.map(|index| index.as_usize()).unwrap_or(0);
        let mut fields = Vec::new();
        for (field, field_ty) in found.fields {
            let scalar = matches!(field_ty.kind(), rustc_middle::ty::Int(_) | rustc_middle::ty::Uint(_) | rustc_middle::ty::Bool | rustc_middle::ty::Char);
            if !scalar {
                return None;
            }
            let int = field.try_to_scalar_int()?;
            let bits = int.to_bits_unchecked();
            let id = self.ty(rustc_public::rustc_internal::stable(*field_ty), 0);
            fields.push((bits.to_string(), id));
        }
        let id = self.ty(constant.ty(), 0);
        Some(IrConst::A(id, variant, fields))
    }

    fn rvalue(&mut self, rvalue: &Rvalue) -> IrRvalue {
        match rvalue {
            Rvalue::Use(operand, _) => IrRvalue::Use(self.operand(operand)),
            Rvalue::CopyForDeref(place) => IrRvalue::Use(IrOp::C(self.place(place))),
            Rvalue::BinaryOp(op, left, right) => IrRvalue::Bin(bin_name(*op), self.operand(left), self.operand(right)),
            Rvalue::CheckedBinaryOp(op, left, right) => IrRvalue::Chk(bin_name(*op), self.operand(left), self.operand(right)),
            Rvalue::UnaryOp(op, value) => match op {
                UnOp::Not => IrRvalue::Un("not".into(), self.operand(value)),
                UnOp::Neg => IrRvalue::Un("neg".into(), self.operand(value)),
                // The length of a slice (design 64, part C): the check knows it for a sequence.
                UnOp::PtrMetadata => IrRvalue::Un("meta".into(), self.operand(value)),
            },
            Rvalue::Cast(CastKind::IntToInt, value, to) => {
                let to = self.ty(*to, 0);
                IrRvalue::Cast(self.operand(value), to)
            }
            // An array as a slice (design 64, part C): the same sequence.
            Rvalue::Cast(CastKind::PointerCoercion(rustc_public::mir::PointerCoercion::Unsize), value, to) if crate::effects::scalar_slice_ref(*to) => IrRvalue::Use(self.operand(value)),
            // A function pointer of a closure with no capture or of a function item (design 66,
            // part A): the value of the closure or of the item. The check does not compare two
            // function pointers exactly (one address can have two functions).
            Rvalue::Cast(CastKind::PointerCoercion(rustc_public::mir::PointerCoercion::ClosureFnPointer(_) | rustc_public::mir::PointerCoercion::ReifyFnPointer(_)), value, _) => IrRvalue::Un("fnptr".into(), self.operand(value)),
            Rvalue::Cast(kind, _, _) => IrRvalue::U(format!("a cast ({})", crate::print::cast_kind_text(kind))),
            Rvalue::Aggregate(kind, operands) => match kind {
                AggregateKind::Tuple => IrRvalue::Agg(IrAgg::T, operands.iter().map(|operand| self.operand(operand)).collect()),
                AggregateKind::Adt(def, variant, args, _, None) if self.modeled(*def) => {
                    let Some(ty) = crate::safe::def_ty_with_args(self.tcx, self.typing, def.def_id(), args) else {
                        return IrRvalue::U("a value of a type that the compiler does not normalize here".into());
                    };
                    let ty = self.ty(ty, 0);
                    IrRvalue::Agg(IrAgg::A(ty, variant.to_index()), operands.iter().map(|operand| self.operand(operand)).collect())
                }
                AggregateKind::Adt(..) => IrRvalue::U("a value of a type that the check does not model".into()),
                // A closure is its captures and the identity of its body (design 66, part A).
                AggregateKind::Closure(..) => match rvalue.ty(self.body.locals()) {
                    Ok(ty) => {
                        let id = self.ty(ty, 0);
                        match self.types.get(id as usize) {
                            Some(IrTy::Clo { .. }) => IrRvalue::Agg(IrAgg::C(id), operands.iter().map(|operand| self.operand(operand)).collect()),
                            _ => IrRvalue::U("a closure value (a closure of a generic item that the check does not know here)".into()),
                        }
                    }
                    Err(_) => IrRvalue::U("a closure value".into()),
                },
                AggregateKind::CoroutineClosure(..) => IrRvalue::U("a closure value (an async closure)".into()),
                AggregateKind::Coroutine(..) => IrRvalue::U("a coroutine value".into()),
                AggregateKind::Array(item) if crate::effects::is_scalar(*item) => {
                    let item = self.ty(*item, 0);
                    IrRvalue::Agg(IrAgg::S(item), operands.iter().map(|operand| self.operand(operand)).collect())
                }
                AggregateKind::Array(_) => IrRvalue::U("an array".into()),
                AggregateKind::RawPtr(..) => IrRvalue::U("a raw pointer".into()),
            },
            Rvalue::Ref(_, kind, place) => IrRvalue::Ref(matches!(kind, BorrowKind::Mut { .. }), self.place(place)),
            Rvalue::Reborrow(_, mutability, place) => IrRvalue::Ref(matches!(mutability, rustc_public::mir::Mutability::Mut), self.place(place)),
            Rvalue::Discriminant(place) => IrRvalue::Disc(self.place(place)),
            Rvalue::AddressOf(..) => IrRvalue::U("a raw pointer".into()),
            Rvalue::Len(place) => IrRvalue::Un("len".into(), IrOp::C(self.place(place))),
            // An array of one value (design 64, part C), up to 64 elements.
            Rvalue::Repeat(operand, count) => match (operand.ty(self.body.locals()), count.eval_target_usize()) {
                (Ok(item), Ok(count)) if crate::effects::is_scalar(item) && count <= 64 => {
                    let item = self.ty(item, 0);
                    IrRvalue::Agg(IrAgg::S(item), vec![self.operand(operand); count as usize])
                }
                _ => IrRvalue::U("an array".into()),
            },
            Rvalue::ThreadLocalRef(_) => IrRvalue::U("a thread local".into()),
        }
    }

    /// The cleanup path of an unwind action: the drops in order up to `resume`.
    fn unwind(&mut self, action: Option<&UnwindAction>) -> IrUnwind {
        // [R69.1-1] With the panic strategy `abort`, a panic stops the process: no cleanup path
        // runs.
        if self.settings.is_some_and(|settings| settings.abort) && matches!(action, Some(UnwindAction::Cleanup(_))) {
            return IrUnwind::Drops(Vec::new());
        }
        match action {
            None | Some(UnwindAction::Continue) => IrUnwind::Drops(Vec::new()),
            Some(UnwindAction::Unreachable) => IrUnwind::Drops(Vec::new()),
            Some(UnwindAction::Terminate) => IrUnwind::Abort,
            Some(UnwindAction::Cleanup(start)) => {
                let mut drops = Vec::new();
                let mut block = *start;
                for _ in 0..self.body.blocks.len() + 1 {
                    let current = &self.body.blocks[block];
                    if current.statements.iter().any(|statement| !matches!(statement.kind, StatementKind::StorageDead(_) | StatementKind::StorageLive(_) | StatementKind::Nop)) {
                        return IrUnwind::U("a cleanup path with a statement".into());
                    }
                    match &current.terminator.kind {
                        TerminatorKind::Resume => return IrUnwind::Drops(drops),
                        TerminatorKind::Abort => return IrUnwind::Abort,
                        TerminatorKind::Goto { target } => block = *target,
                        TerminatorKind::Drop { place, target, .. } => {
                            if self.needs_drop(place) {
                                drops.push(self.place(place));
                            }
                            block = *target;
                        }
                        _ => return IrUnwind::U("a cleanup path with a call".into()),
                    }
                }
                IrUnwind::U("a cleanup path with a cycle".into())
            }
        }
    }

    fn needs_drop(&self, place: &Place) -> bool {
        let Ok(ty) = place.ty(self.body.locals()) else { return true };
        let internal = rustc_public::rustc_internal::internal(self.tcx, ty);
        crate::safe::needs_drop(self.tcx, self.typing, internal)
    }

    fn terminator(&mut self, kind: &TerminatorKind) -> IrTermKind {
        match kind {
            TerminatorKind::Goto { target } => IrTermKind::Goto(*target),
            TerminatorKind::SwitchInt { discr, targets } => {
                let arms = targets.branches().map(|(value, target)| (value.to_string(), target)).collect();
                IrTermKind::Sw(self.operand(discr), arms, targets.otherwise())
            }
            TerminatorKind::Return => IrTermKind::Ret,
            TerminatorKind::Unreachable => IrTermKind::Unreach,
            TerminatorKind::Resume => IrTermKind::U("a resume".into()),
            TerminatorKind::Abort => IrTermKind::U("an abort".into()),
            TerminatorKind::Drop { place, target, unwind } => {
                if !self.needs_drop(place) {
                    return IrTermKind::Goto(*target);
                }
                let text = place.ty(self.body.locals()).map(|ty| format!("{}", rustc_public::rustc_internal::internal(self.tcx, ty))).unwrap_or_default();
                let uw = self.unwind(Some(unwind));
                IrTermKind::Drop(self.place(place), *target, text, uw)
            }
            TerminatorKind::Assert { cond, expected, msg, target, unwind } => {
                // [R69.1-1] The code generation of a session with no overflow checks removes an
                // optional overflow check (also in the code of another crate that the session
                // builds, such as a `#[rustc_inherit_overflow_checks]` helper of `core`).
                if self.settings.is_some_and(|settings| !settings.overflow_checks) && optional_overflow_check(msg) {
                    return IrTermKind::Goto(*target);
                }
                let uw = self.unwind(Some(unwind));
                IrTermKind::Assert(self.operand(cond), *expected, assert_name(msg), *target, uw)
            }
            TerminatorKind::Call { func, args, destination, target, unwind } => {
                if crate::is_opaque_wrap(func) {
                    // The opaque wrap of the driver (`black_box`) is the identity.
                    let value = args.first().map(|arg| self.operand(arg));
                    return match (value, target) {
                        (Some(value), Some(target)) => IrTermKind::Call(IrCallee { p: "black_box".into(), w: None, k: Some("black_box".into()), m: None, g: false, e: Some(crate::effects::IrEff::default()), s: None, c: None, q: Vec::new(), f: None, a: false }, vec![value], self.place(destination), Some(*target), IrUnwind::Drops(Vec::new())),
                        _ => IrTermKind::U("an opaque wrap".into()),
                    };
                }
                let mut callee = self.callee(func, self.span_of(kind));
                callee.a = crate::guarded(|| message_call(self.body, func, args, destination)).unwrap_or(false);
                callee.e = crate::guarded(|| crate::effects::of_call(self.body, func, args));
                callee.s = crate::guarded(|| crate::effects::seq_model(func, self.body)).flatten();
                callee.q = crate::guarded(|| closure_keys(func, self.body)).unwrap_or_default();
                if callee.m.as_deref() == Some("pointer") {
                    callee.f = Some(self.operand(func));
                }
                let uw = self.unwind(Some(unwind));
                let args = args.iter().map(|arg| self.operand(arg)).collect();
                IrTermKind::Call(callee, args, self.place(destination), *target, uw)
            }
            TerminatorKind::InlineAsm { .. } => IrTermKind::U("inline assembly".into()),
        }
    }

    /// The span of a terminator of the body.
    fn span_of(&self, kind: &TerminatorKind) -> rustc_span::Span {
        let found = self.body.blocks.iter().find(|block| std::ptr::eq(&block.terminator.kind, kind)).map(|block| block.terminator.source_info.span).unwrap_or(self.body.span);
        rustc_public::rustc_internal::internal(self.tcx, found)
    }

    fn callee(&mut self, func: &Operand, span: rustc_span::Span) -> IrCallee {
        let typing = self.item.and_then(|item| rustc_public::rustc_internal::internal(self.tcx, item).as_local()).map(|local| rustc_middle::ty::TypingEnv::post_analysis(self.tcx, local.to_def_id()));
        let (text, mark) = crate::graph::callee_text(self.tcx, typing.or(Some(self.typing)), self.body, func, span);
        let mut callee = IrCallee { p: text, w: None, k: None, m: mark.map(str::to_string), g: false, e: None, s: None, c: None, q: Vec::new(), f: None, a: false };
        let Ok(ty) = func.ty(self.body.locals()) else { return callee };
        let TyKind::RigidTy(RigidTy::FnDef(def, args)) = ty.kind() else { return callee };
        // A call of a closure whose body the check knows (design 66, part A).
        if mark == Some("closure")
            && let Some((info, mode)) = crate::guarded(|| closure_call(self.body, func)).flatten()
            && info.known
        {
            callee.p = info.key.clone();
            callee.w = Some(info.name);
            callee.g = info.instance;
            let in_std = info.std;
            callee.c = Some(if in_std { mode + 10 } else { mode });
            return callee;
        }
        if mark.is_some() {
            return callee;
        }
        if !concrete(&args) {
            // A call that stays generic (a trait method on a type parameter).
            if args.0.iter().any(|arg| matches!(arg, GenericArgKind::Type(ty) if matches!(ty.kind(), TyKind::Param(_)))) {
                callee.m = Some("param".into());
            }
            return callee;
        }
        match rustc_public::mir::mono::Instance::resolve(def, &args) {
            Ok(instance) => {
                callee.k = instance.intrinsic_name();
                if callee.k.is_none() {
                    let name = instance.def.name();
                    let krate = instance.def.krate().name;
                    if !crate::stdlib::is_std_name(krate.as_str()) {
                        callee.w = Some(name);
                        callee.g = !instance.args().0.is_empty();
                    }
                }
            }
            Err(_) => {
                // A call that stays generic (a trait method on a type parameter).
                if args.0.iter().any(|arg| matches!(arg, GenericArgKind::Type(ty) if matches!(ty.kind(), TyKind::Param(_)))) {
                    callee.m = Some("param".into());
                }
            }
        }
        callee
    }
}

/// [R69.3-1] True for a call that makes the message of a panic from its literal pieces: the
/// callee is a `const fn` (the compiler: `is_const_fn`), the result has the type of the lang item
/// `format_arguments`, and each argument is a constant. No name decides it.
fn message_call(body: &Body, func: &Operand, args: &[Operand], destination: &Place) -> bool {
    if args.is_empty() || !args.iter().all(|arg| matches!(arg, Operand::Constant(_))) {
        return false;
    }
    let Ok(ty) = func.ty(body.locals()) else { return false };
    let TyKind::RigidTy(RigidTy::FnDef(def, _)) = ty.kind() else { return false };
    let Ok(result) = destination.ty(body.locals()) else { return false };
    rustc_middle::ty::tls::with(|tcx| {
        let def = rustc_public::rustc_internal::internal(tcx, def.def_id());
        let result = rustc_public::rustc_internal::internal(tcx, result);
        let arguments = tcx.lang_items().format_arguments();
        tcx.is_const_fn(def) && result.ty_adt_def().is_some_and(|adt| Some(adt.did()) == arguments)
    })
}

/// True if generic arguments have no parameter and no bound variable: only then can the driver
/// resolve an instance with them (a resolve with a parameter panics in the compiler, and a panic
/// in a query of the compiler is not safe to catch).
pub fn concrete(args: &rustc_public::ty::GenericArgs) -> bool {
    rustc_middle::ty::tls::with(|tcx| {
        let internal = rustc_public::rustc_internal::internal(tcx, args);
        !rustc_middle::ty::TypeVisitableExt::has_param(&internal) && !rustc_middle::ty::TypeVisitableExt::has_escaping_bound_vars(&internal) && !rustc_middle::ty::TypeVisitableExt::has_aliases(&internal)
    })
}

fn bin_name(op: BinOp) -> String {
    match op {
        BinOp::Add => "add",
        BinOp::AddUnchecked => "add unchecked",
        BinOp::Sub => "sub",
        BinOp::SubUnchecked => "sub unchecked",
        BinOp::Mul => "mul",
        BinOp::MulUnchecked => "mul unchecked",
        BinOp::Div => "div",
        BinOp::Rem => "rem",
        BinOp::BitXor => "xor",
        BinOp::BitAnd => "and",
        BinOp::BitOr => "or",
        BinOp::Shl => "shl",
        BinOp::ShlUnchecked => "shl unchecked",
        BinOp::Shr => "shr",
        BinOp::ShrUnchecked => "shr unchecked",
        BinOp::Eq => "eq",
        BinOp::Lt => "lt",
        BinOp::Le => "le",
        BinOp::Ne => "ne",
        BinOp::Ge => "ge",
        BinOp::Gt => "gt",
        BinOp::Cmp => "cmp",
        BinOp::Offset => "offset",
    }
    .to_string()
}

/// The kind of a check: the kind of its panic.
fn assert_name(msg: &AssertMessage) -> String {
    match msg {
        AssertMessage::Overflow(op, ..) => format!("overflow {}", bin_name(*op)),
        AssertMessage::OverflowNeg(_) => "overflow neg".into(),
        AssertMessage::DivisionByZero(_) => "division by zero".into(),
        AssertMessage::RemainderByZero(_) => "remainder by zero".into(),
        AssertMessage::BoundsCheck { .. } => "bounds".into(),
        other => format!("check {}", other.description().unwrap_or("of an other kind")),
    }
}

/// The forms of the callees of the standard library that a body calls: each callee with a body
/// of at most [`MAX_STD_BLOCKS`] blocks and no generic parameter, to depth 2 (the inline step of
/// the check, design 61, C3). Also the form of each instance of a generic function of the
/// workspace that the body calls (with the arguments of the instance, design 61, part B).
pub fn std_callees(ir: &Ir, body: &Body) -> BTreeMap<String, Ir> {
    let mut found: BTreeMap<String, Ir> = BTreeMap::new();
    let mut todo: Vec<(Body, usize)> = vec![(body.clone(), 0)];
    let _ = ir;
    while let Some((current, depth)) = todo.pop() {
        for block in &current.blocks {
            let TerminatorKind::Call { func, .. } = &block.terminator.kind else { continue };
            // A call of a closure of an instance: the form of the closure body with the
            // arguments of the instance, by the key of the closure (design 66, part A).
            if let Some((info, _)) = crate::guarded(|| closure_call(&current, func)).flatten() {
                if info.instance
                    && !found.contains_key(&info.key)
                    && let Some(callee) = crate::guarded(|| closure_body(&info)).flatten()
                {
                    let in_std = info.std;
                    if callee.blocks.len() <= if in_std { MAX_STD_BLOCKS } else { MAX_BLOCKS } {
                        found.insert(info.key.clone(), build(&callee, None, rustc_middle::ty::tls::with(|tcx| Some(Gen::of_session(tcx)))));
                        if depth < 1 {
                            todo.push((callee, depth + 1));
                        }
                    }
                }
                continue;
            }
            let Ok(ty) = func.ty(current.locals()) else { continue };
            let TyKind::RigidTy(RigidTy::FnDef(def, args)) = ty.kind() else { continue };
            if !concrete(&args) {
                continue;
            }
            let Ok(instance) = rustc_public::mir::mono::Instance::resolve(def, &args) else { continue };
            let in_std = crate::stdlib::is_std_name(instance.def.krate().name.as_str());
            if instance.intrinsic_name().is_some() || (!in_std && instance.args().0.is_empty()) {
                continue;
            }
            let text = rustc_middle::ty::tls::with(|tcx| {
                let span = rustc_public::rustc_internal::internal(tcx, current.span);
                crate::graph::callee_text(tcx, Some(rustc_middle::ty::TypingEnv::fully_monomorphized()), &current, func, span).0
            });
            if found.contains_key(&text) {
                continue;
            }
            let Some(callee) = crate::safe::instance_body(&instance) else { continue };
            if callee.blocks.len() > if in_std { MAX_STD_BLOCKS } else { MAX_BLOCKS } {
                continue;
            }
            let form = build(&callee, None, Gen::of_instance(&instance));
            found.insert(text, form);
            if depth < 1 {
                todo.push((callee, depth + 1));
            }
        }
    }
    found
}

/// The values of the constant items and the associated constants that a body reads (design 61,
/// A2): the path and the value, from the body with its generic parameters (an instance body has
/// the values in place of the paths).
pub fn const_values(item: rustc_public::CrateItem) -> Vec<(String, String)> {
    let Some(body) = item.body() else { return Vec::new() };
    let mut found: Vec<(String, String)> = Vec::new();
    let mut add = |constant: &rustc_public::ty::MirConst| {
        // R68.6-1: a read of a static that cannot change (a lookup table) reads a constant. The
        // operand is a pointer to the static. A static of the standard library is not a part of
        // a step.
        if let ConstantKind::Allocated(alloc) = constant.kind() {
            for (_, provenance) in &alloc.provenance.ptrs {
                if let rustc_public::mir::alloc::GlobalAlloc::Static(def) = rustc_public::mir::alloc::GlobalAlloc::from(provenance.0)
                    && crate::effects::plain_static(&def)
                    && !crate::reach::is_std(&def.krate().name)
                {
                    let entry = (def.name(), crate::consts::static_value(&def));
                    if !found.contains(&entry) {
                        found.push(entry);
                    }
                }
            }
            return;
        }
        let ConstantKind::Unevaluated(unevaluated) = constant.kind() else { return };
        if unevaluated.promoted.is_some() {
            return;
        }
        let Some(crate::consts::Val::Bits { v, w, s }) = crate::consts::const_bits(constant, Some(item.def_id())) else {
            // A constant that the compiler does not evaluate here (design 66, part B): the digest
            // of its definition is its value for the compare of the two records, so an equal
            // text with a changed definition is no proof (R64.5-2).
            let generic = rustc_middle::ty::tls::with(|tcx| {
                let internal = rustc_public::rustc_internal::internal(tcx, constant);
                rustc_middle::ty::TypeVisitableExt::has_param(&internal) || rustc_middle::ty::TypeVisitableExt::has_aliases(&internal)
            });
            if generic && let Some(digest) = crate::guarded(|| crate::consts::definition_digest(rustc_public::CrateDef::def_id(&unevaluated.def), None)) {
                for entry in crate::consts::digest_values(&unevaluated.def.name(), &digest) {
                    if !found.contains(&entry) {
                        found.push(entry);
                    }
                }
            }
            return;
        };
        let value = if s && w < 128 { (((v << (128 - w)) as i128) >> (128 - w)).to_string() } else if w == 1 { (v == 1).to_string() } else { v.to_string() };
        let entry = (unevaluated.def.name(), value);
        if !found.contains(&entry) {
            found.push(entry);
        }
    };
    for block in &body.blocks {
        for statement in &block.statements {
            if let StatementKind::Assign(_, value) = &statement.kind {
                let operands: Vec<&Operand> = match value {
                    Rvalue::Use(operand, _) | Rvalue::UnaryOp(_, operand) | Rvalue::Cast(_, operand, _) => vec![operand],
                    Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => vec![a, b],
                    Rvalue::Aggregate(_, list) => list.iter().collect(),
                    _ => Vec::new(),
                };
                for operand in operands {
                    if let Operand::Constant(constant) = operand {
                        add(&constant.const_);
                    }
                }
            }
        }
        let operands: Vec<&Operand> = match &block.terminator.kind {
            TerminatorKind::SwitchInt { discr, .. } => vec![discr],
            TerminatorKind::Call { args, .. } => args.iter().collect(),
            TerminatorKind::Assert { cond, .. } => vec![cond],
            _ => Vec::new(),
        };
        for operand in operands {
            if let Operand::Constant(constant) = operand {
                add(&constant.const_);
            }
        }
    }
    found.sort();
    found
}

// ---------------------------------------------------------------------------------------------
// Closures (design 66, part A).

/// The facts of a closure type that the check needs.
pub struct CloInfo {
    /// The name of the body: its def path, as the name of its record (`shop::f::{closure#0}`).
    pub name: String,
    /// The key of the closure: the name, with the generic arguments of its parent item if it has
    /// some. Two closure types with one key have one body and one type of each capture.
    pub key: String,
    /// True if the form of the body is the form of an instance (in the forms `qx`, by the key):
    /// a closure of a generic item with concrete arguments, or a closure of the standard library.
    pub instance: bool,
    /// False if the check cannot know the form of the body: a closure of a generic item whose
    /// arguments are not concrete and are not the parameters of the item.
    pub known: bool,
    /// The type of each capture (a capture by reference is a reference).
    pub captures: Vec<Ty>,
    /// The kind of the closure (`Fn`, `FnMut`, `FnOnce`), as the compiler gives it.
    pub kind: rustc_middle::ty::ClosureKind,
    pub def: rustc_public::ty::ClosureDef,
    pub args: rustc_public::ty::GenericArgs,
    /// The key and the body name of each closure type in the generic arguments of the parent
    /// item (a closure of the standard library that takes a closure of the workspace).
    pub inner: Vec<(String, String)>,
    /// True for a closure of a crate of the standard library: the compiler loaded its crate from
    /// the sysroot (R65.3-1, R66.10-2). A closure of the local crate is never one.
    pub std: bool,
}

/// The facts of a closure type (design 66, part A). No facts for a type that is not a closure.
pub fn closure_info(ty: Ty) -> Option<CloInfo> {
    let TyKind::RigidTy(RigidTy::Closure(def, args)) = ty.kind() else { return None };
    rustc_middle::ty::tls::with(|tcx| {
        use rustc_middle::ty::TypeVisitableExt;
        let internal = rustc_public::rustc_internal::internal(tcx, ty);
        let rustc_middle::ty::Closure(def_id, closure_args) = internal.kind() else { return None };
        let closure = closure_args.as_closure();
        let name = def.name();
        let in_std = crate::stdlib::is_sysroot_crate(tcx, def_id.krate);
        // The generic arguments of the parent item, with no lifetime (MIR erases them).
        let parent: Vec<rustc_middle::ty::GenericArg<'_>> = closure.parent_args().iter().copied().filter(|arg| arg.as_region().is_none()).collect();
        let concrete = !internal.has_param() && !internal.has_aliases() && !internal.has_escaping_bound_vars();
        let identity: Vec<rustc_middle::ty::GenericArg<'_>> = rustc_middle::ty::GenericArgs::identity_for_item(tcx, tcx.typeck_root_def_id(*def_id)).iter().filter(|arg| arg.as_region().is_none()).collect();
        let key = if parent.is_empty() { name.clone() } else { format!("{name}::<{}>", parent.iter().map(|arg| arg.to_string()).collect::<Vec<_>>().join(", ")) };
        let instance = concrete && (!parent.is_empty() || in_std);
        let known = instance || (!in_std && (parent.is_empty() || parent == identity));
        let kind = closure.kind();
        let captures = closure.upvar_tys().iter().map(rustc_public::rustc_internal::stable).collect();
        let mut inner: Vec<(String, String)> = Vec::new();
        for arg in &parent {
            for part in arg.walk() {
                if let Some(part) = part.as_type()
                    && let rustc_middle::ty::Closure(..) = part.kind()
                    && let Some(found) = closure_info(rustc_public::rustc_internal::stable(part))
                    && !found.std
                    && !inner.iter().any(|(key, _)| *key == found.key)
                {
                    inner.push((found.key, found.name));
                }
            }
        }
        Some(CloInfo { name, key, instance, known, captures, kind, def, args, inner, std: in_std })
    })
}

/// A call of a closure through `Fn::call`, `FnMut::call_mut`, or `FnOnce::call_once` whose self
/// type is the closure type: the facts of the closure and the mode of the call (see
/// `IrCallee::c`). No result for a call through a reference to a closure (`<&F as Fn>::call`:
/// an impl of the standard library) and for each other call.
pub fn closure_call(body: &Body, func: &Operand) -> Option<(CloInfo, u8)> {
    let ty = func.ty(body.locals()).ok()?;
    let TyKind::RigidTy(RigidTy::FnDef(def, args)) = ty.kind() else { return None };
    let GenericArgKind::Type(self_ty) = args.0.first()? else { return None };
    let info = closure_info(*self_ty)?;
    let requested = rustc_middle::ty::tls::with(|tcx| {
        let def_id = rustc_public::rustc_internal::internal(tcx, def.def_id());
        let trait_id = tcx.trait_of_assoc(def_id)?;
        tcx.fn_trait_kind_from_def_id(trait_id)
    })?;
    let by_value = requested == rustc_middle::ty::ClosureKind::FnOnce && info.kind != rustc_middle::ty::ClosureKind::FnOnce;
    Some((info, if by_value { 2 } else { 1 }))
}

/// The body of a closure of an instance (design 66, part A): the body of the closure item with
/// the concrete arguments of the closure type.
pub fn closure_body(info: &CloInfo) -> Option<Body> {
    let kind = match info.kind {
        rustc_middle::ty::ClosureKind::Fn => rustc_public::ty::ClosureKind::Fn,
        rustc_middle::ty::ClosureKind::FnMut => rustc_public::ty::ClosureKind::FnMut,
        rustc_middle::ty::ClosureKind::FnOnce => rustc_public::ty::ClosureKind::FnOnce,
    };
    let instance = rustc_public::mir::mono::Instance::resolve_closure(info.def, &info.args, kind).ok()?;
    crate::safe::instance_body(&instance)
}

/// The keys of the closure types of the workspace in the generic arguments of a callee (also in
/// the arguments of a closure type of the standard library), with the names of their bodies
/// (design 66, part A). The text of a callee names a closure of the standard library by its def
/// path, the same in the two versions; its value has its own identity.
fn closure_keys(func: &Operand, body: &Body) -> Vec<(String, String)> {
    let Ok(ty) = func.ty(body.locals()) else { return Vec::new() };
    let TyKind::RigidTy(RigidTy::FnDef(_, args)) = ty.kind() else { return Vec::new() };
    let mut found: Vec<(String, String)> = Vec::new();
    rustc_middle::ty::tls::with(|tcx| {
        let internal = rustc_public::rustc_internal::internal(tcx, &args);
        for arg in internal.iter() {
            for part in arg.walk() {
                let Some(part) = part.as_type() else { continue };
                if let rustc_middle::ty::Closure(..) = part.kind()
                    && let Some(info) = closure_info(rustc_public::rustc_internal::stable(part))
                    && !info.std
                    && !found.iter().any(|(key, _)| *key == info.key)
                {
                    found.push((info.key, info.name));
                }
            }
        }
    });
    found
}
