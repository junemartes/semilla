# Semilla design (draft 2)

Status: draft, with an MVP implementation (section 18). Date: 2026-11-08.

## 1. Goal and scope

Semilla makes code review faster. It replaces the line diff with verified semantic changes.

1. An agent writes the code.
2. The agent tags its work with **claims**: a list of semantic changes, organized in steps and groups.
3. Semilla **verifies** the claims. It does not guess the changes.
4. A reviewer reads the verified changes in a local UI, with flow graphs and a change tree.

Scope:

- The first language is Rust. Strong types and MIR make the verification possible.
- The scope is a full Cargo workspace, with all its member crates.
- Semilla is local. It does not push to GitHub and does not use a remote service.

## 2. Principles

1. **The agent declares, semilla verifies.** The agent makes claims. Semilla checks them. A mismatch fails the check.
2. **Unclaimed means unchanged.** At every level of the tree, a node without a claim must be unchanged.
3. **The residual comes from the verifier.** The agent cannot hide a change. It can only declare a change badly.
4. **Proof over trust.** A proof (MIR equality, an SMT result, an exact diff shape) is better than an LLM judgment.
5. **A small trusted core.** Only the verifier is trusted. All helpers are untrusted.
6. **Refactor first, then change.** Neutral steps come first. The semantic changes after them are small.

## 3. Architecture

```
           untrusted                               trusted
┌──────────────────────────────┐         ┌──────────────────────────┐
│ agent + its own edit tools   │         │ semilla verify           │
│ semilla tag tools (MCP, CLI) │ claims  │  - entity trees, 3 views │
│ hints, subgroup suggestions  │ ──────▶ │  - claim checkers        │
│ UI, optional LLM intent check│         │  - SMT checks            │
└──────────────────────────────┘         │  - coverage              │
                                         └──────────────────────────┘
                                                   │
                                     accepted / rejected + reasons
```

| Component | Trusted | Purpose |
|---|---|---|
| Verifier | yes | Checks the claims and the coverage. |
| Indexer | yes (shared with the verifier) | Builds the entity trees, the three views, and the workspace graph. |
| Tag tools | no | Let the agent record steps and claims. |
| Helpers | no | Hints, subgroup suggestions, the optional LLM intent check. |
| UI | no | Shows the review. |

Local storage:

| Data | Location |
|---|---|
| Claims, steps, groups | git notes under `refs/notes/semilla` |
| Step snapshots | git trees under `refs/semilla/<commit>/step-<n>` |
| Caches, graph index, review state | `.semilla/` (not in git) |

## 4. The entity tree

### 4.1 Levels

```
workspace
 └─ crate
     └─ module
         └─ item (fn, struct, enum, trait, impl, const, static)
             ├─ signature / header
             └─ body (for a fn)
                 └─ block
                     └─ statement            let, expression statement, item, tail
                         └─ expression       call, method call, binary op, …
                             └─ leaf         literal, path, opaque macro
```

Items can move between crates of the workspace. A `move-item` claim across crates also updates the `use` paths and the `Cargo.toml` dependencies that the move needs.

Rules:

- **Statements give the structure. Expressions give the content.**
- Block-like expressions (`if`, `match`, `loop`, block, closure, `async` block) contain blocks. The tree continues below them.
- Structs have fields. Enums have variants. `impl` blocks have methods. Traits have items.
- Patterns (in `let` and in `match` arms) are small trees with leaves.
- A macro call is an opaque leaf. Known `std` macros (`format!`, `println!`, `assert!`, `vec!`, `matches!`) have their arguments as children.

### 4.2 Expression roles

| Kind | Children (roles) |
|---|---|
| literal, path | none (leaves) |
| binary op | `lhs`, `rhs` |
| unary op, `?`, `&`, `*`, cast | `operand` |
| call | `callee`, `args[i]` |
| method call | `recv`, `args[i]` (the method name is an attribute) |
| field, index | `base`, `index` |
| struct, tuple, array literal | `fields[name]`, `elems[i]` |
| `if` | `cond`, `then`, `else` |
| `match` | `scrutinee`, `arm[i].pat`, `arm[i].guard`, `arm[i].body` |
| loop | `header`, `body` |
| closure | `params`, `body` |

A method chain shows as one flat list of links, not as nested `recv` nodes.

### 4.3 Node IDs and anchors

- Each node has a **stable ID**. Its path and its name are attributes. A move or a rename does not change the ID.
- An anchor is an item ID, a statement path, and a role path:

```
Cart::total/S2/S2.0/cond/rhs
parse/S4/init/recv/args[0]/body
```

- The agent gets anchors from the `anchor` tool. It never writes them by hand.

### 4.4 Three views

Each node has three views. A change is covered only if every view that it touches has an explanation.

| View | Source | What it sees |
|---|---|---|
| **Syntax** | AST without whitespace | everything, including comments and attributes |
| **Signature** | rustc types, rustdoc JSON | signatures, types, trait impls, the public API |
| **Body** | normalized MIR | runtime behavior |

Each expression node also stores its type from the rustc type check.

## 5. Claims

### 5.1 Three types of op

| Type | Ops | Applies to |
|---|---|---|
| **Identity** | `move`, `rename`, `add`, `remove`, `reorder` | any node (`rename` only for nodes with a name) |
| **Own content** | `change-signature`, `change-condition`, `add-field`, `change-operator`, … | the attributes of one node, not its children |
| **Across nodes** | `extract-function`, `inline-function`, `move-statement`, `split-module`, `merge-module` | a subtree that moves to another parent |

A claim on a node covers only its own attributes. Each change to a child needs its own claim. Free edits are the exception: they mask their children. **Free edits exist only at the expression level** (section 5.6). Above the expression level, each change needs a structured op.

### 5.2 Item ops

- `add-item`, `remove-item`, `move-item`, `rename`
- `change-signature`, `change-return-type`, `change-visibility`
- `add-struct-field`, `add-enum-variant`, `add-trait-impl` (with an `affects` list)
- `bump-dependency`, `edit-doc`

### 5.3 Statement ops (structure)

All changes above the expression level use these ops. No free edit exists for a statement, a block, or a function body.

