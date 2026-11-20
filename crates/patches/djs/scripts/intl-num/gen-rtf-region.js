// Rows of RELATIVE and RELATIVE_AUTO for a regional locale whose RelativeTimeFormat text has its
// own data in ICU (zh-HK: Traditional, Hong Kong wording). Run with node 22:
//   node gen-rtf-region.js zh-HK
const L = process.argv[2];
const UNITS = ["second", "minute", "hour", "day", "week", "month", "quarter", "year"];
const STYLES = ["long", "short", "narrow"];
const esc = (s) => '"' + [...s].map((c) => { const u = c.codePointAt(0); return c === '"' ? '\\"' : c === "\\" ? "\\\\" : u < 0x20 || u > 0x7e ? "\\u{" + u.toString(16) + "}" : c; }).join("") + '"';
const rel = [], auto = [];
STYLES.forEach((st, si) => UNITS.forEach((u, ui) => {
  const f = new Intl.RelativeTimeFormat(L, { style: st });
  for (const dir of [0, 1]) {
    const p = f.formatToParts(dir ? -3 : 3, u).map((x) => (x.type === "literal" ? x.value : "{0}")).join("");
    rel.push("    (" + [esc(L), ui, si, dir, '"other"', esc(p)].join(", ") + "),");
  }
  const a = new Intl.RelativeTimeFormat(L, { style: st, numeric: "auto" });
  for (let v = -3; v <= 3; v++) {
    const s = a.format(v, u);
    if (!/\d/.test(s)) auto.push("    (" + [esc(L), ui, si, v, esc(s)].join(", ") + "),");
  }
}));
console.log(rel.join("\n"));
console.log("//AUTO");
console.log(auto.join("\n"));
