//! Board benchmarks: write throughput and latency under racing writers,
//! claim-next, refold on open, refresh, memory per task, keyed-submit dedup
//! and watcher catch-up, on every turn log host nanocode has.
//!
//! cargo run --release --offline -p nanocode-board --example board_bench -- \
//!     [--backend memory|file|sqlite|postgres]... [--only NAME]... [--quick] \
//!     [--json PATH] [--target POSTGRES_URL]
//!
//! NAME is one of submit, claim, open, refresh, memory, keyed, catchup.
//! Each result prints as one "BENCH backend=.. bench=.. param=.. key=value.."
//! line; --json also appends them as JSON lines to PATH. SQLite runs through
//! node:sqlite (one node process per connection), as the conformance example
//! does; the file host runs with sync off.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio},
    sync::{
        Arc, Barrier, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use nanocode_board::{Board, ClaimId, Event, EventId, Fold, NewTask, Op, Outcome, TaskId};
use nanocode_durability_core::{Sql, SqlDialect, SqlRow, SqlStatement, StoreError, StoreFuture};
use nanocode_store_sql::{PgOptions, PgPool, PgSql};
use nanocode_turnlog::{FileLog, FileOptions, LogHost, MemoryLog, SqlLog};
use serde::Deserialize;

// ---------------------------------------------------------------- memory

/// Resident bytes of this process (the workspace forbids unsafe code, so no
/// counting allocator): the memory bench reads its growth over 100K tasks.
fn resident() -> i64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<i64>().ok())
        .map_or(0, |pages| pages * 4096)
}

// ---------------------------------------------------------------- config

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Memory,
    File,
    Sqlite,
    Postgres,
}

impl Backend {
    const fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::File => "file",
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
        }
    }

    /// The most rows a preload writes on this backend.
    const fn max_rows(self) -> usize {
        match self {
            Self::Memory | Self::File => 1_000_000,
            Self::Sqlite | Self::Postgres => 100_000,
        }
    }

    const fn local(self) -> bool {
        matches!(self, Self::Memory | Self::File)
    }
}

struct Config {
    backends: Vec<Backend>,
    only: Vec<String>,
    quick: bool,
    json: Option<PathBuf>,
    target: String,
    dir: PathBuf,
}

impl Config {
    fn wants(&self, bench: &str) -> bool {
        self.only.is_empty() || self.only.iter().any(|only| only == bench)
    }
}

fn parse() -> Result<Config, String> {
    let mut config = Config {
        backends: Vec::new(),
        only: Vec::new(),
        quick: false,
        json: None,
        target: "postgres://postgres@127.0.0.1:55432/postgres?sslmode=disable".to_owned(),
        dir: std::env::temp_dir().join(format!("board-bench-{}", std::process::id())),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--backend" => config.backends.push(match value()?.as_str() {
                "memory" => Backend::Memory,
                "file" => Backend::File,
                "sqlite" => Backend::Sqlite,
                "postgres" => Backend::Postgres,
                other => return Err(format!("unknown backend {other}")),
            }),
            "--only" => config.only.push(value()?),
            "--quick" => config.quick = true,
            "--json" => config.json = Some(PathBuf::from(value()?)),
            "--target" => config.target = value()?,
            "--dir" => config.dir = PathBuf::from(value()?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if config.backends.is_empty() {
        config.backends = vec![
            Backend::Memory,
            Backend::File,
            Backend::Sqlite,
            Backend::Postgres,
        ];
    }
    Ok(config)
}

// ---------------------------------------------------------------- SQLite

/// A SQLite connection: a node:sqlite process that runs each statement list
/// in one BEGIN IMMEDIATE transaction (the conformance example's script).
struct NodeSqlite {
    connection: Mutex<(Child, ChildStdin, BufReader<ChildStdout>)>,
}

#[derive(Deserialize)]
struct Answer {
    ok: Option<Vec<Vec<SqlRow>>>,
    err: Option<String>,
}

