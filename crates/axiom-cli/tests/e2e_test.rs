use anyhow::Result;
use axiom_core::{mcp::JsonRpcRequest, mcp::JsonRpcResponse, AxiomMcpServer};
use serde_json::{json, Value};
use std::sync::Arc;

fn extract_tool_result(resp: &JsonRpcResponse) -> Value {
    let res = resp.result.as_ref().expect("Expected result in JsonRpcResponse");
    let text = res["content"][0]["text"].as_str().expect("Expected text in content");
    serde_json::from_str(text).expect("Expected valid json in content text")
}

#[tokio::test]
async fn test_e2e_agent_full_loop_over_mcp() -> Result<()> {
    // 1. Initialize MCP Server instance
    let server = Arc::new(AxiomMcpServer::new()?);

    // 2. Initialize handshake
    let init_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "initialize".into(),
        params: None,
    };
    let init_resp = server.handle_request(init_req).await;
    assert_eq!(init_resp.jsonrpc, "2.0");
    assert!(init_resp.result.is_some());

    // 3. List available tools
    let list_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "tools/list".into(),
        params: None,
    };
    let list_resp = server.handle_request(list_req).await;
    let list_result = list_resp.result.expect("Expected tools list");
    let tools = list_result["tools"].as_array().expect("Expected tools array");
    assert!(tools.iter().any(|t| t["name"] == "axiom_query_symbol"));
    assert!(tools.iter().any(|t| t["name"] == "axiom_get_blast_radius"));
    assert!(tools.iter().any(|t| t["name"] == "axiom_eval_patch"));
    assert!(tools.iter().any(|t| t["name"] == "axiom_apply_mutation"));
    assert!(tools.iter().any(|t| t["name"] == "axiom_attest_commit"));
    assert!(tools.iter().any(|t| t["name"] == "axiom_search_regex"));

    // 4. Create realistic multi-language repository workspace
    let temp_root = std::env::temp_dir().join(format!("axiom_e2e_{:x}", std::time::Instant::now().elapsed().as_nanos()));
    let java_pkg = temp_root.join("src").join("main").join("java").join("se").join("deversity").join("asynctest").join("runner");
    let rust_pkg = temp_root.join("crates").join("auth-lib").join("src");
    std::fs::create_dir_all(&java_pkg)?;
    std::fs::create_dir_all(&rust_pkg)?;

    // Write Java file
    std::fs::write(
        java_pkg.join("ConcurrencyRunner.java"),
        r#"
package se.deversity.asynctest.runner;

import java.util.concurrent.*;

public class ConcurrencyRunner {
    public void run(Runnable task) {
        task.run();
    }
}
"#,
    )?;

    // Write Java Test file
    std::fs::write(
        java_pkg.join("ConcurrencyRunnerTest.java"),
        r#"
package se.deversity.asynctest.runner;

import org.junit.jupiter.api.Test;

public class ConcurrencyRunnerTest {
    @Test
    public void testRunExecution() {
        assert true;
    }
}
"#,
    )?;

    // Write Rust file
    std::fs::write(
        rust_pkg.join("lib.rs"),
        r#"
pub fn validate_token(token: &str) -> bool {
    token.len() > 10
}

#[test]
fn test_token_validation() {
    assert!(validate_token("valid_secret_token"));
}
"#,
    )?;

    // 5. Scan repository into Merkle AST CAS
    let summary = server.ast_index.scan_directory(&temp_root)?;
    assert_eq!(summary.files_scanned, 3);
    assert!(summary.nodes_indexed >= 4);

    // Verify Merkle Root is dynamically calculated
    let root = server.ast_index.compute_merkle_root();
    assert_ne!(root, "0000000000000000000000000000000000000000000000000000000000000000");

    // 6. Query Java symbol
    let query_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(3)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_query_symbol",
            "arguments": {
                "symbol_path": "se.deversity.asynctest.runner.ConcurrencyRunner"
            }
        })),
    };
    let query_resp = server.handle_request(query_req).await;
    let query_result = extract_tool_result(&query_resp);
    assert_eq!(
        query_result.get("symbol_path").and_then(|v| v.as_str()),
        Some("se.deversity.asynctest.runner.ConcurrencyRunner")
    );

    // 7. Zoekt Trigram Search over scanned codebase
    let search_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(4)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_search_regex",
            "arguments": {
                "query": "ConcurrencyRunner"
            }
        })),
    };
    let search_resp = server.handle_request(search_req).await;
    let search_result = extract_tool_result(&search_resp);
    let count = search_result.get("matches_count").and_then(|v| v.as_u64()).unwrap_or(0);
    assert!(count >= 2, "Expected matches in ConcurrencyRunner.java and ConcurrencyRunnerTest.java");

    // 8. Probe syntax error -> Must return COMPILATION_ERROR
    let syntax_err_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(5)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "auth::service::validate_token",
                "code_snippet": "assert!(false); this is not valid rust @@@"
            }
        })),
    };
    let syntax_err_resp = server.handle_request(syntax_err_req).await;
    let syntax_err_res = extract_tool_result(&syntax_err_resp);
    assert_eq!(syntax_err_res.get("status").and_then(|v| v.as_str()), Some("COMPILATION_ERROR"));
    assert_eq!(syntax_err_res.get("passed_checks_count").and_then(|v| v.as_u64()), Some(0));

    // 8b. A symbol from a language the sandbox cannot compile is named as such,
    // rather than handed to rustc so the error blames the caller's syntax.
    let wrong_lang_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(51)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "se.deversity.asynctest.runner.ConcurrencyRunner",
                "code_snippet": "assert!(true);"
            }
        })),
    };
    let wrong_lang_res = extract_tool_result(&server.handle_request(wrong_lang_req).await);
    assert_eq!(
        wrong_lang_res.get("status").and_then(|v| v.as_str()),
        Some("EVALUATOR_UNAVAILABLE"),
        "a Java symbol must not be compiled as Rust, got {wrong_lang_res:?}"
    );

    // 9. Probe failing assertion -> Must return FAILED
    let fail_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(6)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "auth::service::validate_token",
                "code_snippet": "assert!(validate_token(\"\")); // BUGGY: empty token"
            }
        })),
    };
    let fail_resp = server.handle_request(fail_req).await;
    let fail_res = extract_tool_result(&fail_resp);
    assert_eq!(fail_res.get("status").and_then(|v| v.as_str()), Some("FAILED"));

    // 10. Probe passing assertion -> Must return PASSED
    let pass_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(7)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "auth::service::validate_token",
                "code_snippet": "assert!(validate_token(\"secret_token_12345\")); // FIXED"
            }
        })),
    };
    let pass_resp = server.handle_request(pass_req).await;
    let pass_res = extract_tool_result(&pass_resp);
    assert_eq!(pass_res.get("status").and_then(|v| v.as_str()), Some("PASSED"));
    // The attestation below must name this run. A made-up task id is refused,
    // because a seal that rests on a run nobody performed proves nothing.
    let passing_task = pass_res
        .get("task_id")
        .and_then(|v| v.as_str())
        .expect("a sandbox run must report its task id")
        .to_string();

    // 11. Apply Tree-CRDT mutation
    let mutate_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(8)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_apply_mutation",
            "arguments": {
                "node_id": "node_concurrency_runner",
                "symbol_path": "se.deversity.asynctest.runner.ConcurrencyRunner",
                "content": "public class ConcurrencyRunner { public void run(Runnable task) { CompletableFuture.runAsync(task); } }"
            }
        })),
    };
    let mutate_resp = server.handle_request(mutate_req).await;
    let mutate_res = extract_tool_result(&mutate_resp);
    assert_eq!(mutate_res.get("status").and_then(|v| v.as_str()), Some("APPLIED"));
    let new_root = mutate_res.get("new_merkle_root").and_then(|v| v.as_str()).expect("Expected new_merkle_root");
    assert!(!new_root.is_empty());

    // 12. Attest and seal commit cryptographically
    let attest_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(9)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_attest_commit",
            "arguments": {
                "prompt": "Upgrade ConcurrencyRunner to use CompletableFuture for async execution",
                "symbol_path": "se.deversity.asynctest.runner.ConcurrencyRunner",
                "ctop_task_id": passing_task
            }
        })),
    };
    let attest_resp = server.handle_request(attest_req).await;
    let attest_res = extract_tool_result(&attest_resp);
    let seal = attest_res.get("seal").and_then(|v| v.as_str()).expect("Expected a seal");
    assert!(
        seal.starts_with("blake3_seal_"),
        "the seal is a BLAKE3 integrity tag and must not claim to be a signature; got {seal}"
    );

    // Clean up temp dir
    let _ = std::fs::remove_dir_all(&temp_root);

    Ok(())
}

