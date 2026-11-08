//! The review state (design section 11.8).
//!
//! The state follows the claims, not the lines. For each reviewed claim, the review file stores
//! a hash of the claim content: the op, the anchor, the arguments, and the source text before and
//! after. When the agent changes its work, the hash of a changed claim is different, and that
//! claim becomes "not reviewed" again. The other claims keep their state.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use semilla_core::Claim;

use crate::ctx::{Ctx, item_of};

/// The content of the review file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReviewFile {
    #[serde(default = "version")]
    pub version: u32,
    /// The reviewed claims, by claim ID.
    #[serde(default)]
    pub claims: BTreeMap<String, ReviewEntry>,
}

fn version() -> u32 {
    1
}

/// One reviewed claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEntry {
    /// The claim hash at the time of the review.
    pub hash: String,
    /// The time of the review, in seconds since the Unix epoch.
    #[serde(default)]
    pub marked_at: u64,
}

/// The review state of one claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Reviewed,
    NotReviewed,
    /// The reviewer marked the claim, but the claim changed after that. It counts as not reviewed.
    Changed,
}

impl ReviewState {
    pub fn is_reviewed(self) -> bool {
        self == ReviewState::Reviewed
    }
}

impl ReviewFile {
    /// Reads the review file. A missing file gives an empty state. A bad file gives an empty
    /// state and a warning: the next write replaces it.
    pub fn load(path: &Path) -> ReviewFile {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(file) => file,
                Err(error) => {
                    eprintln!("semilla ui: warning: cannot read {}: {error}. The review state starts empty.", path.display());
                    ReviewFile { version: 1, ..Default::default() }
                }
            },
            Err(_) => ReviewFile { version: 1, ..Default::default() },
        }
    }

    /// Writes the review file. It writes a temporary file first, then renames it.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, text + "\n")?;
        std::fs::rename(&temp, path)
    }

    /// The state of one claim, from its current hash.
    pub fn state(&self, claim: &str, current_hash: Option<&str>) -> ReviewState {
        match (self.claims.get(claim), current_hash) {
            (None, _) => ReviewState::NotReviewed,
            (Some(entry), Some(hash)) if entry.hash == hash => ReviewState::Reviewed,
            (Some(_), _) => ReviewState::Changed,
        }
    }

    pub fn mark(&mut self, claim: &str, hash: &str, now: u64) {
        self.claims.insert(claim.to_string(), ReviewEntry { hash: hash.to_string(), marked_at: now });
    }

    pub fn unmark(&mut self, claim: &str) {
        self.claims.remove(claim);
    }
}

/// The review hash of a claim: SHA-256 of a canonical JSON text of the op, the anchor, the
/// arguments, and the source text before and after.
pub fn claim_hash(ctx: &Ctx, claim: &Claim) -> String {
    let anchor = claim.at.as_ref().map(|at| at.to_string());
    let result = ctx.results.get(&claim.id);
    let mut sources = Vec::new();
    if let Some(anchor) = &anchor {
        if let Some(pair) = ctx.data.sources.get(anchor) {
            sources.push(serde_json::json!({ "key": anchor, "before": pair.before, "after": pair.after }));
        }
        let item = item_of(anchor);
        if item != anchor
            && let Some(pair) = ctx.data.sources.get(item)
        {
            sources.push(serde_json::json!({ "key": item, "before": pair.before, "after": pair.after }));
        }
    }
    let content = serde_json::json!({
        "op": claim.op,
        "at": anchor,
        "args": Value::Object(claim.args.clone()),
        "before": result.and_then(|result| result.before.clone()),
        "after": result.and_then(|result| result.after.clone()),
        "sources": sources,
    });
    let mut text = String::new();
    canonical(&content, &mut text);
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Writes JSON with sorted object keys, so the hash does not depend on the key order.
fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                canonical(&map[key.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(values) => {
            out.push('[');
            for (index, item) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The current time in seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|time| time.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_sorts_keys() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"d":[1,2],"c":"x"}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"c":"x","d":[1,2]},"b":1}"#).unwrap();
        let (mut ta, mut tb) = (String::new(), String::new());
        canonical(&a, &mut ta);
        canonical(&b, &mut tb);
        assert_eq!(ta, tb);
        assert_eq!(ta, r#"{"a":{"c":"x","d":[1,2]},"b":1}"#);
    }

    #[test]
    fn state_from_hash() {
        let mut file = ReviewFile::default();
        file.mark("c1", "h1", 0);
        assert_eq!(file.state("c1", Some("h1")), ReviewState::Reviewed);
        assert_eq!(file.state("c1", Some("h2")), ReviewState::Changed);
        assert_eq!(file.state("c2", Some("h1")), ReviewState::NotReviewed);
    }
}