| Group | Ops |
|---|---|
| Add a statement | `add-let`, `add-expression-statement` (a call, an assignment), `add-if`, `add-match`, `add-loop`, `add-return`, `add-break`, `add-continue`, `add-macro-statement`, `add-nested-item` |
| Block | `delete-statement`, `move-statement` (in a block or to another block), `wrap-statements` (in `if`, a loop, a block, `unsafe`), `unwrap-statements` |
| Variables | `extract-variable`, `inline-variable`, `rename-local`, `change-mutability`, `change-type-annotation` |
| `if` | `add-else`, `add-else-if`, `remove-else`, `remove-if` (keep one branch), `invert-if`, `swap-branches` |
| `match` | `add-arm`, `remove-arm`, `move-arm`, `merge-arms`, `split-arm`, `reorder-arms` |
| Loops | `change-loop-kind` (`for`, `while`, `loop`), `add-loop-label` |
| Conversions | `if-to-match`, `match-to-if`, `if-let-to-let-else`, `let-else-to-match`, `loop-to-iterator`, `iterator-to-loop` |
| Attributes | `add-attribute`, `remove-attribute` (on a statement) |

The same ops work at an expression position for block-like expressions. For example, `add-match` can create the `match` in `let x = match y { … };`.

**New code:** an `add-*` op carries the full new subtree, for example the body of a new `if`. New code is not a mask. The verifier builds its full tree, and the UI shows it in full, as a tree and in a flow view.

### 5.4 Expression ops (content)

| Group | Ops |
|---|---|
| Leaves | `change-literal`, `change-path` |
| Operators | `change-operator`, `negate`, `swap-operands` |
| Wrap | `wrap-expression` (`Ok(x)`, `Some(x)`, `&x`, `x.clone()`, `x as T`, `x?`), `unwrap-expression` |
| Calls | `add-argument`, `remove-argument`, `chain-method`, `remove-chain-link` |
| Extract | `extract-variable`, `extract-constant`, `extract-closure` |
| Many sites | `rewrite-pattern` (one rule for many sites; `sites: all` or a list) |
| Patterns | `change-pattern` (in `let` and in `match` arms), `add-binding`, `remove-binding` |
| Free | `replace-expression`, `replace-pattern`, `replace-type` |

Header changes are expression ops on a role. For example, `change-condition` is an edit of `if.cond`, and `change-guard` is an edit of `arm[i].guard`.

### 5.5 Rust-specific groups

- **Borrow checker fixes:** `add-clone`, `borrow-to-owned`, `wrap-in-rc`, `wrap-in-arc`, `change-borrow`. They change performance, not logic. A custom `Clone` impl rejects `add-clone`.
- **Panic sites:** the verifier counts the explicit sites (`unwrap`, `expect`, `panic!`, `unreachable!`, `todo!`, slice indexing). A claim gives the delta.
- **Always spotlight:** changes in `unsafe`, `#[cfg(...)]`, features, `build.rs`, and proc macro crates.

### 5.6 Free edits: the expression level only

The free ops are `replace-expression`, `replace-pattern`, and `replace-type`. They have three limits:

1. **Block-free:** the replaced subtree, old and new, must not contain a block-like expression with statements (`if`, `match`, a loop, a block, a closure with a block body, an `async` block). A change that adds, removes, or edits such an expression needs a structured op. An expression closure such as `|x| x + 1` is block-free.
2. **The tightness rule:** the verifier finds the lowest node that contains each change. A free claim must be on that node, or close to it. If the claim is too wide, the verifier rejects it, with a hint to the smaller node. One free claim on the lowest common node of several changes is correct. The agent does not need one claim for each leaf.
3. **No free edit above the expression level.** A free claim on a statement, a block, or a function body is `malformed`.

Order of preference:

| Rank | Op | Scope |
|---|---|---|
| 1 | exact expression ops (`change-operator`, `wrap-expression`, …) | one expression |
| 2 | `replace-expression`, `replace-pattern`, `replace-type` | one block-free subtree |

Each change above the expression level is a composition of structured ops: add, delete, move, wrap, unwrap, convert, and header edits.

**Macro calls:** the input of an opaque macro is a token tree, not an expression. `replace-macro-input` edits it. It gets a spotlight, because the token tree can contain statements (see B20).

### 5.7 The rule table

The rule table is data. The verifier and the docs both use it.

| Op | Node kinds | The children |
|---|---|---|
| `move`, `rename` | any / named nodes | unchanged, unless other claims say otherwise |
| `remove` | any | all removed |
| `add` | any | all new |
| `extract-function` | a range of sibling statements | moved unchanged; a call takes their place |
| `inline-function` | a call statement | the callee body takes its place, unchanged |
| `change-signature` | fn | the body is not affected |
| `add-if`, `add-match`, `add-loop`, other `add-*` | statement or block-like expression | all new (the reviewer reads them) |
| `delete-statement` | statement | all removed |
| `move-statement` | statement | moved unchanged |
| `wrap-statements`, `unwrap-statements` | a range of sibling statements | unchanged, in the new parent |
| conversions (`if-to-match`, …) | `if`, `match`, `let-else`, loop | the bodies move unchanged into the new form |
| `replace-expression`, `replace-pattern`, `replace-type` | a block-free subtree | masked (tightness rule) |

## 6. Steps, groups, and subgroups

### 6.1 Steps (the verification unit)

A step has one op or an atomic group of ops, an intent text, a declared class, and a tree snapshot.

```
parent ─▶ step 1 ─▶ step 2 ─▶ … ─▶ step N  ==  commit tree
```

- All claims in one step refer to the "before" snapshot. They are simultaneous. Steps are sequential.
- New nodes get "after" IDs from the claim that creates them.
- Each snapshot must pass `cargo check`. An atomic group can contain ops that do not compile alone.

### 6.2 Groups (declared by the agent)

A group is a set of steps with one intent. It is about the size of one commit. It has a title, a kind, and a "why" text.

| Kind | Rule that the verifier checks |
|---|---|
| `refactor` | all steps are neutral |
| `rename`, `move` | only identity claims |
| `api` | only signatures, types, and their call sites |
| `tests` | only test code |
| `deps` | only `Cargo.toml`, `Cargo.lock`, and the call sites |
| `fix`, `feature`, `behavior` | no rule |

