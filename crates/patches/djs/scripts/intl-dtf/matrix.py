#!/usr/bin/env python3
"""DateTimeFormat matrix: djs (via ws) vs node. usage: matrix.py WS OUTDIR [locales]"""
import json, os, subprocess, sys, collections

WS, OUT = sys.argv[1], sys.argv[2]
LOCALES = sys.argv[3].split(",") if len(sys.argv) > 3 else ["en-US"]
os.makedirs(OUT, exist_ok=True)

ZONES = ["UTC", "America/Los_Angeles", "America/New_York", "Europe/Berlin", "Europe/London", "Asia/Tokyo",
         "Asia/Kolkata", "Australia/Sydney", "America/Sao_Paulo", "Pacific/Auckland", "Asia/Kathmandu",
         "America/St_Johns", "Etc/GMT+5", "+05:30", "America/Phoenix", "Africa/Cairo", "Asia/Shanghai",
         "Europe/Moscow", "America/Chicago", "Pacific/Honolulu"]
if os.environ.get("ZONES"):
    ZONES = os.environ["ZONES"].split(",")
DATES = ["2024-03-10T09:59:59Z", "2024-03-10T10:00:00Z", "2024-11-03T08:59:59Z", "2024-11-03T09:00:00Z",
         "2024-03-31T00:59:59Z", "2024-03-31T01:00:00Z", "2024-10-27T00:59:59Z", "2024-10-27T01:00:00Z",
         "2024-06-15T12:34:56.789Z", "2025-01-01T00:00:00Z", "2023-12-31T23:30:00Z", "2024-02-29T18:00:00Z",
         "2026-07-04T03:05:09.004Z", "2000-07-01T12:00:00Z", "2006-03-20T12:00:00Z", "1990-04-05T12:00:00Z",
         "1970-01-01T00:00:00Z", "1950-06-01T12:00:00Z", "1800-01-01T12:00:00Z"]
O = []
O.append({})
for s in ["full", "long", "medium", "short"]:
    O.append({"dateStyle": s}); O.append({"timeStyle": s})
    for t in ["full", "long", "medium", "short"]:
        O.append({"dateStyle": s, "timeStyle": t})
for z in ["short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"]:
    O.append({"timeZoneName": z})
    O.append({"hour": "numeric", "minute": "2-digit", "timeZoneName": z})
    O.append({"year": "numeric", "month": "short", "day": "numeric", "hour": "2-digit", "minute": "2-digit", "second": "2-digit", "timeZoneName": z})
    O.append({"hour": "numeric", "hourCycle": "h23", "timeZoneName": z})
O += [{"hour": "numeric"}, {"hour": "2-digit", "minute": "2-digit", "hour12": False},
      {"hour": "numeric", "minute": "numeric", "second": "numeric", "fractionalSecondDigits": 3},
      {"weekday": "long", "year": "numeric", "month": "long", "day": "numeric"},
      {"weekday": "short", "hour": "numeric"}, {"month": "long"}, {"weekday": "narrow"}, {"day": "numeric"},
      {"year": "numeric", "month": "2-digit", "day": "2-digit", "hour": "2-digit", "minute": "2-digit", "second": "2-digit", "hour12": False},
      {"era": "short", "year": "numeric"}, {"dayPeriod": "long", "hour": "numeric"}, {"hourCycle": "h11", "hour": "numeric", "minute": "numeric"},
      {"hourCycle": "h24", "hour": "numeric"}, {"month": "short", "day": "numeric"}, {"year": "2-digit", "month": "narrow"},
      {"weekday": "long", "month": "long", "day": "numeric", "hour": "numeric", "minute": "2-digit", "timeZoneName": "short"},
      {"month": "long", "year": "numeric"}, {"month": "short", "year": "numeric", "day": "numeric", "weekday": "short"},
      {"month": "long", "day": "numeric"}, {"weekday": "long"}, {"month": "narrow"}, {"year": "numeric", "month": "numeric"},
      {"weekday": "short", "day": "numeric"}, {"hour": "numeric", "minute": "2-digit"}, {"hour": "numeric", "minute": "2-digit", "second": "2-digit"}]

def js_prog(loc, zone):
    return ("const L=%s,Z=%s,D=%s,O=%s;" % (json.dumps(loc), json.dumps(zone), json.dumps(DATES), json.dumps(O)) +
            "const r=[];for(let i=0;i<O.length;i++){let f;try{f=new Intl.DateTimeFormat(L,{...O[i],timeZone:Z})}catch(e){r.push(i+'|C|'+e.name);continue}"
            "r.push(i+'|R|'+JSON.stringify(f.resolvedOptions()));"
            "for(let j=0;j<D.length;j++){let s;try{s=f.formatToParts(new Date(D[j])).map(p=>p.type[0]+':'+p.value).join('|')}catch(e){s='E:'+e.name}r.push(i+'|'+j+'|'+s)}}"
            "for(let j=0;j<D.length;j++){const d=new Date(D[j]);for(const m of ['toLocaleString','toLocaleDateString','toLocaleTimeString'])r.push('T|'+m+'|'+j+'|'+d[m](L,{timeZone:Z}))}"
            "text(r.join('\\n'))")

cases = []
for loc in LOCALES:
    for z in ZONES:
        cases.append({"id": f"{loc}@{z}", "kind": "js", "src": js_prog(loc, z), "timeout": 60})
PRE = "globalThis.text=v=>process.stdout.write((typeof v==='string'?v:JSON.stringify(v))+'\\n');\n(async()=>{\n"
POST = "\n})().then(()=>{},e=>{process.stdout.write('ERROR '+e+'\\n')})\n"
env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LC_ALL": "C", "HOME": "/tmp"}
ref = {c["id"]: subprocess.run(["node", "--input-type=module", "-e", PRE + c["src"] + POST], capture_output=True, env=env).stdout.decode()
       for c in cases}
payload = "".join(json.dumps({**c, "root": "/tmp/dtfm-" + str(i)}) + "\n" for i, c in enumerate(cases))
for i in range(len(cases)):
    os.makedirs("/tmp/dtfm-" + str(i), exist_ok=True)
p = subprocess.run([WS], input=payload.encode(), capture_output=True, timeout=900)
got = {}
for l in p.stdout.decode("utf-8", "replace").split("\n"):
    try:
        r = json.loads(l); got[r["id"]] = r.get("out", "") + (("ERR:" + r["err"]) if r.get("err") else "") + (r.get("error") or "")
    except Exception:
        pass
mism = []
total = 0
for c in cases:
    a = ref[c["id"]].split("\n"); b = got.get(c["id"], "MISSING").split("\n")
    loc, zone = c["id"].split("@")
    for k in range(max(len(a), len(b))):
        x = a[k] if k < len(a) else "<none>"; y = b[k] if k < len(b) else "<none>"
        total += 1
        if x.replace(" ", " ") != y.replace(" ", " "):
            mism.append({"loc": loc, "zone": zone, "node": x, "djs": y})
json.dump({"O": O, "D": DATES, "mism": mism}, open(os.path.join(OUT, "mism.json"), "w"), indent=0, ensure_ascii=False)
print("lines", total, "mismatches", len(mism))
bz = collections.Counter((m["loc"], m["zone"]) for m in mism)
for k, v in bz.most_common(60):
    print(v, k)
bd = collections.Counter(m["node"].split("|")[1] for m in mism if "|" in m["node"])
print("by date/kind", bd.most_common(30))
