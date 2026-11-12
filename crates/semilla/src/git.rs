//! The git operations of semilla. Semilla stays local: it never pushes.
//!
//! The agent controls the repository folder: the working tree, the git objects, the refs, the
//! notes, `.git/config`, `.git/info`, and `.git/hooks` (design 27). So each git command here
//! obeys these rules:
//!
//! - It runs no hook and no program that the repository configuration names. [`SAFE_CONFIG`]
//!   and [`SAFE_ENV`] apply to each command.
//! - A command that reads a tree for a result is plumbing that reads objects as data
//!   (`ls-tree`, `cat-file`, `rev-parse`, `diff` of two trees with no text conversion).
//! - Semilla writes the files of a tree from the raw blobs. No filter, no attribute, no
//!   conversion of line ends, and no hook applies.

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use semilla_syntax::load::FileEntry;

use crate::store::Store;

/// The configuration that each git command of Semilla gets with `-c`. It stops each hook and
/// each program that the configuration of the repository can name.
pub const SAFE_CONFIG: &[&str] = &[
    // No hook runs (`reference-transaction`, `post-index-change`, `pre-auto-gc`, and each other).
    "core.hooksPath=/dev/null",
    // No file system monitor program runs when git reads the working tree.
    "core.fsmonitor=false",
    // No signing program runs for a commit object that Semilla writes.
    "commit.gpgSign=false",
    // No automatic maintenance runs after a command.
    "gc.auto=0",
    "maintenance.auto=false",
    // No pager and no editor.
    "core.pager=cat",
    "core.editor=true",
    // No transport program (Semilla uses no remote, this is a second guard).
    "core.sshCommand=false",
    // No external diff program. `--no-ext-diff --no-textconv` is on each diff command also.
    "diff.external=",
    // No program that checks a signature runs for `git log`.
    "log.showSignature=false",
    // Git reads the parents from the commit objects, not from a commit graph file.
    "core.commitGraph=false",
    // No transport runs: git gets no missing object from a remote that the configuration names
    // (a promisor remote with the `ext` transport is a program).
    "protocol.allow=never",
    "protocol.ext.allow=never",
    "protocol.file.allow=never",
    "protocol.git.allow=never",
    "protocol.ssh.allow=never",
    "protocol.http.allow=never",
    "protocol.https.allow=never",
];

/// The environment of each git command. Replace refs, the shallow file, and the graft file do
/// not change an object or hide a parent. Git gets no missing object from a remote, asks no
/// question, and takes no optional lock.
pub const SAFE_ENV: &[(&str, &str)] = &[
    ("GIT_NO_REPLACE_OBJECTS", "1"),
    ("GIT_SHALLOW_FILE", ""),
    ("GIT_GRAFT_FILE", "/dev/null"),
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_OPTIONAL_LOCKS", "0"),
];

/// The variables that can point git at other data. Semilla removes them from each git command.
const REMOVED_ENV: &[&str] = &["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_EXTERNAL_DIFF", "GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT", "GIT_NAMESPACE"];

/// A git command with the safe configuration and the safe environment.
fn git_command() -> Command {
    let mut command = Command::new("git");
    command.arg("--no-pager");
    for setting in SAFE_CONFIG {
        command.arg("-c").arg(setting);
    }
    for name in REMOVED_ENV {
        command.env_remove(name);
    }
    for (name, value) in SAFE_ENV {
        command.env(name, value);
    }
    command
}

/// A git repository with a working tree.
#[derive(Clone, Debug)]
pub struct Repo {
    pub root: PathBuf,
    /// The cache folder of this repository. It is outside the repository folder (I5).
    pub store: Arc<Store>,
}

impl Repo {
    /// Finds the repository that contains `dir`.
    pub fn discover(dir: &Path) -> Result<Repo> {
        let output = git_command().arg("-C").arg(dir).args(["rev-parse", "--show-toplevel"]).output().context("run git")?;
        if !output.status.success() {
            bail!("{} is not in a git repository", dir.display());
        }
        let root = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        let store = Arc::new(Store::open(&root));
        Ok(Repo { root, store })
    }

    /// The `.semilla` folder of the repository. It holds only the session of the agent: claims,
    /// never a datum of a result (I5).
    pub fn state_dir(&self) -> PathBuf {
        self.root.join(".semilla")
    }

