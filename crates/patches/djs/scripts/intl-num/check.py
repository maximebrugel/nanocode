#!/usr/bin/env python3
"""check.py RUNJS SUITE: runs each case in node ($NODE) and in djs (the runjs example); prints mismatches."""
import json, os, subprocess, sys
run, suite = sys.argv[1], sys.argv[2]
cases = [json.loads(l) for l in open(suite)]
ref = [subprocess.run([os.environ.get("NODE", "node"), "-e", "globalThis.text=v=>process.stdout.write(v+'\\n');" + c["src"]], capture_output=True, text=True).stdout.rstrip("\n") for c in cases]
p = subprocess.run([run], input="".join(json.dumps(c["src"]) + "\n" for c in cases), capture_output=True, text=True)
got = [json.loads(l) for l in p.stdout.splitlines()]
bad = 0
for c, a, b in zip(cases, ref, got):
    if a != b:
        bad += 1
        al, bl = a.split("\n"), b.split("\n")
        k = next((i for i in range(max(len(al), len(bl))) if (al[i:i+1] != bl[i:i+1])), 0)
        print("MISMATCH", c["id"], "\n N", (al[k] if k < len(al) else "")[:600], "\n D", (bl[k] if k < len(bl) else "")[:600])
    elif not a or "Error" in a[:20]:
        print("EMPTY/ERR", c["id"], a[:200])
print(len(cases) - bad, "/", len(cases), "match;", sum(len(a) for a in ref), "bytes of node output")
