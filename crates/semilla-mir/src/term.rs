//! N4 terms: a loop-free body as one term, by symbolic execution of the control flow graph.
//!
//! The term is the input of the equality saturation (egglog) in `semilla-normal`. Each term
//! denotes the full behavior of the body: the return value, and the ordered list of all side
//! effects. Two equal terms mean two equal behaviors.
//!
//! # How the term is built
//!
//! - Each path from the entry block to an exit gives an outcome term: `ret` (with the return
//!   value), `resume` (unwind to the caller), `abort`, `diverge`, or `unreachable`. A `switchInt`
//!   becomes `Ite` of the outcomes of its targets. So the term is a tree of decisions, with the
//!   outcomes at the leaves.
//! - A value is a pure term over the arguments, the constants, and the state. A pure term has no
//!   side effect, so the same term always gives the same value on a path.
//! - All side effects take the current **state** term and give a new state term: calls, drops,
//!   asserts (a possible panic), writes through pointers, `assume`, and memory copies. So the
//!   side effects keep their order. A read through a pointer (`Load`) takes the state too, so a
//!   read cannot move across a write.
//! - A local whose address the body takes lives in memory: each read is a `Load` and each write
//!   is a `store` effect. The other locals are plain values. A local in memory has the address
//!   `local(slot n)`, where `n` counts the locals in memory in the order of their first use.
//! - An unwind edge to a cleanup block becomes an `uw-cleanup` term that holds the outcome of the
//!   cleanup path. The cleanup path starts from the state `hole`: the state at the panic point.
//!   `hole` refers to the nearest enclosing unwind node.
//!
//! # Rules applied while the term is built
//!
//! Each rule keeps the behavior the same:
//!
//! 1. **Path facts.** On a path, the condition of a branch has a known value, and the condition
//!    of a passed assert is true. An assert whose condition is already true on the path cannot
//!    fail, so it is removed. The facts compare integer comparisons in a canonical form:
//!    `a >= b` is `not (a < b)`, `a > b` is `b < a`, `a <= b` is `not (b < a)`, `a != b` is
//!    `not (a == b)`. These identities hold for integers (a total order) only, so only integer
//!    comparisons get this form. When this rule removes an assert, or follows a bool switch
//!    whose condition is known on the path to the known target, the record says so
//!    (`path-fact`). The same pure term has the same value everywhere on one path.
//! 2. **Read after write.** A read of a part of a local that the body just wrote gives the written
//!    value. A read of another field of a struct or a tuple gives the old value: two different
//!    fields do not overlap. A union field never gets this rule, because union fields overlap
//!    (`read-after-write`).
//! 3. **Field of an aggregate.** Field `i` of a tuple, struct, or closure value that the body
//!    built gives operand `i` of the aggregate. Field `i` of a downcast to variant `v` of an enum
//!    value built as variant `v` gives operand `i` (`aggregate-field`).
//! 4. **Double negation.** `not (not c)` is `c` for a bool (`not-not`).
//! 5. **Single-variant discriminant.** The discriminant of an enum whose layout has one variant
//!    (for example `Option<!>`) is the constant discriminant of that variant. Code generation
//!    does not read memory for it (`single-variant-discriminant`).
//! 6. **Reflexive comparison.** An integer comparison of a pure term with itself has a fixed
//!    result: `a == a`, `a <= a`, `a >= a` are true, and `a != a`, `a < a`, `a > a` are false
//!    (`reflexive-compare`).
//! 7. **True assumption.** `assume(true)` has no effect, so it is removed (`assume-true`).
//! 8. **Constant switch.** A `switchInt` on a constant (a bool, or an integer that is not
//!    negative) always jumps to the target of that value, so the path goes there directly
//!    (`constant-switch`). A short circuit (`a && b`) that a function returns as a bool and the
//!    caller tests again gives such a switch after inlining.
//!
//! The term text has one node for each line: `#<n> (<constructor> <arguments>)`. An argument is a
//! reference `#<m>` to an earlier line, or a string. The last line is the root.

use std::collections::HashMap;

use rustc_public::abi::VariantsShape;
use rustc_public::mir::{
    AggregateKind, AssertMessage, BinOp, Body, Local, NonDivergingIntrinsic, Operand, Place,
    ProjectionElem, Rvalue, StatementKind, TerminatorKind, UnOp, UnwindAction,
};
use rustc_public::ty::{AdtKind, RigidTy, Ty, TyKind, VariantIdx};
use rustc_public_bridge::IndexedVal;

use crate::print::{
    DiscriminantUse, FieldName, Printer, aggregate_fields, borrow_kind_text, cast_kind_text, cast_note,
    discriminant_table, field_name, table_text, variant_path,
};

/// The leaves of the bool constants, in the form of the printer.
const TRUE: &str = "const bool = true";
const FALSE: &str = "const bool = false";

/// The most block visits for one body. A body with more paths gets no term.
const MAX_STEPS: usize = 4000;

/// The term of a body, with the names of the build rules that changed it.
pub struct BodyTerm {
    pub text: String,
    pub rules: Vec<String>,
}

/// Returns the term of a loop-free body, or `None` if the body uses a part that the builder does
/// not model, or if the body has too many paths.
pub fn body_term(body: &Body, generics: &str) -> Option<BodyTerm> {
    let mut builder = Builder::new(body);
    builder.generics = generics.to_string();
    let root = builder.root().ok()?;
    let rules = builder.rules.iter().map(|rule| rule.to_string()).collect();
    Some(BodyTerm { text: builder.nodes.render(root), rules })
}

type Id = usize;

