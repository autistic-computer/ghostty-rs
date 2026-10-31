#!/usr/bin/env python3
"""Summarize porting progress from the STATUS header of each mirrored file."""
import os, sys, collections
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
rows = [l.rstrip("\n").split("\t") for l in open(os.path.join(ROOT, "PORTING_STATUS.tsv"))][1:]
prefix = sys.argv[1] if len(sys.argv) > 1 else ""
tot = collections.Counter(); files = collections.Counter(); detail = []
for zig, rs, n in rows:
    if not zig.startswith(prefix): continue
    text = open(os.path.join(ROOT, rs)).read(2000)
    st = "ported" if "STATUS: ported" in text else "partial" if "STATUS: partial" in text else "todo"
    tot[st] += int(n); files[st] += 1; detail.append((st, zig, int(n)))
if "-v" in sys.argv:
    for st, z, n in sorted(detail): print(f"{st:8} {n:6} {z}")
all_ = sum(tot.values())
for st in ("ported", "partial", "todo"):
    print(f"{st:8} {files[st]:4} files {tot[st]:7} zig lines ({100*tot[st]/max(all_,1):.1f}%)")
