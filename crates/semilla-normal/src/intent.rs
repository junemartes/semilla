//! The intent crate (design 78): rustc compiles the intent files against the two versions.
//!
//! For each step, Semilla makes a crate in its cache:
//!
//! - two facade crates `before` and `after`: each one has `pub extern crate NAME;` for each
//!   library of the workspace of one tree, so `before::shop::parse` and `after::shop::parse` are
//!   real Rust paths (R78.1-1);
//! - the prelude crate `semilla_intent` and its attribute crate `semilla_intent_macros`
//!   (R78.1-3), compiled one time for each toolchain from the sources in the binary;
//! - the intent crate: one module for each intent file (a copy in the cache; each message of
//!   rustc has the path of the file of the user).
//!
//! The two trees compile exactly as normal (R78.2-2). The intent crate reads the metadata of the
//! N0 build of each tree: a copy of the metadata of each member, which the cache keeps after
//! each build (`save_libraries`), because the build of the other tree in the same folder writes
//! a file with the same name. The two versions of one member have the same crate identity (cargo
//! gives one `-C metadata` to the two trees), and rustc loads two crates only if their
//! identities differ, so the copy of the old member gets a new identity (R78.1-2). Only the
//! compile of the intent crate opens the private items (the driver, R78.2-1).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crate::cargo::Library;

/// The folder of the saved metadata in the cache folder of a build.
const META: &str = "meta";
/// The list of the libraries of a build.
const LIBRARIES: &str = "libraries.json";
/// The source of the attribute crate and of the prelude crate (R78.1-3).
const MACROS_SOURCE: &str = include_str!("../../semilla-intent-macros/src/lib.rs");
const PRELUDE_SOURCE: &str = include_str!("../../semilla-intent-prelude/src/lib.rs");
/// The crate name of the intent crate.
pub const CLAIMS_CRATE: &str = "semilla_claims";

/// [R78.1-2] Saves the metadata of the members of an N0 build in its cache folder, with the
/// identity of each member from its unit record, and the list of each library of the build.
pub fn save_libraries(cache: &Path, built: &crate::Built) -> Result<()> {
    let meta = cache.join(META);
    std::fs::create_dir_all(&meta)?;
    let mut list = Vec::new();
    for library in &built.libraries {
        let mut library = library.clone();
        if library.member {
            let Some(name) = library.file.file_name() else { continue };
            if !library.file.is_file() {
                continue;
            }
            std::fs::copy(&library.file, meta.join(name)).with_context(|| format!("cannot copy {}", library.file.display()))?;
            library.identity = unit_identity(cache, &built.records, &library).unwrap_or_default();
            library.file = PathBuf::from(name);
        }
        list.push(library);
    }
    std::fs::write(meta.join(LIBRARIES), serde_json::to_string_pretty(&list)?)?;
    Ok(())
}

/// The identity of a member from the unit record of its record file (the record file has the
/// crate name and the hash of the library file in its name).
fn unit_identity(cache: &Path, records: &[String], library: &Library) -> Option<String> {
    let stem = library.file.file_stem()?.to_str()?;
    let hash = stem.rsplit_once('-')?.1;
    let record = records.iter().find(|name| name.starts_with(&format!("{}.", library.name)) && name.ends_with(&format!(".{hash}.jsonl")))?;
    let text = std::fs::read_to_string(cache.join(record)).ok()?;
    let line = text.lines().next()?;
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    value["unit"]["identity"].as_str().map(str::to_string)
}

/// The libraries of one tree: the members (files in the cache) and the dependencies (files in
/// the target folder of the build).
#[derive(Clone, Debug)]
pub struct Libraries {
    pub members: Vec<Library>,
    pub deps: Vec<Library>,
}

/// Reads the libraries that `save_libraries` saved. `None`: the cache folder has none (a
/// cache of an older driver).
pub fn read_libraries(cache: &Path) -> Option<Libraries> {
    let meta = cache.join(META);
    let list: Vec<Library> = serde_json::from_str(&std::fs::read_to_string(meta.join(LIBRARIES)).ok()?).ok()?;
    let (mut members, deps): (Vec<Library>, Vec<Library>) = list.into_iter().partition(|library| library.member);
    for member in &mut members {
        member.file = meta.join(&member.file);
    }
    Some(Libraries { members, deps })
}

