//! What `source_range` and `signature` have to hold.
//!
//! Both are returned to agents by `axiom_query_symbol`, and neither held what
//! its name said: `source_range` was `(0, content.len())`, the character length
//! of a declaration rather than a position in a file, and `signature` was the
//! symbol path, which the response already carries as `symbol_path`. The
//! declaration text was hashed and then thrown away.
//!
//! That is not a cosmetic problem. `cache-validate` located symbols by
//! `source_range`, read it as a line range, and so edited from line 0 to line
//! `len`, which on a short file is the whole file. It mutated `is_open`,
//! attributed the breakage to `unrelated`, and reported a dependency hole that
//! did not exist. Any agent using the field to find code gets the same answer.

use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "axiom-source-range-{}-{}-{}",
            tag,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test directory");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.0.join(name), body).expect("write the fixture file");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The lines a source range names, read back out of the file the symbol was
/// indexed from. That is the whole point of the field: a caller holding a node
/// should be able to open the file and find the declaration.
fn lines_at(index: &AstIndex, symbol: &str) -> String {
    let node = index
        .get_symbol(symbol)
        .unwrap_or_else(|| panic!("{symbol} should be indexed"));
    let file = node
        .symbol_path
        .split("::")
        .next()
        .expect("a file-keyed symbol names its file");
    let text = std::fs::read_to_string(file)
        .unwrap_or_else(|e| panic!("{symbol} names {file}, which cannot be read: {e}"));
    let all: Vec<&str> = text.lines().collect();

    let (start, end) = node.source_range;
    assert!(
        start >= 1 && end >= start && end <= all.len(),
        "{symbol}: {:?} is not a line range in a file of {} lines",
        node.source_range,
        all.len()
    );
    all[start - 1..end].join("\n")
}

#[test]
fn a_source_range_brackets_the_declaration_it_names() {
    let dir = TempDir::new("rust");
    dir.write(
        "gate.rs",
        "// a comment first, so line 1 is not the answer by accident\npub fn is_open(depth: i32) -> bool {\n    depth > 0\n}\n\npub fn unrelated() -> i32 {\n    7\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert_eq!(
        index.get_symbol("is_open").expect("is_open").source_range,
        (2, 2),
        "is_open is declared on line 2"
    );
    assert_eq!(
        index
            .get_symbol("unrelated")
            .expect("unrelated")
            .source_range,
        (6, 6),
        "unrelated is declared on line 6"
    );

    assert!(lines_at(&index, "is_open").contains("fn is_open"));
    assert!(lines_at(&index, "unrelated").contains("fn unrelated"));
}

#[test]
fn a_signature_holds_the_declaration_rather_than_the_symbol_path() {
    let dir = TempDir::new("signature");
    dir.write(
        "gate.rs",
        "pub fn is_open(depth: i32) -> bool {\n    depth > 0\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let node = index.get_symbol("is_open").expect("is_open");
    assert_eq!(
        node.signature.as_deref(),
        Some("pub fn is_open(depth: i32) -> bool {"),
        "the declaration is the one thing a caller cannot get from the other fields"
    );
    assert_ne!(
        node.signature.as_deref(),
        Some(node.symbol_path.as_str()),
        "the symbol path is already in symbol_path"
    );
}

#[test]
fn a_wrapped_parameter_list_is_bracketed_to_its_last_line() {
    let dir = TempDir::new("java-wrapped");
    // The Java parser joins a wrapped parameter list before it reads the name,
    // so the line cursor has already walked to the closing line by the time the
    // node is made. Recording only that line points a caller at wherever the
    // parameters happen to close rather than at the declaration.
    dir.write(
        "Gate.java",
        "package p;\npublic class Gate {\n    public boolean isOpen(\n            int depth,\n            int width) {\n        return depth > 0;\n    }\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let node = index.get_symbol("p.Gate::isOpen").expect("p.Gate::isOpen");
    assert_eq!(
        node.source_range,
        (3, 5),
        "the declaration opens on line 3 and its parameter list closes on line 5"
    );

    let text = std::fs::read_to_string(dir.path().join("Gate.java")).expect("read the fixture");
    let all: Vec<&str> = text.lines().collect();
    let bracketed = all[node.source_range.0 - 1..node.source_range.1].join("\n");
    assert!(
        bracketed.contains("isOpen") && bracketed.contains("int width"),
        "the range has to hold the whole declaration; got {bracketed:?}"
    );
}

#[test]
fn every_scanned_language_reports_a_position() {
    let dir = TempDir::new("polyglot");
    dir.write("a.rs", "\npub fn rust_fn() -> bool {\n    true\n}\n");
    dir.write("b.py", "\n\ndef python_fn():\n    return True\n");
    dir.write(
        "c.ts",
        "\nexport function ts_fn(): boolean {\n    return true;\n}\n",
    );
    dir.write(
        "d.go",
        "package main\n\nfunc GoFn() bool {\n    return true\n}\n",
    );
    dir.write(
        "E.java",
        "package p;\npublic class E {\n    public boolean javaFn() { return true; }\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    for (symbol, expected) in [
        ("rust_fn", (2usize, 2usize)),
        ("python_fn", (3, 3)),
        ("ts_fn", (2, 2)),
        ("GoFn", (3, 3)),
        ("p.E::javaFn", (3, 3)),
    ] {
        let node = index
            .get_symbol(symbol)
            .unwrap_or_else(|| panic!("{symbol} should be indexed"));
        assert_eq!(
            node.source_range, expected,
            "{symbol} is declared on {expected:?}, not {:?}",
            node.source_range
        );
    }

    // And the range reads back out of the file for the file-keyed ones.
    for symbol in ["rust_fn", "python_fn", "ts_fn", "GoFn"] {
        assert!(
            lines_at(&index, symbol).contains(symbol),
            "{symbol} should appear in the lines its range names"
        );
    }
}

#[test]
fn a_node_with_no_recorded_position_says_so() {
    // `index_node` is the by-hand entry point, used by the demo workspace and
    // by `axiom_apply_mutation`. There is no file to point at, and inventing a
    // range would be worse than reporting none.
    let index = AstIndex::new();
    let node = index.index_node("pkg.Hand::made", "method", "void made() {}", vec![]);
    assert_eq!(
        node.source_range,
        (0, 0),
        "a node with no position reports none rather than a made-up one"
    );
}
