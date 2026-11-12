//! The sandbox of a check build (design 27, invariant I5).
//!
//! A check build runs code of the change set: a build script, a proc macro. This code must not
//! change a file outside the build folder. The sandbox uses Landlock, a security module of the
//! Linux kernel. A process with the sandbox, and each process that it starts, can read each file
//! as before. It can write only the paths of the allow list: the cargo target folder of the build
//! and the folder of the MIR records. The build needs no write in the cargo home folder: cargo
//! works with a read-only cargo home when each package is there (no download in the sandbox).
//!
//! The sandbox has no effect on Semilla: it starts in the child process, before `cargo` starts.
//! If the kernel has no Landlock, the build runs with no sandbox, and each result says so.

use std::path::PathBuf;
use std::process::Command;

/// Why this machine has no sandbox, or `None` if the last build ran in the sandbox.
static PROBLEM: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The line that each result shows when a build of this process ran with no sandbox.
pub const NO_SANDBOX: &str = "the build ran with no sandbox: a build script or a proc macro of this change set can change the result";

/// Why a build of this process ran with no sandbox, or `None`.
pub fn problem() -> Option<String> {
    PROBLEM.lock().ok().and_then(|problem| problem.clone())
}

/// Why this machine cannot make the sandbox, or `None`. The check makes no build.
pub fn check() -> Option<String> {
    imp::abi().err()
}

/// The paths that a check build can write.
#[derive(Debug, Default)]
pub struct Sandbox {
    /// The folders that the build can change in full.
    pub folders: Vec<PathBuf>,
    /// The files that the build can write and truncate. It cannot remove them.
    pub files: Vec<PathBuf>,
}

impl Sandbox {
    /// Puts the sandbox on a command. On an error (no Landlock), the command runs with no
    /// sandbox, and [`problem`] gives the cause.
    pub fn apply(&self, command: &mut Command) {
        let result = imp::apply(self, command);
        if let Ok(mut problem) = PROBLEM.lock() {
            *problem = result.err();
        }
    }

    /// Puts the sandbox on a command that is not a check build (`cargo metadata`). It does not
    /// change the state of [`problem`]. Returns the cause if the command runs with no sandbox.
    pub fn apply_to_tool(&self, command: &mut Command) -> Option<String> {
        if std::env::var_os("SEMILLA_NO_SANDBOX").is_some_and(|value| !value.is_empty()) {
            return Some("the variable SEMILLA_NO_SANDBOX is set".into());
        }
        imp::apply(self, command).err()
    }
}

