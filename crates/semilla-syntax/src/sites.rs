//! The places of a name in a text (design section 51).
//!
//! The map of a rename is a map of the references to an item. It is not a map of words. This
//! module reads a compare text (or a source text) and gives the place of each word: a
//! declaration, a member after `.`, a field label, a path with its qualifier, a macro name. A
//! word in a literal, in a comment, or in a lifetime has no place. The site table of
//! [`crate::Renames`] has the key of each place that is a site of a rename.

use crate::tree::EXPRESSION_MACROS;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Word,
    Literal,
    Lifetime,
    Comment,
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
    start: usize,
}

/// The kind of the place of a word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceKind {
    /// The name that a declaration gives (`fn name`, `struct name`, `const name :`).
    Decl,
    /// A member after `.`. `call`: a method call. `on_self`: the receiver is `self`.
    Member { call: bool, on_self: bool },
    /// A field label in a struct literal or a struct pattern. `owner` is the path of the
    /// literal (`None`: the path is not readable). `short`: the label has no `:` (`S { a }`).
    Key { owner: Option<String>, short: bool },
    /// A segment of a path. `qualifier` has the segments before the word (the empty text for a
    /// bare name, `None` when the text before `::` is not a plain path). `more`: `::` follows.
    Path { qualifier: Option<String>, more: bool },
    /// The name of a macro call (`name ! (…)`).
    MacroName,
    /// The new local name of a `use` line (`… as name`).
    Alias,
}

/// One word of a text, with its place.
#[derive(Clone, Debug)]
pub struct Place {
    pub start: usize,
    pub end: usize,
    pub name: String,
    pub kind: PlaceKind,
    /// The word is in the input of a macro that the syntax phase cannot read.
    pub opaque: bool,
}

impl Place {
    /// The key of the place in the site table of an item. Two places of one item with the same
    /// key name the same thing.
    pub fn key(&self) -> String {
        let name = &self.name;
        if self.opaque {
            return format!("o:{name}");
        }
        match &self.kind {
            PlaceKind::Decl => format!("d:{name}"),
            PlaceKind::Member { call, on_self } => format!("{}{}:{name}", if *call { "c" } else { "f" }, if *on_self { "s" } else { "" }),
            PlaceKind::Key { owner: Some(owner), .. } => format!("k:{owner}:{name}"),
            PlaceKind::Key { owner: None, .. } => format!("k:?:{name}"),
            PlaceKind::Path { qualifier: Some(qualifier), .. } => format!("p:{qualifier}:{name}"),
            PlaceKind::Path { qualifier: None, .. } => format!("q:{name}"),
            PlaceKind::MacroName => format!("m:{name}"),
            PlaceKind::Alias => format!("a:{name}"),
        }
    }
}

/// The first segment of a path that starts with `::`.
pub const ROOT: &str = "{root}";