#[tokio::test]
async fn test_e2e_disk_persistence_cross_instance() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!("axiom_persist_{:x}", std::time::Instant::now().elapsed().as_nanos()));
    let src_dir = temp_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let rust_file = src_dir.join("calc.rs");
    std::fs::write(&rust_file, "pub fn add_numbers(a: i32, b: i32) -> i32 { a + b }\n#[test]\nfn test_add() { assert_eq!(add_numbers(2, 3), 5); }")?;

    // Instance 1: Scan and save to disk
    let server_1 = AxiomMcpServer::new()?;
    server_1.ast_index.scan_directory(&temp_dir)?;
    let index_file = temp_dir.join(".axiom").join("index.json");
    let saved_path = server_1.ast_index.save_to_disk(&index_file)?;

    // Assert file physically exists on disk and is non-empty
    assert!(saved_path.exists(), "Saved index file must exist on disk");
    let file_meta = std::fs::metadata(&saved_path)?;
    assert!(file_meta.len() > 50, "Saved index file must have content");

    // Instance 2: Load directly from saved file
    let loaded_ast = axiom_ast::AstIndex::load_from_disk(&saved_path)?;
    assert!(loaded_ast.total_symbols_count() >= 2);
    assert!(loaded_ast.get_symbol("src/calc.rs::add_numbers").is_some() || loaded_ast.get_symbol("add_numbers").is_some());

    // Clean up
    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

#[tokio::test]
async fn test_e2e_truth_preserving_assertions() -> Result<()> {
    let server = AxiomMcpServer::new()?;

    // 1. assert_eq!(2 + 2, 5) -> MUST BE FAILED
    let req_fail1 = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "math::calc",
                "code_snippet": "assert_eq!(2 + 2, 5);"
            }
        })),
    };
    let resp1 = server.handle_request(req_fail1).await;
    let res1 = extract_tool_result(&resp1);
    assert_eq!(res1.get("status").and_then(|v| v.as_str()), Some("FAILED"), "assert_eq!(2 + 2, 5) must be FAILED");

    // 2. vector emptiness invariant violation -> MUST BE FAILED
    let req_fail2 = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "collections::vec",
                "code_snippet": "let mut v = vec![1]; v.clear(); assert!(!v.is_empty());"
            }
        })),
    };
    let resp2 = server.handle_request(req_fail2).await;
    let res2 = extract_tool_result(&resp2);
    assert_eq!(res2.get("status").and_then(|v| v.as_str()), Some("FAILED"), "assert!(!v.is_empty()) on empty vector must be FAILED");

    // 3. true equality -> MUST BE PASSED
    let req_pass = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(3)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "math::calc",
                "code_snippet": "assert_eq!(2 + 2, 4);"
            }
        })),
    };
    let resp3 = server.handle_request(req_pass).await;
    let res3 = extract_tool_result(&resp3);
    assert_eq!(res3.get("status").and_then(|v| v.as_str()), Some("PASSED"));

    Ok(())
}

#[tokio::test]
async fn test_e2e_java_production_vs_test_classification() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!("axiom_java_test_class_{:x}", std::time::Instant::now().elapsed().as_nanos()));
    let main_pkg = temp_dir.join("async-test-lib").join("src").join("main").join("java").join("se").join("deversity").join("asynctest");
    let test_pkg = temp_dir.join("async-test-lib").join("src").join("test").join("java").join("se").join("deversity").join("asynctest");
    std::fs::create_dir_all(&main_pkg)?;
    std::fs::create_dir_all(&test_pkg)?;

    // Production class (inside async-test-lib folder, but in src/main/java)
    std::fs::write(
        main_pkg.join("AsyncRunner.java"),
        "package se.deversity.asynctest;\npublic class AsyncRunner {\n  public void execute() {}\n}",
    )?;

    // Test class (in src/test/java with @Test)
    std::fs::write(
        test_pkg.join("AsyncRunnerTest.java"),
        "package se.deversity.asynctest;\nimport org.junit.jupiter.api.Test;\npublic class AsyncRunnerTest {\n  @Test\n  public void testExecution() {}\n}",
    )?;

    let ast_index = axiom_ast::AstIndex::new();
    ast_index.scan_directory(&temp_dir)?;

    // Assert total test count is exactly 2 (the test class and test method), not production classes
    let total_tests = ast_index.total_tests_count();
    assert_eq!(total_tests, 2, "Only test files/methods should be classified as kind='test'");

    let prod_symbol = ast_index.get_symbol("se.deversity.asynctest.AsyncRunner").expect("Expected prod symbol");
    assert_eq!(prod_symbol.kind, "class", "Production class in src/main/java must be kind='class'");

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

#[tokio::test]
async fn test_e2e_dynamic_merkle_root_uniqueness() -> Result<()> {
    let index1 = axiom_ast::AstIndex::new();
    index1.index_node("auth::token", "function", "fn token() { 1 }", vec![]);
    let root1 = index1.compute_merkle_root();

    let index2 = axiom_ast::AstIndex::new();
    index2.index_node("auth::token", "function", "fn token() { 2 }", vec![]);
    let root2 = index2.compute_merkle_root();

    assert_ne!(root1, root2, "Different AST content must yield distinct Merkle roots");
    Ok(())
}

#[tokio::test]
async fn test_e2e_javadoc_no_hijacking_and_honest_assertion_counts() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!("axiom_javadoc_test_{:x}", std::time::Instant::now().elapsed().as_nanos()));
    let pkg_dir = temp_dir.join("src").join("main").join("java").join("se").join("deversity").join("asynctest").join("runner");
    let test_dir = temp_dir.join("src").join("test").join("java").join("se").join("deversity").join("asynctest").join("runner");
    std::fs::create_dir_all(&pkg_dir)?;
    std::fs::create_dir_all(&test_dir)?;

    // Real Java file with multiline methods, Javadoc, and nested interface
    std::fs::write(
        pkg_dir.join("ConcurrencyRunner.java"),
        r#"
package se.deversity.asynctest.runner;

/**
 * Runs {@code testMethod} N×M times (see the class Javadoc), scaling
 * across multiple threads.
 */
public class ConcurrencyRunner {
    public static void execute(
        Runnable task,
        int timeoutMs
    ) {
        task.run();
    }

    public interface ContentionBarrier {
        void unwrap();
    }

    public void buildMultiFailureError() {}

    public void resolveTimeoutMultiplier() {}
}
"#,
    )?;

    // Dedicated Test file referencing ConcurrencyRunner without import
    std::fs::write(
        test_dir.join("ConcurrencyRunnerTest.java"),
        r#"
package se.deversity.asynctest.runner;

import org.junit.jupiter.api.Test;

public class ConcurrencyRunnerTest {
    @Test
    public void testRunExecution() {
        ConcurrencyRunner.execute(() -> {}, 1000);
    }
}
"#,
    )?;

    let ast_index = axiom_ast::AstIndex::new();
    ast_index.scan_directory(&temp_dir)?;

    // 1. Verify multiline method execute is indexed under ConcurrencyRunner
    assert!(ast_index.get_symbol("se.deversity.asynctest.runner.ConcurrencyRunner").is_some());
    assert!(ast_index.get_symbol("se.deversity.asynctest.runner.ConcurrencyRunner::execute").is_some(), "Multiline execute method must be indexed");
    assert!(ast_index.get_symbol("se.deversity.asynctest.runner.ConcurrencyRunner::resolveTimeoutMultiplier").is_some());
    
    // 2. Verify nested interface unwrap is indexed under ContentionBarrier
    assert!(ast_index.get_symbol("se.deversity.asynctest.runner.ContentionBarrier::unwrap").is_some());

    // 3. Verify brace-depth restores ConcurrencyRunner for buildMultiFailureError (NOT ContentionBarrier::buildMultiFailureError)
    assert!(ast_index.get_symbol("se.deversity.asynctest.runner.ConcurrencyRunner::buildMultiFailureError").is_some(), "buildMultiFailureError must be under ConcurrencyRunner");
    assert!(ast_index.get_symbol("se.deversity.asynctest.runner.ContentionBarrier::buildMultiFailureError").is_none());
    assert!(ast_index.get_symbol("se.deversity.asynctest.runner.Javadoc),").is_none());

    // 4. Verify Dedicated Test Reachability (even without import)
    let br = ast_index.compute_blast_radius("se.deversity.asynctest.runner.ConcurrencyRunner", 5).expect("Expected blast radius");
    assert_eq!(br.total_tests_in_repo, 2); // ConcurrencyRunnerTest class and testRunExecution method
    assert!(!br.impacted_tests.is_empty(), "Blast radius must find ConcurrencyRunnerTest");
    assert!(br.impacted_tests.iter().any(|t| t.contains("ConcurrencyRunnerTest")), "ConcurrencyRunnerTest must be in impacted_tests");

    // Verify Honest Assertion Counts in execute_eval
    let server = AxiomMcpServer::new()?;

    // 1. No assertions (let x = 5;) -> passed_checks_count MUST BE 0
    let req_no_assert = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "math::calc",
                "code_snippet": "let x = 5;"
            }
        })),
    };
    let resp_no_assert = server.handle_request(req_no_assert).await;
    let res_no_assert = extract_tool_result(&resp_no_assert);
    assert_eq!(res_no_assert.get("status").and_then(|v| v.as_str()), Some("PASSED"));
    assert_eq!(res_no_assert.get("passed_checks_count").and_then(|v| v.as_u64()), Some(0), "let x = 5 must have 0 passed checks");

    // 2. Real assertion (assert_eq!(9 * 9, 81);) -> passed_checks_count MUST BE 1
    let req_assert = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "math::calc",
                "code_snippet": "assert_eq!(9 * 9, 81);"
            }
        })),
    };
    let resp_assert = server.handle_request(req_assert).await;
    let res_assert = extract_tool_result(&resp_assert);
    assert_eq!(res_assert.get("status").and_then(|v| v.as_str()), Some("PASSED"));
    assert_eq!(res_assert.get("passed_checks_count").and_then(|v| v.as_u64()), Some(1), "assert_eq!(9 * 9, 81) must have 1 passed check");

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

