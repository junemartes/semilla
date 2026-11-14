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

extern crate rustc_attr_ir;
extern crate rustc_crate_store;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_ast;
extern crate rustc_ast_pretty;
extern crate rustc_hir;
extern crate rustc_index;
extern crate rustc_infer;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_public;
extern crate rustc_public_bridge;
extern crate rustc_session;
extern crate rustc_span;
extern crate rustc_trait_selection;

mod config;
mod harness;
mod inline;
mod made;
mod normalize;
mod print;
mod reach;
mod rival;
mod term;
mod traits;

use std::collections::{BTreeSet, HashMap, HashSet};
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
            // `-Zinline-mir=no` has no effect at this level. It is a tracked flag: with it, rustc
            // does not use the incremental data of a driver without the pass of `config.rs`.
            Level::N0 => &["-Zmir-opt-level=0", "-Copt-level=0", "-Zinline-mir=no"],
            // The overflow checks and the debug assertions come from the profile of the tree (cargo
            // passes them). A forced value here would hide a profile change (`overflow-checks =
            // false`) at N2 and N3.
            // The inliner of rustc is off (batch 5k). It is on only with no incremental build, so
            // the result must not depend on it. It also copies code of other crates into a body,
            // and that code has values of the check configuration that the optimizer then folds
            // (`size_of::<usize>()`, a `cfg!` in a function of a dependency). N3 inlines the
            // functions of the workspace.
            Level::N2 | Level::N3 => &["-Zmir-opt-level=2", "-Copt-level=2", "-Zinline-mir=no"],
        }
    }
}

/// The `-C debug-assertions` and `-C overflow-checks` values of the original rustc arguments:
/// an explicit flag, or else the rustc default for the original opt-level (on at opt-level 0).
fn inherited_checks(args: &[String]) -> Vec<String> {
    let value = |key: &str| -> Option<String> {
        let mut found = None;
        for (index, arg) in args.iter().enumerate() {
            let flag = if arg == "-C" { args.get(index + 1).map(String::as_str) } else { arg.strip_prefix("-C") };
            if let Some(rest) = flag.and_then(|f| f.strip_prefix(key)) {
                found = Some(rest.trim_start_matches('=').to_string());
            }
        }
        found
    };
    let on = |text: &str| matches!(text, "" | "y" | "yes" | "on" | "true");
    let opt_zero = value("opt-level").is_none_or(|level| level == "0");
    let debug = value("debug-assertions").map(|v| on(&v)).unwrap_or(opt_zero);
    let overflow = value("overflow-checks").map(|v| on(&v)).unwrap_or(debug);
    let flag = |name: &str, on: bool| format!("-C{name}={}", if on { "on" } else { "off" });
    vec![flag("debug-assertions", debug), flag("overflow-checks", overflow)]
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
    /// The N4 term of a loop-free body (see `term.rs`), at the levels n2 and n3 only. At n3, the
    /// term can come from a body with only the loop-free callees inlined. The first
    /// line lists the build rules that changed the term.
    term: Option<String>,
    span: Option<SpanInfo>,
    /// For a body from a macro: the file, the line, and the column of the outermost macro call
    /// (see `made::place`; batch 6b, part 2). At n0 only.
    #[serde(skip_serializing_if = "Option::is_none")]
    site: Option<(String, usize, usize)>,
    /// The compiled target: `<crate>.<kind>.<metadata hash>` in wrapper mode (the same for both
    /// versions of a workspace), empty in command mode. A body name can exist in more than one
    /// target (a lib and its test build), and each target has its own body.
    target: String,
    /// The calls to functions of other crates, with a summary of their signatures (design 10.2).
    externals: Vec<External>,
    /// The calls whose target the driver cannot resolve: a call through a function pointer, a
    /// trait object, or a trait method on a type parameter (A3). At the level n0 only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    indirect: Vec<String>,
    /// The types and the traits whose trait impls can run with no direct call (batch 5f, A1): a
    /// type in the generic arguments of a call, the source of an unsize cast. At n0 only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reach_types: Vec<DropType>,
    /// The types with a destructor that the body drops or gives to generic code. At n0 only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    drop_types: Vec<DropType>,
    /// For a type of `drop_types` that the body does not drop itself: the first call that has
    /// the type in its generic arguments (batch 5k, D9). At n0 only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    drop_calls: Vec<DropCall>,
    /// For a method of a trait impl: the types and the trait of the impl header. For a provided
    /// method of a trait: the trait. At n0 only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    impl_of: Vec<String>,
    /// For the `drop` method of a `Drop` impl: the type. At n0 only.
    #[serde(skip_serializing_if = "Option::is_none")]
    drop_of: Option<String>,
    /// The types in the parameters of a function (see `reach::note_inputs`). At n0 only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    param_types: Vec<DropType>,
    /// The types in the constants and the statics that the body reads. At n0 only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    const_types: Vec<DropType>,
    /// The uses of the build configuration in the body: the text of each macro call that reads
    /// the configuration (`cfg!(..)`, `env!(..)`), sorted. Not the uses through `debug_assert!`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    config: Vec<String>,
    /// The functions that the body can call from a block that its start block reaches, through
    /// its closures (see `reach::live_callees`). Not for a closure. At n0 only.
    #[serde(skip_serializing_if = "Option::is_none")]
    live_callees: Option<Vec<String>>,
    /// The functions that the body calls on each path from its start to a normal return (see
    /// `reach::sure_callees`). Not for a closure. At n0 only.
    #[serde(skip_serializing_if = "Option::is_none")]
    sure_callees: Option<Vec<String>>,
    /// The facts for the ends of a run that are not a return (see `reach::sure_callees`).
    #[serde(skip_serializing_if = "Option::is_none")]
    sure_ends: Option<std::collections::BTreeMap<String, Vec<String>>>,
    /// The callees that the inline step put into the body, at each depth (batch 5l). At n3
    /// only. The compare drops the record if one of them is opaque.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    inlined: Vec<String>,
    /// The types with a definition outside the standard library that the body uses (see
    /// `reach::adts`). At n0 only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    adts: Vec<String>,
    /// The names of the expanded code of the item (see `config::names`). At n0 only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    names: Vec<NameRecord>,
}

