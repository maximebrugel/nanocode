//! `wc` with GNU's output widths (D016): `-l -w -c -m -L`, `--files0-from` falls
//! through. Written for psh after GNU's documented behavior.

use psh_engine::host::FileKind;
use psh_engine::util::UtilCtx;

use crate::common::{self, Input};
use crate::opts::{Spec, both, long};

pub const SPEC: Spec = Spec {
    name: "wc",
    opts: &[
        both('c', "bytes"),
        both('m', "chars"),
        both('l', "lines"),
        both('w', "words"),
        both('L', "max-line-length"),
        long("total").optional(),
    ],
    posix_order: false,
};

#[derive(Clone, Copy, Default)]
struct Counts {
    lines: u64,
    words: u64,
    chars: u64,
    bytes: u64,
    max_line: u64,
}

impl Counts {
    fn add(&mut self, o: &Counts) {
        self.lines += o.lines;
        self.words += o.words;
        self.chars += o.chars;
        self.bytes += o.bytes;
        self.max_line = self.max_line.max(o.max_line);
    }
}

/// Counts as the input streams in: a word or a line may cross chunks.
#[derive(Default)]
struct Counter {
    c: Counts,
    in_word: bool,
    line_width: u64,
    /// Bytes of an incomplete UTF-8 sequence at the end of the last chunk.
    pending: Vec<u8>,
}

impl Counter {
    /// `feed` when only lines and bytes are asked for (`wc -l`, `wc -c`): a newline is one
    /// byte in every encoding `feed` reads, so counting that byte gives the same lines
    /// without decoding a character at a time (which was 31% of `… | wc -l` in wasm).
    fn feed_lines(&mut self, data: &[u8]) {
        self.c.bytes += data.len() as u64;
        self.c.lines += data.iter().filter(|&&b| b == b'\n').count() as u64;
    }

    fn feed(&mut self, data: &[u8], utf8: bool) {
        self.c.bytes += data.len() as u64;
        let mut buf;
        let data = if self.pending.is_empty() {
            data
        } else {
            buf = std::mem::take(&mut self.pending);
            buf.extend_from_slice(data);
            &buf[..]
        };
        let mut i = 0;
        while i < data.len() {
            let b = data[i];
            if b < 0x80 {
                i += 1;
                self.c.chars += 1;
                match b {
                    b'\n' => {
                        self.c.lines += 1;
                        self.c.max_line = self.c.max_line.max(self.line_width);
                        self.line_width = 0;
                        self.in_word = false;
                    }
                    b' ' => {
                        self.line_width += 1;
                        self.in_word = false;
                    }
                    0x0b => self.in_word = false,
                    b'\t' => {
                        self.line_width = (self.line_width / 8 + 1) * 8;
                        self.in_word = false;
                    }
                    b'\r' | 0x0c => {
                        self.c.max_line = self.c.max_line.max(self.line_width);
                        self.line_width = 0;
                        self.in_word = false;
                    }
                    0..=0x1f | 0x7f => {}
                    _ => {
                        self.line_width += 1;
                        if !self.in_word {
                            self.in_word = true;
                            self.c.words += 1;
                        }
                    }
                }
                continue;
            }
            if !utf8 {
                // A byte past ASCII outside a UTF-8 locale is a character that is not
                // printable: it has no width and neither starts nor ends a word (GNU wc).
                i += 1;
                self.c.chars += 1;
                continue;
            }
            let (ch, len) = {
                let len = match b {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => 1,
                };
                if i + len > data.len() {
                    // Complete it with the next chunk.
                    self.pending = data[i..].to_vec();
                    break;
                }
                match std::str::from_utf8(&data[i..i + len]) {
                    Ok(s) => (s.chars().next(), len),
                    Err(_) => (None, 1),
                }
            };
            i += len;
            if let Some(ch) = ch {
                self.c.chars += 1;
                let space = matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c');
                match ch {
                    '\n' => {
                        self.c.lines += 1;
                        self.c.max_line = self.c.max_line.max(self.line_width);
                        self.line_width = 0;
                    }
                    '\t' => self.line_width = (self.line_width / 8 + 1) * 8,
                    '\r' | '\x0c' => {
                        self.c.max_line = self.c.max_line.max(self.line_width);
                        self.line_width = 0;
                    }
                    _ if ch.is_control() => {}
                    _ => self.line_width += 1,
                }
                if space {
                    self.in_word = false;
                } else if !self.in_word && !ch.is_control() {
                    self.in_word = true;
                    self.c.words += 1;
                }
            }
        }
    }

    fn finish(mut self) -> Counts {
        // An incomplete sequence at the end is bytes, not characters.
        self.c.max_line = self.c.max_line.max(self.line_width);
        self.c
    }
}

