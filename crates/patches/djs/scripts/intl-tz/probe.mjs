// Probe the reference node's zone history: node probe.mjs OUT.json [shard nshards]
// For each canonical zone djs knows (zones.json), find every instant 1800..2101 where the UTC offset or an
// en-US zone name (short, long, shortGeneric, longGeneric) changes, to the second. Candidates come from a
// weekly grid bisected to the second plus the box's zoneinfo transitions; node decides what is kept.
import fs from "node:fs";
const [out, shard = "0", nsh = "1"] = process.argv.slice(2);
const zones = JSON.parse(fs.readFileSync(new URL("zones.json", import.meta.url))).filter((_, i) => i % +nsh === +shard);
const START = Date.UTC(1800, 0, 1) / 1000, END = Date.UTC(2101, 0, 1) / 1000, WEEK = 7 * 86400;
function tzif(z) {
  let b;
  try { b = fs.readFileSync("/usr/share/zoneinfo/" + z); } catch { return null; }
  const hdr = o => [20, 24, 28, 32, 36, 40].map(k => b.readUInt32BE(o + k));
  let [isutc, isstd, leap, timecnt, typecnt, charcnt] = hdr(0);
  let o = 44 + timecnt * 5 + typecnt * 6 + charcnt + leap * 8 + isstd + isutc;
  [isutc, isstd, leap, timecnt, typecnt, charcnt] = hdr(o);
  o += 44;
  const times = [], idx = [], types = [];
  for (let i = 0; i < timecnt; i++) times.push(Number(b.readBigInt64BE(o + i * 8)));
  for (let i = 0; i < timecnt; i++) idx.push(b[o + timecnt * 8 + i]);
  const to = o + timecnt * 9;
  for (let i = 0; i < typecnt; i++) types.push({ off: b.readInt32BE(to + i * 6), dst: b[to + i * 6 + 4] });
  return { times, idx, types };
}
const res = {};
for (const z of zones) {
  const fo = new Intl.DateTimeFormat("en-US", { timeZone: z, hourCycle: "h23", year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "numeric", second: "numeric" });
  const fn = ["short", "long", "shortGeneric", "longGeneric"].map(k => new Intl.DateTimeFormat("en-US", { timeZone: z, timeZoneName: k, year: "numeric" }));
  const off = t => {
    const p = {};
    for (const x of fo.formatToParts(t * 1000)) p[x.type] = +x.value;
    return Date.UTC(p.year, p.month - 1, p.day, p.hour, p.minute, p.second) / 1000 - t;
  };
  const key = t => off(t) + "|" + fn.map(f => f.formatToParts(t * 1000).find(x => x.type === "timeZoneName").value).join("|");
  const cps = new Set();
  let t0 = START, k0 = key(START);
  while (t0 < END) {
    const t1 = Math.min(t0 + WEEK, END), k1 = key(t1);
    if (k1 === k0) { t0 = t1; continue; }
    // bisect for the first second whose key differs, then rescan from there
    let lo = t0, hi = t1;
    while (hi - lo > 1) { const m = Math.floor((lo + hi) / 2); if (key(m) === k0) lo = m; else hi = m; }
    cps.add(hi); t0 = hi; k0 = key(hi);
  }
  const zi = tzif(z);
  if (zi) for (const T of zi.times) if (T > START && T < END && key(T - 1) !== key(T)) cps.add(T);
  const dstAt = t => { if (!zi || !zi.times.length) return 0; let i = -1; for (let j = 0; j < zi.times.length && zi.times[j] <= t; j++) i = j; return i < 0 ? 0 : zi.types[zi.idx[i]].dst; };
  const periods = [];
  for (const t of [START, ...[...cps].sort((a, b) => a - b)]) { const k = key(t).split("|"); periods.push([t, +k[0], dstAt(t), k.slice(1)]); }
  res[z] = periods;
}
fs.writeFileSync(out, JSON.stringify(res));
