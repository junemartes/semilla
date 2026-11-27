//! The store of the records of the cache folders (design 86, S4).
//!
//! Each read and each write of a record folder goes through [`RecordStore`]: the records of
//! the bodies, the unit records, the expansions, and the marks of a folder (`.complete` and the
//! other mark files). The callers do not know the files. A database can replace [`FileStore`]
//! with no change of the callers.
//!
//! [R86.1-1] The store reads each part of a folder one time in a process: a second read gives
//! the value in memory, with no copy (R86.3-2). The key of a value is the folder and the time
//! of its `.complete` file: a build that makes the folder again gives a new key.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The key of a folder in memory: the folder and the time of its `.complete` file.
pub(crate) type FolderKey = (PathBuf, std::time::SystemTime);

/// The name of the mark file of a complete folder.
const COMPLETE: &str = ".complete";

/// The read and the write of the record folders.
pub(crate) trait RecordStore: Sync {
    /// True if the folder is complete: a build wrote each of its records.
    fn is_complete(&self, folder: &Path) -> bool;
    /// The key of a complete folder, or `None`.
    fn key(&self, folder: &Path) -> Option<FolderKey>;
    /// The body records of a complete folder, in the order of their units.
    fn records(&self, folder: &Path) -> Result<semilla_normal::Records>;
    /// The unit records of a folder (`None`: a file has no unit record).
    fn units(&self, folder: &Path) -> Option<Arc<Vec<semilla_normal::UnitRecord>>>;
    /// The expansions of each unit of a folder (`None`: a file has no expansion list). The
    /// name of the configuration of each unit is empty: the caller sets it.
    fn expansions(&self, folder: &Path) -> Option<Arc<Vec<semilla_verify::expand::ExpansionUnit>>>;
    /// The text of a mark file of a folder (`.failed`, `.no-sandbox`), or `None`.
    fn mark(&self, folder: &Path, name: &str) -> Option<String>;
    /// Writes a mark file of a folder.
    fn set_mark(&self, folder: &Path, name: &str, text: &str) -> Result<()>;
    /// Removes a folder and each of its values in memory.
    fn remove(&self, folder: &Path);
    /// Makes an empty folder (after `remove`).
    fn create(&self, folder: &Path) -> Result<()>;
    /// Copies one record file of a build into a folder.
    fn put_file(&self, folder: &Path, name: &str, from: &Path) -> Result<()>;
    /// Marks a folder complete.
    fn complete(&self, folder: &Path) -> Result<()>;
}

/// The most folders in memory: the folders of two steps with four configurations.
const MOST_FOLDERS: usize = 48;

/// The values of the folders in memory, by kind.
#[derive(Default)]
struct Memory {
    records: BTreeMap<FolderKey, semilla_normal::Records>,
    units: BTreeMap<FolderKey, Option<Arc<Vec<semilla_normal::UnitRecord>>>>,
    expansions: BTreeMap<FolderKey, Option<Arc<Vec<semilla_verify::expand::ExpansionUnit>>>>,
}

/// The store in the files of the cache folder, with the values of this process in memory.
#[derive(Default)]
pub(crate) struct FileStore {
    memory: Mutex<Memory>,
}

/// The store of this process.
pub(crate) fn store() -> &'static FileStore {
    static STORE: std::sync::OnceLock<FileStore> = std::sync::OnceLock::new();
    STORE.get_or_init(FileStore::default)
}

/// The phase name of the reads of the store in the timing lines.
pub(crate) const READ_PHASE: &str = "read the records of a cache folder";

impl FileStore {
    fn memory(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.memory.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// The value of a folder in memory, or a new read that goes into memory. With no key (the
    /// folder is not complete), each call reads again.
    fn cached<T: Clone>(&self, folder: &Path, map: fn(&mut Memory) -> &mut BTreeMap<FolderKey, T>, read: impl FnOnce() -> T) -> T {
        let key = self.key(folder);
        if let Some(key) = &key {
            if let Some(known) = map(&mut self.memory()).get(key) {
                return known.clone();
            }
        }
        let value = read();
        if let Some(key) = key {
            let mut memory = self.memory();
            let entries = map(&mut memory);
            if entries.len() >= MOST_FOLDERS {
                entries.clear();
            }
            entries.insert(key, value.clone());
        }
        value
    }

    /// The count of the folders with records in memory (for a test).
    #[cfg(test)]
    pub(crate) fn in_memory(&self, folder: &Path) -> usize {
        self.memory().records.keys().filter(|(path, _)| path == folder).count()
    }
}

impl RecordStore for FileStore {
    fn is_complete(&self, folder: &Path) -> bool {
        folder.join(COMPLETE).exists()
    }

    fn key(&self, folder: &Path) -> Option<FolderKey> {
        std::fs::metadata(folder.join(COMPLETE)).and_then(|file| file.modified()).ok().map(|time| (folder.to_path_buf(), time))
    }

