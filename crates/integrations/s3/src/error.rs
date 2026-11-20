use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Put,
    Get,
    Head,
    Delete,
}

impl Operation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Get => "get",
            Self::Head => "head",
            Self::Delete => "delete",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    EndpointScheme,
    EndpointAuthority,
    EndpointPort,
    Bucket,
    Region,
    AccessKeyId,
    SecretAccessKey,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EndpointScheme => "endpoint must start with http:// or https://",
            Self::EndpointAuthority => "endpoint must be a host with an optional port and no path",
            Self::EndpointPort => "endpoint port must be a number from 1 to 65535",
            Self::Bucket => "bucket must be 3 to 63 lowercase letters, digits, dots or hyphens, starting and ending with a letter or digit",
            Self::Region => "region must be non-empty lowercase letters, digits or hyphens",
            Self::AccessKeyId => "access key id must be non-empty printable ASCII without spaces or slashes",
            Self::SecretAccessKey => "secret access key must be non-empty printable ASCII",
        })
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    KeyEmpty,
    KeyTooLong {
        bytes: usize,
        bytes_max: usize,
    },
    KeyDotSegment,
    ObjectTooLarge {
        bytes: u64,
        bytes_max: u64,
    },
    ResponseTooLarge {
        operation: Operation,
        bytes_max: u64,
    },
    ExpiresInvalid {
        seconds: u64,
        seconds_max: u64,
    },
    ChecksumMismatch {
        code: String,
    },
    RangeNotSatisfiable,
    Status {
        operation: Operation,
        status: u16,
        code: Option<String>,
    },
    Malformed {
        operation: Operation,
        problem: &'static str,
    },
    Transport {
        operation: Operation,
        detail: String,
        retryable: bool,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyEmpty => formatter.write_str("object key is empty"),
            Self::KeyTooLong { bytes, bytes_max } => {
                write!(formatter, "object key is {bytes} bytes, over {bytes_max}")
            }
            Self::KeyDotSegment => formatter.write_str("object key has a . or .. path segment"),
            Self::ObjectTooLarge { bytes, bytes_max } => {
                write!(formatter, "object is {bytes} bytes, over {bytes_max}")
            }
            Self::ResponseTooLarge {
                operation,
                bytes_max,
            } => write!(
                formatter,
                "{} response over {bytes_max} bytes",
                operation.as_str()
            ),
            Self::ExpiresInvalid {
                seconds,
                seconds_max,
            } => write!(
                formatter,
                "presigned expiry of {seconds} s is outside 1 to {seconds_max} s"
            ),
            Self::ChecksumMismatch { code } => {
                write!(formatter, "store rejected the checksum: {code}")
            }
            Self::RangeNotSatisfiable => formatter.write_str("byte range is outside the object"),
            Self::Status {
                operation,
                status,
                code,
            } => {
                write!(
                    formatter,
                    "{} failed with HTTP {status}",
                    operation.as_str()
                )?;
                match code {
                    Some(code) => write!(formatter, ": {code}"),
                    None => Ok(()),
                }
            }
            Self::Malformed { operation, problem } => {
                write!(
                    formatter,
                    "{} response is malformed: {problem}",
                    operation.as_str()
                )
            }
            Self::Transport {
                operation, detail, ..
            } => {
                write!(
                    formatter,
                    "{} transport failed: {detail}",
                    operation.as_str()
                )
            }
        }
    }
}

impl std::error::Error for Error {}
