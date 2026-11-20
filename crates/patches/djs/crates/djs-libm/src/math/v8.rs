//! V8's fdlibm port, from v8/src/base/ieee754.cc (V8 13.6, the engine Node 24 ships), so that
//! djs's Math functions round exactly as V8's do. musl's versions (the rest of this crate) are
//! the same algorithms in places but differ in others (exp(1) is not Math.E there, the trig
//! kernels and argument reduction differ, cbrt and the log family round differently), so these
//! are a line-by-line copy of V8's C++ with its constants and evaluation order kept.
//!
//! ====================================================
//! Copyright (C) 1993-2004 by Sun Microsystems, Inc. All rights reserved.
//!
//! Developed at SunSoft, a Sun Microsystems, Inc. business.
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ====================================================
//!
//! The original source code covered by the above license above has been
//! modified significantly by Google Inc.
//! Copyright 2016 the V8 project authors. All rights reserved.

use super::{atan, expm1, fabs, floor, scalbn, sqrt};

#[inline]
fn hi(x: f64) -> i32 {
    (x.to_bits() >> 32) as i32
}

#[inline]
fn lo(x: f64) -> u32 {
    x.to_bits() as u32
}

#[inline]
fn words(h: u32, l: u32) -> f64 {
    f64::from_bits(((h as u64) << 32) | l as u64)
}

#[inline]
fn set_hi(x: f64, h: u32) -> f64 {
    words(h, lo(x))
}

#[inline]
fn set_lo(x: f64, l: u32) -> f64 {
    words(hi(x) as u32, l)
}

const TWO_OVER_PI: [i32; 66] = [
    0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C, 0x439041, 0xFE5163,
    0xABDEBB, 0xC561B7, 0x246E3A, 0x424DD2, 0xE00649, 0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129,
    0xA73EE8, 0x8235F5, 0x2EBB44, 0x84E99C, 0x7026B4, 0x5F7E41, 0x3991D6, 0x398353, 0x39F49C,
    0x845F8B, 0xBDF928, 0x3B1FF8, 0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D, 0x367ECF,
    0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5, 0xF17B3D, 0x0739F7, 0x8A5292,
    0xEA6BFB, 0x5FB11F, 0x8D5D08, 0x560330, 0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3,
    0x91615E, 0xE61B08, 0x659985, 0x5F14A0, 0x68408D, 0xFFD880, 0x4D7327, 0x310606, 0x1556CA,
    0x73A8C9, 0x60E27B, 0xC08C6B,
];

const NPIO2_HW: [i32; 32] = [
    0x3FF921FB, 0x400921FB, 0x4012D97C, 0x401921FB, 0x401F6A7A, 0x4022D97C, 0x4025FDBB,
    0x402921FB, 0x402C463A, 0x402F6A7A, 0x4031475C, 0x4032D97C, 0x40346B9C, 0x4035FDBB,
    0x40378FDB, 0x403921FB, 0x403AB41B, 0x403C463A, 0x403DD85A, 0x403F6A7A, 0x40407E4C,
    0x4041475C, 0x4042106C, 0x4042D97C, 0x4043A28C, 0x40446B9C, 0x404534AC, 0x4045FDBB,
    0x4046C6CB, 0x40478FDB, 0x404858EB, 0x404921FB,
];

