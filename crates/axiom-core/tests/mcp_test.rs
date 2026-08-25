use axiom_core::{AxiomMcpServer, mcp::JsonRpcRequest, mcp::JsonRpcResponse};
use serde_json::{Value, json};

fn extract_tool_result(resp: &JsonRpcResponse) -> Value {
    let res = resp
        .result
        .as_ref()
        .expect("Expected result in JsonRpcResponse");
    let text = res["content"][0]["text"]
        .as_str()
        .expect("Expected text in content");
    serde_json::from_str(text).expect("Expected valid json in content text")
}

#[tokio::test]
async fn test_mcp_initialize() {
    // Explicitly no ambient index: new() searches upwards from the working
    // directory, so a test using it asserts something about the machine it runs
    // on. This one passed locally only because a scan had left an index above
    // the repository, and failed on a clean checkout in CI.
    let server = AxiomMcpServer::with_index(None).expect("Failed to create MCP server");
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "initialize".into(),
        params: None,
    };

    let resp = server.handle_request(req).await;
    assert_eq!(resp.jsonrpc, "2.0");
    let result = resp.result.expect("initialize returns a result");

    // MCP puts server guidance in front of the model through this field, so the
    // agent does not have to learn the tool order by trial. It has to be there
    // and it has to name the loop.
    let instructions = result
        .get("instructions")
        .and_then(|v| v.as_str())
        .expect("initialize must carry instructions");
    for tool in [
        "axiom_query_symbol",
        "axiom_get_blast_radius",
        "axiom_eval_patch",
        "axiom_record_verification",
        "axiom_attest_commit",
    ] {
        assert!(
            instructions.contains(tool),
            "the instructions must name {tool}: {instructions:?}"
        );
    }
    // And it must say the index is a snapshot, or an agent trusts a stale read.
    assert!(
        instructions.contains("snapshot") || instructions.contains("scan"),
        "the instructions must say the index is a snapshot: {instructions:?}"
    );
}

#[tokio::test]
async fn test_mcp_tools_list() {
    // Explicitly no ambient index: new() searches upwards from the working
    // directory, so a test using it asserts something about the machine it runs
    // on. This one passed locally only because a scan had left an index above
    // the repository, and failed on a clean checkout in CI.
    let server = AxiomMcpServer::with_index(None).expect("Failed to create MCP server");
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "tools/list".into(),
        params: None,
    };

    let resp = server.handle_request(req).await;
    let res = resp.result.expect("Expected tools/list result");
    let tools = res
        .get("tools")
        .and_then(|t| t.as_array())
        .expect("Expected tools array");
    assert!(tools.len() >= 4);
}

#[tokio::test]
async fn test_mcp_blast_radius_valid_and_invalid() {
    // Explicitly no ambient index: new() searches upwards from the working
    // directory, so a test using it asserts something about the machine it runs
    // on. This one passed locally only because a scan had left an index above
    // the repository, and failed on a clean checkout in CI.
    let server = AxiomMcpServer::with_index(None).expect("Failed to create MCP server");
    server.seed_demo_workspace();

    // 1. Valid symbol
    let req_valid = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(3)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_get_blast_radius",
            "arguments": { "symbol_path": "auth::service::validate_token" }
        })),
    };
    let resp_valid = server.handle_request(req_valid).await;
    let res_v = extract_tool_result(&resp_valid);
    assert_eq!(
        res_v.get("symbol").and_then(|v| v.as_str()),
        Some("auth::service::validate_token")
    );

    // 2. Non-existent symbol must return error, NOT a fake 98.4%
    let req_invalid = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(4)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_get_blast_radius",
            "arguments": { "symbol_path": "non_existent::nonsense::symbol" }
        })),
    };
    let resp_invalid = server.handle_request(req_invalid).await;
    let res_inv = extract_tool_result(&resp_invalid);
    assert!(
        res_inv.get("error").is_some(),
        "Expected error on non-existent symbol"
    );
    assert_eq!(
        res_inv
            .get("pruned_test_percentage")
            .and_then(|v| v.as_f64()),
        Some(0.0)
    );
}

