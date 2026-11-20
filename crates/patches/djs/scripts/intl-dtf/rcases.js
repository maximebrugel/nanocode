// rcases.js LOC N SEED : JSON list of [options, start, end] for rmatrix.py: random option sets (fields,
// zone names, styles, hour cycles) x date pairs whose greatest differing field is each level in turn.
const LOC = process.argv[2], N = +(process.argv[3] || 200);
let seed = +(process.argv[4] || 7);
const rnd = n => { seed = (seed * 1103515245 + 12345) % 2147483648; return Math.floor(seed / 2147483648 * n); };
const W = ["long", "short", "narrow"], NU = ["numeric", "2-digit"], MO = ["numeric", "2-digit", "long", "short", "narrow"];
const ZN = ["short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"], ST = ["full", "long", "medium", "short"];
const CY = [{}, {hour12: true}, {hour12: false}, {hourCycle: "h11"}, {hourCycle: "h12"}, {hourCycle: "h23"}, {hourCycle: "h24"}];
// a few fixed-offset zones: zone history is format's concern, tested by scripts/intl-tz
const ZONES = ["UTC", "Asia/Tokyo", "Asia/Kolkata", "Etc/GMT+5"];
const out = [];
for (let i = 0; i < N; i++) {
  const o = {timeZone: ZONES[rnd(ZONES.length)]};
  const kind = rnd(10);
  if (kind === 0) { o.dateStyle = ST[rnd(4)]; if (rnd(2)) o.timeStyle = ST[rnd(4)]; }
  else if (kind === 1) { o.timeStyle = ST[rnd(4)]; }
  else {
    const date = kind !== 2, time = kind >= 2 && kind !== 3;
    if (date) { if (!rnd(4)) o.weekday = W[rnd(3)]; if (!rnd(6)) o.era = W[rnd(3)]; if (rnd(2)) o.year = NU[rnd(2)]; if (rnd(3)) o.month = MO[rnd(5)]; if (rnd(2)) o.day = NU[rnd(2)]; }
    if (time) { if (rnd(4)) o.hour = NU[rnd(2)]; if (rnd(2)) o.minute = NU[rnd(2)]; if (!rnd(4)) o.second = NU[rnd(2)]; if (!rnd(6)) o.dayPeriod = W[rnd(3)]; if (!rnd(8)) o.fractionalSecondDigits = 1 + rnd(3); }
    if (!rnd(5)) o.timeZoneName = ZN[rnd(6)];
  }
  Object.assign(o, CY[rnd(7)]);
  const a = Date.UTC(1960 + rnd(90), rnd(12), 1 + rnd(28), rnd(24), rnd(60), rnd(60), rnd(1000));
  const deltas = [Date.UTC(-1 - rnd(300), rnd(12), 1 + rnd(28), rnd(24)) - a, 86400000 * (400 + rnd(3000)), 86400000 * (32 + rnd(300)), 86400000 * (1 + rnd(25)), 3600000 * (1 + rnd(14)), 3600000 * (1 + rnd(3)), 60000 * (1 + rnd(50)), 1000 * (1 + rnd(50)), 1 + rnd(900), 0];
  for (const d of deltas) out.push([o, a, a + (d === deltas[0] || rnd(5) ? d : -d)]);
}
process.stdout.write(JSON.stringify(out));

