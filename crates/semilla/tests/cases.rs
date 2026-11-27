//! The case set (design 57.3) and the properties of each case (design 57.4).
//!
//! Each file `tests/cases/GROUP/NAME.case` is one small change: the files of the old tree, the
//! files of the new tree, the options of `semilla step`, the checks of the case, and the
//! expected text of the step. The runner makes the two trees, runs the step (one mode, design
//! 83: the build, the MIR, and the build check), and compares the full result with the expected
//! text. Then it checks the
//! properties that hold for each change: the cover, no empty change, the same result each
//! time, the tags, the facts of the agent, the reader, the comments with no edit, and the map
//! of the AST pass (each changed node in exactly one entry, R79.1-1).
//!
//! `SEMILLA_CASES_WRITE=1` writes the expected texts again. `SEMILLA_CASES=TEXT` runs only the
//! cases whose name `GROUP/NAME` has the text.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The names of the checks. A line `known_failure: NAME: CAUSE` of a case names one of them.
const CHECKS: &[&str] = &["expected", "exit", "must_match", "must_not_match", "same_facts", "cover", "empty_change", "same_result", "tags", "agent_facts", "reader", "comments", "equivalence", "after_commit", "full_text", "ast_map", "templates"];

/// A spotlight line of `semilla step` with no reason, in the form of a tag after the commit
/// (design 62): `✗ spotlight…: TEXT; it needs a reason: --reason '…'` gives
/// `! spotlight…: TEXT; open: no reason given`. Each other line stays.
fn open_spotlight(line: &str) -> String {
    let trimmed = line.trim_start();
    match (trimmed.strip_prefix("✗ spotlight"), line.rfind("; it needs a reason: --reason ")) {
        (Some(_), Some(at)) => format!("{}! spotlight{}; open: no reason given", &line[..line.len() - trimmed.len()], &line[line.len() - trimmed.len() + "✗ spotlight".len()..at]),
        _ => line.to_string(),
    }
}

/// The line diff command of design 55.8. The test has its own copy of the options: it does not
/// read them from the product.
const LINE_DIFF: &[&str] = &["diff", "-U0", "--inter-hunk-context=0", "--diff-algorithm=myers", "--indent-heuristic", "--no-renames", "--no-ext-diff", "--no-textconv", "--no-relative", "--text", "--no-color"];

/// One case of the set.
struct Case {
    /// `GROUP/NAME`.
    id: String,
    file: PathBuf,
    options: Vec<String>,
    exit: Option<i32>,
    must_match: Vec<String>,
    must_not_match: Vec<String>,
    /// The texts of the facts whose edit changes the outcome (design 61, property 10): such a
    /// fact never gets "equivalent".
    different: Vec<String>,
    same_facts: bool,
    /// The known failures: the name of the check and the cause.
    known: Vec<(String, String)>,
    old: BTreeMap<String, Vec<u8>>,
    new: BTreeMap<String, Vec<u8>>,
    /// The text of the file before the expected text.
    head: String,
    expected: Option<String>,
}

fn base64(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let (mut bits, mut count) = (0u32, 0);
    for c in text.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let value = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return Err(format!("`{c}` is not a base64 character")),
        };
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Ok(out)
}

impl Case {
    fn parse(id: &str, file: &Path, text: &str) -> Result<Case, String> {
        let mut case = Case { id: id.to_string(), file: file.to_path_buf(), options: Vec::new(), exit: None, must_match: Vec::new(), must_not_match: Vec::new(), different: Vec::new(), same_facts: false, known: Vec::new(), old: BTreeMap::new(), new: BTreeMap::new(), head: String::new(), expected: None };
        const EXPECTED: &str = "=== expected\n";
        let (head, expected) = match text.find(&format!("\n{EXPECTED}")) {
            Some(at) => (&text[..at + 1], Some(text[at + 1 + EXPECTED.len()..].to_string())),
            None => (text, None),
        };
        if !head.is_empty() && !head.ends_with('\n') {
            return Err("the last line before the expected text has no line end".into());
        }
        case.head = head.to_string();
        case.expected = expected;
        // The sections: a line `=== SIDE PATH` and the lines of the file.
        let mut section: Option<(String, String)> = None;
        let close = |case: &mut Case, section: Option<(String, String)>| -> Result<(), String> {
            let Some((title, body)) = section else { return Ok(()) };
            let (side, rest) = title.split_once(' ').ok_or(format!("the section `{title}` has no path"))?;
            let forms = [" (base64)", " (no line end at the end)", " (each line ends with CR LF)"];
            let form = forms.iter().position(|form| rest.ends_with(form));
            let path = form.map_or(rest, |form| &rest[..rest.len() - forms[form].len()]).to_string();
            let bytes = match form {
                Some(0) => base64(&body)?,
                Some(1) => body.strip_suffix('\n').unwrap_or(&body).as_bytes().to_vec(),
                Some(_) => body.replace('\n', "\r\n").into_bytes(),
                None => body.into_bytes(),
            };
            match side {
                "old" => case.old.insert(path, bytes).map(|_| ()),
                "new" => case.new.insert(path, bytes).map(|_| ()),
                "both" => case.old.insert(path.clone(), bytes.clone()).or(case.new.insert(path, bytes)).map(|_| ()),
                _ => return Err(format!("the section `{title}` has no side `old`, `new`, or `both`")),
            }
            .map_or(Ok(()), |()| Err(format!("the section `{title}` names a file two times")))
        };
        for line in head.split_inclusive('\n') {
            if let Some(title) = line.strip_prefix("=== ") {
                close(&mut case, section.take())?;
                section = Some((title.trim_end_matches('\n').to_string(), String::new()));
            } else if let Some((_, body)) = &mut section {
                body.push_str(line);
            } else {
                let line = line.trim_end_matches('\n');
                let (key, value) = line.split_once(": ").ok_or(format!("the line `{line}` has no key"))?;
                match key {
                    "note" => {}
                    // A marker of the rule index (design 57.6). The test of `rules.rs` reads it.
                    "rule+" | "rule-" => {
                        if value.split_whitespace().next().is_none() || !value.split_whitespace().all(|name| name.starts_with('R') && name.contains('-')) {
                            return Err(format!("the marker `{line}` names no identity of a rule"));
                        }
                    }
                    "option" => case.options.push(value.to_string()),
                    "exit" => case.exit = Some(value.parse().map_err(|_| format!("`{value}` is not an exit code"))?),
                    "must_match" => case.must_match.push(value.to_string()),
                    "must_not_match" => case.must_not_match.push(value.to_string()),
                    "different" => case.different.push(value.to_string()),
                    "same_facts_as_no_option" if value == "yes" => case.same_facts = true,
                    "known_failure" => {
                        let (check, cause) = value.split_once(": ").ok_or(format!("the known failure `{value}` has no cause"))?;
                        if !CHECKS.contains(&check) || cause.trim().is_empty() {
                            return Err(format!("the known failure `{value}` names no check of {CHECKS:?}, or it has no cause"));
                        }
                        case.known.push((check.to_string(), cause.to_string()));
                    }
                    _ => return Err(format!("the line `{line}` has a key that the runner does not know")),
                }
            }
        }
        close(&mut case, section.take())?;
        if case.old.is_empty() && case.new.is_empty() {
            return Err("the case has no file".into());
        }
        Ok(case)
    }
}

/// A small matcher for the regular expressions of the cases. It knows: a literal, `.`, a
/// class `[…]`, the escapes `\d \w \s \D \W \S`, a group with `|`, the counts `* + ? {m,n}`,
/// the anchors `^ $`, and the flags `(?i)`, `(?m)`, `(?s)` at the start. Each other form is an
/// error: the runner never reads a form with a different meaning.
mod pattern {
    #[derive(Debug)]
    enum Node {
        Char(char),
        Any,
        Set(Vec<(char, char)>, bool),
        Start,
        End,
        Group(Vec<Vec<Node>>),
        Repeat(Box<Node>, usize, Option<usize>),
    }

    pub struct Pattern {
        nodes: Vec<Vec<Node>>,
        fold: bool,
        lines: bool,
        dot_all: bool,
    }

    struct Parser {
        chars: Vec<char>,
        at: usize,
    }

    fn class(c: char) -> Option<(Vec<(char, char)>, bool)> {
        let ranges = match c.to_ascii_lowercase() {
            'd' => vec![('0', '9')],
            'w' => vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
            's' => vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
            _ => return None,
        };
        Some((ranges, c.is_ascii_uppercase()))
    }

    impl Parser {
        fn alternatives(&mut self) -> Result<Vec<Vec<Node>>, String> {
            let mut all = vec![Vec::new()];
            while let Some(&c) = self.chars.get(self.at) {
                match c {
                    ')' => break,
                    '|' => {
                        self.at += 1;
                        all.push(Vec::new());
                    }
                    _ => {
                        let node = self.counted()?;
                        all.last_mut().unwrap().push(node);
                    }
                }
            }
            Ok(all)
        }

        fn counted(&mut self) -> Result<Node, String> {
            let atom = self.atom()?;
            let (min, max) = match self.chars.get(self.at) {
                Some('*') => (0, None),
                Some('+') => (1, None),
                Some('?') => (0, Some(1)),
                Some('{') => {
                    let end = (self.at..self.chars.len()).find(|at| self.chars[*at] == '}').ok_or("a count `{` has no end")?;
                    let text: String = self.chars[self.at + 1..end].iter().collect();
                    let number = |text: &str| text.parse::<usize>().map_err(|_| format!("`{text}` is not a count"));
                    let range = match text.split_once(',') {
                        Some((min, "")) => (number(min)?, None),
                        Some((min, max)) => (number(min)?, Some(number(max)?)),
                        None => (number(&text)?, Some(number(&text)?)),
                    };
                    self.at = end;
                    range
                }
                _ => return Ok(atom),
            };
            self.at += 1;
            if matches!(self.chars.get(self.at), Some('?' | '+')) {
                return Err("a lazy count or a possessive count is not a known form".into());
            }
            if matches!(atom, Node::Start | Node::End) {
                return Err("a count after an anchor is not a known form".into());
            }
            Ok(Node::Repeat(Box::new(atom), min, max))
        }

        fn atom(&mut self) -> Result<Node, String> {
            let c = self.chars[self.at];
            self.at += 1;
            Ok(match c {
                '.' => Node::Any,
                '^' => Node::Start,
                '$' => Node::End,
                '*' | '+' | '?' | '{' => return Err(format!("`{c}` has nothing before it")),
                '(' => {
                    if self.chars.get(self.at) == Some(&'?') {
                        if self.chars.get(self.at + 1) != Some(&':') {
                            return Err("a group `(?…` is not a known form".into());
                        }
                        self.at += 2;
                    }
                    let inner = self.alternatives()?;
                    if self.chars.get(self.at) != Some(&')') {
                        return Err("a group has no end".into());
                    }
                    self.at += 1;
                    Node::Group(inner)
                }
                '[' => {
                    let not = self.chars.get(self.at) == Some(&'^');
                    self.at += usize::from(not);
                    let mut ranges = Vec::new();
                    let mut first = true;
                    loop {
                        let c = *self.chars.get(self.at).ok_or("a class has no end")?;
                        self.at += 1;
                        if c == ']' && !first {
                            break;
                        }
                        first = false;
                        let low = if c == '\\' {
                            let escape = *self.chars.get(self.at).ok_or("an escape has no character")?;
                            self.at += 1;
                            if let Some((inner, false)) = class(escape) {
                                ranges.extend(inner);
                                continue;
                            }
                            literal(escape)?
                        } else {
                            c
                        };
                        if self.chars.get(self.at) == Some(&'-') && self.chars.get(self.at + 1).is_some_and(|c| *c != ']') {
                            let high = self.chars[self.at + 1];
                            if high == '\\' {
                                return Err("an escape at the end of a range is not a known form".into());
                            }
                            self.at += 2;
                            ranges.push((low, high));
                        } else {
                            ranges.push((low, low));
                        }
                    }
                    Node::Set(ranges, not)
                }
                '\\' => {
                    let escape = *self.chars.get(self.at).ok_or("an escape has no character")?;
                    self.at += 1;
                    match class(escape) {
                        Some((ranges, not)) => Node::Set(ranges, not),
                        None => Node::Char(literal(escape)?),
                    }
                }
                c => Node::Char(c),
            })
        }
    }

