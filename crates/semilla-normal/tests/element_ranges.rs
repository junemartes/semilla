//! The driver records a byte range for each MIR element (design 40.1). The offsets are offsets
//! in the file as written: a file with CRLF line ends and a file with a BOM give the ranges of
//! the bytes on the disk. A span from a macro has the range of its tokens and the range of the
//! outermost macro call.

use std::path::{Path, PathBuf};
use std::process::Command;

use semilla_normal::{read_records, run_cargo_check};

fn write(path: &Path, text: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Makes sure that a driver exists, and returns its path.
fn ensure_driver(scratch: &Path) -> PathBuf {
    if let Ok(path) = semilla_normal::cargo::find_driver() {
        return path;
    }
    let target = scratch.join("driver-target");
    let status = Command::new("cargo").args(["build", "-p", "semilla-mir", "--target-dir"]).arg(&target).current_dir(semilla_normal::cargo::semilla_workspace()).status().expect("cargo runs");
    assert!(status.success(), "cannot build the driver");
    target.join("debug/semilla-mir-driver")
}

/// The texts of the ranges of each element of a body: the tokens, and the macro call.
fn element_texts(root: &Path, record: &semilla_normal::BodyRecord) -> Vec<(Option<String>, Option<String>)> {
    let ranges = record.elements.as_ref().expect("an N0 record has its element ranges").ranges().expect("valid ranges");
    let text = |place: &Option<semilla_normal::elements::Place>| {
        place.as_ref().map(|place| {
            let bytes = std::fs::read(root.join(&place.file)).unwrap();
            String::from_utf8_lossy(&bytes[place.start..place.end]).into_owned()
        })
    };
    ranges.iter().map(|(tokens, call)| (text(tokens), text(call))).collect()
}

#[test]
fn the_element_ranges_are_offsets_in_the_file_as_written() {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-normal-element-ranges");
    let driver = ensure_driver(&scratch);
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
    let root = scratch.join("ws");
    let _ = std::fs::remove_dir_all(&root);
    write(&root.join("Cargo.toml"), b"[package]\nname = \"ranges\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[workspace]\n");
    // The root file has a BOM and CRLF line ends. The module has CRLF line ends only.
    write(&root.join("src/lib.rs"), b"\xef\xbb\xbfmod crlf;\r\nmacro_rules! twice {\r\n    ($e:expr) => { $e * 2 };\r\n}\r\n\r\npub fn bom(a: u32) -> u32 {\r\n    let b = twice!(a + 1);\r\n    b - 3\r\n}\r\n");
    write(&root.join("src/crlf.rs"), b"// A comment.\r\npub fn crlf(x: u64) -> u64 {\r\n    let y = x * 7;\r\n    y + 11\r\n}\r\n");
    let out = scratch.join("records");
    let _ = std::fs::remove_dir_all(&out);
    run_cargo_check(&root, "n0", &out, &scratch.join("target")).unwrap_or_else(|error| panic!("{error:#}"));
    let records = read_records(&out).unwrap();
    let find = |name: &str| records.iter().find(|record| record.name == name && record.level == "n0").unwrap_or_else(|| panic!("no record {name}"));

    let crlf = element_texts(&root, find("ranges::crlf::crlf"));
    let tokens: Vec<&str> = crlf.iter().filter_map(|(tokens, _)| tokens.as_deref()).collect();
    assert!(tokens.contains(&"x * 7") && tokens.contains(&"y + 11"), "{crlf:?}");
    assert!(crlf.iter().all(|(_, call)| call.is_none()), "{crlf:?}");

    let bom = element_texts(&root, find("ranges::bom"));
    let tokens: Vec<&str> = bom.iter().filter_map(|(tokens, _)| tokens.as_deref()).collect();
    assert!(tokens.contains(&"a + 1") && tokens.contains(&"b - 3"), "{bom:?}");
    // The code of the macro: the tokens of the `macro_rules` body, and the call.
    assert!(bom.iter().any(|(tokens, call)| tokens.as_deref() == Some("$e * 2") && call.as_deref() == Some("twice!(a + 1)")), "{bom:?}");
    // The body range of the record is in the file as written too.
    let span = find("ranges::crlf::crlf").span.clone().unwrap();
    let (start, end) = span.range.expect("a body range");
    let bytes = std::fs::read(root.join("src/crlf.rs")).unwrap();
    assert!(String::from_utf8_lossy(&bytes[start..end]).starts_with("pub fn crlf(x: u64) -> u64 {\r\n"), "{span:?}");
    let _ = std::fs::remove_dir_all(&scratch);
}
