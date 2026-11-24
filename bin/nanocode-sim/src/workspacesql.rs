use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    ops::Range,
    pin::{Pin, pin},
    process::ExitCode,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

use nanocode_bytes::Bytes;
use nanocode_durability_core::{Sql, SqlAccess, SqlRow, SqlStatement, StoreError, StoreFuture};
use nanocode_shell::{FileSystem, Machine, Remove, Reply, Write};
use nanocode_sync::Atom;
use nanocode_sys::base64::STANDARD;
use nanocode_workspace_fs::{
    Batch, Blob, Caps, Change, Digest, Entry, Failure, Manifest, SqlStore, Store, Sweep,
    WorkspaceFs,
    objects::{Listed, Objects},
    sim::{self as fs_sim, Mutation as FsRule},
};
use serde_json::Value;

use crate::rng::{FNV_OFFSET, Rng, fnv};

const WORKSPACES: [&str; 2] = ["left", "right"];
const WRITERS: [usize; 4] = [0, 0, 1, 1];
const ROUNDS_MIN: u64 = 30;
const ROUNDS_MAX: u64 = 120;
const FLIGHTS_MAX: u64 = 3;
const FILE_BYTES_MIN: u64 = 8;
const FILE_BYTES_MAX: u64 = 96;
const WORKSPACE_FILES_MIN: u64 = 2;
const WORKSPACE_FILES_MAX: u64 = 6;
const PATHS_MIN: u64 = 4;
const PATHS_MAX: u64 = 12;
const OPS_PER_COMMIT: usize = 8;
const PATH_BYTES: usize = 32;
const CONTENTS: u64 = 4;
const FRESH_PPM: u64 = 200_000;
const OVERSIZE_PPM: u64 = 100_000;
const BAD_DIGEST_PPM: u64 = 150_000;
const CRASH_PPM: u64 = 120_000;
const CRASH_AFTER_PPM: u64 = 300_000;
const DROP_PPM: u64 = 5_000;
const REOPEN_PPM: u64 = 60_000;
const REMOVE_FILE_PPM: u64 = 500_000;
const YIELDS_MAX: u64 = 3;
const POLLS_MAX: u64 = 100_000;
const OPENS_MAX: u32 = 32;
const TICK_NANOS: u64 = 1_000;
const MTIME_NS_MAX: u64 = 1 << 40;
const DIGEST_SHOWN: usize = 12;
const NAMES: [&str; 8] = ["a", "b", "d", "d/c", "d/e", "d/e/f", "g", "g/h"];
const MODES: [u32; 3] = [0o644, 0o600, 0o755];
const FILE: &str = "file";
const DIR: &str = "dir";
const SYMLINK: &str = "symlink";
const GATE: &str = "AND token = ?2)";
const OBJECTS_PPM: u64 = 500_000;
const SWEEP_PPM: u64 = 150_000;
const SWEEP_LIMIT: usize = 4;
const SWEEP_TICKS_MAX: u64 = 8;
const SWEEP: Sweep = Sweep {
    settle_ms: 4,
    window_ms: 12,
    put_ms: 6,
};
const OBJECT: &str = "@object";
const GONE: &str = "@gone:";
const OBJECT_PREFIX: &str = "blobs/";
const TOMBSTONE: &str =
    "INSERT INTO nanocode_workspace_blobs (digest, data, refs) SELECT value, ?2, 0 ";
const DEAD: &str = "SELECT digest, data FROM nanocode_workspace_blobs WHERE refs <= 0 ";
const EXPIRE: &str = "DELETE FROM nanocode_workspace_blobs WHERE digest IN (SELECT value FROM json_each(?1)) AND data IN ";
const CAS: &str = "AND version = ";
const GATED: [&str; 5] = [
    "DELETE FROM nanocode_workspace_entries ",
    "INSERT INTO nanocode_workspace_entries ",
    "INSERT INTO nanocode_workspace_blobs ",
    "UPDATE nanocode_workspace_blobs SET refs = refs - 1 ",
    "DELETE FROM nanocode_workspace_blobs ",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceSqlMutation {
    None,
    SkipVersionCas,
    ReleaseBeforeCommit,
    SplitTransaction,
    SweepSkipsTombstone,
    StaleReadSkipsReload,
}

impl WorkspaceSqlMutation {
    const ALL: [Self; 6] = [
        Self::None,
        Self::SkipVersionCas,
        Self::ReleaseBeforeCommit,
        Self::SplitTransaction,
        Self::SweepSkipsTombstone,
        Self::StaleReadSkipsReload,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SkipVersionCas => "skip_version_cas",
            Self::ReleaseBeforeCommit => "release_before_commit",
            Self::SplitTransaction => "split_transaction",
            Self::SweepSkipsTombstone => "sweep_skips_tombstone",
            Self::StaleReadSkipsReload => "stale_read_skips_reload",
        }
    }

    pub fn names() -> String {
        Self::ALL
            .iter()
            .skip(1)
            .map(|mutation| mutation.name())
            .collect::<Vec<_>>()
            .join("|")
    }

    const fn rule(self) -> FsRule {
        match self {
            Self::None => FsRule::None,
            Self::SkipVersionCas => FsRule::SqlSkipVersionCas,
            Self::ReleaseBeforeCommit => FsRule::SqlReleaseBeforeCommit,
            Self::SplitTransaction => FsRule::SqlSplitTransaction,
            Self::SweepSkipsTombstone => FsRule::SqlSweepSkipsTombstone,
            Self::StaleReadSkipsReload => FsRule::StaleReadSkipsReload,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WorkspaceSqlStats {
    pub seeds: u64,
    pub rounds: u64,
    pub flights: u64,
    pub crowded: u64,
    pub transactions: u64,
    pub commits: u64,
    pub stale: u64,
    pub capped: u64,
    pub aborts: u64,
    pub crashes_after: u64,
    pub drops: u64,
    pub reopens: u64,
    pub failed_opens: u64,
    pub shared: u64,
    pub max_version: u64,
    pub sweeps: u64,
    pub reaped: u64,
}

impl WorkspaceSqlStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.rounds += other.rounds;
        self.flights += other.flights;
        self.crowded += other.crowded;
        self.transactions += other.transactions;
        self.commits += other.commits;
        self.stale += other.stale;
        self.capped += other.capped;
        self.aborts += other.aborts;
        self.crashes_after += other.crashes_after;
        self.drops += other.drops;
        self.reopens += other.reopens;
        self.failed_opens += other.failed_opens;
        self.shared += other.shared;
        self.sweeps += other.sweeps;
        self.reaped += other.reaped;
        if other.max_version > self.max_version {
            self.max_version = other.max_version;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    kind: String,
    digest: Option<String>,
    size: i64,
    mode: i64,
    mtime: i64,
    target: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Tables {
    formats: BTreeMap<String, String>,
    heads: BTreeMap<String, (u64, String)>,
    entries: BTreeMap<(String, String), Row>,
    blobs: BTreeMap<String, (String, i64)>,
}

fn refused(why: impl Into<String>) -> StoreError {
    StoreError::Backend(why.into())
}

fn param<'a>(statement: &SqlStatement<'a>, at: usize) -> Result<&'a str, StoreError> {
    statement
        .params
        .get(at)
        .copied()
        .ok_or_else(|| refused(format!("a statement lacks parameter {}", at + 1)))
}

fn elements(text: &str) -> Result<Vec<Value>, StoreError> {
    match serde_json::from_str(text) {
        Ok(Value::Array(items)) => Ok(items),
        _ => Err(refused("a JSON parameter is not an array")),
    }
}

fn texts(text: &str) -> Result<BTreeSet<String>, StoreError> {
    elements(text)?
        .into_iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| refused("a JSON list holds a non-string"))
        })
        .collect()
}

