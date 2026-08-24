use anyhow::Result;
use axiom_ast::{AstIndex, SearchMode};
pub use axiom_crdt;
use axiom_crdt::TreeCrdt;
use axiom_proto::{CtopStatus, NewAttestation, ProvenanceAttestation};
use axiom_vmm::{SandboxEngine, WasiEngine};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
/// What a record says when nobody named themselves.
///
/// Not a plausible-looking agent name. The field used to be the constant
/// `agent_axiom_v1`, which read as the claimed author sitting next to
/// `public_key` when it was the same on every record ever issued. An absent
/// answer has to look absent.
pub const UNATTRIBUTED: &str = "unattributed";

/// Longest identity accepted. The value is printed by `axiom verify`, stored in
/// the ledger and hashed into the seal, so it is bounded where it enters rather
/// than truncated at each of those.
const MAX_AGENT_IDENTITY: usize = 128;

/// What an agent is meant to do with these tools, and in what order. Returned
/// from `initialize`, which is where MCP expects a server to say so.
const INSTRUCTIONS: &str = "\
Axiom indexes a codebase into a symbol graph and answers queries against it. The \
index is a snapshot from `axiom scan`; it does not track edits made since, so a \
result reflects the code as last scanned.

A change to one symbol is checked like this:
1. axiom_query_symbol to read a symbol, its signature and its dependencies. A \
short name that matches several symbols comes back as candidates, not a guess.
2. axiom_get_blast_radius to list the tests that reach the symbol, so only those \
need running. An empty list means none were found in the index, not that nothing \
is affected.
3. axiom_eval_patch to compile and run a snippet in the language of the symbol's \
file. It never returns PASSED for something that did not run, and the snippet \
runs with a confined environment.
4. Either attest against that run, or run the project's own tests and report the \
outcome with axiom_record_verification, then axiom_attest_commit. A record is \
only issued for a check that happened and passed, and says whether axiom ran it \
or an agent reported it.

axiom_apply_mutation records a Tree-CRDT change, and axiom_search_regex searches \
source text.";

/// The identity a caller asked to be recorded as, or an error naming the field.
///
/// Self-declared and unverified: axiom stores what it is told. That is honest
/// only while the value cannot claim more than it is. `axiom verify` prints the
/// record as a column of labelled lines, so an identity carrying a newline could
/// add lines of its own and show `Checked by: sandbox` above a record whose
/// `verified_by` says `reported`. Control characters are refused for that
/// reason, not for tidiness.
///
/// A present-but-not-a-string value is refused rather than read as absent:
/// silently substituting the default is the failure #11 was about, in a
/// different place.
fn agent_identity_of(args: &Value) -> Result<String, String> {
    let raw = match args.get("agent_identity") {
        None | Some(Value::Null) => return Ok(UNATTRIBUTED.to_string()),
        Some(Value::String(s)) => s,
        Some(other) => {
            return Err(format!(
                "agent_identity must be a string, got {other}. Omit it to record the change as {UNATTRIBUTED}."
            ));
        }
    };

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(UNATTRIBUTED.to_string());
    }
    if let Some(bad) = trimmed.chars().find(|c| c.is_control()) {
        return Err(format!(
            "agent_identity must be printable single-line text; it is shown as one line by `axiom verify`, and {bad:?} would let a record add lines of its own"
        ));
    }
    if trimmed.chars().count() > MAX_AGENT_IDENTITY {
        return Err(format!(
            "agent_identity is limited to {MAX_AGENT_IDENTITY} characters, got {}",
            trimmed.chars().count()
        ));
    }
    Ok(trimmed.to_string())
}

/// The `.axiom` directory for the workspace the working directory sits in.
///
/// Walks up for an existing `.axiom`, so `axiom verify` run from a subdirectory
/// reads the same ledger the server, which discovers its index the same way,
/// writes. Falls back to `<cwd>/.axiom` when none is found, which is where a
/// first `axiom scan` will create one.
pub fn find_axiom_dir() -> PathBuf {
    if let Ok(mut curr) = std::env::current_dir() {
        loop {
            let candidate = curr.join(".axiom");
            if candidate.is_dir() {
                return candidate;
            }
            if !curr.pop() {
                break;
            }
        }
    }
    PathBuf::from(".axiom")
}

