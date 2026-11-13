//! The rule table: every claim operation, as data.
//!
//! The verifier, the subgroup suggestions, the agent tool schema, and the docs all use this table.

use serde::{Deserialize, Serialize};

use crate::claim::Role;

/// The class of a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// No behavior change. A normal form proves it.
    Neutral,
    /// The statements or operands change, but not the shape of the control flow graph.
    Logic,
    /// The control flow graph changes: branches, loops, exits.
    ControlFlow,
    /// A change with no MIR: docs, `Cargo.toml`, attributes.
    Other,
    /// No check ran: the step uses build code that the change set changes, and the command
    /// had `--no-build-code` (design 28). It is never a proof.
    NotChecked,
}

/// The level of the tree that an operation changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpLevel {
    Item,
    Statement,
    Expression,
    File,
}

/// The type of an operation (design section 5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    /// `move`, `rename`, `add`, `remove`.
    Identity,
    /// Changes the attributes of one node.
    OwnContent,
    /// Moves a subtree to another parent (`extract-function`).
    AcrossNodes,
    /// A free edit at the expression level. It masks its subtree.
    Free,
    /// A fact about the code after the step (for example `no-panic`). It changes no code, and it
    /// has no class.
    Property,
}

/// One row of the rule table.
#[derive(Clone, Debug, Serialize)]
pub struct OpInfo {
    pub name: &'static str,
    pub level: OpLevel,
    pub kind: OpKind,
    /// The class of the change. `None` means that the verifier computes it from the MIR.
    pub class: Option<Class>,
    /// The role for the subgroup suggestion.
    pub role: Role,
    /// The argument names that the claim must have, in addition to `at`.
    pub args: &'static [&'static str],
    pub summary: &'static str,
}

macro_rules! ops {
    ($( $name:literal, $level:ident, $kind:ident, $class:expr, $role:ident, [$($arg:literal),*], $summary:literal; )*) => {
        /// All operations.
        pub const OPS: &[OpInfo] = &[
            $( OpInfo {
                name: $name,
                level: OpLevel::$level,
                kind: OpKind::$kind,
                class: $class,
                role: Role::$role,
                args: &[$($arg),*],
                summary: $summary,
            }, )*
        ];
    };
}

const N: Option<Class> = Some(Class::Neutral);
const L: Option<Class> = Some(Class::Logic);
const C: Option<Class> = Some(Class::ControlFlow);
const O: Option<Class> = Some(Class::Other);
const COMPUTED: Option<Class> = None;

