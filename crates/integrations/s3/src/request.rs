use nanocode_http::Method;

use crate::encode::{base64, hex, uri_encode, utc};
use crate::error::{ConfigError, Error};
use crate::sign::{ALGORITHM, Canonical, SHA256_BYTES, scope, signature, signed_header_names};

pub const KEY_BYTES_MAX: usize = 1_024;
pub const PRESIGN_EXPIRES_SECONDS_MAX: u64 = 604_800;
const BUCKET_BYTES_MIN: usize = 3;
const BUCKET_BYTES_MAX: usize = 63;
const PORT_HTTP: u16 = 80;
const PORT_HTTPS: u16 = 443;
const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addressing {
    Path,
    VirtualHost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scheme {
    Http,
    Https,
}

impl Scheme {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Http => "http://",
            Self::Https => "https://",
        }
    }

    const fn default_port(self) -> u16 {
        match self {
            Self::Http => PORT_HTTP,
            Self::Https => PORT_HTTPS,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub addressing: Addressing,
    pub object_bytes_max: u64,
    pub response_bytes_max: u64,
}

#[derive(Clone)]
pub struct Config {
    scheme: Scheme,
    authority: String,
    region: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
    addressing: Addressing,
    object_bytes_max: u64,
    response_bytes_max: u64,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Config")
            .field(
                "endpoint",
                &format_args!("{}{}", self.scheme.prefix(), self.authority),
            )
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("addressing", &self.addressing)
            .field("object_bytes_max", &self.object_bytes_max)
            .field("response_bytes_max", &self.response_bytes_max)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    offset: u64,
    bytes: u64,
}

impl ByteRange {
    pub const fn new(offset: u64, bytes: u64) -> Option<Self> {
        if bytes == 0 || offset.checked_add(bytes).is_none() {
            return None;
        }
        Some(Self { offset, bytes })
    }

    pub const fn offset(self) -> u64 {
        self.offset
    }

    pub const fn bytes(self) -> u64 {
        self.bytes
    }

    pub const fn last(self) -> u64 {
        self.offset + self.bytes - 1
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presigned {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub expires_at_unix_seconds: u64,
}

struct Target {
    host: String,
    path: String,
}

fn parse_endpoint(endpoint: &str) -> Result<(Scheme, String), ConfigError> {
    let (scheme, rest) = if let Some(rest) = endpoint.strip_prefix(Scheme::Https.prefix()) {
        (Scheme::Https, rest)
    } else if let Some(rest) = endpoint.strip_prefix(Scheme::Http.prefix()) {
        (Scheme::Http, rest)
    } else {
        return Err(ConfigError::EndpointScheme);
    };
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    let valid =
        |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b':' | b'[' | b']');
    if authority.is_empty() || !authority.bytes().all(valid) {
        return Err(ConfigError::EndpointAuthority);
    }
    let host_end = authority.rfind(']').map_or(0, |index| index + 1);
    let Some(colon) = authority[host_end..]
        .rfind(':')
        .map(|index| index + host_end)
    else {
        return Ok((scheme, authority.to_ascii_lowercase()));
    };
    let host = &authority[..colon];
    if host.is_empty() {
        return Err(ConfigError::EndpointAuthority);
    }
    let port = authority[colon + 1..]
        .parse::<u16>()
        .ok()
        .filter(|&port| port > 0)
        .ok_or(ConfigError::EndpointPort)?;
    let host = host.to_ascii_lowercase();
    if port == scheme.default_port() {
        return Ok((scheme, host));
    }
    Ok((scheme, format!("{host}:{port}")))
}

fn bucket_valid(bucket: &str) -> bool {
    let bytes = bucket.as_bytes();
    let edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    (BUCKET_BYTES_MIN..=BUCKET_BYTES_MAX).contains(&bytes.len())
        && edge(bytes[0])
        && edge(bytes[bytes.len() - 1])
        && bytes
            .iter()
            .all(|&byte| edge(byte) || byte == b'.' || byte == b'-')
}

fn key_valid(key: &str) -> Result<(), Error> {
    if key.is_empty() {
        return Err(Error::KeyEmpty);
    }
    if key.len() > KEY_BYTES_MAX {
        return Err(Error::KeyTooLong {
            bytes: key.len(),
            bytes_max: KEY_BYTES_MAX,
        });
    }
    if key
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(Error::KeyDotSegment);
    }
    Ok(())
}

const fn expires_valid(seconds: u64) -> Result<(), Error> {
    if seconds == 0 || seconds > PRESIGN_EXPIRES_SECONDS_MAX {
        return Err(Error::ExpiresInvalid {
            seconds,
            seconds_max: PRESIGN_EXPIRES_SECONDS_MAX,
        });
    }
    Ok(())
}

impl Config {
    pub fn new(settings: Settings) -> Result<Self, ConfigError> {
        let (scheme, authority) = parse_endpoint(&settings.endpoint)?;
        if !bucket_valid(&settings.bucket) {
            return Err(ConfigError::Bucket);
        }
        let region_byte =
            |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
        if settings.region.is_empty() || !settings.region.bytes().all(region_byte) {
            return Err(ConfigError::Region);
        }
        let key_byte = |byte: u8| byte.is_ascii_graphic() && byte != b'/';
        if settings.access_key_id.is_empty() || !settings.access_key_id.bytes().all(key_byte) {
            return Err(ConfigError::AccessKeyId);
        }
        let secret_byte = |byte: u8| byte.is_ascii_graphic();
        if settings.secret_access_key.is_empty()
            || !settings.secret_access_key.bytes().all(secret_byte)
        {
            return Err(ConfigError::SecretAccessKey);
        }
        Ok(Self {
            scheme,
            authority,
            region: settings.region,
            bucket: settings.bucket,
            access_key_id: settings.access_key_id,
            secret_access_key: settings.secret_access_key,
            addressing: settings.addressing,
            object_bytes_max: settings.object_bytes_max,
            response_bytes_max: settings.response_bytes_max,
        })
    }

    pub const fn object_bytes_max(&self) -> u64 {
        self.object_bytes_max
    }

    pub const fn response_bytes_max(&self) -> u64 {
        self.response_bytes_max
    }

    fn target(&self, key: &str) -> Result<Target, Error> {
        key_valid(key)?;
        let mut path = String::with_capacity(self.bucket.len() + key.len() * 3 + 2);
        path.push('/');
        let host = match self.addressing {
            Addressing::Path => {
                uri_encode(&mut path, &self.bucket, false);
                path.push('/');
                self.authority.clone()
            }
            Addressing::VirtualHost => format!("{}.{}", self.bucket, self.authority),
        };
        uri_encode(&mut path, key, true);
        assert!(path.len() > 1);
        Ok(Target { host, path })
    }

    fn url(&self, target: &Target) -> String {
        format!("{}{}{}", self.scheme.prefix(), target.host, target.path)
    }

    fn signed(
        &self,
        method: Method,
        key: &str,
        mut headers: Vec<(&'static str, String)>,
        payload_sha256: String,
        now_unix_seconds: u64,
    ) -> Result<Prepared, Error> {
        let target = self.target(key)?;
        let time = utc(now_unix_seconds);
        headers.push(("host", target.host.clone()));
        headers.push(("x-amz-content-sha256", payload_sha256.clone()));
        headers.push(("x-amz-date", time.amz_date.clone()));
        headers.sort_by(|left, right| left.0.cmp(right.0));
        let canonical = Canonical {
            method: method.as_str(),
            path: &target.path,
            query: "",
            headers: &headers,
            payload_sha256: &payload_sha256,
        };
        let signature = signature(&self.secret_access_key, &self.region, &time, &canonical);
        let authorization = format!(
            "{ALGORITHM} Credential={}/{}, SignedHeaders={}, Signature={signature}",
            self.access_key_id,
            scope(&time, &self.region),
            signed_header_names(&headers)
        );
        headers.push(("authorization", authorization));
        Ok(Prepared {
            method,
            url: self.url(&target),
            headers,
        })
    }

    pub fn put_if_absent(
        &self,
        key: &str,
        bytes: u64,
        sha256: &[u8; SHA256_BYTES],
        now_unix_seconds: u64,
    ) -> Result<Prepared, Error> {
        if bytes > self.object_bytes_max {
            return Err(Error::ObjectTooLarge {
                bytes,
                bytes_max: self.object_bytes_max,
            });
        }
        let headers = vec![
            ("if-none-match", "*".to_owned()),
            ("x-amz-checksum-sha256", base64(sha256)),
        ];
        self.signed(Method::PUT, key, headers, hex(sha256), now_unix_seconds)
    }

    pub fn get(
        &self,
        key: &str,
        range: Option<ByteRange>,
        now_unix_seconds: u64,
    ) -> Result<Prepared, Error> {
        let mut headers = Vec::new();
        if let Some(range) = range {
            if range.bytes() > self.response_bytes_max {
                return Err(Error::ResponseTooLarge {
                    operation: crate::Operation::Get,
                    bytes_max: self.response_bytes_max,
                });
            }
            headers.push((
                "range",
                format!("bytes={}-{}", range.offset(), range.last()),
            ));
        }
        self.signed(
            Method::GET,
            key,
            headers,
            EMPTY_PAYLOAD_SHA256.to_owned(),
            now_unix_seconds,
        )
    }

    pub fn head(&self, key: &str, now_unix_seconds: u64) -> Result<Prepared, Error> {
        let headers = vec![("x-amz-checksum-mode", "ENABLED".to_owned())];
        self.signed(
            Method::HEAD,
            key,
            headers,
            EMPTY_PAYLOAD_SHA256.to_owned(),
            now_unix_seconds,
        )
    }

    pub fn delete(&self, key: &str, now_unix_seconds: u64) -> Result<Prepared, Error> {
        self.signed(
            Method::DELETE,
            key,
            Vec::new(),
            EMPTY_PAYLOAD_SHA256.to_owned(),
            now_unix_seconds,
        )
    }

    fn presigned(
        &self,
        method: Method,
        key: &str,
        mut headers: Vec<(&'static str, String)>,
        expires_seconds: u64,
        now_unix_seconds: u64,
    ) -> Result<Presigned, Error> {
        expires_valid(expires_seconds)?;
        let target = self.target(key)?;
        let time = utc(now_unix_seconds);
        headers.push(("host", target.host.clone()));
        headers.sort_by(|left, right| left.0.cmp(right.0));
        let mut query = String::new();
        let credential = format!("{}/{}", self.access_key_id, scope(&time, &self.region));
        let pairs = [
            ("X-Amz-Algorithm", ALGORITHM.to_owned()),
            ("X-Amz-Credential", credential),
            ("X-Amz-Date", time.amz_date.clone()),
            ("X-Amz-Expires", expires_seconds.to_string()),
            ("X-Amz-SignedHeaders", signed_header_names(&headers)),
        ];
        assert!(pairs.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for (index, (name, value)) in pairs.iter().enumerate() {
            if index > 0 {
                query.push('&');
            }
            query.push_str(name);
            query.push('=');
            uri_encode(&mut query, value, false);
        }
        let canonical = Canonical {
            method: method.as_str(),
            path: &target.path,
            query: &query,
            headers: &headers,
            payload_sha256: UNSIGNED_PAYLOAD,
        };
        let signature = signature(&self.secret_access_key, &self.region, &time, &canonical);
        let url = format!("{}?{query}&X-Amz-Signature={signature}", self.url(&target));
        headers.retain(|(name, _)| *name != "host");
        Ok(Presigned {
            method,
            url,
            headers,
            expires_at_unix_seconds: now_unix_seconds + expires_seconds,
        })
    }

    pub fn presign_get(
        &self,
        key: &str,
        expires_seconds: u64,
        now_unix_seconds: u64,
    ) -> Result<Presigned, Error> {
        self.presigned(
            Method::GET,
            key,
            Vec::new(),
            expires_seconds,
            now_unix_seconds,
        )
    }

    pub fn presign_put(
        &self,
        key: &str,
        bytes: u64,
        sha256: &[u8; SHA256_BYTES],
        expires_seconds: u64,
        now_unix_seconds: u64,
    ) -> Result<Presigned, Error> {
        if bytes > self.object_bytes_max {
            return Err(Error::ObjectTooLarge {
                bytes,
                bytes_max: self.object_bytes_max,
            });
        }
        let headers = vec![
            ("content-length", bytes.to_string()),
            ("if-none-match", "*".to_owned()),
            ("x-amz-checksum-sha256", base64(sha256)),
        ];
        self.presigned(Method::PUT, key, headers, expires_seconds, now_unix_seconds)
    }
}
