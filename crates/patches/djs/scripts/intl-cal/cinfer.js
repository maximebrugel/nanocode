// cinfer.js TAG -> t-TAG.json: djs DateTimeFormat tables for a locale in a non-gregorian calendar
// (TAG such as en-US-u-ca-buddhist, or th whose default is buddhist), inferred from node's ICU like
// intl-dtf/infer.js, but with the calendar's fields from cal.js (the model calendar.rs mirrors).
// Pattern extensions: \u0001 before a numeric field = the locale's override numbering (hebr, hanidec,
// hanidays); r = related gregorian year; U = cyclic year name.
const {CAL, fromCivil} = require('./cal.js');
const NUMSYS = require('./numsys.js');
const LOC = process.argv[2];
const OUT = process.env.OUT || __dirname;
const WIDTHS = ["", "long", "short", "narrow"], NUMS = ["", "numeric", "2-digit"], MONTHS = ["", "numeric", "2-digit", "long", "short", "narrow"];
const CYC = [{}, {hour12: true}, {hour12: false}, {hourCycle: "h12"}, {hourCycle: "h23"}, {hourCycle: "h11"}, {hourCycle: "h24"}];
const U = (y, mo, d, h = 0, mi = 0, s = 0, ms = 0) => { const t = new Date(Date.UTC(2000, mo, d, h, mi, s, ms)); t.setUTCFullYear(y); return t; };
const DTZ = process.env.DTZ || "UTC";
const fmt = (o) => new Intl.DateTimeFormat(LOC, {timeZone: DTZ, ...o});
const RO = fmt({}).resolvedOptions();
const CALN = RO.calendar;
const model = CAL[CALN];
if (!model) throw new Error("no model for " + CALN);
const LANG = LOC.split("-")[0];
const LUNI = CALN === "chinese" || CALN === "dangi";
const dayOf = d => Math.floor(d.getTime() / 864e5);
const F = d => model(dayOf(d));
const part = (o, d, ty) => { const p = fmt(o).formatToParts(d).find(p => p.type === ty); return p ? p.value : null; };
const W3 = ["long", "short", "narrow"];
const ZERO = (() => { const z = new Intl.NumberFormat(LOC, {useGrouping: false, numberingSystem: RO.numberingSystem}).format(0); return z === "0" ? "" : z; })();
const dig = s => ZERO ? s.replace(/[0-9]/g, c => String.fromCodePoint(ZERO.codePointAt(0) + +c)) : s;
const undig = s => ZERO ? s.replace(/./gu, c => { const d = c.codePointAt(0) - ZERO.codePointAt(0); return d >= 0 && d <= 9 ? String(d) : c; }) : s;
const NUMT = new Set(["year", "month", "day", "hour", "minute", "second", "fractionalSecond", "relatedYear"]);
// sample dates by month code, era and cyclic year (noon UTC), 1900-2100
const LO = fromCivil(1900, 1, 1), HI = fromCivil(2100, 12, 31);
const byMonth = new Map(), byEra = new Map(), byCyc = new Map(), byYear1 = new Map();
for (let k = LO; k <= HI; k++) {
  const f = model(k);
  if (f.day === 5 && !byMonth.has(f.month)) byMonth.set(f.month, k);
  if (!byEra.has(f.era) && f.day >= 5) byEra.set(f.era, k);
  if (f.cyc && !byCyc.has(f.cyc) && f.day >= 5) byCyc.set(f.cyc, k);
  if (f.year === 1 && !byYear1.has(f.era) && f.day >= 5) byYear1.set(f.era, k);
}
const kd = k => new Date(k * 864e5 + 12 * 36e5);
const NM = Math.max(...byMonth.keys()) + 1;
const names = {cal: CALN, months: {}, days: {}, nmonths: NM};
for (const w of W3) {
  names.months["L" + w] = Array.from({length: NM}, (_, m) => byMonth.has(m) ? part({month: w}, kd(byMonth.get(m)), "month") : null);
  // format names from the yMMMd context: he uses its true abbreviations only there (MMMd shows MMMM)
  names.months["M" + w] = Array.from({length: NM}, (_, m) => byMonth.has(m) ? part({year: "numeric", month: w, day: "numeric"}, kd(byMonth.get(m)), "month") : null);
  names.days["c" + w] = Array.from({length: 7}, (_, d) => part({weekday: w}, U(2024, 2, 3 + d), "weekday"));
  names.days["E" + w] = Array.from({length: 7}, (_, d) => part({weekday: w, month: "long", day: "numeric"}, U(2024, 2, 3 + d), "weekday"));
}
const NE = LUNI ? 0 : Math.max(...byEra.keys()) + 1;
names.eras = W3.map(w => Array.from({length: NE}, (_, e) => byEra.has(e) ? part({era: w, year: "numeric", month: "numeric", day: "numeric"}, kd(byEra.get(e)), "era") : null));
names.ampm = [part({hour: "numeric", hourCycle: "h12"}, U(2024, 0, 1, 4), "dayPeriod"), part({hour: "numeric", hourCycle: "h12"}, U(2024, 0, 1, 16), "dayPeriod")];
names.periods = W3.map(w => {
  const a = Array.from({length: 24}, (_, h) => part({hour: "numeric", minute: "numeric", second: "numeric", dayPeriod: w, hourCycle: "h12"}, U(2024, 0, 1, h, 30, 30), "dayPeriod"));
  a.push(part({hour: "numeric", minute: "numeric", second: "numeric", dayPeriod: w, hourCycle: "h12"}, U(2024, 0, 1, 0, 0), "dayPeriod"));
  a.push(part({hour: "numeric", minute: "numeric", second: "numeric", dayPeriod: w, hourCycle: "h12"}, U(2024, 0, 1, 12, 0), "dayPeriod"));
  return a;
});
if (byCyc.size) names.yearNames = Array.from({length: 60}, (_, c) => byCyc.has(c + 1) ? part({year: "numeric"}, kd(byCyc.get(c + 1)), "yearName") : null);
// chinese leap months: numeric month pattern ("{0}bis", "闰{0}") from a leap-month date
names.leapNum = null;
for (const [m, k] of byMonth) if (m >= 12 && (CALN === "chinese" || CALN === "dangi")) { const v = undig(part({month: "numeric", day: "numeric"}, kd(k), "month")); names.leapNum = v.replace(String(m - 11), "{0}"); break; }
// the month number a numeric M shows (hebrew counts Adar I only in leap years)
function mnum(f) {
  if (CALN === "hebrew") { const m = f.month; return f.leap ? (m === 13 ? 7 : m + 1) : (m <= 5 ? m + 1 : m); }
  if (CALN === "chinese" || CALN === "dangi") return f.month % 12 + 1;
  return f.month + 1;
}
const isLeapMonth = f => (CALN === "chinese" || CALN === "dangi") && f.month >= 12;
// ---- formatter (mirror of the Rust one) ----
function tokens(p) {
  const out = []; let i = 0, ov = null;
  while (i < p.length) {
    const c = p[i];
    if (c === "'") { let j = i + 1, lit = ""; while (j < p.length) { if (p[j] === "'") { if (p[j + 1] === "'") { lit += "'"; j += 2; continue; } break; } lit += p[j++]; } if (!lit && j === i + 1) lit = "'"; out.push(["lit", lit]); i = j + 1; continue; }
    if (c in NUMSYS.MARK) { ov = NUMSYS.MARK[c]; i++; continue; }
    if (!/[A-Za-z]/.test(c)) { out.push(["lit", c]); i++; continue; }
    let n = 1; while (p[i + n] === c) n++; out.push([c, n, ov]); ov = null; i += n;
  }
  return out;
}
const WI = n => n === 4 ? "long" : n === 5 ? "narrow" : "short";
function render(p, d) {
  const h = d.getUTCHours(), mi = d.getUTCMinutes(), s = d.getUTCSeconds(), ms = d.getUTCMilliseconds(), wd = d.getUTCDay();
  const f = F(d);
  const num = (v, n) => n >= 2 ? String(v % 100).padStart(2, "0") : String(v);
  let r = [];
  const tk = tokens(p), hasM = tk.some(t => t[0] === "m"), hasS = tk.some(t => t[0] === "s");
  const exact = (!hasM || mi === 0) && (!hasS || s === 0);
  for (const [c, n, ov] of tk) {
    if (c === "lit") { r.push(["literal", n]); continue; }
    let ty, v;
    switch (c) {
      case "E": case "c": ty = "weekday"; v = names.days[c + WI(n)][wd]; break;
      case "G": ty = "era"; v = LUNI ? String(f.era) : names.eras[W3.indexOf(WI(n))][f.era]; break;
      case "y": ty = "year"; v = ov ? NUMSYS.fmt(ov, f.year, "y") : n === 2 ? String(f.year % 100).padStart(2, "0") : String(f.year); break;
      case "r": ty = "relatedYear"; v = String(f.ry); break;
      case "U": ty = "yearName"; v = names.yearNames[f.cyc - 1]; break;
      case "M": case "L": ty = "month"; if (n <= 2) { v = num(mnum(f), n); if (isLeapMonth(f)) v = names.leapNum.replace("{0}", v); } else v = names.months[c + WI(n)][f.month]; break;
      case "d": ty = "day"; v = ov ? NUMSYS.fmt(ov, f.day, "d") : num(f.day, n); break;
      case "h": ty = "hour"; v = num(h % 12 || 12, n); break;
      case "K": ty = "hour"; v = num(h % 12, n); break;
      case "H": ty = "hour"; v = num(h, n); break;
      case "k": ty = "hour"; v = num(h || 24, n); break;
      case "m": ty = "minute"; v = num(mi, n); break;
      case "s": ty = "second"; v = num(s, n); break;
      case "S": ty = "fractionalSecond"; v = String(ms).padStart(3, "0").slice(0, n); break;
      case "a": ty = "dayPeriod"; v = names.ampm[h < 12 ? 0 : 1]; break;
      case "B": { ty = "dayPeriod"; const t = names.periods[W3.indexOf(WI(n))]; v = (exact && h === 0) ? t[24] : (exact && h === 12) ? t[25] : t[h]; break; }
      case "z": case "O": case "v": ty = "timeZoneName"; v = "Z"; break;
      default: ty = "?"; v = c.repeat(n);
    }
    r.push([ty, v]);
  }
  const m = [];
  for (const x of r) { if (x[0] === "literal" && m.length && m[m.length - 1][0] === "literal") m[m.length - 1][1] += x[1]; else m.push([x[0], x[1]]); }
  return m.map(x => x[0] + ":" + (NUMT.has(x[0]) ? dig(x[1]) : x[1])).join("|");
}
const nodeParts = (o, d) => fmt(o).formatToParts(d).map(p => p.type + ":" + (p.type === "timeZoneName" ? "Z" : p.value)).join("|");
// reference date: single-digit day and month, distinct, in 2024 (so numeric widths and names are unambiguous)
const REF = (() => { for (let k = fromCivil(2024, 1, 1); ; k++) { const f = model(k); const mn = mnum(f); if (f.day >= 2 && f.day <= 9 && mn >= 2 && mn <= 9 && mn !== f.day && !f.leap && f.month < 12) { const d = kd(k); d.setUTCHours(4, 7, 9, 12); return d; } } })();
const SPECIAL = [...byEra.values(), ...byYear1.values(), ...[...byMonth.values()]].map(kd);
const VDATES = [REF, ...Array.from({length: 12}, (_, m) => U(2024, m, 5 + (m % 7), (m * 5) % 24, m * 4)), ...Array.from({length: 7}, (_, d) => U(2024, 7, 4 + d, 12, 0)), U(2024, 0, 1, 0, 0), U(2024, 0, 1, 12, 0, 1), U(2024, 0, 1, 12, 30, 0), U(2024, 0, 1, 0, 0, 7), U(2024, 0, 1, 23, 59),
  ...SPECIAL, ...Array.from({length: 21}, (_, i) => U(1900 + i * 10, (i * 5) % 12, 1 + (i * 7) % 28, i % 24, i))];