pub fn attestation_ledger_path() -> PathBuf {
    find_axiom_dir().join("attestations.json")
}

/// Every attestation issued so far. A missing ledger is an empty one: nothing
/// has been attested yet, which is different from failing to read it.
pub fn load_attestations() -> Result<Vec<ProvenanceAttestation>> {
    load_attestations_from(&attestation_ledger_path())
}

/// As above, from an explicit ledger. Kept separate so a caller that must not
/// touch the working directory, a test above all, can point somewhere else.
pub fn load_attestations_from(path: &std::path::Path) -> Result<Vec<ProvenanceAttestation>> {
    // A missing or empty ledger is an empty one. A file with content that yields
    // no records is corruption, and `load_records` returns None for it, so this
    // reports an error rather than reading a damaged ledger as nothing attested.
    if !path.exists() {
        return Ok(Vec::new());
    }
    match load_records(path) {
        Some(records) => Ok(records),
        None => {
            if std::fs::read_to_string(path)
                .map(|r| r.trim().is_empty())
                .unwrap_or(true)
            {
                Ok(Vec::new())
            } else {
                anyhow::bail!("{path:?} exists but no attestation records could be read from it")
            }
        }
    }
}

/// Parse a record file that is either a JSON array (the format these files used
/// to be written whole in) or JSONL, one record per line (the append-only
/// format now). A blank or trailing line, and a final line torn by a crash
/// mid-append, are skipped rather than failing the read.
fn parse_records<T: DeserializeOwned>(raw: &str) -> Option<Vec<T>> {
    if raw.trim().is_empty() {
        return Some(Vec::new());
    }
    if raw.trim_start().starts_with('[') {
        // A whole-file array. A rename-over write is atomic, so a reader sees
        // the old array or the new one; a parse failure here means a genuinely
        // malformed file, which the caller retries or reports.
        return serde_json::from_str(raw).ok();
    }
    // JSONL. Only the last line can be torn, because appends add at the end, so
    // a line that does not parse is dropped rather than the whole file lost.
    Some(
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<T>(l).ok())
            .collect(),
    )
}

/// Read a record file that another agent may be writing as we look at it, with
/// brief retries. Under a lock the read is clean; `verify` and startup read
/// without one, where a torn append can be caught and is settled by a retry.
///
/// `None` means the file is missing or empty. `Some(records)` may be empty for
/// an empty file. A non-empty file that yields no records is corruption, and is
/// returned as `None` so a caller wrapping it in a `Result` can say so.
fn load_records<T: DeserializeOwned>(path: &std::path::Path) -> Option<Vec<T>> {
    for attempt in 0..5 {
        if !path.exists() {
            return None;
        }
        match std::fs::read_to_string(path) {
            Ok(raw) if raw.trim().is_empty() => return None,
            Ok(raw) => match parse_records::<T>(&raw) {
                Some(records) if !records.is_empty() => return Some(records),
                // Non-empty file, nothing parsed: either mid-write, or corrupt.
                _ => std::thread::sleep(std::time::Duration::from_millis(2 * (attempt + 1))),
            },
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(2 * (attempt + 1))),
        }
    }
    // After the retries, one last honest reading: parseable records if any, or
    // None for a file that has content but yields none.
    match std::fs::read_to_string(path).ok().as_deref() {
        Some(raw) if !raw.trim().is_empty() => parse_records::<T>(raw).filter(|r| !r.is_empty()),
        _ => None,
    }
}

/// Does this file still hold a whole-file JSON array, the pre-JSONL format?
fn is_array_format(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|raw| raw.trim_start().starts_with('['))
        .unwrap_or(false)
}