/// __ieee754_rem_pio2: x reduced by multiples of pi/2, as (n, y0, y1).
fn rem_pio2(x: f64) -> (i32, f64, f64) {
    const HALF: f64 = 5.00000000000000000000e-01;
    const TWO24: f64 = 1.67772160000000000000e+07;
    const INVPIO2: f64 = 6.36619772367581382433e-01;
    const PIO2_1: f64 = 1.57079632673412561417e+00;
    const PIO2_1T: f64 = 6.07710050650619224932e-11;
    const PIO2_2: f64 = 6.07710050630396597660e-11;
    const PIO2_2T: f64 = 2.02226624879595063154e-21;
    const PIO2_3: f64 = 2.02226624871116645580e-21;
    const PIO2_3T: f64 = 8.47842766036889956997e-32;

    let hx = hi(x);
    let ix = hx & 0x7FFFFFFF;
    if ix <= 0x3FE921FB {
        return (0, x, 0.0);
    }
    if ix < 0x4002D97C {
        if hx > 0 {
            let mut z = x - PIO2_1;
            let (y0, y1);
            if ix != 0x3FF921FB {
                y0 = z - PIO2_1T;
                y1 = (z - y0) - PIO2_1T;
            } else {
                z -= PIO2_2;
                y0 = z - PIO2_2T;
                y1 = (z - y0) - PIO2_2T;
            }
            return (1, y0, y1);
        } else {
            let mut z = x + PIO2_1;
            let (y0, y1);
            if ix != 0x3FF921FB {
                y0 = z + PIO2_1T;
                y1 = (z - y0) + PIO2_1T;
            } else {
                z += PIO2_2;
                y0 = z + PIO2_2T;
                y1 = (z - y0) + PIO2_2T;
            }
            return (-1, y0, y1);
        }
    }
    if ix <= 0x413921FB {
        let t = fabs(x);
        let n = (t * INVPIO2 + HALF) as i32;
        let fnn = n as f64;
        let mut r = t - fnn * PIO2_1;
        let mut w = fnn * PIO2_1T;
        let y0;
        if n < 32 && ix != NPIO2_HW[(n - 1) as usize] {
            y0 = r - w;
        } else {
            let j = ix >> 20;
            let mut yy = r - w;
            let mut i = j - ((hi(yy) as u32 >> 20) & 0x7FF) as i32;
            if i > 16 {
                let t = r;
                w = fnn * PIO2_2;
                r = t - w;
                w = fnn * PIO2_2T - ((t - r) - w);
                yy = r - w;
                i = j - ((hi(yy) as u32 >> 20) & 0x7FF) as i32;
                if i > 49 {
                    let t = r;
                    w = fnn * PIO2_3;
                    r = t - w;
                    w = fnn * PIO2_3T - ((t - r) - w);
                    yy = r - w;
                }
            }
            y0 = yy;
        }
        let y1 = (r - y0) - w;
        return if hx < 0 { (-n, -y0, -y1) } else { (n, y0, y1) };
    }
    if ix >= 0x7FF00000 {
        let v = x - x;
        return (0, v, v);
    }
    let mut z = set_lo(0.0, lo(x));
    let e0 = (ix >> 20) - 1046;
    z = set_hi(z, ix.wrapping_sub(((e0 as u32) << 20) as i32) as u32);
    let mut tx = [0.0f64; 3];
    for t in tx.iter_mut().take(2) {
        *t = (z as i32) as f64;
        z = (z - *t) * TWO24;
    }
    tx[2] = z;
    let mut nx = 3;
    while tx[nx - 1] == 0.0 {
        nx -= 1;
    }
    let (n, y0, y1) = kernel_rem_pio2(&tx[..nx], e0);
    if hx < 0 { (-n, -y0, -y1) } else { (n, y0, y1) }
}

