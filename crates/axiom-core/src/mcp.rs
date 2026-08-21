use anyhow::Result;
use axiom_ast::{AstIndex, SearchMode};
use axiom_crdt::TreeCrdt;
use axiom_proto::{CtopStatus, ProvenanceAttestation};
use axiom_vmm::{SandboxEngine, WasiEngine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

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

/// Where issued attestations are recorded, beside the index they describe.
pub fn attestation_ledger_path() -> PathBuf {
    PathBuf::from(".axiom").join("attestations.json")
}

/// Every attestation issued so far. A missing ledger is an empty one: nothing
/// has been attested yet, which is different from failing to read it.
pub fn load_attestations() -> Result<Vec<ProvenanceAttestation>> {
    load_attestations_from(&attestation_ledger_path())
}

/// As above, from an explicit ledger. Kept separate so a caller that must not
/// touch the working directory, a test above all, can point somewhere else.
pub fn load_attestations_from(path: &std::path::Path) -> Result<Vec<ProvenanceAttestation>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&raw)?)
}

/// Append one attestation to the ledger.
pub fn append_attestation(attestation: &ProvenanceAttestation) -> Result<()> {
    append_attestation_to(&attestation_ledger_path(), attestation)
}

pub fn append_attestation_to(
    path: &std::path::Path,
    attestation: &ProvenanceAttestation,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Read-modify-write, so two agents appending at once would otherwise drop
    // one of the records. The lock makes the sequence atomic.
    let _lock = axiom_ast::IndexLock::acquire(path)?;
    let mut all = load_attestations_from(path).unwrap_or_default();
    all.push(attestation.clone());
    std::fs::write(path, serde_json::to_string_pretty(&all)?)?;
    Ok(())
}

/// Where the signing key comes from, if anywhere.
///
/// `AXIOM_SIGNING_KEY` holds the key itself; `AXIOM_SIGNING_KEY_FILE` names a
/// file holding it. Neither defaults to anywhere inside the workspace, and that
/// is deliberate. The threat a signature addresses is someone who can write
/// `.axiom/attestations.json`, and a key stored beside that file is readable by
/// the same person, so it would prove nothing the digest did not already.
///
/// With no key configured, records are still written and still tamper-evident
/// through `seal`. They are simply anonymous, and say so.
pub fn configured_signing_key() -> Option<String> {
    if let Ok(key) = std::env::var("AXIOM_SIGNING_KEY") {
        if !key.trim().is_empty() {
            return Some(key.trim().to_string());
        }
    }
    if let Ok(path) = std::env::var("AXIOM_SIGNING_KEY_FILE") {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if !contents.trim().is_empty() {
                return Some(contents.trim().to_string());
            }
        }
    }
    None
}

/// Read a required string argument, or say why it is unusable.
///
/// Defaulting a missing argument to "" turned a malformed request into a lookup
/// for the empty string, which used to match every symbol.
fn required_str<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    match args.get(name) {
        None => Err(format!("{name} is required")),
        Some(Value::String(s)) if s.trim().is_empty() => Err(format!("{name} must not be blank")),
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(format!(
            "{name} must be a string, got {}",
            match other {
                Value::Number(_) => "a number",
                Value::Bool(_) => "a boolean",
                Value::Array(_) => "an array",
                Value::Object(_) => "an object",
                Value::Null => "null",
                Value::String(_) => unreachable!(),
            }
        )),
    }
}

/// A check that was performed before a provenance record was issued.
#[derive(Debug, Clone)]
pub struct Verification {
    pub passed: bool,
    /// "sandbox" when axiom ran it, "reported" when an agent says it ran
    /// something elsewhere. Never collapse the two: axiom can vouch for the
    /// first and is only repeating the second.
    pub kind: String,
    pub detail: String,
}

pub struct AxiomMcpServer {
    /// Verifications this server knows about, by task id.
    ///
    /// A sandbox run is one kind. It cannot be the only kind: the sandbox
    /// compiles Rust, so requiring one made provenance unreachable for every
    /// Java, Kotlin, Python, TypeScript and Go change, which is most of what the
    /// indexer reads. An agent that ran a project's own suite has verified
    /// something real, and can say so. What it cannot do is pass that off as
    /// axiom's own work, which is why the kind travels with the record.
    pub verifications: Arc<RwLock<HashMap<String, Verification>>>,
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
    /// Build a server over whichever index is above the working directory.
    pub fn new() -> Result<Self> {
        Self::with_index(find_index_file().as_deref())
    }

