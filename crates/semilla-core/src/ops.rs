//! The rule table: every claim operation, as data.
//!
//! The verifier, the subgroup suggestions, the agent tool schema, and the docs all use this table.

use serde::{Deserialize, Serialize};

use crate::claim::Role;

/// The category of a fact (design 35). It says what kind of change the text shows. It never
/// says if the change has an effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// A new name of an item, a member, a local, or a parameter, with the forced edits.
    Rename,
    /// An item or a file in a different module.
    Move,
    /// The visibility of an item or a member.
    Visibility,
    /// A signature, generics, bounds, a member type, the set or the order of members, a qualifier.
    Declaration,
    /// A new item, member, `impl`, or test.
    Added,
    /// A removed item, member, `impl`, or test.
    Removed,
    /// A literal or the value of a constant.
    Value,
    /// Statements, operands, calls.
    Logic,
    /// Branches, loops, exits.
    ControlFlow,
    /// An attribute.
    Attribute,
    /// A doc comment or a comment.
    Docs,
    /// A `use` line, a `mod` line, an `extern crate` line.
    Import,
    /// A macro definition or a macro call.
    Macro,
    /// A key of a manifest or of a cargo or rustup configuration.
    Manifest,
    /// A file that is not Rust code.
    File,
    /// A change of build code.
    BuildCode,
    /// The compiled code of a body changes, and its text does not.
    Indirect,
    /// Changed source text with no other fact.
    #[default]
    Text,
}

impl Category {
    /// Each category, in the order of the summary table.
    pub const ALL: &'static [Category] = &[
        Category::Rename,
        Category::Move,
        Category::Visibility,
        Category::Declaration,
        Category::Added,
        Category::Removed,
        Category::Value,
        Category::Logic,
        Category::ControlFlow,
        Category::Attribute,
        Category::Docs,
        Category::Import,
        Category::Macro,
        Category::Manifest,
        Category::File,
        Category::BuildCode,
        Category::Indirect,
        Category::Text,
    ];

    /// The text of the category in each output.
    pub fn name(self) -> &'static str {
        match self {
            Category::Rename => "rename",
            Category::Move => "move",
            Category::Visibility => "visibility",
            Category::Declaration => "declaration",
            Category::Added => "added",
            Category::Removed => "removed",
            Category::Value => "value",
            Category::Logic => "logic",
            Category::ControlFlow => "control flow",
            Category::Attribute => "attribute",
            Category::Docs => "docs",
            Category::Import => "import",
            Category::Macro => "macro",
            Category::Manifest => "manifest",
            Category::File => "file",
            Category::BuildCode => "build code",
            Category::Indirect => "indirect",
            Category::Text => "text",
        }
    }

    /// The category with this text, or `None`.
    pub fn from_name(name: &str) -> Option<Category> {
        Category::ALL.iter().copied().find(|category| category.name() == name)
    }

    /// True if the text of the change gives no cause for different compiled code. Such a fact
    /// with the result `code different` gets the mismatch mark (design 35.2).
    pub fn expects_equal(self) -> bool {
        matches!(self, Category::Rename | Category::Move | Category::Visibility | Category::Import | Category::Docs | Category::Attribute)
    }

    /// The role of the subgroup of a fact with this category (design 35.7).
    pub fn role(self) -> Role {
        match self {
            Category::Rename | Category::Move | Category::Import | Category::Visibility => Role::Mechanical,
            Category::Declaration => Role::Interface,
            Category::Docs => Role::Docs,
            Category::Manifest | Category::File | Category::BuildCode | Category::Attribute => Role::Config,
            Category::Indirect => Role::Propagation,
            Category::Added | Category::Removed | Category::Value | Category::Logic | Category::ControlFlow | Category::Macro | Category::Text => Role::Core,
        }
    }

    /// The weight of the category in a merge of many facts on one tree node: the node shows
    /// the category with the highest weight.
    pub fn weight(self) -> u8 {
        match self {
            Category::Docs => 0,
            Category::Rename | Category::Move | Category::Import | Category::Visibility => 1,
            Category::Attribute | Category::Manifest | Category::File | Category::Text => 2,
            Category::Declaration | Category::Added | Category::Removed | Category::Macro => 3,
            Category::Value => 4,
            Category::Logic | Category::Indirect => 5,
            Category::ControlFlow => 6,
            Category::BuildCode => 7,
        }
    }
}

