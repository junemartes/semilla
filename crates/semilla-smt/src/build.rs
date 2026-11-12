//! The build configuration of a check (batch 5k, A1).
//!
//! The value of an expression can depend on the build: the width of `usize` and `isize` is a
//! property of the target, and an overflow panics only in a build with overflow checks. A
//! proof must hold for each build, so the checks run one time for each configuration in
//! [`ALL`].

use std::cell::Cell;

/// One build configuration of the encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Build {
    /// The number of bits of `usize` and `isize`.
    pub pointer_bits: u32,
    /// True if an overflow of `+`, `-`, `*`, a negation, or a shift panics.
    pub overflow_checks: bool,
}

/// The configuration of the check build of Semilla: a 64-bit target with overflow checks.
pub const CHECK: Build = Build { pointer_bits: 64, overflow_checks: true };

/// Each configuration: the pointer widths of the Rust targets, with and without overflow
/// checks. The first one is [`CHECK`].
pub const ALL: [Build; 6] = [
    CHECK,
    Build { pointer_bits: 64, overflow_checks: false },
    Build { pointer_bits: 32, overflow_checks: true },
    Build { pointer_bits: 32, overflow_checks: false },
    Build { pointer_bits: 16, overflow_checks: true },
    Build { pointer_bits: 16, overflow_checks: false },
];

thread_local! {
    static CURRENT: Cell<Build> = const { Cell::new(CHECK) };
}

/// The configuration of the encoding on this thread.
pub(crate) fn current() -> Build {
    CURRENT.with(|current| current.get())
}

/// Runs `f` with the encoding of `build`.
pub(crate) fn with<T>(build: Build, f: impl FnOnce() -> T) -> T {
    let before = CURRENT.with(|current| current.replace(build));
    let result = f();
    CURRENT.with(|current| current.set(before));
    result
}

impl std::fmt::Display for Build {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a pointer width of {} bits, {} overflow checks", self.pointer_bits, if self.overflow_checks { "with" } else { "no" })
    }
}
