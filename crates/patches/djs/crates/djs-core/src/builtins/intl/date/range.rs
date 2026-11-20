//! Intl.DateTimeFormat.prototype.formatRange and formatRangeToParts, as V8 drives ICU's
//! DateIntervalFormat: find the greatest field that differs between the two dates, take the
//! locale's interval pattern for the format's skeleton at that field (or ICU's fallback, both
//! dates in one pattern joined by the range separator), and format each half; when no field the
//! pattern shows differs, V8 formats the start alone. range_data.rs holds the tables
//! (scripts/intl-dtf/rangeemit.js); scripts/intl-dtf/rangemodel.js is the same algorithm in JS.

use super::range_data::{RANGE_LOCALES, RANGE_PAIRS, RANGE_PATTERNS_ENDS, RANGE_PATTERNS_TEXT, RANGE_ROWS};
use super::{DATE_LOCALES, Spec, civil, format_pattern, zone_at};
use crate::interp::conv::Fault;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// One locale's range tables: the fallback separator, the four glues joining a date pattern and a
/// time pattern (full, long, medium, short by month width) and its cells in rows of RANGE_ROWS.
pub(super) struct RangeLocale {
    pub sep: &'static str,
    pub glue: [&'static str; 4],
    /// The glue ICU joins its date pattern to a time interval pattern with.
    pub gm: &'static str,
    pub rows: &'static [u16],
}

const NONE: u16 = u16::MAX;
const SAME: u16 = u16::MAX - 1;
// Row offsets of each table in RangeLocale::rows: ICU date patterns (864 date keys), ICU time
// patterns (7 zone kinds x 3024 time keys), date interval entries (864 x levels G y M d), time
// interval entries (7 zone kinds x 252 time keys x levels G d a h m), style entries, then TPF, DPN.
const DP: usize = 0;
const TP: usize = DP + 864 / 16;
const DATE: usize = TP + 7 * 3024 / 16;
const TIME: usize = DATE + 864 * 4 / 16;
const STYLE: usize = TIME + (7 * 252 * 5usize).div_ceil(16);
// ICU's time pattern for the whole skeleton (7 x 3024), which fallbacks use, and its date pattern
// from the normalized skeleton (864), which a fallback time range is joined to.
const TPF: usize = STYLE + (16 + 224 + 896usize).div_ceil(16);
const DPN: usize = TPF + 7 * 3024 / 16;
// Date interval entries with a zone name (6 kinds x 864 x levels G y M d): ICU leaves an offset
// zone (O) out of the interval skeleton, so these are read as node shows them.
const DZ: usize = DPN + 864 / 16;

/// What the resolved options select: fields (weekday era year month day dayPeriod hour minute
/// second, encoded as resolve reads them), fractional digits, hour cycle choice (0 to 6), zone name
/// kind (0 none, 1 to 6) and the date and time styles.
#[derive(Clone, Copy)]
pub(super) struct Key {
    pub f: [usize; 9],
    pub fsd: usize,
    pub cy: usize,
    pub z: usize,
    pub ds: Option<usize>,
    pub ts: Option<usize>,
}

impl Key {
    pub(super) fn encode(&self) -> String {
        let mut s = String::new();
        for v in self.f.iter().chain([self.fsd, self.cy, self.z, self.ds.unwrap_or(9), self.ts.unwrap_or(9)].iter()) {
            s.push(char::from(b'0' + *v as u8));
        }
        s
    }
    fn decode(s: &str) -> Option<Key> {
        let b = s.as_bytes();
        if b.len() != 14 {
            return None;
        }
        let d = |i: usize| usize::from(b[i] - b'0');
        let mut f = [0; 9];
        for (i, v) in f.iter_mut().enumerate() {
            *v = d(i);
        }
        let opt = |v: usize| (v != 9).then_some(v);
        Some(Key { f, fsd: d(9), cy: d(10), z: d(11), ds: opt(d(12)), ts: opt(d(13)) })
    }
}

fn cell(l: &RangeLocale, row: usize, i: usize) -> u16 {
    RANGE_ROWS[usize::from(l.rows[row + i / 16])][i % 16]
}

fn pattern(i: u16) -> &'static str {
    let i = usize::from(i);
    let start = if i == 0 { 0 } else { RANGE_PATTERNS_ENDS[i - 1] as usize };
    &RANGE_PATTERNS_TEXT[start..RANGE_PATTERNS_ENDS[i] as usize]
}

#[derive(Clone, PartialEq, Debug)]
enum Entry {
    /// No field the pattern shows differs: the start alone.
    Same,
    Pair(String, String),
}

