//! Latency and throughput of the board sync over each log backend: webhook to
//! board (ingest) and board to platform with its receipt (drain).

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use nanocode_board::{Board, NewTask, Op, TaskId, mirror::Mode};
use nanocode_board_sync::{
    Accounts, Author, Engine, Event, Inbound, Plain, Platform, Route, Router, Target, fake::Fake,
};
use nanocode_sync::block_on;
use nanocode_turnlog::{FileLog, FileOptions, LogHost, MemoryLog};

const SCOPE: &str = "bench";
const WRITER: &str = "bench";
const SLACK: &str = "slack";

struct Unique;

impl Router for Unique {
    fn route(&self, _thread: &Target, _author: &Author, _body: &str) -> Route {
        Route::Ignore
    }

    fn mirror(&self, task: &NewTask) -> Vec<(Target, Mode)> {
        vec![(Target::new(SLACK, format!("C-{}", task.id)), Mode::Always)]
    }
}

struct Row {
    backend: &'static str,
    case: &'static str,
    events: usize,
    p50: Option<Duration>,
    p99: Option<Duration>,
    total: Duration,
}

fn var(name: &str, default: usize) -> usize {
    std::env::var(name).map_or(default, |value| {
        let parsed = value.parse::<usize>().expect("numeric bench variable");
        assert!(parsed > 0, "{name} must be positive");
        parsed
    })
}

fn task(id: &str) -> NewTask {
    NewTask { id: TaskId(id.to_owned()), parent: None, spec: format!("spec of {id}"), labels: BTreeMap::new(), needs: Vec::new() }
}

fn author() -> Author {
    Author { user: "U1".to_owned(), name: "ada".to_owned(), bot: false }
}

fn pct(sorted: &[Duration], per_mille: usize) -> Duration {
    assert!(!sorted.is_empty());
    sorted[(sorted.len() * per_mille / 1000).min(sorted.len() - 1)]
}

fn row(backend: &'static str, case: &'static str, mut laps: Vec<Duration>, total: Duration) -> Row {
    let events = laps.len();
    laps.sort();
    Row { backend, case, events, p50: Some(pct(&laps, 500)), p99: Some(pct(&laps, 990)), total }
}

fn bulk(backend: &'static str, case: &'static str, events: usize, total: Duration) -> Row {
    Row { backend, case, events, p50: None, p99: None, total }
}

fn engine(fake: &Arc<Fake>) -> Engine<Plain, Unique, Accounts> {
    let platform: Arc<dyn Platform> = Arc::clone(fake) as Arc<dyn Platform>;
    Engine::new(Plain, Unique, Accounts::default(), BTreeMap::from([(SLACK.to_owned(), platform)]))
}

fn open(host: &Arc<dyn LogHost>) -> Board {
    block_on(Board::open(Arc::clone(host), SCOPE, WRITER)).expect("board opens")
}

fn inbound_open(host: &Arc<dyn LogHost>, backend: &'static str, events: usize, rows: &mut Vec<Row>) {
    let fake = Arc::new(Fake::new(SLACK));
    let engine = engine(&fake);
    let mut board = open(host);
    let mut laps = Vec::with_capacity(events);
    let begin = Instant::now();
    for index in 0..events {
        let id = format!("in{index}");
        let thread = Target::new(SLACK, format!("C1:{index}.1"));
        let event = Event::Opened { task: task(&id), thread, message: format!("{index}.1"), mode: Mode::Always };
        let inbound = Inbound { key: format!("hook/open/{index}"), event };
        let lap = Instant::now();
        let done = block_on(engine.ingest(&mut board, index as u64, &inbound)).expect("ingest");
        laps.push(lap.elapsed());
        assert_eq!(done.written, 2);
        assert_eq!(done.skipped, 0);
    }
    let total = begin.elapsed();
    assert_eq!(board.fold().entries().len(), events * 2);
    rows.push(row(backend, "inbound open", laps, total));
}

fn inbound_message(host: &Arc<dyn LogHost>, backend: &'static str, events: usize, rows: &mut Vec<Row>) {
    let fake = Arc::new(Fake::new(SLACK));
    let engine = engine(&fake);
    let mut board = open(host);
    let thread = Target::new(SLACK, "C1:1.1");
    let opened = Event::Opened { task: task("thread"), thread: thread.clone(), message: "1.1".to_owned(), mode: Mode::Always };
    block_on(engine.ingest(&mut board, 0, &Inbound { key: "hook/open".to_owned(), event: opened })).expect("open");
    let hooks = (0..events)
        .map(|index| {
            let event = Event::Message {
                thread: thread.clone(),
                message: format!("2.{index}"),
                author: author(),
                body: format!("message {index}"),
                context: None,
                sub: None,
                marker: None,
            };
            Inbound { key: format!("hook/msg/{index}"), event }
        })
        .collect::<Vec<_>>();
    let before = board.fold().entries().len();
    let mut laps = Vec::with_capacity(events);
    let begin = Instant::now();
    for (index, hook) in hooks.iter().enumerate() {
        let lap = Instant::now();
        let done = block_on(engine.ingest(&mut board, index as u64 + 1, hook)).expect("ingest");
        laps.push(lap.elapsed());
        assert_eq!(done.written, 1);
    }
    let total = begin.elapsed();
    assert_eq!(board.fold().entries().len(), before + events);
    rows.push(row(backend, "inbound message", laps, total));

    let mut laps = Vec::with_capacity(events);
    let begin = Instant::now();
    for (index, hook) in hooks.iter().enumerate() {
        let lap = Instant::now();
        let done = block_on(engine.ingest(&mut board, (events + index) as u64, hook)).expect("ingest");
        laps.push(lap.elapsed());
        assert_eq!(done.written + done.skipped, 1);
    }
    let total = begin.elapsed();
    assert_eq!(board.fold().entries().len(), before + events);
    rows.push(row(backend, "inbound redelivery", laps, total));
}