pub async fn run(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let p = match common::parse(ctx, &SPEC, argv).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    let (mut l, mut w, mut c, mut m, mut big_l) = (false, false, false, false, false);
    let mut total_mode = "auto".to_string();
    for (key, value) in p.iter(&SPEC) {
        match key.as_str() {
            "lines" => l = true,
            "words" => w = true,
            "bytes" => c = true,
            "chars" => m = true,
            "max-line-length" => big_l = true,
            "total" => {
                total_mode = String::from_utf8_lossy(value.unwrap_or(b"always")).into_owned();
                if !matches!(total_mode.as_str(), "auto" | "always" | "only" | "never") {
                    let msg = format!(
                        "invalid argument {} for {}",
                        common::quote(ctx, total_mode.as_bytes()),
                        common::quote(ctx, b"--total")
                    );
                    return common::usage(ctx, &msg).await;
                }
            }
            _ => {}
        }
    }
    if !(l || w || c || m || big_l) {
        (l, w, c) = (true, true, true);
    }
    let utf8 = ctx.utf8();
    let lines_only = !(w || m || big_l);
    let files = common::operands_or_stdin(&p);
    let ncounts = [l, w, m, c, big_l].iter().filter(|&&b| b).count();
    // GNU stats every input before reading any, for the width: the digits of the total
    // size of regular files, at least 7 when an input is not a regular file, 1 when the
    // first input can't be stat'd, and no padding for one count of one input.
    let width = if (ncounts == 1 && files.len() == 1) || total_mode == "only" {
        1
    } else {
        let mut total_size: u64 = 0;
        let mut irregular = false;
        let mut first_failed = false;
        for (i, name) in files.iter().enumerate() {
            if name == b"-" {
                irregular = true;
                continue;
            }
            match ctx.stat(name, true).await {
                Ok(md) if md.kind == FileKind::File => total_size += md.size,
                Ok(_) => irregular = true,
                Err(_) => first_failed |= i == 0,
            }
        }
        if first_failed {
            1
        } else {
            let digits = total_size.max(1).to_string().len();
            if irregular { digits.max(7) } else { digits }
        }
    };
    let line = |counts: &Counts, name: &[u8], out: &mut Vec<u8>| {
        let mut fields = Vec::new();
        for (on, v) in [
            (l, counts.lines),
            (w, counts.words),
            (m, counts.chars),
            (c, counts.bytes),
            (big_l, counts.max_line),
        ] {
            if on {
                fields.push(format!("{v:>width$}"));
            }
        }
        out.extend_from_slice(fields.join(" ").as_bytes());
        if !name.is_empty() {
            out.push(b' ');
            out.extend_from_slice(name);
        }
        out.push(b'\n');
    };
    let mut status = 0;
    let mut total = Counts::default();
    let mut buf = vec![0u8; 64 * 1024];
    // Each input's line is written as soon as it is counted, so its errors on stderr
    // interleave with the counts in input order, as GNU's do.
    for name in &files {
        let input = match Input::open(ctx, name).await {
            Ok(i) => i,
            Err(e) => {
                common::file_error(ctx, name, &e).await;
                status = 1;
                continue;
            }
        };
        let mut counter = Counter::default();
        let mut failed = false;
        loop {
            match input.read(ctx, &mut buf).await {
                Ok(0) => break,
                Ok(n) if lines_only => counter.feed_lines(&buf[..n]),
                Ok(n) => counter.feed(&buf[..n], utf8),
                Err(e) => {
                    common::file_error(ctx, name, &e).await;
                    status = 1;
                    failed = true;
                    break;
                }
            }
            ctx.check().await;
        }
        if failed {
            continue;
        }
        let counts = counter.finish();
        total.add(&counts);
        if total_mode != "only" {
            let shown: &[u8] = if p.operands.is_empty() { b"" } else { name };
            let mut out = Vec::new();
            line(&counts, shown, &mut out);
            if let Err(e) = ctx.write(&out).await {
                return common::write_failed(ctx, &e).await;
            }
        }
    }
    let show_total = match total_mode.as_str() {
        "always" | "only" => true,
        "never" => false,
        _ => files.len() > 1,
    };
    if show_total {
        let mut out = Vec::new();
        line(
            &total,
            if total_mode == "only" { b"" } else { b"total" },
            &mut out,
        );
        if let Err(e) = ctx.write(&out).await {
            return common::write_failed(ctx, &e).await;
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::Counter;

    fn words(data: &[u8], utf8: bool) -> u64 {
        let mut c = Counter::default();
        c.feed(data, utf8);
        c.finish().words
    }

    #[test]
    fn bytes_past_ascii_start_no_word_outside_utf8() {
        // GNU wc in the C locale: they are not printable, so they neither start nor end
        // a word.
        assert_eq!(words(b"\xc3\xa9\n", false), 0);
        assert_eq!(words(b"t\xc3\xa9\n", false), 1);
        assert_eq!(words(b"\xa9a \xa9 b\n", false), 2);
        assert_eq!(words("é\n".as_bytes(), true), 1);
    }
}
