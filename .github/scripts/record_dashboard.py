#!/usr/bin/env python3
"""Record `axiom dashboard` watching six agents, as an animated SVG.

The README shows this recording, so it is produced here rather than drawn by
hand: a scratch workspace, real `axiom serve` sessions for every write and
attestation, and after each step one frame from `axiom dashboard --once
--color always`, the same renderer the live view uses. Every frame is what the
dashboard printed; the only edit is the workspace path in the header, shown as
~/shop so the file carries no local path.

    python .github/scripts/record_dashboard.py --binary target/release/axiom \
        --out docs/images/dashboard.svg

The SVG plays with CSS keyframes and no script, which GitHub renders inside an
<img>. Re-run it when the dashboard's layout changes.
"""

import argparse
import html
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", default="target/release/axiom")
parser.add_argument("--out", default="docs/images/dashboard.svg")
parser.add_argument("--columns", type=int, default=104)
args = parser.parse_args()

binary = Path(args.binary)
if not binary.exists() and binary.with_suffix(".exe").exists():
    binary = binary.with_suffix(".exe")
axiom = str(binary.resolve())
COLUMNS = args.columns

LIB = b"""pub fn helper(x: u32) -> u32 {
    x + 1
}

pub fn middle(x: u32) -> u32 {
    helper(x) * 2
}

pub fn outer(x: u32) -> u32 {
    middle(x) + helper(x)
}

pub fn target(x: u32) -> u32 {
    let a = x + 1;
    let b = a * 2;
    let c = b - 3;
    c
}

pub fn parse(s: &str) -> u32 {
    s.len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_adds_one() {
        assert_eq!(helper(1), 2);
    }

    #[test]
    fn middle_doubles() {
        assert_eq!(middle(1), 4);
    }

    #[test]
    fn outer_sums() {
        assert_eq!(outer(1), 6);
    }

    #[test]
    fn target_counts() {
        assert_eq!(target(1), 1);
    }
}
"""

INIT = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "rec", "version": "1"}}})

scratch = Path(tempfile.mkdtemp())
work = scratch / "shop"
(work / "src").mkdir(parents=True)
(work / "src" / "lib.rs").write_bytes(LIB)
subprocess.run([axiom, "scan", "--path", "."], cwd=work, check=True, capture_output=True)


def session(*calls):
    lines = [INIT] + [json.dumps({"jsonrpc": "2.0", "id": i + 2, "method": "tools/call",
                                  "params": {"name": n, "arguments": a}})
                      for i, (n, a) in enumerate(calls)]
    out = subprocess.run([axiom, "serve"], cwd=work, input="\n".join(lines) + "\n",
                         capture_output=True, text=True, encoding="utf-8", check=True).stdout
    replies = []
    for line in out.splitlines():
        if not line.strip():
            continue
        message = json.loads(line)
        if message.get("id", 0) >= 2:
            replies.append(json.loads(message["result"]["content"][0]["text"]))
    return replies


def read(symbol):
    return session(("axiom_query_symbol", {"symbol_path": symbol}))[0]["source_text"]


def write(agent, symbol, base, old, new, expect):
    reply = session(("axiom_apply_mutation", {
        "symbol_path": symbol, "write_source": True, "agent_identity": agent,
        "base_content": base, "content": base.replace(old, new)}))[0]
    if reply.get("status") != expect:
        raise SystemExit(f"{agent} on {symbol}: expected {expect}, got {reply}")


def attest(agent, task, symbol):
    session(("axiom_record_verification", {"task_id": task, "passed": True, "command": "cargo test"}),
            ("axiom_attest_commit", {"prompt": f"work {task}", "symbol_path": symbol,
                                     "ctop_task_id": task, "agent_identity": agent}))


ANSI = re.compile(r"\x1b\[([0-9;]*)m")


def capture():
    out = subprocess.run([axiom, "dashboard", "--symbol", "helper", "--depth", "2",
                          "--once", "--color", "always"],
                         cwd=work, capture_output=True, text=True, encoding="utf-8", check=True,
                         env=dict(os.environ, COLUMNS=str(COLUMNS))).stdout
    lines = out.rstrip("\n").split("\n")
    # The header is `axiom  <path> ... <clock>`, right-aligned to the width.
    # Swap the path and pad it again so the clock stays at the right edge.
    plain = ANSI.sub("", lines[0])
    clock = plain.split()[-1]
    title = f" axiom  ~/shop"
    lines[0] = "\x1b[1m" + title + " " * max(1, COLUMNS - len(title) - len(clock) - 1) + clock + " \x1b[0m"
    return lines


frames = []


def step(caption, hold=3.2):
    time.sleep(2)
    frames.append((caption, capture(), hold))


# Everyone reads before anyone writes, so later writers hold a stale read.
t0, p0, h0 = read("target"), read("parse"), read("helper")
frames.append(("A scanned workspace. Nobody has written yet", capture(), 2.6))