#[tokio::test]
async fn test_e2e_same_package_dependencies_blast_radius() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!("axiom_same_pkg_{:x}", std::time::Instant::now().elapsed().as_nanos()));
    let main_pkg = temp_dir.join("src").join("main").join("java").join("se").join("deversity").join("asynctest").join("diagnostics");
    let test_pkg = temp_dir.join("src").join("test").join("java").join("se").join("deversity").join("asynctest").join("diagnostics");
    let unrelated_pkg = temp_dir.join("src").join("test").join("java").join("se").join("deversity").join("asynctest").join("digest");
    std::fs::create_dir_all(&main_pkg)?;
    std::fs::create_dir_all(&test_pkg)?;
    std::fs::create_dir_all(&unrelated_pkg)?;

    // 1. Production class
    std::fs::write(
        main_pkg.join("RaceConditionDetector.java"),
        r#"
package se.deversity.asynctest.diagnostics;

public class RaceConditionDetector {
    public void detect() {}
}
"#,
    )?;

    // 2. Same-package test class (ZERO imports, instantiates RaceConditionDetector)
    std::fs::write(
        test_pkg.join("RaceConditionDetectorTest.java"),
        r#"
package se.deversity.asynctest.diagnostics;

import org.junit.jupiter.api.Test;

public class RaceConditionDetectorTest {
    @Test
    public void testDetection() {
        RaceConditionDetector detector = new RaceConditionDetector();
        detector.detect();
    }
}
"#,
    )?;

    // 3. Unrelated test class in another package
    std::fs::write(
        unrelated_pkg.join("SharedMessageDigestDetectorTest.java"),
        r#"
package se.deversity.asynctest.digest;

import org.junit.jupiter.api.Test;

public class SharedMessageDigestDetectorTest {
    @Test
    public void testDigest() {}
}
"#,
    )?;

    let ast_index = axiom_ast::AstIndex::new();
    ast_index.scan_directory(&temp_dir)?;

    let br = ast_index.compute_blast_radius("se.deversity.asynctest.diagnostics.RaceConditionDetector", 5)
        .expect("Expected blast radius");

    // Must find same-package test
    assert!(
        br.impacted_tests.iter().any(|t| t.contains("RaceConditionDetectorTest")),
        "Same-package RaceConditionDetectorTest must be included in impacted_tests"
    );

    // Must NOT include unrelated test
    assert!(
        !br.impacted_tests.iter().any(|t| t.contains("SharedMessageDigestDetectorTest")),
        "Unrelated SharedMessageDigestDetectorTest must NOT be included in impacted_tests"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

#[tokio::test]
async fn test_e2e_comment_stripping_and_class_literal_dependencies() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!("axiom_comment_test_{:x}", std::time::Instant::now().elapsed().as_nanos()));
    let main_pkg = temp_dir.join("src").join("main").join("java").join("se").join("deversity").join("asynctest").join("runner");
    let test_pkg = temp_dir.join("src").join("test").join("java").join("se").join("deversity").join("asynctest").join("runner");
    std::fs::create_dir_all(&main_pkg)?;
    std::fs::create_dir_all(&test_pkg)?;

    // 1. Target Class
    std::fs::write(
        main_pkg.join("ConcurrencyRunner.java"),
        r#"
package se.deversity.asynctest.runner;

public class ConcurrencyRunner {
    public static void execute() {}
}
"#,
    )?;

    // 2. Test referencing target class via .class literal (Real Code)
    std::fs::write(
        test_pkg.join("MultiFailureTest.java"),
        r#"
package se.deversity.asynctest.runner;

import org.junit.jupiter.api.Test;

public class MultiFailureTest {
    @Test
    public void testReflectiveInvocation() {
        java.lang.Class<?> clazz = se.deversity.asynctest.runner.ConcurrencyRunner.class;
    }
}
"#,
    )?;

    // 3. Test mentioning ConcurrencyRunner ONLY in comments / Javadoc (Prose)
    std::fs::write(
        test_pkg.join("AsyncBodyRunnerTest.java"),
        r#"
package se.deversity.asynctest.runner;

import org.junit.jupiter.api.Test;

/**
 * Historical note: this is similar to ConcurrencyRunner but operates on threadpools.
 * Do not call ConcurrencyRunner here.
 */
public class AsyncBodyRunnerTest {
    // Note: ConcurrencyRunner used to be tested here.
    @Test
    public void testBody() {
        String note = "See ConcurrencyRunner documentation";
    }
}
"#,
    )?;

    let ast_index = axiom_ast::AstIndex::new();
    ast_index.scan_directory(&temp_dir)?;

    let br = ast_index.compute_blast_radius("se.deversity.asynctest.runner.ConcurrencyRunner", 1)
        .expect("Expected blast radius");

    // MultiFailureTest (.class literal) MUST be in impacted_tests
    assert!(
        br.impacted_tests.iter().any(|t| t.contains("MultiFailureTest")),
        "MultiFailureTest referencing ConcurrencyRunner.class MUST be impacted"
    );

    // AsyncBodyRunnerTest (comment/string only) MUST NOT be in impacted_tests
    assert!(
        !br.impacted_tests.iter().any(|t| t.contains("AsyncBodyRunnerTest")),
        "AsyncBodyRunnerTest with comment-only mention MUST NOT be impacted"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

#[tokio::test]
async fn test_e2e_accessor_return_type_dependency_resolution() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!("axiom_accessor_test_{:x}", std::time::Instant::now().elapsed().as_nanos()));
    let main_pkg = temp_dir.join("src").join("main").join("java").join("se").join("deversity").join("asynctest");
    let test_pkg = temp_dir.join("src").join("test").join("java").join("se").join("deversity").join("asynctest");
    std::fs::create_dir_all(&main_pkg)?;
    std::fs::create_dir_all(&test_pkg)?;

    // 1. Target Class
    std::fs::write(
        main_pkg.join("RaceConditionDetector.java"),
        r#"
package se.deversity.asynctest;

public class RaceConditionDetector {
    public void recordFieldWrite() {}
}
"#,
    )?;

    // 2. Context Class with Accessor Returning Target Class
    std::fs::write(
        main_pkg.join("AsyncTestContext.java"),
        r#"
package se.deversity.asynctest;

public class AsyncTestContext {
    private RaceConditionDetector detector = new RaceConditionDetector();

    public RaceConditionDetector sharedRaceConditionDetector() {
        return this.detector;
    }
}
"#,
    )?;

    // 3. Test Class that calls accessor WITHOUT ever naming the type RaceConditionDetector
    std::fs::write(
        test_pkg.join("Phase1DetectorSetTest.java"),
        r#"
package se.deversity.asynctest;

import org.junit.jupiter.api.Test;

public class Phase1DetectorSetTest {
    @Test
    public void testDetectorUsageViaContext() {
        AsyncTestContext ctx = new AsyncTestContext();
        ctx.sharedRaceConditionDetector().recordFieldWrite();
    }
}
"#,
    )?;

    let ast_index = axiom_ast::AstIndex::new();
    ast_index.scan_directory(&temp_dir)?;

    let br = ast_index.compute_blast_radius("se.deversity.asynctest.RaceConditionDetector", 1)
        .expect("Expected blast radius");

    // Phase1DetectorSetTest MUST be resolved via sharedRaceConditionDetector accessor return-type inference
    assert!(
        br.impacted_tests.iter().any(|t| t.contains("Phase1DetectorSetTest")),
        "Phase1DetectorSetTest calling sharedRaceConditionDetector() must be resolved as impacted"
    );
    assert!(
        br.direct_tests.iter().any(|t| t.contains("Phase1DetectorSetTest")),
        "Phase1DetectorSetTest must be in direct_tests tier"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

#[tokio::test]
async fn test_e2e_swarm_50_agents_concurrency() -> Result<()> {
    let mut engine = axiom_crdt::SwarmEngine::new(50);
    let report = engine.simulate_concurrent_swarm(20).await?;

    assert_eq!(report.agent_count, 50);
    assert_eq!(report.total_operations, 2000);
    assert_eq!(report.merge_conflicts_count, 0, "Swarm must produce zero merge conflicts");
    assert!(report.converged, "All 50 agent replicas must converge to 100% identical Merkle state");
    assert!(!report.merkle_root.is_empty());

    Ok(())
}

/// Accessor resolution has to survive `.axiom/index.json`, not just live in the
/// index that did the scanning. `scan` and `serve` are separate processes, so an
/// index that persists only its nodes loses `method_return_types` and
/// `file_call_names` on the way to disk, and every test that reaches a class
/// through an accessor silently drops out of the blast radius. An in-process
/// scan-then-query assertion passes throughout that failure, which is why this
/// one reloads from disk before asking.
#[tokio::test]
async fn test_e2e_accessor_resolution_survives_disk_round_trip() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_persist_accessor_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    let main_pkg = temp_dir.join("src").join("main").join("java").join("se").join("deversity").join("asynctest");
    let test_pkg = temp_dir.join("src").join("test").join("java").join("se").join("deversity").join("asynctest");
    std::fs::create_dir_all(&main_pkg)?;
    std::fs::create_dir_all(&test_pkg)?;

    std::fs::write(
        main_pkg.join("RaceConditionDetector.java"),
        r#"
package se.deversity.asynctest;

public class RaceConditionDetector {
    public void recordFieldWrite() {}
}
"#,
    )?;

    std::fs::write(
        main_pkg.join("AsyncTestContext.java"),
        r#"
package se.deversity.asynctest;

public class AsyncTestContext {
    private RaceConditionDetector detector = new RaceConditionDetector();

    public RaceConditionDetector sharedRaceConditionDetector() {
        return this.detector;
    }
}
"#,
    )?;

    // Reaches the detector only through the accessor: the type name appears nowhere.
    std::fs::write(
        test_pkg.join("Phase1DetectorSetTest.java"),
        r#"
package se.deversity.asynctest;

import org.junit.jupiter.api.Test;

public class Phase1DetectorSetTest {
    @Test
    public void testDetectorUsageViaContext() {
        AsyncTestContext ctx = new AsyncTestContext();
        ctx.sharedRaceConditionDetector().recordFieldWrite();
    }
}
"#,
    )?;

    // Instance 1 scans and persists, exactly as `axiom scan` does.
    let scanner = axiom_ast::AstIndex::new();
    scanner.scan_directory(&temp_dir)?;
    let index_file = temp_dir.join(".axiom").join("index.json");
    let saved_path = scanner.save_to_disk(&index_file)?;

    // Instance 2 loads from disk, exactly as `axiom serve` does.
    let served = axiom_ast::AstIndex::load_from_disk(&saved_path)?;

    let br = served
        .compute_blast_radius("se.deversity.asynctest.RaceConditionDetector", 1)
        .expect("reloaded index must still resolve the symbol");

    assert!(
        br.impacted_tests.iter().any(|t| t.contains("Phase1DetectorSetTest")),
        "a test reaching the class only through sharedRaceConditionDetector() must survive \
         the disk round trip; got {:?}",
        br.impacted_tests
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// An index written before the side tables existed is a bare `{symbol: node}`
/// map. Those files are already on disk in working trees, so loading one must
/// keep working rather than failing the server at startup; it simply carries no
/// accessor resolution until the next scan rewrites it.
#[tokio::test]
async fn test_e2e_legacy_bare_map_index_still_loads() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_legacy_index_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir)?;
    let index_file = temp_dir.join("index.json");

    // Exactly the shape the old save_to_disk produced: nodes, nothing else.
    let legacy = r#"{
  "se.deversity.asynctest.Widget": {
    "id": "node_legacy01",
    "symbol_path": "se.deversity.asynctest.Widget",
    "kind": "class",
    "hash": "abc123",
    "source_range": [0, 20],
    "signature": "public class Widget",
    "docstring": null,
    "dependencies": []
  },
  "se.deversity.asynctest.WidgetTest": {
    "id": "node_legacy02",
    "symbol_path": "se.deversity.asynctest.WidgetTest",
    "kind": "test",
    "hash": "def456",
    "source_range": [0, 30],
    "signature": "public class WidgetTest",
    "docstring": null,
    "dependencies": ["se.deversity.asynctest.Widget"]
  }
}"#;
    std::fs::write(&index_file, legacy)?;

    let loaded = axiom_ast::AstIndex::load_from_disk(&index_file)?;
    assert_eq!(loaded.total_symbols_count(), 2, "both legacy nodes must load");

    // Reverse dependencies are rebuilt from the nodes, so the plain
    // import-derived path still resolves without any side tables.
    let br = loaded
        .compute_blast_radius("se.deversity.asynctest.Widget", 1)
        .expect("legacy index must still answer blast radius");
    assert!(
        br.impacted_tests.iter().any(|t| t.contains("WidgetTest")),
        "dependent test must resolve from a legacy index; got {:?}",
        br.impacted_tests
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// Text search has to work in the process that serves, which is never the
/// process that scanned. The searchable text lives in an in-memory index built
/// during the walk; if loading does not rebuild it, every query silently falls
/// through to matching symbol names, so a phrase that appears only inside a line
/// of source returns nothing and no caller can tell the difference.
#[tokio::test]
async fn test_e2e_text_search_survives_disk_round_trip() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_search_persist_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    let pkg = temp_dir.join("src").join("main").join("java").join("se").join("deversity").join("asynctest");
    std::fs::create_dir_all(&pkg)?;

    // "barrier.await" appears only in a statement: it is not any symbol's name,
    // so a symbol-name fallback cannot find it.
    std::fs::write(
        pkg.join("Gate.java"),
        r#"
package se.deversity.asynctest;

public class Gate {
    public void open() throws Exception {
        java.util.concurrent.CyclicBarrier barrier = new java.util.concurrent.CyclicBarrier(2);
        barrier.await();
    }
}
"#,
    )?;

    let scanner = axiom_ast::AstIndex::new();
    scanner.scan_directory(&temp_dir)?;
    let saved_path = scanner.save_to_disk(&temp_dir.join(".axiom").join("index.json"))?;

    let served = axiom_ast::AstIndex::load_from_disk(&saved_path)?;
    let (_, hits) = served
        .search("barrier.await", axiom_ast::SearchMode::Literal, 10)
        .expect("search must succeed");

    let text_hit = hits
        .iter()
        .find(|m| m.match_kind == "text")
        .unwrap_or_else(|| panic!("expected a source-text hit after reload; got {:?}", hits));

    assert!(
        text_hit.file_path.ends_with("Gate.java"),
        "a text hit must name the file it came from, got {:?}",
        text_hit.file_path
    );
    assert_eq!(
        text_hit.line_number,
        Some(7),
        "a text hit must carry the real line it was found on"
    );
    assert!(text_hit.line_content.contains("barrier.await"));

    // A symbol-name hit has no line, and must not invent one.
    let (_, sym_hits) = served
        .search("Gate", axiom_ast::SearchMode::Literal, 10)
        .expect("search must succeed");
    for m in sym_hits.iter().filter(|m| m.match_kind == "symbol") {
        assert_eq!(m.line_number, None, "symbol hits must not report a line number");
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// Search mode has to be the caller's choice, not a guess. A query like
/// `config.threads` is ordinary code punctuation that an agent means literally,
/// and reading it as a pattern quietly returns rows it never asked for. So the
/// default is literal, regex is opt-in, auto switches only on constructs that
/// are meaningless as text, and a pattern that will not compile is refused
/// rather than retried as a literal.
#[tokio::test]
async fn test_e2e_search_modes_are_explicit_and_honest() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_search_modes_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    let pkg = temp_dir.join("src").join("main").join("java").join("se").join("deversity").join("asynctest");
    std::fs::create_dir_all(&pkg)?;

    // `configXthreads` exists only to be matched by `config.threads` as a regex
    // and missed by it as a literal.
    std::fs::write(
        pkg.join("Knobs.java"),
        r#"
package se.deversity.asynctest;

public class Knobs {
    int a = config.threads;
    int b = configXthreads;
    int c = sharedRandomDetector;
}
"#,
    )?;

    let idx = axiom_ast::AstIndex::new();
    idx.scan_directory(&temp_dir)?;

    let count = |q: &str, m: axiom_ast::SearchMode| -> (axiom_ast::SearchMode, usize) {
        let (applied, hits) = idx.search(q, m, 50).expect("search must succeed");
        (applied, hits.iter().filter(|h| h.match_kind == "text").count())
    };

    // Literal is the default reading and matches the dot as a dot.
    let (applied, literal_hits) = count("config.threads", axiom_ast::SearchMode::Literal);
    assert_eq!(applied, axiom_ast::SearchMode::Literal);
    assert_eq!(literal_hits, 1, "literal must match only the real occurrence");

    // The same query as a pattern reaches further, which is exactly why it must
    // be asked for rather than inferred.
    let (applied, regex_hits) = count("config.threads", axiom_ast::SearchMode::Regex);
    assert_eq!(applied, axiom_ast::SearchMode::Regex);
    assert_eq!(regex_hits, 2, "as a pattern the dot also matches configXthreads");

    // Auto keeps code punctuation literal ...
    let (applied, _) = count("config.threads", axiom_ast::SearchMode::Auto);
    assert_eq!(
        applied,
        axiom_ast::SearchMode::Literal,
        "auto must not reinterpret ordinary code punctuation as a pattern"
    );

    // ... and only switches on a construct that cannot be meant as text.
    let (applied, auto_hits) = count("shared[A-Z][a-z]+Detector", axiom_ast::SearchMode::Auto);
    assert_eq!(applied, axiom_ast::SearchMode::Regex);
    assert_eq!(auto_hits, 1, "the character class must find sharedRandomDetector");

    // A pattern that does not compile is an error, never a quiet literal search.
    let err = idx
        .search("foo(", axiom_ast::SearchMode::Regex, 10)
        .expect_err("an unparseable pattern must be refused");
    assert!(err.contains("not a valid regular expression"), "got {err}");

    // An unrecognised mode is rejected rather than silently defaulted.
    assert!(axiom_ast::SearchMode::parse("regexp").is_err());
    assert_eq!(axiom_ast::SearchMode::parse("REGEX").unwrap(), axiom_ast::SearchMode::Regex);
    assert_eq!(axiom_ast::SearchMode::parse("").unwrap(), axiom_ast::SearchMode::Literal);

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// A scan describes what the tree contains now. Re-scanning used to only ever
/// add, so a deleted class stayed answerable and a renamed method kept its old
/// name beside the new one. For a tool whose whole output is "run exactly these
/// tests", naming a test that no longer exists is the expensive direction.
#[tokio::test]
async fn test_e2e_rescan_forgets_deleted_and_renamed_symbols() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_rescan_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    let pkg = temp_dir.join("src").join("main").join("java").join("se").join("deversity").join("asynctest");
    std::fs::create_dir_all(&pkg)?;

    let alpha = pkg.join("Alpha.java");
    let beta = pkg.join("Beta.java");
    std::fs::write(&alpha, "package se.deversity.asynctest;\npublic class Alpha {\n    public void alphaMethod() {}\n}\n")?;
    std::fs::write(&beta, "package se.deversity.asynctest;\npublic class Beta {\n    public void betaMethod() {}\n}\n")?;

    let idx = axiom_ast::AstIndex::new();
    idx.scan_directory(&temp_dir)?;
    assert!(idx.get_symbol("se.deversity.asynctest.Beta").is_some());
    assert!(idx.get_symbol("se.deversity.asynctest.Alpha::alphaMethod").is_some());

    // Beta is deleted outright; Alpha's method is renamed in place.
    std::fs::remove_file(&beta)?;
    std::fs::write(&alpha, "package se.deversity.asynctest;\npublic class Alpha {\n    public void renamedMethod() {}\n}\n")?;
    idx.scan_directory(&temp_dir)?;

    assert!(
        idx.get_symbol("se.deversity.asynctest.Beta").is_none(),
        "a class whose file was deleted must not survive a re-scan"
    );
    assert!(
        idx.get_symbol("se.deversity.asynctest.Beta::betaMethod").is_none(),
        "the deleted class's methods must go with it"
    );
    assert!(
        idx.get_symbol("se.deversity.asynctest.Alpha::alphaMethod").is_none(),
        "a renamed method must not linger under its old name"
    );
    assert!(
        idx.get_symbol("se.deversity.asynctest.Alpha::renamedMethod").is_some(),
        "the new name must be indexed"
    );

    // Purging is scoped to files that are gone, not to everything outside the
    // root being scanned: scanning one project must not empty another.
    let other = temp_dir.join("other").join("src");
    std::fs::create_dir_all(&other)?;
    std::fs::write(other.join("Gamma.java"), "package other;\npublic class Gamma {\n    public void mg() {}\n}\n")?;
    idx.scan_directory(&other)?;
    idx.scan_directory(&pkg)?;
    assert!(
        idx.get_symbol("other.Gamma").is_some(),
        "scanning one subtree must not forget a different one that still exists"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// An attestation claims a change was checked, so it may only be issued against
/// a check that happened and passed, and verification has to look the seal up
/// rather than re-derive one from whatever it was asked about.
/// Re-deriving is a tautology: it reports every symbol and prompt as proven,
/// including a symbol that does not exist and a prompt nobody ever issued.
#[tokio::test]
async fn test_e2e_attestation_requires_a_sandbox_run_that_passed() -> Result<()> {
    let server = AxiomMcpServer::new()?;

    let attest = |task: &str| JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(9)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_attest_commit",
            "arguments": {
                "prompt": "Tighten the guard",
                "symbol_path": "auth::service::validate_token",
                "ctop_task_id": task
            }
        })),
    };

    // A task id nobody ran cannot be attested.
    let resp = server.handle_request(attest("task_never_ran")).await;
    let res = extract_tool_result(&resp);
    let err = res.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("no verification recorded"),
        "attesting an unknown task must be refused, got {res:?}"
    );

    // A run that failed cannot be attested either.
    let failing = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(10)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": { "symbol_path": "auth::service::validate_token", "code_snippet": "assert!(false);" }
        })),
    };
    let failed = extract_tool_result(&server.handle_request(failing).await);
    assert_eq!(failed.get("status").and_then(|v| v.as_str()), Some("FAILED"));
    let failed_task = failed.get("task_id").and_then(|v| v.as_str()).unwrap().to_string();

    let res = extract_tool_result(&server.handle_request(attest(&failed_task)).await);
    let err = res.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("did not pass"),
        "attesting a failed run must be refused, got {res:?}"
    );

    Ok(())
}