- Each step belongs to exactly one group.
- If a group has two parts with no link in the call graph or the type graph, the UI suggests a split (a warning only).

### 6.3 Subgroups (suggested by the engine, edited by the agent)

The engine suggests subgroups by **role**:

| Role | Contents | Default |
|---|---|---|
| Interface | new or changed types, signatures, traits, public API | open |
| Core logic | control flow and logic changes | open |
| Tests | new or changed tests, linked to the core nodes that they reach | open |
| Config | `Cargo.toml`, features, `build.rs` | open |
| Propagation | call site updates, `rewrite-pattern`, match arms that the compiler needs | folded |
| Mechanical | renames, moves, extracts, borrow fixes, formatting | folded |

The agent can rename, merge, and split subgroups, move claims, and add custom subgroups.

Rules for the edited layout:

1. The role label must match the contents. For example, `Mechanical` can contain only proved neutral claims.
2. The contents decide the fold state, not the label. A subgroup with a claim that is not neutral is open by default.
3. The engine computes role badges for custom subgroups, for example `custom · 2 ctrl flow · 1 logic`.
4. Each claim is in exactly one subgroup.
5. When the agent adds steps, the engine keeps the agent's edits. It puts new claims in the suggested subgroup and marks them.

The UI marks the subgroups that the agent edited. The reviewer can switch to the engine suggestion.

## 7. Agent tools

The agent keeps its own edit tools. Semilla gives it tag tools, through an MCP server and the CLI.

| Tool | Purpose |
|---|---|
| `step_begin(intent)` | Starts a step and saves the tree (`git write-tree`). |
| `claim(op, args)` | Adds a claim. A typed JSON schema checks the syntax immediately. |
| `step_end()` | Ends the step and saves the tree. |
| `anchor(file, line, column)` | Returns the canonical anchor for a location. |
| `check_step()` | Verifies the current step only. |
| `tag(commit, claims)` | "Tag after" mode: all steps and claims at one time. |
| `group_set(group)` | Declares a group: title, kind, why, steps. |
| `subgroups_suggest(group)` | Returns the engine's subgroup suggestion. |
| `subgroups_set(group, layout)` | Sends the final subgroup layout. |

Optional helpers (untrusted): edit tools that also add a claim, for example `rename_and_claim`.

The `check_step()` result contains the change tree, with the uncovered nodes marked. The agent sees the same structure as the reviewer.

## 8. Verification

### 8.1 Inputs and checks before verification

```
parent tree, step snapshots S1..SN, commit tree, claims, groups, subgroups
```

1. **Chain:** S0 is the parent. After `rustfmt`, SN is the same as the commit tree.
2. **Build:** each snapshot passes `cargo check`.
3. **Syntax of claims:** each claim matches the schema. Each anchor resolves. If not, the result is `malformed`.

### 8.2 The normal form ladder

Each normalization step must keep the behavior the same. Then "equal after normalization" proves "equivalent". **The normalization keeps all drops**, because drop order is observable in Rust.

| Level | Form | Absorbs | MVP |
|---|---|---|---|
| N0 | MIR with only the required passes (`-Zmir-opt-level=0`), without names, spans, storage markers, and borrow-check-only statements; a canonical block and local order; `move` treated as a copy; stable closure names | formatting, comments, imports, local renames, type annotations, lifetimes, visibility, `mut` removal, plain `if` inversion | yes |
| N1 | N0 + call targets addressed by content hash (a Merkle tree over the call graph; one hash for each strongly connected component) | item renames and moves | yes |
| N2 | rustc optimized MIR, with the same flags on both sides | extract and inline variable, temporaries, constant folding | yes |
| N3 | N2 + bounded inlining of the workspace's own functions | extract and inline function | yes |
| N4 | e-graphs over a dataflow graph (`egglog`) | reordered independent statements, inverted `if`, conversions, algebra | yes |

**Panic locations are not behavior.** Two panics with the same message are the same, also at different source locations. The order of panics with different messages is behavior.

**The rustc inliner is not a proof method.** At N2, rustc inlines some small functions, so some extracts and renames become equal. This depends on heuristics. N1 and N3 must prove these changes (spike S1, finding 5).

The verifier tries the levels in order and stops at the first equal level. It stores the level as the proof type, for example `neutral (N1)`. If no level is equal, it uses N0 to find the changed nodes, because N0 is the closest to the source.

#### N3: bounded inlining

- Inline only functions of the workspace, never external functions.
- Inline to a fixed depth (for example, 2) and a fixed size limit.
- Do not inline recursive functions (functions in a cycle of the call graph).

#### N4: equality saturation

1. **Convert each body to a dataflow graph.** Use a form with structured control flow: a "gamma" node for a branch, and a "theta" node for a loop (as in RVSDG). All side effects (writes through `&mut`, calls, I/O, drops, panics) take and return an explicit **state value**. So the effects keep their order, and only pure parts can move.
2. **Run equality saturation** with `egglog` on the "before" and "after" graphs together.
3. **Use audited rewrite rules only.** Each rule must keep the behavior the same for the Rust types it applies to. The rule list is part of the trusted core, like the normalization rules.
4. **Use limits.** Node, iteration, and time limits stop the run. A stop gives `unproved`, never a false proof.
5. **Show the proof.** The chain of rules that connects the two graphs goes into the claim result, for example `neutral (N4: invert-if, de-morgan)`.

The SMT checks (section 8.8) and N4 work together. N4 handles the structure (branches, loops, statement order). SMT handles the conditions and the integer expressions inside them.

### 8.3 Match the items

1. Identity claims give a map from "before" IDs to "after" IDs at all levels.
2. Unclaimed nodes match by the same path and the same kind.
3. `rename` claims give a rename map. `add-struct-field` and `add-enum-variant` give index maps. The verifier applies the maps before each compare.
4. A node on one side only, with no claim, is uncovered.

### 8.4 Indirect changes

The MIR of a function can change when its text does not change. Examples: a new trait impl changes method selection, a changed generic function, a changed `macro_rules!`, a changed type of a variable.

