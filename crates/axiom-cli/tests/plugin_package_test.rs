//! Verifies that the packaged Axiom plugin conforms to the open customization standard.

use std::path::Path;

#[test]
fn plugin_manifest_and_mcp_config_exist_and_are_valid_json() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let plugin_dir = repo_root.join(".agents").join("plugins").join("axiom");

    // 1. plugin.json
    let plugin_json_path = plugin_dir.join("plugin.json");
    assert!(
        plugin_json_path.exists(),
        "plugin.json must exist at {plugin_json_path:?}"
    );
    let plugin_content = std::fs::read_to_string(&plugin_json_path).unwrap();
    let plugin_val: serde_json::Value =
        serde_json::from_str(&plugin_content).expect("plugin.json must be valid JSON");
    assert_eq!(
        plugin_val.get("name").and_then(|v| v.as_str()),
        Some("axiom-engine")
    );

    // 2. mcp_config.json
    let mcp_config_path = plugin_dir.join("mcp_config.json");
    assert!(
        mcp_config_path.exists(),
        "mcp_config.json must exist at {mcp_config_path:?}"
    );
    let mcp_content = std::fs::read_to_string(&mcp_config_path).unwrap();
    let mcp_val: serde_json::Value =
        serde_json::from_str(&mcp_content).expect("mcp_config.json must be valid JSON");
    let server = mcp_val
        .get("mcpServers")
        .and_then(|s| s.get("axiom"))
        .expect("axiom MCP server must be configured");
    assert_eq!(
        server.get("command").and_then(|v| v.as_str()),
        Some("axiom")
    );
    let args = server
        .get("args")
        .and_then(|a| a.as_array())
        .expect("args must be array");
    assert!(args.iter().any(|a| a.as_str() == Some("serve")));

    // 3. rules/AGENTS.md
    let agents_md_path = plugin_dir.join("rules").join("AGENTS.md");
    assert!(
        agents_md_path.exists(),
        "rules/AGENTS.md must exist at {agents_md_path:?}"
    );
    let agents_md = std::fs::read_to_string(&agents_md_path).unwrap();
    assert!(agents_md.contains("axiom_query_symbol"));
    assert!(agents_md.contains("axiom_get_blast_radius"));
    assert!(agents_md.contains("axiom_eval_patch"));

    // 4. skills/axiom-engine/SKILL.md
    let skill_path = plugin_dir
        .join("skills")
        .join("axiom-engine")
        .join("SKILL.md");
    assert!(skill_path.exists(), "SKILL.md must exist at {skill_path:?}");
    let skill_md = std::fs::read_to_string(&skill_path).unwrap();
    assert!(skill_md.contains("name: axiom-engine"));
    assert!(skill_md.contains("axiom_attest_commit"));
}
