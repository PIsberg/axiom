# AXIOM: The Agent-Native Autonomous Software Engine

[![CI](https://github.com/PIsberg/axiom/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/PIsberg/axiom/actions/workflows/ci.yml)
[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue)](LICENSE)
[![Commercial use: separate licence](https://img.shields.io/badge/commercial%20use-separate%20licence-orange)](https://deversity.se/pricing.html)
[![Rust 2024 edition, MSRV 1.85](https://img.shields.io/badge/rust-2024%20edition%20%7C%20MSRV%201.85-b7410e)](Cargo.toml)
[![Tested on Linux and Windows](https://img.shields.io/badge/tested-linux%20%7C%20windows-lightgrey)](.github/workflows/ci.yml)

**Source-available, not open source.** Any noncommercial use is permitted;
commercial use needs a separate licence. The badge is not the whole story and the
distinction matters, so the terms are set out once, in [License](#license).

> One binary that is both a CLI and an MCP server. It indexes a codebase into a
> symbol graph and answers an agent's questions against it: what does this symbol
> look like, which tests can reach it, does this snippet actually run, and what
> was checked before this change was recorded.

Axiom sits beside Git and CI rather than replacing them. Git still stores your
history and CI still gates your merges. What axiom removes is the round trip an
agent pays on every iteration: instead of pushing and waiting several minutes to
learn that a change broke something, it asks which tests can reach the symbol it
touched, runs a snippet in that symbol's own language, and gets an answer in
milliseconds. Nothing here commits, pushes, merges or deploys.

### What it will not do

Stated up front, because a tool an agent trusts is worse than useless when it
guesses:

* **It is not a sandbox for most languages.** WebAssembly runs under wasmtime
  with a fuel limit. Everything else runs the real compiler or interpreter with
  the axiom process's own privileges. See [Security](#security-and-provenance).
* **It does not prove a build.** The provenance record ties a prompt, a symbol
  and a check that was actually seen to pass. It is not SLSA, and nothing here
  rebuilds an artifact.
* **It refuses rather than guesses.** A language with no evaluator, a toolchain
  that is not installed, a symbol name matching several symbols, or a dependency
  closure with a hole in it produces a refusal and no verdict.
* **The line parsers are heuristics.** They need no build and they are wrong in
  the ways heuristics are wrong. Hand it a SCIP index where a build exists and
  the graph rests on the compiler's own resolution instead.

---

## Watch It Run

[![AXIOM Agent Native Engine](https://i.ytimg.com/vi/44XF6IimE1I/hqdefault.jpg)](https://www.youtube.com/watch?v=44XF6IimE1I)

**[AXIOM Agent Native Engine](https://www.youtube.com/watch?v=44XF6IimE1I)**, by
Anonymous Claudeholics

A walkthrough of the engine end to end: the repository answering queries as a
running MCP server, the blast radius narrowing a change down to the tests it can
reach, and the evaluator returning a verdict inside the same loop rather than
minutes later in CI.

**The "23 000 times faster" in the video title is not a measurement, and nothing
in this repository supports it.** It predates
[the speed report](docs/axiom_speed_comparison_report.md), which measured the
same workflow and declines to offer a multiplier at all, because how much time
test selection saves depends entirely on the suite it is applied to. Read the
report, not the title.

---

## What It Does

* **The repository answers as an MCP server.** Structural queries over JSON-RPC
  2.0 on stdio: symbol metadata, dependency edges, text search, blast radius.
  Measured on a 9,058-symbol Java tree with a warm server, across three runs, a
  symbol query returns in 0.08 to 0.13 ms and a text search in 0.27 to 0.36 ms.
* **Blast-radius test selection.** Reverse dependency reachability narrows a
  change to the tests that can reach the symbol, with deeper layers surveyed and
  reported separately so a caller can widen. On that same Java tree, 3,429 tests:
  a sample of 60 non-test symbols selected a median of 8 tests each, pruning a
  median of 99.8% of the suite, with a mean pairwise Jaccard overlap of 0.01
  between two symbols' answers. That last number is the one that matters. A
  selector returning the same tests every time would prune just as much and
  predict nothing. 53 of the 60 reached at least one test; the other 7 reached
  none, which is the honest answer for a helper nothing exercises directly and
  not a claim that changing it is safe.
* **Snippet evaluation in the symbol's own language.** Rust goes to `rustc`,
  WebAssembly to wasmtime, and Python, JavaScript, TypeScript, Go, Java, Kotlin
  and Scala to their own toolchains. A language with no evaluator, or a toolchain
  that is not installed, is refused rather than guessed at, and a snippet that
  does not terminate is killed along with its whole process tree. Measure the
  cost on your own machine with `axiom bench`; on the development machine the
  Rust median is 271 ms, which is `rustc` rather than the harness.
* **Trigram text search.** An in-memory sliding trigram index over the scanned
  tree. It is faster than `grep` only once it is warm, and the fixed cost of
  getting there takes roughly 50 queries to repay. Figures in
  [the speed report](docs/axiom_speed_comparison_report.md).
* **Concurrent agents on one workspace.** Mutations are recorded to a shared,
  commutative operation log, so agents in separate processes converge on the same
  tree whatever order their work lands in. Measured with twelve agents mutating
  at once: twelve operations recorded, none lost or duplicated, and one identical
  Merkle root across four replay orders.
* **Recorded provenance.** Every attested change ties a prompt, a symbol, the
  check that verified it, and the Merkle roots either side into a sealed, chained
  record. Issued only after the check it names has been seen to pass.

---

## Measured Latencies

Every figure in this section was measured on 2026-08-25, on one Windows machine,
release build, against two trees. Nothing here is projected. Re-run the commands
in [the speed report](docs/axiom_speed_comparison_report.md) before quoting any
of it; these numbers move with the tree and with the machine.

**Tree A**, `async-test-lib`: 898 Java source files, 9,058 indexed symbols, of
which 3,429 are tests.

| Operation | Measured | Notes |
|---|---|---|
| `axiom scan` | 3.3 s warm, 4.1 s cold | Produces a 61 MB index. Paid once, then again whenever the tree changes. |
| Server startup | 1.1 to 2.1 s | Loads that index and rebuilds the trigram index. Once per session. |
| `axiom_query_symbol`, warm | 0.08 to 0.13 ms median | In a running server, over MCP. Range across three runs. |
| `axiom_search_regex`, warm | 0.27 to 0.36 ms median | `grep -rn` over the same source tree: 80 ms. |
| `axiom_get_blast_radius`, warm | 4.7 to 8.1 ms median | Selecting 12 of 3,429 tests for one method, 99.65% pruned. |
| `axiom bench` (Rust snippet) | 271 ms median, 204 ms min | `rustc` dominates. Identical snippets are recompiled; there is no artifact cache. |
| `axiom swarm --agents 10 --ops 50` | 9.5 ms for 1,000 operations | Zero merge conflicts, replicas converged. |

**Tree B**, this repository: 55 source files, 543 indexed symbols, of which 53
are tests. `axiom scan` takes 220 ms.

The blast-radius figures for both trees come from
[`.github/scripts/blast_radius_stats.py`](.github/scripts/blast_radius_stats.py),
which asks the shipped CLI about each non-test symbol in turn. Its output on this
repository, at depth 1:

```text
suite             53 tests
non-test symbols  490
reach >= 1 test   103 of 490 asked
tests selected    mean 10.1, median 4, max 31
pruned            mean 81.0%, median 92.5%
mean Jaccard      0.11
```

Read the two trees together, because they say different things. On a 3,429-test
Java suite the selection is worth real time. On this repository, 53 tests deep,
it prunes a median of 92.5% and the absolute saving is seconds. **The value of
test selection scales with the suite it is applied to, and on a small suite it
does not repay the index.** That is the honest shape of the feature, and it is
why no multiplier is quoted anywhere in this repository.

---

## Where the Time Goes

![Axiom versus Git plus CI across five workflow stages](docs/images/axiom_speed_comparison.png)

The figure charts five stages of an agent's edit loop against the same loop on
Git plus a CI pipeline: workspace setup, symbol search, incremental compilation,
the test round trip, and resolving concurrent edits.

**Two of those five stages are design rather than code, and the figure does not
say so.** Reuse of already-compiled artifacts on an identical AST hash is not
built: the content-addressed store has the functions for it and nothing calls
them, so running the same snippet twice compiles it twice. The microVM tier the
sandbox stage implies is not built either. The three stages that are real are
workspace setup, symbol search and the test round trip, and those are the ones
measured in the table above.

The numbers behind the chart, the baselines they are measured against, and a list
of what earlier versions of this document claimed and got wrong, are in
[docs/axiom_speed_comparison_report.md](docs/axiom_speed_comparison_report.md).

---

## Prerequisites and System Requirements

* **Rust**: `1.85` or newer. The workspace is on the 2024 edition, which was
  stabilised in 1.85; an older toolchain fails naming the version rather than the
  syntax.
* **Optional, per language you want `axiom_eval_patch` to run**: `python3` or
  `python`, `node`, `deno` or `tsc`, `go`, a JDK for `javac` and `java`,
  `kotlinc`, `scala`. Each is looked for on `PATH` when it is first needed; a
  missing one produces `EVALUATOR_UNAVAILABLE` naming it, never a verdict.
  `AXIOM_EVAL_NATIVE=off` refuses to run any of them.
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
project, not this repository. A server that appears to know nothing about your
code is usually one started somewhere with no `.axiom/index.json` above it.

#### Precise indexing with SCIP, recommended where a build exists

`axiom scan` without a SCIP index uses fast, build-free line parsers. They are
heuristics: they infer a symbol's owner and its callers from the shape of the
text, and they are wrong in the ways a heuristic is wrong. If your project has a
build, you can hand axiom a **SCIP** index instead, produced by the language's
own indexer running the real compiler, and the symbol graph, and the blast radius
over it, rest on resolved references rather than text matches.

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
tool and CLI command works against it unchanged; `axiom_get_blast_radius` is the
one that gains the most, since its edges are now the compiler's, not a guess.

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

The trade-off is the point: a SCIP index is slower to produce and needs a
buildable project, where the line scan is instant and needs nothing. Use SCIP
where a build exists and you want the graph to be exact; fall back to the scan
for coverage, partial trees and mid-edit code.

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
| `axiom_query_symbol` | Symbol metadata: kind, signature, docstring, hash, line range, direct dependencies. On a miss it also returns `total_symbols_in_index`, which is how you tell a real index from an empty one |
| `axiom_get_blast_radius` | The tests that can reach a symbol, with deeper layers surveyed and reported separately |
| `axiom_eval_patch` | Compiles and runs a snippet in the symbol's own language, or refuses |
| `axiom_apply_mutation` | Records a mutation to the shared, commutative operation log |
| `axiom_run_tests` | Runs the project's own test command and records the exit code as an `executed` verification |
| `axiom_record_verification` | Records a check an agent ran elsewhere, as a `reported` verification |
| `axiom_attest_commit` | Writes a sealed, chained provenance record against a check that passed |
| `axiom_search_regex` | Literal and regex text search over the scanned tree |

`declared_tools_are_dispatched.rs` pins this set, so a tool that is declared but
not dispatched fails the build rather than failing at call time.

---

## CLI Command Reference

Every subcommand constructs the same `AxiomMcpServer` the MCP tools run on, so a
bug reproduced through the CLI is the bug an agent sees.

| Command | Description |
|---|---|
| `axiom serve` | Starts the MCP server over `stdio` (JSON-RPC 2.0) |
| `axiom scan --path <DIR>` | Scans and indexes a codebase into the Merkle AST store and `.axiom/index.json` |
| `axiom scan --scip <FILE> --path <DIR>` | Ingests a precise SCIP index (scip-java, rust-analyzer scip, and the rest) instead of the heuristic scan |
| `axiom search --query <STR> [--mode literal\|regex\|auto]` | Text search across the repository. Literal by default, so `.` and `(` match themselves; `--mode regex` compiles the query as a pattern, `--mode auto` picks regex only for queries that cannot be meant as literal text |
| `axiom eval --symbol <SYM> -c <CODE>` | Compiles and runs a snippet in the symbol's own language, or refuses. Not a sandbox outside WebAssembly |
| `axiom blast-radius --symbol <SYM> [--depth N]` | The tests that can reach a symbol, and the percentage pruned |
| `axiom symbol --path <SYM>` | AST node metadata, signature and direct dependencies |
| `axiom cache-validate --samples <N> --depth <N>` | Breaks symbols on purpose, runs the project's own suite, and checks the blast radius selected every test that really failed. This is the check that can find a missing edge; the audit below cannot |
| `axiom cache-audit --path <DIR>` | Measures what a verdict cache would decide against what the blast radius selects, without caching anything or skipping any test. See [docs/verdict_cache_audit.md](docs/verdict_cache_audit.md); on this repository it currently says do not build it |
| `axiom bench --iterations <N>` | Measures how long one Rust evaluation takes on this machine: min, median, max, mean |
| `axiom demo` | Runs an end-to-end demonstration against a seeded fixture workspace, not against your code |
| `axiom swarm --agents <N> --ops <M>` | Runs the Tree-CRDT concurrency simulation |
| `axiom verify --symbol <SYM> --prompt <P> [--trusted-key K]` | Looks up the provenance record, checks the chain, and checks the signature against a signer you name |
| `axiom keygen --out <PATH>` | Generates an Ed25519 keypair for signing provenance records. Keep the private key outside any workspace you index |
| `axiom mcp-config` | Outputs ready-to-copy JSON configuration for AI IDEs |
| `axiom watch --path <DIR>` | Polls a cheap fingerprint of the tree and re-scans the whole tree when it changes. The re-index is a full re-parse, not an incremental one |
| `axiom git-export` | Writes `.axiom/export.md` summarising the index and Merkle root. It does not touch git |
| `axiom export-slsa [--symbol <SYM>] [--out <PATH>]` | Exports cryptographic provenance ledger attestations as in-toto / SLSA v1.0 statement JSON |
| `axiom git-hook [--install] [--verify]` | Installs or executes Git pre-commit cryptographic attestation provenance verification |
| `axiom dashboard` | Prints a one-shot snapshot of the workspace: symbol counts by kind, index file size, CRDT node count, Merkle root, provenance record count. Not a TUI and not a live feed |

`axiom demo` seeds a fixture workspace on purpose. An earlier version seeded that
fixture into any empty index, which made a workspace nobody had scanned answer
confidently about a symbol in no real codebase. A server with no index above it
now answers nothing.

---

## Running Tests

The suite is 208 tests across 43 test binaries. 40 of those tests are the
end-to-end integration tests in `crates/axiom-cli/tests/e2e_test.rs`:

```bash
cargo test --release --all-targets
```

The gates CI runs on both ubuntu and windows are that command,
`cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and
`.github/scripts/concurrent_agents_check.py`. The lint job fails on a single
warning.

CI sets `AXIOM_REQUIRE_TOOLCHAINS`, which turns a missing toolchain into a
failure rather than a skip. Every evaluator test branches on whether a toolchain
is installed and both branches pass, so without that variable a runner where an
install step silently did nothing would run no recipe and report success. It is
left unset locally, because a developer without `kotlinc` should not get a red
suite.

### Six of the end-to-end tests, and what each one pins

* `test_e2e_agent_full_loop_over_mcp`: full multi-language scan, symbol query, evaluator error trap, self-healing, Tree-CRDT mutation, and provenance record.
* `test_e2e_disk_persistence_cross_instance`: cross-process `.axiom/index.json` save and load. An in-process scan-then-query test passes with persistence completely broken, which is why this one uses two instances.
* `test_e2e_truth_preserving_assertions`: the real compiler catching panics and invariant failures, with no verdict returned for code that did not run.
* `test_e2e_java_production_vs_test_classification`: JUnit `@Test` versus production class filtering.
* `test_e2e_dynamic_merkle_root_uniqueness`: Merkle root determinism across AST deltas.
* `test_e2e_swarm_50_agents_concurrency`: 50 agents, 2,000 operations, 0 merge conflicts, replicas converged.

---

## Security and Provenance

![The three concentric containment layers around an agent's workspace](docs/images/axiom_security_architecture.png)

**The figure above is the containment model the design aims at, and two of its
three layers are not built.** It is kept because it says where this is going, not
because it describes what runs today.
[docs/axiom_security_framework.md](docs/axiom_security_framework.md) goes through
it layer by layer.

What is actually built:

* A WAT or wasm snippet runs in wasmtime with a fuel limit and no host access.
  That one is a sandbox.
* **Every other language runs in tier 2, which is the real compiler or
  interpreter with the axiom process's own privileges.** There is no intercepting
  proxy, no network-egress restriction, and no CPU or memory bound. Do not put
  untrusted snippets through it. `AXIOM_EVAL_NATIVE=off` refuses tier 2 entirely.
* Two things tier 2 does enforce. Every evaluation gets a confined environment:
  the child's environment is cleared and only an allowlist of names a toolchain
  reads is passed through, with `AXIOM_SIGNING_KEY` and `AXIOM_SIGNING_KEY_FILE`
  refused even there. Before that, a snippet read the signing key straight out of
  `os.environ` and the value came back in the report. And every evaluation is
  bounded by a wall-clock deadline (`AXIOM_EVAL_TIMEOUT_SECS`, default 30) that
  kills the whole process tree rather than just the child, because `go run`, the
  `kotlin` launcher and a `Popen` from a Python snippet all outlived a kill aimed
  at the child alone.
* The Merkle AST store is real and content-addressed, so a mutation produces a
  new root rather than overwriting the old one.

### The provenance record

`axiom_attest_commit` writes a record to `.axiom/attestations.json` tying five
things together: the prompt that asked for the change, the symbol it touched, the
check that verified it, two real Merkle roots (the CRDT tree and the AST index of
the code being attested), and when it happened. Every one of those is covered by
the record's seal, so editing any of them after the fact breaks verification.

A record is only issued against a check that happened and passed. Naming a check
the server has no record of, or one that failed, is refused.

**There are three kinds of check, and the record says which it rests on.**

| Kind | What it means | Where it comes from |
|---|---|---|
| `sandbox` | Axiom compiled and ran the code itself | `axiom_eval_patch` |
| `executed` | Axiom ran the project's own test command and saw the exit code | `axiom_run_tests` |
| `reported` | An agent ran something and told axiom the outcome | `axiom_record_verification` |

Axiom vouches for the first two and is repeating the third. `axiom verify` says
which applies:

```
Checked by:    reported (mvn -pl async-test-lib test -Dtest=ConcurrencyRunnerTest)

Axiom did not run this check. The outcome above was reported by
the agent that asked for the record.
```

`axiom_run_tests` runs under the same confined environment and process-tree kill
every evaluation gets, so a test command cannot read the signing key, and it is
bounded by `AXIOM_TEST_TIMEOUT_SECS` (default 600, separate from the evaluator's).

### Who issued it

`agent_identity` is what the caller asked to be recorded as. Axiom stores it
without checking it, so by itself it is a claim rather than an answer.

It is taken as an argument, and it is hashed into the seal and covered by the
signature. That is what makes it worth having: it cannot be edited after the
record is written, and on a signed record it is bound to the key that issued it.
`axiom verify` prints it and says which of those cases applies, so an unsigned
name is never shown as though something had established it. A record whose caller
named nobody reads `unattributed`. Control characters and over-long values are
refused where the value enters, because a name carrying a newline could otherwise
add a line of its own to what `verify` prints.

Read a record back with:

```bash
axiom verify --symbol "auth::service::validate_token" --prompt "Tighten the guard"
```

A symbol nothing was attested for, or the right symbol with a prompt that record
was not issued for, exits non-zero and says which. The seal is recomputed from
the record's stored fields together with the symbol and prompt being claimed, so
a failure to re-derive means either the prompt is not the one the record was
issued for or a stored field has been edited since. There is no
prompt-independent copy to compare against, so `verify` names both causes rather
than picking one.

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
axiom verify --symbol "auth::service::validate_token" \
             --prompt "Tighten the guard" \
             --trusted-key ~/.config/axiom/agent.pub
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

**None of this is a reproducible-build attestation.** Nothing here rebuilds your
artifact independently or establishes that a build was hermetic, so it is not
SLSA in any sense. It records that a particular prompt, symbol and check were
seen together on one machine, which is worth having and is a smaller claim than
the vocabulary around it suggests.

---

## Further Reading

| Document | What it is |
|---|---|
| [docs/axiom_speed_comparison_report.md](docs/axiom_speed_comparison_report.md) | Measured latencies, and what earlier claims got wrong |
| [docs/verdict_cache_audit.md](docs/verdict_cache_audit.md) | Why the verdict cache is measured and not built |
| [docs/axiom_security_framework.md](docs/axiom_security_framework.md) | The containment model, and which layers exist |
| [docs/USAGE_GUIDE.md](docs/USAGE_GUIDE.md) | Machine-readable tool reference for an agent |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | How the crates fit together, and which parts are design |
| [docs/SPEC.md](docs/SPEC.md) | The specification. Describes the target, not the build |
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
