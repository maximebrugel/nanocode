//! Intl.DisplayNames (ECMA-402 12) as node 22 has it, from the names
//! scripts/intl-loc/dngen.js takes from node 22's ICU into dndata.rs, for en, de, fr, es,
//! it, pt, nl, ja, zh, ko and ru (other locales resolve to one of them, as the other
//! formatters do, and use its names).

use super::dndata;
use super::locale::{self, lookup};
use super::{range, Spec};
use crate::interp::conv::Fault;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const FIELDS: [&str; 12] = ["era", "year", "quarter", "month", "weekOfYear", "weekday", "day", "dayPeriod", "hour", "minute", "second", "timeZoneName"];

/// Intl.DisplayNames ( locales, options ): the resolved options.
pub(super) fn resolve(locale: &str, raw: &Spec) -> Result<Spec, Fault> {
    let ctor = "Intl.DisplayNames";
    let style = raw.choose("style", &["narrow", "short", "long"], "long", ctor)?;
    if raw.get("type").is_none() {
        return crate::interp::conv::type_error("invalid_argument");
    }
    let ty = raw.choose("type", &["language", "region", "script", "currency", "calendar", "dateTimeField"], "", ctor)?;
    let fallback = raw.choose("fallback", &["code", "none"], "code", ctor)?;
    let display = raw.choose("languageDisplay", &["dialect", "standard"], "dialect", ctor)?;
    let mut out = Spec::default();
    out.put("locale", locale);
    out.put("style", style);
    out.put("type", ty);
    out.put("fallback", fallback);
    if ty == "language" {
        out.put("languageDisplay", display);
    }
    Ok(out)
}

fn bad<T>() -> Result<T, Fault> {
    range("invalid_argument")
}

struct Names {
    o: &'static str,
    t: &'static str,
    style: &'static str,
}

impl Names {
    /// The name of code of kind k (a long-name letter), by style.
    fn get(&self, k: char, code: &str) -> Option<&'static str> {
        let (o, t) = (self.o, self.t);
        // An overlay's "\u{0}" hides the language's line.
        let find = move |k: &str| match lookup(o, k) {
            Some("\u{0}") => None,
            Some(v) => Some(v),
            None => lookup(t, k),
        };
        let key = |c: char| format!("{c}:{code}");
        let short = match k {
            'l' => Some('L'),
            's' => Some('S'),
            'r' => Some('R'),
            'f' => Some('F'),
            'd' => Some('D'),
            _ => None,
        };
        let narrow = match k {
            'r' => Some('N'),
            'f' => Some('M'),
            _ => None,
        };
        if self.style == "narrow"
            && let Some(n) = narrow.and_then(|c| find(&key(c))) {
                return Some(n);
            }
        if self.style != "long"
            && let Some(s) = short.and_then(|c| find(&key(c))) {
                return Some(s);
            }
        find(&key(k))
    }
}

/// Intl.DisplayNames.prototype.of ( code ): the name, or None for undefined.
pub(super) fn of(spec: &Spec, code: &str) -> Result<Option<String>, Fault> {
    let locale = spec.get("locale").unwrap_or("en");
    let (o, t) = dndata::tables(locale);
    let names = Names { o, t, style: match spec.get("style") {
        Some("short") => "short",
        Some("narrow") => "narrow",
        _ => "long",
    } };
    let fallback = spec.get("fallback") != Some("none");
    let alpha = |s: &str| s.bytes().all(|c| c.is_ascii_alphabetic());
    let found = |n: Option<&str>, code: &str| Ok(match n {
        Some(n) => Some(n.to_string()),
        None if fallback => Some(code.to_string()),
        None => None,
    });
    match spec.get("type").unwrap_or("") {
        "region" => {
            if !locale::is_region(code) {
                return bad();
            }
            let alias = lookup(super::locdata::REGION_ALIAS, code).and_then(|a| a.split(' ').next()).filter(|a| *a != "ZZ");
            found(names.get('r', code).or_else(|| alias.and_then(|a| names.get('r', a))), code)
        }
        "script" => {
            if !locale::is_script(code) {
                return bad();
            }
            found(names.get('s', code), code)
        }
        "currency" => {
            if code.len() != 3 || !alpha(code) {
                return bad();
            }
            found(names.get('c', &code.to_ascii_uppercase()), code)
        }
        "calendar" => {
            let ok = !code.is_empty() && code.split('-').all(|p| (3..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric()));
            if !ok {
                return bad();
            }
            found(names.get('k', code), code)
        }
        "dateTimeField" => {
            if !FIELDS.contains(&code) {
                return bad();
            }
            found(names.get('f', code), code)
        }
        _ => language(&names, spec.get("languageDisplay") != Some("standard"), fallback, code),
    }
}

