//! The shared name lists of the checks with no compiler data (batch 10n, design 65.4).
//!
//! Phase 1 of a step and the syntax-only check (R83.1-3) have no data of the compiler: they read
//! the text of the source. Before design 83 this was the default mode, and the registry of the
//! name lists keeps the kind name `default mode`. Some of the rules need the meaning of a name
//! that the language or the standard library gives: a macro of the standard prelude, a built-in
//! attribute, a trait that the compiler calls with no call in the text. Each such list is a fact
//! of the language or of the tool chain, and it is here, one list for each concept. Before batch
//! 10n, six macro lists in five files had different contents.
//!
//! Each list of the source (`&[&str]`, `[&str; N]`, and each `matches!` of three or more
//! names) has an entry in the registry `docs/name-lists.md` with its reason; the test
//! `crates/semilla/tests/names.rs` fails for a list with no entry (R65.5-1). A line that a list
//! of this file decides says so, and it names the rule of the compiled mode
//! ([`NAME_LIST`]).

/// The start of the text that a line gets when a name list of this file decides it
/// (R65.4-2). The rest of the text names the rule of the compiled mode.
pub const NAME_LIST: &str = "a name list of the language decided it";

/// The crates of the standard library (a fact of the tool chain). The MIR driver does not use
/// this list: it asks the compiler for the crates of the sysroot (R65.3-1).
pub const STD_CRATES: &[&str] = &["core", "alloc", "std", "proc_macro", "test"];

/// True for the name of a crate of the standard library.
pub fn is_std_crate(name: &str) -> bool {
    STD_CRATES.contains(&name)
}

/// One macro of the standard prelude and the facts of its expansion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StdMacro {
    pub name: &'static str,
    /// The input is a list of expressions (`println!("{}", x)`, design 4.1).
    pub expressions: bool,
    /// The code can panic (`panic!`, `assert!`, `todo!`).
    pub panics: bool,
    /// The macro reads a file at build time (`include_str!`).
    pub reads_file: bool,
    /// The value depends on the place of the call in the source (`file!()`, `line!()`).
    pub place: bool,
    /// The macro reads Rust code from a file (`include!`): its expansion can have `cfg`
    /// attributes. Each other macro of the list has an expansion with no `cfg` attribute.
    pub code: bool,
}

const fn m(name: &'static str, expressions: bool, panics: bool, reads_file: bool, place: bool) -> StdMacro {
    StdMacro { name, expressions, panics, reads_file, place, code: false }
}

/// The macros of the standard prelude (design 38.7): the names that each crate can call with
/// no `use`. A local `macro_rules!` with one of these names replaces the macro for each call in
/// its scope. The compiled mode reads the definition of each call from the expansion record
/// (R65.2-1), so it does not use this list.
pub const STD_MACROS: &[StdMacro] = &[
    m("assert", true, true, false, false),
    m("assert_eq", true, true, false, false),
    m("assert_ne", true, true, false, false),
    m("debug_assert", true, true, false, false),
    m("debug_assert_eq", true, true, false, false),
    m("debug_assert_ne", true, true, false, false),
    m("matches", false, false, false, false),
    m("vec", true, false, false, false),
    m("format", true, false, false, false),
    m("format_args", true, false, false, false),
    m("write", true, false, false, false),
    m("writeln", true, false, false, false),
    m("print", true, false, false, false),
    m("println", true, false, false, false),
    m("eprint", true, false, false, false),
    m("eprintln", true, false, false, false),
    m("panic", true, true, false, false),
    m("unreachable", true, true, false, false),
    m("unimplemented", true, true, false, false),
    m("todo", true, true, false, false),
    m("concat", false, false, false, false),
    m("stringify", false, false, false, false),
    m("line", false, false, false, true),
    m("column", false, false, false, true),
    m("file", false, false, false, true),
    m("module_path", false, false, false, true),
    m("cfg", false, false, false, false),
    m("env", false, false, false, false),
    m("option_env", false, false, false, false),
    StdMacro { name: "include", expressions: false, panics: false, reads_file: true, place: false, code: true },
    m("include_str", false, false, true, false),
    m("include_bytes", false, false, true, false),
    m("compile_error", false, false, false, false),
    m("dbg", true, false, false, false),
];

/// The macro of the standard prelude with this name.
pub fn std_macro(name: &str) -> Option<&'static StdMacro> {
    STD_MACROS.iter().find(|entry| entry.name == name)
}

/// True for a macro of the standard prelude.
pub fn is_std_macro(name: &str) -> bool {
    std_macro(name).is_some()
}

/// True for a macro of the standard prelude whose input is a list of expressions.
pub fn has_expressions(name: &str) -> bool {
    std_macro(name).is_some_and(|entry| entry.expressions)
}

/// True for a macro of the standard prelude whose code can panic.
pub fn panics(name: &str) -> bool {
    std_macro(name).is_some_and(|entry| entry.panics)
}

/// True for a macro of the standard prelude that reads a file at build time.
pub fn reads_file(name: &str) -> bool {
    std_macro(name).is_some_and(|entry| entry.reads_file)
}

/// True for a macro of the standard prelude whose result depends on the path of the file of the
/// call (design 56.3): a read file (by a relative path) and `file!()`.
pub fn depends_on_file_path(name: &str) -> bool {
    std_macro(name).is_some_and(|entry| entry.reads_file || entry.name == "file")
}

