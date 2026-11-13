# semilla

Semilla makes code review faster. An agent tags its work with **claims**: a list of semantic changes. Semilla **verifies** each claim against the code. A reviewer then reads verified changes in a local UI, with flow graphs and a change tree, instead of a line diff.

The design is in [`docs/design.md`](docs/design.md). The first language is Rust. Semilla is local: it does not push and does not use a remote service.

## Build

```sh
cargo build                 # the workspace uses the pinned nightly in rust-toolchain.toml
scripts/fetch-z3.sh         # optional: installs Z3 in .semilla/tools/ of this workspace for the SMT checks
```

The binaries are in `target/debug/`:

- `semilla`: the command line, the MCP server, and the review UI.
- `semilla-mir-driver`: the rustc driver that gives the MIR normal forms. `semilla` finds it next to its own binary, or through `SEMILLA_MIR_DRIVER`.

Semilla finds the solver in this order: `SEMILLA_Z3`, a `z3` next to the Semilla binary or in the `tools` folder of the user cache, a `z3` on `PATH`, and the `.semilla/tools/z3` of this workspace. It does not use a `z3` in the repository that it checks.

## The agent workflow

The agent keeps its own edit tools. After each small change, it runs one command with the intent (design 24):

```sh
# ... edit the code: return 0 for an empty cart ...
semilla step "return 0 for an empty cart"
# ... edit the code: rename the field `Cart.items` to `Cart.values` (and nothing else) ...
semilla step "rename Cart.items to values"   # Semilla derives and proves the rename: no claim
# at the end
git commit -am "fix the cart total"
semilla tag                               # the final check, the result line, and the git note
```

- Semilla derives a fact for each change: "added early exit `if …`", "changed operator `<` → `<=` (effect hint: widen)", "parameter of `f` added", "`f`: neutral rewrite (proved N2)". Equal facts fold into one line ("6 site(s): changed expression …"). The agent writes no claim for them.
- A rename or a move in a step with no other change gets a proved fact: "renamed `A` → `B` (proved N1: rename-map)" or "moved `A` → `B` (proved N1)". If the step has other changes, the two items show as removed and added, with a note: put the rename in its own step to get a proof.
- A rejected step stores nothing. The first lines say why, and the last line is the command to run again, for example with the missing `--reason 'ANCHOR=TEXT'`.
- The optional claims: `rename` and `move-item` (only when Semilla finds no pair or more than one name map), `rewrite-pattern`, and the property claims `no-panic`, `no-unsafe`, `no-io`, `calls-only`, `tested-by`. Example: `semilla step "add sum_wide" --claim 'no-io shop_core::cart::sum_wide'`. An explicit claim is an assertion: if it is false, the step is rejected. `tested-by` is `verified (mir)` only if each path of the test from its start to its normal return calls the function (design 30.3, batch 5w). A reachable call that is not on each path (under a condition, in a loop, after an early return or a `?`) gives `verified (partial)`: Semilla did not prove that the test runs the call. No reachable call refutes the claim. Semilla does not run the test.
- `--group "TITLE"` puts steps with the same title in one group. A step with no group is its own group.
- `semilla undo` removes the last stored step. `semilla status` prints one line for each step and the result line; `--verbose` adds the facts, and `--wait` waits for phase 2. `semilla status <commit>` waits for phase 2 by default (at most 600 seconds, or `--wait SECONDS`); `--no-wait` prints the present state. It never prints a result from a git note or from a file of the repository folder. It computes phase 1 again. It uses a phase 2 result only from the cache folder of the user (see "What Semilla trusts"), and it computes the result if the cache has none. `semilla tag` stores its result there, so `semilla status <commit>` right after `tag` prints the result at once.
- The result line has the counts that the reviewer reads, and no score: `result: accepted · 2 steps · 3 facts: 1 proved neutral, 2 to read (0 with a spotlight) · 0 claims`.
- The first step and the reports show the base of the change set. If commits before the change set have no Semilla check, a line names them (design 25.2).
- After `semilla tag`, the change set is closed: the next `semilla step` starts a new change set at `HEAD`. If `HEAD` is not the tagged commit, `semilla status` says if `HEAD` has a Semilla check.
- `semilla step --fast` runs phase 1 only. Each later step prints the phase 2 state of that step. If `tag` rejects an earlier step, it prints the exact commands of the fix.

The first argument of `semilla step` is always the intent: `step` has no subcommand. The commands of the old protocol (`old-step`, `claim`, `check`, `group`) are only for a replay by hand. `semilla --help` hides them. The replay:

