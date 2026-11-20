//! Number to string conversion.
//!
//! Decimal shortest output uses Ryu. Fixed-point, precision and exponential output follow
//! QuickJS' `js_dtoa` (`dtoa.c`, MIT, Fabrice Bellard) with exact big integer arithmetic, so
//! the digits and the corner cases match qjs. Other radixes print what V8 prints.

use super::big::Big;
use super::exact::{Round, scale_round};
use super::ryu;
use super::{MAX_FIXED_LEN, MAX_RADIX_LEN, MAX_SHORT_LEN, NumStr, RangeError};

/// Largest number of digits accepted by toFixed / toExponential / toPrecision.
const MAX_DIGITS: i32 = 100;

/// `round(2^24 / log2(radix))` for radix 2..=36, 0 for powers of two (from qjs).
#[rustfmt::skip]
const MUL_LOG2_RADIX: [u32; 35] = [
    0x000000, 0xa1849d, 0x000000, 0x6e40d2, 0x6308c9, 0x5b3065, 0x000000, 0x50c24e,
    0x4d104d, 0x4a0027, 0x4768ce, 0x452e54, 0x433d00, 0x418677, 0x000000, 0x3ea16b,
    0x3d645a, 0x3c43c2, 0x3b3b9a, 0x3a4899, 0x39680b, 0x3897b3, 0x37d5af, 0x372069,
    0x367686, 0x35d6df, 0x354072, 0x34b261, 0x342bea, 0x33ac62, 0x000000, 0x32bfd9,
    0x3251dd, 0x31e8d6, 0x318465,
];

/// `floor(a / log2(radix))` for -2048 <= a <= 2047.
fn mul_log2_radix(a: i32, radix: u32) -> i32 {
    if radix.is_power_of_two() {
        a.div_euclid(radix.trailing_zeros() as i32)
    } else {
        ((a as i64 * MUL_LOG2_RADIX[radix as usize - 2] as i64) >> 24) as i32
    }
}

/// How the exponent notation is chosen.
#[derive(Clone, Copy)]
enum Exp {
    /// Exponential notation when the decimal point position `e` is `<= -6` or `> max`.
    Auto(i32),
    Always,
}

/// Writes `digits` (the significant digits) with the decimal point after `e` digits (so the
/// value is `0.digits * radix^e`), the way qjs' `js_dtoa` lays them out.
fn layout<const N: usize>(out: &mut NumStr<N>, digits: &[u8], e: i32, exp: Exp) {
    let p = digits.len() as i32;
    let exponential = match exp {
        Exp::Always => true,
        Exp::Auto(max) => e <= -6 || e > max,
    };
    if exponential {
        out.push(digits[0]);
        if p > 1 {
            out.push(b'.');
            out.extend(&digits[1..]);
        }
        out.push(b'e');
        let x = e - 1;
        out.push(if x < 0 { b'-' } else { b'+' });
        let mut tmp = [0u8; 20];
        out.extend(u64_digits(x.unsigned_abs() as u64, 10, &mut tmp));
    } else if e <= 0 {
        out.extend(b"0.");
        out.push_zeros((-e) as usize);
        out.extend(digits);
    } else if e < p {
        out.extend(&digits[..e as usize]);
        out.push(b'.');
        out.extend(&digits[e as usize..]);
    } else {
        out.extend(digits);
        out.push_zeros((e - p) as usize);
    }
}

/// Writes `n` in `radix` at the end of `buf` and returns the digits.
fn u64_digits(mut n: u64, radix: u32, buf: &mut [u8]) -> &[u8] {
    let mut i = buf.len();
    loop {
        i -= 1;
        let d = (n % radix as u64) as u8;
        buf[i] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
        n /= radix as u64;
        if n == 0 {
            return &buf[i..];
        }
    }
}

/// Writes the decimal digits of `b` at the end of `buf` and returns them.
fn big_decimal_digits(mut b: Big, buf: &mut [u8]) -> &[u8] {
    if b.is_zero() {
        let i = buf.len() - 1;
        buf[i] = b'0';
        return &buf[i..];
    }
    let mut i = buf.len();
    while !b.is_zero() {
        let mut chunk = b.div_small(1_000_000_000);
        let last = b.is_zero();
        for _ in 0..9 {
            i -= 1;
            buf[i] = b'0' + (chunk % 10) as u8;
            chunk /= 10;
            if last && chunk == 0 {
                break;
            }
        }
    }
    &buf[i..]
}

