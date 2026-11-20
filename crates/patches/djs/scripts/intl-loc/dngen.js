// Generates crates/djs-core/src/builtins/intl/dndata.rs (Intl.DisplayNames' names) from
// node 22's ICU: node dngen.js CLDR > ../../crates/djs-core/src/builtins/intl/dndata.rs
// CLDR's en localenames give the codes (CLDR/names/*.json, fetch.sh); every name is node's.
// One table per locale: "<k>:<code> <name>" lines sorted, k is
//   l s r v c k f (language script region variant currency calendar dateTimeField, long),
//   L S R F (short), N M (narrow region, field), d D (dialect long, short), P (pattern).
// Short and narrow lines are kept only where they differ from long.
const fs = require("fs"), path = require("path");
const DIR = process.argv[2];
const J = (f) => JSON.parse(fs.readFileSync(path.join(DIR, f), "utf8"));
const esc = (s) => '"' + [...s].map((c) => { const u = c.codePointAt(0); return c === '"' ? '\\"' : c === "\\" ? "\\\\" : c === "\n" ? "\\n" : u < 0x20 || u > 0x7e ? "\\u{" + u.toString(16) + "}" : c; }).join("") + '"';
const LOCALES = ["en", "de", "fr", "es", "it", "pt", "nl", "ja", "zh", "ko", "ru", "pl", "tr", "ar", "hi", "id", "vi", "th", "sv", "da", "nb", "fi", "cs", "el", "he", "uk", "ro", "hu", "bn", "fa", "ms", "ta", "ca", "sk", "bg", "hr", "sr", "lt", "lv", "et", "sl", "fil", "sw", "ur"];
// Regional variants with their own names, kept as their differences from the language's.
const OVERLAYS = ["zh-Hant", "pt-PT", "en-GB", "es-419", "es-MX", "fr-CA", "de-AT"];
const names = (f, k) => Object.keys(Object.values(J("names/" + f + ".json").main)[0].localeDisplayNames[k]);
const langs = [...new Set(names("languages", "languages").map((k) => k.split("-alt-")[0]))];
const regions = [...new Set(names("territories", "territories").map((k) => k.split("-alt-")[0]))];
const scripts = [...new Set(names("scripts", "scripts").map((k) => k.split("-alt-")[0]))];
const variants = [...new Set(names("variants", "variants").map((k) => k.split("-alt-")[0].toLowerCase()))];
const currencies = Object.keys(Object.values(J("names/currencies.json").main)[0].numbers.currencies);
const caTypes = J("bcp47/calendar.json").keyword.u.ca;
const calendars = [...new Set([...Intl.supportedValuesOf("calendar"), ...Object.entries(caTypes).filter(([k]) => !k.startsWith("_")).flatMap(([k, v]) => [k, ...String(v._alias || "").split(" ").filter(Boolean)]), "islamicc"])];
const fields = ["era", "year", "quarter", "month", "weekOfYear", "weekday", "day", "dayPeriod", "hour", "minute", "second", "timeZoneName"];
const simple = langs.filter((k) => !k.includes("-"));
const dialects = langs.filter((k) => k.includes("-"));
const out = [];
const base = {};
for (const loc of [...LOCALES, ...OVERLAYS]) {
  const rows = new Map();
  const of = (type, style, code, extra = {}) => { try { return new Intl.DisplayNames(loc, { type, style, fallback: "none", ...extra }).of(code); } catch { return undefined; } };
  const put = (k, code, v) => { if (v !== undefined) rows.set(k + ":" + code, v); };
  const three = (type, k, codes, short, narrow) => {
    for (const c of codes) {
      const l = of(type, "long", c); put(k, c, l);
      if (short) { const s = of(type, "short", c); if (s !== l) put(short, c, s); }
      if (narrow) { const n = of(type, "narrow", c); const s = of(type, "short", c); if (n !== s) put(narrow, c, n); }
    }
  };
  three("language", "l", simple.filter((c) => { try { return Intl.getCanonicalLocales(c)[0] === c; } catch { return false; } }), "L");
  three("script", "s", scripts, "S");
  three("region", "r", regions, "R", "N");
  three("currency", "c", currencies);
  three("calendar", "k", calendars);
  three("dateTimeField", "f", fields, "F", "M");
  // Dialect names: a language-script, language-region (or both) with its own name.
  for (const c of dialects) {
    let t; try { t = Intl.getCanonicalLocales(c)[0]; } catch { continue; }
    const d = of("language", "long", t), s = of("language", "long", t, { languageDisplay: "standard" });
    if (d !== undefined && d !== s) put("d", t, d);
    const ds = of("language", "short", t);
    if (ds !== undefined && ds !== d) put("D", t, ds);
  }
  // Variants, from their composition with an unnamed language.
  for (const v of variants) {
    const full = of("language", "long", "xyz-" + v);
    const bare = new Intl.DisplayNames(loc, { type: "language" }).of("xyz-" + v);
    if (bare && full !== undefined) {} // unreachable: xyz has no name
    const m = /^xyz(.)(.*)$/su.exec(bare || "");
    if (bare && bare !== "xyz-" + v) rows.set("v:" + v, bare);
  }
  // The pattern: xyz (Script, Region) gives open, separator, close.
  const sN = of("script", "long", "Latn"), rN = of("region", "long", "TW");
  const full = new Intl.DisplayNames(loc, { type: "language" }).of("xyz-Latn-TW");
  const a = full.indexOf(sN), b = full.indexOf(rN);
  rows.set("P:", full.slice(3, a) + "\u0001" + full.slice(a + sN.length, b) + "\u0001" + full.slice(b + rN.length));
  // Variant lines hold the whole composition "xyz (Name)": keep only the name.
  const [open, , close] = rows.get("P:").split("\u0001");
  for (const [k, v] of rows) if (k.startsWith("v:")) { const n = v.slice(3 + open.length, v.length - close.length); n === k.slice(2).toUpperCase() ? rows.delete(k) : rows.set(k, n); }
  const lang = loc.split("-")[0];
  const rowsAll = [...rows];
  // An overlay drops the lines the language's table has the same, and marks the ones it
  // lacks with "\u0000".
  if (loc !== lang) {
    for (const [k, v] of [...rows]) if (base[lang].get(k) === v) rows.delete(k);
    for (const k of base[lang].keys()) if (!rows.has(k) && new Map(rowsAll).get(k) === undefined) rows.set(k, "\u0000");
  }
  if (loc === lang) base[lang] = new Map(rows);
  const sorted0 = [...rows.entries()].sort((x, y) => (x[0] < y[0] ? -1 : x[0] > y[0] ? 1 : 0));
  const sorted = sorted0;
  out.push("/// " + loc + "'s names.");
  out.push("pub(super) const " + loc.toUpperCase().replace("-", "_") + ": &str = concat!(");
  for (const [k, v] of sorted) out.push("    " + esc(k + " " + v + "\n") + ",");
  out.push(");");
}
out.push("/// The languages with their own names.");
out.push("pub(super) const LANGS: [&str; " + LOCALES.length + "] = [" + LOCALES.map((l) => JSON.stringify(l)).join(", ") + "];");
out.push("/// The overlay (or \"\") and the language's table for a resolved locale.");
out.push("pub(super) fn tables(locale: &str) -> (&'static str, &'static str) {");
out.push("    let mut p = locale.split('-');");
out.push("    let lang = p.next().unwrap_or(\"en\");");
out.push("    let rest: alloc::vec::Vec<&str> = p.collect();");
out.push("    let script = rest.iter().find(|s| s.len() == 4).copied().unwrap_or(\"\");");
out.push("    let region = rest.iter().find(|s| s.len() == 2 || s.len() == 3).copied().unwrap_or(\"\");");
out.push("    let base = match lang {");
for (const l of LOCALES.slice(1)) out.push("        " + JSON.stringify(l) + " => " + l.toUpperCase() + ",");
out.push("        _ => EN,");
out.push("    };");
out.push("    let overlay = match (lang, script, region) {");
out.push("        (\"zh\", \"Hant\", _) | (\"zh\", \"\", \"TW\" | \"HK\" | \"MO\") => ZH_HANT,");
out.push("        (\"es\", _, \"MX\") => ES_MX,");
out.push("        (\"de\", _, \"AT\") => DE_AT,");
out.push("        (\"pt\", _, r) if !r.is_empty() && r != \"BR\" => PT_PT,");
out.push("        (\"en\", _, r) if !r.is_empty() && !matches!(r, \"US\" | \"AS\" | \"GU\" | \"MH\" | \"MP\" | \"PR\" | \"UM\" | \"VI\") => EN_GB,");
out.push("        (\"es\", _, r) if !r.is_empty() && !matches!(r, \"ES\" | \"EA\" | \"IC\" | \"GQ\" | \"PH\") => ES_419,");
out.push("        (\"fr\", _, \"CA\") => FR_CA,");
out.push("        _ => \"\",");
out.push("    };");
out.push("    (overlay, base)");
out.push("}");
process.stdout.write("// Made by crates/patches/djs/scripts/intl-loc/dngen.js from node " + process.version + " (ICU " + process.versions.icu + ", CLDR " + process.versions.cldr + "): do not edit.\n#![cfg_attr(rustfmt, rustfmt::skip)]\n\n" + out.join("\n") + "\n");
