use serde::{Deserialize, Serialize};

/// Common Test Output Protocol (CTOP) Status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CtopStatus {
    Passed,
    Failed,
    Timeout,
    CompilationError,
}

/// A specific failure check in CTOP
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedCheck {
    pub symbol: String,
    pub error_type: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub stack_trace_ast_nodes: Vec<String>,
    pub hint: Option<String>,
}

/// Standardized Common Test Output Protocol (CTOP) execution response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtopReport {
    pub task_id: String,
    pub engine: String,
    pub status: CtopStatus,
    pub execution_duration_ms: f64,
    pub blast_radius_nodes: usize,
    pub failed_checks: Vec<FailedCheck>,
    pub passed_checks_count: usize,
    pub stdout: String,
    pub stderr: String,
    pub memory_allocated_bytes: Option<u64>,
}

/// AST Node metadata in the Merkle Graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstNode {
    pub id: String,
    pub symbol_path: String,
    pub kind: String,
    pub hash: String,
    pub source_range: (usize, usize),
    pub docstring: Option<String>,
    pub signature: Option<String>,
    pub dependencies: Vec<String>,
}

/// Evaluation request payload for instant sandboxes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRequest {
    pub workspace_id: String,
    pub symbol_path: Option<String>,
    pub ast_diff: Option<String>,
    pub wasm_bytes: Option<Vec<u8>>,
    pub code_snippet: Option<String>,
    pub test_target: Option<String>,
}

/// SLSA Level 4+ Cryptographic Attestation Proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceAttestation {
    /// The symbol this seal was issued for. Without it a stored attestation
    /// cannot be found again, and verification degenerates into re-deriving a
    /// seal from whatever arguments it was handed.
    #[serde(default)]
    pub symbol_path: String,
    pub parent_merkle_root: String,
    pub commit_merkle_root: String,
    pub agent_identity: String,
    pub prompt_digest: String,
    pub sandbox_trace_hash: String,
    pub ctop_proof_hash: String,
    pub timestamp: String,
    /// Integrity tag over this attestation's own fields plus the symbol and
    /// prompt it was issued for. It is a BLAKE3 digest, not a signature: there
    /// is no private key, so anyone holding the same inputs can recompute it.
    /// That detects an altered record; it does not establish who wrote one.
    pub seal: String,
}

impl ProvenanceAttestation {
    pub fn generate(
        parent_merkle_root: &str,
        commit_merkle_root: &str,
        agent_identity: &str,
        prompt: &str,
        symbol_path: &str,
        ctop_task_id: &str,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(parent_merkle_root.as_bytes());
        hasher.update(commit_merkle_root.as_bytes());
        hasher.update(agent_identity.as_bytes());
        hasher.update(prompt.as_bytes());
        hasher.update(symbol_path.as_bytes());
        hasher.update(ctop_task_id.as_bytes());
        let digest = hasher.finalize().to_hex().to_string();

        Self {
            symbol_path: symbol_path.to_string(),
            parent_merkle_root: parent_merkle_root.to_string(),
            commit_merkle_root: commit_merkle_root.to_string(),
            agent_identity: agent_identity.to_string(),
            prompt_digest: format!("blake3:{}", &digest[..16]),
            sandbox_trace_hash: format!("trace:{}", &digest[16..32]),
            ctop_proof_hash: ctop_task_id.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            seal: format!("blake3_seal_{}", &digest[32..]),
        }
    }

    /// Re-derive the seal from this attestation's own stored fields plus the
    /// symbol and prompt being claimed, and compare. A caller that supplies a
    /// different prompt, or asks about a different symbol, gets false.
    pub fn verify(&self, expected_symbol: &str, prompt: &str) -> bool {
        if self.symbol_path != expected_symbol {
            return false;
        }

        let mut hasher = blake3::Hasher::new();
        hasher.update(self.parent_merkle_root.as_bytes());
        hasher.update(self.commit_merkle_root.as_bytes());
        hasher.update(self.agent_identity.as_bytes());
        hasher.update(prompt.as_bytes());
        hasher.update(expected_symbol.as_bytes());
        hasher.update(self.ctop_proof_hash.as_bytes());
        let digest = hasher.finalize().to_hex().to_string();

        let expected = format!("blake3_seal_{}", &digest[32..]);
        self.seal == expected
    }
}

impl CtopReport {
    pub fn pass(task_id: String, engine: String, duration_ms: f64, passed_count: usize, stdout: String) -> Self {
        Self {
            task_id,
            engine,
            status: CtopStatus::Passed,
            execution_duration_ms: duration_ms,
            blast_radius_nodes: 1,
            failed_checks: Vec::new(),
            passed_checks_count: passed_count,
            stdout,
            stderr: String::new(),
            memory_allocated_bytes: None,
        }
    }

    pub fn fail(
        task_id: String,
        engine: String,
        duration_ms: f64,
        failed_checks: Vec<FailedCheck>,
        stdout: String,
        stderr: String,
    ) -> Self {
        Self {
            task_id,
            engine,
            status: CtopStatus::Failed,
            execution_duration_ms: duration_ms,
            blast_radius_nodes: failed_checks.len().max(1),
            failed_checks,
            passed_checks_count: 0,
            stdout,
            stderr,
            memory_allocated_bytes: None,
        }
    }
}
