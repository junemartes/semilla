# semilla

Semilla makes code review faster. An agent tags its work with **claims**: a list of semantic changes. Semilla **verifies** each claim against the code. A reviewer then reads verified changes in a local UI, with flow graphs and a change tree, instead of a line diff.

The design is in [`docs/design.md`](docs/design.md). The first language is Rust. Semilla is local: it does not push and does not use a remote service.

## Build

```sh
cargo build                 # the workspace uses the pinned nightly in rust-toolchain.toml
scripts/fetch-z3.sh         # optional: installs Z3 in .semilla/tools/ for the SMT checks
```

The binaries are in `target/debug/`:

- `semilla`: the command line, the MCP server, and the review UI.
- `semilla-mir-driver`: the rustc driver that gives the MIR normal forms. `semilla` finds it next to its own binary, or through `SEMILLA_MIR_DRIVER`.

## The agent workflow

The agent keeps its own edit tools. It records each change as a step with claims:

```sh
semilla step begin "return 0 for an empty cart"
# ... edit the code ...
semilla anchor --before crates/shop-core/src/cart.rs:7 # → shop_core::cart::Cart::total/S0
semilla claim add-early-exit --at shop_core::cart::Cart::total/S0 cond="self.items.is_empty()"
semilla check                                           # verifies the open step
semilla step end
```

Rules:

- Claims on existing code use the anchors of the tree **before** the step: get them with `semilla anchor --before FILE:LINE[:COLUMN]`, with the line numbers of the old file. `add-*` statement claims use the anchor of the **new** statement: get them with `semilla anchor FILE:LINE`.
- Free edits exist only at the expression level (`replace-expression`, `replace-pattern`, `replace-type`). Above the expression level, use structured ops (`add-if`, `add-arm`, `wrap-statements`, …). `semilla ops` lists all operations.
- Put neutral refactors in their own steps. A neutral claim needs a body proof of its whole function.
- `semilla suggest-claims` proposes claims for the uncovered changes. The suggestions are not trusted: check them and add them with `semilla claim`.
- `semilla ops <op>` shows the arguments of an op. `semilla step reopen` opens the last step again to fix its claims.
- `semilla check` is fast (phase 1, the syntax checks). The body proofs run in a background worker after `step end` and `tag`; `semilla status` shows their result.

The agent can also make all its changes first, then plan the steps and submit them at once (the replay):

```sh
semilla replay begin                      # the work is the target; the working tree is HEAD again
semilla replay plan                       # a draft plan in .semilla/plan.json: steps, takes, claims
# ... edit the plan: intents, claims, property claims, groups ...
semilla replay apply --full               # start from the base, run all steps, check everything
semilla replay end                        # the working tree is the target again
```

`semilla replay begin --commit <rev>` replays a commit that has no change set (its parent is the base). `semilla replay take` and normal steps also work inside a replay.

Other claims for honest reviews:

- `rewrite-item` claims a whole rewrite on purpose (always in the spotlight). A similar `remove-item` and `add-item` pair gives a pair warning; the agent can refuse it with a reason, and the reviewer can accept it in the UI.
- Property claims state facts about new code: `no-panic`, `no-unsafe`, `no-io`, `calls-only`, `tested-by`.

Then organize the steps and store the result on the commit:

```sh
semilla group G1 --kind refactor --title "Rename the sum" --steps 0
semilla group G2 --kind fix --title "Fix the cart total" --steps 1,2
semilla subgroups G2                      # the engine suggestion; edit it with --set layout.json
semilla verify --final
git commit -am "fix the cart total"
semilla tag                               # a git note on HEAD, and refs under refs/semilla/
```

Agents: read [`docs/agent-guide.md`](docs/agent-guide.md) first.

### MCP

`semilla mcp` serves the same tools over MCP (stdio). For Claude Code:

```sh
claude mcp add semilla -- /path/to/semilla/target/debug/semilla mcp
```

The tools are `ops`, `step_begin`, `claim`, `unclaim`, `step_end`, `anchor`, `check_step`, `suggest_claims`, `verify`, `group_set`, `subgroups_suggest`, `subgroups_set`, and `tag`.

### Feature sets

The body checks use the default features. To check more feature sets, list them in the root manifest:

```toml
[workspace.metadata.semilla]   # or [package.metadata.semilla]
feature-sets = [["serde"], ["std", "unicode"]]
```

A body is equal only if it is equal in every set.

## The review

```sh
semilla status HEAD                    # the state of the background check of a tagged commit
semilla review HEAD                    # the review UI on http://127.0.0.1:7878
semilla review HEAD --md review.md     # a Markdown report
semilla review HEAD --html review.html # a self-contained HTML report (works offline)
semilla load HEAD && semilla show      # the change tree in the terminal
```

The UI shows the change set by group and subgroup, the change tree, flow views (Mermaid), the workspace map (Cytoscape), and search. The review state is in `.semilla/review.json`, and it follows the claims, not the lines.

## What the verifier checks

For each step, the verifier:

1. Applies the claimed renames, and computes the syntax edits down to the expression level.
2. Checks each claim: the shape of its edit, or a body proof with the MIR normal forms (N0 to N4), or an SMT check of a condition.
3. Rejects each edit without a claim (**uncovered**), each claim without an edit (**empty**), and each body whose MIR changed when its text did not (an **indirect change** without `affects`).
4. Compares the declared class of the step with the computed class.
5. Reports the change in explicit panic sites, and puts `unsafe` code, `#[cfg]` attributes, features, and build scripts in the **spotlight**.

`semilla index` writes the index of the working tree to `.semilla/index.json`: the workspace graph, the items, and the MIR content hash of each body.

For the change set, it also checks the group and subgroup rules. The result has a score from 0 to 1.

## Crates

| Crate | Purpose |
|---|---|
| `semilla-core` | The data model: anchors, the rule table of ops, claims, steps, groups, reports, view types |
| `semilla-syntax` | The entity tree from the source, anchors, the syntax diff, flow views, the workspace graph |
| `semilla-mir` | The rustc driver: canonical MIR at each level, N3 inlining, N4 terms |
| `semilla-normal` | Runs the driver on a workspace, and compares bodies (N0 to N4, with `egglog` for N4) |
| `semilla-smt` | Condition effects and expression equivalence with Z3 |
| `semilla-verify` | The verifier (the trusted core) and the views |
| `semilla-ui` | The local review UI |
| `semilla` | The command line and the MCP server |

`spikes/s1` is the normalization spike: before/after cases and negative controls for the normal forms.

## Tests

```sh
cargo test --workspace
```

The end-to-end test (`crates/semilla/tests/e2e.rs`) runs the real binary on a temporary git repository with a two-crate workspace. It covers the agent flow, the storage, the MCP server, and the UI server.

## License

MIT. See [`LICENSE`](LICENSE). The vendored UI libraries in `crates/semilla-ui/assets/vendor/` keep their own licenses (MIT, and EPL-2.0 for elkjs).
