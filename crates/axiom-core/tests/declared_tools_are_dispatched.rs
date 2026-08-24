//! Every tool the server advertises must actually answer.
//!
//! The tool list in `handle_request` and the dispatch `match` below it are two
//! places that have to be edited together, and nothing made them agree. A tool
//! declared but not dispatched fails at call time rather than at startup, which
//! means the agent that trusted the list is the one that finds out.
//!
//! This also pins the count that CLAUDE.md quotes. That claim had already gone
//! stale once: the file said six tools after `axiom_record_verification` made it
//! seven, and CLAUDE.md is loaded into every session, so a wrong number there
//! misleads the next piece of work rather than merely being untidy.

use axiom_core::{AxiomMcpServer, mcp::JsonRpcRequest};
use serde_json::json;

async fn declared_tool_names(server: &AxiomMcpServer) -> Vec<String> {
    let resp = server
        .handle_request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/list".into(),
            params: None,
        })
        .await;
    let result = resp.result.expect("tools/list must return a result");
    result["tools"]
        .as_array()
        .expect("tools must be an array")
        .iter()
        .map(|t| {
            t["name"]
                .as_str()
                .expect("every tool must be named")
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn every_declared_tool_answers_when_called() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    let names = declared_tool_names(&server).await;

    assert!(!names.is_empty(), "the server declared no tools at all");

    for name in &names {
        // Deliberately empty arguments. A dispatched tool may refuse them, and
        // refusing is a fine answer; what must not happen is the request
        // falling through to the unknown-tool arm, which is the shape of the
        // failure a declared-but-undispatched tool produces.
        let resp = server
            .handle_request(JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: Some(json!(2)),
                method: "tools/call".into(),
                params: Some(json!({ "name": name, "arguments": {} })),
            })
            .await;

        let rendered = format!("{:?}", resp);
        assert!(
            !rendered.to_lowercase().contains("unknown tool"),
            "{name} is advertised by tools/list and is not dispatched, so an \
             agent that believed the list gets an unknown-tool error at call \
             time: {rendered}"
        );
    }
}

/// The names themselves, so removing or renaming one is a deliberate act rather
/// than something a caller discovers.
#[tokio::test]
async fn the_advertised_tool_set_is_the_documented_one() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    let mut names = declared_tool_names(&server).await;
    names.sort();

    let mut expected = vec![
        "axiom_apply_mutation",
        "axiom_attest_commit",
        "axiom_eval_patch",
        "axiom_get_blast_radius",
        "axiom_query_symbol",
        "axiom_record_verification",
        "axiom_search_regex",
        "axiom_run_tests",
    ];
    expected.sort();

    assert_eq!(
        names, expected,
        "the tool set changed; update the count and the list in CLAUDE.md in the \
         same change, since that file is read into every session"
    );
}
