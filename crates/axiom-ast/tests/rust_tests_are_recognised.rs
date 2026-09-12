//! A Rust function is a test because it is annotated, not because of its name.
//!
//! The parser decided with
//!
//! ```text
//! let is_test = name.starts_with("test_") || decl.contains("#[test]");
//! ```
//!
//! where `decl` is the declaration line alone. `#[test]` conventionally sits on
//! the line above the `fn`, so that half never fired and in practice only a
//! `test_`-prefixed name counted. This repository mostly does not name them that
//! way: counted over `crates/` on 2026-09-11, 305 `#[test]` and `#[tokio::test]`
//! attributes, 95 with a `test_` name, and exactly 94 symbols indexed as tests.
//! 210 of its own tests were filed as ordinary functions.
//!
//! Two things follow, and the second is the one that matters. Every
//! `pruned_test_percentage` was taken against `total_tests_in_repo` of 94 rather
//! than ~305. And a test the index does not know is a test can never be
//! selected: `compute_blast_radius` only records a node when `kind == "test"`,
//! so a change could report a small, confident impacted set that omitted the
//! tests actually covering it. That is a recall failure, which is the direction
//! the fifth idea in CLAUDE.md calls unsafe.
//!
//! The JVM path already tracked its annotation across the preceding lines. This
//! is the same rule for Rust, pinned in both directions: what must now be a
//! test, and what must still not be.

use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "axiom-rust-tests-{}-{}-{}",
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

fn kind_of(index: &AstIndex, name: &str) -> String {
    index
        .get_symbol(name)
        .unwrap_or_else(|| panic!("{name} should be indexed"))
        .kind
}

/// The ordinary shape: the attribute on its own line, a descriptive name.
///
/// This is how almost every test in this repository is written.
#[test]
fn an_attribute_on_the_line_above_makes_a_function_a_test() {
    let dir = TempDir::new("attr-above");
    dir.write(
        "suite.rs",
        "#[test]\nfn a_descriptive_name_without_the_prefix() {\n    assert!(true);\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert_eq!(
        kind_of(&index, "a_descriptive_name_without_the_prefix"),
        "test",
        "the attribute is what makes it a test; the name is a convention"
    );
}

/// The async shape, which this repository uses for every server test.
#[test]
fn a_tokio_test_attribute_counts_too() {
    let dir = TempDir::new("tokio");
    dir.write(
        "suite.rs",
        "#[tokio::test]\nasync fn the_server_answers() {\n    assert!(true);\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert_eq!(kind_of(&index, "the_server_answers"), "test");
}

/// Attributes stack, and a doc comment may sit above them.
#[test]
fn the_attribute_is_still_found_under_other_attributes_and_a_doc_comment() {
    let dir = TempDir::new("stacked");
    dir.write(
        "suite.rs",
        "/// What this pins.\n#[test]\n#[should_panic]\nfn it_panics() {\n    panic!(\"x\");\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert_eq!(kind_of(&index, "it_panics"), "test");
}

/// The old rule still holds, so nothing that was a test stops being one.
#[test]
fn a_test_prefixed_name_is_still_a_test() {
    let dir = TempDir::new("prefix");
    dir.write(
        "suite.rs",
        "#[test]\nfn test_the_old_way() {\n    assert!(true);\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert_eq!(kind_of(&index, "test_the_old_way"), "test");
}

/// The other direction, which is the one a loosened match gets wrong.
///
/// An ordinary function that merely follows a test must not inherit its kind,
/// or the walk back has run past the thing it was reading.
#[test]
fn a_plain_function_after_a_test_is_not_a_test() {
    let dir = TempDir::new("not-contagious");
    dir.write(
        "suite.rs",
        "#[test]\nfn the_real_test() {\n    assert!(helper());\n}\n\npub fn helper() -> bool {\n    true\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert_eq!(kind_of(&index, "the_real_test"), "test");
    assert_eq!(
        kind_of(&index, "helper"),
        "function",
        "a function that happens to sit below a test is not one"
    );
}

/// An unrelated attribute does not make a test.
#[test]
fn a_function_carrying_some_other_attribute_is_not_a_test() {
    let dir = TempDir::new("other-attr");
    dir.write(
        "suite.rs",
        "#[inline]\npub fn fast_path() -> bool {\n    true\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert_eq!(kind_of(&index, "fast_path"), "function");
}

/// An attribute written inside a string literal is not an annotation.
///
/// This repository indexes parsers, so its fixtures are full of Rust written as
/// strings. The stripper exists for exactly that, and the walk back has to read
/// the stripped text like every other decision here.
#[test]
fn an_attribute_inside_a_string_literal_does_not_make_a_test() {
    let dir = TempDir::new("in-a-string");
    dir.write(
        "suite.rs",
        "pub fn fixture() -> String {\n    String::from(\"#[test]\")\n}\n\npub fn after_the_fixture() -> bool {\n    true\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert_eq!(
        kind_of(&index, "after_the_fixture"),
        "function",
        "an attribute quoted in a string is text, not an annotation"
    );
}

/// The count every pruned percentage is taken against.
#[test]
fn the_suite_size_counts_annotated_tests() {
    let dir = TempDir::new("count");
    dir.write(
        "suite.rs",
        "#[test]\nfn alpha() {\n    assert!(true);\n}\n\n#[tokio::test]\nasync fn beta() {\n    assert!(true);\n}\n\n#[test]\nfn test_gamma() {\n    assert!(true);\n}\n\npub fn not_a_test() -> bool {\n    true\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert_eq!(
        index.total_tests_count(),
        3,
        "three annotated functions, whatever they are called"
    );
}

/// The consequence the whole change is for: a test the index knows about is a
/// test the blast radius can select.
#[test]
fn a_descriptively_named_test_can_be_selected_by_the_blast_radius() {
    let dir = TempDir::new("selectable");
    dir.write(
        "lib.rs",
        "pub fn parse_header(s: &str) -> usize { s.len() }\n",
    );
    dir.write(
        "lib_test.rs",
        "#[test]\nfn a_header_with_no_colon_is_rejected() {\n    assert_eq!(parse_header(\"abc\"), 3);\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let radius = index
        .compute_blast_radius("parse_header", 1)
        .expect("the symbol is indexed");
    let names: Vec<String> = radius
        .impacted_tests
        .iter()
        .map(|t| t.rsplit("::").next().unwrap_or(t).to_string())
        .collect();
    assert!(
        names.contains(&"a_header_with_no_colon_is_rejected".to_string()),
        "the test calls parse_header, so it has to be in the answer: {names:?}"
    );
}
