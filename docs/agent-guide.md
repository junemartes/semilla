# Semilla: the agent guide

Semilla checks what changed in each step of your work. You edit the code with your own tools. After each small change, you run one command with the intent. Semilla derives the facts from the diff and proves what it can.

## The loop

```sh
# 1. edit the code for one intent
semilla step "<intent>"              # check and store the step
# 2. repeat for each intent
# 3. at the end
git add -A && git commit -m "<message>"   # `-A` adds each new file of the steps
semilla tag                          # the final check, the result line, the git note
```

- "Before" is the end of the last step (or `HEAD` for the first step). "After" is the working tree. Do not commit between steps. If `HEAD` moves, `step` prints a note with the fix: `git reset --soft <base>`, then one commit.
- `semilla step` runs both phases (the syntax and the MIR proofs) and prints the final result of the step: each fact with its class, the claims, the notes, and the pair warnings. `tag` adds no new result for a step.
- A step is stored only when its result is accepted. A rejected step stores nothing: fix the cause and run the command again. The last line of a rejection is the exact command to run again.
- The step numbers start at 0. A rejected result has the exit code 1. `semilla verify` checks the session and takes no commit: for a commit, use `semilla status HEAD`.
- `semilla undo` removes the last stored step. The working tree keeps the changes. The output gives the exact `git restore` command that also restores the files of the step before. Do not use `git checkout .`: it goes back to `HEAD`, not to the last step.
- The first step prints the base of the change set: "base `2f12d95`: it has a Semilla check", "base `7adafc0`: no Semilla check (no checked commit before it)", or "! 1 commit before this change set has no Semilla check: …". The line tells the reviewer that Semilla did not check the code before the change set. Do nothing for it.
- `semilla tag` prints the result line: `result: accepted · 7 steps · 23 facts: 3 proved neutral, 20 to read (0 with a spotlight) · 2 claims: 2 verified`. The result has no score: the reviewer reads the facts "to read".
- After `tag`, the change set is closed. Then `semilla status` says "no open change set; the last tagged commit is …". If `HEAD` is another commit, the next line says if `HEAD` has a check: "HEAD `76ee28f` has no Semilla check". The next `semilla step` starts a new change set at `HEAD`, also when Semilla rejects that step.
- `semilla status` prints one line for each step, each line that makes the result not accepted, and the result line. It also prints each problem of the present state that `semilla tag` rejects (`✗ `semilla tag` rejects this state: …`): more than one commit after the base, or an edit with no step in a commit or in the working tree. Then the result word is `rejected`. `semilla status --verbose` adds the facts. `semilla status COMMIT` shows a tagged commit. It waits for the full check (at most 600 seconds); `--no-wait` prints the present state. Right after `tag`, it prints the result at once.
- `semilla review [COMMIT] --md FILE` writes the short report for the reviewer: the result, the groups with their facts, and the flow views of the changed logic. `--full` adds the change tree, the public API changes, all flow views, and all sources. Write the file outside the repository.
- For a large project, `semilla step --fast` runs only phase 1 and runs phase 2 in the background. Then each body fact and each file fact has the class `pending`. Phase 2 runs at once when the step has `--class`, a claim other than `rewrite-pattern`, or a possible rename or move.
- Each later `step` prints the phase 2 state of each `--fast` step: "step 1: phase 2 accepted", "step 1: phase 2 rejected: …" with the commands of the fix, or "step 1: phase 2 runs (the result: `semilla status --wait`)". `semilla status --wait` waits for phase 2 and prints it.
- The check builds the code in a sandbox: the build can write only its build folder, and it does not download. The build reads only the files of the tree, the toolchain, and the cargo registry. It has no network, and it gets no variable of your environment. If a dependency is not in the cargo cache of the machine, the error says so: run `cargo fetch` in the repository, then run the command again. A code change is not necessary. If the build fails with "Permission denied", the error gives the possible causes (a `Cargo.lock` that does not agree with a manifest, a build script that writes outside `OUT_DIR`).
- A changed manifest (a new version, a new dependency) needs a new `Cargo.lock` in the same step. If the lock file does not agree, the command stops with one line: "`Cargo.lock` does not agree with the manifests in the tree after step N". Run `cargo update --workspace --offline`, then run the command again. Semilla does not write the lock file.
- The MCP server (`semilla mcp`) has the same `step`, `undo`, and `tag` tools, with the options as JSON fields.

