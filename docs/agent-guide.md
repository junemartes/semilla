# Semilla: a guide for coding agents

You write code with your own edit tools. Semilla records what you changed as **claims**, and verifies each claim against the code. A reviewer then reads your claims, not a line diff. This guide tells you how to make claims that pass the first time.

Use the MCP tools (`semilla mcp`) or the CLI (`semilla …`). The names are the same.

## The loop

For each small change:

1. `step_begin` with an intent, for example "return 0 for an empty cart".
2. Before you edit, get the anchors of the code that you will change: `anchor` with `before: true` after you edit, or `anchor` before you edit.
3. Edit the code with your own tools.
4. Add one claim for each change: `claim` with an `op`, an anchor `at`, and the `args` of the op.
5. `check_step`. It is fast (phase 1). Fix each `uncovered`, `empty`, `malformed`, or `refuted` result. `pending` is normal: the body proofs run in the background.
6. `step_end`.

At the end of the task:

7. `group_set` for each intent (about one commit of work).
8. `subgroups_suggest`, then `subgroups_set` if you want to change the layout.
9. Commit with git, then `tag`. The background check (phase 2) starts.
10. `results` shows the result of phase 2. Fix a refuted or unproved claim in a new step.

## Anchors

An anchor is an item path, then a path inside the item:

```
shop_core::cart::Cart::total                     the function
shop_core::cart::Cart::total/S2                  statement 2 of the body
shop_core::cart::Cart::total/S2/body/S0/cond     the condition of the first statement in the loop body
shop_core::cart::Cart::total/tail                the tail expression
```

- **Claims on existing code use the numbering before the step.** After you edit, use `anchor` with `before: true` and the line numbers of the old file.
- **`add-*` statement claims use the numbering after the step:** the anchor of the new statement. Use `anchor` without `before`.
- Do not write anchors by hand. Always use the `anchor` tool.
- Without a column, `anchor` gives the lowest statement on the line. With a column, it gives the lowest expression at that position.

## Choose the op

`ops` lists all operations. Use the most exact one:

| Change | Op |
|---|---|
| Rename a function, type, or module | `rename` (`to`) |
| Move an item | `move-item` (`to`) |
| Rename a local variable | `rename-local` (`to`) |
| New or removed function, struct, impl | `add-item`, `remove-item` |
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
- **Neutral claims need a body proof of the whole function.** Put refactors (`extract-variable`, `invert-if`, `rename-local`, …) in their own step, before the behavior changes of the same function.
- **A false claim is the worst result.** A neutral claim that changes behavior is `refuted`. If you are not sure that a change is neutral, use a logic op. `unproved` costs nothing more than an honest logic claim.
- **Indirect changes:** if a change makes the compiled code of another function change (a new trait impl, a changed generic), list that function in `affects`.

## Groups

- One group is one intent, about one commit: `refactor`, `rename`, `move`, `api`, `tests`, `deps`, `fix`, `feature`, or `behavior`.
- A `refactor` group can contain only neutral claims. A `tests` group can contain only test code.
- Each step is in exactly one group.

## When a check fails

| Result | What to do |
|---|---|
| `uncovered` | Add the claim in the hint, at the anchor in the report. |
| `empty` | The anchor is wrong, or the change is not there. Use `anchor` again. |
| `malformed` | A wrong argument or anchor. Read the reason. |
| `refuted` | The claim is false. Use the correct op, or a logic op. |
| `unproved` | The neutral proof failed. Tag the change as logic, or make smaller neutral steps. |
| `pending` | Wait for phase 2 (`results`). |

`suggest_claims` gives draft claims for the uncovered changes. Check each draft before you send it with `claim`.