/// Append one record as a JSONL line, migrating an array-format file to lines
/// first so the result is never half array and half lines.
///
/// The append is not a rename-over-whole-file, so it does not rewrite every
/// record on every write, which was O(n) per append and O(n^2) over a session.
/// The cost is that a crash mid-append can leave a torn final line, which the
/// loaders skip. Callers hold the index lock across the read-check-append, so
/// two agents do not interleave.
fn append_record<T: Serialize + DeserializeOwned>(
    path: &std::path::Path,
    record: &T,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() && is_array_format(path) {
        let existing: Vec<T> = load_records(path).unwrap_or_default();
        let mut buf = String::new();
        for r in &existing {
            buf.push_str(&serde_json::to_string(r).map_err(std::io::Error::other)?);
            buf.push('\n');
        }
        axiom_ast::write_atomically(path, buf.as_bytes())?;
    }
    let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
    line.push('\n');
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()?;
    Ok(())
}

/// Append one attestation to the ledger, refusing any record that does not
/// chain onto the current tail.
pub fn append_attestation(attestation: &ProvenanceAttestation) -> Result<()> {
    append_attestation_to(&attestation_ledger_path(), attestation)
}

/// Append one attestation, refusing any record that does not chain onto the
/// current tail of `path`.
///
/// The check is not a formality. `seal` is a digest *over* `previous_seal`, so
/// a record cannot be re-linked after it is generated: fixing up the field
/// would invalidate the seal and any signature covering it. The link therefore
/// has to be chosen when the record is built, and the only way to choose it
/// correctly is to read the tail under the same lock that will do the write.
///
/// Appending a record built against a different tail leaves a ledger that
/// `verify_chain` rejects from then on, and nothing about the failure points
/// back at the append that caused it. Refusing here turns that silent, permanent
/// corruption into an error at the call that is wrong.
pub fn append_attestation_to(
    path: &std::path::Path,
    attestation: &ProvenanceAttestation,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Read-modify-write, so two agents appending at once would otherwise drop
    // one of the records. The lock makes the sequence atomic, and it has to
    // cover the link check too: a tail read before the lock can be stale by the
    // time the write happens.
    let _lock = axiom_ast::IndexLock::acquire(path)?;
    let all = load_attestations_from(path).unwrap_or_default();

    let tail = all.last().map(|a| a.seal.as_str()).unwrap_or("");
    if attestation.previous_seal != tail {
        anyhow::bail!(
            "this record does not chain onto the ledger: it names predecessor {},              but the ledger currently ends at {}. Build the record against the tail              read under the ledger lock; the seal covers previous_seal, so it cannot              be corrected afterwards.",
            if attestation.previous_seal.is_empty() {
                "(none)"
            } else {
                &attestation.previous_seal
            },
            if tail.is_empty() { "(empty)" } else { tail },
        );
    }

    append_record(path, attestation)?;
    Ok(())
}

