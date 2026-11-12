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

/// True for an environment variable that changes a build: the rust flags, and the `CARGO_PROFILE_`,
/// `CARGO_BUILD_`, and `CARGO_TARGET_` settings.
pub fn is_build_setting_env(name: &str) -> bool {
    matches!(name, "RUSTFLAGS" | "CARGO_ENCODED_RUSTFLAGS" | "RUSTDOCFLAGS" | "CARGO_ENCODED_RUSTDOCFLAGS")
        || name.starts_with("CARGO_PROFILE_")
        || name.starts_with("CARGO_BUILD_")
        || (name.starts_with("CARGO_TARGET_") && name != "CARGO_TARGET_DIR")
}

/// The key of the build settings that are not in the tree: the cargo configuration files of the
/// parent folders of the build folder and of `CARGO_HOME`, the toolchain, and the MIR driver
/// binary (its path, its size, and its time). A cached record is valid only for the same key.
/// So a record that a different driver or a different toolchain wrote is never read (batch 5h).
pub fn settings_key(build_base: &Path, driver: Option<&Path>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut configs: Vec<PathBuf> = build_base.ancestors().flat_map(|dir| [dir.join(".cargo/config.toml"), dir.join(".cargo/config")]).collect();
    let home = std::env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    if let Some(home) = home {
        configs.extend([home.join("config.toml"), home.join("config")]);
    }
    for file in configs {
        if let Ok(text) = std::fs::read(&file) {
            file.hash(&mut hasher);
            text.hash(&mut hasher);
        }
    }
    toolchain().hash(&mut hasher);
    if let Some(driver) = driver {
        std::fs::canonicalize(driver).unwrap_or_else(|_| driver.to_path_buf()).hash(&mut hasher);
        if let Ok(meta) = std::fs::metadata(driver) {
            meta.len().hash(&mut hasher);
            meta.modified().ok().hash(&mut hasher);
        }
    }
    format!("{:012x}", hasher.finish() & 0xffff_ffff_ffff)
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

/// The target triple of the check build: the host of the toolchain. The check build gives no
/// `--target` to cargo. The answer is empty if `rustc` does not run.
pub fn host_target() -> String {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        let output = Command::new("rustc").arg("-vV").env("RUSTUP_TOOLCHAIN", toolchain()).output();
        let text = output.map(|output| String::from_utf8_lossy(&output.stdout).to_string()).unwrap_or_default();
        text.lines().find_map(|line| line.strip_prefix("host: ")).unwrap_or("").trim().to_string()
    })
    .clone()
}

/// The feature selection of a build configuration (design 8.12).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Features {
    /// The default features of each package.
    Default,
    /// `--no-default-features`.
    NoDefault,
    /// `--all-features`.
    All,
    /// The default features and a list (`--features a,b`). The list is sorted.
    Listed(Vec<String>),
}

/// One build configuration of a check build (batch 5s, A1): a cargo profile and a feature
/// selection, for the target of this machine.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Configuration {
    /// True for the `release` profile of the workspace. False for the `dev` profile.
    pub release: bool,
    pub features: Features,
    /// True for the smaller build (batch 5t, part C): the libraries and the binaries only
    /// (`--lib --bins`). False for the build of each target (`--all-targets`). Semilla uses
    /// the smaller build only if the build of each target fails before the step.
    pub narrow: bool,
}

impl Configuration {
    /// The `dev` profile with the default features.
    pub const DEV: Configuration = Configuration { release: false, features: Features::Default, narrow: false };

    /// The end of the name of a smaller build.
    pub const NARROW: &'static str = " (libraries and binaries only)";

    /// A configuration with the build of each target.
    pub fn new(release: bool, features: Features) -> Configuration {
        Configuration { release, features, narrow: false }
    }

    /// The smaller build of this configuration: the libraries and the binaries only.
    pub fn narrowed(&self) -> Configuration {
        Configuration { narrow: true, ..self.clone() }
    }