    fn literal(escape: char) -> Result<char, String> {
        match escape {
            'n' => Ok('\n'),
            't' => Ok('\t'),
            'r' => Ok('\r'),
            c if c.is_ascii_alphanumeric() => Err(format!("the escape `\\{c}` is not a known form")),
            c => Ok(c),
        }
    }

    impl Pattern {
        pub fn new(text: &str) -> Result<Pattern, String> {
            let mut pattern = Pattern { nodes: Vec::new(), fold: false, lines: false, dot_all: false };
            let mut rest = text;
            while let Some(flags) = rest.strip_prefix("(?").and_then(|tail| tail.split_once(')')).filter(|(flags, _)| !flags.is_empty() && flags.chars().all(|c| "ims".contains(c))) {
                pattern.fold |= flags.0.contains('i');
                pattern.lines |= flags.0.contains('m');
                pattern.dot_all |= flags.0.contains('s');
                rest = flags.1;
            }
            let mut parser = Parser { chars: rest.chars().collect(), at: 0 };
            pattern.nodes = parser.alternatives().map_err(|error| format!("the regular expression `{text}`: {error}"))?;
            if parser.at != parser.chars.len() {
                return Err(format!("the regular expression `{text}`: a `)` has no start"));
            }
            Ok(pattern)
        }

        /// True if the pattern matches at a position of the text (a search, not a full match).
        pub fn finds(&self, text: &str) -> bool {
            let chars: Vec<char> = text.chars().collect();
            (0..=chars.len()).any(|start| self.alternatives(&self.nodes, &chars, start, &mut |_| true))
        }

        fn alternatives(&self, all: &[Vec<Node>], text: &[char], at: usize, then: &mut dyn FnMut(usize) -> bool) -> bool {
            all.iter().any(|nodes| self.sequence(nodes, text, at, then))
        }

        fn sequence(&self, nodes: &[Node], text: &[char], at: usize, then: &mut dyn FnMut(usize) -> bool) -> bool {
            let Some((first, rest)) = nodes.split_first() else { return then(at) };
            match first {
                Node::Repeat(inner, min, max) => self.repeat(inner, *min, *max, 0, text, at, &mut |next| self.sequence(rest, text, next, then)),
                Node::Group(all) => self.alternatives(all, text, at, &mut |next| self.sequence(rest, text, next, then)),
                single => self.one(single, text, at).is_some_and(|next| self.sequence(rest, text, next, then)),
            }
        }

        fn repeat(&self, inner: &Node, min: usize, max: Option<usize>, count: usize, text: &[char], at: usize, then: &mut dyn FnMut(usize) -> bool) -> bool {
            // The longest match first, as in the usual engines.
            if max.is_none_or(|max| count < max) && self.sequence(std::slice::from_ref(inner), text, at, &mut |next| next > at && self.repeat(inner, min, max, count + 1, text, next, then)) {
                return true;
            }
            count >= min && then(at)
        }

        fn one(&self, node: &Node, text: &[char], at: usize) -> Option<usize> {
            let fold = |c: char| if self.fold { c.to_lowercase().next().unwrap_or(c) } else { c };
            match node {
                Node::Start => (at == 0 || (self.lines && text[at - 1] == '\n')).then_some(at),
                Node::End => (at == text.len() || (self.lines && text[at] == '\n') || (at + 1 == text.len() && text[at] == '\n')).then_some(at),
                Node::Any => text.get(at).filter(|c| self.dot_all || **c != '\n').map(|_| at + 1),
                Node::Char(c) => text.get(at).filter(|found| fold(**found) == fold(*c)).map(|_| at + 1),
                Node::Set(ranges, not) => {
                    let found = *text.get(at)?;
                    let inside = ranges.iter().any(|(low, high)| (*low..=*high).contains(&found) || (self.fold && ((*low..=*high).contains(&found.to_ascii_lowercase()) || (*low..=*high).contains(&found.to_ascii_uppercase()))));
                    (inside != *not).then_some(at + 1)
                }
                Node::Group(_) | Node::Repeat(..) => unreachable!("the sequence takes these nodes"),
            }
        }
    }
}

use pattern::Pattern;

