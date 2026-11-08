# Spike S1: MIR normalization

Question: does the normal form ladder (N0 to N4) make neutral refactors equal, without making real changes equal?

## How to run

```sh
cargo build -p semilla-mir
cargo run -p semilla-s1
```

The canonical MIR (`.mir`) and the N4 term (`.term`) of each body go to `target/s1/<case>/<level>/<side>/`, for a manual diff.

Each case in `cases/` has a `before.rs`, an `after.rs`, and a `case.toml`:

- A **neutral** case passes ("proved") if all bodies are equal at some level.
- A **changed** case is a negative control. It passes ("ok") only if a body stays different at all levels. A failed control means that a normalization or rewrite rule is unsound.
- `rename` (full def path to full def path) and `added` (full def paths) are the claims of the case. The harness gives them to `semilla_normal::compare` as they are. The harness has no rename logic of its own.

## Levels

The driver (`crates/semilla-mir`) makes three record levels. `semilla_normal::compare` tries five ladder levels in order and stops at the first equal level.

| Level | Input | What makes two bodies equal |
|---|---|---|
| `n0` | driver `n0`: `-Zmir-opt-level=0 -Copt-level=0` | The same canonical text (the rules in `crates/semilla-mir/src/normalize.rs`). |
| `n1` | driver `n0` | The same text after the claimed rename map, or after each call of a local function is replaced by the content hash of the callee (a Merkle tree over the call graph, one hash for each strongly connected component). |
| `n2` | driver `n2`: `-Zmir-opt-level=2 -Copt-level=2`, overflow checks and debug assertions on | The same text (with the rename map and the content hashes). |
| `n3` | driver `n3`: `n2`, then bounded inlining of the crate's own functions (`inline.rs`). The driver writes an `n3` record only for a body with an inlined call; for the other bodies, `compare` uses the `n2` record of the same target. | The same text (with the rename map and the content hashes). |
| `n4` | the term of a loop-free `n2` or `n3` body (`term.rs`) | egglog proves the terms equal with the audited rules in `crates/semilla-normal/src/n4.rs`. |

A table cell is `equal` if all bodies of the case are equal at this level or a lower one.

## Results (nightly-2026-10-25)