fn word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// The index after the end of a quoted literal that starts at `start`.
fn quoted_end(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            byte if byte == quote => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

/// The tokens of a text. A string, a character, and a number are literals. `'a` is a lifetime.
fn lex(text: &str) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < len {
        let (start, byte) = (i, bytes[i]);
        let rest = &bytes[i..];
        let mut kind = Kind::Punct;
        if byte.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if rest.starts_with(b"//") {
            i = text[i..].find('\n').map_or(len, |at| i + at);
            kind = Kind::Comment;
        } else if rest.starts_with(b"/*") {
            let mut depth = 0usize;
            while i < len {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            kind = Kind::Comment;
        } else if byte == b'"' {
            i = quoted_end(bytes, i, b'"');
            kind = Kind::Literal;
        } else if byte == b'\'' {
            // A character literal (`'x'`, `'\n'`). Each other `'` starts a lifetime or a label.
            let width = match bytes.get(i + 1) {
                Some(b) if *b >= 0xF0 => 4,
                Some(b) if *b >= 0xE0 => 3,
                Some(b) if *b >= 0xC0 => 2,
                _ => 1,
            };
            if bytes.get(i + 1) == Some(&b'\\') {
                i = quoted_end(bytes, i, b'\'');
                kind = Kind::Literal;
            } else if bytes.get(i + 1 + width) == Some(&b'\'') {
                i += 2 + width;
                kind = Kind::Literal;
            } else {
                i += 1;
                while i < len && word_byte(bytes[i]) {
                    i += 1;
                }
                kind = Kind::Lifetime;
            }
        } else if byte.is_ascii_digit() {
            while i < len && word_byte(bytes[i]) {
                i += 1;
            }
            if i + 1 < len && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                i += 1;
                while i < len && word_byte(bytes[i]) {
                    i += 1;
                }
            }
            kind = Kind::Literal;
        } else if word_byte(byte) {
            while i < len && word_byte(bytes[i]) {
                i += 1;
            }
            let word = &text[start..i];
            let next = bytes.get(i).copied();
            kind = Kind::Word;
            if matches!(word, "b" | "c") && next == Some(b'"') {
                i = quoted_end(bytes, i, b'"');
                kind = Kind::Literal;
            } else if word == "b" && next == Some(b'\'') {
                i = quoted_end(bytes, i, b'\'');
                kind = Kind::Literal;
            } else if matches!(word, "r" | "br" | "cr") && matches!(next, Some(b'"' | b'#')) {
                let hashes = bytes[i..].iter().take_while(|b| **b == b'#').count();
                if bytes.get(i + hashes) == Some(&b'"') {
                    // A raw string: it ends at `"` with the same count of `#`.
                    let close = format!("\"{}", "#".repeat(hashes));
                    let body = i + hashes + 1;
                    i = text.get(body..).and_then(|tail| tail.find(&close)).map_or(len, |at| body + at + close.len());
                    kind = Kind::Literal;
                } else if word == "r" && hashes == 1 {
                    // A raw identifier (`r#name`): the word is the name.
                    let name = i + 1;
                    i = name;
                    while i < len && word_byte(bytes[i]) {
                        i += 1;
                    }
                    out.push(Token { kind: Kind::Word, text: &text[name..i], start: name });
                    continue;
                }
            }
        } else {
            i += [&b"..="[..], b"...", b"::", b"..", b"->", b"=>"].iter().find(|p| rest.starts_with(p)).map_or(1, |p| p.len());
        }
        i = i.min(len);
        while !text.is_char_boundary(i) {
            i += 1;
        }
        out.push(Token { kind, text: &text[start..i], start });
    }
    out
}

fn keyword(word: &str) -> bool {
    matches!(
        word,
        "as" | "async" | "await" | "break" | "const" | "continue" | "crate" | "dyn" | "else" | "enum" | "extern" | "false" | "fn" | "for" | "if" | "impl" | "in" | "let" | "loop" | "match" | "mod" | "move" | "mut" | "pub" | "ref" | "return" | "self" | "Self" | "static" | "struct" | "super" | "trait" | "true" | "type" | "unsafe" | "use" | "where" | "while"
    )
}

/// An open bracket.
struct Frame {
    /// A struct literal or a struct pattern (`Path { … }`).
    literal: bool,
    /// The path of the literal.
    owner: Option<String>,
    /// The input of a macro that the syntax phase cannot read.
    opaque: bool,
}

/// What comes before the next `{` of a depth.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// A condition (`if`, `while`, `match`): the `{` opens a block.
    Condition,
    /// A pattern (`for`, `if let`, `while let`) before `in` or `=`: a `{` is a struct pattern.
    Pattern,
    /// The header of an item (`fn`, `impl`, `trait`, `struct`, `enum`, `mod`): the `{` opens
    /// the body of the item.
    Header,
}