/// The result of one command.
#[derive(Clone, PartialEq)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    /// The text of the result for an expected text and for a compare of two runs.
    fn text(&self) -> String {
        let mut text = format!("exit: {}\n{}", self.code, self.stdout);
        if !text.ends_with('\n') {
            text.push_str("\n(no line end at the end of the output)\n");
        }
        if !self.stderr.is_empty() {
            text.push_str("=== stderr\n");
            text.push_str(&self.stderr);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
        text
    }

    fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// The variables of git for each command of a case: no configuration of the user or of the
/// system, and a fixed author and date. So each machine gets the same trees.
fn git_env(command: &mut Command) {
    for (key, value) in [("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1"), ("GIT_AUTHOR_NAME", "Case"), ("GIT_AUTHOR_EMAIL", "case@example.com"), ("GIT_COMMITTER_NAME", "Case"), ("GIT_COMMITTER_EMAIL", "case@example.com"), ("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z"), ("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")] {
        command.env(key, value);
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command.current_dir(repo).args(["-c", "core.autocrlf=false", "-c", "core.quotePath=false", "-c", "core.safecrlf=false"]).args(args);
    git_env(&mut command);
    let output = command.output().map_err(|error| format!("git does not start: {error}"))?;
    if !output.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// A text with no hash and no path of this machine: the commit of the base, each tree, the
/// folder of the repository, and the folder of the cache.
fn normal(text: &str, folder: &Path) -> String {
    let mut text = text.to_string();
    for (path, name) in [(folder.join("repo"), "<REPO>"), (folder.join("cache"), "<CACHE>"), (folder.to_path_buf(), "<FOLDER>")] {
        text = text.replace(&path.to_string_lossy().to_string(), name);
    }
    let chars: Vec<char> = text.chars().collect();
    let hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < chars.len() {
        let word_start = at == 0 || !chars[at - 1].is_alphanumeric();
        let end = (at..chars.len()).find(|index| !hex(chars[*index])).unwrap_or(chars.len());
        let word_end = end == chars.len() || !chars[end].is_alphanumeric();
        let before: String = chars[at.saturating_sub(6)..at].iter().collect();
        if word_start && word_end && end - at == 40 {
            out.push_str("<TREE>");
            at = end;
        } else if word_start && word_end && (7..=12).contains(&(end - at)) && (before.ends_with("base `") || before.ends_with("HEAD `")) {
            out.push_str("<COMMIT>");
            at = end;
        } else {
            let stop = end.max(at + 1);
            out.extend(&chars[at..stop]);
            at = stop;
        }
    }
    // The time of a build check changes with each run: `build check: RESULT (1.2 s)`.
    out.lines()
        .map(|line| match line.strip_prefix("build check: ").and_then(|rest| rest.strip_suffix(" s)")).and_then(|rest| rest.rsplit_once(" (")) {
            Some((result, time)) if time.parse::<f64>().is_ok() => format!("build check: {result} (<TIME>)"),
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if out.ends_with('\n') { "\n" } else { "" }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// One line of a file: the file, true for the old tree, and the line number.
type Line = (String, bool, usize);

/// The lines of a list of positions: `a.rs:3-5, a.rs:-9, b.rs:1` (design 55.2).
fn position_lines(list: &str) -> Result<Vec<Line>, String> {
    let mut out = Vec::new();
    for position in list.split(", ") {
        let (file, range) = position.rsplit_once(':').ok_or(format!("the position `{position}` has no line"))?;
        let (old, range) = range.strip_prefix('-').map_or((false, range), |range| (true, range));
        let (start, end) = range.split_once('-').unwrap_or((range, range));
        let number = |text: &str| text.parse::<usize>().map_err(|_| format!("the position `{position}` has no line number"));
        out.extend((number(start)?..=number(end)?).map(|line| (file.to_string(), old, line)));
    }
    Ok(out)
}

/// A block of an output: a line and the lines below it with a larger indent.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Block {
    head: String,
    subs: Vec<String>,
}

fn blocks(text: &str, starts: &dyn Fn(&str) -> bool) -> Vec<Block> {
    let indent = |line: &str| line.chars().take_while(|c| *c == ' ').count();
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if starts(line.trim_start()) {
            let subs = lines[index + 1..].iter().take_while(|sub| !sub.trim().is_empty() && indent(sub) > indent(line)).map(|sub| sub.trim().to_string()).collect();
            out.push(Block { head: line.trim().to_string(), subs });
        }
    }
    out
}

/// Property 10 (design 61, R61.4-6): each fact whose head has a text of `different` has an
/// `equivalence:` line, and its own result does not start with "equivalent".
fn equivalence_problems(output: &str, different: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    for text in different {
        let found: Vec<Block> = facts(output).into_iter().filter(|fact| fact.head.contains(text.as_str())).collect();
        if found.is_empty() {
            problems.push(format!("no fact has the text `{text}`"));
        }
        for fact in found {
            let lines: Vec<&String> = fact.subs.iter().filter(|sub| sub.starts_with("equivalence: ")).collect();
            if lines.is_empty() {
                problems.push(format!("the fact has no equivalence line: {}", fact.head));
            }
            for line in lines {
                let own = line.trim_start_matches("equivalence: ").split("; the body:").next().unwrap_or_default();
                if own.starts_with("equivalent") || own.split("; ").any(|part| part.starts_with("`") && part.contains(": equivalent")) {
                    problems.push(format!("the case marks the fact different, and it gets `{line}`: {}", fact.head));
                }
            }
        }
    }
    problems
}

/// Property 11 (design 61.10, R61.10-2): "equivalent" is a statement about the program. Each
/// line of the part `reached bodies:` is a result of the walk (`different through`,
/// `equivalent (proved after inlining`, `reached through`), a fold line, or a limit of the walk;
/// and a body of that part never has the result "equivalent (proved by equal MIR)" in the step
/// section (it has a changed callee).
fn reach_problems(output: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let lines: Vec<&str> = output.lines().collect();
    let Some(start) = lines.iter().position(|line| line.trim_start().starts_with("reached bodies: ")) else { return problems };
    let indent = lines[start].len() - lines[start].trim_start().len();
    let mut reached: Vec<String> = Vec::new();
    for line in &lines[start + 1..] {
        let depth = line.len() - line.trim_start().len();
        if depth <= indent || line.trim().is_empty() {
            break;
        }
        let text = line.trim();
        if text.starts_with("chain: ") || text.starts_with("the walk stopped") {
            continue;
        }
        let result = text.split_once("): ").map(|(name, rest)| {
            reached.push(name.split(" (").next().unwrap_or_default().to_string());
            rest
        });
        let fold = text.contains(" bodies: ") && text.split(' ').next().is_some_and(|count| count.parse::<usize>().is_ok());
        let result = if fold { text.split_once(" bodies: ").map(|(_, rest)| rest) } else { result };
        let Some(result) = result else {
            problems.push(format!("a line of the reached bodies has no result: {text}"));
            continue;
        };
        let known = result.starts_with("different through ") || result.starts_with("reached through ") || (result.starts_with("equivalent") && result.contains("(proved after inlining "));
        if !known {
            problems.push(format!("a reached body has a result that is not a result of the walk: {text}"));
        }
    }
    for line in &lines[..start] {
        let text = line.trim();
        for name in &reached {
            if text.starts_with(&format!("{name} (")) && text.contains("equivalent (proved by equal MIR)") {
                problems.push(format!("a reached body has the result of an equal MIR text in the step section: {text}"));
            }
        }
    }
    problems
}

/// The facts of an output: each line `◆ fN KIND: TEXT [category]` with its sub lines.
fn facts(text: &str) -> Vec<Block> {
    blocks(text, &|line| line.starts_with("◆ "))
}

/// The entries of the gap list: the count line and each line `? …` with its sub lines.
fn gaps(text: &str) -> Vec<Block> {
    blocks(text, &|line| line.starts_with("? ") || line.starts_with("not stated:"))
}

/// The text of a fact with no number and no kind (`derived`, `stated`): `TEXT [category]`.
fn fact_text(head: &str) -> String {
    let rest = head.strip_prefix("◆ ").unwrap_or(head);
    let rest = rest.split_once(' ').map_or(rest, |(_, rest)| rest);
    rest.split_once(": ").map_or(rest, |(_, text)| text).to_string()
}

/// The sub lines of a block without the lines that name a tag (design 55.3: "A gap line names
/// its tag"). Each other sub line stays.
fn no_tag_subs(block: &Block) -> Vec<String> {
    block.subs.iter().filter(|sub| !sub.starts_with("tags: ") && !sub.starts_with("tag: ") && sub.as_str() != "not in a tag").cloned().collect()
}

/// The note below the first comment with no edit of an output (design 58.4).
const COMMENT_NOTE: &str = "note: Semilla does not check that a comment agrees with the code. A comment with no edit can be out of date.";

/// The mark of a comment with no edit (design 58.4), up to the position.
const NO_EDIT: &str = ", no edit in this step (";

/// One comment of a changed item below a fact line (design 58.6).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Comment {
    /// The first line, without the text.
    head: String,
    /// The lines of the text. `None` for a comment with an edit: its line names a fact.
    text: Option<Vec<String>>,
}

/// Splits the sub lines of a fact (design 58.6): each sub line that is not a line of a comment,
/// and the comments. The note of design 58.4 is in no list.
fn comment_subs(subs: &[String]) -> (Vec<String>, Vec<Comment>) {
    let is_head = |line: &str| (line.starts_with("docs ") || line.starts_with("comment ")) && (line.contains(NO_EDIT) || line.contains(" in this step: see f") || line.contains(", with no pair in the "));
    let (mut other, mut comments) = (Vec::new(), Vec::new());
    let mut index = 0;
    while index < subs.len() {
        let line = &subs[index];
        index += 1;
        if line == COMMENT_NOTE {
            continue;
        }
        if !is_head(line) {
            other.push(line.clone());
        } else if let Some(head) = line.strip_suffix(':') {
            // A block: each line of the text has the mark `| `.
            let mut text = Vec::new();
            while let Some(next) = subs.get(index).filter(|next| *next == "|" || next.starts_with("| ")) {
                text.push(next.strip_prefix("| ").unwrap_or("").to_string());
                index += 1;
            }
            comments.push(Comment { head: head.to_string(), text: Some(text) });
        } else if let Some((head, text)) = line.split_once("): `").filter(|_| line.ends_with('`')) {
            comments.push(Comment { head: format!("{head})"), text: Some(vec![text[..text.len() - 1].to_string()]) });
        } else {
            comments.push(Comment { head: line.clone(), text: None });
        }
    }
    (other, comments)
}

/// The comments of an output in a fixed order. A comment with an edit names its facts by their
/// texts, not by their numbers. The fact that a comment is below is not a part of the result:
/// a comment shows with the first fact of its item in the order of the output (design 58.6),
/// and a tag or a fact of the agent changes that order.
fn comments_of(output: &str) -> Vec<Comment> {
    let facts = facts(output);
    let texts: BTreeMap<&str, String> = facts.iter().filter_map(|fact| Some((fact.head.strip_prefix("◆ ")?.split(' ').next()?, fact_text(&fact.head)))).collect();
    let mut out = Vec::new();
    for fact in &facts {
        for mut comment in comment_subs(&fact.subs).1 {
            const SEE: &str = " in this step: see ";
            if let Some((start, ids)) = comment.head.clone().split_once(SEE) {
                let names: Vec<String> = ids.split(", ").map(|id| texts.get(id).cloned().unwrap_or_else(|| id.to_string())).collect();
                comment.head = format!("{start}{SEE}{}", names.join(" | "));
            }
            out.push(comment);
        }
    }
    out.sort();
    out
}

/// The comments of a Rust file from the tokens of its syntax tree: each comment token and each
/// attribute `doc`, with its first line, its last line, and its text. The test reads the tree
/// itself: it does not use the comment rules of the product.
fn tree_comments(text: &str) -> Vec<(usize, usize, String)> {
    use semilla_syntax::rs::{AstNode, Edition, NodeOrToken, SyntaxKind};
    let (file, _) = semilla_syntax::rs::parse_checked(text, Edition::Edition2021);
    let line = |offset: usize| text[..offset].matches('\n').count() + 1;
    let mut out = Vec::new();
    for element in file.syntax().descendants_with_tokens() {
        let (range, part) = match &element {
            NodeOrToken::Token(token) if matches!(token.kind(), SyntaxKind::COMMENT | SyntaxKind::OUTER_DOC_COMMENT | SyntaxKind::INNER_DOC_COMMENT) => (token.text_range(), token.text().to_string()),
            NodeOrToken::Node(node) if node.kind() == SyntaxKind::ATTR => {
                let compact: String = node.text().to_string().chars().filter(|c| !c.is_whitespace()).collect();
                if !compact.starts_with("#[doc=") && !compact.starts_with("#![doc=") {
                    continue;
                }
                (node.text_range(), node.text().to_string())
            }
            _ => continue,
        };
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        // The line end is not a part of the text of a comment (design 58.1): the token of a
        // line comment in a file with CR LF line ends has the CR.
        out.push((line(start), line(end.saturating_sub(1).max(start)), part.replace("\r\n", "\n").trim_end_matches('\r').to_string()));
    }
    out
}

/// Property 7 (design 58.11). Each comment with the mark "no edit in this step" is in the new
/// tree at its position: the comment tokens of those lines give the text. The same tokens are
/// in one file of the old tree, in the same order. No `docs` fact has a line of the comment. A comment shows one time. The count line
/// has the count of these comments, and an output with no such comment has no count line.
fn comment_problems(output: &str, old: &BTreeMap<String, Vec<u8>>, new: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen: BTreeSet<Comment> = BTreeSet::new();
    // The lines of a text as an output shows them: a hidden character is an escape.
    let shown = |text: &str| -> Vec<String> { text.split('\n').map(|line| semilla_core::text::inline(line).trim_end().to_string()).collect() };
    // The lines of the `docs` facts, for an output that shows the positions (`status --verbose`).
    let mut docs_lines: BTreeSet<Line> = BTreeSet::new();
    for fact in facts(output).iter().filter(|fact| fact.head.ends_with("[docs]")) {
        for list in fact.subs.iter().filter_map(|sub| sub.strip_prefix("lines: ")) {
            docs_lines.extend(position_lines(list).unwrap_or_default());
        }
    }
    for fact in facts(output) {
        for comment in comment_subs(&fact.subs).1 {
            let Some((_, rest)) = comment.head.split_once(NO_EDIT) else { continue };
            // R71.1-1: a comment over the budget shows its first lines, and the note after its
            // position gives the count of the lines ("(a large text: 30 lines, 2.6 kB; the first
            // 3 lines, 27 more lines; …)").
            let (rest, cut) = match rest.split_once(") (a large text: ") {
                Some((position, note)) => (format!("{position})"), Some(note.to_string())),
                None => (rest.to_string(), None),
            };
            let rest = rest.as_str();
            if !seen.insert(comment.clone()) {
                problems.push(format!("a comment shows two times: {}", comment.head));
                continue;
            }
            let Some(text) = &comment.text else {
                problems.push(format!("a comment with no edit has no text: {}", comment.head));
                continue;
            };
            let lines = match position_lines(rest.strip_suffix(')').unwrap_or(rest)) {
                Ok(lines) if !lines.is_empty() && lines.iter().all(|(file, old, _)| !old && *file == lines[0].0) => lines,
                _ => {
                    problems.push(format!("the position of a comment with no edit is not a range of lines of one file of the new tree: {}", comment.head));
                    continue;
                }
            };
            let (file, first, last) = (&lines[0].0, lines[0].2, lines[lines.len() - 1].2);
            // Design 58.4, item 5: a comment with a line in a `docs` fact has an edit.
            if lines.iter().any(|line| docs_lines.contains(line)) {
                problems.push(format!("a `docs` fact has a line of a comment with no edit: {}", comment.head));
                continue;
            }
            let parts: Vec<(usize, usize, String)> = new.get(file).map(|bytes| tree_comments(&String::from_utf8_lossy(bytes))).unwrap_or_default().into_iter().filter(|(start, end, _)| first <= *start && *end <= last).collect();
            let in_tree: Vec<String> = parts.iter().flat_map(|(_, _, part)| shown(part)).collect();
            let in_output: Vec<String> = text.iter().map(|line| line.trim_end().to_string()).collect();
            // A cut text: its lines are the first lines of the comment, and the note gives the
            // count of the lines of the comment.
            let number = |words: &str, before: &str| words.split(before).next().and_then(|head| head.rsplit(' ').next()).and_then(|word| word.parse::<usize>().ok());
            let in_tree = match &cut {
                Some(note) if number(note, " lines,") == Some(in_tree.len()) => in_tree.into_iter().take(in_output.len()).collect(),
                Some(note) => {
                    problems.push(format!("the note of a cut comment does not give the count of its lines ({}): {}", in_tree.len(), note));
                    continue;
                }
                None => in_tree,
            };
            if parts.is_empty() || in_tree != in_output {
                problems.push(format!("the comment tokens of the new tree at the position do not give the text of a comment with no edit: {}\n      the tree: {in_tree:?}\n      the output: {in_output:?}", comment.head));
                continue;
            }
            let in_old = old.iter().filter(|(path, _)| path.ends_with(".rs")).any(|(_, bytes)| {
                let mut next = 0;
                for (_, _, part) in tree_comments(&String::from_utf8_lossy(bytes)) {
                    if parts.get(next).is_some_and(|(_, _, wanted)| *wanted == part) {
                        next += 1;
                    }
                }
                next == parts.len()
            });
            if !in_old {
                problems.push(format!("no file of the old tree has the comment tokens of a comment with no edit: {}", comment.head));
            }
        }
    }
    let stated: Vec<&str> = output.lines().map(str::trim).filter_map(|line| line.strip_prefix("comments: ")).collect();
    let count = stated.first().and_then(|line| line.split(' ').next()).and_then(|word| word.parse::<usize>().ok());
    if stated.len() > 1 || count != (!seen.is_empty()).then_some(seen.len()) {
        problems.push(format!("the output has {} comment(s) with no edit; the count line(s): {stated:?}", seen.len()));
    }
    problems
}

/// The two equal sides of a text "`OLD` → `NEW`", if a fact line has them.
/// The tokens of a line for the eighth property: each word (a name, a keyword, a number), each
/// string literal, and each other character that is not white space. The runner has its own
/// lexer. A `,` and a `;` can be tokens with no meaning (design 52.2): they are not in the list.
fn simple_tokens(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        let end = if c.is_whitespace() {
            at += 1;
            continue;
        } else if c == '"' {
            let mut end = at + 1;
            while end < chars.len() && chars[end] != '"' {
                end += if chars[end] == '\\' { 2 } else { 1 };
            }
            (end + 1).min(chars.len())
        } else if word(c) {
            (at..chars.len()).find(|index| !word(chars[*index])).unwrap_or(chars.len())
        } else {
            at + 1
        };
        let token: String = chars[at..end].iter().collect();
        if token != "," && token != ";" {
            out.push(token);
        }
        at = end;
    }
    out
}

/// Property 8 (design 59, B-e). The output of `status --verbose` lists each line that only the
/// rule of the equal tokens puts in a fact, with the item pair: "equal tokens: in `ITEM` (old
/// lines A-B of `F`, new lines C-D of `G`): LINES". The runner reads the two trees itself:
/// each token of such a line is in the text of the item of the other tree, as often as the
/// line has it. The check is weaker than the rule: it does not prove that the tree compare
/// pairs the token.
fn equal_token_problems(output: &str, old: &BTreeMap<String, Vec<u8>>, new: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut problems = Vec::new();
    let text = |tree: &BTreeMap<String, Vec<u8>>, file: &str| tree.get(file).map(|bytes| String::from_utf8_lossy(bytes).replace("\r\n", "\n"));
    for line in output.lines().map(str::trim).filter(|line| line.starts_with("equal tokens: in `")) {
        let parse = || -> Option<(usize, usize, String, usize, usize, String, String)> {
            let rest = line.split_once(" (old lines ")?.1;
            let (old_range, rest) = rest.split_once(" of `")?;
            let (old_file, rest) = rest.split_once("`, new lines ")?;
            let (new_range, rest) = rest.split_once(" of `")?;
            let (new_file, list) = rest.split_once("`): ")?;
            let range = |text: &str| -> Option<(usize, usize)> {
                let (a, b) = text.split_once('-')?;
                Some((a.parse().ok()?, b.parse().ok()?))
            };
            let ((a, b), (c, d)) = (range(old_range)?, range(new_range)?);
            Some((a, b, old_file.to_string(), c, d, new_file.to_string(), list.to_string()))
        };
        let Some((a, b, old_file, c, d, new_file, list)) = parse() else {
            problems.push(format!("the line `{line}` has not the form of an equal-token line"));
            continue;
        };
        let Ok(lines) = position_lines(&list) else {
            problems.push(format!("the line `{line}` has no list of positions"));
            continue;
        };
        for (file, is_old, number) in lines {
            let (own, other, other_file, range) = if is_old { (old, new, &new_file, (c, d)) } else { (new, old, &old_file, (a, b)) };
            let (Some(own_text), Some(other_text)) = (text(own, &file), text(other, other_file)) else {
                problems.push(format!("`{line}`: a tree has no file `{file}` or `{other_file}`"));
                continue;
            };
            let Some(here) = own_text.lines().nth(number - 1) else {
                problems.push(format!("`{line}`: the file `{file}` has no line {number}"));
                continue;
            };
            let mut pool: BTreeMap<String, usize> = BTreeMap::new();
            for token in other_text.lines().skip(range.0 - 1).take(range.1 + 1 - range.0).flat_map(simple_tokens) {
                *pool.entry(token).or_default() += 1;
            }
            let mut need: BTreeMap<String, usize> = BTreeMap::new();
            for token in simple_tokens(here) {
                *need.entry(token).or_default() += 1;
            }
            let missing: Vec<String> = need.iter().filter(|(token, count)| pool.get(*token).copied().unwrap_or(0) < **count).map(|(token, _)| format!("`{token}`")).collect();
            if !missing.is_empty() {
                problems.push(format!("`{line}`: the item of the other tree does not have the token(s) {} of the line {}{}:{number}", missing.join(", "), if is_old { "-" } else { "" }, file));
            }
        }
    }
    problems
}

/// The full-text commands of an output (R76.1-1): each `semilla expand …`, to the end of the
/// command (a backquote, a parenthesis, a semicolon, or the end of the line), and each
/// `semilla status … --verbose` (a pointer of the form before batch 12b, also in a line of a
/// cut).
fn full_text_commands(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        for head in ["semilla expand ", "semilla status "] {
            let mut rest = line;
            while let Some(at) = rest.find(head) {
                let tail = &rest[at..];
                let end = tail.find(['`', ')', ';']).unwrap_or(tail.len());
                let command = tail[..end].trim().to_string();
                if !found.contains(&command) && (head == "semilla expand " || command.ends_with(" --verbose")) {
                    found.push(command);
                }
                rest = &tail[end..];
            }
        }
    }
    found
}

/// The last characters of a text, and the first characters of a text (on a character border).
fn tail_chars(text: &str, count: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(count)..].iter().collect()
}

