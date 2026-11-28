//! The store of the records of the cache folders (design 86, S4; design 88).
//!
//! Each read and each write of a record folder goes through [`RecordStore`]: the records of
//! the bodies, the unit records, the expansions, and the marks of a folder (`.complete` and the
//! other mark files). The callers do not know the files. [`FileStore`] keeps the record files of
//! the driver in the folders, and the data that Semilla makes from them in one database (design
//! 88): the index of each record file, and the entries of [`ContentStore`].
//!
//! [R86.1-1] The store reads each part of a folder one time in a process: a second read gives
//! the value in memory, with no copy (R86.3-2). The key of a value is the folder and the time
//! of its `.complete` file: a build that makes the folder again gives a new key.
//!
//! [R88.1-1] One database (libmdbx) in the cache folder of a repository holds each stored entry
//! that Semilla makes. Only this module opens it.

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
    /// [R89.3-2] The items of the expanded code and the macro call sites of a folder (`None`: a
    /// file has no list of the items).
    fn made(&self, folder: &Path) -> Option<Arc<semilla_normal::Made>>;
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
    made: BTreeMap<FolderKey, Option<Arc<semilla_normal::Made>>>,
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
        let records = match database_for(folder) {
            Some(db) => read_indexed(&db, folder, key.as_ref()),
            None => semilla_normal::read_records(folder),
        }
        .map(semilla_normal::Records::from);
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

    fn made(&self, folder: &Path) -> Option<Arc<semilla_normal::Made>> {
        self.cached(folder, |memory| &mut memory.made, || {
            semilla_core::timing::phase("read the expanded items of a cache folder", || {
                let mut made = semilla_normal::Made::default();
                semilla_normal::read_made(folder, &mut made).then(|| Arc::new(made))
            })
        })
    }

    fn mark(&self, folder: &Path, name: &str) -> Option<String> {
        std::fs::read_to_string(folder.join(name)).ok()
    }

    fn set_mark(&self, folder: &Path, name: &str, text: &str) -> Result<()> {
        std::fs::write(folder.join(name), text)?;
        Ok(())
    }

    fn remove(&self, folder: &Path) {
        // [R89.4-1] The entries of the files of the folder go in one transaction, before the
        // files: no entry stays with no folder.
        if let Some(db) = database_for(folder) {
            let prefix = folder_prefix(&db, folder);
            let _ = db.remove_where(Kind::Index, &|key| key.starts_with(&prefix));
        }
        let mut memory = self.memory();
        memory.records.retain(|(path, _), _| path != folder);
        memory.units.retain(|(path, _), _| path != folder);
        memory.expansions.retain(|(path, _), _| path != folder);
        memory.made.retain(|(path, _), _| path != folder);
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
        // [R88.2-2] The index of each record file of the build, one time, when the build
        // completes. A store with no database reads the files.
        if let (Some(db), Some(key)) = (database_for(folder), self.key(folder)) {
            semilla_core::timing::phase(INDEX_PHASE, || index_folder(&db, folder, &key));
        }
        Ok(())
    }
}

// ----- the database (design 88) -----

/// The name of the folder of the database in the cache folder of a repository.
pub(crate) const DATABASE: &str = "store";

/// The phase name of the index of a record folder in the timing lines.
pub(crate) const INDEX_PHASE: &str = "the index of a record folder";

/// The kind of an entry of the database: the first byte of its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The index of a record file (R88.2-1).
    Index,
    /// The syntax data of a tree (R88.5-1).
    Syntax,
    /// The output of `cargo metadata` for a tree (R88.5-1).
    Metadata,
    /// [R89.1-1] A construction of the opaque inputs of two trees (design 89).
    Opaque,
    /// [R91.1-1] The stored result of one item of a phase (design 91): the key text starts
    /// with the name of the phase.
    Item,
    /// [R93.1-1] The stored result of a step check (design 93.1).
    Step,
    /// [R93.3-1] The record of one body by its key (design 93.3).
    Body,
}

impl Kind {
    fn byte(self) -> u8 {
        match self {
            Kind::Index => b'i',
            Kind::Syntax => b's',
            Kind::Metadata => b'm',
            Kind::Opaque => b'o',
            Kind::Item => b'r',
            Kind::Step => b't',
            Kind::Body => b'b',
        }
    }
}

/// One entry for a write: the kind, the key text, the format key, and the payload.
pub(crate) type Put = (Kind, String, &'static str, Vec<u8>);

/// [R88.1-1] The database of the store: one libmdbx database in the cache folder of a
/// repository. A value has the format key, the full key text, and the payload: a read checks the
/// two (R88.4-1). A write of a list of entries is one transaction (R88.4-2): a stop in the
/// middle leaves the old state or the new state. libmdbx serves two processes on one database
/// with its lock file (R88.4-3).
pub(crate) struct Db {
    env: libmdbx::Database<libmdbx::NoWriteMap>,
    /// The cache folder of the repository: the parent of the folder of the database.
    pub(crate) cache: PathBuf,
}

/// The largest size of the database file (a map of the address space, not of the disk).
const MOST_BYTES: isize = 1 << 40;

/// The step by which the database file grows.
const GROWTH: isize = 1 << 26;

impl Db {
    fn open(cache: &Path) -> Result<Db> {
        let dir = cache.join(DATABASE);
        std::fs::create_dir_all(&dir)?;
        // A commit writes no sync to the disk: a stop of the system can lose the last
        // transactions, but the database stays whole (the entries are data that Semilla can
        // make again). A stop of a process loses no committed transaction.
        let options = libmdbx::DatabaseOptions {
            mode: libmdbx::Mode::ReadWrite(libmdbx::ReadWriteOptions { sync_mode: libmdbx::SyncMode::SafeNoSync, max_size: Some(MOST_BYTES), growth_step: Some(GROWTH), ..Default::default() }),
            ..Default::default()
        };
        let env = libmdbx::Database::open_with_options(&dir, options)?;
        Ok(Db { env, cache: cache.to_path_buf() })
    }

    /// The key of an entry in the database: the kind, then a digest of the key text (a key of
    /// the database has a limit of size). The value has the full key text.
    fn key(kind: Kind, key: &str) -> Vec<u8> {
        let mut out = vec![kind.byte()];
        out.extend_from_slice(&semilla_normal::index::digest(key));
        out
    }


    /// Reads the payload of an entry with `read`, in one read transaction. `None` if the entry
    /// is not there, has another format key or another key text, or `read` does not accept it.
    pub(crate) fn get<R>(&self, kind: Kind, key: &str, format: &str, read: impl FnOnce(&[u8]) -> Option<R>) -> Option<R> {
        let started = std::time::Instant::now();
        let result = (|| {
            let txn = self.env.begin_ro_txn().ok()?;
            let table = txn.open_table(None).ok()?;
            let value: std::borrow::Cow<'_, [u8]> = txn.get(&table, Db::key(kind, key)).ok()??;
            let (head, payload) = split_value(&value)?;
            if head.0 != format.as_bytes() {
                semilla_core::timing::count(DB_PHASE, "entries of another format", 1);
                return None;
            }
            if head.1 != key.as_bytes() {
                return None;
            }
            let result = read(payload);
            // [R89.7-1] The pages of the value leave the memory of the process after the read.
            if let std::borrow::Cow::Borrowed(bytes) = &value {
                release_pages(bytes);
            }
            result
        })();
        semilla_core::timing::add(DB_PHASE, started);
        result
    }

    /// Writes each entry in one write transaction.
    pub(crate) fn put(&self, entries: Vec<Put>) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let started = std::time::Instant::now();
        let result = (|| -> Result<()> {
            let txn = self.env.begin_rw_txn()?;
            let table = txn.open_table(None)?;
            for (at, (kind, key, format, payload)) in entries.iter().enumerate() {
                txn.put(&table, Db::key(*kind, key), join_value(format, key, payload), libmdbx::WriteFlags::UPSERT)?;
                #[cfg(test)]
                tests::stop_point(at);
                #[cfg(not(test))]
                let _ = at;
            }
            txn.commit()?;
            Ok(())
        })();
        semilla_core::timing::add(DB_PHASE, started);
        semilla_core::timing::count(DB_PHASE, "entries written", entries.len() as u64);
        result
    }
}

/// [R93.2-1] A variable for the tests only: with it, a process makes no steady point in the
/// background (the test of the wait before the file grows).
pub(crate) const NO_BACKGROUND_SYNC: &str = "SEMILLA_TEST_NO_BACKGROUND_SYNC";

/// [R93.2-1] The phase name of the steady points of the database in the timing lines.
pub(crate) const SYNC_PHASE: &str = "the steady point of the database (a thread of its own)";

impl Db {
    /// [R93.2-1] Makes a steady point: the data of the earlier commands goes to the disk, and the
    /// pages that they freed can serve the writes of this command. A commit does not sync
    /// (`SafeNoSync`), and libmdbx makes a steady point before its file grows: then the write
    /// waits for the sync of each page that is not on the disk yet. libmdbx writes the pages
    /// first with no lock (a pre-sync), so a write of this command does not wait for them. An
    /// error changes nothing: the next steady point syncs the data.
    pub(crate) fn steady_point(&self) {
        let started = std::time::Instant::now();
        let synced = self.env.sync(true).is_ok();
        semilla_core::timing::add(SYNC_PHASE, started);
        semilla_core::timing::count(SYNC_PHASE, if synced { "steady points" } else { "failed steady points" }, 1);
    }
}

impl Db {
    /// [R93.3-1] The key texts of each entry of a kind, in one read transaction.
    pub(crate) fn key_texts(&self, kind: Kind) -> Vec<String> {
        let started = std::time::Instant::now();
        let found = (|| -> Result<Vec<String>> {
            let txn = self.env.begin_ro_txn()?;
            let table = txn.open_table(None)?;
            let mut found = Vec::new();
            let mut cursor = txn.cursor(&table)?;
            for entry in cursor.iter_from::<std::borrow::Cow<'_, [u8]>, std::borrow::Cow<'_, [u8]>>(&[kind.byte()]) {
                let (key, value) = entry?;
                if key.first() != Some(&kind.byte()) {
                    break;
                }
                if let Some(((_, text), _)) = split_value(&value)
                    && let Ok(text) = std::str::from_utf8(text)
                {
                    found.push(text.to_string());
                }
            }
            Ok(found)
        })()
        .unwrap_or_default();
        semilla_core::timing::add(DB_PHASE, started);
        found
    }
}

impl Db {
    /// [R89.4-1, R89.4-2] Removes each entry of a kind whose key text `remove` accepts, in one write
    /// transaction (a stop in the middle leaves each entry). The result is the count of the
    /// removed entries.
    pub(crate) fn remove_where(&self, kind: Kind, remove: &dyn Fn(&str) -> bool) -> Result<usize> {
        let started = std::time::Instant::now();
        let result = (|| -> Result<usize> {
            let txn = self.env.begin_rw_txn()?;
            let table = txn.open_table(None)?;
            let mut gone: Vec<Vec<u8>> = Vec::new();
            {
                let mut cursor = txn.cursor(&table)?;
                for entry in cursor.iter_from::<std::borrow::Cow<'_, [u8]>, std::borrow::Cow<'_, [u8]>>(&[kind.byte()]) {
                    let (key, value) = entry?;
                    if key.first() != Some(&kind.byte()) {
                        break;
                    }
                    // A value of another form has no key text: it stays.
                    let Some(((_, text), _)) = split_value(&value) else { continue };
                    if std::str::from_utf8(text).is_ok_and(remove) {
                        gone.push(key.to_vec());
                    }
                }
            }
            for (at, key) in gone.iter().enumerate() {
                txn.del(&table, key, None)?;
                #[cfg(test)]
                tests::stop_point(at);
                #[cfg(not(test))]
                let _ = at;
            }
            txn.commit()?;
            Ok(gone.len())
        })();
        semilla_core::timing::add(DB_PHASE, started);
        if let Ok(count) = &result {
            semilla_core::timing::count(DB_PHASE, "entries removed", *count as u64);
        }
        result
    }
}

