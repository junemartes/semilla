# Semilla: the agent guide

Semilla gives the reviewer a semantic diff of your work: a fact for each change, with as much information as possible, so that a human reviews the code fast. Each changed line of the diff is in a fact. You edit the code with your own tools. Semilla derives the facts from the syntax tree of the code before and after a change. Each fact gets a category. Semilla gives no verdict for a change: the reviewer reads the facts. Your intent, your facts, and your reasons are more information for the reviewer. Each step has a build check: Semilla runs `cargo check` on the code after the step (see "The build check"). The compiled check is an option (see "The compiled check").

## The two workflows

Choose one workflow for a commit. The two give the same facts for the same change.

1. **Steps while you work** ("The loop" below): after each small change, run `semilla step "<intent>"`. At the end, commit and run `semilla tag`. Small steps with one intent are the easiest to review. Use this workflow when you can.
2. **A tag after the commit** ("A tag after the commit" below): commit as usual, then run `semilla tag <rev>` (one commit) or `semilla tag A..B` (each commit of the first-parent chain). Each commit gets one step. Use this workflow when you did not work in steps, or for commits that are already there.

## The loop

```sh
# 1. edit the code for one intent
semilla step "<intent>"              # check and store the step
#    optional: state your facts with --same and --change (see "The facts that you state")
# 2. repeat for each intent
# 3. at the end
git add -A && git commit -m "<message>"   # `-A` adds each new file of the steps
semilla tag                          # the final check, the result line, the git note
```

- "Before" is the end of the last step (or `HEAD` for the first step). "After" is the working tree. In this workflow, do not commit between steps. If `HEAD` moves, `step` prints a note with the fix: `git reset --soft <base>`, then one commit. The first step prints the base of the change set, for example "base `436a109`: no Semilla check (no checked commit before it)". The line is for the reviewer: do nothing for it.
- `semilla step` compares the two syntax trees and prints the final result of the step: each fact, the claims, the notes, and the pair warnings. This part takes less than one second. Then the build check runs. A tree that does not build is rejected with the errors of the compiler. The step numbers start at 0.
- A step is stored only when its result is accepted. A rejected step stores nothing and has the exit code 1. The last line of a rejection is the exact command to run again.
- `semilla undo` removes the last stored step. The working tree keeps the changes. The output gives the exact `git restore` command for the files. Do not use `git checkout .`: it goes back to `HEAD`, not to the last step.
- `semilla tag` prints the result line: `result: accepted · 3 steps · 4 facts · 0 claims`. The line has no score, and a count of zero does not show. After `tag`, the change set is closed. The next `semilla step` starts a new change set at `HEAD`.
- `semilla status` prints one line for each step, each problem that `semilla tag` rejects, and the result line. `--verbose` adds the facts. `semilla status COMMIT` shows a tagged commit. `semilla verify` checks the session and takes no commit.
- `semilla review [COMMIT] --md FILE` writes the report for the reviewer (`--full` adds the change tree, the public API changes, all flow views, and all sources). Write the file outside the repository. The review of a change set with no compiled step starts no build. The review shows the stored result of the build check of each step. It does not run the build check again.
- The options of `semilla step`: `--group "TITLE"` (steps with the same title make one group; Semilla derives the kind of a group from its facts: `rename`, `move`, or `change`), `--same 'KIND FROM TO'` and `--change 'ITEM=TEXT'` (the facts that you state, see below), `--claim 'OP ANCHOR key=value …'` (an optional property claim), `--reason 'ANCHOR=TEXT'` (the reason for a fact in the spotlight), `--tree` (also print the change tree), `--compiled` (also run the compiled check), `--no-check` (do not run the build check). An anchor is an item path (`ledger_core::fees::charge`, `ledger_core::Account.balance`) or a position `FILE:LINE[:COLUMN]`.
- The MCP server (`semilla mcp`) has the same `step`, `fact`, `undo`, and `tag` tools (see also "A tag after the commit"). Each fact in its JSON has the fields `category` and `status` (`derived` or `stated`). The `step` tool has the arguments `same`, `change` (two lists of texts), `compiled`, and `no_check`. The `fact` tool has the arguments `same`, `change`, and `step`. The `step` tool and the `fact` tool also have the argument `tags` (a list of texts, see "The tags of a step"). A step of a replay plan has the same two keys `same` and `change`, and the key `tag`. The `replay_apply` tool has the argument `no_check`.
- The replay (work first, steps later): run `semilla replay begin` (the work becomes the target; the working tree is `HEAD` again), `semilla replay plan` (it writes `.semilla/plan.json`), `semilla replay apply --full` (it runs all steps from the base, with the build check of each step; `--no-check` turns the build check off), and `semilla replay end`. A plan step needs an intent, its takes (`FILE`, `FILE:START-END`, `FILE:LINE`, or an item path) or edits, and optional `claims` and `reasons`. `semilla step` does not work inside a replay.

## A tag after the commit

`semilla tag <rev>` tags a commit that has no change set (you did not use `semilla step` for it). Semilla makes one step: the tree before is the tree of the first parent, and the tree after is the tree of the commit. The step has the facts, the spotlights, the build check, and (with `--compiled`) the compiled result that `semilla step` gives for the same two trees.

```sh
git commit -am "skip small items"
semilla tag                          # HEAD; or `semilla tag <rev>`
semilla tag main..HEAD               # each commit after `main` on the first-parent chain, in order
```

```text
result: tagged after the commit · 1 step · 3 facts (1 with a spotlight) · 1 open spotlight
tagged 059bf49… after the commit: one step from the first parent 01cdb83 to the commit "skip small items" · 3 facts (1 with a spotlight) · 1 open spotlight
build check: passes (0.9 s)
  …
  ◆ f3 derived: added `shop::host` (the public API changes: a different crate can now use the item) [added]
      ! spotlight: a new item with an attribute that can change the behavior: `cfg` on `shop::host`; open: no reason given
  …
to give the reason for each open spotlight, run:
  semilla fact --commit 059bf49 --reason 'shop::host=<why>'
```

- The command changes no file of the working tree, not the index, not `HEAD`, not the branch, and not your session. It writes only the git note. A dirty working tree is valid.
- A root commit compares with the empty tree. A merge commit compares with its first parent, and the output says "a merge commit: compared with the first parent".
- **An open item does not stop the tag.** The commit is already there, so the note is always stored. The open items: a spotlight with no reason, a build check that fails (the note keeps the errors of the compiler), a build check that cannot run, a configuration that does not build (with `--compiled`), and an error of the check. The result line names each one, and the exit code is 1. With no open item, the exit code is 0.
- **Add the reasons later.** Run the commands of the last lines: `semilla fact --commit <rev> --reason 'KEY=TEXT'`. The same command takes `--same`, `--change`, and `--tag`, with the checks of `semilla fact`. A false fact, a tag with no edit, or a reason that belongs to no fact stores nothing; the output then gives the keys of the open spotlights. No build check runs again.
- With `--compiled`, a configuration that does not build does not stop the others. The output names it with its side and its first error ("it does not build after the step: error[E0425]: …"), and each other configuration gives its compare results.
- A commit that has a note keeps it: "commit 059bf49 has a note of a tag after the commit; the note stays". `--again` makes the note again; your facts, tags, and reasons stay, and a fact that does not hold shows with `✗`.
- A commit with the steps of your session (you used `semilla step`) gets the `tag` of the loop: nothing changes there.
- `--compiled` and `--no-check` work as for `semilla step`.
- `semilla status <rev>`, `semilla review <rev>`, and `semilla load <rev>` show the line "tagged after the commit (no steps of the agent)". The result is `not complete` while an item is open.
- The MCP tool `tag` has the arguments `commit` (a revision or a range), `compiled`, `no_check`, and `again`. The tool `fact` has the arguments `commit` and `reasons` (an object from the key to the text).

## The build check

Each command that stores a step runs `cargo check --workspace --all-targets --offline` on the code after the step. The check uses a build folder in the Semilla cache, not your `target` folder. It makes no fact and no compare result. It has four results.

The check uses the tool chain that cargo uses for you in the repository: the file `rust-toolchain.toml` or `rust-toolchain` of the tree, else the default of the machine (`RUSTUP_TOOLCHAIN`, the rustup default, or the `cargo` of `PATH`). It never downloads a tool chain.

