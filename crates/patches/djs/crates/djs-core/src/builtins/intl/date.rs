use super::data::{DATE_PATTERNS, LOCALE_DATES, LocaleDates, PATTERNS, STYLE_PATTERNS, TIME_PATTERNS, ZONE_ALIASES, ZONE_NAMES, ZONES};
use super::{Parts, Spec, range, tzdata, type_err};
use crate::interp::conv::Fault;
use alloc::format;
use alloc::string::{String, ToString};
use locales::{DATE_LOCALES, DTF_PATTERNS_ENDS, DTF_PATTERNS_TEXT, DTF_ROWS, DTF_STRINGS_ENDS, DTF_STRINGS_TEXT, EN_GENERIC_ALT};

mod calendar;
mod calendar_data;
mod locales;
mod range;
mod range_data;

pub(super) use range::format_range;

/// A locale's date and time data, generated from Node's ICU by intl-dtf/emit.js.
/// Name arrays: months and days are [format long, short, narrow, standalone long, short, narrow];
/// eras and periods are [long, short, narrow]; periods hold 24 hours then exact midnight and noon.
/// Pattern tables index DTF_ROWS (rows of 16 pattern indices, u16::MAX: none) by date key
/// and by time key with seven hour-cycle options (none, hour12 true/false, h12, h23, h11, h24).
pub(super) struct DateLocale {
    tags: &'static [&'static str],
    months: [[&'static str; 12]; 6],
    days: [[&'static str; 7]; 6],
    eras: [[&'static str; 2]; 3],
    ampm: [&'static str; 2],
    periods: [[&'static str; 26]; 3],
    date: &'static [u16; 54],
    datez: &'static [u16; 54],
    time: &'static [u16; 189],
    timez: &'static [u16; 189],
    timez_ex: [&'static [(u16, u16)]; 6],
    weekday_ex: &'static [(u16, u16)],
    style_date: [u16; 4],
    style_time: [[u16; 4]; 7],
    style_glue: [&'static str; 4],
    glue: [&'static str; 4],
    zones: &'static [u16],
    gmt: [&'static str; 2],
}

pub(super) const LANGS: [&str; 35] = [
    "en", "de", "fr", "es", "it", "pt", "nl", "ja", "zh", "ko", "ru", "pl", "tr", "ar", "hi", "id", "vi", "hu", "nb", "no", "he", "ta", "sr", "cs", "uk", "sv", "da",
    "ms", "ro", "bn", "el", "sw", "ur", "fil", "ca",
];

fn date_locale(locale: &str) -> Option<usize> {
    let find = |t: &str| DATE_LOCALES.iter().position(|l| l.tags.contains(&t));
    if let Some(i) = find(locale) {
        return Some(i);
    }
    let mut sub = locale.split('-');
    let lang = sub.next().unwrap_or("");
    if lang == "zh" && sub.any(|s| matches!(s, "Hant" | "TW" | "HK" | "MO")) {
        return find("zh-Hant");
    }
    if lang == "sr" && locale.split('-').any(|s| s == "Latn") {
        return find("sr-Latn");
    }
    find(lang)
}

const ARAB_DIGIT_REGIONS: [&str; 21] = ["BH", "DJ", "EG", "ER", "IL", "IQ", "JO", "KM", "KW", "LB", "MR", "OM", "PS", "QA", "SA", "SD", "SO", "SS", "SY", "TD", "YE"];

/// The decimal numbering systems ICU formats dates in (Intl.supportedValuesOf('numberingSystem')) with
/// the code point of their zero; every one but hanidec runs on contiguously from it.
const NU_ZEROS: [(&str, u32); 77] = [("adlm", 0x1e950), ("ahom", 0x11730), ("arab", 0x660), ("arabext", 0x6f0), ("bali", 0x1b50), ("beng", 0x9e6), ("bhks", 0x11c50), ("brah", 0x11066), ("cakm", 0x11136), ("cham", 0xaa50), ("deva", 0x966), ("diak", 0x11950), ("fullwide", 0xff10), ("gara", 0x10d40), ("gong", 0x11da0), ("gonm", 0x11d50), ("gujr", 0xae6), ("gukh", 0x16130), ("guru", 0xa66), ("hmng", 0x16b50), ("hmnp", 0x1e140), ("java", 0xa9d0), ("kali", 0xa900), ("kawi", 0x11f50), ("khmr", 0x17e0), ("knda", 0xce6), ("krai", 0x16d70), ("lana", 0x1a80), ("lanatham", 0x1a90), ("laoo", 0xed0), ("latn", 0x30), ("lepc", 0x1c40), ("limb", 0x1946), ("mathbold", 0x1d7ce), ("mathdbl", 0x1d7d8), ("mathmono", 0x1d7f6), ("mathsanb", 0x1d7ec), ("mathsans", 0x1d7e2), ("mlym", 0xd66), ("modi", 0x11650), ("mong", 0x1810), ("mroo", 0x16a60), ("mtei", 0xabf0), ("mymr", 0x1040), ("mymrepka", 0x116da), ("mymrpao", 0x116d0), ("mymrshan", 0x1090), ("mymrtlng", 0xa9f0), ("nagm", 0x1e4f0), ("newa", 0x11450), ("nkoo", 0x7c0), ("olck", 0x1c50), ("onao", 0x1e5f1), ("orya", 0xb66), ("osma", 0x104a0), ("outlined", 0x1ccf0), ("rohg", 0x10d30), ("saur", 0xa8d0), ("segment", 0x1fbf0), ("shrd", 0x111d0), ("sind", 0x112f0), ("sinh", 0xde6), ("sora", 0x110f0), ("sund", 0x1bb0), ("sunu", 0x11bf0), ("takr", 0x116c0), ("talu", 0x19d0), ("tamldec", 0xbe6), ("telu", 0xc66), ("thai", 0xe50), ("tibt", 0xf20), ("tirh", 0x114d0), ("tnsa", 0x16ac0), ("tols", 0x11de0), ("vaii", 0xa620), ("wara", 0x118e0), ("wcho", 0x1e2f0),];
const HANIDEC: [char; 10] = ['\u{3007}', '\u{4e00}', '\u{4e8c}', '\u{4e09}', '\u{56db}', '\u{4e94}', '\u{516d}', '\u{4e03}', '\u{516b}', '\u{4e5d}'];

fn nu_supported(nu: &str) -> bool {
    nu == "hanidec" || NU_ZEROS.iter().any(|z| z.0 == nu)
}

/// ASCII digit `d` written in numbering system `nu`.
fn nu_digit(nu: &str, d: u32) -> char {
    if nu == "hanidec" {
        return HANIDEC[d as usize];
    }
    NU_ZEROS.iter().find(|z| z.0 == nu).and_then(|z| char::from_u32(z.1 + d)).unwrap_or(char::from(b'0' + d as u8))
}

pub(super) fn numbering_system(locale: &str) -> &'static str {
    let mut sub = locale.split('-');
    let lang = sub.next();
    if lang == Some("ar") && sub.any(|s| ARAB_DIGIT_REGIONS.contains(&s)) {
        "arab"
    } else if lang == Some("bn") {
        "beng"
    } else {
        "latn"
    }
}

fn dtf_str(text: &'static str, ends: &[u32], i: u16) -> &'static str {
    let i = usize::from(i);
    let start = if i == 0 { 0 } else { ends[i - 1] as usize };
    &text[start..ends[i] as usize]
}

fn cell(rows: &[u16], k: usize) -> u16 {
    DTF_ROWS[usize::from(rows[k >> 4])][k & 15]
}

fn join(glue: &str, d: &str, t: &str) -> String {
    if glue.contains('{') {
        glue.replacen("{1}", d, 1).replacen("{0}", t, 1)
    } else {
        format!("{d}{glue}{t}")
    }
}

fn dtf_pattern(i: u16) -> Option<&'static str> {
    (i != u16::MAX).then(|| dtf_str(DTF_PATTERNS_TEXT, DTF_PATTERNS_ENDS, i))
}

/// Replaces each unquoted run of `z` with the letters for the requested zone name.
fn sub_zone(p: &str, letters: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut prev = '\0';
    for c in p.chars() {
        if c == '\'' {
            quoted = !quoted;
        }
        if !quoted && c == 'z' {
            if prev != 'z' {
                out.push_str(letters);
            }
        } else {
            out.push(c);
        }
        prev = if quoted { '\0' } else { c };
    }
    out
}

fn locale_pattern(l: &DateLocale, f: &[usize; 10], fsd: usize, cy: usize, styles: (Option<&str>, Option<&str>), tzn: Option<&str>) -> String {
    let (ds, ts) = styles;
    if ds.is_some() || ts.is_some() {
        let di = ds.map(|s| pos(&STYLES, Some(s)));
        let dp = di.and_then(|i| dtf_pattern(l.style_date[i]));
        let tp = ts.and_then(|s| dtf_pattern(l.style_time[cy][pos(&STYLES, Some(s))]));
        return match (dp, tp) {
            (Some(d), Some(t)) => join(l.style_glue[di.unwrap_or(3)], d, t),
            (Some(d), None) => d.to_string(),
            (None, Some(t)) => t.to_string(),
            (None, None) => String::new(),
        };
    }
    let dk = (((f[0] * 4 + f[1]) * 3 + f[2]) * 6 + f[3]) * 3 + f[4];
    let tk = ((((f[6] * 3 + f[7]) * 3 + f[8]) * 7 + cy) * 4 + f[5]) * 4 + fsd;
    let has_t = f[5] != 0 || f[6] != 0 || f[7] != 0 || f[8] != 0 || fsd != 0;
    let kind = tzn.map(|z| pos(&TZ_NAMES, Some(z)));
    let find = |t: &[(u16, u16)], k: usize| t.iter().find(|e| usize::from(e.0) == k).map(|e| dtf_str(DTF_PATTERNS_TEXT, DTF_PATTERNS_ENDS, e.1));
    let p: String = if !has_t {
        dtf_pattern(cell(if kind.is_some() { l.datez } else { l.date }, dk)).unwrap_or("").to_string()
    } else {
        let t = match kind {
            Some(k) => find(l.timez_ex[k], tk).or_else(|| dtf_pattern(cell(l.timez, tk))),
            None => dtf_pattern(cell(l.time, tk)),
        }
        .unwrap_or("");
        let weekday_only = f[0] != 0 && f[1] == 0 && f[2] == 0 && f[3] == 0 && f[4] == 0;
        let only_era = f[1] != 0 && f[0] == 0 && f[2] == 0 && f[3] == 0 && f[4] == 0;
        if dk == 0 {
            t.to_string()
        } else if let Some(w) = (kind.is_none() && weekday_only).then(|| find(l.weekday_ex, f[0] * 3024 + tk)).flatten() {
            w.to_string()
        } else {
            let d = if only_era { ["", "GGGG", "G", "GGGGG"][f[1]] } else { dtf_pattern(cell(l.date, dk)).unwrap_or("") };
            let glue = match f[3] {
                3 if f[0] != 0 => l.glue[0],
                3 => l.glue[1],
                4 => l.glue[2],
                _ => l.glue[3],
            };
            join(glue, d, t)
        }
    };
    match kind {
        Some(k) => sub_zone(&p, TZ_LETTERS[k]),
        None => p,
    }
}

const WIDTHS: [&str; 4] = ["", "long", "short", "narrow"];
const NUMS: [&str; 3] = ["", "numeric", "2-digit"];
const MONTHS: [&str; 6] = ["", "numeric", "2-digit", "long", "short", "narrow"];
const CYCLES: [&str; 4] = ["h12", "h23", "h11", "h24"];
const STYLES: [&str; 4] = ["full", "long", "medium", "short"];
const TZ_NAMES: [&str; 6] = ["short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"];
const TZ_LETTERS: [&str; 6] = ["z", "zzzz", "O", "OOOO", "v", "vvvv"];
const DAY_NAMES: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTH_NAMES: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];

fn pos(values: &[&str], v: Option<&str>) -> usize {
    values.iter().position(|x| Some(*x) == v).unwrap_or(0)
}

/// tzdata's POSIX-style US zones, which ICU keeps as their own IDs and names only by offset (GMT-5).
const US_POSIX_ZONES: [(&str, &str); 4] =
    [("CST6CDT", "CST6CDT,M3.2.0,M11.1.0"), ("EST5EDT", "EST5EDT,M3.2.0,M11.1.0"), ("MST7MDT", "MST7MDT,M3.2.0,M11.1.0"), ("PST8PDT", "PST8PDT,M3.2.0,M11.1.0")];

/// The hours east of UTC of an Etc/GMT±N zone (case-insensitive; POSIX signs, so Etc/GMT+5 is UTC-5),
/// for N in 1..=12 west and 1..=14 east, as ICU has them. Etc/GMT±0 is an alias of UTC instead.
fn etc_gmt_hours(name: &str) -> Option<i64> {
    let rest = name.get(..7).filter(|p| p.eq_ignore_ascii_case("Etc/GMT")).map(|_| &name[7..])?;
    let (sign, digits) = (rest.as_bytes().first()?, &rest[1..]);
    if digits.is_empty() || digits.len() > 2 || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    match sign {
        b'+' if n <= 12 => Some(-n),
        b'-' if n <= 14 => Some(n),
        _ => None,
    }
}

/// Names ICU resolves to UTC but formats with the Greenwich Mean Time names of Africa/Abidjan.
fn greenwich_alias(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    matches!(u.as_str(), "ETC/GMT0" | "ETC/GMT+0" | "ETC/GMT-0" | "ETC/GREENWICH" | "GREENWICH")
}

/// The zone's canonical name, from a case-insensitive IANA name, alias or offset.
fn canonical_zone(name: &str) -> Option<String> {
    let b = name.as_bytes();
    if matches!(b.first(), Some(b'+' | b'-')) {
        let digits: String = name[1..].chars().filter(|c| *c != ':').collect();
        let ok = name[1..].bytes().all(|c| c.is_ascii_digit() || c == b':');
        let (h, m) = match digits.len() {
            2 => (&digits[..2], "00"),
            4 => (&digits[..2], &digits[2..]),
            _ => return None,
        };
        let colon_ok = !name.contains(':') || (name.len() == 6 && b[3] == b':');
        if !ok || !colon_ok || h > "23" || m > "59" {
            return None;
        }
        return Some(format!("{}{h}:{m}", &name[..1]));
    }
    // An alias first: ICU resolves GMT, Etc/GMT and Etc/UTC to UTC though ZONES lists them for their names.
    if let Some(a) = ZONE_ALIASES.iter().find(|a| a.0.eq_ignore_ascii_case(name)) {
        return Some(a.1.to_string());
    }
    if let Some(z) = ZONES.iter().find(|z| z.0.eq_ignore_ascii_case(name)) {
        return Some(z.0.to_string());
    }
    if let Some(z) = US_POSIX_ZONES.iter().find(|z| z.0.eq_ignore_ascii_case(name)) {
        return Some(z.0.to_string());
    }
    if let Some(h) = etc_gmt_hours(name) {
        return Some(format!("Etc/GMT{}{}", if h < 0 { '+' } else { '-' }, h.abs()));
    }
    None
        .or_else(|| {
            let upper = name.to_ascii_uppercase();
            let etc = upper.strip_prefix("ETC/").unwrap_or(&upper);
            matches!(etc, "UTC" | "GMT" | "UCT" | "ZULU" | "UNIVERSAL" | "GREENWICH" | "GMT0" | "GMT+0" | "GMT-0")
                .then(|| "UTC".to_string())
        })
}

/// ResolveLocale for Intl.DateTimeFormat: `tags` are the requested locales, `locale` the one chosen from them.
pub(super) fn resolve(locale: &str, tags: &[String], raw: &Spec) -> Result<Spec, Fault> {
    let n = "Intl.DateTimeFormat";
    // numberingSystem: a supported option wins, else a supported -u-nu- keyword (which the locale keeps
    // unless an option overrode it), else the locale's own.
    let lang = locale.split('-').next();
    let keywords = tags.iter().find(|t| t.split('-').next() == lang).map(|t| super::collate::unicode_keywords(t)).unwrap_or_default();
    // calendar: a supported option wins, else a supported -u-ca- keyword, which the locale keeps unless
    // an option overrode it.
    let ca_opt = raw.get("calendar");
    if let Some(v) = ca_opt
        && !calendar::well_formed(v)
    {
        return range(&format!("Invalid calendar : {v}"));
    }
    let ca_key = keywords.iter().find(|(k, _)| *k == "ca").and_then(|(_, v)| calendar::canonical(v));
    let (mut cal, mut ca_kept) = match (ca_opt.and_then(calendar::canonical), ca_key) {
        (Some(o), Some(k)) if o != k => (o, false),
        (Some(o), k) => (o, k.is_some()),
        (None, Some(k)) => (k, true),
        (None, None) => (0, false),
    };
    let nu_key = keywords.iter().find(|(k, _)| *k == "nu").map(|(_, v)| v.as_str()).filter(|v| nu_supported(v));
    let nu_opt = raw.get("numberingSystem");
    if let Some(v) = nu_opt
        && (v.is_empty() || !v.split('-').all(|p| (3..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric())))
    {
        return range(&format!("Invalid numberingSystem : {v}"));
    }
    let (nu, nu_kept): (String, Option<String>) = match (nu_opt.filter(|v| nu_supported(v)), nu_key) {
        (Some(o), Some(k)) if o != k => (o.to_string(), None),
        (Some(o), _) => (o.to_string(), nu_key.map(|k| format!("-u-nu-{k}"))),
        (None, Some(k)) => (k.to_string(), Some(format!("-u-nu-{k}"))),
        (None, None) => (numbering_system(locale).to_string(), None),
    };
    let hour12 = raw.get("hour12").map(|v| v == "true");
    let hc_opt = raw.choose_opt("hourCycle", &CYCLES, n)?;
    let zone = match raw.get("timeZone") {
        Some(z) => match canonical_zone(z) {
            Some(c) => c,
            None => return range(&format!("Invalid time zone specified: {z}")),
        },
        None => {
            let off = super::parse_f64(raw.get("@offset").unwrap_or("0")) as i64;
            if off == 0 { "UTC".to_string() } else { offset_name(off, true, true) }
        }
    };
    let mut fields = [0usize; 10];
    fields[0] = pos(&WIDTHS, raw.choose_opt("weekday", &WIDTHS[1..], n)?);
    fields[1] = pos(&WIDTHS, raw.choose_opt("era", &WIDTHS[1..], n)?);
    fields[2] = pos(&NUMS, raw.choose_opt("year", &NUMS[1..], n)?);
    fields[3] = pos(&MONTHS, raw.choose_opt("month", &MONTHS[1..], n)?);
    fields[4] = pos(&NUMS, raw.choose_opt("day", &NUMS[1..], n)?);
    fields[5] = pos(&WIDTHS, raw.choose_opt("dayPeriod", &["narrow", "short", "long"], n)?.map(|p| match p {
        "narrow" => "narrow",
        "short" => "short",
        _ => "long",
    }));
    fields[6] = pos(&NUMS, raw.choose_opt("hour", &NUMS[1..], n)?);
    fields[7] = pos(&NUMS, raw.choose_opt("minute", &NUMS[1..], n)?);
    fields[8] = pos(&NUMS, raw.choose_opt("second", &NUMS[1..], n)?);
    let fsd = match raw.get("fractionalSecondDigits") {
        None => 0,
        Some(v) => {
            let x = super::parse_f64(v);
            if !(1.0..=3.0).contains(&x) {
                return range("fractionalSecondDigits value is out of range.");
            }
            x as usize
        }
    };
    let tzn = raw.choose_opt("timeZoneName", &TZ_NAMES, n)?;
    raw.choose("formatMatcher", &["basic", "best fit"], "best fit", n)?;
    let ds = raw.choose_opt("dateStyle", &STYLES, n)?;
    let ts = raw.choose_opt("timeStyle", &STYLES, n)?;
    let explicit = fields.iter().any(|f| *f != 0) || fsd != 0 || tzn.is_some();
    if (ds.is_some() || ts.is_some()) && explicit {
        return type_err("Invalid option : option");
    }
    let need = raw.get("@required").unwrap_or("any");
    // ToDateTimeOptions: toLocaleDateString takes no timeStyle, toLocaleTimeString no dateStyle.
    if need == "date" && ts.is_some() {
        return type_err("Invalid option : timeStyle");
    }
    if need == "time" && ds.is_some() {
        return type_err("Invalid option : dateStyle");
    }
    let defaults = raw.get("@defaults").unwrap_or("date");
    if ds.is_none() && ts.is_none() {
        let has_date = fields[0] != 0 || fields[2] != 0 || fields[3] != 0 || fields[4] != 0;
        let has_time = fields[5] != 0 || fields[6] != 0 || fields[7] != 0 || fields[8] != 0 || fsd != 0;
        let need_defaults = match need {
            "date" => !has_date,
            "time" => !has_time,
            _ => !has_date && !has_time,
        };
        if need_defaults && defaults != "time" {
            fields[2] = 1;
            fields[3] = 1;
            fields[4] = 1;
        }
        if need_defaults && defaults != "date" {
            fields[6] = 1;
            fields[7] = 1;
            fields[8] = 1;
        }
    } else if need == "date" && ds.is_none() {
        return type_err("Invalid option : dateStyle");
    } else if need == "time" && ts.is_none() {
        return type_err("Invalid option : timeStyle");
    }
    let cycle = match (hour12, hc_opt) {
        (Some(true), _) => 0,
        (Some(false), _) => 1,
        (None, Some(c)) => pos(&CYCLES, Some(c)),
        (None, None) => 0,
    };
    let local = LOCALE_DATES
        .iter()
        .find(|l| l.0 == locale)
        .or_else(|| LOCALE_DATES.iter().find(|l| locale.split('-').next() == Some(l.0)));
    let dl = date_locale(locale);
    let cy = match (hour12, hc_opt) {
        (Some(true), _) => 1,
        (Some(false), _) => 2,
        (None, Some(c)) => 3 + pos(&CYCLES, Some(c)),
        (None, None) => 0,
    };
    let build = |fields: &[usize; 10], ds: Option<&str>, ts: Option<&str>| match (dl, ds, ts) {
        (Some(i), _, _) => locale_pattern(&DATE_LOCALES[i], fields, fsd, cy, (ds, ts), tzn),
        (None, None, None) => match local.and_then(|l| local_pattern(l, fields, fsd, hour12.is_some() || hc_opt.is_some() || tzn.is_some())) {
            Some(p) => p,
            None => compose(fields, fsd, cycle, tzn),
        },
        (None, d, t) => {
            let dp = d.map(|s| PATTERNS[usize::from(STYLE_PATTERNS[pos(&STYLES, Some(s))])]);
            let tp = t.map(|s| PATTERNS[usize::from(STYLE_PATTERNS[4 + cycle * 4 + pos(&STYLES, Some(s))])]);
            match (dp, tp) {
                (Some(d), Some(t)) => {
                    let glue = if matches!(ds, Some("full" | "long")) { "' at '" } else { ", " };
                    format!("{d}{glue}{t}")
                }
                (Some(d), None) => d.to_string(),
                (None, Some(t)) => t.to_string(),
                (None, None) => String::new(),
            }
        }
    };
    let cal_locale = (cal != 0).then(|| calendar::locale_for(cal, dl.map_or("en", |i| DATE_LOCALES[i].tags[0]))).flatten();
    if cal != 0 && cal_locale.is_none() {
        // no names or patterns for this calendar in this locale: format gregorian, as resolvedOptions says
        (cal, ca_kept) = (0, false);
    }
    let pattern = match cal_locale {
        Some(ci) => {
            let has_t = fields[5..9].iter().any(|f| *f != 0) || fsd != 0 || ts.is_some();
            let mut tf = fields;
            tf[..5].fill(0);
            let own = calendar::time(calendar::get(ci), &fields, fsd, cy, ts.map(|t| pos(&STYLES, Some(t))), tzn.is_some());
            let time = match (has_t, own, tzn) {
                (false, _, _) => String::new(),
                (true, Some(t), Some(z)) => sub_zone(t, TZ_LETTERS[pos(&TZ_NAMES, Some(z))]),
                (true, Some(t), None) => t.to_string(),
                (true, None, _) => build(&tf, None, ts),
            };
            let p = calendar::compose(calendar::get(ci), &fields, ds.map(|d| pos(&STYLES, Some(d))), &time, tzn.is_some());
            match tzn {
                Some(z) if !has_t => sub_zone(&p, TZ_LETTERS[pos(&TZ_NAMES, Some(z))]),
                _ => p,
            }
        }
        None => build(&fields, ds, ts),
    };
    let mut out = Spec::default();
    let mut kept: alloc::vec::Vec<String> = alloc::vec::Vec::new();
    if ca_kept {
        kept.push(format!("ca-{}", calendar::CALENDARS[cal]));
    }
    if let Some(k) = nu_kept {
        kept.push(k.trim_start_matches("-u-").to_string());
    }
    out.put("locale", &if kept.is_empty() { locale.to_string() } else { format!("{locale}-u-{}", kept.join("-")) });
    out.put("calendar", calendar::CALENDARS[cal]);
    if let Some(ci) = cal_locale {
        out.put("@cal", &ci.to_string());
    }
    out.put("numberingSystem", &nu);
    out.put("timeZone", &zone);
    if raw.get("timeZone").is_some_and(greenwich_alias) {
        out.put("@zone", "Africa/Abidjan");
        out.put("@gmt", "1");
    }
    let has = |c: char| field_width(&pattern, c) != 0;
    if has('j') {
        let hc = if has('h') { "h12" } else if has('K') { "h11" } else if has('k') { "h24" } else { "h23" };
        out.put("hourCycle", hc);
        out.put("hour12", if hc == "h12" || hc == "h11" { "!t" } else { "!f" });
    }
    if ds.is_none() && ts.is_none() {
        for (name, letter) in [("weekday", 'E'), ("era", 'G'), ("year", 'y'), ("month", 'M'), ("day", 'd'), ("dayPeriod", 'B'), ("hour", 'j'), ("minute", 'm'), ("second", 's'), ("fractionalSecondDigits", 'S'), ("timeZoneName", 'z')] {
            let count = field_width(&pattern, letter);
            if count == 0 {
                continue;
            }
            let v: &str = match letter {
                'E' | 'G' => match count { 4 => "long", 5 => "narrow", _ => "short" },
                'B' => match fields[5] { 3 => "narrow", 2 => "short", _ => "long" },
                'M' => match count { 1 => "numeric", 2 => "2-digit", 3 => "short", 4 => "long", _ => "narrow" },
                'S' => {
                    out.num(name, count as u32);
                    continue;
                }
                'z' => tzn.unwrap_or("short"),
                _ => if count == 2 { "2-digit" } else { "numeric" },
            };
            out.put(name, v);
        }
    } else {
        if let Some(d) = ds {
            out.put("dateStyle", d);
        }
        if let Some(t) = ts {
            out.put("timeStyle", t);
        }
    }
    if let Some(l) = local {
        out.put("@ampm", &format!("{},{}", l.5[0], l.5[1]));
    }
    if let Some(i) = dl {
        out.put("@dl", &i.to_string());
    }
    out.put("@pattern", &pattern);
    let key = range::Key {
        f: [fields[0], fields[1], fields[2], fields[3], fields[4], fields[5], fields[6], fields[7], fields[8]],
        fsd,
        cy,
        z: tzn.map_or(0, |z| 1 + pos(&TZ_NAMES, Some(z))),
        ds: ds.map(|d| pos(&STYLES, Some(d))),
        ts: ts.map(|t| pos(&STYLES, Some(t))),
    };
    out.put("@rk", &key.encode());
    Ok(out)
}

/// A locale's own pattern for numeric dates and times (the few locales with data), else None.
fn local_pattern(l: &LocaleDates, f: &[usize; 10], fsd: usize, custom: bool) -> Option<String> {
    if custom || fsd != 0 || f[0] != 0 || f[1] != 0 || f[5] != 0 {
        return None;
    }
    let date = match (f[2], f[3], f[4]) {
        (0, 0, 0) => None,
        (1, 1, 1) => Some(l.1),
        _ => return None,
    };
    let time = match (f[6], f[7], f[8]) {
        (0, 0, 0) => None,
        (1, 1, 1) => Some(l.2),
        (1, 1, 0) => Some(l.4),
        _ => return None,
    };
    match (date, time) {
        (Some(d), Some(t)) => Some(format!("{d}{}{t}", l.3)),
        (Some(d), None) => Some(d.to_string()),
        (None, Some(t)) => Some(t.to_string()),
        (None, None) => None,
    }
}

/// The longest run of a field's letter outside quotes (`j`: any hour letter).
fn field_width(p: &str, letter: char) -> usize {
    let mut best = 0;
    let mut quoted = false;
    let mut run = 0;
    let mut prev = '\0';
    for c in p.chars() {
        if c == '\'' {
            quoted = !quoted;
            run = 0;
            prev = c;
            continue;
        }
        let hit = !quoted
            && (c == letter
                || (letter == 'j' && matches!(c, 'h' | 'H' | 'k' | 'K'))
                || (letter == 'E' && c == 'c')
                || (letter == 'M' && c == 'L')
                || (letter == 'z' && matches!(c, 'z' | 'O' | 'v')));
        run = if hit && (run == 0 || c == prev) { run + 1 } else if hit { 1 } else { 0 };
        best = best.max(run);
        prev = c;
    }
    best
}

fn insert_zone(t: &str, z: &str) -> String {
    match t.find("' (").or_else(|| t.find("' \u{251c}")) {
        Some(i) => format!("{} {z}{}", &t[..i], &t[i..]),
        None => format!("{t} {z}"),
    }
}

fn compose(f: &[usize; 10], fsd: usize, cycle: usize, tzn: Option<&str>) -> String {
    let dk = (((f[0] * 4 + f[1]) * 3 + f[2]) * 6 + f[3]) * 3 + f[4];
    let tk = ((((f[6] * 3 + f[7]) * 3 + f[8]) * 4 + cycle) * 4 + f[5]) * 4 + fsd;
    let lookup = |i: u16| (i != u16::MAX).then(|| PATTERNS[usize::from(i)]);
    let mut d = lookup(DATE_PATTERNS[dk]).map(String::from);
    let mut t = lookup(TIME_PATTERNS[tk]).map(String::from);
    let only_era = f[1] != 0 && f[0] == 0 && f[2] == 0 && f[3] == 0 && f[4] == 0;
    if let Some(time) = &mut t {
        if d.is_some()
            && f[6] == 0
            && let Some(i) = time.find("' \u{251c}AM/PM: '")
        {
            let end = time[i..].find('\u{2524}').map_or(time.len(), |j| i + j + '\u{2524}'.len_utf8());
            time.replace_range(i..end, "");
        }
        if tzn.is_some() && let Some(i) = time.find(" B") {
            time.replace_range(i..i + 1, "\u{202f}");
        }
    }
    if let Some(z) = tzn {
        let letters = TZ_LETTERS[pos(&TZ_NAMES, Some(z))];
        t = Some(match &t {
            Some(time) => insert_zone(time, letters),
            None => letters.to_string(),
        });
    }
    if only_era && t.is_some() {
        d = Some(["", "GGGG", "G", "GGGGG"][f[1]].to_string());
    }
    match (d, t) {
        (Some(d), Some(t)) => {
            let weekday_only = d.bytes().all(|b| b == b'E');
            let h12 = cycle == 0 || cycle == 2;
            if weekday_only && f[6] != 0 && fsd == 0 && tzn.is_none() && (h12 || f[7] != 0) {
                format!("{d} {t}")
            } else if f[3] == 3 {
                format!("{d}' at '{t}")
            } else {
                format!("{d}, {t}")
            }
        }
        (Some(d), None) => d,
        (None, Some(t)) => t,
        (None, None) => String::new(),
    }
}

// ---- time zones ----

type ZoneEntry = (&'static str, &'static str, [u16; 6]);

struct Rule {
    std: i64,
    dst: Option<(i64, Trans, Trans)>,
}

#[derive(Clone, Copy)]
enum Day {
    Month(u32, u32, u32),
    Julian(u32),
    Zero(u32),
}

#[derive(Clone, Copy)]
struct Trans {
    day: Day,
    secs: i64,
}

fn parse_offset(s: &[u8], i: &mut usize) -> Option<i64> {
    let mut sign = 1;
    if let Some(c) = s.get(*i)
        && (*c == b'+' || *c == b'-')
    {
        if *c == b'-' {
            sign = -1;
        }
        *i += 1;
    }
    let mut parts = [0i64; 3];
    for (k, part) in parts.iter_mut().enumerate() {
        if k > 0 {
            if s.get(*i) != Some(&b':') {
                break;
            }
            *i += 1;
        }
        let start = *i;
        while s.get(*i).is_some_and(u8::is_ascii_digit) {
            *part = *part * 10 + i64::from(s[*i] - b'0');
            *i += 1;
        }
        if *i == start {
            return None;
        }
    }
    Some(sign * (parts[0] * 3600 + parts[1] * 60 + parts[2]))
}

fn skip_name(s: &[u8], i: &mut usize) {
    if s.get(*i) == Some(&b'<') {
        while *i < s.len() && s[*i] != b'>' {
            *i += 1;
        }
        *i += 1;
    } else {
        while s.get(*i).is_some_and(u8::is_ascii_alphabetic) {
            *i += 1;
        }
    }
}

fn parse_trans(s: &[u8], i: &mut usize) -> Option<Trans> {
    let num = |i: &mut usize| {
        let mut v = 0u32;
        while s.get(*i).is_some_and(u8::is_ascii_digit) {
            v = v * 10 + u32::from(s[*i] - b'0');
            *i += 1;
        }
        v
    };
    let day = match s.get(*i) {
        Some(b'M') => {
            *i += 1;
            let m = num(i);
            *i += 1;
            let w = num(i);
            *i += 1;
            Day::Month(m, w, num(i))
        }
        Some(b'J') => {
            *i += 1;
            Day::Julian(num(i))
        }
        _ => Day::Zero(num(i)),
    };
    let secs = if s.get(*i) == Some(&b'/') {
        *i += 1;
        parse_offset(s, i)?
    } else {
        7200
    };
    Some(Trans { day, secs })
}

fn rule(posix: &str) -> Rule {
    let s = posix.as_bytes();
    let mut i = 0;
    skip_name(s, &mut i);
    let std = -parse_offset(s, &mut i).unwrap_or(0);
    if i >= s.len() {
        return Rule { std, dst: None };
    }
    skip_name(s, &mut i);
    let dst = if s.get(i) == Some(&b',') { std + 3600 } else { -parse_offset(s, &mut i).unwrap_or(-(std + 3600)) };
    i += 1;
    let (Some(a), true) = (parse_trans(s, &mut i), s.get(i) == Some(&b',')) else {
        return Rule { std, dst: None };
    };
    i += 1;
    match parse_trans(s, &mut i) {
        Some(b) => Rule { std, dst: Some((dst, a, b)) },
        None => Rule { std, dst: None },
    }
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Seconds since the epoch, in local wall time, of the transition in year y.
fn trans_local(t: Trans, y: i64) -> i64 {
    let day = match t.day {
        Day::Month(m, w, wd) => {
            let first = days_from_civil(y, m, 1);
            let first_wd = (first + 4).rem_euclid(7) as u32;
            let mut d = (wd + 7 - first_wd) % 7 + 1 + (w - 1) * 7;
            let len = [31, if leap(y) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(m - 1) as usize];
            while d > len {
                d -= 7;
            }
            first + i64::from(d) - 1
        }
        Day::Julian(n) => {
            let n = i64::from(n);
            days_from_civil(y, 1, 1) + n - 1 + i64::from(leap(y) && n >= 60)
        }
        Day::Zero(n) => days_from_civil(y, 1, 1) + i64::from(n),
    };
    day * 86_400 + t.secs
}

struct ZoneAt {
    off: i64,
    dst: bool,
    entry: Option<&'static ZoneEntry>,
    tuple: u16,
    overlap: bool,
}

fn rule_offset(r: &Rule, t: i64) -> (i64, bool) {
    let Some((dst, a, b)) = r.dst else { return (r.std, false) };
    let (y, _, _) = civil((t + r.std).div_euclid(86_400));
    let start = trans_local(a, y) - r.std;
    let end = trans_local(b, y) - dst;
    let in_dst = if start < end { t >= start && t < end } else { !(t >= end && t < start) };
    (if in_dst { dst } else { r.std }, in_dst)
}

fn zone_at(zone: &str, t: i64, gmt: bool) -> ZoneAt {
    let fixed = |off| ZoneAt { off, dst: false, entry: None, tuple: 0, overlap: false };
    if let Some(rest) = zone.strip_prefix(['+', '-']) {
        let h: i64 = rest[..2].parse().unwrap_or(0);
        let m: i64 = rest[3..].parse().unwrap_or(0);
        return fixed((h * 3600 + m * 60) * if zone.starts_with('-') { -1 } else { 1 });
    }
    if let Some(h) = etc_gmt_hours(zone) {
        return fixed(h * 3600);
    }
    let (i, posix, entry) = match ZONES.iter().position(|z| z.0 == zone) {
        Some(i) => (i, ZONES[i].1, Some(&ZONES[i])),
        None => match US_POSIX_ZONES.iter().position(|z| z.0 == zone) {
            Some(k) => (ZONES.len() + k, US_POSIX_ZONES[k].1, None),
            None => return fixed(0),
        },
    };
    let r = rule(posix);
    match tzdata::at(if gmt { ZONES.len() + US_POSIX_ZONES.len() } else { i }, t) {
        tzdata::At::Period(k, next) => {
            let overlap = next.is_some_and(|(nt, nk)| {
                let noff = nk.map_or_else(|| rule_offset(&r, nt).0, |nk| nk.off);
                noff < k.off && nt - t <= k.off - noff
            });
            ZoneAt { off: k.off, dst: k.dst, entry, tuple: k.tuple, overlap }
        }
        tzdata::At::Rule => {
            let (off, dst) = rule_offset(&r, t);
            let overlap = r.dst.is_some_and(|(dst, _, b)| {
                let (y, _, _) = civil((t + dst).div_euclid(86_400));
                let end = trans_local(b, y) - dst;
                t < end && end - t <= dst - r.std
            });
            ZoneAt { off, dst, entry, tuple: 0, overlap }
        }
    }
}

fn offset_name(minutes: i64, long: bool, bare: bool) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let (h, m) = (minutes.abs() / 60, minutes.abs() % 60);
    if bare {
        return format!("{sign}{h:02}:{m:02}");
    }
    if minutes == 0 {
        return "GMT".to_string();
    }
    if long {
        format!("GMT{sign}{h:02}:{m:02}")
    } else if m == 0 {
        format!("GMT{sign}{h}")
    } else {
        format!("GMT{sign}{h}:{m:02}")
    }
}

fn zone_name(kind: usize, z: &ZoneAt) -> String {
    if kind == 2 || kind == 3 {
        return tzdata::gmt(z.off, kind == 3);
    }
    if z.tuple != 0 {
        let t = tzdata::tuple(z.tuple);
        let id = match kind {
            0 => t[0],
            1 => t[1],
            4 => t[if z.overlap { 4 } else { 2 }],
            _ => t[if z.overlap { 5 } else { 3 }],
        };
        return tzdata::name(id, z.off);
    }
    match z.entry {
        None => offset_name(z.off / 60, kind == 1 || kind == 5, false),
        Some(e) => {
            let i = match kind {
                0 => usize::from(z.dst),
                1 => 2 + usize::from(z.dst),
                4 => 4,
                _ => 5,
            };
            ZONE_NAMES[usize::from(e.2[i])].to_string()
        }
    }
}

fn locale_offset(l: &DateLocale, off: i64, long: bool) -> String {
    // he isolates the offset with LRMs: GMT\u{200e}+5\u{200e}, but GMT-5\u{200e} (GMT-05:00\u{200e}\u{200e} long).
    if let Some(prefix) = l.gmt[0].strip_suffix('\u{200e}') {
        let plain = locale_offset(&DateLocale { gmt: [prefix, l.gmt[1]], ..*l }, off, long);
        let (head, tail) = plain.split_at(prefix.len());
        return if off < 0 {
            format!("{head}{tail}\u{200e}{}", if long { "\u{200e}" } else { "" })
        } else {
            format!("{head}\u{200e}{tail}\u{200e}")
        };
    }
    let sign = if off < 0 { l.gmt[1] } else { "+" };
    let (h, m, sec) = (off.abs() / 3600, off.abs() / 60 % 60, off.abs() % 60);
    let secs = if sec != 0 { format!(":{sec:02}") } else { String::new() };
    if long {
        format!("{}{sign}{h:02}:{m:02}{secs}", l.gmt[0])
    } else if m != 0 || sec != 0 {
        format!("{}{sign}{h}:{m:02}{secs}", l.gmt[0])
    } else {
        format!("{}{sign}{h}", l.gmt[0])
    }
}

fn locale_zone_name(l: &DateLocale, kind: usize, z: &ZoneAt, zone: &str) -> String {
    if kind == 2 || kind == 3 {
        return locale_offset(l, z.off, kind == 3);
    }
    let long = kind == 1 || kind == 5;
    let overlap = kind >= 4 && z.overlap;
    let slot = match kind {
        0 => usize::from(z.dst),
        1 => 2 + usize::from(z.dst),
        4 if overlap => 6,
        4 => 4,
        _ if overlap => 7,
        _ => 5,
    };
    let at = |i: usize, slot: usize| l.zones.get(i * 8 + slot).copied().filter(|s| *s != u16::MAX);
    let read = |i: usize, slot: usize| {
        at(i, slot)
            .or_else(|| at(i, if slot >= 6 { slot - 2 } else { slot }))
            .map_or_else(|| locale_offset(l, z.off, long), |s| dtf_str(DTF_STRINGS_TEXT, DTF_STRINGS_ENDS, s).to_string())
    };
    let own = || match ZONES.iter().position(|e| e.0 == zone) {
        Some(i) => read(i, slot),
        None => locale_offset(l, z.off, long),
    };
    // a historical period: its metazone's names, as the encoder picked them for this locale (a locale
    // added since keeps the zone's own current names)
    let li = tzdata::TZ_LOCALES.iter().position(|t| *t == l.tags[0]);
    if let (true, Some(li)) = (z.tuple != 0, li) {
        let (src, suffix) = tzdata::locale_src(z.tuple, li, [0, 0, 1, 1, 2, 3, 4, 5][slot]);
        let mut name = match src {
            tzdata::Src::Slot(r, k) => Some(read(r, k)),
            tzdata::Src::Gmt => Some(locale_offset(l, z.off, long)),
            tzdata::Src::Str(id) => Some(tzdata::name(id, z.off)),
            tzdata::Src::Own => None,
        }
        .unwrap_or_else(own);
        if let Some(id) = suffix {
            name.push_str(&tzdata::name(id, z.off));
        }
        return name;
    }
    own()
}


// ---- formatting ----

pub(super) fn format(spec: &Spec, t: f64) -> Result<Parts, Fault> {
    if !t.is_finite() || t.abs() > 8.64e15 {
        return range("Invalid time value");
    }
    let pattern = spec.get("@pattern").unwrap_or("");
    format_pattern(spec, t, pattern, pattern)
}

/// t formatted with pattern; ctx is the whole pattern it is cut from (a range's half), which
/// decides whether noon and midnight show as such.
fn format_pattern(spec: &Spec, t: f64, pattern: &str, ctx: &str) -> Result<Parts, Fault> {
    let ms = t as i64;
    let zone = spec.get("@zone").or(spec.get("timeZone")).unwrap_or("UTC");
    let gmt = spec.get("@gmt").is_some();
    let secs = ms.div_euclid(1000);
    let za = zone_at(zone, secs, gmt);
    let off = za.off;
    let local = ms + off * 1000;
    let days = local.div_euclid(86_400_000);
    let in_day = local.rem_euclid(86_400_000);
    let (y, mo, d) = civil(days);
    let cal = spec.get("@cal").and_then(|i| i.parse::<usize>().ok()).map(calendar::get);
    let cal = cal.map(|l| (l, calendar::fields(usize::from(l.cal), days, (y, mo, d))));
    let mut mark = None;
    let wd = (days + 4).rem_euclid(7) as usize;
    let (h, mi, s, frac) = (in_day / 3_600_000, in_day / 60_000 % 60, in_day / 1000 % 60, in_day % 1000);
    let dl = spec.get("@dl").and_then(|i| i.parse::<usize>().ok()).and_then(|i| DATE_LOCALES.get(i));
    let exact = (field_width(ctx, 'm') == 0 || mi == 0) && (field_width(ctx, 's') == 0 || s == 0);
    let width = |n: usize| match n {
        4 => 0,
        5 => 2,
        _ => 1,
    };
    let mut out = Parts::default();
    let chars: alloc::vec::Vec<char> = pattern.chars().collect();
    let mut i = 0;
    let num = |v: i64, width: usize| if width >= 2 { format!("{:02}", v % 100) } else { v.to_string() };
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            let mut j = i + 1;
            let mut lit = String::new();
            while j < chars.len() {
                if chars[j] == '\'' {
                    if chars.get(j + 1) == Some(&'\'') {
                        lit.push('\'');
                        j += 2;
                        continue;
                    }
                    break;
                }
                lit.push(chars[j]);
                j += 1;
            }
            if lit.is_empty() && j == i + 1 {
                lit.push('\'');
            }
            out.push("literal", &lit);
            i = j + 1;
            continue;
        }
        if let Some(m) = calendar::mark(c) {
            mark = Some(m);
            i += 1;
            continue;
        }
        if !c.is_ascii_alphabetic() {
            let mut lit = String::new();
            lit.push(c);
            out.push("literal", &lit);
            i += 1;
            continue;
        }
        let mut n = 1;
        while chars.get(i + n) == Some(&c) {
            n += 1;
        }
        i += n;
        if let Some((l, f)) = &cal
            && let Some((ty, v)) = calendar::field(l, f, c, n, mark.take())
        {
            out.push(ty, &v);
            continue;
        }
        let era_year = if y <= 0 { 1 - y } else { y };
        if let Some(l) = dl {
            let standalone = usize::from(c == 'c' || c == 'L') * 3;
            let v = match c {
                'E' | 'c' => Some(("weekday", l.days[standalone + width(n)][wd].to_string())),
                'M' | 'L' if n >= 3 => Some(("month", l.months[standalone + width(n)][(mo - 1) as usize].to_string())),
                'G' => Some(("era", l.eras[width(n)][usize::from(y > 0)].to_string())),
                'a' => Some(("dayPeriod", l.ampm[usize::from(h >= 12)].to_string())),
                'B' => {
                    let k = match h {
                        0 if exact => 24,
                        12 if exact => 25,
                        _ => h as usize,
                    };
                    Some(("dayPeriod", l.periods[width(n)][k].to_string()))
                }
                'z' | 'O' | 'v' => {
                    let kind = match (c, n) {
                        ('z', 4) => 1,
                        ('z', _) => 0,
                        ('O', 4) => 3,
                        ('O', _) => 2,
                        ('v', 4) if gmt => 1,
                        ('v', _) if gmt => 0,
                        ('v', 4) => 5,
                        _ => 4,
                    };
                    Some(("timeZoneName", locale_zone_name(l, kind, &za, zone)))
                }
                _ => None,
            };
            if let Some((ty, v)) = v {
                out.push(ty, &v);
                continue;
            }
        }
        match c {
            'E' => {
                let name = DAY_NAMES[wd];
                let v = match n {
                    4 => name.to_string(),
                    5 => name[..1].to_string(),
                    _ => name[..3].to_string(),
                };
                out.push("weekday", &v);
            }
            'G' => {
                let v = match (n, y > 0) {
                    (4, true) => "Anno Domini",
                    (4, false) => "Before Christ",
                    (5, true) => "A",
                    (5, false) => "B",
                    (_, true) => "AD",
                    _ => "BC",
                };
                out.push("era", v);
            }
            'y' => out.push("year", &if n == 2 { format!("{:02}", era_year % 100) } else { era_year.to_string() }),
            'M' | 'L' => {
                let name = MONTH_NAMES[(mo - 1) as usize];
                let v = match n {
                    1 | 2 => num(i64::from(mo), n),
                    3 => name[..3].to_string(),
                    4 => name.to_string(),
                    _ => name[..1].to_string(),
                };
                out.push("month", &v);
            }
            'd' => out.push("day", &num(i64::from(d), n)),
            'h' => out.push("hour", &num(if h % 12 == 0 { 12 } else { h % 12 }, n)),
            'K' => out.push("hour", &num(h % 12, n)),
            'H' => out.push("hour", &num(h, n)),
            'k' => out.push("hour", &num(if h == 0 { 24 } else { h }, n)),
            'm' => out.push("minute", &num(mi, n)),
            's' => out.push("second", &num(s, n)),
            'S' => {
                let digits = format!("{frac:03}");
                out.push("fractionalSecond", &digits[..n.min(3)]);
            }
            'a' => {
                let (am, pm) = spec.get("@ampm").and_then(|s| s.split_once(',')).unwrap_or(("AM", "PM"));
                out.push("dayPeriod", if h < 12 { am } else { pm });
            }
            'B' => {
                let v = match h {
                    12 if mi == 0 && s == 0 => if n == 5 { "n" } else { "noon" },
                    0..=11 => "in the morning",
                    12..=17 => "in the afternoon",
                    18..=20 => "in the evening",
                    _ => "at night",
                };
                out.push("dayPeriod", v);
            }
            'z' | 'O' | 'v' => {
                let kind = match (c, n) {
                    ('z', 4) => 1,
                    ('z', _) => 0,
                    ('O', 4) => 3,
                    ('O', _) => 2,
                    ('v', 4) if gmt => 1,
                    ('v', _) if gmt => 0,
                    ('v', 4) => 5,
                    _ => 4,
                };
                let alt = (kind >= 4 && za.overlap && za.tuple == 0)
                    .then(|| ZONES.iter().position(|z| z.0 == zone))
                    .flatten()
                    .and_then(|i| EN_GENERIC_ALT.iter().find(|a| usize::from(a.0) == i));
                match alt {
                    Some(a) => out.push("timeZoneName", dtf_str(DTF_STRINGS_TEXT, DTF_STRINGS_ENDS, if kind == 4 { a.1 } else { a.2 })),
                    None => out.push("timeZoneName", &zone_name(kind, &za)),
                }
            }
            _ => {}
        }
    }
    let nu = spec.get("numberingSystem").unwrap_or("latn");
    if matches!(nu, "arab" | "arabext") {
        for i in 1..out.items.len() {
            if out.items[i].0 == "fractionalSecond" && out.items[i - 1].0 == "literal" && out.items[i - 1].1.ends_with(['.', ',']) {
                let lit = &mut out.items[i - 1].1;
                lit.pop();
                lit.push('\u{66b}');
            }
        }
    }
    if nu != "latn" {
        for (_, v) in out.items.iter_mut() {
            if v.bytes().any(|b| b.is_ascii_digit()) {
                *v = v.chars().map(|c| c.to_digit(10).map(|d| nu_digit(nu, d)).unwrap_or(c)).collect();
            }
        }
    }
    Ok(out)
}
