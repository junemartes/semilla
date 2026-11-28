//! [R99.8-1] The move of a stored body record to the places of a new tree (design 99.8).
//!
//! The key of a body record has each place relative to its item (`anchors.rs`), so a body
//! that only moves keeps its key. The store keeps the record as the driver wrote it, with the
//! items of its places: for each place in the order of [`walk`], the item that holds it in the
//! tree of the record. A pointer line in a new tree gets the stored record with each place
//! moved by the move of its item: the start of the item in the new tree (the table of the last
//! line of the record file) minus its start in the old tree. The result is the record of a
//! full run (S3 of design 93), byte for byte: the JSON members keep their order, and only the
//! numbers of the places change.

use semilla_normal::anchors::{Kind, Table, text_lines};
use serde_json::Value;

/// The items of the places of a stored record (the first line of its payload in the store).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Used {
    /// Each item: its identity, its first byte, and its first line in the tree of the record.
    pub(crate) a: Vec<(String, i64, i64)>,
    /// The item of each place in the order of the walk, as runs: the index in `a` (`-1`: no
    /// item) and the count of places.
    pub(crate) m: Vec<(i64, u32)>,
}

/// The members of a record with no place: the walk does not read them. The other members can
/// have the name of a closure type with its place (`{closure@FILE:LINE:COLUMN: …}`).
const NO_PLACE: [&str; 9] = ["name", "level", "text", "shape", "term", "target", "config", "cv", "sv"];

/// Calls `visit` for each place of a record, in one order for each text of the record: the
/// kind of the place, the file, and the value (`visit` can change it). Gives true if a value
/// changed.
pub(crate) fn walk(record: &mut Value, visit: &mut dyn FnMut(Kind, &str, &mut i64)) -> bool {
    let mut changed = false;
    let Value::Object(map) = record else { return false };
    let span_file = map.get("span").and_then(|span| span.get("file")).and_then(Value::as_str).unwrap_or_default().to_string();
    for (key, value) in map.iter_mut() {
        match key.as_str() {
            "span" => {
                let Value::Object(span) = value else { continue };
                for member in ["line_start", "line_end"] {
                    changed |= number(span.get_mut(member), Kind::Line, &span_file, visit);
                }
                if let Some(Value::Array(range)) = span.get_mut("range") {
                    for at in range.iter_mut() {
                        changed |= number(Some(at), Kind::Byte, &span_file, visit);
                    }
                }
            }
            "site" => {
                let Value::Array(site) = value else { continue };
                let file = site.first().and_then(Value::as_str).unwrap_or_default().to_string();
                changed |= number(site.get_mut(1), Kind::Line, &file, visit);
            }
            "names" => {
                let Value::Array(names) = value else { continue };
                for name in names.iter_mut() {
                    let Value::Object(name) = name else { continue };
                    changed |= number(name.get_mut("line"), Kind::Line, &span_file, visit);
                    if let Some(rival) = name.get_mut("rival") {
                        changed |= texts(rival, visit);
                    }
                }
            }
            "el" => changed |= elements(value, visit),
            "g" => changed |= graph(value, visit),
            "q" => changed |= inner(value, false, visit),
            "qx" => changed |= inner(value, true, visit),
            name if NO_PLACE.contains(&name) => {}
            _ => changed |= texts(value, visit),
        }
    }
    changed
}

/// One number of a place.
fn number(value: Option<&mut Value>, kind: Kind, file: &str, visit: &mut dyn FnMut(Kind, &str, &mut i64)) -> bool {
    let Some(value) = value else { return false };
    let Some(old) = value.as_i64() else { return false };
    let mut new = old;
    visit(kind, file, &mut new);
    if new != old {
        *value = Value::from(new);
        return true;
    }
    false
}

/// The file names of a list `f`.
fn files(object: &serde_json::Map<String, Value>) -> Vec<String> {
    object.get("f").and_then(Value::as_array).map(|list| list.iter().map(|file| file.as_str().unwrap_or_default().to_string()).collect()).unwrap_or_default()
}

