//! [R74.1-1] A run of the real compiled code confirms each sure difference (design 74). A child
//! module of the oracle.
//!
//! The model of a check gives an exact input and the outcome of each version (`RunCase`). The
//! probe is a small crate in the cache of Semilla with one dependency: the package of the body
//! in the tree of the version (a path dependency; the code of the user does not change). Its
//! `main` calls the body with the input in `catch_unwind` and writes the outcome: the `Debug`
//! text of the value, the message of a panic, and the value behind each `&mut` argument. One
//! probe serves each tree and configuration of a step, with each call of the step. The build and
//! each run have the sandbox of a check build and a time limit.
//!
//! A run never makes a result "equivalent" or "proved": it confirms a difference, it says why it
//! cannot confirm it, or it shows that the model and the run disagree (a defect of Semilla).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use semilla_core::run::{ArgMode, Check, DISAGREE, Ran, RunCall, RunExit, RunNote, RunSide, compare, unquote};
use semilla_normal::cargo::{Configuration, Features};

use super::NormalOracle;

/// The time limit of the build of one probe.
const BUILD_LIMIT: Duration = Duration::from_secs(300);
/// The time limit of one run of a call.
const RUN_LIMIT: Duration = Duration::from_secs(10);
/// The most builds of one probe: a build that the compiler stops at a call builds again without
/// that call.
const MAX_ROUNDS: usize = 4;


/// One call of a version to run. `value`: the probe writes the `Debug` text of the returned
/// value (the model gives it for a call of the same result, so the type has a `Debug` impl).
#[derive(Clone, Debug)]
pub(crate) struct Job {
    pub config: Configuration,
    pub tree: String,
    pub call: RunCall,
    pub value: bool,
}

/// The result of one call.
#[derive(Clone, Debug)]
pub(crate) enum Got {
    Ran(Ran),
    NoRun(String),
}

/// The results of the calls in this process, by the configuration, the tree, and the call.
static KNOWN: Mutex<BTreeMap<String, Got>> = Mutex::new(BTreeMap::new());

fn job_key(job: &Job) -> String {
    let args: Vec<String> = job.call.args.iter().map(|arg| format!("{:?}{:?}", arg.mode, arg.expr)).collect();
    format!("{}|{}|{}|{}|{}|{}|{:?}", job.config.name(), job.tree, job.call.name, job.call.target, args.join(";"), job.call.expect.muts.len(), job.value)
}

/// True if the model gives the `Debug` text of the value that a call returns.
fn gives_value(call: &RunCall) -> bool {
    matches!(call.expect.exit, RunExit::Returns(Some(_)))
}

/// One call that the probe can build: its index in the jobs, the call, and the package.
struct Prepared {
    index: usize,
    call: RunCall,
    value: bool,
    /// True for a body with no argument that the compiler does not accept as a call: the probe
    /// reads it as a value (the value of a constant or a static is a body, design 61.15).
    as_value: bool,
    package: String,
    folder: PathBuf,
}

