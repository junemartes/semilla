//! Batch 5p, C2: the table of known traits against the facts of the compiler.
//!
//! The test compiles a small crate with the driver. The driver writes the associated items of
//! each trait that the crate can see, the blanket impls, and the impls that each `derive`
//! makes. The test compares these facts with `semilla_verify::KNOWN_TRAITS`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

const MANIFEST: &str = "[package]\nname = \"trait_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n";

/// One struct for each `derive` of the table. The name of the struct is the name of the trait.
const SOURCE: &str = "#![allow(dead_code)]
pub mod one {
    #[derive(Clone)] pub struct SClone(u8);
    #[derive(Debug)] pub struct SDebug(u8);
    #[derive(Default)] pub struct SDefault(u8);
    #[derive(PartialEq)] pub struct SPartialEq(u8);
    #[derive(Hash)] pub struct SHash(u8);
    #[derive(serde::Serialize)] pub struct SSerialize(u8);
    #[derive(serde::Deserialize)] pub struct SDeserialize(u8);
}
pub mod two {
    #[derive(Copy)] pub struct SCopy(u8);
    impl Clone for SCopy { fn clone(&self) -> SCopy { *self } }
    #[derive(Eq)] pub struct SEq(u8);
    impl PartialEq for SEq { fn eq(&self, other: &SEq) -> bool { self.0 == other.0 } }
    #[derive(PartialOrd)] pub struct SPartialOrd(u8);
    impl PartialEq for SPartialOrd { fn eq(&self, other: &SPartialOrd) -> bool { self.0 == other.0 } }
    #[derive(Ord)] pub struct SOrd(u8);
    impl PartialEq for SOrd { fn eq(&self, other: &SOrd) -> bool { self.0 == other.0 } }
    impl Eq for SOrd {}
    impl PartialOrd for SOrd { fn partial_cmp(&self, other: &SOrd) -> Option<std::cmp::Ordering> { Some(self.0.cmp(&other.0)) } }
}
";

fn name_of(item: &str) -> &str {
    item.split_once(' ').map(|(_, name)| name).unwrap_or(item)
}

