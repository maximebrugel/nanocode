// rzfilter.js LOC : drop rz-LOC.json entries range.rs derives on its own (a fallback of the format
// pattern, or "=" where the pattern shows nothing at the level), keeping the table small. Writes
// rzf-LOC.json, which rangeemit.js reads.
const fs = require('fs');
const {infer, LOC} = require('./infer.js');
const {dateOpts, KINDS} = require('./range.js');
const RZ = JSON.parse(fs.readFileSync(__dirname + '/rz-' + LOC + '.json'));
const R = JSON.parse(fs.readFileSync(__dirname + '/r-' + LOC + '.json'));
// pattern text with quotes resolved, letter runs read by letter (inferred widths can be ambiguous;
// range.rs uses the format pattern) and any zone run read as one "z"
function norm(p) {
  let out = "";
  for (let i = 0; i < p.length;) {
    const c = p[i];
    if (c === "'") {
      if (p[i + 1] === "'") { out += "'"; i += 2; continue; }
      i++;
      while (i < p.length) { if (p[i] === "'") { if (p[i + 1] === "'") { out += "'"; i += 2; continue; } i++; break; } out += "\u0000" + p[i++]; }
    } else if (/[A-Za-z]/.test(c)) { let j = i; while (p[j] === c) j++; out += /[zOvVX]/.test(c) ? "<z>" : "<" + c + ">"; i = j; }
    else out += "\u0000" + p[i++];
  }
  return out;
}
const UNIT = {G: 0, y: 10, u: 10, U: 10, M: 20, L: 20, d: 30, E: 30, c: 30};
const LU = [0, 10, 20, 30];
const ignored = (lv, p) => ![...p.replace(/'[^']*'/g, "")].some(ch => UNIT[ch] !== undefined && UNIT[ch] >= LU[lv]);
const out = {datez: KINDS.map(() => ({}))};
let kept = 0, dropped = 0;
for (let z = 0; z < KINDS.length; z++) for (const k in RZ.datez[z]) {
  const o = dateOpts(+k);
  const single = infer({...o, timeZoneName: KINDS[z]}).p;
  out.datez[z][k] = RZ.datez[z][k].map((e, lv) => {
    const derived = ignored(lv, single) ? e === "=" : Array.isArray(e) && norm(e[1]) === norm(single) && norm(e[0]) === norm(single) + norm("'" + R.sep.replace(/'/g, "''") + "'");
    if (derived) { dropped++; return null; }
    kept++; return e;
  });
}
fs.writeFileSync(__dirname + '/rzf-' + LOC + '.json', JSON.stringify(out));
console.log(LOC, "kept", kept, "dropped", dropped);