| Result | The output | What it means |
|---|---|---|
| `passes` | `build check: passes (1.2 s)` | Each target builds. The step is stored. |
| `no new failure found` | `build check: no new failure found (0.9 s)`, then one line `! build check: the target … does not build …` for each limit | A target does not build after the step, but it did not build before the step, or cargo did not check it before the step. The step is stored. Semilla did not check a new error in these targets. `semilla status` and the review show the lines. |
| `fails` | `rejected: step 2 "…" is not stored`, then `✗ build check: fails (0.4 s): after the step, the target `shop-core` (lib) does not build`, then the first errors of the compiler with file and line | A target that builds before the step does not build after it. Nothing is stored. |
| `not run` | `build check: not run (`--no-check`)` | You turned the check off. The step is stored. The record, `semilla status`, and the review say: "! build check of step 2: not run: the author turned the check off, so Semilla did not check that this code builds". |

- After a `fails` result, the facts of the syntax phase show below the errors. The last two lines are the two ways out: "the code after the step does not build: correct the code, then run: semilla step '…'" and "or store the step with no build check (…): semilla step '…' --no-check".
- **Correct the code. This is the usual way out.** A usual cause is a rename or a signature change that misses one use.
- `--no-check` (or the variable `SEMILLA_NO_CHECK=1`; the values empty and `0` do not turn the check off) is correct in two cases only:
  1. **A step of work in progress.** You store on purpose a step that does not build yet, and a later step completes it. Say so in the intent. The reviewer sees "not run" for this step.
  2. **An environment that cannot build the project.** The machine has no tool chain, or a dependency is not in the cargo cache, or the project needs a tool that the machine does not have.
- Do not use `--no-check` to get past an error that you can correct. The reviewer sees each step with "not run".
- **A check that cannot run** is not a result of your code: "not stored: step 2 "…": the build check cannot run on this machine", then "! build check: cannot run: …" with the cause (no tool chain, a dependency that is not in the cargo cache, no space on the disk). Correct the environment (for a dependency: `cargo fetch`), or use `--no-check`.
- **A tool chain that is not on the machine:** "cannot run: the toolchain `1.80.0` (the file `rust-toolchain.toml` of the tree) is not on this machine: … Run `rustup toolchain install 1.80.0`". Semilla does not download it. Run the command, or use `--no-check`.
- **A target that does not build before the step** does not reject the step. The result is not `passes`: "build check: no new failure found (0.9 s)", then "! build check: the target `shop-core` (test "old") does not build before the step and after it, so Semilla did not check a new error of the step in this target, or in a target that needs it". A target that builds before the step and that the step breaks rejects the step. A new target that does not build rejects the step too.
- **A target that cargo did not check before the step.** Cargo does not check a target behind a failed target. If your step corrects a library, an old error of a test of this library can show for the first time: "! build check: the target `shop` (test "api") does not build after the step, and cargo did not check it before the step (the targets `shop` (lib test), `shop` (lib) did not build then), so Semilla cannot tell if the step makes this error". The step is stored. Read the error with `cargo check --all-targets`: if your step made it, correct it in the next step.
- `semilla tag` prints each line with the mark `!` of the build check (a step with `not run`, a step with `no new failure found`). It does not refuse the change set.
- The build folder of the check can become large. `semilla cache --clean` removes it and tells the size. The next check then builds each dependency again.
- With `--compiled`, the build of the compiled check is the build check: "build check: passes (the build of the compiled check)". No second build runs. A tree that does not build gets the reject of the compiled check.
- The stored result is the result on your machine at the time of the step. A reviewer command does not run the check again.

## What Semilla derives

Semilla derives each fact, also when you state none. There is one fact for each difference of the syntax tree, and the text of the fact shows the difference. A fact line has the text of the change, then the category. The category says what kind of change the text shows. It does not say if the change has an effect. The category comes from the text of the edit: the same edit always gets the same category.

```text
  ◆ f1 derived: changed expression `self.cmp(rhs)` → `Ord::cmp(self, rhs)` at `semver::impls::<Prerelease as PartialOrd>::partial_cmp/tail/args[0]` [semantics]
  ◆ f1 derived: changed literal `2` → `1` at `semver::identifier::<Identifier as Clone>::clone/S0/else/S3/init` [semantics]
      ! unsafe code: the changed body has an `unsafe` block
  ◆ f1 derived: renamed `semver::eval::matches_caret` → `semver::eval::matches_caret_op` [rename]
  ◆ f1 derived: visibility of `semver::eval::matches_caret`: `private` → `pub(crate)` [visibility]
```

| Category | The change |
|---|---|
| `rename` | a new name of an item, a member, a local, or a parameter, with the forced edits at each use |
| `move` | an item or a file in a different module; an extracted or an inlined variable |
| `visibility` | the visibility of an item or a member |
| `declaration` | a signature, generics, bounds, a type, the set or the order of members, a `derive` list, a qualifier |
| `added`, `removed` | a new or a removed item, member, `impl`, or test |
| `semantics` | a change of the code in a body: a literal, the value of a constant, a statement, an operand, a call, a condition, a branch, a loop, an exit, a `let … else`. The text gives the form ("changed literal", "added early exit") |
| `attribute`, `docs` | an attribute; a doc comment (a doc test change says "doc test" in its text) |
| `import`, `macro` | a `use`, `mod`, or `extern crate` line; a macro definition or a macro call |
| `manifest`, `file`, `build code` | a key of a manifest or of a cargo or rustup configuration; a file that is not Rust code; a change of build code |
| `text` | changed source text with no other fact |

