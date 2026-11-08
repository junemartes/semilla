//! N3: bounded inlining of the crate's own functions.
//!
//! Inlining keeps the behavior the same: a call runs the body of the callee with the arguments,
//! and the inlined code runs the same statements in the same order. The inliner works on the
//! rustc_public body, before the N0 normalization.
//!
//! What the inliner inlines:
//!
//! - Only a direct call of a local function (a function item of this crate), never an external
//!   function, a trait method, a closure, or a call through a pointer.
//! - Only a callee without generic parameters, with the Rust ABI (another ABI can change what a
//!   panic does), without `#[track_caller]` (the caller location is a value that inlining can
//!   change), and with a normal return (the call has a return block).
//! - Never a recursive callee (a function in a cycle of the call graph).
//! - Only to depth 2, and only callees up to a size limit.
//!
//! How the inliner changes the body:
//!
//! - The callee locals become new locals of the caller.
//! - Each argument becomes a new local, with an assignment of the argument operand before the
//!   jump to the callee entry. One exception: if the argument operand is a plain local of the
//!   caller whose address the caller never takes, and the callee never writes or borrows the
//!   argument local, the callee reads the caller local directly. The callee cannot change that
//!   caller local (it has no pointer to it), so it has the same value for the whole callee body.
//!   A constant argument can replace the argument local in the same way, if each use of the
//!   argument local is a plain read.
//! - The return place of the callee is a new local, and each `return` becomes an assignment to
//!   the call destination and a jump to the return block of the call. One exception: if the
//!   destination is a plain local whose address the caller never takes, that is not an argument
//!   of the call, and that no cleanup path of the call uses, the callee writes the destination
//!   local directly. No code reads the destination between the call and the return, so an early
//!   write is not visible.
//! - An unwind edge of the callee to the caller (`unwind continue`, `resume`) becomes the unwind
//!   action of the call.

use std::collections::{HashMap, HashSet};

use rustc_public::CrateDef;
use rustc_public::mir::visit::{Location, MirVisitor, MutMirVisitor, PlaceContext};
use rustc_public::mir::{
    BasicBlock, Body, Local, LocalDecl, Mutability, Operand, Place, ProjectionElem, Rvalue,
    Statement, StatementKind, Terminator, TerminatorKind, UnwindAction, WithRetag,
};
use rustc_public::ty::{RigidTy, TyKind};

use crate::normalize::remap_targets;
use crate::term::address_taken_locals;

/// The inlining depth: a call in inlined code can be inlined once more.
pub const MAX_DEPTH: usize = 2;
/// The largest callee (blocks) that the inliner inlines.
pub const MAX_CALLEE_BLOCKS: usize = 64;
/// The largest callee (statements) that the inliner inlines.
pub const MAX_CALLEE_STATEMENTS: usize = 256;
/// The inliner stops when the body has this number of blocks.
const MAX_BODY_BLOCKS: usize = 2000;

/// The callees that the inliner can inline, by def path. The bodies are the N2 bodies.
pub struct Callees {
    pub bodies: HashMap<String, Body>,
}

/// Returns the def path of the local function that a call calls directly, if the call is a
/// direct call of a local function item without generic arguments.
pub fn direct_local_callee(kind: &TerminatorKind) -> Option<String> {
    let TerminatorKind::Call { func: Operand::Constant(constant), .. } = kind else {
        return None;
    };
    let TyKind::RigidTy(RigidTy::FnDef(def, args)) = constant.ty().kind() else {
        return None;
    };
    (def.krate().is_local && args.0.is_empty()).then(|| def.name())
}

/// Inlines the calls of a body. Returns the new body.
pub fn inline_calls(body: &Body, callees: &Callees) -> Body {
    let mut blocks = body.blocks.clone();
    let mut locals = body.locals().to_vec();
    let arg_count = body.arg_locals().len();
    let mut depth = vec![0usize; blocks.len()];

    let mut index = 0;
    while index < blocks.len() {
        if depth[index] < MAX_DEPTH && blocks.len() < MAX_BODY_BLOCKS {
            let callee = direct_local_callee(&blocks[index].terminator.kind)
                .and_then(|name| callees.bodies.get(&name));
            if let Some(callee) = callee {
                let current = Body::new(
                    blocks.clone(),
                    locals.clone(),
                    arg_count,
                    Vec::new(),
                    body.spread_arg(),
                    body.span,
                );
                if let Some((new_blocks, new_locals)) = inline_one(&current, index, callee) {
                    let added = new_blocks.len() - blocks.len();
                    blocks = new_blocks;
                    locals = new_locals;
                    depth.extend(std::iter::repeat_n(depth[index] + 1, added));
                }
            }
        }
        index += 1;
    }
    Body::new(blocks, locals, arg_count, Vec::new(), body.spread_arg(), body.span)
}