/// __kernel_rem_pio2 with prec = 2 (the only precision V8 uses) and ipio2 = TWO_OVER_PI.
fn kernel_rem_pio2(x: &[f64], e0: i32) -> (i32, f64, f64) {
    const PIO2: [f64; 8] = [
        1.57079625129699707031e+00,
        7.54978941586159635335e-08,
        5.39030252995776476554e-15,
        3.28200341580791294123e-22,
        1.27065575308067607349e-29,
        1.22933308981111328932e-36,
        2.73370053816464559624e-44,
        2.16741683877804819444e-51,
    ];
    const TWO24: f64 = 1.67772160000000000000e+07;
    const TWON24: f64 = 5.96046447753906250000e-08;

    let jk: i32 = 4;
    let jp = jk;
    let jx = x.len() as i32 - 1;
    let mut jv = (e0 - 3) / 24;
    if jv < 0 {
        jv = 0;
    }
    let mut q0 = e0 - 24 * (jv + 1);
    let mut iq = [0i32; 20];
    let mut f = [0.0f64; 20];
    let mut fq = [0.0f64; 20];
    let mut q = [0.0f64; 20];

    let mut j = jv - jx;
    let m = jx + jk;
    for i in 0..=m {
        f[i as usize] = if j < 0 { 0.0 } else { TWO_OVER_PI[j as usize] as f64 };
        j += 1;
    }
    for i in 0..=jk {
        let mut fw = 0.0;
        for j in 0..=jx {
            fw += x[j as usize] * f[(jx + i - j) as usize];
        }
        q[i as usize] = fw;
    }
    let mut jz = jk;
    let mut z;
    let mut n;
    let mut ih;
    loop {
        // recompute:
        let mut i = 0usize;
        let mut j = jz;
        z = q[jz as usize];
        while j > 0 {
            let fw = ((TWON24 * z) as i32) as f64;
            iq[i] = (z - TWO24 * fw) as i32;
            z = q[(j - 1) as usize] + fw;
            i += 1;
            j -= 1;
        }
        z = scalbn(z, q0);
        z -= 8.0 * floor(z * 0.125);
        n = z as i32;
        z -= n as f64;
        ih = 0;
        if q0 > 0 {
            let i = iq[(jz - 1) as usize] >> (24 - q0);
            n += i;
            iq[(jz - 1) as usize] -= i << (24 - q0);
            ih = iq[(jz - 1) as usize] >> (23 - q0);
        } else if q0 == 0 {
            ih = iq[(jz - 1) as usize] >> 23;
        } else if z >= 0.5 {
            ih = 2;
        }
        if ih > 0 {
            n += 1;
            let mut carry = 0;
            for v in iq.iter_mut().take(jz as usize) {
                let j = *v;
                if carry == 0 {
                    if j != 0 {
                        carry = 1;
                        *v = 0x1000000 - j;
                    }
                } else {
                    *v = 0xFFFFFF - j;
                }
            }
            if q0 == 1 {
                iq[(jz - 1) as usize] &= 0x7FFFFF;
            } else if q0 == 2 {
                iq[(jz - 1) as usize] &= 0x3FFFFF;
            }
            if ih == 2 {
                z = 1.0 - z;
                if carry != 0 {
                    z -= scalbn(1.0, q0);
                }
            }
        }
        if z == 0.0 {
            let mut j = 0;
            let mut i = jz - 1;
            while i >= jk {
                j |= iq[i as usize];
                i -= 1;
            }
            if j == 0 {
                let mut k = 1;
                while jk >= k && iq[(jk - k) as usize] == 0 {
                    k += 1;
                }
                for i in (jz + 1)..=(jz + k) {
                    f[(jx + i) as usize] = TWO_OVER_PI[(jv + i) as usize] as f64;
                    let mut fw = 0.0;
                    for j in 0..=jx {
                        fw += x[j as usize] * f[(jx + i - j) as usize];
                    }
                    q[i as usize] = fw;
                }
                jz += k;
                continue;
            }
        }
        break;
    }
    if z == 0.0 {
        jz -= 1;
        q0 -= 24;
        while iq[jz as usize] == 0 {
            jz -= 1;
            q0 -= 24;
        }
    } else {
        z = scalbn(z, -q0);
        if z >= TWO24 {
            let fw = ((TWON24 * z) as i32) as f64;
            iq[jz as usize] = (z - TWO24 * fw) as i32;
            jz += 1;
            q0 += 24;
            iq[jz as usize] = fw as i32;
        } else {
            iq[jz as usize] = z as i32;
        }
    }
    let mut fw = scalbn(1.0, q0);
    let mut i = jz;
    while i >= 0 {
        q[i as usize] = fw * iq[i as usize] as f64;
        fw *= TWON24;
        i -= 1;
    }
    let mut i = jz;
    while i >= 0 {
        let mut fw = 0.0;
        let mut k = 0;
        while k <= jp && k <= jz - i {
            fw += PIO2[k as usize] * q[(i + k) as usize];
            k += 1;
        }
        fq[(jz - i) as usize] = fw;
        i -= 1;
    }
    let mut fw = 0.0;
    let mut i = jz;
    while i >= 0 {
        fw += fq[i as usize];
        i -= 1;
    }
    let y0 = if ih == 0 { fw } else { -fw };
    let mut fw = fq[0] - fw;
    for i in 1..=jz {
        fw += fq[i as usize];
    }
    let y1 = if ih == 0 { fw } else { -fw };
    (n & 7, y0, y1)
}

fn kernel_cos(x: f64, y: f64) -> f64 {
    const C1: f64 = 4.16666666666666019037e-02;
    const C2: f64 = -1.38888888888741095749e-03;
    const C3: f64 = 2.48015872894767294178e-05;
    const C4: f64 = -2.75573143513906633035e-07;
    const C5: f64 = 2.08757232129817482790e-09;
    const C6: f64 = -1.13596475577881948265e-11;
    let ix = hi(x) & 0x7FFFFFFF;
    if ix < 0x3E400000 && x as i32 == 0 {
        return 1.0;
    }
    let z = x * x;
    let r = z * (C1 + z * (C2 + z * (C3 + z * (C4 + z * (C5 + z * C6)))));
    if ix < 0x3FD33333 {
        1.0 - (0.5 * z - (z * r - x * y))
    } else {
        let qx = if ix > 0x3FE90000 { 0.28125 } else { words((ix - 0x00200000) as u32, 0) };
        let iz = 0.5 * z - qx;
        let a = 1.0 - qx;
        a - (iz - (z * r - x * y))
    }
}

fn kernel_sin(x: f64, y: f64, iy: i32) -> f64 {
    const HALF: f64 = 5.00000000000000000000e-01;
    const S1: f64 = -1.66666666666666324348e-01;
    const S2: f64 = 8.33333333332248946124e-03;
    const S3: f64 = -1.98412698298579493134e-04;
    const S4: f64 = 2.75573137070700676789e-06;
    const S5: f64 = -2.50507602534068634195e-08;
    const S6: f64 = 1.58969099521155010221e-10;
    let ix = hi(x) & 0x7FFFFFFF;
    if ix < 0x3E400000 && x as i32 == 0 {
        return x;
    }
    let z = x * x;
    let v = z * x;
    let r = S2 + z * (S3 + z * (S4 + z * (S5 + z * S6)));
    if iy == 0 {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (HALF * y - v * r) - y) - v * S1)
    }
}

