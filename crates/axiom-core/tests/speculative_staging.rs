use axiom_core::{AxiomMcpServer, mcp::JsonRpcRequest};
use serde_json::json;

#[tokio::test]
async fn test_speculative_staging_lifecycle() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    server.seed_demo_workspace();

    // 1. Check initial symbol
    let query_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_query_symbol",
            "arguments": {
                "symbol_path": "auth::service::validate_token",
                "token_budget": 300
            }
        })),
    };
    let query_resp = server.handle_request(query_req).await;
    let text = query_resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(val.get("context_slice").is_some());

    // 2. Stage speculative mutation
    let stage_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_apply_mutation",
            "arguments": {
                "node_id": "spec_node_1",
                "symbol_path": "auth::service::validate_token",
                "content": "pub fn validate_token(t: &str) -> bool { true }",
                "speculative": true
            }
        })),
    };
    let stage_resp = server.handle_request(stage_req).await;
    let s_text = stage_resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
    let s_val: serde_json::Value = serde_json::from_str(&s_text).unwrap();
    assert_eq!(s_val["status"], "STAGED");
    assert_eq!(s_val["speculative"], true);

    // Verify it is held in staged_mutations
    assert_eq!(server.staged_mutations.read().unwrap().len(), 1);

    // 3. Rollback staged mutation
    let rollback_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(3)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_apply_mutation",
            "arguments": {
                "symbol_path": "auth::service::validate_token",
                "rollback_staged": true
            }
        })),
    };
    let rollback_resp = server.handle_request(rollback_req).await;
    let r_text = rollback_resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
    let r_val: serde_json::Value = serde_json::from_str(&r_text).unwrap();
    assert_eq!(r_val["status"], "ROLLED_BACK");
    assert_eq!(server.staged_mutations.read().unwrap().len(), 0);

    // 4. Stage again and Commit
    let stage_req2 = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(4)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_apply_mutation",
            "arguments": {
                "node_id": "spec_node_2",
                "symbol_path": "auth::service::validate_token",
                "content": "pub fn validate_token(t: &str) -> bool { t.starts_with(\"auth_\") }",
                "speculative": true
            }
        })),
    };
    server.handle_request(stage_req2).await;
    assert_eq!(server.staged_mutations.read().unwrap().len(), 1);

    let commit_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(5)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_apply_mutation",
            "arguments": {
                "symbol_path": "auth::service::validate_token",
                "commit_staged": true
            }
        })),
    };
    let commit_resp = server.handle_request(commit_req).await;
    let c_text = commit_resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
    let c_val: serde_json::Value = serde_json::from_str(&c_text).unwrap();
    assert_eq!(c_val["status"], "COMMITTED");
    assert!(c_val.get("new_merkle_root").is_some());
    assert_eq!(server.staged_mutations.read().unwrap().len(), 0);
}

#[tokio::test]
async fn test_mcp_blast_radius_causal_paths() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    server.seed_demo_workspace();

    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(10)),
        method: "tools/call".into(),
        params: Some(json!({
            "name": "axiom_get_blast_radius",
            "arguments": {
                "symbol_path": "auth::service::validate_token",
                "max_depth": 2
            }
        })),
    };
    let resp = server.handle_request(req).await;
    let text = resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert!(val.get("causal_paths").is_some());
    let paths = val["causal_paths"].as_object().unwrap();
    assert!(paths.contains_key("auth::test::test_validate_token"));
}

#[tokio::test]
async fn test_mcp_context_slice_resource() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    server.seed_demo_workspace();

    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(20)),
        method: "resources/read".into(),
        params: Some(json!({
            "uri": "axiom://slice/auth::service::validate_token?budget=200"
        })),
    };
    let resp = server.handle_request(req).await;
    assert!(resp.error.is_none());
    let text = resp.result.unwrap()["contents"][0]["text"].as_str().unwrap().to_string();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(val["symbol"], "auth::service::validate_token");
    assert!(val.get("rendered_slice").is_some());
}
