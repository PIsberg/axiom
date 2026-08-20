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
                "symbol_path": "se.deversity.asynctest.runner.ConcurrencyRunner",
                "code_snippet": "assert!(false); this is not valid rust @@@"
            }
        })),
    };
    let syntax_err_resp = server.handle_request(syntax_err_req).await;
    let syntax_err_res = extract_tool_result(&syntax_err_resp);
    assert_eq!(syntax_err_res.get("status").and_then(|v| v.as_str()), Some("COMPILATION_ERROR"));
    assert_eq!(syntax_err_res.get("passed_checks_count").and_then(|v| v.as_u64()), Some(0));

    // 9. Probe failing assertion -> Must return FAILED
    let fail_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(6)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "se.deversity.asynctest.runner.ConcurrencyRunner",
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
                "symbol_path": "se.deversity.asynctest.runner.ConcurrencyRunner",
                "code_snippet": "assert!(validate_token(\"secret_token_12345\")); // FIXED"
            }
        })),
    };
    let pass_resp = server.handle_request(pass_req).await;
    let pass_res = extract_tool_result(&pass_resp);
    assert_eq!(pass_res.get("status").and_then(|v| v.as_str()), Some("PASSED"));

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
                "ctop_task_id": "eval_pass_001"
            }
        })),
    };
    let attest_resp = server.handle_request(attest_req).await;
    let attest_res = extract_tool_result(&attest_resp);
    let signature = attest_res.get("signature").and_then(|v| v.as_str()).expect("Expected signature");
    assert!(signature.starts_with("ed25519_seal_"));

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