fn optional(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn integer(value: Option<&Value>) -> Result<i64, StoreError> {
    value
        .and_then(Value::as_str)
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| refused("a JSON integer does not parse"))
}

fn single(row: Option<SqlRow>) -> Vec<SqlRow> {
    row.into_iter().collect()
}

impl Tables {
    fn version(&self, workspace: &str) -> u64 {
        self.heads.get(workspace).map_or(0, |(version, _)| *version)
    }

    fn rows_of<'a>(&'a self, workspace: &'a str) -> impl Iterator<Item = (&'a str, &'a Row)> + 'a {
        self.entries
            .range((workspace.to_owned(), String::new())..)
            .take_while(move |((held, _), _)| held == workspace)
            .map(|((_, path), row)| (path.as_str(), row))
    }

    fn load(&self, workspace: &str) -> Vec<SqlRow> {
        let Some((version, _)) = self.heads.get(workspace) else {
            return Vec::new();
        };
        let version = Some(version.to_string());
        let mut rows: Vec<SqlRow> = self
            .rows_of(workspace)
            .map(|(path, row)| {
                vec![
                    version.clone(),
                    Some(path.to_owned()),
                    Some(row.kind.clone()),
                    row.digest.clone(),
                    Some(row.size.to_string()),
                    Some(row.mode.to_string()),
                    Some(row.mtime.to_string()),
                    row.target.clone(),
                ]
            })
            .collect();
        if rows.is_empty() {
            rows.push(vec![version, None, None, None, None, None, None, None]);
        }
        rows
    }

    fn tree(&self, workspace: &str) -> Result<BTreeMap<String, Entry>, String> {
        self.rows_of(workspace)
            .map(|(path, row)| {
                let mode = u32::try_from(row.mode).map_err(|_| format!("{path} holds a bad mode"));
                let entry = match row.kind.as_str() {
                    FILE => Entry::File {
                        digest: row
                            .digest
                            .as_deref()
                            .and_then(Digest::parse)
                            .ok_or_else(|| format!("{path} holds no digest"))?,
                        size: u64::try_from(row.size)
                            .map_err(|_| format!("{path} holds a bad size"))?,
                        mode: mode?,
                        mtime_ns: row.mtime,
                        link: row.target.as_deref().and_then(|link| link.parse().ok()),
                    },
                    DIR => Entry::Dir {
                        mode: mode?,
                        mtime_ns: row.mtime,
                    },
                    SYMLINK => Entry::Symlink {
                        target: row
                            .target
                            .clone()
                            .ok_or_else(|| format!("{path} holds no target"))?,
                        mtime_ns: row.mtime,
                    },
                    other => return Err(format!("{path} holds kind {other}")),
                };
                Ok((path.to_owned(), entry))
            })
            .collect()
    }

