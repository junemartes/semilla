//! The tasks of a step (design 96). A step is one graph of tasks: a task starts when its inputs
//! exist, and the builds run while Semilla does the work that does not read their results.
//!
//! This module has the parts that each task uses:
//!
//! - [`Keyed`]: one result for each key, made one time. A second reader of a key waits for the
//!   first one, and a failed task stops only its readers (R96.1-2).
//! - [`delay`]: the test mode of R96.6-1. Each task waits a random time from a seed before it
//!   starts, so a test can run each case with another order of the tasks.
//! - [`Building`] and [`threads`]: the share of the CPUs (R96.5-1). While a build runs, the tasks
//!   of Semilla use a limited count of threads.
//! - [`span`]: the time line of the tasks for the timing lines (R96.7-1).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// [R96.6-1] The variable of the test mode: a number (the seed). With it, each task waits a
/// random time from the seed before it starts. The variable changes no result.
pub const DELAY_ENV: &str = "SEMILLA_TASK_DELAY";

/// [R96.6-1] The longest wait of a task in the test mode.
const MOST_DELAY_MS: u64 = 40;

/// The seed of the test mode, read one time.
fn seed() -> Option<u64> {
    static SEED: OnceLock<Option<u64>> = OnceLock::new();
    *SEED.get_or_init(|| std::env::var(DELAY_ENV).ok().and_then(|text| text.trim().parse::<u64>().ok()))
}

/// The FNV-1a hash of a text.
fn fnv(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// A step of the generator `splitmix64`.
fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// [R96.6-1] The wait of the test mode for the task `task` with the seed `seed`, and the count
/// of the tasks that started before it in this process.
pub fn delay_of(seed: u64, task: &str, count: u64) -> Duration {
    Duration::from_millis(mix(seed ^ fnv(task) ^ mix(count)) % (MOST_DELAY_MS + 1))
}

/// [R96.6-1] In the test mode, waits a random time before the task `task` starts. With no seed,
/// it does nothing.
pub fn delay(task: &str) {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let Some(seed) = seed() else { return };
    let count = COUNT.fetch_add(1, Ordering::Relaxed);
    std::thread::sleep(delay_of(seed, task, count));
}

/// [R96.1-2] One result for each key, made one time. The first reader of a key runs the task;
/// a reader that comes while the task runs waits for its end. A result that is `Ok` stays for
/// each later reader. A failed task keeps no result: it stops only the readers that wait for it
/// now, and the next reader runs the task again (the behavior of a task with no shared result).
pub struct Keyed<K, T> {
    slots: Mutex<BTreeMap<K, Arc<Mutex<Option<T>>>>>,
}

impl<K, T> std::fmt::Debug for Keyed<K, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keyed")
    }
}

impl<K: Ord + Clone, T: Clone> Default for Keyed<K, T> {
    fn default() -> Self {
        Keyed { slots: Mutex::new(BTreeMap::new()) }
    }
}

impl<K: Ord + Clone, T: Clone> Keyed<K, T> {
    pub const fn new() -> Self {
        Keyed { slots: Mutex::new(BTreeMap::new()) }
    }

    /// The result of the key: the kept result, or the result of `run` (the task) now. Only one
    /// reader of a key runs `run` at a time.
    pub fn get_or_run<E>(&self, key: &K, run: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let slot = self.slots.lock().unwrap_or_else(|error| error.into_inner()).entry(key.clone()).or_default().clone();
        let mut kept = slot.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(found) = kept.as_ref() {
            return Ok(found.clone());
        }
        let made = run()?;
        *kept = Some(made.clone());
        Ok(made)
    }

    /// Runs `run` while no other reader of the key runs a task of the key. A reader that comes
    /// later waits for the end of `run`. The value of `run` is not kept.
    pub fn alone<R>(&self, key: &K, run: impl FnOnce() -> R) -> R {
        let slot = self.slots.lock().unwrap_or_else(|error| error.into_inner()).entry(key.clone()).or_default().clone();
        let _kept = slot.lock().unwrap_or_else(|error| error.into_inner());
        run()
    }

    /// Removes each kept result (for a test).
    pub fn clear(&self) {
        self.slots.lock().unwrap_or_else(|error| error.into_inner()).clear();
    }
}

/// [R96.5-1] The count of the builds that run now in this process.
static BUILDS: AtomicUsize = AtomicUsize::new(0);

/// [R96.5-1] A build that runs: while one exists, the tasks of Semilla use at most
/// [`limit`] threads.
pub struct Building;

impl Building {
    pub fn start() -> Building {
        BUILDS.fetch_add(1, Ordering::SeqCst);
        Building
    }
}

impl Drop for Building {
    fn drop(&mut self) {
        BUILDS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// True if a build runs now in this process.
pub fn building() -> bool {
    BUILDS.load(Ordering::SeqCst) > 0
}

/// [R96.5-1] The most threads of the tasks of Semilla while a build runs: a third of the CPUs of
/// this process, and at least one. Cargo uses the other CPUs.
pub fn limit() -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, |count| count.get());
    (cpus / 3).max(1)
}

/// [R96.5-1] The count of threads for a task of Semilla that can use `most` threads: the limit
/// while a build runs, else `most`.
pub fn threads(most: usize) -> usize {
    let most = most.max(1);
    if building() { most.min(limit()) } else { most }
}

/// The start of the time line of this process.
fn origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// [R96.7-1] Starts the time line of the tasks (the first call fixes the start).
pub fn start_line() {
    let _ = origin();
}

