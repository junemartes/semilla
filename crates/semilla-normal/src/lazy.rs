//! [R86.2-1] The large texts of a record that only some rules read (the forms `q` and `qx` of the
//! equivalence check, design 61). A record from a cache folder keeps the place of each such text
//! in its file, and the text loads on its first read. The file stays open, so a later build of
//! the folder (a new file) does not change the text of a record in memory.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};

/// The place of a JSON value in a record file.
#[derive(Debug)]
pub struct Place {
    pub file: Arc<std::fs::File>,
    pub offset: u64,
    pub len: usize,
}

/// A text of a record: known, or at a place of a file.
#[derive(Clone, Default)]
pub struct LazyText {
    value: OnceLock<Option<Arc<String>>>,
    place: Option<Arc<Place>>,
}

/// The count of the texts that loaded from their place (a test reads it).
pub static LOADED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl LazyText {
    /// A known text.
    pub fn known(value: Option<Arc<String>>) -> LazyText {
        LazyText { value: OnceLock::from(value), place: None }
    }

    /// A text at a place of a file.
    pub fn at(place: Place) -> LazyText {
        LazyText { value: OnceLock::new(), place: Some(Arc::new(place)) }
    }

    /// The text: from memory, or a read of its place. A place that does not read as a JSON text
    /// gives no text.
    pub fn as_ref(&self) -> Option<&Arc<String>> {
        self.value
            .get_or_init(|| {
                let place = self.place.as_ref()?;
                LOADED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut bytes = vec![0; place.len];
                std::os::unix::fs::FileExt::read_exact_at(place.file.as_ref(), &mut bytes, place.offset).ok()?;
                serde_json::from_slice::<Option<String>>(&bytes).ok()?.map(Arc::new)
            })
            .as_ref()
    }

    pub fn as_deref(&self) -> Option<&str> {
        self.as_ref().map(|text| text.as_str())
    }

    /// True if the record has the text. A text at a place is a text.
    pub fn is_some(&self) -> bool {
        match self.value.get() {
            Some(value) => value.is_some(),
            None => self.place.is_some(),
        }
    }

    pub fn is_none(&self) -> bool {
        !self.is_some()
    }

    /// True if the text is in memory (a test reads it).
    pub fn is_loaded(&self) -> bool {
        self.value.get().is_some()
    }

    /// The text as a shared value.
    pub fn cloned(&self) -> Option<Arc<String>> {
        self.as_ref().cloned()
    }
}

impl PartialEq for LazyText {
    fn eq(&self, other: &LazyText) -> bool {
        self.as_deref() == other.as_deref()
    }
}

impl Eq for LazyText {}

impl std::fmt::Debug for LazyText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_deref().fmt(f)
    }
}

impl Serialize for LazyText {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_deref().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LazyText {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<LazyText, D::Error> {
        Ok(LazyText::known(Option::<String>::deserialize(deserializer)?.map(Arc::new)))
    }
}

/// The members of the top object of a JSON line: the key and the byte range of the member
/// (from the key to the end of the value) and of the value. `None` if the line is not one
/// object with keys that are plain texts.
pub fn members(line: &[u8]) -> Option<Vec<(&str, std::ops::Range<usize>, std::ops::Range<usize>)>> {
    let mut at = skip_space(line, 0);
    if line.get(at) != Some(&b'{') {
        return None;
    }
    at = skip_space(line, at + 1);
    let mut out = Vec::new();
    if line.get(at) == Some(&b'}') {
        return (skip_space(line, at + 1) == line.len()).then_some(out);
    }
    loop {
        let key_start = at;
        let key_end = string_end(line, at)?;
        let key = std::str::from_utf8(&line[key_start + 1..key_end - 1]).ok()?;
        if key.contains('\\') {
            return None;
        }
        at = skip_space(line, key_end);
        if line.get(at) != Some(&b':') {
            return None;
        }
        let value_start = skip_space(line, at + 1);
        let value_end = value_end(line, value_start)?;
        out.push((key, key_start..value_end, value_start..value_end));
        at = skip_space(line, value_end);
        match line.get(at) {
            Some(b',') => at = skip_space(line, at + 1),
            Some(b'}') => return (skip_space(line, at + 1) == line.len()).then_some(out),
            _ => return None,
        }
    }
}

fn skip_space(line: &[u8], mut at: usize) -> usize {
    while line.get(at).is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r')) {
        at += 1;
    }
    at
}

/// The end (after the closing quote) of the JSON string at `at`.
fn string_end(line: &[u8], at: usize) -> Option<usize> {
    if line.get(at) != Some(&b'"') {
        return None;
    }
    let mut index = at + 1;
    while index < line.len() {
        match line[index] {
            b'\\' => index += 2,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

/// The end of the JSON value at `at`.
fn value_end(line: &[u8], at: usize) -> Option<usize> {
    match line.get(at)? {
        b'"' => string_end(line, at),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut index = at;
            while index < line.len() {
                match line[index] {
                    b'"' => {
                        index = string_end(line, index)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(index + 1);
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            None
        }
        _ => {
            let mut index = at;
            while index < line.len() && !matches!(line[index], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                index += 1;
            }
            (index > at).then_some(index)
        }
    }
}
