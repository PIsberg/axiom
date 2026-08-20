# AXIOM: AI Agent Machine-Readable Usage Guide
<!-- Target Audience: Autonomous AI Coding Agents, LLM Orchestrators, MCP Clients -->

---

## 1. Quickstart: Registering AXIOM MCP Server

To enable an AI agent to use Axiom for zero-clone code navigation and sub-millisecond test sandboxing, register the Axiom server in your agent configuration file:

### Cursor (`~/.cursor/mcp.json`) / Claude Desktop (`claude_desktop_config.json`)
```json
{
  "mcpServers": {
    "axiom": {
      "command": "C:/dev/private/axiom/target/x86_64-pc-windows-msvc/release/axiom.exe",
      "args": ["serve"]
    }
  }
}
```

Or run `axiom mcp-config` from the command line to generate this automatically.

---

## 2. Standard Autonomous Agent Development Loop

When developing or refactoring code, follow this 4-step workflow:

```mermaid
sequenceDiagram
    autonumber
    actor AI as AI Coding Agent
    participant MCP as Axiom MCP Gateway
    participant AST as Merkle AST Engine
    participant VMM as Sub-15ms Sandbox
    participant Ledger as Provenance Ledger

    AI->>MCP: axiom_query_symbol("auth::service::validate_token")
    MCP->>AST: Fetch AST Node + Signature
    AST-->>AI: Returns AST JSON (Dependencies, Range, Hash)

    AI->>MCP: axiom_get_blast_radius("auth::service::validate_token")
    MCP->>AST: Compute Reverse Transitive Call Graph
    AST-->>AI: Impacted Tests: ["test_auth_validation"] (99.98% Pruned)

    loop Test-Driven Hypothesis Validation
        AI->>MCP: axiom_eval_patch(snippet="assert!(validate_token(\"...\"));")
        MCP->>VMM: Instant WASI/MicroVM Sandbox (<0.1ms)
        VMM-->>AI: CTOP JSON Report (Status: PASSED / FAILED + Hints)
    end

    AI->>MCP: axiom_apply_mutation(node_id, symbol, content)
    MCP->>AST: Commutative Tree-CRDT Merge (0 Merge Conflicts)
    AST-->>AI: New Merkle Root

    AI->>MCP: axiom_attest_commit(prompt, symbol, ctop_task_id)
    MCP->>Ledger: Generate SLSA L4+ Cryptographic Seal
    Ledger-->>AI: Signed Attestation Proof (Ed25519)
```

---

## 3. MCP Tool Reference & Schemas

### 3.1 `axiom_query_symbol`
Inspect an AST node definition, signature, and dependency list without cloning the repository.

* **Request**:
  ```json
  {
    "name": "axiom_query_symbol",
    "arguments": {
      "symbol_path": "auth::service::validate_token"
    }
  }
  ```
* **Response**:
  ```json
  {
    "id": "node_667fde43c6ec",
    "symbol_path": "auth::service::validate_token",
    "kind": "function",
    "hash": "667fde43c6ec9c70fcec87a12c1068a5cc3e949cbb77bec9c82dd67c5a389d12",
    "signature": "auth::service::validate_token",
    "dependencies": ["jwt::verifier"]
  }
  ```

---

### 3.2 `axiom_get_blast_radius`
Calculates the exact subset of unit and integration tests affected by changes to a symbol.

* **Request**:
  ```json
  {
    "name": "axiom_get_blast_radius",
    "arguments": {
      "symbol_path": "auth::service::validate_token",
      "max_depth": 5
    }
  }
  ```
* **Response**:
  ```json
  {
    "symbol": "auth::service::validate_token",
    "impacted_tests": ["test_auth_validation"],
    "pruned_test_percentage": 99.98
  }
  ```
* **Agent Directive**: Execute *only* the tests in `impacted_tests`. Do not run the full test suite.

---

### 3.3 `axiom_eval_patch`
Executes code changes inside an isolated memory sandbox in microsecond latency and returns structured CTOP diagnostics.

* **Request**:
  ```json
  {
    "name": "axiom_eval_patch",
    "arguments": {
      "symbol_path": "auth::service::validate_token",
      "code_snippet": "assert!(validate_token(\"secret_token_123\"));"
    }
  }
  ```
* **Response (Success)**:
  ```json
  {
    "task_id": "task_auth_val_01",
    "engine": "tier1_wasi_wasmtime",
    "status": "PASSED",
    "execution_duration_ms": 0.001,
    "failed_checks": [],
    "passed_checks_count": 1,
    "stdout": "Evaluated snippet: assert!(validate_token(\"secret_token_123\"));"
  }
  ```
