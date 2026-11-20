
// check.js LOC: compose patterns from d-LOC.json like djs will, compare to node on random options
const m = require('./infer.js');
const {apply} = require('./glue.js');
const fs = require('fs');
const {WIDTHS, NUMS, MONTHS, CYC, LOC} = m;
const D = JSON.parse(fs.readFileSync(__dirname + '/d-' + LOC + '.json'));
const dk = (f) => (((f[0] * 4 + f[1]) * 3 + f[2]) * 6 + f[3]) * 3 + f[4];
const tk = (t) => ((((t[0] * 3 + t[1]) * 3 + t[2]) * 7 + t[3]) * 4 + t[4]) * 4 + t[5];
const TZ = ["short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"], TL = ["z", "zzzz", "O", "OOOO", "v", "vvvv"];
const sub = (p, l) => { let o = "", q = false; for (let i = 0; i < p.length; i++) { const c = p[i]; if (c === "'") q = !q; if (!q && c === "z") { o += l; continue; } o += c; } return o; };
const glue = (() => { const g = {}; const t0 = D.time[tk([1, 1, 0, 4, 0, 0])];
  const pick = (f) => { const d = D.date[dk(f)]; return [d]; };
  return g; })();
function stripGlue(both, d, t) { return both.slice(d.length, both.length - t.length); }
const G = [D.glue_full, D.glue_long, D.glue_medium, D.glue_short];
function compose(f, t, tz) {
  const hasD = f.some(x => x), hasT = t[0] || t[1] || t[2] || t[4] || t[5];
  let p;
  if (!hasT) p = tz ? D.datez[dk(f)] : D.date[dk(f)];
  else {
    const ti = tz ? (D.tex[TZ.indexOf(tz)][tk(t)] || D.timez[tk(t)]) : D.time[tk(t)];
    if (!hasD) p = ti;
    else if (!tz && f[0] && !f[1] && !f[2] && !f[3] && !f[4] && D.wex[f[0] * 3024 + tk(t)]) p = D.wex[f[0] * 3024 + tk(t)];
    else {
      let d = D.date[dk(f)];
      if (f[1] && !f[0] && !f[2] && !f[3] && !f[4]) d = ["", "GGGG", "G", "GGGGG"][f[1]];
      const g = f[3] === 3 ? (f[0] ? G[0] : G[1]) : f[3] === 4 ? G[2] : G[3];
      p = apply(g, d, ti);
    }
  }
  return tz ? sub(p, TL[TZ.indexOf(tz)]) : p;
}
const nodeZ = (o, d) => m.fmt(o).formatToParts(d).map(p => p.type + ":" + (p.type === "timeZoneName" ? "Z" : p.value)).join("|");
const renderZ = (p, d) => m.render(p.replace(/(z+|O+|v+)(?=(?:[^']*'[^']*')*[^']*$)/g, "'\u0001'"), d).replace(/literal:([^|]*)\u0001([^|]*)/g, (s, a, b) => (a ? "literal:" + a + "|" : "") + "timeZoneName:Z" + (b ? "|literal:" + b : ""));
function opts(f, t) { const o = {...CYC[t[3]]}; if (f[0]) o.weekday = WIDTHS[f[0]]; if (f[1]) o.era = WIDTHS[f[1]]; if (f[2]) o.year = NUMS[f[2]]; if (f[3]) o.month = MONTHS[f[3]]; if (f[4]) o.day = NUMS[f[4]];
  if (t[0]) o.hour = NUMS[t[0]]; if (t[1]) o.minute = NUMS[t[1]]; if (t[2]) o.second = NUMS[t[2]]; if (t[4]) o.dayPeriod = WIDTHS[t[4]]; if (t[5]) o.fractionalSecondDigits = t[5]; return o; }
let seed = +(process.argv[3] || 99); const rnd = n => { seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5; return (seed >>> 0) % n; };
const buckets = {}; let total = 0, bad = 0;
const N = +(process.argv[4] || 3000);
for (let it = 0; it < N; it++) {
  let f = rnd(3) ? [rnd(4), rnd(4) === 0 ? rnd(4) : 0, rnd(3), rnd(6), rnd(3)] : [0, 0, 0, 0, 0];
  const t = rnd(3) ? [rnd(3), rnd(3), rnd(3), rnd(7), rnd(5) === 0 ? rnd(4) : 0, rnd(5) === 0 ? rnd(4) : 0] : [0, 0, 0, rnd(7), 0, 0];
  const hasT = t[0] || t[1] || t[2] || t[4] || t[5];
  const tz = rnd(3) === 0 ? TZ[rnd(6)] : null;
  if (!f.some(x => x) && !hasT) f = [0, 0, 1, 1, 1];
  const o = opts(f, t); if (tz) o.timeZoneName = tz;
  const p = compose(f, t, tz);
  total++;
  const ok = m.VDATES.slice(0, 10).every(d => m.render(p, d) === m.nodeParts(o, d));
  if (!ok) {
    bad++;
    const k = (f.some(x => x) ? "D" : "") + (hasT ? "T" : "") + (tz ? "z" : "") + (f[0] && !f[1] && !f[2] && !f[3] && !f[4] ? "-wdonly" : "") + (f[1] && !f[0] && !f[2] && !f[3] && !f[4] ? "-eraonly" : "");
    (buckets[k] = buckets[k] || []).push(JSON.stringify(o) + " ## " + p + " ## " + m.fmt(o).format(m.VDATES[0]));
  }
}
console.log(LOC, "total", total, "bad", bad, "glue", JSON.stringify(G));
for (const [k, v] of Object.entries(buckets)) { console.log(" ", k, v.length); for (const e of v.slice(0, 3)) console.log("     ", e); }