    fn execute(&mut self, statement: &SqlStatement<'_>) -> Result<Vec<SqlRow>, StoreError> {
        let sql = statement.sql;
        if sql.starts_with("CREATE TABLE IF NOT EXISTS ") {
            return Ok(Vec::new());
        }
        if sql.starts_with("SELECT CAST(version AS TEXT) FROM nanocode_formats ") {
            let held = self.formats.get(param(statement, 0)?);
            return Ok(single(held.map(|version| vec![Some(version.clone())])));
        }
        if sql.starts_with("INSERT INTO nanocode_formats ") {
            let component = param(statement, 0)?.to_owned();
            self.formats
                .insert(component, param(statement, 1)?.to_owned());
            return Ok(Vec::new());
        }
        if sql.starts_with("CREATE INDEX IF NOT EXISTS ") {
            return Ok(Vec::new());
        }
        if sql.starts_with(TOMBSTONE) {
            let mark = param(statement, 1)?;
            for digest in texts(param(statement, 0)?)? {
                self.blobs
                    .entry(digest)
                    .or_insert_with(|| (mark.to_owned(), 0));
            }
            return Ok(Vec::new());
        }
        if sql.starts_with(DEAD) {
            let limit: usize = param(statement, 0)?
                .parse()
                .map_err(|_| refused("a limit does not parse"))?;
            return Ok(self
                .blobs
                .iter()
                .filter(|(_, (_, refs))| *refs <= 0)
                .take(limit)
                .map(|(digest, (data, _))| vec![Some(digest.clone()), Some(data.clone())])
                .collect());
        }
        if sql.starts_with(EXPIRE) {
            let marks = texts(param(statement, 1)?)?;
            for digest in texts(param(statement, 0)?)? {
                if self
                    .blobs
                    .get(&digest)
                    .is_some_and(|(data, refs)| *refs <= 0 && marks.contains(data))
                {
                    self.blobs.remove(&digest);
                }
            }
            return Ok(Vec::new());
        }
        let key = param(statement, 0)?;
        if sql.starts_with("SELECT CAST(h.version AS TEXT), e.path,") {
            return Ok(self.load(key));
        }
        if sql.starts_with("SELECT CAST(version AS TEXT), token FROM nanocode_workspace_heads ") {
            let head = self.heads.get(key);
            return Ok(single(head.map(|(version, token)| {
                vec![Some(version.to_string()), Some(token.clone())]
            })));
        }
        if sql.starts_with("SELECT data FROM nanocode_workspace_blobs ") {
            let held = self.blobs.get(key);
            return Ok(single(held.map(|(data, _)| vec![Some(data.clone())])));
        }
        let token = param(statement, 1)?;
        if sql.starts_with("INSERT INTO nanocode_workspace_heads ") {
            if self.tombstoned(param(statement, 2)?)? {
                return Ok(Vec::new());
            }
            self.heads
                .entry(key.to_owned())
                .or_insert_with(|| (1, token.to_owned()));
            return Ok(Vec::new());
        }
        if sql.starts_with("UPDATE nanocode_workspace_heads SET version = version + 1") {
            if !self.tombstoned(param(statement, 3)?)? {
                self.claim(sql, key, token, param(statement, 2)?)?;
            }
            return Ok(Vec::new());
        }
        if !GATED.iter().any(|prefix| sql.starts_with(prefix)) {
            return Err(refused(format!("the sim SQL host does not run: {sql}")));
        }
        let admitted =
            !sql.contains(GATE) || self.heads.get(key).is_some_and(|(_, held)| held == token);
        if admitted {
            self.change(statement, key)?;
        }
        Ok(Vec::new())
    }

    fn tombstoned(&self, list: &str) -> Result<bool, StoreError> {
        Ok(texts(list)?.iter().any(|digest| {
            self.blobs
                .get(digest)
                .is_some_and(|(data, _)| data.starts_with(GONE))
        }))
    }

    fn claim(
        &mut self,
        sql: &str,
        workspace: &str,
        token: &str,
        base: &str,
    ) -> Result<(), StoreError> {
        let base: Option<u64> = if sql.contains(CAS) {
            Some(
                base.parse()
                    .map_err(|_| refused("a base version does not parse"))?,
            )
        } else {
            None
        };
        if let Some((version, held)) = self.heads.get_mut(workspace)
            && base.is_none_or(|base| base == *version)
        {
            *version += 1;
            token.clone_into(held);
        }
        Ok(())
    }

    fn change(&mut self, statement: &SqlStatement<'_>, workspace: &str) -> Result<(), StoreError> {
        let sql = statement.sql;
        let list = param(statement, 2)?;
        if sql.starts_with(GATED[0]) {
            for path in texts(list)? {
                self.entries.remove(&(workspace.to_owned(), path));
            }
        } else if sql.starts_with(GATED[1]) {
            for item in elements(list)? {
                self.put(workspace, &item)?;
            }
        } else if sql.starts_with(GATED[2]) {
            let data = param(statement, 3)?;
            self.blobs
                .entry(list.to_owned())
                .and_modify(|(held, refs)| {
                    *refs += 1;
                    if held.starts_with(GONE) {
                        data.clone_into(held);
                    }
                })
                .or_insert_with(|| (data.to_owned(), 1));
        } else if sql.starts_with(GATED[3]) {
            for digest in texts(list)? {
                if let Some((_, refs)) = self.blobs.get_mut(&digest) {
                    *refs -= 1;
                }
            }
        } else {
            for digest in texts(list)? {
                if self.blobs.get(&digest).is_some_and(|(_, refs)| *refs <= 0) {
                    self.blobs.remove(&digest);
                }
            }
        }
        Ok(())
    }

    fn put(&mut self, workspace: &str, item: &Value) -> Result<(), StoreError> {
        let path = optional(item.get(0)).ok_or_else(|| refused("an entry has no path"))?;
        let row = Row {
            kind: optional(item.get(1)).ok_or_else(|| refused("an entry has no kind"))?,
            digest: optional(item.get(2)),
            size: integer(item.get(3))?,
            mode: integer(item.get(4))?,
            mtime: integer(item.get(5))?,
            target: optional(item.get(6)),
        };
        if self
            .entries
            .insert((workspace.to_owned(), path.clone()), row)
            .is_some()
        {
            return Err(refused(format!("UNIQUE constraint failed: {path}")));
        }
        Ok(())
    }
}

struct Hold(u64);

impl Future for Hold {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.0 == 0 {
            return Poll::Ready(());
        }
        self.0 -= 1;
        context.waker().wake_by_ref();
        Poll::Pending
    }
}

struct SimObjects {
    held: Atom<BTreeMap<String, (Vec<u8>, u64)>>,
    now: Arc<AtomicU64>,
}

impl SimObjects {
    fn clock(&self) -> nanocode_workspace_fs::Clock {
        let now = Arc::clone(&self.now);
        Arc::new(move || now.load(Ordering::Relaxed))
    }

    fn bytes(&self, digest: &str) -> Option<Vec<u8>> {
        let key = format!("{OBJECT_PREFIX}{digest}");
        self.held
            .update(|held| held.get(&key).map(|(bytes, _)| bytes.clone()))
    }
}

impl Objects for SimObjects {
    fn put<'a>(&'a self, key: &'a str, bytes: &'a Bytes) -> StoreFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            Hold(1).await;
            let at = self.now.fetch_add(1, Ordering::Relaxed) + 1;
            self.held.update(|held| {
                held.insert(key.to_owned(), (bytes.to_vec(), at));
            });
            Ok(())
        })
    }

    fn get<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Result<Option<Bytes>, Failure>> {
        Box::pin(async move {
            Hold(1).await;
            Ok(self
                .held
                .update(|held| held.get(key).map(|(bytes, _)| Bytes::from(bytes.clone()))))
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            Hold(1).await;
            self.held.update(|held| {
                held.remove(key);
            });
            Ok(())
        })
    }

    fn list<'a>(
        &'a self,
        prefix: &'a str,
        after: &'a str,
        limit: usize,
    ) -> StoreFuture<'a, Result<Vec<Listed>, Failure>> {
        Box::pin(async move {
            Hold(1).await;
            Ok(self.held.update(|held| {
                held.iter()
                    .filter(|(key, _)| key.starts_with(prefix) && key.as_str() > after)
                    .take(limit)
                    .map(|(key, (_, at))| Listed {
                        key: key.clone(),
                        modified_ms: *at,
                    })
                    .collect()
            }))
        })
    }
}

