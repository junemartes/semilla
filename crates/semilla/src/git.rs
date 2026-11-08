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
        let index = self.state_dir().join("index");
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

    /// Writes a tree into a cache folder (once), and returns the folder.
    pub fn materialize(&self, tree: &str) -> Result<PathBuf> {
        let dir = self.state_dir().join("cache/trees").join(tree);
        // The marker is next to the folder, not inside it, so it is not part of the tree.
        let marker = self.state_dir().join("cache/trees").join(format!("{tree}.complete"));
        if marker.exists() {
            return Ok(dir);
        }
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let index = self.state_dir().join(format!("index-{tree}"));
        let env = [("GIT_INDEX_FILE", index.as_path())];
        self.git_env(&["read-tree", tree], &env)?;
        let prefix = format!("{}/", dir.display());
        self.git_env(&["checkout-index", "-a", "-f", &format!("--prefix={prefix}")], &env)?;
        let _ = std::fs::remove_file(&index);
        std::fs::write(marker, tree)?;
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
