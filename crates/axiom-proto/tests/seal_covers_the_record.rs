//! The seal has to cover every stored field, or editing one it misses leaves
//! a record that still verifies.
//!
//! Measured on 2026-08-23 against the shipped binary: a `reported` record was
//! attested, the ledger's `verified_by` was edited to `sandbox` and its
//! `verification_detail` to name a sandbox run, and `axiom verify` printed
//! `ATTESTATION VALID ... Checked by: sandbox`. The seal was a digest over the
//! roots, identity, prompt, symbol and task id only, so the field that carries
//! the whole distinction between "axiom ran it" and "an agent said so" sat
//! outside it. The signature covered those fields, but only when a key was
//! configured, which is not the default.

use axiom_proto::{NewAttestation, ProvenanceAttestation};

fn record() -> ProvenanceAttestation {
    ProvenanceAttestation::generate(NewAttestation {
        parent_merkle_root: "root_parent",
        commit_merkle_root: "root_commit",
        agent_identity: "agent-A",
        prompt: "Tighten the guard",
        symbol_path: "auth::service::validate_token",
        ctop_task_id: "eval_7",
        verified_by: "reported",
        verification_detail: "cargo test",
        previous_seal: "",
    })
}

const SYMBOL: &str = "auth::service::validate_token";
const PROMPT: &str = "Tighten the guard";

#[test]
fn a_fresh_record_verifies_against_its_own_inputs() {
    assert!(record().verify(SYMBOL, PROMPT));
}

#[test]
fn editing_the_verification_kind_breaks_the_seal() {
    let mut r = record();
    r.verified_by = "sandbox".to_string();
    assert!(
        !r.verify(SYMBOL, PROMPT),
        "changing 'reported' to 'sandbox' must not still verify: it is the whole claim"
    );
}

#[test]
fn editing_the_verification_detail_breaks_the_seal() {
    let mut r = record();
    r.verification_detail = "axiom sandbox, engine tier1_native_rustc".to_string();
    assert!(!r.verify(SYMBOL, PROMPT));
}

#[test]
fn editing_the_timestamp_breaks_the_seal() {
    let mut r = record();
    r.timestamp = "2020-01-01T00:00:00+00:00".to_string();
    assert!(
        !r.verify(SYMBOL, PROMPT),
        "the time a record claims to have been issued is part of what it claims"
    );
}

#[test]
fn editing_a_merkle_root_breaks_the_seal() {
    let mut r = record();
    r.commit_merkle_root = "root_something_else".to_string();
    assert!(!r.verify(SYMBOL, PROMPT));
    let mut r = record();
    r.parent_merkle_root = "root_forged".to_string();
    assert!(!r.verify(SYMBOL, PROMPT));
}

#[test]
fn a_different_prompt_or_symbol_still_does_not_verify() {
    assert!(!record().verify(SYMBOL, "a different prompt"));
    assert!(!record().verify("some::other::symbol", PROMPT));
}

#[test]
fn the_prompt_digest_is_a_digest_of_the_prompt() {
    // Not a slice of the seal wearing a prompt-shaped label. Two records for
    // the same prompt but different symbols share a prompt digest; two for
    // different prompts do not.
    let same_prompt_other_symbol = ProvenanceAttestation::generate(NewAttestation {
        parent_merkle_root: "root_parent",
        commit_merkle_root: "root_commit",
        agent_identity: "agent-A",
        prompt: PROMPT,
        symbol_path: "some::other::symbol",
        ctop_task_id: "eval_7",
        verified_by: "reported",
        verification_detail: "cargo test",
        previous_seal: "",
    });
    assert_eq!(
        record().prompt_digest,
        same_prompt_other_symbol.prompt_digest,
        "the prompt digest must depend on the prompt and nothing else"
    );

    let other_prompt = ProvenanceAttestation::generate(NewAttestation {
        parent_merkle_root: "root_parent",
        commit_merkle_root: "root_commit",
        agent_identity: "agent-A",
        prompt: "a different prompt",
        symbol_path: SYMBOL,
        ctop_task_id: "eval_7",
        verified_by: "reported",
        verification_detail: "cargo test",
        previous_seal: "",
    });
    assert_ne!(record().prompt_digest, other_prompt.prompt_digest);
}
