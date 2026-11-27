//! The timing lines of a step (design 86, S5). With `SEMILLA_TIMING` set, each phase of a step
//! prints one line to stderr: its total time and its count of runs. A phase can also count its
//! work (for example the records that it reads). A test reads the counts, never a time.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

/// The total of one phase: the time, the count of runs, and the counts of its work.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Total {
    pub nanos: u64,
    pub runs: u64,
    /// The counts of the work, by name (for example `records parsed`).
    pub counts: BTreeMap<&'static str, u64>,
}

/// The totals of this process, by phase name.
static TOTALS: Mutex<BTreeMap<&'static str, Total>> = Mutex::new(BTreeMap::new());

fn totals() -> std::sync::MutexGuard<'static, BTreeMap<&'static str, Total>> {
    TOTALS.lock().unwrap_or_else(|error| error.into_inner())
}

/// True if the timing lines are on.
pub fn enabled() -> bool {
    std::env::var_os("SEMILLA_TIMING").is_some()
}

/// Adds one run of a phase, from `started` to now.
pub fn add(phase: &'static str, started: Instant) {
    let nanos = started.elapsed().as_nanos() as u64;
    let mut all = totals();
    let total = all.entry(phase).or_default();
    total.nanos += nanos;
    total.runs += 1;
}

/// Adds `count` to a count of the work of a phase (with no run and no time).
pub fn count(phase: &'static str, name: &'static str, count: u64) {
    *totals().entry(phase).or_default().counts.entry(name).or_default() += count;
}

/// Runs `work` as one run of a phase.
pub fn phase<R>(name: &'static str, work: impl FnOnce() -> R) -> R {
    let started = Instant::now();
    let result = work();
    add(name, started);
    result
}

/// A copy of the totals of this process (for a test).
pub fn snapshot() -> BTreeMap<&'static str, Total> {
    totals().clone()
}

/// The total of one phase now (for a test). A phase with no run is the default.
pub fn total(phase: &str) -> Total {
    totals().get(phase).cloned().unwrap_or_default()
}

/// Prints one line for each phase with a run or a count, and starts new totals. The order is
/// the order of the names, so that two runs give the lines in one order.
pub fn print_all() {
    let all = std::mem::take(&mut *totals());
    if !enabled() {
        return;
    }
    for (phase, total) in all {
        let counts: Vec<String> = total.counts.iter().map(|(name, count)| format!("{count} {name}")).collect();
        let counts = if counts.is_empty() { String::new() } else { format!(", {}", counts.join(", ")) };
        if total.runs == 0 {
            eprintln!("[timing] count of {phase} ({})", counts.trim_start_matches(", "));
        } else {
            eprintln!("[timing] total of {phase} ({} times{counts}): {:.2} s", total.runs, total.nanos as f64 / 1e9);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // rule+ R86.5-1
    #[test]
    fn a_phase_counts_its_runs_and_its_work() {
        let before = total("test phase a");
        phase("test phase a", || ());
        phase("test phase a", || ());
        count("test phase a", "records", 3);
        let after = total("test phase a");
        assert_eq!(after.runs, before.runs + 2);
        assert_eq!(after.counts.get("records").copied().unwrap_or(0), before.counts.get("records").copied().unwrap_or(0) + 3);
    }

    // rule- R86.5-1
    #[test]
    fn another_phase_has_its_own_total() {
        phase("test phase b", || ());
        count("test phase c", "records", 2);
        assert_eq!(total("test phase b").counts.get("records"), None);
        assert_eq!(total("test phase c").runs, 0);
        assert_eq!(total("test phase never").runs, 0);
    }
}
