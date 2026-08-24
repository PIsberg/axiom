//! Ingesting a SCIP index produces precise nodes and edges.
//!
//! The index here is built in memory rather than by running scip-java, so the
//! test is deterministic and needs no toolchain. It is shaped like a small Java
//! program: a `Gate` class whose `check` calls `isOpen`, and a `GateTest` whose
//! `testOpen` calls `isOpen`. The point is that the edges come from resolved
//! occurrences, so `check` depends on `isOpen` and the blast radius of `isOpen`
//! reaches the test, with no string matching involved.

use axiom_ast::AstIndex;
use axiom_ast::scip_ingest::render_symbol;
use protobuf::EnumOrUnknown;
use protobuf::Message;
use scip::types::symbol_information::Kind;
use scip::types::{Document, Index, Occurrence, SymbolInformation};

const DEF: i32 = 1;
const TEST: i32 = 32;

fn sym(name: &str) -> String {
    format!("scip-java maven demo 1.0 {name}")
}

fn occ(symbol: String, line: i32, roles: i32, body_end: i32) -> Occurrence {
    let mut o = Occurrence::new();
    o.symbol = symbol;
    o.range = vec![line, 2, line, 40];
    o.symbol_roles = roles;
    o.enclosing_range = vec![line, 0, body_end, 1];
    o
}

fn info(symbol: String, kind: Kind, display: &str) -> SymbolInformation {
    let mut si = SymbolInformation::new();
    si.symbol = symbol;
    si.kind = EnumOrUnknown::new(kind);
    si.display_name = display.to_string();
    si
}

/// A one-document index used by the file round-trip test below.
fn tiny_index() -> Index {
    let is_open = sym("com/example/Gate#isOpen().");
    let check = sym("com/example/Gate#check().");
    let mut doc = Document::new();
    doc.relative_path = "Gate.java".into();
    doc.occurrences = vec![
        occ(is_open.clone(), 0, DEF, 0),
        occ(check.clone(), 1, DEF, 1),
        occ(is_open.clone(), 1, 8, 1),
    ];
    doc.symbols = vec![
        info(is_open, Kind::Method, "isOpen"),
        info(check, Kind::Method, "check"),
    ];
    let mut index = Index::new();
    index.documents = vec![doc];
    index
}

#[test]
fn a_scip_file_is_read_from_disk_and_ingested() {
    // The bytes are real SCIP protobuf, so this exercises the same parse the CLI
    // does for a file an indexer wrote, not just the in-memory path.
    let dir = std::env::temp_dir().join(format!(
        "axiom_scipfile_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let scip_path = dir.join("index.scip");
    std::fs::write(&scip_path, tiny_index().write_to_bytes().unwrap()).unwrap();

    let ast = AstIndex::new();
    let summary = ast.ingest_scip(&scip_path, &dir).expect("ingest the file");
    assert_eq!(summary.nodes_indexed, 2);
    let check = ast
        .get_symbol("com.example.Gate#check")
        .expect("check indexed");
    assert!(
        check
            .dependencies
            .iter()
            .any(|d| d == "com.example.Gate#isOpen"),
        "the edge survived the protobuf round-trip: {:?}",
        check.dependencies
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn render_maps_descriptors_to_a_readable_key() {
    assert_eq!(
        render_symbol("scip-java maven demo 1.0 com/example/Gate#isOpen().").as_deref(),
        Some("com.example.Gate#isOpen")
    );
    assert_eq!(
        render_symbol("scip-java maven demo 1.0 com/example/Gate#").as_deref(),
        Some("com.example.Gate")
    );
    assert_eq!(render_symbol("local 4"), None);
}

#[test]
fn scip_edges_are_resolved_not_matched() {
    let gate = sym("com/example/Gate#");
    let is_open = sym("com/example/Gate#isOpen().");
    let check = sym("com/example/Gate#check().");
    let gate_test = sym("com/example/GateTest#");
    let test_open = sym("com/example/GateTest#testOpen().");

    let mut doc = Document::new();
    doc.relative_path = "src/com/example/Gate.java".into();
    doc.occurrences = vec![
        occ(gate.clone(), 1, DEF, 4),
        occ(is_open.clone(), 2, DEF, 2),
        occ(check.clone(), 3, DEF, 3),
        // check() calls isOpen(): a reference inside check's body.
        occ(is_open.clone(), 3, 8, 3),
        occ(gate_test.clone(), 5, DEF, 7),
        occ(test_open.clone(), 6, DEF | TEST, 6),
        // testOpen() calls isOpen(): a reference inside the test's body.
        occ(is_open.clone(), 6, 8, 6),
    ];
    doc.symbols = vec![
        info(gate, Kind::Class, "Gate"),
        info(is_open.clone(), Kind::Method, "boolean isOpen(int)"),
        info(check.clone(), Kind::Method, "boolean check(int)"),
        info(gate_test, Kind::Class, "GateTest"),
        info(test_open.clone(), Kind::Method, "void testOpen()"),
    ];

    let mut index = Index::new();
    index.documents = vec![doc];

    let ast = AstIndex::new();
    let summary = ast
        .ingest_scip_index(&index, &std::env::temp_dir())
        .expect("ingest");
    assert_eq!(summary.files_scanned, 1);
    assert_eq!(summary.nodes_indexed, 5);

    // The method is indexed under its readable key, carrying its signature.
    let node = ast
        .get_symbol("com.example.Gate#isOpen")
        .expect("isOpen indexed");
    assert_eq!(node.kind, "method");
    assert_eq!(node.signature.as_deref(), Some("boolean isOpen(int)"));

    // The precise edge: check depends on isOpen, resolved from the occurrence,
    // not guessed from the text.
    let check_node = ast
        .get_symbol("com.example.Gate#check")
        .expect("check indexed");
    assert!(
        check_node
            .dependencies
            .iter()
            .any(|d| d == "com.example.Gate#isOpen"),
        "check must depend on isOpen: {:?}",
        check_node.dependencies
    );

    // The test is recognised by the Test role and reaches isOpen, so it is in
    // the blast radius of a change to isOpen.
    let test_node = ast
        .get_symbol("com.example.GateTest#testOpen")
        .expect("test indexed");
    assert_eq!(test_node.kind, "test");

    let radius = ast
        .compute_blast_radius("com.example.Gate#isOpen", 1)
        .expect("blast radius");
    assert!(
        radius
            .impacted_tests
            .iter()
            .any(|t| t == "com.example.GateTest#testOpen"),
        "the test that calls isOpen must be in its blast radius: {:?}",
        radius.impacted_tests
    );
}