#[derive(Clone, PartialEq, Eq, Hash)]
enum Node {
    /// An atom: a constant, a type, an argument, or a fixed word.
    Leaf(String),
    /// An operation with a name and a list of arguments.
    App(String, Vec<Id>),
    Ite(Id, Id, Id),
    Not(Id),
    /// An integer comparison: `lt`, `le`, `gt`, `ge`, `eq`, or `ne`.
    Cmp(&'static str, Id, Id),
    /// `Assert(condition, message, unwind, state)`: a check that must be true.
    Assert(Id, Id, Id, Id),
    /// `Load(type, address, state)`: a read through a pointer.
    Load(Id, Id, Id),
}

/// The nodes of a term. Equal nodes get the same id (hash consing).
#[derive(Default)]
struct Nodes {
    nodes: Vec<Node>,
    ids: HashMap<Node, Id>,
}

impl Nodes {
    fn add(&mut self, node: Node) -> Id {
        if let Some(&id) = self.ids.get(&node) {
            return id;
        }
        let id = self.nodes.len();
        self.nodes.push(node.clone());
        self.ids.insert(node, id);
        id
    }

    fn get(&self, id: Id) -> &Node {
        &self.nodes[id]
    }

    /// Prints the nodes that the root can reach, in a fixed order (children first).
    ///
    /// The slots of the locals in memory get new numbers here, in the order of their first
    /// appearance in the printed term. The numbers of the build depend on the order in which the
    /// builder explores the paths, which depends on the shape of the graph. The new numbers are a
    /// one-to-one renaming inside one term, so the meaning stays the same.
    fn render(&self, root: Id) -> String {
        let mut number: HashMap<Id, usize> = HashMap::new();
        let mut slots: HashMap<String, usize> = HashMap::new();
        let mut lines = Vec::new();
        let mut stack = vec![(root, false)];
        while let Some((id, expanded)) = stack.pop() {
            if number.contains_key(&id) {
                continue;
            }
            let children = self.children(id);
            if !expanded {
                stack.push((id, true));
                for &child in children.iter().rev() {
                    if !number.contains_key(&child) {
                        stack.push((child, false));
                    }
                }
                continue;
            }
            let reference = |child: &Id| format!("#{}", number[child]);
            let text = match self.get(id) {
                Node::Leaf(text) if text.starts_with("slot ") => {
                    let next = slots.len();
                    let slot = *slots.entry(text.clone()).or_insert(next);
                    format!("(L {})", quote(&format!("slot {slot}")))
                }
                Node::Leaf(text) => format!("(L {})", quote(text)),
                Node::App(name, args) => {
                    let mut list = "(Nil)".to_string();
                    for arg in args.iter().rev() {
                        list = format!("(Cons {} {list})", reference(arg));
                    }
                    format!("(A {} {list})", quote(name))
                }
                Node::Ite(c, a, b) => {
                    format!("(Ite {} {} {})", reference(c), reference(a), reference(b))
                }
                Node::Not(c) => format!("(Not {})", reference(c)),
                Node::Cmp(op, a, b) => {
                    format!("(ICmp {} {} {})", quote(op), reference(a), reference(b))
                }
                Node::Assert(c, m, u, s) => format!(
                    "(Assert {} {} {} {})",
                    reference(c),
                    reference(m),
                    reference(u),
                    reference(s)
                ),
                Node::Load(t, a, s) => {
                    format!("(Load {} {} {})", reference(t), reference(a), reference(s))
                }
            };
            number.insert(id, lines.len());
            lines.push(format!("#{} {text}", lines.len()));
        }
        lines.join("\n")
    }

    fn children(&self, id: Id) -> Vec<Id> {
        match self.get(id) {
            Node::Leaf(_) => Vec::new(),
            Node::App(_, args) => args.clone(),
            Node::Ite(a, b, c) | Node::Load(a, b, c) => vec![*a, *b, *c],
            Node::Not(a) => vec![*a],
            Node::Cmp(_, a, b) => vec![*a, *b],
            Node::Assert(a, b, c, d) => vec![*a, *b, *c, *d],
        }
    }
}

/// Quotes a string for the term text. It escapes `"` and `\`.
fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A part of the body that the builder does not model.
struct Unsupported;

type Result<T> = std::result::Result<T, Unsupported>;

/// A known value of a condition on the current path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FactKey {
    /// A bool term that is not an integer comparison.
    Atom(Id),
    /// `a < b` on integers.
    Lt(Id, Id),
    /// `a == b` on integers. The smaller id comes first, because `==` is symmetric.
    Eq(Id, Id),
}

/// The symbolic state at one point of a path.
#[derive(Clone)]
struct Env {
    /// The value of each local that does not live in memory. `None` means "not written yet".
    locals: Vec<Option<Id>>,
    /// The current state term.
    state: Id,
    /// The known conditions on this path, with their values.
    facts: Vec<(FactKey, bool)>,
}

/// Where a place is.
enum Location {
    /// A part of a local that is a plain value: the local and the projection terms.
    Value(Local, Vec<Id>),
    /// A place in memory, at an address term.
    Memory(Id),
}

/// What the builder knows about an aggregate value (rule 3).
#[derive(Clone)]
enum AggregateInfo {
    /// A tuple, a struct, or a closure: field `i` is operand `i`.
    Fields(Vec<Id>),
    /// An enum variant: after a downcast to this variant, field `i` is operand `i`.
    Variant(usize, Vec<Id>),
}

