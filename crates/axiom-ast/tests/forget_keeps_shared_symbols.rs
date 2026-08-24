//! Forgetting one file must not delete a symbol another file still declares.
//!
//! A Java class is keyed by its package and name, so two files can carry the
//! same key: two `se.demo.Widget` in two directories, or, as it happened in
//! practice, a stale entry left in a shared index by one test and a live one
//! written by another. When the stale file was purged, `forget_file` deleted
//! the shared symbol from the graph, taking the live declaration with it.
//! Measured as an intermittent parallel failure of the full end-to-end loop.

use axiom_ast::AstIndex;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_forgetdup_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

fn widget(pkg: &str) -> String {
    format!("package {pkg};\npublic class Widget {{\n    public void run() {{}}\n}}\n")
}

#[test]
fn purging_a_stale_file_keeps_a_symbol_another_file_declares() {
    let root = tmp("root");
    let a = root.join("a");
    let b = root.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    // Two files, one fully-qualified class `se.demo.Widget`, declared in both.
    std::fs::write(a.join("Widget.java"), widget("se.demo")).unwrap();
    std::fs::write(b.join("Widget.java"), widget("se.demo")).unwrap();

    let index = AstIndex::new();
    index.scan_directory(&root).expect("scan both");
    assert!(
        index.get_symbol("se.demo.Widget").is_some(),
        "the class must be indexed: {:?}",
        index.symbol_paths()
    );

    // Delete one of the two files and re-scan. The class is still declared by
    // the surviving file, so purging the deleted one must not remove it.
    std::fs::remove_file(a.join("Widget.java")).unwrap();
    index.scan_directory(&root).expect("re-scan");

    assert!(
        index.get_symbol("se.demo.Widget").is_some(),
        "forgetting one declaration of a shared class deleted the class itself: {:?}",
        index.symbol_paths()
    );

    // And when the last declaration goes, the symbol does too.
    std::fs::remove_file(b.join("Widget.java")).unwrap();
    index.scan_directory(&root).expect("re-scan empty");
    assert!(
        index.get_symbol("se.demo.Widget").is_none(),
        "with no file declaring it, the class must be gone"
    );

    let _ = std::fs::remove_dir_all(&root);
}
