//! jq's JSON: a reader of whitespace-separated values with jq's leniencies (`nan`,
//! `Infinity`, leading zeros) and messages (`Unfinished JSON term at EOF at line 2,
//! column 0`), and jq's printer (indent, `--tab`, `-S`, `-a`, colors). Hand-written
//! (D1180): jq's output format and error texts are what the goldens compare, and its
//! numbers keep their literals. Written for psh after jq's manual and observed output.

use std::io::{self, Read};
use std::rc::Rc;

use super::map::Map;
use super::num::Num;
use super::val::Val;

/// A parse error: jq's message, without the `jq: parse error: ` prefix.
#[derive(Debug)]
pub struct ParseError(pub String);

/// Reads values from a byte source one at a time, keeping jq's position: the line and
/// column of the next byte, and the lines read so far (jq reads its input by lines, and
/// a runtime error names the last line read).
pub struct Reader<R> {
    src: R,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    line: usize,
    col: usize,
    /// Newlines up to the end of the line a value ended on.
    pub lines_read: usize,
    /// A parse error ended the input.
    failed: bool,
    keys: Vec<Option<Rc<str>>>,
    /// The elements of the arrays being read, innermost last, so each array is made at
    /// its size once complete.
    items: Vec<Val>,
    /// The same for the objects being read.
    pairs: Vec<(Rc<str>, Val)>,
}

const CHUNK: usize = 64 * 1024;

impl<R: Read> Reader<R> {
    pub fn new(src: R) -> Reader<R> {
        Reader {
            src,
            buf: Vec::new(),
            pos: 0,
            eof: false,
            line: 1,
            col: 0,
            lines_read: 0,
            failed: false,
            keys: vec![None; 256],
            items: Vec::new(),
            pairs: Vec::new(),
        }
    }