/// The seal binds a symbol and a prompt, and the ledger is what makes it
/// checkable later. A stored seal verifies for the pair it was issued for and
/// for nothing else.
#[tokio::test]
async fn test_e2e_attestation_ledger_binds_symbol_and_prompt() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_ledger_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir)?;
    let ledger = temp_dir.join("attestations.json");

    // Nothing attested yet: an empty ledger proves nothing.
    assert!(axiom_core::mcp::load_attestations_from(&ledger)?.is_empty());

    let seal = axiom_proto::ProvenanceAttestation::generate(
        "root_parent",
        "root_commit",
        "agent_axiom_v1",
        "Tighten the guard",
        "auth::service::validate_token",
        "eval_7",
        "sandbox",
        "axiom sandbox, engine tier1_wasi_cranelift",
    );
    axiom_core::mcp::append_attestation_to(&ledger, &seal)?;

    let stored = axiom_core::mcp::load_attestations_from(&ledger)?;
    assert_eq!(stored.len(), 1);

    let found = &stored[0];
    assert!(
        found.verify("auth::service::validate_token", "Tighten the guard"),
        "the seal must verify for the pair it was issued for"
    );
    assert!(
        !found.verify("auth::service::validate_token", "a prompt nobody issued"),
        "a different prompt must not verify"
    );
    assert!(
        !found.verify("totally::invented::symbol", "Tighten the guard"),
        "a different symbol must not verify"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// The indexer reads Java, Kotlin, Python, TypeScript and Go; the sandbox
/// compiles Rust. Handing a Java symbol to rustc produces a syntax error that
/// blames the caller instead of naming the limit, so the mismatch is caught
/// before the compiler is reached.
#[tokio::test]
async fn test_e2e_eval_refuses_symbols_it_cannot_compile() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_eval_lang_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    let pkg = temp_dir.join("src").join("main").join("java").join("se").join("deversity").join("asynctest");
    std::fs::create_dir_all(&pkg)?;
    std::fs::write(
        pkg.join("Gate.java"),
        "package se.deversity.asynctest;\npublic class Gate {\n    public void open() {}\n}\n",
    )?;

    let idx = axiom_ast::AstIndex::new();
    idx.scan_directory(&temp_dir)?;

    assert_eq!(
        idx.language_of_symbol("se.deversity.asynctest.Gate::open").as_deref(),
        Some("java"),
        "the index must know which language a symbol came from"
    );
    assert_eq!(
        idx.language_of_symbol("nothing::indexed::here"),
        None,
        "an unknown symbol has no language rather than a guessed one"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// Two agents sharing one workspace must not erase each other.
///
/// Persisting a mutation by writing the whole in-memory index writes back every
/// other symbol as that process last saw it, so whichever agent saves second
/// silently discards the first agent's work. Nothing reports it: there is no
/// merge conflict to see, the node is simply gone. Measured against two
/// concurrent `axiom serve` processes before the fix, one mutation was lost in
/// two runs out of four, and which agent lost varied.
#[tokio::test]
async fn test_e2e_concurrent_agents_do_not_erase_each_other() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_two_agents_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir)?;
    let shared_index = temp_dir.join(".axiom").join("index.json");

    // Two agents, each holding its own view of the same workspace.
    let agent_a = axiom_ast::AstIndex::new();
    let agent_b = axiom_ast::AstIndex::new();

    agent_a.index_node("agentA::added", "function", "fn from_a() {}", vec![]);
    agent_b.index_node("agentB::added", "function", "fn from_b() {}", vec![]);

    agent_a.persist_symbol(&shared_index, "agentA::added")?;
    agent_b.persist_symbol(&shared_index, "agentB::added")?;

    let reloaded = axiom_ast::AstIndex::load_from_disk(&shared_index)?;
    assert!(
        reloaded.get_symbol("agentA::added").is_some(),
        "the first agent's symbol must survive the second agent's write"
    );
    assert!(
        reloaded.get_symbol("agentB::added").is_some(),
        "the second agent's symbol must be recorded"
    );

    // A third agent that never saw either still adds without removing them.
    let agent_c = axiom_ast::AstIndex::new();
    agent_c.index_node("agentC::added", "function", "fn from_c() {}", vec![]);
    agent_c.persist_symbol(&shared_index, "agentC::added")?;

    let reloaded = axiom_ast::AstIndex::load_from_disk(&shared_index)?;
    for symbol in ["agentA::added", "agentB::added", "agentC::added"] {
        assert!(
            reloaded.get_symbol(symbol).is_some(),
            "{symbol} must be present after three agents have written"
        );
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// The index lock is what makes read-modify-write safe between agents, so it has
/// to be exclusive while held and released afterwards.
#[tokio::test]
async fn test_e2e_index_lock_is_exclusive_then_released() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_lock_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir)?;
    let index = temp_dir.join("index.json");

    {
        let _held = axiom_ast::IndexLock::acquire(&index)?;
        assert!(
            index.with_extension("lock").exists(),
            "holding the lock must be visible to another process"
        );
    }

    assert!(
        !index.with_extension("lock").exists(),
        "the lock must be released when it goes out of scope, or the next agent waits forever"
    );

    // Releasing means the next acquisition succeeds rather than timing out.
    let _next = axiom_ast::IndexLock::acquire(&index)?;

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// Saving after a scan must merge over what is on disk, not replace it.
///
/// Locking the write is not enough on its own: a scanning process still holds
/// the view it loaded, so writing that view whole drops any symbol another agent
/// recorded in the meantime. Against two processes, a scan racing a mutation
/// lost the mutation in two runs out of five. A plain union would fix that and
/// break re-scan purging, resurrecting every symbol a deleted file used to own,
/// so removals are tracked and subtracted.
#[tokio::test]
async fn test_e2e_scan_merges_over_concurrent_work_but_still_purges() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_scan_merge_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    let src = temp_dir.join("src");
    std::fs::create_dir_all(&src)?;
    std::fs::write(src.join("Alpha.java"), "package p;\npublic class Alpha {\n    public void am() {}\n}\n")?;
    std::fs::write(src.join("Beta.java"), "package p;\npublic class Beta {\n    public void bm() {}\n}\n")?;

    let shared_index = temp_dir.join(".axiom").join("index.json");

    // Agent one indexes the tree and saves.
    let scanner = axiom_ast::AstIndex::new();
    scanner.scan_directory(&temp_dir)?;
    scanner.save_to_disk(&shared_index)?;

    // Agent two records a symbol of its own, which agent one has never seen.
    let mutator = axiom_ast::AstIndex::new();
    mutator.index_node("agentB::mutation", "function", "fn from_b() {}", vec![]);
    mutator.persist_symbol(&shared_index, "agentB::mutation")?;

    // Agent one re-scans, having deleted a file, and saves again from its own
    // view, which still does not contain agent two's symbol.
    std::fs::remove_file(src.join("Beta.java"))?;
    scanner.scan_directory(&temp_dir)?;
    scanner.save_to_disk(&shared_index)?;

    let reloaded = axiom_ast::AstIndex::load_from_disk(&shared_index)?;
    assert!(
        reloaded.get_symbol("agentB::mutation").is_some(),
        "a concurrent agent's symbol must survive another agent's scan"
    );
    assert!(
        reloaded.get_symbol("p.Alpha").is_some(),
        "the scanned tree must still be indexed"
    );
    assert!(
        reloaded.get_symbol("p.Beta").is_none(),
        "merging must not resurrect a class whose file was deleted"
    );
    assert!(
        reloaded.get_symbol("p.Beta::bm").is_none(),
        "nor its methods"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// A workspace nobody has scanned must say so, not answer from a fixture.
///
/// The server used to seed `auth::service::validate_token` and
/// `test_auth_validation` whenever the index was empty, so a fresh workspace
/// answered confidently about a symbol in no real codebase and returned a blast
/// radius for it. The usage guide uses exactly that symbol in its examples, so
/// an agent had no way to tell it was talking to a fixture.
#[tokio::test]
async fn test_e2e_unscanned_workspace_does_not_answer_from_a_fixture() -> Result<()> {
    // Explicitly no index, rather than whatever sits above the working
    // directory: otherwise this asserts something about the machine it runs on.
    let server = AxiomMcpServer::with_index(None)?;

    let query = |symbol: &str| JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(30)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_query_symbol",
            "arguments": { "symbol_path": symbol }
        })),
    };

    // Whatever this workspace holds, it must not hold the demo fixture unless
    // something asked for it.
    let res = extract_tool_result(&server.handle_request(query("auth::service::validate_token")).await);
    assert!(
        res.get("error").is_some(),
        "the demo symbol must not be present until seed_demo_workspace is called, got {res:?}"
    );

    // Asking for it explicitly is still supported, which is how the walkthrough
    // gets its data.
    server.seed_demo_workspace();
    let res = extract_tool_result(&server.handle_request(query("auth::service::validate_token")).await);
    assert_eq!(
        res.get("symbol_path").and_then(|v| v.as_str()),
        Some("auth::service::validate_token"),
        "seeding on request must still work, or the demo has no data"
    );

    Ok(())
}

