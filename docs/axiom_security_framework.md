# Axiom Zero-Trust Security Framework
### Verifiable Threat Containment and Cryptographic Provenance for AI-Native Coding Swarms

Traditional software development and version control systems (like Git and GitHub Actions) were designed with a fundamental assumption: **human developers are the primary actors.** Security is typically enforced via peer review (Pull Requests) and centralized, asynchronous CI environments.

In the agentic age, this model completely breaks down. Autonomous AI coding agents can generate high volumes of code and execute code modifications in milliseconds [1, 25]. If an agent is compromised, experiences a prompt injection, or hallucinates highly destructive code, traditional systems have no way of containing the threat in real time before it spreads.

**Axiom solves this by flipping the security model.** Instead of reactive, post-hoc review, Axiom implements a proactive, **zero-trust defense-in-depth model** that contains and mathematically verifies every single agent action [3, 25, 38].

---

## 1. The Three Concentric Jails of Active Containment

Axiom wraps the AI-native workspace in three nesting layers of strict isolation to guarantee that rogue, compromised, or buggy agents can never escape to the host operating system or compromise the core codebase.

```
+-------------------------------------------------------------------------+
|  LAYER 1: ZERO-TRUST INTERCEPTING PROXY                                 |
|  (Sanitizes LLM tool calls, paths, and prevents command injection)      |
|                                                                         |
|    +---------------------------------------------------------------+    |
|    |  LAYER 2: EPHEMERAL SANDBOX (WASI & KVM MicroVM)               |    |
|    |  (Strict zero-egress, seccomp filters, CPU/Memory quotas)      |    |
|    |                                                               |    |
|    |    +-----------------------------------------------------+    |    |
|    |    |  LAYER 3: IMMUTABLE MERKLE AST CORE                 |    |    |
|    |    |  (BLAKE3 Content-Addressable Storage Graph)         |    |    |
|    |    +-----------------------------------------------------+    |    |
|    +---------------------------------------------------------------+    |
+-------------------------------------------------------------------------+
```

### Layer 1: The Zero-Trust Intercepting Proxy
*   **Purpose**: Neutralizes prompt-injected execution payloads before they reach the local workspace [61, 83].
*   **Mechanism**: All communications between the AI agent, the LLM API, and the Model Context Protocol (MCP) server pass through an intercepting proxy [61, 83]. This proxy sanitizes system paths, validates tool schemas, and strips out command chaining sequences (such as `; rm -rf /` or `&& curl ...`) embedded maliciously inside code mutations or prompt strings [61, 83].

### Layer 2: Ephemeral Sandbox Isolation
If a malicious or buggy instruction passes Layer 1, it is isolated inside isolated runtime sandboxes with zero host escape vectors [3, 84].

1.  **Tier 1 WASI Engine (<1ms Execution)**: For lightweight tasks, code runs inside an embedded `wasmtime` compiler with strict resource (fuel) limits [7, 21]. Copy-on-Write (CoW) linear memory ensures that memory state is reset in `<0.05µs` [7].
2.  **Tier 2 MicroVM Snapshot Engine (<15ms Execution)**: For dynamic languages (Python, Node.js) and compiled runtimes (Java/JUnit), Axiom launches isolated Firecracker/KVM microVMs with a highly stripped `<3MB` Linux kernel [8, 34, 68].
    *   **Zero Egress**: The microVM has **no virtual bridge interface attached**, isolating guest networks completely from the host or external internet [73, 83].
    *   **Direct Memory-Bus IPC**: The host communicates with the guest micro-init daemon strictly over virtual memory sockets (`AF_VSOCK` port 5200) rather than standard network sockets [8, 34, 71].
    *   **Seccomp Containment**: MicroVM processes run inside isolated Linux user namespaces and strictly drop all unneeded host syscalls using custom seccomp profiles [73, 84].

### Layer 3: Immutable Merkle AST Core
*   **Purpose**: Prevents tampering with codebase history and structure.
*   **Mechanism**: Axiom does not store files as raw text [14]. Every function, class, and block is parsed via Tree-sitter into normalized Abstract Syntax Tree (AST) nodes, hashed using the BLAKE3 algorithm, and stored in a global Content-Addressable Storage (CAS) Merkle DAG [4, 15]. No file can be modified or overwritten without immediately mutating the root hash, preventing silent backdoor insertions [4, 15].

