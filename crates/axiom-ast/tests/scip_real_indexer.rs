//! Ingesting the output of a real SCIP indexer, end to end.
//!
//! The other SCIP tests build an index in memory, which pins the ingestion
//! against a shape the test controls. This one runs an actual indexer, so it
//! catches drift between what an indexer really emits, `enclosing_range`
//! presence, how a symbol is spelled, whether a test is marked, and what the
//! ingestion assumes. It uses `rust-analyzer scip`, a rustup component and so
//! reliable to install on a runner; scip-java produces the same format and
//! ingests the same way, which the in-memory Java-shaped tests already pin.
//!
//! It is slow, a minute or so, and gated. With `AXIOM_REQUIRE_TOOLCHAINS` set,
//! as CI does, no indexer is a hard failure, so an install that silently did
//! nothing turns the suite red rather than passing without running the thing it
//! tests. Without it, the test skips.

use axiom_ast::AstIndex;
use std::path::Path;
use std::process::Command;

/// Resolve rust-analyzer, preferring the real binary over the rustup proxy on
/// PATH, and run `scip` in `dir`. Returns whether an index.scip was produced.
fn produce_scip(dir: &Path) -> bool {
    let ra = Command::new("rustup")
        .args(["which", "rust-analyzer"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "rust-analyzer".to_string());

    let ran = Command::new(&ra)
        .arg("scip")
        .arg(".")
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ran && dir.join("index.scip").exists()
}

#[test]
fn a_real_scip_index_ingests_with_resolved_edges() {
    let dir = std::env::temp_dir().join(format!(
        "axiom_scip_ra_{}_{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"rp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src").join("lib.rs"),
        "pub fn is_open(depth: i32) -> bool {\n    depth > 0\n}\n\
         pub fn check(depth: i32) -> bool {\n    is_open(depth)\n}\n\
         #[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn test_is_open() {\n        assert!(is_open(1));\n    }\n}\n",
    )
    .unwrap();

    if !produce_scip(&dir) {
        let _ = std::fs::remove_dir_all(&dir);
        if std::env::var("AXIOM_REQUIRE_TOOLCHAINS").is_ok() {
            panic!(
                "no SCIP index was produced; install one with `rustup component add rust-analyzer`"
            );
        }
        eprintln!("skipping: no SCIP indexer on PATH");
        return;
    }

    let ast = AstIndex::new();
    ast.ingest_scip(&dir.join("index.scip"), &dir)
        .expect("ingest the produced index");
    let symbols = ast.symbol_paths();

    let check = symbols
        .iter()
        .find(|s| s.ends_with("check"))
        .and_then(|s| ast.get_symbol(s))
        .expect("check indexed");
    assert!(
        check.dependencies.iter().any(|d| d.ends_with("is_open")),
        "check must depend on is_open from the real index: {:?}",
        check.dependencies
    );

    let is_open = symbols
        .iter()
        .find(|s| s.ends_with("is_open") && !s.contains("test"))
        .expect("is_open indexed");
    let radius = ast.compute_blast_radius(is_open, 2).expect("blast radius");
    assert!(
        radius
            .impacted_tests
            .iter()
            .any(|t| t.contains("test_is_open")),
        "the test that calls is_open must be selected: {:?}",
        radius.impacted_tests
    );

    let _ = std::fs::remove_dir_all(&dir);
}