    fn fill(&mut self) -> io::Result<bool> {
        if self.eof {
            return Ok(false);
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        // Read straight into the buffer's tail, which keeps its allocation from one
        // fill to the next.
        let len = self.buf.len();
        self.buf.resize(len + CHUNK, 0);
        loop {
            match self.src.read(&mut self.buf[len..]) {
                Ok(0) => {
                    self.buf.truncate(len);
                    self.eof = true;
                    return Ok(false);
                }
                Ok(n) => {
                    self.buf.truncate(len + n);
                    return Ok(true);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.buf.truncate(len);
                    return Err(e);
                }
            }
        }
    }

    fn peek(&mut self) -> io::Result<Option<u8>> {
        if self.pos >= self.buf.len() && !self.fill()? {
            return Ok(None);
        }
        Ok(Some(self.buf[self.pos]))
    }

    fn bump(&mut self) -> io::Result<Option<u8>> {
        let b = self.peek()?;
        if let Some(c) = b {
            self.pos += 1;
            if c == b'\n' {
                self.line += 1;
                self.col = 0;
            } else if c & 0xC0 != 0x80 {
                self.col += 1;
            }
        }
        Ok(b)
    }

    fn error<T>(&self, msg: &str) -> Result<T, ParseError> {
        Err(ParseError(format!(
            "{msg} at line {}, column {}",
            self.line, self.col
        )))
    }

    fn eof_error<T>(&self, msg: &str) -> Result<T, ParseError> {
        Err(ParseError(format!(
            "{msg} at EOF at line {}, column {}",
            self.line, self.col
        )))
    }

    fn skip_ws(&mut self) -> io::Result<()> {
        let rest = &self.buf[self.pos..];
        let n = rest
            .iter()
            .position(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
            .unwrap_or(rest.len());
        let blanks = &rest[..n];
        match blanks.iter().rposition(|&b| b == b'\n') {
            Some(last) => {
                self.line += blanks.iter().filter(|&&b| b == b'\n').count();
                self.col = n - last - 1;
            }
            None => self.col += n,
        }
        self.pos += n;
        while let Some(b) = self.peek()? {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.bump()?;
            } else {
                break;
            }
        }
        Ok(())
    }

    /// After a value: jq has read the rest of its line.
    fn note_line(&mut self) -> io::Result<()> {
        let mut extra = 0;
        let mut i = self.pos;
        loop {
            if i >= self.buf.len() {
                if self.eof {
                    break;
                }
                let before = self.buf.len();
                let consumed = self.pos;
                self.fill()?;
                // `fill` moves the buffer start to `pos`.
                i -= consumed.min(i);
                if self.buf.len() == before - consumed {
                    break;
                }
                continue;
            }
            if self.buf[i] == b'\n' {
                extra = 1;
                break;
            }
            i += 1;
        }
        self.lines_read = self.line - 1 + extra;
        Ok(())
    }

    /// `-R`: the next line without its newline, `None` at the end.
    pub fn next_line(&mut self) -> Option<Result<String, ParseError>> {
        let mut line = Vec::new();
        loop {
            match self.bump() {
                Err(e) => return Some(Err(ParseError(e.to_string()))),
                Ok(None) => {
                    if line.is_empty() {
                        return None;
                    }
                    break;
                }
                Ok(Some(b'\n')) => {
                    self.lines_read += 1;
                    return Some(Ok(String::from_utf8_lossy(&line).into_owned()));
                }
                Ok(Some(b)) => line.push(b),
            }
        }
        Some(Ok(String::from_utf8_lossy(&line).into_owned()))
    }

    /// `-R -s`: everything left, as text.
    pub fn rest(mut self) -> Result<String, ParseError> {
        let mut all = self.buf.split_off(self.pos);
        self.src
            .read_to_end(&mut all)
            .map_err(|e| ParseError(e.to_string()))?;
        Ok(String::from_utf8_lossy(&all).into_owned())
    }

    /// The next value, `None` at the end of the input.
    pub fn next_value(&mut self) -> Option<Result<Val, ParseError>> {
        if self.failed {
            return None;
        }
        let r = self.value_top();
        match r {
            Ok(None) => None,
            Ok(Some(v)) => Some(Ok(v)),
            Err(e) => {
                self.failed = true;
                Some(Err(e))
            }
        }
    }

    fn io<T>(&self, r: io::Result<T>) -> Result<T, ParseError> {
        r.map_err(|e| ParseError(format!("{e}")))
    }

    fn value_top(&mut self) -> Result<Option<Val>, ParseError> {
        // What an error left of a value being read.
        self.items.clear();
        self.pairs.clear();
        // A byte order mark before a value is skipped, as jq does.
        if self.peek().ok().flatten() == Some(0xEF)
            && self.buf.get(self.pos..self.pos + 3) == Some(&[0xEF, 0xBB, 0xBF][..])
        {
            self.pos += 3;
        }
        let r = self.skip_ws();
        self.io(r)?;
        let p = self.peek();
        let Some(first) = self.io(p)? else {
            // At the end jq has read every line.
            self.lines_read = self.line - 1;
            return Ok(None);
        };
        let v = self.value(0)?;
        if !matches!(first, b'[' | b'{' | b'"') {
            // jq delivers a bare literal at the byte that ends it, and only once that byte
            // is taken: a stray `,` `:` `]` or `}` there fails, and the literal is lost.
            let p = self.peek();
            if let Some(b',' | b':' | b']' | b'}') = self.io(p)? {
                return self.value(0).map(Some);
            }
        }
        let r = self.note_line();
        self.io(r)?;
        Ok(Some(v))
    }

    fn unfinished<T>(&mut self) -> Result<T, ParseError> {
        self.eof_error("Unfinished JSON term")
    }

    fn value(&mut self, depth: usize) -> Result<Val, ParseError> {
        if depth > 10000 {
            return self.error("Exceeds depth limit for parsing");
        }
        let r = self.skip_ws();
        self.io(r)?;
        let p = self.peek();
        let Some(b) = self.io(p)? else {
            return self.unfinished();
        };
        match b {
            b'{' => {
                self.bump().ok();
                let r = self.skip_ws();
                self.io(r)?;
                let p = self.peek();
                if self.io(p)? == Some(b'}') {
                    self.bump().ok();
                    return Ok(Val::obj(Map::new()));
                }
                let start = self.pairs.len();
                loop {
                    let r = self.skip_ws();
                    self.io(r)?;
                    let p = self.peek();
                    match self.io(p)? {
                        None => return self.unfinished(),
                        Some(b'"') => {}
                        Some(b'}') => {
                            self.bump().ok();
                            return self.error("Expected another key-value pair");
                        }
                        Some(c) => {
                            // A key that is not a string: jq reads it, then complains (a
                            // literal ends at the delimiter after it).
                            self.value(depth + 1)?;
                            if !matches!(c, b'[' | b'{') {
                                self.col += 1;
                            }
                            return self.error("Object keys must be strings");
                        }
                    }
                    let k = self.key()?;
                    let r = self.skip_ws();
                    self.io(r)?;
                    let p = self.bump();
                    match self.io(p)? {
                        Some(b':') => {}
                        None => return self.unfinished(),
                        Some(b',' | b'}') => {
                            return self.error("Objects must consist of key:value pairs");
                        }
                        Some(b']') => return self.error("Unmatched ']'"),
                        Some(_) => {
                            self.unbump();
                            return self.separator_error();
                        }
                    }
                    let v = self.value(depth + 1)?;
                    self.pairs.push((k, v));
                    let r = self.skip_ws();
                    self.io(r)?;
                    let p = self.bump();
                    match self.io(p)? {
                        Some(b',') => {}
                        Some(b'}') => {
                            let mut m = Map::with_capacity(self.pairs.len() - start);
                            for (k, v) in self.pairs.drain(start..) {
                                m.insert(k, v);
                            }
                            return Ok(Val::obj(m));
                        }
                        None => return self.unfinished(),
                        // jq holds the key on its stack until the value is complete.
                        Some(b']') => return self.error("Unmatched ']'"),
                        Some(b':') => return self.error("':' not as part of an object"),
                        Some(_) => {
                            self.unbump();
                            return self.separator_error();
                        }
                    }
                }
            }
            b'[' => {
                self.bump().ok();
                let r = self.skip_ws();
                self.io(r)?;
                let p = self.peek();
                if self.io(p)? == Some(b']') {
                    self.bump().ok();
                    return Ok(Val::arr(Vec::new()));
                }
                let start = self.items.len();
                loop {
                    let r = self.skip_ws();
                    self.io(r)?;
                    let p = self.peek();
                    match self.io(p)? {
                        None => return self.unfinished(),
                        Some(b']') => {
                            self.bump().ok();
                            return self.error("Expected another array element");
                        }
                        _ => {}
                    }
                    let v = self.value(depth + 1)?;
                    self.items.push(v);
                    let r = self.skip_ws();
                    self.io(r)?;
                    let p = self.bump();
                    match self.io(p)? {
                        Some(b',') => {}
                        Some(b']') => return Ok(Val::arr(self.items.drain(start..).collect())),
                        None => return self.unfinished(),
                        Some(b'}') => return self.error("Objects must consist of key:value pairs"),
                        Some(b':') => return self.error("':' not as part of an object"),
                        Some(_) => {
                            self.unbump();
                            return self.separator_error();
                        }
                    }
                }
            }
            b'"' => Ok(Val::Str(self.string()?)),
            b']' => {
                self.bump().ok();
                self.error("Unmatched ']'")
            }
            b'}' => {
                self.bump().ok();
                self.error("Unmatched '}'")
            }
            b',' => {
                self.bump().ok();
                self.error("Expected value before ','")
            }
            b':' => {
                self.bump().ok();
                self.error("Expected string key before ':'")
            }
            _ => self.literal(),
        }
    }

    /// A value where a separator belongs: jq complains once the value is complete (at
    /// the delimiter after a literal), or at once for an opening bracket.
    fn separator_error<T>(&mut self) -> Result<T, ParseError> {
        const MSG: &str = "Expected separator between values";
        match self.peek().ok().flatten() {
            Some(b'[' | b'{') => {
                self.bump().ok();
                self.error(MSG)
            }
            Some(b'"') => {
                self.string()?;
                self.error(MSG)
            }
            _ => {
                self.literal()?;
                self.col += 1;
                self.error(MSG)
            }
        }
    }

    /// A bare token: `true`, `false`, `null`, `nan`, a number.
    fn literal(&mut self) -> Result<Val, ParseError> {
        let rest = &self.buf[self.pos..];
        if let Some(n) = rest.iter().position(|&b| {
            matches!(
                b,
                b' ' | b'\t' | b'\n' | b'\r' | b'[' | b']' | b'{' | b'}' | b',' | b':' | b'"'
            )
        }) && rest[..n].is_ascii()
            && let Ok(v) = self.check_literal(&rest[..n])
        {
            self.pos += n;
            self.col += n;
            return Ok(v);
        }
        let mut tok = Vec::new();
        loop {
            let p = self.peek();
            match self.io(p)? {
                Some(b)
                    if !matches!(
                        b,
                        b' ' | b'\t'
                            | b'\n'
                            | b'\r'
                            | b'['
                            | b']'
                            | b'{'
                            | b'}'
                            | b','
                            | b':'
                            | b'"'
                    ) =>
                {
                    tok.push(b);
                    self.bump().ok();
                }
                Some(_) => {
                    // The delimiter ends the token; jq checks it there.
                    let p = self.bump();
                    self.io(p)?;
                    let v = self.check_literal(&tok);
                    // Give the delimiter back unless it was whitespace.
                    self.unbump();
                    return v;
                }
                None => {
                    return match self.check_literal(&tok) {
                        Ok(v) => Ok(v),
                        Err(ParseError(m)) => {
                            let m = m.split(" at line").next().unwrap_or("").to_string();
                            self.eof_error(&m)
                        }
                    };
                }
            }
        }
    }

    /// Steps back over the byte just read (a delimiter after a literal).
    fn unbump(&mut self) {
        self.pos -= 1;
        if self.buf[self.pos] == b'\n' {
            self.line -= 1;
            // The column is not needed again before the next newline is read.
            self.col = usize::MAX / 2;
        } else {
            self.col -= 1;
        }
    }

    fn check_literal(&self, tok: &[u8]) -> Result<Val, ParseError> {
        let t = String::from_utf8_lossy(tok);
        match &*t {
            "true" => return Ok(Val::Bool(true)),
            "false" => return Ok(Val::Bool(false)),
            "null" => return Ok(Val::Null),
            "nan" | "NaN" | "-nan" | "-NaN" => return Ok(Val::num(f64::NAN)),
            "Infinity" | "infinity" | "+Infinity" | "+infinity" => {
                return Ok(Val::num(f64::INFINITY));
            }
            "-Infinity" | "-infinity" => return Ok(Val::num(f64::NEG_INFINITY)),
            _ => {}
        }
        // jq matches a token against `true`, `false` or `null` by its first bytes (`nu`
        // for null, as a lone `n` may start `nan`); anything else is read as a number.
        if matches!(tok.first(), Some(b't' | b'f')) || tok.starts_with(b"nu") {
            return self.error("Invalid literal");
        }
        match Num::literal(&t) {
            Some(n) => Ok(Val::Num(n)),
            None => self.error("Invalid numeric literal"),
        }
    }

    fn hex4(&mut self) -> Result<Option<u32>, ParseError> {
        let mut n = 0u32;
        for _ in 0..4 {
            let p = self.peek();
            match self.io(p)? {
                Some(c) if c.is_ascii_hexdigit() => {
                    self.bump().ok();
                    n = n * 16 + (c as char).to_digit(16).unwrap_or(0);
                }
                _ => return Ok(None),
            }
        }
        Ok(Some(n))
    }

    fn key(&mut self) -> Result<Rc<str>, ParseError> {
        let rest = &self.buf[self.pos..];
        if let Some(n) = rest
            .get(1..)
            .and_then(|r| r.iter().position(|&c| c == b'"' || c == b'\\' || c < 0x20))
            && rest[1 + n] == b'"'
            && let raw = &rest[1..1 + n]
            && let Ok(text) = std::str::from_utf8(raw)
        {
            let slot = (raw.len()
                ^ raw.first().map_or(0, |&b| usize::from(b) << 3)
                ^ raw.last().map_or(0, |&b| usize::from(b) << 5))
                & 255;
            let k = match &self.keys[slot] {
                Some(k) if k.as_bytes() == raw => k.clone(),
                _ => {
                    let k: Rc<str> = Rc::from(text);
                    self.keys[slot] = Some(k.clone());
                    k
                }
            };
            self.col += 2 + raw.iter().filter(|&&c| c & 0xC0 != 0x80).count();
            self.pos += n + 2;
            return Ok(k);
        }
        self.string()
    }

    fn string(&mut self) -> Result<Rc<str>, ParseError> {
        // A string without escapes whose end is in the buffer is made straight from it.
        let rest = &self.buf[self.pos..];
        if let Some(n) = rest
            .get(1..)
            .and_then(|r| r.iter().position(|&c| c == b'"' || c == b'\\' || c < 0x20))
            && rest[1 + n] == b'"'
            && let raw = &rest[1..1 + n]
            && let Ok(text) = std::str::from_utf8(raw)
        {
            let s: Rc<str> = Rc::from(text);
            self.col += 2 + raw.iter().filter(|&&c| c & 0xC0 != 0x80).count();
            self.pos += n + 2;
            return Ok(s);
        }
        self.bump().ok(); // the quote
        let mut out: Vec<u8> = Vec::new();
        // jq complains about a bad escape or a raw control character once the string is
        // complete, at its closing quote.
        let mut bad: Option<&'static str> = None;
        loop {
            let rest = &self.buf[self.pos..];
            let n = rest
                .iter()
                .position(|&c| c == b'"' || c == b'\\' || c < 0x20)
                .unwrap_or(rest.len());
            if n > 0 {
                out.extend_from_slice(&rest[..n]);
                self.col += rest[..n].iter().filter(|&&c| c & 0xC0 != 0x80).count();
                self.pos += n;
            }
            let p = self.bump();
            let Some(b) = self.io(p)? else {
                return self.eof_error("Unfinished string");
            };
            match b {
                b'"' if bad.is_some() => return self.error(bad.unwrap_or_default()),
                b'"' => break,
                b'\\' => {
                    let p = self.bump();
                    let Some(e) = self.io(p)? else {
                        return self.unfinished();
                    };
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let Some(hi) = self.hex4()? else {
                                bad.get_or_insert("Invalid \\uXXXX escape");
                                continue;
                            };
                            let c = if (0xD800..0xDC00).contains(&hi) {
                                let ok = self.peek().ok().flatten() == Some(b'\\');
                                let lo = if ok {
                                    self.bump().ok();
                                    if self.peek().ok().flatten() == Some(b'u') {
                                        self.bump().ok();
                                        self.hex4()?
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                };
                                match lo {
                                    Some(lo) if (0xDC00..0xE000).contains(&lo) => char::from_u32(
                                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00),
                                    ),
                                    _ => {
                                        bad.get_or_insert(
                                            "Invalid \\uXXXX\\uXXXX surrogate pair escape",
                                        );
                                        continue;
                                    }
                                }
                            } else if (0xDC00..0xE000).contains(&hi) {
                                Some('\u{FFFD}')
                            } else {
                                char::from_u32(hi)
                            };
                            let c = c.unwrap_or('\u{FFFD}');
                            let mut tmp = [0u8; 4];
                            out.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
                        }
                        _ => {
                            bad.get_or_insert("Invalid escape");
                        }
                    }
                }
                c if c < 0x20 => {
                    bad.get_or_insert(
                        "Invalid string: control characters from U+0000 through U+001F must be escaped",
                    );
                }
                c => out.push(c),
            }
        }
        Ok(match String::from_utf8(out) {
            Ok(s) => s.into(),
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned().into(),
        })
    }
}

