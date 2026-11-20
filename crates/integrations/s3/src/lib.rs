#![forbid(unsafe_code)]

mod encode;
mod error;
mod request;
mod response;
mod sign;

#[cfg(not(target_family = "wasm"))]
mod client;

#[cfg(not(target_family = "wasm"))]
pub use client::Client;
pub use encode::{base64, base64_decode_sha256, hex};
pub use error::{ConfigError, Error, Operation};
pub use request::{
    Addressing, ByteRange, Config, KEY_BYTES_MAX, PRESIGN_EXPIRES_SECONDS_MAX, Prepared, Presigned,
    Settings,
};
pub use response::{
    ContentRange, Head, Put, delete_status, error_code, get_expect, head_response, put_status,
    wants_error_code,
};
pub use sign::{SHA256_BYTES, hmac_sha256};