| case | expect | n0 | n1 | n2 | n3 | n4 | design level | proved at | result | note |
|---|---|---|---|---|---|---|---|---|---|---|
| closure_param_rename | neutral | equal | equal | equal | equal | equal | N0 | N0 | proved | Rename a closure parameter. |
| ctrl_bounds_order | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N4): swap two bounds checks. The panic message prints the index, so the message can change. |
| ctrl_change_literal | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control: `+ 1` becomes `+ 2`. |
| ctrl_change_operator | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control: `<` becomes `<=`. |
| ctrl_closure_body | changed | diff: f::{closure#0} | diff: f::{closure#0} | diff: f::{closure#0} | diff: f::{closure#0} | diff: f::{closure#0} | - | - | ok | Control: change the body of a closure (`x + 1` becomes `x + 2`). |
| ctrl_copy_then_write | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N0 copy forwarding): the copy moves after a write of its source. |
| ctrl_drop_order | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control: swap two lets with Drop types. The drop order changes. |
| ctrl_drop_order_branch | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N4): swap two lets with Drop types inside a branch. The drop order changes. |
| ctrl_extract_borrowed_arg | changed | diff: parts | diff: parts | diff: parts | diff: parts | diff: parts | - | - | ok | Control (N3 argument substitution): the caller changes `patch` before the extracted test, not after it. |
| ctrl_extract_mut_param_wrong | changed | diff: fix_up | diff: fix_up | diff: fix_up | diff: fix_up | diff: fix_up | - | - | ok | Control (N3 and rule 3d): the extracted fn with a `&mut Error` parameter writes the error without the condition. |
| ctrl_extract_wrong_and | changed | diff: pre | diff: pre | diff: pre | diff: pre | diff: pre | - | - | ok | Control (N4 constant-switch): the extracted bool fn uses `||` where the original uses `&&`. |
| ctrl_extract_wrong_constant | changed | diff: rank | diff: rank | diff: rank | diff: rank | diff: rank | - | - | ok | Control (N3): extract a block into a new function, but change a constant in it. |
| ctrl_fact_after_write | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N4 path facts on a switch): the second test reads `*p` after a write, so the first test says nothing about it. |
| ctrl_float_negation | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N4): `!(a < b)` becomes `a >= b` on floats. The result changes for NaN. |
| ctrl_float_reflexive | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N4): `x == x` becomes `true` on a float. The result changes for NaN. |
| ctrl_guard_off_by_one | changed | diff: get | diff: get | diff: get | diff: get | diff: get | - | - | ok | Control (N4 path facts): the guard becomes `i > len`. For `i == len` the result changes (a panic). |
| ctrl_inline_mut_arg | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N3): the callee changes its own argument. A wrong inliner would change `a` of the caller. |
| ctrl_ite_constant | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N4): change a constant in one branch of an if. |
| ctrl_memory_slots | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N4 slot numbers): two locals in memory; the result reads the other one. |
| ctrl_promoted_value | changed | diff: pair | diff: pair | diff: pair | diff: pair | diff: pair | - | - | ok | Control (promoted constant): change a value in a promoted constant of a generic body (`&[1, 2, 3]` becomes `&[1, 2, 4]`). |
| ctrl_ref_two_places | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N0 rule 3d): a reference points to one of two places, so a write through it is not a write of `a`. |
| ctrl_rename_changed_body | changed | diff: add_one, f | diff: add_one | diff: add_one | diff: add_one | diff: add_one | - | - | ok | Control (N1): a rename claim, but the body of the renamed function changes. |
| ctrl_reorder_asserts | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N4): swap two lets that can panic with different messages (add and multiply overflow). |
| ctrl_reorder_calls | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N4): swap two calls with side effects. The output order changes. |
| ctrl_reorder_raw_writes | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control (N4): swap two writes through raw pointers. The pointers can alias. |
| ctrl_swap_branches | changed | diff: pick | diff: pick | diff: pick | diff: pick | diff: pick | - | - | ok | Control: swap the branches without a negated condition. |
| ctrl_wrapping_add | changed | diff: f | diff: f | diff: f | diff: f | diff: f | - | - | ok | Control: `x + 1` (panics on overflow) becomes `x.wrapping_add(1)`. |
| extract_function | neutral | diff: total | diff: total | equal | equal | equal | N3 | N2 | proved | Extract the loop body expression into a new function. |
| extract_function_large | neutral | diff: rank | diff: rank | diff: rank | equal | equal | N3 | N3 | proved | Extract a larger block into a new function. Tests the limit of the rustc inliner at N2. |
| extract_function_mut_arg | neutral | diff: f | diff: f | diff: f | equal | equal | N3 | N3 | proved | Extract a function (`#[inline(never)]`) that changes its own argument (a copy). The caller value stays. |
| extract_variable | neutral | equal | equal | equal | equal | equal | N0 or N2 | N0 | proved | Extract `a * b` into a variable. |
| format_comments | neutral | equal | equal | equal | equal | equal | N0 | N0 | proved | Whitespace and comments only. |
| guard_clause | neutral | diff: get | diff: get | diff: get | diff: get | equal | N4 or SMT | N4 (path-fact, ite-not, int-not-lt) | proved | Replace if/else with an early return (`!(i < n)` becomes `i >= n`). |
| if_to_match | neutral | equal | equal | equal | equal | equal | N0 or N2 | N0 | proved | Convert `if c` to `match c { true, false }`. |
| inline_variable | neutral | equal | equal | equal | equal | equal | N0 or N2 | N0 | proved | Inline a variable into its only use. |
| invert_if | neutral | equal | equal | equal | equal | equal | N4 or SMT | N0 | proved | Invert an if: negate the condition and swap the branches. |
| lifetime_elision | neutral | equal | equal | equal | equal | equal | N0 | N0 | proved | Remove explicit lifetimes (elision). |
| promoted_generic | neutral | equal | equal | equal | equal | equal | N0 | N0 | proved | Add a new function. An unchanged generic body with a promoted constant (`&[1, 2, 3]`) stays equal. |
| question_mark | neutral | diff: f | diff: f | diff: f | diff: f | equal | N2 | N4 (aggregate-field, assume-true, reflexive-compare, single-variant-discriminant) | proved | Replace a manual match on Option with `?`. |
| rename_item | neutral | diff: f | equal | equal | equal | equal | N1 | N1 (rename-map) | proved | Rename a function and its call. The claim gives the rename map. |
| rename_local | neutral | equal | equal | equal | equal | equal | N0 | N0 | proved | Rename a local variable. |
| reorder_independent_lets | neutral | diff: f | diff: f | diff: f | diff: f | equal | N4 | N4 (checked-mul-commute, assert-commute) | proved | Swap two independent lets. Both can panic on overflow, with the same message (only the location differs). |
| semver_extract_and | neutral | diff: pre | diff: pre | diff: pre | diff: pre | equal | N3 | N4 (constant-switch) | proved | semver `comparator`: extract `patch.is_some() && text.starts_with('-')` into a fn that returns bool. |
| semver_extract_and_twice | neutral | diff: parts | diff: parts | diff: parts | diff: parts | equal | N3 or N4 | N4 (constant-switch, path-fact) | proved | semver `comparator`, closer to the real code: the caller borrows `patch` again after the extracted `&&` test. |
| semver_extract_error | neutral | diff: check | diff: check | equal | equal | equal | N3 | N2 | proved | semver `comparator`: extract `Error::new(ErrorKind::EmptySegment(pos))` into `fn empty_segment(pos) -> Error`. |
| semver_extract_mut_param | neutral | diff: fix_up | diff: fix_up | diff: fix_up | equal | equal | N3 | N3 | proved | semver `version_req`: extract a whole `if let` statement into a fn with a `&mut Error` parameter. |
| semver_extract_or | neutral | diff: fix_up | diff: fix_up | diff: fix_up | diff: fix_up | equal | N3 | N4 (aggregate-field, constant-switch) | proved | semver `version_req`: extract `rest.is_empty() || rest.starts_with(',')` into a fn that returns bool. |
| type_annotation | neutral | equal | equal | equal | equal | equal | N0 | N0 | proved | Add a type annotation to a let. |
| unneeded_mut | neutral | equal | equal | equal | equal | equal | N0 | N0 | proved | Remove an unneeded `mut`. |

