
// gen.js LOC -> d-LOC.json : everything djs needs for one locale (run with the parity node)
const m = require('./infer.js');
const fs = require('fs');
const {WIDTHS, NUMS, MONTHS, CYC, LOC, names} = m;
const W3 = ["long", "short", "narrow"];
const SUF = process.env.DTZ ? '-' + process.env.DTZ.replace('/', '_') : '';
const T = JSON.parse(fs.readFileSync(__dirname + '/t-' + LOC + SUF + '.json'));
const tk = (t) => ((((t[0] * 3 + t[1]) * 3 + t[2]) * 7 + t[3]) * 4 + t[4]) * 4 + t[5];
function opts(f, t) { const o = {...CYC[t[3]]}; if (f[0]) o.weekday = WIDTHS[f[0]]; if (f[1]) o.era = WIDTHS[f[1]]; if (f[2]) o.year = NUMS[f[2]]; if (f[3]) o.month = MONTHS[f[3]]; if (f[4]) o.day = NUMS[f[4]];
  if (t[0]) o.hour = NUMS[t[0]]; if (t[1]) o.minute = NUMS[t[1]]; if (t[2]) o.second = NUMS[t[2]]; if (t[4]) o.dayPeriod = WIDTHS[t[4]]; if (t[5]) o.fractionalSecondDigits = t[5]; return o; }
const fixz = (p, k) => p.replace(/(^|[^'])z(?=[^']*$|[^']*'[^']*')/g, "$1" + k);
// time + short zone name
const timez = [];
for (let k = 0; k < 3024; k++) {
  let r = k; const fsd = r % 4; r = (r - fsd) / 4; const dp = r % 4; r = (r - dp) / 4; const cy = r % 7; r = (r - cy) / 7; const se = r % 3; r = (r - se) / 3; const mi = r % 3; const ho = (r - mi) / 3;
  const o = {...opts([0,0,0,0,0], [ho, mi, se, cy, dp, fsd]), timeZoneName: "short"};
  timez.push(m.infer(o).p);
}
// date + zone (no time): date pattern for each date key
const datez = [];
for (let k = 0; k < 864; k++) {
  let r = k; const f4 = r % 3; r = (r - f4) / 3; const f3 = r % 6; r = (r - f3) / 6; const f2 = r % 3; r = (r - f2) / 3; const f1 = r % 4; const f0 = (r - f1) / 4;
  const o = {...opts([f0, f1, f2, f3, f4], [0, 0, 0, 0, 0, 0]), timeZoneName: "short"};
  if (!(f0 || f1 || f2 || f3 || f4)) { o.year = "numeric"; o.month = "numeric"; o.day = "numeric"; }
  datez.push(m.infer(o).p);
}
// per zone-name kind: time keys whose pattern differs from timez (stored with z letters)
const KINDS = ["short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"];
const tex = KINDS.map(kind => {
  const ex = {};
  if (kind === "short") return ex;
  for (let k = 0; k < 3024; k++) {
    let r = k; const fsd = r % 4; r = (r - fsd) / 4; const dp = r % 4; r = (r - dp) / 4; const cy = r % 7; r = (r - cy) / 7; const se = r % 3; r = (r - se) / 3; const mi = r % 3; const ho = (r - mi) / 3;
    if (!(ho || mi || se || dp || fsd)) continue;
    const p = m.infer({...opts([0,0,0,0,0], [ho, mi, se, cy, dp, fsd]), timeZoneName: kind}).p;
    if (p !== timez[k]) ex[k] = p;
  }
  return ex;
});
// combined date + time glue, by month width (full = long month with weekday)
const dkk = (f) => (((f[0] * 4 + f[1]) * 3 + f[2]) * 6 + f[3]) * 3 + f[4];
const {tmpl, apply} = require('./glue.js');
const glueOf = (o, dp, tp) => tmpl(m.infer(o).p, dp, tp);
const t0 = T.time[tk([1, 1, 0, 4, 0, 0])];
const glue_full = glueOf({weekday: "long", month: "long", day: "numeric", hour: "numeric", minute: "numeric", hourCycle: "h23"}, T.date[dkk([1, 0, 0, 3, 1])], t0);
const glue_long = glueOf({month: "long", day: "numeric", hour: "numeric", minute: "numeric", hourCycle: "h23"}, T.date[dkk([0, 0, 0, 3, 1])], t0);
const glue_medium = glueOf({month: "short", day: "numeric", hour: "numeric", minute: "numeric", hourCycle: "h23"}, T.date[dkk([0, 0, 0, 4, 1])], t0);
const glue_short = glueOf({month: "numeric", day: "numeric", hour: "numeric", minute: "numeric", hourCycle: "h23"}, T.date[dkk([0, 0, 0, 1, 1])], t0);
// weekday-only + time: exceptions to weekday + short glue + time (key = width * 3024 + time key)
const wex = {};
for (let w = 1; w <= 3; w++) for (let k = 0; k < 3024; k++) {
  let r = k; const fsd = r % 4; r = (r - fsd) / 4; const dp = r % 4; r = (r - dp) / 4; const cy = r % 7; r = (r - cy) / 7; const se = r % 3; r = (r - se) / 3; const mi = r % 3; const ho = (r - mi) / 3;
  if (!(ho || mi || se || dp || fsd)) continue;
  const o = opts([w, 0, 0, 0, 0], [ho, mi, se, cy, dp, fsd]);
  const want = m.infer(o).p;
  if (want !== apply(glue_short, T.date[dkk([w, 0, 0, 0, 0])], T.time[k])) wex[w * 3024 + k] = want;
}
// localized GMT format: [prefix, minus sign]
const lo = new Intl.DateTimeFormat(LOC, {timeZone: "-03:00", timeZoneName: "shortOffset"}).formatToParts(0).find(p => p.type === "timeZoneName").value;
const hi = new Intl.DateTimeFormat(LOC, {timeZone: "+03:00", timeZoneName: "shortOffset"}).formatToParts(0).find(p => p.type === "timeZoneName").value;
const neg = lo.replace(/\u200e$/, "");
const gmt = [hi.slice(0, hi.indexOf("+")), neg.slice(neg.length - 2, neg.length - 1)];
// styles
const st = ["full", "long", "medium", "short"];
const styles = {date: st.map(s => m.infer({dateStyle: s}).p), time: [], both: []};
for (const c of CYC) styles.time.push(st.map(s => { let p = m.infer({timeStyle: s, ...c}).p; return s === "full" ? p.replace(/(^|[^z])z(?!z)/, "$1zzzz") : p; }));
for (const d of st) styles.both.push(st.map(t => { let p = m.infer({dateStyle: d, timeStyle: t}).p; return t === "full" ? p.replace(/(^|[^z])z(?!z)/, "$1zzzz") : p; }));
// resolved hourCycle per cycle option
const hcs = CYC.map(c => new Intl.DateTimeFormat(LOC, {hour: "numeric", ...c}).resolvedOptions().hourCycle);
const out = {loc: LOC, names, date: T.date, time: T.time, timez, tex, datez, wex, gmt, glue_full, glue_long, glue_medium, glue_short, styles, hcs, bad: T.bad.length};
fs.writeFileSync(__dirname + '/d-' + LOC + SUF + '.json', JSON.stringify(out));
console.log(LOC, "bad", T.bad.length, "wex", Object.keys(wex).length, "gmt", JSON.stringify(gmt), JSON.stringify([glue_full, glue_long, glue_medium, glue_short]));
