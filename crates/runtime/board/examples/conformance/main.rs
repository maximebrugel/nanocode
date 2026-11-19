//! Board conformance: one multi-writer scenario run against every turn log
//! host nanocode has (memory, file, SQLite, Postgres), checked against an
//! independent model of the raw log.
//!
//! cargo run --release --offline -p nanocode-board --example board_conformance -- \
//!     --backend memory|file|sqlite|postgres [--writers 8] [--procs 0] [--tasks 24] \
//!     [--lease-ms 40] [--seed 1] [--runs 1] [--target PATH_OR_URL]
//!
//! --writers is the number of racing Board instances (threads) per process,
//! --procs > 0 runs that many child processes of writers on the same store.
//! SQLite runs through node:sqlite (one node process per connection), the
//! engine nanocode-ts hosts the SQLite dialect on.

use std::{
    collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, Write as _},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use nanocode_board::{
    Board, BoardError, Change, ClaimId, Event, Fold, NewTask, Op, Outcome, Phase, Rejection, TaskId,
};
use nanocode_durability_core::{Sql, SqlDialect, SqlRow, SqlStatement, StoreError, StoreFuture};
use nanocode_store_sql::{PgOptions, PgPool, PgSql};
use nanocode_turnlog::{FileLog, FileOptions, LogHost, MemoryLog, SqlLog};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Memory,
    File,
    Sqlite,
    Postgres,
}

#[derive(Clone, Debug)]
struct Config {
    backend: Backend,
    writers: usize,
    procs: usize,
    tasks: usize,
    lease_ms: u64,
    seed: u64,
    runs: usize,
    target: String,
    scope: String,
    child: Option<usize>,
    reopens: usize,
}

fn usage() -> String {
    "usage: board_conformance --backend memory|file|sqlite|postgres [--writers N] [--procs N] \
     [--tasks N] [--lease-ms N] [--seed N] [--runs N] [--target PATH_OR_URL]"
        .to_owned()
}

fn parse() -> Result<Config, String> {
    let mut config = Config {
        backend: Backend::Memory,
        writers: 8,
        procs: 0,
        tasks: 24,
        lease_ms: 40,
        seed: 1,
        runs: 1,
        target: String::new(),
        scope: String::new(),
        child: None,
        reopens: 3,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} needs a value\n{}", usage()))
        };
        let number = |text: String| {
            text.parse::<u64>()
                .map_err(|error| format!("{text}: {error}"))
        };
        match flag.as_str() {
            "--backend" => {
                config.backend = match value()?.as_str() {
                    "memory" => Backend::Memory,
                    "file" => Backend::File,
                    "sqlite" => Backend::Sqlite,
                    "postgres" => Backend::Postgres,
                    other => return Err(format!("unknown backend {other}\n{}", usage())),
                }
            }
            "--writers" => config.writers = number(value()?)? as usize,
            "--procs" => config.procs = number(value()?)? as usize,
            "--tasks" => config.tasks = number(value()?)? as usize,
            "--lease-ms" => config.lease_ms = number(value()?)?,
            "--seed" => config.seed = number(value()?)?,
            "--runs" => config.runs = number(value()?)? as usize,
            "--reopens" => config.reopens = number(value()?)? as usize,
            "--target" => config.target = value()?,
            "--scope" => config.scope = value()?,
            "--child" => config.child = Some(number(value()?)? as usize),
            _ => return Err(usage()),
        }
    }
    if config.backend == Backend::Memory && config.procs > 0 {
        return Err(
            "the memory host lives in one process: --procs needs file, sqlite or postgres"
                .to_owned(),
        );
    }
    Ok(config)
}

// ---------------------------------------------------------------- SQLite

/// A SQLite connection: a node:sqlite process that runs each statement list
/// in one BEGIN IMMEDIATE transaction.
struct NodeSqlite {
    connection: Mutex<(Child, ChildStdin, BufReader<ChildStdout>)>,
}

#[derive(Deserialize)]
struct Answer {
    ok: Option<Vec<Vec<SqlRow>>>,
    err: Option<String>,
}

