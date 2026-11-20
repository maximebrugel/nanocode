//! log, log2 and log10 as glibc 2.28+ computes them (sysdeps/ieee754/dbl-64
//! e_log.c, e_log2.c and e_log10.c), so math.log* return what CPython does on
//! Linux. djs_libm's fdlibm versions, which the binary exports for the
//! JavaScript engine, differ from glibc's in a few percent of inputs.
//! x86_64 glibc runs e_log.c and e_log2.c built with -mfma, where gcc fuses
//! each product that feeds one addition: fma() marks the same places.

use super::log_tbl as tbl;

#[inline(always)]
fn d(bits: u64) -> f64 {
    f64::from_bits(bits)
}

#[inline(always)]
fn fma(a: f64, b: f64, c: f64) -> f64 {
    a.mul_add(b, c)
}

const OFF: u64 = 0x3fe6_0000_0000_0000;

/// Handles zero, negative, infinite, NaN and subnormal x for log and log2:
/// Err(result), or Ok(the bits of x scaled into the normal range).
#[inline(always)]
fn special(x: f64, ix: u64, top: u32) -> Result<u64, f64> {
    if ix << 1 == 0 {
        return Err(f64::NEG_INFINITY);
    }
    if ix == f64::INFINITY.to_bits() {
        return Err(x);
    }
    if (top & 0x8000) != 0 || (top & 0x7ff0) == 0x7ff0 {
        return Err(f64::NAN);
    }
    Ok((x * f64::from_bits(0x4330_0000_0000_0000)).to_bits() - (52u64 << 52))
}

pub fn log(x: f64) -> f64 {
    let b = |i: usize| d(tbl::LOG_POLY1[i]);
    let a = |i: usize| d(tbl::LOG_POLY[i]);
    const LN2HI: f64 = f64::from_bits(0x3fe6_2e42_fefa_3800);
    const LN2LO: f64 = f64::from_bits(0x3d2e_f357_93c7_6730);
    let mut ix = x.to_bits();
    let top = (ix >> 48) as u32;
    const LO: u64 = 0x3fee_0000_0000_0000; // 1.0 - 0x1p-4
    const HI: u64 = 0x3ff1_0900_0000_0000; // 1.0 + 0x1.09p-4
    if ix.wrapping_sub(LO) < HI - LO {
        if ix == 1f64.to_bits() {
            return 0.0;
        }
        let r = x - 1.0;
        let r2 = r * r;
        let r3 = r * r2;
        let inner = fma(r3, b(10), fma(r2, b(9), fma(r, b(8), b(7))));
        let mid = fma(r3, inner, fma(r2, b(6), fma(r, b(5), b(4))));
        let p = fma(r3, mid, fma(r2, b(3), fma(r, b(2), b(1))));
        let w = r * f64::from_bits(0x41a0_0000_0000_0000); // 0x1p27
        let rhi = r + w - w;
        let rlo = r - rhi;
        let m = rhi * rhi;
        let hi = fma(m, b(0), r);
        let mut lo = fma(m, b(0), r - hi);
        lo = fma(b(0) * rlo, rhi + r, lo);
        // y = r3 * p; y += lo; y += hi: gcc fuses the first two.
        return fma(r3, p, lo) + hi;
    }
    if top.wrapping_sub(0x0010) >= 0x7ff0 - 0x0010 {
        match special(x, ix, top) {
            Ok(v) => ix = v,
            Err(v) => return v,
        }
    }
    let tmp = ix.wrapping_sub(OFF);
    let i = ((tmp >> (52 - 7)) % 128) as usize;
    let k = (tmp as i64) >> 52;
    let iz = ix.wrapping_sub(tmp & (0xfffu64 << 52));
    let invc = d(tbl::LOG_TAB[2 * i]);
    let logc = d(tbl::LOG_TAB[2 * i + 1]);
    let z = f64::from_bits(iz);
    let r = fma(z, invc, -1.0);
    let kd = k as f64;
    let w = fma(kd, LN2HI, logc);
    let hi = w + r;
    let lo = fma(kd, LN2LO, w - hi + r);
    let r2 = r * r;
    let p = fma(r2, fma(r, a(4), a(3)), fma(r, a(2), a(1)));
    let s1 = fma(r2, a(0), lo);
    fma(r * r2, p, s1) + hi
}

