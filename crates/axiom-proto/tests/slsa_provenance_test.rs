use axiom_proto::{NewAttestation, ProvenanceAttestation};

#[test]
fn test_slsa_provenance_statement_structure() {
    let new_att = NewAttestation {
        parent_merkle_root: "root_prev_123",
        commit_merkle_root: "root_curr_456",
        agent_identity: "antigravity-agent",
        prompt: "Refactor auth token validation",
        symbol_path: "auth::service::validate_token",
        ctop_task_id: "task-001",
        verified_by: "sandbox",
        verification_detail: "cargo test --package auth",
        previous_seal: "",
    };

    let att = ProvenanceAttestation::generate(new_att);
    let slsa = att.to_slsa_statement();

    assert_eq!(slsa["_type"], "https://in-toto.io/Statement/v1");
    assert_eq!(slsa["predicateType"], "https://slsa.dev/provenance/v1");

    let subject = slsa["subject"].as_array().expect("subject array");
    assert_eq!(subject.len(), 1);
    assert_eq!(subject[0]["name"], "auth::service::validate_token");
    assert_eq!(subject[0]["digest"]["merkleRoot"], "root_curr_456");
    assert_eq!(subject[0]["digest"]["seal"], att.seal);

    let pred = &slsa["predicate"];
    assert_eq!(
        pred["buildDefinition"]["buildType"],
        "https://axiom.dev/provenance/v1"
    );
    assert_eq!(
        pred["buildDefinition"]["externalParameters"]["agentIdentity"],
        "antigravity-agent"
    );
    assert_eq!(
        pred["buildDefinition"]["externalParameters"]["symbolPath"],
        "auth::service::validate_token"
    );
    assert_eq!(
        pred["runDetails"]["builder"]["id"],
        "https://axiom.dev/verifier/v1"
    );
    assert_eq!(pred["runDetails"]["metadata"]["invocationId"], att.seal);

    let byproducts = pred["runDetails"]["byproducts"]
        .as_array()
        .expect("byproducts");
    assert_eq!(byproducts[0]["verifiedBy"], "sandbox");
    assert_eq!(
        byproducts[0]["verificationDetail"],
        "cargo test --package auth"
    );
}
