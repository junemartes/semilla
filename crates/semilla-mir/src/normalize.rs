//! N0 normalization of a MIR body.
//!
//! Each rule keeps the runtime behavior the same. So if two bodies have the same canonical text,
//! they have the same behavior. Keep this list of rules small: a rule that is too strong can hide
//! a real change.
//!
//! The rules:
//! 1. Remove the statements that have no runtime effect (storage markers, borrow check marks).
//! 2. Remove the debug information (the names of user variables).
//! 3. Jump over empty blocks that contain only a `goto`.
//! 3b. Merge a block that ends with a `goto` with its target, if the target has no other
//!    reachable predecessor (the unwind edges count). The code runs in the same order, so the behavior is
//!    the same. The N3 inliner needs this rule: it adds a `goto` into the callee entry.
//! 3c. Forward a copy along each path (design 46): replace each use of `_t` after
//!    `_t = copy p` (or `move p`) with a read of `p`, and remove the copy when no use of `_t`
//!    stays. A use is an operand that reads `_t` with no projection (`copy _t`, `move _t`).
//!    Each other mention of `_t` must be a use: a place, a borrow, or a drop of `_t` stops the
//!    rule. (Before fix round 1 of design 46, `_t` needed exactly one use.) The conditions, for
//!    each use:
//!    1. The copy runs before each run of the use: the block of the copy dominates the block of
//!       the use (the unwind edges count), or the copy is before the use in one block.
//!    2. No point on a path from the copy to the use writes the local of `p`, names it in a
//!       call or a drop, or is inline assembly. A path starts after the copy, ends before the
//!       use, and does not pass the copy again. A path can pass the use and come back to it (a
//!       loop that does not run the copy): then each point of the block of the use is on a
//!       path. No cleanup block goes back to a block that is not a cleanup block, so a cleanup
//!       block is on no path to such a use.
//!    3. No statement that takes the address of a place of the local of `p` or of `_t` (a borrow
//!       or a raw address with no deref) is at a point from which a path goes to the use.
//!    4. `p` is a local or fields of a local (with a downcast), so it names one fixed place.
//!    5. In a coroutine body, the copy and the use are in one block. (This MIR has no `yield`
//!       terminator; inline assembly is in condition 2.)
//!    6. No move of `_t` is on a path from the copy to the use (fix round 1).
//!    The proof: take a run of the use, and the last run of the copy before it (condition 1).
//!    The run from the copy to the use is a path of condition 2, so no statement and no
//!    terminator of it writes the local of `p` by its name. Each point that ran before the use
//!    has a path to the use (the run itself), so by condition 3 no statement before the use
//!    took the address of the local: no pointer to it exists, and no write through a pointer,
//!    no callee, and no drop can change it. A call or a drop that names the local is not on the
//!    path (condition 2). So `p` has the same value at the use as at the copy. The copy is the
//!    only write of `_t` (each other mention is a use), and no move of `_t` ends its value on
//!    the run (condition 6), so `_t` also has the value of the copy at the use, and the use
//!    reads the same value as before. A forwarded use reads `p` and writes nothing, so it does
//!    not change the value at an other use. Each use decides on the body before the forward
//!    of its copy: a use that becomes `f(copy p)` gives the callee a copy, not a pointer, so it
//!    cannot change `p` for an other use of the same copy. The copy goes away only when no use
//!    of `_t` stays, so no code reads `_t` after that, and the copy has no other effect. Two
//!    uses of `_t` on one path are possible only for a `Copy` type: the MIR builder reads a
//!    place of a type that is not `Copy` with `move`, and the borrow checker rejects a read
//!    after a move. A type that is not `Copy` can have two moves only on separate paths (the
//!    arms of a `match`); then no path goes from one move to the other use, and condition 6
//!    holds. The rule does not depend on this: condition 6 checks it. The cause of fix round
//!    1: with one use only, an added or deleted read of a binding (`note(s);`) changed the
//!    count of the mentions, and the forward of the kept use happened on one side only. The
//!    normal form has no storage markers (rule 1), and a move
//!    reads the same value as a copy (rule 8), so a local keeps its value until a write. A
//!    borrow after the use that no path takes back to the use does not stop the rule: a pointer
//!    that a later statement makes cannot change a value that the use read before. The N3
//!    inliner needs this rule, because the inlined code and the original code copy values in
//!    different places. Before design 46, the rule needed one block, and a borrow anywhere in
//!    the body stopped it: an added statement with a block end (an overflow check, a bounds
//!    check, a call, a `?`, an `if`) between a binding and its use changed the text of the
//!    unchanged statements.
//! 3c2. Forward a copy across a call (batch 5u): since design 46, a part of rule 3c. A call on a
//!    path that names neither local cannot change `p`: the callee gets no pointer to it. The
//!    source text `i < v.len()` and `v.len() > i` differ only in the place of this copy.
//! 3d. Forward a reference: replace each `(*r)...` with `P...` when the only assignments to the
//!    local `r` are `r = &P` (shared or mutable) with the same place `P`, `P` is a local or
//!    fields of a local (no deref and no index, so it names one fixed place), and each other use
//!    of `r` is a deref. Then `(*r)` and `P` name the same memory, so each read and each write
//!    is the same, and the assignments to `r` have no use left. A use of `r` that is not a deref
//!    (a copy, a move into a call, a borrow of `r`) stops the rule. The N3 inliner needs this
//!    rule for a callee with a `&mut` parameter: the inlined code writes through the reference,
//!    and the original code writes the local. The aliasing model (retags, stacked borrows) is not
//!    behavior here, as in the rest of the normalization.
//! 3e. Use `Eq` for a branch on `Ne` (batch 5u): replace `_t = Ne(a, b); switchInt(_t) -> [0: X,
//!    otherwise: Y]` with `_t = Eq(a, b); switchInt(_t) -> [0: Y, otherwise: X]` when the
//!    assignment is the last statement of the block and `_t` has no other mention in the body.
//!    The MIR operators `Eq` and `Ne` are only for the primitive types (integers, `bool`, `char`,
//!    floats, pointers), and for each of them `Ne(a, b)` is `!Eq(a, b)`, also for a NaN. A
//!    comparison with a `PartialEq` impl is a call, and the rule does not change a call. `_t`
//!    is a `bool`: the switch goes to the block of 0 for `false`, and to the other block for
//!    `true`. So the new switch goes to the same block as the old switch for each `a` and `b`,
//!    and no other code reads `_t`. The source text `a == b` and `!(a != b)` in a condition
//!    differ only in this operator and in the order of the two blocks.
//! 4. Remove the blocks that the entry cannot reach.
//! 5. Order the blocks by a depth-first walk from the entry.
//! 6. Number the locals in the order of their first use. The return local and the arguments keep
//!    their numbers. Remove the locals that the body does not use.
//! 7. Remove the mutability of the locals (`let` and `let mut` give the same runtime code).
//! 8. Treat a `move` operand like a copy. Both read the same value, and the drops are already
//!    explicit in this MIR, so a move does not change when a value is dropped.
//! 9. Replace the source location in the names of closure and coroutine types with a number, in
//!    the order of first appearance in the body.
//!
//! The normalization keeps all drops, because the drop order is observable in Rust.

