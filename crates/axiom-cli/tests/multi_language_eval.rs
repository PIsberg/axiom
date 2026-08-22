//! Evaluating a snippet in the language the symbol was written in.
//!
//! The sandbox used to compile Rust and nothing else, so on a Java or Python
//! codebase the check step of the agent loop had nothing to run. These pin the
//! two halves of the replacement: a language with a toolchain gets a real
//! verdict, and a language without one refuses instead of guessing.
//!
//! Every case that needs an external toolchain asks whether it is there first.
//! A machine without `javac` still runs the assertion that matters, which is
//! that the answer is `EVALUATOR_UNAVAILABLE` and never `PASSED`.

use anyhow::Result;
use axiom_core::{mcp::JsonRpcRequest, mcp::JsonRpcResponse, AxiomMcpServer};
use axiom_vmm::native;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn extract_tool_result(resp: &JsonRpcResponse) -> Value {
    let res = resp.result.as_ref().expect("expected a result");
    let text = res["content"][0]["text"]
        .as_str()
        .expect("expected text content");
    serde_json::from_str(text).expect("expected json in the text content")
}

async fn eval(server: &AxiomMcpServer, symbol: &str, snippet: &str) -> Value {
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": { "symbol_path": symbol, "code_snippet": snippet }
        })),
    };
    extract_tool_result(&server.handle_request(req).await)
}

fn status(result: &Value) -> &str {
    result
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("<no status>")
}

fn engine(result: &Value) -> &str {
    result
        .get("engine")
        .and_then(|v| v.as_str())
        .unwrap_or("<no engine>")
}