ops! {
    // ----- items -----
    "rename",                 Item,       Identity,    N, Mechanical,  ["to"],            "Rename an item. All references follow the rename.";
    "move-item",              Item,       Identity,    N, Mechanical,  ["to"],            "Move an item to another module or crate.";
    "add-item",               Item,       Identity,    COMPUTED, Core, [],                "Add a new item (fn, struct, enum, impl, const).";
    "remove-item",            Item,       Identity,    COMPUTED, Core, [],                "Remove an item.";
    "change-signature",       Item,       OwnContent,  L, Interface,   [],                "Change an item signature that no fact covers: a const or static value, a `macro_rules!` body.";
    "edit-doc",               Item,       OwnContent,  O, Docs,        [],                "Change doc comments.";
    "extract-function",       Item,       AcrossNodes, N, Mechanical,  ["new"],           "Move statements or an expression into a new function.";
    "inline-function",        Item,       AcrossNodes, N, Mechanical,  [],                "Replace a call with the body of the function.";
    "rewrite-item",           Item,       Free,        COMPUTED, Core, ["note"],          "Rewrite an item as a whole (optional `new`: its new path). Always in the spotlight.";
    // ----- files -----
    // ----- statements: add -----
    "add-let",                Statement,  OwnContent,  L, Core,        [],                "Add a let statement.";
    "add-expression-statement", Statement, OwnContent, L, Core,        [],                "Add an expression statement (a call, an assignment).";
    "add-if",                 Statement,  OwnContent,  C, Core,        [],                "Add an if statement.";
    "add-match",              Statement,  OwnContent,  C, Core,        [],                "Add a match statement.";
    "add-loop",               Statement,  OwnContent,  C, Core,        [],                "Add a loop (for, while, loop).";
    "add-return",             Statement,  OwnContent,  C, Core,        [],                "Add a return statement.";
    "add-early-exit",         Statement,  OwnContent,  C, Core,        [],                "Add `if cond { return/break/continue }`.";
    "add-break",              Statement,  OwnContent,  C, Core,        [],                "Add a break statement.";
    "add-continue",           Statement,  OwnContent,  C, Core,        [],                "Add a continue statement.";
    "add-macro-statement",    Statement,  OwnContent,  L, Core,        [],                "Add a macro call statement.";
    // ----- statements: structure -----
    "delete-statement",       Statement,  OwnContent,  COMPUTED, Core, [],                "Delete a statement.";
    "move-statement",         Statement,  OwnContent,  COMPUTED, Core, ["to"],            "Move a statement in its block or to another block.";
    "wrap-statements",        Statement,  OwnContent,  C, Core,        ["count", "with"], "Put statements inside a new if, loop, block, or unsafe block.";
    "unwrap-statements",      Statement,  OwnContent,  C, Core,        [],                "Remove a construct and keep its body.";
    "extract-variable",       Statement,  OwnContent,  N, Mechanical,  ["name"],          "Move an expression into a new let.";
    "inline-variable",        Statement,  OwnContent,  N, Mechanical,  [],                "Replace a variable with its value.";
    "rename-local",           Statement,  Identity,    N, Mechanical,  ["to"],            "Rename a local variable, or a parameter (optional `from`: the old name; the anchor is then the function).";
    "change-mutability",      Statement,  OwnContent,  N, Mechanical,  [],                "Add or remove `mut`.";
    "change-type-annotation", Statement,  OwnContent,  N, Mechanical,  [],                "Add, remove, or change a type annotation on a let.";
    "add-else",               Statement,  OwnContent,  C, Core,        [],                "Add an else branch.";
    "add-else-if",            Statement,  OwnContent,  C, Core,        [],                "Add an else-if branch.";
    "remove-else",            Statement,  OwnContent,  C, Core,        [],                "Remove an else branch.";
    "remove-if",              Statement,  OwnContent,  C, Core,        ["keep"],          "Remove an if and keep one branch.";
    "invert-if",              Statement,  OwnContent,  N, Mechanical,  [],                "Negate the condition and swap the branches.";
    "swap-branches",          Statement,  OwnContent,  COMPUTED, Core, [],                "Swap the branches of an if.";
    "add-arm",                Statement,  OwnContent,  C, Core,        [],                "Add a match arm.";
    "remove-arm",             Statement,  OwnContent,  C, Core,        [],                "Remove a match arm.";
    "move-arm",               Statement,  OwnContent,  COMPUTED, Core, [],                "Move a match arm.";
    "change-loop-kind",       Statement,  OwnContent,  L, Core,        [],                "Change a loop between for, while, and loop.";
    "if-to-match",            Statement,  OwnContent,  N, Mechanical,  [],                "Convert an if into a match.";
    "match-to-if",            Statement,  OwnContent,  N, Mechanical,  [],                "Convert a match into an if.";
    "guard-clause",           Statement,  OwnContent,  N, Mechanical,  [],                "Replace an if/else with an early exit.";
    "reorder-statements",     Statement,  OwnContent,  N, Mechanical,  [],                "Reorder independent statements.";
    "question-mark",          Statement,  OwnContent,  N, Mechanical,  [],                "Replace a manual match on Option or Result with `?`.";
    // ----- expressions -----
    "change-literal",         Expression, OwnContent,  L, Core,        [],                "Change a literal.";
    "change-path",            Expression, OwnContent,  L, Core,        [],                "Change a path: another variable, const, or function.";
    "change-operator",        Expression, OwnContent,  COMPUTED, Core, [],                "Change a binary or unary operator.";
    "change-condition",       Expression, OwnContent,  COMPUTED, Core, [],                "Change a condition. The engine computes the effect.";
    "negate",                 Expression, OwnContent,  COMPUTED, Core, [],                "Negate an expression.";
    "swap-operands",          Expression, OwnContent,  COMPUTED, Core, [],                "Swap the operands of a binary operator.";
    "wrap-expression",        Expression, OwnContent,  L, Core,        ["with"],          "Wrap an expression: Ok(x), Some(x), &x, x.clone(), x as T, x?.";
    "unwrap-expression",      Expression, OwnContent,  L, Core,        [],                "Remove a wrapper from an expression.";
    "add-argument",           Expression, OwnContent,  L, Propagation, [],                "Add an argument to a call.";
    "remove-argument",        Expression, OwnContent,  L, Propagation, [],                "Remove an argument from a call.";
    "chain-method",           Expression, OwnContent,  L, Core,        ["method"],        "Add a method call to the end of an expression.";
    "add-clone",              Expression, OwnContent,  L, Core,        [],                "Add `.clone()` (a borrow checker fix).";
    "rewrite-pattern",        Expression, OwnContent,  L, Propagation, ["from", "to"],    "Apply one rewrite rule at many sites.";
    "replace-expression",     Expression, Free,        COMPUTED, Core, [],                "Replace a block-free expression (free edit).";
    "replace-pattern",        Expression, Free,        COMPUTED, Core, [],                "Replace a pattern (free edit).";
    "add-alternative",        Expression, OwnContent,  COMPUTED, Core, [],                "Add alternatives to an or-pattern (`a | b` becomes `a | b | c`).";
    "remove-alternative",     Expression, OwnContent,  COMPUTED, Core, [],                "Remove alternatives from an or-pattern.";
    "replace-type",           Expression, Free,        COMPUTED, Core, [],                "Replace a type (free edit).";
    "replace-macro-input",    Expression, Free,        COMPUTED, Core, [],                "Replace the input of one macro call (free edit, always in the spotlight).";
    // ----- properties of the code after the step -----
    "no-panic",               Item,       Property,    None, Core,     [],                "The local code that can run from the function has no explicit panic site.";
    "no-unsafe",              Item,       Property,    None, Core,     [],                "The local code that can run from the function has no unsafe code.";
    "no-io",                  Item,       Property,    None, Core,     [],                "The local code that can run from the function calls no I/O function (`core` and `alloc`: no I/O; `std`: an allow list of modules).";
    "calls-only",             Item,       Property,    None, Core,     ["calls"],         "Each call to another crate from the local code that can run is under a path in `calls` (full path segments). Calls to `core` and `alloc` are always allowed.";
    "tested-by",              Item,       Property,    None, Core,     ["test"],          "The test `test` calls the function on each path to its normal return (directly or through local calls). A reachable call that is not on each path gives `partial`.";
}

