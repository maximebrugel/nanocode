//! Intl.Collator and String.prototype.localeCompare: the Unicode Collation Algorithm with
//! the CLDR root collation (ducet.rs), as ICU's root collator computes it: strings in NFD,
//! contractions (with discontiguous matching), prefix mappings, implicit weights for Han and
//! unassigned code points, variable weighting (non-ignorable, or shifted with
//! ignorePunctuation), numeric collation and caseFirst.
#[path = "ducet.rs"]
mod ducet;

use ducet::{
    COMMON_S as COMMON_S1, COMMON_T, CONTR, DIGIT_ZERO, DIGIT_ZEROS, IMPLICIT, MAP, POOL, PREFIX, SEQ,
    TRAILING, UNIFIED, VARIABLE_TOP,
};
use super::{Spec, range};
use crate::interp::conv::Fault;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// The common secondary weight, on the scale of 8 that Ce's s uses (room for tailorings below it).
const COMMON_S: u16 = COMMON_S1 << 3;

/// One collation element. `p` is the primary as a sort key: the main weight in the high
/// half (0: ignorable), a sub-weight for implicit and numeric primaries in the low half.
#[derive(Clone, Copy, Debug)]
struct Ce {
    p: u64,
    s: u16,
    t: u8,
    /// 0 lower, 1 mixed, 2 upper.
    case: u8,
}

struct Options {
    han: Option<&'static [u8]>,
    /// ko: Han sort by their Hangul reading.
    ko: bool,
    /// zh: pinyin tones sort before the plain vowel (ā á ǎ à a), and after ê (ê ê̄ ế ê̌ ề).
    tones: bool,
    /// vi: tone marks in Vietnamese order (grave, hook, tilde, acute, dot below).
    vi: bool,
    order: (&'static [Script], bool),
    tail: (&'static [Tail], bool),
    numeric: bool,
    shifted: bool,
    case_first: u8,
    level: u8,
}

/// The sort-key position of a table primary rank: implicit primaries sit between the
/// explicit ones and the trailing ones (U+FFFD, U+FFFF).
fn main_key(rank: u32) -> u64 {
    if rank + 1 < TRAILING {
        u64::from(rank)
    } else if rank + 1 == TRAILING {
        // U+FDD1's primary: between Han and the other implicit primaries.
        0x4_0000
    } else {
        u64::from(rank) + 0x20_0000
    }
}

/// Han in radical-stroke order: runs of code points with consecutive positions, by start,
/// each `(start - 0x3400) | position << 18` in 5 little-endian bytes (gen-collation.py).
static HAN: &[u8] = include_bytes!("han.bin");

/// Han in a locale's reading or stroke order (scripts/gen-han-order.js): zh pinyin, zh stroke
/// (zh-Hant's default) and ja. Their tailored Han sort first, the rest after them by
/// radical and strokes.
static HAN_ZH: &[u8] = include_bytes!("han-zh.bin");
static HAN_STROKE: &[u8] = include_bytes!("han-zh-stroke.bin");
static HAN_JA: &[u8] = include_bytes!("han-ja.bin");
static HAN_ZHUYIN: &[u8] = include_bytes!("han-zh-zhuyin.bin");
/// ko: Han with a Hangul reading sort a secondary difference after that syllable.
static HAN_KO: &[u8] = include_bytes!("han-ko.bin");
static HAN_KO_SYL: &[u8] = include_bytes!("han-ko-syl.bin");

/// A ko Han's collation elements: its reading's, then a secondary for its place among that
/// reading's Han.
fn ko_han(cp: u32, out: &mut Vec<Ce>) -> bool {
    let Some(key) = han_key(HAN_KO, cp) else { return false };
    let count = u64::from(u16::from_le_bytes([HAN_KO[2], HAN_KO[3]]));
    let rank = key - 0x1_0000;
    if rank >= count {
        return false;
    }
    let r = rank as usize;
    let syl = u32::from(u16::from_le_bytes([HAN_KO_SYL[r * 2], HAN_KO_SYL[r * 2 + 1]]));
    let k = u16::from(HAN_KO_SYL[count as usize * 2 + r]);
    // The syllable's jamo (code_points gives Hangul in NFD).
    single(0x1100 + syl / 588, out);
    single(0x1161 + syl % 588 / 28, out);
    if syl % 28 != 0 {
        single(0x11A7 + syl % 28, out);
    }
    out.push(Ce { p: 0, s: COMMON_S + 1 + k, t: COMMON_T, case: 0 });
    true
}

/// The main key of a Han code point in a locale's Han order.
fn han_key(table: &[u8], cp: u32) -> Option<u64> {
    let at = han_position(cp)?;
    let word = |i: usize| u16::from_le_bytes([table[i], table[i + 1]]);
    let (dense, count) = (word(0) == 1, u64::from(word(2)));
    let mut rest = &table[4..];
    if dense {
        if (0x4E00..=0x9FFF).contains(&cp) {
            let r = word(4 + (cp - 0x4E00) as usize * 2);
            return Some(0x1_0000 + if r == 0xFFFF { count + at } else { u64::from(r) });
        }
        rest = &table[4 + 0x5200 * 2..];
    }
    let entry = |i: usize| {
        let e = &rest[i * 5..i * 5 + 5];
        (u32::from(e[0]) | u32::from(e[1]) << 8 | u32::from(e[2]) << 16, u64::from(u16::from_le_bytes([e[3], e[4]])))
    };
    let n = rest.len() / 5;
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = usize::midpoint(lo, hi);
        if entry(mid).0 < cp { lo = mid + 1 } else { hi = mid }
    }
    Some(0x1_0000 + if lo < n && entry(lo).0 == cp { entry(lo).1 } else { count + at })
}

/// The Han order of a collation: None for the root's radical-stroke order.
fn han_table(spec: &Spec) -> Option<&'static [u8]> {
    if spec.get("usage") == Some("search") {
        return None;
    }
    let locale = spec.get("locale").unwrap_or("");
    match locale.split('-').next().unwrap_or("") {
        "ja" => Some(HAN_JA),
        "zh" => {
            let hant = is_hant(locale);
            let co = unicode_keywords(locale).into_iter().find(|(k, _)| *k == "co").map(|(_, v)| v);
            match co.as_deref() {
                Some("pinyin") => Some(HAN_ZH),
                Some("stroke") => Some(HAN_STROKE),
                Some("zhuyin") => Some(HAN_ZHUYIN),
                Some("unihan") => None,
                _ => Some(if hant { HAN_STROKE } else { HAN_ZH }),
            }
        }
        _ => None,
    }
}