/// `|x|` as `m * 2^(e - 53)` with `2^52 <= m < 2^53` (subnormals are normalized, so `e` may
/// be below the normal range), as qjs does. `x` is finite and non-zero.
fn decompose(x: f64) -> (u64, i32) {
    let bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7ff) as i32;
    let mut m = bits & ((1 << 52) - 1);
    if e == 0 {
        let l = m.leading_zeros() as i32 - 11;
        e -= l - 1;
        m <<= l;
    } else {
        m |= 1 << 52;
    }
    (m, e - 1022)
}

/// `ToString(x)` for a Number: `Number::toString(x, 10)` in the spec.
///
/// `-0` gives `"0"`; a caller that wants `console.log`'s `-0` checks for it itself.
pub fn to_string(x: f64) -> NumStr<MAX_SHORT_LEN> {
    let mut out = NumStr::new();
    if non_finite_or_zero(&mut out, x) {
        return out;
    }
    if x < 0.0 {
        out.push(b'-');
    }
    let a = x.abs();
    if a < 9007199254740992.0 && (a as u64) as f64 == a {
        let mut tmp = [0u8; 20];
        out.extend(u64_digits(a as u64, 10, &mut tmp));
        return out;
    }
    let mut tmp = [0u8; 20];
    let (digits, e) = shortest(a, &mut tmp);
    layout(&mut out, digits, e, Exp::Auto(21));
    out
}

/// Handles NaN, the infinities and zero (all signs dropped for zero). Returns true if it did.
fn non_finite_or_zero<const N: usize>(out: &mut NumStr<N>, x: f64) -> bool {
    if x.is_nan() {
        out.extend(b"NaN");
    } else if x.is_infinite() {
        out.extend(if x < 0.0 {
            b"-Infinity" as &[u8]
        } else {
            b"Infinity"
        });
    } else if x == 0.0 {
        out.push(b'0');
    } else {
        return false;
    }
    true
}

/// Shortest decimal digits of a finite positive `a` and the decimal point position.
fn shortest(a: f64, tmp: &mut [u8; 20]) -> (&[u8], i32) {
    if let Some((m, k)) = short_decimal(a) {
        let digits = u64_digits(m, 10, tmp);
        return (digits, digits.len() as i32 - k);
    }
    ryu_shortest(a, tmp)
}

fn ryu_shortest(a: f64, tmp: &mut [u8; 20]) -> (&[u8], i32) {
    let bits = a.to_bits();
    let ryu::Decimal {
        mut mantissa,
        mut exponent,
    } = ryu::d2d(bits & ((1 << 52) - 1), (bits >> 52) as u32);
    while mantissa % 10 == 0 {
        mantissa /= 10;
        exponent += 1;
    }
    let digits = u64_digits(mantissa, 10, tmp);
    let e = exponent + digits.len() as i32;
    (digits, e)
}

