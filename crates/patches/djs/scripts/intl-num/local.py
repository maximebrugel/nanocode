#!/usr/bin/env python3
# local driver: matrix.py cases -> node + runjs (djs-embed example), bucketed diff
import json, subprocess, sys, collections, os
RUN = sys.argv[1]; OUT = sys.argv[2]; locs = sys.argv[3] if len(sys.argv) > 3 else ""
here = os.path.dirname(os.path.abspath(__file__))
cases = [json.loads(l) for l in subprocess.run(["python3", os.path.join(here, "matrix.py"), "-", "x"] + ([locs] if locs else []), capture_output=True, text=True, env={**os.environ}).stdout.splitlines()]
refp = os.path.join(OUT, "ref.json")
os.makedirs(OUT, exist_ok=True)
ref = json.load(open(refp)) if os.path.exists(refp) and not os.environ.get("REGEN") else {}
for c in cases:
    if c["id"] not in ref:
        ref[c["id"]] = subprocess.run([os.environ.get("NODE", "node"), "-e", "globalThis.text=v=>process.stdout.write(v+'\\n');" + c["src"]], capture_output=True, text=True).stdout
json.dump(ref, open(refp, "w"))
p = subprocess.run([RUN], input="".join(json.dumps(c["src"]) + "\n" for c in cases), capture_output=True, text=True)
outs = p.stdout.splitlines()
if p.returncode: print("runjs rc", p.returncode, p.stderr[-2000:])
mism = []; tot = collections.Counter(); bad = collections.Counter()
for i, c in enumerate(cases):
    loc = c["id"].split("@")[0]
    got = json.loads(outs[i]) if i < len(outs) else "MISSING"
    a = ref[c["id"]].rstrip("\n").split("\n"); b = got.rstrip("\n").split("\n")
    if got.startswith("ERR"): print(c["id"], got[:300])
    for k in range(max(len(a), len(b))):
        x = a[k] if k < len(a) else "<none>"; y = b[k] if k < len(b) else "<none>"
        cat = x.split("|")[0]; tot[cat] += 1
        if x != y: bad[cat] += 1; mism.append({"loc": loc, "cat": cat, "node": x, "djs": y})
json.dump(mism, open(os.path.join(OUT, "mism.json"), "w"), ensure_ascii=False, indent=0)
print("total", sum(tot.values()), "mism", len(mism))
for k in tot: print(" ", k, bad[k], "/", tot[k])
bl = collections.Counter((m["loc"], m["cat"]) for m in mism)
print(" by loc:", dict(bl))