/// The range `[file, start, end]` at `at` of a list (the file is an index into `files`; `-1`:
/// no range).
fn range(list: &mut [Value], at: usize, files: &[String], visit: &mut dyn FnMut(Kind, &str, &mut i64)) -> bool {
    let Some(file) = list.get(at).and_then(Value::as_i64).and_then(|index| usize::try_from(index).ok()).and_then(|index| files.get(index)) else { return false };
    let file = file.clone();
    let mut changed = false;
    for offset in [at + 1, at + 2] {
        changed |= number(list.get_mut(offset), Kind::Byte, &file, visit);
    }
    changed
}

/// The places of the element data (`el`, `spans.rs` of the driver).
fn elements(value: &mut Value, visit: &mut dyn FnMut(Kind, &str, &mut i64)) -> bool {
    let Value::Object(object) = value else { return false };
    let files = files(object);
    let mut changed = false;
    // `r`: `[first, count, file, start, end]`, then `[file, start, end]` of the macro call.
    if let Some(Value::Array(runs)) = object.get_mut("r") {
        for run in runs.iter_mut() {
            let Value::Array(run) = run else { continue };
            changed |= range(run, 2, &files, visit);
            if run.len() >= 8 {
                changed |= range(run, 5, &files, visit);
            }
        }
    }
    // `s`: `[path, file, start, end]`.
    if let Some(Value::Array(symbols)) = object.get_mut("s") {
        for symbol in symbols.iter_mut() {
            let Value::Array(symbol) = symbol else { continue };
            changed |= range(symbol, 1, &files, visit);
        }
    }
    // `l`: `[kind, file, start, end]`, then `[file, start, end]` of the type annotation.
    if let Some(Value::Array(locals)) = object.get_mut("l") {
        for local in locals.iter_mut() {
            let Value::Array(local) = local else { continue };
            if local.len() >= 4 {
                changed |= range(local, 1, &files, visit);
            }
            if local.len() >= 7 {
                changed |= range(local, 4, &files, visit);
            }
        }
    }
    changed
}

/// The places of the graph (`g`, `graph.rs` of the driver): the range and the macro call of
/// each point, and the callee texts of a point.
fn graph(value: &mut Value, visit: &mut dyn FnMut(Kind, &str, &mut i64)) -> bool {
    let Value::Object(object) = value else { return false };
    let files = files(object);
    let mut changed = false;
    if let Some(Value::Array(points)) = object.get_mut("p") {
        for point in points.iter_mut() {
            let Value::Object(point) = point else { continue };
            for member in ["r", "c"] {
                if let Some(Value::Array(found)) = point.get_mut(member) {
                    changed |= range(found, 0, &files, visit);
                }
            }
            if let Some(callees) = point.get_mut("x") {
                changed |= texts(callees, visit);
            }
        }
    }
    changed
}

/// A form of the equivalence check as a JSON text (`q`; `qx` is a map of forms).
fn inner(value: &mut Value, map: bool, visit: &mut dyn FnMut(Kind, &str, &mut i64)) -> bool {
    let Value::String(text) = value else { return false };
    let Ok(mut parsed) = serde_json::from_str::<Value>(text) else { return false };
    let changed = if map {
        let mut changed = false;
        if let Value::Object(forms) = &mut parsed {
            for form in forms.values_mut() {
                changed |= self::form(form, visit);
            }
        }
        changed
    } else {
        form(&mut parsed, visit)
    };
    if changed {
        *text = serde_json::to_string(&parsed).unwrap_or_default();
    }
    changed
}

/// The places of one form (`ir.rs` of the driver): the range and the macro call of each
/// statement and terminator, and the texts of the form.
fn form(value: &mut Value, visit: &mut dyn FnMut(Kind, &str, &mut i64)) -> bool {
    let Value::Object(object) = value else { return false };
    let files = files(object);
    let mut changed = false;
    for (key, member) in object.iter_mut() {
        if key == "b" {
            let Value::Array(blocks) = member else { continue };
            for block in blocks.iter_mut() {
                let Value::Object(block) = block else { continue };
                for (part, found) in block.iter_mut() {
                    match part.as_str() {
                        "s" => {
                            let Value::Array(statements) = found else { continue };
                            for statement in statements.iter_mut() {
                                changed |= placed(statement, &files, visit);
                            }
                        }
                        "t" => changed |= placed(found, &files, visit),
                        _ => changed |= texts(found, visit),
                    }
                }
            }
        } else if key != "f" {
            changed |= texts(member, visit);
        }
    }
    changed
}

