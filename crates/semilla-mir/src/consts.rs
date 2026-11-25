//! The constant values of a body (design 61, part A).
//!
//! A value is known for a constant operand (a literal, a `const` item, an associated constant:
//! the compiler evaluates it), and for a local that the body writes one time from known values.
//! The rule follows the chains: `_3 = CheckedMul(const 60_u32, const 1000_u32)` gives the pair
//! `(60000, false)`, and `assert(!move (_3.1: bool))` has a known condition.
//!
//! A parameter that the body never writes (no assignment, no mutable borrow) has the value of
//! the input. A local that the body writes one time with a copy of such a parameter has that
//! value too. The value key of a check or a branch (`Mul u64 p0 60000`) names the operation and
//! each operand as a value: two keys that are equal give the same operation on the same values.

use rustc_public::mir::visit::{Location, MirVisitor, PlaceContext};
use rustc_public::mir::{BinOp, Body, Operand, Place, ProjectionElem, Rvalue, StatementKind, UnOp};
use rustc_public::ty::{RigidTy, Ty, TyKind};

/// A known value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Val {
    /// An integer, a `bool`, or a `char`: the bits (zero extended), the width, and the sign.
    Bits { v: u128, w: u32, s: bool },
    /// The result of a checked operation: the wrapped value and the overflow flag.
    Pair { v: u128, w: u32, s: bool, o: bool },
    /// The input value of the parameter with this local index.
    Param(usize),
}

/// The width and the sign of an integer type, a `bool`, or a `char`.
pub fn int_of(ty: Ty) -> Option<(u32, bool)> {
    match ty.kind() {
        TyKind::RigidTy(RigidTy::Bool) => Some((1, false)),
        TyKind::RigidTy(RigidTy::Char) => Some((32, false)),
        TyKind::RigidTy(RigidTy::Uint(uint)) => Some((8 * uint.num_bytes() as u32, false)),
        TyKind::RigidTy(RigidTy::Int(int)) => Some((8 * int.num_bytes() as u32, true)),
        _ => None,
    }
}

fn mask(w: u32) -> u128 {
    if w >= 128 { u128::MAX } else { (1u128 << w) - 1 }
}

fn signed(v: u128, w: u32) -> i128 {
    if w >= 128 { v as i128 } else { ((v << (128 - w)) as i128) >> (128 - w) }
}

/// The value of an evaluated constant operand of a scalar type, if the compiler can evaluate it.
pub fn const_bits(constant: &rustc_public::ty::MirConst, item: Option<rustc_public::DefId>) -> Option<Val> {
    let (w, s) = int_of(constant.ty())?;
    rustc_middle::ty::tls::with(|tcx| {
        let internal = rustc_public::rustc_internal::internal(tcx, constant);
        if rustc_middle::ty::TypeVisitableExt::has_param(&internal.ty()) {
            return None;
        }
        if let rustc_middle::mir::Const::Unevaluated(unevaluated, _) = internal
            && rustc_middle::ty::TypeVisitableExt::has_param(&unevaluated.args)
        {
            return None;
        }
        let typing = match item.map(|item| rustc_public::rustc_internal::internal(tcx, item)) {
            Some(def) => rustc_middle::ty::TypingEnv::post_analysis(tcx, def),
            None => rustc_middle::ty::TypingEnv::fully_monomorphized(),
        };
        let int = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| internal.try_eval_scalar_int(tcx, typing))).ok()??;
        Some(Val::Bits { v: int.to_bits_unchecked() & mask(w), w, s })
    })
}

/// The known values of the locals of one body.
pub struct Consts {
    pub known: Vec<Option<Val>>,
    pub item: Option<rustc_public::DefId>,
    /// The locals of the body (for the types).
    decls: Vec<rustc_public::mir::LocalDecl>,
}

