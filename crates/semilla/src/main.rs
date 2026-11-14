//! The semilla command line.

// Each print of the CLI goes through the escape of `semilla_core::text` (batch 5k, A2): a
// control character of the agent or of the repository never gets to the terminal. The line
// breaks stay: the code that builds a line uses `put_line`. A print of JSON uses `std::println!`,
// because the JSON escape is sufficient there.
macro_rules! println {
    ($($arg:tt)*) => { std::println!("{}", semilla_core::text::block(&format!($($arg)*))) };
}
macro_rules! print {
    ($($arg:tt)*) => { std::print!("{}", semilla_core::text::block(&format!($($arg)*))) };
}
macro_rules! eprintln {
    ($($arg:tt)*) => { std::eprintln!("{}", semilla_core::text::block(&format!($($arg)*))) };
}

mod app;
mod background;
mod buildcode;
mod git;
mod mcp;
mod onestep;
mod oracle;
mod plan;
mod replay;
mod store;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use semilla_core::{GroupKind, Subgroup};

use crate::app::App;

#[derive(Parser)]
#[command(name = "semilla", version, about = "Verified semantic changes for code review.", after_help = "The loop: edit the code, then `semilla step \"<intent>\"`. At the end: `git commit`, then `semilla tag`.\nThe commands of the old protocol are hidden. Only a replay by hand uses them (see `README.md`).")]
struct Cli {
    /// The repository folder (default: the current folder).
    #[arg(long, global = true)]
    repo: Option<PathBuf>,
    /// Read no cache: compute each record again in a new temporary folder (a full build). Use it
    /// when the agent could write outside the repository folder (the cache folder of the user).
    #[arg(long, global = true)]
    fresh: bool,
    /// For a reviewer command on a machine with no full sandbox: run the build code that the
    /// change set changes with the limits that the machine has. The default does not run it.
    #[arg(long, global = true)]
    build_code_no_sandbox: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Rebuild finished work as a series of checked steps.
    #[command(display_order = 20)]
    Replay {
        #[command(subcommand)]
        action: ReplayAction,
    },
    /// Refuse a pair warning (a removed and an added item that look like one changed item).
    #[command(display_order = 21)]
    Pair {
        #[command(subcommand)]
        action: PairAction,
    },
    /// Check and store one step: `semilla step "<intent>" [--group T] [--claim '…'] [--reason 'ANCHOR=TEXT']`.
    ///
    /// The first argument is always the intent. `step` has no subcommand (batch 5g): each word is
    /// a valid intent, also `edit` or `end`.
    #[command(display_order = 1)]
    Step {
        /// The intent of the step.
        intent: Option<String>,
        /// The group title. Steps with the same title make one group.
        #[arg(long)]
        group: Option<String>,
        /// An explicit claim: `OP ANCHOR key=value …`. The anchor can be `FILE:LINE[:COLUMN]`.
        #[arg(long = "claim")]
        claims: Vec<String>,
        /// The reason for a fact in the spotlight: `ANCHOR=TEXT`.
        #[arg(long = "reason")]
        reasons: Vec<String>,
        /// Also print the change tree.
        #[arg(long)]
        tree: bool,
        /// Run only phase 1 now, and phase 2 in the background (for a large project). Without it,
        /// the command runs phase 2 and prints the final result of the step.
        #[arg(long)]
        fast: bool,
    },
    /// Remove the last stored step (the working tree keeps its changes).
    #[command(display_order = 2)]
    Undo,
    /// (old protocol, for the replay) Open and end a step by hand: `semilla old-step begin "<intent>"`, `semilla old-step end`.
    #[command(hide = true, name = "old-step")]
    OldStep {
        #[command(subcommand)]
        action: StepAction,
    },
    /// (old protocol, for the replay) Add a claim to the open step: `semilla claim add-if --at demo::f/S0 cond="x > 0"`.
    #[command(hide = true)]
    Claim {
        op: String,
        #[arg(long)]
        at: Option<String>,
        /// Arguments as `key=value`. A value that is valid JSON (a number, a list) stays JSON.
        args: Vec<String>,
    },
    /// (old protocol, for the replay) Remove a claim from the open step.
    #[command(hide = true)]
    Unclaim { id: String },
    /// (old protocol, for the replay) Give the reason for a fact in the spotlight: `semilla reason --at ANCHOR "text"`. The
    /// reason is free text for the reviewer, stored with the open step. An empty text removes it.
    #[command(hide = true)]
    Reason {
        #[arg(long)]
        at: String,
        text: String,
    },
    /// (old protocol, for the replay) Print the anchor at a file position: `semilla anchor src/lib.rs:12[:8]`.
    #[command(hide = true)]
    Anchor {
        position: String,
        /// Read the position in the tree before the open step (for claims on existing code).
        #[arg(long)]
        before: bool,
        /// Go this many levels up from the lowest node (for example from `cond/lhs` to `cond`).
        #[arg(long, default_value_t = 0)]
        up: usize,
    },
    /// (old protocol, for the replay) Verify one step (default: the last or open step). Without `--full`, only phase 1 runs:
    /// the syntax checks; the compare results are pending and the MIR checks run in the background.
    #[command(hide = true)]
    Check {
        #[arg(long)]
        step: Option<usize>,
        /// Also run phase 2 now (the MIR checks).
        #[arg(long)]
        full: bool,
    },
    /// Show the check of the session, or of a tagged commit. Phase 1 runs again; phase 2 comes
    /// only from a check on this machine (else it starts now). With a commit, the command waits
    /// for phase 2 (at most 600 seconds).
    #[command(display_order = 4)]
    Status {
        commit: Option<String>,
        /// Wait at most this many seconds for phase 2. With a commit, this is the default.
        #[arg(long, num_args = 0..=1, default_missing_value = "600")]
        wait: Option<u64>,
        /// With a commit: do not wait for phase 2. Print the present state.
        #[arg(long, conflicts_with = "wait")]
        no_wait: bool,
        /// Print the facts and the claims of each step.
        #[arg(long)]
        verbose: bool,
        /// Do not run the build code that the change set changes (a build script, a proc macro
        /// crate, a cargo configuration file). The steps from that change on get no check.
        #[arg(long)]
        no_build_code: bool,
        /// (removed) The command runs the build code by default.
        #[arg(long, hide = true, conflicts_with = "no_build_code")]
        trust_build_code: bool,
    },
    /// Review the session or a tagged commit: the UI, or a standalone report.
    #[command(display_order = 5)]
    Review {
        /// `session` (default) or a commit.
        target: Option<String>,
        /// Write a Markdown report to this file.
        #[arg(long)]
        md: Option<PathBuf>,
        /// Write a standalone HTML report to this file.
        #[arg(long)]
        html: Option<PathBuf>,
        /// With `--md`: add the change tree, each flow view, and each source.
        #[arg(long)]
        full: bool,
        /// Wait at most this many seconds for the background check.
        #[arg(long, default_value_t = 600)]
        wait: u64,
        #[arg(long, default_value_t = 7878)]
        port: u16,
        /// Do not run the build code that the change set changes (a build script, a proc macro
        /// crate, a cargo configuration file). The steps from that change on get no check.
        #[arg(long)]
        no_build_code: bool,
        /// (removed) The command runs the build code by default.
        #[arg(long, hide = true, conflicts_with = "no_build_code")]
        trust_build_code: bool,
    },
    /// Run the background jobs, then stop (normally started by a step and by `tag`).
    #[command(hide = true)]
    Worker,
    /// Check the change set of the session of this folder (a result for the agent). It takes
    /// no commit: for the result of a commit, use `semilla status <commit>`.
    Verify {
        /// (not valid) `verify` takes no commit.
        #[arg(hide = true)]
        commit: Option<String>,
        /// Require groups (the final check before a review).
        #[arg(long)]
        r#final: bool,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// Do not run the build code that the change set changes (a build script, a proc macro
        /// crate, a cargo configuration file). The steps from that change on get no check.
        #[arg(long)]
        no_build_code: bool,
        /// (removed) The command runs the build code by default.
        #[arg(long, hide = true, conflicts_with = "no_build_code")]
        trust_build_code: bool,
    },
    /// (old protocol, for the replay) Suggest claims for the uncovered changes of the last step.
    #[command(hide = true)]
    SuggestClaims,
    /// (old protocol, for the replay) Declare a group.
    #[command(hide = true)]
    Group {
        id: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        why: String,
        /// The step indexes, for example `0,1`.
        #[arg(long, value_delimiter = ',')]
        steps: Vec<usize>,
    },
    /// (old protocol, for the replay) Show the subgroup suggestion of a group, or set the layout from a JSON file.
    #[command(hide = true)]
    Subgroups {
        group: String,
        #[arg(long)]
        set: Option<PathBuf>,
    },
    /// Build the index of the working tree in `.semilla/index.json`.
    Index,
    /// (old protocol, for the replay) Print the change tree.
    #[command(hide = true)]
    Show,
    /// After `git commit`: the final check with phase 2, the result, and the git note (default HEAD).
    #[command(display_order = 3)]
    Tag { commit: Option<String> },
    /// Load the change set of a tagged commit for a review.
    Load { commit: String },
    /// Start the local review UI.
    Ui {
        #[arg(long, default_value_t = 7878)]
        port: u16,
        /// Do not run the build code that the change set changes (a build script, a proc macro
        /// crate, a cargo configuration file). The steps from that change on get no check.
        #[arg(long)]
        no_build_code: bool,
        /// (removed) The command runs the build code by default.
        #[arg(long, hide = true, conflicts_with = "no_build_code")]
        trust_build_code: bool,
    },
    /// Serve the agent tools over MCP (stdio).
    Mcp,
    /// Print the optional claims, or the details of one.
    #[command(display_order = 6)]
    Ops { name: Option<String> },
}

