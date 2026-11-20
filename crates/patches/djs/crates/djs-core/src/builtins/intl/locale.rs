//! Intl.Locale, Intl.getCanonicalLocales's canonicalization and Intl.supportedValuesOf
//! (ECMA-402 14, 6.2, 8.3.2), as node 22 (V8 over ICU 78, CLDR 48) does them, from the tables
//! scripts/intl-loc/gen.js derives from node into locdata.rs.
//!
//! A Locale's resolved spec is its canonical tag. Canonicalization is V8's: the tag must
//! start with a well-formed unicode_language_id ("Incorrect locale information provided"),
//! then ICU parses the whole tag ("Invalid language tag: tag") and replaces aliases
//! (UTS #35 annex C) and sorts variants, extensions and keywords.

use super::locdata as d;
use super::range;
use crate::interp::conv::Fault;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The value of key in a locdata table ("key value" lines sorted by key).
pub(super) fn lookup(table: &'static str, key: &str) -> Option<&'static str> {
    let b = table.as_bytes();
    let (mut lo, mut hi) = (0usize, b.len());
    while lo < hi {
        let mut mid = (lo + hi) / 2;
        while mid > lo && b[mid - 1] != b'\n' {
            mid -= 1;
        }
        let end = table[mid..].find('\n').map_or(b.len(), |e| mid + e);
        let line = &table[mid..end];
        let (k, v) = line.split_once(' ').unwrap_or((line, ""));
        match k.cmp(key) {
            core::cmp::Ordering::Equal => return Some(v),
            core::cmp::Ordering::Less => lo = end + 1,
            core::cmp::Ordering::Greater => hi = mid,
        }
    }
    None
}

fn alpha(s: &str) -> bool {
    s.bytes().all(|c| c.is_ascii_alphabetic())
}
fn alnum(s: &str) -> bool {
    s.bytes().all(|c| c.is_ascii_alphanumeric())
}
pub(super) fn is_language(s: &str) -> bool {
    alpha(s) && (s.len() == 2 || s.len() == 3 || (5..=8).contains(&s.len()))
}
pub(super) fn is_script(s: &str) -> bool {
    s.len() == 4 && alpha(s)
}
pub(super) fn is_region(s: &str) -> bool {
    (s.len() == 2 && alpha(s)) || (s.len() == 3 && s.bytes().all(|c| c.is_ascii_digit()))
}
fn is_variant(s: &str) -> bool {
    alnum(s) && ((5..=8).contains(&s.len()) || (s.len() == 4 && s.as_bytes()[0].is_ascii_digit()))
}
fn is_singleton(s: &str) -> bool {
    s.len() == 1 && alnum(s)
}

/// V8's JSLocale::StartsWithUnicodeLanguageId.
fn starts_with_language_id(tag: &str) -> bool {
    // As std::getline: a trailing "-" adds no token.
    let t: Vec<&str> = tag.strip_suffix('-').unwrap_or(tag).split('-').collect();
    if tag.is_empty() || !is_language(t[0]) {
        return false;
    }
    let mut i = 1;
    if i == t.len() || is_singleton(t[i]) {
        return true;
    }
    if is_script(t[i]) {
        i += 1;
        if i == t.len() {
            return true;
        }
    }
    if is_region(t[i]) {
        i += 1;
    }
    while i < t.len() {
        if is_singleton(t[i]) {
            return true;
        }
        if !is_variant(t[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// A parsed tag: lowercase except script (Title) and region (upper).
#[derive(Clone, Default)]
pub(super) struct Tag {
    pub lang: String,
    pub script: String,
    pub region: String,
    pub variants: Vec<String>,
    /// The u extension's attributes and keywords (key, type; type "" is true).
    pub attrs: Vec<String>,
    pub keys: Vec<(String, String)>,
    pub has_u: bool,
    /// The t extension's language (canonical tag or "") and fields.
    pub tlang: String,
    pub tfields: Vec<(String, String)>,
    pub has_t: bool,
    /// Other extensions (singleton, its subtags joined by "-").
    pub other: Vec<(u8, String)>,
    pub private: String,
}

fn title(s: &str) -> String {
    let mut o = s[..1].to_ascii_uppercase();
    o.push_str(&s[1..].to_ascii_lowercase());
    o
}

/// Parses a unicode_locale_id (lowercased); None if it is not well-formed.
fn parse(t: &str) -> Option<Tag> {
    let sub: Vec<&str> = t.split('-').collect();
    let mut tag = Tag::default();
    let mut i = 0;
    if !is_language(sub[0]) {
        return None;
    }
    tag.lang = sub[0].to_string();
    i += 1;
    if i < sub.len() && is_script(sub[i]) {
        tag.script = title(sub[i]);
        i += 1;
    }
    if i < sub.len() && is_region(sub[i]) {
        tag.region = sub[i].to_ascii_uppercase();
        i += 1;
    }
    while i < sub.len() && is_variant(sub[i]) {
        if tag.variants.iter().any(|v| v == sub[i]) {
            return None;
        }
        tag.variants.push(sub[i].to_string());
        i += 1;
    }
    let mut seen: Vec<u8> = Vec::new();
    while i < sub.len() {
        let s = sub[i];
        if !is_singleton(s) {
            return None;
        }
        let c = s.as_bytes()[0];
        if seen.contains(&c) {
            return None;
        }
        seen.push(c);
        i += 1;
        let start = i;
        match c {
            b'x' => {
                while i < sub.len() && !sub[i].is_empty() && sub[i].len() <= 8 && alnum(sub[i]) {
                    i += 1;
                }
                if i == start || i != sub.len() {
                    return None;
                }
                tag.private = sub[start..].join("-");
            }
            b'u' => {
                tag.has_u = true;
                while i < sub.len() && (3..=8).contains(&sub[i].len()) && alnum(sub[i]) {
                    if !tag.attrs.iter().any(|a| a == sub[i]) {
                        tag.attrs.push(sub[i].to_string());
                    }
                    i += 1;
                }
                while i < sub.len() && sub[i].len() == 2 && alnum(sub[i]) && sub[i].as_bytes()[1].is_ascii_alphabetic() {
                    let key = sub[i];
                    i += 1;
                    let ts = i;
                    while i < sub.len() && (3..=8).contains(&sub[i].len()) && alnum(sub[i]) {
                        i += 1;
                    }
                    if !tag.keys.iter().any(|(k, _)| k == key) {
                        tag.keys.push((key.to_string(), sub[ts..i].join("-")));
                    }
                }
                if i == start {
                    return None;
                }
            }
            b't' => {
                tag.has_t = true;
                // tlang: a unicode_language_id without extensions.
                if i < sub.len() && is_language(sub[i]) {
                    let ls = i;
                    i += 1;
                    if i < sub.len() && is_script(sub[i]) {
                        i += 1;
                    }
                    if i < sub.len() && is_region(sub[i]) {
                        i += 1;
                    }
                    while i < sub.len() && is_variant(sub[i]) {
                        i += 1;
                    }
                    tag.tlang = sub[ls..i].join("-");
                }
                while i < sub.len() && sub[i].len() == 2 && sub[i].as_bytes()[0].is_ascii_alphabetic() && sub[i].as_bytes()[1].is_ascii_digit() {
                    let key = sub[i];
                    i += 1;
                    let ts = i;
                    while i < sub.len() && (3..=8).contains(&sub[i].len()) && alnum(sub[i]) {
                        i += 1;
                    }
                    if ts == i {
                        return None;
                    }
                    if !tag.tfields.iter().any(|(k, _)| k == key) {
                        tag.tfields.push((key.to_string(), sub[ts..i].join("-")));
                    }
                }
                if i == start {
                    return None;
                }
            }
            _ => {
                while i < sub.len() && (2..=8).contains(&sub[i].len()) && alnum(sub[i]) {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                tag.other.push((c, sub[start..i].join("-")));
            }
        }
    }
    Some(tag)
}

impl Tag {
    pub fn base_name(&self) -> String {
        let mut s = self.lang.clone();
        for p in [&self.script, &self.region] {
            if !p.is_empty() {
                s.push('-');
                s.push_str(p);
            }
        }
        for v in &self.variants {
            s.push('-');
            s.push_str(v);
        }
        s
    }
    pub fn to_tag(&self) -> String {
        let mut s = self.base_name();
        let mut ext: Vec<(u8, String)> = self.other.clone();
        if self.has_u && (!self.attrs.is_empty() || !self.keys.is_empty()) {
            let mut u: Vec<String> = self.attrs.clone();
            for (k, v) in &self.keys {
                u.push(if v.is_empty() { k.clone() } else { format!("{k}-{v}") });
            }
            ext.push((b'u', u.join("-")));
        }
        if self.has_t && (!self.tlang.is_empty() || !self.tfields.is_empty()) {
            let mut t: Vec<String> = Vec::new();
            if !self.tlang.is_empty() {
                t.push(self.tlang.clone());
            }
            for (k, v) in &self.tfields {
                t.push(format!("{k}-{v}"));
            }
            ext.push((b't', t.join("-")));
        }
        ext.sort_by_key(|e| e.0);
        for (c, v) in ext {
            s.push('-');
            s.push(c as char);
            s.push('-');
            s.push_str(&v);
        }
        if !self.private.is_empty() {
            s.push_str("-x-");
            s.push_str(&self.private);
        }
        s
    }
    pub fn key(&self, k: &str) -> Option<&str> {
        self.keys.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    pub fn set_key(&mut self, k: &str, v: &str) {
        self.has_u = true;
        match self.keys.iter_mut().find(|(n, _)| n == k) {
            Some(e) => e.1 = v.to_string(),
            None => {
                self.keys.push((k.to_string(), v.to_string()));
                self.keys.sort_by(|a, b| a.0.cmp(&b.0));
            }
        }
    }
}

/// Replaces lang (and the fields the rule matched) by an alias replacement, filling the
/// fields the tag lacks (UTS #35 annex C).
fn apply_lang(tag: &mut Tag, repl: &str, matched_region: bool) {
    let r: Vec<&str> = repl.split('-').collect();
    tag.lang = r[0].to_ascii_lowercase();
    if matched_region {
        tag.region.clear();
    }
    let mut i = 1;
    if i < r.len() && is_script(r[i]) {
        if tag.script.is_empty() {
            tag.script = title(r[i]);
        }
        i += 1;
    }
    if i < r.len() && is_region(r[i]) {
        if tag.region.is_empty() {
            tag.region = r[i].to_ascii_uppercase();
        }
        i += 1;
    }
    for v in &r[i..] {
        let v = v.to_ascii_lowercase();
        if !tag.variants.contains(&v) {
            tag.variants.push(v);
        }
    }
}

fn canonical_language_id(tag: &mut Tag) {
    for _ in 0..8 {
        let mut changed = false;
        // lang-variant rules, then lang-region, then lang.
        let mut hit = None;
        if tag.variants.len() > 1 {
            let mut vs = tag.variants.clone();
            vs.sort();
            let joined = vs.join("-");
            let r = lookup(d::LANG_ALIAS, &format!("{}-{}", tag.lang, joined)).or_else(|| lookup(d::LANG_ALIAS, &format!("und-{joined}")));
            if let Some(r) = r {
                tag.variants.clear();
                let lang = core::mem::take(&mut tag.lang);
                apply_lang(tag, r, false);
                if tag.lang == "und" {
                    tag.lang = lang;
                }
                continue;
            }
        }
        for (i, v) in tag.variants.iter().enumerate() {
            if let Some(r) = lookup(d::LANG_ALIAS, &format!("{}-{}", tag.lang, v)) {
                hit = Some((i, r));
                break;
            }
            if tag.lang != "und"
                && let Some(r) = lookup(d::LANG_ALIAS, &format!("und-{v}")) {
                    hit = Some((i, r));
                    break;
                }
        }
        if let Some((i, r)) = hit {
            tag.variants.remove(i);
            let lang = core::mem::take(&mut tag.lang);
            apply_lang(tag, r, false);
            if tag.lang == "und" {
                tag.lang = lang;
            }
            changed = true;
        } else if let Some(r) = (!tag.region.is_empty()).then(|| lookup(d::LANG_ALIAS, &format!("{}-{}", tag.lang, tag.region.to_ascii_lowercase()))).flatten() {
            apply_lang(tag, r, true);
            changed = true;
        } else if let Some(r) = lookup(d::LANG_ALIAS, &tag.lang) {
            apply_lang(tag, r, false);
            changed = true;
        }
        if !tag.script.is_empty()
            && let Some(r) = lookup(d::SCRIPT_ALIAS, &tag.script) {
                tag.script = r.to_string();
                changed = true;
            }
        if !tag.region.is_empty()
            && let Some(r) = lookup(d::REGION_ALIAS, &tag.region) {
                let opts: Vec<&str> = r.split(' ').collect();
                let mut pick = opts[0];
                if opts.len() > 1 {
                    let mut probe = Tag { lang: tag.lang.clone(), script: tag.script.clone(), ..Tag::default() };
                    maximize(&mut probe);
                    if let Some(p) = opts.iter().find(|o| **o == probe.region) {
                        pick = p;
                    }
                }
                tag.region = pick.to_string();
                changed = true;
            }
        for v in tag.variants.iter_mut() {
            if let Some(r) = lookup(d::VARIANT_ALIAS, v) {
                *v = r.to_string();
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    tag.variants.sort();
    tag.variants.dedup();
}

fn ext_type(ext: &str, key: &str, ty: &str) -> String {
    let ty = ty.to_ascii_lowercase();
    match lookup(d::EXT_ALIAS, &format!("{ext}-{key}-{ty}")) {
        Some("-") => String::new(),
        Some(r) => r.to_string(),
        None if ty == "true" && ext == "u" => String::new(),
        None => ty,
    }
}

fn canonical_tag(mut tag: Tag) -> Tag {
    canonical_language_id(&mut tag);
    // ICU turns the variant posix into the keyword va-posix.
    if let Some(i) = tag.variants.iter().position(|v| v == "posix") {
        tag.variants.remove(i);
        if tag.key("va").is_none() {
            tag.set_key("va", "posix");
        }
    }
    tag.attrs.sort();
    for i in 0..tag.keys.len() {
        let (k, v) = tag.keys[i].clone();
        tag.keys[i].1 = ext_type("u", &k, &v);
    }
    tag.keys.sort_by(|a, b| a.0.cmp(&b.0));
    if !tag.tlang.is_empty()
        && let Some(mut t) = parse(&tag.tlang) {
            canonical_language_id(&mut t);
            tag.tlang = t.base_name().to_ascii_lowercase();
        }
    for i in 0..tag.tfields.len() {
        let (k, v) = tag.tfields[i].clone();
        tag.tfields[i].1 = ext_type("t", &k, &v);
    }
    tag.tfields.sort_by(|a, b| a.0.cmp(&b.0));
    tag
}

/// CanonicalizeUnicodeLocaleId of a language tag as V8 does it, with its errors.
pub(super) fn canonicalize_tag(tag: &str) -> Result<Tag, Fault> {
    if !starts_with_language_id(tag) {
        return range("Incorrect locale information provided");
    }
    let lower = tag.to_ascii_lowercase();
    match parse(&lower) {
        Some(t) => Ok(canonical_tag(t)),
        None => range(&format!("Invalid language tag: {lower}")),
    }
}

pub(super) fn canonicalize(tag: &str) -> Result<String, Fault> {
    canonicalize_tag(tag).map(|t| t.to_tag())
}

// ---- likely subtags --------------------------------------------------------------------

fn lang_known(lang: &str) -> bool {
    if lookup(d::LIKELY, lang).is_some() {
        return true;
    }
    // A language with only L-S or L-R keys.
    let probe = format!("{lang}-");
    let b = d::LIKELY;
    b.find(&format!("\n{probe}")).is_some() || b.starts_with(&probe)
}

fn likely_value(key: &str, lang: &str) -> Option<(String, String, String)> {
    let v = lookup(d::LIKELY, key)?;
    let full = match v.strip_prefix('~') {
        Some(rest) => format!("{lang}{rest}"),
        None => v.to_string(),
    };
    let p: Vec<&str> = full.split('-').collect();
    Some((p[0].to_string(), p.get(1).unwrap_or(&"").to_string(), p.get(2).unwrap_or(&"").to_string()))
}

/// AddLikelySubtags as ICU 78 does it: language, then script (or any), then region (or any).
pub(super) fn maximize(tag: &mut Tag) {
    // ICU keeps a full L-S-R as it is, even with Zzzz or ZZ, except a language whose
    // likely region is 001 with region ZZ.
    if tag.lang != "und" && !tag.script.is_empty() && !tag.region.is_empty() {
        let world = tag.region == "ZZ" && likely_value(&tag.lang, &tag.lang).is_some_and(|v| v.2 == "001");
        if !world {
            return;
        }
    }
    if tag.lang == "und" {
        tag.lang.clear();
    }
    if tag.script == "Zzzz" {
        tag.script.clear();
    }
    if tag.region == "ZZ" {
        tag.region.clear();
    }
    let undo = |t: &mut Tag| if t.lang.is_empty() { t.lang = "und".to_string() };
    if !tag.lang.is_empty() && !tag.script.is_empty() && !tag.region.is_empty() {
        return;
    }
    let l = if tag.lang.is_empty() { "und" } else { tag.lang.as_str() }.to_string();
    if l != "und" && !lang_known(&l) {
        undo(tag);
        return;
    }
    let mut node = l.clone();
    if !tag.script.is_empty() {
        let ls = format!("{l}-{}", tag.script);
        let has = lookup(d::LIKELY, &ls).is_some() || d::LIKELY.contains(&format!("\n{ls}-"));
        if has {
            node = ls;
        }
    }
    let mut found = None;
    if !tag.region.is_empty() {
        found = likely_value(&format!("{node}-{}", tag.region), &l);
    }
    let found = found.or_else(|| likely_value(&node, &l)).or_else(|| likely_value(&l, &l)).or_else(|| likely_value("und", &l));
    if let Some((fl, fs, fr)) = found {
        if tag.lang.is_empty() {
            tag.lang = fl;
        }
        if tag.script.is_empty() {
            tag.script = fs;
        }
        if tag.region.is_empty() {
            tag.region = fr;
        }
    }
    undo(tag);
}

fn same_lsr(a: &Tag, b: &Tag) -> bool {
    a.lang == b.lang && a.script == b.script && a.region == b.region
}

pub(super) fn minimize(tag: &mut Tag) {
    let lsr = format!("{}-{}-{}", tag.lang, tag.script, tag.region);
    if lookup(d::MIN_KEEP, &lsr).is_some() {
        return;
    }
    let mut max = tag.clone();
    if max.script == "Zzzz" {
        max.script.clear();
    }
    if max.region == "ZZ" {
        max.region.clear();
    }
    maximize(&mut max);
    for (s, r) in [(false, false), (false, true), (true, false)] {
        let mut t = Tag { lang: max.lang.clone(), ..Tag::default() };
        if s {
            t.script = max.script.clone();
        }
        if r {
            t.region = max.region.clone();
        }
        let mut m = t.clone();
        maximize(&mut m);
        if same_lsr(&m, &max) {
            tag.lang = t.lang;
            tag.script = t.script;
            tag.region = t.region;
            return;
        }
    }
    tag.lang = max.lang;
    tag.script = max.script;
    tag.region = max.region;
}

// ---- Locale.prototype's getters --------------------------------------------------------

/// The u extension keys Locale's options set, in option order.
const OPTION_KEYS: [(&str, &str); 6] = [
    ("calendar", "ca"),
    ("collation", "co"),
    ("hourCycle", "hc"),
    ("caseFirst", "kf"),
    ("numeric", "kn"),
    ("numberingSystem", "nu"),
];

/// A u extension type of the Locale options (type: one or more 3-8 alphanumerics).
fn is_type(s: &str) -> bool {
    !s.is_empty() && s.split('-').all(|p| (3..=8).contains(&p.len()) && alnum(p))
}

/// Intl.Locale ( tag, options ): the canonical tag of tag with the options applied.
pub(super) fn construct(tag: &str, raw: &super::Spec) -> Result<String, Fault> {
    let bad = || range("Incorrect locale information provided");
    let mut t = canonicalize_tag(tag)?;
    if let Some(l) = raw.get("language") {
        if !is_language(l) {
            return bad();
        }
        t.lang = l.to_ascii_lowercase();
    }
    if let Some(s) = raw.get("script") {
        if !is_script(s) {
            return bad();
        }
        t.script = title(s);
    }
    if let Some(r) = raw.get("region") {
        if !is_region(r) {
            return bad();
        }
        t.region = r.to_ascii_uppercase();
    }
    for (opt, key) in OPTION_KEYS {
        let Some(v) = raw.get(opt) else { continue };
        let v = match opt {
            "hourCycle" => raw.choose(opt, &["h11", "h12", "h23", "h24"], "", "locale")?.to_string(),
            "caseFirst" => raw.choose(opt, &["upper", "lower", "false"], "", "locale")?.to_string(),
            "numeric" => if v == "true" { String::new() } else { "false".to_string() },
            _ => {
                if !is_type(v) {
                    return bad();
                }
                v.to_ascii_lowercase()
            }
        };
        t.set_key(key, &v);
    }
    // ICU canonicalizes again after the options (an option can be an alias).
    canonicalize(&t.to_tag())
}

fn split_list(s: &str) -> Vec<&str> {
    if s.is_empty() { Vec::new() } else { s.split(',').collect() }
}

fn region_for_data(tag: &Tag, max: &Tag) -> String {
    for k in ["rg", "sd"] {
        if let Some(v) = tag.key(k)
            && v.len() >= 3 {
                return v[..2].to_ascii_uppercase();
            }
    }
    max.region.clone()
}

fn region_data(tag: &Tag, max: &Tag, locale_table: &'static str, region_table: &'static str, default: &'static str) -> &'static str {
    if tag.key("rg").is_none() && tag.key("sd").is_none()
        && let Some(v) = lookup(locale_table, &format!("{}-{}-{}", max.lang, max.script, max.region)) {
            return v;
        }
    lookup(region_table, &region_for_data(tag, max)).unwrap_or(default)
}

/// Locale data by ICU's resource fallback of the tag itself: L-S-R, L-S, L-R, L; und is root.
fn locale_data(tag: &Tag, table: &'static str, default: &'static str) -> &'static str {
    if tag.lang == "und" {
        return default;
    }
    let (l, s, r) = (&tag.lang, &tag.script, &tag.region);
    let mut keys: Vec<String> = Vec::new();
    for (ws, wr) in [(true, true), (true, false), (false, true), (false, false)] {
        let mut k = l.clone();
        if ws && !s.is_empty() {
            k.push('-');
            k.push_str(s);
        }
        if wr && !r.is_empty() {
            k.push('-');
            k.push_str(r);
        }
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    for k in keys {
        if let Some(v) = lookup(table, &k) {
            return v;
        }
    }
    default
}

/// The getter or method of Locale.prototype on the canonical tag spec: a string, an
/// "@"-prefixed list, "!t"/"!f", or None for undefined.
pub(super) fn get(spec: &str, what: &str) -> Result<Option<String>, Fault> {
    let tag = canonicalize_tag(spec)?;
    let mut max = tag.clone();
    maximize(&mut max);
    let list = |v: Vec<&str>| Some(format!("@{}", v.join(",")));
    Ok(match what {
        "toString" => Some(tag.to_tag()),
        // ICU keeps va-posix in the base name.
        "baseName" => Some(if tag.key("va") == Some("posix") { format!("{}-u-va-posix", tag.base_name()) } else { tag.base_name() }),
        "language" => (tag.lang != "und").then(|| tag.lang.clone()),
        "script" => (!tag.script.is_empty()).then(|| tag.script.clone()),
        "region" => (!tag.region.is_empty()).then(|| tag.region.clone()),
        "calendar" => tag.key("ca").map(|v| if v.is_empty() { "true".to_string() } else { v.to_string() }),
        "collation" => tag.key("co").map(|v| if v.is_empty() { "true".to_string() } else { v.to_string() }),
        "hourCycle" => tag.key("hc").map(|v| if v.is_empty() { "true".to_string() } else { v.to_string() }),
        "numberingSystem" => tag.key("nu").map(|v| if v.is_empty() { "true".to_string() } else { v.to_string() }),
        "caseFirst" => tag.key("kf").map(|v| v.to_string()),
        "numeric" => Some(if tag.key("kn").is_some_and(|v| v.is_empty() || v == "true") { "!t" } else { "!f" }.to_string()),
        "maximize" => {
            let mut t = tag.clone();
            maximize(&mut t);
            Some(t.to_tag())
        }
        "minimize" => {
            let mut t = tag.clone();
            minimize(&mut t);
            Some(t.to_tag())
        }
        "calendars" => match tag.key("ca") {
            Some("") => list(alloc::vec!["yes"]),
            Some(v) => list(alloc::vec![v]),
            _ => list(split_list(region_data(&tag, &max, d::LOCALE_CA, d::REGION_CA, "gregory"))),
        },
        "collations" => match tag.key("co") {
            Some("") => list(alloc::vec!["yes"]),
            Some(v) => list(alloc::vec![v]),
            _ => list(split_list(locale_data(&tag, d::LOCALE_CO, "emoji,eor"))),
        },
        "hourCycles" => match tag.key("hc") {
            Some("") => list(alloc::vec!["yes"]),
            Some(v) => list(alloc::vec![v]),
            _ => list(split_list(region_data(&tag, &max, d::LOCALE_HC, d::REGION_HC, "h23"))),
        },
        "numberingSystems" => match tag.key("nu") {
            Some("") => list(alloc::vec!["yes"]),
            Some(v) => list(alloc::vec![v]),
            _ => list(split_list(locale_data(&tag, d::LOCALE_NU, "latn"))),
        },
        "timeZones" => {
            if tag.region.is_empty() {
                None
            } else {
                list(split_list(lookup(d::REGION_TZ, &tag.region).unwrap_or("")))
            }
        }
        "textInfo" => {
            let rtl = d::RTL_SCRIPTS.split(',').any(|s| s == max.script);
            Some(if rtl { "rtl" } else { "ltr" }.to_string())
        }
        // firstDay:weekend:minimalDays
        "weekInfo" => {
            let w = region_data(&tag, &max, d::LOCALE_WK, d::REGION_WK, "1:6,7:1");
            let fw = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"].iter().position(|d| Some(*d) == tag.key("fw"));
            Some(match fw {
                Some(i) => format!("{}{}", i + 1, &w[w.find(':').unwrap_or(0)..]),
                None => w.to_string(),
            })
        }
        _ => None,
    })
}

/// Intl.supportedValuesOf ( key ).
pub(super) fn supported_values(key: &str) -> Result<&'static str, Fault> {
    Ok(match key {
        "calendar" => d::SUPPORTED_CALENDAR,
        "collation" => d::SUPPORTED_COLLATION,
        "currency" => d::SUPPORTED_CURRENCY,
        "numberingSystem" => d::SUPPORTED_NUMBERING_SYSTEM,
        "timeZone" => d::SUPPORTED_TIME_ZONE,
        "unit" => d::SUPPORTED_UNIT,
        _ => return range(&format!("Invalid key : {key}")),
    })
}