/// Records that a build runs with no sandbox, with the cause.
pub fn skip(cause: &str) {
    if let Ok(mut problem) = PROBLEM.lock() {
        *problem = Some(cause.to_string());
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Sandbox;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::Command;

    // The access rights of Landlock for files (linux/landlock.h).
    const WRITE_FILE: u64 = 1 << 1;
    const REMOVE_DIR: u64 = 1 << 4;
    const REMOVE_FILE: u64 = 1 << 5;
    const MAKE_CHAR: u64 = 1 << 6;
    const MAKE_DIR: u64 = 1 << 7;
    const MAKE_REG: u64 = 1 << 8;
    const MAKE_SOCK: u64 = 1 << 9;
    const MAKE_FIFO: u64 = 1 << 10;
    const MAKE_BLOCK: u64 = 1 << 11;
    const MAKE_SYM: u64 = 1 << 12;
    const REFER: u64 = 1 << 13;
    const TRUNCATE: u64 = 1 << 14;
    /// Each right that changes a file or a folder. The sandbox does not limit a read.
    const ALL_WRITES: u64 = WRITE_FILE | REMOVE_DIR | REMOVE_FILE | MAKE_CHAR | MAKE_DIR | MAKE_REG | MAKE_SOCK | MAKE_FIFO | MAKE_BLOCK | MAKE_SYM | REFER | TRUNCATE;
    /// The rights that apply to a file that is not a folder.
    const FILE_WRITES: u64 = WRITE_FILE | TRUNCATE;
    // The access rights of Landlock for the network (version 4).
    const NET_BIND_TCP: u64 = 1 << 0;
    const NET_CONNECT_TCP: u64 = 1 << 1;
    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: libc::c_int = 1;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
        handled_access_net: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    /// The Landlock version of the kernel. The sandbox needs version 3: that version can stop a
    /// truncate of a file.
    pub fn abi() -> Result<i64, String> {
        // SAFETY: the call reads no memory (a null pointer with the size 0 asks for the version).
        let version = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, std::ptr::null::<RulesetAttr>(), 0usize, CREATE_RULESET_VERSION) };
        if version < 0 {
            return Err(format!("the kernel has no Landlock ({})", std::io::Error::last_os_error()));
        }
        if version < 3 {
            return Err(format!("the kernel has Landlock version {version}, and the sandbox needs version 3 (Linux 6.2)"));
        }
        Ok(version)
    }

    fn add_rule(ruleset: &OwnedFd, path: &Path, access: u64) -> Result<(), String> {
        let text = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| format!("the path {} has a NUL byte", path.display()))?;
        // SAFETY: `text` is a valid C string. The call opens the path and reads no file data.
        let fd = unsafe { libc::open(text.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("the sandbox cannot open {} ({})", path.display(), std::io::Error::last_os_error()));
        }
        // SAFETY: `fd` is a new descriptor that this function owns.
        let parent = unsafe { OwnedFd::from_raw_fd(fd) };
        let rule = PathBeneathAttr { allowed_access: access, parent_fd: parent.as_raw_fd() };
        // SAFETY: `rule` lives until the call returns, and the kernel reads only its bytes.
        let result = unsafe { libc::syscall(libc::SYS_landlock_add_rule, ruleset.as_raw_fd(), RULE_PATH_BENEATH, &rule as *const PathBeneathAttr, 0u32) };
        if result < 0 {
            return Err(format!("the sandbox cannot add the rule for {} ({})", path.display(), std::io::Error::last_os_error()));
        }
        Ok(())
    }

    pub fn apply(sandbox: &Sandbox, command: &mut Command) -> Result<(), String> {
        // Landlock version 4 (Linux 6.7) can stop TCP: the rule set handles `bind` and `connect`,
        // and it has no rule for a port, so each TCP `bind` and each TCP `connect` fails.
        let net = if abi()? >= 4 { NET_BIND_TCP | NET_CONNECT_TCP } else { 0 };
        let attr = RulesetAttr { handled_access_fs: ALL_WRITES, handled_access_net: net };
        // SAFETY: `attr` lives until the call returns, and the size is the size of the struct.
        let fd = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, &attr as *const RulesetAttr, std::mem::size_of::<RulesetAttr>(), 0u32) };
        if fd < 0 {
            return Err(format!("the kernel cannot make a Landlock rule set ({})", std::io::Error::last_os_error()));
        }
        // SAFETY: `fd` is a new descriptor that this function owns.
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        for folder in &sandbox.folders {
            std::fs::create_dir_all(folder).map_err(|error| format!("the sandbox cannot make {} ({error})", folder.display()))?;
            add_rule(&ruleset, folder, ALL_WRITES)?;
        }
        // A program writes its unused output to `/dev/null`.
        for file in sandbox.files.iter().map(|path| path.as_path()).chain([Path::new("/dev/null")]) {
            if file.is_file() || file == Path::new("/dev/null") {
                add_rule(&ruleset, file, FILE_WRITES)?;
            }
        }
        // SAFETY: the function runs in the child process between `fork` and `exec`. It makes only
        // two system calls, and it uses no lock and no memory allocation.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod group {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    pub fn start(command: &mut Command) {
        // The build gets its own process group, so `stop` can stop the whole group with one
        // signal. `PR_SET_PDEATHSIG` stops the build if the Semilla thread stops first.
        command.process_group(0);
        // SAFETY: the function runs in the child process between `fork` and `exec`, and it makes
        // one system call.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                Ok(())
            });
        }
    }

    pub fn stop(leader: u32) -> usize {
        if leader == 0 {
            return 0;
        }
        // SAFETY: `kill` with a negative number sends the signal to each process of the group.
        // It stops `cargo`, each `rustc`, each build script, and each child that stays in the
        // group. Then it reaps the leader. A process that leaves the group with `setsid` or a
        // double fork survives: the separate folders and the record marks (A2) stop it from
        // poison of a later build, so this kill is a second barrier, not the only one. Only the
        // leader's group is stopped, so a parallel build of another thread is safe.
        unsafe {
            libc::kill(-(leader as i32), libc::SIGKILL);
            let mut status = 0;
            libc::waitpid(leader as i32, &mut status, libc::WNOHANG);
        }
        0
    }
}