fn head_chars(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

/// Property 14 (R76.1-1): each full-text command of an output works in the state after the
/// output (`folder`): it exits with 0, it prints a text, and it does not say "no open change
/// set". Each line with a cut of design 38.5 that names the command has its full line in the
/// text of the command: one line with the text before the cut and the text after it, and no
/// cut. A command of the size form of a fact (R71.1-1) prints no size form that names itself.
fn full_text_problems(work: &mut Work, folder: &Path, output: &str, state: &str) -> Result<Vec<String>, String> {
    let mut problems = Vec::new();
    for command in full_text_commands(output) {
        let args: Vec<String> = command.split_whitespace().skip(1).map(str::to_string).collect();
        let run = work.semilla(folder, &args)?;
        let text = run.all();
        if run.code != 0 || text.contains("no open change set") || run.stdout.trim().is_empty() {
            problems.push(format!("{state}: `{command}` does not print the full text (exit {}):\n{}", run.code, text.lines().take(6).collect::<Vec<_>>().join("\n")));
            continue;
        }
        for line in output.lines().filter(|line| line.contains(&format!("`{command}` shows the full line"))) {
            let (Some(first), Some(last)) = (line.find("… ("), line.rfind(") …")) else { continue };
            let (before, after) = (tail_chars(line[..first].trim_end_matches(", "), 30), head_chars(line[last + ") …".len()..].trim_start_matches(", "), 30));
            if !text.lines().any(|full| full.contains(before.trim()) && full.contains(after.trim()) && !full.contains("shows the full line")) {
                problems.push(format!("{state}: the text of `{command}` has no full line for the cut line `{}`", line.trim()));
            }
        }
        if text.contains(&format!("the full text: {command})")) {
            problems.push(format!("{state}: the text of `{command}` has a text over the budget that names the same command"));
        }
    }
    Ok(problems)
}

/// [R87.6-1] The claim templates of a full text: each piece in back quotes that starts with `#[`
/// of a line "MIR changes with no claim", and each template line of the AST map. One text is
/// one template (the nodes of one item share their template).
fn templates_of(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut add = |template: &str| {
            if !found.iter().any(|known| known == template) {
                found.push(template.to_string());
            }
        };
        if line.contains("MIR changes with no claim: ") {
            for piece in line.split('`').filter(|piece| piece.starts_with("#[")) {
                add(piece);
            }
        }
        if let Some(template) = line.trim_start().strip_prefix("template: #[") {
            add(&format!("#[{template}"));
        }
    }
    found
}

/// Property 16 (R87.6-1, design 87): each template that the step prints passes `semilla intent
/// check` with `true` in place of each `…`, all in one intent file (the full text of the step).
fn template_problems(work: &mut Work, folder: &Path, state: &str) -> Result<Vec<String>, String> {
    let run = work.semilla(folder, &["expand".to_string(), "--step".to_string(), "0".to_string()])?;
    let found = templates_of(&run.stdout);
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let file = folder.join("templates.rs");
    let text = format!("use semilla_intent::prelude::*;\n\n{}\n", found.iter().map(|template| template.replace('…', "true")).collect::<Vec<_>>().join("\n"));
    std::fs::write(&file, &text).map_err(|error| error.to_string())?;
    let check = work.semilla(folder, &["intent".to_string(), "check".to_string(), file.display().to_string()])?;
    if check.code != 0 {
        return Ok(vec![format!("{state}: the {} template(s) of the step do not pass `semilla intent check`:\n{}\n{}", found.len(), text, check.all().lines().filter(|line| line.starts_with("error") || line.contains("-->")).take(12).collect::<Vec<_>>().join("\n"))]);
    }
    Ok(Vec::new())
}

/// The counts of a line `AST changes: N mapped by claims, M mapped by Semilla, K not mapped`.
fn ast_counts(line: &str) -> Option<[usize; 3]> {
    let rest = line.trim_start().strip_prefix("AST changes: ")?;
    let mut out = [0; 3];
    for (at, (part, word)) in rest.split(", ").zip(["mapped by claims", "mapped by Semilla", "not mapped"]).enumerate() {
        out[at] = part.strip_suffix(word).or_else(|| part.split_once(&format!(" {word}")).map(|(number, _)| number))?.trim().parse().ok()?;
    }
    Some(out)
}

/// Property 15 (R79.1-1): the map of the AST pass never hides an edit. The full text of the step
/// (`semilla expand --step 0`) gives the count T of the changed nodes of the syntax diff (counted
/// apart from the entries) and one line for each entry with its node number `nI`: each number
/// from 1 to T is in exactly one line, the three counts add up to T and are the counts of the
/// states of the lines, and the step output has the same counts (unless the full text is the
/// syntax tree computed again, with no data of the compiler, R76.1-1).
fn ast_problems(work: &mut Work, folder: &Path, output: &str, state: &str) -> Result<Vec<String>, String> {
    let mut problems = Vec::new();
    let Some(short) = output.lines().find_map(ast_counts) else {
        problems.push(format!("{state}: the step output has no line `AST changes: …`"));
        return Ok(problems);
    };
    let run = work.semilla(folder, &["expand".to_string(), "--step".to_string(), "0".to_string()])?;
    let Some((full, total)) = run.stdout.lines().find_map(|line| {
        let counts = ast_counts(line)?;
        let total = line.split_once("(of ")?.1.split(' ').next()?.parse::<usize>().ok()?;
        Some((counts, total))
    }) else {
        problems.push(format!("{state}: the full text has no line `AST changes: … (of T changed nodes)`"));
        return Ok(problems);
    };
    // The full text of a step with no printed record on this machine is the syntax tree computed
    // again with no data of the compiler (R76.1-1): its map can differ, and it says so.
    if full != short && !run.stdout.contains("this machine has no record of the output of step") {
        problems.push(format!("{state}: the counts of the step output {short:?} are not the counts of the full text {full:?}"));
    }
    if full.iter().sum::<usize>() != total {
        problems.push(format!("{state}: the counts {full:?} do not add up to the {total} changed nodes"));
    }
    let mut seen: BTreeMap<usize, usize> = BTreeMap::new();
    let mut states = [0usize; 3];
    for line in run.stdout.lines() {
        let Some(rest) = line.strip_prefix("      n") else { continue };
        let Some((number, tail)) = rest.split_once(' ') else { continue };
        let Ok(number) = number.parse::<usize>() else { continue };
        let at = if tail.starts_with("mapped by claim ") { 0 } else if tail.starts_with("mapped by Semilla ") { 1 } else if tail.starts_with("not mapped ") { 2 } else { continue };
        states[at] += 1;
        *seen.entry(number).or_default() += 1;
    }
    if states != full {
        problems.push(format!("{state}: the lines of the entries have the states {states:?}, and the count line {full:?}"));
    }
    let twice: Vec<usize> = seen.iter().filter(|(_, count)| **count > 1).map(|(number, _)| *number).collect();
    let missing: Vec<usize> = (1..=total).filter(|number| !seen.contains_key(number)).collect();
    let extra: Vec<usize> = seen.keys().filter(|number| **number == 0 || **number > total).copied().collect();
    if !twice.is_empty() || !missing.is_empty() || !extra.is_empty() {
        problems.push(format!("{state}: each changed node must be in exactly one entry: in two entries {twice:?}, in no entry {missing:?}, not a node {extra:?}"));
    }
    Ok(problems)
}

