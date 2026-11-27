//! The stored form of a loaded workspace (design 88, F5).
//!
//! [R88.5-1] The syntax data of a tree is a function of the files of the tree, of the cargo
//! targets, and of the loader. The app stores it in the database of the store with a key of
//! the tree, of the output of `cargo metadata`, and of the Semilla binary (the version of the
//! parser). This module gives the bytes of a workspace and the workspace of its bytes: each
//! member, also each member that the JSON form of the workspace does not have (the columns, the
//! display texts, the editions, the byte ranges of the nodes).

use std::collections::{BTreeMap, BTreeSet};

use crate::load::{CodeFile, CrateInfo, FileEntry, Item, ItemDisplay, ItemKind, MacroModule, Target, Workspace};
use crate::rs::Edition;
use crate::tree::Node;

/// The format key of a stored workspace. A change of this encoding needs a new key.
pub const FORMAT: &str = "semilla-workspace-1";

/// A value with a stored form.
trait Stored: Sized {
    fn put(&self, out: &mut Vec<u8>);
    fn take(reader: &mut Reader<'_>) -> Option<Self>;
}

/// A reader of stored bytes. Each read checks the bounds.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let out = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(out)
    }
}

impl Stored for u64 {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
    fn take(reader: &mut Reader<'_>) -> Option<u64> {
        Some(u64::from_le_bytes(reader.bytes(8)?.try_into().ok()?))
    }
}

impl Stored for usize {
    fn put(&self, out: &mut Vec<u8>) {
        (*self as u64).put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<usize> {
        usize::try_from(u64::take(reader)?).ok()
    }
}

impl Stored for bool {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(u8::from(*self));
    }
    fn take(reader: &mut Reader<'_>) -> Option<bool> {
        match reader.bytes(1)?[0] {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

impl Stored for String {
    fn put(&self, out: &mut Vec<u8>) {
        self.len().put(out);
        out.extend_from_slice(self.as_bytes());
    }
    fn take(reader: &mut Reader<'_>) -> Option<String> {
        let len = usize::take(reader)?;
        Some(std::str::from_utf8(reader.bytes(len)?).ok()?.to_string())
    }
}

impl<T: Stored> Stored for Option<T> {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            None => out.push(0),
            Some(value) => {
                out.push(1);
                value.put(out);
            }
        }
    }
    fn take(reader: &mut Reader<'_>) -> Option<Option<T>> {
        match reader.bytes(1)?[0] {
            0 => Some(None),
            1 => Some(Some(T::take(reader)?)),
            _ => None,
        }
    }
}

impl<T: Stored> Stored for Vec<T> {
    fn put(&self, out: &mut Vec<u8>) {
        self.len().put(out);
        for value in self {
            value.put(out);
        }
    }
    fn take(reader: &mut Reader<'_>) -> Option<Vec<T>> {
        let len = usize::take(reader)?;
        // A length can be wrong (cut bytes): the reads stop at the end of the bytes.
        let mut out = Vec::with_capacity(len.min(1 << 16));
        for _ in 0..len {
            out.push(T::take(reader)?);
        }
        Some(out)
    }
}

impl<K: Stored + Ord, V: Stored> Stored for BTreeMap<K, V> {
    fn put(&self, out: &mut Vec<u8>) {
        self.len().put(out);
        for (key, value) in self {
            key.put(out);
            value.put(out);
        }
    }
    fn take(reader: &mut Reader<'_>) -> Option<BTreeMap<K, V>> {
        let len = usize::take(reader)?;
        let mut out = BTreeMap::new();
        for _ in 0..len {
            let key = K::take(reader)?;
            out.insert(key, V::take(reader)?);
        }
        Some(out)
    }
}

impl<T: Stored + Ord> Stored for BTreeSet<T> {
    fn put(&self, out: &mut Vec<u8>) {
        self.len().put(out);
        for value in self {
            value.put(out);
        }
    }
    fn take(reader: &mut Reader<'_>) -> Option<BTreeSet<T>> {
        let len = usize::take(reader)?;
        let mut out = BTreeSet::new();
        for _ in 0..len {
            out.insert(T::take(reader)?);
        }
        Some(out)
    }
}

impl<A: Stored, B: Stored> Stored for (A, B) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
        self.1.put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<(A, B)> {
        Some((A::take(reader)?, B::take(reader)?))
    }
}

