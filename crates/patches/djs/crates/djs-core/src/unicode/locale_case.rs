//! Language-sensitive case mapping for `toLocaleUpperCase` / `toLocaleLowerCase`, as ICU
//! does it for the languages V8 hands to ICU (tr, az, lt, el): the conditional mappings of
//! SpecialCasing.txt for Turkish/Azeri and Lithuanian, and ICU's Greek uppercasing, which
//! drops accents and breathings (GreekUpper in ICU's ustrcase.cpp).

use alloc::vec::Vec;

use super::case;
use super::normalize::ccc;

/// Which language's rules apply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    /// tr and az.
    Turkic,
    Lithuanian,
    Greek,
}

const HAS_VOWEL: u32 = 0x1000;
const HAS_YPOGEGRAMMENI: u32 = 0x2000;
const HAS_ACCENT: u32 = 0x4000;
const HAS_DIALYTIKA: u32 = 0x8000;
const HAS_COMBINING_DIALYTIKA: u32 = 0x10000;
const HAS_OTHER_GREEK_DIACRITIC: u32 = 0x20000;
const HAS_VOWEL_AND_ACCENT: u32 = HAS_VOWEL | HAS_ACCENT;
const HAS_VOWEL_AND_ACCENT_AND_DIALYTIKA: u32 = HAS_VOWEL_AND_ACCENT | HAS_DIALYTIKA;
const HAS_EITHER_DIALYTIKA: u32 = HAS_DIALYTIKA | HAS_COMBINING_DIALYTIKA;
const UPPER_MASK: u32 = 0x3ff;
const AFTER_CASED: u8 = 1;
const AFTER_VOWEL_WITH_COMBINING_ACCENT: u8 = 2;
const AFTER_VOWEL_WITH_PRECOMPOSED_ACCENT: u8 = 4;

fn letter_data(c: u32) -> u32 {
    if c == 0x2126 {
        return 0x03A9 | HAS_VOWEL;
    }
    match c {
        0x370..=0x3FF => DATA_0370[(c - 0x370) as usize] as u32,
        0x1F00..=0x1FFF => DATA_1F00[(c - 0x1F00) as usize] as u32,
        _ => 0,
    }
}

fn diacritic_data(c: u32) -> u32 {
    match c {
        // varia, tonos = oxia, perispomeni, and circumflex, tilde and inverted breve,
        // which can look like perispomeni
        0x300 | 0x301 | 0x342 | 0x302 | 0x303 | 0x311 => HAS_ACCENT,
        0x308 => HAS_COMBINING_DIALYTIKA,
        0x344 => HAS_COMBINING_DIALYTIKA | HAS_ACCENT,
        0x345 => HAS_YPOGEGRAMMENI,
        // macron, breve, comma above, reversed comma above, koronis
        0x304 | 0x306 | 0x313 | 0x314 | 0x343 => HAS_OTHER_GREEK_DIACRITIC,
        _ => 0,
    }
}

fn followed_by_cased(cps: &[u32]) -> bool {
    cps.iter()
        .find(|&&c| !case::is_case_ignorable(c))
        .is_some_and(|&c| case::is_cased(c))
}

fn push_full(out: &mut Vec<u32>, upper: bool, c: u32) {
    let m = if upper { case::to_upper_full(c) } else { case::to_lower_full(c) };
    out.extend_from_slice(m.as_slice());
}