fn error_types(result: &Value) -> Vec<String> {
    result
        .get("failed_checks")
        .and_then(|v| v.as_array())
        .map(|checks| {
            checks
                .iter()
                .filter_map(|c| c.get("error_type").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn toolchain_for(extension: &str) -> Option<&'static str> {
    native::language_for(extension).and_then(native::usable_toolchain)
}

/// A scanned workspace holding one file per language, so a single fixture can
/// answer questions about all of them.
fn polyglot_workspace() -> Result<(AxiomMcpServer, PathBuf)> {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!(
        "axiom_multilang_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root)?;

    std::fs::write(
        root.join("gate.py"),
        "def is_open(depth):\n    return depth > 0\n",
    )?;
    std::fs::write(
        root.join("legacy.py"),
        "def isOpen(depth):\n    return depth > 0\n",
    )?;
    std::fs::write(
        root.join("Gate.java"),
        "public class Gate {\n    public boolean isOpen(int depth) {\n        return depth > 0;\n    }\n}\n",
    )?;
    std::fs::write(
        root.join("gate.js"),
        "function jsIsOpen(depth) {\n  return depth > 0;\n}\nmodule.exports = { jsIsOpen };\n",
    )?;
    std::fs::write(
        root.join("gate.ts"),
        "export function tsIsOpen(depth: number): boolean {
  return depth > 0;
}
",
    )?;
    std::fs::write(
        root.join("Gate.kt"),
        "class KotlinGate {\n    fun isOpen(depth: Int): Boolean = depth > 0\n}\n",
    )?;
    std::fs::write(
        root.join("Gate.scala"),
        "object ScalaGate {\n  def scalaIsOpen(depth: Int): Boolean = depth > 0\n}\n",
    )?;

    // `with_index(None)` rather than `new()`: `new` climbs the directory tree
    // looking for an index, so a test written that way is really testing what
    // happens to be above the checkout.
    let server = AxiomMcpServer::with_index(None)?;
    server.ast_index.scan_directory(&root)?;
    Ok((server, root))
}

#[tokio::test]
async fn a_python_symbol_is_evaluated_by_python() -> Result<()> {
    let (server, root) = polyglot_workspace()?;

    let passing = eval(&server, "is_open", "assert 1 + 1 == 2").await;
    let failing = eval(&server, "is_open", "assert 1 + 1 == 3").await;

    // Holds whether or not python is installed: a snippet that was never run,
    // and a snippet that ran and failed, are both not a pass.
    assert_ne!(
        status(&failing),
        "PASSED",
        "a false assertion must never come back as a pass: {failing:?}"
    );

    match toolchain_for("py") {
        Some(program) => {
            assert_eq!(
                status(&passing),
                "PASSED",
                "{program} should have run this snippet: {passing:?}"
            );
            assert_eq!(engine(&passing), "tier2_native_python");
            assert_eq!(
                status(&failing),
                "FAILED",
                "the assertion is false and python says so: {failing:?}"
            );
            assert!(
                failing
                    .get("stderr")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .contains("AssertionError"),
                "the toolchain's own output should reach the caller: {failing:?}"
            );
        }
        None => {
            assert_eq!(
                status(&passing),
                "EVALUATOR_UNAVAILABLE",
                "no python on PATH, so nothing was run: {passing:?}"
            );
            assert_eq!(
                passing.get("passed_checks_count").and_then(|v| v.as_u64()),
                Some(0)
            );
            assert!(
                error_types(&passing).contains(&"EvaluatorUnavailable".to_string()),
                "{passing:?}"
            );
        }
    }

    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

#[tokio::test]
async fn a_java_assertion_is_checked_with_assertions_enabled() -> Result<()> {
    let (server, root) = polyglot_workspace()?;

    // `java` without -ea treats every `assert` as a no-op, so a false assertion
    // exits zero and the snippet looks like it passed. This is the case that
    // catches a missing -ea rather than a missing compiler.
    let passing = eval(&server, "Gate", "assert 2 + 2 == 4;").await;
    let failing = eval(&server, "Gate", "assert 2 + 2 == 5;").await;

    assert_ne!(
        status(&failing),
        "PASSED",
        "a false Java assertion must not pass; -ea missing would do exactly that: {failing:?}"
    );

    match toolchain_for("java") {
        Some(_) => {
            assert_eq!(status(&passing), "PASSED", "{passing:?}");
            assert_eq!(engine(&passing), "tier2_native_java");
            assert_eq!(status(&failing), "FAILED", "{failing:?}");
        }
        None => {
            assert_eq!(status(&passing), "EVALUATOR_UNAVAILABLE", "{passing:?}");
            assert!(
                passing
                    .get("failed_checks")
                    .map(|c| c.to_string().contains("javac"))
                    .unwrap_or(false),
                "the refusal should name the program that was looked for: {passing:?}"
            );
        }
    }

    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

#[tokio::test]
async fn a_javascript_symbol_is_evaluated_by_node() -> Result<()> {
    let (server, root) = polyglot_workspace()?;

    let failing = eval(&server, "jsIsOpen", "assert.strictEqual(1 + 1, 3);").await;
    assert_ne!(status(&failing), "PASSED", "{failing:?}");

    if toolchain_for("js").is_some() {
        let passing = eval(&server, "jsIsOpen", "assert.strictEqual(1 + 1, 2);").await;
        assert_eq!(status(&passing), "PASSED", "{passing:?}");
        assert_eq!(engine(&passing), "tier2_native_node");
        assert_eq!(status(&failing), "FAILED", "{failing:?}");
    } else {
        assert_eq!(status(&failing), "EVALUATOR_UNAVAILABLE", "{failing:?}");
    }

    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

/// Every language the indexer parses has an evaluator.
///
/// These two lists are edited in different files and nothing made them agree:
/// `parse_by_language` in axiom-ast decides what gets indexed, and `LANGUAGES`
/// in axiom-vmm decides what can be run. Kotlin and Scala sat on the first list
/// and not the second for as long as the tier existed, which is what #4 and #16
/// were about. Adding a parser without an evaluator should fail here rather than
/// surface later as a refusal an agent cannot act on.
///
/// Rust is the exception on purpose: it is compiled by tier 1, not by this tier.
#[test]
fn every_indexed_language_has_an_evaluator() {
    // Mirrors the match arms of parse_by_language.
    let indexed = ["java", "kt", "scala", "py", "ts", "js", "go"];

    for extension in indexed {
        assert!(
            native::language_for(extension).is_some(),
            "the indexer parses .{extension} files, so a symbol from one can be \
             asked about, and this tier has no way to run it"
        );
    }

    assert!(
        native::language_for("rs").is_none(),
        "Rust belongs to tier 1; a recipe here would race the rustc path"
    );
}

/// An extension with no recipe is still refused rather than handed to whichever
/// compiler is nearest.
///
/// The refusal path is no longer reachable through an indexed symbol, since
/// every parsed language now has an evaluator. It is still the behaviour that
/// matters if one is added, and handing Kotlin to javac was the specific thing
/// worth not doing: the error would have been filed against the snippet rather
/// than against the language.
#[test]
fn an_extension_with_no_recipe_is_not_borrowed_from_another() {
    assert!(native::language_for("rb").is_none());
    assert!(native::language_for("").is_none());

    // Script variants reach the compiler that reads them, and nothing else.
    assert_eq!(native::language_for("kts").map(|l| l.extension), Some("kt"));
    assert_eq!(
        native::language_for("sc").map(|l| l.extension),
        Some("scala")
    );
}

/// Kotlin, which #16 was about, and the trap it shares with Java.
///
/// Kotlin's `assert` compiles to a check of the JVM's assertion status for the
/// enclosing class, exactly as Java's does. Without `-ea` a false assertion is a
/// no-op: measured here before the recipe was written, `assert(1 + 1 == 3)`
/// printed the line after it and exited zero, which this tier would have
/// reported as PASSED. So the assertion that matters is the failing one.
#[tokio::test]
async fn a_kotlin_assertion_is_checked_with_assertions_enabled() -> Result<()> {
    let (server, root) = polyglot_workspace()?;

    let failing = eval(&server, "KotlinGate", "assert(1 + 1 == 3)").await;

    assert_ne!(
        status(&failing),
        "PASSED",
        "a false Kotlin assertion must never come back as a pass; without -ea it \
         is a no-op and the snippet exits zero: {failing:?}"
    );

    match toolchain_for("kt") {
        Some(program) => {
            assert_eq!(
                status(&failing),
                "FAILED",
                "{program} should have run this and seen the assertion fail: {failing:?}"
            );
            assert_eq!(engine(&failing), "tier2_native_kotlin");

            let passing = eval(&server, "KotlinGate", "assert(1 + 1 == 2)").await;
            assert_eq!(status(&passing), "PASSED", "{passing:?}");
        }
        None => {
            assert_eq!(status(&failing), "EVALUATOR_UNAVAILABLE", "{failing:?}");
            assert!(
                error_types(&failing).contains(&"EvaluatorUnavailable".to_string()),
                "{failing:?}"
            );
        }
    }

    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

/// Scala, where the same question has the opposite answer.
///
/// `assert` here is `Predef.assert`, which throws unconditionally rather than
/// compiling to a JVM assertion check, so no `-ea` is needed and none is passed.
/// Worth a test of its own precisely because it differs from Java and Kotlin: a
/// recipe copied from either would carry a flag that does nothing, and one
/// copied the other way would lose a flag that matters.
#[tokio::test]
async fn a_scala_assertion_fails_without_needing_an_assertion_flag() -> Result<()> {
    let (server, root) = polyglot_workspace()?;

    let failing = eval(&server, "ScalaGate", "assert(1 + 1 == 3)").await;

    assert_ne!(
        status(&failing),
        "PASSED",
        "a false Scala assertion must never come back as a pass: {failing:?}"
    );

    match toolchain_for("scala") {
        Some(program) => {
            assert_eq!(
                status(&failing),
                "FAILED",
                "{program} should have run this and seen the assertion fail: {failing:?}"
            );
            assert_eq!(engine(&failing), "tier2_native_scala");
            assert!(
                failing
                    .get("stderr")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .contains("AssertionError"),
                "the toolchain's own output should reach the caller: {failing:?}"
            );

            let passing = eval(&server, "ScalaGate", "assert(1 + 1 == 2)").await;
            assert_eq!(status(&passing), "PASSED", "{passing:?}");
        }
        None => {
            assert_eq!(status(&failing), "EVALUATOR_UNAVAILABLE", "{failing:?}");
            assert!(
                error_types(&failing).contains(&"EvaluatorUnavailable".to_string()),
                "{failing:?}"
            );
        }
    }

    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

#[tokio::test]
async fn the_language_is_taken_from_the_symbol_not_from_the_spelling() -> Result<()> {
    let (server, root) = polyglot_workspace()?;

    // `KotlinGate` is the short name; the key it was indexed under carries the
    // path. Matching the caller's spelling against the stored keys found
    // nothing, and "no language known" was treated as "assume Rust", which is
    // how a Kotlin symbol reached rustc.
    // Rust syntax, deliberately. Kotlin now has an evaluator, so what this
    // pins is no longer "refused" but "refused by the right tier": kotlinc
    // rejects `assert!(true);` where rustc would have accepted it, and a pass
    // here would mean the snippet was compiled as the wrong language.
    let short = eval(&server, "KotlinGate", "assert!(true);").await;
    assert_ne!(
        engine(&short),
        "tier1_wasi_cranelift",
        "a Kotlin symbol must not be answered by the Rust tier: {short:?}"
    );
    assert_ne!(
        status(&short),
        "PASSED",
        "this is not valid Kotlin, so a pass means it was compiled as something          else: {short:?}"
    );

    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

#[tokio::test]
async fn an_ambiguous_name_is_not_resolved_by_picking_a_compiler() -> Result<()> {
    let (server, root) = polyglot_workspace()?;

    // `isOpen` is a method on the Java class and on the Kotlin one. With no
    // single symbol there is no single language, and the fallback for "no
    // language" was Rust, so the caller got a rustc error about a name it never
    // wrote in Rust.
    let result = eval(&server, "isOpen", "assert 2 + 2 == 4;").await;

    assert_eq!(status(&result), "EVALUATOR_UNAVAILABLE", "{result:?}");
    assert!(
        error_types(&result).contains(&"AmbiguousSymbol".to_string()),
        "{result:?}"
    );
    assert!(
        result
            .get("candidates")
            .and_then(|c| c.as_array())
            .map(|c| c.len() > 1)
            .unwrap_or(false),
        "the caller needs the candidates to pick from: {result:?}"
    );
    assert_ne!(engine(&result), "tier1_wasi_cranelift", "{result:?}");

    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

/// TypeScript, which #9 was opened about: the recipe shipped without either
/// toolchain installed on the machine that reviewed it, so the running branch
/// had never executed and this test asserted nothing anywhere.
///
/// The assertion style is the measured part. Neither recipe injects a prelude,
/// and they do not offer the same environment: `import assert from
/// "node:assert"` runs under deno and is a type error under tsc, which has no
/// @types/node, so the same snippet passes on one machine and comes back as a
/// compilation error on another. A bare `throw` needs nothing from either. This
/// test uses it for that reason, and the guide tells callers the same thing.
#[tokio::test]
async fn a_typescript_symbol_is_evaluated_by_its_toolchain() -> Result<()> {
    let (server, root) = polyglot_workspace()?;

    let passing = eval(
        &server,
        "tsIsOpen",
        "const n: number = 1 + 1;\nif (n !== 2) { throw new Error(`expected 2, got ${n}`); }",
    )
    .await;
    let failing = eval(
        &server,
        "tsIsOpen",
        "const n: number = 1 + 1;\nif (n !== 3) { throw new Error(`expected 3, got ${n}`); }",
    )
    .await;

    // True with or without a toolchain, which is the assertion that matters:
    // a snippet that was never run and one that ran and threw are both not a
    // pass.
    assert_ne!(
        status(&failing),
        "PASSED",
        "a snippet that throws must never come back as a pass: {failing:?}"
    );

    match toolchain_for("ts") {
        Some(program) => {
            assert_eq!(
                status(&passing),
                "PASSED",
                "{program} should have run this snippet: {passing:?}"
            );
            assert_eq!(engine(&passing), "tier2_native_typescript");
            assert_eq!(
                status(&failing),
                "FAILED",
                "the snippet throws and the toolchain says so: {failing:?}"
            );
            assert!(
                failing
                    .get("stderr")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .contains("expected 3"),
                "the toolchain's own output should reach the caller: {failing:?}"
            );
            // A pass with nothing counted reads as a pass nothing checked.
            assert!(
                passing
                    .get("passed_checks_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    > 0,
                "the documented assertion style must be counted: {passing:?}"
            );
        }
        None => {
            assert_eq!(
                status(&passing),
                "EVALUATOR_UNAVAILABLE",
                "no deno and no tsc on PATH, so nothing was run: {passing:?}"
            );
            assert_eq!(
                passing.get("passed_checks_count").and_then(|v| v.as_u64()),
                Some(0)
            );
            assert!(
                error_types(&passing).contains(&"EvaluatorUnavailable".to_string()),
                "{passing:?}"
            );
        }
    }

    std::fs::remove_dir_all(&root).ok();
    Ok(())
}