impl Stored for Edition {
    fn put(&self, out: &mut Vec<u8>) {
        self.to_string().put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<Edition> {
        String::take(reader)?.parse::<Edition>().ok()
    }
}

impl Stored for ItemKind {
    fn put(&self, out: &mut Vec<u8>) {
        let number: u8 = match self {
            ItemKind::Fn => 0,
            ItemKind::Struct => 1,
            ItemKind::Enum => 2,
            ItemKind::Trait => 3,
            ItemKind::Impl => 4,
            ItemKind::Const => 5,
            ItemKind::Static => 6,
            ItemKind::Type => 7,
            ItemKind::Use => 8,
            ItemKind::Mod => 9,
            ItemKind::Macro => 10,
            ItemKind::Field => 11,
            ItemKind::Variant => 12,
            ItemKind::Other => 13,
        };
        out.push(number);
    }
    fn take(reader: &mut Reader<'_>) -> Option<ItemKind> {
        Some(match reader.bytes(1)?[0] {
            0 => ItemKind::Fn,
            1 => ItemKind::Struct,
            2 => ItemKind::Enum,
            3 => ItemKind::Trait,
            4 => ItemKind::Impl,
            5 => ItemKind::Const,
            6 => ItemKind::Static,
            7 => ItemKind::Type,
            8 => ItemKind::Use,
            9 => ItemKind::Mod,
            10 => ItemKind::Macro,
            11 => ItemKind::Field,
            12 => ItemKind::Variant,
            13 => ItemKind::Other,
            _ => return None,
        })
    }
}

/// The kinds of the nodes of this process: a kind is a static text, so a read kind is kept one
/// time.
fn static_kind(kind: String) -> &'static str {
    static KINDS: std::sync::Mutex<BTreeSet<&'static str>> = std::sync::Mutex::new(BTreeSet::new());
    let mut kinds = KINDS.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(known) = kinds.get(kind.as_str()) {
        return known;
    }
    let made: &'static str = Box::leak(kind.into_boxed_str());
    kinds.insert(made);
    made
}

