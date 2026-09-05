use axiom_core::mcp::{AxiomMcpServer, JsonRpcRequest};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempWorkspace {
    path: std::path::PathBuf,
}

impl TempWorkspace {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("axiom-fc-{}-{}-{}", tag, std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".axiom")).expect("create .axiom dir");
        Self { path: dir }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[tokio::test]
async fn test_verified_fix_cache_attestation_and_retrieval() {
    let ws = TempWorkspace::new("cache_test");
    let index_file = ws.path.join(".axiom").join("index.json");
    let server = AxiomMcpServer::with_index(Some(&index_file)).expect("server created");
    server.seed_demo_workspace();

    // 1. Initially, fixes list should be empty
    let list_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://fixes" })),
    };
    let list_res = server.handle_request(list_req).await;
    assert!(list_res.error.is_none());
    let list_result = list_res.result.unwrap();
    let initial_data: serde_json::Value = serde_json::from_str(
        list_result["contents"][0]["text"].as_str().unwrap()
    ).unwrap();
    assert_eq!(initial_data["count"], 0);
    assert!(initial_data["fixes"].as_object().unwrap().is_empty());

    // 2. First record a valid verification so attestation can anchor to it
    let record_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "axiom_record_verification",
            "arguments": {
                "task_id": "task_verify_001",
                "command": "cargo test",
                "passed": true
            }
        })),
    };
    let record_res = server.handle_request(record_req).await;
    assert!(record_res.error.is_none());
    let rec_res = record_res.result.unwrap();
    assert_eq!(rec_res["isError"], false);

    // 3. Attest commit with error_signature and patch_content to register in patch memory
    let error_sig = "NullPointerException at AuthTokenValidator.java:42";
    let patch_body = "if (token == null) return false;";
    let attest_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "axiom_attest_commit",
            "arguments": {
                "prompt": "Fix NPE in token validation",
                "symbol_path": "auth::service::validate_token",
                "ctop_task_id": "task_verify_001",
                "error_signature": error_sig,
                "patch_content": patch_body
            }
        })),
    };
    let attest_res = server.handle_request(attest_req).await;
    assert!(attest_res.error.is_none());
    let attest_result = attest_res.result.unwrap();
    assert_eq!(attest_result["isError"], false);

    let attest_body: serde_json::Value = serde_json::from_str(
        attest_result["content"][0]["text"].as_str().unwrap()
    ).unwrap();
    assert!(attest_body["seal"].is_string());
    assert_eq!(attest_body["symbol_path"], "auth::service::validate_token");

    // 4. Query axiom://fixes resource - should now contain 1 verified fix
    let read_fixes_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://fixes" })),
    };
    let read_fixes_res = server.handle_request(read_fixes_req).await;
    assert!(read_fixes_res.error.is_none());
    let read_fixes_result = read_fixes_res.result.unwrap();
    let fixes_data: serde_json::Value = serde_json::from_str(
        read_fixes_result["contents"][0]["text"].as_str().unwrap()
    ).unwrap();
    assert_eq!(fixes_data["count"], 1);

    let fixes_map = fixes_data["fixes"].as_object().unwrap();
    let (fingerprint, candidates) = fixes_map.iter().next().unwrap();
    let candidates_arr = candidates.as_array().unwrap();
    assert_eq!(candidates_arr.len(), 1);
    assert_eq!(candidates_arr[0]["error_signature"], error_sig);
    assert_eq!(candidates_arr[0]["patch_content"], patch_body);
    assert_eq!(candidates_arr[0]["symbol_path"], "auth::service::validate_token");

    // 5. Query specific fix by fingerprint axiom://fixes/{fingerprint}
    let single_fix_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(5)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": format!("axiom://fixes/{}", fingerprint) })),
    };
    let single_fix_res = server.handle_request(single_fix_req).await;
    assert!(single_fix_res.error.is_none());
    let single_fix_result = single_fix_res.result.unwrap();
    let fix_obj: serde_json::Value = serde_json::from_str(
        single_fix_result["contents"][0]["text"].as_str().unwrap()
    ).unwrap();
    assert_eq!(fix_obj["fingerprint"], *fingerprint);
    let single_candidates = fix_obj["candidates"].as_array().unwrap();
    assert_eq!(single_candidates[0]["fingerprint"], *fingerprint);
    assert_eq!(single_candidates[0]["patch_content"], patch_body);

    // 6. Test 0ms patch memory lookup matching error signature
    let matching = server.find_matching_fixes("some_ast_hash", error_sig);
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].patch_content, patch_body);

    // 7. Test persistence: reload server from same path, verify fix cache persists
    drop(server);
    let reloaded_server = AxiomMcpServer::with_index(Some(&index_file)).expect("reloaded");
    let reloaded_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(6)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://fixes" })),
    };
    let reloaded_res = reloaded_server.handle_request(reloaded_req).await;
    assert!(reloaded_res.error.is_none());
    let reloaded_result = reloaded_res.result.unwrap();
    let reloaded_data: serde_json::Value = serde_json::from_str(
        reloaded_result["contents"][0]["text"].as_str().unwrap()
    ).unwrap();
    assert_eq!(reloaded_data["count"], 1);
    let reloaded_map = reloaded_data["fixes"].as_object().unwrap();
    assert!(reloaded_map.contains_key(fingerprint));
}

#[tokio::test]
async fn test_dynamic_context_prompts_expansion() {
    let ws = TempWorkspace::new("prompts_test");
    let index_file = ws.path.join(".axiom").join("index.json");
    let server = AxiomMcpServer::with_index(Some(&index_file)).expect("server created");
    server.seed_demo_workspace();

    // 1. axiom_review_patch prompt with symbol_path
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
    assert!(review_res.error.is_none());
    let review_result = review_res.result.unwrap();
    let text = review_result["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("Pre-Computed Sub-Graph Context"));
    assert!(text.contains("auth::service::validate_token"));
    assert!(text.contains("Impacted Tests"));
    assert!(text.contains("Causal Propagation Paths"));

    // 2. axiom_targeted_refactor prompt with target_symbol
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
    assert!(refactor_res.error.is_none());
    let refactor_result = refactor_res.result.unwrap();
    let refactor_text = refactor_result["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(refactor_text.contains("Migrate to argon2 password hashing"));
    assert!(refactor_text.contains("Pre-Computed Context for Target"));
    assert!(refactor_text.contains("Refactoring Directives"));
    assert!(refactor_text.contains("Downstream Impact"));

    // 3. axiom_attest_task prompt
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
    assert!(attest_res.error.is_none());
    let attest_result = attest_res.result.unwrap();
    let attest_text = attest_result["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(attest_text.contains("Implement secure auth token validation"));
    assert!(attest_text.contains("auth::service::validate_token"));
    assert!(attest_text.contains("Task Attestation Context"));
    assert!(attest_text.contains("Merkle Commit Root"));
}