Options of `semilla step`:

| Option | Use |
|---|---|
| `--group "TITLE"` | Steps with the same title make one group. A step with no group is its own group, with the intent as the title. |
| `--claim 'OP ANCHOR key=value …'` | An optional claim (see below). |
| `--reason 'ANCHOR=TEXT'` | The reason for a fact in the spotlight. |
| `--class neutral\|logic\|control_flow\|other` | A declared class. The step is rejected if the computed class differs. |
| `--tree`, `--fast` | Also print the change tree. Phase 1 now and phase 2 in the background (a large project). |

An anchor is an item path (`ledger_core::fees::charge`, `ledger_core::Account.balance`) or a position `FILE:LINE[:COLUMN]`. For `rename` and `move-item`, a position is in the code before the step.

## What Semilla derives

You write nothing for these. Each one is a fact (`◆ f1 derived: …`) with a class:

- Each statement or expression edit, with the words of the change: "added early exit `if account.frozen { return … }`", "removed `if`, kept the `else` branch", "changed literal `1` → `2`", "changed operator `amount > DAILY_LIMIT` → `amount >= DAILY_LIMIT` (effect hint: widen, example: amount = 50000)". The effect is a hint, not a proof: it shows only for a change in a condition (for example of an `if`), and only for `narrow`, `widen`, and `negate`. A moved node shows its two places: "moved let: `let n = v.len();` from `S0` to `S1` in `f`". An edit with no exact shape gives "changed NODE: `A` → `B`".
- Added and removed items, docs, attributes, and visibility. A code block in a doc comment is a doc test, and `cargo test` runs it: "doc test of `f` added (block attributes: `no_run`) [logic]", with the diff of the doc lines.
- Type-level changes: fields, variants, parameters, return types, generics, trait impls. A fact owns its forced edits: "4 site(s): add the argument `1` (in …)". Under the fact, "affected bodies: …" lists the bodies whose MIR changes, and "bodies with a changed call site and no change of behavior (proved): …" lists the bodies with a forced edit and an equal MIR. A list shows 6 bodies, then "and N more"; `semilla review --full` shows each body. A changed order of generic parameters adds "; generic arguments go by position: N site(s)", with each body that names the item with explicit arguments. A renamed parameter with the same type and position is one fact: "parameter `a` of `f` renamed to `b`". A new type is one fact ("added `T`"): its fields and variants are not facts of their own.
- Renames and moves: "renamed `A` → `B` (proved N1: rename-map)", "moved `A` → `B` (proved N1)". See below.
- A member order change of a struct or an enum: "member order of `T`: …". Its class is always `logic`: the order changes the drop order, the derive output, and the discriminant values of an enum. A body that reads the fields, builds the value, or matches on the variants by name stays equal. The bodies that depend on the order go under the fact: the derived bodies (`Debug`, `PartialEq`, `Hash`, …), a constant of the type, a cast `as` of the discriminant, a transmute. The fact needs a reason (`--reason 'T=…'`) for a type with `#[repr]`, `PartialOrd`, `Ord`, or `Hash`, and for an enum whose discriminant a cast reads.
- A body edit next to a member order change or a new member is never "neutral" by position: Semilla compares the fields and the variants by name. A changed field read (`p.a` → `p.b`) is a `logic` fact.
- Const values: "value of `BASE_FEE` changed: 25 → 250", with the bodies that use the const.
- Manifest keys (`Cargo.toml: profile.dev.overflow-checks added (false)`) and other files.
- A file that a crate reads at build time (`include_str!`, `include_bytes!`), also a `*.md`, a `*.txt`, or an image: "crates/core/src/rate.txt: changed [logic]", with the bodies that read the file under "affected bodies". No reason is necessary. A file that `include!` reads is code: it has the same form, and it needs a reason (see "When a reason is necessary"). If no body changes (a `#[doc = include_str!(…)]` file, or code that no checked configuration compiles), the fact has the class `other` and needs a reason. A file that only a build script reads shows as an indirect change of the bodies that the build script changes, and it needs a reason.
- Indirect changes: a body whose MIR changes with no text edit goes under its cause (a changed type, const, macro, included file, new method, or build input).

