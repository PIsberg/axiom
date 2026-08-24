//! `axiom_run_tests` runs the project's own test command and records the
//! outcome so a provenance record can rest on it. The point is the honesty
//! category: axiom ran the command and saw the exit code, so the record says
//! `executed`, not `reported`, and a failed run cannot be attested.

use axiom_core::AxiomMcpServer;
use axiom_core::mcp::JsonRpcRequest;
use serde_json::json;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_runtests_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

fn call(server: &AxiomMcpServer, name: &str, args: serde_json::Value) -> serde_json::Value {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resp = rt.block_on(server.handle_request(JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({ "name": name, "arguments": args })),
    }));
    let text = resp.result.unwrap()["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    serde_json::from_str(&text).unwrap()
}

fn server_over(dir: &std::path::Path) -> AxiomMcpServer {
    let axiom = dir.join(".axiom");
    std::fs::create_dir_all(&axiom).unwrap();
    std::fs::write(
        axiom.join("index.json"),
        r#"{"format_version":2,"nodes":{},"method_return_types":{},"file_call_names":{},"file_to_symbols":{}}"#,
    )
    .unwrap();
    AxiomMcpServer::with_index(Some(&axiom.join("index.json"))).expect("server")
}

#[test]
fn a_passing_command_is_executed_and_can_be_attested() {
    let dir = tmp("pass");
    let server = server_over(&dir);

    let result = call(
        &server,
        "axiom_run_tests",
        json!({ "command": "exit 0", "task_id": "t-pass", "symbol_path": "s::sym" }),
    );
    assert_eq!(result["status"], "PASSED", "{result}");
    assert_eq!(result["recorded_as"], "executed");

    // A record can be issued against it, and it says the check was executed.
    let record = call(
        &server,
        "axiom_attest_commit",
        json!({ "prompt": "fix s::sym", "symbol_path": "s::sym", "ctop_task_id": "t-pass" }),
    );
    assert_eq!(
        record["verified_by"], "executed",
        "the record must record who ran the check: {record}"
    );
    assert!(record["seal"].is_string());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failing_command_cannot_be_attested() {
    let dir = tmp("fail");
    let server = server_over(&dir);

    let result = call(
        &server,
        "axiom_run_tests",
        json!({ "command": "exit 1", "task_id": "t-fail" }),
    );
    assert_eq!(result["status"], "FAILED", "{result}");
    assert_eq!(result["passed"], false);

    let record = call(
        &server,
        "axiom_attest_commit",
        json!({ "prompt": "fix it", "symbol_path": "s::sym", "ctop_task_id": "t-fail" }),
    );
    assert!(
        record["error"]
            .as_str()
            .unwrap_or("")
            .contains("did not pass"),
        "a failed check must not be attestable: {record}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
