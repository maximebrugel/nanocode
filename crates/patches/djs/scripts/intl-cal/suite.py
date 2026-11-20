#!/usr/bin/env python3
# suite.py > intl-dtf-cal.jsonl: tempoxyz/nanocode-rt parity cases for DateTimeFormat calendars
# (one case per calendar x locale, plus option resolution), diffed against reference node v22.23.5.
import json, random
CALS = {"buddhist": [], "roc": ["zh-TW"], "japanese": ["ja"], "persian": [], "islamic-civil": ["ar"], "islamic-tbla": ["ar"],
        "islamic-umalqura": ["ar"], "islamic": ["ar"], "islamic-rgsa": ["ar"], "hebrew": ["he"], "indian": ["hi"], "coptic": ["ar"],
        "ethiopic": [], "ethioaa": [], "chinese": ["zh"], "dangi": ["ko"], "iso8601": []}
rnd = random.Random(22235)
SPECIAL = ["1912-07-29T12:00:00Z", "1912-07-30T00:00:00Z", "1926-12-25T00:00:00Z", "1989-01-07T23:59:59Z", "1989-01-08T00:00:00Z", "2019-04-30T12:00:00Z",
           "2019-05-01T00:00:00Z", "2023-03-22T00:00:00Z", "2023-04-20T00:00:00Z", "2024-03-15T00:00:00Z", "2024-03-21T00:00:00Z", "2025-09-11T00:00:00Z", "2000-02-29T13:05:00Z"]
DATES = SPECIAL + [f"{rnd.randint(1900, 2100)}-{rnd.randint(1, 12):02d}-{rnd.randint(1, 28):02d}T{rnd.randint(0, 23):02d}:{rnd.randint(0, 59):02d}:00Z" for _ in range(27)]
OPTS = [{}, {"dateStyle": "full"}, {"dateStyle": "long"}, {"dateStyle": "medium"}, {"dateStyle": "short"}, {"dateStyle": "long", "timeStyle": "short"},
        {"year": "numeric", "month": "long", "day": "numeric"}, {"era": "long", "year": "numeric", "month": "short", "day": "numeric"}, {"year": "2-digit", "month": "2-digit", "day": "2-digit"},
        {"month": "long"}, {"year": "numeric", "month": "narrow"}, {"weekday": "short", "month": "numeric", "day": "numeric"}, {"era": "short"},
        {"year": "numeric", "month": "long", "day": "numeric", "hour": "numeric", "minute": "2-digit"}, {"year": "numeric", "month": "short", "day": "numeric", "timeZoneName": "short"}]
out = []
for cal, natives in CALS.items():
    for loc in ["en-US"] + natives:
        tag = f"{loc}-u-ca-{cal}"
        src = (f"const D={json.dumps(DATES)}.map(s=>new Date(s));const out=[];for(const o of {json.dumps(OPTS)})"
               f"{{const f=new Intl.DateTimeFormat({json.dumps(tag)},{{timeZone:'UTC',...o}});const r=f.resolvedOptions();"
               "out.push(JSON.stringify(o)+' '+r.locale+' '+r.calendar);for(const d of D)out.push(f.formatToParts(d).map(p=>p.type+':'+p.value).join('|'))}"
               f"const d=D[5];out.push(d.toLocaleDateString({json.dumps(tag)},{{timeZone:'UTC'}}),d.toLocaleString({json.dumps(tag)},{{timeZone:'UTC'}}));text(out.join('\\n'))")
        out.append({"id": f"intl-dtf-cal-{cal}-{loc}", "kind": "js", "src": src})
res = ("const out=[];for(const [l,o] of [['en-US-u-ca-islamicc',{}],['en-US-u-ca-ethiopic-amete-alem',{}],['en-US-u-ca-foo',{}],['en-US',{calendar:'foo'}],"
       "['en-US',{calendar:'buddhist'}],['en-US-u-ca-roc',{calendar:'buddhist'}],['en-US-u-ca-roc',{calendar:'roc'}],['en',{calendar:'ISLAMIC-civil'}],"
       "['en-US-u-nu-arab-ca-buddhist',{}],['en-US-u-ca-gregory',{}],['ja-JP-u-ca-japanese',{}],['en-US',{calendar:'x'}],['en',{calendar:'islamic-'}]])"
       "{try{const r=new Intl.DateTimeFormat(l,o).resolvedOptions();out.push(r.locale+' '+r.calendar)}catch(e){out.push(e.constructor.name+': '+e.message)}}text(out.join('\\n'))")
out.append({"id": "intl-dtf-cal-resolve", "kind": "js", "src": res})
for c in out:
    print(json.dumps(c, ensure_ascii=False))

