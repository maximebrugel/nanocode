// NumLocale rows (data.rs) for locales, derived from node 22's ICU:
//   node gen-numloc.js de-CH it-CH ...
// Patterns: # is the number, ¤ the currency symbol (EUR in its narrow form, €, which needs no currency spacing).
const esc = (s) => '"' + [...s].map((c) => { const u = c.codePointAt(0); return c === '"' ? '\\"' : c === "\\" ? "\\\\" : u < 0x20 || u > 0x7e ? "\\u{" + u.toString(16) + "}" : c; }).join("") + '"';
const NUM = new Set(["integer", "group", "decimal", "fraction"]);
function pat(parts, mark) {
  let out = "", inNum = false;
  for (const p of parts) {
    if (NUM.has(p.type)) { if (!inNum) out += "#"; inNum = true; continue; }
    inNum = false;
    if (p.type === "minusSign" && mark && out.endsWith(mark)) out = out.slice(0, -mark.length);
    out += p.type === "currency" ? "\u00a4" : p.type === "minusSign" ? "-" : p.type === "percentSign" ? "%" : p.value;
  }
  return out;
}
// en-CH is patched by hand after: its EUR amounts follow en (-€1,234.50) while every other
// currency puts the sign after the symbol (CHF-1'234.50), which its row keeps (neg_cur, acct "¤-#").
for (const L of process.argv.slice(2)) {
  const nf = (o) => new Intl.NumberFormat(L, o);
  const p = nf({}).formatToParts(1234567.5);
  const dec = p.find((x) => x.type === "decimal").value, grp = p.find((x) => x.type === "group").value;
  const min2 = !nf({}).formatToParts(1234).some((x) => x.type === "group");
  // read in Latin digits: bn writes its own
  const indian = nf({ numberingSystem: "latn" }).format(1234567).startsWith("12,");
  const m = nf({}).formatToParts(-1);
  const minus = m.slice(0, m.findIndex((x) => x.type === "integer")).map((x) => x.value).join("");
  const mark = minus.slice(0, -1);
  const cur = (v, o) => pat(nf({ style: "currency", currency: "EUR", currencyDisplay: "narrowSymbol", ...o }).formatToParts(v), mark);
  const pct = (v) => pat(nf({ style: "percent" }).formatToParts(v), mark);
  const nan = nf({}).format(NaN);
  const f = [["tag", esc(L)], ["dec", esc(dec)], ["grp", esc(grp)], ["min2", min2], ["indian", indian], ["minus", esc(minus)],
    ["cur", esc(cur(1))], ["neg_cur", esc(cur(-1))], ["acct", esc(cur(-1, { currencySign: "accounting" }))], ["pct", esc(pct(1))], ["neg_pct", esc(pct(-1))], ["nan", esc(nan)]];
  console.log("    NumLocale { " + f.map(([k, v]) => k + ": " + v).join(", ") + " },");
}

