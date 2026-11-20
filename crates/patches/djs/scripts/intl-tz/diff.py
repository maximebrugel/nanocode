#!/usr/bin/env python3
"""Zone differential: djs (runjs example) vs node on every zone name x random instants 1900..2100 plus
every probed transition +-1s. usage: diff.py RUNJS NODE PROBE.json [locale] [n_random] [BASE_RUNJS]
With BASE_RUNJS it also counts per field the instants BASE got right and RUNJS gets wrong (regressions)."""
import json, os, random, re, subprocess, sys

RUNJS, NODE, PROBE = sys.argv[1:4]
LOC = sys.argv[4] if len(sys.argv) > 4 else "en-US"
NR = int(sys.argv[5]) if len(sys.argv) > 5 else 300
HERE = os.path.dirname(os.path.abspath(__file__))
probe = json.load(open(PROBE))
data = open(os.path.join(HERE, "../../crates/djs-core/src/builtins/intl/data.rs")).read()
a = data.index("pub(super) static ZONE_ALIASES")
aliases = re.findall(r'^    \("([^"]+)", "([^"]+)"\)', data[a:data.index("];", a)], re.M)
names = sorted(set(list(probe) + [x for x, y in aliases if y in probe]))
lo, hi = -2208988800, 4102444800
rng = random.Random(20261120)
cases = []
for z in names:
    canon = z if z in probe else dict(aliases)[z]
    ts = {rng.randrange(lo, hi) for _ in range(NR)}
    for p in probe[canon][1:]:
        if lo <= p[0] <= hi:
            ts |= {p[0] - 1, p[0], p[0] + 1}
    cases.append((z, sorted(ts)))
def src(z, ts):
    return ("const z=" + json.dumps(z) + ",T=" + json.dumps(ts) + ",L=" + json.dumps(LOC) + ";"
            "const fs=[new Intl.DateTimeFormat(L,{timeZone:z,hourCycle:'h23',year:'numeric',month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit',second:'2-digit'})]"
            ".concat(['short','long','shortOffset','longOffset','shortGeneric','longGeneric'].map(k=>new Intl.DateTimeFormat(L,{timeZone:z,timeZoneName:k,year:'numeric'})));"
            "const o=[];for(const t of T)o.push(t+' '+fs.map(f=>f.format(t*1000)).join(' | '));text(o.join('\\n'))")
srcs = [src(z, ts) for z, ts in cases]
pre = "globalThis.text=v=>process.stdout.write(JSON.stringify(v)+'\\n');"
ref = subprocess.run([NODE, "-e", pre + "for(const s of JSON.parse(require('fs').readFileSync(0,'utf8')))eval(s)"], input=json.dumps(srcs), capture_output=True, text=True, env={"PATH": "/usr/bin:/bin", "TZ": "UTC"})
refs = [json.loads(l) for l in ref.stdout.splitlines()]
mine = subprocess.run([RUNJS], input="".join(json.dumps(x) + "\n" for x in srcs), capture_output=True, text=True)
ours = [json.loads(l) for l in mine.stdout.splitlines()]
BASE = sys.argv[6] if len(sys.argv) > 6 else None
bases = [json.loads(l) for l in subprocess.run([BASE], input="".join(json.dumps(x) + "\n" for x in srcs), capture_output=True, text=True).stdout.splitlines()] if BASE else None
reg, fix, rex = {}, {}, []
if BASE:
    for (z, ts), r, o, b in zip(cases, refs, ours, bases):
        for x, y, w in zip(r.split("\n"), o.split("\n"), b.split("\n")):
            xs, ys, ws = x.split(" | "), y.split(" | "), w.split(" | ")
            for k in range(len(xs)):
                if ws[k] == xs[k] and ys[k] != xs[k]:
                    reg[k] = reg.get(k, 0) + 1
                    if sum(1 for e in rex if e[1] == k) < 4: rex.append((z, k, x.split(" ")[0], xs[k], ys[k], ws[k]))
                elif ws[k] != xs[k] and ys[k] == xs[k]:
                    fix[k] = fix.get(k, 0) + 1
assert len(refs) == len(srcs) == len(ours), (len(refs), len(srcs), len(ours), ref.stderr[-500:], mine.stderr[-500:])
total = bad = 0
byzone = {}
fields = ["time", "short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"]
byfield = {}
ex = []
for (z, ts), r, o in zip(cases, refs, ours):
    rl, ol = r.split("\n"), o.split("\n")
    for i, x in enumerate(rl):
        total += 1
        y = ol[i] if i < len(ol) else "<missing>"
        if x != y:
            bad += 1
            byzone[z] = byzone.get(z, 0) + 1
            xs, ys = x.split(" | "), y.split(" | ")
            for k, f in enumerate(fields):
                if k >= len(ys) or xs[k] != ys[k]:
                    byfield[f] = byfield.get(f, 0) + 1
            if len(ex) < 40:
                ex.append((z, x, y))
print(f"{LOC}: {len(names)} zones, {total} instants, {bad} mismatches")
print("by field", byfield)
if BASE:
    print("regressions vs base by field index", reg, "fixes", fix)
    for e in rex: print("  regression", e)
print("by zone", sorted(byzone.items(), key=lambda kv: -kv[1])[:30])
for e in ex:
    print(e[0]); print("  node", e[1]); print("  djs ", e[2])