use std::collections::HashMap;

use rustc_public::mir::visit::{Location, MirVisitor, MutMirVisitor, PlaceContext};
use rustc_public::mir::{
    BasicBlock, BasicBlockIdx, BinOp, Body, Local, LocalDecl, Mutability, Operand, Place,
    ProjectionElem, Rvalue, Statement, StatementKind, SwitchTargets, Terminator, TerminatorKind,
    UnwindAction,
};

use rustc_public::ty::{GenericArgKind, RigidTy, Ty, TyKind};

use crate::print::Printer;

/// The canonical form of a body: its text and the local functions that it refers to.
pub struct Canonical {
    pub text: String,
    pub callees: Vec<String>,
    /// The uses of the build configuration in the body (see `config.rs`), sorted.
    pub config: Vec<String>,
}

/// Returns the canonical text of a body that `canonical_body` made. Two bodies with the same
/// text have the same behavior.
pub fn canonical_text(body: &Body) -> Canonical {
    let mut printer = Printer::default();
    let text = printer.body(body);
    let mut config = std::mem::take(&mut printer.config);
    config.sort();
    Canonical { text: stable_closure_names(&text), callees: printer.callees.into_iter().collect(), config }
}

/// Returns the shape of the control flow graph: the kind of each terminator and its successors,
/// without the statements and without the operands. Two bodies with the same shape can still
/// have a different logic, but a different shape always means a change in the control flow.
pub fn shape(body: &Body) -> String {
    let mut out = String::new();
    for (index, block) in body.blocks.iter().enumerate() {
        let kind = &block.terminator.kind;
        let head = match kind {
            TerminatorKind::Goto { .. } => "goto".to_string(),
            TerminatorKind::SwitchInt { targets, .. } => {
                let values: Vec<String> =
                    targets.branches().map(|(value, _)| value.to_string()).collect();
                format!("switch[{}]", values.join(","))
            }
            TerminatorKind::Resume => "resume".to_string(),
            TerminatorKind::Abort => "abort".to_string(),
            TerminatorKind::Return => "return".to_string(),
            TerminatorKind::Unreachable => "unreachable".to_string(),
            TerminatorKind::Drop { .. } => "drop".to_string(),
            TerminatorKind::Call { target, .. } => {
                if target.is_some() {
                    "call".to_string()
                } else {
                    "call!".to_string()
                }
            }
            TerminatorKind::Assert { .. } => "assert".to_string(),
            TerminatorKind::InlineAsm { .. } => "asm".to_string(),
        };
        let unwind = match kind.unwind() {
            Some(UnwindAction::Cleanup(target)) => format!(" u{target}"),
            Some(UnwindAction::Continue) | None => String::new(),
            Some(UnwindAction::Terminate) => " ut".to_string(),
            Some(UnwindAction::Unreachable) => " uu".to_string(),
        };
        let normal: Vec<String> =
            normal_successors(kind).iter().map(|target| target.to_string()).collect();
        if index > 0 {
            out.push(';');
        }
        out.push_str(&format!("{index}:{head}->{}{unwind}", normal.join(",")));
    }
    out
}

/// Returns the successors of a terminator without the unwind target.
fn normal_successors(kind: &TerminatorKind) -> Vec<BasicBlockIdx> {
    match kind {
        TerminatorKind::Goto { target } => vec![*target],
        TerminatorKind::SwitchInt { targets, .. } => targets.all_targets(),
        TerminatorKind::Drop { target, .. } | TerminatorKind::Assert { target, .. } => {
            vec![*target]
        }
        TerminatorKind::Call { target, .. } => target.iter().copied().collect(),
        TerminatorKind::InlineAsm { destination, .. } => destination.iter().copied().collect(),
        TerminatorKind::Resume
        | TerminatorKind::Abort
        | TerminatorKind::Return
        | TerminatorKind::Unreachable => Vec::new(),
    }
}

/// Returns true if the control flow graph has no cycle. The unwind edges count.
pub fn is_loop_free(body: &Body) -> bool {
    // 0: not visited, 1: on the current path, 2: done.
    let mut state = vec![0u8; body.blocks.len()];
    let mut stack: Vec<(BasicBlockIdx, usize)> = vec![(0, 0)];
    state[0] = 1;
    while let Some((block, next)) = stack.pop() {
        let successors = body.blocks[block].terminator.successors();
        if next < successors.len() {
            stack.push((block, next + 1));
            let successor = successors[next];
            match state[successor] {
                0 => {
                    state[successor] = 1;
                    stack.push((successor, 0));
                }
                1 => return false,
                _ => {}
            }
        } else {
            state[block] = 2;
        }
    }
    true
}

