//! Source inside a string literal is not a declaration.
//!
//! Every parser used to match declarations on the raw line, so a fixture written
//! as a string literal put its declarations into the index. On this repository
//! that produced `blast_radius.rs::looks_like_a_pattern`, which is not a
//! function but a line inside a string in a test.
//!
//! The cost was not a larger index. It made the real function ambiguous, so
//! `axiom symbol --path looks_like_a_pattern` refused a real name because a test
//! mentioned it in a fixture. Ambiguity is the right answer to a name that means
//! two things; the defect was that it only meant two things because a string was
//! read as code.
//!
//! A repository whose subject is parsing writes source inside strings
//! constantly, so this is not an edge case here.

use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "axiom-strings-{}-{}-{}",
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

fn scan(dir: &TempDir) -> AstIndex {
    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan the fixture");
    index
}

/// The exact shape that caused it: a test writing a Rust fixture as a string.
#[test]
fn a_rust_fixture_written_as_a_string_contributes_no_symbols() {
    let dir = TempDir::new("rust");
    dir.write(
        "real.rs",
        "pub fn the_only_real_function() -> bool {\n    true\n}\n",
    );
    dir.write(
        "writes_a_fixture.rs",
        "pub fn writer() {\n\
         \x20   let fixture = \"pub fn ghost_from_a_string() -> bool { true }\";\n\
         \x20   let multi = \"fn first_ghost() {}\nstruct GhostStruct {}\n\";\n\
         \x20   let _ = (fixture, multi);\n\
         }\n",
    );

    let symbols = scan(&dir).symbol_paths();
    let short: Vec<&str> = symbols
        .iter()
        .map(|s| s.rsplit("::").next().unwrap_or(s))
        .collect();

    assert!(
        short.contains(&"the_only_real_function") && short.contains(&"writer"),
        "the real declarations must still be indexed: {symbols:?}"
    );
    for ghost in ["ghost_from_a_string", "first_ghost", "GhostStruct"] {
        assert!(
            !short.contains(&ghost),
            "{ghost} is text inside a string literal, not a declaration: {symbols:?}"
        );
    }
}

/// The consequence, stated as the thing a caller sees.
///
/// A name that means one thing has to resolve. Before the fix a fixture
/// mentioning it made it mean two, and the lookup was refused.
#[test]
fn a_real_name_is_not_made_ambiguous_by_a_fixture_that_mentions_it() {
    let dir = TempDir::new("ambiguity");
    dir.write(
        "engine.rs",
        "pub fn looks_like_a_pattern(query: &str) -> bool {\n\
         \x20   query.contains('*')\n\
         }\n",
    );
    dir.write(
        "engine_test.rs",
        "pub fn fixture() {\n\
         \x20   let src = \"fn looks_like_a_pattern(query: &str) -> bool { true }\";\n\
         \x20   let _ = src;\n\
         }\n",
    );

    let index = scan(&dir);
    let found = index
        .get_symbol("looks_like_a_pattern")
        .expect("one real declaration means the name resolves");
    assert!(
        found.symbol_path.contains("engine.rs"),
        "it must resolve to the real one: {}",
        found.symbol_path
    );
}

/// A declaration that genuinely contains a string keeps it in the stored
/// signature. Stripping decides what a declaration *is*; it must not decide what
/// the declaration *says*.
#[test]
fn a_declaration_containing_a_string_keeps_it_in_the_signature() {
    let dir = TempDir::new("signature");
    dir.write(
        "defaults.py",
        "def greet(name=\"world\"):\n    return name\n",
    );

    let index = scan(&dir);
    let node = index.get_symbol("greet").expect("greet is indexed");
    let signature = node.signature.unwrap_or_default();
    assert!(
        signature.contains("world"),
        "the stored signature is the raw declaration, strings and all: {signature:?}"
    );
}

/// The same rule for every language the indexer reads.
///
/// Each of these files declares exactly one thing and mentions another inside a
/// string literal. Written as one test so that a parser fixed in isolation
/// cannot leave a sibling behind, which is how this defect existed in five
/// places at once.
#[test]
fn no_parser_reads_a_string_literal_as_a_declaration() {
    let dir = TempDir::new("all");
    dir.write(
        "gate.py",
        "def real_python():\n    src = \"def ghost_python(): pass\"\n    return src\n",
    );
    dir.write(
        "gate.ts",
        "function realTypescript(): string {\n\
         \x20 const src = \"function ghostTypescript() { return 1; }\";\n\
         \x20 return src;\n\
         }\n",
    );
    dir.write(
        "gate.go",
        "package main\n\n\
         func RealGo() string {\n\
         \x20   src := \"func GhostGo() bool { return true }\"\n\
         \x20   return src\n\
         }\n",
    );
    dir.write(
        "Gate.java",
        "public class RealJava {\n\
         \x20   public String build() {\n\
         \x20       return \"public class GhostJava { public void ghostMethod() {} }\";\n\
         \x20   }\n\
         }\n",
    );

    let symbols = scan(&dir).symbol_paths();
    let short: Vec<&str> = symbols
        .iter()
        .map(|s| s.rsplit("::").next().unwrap_or(s))
        .collect();

    for real in ["real_python", "realTypescript", "RealGo", "RealJava"] {
        assert!(
            short.contains(&real),
            "{real} is a real declaration and must be indexed: {symbols:?}"
        );
    }
    for ghost in [
        "ghost_python",
        "ghostTypescript",
        "GhostGo",
        "GhostJava",
        "ghostMethod",
    ] {
        assert!(
            !short.contains(&ghost),
            "{ghost} is text inside a string literal, not a declaration: {symbols:?}"
        );
    }
}
