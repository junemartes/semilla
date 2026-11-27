//! Token-aware substitution of def paths in a text (a body name, a MIR text, or a term).
//!
//! A def path is a token: the character before it and the character after it are not identifier
//! characters. So `crate::foo` does not match in `crate::foo_bar` or in `other::crate::foo`.

use std::collections::HashMap;

/// Returns true for a character that can be part of an identifier.
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Returns true if a path can start at byte `index`: the character before is not an identifier
/// character and not a `:` (so the match is not the end of a longer path).
fn starts_token(text: &str, index: usize) -> bool {
    match text[..index].chars().next_back() {
        None => true,
        Some(c) => !is_ident_char(c) && c != ':',
    }
}

/// Applies a rename map to a text. A key matches a whole path, or the start of a longer path (a
/// child item, such as a method of a renamed type or a closure of a renamed function). So a
/// rename of `krate::Cart` also renames `krate::Cart::total`. The longest key wins.
pub fn rename(text: &str, renames: &[(String, String)]) -> String {
    rename_cow(text, renames).into_owned()
}

/// [`rename`] with no new text when no key matches (R86.3-1): the result borrows `text`.
pub fn rename_cow<'t>(text: &'t str, renames: &[(String, String)]) -> std::borrow::Cow<'t, str> {
    if renames.is_empty() {
        return std::borrow::Cow::Borrowed(text);
    }
    with_renames(renames, |replacer| replacer.apply_cow(text))
}

thread_local! {
    /// [R86.1-3] The replacer of the last rename map of this thread. A caller gives the same map
    /// for each text of a record set, and the build of the replacer is the larger part of the
    /// work. The key is the full map: another map makes a new replacer.
    static LAST_RENAMES: std::cell::RefCell<Option<(Vec<(String, String)>, std::rc::Rc<Replacer>)>> = const { std::cell::RefCell::new(None) };
}

