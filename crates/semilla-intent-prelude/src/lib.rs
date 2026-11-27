//! The prelude of the intent language (design 78): `use semilla_intent::prelude::*;`.
//!
//! An intent file is Rust that rustc compiles against the two versions of the code: `before::`
//! names the crates of the old tree and `after::` the crates of the new tree. Semilla compiles
//! this crate as `semilla_intent` for each intent crate. Rustc checks the names and the types of
//! each claim; Semilla checks the meaning of the attributes and the truth of each claim.
//!
//! Each function of this crate has its meaning from Semilla, by its definition in this crate
//! (never by a list of names). Its Rust body only gives the types; Semilla never runs it.
//!
//! # The claims (attributes on a function)
//!
//! `same`, `change`, `different`, `new`, `removed`, `invariant`, `relation`, `lemma`,
//! `contract`, `loop_relation`, `assume`, and the hints `rename`, `moved`, `rewrite`. The scope
//! of each claim is a list of paths that rustc resolves (see the crate `semilla_intent_macros`).
//! A claim is a function whose body returns `bool`: it is true for each value of its parameters
//! in each checked configuration, with no panic outside [`panics`], [`panics_with`],
//! [`returns`], and [`same_outcome`].

pub use semilla_intent_macros::{assume, change, contract, different, invariant, lemma, loop_relation, moved, new, relation, removed, rename, rewrite, same};

/// Gives the intent of the change, for the reviewer. Semilla does not check it.
#[macro_export]
macro_rules! intent {
    ($text:literal) => {
        const _: &str = $text;
    };
}

/// The state of a loop at a place, for `#[loop_relation]` (batch 13d).
#[macro_export]
macro_rules! loop_state {
    ($($tokens:tt)*) => {
        ()
    };
}

/// True if the call panics.
pub fn panics<R>(call: impl FnOnce() -> R) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)).is_err()
}

/// True if the call panics with this message.
pub fn panics_with<R>(call: impl FnOnce() -> R, message: &str) -> bool {
    let _ = message;
    panics(call)
}

/// True if the call returns (no panic).
pub fn returns<R>(call: impl FnOnce() -> R) -> bool {
    !panics(call)
}

/// True if the two calls have the same outcome: the same value, or a panic of the same kind, and
/// the same effects (R82.2-2: a write through a `&mut` capture is an effect).
pub fn same_outcome<A, B>(first: impl FnOnce() -> A, second: impl FnOnce() -> B) -> bool {
    let _ = (first, second);
    false
}

/// The ordered effects of a call (design 82, R82.2-1): its effects on the world in their order
/// (the calls with an effect of design 64), and the value of each place that the closure writes
/// through a `&mut` capture, after the call. A claim compares two of them with [`same`].
pub fn effects<R>(call: impl FnOnce() -> R) -> Effects {
    let _ = call;
    Effects(())
}

/// True if the two calls have the same effects in the same order (design 82, R82.2-2): the same
/// effects on the world, and the same values (or related values, with the `#[relation]`
/// functions) of the places that each closure writes through its `&mut` captures. The results
/// and the panics of the calls are not effects: [`same_outcome`] compares them, with the effects.
pub fn same_effects<A, B>(first: impl FnOnce() -> A, second: impl FnOnce() -> B) -> bool {
    let _ = (first, second);
    false
}

/// The effects of a call, for [`effects`].
pub struct Effects(());

/// The structural equality of two values, also of an old and a new type (design 78, R78.4-3):
/// each field is equal, through the `#[relation]` functions where the types differ. Rust has no
/// `==` between `before::T` and `after::T`, so a claim says `same(&a, &b)`.
pub fn same<A: ?Sized, B: ?Sized>(first: &A, second: &B) -> bool {
    let _ = (first, second);
    false
}

/// The value of an argument before the call, in an `ensures` of a contract (batch 13d).
pub fn old<T>(value: T) -> T {
    value
}

/// The prelude: each item of this crate.
pub mod prelude {
    pub use crate::{Effects, assume, change, contract, different, effects, invariant, lemma, loop_relation, moved, new, old, panics, panics_with, relation, removed, rename, returns, rewrite, same, same_effects, same_outcome};
    pub use crate::{intent, loop_state};
}