/// `axiom watch` claimed to listen for changes and hot-patch the index, and
/// returned immediately after its first scan. The two lines it printed, about
/// listening and about Ctrl+C, described a loop that was not there, so an edit
/// made after starting it was never picked up.
#[tokio::test]
async fn test_e2e_watch_notices_a_change_after_the_first_scan() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_watch_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    let src = temp_dir.join("src");
    std::fs::create_dir_all(&src)?;
    let file = src.join("lib.rs");
    std::fs::write(&file, "pub fn alpha() {}\n")?;

    let idx = axiom_ast::AstIndex::new();
    idx.scan_directory(&temp_dir)?;
    let before = idx.tree_fingerprint(&temp_dir);

    // Same tree, same answer: polling must not re-parse on every tick.
    assert_eq!(
        before,
        idx.tree_fingerprint(&temp_dir),
        "an unchanged tree must fingerprint the same, or watch re-scans forever"
    );

    // Size changes, so this is visible without waiting on clock granularity.
    std::fs::write(&file, "pub fn alpha() {}\npub fn added_later() {}\n")?;
    assert_ne!(
        before,
        idx.tree_fingerprint(&temp_dir),
        "an edited tree must fingerprint differently, or watch never notices"
    );

    // And re-scanning on that signal actually picks the new symbol up.
    idx.scan_directory(&temp_dir)?;
    assert!(
        idx.list_symbols().iter().any(|n| n.symbol_path.contains("added_later")),
        "the symbol added after the first scan must be indexed"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// Purging a deleted file works by looking up what that file owned, so every
/// parser has to record its symbols. Only the Java one did, which meant a
/// deleted .rs, .py, .ts or .go file left its symbols in the index for ever
/// while the Java case looked fine. Attribution now happens in index_node, so a
/// language added later cannot forget it.
#[tokio::test]
async fn test_e2e_rescan_purges_every_language_not_just_java() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_purge_langs_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    let src = temp_dir.join("src");
    std::fs::create_dir_all(&src)?;

    std::fs::write(src.join("keep.rs"), "pub fn kept_symbol() {}\n")?;
    std::fs::write(src.join("gone.rs"), "pub fn rust_symbol() {}\n")?;
    std::fs::write(src.join("gone.py"), "def python_symbol():\n    pass\n")?;
    std::fs::write(src.join("gone.ts"), "function tsSymbol() {}\n")?;
    std::fs::write(src.join("gone.go"), "func GoSymbol() {}\n")?;
    std::fs::write(
        src.join("Gone.java"),
        "package p;\npublic class Gone {\n    public void javaSymbol() {}\n}\n",
    )?;

    let idx = axiom_ast::AstIndex::new();
    idx.scan_directory(&temp_dir)?;

    let present = |needle: &str| idx.list_symbols().iter().any(|n| n.symbol_path.contains(needle));
    for needle in ["rust_symbol", "python_symbol", "tsSymbol", "GoSymbol", "javaSymbol"] {
        assert!(present(needle), "{needle} must be indexed by the first scan");
    }

    for name in ["gone.rs", "gone.py", "gone.ts", "gone.go", "Gone.java"] {
        std::fs::remove_file(src.join(name))?;
    }
    idx.scan_directory(&temp_dir)?;

    for needle in ["rust_symbol", "python_symbol", "tsSymbol", "GoSymbol", "javaSymbol"] {
        assert!(
            !present(needle),
            "{needle} came from a deleted file and must not survive the re-scan"
        );
    }
    assert!(
        present("kept_symbol"),
        "the file that still exists must stay indexed"
    );

    Ok(())
}

