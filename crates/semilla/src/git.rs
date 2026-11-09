//! The git operations of semilla. Semilla stays local: it never pushes.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// A git repository with a working tree.
#[derive(Clone, Debug)]
pub struct Repo {
    pub root: PathBuf,
}

impl Repo {
    /// Finds the repository that contains `dir`.
    pub fn discover(dir: &Path) -> Result<Repo> {
        let output = Command::new("git").arg("-C").arg(dir).args(["rev-parse", "--show-toplevel"]).output().context("run git")?;
        if !output.status.success() {
            bail!("{} is not in a git repository", dir.display());
        }
        Ok(Repo { root: PathBuf::from(String::from_utf8(output.stdout)?.trim()) })
    }

    /// The `.semilla` folder of the repository.
    pub fn state_dir(&self) -> PathBuf {
        self.root.join(".semilla")
    }

    /// Runs git and returns its standard output.
    pub fn git(&self, args: &[&str]) -> Result<String> {
        self.git_env(args, &[])
    }

    fn git_env(&self, args: &[&str], env: &[(&str, &Path)]) -> Result<String> {
        let mut command = Command::new("git");
        command.arg("-C").arg(&self.root).args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().with_context(|| format!("run git {}", args.join(" ")))?;
        if !output.status.success() {
            bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    /// Saves the working tree as a git tree, and returns its hash.
    ///
    /// A private index file keeps the user's index unchanged. `.gitignore` applies, so `target`
    /// and `.semilla` are not in the tree.
    pub fn write_tree(&self) -> Result<String> {
        let index = self.private_path("index");
        std::fs::create_dir_all(self.state_dir())?;
        let _ = std::fs::remove_file(&index);
        let env = [("GIT_INDEX_FILE", index.as_path())];
        if self.git(&["rev-parse", "--verify", "-q", "HEAD"]).is_ok() {
            self.git_env(&["read-tree", "HEAD"], &env)?;
        }
        self.git_env(&["add", "-A", "."], &env)?;
        let tree = self.git_env(&["write-tree"], &env)?;
        let _ = std::fs::remove_file(&index);
        Ok(tree)
    }

    /// The tree of a commit.
    pub fn commit_tree(&self, commit: &str) -> Result<String> {
        self.git(&["rev-parse", &format!("{commit}^{{tree}}")])
    }

    /// A path in the state folder that only this process uses (for index files).
    fn private_path(&self, name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        self.state_dir().join(format!("{name}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)))
    }

    /// Writes a tree into a cache folder (once), and returns the folder.
    ///
    /// The tree goes into a private folder first, then a rename puts it in place. The rename is
    /// atomic, so a cache folder that exists is complete, also when other processes write the same
    /// tree at the same time.
    pub fn materialize(&self, tree: &str) -> Result<PathBuf> {
        let base = self.state_dir().join("cache/snapshots");
        let dir = base.join(tree);
        if dir.exists() {
            return Ok(dir);
        }
        std::fs::create_dir_all(&base)?;
        let temp = self.private_path("snapshot");
        let index = self.private_path("index");
        let env = [("GIT_INDEX_FILE", index.as_path())];
        self.git_env(&["read-tree", tree], &env)?;
        let prefix = format!("{}/", temp.display());
        self.git_env(&["checkout-index", "-a", "-f", &format!("--prefix={prefix}")], &env)?;
        let _ = std::fs::remove_file(&index);
        std::fs::create_dir_all(&temp)?;
        if std::fs::rename(&temp, &dir).is_err() {
            // Another process put the same tree in place first.
            let _ = std::fs::remove_dir_all(&temp);
        }
        Ok(dir)
    }

    /// Updates a fixed build folder to a tree. Git writes only the files that differ from the
    /// last tree of the folder, and removes the files that the new tree does not have. So the
    /// unchanged files keep their times, and cargo can reuse its work for them.
    pub fn checkout_build(&self, dir: &Path, tree: &str) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let index = dir.with_extension("index");
        let work_tree = format!("--work-tree={}", dir.display());
        self.git_env(&[&work_tree, "read-tree", "--reset", "-u", tree], &[("GIT_INDEX_FILE", index.as_path())])?;
        Ok(())
    }

    /// Changes the files of the working tree from tree `from` to tree `to`. The working tree must
    /// be `from`. A private index keeps the user's index unchanged. Git writes only the files that
    /// differ, and removes the files that `to` does not have. Ignored files stay.
    pub fn switch_worktree(&self, from: &str, to: &str) -> Result<()> {
        if from == to {
            return Ok(());
        }
        let index = self.private_path("index");
        let env = [("GIT_INDEX_FILE", index.as_path())];
        self.git_env(&["read-tree", from], &env)?;
        // Fill the stat data, so that git does not write the unchanged files again.
        let _ = self.git_env(&["update-index", "-q", "--refresh"], &env);
        let result = self.git_env(&["read-tree", "--reset", "-u", to], &env);
        let _ = std::fs::remove_file(&index);
        result.map(|_| ())
    }

    /// The line hunks between two trees (`git diff -U0`), for all files or for one file.
    pub fn hunks(&self, from: &str, to: &str, file: Option<&str>) -> Result<Vec<Hunk>> {
        let mut args = vec!["diff", "-U0", "--no-renames", "--no-color", "--no-ext-diff", from, to];
        if let Some(file) = file {
            args.extend(["--", file]);
        }
        Ok(parse_hunks(&self.git(&args)?))
    }

    /// A new tree: `base` with some files replaced (`Some`) or removed (`None`). The working tree
    /// and the user's index do not change.
    pub fn tree_with(&self, base: &str, files: &std::collections::BTreeMap<String, Option<String>>) -> Result<String> {
        let index = self.private_path("index");
        let blob = self.private_path("blob");
        let env = [("GIT_INDEX_FILE", index.as_path())];
        let result = (|| -> Result<String> {
            self.git_env(&["read-tree", base], &env)?;
            for (path, content) in files {
                match content {
                    Some(text) => {
                        std::fs::write(&blob, text)?;
                        let hash = self.git(&["hash-object", "-w", &blob.to_string_lossy()])?;
                        self.git_env(&["update-index", "--add", "--cacheinfo", &format!("100644,{hash},{path}")], &env)?;
                    }
                    None => {
                        self.git_env(&["update-index", "--force-remove", path], &env)?;
                    }
                }
            }
            self.git_env(&["write-tree"], &env)
        })();
        let _ = std::fs::remove_file(&index);
        let _ = std::fs::remove_file(&blob);
        result
    }

    /// The files that differ between two trees.
    pub fn changed_files(&self, from: &str, to: &str) -> Result<Vec<String>> {
        Ok(self.git(&["diff", "--name-only", "--no-renames", from, to])?.lines().map(str::to_string).collect())
    }

    /// Stores a note on a commit, under `refs/notes/semilla`.
    pub fn write_note(&self, commit: &str, text: &str) -> Result<()> {
        let file = self.state_dir().join("note.json");
        std::fs::write(&file, text)?;
        self.git(&["notes", "--ref=semilla", "add", "-f", "-F", &file.to_string_lossy(), commit])?;
        let _ = std::fs::remove_file(&file);
        Ok(())
    }

    /// Reads the note of a commit.
    pub fn read_note(&self, commit: &str) -> Result<String> {
        self.git(&["notes", "--ref=semilla", "show", commit])
    }

    /// Keeps a tree reachable with a ref, so that `git gc` does not remove it.
    pub fn keep_tree(&self, reference: &str, tree: &str, message: &str) -> Result<()> {
        let commit = self.git(&["commit-tree", tree, "-m", message])?;
        self.git(&["update-ref", reference, &commit])?;
        Ok(())
    }
}

/// One hunk of a line diff. The old lines are in the "from" tree, the new lines in the "to" tree.
/// A count of 0 means an insert (old) or a delete (new) after the start line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub file: String,
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    /// The `-` and `+` lines of the hunk.
    pub lines: Vec<String>,
}

impl Hunk {
    /// The lines of the hunk in the new file. A delete has the line before it.
    pub fn new_range(&self) -> (usize, usize) {
        (self.new_start, self.new_start + self.new_count.max(1) - 1)
    }

    /// The lines of the hunk in the old file. An insert has the line before it.
    pub fn old_range(&self) -> (usize, usize) {
        (self.old_start, self.old_start + self.old_count.max(1) - 1)
    }
}

/// Parses the output of `git diff -U0`.
pub fn parse_hunks(text: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut old_file, mut file) = (String::new(), String::new());
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("--- ") {
            old_file = rest.strip_prefix("a/").unwrap_or(rest).to_string();
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            file = if rest == "/dev/null" { old_file.clone() } else { rest.strip_prefix("b/").unwrap_or(rest).to_string() };
        } else if let Some(rest) = line.strip_prefix("@@ -") {
            let Some((ranges, _)) = rest.split_once(" @@") else { continue };
            let Some((old, new)) = ranges.split_once(" +") else { continue };
            let range = |text: &str| -> (usize, usize) {
                match text.split_once(',') {
                    Some((start, count)) => (start.parse().unwrap_or(0), count.parse().unwrap_or(0)),
                    None => (text.parse().unwrap_or(0), 1),
                }
            };
            let ((old_start, old_count), (new_start, new_count)) = (range(old), range(new));
            hunks.push(Hunk { file: file.clone(), old_start, old_count, new_start, new_count, lines: Vec::new() });
        } else if (line.starts_with('-') || line.starts_with('+')) && !hunks.is_empty() {
            hunks.last_mut().unwrap().lines.push(line.to_string());
        } else if line.starts_with("diff --git") {
            old_file.clear();
            file.clear();
        }
    }
    hunks
}