- Equal facts fold into one line: "3 site(s): changed literal `1` → `2`". Facts fold only if their full texts are equal. The same type change in many places is one fact: "type `u32` → `u64` in 5 places: …". A fact that needs a reason does not fold. The fact IDs start at `f1` in each step. Facts with the same text, category, and lines below are one line in the text output: "[import] × 3 (also f3, f5)", then "places: src/one.rs, src/two.rs, src/four.rs".
- Two lines under a fact ask for no reason. "! public API: …": a different crate can use the changed item. "! unsafe code: the changed body has an `unsafe` block": the edit is in a body with `unsafe` code.
- A `mod NAME;` line in the input of a macro call (`cfg_if!`, a macro of the project) names a module file. Its items get their usual facts. The step gets one line for each such module with a fact, and the line asks for no reason: "! spotlight: module of a macro input: `depot::unix` (`src/unix.rs`) has its line `mod unix;` in the input of the macro call `depot::pick!#0` (`src/lib.rs:6`); Semilla does not expand the macro, so it does not know if the build has this module".
- Twins are items of one module with one path (two calls of one macro, `cfg` variants, two `impl` blocks of one type). Each twin has a number in source order in its own tree: `depot::a::table!#0`. Twins with equal tokens pair first, so a new call gives one fact: "added macro call `table!(Z = 9,)` [added]", with the reason key of its number in the new tree. A pair with two different numbers has one key, and a note gives the two paths: "note: twins: `depot::a::table!#1` of the old tree and `depot::a::table!#2` of the new tree are one pair …; the facts name the pair `depot::a::table!#2`". The name of a macro call with a path has no space: `depot::crate::table!#0`.
- More fact texts: "public API: a different crate can no longer use `shop_core::over` [move]"; "doc test of `shop_core::cart::Cart::sum_below` changed [docs]"; "added macro call `same!(Cart)` (makes: `impl PartialEq for Cart`) [added]"; a field fact lists its forced sites with the exact edit: "1 site(s): add `..` to the pattern (in demo::a)". A local macro with the name of a standard macro: "added macro `matches` (it replaces the standard macro `matches!`; each call in its scope: `demo::f`)". A new `cfg` twin of an item: "added `slab::Slab::capacity#1` (a twin of `slab::Slab::capacity` under `cfg(not(feature = \"std\"))`) with the body: `{ self.entries.capacity() + 0 }`"; the old item keeps its name. A statement that moves in its body is one fact, also into or out of a block or a closure: "moved statement `s += a * 2` from `shop_core::mix/S1` to `shop_core::mix/S1/then/S0` [semantics]". A moved statement is `semantics`: a move changes the order of execution. Each other change of the statement is its own fact. An extracted variable is one fact: "extracted variable `area` = `a * b + 1` (2 uses) at `demo::g/S0`" (also "inlined variable …").
- Renames and moves: do nothing. Semilla derives a rename or a move from the syntax tree, also when the step has other changes. The pair passes when the old item is equal to the new item after the map of the names. The rename or the move is one fact, and the forced edits go under it: "moved `shop_core::over` → `shop_app::over`, with 1 `use` line(s) [move]". A forced edit is a use that names the item on each side, a `use` line that the map explains, or the `mod` line. Each other change is its own fact at the new path, with the full old and new text: "changed operator `ver.minor > minor` → `ver.minor >= minor` at `semver::eval::matches_caret_op/S3/then/S0/then/S0/value` [semantics]"; "changed path `Fault::TooMany` → `Failure::Invalid` at …"; "added `use` line: `use std::fmt;` in `shop_app`". Limit: Semilla does not resolve the names again after the map. A rename that makes a name point to a different item by shadowing is not detected in the default mode.
- The map of a rename changes only a reference to the renamed item. A word in a string or in a comment, and a path to a different item with the same name, are not references: a change of such a place is its own fact ("changed literal `\"load\"` → `\"fetch\"` at `depot::net::name/tail` [semantics]"). A member after `.` and a name in a macro input are references by name: the syntax tree does not give their target. They go under the rename fact only if no other declaration of the workspace has the old name or the new name. Then the fact has the line "note: 2 site(s) by name (the syntax tree does not give the type of the receiver or the target of the path)". Do nothing for it. If the name is not free, each such use is its own fact: "changed method_call: `l.load()` → `l.fetch()` at `depot::net::c/tail`". A place that did not follow the rename also has a fact: "`load` still names the old item `load` (the place did not follow the rename to `fetch`; …)", or "`load` now names a different item: …". Read this fact: the step possibly does not compile, or the place now uses a different item.
- The own tokens of a node are the tokens that are in no child. If the own tokens and a child change in one step, the fact has the two changes. A node with no block has one fact with the full old text and the full new text: "changed break: `break 'p 1` → `break 'q 2` … [semantics]". A node with a block keeps the facts of its children and has one more fact with its own tokens: "changed async: `async move {▫}` → `async {▫}` … [semantics]". The mark `▫` is in place of a child (a condition, a block, an argument). It is not a cut: each edit in the child has its own fact. A comma that the grammar permits before a closing delimiter and an empty statement give no fact.
- An identifier in a pattern is a local only if it is a sure binding. A name of a constant, a static, a variant, or a struct with no named fields, and a name that starts with an upper-case letter, is a use of an item: "changed pattern `LOW` → `HIGH` at `yard::work::n2b/S0/arm[0].pat` [semantics]", not "renamed local". The words `ref` and `mut` and the sign `@` make a binding sure. A name from a `use` line in a block of a body resolves through that line, not through the module.
- If Semilla cannot derive a pair, the two items show as removed and added, and a note gives the cause: "… show(s) as removed and added: Semilla does not derive the rename (…)". This is also the result when two name maps pass with the same count of other facts. The note does not reject the step. A swap of two names gives no rename fact: each body edit is its own fact. A pair warning (`! pair: …`) shows a removed and an added item with a similar text. It does not reject the step. If they are two different items, run the printed `semilla pair refuse` command with a reason. Two entities of a different kind with one path are not a pair: "removed `quickcheck::arbitrary::Gen` (a trait)" and "added `quickcheck::arbitrary::Gen` (a struct)". An `impl` block follows the rename of its type: its changed header is one `declaration` fact, and each member edit is its own fact.

## The facts that you state

You know the intent of the step. Semilla knows the edits. A fact of yours connects the two: it puts the edits of the step below one line that the reviewer reads first. **Write very simple, high-level facts.** A step usually has 0 to 3 facts.

| You write | When | Semilla checks |
|---|---|---|
| `--change 'ITEM[,ITEM]=TEXT'` | The behavior of existing code changes on purpose. `ITEM` is a function, a type, an `impl`, a module, or a file. `TEXT` is one sentence: how the behavior changes. | Each item has an edit that can change the behavior (a body edit, a macro call, a signature, an attribute). |
| `--same 'rename OLD NEW'` | You renamed an item. | Semilla derives the same rename from the syntax tree. Your pair is a candidate: the check is the same. |
| `--same 'move FROM TO'` | You moved an item to a different module. | Semilla derives the same move. Your pair is a candidate. |
| `--same 'rewrite FROM => TO'` | One mechanical rewrite at many places. Each side is a Rust expression. A variable (`$a`, `$b`) is in place of one expression. | Each site: the full difference of the edit is the pattern. Semilla checks the form, not the behavior. |

```sh
semilla step "use the method for the total" \
  --same 'rewrite $a.total => $a.total()' \
  --change 'limit=the limit of a cart is 20 items, not 10'
```

```text
  facts of the agent (Semilla checks each fact against the edit. It does not check the sentence of a `change`. It does not check the behavior after a `rewrite`):
  ✓ a1 same: rewrite `$a.total` => `$a.total()` (the form holds: 2 site(s) have exactly this edit. Semilla does not check that the behavior is the same)
      places: shop::a/tail/lhs, shop::b/tail/lhs
    ◆ f1 stated: 2 site(s): changed expression `c.total` → `c.total()` [semantics]
        in: shop::a, shop::b
  ✓ a2 change: `limit` "the limit of a cart is 20 items, not 10" (holds: the behavior of existing code can change in each item)
      places: shop::limit/tail/args[0]
    ◆ f2 stated: changed literal `10` → `20` at `shop::limit/tail/args[0]` [semantics]
```

**What you do not write.** Write no fact for a new item, a removed item, a signature, a bound, an attribute, docs, imports, layout, a manifest, or a test. Semilla shows them alone, in the groups "filled in by Semilla". Do not write the intent again as a fact. Do not write `extract` or `inline`: they have no check yet, and the command says so.

**The rules of a fact.**

- Semilla checks each fact against the edit. It does not check the sentence of a `change`. A `rewrite` fact is a check of the form: "the form holds" does not say that the behavior is the same. `rename` and `move` say "holds": Semilla proves them from the syntax tree.
- `ITEM`, `OLD`, `NEW`, `FROM`, and `TO` are loose paths: `limit`, `cart::limit`, `Cart::total` (a method), `Cart.total` (a field), or `src/cart.rs`. The old path and the new path are valid.
- Each form of the path of a trait impl method is valid: `<shop::money::Price as core::fmt::Display>::fmt` (the form of the compiler), `shop::money::<impl Display for Price>::fmt`, `<Price as Display>::fmt`. Generic arguments are optional (`<Price as Scale>::scale`). White space is not important.
- The path of a type or of a trait can be each path that names the item: `shop::money::Price`, the re-export `shop::Price`, or `crate::Price` as in the `impl` header. Two types with one name are different: give the module to select one.
- If the path names two items, the message gives the candidates with their generic arguments, the best first: copy one. An item that a macro call makes has no path: the message names the macro call and its module.
- Two items with the same path (two `impl` blocks with different `#[cfg]` conditions) are one target: the result says "2 items with this path". Add the mark of Semilla (`scale#0`) to select one.
- An item of a target that is not the library has the target in its path, in the form of cargo: `depot (test "list")::first`, `depot (bin "depot")::helper`, `depot (example "demo")::main`, `depot (bench "speed")::run`. An item of the library has the package name only: `depot::list::first`. Semilla prints this form in each fact. Write it in quotes: `--change 'depot (test "list")::first=TEXT'`. A loose path still works if it has one match: `first`, or `list::first` (the crate name of the target names the target). An exact path comes first: `depot::first` is the item of the library if the library has one.
- A member of a block has its own path and its own fact: `depot::Shape::SIDES` (a const of an impl), `depot::<Shape as Named>::Name` (a type of a trait impl), `depot::Named::SIZE` (a member of a trait), `depot::extern C::LEVEL` (a member of an `extern` block).
- A path into a target that Semilla does not parse (`speed::run` for a bench of a package that is not a workspace member) names the file. The result says that the fact covers the full file.
- A stated rename or move is a candidate of the rename rule, also when Semilla does not find the pair alone (many renames in one module, or a rename with a large edit of the body). It needs evidence: one reference that follows the rename, or similar declarations. If you replaced an item with a different item, state `--change`, not `rename`. The check does not get weaker: each edit that is not the rename keeps its own fact, and it is in the gap list. The order of the options is not important.
- When a fact is false or has a form error, look for the line "a fact that holds: `--same '…'`" or "The correct form: …". It is a complete option that passes the check. Use it only if it says what you did.
- A name in a pattern matches only that name. For a variable, write `$raw`, not `raw`.
- A `change` fact on an item with no edit, or with only a rename site, a move, or docs, is false. The message shows what Semilla sees in the item.
- A `change` fact on a new or a removed item is not false, but it does nothing: "note: Semilla shows `ITEM` as added: the fact covers no existing code".
- A `rewrite` site with one more edit is not a site. That edit keeps its own fact. A pattern with no site is false: the message shows the nearest edits and the cause.
- A variable is one expression. It is not a type, a pattern, or a method name. For such a change, use `--change`.
- Two forms of one change need two `rewrite` facts: `rewrite $c.total => $c.total()` and `rewrite $c.total => Cart::total($c)`.
- A false fact rejects the step. Nothing is stored. The last line is the command with no false fact.

