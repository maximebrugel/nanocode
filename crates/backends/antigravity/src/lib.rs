#[cfg(not(target_family = "wasm"))]
mod driver;
mod harness;
#[cfg(not(target_family = "wasm"))]
mod nanotools;
mod profile;
#[cfg(not(target_family = "wasm"))]
mod prompts;
#[cfg(not(target_family = "wasm"))]
mod request;
#[cfg(not(target_family = "wasm"))]
mod search;

pub use harness::{AntigravityHarness, DEFAULT_MODEL, FAMILY};
pub use nanocode_backend_antigravity_client::{
    Auth, AuthMethod, CacheConfig, Client, Endpoint, GoogleAuth, GoogleConfig, RetryPolicy, Route,
    vertex_base_url,
};
pub use profile::Profile;
