//! The check of the intent files with the oracle (design 78, part C): the builds of the two
//! trees, then the intent crate of `semilla_normal::intent`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use semilla_normal::intent::{Compiled, Libraries, Source, compile, fnv, prepare, read_libraries};

/// The result of `semilla intent check` (design 78, part C).
#[derive(Debug)]
pub(crate) struct Check {
    pub compiled: Compiled,
    /// The heads of the claims of each file.
    pub heads: Vec<(String, Vec<semilla_intent::heads::Head>)>,
    /// The findings of Semilla for the meaning of the attributes.
    pub findings: Vec<semilla_intent::heads::Finding>,
    /// True if this check made the facades (a cold check).
    pub cold: bool,
    /// The time of the builds of the two trees (zero if the cache has them).
    pub build: Duration,
    /// The time of the check after the builds.
    pub took: Duration,
    /// [R78.1-2] The second build of the old tree: its time and true if this check ran it.
    pub second: Option<(Duration, bool)>,
}

impl Check {
    /// The errors of rustc and of Semilla.
    pub fn errors(&self) -> (usize, usize) {
        let rustc = self.compiled.diagnostics.iter().filter(|d| d.level == "error").count();
        let semilla = self.findings.iter().filter(|f| f.error).count();
        (rustc, semilla)
    }
}

/// [R78.3-6] The file that names the pair folder of two trees for a warm check: the key has the
/// two trees and each build setting that is not in a tree (the outer cargo configuration, the
/// toolchain, the driver, the sandbox).
fn fast_file(cache_root: &std::path::Path, before: &str, after: &str) -> Option<std::path::PathBuf> {
    let driver = semilla_normal::cargo::find_driver().ok()?;
    let settings = semilla_normal::cargo::settings_key(cache_root, Some(&driver));
    let key = format!("{:016x}", fnv(format!("{before}\n{after}\n{settings}").as_bytes()));
    Some(cache_root.join("intent").join(format!("fast-{key}")))
}

/// [R78.3-6] A warm check: the pair folder of an earlier check of the same two trees, with no
/// build code test and no build. `None`: no such folder.
pub(crate) fn check_fast(cache_root: &std::path::Path, root: &std::path::Path, before: &str, after: &str, files: &[Source]) -> Result<Option<Check>> {
    let started = Instant::now();
    let Some(fast) = fast_file(cache_root, before, after) else { return Ok(None) };
    let Ok(folder) = std::fs::read_to_string(&fast) else { return Ok(None) };
    let Some(prepared) = semilla_normal::intent::prepared_at(cache_root, std::path::Path::new(folder.trim()))? else { return Ok(None) };
    let (heads, mut findings) = heads_of(files);
    let compiled = compile(&prepared, files, root)?;
    findings.extend(link_findings(&compiled, &heads));
    Ok(Some(Check { compiled, heads, findings, cold: false, build: Duration::ZERO, took: started.elapsed(), second: None }))
}

/// [R78.1-3] The error S0112 for each scope path that the driver gives no definition, at the place
/// of the path in the attribute, with the nearest names.
fn link_findings(compiled: &Compiled, heads: &[(String, Vec<semilla_intent::heads::Head>)]) -> Vec<semilla_intent::heads::Finding> {
    let mut found = Vec::new();
    for (file, heads) in heads {
        for head in heads {
            for (index, (scope, at)) in head.scope.iter().zip(&head.scope_at).enumerate() {
                for link in semilla_intent::heads::scope_links(&head.kind, index, scope) {
                    let Some(entry) = compiled.links.iter().find(|entry| entry.path == link) else { continue };
                    if entry.resolved.is_some() {
                        continue;
                    }
                    let near = semilla_intent::candidates(&link, &entry.near, 3);
                    let tail = if near.is_empty() { String::new() } else { format!("; candidates: {}", semilla_intent::quoted_list(&near)) };
                    found.push(semilla_intent::heads::Finding { code: "S0112", error: true, file: file.clone(), line: at.0, column: at.1, start: at.2, end: at.3, message: format!("the scope path `{link}` names no item of the version{tail}"), fix: None });
                }
            }
        }
    }
    found.sort_by(|a, b| (a.file.as_str(), a.start).cmp(&(b.file.as_str(), b.start)));
    found.dedup();
    found
}

fn heads_of(files: &[Source]) -> (Vec<(String, Vec<semilla_intent::heads::Head>)>, Vec<semilla_intent::heads::Finding>) {
    let mut heads = Vec::new();
    let mut findings = Vec::new();
    for file in files {
        let (found, mut finding) = semilla_intent::heads::check(&file.path, &file.text);
        heads.push((file.path.clone(), found));
        findings.append(&mut finding);
    }
    (heads, findings)
}