/// Parses a whole text as values (`fromjson`, `--argjson`, `--slurpfile`).
pub fn parse_all(text: &[u8]) -> Result<Vec<Val>, ParseError> {
    let mut r = Reader::new(text);
    let mut out = Vec::new();
    while let Some(v) = r.next_value() {
        out.push(v?);
    }
    Ok(out)
}

/// How values are printed.
#[derive(Clone, Debug)]
pub struct Style {
    /// `None`: compact (`-c`).
    pub indent: Option<Indent>,
    pub sort_keys: bool,
    pub ascii: bool,
    pub color: Option<Colors>,
}

#[derive(Clone, Copy, Debug)]
pub enum Indent {
    Spaces(usize),
    Tab,
}

/// ANSI sequences per kind, as `JQ_COLORS` orders them.
#[derive(Clone, Debug)]
pub struct Colors {
    pub null: String,
    pub r#false: String,
    pub r#true: String,
    pub number: String,
    pub string: String,
    pub array: String,
    pub object: String,
    pub key: String,
}

impl Colors {
    /// jq 1.7.1's defaults, changed by `JQ_COLORS` (`null:false:true:numbers:strings:
    /// arrays:objects:keys`).
    pub fn new(spec: Option<&str>) -> Colors {
        let mut c = Colors {
            null: "0;90".into(),
            r#false: "0;39".into(),
            r#true: "0;39".into(),
            number: "0;39".into(),
            string: "0;32".into(),
            array: "1;39".into(),
            object: "1;39".into(),
            key: "1;34".into(),
        };
        if let Some(spec) = spec {
            let fields = [
                &mut c.null,
                &mut c.r#false,
                &mut c.r#true,
                &mut c.number,
                &mut c.string,
                &mut c.array,
                &mut c.object,
                &mut c.key,
            ];
            for (slot, part) in fields.into_iter().zip(spec.split(':')) {
                if part.bytes().all(|b| b.is_ascii_digit() || b == b';') {
                    *slot = part.to_string();
                }
            }
        }
        c
    }
}

