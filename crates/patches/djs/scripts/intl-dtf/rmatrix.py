#!/usr/bin/env python3
"""formatRange matrix: djs (via ws) vs node over rcases.js. usage: rmatrix.py WS OUTDIR locales [N]"""
import json, os, subprocess, sys
WS, OUT = sys.argv[1], sys.argv[2]
LOCALES = sys.argv[3].split(",")
N = sys.argv[4] if len(sys.argv) > 4 else "200"
HERE = os.path.dirname(os.path.abspath(__file__))
os.makedirs(OUT, exist_ok=True)
env = {"PATH": os.environ["PATH"], "LC_ALL": "C", "HOME": "/tmp"}
def prog(loc, cs):
    return ("const L=%s,C=%s;const r=[];for(const [o,a,b] of C){let s;try{const f=new Intl.DateTimeFormat(L,o);"
            "s=f.formatRange(a,b)+' # '+f.formatRangeToParts(a,b).map(p=>p.type[0]+p.source[0]+':'+p.value).join('|')}catch(e){s='E:'+e.name}"
            "r.push(JSON.stringify([o,a,b])+' => '+s)}text(r.join('\\n'))") % (json.dumps(loc), json.dumps(cs))
cases = []
for i, loc in enumerate(LOCALES):
    cs = json.loads(subprocess.run(["node", os.path.join(HERE, "rcases.js"), loc, N, str(7 + i)], capture_output=True, env=env).stdout)
    cases.append({"id": loc, "kind": "js", "src": prog(loc, cs), "timeout": 120})
PRE = "globalThis.text=v=>process.stdout.write((typeof v==='string'?v:JSON.stringify(v))+'\\n');\n(async()=>{\n"
POST = "\n})().then(()=>{},e=>{process.stdout.write('ERROR '+e+'\\n')})\n"
def node_run(src):
    path = os.path.join(OUT, "ref.mjs")
    open(path, "w").write(PRE + src + POST)
    return subprocess.run(["node", path], capture_output=True, env=env).stdout.decode()
ref = {c["id"]: node_run(c["src"]) for c in cases}
payload = "".join(json.dumps({**c, "root": "/tmp/rngm-" + str(i)}) + "\n" for i, c in enumerate(cases))
for i in range(len(cases)):
    os.makedirs("/tmp/rngm-" + str(i), exist_ok=True)
p = subprocess.run([WS], input=payload.encode(), capture_output=True, timeout=1800)
got = {}
for l in p.stdout.decode("utf-8", "replace").split("\n"):
    try:
        r = json.loads(l); got[r["id"]] = r.get("out", "") + (("ERR:" + r["err"]) if r.get("err") else "") + (r.get("error") or "")
    except Exception:
        pass
mism, total = [], 0
for c in cases:
    a = ref[c["id"]].rstrip("\n").split("\n"); b = got.get(c["id"], "MISSING").rstrip("\n").split("\n")
    for k in range(max(len(a), len(b))):
        x = a[k] if k < len(a) else "<none>"; y = b[k] if k < len(b) else "<none>"
        total += 1
        if x != y:
            mism.append({"loc": c["id"], "node": x, "djs": y})
json.dump(mism, open(os.path.join(OUT, "rmism.json"), "w"), indent=0, ensure_ascii=False)
by = {}
for m in mism: by[m["loc"]] = by.get(m["loc"], 0) + 1
print("total", total, "mismatches", len(mism), json.dumps(by))