#[tokio::test]
async fn test_mcp_eval_syntax_error_and_assertion_failure() {
    // Explicitly no ambient index: new() searches upwards from the working
    // directory, so a test using it asserts something about the machine it runs
    // on. This one passed locally only because a scan had left an index above
    // the repository, and failed on a clean checkout in CI.
    let server = AxiomMcpServer::with_index(None).expect("Failed to create MCP server");

    // 1. Invalid syntax with @@@ must return CompilationError
    let req_syntax_err = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(5)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "test::func",
                "code_snippet": "assert!(false); this is not valid rust @@@"
            }
        })),
    };
    let resp_syntax = server.handle_request(req_syntax_err).await;
    let res_syn = extract_tool_result(&resp_syntax);
    assert_eq!(
        res_syn.get("status").and_then(|v| v.as_str()),
        Some("COMPILATION_ERROR")
    );
    assert_eq!(
        res_syn.get("passed_checks_count").and_then(|v| v.as_u64()),
        Some(0)
    );

    // 2. assert!(false) must return FAILED
    let req_fail = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(6)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "test::func",
                "code_snippet": "assert!(false);"
            }
        })),
    };
    let resp_fail = server.handle_request(req_fail).await;
    let res_f = extract_tool_result(&resp_fail);
    assert_eq!(res_f.get("status").and_then(|v| v.as_str()), Some("FAILED"));

    // 3. assert!(true) must return PASSED
    let req_pass = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(7)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_eval_patch",
            "arguments": {
                "symbol_path": "test::func",
                "code_snippet": "assert!(true);"
            }
        })),
    };
    let resp_pass = server.handle_request(req_pass).await;
    let res_p = extract_tool_result(&resp_pass);
    assert_eq!(res_p.get("status").and_then(|v| v.as_str()), Some("PASSED"));
}

#[tokio::test]
async fn test_mcp_java_symbol_indexing() {
    // Explicitly no ambient index: new() searches upwards from the working
    // directory, so a test using it asserts something about the machine it runs
    // on. This one passed locally only because a scan had left an index above
    // the repository, and failed on a clean checkout in CI.
    let server = AxiomMcpServer::with_index(None).expect("Failed to create MCP server");

    // Simulate indexing a Java source file
    let java_code = r#"
package se.deversity.asynctest.runner;

import java.util.concurrent.*;

public class ConcurrencyRunner {
    public void run(Runnable task) {
        task.run();
    }
}
"#;
    let temp_dir = std::env::temp_dir().join("axiom_java_test");
    let src_dir = temp_dir
        .join("src")
        .join("main")
        .join("java")
        .join("se")
        .join("deversity")
        .join("asynctest")
        .join("runner");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("ConcurrencyRunner.java"), java_code).unwrap();

    let summary = server.ast_index.scan_directory(&temp_dir).unwrap();
    assert!(
        summary.nodes_indexed >= 2,
        "Expected class and method indexed"
    );

    let req_query = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(8)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_query_symbol",
            "arguments": {
                "symbol_path": "se.deversity.asynctest.runner.ConcurrencyRunner"
            }
        })),
    };
    let resp_query = server.handle_request(req_query).await;
    let res_q = extract_tool_result(&resp_query);
    assert_eq!(
        res_q.get("symbol_path").and_then(|v| v.as_str()),
        Some("se.deversity.asynctest.runner.ConcurrencyRunner")
    );
}

#[tokio::test]
async fn test_mcp_mutation_search_and_attestation_loop() {
    let server = AxiomMcpServer::with_index(None).expect("Failed to create MCP server");

    // 1. Search text via axiom_search_regex
    let req_search = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(9)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_search_regex",
            "arguments": {
                "query": "validate_token",
                "mode": "literal"
            }
        })),
    };
    let resp_search = server.handle_request(req_search).await;
    let res_s = extract_tool_result(&resp_search);
    assert_eq!(
        res_s.get("mode_applied").and_then(|v| v.as_str()),
        Some("literal")
    );

    // 2. Apply mutation via axiom_apply_mutation
    let req_mutate = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(10)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_apply_mutation",
            "arguments": {
                "node_id": "node_auth_service",
                "symbol_path": "auth::service::validate_token",
                "content": "pub fn validate_token(token: &str) -> bool { !token.is_empty() }"
            }
        })),
    };
    let resp_mutate = server.handle_request(req_mutate).await;
    let res_m = extract_tool_result(&resp_mutate);
    assert_eq!(
        res_m.get("status").and_then(|v| v.as_str()),
        Some("APPLIED")
    );

    // 3. Record verification via axiom_record_verification
    let req_rec = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(11)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_record_verification",
            "arguments": {
                "task_id": "task_auth_verification_101",
                "passed": true,
                "command": "cargo test test_auth"
            }
        })),
    };
    let resp_rec = server.handle_request(req_rec).await;
    let res_r = extract_tool_result(&resp_rec);
    assert_eq!(res_r.get("passed").and_then(|v| v.as_bool()), Some(true));

    // 4. Attest commit via axiom_attest_commit
    let req_attest = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(12)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_attest_commit",
            "arguments": {
                "prompt": "Verify non-empty auth tokens",
                "symbol_path": "auth::service::validate_token",
                "ctop_task_id": "task_auth_verification_101",
                "agent_identity": "test-agent"
            }
        })),
    };
    let resp_attest = server.handle_request(req_attest).await;
    let res_a = extract_tool_result(&resp_attest);
    assert!(
        res_a.get("seal").is_some(),
        "Expected cryptographic seal on attestation"
    );
}