**The gap list.** After your facts, Semilla prints each edit of existing code that can change the behavior and that no fact states. Tests, benches, new items, and removed items are not in the list. The list never rejects a step.

```text
  not stated: 3 item(s) with 3 place(s) that can change the behavior of existing code and that no fact of the agent states
  ? 2 places: changed expression `c.total` → `c.total()` (f1)
      in: shop::a, shop::b
      to state it: --same 'rewrite $a.total => $a.total()'
  ? 1 place: changed literal `10` → `20` (f2)
      in: shop::limit
      to state it: --change 'shop::limit=TEXT'
  to state them, add facts to the stored step (no build check runs again): semilla fact --step 0 --same 'rewrite $a.total => $a.total()' --change 'shop::limit=TEXT'
```

- Each line is a group of equal edits, largest first. Start at the top: one fact for the first line explains the most places.
- "to state it" is an option that passes the check. Read it before you use it. Use a `rewrite` option only if the rewrite is what you did on purpose. Write your sentence in place of `TEXT`.
- The step output shows the first 5 lines. `semilla status --verbose` shows each line.
- A gap is not an error. If an edit is not a part of your intent (a side effect that you did not plan), look at it. Then state it, or correct the code in the next step.

**How to add a fact.** `semilla fact [--step N] --same '…' --change '…'` adds facts to a stored step (default: the last step). No build check runs again, because the code of the step is the same. The output shows each fact with its result and the new count of the gaps. A false fact stores nothing.

If a step has a gap, `semilla status`, `semilla tag`, and the review show one line with the count: "not stated: 2 item(s) with 5 place(s) …". A step with one fact of yours or more and no gap shows "not stated: 0 item(s)". A gap line with a long text is short: "changed item, see f1". The full text is at the fact `f1`.

**A changed text with two long sides** shows as one block, a line diff: "changed item: [1] (line diff: `-` old, `+` new)". Each line of the old text and of the new text is in the block.

**A replay plan.** A plan step with a false fact or a refuted claim has the mark `✗` on its plan line, with the cause below it. `semilla replay apply` then has the exit code 1, with and without `--full`.

## The tags of a step

A tag is a name for a part of a step. Use tags to put the facts of a large step in groups that a reviewer can read one at a time. You give the part by the hunks of the line diff of the step.

```
semilla step "add the retry option" \
  --tag 'src/net.rs:40-88,src/cfg.rs:12=retry option' \
  --tag 'src/net.rs:-120-131=old loop removed'
```

**A tag is not a fact.** A tag makes no claim, and Semilla checks no behavior for it. A tag does not replace a fact: each fact of Semilla and each gap stay in the output, and the result of the step is the same. State the intent with `--same` and `--change` as before.

**How to write a position.**

| You write | The lines |
|---|---|
| `FILE` | The full file, the old tree and the new tree. |
| `FILE:LINE`, `FILE:START-END` | Lines of the new tree (the working tree of the step). |
| `FILE:-LINE`, `FILE:-START-END` | Lines of the old tree. A deleted text has only such lines. |
| `FILE:+START,COUNT`, `FILE:-START,COUNT` | One side of a hunk header, as `git diff` prints it. |

**The command that gives the hunks.** The changed lines of a step are the lines of one `git diff` command. The step output prints it with the two trees of the step (the line `hunks:`):

```
git diff -U0 --inter-hunk-context=0 --diff-algorithm=myers --indent-heuristic --no-renames --no-ext-diff --no-textconv --no-relative --text --no-color OLD_TREE NEW_TREE
```

Run the step first. Then run the printed command in the repository: the two trees are objects of the repository. Add the tags to the stored step with `semilla fact --tag` (see below). Each position of a fact, each line of the cover check, and each hunk of a tag is a line of this command. Do not use a plain `git diff`: your configuration can give different hunks. If the output says "git gave no line diff of this step", `semilla status --verbose` prints the hunk headers that Semilla uses.

Read the positions from the hunk headers of the command. The header `@@ -120,12 +40,49 @@` of `src/net.rs` gives two places:

- `src/net.rs:+40,49` (or `src/net.rs:40-88`): the 49 lines of the new tree.
- `src/net.rs:-120,12` (or `src/net.rs:-120-131`): the 12 lines of the old tree.

**How a place selects a hunk.** The unit of a tag is the hunk.

- A place that has the full new side of a hunk, or the full old side, selects the two sides of that hunk. One of the two places above is sufficient for the hunk `@@ -120,12 +40,49 @@`.
- A place `FILE` selects each hunk of the file.
- A side with the count 0 has no line (`@@ -23,0 +29,2 @@` has only new lines). Use the side that has lines (`src/net.rs:+29,2`). The form with the count 0 is also valid (`src/net.rs:-23,0`): it names its hunk.
- A place that has only a part of a side selects only the changed lines in its range. It does not select the other side: a range of the new tree then has no deleted text.
- The simple way for a full step: one place for each hunk header of the command. Then the tag has each fact that has a position.

**The rule.** A tag has each fact whose lines are all selected lines of the tag.

- A fact with lines in the tag and lines out of it shows with the tag and the mark "in part", with the lines that are not in the tag: "in part: f6 has lines that are not in the tag: src/net.rs:3-4". The head line of the tag counts these facts apart ("0 fact(s); 1 fact(s) in part; 2 changed line(s)"). Add the hunk of these lines to the tag if the fact belongs to it.
- A fact with no position (an `indirect` fact, a fact of the public API with no edit of its own) is in no tag and not in the group "not in a tag". These facts show in their own line: "facts with no position (a tag selects changed lines; these facts have none): …".
- `semilla status --verbose` prints the position of each fact (the line `lines:`). The gap list of the step output prints the positions of each gap.

**The errors.**

- A text that is not `PLACE[,PLACE]=NAME`, or a line number after the last line of the file: exit code 2. No step is stored.
- A place with no edit in its lines: the step is not stored (exit code 1). The message names the nearest positions with an edit: "the step has no edit in `src/net.rs:1-3`; the nearest positions with an edit: src/net.rs:11". Correct the position and run the step again.

**The output with tags.** The facts show in the groups of the tags, in your order. The head line of a tag has the count of its facts by category and the count of the changed lines that its places select. The group "not in a tag" shows only if a fact with a position is in no tag. A fact of two tags shows in the first tag that has the full fact.

```
  tag `method calls` (crates/shop-core/src/facts.rs:11-17): 2 fact(s): 2 semantics; 6 changed line(s)
  ◆ f1 derived: 2 site(s): changed expression `c.total` → `c.total()` [semantics]
  …
  not in a tag: 2 fact(s): 2 semantics; 4 changed line(s)
```

A gap line names its tag (`tag: NAME`, or "not in a tag"). A step with no tag has the same output as before: it has no tag line.

**How to add a tag later.** `semilla fact --tag 'PLACE=NAME'` adds a tag to a stored step. No build check runs again.

**The width of a `change` fact.** The result line of a `change` fact tells what it covers: "covers 2 fact(s) in 1 item(s) with 6 changed line(s)". A fact that covers many facts in many items tells the reviewer little. Write one narrow fact for each change of the behavior.

## The cover check

Each changed line of the diff of a step is in the position of a fact of Semilla. The step output has one line for this check:

```
  cover: each of the 18 changed line(s) is in a fact
```

- A changed line with no change of the code gets a fact too: a `layout` fact (equal tokens: white space, line breaks, the same items in a new order), a line-ending fact, or a comment fact (category `docs`).
- A line with the mark `!` is a defect of Semilla, and not an error of yours: "! changed lines with no fact: src/a.rs:12-14". The step is stored. Read these lines in the diff, because no fact of Semilla describes them.

## The comments of a changed item

Semilla shows the comments of each item that your step changes: its docs, its leading comment, and the comments of the nodes on the path to the edit (a statement, a loop, a field). The lines are below the fact line:

