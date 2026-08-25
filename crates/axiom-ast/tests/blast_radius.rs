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

#[test]
fn two_methods_of_the_same_name_in_one_file_are_two_symbols() {
    let dir = TempDir::new("same-name-methods");
    // Both impls declare `search`. Keyed by file and short name alone the two
    // collide: the later declaration overwrites the earlier node, and the line
    // recorded for the key moves with it, so every call the first method makes
    // is charged to whatever symbol happens to precede it.
    dir.write(
        "engine.rs",
        "pub struct Alpha;\n\
         \n\
         impl Alpha {\n\
         \x20   pub fn search(&self, q: &str) -> bool {\n\
         \x20       looks_like_a_pattern(q)\n\
         \x20   }\n\
         }\n\
         \n\
         pub struct Beta;\n\
         \n\
         impl Beta {\n\
         \x20   pub fn search(&self, q: &str) -> bool {\n\
         \x20       q.is_empty()\n\
         \x20   }\n\
         }\n\
         \n\
         fn looks_like_a_pattern(query: &str) -> bool {\n\
         \x20   query.contains('*')\n\
         }\n",
    );
    dir.write(
        "engine_test.rs",
        "#[test]\nfn test_alpha_search_sees_a_pattern() {\n    assert!(Alpha.search(\"a*b\"));\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let paths = index.symbol_paths();
    let searches: Vec<&String> = paths.iter().filter(|p| p.ends_with("::search")).collect();
    assert_eq!(
        searches.len(),
        2,
        "each impl declares its own search; got {paths:?}"
    );

    let alpha = index
        .get_symbol("Alpha::search")
        .expect("Alpha::search should be indexed under its impl");
    assert!(
        alpha
            .dependencies
            .iter()
            .any(|d| d == "looks_like_a_pattern"),
        "Alpha::search calls looks_like_a_pattern, so the call must be recorded; got {:?}",
        alpha.dependencies
    );

    let beta = index
        .get_symbol("Beta::search")
        .expect("Beta::search should be indexed under its impl");
    assert!(
        !beta
            .dependencies
            .iter()
            .any(|d| d == "looks_like_a_pattern"),
        "Beta::search does not call it; got {:?}",
        beta.dependencies
    );

    // The consequence the graph exists for: an agent asking what to run after
    // changing the free function has to be told about the test.
    assert_eq!(
        impacted(&index, "looks_like_a_pattern", 2),
        vec!["test_alpha_search_sees_a_pattern".to_string()],
        "the test reaches the free function through Alpha::search"
    );
}

#[test]
fn a_reference_is_charged_to_the_declaration_it_sits_under() {
    let dir = TempDir::new("cfg-twins");
    // Two declarations really do share one key here: the parser sees no `cfg`,
    // and a file-keyed symbol has nowhere to put the difference. What must not
    // happen is the first one's calls being charged to the symbol above it.
    dir.write(
        "retry.rs",
        "pub fn unrelated() -> bool { true }\n\
         \n\
         #[cfg(windows)]\n\
         pub fn worth_retrying() -> bool {\n\
         \x20   sharing_violation()\n\
         }\n\
         \n\
         #[cfg(unix)]\n\
         pub fn worth_retrying() -> bool {\n\
         \x20   permission_denied()\n\
         }\n\
         \n\
         pub fn sharing_violation() -> bool { true }\n\
         pub fn permission_denied() -> bool { false }\n",
    );
    dir.write(
        "retry_test.rs",
        "#[test]\nfn test_retry_decides() {\n    assert!(worth_retrying());\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let unrelated = index.get_symbol("unrelated").expect("unrelated is indexed");
    assert!(
        !unrelated
            .dependencies
            .iter()
            .any(|d| d == "sharing_violation"),
        "the call sits inside worth_retrying, not inside unrelated; got {:?}",
        unrelated.dependencies
    );

    assert_eq!(
        impacted(&index, "sharing_violation", 2),
        vec!["test_retry_decides".to_string()],
        "the call in the first of the two declarations still reaches the test"
    );
}

#[test]
fn a_lifetime_does_not_shift_every_line_below_it() {
    let dir = TempDir::new("lifetimes");
    // A lifetime opens with an apostrophe and never closes one. Skipping to the
    // next apostrophe in the file swallowed the newlines in between, so every
    // call below `Holder<'a>` was charged to the function above the one it sits
    // in: `first_caller` depended on `second_caller`, and the test that only
    // exercises the second was told to run for the first.
    dir.write(
        "holder.rs",
        "pub struct Holder<'a> {\n\
         \x20   pub inner: &'a str,\n\
         }\n\
         \n\
         pub fn first_caller(h: &Holder) -> bool {\n\
         \x20   only_alpha(h.inner)\n\
         }\n\
         \n\
         pub fn second_caller(h: &Holder) -> bool {\n\
         \x20   only_beta(h.inner)\n\
         }\n\
         \n\
         pub fn only_alpha(s: &str) -> bool {\n\
         \x20   s.starts_with(\"a\")\n\
         }\n\
         \n\
         pub fn only_beta(s: &str) -> bool {\n\
         \x20   s.starts_with(\"b\")\n\
         }\n",
    );
    dir.write(
        "holder_test.rs",
        "#[test]\nfn test_only_the_second() {\n    assert!(second_caller(&h));\n}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let first = index.get_symbol("first_caller").expect("first_caller");
    assert!(
        first.dependencies.contains(&"only_alpha".to_string())
            && !first.dependencies.contains(&"only_beta".to_string()),
        "first_caller calls only_alpha and not only_beta: {:?}",
        first.dependencies
    );

    let second = index.get_symbol("second_caller").expect("second_caller");
    assert!(
        second.dependencies.contains(&"only_beta".to_string())
            && !second.dependencies.contains(&"only_alpha".to_string()),
        "second_caller calls only_beta and not only_alpha: {:?}",
        second.dependencies
    );

    assert!(
        impacted(&index, "only_alpha", 3).is_empty(),
        "no test reaches only_alpha"
    );
    assert_eq!(
        impacted(&index, "only_beta", 2),
        vec!["test_only_the_second".to_string()]
    );
}

#[test]
fn a_python_string_is_not_a_call() {
    let dir = TempDir::new("py-quotes");
    // The same apostrophe means a string in Python, and one that is not
    // stripped puts every name inside it into the graph.
    dir.write("target.py", "def sensitive_thing():\n    return 1\n");
    dir.write(
        "caller.py",
        "def prints_a_name():\n    return 'sensitive_thing() is not called here'\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let caller = index.get_symbol("prints_a_name").expect("prints_a_name");
    assert!(
        !caller.dependencies.iter().any(|d| d == "sensitive_thing"),
        "the name is inside a string literal; got {:?}",
        caller.dependencies
    );
}

#[test]
fn a_generic_rust_function_and_type_are_indexed_properly() {
    let dir = TempDir::new("generics");
    dir.write(
        "lib.rs",
        "pub trait Parser<T> {\n\
             fn parse(&self) -> T;\n\
         }\n\
         pub struct Wrapper<T: Clone> {\n\
             pub item: T,\n\
         }\n\
         pub enum Status<E> {\n\
             Ok,\n\
             Err(E),\n\
         }\n\
         pub const fn make_const() -> u32 { 42 }\n\
         pub async fn fetch_data<T>(id: u64) -> Option<T> { None }\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    assert!(index.get_symbol("Parser").is_some(), "trait Parser indexed");
    assert!(index.get_symbol("Wrapper").is_some(), "struct Wrapper indexed");
    assert!(index.get_symbol("Status").is_some(), "enum Status indexed");
    assert!(index.get_symbol("make_const").is_some(), "const fn make_const indexed");
    assert!(index.get_symbol("fetch_data").is_some(), "generic async fn fetch_data indexed");
}

#[test]
fn python_class_scoping_resets_for_top_level_functions() {
    let dir = TempDir::new("py-scope");
    dir.write(
        "module.py",
        "class MyClass:\n\
         \x20   def method_one(self):\n\
         \x20       return 1\n\
         \n\
         def top_level_func():\n\
         \x20   return 2\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let symbols = index.symbol_paths();
    assert!(
        symbols.iter().any(|s| s.ends_with("::MyClass::method_one")),
        "method_one is scoped under MyClass: {symbols:?}"
    );
    assert!(
        symbols.iter().any(|s| s.ends_with("::top_level_func") && !s.contains("::MyClass::top_level_func")),
        "top_level_func is NOT scoped under MyClass: {symbols:?}"
    );
}

#[test]
fn typescript_generics_interfaces_types_enums_and_arrow_functions_are_indexed() {
    let dir = TempDir::new("ts-ast");
    dir.write(
        "service.ts",
        "export interface UserProfile<T> {\n\
         \x20 id: T;\n\
         }\n\
         export type Status = 'active' | 'inactive';\n\
         export enum Role { Admin, User }\n\
         export function parseResponse<R>(res: string): R {\n\
         \x20 return JSON.parse(res);\n\
         }\n\
         export const validateUser = (user: UserProfile<string>): boolean => {\n\
         \x20 return user.id.length > 0;\n\
         };\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let symbols = index.symbol_paths();
    assert!(index.get_symbol("UserProfile").is_some(), "interface UserProfile indexed: {symbols:?}");
    assert_eq!(index.get_symbol("UserProfile").unwrap().kind, "interface");

    assert!(index.get_symbol("Status").is_some(), "type Status indexed: {symbols:?}");
    assert_eq!(index.get_symbol("Status").unwrap().kind, "type");

    assert!(index.get_symbol("Role").is_some(), "enum Role indexed: {symbols:?}");
    assert_eq!(index.get_symbol("Role").unwrap().kind, "enum");

    assert!(index.get_symbol("parseResponse").is_some(), "generic fn parseResponse indexed: {symbols:?}");
    assert_eq!(index.get_symbol("parseResponse").unwrap().kind, "function");

    assert!(index.get_symbol("validateUser").is_some(), "arrow fn validateUser indexed: {symbols:?}");
    assert_eq!(index.get_symbol("validateUser").unwrap().kind, "function");
}

#[test]
fn cpp_classes_structs_namespaces_enums_and_functions_are_indexed() {
    let dir = TempDir::new("cpp-ast");
    dir.write(
        "engine.cpp",
        "#include <iostream>\n\
         namespace physics {\n\
         \x20   enum State { IDLE, RUNNING };\n\
         \x20   class RigidBody {\n\
         \x20   public:\n\
         \x20       void applyForce(float f) {\n\
         \x20       }\n\
         \x20   };\n\
         }\n\
         int computeTotal(int a, int b) {\n\
         \x20   return a + b;\n\
         }\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let symbols = index.symbol_paths();
    assert!(symbols.iter().any(|s| s.ends_with("::physics::State")), "enum physics::State indexed: {symbols:?}");
    assert!(symbols.iter().any(|s| s.ends_with("::physics::RigidBody")), "class physics::RigidBody indexed: {symbols:?}");
    assert!(symbols.iter().any(|s| s.ends_with("::physics::RigidBody::applyForce")), "method physics::RigidBody::applyForce indexed: {symbols:?}");
    assert!(symbols.iter().any(|s| s.ends_with("::computeTotal")), "function computeTotal indexed: {symbols:?}");
}

#[test]
fn java_records_sealed_classes_generic_returns_and_junit5_annotations_are_indexed() {
    let dir = TempDir::new("java-ast");
    dir.write(
        "ServiceSuite.java",
        "package com.example.service;\n\
         import java.util.Map;\n\
         public sealed class ServiceSuite permits ConcreteService {\n\
         \x20   public record TokenPair(String accessToken, String refreshToken) {}\n\
         \x20   public Map<String, TokenPair> getTokens() {\n\
         \x20       return Map.of();\n\
         \x20   }\n\
         \x20   @DisplayName(\"Token check\")\n\
         \x20   @ParameterizedTest\n\
         \x20   @ValueSource(strings = {\"token1\"})\n\
         \x20   public void verifyTokens(String token) {\n\
         \x20       assert token != null;\n\
         \x20   }\n\
         }\n\
         final class ConcreteService extends ServiceSuite {}\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let symbols = index.symbol_paths();
    assert!(index.get_symbol("com.example.service.ServiceSuite").is_some(), "sealed class ServiceSuite indexed: {symbols:?}");
    assert!(index.get_symbol("com.example.service.TokenPair").is_some(), "record TokenPair indexed: {symbols:?}");
    assert!(index.get_symbol("com.example.service.ServiceSuite::getTokens").is_some(), "method getTokens indexed: {symbols:?}");
    assert!(index.get_symbol("com.example.service.ServiceSuite::verifyTokens").is_some(), "test verifyTokens indexed: {symbols:?}");

    let verify_sym = index.get_symbol("com.example.service.ServiceSuite::verifyTokens").unwrap();
    assert_eq!(verify_sym.kind, "test", "verifyTokens identified as test from JUnit 5 annotations");
}

#[test]
fn java_nested_and_inner_classes_are_scoped_hierarchically() {
    let dir = TempDir::new("java-nested");
    dir.write(
        "Outer.java",
        "package com.example.model;\n\
         public class Outer {\n\
         \x20   public static class NestedConfig {\n\
         \x20       public String getEndpoint() { return \"localhost\"; }\n\
         \x20   }\n\
         \x20   public class InnerWorker {\n\
         \x20       public void doWork() {}\n\
         \x20   }\n\
         }\n",
    );

    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");

    let symbols = index.symbol_paths();
    assert!(index.get_symbol("com.example.model.Outer").is_some(), "Outer indexed: {symbols:?}");
    assert!(index.get_symbol("com.example.model.NestedConfig").is_some(), "NestedConfig indexed: {symbols:?}");
    assert!(index.get_symbol("com.example.model.NestedConfig::getEndpoint").is_some(), "NestedConfig::getEndpoint indexed: {symbols:?}");
    assert!(index.get_symbol("com.example.model.InnerWorker").is_some(), "InnerWorker indexed: {symbols:?}");
    assert!(index.get_symbol("com.example.model.InnerWorker::doWork").is_some(), "InnerWorker::doWork indexed: {symbols:?}");
}