/// The crate hash (SVH) of a metadata file: the 16 bytes after the header and the two root
/// positions.
fn crate_hash(bytes: &[u8]) -> Option<[u8; 16]> {
    bytes.get(24..40)?.try_into().ok()
}

/// [R78.1-2] The old members that need the second build: a member whose twin of the new tree
/// (the same name and crate identity) has another crate hash. Rustc loads two crates only if
/// their identities differ.
pub fn renamed_members(before: &Libraries, after: &Libraries) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for member in &before.members {
        let Some(twin) = after.members.iter().find(|other| other.name == member.name && !member.identity.is_empty() && other.identity == member.identity) else { continue };
        let (old, new) = (std::fs::read(&member.file)?, std::fs::read(&twin.file)?);
        if crate_hash(&old) != crate_hash(&new) && !names.contains(&member.name) {
            names.push(member.name.clone());
        }
    }
    names.sort();
    Ok(names)
}

/// [R78.1-2] The second check build of the old tree (option B of the user, 2026-11-26): its
/// libraries, from the cache when an earlier check made them (the key has the tree, the names,
/// and each build setting), and true if this call built them. `workspace` has the files of the
/// old tree.
pub fn second_build(cache_root: &Path, key: &str, workspace: &Path, names: &[String]) -> Result<(Libraries, bool)> {
    let folder = cache_root.join("intent").join(format!("before-{key}"));
    if folder.join(".complete").exists()
        && let Some(found) = read_libraries(&folder)
    {
        return Ok((found, false));
    }
    let _ = std::fs::remove_dir_all(folder.join(META));
    let stdout = crate::cargo::intent_second_build(workspace, &folder.join("target"), names)?;
    let workspace = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let meta = folder.join(META);
    std::fs::create_dir_all(&meta)?;
    let mut list = Vec::new();
    for mut library in crate::cargo::unit_libraries(&stdout, &workspace) {
        if library.member {
            let Some(name) = library.file.file_name().map(|name| name.to_os_string()) else { continue };
            std::fs::copy(&library.file, meta.join(&name))?;
            library.file = PathBuf::from(name);
        }
        list.push(library);
    }
    std::fs::write(meta.join(LIBRARIES), serde_json::to_string_pretty(&list)?)?;
    std::fs::write(folder.join(".complete"), "")?;
    Ok((read_libraries(&folder).context("the second build has no list of libraries")?, true))
}

/// The folders of one prepared pair of trees.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub folder: PathBuf,
    /// The open folders of the members of the old and the new tree (R78.2-1).
    pub open: [PathBuf; 2],
    pub deps: PathBuf,
    pub facade: PathBuf,
    pub prelude: PathBuf,
    /// The names of the libraries of each facade.
    pub names: [Vec<String>; 2],
    /// True if this call made the folders (a cold check).
    pub made: bool,
}

/// The toolchain programs and their environment.
fn tool(program: &str) -> Result<Command> {
    let root = crate::cargo::toolchain_root()?;
    let mut command = Command::new(if program == "rustc" { root.join("bin/rustc") } else { PathBuf::from(program) });
    crate::cargo::build_environment(&mut command, &root, None);
    command.env("RUSTC_ICE", "0");
    Ok(command)
}

