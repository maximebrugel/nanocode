// range.js LOC -> r-LOC.json : Intl.DateTimeFormat formatRange interval patterns, inferred from
// node's ICU (run with the parity node, after infer.js LOC > t-LOC.json).
// An entry is "=" (the range formats as the start date alone, every part shared) or [first, second],
// the ICU interval pattern cut where the end date starts. Fields present in both halves make each
// half's startRange/endRange span (its first to last such field); all else is shared. Each pattern
// is checked against node on three date pairs that differ first in its field.
// Time keys hold G, d, a, h, m (y and M range like d). The fallback separator is out.sep.
// Levels (greatest differing field): G era, y year, M month, d day, a am/pm, h hour, m minute, s second.
const m = require('./infer.js');
const fs = require('fs');
const {WIDTHS, NUMS, MONTHS, CYC, U, LOC, render} = m;
const TZ = "America/New_York";
const fmt = (o) => new Intl.DateTimeFormat(LOC, {timeZone: TZ, ...o});
const LEVELS = ["G", "y", "M", "d", "a", "h", "m", "s"];
const KINDS = ["short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"];
const ZL = ["z", "zzzz", "O", "OOOO", "v", "vvvv"];
// pairs are wall-clock times in TZ; some run backwards so each half sees one-digit and two-digit hours
const W = (y, mo, d, h = 0, mi = 0, s = 0) => { const t = U(y, mo, d, h, mi, s); const off = t.getTime() - local(t).getTime(); return new Date(t.getTime() + off + (local(new Date(t.getTime() + off)).getTime() === t.getTime() ? 0 : t.getTime() - local(new Date(t.getTime() + off)).getTime())); };
const LP = new Intl.DateTimeFormat("en-US", {timeZone: TZ, era: "short", year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "numeric", second: "numeric", hourCycle: "h23"});
function local(d) {
  const p = Object.fromEntries(LP.formatToParts(d).map(x => [x.type, x.value]));
  const y = p.era === "BC" ? 1 - +p.year : +p.year;
  const r = U(y, +p.month - 1, +p.day, +p.hour, +p.minute, +p.second);
  r.setUTCMilliseconds(d.getUTCMilliseconds());
  return r;
}
const PAIRS = {
  G: [[W(-5, 5, 5, 9, 5), W(2024, 7, 8, 15, 30)], [W(-99, 0, 3, 1, 2), W(1999, 11, 28, 23, 59)], [W(-9, 1, 3, 1, 2), W(7, 2, 4, 3, 4)]],
  y: [[W(2024, 5, 5, 9, 5), W(2025, 7, 8, 15, 30)], [W(2019, 0, 3, 1, 2), W(2023, 11, 28, 23, 59)], [W(2019, 0, 3, 1, 2), W(2023, 1, 4, 3, 4)]],
  M: [[W(2024, 5, 5, 9, 5), W(2024, 7, 8, 15, 30)], [W(2023, 0, 3, 1, 2), W(2023, 10, 28, 23, 59)], [W(2023, 0, 3, 1, 2), W(2023, 1, 4, 3, 4)]],
  d: [[W(2024, 5, 5, 9, 5), W(2024, 5, 8, 15, 30)], [W(2023, 1, 3, 1, 2), W(2023, 1, 28, 23, 59)], [W(2023, 1, 3, 1, 2), W(2023, 1, 5, 3, 4)]],
  a: [[W(2024, 5, 5, 9, 5), W(2024, 5, 5, 15, 30)], [W(2023, 1, 3, 1, 2), W(2023, 1, 3, 23, 59)], [W(2023, 1, 3, 8, 2), W(2023, 1, 3, 13, 4)], [W(2023, 1, 3, 13, 4), W(2023, 1, 3, 5, 6)]],
  h: [[W(2024, 5, 5, 13, 5), W(2024, 5, 5, 15, 30)], [W(2023, 1, 3, 1, 2), W(2023, 1, 3, 10, 59)], [W(2023, 1, 3, 1, 2), W(2023, 1, 3, 3, 4)], [W(2023, 1, 3, 10, 2), W(2023, 1, 3, 3, 4)]],
  m: [[W(2024, 5, 5, 15, 5), W(2024, 5, 5, 15, 30)], [W(2023, 1, 3, 1, 2), W(2023, 1, 3, 1, 59)], [W(2023, 1, 3, 1, 2), W(2023, 1, 3, 1, 4)]],
  s: [[W(2024, 5, 5, 15, 5, 1), W(2024, 5, 5, 15, 5, 30)], [W(2023, 1, 3, 1, 2, 3), W(2023, 1, 3, 1, 2, 59)], [W(2023, 1, 3, 1, 2, 3), W(2023, 1, 3, 1, 2, 4)]],
};
const SRC = {startRange: 0, endRange: 1, shared: 2};
const zcache = new Map();
// the zone kind to try first (rangez.js sets it: far-past offsets read the same in several kinds)
const ZPREF = {kind: -1};
function zoneLetters(d, v) {
  for (const k of ZPREF.kind >= 0 ? [ZPREF.kind, 0, 1, 2, 3, 4, 5] : [0, 1, 2, 3, 4, 5]) {
    const key = d.getTime() + ":" + k;
    if (!zcache.has(key)) zcache.set(key, fmt({hour: "numeric", timeZoneName: KINDS[k]}).formatToParts(d).find(p => p.type === "timeZoneName").value);
    if (zcache.get(key) === v) return "Z:" + ZL[k];
  }
  return "Z:?" + v;
}
const nodeRange = (o, a, b) => fmt(o).formatRangeToParts(a, b).map(p => [p.type, p.type === "timeZoneName" ? zoneLetters(p.source === "endRange" ? b : a, p.value) : p.value, SRC[p.source]]);
const q = lit => /[A-Za-z']/.test(lit) ? "'" + lit.replace(/'/g, "''") + "'" : lit;
function toks(p) {
  const out = []; let i = 0;
  while (i < p.length) {
    const c = p[i];
    if (c === "'") { let j = i + 1; while (j < p.length && !(p[j] === "'" && p[j + 1] !== "'")) j += p[j] === "'" ? 2 : 1; out.push(p.slice(i, j + 1)); i = j + 1; continue; }
    if (!/[A-Za-z]/.test(c)) { let j = i; while (j < p.length && !/[A-Za-z']/.test(p[j])) j++; out.push(p.slice(i, j)); i = j; continue; }
    let n = 1; while (p[i + n] === c) n++; out.push(c.repeat(n)); i += n;
  }
  return out;
}
const TYPE = {E: "weekday", c: "weekday", G: "era", y: "year", M: "month", L: "month", d: "day", a: "dayPeriod", B: "dayPeriod", h: "hour", H: "hour", K: "hour", k: "hour", m: "minute", s: "second", S: "fractionalSecond", z: "timeZoneName", O: "timeZoneName", v: "timeZoneName"};
const one = (tok, d) => {
  if (/^[zOv]/.test(tok)) return ["timeZoneName", "Z:" + tok];
  const s = render(tok, local(d)); const k = s.indexOf(":"); return [s.slice(0, k), s.slice(k + 1)];
};
function predict(pat, a, b) {
  const halves = pat.map(toks);
  const types = halves.map(h => new Set(h.filter(t => /^[A-Za-z]/.test(t)).map(t => TYPE[t[0]])));
  const rep = new Set([...types[0]].filter(t => types[1].has(t)));
  const out = [];
  halves.forEach((h, hi) => {
    const isRep = h.map(t => /^[A-Za-z]/.test(t) && rep.has(TYPE[t[0]]));
    const first = isRep.indexOf(true), last = isRep.lastIndexOf(true);
    h.forEach((t, i) => {
      const src = first >= 0 && i >= first && i <= last ? hi : 2;
      const [ty, v] = one(t, hi ? b : a);
      if (v === "") return;
      const prev = out[out.length - 1];
      if (ty === "literal" && prev && prev[0] === "literal" && prev[2] === src) prev[1] += v; else out.push([ty, v, src]);
    });
  });
  return out;
}
const same = (x, y) => JSON.stringify(x) === JSON.stringify(y);
function letters(ty, obs, single, hc) {
  const runs = toks(single || "").filter(t => /^[A-Za-z]/.test(t) && TYPE[t[0]] === ty);
  const HL = {h12: "h", h11: "K", h23: "H", h24: "k"}[hc] || "h";
  const cands = [...runs];
  const add = (...xs) => cands.push(...xs);
  switch (ty) {
    case "year": add("y", "yy"); break;
    case "month": add("M", "MM", "MMM", "MMMM", "MMMMM", "LLL", "LLLL", "LLLLL"); break;
    case "day": add("d", "dd"); break;
    case "hour": add(HL, HL + HL); break;
    case "minute": add("mm", "m"); break;
    case "second": add("ss", "s"); break;
    case "fractionalSecond": add("S".repeat(obs[0][1].length)); break;
    case "weekday": add("EEE", "EEEE", "EEEEE", "ccc", "cccc", "ccccc"); break;
    case "era": add("G", "GGGG", "GGGGG"); break;
    case "dayPeriod": add("a", "B", "BBBB", "BBBBB"); break;
    case "timeZoneName": add(...ZL); break;
  }
  return cands.find(c => obs.every(([d, v]) => same(one(c, d), [ty, v]))) || "?" + ty;
}
function infer(o, lv, single) {
  const [a, b] = PAIRS[lv][0];
  const parts = nodeRange(o, a, b);
  if (parts.every(p => p[2] === 2)) {
    return PAIRS[lv].every(([x, y]) => nodeRange(o, x, y).every(p => p[2] === 2)) ? "=" : {bad: "single?"};
  }
  const hc = fmt(o).resolvedOptions().hourCycle;
  const split = parts.findIndex(p => p[2] === 1);
  const pat = ["", ""];
  const all = PAIRS[lv].map(([x, y]) => [x, y, nodeRange(o, x, y)]).filter(([, , ps]) => ps.length === parts.length && ps.every((p, i) => p[0] === parts[i][0]));
  parts.forEach((p, i) => {
    const h = i < split ? 0 : 1;
    pat[h] += p[0] === "literal" ? q(p[1]) : letters(p[0], all.map(([x, y, ps]) => [h ? y : x, ps[i][1]]), single, hc);
  });
  for (const [x, y] of PAIRS[lv]) {
    const want = nodeRange(o, x, y), got = predict(pat, x, y);
    if (!same(want, got)) return {bad: pat, want: JSON.stringify(want), got: JSON.stringify(got)};
  }
  return pat;
}
function dateOpts(k) {
  let r = k; const f4 = r % 3; r = (r - f4) / 3; const f3 = r % 6; r = (r - f3) / 6; const f2 = r % 3; r = (r - f2) / 3; const f1 = r % 4; const f0 = (r - f1) / 4;
  if (!(f0 || f1 || f2 || f3 || f4)) return null;
  const o = {}; if (f0) o.weekday = WIDTHS[f0]; if (f1) o.era = WIDTHS[f1]; if (f2) o.year = NUMS[f2]; if (f3) o.month = MONTHS[f3]; if (f4) o.day = NUMS[f4];
  return o;
}
// time keys without seconds or fractional seconds: ((hour * 3 + minute) * 7 + cycle) * 4 + dayPeriod
function timeOpts(k) {
  let r = k; const dp = r % 4; r = (r - dp) / 4; const cy = r % 7; r = (r - cy) / 7; const mi = r % 3; const ho = (r - mi) / 3;
  if (!(ho || mi || dp)) return null;
  const o = {...CYC[cy]}; if (ho) o.hour = NUMS[ho]; if (mi) o.minute = NUMS[mi]; if (dp) o.dayPeriod = WIDTHS[dp];
  return o;
}
const ST = ["full", "long", "medium", "short"];
// a time key's levels: era and day (year and month format like day: the yMd fallback), then am/pm, hour, minute
const TLV = ["G", "d", "a", "h", "m"];
module.exports = {ZPREF, infer, predict, toks, PAIRS, LEVELS, nodeRange, dateOpts, timeOpts, same, fmt, local, W, KINDS, ZL};
if (require.main === module) {
  const T = JSON.parse(fs.readFileSync(__dirname + '/t-' + LOC + '.json'));
  const out = {date: {}, time: {}, timez: KINDS.map(() => ({})), sdate: [], stime: [], sboth: [], bad: []};
  const run = (o, lv, single) => { const r = infer(o, lv, single); if (r.bad) { out.bad.push([o, lv, r]); return r.bad; } return r; };
  for (let k = 0; k < 864; k++) {
    const o = dateOpts(k); if (!o) continue;
    out.date[k] = ["G", "y", "M", "d"].map(lv => run(o, lv, T.date[k]));
  }
  const tk = (ho, mi, cy, dp) => ((((ho * 3 + mi) * 3 + 0) * 7 + cy) * 4 + dp) * 4;
  for (let k = 0; k < 252; k++) {
    const o = timeOpts(k); if (!o) continue;
    const dp = k % 4, cy = ((k - dp) / 4) % 7, mi = ((k - dp) / 4 - cy) / 7 % 3, ho = Math.floor(((k - dp) / 4 - cy) / 21);
    out.time[k] = TLV.map(lv => run(o, lv, T.time[tk(ho, mi, cy, dp)]));
    KINDS.forEach((z, i) => { out.timez[i][k] = TLV.map(lv => run({...o, timeZoneName: z}, lv, null)); });
  }
  for (const ds of ST) out.sdate.push(["G", "y", "M", "d"].map(lv => run({dateStyle: ds}, lv, null)));
  for (const c of CYC) out.stime.push(ST.map(ts => LEVELS.map(lv => run({timeStyle: ts, ...c}, lv, null))));
  for (const ds of ST) out.sboth.push(CYC.map(c => ST.map(ts => LEVELS.map(lv => run({dateStyle: ds, timeStyle: ts, ...c}, lv, null)))));
  // same-day glue between a date and a time range, and the fallback separator
  const [a, b] = PAIRS.h[0];
  const g = fmt({year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "numeric", hourCycle: "h23"}).formatRangeToParts(a, b);
  out.glue_parts = g.map(p => [p.type, p.value, p.source]);
  const f = fmt({second: "numeric"}).formatRangeToParts(...PAIRS.m[0]);
  out.sep = f.find(p => p.source === "shared").value;
  fs.writeFileSync(__dirname + '/r-' + LOC + '.json', JSON.stringify(out));
  const pool = new Set();
  const walk = v => Array.isArray(v) && typeof v[0] === "string" && v.length === 2 && !Array.isArray(v[1]) ? pool.add(JSON.stringify(v)) : v === "=" ? pool.add(v) : Array.isArray(v) ? v.forEach(walk) : typeof v === "object" && v ? Object.values(v).forEach(walk) : 0;
  walk([out.date, out.time, out.timez, out.sdate, out.stime, out.sboth]);
  console.log(LOC, "bad", out.bad.length, "distinct", pool.size, "sep", JSON.stringify(out.sep));
  for (const x of out.bad.slice(0, 5)) console.log(JSON.stringify(x).slice(0, 700));
}

