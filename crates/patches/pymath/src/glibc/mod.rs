//! Ports of glibc's libm routines that CPython 3.12 on Linux returns bit for
//! bit, used natively and on wasm alike. The native binary can't take them
//! from the system libm: nanocode-sys exports djs_libm's fdlibm versions under
//! the C names (atan2, log, log2, log10, sin, cos, ...) for the JavaScript
//! engine, which shadow glibc's.

mod atan2;
mod atan2_tbl;
mod exp;
mod exp_tbl;
mod log;
mod log_tbl;
mod sincos;
mod sincos_tbl;

pub(crate) use atan2::atan2;
pub(crate) use exp::exp;
pub(crate) use log::{log, log2, log10};
pub(crate) use sincos::{cos, sin};

#[cfg(test)]
mod tests {
    // glibc 2.39 results (CPython 3.12 on Linux), where djs_libm's differ.
    #[test]
    fn matches_glibc() {
        let cases: [(f64, f64); 5] = [
            (super::sin(2.3244968667700685), 0.7291614332842077),
            (super::exp(-558.6901362520275), 2.3118344545091706e-243),
            (super::atan2(-1.713100038389232, 3.9121385928035437), -0.412740671008641),
            (super::log(1.0638503527860121), 0.06189473515008555),
            (super::log10(0.9417658139537338), -0.026057078472524864),
        ];
        for (got, want) in cases {
            assert_eq!(got.to_bits(), want.to_bits(), "{got:e} != {want:e}");
        }
    }
}