```sh
semilla replay begin                      # the work is the target; the working tree is HEAD again
semilla replay plan                       # a draft plan in .semilla/plan.json: steps, takes, optional claims
semilla replay apply --full               # start from the base, run all steps, check everything
semilla replay end                        # the working tree is the target again
```

`semilla replay begin --commit <rev>` replays a commit that has no change set (its parent is the base).

Agents: read [`docs/agent-guide.md`](docs/agent-guide.md) first.

### MCP

`semilla mcp` serves the same tools over MCP (stdio). For Claude Code:

```sh
claude mcp add semilla -- /path/to/semilla/target/debug/semilla mcp
```

The tools are `step` (the same fields as the command, as a JSON object: `intent`, `group`, `claims`, `reasons` as an object from anchor to text, `class`, `tree`), `undo`, `tag`, `results` (the `status` command), `ops`, `verify`, `pair_refuse`, and the replay tools `replay_begin`, `replay_status`, `replay_plan`, `replay_apply`, `replay_end`, `replay_abort`. A rejected `step` is a tool error with the cause and the call to make again. The tools of the old protocol are not served.

### Feature sets

Semilla checks each step in a list of build configurations (design 8.12): the profiles `dev` and `release`, each with the default features, no default features, and all features. To check more feature sets, list them in the root manifest:

```toml
[workspace.metadata.semilla]   # or [package.metadata.semilla]
feature-sets = [["serde"], ["std", "unicode"]]
```

A body is `neutral` only if it is equal in each checked configuration. Semilla checks one target: the target of this machine. Each reviewer output shows the list near the base line:

```
checked configurations (target `x86_64-unknown-linux-gnu`): `dev`, `release`, `dev, all features`, `release, all features`
! configurations `dev, no default features`, `release, no default features` are not checked: it does not build before the step: error[E0599]: …
! `cfg` conditions that Semilla did not check: `docsrs`, `feature = "alloc"`, `feature = "std"`
```

If a configuration does not build with each target before the step, Semilla checks its libraries and binaries only, and the header says so: "`dev, no default features` (libraries and binaries only: the other targets do not build)". If this smaller build fails also, the configuration is not checked, and the output names it. A step that breaks a configuration is rejected as a compile error. A proof that uses two conditions or more needs each joint state in the list. If a state is missing, the body is not `neutral`, and the spotlight gives the fix:

```
Not checked together: `feature = "fast"` on with `feature = "slow"` off. Add `feature-sets = [["fast"]]` to `[workspace.metadata.semilla]`
```

## The review

```sh
semilla status HEAD                    # phase 1 again, and phase 2 from the user cache (it starts the check if necessary, and waits for it)
semilla status HEAD --no-wait          # the same, but it does not wait: a fact with no result shows as `pending`
semilla --fresh status HEAD            # read no cache: compute each record again (a full build)
semilla status HEAD --verbose          # the same, with each fact of each step
semilla review HEAD                    # the review UI on http://127.0.0.1:7878
semilla review HEAD --md review.md     # a short Markdown report: the result, the groups with their facts, the flow views of the changed logic
semilla review HEAD --md review.md --full   # also the change tree, the public API changes, all flow views, and all sources
semilla review HEAD --html review.html # a self-contained HTML report (works offline)
semilla load HEAD && semilla show      # the change tree in the terminal
```

The UI shows the change set by group and subgroup, the change tree, flow views (Mermaid), the workspace map (Cytoscape), and search. The change tree is one diff from the base to the head of the change set; a node shows the steps that changed it. The review state is in the cache folder of the user (`review.json`), and it follows the claims, not the lines.

## What Semilla trusts

The agent controls each file of the repository folder: the working tree, the git objects, the refs, the notes, `.git/config`, `.git/info`, `.git/hooks`, and `.semilla/`. The agent does not control the machine outside the repository folder. Code of the change set runs during the check (a build script, a proc macro). It must not change the result.

A result for a reviewer is each output of `review`, `ui`, `load`, `status <commit>`, and `verify`. The rule (invariant I5, design 27): no result for a reviewer depends on the content of a file of the repository folder that is not a git object. One limit stays: build code can test if a path exists (see the limits of the sandbox below).

**Build code.** Build code is a build script, a proc macro crate, a host package, or a cargo or rustup configuration file. It runs during the check and can change each result. Semilla tests if the change set changes the build code (design 28). The rule is "run and mark":

