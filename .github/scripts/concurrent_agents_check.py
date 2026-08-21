"""Run several agents against one workspace and fail if any of them loses work.

The suite covers concurrency through library calls. This drives the real binary
the way an agent does, because the bugs that took longest to find in this
repository lived between processes rather than inside one: two servers both
wrote the index whole and the second silently dropped the first one's node.

Two scenarios run here.

`uniform` is the original: N agents each apply one mutation. It covers the
read-modify-write on the index and the operation log.

`mixed` adds scanners. A scan rewrites the index for a whole tree, so it is the
operation with the widest write, and it runs beside agents persisting single
symbols. This is the shape that broke before, and the uniform scenario does not
reach it.

Both are repeated. A race that is lost only sometimes is missed by a single
run: with the merge deliberately removed, one measured run in three still
reported every symbol intact. REPEATS is what turns this from a coin flip into
a gate, and any repeat losing work fails the check.
"""

import json
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

AGENTS = 6
MUTATORS = 8
SCANNERS = 3
REPEATS = 5

INIT = ('{"jsonrpc":"2.0","id":1,"method":"initialize","params":'
        '{"protocolVersion":"2024-11-05","capabilities":{},'
        '"clientInfo":{"name":"a","version":"1"}}}')


def new_workspace(binary):
    """A scanned workspace with one symbol in it."""
    work = Path(tempfile.mkdtemp())
    (work / "src").mkdir()
    (work / "src" / "lib.rs").write_text(
        "pub fn validate_token(t: &str) -> bool {\n    t.len() > 10\n}\n"
    )
    run(binary, work, "scan", "--path", ".")
    return work


def run(binary, cwd, *args):
    # The binary prints UTF-8; decoding with the platform default fails on a
    # Windows runner, where it is cp1252.
    return subprocess.run([binary, *args], cwd=cwd, check=True, capture_output=True,
                          text=True, encoding="utf-8", errors="replace")


def mutation_call(index):
    return json.dumps({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "axiom_apply_mutation", "arguments": {
            "node_id": f"n{index}",
            "symbol_path": f"agent{index}::sym",
            "content": "fn f() {}",
        }},
    })


def read_nodes(work):
    raw = (work / ".axiom" / "index.json").read_text(encoding="utf-8")
    return json.loads(raw)["nodes"]


def uniform(binary):
    """N agents mutate at once. Every symbol and every operation must survive."""
    work = new_workspace(binary)
    run(binary, work, "symbol", "--path", "validate_token")
    run(binary, work, "blast-radius", "--symbol", "validate_token")

    procs = []
    for i in range(AGENTS):
        p = subprocess.Popen([binary, "serve"], cwd=work, stdin=subprocess.PIPE,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             text=True, encoding="utf-8", errors="replace")
        p.stdin.write(INIT + "\n" + mutation_call(i) + "\n")
        p.stdin.close()
        procs.append(p)
    for p in procs:
        p.wait()

    nodes = read_nodes(work)
    kept = [i for i in range(AGENTS) if f"agent{i}::sym" in nodes]
    ops = json.loads((work / ".axiom" / "crdt_ops.json").read_text(encoding="utf-8"))

    problems = []
    if len(kept) != AGENTS:
        lost = [i for i in range(AGENTS) if i not in kept]
        problems.append(f"agents lost work, kept {len(kept)} of {AGENTS}, lost {lost}")
    if len(ops) != AGENTS:
        problems.append(f"operation log incomplete: {len(ops)} of {AGENTS}")
    return f"{len(kept)}/{AGENTS} symbols, {len(ops)}/{AGENTS} ops", problems


def mixed(binary):
    """Mutators and scanners at once, started together.

    A scan writes the index for the whole tree. If it wrote its own view whole
    instead of merging, it would carry off whatever a mutator recorded between
    the scan's read and its write.
    """
    work = new_workspace(binary)
    start = threading.Barrier(MUTATORS + SCANNERS)
    problems = []

    def mutator(i):
        start.wait()
        p = subprocess.Popen([binary, "serve"], cwd=work, stdin=subprocess.PIPE,
                             stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                             text=True, encoding="utf-8", errors="replace")
        _, err = p.communicate(INIT + "\n" + mutation_call(i) + "\n")
        if p.returncode != 0:
            problems.append(f"mutator {i} exited {p.returncode}: {err[:200]}")

    def scanner(i):
        start.wait()
        try:
            run(binary, work, "scan", "--path", ".")
        except subprocess.CalledProcessError as e:
            problems.append(f"scanner {i} failed: {e.stderr[:200]}")

    threads = [threading.Thread(target=mutator, args=(i,)) for i in range(MUTATORS)]
    threads += [threading.Thread(target=scanner, args=(i,)) for i in range(SCANNERS)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    nodes = read_nodes(work)
    kept = [i for i in range(MUTATORS) if f"agent{i}::sym" in nodes]
    if len(kept) != MUTATORS:
        lost = [i for i in range(MUTATORS) if i not in kept]
        problems.append(
            f"a scan carried off mutations: kept {len(kept)} of {MUTATORS}, lost {lost}"
        )
    # The scan's own symbol must still be there: subtracting what a re-scan
    # purged must not subtract what it just found.
    if not any("validate_token" in name for name in nodes):
        problems.append("the scanned symbol is missing from the index")
    return f"{len(kept)}/{MUTATORS} mutations survived {SCANNERS} scans", problems


def main(binary: str) -> int:
    failures = []
    for name, scenario in (("uniform", uniform), ("mixed", mixed)):
        for attempt in range(1, REPEATS + 1):
            summary, problems = scenario(binary)
            status = "ok" if not problems else "FAILED"
            print(f"{name} {attempt}/{REPEATS}: {summary} [{status}]")
            for problem in problems:
                failures.append(f"{name} repeat {attempt}: {problem}")

    for problem in failures:
        print("FAIL:", problem, file=sys.stderr)
    if not failures:
        print(f"no work lost across {REPEATS} repeats of each scenario")
    return 1 if failures else 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: concurrent_agents_check.py <path to axiom binary>")
    sys.exit(main(sys.argv[1]))
