//! The rule index (design 57.6).
//!
//! A rule of the design has an identity in the design text, at the rule: `[R55.4-1]`. A test that
//! checks a rule names the identity in a marker. A Rust test has the marker in a comment above its
//! `#[test]` line. A case of the case set has the marker as a header line.
//!
//! | Marker | Rust test | Case file |
//! |---|---|---|
//! | the positive case | `// rule+ ID` | `rule+: ID` |
//! | the negative case | `// rule- ID` | `rule-: ID` |
//!
//! The test of this file reads `docs/design.md`, each Rust source below `crates/`, and each case
//! file. It fails if:
//!
//! - a rule has no test of the positive case;
//! - a rule has no test of the negative case, and its identity gives no cause
//!   (`[R55.4-1; no negative case: CAUSE]`);
//! - a marker names an identity that the design does not have;
//! - a marker of a Rust source is not above a `#[test]` line;
//! - an identity is in the design two times, or it is not in the section of its number;
//! - a subsection of the indexed sections has no identity and no line `Rule index: …`;
//! - a subsection has the line `Rule index: not indexed: …`, and it is not in the list
//!   `STAY_NOT_INDEXED` of this file (the lock of batch 9t2), or the list names a subsection
//!   that has no such line.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The first and the last section whose subsections must each tell their state in the index.
const INDEXED: std::ops::RangeInclusive<u32> = 48..=76;
/// The start of the line that gives the state of a subsection with no identity.
const NO_RULE: &str = "Rule index: no rule: ";
const NOT_INDEXED: &str = "Rule index: not indexed: ";
/// The lock of the index (design 57.6, R57.6-6): the subsections that stay "not indexed", each
/// with its cause. A line `Rule index: not indexed: …` of a different subsection is a problem.
/// A new subsection gets its identities, or the line `Rule index: no rule: CAUSE`.
const STAY_NOT_INDEXED: &[(&str, &str)] = &[(
    "48.1",
    "the items are the step model of the compiled check before section 49, and two items do not agree with R49.3-5 (design 57.9)",
)];

/// One rule of the design.
#[derive(Debug, Default, Clone, PartialEq)]
struct Rule {
    /// The line of the identity in the design (the first line is 1).
    line: usize,
    /// The cause of a rule with no negative case.
    no_negative: Option<String>,
}

/// The rules of the design, and the subsections that tell "not indexed".
#[derive(Debug, Default)]
struct Design {
    rules: BTreeMap<String, Rule>,
    not_indexed: Vec<String>,
    no_rule: Vec<String>,
    problems: Vec<String>,
}

/// The number of a heading line: `## 55. Text` gives `55`, and `### 55.4 Text` gives `55.4`.
fn heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("### ").or_else(|| line.strip_prefix("## "))?;
    let number = rest.split(' ').next()?.trim_end_matches('.');
    let is_number = !number.is_empty() && number.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    is_number.then_some(number)
}

/// An identity has the form `R<section>-<number>`. The section is `55` or `55.4`.
fn is_identity(text: &str) -> bool {
    let Some((section, number)) = text.strip_prefix('R').and_then(|rest| rest.split_once('-')) else { return false };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    digits(number) && section.split('.').count() <= 2 && section.split('.').all(digits)
}

/// Each identity that a line of the design gives: `[R55.4-1]` or `[R55.4-1; no negative case: CAUSE]`.
fn identities(line: &str) -> Vec<Result<(String, Option<String>), String>> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("[R") {
        rest = &rest[at + 1..];
        let Some(end) = rest.find(']') else { break };
        let inner = &rest[..end];
        let (name, tail) = inner.split_once("; ").map_or((inner, None), |(name, tail)| (name, Some(tail)));
        if !is_identity(name) {
            continue;
        }
        found.push(match tail {
            None => Ok((name.to_string(), None)),
            Some(tail) => match tail.strip_prefix("no negative case: ").map(str::trim) {
                Some(cause) if cause.split_whitespace().count() >= 3 => Ok((name.to_string(), Some(cause.to_string()))),
                _ => Err(format!("the identity `{name}` has a text after `;` that is not `no negative case: CAUSE` with a cause of three words or more: `{tail}`")),
            },
        });
        rest = &rest[end..];
    }
    found
}

