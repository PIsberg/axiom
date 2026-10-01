use axiom_crdt::{LamportTime, SwarmEngine, TreeOp};

#[test]
fn test_tree_op_binary_roundtrip() {
    let ts = LamportTime {
        time: 42,
        agent_id: 7,
    };

    let op_ins = TreeOp::Insert {
        op_id: "op_ins_7_42".to_string(),
        parent_id: "root".to_string(),
        node_id: "node_func_1".to_string(),
        symbol: "auth::service::validate".to_string(),
        kind: "function".to_string(),
        content: "pub fn validate() -> bool { true }".to_string(),
        timestamp: ts,
    };

    let op_upd = TreeOp::Update {
        op_id: "op_upd_7_43".to_string(),
        node_id: "node_func_1".to_string(),
        new_content: "pub fn validate() -> bool { false }".to_string(),
        timestamp: LamportTime {
            time: 43,
            agent_id: 7,
        },
    };

    let op_del = TreeOp::Delete {
        op_id: "op_del_7_44".to_string(),
        node_id: "node_func_1".to_string(),
        timestamp: LamportTime {
            time: 44,
            agent_id: 7,
        },
    };

    // Test individual encode/decode
    for original in [&op_ins, &op_upd, &op_del] {
        let bin = original.encode_binary();
        let decoded = TreeOp::decode_binary(&bin).expect("must decode valid binary");
        assert_eq!(&decoded, original);
    }
}

#[test]
fn test_tree_op_batch_binary_wire_efficiency() {
    let mut ops = Vec::new();
    for i in 0..100 {
        ops.push(TreeOp::Insert {
            op_id: format!("op_ins_{i}"),
            parent_id: "root".to_string(),
            node_id: format!("node_{i}"),
            symbol: format!("module::func_{i}"),
            kind: "function".to_string(),
            content: format!("fn func_{i}() {{ {i} }}"),
            timestamp: LamportTime {
                time: i as u64,
                agent_id: (i % 10) as u32,
            },
        });
    }

    let bin_batch = TreeOp::encode_batch(&ops);
    let decoded = TreeOp::decode_batch(&bin_batch).expect("must decode batch");
    assert_eq!(decoded.len(), ops.len());
    assert_eq!(decoded, ops);

    // Verify wire size reduction vs JSON
    let json_bytes = serde_json::to_vec(&ops).expect("must serialize json");
    assert!(
        bin_batch.len() < json_bytes.len(),
        "Binary wire ({} bytes) must be significantly smaller than JSON ({} bytes)",
        bin_batch.len(),
        json_bytes.len()
    );
}

#[test]
fn test_tree_op_corrupt_binary_handling() {
    assert!(TreeOp::decode_binary(&[]).is_err());
    assert!(TreeOp::decode_binary(&[0x00, 0x01]).is_err()); // invalid magic
    assert!(TreeOp::decode_batch(&[0x41, 0x58, 0xFF]).is_err()); // truncated batch
}

#[tokio::test]
async fn test_swarm_simulation_over_binary_wire() {
    let mut engine = SwarmEngine::new(10);
    let report = engine
        .simulate_concurrent_swarm_binary(20)
        .await
        .expect("binary swarm simulation must succeed");

    assert_eq!(report.agent_count, 10);
    assert!(report.total_operations > 0);
    assert!(
        report.converged,
        "all agents must reach 100% Merkle convergence"
    );
    assert_eq!(report.merge_conflicts_count, 0);
}