#[derive(Subcommand)]
enum PairAction {
    /// Say why the removed item and the added item are really two items.
    Refuse {
        old: String,
        new: String,
        #[arg(long)]
        reason: String,
    },
    /// Remove a refusal.
    Unrefuse { old: String, new: String },
}

#[derive(Subcommand)]
enum ReplayAction {
    /// Save the work (or a commit) as the target, and put the base tree in the working tree.
    Begin {
        /// Replay this commit. The base is its parent.
        #[arg(long)]
        commit: Option<String>,
    },
    /// List the hunks that remain between the working tree and the target.
    Status,
    /// Write a draft plan of all steps (JSON), from the base to the target.
    Plan {
        /// The plan file (default `.semilla/plan.json`). `-` prints the plan.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Apply a whole plan: start again from the base, run each step, and check the change set.
    Apply {
        /// The plan file (default `.semilla/plan.json`).
        plan: Option<PathBuf>,
        /// Also run phase 2 now (the MIR checks), and wait for it.
        #[arg(long)]
        full: bool,
    },
    /// Copy a part of the target into the working tree: `FILE`, `FILE:LINE`, `FILE:START-END`
    /// (target lines), or an item path.
    Take { spec: Vec<String> },
    /// End the replay. The working tree must be the target.
    End,
    /// Stop the replay, remove its steps, and restore the working tree.
    Abort,
}

#[derive(Subcommand)]
enum StepAction {
    /// Start a step.
    Begin { intent: String },
    /// End the open step.
    End,
    /// Open the last step again, to fix its claims.
    Reopen,
    /// Change the intent of the open step.
    Edit {
        #[arg(long)]
        intent: Option<String>,
    },
    /// Remove the open step when it has no change, or the last step when it is empty.
    Drop,
}

/// Prints the time of a phase to stderr when `SEMILLA_TIMING` is set.
pub fn timing(phase: &str, started: std::time::Instant) {
    if std::env::var_os("SEMILLA_TIMING").is_some() {
        eprintln!("[timing] {phase}: {:.2} s", started.elapsed().as_secs_f64());
    }
}

/// A total of the time of a phase that runs many times (batch 5x, part F).
pub struct TimeTotal {
    nanos: std::sync::atomic::AtomicU64,
    count: std::sync::atomic::AtomicU64,
}

impl TimeTotal {
    pub const fn new() -> Self {
        Self { nanos: std::sync::atomic::AtomicU64::new(0), count: std::sync::atomic::AtomicU64::new(0) }
    }