struct SimSql {
    tables: Atom<Arc<Tables>>,
    rng: Atom<Rng>,
    commits: Atom<Vec<Arc<Tables>>>,
    transactions: AtomicU64,
    aborts: AtomicU64,
    crashes_after: AtomicU64,
}

impl SimSql {
    fn below(&self, bound: u64) -> u64 {
        self.rng.update(|rng| rng.below(bound))
    }

    fn chance(&self, ppm: u64) -> bool {
        self.rng.update(|rng| rng.chance(ppm))
    }

    async fn execute(
        &self,
        writes: bool,
        statements: &[SqlStatement<'_>],
    ) -> Result<Vec<Vec<SqlRow>>, StoreError> {
        Hold(self.below(YIELDS_MAX + 1)).await;
        let crash = writes && self.chance(CRASH_PPM);
        let after = crash && self.chance(CRASH_AFTER_PPM);
        let cut = (crash && !after).then(|| self.below(statements.len() as u64 + 1) as usize);
        let mut scratch = Tables::clone(&self.tables.get());
        let mut results = Vec::with_capacity(statements.len());
        for statement in statements {
            if cut == Some(results.len()) {
                break;
            }
            results.push(scratch.execute(statement)?);
        }
        if let Some(cut) = cut {
            self.aborts.fetch_add(1, Ordering::Relaxed);
            return Err(refused(format!(
                "the transaction aborted after {cut} of {} statements",
                statements.len()
            )));
        }
        if writes {
            self.transactions.fetch_add(1, Ordering::Relaxed);
            let committed = Arc::new(scratch);
            self.tables.set(Arc::clone(&committed));
            self.commits.update(|log| log.push(Arc::clone(&committed)));
        }
        Hold(self.below(YIELDS_MAX + 1)).await;
        if after {
            self.crashes_after.fetch_add(1, Ordering::Relaxed);
            return Err(refused("the connection dropped after the commit"));
        }
        Ok(results)
    }
}

impl Sql for SimSql {
    fn call_bound(&self) -> Option<std::time::Duration> {
        None
    }

    fn transaction<'a>(
        &'a self,
        _: &'static str,
        statements: &'a [SqlStatement<'a>],
    ) -> StoreFuture<'a, Result<Vec<Vec<SqlRow>>, StoreError>> {
        Box::pin(self.execute(true, statements))
    }

    fn run<'a>(
        &'a self,
        access: SqlAccess,
        _: &'static str,
        statements: &'a [SqlStatement<'a>],
    ) -> StoreFuture<'a, Result<Vec<Vec<SqlRow>>, StoreError>> {
        Box::pin(self.execute(access != SqlAccess::Read, statements))
    }
}

type Intents = Atom<Vec<(String, Change)>>;
type Acks = Atom<Vec<(String, u64, Change)>>;

struct Recorder {
    inner: SqlStore,
    intents: Arc<Intents>,
    acks: Arc<Acks>,
}

impl Store for Recorder {
    fn load<'a>(&'a self, workspace: &'a str) -> StoreFuture<'a, Result<Manifest, Failure>> {
        self.inner.load(workspace)
    }

    fn commit<'a>(
        &'a self,
        workspace: &'a str,
        batch: &'a Batch,
    ) -> StoreFuture<'a, Result<u64, Failure>> {
        self.intents
            .update(|log| log.push((workspace.to_owned(), batch.change().clone())));
        Box::pin(async move {
            let committed = self.inner.commit(workspace, batch).await;
            if let Ok(version) = &committed {
                self.acks.update(|log| {
                    log.push((workspace.to_owned(), *version, batch.change().clone()));
                });
            }
            committed
        })
    }

    fn blob<'a>(&'a self, digest: &'a Digest) -> StoreFuture<'a, Result<Blob, Failure>> {
        self.inner.blob(digest)
    }
}

struct Clock(Cell<u64>);

impl Machine for Clock {
    fn now(&self) -> Duration {
        let now = self.0.get() + TICK_NANOS;
        self.0.set(now);
        Duration::from_nanos(now)
    }

    fn sleep(&self, _: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(std::future::ready(()))
    }

    fn random(&self, bytes: &mut [u8]) {
        bytes.fill(0);
    }
}

type Flying = Pin<Box<dyn Future<Output = io::Result<()>>>>;

fn drive<T>(future: impl Future<Output = T>) -> Result<T, String> {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    for _ in 0..POLLS_MAX {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return Ok(value);
        }
    }
    Err(format!("a call did not finish in {POLLS_MAX} polls"))
}

fn reply<T>(reply: Reply<T>) -> Result<io::Result<T>, String> {
    match reply {
        Reply::Now(result) => Ok(result),
        Reply::Later(future) => drive(future),
    }
}

fn flying(reply: Reply<()>) -> Flying {
    match reply {
        Reply::Now(result) => Box::pin(std::future::ready(result)),
        Reply::Later(future) => future,
    }
}

fn applied(base: &BTreeMap<String, Entry>, change: &Change) -> BTreeMap<String, Entry> {
    let mut next = base.clone();
    for (path, entry) in &change.entries {
        match entry {
            Some(entry) => {
                next.insert(path.clone(), entry.clone());
            }
            None => {
                next.remove(path);
            }
        }
    }
    next
}

