//! Line and field tools (nanocode's addition), after GNU coreutils and util-linux: tac,
//! rev, fold, expand, unexpand, column, join, shuf, split, numfmt, tsort and fmt.
//! Inputs are read whole, except by fold, which writes each block as it reads.

use psh_engine::host::{OpenOptions, error_text};
use psh_engine::util::UtilCtx;

use crate::common::{self, Input};
use crate::opts::{Spec, both, flag, long};

async fn read_all(ctx: &mut UtilCtx<'_>, names: &[Vec<u8>]) -> Result<Vec<u8>, i32> {
    let mut data = Vec::new();
    let stdin = [b"-".to_vec()];
    let names = if names.is_empty() { &stdin[..] } else { names };
    let mut status = 0;
    for name in names {
        let read = match Input::open(ctx, name).await {
            Ok(input) => input.read_to_end(ctx, &mut data).await,
            Err(e) => Err(e),
        };
        if let Err(e) = read {
            common::file_error(ctx, name, &e).await;
            status = 1;
        }
    }
    if status != 0 && data.is_empty() {
        return Err(status);
    }
    Ok(data)
}

async fn emit(ctx: &mut UtilCtx<'_>, out: &[u8]) -> i32 {
    match ctx.write(out).await {
        Ok(()) => 0,
        Err(e) => common::write_failed(ctx, &e).await,
    }
}

fn count(text: &[u8]) -> Option<usize> {
    std::str::from_utf8(text).ok()?.parse().ok()
}

fn char_len(first: u8) -> usize {
    match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

pub const TAC: Spec = Spec {
    name: "tac",
    opts: &[both('b', "before"), both('s', "separator").value()],
    posix_order: false,
};

pub async fn tac(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let p = match common::parse(ctx, &TAC, argv).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    let before = p.has(&TAC, "before");
    let sep = p.last(&TAC, "separator").unwrap_or(b"\n").to_vec();
    if sep.is_empty() {
        return common::usage(ctx, "separator cannot be empty").await;
    }
    let mut out = Vec::new();
    let mut status = 0;
    for name in common::operands_or_stdin(&p) {
        let data = match read_all(ctx, std::slice::from_ref(&name)).await {
            Ok(data) => data,
            Err(st) => {
                status = st;
                continue;
            }
        };
        // Where each record starts: after a separator, or at one with -b.
        let mut starts = vec![0];
        let mut at = 0;
        let first = sep[0];
        while let Some(found) = data[at..].iter().position(|&b| b == first) {
            at += found;
            if data[at..].starts_with(&sep) {
                let cut = if before { at } else { at + sep.len() };
                if cut != 0 && cut != data.len() {
                    starts.push(cut);
                }
                at += sep.len();
            } else {
                at += 1;
            }
        }
        starts.dedup();
        for (n, &start) in starts.iter().enumerate().rev() {
            let end = starts.get(n + 1).copied().unwrap_or(data.len());
            out.extend_from_slice(&data[start..end]);
        }
    }
    emit(ctx, &out).await.max(status)
}

pub async fn rev(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let files: Vec<Vec<u8>> = argv[1..].iter().filter(|a| *a != b"--").cloned().collect();
    let data = match read_all(ctx, &files).await {
        Ok(data) => data,
        Err(st) => return st,
    };
    let mut out = Vec::with_capacity(data.len());
    for line in data.split_inclusive(|&b| b == b'\n') {
        let (body, end) = match line.strip_suffix(b"\n") {
            Some(body) => (body, &b"\n"[..]),
            None => (line, &b""[..]),
        };
        let mut chars: Vec<&[u8]> = Vec::new();
        let mut at = 0;
        while at < body.len() {
            let len = char_len(body[at]).min(body.len() - at);
            chars.push(&body[at..at + len]);
            at += len;
        }
        for c in chars.iter().rev() {
            out.extend_from_slice(c);
        }
        out.extend_from_slice(end);
    }
    emit(ctx, &out).await
}

pub const FOLD: Spec = Spec {
    name: "fold",
    opts: &[
        both('b', "bytes"),
        both('s', "spaces"),
        both('w', "width").value(),
    ],
    posix_order: false,
};

pub async fn fold(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    // fold -80 is fold -w 80.
    let mut args = argv.to_vec();
    for arg in args.iter_mut().skip(1) {
        if arg.len() > 1 && arg[0] == b'-' && arg[1..].iter().all(u8::is_ascii_digit) {
            let mut wide = b"-w".to_vec();
            wide.extend_from_slice(&arg[1..]);
            *arg = wide;
        }
    }
    let p = match common::parse(ctx, &FOLD, &args).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    let bytes = p.has(&FOLD, "bytes");
    let spaces = p.has(&FOLD, "spaces");
    let width = match p.last(&FOLD, "width") {
        Some(w) => match count(w) {
            Some(n) if n > 0 => n,
            _ => {
                let w = common::quote(ctx, w);
                return common::usage(ctx, &format!("invalid number of columns: {w}")).await;
            }
        },
        None => 80,
    };
    let advance = |column: usize, b: u8| -> usize {
        if bytes {
            return column + 1;
        }
        match b {
            8 => column.saturating_sub(1),
            b'\r' => 0,
            b'\t' => column + 8 - column % 8,
            _ => column + 1,
        }
    };
    let stdin = [b"-".to_vec()];
    let names = if p.operands.is_empty() {
        &stdin[..]
    } else {
        &p.operands[..]
    };
    let mut status = 0;
    let mut buf = vec![0u8; 64 * 1024];
    let mut out: Vec<u8> = Vec::new();
    let mut line: Vec<u8> = Vec::new();
    let mut column = 0;
    for name in names {
        let input = match Input::open(ctx, name).await {
            Ok(input) => input,
            Err(e) => {
                common::file_error(ctx, name, &e).await;
                status = 1;
                continue;
            }
        };
        loop {
            let n = match input.read(ctx, &mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    common::file_error(ctx, name, &e).await;
                    status = 1;
                    break;
                }
            };
            for &b in &buf[..n] {
                if b == b'\n' {
                    out.extend_from_slice(&line);
                    out.push(b'\n');
                    line.clear();
                    column = 0;
                    continue;
                }
                loop {
                    let next = advance(column, b);
                    if next <= width || line.is_empty() {
                        column = next;
                        line.push(b);
                        break;
                    }
                    let blank = if spaces {
                        line.iter().rposition(|c| c.is_ascii_whitespace())
                    } else {
                        None
                    };
                    match blank {
                        Some(at) => {
                            out.extend_from_slice(&line[..=at]);
                            out.push(b'\n');
                            let rest = line.split_off(at + 1);
                            line = rest;
                            column = line.iter().fold(0, |c, &b| advance(c, b));
                        }
                        None => {
                            out.extend_from_slice(&line);
                            out.push(b'\n');
                            line.clear();
                            column = 0;
                        }
                    }
                }
            }
            if !out.is_empty() {
                let st = emit(ctx, &out).await;
                if st != 0 {
                    return st;
                }
                out.clear();
            }
        }
    }
    out.extend_from_slice(&line);
    match emit(ctx, &out).await {
        0 => status,
        st => st,
    }
}