/// [R96.7-1] The runs of the tasks of this process: the name, the start, and the end.
static LINE: Mutex<Vec<(&'static str, Duration, Duration)>> = Mutex::new(Vec::new());

/// [R96.7-1] One run of a task on the time line. The end of the value ends the run.
pub struct Span {
    name: &'static str,
    started: Duration,
}

/// [R96.7-1] Starts one run of the task `name` on the time line. With no timing lines, the
/// run is not kept.
pub fn span(name: &'static str) -> Option<Span> {
    crate::timing::enabled().then(|| Span { name, started: origin().elapsed() })
}

impl Drop for Span {
    fn drop(&mut self) {
        let ended = origin().elapsed();
        LINE.lock().unwrap_or_else(|error| error.into_inner()).push((self.name, self.started, ended));
    }
}

/// [R96.7-1] Puts one run of a phase on the time line: from `started` to now.
pub fn add_run(name: &'static str, started: Instant) {
    let start = started.saturating_duration_since(origin());
    LINE.lock().unwrap_or_else(|error| error.into_inner()).push((name, start, origin().elapsed()));
}

/// [R96.7-1] Prints one timing line for each run of a task, in the order of the starts, and
/// starts a new time line.
pub fn print_line() {
    let runs = std::mem::take(&mut *LINE.lock().unwrap_or_else(|error| error.into_inner()));
    if !crate::timing::enabled() {
        return;
    }
    for line in line_texts(runs) {
        eprintln!("{line}");
    }
}

/// [R96.7-1] The lines of the time line: one line for each run, in the order of the starts (a
/// start of two runs has the order of the names).
fn line_texts(mut runs: Vec<(&'static str, Duration, Duration)>) -> Vec<String> {
    runs.sort_by_key(|(name, start, _)| (*start, *name));
    runs.into_iter().map(|(name, start, end)| format!("[timing] task {name}: from {:.3} s to {:.3} s", start.as_secs_f64(), end.as_secs_f64())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // rule+ R96.6-1
    #[test]
    fn the_wait_of_a_task_comes_from_the_seed_and_has_a_limit() {
        // The same seed, task, and count give the same wait; the waits are not all equal.
        assert_eq!(delay_of(7, "build", 3), delay_of(7, "build", 3));
        let waits: std::collections::BTreeSet<Duration> = (0..50).map(|count| delay_of(7, "build", count)).collect();
        assert!(waits.len() > 5, "{waits:?}");
        assert!(waits.iter().all(|wait| *wait <= Duration::from_millis(MOST_DELAY_MS)));
    }

    // rule- R96.6-1
    #[test]
    fn another_seed_gives_other_waits() {
        let one: Vec<Duration> = (0..20).map(|count| delay_of(1, "read", count)).collect();
        let two: Vec<Duration> = (0..20).map(|count| delay_of(2, "read", count)).collect();
        assert_ne!(one, two);
    }

    // rule+ R96.1-2
    #[test]
    fn a_second_reader_of_a_key_waits_and_gets_the_result_of_the_first() {
        let keyed: Keyed<String, usize> = Keyed::new();
        let runs = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    let found: Result<usize, String> = keyed.get_or_run(&"k".to_string(), || {
                        std::thread::sleep(Duration::from_millis(20));
                        Ok(runs.fetch_add(1, Ordering::SeqCst) + 10)
                    });
                    assert_eq!(found, Ok(10));
                });
            }
        });
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    // rule- R96.1-2
    #[test]
    fn a_failed_task_keeps_no_result_and_another_key_is_not_stopped() {
        let keyed: Keyed<String, usize> = Keyed::new();
        let failed: Result<usize, String> = keyed.get_or_run(&"a".to_string(), || Err("no build".to_string()));
        assert_eq!(failed, Err("no build".to_string()));
        // Another key does not read the failed task: it runs.
        assert_eq!(keyed.get_or_run(&"b".to_string(), || Ok::<usize, String>(2)), Ok(2));
        // The next reader of the failed key runs the task again.
        assert_eq!(keyed.get_or_run(&"a".to_string(), || Ok::<usize, String>(3)), Ok(3));
    }

    // rule+ R96.7-1
    #[test]
    fn the_time_line_has_each_run_in_the_order_of_the_starts() {
        let ms = Duration::from_millis;
        let lines = line_texts(vec![("b build", ms(300), ms(900)), ("a load", ms(100), ms(200)), ("c read", ms(300), ms(400))]);
        assert_eq!(lines, ["[timing] task a load: from 0.100 s to 0.200 s", "[timing] task b build: from 0.300 s to 0.900 s", "[timing] task c read: from 0.300 s to 0.400 s"]);
    }

    // rule- R96.7-1
    #[test]
    fn with_no_timing_lines_a_task_has_no_run_on_the_time_line() {
        if !crate::timing::enabled() {
            assert!(span("a task").is_none());
        }
        assert!(line_texts(Vec::new()).is_empty());
    }

    // rule+ R96.5-1
    // rule- R96.5-1
    #[test]
    fn a_build_that_runs_limits_the_threads_of_a_task() {
        let most = 64;
        // Negative: with no build, a task has its own count.
        if !building() {
            assert_eq!(threads(most), most);
        }
        let build = Building::start();
        let cpus = std::thread::available_parallelism().map_or(1, |count| count.get());
        assert_eq!(limit(), (cpus / 3).max(1));
        assert_eq!(threads(most), limit().min(most));
        assert!(building());
        drop(build);
    }
}