    /// The short fixed name: `dev`, `release`, `dev, no default features`,
    /// `release, all features`, `dev, features a,b`. The name of a smaller build ends with
    /// [`Configuration::NARROW`].
    pub fn name(&self) -> String {
        let profile = if self.release { "release" } else { "dev" };
        let name = match &self.features {
            Features::Default => profile.to_string(),
            Features::NoDefault => format!("{profile}, no default features"),
            Features::All => format!("{profile}, all features"),
            Features::Listed(list) => format!("{profile}, features {}", list.join(",")),
        };
        if self.narrow { format!("{name}{}", Configuration::NARROW) } else { name }
    }

    /// The suffix of each folder name of the configuration. It is empty for `dev`.
    pub fn suffix(&self) -> String {
        let profile = if self.release { "-release" } else { "" };
        let features = match &self.features {
            Features::Default => String::new(),
            Features::NoDefault => "-nd".to_string(),
            Features::All => "-all".to_string(),
            Features::Listed(list) => {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                list.hash(&mut hasher);
                format!("-f{:012x}", hasher.finish() & 0xffff_ffff_ffff)
            }
        };
        format!("{profile}{features}{}", if self.narrow { "-lb" } else { "" })
    }

    /// The cargo arguments that select the targets of the build.
    pub fn target_args(&self) -> &'static [&'static str] {
        if self.narrow { &["--lib", "--bins"] } else { &["--all-targets"] }
    }

    /// The cargo arguments that select the configuration.
    pub fn cargo_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.release {
            args.push("--release".to_string());
        }
        match &self.features {
            Features::Default => {}
            Features::NoDefault => args.push("--no-default-features".to_string()),
            Features::All => args.push("--all-features".to_string()),
            Features::Listed(list) => {
                args.push("--features".to_string());
                args.push(list.join(","));
            }
        }
        args
    }
}

/// The feature state of each workspace package for a feature selection, by the manifests
/// (`cargo metadata --no-deps`): the features of the selection, and each feature that they
/// name in the feature table of the package.
///
/// Two selections with the same state give the same build: cargo computes each feature of the
/// build from this state and the manifests. So Semilla checks only one of them (batch 5s, A2).
pub fn feature_state(metadata: &serde_json::Value, features: &Features) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let mut state = std::collections::BTreeMap::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let name = package["name"].as_str().unwrap_or("").to_string();
        let table: std::collections::BTreeMap<&str, Vec<&str>> = package["features"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(key, value)| (key.as_str(), value.as_array().into_iter().flatten().filter_map(|v| v.as_str()).collect()))
            .collect();
        let mut on: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut todo: Vec<String> = Vec::new();
        let default = table.contains_key("default").then(|| "default".to_string());
        match features {
            Features::Default => todo.extend(default),
            Features::NoDefault => {}
            Features::All => {
                todo.extend(table.keys().map(|key| key.to_string()));
                // An optional dependency is a feature, also when the table does not list it.
                for dependency in package["dependencies"].as_array().into_iter().flatten() {
                    if dependency["optional"].as_bool() == Some(true) {
                        let name = dependency["rename"].as_str().or(dependency["name"].as_str()).unwrap_or("");
                        todo.push(format!("dep:{name}"));
                    }
                }
            }
            Features::Listed(list) => {
                todo.extend(default);
                for feature in list {
                    // `package/feature` selects a feature of one package.
                    match feature.split_once('/') {
                        Some((package, feature)) if package == name => todo.push(feature.to_string()),
                        Some(_) => todo.push(feature.clone()),
                        None if table.contains_key(feature.as_str()) => todo.push(feature.clone()),
                        None => {}
                    }
                }
            }
        }
        while let Some(feature) = todo.pop() {
            if on.insert(feature.clone()) {
                todo.extend(table.get(feature.as_str()).into_iter().flatten().map(|f| f.to_string()));
            }
        }
        state.insert(name, on);
    }
    state
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
    run_check(workspace, level, out, target_dir, &nonce, false, &Configuration::DEV).map(|_| ())
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
    run_cargo_check_delta_features(workspace, level, out, target_dir, &[])
}

