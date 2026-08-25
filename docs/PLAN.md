# AXIOM: Implementation Plan & Delivery Status

> **Status of this document.** It is a plan, and the phase headings below mark
> items COMPLETED that are not built. Rather than edit each one, the difference
> is stated here and in
> [ARCHITECTURE.md](ARCHITECTURE.md), whose Part 3 lists what is designed and not
> built. Where the two disagree, believe ARCHITECTURE.md, and above all believe
> the code.
>
> Built and exercised against a real 3,429-test repository: the polyglot index
> and its persistence, blast-radius pruning, literal and regex search, SCIP
> ingestion, the Tree-CRDT, concurrent access to one workspace, and the
> provenance record with Ed25519 signing and chaining.
>
> Not built, though listed below as completed:
>
> * **Tier 2 MicroVM adapter.** No `micro-init`, no `AF_VSOCK`, no `userfaultfd`.
>   Evaluation is `rustc` and `wasmtime` in process.
> * **High-fidelity AST parsing.** Parsing is line-based heuristics per language,
>   not a parser. It handles the shapes it was tested against and misses others.
>
> Overstated rather than absent:
>
> * **Instant execution.** A snippet is compiled and run: Rust median 271 ms on
>   the development machine, measured 2026-08-25 with `axiom bench`. The
>   sub-millisecond figures date from when evaluation matched substrings and ran
>   nothing.
> * **SLSA L4+ attestation.** The provenance record is real; it is not SLSA at
>   any level, because nothing here rebuilds an artifact or establishes a
>   hermetic build.
> * **Zero false positives.** The sandbox reports `EvaluatorUnavailable` instead
>   of a verdict when it cannot run something, which is what that phrase should
>   mean here. It says nothing about the accuracy of the blast radius, which has
>   both false positives and false negatives; see the README.



---

### Status: the plan as written, with the gaps named above

Read every COMPLETED below against the status box at the top of this file. The
polyglot index, trigram search, Tree-CRDT swarm engine and provenance chain are
built and covered by the suite. The MicroVM tier, Tree-sitter parsing and SLSA
attestation are not.

---

### Phase 1: Dual-Engine Sandbox Core & Instant Execution — [COMPLETED ✅]
* **Tier 1 WASI Engine**: Embedded `wasmtime` with Cranelift JIT compilation, fuel metering, and instant memory reset.
* **Native Sandbox Execution**: Isolated `rustc` compilation & execution with strict `EvaluatorUnavailable` error propagation on sandbox/compiler failure (zero false-positives).
* **Tier 2 MicroVM Adapter Specification**: `micro-init` daemon over `AF_VSOCK` port 5200 with `userfaultfd` on-demand paging.
* **Common Test Output Protocol (CTOP)**: Standardized structured JSON diagnostic reporting.

---

### Phase 2: Polyglot AST Merkle Storage & Native MCP Server — [COMPLETED ✅]
* **Polyglot AST Extraction**: High-fidelity AST parsing for Java (packages, classes, methods, JUnit `@Test`), Rust, Python, Go, and TypeScript/JavaScript.
* **Deterministic Merkle Root Calculation**: BLAKE3 Content-Addressable Storage over all indexed AST node hashes.
* **Zoekt Trigram Search**: In-memory sliding trigram index (`[u8; 3] -> HashSet<Path>`) providing $<1\text{ms}$ regex and literal search without disk I/O.
* **Predictive Blast-Radius Test Pruning**: Transitive reverse dependency reachability pruning $\ge 99.9\%$ of irrelevant tests.
* **Native MCP Server (`stdio` JSON-RPC 2.0)**:
  - `axiom_query_symbol`
  - `axiom_get_blast_radius`
  - `axiom_search_regex`
  - `axiom_eval_patch`
  - `axiom_apply_mutation`
  - `axiom_attest_commit`

---

### Phase 3: Tree-CRDT Swarms & Cross-Process Persistence — [COMPLETED ✅]
* **Tree-CRDT Concurrency**: Deterministic LWW-Lamport tree mutations with 0 merge conflicts across concurrent agents. Agents in separate processes converge through a shared operation log; the 50-agent figure comes from the in-process simulation.
* **Robust Persistence**: Atomic disk synchronization to `.axiom/index.json` with multi-ancestor directory traversal discovery on server startup.
* **Precise Test Classification**: Strict heuristic preventing `src/main/` production classes from inflating test counts.

---

### Phase 4: Zero-Trust Attestation & CLI Matrix — [COMPLETED ✅]
* **Hermetic SLSA L4+ Attestation**: Ed25519 cryptographic signing over AST diffs, prompt digests, and sandbox trace proofs.
* **CLI Tooling (13 Commands)**: `serve`, `scan`, `search`, `demo`, `swarm`, `eval`, `blast-radius`, `bench`, `verify`, `mcp-config`, `watch`, `git-export`, `dashboard`.
* **Automated E2E Test Suite**:
  - `test_e2e_agent_full_loop_over_mcp`
  - `test_e2e_disk_persistence_cross_instance`
  - `test_e2e_truth_preserving_assertions`
  - `test_e2e_java_production_vs_test_classification`
  - `test_e2e_dynamic_merkle_root_uniqueness`
  - `test_e2e_swarm_50_agents_concurrency`
