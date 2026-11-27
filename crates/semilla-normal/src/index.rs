//! The index of a record file (design 88, F3).
//!
//! The driver writes one JSON line for each body and each level. The index of a record file has,
//! for each line: the identity of the body (the name, the level, the target, and the unit
//! settings of the file), the place of the line in the file, the places of the two forms of the
//! equivalence check (`q`, `qx`), a digest of the text and of the shape, the paths that the text
//! names, and the other members of the line as one JSON object (the small members that the
//! passes over all bodies read). Semilla makes the index from the record file, so the driver does
//! not change. A read of the index needs no scan of the record file.
//!
//! [R88.2-1] The index gives the records of the file: each record is equal to the record of a
//! full read (`read_records`).

use std::sync::{Arc, OnceLock};

use sha2::{Digest as _, Sha256};

use crate::BodyRecord;

/// The format key of an index (R88.4-1). A change of the content or of the encoding of an
/// index needs a new key: an entry with another key is built again.
pub const FORMAT: &str = "semilla-index-1";

/// A digest of a text: the first 16 bytes of its SHA-256 digest.
pub type Digest = [u8; 16];

/// The digest of a text.
pub fn digest(text: &str) -> Digest {
    digest_bytes(text.as_bytes())
}

/// The digest of bytes.
pub fn digest_bytes(bytes: &[u8]) -> Digest {
    let full = Sha256::digest(bytes);
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

/// The facts of the text of a record that a compare reads before the text (R88.3-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextFacts {
    /// The digest of the text.
    pub text: Digest,
    /// The digest of the shape.
    pub shape: Digest,
    /// The paths that the text names: each run of identifier characters and `:` with `::` in
    /// it, sorted, each one time.
    pub paths: Vec<String>,
}

impl TextFacts {
    /// The facts of a text and a shape.
    pub fn of(text: &str, shape: &str) -> TextFacts {
        TextFacts { text: digest(text), shape: digest(shape), paths: path_runs(text) }
    }
}

/// True for a character of a path run: an identifier character or `:` (as `paths::rename`
/// finds the start of a path).
fn path_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == ':'
}

/// The paths of a text: each maximal run of path characters with `::` in it, sorted, each one
/// time.
pub fn path_runs(text: &str) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut start = None;
    for (at, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        match (path_char(c), start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                let run = &text[from..at];
                if run.contains("::") {
                    out.insert(run);
                }
                start = None;
            }
            _ => {}
        }
    }
    out.into_iter().map(str::to_string).collect()
}

/// True if a key of the rename map can match a text with these paths. A key of
/// `paths::rename` matches only where a path starts, so a match of a key that starts with
/// its path part `P` (with `::` in it) is in a path of the text that starts with `P`. A key
/// whose path part has no `::` (a generic key, a key that starts with `<`) can match: the
/// answer is true. So the answer is false only if no key matches (R88.3-2).
pub fn may_rename(paths: &[String], renames: &[(String, String)]) -> bool {
    renames.iter().any(|(key, _)| {
        let end = key.char_indices().find(|(_, c)| !path_char(*c)).map_or(key.len(), |(at, _)| at);
        let part = &key[..end];
        if !part.contains("::") {
            return true;
        }
        // The paths are sorted: the first path that is not less than the part is the only
        // candidate for a prefix of each path after it that starts with the part.
        let from = paths.partition_point(|path| path.as_str() < part);
        paths.get(from).is_some_and(|path| path.starts_with(part))
    })
}

/// The facts of a record: from the index, or made from the text and the shape at the first
/// read. They are a function of the record, so two records with equal members are equal with
/// any value of this member.
#[derive(Clone, Default)]
pub struct Known {
    facts: OnceLock<Arc<TextFacts>>,
    /// True if the index gave the facts.
    index: bool,
}

impl Known {
    /// Facts that the index gives.
    pub fn index(facts: Arc<TextFacts>) -> Known {
        Known { facts: OnceLock::from(facts), index: true }
    }

    /// The facts, made at the first read if the index did not give them.
    pub fn get(&self, text: &str, shape: &str) -> &Arc<TextFacts> {
        self.facts.get_or_init(|| Arc::new(TextFacts::of(text, shape)))
    }