fn workspace_index(name: &str) -> Result<usize, String> {
    WORKSPACES
        .iter()
        .position(|held| *held == name)
        .ok_or_else(|| format!("a commit names the unknown workspace {name}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    Any,
    TooLarge,
    BadDigest,
}

struct Flight {
    writer: usize,
    future: Option<Flying>,
    result: Option<io::Result<()>>,
    expect: Expect,
    written: Option<(String, Vec<u8>)>,
}

struct World {
    rng: Rng,
    caps: Caps,
    host: Arc<SimSql>,
    intents: Arc<Intents>,
    acks: Arc<Acks>,
    clock: Rc<dyn Machine>,
    writers: Vec<(usize, WorkspaceFs)>,
    contents: Vec<Vec<u8>>,
    history: Vec<BTreeMap<u64, BTreeMap<String, Entry>>>,
    seen: Arc<Tables>,
    pending: Vec<(String, Change)>,
    stats: WorkspaceSqlStats,
    objects: Option<Arc<SimObjects>>,
    sweeper: Option<Rc<SqlStore>>,
    cursor: Rc<RefCell<String>>,
    reaped: Rc<Cell<u64>>,
}

impl World {
    fn store(&self) -> Result<Result<SqlStore, Failure>, String> {
        let host: Arc<dyn Sql> = self.host.clone();
        match &self.objects {
            Some(objects) => {
                let clock = objects.clock();
                let objects: Arc<dyn Objects> = objects.clone();
                drive(SqlStore::with_objects(host, objects, SWEEP, clock))
            }
            None => drive(SqlStore::open(host)),
        }
    }

    fn open_sweeper(&mut self) -> Result<(), String> {
        if self.objects.is_none() {
            return Ok(());
        }
        for _ in 0..OPENS_MAX {
            if let Ok(store) = self.store()? {
                self.sweeper = Some(Rc::new(store));
                return Ok(());
            }
            self.stats.failed_opens += 1;
        }
        Err(format!("the sweeper failed to open {OPENS_MAX} times"))
    }

    fn sweep(&mut self) -> Option<Flying> {
        let sweeper = Rc::clone(self.sweeper.as_ref()?);
        let objects = self.objects.as_ref()?;
        if !self.rng.chance(SWEEP_PPM) {
            return None;
        }
        let ticks = self.rng.below(SWEEP_TICKS_MAX + 1);
        objects.now.fetch_add(ticks, Ordering::Relaxed);
        self.stats.sweeps += 1;
        let (cursor, reaped) = (Rc::clone(&self.cursor), Rc::clone(&self.reaped));
        Some(Box::pin(async move {
            let after = cursor.borrow().clone();
            let swept = sweeper
                .sweep(&after, SWEEP_LIMIT)
                .await
                .map_err(|failure| io::Error::other(failure.to_string()))?;
            reaped.set(reaped.get() + swept.deleted as u64);
            *cursor.borrow_mut() = swept.next.unwrap_or_default();
            Ok(())
        }))
    }

    fn open(&mut self, workspace: usize) -> Result<WorkspaceFs, String> {
        for _ in 0..OPENS_MAX {
            let Ok(inner) = self.store()? else {
                self.stats.failed_opens += 1;
                continue;
            };
            let store: Arc<dyn Store> = Arc::new(Recorder {
                inner,
                intents: Arc::clone(&self.intents),
                acks: Arc::clone(&self.acks),
            });
            let opened = drive(WorkspaceFs::open(
                store,
                WORKSPACES[workspace],
                self.caps,
                Rc::clone(&self.clock),
            ))?;
            return opened
                .map_err(|error| format!("{} does not load: {error}", WORKSPACES[workspace]));
        }
        Err(format!("the store failed to open {OPENS_MAX} times"))
    }

    fn check_commits(&mut self) -> Result<(), String> {
        let intents = self.intents.set(Vec::new());
        self.pending.extend(intents.iter().cloned());
        let commits = self.host.commits.set(Vec::new());
        for snapshot in commits.iter() {
            self.check_snapshot(snapshot)?;
            self.seen = Arc::clone(snapshot);
        }
        let acks = self.acks.set(Vec::new());
        for (name, version, change) in acks.iter() {
            let history = &self.history[workspace_index(name)?];
            let whole = match (history.get(&change.base), history.get(version)) {
                (Some(base), Some(held)) => {
                    change.version == *version && applied(base, change) == *held
                }
                _ => false,
            };
            if !whole {
                return Err(format!(
                    "{name} acknowledged version {version} but the store holds another tree there"
                ));
            }
        }
        let seen = Arc::clone(&self.seen);
        self.pending
            .retain(|(name, change)| change.base >= seen.version(name));
        Ok(())
    }

    fn check_snapshot(&mut self, snapshot: &Tables) -> Result<(), String> {
        for (index, name) in WORKSPACES.iter().enumerate() {
            let before = self.seen.version(name);
            let after = snapshot.version(name);
            let tree = snapshot.tree(name)?;
            if after == before {
                if self.history[index].get(&before) != Some(&tree) {
                    return Err(format!(
                        "{name} changed at version {before} without a new version: a partial commit"
                    ));
                }
                continue;
            }
            if after != before + 1 {
                return Err(format!(
                    "{name} moved from version {before} to {after} in one transaction"
                ));
            }
            let base = &self.history[index][&before];
            let whole = self.pending.iter().any(|(held, change)| {
                held == name
                    && change.base == before
                    && change.version == after
                    && applied(base, change) == tree
            });
            if !whole {
                return Err(format!(
                    "{name} version {after} is no whole change on version {before}: a partial or lost update"
                ));
            }
            let cap = self.caps.file_bytes;
            if let Some((path, size)) = tree.iter().find_map(|(path, entry)| match entry {
                Entry::File { size, .. } if *size > cap => Some((path, *size)),
                _ => None,
            }) {
                return Err(format!(
                    "{name} version {after} holds {path} of {size} bytes over the file cap {cap}"
                ));
            }
            if let Err(corrupt) = Manifest::from_entries(after, tree.clone()) {
                return Err(format!(
                    "{name} version {after} is no valid tree: {corrupt:?}"
                ));
            }
            self.history[index].insert(after, tree);
            self.stats.commits += 1;
            self.stats.max_version = self.stats.max_version.max(after);
        }
        self.check_refs(snapshot)
    }

    fn check_refs(&mut self, snapshot: &Tables) -> Result<(), String> {
        let mut holders: BTreeMap<&str, i64> = BTreeMap::new();
        for name in WORKSPACES {
            let held: BTreeSet<&str> = snapshot
                .rows_of(name)
                .filter(|(_, row)| row.kind == FILE)
                .filter_map(|(_, row)| row.digest.as_deref())
                .collect();
            for digest in held {
                *holders.entry(digest).or_default() += 1;
            }
        }
        if holders.values().any(|count| *count > 1) {
            self.stats.shared += 1;
        }
        for (digest, (data, refs)) in &snapshot.blobs {
            let short = &digest[..digest.len().min(DIGEST_SHOWN)];
            if data.starts_with(GONE) {
                if holders.contains_key(digest.as_str()) || *refs > 0 {
                    return Err(format!("the tombstoned blob {short} is still held"));
                }
                continue;
            }
            match holders.get(digest.as_str()) {
                Some(count) if count == refs => {}
                Some(count) => {
                    return Err(format!(
                        "blob {short} counts {refs} references for {count} holders"
                    ));
                }
                None => {
                    return Err(format!(
                        "blob {short} outlives its last holder with {refs} references"
                    ));
                }
            }
            let bytes = match (&self.objects, data == OBJECT) {
                (Some(objects), true) => objects
                    .bytes(digest)
                    .ok_or_else(|| format!("blob {short} lost its object"))?,
                (None, true) => return Err(format!("blob {short} points at no object store")),
                _ => STANDARD
                    .decode(data)
                    .map_err(|_| format!("blob {short} is not base64"))?,
            };
            if Digest::of(&bytes).hex() != *digest {
                return Err(format!("blob {short} holds other bytes"));
            }
        }
        if let Some(digest) = holders
            .keys()
            .find(|digest| !snapshot.blobs.contains_key(**digest))
        {
            return Err(format!("the held digest {digest} has no blob"));
        }
        Ok(())
    }

    fn check_writers(&self) -> Result<(), String> {
        for (index, (workspace, fs)) in self.writers.iter().enumerate() {
            let shown = fs.manifest();
            if !self.history[*workspace]
                .get(&shown.version())
                .is_some_and(|tree| tree.iter().eq(shown.entries().iter()))
            {
                return Err(format!(
                    "writer {index} shows {} version {} that the store never committed",
                    WORKSPACES[*workspace],
                    shown.version()
                ));
            }
        }
        Ok(())
    }

    fn reopen(&mut self, at: usize) -> Result<(), String> {
        self.stats.reopens += 1;
        let workspace = self.writers[at].0;
        let reopened = self.open(workspace)?;
        self.check_commits()?;
        let name = WORKSPACES[workspace];
        let head = self.seen.version(name);
        let shown = reopened.manifest();
        if shown.version() != head
            || !self.history[workspace]
                .get(&head)
                .is_some_and(|tree| tree.iter().eq(shown.entries().iter()))
        {
            return Err(format!(
                "reopening {name} gave version {} instead of the whole head {head}",
                shown.version()
            ));
        }
        for (path, entry) in shown.entries() {
            if let Entry::File { digest, .. } = entry {
                let download = drive(reopened.download(path.as_bytes()))?
                    .map_err(|error| format!("{path} unreadable after a reopen: {error}"))?;
                if download.digest != *digest || Digest::of(&download.bytes) != *digest {
                    return Err(format!("{path} changed across a reopen"));
                }
            }
        }
        self.writers[at].1 = reopened;
        Ok(())
    }

    fn path(&mut self) -> String {
        format!("/{}", NAMES[self.rng.pick(NAMES.len())])
    }

    fn mode(&mut self) -> u32 {
        MODES[self.rng.pick(MODES.len())]
    }

    fn random(&mut self, size: u64) -> Vec<u8> {
        (0..size).map(|_| self.rng.below(256) as u8).collect()
    }

    fn data(&mut self, oversize: bool) -> Vec<u8> {
        let cap = self.caps.file_bytes;
        if oversize {
            let size = cap + 1 + self.rng.below(cap);
            return self.random(size);
        }
        if self.rng.chance(FRESH_PPM) {
            let size = self.rng.below(cap + 1);
            return self.random(size);
        }
        self.contents[self.rng.pick(self.contents.len())].clone()
    }

    fn launch(&mut self) -> Result<Flight, String> {
        let writer = self.rng.pick(self.writers.len());
        let fs = self.writers[writer].1.clone();
        let mut flight = Flight {
            writer,
            future: None,
            result: None,
            expect: Expect::Any,
            written: None,
        };
        if let Some(sweep) = self.sweep() {
            flight.future = Some(sweep);
            return Ok(flight);
        }
        let kind = self.rng.below(10);
        let future = match kind {
            0..=2 => self.write(&fs, kind, &mut flight)?,
            3 => {
                let (path, mode) = (self.path(), self.mode());
                flying(fs.mkdir(path.as_bytes(), mode))
            }
            4 => {
                let path = self.path();
                let what = if self.rng.chance(REMOVE_FILE_PPM) {
                    Remove::File
                } else {
                    Remove::Dir
                };
                flying(fs.remove(path.as_bytes(), what))
            }
            5 => {
                let (from, to) = (self.path(), self.path());
                flying(fs.rename(from.as_bytes(), to.as_bytes()))
            }
            6 => {
                let (target, link) = (self.path(), self.path());
                flying(fs.symlink(&target.as_bytes()[1..], link.as_bytes()))
            }
            7 => {
                let (path, mode) = (self.path(), self.mode());
                flying(fs.set_mode(path.as_bytes(), mode))
            }
            8 => {
                let path = self.path();
                let mtime_ns = self.rng.below(MTIME_NS_MAX) as i64;
                flying(fs.set_mtime(path.as_bytes(), mtime_ns))
            }
            _ => self.upload(&fs, &mut flight),
        };
        flight.future = Some(future);
        Ok(flight)
    }

    fn write(
        &mut self,
        fs: &WorkspaceFs,
        kind: u64,
        flight: &mut Flight,
    ) -> Result<Flying, String> {
        let path = self.path();
        let oversize = self.rng.chance(OVERSIZE_PPM);
        if oversize {
            flight.expect = Expect::TooLarge;
        }
        let data = self.data(oversize);
        let how = match kind {
            0 => Write::Replace,
            1 => Write::Append,
            _ => Write::New,
        };
        let mut whole = data.clone();
        if how == Write::Append
            && let Ok(held) = reply(fs.read(path.as_bytes(), u64::MAX))?
        {
            whole = [&held[..], &data[..]].concat();
        }
        let future = flying(fs.write(path.as_bytes(), &data, how));
        flight.written = Some((path, whole));
        Ok(future)
    }

    fn upload(&mut self, fs: &WorkspaceFs, flight: &mut Flight) -> Flying {
        let path = self.path();
        let oversize = self.rng.chance(OVERSIZE_PPM);
        let data = self.data(oversize);
        let bad = self.rng.chance(BAD_DIGEST_PPM);
        let expected = if bad {
            flight.expect = Expect::BadDigest;
            Digest::of(&[&data[..], b"!"].concat())
        } else {
            Digest::of(&data)
        };
        if oversize {
            flight.expect = Expect::TooLarge;
        }
        let chunk = 1 + self.rng.below(self.caps.file_bytes) as usize;
        let mut upload = fs.upload(path.as_bytes(), expected, MODES[0]);
        let pushed = data.chunks(chunk).try_for_each(|piece| upload.push(piece));
        if !bad {
            flight.written = Some((path, data));
        }
        match pushed {
            Ok(()) => Box::pin(async move { upload.finish().await.map(drop) }),
            Err(error) => Box::pin(std::future::ready(Err(error))),
        }
    }

    fn fly(&mut self, flights: &mut [Flight]) -> Result<BTreeSet<usize>, String> {
        let mut crashed = BTreeSet::new();
        let mut context = Context::from_waker(Waker::noop());
        for _ in 0..POLLS_MAX {
            let live: Vec<usize> = flights
                .iter()
                .enumerate()
                .filter(|(_, flight)| flight.future.is_some())
                .map(|(at, _)| at)
                .collect();
            if live.is_empty() {
                return Ok(crashed);
            }
            let at = live[self.rng.pick(live.len())];
            let flight = &mut flights[at];
            if self.rng.chance(DROP_PPM) {
                flight.future = None;
                crashed.insert(flight.writer);
                self.stats.drops += 1;
                continue;
            }
            if let Some(future) = flight.future.as_mut()
                && let Poll::Ready(result) = future.as_mut().poll(&mut context)
            {
                flight.future = None;
                flight.result = Some(result);
            }
            self.check_commits()?;
        }
        Err(format!("a round did not finish in {POLLS_MAX} polls"))
    }

    fn round(&mut self) -> Result<(), String> {
        self.stats.rounds += 1;
        let count = 1 + self.rng.below(FLIGHTS_MAX);
        self.stats.flights += count;
        if count > 1 {
            self.stats.crowded += 1;
        }
        let mut flights = Vec::new();
        for _ in 0..count {
            flights.push(self.launch()?);
        }
        let crashed = self.fly(&mut flights)?;
        self.check_commits()?;
        self.check_writers()?;
        for flight in &flights {
            self.judge(flight, &flights, &crashed)?;
        }
        self.check_stale_reads()?;
        for writer in crashed {
            self.reopen(writer)?;
        }
        if self.rng.chance(REOPEN_PPM) {
            let at = self.rng.pick(self.writers.len());
            self.reopen(at)?;
        }
        Ok(())
    }

    fn check_stale_reads(&self) -> Result<(), String> {
        for (workspace, fs) in &self.writers {
            let shown = fs.manifest();
            if shown.version() == self.seen.version(WORKSPACES[*workspace]) {
                continue;
            }
            for (path, entry) in shown.entries() {
                if !matches!(entry, Entry::File { .. }) {
                    continue;
                }
                if let Err(error) = reply(fs.read(path.as_bytes(), u64::MAX))?
                    && error.kind() == io::ErrorKind::InvalidData
                {
                    return Err(format!("{path} unreadable on a stale view: {error}"));
                }
            }
        }
        Ok(())
    }

    fn judge(
        &mut self,
        flight: &Flight,
        flights: &[Flight],
        crashed: &BTreeSet<usize>,
    ) -> Result<(), String> {
        let Some(result) = &flight.result else {
            return Ok(());
        };
        if let Err(error) = result {
            match error.kind() {
                io::ErrorKind::ResourceBusy => self.stats.stale += 1,
                io::ErrorKind::FileTooLarge | io::ErrorKind::StorageFull => self.stats.capped += 1,
                _ => {}
            }
        }
        match (flight.expect, result) {
            (Expect::TooLarge, Ok(())) => {
                return Err("a write over the file cap succeeded".to_owned());
            }
            (Expect::BadDigest, Ok(())) => {
                return Err("an upload with the wrong sha256 committed".to_owned());
            }
            _ => {}
        }
        let alone = flights
            .iter()
            .filter(|other| other.writer == flight.writer)
            .count()
            == 1;
        let (Ok(()), Some((path, bytes))) = (result, &flight.written) else {
            return Ok(());
        };
        if !alone || crashed.contains(&flight.writer) {
            return Ok(());
        }
        let (workspace, fs) = &self.writers[flight.writer];
        let stale = fs.manifest().version() != self.seen.version(WORKSPACES[*workspace]);
        let read = reply(fs.read(path.as_bytes(), u64::MAX))?;
        if stale {
            return match read {
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    Err(format!("{path} unreadable on a stale view: {error}"))
                }
                _ => Ok(()),
            };
        }
        let read = read.map_err(|error| format!("{path} unreadable after its write: {error}"))?;
        if read[..] != bytes[..] {
            return Err(format!(
                "{path} reads {} bytes after a write of {}",
                read.len(),
                bytes.len()
            ));
        }
        Ok(())
    }

    fn digest(&self) -> u64 {
        let heads: Vec<(&String, u64)> = self
            .seen
            .heads
            .iter()
            .map(|(name, (version, _))| (name, *version))
            .collect();
        let refs: Vec<(&String, i64)> = self
            .seen
            .blobs
            .iter()
            .map(|(digest, (_, refs))| (digest, *refs))
            .collect();
        let mut hash = fnv(FNV_OFFSET, format!("{heads:?}").as_bytes());
        hash = fnv(hash, format!("{:?}", self.seen.entries).as_bytes());
        hash = fnv(hash, format!("{refs:?}").as_bytes());
        fnv(hash, format!("{:?}", self.stats).as_bytes())
    }
}

pub struct WorkspaceSqlOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: WorkspaceSqlStats,
    shape: String,
}