/// [R89.4-1] The key texts of the index entries of the files of a folder start with this text.
fn folder_prefix(db: &Db, folder: &Path) -> String {
    let rel = folder.strip_prefix(&db.cache).unwrap_or(folder);
    format!("index {}/", rel.display())
}

/// [R91.4-1] The inputs that the stored steps can use: the pair of trees of each stored step
/// (`OLD:NEW`), and the digest of the content of each Rust file of the trees of the stored
/// steps.
#[derive(Clone, Debug, Default)]
pub(crate) struct Live {
    pub(crate) pairs: std::collections::BTreeSet<String>,
    pub(crate) texts: std::collections::BTreeSet<String>,
    /// [R93.4-1] The keys of the body records of the records folders of the trees of the stored
    /// steps (the pointer lists, [`BODIES_FILE`]).
    pub(crate) bodies: std::collections::BTreeSet<String>,
}

/// [R89.4-2] `semilla cache --clean`: removes each entry of the database that no folder of the
/// cache can use: the index of a record file that is not there, and an entry of a tree (the
/// syntax data, the opaque inputs) of another Semilla binary (its key has the identity of the
/// binary that made it). [R91.4-1] It also removes each stored result of a pair of trees that
/// no stored step has (a construction of the opaque inputs), and the syntax data of each file
/// whose content no file of a tree of a stored step has. The result is the count of the
/// removed entries.
pub(crate) fn clean_database(cache: &Path, live: &Live) -> Result<usize> {
    // [R99.8-1] The new body records of this process are in the store before the clean.
    join_body_writes();
    let Some(db) = database_for(cache) else { return Ok(0) };
    let root = db.cache.clone();
    let mut count = db.remove_where(Kind::Index, &|key| key.strip_prefix("index ").is_none_or(|rel| !root.join(rel).is_file()))?;
    let identity = crate::store::binary_identity();
    count += db.remove_where(Kind::Syntax, &|key| !key.ends_with(&format!(" {identity}")))?;
    count += db.remove_where(Kind::Opaque, &|key| !keeps(key, &identity, live))?;
    count += db.remove_where(Kind::Item, &|key| !keeps(key, &identity, live))?;
    // [R93.4-1] The stored step results of a pair of trees that no stored step has, and each
    // body record that no records folder of a tree of a stored step uses.
    count += db.remove_where(Kind::Step, &|key| !keeps(key, &identity, live))?;
    count += db.remove_where(Kind::Body, &|key| !key.strip_prefix(BODY_PHASE).and_then(|rest| rest.strip_prefix(' ')).is_some_and(|body| live.bodies.contains(body)))?;
    Ok(count)
}

/// [R89.4-2, R91.4-1] True if the clean keeps an entry of a stored result: the entry is of this
/// binary, and a stored step can use it. A key of another form is not kept.
fn keeps(key: &str, identity: &str, live: &Live) -> bool {
    let words: Vec<&str> = key.split(' ').collect();
    match words.as_slice() {
        // `opaque DIGEST BINARY OLD:NEW SLOT` (R89.1-1 with the pair of R91.4-1).
        ["opaque", _, binary, pair, _] => *binary == identity && live.pairs.contains(*pair),
        // `file-syntax TEXT CONTEXT BINARY` (R91.2-1).
        [FILE_PHASE, text, _, binary] => *binary == identity && live.texts.contains(*text),
        // `step OLD:NEW DIGEST BINARY` (R93.1-1).
        [STEP_PHASE, pair, _, binary] => *binary == identity && live.pairs.contains(*pair),
        _ => false,
    }
}

/// [R89.7-1] Gives the whole pages of a value of the map of the database back to the system
/// (`madvise` with `MADV_DONTNEED`). The map is a read-only map of the file: the pages stay in
/// the cache of the file system, and a later read of the value maps them again. So a read of
/// the index of each record file does not keep the bytes of the index in the memory of the
/// process (the peak memory of a step of design 88 had them).
fn release_pages(bytes: &[u8]) {
    const PAGE: usize = 4096;
    const MADV_DONTNEED: i32 = 4;
    unsafe extern "C" {
        fn madvise(addr: *mut std::ffi::c_void, len: usize, advice: i32) -> i32;
    }
    let start = (bytes.as_ptr() as usize).div_ceil(PAGE) * PAGE;
    let end = (bytes.as_ptr() as usize + bytes.len()) / PAGE * PAGE;
    if end > start {
        // SAFETY: the range is inside the map of the database, and the map is not written in
        // this process (`NoWriteMap`, a read transaction). The call only drops the pages from
        // the page table of the process; each later read faults them in from the file.
        unsafe {
            madvise(start as *mut std::ffi::c_void, end - start, MADV_DONTNEED);
        }
    }
}

/// The value of an entry: the format key, a line end, the key text, a line end, the payload.
fn join_value(format: &str, key: &str, payload: &[u8]) -> Vec<u8> {
    let mut value = Vec::with_capacity(format.len() + key.len() + payload.len() + 2);
    value.extend_from_slice(format.as_bytes());
    value.push(b'\n');
    value.extend_from_slice(key.as_bytes());
    value.push(b'\n');
    value.extend_from_slice(payload);
    value
}

/// The format key and the key text of a value, and its payload.
fn split_value(value: &[u8]) -> Option<((&[u8], &[u8]), &[u8])> {
    let first = value.iter().position(|byte| *byte == b'\n')?;
    let rest = &value[first + 1..];
    let second = rest.iter().position(|byte| *byte == b'\n')?;
    Some(((&value[..first], &rest[..second]), &rest[second + 1..]))
}

/// The phase name of the database in the timing lines.
pub(crate) const DB_PHASE: &str = "the database of the store";

/// [R88.6-1] The phase name of the counts of the new work of a command in the timing lines:
/// the bodies from the index and the bodies decoded, the files from the store and the files
/// parsed, the trees written, and the cargo commands run.
pub(crate) const WORK_PHASE: &str = semilla_core::timing::WORK_PHASE;

/// [R88.6-1] Adds the counts of the new work since the last timing lines: each of the six counts
/// has a value, also 0.
pub(crate) fn count_work() {
    use std::sync::atomic::Ordering::Relaxed;
    let decoded = semilla_core::timing::total(READ_PHASE).counts.get("records parsed").copied().unwrap_or(0);
    semilla_core::timing::count(WORK_PHASE, "bodies from the index", semilla_normal::FROM_INDEX.swap(0, Relaxed));
    semilla_core::timing::count(WORK_PHASE, "bodies decoded", decoded);
    semilla_core::timing::count(WORK_PHASE, "files from the store", 0);
    semilla_core::timing::count(WORK_PHASE, "files parsed", 0);
    semilla_core::timing::count(WORK_PHASE, "trees written", 0);
    semilla_core::timing::count(WORK_PHASE, "cargo commands run", semilla_normal::cargo::CARGO_RUNS.swap(0, Relaxed));
    // [R89.5-1] The counts of each stored kind of design 89: from the store, and computed.
    for name in [semilla_verify::OPAQUE_FROM_STORE, semilla_verify::OPAQUE_COMPUTED] {
        semilla_core::timing::count(WORK_PHASE, name, 0);
    }
}

/// [R88.1-1] The entries by content next to the record folders (design 88, F5): the syntax data
/// and the cargo metadata of a tree. The cache folder names the database.
pub(crate) trait ContentStore {
    /// The payload of an entry with this format key, or `None`.
    fn content(&self, cache: &Path, kind: Kind, key: &str, format: &str) -> Option<Vec<u8>>;
    /// Writes one entry (one transaction).
    fn put_content(&self, cache: &Path, kind: Kind, key: &str, format: &'static str, payload: Vec<u8>) -> Result<()>;
}

impl ContentStore for FileStore {
    fn content(&self, cache: &Path, kind: Kind, key: &str, format: &str) -> Option<Vec<u8>> {
        database_for(cache)?.get(kind, key, format, |payload| Some(payload.to_vec()))
    }

    fn put_content(&self, cache: &Path, kind: Kind, key: &str, format: &'static str, payload: Vec<u8>) -> Result<()> {
        match database_for(cache) {
            Some(db) => db.put(vec![(kind, key.to_string(), format, payload)]),
            None => Ok(()),
        }
    }
}

/// [R89.1-1] The format key of a stored construction of the opaque inputs: the JSON list of the
/// queries of the rule of the build configuration with their answers, a line end, and the JSON
/// form of the construction.
pub(crate) const OPAQUE_FORMAT: &str = "semilla-opaque-1";

/// [R89.1-1] The store of the phase results of the verifier in the database of a cache folder.
pub(crate) struct PhaseDb {
    db: Arc<Db>,
    /// [R91.4-1] The pair of trees of the step (`OLD:NEW`).
    pair: String,
}

impl PhaseDb {
    /// The store of the database of a cache folder for the check of a step with these trees, or
    /// `None` (no database).
    pub(crate) fn of(cache: &Path, before: &str, after: &str) -> Option<Arc<dyn semilla_verify::PhaseStore>> {
        let db = database_for(cache)?;
        Some(Arc::new(PhaseDb { db, pair: format!("{before}:{after}") }))
    }

    fn kind(kind: semilla_verify::PhaseKind) -> (Kind, &'static str, &'static str) {
        match kind {
            semilla_verify::PhaseKind::Opaque => (Kind::Opaque, "opaque", OPAQUE_FORMAT),
        }
    }
}

impl semilla_verify::PhaseStore for PhaseDb {
    /// The key text: the name of the kind, the digest of the inputs (each with its length), the
    /// identity of the Semilla binary (the code of the phase), and [R91.4-1] the pair of trees of
    /// the step (the clean removes the entries of a pair that no stored step has).
    fn key(&self, kind: semilla_verify::PhaseKind, inputs: &[&[u8]]) -> String {
        let mut all = Vec::with_capacity(inputs.iter().map(|input| input.len() + 8).sum());
        for input in inputs {
            all.extend_from_slice(&(input.len() as u64).to_le_bytes());
            all.extend_from_slice(input);
        }
        let digest: String = semilla_normal::index::digest_bytes(&all).iter().map(|byte| format!("{byte:02x}")).collect();
        format!("{} {digest} {} {}", PhaseDb::kind(kind).1, crate::store::binary_identity(), self.pair)
    }

    fn get(&self, kind: semilla_verify::PhaseKind, key: &str) -> Option<Vec<u8>> {
        let (kind, _, format) = PhaseDb::kind(kind);
        self.db.get(kind, key, format, |payload| Some(payload.to_vec()))
    }

