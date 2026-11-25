//! The prelude of the intent language (`use semilla_intent::prelude::*;`).
//!
//! Each name of the language has its meaning here, as a documented Rust item, so that an agent
//! can read it with its usual tools. Semilla never runs these items: it checks a claim with its
//! own model of the two versions. The parser and the check read the table [`FUNCTIONS`], and a
//! unit test makes sure that the table and the items agree.
//!
//! # The claims (attributes on a function)
//!
//! - `#[same(ITEM, …)]`: an empty body is the full compare of the item: each output, effect,
//!   and panic stays the same (the equivalence check of the step). A body is a property that
//!   must be true for each value of the parameters, in each checked configuration.
//! - `#[change(ITEM)]`: a property of an intended change. An `if` splits the cases.
//! - `#[different(ITEM)]`: a sure difference: a property with no parameters (one input).
//!   Semilla evaluates the two versions on the input and confirms the difference.
//! - `#[rename(A, B)]`, `#[moved(A, B)]`, `#[rewrite(A, B)]`: the forms of `--same` (design 48),
//!   with an empty body.
//!
//! The options of each claim attribute: `cfg = "…"` (the claim is about the matching
//! configurations only), `reason = "…"` (shown, not checked), and `except = [panic_message]`
//! (a panic with another message counts as the same panic).
//!
//! # The body of a claim
//!
//! - `before::PATH(…)` and `after::PATH(…)` call the body of that version. Each call starts in
//!   the world of the input: a call does not see the effects of another call of the claim.
//! - A panic of a call outside [`panics`], [`panics_with`], [`returns`], and [`same_outcome`]
//!   makes the claim false for that input.
//! - The operators, the comparisons, `if`, `let`, blocks, field reads (private fields too),
//!   tuple fields, `as` casts of integers, `&`, `*`, an index of a sequence, `Some`, `None`,
//!   `Ok`, `Err`, a variant or a struct of a version, and the methods of [`METHODS`].
//! - `==` on integers, `bool`, `char`, tuples, sequences of scalars, `Option`, and `Result` of
//!   these compares the values. `==` on a type of a version calls its `PartialEq::eq` of that
//!   version. `==` on another type (a float, an opaque type) is outside the model.
//! - A function with no attribute is a named relation: a claim can call it.

/// Gives the intent of the change, for the reviewer. Semilla does not check it.
#[macro_export]
macro_rules! intent {
    ($text:literal) => {};
}

pub use crate::intent;

/// True if the call of `f` panics. The claim is false for an input where `f` returns.
pub fn panics<T>(f: impl FnOnce() -> T) -> bool {
    never(f)
}

/// True if the call of `f` panics with the message `message`. Semilla checks the message only
/// for a panic of the compiler checks (`attempt to add with overflow`) and for a panic call
/// whose message is a constant text; else the claim is "claimed".
pub fn panics_with<T>(f: impl FnOnce() -> T, message: &str) -> bool {
    let _ = message;
    never(f)
}

/// True if the call of `f` returns (it does not panic).
pub fn returns<T>(f: impl FnOnce() -> T) -> bool {
    never(f)
}

/// True if the two calls have the same outcome: equal values, the same effects in the same
/// order, or the same panic (the same kind and the same arguments; with `except =
/// [panic_message]`, any panic of the same kind).
pub fn same_outcome<T>(a: impl FnOnce() -> T, b: impl FnOnce() -> T) -> bool {
    let _ = never(a);
    never(b)
}

/// The value of a place at the start of a contract (batch 10r3). Not in this batch.
pub fn old<T>(x: T) -> T {
    x
}

fn never<T>(_: impl FnOnce() -> T) -> bool {
    unreachable!("Semilla never runs an intent file")
}

/// The functions of the prelude: the name, the count of the arguments, and the batch of the
/// check (`10r1`: checked now). The parser and the check read this table.
pub const FUNCTIONS: &[(&str, usize, &str)] = &[("panics", 1, "10r1"), ("panics_with", 2, "10r1"), ("returns", 1, "10r1"), ("same_outcome", 2, "10r1"), ("old", 1, "10r3")];

/// The methods of the modeled types: the name, the receiver types, and the meaning. A method
/// call on a type of a version calls the method of that version.
///
/// - `len`, `is_empty`: a `Vec`, a slice, or an array of scalars.
/// - `is_some`, `is_none`, `unwrap`: an `Option`; `unwrap` panics on `None`.
/// - `is_ok`, `is_err`, `unwrap`: a `Result`; `unwrap` panics on `Err`.
pub const METHODS: &[(&str, &str)] = &[("len", "sequence"), ("is_empty", "sequence"), ("is_some", "Option"), ("is_none", "Option"), ("is_ok", "Result"), ("is_err", "Result"), ("unwrap", "Option, Result")];

/// The claim attributes: the name, the kind of the claim, and the batch of the check.
pub const CLAIMS: &[(&str, &str)] = &[
    ("same", "10r1"),
    ("change", "10r1"),
    ("different", "10r1"),
    ("rename", "10r1"),
    ("moved", "10r1"),
    ("rewrite", "10r1"),
    ("pair_loop", "10r2"),
    ("lemma", "10r2"),
    ("contract", "10r3"),
    ("invariant", "10r3"),
    ("relation", "10r3"),
    ("assume", "10r3"),
];

/// The names of `except = […]` that the check knows. Each other name gives "claimed".
pub const EXCEPT: &[&str] = &["panic_message"];

/// The variant names of `Option` and `Result` that a claim can use with no path.
pub const VARIANTS: &[(&str, &str)] = &[("None", "Option"), ("Some", "Option"), ("Ok", "Result"), ("Err", "Result")];

#[cfg(test)]
mod tests {
    use super::*;

    /// One source for each name: each `pub fn` of this file has a row of `FUNCTIONS`, and each
    /// row has a `pub fn`.
    #[test]
    fn the_table_and_the_items_agree() {
        let text = include_str!("prelude.rs");
        let items: Vec<&str> = text.lines().filter_map(|line| line.strip_prefix("pub fn ")).map(|rest| rest.split(['<', '(']).next().unwrap_or("")).collect();
        let table: Vec<&str> = FUNCTIONS.iter().map(|(name, _, _)| *name).collect();
        assert_eq!(items, table);
        for (name, args, _) in FUNCTIONS {
            let line = text.lines().find(|line| line.starts_with(&format!("pub fn {name}<"))).unwrap();
            assert_eq!(line.matches("impl FnOnce").count() + line.matches(": &str").count() + line.matches("x: T").count(), *args, "{name}");
        }
    }
}
