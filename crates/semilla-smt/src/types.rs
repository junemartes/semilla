//! Rust types that the SMT encoding knows, and the type environment.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A Rust type, as the SMT encoding sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RustType {
    /// A primitive integer type. `usize` and `isize` have 64 bits.
    Int {
        bits: u32,
        signed: bool,
    },
    Bool,
    Char,
    /// `f32` or `f64`. The MVP does not model floats.
    Float,
    /// A type that the encoding does not know.
    Unknown,
}

impl RustType {
    /// Parses the source text of a type, for example `u32`, `&mut i64`, or `bool`.
    /// A reference has the same value as its target for the encoding.
    pub fn parse(text: &str) -> RustType {
        let mut rest = text.trim();
        loop {
            if let Some(inner) = rest.strip_prefix('&') {
                rest = inner.trim_start();
                if let Some(inner) = rest.strip_prefix("mut ") {
                    rest = inner.trim_start();
                }
                // Skip a lifetime, for example `&'a u32`.
                if let Some(inner) = rest.strip_prefix('\'') {
                    let end = inner.find(char::is_whitespace).unwrap_or(inner.len());
                    rest = inner[end..].trim_start();
                }
                continue;
            }
            if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
                rest = inner.trim();
                continue;
            }
            break;
        }
        // Accept full paths to the primitive types, for example `core::primitive::u32`.
        let name = rest.rsplit("::").next().unwrap_or(rest).trim();
        match name {
            "u8" => RustType::Int {
                bits: 8,
                signed: false,
            },
            "u16" => RustType::Int {
                bits: 16,
                signed: false,
            },
            "u32" => RustType::Int {
                bits: 32,
                signed: false,
            },
            "u64" | "usize" => RustType::Int {
                bits: 64,
                signed: false,
            },
            "u128" => RustType::Int {
                bits: 128,
                signed: false,
            },
            "i8" => RustType::Int {
                bits: 8,
                signed: true,
            },
            "i16" => RustType::Int {
                bits: 16,
                signed: true,
            },
            "i32" => RustType::Int {
                bits: 32,
                signed: true,
            },
            "i64" | "isize" => RustType::Int {
                bits: 64,
                signed: true,
            },
            "i128" => RustType::Int {
                bits: 128,
                signed: true,
            },
            "bool" => RustType::Bool,
            "char" => RustType::Char,
            "f32" | "f64" | "f16" | "f128" => RustType::Float,
            _ => RustType::Unknown,
        }
    }
}

/// Maps a variable name or the source text of an atom (for example `v.len()`) to its type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeEnv {
    types: HashMap<String, RustType>,
}

impl TypeEnv {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the type of a variable or an atom. The key is normalized, so
    /// `v.len()` and `v . len ()` are the same key.
    pub fn insert(&mut self, name: &str, ty: RustType) {
        self.types.insert(normalize_key(name), ty);
    }

    /// Parses the source text of a type. See [`RustType::parse`].
    pub fn parse_type(text: &str) -> RustType {
        RustType::parse(text)
    }

    /// Returns the type of a variable or an atom, or `Unknown`.
    pub fn get(&self, name: &str) -> RustType {
        self.types
            .get(name)
            .or_else(|| self.types.get(&normalize_key(name)))
            .copied()
            .unwrap_or(RustType::Unknown)
    }
}

/// Gives the canonical text of an expression: the token stream with one space between tokens.
pub(crate) fn normalize_key(text: &str) -> String {
    match syn::parse_str::<syn::Expr>(text) {
        Ok(expr) => crate::lower::expr_text(&expr),
        Err(_) => text.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_types() {
        assert_eq!(
            RustType::parse("u32"),
            RustType::Int {
                bits: 32,
                signed: false
            }
        );
        assert_eq!(
            RustType::parse("usize"),
            RustType::Int {
                bits: 64,
                signed: false
            }
        );
        assert_eq!(
            RustType::parse("isize"),
            RustType::Int {
                bits: 64,
                signed: true
            }
        );
        assert_eq!(
            RustType::parse("&u32"),
            RustType::Int {
                bits: 32,
                signed: false
            }
        );
        assert_eq!(
            RustType::parse("&mut i8"),
            RustType::Int {
                bits: 8,
                signed: true
            }
        );
        assert_eq!(
            RustType::parse("&'a &u16"),
            RustType::Int {
                bits: 16,
                signed: false
            }
        );
        assert_eq!(RustType::parse("bool"), RustType::Bool);
        assert_eq!(RustType::parse("char"), RustType::Char);
        assert_eq!(RustType::parse("f64"), RustType::Float);
        assert_eq!(RustType::parse("Vec<u32>"), RustType::Unknown);
        assert_eq!(
            RustType::parse("std::primitive::u64"),
            RustType::Int {
                bits: 64,
                signed: false
            }
        );
    }

    #[test]
    fn env_keys_are_normalized() {
        let mut env = TypeEnv::new();
        env.insert("v . len ()", RustType::parse("usize"));
        assert_eq!(
            env.get("v.len()"),
            RustType::Int {
                bits: 64,
                signed: false
            }
        );
        assert_eq!(env.get("w.len()"), RustType::Unknown);
    }
}
