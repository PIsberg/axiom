use axiom_core::mcp::{AxiomMcpServer, JsonRpcRequest};
use serde_json::json;

#[tokio::test]
async fn test_mcp_initialize_advertises_resources_and_prompts() {
    let server = AxiomMcpServer::with_index(None).unwrap();
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "initialize".to_string(),
        params: None,
    };

    let res = server.handle_request(req).await;
    assert!(res.error.is_none());
    let result = res.result.expect("result present");
    assert!(result["capabilities"]["resources"].is_object());
    assert!(result["capabilities"]["prompts"].is_object());
    assert!(result["capabilities"]["tools"].is_object());
}

#[tokio::test]
async fn test_mcp_resources_list_and_read() {
    let server = AxiomMcpServer::with_index(None).unwrap();
    server.seed_demo_workspace();

    // 1. resources/list
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "resources/list".to_string(),
        params: None,
    };
    let res = server.handle_request(req).await;
    assert!(res.error.is_none());
    let result = res.result.unwrap();
    let resources = result["resources"].as_array().expect("resources array");
    assert!(resources.iter().any(|r| r["uri"] == "axiom://symbols"));
    assert!(resources.iter().any(|r| r["uri"] == "axiom://ledger"));

    // 2. resources/read axiom://symbols
    let read_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://symbols" })),
    };
    let read_res = server.handle_request(read_req).await;
    assert!(read_res.error.is_none());
    let read_result = read_res.result.unwrap();
    let contents = read_result["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["uri"], "axiom://symbols");

    // 3. resources/read specific symbol
    let sym_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://symbols/auth::service::validate_token" })),
    };
    let sym_res = server.handle_request(sym_req).await;
    assert!(sym_res.error.is_none());
    let sym_result = sym_res.result.unwrap();
    let sym_contents = sym_result["contents"].as_array().unwrap();
    let text = sym_contents[0]["text"].as_str().unwrap();
    assert!(text.contains("validate_token"));

    // 4. resources/read blast radius
    let blast_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(5)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://blast-radius/auth::service::validate_token" })),
    };
    let blast_res = server.handle_request(blast_req).await;
    assert!(blast_res.error.is_none());
}

#[tokio::test]
async fn test_mcp_prompts_list_and_get() {
    let server = AxiomMcpServer::with_index(None).unwrap();

    // 1. prompts/list
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(10)),
        method: "prompts/list".to_string(),
        params: None,
    };
    let res = server.handle_request(req).await;
    assert!(res.error.is_none());
    let result = res.result.unwrap();
    let prompts = result["prompts"].as_array().expect("prompts array");
    assert!(prompts.iter().any(|p| p["name"] == "axiom_review_patch"));
    assert!(
        prompts
            .iter()
            .any(|p| p["name"] == "axiom_targeted_refactor")
    );
    assert!(prompts.iter().any(|p| p["name"] == "axiom_attest_task"));

    // 2. prompts/get axiom_targeted_refactor
    let get_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(11)),
        method: "prompts/get".to_string(),
        params: Some(json!({
            "name": "axiom_targeted_refactor",
            "arguments": {
                "target_symbol": "auth::service::validate_token",
                "goal": "Optimize regex and add expiry check"
            }
        })),
    };
    let get_res = server.handle_request(get_req).await;
    assert!(get_res.error.is_none());
    let get_result = get_res.result.unwrap();
    let messages = get_result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    let prompt_text = messages[0]["content"]["text"].as_str().unwrap();
    assert!(prompt_text.contains("auth::service::validate_token"));
    assert!(prompt_text.contains("Optimize regex and add expiry check"));
    assert!(prompt_text.contains("axiom_get_blast_radius"));
}
