// Differential matrix for Intl.Locale, getCanonicalLocales, supportedValuesOf (and later
// DisplayNames, Segmenter): prints JSONL cases {id, src}; each src prints one line per
// expression, "group|expr => value". Run under node 22: node matrix.js CLDR [group...].
const fs = require("fs"), path = require("path");
const DIR = process.argv[2];
const only = process.argv.slice(3);
const J = (f) => JSON.parse(fs.readFileSync(path.join(DIR, f), "utf8"));
const PRE = "const __show = (v) => { if (v instanceof Intl.Locale) return 'Locale(' + v.toString() + ')'; if (typeof v === 'function') return 'fn'; if (v === undefined) return 'undefined'; return JSON.stringify(v); };" +
  "const __run = (g, es, fs) => { for (let i = 0; i < es.length; i++) { let r; try { r = __show(fs[i]()); } catch (x) { r = (x && x.constructor && x.constructor.name) + ': ' + (x && x.message); } text(g + '|' + es[i] + ' => ' + r); } };";
const cases = [];
const add = (group, exprs, chunk = 400) => {
  if (only.length && !only.includes(group)) return;
  for (let i = 0; i < exprs.length; i += chunk) { const es = exprs.slice(i, i + chunk); cases.push({ id: group + "@" + i, src: PRE + "__run(" + JSON.stringify(group) + "," + JSON.stringify(es) + ",[" + es.map((e) => "() => (" + e + ")").join(",\n") + "]);" }); }
};
const q = JSON.stringify;
const likely = Object.keys(J("likelySubtags.json").supplemental.likelySubtags);
const A = J("aliases.json").supplemental.metadata.alias;
const avail = J("availableLocales.json").availableLocales.full;
// ---- getCanonicalLocales ----
const canon = [
  "Intl.getCanonicalLocales()", "Intl.getCanonicalLocales(null)", "Intl.getCanonicalLocales([])", "Intl.getCanonicalLocales('')",
  "Intl.getCanonicalLocales(5)", "Intl.getCanonicalLocales([5])", "Intl.getCanonicalLocales({length:2,0:'en',1:'EN'})",
  "Intl.getCanonicalLocales(['en-US','en-us','fr'])", "Intl.getCanonicalLocales(new Intl.Locale('en-us'))",
  "Intl.getCanonicalLocales([new Intl.Locale('de'),'de'])", "Intl.getCanonicalLocales.length", "Intl.supportedValuesOf.length",
];
const tags = ["root","und","en-US-u-va-posix","en-u-ca-gregory-ca-buddhist","en-u-cu-usd-ca-gregory","en-z-zz-a-aa-u-ca-x1","en-a-aa-a-bb",
  "en-u-ca-gregory-u-nu-thai","en-variant-variant","en-zzzzz-aaaaa","de-1996-1901","sl-rozaj-biske-1994","en-t-en-latn-us-h0-hybrid",
  "en-T-IW-M0-NAMES","und-Latn-t-und-hani-m0-names","ja-Latn-hepburn-heploc","hy-arevela","hy-arevmda","ru-SU","hy-SU","und-SU","en-062",
  "en-u-rg-no23","en-u-sd-no23","en-u-tz-est5edt","en-u-ms-imperial","en-u-ks-primary","en-u-ca-islamicc","en-u-kb-yes","cmn-Hans",
  "zh-min-nan","zh-hakka","i-klingon","x-private","en-x-a-b-c","en-x","en-u-x-a","abcd","abcde","abcdefghi","en-Latn-US-1abc","en-1ab",
  "en-12","en-Latn-Cyrl","en-US-GB","mo","sh","sh-Cyrl","tl","iw-IL","ji","in","aar","eng-US","en-BU","en-DD","en-Qaai","sgn-GR","no-bok-NO",
  "art-lojban","en-polytoni","cel-gaulish","en-u-ca-gregory-co","en-u-attr1-attr2-ca-gregory","en-u-attr2-attr1","EN-u-CA-Gregory",
  "en-u-kn-false-kn-true","en-U-ca-GREGORY","en-Latn-US-posix","en-","en--US","en_US","a","1234","en-u","en-US-US"," en","en-a-x-y",
  "en-u-kn-true","en-u-kn","EN-latn-us-u-CA-gregory","zh-cmn","en-US-u-ca-ethiopic-amete-alem","en-a-bbb-u-ca-gregory","und-u-rg-gbzzzz",
  "en-u-tz-usnyc","no-bok","cmn-hans-cn","ja-u-ca-japanese-t-it","en-t-zh-hant","aa-SAAHO","en-u-co-phonebk-ka-shifted","en-u-co-dict",
  "en-u-ca-ethiopic-amete-alem","en-u-ks-level1","en-u-kf-yes","en-u-ka-posix","en-u-ms-uksystem","en-u-tz-aqams","en-u-tz-cnckg",
  "en-u-nu-traditio","en-u-hc-h25","de-DE-u-co-phonebk","sr-Latn-ME","x-foo","en-x-foo","de-CH-1996-x-foo","en-t-und-m0-names-x0-a",
  "en-u-0a-foo","en-u-aa-foo-u","en-t","en-t-zz","en-t-ab-cdef","en-t-a1-bcd","en-a","en-x-123456789","en-abcdefgh-x-1","EN-GB-OED",
  "zh-guoyu","zh-xiang","zh-yue","sgn-BR","art-lojban-x-y","und-Qaaa","und-XA","und-AA","und-QM","en-SU","en-YU","en-CS","en-AN","en-NT",
  "ky-Cyrl","az-Arab-IR-u-nu-arabext","tw","ckb-IQ","prs","swc","hbs","hbs-Latn","cnr","yue-Hant","sh-Latn-RS","sh-ME","sr-Cyrl-YU",
  "hy-Latn-arevela","en-scouse","sv-aaland","ja-hepburn-heploc","de-u-ks-level2-kn-false-kc-true","en-u-dx-thai-ss-standard","en-u-em-emoji",
  "en-u-lb-strict","en-u-lw-breakall","en-u-fw-mon","en-u-cf-account","en-u-mu-celsius","en-u-rg-ustx","en-u-sd-ustx","en-u-ms-metric-mu-x"];
