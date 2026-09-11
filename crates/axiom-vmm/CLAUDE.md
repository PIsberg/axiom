# axiom-vmm

The evaluator. `src/native.rs` (1,776 lines) holds the toolchain recipes, `src/lib.rs` the tier
selection, `src/artifact_cache.rs` the compiled-output cache, `src/daemon.rs` the pool and its
telemetry, `src/sandbox.rs` the kernel-level containment.

Read this before touching a recipe, a timeout, the cache, or anything that decides a verdict.

## `axiom_eval_patch` must never return a verdict it did not earn

`execute_eval_in` picks a tier from the extension of the file the symbol was indexed from. A WAT
or wasm snippet goes to wasmtime Cranelift; Rust is written to a temp `.rs` and compiled with
`rustc`; everything the `LANGUAGES` table knows about goes to that language's own toolchain.

Anything else, a toolchain that is not on `PATH`, a name matching several symbols, or a temp
directory that cannot be written, is `EvaluatorUnavailable` with `passed_checks_count: 0`, never
`PASSED`. An earlier version fell back to matching assertion substrings and reported success for
code that never executed, which is the worst available failure for a tool an agent trusts.
`test_e2e_truth_preserving_assertions` and `axiom-cli/tests/multi_language_eval.rs` guard it, and
`tests/unearned_verdicts.rs` pins the WAT cases: a module that traps and a module without a `run`
export are both refused rather than passed.

**A non-zero exit is not a verdict.** `scala` and the JVM launchers fetch their compiler on first
use, and when that fetch fails they exit non-zero having executed nothing the caller wrote. That
reached CI as a failure after 134 seconds of failed downloads, which tells an agent its code is
wrong on the strength of a network error, and it is the same class as the assertion-substring
fallback: a verdict produced by something that is not a run of the code. `toolchain_failure_reason`
matches resolver and downloader failures only, and both the build and run steps turn those into
`EvaluatorUnavailable`. The markers stay narrow on purpose: mistaking a snippet's own output for a
broken toolchain costs a refusal, which says nothing was established and is true either way, while
widening them until they swallow real failures trades one wrong answer for another.

**Language is resolved through the symbol, not the caller's spelling.** `language_of_symbol`
resolves the name first, because comparing the caller's spelling against the stored keys returned
`None` for every short name, and `None` meant Rust. An ambiguous name is refused with
`AmbiguousSymbol` and its candidates rather than compiled as whichever language won.

## Tier 2 is not a sandbox, and the docs must not say it is

It runs the real compiler or interpreter with the process's own privileges, as the `rustc` tier
always did. `AXIOM_EVAL_NATIVE=off` refuses it, and `AXIOM_EVAL_TIMEOUT_SECS` (default 30) bounds
every command, because before that a snippet that did not terminate held the stdio pipe an agent
was blocked on.

`src/sandbox.rs` adds kernel-enforced containment on top: a Windows Job Object with
kill-on-close, a memory ceiling from `AXIOM_SANDBOX_MEMORY_LIMIT_MB` and a process ceiling from
`AXIOM_SANDBOX_MAX_PROCESSES`; on Unix a process group plus `RLIMIT_AS` and `RLIMIT_NPROC`. That
bounds what a runaway snippet can consume. It does not make the tier a security boundary, and
nothing here should claim it does.

## The environment is confined, and every tier's is

`confine_environment` clears the child's environment and passes only an allowlist of names and
prefixes a toolchain reads, plus whatever `AXIOM_EVAL_ENV_PASS` adds. `AXIOM_SIGNING_KEY` and
`AXIOM_SIGNING_KEY_FILE` are refused even there. Before this a snippet read the signing key
straight out of the process environment and the value came back in the report, which handed the
party the signature exists to check the key to sign anything.

The usability probe and the version fingerprint run under the same confinement, so a toolchain
that needs a dropped variable reads as missing rather than failing the snippet.
`tests/child_environment.rs` pins it. A new variable a toolchain needs goes in `PASSED_NAMES` or
`PASSED_PREFIXES`, never by widening the two refused names.

## A timeout ends the whole process tree, not just the child

`run_with_timeout` puts the child in its own process group on Unix and kills the group, and uses
`taskkill /T` on Windows, because `go run`, the `kotlin` launcher and a subprocess spawned from a
Python snippet all outlived a kill aimed at the child alone. The pipes are drained for a bounded
grace after the child exits rather than to EOF, because a surviving grandchild holds them open and
draining to EOF turned a two-second deadline into sixty. `Finished.drained` records when output
may be short for that reason. `tests/process_tree.rs` pins it.

`tests/eval_deadline.rs` passes its own two-second deadline to `native::evaluate` and never reads
the environment variable, so raising `AXIOM_EVAL_TIMEOUT_SECS` in CI does not weaken the guard.

## Compiled artifacts are cached; verdicts never are

`artifact_cache` keys the build step's output on the wrapped source, the shape of the build
command, the toolchain's reported version and the platform, and on a hit restores the artifact
into the work directory and skips the compiler. The artifact still runs, so a failing snippet
fails again from a hit and a nondeterministic one can still change its answer; a stored verdict
would be the assertion-substring fallback in a new form.

Every stored file's BLAKE3 digest is checked before reuse, so a tampered or truncated entry reads
as a miss and the snippet is recompiled, never run from the cache. Any cache failure degrades to a
miss, because the compiler can always answer what the cache cannot. `CtopReport::compile_cache`
reads `hit`, `miss`, or is absent for a language with no build step; `tests/artifact_cache.rs`
pins the fails-again and tampered cases.