/// The problems of the control flow section of `status --verbose` (design 60, R60.4-6): the
/// count of the bodies, and for each result of a body, the counts of its summary and the lines
/// of its points and arrows (`+ KIND …`, `− KIND …`, `~ KIND …`, `+ arrow …`, `− arrow …`).
fn flow_problems(text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|line| line.starts_with("  control flow: ") && line.ends_with(['y', 's'])) else { return problems };
    let count: usize = lines[start].trim_start_matches("  control flow: ").split(' ').next().and_then(|word| word.parse().ok()).unwrap_or(usize::MAX);
    let mut bodies = 0;
    let mut index = start + 1;
    // The counts of one result: points added, removed, changed, arrows added, removed.
    let check = |head: &str, elements: &[&str], problems: &mut Vec<String>| {
        // R71.2-1: the counts end at the first `; ` (the words of the points that move between
        // the bodies of one item follow).
        let summary = head.split_once(": ").map_or("", |(_, rest)| rest.split(" (in ").next().unwrap_or("").split("; ").next().unwrap_or(""));
        let mut want = [0usize; 5];
        let word = head.split([':', '(']).next().unwrap_or("").trim();
        for part in summary.split(", ") {
            let number = |text: &str| text.split(' ').next().and_then(|word| word.parse::<usize>().ok());
            if word == "graph added" || word == "graph removed" {
                let offset = if word == "graph added" { 0 } else { 1 };
                if let Some(value) = number(part) {
                    if part.contains("point") { want[offset] = value } else if part.contains("arrow") { want[3 + offset] = value }
                }
                continue;
            }
            let arrow = part.ends_with("arrow") || part.ends_with("arrows");
            if let Some(rest) = part.strip_prefix('+') {
                want[if arrow { 3 } else { 0 }] += number(rest).unwrap_or(0);
            } else if let Some(rest) = part.strip_prefix('−') {
                want[if arrow { 4 } else { 1 }] += number(rest).unwrap_or(0);
            } else if part.contains(" changed ") {
                want[2] += number(part).unwrap_or(0);
            }
        }
        let mut have = [0usize; 5];
        for element in elements {
            // R71.2-1: a point that moves to a different body of its item is not in the counts
            // of a diff; a full graph counts each of its points.
            let moved = element.starts_with("moved ");
            let slot = if element.starts_with("+ arrow ") { 3 } else if element.starts_with("− arrow ") { 4 } else if element.starts_with("+ ") || (moved && word == "graph added") { 0 } else if element.starts_with("− ") || (moved && word == "graph removed") { 1 } else if element.starts_with("~ ") { 2 } else { continue };
            have[slot] += 1;
        }
        if want != have {
            problems.push(format!("the result `{head}` counts {want:?} (added, removed, changed points, added, removed arrows), and its lines give {have:?}"));
        }
    };
    while index < lines.len() && lines[index].starts_with("    ") {
        let line = lines[index];
        // The part of the reached bodies (design 61.10) has its own property (11).
        if line.trim_start().starts_with("reached bodies: ") && !line.starts_with("     ") {
            break;
        }
        // The one line of the budget of the step (R61.14-6, R77.2-3) is not a body line.
        let stop = ["the equivalence check stopped", "the safety time limit stopped"].iter().any(|start| line.trim_start().starts_with(start));
        if stop {
            index += 1;
            continue;
        }
        if !line.starts_with("     ") {
            bodies += 1;
        } else if line.starts_with("        ") && !line.starts_with("         ") {
            let head = line.trim();
            let mut elements = Vec::new();
            let mut next = index + 1;
            while next < lines.len() && lines[next].starts_with("          ") {
                elements.push(lines[next].trim());
                next += 1;
            }
            check(head, &elements, &mut problems);
        }
        index += 1;
    }
    if bodies != count {
        problems.push(format!("the section counts {count} bodies, and it has {bodies} body lines"));
    }
    problems
}

fn empty_change(line: &str) -> Option<String> {
    const ARROW: &str = "` → `";
    line.match_indices(ARROW).find_map(|(at, _)| {
        let (left, right) = (&line[..at], &line[at + ARROW.len()..]);
        left.match_indices('`').map(|(start, _)| &left[start + 1..]).find(|old| right.strip_prefix(old).is_some_and(|rest| rest.starts_with('`'))).map(str::to_string)
    })
}

/// The words of a command line of a shell with single quotes.
fn shell_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                quote = !quote;
                word.get_or_insert_default();
            }
            '\\' if !quote => word.get_or_insert_default().extend(chars.next()),
            ' ' if !quote => words.extend(word.take()),
            c => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    words
}

/// A Markdown text and a fact line with no escape of Markdown, for a compare.
fn no_escape(text: &str) -> String {
    text.replace('\\', "").replace("&gt;", ">").replace("&lt;", "<").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

/// A line diff of two texts for a message.
fn difference(old: &str, new: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if a[i] == b[j] { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, String::new());
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || table[i][j + 1] >= table[i + 1][j]) {
            out.push_str(&format!("    + {}\n", b[j]));
            j += 1;
        } else {
            out.push_str(&format!("    - {}\n", a[i]));
            i += 1;
        }
    }
    out
}

/// The work of one case: the folder, and the results of the checks.
struct Work {
    folder: PathBuf,
    /// The failed checks: the name and the message.
    failed: Vec<(&'static str, String)>,
    runs: usize,
}

impl Work {
    fn fail(&mut self, check: &'static str, message: String) {
        self.failed.push((check, message));
    }

    /// A new copy of the repository of the case, with its own cache folder.
    fn copy(&mut self, name: &str) -> Result<PathBuf, String> {
        let folder = self.folder.join(name);
        copy_dir(&self.folder.join("template"), &folder.join("repo")).map_err(|error| format!("the copy of the repository: {error}"))?;
        Ok(folder)
    }

    fn semilla(&mut self, folder: &Path, args: &[String]) -> Result<Run, String> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
        command.current_dir(folder.join("repo")).args(args);
        // One mode (design 83): each step builds the two trees, has their MIR, and runs the
        // build check. A removed variable of the mode in the environment of the test would be an
        // error, so the runner removes it.
        command.env_remove("SEMILLA_MIR_DRIVER").env_remove("SEMILLA_COMPILED").env_remove("SEMILLA_NO_CHECK").env_remove("SEMILLA_OWN_LINE_DIFF");
        command.env("SEMILLA_CACHE_DIR", folder.join("cache"));
        git_env(&mut command);
        let output = command.output().map_err(|error| format!("semilla does not start: {error}"))?;
        self.runs += 1;
        let text = |bytes: &[u8]| normal(&String::from_utf8_lossy(bytes), folder);
        Ok(Run { code: output.status.code().unwrap_or(-1), stdout: text(&output.stdout), stderr: text(&output.stderr) })
    }

    fn step(&mut self, folder: &Path, more: &[String]) -> Result<Run, String> {
        let mut args = vec!["step".to_string(), "the change".to_string()];
        args.extend(more.iter().cloned());
        self.semilla(folder, &args)
    }
}

/// The changed lines of the step from the test's own call of the line diff (design 55.8). A file
/// that is not UTF-8 text in a tree is not a text of the two trees: it has no changed line.
fn changed_lines(case: &Case, folder: &Path) -> Result<BTreeSet<Line>, String> {
    let repo = folder.join("repo");
    git(&repo, &["add", "-A"])?;
    let old = git(&repo, &["rev-parse", "HEAD^{tree}"])?.trim().to_string();
    let new = git(&repo, &["write-tree"])?.trim().to_string();
    let mut args: Vec<&str> = LINE_DIFF.to_vec();
    args.extend([old.as_str(), new.as_str()]);
    let diff = git(&repo, &args)?;
    let text_file = |file: &str| [&case.old, &case.new].iter().all(|side| side.get(file).is_none_or(|bytes| std::str::from_utf8(bytes).is_ok()));
    hunk_lines(&diff, &text_file)
}

/// The changed lines of the output of the line diff: each line of each side of each hunk.
fn hunk_lines(diff: &str, text_file: &dyn Fn(&str) -> bool) -> Result<BTreeSet<Line>, String> {
    let mut lines = BTreeSet::new();
    let (mut file, mut pending): (Option<String>, Option<String>) = (None, None);
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            file = None;
            pending = None;
        } else if let Some(path) = line.strip_prefix("--- ").filter(|_| file.is_none()) {
            // Git puts a tab after a path that has a space.
            pending = path.trim_end_matches('\t').strip_prefix("a/").map(str::to_string);
        } else if let Some(path) = line.strip_prefix("+++ ").filter(|_| file.is_none()) {
            file = path.trim_end_matches('\t').strip_prefix("b/").map(str::to_string).or(pending.take());
        } else if let Some(header) = line.strip_prefix("@@ -") {
            let name = file.clone().ok_or(format!("the hunk `{line}` has no file"))?;
            let (ranges, _) = header.split_once(" @@").ok_or(format!("the hunk `{line}` has no end"))?;
            let (old_range, new_range) = ranges.split_once(" +").ok_or(format!("the hunk `{line}` has no new range"))?;
            for (old_side, range) in [(true, old_range), (false, new_range)] {
                let (start, count) = range.split_once(',').unwrap_or((range, "1"));
                let (start, count): (usize, usize) = (start.parse().map_err(|_| format!("the hunk `{line}`"))?, count.parse().map_err(|_| format!("the hunk `{line}`"))?);
                if text_file(&name) {
                    lines.extend((start..start + count).map(|number| (name.clone(), old_side, number)));
                }
            }
        }
    }
    Ok(lines)
}

fn line_text((file, old, line): &Line) -> String {
    format!("{file}:{}{line}", if *old { "-" } else { "" })
}

/// The result of the checks of one case.
struct Outcome {
    /// The failed checks: the name and the message.
    failed: Vec<(&'static str, String)>,
    /// The result of the step, for the compare with the expected text.
    text: String,
    /// The count of the runs of the binary.
    runs: usize,
    /// True if the runner had an accepted step: only such a step shows its positions.
    positions: bool,
}

fn check(case: &Case) -> Result<Outcome, String> {
    let temp = tempfile::Builder::new().prefix("semilla-case-").tempdir().map_err(|error| format!("no temporary folder: {error}"))?;
    let mut work = Work { folder: temp.path().to_path_buf(), failed: Vec::new(), runs: 0 };
    // The old tree is the commit. The new tree is the working tree.
    let template = work.folder.join("template");
    let put = |files: &BTreeMap<String, Vec<u8>>| -> Result<(), String> {
        for (path, bytes) in files {
            let full = template.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).and_then(|()| std::fs::write(&full, bytes)).map_err(|error| format!("the file `{path}`: {error}"))?;
        }
        Ok(())
    };
    std::fs::create_dir_all(&template).map_err(|error| error.to_string())?;
    put(&case.old)?;
    git(&template, &["init", "-q", "-b", "main"])?;
    git(&template, &["add", "-A"])?;
    git(&template, &["commit", "-q", "--allow-empty", "-m", "the old tree"])?;
    for path in case.old.keys().filter(|path| !case.new.contains_key(*path)) {
        std::fs::remove_file(template.join(path)).map_err(|error| format!("the file `{path}`: {error}"))?;
    }
    put(&case.new)?;

    // The step of the case. Its result is the expected text.
    let first = work.copy("a")?;
    let step = work.step(&first, &case.options)?;
    let mut text = step.text();
    let step_text = text.clone();
    let output = step.all();

    // The checks of the case.
    if case.exit.is_some_and(|exit| exit != step.code) {
        work.fail("exit", format!("the exit code is {}, the case wants {}", step.code, case.exit.unwrap()));
    }
    for (check, patterns, want) in [("must_match", &case.must_match, true), ("must_not_match", &case.must_not_match, false)] {
        for pattern in patterns {
            if Pattern::new(pattern)?.finds(&output) != want {
                work.fail(check, format!("the output {} `{pattern}`", if want { "has no match of" } else { "has a match of" }));
            }
        }
    }

    // Property 10: a fact that the case marks `different` never gets "equivalent" (design 61,
    // R61.4-6). Its own result is the text before "; the body:".
    for problem in equivalence_problems(&output, &case.different) {
        work.fail("equivalence", problem);
    }

    // Property 11: a reached caller never shows a plain "equivalent" (design 61.10, R61.10-2).
    for problem in reach_problems(&output) {
        work.fail("reach", problem);
    }

    // Property 2: no fact has an old text that is equal to its new text.
    for fact in facts(&output) {
        if let Some(side) = empty_change(&fact.head) {
            work.fail("empty_change", format!("the two sides of a fact are equal (`{side}`): {}", fact.head));
        }
    }

    // Property 14 (R76.1-1): each full-text command of the step works after the step (a stored
    // step, or a rejected step).
    if step.code != 2 {
        let state = if step.code == 0 { "after the stored step" } else { "after the rejected step" };
        for problem in full_text_problems(&mut work, &first, &output, state)? {
            work.fail("full_text", problem);
        }
        // Property 15 (R79.1-1): each changed node is in exactly one entry of the AST map.
        for problem in ast_problems(&mut work, &first, &output, state)? {
            work.fail("ast_map", problem);
        }
        // Property 16 (R87.6-1): each printed template passes the check of the intent files.
        if step.code == 0 {
            for problem in template_problems(&mut work, &first, state)? {
                work.fail("templates", problem);
            }
        }
    }

