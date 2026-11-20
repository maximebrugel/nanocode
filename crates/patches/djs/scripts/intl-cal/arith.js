// arith.js CAL...: checks cal.js against node over every day 1900-2100 (year, month name, day, era)
const {CAL} = require('./cal.js');
const cals = process.argv.slice(2);
for (const c of cals) {
  const f = new Intl.DateTimeFormat('en-US-u-ca-' + c, {timeZone: 'UTC', era: 'long', year: 'numeric', month: 'long', day: 'numeric'});
  const months = new Map(), eras = new Map(); let bad = 0, n = 0; const ex = [];
  for (let days = -25567; days < 47482; days++) {
    const p = Object.fromEntries(f.formatToParts(days * 864e5).map(x => [x.type, x.value]));
    const m = CAL[c](days); n++;
    const mk = m.month;
    if (!months.has(mk)) months.set(mk, p.month);
    if (!eras.has(m.era)) eras.set(m.era, p.era);
    if (p.relatedYear) { p.year = String(m.ry) === p.relatedYear ? String(m.year) : 'x'; if (!eras.has('c' + m.cyc)) eras.set('c' + m.cyc, p.yearName); if (eras.get('c' + m.cyc) !== p.yearName) p.year = 'y'; p.era = eras.get(m.era); }
    const ok = String(m.year) === p.year && String(m.day) === p.day && months.get(mk) === p.month && eras.get(m.era) === p.era;
    if (!ok) { bad++; if (ex.length < 5) ex.push(new Date(days * 864e5).toISOString().slice(0, 10) + ' node ' + JSON.stringify(p) + ' model ' + JSON.stringify(m)); }
  }
  console.log(c, 'days', n, 'bad', bad); ex.forEach(e => console.log('  ', e));
}