//! The background check (phase 2, design section 19).
//!
//! - the end of a step and `tag` add a job to the queue, and start a worker.
//! - The worker runs the full check of each job (the MIR builds, N1 to N4, SMT, indirect
//!   changes), and stores the report. It stops when the queue is empty. One worker runs at a time.
//! - A report has the hash of its change set. A changed change set makes the old report stale.
//!
//! The queue, the reports (`reports/<key>.json`), and the worker log are in the cache folder of
//! the user, outside the repository folder (invariant I5, design 27). The agent controls each
//! file of the repository folder, so no result comes from a file there. A stored report names
//! the tools that computed it (the Semilla binary, the MIR driver, the solver, the toolchain):
//! a report of other tools is not valid. For a tagged commit, a git note under
//! `refs/notes/semilla-report` holds a copy for other tools. Semilla never reads a result from
//! that note: `status` and `review` compute the result again on this machine.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use anyhow::{Context, Result};
use semilla_core::{ChangeSet, Report};
use serde::{Deserialize, Serialize};

use crate::app::App;

/// One job of the queue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    /// `session` for the work in progress, or a commit hash.
    pub key: String,
    /// The hash of the change set at the time of the job.
    pub hash: String,
    /// The change set of a session job. A commit job reads it from the commit note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changeset: Option<ChangeSet>,
    /// Who asked for the check (design 28). A job of a reviewer command with no permission runs
    /// no build code that the change set changes. An old job file has no mode: it is such a job.
    #[serde(default)]
    pub mode: crate::buildcode::Mode,
}

/// A stored report, with the hash of the change set that it checks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredReport {
    pub key: String,
    /// The tree of the commit of `key` (empty for the session). A report is valid only for this
    /// commit and this tree, so a copied report note is not valid for another commit.
    #[serde(default)]
    pub tree: String,
    pub hash: String,
    /// The tools that computed the report ([`App::tools_key`]). A report is valid only for the
    /// same tools: a run with another solver or another driver does not give a result here.
    #[serde(default)]
    pub tools: String,
    pub report: Report,
    /// The text report, for the agent tools and the CLI.
    pub text: String,
}

/// The hash of a change set (its JSON form).
pub fn changeset_hash(set: &ChangeSet) -> String {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(set).unwrap_or_default().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn queue_dir(app: &App) -> PathBuf {
    app.repo.cache_dir().join("queue")
}

/// The file of a stored result. A result that ran build code that the change set changes has
/// the mark `build-code` in its name (design 28): a reviewer command with no permission never
/// reads that file.
fn report_file(app: &App, key: &str, marked: bool) -> PathBuf {
    app.repo.cache_dir().join("reports").join(format!("{key}{}.json", if marked { ".build-code" } else { "" }))
}

/// True if a result ran build code that the change set changes.
fn is_marked(report: &Report) -> bool {
    report.build_code.as_ref().is_some_and(|note| note.ran && !note.changes.is_empty())
}

/// Adds a job. A newer job for the same key replaces the old one.
pub fn enqueue(app: &App, job: &Job) -> Result<()> {
    let dir = queue_dir(app);
    std::fs::create_dir_all(&dir)?;
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        // A job replaces only an older job of the same key and the same mode.
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(&format!("-{}{}.json", job.key, job.mode.suffix()))) {
            let _ = std::fs::remove_file(path);
        }
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    std::fs::write(dir.join(format!("{nanos:024}-{}{}.json", job.key, job.mode.suffix())), serde_json::to_string(job)?)?;
    Ok(())
}