fn read_design(text: &str, stay: &[(&str, &str)]) -> Design {
    let mut design = Design::default();
    // The open subsection: its number, its line, and the facts that the check needs.
    let mut open: Option<(String, usize, bool)> = None;
    let mut section = String::new();
    let close = |open: &mut Option<(String, usize, bool)>, problems: &mut Vec<String>| {
        if let Some((number, line, stated)) = open.take() {
            let main: u32 = number.split('.').next().and_then(|n| n.parse().ok()).unwrap_or(0);
            if number.contains('.') && INDEXED.contains(&main) && !stated {
                problems.push(format!("design line {line}: the subsection {number} has no identity and no line `{NO_RULE}CAUSE` or `{NOT_INDEXED}CAUSE`"));
            }
        }
    };
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if let Some(found) = heading(line) {
            close(&mut open, &mut design.problems);
            section = found.to_string();
            open = Some((section.clone(), number, false));
            continue;
        }
        for (prefix, list) in [(NO_RULE, &mut design.no_rule), (NOT_INDEXED, &mut design.not_indexed)] {
            if let Some(cause) = line.strip_prefix(prefix) {
                if cause.split_whitespace().count() < 3 {
                    design.problems.push(format!("design line {number}: the line `{prefix}…` needs a cause of three words or more"));
                }
                list.push(section.clone());
                if let Some(open) = open.as_mut() {
                    open.2 = true;
                }
            }
        }
        for identity in identities(line) {
            match identity {
                Err(problem) => design.problems.push(format!("design line {number}: {problem}")),
                Ok((name, no_negative)) => {
                    let of = name[1..].split('-').next().unwrap_or("");
                    if of != section {
                        design.problems.push(format!("design line {number}: the identity `{name}` is in the section {section}, not in the section of its number"));
                    }
                    if let Some(open) = open.as_mut() {
                        open.2 = true;
                    }
                    if let Some(first) = design.rules.get(&name) {
                        design.problems.push(format!("design line {number}: the identity `{name}` is also at line {}", first.line));
                    } else {
                        design.rules.insert(name, Rule { line: number, no_negative });
                    }
                }
            }
        }
    }
    close(&mut open, &mut design.problems);
    for number in design.no_rule.iter().chain(&design.not_indexed) {
        let prefix = format!("R{number}-");
        if design.rules.keys().any(|name| name.starts_with(&prefix)) {
            design.problems.push(format!("the subsection {number} has an identity and a line `Rule index: …`"));
        }
    }
    // The lock: only a subsection of the list can be "not indexed", and each entry of the list
    // is such a subsection.
    for number in &design.not_indexed {
        if !stay.iter().any(|(listed, _)| listed == number) {
            design.problems.push(format!("the subsection {number} has the line `{NOT_INDEXED}…`, and it is not in the list `STAY_NOT_INDEXED`: give an identity to each rule, or write `{NO_RULE}CAUSE`"));
        }
    }
    for (listed, cause) in stay {
        if !design.not_indexed.iter().any(|number| number == listed) {
            design.problems.push(format!("the list `STAY_NOT_INDEXED` names the subsection {listed}, and the design has no line `{NOT_INDEXED}…` for it"));
        }
        if cause.split_whitespace().count() < 3 {
            design.problems.push(format!("the list `STAY_NOT_INDEXED` needs a cause of three words or more for the subsection {listed}"));
        }
    }
    design
}

/// One marker of a test source: the identity, the side (`true` is the positive case), the place.
type Marker = (String, bool, String);

/// The identities of a marker text. A word that is not an identity is an error.
fn marker_names(text: &str, place: &str, problems: &mut Vec<String>) -> Vec<String> {
    let names: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    if names.is_empty() {
        problems.push(format!("{place}: a marker with no identity"));
    }
    for name in &names {
        if !is_identity(name) {
            problems.push(format!("{place}: `{name}` in a marker is not an identity (the form is `R55.4-1`)"));
        }
    }
    names.into_iter().filter(|name| is_identity(name)).collect()
}

/// The markers of a Rust source. A marker is a comment line above a `#[test]` line: only comment
/// lines and attribute lines can be between the two.
fn rust_markers(name: &str, text: &str, problems: &mut Vec<String>) -> Vec<Marker> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut markers = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let side = [("// rule+ ", true), ("// rule- ", false)].into_iter().find_map(|(prefix, side)| line.strip_prefix(prefix).map(|rest| (rest, side)));
        let Some((rest, side)) = side else { continue };
        let place = format!("{name}:{}", index + 1);
        let at_test = lines[index + 1..].iter().take_while(|next| next.starts_with("//") || next.starts_with("#[")).any(|next| *next == "#[test]");
        if !at_test {
            problems.push(format!("{place}: the marker is not above a `#[test]` line"));
        }
        markers.extend(marker_names(rest, &place, problems).into_iter().map(|identity| (identity, side, place.clone())));
    }
    markers
}

