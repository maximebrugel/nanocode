// numsys.js: the override numberings ICU's calendar patterns use (d=hanidays, y=hebr ...), marked in djs
// patterns by a control character before the field.
const MARK = {"\u0001": "hebr", "\u0002": "hanidec", "\u0003": "hanidays", "\u0004": "jpanyear"};
const HANI = "〇一二三四五六七八九";
function hebr(n) { // ICU's %hebrew rule set for 1..999, with geresh / gershayim
  const H = ["", "ק", "ר", "ש", "ת", "תק", "תר", "תש", "תת", "תתק"], T = ["", "י", "כ", "ל", "מ", "נ", "ס", "ע", "פ", "צ"], O = ["", "א", "ב", "ג", "ד", "ה", "ו", "ז", "ח", "ט"];
  const h = Math.floor(n / 100), t = Math.floor(n / 10) % 10, o = n % 10;
  let s = H[h];
  if (t === 1 && (o === 5 || o === 6)) s += "ט" + (o === 5 ? "ו" : "ז");
  else if (n % 100 === 44 && h === 7) s += "דמ"; else if (n % 100 === 98 && h === 6) s += "חצ"; else s += T[t] + O[o];
  const a = [...s];
  if (a.length === 1) return s + "׳";
  // ICU's rule set: only a final pe takes its final form, and 744 and 698 reorder their last letters
  const last = a[a.length - 1] === "פ" ? "ף" : a[a.length - 1];
  return a.slice(0, -1).join("") + "״" + last;
}
function fmt(sys, v, field) {
  if (sys === "hebr") return hebr(field === "y" && v > 5000 && v < 6000 ? v - 5000 : v);
  if (sys === "jpanyear") return v === 1 ? "\u5143" : String(v);
  if (sys === "hanidec") return String(v).replace(/\d/g, c => HANI[+c]);
  if (sys === "hanidays") { // 初一..初十 十一..二十 廿一..廿九 三十
    if (v <= 10) return "初" + (v === 10 ? "十" : HANI[v]);
    if (v < 20) return "十" + HANI[v - 10];
    if (v === 20) return "二十";
    if (v < 30) return "廿" + HANI[v - 20];
    return "三十";
  }
  return String(v);
}
// the numberings a calendar's patterns may use, by field
const FOR = {hebrew: {y: ["\u0001"], d: ["\u0001"]}, japanese: {y: ["\u0004"], d: []}, chinese: {y: ["\u0002"], d: ["\u0002", "\u0003"]}, dangi: {y: ["\u0002"], d: ["\u0002", "\u0003"]}};
module.exports = {MARK, FOR, fmt, hebr};
