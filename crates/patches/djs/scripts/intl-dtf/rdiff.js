// rdiff.js LOC [N] : differential of rangemodel.js against node's formatRangeToParts: random option
// sets x date pairs differing first at each level (America/New_York wall times).
const m = require('./infer.js');
const r = require('./range.js');
const {tmpl} = require('./glue.js');
const M = require('./rangemodel.js');
const {WIDTHS, NUMS, MONTHS, CYC, LOC} = m;
const {nodeRange, predict, W, same, fmt, local, KINDS} = r;
const N = +(process.argv[3] || 3000);
const d = M.load(LOC);
const T = d.T;
const t0 = T.time[M.tkey([0, 0, 0, 0, 0, 0, 1, 1, 0], 4, 0)];
const glueOf = (o, dp) => tmpl(m.infer({...o, hour: "numeric", minute: "numeric", hourCycle: "h23"}).p, dp, t0);
const dk = a => M.dkey(a);
const glues = [glueOf({weekday: "long", month: "long", day: "numeric"}, T.date[dk([1, 0, 0, 3, 1])]), glueOf({month: "long", day: "numeric"}, T.date[dk([0, 0, 0, 3, 1])]),
  glueOf({month: "short", day: "numeric"}, T.date[dk([0, 0, 0, 4, 1])]), glueOf({month: "numeric", day: "numeric"}, T.date[dk([0, 0, 0, 1, 1])])];
