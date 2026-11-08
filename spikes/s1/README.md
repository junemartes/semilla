# Spike S1: MIR normalization

Question: do the N0 and N2 normal forms make neutral refactors equal, without making real changes equal?

## How to run

```sh
cargo build -p semilla-mir
cargo run -p semilla-s1
```

The canonical MIR of each case goes to `target/s1/<case>/<level>/`, for a manual diff.

Each case in `cases/` has a `before.rs`, an `after.rs`, and a `case.toml`:

- A **neutral** case passes ("proved") if one level makes all bodies equal.
- A **changed** case is a negative control. It passes ("ok") only if no level makes the bodies equal. A failed control means that a normalization rule is unsound.

## Levels

| Level | rustc flags | Normalization |
|---|---|---|
| `n0` | `-Zmir-opt-level=0 -Copt-level=0` | the rules in `crates/semilla-mir/src/normalize.rs` |
| `n2` | `-Zmir-opt-level=2 -Copt-level=2`, overflow checks and debug assertions on | the same rules, after the rustc MIR optimizations |

## Results (nightly-2026-10-25)

| case | expect | n0 | n2 | design level | result | note |
|---|---|---|---|---|---|---|
| closure_param_rename | neutral | equal | equal | N0 | proved | Rename a closure parameter. |
| ctrl_change_literal | changed | diff: f | diff: f | - | ok | Control: `+ 1` becomes `+ 2`. |
| ctrl_change_operator | changed | diff: f | diff: f | - | ok | Control: `<` becomes `<=`. |
| ctrl_closure_body | changed | diff: f::{closure#0} | diff: f::{closure#0} | - | ok | Control: change the body of a closure (`x + 1` becomes `x + 2`). |
| ctrl_drop_order | changed | diff: f | diff: f | - | ok | Control: swap two lets with Drop types. The drop order changes. |
| ctrl_swap_branches | changed | diff: pick | diff: pick | - | ok | Control: swap the branches without a negated condition. |
| ctrl_wrapping_add | changed | diff: f | diff: f | - | ok | Control: `x + 1` (panics on overflow) becomes `x.wrapping_add(1)`. |
| extract_function | neutral | diff: total | equal | N3 | proved | Extract the loop body expression into a new function. |
| extract_function_large | neutral | diff: rank | diff: rank | N3 | not proved | Extract a larger block into a new function. Tests the limit of the rustc inliner at N2. |
| extract_variable | neutral | diff: f | equal | N0 or N2 | proved | Extract `a * b` into a variable. |
| format_comments | neutral | equal | equal | N0 | proved | Whitespace and comments only. |
| guard_clause | neutral | diff: get | diff: get | N4 or SMT | not proved | Replace if/else with an early return (`!(i < n)` becomes `i >= n`). |
| if_to_match | neutral | diff: f | equal | N0 or N2 | proved | Convert `if c` to `match c { true, false }`. |
| inline_variable | neutral | diff: f | equal | N0 or N2 | proved | Inline a variable into its only use. |
| invert_if | neutral | equal | equal | N4 or SMT | proved | Invert an if: negate the condition and swap the branches. |
| lifetime_elision | neutral | equal | equal | N0 | proved | Remove explicit lifetimes (elision). |
| question_mark | neutral | diff: f | diff: f | N2 | not proved | Replace a manual match on Option with `?`. |
| rename_item | neutral | diff: f | equal | N1 | proved | Rename a function and its call. The harness maps the body name; the call inside `f` still differs. |
| rename_local | neutral | equal | equal | N0 | proved | Rename a local variable. |
| reorder_independent_lets | neutral | diff: f | diff: f | N4 | not proved | Swap two independent lets. Both can panic on overflow, with the same message (only the location differs). |
| type_annotation | neutral | equal | equal | N0 | proved | Add a type annotation to a let. |
| unneeded_mut | neutral | equal | equal | N0 | proved | Remove an unneeded `mut`. |

Summary: 12 of 16 neutral cases are proved. All 6 negative controls stay different.

## Findings

1. **N0 is strong enough for the simple refactors.** Formatting, comments, local renames, closure parameter renames, type annotations, lifetime elision, and `mut` removal are equal at N0.
2. **`invert_if` is equal at N0.** rustc builds `if !c { b } else { a }` as a switch with swapped targets, so no SMT or N4 is needed for a plain negation.
3. **Two N0 rules were added during the spike:**
   - A `move` operand is treated like a copy. A named variable gives a copy and a temporary gives a move, for the same value.
   - The source location in closure type names (`{closure@file:line:col}`) is replaced by a number.
4. **N2 removes extra copies.** Extract variable, inline variable, and `if` to `match` are equal only at N2.
5. **N2 proves small extracts and renames only by chance.** `extract_function` and `rename_item` are equal at N2 because the rustc inliner inlines the small function. `extract_function_large` is not inlined and is not proved. So N1 (content hashes) and N3 (bounded inlining) are still necessary. Do not rely on the rustc inliner as a proof method.
6. **`?` needs the standard library.** `question_mark` still differs at N2: the `Try` trait code builds and reads a `ControlFlow` value. A proof needs inlining of `std` code (N3 does not include it), or N4.
7. **Panic locations are not behavior.** N0 removes spans, so an overflow check at another source location is the same check. `reorder_independent_lets` depends on this rule: both orders panic with the same message, only at another location. The design must state this rule. N4 must still keep the order of panics with different messages.
8. **N4 or SMT is necessary** for `guard_clause` (`!(i < n)` against `i >= n`) and `reorder_independent_lets`.

## Next steps

- Implement N1 (call targets by content hash), and remove the `rename` map from the harness.
- Implement N3 (bounded inlining of the workspace's own functions) for `extract_function_large`.
- Add cases from real rust-analyzer assists, applied to real crates.
- Measure the tightness threshold on real commits (blocker B14).
