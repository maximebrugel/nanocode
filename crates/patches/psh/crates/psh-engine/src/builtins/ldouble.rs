//! printf's floating-point arguments as bash has them: C's `long double`, which on
//! x86-64 Linux is the x87 80-bit format (a 64-bit mantissa with an explicit integer
//! bit and a 15-bit exponent). `strtold` converts an argument to the nearest such
//! value and glibc prints its exact decimal (or hexadecimal) digits, so `%f` of
//! `1e400` prints 401 digits and `%.20f` of `0.1` prints `0.10000000000000000000`.

use std::cmp::Ordering;

/// The exponent of the smallest subnormal: a value is `mant * 2^exp`.
const MIN_EXP: i64 = -16445;
/// The largest `exp` of a finite value (mantissa `2^64 - 1`).
const MAX_EXP: i64 = 16320;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Ld {
    Nan(bool),
    Inf(bool),
    /// Sign, mantissa and binary exponent: `mant * 2^exp`. A normal value has the
    /// mantissa's top bit set; a subnormal has `exp == MIN_EXP`.
    Fin(bool, u64, i64),
}

/// What `strtold` made of an argument: the value, how many bytes it converted
/// (0 when none) and whether it set `ERANGE` (overflow, or an inexact result below
/// the normal range).
pub(super) struct Parsed {
    pub value: Ld,
    pub used: usize,
    pub erange: bool,
}

impl Ld {
    pub(super) fn from_u64(v: u64) -> Ld {
        if v == 0 {
            return Ld::Fin(false, 0, 0);
        }
        let z = v.leading_zeros();
        Ld::Fin(false, v << z, -i64::from(z))
    }

    pub(super) fn neg(self) -> bool {
        match self {
            Ld::Nan(n) | Ld::Inf(n) | Ld::Fin(n, ..) => n,
        }
    }

    pub(super) fn is_finite(self) -> bool {
        matches!(self, Ld::Fin(..))
    }
}

/// A little-endian unsigned big integer in base 2^32.
#[derive(Clone, Debug)]
struct Big(Vec<u32>);

impl Big {
    fn from_u64(v: u64) -> Big {
        let mut b = Big(vec![v as u32, (v >> 32) as u32]);
        b.trim();
        b
    }
    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }
    fn is_zero(&self) -> bool {
        self.0.is_empty()
    }
    fn bits(&self) -> i64 {
        match self.0.last() {
            None => 0,
            Some(&top) => (self.0.len() as i64) * 32 - i64::from(top.leading_zeros()),
        }
    }
    fn mul_small(&mut self, m: u32) {
        let mut carry = 0u64;
        for d in &mut self.0 {
            let v = u64::from(*d) * u64::from(m) + carry;
            *d = v as u32;
            carry = v >> 32;
        }
        if carry != 0 {
            self.0.push(carry as u32);
        }
    }
    fn add_small(&mut self, a: u32) {
        let mut carry = u64::from(a);
        for d in &mut self.0 {
            if carry == 0 {
                return;
            }
            let v = u64::from(*d) + carry;
            *d = v as u32;
            carry = v >> 32;
        }
        if carry != 0 {
            self.0.push(carry as u32);
        }
    }
    fn mul_pow(&mut self, base: u32, mut n: u64) {
        // The largest power of base that fits in a u32, applied in steps.
        let mut step = 1u32;
        let mut k = 0;
        while let Some(v) = step.checked_mul(base) {
            step = v;
            k += 1;
        }
        while n >= k {
            self.mul_small(step);
            n -= k;
        }
        for _ in 0..n {
            self.mul_small(base);
        }
    }
    fn shl(&self, n: u64) -> Big {
        if self.is_zero() {
            return self.clone();
        }
        let words = (n / 32) as usize;
        let bits = (n % 32) as u32;
        let mut out = vec![0u32; words];
        let mut carry = 0u32;
        for &d in &self.0 {
            if bits == 0 {
                out.push(d);
            } else {
                out.push((d << bits) | carry);
                carry = d >> (32 - bits);
            }
        }
        if carry != 0 {
            out.push(carry);
        }
        Big(out)
    }
    fn cmp(&self, o: &Big) -> Ordering {
        self.0
            .len()
            .cmp(&o.0.len())
            .then_with(|| self.0.iter().rev().cmp(o.0.iter().rev()))
    }
    fn sub(&mut self, o: &Big) {
        let mut borrow = 0i64;
        for i in 0..self.0.len() {
            let v = i64::from(self.0[i]) - i64::from(o.0.get(i).copied().unwrap_or(0)) - borrow;
            if v < 0 {
                self.0[i] = (v + (1i64 << 32)) as u32;
                borrow = 1;
            } else {
                self.0[i] = v as u32;
                borrow = 0;
            }
        }
        self.trim();
    }
    /// Divides by a small number in place, returning the remainder.
    fn div_small(&mut self, d: u32) -> u32 {
        let mut rem = 0u64;
        for x in self.0.iter_mut().rev() {
            let v = (rem << 32) | u64::from(*x);
            *x = (v / u64::from(d)) as u32;
            rem = v % u64::from(d);
        }
        self.trim();
        rem as u32
    }
    fn to_decimal(&self) -> Vec<u8> {
        if self.is_zero() {
            return b"0".to_vec();
        }
        let mut n = self.clone();
        let mut chunks = Vec::new();
        while !n.is_zero() {
            chunks.push(n.div_small(1_000_000_000));
        }
        let mut out = chunks.pop().unwrap_or(0).to_string().into_bytes();
        for c in chunks.iter().rev() {
            out.extend_from_slice(format!("{c:09}").as_bytes());
        }
        out
    }
}

