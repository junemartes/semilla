//! The sandbox of a check build (design 27, invariant I5).
//!
//! A check build runs code of the change set: a build script, a proc macro. The sandbox has
//! three limits for this code:
//!
//! - **The write limit.** The build writes only the paths of the write list: the cargo target
//!   folder of the build and the folder of the MIR records.
//! - **The read limit.** The build reads only the paths of the read list: the write list, the
//!   build folder, the toolchain folder, the registry and the git folders of the cargo home, the
//!   system folders of the compiler and the linker, and the MIR driver. It does not read the
//!   repository folder, the home folder, `/proc`, or `/sys`.
//! - **The network limit.** The build makes no socket: no TCP, no UDP, no raw socket, no
//!   netlink, no UNIX socket. It gets no open file of Semilla, so it gets no socket of Semilla.
//!
//! The write limit and the read limit use Landlock, a security module of the Linux kernel. The
//! network limit uses a seccomp filter. The two need no special right. Each process that the
//! build starts has the same limits, and a process cannot remove them.
//!
//! The sandbox has no effect on Semilla: it starts in the child process, before `cargo` starts.
//! The state of the sandbox is a property of the machine and of `SEMILLA_NO_SANDBOX`. A limit
//! that the machine cannot give is missing for each build of the process, and each result names
//! each missing limit. A sandbox that the machine can give, and that fails to start, stops the
//! build: a build never runs with fewer limits than [`missing`] says.

use std::path::PathBuf;
use std::process::Command;

/// One protection of the sandbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Limit {
    Write,
    Read,
    Network,
}

impl Limit {
    /// The name of the limit in an output.
    pub fn name(self) -> &'static str {
        match self {
            Limit::Write => "the write limit",
            Limit::Read => "the read limit",
            Limit::Network => "the network limit",
        }
    }
}

/// The limits that each build of this process does not have, with the cause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    /// The missing limits, in the order write, read, network. The list is not empty.
    pub limits: Vec<Limit>,
    /// Why the limits are missing.
    pub cause: String,
}

impl Missing {
    /// The names of the missing limits: "the write limit, the read limit, and the network limit".
    pub fn names(&self) -> String {
        let names: Vec<&str> = self.limits.iter().map(|limit| limit.name()).collect();
        match names.as_slice() {
            [] => String::new(),
            [one] => one.to_string(),
            [first, second] => format!("{first} and {second}"),
            [first @ .., last] => format!("{}, and {last}", first.join(", ")),
        }
    }

    /// The line that each result shows when a build ran with a missing limit. The line names
    /// each missing limit.
    pub fn line(&self) -> String {
        let (kind, verb) = if self.limits.len() == 3 { ("no", "are") } else { ("a partial", if self.limits.len() == 1 { "is" } else { "are" }) };
        format!("the build ran with {kind} sandbox: {} {verb} missing ({}). A build script or a proc macro of this change set can change the result", self.names(), self.cause)
    }

    /// The state in one clause: "the network limit is missing (the cause)" (batch 5w, D5).
    pub fn state(&self) -> String {
        format!("{} {} missing ({})", self.names(), if self.limits.len() == 1 { "is" } else { "are" }, self.cause)
    }

    /// The short text of the state for a cache key.
    pub fn key(&self) -> String {
        self.limits.iter().map(|limit| limit.name()).collect::<Vec<_>>().join("+")
    }
}

/// The start of [`Missing::line`] for a build with no limit.
pub const NO_SANDBOX: &str = "the build ran with no sandbox";

/// True if `SEMILLA_NO_SANDBOX` is set to a text that is not empty.
fn variable() -> bool {
    std::env::var_os("SEMILLA_NO_SANDBOX").is_some_and(|value| !value.is_empty())
}

/// The limits that each build of this process does not have, or `None` for the full sandbox.
/// The answer is the same for the whole time of the process.
pub fn missing() -> Option<Missing> {
    static STATE: std::sync::OnceLock<Option<Missing>> = std::sync::OnceLock::new();
    STATE
        .get_or_init(|| {
            if variable() {
                return Some(Missing { limits: vec![Limit::Write, Limit::Read, Limit::Network], cause: "the variable SEMILLA_NO_SANDBOX is set".into() });
            }
            machine()
        })
        .clone()
}

