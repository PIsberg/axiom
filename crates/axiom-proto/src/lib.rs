use serde::{Deserialize, Serialize};

/// Common Test Output Protocol (CTOP) Status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CtopStatus {
    Passed,
    Failed,
    Timeout,
    CompilationError,
    /// Nothing was run, so nothing is known. The toolchain was missing, the
    /// language has no evaluator, or the work directory could not be written.
    ///
    /// Distinct from `CompilationError`, which says the code was read and
    /// rejected. Collapsing the two tells an agent its snippet is wrong when
    /// the truth is that nobody looked at it.
    EvaluatorUnavailable,
}

/// Structured compiler diagnostic span for compiler-guided repair
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DiagnosticSpan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    pub message: String,
    #[serde(default = "default_diagnostic_severity")]
    pub severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_replacement: Option<String>,
}

fn default_diagnostic_severity() -> String {
    "error".to_string()
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<DiagnosticSpan>,
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
    /// What `passed_checks_count` counts. For the native tiers it is the
    /// number of assertion tokens found in the snippet's text, because no
    /// toolchain reports how many assertions ran; a snippet with the word
    /// `assert` in a comment counts it. A count that does not say what it
    /// counts reads as a measurement, so this travels with it.
    #[serde(default)]
    pub passed_checks_basis: String,
    pub stdout: String,
    pub stderr: String,
    pub memory_allocated_bytes: Option<u64>,
    /// Whether the compile step was served by the content-addressed artifact
    /// cache. `Some("hit")`: a previously compiled artifact for byte-identical
    /// source under a byte-identical toolchain was reused; the verdict still
    /// comes from running it, never from a stored verdict. `Some("miss")`:
    /// compiled fresh and stored. `None`: no compile step exists for the
    /// language, or the cache is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compile_cache: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<DiagnosticSpan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_fixes: Vec<VerifiedFixCandidate>,
}

/// The basis for a count of assertion tokens, see `CtopReport::passed_checks_basis`.
pub const ASSERTION_TOKENS_BASIS: &str =
    "assertion tokens found in the snippet text; not assertions observed to execute";

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
    /// A real Merkle root of the state before, not a constant. `axiom` fills it
    /// with the CRDT tree root. It was the literal `merkle_root_prev_77a1` on
    /// every record until that was found to distinguish nothing.
    pub parent_merkle_root: String,
    /// A real Merkle root of the attested code: `axiom` fills it with the AST
    /// index root, a digest over every indexed symbol and its body hash, so it
    /// moves when the code the record is about moves. It was a truncated slice
    /// of the CRDT root before.
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

/// What a provenance record is issued about.
///
/// These arrived as nine positional arguments, which is enough for a caller to
/// transpose two strings and never find out. Naming them at the call site makes
/// that mistake visible while writing it.
pub struct NewAttestation<'a> {
    pub parent_merkle_root: &'a str,
    pub commit_merkle_root: &'a str,
    pub agent_identity: &'a str,
    pub prompt: &'a str,
    pub symbol_path: &'a str,
    pub ctop_task_id: &'a str,
    /// "sandbox" when axiom ran the check, "reported" when it was told.
    pub verified_by: &'a str,
    pub verification_detail: &'a str,
    /// Seal of the record before this one, empty for the first.
    pub previous_seal: &'a str,
}

