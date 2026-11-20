// cal.js: calendar arithmetic model for djs Intl.DateTimeFormat (mirrors intl/date/calendar.rs)
// fields(days) -> {era, year, month, leap, day, ry}: era index into the calendar's era table, year in era,
// month 0-based calendar month code (hebrew: ICU month 0..12, Adar II in a leap year = 13; chinese 0..11 with leap flag),
// day 1-based, ry: related gregorian year (chinese/dangi).
const civil = (days) => { const z = days + 719468, era = Math.floor(z / 146097), doe = z - era * 146097;
  const yoe = Math.floor((doe - Math.floor(doe / 1460) + Math.floor(doe / 36524) - Math.floor(doe / 146096)) / 365);
  const doy = doe - (365 * yoe + Math.floor(yoe / 4) - Math.floor(yoe / 100)); const mp = Math.floor((5 * doy + 2) / 153);
  const d = doy - Math.floor((153 * mp + 2) / 5) + 1; const m = mp < 10 ? mp + 3 : mp - 9; return [m <= 2 ? yoe + era * 400 + 1 : yoe + era * 400, m, d]; };
const fromCivil = (y, m, d) => { y = m <= 2 ? y - 1 : y; const era = Math.floor(y / 400), yoe = y - era * 400, mp = (m + 9) % 12;
  const doy = Math.floor((153 * mp + 2) / 5) + d - 1, doe = yoe * 365 + Math.floor(yoe / 4) - Math.floor(yoe / 100) + doy; return era * 146097 + doe - 719468; };