---

## 2. SLSA Level 4+ Cryptographic Attestation Chain

The crown jewel of Axiom's security model is its **SLSA Level 4+ Cryptographic Attestation Seal** [17, 38, 56]. 

Every code change proposed and applied via the `axiom_apply_mutation` tool generates a mathematically binding, cryptographically signed provenance receipt [31, 42]. This receipt binds the developer’s/agent’s intent directly to the execution verification proof [12, 17, 38]:

$$\text{Seal} = \text{Sign}_{K_{\text{axiom}}}\Big(\text{ParentRoot} \mid \text{ASTDelta} \mid \text{PromptDigest} \mid \text{SandboxTrace} \mid \text{CTOPProof}\Big)$$

### Anatomy of the Cryptographic Seal:
1.  **Parent Merkle Root**: The cryptographically secure starting state of the repository before the mutation was applied [12, 17, 31, 38].
2.  **Commit Merkle Root (AST Delta)**: The direct structural changes made to the Abstract Syntax Tree (AST), proving exactly which nodes were inserted, updated, or deleted [12, 17, 31, 38].
3.  **Agent Identity**: The cryptographic identity of the AI agent or swarm node that initiated the change [12, 31].
4.  **Prompt Digest**: A BLAKE3 hash of the natural language user prompt and the agent's chain-of-thought reasoning trace [12, 17, 31, 38, 57].
5.  **Sandbox Trace Hash**: A cryptographic hash of the sealed microVM execution log, proving the code executed in containment [12, 17, 31, 38, 57].
6.  **CTOP Pass Proof**: A signed test execution token showing that the code successfully passed all required compiler and test suites under the Common Test Output Protocol [12, 17, 38, 57].

These elements are packed and signed using an **Ed25519 private key** ($K_{\text{axiom}}$) held strictly in secure host memory [12, 17, 25, 38].

$$\text{Verify}(\text{Attestation}) \implies \text{Code is formally proven to have passed sandbox tests.}$$ [13]

This guarantees that a production deployment pipeline can verify that code was not just signed, but **formally executed inside an isolated, secure sandbox, and successfully passed all deterministic test suites** before being integrated [57].

---

## 3. Threat Model Comparison: Legacy vs. Axiom

| Threat Vector | Legacy Git + CI/CD | Axiom Zero-Trust Engine |
| :--- | :--- | :--- |
| **Malicious Package/Dependency Injection** | **HIGH**: Malicious code executed immediately on developer machines during install or CI pipelines. | **PREVENTED**: Isolated sandboxes operate under strict zero-egress. Malicious dependencies cannot communicate out [73, 83]. |
| **Command Injection via Prompts** | **HIGH**: Direct access to local bash terminals or raw IDE scripts allows easy host shell escape. | **PREVENTED**: Neutralized at Layer 1 by the path-sanitizing and schema-validating Intercepting Proxy [61, 83]. |
| **Backdoor Code Insertion** | **MEDIUM**: Large line diffs allow clever obfuscation to slip past busy human reviewers. | **PREVENTED**: Cryptographic attestation verifies that the exact code in the CAS matches the sandbox execution trace [12, 17, 38, 57]. |
| **Host Operating System Escape** | **HIGH**: Docker-based CI/CD runners share the host kernel and are easily escaped. | **PREVENTED**: Isolated KVM microVMs with stripped kernel run with strict seccomp filtering [73, 84]. |

---

## 4. How to Cryptographically Audit Code in Axiom

Security engineers or CI compliance pipelines can audit any symbol or commit history instantly using the `verify` CLI command [24, 29]:

```bash
axiom verify --symbol auth.service.validateToken --prompt "Add JWT expiration validation"
```

This command parses the AST history of the symbol, pulls the associated Ed25519 attestation seal, and verifies the mathematical signature against the repository's cryptographic keys [12, 17, 29, 31, 38]. If any node, prompt digest, or test footprint has been tampered with, the signature immediately fails [13, 31].
