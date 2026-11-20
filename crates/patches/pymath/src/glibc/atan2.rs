//! atan2 as glibc 2.28+ computes it (sysdeps/ieee754/dbl-64/e_atan2.c, IBM
//! Accurate Mathematical Library), so math.atan2 returns what CPython does on
//! Linux. djs_libm's fdlibm atan2, which the binary exports as atan2 for the
//! JavaScript engine, differs from glibc's in about 1 in 6 random inputs.
//! x86_64 glibc runs this file built with -mfma, where gcc fuses each product
//! that feeds one addition: fma() below marks the same places, so the results
//! match bit for bit there.

use super::atan2_tbl as tbl;

#[inline(always)]
fn d(bits: u64) -> f64 {
    f64::from_bits(bits)
}

const D3: u64 = 0xbfd5_5555_5555_5555;
const D5: u64 = 0x3fc9_9999_9999_97fd;
const D7: u64 = 0xbfc2_4924_923f_7603;
const D9: u64 = 0x3fbc_71c6_e512_9a3b;
const D11: u64 = 0xbfb7_4580_22b1_3c25;
const D13: u64 = 0x3fb3_75f0_8b31_cbce;
const INV16: f64 = 0.0625;
const OPI: u64 = 0x4009_21fb_5444_2d18;
const OPI1: u64 = 0x3ca1_a626_3314_5c07;
const HPI: u64 = 0x3ff9_21fb_5444_2d18;
const HPI1: u64 = 0x3c91_a626_3314_5c07;
const QPI: u64 = 0x3fe9_21fb_5444_2d18;
const TQPI: u64 = 0x4002_d97c_7f33_21d2;
const TWO500: f64 = f64::from_bits(0x5f30_0000_0000_0000);
const TWOM500: f64 = f64::from_bits(0x20b0_0000_0000_0000);
const TWO52: f64 = 4503599627370496.0;
const CN: f64 = 134217729.0;

/// Dekker's exact product: z + zz == x * y.
#[inline(always)]
fn emulv(x: f64, y: f64) -> (f64, f64) {
    let p = CN * x;
    let hx = (x - p) + p;
    let tx = x - hx;
    let p = CN * y;
    let hy = (y - p) + p;
    let ty = y - hy;
    let z = x * y;
    (z, (((hx * hy - z) + hx * ty) + tx * hy) + tx * ty)
}

#[inline(always)]
fn fma(a: f64, b: f64, c: f64) -> f64 {
    a.mul_add(b, c)
}

#[inline(always)]
fn eadd(x: f64, y: f64) -> (f64, f64) {
    let z = x + y;
    let zz = if x.abs() > y.abs() { (x - z) + y } else { (y - z) + x };
    (z, zz)
}

#[inline(always)]
fn esub(x: f64, y: f64) -> (f64, f64) {
    let z = x - y;
    let zz = if x.abs() > y.abs() { (x - z) - y } else { x - (y + z) };
    (z, zz)
}

#[inline(always)]
fn poly(v: f64) -> f64 {
    fma(v, fma(v, fma(v, fma(v, fma(v, d(D13), d(D11)), d(D9)), d(D7)), d(D5)), d(D3))
}

/// c3 + v * (c4 + v * (c5 + v * c6)).
#[inline(always)]
fn horner3(c: &[u64; 7], v: f64) -> f64 {
    fma(v, fma(v, fma(v, d(c[6]), d(c[5])), d(c[4])), d(c[3]))
}

/// c2 + v * (c3 + v * (c4 + v * (c5 + v * c6))).
#[inline(always)]
fn horner2(c: &[u64; 7], v: f64) -> f64 {
    fma(v, horner3(c, v), d(c[2]))
}

#[inline(always)]
fn row(u: f64) -> &'static [u64; 7] {
    let i = ((TWO52 + 256.0 * u) - TWO52) as usize;
    &tbl::CIJ[i - 16]
}