fn outbound_each(host: &Arc<dyn LogHost>, backend: &'static str, events: usize, rows: &mut Vec<Row>) {
    let fake = Arc::new(Fake::new(SLACK));
    let mut engine = engine(&fake);
    let mut board = open(host);
    let mut laps = Vec::with_capacity(events);
    let begin = Instant::now();
    for index in 0..events {
        let at = index as u64;
        let lap = Instant::now();
        block_on(engine.post(&mut board, &format!("post/{index}"), at, task(&format!("out{index}")))).expect("post");
        let drained = block_on(engine.drain(&mut board, at)).expect("drain");
        laps.push(lap.elapsed());
        assert_eq!(drained.posted, 1);
        assert_eq!(drained.failed + drained.dead + drained.unsettled, 0);
    }
    let total = begin.elapsed();
    assert_eq!(fake.copies().len(), events);
    assert_eq!(fake.lock().calls, events as u64);
    rows.push(row(backend, "outbound post+drain", laps, total));
}

fn outbound_reply(host: &Arc<dyn LogHost>, backend: &'static str, events: usize, rows: &mut Vec<Row>) {
    let fake = Arc::new(Fake::new(SLACK));
    let mut engine = engine(&fake);
    let mut board = open(host);
    block_on(engine.post(&mut board, "post/thread", 0, task("thread"))).expect("post");
    let first = block_on(engine.drain(&mut board, 0)).expect("drain");
    assert_eq!(first.posted, 1);
    let mut laps = Vec::with_capacity(events);
    let begin = Instant::now();
    for index in 0..events {
        let at = index as u64 + 1;
        let lap = Instant::now();
        block_on(board.submit(at, Op::Reply { task: TaskId("thread".to_owned()), body: format!("reply {index}") })).expect("reply");
        let drained = block_on(engine.drain(&mut board, at)).expect("drain");
        laps.push(lap.elapsed());
        assert_eq!(drained.posted, 1);
        assert_eq!(drained.failed + drained.dead + drained.unsettled, 0);
    }
    let total = begin.elapsed();
    assert_eq!(fake.copies().len(), events + 1);
    rows.push(row(backend, "outbound reply+drain", laps, total));
}

fn outbound_bulk(host: &Arc<dyn LogHost>, backend: &'static str, events: usize, rows: &mut Vec<Row>) {
    let fake = Arc::new(Fake::new(SLACK));
    let mut engine = engine(&fake);
    let mut board = open(host);
    for index in 0..events {
        block_on(engine.post(&mut board, &format!("post/{index}"), index as u64, task(&format!("bulk{index}")))).expect("post");
    }
    let begin = Instant::now();
    let drained = block_on(engine.drain(&mut board, events as u64)).expect("drain");
    let total = begin.elapsed();
    assert_eq!(drained.posted, events as u64);
    assert_eq!(drained.failed + drained.dead + drained.unsettled, 0);
    assert_eq!(fake.copies().len(), events);
    rows.push(bulk(backend, "outbound bulk drain", events, total));
}

fn suite(make: &dyn Fn() -> Arc<dyn LogHost>, backend: &'static str, events: usize, rows: &mut Vec<Row>) {
    inbound_open(&make(), backend, events, rows);
    inbound_message(&make(), backend, events, rows);
    outbound_each(&make(), backend, events, rows);
    outbound_reply(&make(), backend, events, rows);
    outbound_bulk(&make(), backend, events, rows);
}

fn micros(value: Option<Duration>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{:.0}", value.as_secs_f64() * 1e6))
}

fn main() {
    let events = var("NANOCODE_BENCH_EVENTS", 200);
    let synced = var("NANOCODE_BENCH_SYNC_EVENTS", 50);
    let dir = std::env::var_os("NANOCODE_BENCH_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let dir = dir.join(format!("nanocode-board-sync-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("bench directory");
    let mut rows = Vec::new();
    suite(&|| Arc::new(MemoryLog::new()), "memory", events, &mut rows);
    let counter = std::cell::Cell::new(0_u32);
    let file = |sync: bool| {
        let counter = &counter;
        let dir = dir.clone();
        move || {
            counter.set(counter.get() + 1);
            let path = dir.join(format!("log-{sync}-{}", counter.get()));
            let log = FileLog::open(&path, FileOptions { scope: SCOPE, sync }).expect("file log");
            Arc::new(log) as Arc<dyn LogHost>
        }
    };
    suite(&file(false), "file", events, &mut rows);
    suite(&file(true), "file+fsync", synced, &mut rows);
    std::fs::remove_dir_all(&dir).expect("bench directory removed");
    println!("{:<11} {:<22} {:>7} {:>10} {:>10} {:>10}", "backend", "case", "events", "p50 us", "p99 us", "events/s");
    for row in &rows {
        let rate = row.events as f64 / row.total.as_secs_f64();
        println!("{:<11} {:<22} {:>7} {:>10} {:>10} {:>10.0}", row.backend, row.case, row.events, micros(row.p50), micros(row.p99), rate);
    }
}