fn run(mut command: Command, what: &str) -> Result<()> {
    let output = command.output().with_context(|| format!("cannot start the compile of {what}"))?;
    if !output.status.success() {
        bail!("the compile of {what} failed:\n{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// [R78.1-3] The prelude crate and its attribute crate, compiled one time for each toolchain.
pub fn prelude(cache_root: &Path) -> Result<PathBuf> {
    let root = crate::cargo::toolchain_root()?;
    let key = format!("{:016x}", fnv(format!("{}\n{MACROS_SOURCE}\n{PRELUDE_SOURCE}", root.display()).as_bytes()));
    let folder = cache_root.join("intent").join(format!("prelude-{key}"));
    if folder.join(".complete").exists() {
        return Ok(folder);
    }
    let partial = cache_root.join("intent").join(format!("prelude-{key}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(partial.join("src"))?;
    std::fs::write(partial.join("src/macros.rs"), MACROS_SOURCE)?;
    std::fs::write(partial.join("src/prelude.rs"), PRELUDE_SOURCE)?;
    let mut macros = tool("rustc")?;
    macros.args(["--edition", "2024", "--crate-type", "proc-macro", "--crate-name", "semilla_intent_macros", "--cap-lints", "allow", "-C", "metadata=semilla-intent-macros", "--extern", "proc_macro", "--out-dir"]).arg(&partial).arg(partial.join("src/macros.rs"));
    run(macros, "the attribute crate of the intent language")?;
    let dylib = std::fs::read_dir(&partial)?.flatten().map(|entry| entry.path()).find(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("libsemilla_intent_macros"))).context("the attribute crate has no library file")?;
    let mut prelude = tool("rustc")?;
    prelude.args(["--edition", "2024", "--crate-type", "lib", "--crate-name", "semilla_intent", "--emit", "metadata", "--cap-lints", "allow", "-C", "metadata=semilla-intent-prelude", "-o"]).arg(partial.join("libsemilla_intent.rmeta")).arg("--extern").arg(format!("semilla_intent_macros={}", dylib.display())).arg(partial.join("src/prelude.rs"));
    run(prelude, "the prelude of the intent language")?;
    std::fs::write(partial.join(".complete"), "")?;
    match std::fs::rename(&partial, &folder) {
        Ok(()) => Ok(folder),
        // Another process made the same folder first.
        Err(_) if folder.join(".complete").exists() => {
            let _ = std::fs::remove_dir_all(&partial);
            Ok(folder)
        }
        Err(error) => Err(error.into()),
    }
}

/// The FNV-1a hash of 64 bits: a fixed function, so a key and an identity are the same on each
/// machine.
pub fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}

/// [R78.1-1] [R78.1-2] Prepares the open folders and the facades of a pair of trees. The key
/// names the two builds; a second call with the same key reuses the folders (a warm check).
pub fn prepare(cache_root: &Path, key: &str, before: &Libraries, after: &Libraries, renamed: Option<&Libraries>) -> Result<Prepared> {
    let prelude = prelude(cache_root)?;
    let folder = cache_root.join("intent").join(format!("pair-{key}"));
    let open = [folder.join("open/before"), folder.join("open/after")];
    let names = [facade_names(before), facade_names(after)];
    let prepared = |made| Prepared { folder: folder.clone(), open: open.clone(), deps: folder.join("deps"), facade: folder.join("facade"), prelude: prelude.clone(), names: names.clone(), made };
    if folder.join(".complete").exists() {
        return Ok(prepared(false));
    }
    let _ = std::fs::remove_dir_all(&folder);
    for part in [&open[0], &open[1], &folder.join("deps"), &folder.join("facade")] {
        std::fs::create_dir_all(part)?;
    }
    // The dependencies of the two trees: one link for each file name (one name is one unit).
    for library in before.deps.iter().chain(&after.deps).chain(renamed.into_iter().flat_map(|renamed| &renamed.deps)) {
        let Some(name) = library.file.file_name() else { continue };
        let link = folder.join("deps").join(name);
        if !link.exists() && library.file.exists() {
            std::os::unix::fs::symlink(&library.file, &link)?;
        }
    }
    // The members of the new tree as they are; each old member that is not the same crate as a
    // new member, with a new identity if the two have one identity.
    let mut chosen: [Vec<(String, PathBuf, bool, usize)>; 2] = [Vec::new(), Vec::new()];
    for member in &after.members {
        let Some(name) = member.file.file_name() else { continue };
        let target = open[1].join(name);
        std::fs::copy(&member.file, &target)?;
        chosen[1].push((member.name.clone(), target, member.proc_macro, member.features.len()));
    }
    for member in &before.members {
        let bytes = std::fs::read(&member.file).with_context(|| format!("cannot read {}", member.file.display()))?;
        let twin = after.members.iter().find(|other| other.name == member.name && !member.identity.is_empty() && other.identity == member.identity);
        if let Some(twin) = twin {
            let other = std::fs::read(&twin.file)?;
            if crate_hash(&bytes).is_some() && crate_hash(&bytes) == crate_hash(&other) {
                // The same crate in the two trees: one crate for the two facades.
                let path = open[1].join(twin.file.file_name().unwrap_or_default());
                chosen[0].push((member.name.clone(), path, member.proc_macro, member.features.len()));
                continue;
            }
            // [R78.1-2] One identity and two crate hashes: the copy of the second build, whose
            // identity rustc made new (also the symbol of a proc-macro crate).
            let Some(copy) = renamed.and_then(|renamed| renamed.members.iter().find(|other| other.name == member.name && other.proc_macro == member.proc_macro && other.features == member.features).or_else(|| renamed.members.iter().find(|other| other.name == member.name && other.proc_macro == member.proc_macro))) else {
                bail!("the old member `{}` has the crate identity of the new member and no copy of the second build", member.name);
            };
            let Some(name) = copy.file.file_name() else { continue };
            let target = open[0].join(name);
            std::fs::copy(&copy.file, &target)?;
            chosen[0].push((member.name.clone(), target, member.proc_macro, member.features.len()));
            continue;
        }
        let Some(name) = member.file.file_name() else { continue };
        let target = open[0].join(name);
        std::fs::write(&target, bytes)?;
        chosen[0].push((member.name.clone(), target, member.proc_macro, member.features.len()));
    }
    for (index, side) in ["before", "after"].into_iter().enumerate() {
        let mut text = String::from("//! The facade of one tree (design 78, R78.1-1).\n");
        let mut command = tool("rustc")?;
        command.args(["--edition", "2021", "--crate-type", "lib", "--crate-name", side, "--emit", "metadata", "--cap-lints", "allow"]);
        command.arg("-C").arg(format!("metadata=semilla-intent-{side}-{key}"));
        // One unit for each library name: a package can have two library units (two feature
        // sets). The facade names the unit with the most features; each unit stays in the open
        // folder for the lookups of the other crates.
        let mut units: Vec<&(String, PathBuf, bool, usize)> = chosen[index].iter().filter(|(_, _, proc_macro, _)| !proc_macro).collect();
        units.sort_by(|a, b| a.0.cmp(&b.0).then(b.3.cmp(&a.3)).then(a.1.cmp(&b.1)));
        units.dedup_by(|later, first| later.0 == first.0);
        for (name, path, _, _) in units {
            text.push_str(&format!("pub extern crate {name};\n"));
            command.arg("--extern").arg(format!("{name}={}", path.display()));
        }
        let source = folder.join("facade").join(format!("{side}.rs"));
        std::fs::write(&source, text)?;
        for search in search_folders(&prepared(true)) {
            command.arg("-L").arg(format!("dependency={}", search.display()));
        }
        command.arg("-o").arg(folder.join("facade").join(format!("lib{side}.rmeta"))).arg(&source);
        run(command, &format!("the facade `{side}`"))?;
    }
    std::fs::write(folder.join(".complete"), "")?;
    Ok(prepared(true))
}

/// [R78.3-6] The prepared folders of a pair that an earlier check made (a warm check). `None`:
/// the folder is not complete.
pub fn prepared_at(cache_root: &Path, folder: &Path) -> Result<Option<Prepared>> {
    if !folder.join(".complete").exists() {
        return Ok(None);
    }
    let prelude = prelude(cache_root)?;
    let open = [folder.join("open/before"), folder.join("open/after")];
    Ok(Some(Prepared { folder: folder.to_path_buf(), open, deps: folder.join("deps"), facade: folder.join("facade"), prelude, names: [Vec::new(), Vec::new()], made: false }))
}

/// The names of the libraries of a facade (no proc-macro crate).
fn facade_names(libraries: &Libraries) -> Vec<String> {
    let mut names: Vec<String> = libraries.members.iter().filter(|member| !member.proc_macro).map(|member| member.name.clone()).collect();
    names.sort();
    names.dedup();
    names
}

/// The search folders of each compile of a pair.
fn search_folders(prepared: &Prepared) -> Vec<PathBuf> {
    vec![prepared.open[0].clone(), prepared.open[1].clone(), prepared.deps.clone(), prepared.facade.clone(), prepared.prelude.clone()]
}

/// One intent file: the path of the user (relative to the repository) and its text.
#[derive(Clone, Debug)]
pub struct Source {
    pub path: String,
    pub text: String,
}

/// One message of rustc, with the places of the files of the user.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Diagnostic {
    /// `error`, `warning`, `note`, `help`.
    pub level: String,
    /// The code of rustc (`E0425`), if the message has one.
    pub code: Option<String>,
    pub message: String,
    /// The file of the user, the line, and the column of the first primary span.
    pub file: Option<String>,
    pub line: usize,
    pub column: usize,
    /// The text of rustc, with the caret and the help.
    pub rendered: String,
    /// The suggestions of rustc that it can apply with no other change (`MachineApplicable`).
    pub fixes: Vec<Fix>,
}

/// One machine-applicable suggestion: a replacement of a byte range of a file of the user.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Fix {
    pub file: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// [R78.1-3] One scope path of a claim and its definition, from the driver.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Link {
    pub path: String,
    /// The record name with the facade (`before::shop::inner::len`); `None`: no definition.
    pub resolved: Option<String>,
    #[serde(default)]
    pub near: Vec<String>,
    /// [R80.3-1] The items of the definition for a scope (a type: its inherent methods; a module:
    /// its `pub` functions and methods; a trait: the methods of its impls in the two trees).
    #[serde(default)]
    pub members: Vec<Member>,
}

/// [R80.3-1] One item of a scope: the record name with the facade, and for a method the type of
/// its impl.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Member {
    pub name: String,
    #[serde(default)]
    pub owner: Option<String>,
    /// The name with the visible paths of the other crates, when it is not `name` (a record of a
    /// tree names a trait of another crate by its visible path).
    #[serde(default)]
    pub visible: Option<String>,
}