The verifier hashes the normalized MIR of **every** body in the workspace. A body with a new hash and no text change is an indirect change. A claim must explain it, for example with an `affects` list.

### 8.5 Check methods

| Method | How it works | Claims |
|---|---|---|
| **Predict** | Compute the expected "after" from "before" and the claim. Compare. | identity claims, neutral claims (N0–N4) |
| **Match the diff shape** | The diff at the anchor must have the exact shape and content of the claim. | most statement and expression ops |
| **Mask** | Hide the anchor subtree on both sides. The rest must be equal. The subtree must be block-free. | `replace-expression`, `replace-pattern`, `replace-type`, `replace-macro-input` |
| **SMT** | Prove a relation between the old and new expression. | effects, neutral expression rewrites |

The verifier does not run refactors. It checks that the diff matches the claim.

### 8.6 Hierarchical check (top-down)

1. Build the trees and the three views.
2. Apply the identity claims and get the ID map.
3. Apply the claims across nodes. Check that each moved subtree is the same in its new place.
4. For each pair of matched nodes: compare the node's own attributes, assign each difference to a claim, align the children, and repeat for each child.
5. Compare the MIR of each matched function. For `extract-function`, inline the new function and compare again.

### 8.7 Body changes and spans

- The verifier maps each MIR change to the lowest expression node whose span contains it. This mapping uses N0.
- A drop belongs to the statement that declared the variable.
- If a span is too wide (macros, some desugarings), the verifier uses the statement level for that change.

### 8.8 SMT checks

SMT is in the MVP.

| Check | Example | Result |
|---|---|---|
| Condition effect | `x < n` → `x <= n` | `widen`, with an example input |
| Neutral expression rewrite | `!(a < b)` → `a >= b` | `neutral (SMT)` |
| `swap-operands` | `a + b` → `b + a` | `neutral` if the op is commutative for the types |
| `invert-if`, `swap-branches` | the new condition is the negation | `neutral` |
| Guard change | `x > 0` → `x >= 1` on integers | `equivalent` |

Effect labels:

| Effect | Meaning | Proof |
|---|---|---|
| `narrow` | Fewer inputs take the branch. | `to → from` |
| `widen` | More inputs take the branch. | `from → to` |
| `negate` | The opposite inputs take the branch. | `to ≡ ¬from` |
| `equivalent` | No behavior change. | `to ≡ from` |
| `unrelated` | No simple relation. | none of the above |

The engine computes the effect label. The agent does not declare it.

Encoding:

- **Interface:** SMT-LIB text through a pipe to a solver process (Z3 or cvc5). The UI can show each query.
- **Integers:** bitvectors with the exact width and sign from rustc.
- **Overflow:** the effect is computed under "no panic". The verifier reports separately when the panic conditions are different.
- **Floats:** `unrelated` in the MVP.
- **Atoms:** an expression that SMT cannot model becomes an uninterpreted value. The same text, after the rename map, gives the same value. If the set or the order of the calls changes, the result is `unrelated`.
- **Witness:** for `narrow`, `widen`, and `unrelated`, the solver gives an example input.
- **Safety:** a timeout or `unknown` gives `unrelated`. Each query has a time limit (about 2 seconds).
- **Determinism:** a pinned solver version, a fixed random seed, and a cache by query hash.

### 8.9 Extra checks for some claims

| Claim | Extra check |
|---|---|
| `add-clone` | The `Clone` impl is a derive or from `std`. |
| logic claims | The graph shape inside the anchor did not change. |
| `swap-operands` | Both operands have no side effects (the MIR shows the calls). |
| `reorder-arms` | The patterns do not overlap (rustc pattern analysis). |
| `change-visibility`, `api` groups | `cargo-semver-checks` or a rustdoc JSON compare checks the API level. |
| `bump-dependency` | Only that line changed. `Cargo.lock` agrees. |

### 8.10 Coverage

The verifier assigns each difference, in all three views, to one claim.

1. A difference without a claim is **uncovered**.
2. A claim without a difference is **empty**.
3. Masks must not overlap. If an exact claim is inside a mask, the verifier asks for one of them only.

### 8.11 Claim results

| Result | Meaning | Effect |
|---|---|---|
| `verified` | The check passed. The result stores the proof type. | OK |
| `unproved` | A neutral claim that no level could prove. | Rejected. The hint says to tag it as logic. No other cost. |
| `refuted` | Evidence of a false claim. | Hard fail |
| `empty` | The claim explains nothing. | Fail |
| `malformed` | Bad syntax, or the anchor does not resolve. | Fail |

The change set is **accepted** only if every claim is `verified`, nothing is uncovered, and the group and subgroup rules pass.

### 8.12 Configurations

A claim must be true in each configuration: the default features, each feature set that the project lists, and `--tests` when the test layer is on.

In a workspace, Cargo unifies the features across the member crates. The verifier uses the same feature resolution as `cargo check --workspace`.

### 8.13 What the verifier trusts

| Trusted | Not trusted |
|---|---|
| rustc (MIR, and the optimizations for N2) | the agent |
| the normalization rules | rust-analyzer |
| the claim checkers and the rule table | the hints and the suggestions |
| the SMT solver | the LLM intent check |
| `egglog` and the N4 rewrite rules | |
| `rustfmt`, git | the UI |

### 8.14 Cost

- Check the last snapshot first. Bisect only when a later check fails (about log2(N) compiles).
- In a workspace, rebuild only the crates that a step changes and the crates that depend on them.
- Compare the full views only for the nodes that the claims touch. Compare only the MIR hashes for all other bodies.
- Run N3, N4, and SMT only for the claims that need them.
- Cache all results by tree hash and by query hash.
- `check_step()` checks one step, so the agent gets fast feedback.

### 8.15 Example report

```
step 1  "rename Cart::sum to Cart::total"
  ✓ rename crate::cart::Cart::sum → total        verified (N1)
  ✓ edit-doc Cart::total                          verified (syntax)

step 2  "return 0 for an empty cart"
  ✓ add-if Cart::total > S0                       verified (shape)
  ✗ uncovered  Cart::total/S2/S2.0/cond
       syntax: item.qty < max  →  item.qty <= max
       hint: add change-operator (the engine computes: widen)

step 3  "accept u32 quantities"
  ✓ add-trait-impl From<Qty> for u32              verified
  ✗ uncovered  crate::report::summary  (indirect: MIR changed, text did not)
       hint: add crate::report::summary to "affects"

result: rejected (2 uncovered)
```