    /// The facts of the index, or `None` for a record that did not come from an index.
    pub fn from_index(&self) -> Option<&Arc<TextFacts>> {
        if self.index { self.facts.get() } else { None }
    }
}

impl PartialEq for Known {
    fn eq(&self, _other: &Known) -> bool {
        true
    }
}

impl Eq for Known {}

impl std::fmt::Debug for Known {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Known")
    }
}

/// The place of a value in the record file: the offset and the length.
pub type Place = (u64, u64);

/// The state of a form of the equivalence check in a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// The line has no such member.
    Missing,
    /// The member is `null`.
    Null,
    /// The text is at this place.
    At(Place),
}

/// The index entry of one line of a record file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub level: String,
    pub target: String,
    /// The place of the line in the file.
    pub line: Place,
    pub q: Form,
    pub qx: Form,
    pub facts: Arc<TextFacts>,
    /// The members of the line other than `q` and `qx`, as one JSON object.
    pub rest: Vec<u8>,
}

/// The index of one record file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnitIndex {
    /// The build settings of the unit record of the file (see `BodyRecord::unit`).
    pub settings: String,
    pub entries: Vec<Entry>,
}

/// Builds the index of the bytes of a record file, and gives the records of the file with it.
/// `None` if a line does not have the plain form of R86.2-1: then the caller reads the file with
/// the full parse (`parse_records`), with its errors, and stores no index.
pub fn build(bytes: &[u8], file: Option<&Arc<std::fs::File>>) -> Option<(UnitIndex, Vec<BodyRecord>)> {
    let mut index = UnitIndex::default();
    let mut records = Vec::new();
    let mut settings = None;
    let mut offset = 0usize;
    for line in bytes.split(|byte| *byte == b'\n') {
        let start = offset;
        offset += line.len() + 1;
        let full = line.len();
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.starts_with(crate::UNIT_LINE.as_bytes()) {
            if settings.is_none() {
                settings = Some(crate::unit_settings(std::str::from_utf8(line).ok()?));
            }
            continue;
        }
        if line.trim_ascii().is_empty() {
            continue;
        }
        let members = crate::lazy::members(line)?;
        let forms: Vec<&(&str, std::ops::Range<usize>, std::ops::Range<usize>)> = members.iter().filter(|(key, _, _)| crate::lazy_member(key).is_some()).collect();
        // Two members with one key: the full parse gives its error.
        if [false, true].iter().any(|kind| forms.iter().filter(|(key, _, _)| crate::lazy_member(key) == Some(*kind)).count() > 1) {
            return None;
        }
        let mut rest = Vec::with_capacity(line.len());
        rest.push(b'{');
        for (key, member, _) in &members {
            if crate::lazy_member(key).is_none() {
                if rest.len() > 1 {
                    rest.push(b',');
                }
                rest.extend_from_slice(&line[member.clone()]);
            }
        }
        rest.push(b'}');
        let mut record: BodyRecord = serde_json::from_slice(&rest).ok()?;
        let (mut q, mut qx) = (Form::Missing, Form::Missing);
        for (key, _, value) in forms {
            let form = if &line[value.clone()] == b"null" {
                Form::Null
            } else {
                // The value must be a text: else the full parse gives its error.
                if line[value.start] != b'"' {
                    return None;
                }
                Form::At(((start + value.start) as u64, value.len() as u64))
            };
            if crate::lazy_member(key) == Some(false) {
                q = form;
            } else {
                qx = form;
            }
        }
        set_forms(&mut record, q, qx, file);
        let facts = Arc::new(TextFacts::of(&record.text, &record.shape));
        record.known = Known::index(facts.clone());
        index.entries.push(Entry { name: record.name.clone(), level: record.level.clone(), target: record.target.clone(), line: (start as u64, full as u64), q, qx, facts, rest });
        records.push(record);
    }
    index.settings = settings.unwrap_or_default();
    for record in records.iter_mut() {
        record.unit.clone_from(&index.settings);
    }
    Some((index, records))
}