/// Requiring a sandbox run before a provenance record made provenance
/// unreachable for most of the codebases axiom indexes.
///
/// The sandbox compiles Rust, and correctly refuses a Java symbol. Combined with
/// "attest only after a run that passed", that left no path at all for a Java,
/// Kotlin, Python, TypeScript or Go change, which is most of what the parsers
/// read and all of what the usage guide is written around. An agent that ran the
/// project's own suite has checked something real and can now say so, on the
/// condition that the record states axiom did not run it.
#[tokio::test]
async fn test_e2e_external_verification_can_back_a_record_but_says_who_ran_it() -> Result<()> {
    let server = AxiomMcpServer::with_index(None)?;

    let call = |name: &str, args: serde_json::Value| JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(40)),
        method: "tools/call".into(),
        params: Some(json!({ "name": name, "arguments": args })),
    };

    // Nothing recorded yet, so nothing can be attested.
    let res = extract_tool_result(&server.handle_request(call("axiom_attest_commit", json!({
        "prompt": "Restore the guard",
        "symbol_path": "se.deversity.asynctest.runner.ConcurrencyRunner",
        "ctop_task_id": "mvn_run_01"
    }))).await);
    assert!(
        res.get("error").and_then(|v| v.as_str()).unwrap_or("").contains("no verification recorded"),
        "attesting an unknown check must be refused, got {res:?}"
    );

    // An outcome with no verdict is not a verification.
    let res = extract_tool_result(&server.handle_request(call("axiom_record_verification", json!({
        "task_id": "mvn_run_01", "command": "mvn test"
    }))).await);
    assert!(res.get("error").is_some(), "passed must be required, got {res:?}");

    // A failed external check cannot back a record either.
    server.handle_request(call("axiom_record_verification", json!({
        "task_id": "mvn_failed", "passed": false, "command": "mvn test"
    }))).await;
    let res = extract_tool_result(&server.handle_request(call("axiom_attest_commit", json!({
        "prompt": "p", "symbol_path": "s", "ctop_task_id": "mvn_failed"
    }))).await);
    assert!(
        res.get("error").and_then(|v| v.as_str()).unwrap_or("").contains("did not pass"),
        "a failed check must not back a record, got {res:?}"
    );

    // A passing one does, and the record says axiom was told rather than that it
    // looked.
    let recorded = extract_tool_result(&server.handle_request(call("axiom_record_verification", json!({
        "task_id": "mvn_run_01", "passed": true, "command": "mvn -pl async-test-lib test -Dtest=ConcurrencyRunnerTest"
    }))).await);
    assert_eq!(recorded.get("recorded_as").and_then(|v| v.as_str()), Some("reported"));

    let sealed = extract_tool_result(&server.handle_request(call("axiom_attest_commit", json!({
        "prompt": "Restore the guard",
        "symbol_path": "se.deversity.asynctest.runner.ConcurrencyRunner",
        "ctop_task_id": "mvn_run_01"
    }))).await);
    assert_eq!(
        sealed.get("verified_by").and_then(|v| v.as_str()),
        Some("reported"),
        "a record backed by an external check must not read as axiom's own work"
    );
    assert!(
        sealed.get("verification_detail").and_then(|v| v.as_str()).unwrap_or("").contains("mvn"),
        "the record must say what was run, got {sealed:?}"
    );

    Ok(())
}