for (const t of tags) canon.push("Intl.getCanonicalLocales(" + q(t) + ")");
const lang = Object.keys(A.languageAlias).map((k) => k.replace(/_/g, "-"));
for (const k of lang) canon.push("Intl.getCanonicalLocales(" + q(k) + ")");
for (const k of Object.keys(A.territoryAlias)) canon.push("Intl.getCanonicalLocales(" + q("und-" + k) + ")", "Intl.getCanonicalLocales(" + q("hy-" + k) + ")");
for (const k of Object.keys(A.scriptAlias)) canon.push("Intl.getCanonicalLocales(" + q("en-" + k) + ")");
for (const k of Object.keys(A.variantAlias)) canon.push("Intl.getCanonicalLocales(" + q("en-" + k) + ")");
for (const k of Object.keys(A.subdivisionAlias).slice(0, 400)) canon.push("Intl.getCanonicalLocales(" + q("en-u-rg-" + k) + ")");
add("canon", canon);
// ---- Locale constructor and getters ----
const ctor = [
  "new Intl.Locale('en',{firstDayOfWeek:'mon'}).toString()", "new Intl.Locale('en',{region:'USA'})", "new Intl.Locale('en',{language:'x'})",
  "new Intl.Locale('en',{script:'La'})", "new Intl.Locale('en',{calendar:'x'})", "new Intl.Locale('en',{calendar:'abc-def'}).toString()",
  "new Intl.Locale('en',{collation:'ab'})", "new Intl.Locale('en',{hourCycle:'h25'})", "new Intl.Locale('en',{caseFirst:'up'})",
  "new Intl.Locale('en',{numberingSystem:'latn-x'})", "new Intl.Locale('en',{numeric:'yes'}).toString()", "new Intl.Locale('en',{numeric:0}).toString()",
  "new Intl.Locale('')", "new Intl.Locale()", "new Intl.Locale(5)", "new Intl.Locale(null)", "new Intl.Locale({toString(){return 'de-AT'}}).toString()",
  "Intl.Locale('en')", "new Intl.Locale('en',null)", "new Intl.Locale('en',5).toString()", "new Intl.Locale('en-u-ca-gregory',{}).calendar",
  "new Intl.Locale('en-US',{region:'GB',script:'Cyrl'}).toString()", "new Intl.Locale('en-US',{language:'fr'}).toString()",
  "new Intl.Locale(new Intl.Locale('en-u-ca-gregory'),{calendar:'buddhist'}).toString()", "new Intl.Locale('en',{numeric:false}).toString()",
  "new Intl.Locale('en',{region:'GB',calendar:'gregory',hourCycle:'h23',numeric:true,caseFirst:'false',script:'latn'}).toString()",
  "new Intl.Locale('en',{calendar:'islamicc'}).toString()", "new Intl.Locale('en',{language:'iw'}).toString()", "new Intl.Locale('en',{region:'SU'}).toString()",
  "new Intl.Locale('en',{hourCycle:'H23'})", "new Intl.Locale('en',{collation:'PHONEBK'}).toString()", "new Intl.Locale('en',{language:'en-US'})",
  "new Intl.Locale('en',{region:'419'}).toString()", "new Intl.Locale('en-u-ca-gregory',{calendar:undefined}).toString()",
  "Intl.Locale.length", "Intl.Locale.name", "Object.prototype.toString.call(new Intl.Locale('en'))", "String(new Intl.Locale('en-us'))",
  "Object.getOwnPropertyNames(Intl.Locale.prototype).sort().join()", "typeof Object.getOwnPropertyDescriptor(Intl.Locale.prototype,'weekInfo').get",
  "Intl.Locale.prototype.maximize.call(1)", "Object.getOwnPropertyDescriptor(Intl.Locale.prototype,'language').get.call({})",
  "Intl.Locale.prototype.toString.call(Intl.Locale.prototype)", "new Intl.Locale('en').maximize() instanceof Intl.Locale",
  "Reflect.construct(Intl.Locale,['en'],function(){}.constructor).toString()", "(() => { class L extends Intl.Locale {} return new L('de').maximize().toString(); })()",
  "new Intl.Locale('en-US').timeZones===new Intl.Locale('en-US').timeZones", "Object.keys(new Intl.Locale('en').weekInfo).join()",
];
const G = ["toString()", "baseName", "language", "script", "region", "calendar", "caseFirst", "collation", "hourCycle", "numeric", "numberingSystem", "maximize()", "minimize()"];
const I = ["hourCycles", "calendars", "collations", "numberingSystems", "timeZones", "textInfo", "weekInfo"];
const gt = [...tags.filter((t) => { try { new Intl.Locale(t); return true; } catch { return false; } }), "en-US", "ar-EG", "ja-JP", "de", "fa-IR", "he", "zh-Hant-TW",
  "en-u-ca-buddhist-hc-h11-nu-thai-co-phonebk-kf-upper-kn", "th-TH", "en-GB-u-rg-uszzzz", "en-u-rg-gbzzzz", "en-US-u-hc-h23", "ar-u-nu-latn", "en-u-kn-false", "en-u-kf", "en-u-ca"];
