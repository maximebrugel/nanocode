// Generates crates/djs-core/src/builtins/intl/locdata.rs (Intl.Locale's data and
// Intl.supportedValuesOf's lists) from node 22's ICU (78, CLDR 48):
//   node gen.js CLDR > ../../crates/djs-core/src/builtins/intl/locdata.rs
// CLDR is a directory holding cldr-json 48.0.0's cldr-core supplemental likelySubtags.json,
// aliases.json, territoryInfo.json and availableLocales.json, and cldr-bcp47's bcp47/*.json
// (fetch.sh fetches them). CLDR gives the keys (which tags, aliases and regions exist); every
// value is node's: maximize() of each likely-subtags key, the canonical form of each
// u-extension type, and the locale info getters of each region (as zxx-R, a language without
// data) and of each available locale where its region's would be wrong.
// Tables are lines "key value" sorted by key, binary searched over the bytes (locale.rs).
const fs = require("fs");
const path = require("path");
const DIR = process.argv[2];
const J = (f) => JSON.parse(fs.readFileSync(path.join(DIR, f), "utf8"));
const esc = (s) => '"' + [...s].map((c) => { const u = c.codePointAt(0); return c === '"' ? '\\"' : c === "\\" ? "\\\\" : c === "\n" ? "\\n" : u < 0x20 || u > 0x7e ? "\\u{" + u.toString(16) + "}" : c; }).join("") + '"';
const out = [];
const table = (name, doc, rows) => {
  rows = [...new Map(rows).entries()].sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  out.push("/// " + doc);
  out.push("pub(super) const " + name + ": &str = concat!(");
  for (const [k, v] of rows) out.push("    " + esc(k + " " + v + "\n") + ",");
  out.push(");");
};
const list = (name, doc, items) => {
  out.push("/// " + doc);
  out.push("pub(super) const " + name + ": &str = " + esc(items.join(",")) + ";");
};

// ---- likely subtags ----------------------------------------------------------------------
const likelyKeys = Object.keys(J("likelySubtags.json").supplemental.likelySubtags);
const likely = [];
for (const k of likelyKeys) {
  let v;
  try { v = new Intl.Locale(k).maximize().toString(); } catch { continue; }
  const lang = k.split("-")[0];
  // "~" stands for the key's own language.
  likely.push([k, v.startsWith(lang + "-") && lang !== "und" ? "~" + v.slice(lang.length) : v]);
}
// und-tags whose minimize() node leaves as they are (ICU's minimize keeps und there).
const keep = [];
for (const k of likelyKeys.filter((k) => k.startsWith("und-"))) {
  const l = new Intl.Locale(k);
  if (l.minimize().toString() === k) keep.push(["und-" + (l.script || "") + "-" + (l.region || ""), "1"]);
}
table("MIN_KEEP", "und-S-R (S or R may be empty) whose minimize() is itself.", keep);
table("LIKELY", "Likely subtags: a key (language, und-Script, und-Region, ...) to node's maximize() of it (~ is the key's language).", likely);

// ---- aliases: CLDR's keys, node's (ICU 78's) replacements -----------------------------------
// ICU's alias data is not CLDR 48's JSON (tw, bh, sgn-NO): each value is what node does to
// the key, and keys node leaves alone are dropped.
const A = J("aliases.json").supplemental.metadata.alias;
const dash = (s) => s.replace(/_/g, "-");
const canon = (t) => { try { return Intl.getCanonicalLocales(t)[0]; } catch { return null; } };
const langRows = [];
for (const k of Object.keys(A.languageAlias)) {
  const key = dash(k).toLowerCase();
  const c = canon(key);
  if (c && c.toLowerCase() !== key) langRows.push([key, c]);
}
table("LANG_ALIAS", "Language aliases: language[-region|-variants] (lowercase) to node's canonical form of it.", langRows);
const regionRows = [];
for (const [k, v] of Object.entries(A.territoryAlias)) {
  const c = canon("und-" + k);
  if (!c || c === "und-" + k) continue;
  const def = new Intl.Locale(c).region;
  const opts = v._replacement.split(" ");
  regionRows.push([k, [def, ...opts.filter((o) => o !== def)].join(" ")]);
}
table("REGION_ALIAS", "Region aliases: a region to its replacements (the first is node's default; the others by likely region).", regionRows);
const sub = (prefix, keys, pick) => keys.map((k) => { const c = canon(prefix + k); return c && c !== prefix + k ? [k, pick(c)] : null; }).filter(Boolean);
table("SCRIPT_ALIAS", "Script aliases.", sub("und-", Object.keys(A.scriptAlias), (c) => new Intl.Locale(c).script));
table("VARIANT_ALIAS", "Variant aliases.", sub("und-", Object.keys(A.variantAlias), (c) => c.slice(4)));

