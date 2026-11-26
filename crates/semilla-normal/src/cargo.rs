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

/// The cargo home folder of the builds: `CARGO_HOME` if it is an absolute path, else
/// `$HOME/.cargo`. The path is part of [`settings_key`]: a different cargo home has different
/// records.
pub fn cargo_home() -> Option<PathBuf> {
    let absolute = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute());
    let home = absolute("CARGO_HOME").or_else(|| absolute("HOME").map(|home| home.join(".cargo")))?;
    Some(std::fs::canonicalize(&home).unwrap_or(home))
}

/// The folder of the toolchain of the driver (the sysroot). Semilla starts `bin/cargo` and
/// `bin/rustc` of this folder by their full paths, so no program of `PATH` and no rustup proxy
/// decides the compiler (batch 5v, part A).
///
/// The folder is `toolchains/<name>` of the rustup home (`RUSTUP_HOME` if it is an absolute
/// path, else `$HOME/.rustup`), or the one folder there whose name starts with `<name>-`. The
/// path, and the size and the time of `bin/rustc`, are part of [`settings_key`].
pub fn toolchain_root() -> Result<PathBuf> {
    static ROOT: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let name = toolchain();
        let Some(home) = rustup_home() else {
            return Err("cannot find the rustup home: set RUSTUP_HOME or HOME to an absolute path".to_string());
        };
        let folder = home.join("toolchains");
        let is_toolchain = |path: &Path| path.join("bin/cargo").is_file() && path.join("bin/rustc").is_file();
        let mut found: Vec<PathBuf> = vec![folder.join(&name)].into_iter().filter(|path| is_toolchain(path)).collect();
        if found.is_empty() {
            let start = format!("{name}-");
            found = std::fs::read_dir(&folder).into_iter().flatten().flatten().map(|entry| entry.path()).filter(|path| path.file_name().is_some_and(|file| file.to_string_lossy().starts_with(&start)) && is_toolchain(path)).collect();
            found.sort();
        }
        match found.as_slice() {
            [one] => Ok(std::fs::canonicalize(one).unwrap_or_else(|_| one.clone())),
            [] => Err(format!("cannot find the toolchain `{name}` in {}. Run `rustup toolchain install {name}`, or set RUSTUP_HOME", folder.display())),
            _ => Err(format!("{} has more than one toolchain `{name}`: set SEMILLA_TOOLCHAIN to the full name", folder.display())),
        }
    })
    .clone()
    .map_err(|text| anyhow::anyhow!(text))
}

/// The rustup home: `RUSTUP_HOME` if it is an absolute path, else `$HOME/.rustup`.
fn rustup_home() -> Option<PathBuf> {
    let absolute = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute());
    absolute("RUSTUP_HOME").or_else(|| absolute("HOME").map(|home| home.join(".rustup")))
}

/// What the toolchain file of a tree names (`rust-toolchain.toml` or `rust-toolchain`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeToolchain {
    /// The key `channel`, or the one line of a file in the old form.
    Channel(String),
    /// The key `path`: the folder of a toolchain.
    Path(PathBuf),
}

/// The toolchain of the build check of a tree (design 53.2): the toolchain that cargo uses for
/// the author in this tree. It is not the toolchain of the driver ([`toolchain_root`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckToolchain {
    /// The folder of the toolchain. Semilla starts `bin/cargo` of this folder by its full path.
    pub root: PathBuf,
    /// The name and its source, for a message: "`stable` (the default of rustup)".
    pub name: String,
}

/// True if a folder has the programs of a toolchain.
fn is_toolchain(path: &Path) -> bool {
    path.join("bin/cargo").is_file() && path.join("bin/rustc").is_file()
}

/// Finds the toolchain of the build check (design 53.2), in this order:
///
/// 1. the toolchain file of the tree (`tree`: what the file names, and the name of the file);
/// 2. the variable `RUSTUP_TOOLCHAIN`;
/// 3. the default toolchain of rustup (`default_toolchain` in `settings.toml` of the rustup home);
/// 4. the `cargo` of `PATH`, if it is not a program of rustup.
///
/// Semilla starts no program of rustup, so it never downloads a toolchain. A toolchain that is
/// not on the machine is an error with the command that installs it.
pub fn check_toolchain(tree: Option<(&TreeToolchain, &str)>) -> Result<CheckToolchain> {
    let variable = std::env::var("RUSTUP_TOOLCHAIN").ok().filter(|name| !name.is_empty());
    let home = rustup_home();
    let default = || {
        let text = std::fs::read_to_string(home.as_ref()?.join("settings.toml")).ok()?;
        let value = text.lines().find_map(|line| line.trim().strip_prefix("default_toolchain")?.trim().strip_prefix('=').map(|value| value.trim().trim_matches('"').to_string()))?;
        (!value.is_empty()).then_some(value)
    };
    let (name, source) = match tree {
        Some((TreeToolchain::Path(path), file)) => {
            if path.is_absolute() && is_toolchain(path) {
                return Ok(CheckToolchain { root: std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()), name: format!("`{}` (the file `{file}` of the tree)", path.display()) });
            }
            bail!("the file `{file}` of the tree names the toolchain folder `{}`. The build check can use only an absolute path of a folder with `bin/cargo` and `bin/rustc`", path.display());
        }
        Some((TreeToolchain::Channel(name), file)) => (name.clone(), format!("the file `{file}` of the tree")),
        None => match (variable, default()) {
            (Some(name), _) => (name, "the variable RUSTUP_TOOLCHAIN".to_string()),
            (None, Some(name)) => (name, "the default of rustup".to_string()),
            (None, None) => return path_toolchain(),
        },
    };
    // `RUSTUP_TOOLCHAIN` can be the path of a toolchain folder.
    if Path::new(&name).is_absolute() && is_toolchain(Path::new(&name)) {
        return Ok(CheckToolchain { root: std::fs::canonicalize(&name).unwrap_or_else(|_| PathBuf::from(&name)), name: format!("`{name}` ({source})") });
    }
    let Some(home) = home else { bail!("cannot find the rustup home for the toolchain `{name}` ({source}): set RUSTUP_HOME or HOME to an absolute path") };
    let folder = home.join("toolchains");
    // The folder has the full name, or the name and the host of this machine
    // (`stable-x86_64-unknown-linux-gnu`). The name `nightly` must not find `nightly-2026-01-01-…`.
    let mut found: Vec<PathBuf> = vec![folder.join(&name)].into_iter().filter(|path| is_toolchain(path)).collect();
    if found.is_empty() {
        let start = format!("{name}-{}-", std::env::consts::ARCH);
        found = std::fs::read_dir(&folder).into_iter().flatten().flatten().map(|entry| entry.path()).filter(|path| path.file_name().is_some_and(|file| file.to_string_lossy().starts_with(&start)) && is_toolchain(path)).collect();
        found.sort();
    }
    match found.as_slice() {
        [one] => Ok(CheckToolchain { root: std::fs::canonicalize(one).unwrap_or_else(|_| one.clone()), name: format!("`{name}` ({source})") }),
        [] => bail!("the toolchain `{name}` ({source}) is not on this machine: {} has no folder for it. The build check does not download a toolchain. Run `rustup toolchain install {name}`", folder.display()),
        _ => bail!("{} has more than one toolchain `{name}` ({source}): give the full name with the host", folder.display()),
    }
}