/// The result of the compile of an intent crate.
#[derive(Debug, Default)]
pub struct Compiled {
    /// The definition of each scope path.
    pub links: Vec<Link>,
    /// The body records of the intent crate (the claims, their closures, the relations).
    pub records: Vec<crate::BodyRecord>,
    pub diagnostics: Vec<Diagnostic>,
    /// The module of each file, in the order of the files.
    pub modules: Vec<String>,
    pub ok: bool,
    pub took: Duration,
}

/// The module name of each file: the file stem if it is a Rust name and no other file has it,
/// else `f` and the number of the file.
pub fn module_names(files: &[Source]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let stem = Path::new(&file.path).file_stem().map(|stem| stem.to_string_lossy().replace('-', "_")).unwrap_or_default();
        let plain = !stem.is_empty() && stem.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        let name = if plain && !names.contains(&stem) { stem } else { format!("f{index}") };
        names.push(name);
    }
    names
}

/// [R78.3-1] Compiles the intent crate with the driver: the errors of rustc, and the records of
/// the claims.
pub fn compile(prepared: &Prepared, files: &[Source], workspace_root: &Path) -> Result<Compiled> {
    compile_in(prepared, files, workspace_root, true)
}

/// The same compile with no open folder: the privacy of the two versions as rustc gives it (a
/// test of R78.2-1).
pub fn compile_closed(prepared: &Prepared, files: &[Source], workspace_root: &Path) -> Result<Compiled> {
    compile_in(prepared, files, workspace_root, false)
}

