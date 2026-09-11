//! An argument the server cannot honour is refused, not quietly replaced.
//!
//! Every tool answer is JSON an agent acts on without checking, so the cost of
//! answering a different question from the one asked is the same as the cost of
//! answering it wrongly. Three shapes of that were in the dispatcher:
//!
//! - `max_depth: -1` and `max_depth: "3"` reached `as_u64()`, which returns
//!   `None` for anything that is not a non-negative integer, and `unwrap_or(1)`
//!   turned that into a depth-1 answer with nothing in the reply saying the
//!   request had been rewritten. The same held for `token_budget` and
//!   `max_results`.
//! - `commit_staged` and `rollback_staged` set together are contradictory.
//!   The server took the rollback branch and reported `ROLLED_BACK` as though
//!   that were what was asked.
//! - `axiom_search_regex` accepted a blank query and returned the whole
//!   repository up to `max_results`, while `axiom_query_symbol` refused a blank
//!   `symbol_path`. The same mistake, two answers.
//!
//! An absent argument still takes its documented default; that is what a
//! default is for. This is about arguments that are present and unusable.

use axiom_core::{AxiomMcpServer, mcp::JsonRpcRequest};
use serde_json::{Value, json};

async fn call(server: &AxiomMcpServer, name: &str, args: Value) -> Value {
    let resp = server
        .handle_request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": name, "arguments": args })),
        })
        .await;
    let text = resp.result.expect("a result")["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string();
    serde_json::from_str(&text).expect("the tool returns JSON")
}

fn error_of(v: &Value) -> String {
    v["error"].as_str().unwrap_or_default().to_string()
}

/// The error has to name the argument, or the agent cannot act on it.
fn assert_refused(answer: &Value, argument: &str) {
    let error = error_of(answer);
    assert!(
        !error.is_empty(),
        "expected a refusal naming {argument}, got {answer}"
    );
    assert!(
        error.contains(argument),
        "the refusal must name {argument} so the caller knows what to change: {error}"
    );
}

fn server() -> AxiomMcpServer {
    let s = AxiomMcpServer::with_index(None).expect("server");
    s.seed_demo_workspace();
    s
}

const SYMBOL: &str = "auth::service::validate_token";

#[tokio::test]
async fn a_negative_depth_is_refused_rather_than_read_as_the_default() {
    let answer = call(
        &server(),
        "axiom_get_blast_radius",
        json!({ "symbol_path": SYMBOL, "max_depth": -1 }),
    )
    .await;
    assert_refused(&answer, "max_depth");
}

#[tokio::test]
async fn a_depth_that_is_not_a_number_is_refused() {
    let answer = call(
        &server(),
        "axiom_get_blast_radius",
        json!({ "symbol_path": SYMBOL, "max_depth": "3" }),
    )
    .await;
    assert_refused(&answer, "max_depth");
}

/// `depth` is the older spelling the CLI still sends, and it goes through the
/// same reader, so it has to refuse the same way.
#[tokio::test]
async fn the_depth_alias_is_checked_too() {
    let answer = call(
        &server(),
        "axiom_get_blast_radius",
        json!({ "symbol_path": SYMBOL, "depth": 2.5 }),
    )
    .await;
    assert_refused(&answer, "depth");
}

#[tokio::test]
async fn an_absent_depth_still_takes_the_documented_default() {
    let answer = call(
        &server(),
        "axiom_get_blast_radius",
        json!({ "symbol_path": SYMBOL }),
    )
    .await;
    assert!(
        error_of(&answer).is_empty(),
        "a request that omits an optional argument is not malformed: {answer}"
    );
    assert!(
        answer.get("impacted_tests").is_some(),
        "the default must still produce a blast radius: {answer}"
    );
}

#[tokio::test]
async fn a_negative_token_budget_is_refused() {
    let answer = call(
        &server(),
        "axiom_query_symbol",
        json!({ "symbol_path": SYMBOL, "token_budget": -50 }),
    )
    .await;
    assert_refused(&answer, "token_budget");
}

#[tokio::test]
async fn a_max_results_that_is_not_a_count_is_refused() {
    let answer = call(
        &server(),
        "axiom_search_regex",
        json!({ "query": "fn", "max_results": "many" }),
    )
    .await;
    assert_refused(&answer, "max_results");
}

#[tokio::test]
async fn a_staging_flag_that_is_not_a_boolean_is_refused() {
    let answer = call(
        &server(),
        "axiom_apply_mutation",
        json!({ "symbol_path": SYMBOL, "speculative": "yes" }),
    )
    .await;
    assert_refused(&answer, "speculative");
}

/// Committing and rolling back are opposites; the server used to pick one.
#[tokio::test]
async fn committing_and_rolling_back_at_once_is_refused() {
    let answer = call(
        &server(),
        "axiom_apply_mutation",
        json!({ "symbol_path": SYMBOL, "commit_staged": true, "rollback_staged": true }),
    )
    .await;

    assert_ne!(
        answer["status"], "ROLLED_BACK",
        "a contradictory request must not be reported as a completed rollback: {answer}"
    );
    let error = error_of(&answer);
    assert!(
        error.contains("commit_staged") && error.contains("rollback_staged"),
        "the refusal must name both flags so the caller can see the conflict: {error}"
    );
}

/// Either flag alone still works; the refusal is for the pair.
#[tokio::test]
async fn rolling_back_alone_still_works() {
    let s = server();
    call(
        &s,
        "axiom_apply_mutation",
        json!({
            "symbol_path": SYMBOL,
            "content": "pub fn validate_token(t: &str) -> bool { true }",
            "speculative": true,
            "node_id": "n1",
        }),
    )
    .await;

    let answer = call(
        &s,
        "axiom_apply_mutation",
        json!({ "symbol_path": SYMBOL, "rollback_staged": true }),
    )
    .await;
    assert_eq!(answer["status"], "ROLLED_BACK", "{answer}");
}

#[tokio::test]
async fn a_blank_search_query_is_refused_like_a_blank_symbol_path() {
    let answer = call(&server(), "axiom_search_regex", json!({ "query": "" })).await;
    assert_refused(&answer, "query");
    assert!(
        answer["matches"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "a refused search must not also return matches: {answer}"
    );
}
