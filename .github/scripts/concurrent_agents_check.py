"""Run several agents against one workspace and fail if any of them loses work.

The suite covers concurrency through library calls. This drives the real binary
the way an agent does, because the bugs that took longest to find in this
repository lived between processes rather than inside one: two servers both
wrote the index whole and the second silently dropped the first one's node.
"""

import json
import subprocess
import sys
import tempfile
from pathlib import Path

AGENTS = 6


def main(binary: str) -> int:
    work = Path(tempfile.mkdtemp())
    (work / "src").mkdir()
    (work / "src" / "lib.rs").write_text(
        "pub fn validate_token(t: &str) -> bool {\n    t.len() > 10\n}\n"
    )

    # The binary prints UTF-8; decoding with the platform default fails on a
    # Windows runner, where it is cp1252.
    def run(*args):
        return subprocess.run([binary, *args], cwd=work, check=True, capture_output=True,
                              text=True, encoding="utf-8", errors="replace")
    run("scan", "--path", ".")
    run("symbol", "--path", "validate_token")
    run("blast-radius", "--symbol", "validate_token")

    init = ('{"jsonrpc":"2.0","id":1,"method":"initialize","params":'
            '{"protocolVersion":"2024-11-05","capabilities":{},'
            '"clientInfo":{"name":"a","version":"1"}}}')

    procs = []
    for i in range(AGENTS):
        call = json.dumps({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "axiom_apply_mutation", "arguments": {
                "node_id": f"n{i}",
                "symbol_path": f"agent{i}::sym",
                "content": "fn f() {}",
            }},
        })
        p = subprocess.Popen([binary, "serve"], cwd=work, stdin=subprocess.PIPE,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             text=True, encoding="utf-8", errors="replace")
        p.stdin.write(init + "\n" + call + "\n")
        p.stdin.close()
        procs.append(p)
    for p in procs:
        p.wait()

    nodes = json.loads((work / ".axiom" / "index.json").read_text())["nodes"]
    kept = [i for i in range(AGENTS) if f"agent{i}::sym" in nodes]
    ops = json.loads((work / ".axiom" / "crdt_ops.json").read_text())

    print(f"agents that kept their symbol: {len(kept)} of {AGENTS}")
    print(f"crdt operations recorded:      {len(ops)} of {AGENTS}")

    failures = []
    if len(kept) != AGENTS:
        failures.append(f"agents lost work, kept {kept}")
    if len(ops) != AGENTS:
        failures.append(f"operation log incomplete: {len(ops)} of {AGENTS}")

    for problem in failures:
        print("FAIL:", problem, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: concurrent_agents_check.py <path to axiom binary>")
    sys.exit(main(sys.argv[1]))
