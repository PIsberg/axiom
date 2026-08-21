# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Rust workspace of six crates that build **one binary**, `axiom`, which is simultaneously a CLI and
an MCP server speaking JSON-RPC 2.0 over stdio. It indexes a target codebase into an in-memory
symbol graph, persists that graph to `.axiom/index.json`, and answers agent queries against it:
symbol lookup, blast-radius test selection, sandboxed snippet evaluation, CRDT mutation, and Ed25519
attestation.

The consumer is an autonomous agent, not a human. Every tool response is JSON an agent will act on
without checking, which is why the invariants below are about *never returning a confident wrong
answer* rather than about coverage.

## Build and test

```bash
cargo build --release --bin axiom     # Windows needs the MSVC env loaded first, see below
cargo test                            # 18 tests: 10 e2e, 5 mcp, 3 crdt
cargo test --test e2e_test            # one test file
cargo test test_e2e_same_package      # one test by name substring
```

The binary is at `target/release/axiom`, for whatever the host target is. A pin to
`x86_64-pc-windows-msvc` used to live in `.cargo/config.toml`; it put the binary somewhere else and
made `cargo build` fail on any machine that is not Windows, which CI caught on its first run.

On Windows the C toolchain must be on the path before cargo runs, or `zstd-sys`, `wasmtime-fiber`
and `ittapi-sys` fail inside their build scripts:

```powershell
cmd.exe /c "`"C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools\VC\Auxiliary\Build\vcvars64.bat`" && cargo build --release --bin axiom"
```

Driving the server by hand is often faster than writing a test. It is a stdio program, so piping
JSON-RPC lines into `axiom serve` gives a full session:

```bash
printf '%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"p","version":"1"}}}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"axiom_query_symbol","arguments":{"symbol_path":"pkg.Class"}}}' \
 | ./target/release/axiom serve