Equal facts fold into one line, with the bodies: "6 site(s): changed expression `a.balance` → `a.cents`", "4 bodies: neutral rewrite (proved N0)". The same type change in many places is one fact: "type `u32` → `u64` in 8 places: `Account.id`, parameter `id` of `find`, …". A fact with another text or another class does not fold, and a fact that needs a reason does not fold. The fact IDs start at `f1` in each step. The claim IDs (`c1`, `c2`, …) count the stored claims of the change set.

The classes:

| Class | Meaning |
|---|---|
| `neutral (proved)` | The MIR of the body is equal (N0 to N4). The edits of the body fold into one fact: "`f`: neutral rewrite (proved N2)". |
| `logic` | The statements or operands change. |
| `control_flow` | The branches, loops, or exits change. |
| `other` | No MIR: docs, doc attributes, manifests, files. A doc change with no code block is always `other`. |
| `pending` | Phase 2 did not run yet (only with `--fast`): a body fact, and also a file fact (a build can read each file). Never read it as neutral. |
| `not checked` | The command did not check the step: a step from a change of build code, in a command with `--no-build-code`. Only a reviewer sees it. Never read it as neutral. |

## The optional claims

A claim says something that the diff does not show. If it is false, the step is rejected. Each example below runs in the tests (`crates/semilla/tests/e2e.rs`).

| Claim | Example | Result |
|---|---|---|
| `tested-by` | `--claim 'tested-by shop_core::cart::sum_wide test=wide'` | `verified (mir)`: the test calls the function on each path from its start to its normal return, and no end of the process is before the call. `verified (partial)`: the call is under a condition, in a loop, after an early return or a `?`, or after a possible `std::process::exit`. The scope line gives the cause |
| `no-panic` | `--claim 'no-panic shop_core::cart::share'` on `total / n` | `refuted: implicit panic sites: division by zero …` |
| `no-unsafe`, `no-io` | `--claim 'no-io shop_core::cart::sum_wide'` | `verified (mir)` |
| `calls-only` | `--claim 'calls-only shop_core::cart::save calls=["std::fs"]'` | `verified (mir)`: each call to another crate is under a path in `calls`. Calls to `core` and `alloc` are always allowed |
| `rewrite-pattern` | `--claim 'rewrite-pattern shop_core::cart::show from="$c.total($m)" to="$c.total($m).min(9)"'` | `verified`: each site in the anchor is rewritten, and no site is missed |
| `rename` | `--claim 'rename shop_core::cart::show to=display'` | `verified (N0)`: no other body uses the name, and each body is equal |
| `move-item` | `--claim 'move-item shop_core::over to=shop_app::over'` | `verified (N1: rename-map)`: a body uses the name, and each body is equal with the name map |

The anchor of a method of a trait impl is `<Type as Trait>::method`. Write it as in the source: Semilla accepts a full trait path and the generic arguments of the trait. Example: `--claim 'no-unsafe shop_core::cart::<Weight as core::convert::From<Kilos>>::from'`. A reason key for an impl is `impl <Type as Trait>`: `--reason 'shop_core::cart::impl <Weight as From<Grams>>=the scale gives grams'`. Two impls of one trait for one type have the names `…#0` and `…#1`: the generic arguments select the impl. If a name matches no item, the message lists the nearest names.

Each other op is retired as a claim: the command says so. `semilla ops` lists the claims. An empty value (`test=`, `to=`, an empty anchor) is `malformed`.