struct Builder<'a> {
    body: &'a Body,
    printer: Printer,
    nodes: Nodes,
    /// The locals whose address the body takes. They live in memory.
    in_memory: Vec<bool>,
    aggregates: HashMap<Id, AggregateInfo>,
    /// The `set` nodes (a write to a part of a local): the path, the new value, the old value.
    sets: HashMap<Id, (Vec<Id>, Id, Id)>,
    /// The projection terms that are fields of a struct, a tuple, or an enum variant (not of a
    /// union), with their field index.
    struct_fields: HashMap<Id, usize>,
    /// The projection terms that are downcasts, with their variant index.
    downcasts: HashMap<Id, usize>,
    steps: usize,
    /// The number of each local in memory (see `local_address`).
    slots: HashMap<Local, usize>,
    /// The build rules that changed the term.
    rules: std::collections::BTreeSet<&'static str>,
    /// The generic parameters of the body (see `generics_text` in `main.rs`), or empty.
    generics: String,
}

impl<'a> Builder<'a> {
    fn new(body: &'a Body) -> Self {
        let mut in_memory = vec![false; body.locals().len()];
        for local in address_taken_locals(body) {
            in_memory[local] = true;
        }
        Builder {
            body,
            printer: Printer::for_body(body),
            nodes: Nodes::default(),
            in_memory,
            aggregates: HashMap::new(),
            sets: HashMap::new(),
            struct_fields: HashMap::new(),
            downcasts: HashMap::new(),
            steps: 0,
            slots: HashMap::new(),
            rules: std::collections::BTreeSet::new(),
            generics: String::new(),
        }
    }

    fn leaf(&mut self, text: impl Into<String>) -> Id {
        self.nodes.add(Node::Leaf(text.into()))
    }

    fn app(&mut self, name: impl Into<String>, args: Vec<Id>) -> Id {
        self.nodes.add(Node::App(name.into(), args))
    }

    /// Rule 4: `not (not c)` is `c`.
    fn not(&mut self, condition: Id) -> Id {
        if let Node::Not(inner) = self.nodes.get(condition) {
            let inner = *inner;
            self.rules.insert("not-not");
            return inner;
        }
        self.nodes.add(Node::Not(condition))
    }

    fn ty_leaf(&mut self, ty: Ty) -> Id {
        let text = self.printer.ty(ty);
        self.leaf(format!("type {text}"))
    }

    /// Builds the term of the whole body: the signature and the outcome tree.
    fn root(&mut self) -> Result<Id> {
        let body = self.body;
        let mut signature: Vec<String> =
            body.arg_locals().iter().map(|decl| self.printer.ty(decl.ty)).collect();
        signature.push(self.printer.ty(body.ret_local().ty));
        let signature = self.leaf(format!("signature {}{}", signature.join(", "), self.generics));

        let mut state = self.leaf("state start");
        let mut locals = vec![None; body.locals().len()];
        for arg in 1..=body.arg_locals().len() {
            let value = self.leaf(format!("arg {arg}"));
            if self.in_memory[arg] {
                // An argument in memory starts with its value stored in its local.
                let address = self.local_address(arg);
                let ty = self.ty_leaf(body.locals()[arg].ty);
                state = self.app("store", vec![ty, address, value, state]);
            } else {
                locals[arg] = Some(value);
            }
        }
        let env = Env { locals, state, facts: Vec::new() };
        let outcome = self.block(0, env)?;
        Ok(self.app("body", vec![signature, outcome]))
    }

    /// The address of a local in memory. The locals get numbers in the order of their first use
    /// on the paths, not their MIR numbers: two equal bodies can number their locals in another
    /// order. Each local gets its own number, so two locals never share an address.
    fn local_address(&mut self, local: Local) -> Id {
        let next = self.slots.len();
        let slot = *self.slots.entry(local).or_insert(next);
        let name = self.leaf(format!("slot {slot}"));
        self.app("local", vec![name])
    }

