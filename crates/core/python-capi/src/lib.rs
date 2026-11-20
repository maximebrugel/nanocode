#![allow(
    non_snake_case,
    non_upper_case_globals,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::missing_const_for_fn
)]

mod abi;
mod address;
pub mod api;
mod bridge;
mod buffer;
mod cast;
mod cxx;
mod dconv;
pub(crate) mod extra;
mod heap;
#[cfg(target_family = "wasm")]
mod libc;
mod module;
mod types;

pub use module::{
    EXTENSIONS, Extension, add_extensions, extension_names, module_defs, run_constructors,
};
