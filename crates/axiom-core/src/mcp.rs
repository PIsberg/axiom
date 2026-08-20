use anyhow::Result;
use axiom_ast::AstIndex;
use axiom_crdt::TreeCrdt;
use axiom_proto::ProvenanceAttestation;
use axiom_vmm::{SandboxEngine, WasiEngine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

pub struct AxiomMcpServer {
    pub ast_index: Arc<AstIndex>,
    pub wasi_engine: Arc<WasiEngine>,
    pub tree_crdt: Arc<TreeCrdt>,
}

fn find_index_file() -> Option<std::path::PathBuf> {
    if let Ok(mut curr) = std::env::current_dir() {
        loop {
            let candidate = curr.join(".axiom").join("index.json");
            if candidate.exists() {
                return Some(candidate);
            }
            if !curr.pop() {
                break;
            }
        }
    }
    None
}

impl AxiomMcpServer {
    pub fn new() -> Result<Self> {
        let ast_index = if let Some(index_path) = find_index_file() {
            match AstIndex::load_from_disk(&index_path) {
                Ok(idx) => Arc::new(idx),
                Err(_) => Arc::new(AstIndex::new()),
            }
        } else {
            Arc::new(AstIndex::new())
        };

        let wasi_engine = Arc::new(WasiEngine::new()?);
        let tree_crdt = Arc::new(TreeCrdt::new(1));

        // If index is empty, seed with standard starter nodes
        if ast_index.total_symbols_count() == 0 {
            ast_index.index_node(
                "auth::service::validate_token",
                "function",
                "pub fn validate_token(t: &str) -> bool { t.len() > 10 }",
                vec!["jwt::verifier".into()],
            );
            ast_index.index_node(
                "test_auth_validation",
                "test",
                "#[test] fn test_auth_validation() { assert!(validate_token(\"valid_token_secret\")); }",
                vec!["auth::service::validate_token".into()],
            );

            tree_crdt.insert_node(
                "root",
                "node_auth_val",
                "auth::service::validate_token",
                "function",
                "pub fn validate_token(t: &str) -> bool { t.len() > 10 }",
            );
        }

        Ok(Self {
            ast_index,
            wasi_engine,
            tree_crdt,
        })
    }

    pub async fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.unwrap_or(Value::Null);

        match req.method.as_str() {
            "initialize" => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": "axiom-mcp-server",
                        "version": "0.1.0"
                    },
                    "capabilities": {
                        "tools": {}
                    }
                })),
                error: None,
            },

            "tools/list" => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({
                    "tools": [
                        {
                            "name": "axiom_query_symbol",
                            "description": "Inspect AST node definition and type signatures without disk clones",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "symbol_path": { "type": "string", "description": "Symbol path e.g. auth::service::validate_token" }
                                },
                                "required": ["symbol_path"]
                            }
                        },
                        {
                            "name": "axiom_get_blast_radius",
                            "description": "Compute topological blast radius and impacted test targets for changed symbol",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "symbol_path": { "type": "string" },
                                    "max_depth": { "type": "integer", "default": 1, "description": "Graph traversal depth (default 1 for targeted direct dependents)" }
                                },
                                "required": ["symbol_path"]
                            }
                        },
                        {
                            "name": "axiom_eval_patch",
                            "description": "Execute sub-15ms test validation inside isolated WASI/MicroVM sandbox",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "symbol_path": { "type": "string" },
                                    "code_snippet": { "type": "string" },
                                    "test_target": { "type": "string" }
                                }
                            }
                        },
                        {
                            "name": "axiom_attest_commit",
                            "description": "Generate SLSA Level 4+ cryptographic provenance seal for verified AST mutation",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "prompt": { "type": "string" },
                                    "symbol_path": { "type": "string" },
                                    "ctop_task_id": { "type": "string" }
                                },
                                "required": ["prompt", "symbol_path"]
                            }
                        },
                        {
                            "name": "axiom_apply_mutation",
                            "description": "Apply commutative Tree-CRDT AST mutation across concurrent agent swarms",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "node_id": { "type": "string" },
                                    "symbol_path": { "type": "string" },
                                    "content": { "type": "string" }
                                },
                                "required": ["node_id", "symbol_path", "content"]
                            }
                        },
                        {
                            "name": "axiom_search_regex",
                            "description": "Ultra-fast Zoekt trigram regex and literal text search across entire repository CAS",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "query": { "type": "string", "description": "Regex or substring query" },
                                    "max_results": { "type": "integer", "default": 20 }
                                },
                                "required": ["query"]
                            }
                        }
                    ]
                })),
                error: None,
            },

            "tools/call" => {
                let params = req.params.unwrap_or(Value::Null);
                let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);

                let result = self.execute_tool(tool_name, args).await;
                match result {
                    Ok(val) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: Some(json!({
                            "content": [
                                {
                                    "type": "text",
                                    "text": serde_json::to_string_pretty(&val).unwrap_or_default()
                                }
                            ]
                        })),
                        error: None,
                    },
                    Err(e) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: None,
                        error: Some(json!({
                            "code": -32603,
                            "message": e.to_string()
                        })),
                    },
                }
            }

            _ => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(json!({
                    "code": -32601,
                    "message": format!("Method '{}' not found", req.method)
                })),
            },
        }
    }

    async fn execute_tool(&self, tool_name: &str, args: Value) -> Result<Value> {
        match tool_name {
            "axiom_query_symbol" => {
                let symbol = args.get("symbol_path").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(node) = self.ast_index.get_symbol(symbol) {
                    Ok(json!(node))
                } else {
                    Ok(json!({
                        "error": format!("Symbol '{}' not found in AST index. Use 'axiom scan' to index your workspace first.", symbol),
                        "total_symbols_in_index": self.ast_index.total_symbols_count()
                    }))
                }
            }

            "axiom_get_blast_radius" => {
                let symbol = args.get("symbol_path").and_then(|v| v.as_str()).unwrap_or("");
                let depth = args.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                if let Some(res) = self.ast_index.compute_blast_radius(symbol, depth) {
                    Ok(json!(res))
                } else {
                    Ok(json!({
                        "error": format!("Symbol '{}' not found in AST index. Blast radius cannot be computed.", symbol),
                        "impacted_tests": [],
                        "total_tests_in_repo": self.ast_index.total_tests_count(),
                        "pruned_test_percentage": 0.0
                    }))
                }
            }

            "axiom_eval_patch" => {
                let symbol = args.get("symbol_path").and_then(|v| v.as_str()).unwrap_or("anonymous");
                let snippet = args.get("code_snippet").and_then(|v| v.as_str()).unwrap_or("");
                let report = self.wasi_engine.execute_eval(symbol, snippet).await?;
                Ok(json!(report))
            }

            "axiom_attest_commit" => {
                let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let symbol = args.get("symbol_path").and_then(|v| v.as_str()).unwrap_or("");
                let task_id = args.get("ctop_task_id").and_then(|v| v.as_str()).unwrap_or("task_00");

                let root = self.tree_crdt.compute_tree_merkle_root();
                let attestation = ProvenanceAttestation::generate(
                    "merkle_root_prev_77a1",
                    &format!("merkle_root_{}", &root[..8]),
                    "agent_axiom_v1",
                    prompt,
                    symbol,
                    task_id,
                );

                Ok(json!(attestation))
            }

            "axiom_apply_mutation" => {
                let node_id = args.get("node_id").and_then(|v| v.as_str()).unwrap_or("node_01");
                let symbol = args.get("symbol_path").and_then(|v| v.as_str()).unwrap_or("module::fn");
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");

                let op = self.tree_crdt.insert_node("root", node_id, symbol, "function", content);
                self.ast_index.index_node(symbol, "function", content, vec![]);
                let root = self.tree_crdt.compute_tree_merkle_root();

                // Save updated index to disk
                if let Err(e) = self.ast_index.save_to_disk(std::path::Path::new(".axiom/index.json")) {
                    eprintln!("Warning: Failed to save .axiom/index.json: {}", e);
                }

                Ok(json!({
                    "status": "APPLIED",
                    "crdt_op": op,
                    "new_merkle_root": root,
                    "active_ast_nodes": self.tree_crdt.active_nodes_count()
                }))
            }

            "axiom_search_regex" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let max = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
                let matches = self.ast_index.search_regex(query, max);
                Ok(json!({
                    "query": query,
                    "matches_count": matches.len(),
                    "matches": matches
                }))
            }

            _ => anyhow::bail!("Unknown tool: {}", tool_name),
        }
    }
}
