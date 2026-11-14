# Semilla: the agent guide

Semilla shows what changed in each step of your work. You edit the code with your own tools. After each small change, you run one command with the intent. Semilla derives the facts from the diff. Each fact gets a category and a compare result. Semilla does not decide if a change is neutral: the reviewer reads the facts.

## The loop

```sh
# 1. edit the code for one intent
semilla step "<intent>"              # check and store the step
# 2. repeat for each intent
# 3. at the end
git add -A && git commit -m "<message>"   # `-A` adds each new file of the steps
semilla tag                          # the final check, the result line, the git note
```

- "Before" is the end of the last step (or `HEAD` for the first step). "After" is the working tree. Do not commit between steps. If `HEAD` moves, `step` prints a note with the fix: `git reset --soft <base>`, then one commit. The first step prints the base of the change set, for example "base `436a109`: no Semilla check (no checked commit before it)". The line is for the reviewer: do nothing for it.
- `semilla step` runs both phases (the syntax and the MIR compare) and prints the final result of the step: each fact, the claims, the notes, and the pair warnings. The step numbers start at 0.
- A step is stored only when its result is accepted. A rejected step stores nothing and has the exit code 1. The last line of a rejection is the exact command to run again.
- `semilla undo` removes the last stored step. The working tree keeps the changes. The output gives the exact `git restore` command for the files. Do not use `git checkout .`: it goes back to `HEAD`, not to the last step.
- `semilla tag` prints the result line: `result: accepted · 3 steps · 4 facts: 2 code different, 2 code equal · 0 claims`. The line has no score, and a count of zero does not show. After `tag`, the change set is closed. The next `semilla step` starts a new change set at `HEAD`.
- `semilla status` prints one line for each step, each problem that `semilla tag` rejects, and the result line. `--verbose` adds the facts. `semilla status COMMIT` shows a tagged commit. `semilla verify` checks the session and takes no commit.
- `semilla review [COMMIT] --md FILE` writes the report for the reviewer (`--full` adds the change tree, the public API changes, all flow views, and all sources). Write the file outside the repository.
- For a large project, `semilla step --fast` runs only phase 1 and runs phase 2 in the background. Then a fact with code has the compare result `pending`. Phase 2 runs at once when the step has a claim other than `rewrite-pattern`, or a possible rename or move. `semilla status --wait` waits for phase 2.
- The check builds the code in a sandbox with no network. If a dependency is not in the cargo cache, run `cargo fetch`. A changed manifest needs a new `Cargo.lock` in the same step: if the command says "`Cargo.lock` does not agree with the manifests", run `cargo update --workspace --offline`.
- The options of `semilla step`: `--group "TITLE"` (steps with the same title make one group), `--claim 'OP ANCHOR key=value …'` (an optional property claim), `--reason 'ANCHOR=TEXT'` (the reason for a fact in the spotlight), `--tree` (also print the change tree), `--fast`. An anchor is an item path (`ledger_core::fees::charge`, `ledger_core::Account.balance`) or a position `FILE:LINE[:COLUMN]`.
- The MCP server (`semilla mcp`) has the same `step`, `undo`, and `tag` tools. Each fact in its JSON has the fields `category` and `compare`.
- The replay (work first, steps later): run `semilla replay begin` (the work becomes the target; the working tree is `HEAD` again), `semilla replay plan` (it writes `.semilla/plan.json`), `semilla replay apply --full` (it runs all steps from the base), and `semilla replay end`. A plan step needs an intent, its takes (`FILE`, `FILE:START-END`, `FILE:LINE`, or an item path) or edits, and optional `claims` and `reasons`. `semilla step` does not work inside a replay.

## What Semilla derives

You write nothing for a fact. There is one fact for each source change. A fact line has the text of the change, then the category and the compare result. The category says what kind of change the text shows. It does not say if the change has an effect.

```text
  ◆ f1 derived: changed expression `self.cmp(rhs)` → `Ord::cmp(self, rhs)` at `semver::impls::<Prerelease as PartialOrd>::partial_cmp/tail/args[0]` [logic · code equal]
  ◆ f1 derived: changed literal `2` → `1` at `semver::identifier::<Identifier as Clone>::clone/S0/else/S3/init` [value · code different]
      affected bodies: semver::identifier::<Identifier as Clone>::clone
      note: this body uses code that a `cfg` attribute can change, which Semilla did not check (`semver::identifier::<Identifier as Clone>::clone`)
  ◆ f1 derived: renamed `semver::eval::matches_caret` → `semver::eval::matches_caret_op` [rename · code equal]
  ◆ f1 derived: visibility of `semver::eval::matches_caret`: `private` → `pub (crate)` [visibility · code equal]
```

