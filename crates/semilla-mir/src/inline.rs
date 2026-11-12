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
//!   caller, and the callee never writes the argument local and borrows it only with shared
//!   borrows of a `Freeze` type (see `is_read_only`), the callee reads the caller local
//!   directly. The caller local must have no borrow at all, or only shared borrows of a
//!   `Freeze` type: then no pointer can change it, the callee cannot name it, and the caller
//!   code does not run while the callee runs. So it has the same value for the whole callee
//!   body.
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
    BasicBlock, Body, BorrowKind, Local, LocalDecl, Mutability, Operand, Place, ProjectionElem,
    Rvalue, Statement, StatementKind, Terminator, TerminatorKind, UnwindAction, WithRetag,
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
    /// The callees whose N2 body has no cycle in its control flow graph.
    pub loop_free: std::collections::HashSet<String>,
}

/// Which callees an inlining run inlines.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// All callees (the N3 text).
    All,
    /// Only the loop-free callees, so a loop-free caller stays loop-free (the N3 term).
    LoopFree,
    /// Only the loop-free callees with at most `MAX_SMALL_CALLEE_BLOCKS` blocks (the second
    /// try for the N3 term, when the paths of a large callee are too many).
    SmallLoopFree,
    /// Only the loop-free callees with at most `MAX_TINY_CALLEE_BLOCKS` blocks (the third try).
    TinyLoopFree,
}

/// The largest callee (blocks) of `Scope::SmallLoopFree`.
pub const MAX_SMALL_CALLEE_BLOCKS: usize = 16;
/// The largest callee (blocks) of `Scope::TinyLoopFree`. A helper that returns a bool from a
/// short circuit (`a && b`) has about 5 blocks.
pub const MAX_TINY_CALLEE_BLOCKS: usize = 6;

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

/// Inlines the calls of a body. Returns the new body, or `None` if no call qualifies (then the
/// N3 body is the N2 body).
pub fn inline_calls(body: &Body, callees: &Callees, scope: Scope) -> Option<Body> {
    inline_calls_named(body, callees, scope, &mut Vec::new())
}

/// `inline_calls`, and the name of each callee that it inlined goes into `names`.
pub fn inline_calls_named(body: &Body, callees: &Callees, scope: Scope, names: &mut Vec<String>) -> Option<Body> {
    let mut blocks = body.blocks.clone();
    let mut locals = body.locals().to_vec();
    let arg_count = body.arg_locals().len();
    let mut depth = vec![0usize; blocks.len()];
    let mut inlined = false;
    // The locals whose address the body takes. `inline_one` keeps this up to date.
    let mut taken = vec![false; locals.len()];
    for local in address_taken_locals(body) {
        taken[local] = true;
    }
    // The locals with a borrow that is not a shared borrow.
    let mut mut_taken = vec![false; locals.len()];
    for local in mutably_borrowed_locals(body) {
        mut_taken[local] = true;
    }

    let mut index = 0;
    while index < blocks.len() {
        if depth[index] < MAX_DEPTH && blocks.len() < MAX_BODY_BLOCKS {
            let callee = direct_local_callee(&blocks[index].terminator.kind)
                .filter(|name| scope == Scope::All || callees.loop_free.contains(name))
                .and_then(|name| callees.bodies.get_key_value(&name))
                .filter(|(_, callee)| match scope {
                    Scope::SmallLoopFree => callee.blocks.len() <= MAX_SMALL_CALLEE_BLOCKS,
                    Scope::TinyLoopFree => callee.blocks.len() <= MAX_TINY_CALLEE_BLOCKS,
                    Scope::All | Scope::LoopFree => true,
                });
            if let Some((name, callee)) = callee {
                let before = blocks.len();
                let borrows = Borrows { taken: &mut taken, mut_taken: &mut mut_taken };
                if inline_one(&mut blocks, &mut locals, borrows, index, callee) {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                    let added = blocks.len() - before;
                    depth.extend(std::iter::repeat_n(depth[index] + 1, added));
                    inlined = true;
                }
            }
        }
        index += 1;
    }
    inlined.then(|| Body::new(blocks, locals, arg_count, Vec::new(), body.spread_arg(), body.span))
}