    /// Adds the time from `started` to now.
    pub fn add(&self, started: std::time::Instant) {
        self.nanos.fetch_add(started.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        self.count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Prints the total to stderr when `SEMILLA_TIMING` is set, and starts a new total.
    pub fn print(&self, phase: &str) {
        let nanos = self.nanos.swap(0, std::sync::atomic::Ordering::Relaxed);
        let count = self.count.swap(0, std::sync::atomic::Ordering::Relaxed);
        if std::env::var_os("SEMILLA_TIMING").is_some() && count > 0 {
            eprintln!("[timing] total of {phase} ({count} times): {:.2} s", nanos as f64 / 1e9);
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            // A rejected result is not a tool error: its first line is the result line.
            match error.downcast_ref::<app::Rejected>() {
                Some(rejected) => eprintln!("{rejected}"),
                // An error can have more than one line (the output of a tool), and it can hold
                // text of the agent: each next line gets an indent.
                None => eprintln!("error: {}", semilla_core::text::block_indent(&format!("{error:#}"), "  ")),
            }
            ExitCode::from(2)
        }
    }
}

/// True if a phase 1 report has no problem except the pending body proofs.
fn phase1_ok(report: &semilla_core::Report) -> bool {
    report.organization.is_empty()
        && report.steps.iter().all(|step| {
            step.errors.is_empty() && step.uncovered.is_empty() && step.facts.iter().all(|f| !f.spotlight || f.reason.is_some()) && step.claims.iter().all(|c| c.status.is_verified() || c.status.is_pending())
        })
}

/// What a command does with changed build code (design 28). Each command runs it in the
/// sandbox. A command with `--no-build-code` does not run it.
fn command_mode(command: &Command) -> buildcode::Mode {
    use buildcode::Mode;
    match command {
        Command::Status { no_build_code: true, .. } | Command::Review { no_build_code: true, .. } | Command::Ui { no_build_code: true, .. } | Command::Verify { no_build_code: true, .. } => Mode::NoBuildCode,
        _ => Mode::Run,
    }
}

/// True for a command that shows the change set of a different person (design 28.2): each
/// command that is not an agent command. `status` with no commit shows the session of the agent.
/// The worker has the mode of its job.
fn is_reviewer_command(command: &Command) -> bool {
    !matches!(command, Command::Step { .. } | Command::Undo | Command::OldStep { .. } | Command::Check { .. } | Command::Tag { .. } | Command::Replay { .. } | Command::Index | Command::Worker | Command::Status { commit: None, .. })
}

/// The line for the option `--trust-build-code`, which is gone (design 28.3).
const OLD_OPTION_LINE: &str = "The option --trust-build-code is gone: the command runs changed build code in the sandbox by default. Remove the option.";

/// True if the command line has the old option.
fn has_old_option(command: &Command) -> bool {
    matches!(command, Command::Status { trust_build_code: true, .. } | Command::Review { trust_build_code: true, .. } | Command::Ui { trust_build_code: true, .. } | Command::Verify { trust_build_code: true, .. })
}

/// Removes each folder of `PATH` that is relative or in the repository folder. Semilla starts
/// `git`, `cargo`, and `rustc` by name. The agent controls the repository folder, so a program
/// there is never one of these tools (invariant I5).
fn safe_path(dir: &std::path::Path) {
    let Some(path) = std::env::var_os("PATH") else { return };
    // The repository folder: the first parent folder with a `.git` entry. Git is not asked,
    // because the search for git uses `PATH`.
    let start = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let root = start.ancestors().find(|folder| folder.join(".git").exists()).map(std::path::Path::to_path_buf);
    let kept: Vec<PathBuf> = std::env::split_paths(&path).filter(|entry| entry.is_absolute() && root.as_ref().is_none_or(|root| !store::is_inside(entry, root))).collect();
    if let Ok(joined) = std::env::join_paths(kept) {
        // No other thread runs yet: this is the start of `main`.
        unsafe { std::env::set_var("PATH", joined) };
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let dir = cli.repo.clone().unwrap_or(std::env::current_dir()?);
    store::set_fresh(cli.fresh);
    safe_path(&dir);
    if matches!(cli.command, Command::Mcp) {
        mcp::serve(&dir)?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut app = App::open(&dir)?;
    app.mode = command_mode(&cli.command);
    // A reviewer command runs changed build code only with the full sandbox, or with the option.
    if is_reviewer_command(&cli.command) {
        let (mode, line) = buildcode::reviewer_mode(app.mode, cli.build_code_no_sandbox);
        app.mode = mode;
        if let Some(line) = line {
            eprintln!("{line}");
        }
    }
    if has_old_option(&cli.command) {
        eprintln!("{OLD_OPTION_LINE}");
    }
    match cli.command {
        Command::Pair { action: PairAction::Refuse { old, new, reason } } => println!("{}", app.pair_refuse(&old, &new, Some(&reason))?),
        Command::Pair { action: PairAction::Unrefuse { old, new } } => println!("{}", app.pair_refuse(&old, &new, None)?),
        Command::Replay { action: ReplayAction::Begin { commit } } => println!("{}", app.replay_begin(commit.as_deref())?),
        Command::Replay { action: ReplayAction::Status } => println!("{}", app.replay_status()?),
        Command::Replay { action: ReplayAction::Plan { out } } => {
            let text = serde_json::to_string_pretty(&app.replay_plan()?)?;
            let file = out.unwrap_or_else(|| app.repo.state_dir().join("plan.json"));
            if file.as_os_str() == "-" {
                std::println!("{text}");
            } else {
                std::fs::write(&file, text + "\n")?;
                println!("wrote the draft plan to {}; check each step and claim, then run `semilla replay apply`", file.display());
            }
        }
        Command::Replay { action: ReplayAction::Apply { plan, full } } => {
            let plan = plan.unwrap_or_else(|| app.repo.state_dir().join("plan.json"));
            let text = std::fs::read_to_string(&plan).with_context(|| format!("read {}", plan.display()))?;
            let plan: plan::Plan = serde_json::from_str(&text).with_context(|| format!("{} is not a valid plan", plan.display()))?;
            let applied = app.replay_apply(&plan, full)?;
            print!("{}", applied.text);
            return Ok(if applied.ok { ExitCode::SUCCESS } else { ExitCode::from(1) });
        }
        Command::Replay { action: ReplayAction::Take { spec } } => {
            anyhow::ensure!(!spec.is_empty(), "name what to take: FILE, FILE:LINE, FILE:START-END, or an item path");
            for one in spec {
                println!("{}", app.replay_take(&one)?);
            }
        }
        Command::Replay { action: ReplayAction::End } => println!("{}", app.replay_end()?),
        Command::Replay { action: ReplayAction::Abort } => println!("{}", app.replay_abort()?),
        Command::Step { intent, group, claims, reasons, tree, fast } => {
            let intent = intent.context("give the intent: `semilla step \"<intent>\"`")?;
            let request = onestep::StepRequest { intent, group, claims, reasons, tree, fast };
            let (text, stored) = app.step_once(&request)?;
            print!("{text}");
            if !stored {
                // A return runs `Drop`: it removes the temporary cache folder of `--fresh`.
                return Ok(ExitCode::from(1));
            }
        }
        Command::Undo => println!("{}", app.undo()?),
        Command::OldStep { action: StepAction::Begin { intent } } => println!("{}", app.step_begin(&intent)?),
        Command::OldStep { action: StepAction::End } => println!("{}", app.step_end()?),
        Command::OldStep { action: StepAction::Reopen } => println!("{}", app.step_reopen()?),
        Command::OldStep { action: StepAction::Edit { intent } } => println!("{}", app.step_edit(intent.as_deref())?),
        Command::OldStep { action: StepAction::Drop } => println!("{}", app.step_drop()?),
        Command::Claim { op, at, args } => {
            let mut map = serde_json::Map::new();
            for arg in args {
                let (key, value) = arg.split_once('=').with_context(|| format!("`{arg}` is not key=value"))?;
                let value = serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
                map.insert(key.to_string(), value);
            }
            println!("claim {} added", app.claim(&op, at.as_deref(), map)?);
        }
        Command::Reason { at, text } => println!("{}", app.reason(&at, &text)?),
        Command::Unclaim { id } => {
            app.unclaim(&id)?;
            println!("claim {id} removed");
        }
        Command::Anchor { position, before, up } => {
            let mut parts = position.rsplitn(3, ':').collect::<Vec<_>>();
            parts.reverse();
            let (file, line, column) = match parts.as_slice() {
                [file, line, column] if line.parse::<usize>().is_ok() => (file.to_string(), line.parse()?, Some(column.parse()?)),
                [file, line] | [_, file, line] => (file.to_string(), line.parse()?, None),
                _ => anyhow::bail!("use FILE:LINE or FILE:LINE:COLUMN"),
            };
            println!("{}", app.anchor_up(&file, line, column, before, up)?);
        }
        Command::Check { step, full } => {
            let session = app.load_session()?;
            let last = (session.changeset.steps.len() + usize::from(session.open.is_some())).checked_sub(1).context("no step yet")?;
            app.no_bodies = app.no_bodies || !full;
            let result = app.check(Some(step.unwrap_or(last)), false)?;
            println!("{}\n{}", result.text, result.tree.render());
            // Phase 1 passes when nothing is wrong except the pending body proofs.
            let ok = if full { result.report.accepted } else { phase1_ok(&result.report) };
            return Ok(if ok { ExitCode::SUCCESS } else { ExitCode::from(1) });
        }
        Command::Status { commit, wait, no_wait, verbose, .. } => {
            // Batch 5k: a reviewer has no use for a list of pending facts, so `status <commit>`
            // waits for the full check. `status` with no commit shows the present state.
            let wait = match wait {
                None if commit.is_some() && !no_wait => Some(600),
                wait => wait,
            };
            let text = app.status(commit.as_deref(), wait, verbose)?;
            println!("{text}");
            // Batch 5y, A3: a result that is rejected or not complete has the exit code 1. For a
            // commit, a result with no phase 2 is not a result. For the session, phase 2 can
            // run in the background: that state keeps the exit code 0.
            if app::result_fails(&text, commit.is_some()) {
                return Ok(ExitCode::from(1));
            }
        }
        Command::Review { target, md, html, full, wait, port, .. } => {
            let target = target.unwrap_or_else(|| "session".into());
            let (key, set) = app.review_target(&target)?;
            let hash = background::changeset_hash(&set);
            // A result of an older state or of an other mode (design 28) is not the result of
            // this command: the check runs again.
            if matches!(background::state(&app, &key, &hash), background::State::Missing | background::State::Stale) {
                background::enqueue(&app, &background::Job { key: key.clone(), hash: hash.clone(), changeset: (key == "session").then(|| set.clone()), mode: app.mode })?;
            }
            let state = background::wait(&mut app, &key, &hash, std::time::Duration::from_secs(wait))?;
            if state != background::State::Ready {
                println!("the background check of {key} is not ready ({state:?}); see `semilla status`");
                return Ok(ExitCode::from(1));
            }
            let data = app.ui_data_for(&set, (key != "session").then_some(key.as_str()))?;
            if let Some(file) = &md {
                std::fs::write(file, semilla_ui::export::render_markdown(&data, full))?;
                println!("wrote {}", file.display());
            }
            if let Some(file) = &html {
                std::fs::write(file, semilla_ui::export::render_html(&data))?;
                println!("wrote {}", file.display());
            }
            // The build code line of the result (design 28), also on the terminal.
            if let Some(line) = app.build_code_notice(&data.report) {
                println!("{line}");
            }
            // The checked configurations (batch 5s, B1), also on the terminal.
            for line in crate::app::config_lines(&data.report) {
                println!("{line}");
            }
            // Batch 5y, A3: the terminal shows the result line, and a result that is rejected
            // or not complete has the exit code 1. The report file is there in each case.
            let result = app::summary_line(&data.report);
            println!("{}", result.lines().next().unwrap_or_default());
            if (md.is_some() || html.is_some()) && !data.report.accepted {
                return Ok(ExitCode::from(1));
            }
            if md.is_none() && html.is_none() {
                let review = app.review_file();
                let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                println!("semilla review UI: http://{addr}");
                tokio::runtime::Runtime::new()?.block_on(semilla_ui::serve(data, review, addr))?;
            }
        }
        Command::Worker => {
            if !background::run_worker(&mut app, true)? {
                eprintln!("[worker] another worker runs");
            }
        }
        Command::Verify { commit: Some(commit), .. } => {
            let commit = semilla_core::text::quoted(&commit);
            anyhow::bail!("`semilla verify` takes no commit ({commit}): it checks the session of this folder. For the result of a commit, use `semilla status HEAD` (or `semilla status <commit>`)");
        }
        Command::Verify { r#final, json, .. } => {
            // Batch 5y, A1: the result is accepted only if the session agrees with git. The
            // first line says what the result covers.
            let (cover, result) = app.verify(r#final)?;
            std::fs::write(app.repo.state_dir().join("report.json"), serde_json::to_string_pretty(&result.report)?)?;
            if json {
                std::println!("{}", serde_json::to_string_pretty(&result.report)?);
            } else {
                // The build code line of the result comes first (design 28.4), then the checked
                // configurations (batch 5s, B1).
                println!("{cover}");
                for line in app.build_code_notice(&result.report).into_iter().chain(crate::app::config_lines(&result.report)) {
                    println!("{line}");
                }
                println!("{}\n{}", result.text, result.tree.render());
            }
            return Ok(if result.report.accepted { ExitCode::SUCCESS } else { ExitCode::from(1) });
        }
        Command::SuggestClaims => println!("suggest-claims is retired: Semilla derives a fact for each change (design 24). Run `semilla step \"<intent>\"`."),
        Command::Group { id, kind, title, why, steps } => {
            let kind: GroupKind = serde_json::from_value(serde_json::Value::String(kind.clone())).with_context(|| format!("`{kind}` is not a group kind"))?;
            app.group_set(&id, kind, &title, &why, steps)?;
            println!("group {id} saved");
        }
        Command::Subgroups { group, set } => match set {
            Some(file) => {
                let layout: Vec<Subgroup> = serde_json::from_str(&std::fs::read_to_string(file)?)?;
                let errors = app.subgroups_set(&group, layout)?;
                if errors.is_empty() {
                    println!("ok");
                } else {
                    println!("rejected:\n{}", errors.join("\n"));
                    return Ok(ExitCode::from(1));
                }
            }
            None => {
                let saved = app.subgroups_saved(&group)?;
                if saved.is_empty() {
                    std::println!("the engine suggestion (no saved layout):\n{}", serde_json::to_string_pretty(&app.subgroups_suggest(&group)?)?);
                } else {
                    std::println!("the saved layout:\n{}", serde_json::to_string_pretty(&saved)?);
                }
            }
        },
        Command::Index => println!("{}", app.index()?),
        Command::Show => println!("{}", app.check(None, false)?.tree.render()),
        Command::Tag { commit } => println!("{}", app.tag(commit.as_deref().unwrap_or("HEAD"))?),
        Command::Load { commit } => {
            let text = app.load(&commit)?;
            println!("{text}");
            if app::result_fails(&text, false) {
                return Ok(ExitCode::from(1));
            }
        }
        Command::Ui { port, .. } => {
            let data = app.ui_data()?;
            let review = app.review_file();
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
            println!("semilla review UI: http://{addr}");
            tokio::runtime::Runtime::new()?.block_on(semilla_ui::serve(data, review, addr))?;
        }
        Command::Ops { name: None } => {
            println!("The claims that stay (design 24): Semilla derives each other change as a fact.");
            for op in semilla_core::ops::OPS.iter().filter(|op| onestep::CLAIMS.contains(&op.name)) {
                let args = if op.args.is_empty() { String::new() } else { format!(" ({})", op.args.join(", ")) };
                println!("{:<26} {:<10} {}{args}", op.name, format!("{:?}", op.level).to_lowercase(), op.summary);
            }
        }
        Command::Ops { name: Some(name) } => {
            let op = semilla_core::ops::op(&name).with_context(|| format!("unknown operation `{name}`"))?;
            println!("{}: {}", op.name, op.summary);
            println!("  level: {}   kind: {}   category: {}   role: {}", format!("{:?}", op.level).to_lowercase(), format!("{:?}", op.kind).to_lowercase(),
                op.category.map(|category| category.name()).unwrap_or("none"),
                serde_json::to_value(op.role).unwrap().as_str().unwrap());
            println!("  needs: {}", if op.args.is_empty() { "an anchor only".to_string() } else { format!("an anchor, and {}", op.args.join(", ")) });
            let optional: Vec<&str> = app::optional_args(op.name).into_iter().filter(|arg| !op.args.contains(arg)).collect();
            println!("  optional: {}", optional.join(", "));
            let example_args: Vec<String> = op.args.iter().map(|a| format!("{a}=…")).collect();
            if let Some(message) = onestep::retired_claim(op.name) {
                println!("  {message}");
            }
            println!("  example: semilla step \"<intent>\" --claim '{} <anchor> {}'", op.name, example_args.join(" "));
        }
        Command::Mcp => unreachable!(),
    }
    Ok(ExitCode::SUCCESS)
}
