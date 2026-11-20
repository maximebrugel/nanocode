#!/usr/bin/env python3
"""Writes intl-number.jsonl: the NumberFormat parity suite (each case prints with text(); the
parity runner compares djs with node 22). Every case was checked with local.py-style runs."""
import json, os
L = ["en-US", "de", "fr", "es", "ja", "zh", "pt-BR",
     # with their own digits, grouping, bidi marks or plural categories
     "ar", "ar-EG", "hi", "en-IN", "pl", "ru", "tr", "id", "vi", "zh-TW", "en-GB", "es-MX", "fr-CA", "ko", "it", "nl",
     "hu", "nb", "no", "he", "ta", "sr", "sr-Latn", "cs", "uk", "sv", "da", "ms", "ro", "bn", "el", "sw", "ur", "fil", "ca", "th", "fi"]
V = "[0,-0,1,-1,0.5,1.005,2.5,-2.5,1234.5678,-1234.5678,1000,99999.5,1234567.891,0.000123,1e21,NaN,Infinity,-Infinity]"
H = "const V=" + V + ";const P=a=>a.map(p=>p.type+':'+p.value).join('|');const out=[];const run=(L,os)=>{for(const o of os){const f=new Intl.NumberFormat(L,o);out.push(JSON.stringify(o)+' '+JSON.stringify(f.resolvedOptions())+' '+V.map(v=>P(f.formatToParts(v))).join(' ; '))}};"
G = {
 "decimal": [{}, {"minimumFractionDigits": 2}, {"maximumFractionDigits": 0}, {"maximumSignificantDigits": 3}, {"minimumSignificantDigits": 5},
             {"useGrouping": False}, {"useGrouping": "always"}, {"useGrouping": "min2"}, {"minimumIntegerDigits": 3}, {"trailingZeroDisplay": "stripIfInteger", "minimumFractionDigits": 2}],
 "currency": [{"style": "currency", "currency": c, "currencyDisplay": d} for c in ["USD", "EUR", "JPY", "BRL", "CNY", "INR"] for d in ["symbol", "narrowSymbol", "code", "name"]]
             + [{"style": "currency", "currency": c, "currencySign": "accounting"} for c in ["USD", "EUR", "CHF"]],
 "unit": [{"style": "unit", "unit": u, "unitDisplay": d} for u in ["kilometer-per-hour", "percent", "megabyte", "celsius", "liter", "mile-per-gallon", "day", "percent-per-second"] for d in ["short", "long", "narrow"]],
 "compact": [{"notation": "compact"}, {"notation": "compact", "compactDisplay": "long"}, {"style": "percent", "notation": "compact"},
             {"style": "currency", "currency": "USD", "notation": "compact"}, {"style": "currency", "currency": "EUR", "notation": "compact", "compactDisplay": "long", "currencyDisplay": "name"},
             {"style": "currency", "currency": "USD", "currencySign": "accounting", "notation": "compact"}, {"notation": "compact", "maximumFractionDigits": 2},
             {"style": "unit", "unit": "percent", "notation": "compact", "compactDisplay": "long"}, {"notation": "compact", "useGrouping": "auto"}],
 "scientific": [{"notation": "scientific"}, {"notation": "engineering"}, {"notation": "scientific", "maximumFractionDigits": 2},
                {"style": "currency", "currency": "GBP", "notation": "scientific"}, {"style": "currency", "currency": "INR", "currencyDisplay": "name", "notation": "scientific"},
                {"style": "percent", "notation": "engineering"}, {"style": "unit", "unit": "meter", "unitDisplay": "long", "notation": "scientific"}],
 "rounding": [{"signDisplay": s} for s in ["always", "exceptZero", "never", "negative"]]
             + [{"roundingMode": m, "maximumFractionDigits": 1} for m in ["ceil", "floor", "expand", "trunc", "halfCeil", "halfFloor", "halfTrunc", "halfEven"]]
             + [{"roundingIncrement": i, "maximumFractionDigits": 2, "minimumFractionDigits": 2} for i in [5, 25, 50]]
             + [{"roundingPriority": p, "maximumSignificantDigits": 2, "maximumFractionDigits": 2} for p in ["morePrecision", "lessPrecision"]],
}
RANGE = r"""const R=[[3,5],[1000,1000],[-5,2.5],[0.9,1.1],[1,1],['1.5','2'],[1n,3n],[0,1e21]];
for(const o of [{},{style:'currency',currency:'EUR'},{style:'currency',currency:'CNY'},{style:'percent'},{style:'unit',unit:'kilometer',unitDisplay:'long'},{style:'unit',unit:'percent'},{notation:'compact'},{notation:'compact',compactDisplay:'long'},{notation:'scientific'},{style:'currency',currency:'USD',signDisplay:'always'},{style:'currency',currency:'INR',currencyDisplay:'name'}]){
 const f=new Intl.NumberFormat(L,o);out.push(JSON.stringify(o)+' '+R.map(([a,b])=>f.formatRange(a,b)+' = '+f.formatRangeToParts(a,b).map(p=>p.type+':'+p.value+':'+p.source).join('|')).join(' ; '))}
for(const [a,b] of [[NaN,1],[1,NaN],[undefined,1],[1,undefined]]){try{new Intl.NumberFormat(L).formatRange(a,b);out.push('no throw')}catch(e){out.push(e.name)}}"""
cases = []
for loc in L:
    tag = loc.lower()
    for g, os_ in G.items():
        cases.append({"id": f"intl-number-{tag}-{g}", "kind": "js", "src": H + "const L=" + json.dumps(loc) + ";run(L," + json.dumps(os_) + ");text(out.join('\\n'))"})
    cases.append({"id": f"intl-number-{tag}-range", "kind": "js", "src": H + "const L=" + json.dumps(loc) + ";" + RANGE + ";text(out.join('\\n'))"})
# PluralRules, ListFormat and RelativeTimeFormat: the matrix's misc case without the APIs djs lacks.
src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "matrix.py")).read()
MISC = src.split('MISC = r"""')[1].split('"""')[0]
MISC = "\n".join(l for l in MISC.split("\n") if not l.startswith(("r.push('ex|", "r.push('dn|", "r.push('loc", "r.push('svo|", "r.push('seg|", "for(const v of")))
PRE = "const P=(a)=>a.map(p=>p.type+':'+p.value).join('|');const tryf=(f)=>{try{return f()}catch(e){return 'E:'+e.name}};"
for loc in L:
    cases.append({"id": f"intl-number-{loc.lower()}-plural-list-relative", "kind": "js", "src": "const LOC=" + json.dumps(loc) + ";" + PRE + MISC})
TLS = r"""const out=[];for(const L of ['en-US','de','fr','es','ja','zh','pt-BR',undefined]){for(const v of [0,-1234.5,1234567.891,0.000123,NaN,-Infinity,1e21,12.345])out.push(L+' '+v.toLocaleString(L)+' | '+v.toLocaleString(L,{style:'currency',currency:'EUR'})+' | '+v.toLocaleString(L,{maximumFractionDigits:1,notation:'compact'}));
for(const v of [0n,-1234n,1234567n,12345678901234567890n])out.push(L+' '+v.toLocaleString(L)+' | '+v.toLocaleString(L,{style:'currency',currency:'JPY'})+' | '+v.toLocaleString(L,{notation:'scientific'}))}text(out.join('\n'))"""
cases.append({"id": "intl-number-tolocalestring", "kind": "js", "src": TLS})
here = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(here, "intl-number.jsonl"), "w") as f:
    for c in cases: f.write(json.dumps(c, ensure_ascii=False) + "\n")
print(len(cases), "cases")
