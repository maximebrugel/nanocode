//! sin and cos as glibc 2.28+ computes them (sysdeps/ieee754/dbl-64/s_sin.c
//! and branred.c, IBM Accurate Mathematical Library), so math.sin and
//! math.cos return what CPython does on Linux: djs_libm's fdlibm versions,
//! which the binary exports for the JavaScript engine, differ from glibc's in
//! about 3 percent of inputs. x86_64 glibc runs s_sin.c built with -mfma,
//! where gcc fuses each product that feeds additions only: fma() marks the
//! same places. branred.c is built without fma, as written.

use super::sincos_tbl::SINCOSTAB;

#[inline(always)]
fn fma(a: f64, b: f64, c: f64) -> f64 {
    a.mul_add(b, c)
}

#[inline(always)]
const fn h(bits: u64) -> f64 {
    f64::from_bits(bits)
}

const S1: f64 = h(0xbfc5_5555_5555_5555);
const S2: f64 = h(0x3f81_1111_1111_0ece);
const S3: f64 = h(0xbf2a_01a0_19db_08b8);
const S4: f64 = h(0x3ec7_1de2_7b9a_7ed9);
const S5: f64 = h(0xbe5a_ddff_c2fc_df59);
const BIG: f64 = h(0x42c8_0000_0000_0000); // 0x1.8p45
const HP0: f64 = h(0x3ff9_21fb_5444_2d18);
const HP1: f64 = h(0x3c91_a626_3314_5c07);
const MP1: f64 = h(0x3ff9_21fb_5800_0000);
const MP2: f64 = h(0xbe4d_de97_3c00_0000);
const PP3: f64 = h(0xbc8c_b3b3_9800_0000);
const PP4: f64 = h(0xbacd_747f_23e3_2ed7);
const HPINV: f64 = h(0x3fe4_5f30_6dc9_c883);
const TOINT: f64 = h(0x4338_0000_0000_0000); // 0x1.8p52

const SN3: f64 = h(0xbfc5_5555_5555_5515);
const SN5: f64 = h(0x3f81_1110_e829_872f);
const CS2: f64 = 0.5;
const CS4: f64 = h(0xbfa5_5555_5555_5535);
const CS6: f64 = h(0x3f56_c16b_edd9_e239);

#[inline(always)]
fn lookup(u: f64) -> (f64, f64, f64, f64) {
    let k = ((u.to_bits() as u32) << 2) as usize;
    (h(SINCOSTAB[k]), h(SINCOSTAB[k + 1]), h(SINCOSTAB[k + 2]), h(SINCOSTAB[k + 3]))
}

#[inline(always)]
fn do_cos(x: f64, dx: f64) -> f64 {
    let dx = if x < 0.0 { -dx } else { dx };
    let u = BIG + x.abs();
    let x = x.abs() - (u - BIG) + dx;
    let xx = x * x;
    let s = fma(x * xx, fma(xx, SN5, SN3), x);
    let c = xx * fma(xx, fma(xx, CS6, CS4), CS2);
    let (sn, ssn, cs, ccs) = lookup(u);
    let cor = fma(-sn, s, fma(-cs, c, fma(-s, ssn, ccs)));
    cs + cor
}

#[inline(always)]
fn do_sin(x: f64, dx: f64) -> f64 {
    let xold = x;
    if x.abs() < 0.126 {
        let xx = x * x;
        let p = fma(fma(fma(fma(S5, xx, S4), xx, S3), xx, S2), xx, S1);
        let t = fma(fma(p, x, -(0.5 * dx)), xx, dx);
        return x + t;
    }
    let dx = if x <= 0.0 { -dx } else { dx };
    let u = BIG + x.abs();
    let x = x.abs() - (u - BIG);
    let xx = x * x;
    let s = x + fma(x * xx, fma(xx, SN5, SN3), dx);
    let c = fma(x, dx, xx * fma(xx, fma(xx, CS6, CS4), CS2));
    let (sn, ssn, cs, ccs) = lookup(u);
    let cor = fma(cs, s, fma(-sn, c, fma(s, ccs, ssn)));
    (sn + cor).copysign(xold)
}

#[inline(always)]
fn reduce_sincos(x: f64) -> (f64, f64, u32) {
    let t = fma(x, HPINV, TOINT);
    let xn = t - TOINT;
    let y = fma(-xn, MP2, fma(-xn, MP1, x));
    let n = (t.to_bits() as u32) & 3;
    // t1 = xn * pp3 feeds only subtractions, so gcc fuses it into each.
    let t2 = fma(-xn, PP3, y);
    let mut db = fma(-xn, PP3, y - t2);
    let b = fma(-xn, PP4, t2);
    db += fma(-xn, PP4, t2 - b);
    (b, db, n)
}

#[inline(always)]
fn do_sincos(a: f64, da: f64, n: u32) -> f64 {
    let r = if n & 1 != 0 { do_cos(a, da) } else { do_sin(a, da) };
    if n & 2 != 0 { -r } else { r }
}

