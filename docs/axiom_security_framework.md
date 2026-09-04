# Axiom security model: what contains an agent, and what does not

This document is in two parts, and the split is the point.

**Part 1 is what is built.** It is written in the present tense because it
describes code you can read and behaviour you can reproduce.

**Part 2 is the design that is not built.** It is written in the conditional
because none of it runs. An earlier version of this document described the whole
design in the present tense, with a threat table marking four attack vectors
**PREVENTED** by microVMs, seccomp profiles and an intercepting proxy that do not
exist. A security document that overstates containment is worse than no document:
it invites someone to run untrusted code through a tier that would not hold it.

The one-line summary, before any of the detail: **an agent talking to this server
has the same access to the host that the axiom process has.** Two things are
genuinely enforced, an environment allowlist and a wall-clock deadline, and they
are described below. Nothing else confines what a snippet can do.

---

## Part 1: What is built

### The evaluator has two tiers, and only one of them is a sandbox

**Tier 1, WebAssembly under wasmtime.** A `.wat` or `.wasm` snippet is compiled
by Cranelift and run in a wasmtime instance with a fuel limit and no WASI host
functions bound. It cannot open a file, make a syscall, or reach the network,
because nothing is there for it to call. A snippet that loops forever exhausts
its fuel and traps. This tier is a sandbox in the ordinary sense of the word.

**Tier 2, the real toolchain.** Rust goes to `rustc`, and Python, JavaScript,
TypeScript, Go, Java, Kotlin and Scala go to their own compilers and
interpreters. Each runs as an ordinary child process **with the privileges of the
axiom process itself**. There is no virtualisation, no namespace, no seccomp
filter, no network-egress restriction, and no CPU or memory bound. A Python
snippet can open a socket, read your home directory and write to disk, because
`python` can.

Set `AXIOM_EVAL_NATIVE=off` to refuse tier 2 entirely. With it off, a request to
evaluate anything outside WebAssembly returns `EVALUATOR_UNAVAILABLE` rather than
running.

### The two things tier 2 does enforce

**A confined environment.** `confine_environment` in `axiom-vmm/src/native.rs`
clears the child's environment and passes through only an allowlist of names and
prefixes a toolchain needs (`PATH`, `HOME`, `TMPDIR`, the `CARGO_*` and `JAVA_*`
families, and so on), plus whatever `AXIOM_EVAL_ENV_PASS` names. Two variables
are refused even when explicitly passed: `AXIOM_SIGNING_KEY` and
`AXIOM_SIGNING_KEY_FILE`.

That refusal exists because it was not always there. A Python snippet read the
signing key straight out of `os.environ`, and the value came back in the
evaluation report, which hands whoever reads the report the key that signs
provenance records. The toolchain probe and the version fingerprint run under the
same confinement, so a toolchain that needs a variable the allowlist drops reads
as missing rather than as a failing snippet. `crates/axiom-vmm/tests/child_environment.rs`
pins it. A new variable a toolchain needs goes in `PASSED_NAMES` or
`PASSED_PREFIXES`, never by widening the two refused names.

**A deadline that kills the process tree.** Every evaluation is bounded by
`AXIOM_EVAL_TIMEOUT_SECS` (default 30). `run_with_timeout` puts the child in its
own process group on Unix and kills the group; on Windows it uses `taskkill /T`.
Killing only the child was not enough: `go run`, the `kotlin` launcher and a
`Popen` started from inside a Python snippet all outlived it. Output pipes are
drained for a bounded grace after the child exits rather than to EOF, because a
surviving grandchild holds them open, and `Finished.drained` records when output
may be short for that reason. `crates/axiom-vmm/tests/process_tree.rs` pins it.

**A cached artifact is checked before it runs.** The compile step of an
evaluation is content-addressed: byte-identical source under the same toolchain
restores the artifact built last time instead of compiling again. The entry
records a BLAKE3 digest of every stored file, and each is re-read and checked
before anything is written into the work directory, so an entry edited or
truncated on disk is purged and the snippet recompiled rather than executed from
the cache. Nothing in the cache is a verdict: a hit still runs the artifact.
`AXIOM_EVAL_CACHE=off` disables it. `crates/axiom-vmm/tests/artifact_cache.rs`
pins the fails-again and tampered cases.

`axiom_run_tests` runs the project's own test command under the same confinement
and the same process-tree kill, bounded by `AXIOM_TEST_TIMEOUT_SECS` (default
600).

### A verdict is never invented

The failure mode this codebase treats as most serious is not an escape, it is a
confident wrong answer. An agent acts on a `PASSED` without checking it.

* A language with no evaluator, a toolchain that is not on `PATH`, a symbol name
  matching several symbols, or a temp directory that cannot be written returns
  `EvaluatorUnavailable` with `passed_checks_count: 0`. Never `PASSED`.
