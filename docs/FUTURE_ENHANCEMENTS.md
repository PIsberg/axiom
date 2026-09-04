# Axiom Autonomous Agent Engine — Future Enhancements Roadmap

This document captures the strategic and architectural roadmap for extending Axiom's agent-native Merkle AST CAS, verification pipeline, sandbox infrastructure, and multi-agent coordination.

---

## 1. Merkle Ledger Patch Memory & Verified Fix Cache
- **Concept**: AST-hash indexed historical patch memory linking error signatures directly to successful provenance-attested mutations.
- **Mechanism**:
  - Hash AST sub-trees before and after verified mutations.
  - When an agent encounters a compiler or test failure in `axiom_eval_patch` or `axiom_run_tests`, compute a diagnostic fingerprint (error code + AST symbol hash).
  - Query CAS for previous attested mutations matching this fingerprint.
  - Deliver instant 0ms suggested patch candidates to the agent, enabling instant automated self-healing.

---

## 2. Dynamic Agent Sub-Graph Context Prompts
- **Concept**: Automated prompt template expansion with pre-computed topological context.
- **Mechanism**:
  - Extend MCP prompt handlers (`axiom_review_patch`, `axiom_targeted_refactor`, `axiom_attest_task`) to automatically resolve and embed adaptive token slices and causal call-paths directly into prompt messages.
  - Eliminates multi-turn context discovery cycles for autonomous agent swarms by delivering complete, pruned sub-graph context in the initial turn.

---

## 3. Tree-Sitter & SCIP Incremental Semantic Indexing
- **Concept**: Transition from hybrid heuristic/regex parsing to full concrete syntax tree (CST) incremental indexing via Tree-Sitter and SCIP (Source Code Intelligence Protocol).
- **Mechanism**:
  - Native Tree-Sitter grammars for C, C++, C#, Kotlin, Swift, Scala, Go, Rust, Python, and TypeScript.
  - Incremental re-parsing: re-parse only changed byte ranges on disk mutations instead of re-scanning entire files.
  - Precise symbol definitions, local variables, type hierarchies, and cross-file references with zero heuristic false positives.

---

## 4. Synthetic Dependency Graph Ingestion (DI & Reflection)
- **Concept**: Bridge the gap between static AST references and runtime dependency injection / reflection.
- **Mechanism**:
  - Detect DI annotations (`@Inject`, `@Autowired`, `@Component`, `@Provides`, `@Bean`, Guice `bind()`, Dagger modules, Spring contexts).
  - Synthesize virtual AST dependency edges between interface definitions, injection sites, and concrete implementations.
  - Blast-radius calculations accurately trace through injected services and dynamic dispatch that static AST callgraphs miss.

---

## 5. MicroVM & Snapshot-Isolated Sandbox Execution (Tier 3)
- **Concept**: Sub-50ms snapshot-isolated virtualization for arbitrary multi-language code execution.
- **Mechanism**:
  - Integrate Firecracker or gVisor microVMs with Copy-on-Write memory snapshots.
  - Allows full execution of arbitrary binaries, network mocks, and database integration tests in complete isolation from host environment secrets, keys, and disk state.
  - Sub-second restore time from warm snapshot states.

---

## 6. Distributed CAS & Multi-Agent Swarm Federation
- **Concept**: P2P or cloud-backed synchronization of Merkle AST CAS, CRDT op logs, and cryptographic provenance ledgers.
- **Mechanism**:
  - S3 / GCS / HTTP CAS backend for pre-compiled sandbox artifacts and attested verification seals.
  - CRDT vector clock synchronization across geographically distributed CI runners and local developer agent instances.
  - Cross-agent collaboration with verifiable provenance and zero merge conflicts across arbitrary branch boundaries.
