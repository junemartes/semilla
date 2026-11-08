//! The semilla MIR driver.
//!
//! It runs rustc on a crate and prints the normalized MIR of each local body, as one JSON object
//! for each line.
//!
//! Usage: `semilla-mir-driver --level <n0|n2> -- <rustc arguments>`

#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_middle;
#[macro_use]
extern crate rustc_public;

mod normalize;

// The `run!` macro brings `std::ops::ControlFlow` into scope for the callback.
use std::process::ExitCode;

use rustc_public::{CompilerError, CrateDef};

/// A normal form level. Each level adds rustc flags before the normalization.
#[derive(Clone, Copy)]
enum Level {
    /// Only the MIR passes that rustc requires. All drops stay explicit.
    N0,
    /// The rustc MIR optimizations.
    N2,
}

impl Level {
    fn parse(text: &str) -> Option<Level> {
        match text {
            "n0" => Some(Level::N0),
            "n2" => Some(Level::N2),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Level::N0 => "n0",
            Level::N2 => "n2",
        }
    }

    /// The rustc flags for this level. Both sides of a compare must use the same flags.
    fn rustc_flags(self) -> &'static [&'static str] {
        match self {
            Level::N0 => &["-Zmir-opt-level=0", "-Copt-level=0"],
            Level::N2 => &[
                "-Zmir-opt-level=2",
                "-Copt-level=2",
                "-Coverflow-checks=on",
                "-Cdebug-assertions=on",
            ],
        }
    }
}

/// One line of output: the normalized MIR of one body.
#[derive(serde::Serialize)]
struct BodyRecord {
    name: String,
    level: &'static str,
    text: String,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some((level, rustc_args)) = parse_args(&args) else {
        eprintln!("usage: semilla-mir-driver --level <n0|n2> -- <rustc arguments>");
        return ExitCode::from(2);
    };

    let mut full_args = vec!["rustc".to_string()];
    full_args.extend(rustc_args.iter().cloned());
    full_args.extend(level.rustc_flags().iter().map(|flag| flag.to_string()));

    let result = run!(&full_args, || {
        for item in rustc_public::all_local_items() {
            let Some(body) = item.body() else {
                continue;
            };
            let record = BodyRecord {
                name: item.name(),
                level: level.name(),
                text: normalize::canonical_text(&body),
            };
            println!("{}", serde_json::to_string(&record).expect("a record is valid JSON"));
        }
        // Stop before code generation. The MIR is all that this driver needs.
        ControlFlow::<(), ()>::Break(())
    });

    match result {
        Ok(()) | Err(CompilerError::Interrupted(())) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Splits the arguments into the level and the rustc arguments.
fn parse_args(args: &[String]) -> Option<(Level, &[String])> {
    let separator = args.iter().position(|arg| arg == "--")?;
    let ours = &args[1..separator];
    let level = match ours {
        [flag, value] if flag == "--level" => Level::parse(value)?,
        _ => return None,
    };
    Some((level, &args[separator + 1..]))
}
