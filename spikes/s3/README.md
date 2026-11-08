# Spike S3: compile cost

Question: how long does a semilla check take on a real crate, and where does the time go?

## How to run

```sh
cargo build --release -p semilla -p semilla-mir
spikes/s3/run.sh ~/.cargo/registry/src/<registry>/regex-syntax-0.8.11 /tmp/s3-regex
```

The script copies the crate into a new git repository, and measures four commands with `SEMILLA_TIMING=1`:

1. `semilla index` of the base tree.
2. The first `semilla check` of a step that adds a function.
3. The same check again (all results come from the cache).
4. The check of a second step that changes a literal (a body differs, so N2 and N3 are necessary).

The script removes the dev-dependencies and the optional dependencies, because the offline cache does not have all of them.

## Results: regex-syntax 0.8.11 (58k lines), release binaries

| Command | First version | Promoted fix | Delta builds |
|---|---|---|---|
| 1 index (base) | 16.6 s | 9.5 s | 2.4 s (N0 only) |
| 2 first check of a step | 26.8 s | 10.1 s | **2.3 s** |
| 3 same check, cached | 5.8 s | 1.4 s | 1.1 s |
| 4 check of a second step | 21.9 s | 10.1 s | 8.6 s (see below) |
| Uncovered bodies in check 4 | 20 (false) | 0 | 0 |

- **First version:** every check compiled the crate three times (N0, N2, N3), in a new folder, one after the other.
- **Promoted fix:** the MIR printer reads promoted constants, so generic bodies can be equal again. The compare went from 2.2 s to 0.15 s.
- **Delta builds:** one fixed build folder for each level. Git updates it to each tree by delta, so cargo compiles only the changed crates and their dependents. N0 runs first; N2 and N3 run only when a body is still different, in parallel.

In check 4, the "before" tree had no N2 or N3 build yet, so those were full builds (2.9 s and 3.7 s). The deltas for the "after" tree took 1.4 s and 2.2 s. In a series of steps, the "before" tree of a step is the "after" tree of the step before, so a step that needs all levels costs about 4 s.

## Where the time goes (check 4, delta builds)

| Part | Time |
|---|---|
| `cargo check` N0 (delta) | 1.1 s |
| `cargo check` N2 and N3, in parallel | 5.9 s (2.4 s when the "before" tree is built) |
| Syntax load of the two trees | 0.9 s |
| Body compare (N0 to N4) | 0.2 s |
| The rest (claims, git, process start) | 0.5 s |

## Findings

1. **rustc is the cost, not the proofs.** The compiles were 85% of the time before the delta builds.
2. **Debug binaries are about 6 times slower** for the syntax load and for egglog. The dev profile now optimizes the dependencies.
3. **A new folder per tree prevents all reuse.** The fixed build folders gave the largest gain.
4. **Cargo can change a tree folder** (it writes `Cargo.lock`). The build folders are separate from the cached tree folders, so the syntax view always sees the git tree.
5. **The delta reuse needs a soundness test.** `crates/semilla-normal/tests/delta.rs` checks that fresh crates keep their records, changed and dependent crates get new records, and removed crates lose them.

## Next

- A per-file syntax cache (the syntax load is now a large part of a fast step).
- Phase 2 in the background (design section 19), so that the agent never waits for the compiles.