impl NodeSqlite {
    fn open(path: &Path) -> Result<Self, String> {
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/conformance/sqlite.mjs"
        );
        let mut child = Command::new("node")
            .arg("--no-warnings")
            .arg(script)
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("node: {error}"))?;
        let stdin = child.stdin.take().ok_or("node stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("node stdout")?);
        Ok(Self {
            connection: Mutex::new((child, stdin, stdout)),
        })
    }

    fn call(&self, statements: &[SqlStatement<'_>]) -> Result<Vec<Vec<SqlRow>>, StoreError> {
        let backend = |detail: String| StoreError::Backend(format!("sqlite: {detail}"));
        let mut connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (_, stdin, stdout) = &mut *connection;
        let line = serde_json::json!({ "statements": statements }).to_string();
        writeln!(stdin, "{line}").map_err(|error| backend(error.to_string()))?;
        stdin.flush().map_err(|error| backend(error.to_string()))?;
        let mut reply = String::new();
        stdout
            .read_line(&mut reply)
            .map_err(|error| backend(error.to_string()))?;
        let answer: Answer =
            serde_json::from_str(&reply).map_err(|error| backend(format!("{error}: {reply:?}")))?;
        match (answer.ok, answer.err) {
            (Some(rows), None) => Ok(rows),
            (_, Some(error)) => Err(backend(error)),
            (None, None) => Err(backend("empty answer".to_owned())),
        }
    }
}

impl Drop for NodeSqlite {
    fn drop(&mut self) {
        let connection = self
            .connection
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        let _ = connection.0.kill();
        let _ = connection.0.wait();
    }
}

impl Sql for NodeSqlite {
    fn transaction<'a>(
        &'a self,
        _operation: &'static str,
        statements: &'a [SqlStatement<'a>],
    ) -> StoreFuture<'a, Result<Vec<Vec<SqlRow>>, StoreError>> {
        let result = self.call(statements);
        Box::pin(async move { result })
    }

    fn dialect(&self) -> SqlDialect {
        SqlDialect::Sqlite
    }
}

// ---------------------------------------------------------------- hosts

/// Counts the round trips a board makes through it.
struct Counted {
    inner: Arc<dyn LogHost>,
    scans: AtomicU64,
    appends: AtomicU64,
    failed: AtomicU64,
}

impl Counted {
    fn new(inner: Arc<dyn LogHost>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            scans: AtomicU64::new(0),
            appends: AtomicU64::new(0),
            failed: AtomicU64::new(0),
        })
    }
}

impl LogHost for Counted {
    fn scan<'a>(
        &'a self,
        scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        self.scans.fetch_add(1, Ordering::Relaxed);
        self.inner.scan(scope, from, to)
    }

    fn load<'a>(
        &'a self,
        scope: &'a str,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        self.inner.load(scope)
    }

    fn append<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        self.appends.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            let result = self.inner.append(scope, rows).await;
            if result.is_err() {
                self.failed.fetch_add(1, Ordering::Relaxed);
            }
            result
        })
    }
}

/// One store a bench runs on: a scope and a way to open connections to it.
struct Store {
    backend: Backend,
    scope: String,
    memory: Arc<MemoryLog>,
    file: Option<Arc<FileLog>>,
    path: PathBuf,
    target: String,
}

static SCOPES: AtomicU64 = AtomicU64::new(0);

impl Store {
    fn new(config: &Config, backend: Backend) -> Result<Self, String> {
        let unique = format!(
            "{}-{}-{}",
            std::process::id(),
            now_ms(),
            SCOPES.fetch_add(1, Ordering::Relaxed)
        );
        let scope = format!("board-bench-{unique}");
        std::fs::create_dir_all(&config.dir).map_err(|error| error.to_string())?;
        let path = config.dir.join(match backend {
            Backend::File => format!("{scope}.log"),
            _ => format!("{scope}.db"),
        });
        let file = match backend {
            Backend::File => Some(Arc::new(
                FileLog::open(
                    &path,
                    FileOptions {
                        scope: &scope,
                        sync: false,
                    },
                )
                .map_err(|error| error.to_string())?,
            )),
            _ => None,
        };
        Ok(Self {
            backend,
            scope,
            memory: Arc::new(MemoryLog::new()),
            file,
            path,
            target: config.target.clone(),
        })
    }

