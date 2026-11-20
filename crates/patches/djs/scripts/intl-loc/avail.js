// Writes crates/djs-core/src/builtins/intl/locdata_avail.rs: the languages node's ICU has
// locale data for, per Intl service (what supportedLocalesOf accepts).
//   node scripts/intl-loc/avail.js   (with node v22.23.5)
const fs = require("fs");
const path = require("path");
const kinds = ["NumberFormat", "DateTimeFormat", "Collator", "PluralRules", "ListFormat", "RelativeTimeFormat", "DisplayNames", "Segmenter"];
const a = "abcdefghijklmnopqrstuvwxyz";
const langs = [];
for (const x of a) for (const y of a) langs.push(x + y);
for (const x of a) for (const y of a) for (const z of a) langs.push(x + y + z);
const bits = new Map();
kinds.forEach((k, b) => {
  for (const l of Intl[k].supportedLocalesOf(langs)) bits.set(l, (bits.get(l) ?? 0) | (1 << b));
});
const rows = [...bits].sort((x, y) => (x[0] < y[0] ? -1 : 1));
let s = "// Made by crates/patches/djs/scripts/intl-loc/avail.js from node " + process.version + " (ICU " + process.versions.icu + "): do not edit.\n";
s += "#![cfg_attr(rustfmt, rustfmt::skip)]\n\n";
s += "/// The languages ICU has locale data for, with a bit per service: NumberFormat 0,\n/// DateTimeFormat 1, Collator 2, PluralRules 3, ListFormat 4, RelativeTimeFormat 5,\n/// DisplayNames 6, Segmenter 7. Sorted.\n";
s += "pub(super) const AVAILABLE: [(&str, u8); " + rows.length + "] = [\n";
for (const [l, m] of rows) s += '    ("' + l + '", 0x' + m.toString(16).toUpperCase().padStart(2, "0") + "),\n";
s += "];\n";
fs.writeFileSync(path.join(__dirname, "../../crates/djs-core/src/builtins/intl/locdata_avail.rs"), s);
