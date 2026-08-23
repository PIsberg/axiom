//! Scanning is parallel across files, so its result must not depend on the
//! order the workers happen to finish in. Two scans of the same tree produce
//! the same symbols and the same Merkle root.

use axiom_ast::AstIndex;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_determinism_{tag}_{:x}",
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
    for i in 0..12 {
        std::fs::write(
            root.join("src").join(format!("mod{i}.rs")),
            format!("pub fn f{i}(x: i32) -> i32 {{\n    helper{i}(x) + 1\n}}\nfn helper{i}(x: i32) -> i32 {{ x }}\n"),
        )
        .unwrap();
    }
    std::fs::write(root.join("app.py"), "def main():\n    return 0\n").unwrap();
    std::fs::write(root.join("Gate.java"), "public class Gate {\n  void open() {}\n}\n").unwrap();
}

#[test]
fn two_fresh_scans_agree() {
    let root = tmp("root");
    write_tree(&root);

    let a = AstIndex::new();
    a.scan_directory(&root).expect("scan a");
    let b = AstIndex::new();
    b.scan_directory(&root).expect("scan b");

    let mut sa = a.symbol_paths();
    let mut sb = b.symbol_paths();
    sa.sort();
    sb.sort();
    assert_eq!(sa, sb, "the same tree produced different symbol sets");
    assert_eq!(
        a.compute_merkle_root(),
        b.compute_merkle_root(),
        "the same tree produced different Merkle roots"
    );
    assert!(sa.len() >= 25, "expected the whole tree, got {}", sa.len());

    for _ in 0..5 {
        let c = AstIndex::new();
        c.scan_directory(&root).expect("scan c");
        assert_eq!(a.compute_merkle_root(), c.compute_merkle_root());
    }

    let _ = std::fs::remove_dir_all(&root);
}
