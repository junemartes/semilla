//! [R93.1-1] The stored form of the result of a step check (design 93.1).
//!
//! The types of the result of a step check have one serde form. The output form (the JSON of a
//! report) leaves out some members that only the check and the views read. The stored form has
//! each member: a member that the output leaves out has `skip_serializing_if = "output_only"`,
//! and [`lossless`] turns the skip off while it writes the stored form. A read of either form
//! gives the default for a member that is not there.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::sync::Mutex;

use serde::{Deserialize, Deserializer};

thread_local! {
    static LOSSLESS: Cell<bool> = const { Cell::new(false) };
}

/// Runs `work` with the stored form on: each member that the output form leaves out is
/// written.
pub fn lossless<R>(work: impl FnOnce() -> R) -> R {
    let before = LOSSLESS.with(|on| on.replace(true));
    let result = work();
    LOSSLESS.with(|on| on.set(before));
    result
}

/// The `skip_serializing_if` of a member that only the stored form has: true (skip) in the
/// output form.
pub fn output_only<T>(_: &T) -> bool {
    !LOSSLESS.with(Cell::get)
}

/// The texts of the members of type `&'static str` that a read of the stored form made (each
/// text one time).
static TEXTS: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());

/// A text with the life of the process. A member of type `&'static str` holds the name of a
/// node kind or a similar word of a small fixed set, so each distinct text is kept one time.
pub fn static_text(text: &str) -> &'static str {
    let mut all = TEXTS.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(found) = all.get(text) {
        return found;
    }
    let kept: &'static str = Box::leak(text.to_string().into_boxed_str());
    all.insert(kept);
    kept
}

/// Reads a member of type `&'static str`.
pub fn read_static<'de, D: Deserializer<'de>>(reader: D) -> Result<&'static str, D::Error> {
    let text = String::deserialize(reader)?;
    Ok(static_text(&text))
}

/// Reads a member of type `[&'static str; 2]`.
pub fn read_static_pair<'de, D: Deserializer<'de>>(reader: D) -> Result<[&'static str; 2], D::Error> {
    let [one, two] = <[String; 2]>::deserialize(reader)?;
    Ok([static_text(&one), static_text(&two)])
}

/// [R93.1-1] A map with keys that are not texts (a tuple), as a list of pairs in the stored
/// form (JSON has text keys only).
pub mod pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(map: &BTreeMap<K, V>, writer: S) -> Result<S::Ok, S::Error> {
        writer.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>, D: Deserializer<'de>>(reader: D) -> Result<BTreeMap<K, V>, D::Error> {
        Ok(Vec::<(K, V)>::deserialize(reader)?.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Default)]
    struct Sample {
        shown: u8,
        #[serde(default, skip_serializing_if = "output_only")]
        kept: Vec<u8>,
        #[serde(deserialize_with = "read_static")]
        kind: &'static str,
    }

    /// [R93.1-1] The stored form has each member, and a read of it gives the same value.
    // rule+ R93.1-1
    #[test]
    fn the_stored_form_has_each_member() {
        let sample = Sample { shown: 1, kept: vec![2, 3], kind: "block" };
        let text: &'static str = Box::leak(lossless(|| serde_json::to_string(&sample).unwrap()).into_boxed_str());
        assert_eq!(text, r#"{"shown":1,"kept":[2,3],"kind":"block"}"#);
        assert_eq!(serde_json::from_str::<Sample>(text).unwrap(), sample);
    }

    /// [R93.1-1] The negative case: the output form leaves the member out, so it does not
    /// change, and a read of it does not give the member.
    // rule- R93.1-1
    #[test]
    fn the_output_form_leaves_out_the_stored_members() {
        let sample = Sample { shown: 1, kept: vec![2, 3], kind: "block" };
        let text: &'static str = Box::leak(serde_json::to_string(&sample).unwrap().into_boxed_str());
        assert_eq!(text, r#"{"shown":1,"kind":"block"}"#);
        assert_ne!(serde_json::from_str::<Sample>(text).unwrap(), sample);
    }
}