/// Sets the two forms of a record from their states in the index.
fn set_forms(record: &mut BodyRecord, q: Form, qx: Form, file: Option<&Arc<std::fs::File>>) {
    let lazy = |form: Form| match (form, file) {
        (Form::At((offset, len)), Some(file)) => Some(crate::lazy::LazyText::at(crate::lazy::Place { file: file.clone(), offset, len: len as usize })),
        (Form::Null, _) => Some(crate::lazy::LazyText::known(None)),
        _ => None,
    };
    if let Some(text) = lazy(q) {
        record.q = text;
    }
    if let Some(text) = lazy(qx) {
        record.qx = text;
    }
}

/// The records of a file from its index. `file` is the open record file, for the forms that
/// load on their first read. `None` if a line of the index does not decode (then the caller
/// builds the index again).
pub fn records(index: &UnitIndex, file: Option<&Arc<std::fs::File>>) -> Option<Vec<BodyRecord>> {
    let mut out = Vec::with_capacity(index.entries.len());
    for entry in &index.entries {
        let mut record: BodyRecord = serde_json::from_slice(&entry.rest).ok()?;
        if matches!(entry.q, Form::At(_)) || matches!(entry.qx, Form::At(_)) {
            file?;
        }
        set_forms(&mut record, entry.q, entry.qx, file);
        record.unit.clone_from(&index.settings);
        record.known = Known::index(entry.facts.clone());
        out.push(record);
    }
    Some(out)
}

// ----- the encoding of an index -----

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn put_place(out: &mut Vec<u8>, place: Place) {
    out.extend_from_slice(&place.0.to_le_bytes());
    out.extend_from_slice(&place.1.to_le_bytes());
}

fn put_form(out: &mut Vec<u8>, form: Form) {
    match form {
        Form::Missing => out.push(0),
        Form::Null => out.push(1),
        Form::At(place) => {
            out.push(2);
            put_place(out, place);
        }
    }
}

/// The bytes of an index.
pub fn encode(index: &UnitIndex) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(&mut out, index.settings.as_bytes());
    out.extend_from_slice(&(index.entries.len() as u64).to_le_bytes());
    for entry in &index.entries {
        put_bytes(&mut out, entry.name.as_bytes());
        put_bytes(&mut out, entry.level.as_bytes());
        put_bytes(&mut out, entry.target.as_bytes());
        put_place(&mut out, entry.line);
        put_form(&mut out, entry.q);
        put_form(&mut out, entry.qx);
        out.extend_from_slice(&entry.facts.text);
        out.extend_from_slice(&entry.facts.shape);
        out.extend_from_slice(&(entry.facts.paths.len() as u64).to_le_bytes());
        for path in &entry.facts.paths {
            put_bytes(&mut out, path.as_bytes());
        }
        put_bytes(&mut out, &entry.rest);
    }
    out
}

/// A reader of the bytes of an index. Each read checks the bounds.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let out = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(out)
    }
    fn number(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.number()?).ok()?;
        self.take(len)
    }
    fn text(&mut self) -> Option<String> {
        Some(std::str::from_utf8(self.bytes()?).ok()?.to_string())
    }
    fn place(&mut self) -> Option<Place> {
        Some((self.number()?, self.number()?))
    }
    fn form(&mut self) -> Option<Form> {
        match self.take(1)?[0] {
            0 => Some(Form::Missing),
            1 => Some(Form::Null),
            2 => Some(Form::At(self.place()?)),
            _ => None,
        }
    }
    fn digest(&mut self) -> Option<Digest> {
        self.take(16)?.try_into().ok()
    }
}