fn greek_upper(cps: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(cps.len());
    let mut state = 0u8;
    let mut i = 0;
    while i < cps.len() {
        let c = cps[i];
        i += 1;
        let mut next_state = 0u8;
        if case::is_case_ignorable(c) {
            next_state |= state & AFTER_CASED;
        } else if case::is_cased(c) {
            next_state |= AFTER_CASED;
        }
        let mut data = letter_data(c);
        if data == 0 {
            push_full(&mut out, true, c);
            state = next_state;
            continue;
        }
        let mut upper = data & UPPER_MASK;
        if data & HAS_VOWEL != 0
            && state & (AFTER_VOWEL_WITH_PRECOMPOSED_ACCENT | AFTER_VOWEL_WITH_COMBINING_ACCENT) != 0
            && (upper == 0x399 || upper == 0x3A5)
        {
            data |= if state & AFTER_VOWEL_WITH_PRECOMPOSED_ACCENT != 0 {
                HAS_DIALYTIKA
            } else {
                HAS_COMBINING_DIALYTIKA
            };
        }
        let mut ypogegrammeni = (data & HAS_YPOGEGRAMMENI != 0) as usize;
        let precomposed_accent = data & HAS_ACCENT != 0;
        let mut next = i;
        while next < cps.len() {
            let d = diacritic_data(cps[next]);
            if d == 0 {
                break;
            }
            data |= d;
            if d & HAS_YPOGEGRAMMENI != 0 {
                ypogegrammeni += 1;
            }
            next += 1;
        }
        if data & HAS_VOWEL_AND_ACCENT_AND_DIALYTIKA == HAS_VOWEL_AND_ACCENT {
            next_state |= if precomposed_accent {
                AFTER_VOWEL_WITH_PRECOMPOSED_ACCENT
            } else {
                AFTER_VOWEL_WITH_COMBINING_ACCENT
            };
        }
        let mut add_tonos = false;
        if upper == 0x397
            && data & HAS_ACCENT != 0
            && ypogegrammeni == 0
            && state & AFTER_CASED == 0
            && !followed_by_cased(&cps[next..])
        {
            // Keep the disjunctive "or" (ή) with only a tonos.
            if precomposed_accent {
                upper = 0x389;
            } else {
                add_tonos = true;
            }
        } else if data & HAS_DIALYTIKA != 0 {
            if upper == 0x399 {
                upper = 0x3AA;
                data &= !HAS_EITHER_DIALYTIKA;
            } else if upper == 0x3A5 {
                upper = 0x3AB;
                data &= !HAS_EITHER_DIALYTIKA;
            }
        }
        out.push(upper);
        if data & HAS_EITHER_DIALYTIKA != 0 {
            out.push(0x308);
        }
        if add_tonos {
            out.push(0x301);
        }
        out.extend(core::iter::repeat_n(0x399, ypogegrammeni));
        i = next;
        state = next_state;
    }
    out
}

/// Whether `c` is Soft_Dotted.
fn soft_dotted(c: u32, set: &[(u32, u32)]) -> bool {
    set.iter().any(|&(a, b)| a <= c && c <= b)
}

