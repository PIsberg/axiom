# axiom-ast

The indexer: parsers, the symbol graph, blast radius, Zoekt search, disk I/O, SCIP ingestion.
`src/lib.rs` is 4,916 lines as of 2026-09-11 and holds several indexes that must stay in
agreement: `nodes` (symbol to `AstNode`), `reverse_deps` (symbol to dependents), and the
supporting `method_return_types` and `clean_file_texts` maps behind accessor inference. Anything
that inserts into one usually has to update the others.

Read this before changing a parser, a symbol key, or the reference-resolution pass. Every entry
below is an incident that reached main, with the test that now stops it.

## Parsers are line-based heuristics, not ASTs

`parse_java_content` and its siblings walk lines and match on shape. That approach has already
produced: javadoc lines containing the words "the class Javadoc" hijacking the enclosing class
name; methods filed under the last nested type because brace depth was not tracked; wrapped
parameter lists dropping a method entirely; construction sites and `catch` clauses indexed as
methods; and the `current_class`-empty fallback writing machine-absolute file paths into symbol
names. Each is now pinned by a test. When adding a language or loosening a match, assume those
five are one edit away, and check the resulting index for symbols whose owner is not a valid
identifier.

`parse_by_language` dispatches on extension through `PARSERS`, which is the one place a language
is added. `AstIndex::indexed_extensions` is derived from that table and read by
`every_indexed_language_has_an_evaluator` in axiom-cli, so a parser added here without an
evaluator in axiom-vmm goes red rather than silently shipping a language that can be indexed and
not run. `SOURCE_EXTS` is wider on purpose: `json` and `toml` are walked for the environment
fingerprint and have no parser.

### A declaration is decided from stripped text; only what is stored comes from the raw line

A repository whose subject is parsing writes source inside string literals constantly, and
matching the raw line indexed those fixtures: `blast_radius.rs::looks_like_a_pattern` existed as a
symbol because a test writes a Rust fixture as a string, which made the real function ambiguous
and got a lookup by that name refused. Python, Go and Java already read stripped text; Rust and
TypeScript did not.

`strip_comments_and_strings` preserves the line count, which is what makes indexing the stripped
lines alongside the raw ones safe, and `stripping_preserves_every_line` pins that. The raw line is
still what is stored, so a signature keeps a string the declaration genuinely contains, and
`a_declaration_containing_a_string_keeps_it_in_the_signature` pins that half.

The stripper must not move a line, and twice it did. A Rust lifetime opens with an apostrophe and
never closes one, so skipping to the next apostrophe in the file swallowed the newlines between
them; one lifetime-parameterised struct put everything below it out by one. A backslash line
continuation inside a string lost a newline the same way, five times in `lib.rs` alone. Both are
pinned by `stripping_preserves_every_line`, which runs the stripper over this crate's own source,
because that is where the continuation was found and not in any fixture. The apostrophe means a
string rather than a character in Python, JavaScript and TypeScript, so the caller says which
language is being read: a closing apostrophe is required for a char literal and optional for a
string that ends its line. Without that, a single-quoted sentence mentioning a call was a call.

### The Java parser reads three languages, and the extras are gated on the extension

It matched only Java's shapes, so a Scala `object` indexed nothing at all and a `fun` or `def` was
never a symbol: a Kotlin or Scala symbol was always a type. `object` and `trait` are now type
keywords and `fun` and `def` declare methods, but only for `.kt`, `.kts`, `.scala` and `.sc`. Java
never sees the extra keywords, which matters because `fun` and `def` are ordinary identifiers
there and loosening a match in this parser has form.

`declares_fun_or_def` looks at the tokens before the parameter list rather than at the whole line,
so a call passing an argument named `fun_arg` does not match, and it cannot key on a brace because
the commonest shape in both languages has none: a Kotlin `fun` with an expression body and no
braces at all. A definition with no enclosing type, which Java cannot have, is owned by the file
stem, close to what Kotlin does itself in compiling a top-level `fun` in Gate.kt into `GateKt`.
The stem is validated as an identifier first: an empty owner is exactly the condition under which
this parser once wrote machine-absolute paths into symbol names. `tests/jvm_symbols.rs` pins all
of that, and pins the four failure modes alongside it.

### A Rust test is marked by its attribute, not by its name