fn compile_in(prepared: &Prepared, files: &[Source], workspace_root: &Path, open: bool) -> Result<Compiled> {
    let started = Instant::now();
    let folder = prepared.folder.join(format!("crate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(folder.join("src"))?;
    let modules = module_names(files);
    let mut root = String::from("// The intent crate (design 78). Each module is one intent file.\n#![allow(dead_code, unused_imports, unused_variables, unused_mut, non_snake_case, clippy::all)]\n");
    let driver = crate::cargo::find_driver()?;
    let mut command = tool(&driver.to_string_lossy())?;
    command.args(["--level", "n0", "--", "--edition", "2024", "--crate-type", "lib", "--crate-name", CLAIMS_CRATE, "--emit", "metadata", "--error-format=json"]);
    for (file, module) in files.iter().zip(&modules) {
        let copy = folder.join("src").join(format!("{module}.rs"));
        // [R78.1-4] The prelude of the language in each file, as in 10r1: an import at the start
        // of line 1 (no line of the file of the user moves; the messages move the columns of
        // line 1 back), or at the end of a file that starts with an inner attribute.
        let text = match prelude_prefix(&file.text) {
            Some(prefix) => format!("{prefix}{}", file.text),
            None => format!("{}\n#[allow(unused_imports)]\nuse semilla_intent::prelude::*;\n", file.text),
        };
        std::fs::write(&copy, text)?;
        // A raw name: a file stem can be a keyword of Rust (`fn.rs`, `match.rs`).
        root.push_str(&format!("#[path = {:?}] mod r#{module};\n", copy.display().to_string()));
        command.arg(format!("--remap-path-prefix={}={}", copy.display(), file.path));
    }
    std::fs::write(folder.join("src/lib.rs"), &root)?;
    for (name, file) in [("before", "libbefore.rmeta"), ("after", "libafter.rmeta")] {
        command.arg("--extern").arg(format!("{name}={}", prepared.facade.join(file).display()));
    }
    command.arg("--extern").arg(format!("semilla_intent={}", prepared.prelude.join("libsemilla_intent.rmeta").display()));
    for search in search_folders(prepared) {
        command.arg("-L").arg(format!("dependency={}", search.display()));
    }
    command.arg("-o").arg(folder.join(format!("lib{CLAIMS_CRATE}.rmeta"))).arg(folder.join("src/lib.rs"));
    if open {
        command.env("SEMILLA_INTENT_OPEN", format!("{}\n{}", prepared.open[0].display(), prepared.open[1].display()));
    }
    command.env("SEMILLA_INTENT_PRELUDE", &prepared.prelude);
    command.current_dir(workspace_root);
    let output = command.output().context("cannot start the driver for the intent crate")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let texts: Vec<(&str, &str)> = files.iter().map(|file| (file.path.as_str(), file.text.as_str())).collect();
    let diagnostics = parse_diagnostics(&stderr, &texts);
    let records = if output.status.success() { crate::parse_records(&stdout).unwrap_or_default() } else { Vec::new() };
    let _ = std::fs::remove_dir_all(&folder);
    let links = stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value["$message_type"].as_str() == Some("semilla_links"))
        .and_then(|value| serde_json::from_value::<Vec<Link>>(value["links"].clone()).ok())
        .unwrap_or_default();
    let mut compiled = Compiled { links, records, diagnostics, modules, ok: output.status.success(), took: started.elapsed() };
    // A failure with no error message (an internal error of the driver): the text of the
    // driver is the error.
    if !compiled.ok && !compiled.diagnostics.iter().any(|d| d.level == "error") {
        let text: String = stderr.lines().filter(|line| !line.starts_with('{')).collect::<Vec<_>>().join("\n");
        compiled.diagnostics.push(Diagnostic { level: "error".into(), code: None, message: format!("the driver stopped: {}", text.lines().next().unwrap_or("no message")), file: None, line: 0, column: 0, rendered: text, fixes: Vec::new() });
    }
    Ok(compiled)
}

/// [R78.1-4] The import of the prelude at the start of line 1 of an intent file, or `None` for a
/// file that starts with an inner attribute or an inner doc comment (the import goes at the end).
pub fn prelude_prefix(text: &str) -> Option<&'static str> {
    let first = text.trim_start();
    (!first.starts_with("#!") && !first.starts_with("//!") && !first.starts_with("/*!")).then_some("#[allow(unused_imports)] use semilla_intent::prelude::*; ")
}