/// Replaces names such as `{closure@src/lib.rs:2:18: 2:21}` with `{closure#0}`.
///
/// The same original name always gets the same number, so two different closures in one body
/// stay different.
pub fn stable_closure_names(text: &str) -> String {
    let mut numbers: HashMap<String, usize> = HashMap::new();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let candidate = &rest[start..];
        // The kind is a short word or phrase before the `@`, such as `closure` or `async block`.
        let kind_end = candidate[1..]
            .find(|c: char| !(c.is_ascii_alphabetic() || c == ' ' || c == '-'))
            .map(|end| end + 1);
        match (kind_end, candidate.find('}')) {
            (Some(kind_end), Some(close))
                if kind_end > 1 && candidate[kind_end..].starts_with('@') =>
            {
                let original = &candidate[..=close];
                let next = numbers.len();
                let number = *numbers.entry(original.to_string()).or_insert(next);
                out.push_str(&format!("{{{}#{number}}}", &candidate[1..kind_end]));
                rest = &candidate[close + 1..];
            }
            _ => {
                out.push('{');
                rest = &candidate[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Returns a new body with the N0 rules applied.
pub fn canonical_body(body: &Body) -> Body {
    canonical_body_with_order(body).0
}

/// Returns a new body with the N0 rules applied, and the number of each of its locals in `body`
/// (rule 6 numbers the locals again; design 45 records the declaration of each local).
pub fn canonical_body_with_order(body: &Body) -> (Body, Vec<Local>) {
    let arg_count = body.arg_locals().len();

    let mut blocks = body.blocks.clone();
    for block in &mut blocks {
        block.statements.retain(|statement| has_runtime_effect(&statement.kind));
    }
    forward_references(&mut blocks, arg_count, body.locals().len());
    skip_empty_gotos(&mut blocks);
    merge_straight_blocks(&mut blocks);
    forward_copies(&mut blocks, arg_count, body.locals().len(), is_coroutine_body(body));
    branch_on_eq(&mut blocks, arg_count, body.locals().len());
    let blocks = order_blocks(blocks);

    // The visitors need a body, so build one with the original locals first.
    let ordered = Body::new(
        blocks,
        body.locals().to_vec(),
        arg_count,
        Vec::new(),
        body.spread_arg(),
        body.span,
    );
    let locals = local_order(&ordered, arg_count);
    let body = renumber_locals(ordered, &locals, arg_count);
    (body, locals)
}

/// True if the body is the body of a coroutine (an `async` body or a `yield` body): an argument
/// is the coroutine, behind `Pin` or a reference. Rule 3c keeps one block there (design 46).
fn is_coroutine_body(body: &Body) -> bool {
    fn holds(ty: Ty, depth: usize) -> bool {
        match ty.kind() {
            TyKind::RigidTy(RigidTy::Coroutine(..)) => true,
            _ if depth == 0 => false,
            TyKind::RigidTy(RigidTy::Ref(_, inner, _) | RigidTy::RawPtr(inner, _)) => holds(inner, depth - 1),
            TyKind::RigidTy(RigidTy::Adt(_, args)) => args
                .0
                .iter()
                .any(|arg| matches!(arg, GenericArgKind::Type(inner) if holds(*inner, depth - 1))),
            _ => false,
        }
    }
    body.arg_locals().iter().any(|decl| holds(decl.ty, 3))
}

/// Returns false for the statements that have no effect on the runtime behavior.
fn has_runtime_effect(kind: &StatementKind) -> bool {
    !matches!(
        kind,
        StatementKind::StorageLive(_)
            | StatementKind::StorageDead(_)
            | StatementKind::Nop
            | StatementKind::ConstEvalCounter
            | StatementKind::Coverage(_)
            | StatementKind::FakeRead(..)
            | StatementKind::PlaceMention(_)
            | StatementKind::AscribeUserType { .. }
    )
}

/// Changes each jump to an empty `goto` block into a jump to the end of the `goto` chain.
fn skip_empty_gotos(blocks: &mut [BasicBlock]) {
    let resolved: Vec<BasicBlockIdx> =
        (0..blocks.len()).map(|index| goto_chain_end(blocks, index)).collect();
    // The entry block stays the entry block, so it is not resolved.
    for block in blocks.iter_mut() {
        remap_targets(&mut block.terminator.kind, |target| resolved[target]);
    }
}

/// Merges each block that ends with a `goto` with its target, if the target has no other
/// predecessor and is not the entry. Only the blocks that the entry can reach count as
/// predecessors: rule 3 leaves empty `goto` blocks that nothing reaches. The merged target
/// becomes unreachable, and `order_blocks` removes it.
fn merge_straight_blocks(blocks: &mut [BasicBlock]) {
    let reachable = reachable_blocks(blocks);
    let mut predecessors = vec![0usize; blocks.len()];
    predecessors[0] = 1;
    for (index, block) in blocks.iter().enumerate() {
        if reachable[index] {
            for successor in block.terminator.successors() {
                predecessors[successor] += 1;
            }
        }
    }
    // A merge moves the edges of the target to `from`, so the counts stay right.
    for from in 0..blocks.len() {
        if !reachable[from] {
            continue;
        }
        while let TerminatorKind::Goto { target } = blocks[from].terminator.kind {
            if target == from || target == 0 || predecessors[target] != 1 {
                break;
            }
            let statements = std::mem::take(&mut blocks[target].statements);
            let terminator = blocks[target].terminator.clone();
            blocks[target].terminator.kind = TerminatorKind::Unreachable;
            blocks[from].statements.extend(statements);
            blocks[from].terminator = terminator;
        }
    }
}

/// Returns the blocks that the entry can reach (the unwind edges count).
fn reachable_blocks(blocks: &[BasicBlock]) -> Vec<bool> {
    let mut seen = vec![false; blocks.len()];
    let mut stack = vec![0];
    while let Some(block) = stack.pop() {
        if std::mem::replace(&mut seen[block], true) {
            continue;
        }
        stack.extend(blocks[block].terminator.successors());
    }
    seen
}

/// Applies rule 3d: replaces each `(*r)...` with `P...` for a reference local `r` whose only
/// assignments are `r = &P` (or `&mut P`) with the same `P`, and whose only other uses are
/// derefs. `P` is a local or fields of a local (no deref, no index), so it names one fixed
/// place, and `(*r)` names the same place. Then the assignments to `r` have no use, and the
/// rule removes them.
fn forward_references(blocks: &mut [BasicBlock], arg_count: usize, local_count: usize) {
    #[derive(Clone, Default)]
    struct Uses {
        /// The borrowed place of the assignments `r = &P`.
        source: Option<Place>,
        /// A use that is not a deref, or two different borrowed places.
        bad: bool,
    }
    struct Scan<'a>(&'a mut [Uses]);
    impl MirVisitor for Scan<'_> {
        fn visit_place(&mut self, place: &Place, _ptx: PlaceContext, _location: Location) {
            if place.projection.first() != Some(&ProjectionElem::Deref) {
                self.0[place.local].bad = true;
            }
            for elem in &place.projection {
                if let ProjectionElem::Index(local) = elem {
                    self.0[*local].bad = true;
                }
            }
        }
        fn visit_local(&mut self, local: &Local, _ptx: PlaceContext, _location: Location) {
            // A use outside a place (the return of `_0`).
            self.0[*local].bad = true;
        }
    }
    let simple = |place: &Place| {
        place
            .projection
            .iter()
            .all(|elem| matches!(elem, ProjectionElem::Field(..) | ProjectionElem::Downcast(_)))
    };

    let mut uses = vec![Uses::default(); local_count];
    for block in blocks.iter() {
        for statement in &block.statements {
            if let StatementKind::Assign(target, Rvalue::Ref(_, _, source)) = &statement.kind {
                if target.projection.is_empty()
                    && target.local > arg_count
                    && simple(source)
                    && source.local != target.local
                {
                    let entry = &mut uses[target.local];
                    match &entry.source {
                        Some(old) if old != source => entry.bad = true,
                        _ => entry.source = Some(source.clone()),
                    }
                    // The borrowed place is a use of its own local (not a deref).
                    uses[source.local].bad = true;
                    continue;
                }
            }
            let single = BasicBlock {
                statements: vec![statement.clone()],
                terminator: Terminator {
                    kind: TerminatorKind::Unreachable,
                    source_info: statement.source_info.clone(),
                },
            };
            Scan(&mut uses).visit_basic_block(&single);
        }
        let single = BasicBlock { statements: Vec::new(), terminator: block.terminator.clone() };
        Scan(&mut uses).visit_basic_block(&single);
    }
    let forward: Vec<Option<Place>> =
        uses.into_iter().map(|entry| if entry.bad { None } else { entry.source }).collect();
    if forward.iter().all(Option::is_none) {
        return;
    }

    struct Rewrite<'a>(&'a [Option<Place>]);
    impl MutMirVisitor for Rewrite<'_> {
        fn visit_place(&mut self, place: &mut Place, ptx: PlaceContext, location: Location) {
            if let Some(source) = &self.0[place.local] {
                if place.projection.first() == Some(&ProjectionElem::Deref) {
                    let mut projection = source.projection.clone();
                    projection.extend(place.projection.drain(1..));
                    *place = Place { local: source.local, projection };
                }
            }
            self.super_place(place, ptx, location);
        }
    }
    for block in blocks.iter_mut() {
        block.statements.retain(|statement| {
            !matches!(&statement.kind, StatementKind::Assign(target, Rvalue::Ref(..))
                if target.projection.is_empty() && forward[target.local].is_some())
        });
        let mut rewrite = Rewrite(&forward);
        for statement in &mut block.statements {
            let mut single = BasicBlock {
                statements: vec![statement.clone()],
                terminator: Terminator {
                    kind: TerminatorKind::Unreachable,
                    source_info: statement.source_info.clone(),
                },
            };
            rewrite.visit_basic_block(&mut single);
            *statement = single.statements.pop().expect("one statement");
        }
        let mut single =
            BasicBlock { statements: Vec::new(), terminator: block.terminator.clone() };
        rewrite.visit_basic_block(&mut single);
        block.terminator = single.terminator;
    }
}