/// The markers of a case file: the header lines `rule+: ID` and `rule-: ID` before the first section.
fn case_markers(name: &str, text: &str, problems: &mut Vec<String>) -> Vec<Marker> {
    let mut markers = Vec::new();
    for (index, line) in text.lines().take_while(|line| !line.starts_with("=== ")).enumerate() {
        let side = [("rule+: ", true), ("rule-: ", false)].into_iter().find_map(|(prefix, side)| line.strip_prefix(prefix).map(|rest| (rest, side)));
        let Some((rest, side)) = side else { continue };
        let place = format!("{name}:{}", index + 1);
        markers.extend(marker_names(rest, &place, problems).into_iter().map(|identity| (identity, side, place.clone())));
    }
    markers
}

/// Each problem of the index: the design, and the markers of the sources (the name and the text of each file).
fn problems_of(design_text: &str, sources: &[(String, String)], stay: &[(&str, &str)]) -> (Design, Vec<Marker>, Vec<String>) {
    let mut design = read_design(design_text, stay);
    let mut problems = std::mem::take(&mut design.problems);
    let mut markers = Vec::new();
    for (name, text) in sources {
        if name.ends_with(".case") {
            markers.extend(case_markers(name, text, &mut problems));
        } else {
            markers.extend(rust_markers(name, text, &mut problems));
        }
    }
    let with = |side: bool| -> BTreeSet<&str> { markers.iter().filter(|marker| marker.1 == side).map(|marker| marker.0.as_str()).collect() };
    let (positive, negative) = (with(true), with(false));
    for (identity, _, place) in &markers {
        if !design.rules.contains_key(identity) {
            problems.push(format!("{place}: the marker names `{identity}`, and the design has no rule with this identity"));
        }
    }
    for (name, rule) in &design.rules {
        if !positive.contains(name.as_str()) {
            problems.push(format!("design line {}: the rule `{name}` has no test of the positive case (a marker `rule+ {name}`)", rule.line));
        }
        match (&rule.no_negative, negative.contains(name.as_str())) {
            (None, false) => problems.push(format!("design line {}: the rule `{name}` has no test of the negative case (a marker `rule- {name}`), and its identity gives no cause (`[{name}; no negative case: CAUSE]`)", rule.line)),
            (Some(_), true) => problems.push(format!("design line {}: the rule `{name}` has a test of the negative case, and its identity says `no negative case`: remove the cause", rule.line)),
            _ => {}
        }
    }
    (design, markers, problems)
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Each file below `folder` with one of the extensions. The folder `target` is not a source.
fn files(folder: &Path, extensions: &[&str], found: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(folder).unwrap_or_else(|error| panic!("read {}: {error}", folder.display())).map(|entry| entry.expect("a folder entry").path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "target") {
                files(&path, extensions, found);
            }
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.contains(&e)) {
            found.push(path);
        }
    }
}

// rule+ R57.6-1 R57.6-2 R57.6-3 R57.6-6
#[test]
fn each_rule_of_the_design_has_its_tests_and_each_marker_names_a_rule() {
    let root = root();
    let design_text = std::fs::read_to_string(root.join("docs/design.md")).expect("docs/design.md");
    let mut paths = Vec::new();
    files(&root.join("crates"), &["rs", "case"], &mut paths);
    let sources: Vec<(String, String)> = paths
        .iter()
        .map(|path| {
            let name = path.strip_prefix(&root).unwrap_or(path).display().to_string();
            // A case file can have bytes that are not UTF-8 text in a section. Its header is plain text.
            (name, String::from_utf8_lossy(&std::fs::read(path).expect("a source file")).into_owned())
        })
        .collect();
    let (design, markers, problems) = problems_of(&design_text, &sources, STAY_NOT_INDEXED);
    let waivers = design.rules.values().filter(|rule| rule.no_negative.is_some()).count();
    println!(
        "rule index: {} rule(s), {} marker(s) of the positive case, {} marker(s) of the negative case, {} rule(s) with no negative case and a cause, {} subsection(s) with no rule, {} subsection(s) not indexed ({})",
        design.rules.len(),
        markers.iter().filter(|marker| marker.1).count(),
        markers.iter().filter(|marker| !marker.1).count(),
        waivers,
        design.no_rule.len(),
        design.not_indexed.len(),
        design.not_indexed.join(", "),
    );
    assert!(design.rules.len() >= 6, "the design has the six core rules at least");
    assert!(problems.is_empty(), "{} problem(s) of the rule index:\n{}", problems.len(), problems.join("\n"));
}

