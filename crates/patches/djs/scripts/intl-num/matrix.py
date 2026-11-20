#!/usr/bin/env python3
"""Intl NumberFormat/PluralRules/ListFormat/RelativeTimeFormat/... differential matrix: djs (via ws) vs node.
usage: matrix.py WS OUTDIR [locales]   (WS=- prints the generated cases as jsonl and exits)"""
import json, os, subprocess, sys, collections, random

WS, OUT = sys.argv[1], sys.argv[2]
LOCALES = sys.argv[3].split(",") if len(sys.argv) > 3 else ["en-US", "de", "fr", "es", "ja", "zh", "pt-BR"]
SEED = int(os.environ.get("SEED", "1"))
NRAND = int(os.environ.get("NRAND", "160"))
rnd = random.Random(SEED)

V = [0, "-0", 1, -1, 0.5, 1.005, 2.5, -2.5, 0.15, 1234.5678, -1234.5678, 1000, 10000, 100000, 999999, 1234567.891,
     123456789.123, 0.000123, 1e-7, 99999.5, 1e21, 5e15, 21, 101, 1.5, 0.01, "NaN", "Infinity", "-Infinity", 7654321]
CUR = ["USD", "EUR", "JPY", "GBP", "BRL", "CNY", "CHF", "INR", "KRW", "CAD", "MXN", "XYZ"]
UNITS = ["kilometer-per-hour", "percent", "byte", "kilobyte", "megabyte", "gigabyte", "celsius", "fahrenheit", "meter", "kilometer",
         "mile", "liter", "kilogram", "gram", "pound", "second", "minute", "hour", "day", "week", "month", "year", "millisecond",
         "inch", "foot", "degree", "acre", "hectare", "liter-per-kilometer", "mile-per-gallon", "meter-per-second", "bit", "terabyte"]
FIXED = [{}, {"style": "percent"}, {"style": "percent", "maximumFractionDigits": 2},
         {"minimumFractionDigits": 2}, {"maximumFractionDigits": 0}, {"maximumSignificantDigits": 3}, {"minimumSignificantDigits": 5},
         {"useGrouping": False}, {"useGrouping": "always"}, {"useGrouping": "min2"}, {"minimumIntegerDigits": 3},
         {"notation": "compact"}, {"notation": "compact", "compactDisplay": "long"}, {"notation": "scientific"}, {"notation": "engineering"},
         {"notation": "scientific", "maximumFractionDigits": 2}, {"signDisplay": "always"}, {"signDisplay": "exceptZero"},
         {"signDisplay": "never"}, {"signDisplay": "negative"}]
for c in CUR:
    FIXED.append({"style": "currency", "currency": c})
for c in ["USD", "EUR", "JPY", "BRL"]:
    for d in ["code", "name", "narrowSymbol"]:
        FIXED.append({"style": "currency", "currency": c, "currencyDisplay": d})
    FIXED.append({"style": "currency", "currency": c, "currencySign": "accounting"})
    FIXED.append({"style": "currency", "currency": c, "notation": "compact"})
for u in UNITS:
    for d in ["short", "long", "narrow"]:
        FIXED.append({"style": "unit", "unit": u, "unitDisplay": d})
for m in ["ceil", "floor", "expand", "trunc", "halfCeil", "halfFloor", "halfExpand", "halfTrunc", "halfEven"]:
    FIXED.append({"roundingMode": m, "maximumFractionDigits": 1})
for i in [5, 25, 50]:
    FIXED.append({"roundingIncrement": i, "maximumFractionDigits": 2, "minimumFractionDigits": 2})
FIXED += [{"roundingPriority": "morePrecision", "maximumSignificantDigits": 2, "maximumFractionDigits": 2},
          {"roundingPriority": "lessPrecision", "maximumSignificantDigits": 2, "maximumFractionDigits": 2},
          {"trailingZeroDisplay": "stripIfInteger", "minimumFractionDigits": 2}]

