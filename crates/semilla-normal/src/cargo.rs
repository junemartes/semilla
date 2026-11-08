//! Runs `cargo check` on a workspace with the semilla MIR driver as the workspace wrapper.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The toolchain file of the semilla workspace. The driver links to this exact nightly, so the
/// checked workspace must use the same toolchain (the metadata of its crates must match).
const TOOLCHAIN_FILE: &str = include_str!("../../../rust-toolchain.toml");

/// Returns the toolchain name of the driver: `SEMILLA_TOOLCHAIN` if it is set, else the channel in
/// the semilla `rust-toolchain.toml`.
pub fn toolchain() -> String {
    if let Ok(name) = std::env::var("SEMILLA_TOOLCHAIN") {
        return name;
    }
    TOOLCHAIN_FILE
        .lines()
        .find_map(|line| {
            let value = line.trim().strip_prefix("channel")?.trim().strip_prefix('=')?.trim();
            Some(value.trim_matches('"').to_string())
        })
        .expect("rust-toolchain.toml has a channel")
}

/// Finds the driver binary: `SEMILLA_MIR_DRIVER`, else next to the current executable (or one
/// folder up, for test binaries in `target/debug/deps`), else `target/debug` of the semilla
/// workspace.
pub fn find_driver() -> Result<PathBuf> {
    const NAME: &str = "semilla-mir-driver";
    if let Some(path) = std::env::var_os("SEMILLA_MIR_DRIVER") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!("SEMILLA_MIR_DRIVER is {}, but that file does not exist", path.display());
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(NAME));
            if let Some(up) = dir.parent() {
                candidates.push(up.join(NAME));
            }
        }
    }
    candidates.push(semilla_workspace().join("target/debug").join(NAME));
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .context("cannot find semilla-mir-driver. Run `cargo build -p semilla-mir`, or set SEMILLA_MIR_DRIVER")
}

/// The root folder of the semilla workspace (where this crate was compiled).
pub fn semilla_workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Returns the `lib` folder of the toolchain sysroot. The driver needs it at run time.
pub fn sysroot_lib(toolchain: &str) -> Result<PathBuf> {
    let output = Command::new("rustc")
        .args(["--print", "sysroot"])
        .env("RUSTUP_TOOLCHAIN", toolchain)
        .output()
        .context("cannot run rustc to find the sysroot")?;
    if !output.status.success() {
        bail!("rustc --print sysroot failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let sysroot = String::from_utf8(output.stdout).context("the sysroot path is not UTF-8")?;
    Ok(PathBuf::from(sysroot.trim()).join("lib"))
}

/// Runs `cargo check --workspace --all-targets` on `workspace` with the driver as
/// `RUSTC_WORKSPACE_WRAPPER`. The driver writes one `.jsonl` file of records for each compiled
/// target to `out`. Each call compiles all workspace crates again (a new `SEMILLA_MIR_RUN`
/// value), so `out` gets a full set of records. The dependencies outside the workspace stay in
/// the cache of `target_dir`.
pub fn run_cargo_check(workspace: &Path, level: &str, out: &Path, target_dir: &Path) -> Result<()> {
    if !matches!(level, "n0" | "n2" | "n3") {
        bail!("unknown level {level:?}: use n0, n2, or n3");
    }
    let driver = find_driver()?;
    let toolchain = toolchain();
    let lib = sysroot_lib(&toolchain)?;
    std::fs::create_dir_all(out).with_context(|| format!("cannot create {}", out.display()))?;
    let out = std::fs::canonicalize(out)?;
    let library_path = match std::env::var_os("LD_LIBRARY_PATH") {
        Some(old) if !old.is_empty() => {
            let mut paths = vec![lib.clone()];
            paths.extend(std::env::split_paths(&old));
            std::env::join_paths(paths)?
        }
        _ => lib.into_os_string(),
    };
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()
    );

    let mut command = Command::new("cargo");
    command
        .args(["check", "--workspace", "--all-targets", "--quiet", "--target-dir"])
        .arg(target_dir)
        .current_dir(workspace)
        .env("RUSTUP_TOOLCHAIN", &toolchain)
        .env("RUSTC_WORKSPACE_WRAPPER", &driver)
        .env("LD_LIBRARY_PATH", library_path)
        .env("SEMILLA_MIR_LEVEL", level)
        .env("SEMILLA_MIR_OUT", &out)
        .env("SEMILLA_MIR_RUN", nonce);
    // Settings of an outer cargo (for example `cargo test`) must not leak into this build.
    for name in [
        "RUSTC",
        "RUSTC_WRAPPER",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_MANIFEST_DIR",
    ] {
        command.env_remove(name);
    }
    let output = command.output().context("cannot run cargo")?;
    if !output.status.success() {
        bail!(
            "cargo check failed in {} (level {level}):\n{}",
            workspace.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