// ---- extension types: node's canonical form of each, where it differs --------------------
const U = [];
const bcp = fs.readdirSync(path.join(DIR, "bcp47")).filter((f) => f.endsWith(".json"));
const valid = (t) => /^[a-z0-9]{3,8}(-[a-z0-9]{3,8})*$/.test(t);
for (const f of bcp) {
  const kw = J(path.join("bcp47", f)).keyword;
  for (const ext of ["u", "t"]) {
    if (!kw || !kw[ext]) continue;
    for (const [key, types] of Object.entries(kw[ext])) {
      for (const [type, info] of Object.entries(types)) {
        if (type.startsWith("_")) continue;
        const cands = [type, ...String(info._alias || "").split(" ").filter(Boolean), "yes", "true"];
        if (info._preferred) cands.push(info._preferred);
        for (const t of cands) {
          const lt = t.toLowerCase();
          if (!valid(lt)) continue;
          const tag = "und-" + ext + "-" + key + "-" + lt;
          let c;
          try { c = Intl.getCanonicalLocales(tag)[0]; } catch { continue; }
          if (c !== tag) U.push([ext + "-" + key + "-" + lt, c.slice(("und-" + ext + "-" + key).length + 1) || "-"]);
        }
      }
    }
  }
}
for (const k of Object.keys(A.subdivisionAlias)) for (const key of ["rg", "sd"]) {
  const tag = "und-u-" + key + "-" + k.toLowerCase();
  const c = canon(tag);
  if (c && c !== tag) U.push(["u-" + key + "-" + k.toLowerCase(), c.slice(tag.length - k.length)]);
}
table("EXT_ALIAS", "ext-key-type of a u or t extension to node's canonical type (- for none, as true).", U);

// ---- locale info -------------------------------------------------------------------------
const info = (t) => {
  const l = new Intl.Locale(t);
  const w = l.weekInfo;
  return {
    hc: l.hourCycles.join(","), ca: l.calendars.join(","), wk: w.firstDay + ":" + w.weekend.join(",") + ":" + w.minimalDays,
    nu: l.numberingSystems.join(","), co: l.collations.join(","), tz: l.timeZones ? l.timeZones.join(",") : "",
    dir: l.textInfo.direction,
  };
};
const ti = Object.keys(J("territoryInfo.json").supplemental.territoryInfo);
const regions = new Set([...ti, ...likelyKeys.filter((k) => /^und-([A-Z]{2}|\d{3})$/.test(k)).map((k) => k.slice(4)), "001", "150", "419", "EU", "EZ", "UN", "QO"]);
for (const r of Object.keys(A.territoryAlias)) regions.delete(r);
const R = {};
for (const r of regions) { try { R[r] = info("zxx-" + r); } catch {} }
const def = info("zxx");
for (const f of ["hc", "ca", "wk", "tz"]) {
  const rows = [];
  for (const [r, v] of Object.entries(R)) if (v[f] !== def[f] || f === "tz") rows.push([r, v[f]]);
  table("REGION_" + f.toUpperCase(), "Locale.prototype's " + f + " by region (zxx-R in node); others: " + (f === "tz" ? "none" : def[f]) + ".", rows);
}