* An earlier version fell back to matching assertion substrings when it could not
  run anything, and reported success for code that never executed. That fallback
  is gone. `test_e2e_truth_preserving_assertions` and
  `crates/axiom-cli/tests/multi_language_eval.rs` guard it.
* A non-zero exit is not automatically a verdict either. The `scala` and JVM
  launchers fetch their compiler on first use, and a failed fetch exits non-zero
  having executed nothing the caller wrote. `toolchain_failure_reason` matches
  resolver and downloader failures and turns those into `EvaluatorUnavailable`,
  because reporting `FAILED` there tells an agent its code is wrong on the
  strength of a network error.
* Java and Kotlin run with assertions enabled (`-ea`, `-J-ea`). Without that flag
  a false `assert` is a no-op, the snippet exits zero, and axiom reports `PASSED`
  for code that failed its own check. Scala needs no flag, because
  `Predef.assert` throws unconditionally.

### The AST store is content-addressed, and the ledger is an ordinary file

Every indexed node is BLAKE3-hashed over its declaration and body, and the tree
has a Merkle root that moves when any node's hash moves. A mutation produces a
new root rather than overwriting the old one.

Two limits worth stating plainly. Parsing is line-based heuristics per language,
not Tree-sitter, unless you supply a SCIP index. And `.axiom/index.json` and
`.axiom/attestations.json` are ordinary files: anything with write access to the
workspace can edit them. What stops an edit going unnoticed is the seal and the
chain, described next, not the filesystem.

### The provenance record

`axiom_attest_commit` writes a record tying together the prompt, the symbol, the
check that verified it, both Merkle roots, the caller's claimed identity, and the
time. `seal_over` in `axiom-proto` hashes ten fields, each length-prefixed:

```
parent_merkle_root, commit_merkle_root, agent_identity, symbol_path,
ctop_proof_hash, verified_by, verification_detail, timestamp,
previous_seal, prompt
```

Length-prefixing matters: without it, two different field splits could hash the
same. The seal once covered five of those ten, and editing `verified_by` from
`reported` to `sandbox` in an unsigned ledger left a record that still printed
VALID, which forges the whole distinction the record exists to carry.
`crates/axiom-core/tests/seal_covers_the_record.rs` pins one edited-field-fails
case per field. **Any new stored field on `ProvenanceAttestation` has to be added
to `seal_over`, or it is forgeable.**

`previous_seal` is what makes the ledger a chain. A signature stops a record
being forged or edited; it does nothing about one being removed, since what is
left still verifies and the history simply looks shorter. Each record naming its
predecessor's seal means a deletion leaves the next record pointing at something
that is not there, and `verify` reports it. The chain cannot catch truncation at
the tail, because nothing points at the last record. Catching that needs the
expected head stored where whoever can write the ledger cannot reach.

Signing is optional and separates two claims. The seal shows a record has not
been altered, and says nothing about who wrote it: anyone with the same inputs
recomputes it. An Ed25519 signature over the record plus the symbol and prompt
says a particular key issued it, and cannot be moved onto a different record.
`axiom verify --trusted-key <pub>` requires a signature from the key you name; an
unsigned record does not satisfy it, because producing an unsigned record takes
no key at all.

**What the record does not establish.** It does not prove the code is correct, it
does not rebuild anything, and it does not establish that a build was hermetic.
It is not SLSA at any level, and the phrase "SLSA Level 4+" that used to appear in
this document was wrong. It records that a particular prompt, symbol and check
were seen together on one machine.

### Three kinds of check, and the difference between them is the point

| Kind | Means | Axiom can vouch for it |
|---|---|---|
| `sandbox` | Axiom compiled and ran the code itself | Yes |
| `executed` | Axiom ran the project's test command and saw the exit code | Yes |
| `reported` | An agent ran something and told axiom the outcome | No |

`axiom verify` prints which one a record rests on, and says in words that axiom
did not run a `reported` check. A record is only issued against a check that
happened and passed; naming a check the server has no record of, or one that
failed, is refused.

`agent_identity` is whatever the caller asked to be recorded as, and axiom does
not check it. It is worth storing anyway because it is hashed into the seal and
covered by the signature, so it cannot be edited afterwards, and on a signed
record it is bound to the issuing key. Control characters and over-long values
are refused where the value enters, because the value is printed by `axiom verify`
as one of a column of labelled lines, and a newline inside it could add a line of
its own claiming a check that did not happen.

---

## Part 2: The design, which is not built

The figure below is the containment model the design aims at. Layer 1 does not
exist. Layer 2 exists only as tier 1 above, the WebAssembly engine; the microVM
half of it is not built. Layer 3 exists in the hashing sense and not in the
Tree-sitter or immutability sense.

