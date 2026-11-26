//! The helpers of the tests that read the suggested tests of a step (design 75) and run them on
//! fixtures: the test suite of Semilla runs them, never the product.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

/// The manifest of a fixture crate `depot`.
pub const MANIFEST: &str = "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n";

/// One suggested test of the output: its name, the function of its module, its cargo flags, and
/// its text.
#[derive(Clone, Debug)]
pub struct Suggested {
    pub name: String,
    pub function: String,
    /// The cargo flags of each configuration where the test applies (empty: `dev`).
    pub flags: Vec<String>,
    pub text: String,
}

/// The suggested tests of the part `suggested tests:` of a step output.
pub fn suggested(output: &str) -> Vec<Suggested> {
    let mut found: Vec<Suggested> = Vec::new();
    let mut inside: Option<usize> = None;
    let mut code = 0;
    for line in output.lines() {
        let indent = line.len() - line.trim_start().len();
        if line.trim_start().starts_with("suggested tests: ") {
            inside = Some(indent);
            continue;
        }
        let Some(start) = inside else { continue };
        if line.trim().is_empty() {
            if let Some(last) = found.last_mut() {
                last.text.push('\n');
            }
            continue;
        }
        if indent <= start {
            inside = None;
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix('`').filter(|rest| rest.starts_with("semilla_test_")) {
            let name = rest.split('`').next().unwrap_or_default().to_string();
            let function = rest.split("in the module of `").nth(1).and_then(|rest| rest.split('`').next()).unwrap_or_default().to_string();
            // The command of each configuration: `run it: `…`` and `also: `…``.
            let commands: Vec<&str> = rest.split(": `").skip(1).filter_map(|part| part.split('`').next()).filter(|command| command.starts_with("cargo test")).collect();
            let flags = commands.iter().map(|command| command.split_whitespace().filter(|word| word.starts_with("--") || word.contains(',')).collect::<Vec<_>>().join(" ")).collect();
            found.push(Suggested { name, function, flags, text: String::new() });
            code = indent + 2;
        } else if let Some(last) = found.last_mut() {
            last.text.push_str(line.get(code..).unwrap_or(line.trim_start()));
            last.text.push('\n');
        }
    }
    found
}

/// [R75.1-1] Adds each test to its module (the crate root, or the block of an inline module with
/// the marker comment `// tests of mod NAME`), runs `cargo test` with the flags of each test, and
/// gives the name of each test that passes.
pub fn passing(dir: &Path, label: &str, source: &str, tests: &[Suggested]) -> Vec<String> {
    let crate_dir = dir.join(format!("tree-{label}"));
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(crate_dir.join("Cargo.toml"), MANIFEST).unwrap();
    let mut text = source.to_string();
    for test in tests {
        let module = test.function.trim_start_matches('<').split("::").nth(1).filter(|part| test.function.split("::").count() > 2 && source.contains(&format!("// tests of mod {part}")));
        match module {
            Some(name) => text = text.replace(&format!("// tests of mod {name}"), &format!("// tests of mod {name}\n{}", test.text)),
            None => text.push_str(&format!("\n{}", test.text)),
        }
    }
    std::fs::write(crate_dir.join("src/lib.rs"), &text).unwrap();
    let mut flags: Vec<String> = tests.iter().flat_map(|test| test.flags.clone()).collect();
    flags.sort();
    flags.dedup();
    // A test passes when it passes in each configuration of its result.
    let mut runs: Vec<(String, bool)> = Vec::new();
    for flag in flags {
        let mut command = Command::new("cargo");
        command.current_dir(&crate_dir).args(["test", "--offline", "--lib"]).args(flag.split_whitespace()).arg("--target-dir").arg(dir.join("target"));
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("test result"), "the suggested tests do not build:\n{}\n{text}", String::from_utf8_lossy(&output.stderr));
        for test in tests.iter().filter(|test| test.flags.contains(&flag)) {
            // A test in an inline module has the path of the module first.
            let passed = stdout.lines().any(|line| line.starts_with("test ") && line.contains(&format!("{}::", test.name)) && line.ends_with(" ok"));
            runs.push((test.name.clone(), passed));
        }
    }
    let mut ok: Vec<String> = Vec::new();
    for test in tests {
        if runs.iter().filter(|(name, _)| *name == test.name).all(|(_, passed)| *passed) && runs.iter().any(|(name, _)| *name == test.name) {
            ok.push(test.name.clone());
        }
    }
    ok
}