/// Inlines the call at the end of block `at`. Returns `None` if the call does not qualify.
fn inline_one(
    caller: &Body,
    at: usize,
    callee: &Body,
) -> Option<(Vec<BasicBlock>, Vec<LocalDecl>)> {
    let call = &caller.blocks[at].terminator;
    let TerminatorKind::Call { args, destination, target: Some(return_block), unwind, .. } =
        &call.kind
    else {
        return None;
    };
    // Only these two unwind actions have a simple meaning for the callee's unwind edges.
    if !matches!(unwind, UnwindAction::Continue | UnwindAction::Cleanup(_)) {
        return None;
    }
    let callee_args = callee.arg_locals().len();
    if callee.spread_arg().is_some() || callee_args != args.len() {
        return None;
    }

    let taken: HashSet<Local> = address_taken_locals(caller).into_iter().collect();
    let mut blocks = caller.blocks.clone();
    let mut locals = caller.locals().to_vec();
    let base_block = blocks.len();

    // The caller local or the constant that each callee local maps to.
    let mut map: Vec<Option<Local>> = vec![None; callee.locals().len()];
    let mut constants: Vec<Option<Operand>> = vec![None; callee.locals().len()];
    let mut arg_statements = Vec::new();

    // The return place.
    let cleanup_uses = match unwind {
        UnwindAction::Cleanup(start) => locals_used_from(caller, *start),
        _ => HashSet::new(),
    };
    let args_use = |local: Local| args.iter().any(|arg| operand_local(arg) == Some(local));
    let direct_return = destination.projection.is_empty()
        && !taken.contains(&destination.local)
        && !args_use(destination.local)
        && !cleanup_uses.contains(&destination.local);
    if direct_return {
        map[0] = Some(destination.local);
    }

    // The arguments.
    for (position, arg) in args.iter().enumerate() {
        let callee_local = position + 1;
        let moved = matches!(arg, Operand::Move(_));
        match arg {
            Operand::Copy(place) | Operand::Move(place)
                if place.projection.is_empty()
                    && !taken.contains(&place.local)
                    && place.local != destination.local
                    && is_read_only(callee, callee_local, moved) =>
            {
                map[callee_local] = Some(place.local);
            }
            Operand::Constant(_) if only_plain_reads(callee, callee_local) => {
                constants[callee_local] = Some(arg.clone());
            }
            _ => {}
        }
    }

    // New locals for the callee locals that do not map to a caller local.
    for (callee_local, decl) in callee.locals().iter().enumerate() {
        if map[callee_local].is_none() && constants[callee_local].is_none() {
            map[callee_local] = Some(locals.len());
            locals.push(LocalDecl { ty: decl.ty, span: decl.span, mutability: Mutability::Mut });
        }
    }
    for (position, arg) in args.iter().enumerate() {
        let callee_local = position + 1;
        let caller_local = map[callee_local];
        let is_new = caller_local.is_some_and(|local| local >= caller.locals().len());
        if is_new {
            arg_statements.push(Statement {
                kind: StatementKind::Assign(
                    Place::from(caller_local.unwrap()),
                    Rvalue::Use(arg.clone(), WithRetag::Yes),
                ),
                source_info: call.source_info.clone(),
            });
        }
    }

    // Copy the callee blocks, with the locals and the blocks renumbered.
    let mut copy = callee.clone();
    let mut remap =
        Remap { map: map.iter().map(|local| local.unwrap_or(usize::MAX)).collect(), constants };
    remap.visit_body(&mut copy);
    let return_local = map[0].expect("the return place has a local");
    for mut block in copy.blocks {
        let kind = &mut block.terminator.kind;
        match kind {
            TerminatorKind::Return => {
                if !direct_return {
                    block.statements.push(Statement {
                        kind: StatementKind::Assign(
                            destination.clone(),
                            Rvalue::Use(Operand::Move(Place::from(return_local)), WithRetag::Yes),
                        ),
                        source_info: block.terminator.source_info.clone(),
                    });
                }
                *kind = TerminatorKind::Goto { target: *return_block };
            }
            TerminatorKind::Resume => {
                *kind = match unwind {
                    UnwindAction::Cleanup(start) => TerminatorKind::Goto { target: *start },
                    _ => TerminatorKind::Resume,
                };
            }
            _ => {
                remap_targets(kind, |target| target + base_block);
                if let Some(callee_unwind) = unwind_mut(kind) {
                    if *callee_unwind == UnwindAction::Continue {
                        *callee_unwind = *unwind;
                    }
                }
            }
        }
        blocks.push(block);
    }

    // The call block runs the argument assignments and jumps to the callee entry.
    let call_block = &mut blocks[at];
    call_block.statements.extend(arg_statements);
    call_block.terminator = Terminator {
        kind: TerminatorKind::Goto { target: base_block },
        source_info: call.source_info.clone(),
    };
    Some((blocks, locals))
}