- **Each command runs the changed build code in the sandbox**, in step order, in a separate folder set. This is the default for the agent and for the reviewer. No option and no approval is necessary.
- **The build code change comes first.** The step that changes build code shows that change in its own section before its other facts, with the spotlight and the diff. Each reviewer output starts with the line `! build code changed in step N (…)`. Read that change before the proofs.
- **The mark.** Each proved fact and each verified claim from step N on has the mark `(with the build code of step N)`, and the counts say how many proofs have it: `2 proved neutral (1 with the build code of step 1)`. The result line has ` · build code changed from step N`. Invariant I6: no output shows a proof that used changed build code without the mark.
- **The careful option.** With `--no-build-code` (on `status`, `review`, `ui`, `verify`), the command does not run changed build code. The steps from step N on are `not checked`, the result is `not complete`, and the exit code is 1. The old option `--trust-build-code` is gone: the command prints one line and runs the build code.
- **The base line.** The base of a change set can have build code that a commit shortly before it changed. Each proof ran that code, and no mark shows it. So the header names the commit and the files: ``! the base has build code that a recent commit changed: `abc1234` (crates/core/build.rs). …``. Semilla examines the 20 commits before the change set, and the output says so when the history is longer. The line is information only.
- **The limit.** The sandbox is the protection (see below). The sandbox does not make a proof independent of the build code. A build script can change each proof from its step on, and the mark says so.
- **With no full sandbox.** A reviewer command runs changed build code only if the sandbox has its three limits. If a limit is missing (an old kernel, a different operating system, `SEMILLA_NO_SANDBOX`), the reviewer command does not run changed build code: one line says why, and the result is that of `--no-build-code`. The option `--build-code-no-sandbox` runs it anyway. A variable does not have this effect, because the agent can control the environment. An agent command continues to run and prints the line "the build ran with no sandbox: …".

- **Git objects are data.** Semilla reads each file of a tree as a raw blob. No git command runs a hook, a filter, or a program that the repository configuration names. A git note and a step ref are claims: Semilla computes each fact and each proof again.
- **The text of the agent is data.** An intent, a group title, a reason, a path, an identifier, and a literal can have each character. Each output shows a control character, a bidirectional control, and a zero-width character as an escape (`\n`, `\u{1b}`, `\u{202e}`), so this text cannot start an output line or hide one. Text in quote marks is the text of the agent. Semilla does not check it. Each other label of a reviewer output (the group kind, the class, the counts) comes from the facts, not from the note. An identifier with a character outside ASCII shows its escape form and is in the spotlight. A literal with a character outside ASCII (a string, a raw string, a byte string, a char) shows its escape form after it: `` `"μs"` (`"\u{3bc}s"`) `` (design 30.2, batch 5w). A fact that changes a literal is in the spotlight and needs a reason if the new literal has a character outside ASCII that the old literal does not have. The text names each such character with its escape form. A new literal in new code gets a note with the form, and it needs no reason.
- **The tools are outside the repository folder.** These are `git`, `cargo`, the pinned toolchain, the MIR driver, and the solver. If a tool is in the repository folder, the result has a warning line.
- **The cache is outside the repository folder.** The MIR records, the stored results, and the review state are in `$SEMILLA_CACHE_DIR`, or `$XDG_CACHE_HOME/semilla`, or `~/.cache/semilla`. If Semilla cannot write the folder, it works with no cache and prints one note.
- **`--fresh` reads no cache.** It computes each record again in a temporary folder. Use it when the agent could write outside the repository folder.
- **Each check build runs in a sandbox** with three limits (design 27.6, 27.12, 27.13). Each process of the build has them: `cargo`, the compiler, each build script, and each proc macro.
  - **The write limit** (Landlock, Linux 6.2 or later). The build writes only its target folder and its records folder in the cache.
  - **The read limit** (Landlock). The build reads only its build folder (the files of the tree), its target folder and records folder, the toolchain, the registry and the git folders of the cargo home, the cargo configuration files outside the tree, the MIR driver, and the system folders of the compiler and the linker. It does not read the repository folder, `.git`, the home folder, the credentials file of cargo, a different cache folder, `/tmp`, `/sys`, or `/proc`. If the code names a file with an absolute path, the error says that the file is outside the build folder.
  - **The network limit** (a seccomp filter, `x86_64` and `aarch64`). The build makes no socket: no TCP, no UDP, no raw socket, no netlink, no UNIX socket of the user. Cargo runs with `--offline`. If a dependency is not in the cargo cache, the error tells you to run `cargo fetch`.