/// Inlines the call at the end of block `at`, in place. Returns false (and changes nothing) if
/// the call does not qualify. `borrows` holds the locals whose address the body takes.
fn inline_one(
    blocks: &mut Vec<BasicBlock>,
    locals: &mut Vec<LocalDecl>,
    borrows: Borrows<'_>,
    at: usize,
    callee: &Body,
) -> bool {
    let call = blocks[at].terminator.clone();
    let TerminatorKind::Call { args, destination, target: Some(return_block), unwind, .. } =
        &call.kind
    else {
        return false;
    };
    // Only these two unwind actions have a simple meaning for the callee's unwind edges.
    if !matches!(unwind, UnwindAction::Continue | UnwindAction::Cleanup(_)) {
        return false;
    }
    let callee_args = callee.arg_locals().len();
    if callee.spread_arg().is_some() || callee_args != args.len() {
        return false;
    }

    let old_local_count = locals.len();
    let base_block = blocks.len();

    // The caller local or the constant that each callee local maps to.
    let mut map: Vec<Option<Local>> = vec![None; callee.locals().len()];
    let mut constants: Vec<Option<Operand>> = vec![None; callee.locals().len()];
    let mut arg_statements = Vec::new();

    // The return place.
    let cleanup_uses = match unwind {
        UnwindAction::Cleanup(start) => locals_used_from(blocks, *start),
        _ => HashSet::new(),
    };
    let args_use = |local: Local| args.iter().any(|arg| operand_local(arg) == Some(local));
    let direct_return = destination.projection.is_empty()
        && !borrows.taken[destination.local]
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
                    && (!borrows.taken[place.local]
                        || (!borrows.mut_taken[place.local]
                            && is_freeze(locals[place.local].ty)))
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
            borrows.taken.push(false);
            borrows.mut_taken.push(false);
        }
    }
    for (position, arg) in args.iter().enumerate() {
        let callee_local = position + 1;
        let caller_local = map[callee_local];
        let is_new = caller_local.is_some_and(|local| local >= old_local_count);
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

    // A local whose address the callee takes is now a caller local whose address the body takes.
    for local in address_taken_locals(callee) {
        if let Some(mapped) = map[local] {
            borrows.taken[mapped] = true;
        }
    }
    for local in mutably_borrowed_locals(callee) {
        if let Some(mapped) = map[local] {
            borrows.mut_taken[mapped] = true;
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
    true
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

/// Returns true if the callee never writes the storage of a local, and never borrows it except
/// with a shared borrow of a `Freeze` type. A write or a borrow through a pointer that the local
/// holds is allowed: it does not change the local. A drop is allowed if the argument was moved:
/// the callee owns the value, and the caller does not use a moved value again.
///
/// A shared borrow of a `Freeze` value (no `UnsafeCell` inside) cannot change the value, and the
/// reference cannot live longer than the callee (the argument local dies at the return). The
/// caller code does not run while the callee runs. So the borrowed caller local and a copy of it
/// hold the same value for the whole life of the reference. Only the address differs, and the
/// address of a stack local is not known before the run, so no program can depend on which one
/// it is. The argument type of a shared borrow must be `Freeze`.
fn is_read_only(body: &Body, local: Local, moved: bool) -> bool {
    let own = |place: &Place| place.local == local && !has_deref(place);
    let freeze = is_freeze(body.locals()[local].ty);
    for block in &body.blocks {
        for statement in &block.statements {
            match &statement.kind {
                StatementKind::Assign(place, rvalue) => {
                    if own(place) {
                        return false;
                    }
                    if let Rvalue::Ref(_, BorrowKind::Shared, place) = rvalue {
                        if own(place) && !freeze {
                            return false;
                        }
                    } else if let Rvalue::Ref(_, _, place)
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

/// The borrowed locals of the body that the inliner changes.
struct Borrows<'a> {
    /// The locals with any borrow (shared, mutable, raw).
    taken: &'a mut Vec<bool>,
    /// The locals with a borrow that is not shared (mutable, raw, fake).
    mut_taken: &'a mut Vec<bool>,
}

/// Returns the locals with a borrow of their storage that is not a shared borrow.
fn mutably_borrowed_locals(body: &Body) -> Vec<Local> {
    let mut found = Vec::new();
    for block in &body.blocks {
        for statement in &block.statements {
            let StatementKind::Assign(_, rvalue) = &statement.kind else { continue };
            let place = match rvalue {
                Rvalue::Ref(_, BorrowKind::Shared, _) => continue,
                Rvalue::Ref(_, _, place)
                | Rvalue::AddressOf(_, place)
                | Rvalue::Reborrow(_, _, place) => place,
                _ => continue,
            };
            if !has_deref(place) {
                found.push(place.local);
            }
        }
    }
    found
}

/// Returns true if a type has no interior mutability (`UnsafeCell`), so a shared reference
/// cannot change a value of it. A generic or unknown type gives false.
fn is_freeze(ty: rustc_public::ty::Ty) -> bool {
    rustc_middle::ty::tls::with(|tcx| {
        let ty = rustc_public::rustc_internal::internal(tcx, ty);
        use rustc_middle::ty::TypeVisitableExt;
        !ty.has_param() && ty.is_freeze(tcx, rustc_middle::ty::TypingEnv::fully_monomorphized())
    })
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
fn locals_used_from(blocks: &[BasicBlock], start: usize) -> HashSet<Local> {
    struct Used(HashSet<Local>);
    impl MirVisitor for Used {
        fn visit_local(&mut self, local: &Local, _ptx: PlaceContext, _location: Location) {
            self.0.insert(*local);
        }
    }
    let mut used = Used(HashSet::new());
    let mut seen = vec![false; blocks.len()];
    let mut stack = vec![start];
    while let Some(block) = stack.pop() {
        if std::mem::replace(&mut seen[block], true) {
            continue;
        }
        used.visit_basic_block(&blocks[block]);
        stack.extend(blocks[block].terminator.successors());
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
