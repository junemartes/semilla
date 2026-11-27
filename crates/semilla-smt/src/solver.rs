//! The interface to the SMT solver process.
//!
//! The crate sends SMT-LIB2 text to a `z3 -in -smt2` process through a pipe.
//! Each query has a time limit and a fixed random seed. A cache keeps the
//! definite answers (`sat` and `unsat`) by the query text.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

/// The environment variable that names the solver binary.
pub const SOLVER_ENV: &str = "SEMILLA_Z3";

/// The path of the solver binary under a project directory.
pub const TOOLS_PATH: &str = ".semilla/tools/z3";

/// The default safety time limit of one query ([R77.2-1]: the result of a query comes from its
/// limit in solver steps, `crate::budget::QUERY_STEPS`; the time limit is only a safety stop).
pub const DEFAULT_TIMEOUT: Duration = crate::budget::SAFETY_QUERY;

/// The default random seed.
pub const DEFAULT_SEED: u32 = 0;

/// An SMT solver process (Z3).
#[derive(Debug)]
pub struct Solver {
    path: PathBuf,
    version: String,
    timeout: Duration,
    /// [R77.2-1] The most solver steps of one query (the resource limit of Z3).
    steps: u64,
    seed: u32,
    /// The answer and the solver steps of each query text.
    cache: Mutex<HashMap<String, (Answer, u64)>>,
}

/// The answer to one `check-sat` query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// The assertions are satisfiable. The model maps each requested symbol to its value text.
    Sat(Vec<(String, String)>),
    Unsat,
    /// The solver gave `unknown` or did not answer in time. The text gives the reason.
    Unknown(String),
}

impl Solver {
    /// Finds the solver binary for a tool that checks no repository (the tests of this crate).
    /// See [`Solver::find_for`].
    pub fn find() -> Result<Solver> {
        Solver::find_for(&[], None)
    }

    /// Finds the solver binary. The search order is:
    ///
    /// 1. The `SEMILLA_Z3` environment variable.
    /// 2. `z3` in each folder of `tool_dirs` (the caller gives the folder next to the Semilla
    ///    binary and the cache folder of the user).
    /// 3. `z3` on `PATH` (only an absolute folder).
    /// 4. `.semilla/tools/z3` of the semilla workspace that built this crate (where
    ///    `scripts/fetch-z3.sh` installs it). This path is fixed at build time.
    ///
    /// `repository` is the folder of the repository that Semilla checks. The agent controls each
    /// file there (invariant I5). So the search never takes a solver from that folder: a
    /// candidate of step 2, 3, or 4 in the folder is skipped. The search does not look at the
    /// current folder or its parents.
    pub fn find_for(tool_dirs: &[PathBuf], repository: Option<&Path>) -> Result<Solver> {
        let own = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(TOOLS_PATH);
        Solver::at(locate(std::env::var_os(SOLVER_ENV), tool_dirs, std::env::var_os("PATH"), Some(&own), repository)?)
    }

    /// [R84.7-1] The path of the solver binary by the search of [`Solver::find_for`], with no run
    /// of the binary: a step with no claim of an intent file starts no solver process (design
    /// 84), and the key of a stored result names the solver by its path.
    pub fn locate_for(tool_dirs: &[PathBuf], repository: Option<&Path>) -> Option<PathBuf> {
        let own = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(TOOLS_PATH);
        locate(std::env::var_os(SOLVER_ENV), tool_dirs, std::env::var_os("PATH"), Some(&own), repository).ok()
    }

    /// Returns true if [`Solver::find`] finds a solver that runs.
    pub fn available() -> bool {
        Solver::find().is_ok()
    }