/// A small design and one Rust source for the tests of the check.
fn sample(design: &str, source: &str) -> Vec<String> {
    sample_with(design, source, &[])
}

/// The same with a list of the subsections that stay "not indexed".
fn sample_with(design: &str, source: &str, stay: &[(&str, &str)]) -> Vec<String> {
    problems_of(design, &[("a.rs".to_string(), source.to_string())], stay).2
}

const GOOD_DESIGN: &str = "## 55. Positions\n\n### 55.4 The cover check\n\n- [R55.4-1] Each changed line is in a fact.\n- [R55.4-2; no negative case: the rule has one result only] The step prints one line.\n";
const GOOD_SOURCE: &str = "// rule+ R55.4-1 R55.4-2\n// rule- R55.4-1\n#[test]\nfn a() {}\n";

// rule+ R57.6-1 R57.6-2 R57.6-3
// rule- R57.6-1 R57.6-2 R57.6-3
#[test]
fn the_check_of_the_index_finds_each_class_of_problem() {
    assert_eq!(sample(GOOD_DESIGN, GOOD_SOURCE), Vec::<String>::new());
    // A rule with no test of the positive case.
    let problems = sample(GOOD_DESIGN, "// rule- R55.4-1\n#[test]\nfn a() {}\n");
    assert!(problems.len() == 2 && problems.iter().all(|p| p.contains("has no test of the positive case")), "{problems:#?}");
    // A rule with no test of the negative case and no cause.
    let problems = sample(GOOD_DESIGN, "// rule+ R55.4-1 R55.4-2\n#[test]\nfn a() {}\n");
    assert!(problems.len() == 1 && problems[0].contains("`R55.4-1` has no test of the negative case"), "{problems:#?}");
    // A cause that stays when a test of the negative case exists.
    let problems = sample(GOOD_DESIGN, &format!("{GOOD_SOURCE}// rule- R55.4-2\n#[test]\nfn b() {{}}\n"));
    assert!(problems.len() == 1 && problems[0].contains("remove the cause"), "{problems:#?}");
    // A marker that names an identity that the design does not have.
    let problems = sample(GOOD_DESIGN, &format!("{GOOD_SOURCE}// rule+ R55.4-9\n#[test]\nfn b() {{}}\n"));
    assert!(problems.len() == 1 && problems[0].contains("a.rs:5: the marker names `R55.4-9`"), "{problems:#?}");
    // A marker that is not above a test, and a marker with a word that is not an identity.
    let problems = sample(GOOD_DESIGN, &format!("{GOOD_SOURCE}// rule+ R55.4-1\nfn helper() {{}}\n"));
    assert!(problems.len() == 1 && problems[0].contains("is not above a `#[test]` line"), "{problems:#?}");
    let problems = sample(GOOD_DESIGN, &format!("{GOOD_SOURCE}// rule+ 55.4-1\n#[test]\nfn b() {{}}\n"));
    assert!(problems.len() == 1 && problems[0].contains("is not an identity"), "{problems:#?}");
}