- **A build has a fixed environment** (design 27.11). No variable of the Semilla command goes to the build. `PATH` is the toolchain folder and the system folders, so a folder of the repository gives no program to the build. A proof does not use the value of `env!`, `option_env!`, `file!`, `line!`, `column!`, `module_path!`, `include_str!`, or `include_bytes!` (design 27.14).
- **A folder that a build can write serves one build code state** (design 28.8 to 28.10). Two builds share a target folder only if they run the same build code. The records cache gets only the record file of each unit of the build; the output names each other file. No process of a build continues after the build.
- **If a limit is missing**, each result has a line that names each missing limit: "the build ran with no sandbox: the write limit, the read limit, and the network limit are missing (…). A build script or a proc macro of this change set can change the result". A record of such a build is never read by a command with the full sandbox.
- **The limits of the sandbox that stay.** Build code can write the record of its own build unit: such code is build code of the base, or its proofs have the mark. A build can test if a path of the machine exists, and it can read the size and the times of that path: Landlock has no rule for this. A build cannot read the content of such a file, and it cannot list a folder. So build code can give a different result when an untracked file exists. The code that does this test is build code: the base line or the mark names it, and the reviewer reads it. A build can send a signal to a process of the user. A build can measure time, so a covert channel through time stays. A build reads each file of the cargo registry and of the toolchain; Semilla trusts their content. An old folder set stays on the disk: Semilla removes none.
- **Limits.** Semilla trusts the kernel, the toolchain, the cargo home folder, and the cache folder. For a `derive` in a `cfg_attr`, Semilla trusts that a name of its allow list (the built-in derives, `Serialize`, `Deserialize`) is the built-in derive or the derive of `serde`, if the workspace does not define a derive macro with that name and no import of the module gives the name a different meaning (a `use`, a glob `use` of a crate outside the workspace, a `#[macro_use] extern crate`). A test reads the members of each known trait and the output of each such `derive` from the compiler. A base commit of the branch is a trusted base, so its build code is not checked (design 28). Semilla does not check that a git object agrees with its hash (`git fsck` does). The session file `.semilla/session.json` is a claim of the agent: `tag` and `status <commit>` check each step again. A proof of `neutral` holds in each checked configuration, and the outputs name them. It does not hold for a different target, for a feature set that is not in the list, or for a `cfg` condition that the outputs list as not checked: the rules for these conditions are a best-effort layer and are not complete (design 8.2). Semilla trusts that each type of the standard library is equal in each build of one toolchain and one target. The named platform aliases (`c_long`) are not trusted. A type of a different crate outside the workspace is not trusted: a body that uses such a type gets no proof (design 8.2).

## What the verifier checks

For each step, the verifier:

1. Applies the renames (claimed or derived), and computes the syntax edits down to the expression level.
2. Checks each claim: the shape of its edit, or a body proof with the MIR normal forms (N0 to N4), or an SMT check of a condition.
3. Derives a fact for each edit with no claim, so no edit is uncovered, and rejects each claim without an edit (**empty**). Each changed path of the git tree gives an edit, also a binary file and a file mode. A step with no identity claim gets a derived rename or move when exactly one name map passes the same proof as a claim (design 24.2, rule 4).
4. Gives each body whose MIR changed an owner in the report (the **MIR backstop**). A body whose MIR changed when its text did not (an **indirect change**) goes under the fact or the claim that causes it by evidence: the callee list, a named const or type, an included file, an expanded macro, a new method, a build input (a build script or a manifest key that can reach the crate of the body). With no known cause, it gets the fact "indirect change, cause not known" in the spotlight. The agent cannot name the affected bodies: the argument `affects` is retired.
5. Derives the type-level changes as **facts** (fields, variants, signature parts, visibility, attributes): the agent writes no claim for them, and each fact owns its forced edits (call sites, struct literals, patterns). A step with a `rename` or a `move-item` must be neutral: put each in its own step. A file with no parsed Rust code (`Cargo.toml`, `Cargo.lock`, a script, a data file) gives manifest facts or a file fact: one fact for each changed key. A code block in a doc comment is a doc test: its change is a fact with the class `logic`. The Rust roots come from `cargo metadata`: a build script is the file that `package.build` names, with each name. A fact in the spotlight (a `profile` key, a new dependency, a file mode, …) needs a reason: `semilla step "…" --reason 'ANCHOR=TEXT'`.
   The spotlight uses allow lists: each attribute and each manifest key that is not known as harmless needs a reason (for example `#[ignore]`, `#[path]`, `workspace.default-members`), on a changed item and on a new item. A manifest key that changes the MIR of a body needs a reason too, also `package.description`.