/// A statement or a terminator of a form: its range `r`, its macro call `c`, and its texts (not
/// the message `m` of a panic).
fn placed(value: &mut Value, files: &[String], visit: &mut dyn FnMut(Kind, &str, &mut i64)) -> bool {
    let Value::Object(object) = value else { return false };
    let mut changed = false;
    for (key, member) in object.iter_mut() {
        match key.as_str() {
            "r" | "c" => {
                if let Value::Array(found) = member {
                    changed |= range(found, 0, files, visit);
                }
            }
            "m" => {}
            _ => changed |= texts(member, visit),
        }
    }
    changed
}

/// The lines of the place names in each text of a value (`anchors::text_lines`).
fn texts(value: &mut Value, visit: &mut dyn FnMut(Kind, &str, &mut i64)) -> bool {
    match value {
        Value::String(text) => {
            let found = text_lines(text, &mut |file, line| {
                let mut new = line;
                visit(Kind::Line, file, &mut new);
                Some(new)
            });
            match found {
                Some(new) => {
                    *text = new;
                    true
                }
                None => false,
            }
        }
        Value::Array(list) => {
            let mut changed = false;
            for item in list.iter_mut() {
                changed |= texts(item, visit);
            }
            changed
        }
        Value::Object(object) => {
            let mut changed = false;
            for item in object.values_mut() {
                changed |= texts(item, visit);
            }
            changed
        }
        _ => false,
    }
}

/// [R99.8-1] The items of the places of a record in the table of its tree. `None` for a text
/// that is not a JSON object.
pub(crate) fn used(record: &str, table: &Table) -> Option<Used> {
    let mut value: Value = serde_json::from_str(record).ok()?;
    if !value.is_object() {
        return None;
    }
    let mut used = Used::default();
    let mut index: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    walk(&mut value, &mut |kind, file, at| {
        let item = match table.find(kind, file, *at) {
            Some(found) => {
                let anchor = table.anchor(found);
                *index.entry(anchor.id.clone()).or_insert_with(|| {
                    used.a.push((anchor.id.clone(), anchor.lo, anchor.line_lo));
                    used.a.len() as i64 - 1
                })
            }
            None => -1,
        };
        match used.m.last_mut() {
            Some((last, count)) if *last == item => *count += 1,
            _ => used.m.push((item, 1)),
        }
    });
    Some(used)
}

