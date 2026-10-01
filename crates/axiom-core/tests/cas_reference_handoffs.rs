use axiom_core::AxiomMcpServer;
use axiom_core::mcp::JsonRpcRequest;
use serde_json::json;

#[tokio::test]
async fn test_query_symbol_returns_cas_ref_and_uri() {
    let server = AxiomMcpServer::new().unwrap();
    server.seed_demo_workspace();

    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "axiom_query_symbol",
            "arguments": {
                "symbol_path": "auth::service::validate_token"
            }
        })),
    };

    let resp = server.handle_request(req).await;
    let res = resp.result.expect("result must be present");
    let text = res["content"][0]["text"].as_str().unwrap();
    let val: serde_json::Value = serde_json::from_str(text).unwrap();

    let cas_ref = val["cas_ref"].as_str().expect("cas_ref must be present");
    let cas_uri = val["cas_uri"].as_str().expect("cas_uri must be present");
    let hash = val["hash"].as_str().expect("hash must be present");

    assert_eq!(cas_ref, format!("auth::service::validate_token@{hash}"));
    assert_eq!(
        cas_uri,
        format!("axiom://slice/auth::service::validate_token#{hash}")
    );
}

#[tokio::test]
async fn test_query_symbol_resolves_valid_cas_ref_pointer() {
    let server = AxiomMcpServer::new().unwrap();
    server.seed_demo_workspace();

    let node = server
        .ast_index
        .get_symbol("auth::service::validate_token")
        .unwrap();
    let pointer = format!("auth::service::validate_token@{}", node.hash);

    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "axiom_query_symbol",
            "arguments": {
                "symbol_path": pointer
            }
        })),
    };

    let resp = server.handle_request(req).await;
    let res = resp.result.expect("result must be present");
    let text = res["content"][0]["text"].as_str().unwrap();
    let val: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(val["symbol_path"], "auth::service::validate_token");
    assert_eq!(val["hash"], node.hash);
}

#[tokio::test]
async fn test_query_symbol_detects_stale_cas_pointer_drift() {
    let server = AxiomMcpServer::new().unwrap();
    server.seed_demo_workspace();

    let stale_pointer = "auth::service::validate_token@0000000000000000000000000000000000000000000000000000000000000000";

    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "axiom_query_symbol",
            "arguments": {
                "symbol_path": stale_pointer
            }
        })),
    };

    let resp = server.handle_request(req).await;
    let res = resp.result.expect("result must be present");
    let text = res["content"][0]["text"].as_str().unwrap();
    let val: serde_json::Value = serde_json::from_str(text).unwrap();

    let error_msg = val["error"].as_str().unwrap();
    assert!(error_msg.contains("CAS reference hash mismatch"));
    assert_eq!(
        val["expected_hash"],
        "0000000000000000000000000000000000000000000000000000000000000000"
    );
}

#[tokio::test]
async fn test_resource_read_verifies_cas_hash_integrity() {
    let server = AxiomMcpServer::new().unwrap();
    server.seed_demo_workspace();

    let node = server
        .ast_index
        .get_symbol("auth::service::validate_token")
        .unwrap();

    // Valid URI with matching hash
    let valid_uri = format!("axiom://slice/auth::service::validate_token#{}", node.hash);
    let req_valid = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": valid_uri })),
    };
    let resp_valid = server.handle_request(req_valid).await;
    assert!(resp_valid.error.is_none());
    assert!(resp_valid.result.is_some());

    // Stale URI with mismatched hash
    let stale_uri = "axiom://slice/auth::service::validate_token#stalehash999";
    let req_stale = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(5)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": stale_uri })),
    };
    let resp_stale = server.handle_request(req_stale).await;
    let err = resp_stale.error.expect("stale URI read must error");
    let msg = err["message"].as_str().unwrap();
    assert!(msg.contains("CAS reference hash mismatch"));
}
