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
cargo test                            # 189 tests across e2e, mcp, crdt, persistence, blast radius, eval, cache audit, key format, seal coverage, env confinement, protocol
cargo test --test e2e_test            # one test file
cargo test test_e2e_same_package      # one test by name substring
```

The binary is at `target/release/axiom`, for whatever the host target is. A pin to
`x86_64-pc-windows-msvc` used to live in `.cargo/config.toml`; it put the binary somewhere else and
made `cargo build` fail on any machine that is not Windows, which CI caught on its first run.

On Windows the C toolchain must be on the path before cargo runs, or `zstd-sys`,
`wasmtime-internal-fiber` and `ittapi-sys` fail inside their build scripts:

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

Eight MCP tools, all declared and dispatched in `axiom-core/src/mcp.rs`: `axiom_query_symbol`,
`axiom_get_blast_radius`, `axiom_eval_patch`, `axiom_apply_mutation`, `axiom_attest_commit`,
`axiom_record_verification`, `axiom_search_regex`, `axiom_run_tests`. The tool list in
`handle_request` and the dispatch `match` below it are two places that must be edited together; a
tool declared but not dispatched fails at call time, not at startup, and
`declared_tools_are_dispatched.rs` pins the set so the count here cannot drift.

`axiom_run_tests` runs the project's own test command in the workspace and records the outcome as a
third verification kind, `executed`: axiom ran it and saw the exit code, so it can vouch for it,
between `sandbox` (axiom's own evaluator ran it) and `reported` (an agent says it ran something).
The command runs with the confined environment `run_with_timeout` gives every evaluation, so it
cannot read the signing key, and is killed as a whole process tree past `AXIOM_TEST_TIMEOUT_SECS`
(default 600, separate from the evaluator's).

Language dispatch is by file extension in `parse_file_content`: Java (shared with Kotlin and Scala),
Rust, Python, TypeScript/JavaScript, and Go each have their own line parser.

## Things that are easy to get wrong

**Index discovery walks up from the current directory.** `find_index_file` in `mcp.rs` climbs parent
directories looking for `.axiom/index.json`. The MCP server inherits its client's working directory,
which is the agent's project, not this repo. A server that appears to know nothing about the
codebase is usually one started somewhere with no index above it.

**Writes go to the same `.axiom` the read came from.** The server records the discovered directory in
`axiom_dir` and derives the ledger, the op log and the mutation index from it, through `ledger_path`,
`op_log_path` and `index_path`. Before that, reads walked up to find the index while every write used
`<cwd>/.axiom`, so an agent working from a subdirectory wrote where the next read would not look.
`axiom verify` walks up the same way, through `find_axiom_dir`. `axiom scan` and `axiom watch` are the
exception on purpose: they anchor to the local `.axiom/index.json` and nothing above it, because a
scan states what one tree contains and must not fold an ancestor index into it.
`server_writes_where_it_reads.rs` and `scan_is_anchored.rs` pin both halves.

**Seeding is asked for, not automatic, and the check this section used to name is gone.**
`AxiomMcpServer::new` once inserted `auth::service::validate_token` and `test_auth_validation`
whenever the index was empty, which made a workspace nobody had scanned answer confidently about a
symbol in no real codebase. That is now `seed_demo_workspace`, called only by `axiom demo`, so a
server with no index above it answers nothing rather than answering a fixture.

`axiom_query_symbol` returns `total_symbols_in_index` only on its not-found branch, beside the
error; a successful lookup returns `dependencies`, `docstring`, `hash`, `id`, `kind`, `signature`,
`source_range` and `symbol_path` and no count. So the count is there to read when a symbol misses,
which is exactly when telling a real index from an empty one matters, but do not expect it on a hit.
To check the index directly, run `axiom scan` and read the symbol count it prints, or look for
`.axiom/index.json` above the working directory.

**A Rust symbol is keyed by every block it sits inside, and `mod` is one of them.**
`impl` and `trait` were tracked and `mod` was not, so two modules declaring the same
function were one key: the second `index_node_at` overwrote the first and the surviving
node carried the second declaration under a name that reads as either.
`rust_symbol_in` joins the whole owner stack, so `mod alpha { impl X { fn y } }` is
`file.rs::alpha::X::y`. `mod foo;` opens no scope and must not become an owner, or every
symbol below it is filed under a module whose body is in another file.
`#[cfg]`-guarded twins stay one key on purpose: they are one name in one scope and only
one is ever compiled, so a single node with both declaration lines recorded is honest
rather than a gap.