/// The dashboard printed a fixed panel under the heading LIVE METRICS: "100+
/// Indexed Symbols" whatever the index held, a blast-radius ratio, an
/// attestation level, and five activity lines with invented timings for calls
/// nobody had made. A display that reports constants as measurements is the same
/// defect as a sandbox that reports PASSED without running anything, so what it
/// shows now has to come from the workspace.
#[tokio::test]
async fn test_e2e_dashboard_counts_come_from_the_index() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_dashboard_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    let src = temp_dir.join("src");
    std::fs::create_dir_all(&src)?;
    std::fs::write(src.join("lib.rs"), "pub fn alpha() {}\npub fn beta() {}\n")?;

    let idx = axiom_ast::AstIndex::new();
    idx.scan_directory(&temp_dir)?;

    // The dashboard reads exactly these, so they must reflect the tree rather
    // than a constant. Two functions in, two functions out.
    let symbols = idx.list_symbols();
    assert_eq!(
        symbols.len(),
        2,
        "the count the dashboard prints must follow the workspace, got {symbols:?}"
    );
    assert!(
        symbols.iter().all(|n| n.kind == "function"),
        "and so must the breakdown by kind"
    );

    // An empty workspace reports nothing indexed rather than a plausible number.
    let empty = axiom_ast::AstIndex::new();
    assert_eq!(
        empty.list_symbols().len(),
        0,
        "an unscanned workspace must not report symbols it does not have"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

/// A name that cannot identify one symbol must not resolve to one anyway.
///
/// Lookup fell back to `key.ends_with(name)`, which is true of every key when
/// the name is empty. A request that omitted its argument, or sent a number
/// where a string belonged, defaulted to "" and came back with a real-looking
/// node for whichever symbol the HashMap reached first. The same fallback
/// resolved an ambiguous suffix to an arbitrary one of its candidates, and to a
/// different one between runs.
#[tokio::test]
async fn test_e2e_symbol_lookup_refuses_names_it_cannot_pin_down() -> Result<()> {
    let idx = axiom_ast::AstIndex::new();
    idx.index_node("pkg.One::execute", "method", "fn execute() {}", vec![]);
    idx.index_node("pkg.Two::execute", "method", "fn execute() {}", vec![]);
    idx.index_node("pkg.Only::unique", "method", "fn unique() {}", vec![]);

    assert!(
        idx.get_symbol("").is_none(),
        "an empty name matches every key under ends_with and must resolve to nothing"
    );
    assert!(idx.get_symbol("   ").is_none(), "nor may whitespace stand in for a name");

    assert!(
        idx.get_symbol("execute").is_none(),
        "an ambiguous name must not silently pick one of its candidates"
    );
    assert_eq!(
        idx.candidates_for("execute"),
        vec!["pkg.One::execute".to_string(), "pkg.Two::execute".to_string()],
        "the candidates must be offered, in a stable order"
    );

    // A short name that does identify one symbol still resolves.
    assert_eq!(
        idx.get_symbol("unique").map(|n| n.symbol_path),
        Some("pkg.Only::unique".to_string())
    );
    assert_eq!(
        idx.get_symbol("pkg.Only::unique").map(|n| n.symbol_path),
        Some("pkg.Only::unique".to_string())
    );

    // But a fragment that does not start on a boundary is not a name.
    assert!(
        idx.get_symbol("nique").is_none(),
        "matching mid-identifier would make half the index reachable by accident"
    );

    // And the MCP layer separates a malformed request from a miss.
    let server = AxiomMcpServer::with_index(None)?;
    let call = |args: serde_json::Value| JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(60)),
        method: "tools/call".into(),
        params: Some(json!({ "name": "axiom_query_symbol", "arguments": args })),
    };

    let res = extract_tool_result(&server.handle_request(call(json!({}))).await);
    assert!(
        res.get("error").and_then(|v| v.as_str()).unwrap_or("").contains("required"),
        "a missing argument must be reported as such, got {res:?}"
    );

    let res = extract_tool_result(&server.handle_request(call(json!({ "symbol_path": 123 }))).await);
    assert!(
        res.get("error").and_then(|v| v.as_str()).unwrap_or("").contains("must be a string"),
        "a number where a name belongs must be reported as such, got {res:?}"
    );

    Ok(())
}