6. Compares the declared class of the step with the computed class.
7. Checks each property claim on the local code that can run from the function, with the trait impls and the drop bodies, and prints what it examined and what it did not (design 25.1).
8. Reports the change in explicit panic sites, and puts `unsafe` code, `#[cfg]` attributes, features, build scripts, and edits in code that no checked configuration compiles in the **spotlight**. "No MIR change" is evidence only for code that a checked configuration compiles (design 30.1, batch 5w). An edit of tokens that no checked configuration compiles has no proof. Its fact is never `neutral`: it is a fact to read with the spotlight "build configuration", the text shows the `cfg` condition, and the step needs one reason. The rule applies to each fact kind (a constant, a static, a type alias, a field, a variant, a `use` line, a macro definition, a body, an added item, a removed item). The `cfg` attribute can be on the item or on a parent: a module, a file module, an `impl`, a trait, a function, a block, a statement. An edit of only a comment stays free.
9. Proves `neutral` in each checked configuration (design 8.2 and 8.12; decided by the user on 2026-11-12). Semilla builds the two trees of a step in each configuration of the list and compares the MIR in each one. A body with a proof in only some of the configurations is not `neutral` (batch 5u). Its fact keeps its class, the reviewer reads it, and the step needs no reason. The fact has a note: "note: proved neutral in `dev`, `dev, all features`; not proved in `release`, `release, all features`". Semilla found no proof there; it did not prove a difference. Semilla makes no promise for a configuration that is not in the list. A profile other than `dev` and `release` (a custom profile, or profile settings from outside the repository) is not a checked configuration. A compiler flag that is not a `cfg` condition (for example the overflow checks) follows the profile. For a `cfg` condition that the checked configurations do not cover (`target_os`, `target_pointer_width`, a `--cfg` flag that is on in each build, a feature that is on in each build), the rules of batches 5k to 5q stay as a best-effort layer. This layer is not complete: a change that only a different target shows can be `neutral`. A condition at a place in a file is covered if one checked compile unit that reads the file makes it true and one makes it false. The values come from the compiler, so a flag in `rustflags` or a `cargo:rustc-cfg` line of a build script counts. No name has a special rule: `test` is not covered in a file under `tests/`. A proof that uses more than one covered condition holds only if the checked units have each joint state of the conditions that can occur (design 8.2, batch 5t). If a state is missing, Semilla adds a configuration with that state, builds it, and does each check of the step again (batch 5u). The limit is 4 added feature selections for one step. An added configuration is equal to a listed one: each proof of the step must hold in it, and a step that breaks its build is rejected. The header of each reviewer output shows it with a mark: "`dev, features cb58` (added for a proof)". If a state is still missing, the body keeps the spotlight, and the text names the state and the `feature-sets` entry that adds it. The key `feature-sets` stays as the manual way. When Semilla removes a proof because of a build condition and the fact has no spotlight, the fact has a note with the cause: "note: no proof: `ledger_core::fees::use_h` uses `feature = "fa"`, and Semilla did not check that condition". The rules of the layer: the normal forms do not fold a value that depends on the build: `cfg!`, `debug_assertions`, the width of `usize`, a layout query, `option_env!`. A changed body with a `cfg` attribute that is not covered, or with a macro or an attribute that Semilla cannot see into, has no proof: it gets the spotlight "build configuration" and needs a reason. The same applies to each input of a proof from a different item: a static, a callee, a name, or a type that the build configuration selects or fills is opaque, and no normal form looks through it. A changed body that uses such an item has no proof. A use is exact (design 8.2, batch 5o): the compiler resolves the code of the body to the item, or a name of the body can mean the item by its module or its type. A name that a macro puts into the body counts too: the names come from the source text and from the expanded code (batch 5p). A method call is a use if a different method with its name can win in a different configuration, and a trait fact of the workspace depends on the configuration (batch 5q): the compiler gives each such rival. The word alone is not a use: `Default::default()` does not use a module `default` under `cfg`. A call of a function under `cfg` with the same signature in each configuration stays an opaque call, so a rewrite next to it keeps its proof. A `cfg_attr` with only a lint, `doc`, `inline`, `cold`, `must_use`, or `deprecated` content is free. A new use in the changed code gets the spotlight, and "Uses: …" names the item. A use in code with no change gets no spotlight. The SMT check of a condition runs for each pointer width, with and with no overflow checks.

`semilla index` writes the index of the working tree to `.semilla/index.json`: the workspace graph, the items, and the MIR content hash of each body.

For the change set, it also checks the group and subgroup rules, and it checks that the git note agrees with the step refs. The result is accepted or rejected, with the counts of the facts: proved neutral, to read, and with a spotlight. It has no score.

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
