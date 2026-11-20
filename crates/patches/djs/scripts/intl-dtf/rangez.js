// rangez.js LOC -> rz-LOC.json : formatRange interval patterns for every date key (no time fields)
// with each timeZoneName kind, inferred from node like range.js (run after range.js LOC). ICU keeps a
// zone named by offset (O) out of the interval skeleton, so these differ from the zone-less entries
// in ways not worth modeling: the table is read as is.
const fs = require('fs');
const {infer, dateOpts, KINDS, ZPREF, ZL} = require('./range.js');
const {LOC} = require('./infer.js');
const out = {datez: KINDS.map(() => ({})), bad: []};
for (let z = 0; z < KINDS.length; z++) for (let k = 0; k < 864; k++) {
  const o = dateOpts(k); if (!o) continue;
  ZPREF.kind = z;
  out.datez[z][k] = ["G", "y", "M", "d"].map(lv => {
    const oz = {...o, timeZoneName: KINDS[z]};
    const r = infer(oz, lv, ZL[z]);
    if (r.bad) { out.bad.push([oz, lv, r]); return null; }
    return r;
  });
}
fs.writeFileSync(__dirname + '/rz-' + LOC + '.json', JSON.stringify(out));
console.log(LOC, "bad", out.bad.length);
for (const x of out.bad.slice(0, 4)) console.log(JSON.stringify(x).slice(0, 500));