impl NormalOracle {
    /// [R74.1-1] Runs each open run of the verdicts and sets its result (design 74). Each item
    /// is a verdict with its configuration and the two trees of its sides. A verdict whose run
    /// disagrees with the model becomes "not decided" with the cause [`DISAGREE`].
    pub(crate) fn confirm(&self, step: &semilla_core::Step, items: &mut [(&str, &str, &str, &mut semilla_verify::equiv::Verdict)]) {
        use semilla_verify::equiv::Verdict;
        let started = std::time::Instant::now();
        let checked = self.checked(step).unwrap_or_default();
        let mut jobs: Vec<Job> = Vec::new();
        let mut owners: Vec<Vec<usize>> = Vec::new();
        for (config, before, after, verdict) in items.iter_mut() {
            let mut mine = Vec::new();
            let Verdict::Different { run, .. } = &mut **verdict else {
                owners.push(mine);
                continue;
            };
            let RunNote::Open(case) = run else {
                owners.push(mine);
                continue;
            };
            if let Some(cause) = &case.blind {
                *run = RunNote::NotConfirmed(cause.clone());
                owners.push(mine);
                continue;
            }
            let Some(configuration) = checked.iter().find(|known| known.name() == *config).cloned() else {
                *run = RunNote::NotConfirmed("the configuration of the result is not known".into());
                owners.push(mine);
                continue;
            };
            // The calls of the two versions of one body have one return type.
            let shared = case.calls.len() == 2 && case.calls[0].args == case.calls[1].args && case.calls.iter().any(gives_value);
            for call in &case.calls {
                mine.push(jobs.len());
                let tree = if call.side == RunSide::Before { before.to_string() } else { after.to_string() };
                jobs.push(Job { config: configuration.clone(), tree, call: call.clone(), value: shared || gives_value(call) });
            }
            owners.push(mine);
        }
        // [R74.1-4] A step with no sure difference starts no probe.
        if jobs.is_empty() {
            return;
        }
        let results = self.run_jobs(&jobs);
        for ((_, _, _, verdict), mine) in items.iter_mut().zip(&owners) {
            if mine.is_empty() {
                continue;
            }
            let found: Vec<(&Job, &Got)> = mine.iter().map(|at| (&jobs[*at], &results[*at])).collect();
            let note = judge(&found);
            match note {
                Ok(note) => {
                    if let Verdict::Different { run, .. } = &mut **verdict {
                        *run = note;
                    }
                }
                Err(ran) => **verdict = Verdict::NotDecided(format!("{DISAGREE} (the run: {ran}); a defect of Semilla")),
            }
        }
        crate::timing(&format!("the probe of {} call(s) (design 74)", jobs.len()), started);
    }

    /// Runs each job: one probe for each tree and configuration.
    pub(crate) fn run_jobs(&self, jobs: &[Job]) -> Vec<Got> {
        let mut results: Vec<Option<Got>> = vec![None; jobs.len()];
        {
            let known = KNOWN.lock().unwrap_or_else(|error| error.into_inner());
            for (index, job) in jobs.iter().enumerate() {
                results[index] = known.get(&job_key(job)).cloned();
            }
        }
        // The sandbox must have each limit: the probe runs code of the change set.
        if let Some(missing) = semilla_normal::sandbox::missing() {
            let cause = format!("the sandbox of this machine is not complete ({})", missing.names());
            return results.into_iter().map(|found| found.unwrap_or_else(|| Got::NoRun(cause.clone()))).collect();
        }
        if !self.mode.runs_build_code() {
            return results.into_iter().map(|found| found.unwrap_or_else(|| Got::NoRun("the command runs no code of the change set (`--no-build-code`)".into()))).collect();
        }
        // A body of a test build with the same call and the same outcomes as the body of the
        // library of its package (one source body in two targets) shares the run of the library.
        let library = |target: &str| library_kind(target);
        let crate_of = |target: &str| target.split('.').next().unwrap_or_default().to_string();
        let mut alias: BTreeMap<usize, usize> = BTreeMap::new();
        for (index, job) in jobs.iter().enumerate() {
            if results[index].is_some() || library(&job.call.target) {
                continue;
            }
            let twin = jobs.iter().position(|other| {
                library(&other.call.target) && crate_of(&other.call.target) == crate_of(&job.call.target) && other.config == job.config && other.tree == job.tree && other.call.name == job.call.name && other.call.args == job.call.args && other.call.expect == job.call.expect
            });
            if let Some(twin) = twin {
                alias.insert(index, twin);
            }
        }
        let mut groups: BTreeMap<(Configuration, String), Vec<usize>> = BTreeMap::new();
        for (index, job) in jobs.iter().enumerate() {
            if results[index].is_none() && !alias.contains_key(&index) {
                groups.entry((job.config.clone(), job.tree.clone())).or_default().push(index);
            }
        }
        // The configurations build in parallel: each one has its own target folder. The trees of
        // one configuration share it, so they build one at a time.
        let mut by_config: BTreeMap<Configuration, Vec<(String, Vec<usize>)>> = BTreeMap::new();
        for ((config, tree), list) in groups {
            by_config.entry(config).or_default().push((tree, list));
        }
        let found: Vec<(usize, Got)> = std::thread::scope(|scope| {
            let handles: Vec<_> = by_config
                .iter()
                .map(|(config, trees)| {
                    scope.spawn(move || {
                        let mut out = Vec::new();
                        for (tree, list) in trees {
                            let started = std::time::Instant::now();
                            out.extend(self.run_group(config, tree, list.iter().map(|index| (*index, &jobs[*index].call, jobs[*index].value)).collect()));
                            crate::timing(&format!("the probe of tree {} in {} ({} call(s))", &tree[..8.min(tree.len())], config.name(), list.len()), started);
                        }
                        out
                    })
                })
                .collect();
            handles.into_iter().flat_map(|handle| handle.join().unwrap_or_default()).collect()
        });
        {
            let mut known = KNOWN.lock().unwrap_or_else(|error| error.into_inner());
            for (index, got) in found {
                known.insert(job_key(&jobs[index]), got.clone());
                results[index] = Some(got);
            }
        }
        for (index, twin) in alias {
            results[index] = results[twin].clone();
        }
        results.into_iter().map(|found| found.unwrap_or_else(|| Got::NoRun("no run".into()))).collect()
    }

