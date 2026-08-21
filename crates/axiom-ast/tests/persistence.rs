//! What the index promises across a process boundary.
//!
//! The gap these close: a build shipped that answered every symbol query with
//! `total_symbols_in_index: 2`, because nothing checked that a scan survives
//! being written and read back. Each test here fails if that regresses.
//!
//! Every test works in absolute paths under its own directory. `save_to_disk`
//! resolves a relative path against the process working directory, so a test
//! that changed the working directory would race every other test in this
//! binary.

use axiom_ast::{write_atomically, AstIndex, IndexLock};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A Java class with one field and two methods, so a scan has both a class and
/// a method symbol to record.
const COUNTER_JAVA: &str = "package p;\npublic class Counter {\n    private int n;\n    public void increment() { n++; }\n    public int value() { return n; }\n}\n";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("axiom-ast-{}-{}-{}", tag, std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test directory");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write(&self, name: &str, body: &str) -> PathBuf {
        let file = self.join(name);
        std::fs::write(&file, body).expect("write the fixture file");
        file
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn scan_records_a_class_and_its_methods() {
    let dir = TempDir::new("scan");
    dir.write("Counter.java", COUNTER_JAVA);

    let index = AstIndex::new();
    let summary = index
        .scan_directory(dir.path())
        .expect("scan the directory");

    assert_eq!(summary.files_scanned, 1, "one .java file to read");
    assert!(
        summary.nodes_indexed > 0,
        "a class with two methods must yield symbols, got {}",
        summary.nodes_indexed
    );
    assert!(
        index.get_symbol("p.Counter").is_some(),
        "the class symbol is missing; recorded: {:?}",
        recorded(&index)
    );
    assert!(
        index.get_symbol("p.Counter::increment").is_some(),
        "the method symbol is missing; recorded: {:?}",
        recorded(&index)
    );
}

/// The regression test for the defect that started this: an index that scans
/// correctly but does not survive being written and read back is worthless to
/// the server, which is a separate process from the scan.
#[test]
fn an_index_survives_a_round_trip_through_disk() {
    let dir = TempDir::new("roundtrip");
    dir.write("Counter.java", COUNTER_JAVA);
    let index_file = dir.join("index.json");

    let written = AstIndex::new();
    written
        .scan_directory(dir.path())
        .expect("scan the directory");
    let before = written.total_symbols_count();
    assert!(
        before > 0,
        "nothing was indexed, so the test proves nothing"
    );

    let saved = written.save_to_disk(&index_file).expect("save the index");
    assert!(
        saved.exists(),
        "save_to_disk reported a path it did not write"
    );

    let read = AstIndex::load_from_disk(&index_file).expect("load the index");
    assert_eq!(
        read.total_symbols_count(),
        before,
        "the reader saw a different number of symbols than the writer wrote"
    );
    assert!(
        read.get_symbol("p.Counter::increment").is_some(),
        "a symbol present before the write is absent after the read"
    );
}

/// A scan states what the tree holds now. A file that has gone must not keep
/// answering, or the blast radius names tests that no longer exist.
#[test]
fn a_rescan_drops_a_file_that_has_gone() {
    let dir = TempDir::new("forget");
    dir.write("Counter.java", COUNTER_JAVA);
    let doomed = dir.write(
        "Doomed.java",
        "package p;\npublic class Doomed {\n    public void gone() {}\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("first scan");
    assert!(
        index.get_symbol("p.Doomed::gone").is_some(),
        "the fixture was not indexed, so the removal proves nothing"
    );

    std::fs::remove_file(&doomed).expect("remove the fixture");
    index.scan_directory(dir.path()).expect("second scan");

    assert!(
        index.get_symbol("p.Doomed::gone").is_none(),
        "a deleted file still answers after a rescan"
    );
    assert!(
        index.get_symbol("p.Counter::increment").is_some(),
        "the rescan dropped a file that is still on disk"
    );
}

/// A reader must never see half a file, and a finished write must not leave its
/// temporary behind for the next scan to trip over.
#[test]
fn an_atomic_write_replaces_the_file_and_leaves_nothing_behind() {
    let dir = TempDir::new("atomic");
    let target = dir.join("payload.json");

    write_atomically(&target, b"first").expect("first write");
    assert_eq!(std::fs::read(&target).unwrap(), b"first");

    write_atomically(&target, b"second").expect("second write");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"second",
        "the second write did not replace the first"
    );

    let left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        vec!["payload.json".to_string()],
        "the write left a temporary file behind"
    );
}

/// The lock exists to keep two agents from writing the index at once. A lock
/// that outlives its holder stalls every later writer for the whole staleness
/// window, so releasing on drop is part of the contract.
#[test]
fn the_index_lock_is_released_when_it_is_dropped() {
    let dir = TempDir::new("lock");
    let index_file = dir.join("index.json");
    let lock_file = dir.join("index.lock");

    {
        let _held = IndexLock::acquire(&index_file).expect("acquire the lock");
        assert!(
            lock_file.exists(),
            "the lock file is absent while the lock is held"
        );
    }

    assert!(
        !lock_file.exists(),
        "the lock file outlived the lock that made it"
    );

    IndexLock::acquire(&index_file).expect("a released lock can be taken again");
}

#[test]
fn loading_an_index_that_was_never_written_is_an_error() {
    let dir = TempDir::new("absent");
    assert!(
        AstIndex::load_from_disk(&dir.join("index.json")).is_err(),
        "a missing index file must not read as an empty index"
    );
}

fn recorded(index: &AstIndex) -> Vec<String> {
    index
        .list_symbols()
        .iter()
        .take(10)
        .map(|n| format!("{:?}", n))
        .collect()
}
