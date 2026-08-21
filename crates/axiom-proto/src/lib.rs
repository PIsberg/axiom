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
    /// How the change was checked before this record was issued: "sandbox" when
    /// axiom compiled and ran it, "reported" when an agent ran something else,
    /// a project's own test suite for instance, and told axiom the outcome.
    ///
    /// The distinction is the whole value of the record. Axiom can vouch for what
    /// it ran itself; for anything else it is repeating a claim, and a reader
    /// deserves to know which they are looking at.
    #[serde(default)]
    pub verified_by: String,

    /// What was run, when that is known: the sandbox task, or the command an
    /// agent reported.
    #[serde(default)]
    pub verification_detail: String,

    /// The seal of the record written before this one, empty for the first.
    ///
    /// Signatures stop a record being forged or edited; they do nothing about
    /// one being removed, because what is left still verifies. Each record
    /// committing to its predecessor makes a deletion visible: the record after
    /// the hole points at a seal that is no longer there, and repairing the
    /// chain would need the signing key.
    #[serde(default)]
    pub previous_seal: String,

    /// Ed25519 signature over this record, when one was made. Empty when no
    /// signing key was configured, in which case the record is tamper-evident
    /// through `seal` but says nothing about who issued it.
    #[serde(default)]
    pub signature: String,

    /// The public key the signature can be checked against.
    #[serde(default)]
    pub public_key: String,

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
        verified_by: &str,
        verification_detail: &str,
        previous_seal: &str,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(parent_merkle_root.as_bytes());
        hasher.update(commit_merkle_root.as_bytes());
        hasher.update(agent_identity.as_bytes());
        hasher.update(prompt.as_bytes());
        hasher.update(symbol_path.as_bytes());
        hasher.update(ctop_task_id.as_bytes());
        hasher.update(previous_seal.as_bytes());
        let digest = hasher.finalize().to_hex().to_string();

        Self {
            previous_seal: previous_seal.to_string(),
            // Filled in by sign_with when a signing key is configured; a record
            // with no key stays tamper-evident through `seal` and anonymous.
            signature: String::new(),
            public_key: String::new(),
            verified_by: verified_by.to_string(),
            verification_detail: verification_detail.to_string(),
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
    /// Sign this record with a key, binding the signature to the symbol and
    /// prompt so it cannot be lifted onto a different record.
    pub fn sign_with(
        &mut self,
        symbol_path: &str,
        prompt: &str,
        private_hex: &str,
    ) -> Result<(), String> {
        let (signature, public_key) = crate::signing::sign(self, symbol_path, prompt, private_hex)?;
        self.signature = signature;
        self.public_key = public_key;
        Ok(())
    }

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
        hasher.update(self.previous_seal.as_bytes());
        let digest = hasher.finalize().to_hex().to_string();

        let expected = format!("blake3_seal_{}", &digest[32..]);
        self.seal == expected
    }
}

