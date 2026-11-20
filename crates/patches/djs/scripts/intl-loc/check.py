#!/usr/bin/env python3
# Runs intl-locale.jsonl under node 22 and runjs and prints the cases that differ.
#   scripts/intl-loc/check.py RUNJS
import json, os, subprocess, sys
here = os.path.dirname(os.path.abspath(__file__))
NODE = os.environ.get("NODE", "node")
cases = [json.loads(l) for l in open(os.path.join(here, "intl-locale.jsonl"), encoding="utf8") if l.strip()]
env = {**os.environ, "NODE_NO_WARNINGS": "1"}
ref = [subprocess.run([NODE, "-"], input="globalThis.text=v=>process.stdout.write(v+'\\n');" + c["src"], capture_output=True, text=True, env=env).stdout for c in cases]
p = subprocess.run([sys.argv[1]], input="".join(json.dumps(c["src"]) + "\n" for c in cases), capture_output=True, text=True)
outs = [l for l in p.stdout.split("\n") if l]
bad = 0
for i, c in enumerate(cases):
    got = json.loads(outs[i]) if i < len(outs) else "MISSING"
    if got.rstrip("\n") != ref[i].rstrip("\n"):
        bad += 1
        a, b = ref[i].split("\n"), got.split("\n")
        k = next((k for k in range(min(len(a), len(b))) if a[k] != b[k]), min(len(a), len(b)))
        print(c["id"], "\n  node", (a[k] if k < len(a) else "")[:400], "\n  djs ", (b[k] if k < len(b) else "")[:400])
print(len(cases) - bad, "/", len(cases), "match node")