An entry of `calls` matches full path segments: `std::fs` matches `std::fs::write`, and `st` or `std::f` matches nothing. For `<T as Trait>::method`, the paths of `T` and of `Trait` must be under an entry. An empty entry is `malformed`.

A call to a function that `core` or `alloc` defines is always allowed. The crate that defines the function decides: `std::iter::Iterator::sum` is a function of `core`. So write only the other paths. `calls=[]` claims that the function has no call to another crate, apart from `core` and `alloc`.

If entries are missing, the claim is refuted. Then the `run again:` line has the claim with the complete list: your entries, and the shortest module paths that cover the other calls. The line before it names the new entries. This command changes your claim. Use it only if the function must make these calls.

### Renames and moves: do nothing

Semilla derives a rename or a move when the step has no other change. It finds the pairs (a removed and an added entity of the same kind and a similar text), tries the name map, and proves it with the MIR. Then the step has one fact, "renamed `A` → `B` (proved N1: rename-map)", and the forced edits (the uses, the private `use` lines, the `mod` line of a moved file) fold under it. This applies to an item, a field, a variant, and a field of a variant. A derived `Debug` body that shows the new name does not stop the proof: the fact says "renamed `A` → `B` (proved N1: rename-map; neutral, but 1 derived body shows the name (Debug for `T`))".

So put one change of identity in its own step, with no other change: no body change, no new item. A move has these parts, all in one step: the item text (with the `impl` blocks of a moved type), the `mod` line and the file of a new module, each `use` line that the move needs, and a wider visibility of the moved item. The visibility is its own fact with no reason: "visibility of `B`: `private` → `pub (crate)`". A visibility change that changes the public API of a library is a fact to read with the class `other`, in a move and in each other step: "visibility of `B`: `private` → `pub` (the public API changes: a different crate can now use the item)". A proved rename or move that changes a path of the public API gets one more fact to read (class `other`, no reason): "public API: `A` is now `B` (a different crate can no longer use the old path)". Two more changes are allowed in the same step, because the MIR shows that they change nothing:

- a doc change (for example the old name in a doc comment, a new `///` comment on the moved item, or a `//!` comment in the file of a new module). It is its own fact with the class `other`. Each other attribute stops the proof (`#![allow(…)]`, `#[doc(hidden)]`, `#[cfg(…)]`);
- a rename of a local or of a parameter, when the body stays equal. It is its own neutral fact: "parameter `balance` of `f` renamed to `cents`".

So `sed 's/\bbalance\b/cents/g'` on the files of a field rename gives one proved rename, with no claim. The `sed` command also changes doc text, and each such doc change is a fact that the reviewer reads: correct the doc text by hand before the step. A changed code block in a doc comment is a doc test fact with the class `logic`.

If the step has other changes, the two items show as removed and added, each edit inside the new item is its own fact ("`pct` differs from the removed `percent`: changed literal `100` → `10`"), and the note names the cause: "the pair has a difference: changed literal `200` → `170`" (the two items are not the same code), or the other change of the step with "Put the rename in its own step to get a proof". With no other change, the note says what stops the proof: "no proof of the rename (`log!` in `f` (Semilla cannot see into it))". The note does not reject the step. To get the proof, run `semilla undo`, keep only the rename in the working tree, and make two steps.

### Pair warnings

A pair warning (`! pair: …`) shows a removed and an added item with a similar text. It does not reject the step. It tells the reviewer that the two items can be one changed item. If they are one item and the step has a second change, put the rename or the move in its own step. If they are two different items, run the printed `semilla pair refuse` command with a reason. A pair that a rename proof covers gives no warning.

A move can change the meaning of a name. Example: the body of `over_limit` moves to a module where `DAILY_LIMIT` is a different constant. Then the warning names the path and the two items: "`A` (removed) and `B` (added) have the same text, but a name in the text points to a different item: `DAILY_LIMIT` is `core::DAILY_LIMIT` in the removed item and `cli::DAILY_LIMIT` in the added item". The move is not neutral. Import the old item in the new module, or keep the facts "removed" and "added" and give the reason.

