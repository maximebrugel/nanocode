// alias.js CANON ALIAS... : does node format ALIAS exactly like CANON (digits normalized to latn)?
const [canon, ...aliases] = process.argv.slice(2);
const W = ["narrow", "short", "long"], N = ["numeric", "2-digit"], M = ["numeric", "2-digit", "narrow", "short", "long"];
const TZN = ["short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"];
const ZONES = ["UTC", "America/New_York", "Europe/Berlin", "Asia/Kolkata", "Asia/Tokyo", "Europe/London", "America/Mexico_City", "Asia/Shanghai", "Australia/Sydney", "Africa/Cairo", "America/Sao_Paulo", "Europe/Moscow"];
const DATES = [Date.UTC(2024, 2, 5, 14, 7, 9, 123), Date.UTC(1999, 11, 31, 23, 59, 59), Date.UTC(2024, 6, 15, 0, 30, 0), Date.UTC(-50, 0, 1, 3, 0, 0), Date.UTC(2023, 9, 29, 8, 5, 1)];
let seed = 7; const rnd = n => { seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5; return (seed >>> 0) % n; };
const opts = [];
for (const ds of [undefined, "full", "long", "medium", "short"]) for (const ts of [undefined, "full", "long", "medium", "short"]) if (ds || ts) opts.push({dateStyle: ds, timeStyle: ts});
for (let i = 0; i < 600; i++) {
  const o = {};
  if (rnd(2)) o.weekday = W[rnd(3)]; if (!rnd(5)) o.era = W[rnd(3)]; if (rnd(2)) o.year = N[rnd(2)]; if (rnd(2)) o.month = M[rnd(5)]; if (rnd(2)) o.day = N[rnd(2)];
  if (rnd(2)) o.hour = N[rnd(2)]; if (rnd(2)) o.minute = N[rnd(2)]; if (rnd(2)) o.second = N[rnd(2)]; if (!rnd(6)) o.dayPeriod = W[rnd(3)]; if (!rnd(6)) o.fractionalSecondDigits = 1 + rnd(3);
  if (!rnd(3)) o.timeZoneName = TZN[rnd(6)]; const c = rnd(4); if (c === 1) o.hour12 = true; if (c === 2) o.hour12 = false; if (c === 3) o.hourCycle = ["h11", "h12", "h23", "h24"][rnd(4)];
  opts.push(o);
}
const norm = s => s.replace(/\u066b/g, ".").replace(/[\u0660-\u0669]/g, c => String(c.charCodeAt(0) - 0x660)).replace(/[\u06f0-\u06f9]/g, c => String(c.charCodeAt(0) - 0x6f0));
const run = l => opts.flatMap(o => ZONES.map(z => { const f = new Intl.DateTimeFormat(l, {...o, timeZone: z}); return DATES.map(d => norm(f.format(d))).join("\n"); }));
const base = run(canon);
for (const a of aliases) {
  const got = run(a); let bad = 0, ex = null;
  got.forEach((g, i) => { if (g !== base[i]) { bad++; ex = ex || [JSON.stringify(opts[Math.floor(i / ZONES.length)]), g.split("\n")[0], base[i].split("\n")[0]]; } });
  const r = new Intl.DateTimeFormat(a).resolvedOptions();
  console.log(canon, a, r.locale, r.numberingSystem, bad ? "DIFF " + bad + " " + JSON.stringify(ex) : "same");
}