/// The toolchain of the `cargo` program of `PATH`, for a machine with no rustup default. A
/// program of rustup is not a toolchain: it can download one.
fn path_toolchain() -> Result<CheckToolchain> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let Some(cargo) = std::env::split_paths(&path).map(|folder| folder.join("cargo")).find(|file| file.is_file()) else {
        bail!("cannot find a toolchain: rustup has no default toolchain, and `PATH` has no `cargo`. Run `rustup default stable`");
    };
    let real = std::fs::canonicalize(&cargo).unwrap_or_else(|_| cargo.clone());
    let proxy = real.file_name().is_some_and(|name| name == "rustup") || cargo.with_file_name("rustup").is_file();
    let root = real.parent().and_then(Path::parent).filter(|root| !proxy && is_toolchain(root));
    match root {
        Some(root) => Ok(CheckToolchain { root: root.to_path_buf(), name: format!("`{}` (the `cargo` of `PATH`)", cargo.display()) }),
        None if proxy => bail!("cannot find a toolchain: `{}` is a program of rustup, and rustup has no default toolchain. Run `rustup default stable`", cargo.display()),
        None => bail!("cannot find a toolchain: `{}` has no `rustc` in its folder", cargo.display()),
    }
}

/// The key of a stored result of the build check (design 53.2): [`settings_key`] with no driver,
/// and the identity of the toolchain of the check (its folder, and the size and the time of its
/// compiler binary). So a different toolchain runs the check again.
pub fn check_settings_key(build_base: &Path, toolchain: &CheckToolchain) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    settings_key(build_base, None).hash(&mut hasher);
    let rustc = toolchain.root.join("bin/rustc");
    std::fs::canonicalize(&rustc).unwrap_or_else(|_| rustc.clone()).hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(&rustc) {
        meta.len().hash(&mut hasher);
        meta.modified().ok().hash(&mut hasher);
    }
    format!("{:012x}", hasher.finish() & 0xffff_ffff_ffff)
}

/// Each cargo configuration file outside the tree that a build in `build_base` reads: the files
/// `.cargo/config.toml` and `.cargo/config` of `build_base` and of each parent folder, and the
/// two files of the cargo home. Only the files that exist are in the list. The sandbox lets a
/// build read exactly these files, and [`settings_key`] has the content of each of them.
pub fn outer_configs(build_base: &Path) -> Vec<PathBuf> {
    let mut configs: Vec<PathBuf> = build_base.ancestors().flat_map(|dir| [dir.join(".cargo/config.toml"), dir.join(".cargo/config")]).collect();
    if let Some(home) = cargo_home() {
        configs.extend([home.join("config.toml"), home.join("config")]);
    }
    configs.retain(|file| file.is_file());
    configs.dedup();
    configs
}

/// The key of the build settings that are not in the tree: the cargo configuration files of the
/// parent folders of the build folder and of the cargo home, the path of the cargo home, the
/// toolchain (its name, its folder, and its compiler binary), the MIR driver binary (its path,
/// its size, and its time), and the state of the sandbox. A cached record is valid only for the
/// same key. So a record that a different driver or a different toolchain wrote is never read
/// (batch 5h), and a record of a build with a missing limit of the sandbox is never read by a
/// command with the full sandbox (batch 5v, part D).
pub fn settings_key(build_base: &Path, driver: Option<&Path>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for file in outer_configs(build_base) {
        if let Ok(text) = std::fs::read(&file) {
            file.hash(&mut hasher);
            text.hash(&mut hasher);
        }
    }
    toolchain().hash(&mut hasher);
    let file_state = |file: &Path, hasher: &mut std::collections::hash_map::DefaultHasher| {
        std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf()).hash(hasher);
        if let Ok(meta) = std::fs::metadata(file) {
            meta.len().hash(hasher);
            meta.modified().ok().hash(hasher);
        }
    };
    if let Some(driver) = driver {
        file_state(driver, &mut hasher);
    }
    // Batch 5v: the folders that the environment of the command selects, and the sandbox.
    match toolchain_root() {
        Ok(root) => file_state(&root.join("bin/rustc"), &mut hasher),
        Err(_) => "no toolchain".hash(&mut hasher),
    }
    cargo_home().hash(&mut hasher);
    crate::sandbox::missing().map(|missing| missing.key()).hash(&mut hasher);
    format!("{:012x}", hasher.finish() & 0xffff_ffff_ffff)
}

/// The root folder of the semilla workspace (where this crate was compiled).
pub fn semilla_workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Returns the `lib` folder of the toolchain sysroot. The driver needs it at run time.
pub fn sysroot_lib() -> Result<PathBuf> {
    Ok(toolchain_root()?.join("lib"))
}

/// The new environment of a command of a build (batch 5v, part A). The command starts from an
/// empty environment. No variable of the Semilla command goes to the build. The values:
///
/// - `PATH`: the `bin` folder of the toolchain, then `/usr/local/bin:/usr/bin:/bin`.
/// - `CARGO_HOME`: [`cargo_home`]. The path is part of [`settings_key`].
/// - `LD_LIBRARY_PATH`: the `lib` folder of the toolchain (the driver needs it).
/// - `HOME` and `TMPDIR`: `scratch`, a folder that the build can write.
pub fn build_environment(command: &mut Command, root: &Path, scratch: Option<&Path>) {
    command.env_clear();
    command.env("PATH", crate::sandbox::build_path(&root.join("bin")));
    command.env("LD_LIBRARY_PATH", root.join("lib"));
    if let Some(home) = cargo_home() {
        command.env("CARGO_HOME", home);
    }
    if let Some(scratch) = scratch {
        command.env("HOME", scratch).env("TMPDIR", scratch);
    }
}

/// The paths that each build command can read (batch 5v, part C): the build folder, the
/// toolchain folder, the registry and the git folders of the cargo home, and each cargo
/// configuration file outside the tree. The sandbox adds the system folders.
fn read_list(workspace: &Path, root: &Path) -> Vec<PathBuf> {
    let mut list = vec![workspace.to_path_buf(), root.to_path_buf()];
    if let Some(home) = cargo_home() {
        list.extend([home.join("registry"), home.join("git")]);
    }
    list.extend(outer_configs(workspace));
    list
}

/// The target triple of the check build: the host of the toolchain. The check build gives no
/// `--target` to cargo. The answer is empty if `rustc` does not run.
pub fn host_target() -> String {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        let Ok(root) = toolchain_root() else { return String::new() };
        let mut command = Command::new(root.join("bin/rustc"));
        build_environment(&mut command, &root, None);
        let output = command.arg("-vV").output();
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
    /// A list with no default features (`--no-default-features --features a,b`; batch 5u,
    /// part C). The list is sorted. Semilla adds this form for a proof that needs a state with
    /// a default feature off.
    NoDefaultListed(Vec<String>),
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
            Features::NoDefaultListed(list) => format!("{profile}, no default features, features {}", list.join(",")),
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
            Features::Listed(list) | Features::NoDefaultListed(list) => {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                list.hash(&mut hasher);
                let start = if matches!(self.features, Features::Listed(_)) { "" } else { "-nd" };
                format!("{start}-f{:012x}", hasher.finish() & 0xffff_ffff_ffff)
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
            Features::NoDefaultListed(list) => {
                args.push("--no-default-features".to_string());
                args.push("--features".to_string());
                args.push(list.join(","));
            }
        }
        args
    }
}