fn tab_stops(spec: Option<&[u8]>) -> Option<(Vec<usize>, usize)> {
    let Some(spec) = spec else {
        return Some((Vec::new(), 8));
    };
    let parts: Vec<&[u8]> = spec
        .split(|&b| b == b',' || b == b' ')
        .filter(|p| !p.is_empty())
        .collect();
    let mut stops = Vec::new();
    for part in &parts {
        stops.push(count(part)?);
    }
    match stops.as_slice() {
        [] => Some((Vec::new(), 8)),
        [one] if *one > 0 => Some((Vec::new(), *one)),
        [_] => None,
        _ => Some((stops, 0)),
    }
}

fn next_stop(stops: &[usize], every: usize, column: usize) -> Option<usize> {
    if every > 0 {
        return Some(column + every - column % every);
    }
    stops.iter().copied().find(|&s| s > column)
}

pub const EXPAND: Spec = Spec {
    name: "expand",
    opts: &[both('i', "initial"), both('t', "tabs").value()],
    posix_order: false,
};

pub async fn expand(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let p = match common::parse(ctx, &EXPAND, argv).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    let Some((stops, every)) = tab_stops(p.last(&EXPAND, "tabs")) else {
        return common::usage(ctx, "tab size cannot be 0").await;
    };
    let initial = p.has(&EXPAND, "initial");
    let data = match read_all(ctx, &p.operands).await {
        Ok(data) => data,
        Err(st) => return st,
    };
    let mut out = Vec::with_capacity(data.len());
    let mut column = 0;
    let mut leading = true;
    for &b in &data {
        match b {
            b'\n' => {
                out.push(b);
                column = 0;
                leading = true;
            }
            b'\t' if leading || !initial => {
                let to = next_stop(&stops, every, column).unwrap_or(column + 1);
                out.resize(out.len() + (to - column), b' ');
                column = to;
            }
            8 => {
                out.push(b);
                column = column.saturating_sub(1);
            }
            _ => {
                if b != b' ' {
                    leading = false;
                }
                out.push(b);
                if b & 0xc0 != 0x80 {
                    column += 1;
                }
            }
        }
    }
    emit(ctx, &out).await
}

pub const UNEXPAND: Spec = Spec {
    name: "unexpand",
    opts: &[
        both('a', "all"),
        long("first-only"),
        both('t', "tabs").value(),
    ],
    posix_order: false,
};

pub async fn unexpand(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let p = match common::parse(ctx, &UNEXPAND, argv).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    let Some((stops, every)) = tab_stops(p.last(&UNEXPAND, "tabs")) else {
        return common::usage(ctx, "tab size cannot be 0").await;
    };
    let all =
        (p.has(&UNEXPAND, "all") || p.has(&UNEXPAND, "tabs")) && !p.has(&UNEXPAND, "first-only");
    let data = match read_all(ctx, &p.operands).await {
        Ok(data) => data,
        Err(st) => return st,
    };
    let mut out = Vec::with_capacity(data.len());
    for line in data.split_inclusive(|&b| b == b'\n') {
        // GNU's loop: blanks wait until it is known whether a tab replaces them. One
        // space that ends at a tab stop becomes a tab only when more blanks follow.
        let mut column = 0;
        let mut convert = true;
        let mut pending: Vec<u8> = Vec::new();
        let mut one_before_stop = false;
        let mut prev_blank = true;
        for &b in line {
            let mut c = b;
            let blank = c == b' ' || c == b'\t';
            if convert && blank {
                let stop = next_stop(&stops, every, column);
                match stop {
                    None => convert = false,
                    Some(stop) => {
                        if c == b'\t' {
                            column = stop;
                            if !pending.is_empty() {
                                pending[0] = b'\t';
                            }
                        } else {
                            column += 1;
                            if !(prev_blank && column == stop) {
                                if column == stop {
                                    one_before_stop = true;
                                }
                                pending.push(c);
                                prev_blank = true;
                                continue;
                            }
                            c = b'\t';
                            if pending.is_empty() {
                                pending.push(c);
                            } else {
                                pending[0] = c;
                            }
                        }
                        pending.truncate(usize::from(one_before_stop));
                    }
                }
            } else if c == 8 {
                column = column.saturating_sub(1);
            } else if c != b'\n' && c & 0xc0 != 0x80 {
                column += 1;
            }
            if !pending.is_empty() {
                if pending.len() > 1 && one_before_stop {
                    pending[0] = b'\t';
                }
                out.extend_from_slice(&pending);
                pending.clear();
                one_before_stop = false;
            }
            prev_blank = blank;
            convert &= all || blank;
            out.push(c);
        }
        out.extend_from_slice(&pending);
    }
    emit(ctx, &out).await
}

pub const COLUMN: Spec = Spec {
    name: "column",
    opts: &[
        both('t', "table"),
        both('s', "separator").value(),
        both('o', "output-separator").value(),
        both('c', "output-width").value(),
        both('x', "fillrows"),
    ],
    posix_order: false,
};

fn width_of(text: &[u8]) -> usize {
    text.iter().filter(|&&b| b & 0xc0 != 0x80).count()
}

