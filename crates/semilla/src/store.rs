//! The cache folder of the user (invariant I5, design 27).
//!
//! The agent controls each file of the repository folder. So Semilla keeps each datum that a
//! result uses outside the repository folder: the snapshots, the MIR records, the build folders,
//! the stored results, and the review state. The folder is `$SEMILLA_CACHE_DIR`, else
//! `$XDG_CACHE_HOME/semilla`, else `~/.cache/semilla`. Each repository has one folder there. Its
//! name is a hash of the absolute path of the repository.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The cache folder of one repository.
#[derive(Debug)]
pub struct Store {
    /// The folder of this repository.
    pub dir: PathBuf,
    /// True if the folder is temporary: `--fresh`, or no cache folder that Semilla can write.
    temporary: bool,
}

static FRESH: AtomicBool = AtomicBool::new(false);

/// Sets the `--fresh` mode of this process: no command reads a cache.
pub fn set_fresh(fresh: bool) {
    FRESH.store(fresh, Ordering::Relaxed);
}

/// The cache folder of the user for all repositories. `None` if no variable gives a folder.
pub fn user_cache() -> Option<PathBuf> {
    let absolute = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute());
    absolute("SEMILLA_CACHE_DIR").or_else(|| absolute("XDG_CACHE_HOME").map(|dir| dir.join("semilla"))).or_else(|| absolute("HOME").map(|dir| dir.join(".cache/semilla")))
}

/// A stable hash of a text (FNV-1a, 64 bits). The standard hasher can change with the compiler.
pub fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// True if `path` is `folder` or is in `folder`. The two paths are canonical when they exist.
pub fn is_inside(path: &Path, folder: &Path) -> bool {
    let canonical = |path: &Path| {
        // A path that does not exist yet: use the first parent that exists.
        let mut rest = Vec::new();
        let mut current = path.to_path_buf();
        loop {
            if let Ok(real) = std::fs::canonicalize(&current) {
                return rest.iter().rev().fold(real, |path, name| path.join(name));
            }
            match (current.file_name().map(|name| name.to_os_string()), current.parent().map(Path::to_path_buf)) {
                (Some(name), Some(parent)) => {
                    rest.push(name);
                    current = parent;
                }
                _ => return path.to_path_buf(),
            }
        }
    };
    canonical(path).starts_with(canonical(folder))
}

impl Store {
    /// Opens the cache folder of a repository. If Semilla cannot use the folder, the work goes on
    /// with a temporary folder (no cache), and one note on stderr says so.
    pub fn open(root: &Path) -> Store {
        // `--fresh`: a new private folder, so this process reads no record of an earlier run.
        if FRESH.load(Ordering::Relaxed) {
            return Store::temporary();
        }
        let real = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let name = format!("{:016x}", stable_hash(real.to_string_lossy().as_bytes()));
        let problem = match user_cache() {
            None => "no variable gives a cache folder (SEMILLA_CACHE_DIR, XDG_CACHE_HOME, HOME)".to_string(),
            Some(base) if is_inside(&base, &real) => format!("the cache folder {} is in the repository folder", base.display()),
            Some(base) => {
                let dir = base.join(name);
                let marker = dir.join("repository");
                let usable = std::fs::create_dir_all(dir.join("tmp")).and_then(|_| std::fs::write(&marker, format!("{}\n", real.display())));
                match usable {
                    Ok(()) => return Store { dir, temporary: false },
                    Err(error) => format!("Semilla cannot write the cache folder {} ({error})", dir.display()),
                }
            }
        };
        static NOTED: AtomicBool = AtomicBool::new(false);
        if !NOTED.swap(true, Ordering::Relaxed) {
            eprintln!("note: {problem}; Semilla works with no cache and computes each record again");
        }
        Store::temporary()
    }

    /// A new private folder in the temporary folder of the system. `Drop` removes it.
    fn temporary() -> Store {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|time| time.subsec_nanos()).unwrap_or(0);
        let mut count = 0;
        loop {
            let dir = std::env::temp_dir().join(format!("semilla-nocache-{}-{nanos:09}-{count}", std::process::id()));
            // `create_dir` fails if the folder exists, so no other program made this folder.
            if std::fs::create_dir(&dir).is_ok() || count > 100 {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                }
                let _ = std::fs::create_dir_all(dir.join("tmp"));
                return Store { dir, temporary: true };
            }
            count += 1;
        }
    }

    /// True if this process has no cache that stays (`--fresh`, or no folder that it can write).
    pub fn is_temporary(&self) -> bool {
        self.temporary
    }

    /// The folder of the records that have a key from content only, for all the folders of
    /// this user (batch 6a, B3). The cache folder of a repository has the path of the repository
    /// in its name, so a copy of the repository at a different path does not find its records.
    /// `None` if this process has no cache that stays.
    pub fn content_dir(&self, name: &str) -> Option<PathBuf> {
        if self.temporary {
            return None;
        }
        Some(self.dir.parent()?.join("by-content").join(name))
    }

    /// A path in the cache folder that only this process uses (an index file, a note text).
    pub fn private_path(&self, name: &str) -> PathBuf {
        use std::sync::atomic::AtomicUsize;
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let _ = std::fs::create_dir_all(self.dir.join("tmp"));
        self.dir.join("tmp").join(format!("{name}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// The folders where Semilla looks for a tool (the solver), in order, after the environment
/// variable of the tool: next to the Semilla binary, then the cache folder of the user. A folder
/// in the checked repository is never in the list (I5).
pub fn tool_dirs(repo_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| std::fs::canonicalize(exe).ok()).and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        dirs.push(dir.join("tools"));
        dirs.push(dir);
    }
    if let Some(base) = user_cache() {
        dirs.push(base.join("tools"));
    }
    dirs.retain(|dir| !is_inside(dir, repo_root));
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_in_the_repository_is_inside_also_when_it_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(is_inside(&repo.join(".semilla/tools/z3"), &repo));
        assert!(is_inside(&repo, &repo));
        assert!(!is_inside(&dir.path().join("cache"), &repo));
        // A link out of the repository is not inside. A link into the repository is inside.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&repo, dir.path().join("link")).unwrap();
            assert!(is_inside(&dir.path().join("link/x"), &repo));
        }
    }

    #[test]
    fn the_hash_of_a_path_is_stable() {
        assert_eq!(stable_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(stable_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