/// A point of a body: a block, and the index of a statement in the block. The index of the
/// terminator is the count of the statements of the block.
type Point = (usize, usize);

/// Applies rule 3c until no copy qualifies (design 46).
fn forward_copies(blocks: &mut [BasicBlock], arg_count: usize, local_count: usize, coroutine: bool) {
    // Rule 3c changes no terminator, so the graph stays the same. The unwind edges count.
    let mut predecessors = vec![Vec::new(); blocks.len()];
    for (from, block) in blocks.iter().enumerate() {
        for to in block.terminator.successors() {
            predecessors[to].push(from);
        }
    }
    let reachable = reachable_blocks(blocks);
    loop {
        let mut mentions = Mentions::of(blocks, local_count);
        // The locals whose reads changed in this pass: their lists of reads are old, so they
        // wait for the next pass.
        let mut stale = vec![false; local_count];
        let mut changed = false;
        for at in 0..blocks.len() {
            for index in 0..blocks[at].statements.len() {
                let StatementKind::Assign(
                    target,
                    Rvalue::Use(Operand::Copy(source) | Operand::Move(source), _),
                ) = &blocks[at].statements[index].kind
                else {
                    continue;
                };
                let (temp, source) = (target.local, source.clone());
                let simple_source = source.projection.iter().all(|elem| {
                    matches!(elem, ProjectionElem::Field(..) | ProjectionElem::Downcast(_))
                });
                // Each other mention of the temporary is an operand that reads it (since fix
                // round 1 of design 46: one use or more). A place, a borrow, or a drop of the
                // temporary stops the rule.
                let candidate = target.projection.is_empty()
                    && temp > arg_count
                    && temp != source.local
                    && simple_source
                    && !stale[temp]
                    && !mentions.reads[temp].is_empty()
                    && mentions.all[temp].len() == 1 + mentions.reads[temp].len()
                    && mentions.borrows[temp].is_empty();
                if !candidate {
                    continue;
                }
                let mut uses = mentions.reads[temp].clone();
                uses.dedup();
                // Each use decides on the body before this copy moves: a forward into one use
                // does not stop the forward into an other use.
                let forward = forwardable_uses(&predecessors, &reachable, (at, index), &uses, &mentions.moves[temp], coroutine, |paths| {
                    paths_keep_value(blocks, paths, source.local, &mentions.all[source.local])
                        && !borrow_reaches(&predecessors, &mentions.borrows[source.local], paths.use_point)
                });
                if forward.is_empty() {
                    continue;
                }
                for &use_point in &forward {
                    // Each replaced read moves from the temporary to the local of `source`. If a
                    // read stays, the copy stays too.
                    let count = replace_reads_at(blocks, use_point, temp, &source);
                    for list in [&mut mentions.all[temp], &mut mentions.reads[temp]] {
                        for _ in 0..count {
                            if let Some(found) = list.iter().position(|point| *point == use_point) {
                                list.remove(found);
                            }
                        }
                    }
                    mentions.all[source.local].extend(std::iter::repeat_n(use_point, count));
                }
                stale[source.local] = true;
                if mentions.reads[temp].is_empty() {
                    // The copy becomes a `Nop` (rule 1 removed each `Nop` before), so each point
                    // keeps its index until the end of the rule. `temp` has no mention left.
                    blocks[at].statements[index].kind = StatementKind::Nop;
                    if let Some(found) = mentions.all[source.local].iter().position(|point| *point == (at, index)) {
                        mentions.all[source.local].remove(found);
                    }
                    mentions.all[temp].clear();
                } else {
                    stale[temp] = true;
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for block in blocks.iter_mut() {
        block.statements.retain(|statement| !matches!(statement.kind, StatementKind::Nop));
    }
}

/// The mentions of each local of a body, as points.
struct Mentions {
    /// Each point that names the local: one entry for each mention.
    all: Vec<Vec<Point>>,
    /// The points of the statements that take the address of a place of the local (a borrow or
    /// a raw address with no deref).
    borrows: Vec<Vec<Point>>,
    /// The points of the operands that read the local with no projection (`copy _t`, `move _t`),
    /// in order: one entry for each operand.
    reads: Vec<Vec<Point>>,
    /// The points of the operands that move the local with no projection (`move _t`).
    moves: Vec<Vec<Point>>,
}

impl Mentions {
    fn of(blocks: &[BasicBlock], local_count: usize) -> Mentions {
        struct Points<'a> {
            block: usize,
            index: usize,
            out: &'a mut Mentions,
        }
        impl MirVisitor for Points<'_> {
            fn visit_statement(&mut self, statement: &Statement, location: Location) {
                self.super_statement(statement, location);
                self.index += 1;
            }
            fn visit_operand(&mut self, operand: &Operand, location: Location) {
                if let Operand::Copy(place) | Operand::Move(place) = operand {
                    if place.projection.is_empty() {
                        self.out.reads[place.local].push((self.block, self.index));
                        if matches!(operand, Operand::Move(_)) {
                            self.out.moves[place.local].push((self.block, self.index));
                        }
                    }
                }
                self.super_operand(operand, location);
            }
            fn visit_local(&mut self, local: &Local, _ptx: PlaceContext, _location: Location) {
                self.out.all[*local].push((self.block, self.index));
            }
        }
        let empty = vec![Vec::new(); local_count];
        let mut out = Mentions { all: empty.clone(), borrows: empty.clone(), reads: empty.clone(), moves: empty };
        for (at, block) in blocks.iter().enumerate() {
            Points { block: at, index: 0, out: &mut out }.visit_basic_block(block);
            for (index, statement) in block.statements.iter().enumerate() {
                if let StatementKind::Assign(
                    _,
                    Rvalue::Ref(_, _, place) | Rvalue::AddressOf(_, place) | Rvalue::Reborrow(_, _, place),
                ) = &statement.kind
                {
                    if !place.projection.iter().any(|elem| matches!(elem, ProjectionElem::Deref)) {
                        out.borrows[place.local].push((at, index));
                    }
                }
            }
        }
        out
    }
}

/// The uses of a copy that rule 3c forwards into (since fix round 1 of design 46, each use, not
/// only the only use). `uses` has the points of the reads of `_t`, and `moves` the points of its
/// moves. A use qualifies if the copy runs before each run of it (condition 1; one block in a
/// coroutine body), if `keeps` holds for its paths (conditions 2 and 3), and if no move of `_t`
/// is on a path from the copy to it: a move ends the value of `_t`.
fn forwardable_uses(
    predecessors: &[Vec<usize>],
    reachable: &[bool],
    copy: Point,
    uses: &[Point],
    moves: &[Point],
    one_block: bool,
    mut keeps: impl FnMut(&Paths) -> bool,
) -> Vec<Point> {
    let mut out = Vec::new();
    for &use_point in uses {
        if one_block && use_point.0 != copy.0 {
            continue;
        }
        let Some(paths) = Paths::new(predecessors, reachable, copy, use_point) else {
            continue;
        };
        if moves.iter().any(|point| paths.contains(*point)) || !keeps(&paths) {
            continue;
        }
        out.push(use_point);
    }
    out
}

/// The paths from a copy to its use (design 46, conditions 1 and 2). A path starts after the
/// copy, ends before the use, and does not pass the copy again.
#[derive(Debug, PartialEq)]
struct Paths {
    copy: Point,
    use_point: Point,
    /// The blocks other than the block of the copy and the block of the use that a path passes.
    inner: Vec<bool>,
    /// True if a path passes the use and comes back to it (a loop that does not run the copy).
    /// Then each point of the block of the use is on a path.
    cycle: bool,
}

impl Paths {
    /// Returns the paths, or `None` if the copy is not before each run of the use: the block of
    /// the copy does not dominate the block of the use, or the use is before the copy in their
    /// block. `predecessors` has the unwind edges. A cleanup block is on no path to a block that
    /// is not a cleanup block, because no cleanup block goes back to such a block.
    fn new(predecessors: &[Vec<usize>], reachable: &[bool], copy: Point, use_point: Point) -> Option<Paths> {
        let ((at, index), (ub, ui)) = (copy, use_point);
        let mut inner = vec![false; predecessors.len()];
        if at == ub {
            // Each path from the use back to the use enters the block at its start, and so it
            // runs the copy.
            return (index < ui).then_some(Paths { copy, use_point, inner, cycle: false });
        }
        if !reachable[at] || !reachable[ub] || ub == 0 {
            return None;
        }
        // Back from the use, but not into the block of the copy: each block that the walk finds
        // can go to the use without the copy. If the entry is one of them, a path from the
        // entry goes to the use without the copy.
        let mut cycle = false;
        let mut stack = predecessors[ub].clone();
        while let Some(block) = stack.pop() {
            if block == at {
                continue;
            }
            if block == ub {
                cycle = true;
                continue;
            }
            if std::mem::replace(&mut inner[block], true) {
                continue;
            }
            if block == 0 {
                return None;
            }
            stack.extend(&predecessors[block]);
        }
        Some(Paths { copy, use_point, inner, cycle })
    }

    /// True if the point is on a path from the copy to the use. The use is on a path only in a
    /// cycle.
    fn contains(&self, (block, index): Point) -> bool {
        let ((at, copy), (ub, ui)) = (self.copy, self.use_point);
        if at == ub {
            return block == at && copy < index && index < ui;
        }
        (block == at && index > copy)
            || (block == ub && (index < ui || self.cycle))
            || (block != at && block != ub && self.inner[block])
    }
}

/// Condition 2 of rule 3c: no point on a path from the copy to the use writes `local`, names it
/// in a call or a drop, or is inline assembly. `mentions` has each point that names `local`.
fn paths_keep_value(blocks: &[BasicBlock], paths: &Paths, local: Local, mentions: &[Point]) -> bool {
    for &(block, index) in mentions {
        if !paths.contains((block, index)) {
            continue;
        }
        match blocks[block].statements.get(index) {
            Some(statement) if writes_local(&statement.kind, local) => return false,
            Some(_) => {}
            None => {
                if matches!(
                    blocks[block].terminator.kind,
                    TerminatorKind::Call { .. } | TerminatorKind::Drop { .. } | TerminatorKind::InlineAsm { .. }
                ) {
                    return false;
                }
            }
        }
    }
    // Inline assembly on a path stops the rule, also when it does not name the local.
    if paths.copy.0 != paths.use_point.0 {
        for (at, block) in blocks.iter().enumerate() {
            let asm = matches!(block.terminator.kind, TerminatorKind::InlineAsm { .. });
            if asm && paths.contains((at, block.statements.len())) {
                return false;
            }
        }
    }
    true
}

/// Condition 3 of rule 3c: true if a statement in `borrows` (the borrows of a local) is at a
/// point from which a path goes to the use. Each point that runs before a run of the use is such
/// a point, so with no such borrow, no pointer to the local exists before the use.
fn borrow_reaches(predecessors: &[Vec<usize>], borrows: &[Point], (ub, ui): Point) -> bool {
    if borrows.is_empty() {
        return false;
    }
    // The blocks with a path to the start of the block of the use (the block itself only in a
    // loop). No block is excluded: a path through the copy also counts.
    let mut reaches = vec![false; predecessors.len()];
    let mut stack = predecessors[ub].clone();
    while let Some(block) = stack.pop() {
        if !std::mem::replace(&mut reaches[block], true) {
            stack.extend(&predecessors[block]);
        }
    }
    borrows.iter().any(|&(block, index)| (block == ub && index < ui) || reaches[block])
}

/// True if the statement writes a place of the local.
fn writes_local(kind: &StatementKind, local: Local) -> bool {
    match kind {
        StatementKind::Assign(place, _) | StatementKind::SetDiscriminant { place, .. } => {
            place.local == local
        }
        _ => false,
    }
}

/// Replaces each operand that reads `temp` (with no projection) at the point with a copy of
/// `source`. Returns the count of the operands that it replaced.
fn replace_reads_at(blocks: &mut [BasicBlock], (block, index): Point, temp: Local, source: &Place) -> usize {
    struct Replace<'a> {
        temp: Local,
        source: &'a Place,
        count: usize,
    }
    impl MutMirVisitor for Replace<'_> {
        fn visit_operand(&mut self, operand: &mut Operand, location: Location) {
            if let Operand::Copy(place) | Operand::Move(place) = operand {
                if place.local == self.temp && place.projection.is_empty() {
                    *operand = Operand::Copy(self.source.clone());
                    self.count += 1;
                    return;
                }
            }
            self.super_operand(operand, location);
        }
    }
    // A visitor needs a block, so the statement or the terminator goes into a block of its own.
    let mut replace = Replace { temp, source, count: 0 };
    let block = &mut blocks[block];
    if let Some(statement) = block.statements.get(index) {
        let mut single = BasicBlock {
            statements: vec![statement.clone()],
            terminator: Terminator { kind: TerminatorKind::Unreachable, source_info: statement.source_info.clone() },
        };
        replace.visit_basic_block(&mut single);
        if replace.count > 0 {
            block.statements[index] = single.statements.pop().expect("the block has one statement");
        }
    } else {
        let mut single = BasicBlock { statements: Vec::new(), terminator: block.terminator.clone() };
        replace.visit_basic_block(&mut single);
        if replace.count > 0 {
            block.terminator = single.terminator;
        }
    }
    replace.count
}