pub fn log2(x: f64) -> f64 {
    let b = |i: usize| d(tbl::LOG2_POLY1[i]);
    let a = |i: usize| d(tbl::LOG2_POLY[i]);
    const INVLN2HI: f64 = f64::from_bits(0x3ff7_1547_6520_0000);
    const INVLN2LO: f64 = f64::from_bits(0x3de7_05fc_2eef_a200);
    let mut ix = x.to_bits();
    let top = (ix >> 48) as u32;
    const LO: u64 = 0x3fee_a4af_0000_0000; // 1.0 - 0x1.5b51p-5
    const HI: u64 = 0x3ff0_b559_0000_0000; // 1.0 + 0x1.6ab2p-5
    if ix.wrapping_sub(LO) < HI - LO {
        if ix == 1f64.to_bits() {
            return 0.0;
        }
        let r = x - 1.0;
        let hi = r * INVLN2HI;
        let mut lo = fma(r, INVLN2LO, fma(r, INVLN2HI, -hi));
        let r2 = r * r;
        let r4 = r2 * r2;
        let q = fma(r, b(1), b(0));
        let y = fma(r2, q, hi);
        lo += fma(r2, q, hi - y);
        let c = fma(r2, fma(r, b(5), b(4)), fma(r, b(3), b(2)));
        let f = fma(r2, fma(r, b(9), b(8)), fma(r, b(7), b(6)));
        lo = fma(r4, fma(r4, f, c), lo);
        return y + lo;
    }
    if top.wrapping_sub(0x0010) >= 0x7ff0 - 0x0010 {
        match special(x, ix, top) {
            Ok(v) => ix = v,
            Err(v) => return v,
        }
    }
    let tmp = ix.wrapping_sub(OFF);
    let i = ((tmp >> (52 - 6)) % 64) as usize;
    let k = (tmp as i64) >> 52;
    let iz = ix.wrapping_sub(tmp & (0xfffu64 << 52));
    let invc = d(tbl::LOG2_TAB[2 * i]);
    let logc = d(tbl::LOG2_TAB[2 * i + 1]);
    let z = f64::from_bits(iz);
    let kd = k as f64;
    let r = fma(z, invc, -1.0);
    let t1 = r * INVLN2HI;
    let t2 = fma(r, INVLN2LO, fma(r, INVLN2HI, -t1));
    let t3 = kd + logc;
    let hi = t3 + t1;
    let lo = t3 - hi + t1 + t2;
    let r2 = r * r;
    let r4 = r2 * r2;
    let p = fma(r4, fma(r, a(5), a(4)), fma(r2, fma(r, a(3), a(2)), fma(r, a(1), a(0))));
    fma(r2, p, lo) + hi
}

/// glibc's e_log10.c, which is built without -mfma and calls glibc's log.
pub fn log10(x: f64) -> f64 {
    const TWO54: f64 = f64::from_bits(0x4350_0000_0000_0000);
    const IVLN10: f64 = f64::from_bits(0x3fdb_cb7b_1526_e50e);
    const LOG10_2HI: f64 = f64::from_bits(0x3fd3_4413_509f_6000);
    const LOG10_2LO: f64 = f64::from_bits(0x3d59_fef3_11f1_2b36);
    let mut x = x;
    let mut hx = x.to_bits() as i64;
    let mut k: i64 = 0;
    if hx < 0x0010_0000_0000_0000 {
        if hx & 0x7fff_ffff_ffff_ffff == 0 {
            return f64::NEG_INFINITY;
        }
        if hx < 0 {
            return f64::NAN;
        }
        k -= 54;
        x *= TWO54;
        hx = x.to_bits() as i64;
    }
    if hx as u64 >= 0x7ff0_0000_0000_0000 {
        return x + x;
    }
    k += (hx >> 52) - 1023;
    let i = ((k as u64) & 0x8000_0000_0000_0000) >> 63;
    let hx = ((hx as u64) & 0x000f_ffff_ffff_ffff) | ((0x3ff - i) << 52);
    let y = (k + i as i64) as f64;
    let x = f64::from_bits(hx);
    let z = y * LOG10_2LO + IVLN10 * log(x);
    z + y * LOG10_2HI
}
