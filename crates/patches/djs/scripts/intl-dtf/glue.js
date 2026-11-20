// glue.js: how a locale joins a date pattern and a time pattern.
// A glue is either the literal between them ("' at '") or, when the time comes first or the
// locale wraps the pair (hi "{1}' को '{0}' बजे'", vi "{0} {1}"), a template: {1} = date, {0} = time.
// The placeholders sit outside quoted literals, so a plain glue never contains them.
const tmpl = (p, dp, tp) => {
  if (p.length >= dp.length + tp.length && p.startsWith(dp) && p.endsWith(tp)) {
    const mid = p.slice(dp.length, p.length - tp.length);
    if (!mid.includes("{")) return mid;
  }
  const i = p.indexOf(dp);
  if (i < 0) return null;
  let j = -1;
  for (let k = p.indexOf(tp); k >= 0; k = p.indexOf(tp, k + 1)) if (k + tp.length <= i || k >= i + dp.length) { j = k; break; }
  if (j < 0) return null;
  return j > i ? p.slice(0, i) + "{1}" + p.slice(i + dp.length, j) + "{0}" + p.slice(j + tp.length)
               : p.slice(0, j) + "{0}" + p.slice(j + tp.length, i) + "{1}" + p.slice(i + dp.length);
};
const apply = (g, d, t) => g.includes("{") ? g.replace("{1}", () => d).replace("{0}", () => t) : d + g + t;
module.exports = {tmpl, apply};
