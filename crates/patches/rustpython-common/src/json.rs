//! VM-independent JSON string escaping and scanning over WTF-8.

use memchr::memchr2;
use rustpython_wtf8::{CodePoint, Wtf8, Wtf8Buf};

pub static ESCAPE_CHARS: [&str; 0x20] = [
    "\\u0000", "\\u0001", "\\u0002", "\\u0003", "\\u0004", "\\u0005", "\\u0006", "\\u0007", "\\b",
    "\\t", "\\n", "\\u000b", "\\f", "\\r", "\\u000e", "\\u000f", "\\u0010", "\\u0011", "\\u0012",
    "\\u0013", "\\u0014", "\\u0015", "\\u0016", "\\u0017", "\\u0018", "\\u0019", "\\u001a",
    "\\u001b", "\\u001c", "\\u001d", "\\u001e", "\\u001f",
];

#[must_use]
pub fn encode_string(value: &Wtf8, ascii_only: bool) -> Wtf8Buf {
    let mut out = Wtf8Buf::with_capacity(value.len() + 2);
    write_string(&mut out, value, ascii_only);
    out
}

/// Whether this byte of a WTF-8 string starts something that must be
/// escaped: a control character, a quote or backslash, or, when
/// `ascii_only`, any non-ASCII code point (whose lead byte is >= 0x80).
#[inline]
const fn needs_escape(b: u8, ascii_only: bool) -> bool {
    b < 0x20 || b == b'"' || b == b'\\' || (ascii_only && b >= 0x7f)
}

/// Append `value` to `out` as a quoted JSON string. Runs of bytes that
/// need no escape are copied whole instead of one code point at a time.
pub fn write_string(out: &mut Wtf8Buf, value: &Wtf8, ascii_only: bool) {
    let bytes = value.as_bytes();
    out.reserve(bytes.len() + 2);
    out.push_char('"');
    let mut run = 0;
    let mut i = 0;
    while i < bytes.len() {
        if !needs_escape(bytes[i], ascii_only) {
            i += 1;
            continue;
        }
        // Every byte skipped is ASCII, or (without `ascii_only`) part of a
        // whole non-ASCII code point, so `i` is a code point boundary.
        out.push_wtf8(&value[run..i]);
        let cp = value[i..].code_points().next().expect("i is in bounds");
        i += cp_len(cp);
        run = i;
        push_escaped(out, cp, ascii_only);
    }
    out.push_wtf8(&value[run..]);
    out.push_char('"');
}

/// The WTF-8 length of one code point.
const fn cp_len(cp: CodePoint) -> usize {
    match cp.to_u32() {
        0..0x80 => 1,
        0x80..0x800 => 2,
        0x800..0x10000 => 3,
        _ => 4,
    }
}

fn push_escaped(out: &mut Wtf8Buf, cp: CodePoint, ascii_only: bool) {
    let n = cp.to_u32();
    match n {
        0x08 => out.push_str("\\b"),
        0x09 => out.push_str("\\t"),
        0x0a => out.push_str("\\n"),
        0x0c => out.push_str("\\f"),
        0x0d => out.push_str("\\r"),
        0x00..=0x1f => out.push_str(ESCAPE_CHARS[n as usize]),
        0x22 => out.push_str("\\\""),
        0x5c => out.push_str("\\\\"),
        0x7f if ascii_only => out.push_str("\\u007f"),
        0x80.. if ascii_only => {
            if n <= 0xffff {
                push_hex_escape(out, n as u16);
            } else {
                let m = n - 0x10000;
                push_hex_escape(out, (0xd800 | ((m >> 10) & 0x3ff)) as u16);
                push_hex_escape(out, (0xdc00 | (m & 0x3ff)) as u16);
            }
        }
        _ => out.push(cp),
    }
}

/// The `\uXXXX` spelling of one UTF-16 unit, written without allocating.
fn push_hex_escape(out: &mut Wtf8Buf, unit: u16) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut escape = *b"\\u0000";
    for (digit, shift) in escape[2..].iter_mut().zip([12, 8, 4, 0]) {
        *digit = DIGITS[((unit >> shift) & 0xf) as usize];
    }
    out.push_str(core::str::from_utf8(&escape).expect("the escape is ASCII"));
}

#[derive(Debug)]
pub struct DecodeError {
    pub msg: String,
    pub pos: usize,
}

