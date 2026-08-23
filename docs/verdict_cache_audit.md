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

**Do not build the cache yet.** The first measurement below is the one that said
so; the section after it is what changed. Not because the idea is wrong, but
because the precondition failed on the graph as it stood. Two separate things are failing,
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

## After over-approximating ambiguous names

```
 Tests in index:             51
 Tests with a usable key:    51 of 51
 Keyed without guessing:     2 of 51
 Extra symbols dragged in:   812

 Both mechanisms agree:      670
 Cache would wrongly skip:   0
 Cache would run unselected: 3895
 Agreement:                  14.68%
```

An ambiguous name is now over-approximated rather than guessed: the closure
depends on every symbol that could answer to it. The real target is among them
whenever it is in the index, so nothing is missed. That took usable keys from
1 of 51 to 51 of 51 and the dangerous disagreement from 312 to zero.

Choosing the nearest candidate instead, by file or by directory, was the obvious
alternative and is unsafe. A wrong pick produces a key that looks complete and
omits the dependency that moved, which is the one failure this whole exercise is
about. Only two of 51 closures resolve without guessing, so that choice would
have been made 49 times.

### Two reasons not to read that zero as a green light

**It is partly structural.** The blast radius walks `reverse_deps` and the
closure walks the same edges forward, over-approximated. If the reverse walk says
a test reaches a symbol, the forward walk from that test will reach it too. So
zero says the two mechanisms agree, not that either is right about the real
dependency graph. A call the parsers never recorded is invisible to both, and
this measurement cannot see it. That is the residual risk, and it is not small
on line-based parsers.

**It currently saves nothing over selection.** Across 284 symbols and 51 tests,
the blast radius selects about 2.4 tests per symbol, and about 16 tests per
symbol have a key that changes. Because the dangerous count is zero, every
selected test also has a changed key, so for a single-symbol edit the cache skips
nothing the selector was going to skip anyway.

Where it would earn its place is the case selection cannot serve: a change whose
extent is not known symbol by symbol, such as a merge or a pull. There, about 16
of 51 keys move and the remaining 35 verdicts still hold, which is a two-thirds
saving with no symbol named. That is worth having, and it is a different claim
from the one the pruning numbers make.

## A second tree, which does not agree

The first repository this ran on is the one it was written in, and the caveat
above says that is one measurement. Running it on a small four-file polyglot
fixture gives a different answer:

```
 Tests in index:             4
 Tests with a usable key:    4 of 4
 Keyed without guessing:     4 of 4
 Cache would wrongly skip:   1
 Agreement:                  85.71%
```

The pair it names is a Python test method and the class that encloses it:

```
   billing.py::BillingTest -> billing.py::BillingTest::test_total
```

The blast radius says changing the class reaches the test inside it, which is
right. The forward closure of that test does not contain its own enclosing type,
because containment is not a call and nothing records it as an edge. So the
closure is missing a real dependency, and a cache keyed on it would skip a test
whose class had changed.

That is a different gap from the ones above, it did not appear on this
repository, and it is exactly why the conclusion is not "zero here, therefore
safe". A second tree found a second hole within minutes.

Alongside it: `crate::auth::validate_token` was reported as a name from outside
the tree, and it is not, it is `auth.rs::validate_token` written the way Rust
writes it. Suffix matching did not connect the two, so an in-tree dependency was
folded into the environment key, which is the unsound direction: editing it would
not have moved the key. It was harmless in that fixture only because the test
also calls `validate_token` by its bare name and that edge does resolve.

### Both are now fixed, and the numbers moved

A method's closure contains the type enclosing it, and `crate::`, `self::` and
`super::` paths resolve here rather than counting as outside. The fallback is
restricted to those three prefixes on purpose: matching any colonned name by its
last segment would let `anyhow::Result` bind to a local `Result` and stop being
covered by the environment, which is the same unsoundness pointing the other way.

```
                        fixture before   fixture after   this repo after
 Usable keys              4 of 4          4 of 4          51 of 51
 Would wrongly skip       1               0               0
 Agreement                85.71%          87.50%          15.63%
 Extra symbols                0               0            1,111
```

**Read the two zeros carefully.** The containment edge was a real missing
dependency, and adding it also moved the closure closer to what the blast radius
already believed. The audit measures agreement between two readings of one graph,
so making one match the other raises agreement without establishing that either
is right about the code. The structural caveat below still applies, and it is now
the main thing standing between this and a cache.

## What would come next

In order, each gated on the one before:

1. ~~Split "unresolved" into "outside the tree" and "ambiguous inside it". Fold
   the first into the key as a toolchain and lockfile digest instead of a gap.~~
   Done. Usable keys went from 0 to 1, which is the honest size of that step.
2. ~~Resolve ambiguous short names, which needs more than the current parsers
   do.~~ Answered a different way: over-approximate instead of resolving. The
   cache wants the opposite bias from selection, so it does not need the type
   information selection would.
3. ~~Teach the closure that a method depends on its enclosing type, and resolve
   `crate::`-prefixed paths to the symbols they name.~~ Done, and both were
   found by running the audit on a second tree rather than by reasoning.
4. Run the audit on more real trees still. Two is not many, and each of the two
   so far produced a finding the other did not.
5. Find the references the parsers never recorded. This audit cannot: both
   directions read the same edges, so a missing edge is invisible to it. That
   needs a different check, comparing against a real test run.
6. Only then let it skip anything.

The measurement stays in the repository either way, because the number moves when
the graph changes, and a cache is exactly the feature that must not be built on a
number nobody re-checked.