`is_test` read only the declaration line, so `name.starts_with("test_") || decl.contains("#[test]")`
was in practice just the name prefix: `#[test]` sits on the line above. This repository mostly
names its tests descriptively, so on 2026-09-11 it held 305 test attributes, 95 of them
`test_`-prefixed, and indexed 94 tests. 210 of its own tests were filed as ordinary functions.

The cost is not mainly the count. `compute_blast_radius` only records a node whose kind is `test`,
so a test the parser did not recognise could never be selected, and a change would get back a
small, confident impacted set with the tests that actually cover it missing. That is a recall
failure, and the fifth idea in the root CLAUDE.md is explicit about which direction is unsafe.

`rust_test_attribute_above` walks up from the declaration over attributes, doc comments and blank
lines, and stops at the first line that is none of those, so a plain function does not inherit the
annotation of whatever sits above it. It reads the stripped text, like every other decision here,
because a fixture in this repository writes `#[test]` inside a string literal often enough to
matter. The Java parser has read `@Test` this way all along; this is the same rule.

`tests/rust_tests_are_recognised.rs` pins both directions, including the string-literal case and
that a function following a test is not one.

### A Go method belongs to its receiver

A Go method declaration has no name before the first parenthesis, only the receiver.
`parse_go_content` took everything before that paren as the name, which for a method is the empty
string, so every method was skipped and `type` was not matched at all: a Go codebase held
package-level free functions and nothing else, one symbol from a file declaring three.
`go_receiver_and_name` reads the receiver out of the parenthesis and treats a pointer receiver as
the same type as a value one. Structs, interfaces and aliases are all indexed, since matching only
`struct` leaves the same gap one keyword narrower.

`tests/go_symbols.rs` is Go's equivalent of `jvm_symbols.rs`, and its absence is why this
survived: `every_indexed_language_has_an_evaluator` checks Go is on both lists, and nothing
checked that the parser finds what a Go file declares.

## Symbol keys

### A key is relative to the scan root

`key_under_root` returns the path below the scanned root with forward slashes and no absolute
prefix, so a Rust symbol is `crates/axiom-ast/src/lib.rs::AstIndex` and not a path starting at the
drive letter. That is what makes the index committable and a ledger's `commit_merkle_root`
comparable across machines; `tests/keys_are_portable.rs` pins that the same source under two
directories indexes identically.

The filesystem still needs the absolute path, so the scan root is kept in `scan_root`, the
per-file root in `file_roots` (one index can hold two subtrees scanned separately), and re-derived
on load from the index's own location, the parent of its `.axiom`, so a repository that moved
still resolves. `file_of_symbol` joins it back on and returns an absolute path, and it is the one
accessor that reaches a real file; a test that read the key prefix as a path directly had to move
to it.

### A Rust symbol is keyed by every block it sits inside, and `mod` is one of them

`impl` and `trait` were tracked and `mod` was not, so two modules declaring the same function were
one key: the second `index_node_at` overwrote the first and the surviving node carried the second
declaration under a name that reads as either. `rust_symbol_in` joins the whole owner stack, so a
function inside an `impl` inside a `mod` carries all three. A bare module declaration with no body
opens no scope and must not become an owner, or every symbol below it is filed under a module
whose body is in another file.

Twins guarded by mutually exclusive `cfg` attributes stay one key on purpose: they are one name in
one scope and only one is ever compiled, so a single node with both declaration lines recorded is
honest rather than a gap.

Before the enclosing-block fix, the two `search` methods in `lib.rs`, one on `AstIndex` and one on
`ZoektIndex`, were one key: the second overwrote the first, and the declaration line recorded for
the key moved with it, so every call inside the first was charged to whatever symbol preceded it.
`symbol_lines` therefore keeps every declaration line for a key rather than the last one parsed.

### A short name is not the last dot-separated segment

`simple_name_of` distinguishes a package-keyed symbol from a file-keyed one. Splitting on the last
dot unconditionally took the file extension for a package separator: every Rust symbol reduced to
`rs`, and the fallback search matched that against every Rust symbol in the index. The blast
radius for anything in this repository was every test in it. A Java-only fixture suite cannot
catch this, which is why the tests scan Rust and Python side by side.

The fallback reads the symbol path, and only the symbol path. `signature` holds the declaration,
and matching a name against that would put every test whose declaration mentions it into the
answer, which is the same loosening wearing a different hat.

### A node's hash covers the body

