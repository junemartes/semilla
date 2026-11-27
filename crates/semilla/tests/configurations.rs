//! Design 85 (batch 14c): one build configuration by default. A step checks `dev` with the
//! default features; the options of the command line, the arguments of the MCP tools, and the
//! setting `configurations` of the manifest give other lists. The list is data of the step.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// A manifest with the feature `fast` (not a default feature) and the setting `extra`.
fn manifest(extra: &str) -> String {
    format!("[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\ndefault = []\nfast = []\n{extra}")
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

/// A repository with `old` as the commit and `new` in the working tree (the file `src/lib.rs`).
fn repository(folder: &Path, manifest: &str, old: &str, new: &str) -> PathBuf {
    let repo = folder.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("Cargo.toml"), manifest).unwrap();
    std::fs::write(repo.join("src/lib.rs"), old).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "old"]);
    std::fs::write(repo.join("src/lib.rs"), new).unwrap();
    repo
}

fn command(repo: &Path) -> Command {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver")).env("SEMILLA_CACHE_DIR", repo.parent().unwrap().join("cache")).env("SEMILLA_NO_WORKER", "1");
    command.env_remove("SEMILLA_COMPILED").env_remove("SEMILLA_NO_CHECK");
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
    command
}

/// Runs semilla. Gives the exit code, stdout, and stderr.
fn semilla(repo: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let output = command(repo).args(args).output().unwrap();
    (output.status.code(), String::from_utf8_lossy(&output.stdout).into_owned(), String::from_utf8_lossy(&output.stderr).into_owned())
}

/// The line of the checked configurations of an output.
fn checked_line(text: &str) -> String {
    // A line of the Markdown report starts with `> `.
    text.lines().map(|line| line.trim().trim_start_matches("> ")).find(|line| line.starts_with("checked configurations (target `")).map(|line| line.split_once("): ").map(|(_, list)| list.to_string()).unwrap_or_default()).unwrap_or_default()
}

const OLD: &str = "pub fn limit() -> u32 {\n    10\n}\n\n#[cfg(feature = \"fast\")]\npub fn fast() -> u32 {\n    1\n}\n";

fn new_code() -> String {
    OLD.replace("10", "20").replace("    1\n", "    2\n")
}

// rule+ R85.2-2
#[test]
fn a_feature_that_no_tree_has_is_one_error_line() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), &manifest(""), OLD, &new_code());
    for args in [&["step", "the change", "--features", "nosuch"][..], &["step", "the change", "--configuration", "debug"], &["step", "the change", "--all-features", "--features", "fast"]] {
        let (code, stdout, stderr) = semilla(&repo, args);
        assert_eq!(code, Some(2), "{args:?}\n{stdout}\n{stderr}");
        assert!(stderr.starts_with("error: ") && stderr.trim_end().lines().count() == 1, "{args:?}\n{stderr}");
        assert!(stderr.contains("`dev`") && (stderr.contains("`nosuch`") || stderr.contains("`debug`") || stderr.contains("`--all-features`")), "{args:?}\n{stderr}");
    }
    let (_, _, stderr) = semilla(&repo, &["step", "the change", "--features", "nosuch"]);
    assert!(stderr.contains("the features: `fast`") && stderr.contains("`release, all features`"), "{stderr}");
    // The command did nothing: no step is stored.
    let session = std::fs::read_to_string(repo.join(".semilla/session.json")).unwrap_or_default();
    assert!(!session.contains("\"tree_after\""), "{session}");
    // A command that takes no list: one error line.
    let (code, _, stderr) = semilla(&repo, &["undo", "--release"]);
    assert_eq!(code, Some(2), "{stderr}");
}

// rule- R85.2-2
// rule+ R85.4-1
// rule+ R85.4-2
// rule+ R85.4-3
// rule+ R85.7-1
#[test]
fn the_default_list_is_dev_and_each_line_of_code_that_it_does_not_compile_names_the_option() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), &manifest(""), OLD, &new_code());
    let (code, stdout, stderr) = semilla(&repo, &["step", "the change"]);
    assert_eq!(code, Some(1), "{stdout}\n{stderr}");
    assert_eq!(checked_line(&stdout), "`dev`", "{stdout}");
    assert!(stdout.contains("no checked configuration compiles the changed code (`#[cfg(feature = \"fast\")]` at src/lib.rs:5; `--features fast` checks it)"), "{stdout}");
    assert!(stdout.contains("note: to check the changed code that no checked configuration compiles, add `--features fast` to the command (or `--configuration full`)"), "{stdout}");
    // The option of the line checks the code: the step has no line of code that is not compiled.
    let (code, stdout, stderr) = semilla(&repo, &["step", "the change", "--features", "fast"]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    assert_eq!(checked_line(&stdout), "`dev, features fast`", "{stdout}");
    assert!(!stdout.contains("no checked configuration compiles") && !stdout.contains("checks it"), "{stdout}");
}