/// The compare result of a fact (design 35). It is an observation. It is not a verdict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    /// Each body in the scope of the fact has the same compiled code before and after, in
    /// each checked configuration.
    CodeEqual,
    /// One body or more has different compiled code, or is new, or no longer exists.
    CodeDifferent,
    /// Semilla made no compare. The note of the fact gives the cause.
    NoCompare,
    /// The fact has no compiled code: a doc comment, a manifest key, a file that no body reads.
    NoCode,
    /// No compare yet: the command had `--fast`, and phase 2 did not run.
    #[default]
    Pending,
    /// No compare: the step uses build code that the change set changes, and the command had
    /// `--no-build-code` (design 28).
    NotChecked,
}

impl Compare {
    /// Each compare result, in the order of the result line.
    pub const ALL: &'static [Compare] = &[Compare::CodeDifferent, Compare::CodeEqual, Compare::NoCompare, Compare::Pending, Compare::NotChecked, Compare::NoCode];

    /// The text of the compare result in each output.
    pub fn name(self) -> &'static str {
        match self {
            Compare::CodeEqual => "code equal",
            Compare::CodeDifferent => "code different",
            Compare::NoCompare => "no compare",
            Compare::NoCode => "no code",
            Compare::Pending => "pending",
            Compare::NotChecked => "not checked",
        }
    }

    /// The compare result with this text, or `None`.
    pub fn from_name(name: &str) -> Option<Compare> {
        Compare::ALL.iter().copied().find(|compare| compare.name() == name)
    }

    /// The place of the compare result in a group of the review report (design 35.7).
    pub fn order(self) -> u8 {
        match self {
            Compare::CodeDifferent => 0,
            Compare::NoCompare => 1,
            Compare::Pending => 2,
            Compare::NotChecked => 3,
            Compare::CodeEqual => 4,
            Compare::NoCode => 5,
        }
    }
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
    /// has no category.
    Property,
}

/// One row of the rule table.
#[derive(Clone, Debug, Serialize)]
pub struct OpInfo {
    pub name: &'static str,
    pub level: OpLevel,
    pub kind: OpKind,
    /// The category of the change. `None`: the verifier chooses `logic` or `control flow` from
    /// the MIR, or the op is a property.
    pub category: Option<Category>,
    /// The role for the subgroup suggestion.
    pub role: Role,
    /// The argument names that the claim must have, in addition to `at`.
    pub args: &'static [&'static str],
    pub summary: &'static str,
}

macro_rules! ops {
    ($( $name:literal, $level:ident, $kind:ident, $category:expr, $role:ident, [$($arg:literal),*], $summary:literal; )*) => {
        /// All operations.
        pub const OPS: &[OpInfo] = &[
            $( OpInfo {
                name: $name,
                level: OpLevel::$level,
                kind: OpKind::$kind,
                category: $category,
                role: Role::$role,
                args: &[$($arg),*],
                summary: $summary,
            }, )*
        ];
    };
}

const R: Option<Category> = Some(Category::Rename);
const M: Option<Category> = Some(Category::Move);
const A: Option<Category> = Some(Category::Added);
const X: Option<Category> = Some(Category::Removed);
const D: Option<Category> = Some(Category::Declaration);
const V: Option<Category> = Some(Category::Value);
const L: Option<Category> = Some(Category::Logic);
const C: Option<Category> = Some(Category::ControlFlow);
const O: Option<Category> = Some(Category::Docs);
const MC: Option<Category> = Some(Category::Macro);
const COMPUTED: Option<Category> = None;

