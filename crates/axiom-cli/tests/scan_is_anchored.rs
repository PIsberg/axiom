//! `axiom scan` indexes the tree it was pointed at, and nothing above it.
//!
//! Every subcommand built its server with `AxiomMcpServer::new`, which walks up
//! from the working directory to find an index and loads it. For a read that is
//! right. For `scan` it meant the symbols of an ancestor index were loaded into
//! memory, then written back into the target's own index alongside the freshly
//! scanned ones. Measured on 2026-08-23: scanning a scratch directory produced
//! an index carrying all 494 symbols of the repository two levels up.

use std::process::Command;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_scan_anchor_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

#[test]
fn a_scan_does_not_inherit_an_ancestor_index() {
    let root = tmp("root");
    // An ancestor index carrying a symbol that exists in no file below.
    std::fs::create_dir_all(root.join(".axiom")).unwrap();
    std::fs::write(
        root.join(".axiom").join("index.json"),
        r#"{"format_version":2,"nodes":{"ghost::from::ancestor":{"id":"g","symbol_path":"ghost::from::ancestor","kind":"function","hash":"h","source_range":[0,0],"docstring":null,"signature":"fn ghost()","dependencies":[]}},"method_return_types":{},"file_call_names":{},"file_to_symbols":{}}"#,
    )
    .unwrap();

    // A subdirectory with one real source file.
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(
        sub.join("gate.py"),
        "def is_open(depth):\n    return depth > 0\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .args(["scan", "--path", "."])
        .current_dir(&sub)
        .output()
        .expect("run axiom scan");
    assert!(out.status.success(), "scan failed: {out:?}");

    let index = sub.join(".axiom").join("index.json");
    assert!(
        index.exists(),
        "scan wrote no index under the scanned directory"
    );
    let text = std::fs::read_to_string(&index).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    let nodes = parsed["nodes"].as_object().unwrap();

    assert!(
        nodes.keys().any(|k| k.contains("is_open")),
        "the scanned file's symbol is missing: {:?}",
        nodes.keys().collect::<Vec<_>>()
    );
    assert!(
        !nodes.contains_key("ghost::from::ancestor"),
        "the ancestor index leaked into the scanned one: {:?}",
        nodes.keys().collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&root);
}
