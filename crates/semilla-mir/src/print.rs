//! The canonical text of a MIR body.
//!
//! The rustc_public printer (`Body::dump`) is not exact enough for a proof: it hides the cast
//! kind, it prints short names for structs and enum variants, and it prints only an allocation
//! number for a constant in memory. Two different bodies can then get the same text. This printer
//! prints all the parts that have an effect on the behavior:
//!
//! - Full def paths, with the crate name first (for example `mycrate::cart::Cart`).
//! - The cast kind of each cast.
//! - The bytes and the pointers of each constant in memory, recursively. The text of a large
//!   allocation (a table) is its SHA-256 hash.
//! - The MIR of a promoted constant of a generic body, in place (a non-generic body has its
//!   constants evaluated already, so it prints the value).
//! - A unique word (`unique`) for a part that it cannot see. Then the body is never equal to
//!   another body. The remaining cases:
//!   - a promoted constant whose MIR rustc does not have (`is_mir_available` is false), or that
//!     nests deeper than `MAX_PROMOTED_DEPTH`;
//!   - a constant with a pointer chain deeper than `MAX_ALLOC_DEPTH`;
//!   - a bound constant (`TyConstKind::Bound`), which does not occur in a body;
//!   - a statement kind that the N0 rules remove (it does not occur after the rules);
//!   - a body that the driver cannot read (a panic in rustc_public, see `failed_record`).
//!
//! The printer also collects the def paths of the local functions that the body refers to.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};

use rustc_public::mir::alloc::{AllocId, GlobalAlloc};
use rustc_public::mir::{
    AggregateKind, AssertMessage, BasicBlock, Body, BorrowKind, CastKind, InlineAsmOperand,
    NonDivergingIntrinsic, Operand, Place, ProjectionElem, Rvalue, StatementKind, TerminatorKind,
    UnwindAction,
};
use rustc_public::ty::{
    Allocation, ConstantKind, GenericArgKind, GenericArgs, MirConst, RigidTy, Ty, TyConst,
    TyConstKind, TyKind,
};
use rustc_public::ty::{ConstDef, Promoted};
use rustc_public::{CrateDef, rustc_internal};
use rustc_public_bridge::IndexedVal;

/// The longest text of an allocation that the printer prints in full. A longer text becomes
/// its hash.
const MAX_ALLOC_TEXT: usize = 1024;

