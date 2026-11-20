use djs_embed::{Data, Limits, Snapshot, Source};
use std::io::Read;
// Reads a JSON array of JS sources on stdin; prints one JSON string per source (output of text() calls or ERR).
fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let cases: Vec<String> = serde_like(&input);
    for c in cases {
        let code = format!(
            "var __o=[];function text(v){{__o.push(typeof v==='string'?v:JSON.stringify(v))}}\n{}\nfunction __get(){{return __o.join('\\n')}}",
            c
        );
        let out = match Snapshot::builder()
            .source(Source::script("case.js", &code))
            .build()
        {
            Err(e) => format!("ERR build {e:?}"),
            Ok(s) => match s.instantiate(Limits::default()) {
                Err(e) => format!("ERR inst {e:?}"),
                Ok(mut vm) => match vm.call("__get", &[], |_| unreachable!()) {
                    Ok(Data::String(s)) => s,
                    Ok(d) => format!("ERR data {d:?}"),
                    Err(e) => format!("ERR call {e:?}"),
                },
            },
        };
        println!("{}", json_str(&out));
    }
}
fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}
// input: one JS source per line, JSON-string-encoded
fn serde_like(s: &str) -> Vec<String> {
    s.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let b: Vec<char> = l.trim().chars().collect();
            let mut o = String::new();
            let mut i = 1;
            while i < b.len() - 1 {
                let c = b[i];
                if c == '\\' {
                    i += 1;
                    match b[i] {
                        'n' => o.push('\n'),
                        't' => o.push('\t'),
                        'r' => o.push('\r'),
                        'b' => o.push('\u{8}'),
                        'f' => o.push('\u{c}'),
                        'u' => {
                            let h: String = b[i + 1..i + 5].iter().collect();
                            let mut cp = u32::from_str_radix(&h, 16).unwrap();
                            i += 4;
                            if (0xd800..0xdc00).contains(&cp)
                                && b.get(i + 1) == Some(&'\\')
                                && b.get(i + 2) == Some(&'u')
                            {
                                let h2: String = b[i + 3..i + 7].iter().collect();
                                let lo = u32::from_str_radix(&h2, 16).unwrap();
                                cp = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
                                i += 6;
                            }
                            o.push(char::from_u32(cp).unwrap_or('\u{fffd}'))
                        }
                        x => o.push(x),
                    }
                } else {
                    o.push(c)
                }
                i += 1;
            }
            o
        })
        .collect()
}
