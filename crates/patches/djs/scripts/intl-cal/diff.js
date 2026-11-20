// diff.js CAL [TAGS...]: prints one JSON line per case (tag, options, date) with formatToParts,
// format, resolvedOptions().calendar/locale and toLocale*String, for running under node and djs and
// comparing (check.sh). Deterministic: dates come from a fixed LCG over 1900-2100.
const ARGS = typeof CAL_ARGS !== "undefined" ? CAL_ARGS : process.argv.slice(2);
const cal = ARGS[0];
const NATIVE = {buddhist: [], roc: ["zh-TW"], japanese: ["ja"], persian: [], "islamic-civil": ["ar"], "islamic-tbla": ["ar"], "islamic-umalqura": ["ar"], islamic: ["ar"], "islamic-rgsa": ["ar"],
  hebrew: ["he"], indian: ["hi"], coptic: ["ar"], ethiopic: [], ethioaa: [], chinese: ["zh"], dangi: ["ko"], iso8601: []};
const tags = ARGS.length > 1 ? ARGS.slice(1) : ["en-US", ...NATIVE[cal]];
let seed = 12345;
const rnd = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648;
const LO = Date.UTC(1900, 0, 1), HI = Date.UTC(2101, 0, 1);
const W = ["long", "short", "narrow"], N = ["numeric", "2-digit"], M = ["numeric", "2-digit", "long", "short", "narrow"];
const pick = a => a[Math.floor(rnd() * a.length)];
const optSets = [{}, {dateStyle: "full"}, {dateStyle: "long"}, {dateStyle: "medium"}, {dateStyle: "short"}, {dateStyle: "full", timeStyle: "short"}, {dateStyle: "short", timeStyle: "medium"},
  {year: "numeric", month: "long", day: "numeric"}, {era: "long", year: "numeric"}, {month: "short", day: "numeric"}, {year: "numeric", month: "numeric"}, {weekday: "long"}, {era: "short"},
  {year: "numeric", month: "long", day: "numeric", hour: "numeric", minute: "2-digit"}, {timeZoneName: "short", year: "numeric", month: "short", day: "numeric"}];
for (let k = 0; k < 60; k++) { const o = {}; if (rnd() < .3) o.weekday = pick(W); if (rnd() < .3) o.era = pick(W); if (rnd() < .7) o.year = pick(N); if (rnd() < .7) o.month = pick(M); if (rnd() < .7) o.day = pick(N); if (rnd() < .2) o.hour = "numeric"; if (rnd() < .1) o.timeZoneName = "short"; optSets.push(o); }
const out = [];
for (const tag of tags) {
  const loc = tag + "-u-ca-" + cal;
  for (const o of optSets) {
    let f; try { f = new Intl.DateTimeFormat(loc, {timeZone: "UTC", ...o}); } catch (e) { out.push(JSON.stringify({loc, o, err: String(e)})); continue; }
    const ro = f.resolvedOptions();
    for (let i = 0; i < 25; i++) {
      const t = Math.floor(LO + rnd() * (HI - LO));
      out.push(JSON.stringify({loc, o, t, cal: ro.calendar, locale: ro.locale, parts: f.formatToParts(t).map(p => p.type + ":" + p.value).join("|")}));
    }
  }
  const d = new Date(Date.UTC(2023, 2, 25, 13, 5));
  out.push(JSON.stringify({loc, tl: [d.toLocaleDateString(loc, {timeZone: "UTC"}), d.toLocaleString(loc, {timeZone: "UTC"}), d.toLocaleTimeString(loc, {timeZone: "UTC"})]}));
  out.push(JSON.stringify({loc, opt: new Intl.DateTimeFormat(tag, {calendar: cal}).resolvedOptions().calendar}));
}
console.log(out.join("\n"));