/// One name of the expanded code, with its count (see `config::NameUse`).
#[derive(serde::Serialize)]
struct NameRecord {
    word: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    member: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    qual: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    prefix: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mac: Option<String>,
    line: usize,
    count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    rival: Option<String>,
}

/// The call that gives a type to generic code: the def path of the type, and the printed path
/// of the function.
#[derive(serde::Serialize)]
struct DropCall {
    path: String,
    call: String,
}

/// A type with a destructor: the def path and the crate that defines it.
#[derive(serde::Serialize)]
struct DropType {
    path: String,
    krate: String,
}

/// A call to a function of another crate, and the facts from its signature.
#[derive(serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct External {
    path: String,
    /// `err` (returns `Result` or `Option`), `mut` (takes `&mut`), `unsafe`, `never` (returns `!`).
    badges: Vec<&'static str>,
    /// The crate that defines the function (not the crate of a re-export).
    krate: String,
    /// The def path in the crate that defines the function.
    def: String,
}

/// True for the function of an opaque value (see `config.rs`): the intrinsic `black_box`. It
/// is not a call of the source.
pub(crate) fn is_opaque_wrap(func: &rustc_public::mir::Operand) -> bool {
    let rustc_public::mir::Operand::Constant(constant) = func else { return false };
    let TyKind::RigidTy(RigidTy::FnDef(def, _)) = constant.const_.ty().kind() else { return false };
    rustc_middle::ty::tls::with(|tcx| config::is_black_box(tcx, rustc_public::rustc_internal::internal(tcx, def.def_id())))
}

/// The calls in a body to functions of other crates.
fn external_calls(body: &Body) -> Vec<External> {
    use rustc_public::mir::{Mutability, Safety, TerminatorKind};
    let mut found = std::collections::BTreeSet::new();
    for block in &body.blocks {
        let TerminatorKind::Call { func, .. } = &block.terminator.kind else { continue };
        let Ok(ty) = func.ty(body.locals()) else { continue };
        let kind = ty.kind();
        let TyKind::RigidTy(RigidTy::FnDef(def, _)) = &kind else { continue };
        if def.krate().is_local || is_opaque_wrap(func) {
            continue;
        }
        let mut badges = Vec::new();
        if let Some(signature) = kind.fn_sig() {
            let signature = signature.skip_binder();
            match signature.output().kind() {
                TyKind::RigidTy(RigidTy::Adt(adt, _))
                    if adt.name().ends_with("::Result") || adt.name().ends_with("::Option") =>
                {
                    badges.push("err")
                }
                TyKind::RigidTy(RigidTy::Never) => badges.push("never"),
                _ => {}
            }
            if signature.inputs().iter().any(|input| {
                matches!(input.kind(), TyKind::RigidTy(RigidTy::Ref(_, _, Mutability::Mut)))
            }) {
                badges.push("mut");
            }
            if signature.safety == Safety::Unsafe {
                badges.push("unsafe");
            }
        }
        let (krate, real) = rustc_middle::ty::tls::with(|tcx| reach::origin(rustc_public::rustc_internal::internal(tcx, def.def_id())));
        let path = rustc_middle::ty::tls::with(|tcx| reach::extern_crate_path(tcx, def.name()));
        found.insert(External { path, badges, krate, def: real });
    }
    found.into_iter().collect()
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
        (Some(level), Some(out)) if !is_probe(rustc_args) && !is_build_script(rustc_args) => {
            let file = out.join(output_file_name(rustc_args));
            run_compiler(level, rustc_args, Output::File(file))
        }
        _ => pass_through(real_rustc, rustc_args),
    }
}