Give an explicit `rename` or `move-item` claim only when the note says that Semilla derives nothing: more than 4 removed or added entities of one kind in one owner, or more than one name map that passes.

## When a reason is necessary

A fact in the spotlight needs `--reason 'ANCHOR=TEXT'`. The reviewer reads the reason; Semilla does not check it. Give the reason in the first call when the step has one of these:

- an attribute outside the allow list, on a changed item and on a new item (the allow list: `doc`, `allow`, `warn`, `deny`, `forbid`, `expect`, `must_use`, `inline`, `cold`, `deprecated`, `derive`, `rustfmt::`, `clippy::`, `diagnostic::`). So `ignore`, `should_panic`, `cfg`, `path`, `repr`, `no_mangle`, each proc-macro attribute, and each unknown attribute need a reason. A new item also allows `#[test]` and exactly `#[cfg(test)]`; a `#[test]` that you add to or remove from an existing item needs a reason;
- a manifest key outside the allow list (the allow list: `package.version`, the descriptive `package` keys, and a version change of an existing registry dependency). So `profile`, `features`, a new dependency, `workspace.members`, and `workspace.default-members` need a reason;
- each key of a cargo or rustup configuration in each folder: each file under `.cargo/` (also the legacy name `.cargo/config`), `rust-toolchain.toml`, and the legacy `rust-toolchain` (one line, for example `beta`, is the key `toolchain.channel`);
- a file outside the allow list of files that no tool reads to build or to test (the allow list: `*.md`, `*.txt`, `LICENSE*`, `COPYING*`, `NOTICE*`, `CHANGELOG*`, `AUTHORS*`, and images). So a CI file, a script, `clippy.toml`, `rustfmt.toml`, `.env`, `.gitignore`, `.gitattributes`, a linker script, and an unknown file need a reason. A doc file that a `#[doc = include_str!(…)]` reads needs a reason too (its doc tests run); for each other included file, see "What Semilla derives". The report shows the content change of each file (a diff, or "binary, N bytes");
- a build script. It is the file that `package.build` names (default: `build.rs`), with each name and each suffix, also `NOTES.md`. Each change of it has the spotlight "build script";
- a file mode, a symbolic link, a submodule, a Rust file that the crates do not parse (a target file of a package that is not a workspace member, a file with each name that `include!` reads: `--reason 'src/NOTICE.txt:=<why>'`);
- a build input that changes bodies with no edit of their text (a build script, each manifest key, also `package.version` and `package.description` through `env!`). The bodies go under the build input, and one reason on it is sufficient: `--reason 'crates/core/build.rs:=<why>'`. If the text says "one of these causes", each cause needs its reason;
- a use of the build configuration in a changed body: `cfg!`, a `cfg` attribute, `option_env!`, or a macro of another crate (Semilla cannot see into it). Semilla proves `neutral` in each checked configuration: `dev` and `release`, each with the default features, no default features, and all features. A rewrite with a proof in only some of the configurations is not `neutral`, but it needs no reason: the fact keeps its class and gets the note "proved neutral in `dev`, …; not proved in `release`, …". A step that breaks one configuration is rejected: the message names the configuration and shows the compiler error. If a configuration does not build with each target before your step, Semilla checks its libraries and binaries only. If these do not build, `semilla step` prints one line "configuration … is not checked". A proof that uses two build conditions or more needs each joint state in the list. If a state is missing, Semilla adds the configuration itself and checks the step again. `semilla step` prints one line: "Semilla added the configuration `dev, features fast` for a proof". You do nothing. The first step with a new configuration is slower, because Semilla builds it. The limit is 4 added feature selections for one step. Only if a state is still missing, the output says: "Not checked together: `feature = "fast"` on with `feature = "slow"` off. Add `feature-sets = [["fast"]]` to `[workspace.metadata.semilla]`". Then add the entry in a step of its own, or give a reason. A fact with no proof and no spotlight can have the note "no proof: `…` uses `feature = "fa"`, and Semilla did not check that condition": the note gives the cause, and you do nothing. Semilla checks only the profiles `dev` and `release` of the repository: a custom profile is not checked, and a compiler flag that is not a `cfg` condition (for example the overflow checks) follows the profile. For a condition that these configurations do not cover (`target_os`, a flag that is on in each build, `test` in a file under `tests/`), a change of a use of the build configuration is never neutral. A new use of an item that the build configuration selects or fills is such a use: an item under `cfg` (also through its module or `impl`), a static with a `cfg!` or `usize` value, a function with a `cfg` attribute in its body, a platform type alias (`c_long`). "Uses: …" names the item. A use is exact: the compiler resolves your code to the item, or a name of your code can mean the item by its module or its type. A name that a macro puts into your code counts too (`write!` gives a call of `write_fmt`). A new method call is a use if a different method with that name can win in a different configuration: "the call means `Other::dig` now; `Sup::dig` wins if `Coin: Sup` holds in a different configuration". Write the call as a path (`Other::dig(&t)`) to keep one meaning. The word alone is not a use (`Default::default()` does not use a module `default` under `cfg`). A body that uses such an item only in code with no change needs no reason, but a rewrite in that body is not `neutral (proved)`. One exemption: a call of a function under `cfg` with the same signature in each configuration, from its own module. A `cfg_attr` with only a lint, `doc`, or `inline` content is not a use. The same applies to a body that uses a type of a crate outside the workspace (`serde_json::Value`): the edits are facts to read;
- an edit in code that no checked configuration compiles (a constant in a module under `#[cfg(windows)]`): the edit has no proof, also if only a constant or a `use` line changes;
- a changed literal with a new character outside ASCII (`"µs"` → `"μs"`): the fact shows the escape form after the literal (`"\u{3bc}s"`) and needs a reason. A new literal in new code needs no reason;
- an `unsafe` or `extern` qualifier, an order change in a type with `#[repr]` or an order derive, a removed argument with a side effect;
- a hand-written trait impl that replaces a derive, a new or removed `Drop`, `Deref`, or `DerefMut` impl;
- a rename of a type with a derive other than `Debug` (serde, `Hash`, …): the derived output changes with the name. The anchor is the old name;
- an indirect change with no known cause, or code that Semilla does not see (generated code).

