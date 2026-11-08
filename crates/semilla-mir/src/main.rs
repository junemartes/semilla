//! The semilla MIR driver.
//!
//! It runs rustc on a crate and writes the normalized MIR of each local body, as one JSON object
//! for each line.
//!
//! It has two modes:
//!
//! - **Command mode:** `semilla-mir-driver --level <n0|n2|n3> -- <rustc arguments>`. The driver
//!   prints the records to stdout and stops before code generation.
//! - **Wrapper mode:** `RUSTC_WORKSPACE_WRAPPER=semilla-mir-driver cargo check`. Cargo gives the
//!   path of the real rustc as the first argument. The driver reads the level from
//!   `SEMILLA_MIR_LEVEL` and the output folder from `SEMILLA_MIR_OUT`, and writes the records of
//!   each crate to `$SEMILLA_MIR_OUT/<crate>.<kind>.<hash>.jsonl`. The compile continues
//!   normally, so the crates that depend on this crate get its metadata. Probe calls (`-vV`,
//!   `--print`, no source file) and calls without `SEMILLA_MIR_LEVEL` go to the real rustc
//!   unchanged.
//!
//! The driver tells cargo that the output depends on `SEMILLA_MIR_LEVEL`, `SEMILLA_MIR_OUT`, and
//! `SEMILLA_MIR_RUN`. Cargo compiles a crate again when one of them changes. A caller can set
//! `SEMILLA_MIR_RUN` to a new value to get the records of all workspace crates again.

#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_public;
extern crate rustc_public_bridge;
extern crate rustc_session;
extern crate rustc_span;

mod inline;
mod normalize;
mod print;
mod term;

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use rustc_driver::Compilation;
use rustc_interface::interface;
use rustc_middle::ty::TyCtxt;
use rustc_public::mir::Body;
use rustc_public::mir::mono::Instance;
use rustc_public::ty::{Abi, RigidTy, TyKind};
use rustc_public::{CrateDef, CrateItem};

/// The environment variables that the output depends on.
const TRACKED_ENV: [&str; 3] = ["SEMILLA_MIR_LEVEL", "SEMILLA_MIR_OUT", "SEMILLA_MIR_RUN"];

/// A normal form level. Each level adds rustc flags before the normalization.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    /// Only the MIR passes that rustc requires. All drops stay explicit.
    N0,
    /// The rustc MIR optimizations.
    N2,
    /// N2, and then bounded inlining of the crate's own functions (see `inline.rs`).
    N3,
}

impl Level {
    fn parse(text: &str) -> Option<Level> {
        match text {
            "n0" => Some(Level::N0),
            "n2" => Some(Level::N2),
            "n3" => Some(Level::N3),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Level::N0 => "n0",
            Level::N2 => "n2",
            Level::N3 => "n3",
        }
    }

