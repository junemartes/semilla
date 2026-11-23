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
}
