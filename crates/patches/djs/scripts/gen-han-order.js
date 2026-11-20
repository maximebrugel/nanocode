// gen-han-order.js: Han reading/stroke order tables for Intl.Collator (zh pinyin, zh stroke, ja),
// from the ICU in the Node that is the parity reference (v22.23.5).
//   node scripts/gen-han-order.js crates/djs-core/src/builtins/intl
// Each locale's collation orders some Han (its tailored ones) first, then the rest in the root's
// radical-stroke order. han-<name>.bin lists the tailored ones' ranks: a u16 flag (1: a dense
// u16 rank for each of U+4E00..U+9FFF follows, 0xFFFF for untailored), a u16 count of tailored
// code points, then (code point: 3 bytes, rank: 2 bytes) entries by code point for the rest.
const fs = require("fs");
const out = process.argv[2];
const ranges = [[0x3400, 0x4DBF], [0x4E00, 0x9FFF], [0xF900, 0xFAFF], [0x20000, 0x323AF]];
const han = [];
for (const [a, b] of ranges) for (let c = a; c <= b; c++) if (/\p{Unified_Ideograph}/u.test(String.fromCodePoint(c))) han.push(String.fromCodePoint(c));
const root = new Intl.Collator("und");
const rootPos = new Map([...han].sort(root.compare).map((s, i) => [s, i]));
for (const [name, loc] of [["zh", "zh"], ["zh-stroke", "zh-Hant"], ["zh-zhuyin", "zh-u-co-zhuyin"], ["ja", "ja"]]) {
  const col = new Intl.Collator(loc);
  const s = [...han].sort(col.compare);
  for (let i = 1; i < s.length; i++) if (col.compare(s[i - 1], s[i]) === 0) throw new Error(loc + ": tie");
  // the untailored tail: the longest suffix in root order
  let k = s.length - 1;
  while (k > 0 && rootPos.get(s[k - 1]) < rootPos.get(s[k])) k--;
  const tailored = s.slice(0, k).map(x => x.codePointAt(0));
  if (tailored.length >= 0xFFFF) throw new Error("too many");
  const rank = new Map(tailored.map((c, i) => [c, i]));
  const inU = tailored.filter(c => c >= 0x4E00 && c <= 0x9FFF).length;
  const dense = inU * 4 > 0x5200 * 2;
  const parts = [Buffer.from(new Uint16Array([dense ? 1 : 0, tailored.length]).buffer)];
  if (dense) {
    const d = new Uint16Array(0x5200).fill(0xFFFF);
    for (const [c, r] of rank) if (c >= 0x4E00 && c <= 0x9FFF) d[c - 0x4E00] = r;
    parts.push(Buffer.from(d.buffer));
  }
  const rest = [...rank].filter(([c]) => !(dense && c >= 0x4E00 && c <= 0x9FFF)).sort((a, b) => a[0] - b[0]);
  const e = Buffer.alloc(rest.length * 5);
  rest.forEach(([c, r], i) => { e.writeUIntLE(c, i * 5, 3); e.writeUInt16LE(r, i * 5 + 3); });
  parts.push(e);
  const buf = Buffer.concat(parts);
  fs.writeFileSync(out + "/han-" + name + ".bin", buf);
  console.log(name, "tailored", tailored.length, "dense", dense, "bytes", buf.length);
}

// ko: Han sort with their Hangul reading, a secondary difference after the syllable
// (&가<<伽<<佳…). han-ko.bin ranks the Han that have a reading (in ko order) like the tables above;
// han-ko-syl.bin gives each rank its syllable (u16, from U+AC00) then its place among that
// syllable's Han (u8).
{
  const col = new Intl.Collator("ko"), base = new Intl.Collator("ko", {sensitivity: "base"});
  const syl = [];
  for (let x = 0xAC00; x <= 0xD7A3; x++) syl.push(String.fromCodePoint(x));
  const reading = h => {
    let lo = 0, hi = syl.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1, c = base.compare(h, syl[mid]);
      if (c === 0) return mid;
      if (c < 0) hi = mid; else lo = mid + 1;
    }
    return -1;
  };
  const read = new Map();
  for (const h of han) { const r = reading(h); if (r >= 0) read.set(h, r); }
  const tailored = [...read.keys()].sort(col.compare);
  const rank = new Map(tailored.map((h, i) => [h.codePointAt(0), i]));
  const inU = [...rank.keys()].filter(c => c >= 0x4E00 && c <= 0x9FFF).length;
  const dense = inU * 4 > 0x5200 * 2;
  const parts = [Buffer.from(new Uint16Array([dense ? 1 : 0, tailored.length]).buffer)];
  if (dense) {
    const d = new Uint16Array(0x5200).fill(0xFFFF);
    for (const [c, r] of rank) if (c >= 0x4E00 && c <= 0x9FFF) d[c - 0x4E00] = r;
    parts.push(Buffer.from(d.buffer));
  }
  const rest = [...rank].filter(([c]) => !(dense && c >= 0x4E00 && c <= 0x9FFF)).sort((a, b) => a[0] - b[0]);
  const e = Buffer.alloc(rest.length * 5);
  rest.forEach(([c, r], i) => { e.writeUIntLE(c, i * 5, 3); e.writeUInt16LE(r, i * 5 + 3); });
  parts.push(e);
  fs.writeFileSync(out + "/han-ko.bin", Buffer.concat(parts));
  const s = new Uint16Array(tailored.length), k = Buffer.alloc(tailored.length);
  tailored.forEach((h, i) => {
    s[i] = read.get(h);
    k[i] = i > 0 && s[i - 1] === s[i] ? k[i - 1] + 1 : 0;
    if (i > 0 && s[i - 1] > s[i]) throw new Error("ko: readings out of order");
    if (k[i] === 255) throw new Error("ko: too many Han for one reading");
  });
  fs.writeFileSync(out + "/han-ko-syl.bin", Buffer.concat([Buffer.from(s.buffer), k]));
  console.log("ko tailored", tailored.length, "dense", dense);
}
