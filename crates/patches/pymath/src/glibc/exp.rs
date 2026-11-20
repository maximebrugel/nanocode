//! exp as glibc 2.28+ computes it (sysdeps/ieee754/dbl-64/e_exp.c), so
//! math.exp returns what CPython does on Linux. glibc's exp is within 0.52
//! ulp, not correctly rounded, so a correctly rounded exp still differs from
//! it in about 1 in 1300 inputs. x86_64 glibc runs e_exp.c built with -mfma,
//! where gcc fuses each product that feeds additions only, in its own block:
//! fma() marks the same places.

use super::exp_tbl::{EXP_POLY, EXP_TAB};

#[inline(always)]
fn fma(a: f64, b: f64, c: f64) -> f64 {
    a.mul_add(b, c)
}

const N: u64 = 128;
const INVLN2N: f64 = f64::from_bits(0x4067_1547_652b_82fe); // 0x1.71547652b82fep0 * 128
const NEGLN2HIN: f64 = f64::from_bits(0xbf76_2e42_fefa_0000);
const NEGLN2LON: f64 = f64::from_bits(0xbd0c_f79a_bc9e_3b3a);
const SHIFT: f64 = f64::from_bits(0x4338_0000_0000_0000); // 0x1.8p52
const P2_1009: f64 = f64::from_bits(0x7f00_0000_0000_0000);
const P2_M1022: f64 = f64::from_bits(0x0010_0000_0000_0000);

#[inline(always)]
fn top12(x: f64) -> u32 {
    (x.to_bits() >> 52) as u32
}

#[cold]
fn specialcase(tmp: f64, sbits: u64, ki: u64) -> f64 {
    if ki & 0x8000_0000 == 0 {
        // k > 0: the exponent of scale may have overflowed by up to 460.
        let scale = f64::from_bits(sbits.wrapping_sub(1009u64 << 52));
        return P2_1009 * fma(scale, tmp, scale);
    }
    // k < 0: round once before scaling into the subnormal range. scale * tmp
    // feeds two blocks here, which gcc never fuses.
    let scale = f64::from_bits(sbits.wrapping_add(1022u64 << 52));
    let st = scale * tmp;
    let mut y = scale + st;
    if y < 1.0 {
        let lo = scale - y + st;
        let hi = 1.0 + y;
        let lo = 1.0 - hi + y + lo;
        y = (hi + lo) - 1.0;
        if y == 0.0 {
            y = 0.0;
        }
    }
    P2_M1022 * y
}

pub fn exp(x: f64) -> f64 {
    let mut abstop = top12(x) & 0x7ff;
    const TINY: u32 = 0x3c9; // top12(0x1p-54)
    const BIG: u32 = 0x408; // top12(512.0)
    if abstop.wrapping_sub(TINY) >= BIG - TINY {
        if abstop.wrapping_sub(TINY) >= 0x8000_0000 {
            return 1.0 + x;
        }
        if abstop >= 0x409 {
            // |x| >= 1024
            if x == f64::NEG_INFINITY {
                return 0.0;
            }
            if abstop >= 0x7ff {
                return 1.0 + x;
            }
            return if x.is_sign_negative() { 0.0 } else { f64::INFINITY };
        }
        abstop = 0;
    }
    let kd = fma(INVLN2N, x, SHIFT);
    let ki = kd.to_bits();
    let kd = kd - SHIFT;
    let r = fma(kd, NEGLN2LON, fma(kd, NEGLN2HIN, x));
    let idx = (2 * (ki % N)) as usize;
    let top = ki << (52 - 7);
    let tail = f64::from_bits(EXP_TAB[idx]);
    let sbits = EXP_TAB[idx + 1].wrapping_add(top);
    let c = |i: usize| f64::from_bits(EXP_POLY[i]);
    let r2 = r * r;
    let tmp = fma(r2 * r2, fma(r, c(3), c(2)), fma(r2, fma(r, c(1), c(0)), tail + r));
    if abstop == 0 {
        return specialcase(tmp, sbits, ki);
    }
    let scale = f64::from_bits(sbits);
    fma(scale, tmp, scale)
}
