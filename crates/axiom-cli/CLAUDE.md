# axiom-cli

clap subcommands (19 as of 2026-09-11), all of which drive `AxiomMcpServer`. Nothing depends on
this crate. `src/mutate.rs` holds the in-place symbol mutation that `cache-validate` uses.

## Ground truth comes from `cache-validate`, not from the audit

The audit compares two readings of one graph, so agreement between them says nothing about a call
the parsers never recorded: both walks are blind to it together.

`axiom cache-validate` breaks a symbol, runs the project's own suite, and checks that every test
that really failed was selected by the blast radius and had its key move. It edits files in place
and restores them from `Drop`, so a panic between writing and restoring still puts the tree back.

Two rules keep it honest: a mutation that does not compile is thrown away, since it fails every
test for one reason and says nothing about dependencies, and a run where nothing failed is
reported as establishing nothing rather than as a pass. Its first run here found a real hole on
its second mutation (#32); after that fix, `cache-validate --samples 10 --depth 2` produced six
real failures and no test that the blast radius missed or whose key stayed still. Four of the ten
mutations broke nothing, which establishes nothing about those four rather than passing them.

## Behind the selector, the cache's saving and its unsafety are one number

A test runs when the blast radius picks it and its key moved, so the work a cache removes is
exactly the pairs where the selector picks a test and the key did not: "would wrongly skip".
Driving that to zero, which is what makes it safe, drives the saving to zero with it. This is
arithmetic rather than an artefact of the parsers, so no amount of precision in the graph escapes
it, and `behind_the_selector_saving_and_unsafety_are_the_same_number` pins it.

The case a cache can serve and selection cannot is a change of unknown extent, a merge or a pull,
where nothing names the change as a symbol. Decide which of those the feature is for before making
the graph more precise.

Measured figures for both cases are in `docs/verdict_cache_audit.md` with the date they were
taken. They move with the suite; re-run `axiom cache-audit` and
`.github/scripts/blast_radius_stats.py` before quoting any of them.

## A mutation has to find the symbol in the file as it is now

`AstNode::source_range` brackets the declaration as it was scanned, not the body and not the file
as it stands. `mutate::symbol_lines` re-finds the declaration rather than trusting the stored
range, and keeps every declaration line for a key, because two declarations can genuinely share
one (mutually exclusive `cfg` spellings of one function being the honest case).

## Driving the server by hand is often faster than writing a test

It is a stdio program, so piping JSON-RPC lines into `axiom serve` gives a full session. See the
root CLAUDE.md for a copy-pasteable example.