Summary: 23 of 23 neutral cases are proved. All 26 negative controls stay different at every level.

## Findings

1. **N0 is strong enough for the simple refactors.** Formatting, comments, local renames, closure parameter renames, type annotations, lifetime elision, and `mut` removal are equal at N0.
2. **`invert_if` is equal at N0.** rustc builds `if !c { b } else { a }` as a switch with swapped targets, so no SMT or N4 is needed for a plain negation.
3. **Two N0 rules from the first round:** a `move` operand is treated like a copy, and the source location in closure type names is replaced by a number.
4. **Two new N0 rules make extract variable, inline variable, and `if` to `match` equal at N0** (before, only at N2): merge a block with its only `goto` successor (rule 3b), and forward a copy to its only use in the same block (rule 3c). The N3 inliner needs both rules: it adds a `goto` into the callee entry, and the inlined code copies values in other places than the original code. `ctrl_copy_then_write` checks that a copy does not move past a write of its source.
5. **The old printer was not exact enough for a proof.** `Body::dump` prints the variant name without the type path (`Some(_5)`), it does not print the cast kind (`x as u32` is the same text for `IntToInt` and `Transmute`), and it prints a constant in memory as an allocation number only. Two different bodies could get the same text. The driver now has its own printer (`print.rs`): full def paths with the crate name first, the cast kind, and the bytes and pointers of each constant. A promoted constant of a generic body (for example `&[1, 2, 3]` in `fn pair<T>`) is not evaluated, so the printer prints the promoted MIR in place, with the same N0 rules (`promoted_generic`, `ctrl_promoted_value`). The first version printed a unique word there: sound, but every generic body with a promoted constant was always different (spike S3 found 19 such bodies in `regex-syntax`). A unique word stays only where nothing else is possible (see the module comment of `print.rs`).
6. **N1 proves `rename_item` with the real rename map.** The claim maps `s1case::helper` to `s1case::add_one`. The map applies to the names and to each token in the text (`foo` does not match `foo_bar`, and a key also renames its child paths, such as `Cart::total` or `f::{closure#0}`). Without the claim, the content hash still makes the caller equal, and `suggest_renames` finds the rename as a hint. `ctrl_rename_changed_body` checks that a rename claim does not hide a changed body. A rename whose target already exists on the "before" side is not applied, because it would make two different items look the same.
7. **N2 still proves small extracts by chance.** `extract_function` is equal at N2, because the rustc inliner inlines the small function. `extract_function_mut_arg` has `#[inline(never)]` to force the N3 path. The proof is still sound (the rustc optimizations are trusted at N2), but it depends on rustc heuristics.
8. **N3 proves `extract_function_large`.** The rustc inliner does not inline `score`. The semilla inliner inlines it (local, no generic parameters, Rust ABI, no `#[track_caller]`, not recursive, at most depth 2 and 64 blocks). After rules 3b and 3c, the two `rank` bodies have the same text. `extract_function_mut_arg` (`#[inline(never)]`, a callee that writes its own argument) shows that the inliner copies such an argument into a new local. `ctrl_inline_mut_arg` and `ctrl_extract_wrong_constant` stay different.
9. **N4 proves `guard_clause`.** The term builder removes the bounds check in the "after" body: on the path where `i >= len` is false, `i < len` is true, so the assert cannot fail (`path-fact`). Then egglog connects `ite(i >= len, None, Some)` with `ite(i < len, Some, None)` by `int-not-lt` and `ite-not`. `ctrl_guard_off_by_one` (`i > len`) stays different.
10. **N4 proves `reorder_independent_lets`, but it needs one more rule than expected.** The two overflow asserts commute (`assert-commute`: the same message key, no cleanup, and a panic location is not behavior). But rustc GVN also changes the "after" multiply to `CheckedMul(b, a)`. So N4 needs `checked-mul-commute` (a checked op exists only for integers). `ctrl_reorder_asserts` (add and multiply overflow: different messages) and `ctrl_bounds_order` (the bounds check message prints the index) stay different.
11. **N4 proves `question_mark`.** At N2, rustc inlines the `Try` code. Three differences stay: a `ControlFlow` value that the body builds and reads at once (`aggregate-field`), the discriminant of an uninitialized `Option<!>` (one variant in the layout, so the discriminant is a constant: `single-variant-discriminant`), and `assume(d == 0)` on that constant (`reflexive-compare`, `assume-true`). This proof depends on the rustc inliner at N2 (finding 7). A body where rustc does not inline `Try::branch` is not proved.
12. **The panic message key decides which asserts commute.** The overflow and division messages are constant strings in `core` (`panic_const_*`), so their key has no operands. The bounds check and the misaligned pointer messages print their operands, so their key has the operand values.
13. **Side effects keep their order in N4.** Calls, drops, asserts, and writes through pointers take and return the state term. No rule moves an effect. `ctrl_reorder_calls`, `ctrl_drop_order_branch`, and `ctrl_reorder_raw_writes` stay different.
14. **The integer rules must stay integer-only.** `ctrl_float_negation` (`!(a < b)` against `a >= b`) and `ctrl_float_reflexive` (`x == x` against `true`) stay different, because the term builder makes `ICmp` only for integer operands.
15. **egglog gives the proof chain.** With `EGraph::new_with_proofs`, the `prove` command gives a proof tree with the rule names. Each N4 proof lists the term build rules first, then the egglog rules. A run stops at 30 iterations, 100 000 tuples, or 3 seconds, and a stop gives "not proved".