It used to cover the declaration line alone, so editing what a multi-line function does moved
nothing, which defeated the verdict cache at its foundation: `closure_hash` is a digest over node
hashes, so a changed body left the key where it was and a cache would report a pass for code that
changed, past every guard because the closure still looked complete.

`body_span` bounds a body by brace balance, or by indentation where the declaration opens no
brace, and `index_node_at` takes it as a separate argument so `signature` and `source_range` keep
meaning the declaration. The test guarding the property could not see it break: its fixture was a
one-line function whose body sat on the declaration, so the hash moved for the wrong reason. It is
multi-line now. Any cache-audit or cache-validate figure taken before this rests on hashes that
did not cover the code and is not comparable with one taken after.

### `source_range` is a line range and `signature` is the declaration

`source_range` is one-based and inclusive over the file `symbol_path` names, so a line range
printed from that file shows the declaration, and it spans several lines for a wrapped parameter
list. A zero range means no position was recorded, which is what a node inserted by hand through
`index_node` has. `tests/source_positions.rs` pins both against the fixture files it writes.

They used to hold a length rather than a position, and a copy of a field the response already
carries. That was not cosmetic: `cache-validate` located symbols by `source_range`, edited from
line zero to that length, which on a short file is all of it, and reported that mutating an
unrelated symbol broke a test only one other symbol reaches. What still cannot be read off the
node is where a symbol ends: the range brackets the declaration, not the body, and it describes
the file as it was scanned. Anything mutating a symbol has to find it in the file as it is now, as
`mutate::symbol_lines` in axiom-cli does.

## Dependency edges

### Java has three mechanisms and everything else has a fourth

Java edges come from imports, from same-package and fully-qualified references, and from accessor
return-type inference, since a test calling an accessor never names the type it returns. Comments
and string literals are stripped first, because matching raw file text turned every javadoc
mention into a dependency. `test_e2e_comment_stripping_and_class_literal_dependencies`,
`test_e2e_same_package_dependencies_blast_radius` and
`test_e2e_accessor_return_type_dependency_resolution` pin the three. Widening any of them
re-admits comment noise; narrowing drops real dependents. Judge a change by measuring both
directions against a real repository, not by the size of the result set.

Rust, Python, TypeScript, JavaScript, Go and C/C++ had none of that: their parsers recorded each
file's import lines verbatim as every node's dependencies, so `reverse_deps` was keyed by strings
naming crates outside the tree and nothing ever resolved to an indexed symbol. `record_references`
and `resolve_reference_edges` supply the missing pass: references are collected per line while
parsing, then, once the whole tree is read, each is charged to the last symbol declared above it
and kept only if some indexed symbol answers to that name. The pass runs at the end of
`scan_directory` because a file that references a symbol defined further down the walk cannot be
resolved when it is read. `tests/blast_radius.rs` pins it.

### `reverse_deps` is keyed by the name a caller writes; its values are full symbol paths

The traversal has to look up both on each hop. Looking up only the path found nothing after the
first step, so every transitive layer was silently empty for the file-keyed languages.

### Three ways the pass has silently dropped an edge

