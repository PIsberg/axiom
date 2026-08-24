//! `language_of_symbol` and `file_of_symbol` read a maintained symbol->file
//! map rather than scanning every file's symbol list. The risk a maintained
//! map carries is going stale, so this pins that a re-scan after a file is
//! deleted leaves no answer for that file's symbols.

use axiom_ast::AstIndex;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_symfile_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

#[test]
fn the_map_answers_and_does_not_go_stale() {
    let root = tmp("root");
    std::fs::write(
        root.join("gate.py"),
        "def is_open(depth):\n    return depth > 0\n",
    )
    .unwrap();
    std::fs::write(root.join("lib.rs"), "pub fn helper() {}\n").unwrap();

    let index = AstIndex::new();
    index.scan_directory(&root).expect("scan");

    let py = index
        .symbol_paths()
        .into_iter()
        .find(|s| s.contains("is_open"))
        .expect("python symbol indexed");
    assert_eq!(
        index.language_of_symbol(&py).as_deref(),
        Some("py"),
        "the map must report the language of an indexed symbol"
    );
    assert!(index.file_of_symbol(&py).is_some());

    // Delete the Python file and re-scan. Its symbol, and the map entry for it,
    // must be gone; the Rust symbol must survive.
    std::fs::remove_file(root.join("gate.py")).unwrap();
    index.scan_directory(&root).expect("re-scan");

    assert_eq!(
        index.language_of_symbol(&py),
        None,
        "a deleted file's symbol must leave no stale map entry"
    );
    let rs = index
        .symbol_paths()
        .into_iter()
        .find(|s| s.contains("helper"))
        .expect("rust symbol still indexed");
    assert_eq!(index.language_of_symbol(&rs).as_deref(), Some("rs"));

    let _ = std::fs::remove_dir_all(&root);
}