/// [`run_cargo_check_delta`] with a list of features (`--features`). An empty list builds the
/// default features. Use one build folder and one records folder for each feature set.
pub fn run_cargo_check_delta_features(
    workspace: &Path,
    level: &str,
    out: &Path,
    target_dir: &Path,
    features: &[String],
) -> Result<()> {
    let features = if features.is_empty() { Features::Default } else { Features::Listed(features.to_vec()) };
    run_cargo_check_delta_config(workspace, level, out, target_dir, &Configuration::new(false, features))
}

/// [`run_cargo_check_delta`] for one build configuration. Use one build folder, one target
/// folder, and one records folder for each configuration.
pub fn run_cargo_check_delta_config(
    workspace: &Path,
    level: &str,
    out: &Path,
    target_dir: &Path,
    config: &Configuration,
) -> Result<()> {
    let driver = find_driver()?;
    let meta = std::fs::metadata(&driver)?;
    let modified = meta.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    let fingerprint = format!("driver-{}-{modified}", meta.len());
    let units = run_check(workspace, level, out, target_dir, &fingerprint, true, config)?;
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

/// The `error` lines of a cargo output, each with a few lines of context, so that the real cause
/// is not lost under the warnings. Without an `error` line, the last lines.
fn first_errors(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().collect();
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("error") {
            out.extend(lines[index..lines.len().min(index + 8)].iter().copied());
            out.push("");
            if out.len() > 48 {
                break;
            }
        }
    }
    if out.is_empty() {
        out = lines[lines.len().saturating_sub(30)..].to_vec();
    }
    out.join("\n")
}

/// The error text of a failed cargo run: the first compiler errors with their cause, then the
/// `error` lines of cargo. With `--message-format=json`, the compiler writes each diagnostic as
/// a JSON message to the standard output, and the standard error has only the last line
/// ("could not compile …"). So the text of the compiler errors comes from the JSON messages
/// (trial 4, D14).
pub(crate) fn error_text(stdout: &str, stderr: &str) -> String {
    let mut causes: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if message.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let Some(diagnostic) = message.get("message") else { continue };
        if diagnostic.get("level").and_then(|l| l.as_str()).is_none_or(|level| !level.starts_with("error")) {
            continue;
        }
        let rendered = diagnostic.get("rendered").and_then(|r| r.as_str()).or_else(|| diagnostic.get("message").and_then(|m| m.as_str()));
        // The summary "aborting due to N previous errors" is not a cause.
        if let Some(text) = rendered.filter(|text| !text.contains("aborting due to") && !causes.iter().any(|known| known == text.trim_end())) {
            causes.push(text.trim_end().to_string());
        }
        if causes.len() == 5 {
            break;
        }
    }
    let rest = first_errors(stderr);
    if causes.is_empty() {
        return rest;
    }
    format!("{}\n\n{rest}", causes.join("\n\n"))
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

/// The packages and the targets of a workspace, as cargo sees them: the output of
/// `cargo metadata --no-deps --offline --format-version 1`. The command reads the manifests and
/// the cargo configuration of the folder. It builds nothing and runs no code of the folder: with
/// `--no-deps`, cargo does not start the compiler, so it starts no program that a configuration
/// file names (batch 5i, A2: tested with `build.rustc` and `build.rustc-wrapper`). The command
/// runs in the sandbox with an empty write set.
pub fn cargo_metadata(workspace: &Path) -> Result<serde_json::Value> {
    metadata_command(workspace, Graph::NoDeps, &crate::sandbox::Sandbox::default())
}

/// The packages of a workspace with the dependency graph: the output of
/// `cargo metadata --offline --format-version 1`.
///
/// This command starts the compiler (`rustc -vV`), so it starts the program that
/// `build.rustc` or `build.rustc-wrapper` of a configuration file of the tree names. The caller
/// must not use it on a tree whose configuration files are not trusted (batch 5i, A1).
///
/// The command runs in the sandbox with an empty write set. Cargo must write a lock file when
/// the tree has none. Then the command runs on a copy of the tree in `scratch` (hard links), and
/// its write set is the lock file of the copy. The paths of the result are then paths of the copy:
/// use them relative to `workspace_root`.
///
/// A tree with a lock file: the command has `--locked`, so cargo does not write the lock file.
/// A lock file that does not agree with the manifests gives the error [`StaleLock`].
pub fn cargo_metadata_full(workspace: &Path, scratch: &Path) -> Result<serde_json::Value> {
    if workspace.join("Cargo.lock").is_file() {
        return metadata_command(workspace, Graph::Locked, &crate::sandbox::Sandbox::default());
    }
    let _ = std::fs::remove_dir_all(scratch);
    link_tree(workspace, scratch).with_context(|| format!("cannot copy {} to {}", workspace.display(), scratch.display()))?;
    let lock = scratch.join("Cargo.lock");
    std::fs::write(&lock, "")?;
    let result = metadata_command(scratch, Graph::Full, &crate::sandbox::Sandbox { folders: Vec::new(), files: vec![lock] });
    let _ = std::fs::remove_dir_all(scratch);
    result
}