/// The features of a selection that no workspace package has, by the manifests (`cargo
/// metadata --no-deps`). Cargo stops with "does not contain this feature" for such a selection
/// (design 37.7). A feature is the name of an entry of the feature table, or the name of an
/// optional dependency. `package/feature` names a feature of one package.
pub fn missing_features(metadata: &serde_json::Value, features: &Features) -> Vec<String> {
    let (Features::Listed(list) | Features::NoDefaultListed(list)) = features else { return Vec::new() };
    let has = |package: &serde_json::Value, feature: &str| -> bool {
        let optional = |dependency: &serde_json::Value| dependency["optional"].as_bool() == Some(true) && dependency["rename"].as_str().or(dependency["name"].as_str()) == Some(feature);
        package["features"].as_object().is_some_and(|table| table.contains_key(feature)) || package["dependencies"].as_array().into_iter().flatten().any(optional)
    };
    let packages: Vec<&serde_json::Value> = metadata["packages"].as_array().into_iter().flatten().collect();
    let found = |feature: &String| match feature.split_once('/') {
        // `dependency/feature` is a feature of a dependency: the manifests of the workspace
        // do not have its table, so it is not in the result.
        Some((package, feature)) => packages.iter().filter(|found| found["name"].as_str() == Some(package)).all(|found| has(found, feature)),
        None => packages.iter().any(|package| has(package, feature)),
    };
    list.iter().filter(|feature| !found(feature)).cloned().collect()
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
            Features::Listed(list) | Features::NoDefaultListed(list) => {
                if matches!(features, Features::Listed(_)) {
                    todo.extend(default);
                }
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
    run_check_command(workspace, level, out, target_dir, &new_run(), Mode::Plain, &Configuration::DEV, false).map(|_| ())
}

/// [`run_cargo_check`] for the test of the trait table: the driver also writes the trait table
/// of each crate to `out`. The environment of a build is new (batch 5v, part A), so a test
/// cannot give `SEMILLA_TRAIT_TABLE` to the driver through its own environment.
pub fn run_cargo_check_with_trait_table(workspace: &Path, level: &str, out: &Path, target_dir: &Path) -> Result<()> {
    run_check_command(workspace, level, out, target_dir, &new_run(), Mode::Plain, &Configuration::DEV, true).map(|_| ())
}

/// A new value of `SEMILLA_MIR_RUN`: each build with a new value compiles each crate again.
fn new_run() -> String {
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|time| time.as_nanos()).unwrap_or(0);
    format!("{}-{time}", std::process::id())
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
    run_cargo_check_delta_config(workspace, level, out, target_dir, &Configuration::new(false, features)).map(|_| ())
}

/// The files of the records folder after a delta build (batch 5v, part F, rule 2).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Built {
    /// The record files of the units of this build: one file name for each unit that has a
    /// record file. Only these files are records of the tree.
    pub records: Vec<String>,
    /// The names of the files that are not a record of a unit of this build or of an earlier
    /// build of the same folder. No build of Semilla makes such a file. The files are removed.
    pub other: Vec<String>,
    /// [R78.1-2] The library files of the units of this build, from the messages of cargo: the
    /// intent crate (design 78) compiles against them.
    pub libraries: Vec<Library>,
}

/// [R78.1-2] One library file of a unit of a build (a `.rmeta` file of a library, or the file
/// of a proc-macro crate), from the messages of cargo.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Library {
    /// The crate name of the target (`-` becomes `_`).
    pub name: String,
    /// The file of the unit.
    pub file: PathBuf,
    /// True for a unit of a package of the workspace (the manifest is in the build folder).
    pub member: bool,
    /// True for a proc-macro crate.
    pub proc_macro: bool,
    /// The identity of the crate (`StableCrateId`) from its unit record: members only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub identity: String,
    /// The features of the unit (a package can have two library units with two feature sets).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

/// [R78.1-2] The library files of each unit of the messages of a build. A build script, a
/// binary, and a test unit have no library file.
pub fn unit_libraries(stdout: &str, workspace: &Path) -> Vec<Library> {
    let mut found = Vec::new();
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if message.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") || message["profile"]["test"].as_bool() == Some(true) {
            continue;
        }
        let target = &message["target"];
        let kinds: Vec<&str> = target["kind"].as_array().into_iter().flatten().filter_map(|kind| kind.as_str()).collect();
        let proc_macro = kinds.contains(&"proc-macro");
        if !proc_macro && !kinds.iter().any(|kind| matches!(*kind, "lib" | "rlib")) {
            continue;
        }
        let member = message["manifest_path"].as_str().is_some_and(|path| Path::new(path).starts_with(workspace));
        let name = target["name"].as_str().unwrap_or("").replace('-', "_");
        let features: Vec<String> = message["features"].as_array().into_iter().flatten().filter_map(|feature| feature.as_str().map(str::to_string)).collect();
        for file in message.get("filenames").and_then(|f| f.as_array()).into_iter().flatten().filter_map(|file| file.as_str()) {
            let wanted = if proc_macro { !file.ends_with(".rmeta") } else { file.ends_with(".rmeta") };
            if wanted {
                found.push(Library { name: name.clone(), file: PathBuf::from(file), member, proc_macro, identity: String::new(), features: features.clone() });
            }
        }
    }
    found.sort_by(|a, b| a.file.cmp(&b.file));
    found.dedup();
    found
}

/// The file with the names of the record files that Semilla knows from the builds of a records
/// folder. It is next to the records folder, in a folder that no build can write.
fn known_file(out: &Path) -> PathBuf {
    let mut name = out.file_name().map(|name| name.to_os_string()).unwrap_or_default();
    name.push(".known");
    out.with_file_name(name)
}

fn read_known(out: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_to_string(known_file(out)).map(|text| text.lines().map(str::to_string).collect()).unwrap_or_default()
}

/// [`run_cargo_check_delta`] for one build configuration. Use one build folder, one target
/// folder, and one records folder for each configuration.
///
/// The result names the record files of the units of this build. The driver writes one file
/// for each unit, with the name `<crate>.<kind>.<hash>.jsonl`. Semilla computes each name from
/// the unit messages of cargo. A file of the records folder with a different name is removed.
/// If no earlier build of the folder has a unit with that name, the result names the file: a
/// program of the build, and not the driver, made it.
pub fn run_cargo_check_delta_config(
    workspace: &Path,
    level: &str,
    out: &Path,
    target_dir: &Path,
    config: &Configuration,
) -> Result<Built> {
    let driver = find_driver()?;
    let fingerprint = driver_fingerprint(&driver)?;
    let mut known = read_known(out);
    let save = |known: &std::collections::BTreeSet<String>| std::fs::write(known_file(out), known.iter().map(|name| format!("{name}\n")).collect::<String>());
    let check = |known: &mut std::collections::BTreeSet<String>| match run_check_command(workspace, level, out, target_dir, &fingerprint, Mode::Json, config, false) {
        Ok(stdout) => Ok(stdout),
        Err(error) => {
            // The units that a failed build compiled have record files too.
            if let Some(failed) = error.downcast_ref::<FailedBuild>() {
                known.extend(unit_files(&failed.stdout));
                let _ = save(known);
            }
            Err(error)
        }
    };
    let mut stdout = check(&mut known)?;
    // A fresh unit of the workspace with no record file (design 37.11): a build with other
    // unit names (a different package version) removed the file, and then a tree came back
    // to the sources of the older build. Cargo does not compile the unit, so the driver
    // writes no records. The build folder goes away, and one more build makes each record.
    if fresh_without_records(&stdout, workspace, out) {
        match std::fs::remove_dir_all(target_dir) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        stdout = check(&mut known)?;
    }
    let units = unit_files(&stdout);
    let mut built = Built { libraries: unit_libraries(&stdout, &std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf())), ..Built::default() };
    for entry in std::fs::read_dir(out)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if units.contains(&name) && entry.file_type()?.is_file() {
            built.records.push(name);
            continue;
        }
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
        if !known.contains(&name) {
            built.other.push(name);
        }
    }
    // One unit gives one file. If two files have the name of one unit, a program of the build
    // made one of them. Semilla uses none of them.
    let (records, double) = one_file_each(std::mem::take(&mut built.records));
    built.records = records;
    for name in double {
        std::fs::remove_file(out.join(&name))?;
        built.other.push(name);
    }
    built.records.sort();
    built.other.sort();
    known.extend(units);
    save(&known)?;
    Ok(built)
}