/// Applies rule 3e: a branch on `Ne` becomes a branch on `Eq` with the two blocks exchanged.
fn branch_on_eq(blocks: &mut [BasicBlock], arg_count: usize, local_count: usize) {
    let mut mentions = vec![0usize; local_count];
    for block in blocks.iter() {
        count_mentions(block, &mut mentions);
    }
    for block in blocks.iter_mut() {
        let TerminatorKind::SwitchInt { discr: Operand::Copy(discr) | Operand::Move(discr), targets } =
            &block.terminator.kind
        else {
            continue;
        };
        let Some(Statement { kind: StatementKind::Assign(target, Rvalue::BinaryOp(BinOp::Ne, left, right)), .. }) =
            block.statements.last()
        else {
            continue;
        };
        // The temporary has two mentions: this assignment and the switch.
        let temp = target.local;
        let plain = target.projection.is_empty() && discr.projection.is_empty() && discr.local == temp;
        if !plain || temp <= arg_count || mentions[temp] != 2 {
            continue;
        }
        // The form of a branch on a `bool`: one block for 0, and one other block.
        let branches: Vec<(u128, BasicBlockIdx)> = targets.branches().collect();
        let [(0, on_false)] = branches[..] else { continue };
        let on_true = targets.otherwise();
        let (left, right, target) = (left.clone(), right.clone(), target.clone());
        let discr = block.terminator.kind.clone();
        let TerminatorKind::SwitchInt { discr, .. } = discr else { continue };
        if let Some(last) = block.statements.last_mut() {
            last.kind = StatementKind::Assign(target, Rvalue::BinaryOp(BinOp::Eq, left, right));
        }
        block.terminator.kind =
            TerminatorKind::SwitchInt { discr, targets: SwitchTargets::new(vec![(0, on_true)], on_false) };
    }
}

