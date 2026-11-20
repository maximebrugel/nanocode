#!/usr/bin/env python3
"""Writes intl-locale.jsonl: the Locale, getCanonicalLocales, supportedValuesOf, DisplayNames
and Segmenter parity suite (each case prints with text(); the parity runner compares djs with
node 22). Every case matched node in diff.py-style runs; CJK and Thai word segmentation (ICU's
dictionaries) are left out."""
import json, os
T = "const tryf=(f)=>{try{const v=f();return v===undefined?'undefined':JSON.stringify(v)}catch(e){return 'E:'+e.name+':'+e.message}};const out=[];const run=(es)=>{for(const e of es)out.push(e+' => '+tryf(()=>(0,eval)(e)))};"
cases = []
def case(id, exprs):
    src = T + "run(" + json.dumps(exprs, ensure_ascii=False) + ");text(out.join('\\n'))"
    cases.append({"id": "intl-locale-" + id, "kind": "js", "src": src})
G = "toString() baseName language script region calendar caseFirst collation hourCycle numeric numberingSystem".split()
I = "hourCycles calendars collations numberingSystems timeZones textInfo weekInfo".split()
def getters(tags): return ["((l)=>[" + ",".join("l." + g for g in G) + ",l.maximize().toString(),l.minimize().toString()])(new Intl.Locale(" + json.dumps(t) + "))" for t in tags]
def info(tags): return ["((l)=>[" + ",".join("l." + g for g in I) + ",typeof l.getWeekInfo])(new Intl.Locale(" + json.dumps(t) + "))" for t in tags]
for i, tags in enumerate([["en-US", "de", "fr-CA", "es-419", "ja-JP"], ["zh-Hant-TW", "zh", "pt-PT", "ko", "ru"], ["ar-EG", "fa-IR", "he", "th-TH", "hi-IN"],
                          ["en-u-ca-buddhist-hc-h11-nu-thai-co-phonebk-kf-upper-kn", "en-Latn-US-posix", "sr-Latn", "und", "und-Cyrl"],
                          ["en-GB-u-rg-uszzzz", "en-u-fw-tue", "ar-u-nu-latn", "en-u-kn-false", "de-CH-1996-x-foo"]]):
    case("getters-%d" % i, getters(tags))
    case("info-%d" % i, info(tags))
canon = ["iw", "in", "ji", "mo", "sh", "tl", "cmn-Hans", "zh-min-nan", "i-klingon", "art-lojban", "no-bok", "sgn-GR", "hy-arevela", "hy-SU", "ru-SU", "und-SU", "en-BU",
  "en-DD", "en-Qaai", "en-u-ca-islamicc", "en-u-kb-yes", "en-u-tz-est5edt", "en-u-ms-imperial", "en-u-rg-no23", "en-u-sd-no23", "EN-u-CA-Gregory", "en-u-attr2-attr1",
  "en-u-ca-gregory-ca-buddhist", "en-z-zz-a-aa-u-ca-x1", "en-t-en-latn-us-h0-hybrid", "en-T-IW-M0-NAMES", "ja-Latn-hepburn-heploc", "sl-rozaj-biske-1994", "de-1996-1901",
  "x-private", "en-x", "en-u", "en_US", "en--US", "en-US-US", "en-Latn-Cyrl", "en-a-aa-a-bb", "abcdefghi", "root", "en-variant-variant", "en-12"]