/// The messages of rustc (`--error-format=json`). The summary lines ("aborting due to …") are
/// not messages of a claim.
pub fn parse_diagnostics(stderr: &str, files: &[(&str, &str)]) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    for line in stderr.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if value["$message_type"].as_str() != Some("diagnostic") {
            continue;
        }
        let message = value["message"].as_str().unwrap_or("").to_string();
        if message.starts_with("aborting due to") || message.starts_with("For more information about") {
            continue;
        }
        let level = value["level"].as_str().unwrap_or("error").to_string();
        // The summary of the codes ("Some errors have detailed explanations") is not a message.
        if level == "failure-note" {
            continue;
        }
        let code = value["code"]["code"].as_str().map(str::to_string);
        let primary = value["spans"].as_array().into_iter().flatten().find(|span| span["is_primary"].as_bool() == Some(true));
        let file = primary.and_then(|span| span["file_name"].as_str()).map(str::to_string);
        let line_number = primary.and_then(|span| span["line_start"].as_u64()).unwrap_or(0) as usize;
        let mut column = primary.and_then(|span| span["column_start"].as_u64()).unwrap_or(0) as usize;
        // [R78.1-4] Line 1 of the copy starts with the import of the prelude.
        let shift = |file: Option<&str>| files.iter().find(|(path, _)| Some(*path) == file).and_then(|(_, text)| prelude_prefix(text)).map_or(0, str::len);
        if line_number == 1 {
            column = column.saturating_sub(shift(file.as_deref()));
        }
        let mut fixes = Vec::new();
        collect_fixes(&value, files, &mut fixes);
        let mut rendered = value["rendered"].as_str().unwrap_or("").trim_end().to_string();
        for (path, text) in files {
            if let Some(prefix) = prelude_prefix(text) {
                rendered = without_prefix(&rendered, path, prefix);
            }
        }
        found.push(Diagnostic { level, code, message, file, line: line_number, column, rendered, fixes });
    }
    found
}

