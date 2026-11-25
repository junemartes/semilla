# The registry of the name lists

Batch 10n (design 65.5, R65.5-1). Each name list of `crates/*/src` (a constant `&[&str]` or `[&str; N]`, and each `matches!` with three or more text patterns) has a row here with its reason. The test `crates/semilla/tests/names.rs` fails for a list with no row and for a row with no list.

The kinds of a reason:

- `language`: a fact of the Rust language (keywords, tokens, built-in attributes, the prelude).
- `tool`: a fact of a tool (cargo, git, rustc, rustdoc, the model of the check, the own data and options of Semilla). An allow list whose missing name gives the safe result is here too.
- `default mode`: an approximation of the default mode, which has no data of the compiler. The reason names the rule of the compiled mode.

The decisions of batch 10n for each list of the audit are in design 65.3 and 65.4.

| File | List | Kind | Reason |
|---|---|---|---|
| `crates/semilla-core/src/names.rs` | `const IMPLICIT_TRAITS` | language | the traits that the compiler calls with no call in the text (lang items `Drop`, `Deref`, `DerefMut`); the shared list (R65.4-1); the compiled mode: the MIR of the drop glue and of each call |
| `crates/semilla-core/src/names.rs` | `const STD_CRATES` | language | the crates of the standard library (a fact of the tool chain); the shared list of the verifier; the driver asks the compiler for the crates of the sysroot (R65.3-1) |
| `crates/semilla-core/src/names.rs` | `const STD_FACADE` | language | the crates of the standard library whose public modules `std` re-exports at the same path (`core`, `alloc`; a fact of the tool chain): an impl of `core::ops::Add` is an impl of `std::ops::Add` (R68.9-1); a path outside the list keeps its text (the safe side: a removed and an added item) |
| `crates/semilla-core/src/ops.rs` | `const RETIRED` | tool | the names of the claim operations that Semilla does not accept any more (its own options) |
| `crates/semilla-core/src/report.rs` | `const CAUSES` | tool | the names of the evidence rules of Semilla (its own data) |
| `crates/semilla-core/src/report.rs` | `const FILLED_GROUPS` | tool | the names of the groups of the report (its own data) |
| `crates/semilla-mir/src/config.rs` | `const FREE_ALIASES` | tool | type aliases of the standard library with one definition and no `cfg` in the source of the pinned tool chain; the driver cannot see the `cfg` of another crate; an alias outside the list is opaque (the safe side) |
| `crates/semilla-mir/src/config.rs` | `const NOT_CONFIG` | tool | the built-in macros of the compiler (`#[rustc_builtin_macro]`) whose value is a file or a place, not the build configuration: no spotlight, the value stays opaque (never a proof); the names are the built-in names of the compiler |
| `crates/semilla-mir/src/config.rs` | `const PURE_BUILTIN` | tool | the built-in macros of the compiler whose value comes only from their input; each other built-in macro is not configuration-free (an allow list: the safe side) |
| `crates/semilla-mir/src/config.rs` | `fn is_fixed_int_const "MIN"` | language | the associated constants `MIN`, `MAX`, and `BITS` of the integer types with a fixed width have one value in each build |
| `crates/semilla-mir/src/effects.rs` | `const PURE_INTRINSICS` | tool | the intrinsics of the compiler with no effect; the compiler has no mark of purity, and an intrinsic outside the list is the world (the safe side) |
| `crates/semilla-mir/src/effects.rs` | `fn drop_is_free "std::vec::Vec"` | tool | `Vec`, `String`, and `Box` of the global allocator own their memory and only free it; the compiler has no mark for a destructor that only frees owned memory (`#[may_dangle]` is not one: `Rc` has it and writes a shared count); each other type with drop glue is the world (the safe side) |
| `crates/semilla-mir/src/ir.rs` | `fn modeled "core::option::Option"` | tool | the types of the standard library that the model of the check has (`Option`, `Result`, `Ordering`, `ControlFlow`, `Range`); each other type of the standard library is opaque |
| `crates/semilla-mir/src/main.rs` | `const TRACKED_ENV` | tool | the environment variables of the driver itself |
| `crates/semilla-mir/src/main.rs` | `fn inherited_checks ""` | tool | the texts of a true value of an environment variable of Semilla |
| `crates/semilla-mir/src/reach.rs` | `const WHOLE_VALUE` | tool | the inherent methods of `Option` and `Result` only move the value (the source of the pinned tool chain); a call of a trait method is whole by the rule `trait_of_assoc`, so the old entries `From::from`, `Into::into`, `Try::branch`, and `FromResidual::from_residual` are removed (R65.3-9); a callee outside the list is a use of the layout (the safe side) |
| `crates/semilla-mir/src/rival.rs` | `const KEEP` | tool | the names of the fields of a record of the driver that the rival check keeps (its own data) |
| `crates/semilla-mir/src/term.rs` | `fn rvalue "eq"` | language | the comparison operators of the MIR (`eq`, `le`, `ge`) with a symmetric form |
| `crates/semilla-normal/src/cargo.rs` | `fn run_check_once "n0"` | tool | the levels of the compiled check of Semilla (its own data) |
| `crates/semilla-normal/src/elements.rs` | `const SYMMETRIC` | language | the MIR operators whose two operands can swap (`Add`, `Mul`, `Eq`, …) |
| `crates/semilla-normal/src/elements.rs` | `fn diff "return"` | language | the MIR terminators that end a body (`return`, `resume`, `unreachable`) |
| `crates/semilla-normal/src/sandbox.rs` | `const SYSTEM_READ` | tool | the system folders that the sandbox lets a build read |
| `crates/semilla-syntax/src/diff.rs` | `fn apply_fields "if"` | language | the keywords of the Rust language |
| `crates/semilla-syntax/src/diff.rs` | `fn same_kind "let"` | tool | the kinds of the statement nodes of the syntax tree of Semilla |
| `crates/semilla-syntax/src/flow.rs` | `fn block "if"` | language | the block expressions of the Rust language (the node kinds of the syntax tree) |
| `crates/semilla-syntax/src/flow.rs` | `fn block "return"` | language | the jump expressions of the Rust language |
| `crates/semilla-syntax/src/flow.rs` | `fn returns_error "closure"` | language | the expressions that start a new body (a closure, an `async` block) |
| `crates/semilla-syntax/src/load.rs` | `fn load_packages "lib"` | tool | the target kinds of cargo |
| `crates/semilla-syntax/src/load.rs` | `fn module_dir "lib"` | language | the module files that own their folder (`lib.rs`, `main.rs`, `mod.rs`) |
| `crates/semilla-syntax/src/mapper.rs` | `const KEYWORDS` | language | the keywords of the Rust language |
| `crates/semilla-syntax/src/mapper.rs` | `fn pattern_names "("` | language | the tokens after a name that make it a path or a call, not a binding |
| `crates/semilla-syntax/src/sites.rs` | `fn keyword "as"` | language | the keywords of the Rust language |
| `crates/semilla-syntax/src/sites.rs` | `fn lex "r"` | language | the prefixes of the raw and byte string literals of the Rust language |
| `crates/semilla-syntax/src/sites.rs` | `fn places "fn"` | language | the item keywords of the Rust language |
| `crates/semilla-syntax/src/tree.rs` | `const BLOCK_LIKE` | tool | the kinds of the block nodes of the syntax tree of Semilla |
| `crates/semilla-syntax/src/tree.rs` | `fn display_tokens "r"` | language | the prefixes of the raw and byte string literals of the Rust language |
| `crates/semilla-syntax/src/tt.rs` | `const COMPOUND` | language | the compound punctuation tokens of the Rust language |
| `crates/semilla-syntax/src/tt.rs` | `const SHIFTS` | language | the shift tokens of the Rust language |
| `crates/semilla-syntax/src/tt.rs` | `fn build "("` | language | the open delimiters of the Rust language |
| `crates/semilla-syntax/src/tt.rs` | `fn flat_layout "("` | language | the open delimiters of the Rust language |
| `crates/semilla-syntax/src/tt.rs` | `fn flat_layout ")"` | language | the close delimiters of the Rust language |
| `crates/semilla-syntax/src/tt.rs` | `fn joins "("` | language | the delimiters of the Rust language |
| `crates/semilla-ui/src/api.rs` | `fn path "1"` | tool | the texts of a true value of an option of the review server |
| `crates/semilla-ui/src/search.rs` | `const KEYS` | tool | the keys of the search of the review page (its own options) |
| `crates/semilla-verify/src/agent.rs` | `fn plain_names "self"` | language | the keywords that are not names of items (`self`, `true`, `false`, `_`) |
| `crates/semilla-verify/src/checks.rs` | `fn check_add_statement "let"` | tool | the kinds of the statement nodes of the syntax tree of Semilla |
| `crates/semilla-verify/src/checks.rs` | `fn check_add_statement "return"` | language | the jump expressions of the Rust language |
| `crates/semilla-verify/src/checks.rs` | `fn check_compound "add-arm"` | tool | the names of the claim operations of Semilla (its own options) |
| `crates/semilla-verify/src/checks.rs` | `fn check_compound "for"` | language | the loop expressions of the Rust language |
| `crates/semilla-verify/src/checks.rs` | `fn check_wrap_expression "call"` | tool | the kinds of the expression nodes of the syntax tree of Semilla |
| `crates/semilla-verify/src/checks.rs` | `fn declares "fn"` | language | the item keywords of the Rust language |
| `crates/semilla-verify/src/checks.rs` | `fn names_owner "crate"` | language | the path keywords of the Rust language (`crate`, `self`, `super`) |
| `crates/semilla-verify/src/facts.rs` | `const DOC_PAGE_KEYS` | tool | the keys of `doc(…)` of rustdoc that change only the doc pages |
| `crates/semilla-verify/src/facts.rs` | `fn attribute_macro "derive"` | language | the attributes of the language that read the item and are no attribute macro (`derive`, `repr`, `cfg`, `cfg_attr`); each other name that is not a built-in attribute of the shared list (R65.4-1) can be an attribute macro (R65.9-2) |
| `crates/semilla-verify/src/facts.rs` | `fn is_fact_edit "struct"` | tool | the kinds of the item nodes of the syntax tree of Semilla |
| `crates/semilla-verify/src/facts.rs` | `fn marks_of "r"` | language | the prefixes of the raw and byte string literals of the Rust language |
| `crates/semilla-verify/src/facts.rs` | `fn members_fact "struct"` | tool | the kinds of the item nodes of the syntax tree of Semilla |
| `crates/semilla-verify/src/facts.rs` | `fn reordered "struct"` | tool | the kinds of the item nodes of the syntax tree of Semilla |
| `crates/semilla-verify/src/files.rs` | `const DESCRIPTIVE` | default mode | the package keys of cargo that do not change the build; a key that the code reads through `CARGO_PKG_<KEY>` is a build input (R65.3-5, a scan of the text); the compiled mode: a changed expansion with the source "the environment variable" (R65.1-2) |
| `crates/semilla-verify/src/files.rs` | `fn dependency "dependencies"` | tool | the dependency tables of cargo |
| `crates/semilla-verify/src/files.rs` | `fn is_inert_file "md"` | default mode | the kinds of files that no tool reads to build or to test (docs, texts, images), unless the code reads the file (R65.3-4, a scan of the text); the compiled mode: the files that the compiler reads (R63.9-6, R65.1-2) |
| `crates/semilla-verify/src/files.rs` | `fn is_set_array "workspace.members"` | tool | the array keys of cargo whose order has no meaning |
| `crates/semilla-verify/src/files.rs` | `fn spotlight "package.edition"` | tool | the package keys of cargo that change the build |
| `crates/semilla-verify/src/files.rs` | `fn spotlight "profile"` | tool | the tables of cargo that change the build |
| `crates/semilla-verify/src/hints.rs` | `fn only_local_uses ":"` | language | the tokens after a name in a field or a binding |
| `crates/semilla-verify/src/identity.rs` | `fn name_targets "self"` | language | the path keywords of the Rust language |
| `crates/semilla-verify/src/identity.rs` | `fn without_free_parts "crate"` | language | the visibility path keywords of the Rust language (`pub(crate)`, `pub(in …)`) |
| `crates/semilla-verify/src/lib.rs` | `fn api_reach "crate"` | language | the path keywords of the Rust language (`crate`, `self`, `super`) |
| `crates/semilla-verify/src/lib.rs` | `fn declaration_part "generics"` | tool | the names of the parts of a declaration fact of Semilla (its own data) |
| `crates/semilla-verify/src/lib.rs` | `fn in_library "bin"` | tool | the target kinds of cargo |
| `crates/semilla-verify/src/lib.rs` | `fn line_counts "add-item"` | tool | the names of the claim operations of Semilla (its own options) |
| `crates/semilla-verify/src/lib.rs` | `fn verify_core "no-io"` | tool | the names of the claim operations of Semilla (its own options) |
| `crates/semilla-verify/src/linediff.rs` | `const REMOVED_ENV` | tool | the environment variables of git that change the repository of a command |
| `crates/semilla-verify/src/opaque.rs` | `const DOC_FEATURES` | tool | the features of the compiler that change only the documentation |
| `crates/semilla-verify/src/opaque.rs` | `const NUMERIC` | language | the numeric types of the Rust language |
| `crates/semilla-verify/src/opaque.rs` | `const PLAIN` | language | the primitive types of the Rust language with no drop code |
| `crates/semilla-verify/src/opaque.rs` | `const PRELUDE_FNS` | default mode | the functions and the variants of the prelude of the standard library (`drop`, `Some`, `None`, `Ok`, `Err`): a call with one of these names can mean the prelude item; the compiled mode compares the MIR of each configuration |
| `crates/semilla-verify/src/opaque.rs` | `const STD_TYPES` | default mode | the types of the standard library that an `impl` under `cfg` can name as its self type, by their short names; the compiled mode compares the MIR of each configuration |
| `crates/semilla-verify/src/opaque.rs` | `const TRUSTED_CRATES` | language | the crates of the standard library (the shared list `STD_CRATES`): their derive names have their meaning; `serde` and `serde_derive` are not on the list any more (R65.3-7) |
| `crates/semilla-verify/src/opaque.rs` | `const WORDS` | language | the keywords and the primitive types of the Rust language: words that are not names of items |
| `crates/semilla-verify/src/owners.rs` | `fn remote_form "path"` | tool | the kinds of the nodes of the syntax tree of Semilla |
| `crates/semilla-verify/src/positions.rs` | `const DELIMITERS` | language | the delimiter tokens of the Rust language |
| `crates/semilla-verify/src/positions.rs` | `fn assign "["` | language | the tokens before an expression in a list |
| `crates/semilla-verify/src/scope.rs` | `const PLAIN_TYPES` | tool | the modules of the standard library whose types hold no code (no trait object, no function pointer); a type outside the list can hold code (the safe side) |
| `crates/semilla-verify/src/scope.rs` | `const STD_NO_IO` | default mode | the modules of the standard library with no input or output; each other function of `std` counts as I/O (the safe side); the compiled mode: the effect summary of each call (R64.3-1) |
| `crates/semilla-verify/src/sites.rs` | `fn block_path "crate"` | language | the path keywords of the Rust language (`crate`, `self`, `super`) |
| `crates/semilla-verify/src/sites.rs` | `fn resolve "crate"` | language | the path keywords of the Rust language (`crate`, `self`, `super`) |
| `crates/semilla-verify/src/sites.rs` | `fn use_path "crate"` | language | the path keywords of the Rust language (`crate`, `self`, `super`) |
| `crates/semilla-verify/src/srcconfig.rs` | `const KEYWORDS` | language | the keywords of the Rust language (each edition) |
| `crates/semilla-verify/src/srcconfig.rs` | `const LOCAL` | language | the path keywords of the Rust language (`crate`, `self`, `super`) |
| `crates/semilla-verify/src/views.rs` | `fn text_report_lists "partial"` | tool | the names of the proof kinds of Semilla (its own data) |
| `crates/semilla/src/background.rs` | `const OLD` | tool | the old names of the categories of Semilla, for old stores |
| `crates/semilla/src/buildcode.rs` | `fn is_plain_manifest_key "opt-level"` | tool | the keys of a cargo profile |
| `crates/semilla/src/git.rs` | `const EMPTY_TREES` | tool | the hashes of the empty tree of git (SHA-1 and SHA-256) |
| `crates/semilla/src/git.rs` | `const REMOVED_ENV` | tool | the environment variables of git that change the repository of a command |
| `crates/semilla/src/git.rs` | `const SAFE_CONFIG` | tool | the git configuration that each command of Semilla sets |
| `crates/semilla/src/git.rs` | `fn tree_entries "100644"` | tool | the file modes of git |
| `crates/semilla/src/onestep.rs` | `const CLAIMS` | tool | the names of the claims of the one-step command (its own options) |
| `crates/semilla/src/onestep.rs` | `const STORED_CLAIMS` | tool | the names of the stored claims of the one-step command (its own options) |
| `crates/semilla/src/plan.rs` | `fn collapse_expression_claims "replace-expression"` | tool | the names of the claim operations of Semilla (its own options) |