def rand_opts():
    o = {}
    st = rnd.choice(["decimal", "decimal", "percent", "currency", "currency", "unit"])
    if st != "decimal" or rnd.random() < .3: o["style"] = st
    if st == "currency":
        o["currency"] = rnd.choice(CUR[:-1])
        if rnd.random() < .5: o["currencyDisplay"] = rnd.choice(["code", "symbol", "narrowSymbol", "name"])
        if rnd.random() < .3: o["currencySign"] = rnd.choice(["standard", "accounting"])
    if st == "unit":
        o["unit"] = rnd.choice(UNITS)
        if rnd.random() < .7: o["unitDisplay"] = rnd.choice(["short", "long", "narrow"])
    if rnd.random() < .35:
        o["notation"] = rnd.choice(["standard", "scientific", "engineering", "compact"])
        if o["notation"] == "compact" and rnd.random() < .5: o["compactDisplay"] = rnd.choice(["short", "long"])
    if rnd.random() < .3: o["signDisplay"] = rnd.choice(["auto", "never", "always", "exceptZero", "negative"])
    if rnd.random() < .2: o["useGrouping"] = rnd.choice([False, True, "always", "auto", "min2"])
    r = rnd.random()
    if r < .2:
        a = rnd.randint(0, 4); o["minimumFractionDigits"] = a
        if rnd.random() < .6: o["maximumFractionDigits"] = rnd.randint(a, 6)
    elif r < .35:
        o["maximumFractionDigits"] = rnd.randint(0, 5)
    elif r < .5:
        a = rnd.randint(1, 4)
        if rnd.random() < .5: o["minimumSignificantDigits"] = a
        o["maximumSignificantDigits"] = rnd.randint(a, 8)
    if rnd.random() < .15: o["roundingMode"] = rnd.choice(["ceil", "floor", "expand", "trunc", "halfCeil", "halfFloor", "halfExpand", "halfTrunc", "halfEven"])
    if rnd.random() < .1: o["minimumIntegerDigits"] = rnd.randint(1, 5)
    if rnd.random() < .08: o["trailingZeroDisplay"] = "stripIfInteger"
    return o

O = FIXED + [rand_opts() for _ in range(NRAND)]

PRE_LIB = r"""
const P=(a)=>a.map(p=>p.type+':'+p.value).join('|');
const tv=(v)=>typeof v==='string'?(v==='-0'?-0:Number(v)):v;
const tryf=(f)=>{try{return f()}catch(e){return 'E:'+e.name}};
"""

def nf_prog(loc, chunk, base):
    return ("const L=%s,V=%s,O=%s,B=%d;" % (json.dumps(loc), json.dumps(V), json.dumps(chunk), base) + PRE_LIB +
            "const r=[];for(let i=0;i<O.length;i++){let f;try{f=new Intl.NumberFormat(L,O[i])}catch(e){r.push('nf|'+(B+i)+'|C|'+e.name);continue}"
            "r.push('nf|'+(B+i)+'|R|'+JSON.stringify(f.resolvedOptions()));"
            "for(let j=0;j<V.length;j++){r.push('nf|'+(B+i)+'|'+j+'|'+tryf(()=>P(f.formatToParts(tv(V[j])))))}"
            "r.push('nf|'+(B+i)+'|str|'+tryf(()=>f.format('1234567.891234567891')));"
            "r.push('nf|'+(B+i)+'|big|'+tryf(()=>f.format(12345678901234567890n)));"
            "r.push('nf|'+(B+i)+'|rng|'+tryf(()=>P(f.formatRange(3,5)))+' / '+tryf(()=>f.formatRange(1000,1000))+' / '+tryf(()=>f.formatRange(-5,2.5))+' / '+tryf(()=>f.formatRange(0.9,1.1)));"
            "}text(r.join('\\n'))")

