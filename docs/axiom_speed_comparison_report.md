# Axiom: measured latencies

Every number here was measured, not projected. Where a measurement contradicts a
claim made elsewhere in this repository, the measurement is what to believe.

**When and where.** Measured 2026-08-25 on one Windows machine, release build,
warm filesystem cache. An earlier version of this document reported the same
operations against a smaller version of the same Java tree; those figures are
superseded, not merely restated, and the two sets are not comparable because the
tree roughly doubled in between.

**What was measured against.**

* **Tree A**, `async-test-lib`: 898 Java source files, 9,058 indexed symbols, of
  which 3,429 are tests.
* **Tree B**, this repository: 55 source files, 543 indexed symbols, of which 53
  are tests.

## The numbers

Tree A unless noted.

| Operation | Measured | Notes |
|---|---|---|
| Index the tree (`axiom scan`) | 3.3 s warm, 4.1 s cold | Produces a 61 MB index. Paid once, then again whenever the tree changes. Tree B: 220 ms. |
| Server startup | 1.1 s, 1.4 s, 2.1 s | Loading that index and rebuilding the trigram index, measured as the time to answer `initialize`. Once per session. Three runs, same machine, same tree; the spread is real. |
| `axiom_query_symbol`, warm | 0.08 to 0.13 ms median | Per query, in a running server, over MCP. Range of the warm median across three runs. |
| `axiom_search_regex`, warm | 0.27 to 0.36 ms median | Per query. First query after startup: 0.34 to 0.46 ms. |
| `axiom_get_blast_radius`, warm | 4.7 to 8.1 ms median | Per query. First: 5.7 to 13.3 ms. |
| `grep -rn` over the same source tree | 80 ms | For comparison with search. |
| One blast radius, `TelemetryEventBuffer::publish` | 12 of 3,429 tests, 99.65% pruned | Depth 1. |
| Evaluate a Rust snippet (`axiom bench`, 20 iterations) | min 204 ms, median 271 ms, mean 373 ms, max 1,485 ms | `rustc` dominates. Identical snippets are recompiled; there is no artifact cache. |
| `axiom swarm --agents 10 --ops 50` | 9.5 ms for 1,000 operations | Zero merge conflicts, replicas converged. |

Blast-radius selection across many symbols, from
`.github/scripts/blast_radius_stats.py` at depth 1:

| | Tree A (sample of 60) | Tree B (all 490) |
|---|---|---|
| Suite | 3,429 tests | 53 tests |
| Reach at least one test | 53 of 60 asked | 103 of 490 |
| Tests selected | mean 16.4, median 8, max 40 | mean 10.1, median 4, max 31 |
| Pruned | mean 99.5%, median 99.8% | mean 81.0%, median 92.5% |
| Mean pairwise Jaccard | 0.01 | 0.11 |

Tree A is sampled because one subprocess per symbol, each loading a 61 MB index,
makes the full 5,629-symbol sweep take about ninety minutes. The sample is seeded
(`--seed 1`) so it repeats.

## What that means, stage by stage

**Search is faster warm and slower cold.** A warm query costs about 0.3 ms
against 80 ms for `grep`, which is roughly 250x. But a session first pays 1 to
2 s of startup on top of a 3.3 s scan, so break-even against simply running
`grep` is somewhere near 50 queries. An agent making a handful of searches is better off
with `grep`; one working a codebase for an hour is not. An earlier claim of
3,125x compared a warm axiom query against a slow `grep` and omitted both fixed
costs.

**Test selection is where the saving is, and its size depends on your suite.**
The few milliseconds it takes to compute a blast radius are beside the point. The point is running 8
tests instead of 3,429. How much wall-clock that saves depends entirely on how
long those tests take, so no multiplier is offered here, and none should be
quoted from this repository. Measure your own.

Read Tree B against Tree A before deciding the feature is for you. On a
53-test suite the same mechanism prunes a median of 92.5% and saves seconds. **The
value of selection scales with the suite it is applied to.**

**The Jaccard number is the one that says selection is real.** A selector that
returned the same tests for every symbol would report the same pruning
percentage and predict nothing. At 0.01 on Tree A, two symbols' answers are
almost disjoint.