fn unwind_mut(kind: &mut TerminatorKind) -> Option<&mut UnwindAction> {
    match kind {
        TerminatorKind::Call { unwind, .. }
        | TerminatorKind::Assert { unwind, .. }
        | TerminatorKind::Drop { unwind, .. }
        | TerminatorKind::InlineAsm { unwind, .. } => Some(unwind),
        _ => None,
    }
}

fn operand_local(operand: &Operand) -> Option<Local> {
    match operand {
        Operand::Copy(place) | Operand::Move(place) => Some(place.local),
        _ => None,
    }
}

fn has_deref(place: &Place) -> bool {
    place.projection.iter().any(|elem| matches!(elem, ProjectionElem::Deref))
}

/// Returns true if the callee never writes or borrows the storage of a local. A write or a
/// borrow through a pointer that the local holds is allowed: it does not change the local. A
/// drop is allowed if the argument was moved: the callee owns the value, and the caller does not
/// use a moved value again.
fn is_read_only(body: &Body, local: Local, moved: bool) -> bool {
    let own = |place: &Place| place.local == local && !has_deref(place);
    for block in &body.blocks {
        for statement in &block.statements {
            match &statement.kind {
                StatementKind::Assign(place, rvalue) => {
                    if own(place) {
                        return false;
                    }
                    if let Rvalue::Ref(_, _, place)
                    | Rvalue::AddressOf(_, place)
                    | Rvalue::Reborrow(_, _, place) = rvalue
                    {
                        if own(place) {
                            return false;
                        }
                    }
                }
                StatementKind::SetDiscriminant { place, .. } if own(place) => return false,
                _ => {}
            }
        }
        match &block.terminator.kind {
            TerminatorKind::Call { destination, .. } if own(destination) => return false,
            TerminatorKind::Drop { place, .. } if own(place) && !moved => return false,
            TerminatorKind::InlineAsm { operands, .. }
                if operands.iter().any(|operand| operand.out_place.as_ref().is_some_and(own)) =>
            {
                return false;
            }
            _ => {}
        }
    }
    true
}

/// Returns true if each use of a local is a plain read of the whole local (`copy _n` or
/// `move _n`), so that a constant can replace it.
fn only_plain_reads(body: &Body, local: Local) -> bool {
    struct Uses {
        local: Local,
        other_use: bool,
    }
    impl MirVisitor for Uses {
        fn visit_operand(&mut self, operand: &Operand, location: Location) {
            if let Operand::Copy(place) | Operand::Move(place) = operand {
                if place.local == self.local && place.projection.is_empty() {
                    return;
                }
            }
            self.super_operand(operand, location);
        }
        fn visit_local(&mut self, local: &Local, _ptx: PlaceContext, _location: Location) {
            if *local == self.local {
                self.other_use = true;
            }
        }
    }
    let mut uses = Uses { local, other_use: false };
    for block in &body.blocks {
        uses.visit_basic_block(block);
    }
    !uses.other_use
}

/// Returns the locals that the blocks reachable from `start` use.
fn locals_used_from(body: &Body, start: usize) -> HashSet<Local> {
    struct Used(HashSet<Local>);
    impl MirVisitor for Used {
        fn visit_local(&mut self, local: &Local, _ptx: PlaceContext, _location: Location) {
            self.0.insert(*local);
        }
    }
    let mut used = Used(HashSet::new());
    let mut seen = vec![false; body.blocks.len()];
    let mut stack = vec![start];
    while let Some(block) = stack.pop() {
        if std::mem::replace(&mut seen[block], true) {
            continue;
        }
        used.visit_basic_block(&body.blocks[block]);
        stack.extend(body.blocks[block].terminator.successors());
    }
    used.0
}

/// Renumbers the callee locals to caller locals, and puts constant arguments in place.
struct Remap {
    map: Vec<Local>,
    constants: Vec<Option<Operand>>,
}

impl MutMirVisitor for Remap {
    fn visit_operand(&mut self, operand: &mut Operand, location: Location) {
        if let Operand::Copy(place) | Operand::Move(place) = operand {
            if place.projection.is_empty() {
                if let Some(constant) = &self.constants[place.local] {
                    *operand = constant.clone();
                    return;
                }
            }
        }
        self.super_operand(operand, location);
    }

    fn visit_local(&mut self, local: &mut Local, _ptx: PlaceContext, _location: Location) {
        *local = self.map[*local];
    }
}