    // Property 3: a second run in a new copy gives the same result.
    let second = work.copy("b")?;
    let again = work.step(&second, &case.options)?;
    if again != step {
        work.fail("same_result", format!("the second run is different:\n{}", difference(&step_text, &again.text())));
    }

    // The run with no option, for a case with options.
    let plain = if case.options.is_empty() {
        None
    } else {
        let folder = work.copy("d")?;
        Some(work.step(&folder, &[])?)
    };
    // A comment of a changed item is below the first fact of the item in the order of the
    // output (design 58.6), and a fact of the agent changes that order. Thus the comments are
    // not sub lines of the compare of the facts: they have their own compare.
    let fact_set = |run: &Run| -> BTreeSet<(String, Vec<String>)> { facts(&run.all()).iter().map(|fact| (fact_text(&fact.head), comment_subs(&fact.subs).0)).collect() };
    if case.same_facts && let Some(plain) = &plain {
        let (with, without) = (fact_set(&step), fact_set(plain));
        if step.code == 2 {
            // A form error is no result of a step: it has no fact set to compare.
            if !with.is_empty() {
                work.fail("same_facts", "the step has a form error (exit 2), but its output has a fact line".to_string());
            }
        } else if with != without {
            let only = |a: &BTreeSet<(String, Vec<String>)>, b: &BTreeSet<(String, Vec<String>)>| a.difference(b).map(|(text, subs)| format!("\n      {text} {subs:?}")).collect::<String>();
            work.fail("same_facts", format!("the facts with the options are not the facts with no option;\n    only with the options:{}\n    only with no option:{}", only(&with, &without), only(&without, &with)));
        } else if comments_of(&step.all()) != comments_of(&plain.all()) {
            work.fail("same_facts", format!("the comments with the options are not the comments with no option:\n    with the options: {:?}\n    with no option: {:?}", comments_of(&step.all()), comments_of(&plain.all())));
        }
    }

    // Property 12 (design 62, R62.3-1): the new tree as a commit, and a tag after the commit.
    // The tag gives the facts of the step with no option.
    {
        let folder = work.copy("t")?;
        let repo = folder.join("repo");
        git(&repo, &["add", "-A"])?;
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "the change"])?;
        let tagged = work.semilla(&folder, &["tag".to_string()])?;
        // Property 14 (R76.1-1): each full-text command of the tag after the commit works after
        // the tag.
        if tagged.code != 2 {
            for problem in full_text_problems(&mut work, &folder, &tagged.all(), "after the tag after the commit")? {
                work.fail("full_text", problem);
            }
        }
        let base = plain.as_ref().unwrap_or(&step);
        // The one text that differs on purpose (R62.3-1): a spotlight with no reason is an open
        // item in the tag, not a cause to run the step again.
        // R76.1-1: a full-text command of the tag names the commit (`--commit SHORT`).
        let no_commit = |line: &str| {
            let mut out = line.to_string();
            while let Some(at) = out.find(" --commit ") {
                out = format!("{}{}", &out[..at], out[at + " --commit ".len()..].trim_start_matches(|c: char| c.is_ascii_hexdigit()));
            }
            out
        };
        let open_set = |run: &Run| -> BTreeSet<(String, Vec<String>)> { fact_set(run).into_iter().map(|(text, subs)| (no_commit(&text), subs.iter().map(|sub| no_commit(&open_spotlight(sub))).collect())).collect() };
        if base.code != 2 && tagged.code == 2 {
            work.fail("after_commit", format!("the tag after the commit gives no result:\n{}", tagged.all()));
        } else if base.code != 2 && open_set(&tagged) != open_set(base) {
            let (with, without) = (open_set(&tagged), open_set(base));
            let only = |a: &BTreeSet<(String, Vec<String>)>, b: &BTreeSet<(String, Vec<String>)>| a.difference(b).map(|(text, subs)| format!("\n      {text} {subs:?}")).collect::<String>();
            work.fail("after_commit", format!("the facts of the tag after the commit are not the facts of the step;\n    only in the tag:{}\n    only in the step:{}", only(&with, &without), only(&without, &with)));
        }
    }

    // Property 7: each comment with the mark "no edit in this step" is in the old tree and in
    // the new tree (design 58.11).
    for problem in comment_problems(&output, &case.old, &case.new) {
        work.fail("comments", problem);
    }

    // Property 5: a rejected step is not stored, and a false fact of the agent removes no fact
    // of Semilla.
    if step.code != 0 {
        let status = work.semilla(&first, &["status".to_string()])?;
        if !status.stdout.contains("no open change set") {
            work.fail("agent_facts", format!("the step is rejected (exit {}), but `semilla status` shows a stored step:\n{}", step.code, status.stdout));
        }
    }
    if let Some(plain) = &plain && output.lines().any(|line| line.trim_start().starts_with("✗ fact ") && line.contains("of the agent is false")) {
        // The step with only the facts of the agent that hold: the command that the output
        // gives, or the step with no option. Each of its facts is a fact of the case.
        let command = output.lines().find_map(|line| line.strip_prefix("run again (without the false fact): semilla ")).map(shell_words);
        let honest = match command {
            Some(args) if args.len() > 2 => {
                let folder = work.copy("f")?;
                work.semilla(&folder, &args)?
            }
            _ => plain.clone(),
        };
        let with: BTreeSet<String> = facts(&output).iter().map(|fact| fact_text(&fact.head)).collect();
        for fact in facts(&honest.all()) {
            if !with.contains(&fact_text(&fact.head)) {
                work.fail("agent_facts", format!("a false fact of the agent removes a fact of Semilla: {}", fact.head));
            }
        }
    }

    // The changed lines from the test's own call of the line diff.
    let diff_folder = work.copy("diff")?;
    let changed = changed_lines(case, &diff_folder)?;

    // Property 4: a tag that has each hunk changes no fact line and no gap line, and it gives
    // no note. A tag never replaces a fact.
    let files: BTreeSet<&str> = changed.iter().map(|(file, _, _)| file.as_str()).collect();
    if !files.is_empty() && files.iter().all(|file| !file.contains([',', '='])) && step.code != 2 {
        let folder = work.copy("c")?;
        let mut options = case.options.clone();
        // The form `--tag=…`: the name of a file can start with `-`.
        options.push(format!("--tag={}=whole", files.iter().copied().collect::<Vec<_>>().join(",")));
        let tagged = work.step(&folder, &options)?;
        let tagged_output = tagged.all();
        if tagged.code != step.code {
            work.fail("tags", format!("the exit code with a tag is {}, without it {}", tagged.code, step.code));
        }
        let sorted = |mut list: Vec<Block>| {
            for block in &mut list {
                // The comments have their own compare below: a tag changes the order of the
                // output, and a comment is below the first fact of its item (design 58.6).
                block.subs = comment_subs(&no_tag_subs(block)).0;
            }
            list.sort();
            list
        };
        for (name, with, without) in [("fact", sorted(facts(&tagged_output)), sorted(facts(&output))), ("gap", sorted(gaps(&tagged_output)), sorted(gaps(&output)))] {
            if with != without {
                let show = |list: &[Block]| list.iter().map(|block| format!("{}\n{}", block.head, block.subs.iter().map(|sub| format!("    {sub}\n")).collect::<String>())).collect::<String>();
                work.fail("tags", format!("a tag changes a {name} line:\n{}", difference(&show(&without), &show(&with))));
            }
        }
        if comments_of(&tagged_output) != comments_of(&output) {
            work.fail("tags", format!("a tag changes a comment of a changed item:\n    with the tag: {:?}\n    without it: {:?}", comments_of(&tagged_output), comments_of(&output)));
        }
        for problem in comment_problems(&tagged_output, &case.old, &case.new) {
            work.fail("comments", format!("with a tag: {problem}"));
        }
        // A new line of the output with a tag is a line of the tag view (design 55.3), or the
        // command. Only a fact with no position is not in the tag: the tag has each hunk.
        const NO_POSITION: &str = "facts with no position (a tag selects changed lines; these facts have none): ";
        let no_position = tagged_output.lines().any(|line| line.trim().starts_with(NO_POSITION));
        let known: BTreeSet<&str> = output.lines().map(str::trim).collect();
        for line in tagged_output.lines().map(str::trim).filter(|line| !known.contains(line)) {
            let of_tag = ["tags of the agent (", "tag `whole` (", "tag: `whole`", "tags: `whole`", "facts, in the groups of the tags below: ", NO_POSITION, "run again"].iter().any(|start| line.starts_with(start)) || (line == "not in a tag" && no_position);
            let note = ["in part", "also in this tag", "✗ tag"].iter().any(|note| line.contains(note)) || (line.contains("not in a tag") && !no_position);
            if !of_tag || note {
                work.fail("tags", format!("a tag gives a new line: {line}"));
            }
        }
    }

    // An accepted step for the reader and for the positions: the step of the case, or the
    // step with the reasons that the output asks for.
    let mut accepted = (step.code == 0).then(|| (first.clone(), step.clone()));
    let (mut last, mut args) = (output.clone(), {
        let mut args = vec!["step".to_string(), "the change".to_string()];
        args.extend(case.options.iter().cloned());
        args
    });
    for round in 0..3 {
        if accepted.is_some() || step.code != 1 {
            break;
        }
        // The command that the output gives (with each reason), or the last command. Then the
        // options that the gap list asks for.
        let again = last.lines().rev().find(|line| line.starts_with("run again") && line.contains(": semilla step ")).and_then(|line| line.split_once(": semilla ")).map(|(_, command)| shell_words(&command.replace("<why>", "the reason of the test")));
        let mut next = again.unwrap_or_else(|| args.clone());
        if let Some(more) = last.lines().find_map(|line| line.trim().strip_prefix("to state them, add these options to the command of the step: ")) {
            next.extend(shell_words(&more.replace("=TEXT'", "=the text of the test'")));
        }
        if next == args {
            break;
        }
        let folder = work.copy(&format!("e{round}"))?;
        let run = work.semilla(&folder, &next)?;
        (last, args) = (run.all(), next);
        if run.code == 0 {
            accepted = Some((folder, run));
        }
    }

    // Property 1: the cover. The count of the cover line is the count of the changed lines.
    // Each flagged line is a changed line. For an accepted step, each changed line is in the
    // position of a fact, or it is flagged.
    let cover = output.lines().map(str::trim).find(|line| line.starts_with("cover: ") || line.starts_with("! cover: "));
    let flagged_of = |text: &str| -> Result<BTreeSet<Line>, String> {
        match text.lines().map(str::trim).find_map(|line| line.strip_prefix("! changed lines with no fact: ")) {
            Some(list) => Ok(position_lines(list)?.into_iter().collect()),
            None => Ok(BTreeSet::new()),
        }
    };
    let flagged = flagged_of(&output)?;
    if step.code != 2 {
        let words: Vec<&str> = cover.unwrap_or("").split(' ').collect();
        let number = |before: &str| words.iter().position(|word| *word == before).and_then(|at| words.get(at + 1)).and_then(|word| word.parse::<usize>().ok());
        let (count, missing) = match cover {
            None => (0, 0),
            Some(line) if line.starts_with('!') => (number("the").unwrap_or(usize::MAX), number("cover:").unwrap_or(usize::MAX)),
            Some(_) => (number("the").unwrap_or(usize::MAX), 0),
        };
        if count != changed.len() {
            work.fail("cover", format!("the cover line counts {count} changed line(s), the line diff of the test has {}: {}", changed.len(), cover.unwrap_or("(no cover line)")));
        }
        if missing != flagged.len() {
            work.fail("cover", format!("the cover line counts {missing} line(s) with no fact, the flagged list has {}", flagged.len()));
        }
        for line in flagged.difference(&changed) {
            work.fail("cover", format!("the flagged line {} is not a changed line", line_text(line)));
        }
        if let Some(line) = output.lines().map(str::trim).find(|line| line.starts_with("hunks: `git diff ")) {
            if !line.ends_with(" <TREE> <TREE>`") {
                work.fail("cover", format!("the hunk line does not end with the two trees: {line}"));
            }
        }
    }
    // The positions are a part of the expected text. Only a stored step shows them.
    text.push_str("=== positions\n");
    if accepted.is_none() {
        text.push_str("none: the runner has no accepted step for this case\n");
    }
    if let Some((folder, run)) = &accepted {
        let status = work.semilla(folder, &["status".to_string(), "--verbose".to_string()])?;
        if step.code != 0 {
            let words: Vec<String> = args.iter().map(|word| if word.chars().all(|c| c.is_alphanumeric() || "-_=./:".contains(c)) { word.clone() } else { format!("'{}'", word.replace('\'', "'\\''")) }).collect();
            text.push_str(&format!("the step is not accepted; the positions are from: semilla {}\n", words.join(" ")));
        }
        for fact in facts(&status.stdout) {
            let lines = fact.subs.iter().find(|sub| sub.starts_with("lines: ")).map_or("no position", String::as_str);
            text.push_str(&format!("{}\n    {lines}\n", fact.head));
        }
        let flagged = flagged_of(&run.all())?;
        let mut in_fact: BTreeSet<Line> = BTreeSet::new();
        for fact in facts(&status.stdout) {
            for list in fact.subs.iter().filter_map(|sub| sub.strip_prefix("lines: ")) {
                in_fact.extend(position_lines(list)?);
            }
        }
        let open: Vec<String> = changed.iter().filter(|line| !in_fact.contains(*line) && !flagged.contains(*line)).map(line_text).collect();
        if !open.is_empty() {
            work.fail("cover", format!("{} changed line(s) are in no fact position and not in the flagged list: {}", open.len(), open.join(", ")));
        }
        let wide: Vec<String> = in_fact.difference(&changed).map(line_text).collect();
        if !wide.is_empty() {
            work.fail("cover", format!("{} line(s) of a fact position are not changed lines: {}", wide.len(), wide.join(", ")));
        }

        // Property 6: the reader. `status --verbose` and the Markdown review have each fact
        // line of the accepted step.
        let report = folder.join("review.md");
        let review = work.semilla(folder, &["review".to_string(), "--md".to_string(), report.to_string_lossy().to_string()])?;
        let markdown = no_escape(&std::fs::read_to_string(&report).unwrap_or_default());
        let status_lines: BTreeSet<&str> = status.stdout.lines().map(str::trim).collect();
        for fact in facts(&run.all()) {
            if !status_lines.contains(fact.head.as_str()) {
                work.fail("reader", format!("`status --verbose` does not have the fact line: {}", fact.head));
            }
            let line = no_escape(fact.head.strip_prefix("◆ ").unwrap_or(&fact.head));
            if review.code != 0 || !markdown.contains(&line) {
                work.fail("reader", format!("the Markdown review (exit {}) does not have the fact line: {}", review.code, fact.head));
            }
        }
        for fact in facts(&status.stdout) {
            if let Some(side) = empty_change(&fact.head) {
                work.fail("empty_change", format!("the two sides of a fact of `status --verbose` are equal (`{side}`): {}", fact.head));
            }
        }
        // The reader has each comment of the accepted step (design 58.9), and property 7 holds
        // for the text of `status --verbose`.
        if comments_of(&status.stdout) != comments_of(&run.all()) {
            work.fail("reader", format!("the comments of `status --verbose` are not the comments of the step:\n    status: {:?}\n    the step: {:?}", comments_of(&status.stdout), comments_of(&run.all())));
        }
        for problem in comment_problems(&status.stdout, &case.old, &case.new) {
            work.fail("comments", format!("`status --verbose`: {problem}"));
        }
        // Property 8: each token of a line of the rule of the equal tokens is in the item of
        // the other tree (design 59, B-e).
        for problem in equal_token_problems(&status.stdout, &case.old, &case.new) {
            work.fail("equal_tokens", format!("`status --verbose`: {problem}"));
        }
        // Property 9: each changed point and each arrow that is not shared of the graph of a
        // body is in a line of `status --verbose` (design 60, R60.4-6).
        for problem in flow_problems(&status.stdout) {
            work.fail("control_flow", format!("`status --verbose`: {problem}"));
        }
        for comment in facts(&run.all()).iter().flat_map(|fact| comment_subs(&fact.subs).1) {
            let lines = std::iter::once(&comment.head).chain(comment.text.iter().flatten());
            for line in lines.filter(|line| !markdown.contains(no_escape(line).trim())) {
                work.fail("reader", format!("the Markdown review does not have the line `{line}` of the comment: {}", comment.head));
            }
        }
    }
    // Property 14 (R76.1-1): after `semilla tag` of the stored step, each full-text command of
    // the step and of the tag still works. The tag closes the change set, so this check is the
    // last use of the folder of the step.
    if step.code == 0 && !full_text_commands(&output).is_empty() {
        let repo = first.join("repo");
        git(&repo, &["add", "-A"])?;
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "the change"])?;
        let tag = work.semilla(&first, &["tag".to_string()])?;
        if tag.code == 0 {
            for problem in full_text_problems(&mut work, &first, &format!("{output}\n{}", tag.all()), "after `semilla tag`")? {
                work.fail("full_text", problem);
            }
        }
    }
    Ok(Outcome { failed: work.failed, text, runs: work.runs, positions: accepted.is_some() })
}

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases")
}

