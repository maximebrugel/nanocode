#!/usr/bin/env python3
# Differential driver: matrix.js cases -> node 22 and runjs (djs-embed example), bucketed.
#   scripts/intl-loc/diff.py RUNJS OUTDIR CLDR [group...]
# Writes OUTDIR/ref.json (node, cached; REGEN=1 redoes it) and OUTDIR/mism.json; prints
# counts by group and, with SHOW=n, the first n mismatches of each group.
import json, os, subprocess, sys, collections
RUN, OUT, CLDR = sys.argv[1:4]; groups = sys.argv[4:]
NODE = os.environ.get("NODE", "node")
here = os.path.dirname(os.path.abspath(__file__))
env = {**os.environ, "NODE_NO_WARNINGS": "1"}
cases = [json.loads(l) for l in subprocess.run([NODE, os.path.join(here, "matrix.js"), CLDR] + groups, capture_output=True, text=True, env=env, check=True).stdout.split("\n") if l]
os.makedirs(OUT, exist_ok=True)
refp = os.path.join(OUT, "ref.json")
ref = json.load(open(refp)) if os.path.exists(refp) and not os.environ.get("REGEN") else {}
for c in cases:
    if c["id"] not in ref:
        ref[c["id"]] = subprocess.run([NODE, "-"], input="globalThis.text=v=>process.stdout.write(v+'\\n');" + c["src"], capture_output=True, text=True, env=env).stdout
json.dump(ref, open(refp, "w"))
p = subprocess.run([RUN], input="".join(json.dumps(c["src"]) + "\n" for c in cases), capture_output=True, text=True)
outs = [l for l in p.stdout.split("\n") if l]
if p.returncode: print("runjs rc", p.returncode, p.stderr[-2000:])
tot = collections.Counter(); bad = collections.Counter(); mism = []
for i, c in enumerate(cases):
    g = c["id"].split("@")[0]
    got = json.loads(outs[i]) if i < len(outs) else "MISSING"
    if got.startswith("ERR"): print(c["id"], got[:400])
    a = ref[c["id"]].rstrip("\n").split("\n"); b = got.rstrip("\n").split("\n")
    for k in range(max(len(a), len(b))):
        x = a[k] if k < len(a) else "<none>"; y = b[k] if k < len(b) else "<none>"
        tot[g] += 1
        if x != y: bad[g] += 1; mism.append({"group": g, "node": x, "djs": y})
json.dump(mism, open(os.path.join(OUT, "mism.json"), "w"), ensure_ascii=False, indent=0)
print("total", sum(tot.values()), "mism", len(mism))
for k in tot: print(" ", k, bad[k], "/", tot[k])
n = int(os.environ.get("SHOW", "0"))
for g in tot:
    for m in [m for m in mism if m["group"] == g][:n]: print("  N", m["node"][:300], "\n  D", m["djs"][:300])