impl Style {
    pub fn compact() -> Style {
        Style {
            indent: None,
            sort_keys: false,
            ascii: false,
            color: None,
        }
    }
}

/// Compact JSON with jq's escapes (`tojson`, messages).
pub fn write_compact(out: &mut Vec<u8>, v: &Val) {
    write(out, v, &Style::compact(), 0);
}

pub fn write_string(out: &mut Vec<u8>, s: &str, ascii: bool) {
    out.push(b'"');
    for c in s.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes());
            }
            c if ascii && !c.is_ascii() => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.extend_from_slice(format!("\\u{u:04x}").as_bytes());
                }
            }
            c => {
                let mut tmp = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
            }
        }
    }
    out.push(b'"');
}

fn newline(out: &mut Vec<u8>, style: &Style, level: usize) {
    if let Some(ind) = style.indent {
        out.push(b'\n');
        match ind {
            Indent::Spaces(n) => out.extend(std::iter::repeat_n(b' ', n * level)),
            Indent::Tab => out.extend(std::iter::repeat_n(b'\t', level)),
        }
    }
}

fn col(out: &mut Vec<u8>, c: &str) {
    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(c.as_bytes());
    out.push(b'm');
}

const RESET: &[u8] = b"\x1b[0m";

/// One value as jq prints it.
pub fn write(out: &mut Vec<u8>, v: &Val, style: &Style, level: usize) {
    let colors = style.color.as_ref();
    let scalar = |out: &mut Vec<u8>, c: Option<&String>, text: &dyn Fn(&mut Vec<u8>)| {
        if let Some(c) = c {
            col(out, c);
            text(out);
            out.extend_from_slice(RESET);
        } else {
            text(out);
        }
    };
    match v {
        Val::Null => scalar(out, colors.map(|c| &c.null), &|o| {
            o.extend_from_slice(b"null")
        }),
        Val::Bool(true) => scalar(out, colors.map(|c| &c.r#true), &|o| {
            o.extend_from_slice(b"true")
        }),
        Val::Bool(false) => scalar(out, colors.map(|c| &c.r#false), &|o| {
            o.extend_from_slice(b"false")
        }),
        Val::Num(n) => scalar(out, colors.map(|c| &c.number), &|o| {
            if n.lit().is_none() && n.f.is_nan() {
                o.extend_from_slice(b"null");
            } else {
                n.write(o)
            }
        }),
        Val::Str(s) => scalar(out, colors.map(|c| &c.string), &|o| {
            write_string(o, s, style.ascii)
        }),
        Val::Arr(a) => {
            let c = colors.map(|c| c.array.as_str());
            if let Some(c) = c {
                col(out, c);
            }
            if a.is_empty() {
                out.extend_from_slice(b"[]");
            } else {
                out.push(b'[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        if let Some(c) = c {
                            col(out, c);
                        }
                        out.push(b',');
                    }
                    newline(out, style, level + 1);
                    write(out, x, style, level + 1);
                }
                if let Some(c) = c {
                    col(out, c);
                }
                newline(out, style, level);
                if let Some(c) = c {
                    col(out, c);
                }
                out.push(b']');
            }
            if c.is_some() {
                out.extend_from_slice(RESET);
            }
        }
        Val::Obj(m) => {
            let c = colors.map(|c| c.object.as_str());
            if let Some(c) = c {
                col(out, c);
            }
            if m.is_empty() {
                out.extend_from_slice(b"{}");
            } else {
                out.push(b'{');
                let entries: Vec<(&Rc<str>, &Val)> = if style.sort_keys {
                    m.sorted()
                } else {
                    m.iter().collect()
                };
                for (i, (k, x)) in entries.into_iter().enumerate() {
                    if i > 0 {
                        if let Some(c) = c {
                            col(out, c);
                        }
                        out.push(b',');
                    }
                    newline(out, style, level + 1);
                    if let Some(cs) = colors {
                        out.extend_from_slice(RESET);
                        col(out, &cs.key);
                        write_string(out, k, style.ascii);
                        out.extend_from_slice(RESET);
                        col(out, &cs.object);
                        out.push(b':');
                        if style.indent.is_some() {
                            out.push(b' ');
                        }
                        out.extend_from_slice(RESET);
                    } else {
                        write_string(out, k, style.ascii);
                        out.push(b':');
                        if style.indent.is_some() {
                            out.push(b' ');
                        }
                    }
                    write(out, x, style, level + 1);
                }
                if let Some(c) = c {
                    col(out, c);
                }
                newline(out, style, level);
                if let Some(c) = c {
                    col(out, c);
                }
                out.push(b'}');
            }
            if c.is_some() {
                out.extend_from_slice(RESET);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ParseError, Reader};

    /// The values read before the input ends, compact, and the parse error ending it.
    fn read(text: &str) -> (Vec<String>, Option<String>) {
        let mut r = Reader::new(text.as_bytes());
        let mut vals = Vec::new();
        while let Some(v) = r.next_value() {
            match v {
                Ok(v) => vals.push(v.to_json()),
                Err(ParseError(m)) => return (vals, Some(m)),
            }
        }
        (vals, None)
    }

    fn check(text: &str, vals: &[&str], err: &str) {
        let (got, e) = read(text);
        let want = vals.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        assert_eq!((got, e.as_deref()), (want, Some(err)), "{text:?}");
    }

    #[test]
    fn literal_ended_by_stray_delimiter_is_lost() {
        check(
            "3,1,2,",
            &[],
            "Expected value before ',' at line 1, column 2",
        );
        check(
            "1: x\n",
            &[],
            "Expected string key before ':' at line 1, column 2",
        );
        check("1]", &[], "Unmatched ']' at line 1, column 2");
        check("1}", &[], "Unmatched '}' at line 1, column 2");
        check(
            "1 2 3,",
            &["1", "2"],
            "Expected value before ',' at line 1, column 6",
        );
        check(
            "1 ,",
            &["1"],
            "Expected value before ',' at line 1, column 3",
        );
        check("1\n]", &["1"], "Unmatched ']' at line 2, column 1");
        check(
            "1[",
            &["1"],
            "Unfinished JSON term at EOF at line 1, column 2",
        );
    }

    #[test]
    fn nested_values_and_strings_read_whole() {
        let (vals, err) = read(
            "[\"a\",\"é\\u00e9\",[[],{}],{\"k\":[1,{\"k\":\"v\\n\"}],\"j\":\"\"}] \"ü\" {\"a\":1,\"a\":2}",
        );
        assert_eq!(err, None);
        assert_eq!(
            vals,
            [
                "[\"a\",\"éé\",[[],{}],{\"k\":[1,{\"k\":\"v\\n\"}],\"j\":\"\"}]",
                "\"ü\"",
                "{\"a\":2}",
            ]
        );
        // The column counts characters, past a string read whole as past one with escapes.
        check("[\"éé\" 1]", &[], "Expected separator between values at line 1, column 8");
        check("[\"\\u00e9\" 1]", &[], "Expected separator between values at line 1, column 12");
    }

    #[test]
    fn values_after_an_error_inside_collections_stand_alone() {
        let mut r = Reader::new(&b"[1,[2,{\"a\":3,]]] [4] {\"b\":[5]}"[..]);
        assert!(matches!(r.next_value(), Some(Err(_))));
        let rest: Vec<_> = std::iter::from_fn(|| r.next_value())
            .filter_map(|v| v.ok().map(|v| v.to_json()))
            .collect();
        assert!(rest.iter().all(|v| !v.contains('2') && !v.contains('3')), "{rest:?}");
    }

    #[test]
    fn colon_without_key() {
        check(
            "[1]:",
            &["[1]"],
            "Expected string key before ':' at line 1, column 4",
        );
        check(
            "\"a\":1",
            &["\"a\""],
            "Expected string key before ':' at line 1, column 4",
        );
        check(
            "[:]",
            &[],
            "Expected string key before ':' at line 1, column 2",
        );
        check(
            "{:1}",
            &[],
            "Expected string key before ':' at line 1, column 2",
        );
        check(
            "[1:2]",
            &[],
            "':' not as part of an object at line 1, column 3",
        );
        check(
            r#"{"a":1:}"#,
            &[],
            "':' not as part of an object at line 1, column 7",
        );
    }

    #[test]
    fn mismatched_closers() {
        check(
            r#"{"a"}"#,
            &[],
            "Objects must consist of key:value pairs at line 1, column 5",
        );
        check(
            r#"{"a",1}"#,
            &[],
            "Objects must consist of key:value pairs at line 1, column 5",
        );
        check(
            "[1}",
            &[],
            "Objects must consist of key:value pairs at line 1, column 3",
        );
        check(r#"{"a":1]"#, &[], "Unmatched ']' at line 1, column 7");
    }

    #[test]
    fn literal_kinds() {
        check("n ", &[], "Invalid numeric literal at line 1, column 2");
        check("1 nul ", &["1"], "Invalid literal at line 1, column 6");
        check("tru,", &[], "Invalid literal at line 1, column 4");
    }
}