/// The quotient (below 2^66 here) and remainder of `n / d`.
fn divmod(mut n: Big, d: &Big) -> (u128, Big) {
    let mut q = 0u128;
    let mut bit = (n.bits() - d.bits()).max(-1);
    while bit >= 0 {
        let sd = d.shl(bit as u64);
        if n.cmp(&sd) != Ordering::Less {
            n.sub(&sd);
            q |= 1u128 << bit;
        }
        bit -= 1;
    }
    (q, n)
}

/// `num / den * 2^e2` rounded to the nearest long double, ties to even, and
/// whether that set `ERANGE`.
fn round_ld(neg: bool, num: &Big, den: &Big, e2: i64) -> (Ld, bool) {
    if num.is_zero() {
        return (Ld::Fin(neg, 0, 0), false);
    }
    // The value's binary magnitude, within one.
    let mag = num.bits() - den.bits() + e2;
    if mag > MAX_EXP + 66 {
        return (Ld::Inf(neg), true);
    }
    if mag < MIN_EXP - 2 {
        return (Ld::Fin(neg, 0, 0), true);
    }
    // q = num * 2^s / den, so the value is q * 2^(e2 - s).
    let divide = |s: i64| -> (u128, Big, Big) {
        let (n, d) = if s >= 0 {
            (num.shl(s as u64), den.clone())
        } else {
            (num.clone(), den.shl((-s) as u64))
        };
        let (q, r) = divmod(n, &d);
        (q, r, d)
    };
    // Pick s so q has 64 bits, or fewer for a subnormal.
    let mut s = 64 - (num.bits() - den.bits());
    let (mut q, mut r, mut d) = divide(s);
    if q >= 1u128 << 64 {
        s -= 1;
        (q, r, d) = divide(s);
    } else if q < 1u128 << 63 {
        s += 1;
        (q, r, d) = divide(s);
    }
    let mut exp = e2 - s;
    if exp < MIN_EXP {
        s -= MIN_EXP - exp;
        exp = MIN_EXP;
        (q, r, d) = divide(s);
    }
    let inexact = !r.is_zero();
    let up = match r.shl(1).cmp(&d) {
        Ordering::Greater => true,
        Ordering::Equal => q & 1 == 1,
        Ordering::Less => false,
    };
    if up {
        q += 1;
        if q == 1u128 << 64 {
            q >>= 1;
            exp += 1;
        }
    }
    if exp > MAX_EXP {
        return (Ld::Inf(neg), true);
    }
    let tiny = q < 1u128 << 63;
    let exp = if q == 0 { 0 } else { exp };
    (Ld::Fin(neg, q as u64, exp), tiny && inexact)
}

