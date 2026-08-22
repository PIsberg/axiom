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
cargo test                            # 73 tests across e2e, mcp, crdt, persistence, blast radius, eval, cache audit
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

**Dependency resolution has three mechanisms for Java and a fourth for everything else, and
precision trades against recall.** Java edges come from imports, from same-package and
fully-qualified references, and from accessor return-type inference (a test calling
`ctx.sharedRaceConditionDetector()` never names the type). Comments and string literals are stripped
first, because matching raw file text turned every javadoc mention into a dependency.
`test_e2e_comment_stripping_and_class_literal_dependencies`,
`test_e2e_same_package_dependencies_blast_radius` and
`test_e2e_accessor_return_type_dependency_resolution` pin the three. Widening any of them re-admits
comment noise; narrowing drops real dependents. Judge a change by measuring both directions against
a real repository, not by the size of the result set.

Rust, Python, TypeScript, JavaScript and Go had none of that: their parsers recorded each file's
`use`/`import` lines verbatim as every node's dependencies, so `reverse_deps` was keyed by strings
like `anyhow::Result` and nothing ever resolved to an indexed symbol. `record_references` and
`resolve_reference_edges` supply the missing pass: references are collected per line while parsing,
then, once the whole tree is read, each is charged to the last symbol declared above it and kept
only if some indexed symbol answers to that name. The pass runs at the end of `scan_directory`
because a file that references a symbol defined further down the walk cannot be resolved when it is
read. `crates/axiom-ast/tests/blast_radius.rs` pins it.

**A symbol's short name is not its last dot-separated segment.** `simple_name_of` distinguishes a
package-keyed symbol, `pkg.Class::method` to `Class`, from a file-keyed one,
`src/lib.rs::write_atomically` to `write_atomically`. Splitting on the last dot unconditionally took
the file extension for a package separator: every Rust symbol reduced to `rs`, and since
`index_node` stores the symbol path in `signature`, the fallback search matched `rs::` against every
Rust symbol in the index. The blast radius for anything in this repository was all 49 tests. A
Java-only fixture suite cannot catch this, which is why the new tests scan Rust and Python side by
side.

**`reverse_deps` is keyed by the name a caller writes, and its values are full symbol paths.** The
traversal therefore has to look up both on each hop. Looking up only the path found nothing after
the first step, so every transitive layer was silently empty for the file-keyed languages.

**`axiom_eval_patch` must never return a verdict it did not earn.** `execute_eval_in` in
`axiom-vmm` picks a tier from the extension of the file the symbol was indexed from: a WAT or wasm
snippet goes to wasmtime Cranelift; Rust is written to a temp `.rs` and compiled with `rustc`; and
everything the table in `axiom-vmm/src/native.rs` knows about goes to that language's own toolchain.
Anything else, a toolchain that is not on `PATH`, a name matching several symbols, or a temp
directory that cannot be written, is `EvaluatorUnavailable` with `passed_checks_count: 0`, never
`PASSED`. An earlier version fell back to matching assertion substrings and reported success for
code that never executed, which is the worst available failure for a tool an agent trusts.
`test_e2e_truth_preserving_assertions` and `crates/axiom-cli/tests/multi_language_eval.rs` guard it.

Two harness details that have already produced wrong answers. A Rust snippet without `fn main` is
wrapped in one, with a `validate_token` helper injected. And Java runs under `java -ea`: without it
every `assert` is a no-op, so a false assertion exits zero and reports `PASSED`, which is why
`a_java_assertion_is_checked_with_assertions_enabled` asserts on the failing case rather than the
passing one.

**Tier 2 is not a sandbox, and the docs must not say it is.** It runs the real compiler or
interpreter with the process's own privileges, as the `rustc` tier always did. `AXIOM_EVAL_NATIVE=off`
refuses it, and `AXIOM_EVAL_TIMEOUT_SECS` (default 30) bounds every command, because before that a
snippet that did not terminate held the stdio pipe an agent was blocked on.

**Language is resolved through the symbol, not the caller's spelling.** `language_of_symbol`
resolves the name first, because comparing the caller's spelling against the stored keys returned
`None` for every short name, and `None` meant Rust. An ambiguous name is refused with
`AmbiguousSymbol` and its candidates rather than compiled as whichever language won.

**The verdict cache is measured, not built, and the measurement says do not build it.**
`axiom cache-audit` reads the same graph in the forward direction, from a test to what
it depends on, and compares that against what the blast radius selects. Nothing is
cached and no test is skipped. On this repository 0 of 52 tests produce a usable key,
and 322 symbol/test pairs disagree in the direction that would skip a test the selector
says must run. Two causes, needing different fixes: names from crates outside the tree
(`anyhow::Result`, `std::path::{Path, PathBuf}`) belong in the key as a toolchain and
lockfile digest rather than counting as gaps, while ambiguous short names (`new`,
`write`, 51 and 48 occurrences) cannot be resolved without type information the
line-based parsers do not have. `closure_hash` returns `Option` for this reason: an
incomplete closure must produce no key at all, because a cache that keys on a partial
view skips a test whose real dependency moved and reports a pass for code that never
ran. Full reasoning in `docs/verdict_cache_audit.md`. Re-run the audit before quoting
any of these numbers; they move with the graph.

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

The gates are `cargo test --release --all-targets`, `cargo fmt --all --check` and
`cargo clippy --all-targets -- -D warnings`, and `.github/workflows/ci.yml` runs all three plus
`.github/scripts/concurrent_agents_check.py` on ubuntu and windows. Run them before opening a PR;
the lint job fails the build on a single warning.

`README.md` mixes measurements with design goals. The blast-radius and eval numbers were taken on
this repository and say so; treat anything quoting a figure for other repositories as a target.

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