## 9. Incentives

### 9.1 Hard rules

1. No uncovered change.
2. No change outside the claim anchors.
3. No empty claim.
4. An over-claim (`refuted`) is the worst failure. It costs more than an honest free edit.
5. A free claim must be block-free and pass the tightness rule.
6. No free claim above the expression level.

### 9.2 Score

| Claim type | Score |
|---|---|
| Neutral, proved | highest |
| Structured op (statement or expression) | high |
| `replace-expression`, `replace-pattern`, `replace-type` | medium; the cost grows with the size of the subtree |
| `replace-macro-input` | low |

The agent instructions, a local CI, or an agent training loop can use the score as a target.

### 9.3 Easy compliance

- Tag tools record the steps while the agent works.
- The hints in each rejection name the exact claim to add.
- `check_step()` is fast.

## 10. The workspace graph

### 10.1 Levels

```
Level 0  workspace map    member crates and their dependencies
Level 1  crate map        modules and their dependencies
Level 2  call graph       functions, with call edges (also across crates)
Level 3  function flow    statements and regions in one function
```

The tree (section 4) continues below level 3 to statements and expressions. The graph stops at statements. A changed expression shows in the label of its statement node.

- Each node and edge has a status: `same`, `added`, `removed`, `changed`, `moved`.
- Edge kinds: `call`, `flow`, `return`, `err`, `panic`, `dispatch`.
- The region tree comes from the source-level structure (`semilla-syntax/src/flow.rs`). Spike S2 measured Charon LLBC and rejected it for now: no `async` support, loops and matches lose their source shape, and it needs a second nightly. The MIR driver will add the facts that the source does not show (exact `?` edges, panic sites, drops), mapped to statements by their spans.
- For `async fn`, the graph comes from the source-level regions. `.await` is a node. The MIR state machine is used only for the equality check.

### 10.2 Dependencies (option B)

Member crates of the workspace are not external. They have the full graph.

Calls to crates outside the workspace go to an `external` leaf with a signature summary:

- A return type of `Result` or `Option` gives an `err` edge.
- An `&mut` argument can change.
- Badges for `async`, `unsafe`, `const`.
- A `-> !` return type makes the leaf an exit.

Closures that the crate passes to a dependency become child nodes of the external call, with a "called 0..n times" edge.

Later work: expand a dependency on demand, and a semantic diff of dependency upgrades.

### 10.3 Entry points and tests

- Entry points: `main` of each binary crate, `pub` items of each library crate, `#[test]` functions.
- Tests are an optional layer. The layer is off by default.

### 10.4 Index and cache

`semilla index` builds the graph and stores it in `.semilla/`. The tree hash is the key. Each body has a MIR hash, so the indexer analyzes only the changed bodies again.

## 11. Review UI

`semilla ui` starts an HTTP server on `localhost` and opens the browser. The binary contains all the JavaScript. The UI works offline.

### 11.1 The review hierarchy

```
Change set          summary: groups, risk, coverage, API level, panic delta
 └─ Group           one intent (agent)
     └─ Subgroup    one role or a custom concern (engine + agent)
         └─ Component   flow view, signature diff, rename list, site list
             └─ Expression diff
```

Three ways to navigate the same claims:

- **By intent** (groups and subgroups): the default.
- **By structure** (the change tree).
- **By time** (the steps): to replay the work.

### 11.2 Order

1. **Groups:** by risk (control flow, `unsafe`, public API, panic delta), then by dependency. A group that adds a type comes before the group that uses it.
2. **Subgroups:** Interface → Core logic → Tests → Config → Propagation → Mechanical.
3. **Components:** by dependency in the call graph. A callee comes before its callers.

### 11.3 The change-set view

```
┌ Change set: 4 groups · 31 steps · coverage 0.94 · API: minor · panics −2 ┐
│                                                                           │
│ ▾ G1  fix   "Return 0 for an empty cart"                   risk ●●○      │
│    ├ Core logic   cart.rs                                  1 ctrl flow    │
│    │   └ Cart::total  [flow view] + if items.is_empty() → return Ok(0)   │
│    ├ Tests        cart_tests.rs                            1 new, reaches │
│    │                                                       Cart::total ✓  │
│    └ Mechanical   (folded) 2 renames                       ✓ proved       │
│                                                                           │
│ ▸ G2  api   "Return Result from Cart::total"               risk ●●●      │
│    Interface · Propagation (14 sites, 1 rule) · Mechanical                │
│                                                                           │
│ ▸ G3  refactor "Extract pricing into billing module"       ✓ all neutral │
│ ▸ G4  tests "Cover quantity limits"                                       │
└───────────────────────────────────────────────────────────────────────────┘
```

A subgroup that crosses files shows its locations as a strip under its title, for example `cart.rs · billing.rs · api/handler.rs`.

### 11.4 The change tree

```
crate
└─ ~ mod cart → mod billing                                  moved
   ├─ ~ fn Cart::sum → Cart::total                            renamed
   │  │    1 ctrl flow · 2 logic · 1 extract · 2 unchanged
   │  ├─ ~ signature   -> u32  →  -> Result<u32, CartError>  logic
   │  ├─ + if self.items.is_empty() { return Ok(0) }          ctrl flow
   │  ├─   let mut sum = 0;                                    ≡
   │  ├─ ~ for item in &self.items
   │  │  ├─ ~ if item.qty > MAX_QTY …
   │  │  │  └─ ~ cond: item.qty ⟦>⟧→⟦>=⟧ MAX_QTY               widen
   │  │  └─ ⤴ sum += item.price * item.qty  →  line_total()   extracted
   │  └─ ~ tail: ⟦sum⟧→⟦Ok(sum)⟧                              wrap-expression
   ├─ + fn line_total                     ⤴ from Cart::total  neutral
   └─ ≡ 4 items unchanged
```