/// The shortest form of `a` (positive, finite) when it has at most 8 decimals, such as
/// prices, scores and coordinates: `(m, k)` with `a == m / 10^k`. Much cheaper than Ryu,
/// and the same result (D1262):
/// - `m as f64 / 10^k` is the correctly rounded value of the decimal `m * 10^-k` (both
///   operands are exact), so the check is exactly "the decimal round-trips";
/// - while `ulp(a) * 10^k < 1`, the round-trip interval (at most `ulp(a)` wide, also for
///   powers of two) holds at most one integer multiple of `10^-k`, so the first `k` that
///   works gives the unique shortest digits Ryu would find, and no shorter form exists;
/// - a candidate near a half-way point cannot round-trip under that bound, so rounding in
///   `a * 10^k` never hides the right candidate.
///
/// `None` means "use Ryu".
fn short_decimal(a: f64) -> Option<(u64, i32)> {
    const P10: [f64; 9] = [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8];
    const MAX: f64 = 9007199254740992.0; // 2^53
    let ulp = f64::from_bits(a.to_bits() + 1) - a;
    // Early exit for full-precision numbers (D1782), which otherwise try all eight `k`:
    // if some `m / 10^k` (k <= 8) round-trips, then `M = m * 10^(8-k)` is within
    // `ulp * 1e8 / 2` of `a * 1e8`, and the product's rounding adds less than
    // `ulp(a * 1e8) / 2 < ulp * 1e8`. So `a * 1e8` is within 1.5 `tol8` of the integer
    // `M`, which is then its nearest integer while `tol8 < 1/4`; a distance over 2 `tol8`
    // rules every `k` out. (With `tol8` that large, or a product at 2^53, the loop
    // decides.) A wrong exit would only cost speed: Ryu gives the same digits.
    let tol8 = ulp * 1e8;
    let y8 = a * 1e8;
    if tol8 < 0.25 && y8 < MAX {
        let d = (y8 + 0.5) as u64 as f64;
        if (y8 - d).abs() > 2.0 * tol8 {
            return None;
        }
    }
    for (k, &p) in P10.iter().enumerate().skip(1) {
        let tol = ulp * p;
        if tol >= 1.0 {
            return None;
        }
        let y = a * p;
        if y >= MAX {
            return None;
        }
        let m = (y + 0.5) as u64;
        let d = m as f64;
        // Cheap filter before the division: the candidate is within ulp(a) * 10^k / 2 of
        // the exact product, and `y` within half an ulp of that.
        if y - d > 2.0 * tol || d - y > 2.0 * tol {
            continue;
        }
        if d / p == a {
            // Not a multiple of 10, or a smaller k would have matched.
            return if m.is_multiple_of(10) {
                None
            } else {
                Some((m, k as i32))
            };
        }
    }
    None
}