/// True for a macro of the standard prelude whose expansion has no `cfg` attribute: each macro
/// of the list but `include!`, which reads code.
pub fn cfg_free_macro(name: &str) -> bool {
    std_macro(name).is_some_and(|entry| !entry.code)
}

/// One built-in attribute of the language and the facts of its meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinAttribute {
    pub name: &'static str,
    /// The attribute cannot change a MIR body or the behavior: docs, lint levels, and hints for
    /// the compiler. The compiled mode compares the MIR, so it does not use this fact.
    pub no_mir: bool,
    /// The attribute does not select code by the build configuration.
    pub no_cfg: bool,
}

const fn a(name: &'static str, no_mir: bool) -> BuiltinAttribute {
    BuiltinAttribute { name, no_mir, no_cfg: true }
}

/// The built-in attributes that the default mode knows. Each other attribute needs a reason
/// when it changes (invariant I3: an allow list), and it can select code.
pub const BUILTIN_ATTRIBUTES: &[BuiltinAttribute] = &[
    a("doc", true),
    a("allow", true),
    a("warn", true),
    a("deny", true),
    a("forbid", true),
    a("expect", true),
    a("must_use", true),
    a("inline", true),
    a("cold", true),
    a("rustfmt::skip", true),
    a("deprecated", true),
    a("test", false),
    a("should_panic", false),
    a("ignore", false),
    a("derive", false),
    a("repr", false),
    a("non_exhaustive", false),
    a("track_caller", false),
    a("automatically_derived", false),
    a("default", false),
    a("macro_export", false),
];

/// True for a built-in attribute that cannot change a MIR body or the behavior.
pub fn attribute_no_mir(name: &str) -> bool {
    BUILTIN_ATTRIBUTES.iter().any(|entry| entry.name == name && entry.no_mir)
}

/// True for a built-in attribute that does not select code by the build configuration.
pub fn attribute_no_cfg(name: &str) -> bool {
    BUILTIN_ATTRIBUTES.iter().any(|entry| entry.name == name && entry.no_cfg)
}

/// The traits that the compiler calls with no call in the text and with no change of the MIR
/// of the caller (lang items, a fact of the language): a new `Drop` runs at each scope end of
/// the type, and a new `Deref` changes the method that a call on a value finds.
pub const IMPLICIT_TRAITS: &[&str] = &["Drop", "Deref", "DerefMut"];

/// True for a trait that the compiler calls with no call in the text.
pub fn is_implicit_trait(name: &str) -> bool {
    IMPLICIT_TRAITS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // rule+ R65.4-1
    // rule- R65.4-1
    #[test]
    fn each_old_list_is_a_view_of_one_shared_list() {
        // The old lists of batch 10c and before, as views of the shared lists: each old entry
        // keeps its answer, and a name that no list had stays out.
        let old_std = [
            "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne", "matches", "vec", "format", "format_args", "write", "writeln", "print", "println", "eprint",
            "eprintln", "panic", "unreachable", "unimplemented", "todo", "concat", "stringify", "line", "column", "file", "module_path", "cfg", "env", "option_env", "include", "include_str",
            "include_bytes", "compile_error", "dbg",
        ];
        for name in old_std {
            assert!(is_std_macro(name), "{name}");
            // `MACRO_FREE` was the same list with no `include`.
            assert_eq!(cfg_free_macro(name), name != "include", "{name}");
        }
        let old_expressions = [
            "println", "print", "eprintln", "eprint", "format", "format_args", "write", "writeln", "panic", "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne",
            "vec", "dbg", "todo", "unimplemented", "unreachable",
        ];
        for name in old_std {
            assert_eq!(has_expressions(name), old_expressions.contains(&name), "{name}");
        }
        for name in ["include", "include_str", "include_bytes", "file"] {
            assert!(depends_on_file_path(name), "{name}");
        }
        for name in ["include", "include_str", "include_bytes"] {
            assert!(reads_file(name), "{name}");
        }
        // The panic macros: the union of the two old lists, with the `debug_assert` macros
        // (they panic in the dev profile), which the two old lists missed.
        for name in ["panic", "unreachable", "todo", "unimplemented", "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne"] {
            assert!(panics(name), "{name}");
        }
        let no_mir = ["doc", "allow", "warn", "deny", "forbid", "expect", "must_use", "inline", "cold", "rustfmt::skip", "deprecated"];
        for name in no_mir {
            assert!(attribute_no_mir(name) && attribute_no_cfg(name), "{name}");
        }
        for name in ["test", "should_panic", "ignore", "derive", "repr", "non_exhaustive", "track_caller", "automatically_derived", "default", "macro_export"] {
            assert!(!attribute_no_mir(name) && attribute_no_cfg(name), "{name}");
        }
        for name in ["Drop", "Deref", "DerefMut"] {
            assert!(is_implicit_trait(name), "{name}");
        }
        // The negative case: names that no list has.
        for name in ["my_assert", "serde", "cfg_attr", "path", "Display", "hashbrown"] {
            assert!(!is_std_macro(name) && !attribute_no_mir(name) && !attribute_no_cfg(name) && !is_implicit_trait(name) && !is_std_crate(name), "{name}");
        }
    }
}