For an edit in an item, the anchor is the path of the item (a function, a method, a constant, a static, a type, a trait): `--reason 'ledger_core::fees::charge=the limit is new'` covers each spotlight in `charge`. A module path or a crate name does not cover the items in it. For each other fact, the anchor is the one that the rejection prints: `--reason 'Cargo.toml:profile.dev.overflow-checks=the benchmark needs it'`, or `--reason 'ci.sh:=the CI runs it'` (a file: the path and `:`). A reason with no text, and a reason that belongs to no fact of the step, are not accepted: the output says so.

## The results and the actions

| Result | Action |
|---|---|
| `accepted` | Nothing. The result is final. |
| `rejected … needs a reason` | Run the printed command with the `--reason`. |
| `unproved` on `rename` or `move-item` | Run the printed command (without the claim), or put the identity change in its own step. |
| `note: … Put the rename in its own step` | Nothing, or `undo` and split the step to get the proof. |
| `rejected … declares the class` | The error names the facts that give the class. Run the printed command, or fix the code. |
| `refuted` | The claim is false. Run the printed command (without the claim), or fix the code. |
| `malformed` | The anchor or an argument is wrong. The message names it. A `tested-by` test must have `#[test]` and no `#[ignore]`. |
| `error: … no change since the last step` | Edit the code first. |
| `tag` rejects | "result: rejected" is the first line, then each reason with its step, then the exact commands of the fix. For a step that is not the last step: run `semilla undo` K times (this removes the step and each later step; the files stay). Then run the printed line of each removed step, in order: `git restore --source=<tree> --worktree -- . && semilla step …`. Then do what the last printed line says (`semilla tag` again; `git commit --amend --no-edit` first if a file changed). |
| `! the build ran with no sandbox: … are missing (…)` | A limit of the build sandbox is missing on this machine (the line names each one). The result stays, and the reviewer sees the line. A reviewer command does not run changed build code on such a machine. Do nothing for it. |
| `! the build left a file in the records folder …` | A program of the build wrote a file into the records folder. Semilla does not use the file. The reviewer sees the line. Remove that write from the build code. |
| `The file … is outside the build folder` | The code names a file with an absolute path. The build reads only the files of the tree. Put the file in the tree, and name it with a relative path. |
| `! build code changed in step N: …` | The step changes build code (a build script, a proc macro crate, a host package, a cargo or rustup configuration file). Each command runs it in the sandbox, and each proof from step N on has the mark `(with the build code of step N)`. Give the reason for the change. If the tree compiles without the other changes, put the build code change in its own step: then the mark starts at that step, and the reviewer reads the build code alone. Do nothing more for the line. |
| `tag`: `commit … has no step` | You made a commit with no step. Run the printed commands: `git reset --soft <commit>`, `semilla step "<intent>"`, `git commit`, `semilla tag`. |
| `tag`: `the commit HEAD does not have the changes of the last step` | The commit lacks a file of a step (`git commit -am` does not add a new file). The message names the files. Run `git add -A`, then `git commit --amend --no-edit`, then `semilla tag`. |
| `tag`: `an edit after the last step has no step` | Run `semilla step "<intent>"` for the edit, then `git commit --amend --no-edit`, then `semilla tag`. |
| `the note does not agree with the step refs` | Someone changed the git note or the step refs after `tag`. The result is not accepted. Do the change set again. |