/// Returns true for the compile of a build script: cargo gives the crate name
/// `build_script_<file>` and the crate type `bin`. Semilla reads no record of a build script, so
/// the driver writes none (batch 5v, part F): each record file is then the file of a unit that
/// the messages of cargo name. A binary of the user with such a name gets no record: its facts
/// stay facts to read.
fn is_build_script(args: &[String]) -> bool {
    let value = |key: &str| args.iter().position(|arg| arg == key).and_then(|index| args.get(index + 1)).map(String::as_str);
    value("--crate-name").is_some_and(|name| name.starts_with("build_script_")) && value("--crate-type") == Some("bin")
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
    // The `extra-filename` hash is also in the artifact names that cargo reports, so a caller can
    // match the record files to the units of a build. `metadata` is the fallback.
    let codegen_value = |key: &str| {
        args.iter().enumerate().find_map(|(index, arg)| {
            let value = if arg == "-C" { args.get(index + 1)? } else { arg.strip_prefix("-C")? };
            value.strip_prefix(key).map(|v| v.trim_start_matches('-').to_string())
        })
    };
    let metadata = codegen_value("extra-filename=")
        .filter(|value| !value.is_empty())
        .or_else(|| codegen_value("metadata="))
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
    // A new opt-level changes the rustc defaults of the debug assertions and the overflow checks.
    // Keep the values of the build that cargo asked for (from the profile of the tree).
    full_args.extend(inherited_checks(rustc_args));

    let mut callbacks = Callbacks { level, output, error: None, made: None };
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
    /// The items after the macro expansion (batch 6b, part 2). `None`: the driver cannot list
    /// them.
    made: Option<Vec<(Option<rustc_span::def_id::LocalDefId>, made::MadeItem)>>,
}

impl rustc_driver::Callbacks for Callbacks {
    fn config(&mut self, config: &mut interface::Config) {
        config::install(config);
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

    fn after_expansion<'tcx>(
        &mut self,
        _compiler: &interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> Compilation {
        // The list is large, and Semilla reads it only from the N0 records.
        if self.level == Level::N0 {
            self.made = guarded(|| made::collect(tcx));
        }
        Compilation::Continue
    }

    fn after_analysis<'tcx>(
        &mut self,
        _compiler: &interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> Compilation {
        if let Err(error) = traits::write(tcx) {
            self.error = Some(format!("cannot write the trait table: {error}"));
            return Compilation::Stop;
        }
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
        // The first line is the record of the compile unit (batch 5t, part A).
        let mut text = String::new();
        let mut unit = UnitLine { unit: unit_record(tcx, &target) };
        unit.unit.made = self.made.take().and_then(|items| guarded(|| made::finish(tcx, items)));
        text.push_str(&serde_json::to_string(&unit).expect("a unit record is valid JSON"));
        text.push('\n');
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

/// The record of one compile unit (batch 5t, part A). The compiler gives each value, so the
/// record has the effect of each flag: the profile, `RUSTFLAGS`, a `--cfg` argument, and a
/// `cargo:rustc-cfg` line of a build script.
#[derive(serde::Serialize)]
struct UnitRecord {
    /// The name of the unit: the stem of the record file.
    target: String,
    /// The name of the package (`CARGO_PKG_NAME`).
    package: String,
    /// The folder of the package, relative to the workspace root (`CARGO_MANIFEST_DIR`).
    folder: String,
    /// Each true `cfg` name (`unix`) and each true pair (`feature="std"`), in order.
    cfg: Vec<String>,
    /// Each source file of this crate that the compiler read, relative to the workspace root.
    files: Vec<String>,
    /// The root file of the crate, relative to the workspace root.
    root: String,
    /// The target triple.
    triple: String,
    /// Each item of the expanded code, with the place of the macro call that makes it (batch
    /// 6b, part 2). `None`: the driver cannot list the items.
    made: Option<Vec<made::MadeItem>>,
}

/// The line of a unit record in the output.
#[derive(serde::Serialize)]
struct UnitLine {
    unit: UnitRecord,
}

/// A path without `.` and `..` parts, relative to the current folder if it is below it. Cargo
/// starts the compiler in the workspace root for a package of the workspace.
pub(crate) fn workspace_path(path: &std::path::Path) -> String {
    let current = std::env::current_dir().unwrap_or_default();
    let full = if path.is_absolute() { path.to_path_buf() } else { current.join(path) };
    let mut clean = PathBuf::new();
    for part in full.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                clean.pop();
            }
            part => clean.push(part),
        }
    }
    clean.strip_prefix(&current).unwrap_or(&clean).to_string_lossy().into_owned()
}