/// Finds an operation by name.
pub fn op(name: &str) -> Option<&'static OpInfo> {
    OPS.iter().find(|info| info.name == name)
}

/// The optional arguments of an op, in addition to its required arguments. Each argument has a
/// check in the verifier; a claim with another argument name is malformed.
pub fn optional_args(name: &str) -> Vec<&'static str> {
    let mut args = Vec::new();
    args.extend(match name {
        "extract-function" => &["count"][..],
        "inline-function" => &["count", "removes"][..],
        "add-if" | "add-early-exit" => &["cond"][..],
        "rewrite-pattern" => &["sites"][..],
        "rewrite-item" => &["new"][..],
        "rename-local" => &["from"][..],
        "extract-variable" | "inline-variable" | "change-mutability" | "change-type-annotation" | "invert-if" | "swap-branches" | "if-to-match" | "match-to-if"
        | "guard-clause" | "reorder-statements" | "question-mark" => &["count"][..],
        _ => &[][..],
    });
    args
}

/// The ops that facts replace (design 22.7). The diff determines these changes, so Semilla
/// derives them as facts, and the agent writes no claim.
pub const RETIRED: &[&str] = &["change-visibility", "add-struct-field", "add-enum-variant", "change-attribute", "add-trait-impl", "edit-config", "bump-dependency"];

/// The error for a retired op, or `None` for an op that is not retired.
/// The message for a retired argument, or `None`. `affects` is retired: Semilla derives the
/// affected bodies (design 8.16).
pub fn retired_arg(name: &str) -> Option<String> {
    (name == "affects").then(|| "the argument `affects` is retired: Semilla derives the affected bodies; remove the argument".to_string())
}

pub fn retired(name: &str) -> Option<String> {
    RETIRED.contains(&name).then(|| format!("the op `{name}` is retired: Semilla derives the fact from the diff (the output of `semilla step` shows it); remove the claim"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let mut names: Vec<_> = OPS.iter().map(|info| info.name).collect();
        names.sort();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn free_ops_are_expression_level() {
        // `rewrite-item` is the one free op at the item level: it is always in the spotlight.
        for info in OPS.iter().filter(|info| info.kind == OpKind::Free && info.name != "rewrite-item") {
            assert_eq!(info.level, OpLevel::Expression, "{} is free above the expression level", info.name);
        }
    }

    #[test]
    fn retired_ops_give_a_clear_error() {
        for name in ["change-visibility", "add-struct-field", "add-enum-variant", "change-attribute", "add-trait-impl"] {
            assert!(op(name).is_none(), "{name} is retired");
            assert!(retired(name).is_some_and(|message| message.contains("Semilla derives the fact")), "{name}");
        }
        assert!(op("change-signature").is_some(), "change-signature stays for the edits that no fact covers");
        assert!(retired("add-if").is_none());
    }

    #[test]
    fn doc_changes_have_their_own_subgroup() {
        // `edit-doc` is not neutral, so it is not in `mechanical` (which holds proved neutral claims).
        assert_eq!(op("edit-doc").unwrap().role, Role::Docs);
        assert!(OPS.iter().filter(|info| info.role == Role::Mechanical).all(|info| info.class == Some(Class::Neutral)), "a mechanical op must be neutral");
    }

    #[test]
    fn lookup() {
        assert_eq!(op("add-if").unwrap().class, Some(Class::ControlFlow));
        assert!(op("edit-fn-body").is_none(), "edit-fn-body must not exist");
    }
}
