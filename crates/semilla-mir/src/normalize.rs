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
    BasicBlock, BasicBlockIdx, Body, Local, LocalDecl, Mutability, Operand, StatementKind,
    SwitchTargets, TerminatorKind, UnwindAction,
};

/// Returns the canonical text of a body. Two bodies with the same text have the same behavior.
pub fn canonical_text(body: &Body) -> String {
    let body = canonical_body(body);
    let mut text = Vec::new();
    // The name is not part of the body, so all bodies get the same placeholder name.
    body.dump(&mut text, "_").expect("a write to a Vec does not fail");
    stable_closure_names(&String::from_utf8(text).expect("MIR text is UTF-8"))
}

/// Replaces names such as `{closure@src/lib.rs:2:18: 2:21}` with `{closure#0}`.
///
/// The same original name always gets the same number, so two different closures in one body
/// stay different.
fn stable_closure_names(text: &str) -> String {
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
            (Some(kind_end), Some(close)) if kind_end > 1 && candidate[kind_end..].starts_with('@') => {
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
    let resolved: Vec<BasicBlockIdx> = (0..blocks.len()).map(|index| goto_chain_end(blocks, index)).collect();
    // The entry block stays the entry block, so it is not resolved.
    for block in blocks.iter_mut() {
        remap_targets(&mut block.terminator.kind, |target| resolved[target]);
    }
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
fn remap_targets(kind: &mut TerminatorKind, map: impl Fn(BasicBlockIdx) -> BasicBlockIdx) {
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
        TerminatorKind::Drop { target, unwind, .. } | TerminatorKind::Assert { target, unwind, .. } => {
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