const TOVERP: [f64; 75] = [
    10680707.0, 7228996.0, 1387004.0, 2578385.0, 16069853.0, 12639074.0, 9804092.0, 4427841.0,
    16666979.0, 11263675.0, 12935607.0, 2387514.0, 4345298.0, 14681673.0, 3074569.0, 13734428.0,
    16653803.0, 1880361.0, 10960616.0, 8533493.0, 3062596.0, 8710556.0, 7349940.0, 6258241.0,
    3772886.0, 3769171.0, 3798172.0, 8675211.0, 12450088.0, 3874808.0, 9961438.0, 366607.0,
    15675153.0, 9132554.0, 7151469.0, 3571407.0, 2607881.0, 12013382.0, 4155038.0, 6285869.0,
    7677882.0, 13102053.0, 15825725.0, 473591.0, 9065106.0, 15363067.0, 6271263.0, 9264392.0,
    5636912.0, 4652155.0, 7056368.0, 13614112.0, 10155062.0, 1944035.0, 9527646.0, 15080200.0,
    6658437.0, 6231200.0, 6832269.0, 16767104.0, 5075751.0, 3212806.0, 1398474.0, 7579849.0,
    6349435.0, 12618859.0, 4703257.0, 12806093.0, 14477321.0, 2786137.0, 12875403.0, 9837734.0,
    14528324.0, 13719321.0, 343717.0,
];

/// One half of branred: x times 2/pi in base 2^24 pieces.
#[inline(always)]
fn branred_part(xp: f64) -> (f64, f64, f64) {
    const T576: f64 = h(0x63f0_0000_0000_0000);
    const TM24: f64 = h(0x3e70_0000_0000_0000);
    const BIGB: f64 = h(0x4338_0000_0000_0000);
    const BIG1: f64 = h(0x4358_0000_0000_0000);
    let mut sum = 0.0;
    let mut k = (((xp.to_bits() >> 52) & 2047) as i32 - 450) / 24;
    if k < 0 {
        k = 0;
    }
    let k = k as usize;
    let mut gor = h(T576.to_bits() - (((k as u64) * 24) << 52));
    let mut r = [0.0f64; 6];
    for (i, ri) in r.iter_mut().enumerate() {
        *ri = xp * TOVERP[k + i] * gor;
        gor *= TM24;
    }
    for ri in r.iter_mut().take(3) {
        let s = (*ri + BIGB) - BIGB;
        sum += s;
        *ri -= s;
    }
    let mut t = 0.0;
    for ri in r.iter().rev() {
        t += *ri;
    }
    let bb = (((((r[0] - t) + r[1]) + r[2]) + r[3]) + r[4]) + r[5];
    let s = (t + BIGB) - BIGB;
    sum += s;
    t -= s;
    let b = t + bb;
    let bb = (t - b) + bb;
    let s = (sum + BIG1) - BIG1;
    sum -= s;
    (b, bb, sum)
}

/// glibc's __branred: x = n*pi/2 + (a + aa) with |a + aa| < pi/4, n mod 4.
fn branred(x: f64) -> (f64, f64, u32) {
    const TM600: f64 = h(0x1a70_0000_0000_0000);
    const SPLIT: f64 = 134_217_729.0;
    let x = x * TM600;
    let t = x * SPLIT;
    let x1 = t - (t - x);
    let x2 = x - x1;
    let (b1, bb1, sum1) = branred_part(x1);
    let (b2, bb2, sum2) = branred_part(x2);
    let mut sum = sum1 + sum2;
    let mut b = b1 + b2;
    let bb = if b1.abs() > b2.abs() { (b1 - b) + b2 } else { (b2 - b) + b1 };
    if b > 0.5 {
        b -= 1.0;
        sum += 1.0;
    } else if b < -0.5 {
        b += 1.0;
        sum -= 1.0;
    }
    let s = b + (bb + bb1 + bb2);
    let t = ((b - s) + bb) + (bb1 + bb2);
    let b = s * SPLIT;
    let t1 = b - (b - s);
    let t2 = s - t1;
    let b = s * HP0;
    let bb = (((t1 * MP1 - b) + t1 * h(0xbe4d_de97_4000_0000)) + t2 * MP1)
        + (t2 * h(0xbe4d_de97_4000_0000) + s * HP1 + t * HP0);
    let s = b + bb;
    let t = (b - s) + bb;
    (s, t, (sum as i32 & 3) as u32)
}

pub fn sin(x: f64) -> f64 {
    let k = ((x.to_bits() >> 32) as u32) & 0x7fff_ffff;
    if k < 0x3e50_0000 {
        x
    } else if k < 0x3feb_6000 {
        do_sin(x, 0.0)
    } else if k < 0x4003_68fd {
        let t = HP0 - x.abs();
        do_cos(t, HP1).copysign(x)
    } else if k < 0x4199_21fb {
        let (a, da, n) = reduce_sincos(x);
        do_sincos(a, da, n)
    } else if k < 0x7ff0_0000 {
        let (a, da, n) = branred(x);
        do_sincos(a, da, n)
    } else {
        x / x
    }
}

pub fn cos(x: f64) -> f64 {
    let k = ((x.to_bits() >> 32) as u32) & 0x7fff_ffff;
    if k < 0x3e40_0000 {
        1.0
    } else if k < 0x3feb_6000 {
        do_cos(x, 0.0)
    } else if k < 0x4003_68fd {
        let y = HP0 - x.abs();
        let a = y + HP1;
        let da = (y - a) + HP1;
        do_sin(a, da)
    } else if k < 0x4199_21fb {
        let (a, da, n) = reduce_sincos(x);
        do_sincos(a, da, n + 1)
    } else if k < 0x7ff0_0000 {
        let (a, da, n) = branred(x);
        do_sincos(a, da, n + 1)
    } else {
        x / x
    }
}
