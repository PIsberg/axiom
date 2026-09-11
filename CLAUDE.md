# CLAUDE.md

## What this is

A Rust workspace of six crates that build one binary, `axiom`, which is simultaneously a CLI and
an MCP server speaking JSON-RPC 2.0 over stdio. It indexes a target codebase into an in-memory
symbol graph, persists that graph to `.axiom/index.json`, and answers agent queries against it:
symbol lookup, blast-radius test selection, sandboxed snippet evaluation, CRDT mutation, and
Ed25519 attestation.

The consumer is an autonomous agent, not a human. Every tool response is JSON an agent will act on
without checking, which is why the invariants below are about never returning a confident wrong
answer rather than about coverage.

## Build and test

```bash
cargo build --release --bin axiom     # Windows needs the MSVC env loaded first, see below
cargo test --release --all-targets    # 287 tests over 64 binaries; 284 run on Windows, see README
cargo test --test e2e_test            # one test file
cargo test test_e2e_same_package      # one test by name substring
```

The binary is at `target/release/axiom`, for whatever the host target is. A pin to
`x86_64-pc-windows-msvc` used to live in `.cargo/config.toml`; it put the binary somewhere else
and made `cargo build` fail on any machine that is not Windows, which CI caught on its first run.

On Windows the C toolchain must be on the path before cargo runs, or `zstd-sys`,
`wasmtime-internal-fiber` and `ittapi-sys` fail inside their build scripts:

```powershell
cmd.exe /c "`"C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools\VC\Auxiliary\Build\vcvars64.bat`" && cargo build --release --bin axiom"
```

Some sandboxed environments deny file creation to processes cargo spawns, which breaks those same
build scripts with `Os { code: 5, PermissionDenied }` before any of this crate's own code
compiles. It is not a toolchain problem and switching to `x86_64-pc-windows-gnu` does not help.
Point cargo at a writable directory instead: `CARGO_TARGET_DIR=<writable-dir> cargo test --release`.

**Never read a build or test result through a pipe.** `cargo test | tail` reports the status of
`tail`, so a failed build looks green. Capture the exit code directly, or redirect to a file and
check it.

Driving the server by hand is often faster than writing a test:

```bash
printf '%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"p","version":"1"}}}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"axiom_query_symbol","arguments":{"symbol_path":"pkg.Class"}}}' \
 | ./target/release/axiom serve
```

## Gates

`cargo test --release --all-targets`, `cargo fmt --all --check`, and
`cargo clippy --all-targets -- -D warnings`. `.github/workflows/ci.yml` runs all three plus
`.github/scripts/concurrent_agents_check.py` on ubuntu and windows. Run them before opening a PR;
the lint job fails the build on a single warning.

CI sets `AXIOM_REQUIRE_TOOLCHAINS=1` and raises `AXIOM_EVAL_TIMEOUT_SECS` to 300. Both matter: see
`crates/axiom-vmm/CLAUDE.md`.

## Architecture

Dependencies run one way. `axiom-proto` is the leaf; nothing depends on `axiom-cli`.

```
axiom-proto ──► everything     wire types only: AstNode, CtopReport, ProvenanceAttestation
axiom-ast   ──► core, crdt     the indexer: parsers, symbol graph, blast radius, Zoekt, disk I/O
axiom-vmm   ──► core           the evaluator: wasmtime (a sandbox) and native toolchains (not)
axiom-crdt  ──► core           Tree-CRDT plus swarm simulation
axiom-core  ──► cli            the MCP server: tool schemas and dispatch
axiom-cli                      clap subcommands, all of which drive AxiomMcpServer
```

Source is 14,700 lines over 13 files as of 2026-09-11. Four files hold most of it:
`axiom-ast/src/lib.rs` (4,946), `axiom-core/src/mcp.rs` (2,048), `axiom-vmm/src/native.rs` (2,029),
`axiom-cli/src/main.rs` (1,991).

The CLI is not a separate code path. Every subcommand constructs an `AxiomMcpServer` and calls the
same crates the MCP tools use, so a bug reproduced through `axiom blast-radius` is the same bug an
agent sees through `axiom_get_blast_radius`.

**The agent-visible surface**, all declared and dispatched in `axiom-core/src/mcp.rs`:

- 8 tools: `axiom_query_symbol`, `axiom_get_blast_radius`, `axiom_eval_patch`,
  `axiom_apply_mutation`, `axiom_attest_commit`, `axiom_record_verification`,
  `axiom_search_regex`, `axiom_run_tests`.
- 3 prompts: `axiom_review_patch`, `axiom_targeted_refactor`, `axiom_attest_task`.
- 3 resources: `axiom://symbols`, `axiom://ledger`, `axiom://fixes`.