/// The limits that this machine cannot give, or `None`. `SEMILLA_NO_SANDBOX` has no effect.
fn machine() -> Option<Missing> {
    let mut limits = Vec::new();
    let mut causes = Vec::new();
    if let Err(cause) = imp::abi() {
        limits.extend([Limit::Write, Limit::Read]);
        causes.push(cause);
    }
    if let Err(cause) = imp::filter_support() {
        limits.push(Limit::Network);
        causes.push(cause);
    }
    causes.dedup();
    (!limits.is_empty()).then(|| Missing { limits, cause: causes.join("; ") })
}

/// Why this machine cannot make the full sandbox, or `None`. The check makes no build.
pub fn check() -> Option<String> {
    machine().map(|missing| format!("{} missing: {}", missing.names(), missing.cause))
}

/// The paths that a check build can use.
#[derive(Debug, Default)]
pub struct Sandbox {
    /// The folders that the build can change in full. It can also read them.
    pub folders: Vec<PathBuf>,
    /// The files that the build can read, write, and truncate. It cannot remove them.
    pub files: Vec<PathBuf>,
    /// The folders and the files that the build can read, and the programs that it can start.
    /// A path that does not exist has no rule. The system folders of [`SYSTEM_READ`] are
    /// always on the list.
    pub read: Vec<PathBuf>,
}

/// The system paths that each build can read: the programs and the libraries of the compiler
/// and the linker, the cache of the dynamic loader, and three devices with no data of the user.
pub const SYSTEM_READ: [&str; 9] = ["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc/ld.so.cache", "/dev/urandom", "/dev/random", "/dev/zero"];

impl Sandbox {
    /// Puts the sandbox on a command: each limit that [`missing`] does not name. An error means
    /// that a limit cannot start. The caller must not run the command then.
    pub fn apply(&self, command: &mut Command) -> Result<(), String> {
        let missing = missing().map(|missing| missing.limits).unwrap_or_default();
        imp::apply(self, command, !missing.contains(&Limit::Write), !missing.contains(&Limit::Network))
    }
}