/// [R78.1-2] The second check build of the old tree for the intent crate (option B of the user,
/// 2026-11-26): `cargo check --workspace` of the tree in its own target folder, in the sandbox
/// of a check build, with the driver as the wrapper of the members: each member in `names` gets
/// the `-C metadata` value `semilla-intent-before`, so rustc gives it a new crate identity (also
/// in its definition hashes and in the symbol of a proc-macro crate). No file is patched, and
/// the builds of the step stay as they are. The result is the messages of cargo.
pub fn intent_second_build(workspace: &Path, target_dir: &Path, names: &[String]) -> Result<String> {
    let driver = find_driver()?;
    let root = toolchain_root()?;
    std::fs::create_dir_all(target_dir)?;
    let target_dir = std::fs::canonicalize(target_dir)?;
    let temporary = target_dir.join("tmp");
    std::fs::create_dir_all(&temporary)?;
    let mut command = Command::new(root.join("bin/cargo"));
    command.args(["check", "--workspace", "--quiet", "--offline", "--message-format=json"]);
    command.arg("--target-dir").arg(&target_dir).current_dir(workspace);
    build_environment(&mut command, &root, Some(&temporary));
    command.env("RUSTC_WORKSPACE_WRAPPER", &driver).env("SEMILLA_INTENT_METADATA", "semilla-intent-before").env("SEMILLA_INTENT_RENAME", names.join(",")).env("RUSTC_ICE", "0");
    let workspace_real = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut read = read_list(&workspace_real, &root);
    read.push(driver.clone());
    let mut sandbox = crate::sandbox::Sandbox { folders: vec![target_dir.clone()], files: Vec::new(), read };
    let lock = workspace.join("Cargo.lock");
    if !lock.exists() && std::fs::write(&lock, "").is_ok() {
        sandbox.files.push(lock);
    }
    sandbox.apply(&mut command).map_err(|cause| anyhow::anyhow!("cannot start the sandbox for the second build: {cause}"))?;
    let output = crate::sandbox::run_and_stop(&mut command).context("cannot run cargo")?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        bail!("the second check build of the old tree failed in {}:\n{}", workspace.display(), error_text(&stdout, &String::from_utf8_lossy(&output.stderr)));
    }
    Ok(stdout)
}

/// A check build that cargo ran and that failed, with the standard output of cargo.
#[derive(Debug)]
struct FailedBuild {
    text: String,
    stdout: String,
}

impl std::fmt::Display for FailedBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for FailedBuild {}

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

/// The name of the record file of each unit in the JSON messages of cargo. The driver gives
/// the name `<crate>.<kind>.<hash>.jsonl` to the record file of a unit. The crate name is the
/// target name with `_` for each `-`. The kind is `test` for a test build, else the crate types.
/// The hash is the part after the last `-` of each artifact file name (`libname-<hash>.rmeta`).
///
/// A unit of a test build has two possible names. With a test harness, the compiler gets
/// `--test` and the kind is `test`. A target with `harness = false` gets no `--test`, and the
/// kind is the crate types. The messages of cargo do not say which case applies, so the result
/// has the two names. [`one_file_each`] accepts only one file for each unit.
///
/// A build script has no record file: the driver writes no record for it. The result has no
/// name for a target of the kind `custom-build`.
pub(crate) fn unit_files(stdout: &str) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if message.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let target = &message["target"];
        if target["kind"].as_array().into_iter().flatten().any(|kind| kind.as_str() == Some("custom-build")) {
            continue;
        }
        let crate_name = target["name"].as_str().unwrap_or("").replace('-', "_");
        let types = target["crate_types"].as_array().into_iter().flatten().filter_map(|kind| kind.as_str()).collect::<Vec<_>>().join("+");
        let test = message["profile"]["test"].as_bool() == Some(true);
        for file in message.get("filenames").and_then(|f| f.as_array()).into_iter().flatten() {
            let Some(file) = file.as_str() else { continue };
            let name = Path::new(file).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if let Some((_, hash)) = name.rsplit_once('-') {
                names.insert(format!("{crate_name}.{types}.{hash}.jsonl"));
                if test {
                    names.insert(format!("{crate_name}.test.{hash}.jsonl"));
                }
            }
        }
    }
    names
}

/// True if cargo did not compile a unit of the workspace (the unit is "fresh") and the records
/// folder has no file of the unit. A build script has no record file and does not count. A unit
/// of a package outside the build folder (a crate of the registry) has no records.
pub(crate) fn fresh_without_records(stdout: &str, workspace: &Path, out: &Path) -> bool {
    stdout.lines().any(|line| {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { return false };
        if message.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") || message["fresh"].as_bool() != Some(true) {
            return false;
        }
        let member = message["manifest_path"].as_str().is_some_and(|path| Path::new(path).starts_with(workspace));
        let names = unit_files(line);
        member && !names.is_empty() && !names.iter().any(|name| out.join(name).is_file())
    })
}

/// Divides the record file names of a build: the names of the units with one file, and the
/// names of the units with more than one file. The crate name and the hash of a file name
/// (`<crate>.<kind>.<hash>.jsonl`) identify the unit.
pub(crate) fn one_file_each(names: Vec<String>) -> (Vec<String>, Vec<String>) {
    let unit = |name: &str| {
        let mut parts = name.split('.');
        (parts.next().unwrap_or("").to_string(), parts.nth(1).unwrap_or("").to_string())
    };
    let mut count = std::collections::BTreeMap::new();
    for name in &names {
        *count.entry(unit(name)).or_insert(0usize) += 1;
    }
    names.into_iter().partition(|name| count[&unit(name)] == 1)
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
    let result = metadata_command(scratch, Graph::Full, &crate::sandbox::Sandbox { folders: Vec::new(), files: vec![lock], read: Vec::new() });
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
    metadata_command_with(&toolchain_root()?, workspace, graph, sandbox)
}

/// As [`metadata_command`], with the cargo of the toolchain folder `root`.
fn metadata_command_with(root: &Path, workspace: &Path, graph: Graph, sandbox: &crate::sandbox::Sandbox) -> Result<serde_json::Value> {
    let mut command = Command::new(root.join("bin/cargo"));
    command.arg("metadata");
    match graph {
        Graph::NoDeps => drop(command.arg("--no-deps")),
        Graph::Full => {}
        Graph::Locked => drop(command.arg("--locked")),
    }
    command.args(["--offline", "--format-version", "1"]).current_dir(workspace);
    // A new environment (batch 5v, part A): no variable of the Semilla command goes to cargo.
    build_environment(&mut command, root, None);
    let workspace_real = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let sandbox = crate::sandbox::Sandbox { folders: sandbox.folders.clone(), files: sandbox.files.clone(), read: read_list(&workspace_real, root) };
    sandbox.apply(&mut command).map_err(|cause| anyhow::anyhow!("cannot start the sandbox for `cargo metadata`: {cause}"))?;
    let output = crate::sandbox::run_and_stop(&mut command).context("cannot run cargo")?;
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

/// One error of the compiler in a build check: the target of cargo, and the text.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BuildError {
    /// The package ID of the target, as cargo writes it.
    pub package: String,
    /// The kinds of the target (`lib`, `bin`, `test`, `example`, `bench`, `custom-build`).
    pub kinds: Vec<String>,
    /// The name of the target.
    pub name: String,
    /// The text of the error, with the file and the line.
    pub text: String,
}