const q = lit => /[A-Za-z'\u0001-\u0004]/.test(lit) ? "'" + lit.replace(/'/g, "''") + "'" : lit;
const REFF = F(REF);
function infer(o) {
  const f = fmt(o), hc = f.resolvedOptions().hourCycle;
  const parts = f.formatToParts(REF);
  const HL = {h12: "h", h11: "K", h23: "H", h24: "k"}[hc];
  let alts = [""];
  const addAll = arr => { const n = []; for (const a of alts) for (const b of arr) n.push(a + b); alts = n; };
  const numAlt = (value, L) => { const c = /^\d+$/.test(value) ? [value.length === 2 ? L + L : L] : []; for (const k of (NUMSYS.FOR[CALN] || {})[L] || []) c.push(k + L); if (L === "y" && !c.includes("y")) c.push("y"); return c; };
  for (let {type, value} of parts) {
    if (NUMT.has(type)) value = undig(value);
    switch (type) {
      case "literal": addAll([q(value)]); break;
      case "year": addAll(numAlt(value, "y")); break;
      case "relatedYear": addAll(["r"]); break;
      case "yearName": addAll(["U"]); break;
      case "day": addAll(numAlt(value, "d")); break;
      case "hour": addAll([HL.repeat(value.length === 2 ? 2 : 1)]); break;
      case "minute": addAll([value.length === 2 ? "mm" : "m"]); break;
      case "second": addAll([value.length === 2 ? "ss" : "s"]); break;
      case "fractionalSecond": addAll(["S".repeat(value.length)]); break;
      case "month": { const c = []; if (/^\d+$/.test(value) || /\d/.test(value) && names.leapNum) c.push(/\d\d/.test(value) ? "MM" : "M");
        for (const w of W3) for (const L of ["M", "L"]) if (names.months[L + w][REFF.month] === value) c.push(L.repeat(w === "long" ? 4 : w === "short" ? 3 : 5)); addAll(c.length ? c : ["MMM"]); break; }
      case "weekday": { const c = []; for (const w of W3) for (const L of ["E", "c"]) if (names.days[L + w][REF.getUTCDay()] === value) c.push(L.repeat(w === "long" ? 4 : w === "short" ? 3 : 5)); addAll(c.length ? c : ["EEE"]); break; }
      case "era": { const c = []; if (LUNI) c.push("G"); else W3.forEach((w, i) => { if (names.eras[i][REFF.era] === value) c.push("G".repeat(w === "long" ? 4 : w === "short" ? 1 : 5)); }); addAll(c.length ? c : ["G"]); break; }
      case "dayPeriod": { const c = []; if (value === names.ampm[0]) c.push("a"); W3.forEach((w, i) => { if (names.periods[i][4] === value) c.push("B".repeat(w === "long" ? 4 : w === "short" ? 1 : 5)); }); addAll(c.length ? c : ["a"]); break; }
      case "timeZoneName": addAll(["z"]); break;
      default: addAll(["?" + type]);
    }
    if (alts.length > 256) alts = alts.slice(0, 256);
  }
  for (const p of alts) {
    let ok = true;
    for (const d of VDATES) if (render(p, d) !== nodeParts(o, d)) { ok = false; break; }
    if (ok) return {p, hc};
  }
  return {p: alts[0], hc, bad: true};
}
module.exports = {infer, names, render, nodeParts, fmt, VDATES, WIDTHS, NUMS, MONTHS, CYC, U, LOC, CALN, OUT, F};
if (require.main === module) {
  const out = {tag: LOC, names, date: [], datez: [], styles: [], bad: []};
  for (let k = 0; k < 864; k++) {
    let r = k; const f4 = r % 3; r = (r - f4) / 3; const f3 = r % 6; r = (r - f3) / 6; const f2 = r % 3; r = (r - f2) / 3; const f1 = r % 4; const f0 = (r - f1) / 4;
    if (!(f0 || f1 || f2 || f3 || f4)) { out.date.push(null); out.datez.push(null); continue; }
    const o = {}; if (f0) o.weekday = WIDTHS[f0]; if (f1) o.era = WIDTHS[f1]; if (f2) o.year = NUMS[f2]; if (f3) o.month = MONTHS[f3]; if (f4) o.day = NUMS[f4];
    const r2 = infer(o); out.date.push(r2.p); if (r2.bad) out.bad.push([o, r2.p]);
    const oz = {...o, timeZoneName: "short"}, r3 = infer(oz); out.datez.push(r3.p); if (r3.bad) out.bad.push([oz, r3.p]);
  }
  for (const s of ["full", "long", "medium", "short"]) { const r2 = infer({dateStyle: s}); out.styles.push(r2.p); if (r2.bad) out.bad.push([{dateStyle: s}, r2.p]); }
  // date-time glue, as "{1}…{0}", for the glue classes date.rs picks by month width and weekday, and per
  // dateStyle: the literal between the date pattern's tokens and the time pattern's in the whole pattern
  const toks = p => { const a = []; for (const t of tokens(p)) { const v = t[0] === "lit" ? ["lit", t[1]] : [t[0] + t[1], (t[2] || "")]; if (v[0] === "lit" && a.length && a[a.length - 1][0] === "lit") a[a.length - 1][1] += v[1]; else a.push(v); } return a; };
  const glueOf = (o, dp, tp) => {
    const r2 = infer(o); if (r2.bad) return null;
    const P = toks(r2.p), D = toks(dp), T = toks(tp);
    const eq = (a, b) => a[0] === b[0] && a[1] === b[1];
    let k = 0, lit = "";
    for (let x = 0; x < D.length; x++, k++) {
      if (x === D.length - 1 && D[x][0] === "lit") { if (P[k][0] !== "lit" || !P[k][1].startsWith(D[x][1])) return null; lit = P[k][1].slice(D[x][1].length); k++; break; }
      if (!P[k] || !eq(P[k], D[x])) return null;
    }
    if (k === D.length && P[k] && P[k][0] === "lit" && !(D.length && D[D.length - 1][0] === "lit")) { lit = P[k][1]; k++; }
    let rest = P.slice(k), suf = "";
    // a glue may also follow the time ("{1} को {0} बजे")
    if (rest.length === T.length + 1 && rest[rest.length - 1][0] === "lit" && (!T.length || T[T.length - 1][0] !== "lit")) suf = rest.pop()[1];
    else if (T.length && T[T.length - 1][0] === "lit" && rest.length === T.length && rest[rest.length - 1][0] === "lit" && rest[rest.length - 1][1].startsWith(T[T.length - 1][1]) && rest[rest.length - 1][1] !== T[T.length - 1][1]) { suf = rest[rest.length - 1][1].slice(T[T.length - 1][1].length); rest[rest.length - 1] = T[T.length - 1]; }
    if (T.length && T[0][0] === "lit" && rest.length && rest[0][0] !== "lit") return null;
    if (T.length && T[0][0] === "lit") { const r0 = lit + ""; void r0; }
    if (rest.length !== T.length || !rest.every((t, x) => eq(t, T[x]) || (x === 0 && t[0] === "lit" && T[x][0] === "lit" && t[1].endsWith(T[x][1])))) return null;
    if (T[0] && T[0][0] === "lit" && rest[0][1] !== T[0][1]) lit += rest[0][1].slice(0, rest[0][1].length - T[0][1].length);
    return "{1}" + q(lit) + "{0}" + q(suf);
  };
  const T = {hour: "numeric", minute: "2-digit"}, tp = infer(T).p;
  const GL = [{weekday: "long", year: "numeric", month: "long", day: "numeric"}, {year: "numeric", month: "long", day: "numeric"}, {year: "numeric", month: "short", day: "numeric"}, {year: "numeric", month: "numeric", day: "numeric"}];
  out.glue = GL.map(o => { const g = glueOf({...o, ...T}, infer(o).p, tp); if (g === null) out.bad.push([{...o, ...T}, "glue"]); return g; });
  const t2 = infer({timeStyle: "short"}).p;
  out.style_glue = ["full", "long", "medium", "short"].map((s, i) => { const g = glueOf({dateStyle: s, timeStyle: "short"}, out.styles[i], t2); if (g === null) out.bad.push([{dateStyle: s, timeStyle: "short"}, "glue"]); return g; });
  // times: the gregorian locale's, unless this calendar's differ (zh-TW roc, zh chinese, ko dangi, iso8601)
  const BASE = LOC.split("-u-")[0], probe = [U(2024, 2, 5, 4, 7, 9, 12), U(2024, 2, 5, 0, 0, 0), U(2024, 2, 5, 12, 0, 0), U(2024, 2, 5, 16, 30, 5)];
  const timeOpts = k => { let r = k; const fsd = r % 4; r = (r - fsd) / 4; const dp = r % 4; r = (r - dp) / 4; const cy = r % 7; r = (r - cy) / 7; const se = r % 3; r = (r - se) / 3; const mi = r % 3; const ho = (r - mi) / 3;
    if (!(ho || mi || se || dp || fsd)) return null; const o = {...CYC[cy]}; if (ho) o.hour = NUMS[ho]; if (mi) o.minute = NUMS[mi]; if (se) o.second = NUMS[se]; if (dp) o.dayPeriod = WIDTHS[dp]; if (fsd) o.fractionalSecondDigits = fsd; return o; };
  const same = o => probe.every(d => fmt(o).format(d) === new Intl.DateTimeFormat(BASE, {timeZone: DTZ, ...o}).format(d));
  let differs = false;
  for (let k = 0; k < 3024 && !differs; k++) { const o = timeOpts(k); if (o && (!same(o) || !same({...o, timeZoneName: "short"}))) differs = true; }
  for (const st of ["full", "long", "medium", "short"]) for (const c of CYC) if (!same({...c, timeStyle: st})) differs = true;
  if (differs) {
    out.time = []; out.timez = []; out.style_time = [];
    for (let k = 0; k < 3024; k++) {
      const o = timeOpts(k); if (!o) { out.time.push(null); out.timez.push(null); continue; }
      const a = infer(o); out.time.push(a.p); if (a.bad) out.bad.push([o, a.p]);
      const oz = {...o, timeZoneName: "short"}, b = infer(oz); out.timez.push(b.p); if (b.bad) out.bad.push([oz, b.p]);
    }
    for (const c of CYC) out.style_time.push(["full", "long", "medium", "short"].map(st => { const a = infer({...c, timeStyle: st}); if (a.bad) out.bad.push([{...c, timeStyle: st}, a.p]); return a.p; }));
  }
  require('fs').writeFileSync(OUT + '/c-' + LOC + '.json', JSON.stringify(out));
  console.log(LOC, CALN, "bad", out.bad.length, JSON.stringify(out.bad.slice(0, 4)));
}