/// Counts the mentions of each local in a block.
fn count_mentions(block: &BasicBlock, mentions: &mut [usize]) {
    struct Count<'a>(&'a mut [usize]);
    impl MirVisitor for Count<'_> {
        fn visit_local(&mut self, local: &Local, _ptx: PlaceContext, _location: Location) {
            self.0[*local] += 1;
        }
    }
    Count(mentions).visit_basic_block(block);
}

/// Follows a chain of empty `goto` blocks. A cycle of empty blocks stops at its first repeat.
fn goto_chain_end(blocks: &[BasicBlock], start: BasicBlockIdx) -> BasicBlockIdx {
    let mut current = start;
    let mut seen = vec![false; blocks.len()];
    loop {
        let block = &blocks[current];
        match block.terminator.kind {
            TerminatorKind::Goto { target } if block.statements.is_empty() && !seen[current] => {
                seen[current] = true;
                current = target;
            }
            _ => return current,
        }
    }
}

/// Orders the blocks by a depth-first walk from the entry, and removes unreachable blocks.
fn order_blocks(blocks: Vec<BasicBlock>) -> Vec<BasicBlock> {
    let mut order = Vec::new();
    let mut seen = vec![false; blocks.len()];
    let mut stack = vec![0];
    while let Some(index) = stack.pop() {
        if seen[index] {
            continue;
        }
        seen[index] = true;
        order.push(index);
        // Push in reverse, so that the first successor is visited first.
        for successor in blocks[index].terminator.successors().into_iter().rev() {
            if !seen[successor] {
                stack.push(successor);
            }
        }
    }

    let new_index: HashMap<BasicBlockIdx, BasicBlockIdx> =
        order.iter().enumerate().map(|(new, &old)| (old, new)).collect();
    order
        .iter()
        .map(|&old| {
            let mut block = blocks[old].clone();
            remap_targets(&mut block.terminator.kind, |target| new_index[&target]);
            block
        })
        .collect()
}