fn decode_hex<I>(chars: &mut I, pos: usize) -> Result<CodePoint, DecodeError>
where
    I: Iterator<Item = (usize, (usize, CodePoint))>,
{
    let mut value = 0u16;
    for _ in 0..4 {
        let (_, (_, cp)) = chars.next().ok_or_else(|| DecodeError {
            msg: "Invalid \\uXXXX escape".to_owned(),
            pos,
        })?;
        let digit = cp
            .to_char()
            .and_then(|c| c.to_digit(16))
            .ok_or_else(|| DecodeError {
                msg: "Invalid \\uXXXX escape".to_owned(),
                pos,
            })?;
        value = (value << 4) | digit as u16;
    }
    Ok(value.into())
}

/// The byte length of the body of a JSON string that starts right after its
/// opening quote, when the body needs no decoding: it ends at a quote with no
/// backslash before it and (when `strict`) no control character in it.
#[must_use]
pub fn plain_string_len(bytes: &[u8], strict: bool) -> Option<usize> {
    let end = memchr2(b'"', b'\\', bytes)?;
    (bytes[end] == b'"' && !(strict && bytes[..end].iter().any(|b| *b < 0x20))).then_some(end)
}

/// The number of code points in a WTF-8 byte string: every byte except the
/// continuation bytes (0b10xx_xxxx) starts one.
#[must_use]
pub fn count_code_points(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| (b as i8) >= -0x40).count()
}

/// CPython `_json.scanstring`, expressed over WTF-8 so lone surrogates retain
/// their Python `str` identity rather than being lossily converted to UTF-8.
pub fn scan_string(
    value: &Wtf8,
    char_offset: usize,
    strict: bool,
) -> Result<(Wtf8Buf, usize, usize), DecodeError> {
    let unterminated = || DecodeError {
        msg: "Unterminated string starting at".to_owned(),
        pos: char_offset.saturating_sub(1),
    };
    // A string with no escape and no closing-quote surprise is the common one,
    // so find that quote and take everything before it in one copy.
    let bytes = value.as_bytes();
    if let Some(end) = plain_string_len(bytes, strict) {
        let mut out = Wtf8Buf::with_capacity(end);
        out.push_wtf8(&value[..end]);
        return Ok((
            out,
            char_offset + count_code_points(&bytes[..end]) + 1,
            end + 1,
        ));
    }

    let mut out = Wtf8Buf::new();
    let mut chars = value.code_point_indices().enumerate().peekable();
    while let Some((char_i, (byte_i, cp))) = chars.next() {
        match cp.to_char_lossy() {
            '"' => return Ok((out, char_offset + char_i + 1, byte_i + 1)),
            '\\' => {
                let (escape_i, (_, escaped)) = chars.next().ok_or_else(unterminated)?;
                match escaped.to_char_lossy() {
                    '"' => out.push_char('"'),
                    '\\' => out.push_char('\\'),
                    '/' => out.push_char('/'),
                    'b' => out.push_char('\x08'),
                    'f' => out.push_char('\x0c'),
                    'n' => out.push_char('\n'),
                    'r' => out.push_char('\r'),
                    't' => out.push_char('\t'),
                    'u' => {
                        let mut decoded = decode_hex(&mut chars, char_offset + escape_i)?;
                        if let Some(lead) = decoded.to_lead_surrogate() {
                            let mut lookahead = chars.clone();
                            if let (Some((_, (_, slash))), Some((u_i, (_, u)))) =
                                (lookahead.next(), lookahead.next())
                                && slash == '\\'
                                && u == 'u'
                            {
                                let second = decode_hex(&mut lookahead, char_offset + u_i)?;
                                if let Some(trail) = second.to_trail_surrogate() {
                                    decoded = lead.merge(trail).into();
                                    chars = lookahead;
                                }
                            }
                        }
                        out.push(decoded);
                    }
                    _ => {
                        return Err(DecodeError {
                            msg: format!("Invalid \\escape: {escaped:?}"),
                            pos: char_offset + char_i,
                        });
                    }
                }
            }
            '\x00'..='\x1f' if strict => {
                return Err(DecodeError {
                    msg: "Invalid control character at".to_owned(),
                    pos: char_offset + char_i,
                });
            }
            _ => out.push(cp),
        }
    }
    Err(unterminated())
}
