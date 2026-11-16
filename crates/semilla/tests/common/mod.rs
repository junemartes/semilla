//! The test folder of the tests that run the `semilla` binary (batch 7d, part 3).
//!
//! A command of a test can start the background worker. The worker can run after the end of
//! the test. Then the drop of a `TempDir` removed the folder while the worker wrote to it, or
//! the worker made the folder again: each such run left a `.tmp*` folder in the temporary
//! folder of the system. A `TestDir` waits for each worker of the folder, and then it removes
//! the folder.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A temporary folder. The drop waits for each background worker of the folder, then it
/// removes the folder.
pub struct TestDir(Option<tempfile::TempDir>);

impl TestDir {
    pub fn new() -> TestDir {
        TestDir(Some(tempfile::tempdir().unwrap()))
    }

    pub fn path(&self) -> &Path {
        self.0.as_ref().unwrap().path()
    }
}

/// Each file `worker.lock` in the folder, to a depth of 4.
fn worker_locks(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            if depth > 0 && path.file_name().is_none_or(|name| name != ".git" && name != "target") {
                worker_locks(&path, depth - 1, out);
            }
        } else if path.file_name().is_some_and(|name| name == "worker.lock") {
            out.push(path);
        }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let Some(dir) = self.0.take() else { return };
        let path = dir.keep();
        // A worker holds the lock while it runs. A worker that starts late can make the folder
        // again: then the loop waits and removes it one more time.
        for _ in 0..200 {
            let mut locks = Vec::new();
            worker_locks(&path, 4, &mut locks);
            for lock in locks {
                if let Ok(file) = std::fs::File::open(&lock) {
                    let _ = file.lock();
                }
            }
            let _ = std::fs::remove_dir_all(&path);
            std::thread::sleep(Duration::from_millis(300));
            if !path.exists() {
                return;
            }
        }
    }
}