/// The radical-stroke position of a Han code point.
fn han_position(cp: u32) -> Option<u64> {
    if !UNIFIED.iter().any(|&(a, b)| (a..=b).contains(&cp)) {
        return None;
    }
    let run = |i: usize| {
        let b = &HAN[i * 5..i * 5 + 5];
        let v = b.iter().rev().fold(0u64, |v, x| v << 8 | u64::from(*x));
        ((v & 0x3_FFFF) as u32 + 0x3400, v >> 18)
    };
    let (mut lo, mut hi) = (0, HAN.len() / 5);
    while lo < hi {
        let mid = usize::midpoint(lo, hi);
        if run(mid).0 <= cp { lo = mid + 1 } else { hi = mid }
    }
    let (start, at) = run(lo.checked_sub(1)?);
    Some(at + u64::from(cp - start))
}

/// Implicit weights, after the table's explicit primaries: Han by radical and strokes, then
/// everything else by code point.
fn implicit(cp: u32, t: u8, case: u8) -> Ce {
    let key = match han_position(cp) {
        Some(at) => 0x1_0000 + at,
        None => 0x4_0001 + u64::from(cp),
    };
    Ce { p: key << 32, s: COMMON_S, t, case }
}

fn unpack(w: u32) -> Ce {
    let rank = w >> 16;
    Ce {
        p: if rank == 0 { 0 } else { main_key(rank) << 32 },
        s: (((w >> 7) & 0x1FF) as u16) << 3,
        t: ((w >> 2) & 31) as u8,
        case: (w & 3) as u8,
    }
}

fn push_words(words: &[u32], out: &mut Vec<Ce>) {
    let mut i = 0;
    while i < words.len() {
        if words[i] == IMPLICIT {
            let x = words[i + 1];
            out.push(implicit(x & 0x1F_FFFF, ((x >> 21) & 31) as u8, ((x >> 26) & 3) as u8));
            i += 2;
        } else {
            out.push(unpack(words[i]));
            i += 1;
        }
    }
}

fn pool(e: u32) -> &'static [u32] {
    let at = (e >> 5) as usize;
    &POOL[at..at + (e & 31) as usize]
}

/// The CEs of one code point without context.
fn single(cp: u32, out: &mut Vec<Ce>) {
    let k = MAP.partition_point(|e| e.0 >> 11 <= cp);
    if k > 0 {
        let (head, w) = MAP[k - 1];
        let start = head >> 11;
        if cp - start <= head & 0x1FF {
            match (head >> 9) & 3 {
                0 => out.push(unpack(w + ((cp - start) << 16))),
                1 => out.push(unpack(w)),
                _ => push_words(pool(w), out),
            }
            return;
        }
    }
    out.push(implicit(cp, COMMON_T, 0));
}

#[cfg(feature = "unicode-normalization")]
fn ccc(c: u32) -> u8 {
    crate::unicode::normalize::ccc(c)
}

#[cfg(not(feature = "unicode-normalization"))]
fn ccc(c: u32) -> u8 {
    u8::from((0x300..0x370).contains(&c))
}

