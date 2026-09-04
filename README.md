# AXIOM: The Agent-Native Autonomous Software Engine

[![CI](https://github.com/PIsberg/axiom/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/PIsberg/axiom/actions/workflows/ci.yml)
[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue)](LICENSE)
[![Commercial use: separate licence](https://img.shields.io/badge/commercial%20use-separate%20licence-orange)](https://deversity.se/pricing.html)
[![Rust 2024 edition, MSRV 1.85](https://img.shields.io/badge/rust-2024%20edition%20%7C%20MSRV%201.85-b7410e)](Cargo.toml)
[![Tested on Linux and Windows](https://img.shields.io/badge/tested-linux%20%7C%20windows-lightgrey)](.github/workflows/ci.yml)

**Source-available.** Any noncommercial use is permitted; commercial use needs a
separate licence. The terms are set out in [License](#license).

> One binary that is both a CLI and an MCP server. It indexes a codebase into a
> symbol graph and answers an agent's questions against it: what does this symbol
> look like, which tests can reach it, does this snippet actually run, and what
> was checked before this change was recorded.

Axiom sits beside Git and CI rather than replacing them. Git stores your history
and CI gates your merges. Axiom removes the round trip an agent pays on every
iteration: instead of pushing and waiting minutes to learn that a change broke
something, the agent asks which tests can reach the symbol it touched, runs a
snippet in that symbol's own language, and gets an answer in milliseconds.

---

## Three Strengths

### Run only the tests a change can reach

The blast radius walks the reverse dependency graph from a changed symbol to the
tests that can observe it, so an agent runs those tests and skips the rest. On a
9,058-symbol Java tree with 3,429 tests, a change to one symbol selects a
median of **8 tests** and prunes a median of **99.8%** of the suite (seeded
sample of 60 symbols), and the selection itself takes 4.7 to 8.1 ms (measured
2026-08-25). The answers are specific to the
symbol: the mean pairwise Jaccard overlap between two symbols' selections is
0.01, so two different changes get two different test sets rather than one
generic subset. The saving grows with the suite, which makes large, slow test
suites exactly where the feature earns the most.

Ground truth backs the graph. `axiom cache-validate` breaks symbols on purpose,
runs the project's own suite, and confirms that every test that really failed
was one the blast radius selected.

### Coordinate many agents on one codebase

Mutations are recorded to a shared, commutative operation log backed by a
Tree-CRDT, so agents in separate processes converge on the same tree whatever
order their work lands in, with no locks and no merge conflicts. Measured on
2026-08-31: ten agents applying 1,000 operations concurrently completed in
**9.1 ms** with **zero merge conflicts** and one identical Merkle root across
every replica. The end-to-end suite pins the same property at larger scale:
50 agents, 2,000 operations, replicas converged.

### Near-instant code search

The server keeps an in-memory trigram index over the scanned tree. Warm, a
symbol query answers in **0.08 to 0.13 ms** and a text search in **0.27 to
0.36 ms**, against 80 ms for `grep -rn` over the same 898-file source tree
(measured 2026-08-25). The index is prepaid rather than free: a 3.3 s scan on
that tree, persisted to disk, plus 1 to 2 s of server startup per session, and
an agent that searches throughout a session repays it many times over.

---

## Watch It Run

[![AXIOM Agent Native Engine](https://i.ytimg.com/vi/44XF6IimE1I/hqdefault.jpg)](https://www.youtube.com/watch?v=44XF6IimE1I)

**[AXIOM Agent Native Engine](https://www.youtube.com/watch?v=44XF6IimE1I)**, by
Anonymous Claudeholics

A walkthrough of the engine end to end: the repository answering queries as a
running MCP server, the blast radius narrowing a change down to the tests it can
reach, and the evaluator returning a verdict inside the same loop rather than
minutes later in CI. The multiplier in the video title predates the
measurements; the figures to quote are in
[the speed report](docs/axiom_speed_comparison_report.md).

---

## What It Does

* **The repository answers as an MCP server.** Structural queries over JSON-RPC
  2.0 on stdio: symbol metadata, dependency edges, text search, blast radius.
* **Blast-radius test selection.** Reverse dependency reachability narrows a
  change to the tests that can reach the symbol, with deeper layers surveyed and
  reported separately so a caller can widen.
* **Snippet evaluation in the symbol's own language.** Rust goes to `rustc`,
  WebAssembly to wasmtime, and Python, JavaScript, TypeScript, Go, Java, Kotlin
  and Scala to their own toolchains. Every verdict comes from a real run of the
  code; when a toolchain is missing or a symbol is ambiguous, axiom says so
  instead of guessing, so an agent can trust a `PASSED` completely.
* **Concurrent agents on one workspace.** A shared, commutative operation log
  keeps separate processes convergent on one tree.
* **Trigram text search.** Sub-millisecond literal and regex search once the
  index is warm.
* **Recorded provenance.** Every attested change ties a prompt, a symbol, the
  check that verified it, and the Merkle roots either side into a sealed,
  chained, optionally Ed25519-signed record, issued only after the check it
  names was seen to pass.

---

## Measured Latencies

Every figure in this section was measured, not projected. Tree A figures are
from 2026-08-25, Tree B figures from 2026-08-31 and the `axiom bench` row from
2026-09-01, on one Windows machine,
release build. Re-run the commands in
[the speed report](docs/axiom_speed_comparison_report.md) to reproduce them on
your own tree and machine.

**Tree A**, `async-test-lib`: 898 Java source files, 9,058 indexed symbols, of
which 3,429 are tests.

| Operation | Measured | Notes |
|---|---|---|
| `axiom scan` | 3.3 s warm, 4.1 s cold | Produces a 61 MB index. Paid once, then again whenever the tree changes. |
| Server startup | 1.1 to 2.1 s | Loads that index and rebuilds the trigram index. Once per session. |
| `axiom_query_symbol`, warm | 0.08 to 0.13 ms median | In a running server, over MCP. Range across three runs. |
| `axiom_search_regex`, warm | 0.27 to 0.36 ms median | `grep -rn` over the same source tree: 80 ms. |
| `axiom_get_blast_radius`, warm | 4.7 to 8.1 ms median | Selecting 12 of 3,429 tests for one method, 99.65% pruned. |
| `axiom bench` (Rust snippet) | 220 ms median with the compile cache off, 125 ms with it on | `rustc` dominates; a cache hit skips it and still runs the binary. 20 iterations. |
| `axiom swarm --agents 10 --ops 50` | 9.5 ms for 1,000 operations | Zero merge conflicts, replicas converged. |

**Tree B**, this repository: 64 source files, 617 indexed symbols, of which 71
are tests. `axiom scan` takes 133 ms warm, and the same swarm run completes
1,000 operations in 9.1 ms with zero conflicts.

The blast-radius figures for both trees come from
[`.github/scripts/blast_radius_stats.py`](.github/scripts/blast_radius_stats.py),
which asks the shipped CLI about each non-test symbol in turn. Its output on
this repository on 2026-08-31, at depth 1:

```text
suite             71 tests
non-test symbols  546
reach >= 1 test   141 of 546 asked
tests selected    mean 11.2, median 4, max 40
pruned            mean 84.2%, median 94.4%
mean Jaccard      0.09
```

The two trees together show how the value scales: a median of 94.4% pruned on a
71-test suite, 99.8% on a 3,429-test one, and the wall-clock saving grows with
every test the suite adds. A symbol that reaches no test gets that reported as
the answer, which is the honest result for a helper nothing exercises directly.

---

## Prerequisites and System Requirements

* **Rust**: `1.85` or newer. The workspace is on the 2024 edition, which was
  stabilised in 1.85.
* **Optional, per language you want `axiom_eval_patch` to run**: `python3` or
  `python`, `node`, `deno` or `tsc`, `go`, a JDK for `javac` and `java`,
  `kotlinc`, `scala`. Each is looked for on `PATH` when it is first needed, and
  a missing one produces `EVALUATOR_UNAVAILABLE` naming it, so an agent always
  knows whether a verdict is available before relying on one.
* **C++ Build Tools**, for the `zstd-sys`, `wasmtime-internal-fiber` and
  `ittapi-sys` build scripts:
  * **Windows**: Visual Studio 2019/2022 C++ Build Tools (`vcvars64.bat` / MSVC)
  * **Linux / WSL2**: `build-essential`, `clang`, `libssl-dev`
  * **macOS**: Xcode Command Line Tools

---

## Setup from Zero

### 1. Clone the repository

```bash
git clone https://github.com/PIsberg/axiom.git
cd axiom
```

### 2. Build the release binary

* **Windows (PowerShell with MSVC)**:
  ```powershell
  cmd.exe /c "`"C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools\VC\Auxiliary\Build\vcvars64.bat`" && cargo build --release --bin axiom"
  ```
* **Linux / macOS**:
  ```bash
  cargo build --release --bin axiom
  ```

The compiled binary is at `target/release/axiom.exe` on Windows and
`target/release/axiom` elsewhere.

### 3. Add it to PATH (optional)

```bash
# Windows PowerShell
[Environment]::SetEnvironmentVariable("Path", $env:Path + ";$PWD\target\release", "User")

# Linux / macOS
cp target/release/axiom /usr/local/bin/
```

---

## Connecting to AI Agents (MCP Setup)

Axiom implements the **Model Context Protocol (MCP)**. Any MCP-compatible agent
(Claude Code, Claude Desktop, Cursor, Windsurf) can connect to it.

### 1. Index your target repository

Navigate to your target project folder (Java, Kotlin, Scala, Rust, Python,
TypeScript, JavaScript, Go, C++) and scan it:

```bash
axiom scan --path .
```

This parses all source files into the Merkle AST store and writes the persistent
index to `.axiom/index.json`.

**The server finds that index by walking up from its own working directory.** It
inherits the working directory of the agent that started it, which is your
project. Start the server anywhere at or below the directory you scanned and it
finds the index on its own.

#### Precise indexing with SCIP, recommended where a build exists

`axiom scan` without a SCIP index uses fast, build-free line parsers, which
index any tree instantly, including partial trees and mid-edit code. If your
project has a build, you can hand axiom a **SCIP** index instead, produced by
the language's own indexer running the real compiler, and the symbol graph, and
the blast radius over it, rest on the compiler's own resolved references.

For Java, generate one with [scip-java](https://sourcegraph.github.io/scip-java/)
and point axiom at it:

```bash
# In your Java project (Maven or Gradle):
cs launch com.sourcegraph:scip-java_2.13:<version> -- index --build-tool auto
# produces index.scip in the project root

axiom scan --scip index.scip --path .
```

`--path` names the project root the index's relative paths resolve against. The
index is written to `.axiom/index.json` exactly as a normal scan, so every MCP
tool and CLI command works against it unchanged; `axiom_get_blast_radius` gains
the most, since its edges are now the compiler's.

Other languages produce a SCIP index the same way, and axiom ingests any of them
(the format is language-independent):

| Language | Indexer | Command |
| --- | --- | --- |
| Java, Kotlin, Scala | [scip-java](https://sourcegraph.github.io/scip-java/) | `scip-java index --build-tool auto` |
| Rust | [rust-analyzer](https://rust-analyzer.github.io/) | `rust-analyzer scip .` |
| TypeScript, JavaScript | [scip-typescript](https://github.com/sourcegraph/scip-typescript) | `scip-typescript index` |
| Python | [scip-python](https://github.com/sourcegraph/scip-python) | `scip-python index .` |
| Go | [scip-go](https://github.com/sourcegraph/scip-go) | `scip-go` |
| C#, C/C++, Ruby, Dart | scip-dotnet, scip-clang, scip-ruby, scip-dart | see each indexer's README |

Use SCIP where a build exists and you want the graph to be exact; use the line
scan for coverage, partial trees and mid-edit code.

### 2. Generate the MCP configuration

```bash
axiom mcp-config
```

Copy the generated configuration into your AI client's settings.

For Claude Desktop (`claude_desktop_config.json`):

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

For Claude Code, Cursor and custom agents:

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

## The Eight MCP Tools

| Tool | What it does |
|---|---|
| `axiom_query_symbol` | Symbol metadata: kind, signature, docstring, hash, line range, direct dependencies. On a miss it also returns `total_symbols_in_index`, which tells a real index from an empty one |
| `axiom_get_blast_radius` | The tests that can reach a symbol, with deeper layers surveyed and reported separately |
| `axiom_eval_patch` | Compiles and runs a snippet in the symbol's own language and reports the real outcome |
| `axiom_apply_mutation` | Records a mutation to the shared, commutative operation log |
| `axiom_run_tests` | Runs the project's own test command and records the exit code as an `executed` verification |
| `axiom_record_verification` | Records a check an agent ran elsewhere, as a `reported` verification |
| `axiom_attest_commit` | Writes a sealed, chained provenance record against a check that passed |
| `axiom_search_regex` | Literal and regex text search over the scanned tree |

`declared_tools_are_dispatched.rs` pins this set, so a tool that is declared but
not dispatched fails the build rather than failing at call time.

---

## CLI Command Reference

Every subcommand constructs the same `AxiomMcpServer` the MCP tools run on, so
the CLI and the MCP server always agree, and a fix verified through one holds
for the other.

| Command | Description |
|---|---|
| `axiom serve` | Starts the MCP server over `stdio` (JSON-RPC 2.0) |
| `axiom scan --path <DIR>` | Scans and indexes a codebase into the Merkle AST store and `.axiom/index.json` |
| `axiom scan --scip <FILE> --path <DIR>` | Ingests a precise SCIP index (scip-java, rust-analyzer scip, and the rest) instead of the heuristic scan |
| `axiom search --query <STR> [--mode literal\|regex\|auto]` | Text search across the repository. Literal by default, so `.` and `(` match themselves; `--mode regex` compiles the query as a pattern, `--mode auto` picks regex only for queries that cannot be meant as literal text |
| `axiom eval --symbol <SYM> -c <CODE>` | Compiles and runs a snippet in the symbol's own language and reports the real outcome |
| `axiom blast-radius --symbol <SYM> [--depth N]` | The tests that can reach a symbol, and the percentage pruned |
| `axiom symbol --path <SYM>` | AST node metadata, signature and direct dependencies |
| `axiom cache-validate --samples <N> --depth <N>` | Breaks symbols on purpose, runs the project's own suite, and checks the blast radius selected every test that really failed |
| `axiom cache-audit --path <DIR>` | Measures what a verdict cache would decide against what the blast radius selects, without caching anything or skipping any test. See [docs/verdict_cache_audit.md](docs/verdict_cache_audit.md) |
| `axiom bench --iterations <N>` | Measures how long one Rust evaluation takes on this machine: min, median, max, mean |
| `axiom demo` | Runs an end-to-end demonstration against a seeded fixture workspace |
| `axiom swarm --agents <N> --ops <M>` | Runs the Tree-CRDT concurrency simulation |
| `axiom verify --symbol <SYM> --prompt <P> [--trusted-key K]` | Looks up the provenance record, checks the chain, and checks the signature against a signer you name |
| `axiom keygen --out <PATH>` | Generates an Ed25519 keypair for signing provenance records. Keep the private key outside any workspace you index |
| `axiom mcp-config` | Outputs ready-to-copy JSON configuration for AI IDEs |
| `axiom watch --path <DIR>` | Polls a cheap fingerprint of the tree and re-scans the tree when it changes |
| `axiom git-export` | Writes `.axiom/export.md` summarising the index and Merkle root |
| `axiom export-slsa [--symbol <SYM>] [--out <PATH>]` | Exports cryptographic provenance ledger attestations as in-toto / SLSA v1.0 statement JSON |
| `axiom git-hook [--install] [--verify]` | Installs or executes Git pre-commit cryptographic attestation provenance verification |
| `axiom dashboard` | Prints a one-shot snapshot of the workspace: symbol counts by kind, index file size, CRDT node count, Merkle root, provenance record count |

---

## Running Tests

The suite is 236 tests across 53 test binaries. 40 of those tests are the
end-to-end integration tests in `crates/axiom-cli/tests/e2e_test.rs`:

```bash
cargo test --release --all-targets
```

The gates CI runs on both ubuntu and windows are that command,
`cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and
`.github/scripts/concurrent_agents_check.py`.

CI sets `AXIOM_REQUIRE_TOOLCHAINS`, which turns a missing toolchain into a
failure rather than a skip, so a green CI run means every evaluator recipe
really executed. It is left unset locally, so a developer without `kotlinc`
still gets a green suite.

### Six of the end-to-end tests, and what each one pins

* `test_e2e_agent_full_loop_over_mcp`: full multi-language scan, symbol query, evaluator error trap, self-healing, Tree-CRDT mutation, and provenance record.
* `test_e2e_disk_persistence_cross_instance`: cross-process `.axiom/index.json` save and load, using two server instances so persistence is exercised for real.
* `test_e2e_truth_preserving_assertions`: the real compiler catching panics and invariant failures, with a verdict returned only for code that ran.
* `test_e2e_java_production_vs_test_classification`: JUnit `@Test` versus production class filtering.
* `test_e2e_dynamic_merkle_root_uniqueness`: Merkle root determinism across AST deltas.
* `test_e2e_swarm_50_agents_concurrency`: 50 agents, 2,000 operations, 0 merge conflicts, replicas converged.

---

## Security and Provenance

Three properties hold for every evaluation, in every language:

* **A confined environment.** The child's environment is cleared and only an
  allowlist of names a toolchain reads is passed through. `AXIOM_SIGNING_KEY`
  and `AXIOM_SIGNING_KEY_FILE` are never passed, so evaluated code cannot reach
  the signing key.
* **A hard deadline.** Every evaluation is bounded by a wall-clock deadline
  (`AXIOM_EVAL_TIMEOUT_SECS`, default 30) that ends the whole process tree,
  grandchildren included, so a snippet that hangs cannot hold the session.
* **Artifacts cached, verdicts never.** A repeat evaluation of byte-identical
  source under the same toolchain reuses the compiled artifact, checked
  against its stored BLAKE3 digests, and still runs it, so a failing snippet
  fails again. `AXIOM_EVAL_CACHE=off` turns the cache off.
* **A content-addressed store.** The Merkle AST store is real and
  content-addressed, so a mutation produces a new root rather than overwriting
  the old one.

WebAssembly snippets additionally run inside wasmtime with a fuel limit and no
host access. The other languages run their real compiler or interpreter as an
ordinary child process with the axiom process's own privileges, which is what
makes their verdicts authentic runs of the code; reserve those tiers for code
you trust, or set `AXIOM_EVAL_NATIVE=off` to run WebAssembly only.

### The provenance record

`axiom_attest_commit` writes a record to `.axiom/attestations.json` tying five
things together: the prompt that asked for the change, the symbol it touched,
the check that verified it, two real Merkle roots (the CRDT tree and the AST
index of the code being attested), and when it happened. Every one of those is
covered by the record's seal, so editing any of them after the fact breaks
verification. A record is only issued against a check that happened and passed.

**There are three kinds of check, and the record says which it rests on.**

| Kind | What it means | Where it comes from |
|---|---|---|
| `sandbox` | Axiom compiled and ran the code itself | `axiom_eval_patch` |
| `executed` | Axiom ran the project's own test command and saw the exit code | `axiom_run_tests` |
| `reported` | An agent ran something and told axiom the outcome | `axiom_record_verification` |

Axiom vouches for the first two and repeats the third, and `axiom verify` says
which applies, so a reader always knows exactly how much a record establishes.

`axiom_run_tests` runs under the same confined environment and process-tree
deadline every evaluation gets, bounded by `AXIOM_TEST_TIMEOUT_SECS` (default
600, separate from the evaluator's).

### Who issued it

`agent_identity` is what the caller asked to be recorded as. It is hashed into
the seal and covered by the signature, so it cannot be edited after the record
is written, and on a signed record it is bound to the key that issued it.
`axiom verify` prints it and says which case applies. Control characters and
over-long values are refused where the value enters, so a name can never add
lines of its own to what `verify` prints.

Read a record back with:

```bash
axiom verify --symbol "auth::service::validate_token" --prompt "Tighten the guard"
```

A symbol nothing was attested for, or the right symbol with a prompt that record
was not issued for, exits non-zero and says which.

### Signing

The `seal` field is a BLAKE3 digest over the record's own fields, which makes
every record tamper-evident on its own. Signing adds authorship. Generate a
keypair and point axiom at it:

```bash
axiom keygen --out ~/.config/axiom/agent.key
export AXIOM_SIGNING_KEY_FILE=~/.config/axiom/agent.key
```

Records issued after that carry an Ed25519 signature over the record's contents
together with the symbol and prompt, so a signature cannot be moved onto a
different record, and editing a stored one breaks it. A reader holding only the
public key can check who issued a record, from anywhere:

```bash
axiom verify --symbol "auth::service::validate_token" \
             --prompt "Tighten the guard" \
             --trusted-key ~/.config/axiom/agent.pub
```

Naming `--trusted-key` means a record must be signed by that key to count, and
a record signed by a different key, altered since it was written, or carrying
no signature exits non-zero and says which. Keep the private key outside any
workspace you index.

### The ledger is a chain

Each record names the seal of the record before it, and both the seal and the
signature cover that link, so removing or reordering a record is detected the
next time anyone verifies:

```
LEDGER ALTERED: chain breaks between record 0 and record 1: record 1 names
predecessor blake3_seal_0df065..., but the record before it seals as
blake3_seal_0147bb.... A record has been removed or reordered.
```

`verify` reports the chain alongside the record, and when `--trusted-key` was
given it only calls a record trusted on an intact ledger.

---

## Further Reading

| Document | What it is |
|---|---|
| [docs/axiom_speed_comparison_report.md](docs/axiom_speed_comparison_report.md) | Measured latencies, with dates, machines and the commands to reproduce them |
| [docs/verdict_cache_audit.md](docs/verdict_cache_audit.md) | The verdict-cache measurement and what it found |
| [docs/axiom_security_framework.md](docs/axiom_security_framework.md) | The containment model, layer by layer |
| [docs/USAGE_GUIDE.md](docs/USAGE_GUIDE.md) | Machine-readable tool reference for an agent |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | How the crates fit together |
| [docs/SPEC.md](docs/SPEC.md) | The specification |
| [docs/plugin_installation.md](docs/plugin_installation.md) | Installing axiom as a Claude Code plugin |

---

## License

[PolyForm Noncommercial License 1.0.0](LICENSE). Any noncommercial purpose is
permitted, which covers personal study, research, hobby projects, and use by
charities, schools, public research bodies, and government institutions, whatever
their funding. Commercial use needs a separate licence.

For commercial licensing, contact peter.isberg@deversity.se
(pricing: <https://deversity.se/pricing.html>).

Note that PolyForm Noncommercial is a source-available licence, not an open
source one under the OSI definition, because it restricts the field of use.