fn entry_of(c: u16) -> Option<Entry> {
    match c {
        NONE => None,
        SAME => Some(Entry::Same),
        _ => {
            let (a, b) = RANGE_PAIRS[usize::from(c)];
            Some(Entry::Pair(pattern(a).into(), pattern(b).into()))
        }
    }
}

/// The calendar unit a pattern letter shows, coarsest first (ICU's field levels).
fn unit(c: char) -> Option<u8> {
    Some(match c {
        'G' => 0,
        'y' | 'u' | 'U' => 10,
        'M' | 'L' => 20,
        'd' | 'E' | 'c' => 30,
        // ICU gives flexible day periods (b, B) no level: a range never differs by them alone.
        'a' => 40,
        'h' | 'H' | 'k' | 'K' => 50,
        'm' => 60,
        's' => 70,
        'S' => 80,
        _ => return None,
    })
}

const LEVEL_UNIT: [u8; 9] = [0, 10, 20, 30, 40, 50, 60, 70, 80];

/// A pattern's tokens: runs of one letter, quoted literals (with their quotes) and other text.
fn tokens(p: &str) -> Vec<&str> {
    let b = p.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let c = b[i];
        if c == b'\'' {
            i += 1;
            while i < b.len() {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
        } else if c.is_ascii_alphabetic() {
            while i < b.len() && b[i] == c {
                i += 1;
            }
        } else {
            while i < b.len() && b[i] != b'\'' && !b[i].is_ascii_alphabetic() {
                i += 1;
            }
        }
        out.push(&p[start..i]);
    }
    out
}

fn letter(t: &str) -> Option<char> {
    t.chars().next().filter(|c| c.is_ascii_alphabetic())
}

/// ICU's isFieldUnitIgnored: the pattern shows nothing at the level's unit or finer.
fn ignored(level: usize, p: &str) -> bool {
    !tokens(p).iter().any(|t| letter(t).and_then(unit).is_some_and(|u| u >= LEVEL_UNIT[level]))
}

fn quote(s: &str) -> String {
    if s.bytes().any(|b| b.is_ascii_alphabetic() || b == b'\'') { format!("'{}'", s.replace('\'', "''")) } else { s.into() }
}

fn fallback(p: &str, sep: &str) -> Entry {
    Entry::Pair(format!("{p}{}", quote(sep)), p.into())
}

fn is_fallback(e: &Entry, sep: &str) -> bool {
    matches!(e, Entry::Pair(a, b) if *a == format!("{b}{}", quote(sep)))
}

fn apply(g: &str, d: &str, t: &str) -> String {
    if g.contains('{') { g.replacen("{1}", d, 1).replacen("{0}", t, 1) } else { format!("{d}{g}{t}") }
}

/// A date pattern joined to a time interval pattern by a glue.
fn glue_date(g: &str, d: &str, a: &str, b: &str) -> Entry {
    if !g.contains('{') {
        return Entry::Pair(format!("{d}{g}{a}"), b.into());
    }
    let (i0, i1) = (g.find("{0}").unwrap_or(0), g.find("{1}").unwrap_or(0));
    if i1 < i0 {
        Entry::Pair(format!("{}{a}", g[..i0].replacen("{1}", d, 1)), format!("{b}{}", &g[i0 + 3..]))
    } else {
        Entry::Pair(format!("{}{a}", &g[..i0]), format!("{b}{}", g[i0 + 3..].replacen("{1}", d, 1)))
    }
}

fn dkey(f: &[usize]) -> usize {
    (((f[0] * 4 + f[1]) * 3 + f[2]) * 6 + f[3]) * 3 + f[4]
}