/// The code points `cps` mapped with `lang`'s rules (and Final_Sigma when lowercasing).
pub fn map(cps: &[u32], upper: bool, lang: Lang) -> Vec<u32> {
    if lang == Lang::Greek && upper {
        return greek_upper(cps);
    }
    let soft = if lang == Lang::Lithuanian && upper {
        super::property::lookup("Soft_Dotted", "", false).map(|p| p.ranges).unwrap_or_default()
    } else {
        Vec::new()
    };
    // Whether a base before k (no ccc 0 or 230 between) satisfies `f`.
    let after = |k: usize, f: &dyn Fn(u32) -> bool| {
        for &c in cps[..k].iter().rev() {
            if f(c) {
                return true;
            }
            let cc = ccc(c);
            if cc == 0 || cc == 230 {
                return false;
            }
        }
        false
    };
    let mut out = Vec::with_capacity(cps.len());
    for (k, &c) in cps.iter().enumerate() {
        match (lang, upper) {
            (Lang::Turkic, true) if c == 0x69 => out.push(0x130),
            (Lang::Turkic, false) if c == 0x130 => out.push(0x69),
            (Lang::Turkic, false) if c == 0x307 && after(k, &|c| c == 0x49) => {}
            (Lang::Turkic, false) if c == 0x49 => {
                // Not_Before_Dot: I before U+0307 (no ccc 0 or 230 between) lowercases with
                // its dot dropped above.
                let mut dot = false;
                for &d in &cps[k + 1..] {
                    if d == 0x307 {
                        dot = true;
                        break;
                    }
                    let cc = ccc(d);
                    if cc == 0 || cc == 230 {
                        break;
                    }
                }
                out.push(if dot { 0x69 } else { 0x131 });
            }
            (Lang::Lithuanian, true) if c == 0x307 && after(k, &|c| soft_dotted(c, &soft)) => {}
            (Lang::Lithuanian, false) if matches!(c, 0xCC | 0xCD | 0x128) => {
                out.extend_from_slice(&[0x69, 0x307]);
                out.push(match c {
                    0xCC => 0x300,
                    0xCD => 0x301,
                    _ => 0x303,
                });
            }
            (Lang::Lithuanian, false) if matches!(c, 0x49 | 0x4A | 0x12E) => {
                // More_Above: a ccc 230 mark follows before any ccc 0 character.
                let mut above = false;
                for &d in &cps[k + 1..] {
                    let cc = ccc(d);
                    if cc == 230 {
                        above = true;
                    }
                    if cc == 0 || cc == 230 {
                        break;
                    }
                }
                push_full(&mut out, false, c);
                if above {
                    out.push(0x307);
                }
            }
            (_, false) if c == 0x03A3 => {
                let before = cps[..k]
                    .iter()
                    .rev()
                    .find(|&&c| !case::is_case_ignorable(c))
                    .is_some_and(|&c| case::is_cased(c));
                let after = followed_by_cased(&cps[k + 1..]);
                out.push(if before && !after { 0x03C2 } else { 0x03C3 });
            }
            _ => push_full(&mut out, upper, c),
        }
    }
    out
}

// ICU GreekUpper's letter data (ustrcase.cpp, ICU 78): the uppercase letter and the
// HAS_* flags of U+0370..03FF and U+1F00..1FFF.
const DATA_0370: [u16; 144] = [
    0x0370, 0x0370, 0x0372, 0x0372, 0x0000, 0x0000, 0x0376, 0x0376,
    0x0000, 0x0000, 0x037A, 0x03FD, 0x03FE, 0x03FF, 0x0000, 0x037F,
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x5391, 0x0000,
    0x5395, 0x5397, 0x5399, 0x0000, 0x539F, 0x0000, 0x53A5, 0x53A9,
    0xD399, 0x1391, 0x0392, 0x0393, 0x0394, 0x1395, 0x0396, 0x1397,
    0x0398, 0x1399, 0x039A, 0x039B, 0x039C, 0x039D, 0x039E, 0x139F,
    0x03A0, 0x03A1, 0x0000, 0x03A3, 0x03A4, 0x13A5, 0x03A6, 0x03A7,
    0x03A8, 0x13A9, 0x9399, 0x93A5, 0x5391, 0x5395, 0x5397, 0x5399,
    0xD3A5, 0x1391, 0x0392, 0x0393, 0x0394, 0x1395, 0x0396, 0x1397,
    0x0398, 0x1399, 0x039A, 0x039B, 0x039C, 0x039D, 0x039E, 0x139F,
    0x03A0, 0x03A1, 0x03A3, 0x03A3, 0x03A4, 0x13A5, 0x03A6, 0x03A7,
    0x03A8, 0x13A9, 0x9399, 0x93A5, 0x539F, 0x53A5, 0x53A9, 0x03CF,
    0x0392, 0x0398, 0x03D2, 0x43D2, 0x83D2, 0x03A6, 0x03A0, 0x03CF,
    0x03D8, 0x03D8, 0x03DA, 0x03DA, 0x03DC, 0x03DC, 0x03DE, 0x03DE,
    0x03E0, 0x03E0, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
    0x039A, 0x03A1, 0x03F9, 0x037F, 0x03F4, 0x1395, 0x0000, 0x03F7,
    0x03F7, 0x03F9, 0x03FA, 0x03FA, 0x03FC, 0x03FD, 0x03FE, 0x03FF,
];

