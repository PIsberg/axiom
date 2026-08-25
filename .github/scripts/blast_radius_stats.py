#!/usr/bin/env python3
"""Measure the blast radius over a scanned tree, symbol by symbol.

The README quotes numbers about how much of a suite the blast radius prunes.
Those numbers move with the graph, so they are produced here rather than
written down by hand: run this, and quote what it prints.

It asks the shipped CLI about every non-test symbol in `.axiom/index.json`,
counts the tests each one selects, and reports how many symbols reach a test at
all, how many tests they select, how much of the suite that prunes, and how much
two different symbols' answers overlap.

Two of those deserve reading carefully. A symbol that reaches no test is the
honest answer for a private helper nothing exercises directly, not a claim that
changing it is safe. And a low mean Jaccard says two symbols select different
tests, which is what makes the selection worth anything: a selector that always
returns the same tests prunes just as much and predicts nothing.

    axiom scan --path .
    python .github/scripts/blast_radius_stats.py --binary target/release/axiom

One subprocess per symbol, each loading the whole index, so a large tree wants
`--sample N`. It reads the index only to enumerate symbols and to tell a test
from a non-test; every selection comes from the CLI.
"""

import argparse
import json
import os
import random
import re
import statistics
import subprocess
import sys
from itertools import combinations

MAX_PAIRS = 200_000

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", default="target/release/axiom", help="the axiom binary to ask")
parser.add_argument("--path", default=".", help="a tree that has been scanned")
parser.add_argument("--depth", default="1", help="blast-radius depth")
parser.add_argument("--sample", type=int, default=0, help="ask about N symbols rather than all")
parser.add_argument("--seed", type=int, default=1, help="sample seed, so a run is repeatable")
args = parser.parse_args()

index_path = os.path.join(args.path, ".axiom", "index.json")
if not os.path.exists(index_path):
    sys.exit(f"no index at {index_path}: run `axiom scan --path {args.path}` first")

nodes = json.load(open(index_path, encoding="utf-8"))["nodes"]
suite = sum(1 for v in nodes.values() if v.get("kind") == "test")
symbols = sorted(k for k, v in nodes.items() if v.get("kind") != "test")
population = len(symbols)
if args.sample and args.sample < population:
    symbols = random.Random(args.seed).sample(symbols, args.sample)

# "  12 of 3429 tests, 99.65% pruned" - the suite size the CLI itself used, which
# is what the percentages must be taken against.
header = re.compile(r"^\s*(\d+) of (\d+) tests, ([\d.]+)% pruned")

selected = []
for done, symbol in enumerate(symbols):
    result = subprocess.run(
        [args.binary, "blast-radius", "--symbol", symbol, "--depth", args.depth],
        cwd=args.path, capture_output=True, text=True)
    picked = set()
    for line in result.stdout.splitlines():
        match = header.match(line)
        if match:
            suite = int(match.group(2))
        candidate = line.strip()
        if nodes.get(candidate, {}).get("kind") == "test":
            picked.add(candidate)
    if picked:
        selected.append(picked)
    if done and done % 50 == 0:
        print(f"  {done}/{len(symbols)}", file=sys.stderr)

if not suite:
    sys.exit("the index holds no tests, so there is nothing to prune")

sizes = sorted(len(p) for p in selected)
mean = statistics.mean(sizes) if sizes else 0.0
median = statistics.median(sizes) if sizes else 0.0
pairs = list(combinations(selected, 2))[:MAX_PAIRS]
overlaps = [len(a & b) / len(a | b) for a, b in pairs]

sampled = "" if len(symbols) == population else f" (sampled {len(symbols)}, seed {args.seed})"
print(f"tree              {os.path.abspath(args.path)}")
print(f"depth             {args.depth}")
print(f"suite             {suite} tests")
print(f"non-test symbols  {population}{sampled}")
print(f"reach >= 1 test   {len(selected)} of {len(symbols)} asked")
print(f"tests selected    mean {mean:.1f}, median {median:.0f}, max {sizes[-1] if sizes else 0}")
print(f"pruned            mean {100.0 * (1 - mean / suite):.1f}%, median {100.0 * (1 - median / suite):.1f}%")
print(f"mean Jaccard      {statistics.mean(overlaps):.2f}" if overlaps else "mean Jaccard      n/a")
