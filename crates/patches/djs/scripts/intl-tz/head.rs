use alloc::format;
use alloc::string::String;

const START: i64 = -5_364_662_400;

#[derive(Clone, Copy)]
pub(super) struct Kind {
    pub off: i64,
    pub dst: bool,
    pub tuple: u16,
}

pub(super) enum At {
    Period(Kind, Option<(i64, Option<Kind>)>),
    Rule,
}

fn varint(b: &[u8], i: &mut usize) -> u64 {
    let (mut v, mut s) = (0u64, 0);
    loop {
        let x = b[*i];
        *i += 1;
        v |= u64::from(x & 0x7f) << s;
        if x < 0x80 {
            return v;
        }
        s += 7;
    }
}

fn unzz(v: u64) -> i64 {
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

pub(super) fn at(zone: usize, t: i64) -> At {
    let b = &TZ_HISTORY[..];
    let mut i = TZ_INDEX[zone] as usize;
    let n = varint(b, &mut i) as usize;
    let mut types = [Kind {
        off: 0,
        dst: false,
        tuple: 0,
    }; TZ_MAX_TYPES];
    for k in types.iter_mut().take(n) {
        let o = varint(b, &mut i);
        let off = if o & 1 == 0 {
            unzz(o >> 1) * 60
        } else {
            unzz(o >> 1)
        };
        let x = varint(b, &mut i);
        *k = Kind {
            off,
            dst: x & 1 == 1,
            tuple: (x >> 1) as u16,
        };
    }
    let c = varint(b, &mut i) as usize;
    let np = varint(b, &mut i) as usize;
    let (mut ds, mut tys) = ([0i64; 8], [0usize; 8]);
    let (mut prev, mut cur) = (START, 0usize);
    for p in 0..np {
        let v = varint(b, &mut i);
        let r = p % c;
        let d = unzz(v >> 3) * [3600, 60, 1, 1][((v >> 1) & 3) as usize]
            + if p >= c { ds[r] } else { 0 };
        let ty = if v & 1 == 1 {
            tys[r]
        } else {
            varint(b, &mut i) as usize
        };
        ds[r] = d;
        tys[r] = ty;
        let tp = prev + d;
        if tp > t {
            return At::Period(
                types[cur],
                Some((tp, if ty < n { Some(types[ty]) } else { None })),
            );
        }
        if ty == n {
            return At::Rule;
        }
        prev = tp;
        cur = ty;
    }
    At::Period(types[cur], None)
}

pub(super) fn tuple(n: u16) -> [u16; 6] {
    tuple_at(n).0
}

fn tuple_at(n: u16) -> ([u16; 6], usize) {
    let b = &TZ_TUPLES[..];
    let mut i = TZ_TUPLE_AT[usize::from(n) - 1] as usize;
    let h = b[i];
    i += 1;
    let mut next = || varint(b, &mut i) as u16;
    let mut r = [0u16; 6];
    r[0] = if h & 1 != 0 { 0 } else { next() };
    r[1] = if h & 2 != 0 { 1 } else { next() };
    r[2] = next();
    r[3] = if h & 4 != 0 { r[2] } else { next() };
    if h & 8 != 0 {
        r[4] = r[2];
        r[5] = r[3];
    } else {
        r[4] = next();
        r[5] = next();
    }
    (r, i)
}

/// Where a locale reads element e (short, long, shortGeneric, longGeneric, the repeated-hour pair) of
/// tuple n's names from.
pub(super) enum Src {
    /// Slot k of zone R's current names (DateLocale.zones).
    Slot(usize, usize),
    /// The locale's GMT format.
    Gmt,
    /// The zone's own current names.
    Own,
    /// String id s, as name reads it.
    Str(u16),
}

/// The source of element e of tuple n's names in locale TZ_LOCALES[locale], and the string id of a suffix
/// to append (a partial location such as " (Tunesien)").
pub(super) fn locale_src(n: u16, locale: usize, e: usize) -> (Src, Option<u16>) {
    let (en, mut i) = tuple_at(n);
    let b = &TZ_TUPLES[..];
    let r = varint(b, &mut i) as usize;
    let dflt = u64::from((b[i + e / 2] >> (4 * (e % 2))) & 15);
    let mut code = dflt;
    i += 3;
    for _ in 0..varint(b, &mut i) {
        let k = varint(b, &mut i) as usize;
        let c = varint(b, &mut i);
        if k == locale * 6 + e {
            code = c;
        }
    }
    let mut suffix = None;
    if code >= 11 && (code - 11) % 2 == 1 {
        suffix = Some(((code - 11) / 2) as u16);
        code = dflt;
    }
    let src = match code {
        0..=7 if r > 0 => Src::Slot(r - 1, code as usize),
        0..=8 => Src::Gmt,
        9 => Src::Own,
        10 => Src::Str(en[e]),
        _ => Src::Str(((code - 11) / 2) as u16),
    };
    (src, suffix)
}

pub(super) fn gmt(off: i64, long: bool) -> String {
    let sign = if off < 0 { '-' } else { '+' };
    let (h, m, s) = (off.abs() / 3600, off.abs() / 60 % 60, off.abs() % 60);
    let secs = if s != 0 {
        format!(":{s:02}")
    } else {
        String::new()
    };
    if long {
        format!("GMT{sign}{h:02}:{m:02}{secs}")
    } else if m != 0 || s != 0 {
        format!("GMT{sign}{h}:{m:02}{secs}")
    } else {
        format!("GMT{sign}{h}")
    }
}

pub(super) fn name(id: u16, off: i64) -> String {
    match id {
        0 | 1 => gmt(off, id == 1),
        _ => {
            let k = usize::from(id) - 2;
            let block = k / 32 * 32;
            let start = TZ_STR_AT[k / 32] as usize + TZ_STR_LEN[block..k].iter().map(|n| usize::from(*n)).sum::<usize>();
            let end = start + usize::from(TZ_STR_LEN[k]);
            let mut out = String::new();
            for ch in TZ_STR_TEXT[start..end].chars() {
                match TZ_WORDS.get((ch as usize).wrapping_sub(1)) {
                    Some(w) if (ch as u32) < 32 => out.push_str(w),
                    _ => out.push(ch),
                }
            }
            out
        }
    }
}
