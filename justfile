# Semilla dev tasks. Run `just` to list recipes.
#
# The workspace builds with the pinned nightly in rust-toolchain.toml. The SMT
# tests need Z3: `just z3` installs the pinned release.

_default:
    @just --list

# Type-check the whole workspace.
check:
    cargo check --workspace --all-targets --locked

# Mechanical style checks from docs/STYLE.md. Run before you open a PR.
style:
    scripts/style-check.sh

# Format the whole workspace.
fmt:
    cargo fmt --all

# Run the test suite.
test:
    cargo test --workspace --locked

# Install the pinned Z3 release in .semilla/tools/ (x86_64 Linux).
z3:
    scripts/fetch-z3.sh