    fn put(&self, kind: semilla_verify::PhaseKind, key: &str, payload: Vec<u8>) {
        let (kind, _, format) = PhaseDb::kind(kind);
        let _ = self.db.put(vec![(kind, key.to_string(), format, payload)]);
    }
}

/// [R91.1-1] The text of a digest of the stored results of design 91: the first 16 bytes of
/// SHA-256, as hexadecimal.
pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    semilla_normal::index::digest_bytes(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// [R93.1-1] The name of the kind of a stored step result in its key text.
pub(crate) const STEP_PHASE: &str = "step";

/// [R93.1-1] The format key of a stored step result: the JSON of the stored form (the result of
/// the check, the configurations of the step, and the records folders that the check read).
pub(crate) const STEP_FORMAT: &str = "semilla-step-1";

/// [R93.1-1] The key text of a stored step result: the pair of trees of the step (for the
/// clean), the digest of each other input of the check, and the identity of the Semilla binary.
pub(crate) fn step_key(before: &str, after: &str, inputs: &str) -> String {
    format!("{STEP_PHASE} {before}:{after} {} {}", hex_digest(inputs.as_bytes()), crate::store::binary_identity())
}

/// [R93.3-1] The name of the kind of a body record in its key text: `body KEY`. The key of the
/// driver has each input of the record, also the driver and the tool chain (design 93.3).
pub(crate) const BODY_PHASE: &str = "body";

/// [R93.3-1] The format key of a body record. [R99.8-1] The payload is the item list of the
/// places of the record (`relocate::Used`, one JSON line), then the JSON line of the record.
pub(crate) const BODY_FORMAT: &str = "semilla-body-2";

/// [R93.4-1] The mark of a records folder with the keys of its body records (its pointer list),
/// one key for each line.
pub(crate) const BODIES_FILE: &str = ".bodies";

/// [R93.6-1] The phase name of the body records of the store in the timing lines.
pub(crate) const BODY_STORE_PHASE: &str = "the body records of the store (design 93)";

/// [R93.3-1] The keys of the body records of the store: the file of the known keys of a build.
pub(crate) fn known_bodies(db: &Db) -> String {
    join_body_writes();
    let mut text = String::new();
    for key in db.key_texts(Kind::Body) {
        if let Some(body) = key.strip_prefix(BODY_PHASE).and_then(|rest| rest.strip_prefix(' ')) {
            text.push_str(body);
            text.push('\n');
        }
    }
    text
}

/// [R93.3-1] An error of a record file of a build with the keys on: a pointer line whose record
/// the store does not have. The build runs again with no keys.
#[derive(Debug)]
pub(crate) struct MissingBody(pub(crate) String);

impl std::fmt::Display for MissingBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the store has no body record with the key {}", self.0)
    }
}

impl std::error::Error for MissingBody {}

/// [R93.3-1] Makes a record file of a build with the keys on into a full record file: each
/// pointer line gets the stored record of its key, and the last line (the keys) goes. Each new
/// record with a key goes into the store, in one write transaction. The keys of the lines of
/// the file go to `keys` (the pointer list of the file). A file with no keys line (a fresh
/// unit, or a build with no keys) does not change.
pub(crate) fn resolve_bodies(db: &Arc<Db>, file: &Path, keys: &Path) -> Result<()> {
    use std::io::{BufRead as _, Write as _};
    // [R93.7-1] A stream of the lines: the file is read two times (the place of the last line,
    // then each line), and no text of the whole file is in memory.
    let mut last = 0u64;
    let mut count = 0usize;
    {
        let mut reader = std::io::BufReader::new(std::fs::File::open(file)?);
        let mut line = Vec::new();
        let mut at = 0u64;
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            last = at;
            at += read as u64;
            count += 1;
        }
    }
    let tail = {
        use std::io::{Read as _, Seek as _};
        let mut handle = std::fs::File::open(file)?;
        handle.seek(std::io::SeekFrom::Start(last))?;
        let mut text = String::new();
        handle.read_to_string(&mut text)?;
        text
    };
    let Ok(serde_json::Value::Object(tail)) = serde_json::from_str::<serde_json::Value>(tail.trim_end()) else { return Ok(()) };
    let Some(serde_json::Value::Array(list)) = tail.get(KEYS_MEMBER) else { return Ok(()) };
    let list: Vec<Option<String>> = list.iter().map(|key| key.as_str().map(str::to_string)).collect();
    // [R99.8-1] The table of the items of the unit (design 99.8): a stored record moves by the
    // start of each item in this tree, and a new record goes into the store with its items.
    let table = semilla_normal::anchors::Table::new(tail.get(ANCHORS_MEMBER).and_then(|found| serde_json::from_value::<Vec<semilla_normal::anchors::FileAnchors>>(found.clone()).ok()).unwrap_or_default());
    drop(tail);
    // The first line is the unit record; each other line but the last is a body line.
    if count != list.len() + 2 {
        anyhow::bail!("the keys line of {} does not have one key for each body line", file.display());
    }
    let temporary = file.with_extension("resolve");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&temporary)?);
    let mut reader = std::io::BufReader::new(std::fs::File::open(file)?);
    // [R99.8-1] The new records with a key, for the store.
    let mut fresh: Vec<(String, String)> = Vec::new();
    let (mut from_store, mut new, mut written, mut moved) = (0u64, 0u64, 0u64, 0u64);
    let mut line = String::new();
    for at in 0..count - 1 {
        line.clear();
        reader.read_line(&mut line)?;
        let text = line.trim_end_matches('\n');
        let key = at.checked_sub(1).and_then(|at| list[at].clone());
        let pointer = text.starts_with(&format!("{{\"{POINTER_MEMBER}\"")).then(|| serde_json::from_str::<serde_json::Value>(text).ok().and_then(|value| value.get(POINTER_MEMBER).and_then(|key| key.as_str()).map(str::to_string))).flatten();
        match (pointer, key) {
            (Some(pointer), _) => {
                // [R99.8-1] The stored record, with each place moved by the move of its item.
                let found = db.get(Kind::Body, &format!("{BODY_PHASE} {pointer}"), BODY_FORMAT, |payload| {
                    let text = std::str::from_utf8(payload).ok()?;
                    let (head, record) = text.split_once('\n')?;
                    let used: crate::relocate::Used = serde_json::from_str(head).ok()?;
                    match crate::relocate::relocate(record, &used, &table) {
                        Ok(None) => out.write_all(record.as_bytes()).ok().map(|()| false),
                        Ok(Some(moved)) => out.write_all(moved.as_bytes()).ok().map(|()| true),
                        Err(_) => None,
                    }
                });
                let Some(was_moved) = found else {
                    drop(out);
                    let _ = std::fs::remove_file(&temporary);
                    return Err(MissingBody(pointer).into());
                };
                from_store += 1;
                moved += u64::from(was_moved);
            }
            (None, Some(key)) => {
                out.write_all(text.as_bytes())?;
                fresh.push((key, text.to_string()));
                new += 1;
                written += text.len() as u64;
            }
            (None, None) => out.write_all(text.as_bytes())?,
        }
        out.write_all(b"\n")?;
    }
    out.flush()?;
    drop(out);
    std::fs::rename(&temporary, file)?;
    if let Some(parent) = keys.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(keys, list.iter().map(|key| format!("{}\n", key.as_deref().unwrap_or("-"))).collect::<String>())?;
    store_fresh(db, table, fresh);
    semilla_core::timing::count(BODY_STORE_PHASE, "records from the store", from_store);
    semilla_core::timing::count(BODY_STORE_PHASE, "stored records moved to new places", moved);
    semilla_core::timing::count(BODY_STORE_PHASE, "records written", new);
    semilla_core::timing::count(BODY_STORE_PHASE, "bytes written", written);
    Ok(())
}

/// [R99.8-1] Writes the new records of a record file into the store: the payload is the item list
/// of the places of a record (`relocate::Used`), then the record; a record with no item list
/// does not go into the store. [R93.7-1] A batch of 4 MB is one write transaction. [R96.9-2] In a
/// command, the work runs in a thread of its own (the end of the command waits for it): the
/// unit task does not wait for the walk of each new record. A key that is not in the store yet
/// is not in the file of the known keys of a later build, so that build prints its record.
fn store_fresh(db: &Arc<Db>, table: semilla_normal::anchors::Table, fresh: Vec<(String, String)>) {
    if fresh.is_empty() {
        return;
    }
    let work = move |db: &Db| {
        let mut puts: Vec<Put> = Vec::new();
        let mut waiting = 0usize;
        for (key, text) in fresh {
            let Some(used) = crate::relocate::used(&text, &table) else { continue };
            let Ok(head) = serde_json::to_string(&used) else { continue };
            let mut payload = head.into_bytes();
            payload.push(b'\n');
            payload.extend_from_slice(text.as_bytes());
            waiting += payload.len();
            puts.push((Kind::Body, format!("{BODY_PHASE} {key}"), BODY_FORMAT, payload));
            if waiting > MOST_WAITING {
                let _ = db.put(std::mem::take(&mut puts));
                waiting = 0;
            }
        }
        let _ = db.put(puts);
    };
    if !ASYNC_WRITES.load(std::sync::atomic::Ordering::SeqCst) {
        work(db);
        return;
    }
    let db = db.clone();
    let write = std::thread::spawn(move || {
        let _span = semilla_core::tasks::span("the write of the new body records");
        work(&db);
    });
    BODY_WRITES.lock().unwrap_or_else(|error| error.into_inner()).push(write);
}

/// [R99.8-1] The writes of new body records that run in their own threads.
static BODY_WRITES: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

/// [R99.8-1] Waits for each write of new body records: the file of the known keys of a build
/// then has each record of the builds before it, and the end of a command has each write.
pub(crate) fn join_body_writes() {
    let writes = std::mem::take(&mut *BODY_WRITES.lock().unwrap_or_else(|error| error.into_inner()));
    for write in writes {
        let _ = write.join();
    }
}

/// [R93.3-1] The member of a pointer line of the driver.
const POINTER_MEMBER: &str = "semilla-body";

/// [R93.3-1] The member of the keys line of the driver.
const KEYS_MEMBER: &str = "semilla-keys";

/// [R99.8-1] The member of the keys line of the driver with the table of the items of the unit.
const ANCHORS_MEMBER: &str = "semilla-anchors";

// ----- the task of a unit at its message (design 96) -----

/// [R96.2-1] The size and the time (nanoseconds) of a file: a change of the file changes them.
fn plain_state(file: &Path) -> Option<[u64; 2]> {
    let meta = std::fs::metadata(file).ok()?;
    let time = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() as u64;
    Some([meta.len(), time])
}

/// [R96.2-1] The results of the unit tasks of this process, by the path of the record file in
/// the records folder of the build: the state of the file after the task, and its index (`None`:
/// the lines do not have the plain form).
type UnitDone = BTreeMap<PathBuf, ([u64; 2], Option<semilla_normal::index::UnitIndex>)>;
static UNIT_DONE: Mutex<UnitDone> = Mutex::new(BTreeMap::new());

/// [R96.2-1] The indexes of the copies of the record files in a cache folder that is not
/// complete yet: the size of the copy and its index. `complete` takes them.
type CopyIndex = BTreeMap<PathBuf, (u64, Option<semilla_normal::index::UnitIndex>)>;
static COPY_INDEX: Mutex<CopyIndex> = Mutex::new(BTreeMap::new());

/// [R96.2-1] The phase name of the unit tasks in the timing lines.
pub(crate) const UNIT_PHASE: &str = "the tasks of the units at their messages (design 96)";

