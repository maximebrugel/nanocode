// derive.js: lunisolar and Umm al-Qura tables from node's ICU over 1900-2100, written to tables.json
// chinese / dangi: per lunar year from the one containing 1900-01-01: [new year day (days since 1970), month lengths bitmask
// (bit i: month i has 30 days, months in order incl. the leap month), leap month position (0: none, else the leap month
// follows regular month n), cyclic number of the year's 60-cycle era].
// islamic-umalqura: per AH year: [start day, 12-bit month-length mask].
const fs = require('fs');
const LO = Math.floor(Date.UTC(1599, 0, 1) / 864e5), HI = Math.floor(Date.UTC(2401, 11, 31) / 864e5);
function lunisolar(cal) {
  const f = new Intl.DateTimeFormat('en-US-u-ca-' + cal, {timeZone: 'UTC', year: 'numeric', month: 'numeric', day: 'numeric'});
  const years = []; let cur = null, mstart = null, mlen = [];
  for (let k = LO; k <= HI; k++) {
    const p = Object.fromEntries(f.formatToParts(k * 864e5).map(x => [x.type, x.value]));
    if (p.day === '1') {
      if (mstart !== null && cur) cur.lens.push(k - mstart);
      mstart = k;
      if (p.month === '1') { cur = {ny: k, ry: +p.relatedYear, cyc: p.year, months: ['1'], lens: []}; years.push(cur); }
      else if (cur) cur.months.push(p.month);
    }
  }
  return years.filter(y => y.lens.length === y.months.length).map(y => {
    const li = y.months.findIndex(m => m.endsWith('bis'));
    let mask = 0; y.lens.forEach((l, i) => { if (l === 30) mask |= 1 << i; else if (l !== 29) throw new Error('len ' + l); });
    return [y.ny, mask, li < 0 ? 0 : li, y.ry];
  });
}
function umalqura(cal) {
  const f = new Intl.DateTimeFormat('en-US-u-ca-' + cal, {timeZone: 'UTC', year: 'numeric', month: 'numeric', day: 'numeric'});
  const years = []; let cur = null, mstart = null;
  for (let k = LO; k <= HI; k++) {
    const p = Object.fromEntries(f.formatToParts(k * 864e5).map(x => [x.type, x.value]));
    if (p.day === '1') {
      if (mstart !== null && cur) cur.lens.push(k - mstart);
      mstart = k;
      if (p.month === '1') { cur = {start: k, y: +p.year, lens: []}; years.push(cur); }
    }
  }
  return years.filter(y => y.lens.length === 12).map(y => { let m = 0; y.lens.forEach((l, i) => { if (l === 30) m |= 1 << i; }); return [y.y, y.start, m]; });
}
const out = {chinese: lunisolar('chinese'), dangi: lunisolar('dangi'), 'islamic-umalqura': umalqura('islamic-umalqura'), islamic: umalqura('islamic')};
fs.writeFileSync(__dirname + '/tables.json', JSON.stringify(out));
for (const [k, v] of Object.entries(out)) console.log(k, v.length, JSON.stringify(v[0]), JSON.stringify(v[v.length - 1]));
let diff = 0; out.chinese.forEach((y, i) => { if (JSON.stringify(y) !== JSON.stringify(out.dangi[i])) diff++; }); console.log('chinese/dangi differing years', diff);
