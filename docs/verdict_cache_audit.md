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
 Tests in index:             51
 Tests with a usable key:    1 of 51
 Symbols audited:            283

 Both mechanisms agree:      358
 Cache would wrongly skip:   312
 Cache would run unselected: 3294
 Agreement:                  9.03%
```

Before out-of-tree names were separated from ambiguous ones, usable keys were
0 of 52. Separating them is necessary and nowhere near sufficient: the ambiguous
half dominates, and until it is answered the cache would still miss on every test
but one.

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

**Almost nothing has a usable key.** A test is keyable only when every name it
reaches was identified. The audit separates two reasons a name is not, because
they want different fixes.

Names from outside the tree no longer count against a key. `serde`, `anyhow` and
`std` were never going to be in the index, and what they mean is pinned by the
compiler and the lock file rather than by this tree, so they are folded into an
environment key instead:

```
 Environment key: env_e69a785d...
   Covering: Cargo.lock, deno=2.9.5, go=go1.24.5, javac=26,
             node=v26.5.0, python=Python 3.11.4
```

A `cargo update` rewrites `Cargo.lock`, a compiler upgrade changes a version
string, and either one changes that digest, which changes every key derived from
it. That is the correct blast radius for an environment change: nothing compiled
against the old one is still known to hold.

The fingerprints must be real for that argument to work. The first version reused
the evaluator's probe arguments, which are chosen to produce no output, so the
key contained `node=` and `python=` and an upgrade to either would have
invalidated nothing. `crates/axiom-vmm/tests/toolchain_fingerprints.rs` now fails
if an installed toolchain reports an empty version.

**Ambiguous names are what remain, and they are the real gap.** Many indexed
symbols answer to these, so resolution picks none:

```
   51x  new
   48x  write
   44x  drop
   32x  path
   21x  extract_tool_result
```

Something in this tree satisfies each of them and the graph cannot say what, so
nothing covers a change behind them. Resolving a bare method name to one
definition needs the receiver's type, which the line-based parsers do not have.
On a Rust tree most files declare a `new`, which is why this alone keeps almost
every test unkeyable.

**The dangerous direction is not zero.** 322 symbol/test pairs where the blast
radius selects a test whose closure omits the symbol. Some of that is the same
unresolved-name problem seen from the other side. It has to reach zero, on more
than one repository, before anything is allowed to skip a test on the strength of
a key.

## What would come next

In order, each gated on the one before:

1. ~~Split "unresolved" into "outside the tree" and "ambiguous inside it". Fold
   the first into the key as a toolchain and lockfile digest instead of a gap.~~
   Done. Usable keys went from 0 to 1, which is the honest size of that step.
2. Resolve ambiguous short names, which needs more than the current parsers do.
3. Re-run the audit. If `would wrongly skip` is zero on several real trees, build
   the cache behind a flag, still shadowed, and compare its decisions against
   real runs.
4. Only then let it skip anything.

The measurement stays in the repository either way, because the number moves when
the graph changes, and a cache is exactly the feature that must not be built on a
number nobody re-checked.