impl Stored for Node {
    fn put(&self, out: &mut Vec<u8>) {
        self.kind.to_string().put(out);
        self.role.put(out);
        self.text.put(out);
        self.attr.put(out);
        self.stmt.put(out);
        self.start.put(out);
        self.end.put(out);
        self.children.put(out);
        self.span.put(out);
        self.parts.put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<Node> {
        Some(Node {
            kind: static_kind(String::take(reader)?),
            role: Stored::take(reader)?,
            text: Stored::take(reader)?,
            attr: Stored::take(reader)?,
            stmt: Stored::take(reader)?,
            start: Stored::take(reader)?,
            end: Stored::take(reader)?,
            children: Stored::take(reader)?,
            span: Stored::take(reader)?,
            parts: Stored::take(reader)?,
        })
    }
}

impl Stored for ItemDisplay {
    fn put(&self, out: &mut Vec<u8>) {
        self.vis.put(out);
        self.signature.put(out);
        self.attrs.put(out);
        self.members.put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<ItemDisplay> {
        Some(ItemDisplay { vis: Stored::take(reader)?, signature: Stored::take(reader)?, attrs: Stored::take(reader)?, members: Stored::take(reader)? })
    }
}

impl Stored for Item {
    fn put(&self, out: &mut Vec<u8>) {
        self.path.put(out);
        self.kind.put(out);
        self.crate_name.put(out);
        self.file.put(out);
        self.line_start.put(out);
        self.line_end.put(out);
        self.col_start.put(out);
        self.col_end.put(out);
        self.vis.put(out);
        self.signature.put(out);
        self.docs.put(out);
        self.attrs.put(out);
        self.members.put(out);
        self.body.put(out);
        self.sig.put(out);
        self.text.put(out);
        self.source.put(out);
        self.test.put(out);
        self.display.put(out);
        self.macro_definition.put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<Item> {
        Some(Item {
            path: Stored::take(reader)?,
            kind: Stored::take(reader)?,
            crate_name: Stored::take(reader)?,
            file: Stored::take(reader)?,
            line_start: Stored::take(reader)?,
            line_end: Stored::take(reader)?,
            col_start: Stored::take(reader)?,
            col_end: Stored::take(reader)?,
            vis: Stored::take(reader)?,
            signature: Stored::take(reader)?,
            docs: Stored::take(reader)?,
            attrs: Stored::take(reader)?,
            members: Stored::take(reader)?,
            body: Stored::take(reader)?,
            sig: Stored::take(reader)?,
            text: Stored::take(reader)?,
            source: Stored::take(reader)?,
            test: Stored::take(reader)?,
            display: Stored::take(reader)?,
            macro_definition: Stored::take(reader)?,
        })
    }
}

impl Stored for CrateInfo {
    fn put(&self, out: &mut Vec<u8>) {
        self.name.put(out);
        self.dir.put(out);
        self.deps.put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<CrateInfo> {
        Some(CrateInfo { name: Stored::take(reader)?, dir: Stored::take(reader)?, deps: Stored::take(reader)? })
    }
}

impl Stored for FileEntry {
    fn put(&self, out: &mut Vec<u8>) {
        self.mode.put(out);
        self.hash.put(out);
        self.size.put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<FileEntry> {
        Some(FileEntry { mode: Stored::take(reader)?, hash: Stored::take(reader)?, size: Stored::take(reader)? })
    }
}

impl Stored for Target {
    fn put(&self, out: &mut Vec<u8>) {
        self.package.put(out);
        self.name.put(out);
        self.kind.put(out);
        self.file.put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<Target> {
        Some(Target { package: Stored::take(reader)?, name: Stored::take(reader)?, kind: Stored::take(reader)?, file: Stored::take(reader)? })
    }
}

impl Stored for CodeFile {
    fn put(&self, out: &mut Vec<u8>) {
        self.package.put(out);
        self.kind.put(out);
        self.target.put(out);
        self.outside.put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<CodeFile> {
        Some(CodeFile { package: Stored::take(reader)?, kind: Stored::take(reader)?, target: Stored::take(reader)?, outside: Stored::take(reader)? })
    }
}

impl Stored for MacroModule {
    fn put(&self, out: &mut Vec<u8>) {
        self.call.put(out);
        self.file.put(out);
        self.at.put(out);
        self.text.put(out);
    }
    fn take(reader: &mut Reader<'_>) -> Option<MacroModule> {
        Some(MacroModule { call: Stored::take(reader)?, file: Stored::take(reader)?, at: Stored::take(reader)?, text: Stored::take(reader)? })
    }
}

/// The bytes of a workspace. The index of the body items and the name record are not stored:
/// a read makes the index at its first use, and the verifier sets the name record.
pub fn encode(workspace: &Workspace) -> Vec<u8> {
    let mut out = Vec::new();
    workspace.crates.put(&mut out);
    workspace.items.put(&mut out);
    workspace.other_files.put(&mut out);
    workspace.rust_files.put(&mut out);
    workspace.files.put(&mut out);
    workspace.parsed_files.put(&mut out);
    workspace.targets.put(&mut out);
    workspace.code_files.put(&mut out);
    workspace.inner_attrs.put(&mut out);
    workspace.inner_attrs_display.put(&mut out);
    workspace.module_files.put(&mut out);
    workspace.macro_modules.put(&mut out);
    workspace.parse_errors.put(&mut out);
    workspace.editions.put(&mut out);
    out
}

/// The workspace of its bytes. `None` if the bytes are not a workspace of this format.
pub fn decode(bytes: &[u8]) -> Option<Workspace> {
    let mut reader = Reader { bytes, at: 0 };
    let reader = &mut reader;
    let workspace = Workspace {
        crates: Stored::take(reader)?,
        items: Stored::take(reader)?,
        other_files: Stored::take(reader)?,
        rust_files: Stored::take(reader)?,
        files: Stored::take(reader)?,
        parsed_files: Stored::take(reader)?,
        targets: Stored::take(reader)?,
        code_files: Stored::take(reader)?,
        inner_attrs: Stored::take(reader)?,
        inner_attrs_display: Stored::take(reader)?,
        module_files: Stored::take(reader)?,
        macro_modules: Stored::take(reader)?,
        parse_errors: Stored::take(reader)?,
        editions: Stored::take(reader)?,
        ..Workspace::default()
    };
    (reader.at == bytes.len()).then_some(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &std::path::Path, rel: &str, text: &str) {
        let full = root.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    fn sample() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        write(root, "src/lib.rs", "//! Shop.\n#![allow(unused)]\npub mod cart;\nmacro_rules! m { () => {} }\n#[cfg(test)]\nmod tests { #[test] fn t() { assert!(true); } }\n");
        write(root, "src/cart.rs", "/// A cart.\npub struct Cart { pub items: Vec<u32> }\nimpl Cart {\n    pub fn total(&self) -> u32 {\n        // add\n        self.items.iter().sum()\n    }\n}\npub enum E { A(u8), B { x: u8 } }\npub fn bad( {}\n");
        write(root, "README.md", "text\n");
        let workspace = Workspace::load(root).unwrap();
        (dir, workspace)
    }

    // [R88.5-1] The stored form of a workspace gives the workspace: each member, also each
    // member that the JSON form does not have.
    // rule+ R88.5-1
    #[test]
    fn the_stored_form_of_a_workspace_is_the_workspace() {
        let (_dir, workspace) = sample();
        assert!(!workspace.items.is_empty() && !workspace.editions.is_empty());
        let again = decode(&encode(&workspace)).unwrap();
        assert_eq!(format!("{workspace:?}"), format!("{again:?}"));
        assert_eq!(serde_json::to_string(&workspace).unwrap(), serde_json::to_string(&again).unwrap());
    }

    // rule- R88.5-1
    #[test]
    fn bytes_that_are_not_a_whole_workspace_give_no_workspace() {
        let (_dir, workspace) = sample();
        let bytes = encode(&workspace);
        assert!(decode(&bytes[..bytes.len() - 1]).is_none());
        assert!(decode(&[bytes.as_slice(), &[0]].concat()).is_none());
        assert!(decode(b"").is_none());
    }
}
