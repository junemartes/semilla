//! The background check (phase 2, design section 19).
//!
//! - `step_end` and `tag` add a job to the queue in `.semilla/queue/`, and start a worker.
//! - The worker runs the full check of each job (the MIR builds, N1 to N4, SMT, indirect
//!   changes), and stores the report. It stops when the queue is empty. One worker runs at a time.
//! - A report has the hash of its change set. A changed change set makes the old report stale.
//!
//! The reports are local: `.semilla/reports/<key>.json`, and for a tagged commit also a git note
//! under `refs/notes/semilla-report`.

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
}

/// A stored report, with the hash of the change set that it checks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredReport {
    pub key: String,
    pub hash: String,
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
    app.repo.state_dir().join("queue")
}

fn report_file(app: &App, key: &str) -> PathBuf {
    app.repo.state_dir().join("reports").join(format!("{key}.json"))
}

/// Adds a job. A newer job for the same key replaces the old one.
pub fn enqueue(app: &App, job: &Job) -> Result<()> {
    let dir = queue_dir(app);
    std::fs::create_dir_all(&dir)?;
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(&format!("-{}.json", job.key))) {
            let _ = std::fs::remove_file(path);
        }
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    std::fs::write(dir.join(format!("{nanos:024}-{}.json", job.key)), serde_json::to_string(job)?)?;
    Ok(())
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
    if std::env::var_os("SEMILLA_NO_WORKER").is_some() {
        return Ok(());
    }
    use std::os::unix::process::CommandExt;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(app.repo.state_dir().join("worker.log"))?;
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

/// Runs all jobs, then returns. Returns false if another worker already runs.
pub fn run_worker(app: &mut App) -> Result<bool> {
    let lock = std::fs::File::create(app.repo.state_dir().join("worker.lock"))?;
    if lock.try_lock().is_err() {
        return Ok(false);
    }
    app.no_bodies = false;
    loop {
        let Some((path, job)) = jobs(app).into_iter().next() else { break };
        let set = match &job.changeset {
            Some(set) => Some(set.clone()),
            None => app.repo.read_note(&job.key).ok().and_then(|note| serde_json::from_str::<ChangeSet>(&note).ok()),
        };
        if let Some(set) = set {
            let started = std::time::Instant::now();
            match app.check_set(&set, job.key != "session") {
                Ok(result) => {
                    let stored = StoredReport { key: job.key.clone(), hash: job.hash.clone(), report: result.report, text: result.text };
                    store(app, &stored)?;
                    eprintln!("[worker] {} checked in {:.1} s", job.key, started.elapsed().as_secs_f64());
                }
                Err(error) => eprintln!("[worker] {} failed: {error:#}", job.key),
            }
        }
        // A newer job for the same key may have replaced this file; remove only this one.
        let _ = std::fs::remove_file(&path);
    }
    lock.unlock()?;
    Ok(true)
}

fn store(app: &App, stored: &StoredReport) -> Result<()> {
    let file = report_file(app, &stored.key);
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(&file, serde_json::to_string_pretty(stored)?)?;
    if stored.key != "session" {
        let note = app.repo.state_dir().join("report-note.json");
        std::fs::write(&note, serde_json::to_string_pretty(stored)?)?;
        app.repo.git(&["notes", "--ref=semilla-report", "add", "-f", "-F", &note.to_string_lossy(), &stored.key])?;
        let _ = std::fs::remove_file(note);
    }
    Ok(())
}

/// The stored report of a key, if one exists.
pub fn stored(app: &App, key: &str) -> Option<StoredReport> {
    let text = std::fs::read_to_string(report_file(app, key)).ok().or_else(|| {
        (key != "session").then(|| app.repo.git(&["notes", "--ref=semilla-report", "show", key]).ok()).flatten()
    })?;
    serde_json::from_str(&text).ok()
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
    if stored(app, key).is_some_and(|report| report.hash == hash) {
        return State::Ready;
    }
    if jobs(app).iter().any(|(_, job)| job.key == key) {
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
        if !run_worker(app)? {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}
