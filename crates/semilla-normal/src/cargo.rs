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
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()
    );
    run_check(workspace, level, out, target_dir, &nonce, false).map(|_| ())
}

/// Runs `cargo check` for a delta build: `workspace` is a fixed build folder that changes only by
/// the files of each new tree, and `out` is the fixed records folder of that build folder.
///
/// Cargo compiles only the crates that changed (and the crates that depend on them). The other
/// crates are fresh, and their record files from an earlier build stay valid: a fresh unit has
/// the same sources, flags, dependencies, toolchain, and tracked environment. `SEMILLA_MIR_RUN`
/// is a fingerprint of the driver binary, so a new driver compiles all crates again. After the
/// build, the record files of units that are not in this build are removed.
pub fn run_cargo_check_delta(workspace: &Path, level: &str, out: &Path, target_dir: &Path) -> Result<()> {
    let driver = find_driver()?;
    let meta = std::fs::metadata(&driver)?;
    let modified = meta.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    let fingerprint = format!("driver-{}-{modified}", meta.len());
    let units = run_check(workspace, level, out, target_dir, &fingerprint, true)?;
    for entry in std::fs::read_dir(out)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(stem) = name.strip_suffix(".jsonl") else { continue };
        let hash = stem.rsplit('.').next().unwrap_or("");
        if !units.contains(hash) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// The hash of each unit in the JSON messages of cargo: the part after the last `-` of each
/// artifact file name (`libname-<hash>.rmeta`).
fn unit_hashes(stdout: &str) -> std::collections::BTreeSet<String> {
    let mut hashes = std::collections::BTreeSet::new();
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if message.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        for file in message.get("filenames").and_then(|f| f.as_array()).into_iter().flatten() {
            let Some(file) = file.as_str() else { continue };
            let name = Path::new(file).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if let Some((_, hash)) = name.rsplit_once('-') {
                hashes.insert(hash.to_string());
            }
        }
    }
    hashes
}

fn run_check(
    workspace: &Path,
    level: &str,
    out: &Path,
    target_dir: &Path,
    nonce: &str,
    json: bool,
) -> Result<std::collections::BTreeSet<String>> {
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
    let mut command = Command::new("cargo");
    command.args(["check", "--workspace", "--all-targets", "--quiet"]);
    if json {
        command.arg("--message-format=json");
    }
    command
        .arg("--target-dir")
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
    Ok(unit_hashes(&String::from_utf8_lossy(&output.stdout)))
}