    /// Build a server over an explicit index, or over an empty one when given
    /// `None`.
    ///
    /// `new` searches upwards from the working directory, which is right for a
    /// server an agent starts inside a project and wrong for anything that must
    /// not depend on what happens to be above it. A test that constructs a
    /// server through `new` is really testing this machine's directory tree.
    pub fn with_index(index_path: Option<&std::path::Path>) -> Result<Self> {
        let ast_index = match index_path {
            Some(path) => match AstIndex::load_from_disk(path) {
                Ok(idx) => Arc::new(idx),
                Err(_) => Arc::new(AstIndex::new()),
            },
            None => Arc::new(AstIndex::new()),
        };

        let wasi_engine = Arc::new(WasiEngine::new()?);
        // Each server is a distinct replica. Sharing one id across processes
        // makes concurrent agents produce identical Lamport stamps, and a
        // last-writer-wins rule cannot order a tie it cannot see.
        let tree_crdt = Arc::new(TreeCrdt::new(std::process::id()));

        Ok(Self {
            verifications: Arc::new(RwLock::new(HashMap::new())),
            ast_index,
            wasi_engine,
            tree_crdt,
        })
    }

/// Populate the workspace with the demo symbols the walkthrough uses.
    ///
    /// This used to run inside `new` whenever the index was empty, which made a
    /// workspace nobody had scanned answer confidently about
    /// `auth::service::validate_token` and hand back a blast radius for it. That
    /// symbol is in no real codebase, and an agent following the usage guide,
    /// which uses exactly that name, had no way to tell it was talking to a
    /// fixture. Seeding is now something `axiom demo` asks for.
    /// The empty-index guard this used to carry belonged to the version that ran
    /// automatically. A caller that asks for the demo data means it whatever the
    /// workspace already holds, and keeping the guard made the call quietly do
    /// nothing wherever an index existed, so `axiom demo` then queried a symbol
    /// it had not inserted and reported zeros.
    pub fn seed_demo_workspace(&self) {
        {
            self.ast_index.index_node(
                "auth::service::validate_token",
                "function",
                "pub fn validate_token(t: &str) -> bool { t.len() > 10 }",
                vec!["jwt::verifier".into()],
            );
            self.ast_index.index_node(
                "test_auth_validation",
                "test",
                "#[test] fn test_auth_validation() { assert!(validate_token(\"valid_token_secret\")); }",
                vec!["auth::service::validate_token".into()],
            );

            self.tree_crdt.insert_node(
                "root",
                "node_auth_val",
                "auth::service::validate_token",
                "function",
                "pub fn validate_token(t: &str) -> bool { t.len() > 10 }",
            );
        }

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
                            "description": "Look up one indexed symbol. A shorter name resolves when it identifies exactly one symbol; a name matching several returns the candidates instead of choosing.",
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
                            "description": "The tests that reach a symbol, so a change can be checked without running everything. An empty result means none were found in the index, which is not the same as nothing being affected.",
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
                            "description": "Compile and run a Rust snippet in process and report what happened. Takes a few hundred milliseconds, since it invokes rustc. A symbol from a language it cannot compile is refused rather than guessed at.",
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
                            "description": "Record that a change to a symbol was checked, tying the prompt, the symbol and the check together. Only issued against a check that happened and passed. Signed when a signing key is configured.",
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
                            "description": "Apply a Tree-CRDT mutation to one symbol and persist it. Only that symbol is written, so a concurrent agent sharing the workspace does not lose its work.",
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
                            "name": "axiom_record_verification",
                            "description": "Record the outcome of a check run outside the sandbox, such as a project's own test suite, so a provenance record can rest on it. Axiom stores what you report and marks it as reported rather than as its own work.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "task_id": { "type": "string", "description": "Identifier to attest against later" },
                                    "passed": { "type": "boolean", "description": "Whether the check succeeded" },
                                    "command": { "type": "string", "description": "What was run, recorded verbatim in the provenance record" }
                                },
                                "required": ["task_id", "passed", "command"]
                            }
                        },
                        {
                            "name": "axiom_search_regex",
                            "description": "Search repository source text, falling back to symbol names. Literal by default; set mode=regex for a pattern.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "query": { "type": "string", "description": "Text to find, or a regular expression when mode is regex" },
                                    "mode": {
                                        "type": "string",
                                        "enum": ["literal", "regex", "auto"],
                                        "default": "literal",
                                        "description": "How to read the query. literal (default) treats it as plain text, so characters like . ( ) < > match themselves. regex compiles it as a pattern. auto uses regex only when the query contains a construct that is meaningless as literal text. The mode actually applied comes back in the response."
                                    },
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
                let symbol = match required_str(&args, "symbol_path") {
                    Ok(s) => s,
                    Err(e) => return Ok(json!({ "error": e })),
                };

                if let Some(node) = self.ast_index.get_symbol(symbol) {
                    return Ok(json!(node));
                }

                // An ambiguous name is not a miss. Saying so beats picking one of
                // the candidates and presenting it as the answer.
                let candidates = self.ast_index.candidates_for(symbol);
                if candidates.len() > 1 {
                    return Ok(json!({
                        "error": format!("{:?} matches {} symbols; name one of them", symbol, candidates.len()),
                        "candidates": candidates.iter().take(10).collect::<Vec<_>>()
                    }));
                }

                Ok(json!({
                    "error": format!("Symbol '{}' not found in AST index. Use 'axiom scan' to index your workspace first.", symbol),
                    "total_symbols_in_index": self.ast_index.total_symbols_count()
                }))
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

                // The sandbox compiles Rust. The indexer does not: it reads Java,
                // Kotlin, Python, TypeScript and Go too, so a symbol from any of
                // those would be handed to rustc and come back with a syntax
                // error that blames the caller instead of naming the real limit.
                if let Some(lang) = self.ast_index.language_of_symbol(symbol) {
                    if lang != "rs" {
                        return Ok(json!({
                            "task_id": "eval_unsupported_language",
                            "status": "EVALUATOR_UNAVAILABLE",
                            "engine": "tier1_wasi_cranelift",
                            "passed_checks_count": 0,
                            "failed_checks": [{
                                "symbol": symbol,
                                "error_type": "UnsupportedLanguage",
                                "expected": "a Rust snippet",
                                "actual": format!("{:?} is defined in a .{} file", symbol, lang),
                                "hint": "The sandbox compiles Rust only. Run this symbol's own test suite instead; axiom_get_blast_radius will name the tests to run."
                            }]
                        }));
                    }
                }

                let report = self.wasi_engine.execute_eval(symbol, snippet).await?;

                // Record the outcome so an attestation can be checked against a
                // run that genuinely happened, rather than against a task id the
                // caller made up.
                let passed = matches!(report.status, CtopStatus::Passed);
                self.verifications.write().unwrap().insert(
                    report.task_id.clone(),
                    Verification {
                        passed,
                        kind: "sandbox".to_string(),
                        detail: format!("axiom sandbox, engine {}", report.engine),
                    },
                );

                Ok(json!(report))
            }

            "axiom_attest_commit" => {
                let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let symbol = args.get("symbol_path").and_then(|v| v.as_str()).unwrap_or("");
                let task_id = match args.get("ctop_task_id").and_then(|v| v.as_str()) {
                    Some(t) if !t.is_empty() => t,
                    _ => {
                        return Ok(json!({
                            "error": "ctop_task_id is required: an attestation must name the sandbox run it rests on"
                        }))
                    }
                };

                // The seal claims the change was verified in the sandbox. Issuing
                // one for a run this server never performed, or for one that
                // failed, would make that claim false, so both are refused.
                let verification = match self.verifications.read().unwrap().get(task_id) {
                    None => {
                        return Ok(json!({
                            "error": format!(
                                "no verification recorded for task {task_id:?}. Either run axiom_eval_patch and attest against the task_id it returns, or report an external check with axiom_record_verification"
                            )
                        }))
                    }
                    Some(v) if !v.passed => {
                        return Ok(json!({
                            "error": format!(
                                "verification {task_id:?} did not pass ({}); a record may only be issued for a check that succeeded",
                                v.detail
                            )
                        }))
                    }
                    Some(v) => v.clone(),
                };

                let root = self.tree_crdt.compute_tree_merkle_root();

                // Link, seal, sign and append under one lock. The chain link has
                // to be known before the record is sealed, and the seal before it
                // is signed, so reading the tail and writing the record cannot be
                // two separate steps without a second agent slipping between them.
                let ledger_path = attestation_ledger_path();
                let _ledger_lock = match axiom_ast::IndexLock::acquire(&ledger_path) {
                    Ok(l) => l,
                    Err(e) => return Ok(json!({ "error": format!("could not lock the ledger: {e}") })),
                };
                let mut existing = load_attestations_from(&ledger_path).unwrap_or_default();
                let previous_seal = existing.last().map(|a| a.seal.clone()).unwrap_or_default();

                let attestation = ProvenanceAttestation::generate(
                    "merkle_root_prev_77a1",
                    &format!("merkle_root_{}", &root[..8]),
                    "agent_axiom_v1",
                    prompt,
                    symbol,
                    task_id,
                    &verification.kind,
                    &verification.detail,
                    &previous_seal,
                );

                // Sign when a key is configured. An unsigned record is still
                // worth writing; it just cannot say who issued it.
                let mut attestation = attestation;
                if let Some(key) = configured_signing_key() {
                    if let Err(e) = attestation.sign_with(symbol, prompt, &key) {
                        return Ok(json!({
                            "error": format!("could not sign the record: {e}")
                        }));
                    }
                }

                // Persist it, or verification later has nothing to look up.
                existing.push(attestation.clone());
                let encoded = match serde_json::to_string_pretty(&existing) {
                    Ok(j) => j,
                    Err(e) => return Ok(json!({ "error": format!("could not encode the ledger: {e}") })),
                };
                if let Some(parent) = ledger_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&ledger_path, encoded) {
                    return Ok(json!({
                        "error": format!("could not record the attestation: {e}")
                    }));
                }

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
                // Persist just this symbol. Writing the whole in-memory index
                // here would also write back every other symbol as this process
                // last saw it, discarding what another agent recorded meanwhile.
                if let Err(e) = self
                    .ast_index
                    .persist_symbol(std::path::Path::new(".axiom/index.json"), symbol)
                {
                    eprintln!("Warning: Failed to save .axiom/index.json: {}", e);
                }

                Ok(json!({
                    "status": "APPLIED",
                    "crdt_op": op,
                    "new_merkle_root": root,
                    "active_ast_nodes": self.tree_crdt.active_nodes_count()
                }))
            }

            "axiom_record_verification" => {
                let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
                let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let passed = match args.get("passed").and_then(|v| v.as_bool()) {
                    Some(p) => p,
                    None => {
                        return Ok(json!({
                            "error": "passed is required and must be true or false: a verification with no outcome is not one"
                        }))
                    }
                };
                if task_id.is_empty() || command.is_empty() {
                    return Ok(json!({
                        "error": "task_id and command are both required: a record that cannot say what was run is worth nothing"
                    }));
                }

                self.verifications.write().unwrap().insert(
                    task_id.to_string(),
                    Verification {
                        passed,
                        kind: "reported".to_string(),
                        detail: command.to_string(),
                    },
                );

                Ok(json!({
                    "task_id": task_id,
                    "passed": passed,
                    "recorded_as": "reported",
                    "note": "Axiom did not run this. The provenance record will say the outcome was reported by the agent, not observed by axiom."
                }))
            }

            "axiom_search_regex" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let max = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
                let requested = args.get("mode").and_then(|v| v.as_str()).unwrap_or("literal");

                let mode = match SearchMode::parse(requested) {
                    Ok(m) => m,
                    Err(e) => return Ok(json!({ "error": e, "query": query })),
                };

                // A pattern that does not compile is reported as such. Retrying it
                // as a literal would answer a question the caller did not ask.
                match self.ast_index.search(query, mode, max) {
                    Ok((applied, matches)) => Ok(json!({
                        "query": query,
                        "mode_requested": requested,
                        "mode_applied": applied.as_str(),
                        "matches_count": matches.len(),
                        "matches": matches
                    })),
                    Err(e) => Ok(json!({ "error": e, "query": query, "mode_requested": requested })),
                }
            }

            _ => anyhow::bail!("Unknown tool: {}", tool_name),
        }
    }
}
