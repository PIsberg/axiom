//! The id an attestation rests on has to name one run.
//!
//! `axiom_attest_commit` refuses to seal anything that is not backed by a
//! sandbox run that passed, and it finds that run by looking `ctop_task_id` up
//! in the verification map. The refusal is only as good as the id: if two runs
//! share one, the later run's outcome silently replaces the earlier one's, and
//! a seal claiming "verified in the sandbox" can be issued for a change whose
//! only check failed. That is the exact shape this repository exists to
//! prevent, an agent acting on a confident wrong answer, so the uniqueness of
//! the id is a correctness property and not a cosmetic one.
//!
//! It was not unique. The Rust and WASI paths built their id from
//! `start.elapsed().as_nanos()` sampled on the statement after
//! `Instant::now()`, which measures the gap between two adjacent statements:
//! twelve consecutive evaluations produced two distinct ids, `eval_64` and
//! `eval_0`. Reproduced on Windows on 2026-09-11; the interval is short enough
//! that the clock's resolution, not the platform, is what collapses it.

use axiom_core::{AxiomMcpServer, mcp::JsonRpcRequest};
use serde_json::json;

async fn call(server: &AxiomMcpServer, name: &str, args: serde_json::Value) -> serde_json::Value {
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

/// Distinct evaluations get distinct ids.
///
/// Snippet text differs each time so nothing about caching can make two runs
/// legitimately the same run.
#[tokio::test]
async fn every_evaluation_gets_its_own_task_id() {
    let server = AxiomMcpServer::with_index(None).expect("server");

    let mut ids = Vec::new();
    for i in 0..12 {
        let report = call(
            &server,
            "axiom_eval_patch",
            json!({
                "symbol_path": "some::symbol",
                "code_snippet": format!("fn main() {{ let _ = {i}; }}"),
            }),
        )
        .await;
        // A machine without rustc reports EvaluatorUnavailable, which still
        // carries a task id; the id is what is under test, not the verdict.
        ids.push(
            report["task_id"]
                .as_str()
                .expect("every report names its run")
                .to_string(),
        );
    }

    let distinct: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "twelve evaluations produced {} distinct ids: {ids:?}",
        distinct.len()
    );
}

/// A failed run's id cannot be redeemed by a later passing run.
///
/// This is the consequence the uniqueness exists for, asserted through the
/// agent-visible surface rather than against the id generator, so it keeps
/// holding however the id is built.
#[tokio::test]
async fn a_failed_run_cannot_be_attested_after_a_later_run_passes() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    server.seed_demo_workspace();
    let symbol = "auth::service::validate_token";

    let broken = call(
        &server,
        "axiom_eval_patch",
        json!({
            "symbol_path": symbol,
            "code_snippet": "fn main() { assert_eq!(2 * 2, 5, \"the patch is broken\"); }",
        }),
    )
    .await;
    let broken_id = broken["task_id"].as_str().expect("an id").to_string();

    if broken["status"] == "Passed" {
        // No rustc here, so nothing failed and there is nothing to redeem.
        // Reported rather than asserted: a skipped check is not a passed one.
        eprintln!(
            "skipped: the evaluator returned {} rather than running the snippet",
            broken["status"]
        );
        return;
    }

    // Ten trivially passing runs. Under the old id this is what overwrote the
    // failure: one of them reused `broken_id` and recorded a pass against it.
    for i in 0..10 {
        call(
            &server,
            "axiom_eval_patch",
            json!({
                "symbol_path": symbol,
                "code_snippet": format!("fn main() {{ assert!(true); let _ = {i}; }}"),
            }),
        )
        .await;
    }

    let attested = call(
        &server,
        "axiom_attest_commit",
        json!({
            "symbol_path": symbol,
            "prompt": "fix validate_token",
            "ctop_task_id": broken_id,
            "agent_identity": "test-agent",
        }),
    )
    .await;

    assert!(
        attested.get("seal").is_none(),
        "a seal was issued for a run that failed: {attested}"
    );
    assert!(
        attested["error"]
            .as_str()
            .unwrap_or_default()
            .contains("did not pass"),
        "the refusal must say the run failed, not something else: {attested}"
    );
}