// rule+ R57.6-4 R57.6-5
// rule- R57.6-4 R57.6-5
#[test]
fn the_check_of_the_index_reads_the_design() {
    // One identity at two places, and an identity in a different section.
    let problems = sample(&format!("{GOOD_DESIGN}- [R55.4-1] One more time.\n"), GOOD_SOURCE);
    assert!(problems.len() == 1 && problems[0].contains("is also at line 5"), "{problems:#?}");
    let problems = sample(&format!("{GOOD_DESIGN}\n### 55.5 Classes\n\n- [R55.4-3; no negative case: one result only] A rule.\n"), &format!("{GOOD_SOURCE}// rule+ R55.4-3\n#[test]\nfn b() {{}}\n"));
    assert!(problems.iter().any(|p| p.contains("`R55.4-3` is in the section 55.5")), "{problems:#?}");
    // A subsection of the indexed sections tells its state. A section before them does not need it.
    let problems = sample(&format!("{GOOD_DESIGN}\n### 55.5 Classes\n\nA text.\n\n### 12.1 Old\n\nA text.\n"), GOOD_SOURCE);
    assert!(problems.len() == 1 && problems[0].contains("the subsection 55.5 has no identity and no line"), "{problems:#?}");
    // The line "not indexed" is a state only for a subsection of the list (the lock, R57.6-6).
    let stay = [("55.5", "the rules wait for a decision")];
    for state in [NO_RULE, NOT_INDEXED] {
        assert_eq!(sample_with(&format!("{GOOD_DESIGN}\n### 55.5 Classes\n\n{state}the results of a measure.\n"), GOOD_SOURCE, if state == NOT_INDEXED { &stay } else { &[] }), Vec::<String>::new());
        let problems = sample_with(&format!("{GOOD_DESIGN}\n### 55.5 Classes\n\n{state}data\n"), GOOD_SOURCE, if state == NOT_INDEXED { &stay } else { &[] });
        assert!(problems.len() == 1 && problems[0].contains("needs a cause"), "{problems:#?}");
    }
    let problems = sample(&format!("{GOOD_DESIGN}\n{NO_RULE}the results of a measure.\n"), GOOD_SOURCE);
    assert!(problems.len() == 1 && problems[0].contains("has an identity and a line"), "{problems:#?}");
    // The cause of a rule with no negative case has a text.
    let problems = sample(&GOOD_DESIGN.replace("the rule has one result only", "none"), GOOD_SOURCE);
    assert!(problems.iter().any(|p| p.contains("a cause of three words or more")), "{problems:#?}");
    // A case file gives its markers in the header, before the first section.
    let case = "note: a case\nrule+: R55.4-1 R55.4-2\nrule-: R55.4-1\n=== both a\nrule+: R55.4-9\n=== expected\n";
    assert_eq!(problems_of(GOOD_DESIGN, &[("a.case".to_string(), case.to_string())], &[]).2, Vec::<String>::new());
    let design = read_design(GOOD_DESIGN, &[]);
    assert_eq!(design.rules["R55.4-2"], Rule { line: 6, no_negative: Some("the rule has one result only".to_string()) });
    assert_eq!((heading("### 55.4 The cover check"), heading("## 55. Positions"), heading("## Notes")), (Some("55.4"), Some("55"), None));
}

/// The lock of the index (design 57.6, batch 9t2): a subsection is "not indexed" only if the
/// list of this file names it. The negative test: a subsection of the list is no problem.
// rule+ R57.6-6
// rule- R57.6-6
#[test]
fn a_subsection_that_is_not_indexed_must_be_in_the_list() {
    let design = format!("{GOOD_DESIGN}\n### 55.5 Classes\n\n{NOT_INDEXED}the rules wait for a decision.\n");
    // The subsection is not in the list: a problem that names the subsection.
    let problems = sample(&design, GOOD_SOURCE);
    assert!(problems.len() == 1 && problems[0].contains("the subsection 55.5 has the line") && problems[0].contains("it is not in the list `STAY_NOT_INDEXED`"), "{problems:#?}");
    let problems = sample_with(&design, GOOD_SOURCE, &[("55.6", "a different subsection waits")]);
    assert!(problems.len() == 2 && problems[0].contains("the subsection 55.5 has the line") && problems[1].contains("names the subsection 55.6, and the design has no line"), "{problems:#?}");
    // Negative: the list names the subsection. The line is its state, and no problem shows.
    assert_eq!(sample_with(&design, GOOD_SOURCE, &[("55.5", "the rules wait for a decision")]), Vec::<String>::new());
    // The line "no rule" needs no entry of the list.
    assert_eq!(sample(&design.replace(NOT_INDEXED, NO_RULE), GOOD_SOURCE), Vec::<String>::new());
    // An entry of the list for a subsection that has its identities, or that has no such line.
    let problems = sample_with(GOOD_DESIGN, GOOD_SOURCE, &[("55.4", "the rules wait for a decision")]);
    assert!(problems.len() == 1 && problems[0].contains("names the subsection 55.4, and the design has no line"), "{problems:#?}");
    // An entry of the list needs a cause.
    let problems = sample_with(&design, GOOD_SOURCE, &[("55.5", "later")]);
    assert!(problems.len() == 1 && problems[0].contains("needs a cause of three words or more for the subsection 55.5"), "{problems:#?}");
    // The list of the product: each entry has a cause, and each one is a subsection of the indexed sections.
    for (number, cause) in STAY_NOT_INDEXED {
        let main: u32 = number.split('.').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        assert!(INDEXED.contains(&main) && number.contains('.') && cause.split_whitespace().count() >= 3, "{number}: {cause}");
    }
}
