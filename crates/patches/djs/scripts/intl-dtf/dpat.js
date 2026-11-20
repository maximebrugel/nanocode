// dpat.js LOC : adds to tp-LOC.json ICU's date pattern next to a time range (dpat, by date key) and
// the medium glue (gm) it is joined with; run after range.js and tpat.js.
const m = require('./infer.js');
const r = require('./range.js');
const fs = require('fs');
const {LOC} = m;
const {infer} = r;
const file = __dirname + '/tp-' + LOC + '.json';
const out = JSON.parse(fs.readFileSync(file));
out.bad = out.bad.filter(b => b[1] !== "dpat");
// ICU's date pattern next to a time range (fDatePattern, from the normalized date skeleton): the
// date run of a same-day range of the date key with hour and minute (h23), whose time interval
// pattern is the time-only key's, joined by the medium glue.
const T = JSON.parse(fs.readFileSync(__dirname + '/t-' + LOC + '.json'));
const R = JSON.parse(fs.readFileSync(__dirname + '/r-' + LOC + '.json'));
const {tmpl} = require('./glue.js');
const dk = f => (((f[0] * 4 + f[1]) * 3 + f[2]) * 6 + f[3]) * 3 + f[4];
const t0 = T.time[((((1 * 3 + 1) * 3 + 0) * 7 + 4) * 4 + 0) * 4];
const gm = tmpl(m.infer({month: "short", day: "numeric", hour: "numeric", minute: "numeric", hourCycle: "h23"}).p, T.date[dk([0, 0, 0, 4, 1])], t0);
const ti = R.time[((1 * 3 + 1) * 7 + 4) * 4][3];
// ICU's interval glue can differ from format's (pt-BR, id join with a space): take whichever of
// format's glue, a space or nothing reads every date key.
const run = gm => {
  const dpat = {}, bad = [];
  for (let k = 0; k < 864; k++) {
    const o = r.dateOpts(k); if (!o) continue;
    const p = infer({...o, hour: "numeric", minute: "numeric", hourCycle: "h23"}, "h", null);
    let D = null;
    if (Array.isArray(p)) {
      if (!gm.includes("{")) { const suf = gm + ti[0]; if (p[0].endsWith(suf) && p[1] === ti[1]) D = p[0].slice(0, -suf.length); }
      else {
        const i0 = gm.indexOf("{0}"), i1 = gm.indexOf("{1}");
        if (i1 < i0) { const pre = gm.slice(0, i1), mid = gm.slice(i1 + 3, i0); if (p[0].startsWith(pre) && p[0].endsWith(mid + ti[0]) && p[1] === ti[1] + gm.slice(i0 + 3)) D = p[0].slice(pre.length, p[0].length - mid.length - ti[0].length); }
        else { const pre = gm.slice(0, i0), mid = gm.slice(i0 + 3, i1), post = gm.slice(i1 + 3); if (p[0] === pre + ti[0] && p[1].startsWith(ti[1] + mid) && p[1].endsWith(post)) D = p[1].slice(ti[1].length + mid.length, p[1].length - post.length); }
      }
    }
    if (D === null) bad.push([o, "dpat", p]); else dpat[k] = D;
  }
  return {gm, dpat, bad};
};
const best = [gm, " ", ""].map(run).sort((x, y) => x.bad.length - y.bad.length)[0];
out.gm = best.gm; out.dpat = best.dpat; out.bad.push(...best.bad);
fs.writeFileSync(file, JSON.stringify(out));
console.log(LOC, "dpat", Object.keys(out.dpat).length, "bad", out.bad.filter(b => b[1] === "dpat").length, JSON.stringify(out.gm));
for (const b of out.bad.filter(b => b[1] === "dpat").slice(0, 3)) console.log(JSON.stringify(b));
