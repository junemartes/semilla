//! [R99.8-1] The anchors of the places of a body record (design 99.8).
//!
//! A place of a body record (a byte offset or a line in a file) is relative to an item: the
//! innermost item of the table of its compile unit that holds the place. The key of a record
//! has each place as the item and the offset in the item, so a line that moves an item does
//! not change the key. The table of a tree gives the start of each item: Semilla adds the
//! start of the item in the new tree to each place of a stored record.
//!
//! The driver (the key) and Semilla (the move of a stored record) use this one file, so the
//! two find the same item for a place. The file uses only the standard library and `serde`.

use std::collections::HashMap;

/// One item of the table: its identity (the def path hash), its byte range in the file as
/// written, and its first and last line (from 1).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Anchor {
    pub id: String,
    pub lo: i64,
    pub hi: i64,
    pub line_lo: i64,
    pub line_hi: i64,
}

/// The items of one file. `names` has each text that names the file in a record (the path from
/// the root of the workspace, and the path that the compiler got).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileAnchors {
    pub names: Vec<String>,
    pub anchors: Vec<Anchor>,
}

/// The kind of a place: a byte offset or a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Byte,
    Line,
}

/// The nesting of the items of one file for one kind of place: the items in the order of
/// their start (a longer item first), and the item that holds each item.
#[derive(Debug, Default)]
struct Chain {
    /// The index of each item in the file, in the order of the walk.
    order: Vec<usize>,
    /// The start and the end of each item of `order`.
    ranges: Vec<(i64, i64)>,
    /// The position in `order` of the item that holds each item of `order`.
    parent: Vec<Option<usize>>,
}

impl Chain {
    fn new(anchors: &[Anchor], kind: Kind) -> Chain {
        let range = |anchor: &Anchor| match kind {
            Kind::Byte => (anchor.lo, anchor.hi),
            Kind::Line => (anchor.line_lo, anchor.line_hi),
        };
        let mut order: Vec<usize> = (0..anchors.len()).collect();
        // The start first, then the longer item (it holds the shorter one), then the identity:
        // one order for each set of items.
        order.sort_by(|a, b| {
            let (x, y) = (&anchors[*a], &anchors[*b]);
            let ((xl, xh), (yl, yh)) = (range(x), range(y));
            xl.cmp(&yl).then(yh.cmp(&xh)).then_with(|| x.lo.cmp(&y.lo)).then_with(|| y.hi.cmp(&x.hi)).then_with(|| x.id.cmp(&y.id))
        });
        let ranges: Vec<(i64, i64)> = order.iter().map(|at| range(&anchors[*at])).collect();
        let mut parent = Vec::with_capacity(order.len());
        let mut open: Vec<usize> = Vec::new();
        for (at, (lo, hi)) in ranges.iter().enumerate() {
            while let Some(top) = open.last() {
                let (top_lo, top_hi) = ranges[*top];
                if top_lo <= *lo && *hi <= top_hi {
                    break;
                }
                open.pop();
            }
            parent.push(open.last().copied());
            open.push(at);
        }
        Chain { order, ranges, parent }
    }

    /// The innermost item that holds `at` (its index in the file).
    fn find(&self, at: i64) -> Option<usize> {
        // The last item that starts at `at` or before it, then the items that hold it.
        let mut next = self.ranges.partition_point(|(lo, _)| *lo <= at).checked_sub(1);
        while let Some(position) = next {
            let (lo, hi) = self.ranges[position];
            if lo <= at && at <= hi {
                return Some(self.order[position]);
            }
            next = self.parent[position];
        }
        None
    }
}

/// The table of the items of a compile unit, with the search of the item of a place.
#[derive(Debug, Default)]
pub struct Table {
    pub files: Vec<FileAnchors>,
    by_name: HashMap<String, usize>,
    by_id: HashMap<String, (usize, usize)>,
    bytes: Vec<Chain>,
    lines: Vec<Chain>,
}

impl Table {
    pub fn new(files: Vec<FileAnchors>) -> Table {
        let mut by_name = HashMap::new();
        let mut by_id = HashMap::new();
        for (file, entry) in files.iter().enumerate() {
            for name in &entry.names {
                by_name.entry(name.clone()).or_insert(file);
            }
            for (at, anchor) in entry.anchors.iter().enumerate() {
                by_id.entry(anchor.id.clone()).or_insert((file, at));
            }
        }
        let bytes = files.iter().map(|entry| Chain::new(&entry.anchors, Kind::Byte)).collect();
        let lines = files.iter().map(|entry| Chain::new(&entry.anchors, Kind::Line)).collect();
        Table { files, by_name, by_id, bytes, lines }
    }

    /// The innermost item of the file `file` that holds the place `at`: the index of the file
    /// and of the item. `None` for a file with no item in the table, or a place out of each item.
    pub fn find(&self, kind: Kind, file: &str, at: i64) -> Option<(usize, usize)> {
        let index = *self.by_name.get(file)?;
        let chain = match kind {
            Kind::Byte => &self.bytes[index],
            Kind::Line => &self.lines[index],
        };
        chain.find(at).map(|anchor| (index, anchor))
    }

    pub fn anchor(&self, at: (usize, usize)) -> &Anchor {
        &self.files[at.0].anchors[at.1]
    }

    /// The item with an identity.
    pub fn by_id(&self, id: &str) -> Option<&Anchor> {
        self.by_id.get(id).map(|at| self.anchor(*at))
    }

    /// The start of an item for a kind of place.
    pub fn start(anchor: &Anchor, kind: Kind) -> i64 {
        match kind {
            Kind::Byte => anchor.lo,
            Kind::Line => anchor.line_lo,
        }
    }
}