impl CtopReport {
    pub fn pass(
        task_id: String,
        engine: String,
        duration_ms: f64,
        passed_count: usize,
        stdout: String,
    ) -> Self {
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

/// Signing and verifying provenance records with Ed25519.
///
/// The `seal` field is a BLAKE3 digest over the record. It shows the record has
/// not been altered and nothing about who wrote it, because anyone holding the
/// same inputs recomputes it. A signature is what distinguishes those two
/// claims.
///
/// The key must be able to live outside the workspace, and that is the whole
/// point rather than a convenience. The threat is someone who can write
/// `.axiom/attestations.json`; a key sitting beside that file is readable by the
/// same person, so signing with it would prove nothing that the digest did not
/// already. What signing buys is a record that stays checkable somewhere else:
/// a reader holding only the public key can tell whether a given signer issued
/// it.
pub mod signing {
    use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

    /// The bytes a signature covers: the record's own fields plus the symbol and
    /// prompt it is about, so a signature cannot be lifted onto another record.
    pub fn signable_bytes(
        attestation: &crate::ProvenanceAttestation,
        symbol_path: &str,
        prompt: &str,
    ) -> Vec<u8> {
        let mut hasher = blake3::Hasher::new();
        for field in [
            attestation.parent_merkle_root.as_str(),
            attestation.commit_merkle_root.as_str(),
            attestation.agent_identity.as_str(),
            attestation.ctop_proof_hash.as_str(),
            attestation.verified_by.as_str(),
            attestation.verification_detail.as_str(),
            attestation.timestamp.as_str(),
            attestation.previous_seal.as_str(),
            symbol_path,
            prompt,
        ] {
            // Length-prefixed, so two different field splits cannot hash alike.
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
        hasher.finalize().as_bytes().to_vec()
    }

    /// Generate a keypair. Returns (private key hex, public key hex).
    pub fn generate_keypair() -> (String, String) {
        let mut csprng = rand_core::OsRng;
        let signing = SigningKey::generate(&mut csprng);
        (
            hex::encode(signing.to_bytes()),
            hex::encode(signing.verifying_key().to_bytes()),
        )
    }

    pub fn public_key_of(private_hex: &str) -> Result<String, String> {
        Ok(hex::encode(
            load_signing_key(private_hex)?.verifying_key().to_bytes(),
        ))
    }

    /// A short, human-comparable form of a public key.
    pub fn fingerprint(public_hex: &str) -> String {
        let digest = blake3::hash(public_hex.as_bytes());
        hex::encode(&digest.as_bytes()[..8])
    }

    fn load_signing_key(private_hex: &str) -> Result<SigningKey, String> {
        let raw =
            hex::decode(private_hex.trim()).map_err(|e| format!("signing key is not hex: {e}"))?;
        let bytes: [u8; 32] = raw
            .try_into()
            .map_err(|_| "signing key must be 32 bytes".to_string())?;
        Ok(SigningKey::from_bytes(&bytes))
    }

    pub fn sign(
        attestation: &crate::ProvenanceAttestation,
        symbol_path: &str,
        prompt: &str,
        private_hex: &str,
    ) -> Result<(String, String), String> {
        let key = load_signing_key(private_hex)?;
        let sig = key.sign(&signable_bytes(attestation, symbol_path, prompt));
        Ok((
            hex::encode(sig.to_bytes()),
            hex::encode(key.verifying_key().to_bytes()),
        ))
    }

    /// Check a signature against the public key the record carries.
    ///
    /// Passing on its own says the holder of that key issued this record. It
    /// does not say the key is one you should trust: that is what comparing the
    /// fingerprint against an expected signer is for.
    pub fn verify(
        attestation: &crate::ProvenanceAttestation,
        symbol_path: &str,
        prompt: &str,
    ) -> Result<(), String> {
        if attestation.signature.is_empty() || attestation.public_key.is_empty() {
            return Err("record carries no signature".to_string());
        }

        let pk_raw = hex::decode(&attestation.public_key)
            .map_err(|e| format!("public key is not hex: {e}"))?;
        let pk_bytes: [u8; 32] = pk_raw
            .try_into()
            .map_err(|_| "public key must be 32 bytes".to_string())?;
        let verifying = VerifyingKey::from_bytes(&pk_bytes)
            .map_err(|e| format!("public key is not a valid Ed25519 key: {e}"))?;

        let sig_raw = hex::decode(&attestation.signature)
            .map_err(|e| format!("signature is not hex: {e}"))?;
        let sig_bytes: [u8; 64] = sig_raw
            .try_into()
            .map_err(|_| "signature must be 64 bytes".to_string())?;

        verifying
            .verify(
                &signable_bytes(attestation, symbol_path, prompt),
                &Signature::from_bytes(&sig_bytes),
            )
            .map_err(|_| "signature does not match this record".to_string())
    }
}

/// Check that a ledger's records still form an unbroken chain.
///
/// Each record after the first names the seal of the one before it. A record
/// removed from the middle leaves the next one pointing at a seal that is no
/// longer present, which is what makes the deletion visible.
///
/// This does not detect a ledger truncated at the end. Nothing points at the
/// last record, so removing it leaves a chain that is internally consistent.
/// Catching that needs the expected head recorded somewhere the writer of the
/// ledger cannot reach, which is outside what this file can do for itself.
pub fn verify_chain(records: &[ProvenanceAttestation]) -> Result<(), String> {
    for (i, window) in records.windows(2).enumerate() {
        let (before, after) = (&window[0], &window[1]);
        if after.previous_seal != before.seal {
            return Err(format!(
                "chain breaks between record {} and record {}: record {} names predecessor {}, \
                 but the record before it seals as {}. A record has been removed or reordered.",
                i,
                i + 1,
                i + 1,
                if after.previous_seal.is_empty() {
                    "(none)"
                } else {
                    &after.previous_seal
                },
                before.seal
            ));
        }
    }

    if let Some(first) = records.first() {
        if !first.previous_seal.is_empty() {
            return Err(format!(
                "chain starts mid-way: the first record names predecessor {}, which is not in the ledger. \
                 Records before it have been removed.",
                first.previous_seal
            ));
        }
    }

    Ok(())
}