/// Re-derive the seal from a record's stored fields and the prompt it is
/// claimed for.
///
/// Every field is here, and every field is length-prefixed. The old seal
/// covered the roots, the identity, the prompt, the symbol, the task id and
/// the previous seal, so editing `verified_by`, `verification_detail` or
/// `timestamp` left a record that still verified. Measured: a `reported`
/// record was re-labelled `sandbox` in the ledger and `axiom verify` still
/// said VALID. Those three are inside it now, and the prefixing stops two
/// different field splits, `a` + `bc` against `ab` + `c`, from hashing alike.
///
/// `generate` and `verify` both go through here so they cannot drift.
#[allow(clippy::too_many_arguments)]
fn seal_over(
    parent_merkle_root: &str,
    commit_merkle_root: &str,
    agent_identity: &str,
    symbol_path: &str,
    ctop_proof_hash: &str,
    verified_by: &str,
    verification_detail: &str,
    timestamp: &str,
    previous_seal: &str,
    prompt: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    for field in [
        parent_merkle_root,
        commit_merkle_root,
        agent_identity,
        symbol_path,
        ctop_proof_hash,
        verified_by,
        verification_detail,
        timestamp,
        previous_seal,
        prompt,
    ] {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    format!("blake3_seal_{}", hasher.finalize().to_hex())
}

impl ProvenanceAttestation {
    pub fn generate(details: NewAttestation<'_>) -> Self {
        let NewAttestation {
            parent_merkle_root,
            commit_merkle_root,
            agent_identity,
            prompt,
            symbol_path,
            ctop_task_id,
            verified_by,
            verification_detail,
            previous_seal,
        } = details;

        // The timestamp is inside the seal, so it is chosen before the seal is
        // computed rather than after the struct is built.
        let timestamp = chrono::Utc::now().to_rfc3339();

        // A real digest of the prompt alone, not a slice of the seal. Two
        // records for one prompt share it; two for different prompts do not,
        // which is what lets a reader group records by prompt without holding
        // the prompt text.
        let prompt_digest = format!("blake3:{}", &blake3::hash(prompt.as_bytes()).to_hex()[..32]);

        // A digest of what was checked and how: the kind, the detail, and the
        // task id it rests on. It used to be a slice of the same combined
        // digest as everything else, so it named no trace in particular.
        let mut trace = blake3::Hasher::new();
        for part in [verified_by, verification_detail, ctop_task_id] {
            trace.update(&(part.len() as u64).to_le_bytes());
            trace.update(part.as_bytes());
        }
        let sandbox_trace_hash = format!("trace:{}", &trace.finalize().to_hex()[..32]);

        let seal = seal_over(
            parent_merkle_root,
            commit_merkle_root,
            agent_identity,
            symbol_path,
            ctop_task_id,
            verified_by,
            verification_detail,
            &timestamp,
            previous_seal,
            prompt,
        );

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
            prompt_digest,
            sandbox_trace_hash,
            ctop_proof_hash: ctop_task_id.to_string(),
            timestamp,
            seal,
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

        let expected = seal_over(
            &self.parent_merkle_root,
            &self.commit_merkle_root,
            &self.agent_identity,
            expected_symbol,
            &self.ctop_proof_hash,
            &self.verified_by,
            &self.verification_detail,
            &self.timestamp,
            &self.previous_seal,
            prompt,
        );
        self.seal == expected
    }

    /// Convert this attestation into a standardized SLSA v1.0 / in-toto Provenance statement
    pub fn to_slsa_statement(&self) -> serde_json::Value {
        serde_json::json!({
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [
                {
                    "name": self.symbol_path,
                    "digest": {
                        "merkleRoot": self.commit_merkle_root,
                        "seal": self.seal
                    }
                }
            ],
            "predicateType": "https://slsa.dev/provenance/v1",
            "predicate": {
                "buildDefinition": {
                    "buildType": "https://axiom.dev/provenance/v1",
                    "externalParameters": {
                        "agentIdentity": self.agent_identity,
                        "promptDigest": self.prompt_digest,
                        "symbolPath": self.symbol_path
                    },
                    "internalParameters": {
                        "parentMerkleRoot": self.parent_merkle_root,
                        "ctopProofHash": self.ctop_proof_hash,
                        "previousSeal": self.previous_seal,
                        "signature": self.signature,
                        "publicKey": self.public_key
                    }
                },
                "runDetails": {
                    "builder": {
                        "id": "https://axiom.dev/verifier/v1",
                        "version": {
                            "axiom": env!("CARGO_PKG_VERSION")
                        }
                    },
                    "metadata": {
                        "invocationId": self.seal,
                        "startedOn": self.timestamp,
                        "finishedOn": self.timestamp
                    },
                    "byproducts": [
                        {
                            "name": "verification",
                            "verifiedBy": self.verified_by,
                            "verificationDetail": self.verification_detail
                        }
                    ]
                }
            }
        })
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
            passed_checks_basis: ASSERTION_TOKENS_BASIS.to_string(),
            stdout,
            stderr: String::new(),
            memory_allocated_bytes: None,
            compile_cache: None,
            diagnostics: Vec::new(),
            suggested_fixes: Vec::new(),
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
            passed_checks_basis: String::new(),
            stdout,
            stderr,
            memory_allocated_bytes: None,
            compile_cache: None,
            diagnostics: Vec::new(),
            suggested_fixes: Vec::new(),
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
    ///
    /// The private key is the 32-byte seed, which is what `to_bytes` returns and
    /// what `load_signing_key` reads back, so a key written by an earlier version
    /// still loads. The seed is filled here rather than through
    /// `SigningKey::generate` because ed25519-dalek 3 wants an infallible
    /// `CryptoRng` and the OS generator is fallible; the two are the same
    /// operation, since `generate` fills 32 bytes and calls `from_bytes` on them.
    ///
    /// A failure to read entropy panics, which is what the previous rand_core
    /// OsRng did on the same condition. It is not a case a caller can do
    /// anything useful with: there is no weaker key worth returning.
    pub fn generate_keypair() -> (String, String) {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed)
            .expect("the operating system refused to supply entropy for a signing key");
        let signing = SigningKey::from_bytes(&seed);
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

/// Verified mutation fix candidate from Merkle ledger patch memory
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VerifiedFixCandidate {
    pub fingerprint: String,
    pub symbol_path: String,
    pub error_signature: String,
    pub patch_content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_ast_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit_ast_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub attestation_seal: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub verified_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub timestamp: String,
}

/// Compute a deterministic diagnostic fingerprint for AST patch memory
pub fn compute_diagnostic_fingerprint(symbol_ast_hash: &str, error_sig: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"axiom_fix_v1:");
    hasher.update(symbol_ast_hash.trim().as_bytes());
    hasher.update(b":");
    hasher.update(error_sig.trim().as_bytes());
    hasher.finalize().to_hex().to_string()
}