    fn records(&self, folder: &Path) -> Result<semilla_normal::Records> {
        // An error is not kept: the next read tries again.
        let key = self.key(folder);
        if let Some(known) = key.as_ref().and_then(|key| self.memory().records.get(key).cloned()) {
            return Ok(known);
        }
        let started = std::time::Instant::now();
        let records = semilla_normal::read_records(folder).map(semilla_normal::Records::from);
        semilla_core::timing::add(READ_PHASE, started);
        if let Ok(records) = &records {
            semilla_core::timing::count(READ_PHASE, "records parsed", records.len() as u64);
        }
        if let (Some(key), Ok(records)) = (key, &records) {
            let mut memory = self.memory();
            if memory.records.len() >= MOST_FOLDERS {
                memory.records.clear();
            }
            memory.records.insert(key, records.clone());
        }
        records
    }

    fn units(&self, folder: &Path) -> Option<Arc<Vec<semilla_normal::UnitRecord>>> {
        self.cached(folder, |memory| &mut memory.units, || {
            semilla_core::timing::phase("read the unit records of a cache folder", || semilla_normal::read_units(folder).map(Arc::new))
        })
    }

    fn expansions(&self, folder: &Path) -> Option<Arc<Vec<semilla_verify::expand::ExpansionUnit>>> {
        self.cached(folder, |memory| &mut memory.expansions, || semilla_core::timing::phase(EXPANSIONS_PHASE, || read_expansions(folder).map(Arc::new)))
    }

    fn mark(&self, folder: &Path, name: &str) -> Option<String> {
        std::fs::read_to_string(folder.join(name)).ok()
    }

    fn set_mark(&self, folder: &Path, name: &str, text: &str) -> Result<()> {
        std::fs::write(folder.join(name), text)?;
        Ok(())
    }

    fn remove(&self, folder: &Path) {
        let mut memory = self.memory();
        memory.records.retain(|(path, _), _| path != folder);
        memory.units.retain(|(path, _), _| path != folder);
        memory.expansions.retain(|(path, _), _| path != folder);
        drop(memory);
        let _ = std::fs::remove_dir_all(folder);
    }

    fn create(&self, folder: &Path) -> Result<()> {
        std::fs::create_dir_all(folder)?;
        Ok(())
    }

    fn put_file(&self, folder: &Path, name: &str, from: &Path) -> Result<()> {
        std::fs::copy(from, folder.join(name))?;
        Ok(())
    }

    fn complete(&self, folder: &Path) -> Result<()> {
        std::fs::write(folder.join(COMPLETE), "")?;
        Ok(())
    }
}

/// The phase name of the read of the expansions in the timing lines.
pub(crate) const EXPANSIONS_PHASE: &str = "read the expansions of a cache folder";

/// [R86.3-4] The expansions of a folder, parsed into the typed data with no generic JSON value.
/// A line that the typed parse does not accept (a record of an earlier driver) takes the path
/// of `semilla_normal::read_expansions`: a value that the typed data does not accept is then
/// left out, as before. So the two paths give the same result.
pub(crate) fn read_expansions(folder: &Path) -> Option<Vec<semilla_verify::expand::ExpansionUnit>> {
    use std::io::BufRead as _;
    #[derive(serde::Deserialize)]
    struct Unit {
        #[serde(default)]
        target: String,
        #[serde(default)]
        package: String,
        #[serde(default)]
        root: String,
        #[serde(default)]
        cfg: Vec<String>,
        expansions: Option<Vec<semilla_verify::expand::Expansion>>,
        #[serde(default)]
        names: Option<semilla_core::namerec::Record>,
    }
    #[derive(serde::Deserialize)]
    struct Line {
        unit: Unit,
    }
    let entries = std::fs::read_dir(folder).ok()?;
    let files = semilla_normal::unit_order(entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).filter(|path| path.extension().is_some_and(|extension| extension == "jsonl")).collect());
    let mut out = Vec::new();
    for file in files {
        let reader = std::io::BufReader::new(std::fs::File::open(&file).ok()?);
        let line = reader.lines().next()?.ok()?;
        if !line.starts_with(semilla_normal::UNIT_LINE) {
            return None;
        }
        let Ok(line) = serde_json::from_str::<Line>(&line) else {
            semilla_core::timing::count(EXPANSIONS_PHASE, "folders with the generic parse", 1);
            return generic_expansions(folder);
        };
        let unit = line.unit;
        out.push(semilla_verify::expand::ExpansionUnit { config: String::new(), target: unit.target, package: unit.package, root: unit.root, cfg: unit.cfg, list: unit.expansions?, names: unit.names.map(Arc::new) });
    }
    Some(out)
}