fn kernel_tan(mut x: f64, mut y: f64, iy: i32) -> f64 {
    const T: [f64; 13] = [
        3.33333333333334091986e-01,
        1.33333333333201242699e-01,
        5.39682539762260521377e-02,
        2.18694882948595424599e-02,
        8.86323982359930005737e-03,
        3.59207910759131235356e-03,
        1.45620945432529025516e-03,
        5.88041240820264096874e-04,
        2.46463134818469906812e-04,
        7.81794442939557092300e-05,
        7.14072491382608190305e-05,
        -1.85586374855275456654e-05,
        2.59073051863633712884e-05,
    ];
    const PIO4: f64 = 7.85398163397448278999e-01;
    const PIO4LO: f64 = 3.06161699786838301793e-17;
    let hx = hi(x);
    let ix = hx & 0x7FFFFFFF;
    if ix < 0x3E300000 && x as i32 == 0 {
        if ((ix as u32 | lo(x)) | (iy + 1) as u32) == 0 {
            return 1.0 / fabs(x);
        } else if iy == 1 {
            return x;
        } else {
            let w = x + y;
            let z = set_lo(w, 0);
            let v = y - (z - x);
            let a = -1.0 / w;
            let t = set_lo(a, 0);
            let s = 1.0 + t * z;
            return t + a * (s + t * v);
        }
    }
    if ix >= 0x3FE59428 {
        if hx < 0 {
            x = -x;
            y = -y;
        }
        let z = PIO4 - x;
        let w = PIO4LO - y;
        x = z + w;
        y = 0.0;
    }
    let z = x * x;
    let w = z * z;
    let r = T[1] + w * (T[3] + w * (T[5] + w * (T[7] + w * (T[9] + w * T[11]))));
    let v = z * (T[2] + w * (T[4] + w * (T[6] + w * (T[8] + w * (T[10] + w * T[12])))));
    let s = z * x;
    let mut r = y + z * (s * (r + v) + y);
    r += T[0] * s;
    let w = x + r;
    if ix >= 0x3FE59428 {
        let v = iy as f64;
        return (1 - ((hx >> 30) & 2)) as f64 * (v - 2.0 * (x - (w * w / (w + v) - r)));
    }
    if iy == 1 {
        w
    } else {
        let z = set_lo(w, 0);
        let v = r - (z - x);
        let a = -1.0 / w;
        let t = set_lo(a, 0);
        let s = 1.0 + t * z;
        t + a * (s + t * v)
    }
}

/// Math.sin.
pub fn sin(x: f64) -> f64 {
    let ix = hi(x) & 0x7FFFFFFF;
    if ix <= 0x3FE921FB {
        kernel_sin(x, 0.0, 0)
    } else if ix >= 0x7FF00000 {
        x - x
    } else {
        let (n, y0, y1) = rem_pio2(x);
        match n & 3 {
            0 => kernel_sin(y0, y1, 1),
            1 => kernel_cos(y0, y1),
            2 => -kernel_sin(y0, y1, 1),
            _ => -kernel_cos(y0, y1),
        }
    }
}

/// Math.cos.
pub fn cos(x: f64) -> f64 {
    let ix = hi(x) & 0x7FFFFFFF;
    if ix <= 0x3FE921FB {
        kernel_cos(x, 0.0)
    } else if ix >= 0x7FF00000 {
        x - x
    } else {
        let (n, y0, y1) = rem_pio2(x);
        match n & 3 {
            0 => kernel_cos(y0, y1),
            1 => -kernel_sin(y0, y1, 1),
            2 => -kernel_cos(y0, y1),
            _ => kernel_sin(y0, y1, 1),
        }
    }
}

/// Math.tan.
pub fn tan(x: f64) -> f64 {
    let ix = hi(x) & 0x7FFFFFFF;
    if ix <= 0x3FE921FB {
        kernel_tan(x, 0.0, 1)
    } else if ix >= 0x7FF00000 {
        x - x
    } else {
        let (n, y0, y1) = rem_pio2(x);
        kernel_tan(y0, y1, 1 - ((n & 1) << 1))
    }
}