impl BuildError {
    /// True if the error can be an error of the build unit `unit`, in the words of cargo: for
    /// example `` `demo` (lib test) `` or `` `demo` (test "api") ``.
    pub fn in_unit(&self, unit: &str) -> bool {
        let Some((package, rest)) = unit.strip_prefix('`').and_then(|rest| rest.split_once("` (")) else { return false };
        if !(self.package.contains(&format!("/{package}#")) || self.package.contains(&format!("#{package}@")) || self.package.starts_with(&format!("{package} "))) {
            return false;
        }
        let rest = rest.trim_end_matches(')');
        let kind = rest.split(' ').next().unwrap_or_default();
        match rest.split('"').nth(1) {
            Some(name) => self.name == name && self.kinds.iter().any(|found| found == kind),
            None if kind == "build" => self.kinds.iter().any(|found| found == "custom-build"),
            None => !self.kinds.iter().any(|found| ["bin", "test", "example", "bench", "custom-build"].contains(&found.as_str())),
        }
    }
}

/// The result of the build check of one tree (design 53).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BuildRun {
    /// The build units that do not build, in the words of cargo: `` `demo` (lib test) ``. The
    /// list is empty if the tree builds.
    pub failed: Vec<String>,
    /// The errors of the compiler.
    pub errors: Vec<BuildError>,
    /// The `error` lines of cargo that name no build unit.
    pub other: String,
    /// The build units that cargo built in a run with a failure (the `compiler-artifact`
    /// messages), in the words of cargo. Empty if the tree builds.
    #[serde(default)]
    pub built: Vec<String>,
    /// Each build unit of the target list of the tree (`cargo metadata`), for a run with a
    /// failure. Empty if the tree builds, or if cargo cannot read the manifests.
    #[serde(default)]
    pub targets: Vec<String>,
}

/// The state of a build unit in the build check of one tree (design 53.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitState {
    /// Cargo built the unit. For a tree that builds, this is the state of each unit: a unit
    /// that the tree does not have is a new unit, with the same effect.
    Builds,
    /// The unit does not build.
    Fails,
    /// The tree has the unit, but cargo did not check it: a unit that it needs does not build.
    NotChecked,
    /// The tree does not have the unit.
    Absent,
}

impl BuildRun {
    /// The state of a build unit in this run. The facts are the failed list, the
    /// `compiler-artifact` messages, and the target list of the tree. If cargo started no build
    /// (a manifest that it cannot read), no unit of the tree was checked.
    pub fn state(&self, unit: &str) -> UnitState {
        let has = |list: &[String]| list.iter().any(|known| known == unit);
        if has(&self.failed) {
            UnitState::Fails
        } else if self.failed.is_empty() || has(&self.built) {
            UnitState::Builds
        } else if has(&self.targets) || self.failed.iter().any(|failed| failed == NO_UNIT) {
            UnitState::NotChecked
        } else {
            UnitState::Absent
        }
    }
}

/// The name of a build unit in the words of cargo, from the package name, the kinds and the
/// name of the target, and the test mode: `` `demo` (lib test) ``, `` `demo` (test "api") ``.
fn unit_name(package: &str, kinds: &[&str], name: &str, test: bool) -> String {
    let kind = kinds.iter().copied().find(|kind| ["bin", "test", "bench", "example", "custom-build"].contains(kind)).unwrap_or("lib");
    let mode = if test && !["test", "bench", "custom-build"].contains(&kind) { " test" } else { "" };
    match kind {
        "lib" => format!("`{package}` (lib{mode})"),
        "custom-build" => format!("`{package}` (build script)"),
        _ => format!("`{package}` ({kind} \"{name}\"{mode})"),
    }
}

/// The name of the unit of a build script that runs.
fn script_run(package: &str) -> String {
    format!("`{package}` (build script run)")
}

/// The package name in a package ID of cargo: `path+file:///work/demo#0.1.0`,
/// `registry+https://…#serde@1.0.0`, or the old form `demo 0.1.0 (path+file:///work/demo)`.
fn package_name(id: &str) -> String {
    if let Some((name, _)) = id.split_once(' ') {
        return name.to_string();
    }
    let (source, tail) = id.rsplit_once('#').unwrap_or((id, ""));
    match tail.split_once('@') {
        Some((name, _)) => name.to_string(),
        None => source.rsplit('/').next().unwrap_or(source).to_string(),
    }
}

/// The build units that cargo built: the `compiler-artifact` messages, and the build scripts
/// that ran (`build-script-executed`). `names` has the package name of each package ID of the
/// workspace.
fn built_units(stdout: &str, names: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    let mut units = std::collections::BTreeSet::new();
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(id) = message.get("package_id").and_then(|id| id.as_str()) else { continue };
        let package = names.get(id).cloned().unwrap_or_else(|| package_name(id));
        match message.get("reason").and_then(|reason| reason.as_str()) {
            Some("compiler-artifact") => {
                let Some(target) = message.get("target") else { continue };
                let kinds: Vec<&str> = target.get("kind").and_then(|kinds| kinds.as_array()).map(|kinds| kinds.iter().filter_map(|kind| kind.as_str()).collect()).unwrap_or_default();
                let name = target.get("name").and_then(|name| name.as_str()).unwrap_or_default();
                let test = message.get("profile").and_then(|profile| profile.get("test")).and_then(|test| test.as_bool()).unwrap_or(false);
                units.insert(unit_name(&package, &kinds, name, test));
            }
            Some("build-script-executed") => drop(units.insert(script_run(&package))),
            _ => {}
        }
    }
    units.into_iter().collect()
}

/// The package names by package ID, and each build unit that `cargo check --all-targets` has
/// for the target list of a workspace (the output of `cargo metadata --no-deps`).
fn target_units(metadata: &serde_json::Value) -> (std::collections::BTreeMap<String, String>, Vec<String>) {
    let mut names = std::collections::BTreeMap::new();
    let mut units = std::collections::BTreeSet::new();
    for package in metadata.get("packages").and_then(|packages| packages.as_array()).into_iter().flatten() {
        let Some(package_name) = package.get("name").and_then(|name| name.as_str()) else { continue };
        if let Some(id) = package.get("id").and_then(|id| id.as_str()) {
            names.insert(id.to_string(), package_name.to_string());
        }
        for target in package.get("targets").and_then(|targets| targets.as_array()).into_iter().flatten() {
            let kinds: Vec<&str> = target.get("kind").and_then(|kinds| kinds.as_array()).map(|kinds| kinds.iter().filter_map(|kind| kind.as_str()).collect()).unwrap_or_default();
            let name = target.get("name").and_then(|name| name.as_str()).unwrap_or_default();
            units.insert(unit_name(package_name, &kinds, name, false));
            // A target with the test mode has a second unit. A build script has the unit of its run.
            if target.get("test").and_then(|test| test.as_bool()).unwrap_or(false) {
                units.insert(unit_name(package_name, &kinds, name, true));
            }
            if kinds.contains(&"custom-build") {
                units.insert(script_run(package_name));
            }
        }
    }
    (names, units.into_iter().collect())
}

/// The name of the failed unit of a tree that cargo cannot start to build (design 53): for
/// example a manifest that cargo cannot read.
pub const NO_UNIT: &str = "`workspace` (cargo starts no build)";

/// The build units of the lines `error: could not compile ...` of cargo, and each package
/// with a build script that fails when it runs.
fn failed_units(stderr: &str) -> Vec<String> {
    let mut units: Vec<String> = stderr
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("error: could not compile "))
        .filter_map(|rest| rest.find(") ").map(|end| rest[..=end].to_string()))
        .collect();
    let scripts = stderr.lines().filter_map(|line| line.trim_start().strip_prefix("error: failed to run custom build command for `"));
    units.extend(scripts.filter_map(|rest| rest.split([' ', '`']).next()).map(script_run));
    units.sort();
    units.dedup();
    units
}