All three were found through one missing call: `AstIndex::search` calls `looks_like_a_pattern` and
the graph did not know it, so the blast radius said no test reaches that function while a real
test of the search modes failed on a mutation of it (#32). The causes were the key collision
above, `symbol_lines` keeping only the last declaration, and the stripper moving lines.

None of the three is visible to `cache-audit`, because the forward closure and the reverse walk
read the same edges. That is the general shape: agreement between two readings of one graph is not
evidence about the code.

### `forget_file` only deletes a symbol it still exclusively owns

Two files can carry one key, a package-keyed Java class of the same name among them, so purging a
stale file must not delete a symbol another file still declares. It checks `symbol_to_file` for
the current owner before removing a node. This was an intermittent parallel failure of the full
end-to-end loop: a stale entry in a shared index, left by one test and re-declared by another,
deleted the live symbol when purged. `tests/forget_keeps_shared_symbols.rs` pins it.

## SCIP ingestion

`src/scip_ingest.rs` is the alternative to the line parsers: it reads a SCIP index a language's
own indexer produced (scip-java, rust-analyzer, and the rest) and builds the same `AstIndex` from
resolved definitions and references rather than heuristics. `axiom scan --scip <file>` routes to
`AstIndex::ingest_scip`; the CLI arm is the only caller, and it persists to `.axiom/index.json`
exactly as a scan does, so every tool downstream is unchanged.

A SCIP symbol is rendered to a readable key by `render_symbol`, dropping the package and version
so the key is portable the way relative file keys are. Edges come from charging each reference
occurrence to the definition whose `enclosing_range` contains it, keeping only references to
symbols defined somewhere in the index; a definition with no `enclosing_range` has its body
widened to the next definition, exactly the fallback the line parsers use. The tests pin that a
real single-line `enclosing_range` is not widened, which was a bug that filed a method's calls
under its enclosing class.

A test is marked by the SCIP `Test` role where the indexer sets it, as scip-java does.
rust-analyzer does not mark Rust test functions, so ingestion falls back to `is_test_path_or_file`
or a `test_` name prefix, or the blast radius would find no tests in a SCIP-ingested Rust project.
That is weaker than what the scan now does: the scan reads `#[test]` off the lines above a
declaration, and a SCIP index carries no source lines to read, so a descriptively named test in a
file whose path does not look like a test path is still invisible on this path. It is the reason
`is_test_path_or_file` stays in the fallback rather than being dropped for the attribute rule. The `relationships` field adds edges an occurrence scan
misses, an implementation reaching its interface.

`tests/scip_ingest.rs` builds a SCIP index in memory, so it needs no indexer installed.
`tests/scip_real_indexer.rs` runs a real rust-analyzer SCIP export (a rustup component CI
installs) and ingests its output, gated on `AXIOM_REQUIRE_TOOLCHAINS` so a missing indexer is red
rather than a silent skip; scip-java emits the same format and would slot in the same way.

## Keys for a verdict cache

`closure_hash` returns `Option` on purpose: an incomplete closure must produce no key at all,
because a cache that keys on a partial view skips a test whose real dependency moved and reports a
pass for code that never ran.

Names from crates outside the tree are folded into `EnvironmentKey`, a digest over lock files,
manifests and compiler versions, rather than counting as gaps; a dependency update or a compiler
upgrade moves that digest and invalidates every key at once. The fingerprints have to be real for
that to hold: reusing the evaluator's probe arguments, which are chosen to be silent, gave empty
versions for node and python, so an upgrade would have invalidated nothing, and
`axiom-vmm/tests/toolchain_fingerprints.rs` now fails on an empty version.

Ambiguous short names are over-approximated rather than resolved: the closure depends on every
symbol that could answer to the name. The two mechanisms want opposite biases from one graph,
which is the thing to keep hold of. For selection a wrong extra edge costs one test run; for a key
a missing edge skips a test and reports a pass for code that never ran. Choosing the nearest
candidate by file or directory would have been wrong 49 times out of 51 here, and each wrong pick
produces a key that looks complete.

Full reasoning in `docs/verdict_cache_audit.md`. Re-run `axiom cache-audit` before quoting any
figure from it; they move with the graph.

## Persistence

**Failures must stay loud.** `save_to_disk` returns the path it wrote and verifies the file
exists, and callers propagate the error instead of discarding it. When these were discarded under
an unconditional success banner, `scan` printed that it had saved the index while writing nothing
and the server then served an empty index. `test_e2e_disk_persistence_cross_instance` writes in
one instance and reads in another, which is the only shape that catches it: an in-process
scan-then-query test passes with persistence completely broken.

**The Merkle root that `scan` prints comes from the CRDT tree, not the AST index.** Keep the two
apart when reading output. `test_e2e_dynamic_merkle_root_uniqueness` pins that the root varies
with scanned content.

**Retries are for errors that can clear, and the set differs by platform.** Windows fails a rename
or an exclusive create with a sharing violation while another process holds the file open,
surfacing as `PermissionDenied`, and it clears when that handle closes. Unix has no such rule: a
rename succeeds with readers attached, and `EACCES` means the directory is not writable, which
waiting will not change. `worth_retrying` encodes that difference, and everything outside it is
treated as final. Retrying a full disk or a cross-device rename only delays an accurate error, and
retrying `EACCES` on Unix turns an immediate report into a thirty-second pause followed by the
same report.

All measurement in this repository so far is from Windows. The concurrency numbers quoted in
commit messages, and the sharing-violation behaviour the retry loops exist for, were observed
there. The retries are written to be correct on Unix rather than merely harmless, but that has
been reasoned rather than run.
