use axiom_crdt::{TreeCrdt, SwarmRelay, merge_statements_3way};

#[test]
fn test_statement_level_3way_merge_non_conflicting() {
    let base = r#"pub fn process_order(order: Order) -> Result<()> {
    validate_order(&order)?;
    charge_payment(&order)?;
    save_order(&order)?;
    Ok(())
}"#;

    // Agent 1 adds telemetry at the top
    let local = r#"pub fn process_order(order: Order) -> Result<()> {
    metrics::increment("orders_processed");
    validate_order(&order)?;
    charge_payment(&order)?;
    save_order(&order)?;
    Ok(())
}"#;

    // Agent 2 adds notification before save
    let remote = r#"pub fn process_order(order: Order) -> Result<()> {
    validate_order(&order)?;
    charge_payment(&order)?;
    send_confirmation_email(&order)?;
    save_order(&order)?;
    Ok(())
}"#;

    let (merged, has_conflicts) = merge_statements_3way(base, local, remote);
    assert!(!has_conflicts, "Should merge non-overlapping statements without conflicts");
    assert!(merged.contains("metrics::increment(\"orders_processed\");"));
    assert!(merged.contains("send_confirmation_email(&order)?;"));
    assert!(merged.contains("validate_order(&order)?;"));
    assert!(merged.contains("save_order(&order)?;"));
}

#[test]
fn test_statement_level_3way_merge_conflicting() {
    let base = "let timeout_secs = 30;";
    let local = "let timeout_secs = 60;";
    let remote = "let timeout_secs = 90;";

    let (merged, has_conflicts) = merge_statements_3way(base, local, remote);
    assert!(has_conflicts, "Conflicting edits to the same statement must flag conflict");
    assert!(merged.contains("<<<<<<< LOCAL"));
    assert!(merged.contains("let timeout_secs = 60;"));
    assert!(merged.contains("======="));
    assert!(merged.contains("let timeout_secs = 90;"));
    assert!(merged.contains(">>>>>>> REMOTE"));
}

#[test]
fn test_crdt_node_content_3way_merge() {
    let crdt = TreeCrdt::new(1);
    let base = "fn init() {\n    let x = 1;\n}";
    crdt.insert_node("root", "fn_init", "crate::init", "function", base);

    // Agent 1 updates locally
    let local = "fn init() {\n    log::info!(\"init\");\n    let x = 1;\n}";
    crdt.update_node("fn_init", local);

    // Remote changes
    let remote = "fn init() {\n    let x = 1;\n    run_setup();\n}";

    let (merged, has_conflicts) = crdt.merge_node_content_3way("fn_init", base, remote);
    assert!(!has_conflicts);
    assert!(merged.contains("log::info!(\"init\");"));
    assert!(merged.contains("run_setup();"));
}

#[tokio::test]
async fn test_swarm_relay_broadcast_and_convergence() {
    let relay = SwarmRelay::new(256);
    let mut rx1 = relay.subscribe();
    let mut rx2 = relay.subscribe();

    let agent1 = TreeCrdt::new(1);
    let agent2 = TreeCrdt::new(2);

    let op1 = agent1.insert_node("root", "user_service", "service::User", "class", "class User {}");
    relay.broadcast(op1.clone());

    let op2 = agent2.insert_node("root", "auth_service", "service::Auth", "class", "class Auth {}");
    relay.broadcast(op2.clone());

    // Verify broadcast reception
    let recv1_a = rx1.recv().await.unwrap();
    let recv1_b = rx1.recv().await.unwrap();
    assert_eq!(recv1_a, op1);
    assert_eq!(recv1_b, op2);

    let recv2_a = rx2.recv().await.unwrap();
    let recv2_b = rx2.recv().await.unwrap();
    assert_eq!(recv2_a, op1);
    assert_eq!(recv2_b, op2);

    // Sync all agents to convergence
    let agents = vec![agent1.clone(), agent2.clone()];
    relay.sync_all(&agents);

    let root1 = agent1.compute_tree_merkle_root();
    let root2 = agent2.compute_tree_merkle_root();
    assert_eq!(root1, root2, "All agents synchronized via SwarmRelay must produce identical Merkle roots");
    assert_eq!(agent1.active_nodes_count(), 3); // root + user_service + auth_service
    assert_eq!(agent2.active_nodes_count(), 3);
}
