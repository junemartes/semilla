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
    },
    /// A reference: true for `&mut`, the type of the target.
    Ref(bool, u32),
    /// A type that the check does not model: the name, and true if a value needs a drop.
    Opaque { n: String, d: bool },
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
}

/// Builds the form of a body. `item` is the def of the body (for the typing environment).
pub fn build(body: &Body, item: Option<rustc_public::DefId>) -> Ir {
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
        let mut builder = Builder { body, tcx, typing, types: Vec::new(), ids: HashMap::new(), files: Vec::new(), item };
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
            TyKind::RigidTy(RigidTy::Adt(def, args)) if self.modeled(def) => {
                let enum_like = def.kind() == AdtKind::Enum;
                let mut variants = Vec::new();
                let mut discriminants = Vec::new();
                let mut discr_ty = None;
                let mut names = Vec::new();
                for variant in def.variants_iter() {
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
                IrTy::Adt { n: format!("{}", rustc_public::rustc_internal::internal(self.tcx, ty)), v: variants, vn: names, d: discriminants, dt, e: enum_like }
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
        !matches!(krate.as_str(), "core" | "std" | "alloc")
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
        IrBlock { s: statements, t: IrTerm { k: self.terminator(&block.terminator.kind), r, c } }
    }

    fn place(&mut self, place: &Place) -> IrPlace {
        let p = place
            .projection
            .iter()
            .map(|projection| match projection {
                ProjectionElem::Deref => IrProj::D,
                ProjectionElem::Field(index, _) => IrProj::F(*index),
                ProjectionElem::Downcast(variant) => IrProj::V(variant.to_index()),
                ProjectionElem::Index(_) | ProjectionElem::ConstantIndex { .. } => IrProj::U("an index".into()),
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
            Operand::RuntimeChecks(kind) => IrOp::R(format!("{kind:?}")),
        }
    }

    fn constant(&mut self, constant: &rustc_public::ty::MirConst) -> IrConst {
        let ty = constant.ty();
        if let Some(crate::consts::Val::Bits { v, .. }) = crate::consts::const_bits(constant, self.item) {
            let id = self.ty(ty, 0);
            return IrConst::I(v.to_string(), id);
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
                UnOp::PtrMetadata => IrRvalue::U("the length of a slice (the metadata of a pointer)".into()),
            },
            Rvalue::Cast(CastKind::IntToInt, value, to) => {
                let to = self.ty(*to, 0);
                IrRvalue::Cast(self.operand(value), to)
            }
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
                AggregateKind::Closure(..) | AggregateKind::CoroutineClosure(..) => IrRvalue::U("a closure value".into()),
                AggregateKind::Coroutine(..) => IrRvalue::U("a coroutine value".into()),
                AggregateKind::Array(_) => IrRvalue::U("an array".into()),
                AggregateKind::RawPtr(..) => IrRvalue::U("a raw pointer".into()),
            },
            Rvalue::Ref(_, kind, place) => IrRvalue::Ref(matches!(kind, BorrowKind::Mut { .. }), self.place(place)),
            Rvalue::Reborrow(_, mutability, place) => IrRvalue::Ref(matches!(mutability, rustc_public::mir::Mutability::Mut), self.place(place)),
            Rvalue::Discriminant(place) => IrRvalue::Disc(self.place(place)),
            Rvalue::AddressOf(..) => IrRvalue::U("a raw pointer".into()),
            Rvalue::Len(_) => IrRvalue::U("the length of a slice".into()),
            Rvalue::Repeat(..) => IrRvalue::U("an array".into()),
            Rvalue::ThreadLocalRef(_) => IrRvalue::U("a thread local".into()),
        }
    }

    /// The cleanup path of an unwind action: the drops in order up to `resume`.
    fn unwind(&mut self, action: Option<&UnwindAction>) -> IrUnwind {
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
                let uw = self.unwind(Some(unwind));
                IrTermKind::Assert(self.operand(cond), *expected, assert_name(msg), *target, uw)
            }
            TerminatorKind::Call { func, args, destination, target, unwind } => {
                if crate::is_opaque_wrap(func) {
                    // The opaque wrap of the driver (`black_box`) is the identity.
                    let value = args.first().map(|arg| self.operand(arg));
                    return match (value, target) {
                        (Some(value), Some(target)) => IrTermKind::Call(IrCallee { p: "black_box".into(), w: None, k: Some("black_box".into()), m: None, g: false }, vec![value], self.place(destination), Some(*target), IrUnwind::Drops(Vec::new())),
                        _ => IrTermKind::U("an opaque wrap".into()),
                    };
                }
                let callee = self.callee(func, self.span_of(kind));
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
        let mut callee = IrCallee { p: text, w: None, k: None, m: mark.map(str::to_string), g: false };
        let Ok(ty) = func.ty(self.body.locals()) else { return callee };
        let TyKind::RigidTy(RigidTy::FnDef(def, args)) = ty.kind() else { return callee };
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
                    if !matches!(krate.as_str(), "core" | "std" | "alloc") {
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
            let Ok(ty) = func.ty(current.locals()) else { continue };
            let TyKind::RigidTy(RigidTy::FnDef(def, args)) = ty.kind() else { continue };
            if !concrete(&args) {
                continue;
            }
            let Ok(instance) = rustc_public::mir::mono::Instance::resolve(def, &args) else { continue };
            let in_std = matches!(instance.def.krate().name.as_str(), "core" | "std" | "alloc");
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
            let form = build(&callee, None);
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
        let ConstantKind::Unevaluated(unevaluated) = constant.kind() else { return };
        if unevaluated.promoted.is_some() {
            return;
        }
        let Some(crate::consts::Val::Bits { v, w, s }) = crate::consts::const_bits(constant, Some(item.def_id())) else { return };
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
