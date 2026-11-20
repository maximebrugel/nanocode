// rangemodel.js : reference model of Intl.DateTimeFormat formatRange (ICU DateIntervalFormat as V8
// drives it) from the generated tables t-LOC (format patterns), r-LOC (interval patterns) and tp-LOC
// (ICU's time patterns). rdiff.js checks it against node; intl/date/range.rs ports it.
// An entry is "=" (V8 formats the start alone), or an interval pattern [first, second] whose
// sources follow range.js predict().
const fs = require('fs');
const {toks} = require('./range.js');
const {apply} = require('./glue.js');
const LV = {G: 0, y: 10, u: 10, U: 10, M: 20, L: 20, d: 30, E: 30, c: 30, a: 40, h: 50, H: 50, k: 50, K: 50, m: 60, s: 70, S: 80};
const LEVEL_NUM = [0, 10, 20, 30, 40, 50, 60, 70, 80]; // G y M d a h m s S
const q = lit => /[A-Za-z']/.test(lit) ? "'" + lit.replace(/'/g, "''") + "'" : lit;
const dkey = f => (((f[0] * 4 + f[1]) * 3 + f[2]) * 6 + f[3]) * 3 + f[4];
const tkey = (f, cy, fsd) => ((((f[6] * 3 + f[7]) * 3 + f[8]) * 7 + cy) * 4 + f[5]) * 4 + fsd;
const tkey252 = (f, cy) => ((f[6] * 3 + f[7]) * 7 + cy) * 4 + f[5];
// ICU's isFieldUnitIgnored: no field in P at the level's unit or smaller
const ignored = (level, P) => !toks(P).some(t => LV[t[0]] !== undefined && LV[t[0]] >= LEVEL_NUM[level]);
const isFallback = (e, sep) => Array.isArray(e) && e[0] === e[1] + q(sep);
const fb = (P, sep) => [P + q(sep), P];
// joins a date pattern to a time interval pair with a glue ("' at '" or a {1}=date {0}=time template)
function glueDate(g, D, pair) {
  if (!g.includes("{")) return [D + g + pair[0], pair[1]];
  const i0 = g.indexOf("{0}"), i1 = g.indexOf("{1}");
  if (i1 < i0) return [g.slice(0, i0).replace("{1}", D) + pair[0], pair[1] + g.slice(i0 + 3)];
  return [g.slice(0, i0) + pair[0], pair[1] + g.slice(i0 + 3).replace("{1}", D)];
}
function load(LOC) {
  const rd = p => JSON.parse(fs.readFileSync(__dirname + '/' + p + '-' + LOC + '.json'));
  return {T: rd('t'), R: rd('r'), TP: rd('tp')};
}
// s: {f[10] (weekday era year month day dayPeriod hour minute second), fsd, cy (0..6), z (0 none, 1..6 kinds),
//     ds, ts (style index or -1), glues [full, long, medium, short], single (format's pattern)}
function entry(d, s, level) {
  const {T, R, TP} = d;
  const sep = R.sep, f = s.f;
  if (s.ds >= 0 || s.ts >= 0) {
    if (level === 8) return "=";
    if (s.ts < 0) return level < 4 ? R.sdate[s.ds][level] : "=";
    if (s.ds < 0) return R.stime[s.cy][s.ts][level];
    return R.sboth[s.ds][s.cy][s.ts][level];
  }
  const hasDate = f[0] || f[1] || f[2] || f[3] || f[4];
  const hasTime = f[5] || f[6] || f[7] || f[8] || s.fsd;
  const dk = dkey(f);
  if (!hasTime && !s.z) return level < 4 ? R.date[dk][level] : "=";
  if (!hasTime) return ignored(level, s.single) ? "=" : fb(s.single, sep);
  const Tpat = TP.tpat[s.z][tkey(f, s.cy, s.fsd)];
  const tab = (s.fsd || f[8]) ? null : (s.z ? R.timez[s.z - 1] : R.time)[tkey252(f, s.cy)];
  // ICU has interval patterns for every hour skeleton (in some locales they read like a fallback)
  const found = !!tab && (f[6] > 0 || [2, 3, 4].some(i => Array.isArray(tab[i]) && !isFallback(tab[i], sep)));
  const glueBy = fd => fd[3] === 3 && fd[0] ? s.glues[0] : fd[3] === 3 ? s.glues[1] : fd[3] === 4 ? s.glues[2] : s.glues[3];
  const dpat = k => (TP.dpat && TP.dpat[k]) || T.date[k];
  const best = fd => apply(glueBy(fd), dpat(dkey(fd)), Tpat);
  if (!hasDate) {
    if (level < 4) return fb(apply(s.glues[3], T.date[dkey(level === 0 ? [0, 2, 1, 1, 1] : [0, 0, 1, 1, 1])], (TP.tpatf && TP.tpatf[s.z][tkey(f, s.cy, s.fsd)]) || Tpat), sep);
    if (found && level <= 6) return tab[level - 2];
    return ignored(level, Tpat) ? "=" : fb(Tpat, sep);
  }
  // fallbacks format both dates with ICU's own pattern for the skeleton (date pattern, glue by month
  // width, ICU's time pattern for the whole skeleton)
  const Tf = (TP.tpatf && TP.tpatf[s.z][tkey(f, s.cy, s.fsd)]) || Tpat;
  const bestF = fd => apply(glueBy(fd), T.date[dkey(fd)], Tf);
  if (level < 4) {
    // ICU sets an interval pattern for a date field the skeleton lacks: the skeleton with that
    // field and every finer one it lacks added. A field it has falls back to the whole pattern.
    const at = [1, 2, 3, 4][level];
    let P = bestF(f.slice(0, 5));
    if (found && !f[at]) {
      const g = f.slice(0, 5);
      for (let lv = 3; lv >= level; lv--) { const i = [1, 2, 3, 4][lv]; if (!g[i]) g[i] = i === 1 ? 2 : 1; }
      P = bestF(g);
    }
    return ignored(level, P) ? "=" : fb(P, sep);
  }
  if (found && level <= 6) {
    const e = tab[level - 2];
    return e === "=" ? "=" : glueDate(TP.gm, TP.dpat[dk], e);
  }
  if (ignored(level, s.single)) return "=";
  return glueDate(TP.gm, (TP.dpatn && TP.dpatn[dk]) || TP.dpat[dk], fb(Tpat, sep));
}
module.exports = {entry, load, dkey, tkey, ignored, glueDate, fb, q};