/// The index of its bytes. `None` if the bytes are not an index of this format.
pub fn decode(bytes: &[u8]) -> Option<UnitIndex> {
    let mut reader = Reader { bytes, at: 0 };
    let settings = reader.text()?;
    let count = usize::try_from(reader.number()?).ok()?;
    let mut entries = Vec::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        let (name, level, target) = (reader.text()?, reader.text()?, reader.text()?);
        let line = reader.place()?;
        let (q, qx) = (reader.form()?, reader.form()?);
        let (text, shape) = (reader.digest()?, reader.digest()?);
        let paths = (0..usize::try_from(reader.number()?).ok()?).map(|_| reader.text()).collect::<Option<Vec<String>>>()?;
        let rest = reader.bytes()?.to_vec();
        entries.push(Entry { name, level, target, line, q, qx, facts: Arc::new(TextFacts { text, shape, paths }), rest });
    }
    (reader.at == bytes.len()).then_some(UnitIndex { settings, entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &std::path::Path, text: &str) -> std::path::PathBuf {
        let path = dir.join("k.lib.1.jsonl");
        std::fs::write(&path, text).unwrap();
        path
    }

    const LINES: &str = concat!(
        "{\"unit\":{\"target\":\"k.lib.1\",\"cfg\":[\"unix\",\"feature=\\\"x\\\"\"],\"files\":[]}}\n",
        "{\"name\":\"k::f\",\"level\":\"n0\",\"text\":\"fn f() -> u32 { k::g::h(_1) }\",\"shape\":\"ret\",\"q\":\"Q1\",\"qx\":null,\"callees\":[\"k::g::h\"]}\n",
        "{\"name\":\"k::g::h\",\"level\":\"n0\",\"text\":\"fn h() { core::panicking::panic() }\"}\n",
    );

    // [R88.2-1] The records from the index are the records of the full read.
    // rule+ R88.2-1
    #[test]
    fn the_records_of_the_index_are_the_records_of_the_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(dir.path(), LINES);
        let handle = Arc::new(std::fs::File::open(&path).unwrap());
        let (index, built) = build(&std::fs::read(&path).unwrap(), Some(&handle)).unwrap();
        let full = crate::read_records(dir.path()).unwrap();
        assert_eq!(built, full);
        let again = records(&decode(&encode(&index)).unwrap(), Some(&handle)).unwrap();
        assert_eq!(again, full);
        assert_eq!(again[0].q.as_deref(), Some("Q1"));
        assert_eq!(again[0].unit, "unix");
        assert_eq!(decode(&encode(&index)).unwrap(), index);
    }

    // rule- R88.2-1
    #[test]
    fn a_line_that_is_not_of_the_plain_form_gives_no_index() {
        let dir = tempfile::tempdir().unwrap();
        // Two members `q`: the full parse gives its error, and no index is made.
        let path = file(dir.path(), "{\"name\":\"k::f\",\"level\":\"n0\",\"text\":\"t\",\"q\":\"a\",\"q\":\"b\"}\n");
        assert!(build(&std::fs::read(&path).unwrap(), None).is_none());
        // A cut index is not an index.
        let (index, _) = build(LINES.as_bytes(), None).unwrap();
        let bytes = encode(&index);
        assert!(decode(&bytes[..bytes.len() - 1]).is_none());
        assert!(decode(&[bytes.as_slice(), b"x"].concat()).is_none());
    }

    // [R88.3-2] The paths of a text give each key of a rename map that can match.
    // rule+ R88.3-2
    #[test]
    fn a_key_that_the_rename_map_can_match_is_in_the_paths() {
        let text = "fn f() { _2 = k::g::h(move _1); _3 = <k::T as k::Tr>::m(); }";
        let paths = path_runs(text);
        for key in ["k::g::h", "k::g", "k::T", "k::Tr"] {
            let renames = vec![(key.to_string(), "k::z".to_string())];
            assert_ne!(crate::paths::rename(text, &renames), text, "{key}");
            assert!(may_rename(&paths, &renames), "{key}");
        }
    }

    // rule- R88.3-2
    #[test]
    fn a_key_that_no_path_of_the_text_starts_with_cannot_rename_the_text() {
        let text = "fn f() { _2 = k::g::h(move _1); }";
        let paths = path_runs(text);
        // `g::h` is in the text, but not at the start of a path: the rename map does not match.
        for key in ["k::x", "g::h", "k::g::hh"] {
            let renames = vec![(key.to_string(), "k::z".to_string())];
            assert_eq!(crate::paths::rename(text, &renames), text, "{key}");
            assert!(!may_rename(&paths, &renames), "{key}");
        }
        // A key with no `::` in its path part can match: the answer is true.
        assert!(may_rename(&paths, &[("<>k::g::h".to_string(), "k::z".to_string())]));
        assert!(may_rename(&paths, &[("k".to_string(), "z".to_string())]));
    }
}
