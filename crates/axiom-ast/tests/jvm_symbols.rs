//! What the Java parser extracts from Kotlin and Scala.
//!
//! It reads all three languages, and until #21 it recognised only Java's shapes.
//! A Kotlin `fun` and a Scala `def` match no Java signature, so neither was
//! indexed: a JVM-language symbol was always a type and never a method. That
//! made `axiom_query_symbol` unable to answer about a method, forced the blast
//! radius to class granularity, and meant `axiom_eval_patch` could only be
//! reached by naming a type.
//!
//! The risk on the other side is why these tests exist at all. Loosening a match
//! in this parser has produced javadoc lines hijacking an enclosing class name,
//! `new Foo(...)` indexed as a method, `catch` clauses indexed as methods, and
//! machine-absolute paths written into symbol names. Every one of those is a way
//! this change could go wrong, so Java is pinned here alongside the new work.

use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("axiom-jvm-{}-{}-{}", tag, std::process::id(), n));
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

#[test]
fn a_kotlin_method_is_indexed_under_its_class() {
    let dir = TempDir::new("kotlin");
    dir.write(
        "Gate.kt",
        "package com.example\n\
         \n\
         class KotlinGate {\n\
         \x20   fun isOpen(depth: Int): Boolean = depth > 0\n\
         \x20   private fun helper(): Int {\n\
         \x20       return 1\n\
         \x20   }\n\
         }\n",
    );

    let index = scan(&dir);

    // The expression body is the point: `= depth > 0` opens no brace, and it is
    // the commonest shape in both languages.
    let open = index
        .get_symbol("isOpen")
        .expect("a Kotlin method must be indexed");
    assert_eq!(open.symbol_path, "com.example.KotlinGate::isOpen");
    assert_eq!(open.kind, "method");

    assert!(
        index.get_symbol("helper").is_some(),
        "a private method is still a method"
    );
}

#[test]
fn a_scala_method_is_indexed_under_its_object() {
    let dir = TempDir::new("scala");
    dir.write(
        "Gate.scala",
        "object ScalaGate {\n\
         \x20 def scalaIsOpen(depth: Int): Boolean = depth > 0\n\
         \x20 private def inner(x: Int): Int = x + 1\n\
         }\n",
    );

    let index = scan(&dir);

    let open = index
        .get_symbol("scalaIsOpen")
        .expect("a Scala method must be indexed");
    assert_eq!(open.symbol_path, "ScalaGate::scalaIsOpen");
    assert_eq!(open.kind, "method");
    assert!(index.get_symbol("inner").is_some());
}

/// Kotlin and Scala allow a definition with no enclosing type, which Java does
/// not. The owner falls back to the file stem, close to what Kotlin does itself:
/// a top-level `fun` in Gate.kt compiles into `GateKt`.
///
/// The assertion that matters is the second one. When this parser had an empty
/// owner before, it wrote machine-absolute file paths into symbol names, so a
/// symbol carrying a path separator or a drive letter is the specific regression
/// to catch.
#[test]
fn a_top_level_definition_is_owned_by_the_file_not_by_a_path() {
    let dir = TempDir::new("toplevel");
    dir.write(
        "Gate.kt",
        "package com.example\n\nfun topLevelGate(): Boolean = true\n",
    );
    dir.write(
        "Solo.scala",
        "@main def scalaTop(): Unit = println(\"hi\")\n",
    );

    let index = scan(&dir);

    let kotlin = index
        .get_symbol("topLevelGate")
        .expect("a top-level Kotlin function must be indexed");
    assert_eq!(kotlin.symbol_path, "com.example.Gate::topLevelGate");

    let scala = index
        .get_symbol("scalaTop")
        .expect("a Scala @main must be indexed");
    assert_eq!(scala.symbol_path, "Solo::scalaTop");

    for symbol in index.symbol_paths() {
        assert!(
            !symbol.contains('/') && !symbol.contains('\\'),
            "a JVM symbol must not carry a file path: {symbol}"
        );
    }
}

/// Java must not see the new keywords.
///
/// The change is gated on the file extension for this reason. `fun` and `def`
/// are ordinary identifiers in Java, so a variable called `def` or a method
/// called `fun` must not start declaring things, and `object` and `trait` are
/// not type keywords there either.
#[test]
fn java_is_unaffected_by_the_kotlin_and_scala_keywords() {
    let dir = TempDir::new("java");
    dir.write(
        "JavaGate.java",
        "package com.example;\n\
         \n\
         public class JavaGate {\n\
         \x20   public boolean isOpen(int depth) {\n\
         \x20       Object object = new Object();\n\
         \x20       int def = compute(depth);\n\
         \x20       return def > 0;\n\
         \x20   }\n\
         }\n",
    );

    let index = scan(&dir);

    assert!(
        index.get_symbol("isOpen").is_some(),
        "the Java method must still be indexed"
    );

    for symbol in index.symbol_paths() {
        let short = symbol.rsplit("::").next().unwrap_or(&symbol);
        assert_ne!(short, "object", "`object` is not a type keyword in Java");
        assert_ne!(
            short, "def",
            "`def` is a variable name here, not a declaration"
        );
        assert_ne!(
            short, "Object",
            "`new Object()` is a call site, not a declaration: {symbol}"
        );
    }
}

/// The shapes this parser has wrongly indexed as methods before, in a Kotlin
/// file now that Kotlin lines reach the method path.
#[test]
fn a_call_site_in_kotlin_is_not_indexed_as_a_definition() {
    let dir = TempDir::new("callsite");
    dir.write(
        "Caller.kt",
        "package com.example\n\
         \n\
         class Caller {\n\
         \x20   fun run() {\n\
         \x20       val gate = KotlinGate()\n\
         \x20       try {\n\
         \x20           gate.isOpen(1)\n\
         \x20       } catch (e: Exception) {\n\
         \x20       }\n\
         \x20   }\n\
         }\n",
    );

    let index = scan(&dir);
    let symbols = index.symbol_paths();

    assert!(
        symbols.iter().any(|s| s.ends_with("::run")),
        "the one real definition must be indexed: {symbols:?}"
    );

    for symbol in &symbols {
        let short = symbol.rsplit("::").next().unwrap_or(symbol);
        assert_ne!(short, "catch", "a catch clause is not a method: {symbol}");
        assert_ne!(
            short, "KotlinGate",
            "a constructor call is not a declaration: {symbol}"
        );
    }
}

/// A doc comment naming a function must not create one.
///
/// The comment-shaped failure this parser has had was a javadoc line hijacking
/// an enclosing class name; the same risk applies to the new keywords, since
/// `* fun something()` inside a KDoc block looks like a declaration to a
/// line-based matcher.
#[test]
fn a_doc_comment_mentioning_fun_does_not_declare_one() {
    let dir = TempDir::new("kdoc");
    dir.write(
        "Documented.kt",
        "package com.example\n\
         \n\
         /**\n\
         \x20* Calls fun ghostFunction(x: Int) when the gate is open.\n\
         \x20* See also def phantomDef(y: Int).\n\
         \x20*/\n\
         class Documented {\n\
         \x20   fun real(): Int = 1\n\
         }\n",
    );

    let index = scan(&dir);
    let symbols = index.symbol_paths();

    assert!(
        symbols.iter().any(|s| s.ends_with("::real")),
        "the real method must be indexed: {symbols:?}"
    );
    for ghost in ["ghostFunction", "phantomDef"] {
        assert!(
            !symbols.iter().any(|s| s.ends_with(&format!("::{ghost}"))),
            "a name mentioned in a comment is not a declaration: {symbols:?}"
        );
    }
}