16. **Cost (spike S3, `regex-syntax`, release build).** `compare` takes 0.12 to 0.17 s for about 2100 bodies (2.2 s before). Most of the old time was the egglog proof mode on bodies that stay different; N4 now runs egglog in the plain mode first, and the proof mode only after a success. The driver writes N3 records only for bodies with an inlined call, inlines in place (the old inliner copied the whole body for each call), and prints the text of a large constant table once, as its SHA-256 hash.

17. **Extract function on real code (semver 1.0.28, from an agent trial).** Four correct extractions were "unproved". The `semver_*` cases reduce them. All four prove now, also on the real `parse.rs` (with `run_cargo_check` and `compare`):

    | extraction | level now | why it failed | fix |
    |---|---|---|---|
    | `rest.is_empty() \|\| rest.starts_with(',')` into a bool fn (`version_req`) | N4 | The inlined fn returns a bool through a join, and the caller switches on it again (`_t = false; goto J; J: switchInt(_t)`). The original code jumps directly. | N4 build rule `constant-switch` (a switch on a constant follows its target). Memory locals get canonical slot numbers (the MIR local numbers differ). |
    | `patch.is_some() && text.starts_with('-')` into a bool fn (`comparator`) | N4 | As above, and more: (a) N3 inlines all local callees, `numeric_identifier` has a loop, so the N3 body has no term; (b) the term of the large body has too many paths; (c) the inlined callee calls `is_some(&copy)` on a copy of its argument, the original calls `is_some(&patch)`; (d) rustc jump threading in the original skips the second `patch.is_some()` test when the first is false. | (a, b) The N3 term comes from a body with only the loop-free callees inlined, then only the small (at most 16 blocks), then only the tiny (at most 6 blocks) ones. (c) The inliner lets the callee read the caller local when both only have shared borrows of a `Freeze` type. (d) Path facts also decide a bool switch. |
    | `Error::new(ErrorKind::EmptySegment(pos))` into `fn empty_segment` | N3 (N2 in the small case) | Not reproduced with the current code: the real case proves at N3. The trial failure came from older problems (promoted constants, or closures of the extracted fn). | - |
    | a whole `if let` into `fn mark_lone_wildcard(input, error: &mut Error)` | N4 (N3 in the small case) | The inlined code writes `(*r).kind` through `r = &mut error`; the original writes `error.kind`. Also, rule 3b did not merge a block, because an unreachable empty `goto` block counted as a predecessor. | N0 rule 3d (forward a reference with one fixed target). Rule 3b counts reachable predecessors only. |

    Controls for the new rules: `ctrl_extract_wrong_and` (`||` instead of `&&`), `ctrl_memory_slots` (another local in memory), `ctrl_ref_two_places` (a reference to one of two places), `ctrl_extract_mut_param_wrong` (the `&mut` helper writes without the condition), `ctrl_extract_borrowed_arg` (the caller changes the argument before the call), `ctrl_fact_after_write` (a second test after a write through the pointer).