/// `Number.prototype.toString(radix)` for an integer `radix` (the caller applies
/// ToIntegerOrInfinity and saturates). Radix 10 is [`to_string`].
///
/// The spec leaves other radixes implementation-defined for non-integers; this prints what
/// V8 prints (`DoubleToRadixCString`), never in exponential notation: fraction digits only
/// up to the double's precision (half the gap to the next double, scaled with each digit),
/// rounded half to even, and integer digits past 2^53 written as zeros.
pub fn to_string_radix(x: f64, radix: i32) -> Result<NumStr<MAX_RADIX_LEN>, RangeError> {
    if !(2..=36).contains(&radix) {
        return Err(RangeError::Radix);
    }
    let radix = radix as u32;
    let mut out = NumStr::new();
    if radix == 10 {
        out.extend(to_string(x).as_bytes());
        return Ok(out);
    }
    if non_finite_or_zero(&mut out, x) {
        return Ok(out);
    }
    const CHARS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let r = radix as f64;
    let value = x.abs();
    let mut integer = djs_libm::floor(value);
    let mut fraction = value - integer;
    let mut delta = (0.5 * (f64::from_bits(value.to_bits() + 1) - value)).max(f64::from_bits(1));
    // Fraction digits; a carry out of them can reach the integer part.
    let mut frac = [0u8; MAX_RADIX_LEN];
    let mut nf = 0;
    if fraction >= delta {
        loop {
            fraction *= r;
            delta *= r;
            let digit = fraction as u32;
            frac[nf] = CHARS[digit as usize];
            nf += 1;
            fraction -= digit as f64;
            if (fraction > 0.5 || (fraction == 0.5 && digit & 1 == 1)) && fraction + delta > 1.0 {
                // Round up, carrying through the digits already written.
                loop {
                    if nf == 0 {
                        integer += 1.0;
                        break;
                    }
                    nf -= 1;
                    let c = frac[nf];
                    let d = if c > b'9' { c - b'a' + 10 } else { c - b'0' } as u32;
                    if d + 1 < radix {
                        frac[nf] = CHARS[d as usize + 1];
                        nf += 1;
                        break;
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    // Integer digits, least significant first; those below the double's precision are zeros.
    let exponent = |d: f64| match (d.to_bits() >> 52) & 0x7ff {
        0 => -1074,
        b => b as i32 - 1075,
    };
    let mut int = [0u8; 1100];
    let mut ni = 0;
    while exponent(integer / r) > 0 {
        integer /= r;
        int[ni] = b'0';
        ni += 1;
    }
    loop {
        let rem = integer % r;
        int[ni] = CHARS[rem as usize];
        ni += 1;
        integer = (integer - rem) / r;
        if integer <= 0.0 {
            break;
        }
    }
    if x < 0.0 {
        out.push(b'-');
    }
    for &c in int[..ni].iter().rev() {
        out.push(c);
    }
    if nf > 0 {
        out.push(b'.');
        out.extend(&frac[..nf]);
    }
    Ok(out)
}

/// `Number.prototype.toFixed(digits)`. The caller passes ToIntegerOrInfinity(digits),
/// saturated to `i32`.
pub fn to_fixed(x: f64, digits: i32) -> Result<NumStr<MAX_FIXED_LEN>, RangeError> {
    if !(0..=MAX_DIGITS).contains(&digits) {
        return Err(RangeError::Digits);
    }
    let mut out = NumStr::new();
    if x.is_nan() {
        out.extend(b"NaN");
        return Ok(out);
    }
    if x.abs() >= 1e21 {
        out.extend(to_string(x).as_bytes());
        return Ok(out);
    }
    let f = digits as usize;
    let mut tmp = [0u8; 128];
    let n: &[u8] = if x == 0.0 {
        b"0"
    } else {
        if x < 0.0 {
            out.push(b'-');
        }
        let (m, e) = decompose(x.abs());
        let b = scale_round(Big::from_u64(m), e - 53, 10, digits, Round::HalfUp);
        big_decimal_digits(b, &mut tmp)
    };
    // n is round(|x| * 10^f); print it with f fractional digits.
    if n.len() > f {
        let k = n.len() - f;
        out.extend(&n[..k]);
        if f > 0 {
            out.push(b'.');
            out.extend(&n[k..]);
        }
    } else {
        out.extend(b"0.");
        out.push_zeros(f - n.len());
        out.extend(n);
    }
    Ok(out)
}

/// `Number.prototype.toExponential(digits)`. `None` is an undefined argument (as many digits
/// as needed); otherwise the caller passes ToIntegerOrInfinity(digits), saturated to `i32`.
pub fn to_exponential(x: f64, digits: Option<i32>) -> Result<NumStr<MAX_FIXED_LEN>, RangeError> {
    let mut out = NumStr::new();
    if !x.is_finite() {
        out.extend(to_string(x).as_bytes());
        return Ok(out);
    }
    match digits {
        None => {
            if x == 0.0 {
                out.extend(b"0e+0");
                return Ok(out);
            }
            if x < 0.0 {
                out.push(b'-');
            }
            let mut tmp = [0u8; 20];
            let (d, e) = shortest(x.abs(), &mut tmp);
            layout(&mut out, d, e, Exp::Always);
        }
        Some(f) => {
            if !(0..=MAX_DIGITS).contains(&f) {
                return Err(RangeError::Digits);
            }
            precision_digits(&mut out, x, f + 1, Exp::Always);
        }
    }
    Ok(out)
}

/// `Number.prototype.toPrecision(precision)`. `None` is an undefined argument (same as
/// `ToString`); otherwise the caller passes ToIntegerOrInfinity(precision), saturated to `i32`.
pub fn to_precision(x: f64, precision: Option<i32>) -> Result<NumStr<MAX_FIXED_LEN>, RangeError> {
    let mut out = NumStr::new();
    let p = match precision {
        Some(p) if x.is_finite() => p,
        _ => {
            out.extend(to_string(x).as_bytes());
            return Ok(out);
        }
    };
    if !(1..=MAX_DIGITS).contains(&p) {
        return Err(RangeError::Digits);
    }
    precision_digits(&mut out, x, p, Exp::Auto(p));
    Ok(out)
}

/// Writes finite `x` rounded (half up) to `p` significant digits.
fn precision_digits(out: &mut NumStr<MAX_FIXED_LEN>, x: f64, p: i32, exp: Exp) {
    let mut tmp = [0u8; 128];
    if x == 0.0 {
        tmp[..p as usize].fill(b'0');
        layout(out, &tmp[..p as usize], 1, exp);
        return;
    }
    if x < 0.0 {
        out.push(b'-');
    }
    let (m, e2) = decompose(x.abs());
    // Start from a lower bound of the decimal exponent and step up until the rounded
    // mantissa has p digits.
    let mut e = 1 + mul_log2_radix(e2 - 1, 10);
    loop {
        let b = scale_round(Big::from_u64(m), e2 - 53, 10, p - e, Round::HalfUp);
        let len = big_decimal_digits(b.clone(), &mut tmp).len() as i32;
        if len <= p {
            let digits = big_decimal_digits(b, &mut tmp);
            layout(out, digits, e, exp);
            return;
        }
        e += 1;
    }
}