    /// Builds the probe of one tree and configuration with the calls, and runs each call.
    fn run_group(&self, config: &Configuration, tree: &str, calls: Vec<(usize, &RunCall, bool)>) -> Vec<(usize, Got)> {
        let all_no = |cause: String| calls.iter().map(|(index, _, _)| (*index, Got::NoRun(cause.clone()))).collect::<Vec<_>>();
        let root = match self.root(tree) {
            Ok((root, _)) => root,
            Err(error) => return all_no(format!("{error:#}")),
        };
        let snapshot = match self.repo.materialize(tree) {
            Ok(dir) => dir,
            Err(error) => return all_no(format!("{error:#}")),
        };
        let units = self.units(tree, config).unwrap_or_default();
        let mut out: Vec<(usize, Got)> = Vec::new();
        let mut ready: Vec<Prepared> = Vec::new();
        for (index, call, value) in calls {
            match prepare(call, &units, &snapshot) {
                Ok((package, folder)) => ready.push(Prepared { index, call: call.clone(), value, as_value: false, package, folder }),
                Err(cause) => out.push((index, Got::NoRun(cause))),
            }
        }
        if ready.is_empty() {
            return out;
        }
        let short = &tree[..12.min(tree.len())];
        let base = root.join("probe");
        let folder = base.join(format!("{short}{}", config.suffix()));
        let target = base.join(format!("target{}", config.suffix()));
        let name = format!("semilla-probe-{short}");
        // One probe build of a configuration at a time, also across processes.
        if std::fs::create_dir_all(&folder).is_err() {
            out.extend(ready.iter().map(|call| (call.index, Got::NoRun(format!("cannot make the probe folder {}", folder.display())))));
            return out;
        }
        let lock = std::fs::File::create(base.join(format!("target{}.lock", config.suffix())));
        let lock = lock.and_then(|file| file.lock().map(|_| file));
        let manifest = manifest(&name, &snapshot, &ready, config, self.metadata(tree).as_ref());
        let mut rounds = 0;
        loop {
            rounds += 1;
            let (code, lines) = main_text(&ready);
            let written = std::fs::write(folder.join("Cargo.toml"), &manifest)
                .and_then(|_| std::fs::create_dir_all(folder.join("src")))
                .and_then(|_| std::fs::write(folder.join("src/main.rs"), &code))
                .and_then(|_| copy_settings(&snapshot, &folder));
            if let Err(error) = written {
                out.extend(ready.iter().map(|call| (call.index, Got::NoRun(format!("cannot write the probe ({error})")))));
                break;
            }
            let packages: Vec<PathBuf> = vec![snapshot.clone()];
            let build = match semilla_normal::cargo::probe_build(&folder, &packages, &target, config.release, BUILD_LIMIT) {
                Ok(build) => build,
                Err(error) => {
                    out.extend(ready.iter().map(|call| (call.index, Got::NoRun(format!("the probe does not build: {error:#}")))));
                    break;
                }
            };
            if build.ok {
                let binary = target.join(if config.release { "release" } else { "debug" }).join(&name);
                let scratch = target.join("run");
                for (position, call) in ready.iter().enumerate() {
                    let file = target.join(format!("out-{short}-{position}.txt"));
                    let got = run_one(&binary, position, &file, &scratch);
                    out.push((call.index, got));
                }
                break;
            }
            if build.stopped {
                out.extend(ready.iter().map(|call| (call.index, Got::NoRun(format!("the build of the probe takes more than {} s", BUILD_LIMIT.as_secs())))));
                break;
            }
            // The compiler stops at a call: that call gets the cause, and the probe builds again
            // without it. An error outside the calls stops each call.
            let mut stopped: BTreeMap<usize, String> = BTreeMap::new();
            for (line, code, text) in &build.errors {
                let Some(line) = line else { continue };
                if let Some(position) = lines.iter().position(|(start, end)| start <= line && line <= end) {
                    stopped.entry(position).or_insert_with(|| error_cause(code, text));
                }
            }
            if stopped.is_empty() || rounds >= MAX_ROUNDS {
                let first = build.errors.first().map(|(_, _, text)| text.clone()).unwrap_or_else(|| build.tail.clone());
                out.extend(ready.iter().map(|call| (call.index, Got::NoRun(format!("the probe does not build: {first}")))));
                break;
            }
            let mut kept = Vec::new();
            for (position, mut call) in ready.into_iter().enumerate() {
                match stopped.get(&position) {
                    // A body with no argument: the next build reads it as a value.
                    Some(_) if call.call.args.is_empty() && !call.as_value => {
                        call.as_value = true;
                        kept.push(call);
                    }
                    Some(cause) => out.push((call.index, Got::NoRun(cause.clone()))),
                    None => kept.push(call),
                }
            }
            ready = kept;
            if ready.is_empty() {
                break;
            }
        }
        if let Ok(lock) = lock {
            let _ = lock.unlock();
        }
        out
    }
}

