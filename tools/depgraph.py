#!/usr/bin/env python3
"""Print file-level @import graph of ghostty/src with topo levels and line counts."""
import os, re, sys, collections
ROOT=os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
Z=os.path.join(ROOT,'ghostty','src')
files={}
for dp,dn,fn in os.walk(Z):
    for f in fn:
        if f.endswith('.zig'):
            p=os.path.relpath(os.path.join(dp,f),Z); files[p]=open(os.path.join(dp,f)).read()
deps={}
for p,t in files.items():
    ds=set()
    for m in re.finditer(r'@import\("([^"]+\.zig)"\)',t):
        q=os.path.normpath(os.path.join(os.path.dirname(p),m.group(1)))
        if q in files and q!=p: ds.add(q)
    deps[p]=ds
def testlines(t):
    n=0; inside=False
    for line in t.split('\n'):
        if line.startswith('test ') : inside=True
        if inside: n+=1
        if inside and line.startswith('}'): inside=False
    return n
prefix=sys.argv[1] if len(sys.argv)>1 else ''
sel=[p for p in files if p.startswith(prefix)]
# SCC-tolerant levels: iterative longest path ignoring cycles
level={p:0 for p in sel}
for _ in range(50):
    ch=False
    for p in sel:
        for d in deps[p]:
            if d in level and level[d]+1>level[p] and level[p]<40:
                level[p]=level[d]+1; ch=True
    if not ch: break
for p in sorted(sel,key=lambda p:(level[p],p)):
    t=files[p]; n=t.count('\n'); tl=testlines(t)
    print(f"{level[p]:3d} {n:6d} {tl:6d} {p}  <- {' '.join(sorted(d for d in deps[p] if d.startswith(prefix)))}")