/// [R78.1-4] The rendered text of a message as the file of the user has it: the import of the
/// prelude leaves each shown line 1 of `path`, and its caret lines and its column move back.
fn without_prefix(rendered: &str, path: &str, prefix: &str) -> String {
    let n = prefix.len();
    let mut out: Vec<String> = Vec::new();
    // True while the lines after a shown line 1 of `path` are its caret lines.
    let mut after_first = false;
    let mut in_file = false;
    for line in rendered.lines() {
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        if let Some(place) = trimmed.strip_prefix("--> ").or_else(|| trimmed.strip_prefix("::: ")) {
            in_file = place.starts_with(&format!("{path}:"));
            after_first = false;
            let marker = &trimmed[..4];
            let moved = place.strip_prefix(&format!("{path}:1:")).and_then(|column| column.parse::<usize>().ok()).map(|column| format!("{indent}{marker}{path}:1:{}", column.saturating_sub(n).max(1)));
            out.push(moved.unwrap_or_else(|| line.to_string()));
            continue;
        }
        if in_file {
            if let Some(rest) = trimmed.strip_prefix("1 | ").or_else(|| trimmed.strip_prefix("1 + ")).and_then(|rest| rest.strip_prefix(prefix).map(|source| (&trimmed[..4], source))) {
                out.push(format!("{indent}{}{}", rest.0, rest.1));
                after_first = true;
                continue;
            }
            if after_first {
                if let Some(marks) = trimmed.strip_prefix("| ") {
                    if marks.len() >= n && marks[..n].bytes().all(|b| b == b' ') {
                        out.push(format!("{indent}| {}", &marks[n..]));
                        continue;
                    }
                }
                after_first = false;
            }
        }
        out.push(line.to_string());
    }
    out.join("\n")
}

/// The machine-applicable suggestions of a message and of its children, in the files of the user.
fn collect_fixes(value: &serde_json::Value, files: &[(&str, &str)], fixes: &mut Vec<Fix>) {
    for span in value["spans"].as_array().into_iter().flatten() {
        let (Some(text), Some("MachineApplicable")) = (span["suggested_replacement"].as_str(), span["suggestion_applicability"].as_str()) else { continue };
        let Some(file) = span["file_name"].as_str() else { continue };
        if !files.iter().any(|(path, _)| *path == file) {
            continue;
        }
        let (Some(start), Some(end)) = (span["byte_start"].as_u64(), span["byte_end"].as_u64()) else { continue };
        let shift = files.iter().find(|(path, _)| *path == file).and_then(|(_, text)| prelude_prefix(text)).map_or(0, str::len);
        let (Some(start), Some(end)) = ((start as usize).checked_sub(shift), (end as usize).checked_sub(shift)) else { continue };
        fixes.push(Fix { file: file.to_string(), start, end, text: text.to_string() });
    }
    for child in value["children"].as_array().into_iter().flatten() {
        collect_fixes(child, files, fixes);
    }
}

