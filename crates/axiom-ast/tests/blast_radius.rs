//! What the blast radius has to discriminate between.
//!
//! The gap these close: every symbol in this repository returned all 49 tests,
//! 0% pruned, whatever was asked. `simple_name_of` reduced a file-keyed symbol
//! to its file extension, so `crates/axiom-ast/src/lib.rs::write_atomically`
//! became `rs`, and the fallback search then matched `rs::` against every
//! symbol indexed from a `.rs` file. A test suite whose fixtures are all Java
//! never saw it, because a Java key is package-qualified and reduces to the
//! class name it is supposed to.
//!
//! Each test here fails if the answer stops depending on the question.

use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("axiom-radius-{}-{}-{}", tag, std::process::id(), n));
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

/// The tests a symbol reaches at the given depth, by their short names.
fn impacted(index: &AstIndex, symbol: &str, depth: usize) -> Vec<String> {
    let radius = index
        .compute_blast_radius(symbol, depth)
        .unwrap_or_else(|| panic!("{symbol} should be in the index"));
    let mut names: Vec<String> = radius
        .impacted_tests
        .iter()
        .map(|t| t.rsplit("::").next().unwrap_or(t).to_string())
        .collect();
    names.sort();
    names
}

/// The tests a symbol reaches at each surveyed depth, by their short names.
fn by_depth(index: &AstIndex, symbol: &str, depth: usize) -> Vec<(usize, Vec<String>)> {
    let radius = index
        .compute_blast_radius(symbol, depth)
        .unwrap_or_else(|| panic!("{symbol} should be in the index"));
    let mut layers: Vec<(usize, Vec<String>)> = radius
        .tests_by_depth
        .into_iter()
        .map(|(d, tests)| {
            let mut names: Vec<String> = tests
                .iter()
                .map(|t| t.rsplit("::").next().unwrap_or(t).to_string())
                .collect();
            names.sort();
            (d, names)
        })
        .collect();
    layers.sort();
    layers
}

#[test]
fn a_symbol_reaches_only_the_tests_that_name_it() {
    let dir = TempDir::new("names-it");
    dir.write(
        "alpha.rs",
        "pub fn alpha_thing() -> bool { true }\npub fn beta_thing() -> bool { false }\n",
    );
    // Two tests in one file. Only the first calls alpha_thing, so charging the
    // second for it would mean the file, not the function, is the unit.
    dir.write(
        "alpha_test.rs",
        "#[test]\nfn test_alpha_path() {\n    assert!(alpha_thing());\n}\n\n\
         #[test]\nfn test_beta_path() {\n    assert!(!beta_thing());\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert_eq!(
        impacted(&index, "alpha_thing", 1),
        vec!["test_alpha_path".to_string()],
        "only the test that calls alpha_thing is impacted"
    );
    assert_eq!(
        impacted(&index, "beta_thing", 1),
        vec!["test_beta_path".to_string()],
        "only the test that calls beta_thing is impacted"
    );
}

#[test]
fn a_symbol_does_not_reach_every_test_written_in_its_language() {
    let dir = TempDir::new("not-everything");
    // Nothing calls this. The old answer was every test in a .rs file, because
    // the symbol reduced to "rs" and the fallback matched "rs::" against every
    // Rust symbol in the index.
    dir.write("lonely.rs", "pub fn nobody_calls_me() -> bool { true }\n");
    dir.write("used.rs", "pub fn somebody_calls_me() -> bool { true }\n");
    dir.write(
        "suite_test.rs",
        "#[test]\nfn test_one() {\n    assert!(somebody_calls_me());\n}\n\n\
         #[test]\nfn test_two() {\n    assert!(somebody_calls_me());\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let radius = index
        .compute_blast_radius("nobody_calls_me", 1)
        .expect("the symbol is indexed");
    assert!(
        radius.impacted_tests.is_empty(),
        "nothing calls this symbol, so nothing should be impacted; got {:?}",
        radius.impacted_tests
    );
    assert_eq!(radius.total_tests_in_repo, 2);

    assert_eq!(
        impacted(&index, "somebody_calls_me", 1),
        vec!["test_one".to_string(), "test_two".to_string()]
    );
}

#[test]
fn the_language_of_a_symbol_does_not_select_the_tests() {
    let dir = TempDir::new("cross-language");
    // The sharpest form of the same defect: ask about a Rust symbol and get the
    // Rust tests, ask about a Python one and get the Python tests, whether or
    // not anything references either.
    dir.write("alpha.rs", "pub fn rust_only_thing() -> bool { true }\n");
    dir.write(
        "alpha_test.rs",
        "#[test]\nfn test_rust_one() {\n    assert!(true);\n}\n",
    );
    dir.write("beta.py", "def python_only_thing():\n    return True\n");
    dir.write(
        "beta_test.py",
        "class BetaTest:\n    def test_py_one(self):\n        assert True\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    for symbol in ["rust_only_thing", "python_only_thing"] {
        let radius = index
            .compute_blast_radius(symbol, 1)
            .unwrap_or_else(|| panic!("{symbol} should be indexed"));
        assert!(
            radius.impacted_tests.is_empty(),
            "no test references {symbol}, so the extension must not stand in for a reference; got {:?}",
            radius.impacted_tests
        );
    }
}

#[test]
fn a_test_reaching_a_symbol_through_another_function_is_surveyed() {
    let dir = TempDir::new("transitive");
    dir.write(
        "core.rs",
        "pub fn inner_detail() -> bool { true }\npub fn outer_api() -> bool { inner_detail() }\n",
    );
    dir.write(
        "core_test.rs",
        "#[test]\nfn test_through_the_api() {\n    assert!(outer_api());\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    // The test never names inner_detail, so it is not a direct dependent and
    // must not be counted as one.
    assert!(
        impacted(&index, "inner_detail", 1).is_empty(),
        "depth 1 is what a caller asked for and must stay that"
    );

    // It is still reachable, and reporting nothing about it leaves a caller
    // unable to decide whether widening is worth it. This is what the walk kept
    // silent: `reverse_deps` is keyed by the name a caller writes, and after the
    // first hop the walk was looking up full symbol paths, which are never keys.
    let layers = by_depth(&index, "inner_detail", 1);
    assert!(
        layers
            .iter()
            .any(|(d, tests)| *d >= 2 && tests.iter().any(|t| t == "test_through_the_api")),
        "the test should be surveyed at depth 2 or beyond; got {layers:?}"
    );

    // And widening moves it into the answer rather than leaving it stranded.
    assert_eq!(
        impacted(&index, "inner_detail", 2),
        vec!["test_through_the_api".to_string()]
    );
}

#[test]
fn a_java_symbol_still_resolves_through_its_class_name() {
    let dir = TempDir::new("java-unchanged");
    // Java keys are package-qualified, so the last dot really is a package
    // separator and the short name really is the class. Nothing here should
    // have changed, which is the point of pinning it beside the Rust cases.
    dir.write(
        "Gate.java",
        "package p;\npublic class Gate {\n    public boolean isOpen() { return true; }\n}\n",
    );
    dir.write(
        "GateTest.java",
        "package p;\nimport org.junit.jupiter.api.Test;\npublic class GateTest {\n    @Test\n    public void checksTheGate() {\n        Gate g = new Gate();\n        assert g.isOpen();\n    }\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let names = impacted(&index, "p.Gate", 1);
    assert!(
        names
            .iter()
            .any(|n| n.contains("GateTest") || n.contains("checksTheGate")),
        "the Java path resolves through the class name; got {names:?}"
    );
}