MISC = r"""
const r=[];const L=LOC;
for(const v of [0,1,-1234.5,1234567.891,0.000123,NaN,-Infinity,1e21,12.345])r.push('tls|'+v+'|'+v.toLocaleString(L)+'|'+tryf(()=>v.toLocaleString(L,{style:'currency',currency:'EUR'}))+'|'+tryf(()=>v.toLocaleString(L,{maximumFractionDigits:1})));
for(const v of [0n,1n,-1234n,1234567n,12345678901234567890n])r.push('btls|'+v+'|'+v.toLocaleString(L)+'|'+tryf(()=>v.toLocaleString(L,{style:'currency',currency:'JPY'}))+'|'+tryf(()=>v.toLocaleString(L,{notation:'compact'})));
for(const t of ['cardinal','ordinal']){const p=new Intl.PluralRules(L,{type:t});r.push('pr|'+t+'|R|'+JSON.stringify(p.resolvedOptions()));
 const a=[];for(let n=0;n<=130;n++)a.push(p.select(n));for(const n of [0.5,1.5,1.0,2.25,1000000,1e6+1,-1,-2,0.1,11.1])a.push(n+':'+p.select(n));r.push('pr|'+t+'|S|'+a.join(','));
 r.push('pr|'+t+'|rng|'+[[1,2],[0,1],[1,5],[2,3],[0.5,1],[1,1.5],[5,10]].map(([x,y])=>tryf(()=>p.selectRange(x,y))).join(','))}
for(const o of [{minimumFractionDigits:2},{maximumSignificantDigits:1},{minimumSignificantDigits:3}]){const p=new Intl.PluralRules(L,o);r.push('pr|opt|'+JSON.stringify(o)+'|'+[0,1,1.5,2,5,21].map(n=>p.select(n)).join(','))}
const items=[['A'],['A','B'],['A','B','C'],['A','B','C','D'],['x','y','z','w','v'],['Paris','iglesia'],['agua','hierro'],['uno','ocho'],['siete','once']];
for(const ty of ['conjunction','disjunction','unit'])for(const st of ['long','short','narrow']){let f;try{f=new Intl.ListFormat(L,{type:ty,style:st})}catch(e){r.push('lf|'+ty+'|'+st+'|C|'+e.name);continue}
 r.push('lf|'+ty+'|'+st+'|'+items.map(x=>f.format(x)).join(' ¦ ')+' ¦ '+P(f.formatToParts(['a','b','c'])))}
const units=['second','minute','hour','day','week','month','quarter','year','seconds','days'];
for(const st of ['long','short','narrow'])for(const nu of ['always','auto']){const f=new Intl.RelativeTimeFormat(L,{style:st,numeric:nu});
 r.push('rtf|'+st+'|'+nu+'|R|'+JSON.stringify(f.resolvedOptions()));
 for(const u of units)r.push('rtf|'+st+'|'+nu+'|'+u+'|'+[-2,-1,0,1,2,3,-0,1.5,-10,1000,21].map(v=>tryf(()=>f.format(v,u))).join(' ¦ ')+' ¦ '+tryf(()=>P(f.formatToParts(-1234.5,u))))}
r.push('ex|'+['DisplayNames','Locale','Segmenter','supportedValuesOf','DurationFormat'].map(k=>k+':'+typeof Intl[k]).join(','));
r.push('dn|'+tryf(()=>{const d=new Intl.DisplayNames(L,{type:'region'});return ['US','DE','FR','JP','CN','BR','ES','GB','419'].map(x=>d.of(x)).join(',')}));
r.push('dn|lang|'+tryf(()=>{const d=new Intl.DisplayNames(L,{type:'language'});return ['en','de','fr','es','ja','zh','pt','en-US','pt-BR','zh-Hant'].map(x=>d.of(x)).join(',')}));
r.push('dn|cur|'+tryf(()=>{const d=new Intl.DisplayNames(L,{type:'currency'});return ['USD','EUR','JPY','BRL'].map(x=>d.of(x)).join(',')}));
r.push('dn|script|'+tryf(()=>{const d=new Intl.DisplayNames(L,{type:'script'});return ['Latn','Hans','Cyrl'].map(x=>d.of(x)).join(',')}));
r.push('loc|'+tryf(()=>{const l=new Intl.Locale(L);return [l.toString(),l.baseName,l.language,l.region,l.script,l.maximize().toString(),l.minimize().toString()].join(',')}));
r.push('loc2|'+tryf(()=>{const l=new Intl.Locale('en-Latn-US-u-ca-gregory-hc-h12-nu-latn');return [l.toString(),l.calendar,l.hourCycle,l.numberingSystem,l.maximize().toString(),l.minimize().toString(),new Intl.Locale('zh').maximize().toString(),new Intl.Locale('zh-TW').maximize().toString(),new Intl.Locale('und').maximize().toString()].join(',')}));
r.push('svo|'+tryf(()=>['calendar','collation','currency','numberingSystem','timeZone','unit'].map(k=>k+':'+Intl.supportedValuesOf(k).length).join(',')));
r.push('seg|'+tryf(()=>[...new Intl.Segmenter(L,{granularity:'word'}).segment('Hello, world! 123')].map(s=>s.segment+(s.isWordLike?'*':'')).join('/')));
text(r.join('\n'))
"""