    /// Returns the outcome term of the path that starts at a block.
    fn block(&mut self, index: usize, mut env: Env) -> Result<Id> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return Err(Unsupported);
        }
        let block = &self.body.blocks[index];
        for statement in &block.statements {
            self.statement(&statement.kind, &mut env)?;
        }
        let span = block.terminator.source_info.span;
        match &block.terminator.kind {
            TerminatorKind::Goto { target } => self.block(*target, env),
            TerminatorKind::SwitchInt { discr, targets } => {
                let value = self.operand(discr, &mut env)?;
                let ty = discr.ty(self.body.locals()).map_err(|_| Unsupported)?;
                let branches: Vec<(u128, usize)> = targets.branches().collect();
                if let Some(number) = self.constant_value(value, ty) {
                    // Rule 8.
                    self.rules.insert("constant-switch");
                    let target = branches
                        .iter()
                        .find(|(branch, _)| *branch == number)
                        .map_or(targets.otherwise(), |(_, target)| *target);
                    return self.block(target, env);
                }
                if ty.kind().is_bool() && branches.len() == 1 && branches[0].0 == 0 {
                    // `switchInt(c) -> [0: else, otherwise: then]`.
                    if let Some(known) = self.known(&env, value) {
                        // Rule 1: the condition has a known value on this path.
                        self.rules.insert("path-fact");
                        let target = if known { targets.otherwise() } else { branches[0].1 };
                        return self.block(target, env);
                    }
                    let then_env = self.with_fact(&env, value, true);
                    let else_env = self.with_fact(&env, value, false);
                    let then_outcome = self.block(targets.otherwise(), then_env)?;
                    let else_outcome = self.block(branches[0].1, else_env)?;
                    return Ok(self.nodes.add(Node::Ite(value, then_outcome, else_outcome)));
                }
                // The general case: test each value in turn. The values are different, so the
                // order of the tests does not change the result.
                let integer = is_integer(ty);
                // A switch on a discriminant tests the variants by name (see `print.rs`), in the
                // order of the names.
                let names = match discr {
                    Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => {
                        match self.printer.discriminant_use(place.local) {
                            Some(DiscriminantUse::Switch(table)) => Some(table.clone()),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let mut branches: Vec<(u128, usize)> = branches;
                let mut labels: Vec<String> = Vec::new();
                if let Some(table) = &names {
                    let name = |number: u128| table.iter().find(|(v, _)| *v == number).map(|(_, path)| path.clone());
                    if branches.iter().all(|(number, _)| name(*number).is_some()) {
                        branches.sort_by_key(|(number, _)| name(*number));
                        labels = branches.iter().map(|(number, _)| format!("variant {}", name(*number).unwrap())).collect();
                    }
                }
                let mut conditions = Vec::new();
                for (position, (number, _)) in branches.iter().enumerate() {
                    let condition = if let Some(label) = labels.get(position) {
                        let variant = self.leaf(label.clone());
                        self.app("is-variant", vec![value, variant])
                    } else {
                        let ty_text = self.printer.ty(ty);
                        let constant = self.leaf(format!("const {ty_text} = {number}"));
                        if integer {
                            self.nodes.add(Node::Cmp("eq", value, constant))
                        } else {
                            self.app("switch-eq", vec![value, constant])
                        }
                    };
                    conditions.push(condition);
                }
                let mut otherwise_env = env.clone();
                for &condition in &conditions {
                    otherwise_env = self.with_fact(&otherwise_env, condition, false);
                }
                let mut outcome = self.block(targets.otherwise(), otherwise_env)?;
                for (position, (_, target)) in branches.iter().enumerate().rev() {
                    let mut branch_env = env.clone();
                    for &earlier in &conditions[..position] {
                        branch_env = self.with_fact(&branch_env, earlier, false);
                    }
                    branch_env = self.with_fact(&branch_env, conditions[position], true);
                    let branch = self.block(*target, branch_env)?;
                    outcome = self.nodes.add(Node::Ite(conditions[position], branch, outcome));
                }
                Ok(outcome)
            }
            TerminatorKind::Resume => Ok(self.app("resume", vec![env.state])),
            TerminatorKind::Abort => Ok(self.app("abort", vec![env.state])),
            TerminatorKind::Unreachable => Ok(self.leaf("unreachable")),
            TerminatorKind::Return => {
                let value = self.read(&Place::from(0), &mut env)?;
                Ok(self.app("ret", vec![value, env.state]))
            }
            TerminatorKind::Drop { place, target, unwind } => {
                let unwind = self.unwind(unwind, &env)?;
                let ty = place.ty(self.body.locals()).map_err(|_| Unsupported)?;
                let ty = self.ty_leaf(ty);
                let dropped = match self.locate(place, &mut env)? {
                    Location::Memory(address) => self.app("at-address", vec![address]),
                    Location::Value(..) => self.read(place, &mut env)?,
                };
                env.state = self.app("drop", vec![ty, dropped, unwind, env.state]);
                if place.projection.is_empty() && !self.in_memory[place.local] {
                    let dropped = self.leaf("dropped");
                    env.locals[place.local] = Some(dropped);
                }
                self.block(*target, env)
            }
            TerminatorKind::Call { func, args, destination, target: Some(target), .. }
                if args.len() == 1 && crate::is_opaque_wrap(func) =>
            {
                // The opaque value of `config.rs`: a pure function of the argument that no
                // rule can read.
                let value = match &args[0] {
                    Operand::Constant(constant) => {
                        let text = self.printer.mir_const(&constant.const_);
                        self.leaf(text)
                    }
                    other => self.operand(other, &mut env)?,
                };
                // The mark gives the source of the value (see `Printer::opaque_mark`).
                let mark = self.printer.opaque_mark(span, &args[0]);
                let mark = self.leaf(format!("source{mark}"));
                let result = self.app("opaque", vec![value, mark]);
                self.write(destination, result, &mut env)?;
                self.block(*target, env)
            }
            TerminatorKind::Call { func, args, destination, target, unwind } => {
                let function = self.operand(func, &mut env)?;
                let mut values = Vec::new();
                for arg in args {
                    values.push(self.operand(arg, &mut env)?);
                }
                let unwind = self.unwind(unwind, &env)?;
                let arguments = self.app("args", values);
                let call = self.app("call", vec![function, arguments, unwind, env.state]);
                env.state = call;
                let result = self.app("result", vec![call]);
                self.write(destination, result, &mut env)?;
                match target {
                    Some(target) => self.block(*target, env),
                    None => Ok(self.app("diverge", vec![env.state])),
                }
            }
            TerminatorKind::Assert { cond, expected, msg, target, unwind } => {
                let value = self.operand(cond, &mut env)?;
                let condition = if *expected { value } else { self.not(value) };
                if self.known(&env, condition) == Some(true) {
                    // Rule 1: the condition is true on this path, so the assert cannot fail.
                    self.rules.insert("path-fact");
                    return self.block(*target, env);
                }
                let message = self.message(msg, &mut env)?;
                let unwind = self.unwind(unwind, &env)?;
                env.state = self.nodes.add(Node::Assert(condition, message, unwind, env.state));
                let env = self.with_fact(&env, condition, true);
                self.block(*target, env)
            }
            TerminatorKind::InlineAsm { .. } => Err(Unsupported),
        }
    }

    /// Returns the term of an unwind action. A cleanup path starts from the state `hole`.
    fn unwind(&mut self, unwind: &UnwindAction, env: &Env) -> Result<Id> {
        Ok(match unwind {
            UnwindAction::Continue => self.leaf("uw-continue"),
            UnwindAction::Unreachable => self.leaf("uw-unreachable"),
            UnwindAction::Terminate => self.leaf("uw-terminate"),
            UnwindAction::Cleanup(target) => {
                let mut cleanup = env.clone();
                cleanup.state = self.leaf("hole");
                let outcome = self.block(*target, cleanup)?;
                self.app("uw-cleanup", vec![outcome])
            }
        })
    }

    /// Returns the panic message key of an assert. Two asserts with the same key panic with the
    /// same message. The overflow and division messages are constant strings in `core`
    /// (`panic_const_*`), so their key has no operands. The bounds check and the misaligned
    /// pointer messages print their operands, so their key has the operand values.
    fn message(&mut self, msg: &AssertMessage, env: &mut Env) -> Result<Id> {
        Ok(match msg {
            AssertMessage::BoundsCheck { len, index } => {
                let len = self.operand(len, env)?;
                let index = self.operand(index, env)?;
                self.app("panic bounds", vec![len, index])
            }
            AssertMessage::Overflow(op, _, _) => self.leaf(format!("panic overflow {op:?}")),
            AssertMessage::OverflowNeg(_) => self.leaf("panic overflow neg"),
            AssertMessage::DivisionByZero(_) => self.leaf("panic division by zero"),
            AssertMessage::RemainderByZero(_) => self.leaf("panic remainder by zero"),
            AssertMessage::ResumedAfterReturn(kind) => {
                self.leaf(format!("panic resumed after return {kind:?}"))
            }
            AssertMessage::ResumedAfterPanic(kind) => {
                self.leaf(format!("panic resumed after panic {kind:?}"))
            }
            AssertMessage::ResumedAfterDrop(kind) => {
                self.leaf(format!("panic resumed after drop {kind:?}"))
            }
            AssertMessage::MisalignedPointerDereference { required, found } => {
                let required = self.operand(required, env)?;
                let found = self.operand(found, env)?;
                self.app("panic misaligned", vec![required, found])
            }
            AssertMessage::NullPointerDereference => self.leaf("panic null pointer dereference"),
            AssertMessage::NullReferenceConstructed => self.leaf("panic null reference"),
            AssertMessage::InvalidEnumConstruction(source) => {
                let source = self.operand(source, env)?;
                self.app("panic invalid enum", vec![source])
            }
        })
    }

    fn statement(&mut self, kind: &StatementKind, env: &mut Env) -> Result<()> {
        match kind {
            StatementKind::Assign(place, Rvalue::Discriminant(source)) => {
                let value = self.discriminant(source, place, env)?;
                self.write(place, value, env)
            }
            StatementKind::Assign(place, rvalue) => {
                let value = self.rvalue(rvalue, env)?;
                self.write(place, value, env)
            }
            StatementKind::SetDiscriminant { place, variant_index } => {
                let ty = place.ty(self.body.locals()).map_err(|_| Unsupported)?;
                let TyKind::RigidTy(RigidTy::Adt(def, _)) = ty.kind() else { return Err(Unsupported) };
                let old = self.read(place, env)?;
                let variant = self.leaf(format!("variant {}", variant_path(def, *variant_index)));
                let value = self.app("set-discriminant", vec![old, variant]);
                self.write(place, value, env)
            }
            StatementKind::Intrinsic(NonDivergingIntrinsic::Assume(operand)) => {
                let value = self.operand(operand, env)?;
                if self.nodes.get(value) == &Node::Leaf(TRUE.to_string()) {
                    // Rule 7.
                    self.rules.insert("assume-true");
                    return Ok(());
                }
                env.state = self.app("assume", vec![value, env.state]);
                Ok(())
            }
            StatementKind::Intrinsic(NonDivergingIntrinsic::CopyNonOverlapping(copy)) => {
                let source = self.operand(&copy.src, env)?;
                let destination = self.operand(&copy.dst, env)?;
                let count = self.operand(&copy.count, env)?;
                env.state = self.app("copy", vec![source, destination, count, env.state]);
                Ok(())
            }
            // The N0 rules removed the statements without a runtime effect.
            StatementKind::StorageLive(_)
            | StatementKind::StorageDead(_)
            | StatementKind::Nop
            | StatementKind::ConstEvalCounter
            | StatementKind::Coverage(_)
            | StatementKind::FakeRead(..)
            | StatementKind::PlaceMention(_)
            | StatementKind::AscribeUserType { .. } => Ok(()),
        }
    }

    fn operand(&mut self, operand: &Operand, env: &mut Env) -> Result<Id> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.read(place, env),
            Operand::Constant(constant) => {
                // With the source of a value that is not configuration-free: then no rule of
                // this file reads the value (the text is not `const bool = true`).
                let text = self.printer.const_operand(constant);
                Ok(self.leaf(text))
            }
            Operand::RuntimeChecks(checks) => Ok(self.leaf(format!("runtime checks {checks:?}"))),
        }
    }

    fn rvalue(&mut self, rvalue: &Rvalue, env: &mut Env) -> Result<Id> {
        Ok(match rvalue {
            Rvalue::Use(operand, _) => self.operand(operand, env)?,
            Rvalue::CopyForDeref(place) => self.read(place, env)?,
            Rvalue::Ref(_, kind, place) => {
                let address = self.address(place, env)?;
                self.app(format!("ref {}", borrow_kind_text(kind)), vec![address])
            }
            Rvalue::AddressOf(kind, place) => {
                let address = self.address(place, env)?;
                self.app(format!("raw pointer {kind:?}"), vec![address])
            }
            Rvalue::Reborrow(target, mutability, place) => {
                let address = self.address(place, env)?;
                let ty = self.ty_leaf(*target);
                self.app(format!("reborrow {mutability:?}"), vec![ty, address])
            }
            Rvalue::Len(place) => {
                let value = self.read(place, env)?;
                self.app("len", vec![value])
            }
            Rvalue::Cast(kind, operand, ty) => {
                let source = operand.ty(self.body.locals()).map_err(|_| Unsupported)?;
                let note = cast_note(kind, source, *ty);
                let value = self.operand(operand, env)?;
                let ty = self.ty_leaf(*ty);
                self.app(format!("cast {}{note}", cast_kind_text(kind)), vec![value, ty])
            }
            Rvalue::BinaryOp(op, left, right) => {
                let left_ty = left.ty(self.body.locals()).map_err(|_| Unsupported)?;
                let a = self.operand(left, env)?;
                let b = self.operand(right, env)?;
                match (comparison(*op), is_integer(left_ty)) {
                    (Some(name), true) if a == b => {
                        // Rule 6.
                        self.rules.insert("reflexive-compare");
                        let result = matches!(name, "eq" | "le" | "ge");
                        self.leaf(if result { TRUE } else { FALSE })
                    }
                    (Some(name), true) => self.nodes.add(Node::Cmp(name, a, b)),
                    _ => self.app(format!("binary {op:?}"), vec![a, b]),
                }
            }
            Rvalue::CheckedBinaryOp(op, left, right) => {
                let a = self.operand(left, env)?;
                let b = self.operand(right, env)?;
                self.app(format!("checked {op:?}"), vec![a, b])
            }
            Rvalue::UnaryOp(op, operand) => {
                let ty = operand.ty(self.body.locals()).map_err(|_| Unsupported)?;
                let value = self.operand(operand, env)?;
                if matches!(op, UnOp::Not) && ty.kind().is_bool() {
                    self.not(value)
                } else {
                    self.app(format!("unary {op:?}"), vec![value])
                }
            }
            Rvalue::Discriminant(place) => {
                // A discriminant that is not the value of a plain local: its value matters.
                let ty = place.ty(self.body.locals()).map_err(|_| Unsupported)?;
                if let Some(constant) = self.single_variant_discriminant(ty) {
                    // Rule 5.
                    self.rules.insert("single-variant-discriminant");
                    return Ok(constant);
                }
                let value = self.read(place, env)?;
                let name = self.discriminant_name(ty, None);
                self.app(name, vec![value])
            }
            Rvalue::Aggregate(kind, operands) => {
                let mut values = Vec::new();
                for operand in operands {
                    values.push(self.operand(operand, env)?);
                }
                // An ADT aggregate: the operands in the order of the field names (see `print.rs`).
                // `values` stays in the order of the fields, for rule 3.
                let head = self.printer.aggregate_kind(kind);
                let (name, children) = match aggregate_fields(kind, values.len()) {
                    Some(names) => {
                        let mut pairs: Vec<(String, Id)> = names.into_iter().zip(values.iter().copied()).collect();
                        pairs.sort();
                        let list: Vec<&str> = pairs.iter().map(|(name, _)| name.as_str()).collect();
                        (format!("aggregate {head} fields [{}]", list.join(", ")), pairs.iter().map(|(_, id)| *id).collect())
                    }
                    None => (format!("aggregate {head}"), values.clone()),
                };
                let id = self.app(name, children);
                let info = match kind {
                    AggregateKind::Tuple | AggregateKind::Closure(..) => {
                        Some(AggregateInfo::Fields(values))
                    }
                    AggregateKind::Adt(def, variant, _, _, None) => match def.kind() {
                        AdtKind::Struct => Some(AggregateInfo::Fields(values)),
                        AdtKind::Enum => Some(AggregateInfo::Variant(variant.to_index(), values)),
                        AdtKind::Union => None,
                    },
                    _ => None,
                };
                if let Some(info) = info {
                    self.aggregates.insert(id, info);
                }
                id
            }
            Rvalue::Repeat(operand, count) => {
                let value = self.operand(operand, env)?;
                let count = self.printer.ty_const(count);
                self.app(format!("repeat {count}"), vec![value])
            }
            Rvalue::ThreadLocalRef(item) => {
                use rustc_public::CrateDef;
                self.leaf(format!("thread local {}", item.name()))
            }
        })
    }

    /// Returns the value of a constant leaf of a bool or an integer type, as `switchInt` sees it.
    /// A negative number gets `None` (its bit pattern depends on the width).
    fn constant_value(&mut self, value: Id, ty: Ty) -> Option<u128> {
        let Node::Leaf(text) = self.nodes.get(value) else {
            return None;
        };
        let text = text.clone();
        if !(ty.kind().is_bool() || is_integer(ty)) {
            return None;
        }
        let ty_text = self.printer.ty(ty);
        let number = text.strip_prefix(&format!("const {ty_text} = "))?;
        match number {
            "true" => Some(1),
            "false" => Some(0),
            _ => number.parse::<u128>().ok(),
        }
    }

    /// The term of `destination = discriminant(source)`.
    fn discriminant(&mut self, source: &Place, destination: &Place, env: &mut Env) -> Result<Id> {
        let ty = source.ty(self.body.locals()).map_err(|_| Unsupported)?;
        if let Some(constant) = self.single_variant_discriminant(ty) {
            // Rule 5.
            self.rules.insert("single-variant-discriminant");
            return Ok(constant);
        }
        let value = self.read(source, env)?;
        let local = destination.projection.is_empty().then_some(destination.local);
        let name = self.discriminant_name(ty, local);
        Ok(self.app(name, vec![value]))
    }

    /// The name of a discriminant node. A discriminant whose only uses are switches is
    /// `discriminant`: the switches test the variants by name. Another discriminant of an enum
    /// has the value of each variant in its name, because its value matters (see `print.rs`).
    fn discriminant_name(&mut self, ty: Ty, local: Option<Local>) -> String {
        let TyKind::RigidTy(RigidTy::Adt(def, _)) = ty.kind() else { return "discriminant".into() };
        if def.kind() != AdtKind::Enum {
            return "discriminant".into();
        }
        let cast = match local.and_then(|local| self.printer.discriminant_use(local)) {
            Some(DiscriminantUse::Switch(_)) => return "discriminant".into(),
            Some(DiscriminantUse::Value { cast }) => *cast,
            None => false,
        };
        use rustc_public::CrateDef;
        let table = table_text(&discriminant_table(def));
        format!("discriminant{} of {} values {table}", if cast { " cast" } else { "" }, def.name())
    }

    /// Returns the constant discriminant of an enum type whose layout has one variant.
    fn single_variant_discriminant(&mut self, ty: Ty) -> Option<Id> {
        let TyKind::RigidTy(RigidTy::Adt(def, _)) = ty.kind() else {
            return None;
        };
        if def.kind() != AdtKind::Enum {
            return None;
        }
        let VariantsShape::Single { index } = ty.layout().ok()?.shape().variants else {
            return None;
        };
        let discriminant = def.discriminant_for_variant(index);
        // A large value can be a negative number in two's complement. Keep it simple.
        if discriminant.val > i64::MAX as u128 {
            return None;
        }
        let ty = self.printer.ty(discriminant.ty);
        Some(self.leaf(format!("const {ty} = {}", discriminant.val)))
    }

    /// Returns the address term of a place in memory.
    fn address(&mut self, place: &Place, env: &mut Env) -> Result<Id> {
        match self.locate(place, env)? {
            Location::Memory(address) => Ok(address),
            // The body takes the address of this place, so `address_taken_locals` put its local
            // in memory. This case does not happen.
            Location::Value(..) => Err(Unsupported),
        }
    }

    /// Finds where a place is. A deref reads the pointer, so it can produce a `Load`.
    fn locate(&mut self, place: &Place, env: &mut Env) -> Result<Location> {
        let locals = self.body.locals();
        let mut ty = locals[place.local].ty;
        let mut location = if self.in_memory[place.local] {
            Location::Memory(self.local_address(place.local))
        } else {
            Location::Value(place.local, Vec::new())
        };
        let mut variant = None;
        for elem in &place.projection {
            if let ProjectionElem::Deref = elem {
                let pointer = self.read_location(&location, ty, env)?;
                location = Location::Memory(self.app("deref", vec![pointer]));
            } else {
                let projection = self.projection(elem, ty, variant, env)?;
                location = match location {
                    Location::Value(local, mut path) => {
                        path.push(projection);
                        Location::Value(local, path)
                    }
                    Location::Memory(address) => {
                        Location::Memory(self.app("at", vec![projection, address]))
                    }
                };
            }
            variant = match elem {
                ProjectionElem::Downcast(index) => Some(*index),
                _ => None,
            };
            ty = elem.ty(ty).map_err(|_| Unsupported)?;
        }
        Ok(location)
    }

    /// Returns the term of a projection (not a deref). `base` is the type before the projection,
    /// and `variant` is the variant of a downcast just before it.
    fn projection(&mut self, elem: &ProjectionElem, base: Ty, variant: Option<VariantIdx>, env: &mut Env) -> Result<Id> {
        Ok(match elem {
            ProjectionElem::Deref => unreachable!("the caller handles a deref"),
            ProjectionElem::Field(index, ty) => {
                let union = matches!(base.kind(), TyKind::RigidTy(RigidTy::Adt(def, _)) if def.kind() == AdtKind::Union);
                let text = self.printer.ty(*ty);
                // A member by its name (see `print.rs`); a tuple or a closure field by position.
                let name = match field_name(base, variant, *index) {
                    FieldName::Member(name) => name,
                    FieldName::Position => index.to_string(),
                };
                let id = self.leaf(format!(
                    "{} {name} {text}",
                    if union { "union field" } else { "field" }
                ));
                if !union {
                    self.struct_fields.insert(id, *index);
                }
                id
            }
            ProjectionElem::Index(local) => {
                let index = self.read(&Place::from(*local), env)?;
                self.app("index", vec![index])
            }
            ProjectionElem::ConstantIndex { offset, min_length, from_end } => {
                self.leaf(format!("constant index {offset} {min_length} {from_end}"))
            }
            ProjectionElem::Subslice { from, to, from_end } => {
                self.leaf(format!("subslice {from} {to} {from_end}"))
            }
            ProjectionElem::Downcast(variant) => {
                let TyKind::RigidTy(RigidTy::Adt(def, _)) = base.kind() else { return Err(Unsupported) };
                let id = self.leaf(format!("downcast {}", variant_path(def, *variant)));
                self.downcasts.insert(id, variant.to_index());
                id
            }
            ProjectionElem::OpaqueCast(ty) => {
                let text = self.printer.ty(*ty);
                self.leaf(format!("opaque cast {text}"))
            }
        })
    }

    fn read(&mut self, place: &Place, env: &mut Env) -> Result<Id> {
        let ty = place.ty(self.body.locals()).map_err(|_| Unsupported)?;
        let location = self.locate(place, env)?;
        self.read_location(&location, ty, env)
    }

    fn read_location(&mut self, location: &Location, ty: Ty, env: &mut Env) -> Result<Id> {
        Ok(match location {
            Location::Value(local, path) => {
                let base = match env.locals[*local] {
                    Some(value) => value,
                    None => self.leaf("uninit"),
                };
                self.get_path(base, path)
            }
            Location::Memory(address) => {
                let ty = self.ty_leaf(ty);
                self.nodes.add(Node::Load(ty, *address, env.state))
            }
        })
    }

    /// Reads a path of projections from a value. It uses rules 2 and 3.
    fn get_path(&mut self, value: Id, path: &[Id]) -> Id {
        let Some((&first, rest)) = path.split_first() else {
            return value;
        };
        if let Some((written, new, old)) = self.sets.get(&value).cloned() {
            // Rule 2: the path starts with the written path, so the read is inside the new value.
            if path.starts_with(&written) {
                self.rules.insert("read-after-write");
                return self.get_path(new, &path[written.len()..]);
            }
            // Rule 2: the paths go to two different fields of the same struct, so they do not
            // overlap. The read gives the old value.
            let common = path.iter().zip(&written).take_while(|(a, b)| a == b).count();
            if common < written.len() && common < path.len() {
                let (a, b) = (path[common], written[common]);
                if let (Some(x), Some(y)) = (self.struct_fields.get(&a), self.struct_fields.get(&b))
                {
                    if x != y {
                        self.rules.insert("read-after-write");
                        return self.get_path(old, path);
                    }
                }
            }
        }
        if let Some(info) = self.aggregates.get(&value).cloned() {
            // Rule 3: a field of an aggregate that the body built.
            match info {
                AggregateInfo::Fields(operands) => {
                    if let Some(&index) = self.struct_fields.get(&first) {
                        if let Some(&operand) = operands.get(index) {
                            self.rules.insert("aggregate-field");
                            return self.get_path(operand, rest);
                        }
                    }
                }
                AggregateInfo::Variant(variant, operands) => {
                    if self.downcasts.get(&first) == Some(&variant) {
                        if let Some((&field, after)) = rest.split_first() {
                            if let Some(&index) = self.struct_fields.get(&field) {
                                if let Some(&operand) = operands.get(index) {
                                    self.rules.insert("aggregate-field");
                                    return self.get_path(operand, after);
                                }
                            }
                        }
                    }
                }
            }
        }
        let inner = self.app("get", vec![first, value]);
        self.get_path(inner, rest)
    }

    fn write(&mut self, place: &Place, value: Id, env: &mut Env) -> Result<()> {
        let ty = place.ty(self.body.locals()).map_err(|_| Unsupported)?;
        match self.locate(place, env)? {
            Location::Value(local, path) if path.is_empty() => env.locals[local] = Some(value),
            Location::Value(local, path) => {
                let old = match env.locals[local] {
                    Some(old) => old,
                    None => self.leaf("uninit"),
                };
                let path_term = self.app("path", path.clone());
                let set = self.app("set", vec![path_term, value, old]);
                self.sets.insert(set, (path, value, old));
                env.locals[local] = Some(set);
            }
            Location::Memory(address) => {
                let ty = self.ty_leaf(ty);
                env.state = self.app("store", vec![ty, address, value, env.state]);
            }
        }
        Ok(())
    }

    /// Returns a copy of the environment with one more known condition.
    fn with_fact(&mut self, env: &Env, condition: Id, value: bool) -> Env {
        let mut env = env.clone();
        let (key, positive) = self.fact_key(condition);
        env.facts.push((key, positive == value));
        env
    }

    /// Returns the known value of a condition on the path, if the facts give it.
    fn known(&mut self, env: &Env, condition: Id) -> Option<bool> {
        let (key, positive) = self.fact_key(condition);
        env.facts.iter().rev().find(|(fact, _)| *fact == key).map(|(_, value)| *value == positive)
    }

    /// Returns the canonical form of a bool condition: a key and a polarity. The condition is
    /// true when the key is true and the polarity is `true`, or when the key is false and the
    /// polarity is `false`. See rule 1 for the integer identities.
    fn fact_key(&self, condition: Id) -> (FactKey, bool) {
        match self.nodes.get(condition) {
            Node::Not(inner) => {
                let (key, positive) = self.fact_key(*inner);
                (key, !positive)
            }
            Node::Cmp(op, a, b) => {
                let (a, b) = (*a, *b);
                let eq = (a.min(b), a.max(b));
                match *op {
                    "lt" => (FactKey::Lt(a, b), true),
                    "ge" => (FactKey::Lt(a, b), false),
                    "gt" => (FactKey::Lt(b, a), true),
                    "le" => (FactKey::Lt(b, a), false),
                    "eq" => (FactKey::Eq(eq.0, eq.1), true),
                    "ne" => (FactKey::Eq(eq.0, eq.1), false),
                    _ => (FactKey::Atom(condition), true),
                }
            }
            _ => (FactKey::Atom(condition), true),
        }
    }
}

