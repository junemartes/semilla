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
    "change-signature",       Item,       OwnContent,  L, Interface,   [],                "Change the signature of a function.";
    "change-visibility",      Item,       OwnContent,  N, Interface,   [],                "Change the visibility of an item.";
    "add-struct-field",       Item,       OwnContent,  L, Interface,   ["field"],         "Add a field to a struct.";
    "add-enum-variant",       Item,       OwnContent,  L, Interface,   ["variant"],       "Add a variant to an enum.";
    "add-trait-impl",         Item,       Identity,    L, Interface,   ["affects"],       "Add a trait impl. `affects` lists the bodies that it changes.";
    "edit-doc",               Item,       OwnContent,  O, Mechanical,  [],                "Change doc comments or comments.";
    "extract-function",       Item,       AcrossNodes, N, Mechanical,  ["new"],           "Move statements or an expression into a new function.";
    "inline-function",        Item,       AcrossNodes, N, Mechanical,  [],                "Replace a call with the body of the function.";
    // ----- files -----
    "bump-dependency",        File,       OwnContent,  O, Config,      ["crate", "to"],   "Change the version of a dependency in Cargo.toml.";
    "edit-config",            File,       OwnContent,  O, Config,      ["file"],          "Change a file that has no Rust code.";
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
    "rename-local",           Statement,  Identity,    N, Mechanical,  ["to"],            "Rename a local variable.";
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
    "move-arm",               Statement,  OwnContent,  COMPUTED, Core, ["to"],            "Move a match arm.";
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
    "add-clone",              Expression, OwnContent,  L, Mechanical,  [],                "Add `.clone()` (a borrow checker fix).";
    "rewrite-pattern",        Expression, OwnContent,  L, Propagation, ["from", "to"],    "Apply one rewrite rule at many sites.";
    "replace-expression",     Expression, Free,        COMPUTED, Core, [],                "Replace a block-free expression (free edit).";
    "replace-pattern",        Expression, Free,        COMPUTED, Core, [],                "Replace a pattern (free edit).";
    "replace-type",           Expression, Free,        COMPUTED, Core, [],                "Replace a type (free edit).";
}

/// Finds an operation by name.
pub fn op(name: &str) -> Option<&'static OpInfo> {
    OPS.iter().find(|info| info.name == name)
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
        for info in OPS.iter().filter(|info| info.kind == OpKind::Free) {
            assert_eq!(info.level, OpLevel::Expression, "{} is free above the expression level", info.name);
        }
    }

    #[test]
    fn lookup() {
        assert_eq!(op("add-if").unwrap().class, Some(Class::ControlFlow));
        assert!(op("edit-fn-body").is_none(), "edit-fn-body must not exist");
    }
}
