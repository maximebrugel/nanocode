//! Numbering systems for NumberFormat and RelativeTimeFormat: which ones ICU formats in, their
//! digits, and how the option and the -u-nu- keyword pick one (as date.rs does for dates).
use super::{Spec, range};
use crate::interp::conv::Fault;
use alloc::format;
use alloc::string::{String, ToString};

/// The decimal numbering systems ICU formats in (Intl.supportedValuesOf('numberingSystem')), with
/// each one's zero digit; a copy of date.rs's list.
const NU_ZEROS: [(&str, u32); 77] = [("adlm", 0x1e950), ("ahom", 0x11730), ("arab", 0x660), ("arabext", 0x6f0), ("bali", 0x1b50), ("beng", 0x9e6), ("bhks", 0x11c50), ("brah", 0x11066), ("cakm", 0x11136), ("cham", 0xaa50), ("deva", 0x966), ("diak", 0x11950), ("fullwide", 0xff10), ("gara", 0x10d40), ("gong", 0x11da0), ("gonm", 0x11d50), ("gujr", 0xae6), ("gukh", 0x16130), ("guru", 0xa66), ("hmng", 0x16b50), ("hmnp", 0x1e140), ("java", 0xa9d0), ("kali", 0xa900), ("kawi", 0x11f50), ("khmr", 0x17e0), ("knda", 0xce6), ("krai", 0x16d70), ("lana", 0x1a80), ("lanatham", 0x1a90), ("laoo", 0xed0), ("latn", 0x30), ("lepc", 0x1c40), ("limb", 0x1946), ("mathbold", 0x1d7ce), ("mathdbl", 0x1d7d8), ("mathmono", 0x1d7f6), ("mathsanb", 0x1d7ec), ("mathsans", 0x1d7e2), ("mlym", 0xd66), ("modi", 0x11650), ("mong", 0x1810), ("mroo", 0x16a60), ("mtei", 0xabf0), ("mymr", 0x1040), ("mymrepka", 0x116da), ("mymrpao", 0x116d0), ("mymrshan", 0x1090), ("mymrtlng", 0xa9f0), ("nagm", 0x1e4f0), ("newa", 0x11450), ("nkoo", 0x7c0), ("olck", 0x1c50), ("onao", 0x1e5f1), ("orya", 0xb66), ("osma", 0x104a0), ("outlined", 0x1ccf0), ("rohg", 0x10d30), ("saur", 0xa8d0), ("segment", 0x1fbf0), ("shrd", 0x111d0), ("sind", 0x112f0), ("sinh", 0xde6), ("sora", 0x110f0), ("sund", 0x1bb0), ("sunu", 0x11bf0), ("takr", 0x116c0), ("talu", 0x19d0), ("tamldec", 0xbe6), ("telu", 0xc66), ("thai", 0xe50), ("tibt", 0xf20), ("tirh", 0x114d0), ("tnsa", 0x16ac0), ("tols", 0x11de0), ("vaii", 0xa620), ("wara", 0x118e0), ("wcho", 0x1e2f0)];
const HANIDEC: [char; 10] = ['\u{3007}', '\u{4e00}', '\u{4e8c}', '\u{4e09}', '\u{56db}', '\u{4e94}', '\u{516d}', '\u{4e03}', '\u{516b}', '\u{4e5d}'];

pub(super) fn supported(nu: &str) -> bool {
    nu == "hanidec" || NU_ZEROS.iter().any(|z| z.0 == nu)
}

/// s with its ASCII digits written in numbering system nu.
pub(super) fn map_digits(nu: &str, s: &str) -> String {
    if nu == "latn" || nu.is_empty() {
        return s.to_string();
    }
    let zero = NU_ZEROS.iter().find(|z| z.0 == nu).map(|z| z.1);
    s.chars()
        // Arabic-Indic digits (arabext keeps the arab symbols) count as digits too.
        .map(|c| match c.to_digit(10).or_else(|| (c as u32).checked_sub(0x660).filter(|d| *d < 10)) {
            Some(d) if nu == "hanidec" => HANIDEC[d as usize],
            Some(d) => zero.and_then(|z| char::from_u32(z + d)).unwrap_or(c),
            None => c,
        })
        .collect()
}

/// The locale without its Unicode extension (en-u-nu-thai is en).
pub(super) fn base_locale(locale: &str) -> &str {
    locale.find("-u-").map_or(locale, |i| &locale[..i])
}

/// The numbering system a NumberFormat or RelativeTimeFormat formats in, and its locale as
/// resolvedOptions shows it: a supported option wins, else a supported -u-nu- keyword (which the
/// locale keeps unless the option overrode it), else the locale's own. The third value is the
/// locale's own system when the option chose latn over it, which V8's formatRange still uses.
pub(super) fn resolve(locale: &str, tags: &[String], raw: &Spec) -> Result<(String, String, Option<&'static str>), Fault> {
    let lang = locale.split('-').next();
    // A locale without data falls back with its keywords (th-u-nu-thai still writes Thai digits).
    let keywords = tags.iter().find(|t| t.split('-').next() == lang).or(tags.first()).map(|t| super::collate::unicode_keywords(t)).unwrap_or_default();
    let nu_key = keywords.iter().find(|(k, _)| *k == "nu").map(|(_, v)| v.as_str()).filter(|v| supported(v));
    let nu_opt = raw.get("numberingSystem");
    if let Some(v) = nu_opt
        && (v.is_empty() || !v.split('-').all(|p| (3..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric())))
    {
        return range(&format!("Invalid numberingSystem : {v}"));
    }
    let own = super::date::numbering_system(locale);
    let (nu, kept) = match (nu_opt.filter(|v| supported(v)), nu_key) {
        (Some(o), Some(k)) if o != k => (o.to_string(), None),
        (Some(o), _) => (o.to_string(), nu_key.map(|k| format!("-u-nu-{k}"))),
        (None, Some(k)) => (k.to_string(), Some(format!("-u-nu-{k}"))),
        (None, None) => (own.to_string(), None),
    };
    let range_nu = (nu == "latn" && own != "latn" && nu_key.is_none()).then_some(own);
    Ok((nu, format!("{locale}{}", kept.unwrap_or_default()), range_nu))
}
