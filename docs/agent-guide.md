# Semilla: a guide for coding agents

You write code with your own edit tools. Semilla records what you changed as **claims**, and verifies each claim against the code. A reviewer then reads your claims, not a line diff. This guide tells you how to make claims that pass the first time.

## Setup

- Use the MCP server (`semilla mcp`) or the CLI. The table below gives both names.
- Run the CLI with the repository as the current folder, or with `--repo <folder>`.
- Semilla finds its MIR driver next to its own binary, and Z3 in `.semilla/tools/z3` of the semilla repository. If they are somewhere else, set `SEMILLA_MIR_DRIVER` and `SEMILLA_Z3`. Without Z3, the output has the warning "Z3 is not available", and condition effects are not computed.
- Do not run `cargo` in a way that changes tracked files (for example a `cargo check` that rewrites `Cargo.lock`) inside a step, unless you want to claim that change too.

| CLI | MCP tool |
|---|---|
| `semilla step begin "<intent>" [--class <class>]` | `step_begin` |
| `semilla anchor [--before] [--up N] FILE:LINE[:COLUMN]` | `anchor` (`before: true`, `up: N`) |
| `semilla claim <op> --at <anchor> key=value …` | `claim` |
| `semilla unclaim <id>` | `unclaim` |
| `semilla check [--full]` | `check_step` (`full: true`) |
| `semilla step end` | `step_end` |
| `semilla step reopen` | `step_reopen` |
| `semilla step edit [--intent "…"] [--class <class>\|none]` | `step_edit` |
| `semilla step drop` | `step_drop` |
| `semilla suggest-claims` | `suggest_claims` |
| `semilla group <id> --kind <kind> --title "…" --steps 0,1` | `group_set` |
| `semilla subgroups <group> [--set layout.json]` | `subgroups_suggest`, `subgroups_set` |
| `semilla verify --final` | `verify` (`final: true`) |
| `semilla tag` | `tag` |
| `semilla status [commit]` | `results` |
| `semilla ops [<op>]` | `ops` |

The `--class` values are `neutral`, `logic`, `control_flow`, and `other`. A declared class must be the class that the verifier computes, or the step fails. If you are not sure, do not declare a class: the report shows the computed class. `step edit --class none` removes a declared class.

## The loop

For each small change:

1. Start a step with an intent, for example "return 0 for an empty cart".
2. Get the anchors of the code that you will change. Before you edit, use `anchor`. After you edit, use `anchor --before` with the line numbers of the old file.
3. Edit the code with your own tools.
4. Add one claim for each change, with an op, an anchor, and the arguments of the op. `semilla ops <op>` shows the arguments and an example. A claim with an unknown argument is rejected.
5. Check the step. Fix each `uncovered`, `empty`, `malformed`, or `refuted` result. The result line says `phase 1 ok, N body proof(s) pending` when only the background proofs remain: that is the normal good result.
6. End the step. The background check (phase 2: the body proofs) starts.

If you must fix a claim of a step that you ended, run `step reopen` (it works on the last step, when the working tree did not change since the end of the step), fix the claims, and end the step again. `step drop` removes an open step without changes, or an empty last step.

At the end of the task:

7. Declare a group for each intent (about one commit of work).
8. Look at the subgroup suggestion of each group. Change the layout only if you need a custom subgroup. A group without your layout uses the suggestion.
9. Run `verify --final`. Commit with git, then `tag`.
10. `status HEAD` shows the result of phase 2. Fix a refuted or unproved claim in a new step.

## Anchors

An anchor is an item path, then a path inside the item:

```
shop_core::cart::Cart::total                     the function
shop_core::cart::Cart::total/S2                  statement 2 of the body
shop_core::cart::Cart::total/S2/body/S0/cond     the condition of the first statement in the loop body
shop_core::cart::Cart::total/tail                the tail expression
```

- **Claims on existing code use the numbering before the step.** After you edit, use `anchor --before` and the line numbers of the old file.
- **`add-*` statement claims use the numbering after the step:** the anchor of the new statement. Use `anchor` without `--before`.
- Do not write anchors by hand. Always use the `anchor` tool.
- Without a column, `anchor` gives the lowest statement on the line (or the item, on a `fn`, `mod`, or `impl` line). With a column, it gives the lowest expression at that position.
- To get a whole condition, point the column at its operator (`||`, `&&`, `==`). To get a whole call, point at its closing `)`. Or use `--up N` to go N levels up from the lowest node.
- An argument that names an item (`to`, `new`) can be a full path, or a last name in the same module as the anchor.

## Choose the op

`semilla ops` lists all operations. Use the most exact one:

| Change | Op |
|---|---|
| Rename a function, type, or module | `rename` (`to`) |
| Move an item | `move-item` (`to`) |
| Rename a local variable (all its uses) | `rename-local` (`to`) |
| New or removed function, struct, impl, method, module | `add-item`, `remove-item` |
| Move code into a new function | `extract-function` (`new`) |
| New statement | `add-let`, `add-if`, `add-early-exit`, `add-match`, `add-loop`, `add-return`, `add-expression-statement`, … |
| Removed or moved statement | `delete-statement`, `move-statement` (`to`) |
| Statements into a new `if` or loop | `wrap-statements` (`count`, `with`) |
| Changed condition | `change-condition` (on the `cond` anchor) |
| Changed operator or literal | `change-operator`, `change-literal` |
| `x` becomes `Ok(x)`, `x?`, `x.clone()` | `wrap-expression` (`with`), `add-clone` |
| Same rewrite at many sites | `rewrite-pattern` (`from`, `to`, with `$x` metavariables) |
| Other expression change | `replace-expression` (the smallest expression that contains the change) |
| Input of a non-`std` macro | `replace-macro-input` |
| `Cargo.toml` version | `bump-dependency` (`crate`, `to`) |
| Other non-Rust files | `edit-config` (`file`) |
| Docs and comments | `edit-doc` |

## Rules that the verifier enforces

- **Unclaimed means unchanged.** Each change needs a claim, and each claim needs a change.
- **Free edits exist only for expressions.** `replace-expression` cannot be on a statement, a block, or a function body, and cannot contain an `if`, a `match`, a loop, or a block. Use the structured statement ops for those.
- **Free edits must be tight.** Put the claim on the lowest expression that contains the change, or one level above it.
- **Neutral claims need a body proof of the whole function.** Put refactors (`extract-variable`, `extract-function`, `rename-local`, …) in their own step, before the behavior changes of the same function. The proof covers all changes of the function in the step: if one neutral change cannot be proved, every neutral claim of that function in the step is unproved. So put each neutral change that can fail in its own step.
- **A false claim is the worst result.** A neutral claim with evidence of a behavior change is `refuted`. If a neutral proof fails, the claim is `unproved`: tag the change as logic, at no extra cost.
- **Indirect changes:** if a change makes the compiled code of another function change (a new trait impl, a changed generic), list that function in `affects`. `module::*` covers a whole module.

### Limits of the neutral proofs

Some correct refactors are `unproved`, not `verified`:

- An extracted expression with a closure that borrows a local: after the extraction, the closure borrows the new parameter, not the old local.
- A refactor that changes the branch structure (for example a `||` condition moved into a helper) needs the strongest proof (N4). N4 works only on functions without loops, and only when the extracted function is small (a few blocks) or the function is not too large: the reason is then "the N4 term has too many paths".
- A condition change between method calls (`v.len() > 0` against `!v.is_empty()`): the solver does not know what the methods return, so the effect is `unrelated`.

In these cases, use logic ops for the same change: `add-item` for the new function, and `replace-expression` on the changed expression at the call site (or the statement ops, if a statement changed). Then say in the intent that the step is a refactor, and use a `behavior` group, not a `refactor` group.

## Groups and subgroups

A group is one intent, about one commit. Each step is in exactly one group.

| Kind | Accepts |
|---|---|
| `refactor` | neutral claims only |
| `rename`, `move` | `rename`, `move-item`, `rename-local` only |
| `api` | item claims (signatures, types, new items) and expression claims at call sites |
| `tests` | claims on test code only |
| `deps` | file claims and call site claims |
| `fix`, `feature`, `behavior` | any claim |

Subgroups split a group by role. The engine suggests them. You can rename, merge, or split them, and add `custom` subgroups.

| Role | Accepts |
|---|---|
| `interface` | item claims |
| `core` | any claim |
| `tests` | any claim (use it for test code) |
| `config` | file claims |
| `propagation` | call site claims: expression claims and `rewrite-pattern` |
| `mechanical` | proved neutral claims only |
| `custom` | any claim |

Each claim must be in exactly one subgroup of its group. A layout that breaks a rule is rejected and not saved.

## When a check fails

| Result | What to do |
|---|---|
| `uncovered` | Add the claim in the hint, at the anchor in the report. |
| `empty` | The anchor is wrong, or the change is not there. The hint names the anchor of the nearest change. |
| `malformed` | A wrong argument or anchor. Read the reason. |
| `refuted` | The claim is false. Use the correct op, or a logic op. |
| `unproved` | The neutral proof failed. Tag the change as logic, or make smaller neutral steps. |
| `pending` | Normal in phase 1. Wait for phase 2 (`status`). |

`suggest-claims` gives draft claims for the uncovered changes. Check each draft before you send it with `claim`.