/// Append a record without checking that it chains, for tests that need to put
/// a ledger into a state a correct caller could not produce.
///
/// Real tampering happens by writing the file, not by calling this crate, so a
/// test that simulates an attacker has to be able to bypass the check the same
/// way an attacker does.
#[doc(hidden)]
pub fn append_attestation_unlinked_to(
    path: &std::path::Path,
    attestation: &ProvenanceAttestation,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = axiom_ast::IndexLock::acquire(path)?;
    append_record(path, attestation)?;
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
/// How long `axiom_run_tests` lets a suite run before killing it. A test suite
/// is slower than a snippet, so this is minutes by default, separate from the
/// evaluator's `AXIOM_EVAL_TIMEOUT_SECS`.
fn test_timeout() -> std::time::Duration {
    std::env::var("AXIOM_TEST_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(std::time::Duration::from_secs(600))
}

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

/// Where the Tree-CRDT operation log lives, beside the index it describes.
pub fn crdt_op_log_path() -> PathBuf {
    find_axiom_dir().join("crdt_ops.json")
}

/// Every operation recorded so far. A missing log is an empty one.
pub fn load_crdt_ops(path: &std::path::Path) -> Vec<axiom_crdt::TreeOp> {
    load_records(path).unwrap_or_default()
}

/// Append one operation.
///
/// Without this the CRDT never leaves the process that produced it. Each server
/// started with an empty tree and saw only its own operations, so two agents
/// working the same workspace reported different Merkle roots and neither could
/// see the other's nodes. There were no merge conflicts because there was no
/// merge: the convergence the type provides was only ever exercised by the
/// in-process swarm simulation.
///
/// The operations are commutative, so replaying them in whatever order the file
/// happens to hold converges to the same tree. That is the property the CRDT was
/// chosen for, and it is what makes appending to a shared file enough.
pub fn append_crdt_op(path: &std::path::Path, op: &axiom_crdt::TreeOp) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = axiom_ast::IndexLock::acquire(path)?;
    append_record(path, op)?;
    Ok(())
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
    /// The `.axiom` directory this server reads from and writes to.
    ///
    /// `find_index_file` walks up from the working directory to find the index,
    /// and the server inherits its client's working directory, which may be a
    /// subdirectory of the project. Reads came from the discovered directory
    /// while every write, the ledger, the op log and a persisted mutation, went
    /// to `<cwd>/.axiom`, so an agent working from a subdirectory wrote where
    /// the next read would not look. Recording the directory here and deriving
    /// every path from it keeps the two together.
    axiom_dir: PathBuf,
}

impl AxiomMcpServer {
    /// The ledger of issued attestations, under this server's `.axiom`.
    pub fn ledger_path(&self) -> PathBuf {
        self.axiom_dir.join("attestations.json")
    }

    /// The Tree-CRDT operation log, under this server's `.axiom`.
    pub fn op_log_path(&self) -> PathBuf {
        self.axiom_dir.join("crdt_ops.json")
    }

    /// The index this server persists a mutated symbol into.
    pub fn index_path(&self) -> PathBuf {
        self.axiom_dir.join("index.json")
    }
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

        // Reads and writes share one directory. When an index was discovered,
        // it is that index's own `.axiom`; otherwise it is `<cwd>/.axiom`, so a
        // server started with no index still writes somewhere consistent.
        let axiom_dir = match index_path.and_then(|p| p.parent()) {
            Some(dir) => dir.to_path_buf(),
            None => std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".axiom"),
        };

        let wasi_engine = Arc::new(WasiEngine::new()?);
        // Each server is a distinct replica. Sharing one id across processes
        // makes concurrent agents produce identical Lamport stamps, and a
        // last-writer-wins rule cannot order a tie it cannot see.
        let tree_crdt = Arc::new(TreeCrdt::new(std::process::id()));

        // Replay what other agents have recorded, so this server starts from the
        // shared state rather than an empty tree of its own.
        {
            let ops = load_crdt_ops(&axiom_dir.join("crdt_ops.json"));
            for op in ops {
                tree_crdt.apply_op(op);
            }
        }

        Ok(Self {
            verifications: Arc::new(RwLock::new(HashMap::new())),
            ast_index,
            wasi_engine,
            tree_crdt,
            axiom_dir,
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
                    },
                    // The loop these tools are for, in the field MCP provides so
                    // a client can put it in front of the model without the
                    // agent having to discover the order by trial. The index is
                    // a snapshot taken by `axiom scan`; a change on disk since
                    // then is not reflected until the next scan.
                    "instructions": INSTRUCTIONS
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
                            "description": "The tests that reach a symbol, so a change can be checked without running everything. impacted_tests holds what to run: direct dependents, and tests reaching the symbol through an accessor. tests_by_depth also lists tests that reach it through another class, at depth 2 and beyond, which are not in impacted_tests because including them costs more precision than it gains; widen max_depth to move them into the answer. An empty result means none were found in the index, which is not the same as nothing being affected.",
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
                            "description": "Run a snippet and report what happened. The snippet is written in the language of the file the symbol came from, and is run by that language's toolchain: rustc for Rust, wasmtime for a WAT or wasm snippet, and python, node, deno or tsc, go and javac for the rest. A symbol the index does not recognise is treated as Rust, and 'anonymous' forces it; pass a symbol that is indexed to have its own language chosen. Takes a few hundred milliseconds, since it invokes a real compiler. engine says which one answered. The snippet runs with a confined environment, so it cannot read the signing key or the operator's other secrets. A language with no evaluator, a toolchain that is not installed, and a name matching several symbols are all refused rather than guessed at, and a snippet that does not terminate is killed, along with anything it started, and reported as TIMEOUT. Never returns PASSED for something that did not run.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "symbol_path": { "type": "string", "description": "The symbol whose language to evaluate in; an unrecognised name is treated as Rust, 'anonymous' forces Rust" },
                                    "code_snippet": { "type": "string", "description": "The code to run. Rust and WAT run in tier 1; other languages in their own toolchain" }
                                },
                                "required": ["code_snippet"]
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
                                    "ctop_task_id": { "type": "string" },
                                    "agent_identity": { "type": "string", "description": "What to record as the author. Self-declared: axiom stores what you send and does not check it. It is covered by the seal, so it cannot be edited afterwards, and by the signature when a key is configured, which is what ties it to an issuer. Omit it and the record reads 'unattributed' rather than naming an agent nothing established. Printable single-line text, at most 128 characters." }
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
                        },
                        {
                            "name": "axiom_run_tests",
                            "description": "Run the project's own test command and record the outcome so a provenance record can rest on it. Unlike axiom_record_verification, axiom runs the command itself and observes the exit code, so the record says 'executed', not 'reported'. Build the command from the tests axiom_get_blast_radius named, so only the affected tests run: for example 'cargo test --test e2e_test search_modes', or 'pytest tests/test_gate.py::test_is_open'. The command runs in the workspace root with a confined environment, so it cannot read the signing key, and is killed with everything it started if it outruns AXIOM_TEST_TIMEOUT_SECS (default 600). A non-zero exit is recorded as a failed check; attesting against a failed check is refused.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "command": { "type": "string", "description": "The test command to run, e.g. 'cargo test --test e2e_test name' or 'pytest file::test'" },
                                    "task_id": { "type": "string", "description": "Identifier to attest against later; one is generated if omitted" },
                                    "symbol_path": { "type": "string", "description": "The symbol the tests cover, recorded for context; optional" }
                                },
                                "required": ["command"]
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
                    Ok(val) => {
                        // A tool that ran and reported a problem carries an
                        // `error` field in its payload. MCP surfaces such a
                        // result to the model only when `isError` is set, so a
                        // not-found or refusal reaches the agent as something to
                        // react to rather than as an ordinary answer. It stays a
                        // result, not a JSON-RPC error, because the tool did run.
                        let is_error = val.get("error").is_some();
                        JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: Some(json!({
                                "isError": is_error,
                                "content": [
                                    {
                                        "type": "text",
                                        "text": serde_json::to_string_pretty(&val).unwrap_or_default()
                                    }
                                ]
                            })),
                            error: None,
                        }
                    }
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
                let symbol = args
                    .get("symbol_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
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
                let symbol = args
                    .get("symbol_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("anonymous");
                let snippet = args
                    .get("code_snippet")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // The snippet is evaluated in the language of the file the
                // symbol was indexed from. Handing a Java symbol to rustc came
                // back with a syntax error that blamed the caller instead of
                // naming the real limit, so the extension travels with the call
                // and the engine picks the toolchain. A language with no
                // toolchain still refuses rather than guessing.
                let language = self.ast_index.language_of_symbol(symbol);

                // An ambiguous name resolves to no language, and no language
                // means Rust, which is how `isOpen` in a repository holding
                // Java, Kotlin and JavaScript got compiled by rustc and came
                // back as a syntax error. Say the name matches several things
                // instead of picking a compiler on the caller's behalf.
                if language.is_none() {
                    let candidates = self.ast_index.candidates_for(symbol);
                    if candidates.len() > 1 {
                        return Ok(json!({
                            "task_id": "eval_ambiguous_symbol",
                            "status": "EVALUATOR_UNAVAILABLE",
                            "engine": "tier2_native",
                            "passed_checks_count": 0,
                            "failed_checks": [{
                                "symbol": symbol,
                                "error_type": "AmbiguousSymbol",
                                "expected": "one symbol, so its language is known",
                                "actual": format!("{:?} matches {} symbols", symbol, candidates.len()),
                                "hint": "Name one of the candidates. Which language the snippet is evaluated in follows from which symbol was meant."
                            }],
                            "candidates": candidates.iter().take(10).collect::<Vec<_>>()
                        }));
                    }
                }

                let report = self
                    .wasi_engine
                    .execute_eval_in(symbol, snippet, language.as_deref())
                    .await?;

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
                let symbol = args
                    .get("symbol_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let agent_identity = match agent_identity_of(&args) {
                    Ok(i) => i,
                    Err(e) => return Ok(json!({ "error": e })),
                };
                let task_id = match args.get("ctop_task_id").and_then(|v| v.as_str()) {
                    Some(t) if !t.is_empty() => t,
                    _ => {
                        return Ok(json!({
                            "error": "ctop_task_id is required: an attestation must name the sandbox run it rests on"
                        }));
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
                        }));
                    }
                    Some(v) if !v.passed => {
                        return Ok(json!({
                            "error": format!(
                                "verification {task_id:?} did not pass ({}); a record may only be issued for a check that succeeded",
                                v.detail
                            )
                        }));
                    }
                    Some(v) => v.clone(),
                };

                let root = self.tree_crdt.compute_tree_merkle_root();

                // Link, seal, sign and append under one lock. The chain link has
                // to be known before the record is sealed, and the seal before it
                // is signed, so reading the tail and writing the record cannot be
                // two separate steps without a second agent slipping between them.
                let ledger_path = self.ledger_path();
                let _ledger_lock = match axiom_ast::IndexLock::acquire(&ledger_path) {
                    Ok(l) => l,
                    Err(e) => {
                        return Ok(json!({ "error": format!("could not lock the ledger: {e}") }));
                    }
                };
                let existing = load_attestations_from(&ledger_path).unwrap_or_default();
                let previous_seal = existing.last().map(|a| a.seal.clone()).unwrap_or_default();

                // Two real Merkle roots the engine maintains, not a constant and
                // a slice of one. `parent_merkle_root` used to be the literal
                // `merkle_root_prev_77a1` on every record ever issued, and
                // `commit_merkle_root` the first eight hex of the CRDT root, so
                // a reader could not tell one attested state from another. The
                // AST index root is a digest over every indexed symbol and its
                // body hash, so it moves when the code the record is about
                // moves; the CRDT tree root is the multi-agent mutation state.
                // Both are covered by the seal.
                let code_root = self.ast_index.compute_merkle_root();
                let attestation = ProvenanceAttestation::generate(NewAttestation {
                    parent_merkle_root: &root,
                    commit_merkle_root: &code_root,
                    agent_identity: &agent_identity,
                    prompt,
                    symbol_path: symbol,
                    ctop_task_id: task_id,
                    verified_by: &verification.kind,
                    verification_detail: &verification.detail,
                    previous_seal: &previous_seal,
                });

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

                // Persist it, or verification later has nothing to look up. An
                // append, not a whole-file rewrite: the chain was checked
                // against `previous_seal` above, under this lock.
                if let Err(e) = append_record(&ledger_path, &attestation) {
                    return Ok(json!({
                        "error": format!("could not record the attestation: {e}")
                    }));
                }

                Ok(json!(attestation))
            }

            "axiom_apply_mutation" => {
                // Required, not defaulted. A missing symbol_path used to become
                // the literal `module::fn`, so a malformed call wrote a node
                // under a name no source declares and persisted it to the
                // index. The same for node_id, which keyed the CRDT op.
                let symbol = match required_str(&args, "symbol_path") {
                    Ok(s) => s,
                    Err(e) => return Ok(json!({ "error": e })),
                };
                let node_id = match required_str(&args, "node_id") {
                    Ok(s) => s,
                    Err(e) => return Ok(json!({ "error": e })),
                };
                let content = match required_str(&args, "content") {
                    Ok(s) => s,
                    Err(e) => return Ok(json!({ "error": e })),
                };

                let op = self
                    .tree_crdt
                    .insert_node("root", node_id, symbol, "function", content);

                // Record it where the next agent will see it.
                if let Err(e) = append_crdt_op(&self.op_log_path(), &op) {
                    return Ok(json!({
                        "error": format!("could not record the mutation: {e}")
                    }));
                }
                self.ast_index
                    .index_node(symbol, "function", content, vec![]);
                let root = self.tree_crdt.compute_tree_merkle_root();

                // Save updated index to disk
                // Persist just this symbol. Writing the whole in-memory index
                // here would also write back every other symbol as this process
                // last saw it, discarding what another agent recorded meanwhile.
                let index_path = self.index_path();
                if let Err(e) = self.ast_index.persist_symbol(&index_path, symbol) {
                    eprintln!("Warning: Failed to save {}: {}", index_path.display(), e);
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
                        }));
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
                let max = args
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20) as usize;
                let requested = args
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("literal");

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
                    Err(e) => {
                        Ok(json!({ "error": e, "query": query, "mode_requested": requested }))
                    }
                }
            }

            "axiom_run_tests" => {
                let command = match required_str(&args, "command") {
                    Ok(c) => c.to_string(),
                    Err(e) => return Ok(json!({ "error": e })),
                };
                let task_id = args
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .filter(|t| !t.is_empty())
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| format!("test_run_{}", std::process::id()));

                // Run in the workspace the index describes: the parent of the
                // `.axiom` directory, so the project's own test runner sees its
                // own tree.
                let workspace = self
                    .axiom_dir
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));

                // Through the shell, so a full command line with arguments works
                // as written. run_with_timeout confines the environment, so the
                // command cannot read the signing key, and ends the whole
                // process tree if it outruns the deadline.
                let mut cmd = if cfg!(windows) {
                    let mut c = std::process::Command::new("cmd");
                    c.args(["/C", &command]);
                    c
                } else {
                    let mut c = std::process::Command::new("sh");
                    c.args(["-c", &command]);
                    c
                };
                cmd.current_dir(&workspace);

                let done = match axiom_vmm::native::run_with_timeout(cmd, test_timeout()) {
                    Ok(d) => d,
                    Err(e) => {
                        return Ok(json!({
                            "error": format!("could not run the test command: {e}")
                        }));
                    }
                };

                if done.timed_out {
                    // A run that was killed says nothing about whether the tests
                    // would have passed, so it is not recorded as a verification.
                    return Ok(json!({
                        "task_id": task_id,
                        "status": "TIMEOUT",
                        "passed": false,
                        "note": format!(
                            "the command was killed after {}s; raise AXIOM_TEST_TIMEOUT_SECS if the suite is genuinely slow. Nothing was recorded.",
                            test_timeout().as_secs()
                        )
                    }));
                }

                let passed = done.succeeded();
                let tail = |s: &str| {
                    s.lines()
                        .rev()
                        .take(40)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n")
                };

                // Recorded as "executed": axiom ran it and saw the exit code, so
                // it can vouch for the outcome, unlike a "reported" check.
                self.verifications.write().unwrap().insert(
                    task_id.clone(),
                    Verification {
                        passed,
                        kind: "executed".to_string(),
                        detail: format!("axiom ran: {command}"),
                    },
                );

                Ok(json!({
                    "task_id": task_id,
                    "status": if passed { "PASSED" } else { "FAILED" },
                    "passed": passed,
                    "recorded_as": "executed",
                    "command": command,
                    "stdout": tail(&done.stdout),
                    "stderr": tail(&done.stderr),
                    "note": "Axiom ran this command and observed its exit code. A provenance record issued against this task will say the outcome was executed by axiom."
                }))
            }

            _ => anyhow::bail!("Unknown tool: {}", tool_name),
        }
    }
}
