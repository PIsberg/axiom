//! Every tool the server declares must be named in the docs an agent reads.
//!
//! `declared_tools_are_dispatched.rs` pins that a declared tool answers. This
//! pins the other end: that a declared tool is documented. `axiom_run_tests`
//! shipped and the README kept saying seven tools while USAGE_GUIDE.md
//! documented seven and stopped, so the one tool that lets axiom vouch for a
//! test run was invisible to anyone reading the docs to decide what to call.
//!
//! Prose drifts silently; a tool list does not, because it lives in code. So the
//! code is the source and the docs are checked against it, never the reverse.

use axiom_core::{AxiomMcpServer, mcp::JsonRpcRequest};
use serde_json::json;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // crates/axiom-core -> crates -> repo root
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the crate must sit two levels below the repository root")
        .to_path_buf()
}

fn read_doc(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

async fn declared_tool_names() -> Vec<String> {
    let server = AxiomMcpServer::with_index(None).expect("server");
    let resp = server
        .handle_request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/list".into(),
            params: None,
        })
        .await;
    resp.result.expect("tools/list must return a result")["tools"]
        .as_array()
        .expect("tools must be an array")
        .iter()
        .map(|t| t["name"].as_str().expect("named").to_string())
        .collect()
}

#[tokio::test]
async fn the_readme_names_every_declared_tool() {
    let names = declared_tool_names().await;
    let readme = read_doc("README.md");

    let missing: Vec<&String> = names.iter().filter(|n| !readme.contains(*n)).collect();
    assert!(
        missing.is_empty(),
        "README.md does not mention these declared tools: {missing:?}. \
         A tool an agent can call but cannot read about may as well not exist."
    );

    // The README states the count in a heading. A tool added without touching
    // that heading leaves the docs claiming a smaller surface than ships.
    let heading = format!("## The {} MCP Tools", spelled(names.len()));
    assert!(
        readme.contains(&heading),
        "README.md should carry the heading {heading:?} for the {} declared tools",
        names.len()
    );
}

#[tokio::test]
async fn the_usage_guide_documents_every_declared_tool() {
    let names = declared_tool_names().await;
    let guide = read_doc("docs/USAGE_GUIDE.md");

    // A mention is not documentation. The guide gives each tool its own
    // numbered section, so require the section rather than the string.
    let missing: Vec<&String> = names
        .iter()
        .filter(|n| !guide.contains(&format!("`{n}`\n")))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/USAGE_GUIDE.md has no section heading for these declared tools: \
         {missing:?}. The guide is what an agent reads to decide what to call."
    );
}

/// Small numbers as words, because that is how the heading reads.
fn spelled(n: usize) -> String {
    match n {
        5 => "Five".into(),
        6 => "Six".into(),
        7 => "Seven".into(),
        8 => "Eight".into(),
        9 => "Nine".into(),
        10 => "Ten".into(),
        other => other.to_string(),
    }
}
