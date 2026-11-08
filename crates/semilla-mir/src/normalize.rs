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
//!    predecessor (the unwind edges count). The code runs in the same order, so the behavior is
//!    the same. The N3 inliner needs this rule: it adds a `goto` into the callee entry.
//! 3c. Forward a copy: replace `_t = copy p; ...; use(copy _t)` with `...; use(copy p)` when the
//!    copy and the only use of `_t` are in the same block, `p` is a local or a field of a local,
//!    no statement between them writes the local of `p`, and the body takes the address of
//!    neither local. Then `p` has the same value at the use, so the use reads the same value.
//!    `_t` has no other use, so the copy has no other effect. The N3 inliner needs this rule,
//!    because the inlined code and the original code copy values in different places.
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
    BasicBlock, BasicBlockIdx, Body, Local, LocalDecl, Mutability, Operand, Place, ProjectionElem,
    Rvalue, StatementKind, SwitchTargets, Terminator, TerminatorKind, UnwindAction,
};

use crate::print::Printer;

/// The canonical form of a body: its text and the local functions that it refers to.
pub struct Canonical {
    pub text: String,
    pub callees: Vec<String>,
}

/// Returns the canonical text of a body that `canonical_body` made. Two bodies with the same
/// text have the same behavior.
pub fn canonical_text(body: &Body) -> Canonical {
    let mut printer = Printer::default();
    let text = printer.body(body);
    Canonical { text: stable_closure_names(&text), callees: printer.callees.into_iter().collect() }
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
    let arg_count = body.arg_locals().len();

    let mut blocks = body.blocks.clone();
    for block in &mut blocks {
        block.statements.retain(|statement| has_runtime_effect(&statement.kind));
    }
    skip_empty_gotos(&mut blocks);
    merge_straight_blocks(&mut blocks);
    forward_copies(&mut blocks, arg_count, body.locals().len());
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
    renumber_locals(ordered, &locals, arg_count)
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
/// predecessor and is not the entry. The merged target becomes unreachable, and
/// `order_blocks` removes it.
fn merge_straight_blocks(blocks: &mut [BasicBlock]) {
    loop {
        let mut predecessors = vec![0usize; blocks.len()];
        predecessors[0] = 1;
        for block in blocks.iter() {
            for successor in block.terminator.successors() {
                predecessors[successor] += 1;
            }
        }
        let candidate = (0..blocks.len()).find_map(|from| match blocks[from].terminator.kind {
            TerminatorKind::Goto { target }
                if target != from && target != 0 && predecessors[target] == 1 =>
            {
                Some((from, target))
            }
            _ => None,
        });
        let Some((from, target)) = candidate else {
            return;
        };
        let statements = std::mem::take(&mut blocks[target].statements);
        let terminator = blocks[target].terminator.clone();
        // The old target keeps no successor, so it does not count as a predecessor.
        blocks[target].terminator.kind = TerminatorKind::Unreachable;
        blocks[from].statements.extend(statements);
        blocks[from].terminator = terminator;
    }
}

/// Applies rule 3c until no copy qualifies.
fn forward_copies(blocks: &mut [BasicBlock], arg_count: usize, local_count: usize) {
    loop {
        let mut mentions = vec![0usize; local_count];
        let mut taken = vec![false; local_count];
        for block in blocks.iter() {
            count_mentions(block, &mut mentions);
            for statement in &block.statements {
                if let StatementKind::Assign(
                    _,
                    Rvalue::Ref(_, _, place)
                    | Rvalue::AddressOf(_, place)
                    | Rvalue::Reborrow(_, _, place),
                ) = &statement.kind
                {
                    if !place.projection.iter().any(|elem| matches!(elem, ProjectionElem::Deref)) {
                        taken[place.local] = true;
                    }
                }
            }
        }
        let mut changed = false;
        for block in blocks.iter_mut() {
            let mut index = 0;
            while index < block.statements.len() {
                let StatementKind::Assign(
                    target,
                    Rvalue::Use(Operand::Copy(source) | Operand::Move(source), _),
                ) = &block.statements[index].kind
                else {
                    index += 1;
                    continue;
                };
                let (temp, source) = (target.local, source.clone());
                let simple_source = source.projection.iter().all(|elem| {
                    matches!(elem, ProjectionElem::Field(..) | ProjectionElem::Downcast(_))
                });
                // The temporary has two mentions: this assignment and its only use.
                let candidate = target.projection.is_empty()
                    && temp > arg_count
                    && temp != source.local
                    && mentions[temp] == 2
                    && !taken[temp]
                    && !taken[source.local]
                    && simple_source;
                if candidate && forward_in_block(block, index, temp, &source) {
                    // The copy is gone, and the use reads `source` now: the mentions of the
                    // locals of `source` stay the same, and `temp` has no mention left. The
                    // next statement is at `index` now.
                    mentions[temp] = 0;
                    changed = true;
                    continue;
                }
                index += 1;
            }
        }
        if !changed {
            return;
        }
    }
}

/// Replaces the use of `temp` after statement `index` with `source`, and removes the copy.
/// Returns false (and changes nothing) if the use is not in this block or if a statement between
/// writes the local of `source`.
fn forward_in_block(block: &mut BasicBlock, index: usize, temp: Local, source: &Place) -> bool {
    struct Replace<'a> {
        temp: Local,
        source: &'a Place,
        done: bool,
    }
    impl MutMirVisitor for Replace<'_> {
        fn visit_operand(&mut self, operand: &mut Operand, location: Location) {
            if let Operand::Copy(place) | Operand::Move(place) = operand {
                if !self.done && place.local == self.temp && place.projection.is_empty() {
                    *operand = Operand::Copy(self.source.clone());
                    self.done = true;
                    return;
                }
            }
            self.super_operand(operand, location);
        }
    }
    let writes_source = |kind: &StatementKind| match kind {
        StatementKind::Assign(place, _) | StatementKind::SetDiscriminant { place, .. } => {
            place.local == source.local
        }
        _ => false,
    };
    // A visitor needs a block, so each statement goes into a block of its own.
    let mut replace = Replace { temp, source, done: false };
    for later in index + 1..block.statements.len() {
        let statement = block.statements[later].clone();
        let mut single = BasicBlock {
            statements: vec![statement.clone()],
            terminator: Terminator {
                kind: TerminatorKind::Unreachable,
                source_info: statement.source_info,
            },
        };
        replace.visit_basic_block(&mut single);
        if replace.done {
            block.statements[later] = single.statements.pop().expect("the block has one statement");
            block.statements.remove(index);
            return true;
        }
        if writes_source(&block.statements[later].kind) {
            return false;
        }
    }
    let mut single = BasicBlock { statements: Vec::new(), terminator: block.terminator.clone() };
    replace.visit_basic_block(&mut single);
    if replace.done {
        block.terminator = single.terminator;
        block.statements.remove(index);
        return true;
    }
    false
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