```
+-------------------------------------------------------------------------+
|  LAYER 1: ZERO-TRUST INTERCEPTING PROXY            [NOT BUILT]          |
|  (Would sanitize LLM tool calls, paths, command chaining)               |
|                                                                         |
|    +---------------------------------------------------------------+    |
|    |  LAYER 2: EPHEMERAL SANDBOX                                   |    |
|    |  Tier 1 WASI engine                        [BUILT]            |    |
|    |  KVM microVM, zero egress, seccomp         [NOT BUILT]        |    |
|    |                                                               |    |
|    |    +-----------------------------------------------------+    |    |
|    |    |  LAYER 3: MERKLE AST CORE              [PARTLY]     |    |    |
|    |    |  BLAKE3 content addressing: built                   |    |    |
|    |    |  Tree-sitter parsing: not built (line heuristics)   |    |    |
|    |    |  Immutable store: it is a JSON file                 |    |    |
|    |    +-----------------------------------------------------+    |    |
|    +---------------------------------------------------------------+    |
+-------------------------------------------------------------------------+
```

**Layer 1, an intercepting proxy.** Everything between the agent, the model API
and the MCP server would pass through a proxy that validates tool schemas,
normalises paths and strips command chaining out of mutation payloads. Nothing
sits between an agent and the server today.

**Layer 2's second tier, a microVM snapshot engine.** Dynamic languages and JVM
runtimes would run in a Firecracker or KVM microVM with a stripped kernel, no
virtual bridge interface, `AF_VSOCK` for host-guest IPC, and a seccomp profile
dropping unneeded syscalls. None of that exists; those languages run as ordinary
child processes today. The `<15ms` figure that used to be attached to this tier
was never measured against anything.

**Layer 3, an immutable store.** Nodes would be parsed by Tree-sitter into
normalised AST nodes and held in a store that cannot be edited in place. Today
the parsing is heuristic and the store is a JSON file.

---

## Threat model: what actually holds today

The columns say what a threat does against axiom **as built**. Where the design
would change the answer, the last column says so.

| Threat | Against axiom as built | Would change if Layer 1 and the microVM tier existed |
|---|---|---|
| Malicious dependency executed during evaluation | **Not contained.** A tier 2 snippet runs with the axiom process's privileges and can reach the network. Only WebAssembly is contained. | Yes: a zero-egress microVM would stop it communicating out. |
| Command injection through a prompt | **Not contained.** Nothing sanitises tool calls or paths. Axiom does not itself expose a shell tool, which narrows the surface, but `axiom_run_tests` runs a caller-supplied command and `axiom_eval_patch` runs caller-supplied code. | Yes: this is exactly what Layer 1 is for. |
| Reading the signing key out of the evaluation environment | **Contained.** `AXIOM_SIGNING_KEY` and `AXIOM_SIGNING_KEY_FILE` are refused to every child, including the probe. | No change; already enforced. |
| A snippet that never terminates | **Contained.** Wall-clock deadline, whole process tree killed. | No change; already enforced. |
| Editing a provenance record after the fact | **Detected, not prevented.** Every stored field is covered by the seal, and the signature covers the seal. Anyone with write access can still edit the file; `verify` fails on it. | No change. |
| Deleting a provenance record | **Detected, except at the tail.** The chain breaks visibly. Truncating the last record leaves a consistent chain. | No: needs an external anchor for the head, which no layer here provides. |
| Backdoor inserted in code | **Not addressed.** The record says a check was seen to pass. It does not establish that the code is correct or that the check was adequate. | No. |
| Host OS escape from an evaluation | **Not contained.** There is nothing to escape from: tier 2 already runs on the host. | Yes: that is the microVM's entire purpose. |

**The operating rule that follows from this table: do not put untrusted code
through tier 2.** Untrusted here means code from an untrusted prompt as much as
code from an untrusted repository. If you need to evaluate something you do not
trust, either restrict it to WebAssembly or set `AXIOM_EVAL_NATIVE=off` and run
it somewhere with real isolation.

---

## Auditing a record

```bash
axiom verify --symbol auth.service.validateToken \
             --prompt "Add JWT expiration validation" \
             --trusted-key ~/.config/axiom/agent.pub
```

This looks up the record for that symbol and prompt, recomputes the seal from the
record's stored fields together with the symbol and prompt being claimed, checks
the chain back through `previous_seal`, and checks the Ed25519 signature against
the key you named. It exits non-zero when the record is missing, when the seal
does not re-derive, when the chain is broken, or when the signature is absent or
from another key.

A seal that fails to re-derive does not say *why*, and `verify` does not pretend
it does. The prompt is not stored, only a digest covering it, so there is no
prompt-independent copy to compare against: a failure means either the prompt is
not the one the record was issued for, or a stored field has been edited since.
`verify` names both causes. A broken chain is the one piece of evidence that does
point specifically at tampering, and it is reported separately.