/// [R96.2-1] The task of one unit at the message of cargo: the pointer lines of its record file
/// get their records (R93.3-1), then the index of the file. The build of the other units runs
/// at the same time. The task reads the file only after the message of its unit (the caller
/// gives only the files of [`semilla_normal::cargo::message_units`]).
pub(crate) fn unit_task(db: &Arc<Db>, file: &Path, keys: &Path) -> Result<()> {
    let started = std::time::Instant::now();
    resolve_bodies(db, file, keys)?;
    // [R99.5-2] The index of the file is a stream (design 99): one line is in memory at a time,
    // and the task keeps no record. A file that changes while the task reads it gets no result:
    // the end of the build makes its index again.
    let Some(state) = plain_state(file) else { return Ok(()) };
    let index = semilla_normal::index::build_stream(std::io::BufReader::new(std::fs::File::open(file)?));
    if plain_state(file) != Some(state) {
        return Ok(());
    }
    UNIT_DONE.lock().unwrap_or_else(|error| error.into_inner()).insert(file.to_path_buf(), (state, index));
    semilla_core::timing::add(UNIT_PHASE, started);
    semilla_core::timing::count(UNIT_PHASE, "units done during the build", 1);
    Ok(())
}

/// [R96.2-1] True if a unit task resolved the file and the file did not change after the task.
pub(crate) fn resolved_by_task(file: &Path) -> bool {
    let done = UNIT_DONE.lock().unwrap_or_else(|error| error.into_inner());
    done.get(file).is_some_and(|(state, _)| plain_state(file).as_ref() == Some(state))
}

/// [R96.2-1] Forgets the unit tasks of the files of a records folder (a new build of the folder
/// starts).
pub(crate) fn forget_unit_tasks(folder: &Path) {
    UNIT_DONE.lock().unwrap_or_else(|error| error.into_inner()).retain(|file, _| !file.starts_with(folder));
}

/// [R96.2-1] The copy `to` of the record file `from` is in a cache folder: if a unit task made
/// the index of `from`, and `from` did not change after the task, the index is the index of
/// the copy (the copy has the same bytes).
pub(crate) fn index_for_copy(from: &Path, to: &Path) {
    let found = UNIT_DONE.lock().unwrap_or_else(|error| error.into_inner()).remove(from);
    let Some((state, index)) = found else { return };
    if plain_state(from) != Some(state) {
        return;
    }
    COPY_INDEX.lock().unwrap_or_else(|error| error.into_inner()).insert(to.to_path_buf(), (state[0], index));
}

/// [R96.2-1] The index of a unit task for a file of a cache folder, if the file has the size of
/// the bytes of the task. `None`: no such index (the caller makes the index).
fn take_copy_index(file: &Path, size: u64) -> Option<Option<semilla_normal::index::UnitIndex>> {
    let (known, index) = COPY_INDEX.lock().unwrap_or_else(|error| error.into_inner()).remove(file)?;
    (known == size).then_some(index)
}

/// [R91.2-1] The name of the phase of the stored syntax data of one file.
pub(crate) const FILE_PHASE: &str = "file-syntax";

/// [R91.2-1] The store of the syntax data of each file in the database of a cache folder. A read
/// is one read transaction; the parts of the parsed files of one load are written in batches of
/// at most [`MOST_WAITING`] bytes, each one write transaction (R93.7-1), and the last batch at
/// [`FileSyntaxDb::finish`].
pub(crate) struct FileSyntaxDb {
    db: Arc<Db>,
    puts: std::cell::RefCell<Vec<Put>>,
    /// [R93.7-1] The bytes of the parts that wait for a write.
    waiting: std::cell::Cell<usize>,
    counts: std::cell::Cell<(u64, u64)>,
}

/// [R93.7-1] The most bytes of the parts of the files that wait for a write: a larger batch is
/// one write transaction now, so the memory of a cold load does not hold the parts of each file.
const MOST_WAITING: usize = 4 << 20;

impl FileSyntaxDb {
    /// The store of the database of a cache folder, or `None` (no database).
    pub(crate) fn of(cache: &Path) -> Option<FileSyntaxDb> {
        Some(FileSyntaxDb { db: database_for(cache)?, puts: Default::default(), waiting: Default::default(), counts: Default::default() })
    }

    /// [R91.2-1] The key text of the part of a file: the name of the phase, the digest of the
    /// content of the file, the digest of its context (the path, the module path, the test
    /// state, the folder of the child modules, the edition, and the crate), and the identity of
    /// the Semilla binary (the version of the parser and of the loader).
    pub(crate) fn key(context: &str, text: &str) -> String {
        format!("{FILE_PHASE} {} {} {}", hex_digest(text.as_bytes()), hex_digest(context.as_bytes()), crate::store::binary_identity())
    }

    /// Writes the parts of the parsed files (one transaction), and gives the count of the files
    /// from the store and of the files parsed.
    pub(crate) fn finish(self) -> (u64, u64) {
        let _ = self.db.put(self.puts.take());
        self.counts.get()
    }
}

impl semilla_syntax::load::FileSyntaxStore for FileSyntaxDb {
    fn get(&self, context: &str, text: &str) -> Option<Vec<u8>> {
        self.db.get(Kind::Item, &FileSyntaxDb::key(context, text), semilla_syntax::stored::FILE_FORMAT, |payload| Some(payload.to_vec()))
    }

    fn put(&self, context: &str, text: &str, bytes: Vec<u8>) {
        self.waiting.set(self.waiting.get() + bytes.len());
        self.puts.borrow_mut().push((Kind::Item, FileSyntaxDb::key(context, text), semilla_syntax::stored::FILE_FORMAT, bytes));
        if self.waiting.get() > MOST_WAITING {
            self.waiting.set(0);
            let _ = self.db.put(self.puts.take());
        }
    }

    fn count(&self, from_store: bool) {
        let (stored, parsed) = self.counts.get();
        self.counts.set(if from_store { (stored + 1, parsed) } else { (stored, parsed + 1) });
    }
}

/// The databases of this process, by cache folder (`None`: the database does not open).
static DATABASES: Mutex<Vec<(PathBuf, Option<Arc<Db>>)>> = Mutex::new(Vec::new());

/// Opens the database of the cache folder of a repository, one time in a process. A database
/// that does not open gives one note, and the store works with the files only.
pub(crate) fn open_database(cache: &Path) -> Option<Arc<Db>> {
    let mut all = DATABASES.lock().unwrap_or_else(|error| error.into_inner());
    if let Some((_, db)) = all.iter().find(|(dir, _)| dir == cache) {
        return db.clone();
    }
    let db = match Db::open(cache) {
        Ok(db) => {
            let db = Arc::new(db);
            // [R93.2-1] The steady point of the data of the earlier commands runs in a thread of
            // its own, while this command does its other work.
            if std::env::var_os(NO_BACKGROUND_SYNC).is_none() {
                let background = Arc::clone(&db);
                std::thread::spawn(move || background.steady_point());
            }
            Some(db)
        }
        Err(error) => {
            eprintln!("note: Semilla cannot open the database of the store in {} ({error:#}); it reads the record files with no index", cache.display());
            None
        }
    };
    all.push((cache.to_path_buf(), db.clone()));
    db
}

/// The database of the cache folder that has the path (the longest cache folder that is a
/// start of the path), or `None`.
pub(crate) fn database_for(path: &Path) -> Option<Arc<Db>> {
    let all = DATABASES.lock().unwrap_or_else(|error| error.into_inner());
    all.iter().filter(|(dir, _)| path.starts_with(dir)).max_by_key(|(dir, _)| dir.as_os_str().len()).and_then(|(_, db)| db.clone())
}

/// The data that says if an index is the index of a record file now: the size and the time of
/// the file, and the time of the `.complete` file of its folder (in nanoseconds).
fn file_state(file: &Path, complete: std::time::SystemTime) -> Option<[u64; 3]> {
    let meta = std::fs::metadata(file).ok()?;
    let nanos = |time: std::time::SystemTime| time.duration_since(std::time::UNIX_EPOCH).ok().map(|since| since.as_nanos() as u64);
    Some([meta.len(), nanos(meta.modified().ok()?)?, nanos(complete)?])
}

/// [R88.2-2] The key text of the index of a record file: the folder (from the cache folder: the
/// folder set, the tree, the level, the configuration, and the tool settings) and the file name
/// (the unit).
fn index_key(db: &Db, file: &Path) -> String {
    let rel = file.strip_prefix(&db.cache).unwrap_or(file);
    format!("index {}", rel.display())
}

/// The payload of a stored index: the state of the file, then the encoded index.
fn index_payload(state: [u64; 3], index: &semilla_normal::index::UnitIndex) -> Vec<u8> {
    let mut out = Vec::new();
    for number in state {
        out.extend_from_slice(&number.to_le_bytes());
    }
    out.extend(semilla_normal::index::encode(index));
    out
}

/// The record files of a folder, in the order of their units.
fn record_files(folder: &Path) -> Result<Vec<PathBuf>> {
    use anyhow::Context as _;
    Ok(semilla_normal::unit_order(
        std::fs::read_dir(folder)
            .with_context(|| format!("cannot read {}", folder.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|extension| extension == "jsonl"))
            .collect(),
    ))
}

/// Builds and writes the index of each record file of a complete folder, in one transaction.
/// [R96.2-1] The index that a unit task made for the bytes of the file is the index of the file.
fn index_folder(db: &Arc<Db>, folder: &Path, key: &FolderKey) {
    let Ok(files) = record_files(folder) else { return };
    let mut puts = Vec::new();
    for file in files {
        let Some(state) = file_state(&file, key.1) else { continue };
        let index = match take_copy_index(&file, state[0]) {
            Some(made) => made,
            // [R99.5-2] The index of a file as a stream.
            None => match std::fs::File::open(&file) {
                Ok(handle) => semilla_normal::index::build_stream(std::io::BufReader::new(handle)),
                Err(_) => continue,
            },
        };
        if let Some(index) = index {
            puts.push((Kind::Index, index_key(db, &file), semilla_normal::index::FORMAT, index_payload(state, &index)));
            INDEXED.lock().unwrap_or_else(|error| error.into_inner()).insert(file.clone(), (state, Arc::new(index)));
        }
    }
    semilla_core::timing::count(INDEX_PHASE, "units indexed", puts.len() as u64);
    // [R96.9-2] The write of the index goes to a thread of its own: this process reads the
    // index from memory, and the end of the command waits for the write (`join_writes`).
    if !ASYNC_WRITES.load(std::sync::atomic::Ordering::SeqCst) {
        let _ = db.put(puts);
        return;
    }
    let db = db.clone();
    let write = std::thread::spawn(move || {
        let _span = semilla_core::tasks::span("the write of the index of a record folder");
        let _ = db.put(puts);
    });
    WRITES.lock().unwrap_or_else(|error| error.into_inner()).push(write);
}