/// The expansions of a folder by the generic parse (the path before batch 14d).
fn generic_expansions(folder: &Path) -> Option<Vec<semilla_verify::expand::ExpansionUnit>> {
    let units = semilla_normal::read_expansions(folder)?;
    Some(
        units
            .into_iter()
            .map(|unit| semilla_verify::expand::ExpansionUnit { config: String::new(), target: unit.target, package: unit.package, root: unit.root, cfg: unit.cfg, list: unit.list.into_iter().filter_map(|value| serde_json::from_value(value).ok()).collect(), names: unit.names.and_then(|value| serde_json::from_value(value).ok()).map(Arc::new) })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(unit_line: &str, seconds: u64) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("k.lib.1.jsonl"), format!("{unit_line}\n{{\"name\":\"k::f\",\"level\":\"n0\",\"text\":\"t\"}}\n")).unwrap();
        let complete = std::fs::File::create(dir.path().join(COMPLETE)).unwrap();
        complete.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds)).unwrap();
        dir
    }

    const UNIT: &str = r#"{"unit":{"target":"k.lib.1","package":"k","cfg":["unix"],"files":["src/lib.rs"],"expansions":[{"name":"m","kind":"bang","def":"crate m","form":"expr","text":"1"},{"name":"bad"}]}}"#;

    // [R86.1-1] Each part of a folder is read one time; the second read is the value in memory.
    // rule+ R86.1-1
    #[test]
    fn each_part_of_a_folder_is_read_one_time() {
        let store = FileStore::default();
        let dir = folder(UNIT, 1_000);
        let (records, units, expansions) = (store.records(dir.path()).unwrap(), store.units(dir.path()).unwrap(), store.expansions(dir.path()).unwrap());
        // The files go away: each value comes from memory, with no copy.
        std::fs::remove_file(dir.path().join("k.lib.1.jsonl")).unwrap();
        assert!(store.records(dir.path()).unwrap().same_list(&records));
        assert!(Arc::ptr_eq(&store.units(dir.path()).unwrap(), &units));
        assert!(Arc::ptr_eq(&store.expansions(dir.path()).unwrap(), &expansions));
    }

    // rule- R86.1-1
    #[test]
    fn a_folder_that_a_build_makes_again_is_read_again() {
        let store = FileStore::default();
        let dir = folder(UNIT, 1_000);
        assert_eq!(store.units(dir.path()).unwrap()[0].cfg.len(), 1);
        assert_eq!(store.records(dir.path()).unwrap()[0].text, "t");
        // A new build: the same size of the files, a new time of `.complete`.
        std::fs::write(dir.path().join("k.lib.1.jsonl"), format!("{}\n{{\"name\":\"k::f\",\"level\":\"n0\",\"text\":\"u\"}}\n", UNIT.replace("unix", "unit"))).unwrap();
        std::fs::File::options().write(true).open(dir.path().join(COMPLETE)).unwrap().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(2_000)).unwrap();
        assert!(store.units(dir.path()).unwrap()[0].cfg.contains("unit"));
        assert_eq!(store.records(dir.path()).unwrap()[0].text, "u");
        // A folder with no `.complete` file is never in memory.
        store.remove(dir.path());
        assert_eq!(store.in_memory(dir.path()), 0);
    }

    // [R86.4-1] A folder that the store writes is the folder that it reads.
    // rule+ R86.4-1
    #[test]
    fn the_store_reads_the_folder_that_it_writes() {
        let store = FileStore::default();
        let dir = tempfile::tempdir().unwrap();
        let build = dir.path().join("build.jsonl");
        std::fs::write(&build, "{\"name\":\"k::f\",\"level\":\"n0\",\"text\":\"t\"}\n").unwrap();
        let folder = dir.path().join("n0");
        store.create(&folder).unwrap();
        store.put_file(&folder, "k.lib.1.jsonl", &build).unwrap();
        store.set_mark(&folder, ".other-files", "x\n").unwrap();
        assert!(!store.is_complete(&folder) && store.key(&folder).is_none());
        store.complete(&folder).unwrap();
        assert!(store.is_complete(&folder) && store.key(&folder).is_some());
        assert_eq!(store.mark(&folder, ".other-files").as_deref(), Some("x\n"));
        assert_eq!(store.records(&folder).unwrap()[0].text, "t");
    }

    // rule- R86.4-1
    #[test]
    fn a_removed_folder_has_no_records_and_no_marks() {
        let store = FileStore::default();
        let dir = folder(UNIT, 1_000);
        assert!(store.records(dir.path()).is_ok());
        store.remove(dir.path());
        assert!(!store.is_complete(dir.path()));
        assert_eq!(store.mark(dir.path(), COMPLETE), None);
        assert!(store.records(dir.path()).is_err());
    }

    // [R86.3-4] The typed parse of the expansions gives the result of the generic parse.
    // rule+ R86.3-4
    #[test]
    fn the_typed_parse_of_the_expansions_is_the_generic_parse() {
        let good = UNIT.replace(r#",{"name":"bad"}"#, "");
        for line in [good.as_str(), UNIT] {
            let dir = folder(line, 1_000);
            let typed = read_expansions(dir.path()).unwrap();
            let generic = generic_expansions(dir.path()).unwrap();
            assert_eq!(typed, generic, "{line}");
            assert_eq!(typed[0].list.len(), 1);
        }
    }

    // rule- R86.3-4
    #[test]
    fn a_unit_with_no_expansion_list_has_no_expansions() {
        let dir = folder(r#"{"unit":{"target":"k.lib.1","cfg":[],"files":[]}}"#, 1_000);
        assert!(read_expansions(dir.path()).is_none());
        assert!(generic_expansions(dir.path()).is_none());
    }
}