/// [R74.1-3] The package of a call, or the cause why the probe cannot call it: a library
/// target, a body with a path, and an input that Rust can write.
fn prepare(call: &RunCall, units: &[semilla_normal::UnitRecord], snapshot: &Path) -> Result<(String, PathBuf), String> {
    if !library_kind(&call.target) {
        let kind = call.target.split('.').nth(1).unwrap_or_default();
        return Err(format!("the body is in a target of the kind `{kind}`, which another crate cannot use"));
    }
    if call.name.contains('{') {
        return Err("the body has no path that a call can name (a closure or a nested body)".into());
    }
    if let Some(cause) = call.args.iter().find_map(|arg| arg.expr.as_ref().err().map(|cause| format!("the input `{}` has no Rust form: {cause}", arg.name))) {
        return Err(cause);
    }
    let unit = units.iter().find(|unit| unit.target == call.target).ok_or_else(|| "the compiled target of the body has no unit record".to_string())?;
    if unit.package.is_empty() {
        return Err("the unit record of the body has no package".into());
    }
    Ok((unit.package.clone(), snapshot.join(&unit.folder)))
}

/// True if a target is a library: the crate types of its name (`<crate>.<types>.<hash>`) have
/// one that ends with `lib`. The compiler decides the rest: a crate type that a dependency cannot
/// use gives an error of the probe build with its cause.
fn library_kind(target: &str) -> bool {
    target.split('.').nth(1).unwrap_or_default().split('+').any(|part| part.ends_with("lib"))
}

/// [R74.1-3] The cause of a compiler error at a call of the probe: the text of the compiler
/// (an item or a field that is not public, a path that the crate root does not have).
fn error_cause(code: &str, text: &str) -> String {
    if code.is_empty() { format!("the compiler does not accept the call in the probe: {text}") } else { format!("the compiler does not accept the call in the probe ({code}: {text})") }
}