/// Applies a subset of the hunks of one file. The hunks come from one diff of `old` to `new`, so
/// their old line numbers all refer to `old`.
pub fn apply_hunks(old: &str, new: &str, hunks: &[&Hunk]) -> String {
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let mut sorted: Vec<&&Hunk> = hunks.iter().collect();
    sorted.sort_by_key(|h| h.old_start);
    let mut out = String::new();
    let mut position = 0;
    for hunk in sorted {
        let start = if hunk.old_count == 0 { hunk.old_start } else { hunk.old_start - 1 };
        let start = start.max(position).min(old_lines.len());
        out.extend(old_lines[position..start].iter().copied());
        if hunk.new_count > 0 {
            let from = (hunk.new_start - 1).min(new_lines.len());
            let to = (from + hunk.new_count).min(new_lines.len());
            out.extend(new_lines[from..to].iter().copied());
        }
        position = (start + hunk.old_count).min(old_lines.len());
    }
    out.extend(old_lines[position..].iter().copied());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunks_apply_one_at_a_time() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nB\nc\nx\ny\nd\n";
        // The diff of old to new: line 2 changes, two lines after line 3 are new, line 5 is removed.
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -2 +2 @@\n-b\n+B\n@@ -3,0 +4,2 @@\n+x\n+y\n@@ -5 +6,0 @@\n-e\n";
        let hunks = parse_hunks(diff);
        assert_eq!(hunks.len(), 3);
        assert_eq!(hunks[1].new_range(), (4, 5));
        assert_eq!(apply_hunks(old, new, &[&hunks[1]]), "a\nb\nc\nx\ny\nd\ne\n");
        assert_eq!(apply_hunks(old, new, &[&hunks[0], &hunks[2]]), "a\nB\nc\nd\n");
        let all: Vec<&Hunk> = hunks.iter().collect();
        assert_eq!(apply_hunks(old, new, &all), new);
    }

    #[test]
    fn insert_at_the_start_and_new_file() {
        let diff = "diff --git a/f b/f\nnew file mode 100644\n--- /dev/null\n+++ b/f\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let hunks = parse_hunks(diff);
        assert_eq!(hunks[0].file, "f");
        assert_eq!(apply_hunks("", "x\ny\n", &[&hunks[0]]), "x\ny\n");
    }
}