// rule- R85.4-1
// rule- R85.4-2
// rule- R85.4-3
// rule+ R85.6-1
#[test]
fn the_full_list_has_the_lines_of_batch_14b() {
    let dir = common::TestDir::new();
    let old = "pub fn limit() -> u32 {\n    10\n}\n\n#[cfg(target_os = \"none_such_os\")]\npub fn there() -> u32 {\n    1\n}\n";
    let repo = repository(dir.path(), &manifest(""), old, &old.replace("10", "20").replace("    1\n", "    2\n"));
    let (code, stdout, stderr) = semilla(&repo, &["step", "the change", "--configuration", "full"]);
    assert_eq!(code, Some(1), "{stdout}\n{stderr}");
    // No line of the checked configurations in the step output, and no option in a line.
    assert_eq!(checked_line(&stdout), "", "{stdout}");
    assert!(stdout.contains("(`#[cfg(target_os = \"none_such_os\")]` at src/lib.rs:5), so Semilla") && !stdout.contains("checks it") && !stdout.contains("not on this target"), "{stdout}");
    assert!(stdout.contains("run again: semilla step 'the change' --reason 'depot::there=<why>' --configuration full"), "{stdout}");
    // The default list on the same change: the line says what checks the code.
    let (_, stdout, _) = semilla(&repo, &["step", "the change"]);
    assert!(stdout.contains("at src/lib.rs:5; not on this target), so Semilla"), "{stdout}");
}

// rule+ R85.3-1
#[test]
fn a_stored_step_keeps_its_list_for_each_later_check() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), &manifest(""), OLD, &OLD.replace("10", "20"));
    let (code, stdout, stderr) = semilla(&repo, &["step", "the change", "--release"]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    assert_eq!(checked_line(&stdout), "`release`", "{stdout}");
    let session = std::fs::read_to_string(repo.join(".semilla/session.json")).unwrap();
    assert!(session.contains("\"configurations\"") && session.contains("\"release\""), "{session}");
    for args in [&["status", "--verbose"][..], &["verify"], &["review", "--md", "../report.md"]] {
        let (code, stdout, stderr) = semilla(&repo, args);
        assert!(code == Some(0), "{args:?}\n{stdout}\n{stderr}");
        let text = if args[0] == "review" { std::fs::read_to_string(dir.path().join("report.md")).unwrap() } else { stdout.clone() };
        assert_eq!(checked_line(&text), "`release`", "{args:?}\n{text}");
        assert!(!text.contains("replace the list of each step"), "{args:?}\n{text}");
    }
}

// rule- R85.3-1
// rule+ R85.3-2
#[test]
fn a_step_of_an_earlier_version_has_the_full_list_and_an_option_of_a_reviewer_replaces_the_list() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), &manifest(""), OLD, &OLD.replace("10", "20"));
    let (code, stdout, stderr) = semilla(&repo, &["step", "the change"]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    // A reviewer command with an option checks with the list of the option, and says so.
    let (code, stdout, stderr) = semilla(&repo, &["verify", "--configuration", "release, all features"]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    assert_eq!(checked_line(&stdout), "`release, all features`", "{stdout}");
    assert!(stdout.contains("! the configurations of the option (`release, all features`) replace the list of each step for this check"), "{stdout}");
    // The stored step keeps its own list.
    let (_, stdout, _) = semilla(&repo, &["verify"]);
    assert_eq!(checked_line(&stdout), "`dev`", "{stdout}");
    // A record of an earlier version has no list: the full list.
    let path = repo.join(".semilla/session.json");
    let mut session: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    session["changeset"]["steps"][0].as_object_mut().unwrap().remove("configurations");
    std::fs::write(&path, serde_json::to_string_pretty(&session).unwrap()).unwrap();
    let (_, stdout, _) = semilla(&repo, &["verify"]);
    let list = checked_line(&stdout);
    assert!(list.contains("`dev`") && list.contains("`release`") && list.contains("`dev, all features`") && list.contains("`release, no default features`"), "{stdout}");
}

// rule- R85.3-2
#[test]
fn an_option_of_a_reviewer_that_is_the_list_of_the_step_has_no_line() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), &manifest(""), OLD, &OLD.replace("10", "20"));
    let (code, _, stderr) = semilla(&repo, &["step", "the change", "--configuration", "release"]);
    assert_eq!(code, Some(0), "{stderr}");
    let (_, stdout, _) = semilla(&repo, &["verify", "--release"]);
    assert_eq!(checked_line(&stdout), "`release`", "{stdout}");
    assert!(!stdout.contains("replace the list of each step"), "{stdout}");
}