/// The manifest of the probe: one path dependency for each package of the calls, with the
/// features of the configuration, and the profiles of the tree.
fn manifest(name: &str, snapshot: &Path, calls: &[Prepared], config: &Configuration, metadata: Option<&serde_json::Value>) -> String {
    let mut text = format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[dependencies]\n");
    let mut seen: Vec<&str> = Vec::new();
    for call in calls {
        if seen.contains(&call.package.as_str()) {
            continue;
        }
        seen.push(&call.package);
        let known: Vec<String> = metadata
            .and_then(|data| data["packages"].as_array())
            .into_iter()
            .flatten()
            .filter(|package| package["name"].as_str() == Some(call.package.as_str()))
            .flat_map(|package| package["features"].as_object().map(|map| map.keys().cloned().collect::<Vec<_>>()).unwrap_or_default())
            .collect();
        let pick = |list: &[String]| -> Vec<String> {
            list.iter()
                .filter_map(|feature| match feature.split_once('/') {
                    Some((package, feature)) if package == call.package => Some(feature.to_string()),
                    Some(_) => None,
                    None => Some(feature.clone()),
                })
                .filter(|feature| known.contains(feature))
                .collect()
        };
        let (default, features) = match &config.features {
            Features::Default => (true, Vec::new()),
            Features::NoDefault => (false, Vec::new()),
            Features::All => (true, known.iter().filter(|feature| *feature != "default").cloned().collect()),
            Features::Listed(list) => (true, pick(list)),
            Features::NoDefaultListed(list) => (false, pick(list)),
        };
        let features: Vec<String> = features.iter().map(|feature| format!("{feature:?}")).collect();
        text.push_str(&format!("{:?} = {{ path = {:?}, default-features = {default}, features = [{}] }}\n", call.package, call.folder.display().to_string(), features.join(", ")));
    }
    text.push_str("\n[workspace]\n");
    // The profiles and the patches of the tree: a dependency does not read them, so the probe
    // has them (the code generation settings of the configuration).
    if let Some(table) = std::fs::read_to_string(snapshot.join("Cargo.toml")).ok().and_then(|text| text.parse::<toml::Table>().ok()) {
        let mut extra = toml::Table::new();
        if let Some(profile) = table.get("profile") {
            extra.insert("profile".into(), profile.clone());
        }
        if let Some(mut patch) = table.get("patch").cloned() {
            absolute_paths(&mut patch, snapshot);
            extra.insert("patch".into(), patch);
        }
        if !extra.is_empty() {
            text.push('\n');
            text.push_str(&toml::to_string(&extra).unwrap_or_default());
        }
    }
    text
}

/// Makes each `path` value of a table absolute (relative to the tree).
fn absolute_paths(value: &mut toml::Value, base: &Path) {
    match value {
        toml::Value::Table(table) => {
            for (key, item) in table.iter_mut() {
                if key == "path"
                    && let toml::Value::String(path) = item
                {
                    *path = base.join(&*path).display().to_string();
                } else {
                    absolute_paths(item, base);
                }
            }
        }
        toml::Value::Array(items) => items.iter_mut().for_each(|item| absolute_paths(item, base)),
        _ => {}
    }
}

/// Copies the lock file and the cargo configuration of the tree into the probe folder, so the
/// probe uses the versions and the settings of the tree.
fn copy_settings(snapshot: &Path, folder: &Path) -> std::io::Result<()> {
    match std::fs::read(snapshot.join("Cargo.lock")) {
        Ok(lock) => std::fs::write(folder.join("Cargo.lock"), lock)?,
        Err(_) => {
            let _ = std::fs::remove_file(folder.join("Cargo.lock"));
        }
    }
    for name in ["config.toml", "config"] {
        if let Ok(text) = std::fs::read(snapshot.join(".cargo").join(name)) {
            std::fs::create_dir_all(folder.join(".cargo"))?;
            std::fs::write(folder.join(".cargo").join(name), text)?;
        }
    }
    Ok(())
}

