use nanocode_digest::Sha256;

use crate::encode::{UtcTime, hex};

pub const SHA256_BYTES: usize = Sha256::OUTPUT_BYTES;
const SHA256_BLOCK_BYTES: usize = 64;
const HMAC_INNER_PAD: u8 = 0x36;
const HMAC_OUTER_PAD: u8 = 0x5c;
pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const SERVICE: &str = "s3";
const SCOPE_TERMINATOR: &str = "aws4_request";
const SIGNATURE_HEX_CHARS: usize = SHA256_BYTES * 2;

pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; SHA256_BYTES] {
    let mut block = [0u8; SHA256_BLOCK_BYTES];
    if key.len() > SHA256_BLOCK_BYTES {
        block[..SHA256_BYTES].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(&block.map(|byte| byte ^ HMAC_INNER_PAD));
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(&block.map(|byte| byte ^ HMAC_OUTER_PAD));
    outer.update(&inner_hash);
    outer.finalize()
}

pub fn scope(time: &UtcTime, region: &str) -> String {
    format!("{}/{region}/{SERVICE}/{SCOPE_TERMINATOR}", time.date)
}

pub fn signed_header_names(headers: &[(&'static str, String)]) -> String {
    let mut names = String::new();
    for (index, (name, _)) in headers.iter().enumerate() {
        if index > 0 {
            names.push(';');
        }
        names.push_str(name);
    }
    names
}

pub struct Canonical<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    pub headers: &'a [(&'static str, String)],
    pub payload_sha256: &'a str,
}

fn canonical_request(canonical: &Canonical<'_>) -> String {
    assert!(canonical.path.starts_with('/'));
    assert!(!canonical.headers.is_empty());
    assert!(
        canonical
            .headers
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0)
    );
    let mut text = String::new();
    text.push_str(canonical.method);
    text.push('\n');
    text.push_str(canonical.path);
    text.push('\n');
    text.push_str(canonical.query);
    text.push('\n');
    for (name, value) in canonical.headers {
        assert!(name.bytes().all(|byte| !byte.is_ascii_uppercase()));
        text.push_str(name);
        text.push(':');
        text.push_str(value.trim());
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&signed_header_names(canonical.headers));
    text.push('\n');
    text.push_str(canonical.payload_sha256);
    text
}

pub fn signature(
    secret_access_key: &str,
    region: &str,
    time: &UtcTime,
    canonical: &Canonical<'_>,
) -> String {
    let request = canonical_request(canonical);
    let string_to_sign = format!(
        "{ALGORITHM}\n{}\n{}\n{}",
        time.amz_date,
        scope(time, region),
        hex(&Sha256::digest(request.as_bytes()))
    );
    let secret = format!("AWS4{secret_access_key}");
    let date_key = hmac_sha256(secret.as_bytes(), time.date.as_bytes());
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, SERVICE.as_bytes());
    let signing_key = hmac_sha256(&service_key, SCOPE_TERMINATOR.as_bytes());
    let signature = hex(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    assert_eq!(signature.len(), SIGNATURE_HEX_CHARS);
    signature
}
