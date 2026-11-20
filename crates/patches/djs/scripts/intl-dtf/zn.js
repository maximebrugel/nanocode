
const fs = require('fs');
const LOC = process.argv[2];
const zones = fs.readFileSync(__dirname + '/zones.tsv', 'utf8').trim().split('\n').map(l => l.split('\t'));
function stdOff(posix) { let i = 0; if (posix[0] === '<') i = posix.indexOf('>') + 1; else while (/[A-Za-z]/.test(posix[i])) i++; const m = posix.slice(i).match(/^([+-]?)(\d+)(?::(\d+))?(?::(\d+))?/); const v = (+m[2]) * 60 + (+(m[3] || 0)); return -(m[1] === '-' ? -v : v); }
const KINDS = ["short", "long", "shortGeneric", "longGeneric"];
const dates = [new Date(Date.UTC(2024, 0, 15, 12)), new Date(Date.UTC(2024, 6, 15, 12))];
const OFF = new Map();
const offFmt = z => { if (!OFF.has(z)) OFF.set(z, new Intl.DateTimeFormat('en', {timeZone: z, timeZoneName: 'longOffset'})); return OFF.get(z); };
const offMin = (z, d) => { const s = offFmt(z).formatToParts(d).find(p => p.type === 'timeZoneName').value; const m = s.match(/GMT([+-])(\d\d):(\d\d)/); return m ? (m[1] === '-' ? -1 : 1) * (+m[2] * 60 + +m[3]) : 0; };
const offMinOld = (z, d) => { const s = new Intl.DateTimeFormat('en', {timeZone: z, timeZoneName: 'longOffset'}).formatToParts(d).find(p => p.type === 'timeZoneName').value; const m = s.match(/GMT([+-])(\d\d):(\d\d)/); return m ? (m[1] === '-' ? -1 : 1) * (+m[2] * 60 + +m[3]) : 0; };
const name = (z, d, k) => new Intl.DateTimeFormat(LOC, {timeZone: z, timeZoneName: k}).formatToParts(d).find(p => p.type === 'timeZoneName').value;
const out = {};
for (const [z, posix] of zones) {
  const so = stdOff(posix);
  const e = [null, null, null, null, null, null];
  for (const d of dates) {
    const dst = offMin(z, d) !== so;
    e[dst ? 1 : 0] = name(z, d, 'short'); e[dst ? 3 : 2] = name(z, d, 'long');
    if (!dst) { e[4] = name(z, d, 'shortGeneric'); e[5] = name(z, d, 'longGeneric'); }
  }
  if (e[1] === null) { e[1] = e[0]; e[3] = e[2]; }
  if (e[4] === null) { e[4] = name(z, dates[0], 'shortGeneric'); e[5] = name(z, dates[0], 'longGeneric'); }
  // generic names in the repeated hour before DST ends (ICU then qualifies non-golden zones)
  let alt = [null, null];
  if (e[1] !== e[0] || e[3] !== e[2]) {
    let t = Date.UTC(2024, 0, 1), prev = offMin(z, new Date(t));
    for (let h = 1; h < 367; h++) {
      const o = offMin(z, new Date(t + h * 86400e3));
      if (o < prev) {
        let lo = t + (h - 1) * 86400e3, hi = t + h * 86400e3;
        while (hi - lo > 1000) { const mid = Math.floor((lo + hi) / 2000) * 1000; if (offMin(z, new Date(mid)) === prev) lo = mid; else hi = mid; }
        const d = new Date(hi - 1000);
        const a = name(z, d, 'shortGeneric'), b = name(z, d, 'longGeneric');
        if (a !== e[4] || b !== e[5]) alt = [a, b];
        break;
      }
      prev = o;
    }
  }
  out[z] = e.concat(alt);
}
// localized GMT format probes
const g = {};
for (const [k, d] of [["shortOffset", 0], ["longOffset", 0]]) {
  g[k] = ["UTC", "America/Los_Angeles", "Asia/Kolkata", "Asia/Tokyo"].map(z => name(z, dates[0], k));
}
out["@gmt"] = g;
process.stdout.write(JSON.stringify(out));
