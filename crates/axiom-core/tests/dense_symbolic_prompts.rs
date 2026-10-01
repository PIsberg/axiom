use axiom_core::mcp::{AxiomMcpServer, JsonRpcRequest};
use serde_json::json;

#[tokio::test]
async fn test_dense_symbolic_prompts_format() {
    let server = AxiomMcpServer::new().unwrap();
    server.seed_demo_workspace();

    // 1. axiom_review_patch should use dense ACTION / SYM format without conversational English filler
    let review_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "prompts/get".to_string(),
        params: Some(json!({
            "name": "axiom_review_patch",
            "arguments": {
                "symbol_path": "validate_token"
            }
        })),
    };
    let review_res = server.handle_request(review_req).await;
    let review_text = review_res.result.unwrap()["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();

    assert!(review_text.starts_with("ACTION: REVIEW_PATCH | SYM: auth::service::validate_token | VERIFY: AST_SIG,BLAST_RADIUS,SANDBOX"));
    assert!(!review_text.contains("Please review changes affecting symbol"));

    // 2. axiom_targeted_refactor should use dense ACTION / TARGET / GOAL format
    let refactor_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "prompts/get".to_string(),
        params: Some(json!({
            "name": "axiom_targeted_refactor",
            "arguments": {
                "target_symbol": "validate_token",
                "goal": "Migrate to argon2 password hashing"
            }
        })),
    };
    let refactor_res = server.handle_request(refactor_req).await;
    let refactor_text = refactor_res.result.unwrap()["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();

    assert!(refactor_text.starts_with("ACTION: TARGETED_REFACTOR | TARGET: auth::service::validate_token | GOAL: Migrate to argon2 password hashing\nSTEPS: axiom_query_symbol -> axiom_get_blast_radius -> axiom_apply_mutation -> axiom_eval_patch/axiom_run_tests -> axiom_attest_commit"));
    assert!(
        !refactor_text.contains("Refactor symbol 'auth::service::validate_token' to accomplish:")
    );

    // 3. axiom_attest_task should use dense ACTION / TASK / SYM format
    let attest_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "prompts/get".to_string(),
        params: Some(json!({
            "name": "axiom_attest_task",
            "arguments": {
                "prompt": "Implement secure auth token validation",
                "symbol_path": "validate_token"
            }
        })),
    };
    let attest_res = server.handle_request(attest_req).await;
    let attest_text = attest_res.result.unwrap()["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();

    assert!(attest_text.starts_with("ACTION: ATTEST_TASK | TASK: Implement secure auth token validation | SYM: auth::service::validate_token | REQ: VERIFICATION_PASSED"));
    assert!(!attest_text.contains("Attest task completion for prompt"));
}
