//! Token-aware substitution of def paths in a text (a body name, a MIR text, or a term).
//!
//! A def path is a token: the character before it and the character after it are not identifier
//! characters. So `crate::foo` does not match in `crate::foo_bar` or in `other::crate::foo`.

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
    replace_tokens(text, renames, |rest| match rest.chars().next() {
        None => true,
        Some(c) => !is_ident_char(c),
    })
}

/// Replaces each exact reference to a function (not a longer path that starts with it) by a
/// replacement text. A generic argument list (`::<`) may follow the function path.
///
/// Only a reference in the position of a function item matches: the driver prints a function
/// item as `{path}` (in the type `fn(u32) -> u32 {path}` and in a function pointer constant).
/// A struct can have the same path as a function (types and values have separate namespaces),
/// and a type reference must not get the hash of the function.
pub fn replace_functions(text: &str, replacements: &[(String, String)]) -> String {
    replace_tokens_after(text, replacements, Some('{'), |rest| {
        if let Some(after) = rest.strip_prefix("::") {
            return after.starts_with('<');
        }
        match rest.chars().next() {
            None => true,
            Some(c) => !is_ident_char(c),
        }
    })
}

/// Replaces the keys in `text` (the longest first). `end_ok` checks the text after a match.
fn replace_tokens(text: &str, pairs: &[(String, String)], end_ok: impl Fn(&str) -> bool) -> String {
    replace_tokens_after(text, pairs, None, end_ok)
}

/// Like `replace_tokens`, but if `before` is set, a match must come right after that character.
fn replace_tokens_after(
    text: &str,
    pairs: &[(String, String)],
    before: Option<char>,
    end_ok: impl Fn(&str) -> bool,
) -> String {
    if pairs.is_empty() {
        return text.to_string();
    }
    let mut sorted: Vec<&(String, String)> =
        pairs.iter().filter(|(key, _)| !key.is_empty()).collect();
    sorted.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let position_ok = match before {
            Some(wanted) => text[..index].ends_with(wanted),
            None => starts_token(text, index),
        };
        if position_ok {
            let rest = &text[index..];
            let found = sorted
                .iter()
                .find(|(key, _)| rest.starts_with(key.as_str()) && end_ok(&rest[key.len()..]));
            if let Some((key, value)) = found {
                out.push_str(value);
                index += key.len();
                continue;
            }
        }
        let c = text[index..].chars().next().expect("index is on a character boundary");
        out.push(c);
        index += c.len_utf8();
    }
    out
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