fn unit_record(tcx: TyCtxt<'_>, target: &str) -> UnitRecord {
    let mut cfg: Vec<String> = tcx
        .sess
        .config
        .iter()
        .map(|(name, value)| match value {
            Some(value) => format!("{name}=\"{value}\""),
            None => name.to_string(),
        })
        .collect();
    cfg.sort();
    let mut files = BTreeSet::new();
    let root = match &tcx.sess.io.input {
        rustc_session::config::Input::File(path) => workspace_path(path),
        _ => String::new(),
    };
    for file in tcx.sess.source_map().files().iter() {
        if file.cnum != rustc_span::def_id::LOCAL_CRATE {
            continue;
        }
        if let rustc_span::FileName::Real(name) = &file.name
            && let Some(path) = name.local_path()
        {
            files.insert(workspace_path(path));
        }
    }
    UnitRecord {
        target: target.to_string(),
        package: std::env::var("CARGO_PKG_NAME").unwrap_or_default(),
        folder: std::env::var("CARGO_MANIFEST_DIR").map(|folder| workspace_path(std::path::Path::new(&folder))).unwrap_or_default(),
        cfg,
        files: files.into_iter().collect(),
        root,
        triple: tcx.sess.opts.target_triple.to_string(),
        made: None,
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
    // The printer needs the source of each constant that is not configuration-free.
    if guarded(|| rustc_middle::ty::tls::with(config::note_local_bodies)).is_none() {
        return vec![failed_record("the driver failed to read the constants".to_string(), level, target)];
    }
    let bodies: Vec<(CrateItem, Body)> = rustc_public::all_local_items()
        .into_iter()
        .filter_map(|item| guarded(|| item_body(item)).flatten().map(|body| (item, body)))
        .collect();
    // The `main` function of the test harness (see `harness.rs`). A failure of the test must not
    // hide a body: then the item keeps its def path and its full text.
    let harness: HashSet<rustc_public::DefId> =
        bodies.iter().map(|(item, _)| item.def_id()).filter(|def| guarded(|| harness::is_harness_main(*def)).unwrap_or(false)).collect();
    let callees = if level == Level::N3 { guarded(|| inline_candidates(&bodies, &harness)) } else { None };
    let by_def: std::collections::HashMap<rustc_public::DefId, &Body> = bodies.iter().map(|(item, body)| (item.def_id(), body)).collect();

    // A constant with a literal initializer has no MIR body. If the literal comes from the
    // build configuration, the constant gets a record that names its source.
    let known: HashSet<String> = bodies.iter().map(|(item, _)| item.name()).collect();
    let literal: Vec<BodyRecord> = if level == Level::N3 {
        Vec::new()
    } else {
        guarded(|| rustc_middle::ty::tls::with(|tcx| {
            config::literal_constants(tcx)
                .into_iter()
                .filter(|(name, _, _)| !known.contains(name))
                .map(|(name, identity, span)| config_record(name, identity, rustc_public::rustc_internal::stable(span), level, target))
                .collect()
        }))
        .unwrap_or_default()
    };

    bodies
        .iter()
        .filter_map(|(item, body)| {
            if harness.contains(&item.def_id()) {
                let name = guarded(harness::record_name).unwrap_or_else(|| format!("{}::{{test-harness-main}}", item.name()));
                // The harness has no N3 record: the compare uses its N2 record.
                if level == Level::N3 {
                    return None;
                }
                return guarded(|| harness_record(name.clone(), body, level, target)).or_else(|| Some(failed_record(name, level, target)));
            }
            let name = item.name();
            // The descriptor of a `#[test]` function: its text names the crate `test` by one path
            // in each tree.
            let test_crate = guarded(|| test_crate_path(*item)).flatten();
            guarded(|| {
                // The printer reads the constants of this body.
                let _reader = rustc_middle::ty::tls::with(|tcx| config::Reader::set(rustc_public::rustc_internal::internal(tcx, item.def_id()).as_local()));
                let Some(callees) = &callees else {
                    if level == Level::N3 {
                        return None;
                    }
                    let mut found = record(name.clone(), body, level, target, &[], &generics_text(*item));
                    if level == Level::N0 {
                        add_reach(&mut found, *item, body);
                        found.live_callees = guarded(|| reach::live_callees(item.def_id(), body, &by_def)).flatten();
                        if let Some((sure, ends)) = guarded(|| reach::sure_callees(item.def_id(), body, &by_def)).flatten() {
                            found.sure_callees = Some(sure);
                            found.sure_ends = Some(ends).filter(|ends| !ends.is_empty());
                        }
                        // No list: the driver failed. The marker makes the body opaque.
                        found.adts = guarded(|| reach::adts(body)).unwrap_or_else(|| vec!["?".to_string()]);
                        // A type alias of a crate outside the workspace: an entry `alias PATH`.
                        let aliases = guarded(|| {
                            rustc_middle::ty::tls::with(|tcx| {
                                let local = rustc_public::rustc_internal::internal(tcx, item.def_id()).as_local();
                                local.map(|local| config::aliases(tcx, local).iter().map(|path| format!("alias {path}")).collect::<Vec<_>>())
                            })
                        });
                        found.adts.extend(aliases.flatten().unwrap_or_else(|| vec!["?".to_string()]));
                        // The items and the types by the resolution of the compiler (batch 5o).
                        let uses = guarded(|| {
                            rustc_middle::ty::tls::with(|tcx| {
                                let local = rustc_public::rustc_internal::internal(tcx, item.def_id()).as_local();
                                local.map(|local| config::uses(tcx, local))
                            })
                        });
                        for entry in uses.flatten().unwrap_or_else(|| vec!["?".to_string()]) {
                            if !found.adts.contains(&entry) {
                                found.adts.push(entry);
                            }
                        }
                        // The names of the expanded code (batch 5p). No list: the marker `?` in
                        // `adts` makes the body opaque.
                        let names = guarded(|| {
                            rustc_middle::ty::tls::with(|tcx| {
                                let local = rustc_public::rustc_internal::internal(tcx, item.def_id()).as_local();
                                local.map(|local| config::names(tcx, local))
                            })
                        });
                        match names.flatten() {
                            Some(names) => {
                                found.names = names
                                    .into_iter()
                                    .map(|(name, count)| NameRecord { word: name.word, member: name.member, qual: name.qual, prefix: name.prefix, mac: name.mac, line: name.line, count, rival: name.rival })
                                    .collect();
                            }
                            None => {
                                if !found.adts.iter().any(|entry| entry == "?") {
                                    found.adts.push("?".to_string());
                                }
                            }
                        }
                    }
                    return Some(found);
                };
                let mut inlined = Vec::new();
                let full = inline::inline_calls_named(body, callees, inline::Scope::All, &mut inlined)?;
                // The N3 term comes from a body with only the loop-free callees inlined: a body
                // with all callees inlined often has a loop (from one callee) and then no term.
                // If that body has too many paths for a term, the next tries inline only the
                // small, then the tiny loop-free callees. The text and the term are two separate normal forms
                // of the same behavior.
                let mut tries = Vec::new();
                if normalize::is_loop_free(&full) {
                    tries.push(full.clone());
                }
                for scope in [
                    inline::Scope::LoopFree,
                    inline::Scope::SmallLoopFree,
                    inline::Scope::TinyLoopFree,
                ] {
                    tries.push(
                        inline::inline_calls(body, callees, scope).unwrap_or_else(|| body.clone()),
                    );
                }
                let mut found = record(name.clone(), &full, level, target, &tries, &generics_text(*item));
                inlined.sort();
                found.inlined = inlined;
                Some(found)
            })
            .unwrap_or_else(|| Some(failed_record(name, level, target)))
            .map(|mut found| {
                if let Some(path) = &test_crate {
                    let (from, to) = (format!("{path}::"), "test::");
                    found.text = found.text.replace(&from, to);
                    found.shape = found.shape.replace(&from, to);
                    found.term = found.term.map(|term| term.replace(&from, to));
                }
                found
            })
        })
        .chain(literal)
        .collect()
}

/// For the item that `#[test]` makes for a test function (the test descriptor): the path
/// that the printer gives to the crate `test`, if it is not `test`.
///
/// The printer names a crate through an `extern crate` item of the local crate, if the crate
/// root has one. `#[test]` adds `extern crate test;` next to each test function. So a test
/// function in the crate root (the usual form of a file in `tests/`) changes the path of each
/// descriptor type from `test::TestDesc` to `<crate>::test::TestDesc`. The reader finds the
/// source position in a descriptor by the path `test::TestDesc`, and with the other path the
/// line of each test was a part of the compare: a new test above gave a changed MIR text for
/// each test below it (batch 6b, part 1, case 4). The record has the plain path in each case.
///
/// The identity comes from the compiler: a `static` or a `const` whose type is `TestDescAndFn`
/// of the crate `test`. A type of the user with this name is in a different crate.
fn test_crate_path(item: CrateItem) -> Option<String> {
    use rustc_hir::def::DefKind;
    rustc_middle::ty::tls::with(|tcx| {
        let def = rustc_public::rustc_internal::internal(tcx, item.def_id());
        // The compiler of today makes a `static`. An older compiler made a `const`.
        if !matches!(tcx.def_kind(def), DefKind::Static { .. } | DefKind::Const) {
            return None;
        }
        let rustc_middle::ty::Adt(adt, _) = tcx.type_of(def).instantiate_identity().skip_norm_wip().kind() else { return None };
        let krate = adt.did().krate;
        if krate == rustc_span::def_id::LOCAL_CRATE || tcx.crate_name(krate) != rustc_span::sym::test || tcx.item_name(adt.did()).as_str() != "TestDescAndFn" {
            return None;
        }
        let path = tcx.def_path_str(krate.as_def_id());
        (path != "test").then_some(path)
    })
}

/// The place of the outermost macro call of a body from a macro expansion.
fn body_site(body: &Body) -> Option<(String, usize, usize)> {
    rustc_middle::ty::tls::with(|tcx| {
        let span = rustc_public::rustc_internal::internal(tcx, body.span);
        if span.from_expansion() { made::place(tcx, span) } else { None }
    })
}

/// The record of a constant with no MIR body whose value comes from the build configuration.
/// The text names the source and not the value.
fn config_record(name: String, identity: String, span: rustc_public::ty::Span, level: Level, target: &str) -> BodyRecord {
    let mut found = failed_record(name, level, target);
    found.text = format!("const _ = opaque(@{identity});");
    found.shape = "const".to_string();
    found.indirect = Vec::new();
    found.loop_free = true;
    found.span = guarded(|| {
        let lines = span.get_lines();
        SpanInfo { file: span.get_filename(), line_start: lines.start_line, line_end: lines.end_line }
    });
    found.config = vec![identity];
    found
}

/// The record of the `main` function of the test harness. The text names the test runner and not
/// the list of the tests (see `harness.rs`). A body with an unknown form keeps its full text.
fn harness_record(name: String, body: &Body, level: Level, target: &str) -> BodyRecord {
    let mut found = record(name, body, level, target, &[], "");
    if let Some(text) = harness::text(body) {
        found.shape = text.clone();
        found.text = text;
        found.term = None;
    }
    // The span of generated code has no line of the user.
    found.span = None;
    found
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
        site: None,
        target: target.to_string(),
        externals: Vec::new(),
        // A property claim must not read a body with no record as examined.
        indirect: if level == Level::N0 { vec!["the driver failed to read a body".into()] } else { Vec::new() },
        reach_types: Vec::new(),
        drop_types: Vec::new(),
        drop_calls: Vec::new(),
        impl_of: Vec::new(),
        drop_of: None,
        live_callees: None,
        sure_callees: None,
        sure_ends: None,
        inlined: Vec::new(),
        adts: Vec::new(),
        names: Vec::new(),
        param_types: Vec::new(),
        const_types: Vec::new(),
        config: Vec::new(),
    }
}