fn load_cases() -> Vec<Case> {
    let root = cases_dir();
    let mut cases = Vec::new();
    let mut groups: Vec<PathBuf> = std::fs::read_dir(&root).unwrap().flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
    groups.sort();
    for group in groups {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&group).unwrap().flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|extension| extension == "case")).collect();
        files.sort();
        for file in files {
            let id = format!("{}/{}", group.file_name().unwrap().to_string_lossy(), file.file_stem().unwrap().to_string_lossy());
            let text = std::fs::read_to_string(&file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            cases.push(Case::parse(&id, &file, &text).unwrap_or_else(|error| panic!("{}: {error}", file.display())));
        }
    }
    cases
}

/// The runner of the case set (design 57.3, 57.4).
// rule+ R62.3-1
// rule+ R44.1-1 R51.3-4 R55-1 R55.2-1 R55.3-4 R55.4-1 R55.4-3 R55.8-1 R57.4-1 R57.4-2 R57.4-3 R57.4-4 R60.4-6
// rule- R57.4-2
// rule+ R87.6-1
#[test]
fn each_case_gives_its_expected_text_and_keeps_the_properties() {
    let start = std::time::Instant::now();
    let filter = std::env::var("SEMILLA_CASES").ok();
    let write = std::env::var("SEMILLA_CASES_WRITE").is_ok_and(|value| !value.is_empty() && value != "0");
    let all = load_cases();
    assert!(all.len() >= 354, "the case set has {} cases; it had 354 at its start (design 57.3)", all.len());
    let cases: Vec<&Case> = all.iter().filter(|case| filter.as_ref().is_none_or(|filter| case.id.contains(filter.as_str()))).collect();
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Result<Outcome, String>)>> = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |count| count.get());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            // The matcher of the patterns calls itself once for each character that a `.*`
            // takes, so a pattern over a long output needs a large stack (batch 11b: one more
            // line of the output was past the default size of 2 MB).
            let worker = std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(scope, || {
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(case) = cases.get(index) else { break };
                    let result = check(case);
                    results.lock().unwrap().push((index, result));
                }
            });
            worker.expect("a thread of the case runner starts");
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(index, _)| *index);
    let mut problems: Vec<String> = Vec::new();
    let (mut written, mut known_count, mut runs) = (0, 0, 0);
    let mut no_positions: Vec<&str> = Vec::new();
    for (index, result) in results {
        let case = cases[index];
        let Outcome { mut failed, text, runs: count, positions } = match result {
            Ok(result) => result,
            Err(error) => {
                problems.push(format!("{}: the runner cannot run the case: {error}", case.id));
                continue;
            }
        };
        runs += count;
        if !positions {
            no_positions.push(case.id.as_str());
        }
        if case.expected.as_deref() != Some(text.as_str()) {
            if write {
                std::fs::write(&case.file, format!("{}=== expected\n{text}", case.head)).unwrap();
                written += 1;
            } else {
                match &case.expected {
                    Some(expected) => failed.push(("expected", format!("the result is not the expected text (`-` expected, `+` now):\n{}", difference(expected, &text)))),
                    None => failed.push(("expected", "the case has no expected text; run the test with `SEMILLA_CASES_WRITE=1` and review the text".to_string())),
                }
            }
        }
        for (check, cause) in &case.known {
            if failed.iter().any(|(name, _)| name == check) {
                known_count += 1;
            } else {
                problems.push(format!("{}: the known failure `{check}` ({cause}) passes now: remove the line `known_failure` from the case", case.id));
            }
        }
        for (check, message) in failed {
            if !case.known.iter().any(|(name, _)| name == check) {
                problems.push(format!("{} [{check}]: {message}", case.id));
            }
        }
    }
    println!("case set: {} case(s), {runs} run(s) of semilla, {known_count} known failure(s), {written} expected text(s) written, {threads} thread(s), {:.1} s", cases.len(), start.elapsed().as_secs_f64());
    println!("no accepted step, so no check of the fact positions, for {} case(s): {}", no_positions.len(), no_positions.join(", "));
    assert!(problems.is_empty(), "{} problem(s) in the case set:\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn the_pattern_matcher_knows_the_forms_of_the_cases() {
    let finds = |pattern: &str, text: &str| Pattern::new(pattern).unwrap().finds(text);
    assert!(finds("not stated: [1-9]", "  not stated: 3 item(s)") && !finds("not stated: [1-9]", "  not stated: 0 item(s)"));
    assert!(finds("(Rest|Idle).*Busy|Busy", "x Busy") && finds("(Rest|Idle).*Busy|Busy", "Rest to Busy") && !finds("(Rest|Idle).*Busy", "Rest\nBusy"));
    assert!(finds(r"\$\w+\.total => \$\w+\.sum\(\)", "rewrite $a.total => $b.sum()") && !finds(r"\$\w+\.total", "$.total"));
    assert!(finds("(?i)behavior", "the Behavior") && !finds("behavior", "the Behavior"));
    assert!(finds(r"(?m)^\s*-\s+self\.0\[index\] = 0;", "a\n  -   self.0[index] = 0;") && !finds(r"^\s*-\s+self", "a\n  - self"));
    assert!(finds("a{2,3}$", "caa") && !finds("a{2,3}$", "ca") && finds("[^a-c]x", "dx") && !finds("[^a-c]x", "bx") && finds("ab?c+", "acc"));
    for unknown in ["a*?", r"\bword", "(?=a)", "(a", "a)", "[a"] {
        assert!(Pattern::new(unknown).is_err(), "`{unknown}` is not a known form");
    }
}