## Limits

- N4 only handles loop-free bodies (no `theta` nodes yet). The term is a tree of paths (no merge at join points), so a body with many sequential branches stops at 4000 block visits and gets no term. An inlined callee with several returns multiplies the paths after the call. So an extract-function from a large caller proves at N4 only if the extracted fn is small enough (the third try inlines callees of at most 6 blocks), or if N3 makes the text equal. For a larger extracted fn in a large caller, the exact reason is: "the N4 term has too many paths". A fix needs gated values at join points (an `ite` of the values, as in gated SSA) and N4 rules that move `ite` through effects.
- The N3 text and the N3 term can come from different inlined bodies (all callees for the text, only loop-free callees for the term). Each is a normal form of the same behavior, so each compare is sound on its own.
- N4 does not model inline assembly. A body with inline assembly gets no term.
- A local whose address the body takes lives in memory in the N4 term. N4 never reorders its reads and writes.
- N3 does not inline trait methods, closures, generic functions, or functions of other crates (for example `std`).
- The content hash only follows the callees of the same crate. A call into another workspace crate keeps its path in the hash input.

## Next steps

- Add `theta` nodes (loops) to N4.
- Add cases from real rust-analyzer assists, applied to real crates.
- Measure the tightness threshold on real commits (blocker B14).