    /// The cache folder of this repository, outside the repository folder.
    pub fn cache_dir(&self) -> PathBuf {
        self.store.dir.clone()
    }

    /// The folder of the build checkouts. It is outside the repository: cargo reads each
    /// `.cargo/config` of the parent folders of the build, so a build inside the repository reads
    /// the cargo configuration of the working tree. Then a new `.cargo/config` in a step changes
    /// the builds of both trees, and the change is not visible (C5).
    pub fn build_base(&self) -> PathBuf {
        self.store.dir.join("build")
    }

    /// Runs git and returns its standard output.
    pub fn git(&self, args: &[&str]) -> Result<String> {
        self.git_env(args, &[])
    }

    fn git_env(&self, args: &[&str], env: &[(&str, &Path)]) -> Result<String> {
        let mut command = git_command();
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
    /// and `.semilla` are not in the tree. `git add` makes the blobs as `git commit` makes them
    /// (with the clean filters of the repository), so the tree of a step is the tree that the
    /// commit gets. This command is for the agent (`step`). Each check reads the tree as raw
    /// blobs, so a filter cannot hide a change from a result.
    pub fn write_tree(&self) -> Result<String> {
        let index = self.private_path("index");
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

    /// The tree of a commit. `COMMIT^` is the first parent from the raw commit object.
    pub fn commit_tree(&self, commit: &str) -> Result<String> {
        if let Some(child) = commit.strip_suffix('^') {
            let parent = self.parent(child)?.with_context(|| format!("commit {child} has no parent"))?;
            return self.git(&["rev-parse", "--verify", &format!("{parent}^{{tree}}")]);
        }
        self.git(&["rev-parse", "--verify", &format!("{commit}^{{tree}}")])
    }

    /// The first parent of a commit, from the raw commit object. A shallow file or a graft in the
    /// repository folder cannot hide the parent.
    pub fn parent(&self, commit: &str) -> Result<Option<String>> {
        let hash = self.git(&["rev-parse", "--verify", &format!("{commit}^{{commit}}")])?;
        let text = self.git(&["cat-file", "commit", &hash])?;
        Ok(text.lines().take_while(|line| !line.is_empty()).find_map(|line| line.strip_prefix("parent ")).map(str::to_string))
    }

    /// A path in the cache folder that only this process uses (for index files).
    fn private_path(&self, name: &str) -> PathBuf {
        self.store.private_path(name)
    }

    /// Writes a tree into a cache folder (once), and returns the folder.
    ///
    /// The files come from the raw blobs of the tree ([`Repo::write_files`]). The tree goes into
    /// a private folder first, then a rename puts it in place. The rename is atomic, so a cache
    /// folder that exists is complete, also when other processes write the same tree at the same
    /// time.
    pub fn materialize(&self, tree: &str) -> Result<PathBuf> {
        let base = self.store.dir.join("snapshots");
        let dir = base.join(tree);
        if dir.exists() {
            return Ok(dir);
        }
        std::fs::create_dir_all(&base)?;
        let temp = self.private_path("snapshot");
        std::fs::create_dir_all(&temp)?;
        let entries = self.tree_entries(tree)?;
        let all: Vec<(&String, &FileEntry)> = entries.iter().collect();
        self.write_files(&temp, &all)?;
        if std::fs::rename(&temp, &dir).is_err() {
            // Another process put the same tree in place first.
            let _ = std::fs::remove_dir_all(&temp);
        }
        Ok(dir)
    }

    /// Writes tree entries into a folder from the raw blobs (`git cat-file --batch`). The content
    /// of each file is the bytes of its blob: no smudge filter, no attribute, and no conversion of
    /// line ends applies. The mode and the kind (file, executable file, symbolic link, submodule)
    /// come from the tree entry. A submodule becomes an empty folder.
    fn write_files(&self, dir: &Path, entries: &[(&String, &FileEntry)]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut child = git_command().arg("-C").arg(&self.root).args(["cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().context("run git cat-file --batch")?;
        let mut stdin = child.stdin.take().context("no stdin of git cat-file")?;
        let hashes: Vec<String> = entries.iter().filter(|(_, entry)| entry.mode != "160000").map(|(_, entry)| entry.hash.clone()).collect();
        // A second thread writes the requests, so that a full pipe cannot stop both sides.
        let writer = std::thread::spawn(move || {
            for hash in hashes {
                if writeln!(stdin, "{hash}").is_err() {
                    break;
                }
            }
        });
        let mut stdout = std::io::BufReader::new(child.stdout.take().context("no stdout of git cat-file")?);
        let result = (|| -> Result<()> {
            for (path, entry) in entries {
                let file = dir.join(path.as_str());
                prepare_place(dir, path)?;
                if entry.mode == "160000" {
                    std::fs::create_dir_all(&file)?;
                    continue;
                }
                let mut header = String::new();
                stdout.read_line(&mut header)?;
                let mut parts = header.split_whitespace();
                let (Some(hash), Some("blob"), Some(size)) = (parts.next(), parts.next(), parts.next().and_then(|size| size.parse::<usize>().ok())) else {
                    bail!("the tree entry `{path}` is not a blob that git can read ({})", header.trim());
                };
                anyhow::ensure!(hash == entry.hash, "git gave object {hash} for the entry `{path}` ({})", entry.hash);
                let mut content = vec![0; size];
                stdout.read_exact(&mut content)?;
                let mut newline = [0; 1];
                stdout.read_exact(&mut newline)?;
                if entry.mode == "120000" {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(std::ffi::OsStr::new(std::str::from_utf8(&content).with_context(|| format!("the link `{path}` has a target that is not text"))?), &file)?;
                    #[cfg(not(unix))]
                    std::fs::write(&file, &content)?;
                    continue;
                }
                std::fs::write(&file, &content).with_context(|| format!("write {}", file.display()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = if entry.mode == "100755" { 0o755 } else { 0o644 };
                    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode))?;
                }
            }
            Ok(())
        })();
        drop(stdout);
        let _ = child.kill();
        let _ = child.wait();
        let _ = writer.join();
        result
    }

    /// Updates a fixed build folder to a tree. Semilla writes only the files that differ from the
    /// last tree of the folder, and removes each file that the new tree does not have. So the
    /// unchanged files keep their times, and cargo can reuse its work for them.
    ///
    /// A build can write its build folder (a build script, a proc macro). So Semilla does not
    /// trust the folder: a file that the tree does not have is removed, and a file whose state
    /// (size, change time, kind) is not the state that Semilla recorded is written again.
    pub fn checkout_build(&self, dir: &Path, tree: &str) -> Result<()> {
        let entries = self.tree_entries(tree)?;
        let record = dir.with_extension("files");
        let known: BTreeMap<String, String> = std::fs::read_to_string(&record)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once('\t').map(|(state, path)| (path.to_string(), state.to_string())))
            .collect();
        // The record is not valid while the folder changes.
        let _ = std::fs::remove_file(&record);
        std::fs::create_dir_all(dir)?;
        remove_unknown(dir, dir, &entries)?;
        let stale: Vec<(&String, &FileEntry)> = entries.iter().filter(|(path, entry)| known.get(*path).is_none_or(|state| file_state(&dir.join(path.as_str()), entry).as_deref() != Some(state))).collect();
        self.write_files(dir, &stale)?;
        let mut text = String::new();
        for (path, entry) in &entries {
            let state = file_state(&dir.join(path.as_str()), entry).with_context(|| format!("the build folder has no file `{path}`"))?;
            text.push_str(&format!("{state}\t{path}\n"));
        }
        std::fs::write(&record, text)?;
        Ok(())
    }

    /// Changes the files of the working tree from tree `from` to tree `to`. The working tree must
    /// be `from`. A private index keeps the user's index unchanged. Git writes only the files that
    /// differ, and removes the files that `to` does not have. Ignored files stay. This command is
    /// for the agent (the replay): it writes the working tree as `git checkout` writes it.
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

    /// The line hunks between two trees (`git diff -U0`), for all files or for one file. No
    /// external diff program and no text conversion program runs.
    pub fn hunks(&self, from: &str, to: &str, file: Option<&str>) -> Result<Vec<Hunk>> {
        let mut args = vec!["diff", "-U0", "--no-renames", "--no-color", "--no-ext-diff", "--no-textconv", from, to];
        if let Some(file) = file {
            args.extend(["--", file]);
        }
        Ok(parse_hunks(&self.git(&args)?))
    }

    /// A new tree: `base` with some files replaced (`Some`) or removed (`None`). The working tree
    /// and the user's index do not change.
    pub fn tree_with(&self, base: &str, files: &BTreeMap<String, Option<String>>) -> Result<String> {
        let index = self.private_path("index");
        let blob = self.private_path("blob");
        let env = [("GIT_INDEX_FILE", index.as_path())];
        let result = (|| -> Result<String> {
            self.git_env(&["read-tree", base], &env)?;
            for (path, content) in files {
                match content {
                    Some(text) => {
                        std::fs::write(&blob, text)?;
                        let hash = self.git(&["hash-object", "-w", "--no-filters", &blob.to_string_lossy()])?;
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

    /// All entries of a tree, by path: each file, symbolic link, and submodule, with its mode and
    /// object hash (`git ls-tree -r`).
    ///
    /// The agent can write a tree object by hand. So a path that can leave the folder of the tree
    /// (`..`, an absolute path, `.git`), a path that occurs two times, and a path below an entry
    /// that is not a folder are errors.
    pub fn tree_entries(&self, tree: &str) -> Result<BTreeMap<String, FileEntry>> {
        let text = self.git(&["ls-tree", "-r", "-l", "-z", "--full-tree", tree])?;
        let mut entries = BTreeMap::new();
        for record in text.split('\0').filter(|record| !record.is_empty()) {
            // `<mode> <type> <hash> <size>\t<path>` (the size is `-` for a submodule).
            let (meta, path) = record.split_once('\t').with_context(|| format!("bad ls-tree line: {record}"))?;
            let mut parts = meta.split_whitespace();
            let (Some(mode), Some(_), Some(hash), size) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
                bail!("bad ls-tree line: {record}");
            };
            anyhow::ensure!(is_safe_path(path), "tree {tree} has the unsafe path `{path}`; Semilla does not read this tree");
            anyhow::ensure!(matches!(mode, "100644" | "100755" | "120000" | "160000"), "tree {tree} has the mode {mode} for `{path}`; Semilla does not read this tree");
            let size = size.and_then(|s| s.parse().ok()).unwrap_or(0);
            let entry = FileEntry { mode: mode.to_string(), hash: hash.to_string(), size };
            anyhow::ensure!(entries.insert(path.to_string(), entry).is_none(), "tree {tree} has two entries for the path `{path}`; Semilla does not read this tree");
        }
        for path in entries.keys() {
            let mut parent = Path::new(path).parent();
            while let Some(folder) = parent.filter(|folder| !folder.as_os_str().is_empty()) {
                anyhow::ensure!(!entries.contains_key(&*folder.to_string_lossy()), "tree {tree} has a file and a folder with the path `{}`; Semilla does not read this tree", folder.display());
                parent = folder.parent();
            }
        }
        Ok(entries)
    }

    /// The files that differ between two trees.
    pub fn changed_files(&self, from: &str, to: &str) -> Result<Vec<String>> {
        Ok(self.git(&["diff-tree", "-r", "--name-only", "--no-renames", from, to])?.lines().map(str::to_string).collect())
    }

    /// Stores a note on a commit, under `refs/notes/semilla`.
    pub fn write_note(&self, commit: &str, text: &str) -> Result<()> {
        let file = self.private_path("note");
        std::fs::write(&file, text)?;
        let result = self.git(&["notes", "--ref=semilla", "add", "-f", "-F", &file.to_string_lossy(), commit]);
        let _ = std::fs::remove_file(&file);
        result.map(|_| ())
    }

    /// Reads the note of a commit.
    pub fn read_note(&self, commit: &str) -> Result<String> {
        self.git(&["notes", "--ref=semilla", "show", commit])
    }

    /// Keeps a tree reachable with a ref, so that `git gc` does not remove it.
    pub fn keep_tree(&self, reference: &str, tree: &str, message: &str) -> Result<()> {
        let commit = self.git(&["commit-tree", "--no-gpg-sign", tree, "-m", message])?;
        self.git(&["update-ref", reference, &commit])?;
        Ok(())
    }
}

/// True if a tree path stays in the folder of the tree: it is relative, and no part is empty,
/// `.`, `..`, or `.git`.
fn is_safe_path(path: &str) -> bool {
    !path.is_empty() && !path.contains('\\') && path.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !part.eq_ignore_ascii_case(".git"))
}

/// Makes the place of a tree entry ready: each parent is a real folder in `dir` (not a link),
/// and nothing is at the path of the entry.
fn prepare_place(dir: &Path, path: &str) -> Result<()> {
    anyhow::ensure!(is_safe_path(path), "the unsafe path `{path}`");
    let mut current = dir.to_path_buf();
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                std::fs::remove_file(&current)?;
                std::fs::create_dir(&current)?;
            }
            Err(_) => std::fs::create_dir(&current)?,
        }
    }
    current.push(parts[parts.len() - 1]);
    match std::fs::symlink_metadata(&current) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&current)?,
        Ok(_) => std::fs::remove_file(&current)?,
        Err(_) => {}
    }
    Ok(())
}

/// The state of a file of a build folder: the object hash and the mode of its tree entry, and
/// the kind, the size, and the times of the file. A program without root rights cannot set the
/// change time of a file, so a build that edits a file changes this state.
fn file_state(file: &Path, entry: &FileEntry) -> Option<String> {
    let meta = std::fs::symlink_metadata(file).ok()?;
    let kind = if meta.file_type().is_symlink() { "link" } else if meta.is_dir() { "folder" } else { "file" };
    let expected = match entry.mode.as_str() {
        "120000" => "link",
        "160000" => "folder",
        _ => "file",
    };
    if kind != expected {
        return None;
    }
    #[cfg(unix)]
    let times = {
        use std::os::unix::fs::MetadataExt;
        format!("{}.{}-{}.{}-{:o}", meta.mtime(), meta.mtime_nsec(), meta.ctime(), meta.ctime_nsec(), meta.mode())
    };
    #[cfg(not(unix))]
    let times = format!("{:?}", meta.modified().ok());
    let size = if kind == "folder" { 0 } else { meta.len() };
    Some(format!("{} {} {kind} {size} {times}", entry.hash, entry.mode))
}

/// Removes each file and each folder of a build folder that the tree does not have.
fn remove_unknown(root: &Path, dir: &Path, entries: &BTreeMap<String, FileEntry>) -> Result<()> {
    for item in std::fs::read_dir(dir)? {
        let item = item?;
        let path = item.path();
        let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
        let is_folder = item.file_type()?.is_dir();
        if let Some(entry) = entries.get(&relative) {
            // A submodule folder must be empty. A file entry must not be a folder.
            if is_folder {
                std::fs::remove_dir_all(&path)?;
                if entry.mode == "160000" {
                    std::fs::create_dir(&path)?;
                }
            }
            continue;
        }
        let prefix = format!("{relative}/");
        let has_files = is_folder && entries.range(prefix.clone()..).next().is_some_and(|(name, _)| name.starts_with(&prefix));
        if has_files {
            remove_unknown(root, &path, entries)?;
        } else if is_folder {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
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
    fn a_tree_path_that_can_leave_the_folder_is_not_safe() {
        // Brief 5h, A3: Semilla writes the files of a tree. A path of a forged tree object must
        // not put a file outside the folder or into a `.git` folder.
        for path in ["src/lib.rs", "a/b/c", "LICENSE", ".cargo/config.toml", "a..b/c"] {
            assert!(is_safe_path(path), "{path}");
        }
        for path in ["", "/etc/passwd", "../x", "a/../../x", "a//b", "./a", "a/.", ".git/config", "a/.GIT/hooks/x", "a\\..\\b"] {
            assert!(!is_safe_path(path), "{path}");
        }
    }

    #[test]
    fn a_link_in_the_build_folder_does_not_move_a_file_out_of_the_folder() {
        // A build script of an earlier build can leave a link in the build folder. The next
        // write of a tree entry replaces the link with a real folder.
        let dir = tempfile::tempdir().unwrap();
        let (build, outside) = (dir.path().join("build"), dir.path().join("outside"));
        std::fs::create_dir_all(&build).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, build.join("src")).unwrap();
            prepare_place(&build, "src/lib.rs").unwrap();
            std::fs::write(build.join("src/lib.rs"), "x").unwrap();
            assert!(std::fs::symlink_metadata(build.join("src")).unwrap().is_dir());
            assert!(!outside.join("lib.rs").exists());
        }
        assert!(prepare_place(&build, "../outside/x").is_err());
    }

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