/// Applies `map` to each block target of a terminator, the unwind targets included.
pub fn remap_targets(kind: &mut TerminatorKind, map: impl Fn(BasicBlockIdx) -> BasicBlockIdx) {
    let remap_unwind = |unwind: &mut UnwindAction| {
        if let UnwindAction::Cleanup(target) = unwind {
            *target = map(*target);
        }
    };
    match kind {
        TerminatorKind::Goto { target } => *target = map(*target),
        TerminatorKind::SwitchInt { targets, .. } => {
            let branches = targets.branches().map(|(value, target)| (value, map(target))).collect();
            *targets = SwitchTargets::new(branches, map(targets.otherwise()));
        }
        TerminatorKind::Drop { target, unwind, .. }
        | TerminatorKind::Assert { target, unwind, .. } => {
            *target = map(*target);
            remap_unwind(unwind);
        }
        TerminatorKind::Call { target, unwind, .. } => {
            if let Some(target) = target {
                *target = map(*target);
            }
            remap_unwind(unwind);
        }
        TerminatorKind::InlineAsm { destination, unwind, .. } => {
            if let Some(destination) = destination {
                *destination = map(*destination);
            }
            remap_unwind(unwind);
        }
        TerminatorKind::Resume
        | TerminatorKind::Abort
        | TerminatorKind::Return
        | TerminatorKind::Unreachable => {}
    }
}

/// Returns the old local numbers in their new order.
fn local_order(body: &Body, arg_count: usize) -> Vec<Local> {
    struct FirstUse {
        order: Vec<Local>,
        seen: Vec<bool>,
    }

    impl MirVisitor for FirstUse {
        fn visit_local(&mut self, local: &Local, _ptx: PlaceContext, _location: Location) {
            if !self.seen[*local] {
                self.seen[*local] = true;
                self.order.push(*local);
            }
        }
    }

    // The return local and the arguments come first, in their original order.
    let fixed: Vec<Local> = (0..=arg_count).collect();
    let mut seen = vec![false; body.locals().len()];
    for &local in &fixed {
        seen[local] = true;
    }
    let mut visitor = FirstUse { order: fixed, seen };
    visitor.visit_body(body);
    visitor.order
}

/// Builds a body where local `order[i]` becomes local `i`.
fn renumber_locals(mut body: Body, order: &[Local], arg_count: usize) -> Body {
    struct Renumber {
        new_index: HashMap<Local, Local>,
    }

    impl MutMirVisitor for Renumber {
        fn visit_local(&mut self, local: &mut Local, _ptx: PlaceContext, _location: Location) {
            *local = self.new_index[local];
        }

        fn visit_operand(&mut self, operand: &mut Operand, location: Location) {
            if let Operand::Move(place) = operand {
                *operand = Operand::Copy(place.clone());
            }
            self.super_operand(operand, location);
        }
    }

    let new_index = order.iter().enumerate().map(|(new, &old)| (old, new)).collect();
    Renumber { new_index }.visit_body(&mut body);

    let locals = order
        .iter()
        .map(|&old| {
            let decl = &body.locals()[old];
            LocalDecl { ty: decl.ty, span: decl.span, mutability: Mutability::Not }
        })
        .collect();
    // The spread argument is an argument, so its number does not change.
    let spread_arg = body.spread_arg();
    Body::new(body.blocks, locals, arg_count, Vec::new(), spread_arg, body.span)
}

#[cfg(test)]
mod tests {
    use super::{Paths, borrow_reaches, forwardable_uses};

    /// The predecessors of each block from its successors (an unwind edge is a successor too).
    fn predecessors(successors: &[&[usize]]) -> Vec<Vec<usize>> {
        let mut list = vec![Vec::new(); successors.len()];
        for (from, targets) in successors.iter().enumerate() {
            for &to in *targets {
                list[to].push(from);
            }
        }
        list
    }

    #[test]
    fn the_paths_of_a_copy_go_to_a_use_in_a_later_block() {
        // bb0 (the copy at 1) -> bb1 -> bb2 (the use at 1).
        let graph = predecessors(&[&[1], &[2], &[]]);
        let paths = Paths::new(&graph, &[true; 3], (0, 1), (2, 1)).unwrap();
        for point in [(0, 2), (0, 5), (1, 0), (1, 3), (2, 0)] {
            assert!(paths.contains(point), "{point:?}");
        }
        for point in [(0, 0), (0, 1), (2, 1), (2, 2)] {
            assert!(!paths.contains(point), "{point:?}");
        }
        // One block: only the points between the copy and the use.
        let paths = Paths::new(&graph, &[true; 3], (1, 1), (1, 4)).unwrap();
        assert!(paths.contains((1, 2)) && paths.contains((1, 3)));
        assert!(!paths.contains((1, 1)) && !paths.contains((1, 4)) && !paths.contains((1, 5)) && !paths.contains((2, 0)));
    }

