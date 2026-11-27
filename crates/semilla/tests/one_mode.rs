//! Design 83: Semilla has one mode. Each step builds the two trees and has their MIR. The tests
//! of this file: a removed option of the mode is one error line (R83.1-1), and a digest never
//! depends on the folder of the build or of the binary (R83.2-1).

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

const MANIFEST: &str = "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

/// A repository with `old` as the commit and `new` in the working tree.
fn repository(folder: &Path, old: &str, new: &str) -> PathBuf {
    let repo = folder.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("Cargo.toml"), MANIFEST).unwrap();
    std::fs::write(repo.join("src/lib.rs"), old).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "old"]);
    std::fs::write(repo.join("src/lib.rs"), new).unwrap();
    repo
}

/// Runs semilla with the driver `driver` and more variables. Gives the exit code, stdout, and
/// stderr.
fn semilla(repo: &Path, driver: &Path, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo).args(args);
    command.env("SEMILLA_MIR_DRIVER", driver).env("SEMILLA_CACHE_DIR", repo.parent().unwrap().join("cache"));
    command.env_remove("SEMILLA_COMPILED").env_remove("SEMILLA_NO_CHECK").env_remove("SEMILLA_TEST_RULE_OFF");
    command.envs(env.iter().copied());
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
    let output = command.output().unwrap();
    (output.status.code(), String::from_utf8_lossy(&output.stdout).into_owned(), String::from_utf8_lossy(&output.stderr).into_owned())
}

fn driver() -> PathBuf {
    workspace_root().join("target/debug/semilla-mir-driver")
}

const LIMIT: &str = "pub fn limit() -> u32 {\n    10\n}\n";

// rule+ R83.1-1
#[test]
fn a_removed_option_of_the_mode_is_one_error_line() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), LIMIT, &LIMIT.replace("10", "20"));
    let gone = |what: &str| format!("error: {what} is gone (design 83): ");
    let cases: [(&[&str], &[(&str, &str)], String); 6] = [
        (&["step", "raise the limit", "--compiled"], &[], gone("the option `--compiled`")),
        (&["step", "raise the limit", "--no-check"], &[], gone("the option `--no-check`")),
        (&["step", "raise the limit"], &[("SEMILLA_COMPILED", "1")], gone("the variable `SEMILLA_COMPILED`")),
        (&["step", "raise the limit"], &[("SEMILLA_NO_CHECK", "1")], gone("the variable `SEMILLA_NO_CHECK`")),
        (&["tag", "--no-check"], &[], gone("the option `--no-check`")),
        (&["status"], &[("SEMILLA_NO_CHECK", "yes")], gone("the variable `SEMILLA_NO_CHECK`")),
    ];
    for (args, env, line) in cases {
        let (code, stdout, stderr) = semilla(&repo, &driver(), args, env);
        assert_eq!(code, Some(2), "{args:?} {env:?}\n{stdout}\n{stderr}");
        assert!(stdout.is_empty() && stderr.starts_with(&line) && stderr.trim_end().ends_with("Remove it.") && stderr.trim_end().lines().count() == 1, "{args:?} {env:?}\n{stderr}");
    }
    // The command did nothing: no session file, so no step is stored.
    assert!(!repo.join(".semilla/session.json").exists());
    // The MCP tools: the arguments `compiled` and `no_check` are errors of the tool.
    let mut child = Command::new(env!("CARGO_BIN_EXE_semilla"))
        .current_dir(&repo)
        .arg("mcp")
        .env("SEMILLA_MIR_DRIVER", driver())
        .env("SEMILLA_CACHE_DIR", dir.path().join("cache"))
        .env_remove("SEMILLA_COMPILED")
        .env_remove("SEMILLA_NO_CHECK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::{BufRead, Write};
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut ask = |message: serde_json::Value| -> serde_json::Value {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    ask(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}));
    for (id, (tool, key)) in [("step", "compiled"), ("step", "no_check"), ("tag", "no_check"), ("replay_apply", "no_check")].into_iter().enumerate() {
        let answer = ask(serde_json::json!({"jsonrpc": "2.0", "id": id + 2, "method": "tools/call", "params": {"name": tool, "arguments": {"intent": "raise the limit", key: true}}}));
        let text = answer["result"]["content"][0]["text"].as_str().unwrap_or_default();
        assert!(answer["result"]["isError"] == true && text.contains(&format!("the argument `{key}` is gone (design 83)")), "{tool} {key}: {answer}");
    }
    drop(stdin);
    child.wait().unwrap();
    assert!(!repo.join(".semilla/session.json").exists());
}