const leapG = y => (y % 4 === 0 && y % 100 !== 0) || y % 400 === 0;
const JD_EPOCH = 2440588; // julian day of 1970-01-01
// japanese eras (ICU, start dates) from Meiji
const JA_ERAS = [[1868, 9, 8], [1912, 7, 30], [1926, 12, 25], [1989, 1, 8], [2019, 5, 1]];
function japanese(days) {
  const [y, m, d] = civil(days);
  let e = JA_ERAS.length - 1; while (e > 0 && days < fromCivil(...JA_ERAS[e])) e--;
  return {era: e, year: y - JA_ERAS[e][0] + 1, month: m - 1, day: d};
}
// islamic civil / tbla: tabular, epoch julian day 1948440 (civil, Friday) / 1948439 (tbla, Thursday)
function islamicTab(days, epoch) {
  const jd = days + JD_EPOCH; const n = jd - epoch;
  const year = Math.floor((30 * n + 10646) / 10631);
  const ystart = epoch + 354 * (year - 1) + Math.floor((3 + 11 * year) / 30);
  let month = Math.ceil((jd - 29 - ystart) / 29.5); month = Math.min(Math.max(month, 0), 11);
  const mstart = ystart + Math.ceil(29.5 * month);
  return {era: 0, year, month, day: jd - mstart + 1};
}
// persian: 33-year arithmetic as ICU's PersianCalendar
const PERSIAN_EPOCH = 1948320; // julian day of 1 Farvardin 1 AP
function persian(days) {
  const jd = days + JD_EPOCH; const dse = jd - PERSIAN_EPOCH; // days since epoch, day 0 = 1/1/1
  const year = 1 + Math.floor((33 * dse + 3) / 12053);
  const farv = 365 * (year - 1) + Math.floor((8 * year + 21) / 33);
  const doy = dse - farv;
  const month = doy < 216 ? Math.floor(doy / 31) : Math.floor((doy - 6) / 30);
  const day = doy - (month < 7 ? month * 31 : month * 30 + 6) + 1;
  return {era: 0, year, month, day};
}
// indian (Saka): ICU IndianCalendar
function indian(days) {
  const [gy, gm, gd] = civil(days);
  let year = gy - 78;
  const start = (y) => fromCivil(y + 78, 3, leapG(y + 78) ? 21 : 22);
  let yday = days - start(year);
  if (yday < 0) { year--; yday = days - start(year); }
  const leap = leapG(year + 78), l1 = leap ? 31 : 30;
  let month, day;
  if (yday < l1) { month = 0; day = yday + 1; }
  else { let md = yday - l1; if (md < 31 * 5) { month = 1 + Math.floor(md / 31); day = md % 31 + 1; } else { md -= 31 * 5; month = 6 + Math.floor(md / 30); day = md % 30 + 1; } }
  return {era: 0, year, month, day};
}
// coptic / ethiopic: 13 months of 30 days + epagomenal, julian-style leap every 4 years
function cop(days, epoch, eraOff) { // ICU CECalendar::jdToCE
  const jd = days + JD_EPOCH;
  const c4 = Math.floor((jd - epoch) / 1461), r4 = (jd - epoch) - c4 * 1461;
  const year = 4 * c4 + Math.floor(r4 / 365) - Math.floor(r4 / 1460);
  const doy = r4 === 1460 ? 365 : r4 % 365;
  return {era: 0, year: year + eraOff, month: Math.floor(doy / 30), day: doy % 30 + 1};
}
// hebrew: ICU HebrewCalendar
const HOUR_PARTS = 1080, DAY_PARTS = 24 * HOUR_PARTS, MONTH_FRACT = 12 * HOUR_PARTS + 793, BAHARAD = 11 * HOUR_PARTS + 204;
const hleap = y => ((12 * y + 17) % 19) >= 12;
function hstart(y) { // days from hebrew epoch (1 Tishri 1 = day 0 offset) to 1 Tishri y
  const months = Math.floor((235 * y - 234) / 19);
  const frac = months * MONTH_FRACT + BAHARAD;
  let day = months * 29 + Math.floor(frac / DAY_PARTS); const parts = frac % DAY_PARTS;
  const wd = day % 7;
  if (wd === 2 || wd === 4 || wd === 6) day++; // the other postponements test the molad weekday
  if (wd === 1 && parts > 15 * HOUR_PARTS + 204 && !hleap(y)) day += 2;
  else if (wd === 0 && parts > 21 * HOUR_PARTS + 589 && hleap(y - 1)) day++;
  return day;
}
const HEBREW_EPOCH = 347998; // julian day of 1 Tishri 1 less hstart(1)
function hebrew(days) {
  const jd = days + JD_EPOCH; const d = jd - HEBREW_EPOCH;
  let y = Math.floor((19 * d) / 6940) + 1; // estimate (ICU style)
  while (hstart(y) > d) y--; while (hstart(y + 1) <= d) y++;
  const len = hstart(y + 1) - hstart(y), leap = hleap(y);
  const type = len % 10 - 3; // 0 deficient (353/383), 1 regular, 2 complete
  // month lengths Tishri..Elul (13 slots, Adar I slot 5 only in leap)
  const ml = [30, type === 2 ? 30 : 29, type === 0 ? 29 : 30, 29, 30, leap ? 30 : 0, 29, 30, 29, 30, 29, 30, 29];
  let r = d - hstart(y), m = 0; while (r >= ml[m]) { r -= ml[m]; m++; }
  return {era: 0, year: y, month: m === 6 && leap ? 13 : m, day: r + 1, leap};
}
// table calendars (derive.js): lunisolar years [new year day, 30-day month mask, leap position, related year]
const T = require('./tables.json');
function lunisolar(tab, days) {
  let i = tab.length - 1; while (i > 0 && tab[i][0] > days) i--;
  const [ny, mask, lp, ry] = tab[i]; let r = days - ny, m = 0;
  while (r >= (mask >> m & 1 ? 30 : 29)) { r -= mask >> m & 1 ? 30 : 29; m++; }
  // month code: regular months 0..11, the leap month after regular month n is 11 + n
  let month, leap = false;
  if (lp && m === lp) { month = 11 + lp; leap = true; } else month = lp && m > lp ? m - 1 : m;
  const cyc = ((ry - 4) % 60 + 60) % 60 + 1;
  return {era: Math.floor((ry + 2636) / 60) + 1, year: cyc, cyc, month, day: r + 1, ry, leap};
}
function hijriTable(tab, days, epoch) {
  if (days < tab[0][1] || days >= tab[tab.length - 1][1] + 354) return islamicTab(days, epoch);
  let i = tab.length - 1; while (i > 0 && tab[i][1] > days) i--;
  const [y, start, mask] = tab[i]; let r = days - start, m = 0;
  while (m < 11 && r >= (mask >> m & 1 ? 30 : 29)) { r -= mask >> m & 1 ? 30 : 29; m++; }
  return {era: 0, year: y, month: m, day: r + 1};
}
const CAL = {
  chinese: d => lunisolar(T.chinese, d), dangi: d => lunisolar(T.dangi, d),
  "islamic-umalqura": d => hijriTable(T["islamic-umalqura"], d, 1948440), islamic: d => hijriTable(T.islamic, d, 1948440),
  "islamic-rgsa": d => hijriTable(T.islamic, d, 1948440),
  iso8601: days => CAL.gregory(days),
  gregory: days => { const [y, m, d] = civil(days); return {era: y > 0 ? 1 : 0, year: y > 0 ? y : 1 - y, month: m - 1, day: d}; },
  buddhist: days => { const [y, m, d] = civil(days); return {era: 0, year: y + 543, month: m - 1, day: d}; },
  roc: days => { const [y, m, d] = civil(days); return y > 1911 ? {era: 1, year: y - 1911, month: m - 1, day: d} : {era: 0, year: 1912 - y, month: m - 1, day: d}; },
  japanese, persian, indian, hebrew,
  "islamic-civil": d => islamicTab(d, 1948440), "islamic-tbla": d => islamicTab(d, 1948439),
  coptic: d => cop(d, 1824665, 0), ethiopic: d => cop(d, 1723856, 0), ethioaa: d => cop(d, 1723856, 5500),
};
module.exports = {CAL, civil, fromCivil, JA_ERAS, hstart, hleap};