ops! {
    // ----- items -----
    "rename",                 Item,       Identity,    R, Mechanical,  ["to"],            "Rename an item. All references follow the rename. Semilla derives it: it is not a claim of the agent.";
    "move-item",              Item,       Identity,    M, Mechanical,  ["to"],            "Move an item to another module or crate. Semilla derives it: it is not a claim of the agent.";
    "add-item",               Item,       Identity,    A, Core, [],                "Add a new item (fn, struct, enum, impl, const).";
    "remove-item",            Item,       Identity,    X, Core, [],                "Remove an item.";
    "change-signature",       Item,       OwnContent,  D, Interface,   [],                "Change an item signature that no fact covers: a const or static value, a `macro_rules!` body.";
    "edit-doc",               Item,       OwnContent,  O, Docs,        [],                "Change doc comments.";
    "extract-function",       Item,       AcrossNodes, L, Mechanical,  ["new"],           "Move statements or an expression into a new function.";
    "inline-function",        Item,       AcrossNodes, L, Mechanical,  [],                "Replace a call with the body of the function.";
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
    "extract-variable",       Statement,  OwnContent,  L, Mechanical,  ["name"],          "Move an expression into a new let.";
    "inline-variable",        Statement,  OwnContent,  L, Mechanical,  [],                "Replace a variable with its value.";
    "rename-local",           Statement,  Identity,    R, Mechanical,  ["to"],            "Rename a local variable, or a parameter (optional `from`: the old name; the anchor is then the function).";
    "change-mutability",      Statement,  OwnContent,  L, Mechanical,  [],                "Add or remove `mut`.";
    "change-type-annotation", Statement,  OwnContent,  L, Mechanical,  [],                "Add, remove, or change a type annotation on a let.";
    "add-else",               Statement,  OwnContent,  C, Core,        [],                "Add an else branch.";
    "add-else-if",            Statement,  OwnContent,  C, Core,        [],                "Add an else-if branch.";
    "remove-else",            Statement,  OwnContent,  C, Core,        [],                "Remove an else branch.";
    "remove-if",              Statement,  OwnContent,  C, Core,        ["keep"],          "Remove an if and keep one branch.";
    "invert-if",              Statement,  OwnContent,  C, Mechanical,  [],                "Negate the condition and swap the branches.";
    "swap-branches",          Statement,  OwnContent,  COMPUTED, Core, [],                "Swap the branches of an if.";
    "add-arm",                Statement,  OwnContent,  C, Core,        [],                "Add a match arm.";
    "remove-arm",             Statement,  OwnContent,  C, Core,        [],                "Remove a match arm.";
    "move-arm",               Statement,  OwnContent,  COMPUTED, Core, [],                "Move a match arm.";
    "change-loop-kind",       Statement,  OwnContent,  L, Core,        [],                "Change a loop between for, while, and loop.";
    "if-to-match",            Statement,  OwnContent,  C, Mechanical,  [],                "Convert an if into a match.";
    "match-to-if",            Statement,  OwnContent,  C, Mechanical,  [],                "Convert a match into an if.";
    "guard-clause",           Statement,  OwnContent,  C, Mechanical,  [],                "Replace an if/else with an early exit.";
    "reorder-statements",     Statement,  OwnContent,  L, Mechanical,  [],                "Reorder independent statements.";
    "question-mark",          Statement,  OwnContent,  C, Mechanical,  [],                "Replace a manual match on Option or Result with `?`.";
    // ----- expressions -----
    "change-literal",         Expression, OwnContent,  V, Core,        [],                "Change a literal.";
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
    "replace-macro-input",    Expression, Free,        MC, Core, [],                "Replace the input of one macro call (free edit, always in the spotlight).";
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
        assert_eq!(op("edit-doc").unwrap().role, Role::Docs);
        assert_eq!(Category::Docs.role(), Role::Docs);
        for category in [Category::Rename, Category::Move, Category::Import, Category::Visibility] {
            assert_eq!(category.role(), Role::Mechanical, "{}", category.name());
        }
    }

    #[test]
    fn the_category_list_is_closed_and_each_name_is_unique() {
        assert!(Category::ALL.len() <= 20);
        let mut names: Vec<_> = Category::ALL.iter().map(|category| category.name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), Category::ALL.len());
        assert_eq!(Category::from_name("control flow"), Some(Category::ControlFlow));
        assert_eq!(Compare::from_name("code equal"), Some(Compare::CodeEqual));
        assert_eq!(serde_json::to_string(&Compare::NoCompare).unwrap(), "\"no_compare\"");
        assert_eq!(serde_json::to_string(&Category::BuildCode).unwrap(), "\"build_code\"");
    }

    #[test]
    fn lookup() {
        assert_eq!(op("add-if").unwrap().category, Some(Category::ControlFlow));
        assert!(op("edit-fn-body").is_none(), "edit-fn-body must not exist");
    }
}