| Marker | Meaning |
|---|---|
| `+` | added |
| `−` | removed (grey) |
| `~` | changed (the node or its children) |
| `→` | moved or renamed |
| `⤴` | extracted or inlined, with a link to the other place |
| `≡` | unchanged (folded) |
| `✗` | uncovered (in the agent's `check_step()` output) |

- The tree shows statements by default. It expands expressions only on the path to a changed leaf.
- `⟦old⟧→⟦new⟧` marks a changed leaf inside a line.
- Code that moved across nodes shows in both places, with a link.

### 11.5 Flow views

A **flow view** is the smallest part of a control flow graph that contains a change. (The code calls it a "CFG frame".)

Selection:

1. The frame is the lowest common ancestor of all changed nodes.
2. The UI expands the changed children. It expands a child again if it has a control flow change.
3. Each unchanged subtree becomes one summary node, for example `… 4 statements unchanged`.
4. A changed leaf shows its diff inline.
5. If the frame has more than about 12 visible nodes, the UI collapses the nodes with the fewest changes.

Moves are a special case: the UI shows the move with the old and new places marked.

```mermaid
flowchart TD
  IN([from: function entry]) --> C[for item in items]
  C --> D{"item.qty <= max"}:::changed
  D -- yes --> E[… 2 statements unchanged]
  D -- no --> X(["exit: return Err"])
  C -- done --> OUT([to: return Ok])
  classDef changed fill:#fff3c4,stroke:#b80
```

### 11.6 Navigation

- A click on a component opens its flow view.
- The entry and exit stubs open the caller, the callee, or the next region. The reviewer can walk all the unchanged flow.
- The breadcrumb moves up the levels. A click on a node moves down.
- Back and forward history.
- The path view shows the call paths from the entry points to a changed node.
- The impact overlay marks all callers that can reach a changed node.
- A toggle shows the before, after, or merged graph.
- At levels 0 to 2, the colors show the number of changes in each crate, module, and function.

### 11.7 Search

| Query | Result |
|---|---|
| `total` | fuzzy search on symbol names |
| `"qty <= max"` | text search on labels and source |
| `changed:`, `class:logic`, `step:3`, `group:G2` | filters by diff status |
| `kind:panic`, `kind:unsafe`, `kind:err` | panic sites, `unsafe` blocks, error exits |
| `callers:Cart::total`, `callees:…` | graph neighbors |
| `path:main -> Cart::total` | call paths |

### 11.8 Review state

- The reviewer marks a subgroup or a group as reviewed. Semilla saves the state in `.semilla/`.
- The state follows the claims, not the lines. When the agent changes its work, only the changed claims become "not reviewed".

### 11.9 Renderers

- **Workspace map, crate map, call graph (levels 0–2):** Cytoscape.js with the ELK layout.
- **Flow views (level 3):** Mermaid. Mermaid is also the export format.

### 11.10 Server API

```
GET  /api/changeset
GET  /api/group/:id
GET  /api/search?q=…
GET  /api/node/:id
GET  /api/node/:id/children
GET  /api/node/:id/neighbors
GET  /api/flow/:node
GET  /api/path?from=…&to=…
GET  /api/source/:id?v=before|after
POST /api/review/:claim_or_group
```

## 12. Tool stack

| Need | Tool |
|---|---|
| MIR access | `rustc_driver` / `rustc_public` (pinned nightly), behind one internal "MIR adapter" |
| Structured MIR | Charon (LLBC) |
| Syntax view | rust-analyzer syntax crates (`ra_ap_syntax`, pinned) |
| SMT | Z3 or cvc5, through SMT-LIB text |
| Format | `rustfmt` |
| Public API | `cargo-semver-checks`, rustdoc JSON |
| Equivalence (N4) | `egglog` |
| Equivalence, later | Kani, `cargo-fuzz` |
| Agent tools | an MCP server |
| Local UI | `axum`, Cytoscape.js + ELK, Mermaid |

The analyzer runs as a separate driver binary, like Clippy or Miri. The user's crate keeps its own toolchain for normal builds.

## 13. Decisions

| Topic | Decision |
|---|---|
| First language | Rust |
| Scope | The full Cargo workspace |
| Hosting | Local only. No GitHub. |
| Roles | The agent declares. Semilla verifies. Small trusted core. |
| Agent tools | Tag tools. The agent keeps its own edit tools. |
| Tree leaves | Expressions |
| Free edits | Expression level only (expressions, patterns, types), block-free, with a tightness rule. Structured ops for everything above. |
| Hierarchy | Stable IDs. Unclaimed means unchanged. |
| Normal forms in the MVP | N0 to N4 |
| SMT | In the MVP |
| Review hierarchy | Group (agent) → subgroup (engine suggests, agent edits) → component |
| Review UI | Local web UI on `localhost` |
| Graph scope | The full workspace, searchable |
| Dependencies | Signature summary leaves (option B) |
| Entry points | `main`, `pub` items, `#[test]` functions |
| Tests in the graph | Optional layer, off by default |
| UI term | "Flow view" in the UI. "CFG frame" in the code. |

## 14. Open questions

1. Is a pinned nightly toolchain acceptable for the analyzer?
2. Tightness threshold for free claims (spike S1 measures real commits).
3. Default node budget for a flow view (about 12)?
4. Solver: Z3 or cvc5 as the default? How to install it (a pinned binary in `.semilla/tools/`, or the system package)?
5. N4 rule set: which rewrite rules go into the first audited list?

## 15. Foreseeable blockers

Severity: **Critical** can stop the approach. **High** needs real work, with a known fix. **Medium** and **Low** have simple fixes or limits.

| # | Blocker | Severity | Fix |
|---|---|---|---|
| B1 | MIR normalization gives false results | Critical | Small, tested normalization rules from a corpus of real refactors. A false "logic" is safe. Keep all drops. |
| B2 | Dependency on compiler internals (nightly, `rustc_public` not published) | Critical | One MIR adapter, a pinned nightly with a fixed update schedule, a separate driver binary. |
| B3 | Compile cost per step | Critical | Check the last snapshot first, then bisect. Incremental builds. Caches by tree hash. `check_step()` for one step. |
| B4 | Charon is alpha software | High | Spike S2. Fallback: build the regions from raw MIR and HIR spans (dominator trees). Untranslated functions are opaque. |
| B5 | Steps that compile in "tag after" mode | High | `step_begin` / `step_end` while the agent works. Atomic groups. |
| B6 | `async` MIR is a state machine | High | MIR only for equality. Graphs from source-level regions. |
| B7 | MIR spans: desugarings and macros | Medium | Group blocks by span and desugaring kind. Use the statement level when a span is too wide. |
| B8 | The syntax and signature views are new work | Medium | Reuse rust-analyzer syntax crates and rustdoc JSON. |
| B9 | Indirect changes and long `affects` lists | Medium | MIR hashes for all bodies. A rule to declare an effect for a full module. |
| B10 | Tree diff inside a function | Medium | GumTree on one function at a time, with the claim anchors as fixed points. |
| B11 | Features and `cfg` | Medium | Default features in the MVP. Listed feature sets later. Code that no checked set compiles gets a spotlight. |
| B12 | SMT encoding of exact Rust semantics | Medium | Bitvectors from rustc types. Effects under "no panic". Floats are `unrelated`. Atoms with changed calls are `unrelated`. |
| B13 | The SMT solver is trusted | Medium | Pinned version. Later: a second solver, or cvc5 proof certificates. |
| B14 | Tightness threshold | Low | Measure it in spike S1. |
| B15 | Deep trees in method chains and closures | Low | Flat chains in the tree. Closures as child entities. |
| B16 | Build scripts and proc macros | Low | Class "other" with a spotlight. The generated code is still in the MIR. |
| B17 | Panic site noise from implicit checks | Low | Count explicit sites by default. |
| B18 | Graph scale and layout | Low | Cytoscape + ELK for the map, Mermaid for small flow views, lazy loading. |
| B19 | Agents that game the score or the subgroups | Low | The size cost on free edits, the tightness rule, and the subgroup rules. |
| B20 | Macros with statements in their input (`tokio::select!`, custom DSLs) | Medium | `replace-macro-input` with a spotlight. Later: parse the input of known macros into statements. |
| B21 | More claims for the agent, because no free edit exists above expressions | Medium | An untrusted helper `suggest_claims(step)` that converts the tree diff (a GumTree edit script) into structured claims. The agent checks and sends them. |
| B22 | Gaps in the structured op catalog | Medium | Prove in a test that each statement-level tree diff decomposes into the ops of section 5.3. A real change that does not decompose shows a missing op. |
| B23 | N4: MIR to a dataflow graph with explicit effects | Critical | This is the largest single part of the work. Start with loop-free functions, then add loops (theta nodes). Every unsupported construct gives `unproved`, not a failure. Spike S6. |
| B24 | N4: soundness of the rewrite rules | High | A small audited rule list. Test each rule with the SMT solver on its Rust types before it goes into the list. |
| B25 | N4 and N3: cost | High | Strict node, iteration, and time limits. Run them only after N0–N2 fail. Cache by the hash of both bodies. |
| B26 | Workspace scale: more crates, more MIR, longer builds | High | Rebuild only the changed crates and their dependents. One index per crate, merged at query time. |
| B27 | Moves across crates | Medium | `move-item` across crates also checks the `use` and `Cargo.toml` changes. N1 hashes work across crates. |

Resolved: the verifier does not replay refactors, so rust-analyzer version changes do not affect the trusted core.

## 16. Plan

### 16.1 Spikes

| # | Spike | Question | Blockers |
|---|---|---|---|
| S1 | MIR normalization | For each rust-analyzer assist, which level (N0 to N3) makes the MIR equal? | B1, B2, B14 |
| S2 | Charon coverage | What percentage of functions does Charon translate on 3 real crates? Closures? `async`? | B4, B6 |
| S3 | Compile cost | How long do `cargo check` and the MIR extraction take per snapshot on a medium crate? | B3 |
| S4 | SMT encoding | Can real conditions from 3 crates go to SMT-LIB with rustc types? How many get a label other than `unrelated`? | B12 |
| S5 | UI prototype | Can Cytoscape + Mermaid show the workspace graph and a flow view from S2 output? | B18 |
| S6 | N4 prototype | Can 20 real loop-free functions go to a dataflow graph in `egglog`? Does N4 prove `invert-if` and reordered independent statements? | B23, B24, B25 |

S1 is the most important. If neutral refactors do not keep the MIR equal after a small normalization, the "neutral" class needs another proof method.

### 16.2 MVP milestones

| # | Milestone | Contents |
|---|---|---|
| M0 | Spikes | S1–S6 |
| M1 | Index | Workspace entity tree, anchors, three views, MIR hashes, `semilla index` |
| M2 | Verifier core | Claim format, steps, item matching, N0–N2, predict / match / mask, coverage, the CLI report |
| M3 | Statement and expression ops | The op catalog, block-free free edits, the tightness rule, SMT checks |
| M4 | Strong normal forms | N3 (bounded inlining), N4 (dataflow graph + `egglog`, audited rules) |
| M5 | Agent tools | The MCP server, `check_step()`, `suggest_claims()`, groups, subgroups |
| M6 | UI | Change-set view, change tree, flow views, workspace map, search, review state |

Not in the MVP: dependency expansion and upgrade diffs, Kani and fuzzing, the LLM intent check.

## 17. Prior art

- **Research:** RefactoringMiner 3.x (refactoring-aware AST diff), ARCTIC (intent and drift for AI diffs), structure-aware labels of code changes, differential fuzzing of LLM refactorings (19–35% not equivalent), Tao et al. 2012 (engineers want refactorings and behavior changes separated), Fregnan et al. 2022 (file order changes review attention).
- **Tools:** semantic-review (LLM grouping, no verification), RAID, SemanticDiff, GitClear.
- **Rust analysis:** Charon, Flowistry, mirage-analyzer, cga, coviz.
- **Equivalence:** Peggy and equality saturation, `egg`, Horwitz et al. (dependence graphs), SymDiff, Alive2.
- **Version control:** Jujutsu, Darcs, Pijul, Unison.

No known project combines verified semantic claims, MIR classes, and a crate flow explorer.

## 18. Implementation status (MVP)

All milestones M0 to M6 have an implementation. `cargo test --workspace` runs the tests; `crates/semilla/tests/e2e.rs` runs the real binary on a two-crate git repository.

| Milestone | Status | Notes |
|---|---|---|
| M0 Spikes | S1, S2, S3 done; S4 and S6 done inside the crates | S1 proves 17 of 17 neutral cases; 19 negative controls stay different (`spikes/s1/README.md`). S2: keep the source-level regions, not Charon (`spikes/s2/README.md`). S3: the compile cost is too high for a synchronous check on large crates (`spikes/s3/`), see section 19. |
| M1 Index | Done | `semilla-syntax` (entity tree, anchors, three views), `semilla index` (graph, items, N0/N1 body hashes). |
| M2 Verifier core | Done | `semilla-verify`: predict / match / mask, coverage, indirect changes, class checks, score. |
| M3 Ops and SMT | Done | All ops of section 5 have a checker. Free edits are block-free and tight. `semilla-smt` computes effects with Z3 4.15.4. |
| M4 Strong normal forms | Done | `semilla-normal`: N1 (rename maps, content hashes), N3 (bounded inlining), N4 (loop-free terms in egglog, audited rules). |
| M5 Agent tools | Done | CLI and MCP server: steps, claims, `anchor --before`, `check_step`, `suggest_claims`, groups, subgroups, `tag`. |
| M6 UI | Done | `semilla-ui`: change set, change tree, flow views, workspace map, search, review state. |

### Changes to the design from the implementation

1. **`anchor --before`.** Claims on existing code use the "before" numbering, so the anchor tool can read the tree before the open step.
2. **Known `std` macros** (`println!`, `format!`, `assert!`, `vec!`, …) have their arguments as children (`args[i]`). Other macros stay opaque.
3. **A final block-like expression** (`if`, `match`, a loop) is a statement (`S<i>`), not the `tail`.
4. **Panic locations are not behavior** (section 8.2). N4 has rules that commute asserts with the same message.
5. **The MIR printer** of the driver replaces `Body::dump`, because `dump` can print two different bodies as the same text (cast kinds, constants).
6. **Flow views** come from source-level regions, not from Charon (spike S2).
7. **Subgroups** have IDs, and the engine marks the claims that it adds after the agent's edit (`new_claims`).

### Limits

- The body check covers the default features only (B11).
- N3 does not inline trait methods, closures, generic functions, or functions of other crates. N4 handles loop-free bodies only.
- The external leaves of the workspace graph get their badges from the call sites, not from the callee signatures.
- The API level comes from the claim ops, not from `cargo-semver-checks`.
- The LLM intent check, the revert check, and the dependency upgrade diff are not implemented.

## 19. Background verification

Spike S3 showed that a full check of one step can take a minute on a large crate. The agent must not wait for it. So the check has two phases.

| | Phase 1: at `step_end` and `tag` (synchronous) | Phase 2: in the background |
|---|---|---|
| Contents | The syntax view: anchors, shapes, coverage, the block-free and tightness rules, groups, subgroups | The MIR builds, N1 to N4, SMT effects, indirect changes, the final verdict |
| Time | Under 1 s for most crates | Seconds to minutes |
| Neutral claims | `pending` | `verified`, `unproved`, or `refuted` |

The flow:

1. `step_end` and `tag` run phase 1, and add a phase 2 job to a local queue (`.semilla/queue/`).
2. A worker (`semilla worker`) runs the jobs. `tag` starts it in the background when it does not run, and it stops when the queue is empty. It checks steps in parallel, and caches the trees and the MIR records.
3. The worker stores the report as a git note on the commit (`refs/notes/semilla-report`), with the hashes of the trees and of the claims. A changed claim gives a new job; the old report becomes stale.
4. The agent reads the results with `semilla status` or the MCP tool `results`, and fixes a refuted claim in a new step.
5. The reviewer runs `semilla review <commit>` when the status is "ready". It opens the local UI, or writes a standalone report for humans: one HTML file with all assets inline, or Markdown.

The cost work (B3: rebuild only the changed crates, compare only the changed bodies) stays. It now shortens the wait of the reviewer, not of the agent.

### 19.1 Baseline and deltas

`semilla init` builds a baseline for `HEAD`, and stores it in a content-addressed cache. After that, each step builds only a delta.

| Layer | Baseline | Delta |
|---|---|---|
| Files | The blob hash of each file | The files with a new blob |
| Syntax index | The parsed items of each file, keyed by the blob hash | Parse only the changed files |
| Build folder | One fixed folder for each level, with the incremental state of cargo | Write only the changed files; cargo rebuilds only the changed crates and their dependents |
| MIR records | The records of each crate, keyed by the crate hash | Reuse the records of an unchanged crate |
| Body compare | The content hash of each body | Compare only the bodies with a new hash |

The crate hash must include every input that can change the MIR, because a stale cache gives a false proof:

- the files of the crate, including `build.rs`;
- its `Cargo.toml`, and the locked versions of its dependencies;
- the hashes of the workspace crates that it depends on;
- the toolchain, the level flags, the features, and the driver version.

A crate with a build script or a proc macro can also depend on the environment. Such a crate is rebuilt every time, until the key covers its inputs.

`semilla init` can also install a local `post-commit` hook that queues the phase 2 job of each tagged commit.

### 19.2 Status

Implemented:

- Phase 1: `check` and the MCP tool `check_step`. Neutral claims are `pending`. `check --full` runs both phases.
- Phase 2: `step end` and `tag` add a job to `.semilla/queue/` and start `semilla worker`. The worker stores each report with the hash of its change set, in `.semilla/reports/` and as a git note under `refs/notes/semilla-report`.
- `semilla status [commit]` and the MCP tool `results` show `ready`, `queued`, `stale`, or `missing`.
- `semilla review [commit] [--md FILE] [--html FILE]` waits for phase 2, then opens the UI or writes a standalone report.
- The delta build folders of 19.1, with lazy N2 and N3 builds. The tree cache and the build folders are safe for concurrent processes.

Not implemented yet: the per-file syntax cache, the crate hash keys for records (cargo's fingerprints decide the reuse now), and the `post-commit` hook of `semilla init`.