impl NodeSqlite {
    fn open(path: &str) -> Result<Self, String> {
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

/// Opens a new connection to the store (a new handle, as a new process would).
fn connect(config: &Config, memory: &Arc<MemoryLog>) -> Result<Arc<dyn LogHost>, String> {
    Ok(match config.backend {
        Backend::Memory => Arc::clone(memory) as Arc<dyn LogHost>,
        Backend::File => Arc::new(
            FileLog::open(
                Path::new(&config.target),
                FileOptions {
                    scope: &config.scope,
                    sync: false,
                },
            )
            .map_err(|error| error.to_string())?,
        ),
        Backend::Sqlite => Arc::new(SqlLog::new(Arc::new(NodeSqlite::open(&config.target)?))),
        Backend::Postgres => {
            let options = PgOptions::parse(&config.target).map_err(|error| format!("{error:?}"))?;
            let sql = PgSql::reading(PgPool::new(options.clone(), 1), PgPool::new(options, 1));
            Arc::new(SqlLog::new(Arc::new(sql)))
        }
    })
}

/// The handle each writer thread of one process uses: the file host is one
/// instance per process (it serializes its writes on one thread), the SQL
/// hosts are one connection per writer.
fn writer_hosts(config: &Config, memory: &Arc<MemoryLog>) -> Result<Vec<Arc<dyn LogHost>>, String> {
    match config.backend {
        Backend::Memory | Backend::File => {
            let host = connect(config, memory)?;
            Ok((0..config.writers).map(|_| Arc::clone(&host)).collect())
        }
        Backend::Sqlite | Backend::Postgres => (0..config.writers)
            .map(|_| connect(config, memory))
            .collect(),
    }
}

// ---------------------------------------------------------------- workers

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64, name: &str) -> Self {
        let mut hasher = DefaultHasher::new();
        (seed, name).hash(&mut hasher);
        Self(hasher.finish() | 1)
    }

    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Report {
    writer: String,
    claims: Vec<String>,
    claim_rejected: u64,
    finishes: Vec<String>,
    stale_finish_rejected: u64,
    renews: u64,
    renew_rejected: u64,
    releases: u64,
    stalls: u64,
    keys: BTreeSet<String>,
    deliveries: u64,
    errors: u64,
    error_samples: Vec<String>,
    open_retries: u64,
    timed_out: bool,
    fold: String,
}

impl Report {
    fn note(&mut self, error: &BoardError) {
        self.errors += 1;
        if self.error_samples.len() < 4 {
            self.error_samples.push(error.to_string());
        }
    }
}

fn digest(fold: &Fold) -> String {
    let mut hasher = DefaultHasher::new();
    format!("{fold:?}").hash(&mut hasher);
    format!("{:016x}/{}", hasher.finish(), fold.next())
}

async fn open_board(
    host: &Arc<dyn LogHost>,
    scope: &str,
    writer: &str,
    report: &mut Report,
) -> Result<Board, String> {
    let mut last = String::new();
    for _ in 0..100 {
        match Board::open(Arc::clone(host), scope, writer).await {
            Ok(board) => return Ok(board),
            Err(error) => {
                report.open_retries += 1;
                last = error.to_string();
                nanocode_executor::sleep(Duration::from_millis(10)).await;
            }
        }
    }
    Err(format!("{writer} could not open the board: {last}"))
}

async fn worker(
    config: Config,
    host: Arc<dyn LogHost>,
    writer: String,
    deadline: Instant,
) -> Report {
    let mut report = Report {
        writer: writer.clone(),
        ..Report::default()
    };
    let mut rng = Rng::new(config.seed, &writer);
    let lease = config.lease_ms;
    let sleep = |ms: u64| nanocode_executor::sleep(Duration::from_millis(ms));
    let mut board = match open_board(&host, &config.scope, &writer, &mut report).await {
        Ok(board) => board,
        Err(error) => {
            report.error_samples.push(error);
            report.errors += 1;
            return report;
        }
    };
    loop {
        if Instant::now() > deadline {
            report.timed_out = true;
            break;
        }
        if let Err(error) = board.refresh().await {
            report.note(&error);
            sleep(5).await;
            continue;
        }
        if board.fold().tasks().len() >= config.tasks && board.fold().settled() {
            break;
        }
        let ready: Vec<TaskId> = board.fold().ready(now_ms()).cloned().collect();
        if ready.is_empty() {
            sleep(2).await;
            continue;
        }
        // Mostly the first ready task, so writers collide on it.
        let task = if rng.below(4) == 0 {
            ready[rng.below(ready.len())].clone()
        } else {
            ready[0].clone()
        };
        let claim = match board
            .submit(
                now_ms(),
                Op::Claim {
                    task: task.clone(),
                    lease_ms: lease,
                },
            )
            .await
        {
            Ok(applied) => {
                report.claims.push(applied.event.0.clone());
                ClaimId(applied.event.0)
            }
            Err(BoardError::Rejected(_)) => {
                report.claim_rejected += 1;
                continue;
            }
            Err(error) => {
                report.note(&error);
                continue;
            }
        };
        let mut held = true;
        match rng.below(10) {
            // Stall past the lease: another writer may take the task over,
            // and the finish below must then be fenced.
            0 | 1 => {
                report.stalls += 1;
                sleep(lease * 2 + 5).await;
            }
            2 => {
                match board
                    .submit(
                        now_ms(),
                        Op::Release {
                            task: task.clone(),
                            claim: claim.clone(),
                            note: "later".into(),
                        },
                    )
                    .await
                {
                    Ok(_) => report.releases += 1,
                    Err(BoardError::Rejected(_)) => {}
                    Err(error) => report.note(&error),
                }
                held = false;
            }
            _ => {
                for _ in 0..rng.below(4) {
                    sleep(lease / 3).await;
                    match board
                        .submit(
                            now_ms(),
                            Op::Renew {
                                task: task.clone(),
                                claim: claim.clone(),
                                lease_ms: lease,
                            },
                        )
                        .await
                    {
                        Ok(_) => report.renews += 1,
                        Err(BoardError::Rejected(_)) => {
                            report.renew_rejected += 1;
                            break;
                        }
                        Err(error) => report.note(&error),
                    }
                }
            }
        }
        if held {
            let outcome = Outcome::Done(format!("{task} by {writer}"));
            match board
                .submit(
                    now_ms(),
                    Op::Finish {
                        task: task.clone(),
                        claim: claim.clone(),
                        outcome,
                    },
                )
                .await
            {
                Ok(applied) => report.finishes.push(applied.event.0),
                Err(BoardError::Rejected(Rejection::NotHolder | Rejection::Terminal)) => {
                    report.stale_finish_rejected += 1;
                }
                Err(error) => report.note(&error),
            }
        }
        // A keyed reply, as a webhook delivers it: every writer that touched
        // the task sends the same key, two or three times.
        let key = format!("review:{task}");
        for _ in 0..2 + rng.below(2) {
            report.deliveries += 1;
            match board
                .submit_keyed(
                    &key,
                    now_ms(),
                    Op::Reply {
                        task: task.clone(),
                        body: format!("review of {task}"),
                    },
                )
                .await
            {
                Ok(_) | Err(BoardError::Rejected(_)) => {}
                Err(error) => report.note(&error),
            }
        }
        report.keys.insert(key);
    }
    for _ in 0..20 {
        match board.refresh().await {
            Ok(()) => break,
            Err(error) => report.note(&error),
        }
    }
    report.fold = digest(board.fold());
    report
}

fn run_writers(
    config: &Config,
    memory: &Arc<MemoryLog>,
    prefix: &str,
) -> Result<Vec<Report>, String> {
    let hosts = writer_hosts(config, memory)?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let threads: Vec<_> = hosts
        .into_iter()
        .enumerate()
        .map(|(index, host)| {
            let config = config.clone();
            let writer = format!("{prefix}w{index}");
            std::thread::spawn(move || {
                let runtime = nanocode_executor::Builder::new_current_thread()
                    .build()
                    .expect("a runtime");
                runtime.block_on(worker(config, host, writer, deadline))
            })
        })
        .collect();
    threads
        .into_iter()
        .map(|thread| thread.join().map_err(|_| "a writer panicked".to_owned()))
        .collect()
}

// ---------------------------------------------------------------- lead

fn plan(config: &Config) -> Vec<NewTask> {
    (0..config.tasks)
        .map(|index| NewTask {
            id: TaskId(format!("t{index:03}")),
            parent: (index % 7 == 6).then(|| TaskId(format!("t{:03}", index - 1))),
            spec: format!("task {index}"),
            labels: BTreeMap::new(),
            needs: if index % 3 == 2 {
                vec![
                    TaskId(format!("t{:03}", index - 1)),
                    TaskId(format!("t{:03}", index - 2)),
                ]
            } else {
                Vec::new()
            },
        })
        .collect()
}

async fn lead(config: &Config, host: Arc<dyn LogHost>) -> Result<(), String> {
    let mut report = Report::default();
    let mut board = open_board(&host, &config.scope, "lead", &mut report).await?;
    for task in plan(config) {
        let key = format!("post:{}", task.id);
        // Delivered twice: the second must return the first's seq.
        let first = board
            .submit_keyed(&key, now_ms(), Op::Post { task: task.clone() })
            .await
            .map_err(|error| error.to_string())?;
        let again = board
            .submit_keyed(&key, now_ms(), Op::Post { task })
            .await
            .map_err(|error| error.to_string())?;
        if first != again {
            return Err(format!(
                "keyed post {key} applied twice: {first:?} then {again:?}"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- model

#[derive(Clone, Debug, PartialEq, Eq)]
enum Held {
    Open,
    Claimed { claim: ClaimId, until: u64 },
    Done,
}

/// An independent replay of the raw rows under the board's rules for the ops
/// this scenario writes, returning each event's verdict.
fn model(
    rows: &[(u64, String)],
    failures: &mut Vec<String>,
) -> (
    BTreeMap<String, bool>,
    BTreeMap<TaskId, Held>,
    BTreeMap<String, usize>,
) {
    let mut verdicts = BTreeMap::new();
    let mut tasks: BTreeMap<TaskId, (Held, Vec<TaskId>)> = BTreeMap::new();
    let mut landed: BTreeMap<String, usize> = BTreeMap::new();
    let mut now = 0;
    for (index, (seq, body)) in rows.iter().enumerate() {
        if *seq != index as u64 + 1 {
            failures.push(format!("row {index} has seq {seq}"));
        }
        let event: Event = match serde_json::from_str(body) {
            Ok(event) => event,
            Err(error) => {
                failures.push(format!("row {seq} is not an event: {error}"));
                continue;
            }
        };
        *landed.entry(event.id.0.clone()).or_default() += 1;
        if verdicts.contains_key(&event.id.0) {
            continue;
        }
        now = now.max(event.at_ms);
        let accepted = match &event.op {
            Op::Post { task } => {
                let fresh = !tasks.contains_key(&task.id)
                    && task
                        .parent
                        .iter()
                        .chain(&task.needs)
                        .all(|above| tasks.contains_key(above));
                if fresh {
                    tasks.insert(task.id.clone(), (Held::Open, task.needs.clone()));
                }
                fresh
            }
            Op::Claim { task, lease_ms } => {
                let free = match tasks.get(task) {
                    Some((Held::Open, needs)) => needs.clone(),
                    Some((Held::Claimed { until, .. }, needs)) if *until <= now => needs.clone(),
                    _ => vec![TaskId(String::new())],
                };
                let ok = free
                    .iter()
                    .all(|need| matches!(tasks.get(need), Some((Held::Done, _))));
                if ok {
                    tasks.get_mut(task).expect("claimed task").0 = Held::Claimed {
                        claim: ClaimId(event.id.0.clone()),
                        until: now + lease_ms,
                    };
                }
                ok
            }
            Op::Renew {
                task,
                claim,
                lease_ms,
            } => match tasks.get_mut(task) {
                Some((
                    Held::Claimed {
                        claim: current,
                        until,
                    },
                    _,
                )) if current == claim => {
                    *until = now + lease_ms;
                    true
                }
                _ => false,
            },
            Op::Release { task, claim, .. } | Op::Finish { task, claim, .. } => {
                match tasks.get_mut(task) {
                    Some((held, _)) if matches!(&*held, Held::Claimed { claim: current, .. } if current == claim) =>
                    {
                        *held = if matches!(event.op, Op::Release { .. }) {
                            Held::Open
                        } else {
                            Held::Done
                        };
                        true
                    }
                    _ => false,
                }
            }
            Op::Reply { task, .. } => tasks.contains_key(task),
            Op::Cancel { .. } => {
                failures.push(format!("row {seq}: this scenario writes no cancel"));
                false
            }
        };
        verdicts.insert(event.id.0.clone(), accepted);
    }
    (
        verdicts,
        tasks
            .into_iter()
            .map(|(id, (held, _))| (id, held))
            .collect(),
        landed,
    )
}

fn verify(
    config: &Config,
    rows: &[(u64, String)],
    fold: &Fold,
    reports: &[Report],
) -> (Vec<String>, BTreeMap<&'static str, u64>) {
    let mut failures = Vec::new();
    let mut stats = BTreeMap::new();
    let (verdicts, held, landed) = model(rows, &mut failures);
    // The fold agrees with the model on every event and every task.
    for (id, accepted) in &verdicts {
        let folded = fold
            .outcome(&nanocode_board::EventId(id.clone()))
            .map(Result::is_ok);
        if folded != Some(*accepted) {
            failures.push(format!("event {id}: model {accepted}, fold {folded:?}"));
        }
    }
    for (id, task) in fold.tasks() {
        let phase = match &task.phase {
            Phase::Open => Held::Open,
            Phase::Claimed {
                claim, until_ms, ..
            } => Held::Claimed {
                claim: claim.clone(),
                until: *until_ms,
            },
            Phase::Done { .. } => Held::Done,
            other => {
                failures.push(format!("task {id} ended {other:?}"));
                continue;
            }
        };
        if held.get(id) != Some(&phase) {
            failures.push(format!(
                "task {id}: model {:?}, fold {phase:?}",
                held.get(id)
            ));
        }
    }
    if fold.tasks().len() != config.tasks || !fold.settled() {
        failures.push(format!(
            "{} of {} tasks, settled {}",
            fold.tasks().len(),
            config.tasks,
            fold.settled()
        ));
    }
    // At most one live claim per task, and every finish and renew from it.
    let mut live: BTreeMap<TaskId, (ClaimId, u64)> = BTreeMap::new();
    let mut finished: BTreeMap<TaskId, u64> = BTreeMap::new();
    for entry in fold.entries() {
        match &entry.change {
            Change::Claimed {
                claim,
                until_ms,
                took_over,
                ..
            } => {
                if let Some((previous, until)) = live.get(&entry.task) {
                    if *until > entry.at_ms {
                        failures.push(format!(
                            "{} claimed by {claim} at {} while {previous} held it to {until}",
                            entry.task, entry.at_ms
                        ));
                    }
                    if took_over.as_ref() != Some(previous) {
                        failures.push(format!(
                            "{} took over {took_over:?}, live was {previous}",
                            entry.task
                        ));
                    }
                    *stats.entry("takeovers").or_default() += 1;
                } else if took_over.is_some() {
                    failures.push(format!(
                        "{} took over {took_over:?} with no live claim",
                        entry.task
                    ));
                }
                *stats.entry("claims").or_default() += 1;
                live.insert(entry.task.clone(), (claim.clone(), *until_ms));
            }
            Change::Renewed { claim, until_ms } => match live.get_mut(&entry.task) {
                Some((current, until)) if current == claim => *until = *until_ms,
                _ => failures.push(format!(
                    "{} renewed by {claim}, not its live claim",
                    entry.task
                )),
            },
            Change::Released { claim, .. } => {
                if live.remove(&entry.task).map(|(current, _)| current) != Some(claim.clone()) {
                    failures.push(format!(
                        "{} released by {claim}, not its live claim",
                        entry.task
                    ));
                }
            }
            Change::Finished(_) => {
                let raw = rows.iter().find_map(|(_, body)| {
                    serde_json::from_str::<Event>(body)
                        .ok()
                        .filter(|event| event.id == entry.event)
                });
                let claim = match raw.map(|event| event.op) {
                    Some(Op::Finish { claim, .. }) => claim,
                    other => {
                        failures.push(format!("finish {} is {other:?}", entry.event));
                        continue;
                    }
                };
                if live.remove(&entry.task).map(|(current, _)| current) != Some(claim.clone()) {
                    failures.push(format!(
                        "{} finished by {claim}, not its live claim",
                        entry.task
                    ));
                }
                *finished.entry(entry.task.clone()).or_default() += 1;
            }
            Change::Replied { .. } | Change::Posted => {}
            Change::Cancelled { .. } => failures.push(format!("{} cancelled", entry.task)),
        }
    }
    if let Some((task, count)) = finished.iter().find(|(_, count)| **count != 1) {
        failures.push(format!("{task} finished {count} times"));
    }
    // What writers were told matches the log.
    let applied: BTreeSet<&str> = fold
        .entries()
        .iter()
        .map(|entry| entry.event.0.as_str())
        .collect();
    let mut keys = BTreeSet::new();
    for report in reports {
        for id in report.claims.iter().chain(&report.finishes) {
            if !applied.contains(id.as_str()) {
                failures.push(format!(
                    "{} was told {id} applied, the log disagrees",
                    report.writer
                ));
            }
        }
        keys.extend(report.keys.iter().cloned());
        *stats.entry("deliveries").or_default() += report.deliveries;
        *stats.entry("stale_finishes_fenced").or_default() += report.stale_finish_rejected;
        *stats.entry("renews").or_default() += report.renews;
        *stats.entry("renews_fenced").or_default() += report.renew_rejected;
        *stats.entry("releases").or_default() += report.releases;
        *stats.entry("stalls").or_default() += report.stalls;
        *stats.entry("claims_rejected").or_default() += report.claim_rejected;
        *stats.entry("store_errors").or_default() += report.errors;
        *stats.entry("open_retries").or_default() += report.open_retries;
        if report.timed_out {
            failures.push(format!("{} timed out", report.writer));
        }
        if !report.error_samples.is_empty() {
            eprintln!("  {} errors: {:?}", report.writer, report.error_samples);
        }
    }
    // Each keyed event applied once, however often it landed.
    for task in plan(config) {
        keys.insert(format!("post:{}", task.id));
    }
    for key in &keys {
        let count = fold
            .entries()
            .iter()
            .filter(|entry| &entry.event.0 == key)
            .count();
        if count != 1 {
            failures.push(format!("key {key} applied {count} times"));
        }
        let rows = landed.get(key).copied().unwrap_or(0);
        *stats.entry("keyed_rows_deduped").or_default() += rows.saturating_sub(1) as u64;
    }
    for (id, task) in fold.tasks() {
        let body = format!("review of {id}");
        let replies = task
            .replies
            .iter()
            .filter(|(_, text)| *text == body)
            .count();
        if replies > 1 {
            failures.push(format!("{id} holds {replies} copies of its keyed reply"));
        }
    }
    *stats.entry("keys").or_default() += keys.len() as u64;
    *stats.entry("rows").or_default() += rows.len() as u64;
    // Every writer's own fold, once caught up, equals the reference.
    let reference = digest(fold);
    for report in reports {
        if report.fold != reference {
            failures.push(format!(
                "{} folded {}, reference {reference}",
                report.writer, report.fold
            ));
        }
    }
    (failures, stats)
}

// ---------------------------------------------------------------- driver

fn block<F: std::future::Future>(future: F) -> F::Output {
    nanocode_executor::Builder::new_current_thread()
        .build()
        .expect("a runtime")
        .block_on(future)
}

fn child(config: &Config, index: usize) -> ExitCode {
    let memory = Arc::new(MemoryLog::new());
    match run_writers(config, &memory, &format!("p{index}")) {
        Ok(reports) => {
            println!(
                "REPORT {}",
                serde_json::to_string(&reports).expect("reports serialize")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("child {index}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn spawn_children(config: &Config) -> Result<Vec<Report>, String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let backend = match config.backend {
        Backend::Memory => "memory",
        Backend::File => "file",
        Backend::Sqlite => "sqlite",
        Backend::Postgres => "postgres",
    };
    let children: Vec<Child> = (0..config.procs)
        .map(|index| {
            Command::new(&exe)
                .args([
                    "--backend",
                    backend,
                    "--target",
                    &config.target,
                    "--scope",
                    &config.scope,
                ])
                .args([
                    "--writers",
                    &config.writers.to_string(),
                    "--tasks",
                    &config.tasks.to_string(),
                ])
                .args([
                    "--lease-ms",
                    &config.lease_ms.to_string(),
                    "--seed",
                    &config.seed.to_string(),
                ])
                .args(["--child", &index.to_string()])
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|error| error.to_string())
        })
        .collect::<Result<_, _>>()?;
    let mut reports = Vec::new();
    for (index, child) in children.into_iter().enumerate() {
        let output = child
            .wait_with_output()
            .map_err(|error| error.to_string())?;
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text
            .lines()
            .find_map(|line| line.strip_prefix("REPORT "))
            .ok_or_else(|| format!("child {index} exited {} with no report", output.status))?;
        reports
            .extend(serde_json::from_str::<Vec<Report>>(line).map_err(|error| error.to_string())?);
    }
    Ok(reports)
}

fn run(base: &Config, run: usize) -> Result<(Vec<String>, BTreeMap<&'static str, u64>), String> {
    let mut config = base.clone();
    config.seed = base.seed + run as u64;
    let unique = format!("{}-{}-{}", std::process::id(), now_ms(), run);
    config.scope = format!("board-conformance-{unique}");
    let mut temp = None;
    match config.backend {
        Backend::Memory => {}
        Backend::File | Backend::Sqlite if base.target.is_empty() || base.runs > 1 => {
            let dir = std::env::temp_dir().join(format!("board-conformance-{unique}"));
            std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            let name = if config.backend == Backend::File {
                "board.log"
            } else {
                "board.db"
            };
            config.target = dir.join(name).display().to_string();
            temp = Some(dir);
        }
        Backend::File | Backend::Sqlite => {}
        Backend::Postgres if config.target.is_empty() => {
            config.target =
                "postgres://postgres@127.0.0.1:55432/postgres?sslmode=disable".to_owned();
        }
        Backend::Postgres => {}
    }
    let memory = Arc::new(MemoryLog::new());
    let started = Instant::now();
    {
        let host = connect(&config, &memory)?;
        block(lead(&config, host))?;
    }
    let reports = if config.procs > 0 {
        spawn_children(&config)?
    } else {
        run_writers(&config, &memory, "")?
    };
    let elapsed = started.elapsed();
    // Reopen from scratch: a new connection and a new board, several times.
    let mut folds = Vec::new();
    let mut rows = Vec::new();
    for _ in 0..config.reopens.max(1) {
        let host = connect(&config, &memory)?;
        let mut report = Report::default();
        let (board, raw) = block(async {
            let board = open_board(&host, &config.scope, "refold", &mut report).await?;
            let raw = host
                .scan(&config.scope, 1, i64::MAX as u64)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((board, raw))
        })?;
        folds.push(board.fold().clone());
        rows = raw;
    }
    let (mut failures, mut stats) = verify(&config, &rows, &folds[0], &reports);
    if folds.windows(2).any(|pair| pair[0] != pair[1]) {
        failures.push("refolds from scratch differ".to_owned());
    }
    stats.insert("refolds", folds.len() as u64);
    stats.insert("writers", reports.len() as u64);
    stats.insert("ms", elapsed.as_millis() as u64);
    if let Some(dir) = temp
        && failures.is_empty()
    {
        let _ = std::fs::remove_dir_all(dir);
    }
    Ok((failures, stats))
}

fn main() -> ExitCode {
    let config = match parse() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    if let Some(index) = config.child {
        return child(&config, index);
    }
    let mut failed = 0;
    for index in 0..config.runs {
        match run(&config, index) {
            Ok((failures, stats)) => {
                let verdict = if failures.is_empty() { "ok" } else { "FAIL" };
                println!(
                    "{:?} run {index} seed {}: {verdict} {stats:?}",
                    config.backend,
                    config.seed + index as u64
                );
                for failure in failures.iter().take(12) {
                    println!("  {failure}");
                }
                if !failures.is_empty() {
                    failed += 1;
                    if failures.len() > 12 {
                        println!("  ... {} more", failures.len() - 12);
                    }
                }
            }
            Err(error) => {
                println!("{:?} run {index}: ERROR {error}", config.backend);
                failed += 1;
            }
        }
    }
    println!(
        "{:?}: {} of {} runs passed",
        config.backend,
        config.runs - failed,
        config.runs
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