/// The longest contraction starting at `i`: its table entry and the positions it takes.
fn contraction(cps: &[u32], used: &[bool], i: usize) -> Option<(u32, Vec<usize>)> {
    let c = cps[i];
    let first = |e: &(u32, u32)| SEQ[(e.0 >> 3) as usize];
    let from = CONTR.partition_point(|e| first(e) < c);
    'next: for e in CONTR[from..].iter().take_while(|e| first(e) == c) {
        let at = (e.0 >> 3) as usize;
        let seq = &SEQ[at + 1..at + (e.0 & 7) as usize];
        let mut taken = Vec::new();
        let mut j = i + 1;
        // The highest combining class skipped over (a discontiguous match).
        let mut skipped: Option<u8> = None;
        for &want in seq {
            loop {
                if j >= cps.len() {
                    continue 'next;
                }
                if used[j] {
                    j += 1;
                    continue;
                }
                let cc = ccc(cps[j]);
                if cps[j] == want && skipped.is_none_or(|s| cc != 0 && s < cc) {
                    taken.push(j);
                    j += 1;
                    break;
                }
                if cc == 0 {
                    continue 'next;
                }
                skipped = Some(skipped.map_or(cc, |s| s.max(cc)));
                j += 1;
            }
        }
        return Some((e.1, taken));
    }
    None
}

fn digit(c: u32) -> Option<u32> {
    let k = DIGIT_ZEROS.partition_point(|z| *z <= c);
    let z = *DIGIT_ZEROS.get(k.checked_sub(1)?)?;
    (c - z < 10).then_some(c - z)
}

fn code_points(units: &[u16]) -> Vec<u32> {
    #[cfg(feature = "unicode-normalization")]
    let nfd = crate::unicode::normalize::normalize(
        units,
        crate::unicode::normalize::Form::Nfd,
        &mut crate::unicode::normalize::Compositions::default(),
    );
    #[cfg(feature = "unicode-normalization")]
    let units = &nfd[..];
    // A lone surrogate collates as its own (unassigned) code point.
    char::decode_utf16(units.iter().copied())
        .map(|r| r.map_or_else(|e| u32::from(e.unpaired_surrogate()), u32::from))
        .collect()
}

