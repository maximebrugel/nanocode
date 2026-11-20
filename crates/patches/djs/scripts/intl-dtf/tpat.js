// tpat.js LOC -> tp-LOC.json : ICU's own time pattern (DateIntervalFormat's getBestPattern) for every
// time key and zone kind, which formatRange uses in fallbacks (it can differ from format's: de HH vs H).
// Read from the day-level range of each time-only key: ICU prefixes yMd, so the fallback pattern is
// Dpat(yMd) + short glue + Tpat (or Tpat first: vi); we strip the date run and the glue.
const m = require('./infer.js');
const r = require('./range.js');
const fs = require('fs');
const {NUMS, WIDTHS, CYC, LOC} = m;
const {infer, toks, KINDS} = r;
const DT = {G: 1, y: 1, M: 1, L: 1, d: 1, E: 1, c: 1};
const isF = t => /^[A-Za-z]/.test(t);
// time key: ((((hour * 3 + minute) * 3 + second) * 7 + cycle) * 4 + dayPeriod) * 4 + fsd
function timeOpts(k) {
  let x = k; const fsd = x % 4; x = (x - fsd) / 4; const dp = x % 4; x = (x - dp) / 4; const cy = x % 7; x = (x - cy) / 7; const se = x % 3; x = (x - se) / 3; const mi = x % 3; const ho = (x - mi) / 3;
  if (!(ho || mi || se || dp || fsd)) return null;
  const o = {...CYC[cy]}; if (ho) o.hour = NUMS[ho]; if (mi) o.minute = NUMS[mi]; if (se) o.second = NUMS[se]; if (dp) o.dayPeriod = WIDTHS[dp]; if (fsd) o.fractionalSecondDigits = fsd;
  return o;
}
function split(P) {
  const ts = toks(P);
  const dIdx = ts.map((t, i) => isF(t) && DT[t[0]] ? i : -1).filter(i => i >= 0);
  const lo = dIdx[0], hi = dIdx[dIdx.length - 1];
  if (lo === 0) {
    let j = hi + 1; let g = ""; while (j < ts.length && !isF(ts[j])) g += ts[j++];
    return {tp: ts.slice(j).join(""), glue: "{1}" + g + "{0}", dp: ts.slice(0, hi + 1).join("")};
  }
  let j = lo - 1; let g = ""; while (j >= 0 && !isF(ts[j])) g = ts[j--] + g;
  return {tp: ts.slice(0, j + 1).join(""), glue: "{0}" + g + "{1}", tail: ts.slice(hi + 1).join(""), dp: ts.slice(lo, hi + 1).join("")};
}
module.exports = {timeOpts, split};
if (require.main === module) {
  const out = {tpat: [{}, {}, {}, {}, {}, {}, {}], bad: []};
  const glues = new Map();
  for (let k = 0; k < 3024; k++) {
    const o = timeOpts(k); if (!o) continue;
    for (let z = 0; z < 7; z++) {
      const oz = z === 0 ? o : {...o, timeZoneName: KINDS[z - 1]};
      const p = infer(oz, "d", null);
      if (p.bad || p === "=") { out.bad.push([oz, p]); continue; }
      const s = split(p[1]);
      if (s.tail) { out.bad.push([oz, "tail", p[1]]); continue; }
      glues.set(s.glue, (glues.get(s.glue) || 0) + 1);
      out.tpat[z][k] = s.tp;
    }
  }
  out.glues = [...glues].sort((a, b) => b[1] - a[1]);
  fs.writeFileSync(__dirname + '/tp-' + LOC + '.json', JSON.stringify(out));
  console.log(LOC, "bad", out.bad.length, "glues", JSON.stringify(out.glues));
  for (const x of out.bad.slice(0, 4)) console.log(JSON.stringify(x).slice(0, 400));
}