/// Returns the name of an integer comparison.
fn comparison(op: BinOp) -> Option<&'static str> {
    Some(match op {
        BinOp::Lt => "lt",
        BinOp::Le => "le",
        BinOp::Gt => "gt",
        BinOp::Ge => "ge",
        BinOp::Eq => "eq",
        BinOp::Ne => "ne",
        _ => return None,
    })
}

/// Returns true for the integer types (signed and unsigned). Floats and chars are not integers.
fn is_integer(ty: Ty) -> bool {
    matches!(ty.kind(), TyKind::RigidTy(RigidTy::Int(_) | RigidTy::Uint(_)))
}

/// Returns the locals whose storage the body borrows (a reference or a raw pointer to the local
/// or to a part of it, without a deref in the place).
pub fn address_taken_locals(body: &Body) -> Vec<Local> {
    let mut taken = vec![false; body.locals().len()];
    for block in &body.blocks {
        for statement in &block.statements {
            if let StatementKind::Assign(_, rvalue) = &statement.kind {
                let place = match rvalue {
                    Rvalue::Ref(_, _, place)
                    | Rvalue::AddressOf(_, place)
                    | Rvalue::Reborrow(_, _, place) => place,
                    _ => continue,
                };
                if !place.projection.iter().any(|elem| matches!(elem, ProjectionElem::Deref)) {
                    taken[place.local] = true;
                }
            }
        }
    }
    taken.iter().enumerate().filter(|(_, taken)| **taken).map(|(local, _)| local).collect()
}
