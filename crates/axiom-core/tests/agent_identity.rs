//! Who a provenance record says wrote it.
//!
//! `axiom_attest_commit` used to hardcode `agent_identity: "agent_axiom_v1"`.
//! Two agents sharing a workspace produced records indistinguishable by author,
//! and a caller that passed the field got its value silently dropped: the
//! response echoed the constant back, so nothing in the reply said the argument
//! had not landed. A field that looks like an answer and is not is worse than no
//! field at all, which is what #11 is about.
//!
//! What the field can and cannot carry is the point of these tests. It is
//! self-declared, so on an unsigned record it is a claim and nothing more. It is
//! covered by the seal and by the signature, so on a signed record it is bound
//! to the key that issued it and cannot be edited afterwards.

use axiom_core::{mcp::JsonRpcRequest, mcp::JsonRpcResponse, AxiomMcpServer};
use serde_json::{json, Value};

fn extract_tool_result(resp: &JsonRpcResponse) -> Value {
    let res = resp
        .result
        .as_ref()
        .expect("Expected result in JsonRpcResponse");
    let text = res["content"][0]["text"]
        .as_str()
        .expect("Expected text in content");
    serde_json::from_str(text).expect("Expected valid json in content text")
}

fn call(name: &str, args: Value) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({ "name": name, "arguments": args })),
    }
}

/// Record a passing external check so an attestation has something to rest on,
/// and return the task id. Each test uses its own id: the ledger is shared
/// through the working directory, so tests must not depend on one another.
async fn passing_check(server: &AxiomMcpServer, task_id: &str) -> String {
    let res = extract_tool_result(
        &server
            .handle_request(call(
                "axiom_record_verification",
                json!({ "task_id": task_id, "passed": true, "command": "cargo test" }),
            ))
            .await,
    );
    assert_eq!(
        res.get("recorded_as").and_then(|v| v.as_str()),
        Some("reported"),
        "the check this test rests on was not recorded: {res:?}"
    );
    task_id.to_string()
}

#[tokio::test]
async fn the_agent_identity_a_caller_supplies_is_the_one_recorded() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    let task = passing_check(&server, "identity_supplied_01").await;

    let sealed = extract_tool_result(
        &server
            .handle_request(call(
                "axiom_attest_commit",
                json!({
                    "prompt": "self-test of the provenance loop",
                    "symbol_path": "worth_retrying",
                    "ctop_task_id": task,
                    "agent_identity": "claude-code-selftest"
                }),
            ))
            .await,
    );

    assert_eq!(
        sealed.get("agent_identity").and_then(|v| v.as_str()),
        Some("claude-code-selftest"),
        "the caller named itself and the record must say so, got {sealed:?}"
    );
}

/// The old default looked like an identity. Nothing had established it, so a
/// reader seeing `agent_axiom_v1` next to `public_key` reasonably took it for
/// the claimed author when it was a constant every record carried.
#[tokio::test]
async fn an_unnamed_caller_is_not_given_an_identity_that_looks_established() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    let task = passing_check(&server, "identity_absent_01").await;

    let sealed = extract_tool_result(
        &server
            .handle_request(call(
                "axiom_attest_commit",
                json!({
                    "prompt": "no identity given",
                    "symbol_path": "worth_retrying",
                    "ctop_task_id": task
                }),
            ))
            .await,
    );

    let identity = sealed
        .get("agent_identity")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_ne!(
        identity, "agent_axiom_v1",
        "a caller that named nobody must not be given a plausible-looking agent name"
    );
    assert_eq!(
        identity, "unattributed",
        "the absent case must read as absent, got {sealed:?}"
    );
}

/// `axiom verify` prints the identity in a column of labelled lines. A value
/// carrying a newline could add lines of its own, so a record could claim
/// "Checked by: sandbox" in output while its `verified_by` field said
/// `reported`. The field is caller-controlled, so the terminal rendering is an
/// injection surface and the value has to be constrained where it enters.
#[tokio::test]
async fn an_agent_identity_that_could_forge_verify_output_is_refused() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    let task = passing_check(&server, "identity_forged_01").await;

    let forged = "innocent\n   Checked by:    sandbox (axiom sandbox, engine tier1)";
    let res = extract_tool_result(
        &server
            .handle_request(call(
                "axiom_attest_commit",
                json!({
                    "prompt": "forge the verify output",
                    "symbol_path": "worth_retrying",
                    "ctop_task_id": task,
                    "agent_identity": forged
                }),
            ))
            .await,
    );

    let error = res.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        error.contains("agent_identity"),
        "a newline in the identity must be refused and the refusal must name the field, got {res:?}"
    );
    assert!(
        res.get("seal").is_none(),
        "a refused attestation must not also issue a record, got {res:?}"
    );
}

/// A length bound for the same reason: the value is printed, stored in the
/// ledger, and hashed into the seal.
#[tokio::test]
async fn an_over_long_agent_identity_is_refused() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    let task = passing_check(&server, "identity_long_01").await;

    let res = extract_tool_result(
        &server
            .handle_request(call(
                "axiom_attest_commit",
                json!({
                    "prompt": "too long",
                    "symbol_path": "worth_retrying",
                    "ctop_task_id": task,
                    "agent_identity": "a".repeat(129)
                }),
            ))
            .await,
    );

    assert!(
        res.get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("agent_identity"),
        "an identity past the length bound must be refused, got {res:?}"
    );
}

/// A caller cannot discover an argument the schema does not declare. The tool
/// list is the only description of the input an agent gets.
#[tokio::test]
async fn the_tool_schema_declares_agent_identity() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    let resp = server
        .handle_request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/list".into(),
            params: None,
        })
        .await;

    let result = resp.result.expect("tools/list result");
    let tools = result["tools"].as_array().expect("tools array");
    let attest = tools
        .iter()
        .find(|t| t["name"] == "axiom_attest_commit")
        .expect("axiom_attest_commit must be declared");

    assert!(
        attest["inputSchema"]["properties"]
            .get("agent_identity")
            .is_some(),
        "the caller has no way to learn about an argument the schema omits, got {attest:?}"
    );
}

/// The field is hashed into the seal, so a supplied identity is tamper-evident:
/// editing it in a stored record breaks verification rather than quietly
/// reassigning authorship. That is what makes accepting the argument safe.
#[test]
fn editing_the_agent_identity_of_a_stored_record_breaks_its_seal() {
    use axiom_proto::{NewAttestation, ProvenanceAttestation};

    let details = |identity: &'static str| NewAttestation {
        parent_merkle_root: "merkle_root_prev_77a1",
        commit_merkle_root: "merkle_root_aaaaaaaa",
        agent_identity: identity,
        prompt: "Tighten the guard",
        symbol_path: "auth::service::validate_token",
        ctop_task_id: "eval_0",
        verified_by: "sandbox",
        verification_detail: "axiom sandbox, engine tier1_wasi_cranelift",
        previous_seal: "",
    };

    let record = ProvenanceAttestation::generate(details("agent-one"));
    assert!(
        record.verify("auth::service::validate_token", "Tighten the guard"),
        "the record must verify as issued"
    );

    let mut edited = record.clone();
    edited.agent_identity = "agent-two".to_string();
    assert!(
        !edited.verify("auth::service::validate_token", "Tighten the guard"),
        "reassigning authorship in a stored record must break the seal"
    );

    let other = ProvenanceAttestation::generate(details("agent-two"));
    assert_ne!(
        record.seal, other.seal,
        "two records differing only in who issued them must not share a seal"
    );
}