// Available locales: the overrides of region data (by the maximized L-S-R), and the locale
// data nu and co (by the tag's own L-S-R, L-S, L-R, L: ICU's resource fallback; und is root).
const avail = [...new Set(Intl.DateTimeFormat.supportedLocalesOf(J("availableLocales.json").availableLocales.full.map((x) => canon(x) || "und")))];
const maxKey = (t) => { const m = new Intl.Locale(t).maximize(); return m.language + "-" + m.script + "-" + m.region; };
const own = (t) => { const l = new Intl.Locale(t); if (l.language === "und" || !l.language) return []; const L = l.language, S = l.script, R = l.region; return [...new Set([[L, S, R], [L, S], [L, R], [L]].map((p) => p.filter(Boolean).join("-")))]; };
const ovr = { hc: new Map(), ca: new Map(), wk: new Map() };
const loc = { nu: new Map(), co: new Map() };
const regionOf = (t) => new Intl.Locale(t).region || new Intl.Locale(t).maximize().region;
const predictRegion = (f, t) => { const k = maxKey(t); if (ovr[f].has(k)) return ovr[f].get(k); const r = regionOf(t); return R[r] ? R[r][f] : def[f]; };
const predictLoc = (f, t) => { for (const c of own(t)) if (loc[f].has(c)) return loc[f].get(c); return def[f]; };
avail.sort((a, b) => a.split("-").length - b.split("-").length || (a < b ? -1 : 1));
const SAMPLE_REGIONS = ["US", "GB", "DE", "FR", "ES", "MX", "BR", "PT", "IT", "NL", "BE", "CH", "AT", "JP", "CN", "TW", "HK", "KR", "RU", "IN", "EG", "SA", "AE", "IR", "IL", "CA", "AU", "ZA", "NG", "419", "001", "150"];
const sample = [...avail];
for (const t of avail) { const l = new Intl.Locale(t); if (!l.region && !l.script) for (const r of SAMPLE_REGIONS) sample.push(t + "-" + r); }
for (const r of SAMPLE_REGIONS) sample.push("und-" + r);
for (const t of sample) {
  let v;
  try { v = info(t); } catch { continue; }
  for (const f of ["hc", "ca", "wk"]) if (predictRegion(f, t) !== v[f]) ovr[f].set(maxKey(t), v[f]);
  for (const f of ["nu", "co"]) if (predictLoc(f, t) !== v[f] && own(t).length) loc[f].set(own(t)[0], v[f]);
}
let miss = 0;
for (const t of sample) { let v; try { v = info(t); } catch { continue; } for (const f of ["hc", "ca", "wk"]) if (predictRegion(f, t) !== v[f]) miss++; for (const f of ["nu", "co"]) if (predictLoc(f, t) !== v[f]) miss++; }
process.stderr.write("locale info: " + sample.length + " sampled locales, " + miss + " mispredicted\n");
for (const f of ["hc", "ca", "wk"]) table("LOCALE_" + f.toUpperCase(), "Locales (maximized L-S-R) whose " + f + " is not their region's.", [...ovr[f]]);
for (const f of ["nu", "co"]) table("LOCALE_" + f.toUpperCase(), "Locale data " + f + " by the tag's L-S-R, L-S, L-R, L; others (and und): " + def[f] + ".", [...loc[f]]);

// Right-to-left scripts (textInfo, by the likely script).
const scripts = likelyKeys.filter((k) => /^und-[A-Z][a-z]{3}$/.test(k)).map((k) => k.slice(4));
list("RTL_SCRIPTS", "The scripts whose textInfo.direction is rtl.", scripts.filter((s) => { try { return info("zxx-" + s).dir === "rtl"; } catch { return false; } }).sort());

// ---- Intl.supportedValuesOf -------------------------------------------------------------------
for (const k of ["calendar", "collation", "currency", "numberingSystem", "timeZone", "unit"]) {
  list("SUPPORTED_" + k.replace(/[A-Z]/g, (c) => "_" + c).toUpperCase(), "Intl.supportedValuesOf(\"" + k + "\") (node 22).", Intl.supportedValuesOf(k));
}

process.stdout.write(
  "// Made by crates/patches/djs/scripts/intl-loc/gen.js from node " + process.version + " (ICU " + process.versions.icu + ", CLDR " + process.versions.cldr + "): do not edit.\n" +
  "#![cfg_attr(rustfmt, rustfmt::skip)]\n#![allow(clippy::useless_concat)]\n\n" + out.join("\n") + "\n");