thread_local! {
    /// The count of the replacers that [`with_renames`] built in this thread (a test reads it,
    /// R86.1-3).
    pub static RENAMES_BUILT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Runs `apply` with the replacer of a rename map: the replacer of the last call of this thread
/// if the map is equal, else a new one.
pub fn with_renames<R>(renames: &[(String, String)], apply: impl FnOnce(&Replacer) -> R) -> R {
    let known = LAST_RENAMES.with(|last| last.borrow().as_ref().filter(|(map, _)| map.as_slice() == renames).map(|(_, replacer)| replacer.clone()));
    let replacer = match known {
        Some(replacer) => replacer,
        None => {
            RENAMES_BUILT.with(|built| built.set(built.get() + 1));
            let made = std::rc::Rc::new(Replacer::renames(renames));
            LAST_RENAMES.with(|last| *last.borrow_mut() = Some((renames.to_vec(), made.clone())));
            made
        }
    };
    apply(&replacer)
}

/// Replaces each exact reference to a function (not a longer path that starts with it) by a
/// replacement text. A generic argument list (`::<`) may follow the function path.
///
/// Only a reference in the position of a function item matches: the driver prints a function
/// item as `{path}` (in the type `fn(u32) -> u32 {path}` and in a function pointer constant).
/// A struct can have the same path as a function (types and values have separate namespaces),
/// and a type reference must not get the hash of the function.
pub fn replace_functions(text: &str, replacements: &[(String, String)]) -> String {
    if replacements.is_empty() {
        return text.to_string();
    }
    Replacer::functions(replacements).apply(text)
}

/// Which matches a `Replacer` makes.
#[derive(Clone, Copy)]
enum Mode {
    /// See `rename`.
    Rename,
    /// See `replace_functions`.
    Functions,
}

/// A prepared set of replacements. Build it once and apply it to many texts. The keys are in
/// buckets by their first character, longest key first, so a position costs one map lookup.
pub struct Replacer {
    buckets: HashMap<char, Vec<(String, String)>>,
    /// The ASCII characters that start a key of `buckets` or of `generic` (R86.3-3): a position
    /// with another ASCII character needs no look-up.
    ascii_starts: [bool; 128],
    /// The generic keys (`GENERIC_KEY`): the segments of the key and of the value.
    generic: HashMap<char, Vec<(Vec<String>, Vec<String>)>>,
    mode: Mode,
}

/// The mark of a generic key of a rename map (design 37.4). The key `<>a::Type::method` matches
/// the path `a::Type::<A, B>::method`: a list of generic arguments can follow each segment but
/// the last one, and one list or more must be there. The value gets each list at the same
/// distance from its end. No def path starts with `<>`.
pub const GENERIC_KEY: &str = "<>";

/// The segments of a def path. A `::` in `<…>` or in `{…}` does not split.
fn segments(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'<' | b'{' | b'(' | b'[' => depth += 1,
            b'>' if index > 0 && bytes[index - 1] == b'-' => {}
            b'>' | b'}' | b')' | b']' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes.get(index + 1) == Some(&b':') => {
                out.push(path[start..index].to_string());
                index += 2;
                start = index;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    out.push(path[start..].to_string());
    out
}

/// The length of the list of generic arguments (`::<…>`) at the start of a text, if there is one.
fn generic_list(text: &str) -> Option<usize> {
    if !text.starts_with("::<") {
        return None;
    }
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for index in 2..bytes.len() {
        match bytes[index] {
            b'<' => depth += 1,
            // The arrow of a function type is no bracket.
            b'>' if bytes[index - 1] == b'-' => {}
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The match of a generic key at the start of a text: the length of the match, and the list of
/// generic arguments after each segment. `None` if the text has no list between the segments.
fn generic_match<'a>(text: &'a str, key: &[String]) -> Option<(usize, Vec<&'a str>)> {
    let mut at = 0;
    let mut lists = Vec::with_capacity(key.len());
    let mut found = false;
    for (index, segment) in key.iter().enumerate() {
        if index > 0 {
            if !text[at..].starts_with("::") {
                return None;
            }
            at += 2;
        }
        if !text[at..].starts_with(segment.as_str()) {
            return None;
        }
        at += segment.len();
        let mut list = "";
        if index + 1 < key.len() {
            if let Some(len) = generic_list(&text[at..]) {
                list = &text[at..at + len];
                at += len;
                found = true;
            }
        }
        lists.push(list);
    }
    found.then_some((at, lists))
}

/// The start of the text that a generic key matches, with its lists of generic arguments
/// (for example `a::Type::<T>::method` of `a::Type::<T>::method::{closure#0}`). `None` if the
/// key does not match the text from its start to the end of a path segment.
pub fn generic_prefix<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let (len, _) = generic_match(text, &segments(key))?;
    let end_ok = text[len..].chars().next().is_none_or(|c| !is_ident_char(c));
    end_ok.then(|| &text[..len])
}

impl Replacer {
    /// The replacer of `rename`.
    pub fn renames(pairs: &[(String, String)]) -> Replacer {
        Replacer::new(pairs, Mode::Rename)
    }

    /// The replacer of `replace_functions`.
    pub fn functions(pairs: &[(String, String)]) -> Replacer {
        Replacer::new(pairs, Mode::Functions)
    }

    fn new(pairs: &[(String, String)], mode: Mode) -> Replacer {
        let mut buckets: HashMap<char, Vec<(String, String)>> = HashMap::new();
        let mut generic: HashMap<char, Vec<(Vec<String>, Vec<String>)>> = HashMap::new();
        for (key, value) in pairs {
            if let Some(key) = key.strip_prefix(GENERIC_KEY) {
                let (from, to) = (segments(key), segments(value));
                if let Some(first) = key.chars().next() {
                    generic.entry(first).or_default().push((from, to));
                }
                continue;
            }
            if let Some(first) = key.chars().next() {
                buckets.entry(first).or_default().push((key.clone(), value.clone()));
            }
        }
        for keys in buckets.values_mut() {
            keys.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        }
        let mut ascii_starts = [false; 128];
        for first in buckets.keys().chain(generic.keys()).filter(|first| first.is_ascii()) {
            ascii_starts[*first as usize] = true;
        }
        Replacer { buckets, generic, mode, ascii_starts }
    }

    /// The match of a generic key at the start of `rest`: the length of the match and the new
    /// text. Each list of generic arguments goes to the segment at the same distance from the end.
    fn find_generic(&self, rest: &str) -> Option<(usize, String)> {
        let first = rest.chars().next()?;
        for (from, to) in self.generic.get(&first)? {
            let Some((len, lists)) = generic_match(rest, from) else { continue };
            if !self.end_ok(&rest[len..]) {
                continue;
            }
            let mut out: Vec<String> = to.clone();
            let mut fits = true;
            for (index, list) in lists.iter().enumerate().filter(|(_, list)| !list.is_empty()) {
                // The distance from the end of the path.
                let distance = from.len() - 1 - index;
                match to.len().checked_sub(1 + distance) {
                    Some(target) => out[target].push_str(list),
                    None => fits = false,
                }
            }
            if fits {
                return Some((len, out.join("::")));
            }
        }
        None
    }

    /// Returns true if the text after a match ends the matched path correctly.
    fn end_ok(&self, rest: &str) -> bool {
        if let Mode::Functions = self.mode {
            if let Some(after) = rest.strip_prefix("::") {
                return after.starts_with('<');
            }
        }
        match rest.chars().next() {
            None => true,
            Some(c) => !is_ident_char(c),
        }
    }

    /// Returns the key and the value that match at the start of `rest`, if any.
    fn find(&self, rest: &str) -> Option<&(String, String)> {
        let first = rest.chars().next()?;
        self.buckets
            .get(&first)?
            .iter()
            .find(|(key, _)| rest.starts_with(key.as_str()) && self.end_ok(&rest[key.len()..]))
    }

    /// Applies the replacements to a text.
    pub fn apply(&self, text: &str) -> String {
        self.apply_cow(text).into_owned()
    }

    /// [`Replacer::apply`] with no new text when no key matches (R86.3-1).
    pub fn apply_cow<'t>(&self, text: &'t str) -> std::borrow::Cow<'t, str> {
        if self.buckets.is_empty() && self.generic.is_empty() {
            return std::borrow::Cow::Borrowed(text);
        }
        let mut out = String::new();
        let mut copied = 0;
        let mut index = 0;
        while index < text.len() {
            // The next position where a match can start.
            let candidate = match self.mode {
                Mode::Functions => match text[index..].find('{') {
                    Some(brace) => index + brace + 1,
                    None => break,
                },
                Mode::Rename => index,
            };
            if candidate >= text.len() {
                break;
            }
            // No key starts with this ASCII character: no match here.
            let byte = text.as_bytes()[candidate];
            if byte.is_ascii() && !self.ascii_starts[byte as usize] {
                index = match self.mode {
                    Mode::Functions => candidate,
                    Mode::Rename => candidate + 1,
                };
                continue;
            }
            let position_ok = match self.mode {
                Mode::Functions => true,
                Mode::Rename => starts_token(text, candidate),
            };
            if position_ok {
                if let Some((key, value)) = self.find(&text[candidate..]) {
                    if out.capacity() == 0 {
                        out.reserve(text.len() + 16);
                    }
                    out.push_str(&text[copied..candidate]);
                    out.push_str(value);
                    index = candidate + key.len();
                    copied = index;
                    continue;
                }
                if let Some((len, value)) = self.find_generic(&text[candidate..]) {
                    if out.capacity() == 0 {
                        out.reserve(text.len() + 16);
                    }
                    out.push_str(&text[copied..candidate]);
                    out.push_str(&value);
                    index = candidate + len;
                    copied = index;
                    continue;
                }
            }
            index = match self.mode {
                Mode::Functions => candidate,
                Mode::Rename => {
                    candidate + text[candidate..].chars().next().expect("a character").len_utf8()
                }
            };
        }
        if copied == 0 {
            return std::borrow::Cow::Borrowed(text);
        }
        out.push_str(&text[copied..]);
        std::borrow::Cow::Owned(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    // [R86.1-3] One replacer for each rename map of a thread.
    // rule+ R86.1-3
    #[test]
    fn the_replacer_of_a_rename_map_is_built_one_time() {
        let renames = map(&[("k::once", "k::bar")]);
        let built = || RENAMES_BUILT.with(std::cell::Cell::get);
        let before = built();
        for _ in 0..5 {
            assert_eq!(rename("k::once(_1)", &renames), "k::bar(_1)");
        }
        assert_eq!(built() - before, 1);
    }

    // rule- R86.1-3
    #[test]
    fn another_rename_map_gets_its_own_replacer() {
        let (one, two) = (map(&[("k::foo", "k::bar")]), map(&[("k::foo", "k::baz")]));
        let built = || RENAMES_BUILT.with(std::cell::Cell::get);
        let before = built();
        assert_eq!(rename("k::foo(_1)", &one), "k::bar(_1)");
        // The key is the full map: a stale replacer would give `k::bar` here.
        assert_eq!(rename("k::foo(_1)", &two), "k::baz(_1)");
        assert_eq!(rename("k::foo(_1)", &one), "k::bar(_1)");
        assert_eq!(built() - before, 3);
    }

    // [R86.3-1] A rename with no match makes no new text.
    // rule+ R86.3-1
    #[test]
    fn a_rename_with_no_match_borrows_the_text() {
        let renames = map(&[("k::foo", "k::bar")]);
        assert!(matches!(rename_cow("k::other(_1)", &renames), std::borrow::Cow::Borrowed(_)));
        assert!(matches!(rename_cow("k::foo(_1)", &[]), std::borrow::Cow::Borrowed(_)));
        assert!(matches!(Replacer::functions(&map(&[("k::f", "#1")])).apply_cow("fn() {k::g}"), std::borrow::Cow::Borrowed(_)));
    }

    // rule- R86.3-1
    #[test]
    fn a_rename_with_a_match_makes_the_new_text() {
        let renames = map(&[("k::foo", "k::bar")]);
        assert_eq!(rename_cow("x k::foo(_1)", &renames), std::borrow::Cow::<str>::Owned("x k::bar(_1)".to_string()));
        assert_eq!(Replacer::functions(&map(&[("k::f", "#1")])).apply_cow("a {{k::f}"), "a {{#1}");
    }

    // [R86.3-3] A position whose ASCII character starts no key needs no look-up; each other
    // position is a look-up as before.
    // rule+ R86.3-3
    #[test]
    fn the_scan_skips_only_the_characters_that_start_no_key() {
        let renames = map(&[("k::foo", "k::bar"), ("é::x", "é::y")]);
        assert_eq!(rename("(k::foo, é::x, zk::foo)", &renames), "(k::bar, é::y, zk::foo)");
    }

    // rule- R86.3-3
    #[test]
    fn a_key_with_a_start_character_of_the_text_still_matches() {
        // Each start character of a key is in the table: `_` and a digit too.
        let renames = map(&[("_k::a", "_k::b"), ("9", "8")]);
        assert_eq!(rename("(_k::a, 9)", &renames), "(_k::b, 8)");
    }

    #[test]
    fn rename_matches_whole_tokens_only() {
        let renames = map(&[("k::foo", "k::bar")]);
        assert_eq!(rename("k::foo(_1)", &renames), "k::bar(_1)");
        assert_eq!(rename("k::foo_bar(_1)", &renames), "k::foo_bar(_1)");
        assert_eq!(rename("x::k::foo", &renames), "x::k::foo");
        assert_eq!(rename("{k::foo}", &renames), "{k::bar}");
        assert_eq!(rename("\"const fn() {k::foo}\"", &renames), "\"const fn() {k::bar}\"");
    }

    #[test]
    fn rename_includes_child_paths() {
        let renames = map(&[("k::Cart", "k::Basket")]);
        assert_eq!(rename("k::Cart::total", &renames), "k::Basket::total");
        assert_eq!(rename("<k::Cart as Clone>::clone", &renames), "<k::Basket as Clone>::clone");
        assert_eq!(rename("k::Carts", &renames), "k::Carts");
    }

    #[test]
    fn longest_key_wins() {
        let renames = map(&[("k::a", "k::x"), ("k::a::b", "k::y")]);
        assert_eq!(rename("k::a::b k::a::c", &renames), "k::y k::x::c");
    }

    #[test]
    fn function_replacement_is_exact() {
        let hashes = map(&[("k::f", "#abc")]);
        assert_eq!(replace_functions("fn() {k::f}", &hashes), "fn() {#abc}");
        assert_eq!(replace_functions("{k::f::<u32>}", &hashes), "{#abc::<u32>}");
        assert_eq!(replace_functions("{k::f::{closure#0}}", &hashes), "{k::f::{closure#0}}");
        assert_eq!(replace_functions("{k::f::g}", &hashes), "{k::f::g}");
        assert_eq!(replace_functions("{k::ff}", &hashes), "{k::ff}");
    }

    #[test]
    fn function_replacement_skips_type_positions() {
        // A struct `k::f` in a type position keeps its path.
        let hashes = map(&[("k::f", "#abc")]);
        assert_eq!(replace_functions("let _1: k::f; {k::f}", &hashes), "let _1: k::f; {#abc}");
    }
}

/// A type text with no lifetime (design 37.2): `<&'a mut a::Node as a::Parse>::parse` gives
/// `<&mut a::Node as a::Parse>::parse`, and `(dyn a::Shape + 'static)` gives `(dyn a::Shape)`.
pub fn no_lifetimes(text: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\'') {
        let name = rest[at + 1..].find(|c: char| !word(c)).unwrap_or(rest.len() - at - 1);
        let after = &rest[at + 1 + name..];
        if name == 0 || after.starts_with('\'') {
            // A character literal, not a lifetime.
            out.push_str(&rest[..at + 1]);
            rest = &rest[at + 1..];
            continue;
        }
        // A bound (`+ 'a`) goes away with its `+`.
        let before = rest[..at].trim_end();
        out.push_str(before.strip_suffix('+').map_or(&rest[..at], str::trim_end));
        rest = after.strip_prefix(' ').unwrap_or(after);
        // An argument of a list goes away with its comma. An empty list goes away.
        if let Some(next) = rest.strip_prefix(',') {
            rest = next.trim_start();
        } else if rest.starts_with('>') {
            let kept = out.trim_end().strip_suffix(',').map(str::len);
            out.truncate(kept.unwrap_or(out.len()));
            if let Some(kept) = out.strip_suffix("::<").or_else(|| out.strip_suffix('<')).map(str::len) {
                out.truncate(kept);
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}