write("alice", "target", t0, "x + 1", "x + 10", "WRITTEN")
step("alice changes target: one agent, green")
write("dave", "parse", p0, "s.len()", "s.trim().len()", "WRITTEN")
step("dave changes parse")
write("bob", "target", t0, "x + 1", "x + 20", "CONFLICT")
step("bob changes the same line of target from a stale read: refused, red", 4)
write("carol", "target", t0, "b - 3", "b - 30", "WRITTEN")
step("carol changes another line from the same stale read: merged, but bob's change is still out", 4)
attest("alice", "t1", "target")
attest("dave", "t2", "parse")
attest("carol", "t3", "target")
step("three attestations go on the ledger, each linked to the one before")
write("bob", "target", read("target"), "x + 10", "x + 20", "WRITTEN")
step("bob redoes his change on what the file holds now: it lands, target turns yellow", 4)
write("erin", "helper", h0, "x + 1", "x + 2", "WRITTEN")
write("frank", "helper", h0, "x + 1", "x + 3", "CONFLICT")
step("erin and frank edit helper from one read: frank is refused", 4)
attest("erin", "t4", "helper")
ledger = work / ".axiom" / "attestations.json"
rows = [line for line in ledger.read_text(encoding="utf-8").splitlines() if line.strip()]
ledger.write_bytes(("\n".join(rows[:1] + rows[2:]) + "\n").encode("utf-8"))
step("a ledger record is deleted: the chain breaks where it was", 7)
shutil.rmtree(scratch, ignore_errors=True)

# Colours of GitHub's dark theme, keyed by the SGR codes the dashboard emits.
FG, BG = "#c9d1d9", "#0d1117"
STYLE = {"1": ("#f0f6fc", True), "31": ("#ff7b72", False), "32": ("#3fb950", False),
         "33": ("#d29922", False), "90": ("#8b949e", False)}
CW, LH, FS = 8.4, 19, 14
PAD, TOP = 16, 44
rows_max = max(len(lines) for _, lines, _ in frames)
width = round(PAD * 2 + COLUMNS * CW)
height = TOP + rows_max * LH + 48


CHUNK = re.compile(r"[^ ]+(?: [^ ]+)*")


def spans(line, y):
    """One <text> per line, and every stretch of text between two or more
    spaces placed at its own column. A font draws a space, a bullet or a box
    glyph at its own width rather than a terminal cell's, so text laid out
    by flow drifts; placed by column, the dashboard's columns stay columns."""
    out, col, style, at = [], 0, None, 0
    for m in list(ANSI.finditer(line)) + [None]:
        end = m.start() if m else len(line)
        text = line[at:end]
        if text:
            fill, bold = STYLE.get(style, (FG, False))
            weight = ' font-weight="bold"' if bold else ""
            for chunk in CHUNK.finditer(text):
                out.append(f'<tspan x="{PAD + (col + chunk.start()) * CW:.1f}" fill="{fill}"{weight}>'
                           f"{html.escape(chunk.group())}</tspan>")
            col += len(text)
        if m:
            code = m.group(1)
            style = None if code in ("", "0") else code
            at = m.end()
    return f'<text y="{y}" xml:space="preserve">{"".join(out)}</text>' if out else ""


total = sum(hold for _, _, hold in frames)
css, groups, start = [], [], 0.0
for i, (caption, lines, hold) in enumerate(frames):
    a, b = start / total * 100, (start + hold) / total * 100
    keys = f"0%{{opacity:0}}{a:.3f}%{{opacity:1}}{b:.3f}%{{opacity:0}}" if i else \
        f"0%{{opacity:1}}{b:.3f}%{{opacity:0}}"
    css.append(f"@keyframes f{i}{{{keys}}}.f{i}{{animation:f{i} {total:.1f}s step-end infinite}}")
    body = "".join(spans(line, TOP + (n + 1) * LH) for n, line in enumerate(lines))
    cap = (f'<text x="{PAD}" y="{height - 18}" fill="#58a6ff">'
           f"{i}/{len(frames) - 1}  {html.escape(caption)}</text>")
    groups.append(f'<g class="f f{i}">{body}{cap}</g>')
    start += hold

svg = f"""<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="axiom dashboard recording: six agents write one workspace; conflicts turn rows red, a deleted ledger record breaks the chain">
<style>text{{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,'Liberation Mono',monospace;font-size:{FS}px}}.f{{opacity:0}}{''.join(css)}</style>
<rect width="{width}" height="{height}" rx="8" fill="{BG}" stroke="#30363d"/>
<circle cx="20" cy="18" r="6" fill="#ff5f56"/><circle cx="40" cy="18" r="6" fill="#ffbd2e"/><circle cx="60" cy="18" r="6" fill="#27c93f"/>
<text x="{width / 2:.0f}" y="23" fill="#8b949e" text-anchor="middle">axiom dashboard --symbol helper --depth 2</text>
{''.join(groups)}
</svg>
"""
Path(args.out).parent.mkdir(parents=True, exist_ok=True)
Path(args.out).write_bytes(svg.encode("utf-8"))
print(f"wrote {args.out}: {len(frames)} frames, {total:.1f}s loop, {len(svg.encode('utf-8'))} bytes")