    /// Uses the solver binary at `path`. The function makes sure that the binary runs.
    pub fn at(path: impl Into<PathBuf>) -> Result<Solver> {
        let path = path.into();
        let output = Command::new(&path)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("cannot run the SMT solver at {}", path.display()))?;
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || !version.contains("Z3") {
            bail!(
                "the program at {} is not a Z3 solver (it gave {:?} for --version)",
                path.display(),
                version
            );
        }
        Ok(Solver {
            path,
            version,
            timeout: DEFAULT_TIMEOUT,
            steps: crate::budget::QUERY_STEPS,
            seed: DEFAULT_SEED,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Sets the safety time limit of one query.
    pub fn with_timeout(mut self, timeout: Duration) -> Solver {
        self.timeout = timeout;
        self
    }

    /// [R77.2-1] Sets the most solver steps of one query.
    pub fn with_steps(mut self, steps: u64) -> Solver {
        self.steps = steps.max(1);
        self
    }

    /// The most solver steps of one query.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// Sets the random seed of the solver.
    pub fn with_seed(mut self, seed: u32) -> Solver {
        self.seed = seed;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The output of `z3 --version`, for example `Z3 version 4.15.4 - 64 bit`.
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn seed(&self) -> u32 {
        self.seed
    }

    /// The header that each query starts with: the limit in solver steps (`rlimit`), the safety
    /// time limit, and the fixed seeds.
    pub(crate) fn header(&self, steps: u64) -> String {
        let millis = self.timeout.as_millis().max(1);
        format!(
            "(set-option :produce-models true)\n\
             (set-option :rlimit {steps})\n\
             (set-option :timeout {millis})\n\
             (set-option :smt.random_seed {seed})\n\
             (set-option :sat.random_seed {seed})\n",
            seed = self.seed
        )
    }

    /// Runs one query. The query must have exactly one `check-sat`.
    /// `symbols` are the constants to read from the model when the answer is `sat`.
    ///
    /// [R77.2-1] The query has the smaller of the limit of one query and the rest of the budget of
    /// the check of this thread (`crate::budget`). Its solver steps count in that budget, also for
    /// an answer from the cache (so the count does not depend on the order of the threads). A
    /// query that reaches the rest of the budget gives `Unknown(BUDGET_CAUSE)`; a query that
    /// the safety time limit stops gives `Unknown(SAFETY_CAUSE)`.
    pub(crate) fn check(&self, body: &str, symbols: &[String]) -> Result<(String, Answer)> {
        use crate::budget;
        let rest = budget::rest();
        if let Some(cause) = budget::stop() {
            return Ok((String::new(), Answer::Unknown(cause.into())));
        }
        let steps = rest.map(|rest| rest.min(self.steps)).unwrap_or(self.steps).max(1);
        let by_budget = steps < self.steps;
        let mut query = self.header(steps);
        query.push_str(body);
        query.push_str("(check-sat)\n(get-info :reason-unknown)\n(get-info :rlimit)\n");
        if !symbols.is_empty() {
            query.push_str(&format!("(get-value ({}))\n", symbols.join(" ")));
        }
        let cached = self.cache.lock().unwrap().get(&query).cloned();
        let (answer, cost) = match cached {
            Some(found) => found,
            None => {
                let started = Instant::now();
                let (answer, cost) = self.run(&query)?;
                // [R77.2-1] Z3 gives the reason "canceled" for each of its two stops, so the counts
                // decide: a count at the limit is the step limit; a time at the time limit is the
                // safety stop.
                let answer = match answer {
                    Answer::Unknown(reason) if reason != budget::SAFETY_CAUSE && cost >= steps => {
                        let _ = reason;
                        Answer::Unknown(budget::QUERY_LIMIT_CAUSE.into())
                    }
                    Answer::Unknown(reason) if reason == budget::QUERY_LIMIT_CAUSE || reason == "canceled" || reason == "timeout" => {
                        if started.elapsed() >= self.timeout { Answer::Unknown(budget::SAFETY_CAUSE.into()) } else { Answer::Unknown(reason) }
                    }
                    other => other,
                };
                if !matches!(&answer, Answer::Unknown(reason) if reason == budget::SAFETY_CAUSE) {
                    self.cache.lock().unwrap().insert(query.clone(), (answer.clone(), cost));
                }
                (answer, cost)
            }
        };
        budget::charge(cost);
        let answer = match answer {
            Answer::Unknown(reason) if reason == budget::QUERY_LIMIT_CAUSE && by_budget => Answer::Unknown(budget::BUDGET_CAUSE.into()),
            other => other,
        };
        Ok((query, answer))
    }

    fn run(&self, query: &str) -> Result<(Answer, u64)> {
        let mut child = Command::new(&self.path)
            .args(["-in", "-smt2"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot start the SMT solver at {}", self.path.display()))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stdout.read_to_string(&mut text);
            text
        });
        stdin
            .write_all(query.as_bytes())
            .context("cannot write the query to the SMT solver")?;
        drop(stdin);

        // The solver stops itself at the soft time limit. Kill it if it does not stop.
        let deadline = Instant::now() + self.timeout * 2 + Duration::from_secs(1);
        let mut killed = false;
        loop {
            if child.try_wait()?.is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                killed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let text = reader
            .join()
            .map_err(|_| anyhow!("the solver output thread failed"))?;
        if killed {
            return Ok((Answer::Unknown(crate::budget::SAFETY_CAUSE.into()), 0));
        }
        let cost = rlimit_count(&text);
        let answer = match parse_output(&text, query)? {
            Answer::Unknown(reason) if reason.contains("resource limit") || reason.contains("rlimit") => Answer::Unknown(crate::budget::QUERY_LIMIT_CAUSE.into()),
            other => other,
        };
        Ok((answer, cost))
    }
}

/// Finds the solver binary. See [`Solver::find_for`].
pub(crate) fn locate(
    env_value: Option<OsString>,
    tool_dirs: &[PathBuf],
    path_var: Option<OsString>,
    build_time: Option<&Path>,
    repository: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(value) = env_value.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(&value);
        if is_executable(&path) {
            return Ok(path);
        }
        bail!(
            "{SOLVER_ENV} is set to {}, but that is not an executable file",
            path.display()
        );
    }
    // A candidate in the checked repository is never the solver: the agent can write it.
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let repository = repository.map(real);
    let allowed = |candidate: &Path| is_executable(candidate) && repository.as_ref().is_none_or(|repository| !real(candidate).starts_with(repository));
    let on_path = path_var.iter().flat_map(std::env::split_paths).filter(|dir| dir.is_absolute()).collect::<Vec<_>>();
    let candidates = tool_dirs.iter().chain(&on_path).map(|dir| dir.join("z3")).chain(build_time.map(Path::to_path_buf));
    for candidate in candidates {
        if allowed(&candidate) {
            return Ok(candidate);
        }
    }
    bail!(
        "no Z3 SMT solver found. Set {SOLVER_ENV} to the path of a z3 binary, \
         or run scripts/fetch-z3.sh to install one, or put z3 on PATH. \
         Semilla never takes a solver from the repository that it checks"
    )
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// [R77.2-1] The solver steps of one query: the value of `(get-info :rlimit)` in the output.
fn rlimit_count(text: &str) -> u64 {
    let Some(at) = text.find("(:rlimit ") else { return 0 };
    text[at + "(:rlimit ".len()..].chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)
}

/// Reads the output of one query.
fn parse_output(text: &str, query: &str) -> Result<Answer> {
    let exprs = sexp::parse_all(text);
    let mut items = exprs.iter();
    let first = items
        .next()
        .ok_or_else(|| anyhow!("the SMT solver gave no answer to the query:\n{query}"))?;
    match first {
        sexp::Sexp::Atom(word) if word == "sat" => {
            let mut model = Vec::new();
            for item in items {
                if let Some(pairs) = model_pairs(item) {
                    model = pairs;
                }
            }
            Ok(Answer::Sat(model))
        }
        sexp::Sexp::Atom(word) if word == "unsat" => Ok(Answer::Unsat),
        sexp::Sexp::Atom(word) if word == "unknown" => {
            let mut reason = String::from("unknown");
            for item in items {
                if let sexp::Sexp::List(list) = item
                    && list.len() == 2
                    && list[0] == sexp::Sexp::Atom(":reason-unknown".into())
                {
                    reason = list[1].to_string().trim_matches('"').to_string();
                }
            }
            Ok(Answer::Unknown(reason))
        }
        other => Err(anyhow!(
            "the SMT solver did not accept the query: {other}\nquery:\n{query}"
        )),
    }
}

fn model_pairs(item: &sexp::Sexp) -> Option<Vec<(String, String)>> {
    let sexp::Sexp::List(list) = item else {
        return None;
    };
    let mut pairs = Vec::new();
    for entry in list {
        let sexp::Sexp::List(pair) = entry else {
            return None;
        };
        if pair.len() != 2 {
            return None;
        }
        let sexp::Sexp::Atom(name) = &pair[0] else {
            return None;
        };
        pairs.push((name.clone(), pair[1].to_string()));
    }
    Some(pairs)
}

/// A small S-expression reader for the solver output.
pub(crate) mod sexp {
    use std::fmt;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Sexp {
        Atom(String),
        List(Vec<Sexp>),
    }

    impl fmt::Display for Sexp {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Sexp::Atom(atom) => write!(f, "{atom}"),
                Sexp::List(items) => {
                    write!(f, "(")?;
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            write!(f, " ")?;
                        }
                        write!(f, "{item}")?;
                    }
                    write!(f, ")")
                }
            }
        }
    }

    pub fn parse_all(text: &str) -> Vec<Sexp> {
        let tokens = tokenize(text);
        let mut pos = 0;
        let mut out = Vec::new();
        while pos < tokens.len() {
            match parse(&tokens, &mut pos) {
                Some(expr) => out.push(expr),
                None => pos += 1,
            }
        }
        out
    }

    pub fn parse_one(text: &str) -> Option<Sexp> {
        parse_all(text).into_iter().next()
    }

    fn tokenize(text: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut chars = text.chars().peekable();
        while let Some(&c) = chars.peek() {
            match c {
                '(' | ')' => {
                    tokens.push(c.to_string());
                    chars.next();
                }
                '"' => {
                    let mut s = String::from('"');
                    chars.next();
                    while let Some(c) = chars.next() {
                        s.push(c);
                        if c == '"' {
                            // SMT-LIB writes a quote in a string as two quotes.
                            if chars.peek() == Some(&'"') {
                                s.push('"');
                                chars.next();
                                continue;
                            }
                            break;
                        }
                    }
                    tokens.push(s);
                }
                '|' => {
                    let mut s = String::from('|');
                    chars.next();
                    for c in chars.by_ref() {
                        s.push(c);
                        if c == '|' {
                            break;
                        }
                    }
                    tokens.push(s);
                }
                ';' => {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                c if c.is_whitespace() => {
                    chars.next();
                }
                _ => {
                    let mut s = String::new();
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() || c == '(' || c == ')' || c == '"' || c == ';' {
                            break;
                        }
                        s.push(c);
                        chars.next();
                    }
                    tokens.push(s);
                }
            }
        }
        tokens
    }

    fn parse(tokens: &[String], pos: &mut usize) -> Option<Sexp> {
        let token = tokens.get(*pos)?;
        *pos += 1;
        match token.as_str() {
            "(" => {
                let mut items = Vec::new();
                loop {
                    match tokens.get(*pos)?.as_str() {
                        ")" => {
                            *pos += 1;
                            return Some(Sexp::List(items));
                        }
                        _ => items.push(parse(tokens, pos)?),
                    }
                }
            }
            ")" => None,
            _ => Some(Sexp::Atom(token.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn missing_solver_gives_a_clear_error() {
        let error = locate(None, &[], Some(OsStr::new("").to_os_string()), None, None).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("no Z3 SMT solver found"), "{text}");
        assert!(text.contains("SEMILLA_Z3"), "{text}");
        assert!(text.contains("scripts/fetch-z3.sh"), "{text}");
    }

    #[test]
    fn a_bad_env_value_is_an_error() {
        let error = locate(Some("/nonexistent/z3".into()), &[], None, None, None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("SEMILLA_Z3 is set to /nonexistent/z3"),
            "{error}"
        );
    }

    /// Batch 5h, A1: the agent controls the repository folder. A solver there is never used:
    /// not from a tool folder, not from `PATH`, not from the build-time path, and not from the
    /// current folder.
    #[test]
    fn a_solver_in_the_checked_repository_is_never_used() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("semilla-smt-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let program = |path: &Path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "#!/bin/sh\necho unsat\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let repo = base.join("repo");
        let inside = [repo.join(".semilla/tools/z3"), repo.join("bin/z3")];
        inside.iter().for_each(|path| program(path));
        let path_var = std::env::join_paths([repo.join("bin"), PathBuf::from("relative")]).unwrap();
        let tool_dirs = [repo.join(".semilla/tools")];
        let error = locate(None, &tool_dirs, Some(path_var.clone()), Some(&inside[0]), Some(&repo)).unwrap_err().to_string();
        assert!(error.contains("never takes a solver from the repository"), "{error}");
        // A link from outside into the repository is in the repository also.
        std::os::unix::fs::symlink(repo.join("bin"), base.join("link")).unwrap();
        assert!(locate(None, &[base.join("link")], None, None, Some(&repo)).is_err());
        // A solver outside the repository is found: in a tool folder first, then on `PATH`.
        let outside = base.join("cache/tools/z3");
        program(&outside);
        assert_eq!(locate(None, &[base.join("cache/tools")], Some(path_var.clone()), None, Some(&repo)).unwrap(), outside);
        let on_path = std::env::join_paths([repo.join("bin"), base.join("cache/tools")]).unwrap();
        assert_eq!(locate(None, &[], Some(on_path), None, Some(&repo)).unwrap(), outside);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_binary_is_an_error() {
        let error = Solver::at("/nonexistent/z3").unwrap_err().to_string();
        assert!(error.contains("cannot run the SMT solver"), "{error}");
    }

    #[test]
    fn parse_solver_output() {
        let answer = parse_output(
            "sat\n(:reason-unknown \"\")\n((v0 #x00000005)\n (v1 true))\n",
            "",
        )
        .unwrap();
        assert_eq!(
            answer,
            Answer::Sat(vec![
                ("v0".into(), "#x00000005".into()),
                ("v1".into(), "true".into())
            ])
        );
        let answer = parse_output(
            "unknown\n(:reason-unknown \"timeout\")\n(error \"line 7 column 14: model is not available\")\n",
            "",
        )
        .unwrap();
        assert_eq!(answer, Answer::Unknown("timeout".into()));
        assert_eq!(
            parse_output("unsat\n(error \"x\")", "").unwrap(),
            Answer::Unsat
        );
        assert!(parse_output("(error \"bad\")\nsat", "").is_err());
    }
}
