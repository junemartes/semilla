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
    let sources = compile_sources(files, &heads, &findings);
    let mut compiled = compile(&prepared, &sources, root)?;
    clean_panic(&mut compiled, &heads, &sources, &|sources| compile(&prepared, sources, root).ok());
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
                for link in semilla_intent::heads::head_links(head, index, scope) {
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
    let sources = compile_sources(files, &heads, &findings);
    let mut compiled = compile(&prepared, &sources, &oracle.repo.root)?;
    clean_panic(&mut compiled, &heads, &sources, &|sources| compile(&prepared, sources, &oracle.repo.root).ok());
    let mut findings = findings;
    findings.extend(link_findings(&compiled, &heads));
    Ok(Check { compiled, heads, findings, cold: prepared.made, build: if build.as_millis() > 50 { build } else { Duration::ZERO }, took: checking.elapsed(), second })
}

/// [R87.3-1] True for the text of an internal error of the compiler (a panic).
pub(crate) fn is_panic(text: &str) -> bool {
    text.contains("panicked at") || text.contains("unexpectedly panicked") || text.contains("internal compiler error") || text.starts_with("the driver stopped")
}

/// The claims of the files: the index of the file, the first line, the end line (exclusive),
/// and the function.
fn claim_units(heads: &[(String, Vec<semilla_intent::heads::Head>)], files: &[Source]) -> Vec<(usize, usize, usize, String)> {
    let mut units: Vec<(usize, usize, usize, String)> = Vec::new();
    for (file_at, file) in files.iter().enumerate() {
        let Some((_, list)) = heads.iter().find(|(path, _)| *path == file.path) else { continue };
        let mut starts: Vec<(usize, String)> = Vec::new();
        for head in list {
            if !starts.iter().any(|(_, name)| *name == head.function) {
                starts.push((head.line, head.function.clone()));
            }
        }
        starts.sort();
        let lines = file.text.lines().count() + 1;
        for (index, (line, name)) in starts.iter().enumerate() {
            let end = starts.get(index + 1).map_or(lines + 1, |(next, _)| *next);
            units.push((file_at, *line, end, name.clone()));
        }
    }
    units
}

/// The files with only the claims `chosen` (each other claim is blank lines, so no line moves).
fn with_claims(files: &[Source], units: &[(usize, usize, usize, String)], chosen: &[usize]) -> Vec<Source> {
    files
        .iter()
        .enumerate()
        .map(|(file_at, file)| {
            let text = file
                .text
                .lines()
                .enumerate()
                .map(|(at, line)| {
                    let number = at + 1;
                    let blank = units.iter().enumerate().any(|(unit, (f, start, end, _))| *f == file_at && (*start..*end).contains(&number) && !chosen.contains(&unit));
                    if blank { "" } else { line }
                })
                .collect::<Vec<_>>()
                .join("\n");
            Source { path: file.path.clone(), text: format!("{text}\n") }
        })
        .collect()
}

/// [R87.3-1] The files for rustc: a claim with a scope path that rustc cannot take (`S0113`) is
/// blank lines, so the compiler never sees it (the finding of Semilla is its error).
fn compile_sources(files: &[Source], heads: &[(String, Vec<semilla_intent::heads::Head>)], findings: &[semilla_intent::heads::Finding]) -> Vec<Source> {
    let units = claim_units(heads, files);
    let chosen: Vec<usize> = (0..units.len()).filter(|unit| {
        let (file_at, start, end, _) = &units[*unit];
        !findings.iter().any(|finding| finding.code == "S0113" && finding.file == files[*file_at].path && (*start..*end).contains(&finding.line))
    }).collect();
    if chosen.len() == units.len() { files.to_vec() } else { with_claims(files, &units, &chosen) }
}