// rule+ R85.2-3
// rule- R85.2-3
#[test]
fn the_setting_of_the_manifest_gives_the_default_list_and_an_option_replaces_it() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), &manifest("\n[package.metadata.semilla]\nconfigurations = [\"release\", \"dev, features fast\"]\n"), OLD, &new_code());
    let (code, stdout, stderr) = semilla(&repo, &["step", "the change"]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    assert_eq!(checked_line(&stdout), "`release`, `dev, features fast`", "{stdout}");
    semilla(&repo, &["undo"]);
    let (code, stdout, stderr) = semilla(&repo, &["step", "the change", "--configuration", "dev"]);
    assert_eq!(code, Some(1), "{stdout}\n{stderr}");
    assert_eq!(checked_line(&stdout), "`dev`", "{stdout}");
}

// rule+ R85.8-1
// rule- R85.8-1
#[test]
fn no_configuration_of_the_list_builds_before_the_step() {
    let dir = common::TestDir::new();
    let old = "#[cfg(not(feature = \"fast\"))]\ncompile_error!(\"the crate needs the feature `fast`\");\n\npub fn limit() -> u32 {\n    10\n}\n";
    let repo = repository(dir.path(), &manifest(""), old, &old.replace("10", "20"));
    let (code, stdout, stderr) = semilla(&repo, &["step", "the change"]);
    assert_ne!(code, Some(0), "{stdout}\n{stderr}");
    let text = format!("{stdout}{stderr}");
    assert!(text.contains("no configuration of the list of the step builds before the step (`dev`: ") && text.contains("give `--configuration NAME`") && text.contains("`dev, all features`"), "{text}");
    let (code, stdout, stderr) = semilla(&repo, &["step", "the change", "--features", "fast"]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    assert_eq!(checked_line(&stdout), "`dev, features fast`", "{stdout}");
}

// rule+ R85.2-4
// rule- R85.2-4
#[test]
fn the_mcp_tools_take_the_list_as_arguments() {
    let dir = common::TestDir::new();
    let repo = repository(dir.path(), &manifest(""), OLD, &OLD.replace("10", "20"));
    let mut child = command(&repo).arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
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
    let bad = ask(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "step", "arguments": {"intent": "the change", "configurations": ["debug"]}}}));
    assert!(bad["result"]["isError"] == true && bad["result"]["content"][0]["text"].as_str().unwrap_or_default().contains("`debug` is not the name of a configuration"), "{bad}");
    let good = ask(serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "step", "arguments": {"intent": "the change", "release": true}}}));
    let text = good["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string();
    assert_eq!(checked_line(&text), "`release`", "{good}");
    let verify = ask(serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "verify", "arguments": {}}}));
    assert_eq!(checked_line(verify["result"]["content"][0]["text"].as_str().unwrap_or_default()), "`release`", "{verify}");
    drop(stdin);
    child.wait().unwrap();
}