/// The places of the words of `text` for which `wanted` is true.
pub fn places(text: &str, wanted: &dyn Fn(&str) -> bool) -> Vec<Place> {
    let tokens: Vec<Token> = lex(text).into_iter().filter(|token| token.kind != Kind::Comment).collect();
    if !tokens.iter().any(|token| token.kind == Kind::Word && wanted(token.text)) {
        return Vec::new();
    }
    let word = |index: Option<usize>| index.and_then(|index| tokens.get(index)).filter(|token| token.kind == Kind::Word).map(|token| token.text);
    let punct = |index: Option<usize>, text: &str| index.and_then(|index| tokens.get(index)).is_some_and(|token| token.kind == Kind::Punct && token.text == text);
    // The segments of the path before the token `index` (`a :: b ::` gives `a::b`). `None`: the
    // text before `::` is not a plain path (`<T as Tr> ::`, `Vec :: <u8> ::`).
    let qualifier = |index: usize| -> Option<String> {
        let mut segments: Vec<&str> = Vec::new();
        let mut at = index;
        while punct(at.checked_sub(1), "::") {
            match at.checked_sub(2).map(|before| tokens[before]) {
                Some(token) if token.kind == Kind::Word => segments.push(token.text),
                Some(token) if token.kind == Kind::Punct && token.text == ">" => return None,
                _ => {
                    segments.push(ROOT);
                    break;
                }
            }
            at -= 2;
        }
        segments.reverse();
        Some(segments.join("::"))
    };
    let mut out = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut waits: Vec<(usize, Wait)> = Vec::new();
    // A `use` line: the depth of its start, and the prefix of each open group.
    let mut use_line: Option<(usize, Vec<String>)> = None;
    for (index, token) in tokens.iter().enumerate() {
        let before = index.checked_sub(1);
        let depth = stack.len();
        let wait_here = |waits: &[(usize, Wait)]| waits.last().copied().filter(|(at, _)| *at == depth).map(|(_, wait)| wait);
        match token.kind {
            Kind::Punct => match token.text {
                "{" | "(" | "[" => {
                    let inherited = stack.last().is_some_and(|frame| frame.opaque);
                    // `name ! (…)`: the input of a macro call. `macro_rules ! name { … }`: rules.
                    let call = punct(before, "!").then(|| word(index.checked_sub(2))).flatten();
                    let rules = punct(index.checked_sub(2), "!") && word(index.checked_sub(3)) == Some("macro_rules");
                    let opaque = inherited || rules || call.is_some_and(|name| !EXPRESSION_MACROS.contains(&name));
                    if token.text == "{" && !opaque && call.is_none() {
                        if let Some((_, prefixes)) = use_line.as_mut() {
                            // A group of a `use` line: the names of the group follow its prefix.
                            let outer = prefixes.last().cloned().unwrap_or_default();
                            let inner = qualifier(index).unwrap_or_default();
                            prefixes.push(if outer.is_empty() || inner.is_empty() { format!("{outer}{inner}") } else { format!("{outer}::{inner}") });
                            stack.push(Frame { literal: false, owner: None, opaque });
                            continue;
                        }
                    }
                    let mut literal = false;
                    if token.text == "{" && call.is_none() {
                        let named = word(before).is_some_and(|word| !keyword(word) || word == "Self") || punct(before, ">");
                        literal = match wait_here(&waits) {
                            Some(Wait::Pattern) | None => named,
                            Some(Wait::Condition | Wait::Header) => {
                                waits.pop();
                                false
                            }
                        };
                    }
                    // The path of the literal: the word before `{` with its qualifier.
                    let owner = match (literal, before, word(before)) {
                        (true, Some(at), Some(last)) => qualifier(at).map(|path| if path.is_empty() { last.to_string() } else { format!("{path}::{last}") }),
                        _ => None,
                    };
                    stack.push(Frame { literal, owner, opaque });
                }
                "}" | ")" | "]" => {
                    stack.pop();
                    waits.retain(|(at, _)| *at <= stack.len());
                    if token.text == "}"
                        && let Some((_, prefixes)) = use_line.as_mut()
                        && prefixes.len() > 1
                    {
                        prefixes.pop();
                    }
                }
                ";" => {
                    waits.retain(|(at, _)| *at < depth);
                    if use_line.as_ref().is_some_and(|(at, _)| *at == depth) {
                        use_line = None;
                    }
                }
                "=" if wait_here(&waits) == Some(Wait::Pattern) => waits.last_mut().unwrap().1 = Wait::Condition,
                // The guard of a match arm ends at `=>`.
                "=>" if wait_here(&waits) == Some(Wait::Condition) => {
                    waits.pop();
                }
                _ => {}
            },
            Kind::Word => {
                let opaque = stack.last().is_some_and(|frame| frame.opaque);
                match token.text {
                    "if" | "while" | "match" => waits.push((depth, Wait::Condition)),
                    // `impl Trait for Type` and `for <'a>` are not loops.
                    "for" if wait_here(&waits) != Some(Wait::Header) && !punct(Some(index + 1), "<") => waits.push((depth, Wait::Pattern)),
                    "let" if wait_here(&waits) == Some(Wait::Condition) => waits.last_mut().unwrap().1 = Wait::Pattern,
                    "in" if wait_here(&waits) == Some(Wait::Pattern) => waits.last_mut().unwrap().1 = Wait::Condition,
                    "fn" | "impl" | "trait" | "struct" | "enum" | "mod" | "union" if !opaque && word(Some(index + 1)).is_some() || token.text == "impl" => waits.push((depth, Wait::Header)),
                    "use" if !opaque && use_line.is_none() => use_line = Some((depth, vec![String::new()])),
                    _ => {}
                }
                if keyword(token.text) || !wanted(token.text) {
                    continue;
                }
                let next = Some(index + 1);
                let previous = word(before);
                let kind = if punct(before, ".") {
                    let call = punct(next, "(") || punct(next, "::") && punct(Some(index + 2), "<");
                    let on_self = word(index.checked_sub(2)) == Some("self") && !punct(index.checked_sub(3), ".") && !punct(index.checked_sub(3), "::");
                    PlaceKind::Member { call, on_self }
                } else if matches!(previous, Some("fn" | "struct" | "enum" | "trait" | "type" | "mod" | "union" | "static"))
                    || previous == Some("const") && punct(next, ":")
                    || previous == Some("mut") && word(index.checked_sub(2)) == Some("static")
                    || punct(before, "!") && word(index.checked_sub(2)) == Some("macro_rules")
                {
                    PlaceKind::Decl
                } else if punct(next, "!") && ["(", "[", "{"].iter().any(|open| punct(Some(index + 2), open)) {
                    PlaceKind::MacroName
                } else if let Some((_, prefixes)) = use_line.as_ref().filter(|_| !opaque) {
                    if previous == Some("as") {
                        PlaceKind::Alias
                    } else {
                        let outer = prefixes.last().cloned().unwrap_or_default();
                        let inner = qualifier(index).unwrap_or_default();
                        let path = if outer.is_empty() || inner.is_empty() { format!("{outer}{inner}") } else { format!("{outer}::{inner}") };
                        PlaceKind::Path { qualifier: Some(path), more: punct(next, "::") }
                    }
                } else {
                    let label = stack.last().filter(|frame| frame.literal && (punct(before, "{") || punct(before, ",")));
                    match label {
                        Some(frame) if punct(next, ":") => PlaceKind::Key { owner: frame.owner.clone(), short: false },
                        Some(frame) if punct(next, ",") || punct(next, "}") => PlaceKind::Key { owner: frame.owner.clone(), short: true },
                        _ => PlaceKind::Path { qualifier: qualifier(index), more: punct(next, "::") },
                    }
                };
                out.push(Place { start: token.start, end: token.start + token.text.len(), name: token.text.to_string(), kind, opaque });
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str, name: &str) -> Vec<String> {
        places(text, &|word| word == name).iter().map(Place::key).collect()
    }

    #[test]
    fn a_word_in_a_literal_or_a_lifetime_has_no_place() {
        assert_eq!(keys(r##"{ let a = "load" ; let b = 'l' ; let c = b"load" ; let d = r#"load "x" load"# ; load (1) }"##, "load"), ["p::load"]);
        assert_eq!(keys("fn f < 'load > (x : & 'load u8) { 'load : loop { break 'load ; } }", "load"), Vec::<String>::new());
        assert_eq!(keys("{ x // load\n ; /* load */ load }", "load"), ["p::load"]);
    }

    #[test]
    fn each_kind_of_place_has_its_key() {
        assert_eq!(keys("fn load (k : u32) -> u32", "load"), ["d:load"]);
        assert_eq!(keys("{ l . load () + l . load + self . load + self . load () }", "load"), ["c:load", "f:load", "fs:load", "cs:load"]);
        assert_eq!(keys("{ crate :: net :: load (1) + load (2) + Link :: load (l) + < T as S > :: load () + :: load () }", "load"), ["p:crate::net:load", "p::load", "p:Link:load", "q:load", "p:{root}:load"]);
        assert_eq!(keys("{ Link { load : 1 , fetch } ; let net :: Link { load , .. } = l ; }", "load"), ["k:Link:load", "k:net::Link:load"]);
        assert_eq!(keys("{ load ! (1) ; my ! (load . load (load)) ; format ! (\"load {}\" , load (3)) }", "load"), ["m:load", "o:load", "o:load", "o:load", "p::load"]);
        assert_eq!(keys("const MAX : u32 = MAX_2 + load ;", "MAX"), ["d:MAX"]);
        assert_eq!(keys("{ let p = x as * const load ; }", "load"), ["p::load"]);
    }

    #[test]
    fn a_use_line_gives_the_full_path_of_each_name() {
        assert_eq!(keys("use crate :: net :: { load , Link as load , inner :: { load } } ;", "load"), ["p:crate::net:load", "a:load", "p:crate::net::inner:load"]);
        assert_eq!(keys("use crate :: disk :: load ;", "disk"), ["p:crate:disk"]);
        assert_eq!(keys("{ use super :: load ; load (1) }", "load"), ["p:super:load", "p::load"]);
    }

    #[test]
    fn a_block_after_a_condition_is_not_a_struct_literal() {
        assert_eq!(keys("{ if x == load { load } else { load } }", "load"), ["p::load", "p::load", "p::load"]);
        assert_eq!(keys("{ match s { S { load } if load > 1 => 1 , T { load : 2 } => 2 } }", "load"), ["k:S:load", "p::load", "k:T:load"]);
        assert_eq!(keys("{ for S { load , .. } in list { load ; } }", "load"), ["k:S:load", "p::load"]);
    }
}
