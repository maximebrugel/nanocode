#![allow(clippy::approx_constant)] // many false positives

macro_rules! force_eval {
    ($e:expr) => {
        unsafe { ::core::ptr::read_volatile(&$e) }
    };
}

#[cfg(not(debug_assertions))]
macro_rules! i {
    ($array:expr, $index:expr) => {
        unsafe { *$array.get_unchecked($index) }
    };
    ($array:expr, $index:expr, = , $rhs:expr) => {
        unsafe {
            *$array.get_unchecked_mut($index) = $rhs;
        }
    };
    ($array:expr, $index:expr, += , $rhs:expr) => {
        unsafe {
            *$array.get_unchecked_mut($index) += $rhs;
        }
    };
    ($array:expr, $index:expr, -= , $rhs:expr) => {
        unsafe {
            *$array.get_unchecked_mut($index) -= $rhs;
        }
    };
    ($array:expr, $index:expr, &= , $rhs:expr) => {
        unsafe {
            *$array.get_unchecked_mut($index) &= $rhs;
        }
    };
    ($array:expr, $index:expr, == , $rhs:expr) => {
        unsafe { *$array.get_unchecked_mut($index) == $rhs }
    };
}

#[cfg(debug_assertions)]
macro_rules! i {
    ($array:expr, $index:expr) => {
        *$array.get($index).unwrap()
    };
    ($array:expr, $index:expr, = , $rhs:expr) => {
        *$array.get_mut($index).unwrap() = $rhs;
    };
    ($array:expr, $index:expr, -= , $rhs:expr) => {
        *$array.get_mut($index).unwrap() -= $rhs;
    };
    ($array:expr, $index:expr, += , $rhs:expr) => {
        *$array.get_mut($index).unwrap() += $rhs;
    };
    ($array:expr, $index:expr, &= , $rhs:expr) => {
        *$array.get_mut($index).unwrap() &= $rhs;
    };
    ($array:expr, $index:expr, == , $rhs:expr) => {
        *$array.get_mut($index).unwrap() == $rhs
    };
}

#[macro_use]
pub(crate) mod support;

mod generic;

// Private modules
mod arch;

// Private re-imports
#[allow(unused_imports)]
use self::support::{CastFrom, CastInto, DInt, Float, HInt, Int, IntTy, MinInt};

// Public modules
mod acos;
mod asin;
mod atan;
mod ceil;
mod expm1;
mod fabs;
mod floor;
mod fma;
mod fmod;
mod ldexp;
mod log2;
mod pow;
mod scalbn;
mod sqrt;
mod trunc;
// V8's fdlibm (src/base/ieee754.cc), for the functions where musl's rounding differs.
mod v8;

// Use separated imports instead of {}-grouped imports for easier merging.
pub use self::acos::acos;
pub use self::asin::asin;
pub use self::atan::atan;
pub use self::ceil::ceil;
pub use self::expm1::expm1;
pub use self::fabs::fabs;
pub use self::floor::floor;
pub use self::fma::fma;
pub use self::fmod::fmod;
pub use self::ldexp::ldexp;
pub use self::log2::log2;
pub use self::pow::pow;
pub use self::scalbn::scalbn;
pub use self::sqrt::sqrt;
pub use self::trunc::trunc;
pub use self::v8::{
    acosh, asinh, atan2, atanh, cbrt, cos, cosh, exp, hypot, log, log1p, log10, sin, sinh, tan,
    tanh,
};

#[inline]
fn get_high_word(x: f64) -> u32 {
    (x.to_bits() >> 32) as u32
}

#[inline]
fn get_low_word(x: f64) -> u32 {
    x.to_bits() as u32
}

#[inline]
fn with_set_low_word(f: f64, lo: u32) -> f64 {
    let mut tmp = f.to_bits();
    tmp &= 0xffffffff_00000000;
    tmp |= lo as u64;
    f64::from_bits(tmp)
}