`AXIOM_EVAL_CACHE=off` disables it, `AXIOM_EVAL_CACHE_DIR` moves it from `axiom-eval-cache` under
the system temp directory, and `AXIOM_EVAL_CACHE_MAX_MB` (default 512) caps it, pruned
least-recently-restored. Measured 2026-09-01 with `axiom bench --iterations 20` on Windows: median
220 ms with the cache off, 125 ms with it on; a hit still pays the restore, the spawn and the run.

`program_version` feeds both this key and the environment fingerprint, so the two cannot disagree
about which toolchain built an artifact.

## The pool holds no resident process

The native tier is reached through `DaemonPool::global().evaluate`. `warmup` primes the probe and
version caches and the cache root, nothing more, and `daemon.rs` says so in its module comment
because an earlier one promised pre-warmed sandboxes that did not exist. The saving comes from the
artifact cache, which the pool surfaces per language as `cache_hits`. The pool also owns eviction:
idle workers are dropped after `idle_evict_duration`, and `evict_idle` prunes the artifact cache to
its size cap at the same time.

## Per-language traps, each answered by running it

**Java runs under `-ea`.** Without it every `assert` is a no-op, so a false assertion exits zero
and reports `PASSED`, which is why `a_java_assertion_is_checked_with_assertions_enabled` asserts
on the failing case rather than the passing one.

**Kotlin shares the trap; Scala does not.** Kotlin's `assert` compiles to a check of the JVM's
assertion status, exactly as Java's does, so without `-J-ea` a false assertion is a no-op and the
snippet exits zero. Measured: an assertion of a false arithmetic identity printed the line after
it and returned success until the flag was passed. Scala's `assert` is `Predef.assert`, which
throws unconditionally, so no flag is needed and none is passed. A recipe copied from Java to
Scala would carry a flag that does nothing; one copied the other way would lose a flag that
decides whether a false assertion reports `PASSED`. Ask the question per language and answer it by
running it.

**C and C++ share the trap in a third spelling: `NDEBUG`.** C's `assert` is compiled out entirely
when `NDEBUG` is defined, so a false assertion becomes a no-op and the snippet exits zero.
Measured 2026-09-11 with gcc 14 on Windows: by default `assert(1 + 1 == 3)` aborted with a
non-zero status, and with `-DNDEBUG` it printed the line after the assertion and exited 0. The
recipes pass `-UNDEBUG`, and a later `-U` beats an earlier `-D`, so the assertion stays live even
if a flag arrives from elsewhere.

MSVC's `cl` is deliberately not a recipe. It reads `INCLUDE` and `LIB`, which `confine_environment`
strips, so it would read as a broken toolchain rather than a missing one. `cc`, `gcc` and `clang`
need nothing outside the existing allowlist: verified 2026-09-11 by compiling and running with only
PATH, PATHEXT, SYSTEMROOT, TEMP and TMP set.

**A Rust snippet without `fn main` is wrapped in one**, with a `validate_token` helper injected.

**A TypeScript snippet cannot assume Node's type declarations.** A `node:assert` import runs under
deno and is TS2591 under `tsc`, which has no Node type package, so the same snippet passes on one
machine and returns a compilation error on another. The portable form is a bare `throw`, which is
why `throw ` is in the language's `assertion_tokens`: without it a snippet written the documented
way reports `passed_checks_count: 0` beside `PASSED`.

## Windows resolves a bare program name by appending `.exe`, and npm does not ship one

`Command::new("tsc")` cannot see `tsc.cmd`, so a toolchain the user runs from their own shell was
reported as not installed. `resolve_program` searches PATHEXT. The order matters: npm drops
`deno.cmd`, `deno.ps1` and an extension-less `deno` holding a POSIX shell script, and matching the
bare name first finds the one Windows cannot execute. PATHEXT candidates win, and the bare name is
only considered when it already carries an extension.

## A cold JVM toolchain can outlast the evaluation deadline, and CI is where that shows

`scala` and `kotlinc` fetch their compiler on first use. Locally, warm, a Scala snippet evaluates
in about a second; on a fresh CI runner the first one spent 187 s downloading and was killed at
the 30 s deadline and reported as `TIMEOUT`. That verdict is correct, it says nothing is known
about the snippet, and it is useless to a caller who thinks their code hung. CI raises
`AXIOM_EVAL_TIMEOUT_SECS` to 300 so the tests measure the recipe rather than the download.

A bash warm-up step was the first attempt and was wrong for a Windows-specific reason worth
remembering: coursier installs `scala.bat`, which bash cannot find under the bare name, while
axiom's own PATHEXT lookup can. The step failed for a problem the product does not have. A user's
first Scala evaluation on a cold machine hits the same wall; the hint already names the variable.
This is the general shape to expect from this suite: a local green says nothing about a machine
with cold caches, which is why CI runs on two.

## A toolchain-conditional test can pass without running the thing it tests

`axiom-cli/tests/multi_language_eval.rs` branches on whether a toolchain is on PATH: with one it
asserts the verdict, without one it asserts the refusal. Both branches are green, so the suite
says nothing about which ran. The TypeScript recipe reached main that way, reasoned rather than
executed (#9). When touching one of these, break an assertion that only the running branch reaches
and confirm the test goes red. Doing exactly that is what found `resolve_program`: with `deno` and
`tsc` both installed, the test was still taking the refusal branch.

`every_language_has_a_toolchain_when_the_environment_promises_one` in
`tests/toolchain_fingerprints.rs` is the systematic answer. It iterates `native::languages()`,
derived rather than copied, and under `AXIOM_REQUIRE_TOOLCHAINS` (which CI sets) fails naming the
languages that had none. Unset locally, because a developer without kotlinc should not get a red
suite.
