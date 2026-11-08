//! The semilla command line.

mod app;
mod git;
mod mcp;
mod oracle;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use semilla_core::{Class, GroupKind, Subgroup};

use crate::app::App;

#[derive(Parser)]
#[command(name = "semilla", version, about = "Verified semantic changes for code review.")]
struct Cli {
    /// The repository folder (default: the current folder).
    #[arg(long, global = true)]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage steps.
    Step {
        #[command(subcommand)]
        action: StepAction,
    },
    /// Add a claim to the open step: `semilla claim add-if --at demo::f/S0 cond="x > 0"`.
    Claim {
        op: String,
        #[arg(long)]
        at: Option<String>,
        /// Arguments as `key=value`. A value that is valid JSON (a number, a list) stays JSON.
        args: Vec<String>,
    },
    /// Remove a claim from the open step.
    Unclaim { id: String },
    /// Print the anchor at a file position: `semilla anchor src/lib.rs:12[:8]`.
    Anchor {
        position: String,
        /// Read the position in the tree before the open step (for claims on existing code).
        #[arg(long)]
        before: bool,
    },
    /// Verify one step (default: the last or open step).
    Check {
        #[arg(long)]
        step: Option<usize>,
    },
    /// Verify the full change set.
    Verify {
        /// Require groups (the final check before a review).
        #[arg(long)]
        r#final: bool,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Suggest claims for the uncovered changes of the last step.
    SuggestClaims,
    /// Declare a group.
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
    /// Show the subgroup suggestion of a group, or set the layout from a JSON file.
    Subgroups {
        group: String,
        #[arg(long)]
        set: Option<PathBuf>,
    },
    /// Build the index of the working tree in `.semilla/index.json`.
    Index,
    /// Print the change tree.
    Show,
    /// Store the change set on a commit (default HEAD) as a git note.
    Tag { commit: Option<String> },
    /// Load the change set of a tagged commit for a review.
    Load { commit: String },
    /// Start the local review UI.
    Ui {
        #[arg(long, default_value_t = 7878)]
        port: u16,
    },
    /// Serve the agent tools over MCP (stdio).
    Mcp,
    /// Print all claim operations.
    Ops,
}

#[derive(Subcommand)]
enum StepAction {
    /// Start a step.
    Begin {
        intent: String,
        #[arg(long)]
        class: Option<String>,
    },
    /// End the open step.
    End,
}

/// Prints the time of a phase to stderr when `SEMILLA_TIMING` is set.
pub fn timing(phase: &str, started: std::time::Instant) {
    if std::env::var_os("SEMILLA_TIMING").is_some() {
        eprintln!("[timing] {phase}: {:.2} s", started.elapsed().as_secs_f64());
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn parse_class(text: &str) -> Result<Class> {
    serde_json::from_value(serde_json::Value::String(text.to_string())).with_context(|| format!("`{text}` is not a class (neutral, logic, control_flow, other)"))
}

fn run(cli: Cli) -> Result<ExitCode> {
    let dir = cli.repo.clone().unwrap_or(std::env::current_dir()?);
    if matches!(cli.command, Command::Mcp) {
        mcp::serve(&dir)?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut app = App::open(&dir)?;
    match cli.command {
        Command::Step { action: StepAction::Begin { intent, class } } => println!("{}", app.step_begin(&intent, class.as_deref().map(parse_class).transpose()?)?),
        Command::Step { action: StepAction::End } => println!("{}", app.step_end()?),
        Command::Claim { op, at, args } => {
            let mut map = serde_json::Map::new();
            for arg in args {
                let (key, value) = arg.split_once('=').with_context(|| format!("`{arg}` is not key=value"))?;
                let value = serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
                map.insert(key.to_string(), value);
            }
            println!("claim {} added", app.claim(&op, at.as_deref(), map)?);
        }
        Command::Unclaim { id } => app.unclaim(&id)?,
        Command::Anchor { position, before } => {
            let mut parts = position.rsplitn(3, ':').collect::<Vec<_>>();
            parts.reverse();
            let (file, line, column) = match parts.as_slice() {
                [file, line, column] if line.parse::<usize>().is_ok() => (file.to_string(), line.parse()?, Some(column.parse()?)),
                [file, line] | [_, file, line] => (file.to_string(), line.parse()?, None),
                _ => anyhow::bail!("use FILE:LINE or FILE:LINE:COLUMN"),
            };
            println!("{}", app.anchor(&file, line, column, before)?);
        }
        Command::Check { step } => {
            let session = app.load_session()?;
            let last = (session.changeset.steps.len() + usize::from(session.open.is_some())).checked_sub(1).context("no step yet")?;
            let result = app.check(Some(step.unwrap_or(last)), false)?;
            println!("{}\n{}", result.text, result.tree.render());
            return Ok(if result.report.accepted { ExitCode::SUCCESS } else { ExitCode::from(1) });
        }
        Command::Verify { r#final, json } => {
            let result = app.check(None, r#final)?;
            std::fs::write(app.repo.state_dir().join("report.json"), serde_json::to_string_pretty(&result.report)?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result.report)?);
            } else {
                println!("{}\n{}", result.text, result.tree.render());
            }
            return Ok(if result.report.accepted { ExitCode::SUCCESS } else { ExitCode::from(1) });
        }
        Command::SuggestClaims => println!("{}", serde_json::to_string_pretty(&app.suggest_claims()?)?),
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
            None => println!("{}", serde_json::to_string_pretty(&app.subgroups_suggest(&group)?)?),
        },
        Command::Index => println!("{}", app.index()?),
        Command::Show => println!("{}", app.check(None, false)?.tree.render()),
        Command::Tag { commit } => println!("{}", app.tag(commit.as_deref().unwrap_or("HEAD"))?),
        Command::Load { commit } => println!("{}", app.load(&commit)?),
        Command::Ui { port } => {
            let data = app.ui_data()?;
            let review = app.repo.state_dir().join("review.json");
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
            println!("semilla review UI: http://{addr}");
            tokio::runtime::Runtime::new()?.block_on(semilla_ui::serve(data, review, addr))?;
        }
        Command::Ops => {
            for op in semilla_core::ops::OPS {
                println!("{:<26} {:<10} {}", op.name, format!("{:?}", op.level).to_lowercase(), op.summary);
            }
        }
        Command::Mcp => unreachable!(),
    }
    Ok(ExitCode::SUCCESS)
}
