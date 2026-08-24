//! Symbol keys and the Merkle root over them are the same wherever the code
//! lives, which is what lets the index be committed and a ledger's root be
//! compared across machines.
//!
//! Before this, keys were the absolute path plus the relative one, so the same
//! source under two directories produced two different indexes and two
//! different Merkle roots: `C:/a/lib.rs::f` versus `D:/b/lib.rs::f`.

use axiom_ast::AstIndex;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_portable_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

fn write_tree(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src").join("lib.rs"),
        "pub fn is_open(depth: i32) -> bool {\n    depth > 0\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("app.py"), "def main():\n    return is_open(1)\n").unwrap();
}

#[test]
fn the_same_code_in_two_places_indexes_the_same() {
    let one = tmp("one");
    let two = tmp("two");
    write_tree(&one);
    write_tree(&two);

    let a = AstIndex::new();
    a.scan_directory(&one).expect("scan one");
    let b = AstIndex::new();
    b.scan_directory(&two).expect("scan two");

    let mut ka = a.symbol_paths();
    let mut kb = b.symbol_paths();
    ka.sort();
    kb.sort();
    assert_eq!(
        ka, kb,
        "the same code under two roots produced different keys"
    );
    assert!(
        ka.iter().any(|k| k == "src/lib.rs::is_open"),
        "keys must be relative to the scan root, got {ka:?}"
    );
    assert!(
        !ka.iter().any(|k| k.contains(':') && k.contains("Temp")),
        "no key may carry an absolute path: {ka:?}"
    );
    assert_eq!(
        a.compute_merkle_root(),
        b.compute_merkle_root(),
        "the Merkle root must not depend on where the code lives"
    );

    // The relative key still resolves to a real file on this machine.
    let file = a.file_of_symbol("src/lib.rs::is_open").expect("resolves");
    assert!(
        std::path::Path::new(&file).is_absolute() && std::fs::read_to_string(&file).is_ok(),
        "file_of_symbol must return an openable absolute path, got {file}"
    );

    let _ = std::fs::remove_dir_all(&one);
    let _ = std::fs::remove_dir_all(&two);
}