fn world(seed: u64) -> (World, u64, String) {
    let mut rng = Rng::derive(seed, "workspace-sql");
    let file_bytes = rng.range(FILE_BYTES_MIN, FILE_BYTES_MAX);
    let caps = Caps {
        file_bytes,
        workspace_bytes: file_bytes * rng.range(WORKSPACE_FILES_MIN, WORKSPACE_FILES_MAX),
        paths: rng.range(PATHS_MIN, PATHS_MAX) as usize,
        ops: OPS_PER_COMMIT,
        path_bytes: PATH_BYTES,
    };
    let rounds = rng.range(ROUNDS_MIN, ROUNDS_MAX);
    let shape = format!(
        "rounds={rounds} file_bytes={} workspace_bytes={} paths={}",
        caps.file_bytes, caps.workspace_bytes, caps.paths
    );
    let contents = (0..CONTENTS)
        .map(|_| {
            let size = rng.range(1, file_bytes);
            (0..size).map(|_| rng.below(256) as u8).collect()
        })
        .collect();
    let host = Arc::new(SimSql {
        tables: Atom::new(Arc::new(Tables::default())),
        rng: Atom::new(Rng::derive(seed, "workspace-sql-host")),
        commits: Atom::new(Vec::new()),
        transactions: AtomicU64::new(0),
        aborts: AtomicU64::new(0),
        crashes_after: AtomicU64::new(0),
    });
    let objects = (fs_sim::mutated(FsRule::SqlSweepSkipsTombstone)
        || Rng::derive(seed, "workspace-sql-objects").chance(OBJECTS_PPM))
    .then(|| {
        Arc::new(SimObjects {
            held: Atom::new(BTreeMap::new()),
            now: Arc::new(AtomicU64::new(0)),
        })
    });
    let shape = format!("{shape} objects={}", objects.is_some());
    let world = World {
        rng,
        caps,
        host,
        intents: Arc::new(Atom::new(Vec::new())),
        acks: Arc::new(Atom::new(Vec::new())),
        clock: Rc::new(Clock(Cell::new(0))),
        writers: Vec::new(),
        contents,
        history: vec![BTreeMap::from([(0, BTreeMap::new())]); WORKSPACES.len()],
        seen: Arc::new(Tables::default()),
        pending: Vec::new(),
        stats: WorkspaceSqlStats {
            seeds: 1,
            ..WorkspaceSqlStats::default()
        },
        objects,
        sweeper: None,
        cursor: Rc::new(RefCell::new(String::new())),
        reaped: Rc::new(Cell::new(0)),
    };
    (world, rounds, shape)
}