/// The errors of the compiler in the JSON messages of cargo. An error text shows one time.
fn build_errors(stdout: &str) -> Vec<BuildError> {
    let mut errors: Vec<BuildError> = Vec::new();
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if message.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let Some(diagnostic) = message.get("message") else { continue };
        if diagnostic.get("level").and_then(|l| l.as_str()).is_none_or(|level| !level.starts_with("error")) {
            continue;
        }
        let Some(text) = diagnostic.get("rendered").and_then(|r| r.as_str()).or_else(|| diagnostic.get("message").and_then(|m| m.as_str())) else { continue };
        let text = text.trim_end();
        // The summary "aborting due to N previous errors" is not a cause.
        if text.contains("aborting due to") {
            continue;
        }
        let target = message.get("target");
        let word = |key: &str| target.and_then(|target| target.get(key)).and_then(|value| value.as_str()).unwrap_or_default().to_string();
        let kinds = target.and_then(|target| target.get("kind")).and_then(|kinds| kinds.as_array()).map(|kinds| kinds.iter().filter_map(|kind| kind.as_str().map(str::to_string)).collect()).unwrap_or_default();
        let error = BuildError { package: message.get("package_id").and_then(|id| id.as_str()).unwrap_or_default().to_string(), kinds, name: word("name"), text: text.to_string() };
        // A library and its test build give the same error two times.
        if !errors.iter().any(|known| known.text == error.text && known.package == error.package && known.kinds == error.kinds && known.name == error.name) {
            errors.push(error);
        }
    }
    errors
}

/// Runs the build check of a tree (design 53): `cargo check` for each target of the workspace,
/// with the toolchain of the tree ([`check_toolchain`]), and with the new environment and the
/// sandbox of a check build. The MIR driver does not run. `--keep-going` gives each build unit
/// that does not build.
///
/// `Ok` is a result of the code: the units that do not build, and their errors. `Err` is a
/// check that cannot run: no sandbox, a dependency that is not in the cargo cache, or a disk
/// with no space.
pub fn build_check(workspace: &Path, target_dir: &Path, toolchain: &CheckToolchain) -> Result<BuildRun> {
    build_check_with(workspace, target_dir, toolchain, None).map(|(run, _)| run)
}

/// The expansion records of a build check (R65.8-1): the driver of Semilla is the wrapper of the
/// compiler of the build, at the level `ex`. It writes the unit record of each crate of the
/// workspace with the expansion of each macro call, and it changes no flag and no query of the
/// compiler. So the build is the build of a plain `cargo check`.
pub struct CheckRecords {
    /// The driver binary.
    pub driver: PathBuf,
    /// The records folder of the build folder: it keeps the record file of each fresh unit, so
    /// it changes only by the units that cargo compiles again (a delta build).
    pub out: PathBuf,
}

/// The record files of a build check with [`CheckRecords`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CheckRecordFiles {
    /// The record file of each unit of the workspace that the build has (the file names in the
    /// records folder).
    Files(Vec<String>),
    /// A fresh unit of the workspace has no record file: an earlier build of the folder did not
    /// write it. The caller removes the target folder and builds again.
    FreshWithout,
    /// The build has a unit that does not build: its record is missing.
    #[default]
    Failed,
}

/// True if the driver of Semilla can be the wrapper of the build check of a toolchain: it is the
/// toolchain of the driver (the driver links to the compiler crates of that toolchain).
pub fn driver_toolchain(toolchain: &CheckToolchain) -> bool {
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    toolchain_root().is_ok_and(|root| canonical(&root) == canonical(&toolchain.root))
}

/// The fingerprint of the driver binary (its size and its time): the value of
/// `SEMILLA_MIR_RUN`, so a new driver compiles each crate of the workspace again.
pub fn driver_fingerprint(driver: &Path) -> Result<String> {
    let meta = std::fs::metadata(driver)?;
    let modified = meta.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    Ok(format!("driver-{}-{modified}", meta.len()))
}

/// [`build_check`] that can also write the expansion records (`records`, R65.8-1). The second
/// value is `None` with no `records`.
pub fn build_check_with(workspace: &Path, target_dir: &Path, toolchain: &CheckToolchain, records: Option<&CheckRecords>) -> Result<(BuildRun, Option<CheckRecordFiles>)> {
    let root = toolchain.root.clone();
    std::fs::create_dir_all(target_dir).with_context(|| format!("cannot create {}", target_dir.display()))?;
    let target_dir = std::fs::canonicalize(target_dir)?;
    let temporary = target_dir.join("tmp");
    std::fs::create_dir_all(&temporary).with_context(|| format!("cannot create {}", temporary.display()))?;
    let mut command = Command::new(root.join("bin/cargo"));
    command.args(["check", "--workspace", "--quiet", "--offline", "--all-targets", "--keep-going", "--message-format=json"]);
    command.arg("--target-dir").arg(&target_dir).current_dir(workspace);
    build_environment(&mut command, &root, Some(&temporary));
    // The sandbox of a check build (design 27): the build writes only its target folder (and the
    // records folder of the driver).
    let workspace_real = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut sandbox = crate::sandbox::Sandbox { folders: vec![target_dir.clone()], files: Vec::new(), read: read_list(&workspace_real, &root) };
    let records_out = match records {
        Some(records) => {
            std::fs::create_dir_all(&records.out).with_context(|| format!("cannot create {}", records.out.display()))?;
            let out = std::fs::canonicalize(&records.out)?;
            command.env("RUSTC_WORKSPACE_WRAPPER", &records.driver).env("SEMILLA_MIR_LEVEL", "ex").env("SEMILLA_MIR_OUT", &out).env("SEMILLA_MIR_RUN", driver_fingerprint(&records.driver)?);
            sandbox.folders.push(out.clone());
            sandbox.read.push(records.driver.clone());
            Some(out)
        }
        None => None,
    };
    // A tree with no lock file: cargo must write one, as in a check build of phase 2.
    let lock = workspace.join("Cargo.lock");
    if !lock.exists() && std::fs::write(&lock, "").is_ok() {
        sandbox.files.push(lock);
    }
    sandbox.apply(&mut command).map_err(|cause| anyhow::anyhow!("cannot start the sandbox for the build check: {cause}"))?;
    let output = crate::sandbox::run_and_stop(&mut command).with_context(|| format!("cannot run {}", root.join("bin/cargo").display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        let files = match &records_out {
            Some(out) => Some(record_files(&stdout, &workspace_real, out)?),
            None => None,
        };
        return Ok((BuildRun::default(), files));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let errors = build_errors(&stdout);
    let all = format!("{stderr}\n{}", errors.iter().map(|error| error.text.as_str()).collect::<Vec<_>>().join("\n"));
    // The causes that are not a defect of the code of the tree.
    if missing_dependency(&all) {
        bail!("a dependency is not in the cargo cache of this machine, and the build check does not download (cargo runs with `--offline`). Run `cargo fetch` in the repository.\n{}", first_errors(&stderr));
    }
    if all.contains("No space left on device") {
        bail!("the disk has no space for the build folder {}.\n{}", target_dir.display(), first_errors(&stderr));
    }
    let mut failed = failed_units(&stderr);
    // A cargo before version 1.74 does not have the option.
    if failed.is_empty() && errors.is_empty() && stderr.contains("--keep-going") {
        bail!("the cargo of the toolchain {} does not have the option `--keep-going` (cargo 1.74 or later has it).\n{}", toolchain.name, first_errors(&stderr));
    }
    if failed.is_empty() && errors.is_empty() && all.contains("Permission denied") {
        bail!("the sandbox of the build stops a read or a write (the build writes only {}).\n{}", target_dir.display(), first_errors(&stderr));
    }
    if failed.is_empty() {
        failed.push(NO_UNIT.to_string());
    }
    let other: Vec<&str> = stderr.lines().filter(|line| !line.trim_start().starts_with("error: could not compile ")).collect();
    let mut other = if other.iter().any(|line| line.starts_with("error")) { first_errors(&other.join("\n")).trim_end().to_string() } else { String::new() };
    // A file outside the tree that the sandbox does not let the build read.
    if let Some(outside) = outside_files(&all) {
        other = format!("{other}\n{outside}").trim_start().to_string();
    }
    // The facts for the state of each unit (design 53.4): the units that cargo built, and the
    // target list of the tree. `cargo metadata --no-deps` starts no compiler and runs no code.
    let (names, targets) = metadata_command_with(&root, workspace, Graph::NoDeps, &crate::sandbox::Sandbox::default()).map(|metadata| target_units(&metadata)).unwrap_or_default();
    let built = built_units(&stdout, &names);
    let files = records_out.map(|_| CheckRecordFiles::Failed);
    Ok((BuildRun { failed, errors, other, built, targets }, files))
}

/// The record files of the units of a build check that passes (R65.8-1). A file of the records
/// folder that is not a file of a unit of this build is removed: the folder keeps only the
/// records of the state of the build folder.
fn record_files(stdout: &str, workspace: &Path, out: &Path) -> Result<CheckRecordFiles> {
    if fresh_without_records(stdout, workspace, out) {
        return Ok(CheckRecordFiles::FreshWithout);
    }
    let units = unit_files(stdout);
    let mut found = Vec::new();
    for entry in std::fs::read_dir(out)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if units.contains(&name) && entry.file_type()?.is_file() {
            found.push(name);
        } else if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    // One unit gives one file; a second file of one unit is not a file of the driver.
    let (mut found, double) = one_file_each(found);
    for name in double {
        std::fs::remove_file(out.join(&name))?;
    }
    found.sort();
    Ok(CheckRecordFiles::Files(found))
}

/// The output mode of the cargo command of a check build.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Plain,
    /// `--message-format=json`.
    Json,
}

/// The text for a file that a build cannot read (batch 5v, part C): the paths of the error
/// lines `couldn't read `<path>`: Permission denied`, and the rule.
fn outside_files(text: &str) -> Option<String> {
    let mut files: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("Permission denied"))
        .filter_map(|line| line.split_once("couldn't read `").or_else(|| line.split_once("couldn't read ")).map(|(_, rest)| rest))
        .filter_map(|rest| rest.split_once("`: ").or_else(|| rest.split_once(": ")).map(|(path, _)| path))
        .collect();
    files.sort();
    files.dedup();
    if files.is_empty() {
        return None;
    }
    let list = files.iter().map(|file| format!("`{file}`")).collect::<Vec<_>>().join(", ");
    Some(format!("The file {list} is outside the build folder. A check build reads only the files of the tree, the toolchain, and the cargo registry. Put the file in the tree, and name it with a path relative to the source file."))
}