/// The text of `src/main.rs` of the probe, and the first and the last line of the function of
/// each call (an error of the compiler in these lines stops that call).
fn main_text(calls: &[Prepared]) -> (String, Vec<(usize, usize)>) {
    let mut text = String::from(
        "#![allow(warnings)]\n\
         // The probe of Semilla (design 74): one call of a body for each index.\n\
         use std::io::Write as _;\n\
         \n\
         fn semilla_put(out: &mut std::fs::File, tag: &str, text: &str) {\n\
         \x20   let _ = writeln!(out, \"{tag} {text:?}\");\n\
         \x20   let _ = out.flush();\n\
         }\n\
         \n\
         fn main() {\n\
         \x20   let args: Vec<String> = std::env::args().collect();\n\
         \x20   let index: usize = args.get(1).and_then(|text| text.parse().ok()).unwrap_or(usize::MAX);\n\
         \x20   let path = args.get(2).cloned().unwrap_or_default();\n\
         \x20   let mut out = std::fs::OpenOptions::new().append(true).open(&path).expect(\"the output file of the probe\");\n\
         \x20   if cfg!(panic = \"abort\") {\n\
         \x20       semilla_put(&mut out, \"A\", \"\");\n\
         \x20   }\n\
         \x20   let hook = path.clone();\n\
         \x20   std::panic::set_hook(Box::new(move |info| {\n\
         \x20       if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&hook) {\n\
         \x20           match info.payload_as_str() {\n\
         \x20               Some(text) => semilla_put(&mut file, \"P\", text),\n\
         \x20               None => semilla_put(&mut file, \"Q\", \"\"),\n\
         \x20           }\n\
         \x20       }\n\
         \x20   }));\n\
         \x20   match index {\n",
    );
    for position in 0..calls.len() {
        text.push_str(&format!("        {position} => semilla_call_{position}(&mut out),\n"));
    }
    text.push_str("        _ => semilla_put(&mut out, \"X\", \"\"),\n    }\n}\n");
    let mut lines = Vec::new();
    for (position, prepared) in calls.iter().enumerate() {
        let call = &prepared.call;
        let start = text.lines().count() + 2;
        text.push_str(&format!("\n// {} `{}`\nfn semilla_call_{position}(out: &mut std::fs::File) {{\n", call.side.word(), call.name));
        let mut passed = Vec::new();
        let mut muts = Vec::new();
        for (at, arg) in call.args.iter().enumerate() {
            let expr = arg.expr.as_ref().map(String::as_str).unwrap_or("()");
            text.push_str(&format!("    let mut semilla_a{at} = {expr};\n"));
            passed.push(match arg.mode {
                ArgMode::Value => format!("semilla_a{at}"),
                ArgMode::Ref => format!("&semilla_a{at}"),
                ArgMode::Mut => {
                    muts.push(at);
                    format!("&mut semilla_a{at}")
                }
            });
        }
        let expr = if prepared.as_value { call.name.clone() } else { format!("{}({})", call.name, passed.join(", ")) };
        text.push_str(&format!("    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {expr}));\n"));
        if prepared.value {
            text.push_str("    match &result {\n        Ok(value) => semilla_put(out, \"R\", &format!(\"{:?}\", value)),\n        Err(_) => semilla_put(out, \"U\", \"\"),\n    }\n");
        } else {
            text.push_str("    match &result {\n        Ok(_) => semilla_put(out, \"S\", \"\"),\n        Err(_) => semilla_put(out, \"U\", \"\"),\n    }\n");
        }
        for at in muts {
            text.push_str(&format!("    semilla_put(out, \"M\", &format!(\"{{:?}}\", semilla_a{at}));\n"));
        }
        text.push_str("}\n");
        lines.push((start, text.lines().count()));
    }
    (text, lines)
}

/// Runs one call of a built probe and reads its outcome.
fn run_one(binary: &Path, position: usize, file: &Path, scratch: &Path) -> Got {
    let run = match semilla_normal::cargo::probe_run(binary, &[position.to_string(), file.display().to_string()], file, scratch, RUN_LIMIT) {
        Ok(run) => run,
        Err(error) => return Got::NoRun(format!("the probe does not run: {error:#}")),
    };
    if run.stopped {
        return Got::NoRun(format!("the run takes more than {} s", RUN_LIMIT.as_secs()));
    }
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let _ = std::fs::remove_file(file);
    read_outcome(&text, run.code, run.signal)
}