/// [R87.3-1] A compile that stops with an internal error of the compiler (a panic) gives one
/// error of Semilla (`S0121`) at the place of the claim whose compile alone stops the compiler,
/// in place of the text of the panic. The search compiles halves of the claims (the other
/// claims are blank lines, so no line moves).
fn clean_panic(compiled: &mut Compiled, heads: &[(String, Vec<semilla_intent::heads::Head>)], files: &[Source], again: &dyn Fn(&[Source]) -> Option<Compiled>) {
    let panicked = |compiled: &Compiled| compiled.diagnostics.iter().any(|d| d.level == "error" && (is_panic(&d.message) || is_panic(&d.rendered)));
    if !panicked(compiled) {
        return;
    }
    let units = claim_units(heads, files);
    let keep = |chosen: &[usize]| with_claims(files, &units, chosen);
    let mut set: Vec<usize> = (0..units.len()).collect();
    while set.len() > 1 {
        let (left, right) = set.split_at(set.len() / 2);
        let (left, right) = (left.to_vec(), right.to_vec());
        if again(&keep(&left)).is_some_and(|found| panicked(&found)) {
            set = left;
        } else if again(&keep(&right)).is_some_and(|found| panicked(&found)) {
            set = right;
        } else {
            break;
        }
    }
    let message = compiled.diagnostics.iter().find(|d| d.level == "error" && (is_panic(&d.message) || is_panic(&d.rendered))).map(|d| d.message.lines().next().unwrap_or("").to_string()).unwrap_or_default();
    let _ = message;
    compiled.diagnostics.retain(|d| !(is_panic(&d.message) || is_panic(&d.rendered)) && !(d.level != "error" && d.file.is_none()));
    let (file, line, function) = match set.first().and_then(|at| units.get(*at)) {
        Some((file_at, line, _, function)) if set.len() == 1 => (Some(files[*file_at].path.clone()), *line, format!("the claim `fn {function}`")),
        _ => (None, 0, "a claim of the intent files (the search found no one claim)".to_string()),
    };
    let place = file.as_ref().map(|file| format!("\n  --> {file}:{line}:1")).unwrap_or_default();
    let text = format!("the compiler stopped with an internal error at {function}; Semilla does not check this claim (`semilla intent explain S0121`)");
    compiled.diagnostics.insert(0, semilla_normal::intent::Diagnostic { level: "error".into(), code: Some("S0121".into()), message: text.clone(), file, line, column: 1, rendered: format!("error[S0121]: {text}{place}"), fixes: Vec::new() });
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A compile of the test: a panic if a file has a line with `bad`.
    fn fake(sources: &[Source]) -> Compiled {
        let mut compiled = Compiled { ok: true, ..Default::default() };
        if sources.iter().any(|source| source.text.lines().any(|line| line.contains("bad"))) {
            compiled.ok = false;
            compiled.diagnostics.push(semilla_normal::intent::Diagnostic { level: "error".into(), code: None, message: "the driver stopped: thread 'rustc' panicked at compiler/rustc_middle/src/ty/assoc.rs".into(), file: None, line: 0, column: 0, rendered: "thread 'rustc' panicked at …".into(), fixes: Vec::new() });
        }
        compiled
    }

    // rule+ R87.3-2
    // rule- R87.3-2
    #[test]
    fn a_panic_of_the_compiler_is_one_error_of_semilla_at_the_place_of_its_claim() {
        let text = "use semilla_intent::prelude::*;\n\n#[same(shop::a)]\nfn a_same() {}\n\n#[same(shop::bad)]\nfn bad_same() {}\n\n#[same(shop::c)]\nfn c_same() {}\n";
        let files = vec![Source { path: "i.rs".into(), text: text.into() }];
        let (heads, _) = heads_of(&files);
        let mut compiled = fake(&files);
        clean_panic(&mut compiled, &heads, &files, &|sources| Some(fake(sources)));
        assert_eq!(compiled.diagnostics.len(), 1, "{:?}", compiled.diagnostics);
        let found = &compiled.diagnostics[0];
        assert_eq!((found.code.as_deref(), found.file.as_deref(), found.line), (Some("S0121"), Some("i.rs"), 6));
        assert!(found.message.contains("the claim `fn bad_same`") && !found.rendered.contains("panicked"), "{found:?}");
        // The negative case: a compile with no panic keeps its messages.
        let clean = vec![Source { path: "i.rs".into(), text: text.replace("bad", "b") }];
        let (heads, _) = heads_of(&clean);
        let mut compiled = fake(&clean);
        clean_panic(&mut compiled, &heads, &clean, &|sources| Some(fake(sources)));
        assert!(compiled.diagnostics.is_empty());
    }
}