/// Runs the cargo command of a check build, and returns its standard output.
///
/// The command runs with a new environment (part A of batch 5v) and in the sandbox (design 27).
/// The text of an internal compiler error in the output of a build.
const ICE_TEXT: &str = "internal compiler error";

/// Runs the check build with the driver. If the driver stops the compiler with an internal
/// compiler error, the build runs one more time with no forms of the equivalence check
/// (`SEMILLA_MIR_FORMS=off`): an extraction of Semilla never changes the result of a build
/// (design 61.9, part 0). Each body of a crate that the second run builds then has the mark
/// "no form: …". If the second build fails too, the error of the second build is the result.
#[allow(clippy::too_many_arguments)]
fn run_check_command(
    workspace: &Path,
    level: &str,
    out: &Path,
    target_dir: &Path,
    nonce: &str,
    mode: Mode,
    config: &Configuration,
    trait_table: bool,
) -> Result<String> {
    match run_check_once(workspace, level, out, target_dir, nonce, mode, config, trait_table, true) {
        Err(error) if level == "n0" && error.downcast_ref::<FailedBuild>().is_some_and(|failed| failed.text.contains(ICE_TEXT) || failed.stdout.contains(ICE_TEXT)) => {
            run_check_once(workspace, level, out, target_dir, nonce, mode, config, trait_table, false)
        }
        other => other,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_check_once(
    workspace: &Path,
    level: &str,
    out: &Path,
    target_dir: &Path,
    nonce: &str,
    mode: Mode,
    config: &Configuration,
    trait_table: bool,
    forms: bool,
) -> Result<String> {
    if !matches!(level, "n0" | "n2" | "n3") {
        bail!("unknown level {level:?}: use n0, n2, or n3");
    }
    let driver = find_driver()?;
    let root = toolchain_root()?;
    std::fs::create_dir_all(out).with_context(|| format!("cannot create {}", out.display()))?;
    let out = std::fs::canonicalize(out)?;
    std::fs::create_dir_all(target_dir).with_context(|| format!("cannot create {}", target_dir.display()))?;
    let target_dir = std::fs::canonicalize(target_dir)?;
    // The temporary files of the build go to the target folder. `HOME` is the same folder: a
    // program of the build that looks for a file of the user finds an empty folder.
    let temporary = target_dir.join("tmp");
    std::fs::create_dir_all(&temporary)?;
    let mut command = Command::new(root.join("bin/cargo"));
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
    command.arg("--target-dir").arg(&target_dir).current_dir(workspace);
    // A new environment: no variable of the Semilla command goes to the build. So the records
    // of a tree depend only on the tree and on the settings in `settings_key`.
    build_environment(&mut command, &root, Some(&temporary));
    command.env("RUSTC_WORKSPACE_WRAPPER", &driver).env("SEMILLA_MIR_LEVEL", level).env("SEMILLA_MIR_OUT", &out).env("SEMILLA_MIR_RUN", nonce);
    // No file of an internal compiler error in the tree.
    command.env("RUSTC_ICE", "0");
    // [R75.2-1] The switch of the test of a model defect (design 75) goes to the driver.
    if let Ok(rule) = std::env::var("SEMILLA_TEST_RULE_OFF") {
        command.env("SEMILLA_TEST_RULE_OFF", rule);
    }
    if !forms {
        command.env("SEMILLA_MIR_FORMS", "off");
    }
    if trait_table {
        command.env("SEMILLA_TRAIT_TABLE", &out);
    }
    // The sandbox (design 27). A build script and a proc macro of the tree run in this build.
    // They write only the cargo target folder and the records folder. They read only these
    // folders, the build folder, the toolchain, the cargo registry, the system folders, and the
    // driver. They make no socket.
    let workspace_real = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut read = read_list(&workspace_real, &root);
    read.push(driver.clone());
    let mut sandbox = crate::sandbox::Sandbox { folders: vec![target_dir.clone(), out.clone()], files: Vec::new(), read };
    // A tree with no lock file: cargo must write one. Semilla makes an empty file, and the build
    // can write this one file. The next build of this folder starts from the files of its tree.
    let lock = workspace.join("Cargo.lock");
    if !lock.exists() && std::fs::write(&lock, "").is_ok() {
        sandbox.files.push(lock);
    }
    sandbox.apply(&mut command).map_err(|cause| anyhow::anyhow!("cannot start the sandbox for the check build: {cause}"))?;
    // The build runs to its end. Then Semilla stops each process that the build left.
    let output = crate::sandbox::run_and_stop(&mut command).context("cannot run cargo")?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let text = error_text(&stdout, &String::from_utf8_lossy(&output.stderr));
        // A dependency that cargo cannot get is not a defect of the code of the tree.
        if missing_dependency(&text) {
            bail!("cargo cannot get a dependency for the check build in {} (level {level}, configuration `{}`):\n{text}\n\n{MISSING_DEPENDENCY}", workspace.display(), config.name());
        }
        // A read or a write that the sandbox stops shows as "Permission denied".
        let limits = crate::sandbox::missing().map(|missing| missing.limits).unwrap_or_default();
        let hint = if limits.contains(&crate::sandbox::Limit::Write) || !text.contains("Permission denied") {
            String::new()
        } else if let Some(outside) = outside_files(&text) {
            format!("\n\n{outside}")
        } else {
            "\n\nThe check build runs in a sandbox: it writes only its build folder, it reads only the tree, the toolchain, and the cargo registry, and it does not download. Possible causes: a dependency is not in the cargo cache of this machine (run `cargo fetch` in the repository); `Cargo.lock` does not agree with a manifest (update it, and put it in the step); a build script or a proc macro writes a file outside `OUT_DIR`, or reads a file outside the tree.".to_string()
        };
        let targets = if config.narrow { "the body check builds the libraries and the binaries" } else { "the body check builds all targets, so the test code must compile too" };
        let text = format!("cargo check failed in {} (level {level}, configuration `{}`); {targets}:\n{text}{hint}", workspace.display(), config.name());
        return Err(anyhow::Error::new(FailedBuild { text, stdout }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod build_check_tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn a_unit_has_the_name_that_cargo_gives() {
        assert_eq!(unit_name("shop", &["lib"], "shop", false), "`shop` (lib)");
        assert_eq!(unit_name("shop", &["rlib", "cdylib"], "shop", true), "`shop` (lib test)");
        assert_eq!(unit_name("shop", &["proc-macro"], "shop", false), "`shop` (lib)");
        assert_eq!(unit_name("shop", &["bin"], "tool", false), "`shop` (bin \"tool\")");
        assert_eq!(unit_name("shop", &["bin"], "tool", true), "`shop` (bin \"tool\" test)");
        assert_eq!(unit_name("shop", &["test"], "api", true), "`shop` (test \"api\")");
        assert_eq!(unit_name("shop", &["bench"], "speed", true), "`shop` (bench \"speed\")");
        assert_eq!(unit_name("shop", &["example"], "demo", false), "`shop` (example \"demo\")");
        assert_eq!(unit_name("shop", &["custom-build"], "build-script-build", false), "`shop` (build script)");
        assert_eq!(package_name("path+file:///work/shop#0.1.0"), "shop");
        assert_eq!(package_name("path+file:///work/dir#shop@0.1.0"), "shop");
        assert_eq!(package_name("registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0"), "serde");
        assert_eq!(package_name("shop 0.1.0 (path+file:///work/shop)"), "shop");
    }

    // rule+ R53.4-1 R53.4-2 R53.4-3 R53.4-4 R53.4-9
    // rule- R53.4-1 R53.4-2 R53.4-3 R53.4-4 R53.4-9
    #[test]
    fn the_state_of_a_unit_comes_from_the_facts_of_the_cargo_run() {
        // The base tree of design 53.4: the library does not build, so cargo does not check
        // the integration test. The build script builds and runs.
        let id = "path+file:///work/dir#0.1.0";
        let metadata = serde_json::json!({"packages": [{"id": id, "name": "shop", "targets": [
            {"kind": ["lib"], "name": "shop", "test": true},
            {"kind": ["test"], "name": "api", "test": true},
            {"kind": ["example"], "name": "demo", "test": false},
            {"kind": ["custom-build"], "name": "build-script-build", "test": false},
        ]}]});
        let (names, targets) = target_units(&metadata);
        assert_eq!(targets, words(&["`shop` (build script run)", "`shop` (build script)", "`shop` (example \"demo\")", "`shop` (lib test)", "`shop` (lib)", "`shop` (test \"api\")"]));
        let artifact = |kind: &str, name: &str, test: bool| serde_json::json!({"reason": "compiler-artifact", "package_id": id, "target": {"kind": [kind], "name": name}, "profile": {"test": test}}).to_string();
        let stdout = [artifact("custom-build", "build-script-build", false), serde_json::json!({"reason": "build-script-executed", "package_id": id}).to_string(), serde_json::json!({"reason": "build-finished", "success": false}).to_string()].join("\n");
        // The package name comes from the target list: the folder name `dir` is not the name.
        let built = built_units(&stdout, &names);
        assert_eq!(built, words(&["`shop` (build script run)", "`shop` (build script)"]));
        let run = BuildRun { failed: words(&["`shop` (lib test)", "`shop` (lib)"]), built, targets, ..Default::default() };
        assert_eq!(run.state("`shop` (lib)"), UnitState::Fails);
        // The failed list comes from the lines "could not compile" of cargo (R53.4-1).
        assert_eq!(failed_units("error: could not compile `shop` (lib) due to 1 previous error\nerror: could not compile `shop` (lib test) due to 2 previous errors\n"), run.failed);
        // Negative: an error line of a different kind and a warning line name no target.
        assert_eq!(failed_units("error: expected one of `!` or `::`, found `x`\nwarning: `shop` (lib) generated 1 warning\n"), Vec::<String>::new());
        assert_eq!(run.state("`shop` (build script)"), UnitState::Builds);
        assert_eq!(run.state("`shop` (test \"api\")"), UnitState::NotChecked);
        assert_eq!(run.state("`shop` (example \"demo\")"), UnitState::NotChecked);
        // The tree does not have this target: a step that adds it makes its error.
        assert_eq!(run.state("`shop` (test \"new\")"), UnitState::Absent);
        // A tree that builds: each unit builds.
        assert_eq!(BuildRun::default().state("`shop` (test \"api\")"), UnitState::Builds);
        // Cargo started no build: no unit of the tree was checked.
        let no_build = BuildRun { failed: words(&[NO_UNIT]), ..Default::default() };
        assert_eq!(no_build.state("`shop` (lib)"), UnitState::NotChecked);
        assert_eq!(no_build.state(NO_UNIT), UnitState::Fails);
        // A stored result of batch 9a4 has no such lists: the state is not `NotChecked`.
        let old: BuildRun = serde_json::from_str(r#"{"failed":["`shop` (lib)"],"errors":[],"other":""}"#).unwrap();
        assert_eq!(old.state("`shop` (test \"api\")"), UnitState::Absent);
    }

    /// A folder with the two programs of a toolchain. `rustc` has the given content.
    fn toolchain_folder(root: &Path, rustc: &str) {
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/cargo"), "").unwrap();
        std::fs::write(root.join("bin/rustc"), rustc).unwrap();
    }

    #[test]
    fn the_key_of_a_stored_result_has_the_identity_of_the_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("one"), dir.path().join("two"));
        toolchain_folder(&one, "a");
        toolchain_folder(&two, "a");
        let key = |root: &Path| check_settings_key(dir.path(), &CheckToolchain { root: root.to_path_buf(), name: String::new() });
        let first = key(&one);
        assert_eq!(first, key(&one));
        // A different folder is a different toolchain.
        assert_ne!(first, key(&two));
        // A new compiler binary in the same folder (an update of `stable`) is a different toolchain.
        std::fs::write(one.join("bin/rustc"), "a longer file").unwrap();
        assert_ne!(first, key(&one));
    }

    #[test]
    fn a_toolchain_path_of_the_tree_must_be_an_absolute_toolchain_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("custom");
        toolchain_folder(&root, "a");
        let found = check_toolchain(Some((&TreeToolchain::Path(root.clone()), "rust-toolchain.toml"))).unwrap();
        assert_eq!(found.root, std::fs::canonicalize(&root).unwrap());
        assert!(found.name.contains("the file `rust-toolchain.toml` of the tree"), "{}", found.name);
        for path in [PathBuf::from("custom"), dir.path().join("no-such-folder")] {
            let error = check_toolchain(Some((&TreeToolchain::Path(path), "rust-toolchain.toml"))).unwrap_err().to_string();
            assert!(error.contains("can use only an absolute path of a folder with `bin/cargo` and `bin/rustc`"), "{error}");
        }
    }
}