/// Applies the fixes of one file (each range once, from the end, with no overlap).
pub fn apply_fixes(text: &str, fixes: &[&Fix]) -> String {
    let mut sorted: Vec<&&Fix> = fixes.iter().collect();
    sorted.sort_by(|a, b| b.start.cmp(&a.start));
    let mut out = text.to_string();
    let mut limit = usize::MAX;
    for fix in sorted {
        if fix.end > limit || fix.end > out.len() || fix.start > fix.end || !out.is_char_boundary(fix.start) || !out.is_char_boundary(fix.end) {
            continue;
        }
        out.replace_range(fix.start..fix.end, &fix.text);
        limit = fix.start;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names_are_rust_names_with_no_twin() {
        let files = [Source { path: ".semilla/intent/pr.rs".into(), text: String::new() }, Source { path: "x/pr.rs".into(), text: String::new() }, Source { path: "a/9-x.rs".into(), text: String::new() }, Source { path: "a/before.rs".into(), text: String::new() }];
        assert_eq!(module_names(&files), ["pr", "f1", "f2", "before"]);
    }

    // rule+ R78.1-4
    #[test]
    fn a_message_on_line_one_gives_the_place_and_the_text_of_the_file_of_the_user() {
        let text = "#[same(shop::parse)] fn f() -> u8 { 1u16 }\n";
        let prefix = prelude_prefix(text).unwrap();
        let column = prefix.len() + 37;
        let rendered = format!("error[E0308]: mismatched types\n --> a.rs:1:{column}\n  |\n1 | {prefix}#[same(shop::parse)] fn f() -> u8 {{ 1u16 }}\n  | {}^^^^ expected `u8`\n", " ".repeat(column - 1));
        let message = serde_json::json!({"$message_type": "diagnostic", "message": "mismatched types", "level": "error", "code": {"code": "E0308"}, "rendered": rendered, "children": [],
            "spans": [{"is_primary": true, "file_name": "a.rs", "line_start": 1, "column_start": column, "byte_start": column - 1, "byte_end": column + 3, "suggested_replacement": "1u8", "suggestion_applicability": "MachineApplicable"}]});
        let found = parse_diagnostics(&message.to_string(), &[("a.rs", text)]);
        assert_eq!((found[0].line, found[0].column), (1, 37));
        assert_eq!(found[0].rendered, format!("error[E0308]: mismatched types\n --> a.rs:1:37\n  |\n1 | #[same(shop::parse)] fn f() -> u8 {{ 1u16 }}\n  | {}^^^^ expected `u8`", " ".repeat(36)));
        assert_eq!(apply_fixes(text, &[&found[0].fixes[0]]), "#[same(shop::parse)] fn f() -> u8 { 1u8 }\n");
    }

    // rule- R78.1-4
    #[test]
    fn a_file_with_an_inner_attribute_gets_the_import_at_its_end_and_no_place_moves() {
        let text = "#![allow(dead_code)]\n#[same(shop::parse)] fn f() -> u8 { 1u16 }\n";
        assert_eq!(prelude_prefix(text), None);
        assert!(prelude_prefix("  //! a file doc\n").is_none() && prelude_prefix("/*! doc */\n").is_none());
        let message = serde_json::json!({"$message_type": "diagnostic", "message": "unused", "level": "warning", "rendered": " --> a.rs:1:4\n1 | #![allow(dead_code)]", "children": [],
            "spans": [{"is_primary": true, "file_name": "a.rs", "line_start": 1, "column_start": 4}]});
        let found = parse_diagnostics(&message.to_string(), &[("a.rs", text)]);
        assert_eq!((found[0].line, found[0].column), (1, 4));
        assert_eq!(found[0].rendered, " --> a.rs:1:4\n1 | #![allow(dead_code)]");
    }

    #[test]
    fn fixes_apply_from_the_end_and_skip_an_overlap() {
        let fix = |start, end, text: &str| Fix { file: "f".into(), start, end, text: text.into() };
        let (a, b, c) = (fix(0, 1, "X"), fix(4, 5, "Y"), fix(3, 5, "Z"));
        assert_eq!(apply_fixes("abcdef", &[&a, &b]), "XbcdYf");
        assert_eq!(apply_fixes("abcdef", &[&b, &c]), "abcdYf");
    }
}