/// The fixed search path of a build for programs: the programs of the toolchain, then the
/// system folders. No folder of the user and no folder of the repository is in it.
pub fn build_path(toolchain_bin: &std::path::Path) -> std::ffi::OsString {
    let mut path = toolchain_bin.as_os_str().to_os_string();
    path.push(":/usr/local/bin:/usr/bin:/bin");
    path
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
    const EXECUTE: u64 = 1 << 0;
    const WRITE_FILE: u64 = 1 << 1;
    const READ_FILE: u64 = 1 << 2;
    const READ_DIR: u64 = 1 << 3;
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
    /// Each right that changes a file or a folder.
    const ALL_WRITES: u64 = WRITE_FILE | REMOVE_DIR | REMOVE_FILE | MAKE_CHAR | MAKE_DIR | MAKE_REG | MAKE_SOCK | MAKE_FIFO | MAKE_BLOCK | MAKE_SYM | REFER | TRUNCATE;
    /// Each right that reads a file or a folder, or starts a program.
    const ALL_READS: u64 = EXECUTE | READ_FILE | READ_DIR;
    /// The write rights that apply to a file that is not a folder.
    const FILE_WRITES: u64 = WRITE_FILE | TRUNCATE;
    /// The read rights that apply to a file that is not a folder.
    const FILE_READS: u64 = EXECUTE | READ_FILE;
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

    /// The Landlock rule set of a sandbox: the write limit and the read limit.
    fn ruleset(sandbox: &Sandbox) -> Result<OwnedFd, String> {
        // Landlock version 4 (Linux 6.7) can stop TCP: the rule set handles `bind` and `connect`,
        // and it has no rule for a port, so each TCP `bind` and each TCP `connect` fails. This is
        // a second barrier. The seccomp filter is the network limit.
        let net = if abi()? >= 4 { NET_BIND_TCP | NET_CONNECT_TCP } else { 0 };
        let attr = RulesetAttr { handled_access_fs: ALL_WRITES | ALL_READS, handled_access_net: net };
        // SAFETY: `attr` lives until the call returns, and the size is the size of the struct.
        let fd = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, &attr as *const RulesetAttr, std::mem::size_of::<RulesetAttr>(), 0u32) };
        if fd < 0 {
            return Err(format!("the kernel cannot make a Landlock rule set ({})", std::io::Error::last_os_error()));
        }
        // SAFETY: `fd` is a new descriptor that this function owns.
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        for folder in &sandbox.folders {
            std::fs::create_dir_all(folder).map_err(|error| format!("the sandbox cannot make {} ({error})", folder.display()))?;
            add_rule(&ruleset, folder, ALL_WRITES | ALL_READS)?;
        }
        for file in sandbox.files.iter().filter(|file| file.is_file()) {
            add_rule(&ruleset, file, FILE_WRITES | FILE_READS)?;
        }
        // A program writes its unused output to `/dev/null`.
        add_rule(&ruleset, Path::new("/dev/null"), FILE_WRITES | READ_FILE)?;
        let system = super::SYSTEM_READ.iter().map(|path| Path::new(*path));
        for path in sandbox.read.iter().map(|path| path.as_path()).chain(system) {
            // The rule is for the real file. A path that does not exist needs no rule.
            match std::fs::metadata(path) {
                Ok(meta) if meta.is_dir() => add_rule(&ruleset, path, ALL_READS)?,
                Ok(_) => add_rule(&ruleset, path, FILE_READS)?,
                Err(_) => {}
            }
        }
        Ok(ruleset)
    }

    // The seccomp filter (linux/seccomp.h, linux/filter.h, linux/audit.h).
    const SECCOMP_MODE_FILTER: libc::c_ulong = 2;
    const RET_ALLOW: u32 = 0x7fff_0000;
    const RET_ERRNO: u32 = 0x0005_0000;
    const LOAD_WORD: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const JUMP_EQUAL: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    #[cfg(target_arch = "x86_64")]
    const JUMP_GREATER_EQUAL: u16 = 0x35; // BPF_JMP | BPF_JGE | BPF_K
    const RETURN: u16 = 0x06; // BPF_RET | BPF_K
    #[cfg(target_arch = "x86_64")]
    const ARCH: Option<u32> = Some(0xC000_003E); // AUDIT_ARCH_X86_64
    #[cfg(target_arch = "aarch64")]
    const ARCH: Option<u32> = Some(0xC000_00B7); // AUDIT_ARCH_AARCH64
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const ARCH: Option<u32> = None;
    const CLOSE_RANGE_CLOEXEC: libc::c_uint = 4;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Instruction {
        code: u16,
        jt: u8,
        jf: u8,
        k: u32,
    }

    #[repr(C)]
    struct Program {
        len: libc::c_ushort,
        filter: *const Instruction,
    }

    /// The system calls that the filter stops. The first group makes or uses a socket.
    /// `socketpair` is not in the list: see [`filter`]. The
    /// second group is `io_uring`: it can do the calls of the first group with no system call
    /// that a filter sees. The third group reaches a process of the user through an object of
    /// the kernel that has no file path. The last group moves a process out of the process
    /// group of the build (see [`super::run_and_stop`]).
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    const STOPPED: [libc::c_long; 20] = [
        libc::SYS_socket,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_shmget,
        libc::SYS_msgget,
        libc::SYS_semget,
        libc::SYS_mq_open,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_keyctl,
        libc::SYS_setsid,
        libc::SYS_setpgid,
        libc::SYS_ptrace,
        libc::SYS_process_vm_writev,
    ];
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const STOPPED: [libc::c_long; 0] = [];

    /// Why this machine has no seccomp filter for the network limit, or `Ok`.
    pub fn filter_support() -> Result<(), String> {
        if ARCH.is_none() {
            return Err("the network limit supports only x86_64 and aarch64".into());
        }
        // SAFETY: the call has a null pointer for the filter. A kernel with seccomp filters
        // reads the pointer and gives EFAULT. It installs no filter.
        let result = unsafe { libc::prctl(libc::PR_SET_SECCOMP, SECCOMP_MODE_FILTER, std::ptr::null::<Program>(), 0, 0) };
        let error = std::io::Error::last_os_error();
        if result == -1 && error.raw_os_error() == Some(libc::EFAULT) {
            return Ok(());
        }
        Err(format!("the kernel has no seccomp filter ({error})"))
    }

    /// The filter program: a system call of the list fails with `EPERM`, and each system call of
    /// a different architecture fails with `EPERM` (the 32-bit entry and the x32 numbers on
    /// x86_64, the 32-bit entry on aarch64). Each other system call continues.
    fn filter() -> Vec<Instruction> {
        let deny = Instruction { code: RETURN, jt: 0, jf: 0, k: RET_ERRNO | libc::EPERM as u32 };
        let mut program = vec![
            // The architecture of the system call (offset 4 of `seccomp_data`).
            Instruction { code: LOAD_WORD, jt: 0, jf: 0, k: 4 },
            Instruction { code: JUMP_EQUAL, jt: 1, jf: 0, k: ARCH.unwrap_or(0) },
            deny,
            // The number of the system call (offset 0).
            Instruction { code: LOAD_WORD, jt: 0, jf: 0, k: 0 },
        ];
        // The x32 numbers have the same architecture value as x86_64, and the bit 30.
        #[cfg(target_arch = "x86_64")]
        program.extend([Instruction { code: JUMP_GREATER_EQUAL, jt: 0, jf: 1, k: 0x4000_0000 }, deny]);
        for number in STOPPED {
            program.extend([Instruction { code: JUMP_EQUAL, jt: 0, jf: 1, k: number as u32 }, deny]);
        }
        // `socketpair` continues only for the family `AF_UNIX` (the first argument, offset 16).
        // The standard library of Rust uses such a pair to start a process, and cargo needs
        // that. The pair has no name: it connects only processes of the build.
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        program.extend([
            Instruction { code: JUMP_EQUAL, jt: 0, jf: 3, k: libc::SYS_socketpair as u32 },
            Instruction { code: LOAD_WORD, jt: 0, jf: 0, k: 16 },
            Instruction { code: JUMP_EQUAL, jt: 1, jf: 0, k: libc::AF_UNIX as u32 },
            deny,
        ]);
        program.push(Instruction { code: RETURN, jt: 0, jf: 0, k: RET_ALLOW });
        program
    }

    pub fn apply(sandbox: &Sandbox, command: &mut Command, files: bool, network: bool) -> Result<(), String> {
        let ruleset = if files { Some(ruleset(sandbox)?) } else { None };
        let filter = if network {
            filter_support()?;
            Some(filter())
        } else {
            None
        };
        if ruleset.is_none() && filter.is_none() {
            return Ok(());
        }
        // SAFETY: the function runs in the child process between `fork` and `exec`. It makes only
        // system calls, and it uses no lock and no memory allocation: the rule set and the
        // filter program are ready before the `fork`.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(ruleset) = &ruleset
                    && libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(filter) = &filter {
                    // The build gets no open file of Semilla except the three standard ones: an
                    // open socket of the parent process would pass the filter. The flag closes
                    // each file at the `exec`. The call needs Linux 5.11, which is older than
                    // each kernel with the Landlock version of the sandbox.
                    if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, CLOSE_RANGE_CLOEXEC) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let program = Program { len: filter.len() as libc::c_ushort, filter: filter.as_ptr() };
                    if libc::prctl(libc::PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program as *const Program, 0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
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
        // signal. `PR_SET_PDEATHSIG` stops `cargo` if the Semilla thread stops first.
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

    /// Waits until the leader of the group ends, and then stops each process of the group. The
    /// leader stays in the process table until the caller reads its state, so no new process
    /// can get the number of the group before the signal.
    pub fn wait_and_stop(leader: u32) {
        // SAFETY: `waitid` with `WNOWAIT` reads the state of the child of this process and does
        // not remove it. `kill` with a negative number sends the signal to each process of the
        // group: `cargo`, each `rustc`, each build script, and each child of them. The seccomp
        // filter stops `setsid` and `setpgid`, so no process of the build leaves the group.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            while libc::waitid(libc::P_PID, leader as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT) != 0 {
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break;
                }
            }
            libc::kill(-(leader as i32), libc::SIGKILL);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod group {
    pub fn start(_command: &mut std::process::Command) {}

    pub fn wait_and_stop(_leader: u32) {}
}

/// Runs a build command to its end, and then stops each process that the build left (batch 5v,
/// part F, rule 3).
///
/// The command gets its own process group. When `cargo` ends, Semilla sends `SIGKILL` to the
/// group. With the network limit, the seccomp filter stops `setsid` and `setpgid`, so each
/// process of the build is in the group at that time. With no network limit, a process that
/// leaves the group continues, and the output names the missing limit.
pub fn run_and_stop(command: &mut Command) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    group::start(command);
    let mut child = command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn()?;
    // A process of the build can hold the output pipes after `cargo` ends. So two threads read
    // the pipes, and this thread waits only for `cargo`.
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut data = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut data);
            }
            data
        })
    };
    let stdout = reader(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let stderr = reader(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    group::wait_and_stop(child.id());
    let status = child.wait()?;
    Ok(std::process::Output { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::Sandbox;
    use std::process::Command;

    pub fn abi() -> Result<i64, String> {
        Err("Semilla has a sandbox only on Linux (Landlock and seccomp)".into())
    }

    pub fn filter_support() -> Result<(), String> {
        Err("Semilla has a sandbox only on Linux (Landlock and seccomp)".into())
    }

    pub fn apply(_sandbox: &Sandbox, _command: &mut Command, files: bool, network: bool) -> Result<(), String> {
        if files || network { abi().map(|_| ()) } else { Ok(()) }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// True if this machine has the full sandbox and the variable is not set.
    fn full() -> bool {
        match missing() {
            Some(missing) => {
                eprintln!("no full sandbox: {}", missing.line());
                false
            }
            None => true,
        }
    }

    fn shell(dir: &std::path::Path, script: &str, sandbox: &Sandbox) -> std::process::Output {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script).current_dir(dir).env_clear().env("PATH", "/usr/bin:/bin");
        sandbox.apply(&mut command).unwrap();
        run_and_stop(&mut command).unwrap()
    }

    /// A command in the sandbox writes a folder of the allow list, and no other path.
    #[test]
    fn a_command_in_the_sandbox_writes_only_the_allow_list() {
        if !full() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (open, closed) = (dir.path().join("open"), dir.path().join("closed"));
        std::fs::create_dir_all(&open).unwrap();
        std::fs::create_dir_all(&closed).unwrap();
        std::fs::write(closed.join("old.txt"), "old").unwrap();
        std::fs::write(closed.join("one.txt"), "old").unwrap();
        let script = "echo new > open/a.txt; echo new > closed/a.txt; echo new > closed/old.txt; rm closed/old.txt; mv open/a.txt closed/b.txt; echo new > closed/one.txt; true";
        let output = shell(dir.path(), script, &Sandbox { folders: vec![open.clone()], files: vec![closed.join("one.txt")], read: Vec::new() });
        assert!(output.status.success());
        assert_eq!(std::fs::read_to_string(open.join("a.txt")).unwrap(), "new\n", "the build writes its folder");
        assert!(!closed.join("a.txt").exists(), "no new file outside the allow list");
        assert!(!closed.join("b.txt").exists(), "no move to a folder outside the allow list");
        assert_eq!(std::fs::read_to_string(closed.join("old.txt")).unwrap(), "old", "no change and no removal of a file outside the allow list");
        assert_eq!(std::fs::read_to_string(closed.join("one.txt")).unwrap(), "new\n", "a file of the allow list is writable");
        // The sandbox is in the child only: this process writes as before.
        std::fs::write(closed.join("parent.txt"), "x").unwrap();
    }

    /// A command in the sandbox reads a path of the read list, and no other path (part C).
    #[test]
    fn a_command_in_the_sandbox_reads_only_the_allow_list() {
        if !full() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (open, closed, write) = (dir.path().join("open"), dir.path().join("closed"), dir.path().join("write"));
        for folder in [&open, &closed, &write] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join("a.txt"), "text").unwrap();
        }
        std::fs::write(closed.join("one.txt"), "text").unwrap();
        // Each test prints only "yes" or "no": the content of a file is not in the output.
        let can = |what: &str| format!("if {what} >/dev/null 2>&1; then echo yes; else echo no; fi");
        let script = [
            can("cat open/a.txt"),
            can("cat write/a.txt"),
            can("cat closed/a.txt"),
            can("ls closed"),
            can("cat closed/one.txt"),
            can("cat /proc/self/environ"),
            can("cat /proc/1/cmdline"),
            can("ls /proc"),
            can("ls /sys"),
            can("ls \"$HOME_FOLDER\""),
            can("cat /etc/passwd"),
            can("ls /tmp"),
            can("ls /usr/bin"),
        ]
        .join("; ");
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script).current_dir(dir.path()).env_clear().env("PATH", "/usr/bin:/bin");
        // The home folder of the user of the test: the test checks only if a list of it works.
        command.env("HOME_FOLDER", std::env::var_os("HOME").unwrap_or_else(|| "/root".into()));
        Sandbox { folders: vec![write.clone()], files: Vec::new(), read: vec![open.clone(), closed.join("one.txt"), closed.join("missing.txt")] }.apply(&mut command).unwrap();
        let output = run_and_stop(&mut command).unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let answers: Vec<&str> = text.lines().collect();
        assert_eq!(answers, ["yes", "yes", "no", "no", "yes", "no", "no", "no", "no", "no", "no", "no", "yes"], "{}", String::from_utf8_lossy(&output.stderr));
    }

    /// A command in the sandbox makes no socket (part D): no TCP, no UDP, no UNIX socket, no
    /// netlink. A packet for a listener on `127.0.0.1` does not arrive.
    #[test]
    fn a_command_in_the_sandbox_has_no_network() {
        if !full() {
            return;
        }
        let Some(python) = ["/usr/bin/python3", "/bin/python3"].into_iter().find(|path| std::path::Path::new(path).is_file()) else {
            eprintln!("no python3 on this machine");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(std::time::Duration::from_millis(300))).unwrap();
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        tcp.set_nonblocking(true).unwrap();
        // The listener has an abstract name: the path of a test folder can be too long for a
        // socket path. The filter stops the `socket` call, so the two name forms get one check.
        let name = format!("semilla-test-{}", std::process::id());
        let address = <std::os::unix::net::SocketAddr as std::os::linux::net::SocketAddrExt>::from_abstract_name(name.as_bytes()).unwrap();
        let unix = std::os::unix::net::UnixListener::bind_addr(&address).unwrap();
        unix.set_nonblocking(true).unwrap();
        let script = format!(
            r#"
import socket, os
def attempt(name, family, kind, target=None, proto=0):
    try:
        s = socket.socket(family, kind, proto)
        if target is not None:
            if kind == socket.SOCK_DGRAM:
                s.sendto(b"x", target)
            else:
                s.connect(target)
        print(name, "open")
    except OSError as error:
        print(name, "stopped", error.errno)
attempt("udp", socket.AF_INET, socket.SOCK_DGRAM, ("127.0.0.1", {udp}))
attempt("tcp", socket.AF_INET, socket.SOCK_STREAM, ("127.0.0.1", {tcp}))
attempt("udp6", socket.AF_INET6, socket.SOCK_DGRAM)
attempt("unix", socket.AF_UNIX, socket.SOCK_STREAM, "{unix}")
attempt("abstract", socket.AF_UNIX, socket.SOCK_STREAM, "\0{name}")
attempt("netlink", socket.AF_NETLINK, socket.SOCK_RAW)
attempt("raw", socket.AF_INET, socket.SOCK_RAW, None, 1)
attempt("pair", socket.AF_UNIX, socket.SOCK_STREAM)
try:
    socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    print("local pair open")
except OSError as error:
    print("local pair stopped", error.errno)
try:
    socket.socketpair(socket.AF_INET, socket.SOCK_STREAM)
    print("pair open")
except OSError as error:
    print("pair stopped", error.errno)
try:
    os.setsid()
    print("setsid open")
except OSError as error:
    print("setsid stopped", error.errno)
"#,
            udp = udp.local_addr().unwrap().port(),
            tcp = tcp.local_addr().unwrap().port(),
            unix = dir.path().join("socket").display(),
            name = name,
        );
        let mut command = Command::new(python);
        command.arg("-I").arg("-c").arg(script).current_dir(dir.path()).env_clear();
        Sandbox { folders: Vec::new(), files: Vec::new(), read: vec![dir.path().to_path_buf()] }.apply(&mut command).unwrap();
        let output = run_and_stop(&mut command).unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let eperm = libc::EPERM;
        // A pair of the family `AF_UNIX` with no name is the one socket that a build can make.
        let expected: Vec<String> = ["udp", "tcp", "udp6", "unix", "abstract", "netlink", "raw", "pair", "local pair", "pair", "setsid"]
            .iter()
            .map(|name| if *name == "local pair" { "local pair open".to_string() } else { format!("{name} stopped {eperm}") })
            .collect();
        assert_eq!(text.lines().collect::<Vec<_>>(), expected, "{}", String::from_utf8_lossy(&output.stderr));
        let mut buffer = [0u8; 8];
        assert!(udp.recv_from(&mut buffer).is_err(), "no UDP packet arrives");
        assert!(tcp.accept().is_err(), "no TCP connection arrives");
        assert!(unix.accept().is_err(), "no UNIX connection arrives");
    }

    /// The filter stops a system call of a different architecture: the 32-bit entry of x86_64
    /// (`int 0x80`). The call is `getpid` (number 20 of the 32-bit table). It gives `-EPERM`.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_filter_stops_a_system_call_of_a_different_architecture() {
        if !full() {
            return;
        }
        if std::env::var_os("SEMILLA_TEST_INT80").is_some() {
            // The child: the filter is active. The exit code says what the 32-bit entry gave.
            let result: i64;
            // SAFETY: `int 0x80` with the number 20 (`getpid` of the 32-bit table) reads no
            // memory. With no 32-bit support in the kernel, the result is `-ENOSYS`.
            unsafe {
                std::arch::asm!("int 0x80", inlateout("rax") 20i64 => result, options(nostack));
            }
            std::process::exit(if result == -(libc::EPERM as i64) { 7 } else if result == -(libc::ENOSYS as i64) { 8 } else { 9 });
        }
        let exe = std::env::current_exe().unwrap();
        let mut command = Command::new(&exe);
        command.args(["--exact", "sandbox::tests::the_filter_stops_a_system_call_of_a_different_architecture", "--nocapture"]).env("SEMILLA_TEST_INT80", "1");
        let folder = exe.parent().unwrap().to_path_buf();
        let read: Vec<PathBuf> = std::env::var_os("LD_LIBRARY_PATH").map(|paths| std::env::split_paths(&paths).collect()).unwrap_or_default();
        Sandbox { folders: Vec::new(), files: Vec::new(), read: read.into_iter().chain([folder]).collect() }.apply(&mut command).unwrap();
        let output = run_and_stop(&mut command).unwrap();
        assert_eq!(output.status.code(), Some(7), "the 32-bit entry gives EPERM: {}", String::from_utf8_lossy(&output.stderr));
    }

    /// No process of a command continues after the command ends (part F, rule 3): a child that
    /// tries to leave the process group stays in the group, and Semilla stops the group.
    #[test]
    fn no_process_continues_after_the_command() {
        if !full() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // The child process waits, and then it writes a marker file. `setsid` fails in the
        // sandbox, so the shell runs the command in the same group.
        let script = "(setsid sh -c 'sleep 1; echo late > late.txt' || sh -c 'sleep 1; echo late > late.txt') >/dev/null 2>&1 & echo started > started.txt";
        let started = std::time::Instant::now();
        let output = shell(dir.path(), script, &Sandbox { folders: vec![dir.path().to_path_buf()], files: Vec::new(), read: Vec::new() });
        assert!(output.status.success());
        assert!(started.elapsed() < std::time::Duration::from_millis(900), "the command does not wait for the child");
        assert!(dir.path().join("started.txt").exists());
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(!dir.path().join("late.txt").exists(), "the child process does not continue");
    }

    /// The line names each missing limit.
    #[test]
    fn the_line_names_each_missing_limit() {
        let all = Missing { limits: vec![Limit::Write, Limit::Read, Limit::Network], cause: "cause".into() };
        assert_eq!(all.line(), "the build ran with no sandbox: the write limit, the read limit, and the network limit are missing (cause). A build script or a proc macro of this change set can change the result");
        assert!(all.line().starts_with(NO_SANDBOX));
        let one = Missing { limits: vec![Limit::Network], cause: "cause".into() };
        assert_eq!(one.line(), "the build ran with a partial sandbox: the network limit is missing (cause). A build script or a proc macro of this change set can change the result");
        let two = Missing { limits: vec![Limit::Write, Limit::Read], cause: "cause".into() };
        assert!(two.line().starts_with("the build ran with a partial sandbox: the write limit and the read limit are missing (cause)."));
        assert_ne!(all.key(), one.key());
    }
}
