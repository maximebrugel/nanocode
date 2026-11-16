//! Runs one djs benchmark file: builds a snapshot of the script, instantiates it,
//! calls its global `main()` once and prints `{"ms": <main's wall time>, "result": "<value>"}`.
//! Compilation and instantiation are outside the timed region, as in the QuickJS wrapper
//! that `run.py` writes.

use djs_embed::{Data, Limits, Snapshot, Source, Value};
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("usage: djsrun <bench.js>");
    let src = std::fs::read_to_string(&path).expect("read the benchmark file");
    let snapshot = Snapshot::builder()
        .source(Source::script("bench.js", &src))
        .build()
        .expect("build the snapshot");
    let limits = Limits {
        memory: 1 << 30,
        fuel: u64::MAX / 4,
        slice: 1 << 40,
        deadline: None,
    };
    let mut vm = snapshot.instantiate(limits).expect("instantiate");
    let start = Instant::now();
    let out = vm
        .call("main", &[], |call| call.reply(Ok(Value::Undefined)))
        .expect("call main");
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let result = match out {
        Data::Number(n) => format!("{n}"),
        other => format!("{other:?}"),
    };
    println!("{{\"ms\": {ms:.6}, \"result\": \"{result}\"}}");
}
