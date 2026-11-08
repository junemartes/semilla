# Spike S2: Charon coverage

Question (design section 16.1): is Charon LLBC a good source for the region tree of the flow views (section 10.1, blockers B4 and B6)? What percentage of functions does Charon translate? Does it translate closures and `async`?

Short answer: Charon translates all synchronous functions and closures of the three crates without errors. It does not translate `async` bodies. The LLBC structure is correct, but it is not the source structure. **Recommendation: keep the source-level regions for the region tree. Do not add Charon now.** See [Recommendation](#recommendation).

## How to run

```sh
spikes/s2/run.sh <cache-dir> <crate-dir> [<out-dir>]
```

- The script clones Charon at a pinned commit into `<cache-dir>`, and builds it once.
- It runs rustc (`-Zunpretty=mir`) and four Charon variants on `<crate-dir>`.
- It writes the outputs, logs, times, and measurements to `<out-dir>` (default: `<cache-dir>/out/<crate>`).
- Use a copy of the crate. Cargo and build scripts run in it.

The tools in `tools/`:

| Tool | Input | Output |
|---|---|---|
| `count_mir.py` | the rustc MIR dump | the number of MIR bodies by kind (functions, closures, coroutines, consts) |
| `analyze.py` | Charon JSON (plain, `--no-dedup-serialized-ast`) | declarations by kind, body kinds, errors, closures, statement census, code duplication, span checks |
| `skeleton.py` | Charon JSON (plain) and a function name | the control skeleton of the function (loop, switch, break, continue, return, panic, calls) with source lines |

## Setup

| Item | Value |
|---|---|
| Charon | tag `nightly-2026.10.15`, commit `1c4aedb1dfaeb1436deffb03513fcccf2b0abecd`, version `0.1.289` (24 days old on 2026-11-08) |
| Charon toolchain | `nightly-2026-09-17` (from `charon/rust-toolchain`), components `rustc-dev`, `llvm-tools`, `rust-src`, `miri`, and 8 targets |
| semilla toolchain | `nightly-2026-10-25` (not the same nightly) |
| Machine | Linux x86_64, 12 cores, 62 GB RAM |

The build needed two changes to the README steps:

1. `git submodule update --init` is necessary. The build needs `crates/minirust`. The README does not say this.
2. `make build-charon-rust` fails. It runs `cargo fmt` first, and the pinned toolchain has no `rustfmt`. The script runs `cargo build --release` in `charon/` directly. This is the build step of the Makefile.

### Variants

| Variant | Flags |
|---|---|
| `llbc` | the defaults |
| `ullbc` | `--ullbc` |
| `review` | `--reconstruct-matches --reconstruct-asserts --reconstruct-panic-calls --reconstruct-fallible-operations --hide-marker-traits --no-dedup-serialized-ast` |
| `nodup` | `review` and `--no-code-duplication` |

The `review` flags make the output closer to the source: `match` on enum variants, `assert`, `panic`, and `panic.+` operators. All structure numbers below come from `review`, unless the text says otherwise.

## Crates

| Crate | Size | Why |
|---|---|---|
| `strsim-0.11.1` | 1307 lines, no dependencies | small; loops, `match`, closures |
| `itertools-0.14.0` | 14505 lines, 1 dependency | medium; generic iterator adapters, 263 closures, `?` |
| `async-lock-3.4.2` | 4918 lines, 3 dependencies | `async fn` in library code (not only in tests); `pin-project-lite` macros |

The line counts include comments and `#[cfg(test)]` code. The MIR counts are for the `lib` target with the default features.

## Results

### Coverage

| | strsim | itertools | async-lock |
|---|---|---|---|
| rustc MIR bodies: functions | 37 | 1010 | 364 |
| rustc MIR bodies: closures | 8 | 263 | 12 |
| rustc MIR bodies: coroutines (`async fn` / `async` block) | 0 | 0 | 7 (5 / 2) |
| rustc MIR bodies: `const` / `static` | 2 | 33 | 43 |
| Charon local function declarations | 51 | 1591 | 455 |
| ... functions (normal, trait impl, trait default) | 36 | 997 | 358 |
| ... closure methods (`call`, `call_mut`, `call_once`) | 13 | 433 | 9 |
| ... generated drop glue | 2 | 137 | 54 |
| ... other (global initializers, constructors) | 0 | 24 | 34 |
| Declarations with a body and no error | 51 (100%) | 1591 (100%) | 450 (98.9%) |
| Declarations with an error | 0 | 0 | 5 bodies + 1 flag |
| Closures translated (matched by source line) | 7 of 7 lines (8 closures) | 204 of 204 (270 closure types for 263 rustc bodies) | 9 of 11 |
| `async` bodies translated | - | - | 0 of 7 |
| Charon errors / warnings | 0 / 0 | 0 / 0 | 0 / 18–23 |

Notes:

- The function counts differ by 1–2%. In strsim, the difference is the tuple-struct constructor `StringWrapper`. rustc prints a body for it. Charon emits a constructor only when the code uses it. I did not match each body by name in the other two crates.
- `--translate-all-methods` changes nothing in the local counts. All 130 provided methods of the `Itertools` trait are in the default output.
- Charon puts a closure body in the `call_mut` or `call_once` method of a generated trait impl. The FnOnce impl of an FnMut closure is an extra shim.
- **Errors by kind (async-lock):**
  - 5 bodies are `Error("Coroutines are not supported")`. These are the 5 `async fn`: `OnceCell::wait`, `get_or_try_init`, `get_or_init`, `set`, and `initialize_or_wait`.
  - The 2 `async` blocks and the 2 closures inside these functions are missing.
  - `get_or_try_init_blocking` is a synchronous function. It has a body, but it has the `has_errors` flag, because it uses the coroutine type of `initialize_or_wait`.
  - The warnings are "Coroutine types are not supported yet" (10–15), "Coroutines are not supported" (5), and "failed to translate drop glue" for a coroutine type (2).

### Run time and output size

Each run starts from a clean target folder, so the time includes the compile of the crate.

| | strsim | itertools | async-lock |
|---|---|---|---|
| `llbc` time (wall) | 0.75 s | 10.7 s | 1.8 s |
| `ullbc` time | 0.74 s | 9.3 s | 1.75 s |
| `review` time | 0.50 s | 4.8 s | 1.2 s |
| Peak memory (`llbc`) | 209 MB | 801 MB | 248 MB |
| `llbc` size (deduplicated JSON) | 5.2 MB | 103 MB | 10.3 MB |
| `ullbc` size | 4.9 MB | 81 MB | 10.1 MB |
| `review` size (plain JSON) | 7.0 MB | 201 MB | 13.2 MB |
| default flags, plain JSON | 23.5 MB | 535 MB | 42.8 MB |
| Python parse and analysis of `review` | 1.1 s | 28 s | 1.4 s |

The output also contains the signatures of all foreign items that the crate uses. This is a large part of the size.

### Structure checks

I compared the LLBC skeleton (`tools/skeleton.py`) with the source for these functions:

| Function | Source | LLBC (`review`) | Match? |
|---|---|---|---|
| `strsim::generic_hamming` | `loop { match (a, b) { 3 arms } }` | `loop`, then 2 nested `match` (one for each tuple field). The `_` arm body occurs 2 times (3 times with the default flags). An extra `continue` at the end of the loop. | partly |
| `strsim::osa_distance` | 2 nested `for`, `if a && b && c && d && e` | Each `for` is `loop { next(); match { None => break } ... continue }`. The nesting is correct. The one `if` is 5 nested switches. Index operations add `assert` (bounds checks). | partly |
| `strsim::normalized_levenshtein` | `if a && b { return 1.0 }` | 2 nested switches, `return`. | partly |
| `itertools::FormatWith::fmt` | `match` with a `panic!` arm, `if let`, 2 `?`, 3 closures | Each `?` is `Try::branch`, a switch, `from_residual`, `return`. The `panic!` arm moves out of the `match`, after it. The closures are separate bodies with correct spans. A `?` in a closure returns from the closure (correct). | partly |
| `async_lock::Semaphore::try_acquire` | `loop { if c { return None } match .. { Ok => return Some(..), Err(c) => .. } }` | `loop`, `if`, `match` are correct. `return None` becomes `break` to a shared `return` after the loop. | mostly |
| `async_lock::OnceCell::wait` | `async fn` | `Error` body | no |

Census of the control statements (`review`):

| | strsim | itertools | async-lock |
|---|---|---|---|
| `Loop` / `Switch` | 24 / 82 | 123 / 1697 | 16 / 128 |
| `Break` / `Continue` / `Return` | 27 / 26 / 62 | 144 / 129 / 1793 | 31 / 25 / 505 |
| `Panic` / `Assert` | 0 / 1 | 14 / 16 | 4 / 2 |
| Calls / calls with an unwind block | 311 / 311 | 5316 / 5316 | 880 / 880 |
| `Drop` / unwind statements | 1242 / 1554 | 70317 / 75663 | 979 / 1865 |

Code duplication (`Assign` and `Call` statements with the same span and the same content, more than once in a body):

| | strsim | itertools | async-lock |
|---|---|---|---|
| default reconstruction | 110 of 1419 (7.8%), 3 bodies | 5940 of 26321 (22.6%), 138 bodies | 106 of 3160 (3.4%), 3 bodies |
| `--no-code-duplication` | 1 of 1286 | 429 of 20160 (2.1%) | 4 of 3021 |
| `Loop` / `Break` with `--no-code-duplication` | 26 / 34 | 208 / 323 | 27 / 55 |

`--no-code-duplication` removes the copies. But it adds `loop { ... break }` blocks for forward jumps. The source does not have these loops.

### Spans

| | strsim | itertools | async-lock |
|---|---|---|---|
| Statements in a crate file, inside the line range of the function | 97.5% | 97.1% | 64.3% |
| Statements in other files (std, registry) | 1.2% | 1.0% | 29.8% |
| `Loop` / `Switch` with a correct start line | 66 of 106 (62%) | 1774 of 1820 (97.5%) | 110 of 144 (76%) |

- The spans of plain statements and calls map to the correct source lines.
- Code from macros has spans in the macro definition. Examples: `panic!` maps to `std/src/panic.rs:221`, and `pin_project!` maps to the `pin-project-lite` source in the registry. `generated_from_span` is empty, so the call site is lost. This is the cause of the 29.8% in async-lock.
- Many `Loop` and `Switch` spans start at line 1, column 0, but their end line is correct. In ULLBC, all `Switch` terminators have correct spans. Some `Goto` terminators have a dummy span, and the LLBC reconstruction merges that dummy span into the region span. A consumer can repair this: use the smallest start of the child statements and ignore the dummy spans.

## Comparison with the source-level flow views

The flow views today come from `crates/semilla-syntax/src/flow.rs`. They use the statement tree of the source.

### What Charon gives that the source regions do not

1. **Exact `?` edges.** Charon shows each `?` as `Try::branch`, a switch, `from_residual`, and `return`. `flow.rs` adds an `err` edge if the statement text contains `?`. This is wrong for a `?` inside a closure: that `?` returns from the closure, not from the function. It is also wrong for a `?` inside a string literal.
2. **Panic sites.** Charon shows `panic!`, `unreachable!`, bounds checks (`assert`), and overflow checks (`panic.+`). The source regions have no `panic` edges. Design section 10.1 lists `panic` as an edge kind.
3. **Unwind and drops.** Each call has an unwind block, and each drop is a statement. This gives the drop order. The source regions have no drops.
4. **Call targets.** Each call refers to a resolved function or trait method. This gives exact `call` and `dispatch` edges for level 2 (the call graph). The source has only the text of the call.
5. **Closures and macros as code.** Closures are separate bodies with correct spans. Macro bodies (`vec!`, `format!`, `panic!`) are expanded.
6. **Desugared `for` loops.** The `into_iter` and `next` calls and the exit on `None` are explicit.

### What Charon loses

1. **`async`.** No `async fn` and no `async` block is translated (B6). These functions need the source regions in all cases.
2. **The loop kind.** `for`, `while`, and `loop` are all `loop`. `if let`, `while let`, and `let else` are switches. This is by design (Charon `docs/limitations.md`).
3. **Match arms.** A tuple pattern becomes one switch for each field. Arm labels are variant names, not source patterns. A `_` arm body is copied into each branch that needs it, unless `--no-code-duplication` is on.
4. **Conditions.** One `if a && b && c` becomes one switch for each operand.
5. **Exits.** A `return` can become a `break` to a shared exit.
6. **Statement boundaries and anchors.** LLBC has no source statements and no role paths (`S1/then/S0`). The claims and the review UI use the role paths. A mapping from LLBC spans to role paths is necessary for each node.
7. **Region spans and macro spans.** See [Spans](#spans).
8. **Code that does not compile.** Charon needs a crate that compiles and passes the borrow check. The source regions need only a parse.
9. **Noise.** 68–86% of the statements are storage markers, drops, and unwind code (strsim 76%, itertools 86%, async-lock 68%). A flow view must remove them.

## Cost

| Item | Cost |
|---|---|
| Extra pinned nightly | `nightly-2026-09-17`. It is not the semilla nightly (`nightly-2026-10-25`). The toolchain uses 2.5 GB on disk with the 8 targets of Charon's `rust-toolchain` file (the host target alone is 1.0 GB). |
| Build time | 90 s wall time, 623 s CPU time, 3.1 GB peak memory, on 12 cores. The `target` folder is 430 MB. |
| Binary size | `charon` 17.8 MB and `charon-driver` 26.3 MB. `charon-driver` loads `librustc_driver` (159 MB) from the pinned toolchain at run time. |
| First run | The full-MIR sysroot: 17 s and 112 MB, once for each toolchain. |
| Run time | 0.5–11 s for each crate and snapshot. This is a second compile of the crate, in addition to the semilla MIR driver. |
| Output | 5–200 MB of JSON for each crate and snapshot. |
| Breakage risk | High. The AST version changed from 0.1.289 to 0.1.315 in 24 days. Charon increments this number for each breaking change of the AST (`docs/versioning.md`). The repository had 277 commits in the same 24 days. The pinned nightly changes every 1–3 months. The README calls Charon beta software. |

## Recommendation

**Keep the source-level regions for the region tree and the flow views. Do not add Charon now.**

Reasons:

1. The coverage is very good for synchronous code (100% in all three crates), so coverage is not the problem.
2. The structure is correct, but it is not the source structure. Reviewers and claims use the source structure. A Charon region tree needs a span-to-anchor mapping, a repair of the region spans, the removal of the unwind code, and a rebuild of the match arms. The result is then close to the source tree that we already have.
3. `async` gives no output at all. So B6 stays, and the source regions are necessary in all cases.
4. The cost is high: a second pinned nightly, a second compile for each snapshot, large output, and a breaking AST change about every day.

What to do instead:

1. Keep `flow.rs` as the region tree.
2. Add the MIR facts that the source does not have as annotations on the source statements: `err` edges at the exact `?` sites, `panic` sites, drops, and resolved call targets. Get these facts from the existing `semilla-mir` driver. It already runs on the semilla nightly, and it already reads the MIR spans. Map each fact to a source statement by its span. This replaces the B4 fallback ("regions from raw MIR and dominator trees"): MIR gives only the edge labels, not the regions.
3. Fix the `?` detection in `flow.rs` first. A `?` inside a closure must not get an `err` edge to the function exit.

Do the spike again if one of these changes:

- Charon supports coroutines.
- Charon reaches version 1.0, with a stable AST.
- Charon and semilla can use the same nightly.

If a "MIR view" for experts is necessary before that, use Charon as a separate, optional tool with the `review` flags and `--no-code-duplication`. Do not put it in the verifier or in the index.
