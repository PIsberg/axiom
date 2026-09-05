# Axiom Autonomous Agent Engine — Future Enhancements Roadmap

This document captures the strategic and architectural roadmap for extending Axiom's agent-native Merkle AST CAS, verification pipeline, sandbox infrastructure, and multi-agent coordination.

---

## 1. Merkle Ledger Patch Memory & Verified Fix Cache [Implemented]
- **Status**: Completed in PR #72 (`crates/axiom-core/src/mcp.rs`, `crates/axiom-proto/src/lib.rs`).
- **Concept**: AST-hash indexed historical patch memory linking error signatures directly to successful provenance-attested mutations.
- **Mechanism & Capabilities**:
  - `axiom_proto::compute_diagnostic_fingerprint` generates BLAKE3 hashes of `symbol_ast_hash:error_signature`.
  - Attesting commits (`axiom_attest_commit`) with `error_signature` and `patch_content` records verified fix candidates into `.axiom/fix_cache.json`.
  - When compiler or test failures occur in `axiom_eval_patch`, matching fix candidates are retrieved with 0ms overhead and returned in `CtopReport.suggested_fixes`.
  - Exposes MCP resources `axiom://fixes` and `axiom://fixes/{fingerprint}` for direct agent discovery and retrieval.

---

## 2. Dynamic Agent Sub-Graph Context Prompts [Implemented]
- **Status**: Completed in PR #72 (`crates/axiom-core/src/mcp.rs`).
- **Concept**: Automated prompt template expansion with pre-computed topological context.
- **Mechanism & Capabilities**:
  - Dynamically resolves symbol candidates in MCP prompt handlers (`axiom_review_patch`, `axiom_targeted_refactor`, `axiom_attest_task`).
  - Pre-computes and embeds adaptive token slices, blast-radius impacted test suites, and topological causal propagation paths directly into the initial turn user message.
  - Eliminates context-gathering round trips for autonomous agents and swarms.

---

## 3. Tree-Sitter & SCIP Incremental Semantic Indexing
- **Concept**: Transition from hybrid heuristic/regex parsing to full concrete syntax tree (CST) incremental indexing via Tree-Sitter and SCIP (Source Code Intelligence Protocol).
- **Mechanism**:
  - Native Tree-Sitter grammars for C, C++, C#, Kotlin, Swift, Scala, Go, Rust, Python, and TypeScript.
  - Incremental re-parsing: re-parse only changed byte ranges on disk mutations instead of re-scanning entire files.
  - Precise symbol definitions, local variables, type hierarchies, and cross-file references with zero heuristic false positives.

---

## 4. Synthetic Dependency Graph Ingestion (DI & Reflection) [Implemented]
- **Status**: Completed in PR #72 (`crates/axiom-ast/src/lib.rs`).
- **Concept**: Bridge the gap between static AST references and runtime dependency injection / reflection.
- **Mechanism & Capabilities**:
  - Automatically detects DI annotations (`@Inject`, `@Autowired`, `@Resource`, `@Component`, `@Service`, constructor parameters) in Java/JVM parsing.
  - Registers consumer-to-provider virtual dependency bindings in `AstIndex` (`di_consumers` and `di_providers`), persisted in `index.json`.
  - `compute_blast_radius` traverses synthetic DI consumer edges and implementor hierarchies, identifying downstream consumers and their test suites in `impacted_tests` and `causal_paths`.

---

## 5. MicroVM & Snapshot-Isolated Sandbox Execution [Kernel Isolation Implemented]
- **Status**: Windows Job Objects and Unix rlimit process isolation implemented in PR #73 (`crates/axiom-vmm/src/sandbox.rs`, `crates/axiom-vmm/src/native.rs`).
- **Concept**: Kernel-enforced execution containment preventing runaway resource exhaustion, process leaks, and secret exfiltration.
- **Mechanism & Capabilities**:
  - Windows Job Object sandbox (`SandboxGuard`) with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`, and memory/process ceilings.
  - Guarantees termination of entire child + grandchild process trees on timeout or unexpected exit.
  - Real peak memory accounting returned in `CtopReport.memory_allocated_bytes`.
  - Unix process group leadership (`setpgid`), address space memory limits (`RLIMIT_AS`), process limits (`RLIMIT_NPROC`), and negative-PID group termination.
  - Complete environment secret confinement (`is_refused_secret`) eliminating risk of signing key, AWS token, or credential leakage to untrusted agent snippets.
  - Future roadmap: Firecracker/gVisor microVMs with Copy-on-Write memory snapshots for full network and disk virtualization.

---

## 6. Distributed CAS & Multi-Agent Swarm Federation
- **Concept**: P2P or cloud-backed synchronization of Merkle AST CAS, CRDT op logs, and cryptographic provenance ledgers.
- **Mechanism**:
  - S3 / GCS / HTTP CAS backend for pre-compiled sandbox artifacts and attested verification seals.
  - CRDT vector clock synchronization across geographically distributed CI runners and local developer agent instances.
  - Cross-agent collaboration with verifiable provenance and zero merge conflicts across arbitrary branch boundaries.

---

## 7. CI / GitHub Action Provenance Gate & Git Verification [Implemented]
- **Status**: Completed in PR #73 (`.github/actions/axiom-gate/action.yml`, `.github/workflows/axiom-gate.yml`, `crates/axiom-cli/src/main.rs`).
- **Concept**: Native CI/CD provenance gate enforcing unbroken Merkle ledger chains, valid cryptographic seals, required Ed25519 signer anchors, and in-toto/SLSA v1.0 provenance compliance.
- **Mechanism & Capabilities**:
  - Reusable composite GitHub Action (`.github/actions/axiom-gate/action.yml`) verifying repository provenance before merges.
  - Automated workflow (`.github/workflows/axiom-gate.yml`) testing on Ubuntu and Windows.
  - CLI gate flags on `axiom git-hook --verify`:
    - `--strict`: Fails if the attestation ledger is empty or untrusted.
    - `--trusted-key <KEY>`: Validates that all ledger attestations are signed by the specified Ed25519 public key.
    - `--slsa <PATH>`: Exports and validates in-toto / SLSA v1.0 provenance statements during the gate verification.