**A node's hash covers the body, and a one-line fixture is why it did not.** It used to
cover the declaration line alone, so editing what a multi-line function does moved
nothing, which defeated the verdict cache at its foundation: `closure_hash` is a digest
over node hashes, so a changed body left the key where it was and a cache would report a
pass for code that changed, past every guard because the closure still looked complete.
`body_span` bounds a body by brace balance, or by indentation where the declaration opens
no brace, and `index_node_at` takes it as a separate argument so `signature` and
`source_range` keep meaning the declaration.
The test guarding the property could not see it break: its fixture was
`pub fn is_open(depth: i32) -> bool { depth > 0 }`, one line, body on the declaration, so
the hash moved for the wrong reason. It is multi-line now, and reverting the source makes
it fail along with the two new ones. Any cache-audit or cache-validate figure taken
before this rests on hashes that did not cover the code and is not comparable with one
taken after.

**A Go method belongs to its receiver, and `func (a *Alpha) Search(` has no name before
the first paren.** `parse_go_content` took everything before that paren as the name, which
for a method is the empty string, so every method was skipped and `type` was not matched
at all: a Go codebase held package-level free functions and nothing else, one symbol from
a file declaring three. `go_receiver_and_name` reads the receiver out of the parenthesis
and treats a pointer receiver as the same type as a value one. Structs, interfaces and
aliases are all indexed, since matching only `struct` leaves the same gap one keyword
narrower. `crates/axiom-ast/tests/go_symbols.rs` is Go's equivalent of `jvm_symbols.rs`,
and its absence is why this survived: `every_indexed_language_has_an_evaluator` checks Go
is on both lists, and nothing checked that the parser finds what a Go file declares.

**A declaration is decided from the stripped text; only what is stored comes from the
raw line.** A repository whose subject is parsing writes source inside string literals
constantly, and matching the raw line indexed those fixtures:
`blast_radius.rs::looks_like_a_pattern` existed as a symbol because a test writes a Rust
fixture as a string, which made the real function ambiguous and got
`axiom symbol --path looks_like_a_pattern` refused by name. Python, Go and Java already
read stripped text; Rust and TypeScript did not. `strip_comments_and_strings` preserves
the line count, which is what makes indexing the stripped lines alongside the raw ones
safe, and `stripping_preserves_every_line` pins that. The raw line is still what is
stored, so a signature keeps a string the declaration genuinely contains, and
`a_declaration_containing_a_string_keeps_it_in_the_signature` pins that half.

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