    #[test]
    fn a_copy_that_does_not_run_before_each_use_has_no_paths() {
        // Condition 1. A diamond: bb0 -> bb1, bb2 -> bb3. The path through bb2 does not run a
        // copy in bb1.
        let graph = predecessors(&[&[1, 2], &[3], &[3], &[]]);
        let all = [true; 4];
        assert_eq!(Paths::new(&graph, &all, (1, 0), (3, 0)), None);
        // In one block, the use comes after the copy.
        assert_eq!(Paths::new(&graph, &all, (1, 2), (1, 1)), None);
        assert_eq!(Paths::new(&graph, &all, (1, 2), (1, 2)), None);
        // The use in the entry block, and a use that the entry does not reach.
        assert_eq!(Paths::new(&graph, &all, (1, 0), (0, 0)), None);
        assert_eq!(Paths::new(&graph, &[true, true, true, false], (1, 0), (3, 0)), None);
        // The entry dominates each block: both arms are on a path.
        let paths = Paths::new(&graph, &all, (0, 0), (3, 0)).unwrap();
        assert!(paths.contains((1, 0)) && paths.contains((2, 7)) && !paths.cycle);
    }

    #[test]
    fn a_loop_that_does_not_run_the_copy_puts_the_whole_block_of_the_use_on_a_path() {
        // bb0 (the copy) -> bb1 (the loop head, the use at 1) -> bb2 (the loop body) -> bb1;
        // bb1 -> bb3 (the end). A write in bb2 or after the use in bb1 changes the value at the
        // next run of the use.
        let graph = predecessors(&[&[1], &[2, 3], &[1], &[]]);
        let paths = Paths::new(&graph, &[true; 4], (0, 0), (1, 1)).unwrap();
        assert!(paths.cycle);
        for point in [(1, 0), (1, 1), (1, 3), (2, 0)] {
            assert!(paths.contains(point), "{point:?}");
        }
        assert!(!paths.contains((3, 0)) && !paths.contains((0, 0)));
        // The copy in the loop head, the use in the loop body: each path from the use back to
        // the use runs the copy again.
        let paths = Paths::new(&graph, &[true; 4], (1, 0), (2, 1)).unwrap();
        assert!(!paths.cycle && paths.contains((1, 1)) && paths.contains((2, 0)));
        assert!(!paths.contains((2, 2)) && !paths.contains((1, 0)) && !paths.contains((3, 0)));
    }

    #[test]
    fn a_cleanup_block_is_on_no_path_to_a_use() {
        // bb0 -> bb1 (the return block of a call), bb0 -> bb2 (the cleanup block) -> bb3.
        let graph = predecessors(&[&[1, 2], &[], &[3], &[]]);
        let paths = Paths::new(&graph, &[true; 4], (0, 0), (1, 0)).unwrap();
        assert!(paths.contains((0, 1)) && !paths.contains((2, 0)) && !paths.contains((3, 0)));
    }

    #[test]
    fn a_copy_moves_into_each_use_that_meets_the_conditions() {
        // Fix round 1 of design 46. bb0 (the copy at 0, a use at 1) -> bb1 (a use at 0, a move
        // of `_t` at 1) -> bb2 (a use at 0). Each use decides alone.
        let graph = predecessors(&[&[1], &[2], &[]]);
        let reachable = [true; 3];
        let uses = [(0, 1), (1, 0), (1, 1), (2, 0)];
        assert_eq!(forwardable_uses(&graph, &reachable, (0, 0), &uses, &[], false, |_| true), uses.to_vec());
        // Condition 6: the move at (1, 1) ends the value of `_t` on the paths to (2, 0) only.
        assert_eq!(forwardable_uses(&graph, &reachable, (0, 0), &uses, &[(1, 1)], false, |_| true), vec![(0, 1), (1, 0), (1, 1)]);
        // Conditions 2 and 3 hold only for the paths that `keeps` accepts (here: not to bb1).
        assert_eq!(forwardable_uses(&graph, &reachable, (0, 0), &uses, &[], false, |paths| paths.use_point.0 != 1), vec![(0, 1), (2, 0)]);
        // Condition 5: a coroutine body keeps one block.
        assert_eq!(forwardable_uses(&graph, &reachable, (0, 0), &uses, &[], true, |_| true), vec![(0, 1)]);
        // Condition 1: a diamond bb0 -> bb1, bb2 -> bb3 with the copy in bb1. The use after the
        // join does not run the copy on each path; the use in bb1 does.
        let graph = predecessors(&[&[1, 2], &[3], &[3], &[]]);
        assert_eq!(forwardable_uses(&graph, &[true; 4], (1, 0), &[(1, 2), (3, 0)], &[], false, |_| true), vec![(1, 2)]);
    }

    #[test]
    fn only_a_borrow_with_a_path_to_the_use_stops_the_forward() {
        // Condition 3. bb0 (the copy) -> bb1 (the use at 2) -> bb2.
        let graph = predecessors(&[&[1], &[2], &[]]);
        // A borrow after the use, also in the block of the use: no pointer exists at the use.
        assert!(!borrow_reaches(&graph, &[], (1, 2)));
        assert!(!borrow_reaches(&graph, &[(2, 0), (1, 3)], (1, 2)));
        // A borrow before the copy or between the copy and the use.
        assert!(borrow_reaches(&graph, &[(0, 0)], (1, 2)));
        assert!(borrow_reaches(&graph, &[(1, 1)], (1, 2)));
        // A loop: a borrow after the use goes back to the use.
        let graph = predecessors(&[&[1], &[2, 3], &[1], &[]]);
        assert!(borrow_reaches(&graph, &[(2, 0)], (1, 1)));
        assert!(borrow_reaches(&graph, &[(1, 4)], (1, 1)));
        assert!(!borrow_reaches(&graph, &[(3, 0)], (1, 1)));
    }
}
