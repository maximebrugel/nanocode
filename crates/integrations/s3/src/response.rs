use crate::encode::base64_decode_sha256;
use crate::error::{Error, Operation};
use crate::request::ByteRange;
use crate::sign::SHA256_BYTES;

const STATUS_OK: u16 = 200;
const STATUS_NO_CONTENT: u16 = 204;
const STATUS_PARTIAL_CONTENT: u16 = 206;
const STATUS_NOT_FOUND: u16 = 404;
const STATUS_PRECONDITION_FAILED: u16 = 412;
const STATUS_RANGE_NOT_SATISFIABLE: u16 = 416;
const CHECKSUM_CODES: [&str; 4] = [
    "BadDigest",
    "InvalidDigest",
    "XAmzContentSHA256Mismatch",
    "XAmzContentChecksumMismatch",
];
const CODE_OPEN: &str = "<Code>";
const CODE_CLOSE: &str = "</Code>";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Put {
    Written,
    AlreadyPresent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub bytes: u64,
    pub etag: Option<String>,
    pub checksum_sha256: Option<[u8; SHA256_BYTES]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentRange {
    pub first: u64,
    pub last: u64,
    pub total: Option<u64>,
}

pub fn error_code(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    let start = text.find(CODE_OPEN)? + CODE_OPEN.len();
    let length = text[start..].find(CODE_CLOSE)?;
    let code = &text[start..start + length];
    if code.is_empty() || !code.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return None;
    }
    Some(code.to_owned())
}

pub const fn wants_error_code(status: u16) -> bool {
    !matches!(
        status,
        STATUS_OK
            | STATUS_NO_CONTENT
            | STATUS_PARTIAL_CONTENT
            | STATUS_NOT_FOUND
            | STATUS_PRECONDITION_FAILED
    )
}

fn status_error(operation: Operation, status: u16, code: Option<&str>) -> Error {
    Error::Status {
        operation,
        status,
        code: code.map(str::to_owned),
    }
}

pub fn put_status(status: u16, code: Option<&str>) -> Result<Put, Error> {
    match status {
        STATUS_OK => Ok(Put::Written),
        STATUS_PRECONDITION_FAILED => Ok(Put::AlreadyPresent),
        _ => match code {
            Some(code) if CHECKSUM_CODES.contains(&code) => Err(Error::ChecksumMismatch {
                code: code.to_owned(),
            }),
            _ => Err(status_error(Operation::Put, status, code)),
        },
    }
}

pub fn delete_status(status: u16, code: Option<&str>) -> Result<(), Error> {
    match status {
        STATUS_OK | STATUS_NO_CONTENT | STATUS_NOT_FOUND => Ok(()),
        _ => Err(status_error(Operation::Delete, status, code)),
    }
}

fn parse_content_range(text: &str) -> Option<ContentRange> {
    let (span, total) = text.strip_prefix("bytes ")?.split_once('/')?;
    let (first, last) = span.split_once('-')?;
    let first = first.parse().ok()?;
    let last = last.parse().ok()?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    if last < first || total.is_some_and(|total| last >= total) {
        return None;
    }
    Some(ContentRange { first, last, total })
}

const fn malformed(problem: &'static str) -> Error {
    Error::Malformed {
        operation: Operation::Get,
        problem,
    }
}

pub fn get_expect(
    status: u16,
    code: Option<&str>,
    range: Option<ByteRange>,
    content_range: Option<&str>,
    content_length: Option<&str>,
    bytes_max: u64,
) -> Result<Option<u64>, Error> {
    match (status, range) {
        (STATUS_NOT_FOUND, _) => return Ok(None),
        (STATUS_RANGE_NOT_SATISFIABLE, Some(_)) => return Err(Error::RangeNotSatisfiable),
        (STATUS_OK, None) | (STATUS_PARTIAL_CONTENT, Some(_)) => {}
        (STATUS_OK, Some(_)) => return Err(malformed("store ignored the byte range")),
        _ => return Err(status_error(Operation::Get, status, code)),
    }
    let bytes: u64 = content_length
        .and_then(|length| length.parse().ok())
        .ok_or(malformed("missing or invalid content-length"))?;
    if bytes > bytes_max {
        return Err(Error::ResponseTooLarge {
            operation: Operation::Get,
            bytes_max,
        });
    }
    if let Some(range) = range {
        let served = content_range
            .and_then(parse_content_range)
            .ok_or(malformed("missing or invalid content-range"))?;
        if served.first != range.offset() {
            return Err(malformed(
                "content-range starts away from the requested offset",
            ));
        }
        if served.last > range.last() {
            return Err(malformed("content-range ends past the requested range"));
        }
        if served.last - served.first + 1 != bytes {
            return Err(malformed("content-length differs from content-range"));
        }
    }
    Ok(Some(bytes))
}

pub fn head_response(
    status: u16,
    content_length: Option<&str>,
    etag: Option<&str>,
    checksum_sha256: Option<&str>,
) -> Result<Option<Head>, Error> {
    match status {
        STATUS_NOT_FOUND => return Ok(None),
        STATUS_OK => {}
        _ => return Err(status_error(Operation::Head, status, None)),
    }
    let malformed = |problem| Error::Malformed {
        operation: Operation::Head,
        problem,
    };
    let bytes = content_length
        .and_then(|length| length.parse().ok())
        .ok_or(malformed("missing or invalid content-length"))?;
    let checksum_sha256 = match checksum_sha256 {
        Some(text) => {
            Some(base64_decode_sha256(text).ok_or(malformed("invalid x-amz-checksum-sha256"))?)
        }
        None => None,
    };
    Ok(Some(Head {
        bytes,
        etag: etag.map(str::to_owned),
        checksum_sha256,
    }))
}