// rule+ R85.5-1
// rule- R85.5-1
#[test]
fn a_list_that_is_not_the_full_list_adds_no_configuration_for_a_joint_state() {
    let dir = common::TestDir::new();
    let alias = |name: &str, feature: &str| format!("#[cfg(feature = \"{feature}\")]\ntype {name} = u8;\n#[cfg(not(feature = \"{feature}\"))]\ntype {name} = u64;\n");
    let base = format!("{}{}pub fn cut(a: u64) -> u64 {{ a as TA as TB as u64 }}\n", alias("TA", "fast"), alias("TB", "slow"));
    // False in the build with only `fast`: `a as u8 as u64` is not `a as u64`.
    let drop = base.replace("a as TA as TB as u64", "a as TB as u64");
    let manifest = "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nfast = []\nslow = []\n";
    let repo = repository(dir.path(), manifest, &base, &drop);
    // The list covers each condition (on in `dev, all features`, off in `dev`), and no build of
    // it has `fast` on with `slow` off. Semilla adds no configuration: the note names the option.
    let (code, stdout, stderr) = semilla(&repo, &["step", "probe", "--configuration", "dev", "--all-features"]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    assert!(!stdout.contains("Semilla added") && stdout.contains("not checked together: `feature = \"fast\"` on with `feature = \"slow\"` off. To check it: `--features fast`"), "{stdout}");
    assert!(!repo.parent().unwrap().join("cache").read_dir().map(|mut dir| dir.any(|entry| entry.is_ok_and(|entry| entry.path().join("added-configurations.json").exists()))).unwrap_or(false));
    semilla(&repo, &["undo"]);
    // The full list adds the configurations of the joint state (batch 5u).
    let (code, stdout, stderr) = semilla(&repo, &["step", "probe", "--configuration", "full"]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    assert!(stdout.contains("Semilla added the configurations `dev, features fast`"), "{stdout}");
}

/// A repository with an optional path dependency `helper` and the feature `fast`. `new` is the
/// manifest of the working tree.
fn with_helper(folder: &Path, old: &str, new: &str) -> PathBuf {
    let repo = repository(folder, old, OLD, OLD);
    std::fs::create_dir_all(repo.join("helper/src")).unwrap();
    std::fs::write(repo.join("helper/Cargo.toml"), "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\ndefault = []\nextra = []\n").unwrap();
    std::fs::write(repo.join("helper/src/lib.rs"), "pub fn one() -> u32 {\n    1\n}\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "helper"]);
    std::fs::write(repo.join("Cargo.toml"), new).unwrap();
    repo
}

const HELPER: &str = "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\ndefault = []\nfast = []\n\n[dependencies]\nhelper = { path = \"helper\", optional = true }\n";

// rule+ R85.4-4
#[test]
fn a_dependency_that_only_a_configuration_outside_the_list_uses_names_the_option() {
    let dir = common::TestDir::new();
    let new = HELPER.replace("optional = true }", "optional = true, features = [\"extra\"] }");
    let repo = with_helper(dir.path(), HELPER, &new);
    let (code, stdout, stderr) = semilla(&repo, &["step", "the change"]);
    assert!(code == Some(0) || code == Some(1), "{stdout}\n{stderr}");
    let fact = stdout.lines().find(|line| line.contains("Cargo.toml: dependencies.helper.features")).unwrap_or_else(|| panic!("{stdout}"));
    assert!(fact.contains("[manifest · no compare]") && !fact.contains("no code"), "{stdout}");
    assert!(stdout.contains("note: no compare: no checked configuration uses the dependency `helper`; `--features helper` checks it"), "{stdout}");
    // The line of the features that no checked configuration enables names the options.
    assert!(stdout.contains("`depot/fast` (`--features fast` checks it)"), "{stdout}");
}

// rule- R85.4-4
#[test]
fn a_dependency_that_a_checked_configuration_uses_keeps_its_result() {
    let dir = common::TestDir::new();
    let new = HELPER.replace("optional = true }", "optional = true, features = [\"extra\"] }");
    let repo = with_helper(dir.path(), HELPER, &new);
    let (_, stdout, stderr) = semilla(&repo, &["step", "the change", "--features", "helper"]);
    assert!(!stdout.contains("no checked configuration uses"), "{stdout}\n{stderr}");
    let fact = stdout.lines().find(|line| line.contains("Cargo.toml: dependencies.helper.features")).unwrap_or_else(|| panic!("{stdout}"));
    assert!(!fact.contains("no compare]"), "{stdout}");
    // The full list keeps the result of batch 14b.
    let (_, full, _) = semilla(&repo, &["step", "the change", "--configuration", "full"]);
    assert!(!full.contains("no checked configuration uses") && !full.contains("checks it)"), "{full}");
}