const DATA_1F00: [u16; 256] = [
    0x1391, 0x1391, 0x5391, 0x5391, 0x5391, 0x5391, 0x5391, 0x5391,
    0x1391, 0x1391, 0x5391, 0x5391, 0x5391, 0x5391, 0x5391, 0x5391,
    0x1395, 0x1395, 0x5395, 0x5395, 0x5395, 0x5395, 0x0000, 0x0000,
    0x1395, 0x1395, 0x5395, 0x5395, 0x5395, 0x5395, 0x0000, 0x0000,
    0x1397, 0x1397, 0x5397, 0x5397, 0x5397, 0x5397, 0x5397, 0x5397,
    0x1397, 0x1397, 0x5397, 0x5397, 0x5397, 0x5397, 0x5397, 0x5397,
    0x1399, 0x1399, 0x5399, 0x5399, 0x5399, 0x5399, 0x5399, 0x5399,
    0x1399, 0x1399, 0x5399, 0x5399, 0x5399, 0x5399, 0x5399, 0x5399,
    0x139F, 0x139F, 0x539F, 0x539F, 0x539F, 0x539F, 0x0000, 0x0000,
    0x139F, 0x139F, 0x539F, 0x539F, 0x539F, 0x539F, 0x0000, 0x0000,
    0x13A5, 0x13A5, 0x53A5, 0x53A5, 0x53A5, 0x53A5, 0x53A5, 0x53A5,
    0x0000, 0x13A5, 0x0000, 0x53A5, 0x0000, 0x53A5, 0x0000, 0x53A5,
    0x13A9, 0x13A9, 0x53A9, 0x53A9, 0x53A9, 0x53A9, 0x53A9, 0x53A9,
    0x13A9, 0x13A9, 0x53A9, 0x53A9, 0x53A9, 0x53A9, 0x53A9, 0x53A9,
    0x5391, 0x5391, 0x5395, 0x5395, 0x5397, 0x5397, 0x5399, 0x5399,
    0x539F, 0x539F, 0x53A5, 0x53A5, 0x53A9, 0x53A9, 0x0000, 0x0000,
    0x3391, 0x3391, 0x7391, 0x7391, 0x7391, 0x7391, 0x7391, 0x7391,
    0x3391, 0x3391, 0x7391, 0x7391, 0x7391, 0x7391, 0x7391, 0x7391,
    0x3397, 0x3397, 0x7397, 0x7397, 0x7397, 0x7397, 0x7397, 0x7397,
    0x3397, 0x3397, 0x7397, 0x7397, 0x7397, 0x7397, 0x7397, 0x7397,
    0x33A9, 0x33A9, 0x73A9, 0x73A9, 0x73A9, 0x73A9, 0x73A9, 0x73A9,
    0x33A9, 0x33A9, 0x73A9, 0x73A9, 0x73A9, 0x73A9, 0x73A9, 0x73A9,
    0x1391, 0x1391, 0x7391, 0x3391, 0x7391, 0x0000, 0x5391, 0x7391,
    0x1391, 0x1391, 0x5391, 0x5391, 0x3391, 0x0000, 0x1399, 0x0000,
    0x0000, 0x0000, 0x7397, 0x3397, 0x7397, 0x0000, 0x5397, 0x7397,
    0x5395, 0x5395, 0x5397, 0x5397, 0x3397, 0x0000, 0x0000, 0x0000,
    0x1399, 0x1399, 0xD399, 0xD399, 0x0000, 0x0000, 0x5399, 0xD399,
    0x1399, 0x1399, 0x5399, 0x5399, 0x0000, 0x0000, 0x0000, 0x0000,
    0x13A5, 0x13A5, 0xD3A5, 0xD3A5, 0x03A1, 0x03A1, 0x53A5, 0xD3A5,
    0x13A5, 0x13A5, 0x53A5, 0x53A5, 0x03A1, 0x0000, 0x0000, 0x0000,
    0x0000, 0x0000, 0x73A9, 0x33A9, 0x73A9, 0x0000, 0x53A9, 0x73A9,
    0x539F, 0x539F, 0x53A9, 0x53A9, 0x33A9, 0x0000, 0x0000, 0x0000,
];