**Languages.** `parse_by_language` in axiom-ast indexes Java (shared with Kotlin and Scala), Rust,
Python, TypeScript/JavaScript, Go, and C/C++, dispatching through `AstIndex::PARSERS`. `LANGUAGES`
in axiom-vmm decides what can be evaluated, and every indexed language now has a recipe except
Rust, which belongs to tier 1. `scip_ingest.rs` is the precise alternative to the line parsers, reading a SCIP index
produced by a language's own indexer.

## The ideas the whole thing rests on

Each is one line here and argued where it is enforced. Follow the pointer before changing code
that touches one.

1. **A refusal beats an unearned verdict.** `EvaluatorUnavailable` with `passed_checks_count: 0`,
   `AmbiguousSymbol` with its candidates, `closure_hash` returning `Option`, a cache-validate run
   where nothing failed reported as establishing nothing. An answer that says nothing is
   established is true either way; a wrong one is acted on. → `axiom-vmm/CLAUDE.md`

2. **A non-zero exit is not a verdict, and neither is a green test that ran nothing.** A failed
   compiler download and a skipped branch both produce a status with no execution behind it.
   → `axiom-vmm/CLAUDE.md`

3. **A guardrail must derive its list from the thing it guards, never mirror it.** Every pin in
   this repository that survived reads the code: `declared_tools_are_dispatched.rs` calls
   `tools/list`, `docs_name_every_tool.rs` compares the docs against it,
   `readme_lists_every_subcommand.rs` reads clap, `every_language_has_a_toolchain...` iterates
   `native::languages()`, `every_indexed_language_has_an_evaluator` reads
   `AstIndex::indexed_extensions`, and `docs_quote_the_real_numbers.rs` counts the suite rather
   than trusting the README's count of it. A guard that copies the list it guards drifts silently
   and stays green.
   → `axiom-core/CLAUDE.md`, `axiom-vmm/CLAUDE.md`

4. **Agreement between two readings of one graph is not evidence about the code.** `cache-audit`
   walks forward and backward over the same edges, so a call the parsers never recorded is
   invisible to both together and reports as a zero. Only `cache-validate`, which breaks a symbol
   and runs the real suite, produces evidence. → `axiom-cli/CLAUDE.md`

5. **Precision and safety want opposite biases from one graph.** For test selection a wrong extra
   edge costs one test run; for a cache key a missing edge skips a test and reports a pass for
   code that never ran. Behind the selector, the saving a cache offers and its unsafety are the
   same number. → `axiom-ast/CLAUDE.md`, `axiom-cli/CLAUDE.md`

6. **Everything stored must be covered by the seal; everything caller-set and displayed must be
   sanitised where it enters.** A new field on `ProvenanceAttestation` that is not in `seal_over`
   is forgeable. → `axiom-proto/CLAUDE.md`, `axiom-core/CLAUDE.md`

7. **Keys are portable or the index is machine-local.** Symbol keys are relative to the scan root,
   which is what makes the index committable and a Merkle root comparable across machines.
   → `axiom-ast/CLAUDE.md`

8. **The parsers are line-based heuristics and fail in known ways.** Comments and string literals
   are stripped before a declaration is decided; the stripper must never move a line. Adding a
   language or loosening a match re-admits a documented failure. → `axiom-ast/CLAUDE.md`

9. **Failures stay loud.** Persistence errors propagate rather than printing a success banner.
   A skipped gate is reported as skipped, not passed. → `axiom-ast/CLAUDE.md`

## Where the reasoning lives

Per-crate context files, loaded when you work in that crate:

| File | Covers |
| --- | --- |
| `crates/axiom-ast/CLAUDE.md` | parsers, symbol keys, dependency edges, SCIP, persistence |
| `crates/axiom-vmm/CLAUDE.md` | tiers, verdicts, toolchains, confinement, artifact cache |
| `crates/axiom-core/CLAUDE.md` | MCP surface, index discovery, seeding, doc drift |
| `crates/axiom-proto/CLAUDE.md` | the seal and what it covers |
| `crates/axiom-cli/CLAUDE.md` | cache-validate versus cache-audit, in-place mutation |

Longer documents: `docs/verdict_cache_audit.md` (why the verdict cache is measured and not built),
`docs/ARCHITECTURE.md`, `docs/USAGE_GUIDE.md`.

`README.md` mixes measurements with design goals. The blast-radius and eval numbers were taken on
this repository and say so; treat anything quoting a figure for other repositories as a target.
Every measured figure carries the date it was taken, and figures move with the suite: re-run
`axiom cache-audit` and `.github/scripts/blast_radius_stats.py` before quoting one.
