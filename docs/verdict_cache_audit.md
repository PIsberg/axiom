# Can a verdict cache be built on this graph?

Not yet. This is what the measurement says, and how to repeat it.

## The idea being tested

Blast-radius selection answers "what could this change break?" It narrows a
change to the tests that reach the symbol, and that is where most of the saving
in the agent loop comes from. But every test it selects still compiles and runs
from cold, at full price.

A verdict cache asks the other question. A test's outcome is a function of its
inputs: the transitive set of symbols it can reach, and their contents. If that
set is unchanged since the last run, the previous verdict is still valid, and
neither the test nor the compilation behind it has to happen again. Selection
bounds correctness; a cache bounds work. It would turn "run the 32 tests in the
blast radius" into "run the 3 whose inputs actually moved".

This is what Bazel and Buck do. The difference is that they require hand-written
build files declaring dependencies, and axiom derives the graph from the AST, so
nobody maintains it. That is the appeal, and it is also the risk: a derived graph
can be wrong in ways a declared one cannot.

## Why it is measured before it is built

An under-approximated closure produces a stable key for a test whose real
dependency changed. The cache then reports a pass for code that was never run.
That is the same wrong answer `EvaluatorUnavailable` exists to prevent, with a
longer reach, because a stale green survives across sessions rather than being
re-derived each time.

So the cache ships as measurement first. `axiom cache-audit` computes what a
cache would decide and reports it. Nothing is cached, and no test is skipped.

The audit compares two readings of the same graph. For every symbol, the tests
the blast radius selects are compared against the tests whose forward closure
contains that symbol. Where they disagree, the direction matters:

- **Would wrongly skip.** The blast radius selects the test; the test's closure
  does not contain the symbol. A cache keyed on that closure would skip a test
  the selector says must run. This is the number that decides whether to build
  anything.
- **Would run unselected.** The closure contains the symbol; the blast radius did
  not select the test. Costs a test run. Points at under-selection, not at an
  unsound cache.

## The measurement

Taken on this repository, 25 files and 271 symbols, at depth 1:

```
 Tests in index:             52
 Tests with a usable key:    0 of 52
 Symbols audited:            271

 Both mechanisms agree:      362
 Cache would wrongly skip:   322
 Cache would run unselected: 3370
 Agreement:                  8.93%
```

Repeat it with:

```bash
axiom cache-audit --path .
```

Two runs over an unchanged tree produce identical counts. Editing the tree
between runs changes them, which is the point.

## What it means

**Do not build the cache.** Not because the idea is wrong, but because the
precondition fails on the graph as it stands. Two separate things are failing,
and they want different fixes.

**Nothing has a usable key.** Every test's closure contains at least one name
that resolved to no indexed symbol, so no test can be keyed at all. A cache built
on this today would miss on 100% of tests: safe, and useless.

The names that fail to resolve, by frequency, say why:

```
   51x  new
   48x  serde::{Deserialize, Serialize}
   48x  write
   47x  anyhow::Result
   46x  std::path::{Path, PathBuf}
```

Two different problems sitting in one list.

`serde`, `anyhow` and `std` are crates outside the tree. The index was never
going to hold them, and their contents do not change between two runs on one
machine unless the toolchain or the lockfile changes. Treating them as
unresolved is correct today, because the key does not cover them, but the fix is
not to resolve them into the index: it is to make the toolchain and lockfile part
of the key, and stop counting them as gaps.

`new` and `write` are the harder half, and they are not external at all. They are
ambiguous: many indexed symbols answer to those names, so resolution returns
nothing rather than picking one. Resolving a bare method name to one definition
needs type information the line-based parsers do not have. Until that is
answered, any file declaring a `new` cannot be keyed, which on a Rust tree is
most of them.

**The dangerous direction is not zero.** 322 symbol/test pairs where the blast
radius selects a test whose closure omits the symbol. Some of that is the same
unresolved-name problem seen from the other side. It has to reach zero, on more
than one repository, before anything is allowed to skip a test on the strength of
a key.

## What would come next

In order, each gated on the one before:

1. Split "unresolved" into "outside the tree" and "ambiguous inside it". Fold the
   first into the key as a toolchain and lockfile digest instead of a gap. That
   alone should take usable keys from zero to something.
2. Resolve ambiguous short names, which needs more than the current parsers do.
3. Re-run the audit. If `would wrongly skip` is zero on several real trees, build
   the cache behind a flag, still shadowed, and compare its decisions against
   real runs.
4. Only then let it skip anything.

The measurement stays in the repository either way, because the number moves when
the graph changes, and a cache is exactly the feature that must not be built on a
number nobody re-checked.