/// The generic type and const parameters of an item, with the parameters of its parents (an
/// impl, the function of a closure): ` generics <A#0, B#1>`, or empty for an item with none.
///
/// The body prints a type parameter by its name (`A`), and a caller gives the generic arguments
/// by position (`size::<u8, u64>`). A step that changes the order of the parameters changes the
/// map from names to positions, and no body text shows it (batch 5e). This text shows it.
fn generics_text(item: CrateItem) -> String {
    let params = rustc_middle::ty::tls::with(|tcx| {
        let mut params = Vec::new();
        let mut def_id = Some(rustc_public::rustc_internal::internal(tcx, item.def_id()));
        while let Some(id) = def_id {
            let generics = tcx.generics_of(id);
            for param in generics.own_params.iter().rev() {
                if !matches!(param.kind, rustc_middle::ty::GenericParamDefKind::Lifetime) {
                    params.push(format!("{}#{}", param.name, param.index));
                }
            }
            def_id = generics.parent;
        }
        params.reverse();
        params
    });
    if params.is_empty() { String::new() } else { format!(" generics <{}>", params.join(", ")) }
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

/// Builds the record of a body. The term comes from the first body of `term_tries` that is
/// loop-free and gives a term (each one has the same behavior as `body`). Without tries, the
/// term comes from `body`.
fn record(
    name: String,
    body: &Body,
    level: Level,
    target: &str,
    term_tries: &[Body],
    generics: &str,
) -> BodyRecord {
    let canonical = normalize::canonical_body(body);
    let mut printed = normalize::canonical_text(&canonical);
    // The generic parameters go on the first line (the signature).
    if !generics.is_empty() {
        if let Some(end) = printed.text.find(" {\n") {
            printed.text.insert_str(end, generics);
        }
    }
    // A pattern constant that is not configuration-free has no operand: the text names it, and
    // the body gets no term (see `config.rs`).
    let patterns = rustc_middle::ty::tls::with(|tcx| config::pattern_lines(rustc_public::rustc_internal::internal(tcx, body.span)));
    for line in &patterns {
        printed.text.push_str(line);
        printed.text.push('\n');
    }
    let loop_free = normalize::is_loop_free(&canonical);
    // No level uses an N0 term, so the driver does not build it.
    let term = if level == Level::N0 || !patterns.is_empty() {
        None
    } else if term_tries.is_empty() {
        loop_free.then(|| body_term(&canonical, generics)).flatten()
    } else {
        let mut found = None;
        let mut previous: Option<String> = None;
        for candidate in term_tries {
            let canonical = normalize::canonical_body(candidate);
            if !normalize::is_loop_free(&canonical) {
                continue;
            }
            // The same body as the try before has the same (missing) term.
            let text = normalize::canonical_text(&canonical).text;
            if previous.as_ref() == Some(&text) {
                continue;
            }
            previous = Some(text);
            found = body_term(&canonical, generics);
            if found.is_some() {
                break;
            }
        }
        found
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
        callees: {
            let mut callees = printed.callees;
            for callee in guarded(|| resolved_callees(&canonical)).unwrap_or_default() {
                if !callees.contains(&callee) {
                    callees.push(callee);
                }
            }
            callees
        },
        loop_free,
        term,
        span,
        site: if level == Level::N0 { guarded(|| body_site(body)).flatten() } else { None },
        target: target.to_string(),
        externals: if level == Level::N0 {
            guarded(|| external_calls(&canonical)).unwrap_or_default()
        } else {
            Vec::new()
        },
        indirect: if level == Level::N0 {
            guarded(|| indirect_calls(&canonical)).unwrap_or_else(|| vec!["the driver failed to list the calls".into()])
        } else {
            Vec::new()
        },
        reach_types: Vec::new(),
        drop_types: Vec::new(),
        drop_calls: Vec::new(),
        impl_of: Vec::new(),
        drop_of: None,
        live_callees: None,
        sure_callees: None,
        sure_ends: None,
        inlined: Vec::new(),
        adts: Vec::new(),
        names: Vec::new(),
        param_types: Vec::new(),
        const_types: Vec::new(),
        config: printed.config,
    }
}

/// Adds the code that the body can run with no direct call (see `reach.rs`). At n0 only.
fn add_reach(record: &mut BodyRecord, item: CrateItem, body: &Body) {
    let Some(found) = guarded(|| reach::body_reach(item.def_id(), body)) else {
        record.indirect.push("the driver failed to list the types of a body".into());
        return;
    };
    for callee in found.callees {
        if !record.callees.contains(&callee) {
            record.callees.push(callee);
        }
    }
    for def_id in found.extern_values {
        let path = reach::printed_path(def_id);
        if !record.externals.iter().any(|external| external.path == path) {
            let (krate, def) = reach::origin(def_id);
            record.externals.push(External { path, badges: Vec::new(), krate, def });
        }
    }
    record.indirect.extend(found.gaps);
    let refs = |found: BTreeSet<(String, String)>| found.into_iter().map(|(path, krate)| DropType { path, krate }).collect::<Vec<_>>();
    record.reach_types = refs(found.types);
    record.param_types = refs(found.params);
    record.const_types = refs(found.consts);
    record.drop_types = found.drops.into_iter().map(|(path, krate)| DropType { path, krate }).collect();
    record.drop_calls = found.drop_calls.into_iter().map(|(path, call)| DropCall { path, call }).collect();
    match guarded(|| reach::impl_header(item.def_id())) {
        Some((impl_of, drop_of)) => {
            record.impl_of = impl_of;
            record.drop_of = drop_of;
        }
        None => record.indirect.push("the driver failed to read the impl of a body".into()),
    }
}

/// True if a type or its arguments contain a type parameter, a `const` parameter, or an alias (a
/// generic type).
fn is_generic<T: rustc_public::visitor::Visitable>(item: &T) -> bool {
    struct Find;
    impl rustc_public::visitor::Visitor for Find {
        type Break = ();
        fn visit_ty(&mut self, ty: &rustc_public::ty::Ty) -> std::ops::ControlFlow<()> {
            match ty.kind() {
                TyKind::Param(_) | TyKind::Alias(..) | TyKind::Bound(..) => std::ops::ControlFlow::Break(()),
                _ => rustc_public::visitor::Visitable::super_visit(ty, self),
            }
        }
        // A `const` parameter is generic too (`[usize; N]`). rustc stops with an internal error
        // if the driver asks for the impl of a call with such an argument.
        fn visit_const(&mut self, constant: &rustc_public::ty::TyConst) -> std::ops::ControlFlow<()> {
            use rustc_public::ty::TyConstKind;
            match constant.kind() {
                TyConstKind::Param(_) | TyConstKind::Bound(..) | TyConstKind::Unevaluated(..) => std::ops::ControlFlow::Break(()),
                _ => rustc_public::visitor::Visitable::super_visit(constant, self),
            }
        }
    }
    item.visit(&mut Find).is_break()
}

/// The calls in a body whose target the driver cannot resolve (A3): a call through a function
/// pointer, a call of a trait method on a trait object, and a call of a trait method on a type
/// parameter. A call of a trait method on a known type resolves to its impl: that is a callee.
fn indirect_calls(body: &Body) -> Vec<String> {
    use rustc_public::mir::TerminatorKind;
    use rustc_public::ty::{AssocContainer, GenericArgKind};
    let mut found = Vec::new();
    for block in &body.blocks {
        let TerminatorKind::Call { func, .. } = &block.terminator.kind else { continue };
        let Ok(ty) = func.ty(body.locals()) else {
            found.push("a call with no known type".to_string());
            continue;
        };
        match ty.kind() {
            TyKind::RigidTy(RigidTy::FnPtr(_)) => found.push("a call through a function pointer".to_string()),
            TyKind::RigidTy(RigidTy::FnDef(def, args)) => {
                let trait_method = def.associated_item().is_some_and(|item| item.container == AssocContainer::Trait);
                if !trait_method {
                    continue;
                }
                let self_ty = args.0.first().and_then(|arg| match arg {
                    GenericArgKind::Type(ty) => Some(*ty),
                    _ => None,
                });
                match self_ty.map(|ty| ty.kind()) {
                    Some(TyKind::RigidTy(RigidTy::Dynamic(..))) => found.push(format!("a call of `{}` on a trait object", def.name())),
                    Some(TyKind::RigidTy(RigidTy::FnPtr(_))) => found.push(format!("a call of `{}` on a function pointer", def.name())),
                    Some(TyKind::RigidTy(RigidTy::Ref(_, inner, _))) if matches!(inner.kind(), TyKind::RigidTy(RigidTy::Dynamic(..))) => {
                        found.push(format!("a call of `{}` on a trait object", def.name()))
                    }
                    _ if is_generic(&args) => found.push(format!("a call of `{}` on a type parameter", def.name())),
                    _ => {}
                }
            }
            _ => found.push("a call of an unknown kind".to_string()),
        }
    }
    found
}

/// The local impl methods that the trait method calls of a body resolve to (A3). The callee list
/// of the printer names the trait item, not the impl body that runs.
fn resolved_callees(body: &Body) -> Vec<String> {
    use rustc_public::mir::TerminatorKind;
    use rustc_public::ty::AssocContainer;
    let mut found = Vec::new();
    for block in &body.blocks {
        let TerminatorKind::Call { func, .. } = &block.terminator.kind else { continue };
        let Ok(ty) = func.ty(body.locals()) else { continue };
        let TyKind::RigidTy(RigidTy::FnDef(def, args)) = ty.kind() else { continue };
        if !def.associated_item().is_some_and(|item| item.container == AssocContainer::Trait) || is_generic(&args) {
            continue;
        }
        if let Ok(instance) = Instance::resolve(def, &args) {
            if instance.def.krate().is_local {
                found.push(instance.def.name());
            }
        }
    }
    found
}

/// Returns the term text of a canonical loop-free body, with the line of the build rules first.
fn body_term(canonical: &Body, generics: &str) -> Option<String> {
    term::body_term(canonical, generics).map(|term| {
        let text = normalize::stable_closure_names(&term.text);
        format!(";rules {}\n{text}", term.rules.join(" "))
    })
}

/// Returns the functions that N3 can inline: local, without generic parameters, with the Rust
/// ABI, without `#[track_caller]`, not in a cycle of the call graph, and small.
///
/// In the test build of a binary crate, the `main` function of the test harness has the def path
/// of the `main` function of the user. The harness is not a candidate, and its name is not in
/// the index: a call of `main` must find the body of the user.
fn inline_candidates(bodies: &[(CrateItem, Body)], harness: &HashSet<rustc_public::DefId>) -> inline::Callees {
    let names: Vec<String> = bodies
        .iter()
        .map(|(item, _)| if harness.contains(&item.def_id()) { format!("{}::{{test-harness-main}}", item.name()) } else { item.name() })
        .collect();
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
    let mut loop_free = HashSet::new();
    for (position, (item, body)) in bodies.iter().enumerate() {
        if recursive.contains(&position) || item.requires_monomorphization() || harness.contains(&item.def_id()) {
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
        // An item that the build configuration selects is opaque (batch 5l).
        let selected = guarded(|| rustc_middle::ty::tls::with(|tcx| config::under_cfg(tcx, rustc_public::rustc_internal::internal(tcx, item.def_id())))).unwrap_or(true);
        if selected {
            continue;
        }
        let statements: usize = body.blocks.iter().map(|block| block.statements.len()).sum();
        if body.blocks.len() > inline::MAX_CALLEE_BLOCKS
            || statements > inline::MAX_CALLEE_STATEMENTS
        {
            continue;
        }
        if normalize::is_loop_free(body) {
            loop_free.insert(names[position].clone());
        }
        callees.insert(names[position].clone(), body.clone());
    }
    inline::Callees { bodies: callees, loop_free }
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