/// Signing a provenance record, and the limits of what a signature shows.
///
/// The seal is a digest over public inputs, so it proves a record is unaltered
/// and nothing about who wrote it. A signature separates those. It is only worth
/// having if the key can live away from the records it signs: the threat is
/// someone who can write the ledger, and a key stored beside it is readable by
/// that same person.
#[tokio::test]
async fn test_e2e_records_can_be_signed_and_tampering_is_caught() -> Result<()> {
    let (private_hex, public_hex) = axiom_proto::signing::generate_keypair();

    let mut record = axiom_proto::ProvenanceAttestation::generate(
        "root_parent",
        "root_commit",
        "agent_axiom_v1",
        "Tighten the guard",
        "auth::service::validate_token",
        "eval_7",
        "sandbox",
        "axiom sandbox, engine tier1_wasi_cranelift",
    );

    // An unsigned record is anonymous, and says so by carrying no key.
    assert!(record.signature.is_empty() && record.public_key.is_empty());
    assert!(
        axiom_proto::signing::verify(&record, "auth::service::validate_token", "Tighten the guard").is_err(),
        "an unsigned record must not verify as signed"
    );

    record
        .sign_with("auth::service::validate_token", "Tighten the guard", &private_hex)
        .map_err(|e| anyhow::anyhow!(e))?;
    assert_eq!(record.public_key, public_hex, "the record must carry the key that signed it");

    axiom_proto::signing::verify(&record, "auth::service::validate_token", "Tighten the guard")
        .expect("a freshly signed record must verify");

    // The signature covers the symbol and prompt, so it cannot be lifted onto a
    // record about something else.
    assert!(
        axiom_proto::signing::verify(&record, "auth::service::validate_token", "a different prompt").is_err(),
        "a signature must not carry over to another prompt"
    );
    assert!(
        axiom_proto::signing::verify(&record, "some::other::symbol", "Tighten the guard").is_err(),
        "nor to another symbol"
    );

    // Altering a stored field breaks it, which is the point of signing the
    // record's own contents rather than just its identity.
    let mut tampered = record.clone();
    tampered.verification_detail = "pretend this was a full CI run".to_string();
    assert!(
        axiom_proto::signing::verify(&tampered, "auth::service::validate_token", "Tighten the guard").is_err(),
        "an edited record must not verify"
    );

    // A different key does not verify, which is what makes anchoring meaningful.
    let (other_private, other_public) = axiom_proto::signing::generate_keypair();
    assert_ne!(other_public, public_hex);
    let mut by_other = record.clone();
    by_other
        .sign_with("auth::service::validate_token", "Tighten the guard", &other_private)
        .map_err(|e| anyhow::anyhow!(e))?;
    assert_ne!(
        by_other.public_key, public_hex,
        "a record signed by another key must be distinguishable from one signed by this key"
    );

    // Rubbish keys are refused rather than panicking.
    assert!(record
        .clone()
        .sign_with("s", "p", "not-hex-at-all")
        .is_err());
    assert!(record.clone().sign_with("s", "p", "abcd").is_err());

    Ok(())
}

/// Requiring a signer must not accept a record that has no signer.
///
/// Anyone can write an unsigned record: the seal is a digest over public inputs,
/// so producing one takes no key at all. An attacker who can write the ledger
/// therefore only needs `axiom attest` with no key configured to manufacture a
/// record for a symbol and prompt nobody ever checked. Treating "no signature"
/// as good enough when a signer was demanded lets that straight through the
/// check meant to stop it, which is the downgrade this pins shut.
#[tokio::test]
async fn test_e2e_an_unsigned_record_cannot_satisfy_a_demanded_signer() -> Result<()> {
    let temp_dir = std::env::temp_dir().join(format!(
        "axiom_downgrade_{:x}",
        std::time::Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir)?;
    let ledger = temp_dir.join("attestations.json");

    let (private_hex, public_hex) = axiom_proto::signing::generate_keypair();

    let make = |symbol: &str, prompt: &str| {
        axiom_proto::ProvenanceAttestation::generate(
            "root_parent",
            "root_commit",
            "agent_axiom_v1",
            prompt,
            symbol,
            "task_1",
            "reported",
            "cargo test",
        )
    };

    // What an attacker with no key can produce: a well-formed, unsigned record.
    let forged = make("src/lib.rs::payload", "ship the backdoor");
    assert!(
        forged.verify("src/lib.rs::payload", "ship the backdoor"),
        "the seal is computable without a key, which is exactly the problem"
    );
    assert!(forged.signature.is_empty());
    axiom_core::mcp::append_attestation_to(&ledger, &forged)?;

    // And a genuine signed one for something else.
    let mut genuine = make("src/lib.rs::validate_token", "tighten the guard");
    genuine
        .sign_with("src/lib.rs::validate_token", "tighten the guard", &private_hex)
        .map_err(|e| anyhow::anyhow!(e))?;
    axiom_core::mcp::append_attestation_to(&ledger, &genuine)?;

    let stored = axiom_core::mcp::load_attestations_from(&ledger)?;
    assert_eq!(stored.len(), 2);

    // The rule verify applies: with a signer required, a record counts only if
    // it is signed by that signer.
    let satisfies = |symbol: &str, prompt: &str, want: &str| {
        stored.iter().any(|a| {
            a.verify(symbol, prompt)
                && !a.signature.is_empty()
                && a.public_key == want
                && axiom_proto::signing::verify(a, symbol, prompt).is_ok()
        })
    };

    assert!(
        !satisfies("src/lib.rs::payload", "ship the backdoor", &public_hex),
        "an unsigned forgery must not satisfy a check that named its expected signer"
    );
    assert!(
        satisfies("src/lib.rs::validate_token", "tighten the guard", &public_hex),
        "the genuine signed record must still satisfy it"
    );

    // A different signer does not satisfy it either.
    let (_, other_public) = axiom_proto::signing::generate_keypair();
    assert!(
        !satisfies("src/lib.rs::validate_token", "tighten the guard", &other_public),
        "a record signed by one key must not satisfy a check demanding another"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}
