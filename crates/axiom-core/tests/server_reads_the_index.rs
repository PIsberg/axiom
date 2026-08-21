//! The server answers from the index a scan left on disk.
//!
//! This is the seam that failed in the field: scanning worked, persistence
//! worked, and the server still answered every query as a miss, because it was
//! a separate process and nothing checked that it read what the scan wrote. A
//! test of the scan alone would not have caught it.

use axiom_ast::AstIndex;
use axiom_core::mcp::{AxiomMcpServer, JsonRpcRequest};
use serde_json::json;

const COUNTER_JAVA: &str = "package p;\npublic class Counter {\n    private int n;\n    public void increment() { n++; }\n}\n";

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("axiom-core-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test directory");
    dir
}

fn query(symbol: &str) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "axiom_query_symbol",
            "arguments": { "symbol_path": symbol }
        })),
    }
}

fn answer_text(response: &axiom_core::mcp::JsonRpcResponse) -> String {
    response
        .result
        .as_ref()
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_server_answers_from_the_index_a_scan_wrote() {
    let dir = temp_dir("reads");
    std::fs::write(dir.join("Counter.java"), COUNTER_JAVA).expect("write the fixture");
    let index_file = dir.join("index.json");

    let scanned = AstIndex::new();
    scanned.scan_directory(&dir).expect("scan the directory");
    scanned.save_to_disk(&index_file).expect("save the index");

    let server = AxiomMcpServer::with_index(Some(&index_file)).expect("build the server");
    let text = answer_text(&server.handle_request(query("p.Counter::increment")).await);

    assert!(
        !text.contains("not found"),
        "the server reported a miss for a symbol the scan recorded: {text}"
    );
    assert!(
        text.contains("increment"),
        "the answer does not name the symbol asked for: {text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The other direction, so the test above cannot pass by the server answering
/// everything: with no index, the same query is a miss.
#[tokio::test]
async fn a_server_with_no_index_reports_a_miss() {
    let server = AxiomMcpServer::with_index(None).expect("build the server");
    let text = answer_text(&server.handle_request(query("p.Counter::increment")).await);

    assert!(
        text.contains("not found"),
        "a server with no index answered a query it cannot know: {text}"
    );
}
