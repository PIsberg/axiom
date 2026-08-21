# Axiom: measured latencies

Every number here was measured on one machine against one repository. Nothing in
this document is projected, and where a measurement contradicts a claim made
elsewhere in this repository, the measurement is what to believe.

**What was measured against.** `async-test-lib`: 459 Java source files, 5,934
indexed symbols, 2,219 tests. Windows, release build, warm filesystem cache.
Repeat with `axiom bench` and `axiom scan` on your own tree; the shape of the
answer will hold and the figures will not.

## The numbers

| Operation | Measured | Notes |
|---|---|---|
| Index the tree (`axiom scan`) | 1.5 s warm, 3.8 s cold | Produces a 49 MB index. Paid once, then again whenever the tree changes. |
| Server startup | 1.1 s | Loads that index and rebuilds the text search index. Once per session. |
| Symbol search, warm | 0.2 to 0.4 ms | Per query, in a running server. |
| Blast radius, warm | 1.2 to 1.4 ms | Per query. Selects 32 of 2,219 tests for one method. |
| Evaluate a Rust snippet | 165 to 250 ms, median 176 | `rustc` dominates. Identical snippets are recompiled; there is no artifact cache. |
| `grep -rn` over the same tree | 52 ms | For comparison with search. |

## What that means, stage by stage

**Search is faster warm and slower cold.** A query costs 0.4 ms against 52 ms for
`grep`, which is about 130x. But a session first pays 1.1 s of startup on top of
a 1.5 s scan, so the break-even against simply running `grep` is somewhere near
50 queries. An agent making a handful of searches is better off with `grep`; one
working a codebase for an hour is not. The earlier claim of 3,125x compared a
warm axiom query against a slow `grep` and omitted both fixed costs.

**Test selection is where the real saving is, and it is not about query speed.**
The 1.4 ms to compute a blast radius is beside the point. The point is running 26
tests instead of 2,219. That was verified by breaking a method deliberately and
running only the tests axiom named: two of them failed, the two that cover it.
How much time that saves depends entirely on the suite, so no multiplier is
offered here. Measure your own.

**Zero-clone setup is not free, it is prepaid.** Querying a running server does
avoid a clone. It does not avoid reading the tree: `axiom scan` walks 459 files
in 1.5 s and writes 49 MB, and the server spends 1.1 s loading that back. Calling
this 0 ms measured the query and ignored everything that had to happen first.

**There is no compiled-artifact reuse.** The claim was that an identical AST hash
reuses pre-compiled bytecode for a 0 ms compile. The content-addressed store has
the functions for it and nothing calls them. Running the same snippet twice
compiles it twice: 347 ms, then 178 ms, the difference being filesystem cache
rather than reuse.

**Conflict-free concurrency is real; the baseline it was compared against was
not.** Sixteen agents attesting at once produced 16 records with no forked or
broken links, five runs in a row, and eight agents mutating one workspace kept
8 of 8 symbols. That is worth having. Comparing it against "10 minutes of manual
merge conflict resolution" produced a 6,000,000x figure that describes an
invented baseline rather than a measurement.

## What is not built

The earlier version of this document described a Tier 2 MicroVM engine with
`Firecracker`, `userfaultfd` paging and `AF_VSOCK`, and quoted a 13 ms test round
trip from it. None of that exists. Evaluation is `rustc` and `wasmtime` in
process, which is why a snippet costs about 176 ms rather than microseconds.

## How to check any of this

```bash
axiom scan --path <your tree>     # prints files, nodes and elapsed time
axiom bench --iterations 20       # min, median, max and mean for one evaluation
axiom blast-radius --symbol <SYM> # how many of your tests a change reaches
```

The honest summary is narrower than a table of multipliers and more useful: on a
codebase of this size, axiom answers structural questions in about a millisecond
once it is warm, costs a couple of seconds to get there, cannot evaluate anything
outside Rust, and earns its place by cutting a 2,219-test suite to 26 for a
one-method change.