pub fn atan2(y: f64, x: f64) -> f64 {
    let xb = x.to_bits();
    let yb = y.to_bits();
    let ux = (xb >> 32) as u32;
    let uy = (yb >> 32) as u32;
    let dy = yb as u32;
    if x.is_nan() {
        return x + y;
    }
    if y.is_nan() {
        return y + y;
    }
    // y = +-0
    if uy == 0 && dy == 0 {
        return if ux & 0x8000_0000 == 0 { 0.0 } else { d(OPI) };
    } else if uy == 0x8000_0000 && dy == 0 {
        return if ux & 0x8000_0000 == 0 { -0.0 } else { -d(OPI) };
    }
    // x = +-0
    if x == 0.0 {
        return if uy & 0x8000_0000 == 0 { d(HPI) } else { -d(HPI) };
    }
    // x = +-inf
    if x == f64::INFINITY {
        if y == f64::INFINITY {
            return d(QPI);
        } else if y == f64::NEG_INFINITY {
            return -d(QPI);
        }
        return if uy & 0x8000_0000 == 0 { 0.0 } else { -0.0 };
    } else if x == f64::NEG_INFINITY {
        if y == f64::INFINITY {
            return d(TQPI);
        } else if y == f64::NEG_INFINITY {
            return -d(TQPI);
        }
        return if uy & 0x8000_0000 == 0 { d(OPI) } else { -d(OPI) };
    }
    // y = +-inf
    if y == f64::INFINITY {
        return d(HPI);
    } else if y == f64::NEG_INFINITY {
        return -d(HPI);
    }

    let mut ax = x.abs();
    let mut ay = y.abs();
    let de = (uy & 0x7ff0_0000) as i32 - (ux & 0x7ff0_0000) as i32;
    const EP: i32 = 59_768_832; // 57 * 16**5
    if de >= EP {
        return if y > 0.0 { d(HPI) } else { -d(HPI) };
    } else if de <= -EP {
        if x > 0.0 {
            return (ay / ax).copysign(y);
        }
        return if y > 0.0 { d(OPI) } else { -d(OPI) };
    }
    if ax < TWOM500 || ay < TWOM500 {
        ax *= TWO500;
        ay *= TWO500;
    }
    if ax > TWO500 || ay > TWO500 {
        ax *= TWOM500;
        ay *= TWOM500;
    }

    let (u, du) = if ay < ax {
        let u = ay / ax;
        let (v, vv) = emulv(ax, u);
        (u, ((ay - v) - vv) / ax)
    } else {
        let u = ax / ay;
        let (v, vv) = emulv(ay, u);
        (u, ((ax - v) - vv) / ay)
    };

    let z = if x > 0.0 {
        if ay < ax {
            // (i) atan(ay/ax)
            if u < INV16 {
                let v = u * u;
                let zz = fma(u * v, poly(v), du);
                u + zz
            } else {
                let c = row(u);
                let t3 = u - d(c[0]);
                let (v, dv) = eadd(t3, du);
                let t1 = d(c[1]);
                let t2 = d(c[2]);
                let zz = fma(v, t2, fma(dv, t2, (v * v) * horner3(c, v)));
                t1 + zz
            }
        } else if u < INV16 {
            // (ii) pi/2 - atan(ax/ay)
            let v = u * u;
            let (t2, cor) = esub(d(HPI), u);
            let t3 = fma(-(u * v), poly(v), (d(HPI1) + cor) - du);
            t2 + t3
        } else {
            let c = row(u);
            let v = (u - d(c[0])) + du;
            let zz = fma(-v, horner2(c, v), d(HPI1));
            let t1 = d(HPI) - d(c[1]);
            t1 + zz
        }
    } else if ax < ay {
        // (iii) pi/2 + atan(ax/ay)
        if u < INV16 {
            let v = u * u;
            let (t2, cor) = eadd(d(HPI), u);
            let t3 = fma(u * v, poly(v), (d(HPI1) + cor) + du);
            t2 + t3
        } else {
            let c = row(u);
            let v = (u - d(c[0])) + du;
            let zz = fma(v, horner2(c, v), d(HPI1));
            let t1 = d(HPI) + d(c[1]);
            t1 + zz
        }
    } else if u < INV16 {
        // (iv) pi - atan(ax/ay)
        let v = u * u;
        let (t2, cor) = esub(d(OPI), u);
        let t3 = fma(-(u * v), poly(v), (d(OPI1) + cor) - du);
        t2 + t3
    } else {
        let c = row(u);
        let v = (u - d(c[0])) + du;
        let zz = fma(-v, horner2(c, v), d(OPI1));
        let t1 = d(OPI) - d(c[1]);
        t1 + zz
    };
    z.copysign(y)
}
