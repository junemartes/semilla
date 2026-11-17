# Semilla

Semilla derives the facts of each step of a code change and checks them against the
compiled code. The Rust workspace is under `crates/`, the spikes are under `spikes/`.
See `README.md` for the build, `docs/design.md` for the design, and
`docs/agent-guide.md` for the agent workflow of the tool.

## Style

Every change follows `docs/STYLE.md`. Read it before you write code. The sixteen rules
there are the review bar for this repository.

Before you open a pull request, or push a branch that a pull request will use:

1. Run `just style`. It must pass with no warnings.
2. Run `just test`. The SMT tests need Z3: run `just z3` once.
3. Read the diff against the judgment rules in `docs/STYLE.md`: self-contained comments,
   no mutex where a channel or ownership works, no `dyn`, no defensive checks, methods
   instead of standalone functions, no nested loops, no duplicated shape.
4. Say in the pull request which rules the change touched and how.

A pull request that fails `just style` is not ready. The CI runs the same checks on each
pull request and on each push to `main`.

## Version control

This repository uses jj. There is no branch protection on `main`, so CI does not block
a merge. Wait for a green CI run before you merge. Commits are immutable after push;
use a new commit to fix a pushed change.