pub async fn column(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let p = match common::parse(ctx, &COLUMN, argv).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    let data = match read_all(ctx, &p.operands).await {
        Ok(data) => data,
        Err(st) => return st,
    };
    let lines: Vec<&[u8]> = data
        .split(|&b| b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .collect();
    let mut out = Vec::new();
    if p.has(&COLUMN, "table") {
        let seps = p.last(&COLUMN, "separator").map(<[u8]>::to_vec);
        let join = p
            .last(&COLUMN, "output-separator")
            .unwrap_or(b"  ")
            .to_vec();
        let is_sep = |b: u8| match &seps {
            Some(set) => set.contains(&b),
            None => b == b' ' || b == b'\t',
        };
        let rows: Vec<Vec<&[u8]>> = lines
            .iter()
            .map(|l| l.split(|&b| is_sep(b)).filter(|f| !f.is_empty()).collect())
            .collect();
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![0; columns];
        for row in &rows {
            for (i, field) in row.iter().enumerate() {
                widths[i] = widths[i].max(width_of(field));
            }
        }
        for row in &rows {
            for (i, field) in row.iter().enumerate() {
                out.extend_from_slice(field);
                if i + 1 < row.len() {
                    out.resize(out.len() + widths[i] - width_of(field), b' ');
                    out.extend_from_slice(&join);
                }
            }
            out.push(b'\n');
        }
        return emit(ctx, &out).await;
    }
    let total = match p.last(&COLUMN, "output-width") {
        Some(w) => count(w).unwrap_or(80),
        None => ctx.var(b"COLUMNS").and_then(count).unwrap_or(80),
    };
    let longest = lines.iter().map(|l| width_of(l)).max().unwrap_or(0);
    let cell = 8 * ((longest + 8) / 8);
    let columns = (total / cell).max(1);
    let rows = lines.len().div_ceil(columns);
    let across = p.has(&COLUMN, "fillrows");
    for row in 0..rows {
        let mut written = 0;
        for col in 0..columns {
            let at = if across {
                row * columns + col
            } else {
                col * rows + row
            };
            let Some(entry) = lines.get(at) else { break };
            out.extend_from_slice(entry);
            written += width_of(entry);
            let more = if across {
                col + 1 < columns && at + 1 < lines.len()
            } else {
                (col + 1) * rows + row < lines.len()
            };
            if more {
                let end = (col + 1) * cell;
                while written < end {
                    out.push(b'\t');
                    written = (written + 8) & !7;
                }
            }
        }
        out.push(b'\n');
    }
    emit(ctx, &out).await
}

pub const JOIN: Spec = Spec {
    name: "join",
    opts: &[
        flag('1').value(),
        flag('2').value(),
        flag('j').value(),
        flag('t').value(),
        flag('a').value(),
        flag('v').value(),
        flag('e').value(),
        flag('o').value(),
        both('i', "ignore-case"),
        long("header"),
        long("nocheck-order"),
        long("check-order"),
    ],
    posix_order: false,
};

pub async fn join(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let p = match common::parse(ctx, &JOIN, argv).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    let mut keys = [1usize, 1];
    let mut unpaired = [false, false];
    let mut only_unpaired = false;
    let mut sep: Option<u8> = None;
    let mut empty: Vec<u8> = Vec::new();
    let mut format: Option<Vec<(usize, usize)>> = None;
    let mut auto = false;
    for (key, value) in p.iter(&JOIN) {
        let value = value.unwrap_or_default();
        let field = || count(value).filter(|n| *n > 0);
        let which = || match value {
            b"1" => Some(0),
            b"2" => Some(1),
            _ => None,
        };
        let bad = match key.as_str() {
            "1" => field().map(|n| keys[0] = n).is_none(),
            "2" => field().map(|n| keys[1] = n).is_none(),
            "j" => field().map(|n| keys = [n, n]).is_none(),
            "a" => which().map(|w| unpaired[w] = true).is_none(),
            "v" => which()
                .map(|w| {
                    unpaired[w] = true;
                    only_unpaired = true;
                })
                .is_none(),
            "t" => {
                sep = value.first().copied();
                value.len() > 1 && value != b"\\0"
            }
            "e" => {
                empty = value.to_vec();
                false
            }
            "o" => {
                if value == b"auto" {
                    auto = true;
                    false
                } else {
                    let mut list = Vec::new();
                    let mut ok = true;
                    for item in value
                        .split(|&b| b == b',' || b == b' ')
                        .filter(|i| !i.is_empty())
                    {
                        if item == b"0" {
                            list.push((0, 0));
                            continue;
                        }
                        let parts: Vec<&[u8]> = item.split(|&b| b == b'.').collect();
                        match (parts.as_slice(), parts.get(1).and_then(|f| count(f))) {
                            ([b"1", _], Some(f)) if f > 0 => list.push((1, f)),
                            ([b"2", _], Some(f)) if f > 0 => list.push((2, f)),
                            _ => ok = false,
                        }
                    }
                    format = Some(list);
                    !ok
                }
            }
            _ => false,
        };
        if bad {
            let v = common::quote(ctx, value);
            return common::usage(ctx, &format!("invalid field specifier: {v}")).await;
        }
    }
    let ignore_case = p.has(&JOIN, "ignore-case");
    let [first, second] = p.operands.as_slice() else {
        return common::usage(ctx, "missing operand").await;
    };
    let mut tables: Vec<Vec<Vec<Vec<u8>>>> = Vec::new();
    // Each row's line number and text, for order errors.
    let mut raws: Vec<Vec<(usize, Vec<u8>)>> = Vec::new();
    for name in [first, second] {
        let data = match read_all(ctx, std::slice::from_ref(name)).await {
            Ok(data) => data,
            Err(st) => return st,
        };
        let mut rows = Vec::new();
        let mut raw = Vec::new();
        // Every line counts, an empty one too (it has no fields, so an empty key).
        let body = data.strip_suffix(b"\n").unwrap_or(&data);
        let lines = (!data.is_empty()).then(|| body.split(|&b| b == b'\n'));
        for (n, line) in lines.into_iter().flatten().enumerate() {
            raw.push((n + 1, line.to_vec()));
            let fields: Vec<Vec<u8>> = match sep {
                Some(_) if line.is_empty() => Vec::new(),
                Some(s) => line.split(|&b| b == s).map(<[u8]>::to_vec).collect(),
                None => blank_fields(line),
            };
            rows.push(fields);
        }
        tables.push(rows);
        raws.push(raw);
    }
    let key = |row: &[Vec<u8>], side: usize| -> Vec<u8> {
        let k = row.get(keys[side] - 1).cloned().unwrap_or_default();
        if ignore_case {
            k.to_ascii_lowercase()
        } else {
            k
        }
    };
    let out_sep = sep.unwrap_or(b' ');
    let widths = [
        tables[0].first().map_or(0, Vec::len),
        tables[1].first().map_or(0, Vec::len),
    ];
    let mut out = Vec::new();
    let mut line = |a: Option<&Vec<Vec<u8>>>, b: Option<&Vec<Vec<u8>>>| {
        let mut fields: Vec<Vec<u8>> = Vec::new();
        let get = |row: Option<&Vec<Vec<u8>>>, n: usize| -> Vec<u8> {
            match row.and_then(|r| r.get(n - 1)) {
                Some(f) if !f.is_empty() => f.clone(),
                _ => empty.clone(),
            }
        };
        let join_key = match (a, b) {
            (Some(a), _) => a.get(keys[0] - 1).cloned().unwrap_or_default(),
            (None, Some(b)) => b.get(keys[1] - 1).cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        match &format {
            Some(list) => {
                for &(file, n) in list {
                    fields.push(match file {
                        0 => join_key.clone(),
                        1 => get(a, n),
                        _ => get(b, n),
                    });
                }
            }
            None => {
                fields.push(join_key);
                for (side, row) in [a, b].into_iter().enumerate() {
                    let len = if auto {
                        widths[side]
                    } else {
                        row.map_or(0, Vec::len)
                    };
                    for n in 1..=len {
                        if n != keys[side] {
                            fields.push(if auto {
                                get(row, n)
                            } else {
                                row.and_then(|r| r.get(n - 1)).cloned().unwrap_or_default()
                            });
                        }
                    }
                }
            }
        }
        out.extend_from_slice(&fields.join(&out_sep));
        out.push(b'\n');
    };
    // GNU's order check: lines are checked as they are read, from the first unpairable
    // line on (always with --check-order, never with --nocheck-order).
    let check = if p.has(&JOIN, "nocheck-order") {
        None
    } else {
        Some(p.has(&JOIN, "check-order"))
    };
    let names = [first.as_slice(), second.as_slice()];
    let mut order = Order {
        check,
        seen_unpairable: false,
        warned: [false, false],
        errors: Vec::new(),
    };
    let (mut i, mut j) = (0, 0);
    while i < tables[0].len() && j < tables[1].len() {
        let (ka, kb) = (key(&tables[0][i], 0), key(&tables[1][j], 1));
        match ka.cmp(&kb) {
            std::cmp::Ordering::Less => {
                if unpaired[0] {
                    line(Some(&tables[0][i]), None);
                }
                i += 1;
                if order.read(0, i, &tables, &key, &raws, names) {
                    break;
                }
                order.seen_unpairable = true;
            }
            std::cmp::Ordering::Greater => {
                if unpaired[1] {
                    line(None, Some(&tables[1][j]));
                }
                j += 1;
                if order.read(1, j, &tables, &key, &raws, names) {
                    break;
                }
                order.seen_unpairable = true;
            }
            std::cmp::Ordering::Equal => {
                let i_end = (i..tables[0].len())
                    .find(|&n| key(&tables[0][n], 0) != ka)
                    .unwrap_or(tables[0].len());
                let j_end = (j..tables[1].len())
                    .find(|&n| key(&tables[1][n], 1) != kb)
                    .unwrap_or(tables[1].len());
                let fatal = (i + 1..=i_end).any(|n| order.read(0, n, &tables, &key, &raws, names))
                    || (j + 1..=j_end).any(|n| order.read(1, n, &tables, &key, &raws, names));
                if !only_unpaired {
                    for a in &tables[0][i..i_end] {
                        for b in &tables[1][j..j_end] {
                            line(Some(a), Some(b));
                        }
                    }
                }
                i = i_end;
                j = j_end;
                if fatal {
                    break;
                }
            }
        }
    }
    let fatal = order.check == Some(true) && order.warned.contains(&true);
    let checktail = order.check.is_some() && !(order.warned[0] && order.warned[1]);
    if !fatal {
        for side in 0..2 {
            let (at, other) = if side == 0 { (i, j) } else { (j, i) };
            if !(unpaired[side] || checktail) || at >= tables[side].len() {
                continue;
            }
            if other < tables[1 - side].len() {
                order.seen_unpairable = true;
            }
            for n in at..tables[side].len() {
                if n > at && order.read(side, n, &tables, &key, &raws, names) {
                    break;
                }
                if unpaired[side] {
                    let row = Some(&tables[side][n]);
                    if side == 0 {
                        line(row, None);
                    } else {
                        line(None, row);
                    }
                } else if order.warned[side] {
                    break;
                }
            }
        }
    }
    for e in std::mem::take(&mut order.errors) {
        ctx.error(&e).await;
    }
    let st = emit(ctx, &out).await;
    if order.warned.contains(&true) {
        if order.check.is_none_or(|on| !on) {
            ctx.error("input is not in sorted order").await;
        }
        return 1;
    }
    st
}

/// GNU join's default field split: leading blanks are skipped and runs of blanks
/// separate fields, so blanks at the end of a line leave an empty last field.
fn blank_fields(line: &[u8]) -> Vec<Vec<u8>> {
    let blank = |b: &u8| *b == b' ' || *b == b'\t';
    let Some(start) = line.iter().position(|b| !blank(b)) else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    let mut rest = &line[start..];
    loop {
        let end = rest.iter().position(blank).unwrap_or(rest.len());
        fields.push(rest[..end].to_vec());
        if end == rest.len() {
            return fields;
        }
        let next = rest[end..].iter().position(|b| !blank(b));
        match next {
            Some(k) => rest = &rest[end + k..],
            None => {
                fields.push(Vec::new());
                return fields;
            }
        }
    }
}

/// A line of `join`'s input: its fields.
type Row = Vec<Vec<u8>>;

/// `join`'s join field of a row, by side.
type KeyFn<'a> = dyn Fn(&[Vec<u8>], usize) -> Vec<u8> + 'a;

/// `join`'s input order check (GNU `check_order`).
struct Order {
    /// `None`: --nocheck-order; `Some(true)`: --check-order (fatal).
    check: Option<bool>,
    seen_unpairable: bool,
    warned: [bool; 2],
    errors: Vec<String>,
}

impl Order {
    /// Line `n` of `side` is read: checks it against the line before. True when the
    /// disorder is fatal.
    fn read(
        &mut self,
        side: usize,
        n: usize,
        tables: &[Vec<Row>],
        key: &KeyFn<'_>,
        raws: &[Vec<(usize, Vec<u8>)>],
        names: [&[u8]; 2],
    ) -> bool {
        let Some(check) = self.check else {
            return false;
        };
        if n == 0 || n >= tables[side].len() || self.warned[side] || !(check || self.seen_unpairable)
        {
            return false;
        }
        if key(&tables[side][n - 1], side) > key(&tables[side][n], side) {
            let (no, text) = &raws[side][n];
            self.errors.push(format!(
                "{}:{no}: is not sorted: {}",
                String::from_utf8_lossy(names[side]),
                String::from_utf8_lossy(text)
            ));
            self.warned[side] = true;
            return check;
        }
        false
    }
}

/// Random bytes from the host's /dev/urandom.
pub async fn entropy(ctx: &mut UtilCtx<'_>, bytes: &mut [u8]) -> bool {
    let Ok(file) = ctx.open(b"/dev/urandom", OpenOptions::read()).await else {
        return false;
    };
    let mut got = 0;
    while got < bytes.len() {
        match file.read(&mut bytes[got..]).await {
            Ok(n) if n > 0 => got += n,
            _ => return false,
        }
    }
    true
}

pub const SHUF: Spec = Spec {
    name: "shuf",
    opts: &[
        both('e', "echo"),
        both('i', "input-range").value(),
        both('n', "head-count").value(),
        both('o', "output").value(),
        both('r', "repeat"),
        both('z', "zero-terminated"),
    ],
    posix_order: false,
};

pub async fn shuf(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let p = match common::parse(ctx, &SHUF, argv).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    let end = if p.has(&SHUF, "zero-terminated") {
        0u8
    } else {
        b'\n'
    };
    let mut items: Vec<Vec<u8>> = if p.has(&SHUF, "echo") {
        p.operands.clone()
    } else if let Some(range) = p.last(&SHUF, "input-range") {
        let parsed = std::str::from_utf8(range)
            .ok()
            .and_then(|r| r.split_once('-'))
            .and_then(|(lo, hi)| Some((lo.parse::<u64>().ok()?, hi.parse::<u64>().ok()?)))
            .filter(|(lo, hi)| lo <= hi);
        let Some((lo, hi)) = parsed else {
            let range = common::quote(ctx, range);
            return common::usage(ctx, &format!("invalid input range: {range}")).await;
        };
        (lo..=hi).map(|n| n.to_string().into_bytes()).collect()
    } else {
        if p.operands.len() > 1 {
            let extra = common::quote(ctx, &p.operands[1]);
            return common::usage(ctx, &format!("extra operand {extra}")).await;
        }
        let data = match read_all(ctx, &p.operands).await {
            Ok(data) => data,
            Err(st) => return st,
        };
        let mut items: Vec<Vec<u8>> = data.split(|&b| b == end).map(<[u8]>::to_vec).collect();
        if items.last().is_some_and(Vec::is_empty) {
            items.pop();
        }
        items
    };
    let limit = match p.last(&SHUF, "head-count") {
        Some(n) => match count(n) {
            Some(n) => Some(n),
            None => {
                let n = common::quote(ctx, n);
                return common::usage(ctx, &format!("invalid line count: {n}")).await;
            }
        },
        None => None,
    };
    let mut seed = [0u8; 16];
    if !entropy(ctx, &mut seed).await {
        ctx.error("/dev/urandom: cannot read").await;
        return 1;
    }
    let mut state = u128::from_le_bytes(seed) | 1;
    let mut random = |below: usize| -> usize {
        // xorshift, and the high bits of each step.
        state ^= state << 23;
        state ^= state >> 17;
        state ^= state << 26;
        ((state >> 64) as u64 % below as u64) as usize
    };
    let mut out = Vec::new();
    if p.has(&SHUF, "repeat") {
        if items.is_empty() {
            ctx.error("no lines to repeat").await;
            return 1;
        }
        let Some(limit) = limit else {
            return common::usage(ctx, "-r without -n would not end (psh built-in shuf)").await;
        };
        for _ in 0..limit {
            out.extend_from_slice(&items[random(items.len())]);
            out.push(end);
        }
    } else {
        let take = limit.unwrap_or(items.len()).min(items.len());
        for n in 0..take {
            let pick = n + random(items.len() - n);
            items.swap(n, pick);
            out.extend_from_slice(&items[n]);
            out.push(end);
        }
    }
    if let Some(target) = p.last(&SHUF, "output") {
        let written = match ctx.open(target, OpenOptions::write()).await {
            Ok(file) => file.write_all(&out).await,
            Err(e) => Err(e),
        };
        if let Err(e) = written {
            ctx.error(&format!(
                "{}: {}",
                String::from_utf8_lossy(target),
                error_text(&e)
            ))
            .await;
            return 1;
        }
        return 0;
    }
    emit(ctx, &out).await
}

pub const SPLIT: Spec = Spec {
    name: "split",
    opts: &[
        both('l', "lines").value(),
        both('b', "bytes").value(),
        both('n', "number").value(),
        both('a', "suffix-length").value(),
        flag('d'),
        long("numeric-suffixes").optional(),
        long("additional-suffix").value(),
    ],
    posix_order: false,
};

pub async fn split(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let p = match common::parse(ctx, &SPLIT, argv).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    if p.operands.len() > 2 {
        let extra = common::quote(ctx, &p.operands[2]);
        return common::usage(ctx, &format!("extra operand {extra}")).await;
    }
    let input: Vec<Vec<u8>> = p.operands.first().cloned().into_iter().collect();
    let prefix = p.operands.get(1).cloned().unwrap_or_else(|| b"x".to_vec());
    let data = match read_all(ctx, &input).await {
        Ok(data) => data,
        Err(st) => return st,
    };
    let mut pieces: Vec<&[u8]> = Vec::new();
    if let Some(size) = p.last(&SPLIT, "bytes") {
        let Some(size) = common::parse_count(size).filter(|n| *n > 0) else {
            let size = common::quote(ctx, size);
            return common::usage(ctx, &format!("invalid number of bytes: {size}")).await;
        };
        pieces.extend(data.chunks(size as usize));
    } else if let Some(n) = p.last(&SPLIT, "number") {
        let by_lines = n.starts_with(b"l/");
        let Some(n) = count(n.strip_prefix(b"l/").unwrap_or(n)).filter(|n| *n > 0) else {
            let n = common::quote(ctx, n);
            return common::usage(ctx, &format!("invalid number of chunks: {n}")).await;
        };
        let share = data.len() / n;
        let mut at = 0;
        for k in 0..n {
            let mut end = if k + 1 == n {
                data.len()
            } else {
                at.max((k + 1) * share)
            };
            if by_lines && k + 1 < n && end > at {
                // A chunk ends with the line that reaches its share.
                end = match data[end - 1..].iter().position(|&b| b == b'\n') {
                    Some(nl) => end + nl,
                    None => data.len(),
                };
            }
            pieces.push(&data[at.min(data.len())..end.min(data.len())]);
            at = end.min(data.len());
        }
    } else {
        let lines = match p.last(&SPLIT, "lines") {
            Some(n) => match count(n).filter(|n| *n > 0) {
                Some(n) => n,
                None => {
                    let n = common::quote(ctx, n);
                    return common::usage(ctx, &format!("invalid number of lines: {n}")).await;
                }
            },
            None => 1000,
        };
        let mut start = 0;
        let mut seen = 0;
        for (i, &b) in data.iter().enumerate() {
            if b == b'\n' {
                seen += 1;
                if seen == lines {
                    pieces.push(&data[start..=i]);
                    start = i + 1;
                    seen = 0;
                }
            }
        }
        if start < data.len() {
            pieces.push(&data[start..]);
        }
    }
    let numeric = p.has(&SPLIT, "d") || p.has(&SPLIT, "numeric-suffixes");
    let length = p.last(&SPLIT, "suffix-length").and_then(count).unwrap_or(2);
    let extra = p
        .last(&SPLIT, "additional-suffix")
        .unwrap_or_default()
        .to_vec();
    let radix: usize = if numeric { 10 } else { 26 };
    if radix
        .checked_pow(length as u32)
        .is_none_or(|most| pieces.len() > most)
    {
        ctx.error("output file suffixes exhausted").await;
        return 1;
    }
    for (n, piece) in pieces.iter().enumerate() {
        let mut suffix = vec![0u8; length];
        let mut rest = n;
        for slot in suffix.iter_mut().rev() {
            let digit = (rest % radix) as u8;
            *slot = if numeric { b'0' + digit } else { b'a' + digit };
            rest /= radix;
        }
        let name = [&prefix[..], &suffix[..], &extra[..]].concat();
        let written = match ctx.open(&name, OpenOptions::write()).await {
            Ok(file) => file.write_all(piece).await,
            Err(e) => Err(e),
        };
        if let Err(e) = written {
            ctx.error(&format!(
                "{}: {}",
                String::from_utf8_lossy(&name),
                error_text(&e)
            ))
            .await;
            return 1;
        }
    }
    0
}

pub const NUMFMT: Spec = Spec {
    name: "numfmt",
    opts: &[
        long("to").value(),
        long("from").value(),
        long("suffix").value(),
        long("to-unit").value(),
        long("from-unit").value(),
        long("padding").value(),
        long("field").value(),
        // Thousands separators: none in the C locale, the only one psh has.
        long("grouping"),
    ],
    posix_order: false,
};

/// A unit system of numfmt: its base and whether suffixes end in `i`.
#[derive(Clone, Copy, PartialEq)]
enum Scale {
    None,
    Si,
    Iec,
    IecI,
    Auto,
}

fn scale_of(text: &[u8]) -> Option<Scale> {
    Some(match text {
        b"none" => Scale::None,
        b"si" => Scale::Si,
        b"iec" => Scale::Iec,
        b"iec-i" => Scale::IecI,
        b"auto" => Scale::Auto,
        _ => return None,
    })
}

const NUMFMT_SUFFIXES: &[u8] = b"KMGTPEZYRQ";

/// Reads a number with an optional unit suffix, as `--from` reads it.
fn numfmt_parse(text: &str, from: Scale, unit: f64) -> Option<f64> {
    let digits = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(digits);
    let mut value: f64 = number.parse().ok()?;
    if !suffix.is_empty() {
        let mut chars = suffix.chars();
        let letter = chars.next()?.to_ascii_uppercase();
        let power = NUMFMT_SUFFIXES.iter().position(|&s| s as char == letter)? as i32 + 1;
        let rest = chars.as_str();
        let base = match (from, rest) {
            (Scale::Si, "") => 1000.0,
            (Scale::Iec, "") => 1024.0,
            (Scale::IecI | Scale::Auto, "i") => 1024.0,
            (Scale::Auto, "") => 1000.0,
            _ => return None,
        };
        value *= f64::powi(base, power);
    }
    Some(value * unit)
}

/// Writes a number in a unit system, rounding away from zero as GNU does by default:
/// one decimal below 10, none above.
fn numfmt_show(value: f64, to: Scale) -> String {
    let base = match to {
        Scale::Si => 1000.0,
        Scale::Iec | Scale::IecI => 1024.0,
        Scale::None | Scale::Auto => {
            return format!("{}", value.abs().ceil().copysign(value));
        }
    };
    let magnitude = value.abs();
    if magnitude < base {
        return format!("{}", magnitude.ceil().copysign(value));
    }
    let mut scaled = magnitude;
    let mut power = 0;
    while scaled >= base && power < NUMFMT_SUFFIXES.len() {
        scaled /= base;
        power += 1;
    }
    let text = loop {
        let shown = if scaled < 10.0 {
            let tenths = (scaled * 10.0 - 1e-9).ceil() / 10.0;
            if tenths >= 10.0 {
                scaled = tenths;
                continue;
            }
            format!("{tenths:.1}")
        } else {
            let whole = (scaled - 1e-9).ceil();
            if whole >= base && power < NUMFMT_SUFFIXES.len() {
                scaled = whole / base;
                power += 1;
                continue;
            }
            format!("{whole}")
        };
        break shown;
    };
    let sign = if value < 0.0 { "-" } else { "" };
    let i = if to == Scale::IecI { "i" } else { "" };
    format!("{sign}{text}{}{i}", NUMFMT_SUFFIXES[power - 1] as char)
}

/// A --field list: `N`, `N-M`, `N-`, `-M` or `-`, separated by commas, as
/// inclusive ranges from 1.
fn numfmt_fields(spec: &[u8]) -> Option<Vec<(usize, usize)>> {
    let num = |s: &str| s.parse::<usize>().ok().filter(|&n| n > 0);
    std::str::from_utf8(spec)
        .ok()?
        .split(',')
        .map(|part| match part.split_once('-') {
            None => num(part).map(|n| (n, n)),
            Some(("", "")) => Some((1, usize::MAX)),
            Some((a, "")) => Some((num(a)?, usize::MAX)),
            Some(("", b)) => Some((1, num(b)?)),
            Some((a, b)) => Some((num(a)?, num(b)?)).filter(|(a, b)| a <= b),
        })
        .collect()
}

pub async fn numfmt(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let p = match common::parse(ctx, &NUMFMT, argv).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    let mut scales = [Scale::None, Scale::None];
    for (slot, key) in scales.iter_mut().zip(["to", "from"]) {
        if let Some(v) = p.last(&NUMFMT, key) {
            match scale_of(v) {
                Some(s) if key == "from" || s != Scale::Auto => *slot = s,
                _ => {
                    let v = common::quote(ctx, v);
                    return common::usage(ctx, &format!("invalid argument {v} for '--{key}'"))
                        .await;
                }
            }
        }
    }
    let [to, from] = scales;
    let unit = |key: &str| -> Option<f64> {
        match p.last(&NUMFMT, key) {
            None => Some(1.0),
            Some(v) => numfmt_parse(std::str::from_utf8(v).ok()?, Scale::Auto, 1.0),
        }
    };
    let (Some(from_unit), Some(to_unit)) = (unit("from-unit"), unit("to-unit")) else {
        return common::usage(ctx, "invalid unit size").await;
    };
    let suffix = p.last(&NUMFMT, "suffix").unwrap_or_default().to_vec();
    let padding = match p.last(&NUMFMT, "padding") {
        None => 0,
        Some(v) => match std::str::from_utf8(v).ok().and_then(|v| v.parse::<i64>().ok()) {
            Some(n) if n != 0 => n,
            _ => {
                let v = common::quote(ctx, v);
                return common::usage(ctx, &format!("invalid padding value {v}")).await;
            }
        },
    };
    let fields = match p.last(&NUMFMT, "field") {
        None => vec![(1, 1)],
        Some(v) => match numfmt_fields(v) {
            Some(f) => f,
            None => {
                let v = common::quote(ctx, v);
                return common::usage(ctx, &format!("invalid field value {v}")).await;
            }
        },
    };
    let pad = |text: String| -> String {
        let width = padding.unsigned_abs() as usize;
        match padding {
            n if n > 0 => format!("{text:>width$}"),
            n if n < 0 => format!("{text:<width$}"),
            _ => text,
        }
    };
    let numbers: Vec<Vec<u8>> = if p.operands.is_empty() {
        let data = match read_all(ctx, &[]).await {
            Ok(data) => data,
            Err(st) => return st,
        };
        data.split(|&b| b == b'\n').map(<[u8]>::to_vec).collect::<Vec<_>>()
    } else {
        p.operands.clone()
    };
    let last_empty = p.operands.is_empty() && numbers.last().is_some_and(Vec::is_empty);
    let count = numbers.len() - usize::from(last_empty);
    let mut out = Vec::new();
    let only_first = p.last(&NUMFMT, "field").is_none();
    for raw in &numbers[..count] {
        let text = String::from_utf8_lossy(raw);
        // Fields are runs of non-blanks, each kept with the blanks before it; without
        // --field the whole line is the number.
        let mut tokens: Vec<(&str, &str)> = Vec::new();
        if only_first {
            tokens.push(("", text.trim()));
        } else {
            let mut rest: &str = &text;
            while !rest.is_empty() {
                let lead = rest.len() - rest.trim_start().len();
                let (blanks, after) = rest.split_at(lead);
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                let (word, next) = after.split_at(end);
                tokens.push((blanks, word));
                rest = next;
            }
        }
        for (n, (blanks, word)) in tokens.iter().enumerate() {
            out.extend_from_slice(blanks.as_bytes());
            let n = n + 1;
            if !fields.iter().any(|&(a, b)| a <= n && n <= b) || (word.is_empty() && !only_first) {
                out.extend_from_slice(word.as_bytes());
                continue;
            }
            let body = word.strip_suffix(std::str::from_utf8(&suffix).unwrap_or("")).unwrap_or(word);
            let Some(value) = numfmt_parse(body, from, from_unit) else {
                let _ = ctx.write(&out).await;
                let shown = common::quote(ctx, word.as_bytes());
                ctx.error(&format!("invalid number: {shown}")).await;
                return 2;
            };
            let mut shown = numfmt_show(value / to_unit, to);
            shown.push_str(std::str::from_utf8(&suffix).unwrap_or(""));
            out.extend_from_slice(pad(shown).as_bytes());
        }
        out.push(b'\n');
    }
    emit(ctx, &out).await
}

pub const TSORT: Spec = Spec {
    name: "tsort",
    opts: &[],
    posix_order: false,
};

/// tsort: Kahn's algorithm, nodes taken in the order they first appear. A loop is
/// reported (its nodes listed) and broken, and the status is 1, as GNU does.
pub async fn tsort(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let p = match common::parse(ctx, &TSORT, argv).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    if p.operands.len() > 1 {
        let extra = common::quote(ctx, &p.operands[1]);
        return common::usage(ctx, &format!("extra operand {extra}")).await;
    }
    let data = match read_all(ctx, &p.operands).await {
        Ok(data) => data,
        Err(st) => return st,
    };
    let shown = p.operands.first().map_or_else(|| "-".to_string(), |n| String::from_utf8_lossy(n).into_owned());
    let tokens: Vec<&[u8]> = data
        .split(|b| b.is_ascii_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.len() % 2 == 1 {
        ctx.error(&format!("{shown}: input contains an odd number of tokens")).await;
        return 1;
    }
    let mut names: Vec<&[u8]> = Vec::new();
    let mut index = std::collections::HashMap::new();
    let mut next: Vec<Vec<usize>> = Vec::new();
    let mut indegree: Vec<usize> = Vec::new();
    for pair in tokens.as_chunks::<2>().0 {
        let mut ids = [0usize; 2];
        for (slot, &token) in ids.iter_mut().zip(pair) {
            *slot = *index.entry(token).or_insert_with(|| {
                names.push(token);
                next.push(Vec::new());
                indegree.push(0);
                names.len() - 1
            });
        }
        if ids[0] != ids[1] {
            next[ids[0]].push(ids[1]);
            indegree[ids[1]] += 1;
        }
    }
    let mut out = Vec::new();
    let mut done = vec![false; names.len()];
    let mut status = 0;
    let mut left = names.len();
    while left > 0 {
        let ready: Vec<usize> = (0..names.len()).filter(|&n| !done[n] && indegree[n] == 0).collect();
        let ready = if ready.is_empty() {
            // A loop: report it from the first node left, and break it there.
            status = 1;
            let start = (0..names.len()).find(|&n| !done[n]).unwrap_or(0);
            ctx.error(&format!("{shown}: input contains a loop:")).await;
            let mut at = start;
            let mut seen = vec![false; names.len()];
            while !seen[at] {
                seen[at] = true;
                ctx.error(&String::from_utf8_lossy(names[at])).await;
                match next[at].iter().find(|&&n| !done[n]) {
                    Some(&n) => at = n,
                    None => break,
                }
            }
            indegree[start] = 0;
            vec![start]
        } else {
            ready
        };
        for n in ready {
            if done[n] {
                continue;
            }
            done[n] = true;
            left -= 1;
            out.extend_from_slice(names[n]);
            out.push(b'\n');
            for &m in &next[n] {
                indegree[m] = indegree[m].saturating_sub(1);
            }
        }
    }
    let st = emit(ctx, &out).await;
    if st != 0 { st } else { status }
}

pub const FMT: Spec = Spec {
    name: "fmt",
    opts: &[
        both('w', "width").value(),
        both('p', "prefix").value(),
        both('s', "split-only"),
        both('u', "uniform-spacing"),
        both('c', "crown-margin"),
        both('t', "tagged-paragraph"),
    ],
    posix_order: false,
};

/// fmt: fills each paragraph (lines between blank lines with the same indentation)
/// with GNU's line breaking: least cost against a goal width, sentence ends kept two
/// spaces apart.
pub async fn fmt(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>]) -> i32 {
    let mut args = argv.to_vec();
    for arg in args.iter_mut().skip(1) {
        if arg.len() > 1 && arg[0] == b'-' && arg[1..].iter().all(u8::is_ascii_digit) {
            let mut wide = b"-w".to_vec();
            wide.extend_from_slice(&arg[1..]);
            *arg = wide;
        }
    }
    let p = match common::parse(ctx, &FMT, &args).await {
        Ok(p) => p,
        Err(st) => return st,
    };
    if p.has(&FMT, "prefix") || p.has(&FMT, "tagged-paragraph") || p.has(&FMT, "crown-margin") {
        return crate::textio::fall_through(ctx, argv, "fmt -p, -t and -c").await;
    }
    let width = match p.last(&FMT, "width") {
        Some(w) => match count(w) {
            Some(n) if n > 0 => n,
            _ => {
                let w = common::quote(ctx, w);
                return common::usage(ctx, &format!("invalid width: {w}")).await;
            }
        },
        None => 75,
    };
    let split_only = p.has(&FMT, "split-only");
    let uniform = p.has(&FMT, "uniform-spacing");
    let data = match read_all(ctx, &p.operands).await {
        Ok(data) => data,
        Err(st) => return st,
    };
    let mut out = Vec::new();
    let mut words: Vec<FmtWord> = Vec::new();
    let mut indent: &[u8] = b"";
    for line in data.split_inclusive(|&b| b == b'\n') {
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        let lead = body.iter().take_while(|b| **b == b' ' || **b == b'\t').count();
        if lead == body.len() {
            fmt_paragraph(&mut out, &mut words, indent, width);
            out.extend_from_slice(body);
            out.push(b'\n');
            continue;
        }
        if words.is_empty() || split_only || body[..lead] != *indent {
            fmt_paragraph(&mut out, &mut words, indent, width);
            indent = &body[..lead];
        }
        fmt_words(&body[lead..], uniform, &mut words);
    }
    // The input's last word ends a sentence, as GNU counts it.
    if let Some(last) = words.last_mut() {
        last.final_ = true;
    }
    fmt_paragraph(&mut out, &mut words, indent, width);
    emit(ctx, &out).await
}

/// A word of fmt, with what GNU's line breaking weighs.
struct FmtWord {
    text: Vec<u8>,
    /// Spaces after it within a line.
    space: usize,
    /// Ends a sentence (`.?!` then two spaces or the line's end).
    final_: bool,
    period: bool,
    punct: bool,
    paren: bool,
}

fn fmt_words(line: &[u8], uniform: bool, words: &mut Vec<FmtWord>) {
    let mut at = 0;
    while at < line.len() {
        let end = line[at..].iter().position(|&b| b == b' ' || b == b'\t').map_or(line.len(), |n| at + n);
        let text = line[at..end].to_vec();
        let gap_end = line[end..].iter().position(|&b| b != b' ' && b != b'\t').map_or(line.len(), |n| end + n);
        let last_real = text.iter().rev().find(|&&b| !matches!(b, b')' | b']' | b'}' | b'\'' | b'"'));
        let period = last_real.is_some_and(|&b| matches!(b, b'.' | b'?' | b'!'));
        let at_end = gap_end == line.len();
        let gap = gap_end - end;
        let final_ = period && (gap > 1 || at_end);
        let space = if at_end || uniform { if final_ { 2 } else { 1 } } else { gap };
        words.push(FmtWord {
            punct: text.last().is_some_and(u8::is_ascii_punctuation),
            paren: text.first().is_some_and(|b| matches!(b, b'(' | b'[' | b'{' | b'\'' | b'"' | b'`')),
            text,
            space,
            final_,
            period,
        });
        at = gap_end;
    }
}

/// GNU fmt's costs: the square of a shortfall, scaled.
const fn fmt_equiv(n: i64) -> i64 {
    n * n
}

fn fmt_short_cost(n: i64) -> i64 {
    fmt_equiv(n * 10)
}

fn fmt_ragged_cost(n: i64) -> i64 {
    fmt_short_cost(n) / 2
}

/// The cost of breaking a line before word `i`, as GNU's base_cost.
fn fmt_base_cost(words: &[FmtWord], i: usize) -> i64 {
    let mut cost = fmt_equiv(70);
    if i > 0 {
        let prev = &words[i - 1];
        if prev.period {
            if prev.final_ {
                cost -= fmt_equiv(50);
            } else {
                cost += fmt_equiv(600);
            }
        } else if prev.punct {
            cost -= fmt_equiv(40);
        } else if i > 1 && words[i - 2].final_ {
            cost += fmt_equiv(200) / (words[i - 2].text.len() as i64 + 2);
        }
    }
    if words[i].paren {
        cost -= fmt_equiv(40);
    } else if words[i].final_ {
        cost += fmt_equiv(150) / (words[i].text.len() as i64 + 2);
    }
    cost
}

/// Breaks a paragraph into lines as GNU fmt does: the least total cost, lines shorter
/// than the width, aiming at 93.5% of it, with ragged edges costing too.
fn fmt_paragraph(out: &mut Vec<u8>, words: &mut Vec<FmtWord>, indent: &[u8], width: usize) {
    let count = words.len();
    if count == 0 {
        return;
    }
    let goal = (width * 187 / 200) as i64;
    let mut best = vec![0i64; count + 1];
    let mut next = vec![count; count + 1];
    let mut length = vec![0i64; count + 1];
    for start in (0..count).rev() {
        let mut best_cost = i64::MAX;
        let mut len = (indent.len() + words[start].text.len()) as i64;
        let mut w = start;
        loop {
            w += 1;
            let line_cost = if w == count {
                0
            } else {
                let mut c = fmt_short_cost(goal - len);
                if next[w] != count {
                    c += fmt_ragged_cost(len - length[w]);
                }
                c
            };
            let cost = line_cost.saturating_add(best[w]);
            if cost < best_cost {
                best_cost = cost;
                next[start] = w;
                length[start] = len;
            }
            if w == count {
                break;
            }
            len += (words[w - 1].space + words[w].text.len()) as i64;
            if len >= width as i64 {
                break;
            }
        }
        best[start] = best_cost.saturating_add(fmt_base_cost(words, start));
    }
    let mut start = 0;
    while start < count {
        let end = next[start];
        out.extend_from_slice(indent);
        for (i, w) in words.iter().enumerate().take(end).skip(start) {
            out.extend_from_slice(&w.text);
            if i + 1 < end {
                out.extend(std::iter::repeat_n(b' ', w.space));
            }
        }
        out.push(b'\n');
        start = end;
    }
    words.clear();
}