const getters = [...ctor];
for (const t of gt) getters.push("((l)=>[" + [...G, ...I].map((g) => "l." + g).join(",") + "])(new Intl.Locale(" + q(t) + "))");
add("locale", getters, 200);
// maximize/minimize of every likely-subtags key, and a few variants of each
const mm = [];
for (const k of likely) {
  for (const t of [k, k.replace(/^und/, "zxx")]) mm.push("((l)=>[l.maximize().toString(),l.minimize().toString()])(new Intl.Locale(" + q(t) + "))");
}
for (const k of likely.filter((k) => !k.startsWith("und"))) {
  const m = new Intl.Locale(k).maximize();
  for (const t of [m.language + "-" + m.script, m.language + "-" + m.region, m.language + "-" + m.script + "-" + m.region, m.language + "-Zzzz-ZZ", m.language + "-Latn", m.language + "-US"])
    mm.push("((l)=>[l.maximize().toString(),l.minimize().toString()])(new Intl.Locale(" + q(t) + "))");
}
add("maxmin", [...new Set(mm)], 1000);
// locale info of every available locale, and of each language in sample regions
const info = [];
const regions = ["US", "GB", "DE", "FR", "ES", "MX", "BR", "PT", "IT", "NL", "CH", "JP", "CN", "TW", "KR", "RU", "IN", "EG", "SA", "IR", "IL", "CA", "AU", "001", "419"];
const infoTags = new Set(avail);
for (const t of avail) if (!t.includes("-")) for (const r of regions) infoTags.add(t + "-" + r);
for (const t of infoTags) info.push("((l)=>[" + I.map((g) => "l." + g).join(",") + "])(new Intl.Locale(" + q(t) + "))");
add("info", info, 1000);
// ---- supportedValuesOf ----
add("svo", ["calendar", "collation", "currency", "numberingSystem", "timeZone", "unit", "x", "Calendar", undefined].map((k) => "Intl.supportedValuesOf(" + (k === undefined ? "" : q(k)) + ")").concat(["Intl.supportedValuesOf({toString(){return 'unit'}}).length", "Intl.supportedValuesOf(Symbol())"]));
// ---- DisplayNames ----
if (!only.length || only.includes("dn") || only.includes("dnx")) {
  const N = (f, k) => [...new Set(Object.keys(Object.values(J("names/" + f + ".json").main)[0].localeDisplayNames[k]).map((x) => x.split("-alt-")[0]))];
  const langs = N("languages", "languages"), regions = N("territories", "territories"), scripts = N("scripts", "scripts"), variants = N("variants", "variants").map((v) => v.toLowerCase());
  const currencies = Object.keys(Object.values(J("names/currencies.json").main)[0].numbers.currencies);
  const calendars = Intl.supportedValuesOf("calendar").concat(["ethiopic-amete-alem", "islamicc", "gregory", "Gregory", "xx", "abcdef", "gregorian"]);
  const fields = ["era", "year", "quarter", "month", "weekOfYear", "weekday", "day", "dayPeriod", "hour", "minute", "second", "timeZoneName", "foo"];
  const extraLang = ["en-US", "en-GB", "zh-Hant-TW", "zh-Hans", "zh-Hant", "sr-Latn", "sr-Cyrl-RS", "xyz", "xyz-US", "xyz-Latn-TW", "en-Latn-US-u-ca-gregory", "en_US", "EN-us", "root", "und", "iw", "en-x-foo", "de-CH-1996", "zh-Hant-TW-pinyin", "es-419", "pt-BR", "pt-PT", "fr-CA", "nl-BE", "ar-001", "en-AA", "en-Qaaa", "ja-Latn-JP-hepburn", "sl-rozaj-biske", "de-AT", "ko-KR", "ru-RU", "it-CH", "es-MX", "en-Zzzz-ZZ", "mo", "sh", "tl"];
  const codes = { language: [...langs, ...variants.map((v) => "xyz-" + v), ...extraLang], region: [...regions, "us", "SU", "ZZ", "AA", "999", "USA", "419", "XA"], script: [...scripts, "latn", "Qaaa", "Abcd", "Lat"], currency: [...currencies, "usd", "XYZ", "US"], calendar: calendars, dateTimeField: fields };
  const exprs = [];
  const LOC = only.includes("dnx") ? ["en-GB", "pt-PT", "zh-Hant", "de-AT", "es-MX", "fr-CA", "xx"] : ["en", "de", "fr", "es", "it", "pt", "nl", "ja", "zh", "ko", "ru"];
  for (const loc of LOC) for (const [type, cs] of Object.entries(codes)) for (const style of ["long", "short", "narrow"]) for (const ld of type === "language" ? ["dialect", "standard"] : ["dialect"]) for (const fb of ["code", "none"]) {
    if (fb === "none" && style !== "long") continue;
    const opts = q({ type, style, fallback: fb, languageDisplay: ld });
    for (let i = 0; i < cs.length; i += 40) exprs.push("((d)=>" + q(cs.slice(i, i + 40)) + ".map((c)=>{try{return d.of(c)}catch(e){return 'E:'+e.constructor.name+':'+e.message}}))(new Intl.DisplayNames(" + q(loc) + "," + opts + "))");
  }
  exprs.push(...["new Intl.DisplayNames('en')", "new Intl.DisplayNames('en',{})", "new Intl.DisplayNames('en',{type:'x'})", "new Intl.DisplayNames('en',{type:'language',style:'x'})", "new Intl.DisplayNames('en',{type:'language',languageDisplay:'x'})", "new Intl.DisplayNames('en',{type:'region',languageDisplay:'standard'}).resolvedOptions()", "new Intl.DisplayNames('en-US-u-ca-buddhist',{type:'language'}).resolvedOptions()", "new Intl.DisplayNames('xx',{type:'language'}).resolvedOptions()", "new Intl.DisplayNames(['en-GB'],{type:'region'}).resolvedOptions()", "new Intl.DisplayNames('zh-Hant',{type:'region'}).resolvedOptions()", "Intl.DisplayNames('en',{type:'region'})", "new Intl.DisplayNames('en',{type:'language'}).of()", "Object.getOwnPropertyNames(Intl.DisplayNames).join()", "Object.getOwnPropertyNames(Intl.DisplayNames.prototype).join()", "Intl.DisplayNames.length", "Intl.DisplayNames.supportedLocalesOf(['en','xx','de-AT'])", "Object.prototype.toString.call(new Intl.DisplayNames('en',{type:'region'}))", "Intl.DisplayNames.prototype.of.call({}, 'en')", "new Intl.DisplayNames('en',{type:'region',fallback:'x'})", "new Intl.DisplayNames('en',null)", "new Intl.DisplayNames('en',{type:'region',style:undefined}).resolvedOptions()"]);
  add(only.includes("dnx") ? "dnx" : "dn", exprs, 300);
}
// ---- Segmenter ----
if (!only.length || only.includes("seg")) {
  const fromTest = (f) => fs.readFileSync(path.join(DIR, "ucd", f), "utf8").split("\n").map((l) => l.split("#")[0].trim()).filter(Boolean).map((l) => String.fromCodePoint(...l.split(/\s*[÷×]\s*/).filter(Boolean).map((h) => parseInt(h, 16))));
  const extra = ["", "a", "Hello, world! How are you?", "U.S.A. is big. Mr. Smith left.", "3.14 is pi; 1,000,000.", "can't won't O'Neil", "e\u0301\u0301x", "👨‍👩‍👧‍👦👍🏽🇺🇸🇩🇪🇫", "🏳️‍🌈 ok", "नमस्ते क्षत्रिय", "ক্ষ্মী", "สวัสดีครับ ไทย", "日本語のテキストです。東京タワー", "中华人民共和国成立了", "한국어 텍스트", "Привет, мир. Как дела?", "a\r\nb\nc", "\ud800x\udc00", "foo_bar baz-qux", "αβγ. Δέλτα", "שלום עולם", "مرحبا بالعالم", "カタカナとひらがな", "ﾊﾝｶｸ", "x\u200dy", "$12.50 or €3", "e.g. this", "end.\u2029Next", "(Quote.) Next", "‘single’ “double”"];
  const strs = [...fromTest("GraphemeBreakTest.txt"), ...fromTest("WordBreakTest.txt"), ...fromTest("SentenceBreakTest.txt"), ...extra];
  const exprs = [];
  for (const g of ["grapheme", "word", "sentence"]) for (let i = 0; i < strs.length; i += 25) exprs.push("((s)=>" + q(strs.slice(i, i + 25)) + ".map((t)=>[...s.segment(t)].map((x)=>x.index+(x.isWordLike===undefined?'':x.isWordLike?'w':'n'))))(new Intl.Segmenter('en',{granularity:" + q(g) + "}))");
  for (const loc of ["ja", "zh", "th"]) exprs.push("((s)=>" + q(extra) + ".map((t)=>[...s.segment(t)].map((x)=>x.index+(x.isWordLike?'w':'n'))))(new Intl.Segmenter(" + q(loc) + ",{granularity:'word'}))");
  exprs.push(...["new Intl.Segmenter().resolvedOptions()", "new Intl.Segmenter('de',{granularity:'word'}).resolvedOptions()", "new Intl.Segmenter('en',{granularity:'x'})", "Intl.Segmenter()", "Intl.Segmenter.length", "Object.getOwnPropertyNames(Intl.Segmenter.prototype).join()", "Object.prototype.toString.call(new Intl.Segmenter())",
    "((s)=>[s.containing(0),s.containing(4),s.containing(-1),s.containing(99),s.containing('2'),s.containing(1.7),s.containing()])(new Intl.Segmenter('en',{granularity:'word'}).segment('Hi there'))",
    "((s)=>Object.getOwnPropertyNames(Object.getPrototypeOf(s)).join()+'/'+Object.prototype.toString.call(s))(new Intl.Segmenter().segment('x'))",
    "((it)=>[Object.prototype.toString.call(it),typeof it.next,it.next(),it.next(),it.next(),Object.getPrototypeOf(Object.getPrototypeOf(it))===Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]()))])(new Intl.Segmenter().segment('ab')[Symbol.iterator]())",
    "((s)=>{const a=[...s],b=[...s];return a.length===b.length})(new Intl.Segmenter().segment('abc'))", "Intl.Segmenter.prototype.segment.call({}, 'x')", "Object.getPrototypeOf(new Intl.Segmenter().segment('x')).containing.call({}, 0)",
    "Intl.Segmenter.supportedLocalesOf(['en','th','xx'])", "[...new Intl.Segmenter('en',{granularity:'word'}).segment({toString(){return 'a b'}})].length"]);
  add("seg", exprs, 60);
}
for (const c of cases) process.stdout.write(JSON.stringify(c) + "\n");