**Three ways that pass has silently dropped an edge, all found through one missing call.**
`AstIndex::search` calls `looks_like_a_pattern` and the graph did not know it, so the blast
radius said no test reaches that function while `cargo test --test e2e_test search_modes`
really failed on a mutation of it (#32). None of the three is visible to `cache-audit`,
because the forward closure and the reverse walk read the same edges.

A Rust symbol used to be keyed by file and short name alone, so the two `search` methods in
`lib.rs`, one on `AstIndex` and one on `ZoektIndex`, were one key: the second overwrote the
first, and the declaration line recorded for the key moved with it, so every call inside the
first was charged to whatever symbol preceded it. `parse_rust_content` now tracks the
enclosing `impl` or `trait` by brace depth and keys methods as `lib.rs::AstIndex::search`.
Modules are not tracked, so two same-named functions in two `mod` blocks of one file still
collide.

`symbol_lines` therefore keeps *every* declaration line for a key rather than the last one
parsed. Two declarations can genuinely share a key, `#[cfg(windows)]` and `#[cfg(unix)]`
spellings of one function being the honest case, and charging the earlier one's calls to
the symbol above it is the failure that leaves.

`strip_comments_and_strings` must not move a line, and twice it did. A Rust lifetime opens
with an apostrophe and never closes one, so skipping to the next apostrophe in the file
swallowed the newlines between them; one `struct Holder<'a>` put everything below it out by
one. A backslash line continuation inside a string lost a newline the same way, five times
in `lib.rs` alone. Both are pinned by `stripping_preserves_every_line`, which runs the
stripper over this crate's own source, because that is where the continuation was found and
not in any fixture. The apostrophe means a string rather than a character in Python,
JavaScript and TypeScript, so the caller says which language is being read: a closing
apostrophe is required for a char literal and optional for a string that ends its line.
Without that, `'sensitive_thing() is not called here'` was a call.

**A symbol's short name is not its last dot-separated segment.** `simple_name_of` distinguishes a
package-keyed symbol, `pkg.Class::method` to `Class`, from a file-keyed one,
`src/lib.rs::write_atomically` to `write_atomically`. Splitting on the last dot unconditionally took
the file extension for a package separator: every Rust symbol reduced to `rs`, and the fallback
search matched `rs::` against every Rust symbol in the index. The blast radius for anything in this
repository was all 49 tests. A Java-only fixture suite cannot catch this, which is why the new tests
scan Rust and Python side by side. The fallback reads the symbol path, and only the symbol path:
`signature` now holds the declaration, and matching a name against that would put every test whose
declaration mentions it into the answer, which is the same loosening wearing a different hat.

**`reverse_deps` is keyed by the name a caller writes, and its values are full symbol paths.** The
traversal therefore has to look up both on each hop. Looking up only the path found nothing after
the first step, so every transitive layer was silently empty for the file-keyed languages.

**A non-zero exit is not a verdict.** `scala` and the JVM launchers fetch their compiler
on first use, and when that fetch fails they exit non-zero having executed nothing the
caller wrote. That reached CI as `FAILED` after 134 seconds of failed downloads, which
tells an agent its code is wrong on the strength of a network error, and it is the same
class as the assertion-substring fallback removed earlier: a verdict produced by
something that is not a run of the code. `toolchain_failure_reason` in
`axiom-vmm/src/native.rs` matches resolver and downloader failures only, and both the
build and run steps turn those into `EvaluatorUnavailable`. The markers stay narrow on
purpose: mistaking a snippet's own output for a broken toolchain costs a refusal, which
says nothing was established and is true either way, while widening them until they
swallow real failures trades one wrong answer for another.
`AXIOM_EVAL_TIMEOUT_SECS` does not cover this; CI raises it to 300 and the run was well
inside that.

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

**But its environment is confined, and every tier's is.** `confine_environment` in `native.rs`
clears the child's environment and passes only an allowlist of names and prefixes a toolchain reads,
plus whatever `AXIOM_EVAL_ENV_PASS` adds; `AXIOM_SIGNING_KEY` and `AXIOM_SIGNING_KEY_FILE` are
refused even there. Before this a snippet read the signing key straight out of `os.environ` and the
value came back in the report, which handed the party the signature exists to check the key to sign
anything. The usability probe and the version fingerprint run under the same confinement, so a
toolchain that needs a dropped variable reads as missing rather than failing the snippet.
`child_environment.rs` pins it, and a new variable a toolchain needs goes in `PASSED_NAMES` or
`PASSED_PREFIXES`, never by widening the two refused names.

**A timeout ends the whole process tree, not just the child.** `run_with_timeout` puts the child in
its own process group on Unix and kills the group, and uses `taskkill /T` on Windows, because
`go run`, the `kotlin` launcher and a `Popen` from a Python snippet all outlived a kill aimed at the
child alone. The pipes are drained for a bounded grace after the child exits rather than to EOF,
because a surviving grandchild holds them open and draining to EOF turned a two-second deadline into
sixty. `Finished.drained` records when output may be short for that reason. `process_tree.rs` pins it.

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

**`AstNode::source_range` is a line range and `AstNode::signature` is the declaration.**
`source_range` is one-based and inclusive over the file `symbol_path` names, so
`sed -n 'start,endp'` on that file prints the declaration, and it spans several lines for a
wrapped parameter list. `(0, 0)` means no position was recorded, which is what a node
inserted by hand through `index_node` has. `crates/axiom-ast/tests/source_positions.rs`
pins both against the fixture files it writes.

They used to hold `(0, content.len())` and the symbol path, a length rather than a position
and a copy of a field the response already carries. That was not a cosmetic problem:
`cache-validate` located symbols by `source_range`, edited from line 0 to line `len`, which
on a short file is all of it, and reported that mutating `unrelated` broke a test only
`is_open` reaches. What still cannot be read off the node is where a symbol *ends*: the
range brackets the declaration, not the body, and it describes the file as it was scanned.
Anything mutating a symbol has to find it in the file as it is now, as `mutate::symbol_lines`
does.

**Ground truth comes from `cache-validate`, not from the audit.** The audit compares two
readings of one graph, so agreement between them says nothing about a call the parsers
never recorded: both walks are blind to it together. `axiom cache-validate` breaks a
symbol, runs the project's own suite, and checks that every test that really failed was
selected by the blast radius and had its key move. It edits files in place and restores
them from `Drop`. Two rules keep it honest: a mutation that does not compile is thrown
away, since it fails every test for one reason and says nothing about dependencies, and a
run where nothing failed is reported as establishing nothing rather than as a pass. Its
first run here found a real hole on its second mutation (#32); after that fix,
`cache-validate --samples 10 --depth 2` produced six real failures and no test that the
blast radius missed or whose key stayed still. Four of the ten mutations broke nothing,
which establishes nothing about those four rather than passing them.

**Behind the selector, the cache's saving and its unsafety are one number.** A test
runs when the blast radius picks it and its key moved, so the work a cache removes is
exactly the pairs where the selector picks a test and the key did not: `would wrongly
skip`. Driving that to zero, which is what makes it safe, drives the saving to zero with
it. This is arithmetic rather than an artefact of the parsers, so no amount of precision
in the graph escapes it, and `behind_the_selector_saving_and_unsafety_are_the_same_number`
pins it. Measured here: for a change to one known symbol the selector runs 2.3 of 54
tests and adding the cache skips 0 more. The case a cache can serve and selection cannot
is a change of unknown extent, a merge or a pull, where nothing names the change as a
symbol: there it runs 11.6 of 54, leaving 79% of verdicts standing. Decide which of
those the feature is for before making the graph more precise.

**The verdict cache is measured, not built, and the measurement says do not build it.**
`axiom cache-audit` reads the same graph in the forward direction, from a test to what
it depends on, and compares that against what the blast radius selects. Nothing is
cached and no test is skipped. On this repository 54 of 54 tests produce a usable key
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
key that looks complete. That took usable keys to 54 of 54 and the dangerous count to
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

**The seal covers every stored field, and it did not always.** `seal_over` in `axiom-proto` hashes
the two roots, the agent identity, the symbol, the task id, `verified_by`, `verification_detail`,
the timestamp and the previous seal, each length-prefixed, plus the prompt. It once covered the
roots, the identity, the prompt, the symbol and the task id only, so editing `verified_by` from
`reported` to `sandbox` in an unsigned ledger left a record that still printed VALID, which forges
the whole distinction the record exists to carry. `generate` and `verify` both go through
`seal_over` so they cannot drift, and `seal_covers_the_record.rs` pins one edited-field-fails case
per field. Any new stored field on `ProvenanceAttestation` has to be added to `seal_over`, or it is
forgeable. `prompt_digest` and `sandbox_trace_hash` are now real digests of the prompt and of the
verification, not slices of the combined digest.

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
(`target-lexicon`, `zstd-sys`, `wasmtime-internal-fiber`, `ittapi-sys`) with `Os { code: 5, PermissionDenied }`
before any of this crate's own code compiles. It is not a toolchain problem and switching to the
`x86_64-pc-windows-gnu` target does not help. Point cargo at a writable directory instead:

```bash
CARGO_TARGET_DIR=<writable-dir> cargo test --release
```

Never read a build or test result through a pipe: `cargo test | tail` reports the status of `tail`,
so a failed build looks green. Capture the exit code directly, or redirect to a file and check `$?`.