/// Converts the longest prefix of `t` that `strtold` accepts: leading blanks, a
/// sign, then a decimal or `0x` hexadecimal number with its exponent, `inf`,
/// `infinity`, `nan` or `nan(chars)`.
pub(super) fn strtold(t: &[u8]) -> Parsed {
    let mut i = 0;
    while t
        .get(i)
        .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
    {
        i += 1;
    }
    let neg = match t.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let rest = &t[i..];
    let lower: Vec<u8> = rest.iter().take(8).map(u8::to_ascii_lowercase).collect();
    if lower.starts_with(b"inf") {
        let n = if lower.starts_with(b"infinity") { 8 } else { 3 };
        return Parsed {
            value: Ld::Inf(neg),
            used: i + n,
            erange: false,
        };
    }
    if lower.starts_with(b"nan") {
        let mut n = 3;
        if rest.get(3) == Some(&b'(') {
            let mut j = 4;
            while rest
                .get(j)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                j += 1;
            }
            if rest.get(j) == Some(&b')') {
                n = j + 1;
            }
        }
        return Parsed {
            value: Ld::Nan(neg),
            used: i + n,
            erange: false,
        };
    }
    let hex = rest.len() > 1 && rest[0] == b'0' && matches!(rest[1], b'x' | b'X');
    let (radix, start) = if hex { (16u32, 2) } else { (10u32, 0) };
    // Digits past CAP significant ones only break ties.
    const CAP: usize = 20_000;
    let mut j = start;
    let mut num = Big(Vec::new());
    let mut ndig = 0usize;
    let mut frac_digits = 0i64;
    let mut dropped = 0i64;
    let mut sticky = false;
    let mut any = false;
    let mut seen_point = false;
    loop {
        match rest.get(j) {
            Some(&c) if (c as char).is_digit(radix) => {
                any = true;
                let d = (c as char).to_digit(radix).unwrap_or(0);
                if ndig < CAP {
                    if !(num.is_zero() && d == 0) {
                        ndig += 1;
                    }
                    num.mul_small(radix);
                    num.add_small(d);
                    if seen_point {
                        frac_digits += 1;
                    }
                } else {
                    sticky |= d != 0;
                    if !seen_point {
                        dropped += 1;
                    }
                }
                j += 1;
            }
            Some(b'.') if !seen_point => {
                seen_point = true;
                j += 1;
            }
            _ => break,
        }
    }
    if !any {
        // `0x` with no digits converts its `0`.
        return Parsed {
            value: Ld::Fin(hex && neg, 0, 0),
            used: if hex { i + 1 } else { 0 },
            erange: false,
        };
    }
    let mut exp: i64 = 0;
    let marker: &[u8] = if hex { b"pP" } else { b"eE" };
    if rest.get(j).is_some_and(|c| marker.contains(c)) {
        let mut k = j + 1;
        let eneg = match rest.get(k) {
            Some(b'-') => {
                k += 1;
                true
            }
            Some(b'+') => {
                k += 1;
                false
            }
            _ => false,
        };
        let ds = k;
        let mut e: i64 = 0;
        while let Some(c) = rest.get(k).filter(|c| c.is_ascii_digit()) {
            e = (e * 10 + i64::from(c - b'0')).min(1 << 40);
            k += 1;
        }
        if k > ds {
            exp = if eneg { -e } else { e };
            j = k;
        }
    }
    if sticky {
        num.mul_small(radix);
        num.add_small(1);
        frac_digits += 1;
    }
    let one = Big::from_u64(1);
    let (value, erange) = if num.is_zero() {
        (Ld::Fin(neg, 0, 0), false)
    } else if hex {
        round_ld(neg, &num, &one, exp + 4 * (dropped - frac_digits))
    } else {
        let e = exp + dropped - frac_digits;
        let digits = num.to_decimal().len() as i64;
        if digits + e > 4934 {
            (Ld::Inf(neg), true)
        } else if digits + e < -4952 {
            (Ld::Fin(neg, 0, 0), true)
        } else if e >= 0 {
            let mut n = num;
            n.mul_pow(10, e as u64);
            round_ld(neg, &n, &one, 0)
        } else {
            let mut d = one;
            d.mul_pow(10, (-e) as u64);
            round_ld(neg, &num, &d, 0)
        }
    };
    Parsed {
        value,
        used: i + j,
        erange,
    }
}