for i in range(0, len(canon), 12):
    case("canonical-%d" % (i // 12), ["Intl.getCanonicalLocales(" + json.dumps(t) + ")" for t in canon[i:i + 12]])
case("canonical-args", ["Intl.getCanonicalLocales()", "Intl.getCanonicalLocales(null)", "Intl.getCanonicalLocales('')", "Intl.getCanonicalLocales([5])",
  "Intl.getCanonicalLocales({length:2,0:'en',1:'EN'})", "Intl.getCanonicalLocales(['en-US','en-us','fr'])", "Intl.getCanonicalLocales(new Intl.Locale('en-us'))", "Intl.getCanonicalLocales.length"])
case("constructor", ["new Intl.Locale('en',{region:'USA'})", "new Intl.Locale('en',{script:'La'})", "new Intl.Locale('en',{calendar:'x'})", "new Intl.Locale('en',{hourCycle:'h25'})",
  "new Intl.Locale('en',{numeric:'yes'}).toString()", "new Intl.Locale('')", "new Intl.Locale()", "new Intl.Locale(5)", "new Intl.Locale({toString(){return 'de-AT'}}).toString()",
  "Intl.Locale('en')", "new Intl.Locale('en',null)", "new Intl.Locale('en-US',{region:'GB',script:'Cyrl'}).toString()", "new Intl.Locale(new Intl.Locale('en-u-ca-gregory'),{calendar:'buddhist'}).toString()",
  "new Intl.Locale('en',{region:'GB',calendar:'gregory',hourCycle:'h23',numeric:true,caseFirst:'false',script:'latn'}).toString()", "new Intl.Locale('en',{language:'iw'}).toString()",
  "Intl.Locale.length", "Object.prototype.toString.call(new Intl.Locale('en'))", "Intl.Locale.prototype.maximize.call(1)"])
mm = ["sh", "und-RU", "und-Hant", "zh-Hant", "zh-Hant-TW", "zh-Hans-CN", "sr-Cyrl-RS", "sr-Latn-RS", "es-419", "pt-PT", "xyz", "xyz-Arab", "und-Latn-EG", "und-Zzzz-ZZ",
  "en-Zzzz-ZZ", "ja-Kana", "az-IR", "pa-PK", "und-150", "und-AQ", "eo-Zzzz-ZZ", "und-Arab-MM", "mn-Mong", "en-Latn-ZZ"]
case("maximize-minimize", ["((l)=>[l.maximize().toString(),l.minimize().toString()])(new Intl.Locale(" + json.dumps(t) + "))" for t in mm])
case("supported-values", ["Intl.supportedValuesOf(" + json.dumps(k) + ")" for k in ["calendar", "collation", "currency", "numberingSystem", "timeZone", "unit"]] + ["Intl.supportedValuesOf('x')", "Intl.supportedValuesOf()"])
LANG = ["en-US", "en-GB", "zh-Hant-TW", "sr-Latn", "xyz", "xyz-US", "xyz-Latn-TW", "de-CH-1996", "es-419", "pt-BR", "fr-CA", "ar-001", "sw-CD", "en_US", "und", "en-u-ca-gregory"]
OTHER = {"region": ["US", "GB", "419", "SU", "ZZ", "AA", "999", "us", "USA"], "script": ["Latn", "Hant", "Cyrl", "Qaaa", "Lat"], "currency": ["USD", "EUR", "JPY", "XYZ", "usd", "US"],
  "calendar": ["gregory", "islamic-umalqura", "ethiopic-amete-alem", "islamicc", "xx"], "dateTimeField": ["era", "year", "weekOfYear", "dayPeriod", "timeZoneName", "foo"]}
for loc in ["en", "de", "fr", "es", "it", "pt", "nl", "ja", "zh", "ko", "ru", "zh-Hant", "pt-PT", "en-GB", "es-MX", "fr-CA", "de-AT"]:
    ex = []
    for style in ["long", "short", "narrow"]:
        for ld in ["dialect", "standard"]:
            ex.append("((d)=>" + json.dumps(LANG) + ".map((c)=>tryf(()=>d.of(c))))(new Intl.DisplayNames(" + json.dumps(loc) + ",{type:'language',style:'" + style + "',languageDisplay:'" + ld + "'}))")
        for ty, cs in OTHER.items():
            ex.append("((d)=>" + json.dumps(cs) + ".map((c)=>tryf(()=>d.of(c))))(new Intl.DisplayNames(" + json.dumps(loc) + ",{type:'" + ty + "',style:'" + style + "'}))")
    ex.append("((d)=>[d.of('xyz'),d.of('xyz-US'),d.resolvedOptions()])(new Intl.DisplayNames(" + json.dumps(loc) + ",{type:'language',fallback:'none'}))")
    case("displaynames-" + loc.lower(), ex)
case("displaynames-errors", ["new Intl.DisplayNames('en')", "new Intl.DisplayNames('en',{})", "new Intl.DisplayNames('en',{type:'x'})", "new Intl.DisplayNames('en',{type:'language',style:'x'})",
  "Intl.DisplayNames('en',{type:'region'})", "new Intl.DisplayNames('en',{type:'language'}).of()", "Intl.DisplayNames.length", "Intl.DisplayNames.supportedLocalesOf(['en','xx','de-AT'])",
  "Object.prototype.toString.call(new Intl.DisplayNames('en',{type:'region'}))", "Intl.DisplayNames.prototype.of.call({}, 'en')", "new Intl.DisplayNames('xx',{type:'language'}).resolvedOptions()"])
SEG = ["", "a", "Hello, world! How are you?", "U.S.A. is big. Mr. Smith left.", "3.14 is pi; 1,000,000.", "can't won't O'Neil", "e\u0301\u0301x", "\U0001F468\u200d\U0001F469\u200d\U0001F467\u200d\U0001F466\U0001F44D\U0001F3FD\U0001F1FA\U0001F1F8\U0001F1E9\U0001F1EA\U0001F1EB",
  "\U0001F3F3\ufe0f\u200d\U0001F308 ok", "\u0928\u092e\u0938\u094d\u0924\u0947 \u0915\u094d\u0937\u0924\u094d\u0930\u093f\u092f", "\u0995\u09cd\u09b7\u09cd\u09ae\u09c0", "\ud55c\uad6d\uc5b4 \ud14d\uc2a4\ud2b8", "\u041f\u0440\u0438\u0432\u0435\u0442, \u043c\u0438\u0440. \u041a\u0430\u043a \u0434\u0435\u043b\u0430?",
  "a\r\nb\nc", "\ud800x\udc00", "foo_bar baz-qux", "\u03b1\u03b2\u03b3. \u0394\u03ad\u03bb\u03c4\u03b1", "\u05e9\u05dc\u05d5\u05dd \u05e2\u05d5\u05dc\u05dd", "x\u200dy", "$12.50 or \u20ac3", "e.g. this", "end.\u2029Next", "(Quote.) Next", "\u2018single\u2019 \u201cdouble\u201d"]
for g in ["grapheme", "word", "sentence"]:
    case("segmenter-" + g, ["((s)=>" + json.dumps(SEG) + ".map((t)=>[...s.segment(t)].map((x)=>[x.segment,x.index,x.isWordLike])))(new Intl.Segmenter('en',{granularity:'" + g + "'}))",
      "((s)=>[0,3,7,8,26,-1,1.5].map((i)=>s.containing(i)))(new Intl.Segmenter('en',{granularity:'" + g + "'}).segment('Hello, world! How are you?'))"])
case("segmenter-api", ["new Intl.Segmenter().resolvedOptions()", "new Intl.Segmenter('de',{granularity:'word'}).resolvedOptions()", "new Intl.Segmenter('en',{granularity:'x'})", "Intl.Segmenter()",
  "Intl.Segmenter.length", "Object.prototype.toString.call(new Intl.Segmenter())", "((it)=>[Object.prototype.toString.call(it),it.next(),it.next(),it.next()])(new Intl.Segmenter().segment('ab')[Symbol.iterator]())",
  "Intl.Segmenter.prototype.segment.call({}, 'x')", "[...new Intl.Segmenter('en',{granularity:'word'}).segment({toString(){return 'a b'}})].length",
  "((s)=>Object.prototype.toString.call(s)+' '+typeof s.containing)(new Intl.Segmenter().segment('x'))"])
here = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(here, "intl-locale.jsonl"), "w") as f:
    for c in cases: f.write(json.dumps(c, ensure_ascii=False) + "\n")
print(len(cases), "cases")