/// Math.exp: V8 returns exactly Math.E for exp(1).
pub fn exp(mut x: f64) -> f64 {
    const HALF: [f64; 2] = [0.5, -0.5];
    const O_THRESHOLD: f64 = 7.09782712893383973096e+02;
    const U_THRESHOLD: f64 = -7.45133219101941108420e+02;
    const LN2HI: [f64; 2] = [6.93147180369123816490e-01, -6.93147180369123816490e-01];
    const LN2LO: [f64; 2] = [1.90821492927058770002e-10, -1.90821492927058770002e-10];
    const INVLN2: f64 = 1.44269504088896338700e+00;
    const P1: f64 = 1.66666666666666019037e-01;
    const P2: f64 = -2.77777777770155933842e-03;
    const P3: f64 = 6.61375632143793436117e-05;
    const P4: f64 = -1.65339022054652515390e-06;
    const P5: f64 = 4.13813679705723846039e-08;
    const E: f64 = 2.718281828459045;
    const HUGE: f64 = 1.0e+300;
    const TWOM1000: f64 = 9.33263618503218878990e-302;
    const TWO1023: f64 = 8.988465674311579539e307;

    let mut hx = hi(x) as u32;
    let xsb = ((hx >> 31) & 1) as usize;
    hx &= 0x7FFFFFFF;
    let mut hi_ = 0.0;
    let mut lo_ = 0.0;
    let mut k: i32 = 0;
    if hx >= 0x40862E42 {
        if hx >= 0x7FF00000 {
            if ((hx & 0xFFFFF) | lo(x)) != 0 {
                return x + x;
            }
            return if xsb == 0 { x } else { 0.0 };
        }
        if x > O_THRESHOLD {
            return HUGE * HUGE;
        }
        if x < U_THRESHOLD {
            return TWOM1000 * TWOM1000;
        }
    }
    if hx > 0x3FD62E42 {
        if hx < 0x3FF0A2B2 {
            if x == 1.0 {
                return E;
            }
            hi_ = x - LN2HI[xsb];
            lo_ = LN2LO[xsb];
            k = 1 - xsb as i32 - xsb as i32;
        } else {
            k = (INVLN2 * x + HALF[xsb]) as i32;
            let t = k as f64;
            hi_ = x - t * LN2HI[0];
            lo_ = t * LN2LO[0];
        }
        x = hi_ - lo_;
    } else if hx < 0x3E300000 && HUGE + x > 1.0 {
        return 1.0 + x;
    }
    let t = x * x;
    let twopk = if k >= -1021 {
        words(0x3FF00000u32.wrapping_add((k as u32) << 20), 0)
    } else {
        words(0x3FF00000u32.wrapping_add(((k + 1000) as u32) << 20), 0)
    };
    let c = x - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    if k == 0 {
        return 1.0 - ((x * c) / (c - 2.0) - x);
    }
    let y = 1.0 - ((lo_ - (x * c) / (2.0 - c)) - hi_);
    if k >= -1021 {
        if k == 1024 {
            return y * 2.0 * TWO1023;
        }
        y * twopk
    } else {
        y * twopk * TWOM1000
    }
}

const LN2_HI: f64 = 6.93147180369123816490e-01;
const LN2_LO: f64 = 1.90821492927058770002e-10;
const TWO54: f64 = 1.80143985094819840000e+16;
const LG1: f64 = 6.666666666666735130e-01;
const LG2: f64 = 3.999999999940941908e-01;
const LG3: f64 = 2.857142874366239149e-01;
const LG4: f64 = 2.222219843214978396e-01;
const LG5: f64 = 1.818357216161805012e-01;
const LG6: f64 = 1.531383769920937332e-01;
const LG7: f64 = 1.479819860511658591e-01;

