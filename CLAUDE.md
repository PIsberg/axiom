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
cargo test                            # 105 tests across e2e, mcp, crdt, persistence, blast radius, eval, cache audit
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

`axiom-ast/src/lib.rs` is the bulk of the system (~2,900 lines) and holds several indexes that must
stay in agreement: `nodes` (symbol to AstNode), `reverse_deps` (symbol to dependents), and the
supporting `method_return_types` and `clean_file_texts` maps behind accessor inference. Anything
that inserts into one usually has to update the others.

The CLI is not a separate code path. Every subcommand constructs an `AxiomMcpServer` and calls the
same crates the MCP tools use, so a bug reproduced through `axiom blast-radius` is the same bug an
agent sees through `axiom_get_blast_radius`.

Seven MCP tools, all declared and dispatched in `axiom-core/src/mcp.rs`: `axiom_query_symbol`,
`axiom_get_blast_radius`, `axiom_eval_patch`, `axiom_apply_mutation`, `axiom_attest_commit`,
`axiom_record_verification`, `axiom_search_regex`. The tool list in `handle_request` and the
dispatch `match` below it are two places that must be edited together; a tool declared but not
dispatched fails at call time, not at startup.

Language dispatch is by file extension in `parse_file_content`: Java (shared with Kotlin and Scala),
Rust, Python, TypeScript/JavaScript, and Go each have their own line parser.

## Things that are easy to get wrong

**Index discovery walks up from the current directory.** `find_index_file` in `mcp.rs` climbs parent
directories looking for `.axiom/index.json`. The MCP server inherits its client's working directory,
which is the agent's project, not this repo. A server that appears to know nothing about the
codebase is usually one started somewhere with no index above it.

**Seeding is asked for, not automatic, and the check this section used to name is gone.**
`AxiomMcpServer::new` once inserted `auth::service::validate_token` and `test_auth_validation`
whenever the index was empty, which made a workspace nobody had scanned answer confidently about a
symbol in no real codebase. That is now `seed_demo_workspace`, called only by `axiom demo`, so a
server with no index above it answers nothing rather than answering a fixture.

The advice that replaced it was to read `total_symbols_in_index` in the response. No such field
exists, in this or any earlier version reachable from here; `axiom_query_symbol` returns
`dependencies`, `docstring`, `hash`, `id`, `kind`, `signature`, `source_range` and `symbol_path`.
To tell a real index from an empty one, run `axiom scan` and read the symbol count it prints, or
look for `.axiom/index.json` above the working directory.

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