/// The exact decimal digits of a finite value's magnitude and where the point goes:
/// the value is `0.DIGITS * 10^point`.
fn decimal(mant: u64, exp: i64) -> (Vec<u8>, i64) {
    if mant == 0 {
        return (b"0".to_vec(), 1);
    }
    let mut n = Big::from_u64(mant);
    if exp >= 0 {
        let d = n.shl(exp as u64).to_decimal();
        let p = d.len() as i64;
        (d, p)
    } else {
        n.mul_pow(5, (-exp) as u64);
        let d = n.to_decimal();
        let p = d.len() as i64 + exp;
        (d, p)
    }
}

/// Rounds `0.digits * 10^point` to its first `keep` digits, ties to even: the kept
/// digits (one more after a carry, which also moves the point) and the new point.
fn round_digits(digits: &[u8], point: i64, keep: i64) -> (Vec<u8>, i64) {
    if keep < 0 {
        return (Vec::new(), point);
    }
    let k = keep as usize;
    if k >= digits.len() {
        let mut d = digits.to_vec();
        d.resize(k, b'0');
        return (d, point);
    }
    let mut d = digits[..k].to_vec();
    let up = match digits[k].cmp(&b'5') {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => {
            digits[k + 1..].iter().any(|&c| c != b'0')
                || d.last().is_some_and(|c| (c - b'0') % 2 == 1)
        }
    };
    if up {
        let mut i = d.len();
        loop {
            if i == 0 {
                d.insert(0, b'1');
                return (d, point + 1);
            }
            i -= 1;
            if d[i] == b'9' {
                d[i] = b'0';
            } else {
                d[i] += 1;
                break;
            }
        }
    }
    (d, point)
}

/// `%f`: the magnitude with `prec` decimals (`alt` keeps the point).
fn fixed(mant: u64, exp: i64, prec: usize, alt: bool) -> String {
    let (digits, point) = decimal(mant, exp);
    let (d, point) = round_digits(&digits, point, point + prec as i64);
    // All prec decimals and the integer digits, as one string of digits.
    let mut all = Vec::new();
    if point <= 0 {
        all.push(b'0');
        all.extend(std::iter::repeat_n(b'0', ((-point) as usize).min(prec)));
        all.extend_from_slice(&d);
    } else {
        all.extend_from_slice(&d);
    }
    let int_len = point.max(1) as usize;
    all.resize(int_len + prec, b'0');
    let (int, frac) = all.split_at(int_len);
    let int = String::from_utf8_lossy(int);
    let frac = String::from_utf8_lossy(frac);
    if prec > 0 {
        format!("{int}.{frac}")
    } else if alt {
        format!("{int}.")
    } else {
        int.into_owned()
    }
}

/// `%e`: `d.ddde+XX` with `prec` decimals.
fn sci(mant: u64, exp: i64, prec: usize, alt: bool) -> String {
    let (digits, point) = decimal(mant, exp);
    let (d, point) = round_digits(&digits, point, prec as i64 + 1);
    let x = if mant == 0 { 0 } else { point - 1 };
    let mut s = String::new();
    s.push(d[0] as char);
    if prec > 0 || alt {
        s.push('.');
    }
    s.push_str(&String::from_utf8_lossy(&d[1..=prec]));
    let sign = if x < 0 { '-' } else { '+' };
    format!("{s}e{sign}{:02}", x.abs())
}

/// `%g`: `%e` or `%f` with `prec` significant digits, trailing zeros removed unless `#`.
fn general(mant: u64, exp: i64, prec: usize, alt: bool) -> String {
    let p = prec.max(1);
    let x = if mant == 0 {
        0
    } else {
        let (digits, point) = decimal(mant, exp);
        round_digits(&digits, point, p as i64).1 - 1
    };
    let out = if x < -4 || x >= p as i64 {
        sci(mant, exp, p - 1, alt)
    } else {
        fixed(mant, exp, (p as i64 - 1 - x) as usize, alt)
    };
    if alt {
        return out;
    }
    let (num, suffix) = out.split_at(out.find('e').unwrap_or(out.len()));
    let num = if num.contains('.') {
        num.trim_end_matches('0').trim_end_matches('.')
    } else {
        num
    };
    format!("{num}{suffix}")
}