/// Copies a folder tree with hard links (a copy of the data when a link is not possible).
fn link_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            link_tree(&source, &target)?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&source)?, &target)?;
        } else if std::fs::hard_link(&source, &target).is_err() {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// The lock file of a tree does not agree with the manifests of the tree: cargo must change the
/// lock file before it can build the tree. Semilla does not write a file of a tree.
#[derive(Debug)]
pub struct StaleLock {
    /// The folder of the tree.
    pub folder: std::path::PathBuf,
}

impl std::fmt::Display for StaleLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`Cargo.lock` does not agree with the manifests in {}", self.folder.display())
    }
}

impl std::error::Error for StaleLock {}

/// The part of the dependency data that `cargo metadata` gives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Graph {
    /// The packages of the workspace only. Cargo does not start the compiler.
    NoDeps,
    /// The full graph. Cargo can write the lock file.
    Full,
    /// The full graph from the lock file of the tree. Cargo does not write the lock file.
    Locked,
}

fn metadata_command(workspace: &Path, graph: Graph, sandbox: &crate::sandbox::Sandbox) -> Result<serde_json::Value> {
    let mut command = Command::new("cargo");
    command.arg("metadata");
    match graph {
        Graph::NoDeps => drop(command.arg("--no-deps")),
        Graph::Full => {}
        Graph::Locked => drop(command.arg("--locked")),
    }
    command.args(["--offline", "--format-version", "1"]).current_dir(workspace).env("RUSTUP_TOOLCHAIN", toolchain());
    for name in ["RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_MANIFEST_DIR"] {
        command.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy().into_owned();
        if is_build_setting_env(&name) {
            command.env_remove(&name);
        }
    }
    sandbox.apply_to_tool(&mut command);
    let output = command.output().context("cannot run cargo")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Only this command gives `--locked`, so only the lock file rule of cargo names the flag.
        // A package that is not in the cargo cache gives a different error (`--offline`).
        if graph == Graph::Locked && stderr.lines().any(|line| line.starts_with("error") && line.contains("lock file") && line.contains("--locked was passed")) {
            return Err(anyhow::Error::new(StaleLock { folder: workspace.to_path_buf() }));
        }
        bail!("cargo cannot read the manifests in {}:\n{}", workspace.display(), first_errors(&stderr));
    }
    serde_json::from_slice(&output.stdout).context("the output of `cargo metadata` is not JSON")
}

/// The text for a check build that stops because a dependency is not in the cargo cache. The
/// step output uses it to tell that a code change is not necessary.
pub const MISSING_DEPENDENCY: &str = "Semilla has no result for the bodies: the check build does not download (cargo runs with `--offline`), and a dependency is not in the cargo cache of this machine. Run `cargo fetch` in the repository, then run the command again. A code change is not necessary.";

/// True if the error text of cargo says that cargo cannot get a dependency. The list has the
/// texts of cargo for a package that is not in the cache and for a registry that cargo cannot
/// read. Each other error stays a failed build.
pub fn missing_dependency(text: &str) -> bool {
    [
        "failed to download",
        "unable to update registry",
        "failed to load source for dependency",
        "no matching package named",
        "you're using offline mode (--offline)",
        "--offline was specified",
    ]
    .iter()
    .any(|mark| text.contains(mark))
}