| Category | The change |
|---|---|
| `rename` | a new name of an item, a member, a local, or a parameter, with the forced edits at each use |
| `move` | an item or a file in a different module |
| `visibility` | the visibility of an item or a member |
| `declaration` | a signature, generics, bounds, a type, the set or the order of members, a `derive` list, a qualifier |
| `added`, `removed` | a new or a removed item, member, `impl`, or test |
| `value` | a literal or the value of a constant |
| `logic` | statements, operands, calls |
| `control flow` | branches, loops, exits |
| `attribute`, `docs` | an attribute; a doc comment (a doc test change says "doc test" in its text) |
| `import`, `macro` | a `use`, `mod`, or `extern crate` line; a macro definition or a macro call |
| `manifest`, `file`, `build code` | a key of a manifest or of a cargo or rustup configuration; a file that is not Rust code; a change of build code |
| `indirect`, `text` | the compiled code of a body changes, and its text does not; changed source text with no other fact |

The compare result is an observation. It is not a verdict. It holds for the checked configurations only: `dev` and `release`, each with the default features, no default features, and all features.

| Compare result | Meaning |
|---|---|
| `code equal` | Each body in the scope of the fact has the same compiled code before and after, in each checked configuration. |
| `code different` | One body or more has different compiled code, or is new, or no longer exists. "affected bodies: …" lists the bodies. |
| `no compare` | Semilla made no compare. A note gives the cause: "note: no compare: no checked configuration compiles `shop_app::license::limit`". |
| `no code` | The fact has no compiled code: a doc comment, a manifest key, a file that no body reads. |
| `pending`, `not checked` | Phase 2 did not run yet (only with `--fast`). The command did not check the step (a reviewer command with `--no-build-code`). |

- "The same compiled code" means: the MIR is equal after the map of the renamed and moved names. Nothing more. A swap of two operands, a removed local, and a fold of a constant condition give `code different`.
- An equal compare never hides the text of an edit: each edit of a body is its own fact. A forced edit with equal code goes under its fact: "bodies with a changed call site and equal compiled code: …".
- Equal facts fold into one line: "3 site(s): changed literal `1` → `2`". The same type change in many places is one fact: "type `u32` → `u64` in 5 places: …". A fact that needs a reason does not fold. The fact IDs start at `f1` in each step.
- A mismatch mark shows when a fact of the category `rename`, `move`, `visibility`, `import`, `docs`, or `attribute` has `code different`: "! the compiled code changes: <shop_core::Account as std::fmt::Debug>::fmt". The mark asks for no reason. The reviewer reads it first.
- More fact texts: "public API: a different crate can no longer use `shop_core::over` [move · no code]"; "doc test of `shop_core::cart::Cart::sum_below` changed [docs · no compare]" (Semilla does not compile a doc test); "added macro call `same!(Cart)` (makes: `impl PartialEq for Cart`) [added · code different]".
- A note under a fact (`note: …`) gives a limit of its compare result. A note asks for no reason, and you do nothing for it. Example of a condition that Semilla did not check: "note: this body uses `cfg!(unix)`, which Semilla did not check (`shop_core::percent`)". Example of configurations that do not agree: "note: the compiled code is equal in `dev`; different in `release`" (the compare result is `code different`).
- An equal compare that uses two build conditions or more needs each joint state. Semilla adds the missing configuration and checks the step again: "Semilla added the configurations `dev, features fast`, `release, features fast` for a joint state". The limit is 4 added feature selections. If a state is still missing, a note names it and gives the `feature-sets` entry for `[workspace.metadata.semilla]`. A step that breaks one configuration is rejected: the message names the configuration and shows the compiler error.
- Renames and moves: do nothing. Semilla derives a rename or a move when the step has no other change. Then the step has one fact, and the forced edits (the uses, the `use` lines, the `mod` line) go under it: "moved `shop_core::over` → `shop_app::over`, with 1 `use` line(s) [move · code equal]". A doc change in the same step is its own fact. A derived `Debug` body shows the new name of a type or a field: then the fact is `[rename · code different]` with the mismatch mark. This is correct: do nothing.
- If the step has a second change, the two items show as removed and added, and a note gives the cause: "… show(s) as removed and added: the pair has a difference: changed operator `ver.minor > minor` → `ver.minor >= minor` at …. Semilla derives a rename only in a step with no other change". The note does not reject the step. One change of identity in its own step gives the reviewer one fact in place of four. A pair warning (`! pair: …`) shows a removed and an added item with a similar text. It does not reject the step. If they are two different items, run the printed `semilla pair refuse` command with a reason.

## The optional claims

A claim says a property that the diff does not show. If it is false, the step is rejected. Each example below runs in the tests (`crates/semilla/tests/e2e.rs`).

| Claim | Example | Result |
|---|---|---|
| `tested-by` | `--claim 'tested-by shop_core::cart::sum_wide test=wide'` | `verified (mir)`: the test calls the function on each path to its normal return. `verified (partial)`: the call is under a condition, in a loop, or after a possible exit. The scope line gives the cause |
| `no-panic` | `--claim 'no-panic shop_core::cart::share'` on `total / n` | `refuted: implicit panic sites: division by zero …` |
| `no-unsafe`, `no-io` | `--claim 'no-io shop_core::cart::sum_wide'` | `verified (mir)` |
| `calls-only` | `--claim 'calls-only shop_core::cart::save calls=["std::fs"]'` | `verified (mir)`: each call to another crate is under a path in `calls`. Calls to `core` and `alloc` are always allowed |
| `rewrite-pattern` | `--claim 'rewrite-pattern shop_core::cart::show from="$c.total($m)" to="$c.total($m).min(9)"'` | `verified`: each site in the anchor is rewritten, and no site is missed |