/// [R99.8-1] The record of a stored record in a new tree: each place moves by the move of its
/// item. `Ok(None)`: no item moved, the stored text is the record. `Err`: an item of the record
/// is not in the table of the new tree, or the record does not have the places of `used` (the
/// caller then builds the record again).
pub(crate) fn relocate(record: &str, used: &Used, table: &Table) -> Result<Option<String>, String> {
    let mut moves: Vec<(i64, i64)> = Vec::with_capacity(used.a.len());
    for (id, lo, line) in &used.a {
        let anchor = table.by_id(id).ok_or_else(|| format!("the item {id} is not in the table of the tree"))?;
        moves.push((anchor.lo - lo, anchor.line_lo - line));
    }
    if moves.iter().all(|found| *found == (0, 0)) {
        return Ok(None);
    }
    let mut value: Value = serde_json::from_str(record).map_err(|error| format!("a stored record is not JSON: {error}"))?;
    let mut runs = used.m.iter().copied();
    let mut current: Option<(i64, u32)> = None;
    let mut missing = false;
    walk(&mut value, &mut |kind, _, at| {
        let item = loop {
            match current.as_mut() {
                Some((item, count)) if *count > 0 => {
                    *count -= 1;
                    break Some(*item);
                }
                _ => match runs.next() {
                    Some(run) => current = Some(run),
                    None => break None,
                },
            }
        };
        let Some(item) = item else {
            missing = true;
            return;
        };
        if let Some((bytes, lines)) = usize::try_from(item).ok().and_then(|item| moves.get(item)) {
            *at += match kind {
                Kind::Byte => bytes,
                Kind::Line => lines,
            };
        }
    });
    let rest = current.map_or(0, |(_, count)| count) + runs.map(|(_, count)| count).sum::<u32>();
    if missing || rest > 0 {
        return Err("the stored record does not have the places of its item list".into());
    }
    serde_json::to_string(&value).map(Some).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use semilla_normal::anchors::{Anchor, FileAnchors};

    fn table(shift: i64, lines: i64) -> Table {
        let anchors = vec![
            Anchor { id: "body".into(), lo: 100 + shift, hi: 200 + shift, line_lo: 10 + lines, line_hi: 20 + lines },
            Anchor { id: "type".into(), lo: 0, hi: 50, line_lo: 1, line_hi: 5 },
        ];
        Table::new(vec![FileAnchors { names: vec!["src/lib.rs".into()], anchors }])
    }

    /// A record whose body moves by `shift` bytes and `lines` lines, with a type above it that
    /// does not move.
    fn record(shift: i64, lines: i64) -> String {
        let at = |value: i64| value + shift;
        let line = |value: i64| value + lines;
        let q = serde_json::json!({"a": 1, "b": [{"s": [{"k": "x", "r": [0, at(120), at(125)]}], "t": {"k": "Ret", "r": [0, at(190), at(190)], "m": "{closure@src/lib.rs:2:1}"}}], "t": [{"Clo": {"g": format!("{{closure@src/lib.rs:{}:5: {}:9}}", line(12), line(12))}}], "f": ["src/lib.rs"]});
        serde_json::json!({
            "name": "k::f",
            "text": "fn _() {closure@src/lib.rs:12:5: 12:9}",
            "span": {"file": "src/lib.rs", "line_start": line(10), "line_end": line(20), "range": [at(100), at(200)]},
            "names": [{"word": "T", "line": 3, "count": 1}, {"word": "a", "line": line(11), "count": 1}],
            "el": {"n": 2, "f": ["src/lib.rs"], "r": [[0, 1, 0, at(120), at(125)], [1, 1, -1, 7, 8, 0, at(130), at(140)]], "s": [["k::f", 0, at(100), at(200)], ["k::T", 0, 0, 50]], "l": [[0], [1, 0, at(103), at(104), 0, at(106), at(109)]]},
            "g": {"p": [{"k": "entry", "r": [0, at(100), at(200)], "x": [format!("{{closure@src/lib.rs:{}:5: {}:9}}", line(12), line(12))]}], "f": ["src/lib.rs"]},
            "q": serde_json::to_string(&q).unwrap(),
        })
        .to_string()
    }

    // [R99.8-1] A stored record in a tree where its body moved is the record of that tree: each
    // place of the body moves, a place of a type that does not move stays.
    // rule+ R99.8-1
    #[test]
    fn a_stored_record_moves_to_the_places_of_the_new_tree() {
        let old = record(0, 0);
        let used = used(&old, &table(0, 0)).unwrap();
        assert_eq!(used.a.iter().map(|(id, _, _)| id.as_str()).collect::<Vec<_>>(), ["body", "type"]);
        assert_eq!(relocate(&old, &used, &table(0, 0)), Ok(None));
        assert_eq!(relocate(&old, &used, &table(17, 2)), Ok(Some(record(17, 2))));
    }

    // [R99.8-1] An item that the new tree does not have, or a record with other places than its
    // item list, gives no record (the caller builds it again).
    // rule- R99.8-1
    #[test]
    fn a_stored_record_with_an_unknown_item_or_other_places_does_not_move() {
        let old = record(0, 0);
        let used = used(&old, &table(0, 0)).unwrap();
        let other = Table::new(vec![FileAnchors { names: vec!["src/lib.rs".into()], anchors: vec![Anchor { id: "type".into(), lo: 3, hi: 50, line_lo: 1, line_hi: 5 }] }]);
        assert!(relocate(&old, &used, &other).is_err());
        let short = Used { a: used.a.clone(), m: vec![(0, 1)] };
        assert!(relocate(&old, &short, &table(5, 1)).is_err());
    }
}