impl Consts {
    pub fn new(body: &Body, item: Option<rustc_public::DefId>) -> Consts {
        struct Writes(Vec<u32>);
        impl MirVisitor for Writes {
            fn visit_place(&mut self, place: &Place, context: PlaceContext, _location: Location) {
                if context.is_mutating()
                    && let Some(count) = self.0.get_mut(place.local)
                {
                    *count += 1;
                }
            }
        }
        let decls = body.locals().to_vec();
        let mut writes = Writes(vec![0; decls.len()]);
        writes.visit_body(body);
        let args = body.arg_locals().len();
        let mut consts = Consts { known: vec![None; decls.len()], item, decls };
        // A parameter that the body never writes keeps the input value.
        for index in 1..=args {
            if writes.0.get(index) == Some(&0) {
                consts.known[index] = Some(Val::Param(index));
            }
        }
        // A chain of known locals needs one more round for each link.
        for _ in 0..8 {
            let mut changed = false;
            for block in &body.blocks {
                for statement in &block.statements {
                    let StatementKind::Assign(place, value) = &statement.kind else { continue };
                    if !place.projection.is_empty() || place.local <= args || writes.0.get(place.local) != Some(&1) || consts.known[place.local].is_some() {
                        continue;
                    }
                    if let Some(found) = consts.rvalue(value) {
                        consts.known[place.local] = Some(found);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        consts
    }

    /// The value of an operand.
    pub fn operand(&self, operand: &Operand) -> Option<Val> {
        match operand {
            Operand::Constant(constant) => const_bits(&constant.const_, self.item),
            Operand::Copy(place) | Operand::Move(place) => self.place(place),
            _ => None,
        }
    }

    fn place(&self, place: &Place) -> Option<Val> {
        let value = self.known.get(place.local).copied().flatten()?;
        match (place.projection.as_slice(), value) {
            ([], value) => Some(value),
            ([ProjectionElem::Field(index, _)], Val::Pair { v, w, s, o }) => match index {
                0 => Some(Val::Bits { v, w, s }),
                1 => Some(Val::Bits { v: o as u128, w: 1, s: false }),
                _ => None,
            },
            _ => None,
        }
    }

    fn rvalue(&self, rvalue: &Rvalue) -> Option<Val> {
        match rvalue {
            Rvalue::Use(value, _) => self.operand(value),
            Rvalue::CopyForDeref(place) => self.place(place),
            Rvalue::UnaryOp(op, value) => {
                let Val::Bits { v, w, s } = self.operand(value)? else { return None };
                match op {
                    UnOp::Not => Some(Val::Bits { v: !v & mask(w), w, s }),
                    UnOp::Neg if s => Some(Val::Bits { v: v.wrapping_neg() & mask(w), w, s }),
                    _ => None,
                }
            }
            Rvalue::BinaryOp(op, left, right) => {
                let (Val::Bits { v: a, w, s }, Val::Bits { v: b, w: wb, .. }) = (self.operand(left)?, self.operand(right)?) else { return None };
                binary(*op, a, b, w, wb, s).map(|(v, w, s, _)| Val::Bits { v, w, s })
            }
            Rvalue::CheckedBinaryOp(op, left, right) => {
                let (Val::Bits { v: a, w, s }, Val::Bits { v: b, w: wb, .. }) = (self.operand(left)?, self.operand(right)?) else { return None };
                binary(*op, a, b, w, wb, s).map(|(v, w, s, o)| Val::Pair { v, w, s, o })
            }
            Rvalue::Cast(rustc_public::mir::CastKind::IntToInt, value, to) => {
                let Val::Bits { v, w, s } = self.operand(value)? else { return None };
                let (tw, ts) = int_of(*to)?;
                let wide = if s { signed(v, w) as u128 } else { v };
                Some(Val::Bits { v: wide & mask(tw), w: tw, s: ts })
            }
            _ => None,
        }
    }

    /// The value key of an operation (design 61, A1): the operation, the type, and each operand
    /// as a value. `None` if an operand is not a known value.
    pub fn key(&self, op: &str, operands: &[&Operand]) -> Option<String> {
        let mut parts = vec![op.to_string()];
        let ty = operands.first()?.ty(&self.decls).ok()?;
        let (w, s) = int_of(ty)?;
        parts.push(format!("{}{w}", if s { "i" } else { "u" }));
        for operand in operands {
            parts.push(match self.operand(operand)? {
                Val::Bits { v, .. } => v.to_string(),
                Val::Param(index) => format!("p{index}"),
                Val::Pair { .. } => return None,
            });
        }
        Some(parts.join(" "))
    }

    /// The value key of the local that a switch reads: the comparison that the body assigns to
    /// it one time, or the parameter.
    pub fn switch_key(&self, body: &Body, discr: &Operand) -> Option<String> {
        if let Some(Val::Param(index)) = self.operand(discr) {
            return Some(format!("p{index}"));
        }
        let (Operand::Copy(place) | Operand::Move(place)) = discr else { return None };
        if !place.projection.is_empty() {
            return None;
        }
        let mut found = None;
        let mut count = 0;
        for block in &body.blocks {
            for statement in &block.statements {
                if let StatementKind::Assign(target, value) = &statement.kind
                    && target.local == place.local
                {
                    count += 1;
                    found = Some(value);
                }
            }
        }
        if count != 1 {
            return None;
        }
        match found? {
            Rvalue::BinaryOp(op, left, right) => self.key(&format!("{op:?}"), &[left, right]),
            Rvalue::UnaryOp(UnOp::Not, value) => self.key("Not", &[value]),
            _ => None,
        }
    }
}

/// A binary operation on known bits: the wrapped result, its width and sign, and the overflow
/// flag. `None` for an operation that the rule does not fold (a division by zero, a shift).
fn binary(op: BinOp, a: u128, b: u128, w: u32, wb: u32, s: bool) -> Option<(u128, u32, bool, bool)> {
    let m = mask(w);
    let (sa, sb) = (signed(a, w), signed(b, wb));
    let in_range = |value: i128| -> bool {
        if s {
            let low = if w >= 128 { i128::MIN } else { -(1i128 << (w - 1)) };
            let high = if w >= 128 { i128::MAX } else { (1i128 << (w - 1)) - 1 };
            low <= value && value <= high
        } else {
            value >= 0 && (w >= 128 || (value as u128) <= m)
        }
    };
    let arith = |result: Option<i128>, wrapped: u128| -> Option<(u128, u32, bool, bool)> {
        let overflow = match result {
            Some(value) => !in_range(value),
            None => true,
        };
        Some((wrapped & m, w, s, overflow))
    };
    let (x, y) = if s { (sa, sb) } else { (a as i128, b as i128) };
    // An operand of 128 bits that does not fit `i128` gives no fold.
    if !s && (a > i128::MAX as u128 || b > i128::MAX as u128) {
        return None;
    }
    let flag = |value: bool| Some((value as u128, 1, false, false));
    match op {
        BinOp::Add => arith(x.checked_add(y), a.wrapping_add(b)),
        BinOp::Sub => arith(x.checked_sub(y), a.wrapping_sub(b)),
        BinOp::Mul => arith(x.checked_mul(y), a.wrapping_mul(b)),
        BinOp::Div if y != 0 => arith(x.checked_div(y), x.wrapping_div(y) as u128),
        BinOp::Rem if y != 0 => arith(x.checked_rem(y), x.wrapping_rem(y) as u128),
        BinOp::BitAnd => Some((a & b, w, s, false)),
        BinOp::BitOr => Some((a | b, w, s, false)),
        BinOp::BitXor => Some((a ^ b, w, s, false)),
        BinOp::Eq => flag(a == b),
        BinOp::Ne => flag(a != b),
        BinOp::Lt => flag(x < y),
        BinOp::Le => flag(x <= y),
        BinOp::Gt => flag(x > y),
        BinOp::Ge => flag(x >= y),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_product_of_two_constants_folds_with_its_overflow_flag() {
        assert_eq!(binary(BinOp::Mul, 60, 1000, 32, 32, false), Some((60000, 32, false, false)));
        assert_eq!(binary(BinOp::Add, 255, 1, 8, 8, false), Some((0, 8, false, true)));
        assert_eq!(binary(BinOp::Sub, 0x80, 1, 8, 8, true), Some((0x7f, 8, true, true)));
        assert_eq!(binary(BinOp::Div, 1, 0, 8, 8, false), None);
        assert_eq!(binary(BinOp::Gt, 0xff, 1, 8, 8, true), Some((0, 1, false, false)));
    }

    /// R66.10-1: a part of a digest that the driver cannot compute (the limit of the digest, the
    /// limit of the depth, a local item with no MIR) is never equal to another value, also with
    /// the same cause, and each digest that contains it says so. The negative case: a digest
    /// with only known parts keeps one text for one list. The driver never asks the compiler
    /// for the hash of its own crate: the compiler has no such hash before it writes the
    /// metadata, and the query stops the build.
    // rule+ R66.10-1
    // rule- R66.10-1
    #[test]
    fn a_part_that_the_driver_cannot_compute_is_never_equal_to_another_value() {
        let (a, b) = (not_known("an item of the crate with no MIR"), not_known("an item of the crate with no MIR"));
        assert_ne!(a, b);
        assert!(a.starts_with(NOT_KNOWN));
        let list = |value: &str| vec![("shop::unit".to_string(), "4f1c".to_string()), ("shop::Grid::ROWS".to_string(), value.to_string())];
        let (first, second) = (digest_text(&list(&a)), digest_text(&list(&b)));
        assert_ne!(first, second);
        assert!(first.starts_with("definition not known: an item of the crate with no MIR"), "{first}");
        // A nested digest keeps the part.
        let outer = digest_text(&[("impl: shop::L0::X for u8".to_string(), first.clone())]);
        assert!(outer.starts_with("definition not known: "), "{outer}");
        assert_eq!(digest_text(&list("9a2b")), digest_text(&list("9a2b")));
        assert!(digest_text(&list("9a2b")).starts_with("definition #"));
        // No source file of the driver asks for the hash of the local crate.
        let query = format!("{}({}", "crate_hash", "rustc_hir::def_id::LOCAL");
        let short = format!("{}({}", "crate_hash", "LOCAL");
        let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(&folder).unwrap().flatten() {
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            let code: String = text.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
            assert!(!code.contains(&query) && !code.contains(&short), "{} asks for the hash of the local crate", entry.path().display());
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The definition of a constant that the compiler does not evaluate here (design 66, part B).

/// The most items in the digest of one constant (design 66, part B, Decided by Claude). Past it,
/// the digest gets a part that the driver cannot compute (R66.10-1).
const MAX_DIGEST_ITEMS: usize = 48;

/// The start of the value of a digest part that the driver cannot compute (R66.10-1).
pub const NOT_KNOWN: &str = "not known: ";

/// The value of a digest part that the driver cannot compute (R66.10-1): the cause and a word
/// that no other call returns (`print::unique`), so the value is never equal to another value,
/// also in the other version. A body that reads the constant gets "not decided", never a proof.
/// Before, the value was the hash of the local crate (`tcx.crate_hash(LOCAL_CRATE)`), but the
/// compiler has no such hash before it writes the metadata of the crate: the query stops the
/// compiler, and the build fails.
fn not_known(cause: &str) -> String {
    format!("{NOT_KNOWN}{cause} ({})", crate::print::unique("digest"))
}

/// The deepest nesting of the digests of the impls of trait constants.
const MAX_DIGEST_DEPTH: usize = 4;

thread_local! {
    /// The nesting of the digests of the impls of trait constants.
    static DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The digest of each constant (the def and the promoted index), for the run of the driver.
    static DIGESTS: std::cell::RefCell<std::collections::HashMap<(rustc_hir::def_id::DefId, Option<u32>), Vec<(String, String)>>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// A short hash of a text.
pub(crate) fn short_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).iter().take(8).map(|byte| format!("{byte:02x}")).collect()
}

/// The text of a digest list for the constant values of a record (design 66, part B): one short
/// hash of each item and its digest. A list with a part that the driver cannot compute gives a
/// text with that part (R66.10-1): it is never equal to another text.
pub fn digest_text(list: &[(String, String)]) -> String {
    // A nested digest (the impl of a trait constant) keeps the part, so it reaches each digest
    // that contains it.
    if let Some(at) = list.iter().find_map(|(_, value)| value.find(NOT_KNOWN).map(|at| &value[at..])) {
        return format!("definition {at}");
    }
    let all: Vec<String> = list.iter().map(|(item, hash)| format!("{item}={hash}")).collect();
    format!("definition #{}", short_hash(&all.join(";")))
}

/// The constant values of a record for a constant with a digest (R66.3-2): the path with one
/// digest of the entries that are not impls, and one value for each impl (`PATH for TYPE`). The
/// compare of two records only compares the paths of the two versions, so an impl of one
/// version only gives no changed value.
pub fn digest_values(path: &str, list: &[(String, String)]) -> Vec<(String, String)> {
    let own: Vec<(String, String)> = list.iter().filter(|(item, _)| !item.starts_with("impl: ")).cloned().collect();
    let mut found = vec![(path.to_string(), digest_text(&own))];
    for (item, value) in list {
        if let Some(rest) = item.strip_prefix("impl: ")
            && let Some((_, ty)) = rest.split_once(" for ")
        {
            found.push((format!("{path} for {ty}"), value.clone()));
        }
    }
    found
}

/// The items that the value of a constant depends on, with a digest of each one (design 66,
/// part B):
///
/// - the definition of the constant: the text of its MIR (or its value for a trivial constant),
///   or of the promoted body;
/// - each function and each constant that this MIR uses, to the end of the chain;
/// - each impl of a trait constant (a type parameter selects the impl);
/// - the declaration of each ADT of the crate in a type of the MIR (its layout can be a value).
///
/// An item of another crate has the hash of its crate (one tool chain gives one standard library;
/// a dependency with another version gives another hash). A part that the driver cannot compute
/// (the limit of the digest, the limit of the depth, a local item with no MIR) has a value that
/// is never equal to another value (R66.10-1). Two equal lists give one value for the same
/// generic arguments.
pub fn definition_digest(def: rustc_public::DefId, promoted: Option<u32>) -> Vec<(String, String)> {
    rustc_middle::ty::tls::with(|tcx| {
        let def = rustc_public::rustc_internal::internal(tcx, def);
        if let Some(found) = DIGESTS.with(|memo| memo.borrow().get(&(def, promoted)).cloned()) {
            return found;
        }
        let found = digest_of(tcx, def, promoted);
        DIGESTS.with(|memo| memo.borrow_mut().insert((def, promoted), found.clone()));
        found
    })
}

fn digest_of(tcx: rustc_middle::ty::TyCtxt<'_>, def: rustc_hir::def_id::DefId, promoted: Option<u32>) -> Vec<(String, String)> {
    use rustc_hir::def::DefKind;
    let mut found: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<(rustc_hir::def_id::DefId, Option<u32>)> = std::collections::HashSet::new();
    let mut adts: std::collections::HashSet<rustc_hir::def_id::DefId> = std::collections::HashSet::new();
    let mut work: Vec<(rustc_hir::def_id::DefId, Option<u32>)> = vec![(def, promoted)];
    while let Some((item, index)) = work.pop() {
        if !seen.insert((item, index)) {
            continue;
        }
        if found.len() >= MAX_DIGEST_ITEMS {
            found.push(("the limit of the digest".into(), not_known("the limit of the digest")));
            break;
        }
        let name = match index {
            Some(index) => format!("{}::promoted[{index}]", tcx.def_path_str(item)),
            None => tcx.def_path_str(item),
        };
        // A trait constant: the impl that a type parameter selects gives the value. Each impl
        // has an entry of its own (`impl: …`): the digest of its item, or "the default". An impl
        // of one version only is a new type for the generic body, not a change of an old one.
        if index.is_none()
            && matches!(tcx.def_kind(item), DefKind::AssocConst)
            && let Some(trait_id) = tcx.trait_of_assoc(item)
        {
            for implementation in tcx.all_impls(trait_id) {
                let self_ty = tcx.type_of(implementation).instantiate_identity().skip_norm_wip();
                let key = format!("impl: {name} for {self_ty}");
                let value = match tcx.impl_item_implementor_ids(implementation).get(&item) {
                    Some(target) => {
                        let nested = DEPTH.with(|depth| depth.get());
                        if nested >= MAX_DIGEST_DEPTH {
                            not_known("the limit of the depth of the digest")
                        } else {
                            DEPTH.with(|depth| depth.set(nested + 1));
                            let found = definition_digest(rustc_public::rustc_internal::stable(*target), None);
                            DEPTH.with(|depth| depth.set(nested));
                            digest_text(&found)
                        }
                    }
                    None => "the default".into(),
                };
                found.push((key, value));
            }
        }
        if !item.is_local() {
            // The hash of another crate comes from its metadata, so the compiler has it.
            found.push((name, format!("crate {}", tcx.crate_hash(item.krate))));
            continue;
        }
        // An item with no body (a trait constant with no default value): its impls give the
        // value.
        if tcx.hir_maybe_body_owned_by(item.expect_local()).is_none() {
            found.push((name, "no body".into()));
            continue;
        }
        let body: Option<rustc_public::mir::Body> = match index {
            Some(index) => {
                if !tcx.is_mir_available(item) && !matches!(tcx.def_kind(item), DefKind::Const | DefKind::AssocConst | DefKind::AnonConst | DefKind::Static { .. }) {
                    None
                } else {
                    let all = tcx.promoted_mir(item);
                    all.get(rustc_middle::mir::Promoted::from_u32(index)).map(rustc_public::rustc_internal::stable)
                }
            }
            None => {
                if matches!(tcx.def_kind(item), DefKind::Const | DefKind::AssocConst) && tcx.is_trivial_const(item) {
                    found.push((name, short_hash(&format!("{:?}", tcx.trivial_const(item)))));
                    continue;
                }
                let crate_item = rustc_public::CrateItem(rustc_public::rustc_internal::stable(item));
                if crate_item.has_body() { crate_item.body() } else { None }
            }
        };
        let Some(body) = body else {
            found.push((name, not_known("an item of the crate with no MIR")));
            continue;
        };
        let text = crate::print::Printer::default().body(&crate::normalize::canonical_body(&body));
        found.push((name, short_hash(&text)));
        // The functions, the constants, and the types of the body.
        let internal_body = |b: &rustc_public::mir::Body| b.locals().iter().map(|decl| rustc_public::rustc_internal::internal(tcx, decl.ty)).collect::<Vec<_>>();
        let mut types = internal_body(&body);
        for block in &body.blocks {
            let mut operands: Vec<&Operand> = Vec::new();
            for statement in &block.statements {
                if let StatementKind::Assign(_, value) = &statement.kind {
                    match value {
                        Rvalue::Use(operand, _) | Rvalue::UnaryOp(_, operand) | Rvalue::Cast(_, operand, _) | Rvalue::Repeat(operand, _) => operands.push(operand),
                        Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
                            operands.push(a);
                            operands.push(b);
                        }
                        Rvalue::Aggregate(_, list) => operands.extend(list.iter()),
                        _ => {}
                    }
                }
            }
            match &block.terminator.kind {
                rustc_public::mir::TerminatorKind::Call { func, args, .. } => {
                    operands.push(func);
                    operands.extend(args.iter());
                }
                rustc_public::mir::TerminatorKind::SwitchInt { discr, .. } => operands.push(discr),
                rustc_public::mir::TerminatorKind::Assert { cond, .. } => operands.push(cond),
                _ => {}
            }
            for operand in operands {
                let Operand::Constant(constant) = operand else { continue };
                let ty = rustc_public::rustc_internal::internal(tcx, constant.const_.ty());
                types.push(ty);
                if let rustc_middle::ty::FnDef(callee, _) = ty.kind() {
                    work.push((*callee, None));
                }
                if let rustc_public::ty::ConstantKind::Unevaluated(unevaluated) = constant.const_.kind() {
                    let target = rustc_public::rustc_internal::internal(tcx, rustc_public::CrateDef::def_id(&unevaluated.def));
                    work.push((target, unevaluated.promoted.map(|promoted| promoted as u32)));
                }
            }
        }
        // The declaration of each ADT of the crate in a type of the body.
        for ty in types {
            for part in ty.walk() {
                let Some(part) = part.as_type() else { continue };
                if let rustc_middle::ty::Adt(adt, _) = part.kind()
                    && adt.did().is_local()
                    && adts.insert(adt.did())
                {
                    let fields: Vec<String> = adt.all_fields().map(|field| format!("{}: {}", field.name, tcx.type_of(field.did).instantiate_identity().skip_norm_wip())).collect();
                    let text = format!("{:?} {:?} {}", adt.repr(), adt.variants().iter().map(|variant| format!("{:?}", variant.discr)).collect::<Vec<_>>(), fields.join(", "));
                    found.push((format!("the declaration of {}", tcx.def_path_str(adt.did())), short_hash(&text)));
                }
            }
        }
    }
    found.sort();
    found
}

/// The most elements of a static value with a text of each element (R68.6-1). A larger value
/// has the digest of its allocation.
const MAX_STATIC_ELEMENTS: u64 = 1024;

/// R68.6-1: the value text of a static that cannot change, for the constant values of a reader
/// (`cv`). An integer, a `bool`, a `char`, or an array of them (also nested) has the text of its
/// elements (`[1, 2, 9]`), so the line of a reader can name each changed element. Each other
/// value has a short hash of the text of its allocation (the bytes and the targets of its
/// pointers): two equal values have equal texts. A static that the compiler does not evaluate
/// has the digest of its definition (R66.3-2).
pub fn static_value(def: &rustc_public::mir::mono::StaticDef) -> String {
    thread_local! {
        /// The value of each static, for the run of the driver: many bodies read one table.
        static VALUES: std::cell::RefCell<std::collections::HashMap<String, String>> = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let key = format!("{:?}", rustc_public::CrateDef::def_id(def));
    if let Some(found) = VALUES.with(|memo| memo.borrow().get(&key).cloned()) {
        return found;
    }
    let found = static_value_of(def);
    VALUES.with(|memo| memo.borrow_mut().insert(key, found.clone()));
    found
}

fn static_value_of(def: &rustc_public::mir::mono::StaticDef) -> String {
    match def.eval_initializer() {
        Ok(alloc) => match typed_value(def.ty(), &alloc, 0) {
            Some((text, _)) => text,
            None => format!("value #{}", short_hash(&crate::print::Printer::default().allocation(&alloc, 0))),
        },
        Err(_) => digest_text(&definition_digest(rustc_public::CrateDef::def_id(def), None)),
    }
}

/// The text and the size in bytes of a value of an integer, a `bool`, a `char`, or an array of
/// them at an offset of an allocation. `None` for each other type, and for a byte that is not
/// initialized or that has a pointer.
fn typed_value(ty: Ty, alloc: &rustc_public::ty::Allocation, offset: usize) -> Option<(String, usize)> {
    if let Some((w, s)) = int_of(ty) {
        let size = (w as usize).div_ceil(8);
        if alloc.provenance.ptrs.iter().any(|(at, _)| *at < offset + size && offset < *at + 8) {
            return None;
        }
        let v = alloc.read_partial_uint(offset..offset + size).ok()?;
        let text = match ty.kind() {
            TyKind::RigidTy(RigidTy::Bool) => (v != 0).to_string(),
            TyKind::RigidTy(RigidTy::Char) => char::from_u32(v as u32).map(|c| format!("{c:?}")).unwrap_or_else(|| v.to_string()),
            _ if s => signed(v & mask(w), w).to_string(),
            _ => (v & mask(w)).to_string(),
        };
        return Some((text, size));
    }
    let TyKind::RigidTy(RigidTy::Array(elem, len)) = ty.kind() else { return None };
    let count = len.eval_target_usize().ok()?;
    if count > MAX_STATIC_ELEMENTS {
        return None;
    }
    let mut parts = Vec::new();
    let mut at = offset;
    for _ in 0..count {
        let (text, size) = typed_value(elem, alloc, at)?;
        parts.push(text);
        at += size;
    }
    Some((format!("[{}]", parts.join(", ")), at - offset))
}
