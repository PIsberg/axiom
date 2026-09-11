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

// The three fields below are hashed by `seal_over` and had no edited-field test,
// so a change that quietly dropped one from the digest would not have been
// caught by this file, which is the file named after catching exactly that.

#[test]
fn editing_the_agent_identity_breaks_the_seal() {
    let mut r = record();
    r.agent_identity = "agent-B".to_string();
    assert!(
        !r.verify(SYMBOL, PROMPT),
        "the identity a record is issued under must not be editable afterwards; \
         it is unverified when it arrives, and the seal is the only thing that \
         makes storing it acceptable"
    );
}

#[test]
fn editing_the_task_id_breaks_the_seal() {
    let mut r = record();
    r.ctop_proof_hash = "eval_8".to_string();
    assert!(
        !r.verify(SYMBOL, PROMPT),
        "the task id names the evaluation the record rests on; pointing it at a \
         different one is the whole forgery"
    );
}

#[test]
fn editing_the_previous_seal_breaks_the_seal() {
    let mut r = record();
    r.previous_seal = "blake3_seal_somethingelse".to_string();
    assert!(
        !r.verify(SYMBOL, PROMPT),
        "the chain is what makes a deleted record visible; a repairable link is \
         no link"
    );
}

// Two stored fields are outside `seal_over` and always were. Both are pure
// functions of fields it does cover, so `verify` re-derives them rather than the
// seal format changing, which would invalidate every record ever issued.

#[test]
fn editing_the_sandbox_trace_hash_is_rejected() {
    let mut r = record();
    r.sandbox_trace_hash = "trace:0000000000000000000000000000000f".to_string();
    assert!(
        !r.verify(SYMBOL, PROMPT),
        "the trace digest names what was checked and how; a record that still \
         verifies with an edited one vouches for a verification that did not \
         happen, which is the defect that widened seal_over one field along"
    );
}

#[test]
fn editing_the_prompt_digest_is_rejected() {
    let mut r = record();
    r.prompt_digest = "blake3:0000000000000000000000000000000f".to_string();
    assert!(
        !r.verify(SYMBOL, PROMPT),
        "prompt_digest is published as externalParameters.promptDigest in the \
         SLSA statement, so an edited one is exported as though the seal \
         vouched for it"
    );
}

/// The derived digests are re-derived, not merely compared to a copy.
///
/// If `verify` compared `prompt_digest` against a second stored copy, or against
/// itself, both tests above would pass and neither would establish anything.
/// This pins that the value actually tracks its inputs.
#[test]
fn the_derived_digests_track_the_fields_they_digest() {
    let a = record();

    let b = ProvenanceAttestation::generate(NewAttestation {
        parent_merkle_root: "root_parent",
        commit_merkle_root: "root_commit",
        agent_identity: "agent-A",
        prompt: "Tighten the guard",
        symbol_path: SYMBOL,
        ctop_task_id: "eval_7",
        // The one difference, and it is an input to the trace digest.
        verified_by: "sandbox",
        verification_detail: "cargo test",
        previous_seal: "",
    });

    assert_ne!(
        a.sandbox_trace_hash, b.sandbox_trace_hash,
        "a record checked by the sandbox and one merely reported must not share \
         a trace digest"
    );
    assert_eq!(
        a.prompt_digest, b.prompt_digest,
        "two records for one prompt share its digest, which is what lets a \
         reader group them without holding the prompt text"
    );
}
