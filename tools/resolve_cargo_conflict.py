#!/usr/bin/env python3
"""Resolve a git conflict in Cargo.toml by taking the union of both sides
(sorting dependency-style `key = value` lines within each conflict)."""
import re, sys
p = sys.argv[1] if len(sys.argv) > 1 else "Cargo.toml"
s = open(p).read()
def fix(m):
    lines = []
    for l in (m.group(1) + m.group(2)).splitlines():
        if l and l not in lines:
            lines.append(l)
    if all(re.match(r'^[A-Za-z0-9_\-"]+\s*=', l) for l in lines):
        lines.sort(key=lambda l: l.split("=")[0].strip().strip('"'))
    return "\n".join(lines) + "\n"
s2 = re.sub(r"<<<<<<< [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n", fix, s, flags=re.S)
open(p, "w").write(s2)