- The claims `rename` and `move-item` are gone. The command says: "the claim `move-item` is gone: Semilla derives each rename and each move …". `semilla ops` lists the claims. The anchor of a method of a trait impl is `<Type as Trait>::method`: `--claim 'no-unsafe shop_core::cart::<Weight as core::convert::From<Kilos>>::from'`. If a name matches no item, the message lists the nearest names.
- An entry of `calls` matches full path segments: `std::fs` matches `std::fs::write`. If entries are missing, the claim is refuted, and the `run again:` line has the complete list. Use it only if the function must make these calls.
- Semilla does not run tests, and it does not check what a test asserts. It does not follow a call through a trait object or a function pointer. Do not change an honest test to get `verified`. A property claim examines the local code that can run from the function. The result gives the scope: "examined: 3 local bodies (1 through trait impls, 0 drop bodies); not examined: …". Something "not examined" gives `verified (partial)`.

## When a reason is necessary

A fact in the spotlight needs `--reason 'ANCHOR=TEXT'`. The reviewer reads the reason; Semilla does not check it. A compare result never asks for a reason. For an edit in an item, the anchor is the path of the item: `--reason 'ledger_core::fees::charge=the limit is new'`. For each other fact, the anchor is the one that the rejection prints: `--reason 'Cargo.toml:profile.dev.overflow-checks=the benchmark needs it'`, or `--reason 'ci.sh:=the CI runs it'`. A reason with no text is not accepted. Give the reason in the first call when the step has one of these:

- an attribute outside the allow list (`doc`, `allow`, `warn`, `deny`, `forbid`, `expect`, `must_use`, `inline`, `cold`, `deprecated`, `derive`, `rustfmt::`, `clippy::`, `diagnostic::`). So `ignore`, `should_panic`, `cfg`, `path`, `repr`, `no_mangle`, and each proc-macro attribute need a reason. A new item also allows `#[test]` and `#[cfg(test)]`;
- a manifest key outside the allow list (`package.version`, the descriptive `package` keys, a version change of a registry dependency), and each key of a cargo or rustup configuration;
- a file outside the allow list (`*.md`, `*.txt`, `LICENSE*`, `COPYING*`, `NOTICE*`, `CHANGELOG*`, `AUTHORS*`, images), a file mode, a symbolic link, a submodule;
- a build script, and a build input that changes bodies with no edit of their text (`--reason 'crates/core/build.rs:=<why>'`); a file that the code reads at build time (`include_str!`, `include_bytes!`, `include!`), when the use is new or no body changes with the file; a changed `macro_rules!` definition that changes the bodies of its calls: `--reason 'k::tests::t!=<why>'`;
- inactive code: "inactive code: no checked configuration compiles the changed code (…), so Semilla did not compare the edit". The fact has `no compare`;
- a changed literal with a new character outside ASCII (`"µs"` → `"μs"`); an `unsafe` or `extern` qualifier; an order change in a type with `#[repr]` or an order derive; a removed argument with a side effect;
- a hand-written trait impl that replaces a derive; a new or removed `Drop`, `Deref`, or `DerefMut` impl; a rename of a type with a derive other than `Debug` (the anchor is the old name);
- an indirect change with no known cause, or code that Semilla does not see (generated code).

## The results and the actions

| Result | Action |
|---|---|
| `accepted` | Nothing. The result is final. |
| `rejected … needs a reason` | Run the printed command with the `--reason`. |
| `refuted` | The claim is false. Run the printed command (without the claim), or fix the code. |
| `malformed` | The anchor or an argument is wrong. The message names it. A `tested-by` test must have `#[test]` and no `#[ignore]`. |
| `the claim … is gone` | Run the command again without the claim. |
| `note: …` or `! the compiled code changes: …` | Nothing. The reviewer reads the line. |
| `error: … no change since the last step` | Edit the code first. |
| `! build code changed in step N: …` | Each result `code equal` and each verified claim from step N on has the mark `(with the build code of step N)`. Give the reason. If possible, put the build code change in its own step. |
| `! the build ran with no sandbox: …` | A limit of the build sandbox is missing on this machine. The result stays. Do nothing for it. |
| `tag` rejects | "result: rejected" is the first line, then each reason with its step, then the exact commands of the fix. Run them in order. Examples: `commit … has no step`, `the commit HEAD does not have the changes of the last step` (run `git add -A`, `git commit --amend --no-edit`, `semilla tag`), `an edit after the last step has no step`. |
| `the note does not agree with the step refs` | Someone changed the git note or the step refs after `tag`. Do the change set again. |
