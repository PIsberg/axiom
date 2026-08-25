//! What the Go parser extracts from a Go file.
//!
//! It took a symbol's name as everything before the first `(` on a `func` line.
//! For `func (a *Alpha) Search(...)` that is the empty string, so every method
//! was skipped, and `type` declarations were not matched at all. A Go codebase
//! held package-level free functions and nothing else: one symbol from a file
//! declaring three.
//!
//! Nothing caught it because `every_indexed_language_has_an_evaluator` checks
//! that Go is on both the indexed list and the evaluator list, which it was.
//! Nothing checked that the parser finds what a Go file declares. Java has
//! `jvm_symbols.rs` for exactly this; this is Go's.

use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("axiom-go-{}-{}-{}", tag, std::process::id(), n));
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

fn short_names(index: &AstIndex) -> Vec<String> {
    index
        .symbol_paths()
        .iter()
        .map(|s| s.rsplit("::").next().unwrap_or(s).to_string())
        .collect()
}

/// The reproduction from the issue: three declarations, one symbol.
#[test]
fn a_go_file_declaring_three_things_yields_three_symbols() {
    let dir = TempDir::new("three");
    dir.write(
        "main.go",
        "package main\n\n\
         type Alpha struct{}\n\n\
         func (a *Alpha) Search(q string) bool {\n\
         \x20   return q != \"\"\n\
         }\n\n\
         func Helper() bool {\n\
         \x20   return true\n\
         }\n",
    );

    let index = scan(&dir);
    let names = short_names(&index);
    let symbols = index.symbol_paths();

    assert!(
        names.contains(&"Alpha".to_string()),
        "the type: {symbols:?}"
    );
    assert!(
        names.contains(&"Search".to_string()),
        "the method, which used to be skipped because its name parsed as empty: {symbols:?}"
    );
    assert!(
        names.contains(&"Helper".to_string()),
        "the free function: {symbols:?}"
    );
}

/// A method belongs to its receiver, as a Java method belongs to its class and a
/// Rust method to its `impl`.
#[test]
fn a_method_is_filed_under_its_receiver_not_at_file_scope() {
    let dir = TempDir::new("receiver");
    dir.write(
        "gate.go",
        "package main\n\n\
         type Alpha struct{}\n\
         type Beta struct{}\n\n\
         func (a *Alpha) Run() bool { return true }\n\
         func (b Beta) Run() bool { return false }\n",
    );

    let index = scan(&dir);
    let symbols = index.symbol_paths();

    assert!(
        symbols.iter().any(|s| s.ends_with("::Alpha::Run")),
        "a pointer receiver is the same type as a value one: {symbols:?}"
    );
    assert!(
        symbols.iter().any(|s| s.ends_with("::Beta::Run")),
        "two types declaring Run are two symbols: {symbols:?}"
    );
}

/// The failure modes a line-based parser has here, pinned rather than assumed.
///
/// A receiver taken for a name, a name taken from a call site, and a method
/// filed under the wrong type are each a way this parser could go wrong, and
/// each has an equivalent that has already happened in the Java parser.
#[test]
fn a_call_site_and_a_string_declare_nothing() {
    let dir = TempDir::new("callsite");
    dir.write(
        "caller.go",
        "package main\n\n\
         func Caller() bool {\n\
         \x20   src := \"func GhostFromAString() bool { return true }\"\n\
         \x20   _ = src\n\
         \x20   return Helper()\n\
         }\n",
    );

    let index = scan(&dir);
    let names = short_names(&index);
    let symbols = index.symbol_paths();

    assert!(names.contains(&"Caller".to_string()), "{symbols:?}");
    assert!(
        !names.contains(&"GhostFromAString".to_string()),
        "source inside a string is not a declaration: {symbols:?}"
    );
    assert!(
        !names.contains(&"Helper".to_string()),
        "Helper is called here, not declared here: {symbols:?}"
    );
    for name in &names {
        assert!(
            !name.is_empty() && !name.contains(' ') && !name.contains('*'),
            "a receiver must not be taken for a name: {symbols:?}"
        );
    }
}

/// Interfaces and aliases are declarations too. Matching only `struct` would
/// have left the same gap one keyword narrower.
#[test]
fn an_interface_and_an_alias_are_indexed() {
    let dir = TempDir::new("kinds");
    dir.write(
        "kinds.go",
        "package main\n\n\
         type Reader interface {\n\
         \x20   Read() bool\n\
         }\n\n\
         type Meters float64\n",
    );

    let index = scan(&dir);
    assert_eq!(
        index.get_symbol("Reader").expect("interface indexed").kind,
        "interface"
    );
    assert_eq!(
        index.get_symbol("Meters").expect("alias indexed").kind,
        "type"
    );
}

/// A Go test function is a test, so the blast radius can select it.
#[test]
fn a_go_test_function_is_indexed_as_a_test() {
    let dir = TempDir::new("gotest");
    dir.write(
        "gate_test.go",
        "package main\n\n\
         func TestSearchWorks(t *testing.T) {\n\
         \x20   _ = t\n\
         }\n",
    );

    let index = scan(&dir);
    assert_eq!(
        index.get_symbol("TestSearchWorks").expect("indexed").kind,
        "test"
    );
}

#[test]
fn generic_go_functions_types_and_methods_are_indexed() {
    let dir = TempDir::new("generics");
    dir.write(
        "generics.go",
        "package main\n\n\
         type Stack[T any] struct {\n\
         \x20   items []T\n\
         }\n\n\
         type Container[K comparable, V any] interface {\n\
         \x20   Get(k K) V\n\
         }\n\n\
         func (s *Stack[T]) Push(item T) {\n\
         \x20   s.items = append(s.items, item)\n\
         }\n\n\
         func MapValues[K comparable, V any, R any](m map[K]V, f func(V) R) map[K]R {\n\
         \x20   return nil\n\
         }\n",
    );

    let index = scan(&dir);
    let symbols = index.symbol_paths();

    assert!(
        index.get_symbol("Stack").is_some(),
        "generic struct Stack indexed: {symbols:?}"
    );
    assert!(
        index.get_symbol("Container").is_some(),
        "generic interface Container indexed: {symbols:?}"
    );
    assert!(
        symbols.iter().any(|s| s.ends_with("::Stack::Push")),
        "method on generic receiver indexed: {symbols:?}"
    );
    assert!(
        index.get_symbol("MapValues").is_some(),
        "generic free function MapValues indexed: {symbols:?}"
    );
}
