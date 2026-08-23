//! A server writes its ledger, its op log and its mutated index to the same
//! `.axiom` it read from, not to the current directory.
//!
//! `find_index_file` walks up from the working directory to find the index,
//! and the MCP server inherits its client's working directory, which is the
//! agent's project and may be a subdirectory of it. Reads came from the
//! discovered `.axiom` while writes went to `<cwd>/.axiom`, so a mutation, an
//! attestation and a CRDT op landed somewhere the next read would not look.
//! Measured on 2026-08-23: scanning a subdirectory loaded the repository's
//! index from an ancestor and wrote it back a directory away.

use axiom_core::AxiomMcpServer;
use axiom_core::mcp::JsonRpcRequest;
use serde_json::json;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_writes_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("create temp dir");
    d
}

fn call(server: &AxiomMcpServer, name: &str, args: serde_json::Value) -> serde_json::Value {
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({ "name": name, "arguments": args })),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resp = rt.block_on(server.handle_request(req));
    let text = resp.result.unwrap()["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn writes_land_in_the_discovered_axiom_directory() {
    let root = tmp("root");
    let axiom = root.join(".axiom");
    std::fs::create_dir_all(&axiom).unwrap();
    // A minimal index the server can discover and load.
    std::fs::write(
        axiom.join("index.json"),
        r#"{"format_version":2,"nodes":{},"method_return_types":{},"file_call_names":{},"file_to_symbols":{}}"#,
    )
    .unwrap();

    let server = AxiomMcpServer::with_index(Some(&axiom.join("index.json"))).expect("server");

    // A mutation writes the index and records a CRDT op.
    let mutation = call(
        &server,
        "axiom_apply_mutation",
        json!({ "node_id": "n1", "symbol_path": "demo::widget", "content": "fn widget() {}" }),
    );
    assert_eq!(mutation["status"], "APPLIED", "{mutation}");
    assert!(
        axiom.join("crdt_ops.json").exists(),
        "the CRDT op log must be written under the discovered .axiom, not the working directory"
    );

    // A reported verification and an attestation write the ledger.
    call(
        &server,
        "axiom_record_verification",
        json!({ "task_id": "t1", "passed": true, "command": "cargo test" }),
    );
    let attestation = call(
        &server,
        "axiom_attest_commit",
        json!({ "prompt": "add a widget", "symbol_path": "demo::widget", "ctop_task_id": "t1" }),
    );
    assert!(
        attestation.get("seal").is_some(),
        "attest failed: {attestation}"
    );
    assert!(
        axiom.join("attestations.json").exists(),
        "the ledger must be written under the discovered .axiom, not the working directory"
    );

    let _ = std::fs::remove_dir_all(&root);
}
