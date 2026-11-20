// rglue.js LOC : the four glues ICU's interval formatter joins a date pattern and a time pattern with
// (full, long, medium, short by month width), as JSON; rangeemit.js reads them.
const m = require('./infer.js');
const {tmpl} = require('./glue.js');
const T = JSON.parse(require('fs').readFileSync(__dirname + '/t-' + m.LOC + '.json'));
const dkey = f => (((f[0] * 4 + f[1]) * 3 + f[2]) * 6 + f[3]) * 3 + f[4];
const tkey = (f, cy, fsd) => ((((f[6] * 3 + f[7]) * 3 + f[8]) * 7 + cy) * 4 + f[5]) * 4 + fsd;
const t0 = T.time[tkey([0, 0, 0, 0, 0, 0, 1, 1, 0], 4, 0)];
const g = (o, f) => tmpl(m.infer({...o, hour: "numeric", minute: "numeric", hourCycle: "h23"}).p, T.date[dkey(f)], t0);
process.stdout.write(JSON.stringify([g({weekday: "long", month: "long", day: "numeric"}, [1, 0, 0, 3, 1]), g({month: "long", day: "numeric"}, [0, 0, 0, 3, 1]),
  g({month: "short", day: "numeric"}, [0, 0, 0, 4, 1]), g({month: "numeric", day: "numeric"}, [0, 0, 0, 1, 1])]));