#[test]
fn the_table_of_known_traits_agrees_with_the_compiler() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("probe");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), MANIFEST).unwrap();
    std::fs::write(root.join("src/lib.rs"), SOURCE).unwrap();
    let toolchain = semilla_normal::cargo::toolchain();
    std::fs::write(root.join("rust-toolchain.toml"), format!("[toolchain]\nchannel = \"{toolchain}\"\n")).unwrap();
    // The lock file of Semilla has a version of `serde` that is in the cargo cache. Cargo
    // removes the packages that the probe does not use.
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    std::fs::copy(lock, root.join("Cargo.lock")).unwrap();
    let mut update = Command::new("cargo");
    update.args(["update", "--workspace", "--offline", "--quiet"]).current_dir(&root).env("RUSTUP_TOOLCHAIN", &toolchain);
    for name in ["RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_TARGET_DIR", "CARGO_MANIFEST_DIR"] {
        update.env_remove(name);
    }
    let done = update.output().unwrap();
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    // The build can write only its target folder and the folder of the records.
    unsafe { std::env::set_var("SEMILLA_TRAIT_TABLE", std::fs::canonicalize(&out).unwrap()) };
    semilla_normal::run_cargo_check(&root, "n0", &out, &dir.path().join("target")).unwrap();
    let text = std::fs::read_to_string(out.join("trait_probe.json")).expect("the driver writes the trait table");
    let table: serde_json::Value = serde_json::from_str(&text).unwrap();
    let list = |value: &serde_json::Value| value.as_array().unwrap().iter().map(|item| item.as_str().unwrap().to_string()).collect::<Vec<_>>();
    // The path of a trait, to its items and to the bounds of each blanket impl. A trait that
    // is not `pub` gives no blanket impl to the test: code outside its crate cannot name the
    // trait, so it cannot call a method of the trait and cannot write a path to an item.
    let mut traits: BTreeMap<String, (Vec<String>, Vec<Vec<String>>)> = BTreeMap::new();
    for entry in table["traits"].as_array().unwrap() {
        let blanket = if entry["public"].as_bool().unwrap() { entry["blanket"].as_array().unwrap().iter().map(list).collect() } else { Vec::new() };
        traits.insert(entry["path"].as_str().unwrap().to_string(), (list(&entry["items"]), blanket));
    }
    let known: BTreeSet<&str> = semilla_verify::KNOWN_TRAITS.iter().map(|row| row.path).collect();
    let mut wrong = Vec::new();
    for row in &semilla_verify::KNOWN_TRAITS {
        let (items, _) = traits.get(row.path).unwrap_or_else(|| panic!("the compiler has no trait `{}`", row.path));
        // A use of an associated type or constant can have no name. The table has no such trait.
        assert!(items.iter().all(|item| item.starts_with("fn ")), "`{}` has an item that is not a function: {items:?}", row.path);
        // The names that the trait gives to a type: its items, and the items of each trait
        // that a blanket impl gives to a type with the trait. The rule applies again to each
        // trait that it finds.
        let mut reached: BTreeSet<&str> = BTreeSet::from([row.path]);
        loop {
            let more: Vec<&str> = traits
                .iter()
                .filter(|(path, (_, blanket))| !reached.contains(path.as_str()) && blanket.iter().any(|bounds| bounds.iter().any(|bound| reached.contains(bound.as_str()))))
                .map(|(path, _)| path.as_str())
                .collect();
            if more.is_empty() {
                break;
            }
            reached.extend(more);
        }
        let expected: BTreeSet<&str> = reached.iter().flat_map(|path| traits[*path].0.iter()).map(|item| name_of(item)).collect();
        let names: BTreeSet<&str> = row.names.iter().copied().collect();
        if names != expected {
            wrong.push(format!("the names of `{}` are {expected:?}; the traits: {reached:?}", row.path));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    // The impls that each `derive` of the table makes: one `impl` of the trait for the type,
    // with items of the table. A marker trait with no item is not a name.
    let derives: Vec<&semilla_verify::KnownTrait> = semilla_verify::KNOWN_TRAITS.iter().filter(|row| row.derive).collect();
    for row in &derives {
        let made: Vec<&serde_json::Value> = table["impls"].as_array().unwrap().iter().filter(|entry| entry["derive"].as_bool().unwrap() && entry["self_type"].as_str().unwrap().ends_with(&format!("::S{}", row.name))).collect();
        assert!(!made.is_empty(), "the probe has no `derive({})`", row.name);
        for entry in made {
            let of_trait = entry["of_trait"].as_str().unwrap_or("");
            let items = list(&entry["items"]);
            if items.is_empty() && of_trait != row.path {
                continue;
            }
            assert_eq!(of_trait, row.path, "`derive({})` makes an `impl` of a different trait: {entry}", row.name);
            assert!(items.iter().all(|item| item.starts_with("fn ") && row.names.contains(&name_of(item))), "`derive({})`: {items:?}", row.name);
        }
    }
    // Each `impl` that a `derive` makes for a type of the probe belongs to a row. An `impl`
    // for a different type is in a block with no name: no code outside the block can name it.
    for entry in table["impls"].as_array().unwrap().iter().filter(|entry| entry["derive"].as_bool().unwrap()) {
        let self_type = entry["self_type"].as_str().unwrap();
        let of_trait = entry["of_trait"].as_str().unwrap_or("");
        // `one::_::…`: a type in the block of a constant with no name.
        let named = !self_type.contains("::_::");
        assert!(!named || known.contains(of_trait) || list(&entry["items"]).is_empty(), "an `impl` of a `derive` with no row: {entry}");
    }
    assert_eq!(derives.len(), 11);
}
