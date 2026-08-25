use axiom_crdt::{SwarmEngine, TreeCrdt};

#[test]
fn test_crdt_local_mutation() {
    let tree = TreeCrdt::new(1);
    tree.insert_node(
        "root",
        "fn_1",
        "auth::login",
        "function",
        "pub fn login() {}",
    );
    assert_eq!(tree.active_nodes_count(), 2); // root + fn_1

    tree.update_node("fn_1", "pub fn login(user: &str) {}");
    assert_eq!(tree.active_nodes_count(), 2);

    let root = tree.compute_tree_merkle_root();
    assert!(!root.is_empty());
}

#[test]
fn test_crdt_commutative_merge() {
    let agent1 = TreeCrdt::new(1);
    let agent2 = TreeCrdt::new(2);

    let op1 = agent1.insert_node(
        "root",
        "fn_auth",
        "auth::check",
        "function",
        "fn check() {}",
    );
    let op2 = agent2.insert_node(
        "root",
        "fn_billing",
        "billing::pay",
        "function",
        "fn pay() {}",
    );

    // Agent 1 receives op2, Agent 2 receives op1
    agent1.apply_op(op2);
    agent2.apply_op(op1);

    // Both must converge to the identical Merkle root
    assert_eq!(
        agent1.compute_tree_merkle_root(),
        agent2.compute_tree_merkle_root()
    );
    assert_eq!(agent1.active_nodes_count(), 3);
    assert_eq!(agent2.active_nodes_count(), 3);
}

#[tokio::test]
async fn test_swarm_simulation_50_agents() {
    let mut swarm = SwarmEngine::new(50);
    let report = swarm
        .simulate_concurrent_swarm(10)
        .await
        .expect("Swarm simulation failed");

    assert!(report.converged);
    assert_eq!(report.merge_conflicts_count, 0);
    assert_eq!(report.agent_count, 50);
}

#[test]
fn test_crdt_out_of_order_parent_and_child() {
    let agent1 = TreeCrdt::new(1);
    let agent2 = TreeCrdt::new(2);

    let op_parent = agent1.insert_node("root", "mod_1", "auth", "module", "");
    let op_child = agent1.insert_node("mod_1", "fn_1", "auth::login", "function", "fn login() {}");

    // Agent 2 receives child FIRST, then parent
    agent2.apply_op(op_child.clone());
    agent2.apply_op(op_parent.clone());

    assert_eq!(
        agent1.compute_tree_merkle_root(),
        agent2.compute_tree_merkle_root()
    );
    assert_eq!(agent1.active_nodes_count(), 3);
    assert_eq!(agent2.active_nodes_count(), 3);
}

#[test]
fn test_crdt_reparenting_and_metadata_update() {
    let agent1 = TreeCrdt::new(1);
    let agent2 = TreeCrdt::new(2);

    let op1 = agent1.insert_node("root", "node_x", "old::name", "function", "fn x() {}");
    agent2.apply_op(op1);

    // Agent 1 reparents and renames node_x
    let op2 = agent1.insert_node("root", "node_x", "new::name", "method", "fn x() { 1 }");
    agent2.apply_op(op2);

    assert_eq!(
        agent1.compute_tree_merkle_root(),
        agent2.compute_tree_merkle_root()
    );
}
