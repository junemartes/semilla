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
semilla anchor crates/shop-core/src/cart.rs:7          # → shop_core::cart::Cart::total/S0
semilla claim add-early-exit --at shop_core::cart::Cart::total/S0 cond="self.items.is_empty()"
semilla check                                           # verifies the open step
semilla step end
```

Rules:

- Claims on existing code use the anchors of the tree **before** the step. `add-*` statement claims use the anchor of the **new** statement.
- Free edits exist only at the expression level (`replace-expression`, `replace-pattern`, `replace-type`). Above the expression level, use structured ops (`add-if`, `add-arm`, `wrap-statements`, …). `semilla ops` lists all operations.
- Put neutral refactors in their own steps. A neutral claim needs a body proof of its whole function.
- `semilla suggest-claims` proposes claims for the uncovered changes. The suggestions are not trusted: check them and add them with `semilla claim`.

Then organize the steps and store the result on the commit:

```sh
semilla group G1 --kind refactor --title "Rename the sum" --steps 0
semilla group G2 --kind fix --title "Fix the cart total" --steps 1,2
semilla subgroups G2                      # the engine suggestion; edit it with --set layout.json
semilla verify --final
git commit -am "fix the cart total"
semilla tag                               # a git note on HEAD, and refs under refs/semilla/
```

### MCP

`semilla mcp` serves the same tools over MCP (stdio). For Claude Code:

```sh
claude mcp add semilla -- /path/to/semilla/target/debug/semilla mcp
```

The tools are `ops`, `step_begin`, `claim`, `unclaim`, `step_end`, `anchor`, `check_step`, `suggest_claims`, `verify`, `group_set`, `subgroups_suggest`, `subgroups_set`, and `tag`.

## The review

```sh
semilla load HEAD        # load the claims of a tagged commit
semilla verify --final   # verify them again
semilla show             # the change tree in the terminal
semilla ui               # the review UI on http://127.0.0.1:7878
```

The UI shows the change set by group and subgroup, the change tree, flow views (Mermaid), the workspace map (Cytoscape), and search. The review state is in `.semilla/review.json`, and it follows the claims, not the lines.

## What the verifier checks

For each step, the verifier:

1. Applies the claimed renames, and computes the syntax edits down to the expression level.
2. Checks each claim: the shape of its edit, or a body proof with the MIR normal forms (N0 to N4), or an SMT check of a condition.
3. Rejects each edit without a claim (**uncovered**), each claim without an edit (**empty**), and each body whose MIR changed when its text did not (an **indirect change** without `affects`).
4. Compares the declared class of the step with the computed class.

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
