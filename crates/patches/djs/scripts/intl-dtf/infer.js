
// infer.js LOC -> JSON tables for djs DateTimeFormat, inferred from node's ICU
const LOC = process.argv[2];
const WIDTHS = ["", "long", "short", "narrow"], NUMS = ["", "numeric", "2-digit"], MONTHS = ["", "numeric", "2-digit", "long", "short", "narrow"];
const CYC = [{}, {hour12: true}, {hour12: false}, {hourCycle: "h12"}, {hourCycle: "h23"}, {hourCycle: "h11"}, {hourCycle: "h24"}];
const U = (y, mo, d, h = 0, mi = 0, s = 0, ms = 0) => { const t = new Date(Date.UTC(2000, mo, d, h, mi, s, ms)); t.setUTCFullYear(y); return t; };
const REF = U(2024, 2, 5, 4, 7, 9, 12);
const DTZ = process.env.DTZ || "UTC";
const fmt = (o) => new Intl.DateTimeFormat(LOC, {timeZone: DTZ, ...o});
const part = (o, d, ty) => { const p = fmt(o).formatToParts(d).find(p => p.type === ty); return p ? p.value : null; };
const W3 = ["long", "short", "narrow"];
// native digits (ar-EG and the like resolve numberingSystem "arab"); "" for latn
const ZERO = (() => { const z = new Intl.NumberFormat(LOC, {useGrouping: false}).format(0); return z === "0" ? "" : z; })();
const dig = s => ZERO ? s.replace(/[0-9]/g, c => String.fromCodePoint(ZERO.codePointAt(0) + +c)) : s;
const undig = s => ZERO ? s.replace(/./gu, c => { const d = c.codePointAt(0) - ZERO.codePointAt(0); return d >= 0 && d <= 9 ? String(d) : c; }) : s;
const NUMT = new Set(["year", "month", "day", "hour", "minute", "second", "fractionalSecond"]);
const names = {months: {}, days: {}};
for (const w of W3) {
  names.months["L" + w] = Array.from({length: 12}, (_, m) => part({month: w}, U(2024, m, 5), "month"));
  names.months["M" + w] = Array.from({length: 12}, (_, m) => part({month: w, day: "numeric"}, U(2024, m, 5), "month"));
  if (names.months["M" + w].every(v => /^\d+$/.test(undig(v)))) names.months["M" + w] = names.months["L" + w];
  // el: the month alone is genitive like the format form; its standalone forms (nominative, accented abbreviations) show with a year and era
  if (names.months["L" + w].join() === names.months["M" + w].join()) {
    const ly = Array.from({length: 12}, (_, m) => part({month: w, year: "numeric", era: "short"}, U(2024, m, 5), "month"));
    if (ly.every(v => !/^\d+$/.test(undig(v)))) names.months["L" + w] = ly;
  }
  names.days["c" + w] = Array.from({length: 7}, (_, d) => part({weekday: w}, U(2024, 2, 3 + d), "weekday"));
  names.days["E" + w] = Array.from({length: 7}, (_, d) => part({weekday: w, month: "long", day: "numeric"}, U(2024, 2, 3 + d), "weekday"));
  // fi: the format forms (essive "tiistaina") show with a day but no month; with a month the pattern uses standalone ones
  if (names.days["E" + w].join() === names.days["c" + w].join()) {
    const wd = Array.from({length: 7}, (_, d) => part({weekday: w, day: "numeric"}, U(2024, 2, 3 + d), "weekday"));
    if (wd.every(v => v)) names.days["E" + w] = wd;
  }
}
names.eras = W3.map(w => [part({era: w, year: "numeric"}, U(-5, 0, 1), "era"), part({era: w, year: "numeric"}, U(2024, 0, 1), "era")]);
names.ampm = [part({hour: "numeric", hourCycle: "h12"}, U(2024, 0, 1, 4), "dayPeriod"), part({hour: "numeric", hourCycle: "h12"}, U(2024, 0, 1, 16), "dayPeriod")];
names.periods = W3.map(w => {
  const a = Array.from({length: 24}, (_, h) => part({hour: "numeric", minute: "numeric", second: "numeric", dayPeriod: w, hourCycle: "h12"}, U(2024, 0, 1, h, 30, 30), "dayPeriod"));
  a.push(part({hour: "numeric", minute: "numeric", second: "numeric", dayPeriod: w, hourCycle: "h12"}, U(2024, 0, 1, 0, 0), "dayPeriod"));
  a.push(part({hour: "numeric", minute: "numeric", second: "numeric", dayPeriod: w, hourCycle: "h12"}, U(2024, 0, 1, 12, 0), "dayPeriod"));
  return a;
});
// ---- formatter (mirror of the Rust one) ----
function tokens(p) {
  const out = []; let i = 0;
  while (i < p.length) {
    const c = p[i];
    if (c === "'") { let j = i + 1, lit = ""; while (j < p.length) { if (p[j] === "'") { if (p[j + 1] === "'") { lit += "'"; j += 2; continue; } break; } lit += p[j++]; } if (!lit && j === i + 1) lit = "'"; out.push(["lit", lit]); i = j + 1; continue; }
    if (!/[A-Za-z]/.test(c)) { out.push(["lit", c]); i++; continue; }
    let n = 1; while (p[i + n] === c) n++; out.push([c, n]); i += n;
  }
  return out;
}
const WI = n => n === 4 ? "long" : n === 5 ? "narrow" : "short";
function render(p, d, hc, bw) {
  const y = d.getUTCFullYear(), mo = d.getUTCMonth(), dd = d.getUTCDate(), wd = d.getUTCDay(), h = d.getUTCHours(), mi = d.getUTCMinutes(), s = d.getUTCSeconds(), ms = d.getUTCMilliseconds();
  const num = (v, n) => n >= 2 ? String(v % 100).padStart(2, "0") : String(v);
  const ey = y <= 0 ? 1 - y : y;
  let r = [];
  const tk = tokens(p), hasM = tk.some(t => t[0] === "m"), hasS = tk.some(t => t[0] === "s");
  const exact = (!hasM || mi === 0) && (!hasS || s === 0);
  for (const [c, n] of tk) {
    if (c === "lit") { r.push(["literal", n]); continue; }
    let ty, v;
    switch (c) {
      case "E": case "c": ty = "weekday"; v = names.days[c + WI(n)][wd]; break;
      case "G": ty = "era"; v = names.eras[W3.indexOf(WI(n))][y > 0 ? 1 : 0]; break;
      case "y": ty = "year"; v = n === 2 ? String(ey % 100).padStart(2, "0") : String(ey); break;
      case "M": case "L": ty = "month"; v = n <= 2 ? num(mo + 1, n) : names.months[c + WI(n)][mo]; break;
      case "d": ty = "day"; v = num(dd, n); break;
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
  // merge adjacent literals like ICU
  const m = [];
  for (const x of r) { if (x[0] === "literal" && m.length && m[m.length - 1][0] === "literal") m[m.length - 1][1] += x[1]; else m.push([x[0], x[1]]); }
  return m.map(x => x[0] + ":" + (NUMT.has(x[0]) ? dig(x[1]) : x[1])).join("|");
}
const nodeParts = (o, d) => fmt(o).formatToParts(d).map(p => p.type + ":" + (p.type === "timeZoneName" ? "Z" : p.value)).join("|");
const VDATES = [REF, ...Array.from({length: 12}, (_, m) => U(2024, m, 5 + (m % 7), (m * 5) % 24, m * 4)), ...Array.from({length: 7}, (_, d) => U(2024, 7, 4 + d, 12, 0)), U(2024, 0, 1, 0, 0), U(2024, 0, 1, 12, 0, 1), U(2024, 0, 1, 12, 30, 0), U(2024, 0, 1, 0, 0, 7), U(2024, 0, 1, 23, 59), U(-5, 5, 6, 7, 8)];
const q = lit => /[A-Za-z']/.test(lit) ? "'" + lit.replace(/'/g, "''") + "'" : lit;
function infer(o) {
  const f = fmt(o), hc = f.resolvedOptions().hourCycle;
  const parts = f.formatToParts(REF);
  const HL = {h12: "h", h11: "K", h23: "H", h24: "k"}[hc];
  // alternatives per text field
  let alts = [""];
  const addAll = arr => { const n = []; for (const a of alts) for (const b of arr) n.push(a + b); alts = n; };
  for (let {type, value} of parts) {
    if (NUMT.has(type)) value = undig(value);
    switch (type) {
      case "literal": addAll([q(value)]); break;
      case "year": addAll([value.length === 2 ? "yy" : "y"]); break;
      case "day": addAll([value.length === 2 ? "dd" : "d"]); break;
      case "hour": addAll([HL.repeat(value.length === 2 ? 2 : 1)]); break;
      case "minute": addAll([value.length === 2 ? "mm" : "m"]); break;
      case "second": addAll([value.length === 2 ? "ss" : "s"]); break;
      case "fractionalSecond": addAll(["S".repeat(value.length)]); break;
      case "month": if (/^\d+$/.test(value)) addAll([value.length === 2 ? "MM" : "M"]); else { const c = []; for (const w of W3) for (const L of ["M", "L"]) if (names.months[L + w][2] === value) c.push(L.repeat(w === "long" ? 4 : w === "short" ? 3 : 5)); addAll(c.length ? c : ["MMM"]); } break;
      case "weekday": { const c = []; for (const w of W3) for (const L of ["E", "c"]) if (names.days[L + w][2] === value) c.push(L.repeat(w === "long" ? 4 : w === "short" ? 3 : 5)); addAll(c.length ? c : ["EEE"]); break; }
      case "era": { const c = []; W3.forEach((w, i) => { if (names.eras[i][1] === value) c.push("G".repeat(w === "long" ? 4 : w === "short" ? 1 : 5)); }); addAll(c.length ? c : ["G"]); break; }
      case "dayPeriod": { const c = []; if (value === names.ampm[0]) c.push("a"); W3.forEach((w, i) => { if (names.periods[i][4] === value) c.push("B".repeat(w === "long" ? 4 : w === "short" ? 1 : 5)); }); addAll(c.length ? c : ["a"]); break; }
      case "timeZoneName": addAll(["z"]); break;
      default: addAll(["?" + type]);
    }
    if (alts.length > 64) alts = alts.slice(0, 64);
  }
  // prefer requested widths: order alternatives so ones matching option widths come first
  for (const p of alts) {
    let ok = true;
    for (const d of VDATES) if (render(p, d, hc) !== nodeParts(o, d)) { ok = false; break; }
    if (ok) return {p, hc};
  }
  return {p: alts[0], hc, bad: true};
}
module.exports = {infer, names, render, nodeParts, fmt, VDATES, WIDTHS, NUMS, MONTHS, CYC, U, LOC};
if (require.main === module) {
  const out = {names, date: [], time: [], bad: []};
  const pats = new Map();
  for (let k = 0; k < 864; k++) {
    let r = k; const f4 = r % 3; r = (r - f4) / 3; const f3 = r % 6; r = (r - f3) / 6; const f2 = r % 3; r = (r - f2) / 3; const f1 = r % 4; const f0 = (r - f1) / 4;
    if (!(f0 || f1 || f2 || f3 || f4)) { out.date.push(null); continue; }
    const o = {}; if (f0) o.weekday = WIDTHS[f0]; if (f1) o.era = WIDTHS[f1]; if (f2) o.year = NUMS[f2]; if (f3) o.month = MONTHS[f3]; if (f4) o.day = NUMS[f4];
    const r2 = infer(o); out.date.push(r2.p); if (r2.bad) out.bad.push([o, r2.p]);
  }
  for (let k = 0; k < 3 * 3 * 3 * 7 * 4 * 4; k++) {
    let r = k; const fsd = r % 4; r = (r - fsd) / 4; const dp = r % 4; r = (r - dp) / 4; const cy = r % 7; r = (r - cy) / 7; const se = r % 3; r = (r - se) / 3; const mi = r % 3; const ho = (r - mi) / 3;
    if (!(ho || mi || se || dp || fsd)) { out.time.push(null); continue; }
    const o = {...CYC[cy]}; if (ho) o.hour = NUMS[ho]; if (mi) o.minute = NUMS[mi]; if (se) o.second = NUMS[se]; if (dp) o.dayPeriod = WIDTHS[dp]; if (fsd) o.fractionalSecondDigits = fsd;
    const r2 = infer(o); out.time.push(r2.p); if (r2.bad) out.bad.push([o, r2.p]);
  }
  process.stdout.write(JSON.stringify(out));
}