/// Math.log.
pub fn log(mut x: f64) -> f64 {
    let mut hx = hi(x);
    let lx = lo(x);
    let mut k: i32 = 0;
    if hx < 0x00100000 {
        if ((hx & 0x7FFFFFFF) as u32 | lx) == 0 {
            return f64::NEG_INFINITY;
        }
        if hx < 0 {
            return f64::NAN;
        }
        k -= 54;
        x *= TWO54;
        hx = hi(x);
    }
    if hx >= 0x7FF00000 {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    hx &= 0x000FFFFF;
    let mut i = (hx + 0x95F64) & 0x100000;
    x = set_hi(x, (hx | (i ^ 0x3FF00000)) as u32);
    k += i >> 20;
    let f = x - 1.0;
    if (0x000FFFFF & (2 + hx)) < 3 {
        if f == 0.0 {
            if k == 0 {
                return 0.0;
            }
            let dk = k as f64;
            return dk * LN2_HI + dk * LN2_LO;
        }
        let r = f * f * (0.5 - 0.33333333333333333 * f);
        if k == 0 {
            return f - r;
        }
        let dk = k as f64;
        return dk * LN2_HI - ((r - dk * LN2_LO) - f);
    }
    let s = f / (2.0 + f);
    let dk = k as f64;
    let z = s * s;
    i = hx - 0x6147A;
    let w = z * z;
    let j = 0x6B851 - hx;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    i |= j;
    let r = t2 + t1;
    if i > 0 {
        let hfsq = 0.5 * f * f;
        if k == 0 {
            f - (hfsq - s * (hfsq + r))
        } else {
            dk * LN2_HI - ((hfsq - (s * (hfsq + r) + dk * LN2_LO)) - f)
        }
    } else if k == 0 {
        f - s * (f - r)
    } else {
        dk * LN2_HI - ((s * (f - r) - dk * LN2_LO) - f)
    }
}

/// Math.log1p.
pub fn log1p(x: f64) -> f64 {
    let hx = hi(x);
    let ax = hx & 0x7FFFFFFF;
    let mut k: i32 = 1;
    let mut f = 0.0;
    let mut hu: i32 = 0;
    let mut c = 0.0;
    if hx < 0x3FDA827A {
        if ax >= 0x3FF00000 {
            return if x == -1.0 { f64::NEG_INFINITY } else { f64::NAN };
        }
        if ax < 0x3E200000 {
            if TWO54 + x > 0.0 && ax < 0x3C900000 {
                return x;
            }
            return x - x * x * 0.5;
        }
        if hx > 0 || hx <= 0xBFD2BEC4u32 as i32 {
            k = 0;
            f = x;
            hu = 1;
        }
    }
    if hx >= 0x7FF00000 {
        return x + x;
    }
    if k != 0 {
        let mut u;
        if hx < 0x43400000 {
            u = 1.0 + x;
            hu = hi(u);
            k = (hu >> 20) - 1023;
            c = if k > 0 { 1.0 - (u - x) } else { x - (u - 1.0) };
            c /= u;
        } else {
            u = x;
            hu = hi(u);
            k = (hu >> 20) - 1023;
            c = 0.0;
        }
        hu &= 0x000FFFFF;
        if hu < 0x6A09E {
            u = set_hi(u, (hu | 0x3FF00000) as u32);
        } else {
            k += 1;
            u = set_hi(u, (hu | 0x3FE00000) as u32);
            hu = (0x00100000 - hu) >> 2;
        }
        f = u - 1.0;
    }
    let hfsq = 0.5 * f * f;
    let dk = k as f64;
    if hu == 0 {
        if f == 0.0 {
            if k == 0 {
                return 0.0;
            }
            c += dk * LN2_LO;
            return dk * LN2_HI + c;
        }
        let r = hfsq * (1.0 - 0.66666666666666666 * f);
        if k == 0 {
            return f - r;
        }
        return dk * LN2_HI - ((r - (dk * LN2_LO + c)) - f);
    }
    let s = f / (2.0 + f);
    let z = s * s;
    let r = z * (LG1 + z * (LG2 + z * (LG3 + z * (LG4 + z * (LG5 + z * (LG6 + z * LG7))))));
    if k == 0 {
        f - (hfsq - s * (hfsq + r))
    } else {
        dk * LN2_HI - ((hfsq - (s * (hfsq + r) + (dk * LN2_LO + c))) - f)
    }
}

/// Math.log10.
pub fn log10(mut x: f64) -> f64 {
    const IVLN10: f64 = 4.34294481903251816668e-01;
    const LOG10_2HI: f64 = 3.01029995663611771306e-01;
    const LOG10_2LO: f64 = 3.69423907715893078616e-13;
    let mut hx = hi(x);
    let mut lx = lo(x);
    let mut k: i32 = 0;
    if hx < 0x00100000 {
        if ((hx & 0x7FFFFFFF) as u32 | lx) == 0 {
            return f64::NEG_INFINITY;
        }
        if hx < 0 {
            return f64::NAN;
        }
        k -= 54;
        x *= TWO54;
        hx = hi(x);
        lx = lo(x);
    }
    if hx >= 0x7FF00000 {
        return x + x;
    }
    if hx == 0x3FF00000 && lx == 0 {
        return 0.0;
    }
    k += (hx >> 20) - 1023;
    let i = ((k as u32 & 0x80000000) >> 31) as i32;
    hx = (hx & 0x000FFFFF) | ((0x3FF - i) << 20);
    let y = (k + i) as f64;
    x = words(hx as u32, lx);
    let z = y * LOG10_2LO + IVLN10 * log(x);
    z + y * LOG10_2HI
}

/// Math.acosh.
pub fn acosh(x: f64) -> f64 {
    const LN2: f64 = 6.93147180559945286227e-01;
    let hx = hi(x);
    let lx = lo(x);
    if hx < 0x3FF00000 {
        f64::NAN
    } else if hx >= 0x41B00000 {
        if hx >= 0x7FF00000 { x + x } else { log(x) + LN2 }
    } else if ((hx - 0x3FF00000) as u32 | lx) == 0 {
        0.0
    } else if hx > 0x40000000 {
        let t = x * x;
        log(2.0 * x - 1.0 / (x + sqrt(t - 1.0)))
    } else {
        let t = x - 1.0;
        log1p(t + sqrt(2.0 * t + t * t))
    }
}

/// Math.asinh.
pub fn asinh(x: f64) -> f64 {
    const LN2: f64 = 6.93147180559945286227e-01;
    const HUGE: f64 = 1.00000000000000000000e+300;
    let hx = hi(x);
    let ix = hx & 0x7FFFFFFF;
    if ix >= 0x7FF00000 {
        return x + x;
    }
    if ix < 0x3E300000 && HUGE + x > 1.0 {
        return x;
    }
    let w = if ix > 0x41B00000 {
        log(fabs(x)) + LN2
    } else if ix > 0x40000000 {
        let t = fabs(x);
        log(2.0 * t + 1.0 / (sqrt(x * x + 1.0) + t))
    } else {
        let t = x * x;
        log1p(fabs(x) + t / (1.0 + sqrt(1.0 + t)))
    };
    if hx > 0 { w } else { -w }
}

/// Math.atanh.
pub fn atanh(mut x: f64) -> f64 {
    const HUGE: f64 = 1e300;
    let hx = hi(x);
    let lx = lo(x);
    let ix = hx & 0x7FFFFFFF;
    if (ix as u32 | ((lx | lx.wrapping_neg()) >> 31)) > 0x3FF00000 {
        return f64::NAN;
    }
    if ix == 0x3FF00000 {
        return if x > 0.0 { f64::INFINITY } else { f64::NEG_INFINITY };
    }
    if ix < 0x3E300000 && (HUGE + x) > 0.0 {
        return x;
    }
    x = set_hi(x, ix as u32);
    let t = if ix < 0x3FE00000 {
        let t = x + x;
        0.5 * log1p(t + t * x / (1.0 - x))
    } else {
        0.5 * log1p((x + x) / (1.0 - x))
    };
    if hx >= 0 { t } else { -t }
}

/// Math.atan2.
pub fn atan2(y: f64, x: f64) -> f64 {
    const TINY: f64 = 1.0e-300;
    const PI_O_4: f64 = 7.8539816339744827900E-01;
    const PI_O_2: f64 = 1.5707963267948965580E+00;
    const PI: f64 = 3.1415926535897931160E+00;
    const PI_LO: f64 = 1.2246467991473531772E-16;
    let hx = hi(x);
    let lx = lo(x);
    let ix = hx & 0x7FFFFFFF;
    let hy = hi(y);
    let ly = lo(y);
    let iy = hy & 0x7FFFFFFF;
    if (ix as u32 | ((lx | lx.wrapping_neg()) >> 31)) > 0x7FF00000
        || (iy as u32 | ((ly | ly.wrapping_neg()) >> 31)) > 0x7FF00000
    {
        return x + y;
    }
    if (hx.wrapping_sub(0x3FF00000) as u32 | lx) == 0 {
        return atan(y);
    }
    let mut m = ((hy >> 31) & 1) | ((hx >> 30) & 2);
    if (iy as u32 | ly) == 0 {
        match m {
            0 | 1 => return y,
            2 => return PI + TINY,
            _ => return -PI - TINY,
        }
    }
    if (ix as u32 | lx) == 0 {
        return if hy < 0 { -PI_O_2 - TINY } else { PI_O_2 + TINY };
    }
    if ix == 0x7FF00000 {
        if iy == 0x7FF00000 {
            return match m {
                0 => PI_O_4 + TINY,
                1 => -PI_O_4 - TINY,
                2 => 3.0 * PI_O_4 + TINY,
                _ => -3.0 * PI_O_4 - TINY,
            };
        } else {
            return match m {
                0 => 0.0,
                1 => -0.0,
                2 => PI + TINY,
                _ => -PI - TINY,
            };
        }
    }
    if iy == 0x7FF00000 {
        return if hy < 0 { -PI_O_2 - TINY } else { PI_O_2 + TINY };
    }
    let k = (iy - ix) >> 20;
    let z = if k > 60 {
        m &= 1;
        PI_O_2 + 0.5 * PI_LO
    } else if hx < 0 && k < -60 {
        0.0
    } else {
        atan(fabs(y / x))
    };
    match m {
        0 => z,
        1 => -z,
        2 => PI - (z - PI_LO),
        _ => (z - PI_LO) - PI,
    }
}

/// Math.cbrt.
pub fn cbrt(x: f64) -> f64 {
    const B1: u32 = 715094163;
    const B2: u32 = 696219795;
    const P0: f64 = 1.87595182427177009643;
    const P1: f64 = -1.88497979543377169875;
    const P2: f64 = 1.621429720105354466140;
    const P3: f64 = -0.758397934778766047437;
    const P4: f64 = 0.145996192886612446982;
    let mut hx = hi(x) as u32;
    let low = lo(x);
    let sign = hx & 0x80000000;
    hx ^= sign;
    if hx >= 0x7FF00000 {
        return x + x;
    }
    let mut t;
    if hx < 0x00100000 {
        if (hx | low) == 0 {
            return x;
        }
        t = words(0x43500000, 0);
        t *= x;
        let high = hi(t) as u32;
        t = words(sign | ((high & 0x7FFFFFFF) / 3 + B2), 0);
    } else {
        t = words(sign | (hx / 3 + B1), 0);
    }
    let r = (t * t) * (t / x);
    t = t * ((P0 + r * (P1 + r * P2)) + ((r * r) * r) * (P3 + r * P4));
    t = f64::from_bits((t.to_bits().wrapping_add(0x80000000)) & 0xFFFFFFFFC0000000);
    let s = t * t;
    let mut r = x / s;
    let w = t + t;
    r = (r - t) / (w + r);
    t + t * r
}

/// Math.cosh.
pub fn cosh(x: f64) -> f64 {
    const KCOSH_OVERFLOW: f64 = 710.4758600739439;
    const HUGE: f64 = 1.0e+300;
    let ix = hi(x) & 0x7FFFFFFF;
    if ix < 0x3FD62E43 {
        let t = expm1(fabs(x));
        let w = 1.0 + t;
        if ix < 0x3C800000 {
            return w;
        }
        return 1.0 + (t * t) / (w + w);
    }
    if ix < 0x40360000 {
        let t = exp(fabs(x));
        return 0.5 * t + 0.5 / t;
    }
    if ix < 0x40862E42 {
        return 0.5 * exp(fabs(x));
    }
    if fabs(x) <= KCOSH_OVERFLOW {
        let w = exp(0.5 * fabs(x));
        let t = 0.5 * w;
        return t * w;
    }
    if ix >= 0x7FF00000 {
        return x * x;
    }
    HUGE * HUGE
}

/// Math.sinh.
pub fn sinh(x: f64) -> f64 {
    const KSINH_OVERFLOW: f64 = 710.4758600739439;
    const TWO_M28: f64 = 3.725290298461914e-9;
    const LOG_MAXD: f64 = 709.7822265625;
    const SHUGE: f64 = 1.0e307;
    let h = if x < 0.0 { -0.5 } else { 0.5 };
    let ax = fabs(x);
    if ax < 22.0 {
        if ax < TWO_M28 {
            return x;
        }
        let t = expm1(ax);
        if ax < 1.0 {
            return h * (2.0 * t - t * t / (t + 1.0));
        }
        return h * (t + t / (t + 1.0));
    }
    if ax < LOG_MAXD {
        return h * exp(ax);
    }
    if ax <= KSINH_OVERFLOW {
        let w = exp(0.5 * ax);
        let t = h * w;
        return t * w;
    }
    x * SHUGE
}

/// Math.tanh.
pub fn tanh(x: f64) -> f64 {
    const TINY: f64 = 1.0e-300;
    const HUGE: f64 = 1.0e300;
    let jx = hi(x);
    let ix = jx & 0x7FFFFFFF;
    if ix >= 0x7FF00000 {
        return if jx >= 0 { 1.0 / x + 1.0 } else { 1.0 / x - 1.0 };
    }
    let z;
    if ix < 0x40360000 {
        if ix < 0x3E300000 && HUGE + x > 1.0 {
            return x;
        }
        if ix >= 0x3FF00000 {
            let t = expm1(2.0 * fabs(x));
            z = 1.0 - 2.0 / (t + 2.0);
        } else {
            let t = expm1(-2.0 * fabs(x));
            z = -t / (t + 2.0);
        }
    } else {
        z = 1.0 - TINY;
    }
    if jx >= 0 { z } else { -z }
}

/// Two-argument hypot as V8's Math.hypot computes it for two arguments (FastMathHypot in
/// src/builtins/math.tq): both scaled by the larger, so hypot(0, x) is |x| exactly and
/// Math.hypot(a, b), which djs folds pairwise from 0, matches V8.
pub fn hypot(x: f64, y: f64) -> f64 {
    let a = fabs(x);
    let b = fabs(y);
    if a == f64::INFINITY || b == f64::INFINITY {
        return f64::INFINITY;
    }
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    let max = if a > b { a } else { b };
    if max == 0.0 {
        return 0.0;
    }
    sqrt((a / max) * (a / max) + (b / max) * (b / max)) * max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_one_is_e() {
        assert_eq!(exp(1.0).to_bits(), core::f64::consts::E.to_bits());
    }

    #[test]
    fn values_match_v8() {
        // (function, x, the bits node 24 returns)
        assert_eq!(log10(1000.0), 3.0);
        assert_eq!(acosh(3.0).to_bits(), 1.7627471740390859f64.to_bits());
        assert_eq!(cbrt(27.0), 3.0);
        assert_eq!(sin(1e300).to_bits(), (-0.8178819121159085f64).to_bits());
        assert_eq!(cos(1e22).to_bits(), 0.523214785395139f64.to_bits());
        assert_eq!(hypot(3.0, 4.0), 5.0);
    }
}
