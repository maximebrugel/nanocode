const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
const HEX_DIGITS_UPPER: &[u8; 16] = b"0123456789ABCDEF";
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64_GROUP_BYTES: usize = 3;
const BASE64_GROUP_CHARS: usize = 4;
const SHA256_BASE64_CHARS: usize = 44;
const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_MINUTE: u64 = 60;
const YEAR_MAX: u64 = 9_999;

pub fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    assert_eq!(text.len(), bytes.len() * 2);
    text
}

pub fn base64(bytes: &[u8]) -> String {
    let mut text =
        String::with_capacity(bytes.len().div_ceil(BASE64_GROUP_BYTES) * BASE64_GROUP_CHARS);
    for group in bytes.chunks(BASE64_GROUP_BYTES) {
        let b0 = u32::from(group[0]);
        let b1 = group.get(1).copied().map_or(0, u32::from);
        let b2 = group.get(2).copied().map_or(0, u32::from);
        let word = (b0 << 16) | (b1 << 8) | b2;
        let digit = |shift: u32| char::from(BASE64_ALPHABET[((word >> shift) & 0x3f) as usize]);
        text.push(digit(18));
        text.push(digit(12));
        text.push(if group.len() > 1 { digit(6) } else { '=' });
        text.push(if group.len() > 2 { digit(0) } else { '=' });
    }
    assert_eq!(text.len() % BASE64_GROUP_CHARS, 0);
    text
}

fn base64_value(character: u8) -> Option<u32> {
    BASE64_ALPHABET
        .iter()
        .position(|&candidate| candidate == character)
        .map(|position| position as u32)
}

pub fn base64_decode_sha256(text: &str) -> Option<[u8; 32]> {
    let characters = text.as_bytes();
    if characters.len() != SHA256_BASE64_CHARS || characters[SHA256_BASE64_CHARS - 1] != b'=' {
        return None;
    }
    let mut decoded = [0u8; 32];
    let mut written = 0;
    for group in characters.chunks(BASE64_GROUP_CHARS) {
        let mut word = 0u32;
        let mut digits = 0;
        for &character in group {
            if character == b'=' {
                break;
            }
            word = (word << 6) | base64_value(character)?;
            digits += 1;
        }
        let bytes = digits * 6 / 8;
        word <<= 6 * (BASE64_GROUP_CHARS - digits);
        for index in 0..bytes {
            if written == decoded.len() {
                return None;
            }
            decoded[written] = (word >> (16 - 8 * index)) as u8;
            written += 1;
        }
    }
    if written != decoded.len() || base64(&decoded) != text {
        return None;
    }
    Some(decoded)
}

pub fn uri_encode(out: &mut String, text: &str, keep_slash: bool) {
    for &byte in text.as_bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if unreserved || (keep_slash && byte == b'/') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX_DIGITS_UPPER[usize::from(byte >> 4)]));
            out.push(char::from(HEX_DIGITS_UPPER[usize::from(byte & 0x0f)]));
        }
    }
}

pub struct UtcTime {
    pub amz_date: String,
    pub date: String,
}

pub fn utc(unix_seconds: u64) -> UtcTime {
    let days = unix_seconds / SECONDS_PER_DAY;
    let seconds_of_day = unix_seconds % SECONDS_PER_DAY;
    let (year, month, day) = civil_from_days(days);
    assert!(year <= YEAR_MAX);
    assert!((1..=12).contains(&month));
    assert!((1..=31).contains(&day));
    let hour = seconds_of_day / SECONDS_PER_HOUR;
    let minute = seconds_of_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE;
    let second = seconds_of_day % SECONDS_PER_MINUTE;
    let date = format!("{year:04}{month:02}{day:02}");
    let amz_date = format!("{date}T{hour:02}{minute:02}{second:02}Z");
    assert_eq!(amz_date.len(), 16);
    UtcTime { amz_date, date }
}

const fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}