/// [R96.9-2] True if the writes of the index run in their own threads: the command turns it on
/// (it waits for the writes at its end). A test of the store writes at once.
static ASYNC_WRITES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// [R96.9-2] Turns on the writes in their own threads (for a command that calls `join_writes`).
pub(crate) fn async_writes() {
    ASYNC_WRITES.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// [R96.9-2] The index of each record file of a folder that this process completed, by the
/// file: the state of the file (as in the database) and the index.
static INDEXED: Mutex<BTreeMap<PathBuf, ([u64; 3], Arc<semilla_normal::index::UnitIndex>)>> = Mutex::new(BTreeMap::new());

/// [R96.9-2] The writes of the store that run in their own threads.
static WRITES: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

/// [R96.9-2] Waits for each write of the store that runs in its own thread. The end of each
/// command calls it.
pub(crate) fn join_writes() {
    join_body_writes();
    let writes = std::mem::take(&mut *WRITES.lock().unwrap_or_else(|error| error.into_inner()));
    for write in writes {
        let _ = write.join();
    }
}

/// [R88.2-1] The records of a complete folder through the index of each record file: the index
/// from the database if it is the index of the file now, else a new index that goes into the
/// database. A file whose lines do not have the plain form takes the full parse, with its
/// errors. The result is the result of `semilla_normal::read_records`.
fn read_indexed(db: &Arc<Db>, folder: &Path, key: Option<&FolderKey>) -> Result<Vec<semilla_normal::BodyRecord>> {
    let files = record_files(folder)?;
    // [R88.2-1] The files decode in parallel; the records join in the order of the files, so
    // the result does not depend on the threads. [R99.5-1] Each file is a job of the pool of the
    // tasks (long-lived threads, the CPU share of R96.5-1).
    let jobs: Vec<Box<dyn FnOnce() -> Result<UnitRead> + Send>> = files
        .iter()
        .map(|file| {
            let (db, file, key) = (db.clone(), file.clone(), key.cloned());
            Box::new(move || read_unit(&db, &file, key.as_ref())) as Box<dyn FnOnce() -> Result<UnitRead> + Send>
        })
        .collect();
    let results: Vec<(usize, Result<UnitRead>)> = semilla_core::tasks::run_all(jobs).into_iter().map(|done| done.unwrap_or_else(|_| Err(anyhow::anyhow!("a read of a record file stopped")))).enumerate().collect();
    let mut records = Vec::new();
    let mut puts = Vec::new();
    let (mut from_index, mut indexed, mut full) = (0u64, 0u64, 0u64);
    for (_, read) in results {
        let read = read?;
        match &read.put {
            Some(_) => indexed += 1,
            None if read.full => full += 1,
            None if read.from_index => from_index += 1,
            None => indexed += 1,
        }
        puts.extend(read.put);
        let mut found = read.records;
        records.append(&mut found);
    }
    semilla_core::timing::count(READ_PHASE, "units from the index", from_index);
    semilla_core::timing::count(READ_PHASE, "units indexed now", indexed);
    if full > 0 {
        semilla_core::timing::count(READ_PHASE, "units with the full parse", full);
    }
    let _ = db.put(puts);
    Ok(records)
}

/// The read of one record file.
struct UnitRead {
    records: Vec<semilla_normal::BodyRecord>,
    /// The new index of the file, for the database.
    put: Option<Put>,
    from_index: bool,
    /// True if the file took the full parse.
    full: bool,
}

/// The records of one record file: from its index in the database, else from a new index, else
/// from the full parse.
fn read_unit(db: &Db, file: &Path, key: Option<&FolderKey>) -> Result<UnitRead> {
    use anyhow::Context as _;
    let state = key.and_then(|key| file_state(file, key.1));
    let handle = || std::fs::File::open(file).map(Arc::new).with_context(|| format!("cannot read {}", file.display()));
    // [R96.9-2] The index that this process made when the folder completed, if the file has
    // the same state. The read takes it (the memory of the store keeps the records of the
    // folder), so the index is not in memory for the rest of the command.
    let made = state.and_then(|state| {
        let mut indexed = INDEXED.lock().unwrap_or_else(|error| error.into_inner());
        let (known, _) = indexed.get(file)?;
        (*known == state).then(|| indexed.remove(file)).flatten().map(|(_, index)| index)
    });
    if let Some(index) = made {
        if let Some(records) = semilla_normal::index::records(&index, Some(&handle()?)) {
            return Ok(UnitRead { records, put: None, from_index: true, full: false });
        }
    }
    let stored = state.and_then(|state| {
        db.get(Kind::Index, &index_key(db, file), semilla_normal::index::FORMAT, |payload| {
            let (head, rest) = payload.split_at_checked(24)?;
            let known: Vec<u64> = head.chunks(8).map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap_or_default())).collect();
            (known == state).then(|| semilla_normal::index::decode(rest)).flatten()
        })
    });
    if let Some(index) = stored {
        if let Some(records) = semilla_normal::index::records(&index, Some(&handle()?)) {
            return Ok(UnitRead { records, put: None, from_index: true, full: false });
        }
    }
    let bytes = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
    if let Some((index, records)) = semilla_normal::index::build(&bytes, Some(&handle()?)) {
        let put = state.map(|state| (Kind::Index, index_key(db, file), semilla_normal::index::FORMAT, index_payload(state, &index)));
        return Ok(UnitRead { records, put, from_index: false, full: false });
    }
    let text = String::from_utf8(bytes).with_context(|| format!("cannot read {}", file.display()))?;
    let unit = semilla_normal::unit_settings(&text);
    let mut records = semilla_normal::parse_records(&text).with_context(|| format!("in {}", file.display()))?;
    for record in records.iter_mut() {
        record.unit.clone_from(&unit);
    }
    Ok(UnitRead { records, put: None, from_index: false, full: true })
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

    /// The variable that makes a run of the test binary a child process of a test.
    const CHILD: &str = "SEMILLA_RECSTORE_TEST_CHILD";

    /// The place of a stop in the middle of a write in this process: the child of the stop
    /// test sets it, and the process ends after that entry, with the transaction open.
    pub(super) fn stop_point(at: usize) {
        if std::env::var(CHILD).is_ok_and(|role| role == format!("stop-after-{at}")) {
            std::process::exit(0);
        }
    }

    /// Runs one test of this binary as a child process with a role.
    fn child(test: &str, role: &str, dir: &Path) -> std::process::Child {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads", "1"])
            .env(CHILD, role)
            .env("SEMILLA_RECSTORE_TEST_DIR", dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    fn entries(prefix: &str, count: usize) -> Vec<Put> {
        (0..count).map(|at| (kind_of(at), format!("{prefix} {at}"), "test-format-1", format!("{prefix} payload {at}").repeat(50).into_bytes())).collect()
    }

    /// [R89.1-1] The kind of the test entry with this number: the kinds of the tree entries in
    /// turn, so that the tests of R88.4-2 and R88.4-3 write and read each kind ([R91.1-1] also
    /// the stored results of the items).
    fn kind_of(at: usize) -> Kind {
        [Kind::Syntax, Kind::Opaque, Kind::Item][at % 3]
    }

    fn read(db: &Db, key: &str) -> Option<Vec<u8>> {
        let at: usize = key.rsplit(' ').next().and_then(|number| number.parse().ok()).unwrap_or(0);
        db.get(kind_of(at), key, "test-format-1", |payload| Some(payload.to_vec()))
    }

    // [R88.1-1] The database keeps an entry by its key; another key text or another kind gives
    // no entry.
    // rule+ R88.1-1
    #[test]
    fn the_database_gives_the_entry_of_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        db.put(entries("a", 3)).unwrap();
        assert_eq!(read(&db, "a 1").unwrap(), "a payload 1".repeat(50).into_bytes());
        assert!(database_for(&dir.path().join("shared/x/mir")).is_some_and(|found| Arc::ptr_eq(&found, &db)));
    }

    // rule- R88.1-1
    #[test]
    fn another_key_or_another_kind_gives_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        db.put(entries("a", 1)).unwrap();
        assert!(read(&db, "a 2").is_none());
        assert!(db.get(Kind::Index, "a 0", "test-format-1", |payload| Some(payload.to_vec())).is_none());
        // A folder outside each cache folder has no database.
        assert!(database_for(Path::new("/no/cache/folder")).is_none());
    }

    // [R88.1-1] Only this module opens the database: no other source file names the crate.
    // rule+ R88.1-1
    #[test]
    fn only_the_store_names_the_database_crate() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&src).unwrap().flatten() {
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            if text.contains(concat!("lib", "mdbx::")) && entry.file_name() != "recstore.rs" {
                found.push(entry.file_name());
            }
        }
        assert!(found.is_empty(), "{found:?}");
    }

    // [R88.4-1] An entry with another format key is not read: the caller builds it again.
    // rule+ R88.4-1
    #[test]
    fn an_entry_of_this_format_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        db.put(vec![(Kind::Syntax, "k".to_string(), "test-format-1", b"one".to_vec())]).unwrap();
        assert_eq!(read(&db, "k").as_deref(), Some(&b"one"[..]));
    }

    // rule- R88.4-1
    #[test]
    fn an_entry_of_another_format_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        db.put(vec![(Kind::Syntax, "k".to_string(), "test-format-0", b"old".to_vec())]).unwrap();
        let before = semilla_core::timing::total(DB_PHASE).counts.get("entries of another format").copied().unwrap_or(0);
        assert!(read(&db, "k").is_none());
        assert!(semilla_core::timing::total(DB_PHASE).counts.get("entries of another format").copied().unwrap_or(0) > before);
    }

    /// The child of the stop test: a write of 5 entries that stops after the third one.
    #[test]
    fn child_stop_in_the_middle_of_a_write() {
        let Ok(dir) = std::env::var("SEMILLA_RECSTORE_TEST_DIR") else { return };
        if !std::env::var(CHILD).is_ok_and(|role| role.starts_with("stop-after-")) {
            return;
        }
        let db = open_database(Path::new(&dir)).unwrap();
        let _ = db.put(entries("new", 5));
        // The stop point ends the process before this line.
        std::process::exit(3);
    }

    // [R88.4-2] A write is one transaction: a process that stops in the middle of a write
    // leaves the old state, and the next run reads it and writes again.
    // rule+ R88.4-2
    #[test]
    fn a_stop_in_the_middle_of_a_write_leaves_the_old_state() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::open(dir.path()).unwrap();
            db.put(entries("old", 2)).unwrap();
        }
        let status = child("recstore::tests::child_stop_in_the_middle_of_a_write", "stop-after-2", dir.path()).wait().unwrap();
        assert_eq!(status.code(), Some(0), "the child stops at its stop point");
        let db = open_database(dir.path()).unwrap();
        for at in 0..5 {
            assert!(read(&db, &format!("new {at}")).is_none(), "entry {at} of the stopped write");
        }
        assert!(read(&db, "old 1").is_some());
        db.put(entries("new", 5)).unwrap();
        assert_eq!(read(&db, "new 4").unwrap(), "new payload 4".repeat(50).into_bytes());
    }

    // rule- R88.4-2
    #[test]
    fn a_write_that_completes_has_each_entry() {
        let dir = tempfile::tempdir().unwrap();
        let status = child("recstore::tests::child_stop_in_the_middle_of_a_write", "stop-after-9", dir.path()).wait().unwrap();
        // No stop point: the child completes its write.
        assert_eq!(status.code(), Some(3));
        let db = open_database(dir.path()).unwrap();
        for at in 0..5 {
            assert!(read(&db, &format!("new {at}")).is_some(), "entry {at}");
        }
    }

    /// The child of the test of two processes: it writes its entries and reads each entry of
    /// the other child that is there.
    #[test]
    fn child_of_two_processes() {
        let (Ok(dir), Ok(role)) = (std::env::var("SEMILLA_RECSTORE_TEST_DIR"), std::env::var(CHILD)) else { return };
        let Some(me) = role.strip_prefix("writer-") else { return };
        let db = open_database(Path::new(&dir)).unwrap();
        let other = if me == "a" { "b" } else { "a" };
        for round in 0..40 {
            db.put(entries(&format!("{me} {round}"), 5)).unwrap();
            for at in 0..5 {
                if let Some(found) = read(&db, &format!("{other} {round} {at}")) {
                    if found != format!("{other} {round} payload {at}").repeat(50).into_bytes() {
                        std::process::exit(4);
                    }
                }
            }
        }
        std::process::exit(0);
    }

    // [R88.4-3] Two processes on one database: each one writes and reads, and each entry is
    // whole after the two.
    // rule+ R88.4-3
    #[test]
    fn two_processes_write_and_read_one_database() {
        let dir = tempfile::tempdir().unwrap();
        let mut children = vec![child("recstore::tests::child_of_two_processes", "writer-a", dir.path()), child("recstore::tests::child_of_two_processes", "writer-b", dir.path())];
        for child in &mut children {
            assert_eq!(child.wait().unwrap().code(), Some(0));
        }
        let db = open_database(dir.path()).unwrap();
        for me in ["a", "b"] {
            for round in 0..40 {
                for at in 0..5 {
                    assert_eq!(read(&db, &format!("{me} {round} {at}")).unwrap(), format!("{me} {round} payload {at}").repeat(50).into_bytes());
                }
            }
        }
    }

    // rule- R88.4-3
    #[test]
    fn a_process_that_opens_the_database_again_gets_the_one_database() {
        // In one process a database opens one time: a second open gives the same value, so
        // two handles of one process never use one file in two ways.
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (open_database(dir.path()).unwrap(), open_database(dir.path()).unwrap());
        assert!(Arc::ptr_eq(&one, &two));
    }

    fn indexed_folder(dir: &Path, text: &str) -> PathBuf {
        let folder = dir.join("shared/k/mir/tree/n0-s1");
        let store = FileStore::default();
        store.create(&folder).unwrap();
        std::fs::write(folder.join("k.lib.1.jsonl"), text).unwrap();
        store.complete(&folder).unwrap();
        folder
    }

    const RECORDS: &str = "{\"unit\":{\"target\":\"k.lib.1\",\"cfg\":[\"unix\"],\"files\":[]}}\n{\"name\":\"k::f\",\"level\":\"n0\",\"text\":\"fn f() {}\",\"q\":\"Q\"}\n{\"name\":\"k::g\",\"level\":\"n0\",\"text\":\"fn g() {}\"}\n";

    // [R88.2-2] The build writes the index when it completes, and a read of the folder takes
    // the records from the index: they are the records of the full read.
    // rule+ R88.2-2
    #[test]
    fn a_complete_build_has_its_index_and_the_read_uses_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let folder = indexed_folder(dir.path(), RECORDS);
        let key = FileStore::default().key(&folder).unwrap();
        let file = folder.join("k.lib.1.jsonl");
        let stored = db.get(Kind::Index, &index_key(&db, &file), semilla_normal::index::FORMAT, |payload| Some(payload.len()));
        assert!(stored.is_some(), "the index of the build");
        let read = read_unit(&db, &file, Some(&key)).unwrap();
        assert!(read.from_index && read.put.is_none());
        assert_eq!(read.records, semilla_normal::read_records(&folder).unwrap());
        assert!(read.records.iter().all(|record| record.known.from_index().is_some_and(|facts| facts.text == semilla_normal::index::digest(&record.text))));
        assert_eq!(FileStore::default().records(&folder).unwrap()[1].name, "k::g");
    }

    // rule- R88.2-2
    #[test]
    fn a_folder_that_a_build_makes_again_gets_a_new_index() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let folder = indexed_folder(dir.path(), RECORDS);
        // A new build of the folder: new records, a new time of `.complete`.
        let file = folder.join("k.lib.1.jsonl");
        std::fs::write(&file, RECORDS.replace("fn g() {}", "fn g() { 1 }")).unwrap();
        std::fs::File::options().write(true).open(folder.join(COMPLETE)).unwrap().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(5)).unwrap();
        let key = FileStore::default().key(&folder).unwrap();
        let read = read_unit(&db, &file, Some(&key)).unwrap();
        assert!(!read.from_index && read.put.is_some(), "the stored index is of the earlier build");
        assert_eq!(read.records[1].text, "fn g() { 1 }");
        // A folder of BASE has no index: the first read makes it.
        let base = dir.path().join("shared/k/mir/tree/n0-s2");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("k.lib.1.jsonl"), RECORDS).unwrap();
        std::fs::write(base.join(COMPLETE), "").unwrap();
        let key = FileStore::default().key(&base).unwrap();
        let first = read_unit(&db, &base.join("k.lib.1.jsonl"), Some(&key)).unwrap();
        assert!(!first.from_index && first.put.is_some());
    }

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

    /// The unit line of a folder with a list of the expanded items, and one site.
    const MADE_UNIT: &str = "{\"unit\":{\"target\":\"k.lib.1\",\"cfg\":[\"unix\"],\"files\":[],\"made\":[{\"file\":\"src/lib.rs\",\"line\":1,\"col\":0,\"text\":\"fn f() {}\"}]}}";

    /// [R89.3-2] The store reads the expanded items of a folder one time in a process: a second
    /// read gives the value in memory, also after a change of the file with no new build.
    // rule+ R89.3-2
    #[test]
    fn the_expanded_items_of_a_folder_are_read_one_time() {
        let store = FileStore::default();
        let dir = folder(MADE_UNIT, 1_000);
        let first = store.made(dir.path()).unwrap();
        assert_eq!(first.items.len(), 1);
        let mut full = semilla_normal::Made::default();
        assert!(semilla_normal::read_made(dir.path(), &mut full));
        assert_eq!(*first, full);
        std::fs::write(dir.path().join("k.lib.1.jsonl"), "{\"unit\":{\"target\":\"k.lib.1\"}}\n").unwrap();
        assert!(Arc::ptr_eq(&store.made(dir.path()).unwrap(), &first));
    }

    /// [R89.3-2] The negative case: a build that makes the folder again (a new time of
    /// `.complete`) gives a new read, and a folder with no list of the items has no items.
    // rule- R89.3-2
    #[test]
    fn a_folder_that_a_build_makes_again_has_its_new_expanded_items() {
        let store = FileStore::default();
        let dir = folder(MADE_UNIT, 1_000);
        assert!(store.made(dir.path()).is_some());
        std::fs::write(dir.path().join("k.lib.1.jsonl"), "{\"unit\":{\"target\":\"k.lib.1\"}}\n").unwrap();
        std::fs::File::options().write(true).open(dir.path().join(COMPLETE)).unwrap().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(2_000)).unwrap();
        assert!(store.made(dir.path()).is_none());
    }

    /// The count of the index entries of the database whose key text starts with a text.
    fn index_entries(db: &Db, start: &str) -> usize {
        let txn = db.env.begin_ro_txn().unwrap();
        let table = txn.open_table(None).unwrap();
        let mut cursor = txn.cursor(&table).unwrap();
        cursor
            .iter_from::<std::borrow::Cow<'_, [u8]>, std::borrow::Cow<'_, [u8]>>(&[Kind::Index.byte()])
            .map(|entry| entry.unwrap())
            .take_while(|(key, _)| key.first() == Some(&Kind::Index.byte()))
            .filter(|(_, value)| split_value(value).is_some_and(|((_, text), _)| text.starts_with(start.as_bytes())))
            .count()
    }

    /// [R89.4-1] The remove of a folder removes the index entries of its files in one write
    /// transaction; the entries of a different folder stay.
    // rule+ R89.4-1
    #[test]
    fn a_removed_folder_has_no_entries_in_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let folder = indexed_folder(dir.path(), RECORDS);
        let other = dir.path().join("shared/k/mir/tree/n0-s1x");
        let store = FileStore::default();
        store.create(&other).unwrap();
        std::fs::write(other.join("k.lib.1.jsonl"), RECORDS).unwrap();
        store.complete(&other).unwrap();
        assert_eq!(index_entries(&db, "index shared/k/mir/tree/n0-s1/"), 1);
        store.remove(&folder);
        assert_eq!(index_entries(&db, "index shared/k/mir/tree/n0-s1/"), 0);
        assert_eq!(index_entries(&db, "index shared/k/mir/tree/n0-s1x/"), 1, "the entry of the other folder stays");
    }

    /// The child of the stop test of a remove: it stops after the first removed entry.
    #[test]
    fn child_stop_in_the_middle_of_a_remove() {
        let Ok(dir) = std::env::var("SEMILLA_RECSTORE_TEST_DIR") else { return };
        if !std::env::var(CHILD).is_ok_and(|role| role.starts_with("stop-after-")) {
            return;
        }
        let db = open_database(Path::new(&dir)).unwrap();
        let _ = db.remove_where(Kind::Index, &|_| true);
        std::process::exit(3);
    }

    /// [R89.4-1] The negative case: a process that stops in the middle of a remove leaves
    /// each entry (one transaction), and a remove of a folder with no entry removes nothing.
    // rule- R89.4-1
    #[test]
    fn a_stop_in_the_middle_of_a_remove_leaves_each_entry() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::open(dir.path()).unwrap();
            let puts = (0..3).map(|at| (Kind::Index, format!("index shared/x/{at}.jsonl"), "test-format-1", b"p".to_vec())).collect();
            db.put(puts).unwrap();
            assert_eq!(db.remove_where(Kind::Index, &|key| key.starts_with("index shared/y/")).unwrap(), 0);
        }
        let status = child("recstore::tests::child_stop_in_the_middle_of_a_remove", "stop-after-0", dir.path()).wait().unwrap();
        assert_eq!(status.code(), Some(0), "the child stops at its stop point");
        let db = open_database(dir.path()).unwrap();
        assert_eq!(index_entries(&db, "index shared/x/"), 3);
    }

    /// [R89.4-2] `semilla cache --clean` removes the index of a record file that is not there
    /// and the tree entries of another Semilla binary.
    // rule+ R89.4-2
    #[test]
    fn the_clean_removes_each_entry_that_no_folder_can_use() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let folder = indexed_folder(dir.path(), RECORDS);
        db.put(vec![(Kind::Opaque, "opaque 00 other-binary".to_string(), OPAQUE_FORMAT, b"x".to_vec()), (Kind::Syntax, "syntax t m other-binary".to_string(), "f", b"x".to_vec())]).unwrap();
        // The folder goes with no call of the store (as the clean of the build code cache).
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(clean_database(dir.path(), &Live::default()).unwrap(), 3);
        assert_eq!(index_entries(&db, "index "), 0);
        assert!(db.get(Kind::Opaque, "opaque 00 other-binary", OPAQUE_FORMAT, |_| Some(())).is_none());
    }

    /// [R89.4-2] The negative case: the clean keeps the index of each record file that is there
    /// and each tree entry of this binary.
    // rule- R89.4-2
    #[test]
    fn the_clean_keeps_each_entry_that_a_folder_can_use() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let _folder = indexed_folder(dir.path(), RECORDS);
        // A construction of this binary for the pair of a stored step, with its slot (the key
        // of R89.1-1; the test of batch 14g had no slot, and the clean of 14g removed each real
        // construction).
        let own = format!("opaque 00 {} a:b 0", crate::store::binary_identity());
        db.put(vec![(Kind::Opaque, own.clone(), OPAQUE_FORMAT, b"x".to_vec())]).unwrap();
        let live = Live { pairs: ["a:b".to_string()].into_iter().collect(), texts: Default::default(), bodies: Default::default() };
        assert_eq!(clean_database(dir.path(), &live).unwrap(), 0);
        assert_eq!(index_entries(&db, "index "), 1);
        assert!(db.get(Kind::Opaque, &own, OPAQUE_FORMAT, |_| Some(())).is_some());
    }

    /// The key text of the stored part of a file with this text (R91.2-1), and a database with
    /// the entry.
    fn file_entry(db: &Db, text: &str) -> String {
        let key = FileSyntaxDb::key("src/lib.rs\ndepot\nfalse\nsrc\nEdition2021\ndepot", text);
        db.put(vec![(Kind::Item, key.clone(), semilla_syntax::stored::FILE_FORMAT, b"x".to_vec())]).unwrap();
        key
    }

    /// [R91.4-1] `semilla cache --clean` removes each stored result of a pair of trees that no
    /// stored step has (a construction of the opaque inputs), and the stored part of a file
    /// whose content no tree of a stored step has. An entry of another binary goes too.
    // rule+ R91.4-1
    #[test]
    fn the_clean_removes_the_results_of_a_pair_that_no_stored_step_has() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let identity = crate::store::binary_identity();
        let gone = format!("opaque 00 {identity} old:new 0");
        let other = format!("opaque 00 other-binary a:b 0");
        db.put(vec![(Kind::Opaque, gone.clone(), OPAQUE_FORMAT, b"x".to_vec()), (Kind::Opaque, other.clone(), OPAQUE_FORMAT, b"x".to_vec())]).unwrap();
        let file = file_entry(&db, "pub fn gone() {}\n");
        let live = Live { pairs: ["a:b".to_string()].into_iter().collect(), texts: [hex_digest(b"pub fn kept() {}\n")].into_iter().collect(), bodies: Default::default() };
        assert_eq!(clean_database(dir.path(), &live).unwrap(), 3);
        for (kind, key, format) in [(Kind::Opaque, &gone, OPAQUE_FORMAT), (Kind::Opaque, &other, OPAQUE_FORMAT), (Kind::Item, &file, semilla_syntax::stored::FILE_FORMAT)] {
            assert!(db.get(kind, key, format, |_| Some(())).is_none(), "{key}");
        }
    }

    /// [R91.4-1] The negative case: the clean keeps a construction of the pair of a stored step,
    /// and the stored part of a file whose content a tree of a stored step has.
    // rule- R91.4-1
    #[test]
    fn the_clean_keeps_the_results_that_a_stored_step_can_use() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let kept = format!("opaque 00 {} a:b 3", crate::store::binary_identity());
        db.put(vec![(Kind::Opaque, kept.clone(), OPAQUE_FORMAT, b"x".to_vec())]).unwrap();
        let file = file_entry(&db, "pub fn kept() {}\n");
        let live = Live { pairs: ["a:b".to_string()].into_iter().collect(), texts: [hex_digest(b"pub fn kept() {}\n")].into_iter().collect(), bodies: Default::default() };
        assert_eq!(clean_database(dir.path(), &live).unwrap(), 0);
        assert!(db.get(Kind::Opaque, &kept, OPAQUE_FORMAT, |_| Some(())).is_some());
        assert!(db.get(Kind::Item, &file, semilla_syntax::stored::FILE_FORMAT, |_| Some(())).is_some());
    }

    /// [R89.1-1] The key of a stored construction has the digest of each input and the
    /// identity of the binary: another input (one byte, or a byte that moves to the next
    /// input) gives another key, and an entry of another format is not read.
    // rule+ R89.1-1
    #[test]
    fn the_key_of_a_phase_result_has_each_input_and_the_binary() {
        use semilla_verify::PhaseStore as _;
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let store = PhaseDb { db: db.clone(), pair: "a:b".to_string() };
        let key = store.key(semilla_verify::PhaseKind::Opaque, &[b"ab", b"c"]);
        assert!(key.starts_with("opaque ") && key.ends_with(&format!(" {} a:b", crate::store::binary_identity())));
        // [R91.4-1] Another pair of trees gives another key.
        assert_ne!(key, PhaseDb { db: db.clone(), pair: "a:c".to_string() }.key(semilla_verify::PhaseKind::Opaque, &[b"ab", b"c"]));
        assert_ne!(key, store.key(semilla_verify::PhaseKind::Opaque, &[b"a", b"bc"]));
        assert_ne!(key, store.key(semilla_verify::PhaseKind::Opaque, &[b"ab", b"d"]));
        store.put(semilla_verify::PhaseKind::Opaque, &key, b"payload".to_vec());
        assert_eq!(store.get(semilla_verify::PhaseKind::Opaque, &key).as_deref(), Some(&b"payload"[..]));
        db.put(vec![(Kind::Opaque, key.clone(), "semilla-opaque-0", b"old".to_vec())]).unwrap();
        assert!(store.get(semilla_verify::PhaseKind::Opaque, &key).is_none());
    }

    /// [R91.1-1] The key of the stored result of one item (here the part of one file, R91.2-1)
    /// has the name of the phase, the digest of each input (the text and the context), and the
    /// identity of the binary; the entry is read with its format key.
    // rule+ R91.1-1
    #[test]
    fn the_key_of_a_file_part_has_the_text_the_context_and_the_binary() {
        use semilla_syntax::load::FileSyntaxStore as _;
        let dir = tempfile::tempdir().unwrap();
        let _db = open_database(dir.path()).unwrap();
        let store = FileSyntaxDb::of(dir.path()).unwrap();
        let key = FileSyntaxDb::key("context", "text");
        let words: Vec<&str> = key.split(' ').collect();
        assert_eq!(words, [FILE_PHASE, &hex_digest(b"text"), &hex_digest(b"context"), &crate::store::binary_identity()]);
        store.put("context", "text", b"part".to_vec());
        assert_eq!(store.get("context", "text"), None, "a part is written at the end of the load");
        store.finish();
        let store = FileSyntaxDb::of(dir.path()).unwrap();
        assert_eq!(store.get("context", "text").as_deref(), Some(&b"part"[..]));
    }

    /// [R91.1-1] The negative case: another text, another context, or another format gives no
    /// stored result.
    // rule- R91.1-1
    #[test]
    fn another_text_another_context_or_another_format_gives_no_stored_result() {
        use semilla_syntax::load::FileSyntaxStore as _;
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let store = FileSyntaxDb::of(dir.path()).unwrap();
        store.put("context", "text", b"part".to_vec());
        store.finish();
        let store = FileSyntaxDb::of(dir.path()).unwrap();
        assert_eq!(store.get("context", "text 2"), None);
        assert_eq!(store.get("context 2", "text"), None);
        assert_ne!(FileSyntaxDb::key("a", "bc"), FileSyntaxDb::key("ab", "c"));
        db.put(vec![(Kind::Item, FileSyntaxDb::key("context", "text"), "semilla-file-syntax-0", b"old".to_vec())]).unwrap();
        assert_eq!(store.get("context", "text"), None, "an entry of another format is not read");
    }

    /// The resident pages of the files that the process maps, in kB.
    fn file_pages() -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        status.lines().find_map(|line| line.strip_prefix("RssFile:")).and_then(|rest| rest.trim().trim_end_matches(" kB").trim().parse().ok()).unwrap_or(0)
    }

    /// The two tests of the resident pages run one at a time: the measure is of the process.
    static PAGES: Mutex<()> = Mutex::new(());

    /// A database with one entry of 48 MB, and the sum of its bytes.
    fn large_entry(dir: &Path) -> (Arc<Db>, u64) {
        let db = open_database(dir).unwrap();
        let payload: Vec<u8> = (0..48u32 << 20).map(|at| (at % 251) as u8).collect();
        let sum = payload.iter().map(|byte| u64::from(*byte)).sum();
        db.put(vec![(Kind::Syntax, "large".to_string(), "test-format-1", payload)]).unwrap();
        (db, sum)
    }

    /// [R89.7-1] A read of a value gives its pages back after the read: the resident pages of
    /// the map do not grow by the size of the value, and a second read gives the same bytes.
    // rule+ R89.7-1
    #[test]
    fn a_read_of_the_database_keeps_no_pages_of_the_value() {
        let _one = PAGES.lock().unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db, sum) = large_entry(dir.path());
        let before = file_pages();
        let read = || db.get(Kind::Syntax, "large", "test-format-1", |payload| Some(payload.iter().map(|byte| u64::from(*byte)).sum::<u64>()));
        assert_eq!(read(), Some(sum));
        let grown = file_pages().saturating_sub(before);
        assert!(grown < 16 << 10, "the map keeps {grown} kB");
        assert_eq!(read(), Some(sum));
    }

    /// [R89.7-1] The negative case: a read of the map with no release keeps the pages of the
    /// value (the measure of the positive case sees them).
    // rule- R89.7-1
    #[test]
    fn a_read_with_no_release_keeps_the_pages_of_the_value() {
        let _one = PAGES.lock().unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db, sum) = large_entry(dir.path());
        let before = file_pages();
        let txn = db.env.begin_ro_txn().unwrap();
        let table = txn.open_table(None).unwrap();
        let value: std::borrow::Cow<'_, [u8]> = txn.get(&table, Db::key(Kind::Syntax, "large")).unwrap().unwrap();
        let (_, payload) = split_value(&value).unwrap();
        assert_eq!(payload.iter().map(|byte| u64::from(*byte)).sum::<u64>(), sum);
        let grown = file_pages().saturating_sub(before);
        assert!(grown > 32 << 10, "the map keeps only {grown} kB");
    }

    /// [R99.8-1] The payload of a stored record with no place: the empty item list, then the
    /// record.
    fn stored_payload(record: &str) -> Vec<u8> {
        format!("{{\"a\":[],\"m\":[]}}\n{record}").into_bytes()
    }

    /// A record file of a build with the keys on: the unit line, a pointer line, a new record
    /// with a key, a record with no key, and the keys line.
    fn keyed_file(dir: &Path, pointer: &str) -> PathBuf {
        let file = dir.join("k.lib.1.jsonl");
        let lines = [
            r#"{"unit":{"target":"k.lib.1","cfg":[],"files":[]}}"#.to_string(),
            format!(r#"{{"{POINTER_MEMBER}":"{pointer}"}}"#),
            r#"{"name":"k::new","level":"n0","text":"fn new() {}"}"#.to_string(),
            r#"{"name":"k::plain","level":"n0","text":"fn plain() {}"}"#.to_string(),
            format!(r#"{{"{KEYS_MEMBER}":["{pointer}","00ff",null]}}"#),
        ];
        std::fs::write(&file, lines.join("\n") + "\n").unwrap();
        file
    }

    // [R93.3-1] A pointer line of the driver gets the stored record of its key, the keys line
    // goes, each new record with a key goes into the store, and the keys of the lines are the
    // pointer list of the file.
    // rule+ R93.3-2
    #[test]
    fn a_pointer_line_gets_the_stored_record_of_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let stored = r#"{"name":"k::old","level":"n0","text":"fn old() {}"}"#;
        db.put(vec![(Kind::Body, format!("{BODY_PHASE} aa11"), BODY_FORMAT, stored_payload(stored))]).unwrap();
        let file = keyed_file(dir.path(), "aa11");
        let keys = dir.path().join("keys/k.lib.1.jsonl");
        resolve_bodies(&db, &file, &keys).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert_eq!(lines[1], stored);
        assert!(lines[2].contains("k::new") && lines[3].contains("k::plain"), "{text}");
        assert!(db.get(Kind::Body, &format!("{BODY_PHASE} 00ff"), BODY_FORMAT, |payload| Some(payload.to_vec())).is_some_and(|payload| payload == stored_payload(lines[2])));
        assert_eq!(std::fs::read_to_string(&keys).unwrap(), "aa11\n00ff\n-\n");
        // A file with no keys line does not change.
        resolve_bodies(&db, &file, &keys).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    }

    // [R96.2-1] The task of a unit at its message resolves the pointer lines of its file and
    // makes its index; the copy of the file in a cache folder gets that index.
    // rule+ R96.2-1
    #[test]
    fn a_unit_task_resolves_its_file_and_its_index_goes_to_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let stored = r#"{"name":"k::old","level":"n0","text":"fn old() {}"}"#;
        db.put(vec![(Kind::Body, format!("{BODY_PHASE} aa11"), BODY_FORMAT, stored_payload(stored))]).unwrap();
        let file = keyed_file(dir.path(), "aa11");
        let keys = dir.path().join("keys/k.lib.1.jsonl");
        unit_task(&db, &file, &keys).unwrap();
        assert!(resolved_by_task(&file));
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().nth(1), Some(stored));
        let copy = dir.path().join("cache/k.lib.1.jsonl");
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::copy(&file, &copy).unwrap();
        index_for_copy(&file, &copy);
        let size = std::fs::metadata(&copy).unwrap().len();
        let index = take_copy_index(&copy, size).expect("the index of the task").expect("a plain file");
        let fresh = semilla_normal::index::build(&std::fs::read(&copy).unwrap(), None).map(|(index, _)| index).unwrap();
        assert_eq!(semilla_normal::index::encode(&index), semilla_normal::index::encode(&fresh));
    }

    // [R96.2-1] The negative case: a file that changes after its task is not resolved by the
    // task, and its copy gets no index of the task (the end of the build does the work again).
    // rule- R96.2-1
    #[test]
    fn a_file_that_changes_after_its_unit_task_gets_no_result_of_the_task() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let stored = r#"{"name":"k::old","level":"n0","text":"fn old() {}"}"#;
        db.put(vec![(Kind::Body, format!("{BODY_PHASE} bb22"), BODY_FORMAT, stored_payload(stored))]).unwrap();
        let file = keyed_file(dir.path(), "bb22");
        let keys = dir.path().join("keys/k.lib.1.jsonl");
        unit_task(&db, &file, &keys).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{text}{{\"name\":\"k::more\",\"level\":\"n0\",\"text\":\"fn more() {{}}\"}}\n")).unwrap();
        assert!(!resolved_by_task(&file));
        let copy = dir.path().join("cache/k.lib.1.jsonl");
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::copy(&file, &copy).unwrap();
        index_for_copy(&file, &copy);
        assert!(take_copy_index(&copy, std::fs::metadata(&copy).unwrap().len()).is_none());
        // A file with no task has no result of a task.
        assert!(!resolved_by_task(&dir.path().join("other.jsonl")));
    }

    /// A complete record folder with one plain record file, and its key.
    fn complete_folder(dir: &Path) -> (PathBuf, PathBuf, FolderKey) {
        let folder = dir.join("mir/tree/n0");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("k.lib.1.jsonl");
        let lines = [r#"{"unit":{"target":"k.lib.1","cfg":[],"files":[]}}"#, r#"{"name":"k::a","level":"n0","text":"fn a() {}"}"#, r#"{"name":"k::b","level":"n0","text":"fn b() {}"}"#];
        std::fs::write(&file, lines.join("\n") + "\n").unwrap();
        std::fs::write(folder.join(COMPLETE), "").unwrap();
        let time = std::fs::metadata(folder.join(COMPLETE)).unwrap().modified().unwrap();
        (folder.clone(), file, (folder, time))
    }

    // [R96.9-2] The index of a folder that this process completed comes from memory: the read
    // needs no entry of the database (the write runs in its own thread).
    // rule+ R96.9-2
    #[test]
    fn a_complete_folder_gives_its_index_to_the_reads_of_this_process() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let (folder, file, key) = complete_folder(dir.path());
        index_folder(&db, &folder, &key);
        // With no entry in the database, the read takes the index from memory.
        db.remove_where(Kind::Index, &|_| true).unwrap();
        let read = read_unit(&db, &file, Some(&key)).unwrap();
        assert!(read.from_index && read.put.is_none(), "the read made a new index");
        assert_eq!(read.records.iter().map(|record| record.name.as_str()).collect::<Vec<_>>(), ["k::a", "k::b"]);
    }

    // [R96.9-2] The negative case: a file that changed after the index takes a new index.
    // rule- R96.9-2
    #[test]
    fn a_file_that_changed_after_its_index_is_indexed_again() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let (folder, file, key) = complete_folder(dir.path());
        index_folder(&db, &folder, &key);
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{text}{{\"name\":\"k::c\",\"level\":\"n0\",\"text\":\"fn c() {{}}\"}}\n")).unwrap();
        let read = read_unit(&db, &file, Some(&key)).unwrap();
        assert!(!read.from_index, "an index of another state of the file");
        assert_eq!(read.records.len(), 3);
    }

    // [R93.3-1] The negative case: a pointer whose record the store does not have is an error
    // (the build runs again with no keys), and the file does not change.
    // rule- R93.3-2
    #[test]
    fn a_pointer_with_no_stored_record_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let file = keyed_file(dir.path(), "bb22");
        let before = std::fs::read_to_string(&file).unwrap();
        let error = resolve_bodies(&db, &file, &dir.path().join("keys/k")).unwrap_err();
        assert!(error.downcast_ref::<MissingBody>().is_some(), "{error:#}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    }

    // [R93.4-1] `cache --clean` removes each stored step result of a pair that no stored step
    // has (or of another binary), and each body record that no pointer list of a tree of a
    // stored step has.
    // rule+ R93.4-1
    #[test]
    fn the_clean_removes_the_step_results_and_the_body_records_that_no_stored_step_uses() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let identity = crate::store::binary_identity();
        let steps = [format!("{STEP_PHASE} old:new 00 {identity}"), format!("{STEP_PHASE} a:b 00 other-binary")];
        for key in &steps {
            db.put(vec![(Kind::Step, key.clone(), STEP_FORMAT, b"{}".to_vec())]).unwrap();
        }
        db.put(vec![(Kind::Body, format!("{BODY_PHASE} dead"), BODY_FORMAT, b"{}".to_vec())]).unwrap();
        let live = Live { pairs: ["a:b".to_string()].into_iter().collect(), bodies: ["kept".to_string()].into_iter().collect(), ..Live::default() };
        assert_eq!(clean_database(dir.path(), &live).unwrap(), 3);
        for key in &steps {
            assert!(db.get(Kind::Step, key, STEP_FORMAT, |_| Some(())).is_none(), "{key}");
        }
        assert!(db.get(Kind::Body, &format!("{BODY_PHASE} dead"), BODY_FORMAT, |_| Some(())).is_none());
    }

    // rule- R93.4-1
    #[test]
    fn the_clean_keeps_the_step_results_and_the_body_records_of_the_stored_steps() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let step = step_key("a", "b", "inputs");
        db.put(vec![(Kind::Step, step.clone(), STEP_FORMAT, b"{}".to_vec()), (Kind::Body, format!("{BODY_PHASE} kept"), BODY_FORMAT, b"{}".to_vec())]).unwrap();
        let live = Live { pairs: ["a:b".to_string()].into_iter().collect(), bodies: ["kept".to_string()].into_iter().collect(), ..Live::default() };
        assert_eq!(clean_database(dir.path(), &live).unwrap(), 0);
        assert!(db.get(Kind::Step, &step, STEP_FORMAT, |_| Some(())).is_some());
        assert!(db.get(Kind::Body, &format!("{BODY_PHASE} kept"), BODY_FORMAT, |_| Some(())).is_some());
    }

    // [R93.1-1] The key text of a stored step result has the pair of trees, the digest of the
    // inputs, and the binary: other inputs give another key text, and a read with another key
    // text gives no result.
    // rule+ R93.1-3
    #[test]
    fn a_stored_step_result_is_read_with_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let key = step_key("a", "b", "inputs");
        assert!(key.starts_with(&format!("{STEP_PHASE} a:b ")) && key.ends_with(&crate::store::binary_identity()), "{key}");
        db.put(vec![(Kind::Step, key.clone(), STEP_FORMAT, b"{}".to_vec())]).unwrap();
        assert!(db.get(Kind::Step, &key, STEP_FORMAT, |_| Some(())).is_some());
    }

    // rule- R93.1-3
    #[test]
    fn another_input_or_format_gives_no_stored_step_result() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_database(dir.path()).unwrap();
        let key = step_key("a", "b", "inputs");
        db.put(vec![(Kind::Step, key.clone(), STEP_FORMAT, b"{}".to_vec())]).unwrap();
        for other in [step_key("a", "c", "inputs"), step_key("a", "b", "inputs 2")] {
            assert_ne!(other, key);
            assert!(db.get(Kind::Step, &other, STEP_FORMAT, |_| Some(())).is_none(), "{other}");
        }
        assert!(db.get(Kind::Step, &key, "semilla-step-0", |_| Some(())).is_none());
    }

    /// The child of the test of the steady point: it opens a database, waits, and ends with the
    /// count of its steady points as its exit code.
    #[test]
    fn child_of_the_steady_point() {
        let Ok(dir) = std::env::var("SEMILLA_RECSTORE_TEST_DIR") else { return };
        if !std::env::var(CHILD).is_ok_and(|role| role == "steady") {
            return;
        }
        let _db = open_database(Path::new(&dir)).unwrap();
        let mut count = 0;
        for _ in 0..100 {
            count = semilla_core::timing::total(SYNC_PHASE).counts.get("steady points").copied().unwrap_or(0);
            if count > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::process::exit(count as i32);
    }

    fn steady_points(no_sync: bool) -> i32 {
        let dir = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "recstore::tests::child_of_the_steady_point", "--nocapture", "--test-threads", "1"]).env(CHILD, "steady").env("SEMILLA_RECSTORE_TEST_DIR", dir.path());
        command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        if no_sync {
            command.env(NO_BACKGROUND_SYNC, "1");
        } else {
            command.env_remove(NO_BACKGROUND_SYNC);
        }
        command.status().unwrap().code().unwrap()
    }

    // [R93.2-1] A process that opens the database makes a steady point in a thread of its own:
    // the write of the command does not wait for the sync of the data of earlier commands.
    // rule+ R93.2-1
    #[test]
    fn a_process_makes_a_steady_point_in_a_thread_of_its_own() {
        assert_eq!(steady_points(false), 1);
    }

    // rule- R93.2-1
    #[test]
    fn with_no_background_sync_the_process_makes_no_steady_point() {
        assert_eq!(steady_points(true), 0);
    }

    // [R93.7-1] The parts of the files of a load wait for a write only up to a limit of bytes:
    // a larger batch goes into the store at once, so a cold load does not hold each part.
    // rule+ R93.7-1
    #[test]
    fn a_large_batch_of_file_parts_goes_into_the_store_at_once() {
        use semilla_syntax::load::FileSyntaxStore as _;
        let dir = tempfile::tempdir().unwrap();
        let _db = open_database(dir.path()).unwrap();
        let store = FileSyntaxDb::of(dir.path()).unwrap();
        store.put("context", "fn a() {}", vec![7; MOST_WAITING + 1]);
        assert!(store.get("context", "fn a() {}").is_some());
        assert!(store.puts.borrow().is_empty());
    }

    // rule- R93.7-1
    #[test]
    fn a_small_part_waits_for_the_end_of_the_load() {
        use semilla_syntax::load::FileSyntaxStore as _;
        let dir = tempfile::tempdir().unwrap();
        let _db = open_database(dir.path()).unwrap();
        let store = FileSyntaxDb::of(dir.path()).unwrap();
        store.put("context", "fn b() {}", vec![7; 16]);
        assert!(store.get("context", "fn b() {}").is_none());
        store.finish();
        let again = FileSyntaxDb::of(dir.path()).unwrap();
        assert!(again.get("context", "fn b() {}").is_some());
    }
}