#[cfg(not(target_os = "linux"))]
mod group {
    pub fn start(_command: &mut std::process::Command) {}

    pub fn stop(_leader: u32) -> usize {
        0
    }
}

/// Runs a build command to its end, and then stops each process that the build left.
///
/// The command gets its own process group, and Semilla stops the group at the end. A process
/// that left the group loses its parent when the build ends: it becomes a child of Semilla (the
/// "child subreaper" flag of Linux), and Semilla stops it too. The second part works only when
/// no other build of this process runs, and only on Linux.
pub fn run_and_stop(command: &mut Command) -> std::io::Result<std::process::Output> {
    group::start(command);
    let child = command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn();
    let child = match child {
        Ok(child) => child,
        Err(error) => {
            group::stop(0);
            return Err(error);
        }
    };
    let leader = child.id();
    let output = child.wait_with_output();
    group::stop(leader);
    output
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::Sandbox;
    use std::process::Command;

    pub fn abi() -> Result<i64, String> {
        Err("Semilla has a sandbox only on Linux (Landlock)".into())
    }

    pub fn apply(_sandbox: &Sandbox, _command: &mut Command) -> Result<(), String> {
        abi().map(|_| ())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A command in the sandbox writes a folder of the allow list, and no other path.
    #[test]
    fn a_command_in_the_sandbox_writes_only_the_allow_list() {
        if let Some(cause) = check() {
            eprintln!("no sandbox on this machine: {cause}");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (open, closed) = (dir.path().join("open"), dir.path().join("closed"));
        std::fs::create_dir_all(&open).unwrap();
        std::fs::create_dir_all(&closed).unwrap();
        std::fs::write(closed.join("old.txt"), "old").unwrap();
        std::fs::write(closed.join("one.txt"), "old").unwrap();
        let script = "echo new > open/a.txt; echo new > closed/a.txt; echo new > closed/old.txt; rm closed/old.txt; mv open/a.txt closed/b.txt; echo new > closed/one.txt; true";
        let mut command = Command::new("sh");
        command.arg("-c").arg(script).current_dir(dir.path());
        Sandbox { folders: vec![open.clone()], files: vec![closed.join("one.txt")] }.apply(&mut command);
        assert_eq!(problem(), None);
        assert!(command.output().unwrap().status.success());
        assert_eq!(std::fs::read_to_string(open.join("a.txt")).unwrap(), "new\n", "the build writes its folder");
        assert!(!closed.join("a.txt").exists(), "no new file outside the allow list");
        assert!(!closed.join("b.txt").exists(), "no move to a folder outside the allow list");
        assert_eq!(std::fs::read_to_string(closed.join("old.txt")).unwrap(), "old", "no change and no removal of a file outside the allow list");
        assert_eq!(std::fs::read_to_string(closed.join("one.txt")).unwrap(), "new\n", "a file of the allow list is writable");
        // The sandbox is in the child only: this process writes as before.
        std::fs::write(closed.join("parent.txt"), "x").unwrap();
    }
}