/// `%a` the way glibc prints an x87 value: the mantissa's top four bits before the
/// point (`0xc.ccccccccccccccdp-7` for 0.1).
fn hexf(mant: u64, exp: i64, prec: Option<usize>, alt: bool) -> String {
    if mant == 0 {
        let p = prec.unwrap_or(0);
        let point = if p > 0 || alt { "." } else { "" };
        return format!("0x0{point}{}p+0", "0".repeat(p));
    }
    let mut lead = mant >> 60;
    let mut frac = mant & ((1u64 << 60) - 1);
    let mut e = exp + 60;
    let mut nibbles = 15usize;
    if let Some(p) = prec
        && p < 15
    {
        let drop = 4 * (15 - p) as u32;
        let rem = frac & ((1u64 << drop) - 1);
        let half = 1u64 << (drop - 1);
        frac >>= drop;
        let odd = if p == 0 { lead & 1 == 1 } else { frac & 1 == 1 };
        if rem > half || (rem == half && odd) {
            frac += 1;
            if frac >> (4 * p) != 0 {
                frac = 0;
                lead += 1;
                if lead == 16 {
                    lead = 1;
                    e += 4;
                }
            }
        }
        nibbles = p;
    }
    let mut f = if nibbles == 0 {
        String::new()
    } else {
        format!("{frac:0nibbles$x}")
    };
    match prec {
        Some(p) => {
            while f.len() < p {
                f.push('0');
            }
        }
        None => {
            while f.ends_with('0') {
                f.pop();
            }
        }
    }
    let point = if !f.is_empty() || alt { "." } else { "" };
    let sign = if e < 0 { '-' } else { '+' };
    format!("0x{lead:x}{point}{f}p{sign}{}", e.abs())
}