fn run_check(
    workspace: &Path,
    level: &str,
    out: &Path,
    target_dir: &Path,
    nonce: &str,
    json: bool,
    config: &Configuration,
) -> Result<std::collections::BTreeSet<String>> {
    let stdout = run_check_command(workspace, level, out, target_dir, nonce, if json { Mode::Json } else { Mode::Plain }, config)?;
    Ok(unit_hashes(&stdout))
}

/// The output mode of the cargo command of a check build.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Plain,
    /// `--message-format=json`.
    Json,
}

/// Runs the cargo command of a check build, and returns its standard output.
fn run_check_command(
    workspace: &Path,
    level: &str,
    out: &Path,
    target_dir: &Path,
    nonce: &str,
    mode: Mode,
    config: &Configuration,
) -> Result<String> {
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
    // `--offline`: the check build does not download (the sandbox stops the network). Without the
    // flag, cargo tries to update the registry for a tree with no lock file (a library that
    // ignores `Cargo.lock`), and the build fails. With the flag, cargo uses the packages in the
    // cargo cache of this machine.
    command.args(["check", "--workspace", "--quiet", "--offline"]);
    command.args(config.target_args());
    match mode {
        Mode::Plain => {}
        Mode::Json => drop(command.arg("--message-format=json")),
    }
    command.args(config.cargo_args());
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
    // Build settings from the environment are not in the tree and not in the cache key: remove them,
    // so that the records of a tree depend only on the tree (and the settings in `settings_key`).
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy().into_owned();
        if is_build_setting_env(&name) {
            command.env_remove(&name);
        }
    }
    // The sandbox (design 27): the build can write only the cargo target folder and the records
    // folder. A build script and a proc macro of the tree run in this build. They cannot change
    // the source files of the build, the cache folder of the user, the cargo home folder, or
    // the repository. The temporary files of the build go to the target folder.
    std::fs::create_dir_all(target_dir).with_context(|| format!("cannot create {}", target_dir.display()))?;
    let target_dir = std::fs::canonicalize(target_dir)?;
    let temporary = target_dir.join("tmp");
    std::fs::create_dir_all(&temporary)?;
    command.env("TMPDIR", &temporary);
    let mut sandbox = crate::sandbox::Sandbox { folders: vec![target_dir.clone(), out.clone()], files: Vec::new() };
    // A tree with no lock file: cargo must write one. Semilla makes an empty file, and the build
    // can write this one file. The next build of this folder starts from the files of its tree.
    let lock = workspace.join("Cargo.lock");
    if !lock.exists() && std::fs::write(&lock, "").is_ok() {
        sandbox.files.push(lock);
    }
    if std::env::var_os("SEMILLA_NO_SANDBOX").is_none_or(|value| value.is_empty()) {
        sandbox.apply(&mut command);
    } else {
        crate::sandbox::skip("the variable SEMILLA_NO_SANDBOX is set");
    }
    // The build runs to its end. Then Semilla stops each process that the build left.
    let output = crate::sandbox::run_and_stop(&mut command).context("cannot run cargo")?;
    if !output.status.success() {
        let text = error_text(&String::from_utf8_lossy(&output.stdout), &String::from_utf8_lossy(&output.stderr));
        // A write that the sandbox stops shows as "Permission denied".
        // A dependency that cargo cannot get is not a defect of the code of the tree.
        if missing_dependency(&text) {
            bail!("cargo cannot get a dependency for the check build in {} (level {level}, configuration `{}`):\n{text}\n\n{MISSING_DEPENDENCY}", workspace.display(), config.name());
        }
        let hint = if crate::sandbox::problem().is_none() && text.contains("Permission denied") {
            "\n\nThe check build runs in a sandbox: it can write only its build folder, and it does not download. Possible causes: a dependency is not in the cargo cache of this machine (run `cargo fetch` in the repository); `Cargo.lock` does not agree with a manifest (update it, and put it in the step); a build script or a proc macro writes a file outside `OUT_DIR`."
        } else {
            ""
        };
        let targets = if config.narrow { "the body check builds the libraries and the binaries" } else { "the body check builds all targets, so the test code must compile too" };
        bail!("cargo check failed in {} (level {level}, configuration `{}`); {targets}:\n{text}{hint}", workspace.display(), config.name());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
