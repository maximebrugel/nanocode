import fs from "node:fs";
// Probe the reference node's zone names in every DateTimeFormat locale djs has besides en-US, at each
// period probe.mjs found: node probe-loc.mjs PROBE.json OUT.json [shard nshards]
// OUT[zone][locale][period] is [names at its start, names at its last second (a day in for the last
// period), names in the hour before it ends when its end drops the offset (else null)], each
// [short, long, shortGeneric, longGeneric].
// DATE_LOCALES' first tags, in order (each locale's zone names table, DateLocale.zones)
const LOC_RS = new URL("../../crates/djs-core/src/builtins/intl/date/locales.rs", import.meta.url);
const LOCALES = [...fs.readFileSync(LOC_RS, "utf8").matchAll(/tags: &\["([^"]+)"/g)].map(m => m[1]);
const [pin, out, shard = "0", nsh = "1"] = process.argv.slice(2);
const probe = JSON.parse(fs.readFileSync(pin));
const res = {};
Object.keys(probe).filter((_, i) => i % +nsh === +shard).forEach(z => {
  res[z] = {};
  const P = probe[z];
  for (const L of LOCALES) {
    const fn = ["short", "long", "shortGeneric", "longGeneric"].map(k => new Intl.DateTimeFormat(L, { timeZone: z, timeZoneName: k, year: "numeric" }));
    const at = t => fn.map(f => f.formatToParts(t * 1000).find(x => x.type === "timeZoneName").value);
    res[z][L] = P.map((p, j) => {
      const n = P[j + 1];
      const drop = n && n[1] < p[1] ? Math.max(p[0], n[0] - (p[1] - n[1]) + 1) : null;
      return [at(p[0]), at(n ? n[0] - 1 : p[0] + 86400), drop === null ? null : at(drop)];
    });
  }
});
fs.writeFileSync(out, JSON.stringify(res));