/// [R78.3-1] Checks the intent files against two trees: the builds of the trees (from the
/// cache, or one N0 build each), the facades (one time for each pair), the compile of the
/// intent crate by rustc, then the meaning of each attribute.
pub(crate) fn check(oracle: &crate::oracle::NormalOracle, before: &str, after: &str, files: &[Source]) -> Result<Check> {
    let started = Instant::now();
    let (heads, findings) = heads_of(files);
    let config = semilla_normal::Configuration::DEV;
    let mut libraries = Vec::new();
    let mut keys = String::new();
    for tree in [before, after] {
        oracle.records_or_build(tree, "n0", &config, false)?;
        let (_, cache, _, _) = oracle.cache_folder(tree, "n0", &config)?;
        let found = if crate::git::is_empty_tree(tree) { Some(Libraries { members: Vec::new(), deps: Vec::new() }) } else { read_libraries(&cache) };
        let found = found.with_context(|| format!("the cache of the build of tree {tree} has no metadata for the intent crate (a cache of an older driver): remove {}", cache.display()))?;
        let time = std::fs::metadata(cache.join(".complete")).and_then(|meta| meta.modified()).map(|time| format!("{time:?}")).unwrap_or_default();
        keys.push_str(&format!("{}\n{time}\n", cache.display()));
        libraries.push(found);
    }
    let build = started.elapsed();
    let checking = Instant::now();
    let key = format!("{:016x}", fnv(keys.as_bytes()));
    // [R78.1-2] The members of the old tree with the identity of a new member and another crate
    // hash: the second build gives them a new identity (option B of the user).
    let names = semilla_normal::intent::renamed_members(&libraries[0], &libraries[1])?;
    let mut second = None;
    let renamed = if names.is_empty() {
        None
    } else {
        let started = Instant::now();
        let (root, _, _, name) = oracle.cache_folder(before, "n0", &config)?;
        let base = root.join("build");
        let driver = semilla_normal::cargo::find_driver()?;
        let settings = semilla_normal::cargo::settings_key(&base, Some(&driver));
        let second_key = format!("{:016x}", fnv(format!("{before}\n{}\n{settings}", names.join(",")).as_bytes()));
        // One build folder serves one build at a time: the lock of the builds of the step.
        std::fs::create_dir_all(&base)?;
        let lock = std::fs::File::create(base.join(format!("{name}.lock")))?;
        lock.lock()?;
        let folder = oracle.repo.cache_dir().join("intent").join(format!("before-{second_key}"));
        let found = if folder.join(".complete").exists() {
            semilla_normal::intent::second_build(&oracle.repo.cache_dir(), &second_key, &base.join(&name), &names)
        } else {
            oracle.repo.checkout_build(&base.join(&name), before).and_then(|()| semilla_normal::intent::second_build(&oracle.repo.cache_dir(), &second_key, &base.join(&name), &names))
        };
        lock.unlock()?;
        let (libraries, built) = found?;
        second = Some((started.elapsed(), built));
        Some(libraries)
    };
    let prepared = prepare(&oracle.repo.cache_dir(), &key, &libraries[0], &libraries[1], renamed.as_ref())?;
    if let Some(fast) = fast_file(&oracle.repo.cache_dir(), before, after) {
        let _ = std::fs::write(fast, prepared.folder.display().to_string());
    }
    let compiled = compile(&prepared, files, &oracle.repo.root)?;
    let mut findings = findings;
    findings.extend(link_findings(&compiled, &heads));
    Ok(Check { compiled, heads, findings, cold: prepared.made, build: if build.as_millis() > 50 { build } else { Duration::ZERO }, took: checking.elapsed(), second })
}

/// The text of a finding of Semilla, in the form of a message of rustc.
pub(crate) fn render_finding(finding: &semilla_intent::heads::Finding, text: &str) -> String {
    let word = if finding.error { "error" } else { "not checked yet" };
    let line_text = text.lines().nth(finding.line.saturating_sub(1)).unwrap_or("");
    let width = finding.line.to_string().len();
    let pad = " ".repeat(width);
    let start_column = finding.column.saturating_sub(1);
    let length = text.get(finding.start..finding.end).map(|part| part.lines().next().unwrap_or("").chars().count()).unwrap_or(1).max(1);
    let mut out = format!("{word}[{}]: {}\n{pad}--> {}:{}:{}\n{pad} |\n{} | {line_text}\n{pad} | {}{}\n", finding.code, finding.message, finding.file, finding.line, finding.column, finding.line, " ".repeat(start_column), "^".repeat(length));
    if let Some((_, _, replacement)) = &finding.fix {
        out.push_str(&format!("{pad} = help: write `{}`\n", replacement.trim()));
    }
    out.push_str(&format!("{pad} = note: `semilla intent explain {}`\n", finding.code));
    out
}