    /// A connection as one writer would hold it: the process's one memory or
    /// file host, or a new SQL connection.
    fn connect(&self) -> Result<Arc<dyn LogHost>, String> {
        Ok(match self.backend {
            Backend::Memory => Arc::clone(&self.memory) as Arc<dyn LogHost>,
            Backend::File => {
                Arc::clone(self.file.as_ref().ok_or("no file host")?) as Arc<dyn LogHost>
            }
            Backend::Sqlite => Arc::new(SqlLog::new(Arc::new(NodeSqlite::open(&self.path)?))),
            Backend::Postgres => {
                let options =
                    PgOptions::parse(&self.target).map_err(|error| format!("{error:?}"))?;
                let sql = PgSql::reading(PgPool::new(options.clone(), 1), PgPool::new(options, 1));
                Arc::new(SqlLog::new(Arc::new(sql)))
            }
        })
    }

    /// Appends rows straight to the log, in batches, starting at first.
    fn preload(
        &self,
        host: &Arc<dyn LogHost>,
        first: u64,
        bodies: &[String],
    ) -> Result<(), String> {
        let batch = if self.backend.local() { 10_000 } else { 1_000 };
        block(async {
            // The SQL hosts create their table on load.
            host.load(&self.scope)
                .await
                .map_err(|error| error.to_string())?;
            for (index, chunk) in bodies.chunks(batch).enumerate() {
                let start = first + (index * batch) as u64;
                let rows: Vec<(u64, String)> = chunk
                    .iter()
                    .enumerate()
                    .map(|(at, body)| (start + at as u64, body.clone()))
                    .collect();
                host.append(&self.scope, &rows)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.path);
        for suffix in ["-wal", "-shm", ".index"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
        if self.backend == Backend::Postgres {
            let _ = Command::new("psql")
                .arg(&self.target)
                .arg("-qc")
                .arg(format!(
                    "DELETE FROM nanocode_log WHERE scope = '{}'",
                    self.scope
                ))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

// ---------------------------------------------------------------- helpers

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

fn block<F: std::future::Future>(future: F) -> F::Output {
    nanocode_executor::Builder::new_current_thread()
        .build()
        .expect("a runtime")
        .block_on(future)
}

fn task(index: usize) -> NewTask {
    let lane = if index % 100 == 99 { "a" } else { "b" };
    NewTask {
        id: TaskId(format!("t{index:08}")),
        parent: None,
        spec: format!("do the thing number {index} and report back"),
        labels: BTreeMap::from([("lane".to_owned(), lane.to_owned())]),
        needs: Vec::new(),
    }
}

fn body(id: String, at_ms: u64, op: Op) -> String {
    serde_json::to_string(&Event {
        id: EventId(id),
        by: "seed".to_owned(),
        at_ms,
        op,
    })
    .expect("an event serializes")
}

/// done tasks that were posted, claimed and finished, then open posts.
fn history(open: usize, done: usize) -> Vec<String> {
    let mut rows = Vec::with_capacity(open + done * 3);
    for index in 0..done {
        let id = task(index).id;
        rows.push(body(format!("p{index}"), 1, Op::Post { task: task(index) }));
        rows.push(body(
            format!("c{index}"),
            2,
            Op::Claim {
                task: id.clone(),
                lease_ms: 60_000,
            },
        ));
        rows.push(body(
            format!("f{index}"),
            3,
            Op::Finish {
                task: id,
                claim: ClaimId(format!("c{index}")),
                outcome: Outcome::Done("ok".to_owned()),
            },
        ));
    }
    for index in done..done + open {
        rows.push(body(format!("p{index}"), 4, Op::Post { task: task(index) }));
    }
    rows
}

#[derive(Default)]
struct Samples(Vec<Duration>);

impl Samples {
    fn time<T>(&mut self, run: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let value = run();
        self.0.push(started.elapsed());
        value
    }

    fn quantile_us(&mut self, quantile: f64) -> f64 {
        if self.0.is_empty() {
            return 0.0;
        }
        self.0.sort();
        let at = ((self.0.len() - 1) as f64 * quantile).round() as usize;
        self.0[at].as_secs_f64() * 1e6
    }
}

struct Out {
    json: Option<std::fs::File>,
}

impl Out {
    fn emit(&mut self, backend: Backend, bench: &str, param: &str, values: &[(&str, f64)]) {
        let mut line = format!(
            "BENCH backend={} bench={bench} param={param}",
            backend.name()
        );
        let mut json =
            serde_json::json!({ "backend": backend.name(), "bench": bench, "param": param });
        for (key, value) in values {
            line.push_str(&format!(" {key}={}", round(*value)));
            json[*key] = serde_json::json!(round(*value));
        }
        println!("{line}");
        if let Some(file) = &mut self.json {
            let _ = writeln!(file, "{json}");
        }
    }
}

fn round(value: f64) -> f64 {
    if value.abs() >= 100.0 {
        value.round()
    } else {
        (value * 100.0).round() / 100.0
    }
}

// ---------------------------------------------------------------- benches

/// Racing writers: each writer posts its share of tasks through its own
/// board. The hand allows about 330 threads for everything, so writers run
/// on at most 8 OS threads, each owning one connection and taking its
/// writers' submits in turn: every board's fold goes stale behind the
/// others' writes, as concurrent writers' would.
fn submit(config: &Config, backend: Backend, out: &mut Out) -> Result<(), String> {
    let total = match (backend, config.quick) {
        (Backend::Memory, false) => 16_000,
        (Backend::File, false) => 8_000,
        (_, false) => 2_000,
        (_, true) => 512,
    };
    for writers in [1usize, 4, 16, 64] {
        let store = Store::new(config, backend)?;
        let each = (total / writers).max(8);
        let threads = writers.min(8);
        let barrier = Arc::new(Barrier::new(threads + 1));
        let hosts = (0..threads)
            .map(|_| store.connect().map(Counted::new))
            .collect::<Result<Vec<_>, _>>()?;
        block(store.connect()?.load(&store.scope)).map_err(|error| error.to_string())?;
        let handles: Vec<_> = hosts
            .iter()
            .enumerate()
            .map(|(thread, host)| {
                let host = Arc::clone(host) as Arc<dyn LogHost>;
                let barrier = Arc::clone(&barrier);
                let scope = store.scope.clone();
                let mine: Vec<usize> = (thread..writers).step_by(threads).collect();
                std::thread::spawn(move || {
                    let opened: Result<Vec<Board>, String> = mine
                        .iter()
                        .map(|writer| {
                            block(Board::open(
                                Arc::clone(&host),
                                &scope,
                                &format!("w{writer}"),
                            ))
                            .map_err(|error| error.to_string())
                        })
                        .collect();
                    barrier.wait();
                    let mut boards = opened?;
                    block(async move {
                        let mut samples = Samples::default();
                        for index in 0..each {
                            for (board, writer) in boards.iter_mut().zip(&mine) {
                                let mut new = task(index);
                                new.id = TaskId(format!("w{writer}-{index}"));
                                let started = Instant::now();
                                board
                                    .submit(now_ms(), Op::Post { task: new })
                                    .await
                                    .map_err(|error| error.to_string())?;
                                samples.0.push(started.elapsed());
                            }
                        }
                        Ok::<_, String>(samples)
                    })
                })
            })
            .collect();
        barrier.wait();
        let started = Instant::now();
        let mut all = Samples::default();
        for handle in handles {
            all.0
                .extend(handle.join().map_err(|_| "writer panicked")??.0);
        }
        let elapsed = started.elapsed().as_secs_f64();
        let ops = (each * writers) as f64;
        let appends: u64 = hosts
            .iter()
            .map(|host| host.appends.load(Ordering::Relaxed))
            .sum();
        let scans: u64 = hosts
            .iter()
            .map(|host| host.scans.load(Ordering::Relaxed))
            .sum();
        let failed: u64 = hosts
            .iter()
            .map(|host| host.failed.load(Ordering::Relaxed))
            .sum();
        out.emit(
            backend,
            "submit",
            &format!("writers={writers}"),
            &[
                ("ops_per_s", ops / elapsed),
                ("p50_us", all.quantile_us(0.5)),
                ("p99_us", all.quantile_us(0.99)),
                ("appends_per_op", appends as f64 / ops),
                ("failed_appends_per_op", failed as f64 / ops),
                ("scans_per_op", scans as f64 / ops),
            ],
        );
        drop(hosts);
        store.cleanup();
    }
    Ok(())
}

/// Claim-next: refresh, take the first ready task (any, or with lane=a, one
/// task in 100), claim it.
fn claim(config: &Config, backend: Backend, out: &mut Out) -> Result<(), String> {
    let mut cases = vec![
        (1_000usize, 0usize),
        (10_000, 0),
        (100_000, 0),
        (1_000, 30_000),
    ];
    if backend.local() && !config.quick {
        cases.push((1_000, 300_000));
    }
    if config.quick {
        cases.retain(|(open, done)| open + done * 3 <= 30_000);
    }
    for (open, done) in cases {
        let store = Store::new(config, backend)?;
        let host = store.connect()?;
        store.preload(&host, 1, &history(open, done))?;
        let reps = if backend.local() { 400 } else { 100 };
        let mut samples = [
            Samples::default(),
            Samples::default(),
            Samples::default(),
            Samples::default(),
        ];
        block(async {
            let mut board = Board::open(Arc::clone(&host), &store.scope, "claimer")
                .await
                .map_err(|error| error.to_string())?;
            for rep in 0..reps {
                let label = rep % 2;
                let started = Instant::now();
                board.refresh().await.map_err(|error| error.to_string())?;
                let picked = Instant::now();
                let next = if label == 1 {
                    ready_labelled(board.fold(), "lane", "a")
                } else {
                    board.fold().ready(now_ms()).next().cloned()
                };
                let pick = picked.elapsed();
                let Some(next) = next else { break };
                board
                    .submit(
                        now_ms(),
                        Op::Claim {
                            task: next,
                            lease_ms: 3_600_000,
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                samples[label].0.push(started.elapsed());
                samples[2 + label].0.push(pick);
            }
            Ok::<_, String>(())
        })?;
        let [mut any, mut labelled, mut pick, mut pick_label] = samples;
        out.emit(
            backend,
            "claim_next",
            &format!("open={open},done={done}"),
            &[
                ("p50_us", any.quantile_us(0.5)),
                ("p99_us", any.quantile_us(0.99)),
                ("pick_p50_us", pick.quantile_us(0.5)),
                ("label_p50_us", labelled.quantile_us(0.5)),
                ("label_p99_us", labelled.quantile_us(0.99)),
                ("label_pick_p50_us", pick_label.quantile_us(0.5)),
            ],
        );
        drop(host);
        store.cleanup();
    }
    Ok(())
}

/// The first ready task carrying label key=value.
// API-BEGIN
fn ready_labelled(fold: &Fold, key: &str, value: &str) -> Option<TaskId> {
    fold.ready_labelled(key, value, now_ms()).next().cloned()
}
// API-END

/// Board::open over a log of finished tasks (three rows each).
fn open(config: &Config, backend: Backend, out: &mut Out) -> Result<(), String> {
    let sizes: &[usize] = if config.quick {
        &[10_000, 30_000]
    } else {
        &[10_000, 100_000, 1_000_000]
    };
    for &rows in sizes.iter().filter(|rows| **rows <= backend.max_rows()) {
        let store = Store::new(config, backend)?;
        let host = store.connect()?;
        store.preload(&host, 1, &history(0, rows / 3))?;
        let reps = if rows >= 1_000_000 { 2 } else { 3 };
        let (mut opened, mut loaded, mut parsed, mut resumed) = (
            Samples::default(),
            Samples::default(),
            Samples::default(),
            Samples::default(),
        );
        for rep in 0..reps {
            let host = store.connect()?;
            let raw = loaded
                .time(|| block(host.load(&store.scope)))
                .map_err(|error| error.to_string())?;
            parsed.time(|| {
                raw.iter()
                    .filter(|(_, body)| serde_json::from_str::<Event>(body).is_ok())
                    .count()
            });
            drop(raw);
            let board = opened
                .time(|| block(Board::open(Arc::clone(&host), &store.scope, "reader")))
                .map_err(|error| error.to_string())?;
            let started = Instant::now();
            let again = resume(&host, &store.scope, &format!("again{rep}"), board.fold())?;
            resumed.0.push(started.elapsed());
            drop((board, again));
        }
        let open_ms = opened.quantile_us(0.5) / 1e3;
        let load_ms = loaded.quantile_us(0.5) / 1e3;
        out.emit(
            backend,
            "open",
            &format!("rows={rows}"),
            &[
                ("open_ms", open_ms),
                ("load_ms", load_ms),
                ("parse_ms", parsed.quantile_us(0.5) / 1e3),
                ("fold_ms", open_ms - load_ms),
                ("us_per_row", open_ms * 1e3 / rows as f64),
                ("resume_ms", resumed.quantile_us(0.5) / 1e3),
            ],
        );
        drop(host);
        store.cleanup();
    }
    Ok(())
}

/// Reopens from a checkpoint: a clone of a fold this process holds, then
/// the rows after it (none here). Without Board::resume it refolds.
// API3-BEGIN
fn resume(
    host: &Arc<dyn LogHost>,
    scope: &str,
    writer: &str,
    fold: &Fold,
) -> Result<Board, String> {
    block(Board::resume(Arc::clone(host), scope, writer, fold.clone()))
        .map_err(|error| error.to_string())
}
// API3-END

/// Refresh after another writer appends k rows, on a log of 100K rows.
fn refresh(config: &Config, backend: Backend, out: &mut Out) -> Result<(), String> {
    let base = if config.quick { 10_000 } else { 100_000 };
    let store = Store::new(config, backend)?;
    let host = store.connect()?;
    let open_tasks = base / 3;
    store.preload(&host, 1, &history(open_tasks, (base - open_tasks) / 3))?;
    let other = store.connect()?;
    block(async {
        let mut board = Board::open(Arc::clone(&host), &store.scope, "reader")
            .await
            .map_err(|error| error.to_string())?;
        let mut next = board.fold().next();
        for k in [1usize, 100] {
            let mut samples = Samples::default();
            let reps = if k == 1 { 200 } else { 40 };
            for rep in 0..reps {
                let rows: Vec<(u64, String)> = (0..k)
                    .map(|at| {
                        let seq = next + at as u64;
                        (
                            seq,
                            body(
                                format!("r{seq}"),
                                5,
                                Op::Reply {
                                    task: task(rep % open_tasks).id,
                                    body: "progress".to_owned(),
                                },
                            ),
                        )
                    })
                    .collect();
                other
                    .append(&store.scope, &rows)
                    .await
                    .map_err(|error| error.to_string())?;
                next += k as u64;
                let started = Instant::now();
                board.refresh().await.map_err(|error| error.to_string())?;
                samples.0.push(started.elapsed());
            }
            let p50 = samples.quantile_us(0.5);
            out.emit(
                backend,
                "refresh",
                &format!("rows={base},new={k}"),
                &[
                    ("p50_us", p50),
                    ("p99_us", samples.quantile_us(0.99)),
                    ("us_per_new_row", p50 / k as f64),
                ],
            );
        }
        Ok::<_, String>(())
    })?;
    drop((host, other));
    store.cleanup();
    Ok(())
}

/// Live heap a fold holds per task, by what it went through.
fn memory(config: &Config, out: &mut Out) -> Result<(), String> {
    let tasks = if config.quick { 10_000 } else { 100_000 };
    for (name, open, done) in [("open", tasks, 0), ("done", 0, tasks)] {
        let rows = history(open, done);
        let before = resident();
        let mut fold = Fold::new();
        for (index, body) in rows.iter().enumerate() {
            fold.apply(index as u64 + 1, body)
                .map_err(|error| error.to_string())?;
        }
        let after = resident();
        let row_bytes = rows.iter().map(String::len).sum::<usize>() as f64 / tasks as f64;
        out.emit(
            Backend::Memory,
            "memory",
            &format!("tasks={tasks},state={name}"),
            &[
                ("bytes_per_task", (after - before) as f64 / tasks as f64),
                ("log_bytes_per_task", row_bytes),
            ],
        );
        drop(fold);
    }
    Ok(())
}

/// Keyed submits: a fresh key, the same key again (dedup), and an unkeyed
/// submit for comparison, on a log of 10K rows.
fn keyed(config: &Config, backend: Backend, out: &mut Out) -> Result<(), String> {
    let store = Store::new(config, backend)?;
    let host = store.connect()?;
    store.preload(&host, 1, &history(10_000, 0))?;
    let reps = if backend.local() { 500 } else { 100 };
    let (mut fresh, mut again, mut plain) =
        (Samples::default(), Samples::default(), Samples::default());
    block(async {
        let mut board = Board::open(Arc::clone(&host), &store.scope, "keyed")
            .await
            .map_err(|error| error.to_string())?;
        let reply = |rep: usize| Op::Reply {
            task: task(rep).id,
            body: "noted".to_owned(),
        };
        for rep in 0..reps {
            let key = format!("hook:{rep}");
            let started = Instant::now();
            board
                .submit_keyed(&key, now_ms(), reply(rep))
                .await
                .map_err(|error| error.to_string())?;
            fresh.0.push(started.elapsed());
            let started = Instant::now();
            board
                .submit_keyed(&key, now_ms(), reply(rep))
                .await
                .map_err(|error| error.to_string())?;
            again.0.push(started.elapsed());
            let started = Instant::now();
            board
                .submit(now_ms(), reply(rep))
                .await
                .map_err(|error| error.to_string())?;
            plain.0.push(started.elapsed());
        }
        Ok::<_, String>(())
    })?;
    out.emit(
        backend,
        "keyed",
        "rows=10000",
        &[
            ("fresh_p50_us", fresh.quantile_us(0.5)),
            ("dedup_p50_us", again.quantile_us(0.5)),
            ("dedup_p99_us", again.quantile_us(0.99)),
            ("plain_p50_us", plain.quantile_us(0.5)),
        ],
    );
    drop(host);
    store.cleanup();
    Ok(())
}

/// A watcher at cursor next - k reading the entries it has not seen, out of
/// a fold of 300K entries: the fold-side cost, independent of the host.
fn catchup(config: &Config, out: &mut Out) -> Result<(), String> {
    let tasks = if config.quick { 10_000 } else { 100_000 };
    let rows = history(0, tasks);
    let mut fold = Fold::new();
    for (index, body) in rows.iter().enumerate() {
        fold.apply(index as u64 + 1, body)
            .map_err(|error| error.to_string())?;
    }
    let total = fold.entries().len();
    for k in [1usize, 100, 10_000] {
        let cursor = fold.next() - k as u64;
        let mut samples = Samples::default();
        let mut seen = 0;
        for _ in 0..200 {
            seen = samples.time(|| entries_after(&fold, cursor));
        }
        out.emit(
            Backend::Memory,
            "catchup",
            &format!("entries={total},behind={k}"),
            &[
                ("p50_us", samples.quantile_us(0.5)),
                ("p99_us", samples.quantile_us(0.99)),
                ("read", seen as f64),
            ],
        );
    }
    Ok(())
}

/// How many entries at or after its cursor a watcher finds.
// API2-BEGIN
fn entries_after(fold: &Fold, cursor: u64) -> usize {
    std::hint::black_box(fold.entries_after(cursor).len())
}
// API2-END

// ---------------------------------------------------------------- main

fn main() -> ExitCode {
    let config = match parse() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let mut out = Out {
        json: config.json.as_ref().and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        }),
    };
    let mut failed = false;
    let mut run = |name: &str, result: Result<(), String>| {
        if let Err(error) = result {
            eprintln!("ERROR {name}: {error}");
            failed = true;
        }
    };
    if config.wants("memory") {
        run("memory", memory(&config, &mut out));
    }
    if config.wants("catchup") {
        run("catchup", catchup(&config, &mut out));
    }
    for &backend in &config.backends {
        if config.wants("submit") {
            run("submit", submit(&config, backend, &mut out));
        }
        if config.wants("claim") {
            run("claim", claim(&config, backend, &mut out));
        }
        if config.wants("open") {
            run("open", open(&config, backend, &mut out));
        }
        if config.wants("refresh") {
            run("refresh", refresh(&config, backend, &mut out));
        }
        if config.wants("keyed") {
            run("keyed", keyed(&config, backend, &mut out));
        }
    }
    let _ = std::fs::remove_dir_all(&config.dir);
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