```
  ◆ f1 derived: changed literal `10` → `5` at `shop::price/tail/rhs/rhs` [semantics]
      docs of `shop::price`, no edit in this step (src/lib.rs:1): `/// Gives the price with a tax of 10 percent.`
      note: Semilla does not check that a comment agrees with the code. A comment with no edit can be out of date.
  comments: 1 comment(s) of the changed items have no edit in this step
```

- **"no edit in this step"**: the text of the comment is equal before and after your step. Read the comment. If it is not true for the new code, correct it in the next step. If it is still true, do nothing.
- **"changed in this step: see f2"** (also "added", "removed"): your step has an edit in the comment. The fact `f2` shows it. The line does not show the text a second time.
- **"moved in this step: see f1, f3"**: the text of the comment is equal, and your step put it at a different place of the item. The facts show the removed line and the added line.
- **"comment that was above the removed code at old line N"**: your step removed the code below the comment, and the comment stays. Check that it is still at a correct place.
- A long comment is a block: the head line ends with `:`, and each line of the comment follows with the mark `| `. The text has no cut.
- A comment shows one time, with the first fact of its item in the output.
- The line `comments: N comment(s) …` gives the count of the lines with the mark "no edit in this step". The step output, `status`, `tag`, and the review show it. No line shows for the count 0.
- **A comment with no edit is never a reject.** It needs no fact, no reason, and no option. It is not in the list "not stated".
- Semilla does not check that a comment agrees with the code. It compares only the text of the comment in the two trees.
- A new item, a removed item, and a step with only a docs edit give no such line.

## The optional claims

A claim says a property that the diff does not show. If it is false, the step is rejected. A property claim (each claim of the table) reads the compiled code. So it starts the compiled check for its step: "note: the claim `no-panic` is a property claim, so this step runs the compiled check (as with `--compiled`)". Each example below runs in the tests (`crates/semilla/tests/e2e.rs`).

| Claim | Example | Result |
|---|---|---|
| `tested-by` | `--claim 'tested-by shop_core::cart::sum_wide test=wide'` | `verified (mir)`: the test calls the function on each path to its normal return. `verified (partial)`: the call is under a condition, in a loop, or after a possible exit. The scope line gives the cause |
| `no-panic` | `--claim 'no-panic shop_core::cart::share'` on `total / n` | `refuted: implicit panic sites: division by zero …` |
| `no-unsafe`, `no-io` | `--claim 'no-io shop_core::cart::sum_wide'` | `verified (mir)` |
| `calls-only` | `--claim 'calls-only shop_core::cart::save calls=["std::fs"]'` | `verified (mir)`: each call to another crate is under a path in `calls`. Calls to `core` and `alloc` are always allowed |

- The claim `rewrite-pattern` is gone: state the pattern as a fact of the step, `--same 'rewrite FROM => TO'`. A stored session with the claim still reads.
- The claims `rename` and `move-item` are gone. The command says: "the claim `move-item` is gone: Semilla derives each rename and each move …". `semilla ops` lists the claims. The anchor of a method of a trait impl is `<Type as Trait>::method`: `--claim 'no-unsafe shop_core::cart::<Weight as core::convert::From<Kilos>>::from'`. If a name matches no item, the message lists the nearest names.
- An entry of `calls` matches full path segments: `std::fs` matches `std::fs::write`. If entries are missing, the claim is refuted, and the `run again:` line has the complete list. Use it only if the function must make these calls.
- Semilla does not run tests, and it does not check what a test asserts. It does not follow a call through a trait object or a function pointer. Do not change an honest test to get `verified`. A property claim examines the local code that can run from the function. The result gives the scope: "examined: 3 local bodies (1 through trait impls, 0 drop bodies); not examined: …". Something "not examined" gives `verified (partial)`. A refuted `no-panic` claim gives the route to a site in a different item: "the route to `semver::parse::<Comparator as FromStr>::from_str`: `semver::eval::matches_req` → a generic function of a different crate → each trait impl of `semver::Comparator` → `semver::parse::<Comparator as FromStr>::from_str`".

## The compiled check (`--compiled`)

`semilla step "<intent>" --compiled` (or the variable `SEMILLA_COMPILED=1`) also builds the two trees and compares the compiled code (MIR). It needs the nightly tool chain and each dependency in the cargo cache. Each step stores its mode. `status`, `tag`, and `review` start a build only for a step with the compiled check. A fact of such a step has a compare result after the category: `[semantics · code different]`. The result line of `tag` counts them: `result: accepted · 3 steps · 4 facts: 2 code different, 2 code equal · 0 claims` (a change set with the two modes adds "2 with no compiled check"). `semilla status --verbose` adds the counts of the MIR map. The compare result is an observation. It is not a verdict. It holds for the checked configurations only: `dev` and `release`, each with the default features, no default features, and all features.

| Compare result | Meaning |
|---|---|
| `code equal` | The edit owns no MIR element that differs, and each element that differs in its bodies has an owner (an other edit). Else each body in the scope of the fact has the same compiled code, in each checked configuration. |
| `code different` | The edit owns a MIR element that differs, or a body has different compiled code and an element of it has no owner, or a body is new or gone. "affected bodies: …" lists the bodies. |
| `no compare` | Semilla made no compare. A note gives the cause: "note: no compare: no checked configuration compiles `shop_app::license::limit`", "note: no compare: Semilla does not compare the code of a dependency" (a new version in `Cargo.lock`). |
| `no code` | The fact has no compiled code: a doc comment, a manifest key, a file that no body reads. |
| `pending`, `not checked` | The compiled check did not run yet (only with `--fast`). The command did not check the step (a reviewer command with `--no-build-code`). |

- "The same compiled code" means: the MIR is equal after the map of the renamed and moved names. Nothing more. A swap of two operands, a removed local, and a fold of a constant condition give `code different`. The category `indirect` exists only with this check: the compiled code changes at a place with no edit. Semilla proves a rename or a move with the MIR in this mode; the rules of the forced edits are the same.
- Each MIR element that differs has an owner: the edit at its place, else the edit of a changed definition that the element names (a constant, a type, a field, a signature, an alias, an `impl`, a `use` line), else the edit of a changed local of the same body that the element depends on (`let x: u32` → `let x: u16` owns each element that reads `x`; a temporary follows the element that reads it). A line names the symbols: "through: `shop_core::LIMIT` (2 bodies)". A changed type of a `let` is "changed pattern `x: u32` → `x: u16` … [semantics · code different]", not a rename. A line "branches: the compiled code of this edit changes its branches (0 → 1 branch point(s), 0 → 2 targets)" asks for no reason. The elements with no owner of a body give one fact: "the compiled code of `shop_core::bonus` changes with no edit at its place: `BONUS` [indirect · code different]", with the lines in a note. In a body with no edit, it needs a reason; more than 3 such bodies of a crate give one fact for the crate (the reason key is the crate name). A forced edit with equal code goes under its fact: "bodies with a changed call site and equal compiled code: …". If a body has more edits, its line says so: "affected bodies: semver::eval::matches_caret (with 4 other edits of this body)".
- A mismatch mark shows when a fact of the category `rename`, `move`, `visibility`, `import`, `docs`, or `attribute` has `code different`: "! the compiled code changes: <shop_core::Account as std::fmt::Debug>::fmt". The mark asks for no reason. A derived `Debug` body shows the new name of a type or a field: then the fact is `[rename · code different]` with the mark. This is correct: do nothing. The line "! public API: the compare does not cover the code of a different crate" replaces the short form.
- A note under a fact (`note: …`) gives a limit of its compare result, or the count of the by-name sites of a rename. A note asks for no reason, and you do nothing for it. Example of a condition that Semilla did not check: "note: this body uses `cfg!(unix)`, which Semilla did not check (`shop_core::percent`)". Example of configurations that do not agree: "note: the compiled code is equal in `dev`; different in `release`" (the compare result is `code different`).
- An equal compare that uses two build conditions or more needs each joint state. Semilla adds the missing configuration and checks the step again: "Semilla added the configurations `dev, features fast`, `release, features fast` for a joint state". The limit is 4 added feature selections. If a state is still missing, a note names it and gives the `feature-sets` entry for `[workspace.metadata.semilla]`. A step that breaks one configuration is rejected: the message names the configuration and shows the compiler error.
- For a large project, `semilla step --compiled --fast` runs only the syntax phase and runs the compiled check in the background. Then a fact with code has the compare result `pending`. The check runs at once when the step has a claim, or a possible rename or move. `semilla status --wait` waits for it. Without a compiled check, `--fast` has no effect, and a note says so.
- The control flow graph of each body (design 60). A fact whose lines hold a changed point or arrow of the graph gets lines `control flow: …`, at most three, then `control flow: … N more (semilla status --verbose)`. Examples: "control flow: + branch `qty == 0` (src/lib.rs:12): true → return, false → call `price`" (a new branch with its arrows), "control flow: ~ branch `v > 3` → `v >= 3` (src/lib.rs:-13, src/lib.rs:14)" (a changed condition), "control flow: ~ call `<Food as Tax>::rate` → `<Fuel as Tax>::rate` (src/lib.rs:-9, src/lib.rs:9)" (a changed call target: the callee that the compiled code calls, also for a new receiver type), "control flow: + panic `assert!(x < 9)` (src/lib.rs:4)" (the panic of a macro is one point), "control flow: + graph of `shop::two`: 4 points, 4 arrows (semilla status --verbose)" (a new body). A fact line shows the short form of a callee (no module part); `semilla status --verbose` shows the full path. A closure has its source text after its name: `shop::total::{closure#1} |x| x.cost() (src/lib.rs:7)`. These lines replace the line `branches: …`. The step section `control flow: N bodies` has one line for each body with an edit or a changed graph: `graph changes: +1 branch, +3 arrows, −1 arrow`, `same structure: 1 changed condition`, `same graph (the edit keeps the control flow)`, `graph added`, `graph removed`, `graph not shown: coroutine` (an `async` body), and `; no edit in the body` for a body whose graph changes with no edit at its place (a changed macro, constant, or callee). `semilla status --verbose` shows each changed point and each changed arrow. The lines ask for no reason, and you do nothing for them. Read a `graph changes` line of a step that you think keeps the behavior: it tells a changed branch, call, or exit.
- The equivalence of the two versions of each body (design 61). A fact in a body with an edit or a changed graph gets the line `equivalence: …` after its `control flow:` lines: "equivalence: equivalent (proved by SMT after inlining `shop::ship`)", "equivalence: equivalent (proved by equal MIR)", "equivalence: different: for w = 20: before returns 5, after returns 15", "equivalence: different in dev: for x = 4294967295: before returns 0, after panics (overflow add); equivalent in release (proved by SMT …)", or "equivalence: not decided: a loop" (the cause). "Equivalent" is a proof for the checked configurations: each input gives the same return value or panic, the same calls in the same order, and the same values behind each `&mut` parameter. "Different" always has a real input and the two outcomes. In a body with more than one edit, each fact gets its own result first: "equivalence: equivalent alone (proved by equal MIR); the body: different: …" (the sites of one rename go together), or "not decided alone (the body has 2 edits; …)". The step section adds the result at the end of each body line: "shop::quote (src/lib.rs:1): same structure: 1 changed condition; different (for n = 1, p = 100: before returns 100, after returns 95)". A changed instance of a generic body has a line under the fact that names its type ("control flow: instance charge::<shop::Fuel>: call `<T as shop::Tax>::rate` resolves to `<shop::Fuel as shop::Tax>::rate` (before: the instance charge::<shop::Food>)"), and a changed set of the types that the code converts to `dyn Trait` has a line in the step section. A check or a branch whose operands are all constants is not a point of the graph (`60 * 1000` has no overflow check). `semilla status --verbose` shows the method, the inlined callees, the solver queries, and the time of each check. The check of a step has a time budget (10 s to 60 s, with the builds of the variants). A check that the budget stops says "equivalence: not checked (the budget of the step)", and the step section has one line: "the equivalence check stopped after 12 s: 41 bodies not checked; semilla status --verbose". A proof by equal MIR needs no solver, so the budget never stops it (design 66.10). The builds of the single edits ("alone") run next to the body checks from the start of the budget, in the order of the configurations: each fact gets its first configuration before a fact gets its second (design 66.11). "not checked alone (the budget of the step)" means that no time was left for the build of that variant. The lines ask for no reason. Read a `different` line of a step that you think keeps the behavior: the input shows the case.
- The value of a `const`, a `static`, or an associated constant is a body (design 61.15). An edit of the value is an edit of each of its bodies, also of a const block in it: each gets its line in the step section and its `equivalence:` line ("equivalence: different: for each input: before returns 7, after returns 6").
- "Equivalent" is a statement about the program (design 61.10). A body with the same compiled code and a changed callee is not "equivalent (proved by equal MIR)": the check inlines each changed callee. `status --verbose` shows the body-local result: "body-local: same body code; changed callee: shop::tier".
- The callers of a changed body (design 61.10). A fact whose body changes gets the line "reaches: 3 public functions: shop::cost, shop::quote, shop::bill (semilla status --verbose)". The step section `control flow:` has the part "reached bodies: N": "shop::cost (src/lib.rs:9): different through shop::tier: for k = 1, n = 1: before returns 4, after returns 5", "shop::base (src/lib.rs:21): equivalent (proved after inlining shop::tier)", "shop::bill (src/lib.rs:17): reached through shop::quote, not decided (…)". Bodies with an equal result fold into one line. The walk stops at depth 4, at 64 checked bodies, or after 20 s (less if the budget of the step is smaller), and says so: "the walk stopped at depth 4: 2 more caller(s) not checked". `status --verbose` gives the chain of each body. If a caller is different and you did not expect it, check the caller.
- A loop (design 61.11). Two loops that pair can be "equivalent (proved by SMT, by induction over the loop)". A difference that a bounded run finds has the mark "(a bounded run of at most 4 loop iterations)". Else: "not decided: the loop bodies differ for a state that Semilla cannot show is reachable; …".
- A cause of "not decided" names the part and its place: "not decided (a closure value: |x| *x > 9 (src/lib.rs:-2))" (a line `-N` is in the old tree). "not decided: the order of the calls changes: shop::log_a, shop::log_b": the same calls run in a different order, and at least one of them has an effect (design 64): a write through a `&mut` argument of the other call's place, an effect on the world (a `static` that can change, a `Cell` or another type with interior mutability, a raw pointer, input and output, a `dyn` call, a callee with no MIR), or a stop (a panic or a loop). A call with no effect is a value: you can move it, and the result stays "equivalent". Two calls that write two different `&mut` places can change their order. Two `push` calls on one `Vec` of integers in another order give "different: for v = &[]: before returns () with `*v` = [1, 2], after returns () with `*v` = [2, 1]".
- A body whose text does not change but that reads a constant with a changed value has a line in the step section (design 64.5): "shop::price (src/lib.rs:4): same graph; reads a changed constant: `shop::RATE` (7 → 6); different (for n = 1: before returns 7, after returns 6)". Check each such line: the change of the constant reaches it. "not decided: Semilla has no form of this body (…)": Semilla could not read the body; the build result does not change.
- Closures (design 66.2). A closure is a value: its captures and its body. A call of a closure runs its body, also in a small helper of the standard library (`Option::map`, `unwrap_or_else`), so `o.map(|x| x + k)` → `match o { … }` is "equivalent". A closure that a call does not inline is equal to the other version only if Semilla proves the two closure bodies equivalent: "equivalent (proved by SMT and equivalent closure bodies `shop::f::{closure#0}`)". A closure that captures by `&mut` writes its place, so two calls in another order are not equivalent; a closure that captures a `Cell` is an effect. A closure behind `dyn Fn` is an unknown callee. "not decided: a raw pointer" after an `iter()` of a slice: the iterator of a slice is not in the model yet.
- A generic or associated constant (design 66.3) has one value in the two versions only if its definition and each item of its value do not change. Else: "not decided (a constant that the compiler does not evaluate here changes: `shop::Grid::<T>::ROWS::<T>` (changed: `shop::unit`))", and a body that reads it has the line "reads a changed constant: `shop::Grid::<T>::ROWS` (definition #7bc0… → definition #38eb…)". Check that the change of the definition is what you want for each reader. A part of the definition that Semilla cannot compute (more than 48 items, a chain of trait constants deeper than 4, a local item with no MIR) is never equal to the other version (design 66.10). Semilla does not know if such a constant changes (design 66.11): "not decided (a constant that the check cannot compare: `shop::Big::<T>::FAR::<T>` (the limit of the digest))", and the step line says "reads a constant that the check cannot compare: `shop::Big::<T>::FAR` (the limit of the digest)". The line shows in each step that checks the body, also with no change of the constant. Read the body and the definition. "reads a changed constant" means two known values that differ.
- The first try inlines the small helpers of the standard library (design 66.4): the method text of a proof can name them ("proved by SMT after inlining `core::num::<impl u32>::wrapping_add`"). A cause "(only the changed callees and the small helpers of the standard library inlined)" means that the difference depends on a call that Semilla does not inline.
- Expanded code is code (design 63). The check compares the expansion of each macro call before and after the step with the same compare as the code that you write: a changed expansion gives the normal facts, with the call and its place. The line goes under the fact that causes it (a changed macro definition, also through a different macro that calls it; an edit in the input of the call; for a derive, an edit of the type): "expansion: `fee!(v)` at `shop::price/tail` (src/lib.rs:4): expansion changed: changed literal `10` → `12` at `expansion/lhs/rhs` [semantics]", "expansion: derived `PartialEq` of `shop::Point`: expansion changed: added operand `&& self.z == other.z` at `shop::<Point as ::core::cmp::PartialEq>::eq/tail` [semantics]". Equal changes of one macro fold: "expansion: 4 calls of `bump`: expansion changed: changed literal `1` → `5` at `expansion/rhs` [semantics]: `shop::a/tail`, `shop::b/tail`, `shop::c/tail`, and 1 more (semilla status --verbose)". A line goes under one fact only: the first fact that causes it; the line names the other causes: "… (also the change of f2, f3)". A changed file that the code of the call reads (`include_bytes!`, `include_str!`) is a cause: "the code of the call reads `data/a.txt`, which the step changes". A call whose definitions and input are equal, and whose expansion changes only in values that the compiler or a proc macro makes at the call, says so and names the source (design 65): "only the context of the call changes: the line of the call", "… the module path of the call (`module_path!()`)", "… the environment variable `CARGO_PKG_DESCRIPTION` (`env!("CARGO_PKG_DESCRIPTION")`); changed code `a shop` → `a small shop`", "… the file `notes.txt` (`include_str!("../notes.txt")`); changed code `nine` → `ten`", "… the line of the call (`depot::here!(1)`)" (a proc macro that reads its span), "… a value that the macro makes at the call (`#[test]`)". A source of data (a file, a variable, the module path, the output of a proc macro) adds the changed text. Such a line goes only under an indirect fact of its body, under the fact of the read file, or only in the step section. A changed proc macro of your workspace owns its calls: "from `proc macro depot::here` (depot/src/lib.rs:4)". A call that a macro with no expansion of its own makes names the call in your file: "`make!{@inner …}` from `sum!(1, 2)` at `depot::total/tail` (src/lib.rs:12)". A changed definition whose calls all keep their expansion gets the proof line "expansion: each of the 2 calls that use `add!` has an equal expansion" (for example a renamed metavariable). The step section `expansions: 3 changed, 1 added, 12 equal` lists each call; `semilla status --verbose` has each one with its facts and its cause. A body line of the step section with "no edit in the body" at a changed expansion names it: "…; the expansion of `fee!(v)` changes (semilla expand src/lib.rs:4)". The lines ask for no reason. Each macro call has an expansion record, also a macro of the standard library (`format!`, `println!`, `assert!`, `vec!`, `#[test]`): a changed, an added, or a removed expansion shows, and an equal one is only a count in the section. An expansion that the printer of the compiler cannot print (design 66.11, for example a tree deeper than 1000 levels) is not known, and it is never equal to the other version: "`depot::deep!()` at `shop::total/tail/lhs` (src/lib.rs:6): expansion not known: not known: the printer of the compiler cannot print a tree deeper than the limit of the driver". The line shows in each step, also with no change of the call: read the macro and its input.
- The default mode has the expansion lines too (design 65.8): the build check of the step writes the expansion record of each macro call of the two trees, so `semilla step` with no `--compiled` shows the lines and the section `expansions: …` above. The build check then builds the tree before the step too (once; the cache keeps it). With `--no-check` (or `SEMILLA_NO_CHECK`), the step has no records, and one line says so: "expansions: not read: the build check did not run, so no build made the expansion records". A build check with a toolchain that is not the toolchain of Semilla says so in the same line ("expansions: not read: the build check uses the toolchain …"); `--compiled` or `semilla expand PLACE` then builds the records.
- A member order change of a type with a derive or an attribute macro needs a reason (design 65.3). With the expansion records, it needs none when each derive and each attribute macro of the type makes the same code in the two versions (design 65.9); the fact has the note "the member order needs no reason: each macro of the type (`Marker`) makes the same code in the two versions". A type with `#[repr]` or `cfg_attr`, an enum whose implicit discriminant values change, and a changed expansion keep the spotlight. A new order of the derives (`#[derive(Debug, Clone)]` → `#[derive(Clone, Debug)]`) needs no reason. An attribute macro that moves across a derive needs one: "an attribute macro moves across a derive (`#[depot::tag]`, `#[derive(Debug)]`): the macro gets the item before the derive or after it, so the input of the derive can change".
- A macro of a dependency (design 65.10): its changed expansion goes under the fact of the package in `Cargo.lock` when the version, the source, or the checksum of the package changes ("from the package `depot` (1.0.1 → 1.0.2 in `Cargo.lock`)"), and the equal calls of each of its macros are one count line under that fact: "expansion: 12 expansions of `derive depot::Rows` stay the same". With the same entry in `Cargo.lock`, an edit in the input of the call owns the line as for each other macro.
- A changed expansion with a text over 50 kB gets no compare. Its line gives the size and the command that prints the full text: "a large expansion (3000 → 3000 lines, 61.2 kB → 61.2 kB; 2 lines differ): the text is too large for a compare; the full text: semilla expand src/lib.rs:3". Run that command, and read the diff.
- A macro call in an `impl` block lists the methods that it makes: "added macro call `field_get!(total, sum)` (makes: `pub fn Cart::total`)".
- A text that a name list of the language decides says so, and it names the rule of the compiled mode (design 65.4): "added macro `matches` (it replaces the standard macro `matches!`; each call in its scope: `demo::f`; a name list of the language decided it (the macros of the standard prelude); the compiled mode reads the definition of each call from its expansion record (R65.2-1))". The spotlight of a new `impl Drop`, `Deref`, or `DerefMut` names the list too.
- `semilla expand PLACE` prints the expansion of each macro call at a place, before and after the step, with a line diff. `PLACE` is `FILE:LINE` (`FILE:-LINE` for the old tree), an item path, or a macro name with `!` (each call of the macro, also each call of a macro whose expansion calls it). The default step is the working tree against `HEAD`; `--step N` takes a stored step. `--before` or `--after` prints one side, and `--full` prints each line (the default cut is 200 lines). The command reads the records of the build check of a step of the default mode, else the compiled records; it builds the compiled records only if the cache has no record of the two trees. It needs no step with `--compiled`. Example:

  ```text
  $ semilla expand 'base!'
  `fee!(v)` at `shop::price/tail` (src/lib.rs:4): expansion changed
    definition: `macro_rules! fee` (src/lib.rs:2); uses: `base!` (src/lib.rs:1)
    --- before
    +++ after
    - (v + 10) * 2
    + (v + 12) * 2
  ```

  `semilla review --md FILE` has the section "Macro expansions": a part for each changed expansion with the call (a link), the definition, the facts, and the diff of the expanded code (equal changes fold; a long diff has a cut that points to `semilla expand`).
- The check builds the code in a sandbox with no network. If a dependency is not in the cargo cache, run `cargo fetch`. A changed manifest needs a new `Cargo.lock` in the same step: if the command says "`Cargo.lock` does not agree with the manifests", run `cargo update --workspace --offline`.

## When a reason is necessary

A fact in the spotlight needs `--reason 'ANCHOR=TEXT'`. The reviewer reads the reason; Semilla does not check it. A compare result never asks for a reason. A spotlight that reads the compiled code exists only with `--compiled` (the list marks each one). For an edit in an item, the anchor is the path of the item: `--reason 'ledger_core::fees::charge=the limit is new'`. For each other fact, the anchor is the one that the rejection prints: `--reason 'Cargo.toml:profile.dev.overflow-checks=the benchmark needs it'`, or `--reason 'ci.sh:=the CI runs it'`. A reason with no text is not accepted. Give the reason in the first call when the step has one of these:

- an attribute outside the allow list (`doc`, `allow`, `warn`, `deny`, `forbid`, `expect`, `must_use`, `inline`, `cold`, `deprecated`, `derive`, `rustfmt::`, `clippy::`, `diagnostic::`). So `ignore`, `should_panic`, `cfg`, `path`, `repr`, `no_mangle`, and each proc-macro attribute need a reason. A new item also allows `#[test]` and `#[cfg(test)]`;
- a manifest key outside the allow list (`package.version`, the descriptive `package` keys, a version change of a registry dependency), and each key of a cargo or rustup configuration;
- a file outside the allow list (`*.md`, `*.txt`, `LICENSE*`, `COPYING*`, `NOTICE*`, `CHANGELOG*`, `AUTHORS*`, images), a file mode, a symbolic link, a submodule; a Rust file that Semilla cannot parse ("Semilla cannot parse this file: …"; the other files get their usual facts);
- a build script (`--reason 'crates/core/build.rs:=<why>'`); a file that the code reads at build time (`include_str!`, `include_bytes!`, `include!`); with `--compiled` also a build input or a changed `macro_rules!` definition that changes bodies with no edit of their text: `--reason 'k::tests::t!=<why>'`;
- with `--compiled`, inactive code: "inactive code: no checked configuration compiles the changed code (…), so Semilla did not compare the edit". The fact has `no compare`. A file that no checked configuration compiles has one spotlight, and its key is the file: `--reason 'crates/shop-core/src/win.rs=<why>'`. Each edit of the file keeps its fact: "changed literal `1` → `0` at `socket2::sys#1::set_nonblocking/S0/init/then/tail` [semantics · no compare]";
- a changed literal with a new character outside ASCII (`"µs"` → `"μs"`); an `unsafe` or `extern` qualifier; an order change in a type with `#[repr]`, a derive (any name: `Debug` prints the fields in order), or an attribute macro; a file of a kind that no tool reads (`.txt`, `.md`) when the code reads it at build time ("a file that the code reads at build time (in `src/lib.rs`)"); a package key that the code reads with `env!("CARGO_PKG_…")` ("a package key that the code reads at build time (`CARGO_PKG_DESCRIPTION`)"); a removed argument with a side effect;
- a hand-written trait impl that replaces a derive; a new or removed `Drop`, `Deref`, or `DerefMut` impl; with `--compiled`, a rename of a type with a derive other than `Debug` (the anchor is the old name);
- with `--compiled`, an indirect change with no known cause, or code that Semilla does not see (generated code).

## A large step

- A step of the size of a pull request (500 to 800 changed lines) is a usual step. The text output puts the facts in this order: a build code change; each body edit (with `--compiled`: with `code different`); each declaration, added item, and removed item; each other code fact; the manifest facts; the docs facts, the file facts, and each other fact with no code. A fact keeps its number: `f6` can come before `f1`.
- One reason can cover each open spotlight of one kind in the step: `--reason 'kind:KIND=TEXT'`. The `run again:` line prints a kind reason for a kind with 4 open spotlights or more: `run again: semilla step 'update to the new version' --reason 'kind:new-attribute=<why>' --reason 'kind:cfg-attribute=<why>' --reason 'kind:inactive-code=<why>'`. Each covered spotlight says so: "reason (key `kind:inactive-code`; one reason covers 26 spotlights of the kind inactive-code)". The reason of an exact key goes first. The kinds: `cfg-attribute`, `new-attribute`, `changed-attribute`, `inactive-code`, `indirect-change`, `changed-macro`, `build-input`, `build-setting`, `manifest-key`, `package-setting`, `new-dependency`, `dependency-features`, `dependency-setting`, `other-file`. A changed file of build code needs its own reason.
- A spotlight line that is equal for a run of facts shows one time, after the last fact: "spotlight of f1 to f4: …". No list line has more than 500 characters: a cut says "`semilla status --verbose` shows the full line". A cut keeps each result word and its configurations: it hides items of a list of code names first (the inlined callees of a proof). A fact line is never cut. A text that many facts have shows one time, at its first fact: "(the same text for f10 to f39)" or "(the same list for f10 to f39)". Each other fact says "the same text as f10" or "the same list as f10". The diff of a manifest file shows one time, after the last manifest fact of the file. A "build input" spotlight shows five causes, then "and 25 more (`semilla status --verbose` shows each cause)". A fact shows its full text. A text of more than 80 characters, or with more than one line, goes in a block below the fact line: the line has the reference `[1]`, and the block starts with `[1]`.
- With `--compiled`, a feature that only one tree has gives no compare for its configurations, and the step does not fail: "! configurations `dev, features serde1`, `release, features serde1` are not checked: only the code before the step has the feature `serde1`. The code after the step has no such configuration: no compare". With `--compiled`, changed build code makes a cache entry of 500 MB or more for each tree. After each step, Semilla removes the entries that no stored step uses, and it keeps the 4 newest of them. `semilla cache --clean` removes each such entry: "removed 4 entries of the build code cache that no stored step uses: 4 kB freed".
- The time to expect, from ten real crates with 300 to 1700 changed lines of Rust code: a run takes 0.1 to 0.9 s in the default mode with `--no-check`. The build check adds 0.1 to 3.6 s for the first step of a repository (cargo checks each dependency), and 0.1 to 0.6 s for each next step. With `--compiled`, the first run takes 0.5 to 23 s with an empty cache; the `cargo check` of the two trees uses most of the time. After an accepted step, `semilla review --md` takes less than 1 second.

## The results and the actions

| Result | Action |
|---|---|
| `accepted` | Nothing. The result is final. |
| `rejected … needs a reason` | Run the printed command with the `--reason`. |
| `rejected …`, then `✗ build check: fails …` | The code after the step does not build. Correct the code and run the step again. Use `--no-check` only for a step of work in progress. |
| `not stored … the build check cannot run on this machine` | Correct the environment (the cause is in the message), or run the printed command with `--no-check`. |
| `build check: no new failure found …`, then `! build check: the target …` | The step is stored. A target does not build, and Semilla did not check a new error in it. Read the line. If your step made the error, correct it in the next step. |
| `build check: not run …`, `! build check of step N: …` | Nothing. The reviewer reads the line. |
| `✗ fact a2 of the agent is false: …` | The tag does not agree with the edit that Semilla sees. Read the lines "Semilla sees: …" below the fact. Correct the fact, or run the printed command (it has no false fact). Do not change the code to make a fact true. |
| `not stated: N item(s) …` | The step is stored. State the gaps: run the printed `semilla fact …` command with your sentence in place of each `TEXT`. Remove an option that is not true for your intent. |
| `refuted` | The claim is false. Run the printed command (without the claim), or fix the code. |
| `malformed` | The anchor or an argument is wrong. The message names it. A `tested-by` test must have `#[test]` and no `#[ignore]`. |
| `the claim … is gone` | Run the command again without the claim. |
| `note: …` or `! the compiled code changes: …` (`--compiled`) | Nothing. The reviewer reads the line. |
| `control flow: …` (`--compiled`) | Nothing. The reviewer reads the line. If you expect no change of the control flow, check the body that the line names. |
| `expansion: …` | Nothing. The reviewer reads the line. If you expect no change of the code that a macro makes, run `semilla expand PLACE` for the call. A line "a large expansion (…)" has no compare: run the `semilla expand` command of the line. |
| `expansions: not read: …` | Nothing, if you turned the build check off. The step has no expansion lines. Run the step with the build check (no `--no-check`) to get them. |
| `docs of …, no edit in this step …`, `comments: N comment(s) …` | The step is stored. Read each such comment. Correct a comment that is not true for the new code in the next step. |
| `error: … no change since the last step` | Edit the code first. |
| `! build code changed in step N: …` | Give the reason. With `--compiled`, each result `code equal` and each verified claim from step N on has the mark `(with the build code of step N)`. If possible, put the build code change in its own step. |
| `! the build ran with no sandbox: …` (`--compiled`) | A limit of the build sandbox is missing on this machine. The result stays. Do nothing for it. |
| `tag` rejects | "result: rejected" is the first line, then each reason with its step, then the exact commands of the fix. Run them in order. Examples: `commit … has no step`, `the commit HEAD does not have the changes of the last step` (run `git add -A`, `git commit --amend --no-edit`, `semilla tag`), `an edit after the last step has no step`. |
| `the note does not agree with the step refs` | Someone changed the git note or the step refs after `tag`. Do the change set again. |
