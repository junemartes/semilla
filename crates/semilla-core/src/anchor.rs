//! Anchors: locations in the entity tree.
//!
//! An anchor is an item path, then an optional path inside the item:
//!
//! ```text
//! cart::Cart::total                      the item
//! cart::Cart::total/S2                   statement 2 of the body
//! cart::Cart::total/S2/body/S0           statement 0 in the body of statement 2 (a loop)
//! cart::Cart::total/S2.0                 a short form of the line above
//! cart::Cart::total/S2/body/S0/cond/rhs  the right side of the condition
//! cart::Cart::total/tail                 the tail expression of the body
//! ```
//!
//! Item paths start with the crate name. Statement numbers refer to the "before" snapshot of the
//! step, except in claims that add a statement: there they give the insert position.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// One segment of the path inside an item.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Segment {
    /// A statement in the current block, by index.
    Stmt(usize),
    /// The tail expression of the current block.
    Tail,
    /// A child by role, for example `cond`, `then`, `args[1]`, or `arm[0].body`.
    Role(String),
}

impl fmt::Display for Segment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Segment::Stmt(index) => write!(f, "S{index}"),
            Segment::Tail => write!(f, "tail"),
            Segment::Role(role) => write!(f, "{role}"),
        }
    }
}

/// A location in the entity tree.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Anchor {
    /// The full item path, for example `cart::Cart::total`.
    pub item: String,
    /// The path inside the item. Empty for the item itself.
    pub path: Vec<Segment>,
}

impl Anchor {
    pub fn item(item: impl Into<String>) -> Anchor {
        Anchor { item: item.into(), path: Vec::new() }
    }

    /// Returns a new anchor with one more segment.
    pub fn child(&self, segment: Segment) -> Anchor {
        let mut path = self.path.clone();
        path.push(segment);
        Anchor { item: self.item.clone(), path }
    }

    /// Returns true if `self` is `other` or an ancestor of `other`.
    pub fn contains(&self, other: &Anchor) -> bool {
        self.item == other.item && other.path.starts_with(&self.path)
    }

    /// Returns the parent anchor, or `None` for an item anchor.
    pub fn parent(&self) -> Option<Anchor> {
        let mut path = self.path.clone();
        path.pop()?;
        Some(Anchor { item: self.item.clone(), path })
    }

    /// The number of segments between `self` and a descendant `other`.
    pub fn depth_to(&self, other: &Anchor) -> Option<usize> {
        self.contains(other).then(|| other.path.len() - self.path.len())
    }
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.item)?;
        for segment in &self.path {
            write!(f, "/{segment}")?;
        }
        Ok(())
    }
}

/// An error in the text of an anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorError(pub String);

impl fmt::Display for AnchorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad anchor: {}", self.0)
    }
}

impl std::error::Error for AnchorError {}

impl FromStr for Anchor {
    type Err = AnchorError;

    fn from_str(text: &str) -> Result<Anchor, AnchorError> {
        let mut parts = text.split('/');
        let item = parts.next().filter(|item| !item.is_empty()).ok_or_else(|| AnchorError(text.to_string()))?;
        let mut path = Vec::new();
        for part in parts {
            if part.is_empty() {
                return Err(AnchorError(text.to_string()));
            }
            if part == "tail" {
                path.push(Segment::Tail);
            } else if let Some(numbers) = part.strip_prefix('S').filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit())) {
                // `S2.0.1` is a short form of `S2/body/S0/body/S1`.
                let mut first = true;
                for number in numbers.split('.') {
                    let index = number.parse().map_err(|_| AnchorError(text.to_string()))?;
                    if !first {
                        path.push(Segment::Role("body".to_string()));
                    }
                    path.push(Segment::Stmt(index));
                    first = false;
                }
            } else {
                path.push(Segment::Role(part.to_string()));
            }
        }
        Ok(Anchor { item: item.to_string(), path })
    }
}

impl Serialize for Anchor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Anchor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Anchor, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_print() {
        let anchor: Anchor = "cart::Cart::total/S2/body/S0/cond/rhs".parse().unwrap();
        assert_eq!(anchor.item, "cart::Cart::total");
        assert_eq!(anchor.path.len(), 5);
        assert_eq!(anchor.to_string(), "cart::Cart::total/S2/body/S0/cond/rhs");
    }

    #[test]
    fn short_form_expands() {
        let short: Anchor = "a::f/S2.0".parse().unwrap();
        let long: Anchor = "a::f/S2/body/S0".parse().unwrap();
        assert_eq!(short, long);
    }

    #[test]
    fn contains_and_depth() {
        let outer: Anchor = "a::f/S1".parse().unwrap();
        let inner: Anchor = "a::f/S1/cond/lhs".parse().unwrap();
        assert!(outer.contains(&inner));
        assert!(!inner.contains(&outer));
        assert_eq!(outer.depth_to(&inner), Some(2));
        assert_eq!(inner.parent().unwrap().parent().unwrap(), outer);
    }

    #[test]
    fn rejects_bad_text() {
        assert!("".parse::<Anchor>().is_err());
        assert!("a::f//S1".parse::<Anchor>().is_err());
        assert!("a::f/S1.x".parse::<Anchor>().is_err());
    }

    #[test]
    fn serde_round_trip() {
        let anchor: Anchor = "a::f/tail".parse().unwrap();
        let json = serde_json::to_string(&anchor).unwrap();
        assert_eq!(json, "\"a::f/tail\"");
        assert_eq!(serde_json::from_str::<Anchor>(&json).unwrap(), anchor);
    }
}
