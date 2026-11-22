//! The line diff of a step (design 55.8). The changed lines of a step are the lines that one
//! `git diff -U0` command shows between the old tree and the new tree. Semilla runs the
//! command of [`semilla_core::report::LINE_DIFF_COMMAND`] on a copy of the changed text files
//! of the two trees. The command gets no configuration of the user, and no attribute file.
//! Thus the hunks are the hunks that the command shows for the two trees of the step.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// One hunk of one file, with the numbers of the hunk header: the start and the count of the
/// old side, then of the new side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GitHunk {
    pub old: (usize, usize),
    pub new: (usize, usize),
}

/// One changed file: the text of each tree, or `None` if the tree does not have the file.
pub(crate) struct Texts<'a> {
    pub old: Option<&'a str>,
    pub new: Option<&'a str>,
}

/// The variables that can change the result of a git command. Semilla removes each one.
const REMOVED_ENV: &[&str] = &["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_EXTERNAL_DIFF", "GIT_DIFF_OPTS", "GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT", "GIT_CONFIG", "GIT_NAMESPACE", "GIT_ATTR_SOURCE"];

/// A folder that the function removes at its end.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A new empty folder in the temporary folder of the process.
fn scratch() -> Option<Scratch> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    for _ in 0..16 {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|time| time.subsec_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("semilla-linediff-{}-{}-{stamp}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        if std::fs::create_dir(&dir).is_ok() {
            return Some(Scratch(dir));
        }
    }
    None
}

/// The options of the command, without `git diff` and without the two trees.
fn options() -> Vec<&'static str> {
    semilla_core::report::LINE_DIFF_COMMAND.split(' ').skip(2).collect()
}

/// Runs the line diff command on the files. The result has the hunks of each file, in the
/// order of the list. `None`: git did not start, or its output is not a diff of the files.
pub(crate) fn git_hunks(files: &[Texts]) -> Option<Vec<Vec<GitHunk>>> {
    if std::env::var_os("SEMILLA_OWN_LINE_DIFF").is_some() {
        return None;
    }
    let scratch = scratch()?;
    let root = &scratch.0;
    // The name of a file is its index: a name has no effect on the lines of a diff, and no
    // file of the trees can be a configuration file or an attribute file of this command.
    for (side, pick) in [("a", 0), ("b", 1)] {
        let dir = root.join(side);
        std::fs::create_dir(&dir).ok()?;
        for (index, texts) in files.iter().enumerate() {
            if let Some(text) = if pick == 0 { texts.old } else { texts.new } {
                std::fs::write(dir.join(index.to_string()), text).ok()?;
            }
        }
    }
    let output = run(root)?;
    let mut out = vec![Vec::new(); files.len()];
    let mut current: Option<usize> = None;
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `diff --git a/a/12 b/b/12`: the index is the last part of the line.
            let index: usize = rest.rsplit('/').next()?.parse().ok()?;
            if index >= files.len() {
                return None;
            }
            current = Some(index);
        } else if let Some(rest) = line.strip_prefix("@@ -") {
            let (old, rest) = rest.split_once(" +")?;
            let (new, _) = rest.split_once(" @@")?;
            out[current?].push(GitHunk { old: header_side(old)?, new: header_side(new)? });
        }
    }
    // The check of the output: the lines that are in no hunk are equal lines of the two texts.
    for (texts, hunks) in files.iter().zip(&out) {
        if !agrees(texts, hunks) {
            return None;
        }
    }
    Some(out)
}