thread_local! {
    /// The printed text of each allocation of this compile, with the local functions in it.
    static ALLOC_TEXT: std::cell::RefCell<std::collections::HashMap<usize, (String, Vec<String>)>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The deepest nesting of promoted constants that the printer prints.
const MAX_PROMOTED_DEPTH: usize = 4;

/// The deepest chain of pointers in a constant that the printer follows.
const MAX_ALLOC_DEPTH: usize = 8;

/// Prints the parts of a body. It also records the local functions that the body refers to.
#[derive(Default)]
pub struct Printer {
    /// The def paths of the local functions in the printed text.
    pub callees: BTreeSet<String>,
    /// The depth of the promoted bodies that the printer prints now.
    promoted_depth: usize,
}

impl Printer {
    /// Returns the canonical text of a body.
    pub fn body(&mut self, body: &Body) -> String {
        let mut out = String::new();
        let args: Vec<String> = body
            .arg_locals()
            .iter()
            .enumerate()
            .map(|(index, decl)| format!("_{}: {}", index + 1, self.ty(decl.ty)))
            .collect();
        writeln!(out, "fn _({}) -> {} {{", args.join(", "), self.ty(body.ret_local().ty)).unwrap();
        for (local, decl) in body.locals().iter().enumerate() {
            if local == 0 || local > body.arg_locals().len() {
                writeln!(out, "    let _{local}: {};", self.ty(decl.ty)).unwrap();
            }
        }
        for (index, block) in body.blocks.iter().enumerate() {
            writeln!(out, "    bb{index}: {{").unwrap();
            self.block(&mut out, block);
            writeln!(out, "    }}").unwrap();
        }
        out.push_str("}\n");
        out
    }

    fn block(&mut self, out: &mut String, block: &BasicBlock) {
        for statement in &block.statements {
            let text = self.statement(&statement.kind);
            writeln!(out, "        {text};").unwrap();
        }
        let text = self.terminator(&block.terminator.kind);
        writeln!(out, "        {text};").unwrap();
    }

    fn statement(&mut self, kind: &StatementKind) -> String {
        match kind {
            StatementKind::Assign(place, rvalue) => {
                format!("{} = {}", self.place(place), self.rvalue(rvalue))
            }
            StatementKind::SetDiscriminant { place, variant_index } => {
                format!("discriminant({}) = {}", self.place(place), variant_index.to_index())
            }
            StatementKind::Intrinsic(NonDivergingIntrinsic::Assume(operand)) => {
                format!("assume({})", self.operand(operand))
            }
            StatementKind::Intrinsic(NonDivergingIntrinsic::CopyNonOverlapping(copy)) => format!(
                "copy_nonoverlapping({}, {}, {})",
                self.operand(&copy.src),
                self.operand(&copy.dst),
                self.operand(&copy.count)
            ),
            // The N0 rules remove the other statements before the print. If one stays, it must
            // not make two bodies equal by accident, so it gets a unique word.
            _ => unique("statement"),
        }
    }

    fn terminator(&mut self, kind: &TerminatorKind) -> String {
        match kind {
            TerminatorKind::Goto { target } => format!("goto -> bb{target}"),
            TerminatorKind::SwitchInt { discr, targets } => {
                let mut arms: Vec<String> = targets
                    .branches()
                    .map(|(value, target)| format!("{value}: bb{target}"))
                    .collect();
                arms.push(format!("otherwise: bb{}", targets.otherwise()));
                format!("switchInt({}) -> [{}]", self.operand(discr), arms.join(", "))
            }
            TerminatorKind::Resume => "resume".to_string(),
            TerminatorKind::Abort => "abort".to_string(),
            TerminatorKind::Return => "return".to_string(),
            TerminatorKind::Unreachable => "unreachable".to_string(),
            TerminatorKind::Drop { place, target, unwind } => {
                format!(
                    "drop({}) -> [return: bb{target}, {}]",
                    self.place(place),
                    unwind_text(unwind)
                )
            }
            TerminatorKind::Call { func, args, destination, target, unwind } => {
                let args: Vec<String> = args.iter().map(|arg| self.operand(arg)).collect();
                let target = match target {
                    Some(target) => format!("return: bb{target}"),
                    None => "no return".to_string(),
                };
                format!(
                    "{} = {}({}) -> [{target}, {}]",
                    self.place(destination),
                    self.operand(func),
                    args.join(", "),
                    unwind_text(unwind)
                )
            }
            TerminatorKind::Assert { cond, expected, msg, target, unwind } => format!(
                "assert({}{}, {}) -> [success: bb{target}, {}]",
                if *expected { "" } else { "!" },
                self.operand(cond),
                self.assert_message(msg),
                unwind_text(unwind)
            ),
            TerminatorKind::InlineAsm {
                template,
                operands,
                options,
                line_spans: _,
                destination,
                unwind,
            } => {
                let operands: Vec<String> =
                    operands.iter().map(|operand| self.asm_operand(operand)).collect();
                let destination = match destination {
                    Some(target) => format!("return: bb{target}"),
                    None => "no return".to_string(),
                };
                format!(
                    "asm({template:?}, [{}], {options:?}) -> [{destination}, {}]",
                    operands.join(", "),
                    unwind_text(unwind)
                )
            }
        }
    }

    fn asm_operand(&mut self, operand: &InlineAsmOperand) -> String {
        let input = operand.in_value.as_ref().map(|value| self.operand(value)).unwrap_or_default();
        let output = operand.out_place.as_ref().map(|place| self.place(place)).unwrap_or_default();
        format!("in({input}) out({output}) {:?}", operand.raw_rpr)
    }

    /// Prints the kind of the message and all its operands.
    pub fn assert_message(&mut self, msg: &AssertMessage) -> String {
        match msg {
            AssertMessage::BoundsCheck { len, index } => {
                format!("bounds_check(len: {}, index: {})", self.operand(len), self.operand(index))
            }
            AssertMessage::Overflow(op, left, right) => {
                format!("overflow {op:?}({}, {})", self.operand(left), self.operand(right))
            }
            AssertMessage::OverflowNeg(operand) => {
                format!("overflow_neg({})", self.operand(operand))
            }
            AssertMessage::DivisionByZero(operand) => {
                format!("division_by_zero({})", self.operand(operand))
            }
            AssertMessage::RemainderByZero(operand) => {
                format!("remainder_by_zero({})", self.operand(operand))
            }
            AssertMessage::ResumedAfterReturn(kind) => format!("resumed_after_return {kind:?}"),
            AssertMessage::ResumedAfterPanic(kind) => format!("resumed_after_panic {kind:?}"),
            AssertMessage::ResumedAfterDrop(kind) => format!("resumed_after_drop {kind:?}"),
            AssertMessage::MisalignedPointerDereference { required, found } => format!(
                "misaligned_pointer_dereference(required: {}, found: {})",
                self.operand(required),
                self.operand(found)
            ),
            AssertMessage::NullPointerDereference => "null_pointer_dereference".to_string(),
            AssertMessage::NullReferenceConstructed => "null_reference_constructed".to_string(),
            AssertMessage::InvalidEnumConstruction(operand) => {
                format!("invalid_enum_construction({})", self.operand(operand))
            }
        }
    }

    pub fn place(&mut self, place: &Place) -> String {
        let mut text = format!("_{}", place.local);
        for elem in &place.projection {
            text = match elem {
                ProjectionElem::Deref => format!("(*{text})"),
                ProjectionElem::Field(index, ty) => format!("({text}.{index}: {})", self.ty(*ty)),
                ProjectionElem::Index(local) => format!("{text}[_{local}]"),
                ProjectionElem::ConstantIndex { offset, min_length, from_end } => {
                    format!("{text}[{}{offset} of {min_length}]", if *from_end { "-" } else { "" })
                }
                ProjectionElem::Subslice { from, to, from_end } => {
                    format!("{text}[{from}:{}{to}]", if *from_end { "-" } else { "" })
                }
                ProjectionElem::Downcast(variant) => {
                    format!("({text} as variant#{})", variant.to_index())
                }
                ProjectionElem::OpaqueCast(ty) => format!("({text} as opaque {})", self.ty(*ty)),
            };
        }
        text
    }

    pub fn operand(&mut self, operand: &Operand) -> String {
        match operand {
            Operand::Copy(place) => self.place(place),
            Operand::Move(place) => format!("move {}", self.place(place)),
            Operand::Constant(constant) => self.mir_const(&constant.const_),
            Operand::RuntimeChecks(checks) => format!("runtime_checks({checks:?})"),
        }
    }

    pub fn rvalue(&mut self, rvalue: &Rvalue) -> String {
        match rvalue {
            Rvalue::AddressOf(kind, place) => format!("&raw {kind:?} {}", self.place(place)),
            Rvalue::Aggregate(kind, operands) => {
                let operands: Vec<String> =
                    operands.iter().map(|operand| self.operand(operand)).collect();
                format!("{}({})", self.aggregate_kind(kind), operands.join(", "))
            }
            Rvalue::BinaryOp(op, left, right) => {
                format!("{op:?}({}, {})", self.operand(left), self.operand(right))
            }
            Rvalue::Cast(kind, operand, ty) => {
                format!("{} as {} ({})", self.operand(operand), self.ty(*ty), cast_kind_text(kind))
            }
            Rvalue::CheckedBinaryOp(op, left, right) => {
                format!("Checked{op:?}({}, {})", self.operand(left), self.operand(right))
            }
            Rvalue::CopyForDeref(place) => format!("CopyForDeref({})", self.place(place)),
            Rvalue::Discriminant(place) => format!("discriminant({})", self.place(place)),
            Rvalue::Len(place) => format!("len({})", self.place(place)),
            Rvalue::Ref(_, kind, place) => {
                format!("&{} {}", borrow_kind_text(kind), self.place(place))
            }
            Rvalue::Reborrow(target, mutability, place) => {
                format!("reborrow {mutability:?} {} as {}", self.place(place), self.ty(*target))
            }
            Rvalue::Repeat(operand, count) => {
                format!("[{}; {}]", self.operand(operand), self.ty_const(count))
            }
            Rvalue::ThreadLocalRef(item) => format!("thread_local_ref {}", item.name()),
            Rvalue::UnaryOp(op, operand) => format!("{op:?}({})", self.operand(operand)),
            Rvalue::Use(operand, retag) => {
                let prefix = if matches!(retag, rustc_public::mir::WithRetag::No) {
                    "no_retag "
                } else {
                    ""
                };
                format!("{prefix}{}", self.operand(operand))
            }
        }
    }

    /// Prints the kind of an aggregate with its full def path and its generic arguments.
    pub fn aggregate_kind(&mut self, kind: &AggregateKind) -> String {
        match kind {
            AggregateKind::Array(ty) => format!("array {}", self.ty(*ty)),
            AggregateKind::Tuple => "tuple".to_string(),
            AggregateKind::Adt(def, variant, args, _user_ty, active_field) => format!(
                "adt {}{} variant#{} active {active_field:?}",
                def.name(),
                self.generic_args(args),
                variant.to_index()
            ),
            AggregateKind::Closure(def, args) => {
                format!("closure {}{}", def.name(), self.generic_args(args))
            }
            AggregateKind::Coroutine(def, args) => {
                format!("coroutine {}{}", def.name(), self.generic_args(args))
            }
            AggregateKind::CoroutineClosure(def, args) => {
                format!("coroutine_closure {}{}", def.name(), self.generic_args(args))
            }
            AggregateKind::RawPtr(ty, mutability) => {
                format!("raw_ptr {mutability:?} {}", self.ty(*ty))
            }
        }
    }

    /// Prints a type with full def paths. It records each local function in the type.
    pub fn ty(&mut self, ty: Ty) -> String {
        self.note_fn_defs(ty);
        ty.to_string()
    }

    /// Records the local functions that a type refers to (a function item type).
    fn note_fn_defs(&mut self, ty: Ty) {
        if let TyKind::RigidTy(RigidTy::FnDef(def, _)) = ty.kind() {
            if def.krate().is_local {
                self.callees.insert(def.name());
            }
        }
    }

    pub fn generic_args(&mut self, args: &GenericArgs) -> String {
        if args.0.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = args
            .0
            .iter()
            .map(|arg| match arg {
                GenericArgKind::Lifetime(_) => "'_".to_string(),
                GenericArgKind::Type(ty) => self.ty(*ty),
                GenericArgKind::Const(constant) => self.ty_const(constant),
            })
            .collect();
        format!("::<{}>", parts.join(", "))
    }

    /// Prints a constant with its type and its full value.
    pub fn mir_const(&mut self, constant: &MirConst) -> String {
        let ty = self.ty(constant.ty());
        match constant.kind() {
            // A zero-sized value has no bytes. Its type gives it fully, for example the type of a
            // function item contains the def path of the function.
            ConstantKind::ZeroSized => format!("const {ty}"),
            ConstantKind::Allocated(alloc) => {
                format!("const {ty} = {}", self.scalar_or_alloc(constant.ty(), alloc))
            }
            ConstantKind::Ty(ty_const) => format!("const {ty} = {}", self.ty_const(ty_const)),
            ConstantKind::Unevaluated(unevaluated) => {
                let args = self.generic_args(&unevaluated.args);
                let Some(index) = unevaluated.promoted else {
                    return format!("const {ty} = unevaluated {}{args}", unevaluated.def.name());
                };
                // A promoted constant of a generic body: print its MIR in place, with the
                // same N0 rules. Its value depends only on this MIR and the generic arguments.
                match self.promoted_body(&unevaluated.def, index) {
                    Some(text) => format!("const {ty} = promoted{args} {{\n{text}}}"),
                    // The MIR is not available (for example a body of another crate without
                    // MIR in its metadata), or the promoted constants nest too deep.
                    None => unique("promoted"),
                }
            }
            ConstantKind::Param(param) => {
                format!("const {ty} = param {}#{}", param.name, param.index)
            }
        }
    }

    /// Returns the canonical text of a promoted constant body: promoted `index` of the body of
    /// `def`. It reads the MIR from rustc, and uses the same N0 rules as for a normal body.
    fn promoted_body(&mut self, def: &ConstDef, index: Promoted) -> Option<String> {
        if self.promoted_depth >= MAX_PROMOTED_DEPTH {
            return None;
        }
        let body = rustc_middle::ty::tls::with(|tcx| {
            let def_id = rustc_internal::internal(tcx, def.def_id());
            if !tcx.is_mir_available(def_id) {
                return None;
            }
            let promoted = tcx.promoted_mir(def_id);
            let body = promoted.get(rustc_middle::mir::Promoted::from_u32(index))?;
            Some(rustc_internal::stable(body))
        })?;
        let canonical = crate::normalize::canonical_body(&body);
        self.promoted_depth += 1;
        let text = self.body(&canonical);
        self.promoted_depth -= 1;
        Some(text)
    }

    pub fn ty_const(&mut self, constant: &TyConst) -> String {
        match constant.kind() {
            TyConstKind::Param(param) => format!("param {}#{}", param.name, param.index),
            TyConstKind::Bound(..) => unique("bound_const"),
            TyConstKind::Unevaluated(def, args) => {
                format!("unevaluated {}{}", def.name(), self.generic_args(args))
            }
            TyConstKind::Value(ty, alloc) => {
                let text = self.scalar_or_alloc(*ty, alloc);
                format!("{} = {text}", self.ty(*ty))
            }
            TyConstKind::ZSTValue(ty) => self.ty(*ty),
        }
    }

    /// Prints an integer or a bool as a number. Prints other values as bytes.
    fn scalar_or_alloc(&mut self, ty: Ty, alloc: &Allocation) -> String {
        if alloc.provenance.ptrs.is_empty() && alloc.bytes.iter().all(Option::is_some) {
            let value = match ty.kind() {
                TyKind::RigidTy(RigidTy::Int(_)) => {
                    alloc.read_int().ok().map(|value| value.to_string())
                }
                TyKind::RigidTy(RigidTy::Uint(_)) => {
                    alloc.read_uint().ok().map(|value| value.to_string())
                }
                TyKind::RigidTy(RigidTy::Bool) => {
                    alloc.read_bool().ok().map(|value| value.to_string())
                }
                _ => None,
            };
            if let Some(value) = value {
                return value;
            }
        }
        self.allocation(alloc, 0)
    }

    /// Prints the bytes of an allocation and the targets of its pointers.
    fn allocation(&mut self, alloc: &Allocation, depth: usize) -> String {
        let mut text = String::from("[");
        for byte in &alloc.bytes {
            match byte {
                Some(byte) => write!(text, "{byte:02x}").unwrap(),
                None => text.push_str("__"),
            }
        }
        for (offset, provenance) in &alloc.provenance.ptrs {
            let target = self.global_alloc(provenance.0, depth + 1);
            write!(text, " @{offset}={target}").unwrap();
        }
        write!(text, "] align {} {:?}", alloc.align, alloc.mutability).unwrap();
        text
    }

    /// Prints the target of a pointer in a constant. The text of each allocation is computed
    /// once in a compile (a cache by allocation id), because many bodies refer to the same large
    /// tables.
    fn global_alloc(&mut self, id: AllocId, depth: usize) -> String {
        if depth > MAX_ALLOC_DEPTH {
            return unique("deep_alloc");
        }
        let key = id.to_index();
        if let Some((text, callees)) = ALLOC_TEXT.with(|cache| cache.borrow().get(&key).cloned()) {
            self.callees.extend(callees);
            return text;
        }
        let mut inner = Printer { promoted_depth: self.promoted_depth, ..Printer::default() };
        let mut text = inner.global_alloc_text(id, depth);
        if text.len() > MAX_ALLOC_TEXT {
            // A long text becomes its SHA-256 hash. Two allocations with the same hash have the
            // same text (the hash is collision resistant), so the compare stays exact.
            let digest = Sha256::digest(text.as_bytes());
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            text = format!("sha256:{hex} ({} characters)", text.len());
        }
        let callees: Vec<String> = inner.callees.into_iter().collect();
        self.callees.extend(callees.iter().cloned());
        ALLOC_TEXT.with(|cache| cache.borrow_mut().insert(key, (text.clone(), callees)));
        text
    }

    fn global_alloc_text(&mut self, id: AllocId, depth: usize) -> String {
        match GlobalAlloc::from(id) {
            GlobalAlloc::Function(instance) => {
                if instance.def.krate().is_local {
                    self.callees.insert(instance.def.name());
                }
                // The braces mark the position of a function item, as in a function item type.
                format!("fn {{{}}}", instance.name())
            }
            GlobalAlloc::VTable(ty, trait_ref) => {
                let trait_text = match trait_ref {
                    Some(binder) => {
                        let trait_ref = binder.skip_binder();
                        format!(
                            "{}{}",
                            trait_ref.def_id.name(),
                            self.generic_args(&trait_ref.generic_args)
                        )
                    }
                    None => "auto".to_string(),
                };
                format!("vtable {} {trait_text}", self.ty(ty))
            }
            GlobalAlloc::Static(def) => format!("static {}", def.name()),
            GlobalAlloc::Memory(alloc) => format!("memory {}", self.allocation(&alloc, depth)),
            GlobalAlloc::TypeId { ty } => format!("type_id {}", self.ty(ty)),
        }
    }
}

fn unwind_text(unwind: &UnwindAction) -> String {
    match unwind {
        UnwindAction::Continue => "unwind continue".to_string(),
        UnwindAction::Unreachable => "unwind unreachable".to_string(),
        UnwindAction::Terminate => "unwind terminate".to_string(),
        UnwindAction::Cleanup(target) => format!("unwind: bb{target}"),
    }
}

pub fn cast_kind_text(kind: &CastKind) -> String {
    format!("{kind:?}")
}

/// Prints a borrow kind. All mutable kinds (default, two-phase, closure capture) give the same
/// runtime code, so they get the same text. A fake borrow has no runtime effect.
pub fn borrow_kind_text(kind: &BorrowKind) -> &'static str {
    match kind {
        BorrowKind::Shared => "shared",
        BorrowKind::Fake(_) => "fake",
        BorrowKind::Mut { .. } => "mut",
    }
}

/// Returns a word that no other call returns, in this process or in another process. Use it for
/// a part that the printer cannot see, so that the body is never equal to another body.
pub fn unique(what: &str) -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_nanos())
        .unwrap_or_default();
    format!(
        "unknown_{what}#{}.{}.{nanos}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}