#[test]
fn a_case_file_gives_the_two_trees_and_the_checks() {
    let text = "note: a note\noption: --same\noption: rename a b\nexit: 1\nmust_match: x+\nknown_failure: cover: the cause\n=== both Cargo.toml\n[package]\n=== old a.rs\n=== old b.rs (no line end at the end)\nfn b() {}\n=== new a.rs (each line ends with CR LF)\nfn a() {}\n\n=== new c.bin (base64)\n/wA=\n=== expected\nexit: 1\n=== old not a section\n";
    let case = Case::parse("g/n", Path::new("n.case"), text).unwrap();
    assert_eq!(case.options, ["--same", "rename a b"]);
    assert_eq!((case.exit, case.must_match.len(), case.same_facts), (Some(1), 1, false));
    assert_eq!(case.known, [("cover".to_string(), "the cause".to_string())]);
    assert_eq!(case.old.iter().map(|(path, bytes)| (path.as_str(), bytes.as_slice())).collect::<Vec<_>>(), [("Cargo.toml", &b"[package]\n"[..]), ("a.rs", &b""[..]), ("b.rs", &b"fn b() {}"[..])]);
    assert_eq!(case.new.iter().map(|(path, bytes)| (path.as_str(), bytes.as_slice())).collect::<Vec<_>>(), [("Cargo.toml", &b"[package]\n"[..]), ("a.rs", &b"fn a() {}\r\n\r\n"[..]), ("c.bin", &b"\xff\x00"[..])]);
    assert_eq!(case.expected.as_deref(), Some("exit: 1\n=== old not a section\n"));
    // A line that the runner does not know is an error: a wrong key never turns a check off.
    for bad in ["exits: 1\n=== old a.rs\n", "known_failure: cover\n=== old a.rs\n", "known_failure: speed: slow\n=== old a.rs\n", "=== left a.rs\n", "=== old a.rs\n=== old a.rs\n", "note: no file\n"] {
        assert!(Case::parse("g/n", Path::new("n.case"), bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_helpers_of_the_properties_read_the_output() {
    assert_eq!(empty_change("◆ f1 derived: changed macro call `t!( A = 1, )` → `t!( A = 1, )` (makes items) [declaration]").as_deref(), Some("t!( A = 1, )"));
    assert_eq!(empty_change("◆ f1 derived: changed `a` → `b`, changed `c` → `c` [x]").as_deref(), Some("c"));
    assert_eq!(empty_change("◆ f1 derived: changed `a` → `a b` and `x` → `y` [semantics]"), None);
    assert_eq!(position_lines("a.rs:3-4, a.rs:-9, b c.rs:1").unwrap(), [("a.rs".to_string(), false, 3), ("a.rs".to_string(), false, 4), ("a.rs".to_string(), true, 9), ("b c.rs".to_string(), false, 1)]);
    assert!(position_lines("a.rs").is_err());
    assert_eq!(open_spotlight("    ✗ spotlight of f1, f2: a cfg; it needs a reason: --reason 'a::b=…'"), "    ! spotlight of f1, f2: a cfg; open: no reason given");
    assert_eq!(open_spotlight("    spotlight: a cfg; reason: \"x\""), "    spotlight: a cfg; reason: \"x\"");
    assert_eq!(shell_words("step 'the change' --reason 'a::b=it'\\''s <why>' x"), ["step", "the change", "--reason", "a::b=it's <why>", "x"]);
    assert_eq!(fact_text("◆ f12 stated: 2 site(s): changed `a` → `b` [semantics]"), "2 site(s): changed `a` → `b` [semantics]");
    let text = "  head:\n  ◆ f1 derived: one [docs]\n      in: a\n      lines: a.rs:1\n  ◆ f2 derived: two [docs]\n  not stated: 1 item(s)\n  ? 1 place\n      in: b\n";
    assert_eq!(facts(text), [Block { head: "◆ f1 derived: one [docs]".into(), subs: vec!["in: a".into(), "lines: a.rs:1".into()] }, Block { head: "◆ f2 derived: two [docs]".into(), subs: vec![] }]);
    assert_eq!(gaps(text).len(), 2);
    let diff = "diff --git a/a b.rs b/a b.rs\nindex 1..2 100644\n--- a/a b.rs\t\n+++ b/a b.rs\t\n@@ -3,0 +4,2 @@ fn f() {\n+x\n+--- a/not a file\n@@ -9 +11 @@\n-y\n+z\ndiff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+n\ndiff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-g\n-h\ndiff --git a/raw.bin b/raw.bin\n--- a/raw.bin\n+++ b/raw.bin\n@@ -1 +1 @@\n-r\n+s\n";
    let lines: Vec<String> = hunk_lines(diff, &|file| file != "raw.bin").unwrap().iter().map(line_text).collect();
    assert_eq!(lines, ["a b.rs:4", "a b.rs:5", "a b.rs:11", "a b.rs:-9", "gone.rs:-1", "gone.rs:-2", "new.rs:1"]);
    assert!(hunk_lines("@@ -1 +1 @@\n", &|_| true).is_err());
    let folder = Path::new("/tmp/x");
    assert_eq!(normal("base `1ea5d28`: /tmp/x/repo/a 62b58cee548ee0eeb2e6e08b7968d0dd20c28424 deadbeef", folder), "base `<COMMIT>`: <REPO>/a <TREE> deadbeef");
}

/// Design 59, B-e, property 8. A true line gives no problem. A line with a token that the item
/// of the other tree does not have gives a problem.
// rule+ R59.2-5
// rule- R59.2-5
#[test]
fn the_eighth_property_finds_a_token_that_the_other_item_does_not_have() {
    let tree = |text: &str| -> BTreeMap<String, Vec<u8>> { [("src/lib.rs".to_string(), text.as_bytes().to_vec())].into_iter().collect() };
    let old = tree("pub fn f() -> u32 {\n    g(\n        1,\n        2,\n    )\n}\n");
    let new = tree("pub fn f() -> u32 {\n    g(1, 2)\n}\n");
    assert_eq!(simple_tokens("    g(1, \"a b\"),"), ["g", "(", "1", "\"a b\"", ")"]);
    let good = "  equal tokens: in `depot::f` (old lines 1-6 of `src/lib.rs`, new lines 1-3 of `src/lib.rs`): src/lib.rs:2, src/lib.rs:-2-5\n";
    assert_eq!(equal_token_problems(good, &old, &new), Vec::<String>::new());
    // A token that the new item does not have.
    let changed = tree("pub fn f() -> u32 {\n    g(1, 3)\n}\n");
    let problems = equal_token_problems(good, &old, &changed);
    assert!(problems.iter().any(|problem| problem.contains("`2`")), "{problems:#?}");
    // A count: the old line has the token two times.
    let twice = tree("pub fn f() -> u32 {\n    g(1, 1)\n}\n");
    let problems = equal_token_problems("  equal tokens: in `depot::f` (old lines 1-3 of `src/lib.rs`, new lines 1-6 of `src/lib.rs`): src/lib.rs:-2\n", &twice, &old);
    assert!(problems.iter().any(|problem| problem.contains("`1`")), "{problems:#?}");
    // A line of a different form.
    assert_eq!(equal_token_problems("  equal tokens: in `depot::f`: src/lib.rs:2\n", &old, &new).len(), 1);
}

/// Design 58.11, property 7. The runner reads the comment tokens of the two trees itself. A
/// true output gives no problem. Negative test: a text that is not in the new tree, a text
/// that is not in the old tree, a position that is not the position of the comment, a comment
/// that shows two times, and a count line that does not agree give a problem each.
// rule+ R58.11-1
// rule- R58.11-1
// rule- R60.4-6
#[test]
fn the_ninth_property_finds_a_changed_point_with_no_line() {
    let good = "step 0\n  control flow: 1 body\n    demo::f (src/lib.rs:3): graph changes: +1 branch, +3 arrows, −1 arrow\n        graph changes: +1 branch, +3 arrows, −1 arrow\n          + branch `x > 3` (src/lib.rs:4)\n          + arrow entry (src/lib.rs:3) → branch `x > 3` (src/lib.rs:4)\n          + arrow branch `x > 3` (src/lib.rs:4) → return (src/lib.rs:6) (true)\n          + arrow branch `x > 3` (src/lib.rs:4) → call `demo::g` (src/lib.rs:5) (false)\n          − arrow entry (src/lib.rs:-3) → call `demo::g` (src/lib.rs:-4)\n";
    assert!(flow_problems(good).is_empty(), "{:?}", flow_problems(good));
    // The negative case: a line of an arrow is missing, and the count of the bodies is wrong.
    let bad = good.replace("          − arrow entry (src/lib.rs:-3) → call `demo::g` (src/lib.rs:-4)\n", "").replace("1 body", "2 bodies");
    assert_eq!(flow_problems(&bad).len(), 2, "{:?}", flow_problems(&bad));
}

#[test]
fn the_seventh_property_finds_a_false_mark_no_edit() {
    let tree = |text: &str| -> BTreeMap<String, Vec<u8>> { [("src/lib.rs".to_string(), text.as_bytes().to_vec())].into_iter().collect() };
    let old = tree("/// Gives the price.\n#[doc = \"The tax is 10 percent.\"]\npub fn price(x: u32) -> u32 {\n    /* One.\r\n       Two. */\n    x + x / 10 // ten\n}\npub const TEXT: &str = \"// no comment\";\n");
    let new = tree("\n/// Gives the price.\n#[doc = \"The tax is 10 percent.\"]\npub fn price(x: u32) -> u32 {\n    /* One.\n       Two. */\n    x + x / 5 // ten\n}\npub const TEXT: &str = \"// no comment\";\n");
    let fact = "  ◆ f1 derived: changed literal `10` → `5` at `shop::price/tail/rhs/rhs` [semantics]\n";
    let docs = "      docs of `shop::price`, no edit in this step (src/lib.rs:2-3):\n          | /// Gives the price.\n          | #[doc = \"The tax is 10 percent.\"]\n";
    let block = "      comment above line 7, no edit in this step (src/lib.rs:5-6):\n          | /* One.\n          |        Two. */\n";
    let end = "      comment at the end of line 7, no edit in this step (src/lib.rs:7): `// ten`\n";
    let note = format!("      {COMMENT_NOTE}\n");
    let count = |count: usize| format!("  comments: {count} comment(s) of the changed items have no edit in this step\n");
    let good = format!("{fact}{docs}{block}{end}{note}{}", count(3));
    assert_eq!(comment_problems(&good, &old, &new), Vec::<String>::new());
    // The helpers: the note is in no list, and a comment with an edit has no text.
    let subs: Vec<String> = format!("in: shop::price\n{docs}{note}docs of `shop::cost`, changed in this step: see f2\nlines: src/lib.rs:7").lines().map(|line| line.trim().to_string()).collect();
    let (other, comments) = comment_subs(&subs);
    assert_eq!(other, ["in: shop::price", "lines: src/lib.rs:7"]);
    assert_eq!(comments, [Comment { head: "docs of `shop::price`, no edit in this step (src/lib.rs:2-3)".into(), text: Some(vec!["/// Gives the price.".into(), "#[doc = \"The tax is 10 percent.\"]".into()]) }, Comment { head: "docs of `shop::cost`, changed in this step: see f2".into(), text: None }]);

    let problem = |output: String, words: &str| {
        let problems = comment_problems(&output, &old, &new);
        assert!(problems.iter().any(|problem| problem.contains(words)), "the output must give a problem with `{words}`:\n{output}\n{problems:#?}");
    };
    // A text that the new tree does not have at the position.
    problem(format!("{fact}{}{}", end.replace("// ten", "// five"), count(1)), "do not give the text");
    // A cut text.
    problem(format!("{fact}{}{}", docs.replace("          | #[doc = \"The tax is 10 percent.\"]\n", ""), count(1)), "do not give the text");
    // The position of a different line.
    problem(format!("{fact}{}{}", end.replace("src/lib.rs:7)", "src/lib.rs:6)"), count(1)), "do not give the text");
    // A text in a string literal is no comment token.
    problem(format!("{fact}      comment above line 9, no edit in this step (src/lib.rs:9): `// no comment`\n{}", count(1)), "do not give the text");
    // A `docs` fact has the line of the comment (an output with positions).
    problem(format!("{fact}{end}  ◆ f2 derived: added comment `// ten` [docs]\n      lines: src/lib.rs:7\n{}", count(1)), "a `docs` fact has a line");
    assert_eq!(comment_problems(&format!("{fact}{end}  ◆ f2 derived: added comment `// five` [docs]\n      lines: src/lib.rs:8\n{}", count(1)), &old, &new), Vec::<String>::new());
    // The comment is in the new tree, and the old tree does not have it.
    let other_old = tree("/// Gives the price.\npub fn price(x: u32) -> u32 {\n    x + x / 10 // 10\n}\n");
    assert!(comment_problems(&format!("{fact}{end}{}", count(1)), &other_old, &new).iter().any(|problem| problem.contains("no file of the old tree")));
    // A comment that shows two times.
    problem(format!("{fact}{end}{fact}{end}{}", count(1)), "shows two times");
    // The count line.
    problem(format!("{fact}{docs}{end}{}", count(3)), "the count line(s)");
    problem(format!("{fact}{docs}{end}"), "the count line(s)");
    problem(format!("{fact}{}", count(1)), "the count line(s)");
}