/// One side of a hunk header: `START,COUNT`, or `START` for the count 1.
fn header_side(text: &str) -> Option<(usize, usize)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// The command on the folders `a` and `b` of `root`.
fn run(root: &Path) -> Option<String> {
    let mut command = Command::new("git");
    command.arg("--no-pager");
    for setting in ["core.quotePath=false", "core.autocrlf=false", "core.safecrlf=false", "core.attributesFile=/dev/null", "core.fsmonitor=false"] {
        command.arg("-c").arg(setting);
    }
    command.arg("diff").arg("--no-index");
    command.args(options());
    command.args(["--src-prefix=a/", "--dst-prefix=b/", "a", "b"]);
    for name in REMOVED_ENV {
        command.env_remove(name);
    }
    command.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_SYSTEM", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_ATTR_NOSYSTEM", "1").env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0").env("LC_ALL", "C");
    // Git does not look for a repository above the folder.
    command.env("GIT_CEILING_DIRECTORIES", root);
    command.current_dir(root).stdin(Stdio::null()).stderr(Stdio::null());
    let output = command.output().ok()?;
    // The exit code of `git diff --no-index`: 0 (no difference) or 1 (a difference).
    if !matches!(output.status.code(), Some(0) | Some(1)) {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The lines of a text, each with its line ending.
pub(crate) fn full_lines(text: Option<&str>) -> Vec<&str> {
    text.map(|text| text.split_inclusive('\n').collect()).unwrap_or_default()
}

/// True if the hunks are a diff of the two texts: the hunks are in order, and each line that is
/// in no hunk is equal in the two texts.
fn agrees(texts: &Texts, hunks: &[GitHunk]) -> bool {
    let (old, new) = (full_lines(texts.old), full_lines(texts.new));
    let (mut a, mut b) = (0, 0);
    for hunk in hunks {
        // A side with the count 0 has the number of the line before the hunk.
        let start = |side: (usize, usize)| if side.1 == 0 { side.0 } else { side.0.saturating_sub(1) };
        let (next_a, next_b) = (start(hunk.old), start(hunk.new));
        if next_a < a || next_b < b || next_a - a != next_b - b || next_a + hunk.old.1 > old.len() || next_b + hunk.new.1 > new.len() {
            return false;
        }
        if old[a..next_a] != new[b..next_b] {
            return false;
        }
        (a, b) = (next_a + hunk.old.1, next_b + hunk.new.1);
    }
    old[a..] == new[b..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_gives_the_hunks_of_each_file() {
        let files = [
            Texts { old: Some("a\nb\nc\n"), new: Some("a\nB\nc\nd\n") },
            Texts { old: None, new: Some("new\n") },
            Texts { old: Some("x\ny\n"), new: None },
            Texts { old: Some("1\n2\n3\n"), new: Some("1\n3\n") },
        ];
        let hunks = git_hunks(&files).expect("git runs");
        assert_eq!(hunks[0], [GitHunk { old: (2, 1), new: (2, 1) }, GitHunk { old: (3, 0), new: (4, 1) }]);
        assert_eq!(hunks[1], [GitHunk { old: (0, 0), new: (1, 1) }]);
        assert_eq!(hunks[2], [GitHunk { old: (1, 2), new: (0, 0) }]);
        assert_eq!(hunks[3], [GitHunk { old: (2, 1), new: (1, 0) }]);
        // A line with a different line ending is a changed line of git.
        let hunks = git_hunks(&[Texts { old: Some("a\r\nb\n"), new: Some("a\nb\n") }]).unwrap();
        assert_eq!(hunks[0], [GitHunk { old: (1, 1), new: (1, 1) }]);
        // Negative: equal texts have no hunk.
        assert_eq!(git_hunks(&[Texts { old: Some("a\n"), new: Some("a\n") }]).unwrap()[0], []);
    }

    #[test]
    fn hunks_that_are_not_a_diff_of_the_texts_do_not_agree() {
        let texts = Texts { old: Some("a\nb\nc\n"), new: Some("a\nB\nc\n") };
        assert!(agrees(&texts, &[GitHunk { old: (2, 1), new: (2, 1) }]));
        // Negative: the hunk is at a line that is equal, and the changed line is in no hunk.
        assert!(!agrees(&texts, &[GitHunk { old: (1, 1), new: (1, 1) }]));
        assert!(!agrees(&texts, &[]));
        assert!(!agrees(&texts, &[GitHunk { old: (2, 5), new: (2, 1) }]));
    }
}