def misc_prog(loc):
    return "const LOC=%s;" % json.dumps(loc) + PRE_LIB + MISC

cases = []
CH = 60
for loc in LOCALES:
    for b in range(0, len(O), CH):
        cases.append({"id": f"{loc}@nf{b}", "kind": "js", "src": nf_prog(loc, O[b:b + CH], b), "timeout": 120})
    cases.append({"id": f"{loc}@misc", "kind": "js", "src": misc_prog(loc), "timeout": 60})
if WS == "-":
    for c in cases: print(json.dumps(c))
    sys.exit(0)
os.makedirs(OUT, exist_ok=True)
PRE = "globalThis.text=v=>process.stdout.write((typeof v==='string'?v:JSON.stringify(v))+'\\n');\n(async()=>{\n"
POST = "\n})().then(()=>{},e=>{process.stdout.write('ERROR '+e+'\\n')})\n"
env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LC_ALL": "C", "HOME": "/tmp"}
ref = {c["id"]: subprocess.run(["node", "--input-type=module", "-e", PRE + c["src"] + POST], capture_output=True, env=env).stdout.decode()
       for c in cases}
payload = "".join(json.dumps({**c, "root": "/tmp/nfm-" + str(i)}) + "\n" for i, c in enumerate(cases))
for i in range(len(cases)):
    os.makedirs("/tmp/nfm-" + str(i), exist_ok=True)
p = subprocess.run([WS], input=payload.encode(), capture_output=True, timeout=1800)
got = {}
for l in p.stdout.decode("utf-8", "replace").split("\n"):
    try:
        r = json.loads(l); got[r["id"]] = r.get("out", "") + (("ERR:" + r["err"]) if r.get("err") else "") + (r.get("error") or "")
    except Exception:
        pass
mism = []; total = 0; bytot = collections.Counter()
for c in cases:
    loc = c["id"].split("@")[0]
    a = ref[c["id"]].rstrip("\n").split("\n"); b = got.get(c["id"], "MISSING").rstrip("\n").split("\n")
    bk = {x.rsplit("|", 1)[0] if x.count("|") >= 3 else x: x for x in b}
    for k in range(max(len(a), len(b))):
        x = a[k] if k < len(a) else "<none>"; y = b[k] if k < len(b) else "<none>"
        total += 1; cat = x.split("|")[0]; bytot[(loc, cat)] += 1
        if x != y:
            mism.append({"loc": loc, "cat": cat, "node": x, "djs": y})
json.dump({"O": O, "V": V, "mism": mism}, open(os.path.join(OUT, "mism.json"), "w"), indent=0, ensure_ascii=False)
print("lines", total, "mismatches", len(mism))
bc = collections.Counter((m["loc"], m["cat"]) for m in mism)
for k in sorted(bytot):
    print(k, bc.get(k, 0), "/", bytot[k])