```

## Architecture

Dependencies run one way. `axiom-proto` is the leaf; nothing depends on `axiom-cli`.

```
axiom-proto ──► everything     wire types only: AstNode, CtopReport, ProvenanceAttestation
axiom-ast   ──► core, crdt     the indexer: parsers, symbol graph, blast radius, Zoekt, disk I/O
axiom-vmm   ──► core           the sandbox: wasmtime and rustc tiers
axiom-crdt  ──► core           Tree-CRDT plus swarm simulation
axiom-core  ──► cli            the MCP server: tool schemas and dispatch
axiom-cli                      clap subcommands, all of which drive AxiomMcpServer
```

`axiom-ast/src/lib.rs` is the bulk of the system (~1000 lines) and holds several indexes that must
stay in agreement: `nodes` (symbol to AstNode), `reverse_deps` (symbol to dependents), and the
supporting `method_return_types` and `clean_file_texts` maps behind accessor inference. Anything
that inserts into one usually has to update the others.

The CLI is not a separate code path. Every subcommand constructs an `AxiomMcpServer` and calls the
same crates the MCP tools use, so a bug reproduced through `axiom blast-radius` is the same bug an
agent sees through `axiom_get_blast_radius`.

Six MCP tools, all declared and dispatched in `axiom-core/src/mcp.rs`: `axiom_query_symbol`,
`axiom_get_blast_radius`, `axiom_eval_patch`, `axiom_apply_mutation`, `axiom_attest_commit`,
`axiom_search_regex`. The tool list in `handle_request` and the dispatch `match` below it are two
places that must be edited together; a tool declared but not dispatched fails at call time, not at
startup.

Language dispatch is by file extension in `parse_file_content`: Java (shared with Kotlin and Scala),
Rust, Python, TypeScript/JavaScript, and Go each have their own line parser.

## Things that are easy to get wrong

**Index discovery walks up from the current directory.** `find_index_file` in `mcp.rs` climbs parent
directories looking for `.axiom/index.json`. The MCP server inherits its client's working directory,
which is the agent's project, not this repo. A server that appears to know nothing about the
codebase is usually one started somewhere with no index above it.

**An empty index seeds two demo nodes.** `AxiomMcpServer::new` inserts
`auth::service::validate_token` and `test_auth_validation` when the index is empty, which makes a
fresh server *look* functional. When verifying real behaviour, read `total_symbols_in_index` in the
response: a 2 means you are talking to the seed, not to a scanned repository.

**The parsers are line-based heuristics, not ASTs.** `parse_java_content` and its siblings walk
lines and match on shape. That approach has already produced: javadoc lines containing the words
"the class Javadoc" hijacking the enclosing class name; methods filed under the last nested type
because brace depth was not tracked; wrapped parameter lists dropping a method entirely; `new
Foo(...)` sites and `catch` clauses indexed as methods; and the `current_class`-empty fallback
writing machine-absolute file paths into symbol names. Each is now pinned by a test. When adding a
language or loosening a match, assume those failure modes are one edit away, and check the resulting
index for symbols whose owner is not a valid identifier.

**Dependency resolution has three mechanisms, and precision trades against recall.** Edges come from
imports, from same-package and fully-qualified references, and from accessor return-type inference
(a test calling `ctx.sharedRaceConditionDetector()` never names the type). Comments and string
literals are stripped first, because matching raw file text turned every javadoc mention into a
dependency. `test_e2e_comment_stripping_and_class_literal_dependencies`,
`test_e2e_same_package_dependencies_blast_radius` and
`test_e2e_accessor_return_type_dependency_resolution` pin the three. Widening any of them re-admits
comment noise; narrowing drops real dependents. Judge a change by measuring both directions against
a real repository, not by the size of the result set.

**`axiom_eval_patch` must never return a verdict it did not earn.** Three tiers in `execute_eval`: a
WAT or wasm snippet compiles and runs through wasmtime Cranelift; anything else is written to a temp
`.rs` and compiled with `rustc`; and if the temp write or `rustc` fails, the answer is
`EvaluatorUnavailable` with `passed_checks_count: 0`, never `PASSED`. An earlier version fell back
to matching assertion substrings and reported success for code that never executed, which is the
worst available failure for a tool an agent trusts. `test_e2e_truth_preserving_assertions` guards
it. Note the harness: a snippet without `fn main` is wrapped in one, with a `validate_token` helper
injected so snippets can call it.

**Persistence failures must stay loud.** `save_to_disk` returns the path it wrote and verifies the
file exists, and callers propagate the error instead of discarding it. When these were `let _ = ...`
under an unconditional success banner, `scan` printed "Saved to .axiom/index.json" while writing
nothing and the server then served an empty index. `test_e2e_disk_persistence_cross_instance` writes
in one instance and reads in another, which is the only shape that catches it: an in-process
scan-then-query test passes with persistence completely broken.

**The Merkle root that `scan` prints comes from the CRDT tree, not the AST index.** Keep the two
apart when reading output. `test_e2e_dynamic_merkle_root_uniqueness` pins that the root actually
varies with scanned content.

**Retries are for errors that can clear, and the set differs by platform.**
Windows fails a rename or an exclusive create with a sharing violation while
another process holds the file open, surfacing as `PermissionDenied`, and it
clears when that handle closes. Unix has no such rule: a rename succeeds with
readers attached, and `EACCES` means the directory is not writable, which waiting
will not change. `worth_retrying` in `axiom-ast` encodes that difference, and
everything outside it is treated as final. Retrying a full disk or a
cross-device rename only delays an accurate error, and retrying `EACCES` on Unix
turns an immediate report into a thirty-second pause followed by the same report.

**All measurement in this repository so far is from Windows.** The concurrency
numbers quoted in commit messages, and the sharing-violation behaviour the retry
loops exist for, were observed there. The retries are written to be correct on
Unix rather than merely harmless, but that has been reasoned rather than run.

## Repository state

There is no git repository here (`git status` fails) and no CI. `cargo test` is the only gate, so it
is the whole verification story before and after a change.

`README.md` describes the intended product and its performance claims; treat its numbers as targets
rather than measurements. It also lists six e2e tests where the suite now has ten.

## Building in a sandboxed session

Some sandboxed environments deny file creation to processes cargo spawns, which breaks build scripts
(`target-lexicon`, `zstd-sys`, `wasmtime-fiber`, `ittapi-sys`) with `Os { code: 5, PermissionDenied }`
before any of this crate's own code compiles. It is not a toolchain problem and switching to the
`x86_64-pc-windows-gnu` target does not help. Point cargo at a writable directory instead:

```bash
CARGO_TARGET_DIR=<writable-dir> cargo test --release
```

Never read a build or test result through a pipe: `cargo test | tail` reports the status of `tail`,
so a failed build looks green. Capture the exit code directly, or redirect to a file and check `$?`.
