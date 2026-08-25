# Axiom Autonomous Agent Guidelines

When interacting with a codebase equipped with the Axiom engine, ALWAYS follow the agent-native loop over MCP stdio rather than legacy whole-repo scans:

1. **Symbol Navigation (`axiom_query_symbol`)**:
   - Query exact AST symbol metadata, declarations, and dependency graphs before modifying code.
   - Use short names or fully-qualified paths.

2. **Predictive Test Selection (`axiom_get_blast_radius`)**:
   - Before running test suites, query `axiom_get_blast_radius` on target symbols.
   - Run ONLY the impacted test suites identified in `impacted_tests` (typically 80-99% pruned).

3. **Sub-Second Sandbox Validation (`axiom_eval_patch`)**:
   - Validate logic hypotheses and test snippets in the native multi-language sandbox.
   - Supports Java (JUnit/asserts), Rust, Python, TypeScript/JavaScript, Go, Scala, Kotlin, and WASM.

4. **Targeted Test Execution (`axiom_run_tests`)**:
   - Run test commands targeted to the impacted tests to capture real execution verification.

5. **Atomic Multi-Agent Mutation (`axiom_apply_mutation`)**:
   - Apply Tree-CRDT atomic symbol updates to prevent textual diff conflicts in multi-agent swarms.

6. **Cryptographic Attestation (`axiom_attest_commit`)**:
   - Seal verified changes with prompt, symbol, and execution verification in the tamper-evident Merkle ledger.
