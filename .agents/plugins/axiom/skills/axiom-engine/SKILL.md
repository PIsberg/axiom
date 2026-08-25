---
name: axiom-engine
description: >-
  Agent-native software engineering workflow using the Axiom Merkle AST CAS,
  topological blast radius test pruning, sub-second multi-language sandbox evaluation,
  Tree-CRDT multi-agent mutation, and cryptographic provenance attestation.
---

# Axiom Engine Skill

Use this skill when developing, refactoring, evaluating, or testing code in a workspace indexed by Axiom.

## Autonomous Workflow Loop

### 1. Query Symbol Metadata
Before making edits, look up symbol dependencies, exact signatures, and hashes:
```json
{
  "name": "axiom_query_symbol",
  "arguments": {
    "symbol_path": "com.example.service.OrderService::processOrder"
  }
}
```

### 2. Compute Predictive Blast Radius
Determine which test suites are impacted by changes to the symbol:
```json
{
  "name": "axiom_get_blast_radius",
  "arguments": {
    "symbol_path": "com.example.service.OrderService::processOrder",
    "max_depth": 1
  }
}
```
Use `impacted_tests` to run only affected tests rather than full project suites.

### 3. Evaluate Hypotheses in Sandbox
Run rapid validation snippets across Java, Rust, Python, Go, TypeScript/JavaScript, Kotlin, and Scala:
```json
{
  "name": "axiom_eval_patch",
  "arguments": {
    "symbol_path": "com.example.service.OrderService",
    "code_snippet": "import com.example.service.OrderService;\npublic class AxiomEval {\n    public static void main(String[] args) {\n        OrderService svc = new OrderService();\n        assert svc.processOrder(100);\n    }\n}"
  }
}
```

### 4. Run Targeted Project Tests
Execute project test runners (e.g. Maven, Gradle, Cargo, Pytest) for impacted tests:
```json
{
  "name": "axiom_run_tests",
  "arguments": {
    "command": "mvn test -Dtest=OrderServiceTest#testProcessOrder"
  }
}
```

### 5. Attest Verified Changes
Record tamper-evident Ed25519-signed provenance in the ledger:
```json
{
  "name": "axiom_attest_commit",
  "arguments": {
    "prompt": "Implement validated order discount logic",
    "symbol_path": "com.example.service.OrderService::processOrder",
    "ctop_task_id": "test_run_12345",
    "agent_identity": "antigravity-pair-programmer"
  }
}
```
