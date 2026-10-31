#!/usr/bin/env python3
"""Compare two builds of bench/capi_bench.c (zz's Zig engine and this crate).

Runs every scenario in both binaries, alternating which goes first, and
prints the median CPU time (and user instructions on Linux) of each, with
the Rust/Zig ratio. A ratio above 1 means the Rust engine is slower.

    bench/capi_compare.py ZIG_BIN RUST_BIN [--runs N] [--reps N] [--only a,b] [--json OUT]
"""
import argparse, json, statistics as st, subprocess, sys

p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
p.add_argument("zig"); p.add_argument("rust")
p.add_argument("--runs", type=int, default=5, help="process runs per engine (alternating)")
p.add_argument("--reps", type=int, default=3, help="timed repetitions per process")
p.add_argument("--only", default="", help="comma-separated scenario prefixes")
p.add_argument("--json", default="")
a = p.parse_args()

names = subprocess.run([a.rust, "list"], capture_output=True, text=True, check=True).stdout.split()
if a.only:
    names = [n for n in names if any(n.startswith(x) for x in a.only.split(","))]

def once(exe, name):
    out = subprocess.run([exe, name, str(a.reps)], capture_output=True, text=True, check=True).stdout
    return json.loads(out)

rows = []
print(f"{'scenario':18} {'zig ms':>9} {'rust ms':>9} {'cpu':>6} {'zig Minstr':>11} {'rust Minstr':>11} {'instr':>6}  sink", flush=True)
for name in names:
    res = {"zig": [], "rust": []}
    for i in range(a.runs):
        order = [("zig", a.zig), ("rust", a.rust)] if i % 2 == 0 else [("rust", a.rust), ("zig", a.zig)]
        for label, exe in order:
            res[label].append(once(exe, name))
    ms = {k: st.median(min(r["cpu_ms"]) for r in v) for k, v in res.items()}
    ins = {k: st.median(min(r["instructions"]) for r in v) / 1e6 for k, v in res.items()}
    same = res["zig"][0]["sink"] == res["rust"][0]["sink"]
    row = {"scenario": name, "zig_ms": ms["zig"], "rust_ms": ms["rust"], "cpu_ratio": ms["rust"] / ms["zig"],
           "zig_minstr": ins["zig"], "rust_minstr": ins["rust"],
           "instr_ratio": ins["rust"] / ins["zig"] if ins["zig"] > 0 else None, "same_output": same}
    rows.append(row)
    ir = f"{row['instr_ratio']:6.3f}" if row["instr_ratio"] else "     -"
    zi = f"{ins['zig']:11.1f}" if ins["zig"] > 0 else "          -"
    ri = f"{ins['rust']:11.1f}" if ins["rust"] > 0 else "          -"
    print(f"{name:18} {ms['zig']:9.2f} {ms['rust']:9.2f} {row['cpu_ratio']:6.3f} {zi} {ri} {ir}  {'same' if same else 'DIFF'}", flush=True)
if a.json:
    json.dump(rows, open(a.json, "w"), indent=1)