**Symbols that reach no test are not a defect and not a pass.** 387 of Tree B's
490 reach nothing at depth 1. For a private helper nothing exercises directly
that is the honest answer. It is not a statement that changing it is safe.

**Zero-clone setup is not free, it is prepaid.** Querying a running server does
avoid a clone. It does not avoid reading the tree: `axiom scan` walks 898 files
in 3.3 s and writes 61 MB, and the server spends another 1 to 2 s loading that
back. Calling this 0 ms measured the query and ignored everything that had to
happen first.

**There is no compiled-artifact reuse.** The claim was that an identical AST hash
reuses pre-compiled bytecode for a 0 ms compile. The content-addressed store has
the functions for it and nothing calls them. Running the same snippet twice
compiles it twice.

**Conflict-free concurrency is real; the baseline it was compared against was
not.** Sixteen agents attesting at once produced 16 records with no forked or
broken links, five runs in a row, and eight agents mutating one workspace kept 8
of 8 symbols. That is worth having. Comparing it against "10 minutes of manual
merge conflict resolution" produced a 6,000,000x figure that describes an
invented baseline rather than a measurement.

**A cold toolchain can outlast the deadline.** `scala` and `kotlinc` fetch their
compiler on first use. Warm, a Scala snippet evaluates in about a second; on a
fresh CI runner the first one spent 187 s downloading and was killed at the 30 s
deadline and reported as `TIMEOUT`. That verdict is correct, it says nothing is
known about the snippet, and it is useless to a caller who thinks their code
hung. CI raises `AXIOM_EVAL_TIMEOUT_SECS` to 300 so the tests measure the recipe
rather than the download.

## One correction from taking these measurements

The first pass at the search figure gave 0.07 ms, four times faster than what is
in the table. It was wrong: the harness sent `{"pattern": ...}` and
`axiom_search_regex` takes `query`, so every one of those calls was timing an
error response rather than a search. The tool behaved correctly, the measurement
did not, and a plausible-looking number came out the other end.

It is recorded here because it is the same failure this repository refuses
everywhere else, arriving from the direction nobody was watching: a number
produced by something that is not a run of the thing being measured. Anything
timing a tool over MCP should assert on the response body before timing it.

## What is not built

Earlier versions of this document described a Tier 2 MicroVM engine with
`Firecracker`, `userfaultfd` paging and `AF_VSOCK`, and quoted a 13 ms test round
trip from it. None of that exists. Evaluation is `rustc` and `wasmtime` in
process, plus each language's own toolchain as an ordinary child process, which
is why a snippet costs a few hundred milliseconds rather than microseconds.

An earlier version of this document also said axiom "cannot evaluate anything
outside Rust". That was true when it was written and is not now: Python,
JavaScript, TypeScript, Go, Java, Kotlin and Scala each have a recipe, and a
language without one is refused rather than guessed at.

## The multiplier in the video title

"23 000 times faster" is not from this document and is not supported by it. It
predates these measurements. No multiplier is offered here, on purpose, because
the only stage where axiom saves meaningful time is the test round trip and that
saving is a function of the suite.

## How to check any of this

```bash
axiom scan --path <your tree>       # files, nodes and elapsed time
axiom bench --iterations 20         # min, median, max and mean for one evaluation
axiom blast-radius --symbol <SYM>   # how many of your tests a change reaches
axiom swarm --agents 10 --ops 50    # concurrent operations and convergence

python .github/scripts/blast_radius_stats.py --binary target/release/axiom
python .github/scripts/blast_radius_stats.py --binary target/release/axiom \
       --path <your tree> --sample 60
```

The warm in-server latencies need a running server rather than the CLI, because
the CLI pays the index load on every invocation: a single `axiom blast-radius`
against Tree A takes 1.1 s wall, almost all of it loading 61 MB. Drive
`axiom serve` over stdio and time the responses to reproduce the sub-millisecond
figures.

The honest summary is narrower than a table of multipliers and more useful: on a
codebase of this size, axiom answers a symbol query in about a tenth of a
millisecond and a search in about a third of one once it is warm, costs four to
six seconds to get there, runs a snippet in a few hundred milliseconds in eight
languages and refuses in the rest, and earns its place by cutting a 3,429-test
suite to single digits for a one-method change.