/// The outcome of a run from the lines of the probe.
fn read_outcome(text: &str, code: Option<i32>, signal: Option<i32>) -> Got {
    let mut abort_strategy = false;
    let mut panic: Option<Option<String>> = None;
    let mut ret: Option<Option<String>> = None;
    let mut caught = false;
    let mut muts = Vec::new();
    for line in text.lines() {
        let (tag, rest) = line.split_once(' ').unwrap_or((line, ""));
        let value = unquote(rest);
        match tag {
            "A" => abort_strategy = true,
            "P" => panic = Some(value),
            "Q" => panic = Some(None),
            "R" => ret = Some(value),
            "S" => ret = Some(None),
            "U" => caught = true,
            "M" => muts.push(value.unwrap_or_default()),
            "X" => return Got::NoRun("the probe has no call with this index".into()),
            _ => {}
        }
    }
    // A return: a panic before it was a panic that the body caught itself.
    let exit = if let Some(value) = ret {
        RunExit::Returns(value)
    } else if caught {
        RunExit::Panics(panic.flatten())
    } else if signal.is_some() || code.is_some_and(|code| code != 0) {
        match panic {
            // The panic strategy `abort`: a panic ends the process.
            Some(message) if abort_strategy => RunExit::Panics(message),
            Some(_) | None if signal.is_some() => RunExit::Aborts,
            _ => return Got::NoRun(format!("the run ends with no outcome (exit code {})", code.unwrap_or(-1))),
        }
    } else {
        return Got::NoRun("the run ends with no outcome".into());
    };
    Got::Ran(Ran { exit, muts })
}

/// [R74.1-2] The note of a result from the runs of its calls, or the text of the runs when they
/// disagree with the model.
fn judge(found: &[(&Job, &Got)]) -> Result<RunNote, String> {
    let mut texts = Vec::new();
    let mut differs = false;
    let mut unknown: Option<String> = None;
    for (job, got) in found {
        match got {
            Got::NoRun(cause) => return Ok(RunNote::NotConfirmed(cause.clone())),
            Got::Ran(ran) => {
                texts.push(format!("{} {}", job.call.side.word(), ran.text()));
                match compare(&job.call.expect, ran) {
                    Check::Same => {}
                    Check::Differs => differs = true,
                    Check::Unknown(cause) => {
                        unknown.get_or_insert(cause);
                    }
                }
            }
        }
    }
    if differs {
        return Err(texts.join(", "));
    }
    Ok(match unknown {
        Some(cause) => RunNote::NotConfirmed(format!("{cause} (the run: {})", texts.join(", "))),
        None => RunNote::Confirmed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lines_of_a_run_give_its_outcome() {
        let ran = |text: &str, code: Option<i32>, signal: Option<i32>| match read_outcome(text, code, signal) {
            Got::Ran(ran) => Some(ran.exit),
            Got::NoRun(_) => None,
        };
        assert_eq!(ran("R \"5\"\n", Some(0), None), Some(RunExit::Returns(Some("5".into()))));
        assert_eq!(ran("P \"attempt to add with overflow\"\nU \"\"\n", Some(0), None), Some(RunExit::Panics(Some("attempt to add with overflow".into()))));
        // A panic that the body catches itself, then a return.
        assert_eq!(ran("P \"inner\"\nS \"\"\n", Some(0), None), Some(RunExit::Returns(None)));
        // The panic strategy `abort`.
        assert_eq!(ran("A \"\"\nP \"too big\"\n", None, Some(6)), Some(RunExit::Panics(Some("too big".into()))));
        // A panic in a drop on the unwind path: an abort.
        assert_eq!(ran("P \"first\"\nP \"second\"\n", None, Some(6)), Some(RunExit::Aborts));
        assert_eq!(ran("", Some(3), None), None);
    }
}