/// The interval pattern for the key at a level (0 era to 8 millisecond), from the tables.
fn entry(l: &RangeLocale, k: &Key, single: &str, level: usize) -> Entry {
    let sep = l.sep;
    let f = &k.f;
    let get = |row: usize, i: usize| entry_of(cell(l, row, i)).unwrap_or(Entry::Same);
    if k.ds.is_some() || k.ts.is_some() {
        if level == 8 {
            return Entry::Same;
        }
        return match (k.ds, k.ts) {
            (Some(d), None) => if level < 4 { get(STYLE, d * 4 + level) } else { Entry::Same },
            (None, Some(t)) => get(STYLE, 16 + (k.cy * 4 + t) * 8 + level),
            (Some(d), Some(t)) => get(STYLE, 16 + 224 + ((d * 7 + k.cy) * 4 + t) * 8 + level),
            (None, None) => Entry::Same,
        };
    }
    let has_date = f[..5].iter().any(|v| *v != 0);
    let has_time = f[5..].iter().any(|v| *v != 0) || k.fsd != 0;
    if !has_time && k.z == 0 {
        return if level < 4 { get(DATE, dkey(f) * 4 + level) } else { Entry::Same };
    }
    if !has_time {
        if level < 4
            && let Some(e) = entry_of(cell(l, DZ, ((k.z - 1) * 864 + dkey(f)) * 4 + level))
        {
            return e;
        }
        return if ignored(level, single) { Entry::Same } else { fallback(single, sep) };
    }
    let tk = ((((f[6] * 3 + f[7]) * 3 + f[8]) * 7 + k.cy) * 4 + f[5]) * 4 + k.fsd;
    let tp_cell = cell(l, TP, k.z * 3024 + tk);
    let tpat = if tp_cell == NONE { single } else { pattern(tp_cell) };
    let tab: Option<[Option<Entry>; 5]> = (k.fsd == 0 && f[8] == 0).then(|| {
        let base = (k.z * 252 + ((f[6] * 3 + f[7]) * 7 + k.cy) * 4 + f[5]) * 5;
        core::array::from_fn(|i| entry_of(cell(l, TIME, base + i)))
    });
    // ICU has interval patterns for every hour skeleton (in some locales they read like a fallback).
    // With a day period (B) ICU can miss: en-US with a zone has none, and its hour entry reads as one.
    let real = |e: &Option<Entry>| matches!(e, Some(e @ Entry::Pair(..)) if !is_fallback(e, sep));
    let found = tab.as_ref().is_some_and(|t| (f[6] != 0 && (f[5] == 0 || k.z == 0 || real(&t[3]))) || t[2..].iter().any(real));
    fn pat_or<'a>(l: &RangeLocale, row: usize, i: usize, or: &'a str) -> &'a str {
        let c = cell(l, row, i);
        if c == NONE { or } else { pattern(c) }
    }
    let tpatf = pat_or(l, TPF, k.z * 3024 + tk, tpat);
    let glue_by = |fd: &[usize]| match (fd[3], fd[0]) {
        (3, w) if w != 0 => l.glue[0],
        (3, _) => l.glue[1],
        (4, _) => l.glue[2],
        _ => l.glue[3],
    };
    // A fallback formats both dates with ICU's own pattern for the skeleton.
    let best = |fd: &[usize]| apply(glue_by(fd), pat_or(l, DP, dkey(fd), ""), tpatf);
    if !has_date {
        if level < 4 {
            let fd: [usize; 5] = if level == 0 { [0, 2, 1, 1, 1] } else { [0, 0, 1, 1, 1] };
            // ICU builds this skeleton from its time skeleton, which leaves an offset zone (O) out.
            let t = if matches!(k.z, 3 | 4) { pat_or(l, TPF, tk, pat_or(l, TP, tk, tpatf)) } else { tpatf };
            return fallback(&apply(l.glue[3], pat_or(l, DP, dkey(&fd), ""), t), sep);
        }
        if found && level <= 6 {
            return tab.and_then(|t| t[level - 2].clone()).unwrap_or(Entry::Same);
        }
        return if ignored(level, tpat) { Entry::Same } else { fallback(tpat, sep) };
    }
    if level < 4 {
        // ICU sets an interval pattern for a date field the skeleton lacks: the skeleton with that
        // field and every finer one it lacks added. A field it has falls back to the whole pattern.
        let mut g = [f[0], f[1], f[2], f[3], f[4]];
        // ICU looks for the field's letter in the skeleton V8 reads off the pattern, so a stand-alone
        // month (L, as en-US picks for a month without a day) counts as missing too.
        let missing = f[level + 1] == 0 || (level == 2 && tokens(single).iter().any(|t| letter(t) == Some('L')));
        if found && missing {
            for (lv, i, v) in [(3, 4, 1), (2, 3, 1), (1, 2, 1), (0, 1, 2)] {
                if level <= lv && g[i] == 0 {
                    g[i] = v;
                }
            }
        }
        let p = best(&g);
        return if ignored(level, &p) { Entry::Same } else { fallback(&p, sep) };
    }
    if found && level <= 6 {
        return match tab.and_then(|t| t[level - 2].clone()) {
            Some(Entry::Pair(a, b)) => glue_date(l.gm, pat_or(l, DP, dkey(f), ""), &a, &b),
            _ => Entry::Same,
        };
    }
    if ignored(level, single) {
        return Entry::Same;
    }
    let dpn = pat_or(l, DPN, dkey(f), pat_or(l, DP, dkey(f), ""));
    // ICU's time pattern here comes from the normalized time skeleton: one z or v, and no O.
    let zn = match k.z {
        2 => 1,
        6 => 5,
        3 | 4 => 0,
        z => z,
    };
    let tpn = pat_or(l, TP, zn * 3024 + tk, tpat);
    glue_date(l.gm, dpn, &format!("{tpn}{}", quote(sep)), tpn)
}

