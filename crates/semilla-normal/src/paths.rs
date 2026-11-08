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
    if renames.is_empty() {
        return text.to_string();
    }
    Replacer::renames(renames).apply(text)
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
    mode: Mode,
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
        for (key, value) in pairs {
            if let Some(first) = key.chars().next() {
                buckets.entry(first).or_default().push((key.clone(), value.clone()));
            }
        }
        for keys in buckets.values_mut() {
            keys.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        }
        Replacer { buckets, mode }
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
        if self.buckets.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
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
            let position_ok = match self.mode {
                Mode::Functions => true,
                Mode::Rename => starts_token(text, candidate),
            };
            if position_ok {
                if let Some((key, value)) = self.find(&text[candidate..]) {
                    out.push_str(&text[copied..candidate]);
                    out.push_str(value);
                    index = candidate + key.len();
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
        out.push_str(&text[copied..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
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
