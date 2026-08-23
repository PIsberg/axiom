//! A scan indexes the codebase, not its vendored dependencies.
//!
//! The walk skipped `target`, `node_modules`, `build` and `dist`. A Python
//! project's `venv` and `__pycache__`, and a vendored `vendor` tree, went
//! straight into the symbol graph and the trigram store, burying the real
//! symbols under copies of libraries nobody is changing.

use axiom_ast::AstIndex;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_skip_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

#[test]
fn vendored_and_cache_directories_are_not_indexed() {
    let root = tmp("root");
    std::fs::write(root.join("app.py"), "def mine():\n    return 1\n").unwrap();

    for dir in ["venv", "__pycache__", "vendor", "node_modules"] {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("theirs.py"), "def vendored():\n    return 2\n").unwrap();
    }
    // `src/bin` must still be scanned: it is real Rust source, not a build dir,
    // so the skip list must not match a bare `bin`.
    let bin = root.join("src").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("tool.rs"), "fn helper_in_bin() {}\n").unwrap();

    let index = AstIndex::new();
    index.scan_directory(&root).expect("scan");
    let symbols = index.symbol_paths();

    assert!(
        symbols.iter().any(|s| s.contains("mine")),
        "the project's own symbol is missing: {symbols:?}"
    );
    assert!(
        symbols.iter().any(|s| s.contains("helper_in_bin")),
        "src/bin is real source and must be indexed: {symbols:?}"
    );
    assert!(
        !symbols.iter().any(|s| s.contains("vendored")),
        "a vendored directory was indexed: {symbols:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