/// A part's type for a pattern letter, to tell which fields both halves show.
fn field_type(c: char) -> u8 {
    match c {
        'G' => 0,
        'y' | 'u' | 'U' => 1,
        'M' | 'L' => 2,
        'd' => 3,
        'E' | 'c' => 4,
        'a' | 'b' | 'B' => 5,
        'h' | 'H' | 'k' | 'K' => 6,
        'm' => 7,
        's' => 8,
        'S' => 9,
        _ => 10,
    }
}

/// The local calendar fields ICU compares: era, year, month, day, am/pm, hour, minute, second, ms.
fn fields_at(zone: &str, gmt: bool, t: f64) -> [i64; 9] {
    let ms = t as i64;
    let off = zone_at(zone, ms.div_euclid(1000), gmt).off;
    let local = ms + off * 1000;
    let (y, mo, d) = civil(local.div_euclid(86_400_000));
    let in_day = local.rem_euclid(86_400_000);
    let h = in_day / 3_600_000;
    [i64::from(y > 0), y, i64::from(mo), i64::from(d), i64::from(h >= 12), h % 12, in_day / 60_000 % 60, in_day / 1000 % 60, in_day % 1000]
}

/// A range part: (type, value, source) with source 0 startRange, 1 endRange, 2 shared.
pub(in crate::builtins::intl) type RangePart = (&'static str, String, u8);

/// A range's parts, and whether they are the start alone.
pub(in crate::builtins::intl) fn format_range(spec: &Spec, x: f64, y: f64) -> Result<(Vec<RangePart>, bool), Fault> {
    for t in [x, y] {
        if !t.is_finite() || t.abs() > 8.64e15 {
            return super::range("Invalid time value");
        }
    }
    let zone = spec.get("@zone").or(spec.get("timeZone")).unwrap_or("UTC");
    let single = spec.get("@pattern").unwrap_or("");
    let gmt = spec.get("@gmt").is_some();
    let (fa, fb) = (fields_at(zone, gmt, x), fields_at(zone, gmt, y));
    let level = (0..9).find(|&i| fa[i] != fb[i]);
    let key = spec.get("@rk").and_then(Key::decode);
    let l = match spec.get("@dl").and_then(|i| i.parse::<usize>().ok()) {
        // RANGE_LOCALES follows DATE_LOCALES and ends with en-US: a locale past it has no tables.
        Some(i) if i < DATE_LOCALES.len() && i + 1 < RANGE_LOCALES.len() => RANGE_LOCALES.get(i),
        _ if spec.get("locale").is_some_and(|loc| loc == "en-US" || loc.starts_with("en-US-")) => RANGE_LOCALES.last(),
        _ => None,
    };
    let e = match (level, key, l) {
        (None, _, _) => Entry::Same,
        (Some(lv), Some(k), Some(l)) => entry(l, &k, single, lv),
        // Locales without range tables: ICU's fallback over the format's own pattern.
        (Some(lv), _, _) => if ignored(lv, single) { Entry::Same } else { fallback(single, " – ") },
    };
    let mut out: Vec<RangePart> = Vec::new();
    let mut push = |ty: &'static str, v: String, src: u8| {
        if ty == "literal"
            && let Some(last) = out.last_mut()
            && last.0 == "literal"
            && last.2 == src
        {
            last.1.push_str(&v);
            return;
        }
        out.push((ty, v, src));
    };
    let Entry::Pair(a, b) = e else {
        for (ty, v) in format_pattern(spec, x, single, single)?.items {
            push(ty, v, 2);
        }
        return Ok((out, true));
    };
    let halves = [tokens(&a), tokens(&b)];
    let types = |h: &[&str]| -> u16 { h.iter().filter_map(|t| letter(t)).fold(0, |m, c| m | 1 << field_type(c)) };
    let rep = types(&halves[0]) & types(&halves[1]);
    for (hi, (h, p)) in halves.iter().zip([&a, &b]).enumerate() {
        let t = if hi == 0 { x } else { y };
        let is_rep = |tok: &&str| letter(tok).is_some_and(|c| rep & (1 << field_type(c)) != 0);
        let first = h.iter().position(is_rep);
        let last = h.iter().rposition(is_rep);
        let mut i = 0;
        while i < h.len() {
            let src = |j: usize| match (first, last) {
                (Some(f), Some(l)) if j >= f && j <= l => hi as u8,
                _ => 2,
            };
            let s = src(i);
            let mut j = i;
            let mut seg = String::new();
            while j < h.len() && src(j) == s {
                seg.push_str(h[j]);
                j += 1;
            }
            for (ty, v) in format_pattern(spec, t, &seg, p)?.items {
                push(ty, v, s);
            }
            i = j;
        }
    }
    Ok((out, false))
}