## What Semilla does not do

- It does not run tests. `tested-by` is `verified (mir)` if each path of the test from its start to its normal return calls the function (directly or through local functions). A reachable call that is not on each path gives `verified (partial)`. A call after a possible end of the process (`std::process::exit`, `abort`), or after a possible panic in a `#[should_panic]` test, gives `verified (partial)` too. `partial` is the correct result for a test with the call in a loop: the step is accepted, and the reviewer reads the scope. Do not change an honest test to get `verified`. A call in code that cannot run (`if false { … }`) refutes the claim. Semilla does not check what the test asserts, and it does not follow a call through a trait object or a function pointer.
- A property claim (`no-panic`, `no-unsafe`, `no-io`, `calls-only`) examines the local code that can run from the function: its local callees, each function that the body uses as a value, each closure, each `Drop` body of a value that the code drops, and each trait impl of a local type that goes to generic code of another crate or to a trait object (for example `Display::fmt` through `to_string`). The result gives the scope: "examined: 3 local bodies (1 through trait impls, 0 drop bodies); not examined: …".
- The result is `verified (partial)` when something is "not examined": a call through a function pointer or a trait object, a call to another crate (`no-panic`), a foreign function, inline assembly, a parameter that holds a trait object or a function pointer, a `#[global_allocator]`, or a `#[panic_handler]`. The reviewer reads a partial result.
- `no-io` uses the crate that defines each called function. `core` and `alloc` do no I/O. `std` does no I/O only in the modules of an allow list (`std::collections`, `std::hash`, `std::num`, `std::sync`, …; see `docs/design.md` 25.1); each other `std` function is I/O and refutes the claim. A function of another crate is "not examined". `no-unsafe` means: no `unsafe` in the local code that can run. `no-panic` checks the explicit panics, the MIR asserts, and the calls to the panic functions; a division by a constant that is not zero is not a panic site.

## The replay (work first, steps later)

Run `semilla replay begin` (the work becomes the target; the working tree is `HEAD` again), `semilla replay plan` (it writes `.semilla/plan.json`: the steps with their takes and optional claims), `semilla replay apply --full` (it runs all steps from the base, with phase 2), and `semilla replay end`. A plan step needs an intent, its takes (`FILE`, `FILE:START-END`, `FILE:LINE`, or an item path) or edits, and optional `claims` and `reasons`. `semilla step` does not work inside a replay.