/// [R99.8-1] The lines of the places in a text of the compiler: each `@FILE:LINE:COLUMN` and
/// `@FILE:LINE:COLUMN: LINE:COLUMN` in a `{…@…}` name (the name of a closure type, of a
/// coroutine, or of an `async` block). `change` gets the file and each line, and gives the
/// new line (or `None`: no change). The result is `None` if no line changes.
pub fn text_lines(text: &str, change: &mut dyn FnMut(&str, i64) -> Option<i64>) -> Option<String> {
    if !text.contains('@') {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut copied = 0usize;
    let mut changed = false;
    let mut at = 0usize;
    while let Some(found) = text[at..].find('@') {
        let sign = at + found;
        at = sign + 1;
        // The name starts with `{` and words: `{closure@`, `{async block@`, `{coroutine@`.
        let head = &text[..sign];
        let Some(open) = head.rfind('{') else { continue };
        if open + 1 == sign || !head[open + 1..].bytes().all(|byte| byte.is_ascii_lowercase() || byte == b' ' || byte == b'-') {
            continue;
        }
        // The file ends at the first `:` that a digit follows.
        let rest = &text[sign + 1..];
        let Some(colon) = rest.char_indices().find(|(index, ch)| *ch == ':' && rest.as_bytes().get(index + 1).is_some_and(u8::is_ascii_digit)).map(|(index, _)| index) else { continue };
        let file = &rest[..colon];
        if file.is_empty() || file.contains('}') {
            continue;
        }
        // The numbers: LINE:COLUMN, then `: LINE:COLUMN` if the name has an end.
        let mut lines: Vec<(usize, usize)> = Vec::new();
        let mut cursor = sign + 1 + colon + 1;
        let digits = |from: usize| -> usize { bytes[from..].iter().take_while(|byte| byte.is_ascii_digit()).count() };
        let first = digits(cursor);
        if first == 0 || bytes.get(cursor + first) != Some(&b':') {
            continue;
        }
        lines.push((cursor, cursor + first));
        cursor += first + 1;
        let column = digits(cursor);
        if column == 0 {
            continue;
        }
        cursor += column;
        if text[cursor..].starts_with(": ") {
            let second = digits(cursor + 2);
            if second > 0 && bytes.get(cursor + 2 + second) == Some(&b':') {
                lines.push((cursor + 2, cursor + 2 + second));
                cursor += 2 + second + 1;
                cursor += digits(cursor);
            }
        }
        if bytes.get(cursor) != Some(&b'}') {
            continue;
        }
        for (start, end) in lines {
            let Ok(line) = text[start..end].parse::<i64>() else { continue };
            if let Some(new) = change(file, line).filter(|new| *new != line) {
                out.push_str(&text[copied..start]);
                out.push_str(&new.to_string());
                copied = end;
                changed = true;
            }
        }
        at = cursor;
    }
    if !changed {
        return None;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(id: &str, lo: i64, hi: i64, line_lo: i64, line_hi: i64) -> Anchor {
        Anchor { id: id.into(), lo, hi, line_lo, line_hi }
    }

    fn table() -> Table {
        // A module (the file), a function with a closure in it, and a struct after it.
        let anchors = vec![anchor("m", 0, 200, 1, 20), anchor("f", 10, 90, 2, 8), anchor("c", 40, 60, 4, 4), anchor("s", 100, 150, 10, 14)];
        Table::new(vec![FileAnchors { names: vec!["src/lib.rs".into(), "./src/lib.rs".into()], anchors }])
    }

    // [R99.8-1] The item of a place is the innermost item that holds it, by bytes and by lines.
    // rule+ R99.8-1
    #[test]
    fn the_item_of_a_place_is_the_innermost_item_that_holds_it() {
        let table = table();
        let id = |kind: Kind, at: i64| table.find(kind, "src/lib.rs", at).map(|found| table.anchor(found).id.clone());
        assert_eq!(id(Kind::Byte, 45).as_deref(), Some("c"));
        assert_eq!(id(Kind::Byte, 60).as_deref(), Some("c"));
        assert_eq!(id(Kind::Byte, 61).as_deref(), Some("f"));
        assert_eq!(id(Kind::Byte, 95).as_deref(), Some("m"));
        assert_eq!(id(Kind::Byte, 120).as_deref(), Some("s"));
        assert_eq!(id(Kind::Line, 4).as_deref(), Some("c"));
        assert_eq!(id(Kind::Line, 9).as_deref(), Some("m"));
        assert_eq!(table.find(Kind::Line, "./src/lib.rs", 3).map(|found| table.anchor(found).id.clone()).as_deref(), Some("f"));
        assert!(table.find(Kind::Byte, "src/other.rs", 45).is_none());
        assert!(table.find(Kind::Byte, "src/lib.rs", 300).is_none());
    }

    // [R99.8-1] Only the lines of a `{…@FILE:LINE:COLUMN: LINE:COLUMN}` name change in a text.
    // rule- R99.8-1
    #[test]
    fn only_the_lines_of_a_place_name_change_in_a_text() {
        let mut add = |_: &str, line: i64| Some(line + 10);
        let text = "Guard<&mut T, {closure@src/raw.rs:840:37: 841:44}>, {async block@src/a.rs:3:1}";
        assert_eq!(text_lines(text, &mut add).as_deref(), Some("Guard<&mut T, {closure@src/raw.rs:850:37: 851:44}>, {async block@src/a.rs:13:1}"));
        for plain in ["user@host:12:3", "{closure#0}", "a {B@src/a.rs:1:2}", "{closure@src/a.rs:x:1}", "{closure@src/a.rs:1:2"] {
            assert_eq!(text_lines(plain, &mut add), None, "{plain}");
        }
    }
}