**A toolchain-conditional test can pass without ever running the thing it tests.**
`crates/axiom-cli/tests/multi_language_eval.rs` branches on whether a toolchain is on
PATH: with one it asserts the verdict, without one it asserts the refusal. Both
branches are green, so the suite says nothing about which ran. The TypeScript recipe
reached main that way, reasoned rather than executed (#9). When touching one of these,
break an assertion that only the running branch reaches and confirm the test goes red.
Doing exactly that is what found `resolve_program`: with `deno` and `tsc` both
installed, the test was still taking the refusal branch.

**Windows resolves a bare program name by appending `.exe`, and npm does not ship one.**
`Command::new("tsc")` cannot see `tsc.cmd`, so a toolchain the user runs from their own
shell was reported as not installed. `resolve_program` in `axiom-vmm/src/native.rs`
searches PATHEXT. The order matters: npm drops `deno.cmd`, `deno.ps1` and an
extension-less `deno` holding a POSIX shell script, and matching the bare name first
finds the one Windows cannot execute. PATHEXT candidates win, and the bare name is only
considered when it already carries an extension.

**Kotlin shares Java's assertion trap; Scala does not, and the difference is per language.**
Kotlin's `assert` compiles to a check of the JVM's assertion status, exactly as Java's
does, so without `-J-ea` a false assertion is a no-op and the snippet exits zero.
Measured: `assert(1 + 1 == 3)` printed the line after it and returned success until the
flag was passed. Scala's `assert` is `Predef.assert`, which throws unconditionally, so
no flag is needed and none is passed. A recipe copied from Java to Scala would carry a
flag that does nothing; one copied the other way would lose a flag that decides whether
a false assertion reports `PASSED`. Ask the question per language and answer it by
running it.

**A cold JVM toolchain can outlast the evaluation deadline, and CI is where that shows.**
`scala` and `kotlinc` fetch their compiler on first use. Locally, warm, a Scala snippet
evaluates in about a second; on a fresh CI runner the first one spent 187s downloading
and was killed at the 30s deadline and reported as `TIMEOUT`. That verdict is correct,
it says nothing is known about the snippet, and it is useless to a caller who thinks
their code hung. CI raises `AXIOM_EVAL_TIMEOUT_SECS` to 300 so the tests measure the
recipe rather than the download, which does not weaken the deadline guard:
`eval_deadline.rs` passes its own two-second deadline to `native::evaluate` and never
reads the variable. A bash warm-up step was the first attempt and was wrong for a
Windows-specific reason worth remembering: coursier installs `scala.bat`, which bash
cannot find under the bare name, while axiom's own PATHEXT lookup can. The step failed
for a problem the product does not have. A user's first Scala evaluation on a cold machine will hit the
same wall; the hint already names `AXIOM_EVAL_TIMEOUT_SECS`. This is also the general
shape to expect from this suite: a local green says nothing about a machine with cold
caches, which is why CI runs on two.

**CI sets `AXIOM_REQUIRE_TOOLCHAINS` so a missing toolchain is red rather than green.**
Every evaluator test branches, and both branches pass, so a runner where an install step
silently did nothing runs no recipe and reports success.
`every_language_has_a_toolchain_when_the_environment_promises_one` fails instead, naming
the languages that had none. Unset locally, because a developer without kotlinc should
not get a red suite.

**The Java parser reads three languages, and everything it was taught for the other two
is gated on the file extension.** It matched only Java's shapes, so `object ScalaGate`
indexed nothing at all and a `fun` or `def` was never a symbol: a Kotlin or Scala symbol
was always a type. `object` and `trait` are now type keywords and `fun`/`def` declare
methods, but only for `.kt`, `.kts`, `.scala` and `.sc`. Java never sees the extra
keywords, which matters because `fun` and `def` are ordinary identifiers there and
loosening a match in this parser has form.

`declares_fun_or_def` looks at the tokens before the parameter list rather than at the
whole line, so `foo(fun_arg)` does not match, and it cannot key on a brace because the
commonest shape in both languages has none: `fun isOpen(depth: Int): Boolean = depth > 0`.
A definition with no enclosing type, which Java cannot have, is owned by the file stem,
close to what Kotlin does itself in compiling a top-level `fun` in Gate.kt into `GateKt`.
The stem is validated as an identifier first: an empty owner is exactly the condition
under which this parser once wrote machine-absolute paths into symbol names.
`crates/axiom-ast/tests/jvm_symbols.rs` pins all of that, and pins the four failure
modes alongside it, comment-declared ghosts, call sites, `catch` clauses and paths in
symbol names, because each is a way this change could have gone wrong.

**`LANGUAGES` in axiom-vmm and `parse_by_language` in axiom-ast are twins.** One decides
what is indexed, the other what can be run, they live in different crates, and nothing
made them agree; Kotlin and Scala sat on the first list and not the second for as long
as the tier existed. `every_indexed_language_has_an_evaluator` now fails when they
diverge. Rust is the deliberate exception: it belongs to tier 1.

**A TypeScript snippet cannot assume Node's type declarations.** `import assert from
"node:assert"` runs under deno and is TS2591 under `tsc`, which has no `@types/node`,
so the same snippet passes on one machine and returns a compilation error on another.
The portable form is a bare `throw`, which is why `throw ` is in the language's
`assertion_tokens`: without it a snippet written the documented way reports
`passed_checks_count: 0` beside `PASSED`.

**The verdict cache is measured, not built, and the measurement says do not build it.**
`axiom cache-audit` reads the same graph in the forward direction, from a test to what
it depends on, and compares that against what the blast radius selects. Nothing is
cached and no test is skipped. On this repository 51 of 51 tests produce a usable key
and nothing disagrees in the direction that would skip a test the selector says must run.
A four-file polyglot fixture disagreed on one pair, which is
how the two remaining closure gaps were found: a method did not depend on the type
enclosing it, since containment is not a call, and a `crate::` path was charged to the
environment as though it named something outside the tree. Both are fixed and both trees
now report zero, but read that carefully: adding the containment edge moved the closure
closer to what the blast radius already believed, and the audit measures agreement
between two readings of one graph. Making one match the other raises agreement without
establishing that either is right about the code. Two causes of unkeyability, needing
different fixes. Names from crates outside the tree
(`anyhow::Result`, `std::path::{Path, PathBuf}`) are now folded into `EnvironmentKey`,
a digest over lock files, manifests and compiler versions, rather than counting as
gaps; a `cargo update` or a compiler upgrade moves that digest and invalidates every
key at once. The fingerprints have to be real for that to hold: reusing the evaluator's
probe arguments, which are chosen to be silent, gave `node=` and `python=`, so an
upgrade would have invalidated nothing, and `toolchain_fingerprints.rs` now fails on an
empty version. Ambiguous short names are over-approximated rather than
resolved: the closure depends on every symbol that could answer to the name. The two
mechanisms want opposite biases from one graph, which is the thing to keep hold of. For
selection a wrong extra edge costs one test run; for a key a missing edge skips a test
and reports a pass for code that never ran. Choosing the nearest candidate by file or
directory would have been wrong 49 times out of 51 here, and each wrong pick produces a
key that looks complete. That took usable keys to 51 of 51 and the dangerous count to
zero, but the zero is partly structural: both directions read the same edges, so a call
the parsers never recorded is invisible to the audit as well as to the cache. `closure_hash` returns `Option` for this reason: an
incomplete closure must produce no key at all, because a cache that keys on a partial
view skips a test whose real dependency moved and reports a pass for code that never
ran. Full reasoning in `docs/verdict_cache_audit.md`. Re-run the audit before quoting
any of these numbers; they move with the graph.

**A seal that fails to re-derive does not say why, and `verify` must not pretend it does.**
The seal is recomputed from a record's stored fields together with the symbol and prompt
being claimed, so it fails both when the prompt is not the one the record was issued for
and when a stored field has been edited since. There is no prompt-independent copy to
compare against, because the prompt is not stored, only a digest covering it. So the
no-match path names both causes rather than picking one: it used to report "none for
this prompt", which sent anyone holding an altered ledger looking for a typo. A broken
chain is the one piece of evidence that does point at tampering, and it is reported
here too.

**A caller-supplied field that is printed is an injection surface.**
`agent_identity` reaches `axiom_attest_commit` from the caller and is rendered by
`axiom verify` as one of a column of labelled lines. A value carrying a newline could
add lines of its own, showing `Checked by: sandbox` above a record whose `verified_by`
says `reported`. `agent_identity_of` in `mcp.rs` refuses control characters and bounds
the length where the value enters, rather than escaping it at each place it is shown.
The same reasoning applies to any future field that is both caller-set and displayed.
Note also what makes storing an unverified name acceptable at all: it is hashed into
the seal and covered by the signature, so it cannot be edited afterwards.

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
