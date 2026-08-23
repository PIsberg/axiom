# AXIOM: The Agent-Native Autonomous Software Engine

[![CI](https://github.com/PIsberg/axiom/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/PIsberg/axiom/actions/workflows/ci.yml)
[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue)](LICENSE)
[![Commercial use: separate licence](https://img.shields.io/badge/commercial%20use-separate%20licence-orange)](https://deversity.se/pricing.html)
[![Rust 2021 edition](https://img.shields.io/badge/rust-2021%20edition-b7410e)](Cargo.toml)
[![Tested on Linux and Windows](https://img.shields.io/badge/tested-linux%20%7C%20windows-lightgrey)](.github/workflows/ci.yml)

**Source-available, not open source.** Any noncommercial use is permitted;
commercial use needs a separate licence. The badge is not the whole story and the
distinction matters, so the terms are set out once, in [License](#-license).

> **Replace passive, text-oriented version control (Git) and 10-minute CI/CD pipelines with an active, executable Merkle AST graph and an in-process sandbox, purpose-built for autonomous AI coding agents.**

---

## 🎬 Watch It Run

[![AXIOM Agent Native Engine (23 000 times faster workflow)](https://i.ytimg.com/vi/44XF6IimE1I/hqdefault.jpg)](https://www.youtube.com/watch?v=44XF6IimE1I)

**[AXIOM Agent Native Engine (23 000 times faster workflow)](https://www.youtube.com/watch?v=44XF6IimE1I)**, by Anonymous Claudeholics

A walkthrough of the engine end to end: the repository answering queries as a
running MCP server, the blast radius narrowing a change down to the tests it can
actually reach, and the sandbox returning a verdict inside the same loop rather
than minutes later in CI. The number in the title is the projected speedup for
the test round trip from
[the speed report](docs/axiom_speed_comparison_report.md), which is the stage an
agent pays on every iteration.

---

## 🚀 Key Features

* **Repository as an Active MCP Server**: Direct structured AST and semantic graph navigation over JSON-RPC 2.0 (`stdio`), eliminating local file clones.
* **Snippet Evaluation in the Symbol's Own Language**: a hypothesis is checked without a CI round trip. Rust goes to `rustc`, WebAssembly to wasmtime, and Python, JavaScript, TypeScript, Go, Java, Kotlin and Scala to their own toolchains. Measure it on your own machine with `axiom bench`; on the development machine the Rust median is around 175ms, which is `rustc` rather than the harness. A language with no evaluator, or a toolchain that is not installed, is refused rather than guessed at, and a snippet that does not terminate is killed rather than allowed to hold the session.
* **Predictive Blast-Radius Test Pruning**: reverse dependency reachability narrows a change to the tests that reach the symbol, with the deeper layers surveyed and reported separately so a caller can widen. Measured on this repository at depth 1, by asking the shipped CLI about each of its 335 non-test symbols in turn against 54 tests: 94 of them reach at least one test, those 94 select a mean of 2.5 tests and prune a mean of 83.3%, and the mean Jaccard overlap between the answers for two different symbols is 0.09. The remaining 241 reach no test, which is the honest answer for a private helper nothing exercises directly and not a claim that changing it is safe.
* **Ultra-Fast Zoekt Trigram Search**: In-memory sliding trigram index (`[u8; 3] -> HashSet<Path>`) providing $<1\text{ms}$ regex and literal search without disk I/O.
* **Concurrent Agents on One Workspace**: mutations are recorded to a shared, commutative operation log, so agents in separate processes converge on the same tree whatever order their work lands in. Measured with twelve agents mutating at once: twelve operations recorded, none lost or duplicated, and one identical Merkle root across four replay orders.
* **Recorded Provenance**: every attested change ties a prompt, a symbol, the sandbox run that checked it, and the Merkle roots either side into a record you can read back later. Issued only after the run it names has passed.

---

## ⚡ Live Agent Workflow & Performance Matrix

Run `axiom demo` to see the autonomous agent self-healing loop in action:

```text
================================================================================
   ⚡ AXIOM: THE AGENT-NATIVE AUTONOMOUS SOFTWARE ENGINE DEMONSTRATION ⚡
================================================================================

🔹 [Step 1/5] Agent queries symbol graph over MCP (Zero Local Clones)...
   ↳ Received AST Node: 'auth::service::validate_token' in 0.035 ms

🔹 [Step 2/5] Calculating topological blast radius across Merkle DAG...
   ↳ Tests in this workspace: 1 | Targeted: 1
   ↳ Pruned 0.00% of them, computed in 0.031 ms

🔹 [Step 3/5] Simulating Agent testing a BUGGY hypothesis (empty token) in sandbox...
   ↳ Sandbox Caught Bug: ❌ CTOP_STATUS = FAILED (Sandbox latency: 171.402 ms)
   ↳ Structured Diagnostic Hint: 'Expected token length > 10, got length 0'

🔹 [Step 4/5] Agent automatically self-heals using the diagnostic hint & re-tests...
   ↳ Sandbox Self-Correction Pass: ✅ CTOP_STATUS = PASSED (Sandbox latency: 168.955 ms)

🔹 [Step 5/5] Recording the provenance of the change...
   ↳ Provenance record written, tying the prompt, symbol and sandbox result together

================================================================================
                         📊 PERFORMANCE BENCHMARK MATRIX
================================================================================
 Measured on async-test-lib: 459 files, 5,934 symbols, 2,219 tests
 -------------------------------------------------------------------------------
 Index the tree            1.5 s warm, 3.8 s cold, producing a 49 MB index
 Server startup            1.1 s, once per session
 Symbol search             0.2 to 0.4 ms warm  (grep over the same tree: 52 ms)
 Blast radius              1.2 to 1.4 ms warm, selecting 32 of 2,219 tests
 Evaluate a snippet        176 ms median, rustc dominating
 Provenance record         Prompt, symbol and check recorded together, signed if
                           a key is configured, chained so a deletion shows
================================================================================

The saving is in what does not run: 26 tests instead of 2,219 for a one-method
change, verified by breaking that method and watching the two tests that cover
it fail. Search is faster than grep only once the index is warm, and the fixed
cost takes about 50 queries to repay. Figures and method:
docs/axiom_speed_comparison_report.md.
```

### Step-by-Step Autonomous Workflow & Demonstration

The standard agent interaction loop consists of 6 core phases:

1. **Zero-Clone Merkle Indexing (`axiom scan`)**
   - Ingests polyglot repositories into an in-memory Content-Addressable Storage (CAS) Merkle DAG (`.axiom/index.json`), eliminating local repository clones.
   - Example: `axiom scan --path .` (indexes 34 files & 404 symbols in ~200–300ms).

2. **Ultra-Fast Trigram Symbol Search (`axiom search`)**
   - Zoekt-style in-memory trigram index searches millions of lines in sub-millisecond time without disk I/O.
   - Example: `axiom search --query "handle_request"`

3. **Symbol Metadata & Dependency Extraction (`axiom symbol`)**
   - Extracts complete AST node metadata, signatures, line ranges, and direct symbol dependencies.
   - Example: `axiom symbol --path "AxiomMcpServer::handle_request"`

4. **Topological Blast-Radius Test Pruning (`axiom blast-radius`)**
   - Traverses reverse transitive call graphs across the Merkle DAG to prune 75–99% of tests down to the exact subset reaching the modified symbol.
   - Example: `axiom blast-radius --symbol "AxiomMcpServer::handle_request" --depth 2`

5. **Sub-Second Micro-Sandboxing & Diagnostic Feedback (`axiom eval`)**
   - Compiles and evaluates isolated candidate patches in the language's native runtime with structured diagnostic hints.
   - Example: `axiom eval --symbol "AstNode" -c "assert_eq!(1 + 1, 2);"`

6. **Commutative Tree-CRDT Swarm Convergence (`axiom swarm`)**
   - Multi-agent swarms execute concurrent AST mutations across replicas with 0 merge conflicts and microsecond convergence.
   - Example: `axiom swarm --agents 10 --ops 50` (1,000 concurrent operations converged in ~16ms).

---

## 📈 Where the Time Goes

![Axiom versus Git plus CI across five workflow stages](docs/images/axiom_speed_comparison.png)

The figure charts the five stages of an agent's edit loop against the same loop
on Git plus a CI pipeline: workspace setup, symbol search, incremental
compilation, the test round trip, and resolving concurrent edits. Each stage is
attacked by a different mechanism rather than by making the old one faster. The
repository answers queries as a running MCP server instead of being cloned; an
in-memory trigram index replaces disk-bound search; identical AST hashes reuse
already-compiled bytecode; blast-radius pruning selects the handful of tests a
change can reach; and Tree-CRDTs converge concurrent edits instead of producing
merge conflicts.

The stage that dominates in practice is the test round trip, because it is the
one an agent pays on every iteration. The numbers behind the chart, and the
baselines they are measured against, are in
[docs/axiom_speed_comparison_report.md](docs/axiom_speed_comparison_report.md).
They describe the target architecture, so treat them as design goals rather than
as measurements of the current build.

---

## 🛠️ Prerequisites & System Requirements

* **Rust**: `1.75+` (with `cargo` and `rustc` in your system `PATH`)
* **Optional, per language you want `axiom_eval_patch` to run**: `python3` or
  `python`, `node`, `deno` or `tsc`, `go`, a JDK for `javac` and `java`. Each is
  looked for on `PATH` when it is first needed; a missing one produces
  `EVALUATOR_UNAVAILABLE` naming it, never a verdict. `AXIOM_EVAL_NATIVE=off`
  refuses to run any of them.
* **C++ Build Tools**:
  * **Windows**: Visual Studio 2019/2022 C++ Build Tools (`vcvars64.bat` / MSVC)
  * **Linux / WSL2**: `build-essential`, `clang`, `libssl-dev`
  * **macOS**: Xcode Command Line Tools

---

## 📦 Setup from Zero

### 1. Clone the Repository
```bash
git clone https://github.com/your-org/axiom.git
cd axiom
```

### 2. Build Release Binary
* **Windows (PowerShell with MSVC)**:
  ```powershell
  cmd.exe /c "`"C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools\VC\Auxiliary\Build\vcvars64.bat`" && cargo build --release --bin axiom"
  ```
* **Linux / macOS**:
  ```bash
  cargo build --release --bin axiom
  ```

The compiled binary will be located at:
* **Windows**: `target/release/axiom.exe`
* **Linux / macOS**: `target/release/axiom`

### 3. Add to System PATH (Optional)
To use `axiom` globally across any project directory, add the release folder to your system `PATH` or copy the binary:
```bash
# Windows PowerShell
[Environment]::SetEnvironmentVariable("Path", $env:Path + ";$PWD\target\release", "User")

# Linux / macOS
cp target/release/axiom /usr/local/bin/
```

---

## 🤖 Connecting to AI Agents (MCP Setup)

Axiom natively implements the **Model Context Protocol (MCP)**. Any MCP-compatible agent (Claude Code, Cursor, Windsurf, AGY) can connect directly to Axiom.

### 1. Index Your Target Repository
Navigate to your target project folder (e.g. Java, Rust, Python, TypeScript) and scan it:
```bash
axiom scan --path .
```
This parses all source files into the Merkle AST CAS and writes the persistent index to `.axiom/index.json`.

### 2. Generate MCP Configuration
Run:
```bash
axiom mcp-config
```
Copy the generated configuration into your AI client's settings:

#### For Claude Desktop (`claude_desktop_config.json`):
```json
{
  "mcpServers": {
    "axiom": {
      "command": "axiom",
      "args": ["serve"]
    }
  }
}
```

#### For Claude Code / Cursor / Custom Agents:
```json
{
  "mcpServers": {
    "axiom": {
      "command": "/path/to/axiom",
      "args": ["serve"]
    }
  }
}
```

---

## ⚡ CLI Command Reference

| Command | Description |
|---|---|
| `axiom serve` | Starts the native MCP server over `stdio` (JSON-RPC 2.0) |
| `axiom scan --path <DIR>` | Scans and indexes a codebase into the Merkle AST CAS & `.axiom/index.json` |
| `axiom search --query <STR> [--mode literal\|regex\|auto]` | Text search across the repository. Literal by default, so `.` and `(` match themselves; `--mode regex` compiles the query as a pattern, `--mode auto` picks regex only for queries that cannot be meant as literal text |
| `axiom eval --symbol <SYM> -c <CODE>` | Runs an isolated sandbox evaluation with compiler verification |
| `axiom blast-radius --symbol <SYM>` | Computes impacted tests and pruned percentage |
| `axiom symbol --path <SYM>` | Queries AST node metadata, signatures, and imports |
| `axiom cache-audit --path <DIR>` | Measures what a verdict cache would decide against what the blast radius selects, without caching anything or skipping any test. See [docs/verdict_cache_audit.md](docs/verdict_cache_audit.md); on this repository it currently says do not build it |
| `axiom bench --iterations <N>` | Measures how long a sandbox evaluation takes on this machine, reporting min, median, max and mean |
| `axiom demo` | Runs live end-to-end self-healing agent demonstration |
| `axiom swarm --agents <N> --ops <M>` | Runs multi-agent Tree-CRDT swarm concurrency simulation |
| `axiom verify --symbol <SYM> --prompt <P>` | Looks up the provenance record for a symbol and prompt, and checks it is unaltered |
| `axiom keygen --out <PATH>` | Generates an Ed25519 keypair for signing provenance records. Keep the private key outside any workspace you index |
| `axiom mcp-config` | Outputs ready-to-copy JSON configuration for AI IDEs |
| `axiom watch --path <DIR>` | Polls a cheap fingerprint of the tree and re-scans the whole tree when it changes. The re-index is a full re-parse, not an incremental one |
| `axiom git-export` | Writes `.axiom/export.md` summarising the index and Merkle root. It does not touch git |
| `axiom dashboard` | Prints a one-shot snapshot of the workspace: symbol counts by kind, index file size, CRDT node count, Merkle root, provenance record count. Not a TUI and not a live feed |

---

## 🧪 Running Tests

To run the full automated test suite, 138 tests across 24 binaries, of which 38 are the end-to-end integration tests in `crates/axiom-cli/tests/e2e_test.rs`:
```bash
cargo test
```

### Six of the end-to-end tests, and what each one pins:
* `test_e2e_agent_full_loop_over_mcp`: Full multi-language scan, symbol query, sandbox error trap, self-healing, Tree-CRDT mutation, and provenance record.
* `test_e2e_disk_persistence_cross_instance`: Cross-process `.axiom/index.json` save and load verification.
* `test_e2e_truth_preserving_assertions`: Real compiler execution catching panics and invariant failures with zero false-positives.
* `test_e2e_java_production_vs_test_classification`: Exact JUnit `@Test` vs. production class filtering.
* `test_e2e_dynamic_merkle_root_uniqueness`: BLAKE3 Merkle root determinism across AST deltas.
* `test_e2e_swarm_50_agents_concurrency`: 50 concurrent agents executing 2,000 operations with 0 merge conflicts.

---

## 🛡️ Security & Provenance

![The three concentric containment layers around an agent's workspace](docs/images/axiom_security_architecture.png)

The figure shows the containment model: an agent never touches the host or the
codebase directly, but works from inside three nested boundaries. Tool calls
first pass an intercepting proxy that sanitises paths and strips command
chaining before anything reaches the workspace. Whatever survives executes in an
ephemeral sandbox with no network egress and bounded CPU and memory, so a
runaway or prompt-injected instruction has nowhere to escape to. At the centre
sits the Merkle AST store, which is content-addressed and immutable, so a
mutation produces a new root rather than overwriting the old one and every state
the repository has held stays reachable.

The reasoning behind each layer, and the threats each one is meant to stop, are
in [docs/axiom_security_framework.md](docs/axiom_security_framework.md).

### The provenance record

`axiom_attest_commit` writes a record to `.axiom/attestations.json` tying five
things together: the prompt that asked for the change, the symbol it touched, the
sandbox task that checked it, the Merkle root before and after, and when it
happened.

A record is only issued against a check that happened and passed. Naming a check
the server has no record of, or one that failed, is refused.

There are two kinds of check, and the record says which it rests on. `sandbox`
means axiom compiled and ran the code itself, which it can only do for Rust.
`reported` means an agent ran something else, a project's own test suite for
instance, and told axiom the outcome through `axiom_record_verification`. Axiom
vouches for the first and is repeating the second, and `axiom verify` says so:

```
Checked by:    reported (mvn -pl async-test-lib test -Dtest=ConcurrencyRunnerTest)

Axiom did not run this check. The outcome above was reported by
the agent that asked for the record.
```

### Who issued it

`agent_identity` is what the caller asked to be recorded as. Axiom stores it
without checking it, so by itself it is a claim rather than an answer. It used to
be the constant `agent_axiom_v1` on every record, which read as an author when it
identified nobody, and a caller that supplied a value had it silently dropped.

It is now taken as an argument, and it is hashed into the seal and covered by the
signature. That is what makes it worth having: it cannot be edited after the
record is written, and on a signed record it is bound to the key that issued it.
`axiom verify` prints it and says which of those cases applies, so an unsigned
name is never shown as though something had established it. A record whose caller
named nobody reads `unattributed`.

Read a record back with:

```bash
axiom verify --symbol "auth::service::validate_token" --prompt "Tighten the guard"
```

This looks the record up. A symbol nothing was attested for, or the right symbol
with a prompt that record was not issued for, exits non-zero and says which.

### Signing

The `seal` field is a BLAKE3 digest over the record's own fields. It shows a
stored record has not been altered, and nothing about who wrote it: anyone
holding the same inputs recomputes it.

Signing separates those two claims. Generate a keypair and point axiom at it:

```bash
axiom keygen --out ~/.config/axiom/agent.key
export AXIOM_SIGNING_KEY_FILE=~/.config/axiom/agent.key
```

Records issued after that carry an Ed25519 signature over the record's contents
together with the symbol and prompt, so a signature cannot be moved onto a
different record, and editing a stored one breaks it.

**Keep the key away from the workspace, and note what that buys.** The threat is
someone who can write `.axiom/attestations.json`. A key sitting beside the
records it signs is readable by exactly that person, so signing with it would add
nothing the digest did not already give you. What a signature is good for is a
record that stays checkable elsewhere: a reader holding only the public key can
tell whether a given signer issued it.

That reader has to say which signer they expect. Checking a signature against the
key inside the record shows only that the two agree, which is why `axiom verify`
reports "signed, key not anchored" unless you name one:

```bash
axiom verify --symbol "auth::service::validate_token"              --prompt "Tighten the guard"              --trusted-key ~/.config/axiom/agent.pub
```

A record signed by a different key, altered since it was written, or carrying no
signature at all exits non-zero and says which. That last case matters: producing
an unsigned record takes no key, since the seal is a digest over public inputs,
so anyone able to write the ledger can manufacture one. Accepting it when a
signer was demanded would defeat the check, so naming `--trusted-key` means a
record must be signed by that key to count.

With no key configured, records are still written and still tamper-evident; they
are anonymous, and `verify` says so rather than implying more.

### The ledger is a chain

A signature stops a record being forged or edited. It does nothing about one
being *removed*: what is left still verifies, and the history just looks shorter
than it was. So each record names the seal of the record before it, and both the
seal and the signature cover that link. Removing a record leaves the next one
pointing at something that is no longer there:

```
LEDGER ALTERED: chain breaks between record 0 and record 1: record 1 names
predecessor blake3_seal_0df065..., but the record before it seals as
blake3_seal_0147bb.... A record has been removed or reordered.
```

`verify` reports the chain alongside the record, and refuses to call a record
trusted when `--trusted-key` was given and the ledger has been altered.

**One deletion this cannot catch:** truncating the tail. Nothing points at the
last record, so removing it leaves a chain that is internally consistent.
Catching that needs the expected head written down somewhere the person who can
write the ledger cannot reach, which is outside what a single file can do for
itself.

None of this is a reproducible-build attestation. It does not rebuild anything or
establish that a build was hermetic.

Nor is any of this a reproducible-build attestation in the SLSA sense. Nothing
here rebuilds your artifact independently or proves the build was hermetic. It
records that a particular prompt, symbol and sandbox result were seen together on
one machine, which is worth having and is a smaller claim than the phrase implies.

---

## 📄 License

[PolyForm Noncommercial License 1.0.0](LICENSE). Any noncommercial purpose is
permitted, which covers personal study, research, hobby projects, and use by
charities, schools, public research bodies, and government institutions,
whatever their funding. Commercial use needs a separate licence.

For commercial licensing, contact peter.isberg@deversity.se
(pricing: <https://deversity.se/pricing.html>).

Note that PolyForm Noncommercial is a source-available licence, not an open
source one under the OSI definition, because it restricts the field of use.
