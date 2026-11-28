//! [R93.5-1] The parts of the time of the driver for one compile (design 93.5): the time and
//! the count of each part. The timing line of the compile (R91.5-1) has them, so Semilla can
//! show where the time of the driver goes. A part changes no record.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// One part of the work of the driver.
#[derive(Clone, Copy)]
pub(crate) enum Part {
    /// The lists after the expansion and the unit record: the expanded items, the expansions,
    /// the trait table, the name record, and the definitions of the crate.
    Unit,
    /// The list of the local items and the start of the printer.
    Items,
    /// The MIR queries of each body (the body of the instance, or the generic body).
    Mir,
    /// The canonical body and the print of its text, its shape, and its calls.
    Text,
    /// The source range of each MIR element (`el`).
    Elements,
    /// The control flow graph (`g`).
    Graph,
    /// The reach of a body: the types, the impls, the live and the sure callees, the uses.
    Reach,
    /// The names of the expanded code of a body.
    Names,
    /// The forms of the equivalence check (`q`, `qx`, the constant values, the instances).
    Forms,
    /// The JSON text of the records.
    Json,
    /// The write of the record file.
    Write,
    /// [R93.3-1] The keys of the records: the hash of the unit and the key of each body.
    Key,
}

/// The names of the parts in the timing line, in the order of [`Part`].
pub(crate) const NAMES: [&str; 12] = ["unit", "items", "mir", "text", "elements", "graph", "reach", "names", "forms", "json", "write", "key"];

static NANOS: [AtomicU64; 12] = [const { AtomicU64::new(0) }; 12];
static COUNTS: [AtomicU64; 12] = [const { AtomicU64::new(0) }; 12];

/// Runs `work` as one run of a part.
pub(crate) fn timed<T>(part: Part, work: impl FnOnce() -> T) -> T {
    let started = std::time::Instant::now();
    let result = work();
    NANOS[part as usize].fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
    COUNTS[part as usize].fetch_add(1, Relaxed);
    result
}

/// Adds one run of a part, from `started` to now.
pub(crate) fn add(part: Part, started: std::time::Instant) {
    NANOS[part as usize].fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
    COUNTS[part as usize].fetch_add(1, Relaxed);
}

/// Adds time to a part, with no run (a second piece of one run).
pub(crate) fn add_time(part: Part, started: std::time::Instant) {
    NANOS[part as usize].fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
}

/// [R93.3-1] The count of the bodies with a pointer line (the store has their record).
static POINTERS: AtomicU64 = AtomicU64::new(0);

/// Counts one pointer line.
pub(crate) fn count_pointer() {
    POINTERS.fetch_add(1, Relaxed);
}

/// [R93.5-2] The count of the results that a memo of the driver gave again (a path text, the
/// form of a callee).
static MEMO: AtomicU64 = AtomicU64::new(0);

/// Counts one result of a memo.
pub(crate) fn count_memo() {
    MEMO.fetch_add(1, Relaxed);
}

/// The parts for the timing line: `parts=NAME:NANOS:COUNT,…`, with the count of the results of
/// the memos as the part `memo` (no time).
pub(crate) fn field() -> String {
    let mut parts: Vec<String> = NAMES.iter().enumerate().map(|(at, name)| format!("{name}:{}:{}", NANOS[at].load(Relaxed), COUNTS[at].load(Relaxed))).collect();
    parts.push(format!("memo:0:{}", MEMO.load(Relaxed)));
    parts.push(format!("pointers:0:{}", POINTERS.load(Relaxed)));
    format!("parts={}", parts.join(","))
}

/// [R93.5-2] A memo of the driver: the value of `key` in `memo`, or the value of `make`. A value
/// whose work called `print::unique` is not kept: its text is never equal to another, so each
/// caller makes its own, as a full run does.
pub(crate) fn kept<K: std::hash::Hash + Eq, V: Clone>(memo: &std::cell::RefCell<std::collections::HashMap<K, V>>, key: K, make: impl FnOnce() -> V) -> V {
    if let Some(found) = memo.borrow().get(&key).cloned() {
        count_memo();
        return found;
    }
    let before = crate::print::unique_count();
    let made = make();
    if crate::print::unique_count() == before {
        memo.borrow_mut().insert(key, made.clone());
    }
    made
}

#[cfg(test)]
mod tests {
    use super::*;

    // [R93.5-2] A memo gives the kept value again, and the path text of a memo is the text of a
    // full computation.
    // rule+ R93.5-2
    #[test]
    fn a_memo_gives_its_value_again() {
        let memo = std::cell::RefCell::new(std::collections::HashMap::new());
        assert_eq!(kept(&memo, 1, || "one".to_string()), "one");
        assert_eq!(kept(&memo, 1, || "two".to_string()), "one");
        for path in ["src/lib.rs", "./src/../src/lib.rs", "/tmp/x.rs"] {
            let path = std::path::Path::new(path);
            assert_eq!(crate::workspace_path(path), crate::workspace_path_text(path));
            assert_eq!(crate::workspace_path(path), crate::workspace_path_text(path));
        }
    }

    // [R93.5-2] The negative case: a value whose work called `unique` is made again for each
    // caller.
    // rule- R93.5-2
    #[test]
    fn a_value_with_a_unique_word_is_not_kept() {
        let memo = std::cell::RefCell::new(std::collections::HashMap::new());
        let first = kept(&memo, 1, || crate::print::unique("test"));
        let second = kept(&memo, 1, || crate::print::unique("test"));
        assert_ne!(first, second);
    }
}