// rule- R83.1-1
#[test]
fn an_empty_value_or_zero_of_a_removed_variable_is_no_error() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), LIMIT, &LIMIT.replace("10", "20"));
    let (code, stdout, stderr) = semilla(&repo, &driver(), &["step", "raise the limit"], &[("SEMILLA_COMPILED", "0"), ("SEMILLA_NO_CHECK", "")]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    // One mode: the step has the build check of the compiled build, and a compare result.
    assert!(stdout.contains("build check: passes (the build of the compiled check)") && stdout.contains("changed literal `10` → `20` at `depot::limit/tail` [semantics · code different]"), "{stdout}");
    assert!(!stderr.contains("is gone"), "{stderr}");
}

/// A generic constant that the compiler does not evaluate here: its value has a digest of its
/// definition (design 66.3), with the declaration of the type `Bucket`.
fn capacity(divisor: &str, repr: &str) -> String {
    format!(
        "{repr}pub struct Bucket<K, V> {{\n    pub hash: u64,\n    pub key: K,\n    pub value: V,\n}}\n\npub struct Core<K, V> {{\n    pub entries: Vec<Bucket<K, V>>,\n}}\n\nimpl<K, V> Core<K, V> {{\n    const MAX: usize = (isize::MAX as usize) / {divisor}core::mem::size_of::<Bucket<K, V>>();\n\n    pub fn room(&self) -> usize {{\n        Self::MAX - self.entries.len()\n    }}\n}}\n"
    )
}

/// The `definition #…` texts of an output.
fn digests(output: &str) -> Vec<String> {
    output.match_indices("definition #").map(|(at, _)| output[at..].chars().take("definition #".len() + 16).collect()).collect()
}

// rule+ R83.2-1
#[test]
fn a_digest_does_not_depend_on_the_folder_of_the_binary_or_of_the_build() {
    let dir = common::TestDir::new();
    let old = capacity("", "");
    let new = capacity("2 / ", "");
    let mut outputs = Vec::new();
    for name in ["one", "a-second-folder-with-a-longer-name"] {
        // Each run has its own copy of the driver in its own folder, and its own repository and
        // cache folder: the build folder is in the cache.
        let folder = dir.path().join(name);
        let bin = folder.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let copy = bin.join("semilla-mir-driver");
        std::fs::copy(driver(), &copy).unwrap();
        let repo = repository(&folder, &old, &new);
        let (code, stdout, stderr) = semilla(&repo, &copy, &["step", "halve the capacity"], &[]);
        assert_eq!(code, Some(0), "{stdout}\n{stderr}");
        assert!(digests(&stdout).len() >= 2, "the output has the digests of the constant:\n{stdout}");
        // The commits of the two repositories have different times, so the lines with the base
        // commit and with the trees differ.
        let lines: Vec<&str> = stdout.lines().filter(|line| !line.starts_with("base `") && !line.trim_start().starts_with("hunks: ")).collect();
        outputs.push(lines.join("\n").replace(&folder.to_string_lossy().to_string(), "<FOLDER>"));
    }
    assert_eq!(digests(&outputs[0]), digests(&outputs[1]));
    assert_eq!(outputs[0], outputs[1]);
}

// rule- R83.2-1
#[test]
fn a_digest_keeps_the_repr_that_the_source_gives() {
    // Only the `repr` of the type changes: the declaration is a part of the digest, so the value
    // of the constant changes.
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), &capacity("", ""), &capacity("", "#[repr(C)]\n"));
    let (code, stdout, stderr) = semilla(&repo, &driver(), &["step", "give the bucket the layout of C", "--reason", "depot::Bucket=the layout of C"], &[]);
    assert!(code.is_some_and(|code| code <= 1), "{stdout}\n{stderr}");
    let found = digests(&stdout);
    assert!(found.len() >= 2 && found[0] != found[1], "the digest of the constant changes with the repr:\n{stdout}");
}