* **Response (Failure - Agent Self-Correction Signal)**:
  ```json
  {
    "task_id": "task_auth_val_02",
    "engine": "tier1_wasi_wasmtime",
    "status": "FAILED",
    "execution_duration_ms": 0.002,
    "failed_checks": [
      {
        "symbol": "auth::service::validate_token",
        "error_type": "AssertionError",
        "expected": "token.len() > 10",
        "actual": "token.len() == 0",
        "hint": "Expected token length > 10, got length 0"
      }
    ]
  }
  ```
* **Agent Directive**: If `status == "FAILED"`, inspect `failed_checks[].hint` and modify your code logic before requesting commit.

---

### 3.4 `axiom_apply_mutation`
Applies commutative Tree-CRDT AST modifications to the shared repository graph without merge conflicts.

* **Request**:
  ```json
  {
    "name": "axiom_apply_mutation",
    "arguments": {
      "node_id": "node_auth_val",
      "symbol_path": "auth::service::validate_token",
      "content": "pub fn validate_token(t: &str) -> bool { t.len() > 10 }"
    }
  }
  ```
* **Response**:
  ```json
  {
    "status": "APPLIED",
    "new_merkle_root": "1a9dab4c9c1e3f13a9a206501457b6511de0f552df57039e9952559e81590366",
    "active_ast_nodes": 102
  }
  ```

---

### 3.5 `axiom_attest_commit`
Generates a cryptographically sealed SLSA Level 4+ attestation proof binding prompt intent, AST delta, and sandbox verification results.

* **Request**:
  ```json
  {
    "name": "axiom_attest_commit",
    "arguments": {
      "prompt": "Fix token validation minimum length invariant",
      "symbol_path": "auth::service::validate_token",
      "ctop_task_id": "task_auth_val_01"
    }
  }
  ```
* **Response**:
  ```json
  {
    "parent_merkle_root": "merkle_root_prev_77a1",
    "commit_merkle_root": "merkle_root_1a9dab4c",
    "agent_identity": "agent_axiom_v1",
    "prompt_digest": "blake3:933176e307f6ccab",
    "sandbox_trace_hash": "trace:d06ad0bb6e8b09eb",
    "ctop_proof_hash": "task_auth_val_01",
    "timestamp": "2026-08-20T22:28:44Z",
    "signature": "ed25519_seal_d06ad0bb6e8b09eb1f644ce0626fdc6e"
  }
  ```

### 3.6 `axiom_search_regex`
Fast Zoekt-style in-memory trigram regex and literal search across all files and symbols in the CAS.

* **Request**:
  ```json
  {
    "name": "axiom_search_regex",
    "arguments": {
      "query": "validate_token",
      "max_results": 10
    }
  }
  ```
* **Response**:
  ```json
  {
    "query": "validate_token",
    "matches_count": 3,
    "matches": [
      {
        "file_path": "auth::service::validate_token",
        "line_number": 1,
        "line_content": "auth::service::validate_token"
      }
    ]
  }
  ```

---

## 4. CLI Reference Commands

| Command | Purpose |
|---|---|
| `axiom serve` | Starts the native MCP server over `stdio` (JSON-RPC 2.0) |
| `axiom eval --symbol <SYM> -c <CODE>` | Runs an instant isolated sandbox evaluation |
| `axiom symbol --path <SYM>` | Queries AST node metadata and type signatures |
| `axiom blast-radius --symbol <SYM>` | Calculates impacted tests and pruned percentage |
| `axiom bench --iterations <N>` | Executes performance latency benchmarks |
| `axiom demo` | Runs live end-to-end agent workflow demonstration |
| `axiom swarm --agents <N> --ops <M>` | Runs multi-agent Tree-CRDT swarm simulation |
| `axiom verify --symbol <SYM> --prompt <P>` | Cryptographically audits SLSA L4+ commit seal |
| `axiom mcp-config` | Outputs ready-to-copy JSON configuration for AI IDEs |
| `axiom scan --path <DIR>` | Scans and indexes a real codebase into the Merkle AST CAS |
| `axiom search --query <STR>` | Ultra-fast Zoekt trigram regex and text search across repo |
| `axiom watch --path <DIR>` | Watches filesystem for live incremental AST Merkle updates |
| `axiom git-export` | Exports current Merkle state to a Git-compatible commit |
| `axiom dashboard` | Displays live real-time terminal metrics & swarm activity |