/// A conversion's (`f F e E g G a A`) text for `v`, without its sign.
pub(super) fn format(v: Ld, conv: u8, prec: Option<usize>, alt: bool) -> String {
    let body = match v {
        Ld::Nan(_) => "nan".to_string(),
        Ld::Inf(_) => "inf".to_string(),
        Ld::Fin(_, mant, exp) => {
            let p = prec.unwrap_or(6);
            match conv.to_ascii_lowercase() {
                b'f' => fixed(mant, exp, p, alt),
                b'e' => sci(mant, exp, p, alt),
                b'g' => general(mant, exp, p, alt),
                _ => hexf(mant, exp, prec, alt),
            }
        }
    };
    if conv.is_ascii_uppercase() {
        body.to_uppercase()
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::{Ld, format, strtold};

    fn f(conv: u8, prec: Option<usize>, alt: bool, arg: &str) -> String {
        let v = strtold(arg.as_bytes()).value;
        let sign = if v.neg() { "-" } else { "" };
        format!("{sign}{}", format(v, conv, prec, alt))
    }

    #[test]
    fn long_double_digits_match_glibc() {
        assert_eq!(f(b'f', Some(20), false, "0.1"), "0.10000000000000000000");
        assert_eq!(
            f(b'f', Some(40), false, "0.1"),
            "0.1000000000000000000013552527156068805425"
        );
        assert_eq!(
            f(b'g', Some(30), false, "0.1"),
            "0.100000000000000000001355252716"
        );
        assert_eq!(f(b'e', None, false, "1e400"), "1.000000e+400");
        let big = f(b'f', Some(0), false, "1e400");
        assert!(big.starts_with("1000000000000000000028188068394758651458645"));
        assert_eq!(big.len(), 401);
        assert_eq!(
            f(b'g', Some(30), false, "1e400"),
            "1.00000000000000000002818806839e+400"
        );
        assert_eq!(f(b'e', None, false, "1e-4950"), "1.093560e-4950");
        assert_eq!(f(b'f', None, false, "1e-4950"), "0.000000");
        assert_eq!(f(b'g', None, false, "0x1p-16445"), "3.6452e-4951");
        assert_eq!(
            f(b'f', None, false, "123456789012345678901234567890"),
            "123456789012345678899921813504.000000"
        );
        assert_eq!(f(b'f', Some(0), false, "2.5"), "2");
        assert_eq!(f(b'f', Some(0), false, "0.5"), "0");
        assert_eq!(f(b'f', Some(0), false, "1.5"), "2");
        assert_eq!(f(b'f', Some(1), false, "0.05"), "0.1");
        assert_eq!(f(b'f', Some(0), true, "0.1"), "0.");
        assert_eq!(f(b'f', Some(2), false, "-0.004"), "-0.00");
        assert_eq!(f(b'f', Some(2), false, "99.999"), "100.00");
        assert_eq!(f(b'e', Some(0), false, "15"), "2e+01");
        assert_eq!(f(b'e', Some(3), false, "1.0005"), "1.001e+00");
        assert_eq!(f(b'e', None, false, "0"), "0.000000e+00");
        assert_eq!(f(b'g', None, true, "1"), "1.00000");
        assert_eq!(f(b'g', Some(3), true, "100"), "100.");
        assert_eq!(f(b'g', None, false, "1e-5"), "1e-05");
        assert_eq!(f(b'g', None, false, "100000"), "100000");
        assert_eq!(f(b'G', None, false, "1e6"), "1E+06");
        assert_eq!(f(b'g', None, false, "0"), "0");
        assert_eq!(f(b'f', None, false, "-0"), "-0.000000");
    }

    #[test]
    fn hex_conversion_matches_glibc() {
        assert_eq!(f(b'a', None, false, "1"), "0x8p-3");
        assert_eq!(f(b'a', None, false, "0.1"), "0xc.ccccccccccccccdp-7");
        assert_eq!(f(b'a', Some(3), false, "0.1"), "0xc.ccdp-7");
        assert_eq!(f(b'a', Some(0), false, "0.1"), "0xdp-7");
        assert_eq!(f(b'a', Some(0), true, "0.1"), "0xd.p-7");
        assert_eq!(f(b'a', None, false, "255"), "0xf.fp+4");
        assert_eq!(f(b'a', Some(1), false, "0x1.ffp0"), "0x1.0p+1");
        assert_eq!(f(b'a', Some(2), false, "0x1.08p0"), "0x8.40p-3");
        assert_eq!(
            f(b'a', None, false, "1e-4950"),
            "0x0.000000000000003p-16385"
        );
        assert_eq!(f(b'a', None, false, "0"), "0x0p+0");
        assert_eq!(f(b'a', Some(3), false, "0"), "0x0.000p+0");
        assert_eq!(f(b'a', None, false, "0xffffffffffffffffff"), "0x8p+69");
        assert_eq!(f(b'a', None, false, "1e4932"), "0xd.72cb2a95c7ef6cdp+16380");
        assert_eq!(
            f(b'A', Some(3), false, "1.1897314953572317650e4932"),
            "0X1.000P+16384"
        );
    }

    #[test]
    fn strtold_ranges_and_specials() {
        let p = strtold(b"1.18973149535723176508e4932");
        assert!(matches!(p.value, Ld::Inf(false)) && p.erange);
        let p = strtold(b"1.1897314953572317650e4932");
        assert!(p.value.is_finite() && !p.erange);
        assert!(strtold(b"1e-4950").erange);
        assert!(!strtold(b"0x1p-16445").erange);
        let p = strtold(b"0x1p-16446");
        assert!(p.erange && p.value == Ld::Fin(false, 0, 0));
        assert!(strtold(b"1e-5000").erange);
        assert!(!strtold(b"1e-400").erange);
        assert_eq!(strtold(b"-nan").value, Ld::Nan(true));
        assert_eq!(strtold(b"nan(12)").used, 7);
        assert_eq!(strtold(b"nan(").used, 3);
        assert_eq!(strtold(b"infinit").used, 3);
        assert_eq!(strtold(b" 1").used, 2);
        assert_eq!(strtold(b"1e+").used, 1);
        assert_eq!(strtold(b"0x").used, 1);
        assert_eq!(strtold(b"0x.p1").used, 1);
        assert_eq!(strtold(b".").used, 0);
        assert_eq!(strtold(b"-n").value, Ld::Fin(false, 0, 0));
        assert_eq!(strtold(b"-0x").value, Ld::Fin(true, 0, 0));
        assert_eq!(strtold(b"").used, 0);
        assert_eq!(f(b'f', None, false, "0x1.fffffffffffffffffp0"), "2.000000");
        assert_eq!(f(b'f', None, false, "0x3p-16447"), "0.000000");
        assert_eq!(
            f(b'a', None, false, "0x3p-16447"),
            "0x0.000000000000001p-16385"
        );
    }
}