    /// The rustc flags for this level. Both sides of a compare must use the same flags.
    fn rustc_flags(self) -> &'static [&'static str] {
        match self {
            Level::N0 => &["-Zmir-opt-level=0", "-Copt-level=0"],
            Level::N2 | Level::N3 => &[
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
    /// The full def path, with the crate name first.
    name: String,
    level: &'static str,
    /// The canonical MIR text.
    text: String,
    /// The shape of the control flow graph, without statements.
    shape: String,
    /// The def paths of the local functions that the body refers to.
    callees: Vec<String>,
    /// True if the control flow graph has no cycle.
    loop_free: bool,
    /// The N4 term of a loop-free body (see `term.rs`), at the levels n2 and n3 only. The first
    /// line lists the build rules that changed the term.
    term: Option<String>,
    span: Option<SpanInfo>,
    /// The compiled target: `<crate>.<kind>.<metadata hash>` in wrapper mode (the same for both
    /// versions of a workspace), empty in command mode. A body name can exist in more than one
    /// target (a lib and its test build), and each target has its own body.
    target: String,
}

#[derive(serde::Serialize)]
struct SpanInfo {
    file: String,
    line_start: usize,
    line_end: usize,
}

/// Where the records go.
enum Output {
    Stdout,
    File(PathBuf),
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--level") {
        return command_mode(&args);
    }
    wrapper_mode(&args)
}

fn command_mode(args: &[String]) -> ExitCode {
    let Some((level, rustc_args)) = parse_args(args) else {
        eprintln!("usage: semilla-mir-driver --level <n0|n2|n3> -- <rustc arguments>");
        return ExitCode::from(2);
    };
    run_compiler(level, rustc_args, Output::Stdout)
}

fn wrapper_mode(args: &[String]) -> ExitCode {
    let Some(real_rustc) = args.get(1) else {
        eprintln!("usage: semilla-mir-driver --level <n0|n2|n3> -- <rustc arguments>");
        eprintln!("   or: RUSTC_WORKSPACE_WRAPPER=semilla-mir-driver cargo check");
        return ExitCode::from(2);
    };
    let rustc_args = &args[2..];
    let level = std::env::var("SEMILLA_MIR_LEVEL").ok().and_then(|text| Level::parse(&text));
    let out = std::env::var_os("SEMILLA_MIR_OUT").map(PathBuf::from);
    match (level, out) {
        (Some(level), Some(out)) if !is_probe(rustc_args) => {
            let file = out.join(output_file_name(rustc_args));
            run_compiler(level, rustc_args, Output::File(file))
        }
        _ => pass_through(real_rustc, rustc_args),
    }
}

/// Returns true for a rustc call that does not compile a crate: a version query, a `--print`
/// query, or a call without a source file.
fn is_probe(args: &[String]) -> bool {
    let query = args.iter().any(|arg| {
        arg == "-vV"
            || arg == "-V"
            || arg == "--version"
            || arg == "-"
            || arg.starts_with("--print")
    });
    let has_source = args.iter().any(|arg| !arg.starts_with('-') && arg.ends_with(".rs"));
    query || !has_source
}

/// Runs the real rustc with the same arguments, and returns its exit code.
fn pass_through(rustc: &str, args: &[String]) -> ExitCode {
    match Command::new(rustc).args(args).status() {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("semilla-mir-driver: cannot run {rustc}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Returns a file name that is different for each target of a crate: the crate name, the crate
/// kind (`lib`, `bin`, `test`, ...), and the metadata hash that cargo gives.
fn output_file_name(args: &[String]) -> String {
    let value_of = |flag: &str| {
        args.iter().position(|arg| arg == flag).and_then(|index| args.get(index + 1)).cloned()
    };
    let crate_name = value_of("--crate-name").unwrap_or_else(|| "unknown".to_string());
    let kind = if args.iter().any(|arg| arg == "--test") {
        "test".to_string()
    } else {
        let kinds: Vec<&str> = args
            .iter()
            .enumerate()
            .filter(|(_, arg)| *arg == "--crate-type")
            .filter_map(|(index, _)| args.get(index + 1).map(String::as_str))
            .collect();
        if kinds.is_empty() { "bin".to_string() } else { kinds.join("+") }
    };
    let metadata = args
        .iter()
        .enumerate()
        .find_map(|(index, arg)| {
            let value = if arg == "-C" { args.get(index + 1)? } else { arg.strip_prefix("-C")? };
            value.strip_prefix("metadata=").map(str::to_string)
        })
        .unwrap_or_else(|| {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            args.hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        });
    format!("{crate_name}.{kind}.{metadata}.jsonl")
}

fn run_compiler(level: Level, rustc_args: &[String], output: Output) -> ExitCode {
    let mut full_args = vec!["rustc".to_string()];
    full_args.extend(rustc_args.iter().cloned());
    full_args.extend(level.rustc_flags().iter().map(|flag| flag.to_string()));

    let mut callbacks = Callbacks { level, output, error: None };
    let result = rustc_driver::catch_fatal_errors(|| {
        rustc_driver::compiler_entrypoint(&full_args, &mut callbacks)
    });
    if let Some(error) = callbacks.error {
        eprintln!("semilla-mir-driver: {error}");
        return ExitCode::FAILURE;
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

struct Callbacks {
    level: Level,
    output: Output,
    error: Option<String>,
}

impl rustc_driver::Callbacks for Callbacks {
    fn config(&mut self, config: &mut interface::Config) {
        let tracked: Vec<(String, Option<String>)> =
            TRACKED_ENV.iter().map(|name| (name.to_string(), std::env::var(name).ok())).collect();
        config.track_state = Some(Box::new(move |sess| {
            let mut env = sess.env_depinfo.lock();
            for (name, value) in &tracked {
                env.insert((
                    rustc_span::Symbol::intern(name),
                    value.as_deref().map(rustc_span::Symbol::intern),
                ));
            }
        }));
    }

    fn after_analysis<'tcx>(
        &mut self,
        _compiler: &interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> Compilation {
        let level = self.level;
        let target = match &self.output {
            Output::Stdout => String::new(),
            Output::File(path) => {
                path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default()
            }
        };
        let records = match rustc_public::rustc_internal::run(tcx, || collect(level, &target)) {
            Ok(records) => records,
            Err(error) => {
                self.error = Some(format!("rustc_public failed: {error:?}"));
                return Compilation::Stop;
            }
        };
        let mut text = String::new();
        for record in &records {
            text.push_str(&serde_json::to_string(record).expect("a record is valid JSON"));
            text.push('\n');
        }
        match &self.output {
            Output::Stdout => {
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(text.as_bytes()).expect("stdout is writable");
                // Stop before code generation. The MIR is all that this mode needs.
                Compilation::Stop
            }
            Output::File(path) => {
                if let Err(error) = write_atomically(path, &text) {
                    self.error = Some(format!("cannot write {}: {error}", path.display()));
                    return Compilation::Stop;
                }
                // Continue, so that cargo gets the metadata for the dependent crates.
                Compilation::Continue
            }
        }
    }
}

/// Writes a file through a temporary file, so that a reader never sees a partial file.
fn write_atomically(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

/// Returns the records of all local bodies. Paths print in full, with the crate name first.
fn collect(level: Level, target: &str) -> Vec<BodyRecord> {
    rustc_middle::ty::print::with_resolve_crate_name!(
        rustc_middle::ty::print::with_no_trimmed_paths!(collect_bodies(level, target))
    )
}

/// At N3, a body without an inlined call gets no record: its N3 body is its N2 body, and the
/// compare uses the N2 record of the same target and name.
fn collect_bodies(level: Level, target: &str) -> Vec<BodyRecord> {
    let bodies: Vec<(CrateItem, Body)> = rustc_public::all_local_items()
        .into_iter()
        .filter_map(|item| guarded(|| item_body(item)).flatten().map(|body| (item, body)))
        .collect();
    let callees = if level == Level::N3 { guarded(|| inline_candidates(&bodies)) } else { None };

    bodies
        .iter()
        .filter_map(|(item, body)| {
            let name = item.name();
            guarded(|| {
                let body = match &callees {
                    Some(callees) => inline::inline_calls(body, callees)?,
                    None if level == Level::N3 => return None,
                    None => body.clone(),
                };
                Some(record(name.clone(), &body, level, target))
            })
            .unwrap_or_else(|| Some(failed_record(name, level, target)))
        })
        .collect()
}

/// Runs `f`. Returns `None` if it panics. A panic in rustc_public for one body must not stop the
/// compile of the crate.
fn guarded<T>(f: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

/// A record for a body that the driver could not read. Its text is unique, so it is never equal
/// to another body.
fn failed_record(name: String, level: Level, target: &str) -> BodyRecord {
    BodyRecord {
        name,
        level: level.name(),
        text: print::unique("failed_body"),
        shape: print::unique("failed_body"),
        callees: Vec::new(),
        loop_free: false,
        term: None,
        span: None,
        target: target.to_string(),
    }
}

/// Returns the body of an item. For an item without generic parameters, this is the body of its
/// instance, with all constants evaluated (also the promoted constants).
fn item_body(item: CrateItem) -> Option<Body> {
    if !item.has_body() {
        return None;
    }
    if !item.requires_monomorphization() {
        if let Some(body) = Instance::try_from(item).ok().and_then(|instance| instance.body()) {
            return Some(body);
        }
    }
    item.body()
}

fn record(name: String, body: &Body, level: Level, target: &str) -> BodyRecord {
    let canonical = normalize::canonical_body(body);
    let printed = normalize::canonical_text(&canonical);
    let loop_free = normalize::is_loop_free(&canonical);
    // No level uses an N0 term, so the driver does not build it.
    let term = if loop_free && level != Level::N0 {
        term::body_term(&canonical).map(|term| {
            let text = normalize::stable_closure_names(&term.text);
            format!(";rules {}\n{text}", term.rules.join(" "))
        })
    } else {
        None
    };
    let span = guarded(|| {
        let lines = body.span.get_lines();
        SpanInfo {
            file: body.span.get_filename(),
            line_start: lines.start_line,
            line_end: lines.end_line,
        }
    });
    BodyRecord {
        name,
        level: level.name(),
        text: printed.text,
        shape: normalize::shape(&canonical),
        callees: printed.callees,
        loop_free,
        term,
        span,
        target: target.to_string(),
    }
}

/// Returns the functions that N3 can inline: local, without generic parameters, with the Rust
/// ABI, without `#[track_caller]`, not in a cycle of the call graph, and small.
fn inline_candidates(bodies: &[(CrateItem, Body)]) -> inline::Callees {
    let names: Vec<String> = bodies.iter().map(|(item, _)| item.name()).collect();
    let index: HashMap<&str, usize> =
        names.iter().enumerate().map(|(i, name)| (name.as_str(), i)).collect();
    let edges: Vec<Vec<usize>> = bodies
        .iter()
        .map(|(_, body)| {
            body.blocks
                .iter()
                .filter_map(|block| inline::direct_local_callee(&block.terminator.kind))
                .filter_map(|callee| index.get(callee.as_str()).copied())
                .collect()
        })
        .collect();
    let recursive = in_cycles(&edges);

    let mut callees = HashMap::new();
    for (position, (item, body)) in bodies.iter().enumerate() {
        if recursive.contains(&position) || item.requires_monomorphization() {
            continue;
        }
        let TyKind::RigidTy(RigidTy::FnDef(def, _)) = item.ty().kind() else {
            continue;
        };
        if def.fn_sig().skip_binder().abi != Abi::Rust {
            continue;
        }
        let Ok(instance) = Instance::try_from(*item) else {
            continue;
        };
        if instance.requires_caller_location() {
            continue;
        }
        let statements: usize = body.blocks.iter().map(|block| block.statements.len()).sum();
        if body.blocks.len() > inline::MAX_CALLEE_BLOCKS
            || statements > inline::MAX_CALLEE_STATEMENTS
        {
            continue;
        }
        callees.insert(names[position].clone(), body.clone());
    }
    inline::Callees { bodies: callees }
}

/// Returns the nodes that are in a cycle of a graph: in a strongly connected component with more
/// than one node, or with an edge to itself. This is Tarjan's algorithm, without recursion.
fn in_cycles(edges: &[Vec<usize>]) -> HashSet<usize> {
    let count = edges.len();
    let mut index = vec![usize::MAX; count];
    let mut low = vec![0; count];
    let mut on_stack = vec![false; count];
    let mut stack = Vec::new();
    let mut next = 0;
    let mut result = HashSet::new();
    for start in 0..count {
        if index[start] != usize::MAX {
            continue;
        }
        let mut work = vec![(start, 0usize)];
        while let Some((node, edge)) = work.pop() {
            if edge == 0 {
                index[node] = next;
                low[node] = next;
                next += 1;
                stack.push(node);
                on_stack[node] = true;
            }
            if let Some(&target) = edges[node].get(edge) {
                work.push((node, edge + 1));
                if index[target] == usize::MAX {
                    work.push((target, 0));
                } else if on_stack[target] {
                    low[node] = low[node].min(index[target]);
                }
                continue;
            }
            if low[node] == index[node] {
                let mut component = Vec::new();
                loop {
                    let member = stack.pop().expect("the stack holds the component");
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                if component.len() > 1 || edges[node].contains(&node) {
                    result.extend(component);
                }
            }
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[node]);
            }
        }
    }
    result
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