fn language(names: &Names, dialect: bool, fallback: bool, code: &str) -> Result<Option<String>, Fault> {
    let Ok(mut tag) = locale::canonicalize_tag(code) else { return bad() };
    // Canonicalization makes the variant posix a keyword; ICU names it as a variant.
    if tag.key("va") == Some("posix") && tag.keys.len() == 1 && tag.attrs.is_empty() {
        tag.keys.clear();
        tag.has_u = false;
        tag.variants.push("posix".to_string());
        tag.variants.sort();
    }
    if tag.has_u || tag.has_t || !tag.other.is_empty() || !tag.private.is_empty() {
        return bad();
    }
    let (l, s, r) = (tag.lang.as_str(), tag.script.as_str(), tag.region.as_str());
    let mut used_s = s.is_empty();
    let mut used_r = r.is_empty();
    let mut name: Option<&str> = None;
    // ICU's dialect names: L-S-R when the tag has both, else L-S or L-R.
    if dialect {
        let key = match (used_s, used_r) {
            (false, false) => format!("{l}-{s}-{r}"),
            (false, true) => format!("{l}-{s}"),
            (true, false) => format!("{l}-{r}"),
            (true, true) => String::new(),
        };
        if !key.is_empty()
            && let Some(n) = names.get('d', &key) {
                name = Some(n);
                used_s = true;
                used_r = true;
            }
    }
    let mut missing = false;
    let base = match name.or_else(|| names.get('l', l)) {
        Some(n) => n.to_string(),
        None if !fallback => return Ok(None),
        None if l == "und" => "root".to_string(),
        None => l.to_string(),
    };
    let mut parts: Vec<String> = Vec::new();
    if !used_s {
        parts.push(names.get('s', s).map(|n| n.to_string()).unwrap_or_else(|| {
            missing = true;
            s.to_string()
        }));
    }
    if !used_r {
        parts.push(names.get('r', r).map(|n| n.to_string()).unwrap_or_else(|| {
            missing = true;
            r.to_string()
        }));
    }
    // ICU looks the variants up as one (sorted, joined by _).
    if !tag.variants.is_empty() {
        let joined = tag.variants.join("_");
        parts.push(match (tag.variants.len(), names.get('v', &joined)) {
            (1, Some(n)) => n.to_string(),
            _ => {
                missing = true;
                joined.to_ascii_uppercase()
            }
        });
    }
    if missing && !fallback {
        return Ok(None);
    }
    if parts.is_empty() {
        return Ok(Some(base));
    }
    // ICU brackets the parentheses of the parts.
    let parts: Vec<String> = parts.iter().map(|p| p.replace('(', "[").replace(')', "]").replace('\u{ff08}', "\u{ff3b}").replace('\u{ff09}', "\u{ff3d}")).collect();
    let p = lookup(names.o, "P:").or_else(|| lookup(names.t, "P:")).unwrap_or(" (\u{1}, \u{1})");
    let mut it = p.split('\u{1}');
    let (open, sep, close) = (it.next().unwrap_or(" ("), it.next().unwrap_or(", "), it.next().unwrap_or(")"));
    Ok(Some(format!("{base}{open}{}{close}", parts.join(sep))))
}
