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
//! - Members by name, not by position (batch 5e). The source names a struct field and an enum
//!   variant by its name; a step can change the map from names to positions (a reorder, or a new
//!   member in the middle). So:
//!   - a field projection prints the field path (`k::Pair.a`, `k::Ev::A.x`); a field of a tuple,
//!     a closure, or a tuple struct keeps its position (`k::T.0` for a tuple struct);
//!   - a downcast, an aggregate, and `SetDiscriminant` print the variant path (`k::Kind::Small`);
//!   - an ADT aggregate prints each operand with its field path, sorted (the order of the
//!     operands has no effect: the statements before the aggregate evaluate them);
//!   - a `switchInt` on a discriminant prints each target with the variant path, sorted, when the
//!     only uses of the discriminant are `switchInt` operands;
//!   - another read of a discriminant (a cast, a compare, a derived `PartialOrd` or `Hash`)
//!     prints the discriminant value of each variant, because the result depends on the values;
//!   - a constant in memory of a type with a local ADT prints the layout of each local ADT with
//!     its member names, because the bytes are in the order of the layout, not of the names.
//!
//! The printer also collects the def paths of the local functions that the body refers to.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};

use rustc_public::mir::alloc::{AllocId, GlobalAlloc};
use rustc_public::mir::visit::{Location, MirVisitor, PlaceContext};
use rustc_public::mir::{
    AggregateKind, AssertMessage, BasicBlock, BinOp, Body, BorrowKind, CastKind, InlineAsmOperand,
    Local,
    NonDivergingIntrinsic, Operand, Place, ProjectionElem, Rvalue, StatementKind, TerminatorKind,
    UnwindAction,
};
use rustc_public::ty::{
    AdtDef, AdtKind, Allocation, ConstantKind, GenericArgKind, GenericArgs, MirConst, RigidTy, Ty,
    TyConst, TyConstKind, TyKind, VariantIdx,
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
    /// The layout note of each type of a constant in this compile (see `layout_note`).
    static LAYOUT_NOTE: std::cell::RefCell<std::collections::HashMap<Ty, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The most local ADTs that the layout note of one constant type describes. A type with more
/// gets a unique word.
const MAX_LAYOUT_ADTS: usize = 64;

/// The deepest nesting of promoted constants that the printer prints.
const MAX_PROMOTED_DEPTH: usize = 4;

/// The deepest chain of pointers in a constant that the printer follows.
const MAX_ALLOC_DEPTH: usize = 8;

/// Prints the parts of a body. It also records the local functions that the body refers to.
#[derive(Default)]
pub struct Printer {
    /// The def paths of the local functions in the printed text.
    pub callees: BTreeSet<String>,
    /// The uses of the build configuration in the printed text (see `config.rs`).
    pub config: Vec<String>,
    /// The depth of the promoted bodies that the printer prints now.
    promoted_depth: usize,
    /// The body that the printer prints now.
    ctx: BodyCtx,
}

/// What the printer knows about the body that it prints now.
#[derive(Default)]
struct BodyCtx {
    /// The type of each local.
    locals: Vec<Ty>,
    /// The discriminant locals (see `discriminant_uses`).
    discriminants: HashMap<Local, DiscriminantUse>,
    /// The locals that an opaque call with no span writes (see `opaque_mark`).
    opaque: std::collections::HashSet<Local>,
}

/// How a body uses a local that holds the discriminant of an enum.
#[derive(Clone, Debug)]
pub enum DiscriminantUse {
    /// The only uses are `switchInt` operands, and each value of the switches is the
    /// discriminant of a variant. The value of each variant, with the variant path.
    Switch(Vec<(u128, String)>),
    /// Another use: the value of the discriminant matters. `cast` is true when a cast (`as`)
    /// reads it.
    Value { cast: bool },
}

/// The path of a variant: `k::Kind::Small` for an enum, the type path for a struct or a union.
pub fn variant_path(def: AdtDef, variant: VariantIdx) -> String {
    if def.kind() != AdtKind::Enum {
        return def.name();
    }
    match def.variant(variant) {
        Some(found) => format!("{}::{}", def.name(), found.name()),
        None => unique("variant"),
    }
}

/// The name of a field of a value of type `base`.
pub enum FieldName {
    /// A member of an ADT: `k::Pair.a`, `k::Ev::A.x`, or `k::T.0` (a tuple struct).
    Member(String),
    /// A field that the source names by position: of a tuple, a closure, or a coroutine.
    Position,
}

/// Returns the name of field `index` of a value of type `base`. `variant` is the variant of the
/// downcast before the field, for an enum. The result is a unique word when the field is not
/// known, so that a body with it is never equal to another body.
pub fn field_name(base: Ty, variant: Option<VariantIdx>, index: usize) -> FieldName {
    let TyKind::RigidTy(RigidTy::Adt(def, _)) = base.kind() else {
        return FieldName::Position;
    };
    let variant = match (def.kind(), variant) {
        (AdtKind::Enum, Some(variant)) => variant,
        (AdtKind::Enum, None) => return FieldName::Member(unique("field")),
        _ => VariantIdx::to_val(0),
    };
    let Some(found) = def.variant(variant) else { return FieldName::Member(unique("field")) };
    match found.fields().get(index) {
        Some(field) => FieldName::Member(format!("{}.{}", variant_path(def, variant), field.name)),
        None => FieldName::Member(unique("field")),
    }
}

/// The locals that an opaque call with no span writes (see `Printer::opaque_mark`).
fn opaque_locals(body: &Body) -> std::collections::HashSet<Local> {
    let mut found = std::collections::HashSet::new();
    for block in &body.blocks {
        let TerminatorKind::Call { func, destination, .. } = &block.terminator.kind else { continue };
        if !destination.projection.is_empty() || !crate::is_opaque_wrap(func) {
            continue;
        }
        let dummy = rustc_middle::ty::tls::with(|tcx| rustc_internal::internal(tcx, block.terminator.source_info.span).is_dummy());
        if dummy {
            found.insert(destination.local);
        }
    }
    found
}

/// The discriminant value of each variant of an enum, with the variant path, sorted by path.
pub fn discriminant_table(def: AdtDef) -> Vec<(u128, String)> {
    let mut table: Vec<(u128, String)> = def
        .variants_iter()
        .map(|variant| (def.discriminant_for_variant(variant.idx()).val, variant_path(def, variant.idx())))
        .collect();
    table.sort_by(|a, b| a.1.cmp(&b.1));
    table
}

/// The text of a discriminant table: `[k::Kind::Large = 1, k::Kind::Small = 0]`.
pub fn table_text(table: &[(u128, String)]) -> String {
    let parts: Vec<String> = table.iter().map(|(value, path)| format!("{path} = {value}")).collect();
    format!("[{}]", parts.join(", "))
}

/// Returns the enum of a type, or `None`.
fn enum_of(ty: Ty) -> Option<AdtDef> {
    match ty.kind() {
        TyKind::RigidTy(RigidTy::Adt(def, _)) if def.kind() == AdtKind::Enum => Some(def),
        _ => None,
    }
}

/// Finds the locals of a body that hold the discriminant of an enum: a local with exactly one
/// assignment, `_n = discriminant(place)`. A local that is a `switchInt` operand only gets the
/// variant paths of the switch values. Each other local prints the discriminant values.
pub fn discriminant_uses(body: &Body) -> HashMap<Local, DiscriminantUse> {
    struct Mentions(Vec<usize>);
    impl MirVisitor for Mentions {
        fn visit_local(&mut self, local: &Local, _ptx: PlaceContext, _location: Location) {
            self.0[*local] += 1;
        }
    }
    let mut mentions = Mentions(vec![0; body.locals().len()]);
    mentions.visit_body(body);
    let mentions = mentions.0;
    let plain = |operand: &Operand| match operand {
        Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => Some(place.local),
        _ => None,
    };
    // The assignments of each local.
    let mut assignments: HashMap<Local, usize> = HashMap::new();
    let mut enums: HashMap<Local, AdtDef> = HashMap::new();
    let mut casts: BTreeSet<Local> = BTreeSet::new();
    for block in &body.blocks {
        for statement in &block.statements {
            let StatementKind::Assign(place, rvalue) = &statement.kind else { continue };
            if place.projection.is_empty() {
                *assignments.entry(place.local).or_default() += 1;
            }
            match rvalue {
                Rvalue::Discriminant(source) if place.projection.is_empty() => {
                    if let Some(def) = source.ty(body.locals()).ok().and_then(enum_of) {
                        enums.insert(place.local, def);
                    }
                }
                Rvalue::Cast(_, operand, _) => casts.extend(plain(operand)),
                _ => {}
            }
        }
        if let TerminatorKind::Call { destination, .. } = &block.terminator.kind {
            if destination.projection.is_empty() {
                *assignments.entry(destination.local).or_default() += 1;
            }
        }
    }
    let mut switches: HashMap<Local, (usize, bool)> = HashMap::new();
    for block in &body.blocks {
        let TerminatorKind::SwitchInt { discr, targets } = &block.terminator.kind else { continue };
        let Some(local) = plain(discr) else { continue };
        let Some(def) = enums.get(&local) else { continue };
        let table = discriminant_table(*def);
        let known = targets.branches().all(|(value, _)| table.iter().any(|(v, _)| *v == value));
        let entry = switches.entry(local).or_insert((0, true));
        entry.0 += 1;
        entry.1 &= known;
    }
    enums
        .into_iter()
        .map(|(local, def)| {
            let single = assignments.get(&local) == Some(&1);
            let use_ = match switches.get(&local) {
                Some((count, true)) if single && mentions[local] == 1 + count => {
                    DiscriminantUse::Switch(discriminant_table(def))
                }
                _ => DiscriminantUse::Value { cast: casts.contains(&local) },
            };
            (local, use_)
        })
        .collect()
}

impl Printer {
    /// Returns the canonical text of a body.
    pub fn body(&mut self, body: &Body) -> String {
        let ctx = BodyCtx {
            locals: body.locals().iter().map(|decl| decl.ty).collect(),
            discriminants: discriminant_uses(body),
            opaque: opaque_locals(body),
        };
        let outer = std::mem::replace(&mut self.ctx, ctx);
        let text = self.body_text(body);
        self.ctx = outer;
        text
    }

    /// Prepares the printer for the parts of one body, without a print of the whole body (for
    /// the N4 terms).
    pub fn for_body(body: &Body) -> Printer {
        Printer {
            ctx: BodyCtx {
                locals: body.locals().iter().map(|decl| decl.ty).collect(),
                discriminants: discriminant_uses(body),
                opaque: opaque_locals(body),
            },
            ..Printer::default()
        }
    }

    /// How the body uses a discriminant local, or `None` for another local.
    pub fn discriminant_use(&self, local: Local) -> Option<&DiscriminantUse> {
        self.ctx.discriminants.get(&local)
    }

    fn body_text(&mut self, body: &Body) -> String {
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
        let text = match self.opaque_call(&block.terminator) {
            Some(text) => text,
            None => self.terminator(&block.terminator.kind),
        };
        writeln!(out, "        {text};").unwrap();
    }

    /// The text of an opaque call (see `config.rs`): `_3 = opaque(const bool = true @cfg!(..))`.
    fn opaque_call(&mut self, terminator: &rustc_public::mir::Terminator) -> Option<String> {
        let TerminatorKind::Call { func, args, destination, target: Some(target), .. } = &terminator.kind else { return None };
        if args.len() != 1 || !crate::is_opaque_wrap(func) {
            return None;
        }
        let value = match &args[0] {
            // The mark gives the source of the value.
            Operand::Constant(constant) => self.mir_const(&constant.const_),
            other => self.operand(other),
        };
        let mark = self.opaque_mark(terminator.source_info.span, &args[0]);
        Some(format!("{} = opaque({value}{mark}) -> [return: bb{target}]", self.place(destination)))
    }

    /// The source of the value of an opaque call, as the pass of `config.rs` marks it:
    /// - a call with a span: the span gives the source. A span with no source gives a text that
    ///   is different from each other text;
    /// - a call with no span on the result of a call with no span: the source is not known, and
    ///   the text is different from each other text;
    /// - each other call with no span: no source. The text of the value is sufficient.
    pub fn opaque_mark(&mut self, span: rustc_public::ty::Span, arg: &Operand) -> String {
        let source = rustc_middle::ty::tls::with(|tcx| {
            let span = rustc_internal::internal(tcx, span);
            if span.is_dummy() { None } else { Some(crate::config::span_source(tcx, span)) }
        });
        match source {
            None => {
                let twice = match arg {
                    Operand::Copy(place) | Operand::Move(place) => place.projection.is_empty() && self.ctx.opaque.contains(&place.local),
                    _ => false,
                };
                if twice { format!(" @{}", unique("config_value")) } else { String::new() }
            }
            Some(Some(source)) => {
                self.config.extend(source.config_line());
                source.suffix().unwrap_or_else(|| format!(" @{}", unique("config_value")))
            }
            Some(None) => format!(" @{}", unique("config_value")),
        }
    }

    fn statement(&mut self, kind: &StatementKind) -> String {
        match kind {
            StatementKind::Assign(place, Rvalue::Discriminant(source)) if place.projection.is_empty() => {
                let text = format!("discriminant({})", self.place(source));
                let text = match self.place_ty(source).and_then(enum_of) {
                    Some(def) => self.discriminant_text(text, def, Some(place.local)),
                    None => text,
                };
                format!("{} = {text}", self.place(place))
            }
            StatementKind::Assign(place, rvalue) => {
                format!("{} = {}", self.place(place), self.rvalue(rvalue))
            }
            StatementKind::SetDiscriminant { place, variant_index } => {
                let variant = match self.place_ty(place).map(|ty| ty.kind()) {
                    Some(TyKind::RigidTy(RigidTy::Adt(def, _))) => variant_path(def, *variant_index),
                    // A coroutine: its variants are states of the compiler, with no name.
                    _ => format!("variant#{}", variant_index.to_index()),
                };
                format!("discriminant({}) = {variant}", self.place(place))
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
                let names = match discr {
                    Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => {
                        match self.ctx.discriminants.get(&place.local) {
                            Some(DiscriminantUse::Switch(table)) => Some(table.clone()),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let mut arms: Vec<String> = targets
                    .branches()
                    .map(|(value, target)| {
                        let name = names.as_ref().and_then(|table| table.iter().find(|(v, _)| *v == value));
                        match name {
                            Some((_, path)) => format!("{path}: bb{target}"),
                            None => format!("{value}: bb{target}"),
                        }
                    })
                    .collect();
                // The values are different, so the order of the arms has no effect.
                if names.is_some() {
                    arms.sort();
                }
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
                    "{} = {}({}){} -> [{target}, {}]",
                    self.place(destination),
                    self.operand(func),
                    args.join(", "),
                    discriminant_call_note(func),
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
            AssertMessage::Overflow(op, left, right) => self.binary("overflow ", op, left, right),
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

    /// The text of a binary operation, with one order of the operands (batch 5n, A5).
    ///
    /// The optimizer of rustc gives the operands of a symmetric operation an order that depends
    /// on the order of the source text. So `x += y` and `x = x + (y)` can get different texts.
    /// The operands are places and constants: the read of an operand has no effect, and the
    /// statements before the operation evaluate them. So for the types on the list below:
    /// - a symmetric operation prints the operand with the smaller text first;
    /// - `Gt(a, b)` prints as `Lt(b, a)`, and `Ge(a, b)` prints as `Le(b, a)`.
    ///
    /// The list has the types with an exact symmetric operation: the integers, `bool`, and
    /// `char`. Each other type (a float, a pointer) keeps the order of rustc.
    fn binary(&mut self, prefix: &str, op: &BinOp, left: &Operand, right: &Operand) -> String {
        let exact = |ty: Option<Ty>| ty.is_some_and(|ty| matches!(ty.kind(), TyKind::RigidTy(RigidTy::Int(_) | RigidTy::Uint(_) | RigidTy::Bool | RigidTy::Char)));
        let exact = exact(self.operand_ty(left)) && exact(self.operand_ty(right));
        let (mut first, mut second) = (self.operand(left), self.operand(right));
        let mut name = format!("{op:?}");
        if exact {
            match op {
                BinOp::Gt | BinOp::Ge => {
                    name = if matches!(op, BinOp::Gt) { "Lt" } else { "Le" }.to_string();
                    std::mem::swap(&mut first, &mut second);
                }
                BinOp::Add | BinOp::AddUnchecked | BinOp::Mul | BinOp::MulUnchecked | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Eq | BinOp::Ne if first > second => {
                    std::mem::swap(&mut first, &mut second);
                }
                _ => {}
            }
        }
        format!("{prefix}{name}({first}, {second})")
    }

    /// The type of an operand in the current body, or `None` when it is not known.
    fn operand_ty(&self, operand: &Operand) -> Option<Ty> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.place_ty(place),
            Operand::Constant(constant) => Some(constant.const_.ty()),
            Operand::RuntimeChecks(_) => None,
        }
    }

    /// The type of a place in the current body, or `None` when it is not known.
    fn place_ty(&self, place: &Place) -> Option<Ty> {
        let mut ty = *self.ctx.locals.get(place.local)?;
        for elem in &place.projection {
            ty = elem.ty(ty).ok()?;
        }
        Some(ty)
    }

    pub fn place(&mut self, place: &Place) -> String {
        let mut text = format!("_{}", place.local);
        // The type of the place so far, and the variant of a downcast just before.
        let mut base = self.ctx.locals.get(place.local).copied();
        let mut variant = None;
        for elem in &place.projection {
            let downcast = match elem {
                ProjectionElem::Downcast(index) => Some(*index),
                _ => None,
            };
            text = match elem {
                ProjectionElem::Deref => format!("(*{text})"),
                ProjectionElem::Field(index, ty) => {
                    let name = match base {
                        Some(base) => match field_name(base, variant, *index) {
                            FieldName::Member(name) => name,
                            FieldName::Position => index.to_string(),
                        },
                        // The base type is not known: the position alone can hide a change.
                        None => unique("field"),
                    };
                    format!("({text}.{name}: {})", self.ty(*ty))
                }
                ProjectionElem::Index(local) => format!("{text}[_{local}]"),
                ProjectionElem::ConstantIndex { offset, min_length, from_end } => {
                    format!("{text}[{}{offset} of {min_length}]", if *from_end { "-" } else { "" })
                }
                ProjectionElem::Subslice { from, to, from_end } => {
                    format!("{text}[{from}:{}{to}]", if *from_end { "-" } else { "" })
                }
                ProjectionElem::Downcast(index) => {
                    let name = match base.map(|ty| ty.kind()) {
                        Some(TyKind::RigidTy(RigidTy::Adt(def, _))) => variant_path(def, *index),
                        // A coroutine state has no name in the source.
                        Some(TyKind::RigidTy(RigidTy::Coroutine(..))) => format!("variant#{}", index.to_index()),
                        _ => unique("downcast"),
                    };
                    format!("({text} as {name})")
                }
                ProjectionElem::OpaqueCast(ty) => format!("({text} as opaque {})", self.ty(*ty)),
            };
            variant = downcast;
            base = base.and_then(|ty| elem.ty(ty).ok());
        }
        text
    }

    pub fn operand(&mut self, operand: &Operand) -> String {
        match operand {
            Operand::Copy(place) => self.place(place),
            Operand::Move(place) => format!("move {}", self.place(place)),
            Operand::Constant(constant) => self.const_operand(constant),
            Operand::RuntimeChecks(checks) => format!("runtime_checks({checks:?})"),
        }
    }

    pub fn rvalue(&mut self, rvalue: &Rvalue) -> String {
        match rvalue {
            Rvalue::AddressOf(kind, place) => format!("&raw {kind:?} {}", self.place(place)),
            Rvalue::Aggregate(kind, operands) => {
                let head = self.aggregate_kind(kind);
                let operands: Vec<String> =
                    operands.iter().map(|operand| self.operand(operand)).collect();
                match aggregate_fields(kind, operands.len()) {
                    Some(names) => {
                        // Sorted: the order of the operands has no effect (see the module doc).
                        let mut parts: Vec<String> = names
                            .iter()
                            .zip(&operands)
                            .map(|(name, operand)| format!("{name}: {operand}"))
                            .collect();
                        parts.sort();
                        format!("{head} fields {{{}}}", parts.join(", "))
                    }
                    None => format!("{head}({})", operands.join(", ")),
                }
            }
            Rvalue::BinaryOp(op, left, right) => self.binary("", op, left, right),
            Rvalue::Cast(kind, operand, ty) => {
                let note = match self.operand_ty(operand) {
                    Some(source) => cast_note(kind, source, *ty),
                    None => unique("cast"),
                };
                format!("{} as {} ({}){note}", self.operand(operand), self.ty(*ty), cast_kind_text(kind))
            }
            Rvalue::CheckedBinaryOp(op, left, right) => self.binary("Checked", op, left, right),
            Rvalue::CopyForDeref(place) => format!("CopyForDeref({})", self.place(place)),
            Rvalue::Discriminant(place) => {
                let text = format!("discriminant({})", self.place(place));
                match self.place_ty(place).and_then(enum_of) {
                    Some(def) => self.discriminant_text(text, def, None),
                    None => text,
                }
            }
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

    /// The text of a discriminant read. A read whose only uses are `switchInt` operands needs no
    /// value: the switch prints the variant paths. Another read prints the value of each
    /// variant, and the word `cast` when a cast reads it. `local` is the destination, when the
    /// caller knows it.
    fn discriminant_text(&self, text: String, def: AdtDef, local: Option<Local>) -> String {
        let cast = match local.and_then(|local| self.ctx.discriminants.get(&local)) {
            Some(DiscriminantUse::Switch(_)) => return text,
            Some(DiscriminantUse::Value { cast }) => *cast,
            None => false,
        };
        let table = table_text(&discriminant_table(def));
        format!("{text}{} of {} values {table}", if cast { " cast" } else { "" }, def.name())
    }

    /// Prints the kind of an aggregate with its full def path and its generic arguments. The
    /// variant of an enum is its path. The field names are in `aggregate_fields`.
    pub fn aggregate_kind(&mut self, kind: &AggregateKind) -> String {
        match kind {
            AggregateKind::Array(ty) => format!("array {}", self.ty(*ty)),
            AggregateKind::Tuple => "tuple".to_string(),
            AggregateKind::Adt(def, variant, args, _user_ty, _active_field) => {
                let args = self.generic_args(args);
                match def.kind() {
                    AdtKind::Enum => format!("adt {}{args} variant {}", def.name(), variant_path(*def, *variant)),
                    AdtKind::Struct => format!("adt {}{args}", def.name()),
                    AdtKind::Union => format!("adt union {}{args}", def.name()),
                }
            }
            AggregateKind::Closure(def, args) => {
                // A closure is a callee of the body that makes it (A3): the body can pass it to
                // code that calls it.
                if def.krate().is_local {
                    self.callees.insert(def.name());
                }
                format!("closure {}{}", def.name(), self.generic_args(args))
            }
            AggregateKind::Coroutine(def, args) => {
                // The body of an `async` block or function runs when a caller polls the value.
                if def.krate().is_local {
                    self.callees.insert(def.name());
                }
                format!("coroutine {}{}", def.name(), self.generic_args(args))
            }
            AggregateKind::CoroutineClosure(def, args) => {
                if def.krate().is_local {
                    self.callees.insert(def.name());
                }
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

    /// Prints a constant operand: the constant, and the source of its value if the value is not
    /// configuration-free (see `config.rs`). The source makes two constants different that have
    /// the same value in the check build. It also stops each rule that reads the value from the
    /// text.
    pub fn const_operand(&mut self, constant: &rustc_public::mir::ConstOperand) -> String {
        let text = self.mir_const(&constant.const_);
        let source = rustc_middle::ty::tls::with(|tcx| {
            let span = rustc_internal::internal(tcx, constant.span);
            let internal = rustc_internal::internal(tcx, &constant.const_);
            crate::config::source_of(tcx, span, &internal)
        });
        match source.suffix() {
            Some(suffix) => {
                self.config.extend(source.config_line());
                format!("{text}{suffix}")
            }
            None => text,
        }
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
                    // A const item: print its value when it has one (no generic argument). The
                    // path alone hides a changed value: the const of an `include!` file, or of a
                    // file that a build script writes, has no record of its own.
                    if let Some(value) = self.evaluated(constant) {
                        return value;
                    }
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

    /// The value of a constant with no generic argument, printed as an allocated constant.
    fn evaluated(&mut self, constant: &MirConst) -> Option<String> {
        let value = rustc_middle::ty::tls::with(|tcx| {
            let internal = rustc_internal::internal(tcx, constant);
            if rustc_middle::ty::TypeVisitableExt::has_param(&internal.ty()) {
                return None;
            }
            if let rustc_middle::mir::Const::Unevaluated(unevaluated, _) = internal {
                if !unevaluated.args.is_empty() {
                    return None;
                }
            }
            let typing = rustc_middle::ty::TypingEnv::fully_monomorphized();
            let value = internal.eval(tcx, typing, rustc_span::DUMMY_SP).ok()?;
            Some(rustc_internal::stable(rustc_middle::mir::Const::Val(value, internal.ty())))
        })?;
        match value.kind() {
            ConstantKind::Allocated(_) | ConstantKind::ZeroSized => Some(self.mir_const(&value)),
            _ => None,
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
        let text = self.allocation(alloc, 0);
        format!("{text}{}", layout_note(ty))
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
                format!("vtable {}{} {trait_text}", self.ty(ty), layout_note(ty))
            }
            GlobalAlloc::Static(def) => format!("static {}", def.name()),
            GlobalAlloc::Memory(alloc) => format!("memory {}", self.allocation(&alloc, depth)),
            GlobalAlloc::TypeId { ty } => format!("type_id {}", self.ty(ty)),
        }
    }
}

/// The field paths of the operands of an ADT aggregate, in the order of the operands, or `None`
/// for an aggregate whose operands have positions only (a tuple, an array, a closure).
pub fn aggregate_fields(kind: &AggregateKind, count: usize) -> Option<Vec<String>> {
    let AggregateKind::Adt(def, variant, _, _, active_field) = kind else { return None };
    let path = variant_path(*def, *variant);
    let fields = match def.variant(*variant) {
        Some(found) => found.fields(),
        None => return Some((0..count).map(|_| unique("field")).collect()),
    };
    let name = |index: usize| match fields.get(index) {
        Some(field) => format!("{path}.{}", field.name),
        None => unique("field"),
    };
    Some(match active_field {
        // A union: the one operand is the active field.
        Some(index) => (0..count).map(|_| name(*index)).collect(),
        None => (0..count).map(name).collect(),
    })
}

/// A note for a cast that reads the bytes of a value in another type: a transmute, or a cast of
/// a pointer to another pointee type. The result depends on the layout of the local ADTs in the
/// two types, so the note gives their layouts (see `layout_note`). Empty for another cast.
pub fn cast_note(kind: &CastKind, source: Ty, target: Ty) -> String {
    if !matches!(kind, CastKind::Transmute | CastKind::PtrToPtr) {
        return String::new();
    }
    format!("{}{}", layout_note(source), layout_note(target))
}

/// A note for a call to a function that reads the discriminant value of an enum
/// (`std::mem::discriminant`, the `discriminant_value` intrinsic): the value of each variant.
/// The result (for example its hash) depends on the values, so a reorder must change the text.
fn discriminant_call_note(func: &Operand) -> String {
    let Operand::Constant(constant) = func else { return String::new() };
    let TyKind::RigidTy(RigidTy::FnDef(def, args)) = constant.const_.ty().kind() else {
        return String::new();
    };
    let name = def.name();
    let reads = ["std::", "core::"].iter().any(|krate| {
        name == format!("{krate}mem::discriminant")
            || name == format!("{krate}intrinsics::discriminant_value")
    });
    if !reads {
        return String::new();
    }
    let Some(GenericArgKind::Type(ty)) = args.0.first() else { return String::new() };
    match enum_of(*ty) {
        Some(adt) => format!(" reads the values of {} {}", adt.name(), table_text(&discriminant_table(adt))),
        None => String::new(),
    }
}

/// Collects the local ADT types that a type contains: in its fields (recursively), in its
/// generic arguments, and behind references, pointers, arrays, slices, and tuples. Returns false
/// when the type has more than `MAX_LAYOUT_ADTS` of them.
fn local_adts(ty: Ty, found: &mut Vec<Ty>, seen: &mut BTreeSet<String>) -> bool {
    let key = ty.to_string();
    if !seen.insert(key) {
        return true;
    }
    if seen.len() > 4 * MAX_LAYOUT_ADTS {
        return false;
    }
    match ty.kind() {
        TyKind::RigidTy(RigidTy::Adt(def, args)) => {
            if def.krate().is_local {
                found.push(ty);
                if found.len() > MAX_LAYOUT_ADTS {
                    return false;
                }
            }
            for arg in &args.0 {
                if let GenericArgKind::Type(inner) = arg {
                    if !local_adts(*inner, found, seen) {
                        return false;
                    }
                }
            }
            if def.krate().is_local {
                for variant in def.variants_iter() {
                    for field in variant.fields() {
                        use rustc_public::CrateDefType;
                        if !local_adts(field.ty_with_args(&args), found, seen) {
                            return false;
                        }
                    }
                }
            }
            true
        }
        TyKind::RigidTy(
            RigidTy::Ref(_, inner, _)
            | RigidTy::RawPtr(inner, _)
            | RigidTy::Array(inner, _)
            | RigidTy::Slice(inner),
        ) => local_adts(inner, found, seen),
        TyKind::RigidTy(RigidTy::Tuple(items)) => items.iter().all(|item| local_adts(*item, found, seen)),
        _ => true,
    }
}

/// A note on the layout of each local ADT in the type of a constant in memory. The bytes of the
/// constant are in the order of the layout. The note gives each member path with its place in
/// the layout, so a reorder of the members (or a new member) changes the text, and two
/// constants with the same bytes and other members never get the same text. Empty for a type
/// with no local ADT.
fn layout_note(ty: Ty) -> String {
    if let Some(note) = LAYOUT_NOTE.with(|cache| cache.borrow().get(&ty).cloned()) {
        return note;
    }
    let mut found = Vec::new();
    let note = if !local_adts(ty, &mut found, &mut BTreeSet::new()) {
        format!(" layout {}", unique("layout"))
    } else if found.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = found
            .iter()
            .map(|adt| {
                let TyKind::RigidTy(RigidTy::Adt(def, args)) = adt.kind() else { unreachable!("an ADT") };
                let variants: Vec<String> = def
                    .variants_iter()
                    .map(|variant| {
                        // The member paths, as in a field place (`k::Pair.a`, `k::Ev::A.x`): the
                        // name map of a rename applies to them.
                        let path = variant_path(def, variant.idx());
                        let fields: Vec<String> = variant.fields().iter().map(|field| format!("{path}.{}", field.name)).collect();
                        let value = if def.kind() == AdtKind::Enum {
                            format!(" = {}", def.discriminant_for_variant(variant.idx()).val)
                        } else {
                            String::new()
                        };
                        format!("{}{value} ({})", variant_path(def, variant.idx()), fields.join(", "))
                    })
                    .collect();
                let shape = match adt.layout() {
                    Ok(layout) => format!("{:?}", layout.shape()),
                    Err(_) if crate::is_generic(adt) => declared_layout(&def, &args),
                    Err(_) => unique("layout"),
                };
                format!("layout-of {adt}: {} {shape}", variants.join("; "))
            })
            .collect();
        format!(" layout {{{}}}", parts.join("; "))
    };
    LAYOUT_NOTE.with(|cache| cache.borrow_mut().insert(ty, note.clone()));
    note
}

/// The layout text of a generic local ADT (`Entry<T>`). rustc has no layout for a type with a
/// type parameter. The layout of each use of the type is a function of the declaration and of
/// the type arguments, so the text gives the parts of the declaration that the layout reads: the
/// `repr` options and the type of each member, in the order of the declaration. The caller
/// prints the member paths and the discriminant values. Each local ADT in a member type gets its
/// own text (`local_adts`). The text is the same in each build of the same declaration.
fn declared_layout(def: &AdtDef, args: &GenericArgs) -> String {
    use rustc_public::CrateDefType;
    let types: Vec<String> = def
        .variants_iter()
        .map(|variant| {
            let fields: Vec<String> = variant.fields().iter().map(|field| field.ty_with_args(args).to_string()).collect();
            format!("({})", fields.join(", "))
        })
        .collect();
    format!("declared {:?} types {}", def.repr(), types.join("; "))
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