/// Removes each queued job of a key (each mode). `tag` calls it: the session jobs are done, and
/// their result is stored under the tagged commit.
pub fn remove_jobs(app: &App, key: &str) {
    for (path, job) in jobs(app) {
        if job.key == key {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The jobs in the queue, oldest first.
pub fn jobs(app: &App) -> Vec<(PathBuf, Job)> {
    let Ok(entries) = std::fs::read_dir(queue_dir(app)) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    paths.sort();
    paths.into_iter().filter_map(|path| Some((path.clone(), serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?))).collect()
}

/// Starts a worker in the background, unless `SEMILLA_NO_WORKER` is set. A second worker stops at
/// once when one already runs.
pub fn spawn_worker(app: &App) -> Result<()> {
    // A process with no cache that stays (`--fresh`) runs each check itself.
    if std::env::var_os("SEMILLA_NO_WORKER").is_some() || app.repo.store.is_temporary() {
        return Ok(());
    }
    use std::os::unix::process::CommandExt;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(app.repo.cache_dir().join("worker.log"))?;
    std::process::Command::new(std::env::current_exe()?)
        .arg("--repo")
        .arg(&app.repo.root)
        .arg("worker")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .context("start the background worker")?;
    Ok(())
}

/// Runs all jobs, then returns. Returns false if another worker already runs. With `log`, each
/// job writes one line to the standard error (the worker log). A run inside `status` writes no
/// log line to the normal output (C8).
pub fn run_worker(app: &mut App, log: bool) -> Result<bool> {
    let lock = std::fs::File::create(app.repo.cache_dir().join("worker.lock"))?;
    if lock.try_lock().is_err() {
        return Ok(false);
    }
    app.no_bodies = false;
    let mode = app.mode;
    loop {
        let Some((path, job)) = jobs(app).into_iter().next() else { break };
        // The job has the permission of the command that asked for it, not of this process.
        app.mode = job.mode;
        // The change set of a commit must belong to the commit (a copied note does not).
        let set = match &job.changeset {
            Some(set) => Some(set.clone()),
            None => app.commit_changeset(&job.key).ok().map(|(_, set)| set),
        };
        if let Some(set) = set {
            let started = std::time::Instant::now();
            match app.check_set(&set, job.key != "session") {
                Ok(result) => {
                    let tree = if job.key == "session" { String::new() } else { app.repo.commit_tree(&job.key).unwrap_or_default() };
                    let stored = StoredReport { key: job.key.clone(), tree, hash: job.hash.clone(), tools: app.tools_key(), report: result.report, text: result.text };
                    store(app, &stored)?;
                    if log {
                        eprintln!("[worker] {} checked in {:.1} s", job.key, started.elapsed().as_secs_f64());
                    }
                }
                Err(error) => {
                    // The error stays in the log file, so that `status` can show the last error.
                    // A background worker writes its standard error to that file.
                    if !log {
                        let _ = std::fs::OpenOptions::new().create(true).append(true).open(app.repo.cache_dir().join("worker.log")).and_then(|mut file| std::io::Write::write_all(&mut file, format!("[worker] {} failed: {error:#}\n", job.key).as_bytes()));
                    } else {
                        eprintln!("[worker] {} failed: {error:#}", job.key);
                    }
                }
            }
        }
        // A newer job for the same key may have replaced this file; remove only this one.
        let _ = std::fs::remove_file(&path);
    }
    app.mode = mode;
    lock.unlock()?;
    Ok(true)
}

pub(crate) fn store(app: &App, stored: &StoredReport) -> Result<()> {
    let file = report_file(app, &stored.key, is_marked(&stored.report));
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(&file, serde_json::to_string_pretty(stored)?)?;
    if stored.key != "session" {
        let note = app.repo.store.private_path("report-note");
        std::fs::write(&note, serde_json::to_string_pretty(stored)?)?;
        app.repo.git(&["notes", "--ref=semilla-report", "add", "-f", "-F", &note.to_string_lossy(), &stored.key])?;
        let _ = std::fs::remove_file(note);
    }
    Ok(())
}

/// The stored report of a key, if one exists.
pub fn stored(app: &App, key: &str) -> Option<StoredReport> {
    candidates(app, key).into_iter().next()
}

/// The stored results of a key that the command can read, in the order of use.
fn candidates(app: &App, key: &str) -> Vec<StoredReport> {
    let allowed = app.mode.runs_build_code();
    // A command with the permission reads the marked result first. A plain result serves it only
    // if that result is complete.
    let files = if allowed { vec![true, false] } else { vec![false] };
    files.into_iter().filter_map(|marked| {
        let text = std::fs::read_to_string(report_file(app, key, marked)).ok()?;
        let stored: StoredReport = serde_json::from_str(&text).ok()?;
        if stored.tools != app.tools_key() {
            return None;
        }
        // A report belongs to its commit: a report of a different tree (a moved branch, a copied
        // file) is not valid.
        if key != "session" && (stored.key != key || app.repo.commit_tree(key).ok().as_deref() != Some(stored.tree.as_str())) {
            return None;
        }
        // I6: the content decides, not only the file name.
        if is_marked(&stored.report) != marked || (allowed && stored.report.not_complete()) {
            return None;
        }
        Some(stored)
    }).collect()
}

/// The stored result of a key for a change set hash, in the mode of the command.
pub fn stored_for(app: &App, key: &str, hash: &str) -> Option<StoredReport> {
    candidates(app, key).into_iter().find(|stored| stored.hash == hash)
}

/// The state of the background check of a key.
#[derive(Debug, PartialEq, Eq)]
pub enum State {
    /// The stored report checks this change set.
    Ready,
    /// A job for this change set waits or runs.
    Queued,
    /// The stored report checks an older change set, and no job exists.
    Stale,
    /// No report and no job.
    Missing,
}

pub fn state(app: &App, key: &str, hash: &str) -> State {
    if stored_for(app, key, hash).is_some() {
        return State::Ready;
    }
    if jobs(app).iter().any(|(_, job)| job.key == key && job.mode == app.mode) {
        return State::Queued;
    }
    if stored(app, key).is_some() { State::Stale } else { State::Missing }
}

/// Waits until a key is ready, at most `timeout`. Runs the worker in this process when no worker
/// runs.
pub fn wait(app: &mut App, key: &str, hash: &str, timeout: std::time::Duration) -> Result<State> {
    let started = std::time::Instant::now();
    loop {
        let current = state(app, key, hash);
        if current != State::Queued || started.elapsed() > timeout {
            return Ok(current);
        }
        if !run_worker(app, false)? {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}