/// One tailored letter of a locale (CLDR's collation rules for it, as ICU applies them): seq
/// in NFC, lowercase, sorts by the primary of after (a lowercase ASCII letter), n places
/// after it (0: the same primary), with sec added to its secondary weight. The letter's
/// uppercase matches too, at the tertiary level.
type Tail = (&'static str, char, u8, u16);

const SV: &[Tail] = &[("å", 'z', 1, 0), ("ä", 'z', 2, 0), ("æ", 'z', 2, 1), ("ö", 'z', 3, 0), ("ø", 'z', 3, 1), ("ü", 'y', 0, 1)];
const DA: &[Tail] = &[("æ", 'z', 1, 0), ("ä", 'z', 1, 1), ("ø", 'z', 2, 0), ("ö", 'z', 2, 1), ("å", 'z', 3, 0), ("ü", 'y', 0, 1)];
const ES: &[Tail] = &[("ñ", 'n', 1, 0)];
const ES_TRAD: &[Tail] = &[("ch", 'c', 1, 0), ("ll", 'l', 1, 0), ("ñ", 'n', 1, 0)];
static TR: &[Tail] = &[("ç", 'c', 1, 0), ("ğ", 'g', 1, 0), ("ı", 'h', 1, 0), ("ö", 'o', 1, 0), ("ş", 's', 1, 0), ("ü", 'u', 1, 0)];
const CS: &[Tail] = &[("č", 'c', 1, 0), ("ch", 'h', 1, 0), ("ř", 'r', 1, 0), ("š", 's', 1, 0), ("ž", 'z', 1, 0)];
static HU: &[Tail] = &[
    ("cs", 'c', 1, 0),
    ("dzs", 'd', 2, 0),
    ("dz", 'd', 1, 0),
    ("gy", 'g', 1, 0),
    ("ly", 'l', 1, 0),
    ("ny", 'n', 1, 0),
    ("sz", 's', 1, 0),
    ("ty", 't', 1, 0),
    ("zs", 'z', 1, 0),
    ("ö", 'o', 1, 0),
    ("ő", 'o', 1, 1),
    ("ü", 'u', 1, 0),
    ("ű", 'u', 1, 1),
];
/// ro and vi letters keep their tailored primary under further accents (ắ is ă with an acute).
static RO: &[Tail] = &[("ă", 'a', 1, 0), ("â", 'a', 2, 0), ("î", 'i', 1, 0), ("ș", 's', 1, 0), ("ş", 's', 1, 0), ("ț", 't', 1, 0), ("ţ", 't', 1, 0)];
static VI: &[Tail] = &[("ă", 'a', 1, 0), ("â", 'a', 2, 0), ("đ", 'd', 1, 0), ("ê", 'e', 1, 0), ("ô", 'o', 1, 0), ("ơ", 'o', 2, 0), ("ư", 'u', 1, 0)];
const FIL: &[Tail] = &[("ñ", 'n', 1, 0), ("ng", 'n', 2, 0)];
const PL: &[Tail] = &[("ą", 'a', 1, 0), ("ć", 'c', 1, 0), ("ę", 'e', 1, 0), ("ł", 'l', 1, 0), ("ń", 'n', 1, 0), ("ó", 'o', 1, 0), ("ś", 's', 1, 0), ("ź", 'z', 1, 0), ("ż", 'z', 2, 0)];
/// de-u-co-phonebk: the umlauts expand to their base letter and e.
const DE_PHONEBK: &[(&str, &str)] = &[("ä", "ae"), ("ö", "oe"), ("ü", "ue")];

/// The languages a Collator resolves to beyond mod.rs LANGS: tailored, reordered, or (ca, sw,
/// id) collating as the root does.
pub(super) const TAILORED: [&str; 20] = ["sv", "fi", "da", "nb", "no", "tr", "cs", "pl", "uk", "el", "he", "ar", "th", "ca", "sw", "id", "hu", "ro", "vi", "fil"];

fn tailoring(spec: &Spec) -> (&'static [Tail], bool) {
    let locale = spec.get("locale").unwrap_or("");
    let co = spec.get("collation").unwrap_or("default");
    let search = spec.get("usage") == Some("search");
    match (locale.split('-').next().unwrap_or(""), co) {
        // The search collations of cs and pl are the root's; de's expands umlauts as phonebk does.
        ("cs" | "pl", _) if search => (&[], false),
        ("de", _) if search => (&[], true),
        // hu, ro, vi and fil search as the root does.
        ("hu" | "ro" | "vi" | "fil", _) if search => (&[], false),
        ("sv" | "fi", _) => (SV, false),
        ("da" | "nb" | "no", _) => (DA, false),
        ("es", "trad") if spec.get("usage") != Some("search") => (ES_TRAD, false),
        ("es", _) => (ES, false),
        ("tr", _) => (TR, false),
        ("cs", _) => (CS, false),
        ("pl", _) => (PL, false),
        ("hu", _) => (HU, false),
        ("ro", _) => (RO, false),
        ("vi", _) => (VI, false),
        ("fil", _) => (FIL, false),
        ("de", "phonebk") => (&[], true),
        _ => (&[], false),
    }
}

fn nfd_of(s: &str) -> Vec<u32> {
    let units: Vec<u16> = s.encode_utf16().collect();
    code_points(&units)
}

/// Whether want (NFD) starts at cps[i], its first code point in either case, with no further
/// combining mark on it; the case of the match (0 lower, 2 upper).
#[allow(clippy::if_same_then_else)]
fn tail_match(cps: &[u32], used: &[bool], i: usize, want: &[u32], loose: bool) -> Option<u8> {
    if i + want.len() > cps.len() || used[i..i + want.len()].iter().any(|u| *u) {
        return None;
    }
    let first = char::from_u32(want[0])?;
    let upper: Vec<u32> = first.to_uppercase().map(u32::from).collect();
    let case = if cps[i] == want[0] {
        0
    } else if upper.len() == 1 && cps[i] == upper[0] {
        2
    } else if first == '\u{131}' && cps[i] == u32::from('I') {
        2
    } else {
        return None;
    };
    // Ch is mixed case; CH, and an uppercase letter with combining marks, upper.
    let rest_lower = want[1..].iter().zip(&cps[i + 1..]).any(|(w, c)| w == c && char::from_u32(*w).is_some_and(char::is_lowercase));
    let case = if case == 2 && rest_lower { 1 } else { case };
    let rest_ok = want[1..].iter().zip(&cps[i + 1..]).all(|(w, c)| {
        *w == *c || char::from_u32(*w).is_some_and(|w| w.to_uppercase().eq(char::from_u32(*c)))
    });
    let next_free = loose || cps.get(i + want.len()).is_none_or(|c| ccc(*c) == 0);
    (rest_ok && next_free).then_some(case)
}

/// The CEs of a tailored letter or expansion at cps[i], and how many code points it takes.
/// hu's doubled digraphs (ccs, ggy, ssz, ddzs...) sort as the digraph twice.
fn hu_double(cps: &[u32], used: &[bool], i: usize, tail: &(&'static [Tail], bool), out: &mut Vec<Ce>) -> Option<usize> {
    let lower = |c: u32| char::from_u32(c).map_or(c, |c| u32::from(c.to_ascii_lowercase()));
    let (first, second) = (*cps.get(i)?, *cps.get(i + 1)?);
    if used.get(i + 1) != Some(&false) || lower(first) != lower(second) {
        return None;
    }
    for digraph in ["dzs", "cs", "dz", "gy", "ly", "ny", "sz", "ty", "zs"] {
        let want: Vec<u32> = digraph.chars().map(u32::from).collect();
        let n = want.len();
        let Some(rest) = cps.get(i + 1..i + 1 + n) else { continue };
        if rest.iter().map(|c| lower(*c)).ne(want.iter().copied()) || used[i + 1..i + 1 + n].iter().any(|u| *u) {
            continue;
        }
        if cps.get(i + 1 + n).is_some_and(|c| ccc(*c) != 0) {
            return None;
        }
        let mut one: Vec<u32> = alloc::vec![first];
        one.extend_from_slice(&rest[1..]);
        let free = alloc::vec![false; n];
        tailored(&one, &free, 0, tail, out)?;
        tailored(rest, &free, 0, tail, out)?;
        return Some(n + 1);
    }
    None
}

fn tailored(cps: &[u32], used: &[bool], i: usize, tail: &(&'static [Tail], bool), out: &mut Vec<Ce>) -> Option<usize> {
    // Turkish: dotted İ (I and U+0307 in NFD) is the uppercase of i.
    if core::ptr::eq(tail.0, TR) && cps[i] == u32::from('I') && cps.get(i + 1) == Some(&0x307) && !used[i + 1] {
        let mut base = Vec::new();
        single(u32::from('i'), &mut base);
        let b = *base.first()?;
        out.push(Ce { p: b.p, s: b.s, t: upper_tertiary('i'), case: 2 });
        return Some(2);
    }
    let loose = core::ptr::eq(tail.0, RO) || core::ptr::eq(tail.0, VI);
    for &(seq, after, n, sec) in tail.0 {
        let want = nfd_of(seq);
        let Some(case) = tail_match(cps, used, i, &want, loose) else { continue };
        if seq == "\u{131}" && case == 2 && cps[i] != u32::from('I') {
            continue;
        }
        let mut base = Vec::new();
        single(u32::from(after), &mut base);
        let b = *base.first()?;
        let t = match case {
            2 => upper_tertiary(after) + u8::from(want.len() > 1),
            1 => upper_tertiary(after),
            _ => b.t,
        };
        out.push(Ce { p: b.p | u64::from(n), s: b.s + (sec << 3), t, case });
        return Some(want.len());
    }
    if tail.1 {
        for &(seq, exp) in DE_PHONEBK {
            let want = nfd_of(seq);
            let Some(case) = tail_match(cps, used, i, &want, false) else { continue };
            let start = out.len();
            for (k, c) in exp.chars().enumerate() {
                single(u32::from(if case == 2 && k == 0 { c.to_ascii_uppercase() } else { c }), out);
            }
            // A secondary just after the expansion's, and before æ's (&AE<<ä).
            if let Some(e) = out.get_mut(start) {
                e.s += 4;
            }
            return Some(want.len());
        }
    }
    None
}

/// zh's pinyin tones: a vowel (or m, n, ü) with a macron, acute, caron or grave sorts just
/// before the plain letter in that order; ê with one sorts just after ê.
fn tone(cps: &[u32], used: &[bool], i: usize, out: &mut Vec<Ce>) -> Option<usize> {
    let c = char::from_u32(cps[i])?;
    let next = |k: usize| cps.get(i + k).copied().filter(|_| !used.get(i + k).copied().unwrap_or(true));
    let lower = c.to_ascii_lowercase();
    let base = match (lower, next(1)) {
        ('u', Some(0x308)) => 2,
        ('e', Some(0x302)) => 2,
        ('a' | 'e' | 'i' | 'o' | 'u' | 'm' | 'n', _) => 1,
        _ => return None,
    };
    let at = [0x304, 0x301, 0x30C, 0x300].iter().position(|m| Some(*m) == next(base))?;
    if next(base + 1).is_some_and(|c| ccc(c) != 0) {
        return None;
    }
    for k in 0..base {
        single(cps[i + k], out);
    }
    if lower == 'e' && base == 2 {
        let mut mark = Vec::new();
        single(0x301, &mut mark);
        let t = mark.first().map_or(COMMON_T, |e| e.t);
        out.push(Ce { p: 0, s: at as u16 + 1, t, case: 0 });
    } else {
        let e = out.last_mut()?;
        e.s -= 4 - at as u16;
    }
    Some(base + 1)
}

/// The tertiary weight of an uppercase ASCII letter.
fn upper_tertiary(c: char) -> u8 {
    let mut v = Vec::new();
    single(u32::from(c.to_ascii_uppercase()), &mut v);
    v.first().map_or(COMMON_T, |e| e.t)
}
/// The script groups a locale's collation moves (CLDR's reorder codes): before Latin, or
/// right after it (ja), each group keeping its own order.
#[derive(Clone, Copy, PartialEq)]
enum Script { Cyrl, Grek, Hebr, Arab, Thai, Hang, Kana, Hani }

fn script_of(cp: u32) -> Option<Script> {
    Some(match cp {
        0x400..=0x52F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => Script::Cyrl,
        0x370..=0x3FF | 0x1F00..=0x1FFF => Script::Grek,
        0x591..=0x5FF | 0xFB1D..=0xFB4F => Script::Hebr,
        0x600..=0x6FF | 0x750..=0x77F | 0x8A0..=0x8FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => Script::Arab,
        0xE01..=0xE7F => Script::Thai,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7AF | 0xD7B0..=0xD7FF => Script::Hang,
        0x3041..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => Script::Kana,
        _ if han_position(cp).is_some() => Script::Hani,
        _ => return None,
    })
}

/// The locale's reordered groups, and whether they go after Latin rather than before it.
fn reorder(locale: &str) -> (&'static [Script], bool) {
    match locale.split('-').next().unwrap_or("") {
        "ru" | "uk" => (&[Script::Cyrl], false),
        "el" => (&[Script::Grek], false),
        "he" => (&[Script::Hebr], false),
        "ar" => (&[Script::Arab], false),
        "th" => (&[Script::Thai], false),
        "zh" => (&[Script::Hani], false),
        "ko" => (&[Script::Hang, Script::Hani], false),
        "ja" => (&[Script::Kana, Script::Hani], true),
        _ => (&[], false),
    }
}

/// Moves the primaries of a reordered group's CEs next to Latin: under the main key just
/// before Latin's first letter (or Greek's, after Latin), by group and then original key.
fn reorder_ces(ces: &mut [Ce], cp: u32, order: &(&'static [Script], bool)) {
    let Some(at) = script_of(cp).and_then(|s| order.0.iter().position(|g| *g == s)) else { return };
    let mut first = Vec::new();
    single(u32::from(if order.1 { '\u{3b1}' } else { 'a' }), &mut first);
    let Some(anchor) = first.first().map(|e| e.p >> 32) else { return };
    for e in ces.iter_mut().filter(|e| e.p != 0) {
        // Every main key (Han and implicit ones too) fits in 24 bits.
        e.p = ((anchor - 1) << 32) | ((at as u64 + 1) << 24) | ((e.p >> 32) & 0xFF_FFFF);
    }
}
fn elems(units: &[u16], o: &Options) -> Vec<Ce> {
    let cps = code_points(units);
    let mut out: Vec<Ce> = Vec::new();
    let mut used = alloc::vec![false; cps.len()];
    let zero = main_key(DIGIT_ZERO) << 32;
    let mut i = 0;
    while i < cps.len() {
        if used[i] {
            i += 1;
            continue;
        }
        let c = cps[i];
        if o.numeric && digit(c).is_some() {
            let mut run = Vec::new();
            while i < cps.len() && !used[i] {
                let Some(d) = digit(cps[i]) else { break };
                if !(run.is_empty() && d == 0) {
                    run.push(d);
                }
                i += 1;
            }
            if run.is_empty() {
                run.push(0);
            }
            out.push(Ce { p: zero | run.len() as u64, s: COMMON_S, t: COMMON_T, case: 0 });
            for d in run {
                out.push(Ce { p: zero | u64::from(d + 1), s: COMMON_S, t: COMMON_T, case: 0 });
            }
            continue;
        }
        if o.tones
            && let Some(n) = tone(&cps, &used, i, &mut out)
        {
            i += n;
            continue;
        }
        if core::ptr::eq(o.tail.0, HU)
            && let Some(n) = hu_double(&cps, &used, i, &o.tail, &mut out)
        {
            i += n;
            continue;
        }
        let start = out.len();
        if let Some(n) = tailored(&cps, &used, i, &o.tail, &mut out) {
            i += n;
            continue;
        }
        if i > 0
            && let Some(&(_, _, w)) = PREFIX.iter().find(|e| e.0 == c && e.1 == cps[i - 1])
        {
            out.push(unpack(w));
        } else if let Some((e, taken)) = contraction(&cps, &used, i) {
            for j in taken {
                used[j] = true;
            }
            push_words(pool(e), &mut out);
        } else if o.ko && ko_han(c, &mut out) {
        } else if let Some(key) = o.han.and_then(|t| han_key(t, c)) {
            out.push(Ce { p: key << 32, s: COMMON_S, t: COMMON_T, case: 0 });
        } else {
            single(c, &mut out);
        }
        if !o.order.0.is_empty() {
            // A ko Han with a Hangul reading reorders with Hangul.
            let reading = o.ko && han_position(c).is_some() && out[start..].first().is_some_and(|e| e.p >> 32 < 0x1_0000);
            reorder_ces(&mut out[start..], if reading { 0xAC00 } else { c }, &o.order);
        }
        i += 1;
    }
    if o.vi {
        // vi: the tone marks follow the grave in Vietnamese order (hook, tilde, acute, dot below).
        let marks = [0x300, 0x309, 0x303, 0x301, 0x323].map(|m| {
            let mut v = Vec::new();
            single(m, &mut v);
            v.first().map_or(0, |e| e.s)
        });
        for e in out.iter_mut().filter(|e| e.p == 0) {
            if let Some(k) = marks.iter().position(|m| *m == e.s) {
                e.s = marks[0] + k as u16;
            }
        }
    }
    if o.shifted {
        // Variable CEs (spaces and punctuation) are ignored, with the ignorables after them.
        let mut after_variable = false;
        out.retain(|e| {
            let main = e.p >> 32;
            if main != 0 && main <= u64::from(VARIABLE_TOP) {
                after_variable = true;
                return false;
            }
            if main != 0 {
                after_variable = false;
            }
            !after_variable
        });
    }
    out
}

fn options(spec: &Spec) -> Options {
    let level = match spec.get("sensitivity") {
        Some("base") => 1,
        Some("accent") => 2,
        Some("case") => 3,
        _ => 4,
    };
    let lang = spec.get("locale").unwrap_or("").split('-').next().unwrap_or("");
    let sort = spec.get("usage") != Some("search");
    Options {
        han: han_table(spec),
        ko: sort && lang == "ko",
        tones: sort && lang == "zh",
        vi: sort && lang == "vi",
        // The search collations keep the root script order.
        order: if spec.get("usage") == Some("search") { (&[], false) } else { reorder(spec.get("locale").unwrap_or("")) },
        tail: tailoring(spec),
        numeric: spec.get("numeric") == Some("!t"),
        shifted: spec.get("ignorePunctuation") == Some("!t"),
        case_first: match spec.get("caseFirst") {
            Some("upper") => 2,
            Some("lower") => 1,
            _ => 0,
        },
        level,
    }
}

fn cmp_level<K: Ord>(x: &[Ce], y: &[Ce], key: impl Fn(&Ce) -> Option<K>) -> Ordering {
    x.iter().filter_map(&key).cmp(y.iter().filter_map(&key))
}

pub(super) fn compare(spec: &Spec, a: &[u16], b: &[u16]) -> i32 {
    let o = options(spec);
    let (x, y) = (elems(a, &o), elems(b, &o));
    let case = |e: &Ce| if o.case_first == 2 { 2 - e.case } else { e.case };
    let mut r = cmp_level(&x, &y, |e| (e.p != 0).then_some(e.p));
    if r == Ordering::Equal && (o.level == 2 || o.level == 4) {
        r = cmp_level(&x, &y, |e| (e.s != 0).then_some(e.s));
    }
    if r == Ordering::Equal && o.level == 3 {
        r = cmp_level(&x, &y, |e| (e.p != 0).then_some(case(e)));
    }
    if r == Ordering::Equal && o.level == 4 {
        r = cmp_level(&x, &y, |e| {
            (e.t != 0).then_some(if o.case_first == 0 { (0, e.t) } else { (case(e), e.t) })
        });
    }
    match r {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// The Unicode extension keywords (`-u-kn-true`) of a language tag, with their values.
pub(super) fn unicode_keywords(tag: &str) -> Vec<(&str, String)> {
    let mut out: Vec<(&str, String)> = Vec::new();
    let subtags = tag.split('-').skip_while(|s| !s.eq_ignore_ascii_case("u")).skip(1);
    for s in subtags.take_while(|s| s.len() != 1) {
        if s.len() == 2 {
            out.push((s, String::new()));
        } else if let Some((_, v)) = out.last_mut() {
            if !v.is_empty() {
                v.push('-');
            }
            v.push_str(s);
        }
    }
    out
}

/// ResolveLocale for Intl.Collator: `tags` are the requested locales, `locale` the one
/// chosen from them.
/// Whether a zh-Hant collator was made in this process (ICU's data cache, which changes what
/// zh-u-co-stroke reports).
static ZH_HANT_LOADED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Whether a zh locale is Traditional Chinese.
fn is_hant(locale: &str) -> bool {
    let base: Vec<&str> = locale.split("-u-").next().unwrap_or("").split('-').collect();
    base.contains(&"Hant") || (!base.contains(&"Hans") && base.iter().any(|s| matches!(*s, "TW" | "HK" | "MO")))
}

/// The zh locale ICU has collation data for: a region only with its own script.
fn zh_available(locale: &str) -> String {
    let base: Vec<&str> = locale.split('-').collect();
    match base.get(1).copied() {
        Some(script) if script.len() == 4 => {
            let region = base.get(2).copied().unwrap_or("");
            match (script, region) {
                ("Hans", "CN" | "SG") | ("Hant", "HK" | "TW" | "MO") => alloc::format!("zh-{script}-{region}"),
                ("Hans" | "Hant", _) => alloc::format!("zh-{script}"),
                _ => String::from("zh"),
            }
        }
        _ => String::from(locale),
    }
}

/// The regional locales ICU has their own collation data for (outside zh); every other locale
/// resolves to its bare language, as new Intl.Collator("nl-BE").resolvedOptions().locale is "nl".
const COLLATION_REGIONS: [&str; 7] = ["en-US", "de-AT", "fr-CA", "nb-NO", "he-IL", "ar-SA", "id-ID"];

fn collation_locale(locale: &str) -> String {
    // A script subtag (en-Latn-US) has no data of its own either: it resolves to the language.
    let mut sub = locale.split('-');
    let lang = sub.next().unwrap_or("");
    let tag = alloc::format!("{lang}-{}", sub.next().unwrap_or(""));
    if COLLATION_REGIONS.contains(&tag.as_str()) { tag } else { String::from(lang) }
}

pub(super) fn resolve(locale: &str, tags: &[String], raw: &Spec) -> Result<Spec, Fault> {
    let n = "Intl.Collator";
    let zh_locale;
    let locale = if locale.split('-').next() == Some("zh") {
        zh_locale = zh_available(locale);
        zh_locale.as_str()
    } else {
        zh_locale = collation_locale(locale);
        zh_locale.as_str()
    };
    let usage = raw.choose("usage", &["sort", "search"], "sort", n)?;
    let sensitivity = raw.choose("sensitivity", &["base", "accent", "case", "variant"], "variant", n)?;
    let lang = locale.split('-').next();
    // th ignores punctuation by default, but not in its search collation.
    let ignore = raw.get("ignorePunctuation").map_or(lang == Some("th") && raw.get("usage") != Some("search"), |v| v == "true");
    let keywords = tags.iter().find(|t| t.split('-').next() == lang).map(|t| unicode_keywords(t)).unwrap_or_default();
    let keyword = |k: &str| keywords.iter().find(|(key, _)| *key == k).map(|(_, v)| v.as_str());
    // The locale keeps a keyword whose value is supported and that no option overrides.
    let mut kept: Vec<String> = Vec::new();
    let kn = match keyword("kn") {
        Some("" | "true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    let numeric = match (raw.get("numeric"), kn) {
        (Some(o), Some(k)) if (o == "true") != k => o == "true",
        (Some(o), None) => o == "true",
        (_, Some(k)) => {
            kept.push(String::from(if k { "kn" } else { "kn-false" }));
            k
        }
        (None, None) => false,
    };
    let kf = keyword("kf").filter(|v| matches!(*v, "upper" | "lower" | "false"));
    let case_first = match (raw.get("caseFirst"), kf) {
        (Some(_), _) => {
            let o = raw.choose("caseFirst", &["upper", "lower", "false"], "false", n)?;
            if kf == Some(o) {
                kept.push(alloc::format!("kf-{o}"));
            }
            o
        }
        (None, Some(k)) => {
            kept.push(alloc::format!("kf-{k}"));
            k
        }
        // Danish sorts uppercase first by default (CLDR's [caseFirst upper]).
        (None, None) if lang == Some("da") && usage == "sort" => "upper",
        (None, None) => "false",
    };
    kept.sort();
    let locale = if kept.is_empty() { String::from(locale) } else { alloc::format!("{locale}-u-{}", kept.join("-")) };
    let locale = locale.as_str();
    if let Some(c) = raw.get("collation")
        && (c.len() < 3 || c.len() > 8 || !c.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return range(&alloc::format!("Invalid collation : {c}"));
    }
    // co: phonebk for de and trad for es, from the option or else the keyword; the locale keeps it.
    let co = |c: &&str| {
        matches!(
            (lang, *c),
            (Some("de"), "phonebk") | (Some("es"), "trad") | (Some("zh"), "pinyin" | "stroke" | "zhuyin" | "unihan") | (Some("ja"), "unihan")
        )
    };
    // usage: "search" selects the search collation, which drops co (ICU reports default).
    let chosen = if usage == "search" {
        None
    } else {
        match raw.get("collation") {
            Some(c) => Some(c).filter(co).map(|c| (c, false)),
            None => keyword("co").filter(co).map(|c| (c, true)),
        }
    };
    // zh's own default (pinyin, or stroke for Traditional) reports default, and the locale keeps it
    // only from a keyword.
    let hant = lang == Some("zh") && is_hant(locale);
    let zh_default = if hant { "stroke" } else { "pinyin" };
    let (collation, tag_co) = match chosen {
        None => ("default", None),
        Some((c, from_keyword)) if lang == Some("zh") && c == zh_default => ("default", from_keyword.then_some(c)),
        // ICU reports stroke as zh's default once it has loaded zh-Hant's data, whose default it is.
        Some(("stroke", true)) if lang == Some("zh") && ZH_HANT_LOADED.load(core::sync::atomic::Ordering::Relaxed) => ("default", Some("stroke")),
        Some((c, _)) => (c, Some(c)),
    };
    if hant && matches!(chosen, None | Some(("stroke", _))) {
        ZH_HANT_LOADED.store(true, core::sync::atomic::Ordering::Relaxed);
    }
    let with_co;
    let locale = match tag_co {
        None => locale,
        Some(c) => {
            with_co = if locale.contains("-u-") { alloc::format!("{locale}-co-{c}") } else { alloc::format!("{locale}-u-co-{c}") };
            with_co.as_str()
        }
    };
    let mut out = Spec::default();
    out.put("locale", locale);
    out.put("usage", usage);
    out.put("sensitivity", sensitivity);
    out.put("ignorePunctuation", if ignore { "!t" } else { "!f" });
    out.put("collation", collation);
    out.put("numeric", if numeric { "!t" } else { "!f" });
    out.put("caseFirst", case_first);
    Ok(out)
}