fn play(world: &mut World, rounds: u64) -> Result<(), String> {
    for workspace in WRITERS {
        let opened = world
            .open(workspace)
            .map_err(|message| format!("open: {message}"))?;
        world.writers.push((workspace, opened));
    }
    world.open_sweeper()?;
    world.check_commits()?;
    for round in 0..rounds {
        world
            .round()
            .map_err(|message| format!("round {round}: {message}"))?;
    }
    for at in 0..world.writers.len() {
        world
            .reopen(at)
            .map_err(|message| format!("final reopen: {message}"))?;
    }
    Ok(())
}

fn simulate(seed: u64, mutation: WorkspaceSqlMutation) -> WorkspaceSqlOutcome {
    crate::host::enter(seed, &crate::executor::Clock::new());
    fs_sim::mutate(mutation.rule());
    let (mut world, rounds, shape) = world(seed);
    let wrong = play(&mut world, rounds).err();
    fs_sim::mutate(FsRule::None);
    world.stats.transactions = world.host.transactions.load(Ordering::Relaxed);
    world.stats.aborts = world.host.aborts.load(Ordering::Relaxed);
    world.stats.crashes_after = world.host.crashes_after.load(Ordering::Relaxed);
    world.stats.reaped = world.reaped.get();
    WorkspaceSqlOutcome {
        digest: world.digest(),
        wrong,
        stats: world.stats,
        shape,
    }
}