let seed = 12345;
const rnd = n => { seed = (seed * 1103515245 + 12345) % 2147483648; return Math.floor(seed / 2147483648 * n); };
const pick = a => a[rnd(a.length)];
function randSpec() {
  const s = {f: [0, 0, 0, 0, 0, 0, 0, 0, 0], fsd: 0, cy: 0, z: 0, ds: -1, ts: -1, glues};
  const o = {};
  if (rnd(8) === 0) {
    const k = rnd(3);
    if (k !== 1) { s.ds = rnd(4); o.dateStyle = M.ST ? 0 : ["full", "long", "medium", "short"][s.ds]; }
    if (k !== 0) { s.ts = rnd(4); o.timeStyle = ["full", "long", "medium", "short"][s.ts]; }
    s.cy = rnd(7); Object.assign(o, CYC[s.cy]);
    return [s, o];
  }
  const p = rnd(4); // 0 date, 1 time, 2 both, 3 zone-ish
  if (p !== 1) {
    if (rnd(3) === 0) s.f[0] = 1 + rnd(3);
    if (rnd(5) === 0) s.f[1] = 1 + rnd(3);
    if (rnd(2)) s.f[2] = 1 + rnd(2);
    if (rnd(3)) s.f[3] = 1 + rnd(5);
    if (rnd(2)) s.f[4] = 1 + rnd(2);
  }
  if (p !== 0) {
    if (rnd(4)) s.f[6] = 1 + rnd(2);
    if (rnd(2)) s.f[7] = 1 + rnd(2);
    if (rnd(4) === 0) s.f[8] = 1 + rnd(2);
    if (rnd(6) === 0) s.f[5] = 1 + rnd(3);
    if (rnd(8) === 0) s.fsd = 1 + rnd(3);
    s.cy = rnd(7);
  }
  if (p === 3 || rnd(5) === 0) s.z = 1 + rnd(6);
  const names = ["weekday", "era", "year", "month", "day", "dayPeriod", "hour", "minute", "second"];
  const vals = [WIDTHS, WIDTHS, NUMS, MONTHS, NUMS, WIDTHS, NUMS, NUMS, NUMS];
  s.f.forEach((v, i) => { if (v) o[names[i]] = vals[i][v]; });
  if (s.fsd) o.fractionalSecondDigits = s.fsd;
  Object.assign(o, CYC[s.cy]);
  if (s.z) o.timeZoneName = KINDS[s.z - 1];
  if (!s.f.some(x => x) && !s.fsd) { s.f[2] = 1; s.f[3] = 1; s.f[4] = 1; }
  return [s, o];
}
function pair(level) {
  const y = 1950 + rnd(100), mo = rnd(12), dd = 1 + rnd(27), h = rnd(24), mi = rnd(60), se = rnd(60);
  const a = W(y, mo, dd, h, mi, se);
  let b;
  switch (level) {
    case 0: b = W(-rnd(300), rnd(12), 1 + rnd(27), rnd(24), rnd(60), rnd(60)); break;
    case 1: b = W(y + 1 + rnd(5), rnd(12), 1 + rnd(27), rnd(24), rnd(60), rnd(60)); break;
    case 2: b = W(y, (mo + 1 + rnd(11)) % 12, 1 + rnd(27), rnd(24), rnd(60), rnd(60)); break;
    case 3: b = W(y, mo, (dd % 27) + 1, rnd(24), rnd(60), rnd(60)); break;
    case 4: b = W(y, mo, dd, (h + 12) % 24, rnd(60), rnd(60)); break;
    case 5: b = W(y, mo, dd, h < 12 ? (h + 1 + rnd(11 - h % 12 || 1)) % 12 + (h >= 12 ? 12 : 0) : h, rnd(60), rnd(60)); if (h % 12 === 11) b = W(y, mo, dd, h - 5, rnd(60)); break;
    case 6: b = W(y, mo, dd, h, (mi + 1 + rnd(59)) % 60, rnd(60)); break;
    case 7: b = W(y, mo, dd, h, mi, (se + 1 + rnd(59)) % 60); break;
    default: b = new Date(a.getTime() + 1 + rnd(998));
  }
  return [a, b];
}
function levelOf(a, b) {
  const pa = local(a), pb = local(b);
  const ea = pa.getUTCFullYear() > 0, eb = pb.getUTCFullYear() > 0;
  const fa = [ea, pa.getUTCFullYear(), pa.getUTCMonth(), pa.getUTCDate(), pa.getUTCHours() >= 12, pa.getUTCHours() % 12, pa.getUTCMinutes(), pa.getUTCSeconds(), pa.getUTCMilliseconds()];
  const fb2 = [eb, pb.getUTCFullYear(), pb.getUTCMonth(), pb.getUTCDate(), pb.getUTCHours() >= 12, pb.getUTCHours() % 12, pb.getUTCMinutes(), pb.getUTCSeconds(), pb.getUTCMilliseconds()];
  const i = fa.findIndex((v, k) => v !== fb2[k]);
  return i < 0 ? 9 : i;
}
let ok = 0, bad = 0; const fails = {};
for (let i = 0; i < N; i++) {
  const [s, o] = randSpec();
  let single;
  try { single = m.infer(o).p; } catch (e) { continue; }
  s.single = single;
  s.singleOf = g => { const o2 = {...o}; ["weekday", "era", "year", "month", "day"].forEach((n, i) => { if (g[i]) o2[n] = [WIDTHS, WIDTHS, NUMS, MONTHS, NUMS][i][g[i]]; }); return m.infer(o2).p; };
  for (let level = 0; level < 9; level++) {
    const [a, b] = pair(level);
    const lv = levelOf(a, b);
    if (lv === 9) continue;
    const want = nodeRange(o, a, b);
    let got;
    try {
      const e = M.entry(d, s, lv);
      got = e === "=" ? fmt(o).formatToParts(a).map(p => [p.type, p.value, 2]) : predict(e, a, b);
      if (e === "=") want.forEach(p => { if (p[0] === "timeZoneName") p[1] = fmt(o).formatToParts(a).find(x => x.type === "timeZoneName").value; });
      if (!same(want, got)) { const key = JSON.stringify(o) + " L" + lv; const W2 = want.map(p => p[1] + "/" + p[2]), G2 = got.map(p => p[1] + "/" + p[2]); let j = 0; while (W2[j] === G2[j]) j++; fails[key] = {e, at: j, want: W2.slice(Math.max(0, j - 2), j + 4).join("|"), got: G2.slice(Math.max(0, j - 2), j + 4).join("|")}; bad++; } else ok++;
    } catch (err) { bad++; fails[JSON.stringify(o) + " L" + lv] = String(err.stack).slice(0, 300); }
  }
}
console.log(LOC, "ok", ok, "bad", bad);
const byLv = {}; for (const k of Object.keys(fails)) { const c = (k.includes("Style") ? "style " : "") + k.slice(k.lastIndexOf(" ") + 1); byLv[c] = (byLv[c] || 0) + 1; } console.log(JSON.stringify(byLv));
const ks = Object.keys(fails);
for (const k of ks.slice(0, +(process.argv[4] || 8))) console.log(k, JSON.stringify(fails[k]));

