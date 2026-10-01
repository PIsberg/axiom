use axiom_proto::{AgentHandoff, AgentIntentOp, ProvenanceAttestation};

#[test]
fn test_agent_handoff_serialization_and_digest() {
    let handoff = AgentHandoff {
        handoff_id: "handoff_agent1_001".to_string(),
        sender_agent: "agent_alpha".to_string(),
        recipient_agent: Some("agent_beta".to_string()),
        intent_op: AgentIntentOp::Refactor,
        target_symbol: "auth::service::validate_token".to_string(),
        target_cas_hash: Some("3f8a42bc11223344556677889900aabb".to_string()),
        pre_merkle_root: "merkle_root_pre_1234567890abcdef".to_string(),
        post_merkle_root: Some("merkle_root_post_abcdef1234567890".to_string()),
        blast_tests: vec![
            "auth::test::test_validate_token".to_string(),
            "auth::test::test_login_flow".to_string(),
        ],
        ctop_hash: Some("ctop_hash_pass_998877".to_string()),
        context_payload: None,
        timestamp: "2026-10-01T15:00:00Z".to_string(),
    };

    // Deterministic digest
    let d1 = handoff.compute_digest();
    let d2 = handoff.compute_digest();
    assert_eq!(d1, d2);
    assert!(d1.starts_with("blake3_handoff_"));

    // Dense wire format
    let wire = handoff.to_dense_wire();
    assert!(wire.contains("OP: Refactor"));
    assert!(wire.contains("SYM: auth::service::validate_token"));
    assert!(wire.contains("TESTS: 2"));

    // JSON roundtrip
    let json_str = serde_json::to_string(&handoff).expect("must serialize");
    let deserialized: AgentHandoff = serde_json::from_str(&json_str).expect("must deserialize");
    assert_eq!(deserialized, handoff);
}

#[test]
fn test_provenance_attestation_from_typed_handoff() {
    let handoff = AgentHandoff {
        handoff_id: "handoff_tx_42".to_string(),
        sender_agent: "antigravity-agent".to_string(),
        recipient_agent: None,
        intent_op: AgentIntentOp::FixBug,
        target_symbol: "crypto::token::verify".to_string(),
        target_cas_hash: Some("hash123".to_string()),
        pre_merkle_root: "merkle_pre_1111".to_string(),
        post_merkle_root: Some("merkle_post_2222".to_string()),
        blast_tests: vec!["crypto::test_verify".to_string()],
        ctop_hash: Some("ctop_proof_001".to_string()),
        context_payload: None,
        timestamp: "2026-10-01T15:30:00Z".to_string(),
    };

    let attestation = ProvenanceAttestation::generate_from_handoff(
        &handoff,
        "task_verify_88",
        "sandbox",
        "axiom ran cargo test",
        "",
    );

    assert_eq!(attestation.symbol_path, "crypto::token::verify");
    assert_eq!(attestation.parent_merkle_root, "merkle_pre_1111");
    assert_eq!(attestation.commit_merkle_root, "merkle_post_2222");
    assert_eq!(attestation.agent_identity, "antigravity-agent");

    // Verify against typed handoff
    assert!(attestation.verify_against_handoff(&handoff));

    // Tampering with any field in handoff breaks verification
    let mut tampered = handoff.clone();
    tampered.intent_op = AgentIntentOp::AddFeature;
    assert!(!attestation.verify_against_handoff(&tampered));

    let mut tampered_target = handoff.clone();
    tampered_target.target_symbol = "crypto::token::other".to_string();
    assert!(!attestation.verify_against_handoff(&tampered_target));
}