pub fn run_seed(seed: u64, mutation: WorkspaceSqlMutation) -> WorkspaceSqlOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        fs_sim::mutate(FsRule::None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        WorkspaceSqlOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: WorkspaceSqlStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: WorkspaceSqlMutation,
    twice: bool,
    verbose: bool,
    totals: &mut WorkspaceSqlStats,
) -> Option<String> {
    let first = run_seed(seed, mutation);
    totals.add(&first.stats);
    if verbose {
        println!(
            "seed {seed} digest {:016x} {} stats={:?}",
            first.digest, first.shape, first.stats
        );
    }
    if first.wrong.is_some() || !twice {
        return first
            .wrong
            .map(|message| format!("{} :: {message}", first.shape));
    }
    let second = run_seed(seed, mutation);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: WorkspaceSqlMutation) -> String {
    let mut command = format!("nanocode-sim --scenario workspace_sql --seed {seed}");
    if mutation != WorkspaceSqlMutation::None {
        command.push_str(" --mutation ");
        command.push_str(mutation.name());
    }
    command
}

pub fn report(
    seeds: &Range<u64>,
    passed: u64,
    failed: u64,
    twice: bool,
    mutation: WorkspaceSqlMutation,
    totals: &WorkspaceSqlStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=workspace_sql mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        WorkspaceSqlMutation::None if failed == 0 => ExitCode::SUCCESS,
        WorkspaceSqlMutation::None => ExitCode::FAILURE,
        _ if failed > 0 => {
            println!("MUTATION {} caught by {failed} seeds", mutation.name());
            ExitCode::SUCCESS
        }
        _ => {
            println!("MUTATION {} NOT caught", mutation.name());
            ExitCode::FAILURE
        }
    }
}
