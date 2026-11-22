use std::{ops::Range, sync::Arc};

use nanocode_durability_core::{Sql, SqlAccess, SqlStatement, StoreError, StoreFuture};
use nanocode_sync::Atom;

/// One scope's rows as `(seq, row)` pairs, in seq order.
pub type ScopeRows = Vec<(u64, String)>;

/// A reader's saved state for a scope at a seq, kept beside its rows: what it folded from
/// every row before seq, under a version the reader names. The log treats version and body as
/// opaque text and keeps one per scope, the last one saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCheckpoint {
    pub seq: u64,
    pub version: String,
    pub body: String,
}

#[derive(Clone)]
pub struct Compaction<'a> {
    pub archived: &'a [(u64, String)],
    pub dropped: Range<u64>,
    pub superseded: &'a [Range<u64>],
}

#[derive(Clone, Copy)]
pub struct Extension<'a> {
    pub cut: usize,
    pub row: u64,
    pub seq: u64,
    pub tail: &'a str,
}

pub enum Write<'a> {
    Append {
        rows: &'a [(u64, String)],
        compaction: Compaction<'a>,
    },
    Extend(Extension<'a>),
}

#[derive(Clone, Copy)]
pub struct Rider<'a> {
    pub host: usize,
    pub statements: &'a [SqlStatement<'a>],
}

pub trait LogHost: Send + Sync {
    fn scan<'a>(
        &'a self,
        scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>>;

    /// The rows of each scope from its seq on, in the order asked: what a partitioned reader catches up on at once.
    /// A host that can answers them all in one read; the default scans one scope after another.
    fn scans<'a>(
        &'a self,
        scans: &'a [(&'a str, u64)],
    ) -> StoreFuture<'a, Result<Vec<ScopeRows>, StoreError>> {
        Box::pin(async move {
            let mut rows = Vec::with_capacity(scans.len());
            for (scope, from) in scans {
                rows.push(self.scan(scope, *from, SCAN_END).await?);
            }
            Ok(rows)
        })
    }

    fn load<'a>(
        &'a self,
        scope: &'a str,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>>;

    fn append<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
    ) -> StoreFuture<'a, Result<(), StoreError>>;

    fn append_compacting<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
        _compaction: Compaction<'a>,
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        self.append(scope, rows)
    }

    fn extend<'a>(
        &'a self,
        _scope: &'a str,
        _extension: Extension<'a>,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        None
    }

    fn carry<'a>(
        &'a self,
        _scope: &'a str,
        _write: Write<'a>,
        _rider: Rider<'a>,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        None
    }

    /// The scope's saved checkpoint, if any; None when this host keeps no checkpoints.
    fn checkpoint<'a>(
        &'a self,
        _scope: &'a str,
    ) -> Option<StoreFuture<'a, Result<Option<StoredCheckpoint>, StoreError>>> {
        None
    }

    /// Replaces the scope's checkpoint; None when this host keeps no checkpoints.
    fn save_checkpoint<'a>(
        &'a self,
        _scope: &'a str,
        _checkpoint: &'a StoredCheckpoint,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        None
    }
}

/// How many bytes of a checkpoint's body one row holds: under the 2 MB a SQLite value may hold
/// on hosts that cap it, so a large fold's checkpoint spans rows.
pub const CHECKPOINT_PART_BYTES: usize = 1 << 20;

/// The exclusive end that scans every row from a seq on.
pub const SCAN_END: u64 = i64::MAX as u64;

type Dialects = [&'static str; 2];

/// Serializes schema preparation across processes on Postgres: two loads that create nanocode_log at once otherwise
/// race in CREATE TABLE IF NOT EXISTS (duplicate key in pg_type for the table's row type). SQLite has one writer.
const PREPARE_LOCK: Dialects = ["SELECT NULL", "SELECT CAST(pg_advisory_xact_lock(7_061_636_193) AS TEXT)"];
const CREATE: Dialects = [
    "CREATE TABLE IF NOT EXISTS nanocode_log (scope TEXT NOT NULL, seq INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY (scope, seq)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS nanocode_log (scope TEXT NOT NULL, seq BIGINT NOT NULL, body TEXT NOT NULL, PRIMARY KEY (scope, seq))",
];
const INDEX: Dialects = [
    "CREATE INDEX IF NOT EXISTS nanocode_log_snapshots ON nanocode_log (scope, seq) WHERE substr(body, 1, 1) = 's'",
    "CREATE INDEX IF NOT EXISTS nanocode_log_snapshots ON nanocode_log (scope, seq) WHERE substr(body, 1, 1) = 's'",
];
const LOAD: Dialects = [
    "SELECT CAST(seq AS TEXT), body FROM nanocode_log WHERE scope = ?1 AND seq >= COALESCE((SELECT seq FROM nanocode_log INDEXED BY nanocode_log_snapshots WHERE scope = ?1 AND substr(body, 1, 1) = 's' ORDER BY seq DESC LIMIT 1), 0) ORDER BY seq",
    "SELECT CAST(seq AS TEXT), body FROM nanocode_log WHERE scope = $1::text AND seq >= COALESCE((SELECT seq FROM nanocode_log WHERE scope = $1::text AND substr(body, 1, 1) = 's' ORDER BY seq DESC LIMIT 1), 0) ORDER BY nanocode_log.seq",
];
const SCAN: Dialects = [
    "SELECT CAST(seq AS TEXT), body FROM nanocode_log WHERE scope = ?1 AND seq >= CAST(?2 AS INTEGER) AND seq < CAST(?3 AS INTEGER) ORDER BY seq",
    "SELECT CAST(seq AS TEXT), body FROM nanocode_log WHERE scope = $1::text AND seq >= CAST($2::text AS BIGINT) AND seq < CAST($3::text AS BIGINT) ORDER BY nanocode_log.seq",
];
const APPEND: Dialects = [
    "INSERT INTO nanocode_log (scope, seq, body) VALUES (?1, CAST(?2 AS INTEGER), ?3)",
    "INSERT INTO nanocode_log (scope, seq, body) VALUES ($1::text, CAST($2::text AS BIGINT), $3::text)",
];
const ARCHIVE: Dialects = [
    "UPDATE nanocode_log SET body = ?3 WHERE scope = ?1 AND seq = CAST(?2 AS INTEGER)",
    "UPDATE nanocode_log SET body = $3::text WHERE scope = $1::text AND seq = CAST($2::text AS BIGINT)",
];
const EXTEND: Dialects = [
    "UPDATE nanocode_log SET seq = CAST(?3 AS INTEGER), body = substr(CAST(body AS BLOB), 1, length(CAST(body AS BLOB)) - CAST(?4 AS INTEGER)) || ?5 WHERE scope = ?1 AND seq = CAST(?2 AS INTEGER) RETURNING CAST(seq AS TEXT)",
    "UPDATE nanocode_log SET seq = CAST($3::text AS BIGINT), body = substr(body, 1, length(body) - CAST($4::text AS INTEGER)) || $5::text WHERE scope = $1::text AND seq = CAST($2::text AS BIGINT) RETURNING CAST(seq AS TEXT)",
];
pub(crate) const CHECKPOINT_CREATE: Dialects = [
    "CREATE TABLE IF NOT EXISTS nanocode_checkpoint (scope TEXT NOT NULL, part INTEGER NOT NULL, parts INTEGER NOT NULL, seq INTEGER NOT NULL, version TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY (scope, part)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS nanocode_checkpoint (scope TEXT NOT NULL, part INTEGER NOT NULL, parts INTEGER NOT NULL, seq BIGINT NOT NULL, version TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY (scope, part))",
];
const CHECKPOINT_LOAD: Dialects = [
    "SELECT CAST(part AS TEXT), CAST(parts AS TEXT), CAST(seq AS TEXT), version, body FROM nanocode_checkpoint WHERE scope = ?1 ORDER BY part",
    "SELECT CAST(part AS TEXT), CAST(parts AS TEXT), CAST(seq AS TEXT), version, body FROM nanocode_checkpoint WHERE scope = $1::text ORDER BY part",
];
const CHECKPOINT_CLEAR: Dialects = [
    "DELETE FROM nanocode_checkpoint WHERE scope = ?1",
    "DELETE FROM nanocode_checkpoint WHERE scope = $1::text",
];
const CHECKPOINT_SAVE: Dialects = [
    "INSERT INTO nanocode_checkpoint (scope, part, parts, seq, version, body) VALUES (?1, CAST(?2 AS INTEGER), CAST(?3 AS INTEGER), CAST(?4 AS INTEGER), ?5, ?6)",
    "INSERT INTO nanocode_checkpoint (scope, part, parts, seq, version, body) VALUES ($1::text, CAST($2::text AS INTEGER), CAST($3::text AS INTEGER), CAST($4::text AS BIGINT), $5::text, $6::text)",
];
const DROP: Dialects = [
    "DELETE FROM nanocode_log WHERE scope = ?1 AND seq >= CAST(?2 AS INTEGER) AND seq < CAST(?3 AS INTEGER)",
    "DELETE FROM nanocode_log WHERE scope = $1::text AND seq >= CAST($2::text AS BIGINT) AND seq < CAST($3::text AS BIGINT)",
];
pub struct SqlLog<H: ?Sized> {
    /// Set once this log created its table and index, so later loads read without the schema lock.
    prepared: std::sync::atomic::AtomicBool,
    host: Arc<H>,
}

impl<H: Sql + ?Sized> SqlLog<H> {
    pub const fn new(host: Arc<H>) -> Self {
        Self {
            prepared: std::sync::atomic::AtomicBool::new(false),
            host,
        }
    }

    /// Creates nanocode_log, its index and nanocode_checkpoint under PREPARE_LOCK, once per log.
    async fn prepare(&self) -> Result<(), StoreError> {
        use std::sync::atomic::Ordering;
        if self.prepared.load(Ordering::Acquire) {
            return Ok(());
        }
        let statements =
            [&PREPARE_LOCK, &CREATE, &INDEX, &CHECKPOINT_CREATE].map(|statement| SqlStatement {
                sql: self.pick(statement),
                params: Vec::new(),
            });
        self.host
            .transaction("turn_log_prepare", &statements)
            .await?;
        self.prepared.store(true, Ordering::Release);
        Ok(())
    }

    fn pick(&self, statements: &Dialects) -> &'static str {
        self.host.dialect().pick(statements)
    }

    fn appending<'a>(
        &self,
        scope: &'a str,
        rows: &'a [(u64, String)],
        compaction: &Compaction<'a>,
        seqs: &'a [String],
        ranges: &'a [(String, String)],
    ) -> Vec<SqlStatement<'a>> {
        let mut statements = rows
            .iter()
            .chain(compaction.archived)
            .zip(seqs)
            .enumerate()
            .map(|(index, ((_, body), seq))| SqlStatement {
                sql: self.pick(if index < rows.len() {
                    &APPEND
                } else {
                    &ARCHIVE
                }),
                params: vec![scope, seq, body],
            })
            .collect::<Vec<_>>();
        for (from, to) in ranges {
            statements.push(SqlStatement {
                sql: self.pick(&DROP),
                params: vec![scope, from, to],
            });
        }
        statements
    }
}

pub fn host_id<H: ?Sized>(host: &Arc<H>) -> usize {
    Arc::as_ptr(host).cast::<()>() as usize
}

fn seqs(
    rows: &[(u64, String)],
    compaction: &Compaction<'_>,
) -> (Vec<String>, Vec<(String, String)>) {
    (
        rows.iter()
            .chain(compaction.archived)
            .map(|(seq, _)| seq.to_string())
            .collect(),
        std::iter::once(&compaction.dropped)
            .chain(compaction.superseded)
            .filter(|range| !range.is_empty())
            .map(|range| (range.start.to_string(), range.end.to_string()))
            .collect(),
    )
}

fn extending(extension: &Extension<'_>) -> [String; 3] {
    [
        extension.row.to_string(),
        extension.seq.to_string(),
        extension.cut.to_string(),
    ]
}

fn invalid(detail: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(format!(
        "SQL host answered an invalid turn log row: {detail}"
    ))
}

fn row(cells: Vec<Option<String>>) -> Result<(u64, String), StoreError> {
    let count = cells.len();
    match <[Option<String>; 2]>::try_from(cells) {
        Ok([Some(seq), Some(body)]) => Ok((
            seq.parse()
                .map_err(|error| invalid(format!("seq {seq}: {error}")))?,
            body,
        )),
        _ => Err(invalid(format!("{count} cells"))),
    }
}

impl<H: Sql + Send + Sync + ?Sized> LogHost for SqlLog<H> {
    fn scan<'a>(
        &'a self,
        scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        Box::pin(async move {
            let (from, to) = (from.to_string(), to.to_string());
            let statements = [SqlStatement {
                sql: self.pick(&SCAN),
                params: vec![scope, &from, &to],
            }];
            // A read: on SQLite it shares the file with writers instead of taking the write lock.
            let rows = self
                .host
                .run(SqlAccess::Read, "turn_log_scan", &statements)
                .await?;
            rows.into_iter()
                .next()
                .ok_or_else(|| invalid("scan returned no result"))?
                .into_iter()
                .map(row)
                .collect()
        })
    }

    fn scans<'a>(
        &'a self,
        scans: &'a [(&'a str, u64)],
    ) -> StoreFuture<'a, Result<Vec<ScopeRows>, StoreError>> {
        Box::pin(async move {
            if scans.is_empty() {
                return Ok(Vec::new());
            }
            let bounds: Vec<(String, String)> = scans
                .iter()
                .map(|(_, from)| (from.to_string(), SCAN_END.to_string()))
                .collect();
            let statements: Vec<SqlStatement<'_>> = scans
                .iter()
                .zip(&bounds)
                .map(|((scope, _), (from, to))| SqlStatement {
                    sql: self.pick(&SCAN),
                    params: vec![*scope, from.as_str(), to.as_str()],
                })
                .collect();
            let results = self
                .host
                .run(SqlAccess::Read, "turn_log_scans", &statements)
                .await?;
            if results.len() != scans.len() {
                return Err(invalid(format!(
                    "{} scans returned {} results",
                    scans.len(),
                    results.len()
                )));
            }
            results
                .into_iter()
                .map(|rows| rows.into_iter().map(row).collect())
                .collect()
        })
    }

    fn load<'a>(
        &'a self,
        scope: &'a str,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        Box::pin(async move {
            self.prepare().await?;
            let statements = [SqlStatement {
                sql: self.pick(&LOAD),
                params: vec![scope],
            }];
            let rows = self.host.transaction("turn_log_load", &statements).await?;
            rows.into_iter()
                .next()
                .ok_or_else(|| invalid("load returned no result"))?
                .into_iter()
                .map(row)
                .collect()
        })
    }

    fn append<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        self.append_compacting(
            scope,
            rows,
            Compaction {
                archived: &[],
                dropped: 0..0,
                superseded: &[],
            },
        )
    }

    fn append_compacting<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
        compaction: Compaction<'a>,
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let (seqs, dropped) = seqs(rows, &compaction);
            let statements = self.appending(scope, rows, &compaction, &seqs, &dropped);
            self.host
                .transaction("turn_log_append", &statements)
                .await
                .map(drop)
        })
    }

    fn extend<'a>(
        &'a self,
        scope: &'a str,
        extension: Extension<'a>,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        Some(Box::pin(async move {
            let [row, seq, cut] = extending(&extension);
            let statements = [SqlStatement {
                sql: self.pick(&EXTEND),
                params: vec![scope, &row, &seq, &cut, extension.tail],
            }];
            let rows = self
                .host
                .transaction("turn_log_extend", &statements)
                .await?;
            match rows.first() {
                Some(moved) if !moved.is_empty() => Ok(()),
                _ => Err(StoreError::Fenced),
            }
        }))
    }

    fn carry<'a>(
        &'a self,
        scope: &'a str,
        write: Write<'a>,
        rider: Rider<'a>,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        if rider.host != host_id(&self.host) {
            return None;
        }
        Some(Box::pin(async move {
            match write {
                Write::Append { rows, compaction } => {
                    let (seqs, dropped) = seqs(rows, &compaction);
                    let mut statements = self.appending(scope, rows, &compaction, &seqs, &dropped);
                    statements.extend(rider.statements.iter().cloned());
                    self.host
                        .transaction("turn_log_carry", &statements)
                        .await
                        .map(drop)
                }
                Write::Extend(extension) => {
                    let [row, seq, cut] = extending(&extension);
                    let mut statements = vec![SqlStatement {
                        sql: self.pick(&EXTEND),
                        params: vec![scope, &row, &seq, &cut, extension.tail],
                    }];
                    statements.extend(rider.statements.iter().cloned());
                    let rows = self.host.transaction("turn_log_carry", &statements).await?;
                    match rows.first() {
                        Some(moved) if !moved.is_empty() => Ok(()),
                        _ => Err(StoreError::Fenced),
                    }
                }
            }
        }))
    }

    fn checkpoint<'a>(
        &'a self,
        scope: &'a str,
    ) -> Option<StoreFuture<'a, Result<Option<StoredCheckpoint>, StoreError>>> {
        Some(Box::pin(async move {
            self.prepare().await?;
            load_checkpoint(&*self.host, scope).await
        }))
    }

    fn save_checkpoint<'a>(
        &'a self,
        scope: &'a str,
        checkpoint: &'a StoredCheckpoint,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        Some(Box::pin(async move {
            self.prepare().await?;
            store_checkpoint(&*self.host, scope, checkpoint).await
        }))
    }
}

/// The scope's checkpoint from nanocode_checkpoint, which the caller has created.
pub(crate) async fn load_checkpoint<H: Sql + ?Sized>(
    host: &H,
    scope: &str,
) -> Result<Option<StoredCheckpoint>, StoreError> {
    let statements = [SqlStatement {
        sql: host.dialect().pick(&CHECKPOINT_LOAD),
        params: vec![scope],
    }];
    let rows = host
        .run(SqlAccess::Read, "turn_log_checkpoint", &statements)
        .await?;
    let parts = rows
        .into_iter()
        .next()
        .ok_or_else(|| invalid("checkpoint load returned no result"))?;
    joined(parts)
}

/// Replaces the scope's checkpoint in nanocode_checkpoint, which the caller has created, in one
/// transaction, so a reader sees the old one or the new one whole.
pub(crate) async fn store_checkpoint<H: Sql + ?Sized>(
    host: &H,
    scope: &str,
    checkpoint: &StoredCheckpoint,
) -> Result<(), StoreError> {
    let bodies = split(&checkpoint.body, CHECKPOINT_PART_BYTES);
    let numbers: Vec<String> = (0..bodies.len()).map(|part| part.to_string()).collect();
    let (parts, seq) = (bodies.len().to_string(), checkpoint.seq.to_string());
    let mut statements = vec![SqlStatement {
        sql: host.dialect().pick(&CHECKPOINT_CLEAR),
        params: vec![scope],
    }];
    for (number, body) in numbers.iter().zip(&bodies) {
        statements.push(SqlStatement {
            sql: host.dialect().pick(&CHECKPOINT_SAVE),
            params: vec![scope, number, &parts, &seq, &checkpoint.version, body],
        });
    }
    host.transaction("turn_log_save_checkpoint", &statements)
        .await
        .map(drop)
}

/// Splits a body into pieces of at most limit bytes, each ending on a char boundary; an empty
/// body is one empty piece.
fn split(body: &str, limit: usize) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = body;
    loop {
        if rest.len() <= limit {
            pieces.push(rest);
            return pieces;
        }
        let mut at = limit.max(1);
        while !rest.is_char_boundary(at) {
            at -= 1;
        }
        if at == 0 {
            at = rest.chars().next().map_or(rest.len(), char::len_utf8);
        }
        let (piece, tail) = rest.split_at(at);
        pieces.push(piece);
        rest = tail;
    }
}

/// Joins a checkpoint's rows, in part order, into one; none when the scope has no checkpoint
/// or its rows don't make one whole checkpoint.
fn joined(rows: Vec<Vec<Option<String>>>) -> Result<Option<StoredCheckpoint>, StoreError> {
    let total = rows.len() as u64;
    let mut joined: Option<StoredCheckpoint> = None;
    for (index, cells) in rows.into_iter().enumerate() {
        let count = cells.len();
        let Ok(
            [
                Some(part),
                Some(parts),
                Some(seq),
                Some(version),
                Some(body),
            ],
        ) = <[Option<String>; 5]>::try_from(cells)
        else {
            return Err(invalid(format!("checkpoint row with {count} cells")));
        };
        let number = |text: &str| {
            text.parse::<u64>()
                .map_err(|error| invalid(format!("checkpoint {text}: {error}")))
        };
        let (part, parts, seq) = (number(&part)?, number(&parts)?, number(&seq)?);
        if part != index as u64 || parts != total {
            return Ok(None);
        }
        match &mut joined {
            None => {
                joined = Some(StoredCheckpoint { seq, version, body });
            }
            Some(whole) if whole.seq == seq && whole.version == version => {
                whole.body.push_str(&body);
            }
            Some(_) => return Ok(None),
        }
    }
    Ok(joined)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemoryWrites {
    pub bytes: u64,
    pub rows: u64,
}

#[derive(Clone, Default)]
struct Memory {
    rows: nanocode_sync::OrdMap<(Arc<str>, u64), Arc<str>>,
    checkpoints: nanocode_sync::OrdMap<Arc<str>, Arc<StoredCheckpoint>>,
    writes: MemoryWrites,
}

#[derive(Default)]
pub struct MemoryLog {
    memory: Atom<Memory>,
}

impl MemoryLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn writes(&self) -> MemoryWrites {
        self.memory.read(|memory| memory.writes)
    }

    pub fn truncate(&self, seq: u64) {
        self.memory.update(|memory| {
            memory.rows = memory
                .rows
                .iter()
                .filter(|((_, row), _)| *row <= seq)
                .map(|(key, body)| (key.clone(), Arc::clone(body)))
                .collect();
        });
    }
}

impl LogHost for MemoryLog {
    fn scan<'a>(
        &'a self,
        scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        let rows = self.memory.read(|memory| {
            let scope: Arc<str> = Arc::from(scope);
            memory
                .rows
                .range((Arc::clone(&scope), from)..(scope, to))
                .map(|((_, seq), body)| (*seq, body.to_string()))
                .collect()
        });
        Box::pin(async move { Ok(rows) })
    }

    fn load<'a>(
        &'a self,
        scope: &'a str,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        let rows = self.memory.read(|memory| {
            let scope: Arc<str> = Arc::from(scope);
            let mut rows = Vec::new();
            for ((_, seq), body) in memory
                .rows
                .range((Arc::clone(&scope), 0)..=(scope, u64::MAX))
                .rev()
            {
                rows.push((*seq, body.to_string()));
                if body.starts_with('s') {
                    break;
                }
            }
            rows.reverse();
            rows
        });
        Box::pin(async move { Ok(rows) })
    }

    fn append<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        self.append_compacting(
            scope,
            rows,
            Compaction {
                archived: &[],
                dropped: 0..0,
                superseded: &[],
            },
        )
    }

    fn append_compacting<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
        compaction: Compaction<'a>,
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        let scope = Arc::<str>::from(scope);
        let result = self.memory.update(|memory| {
            if rows
                .iter()
                .any(|(seq, _)| memory.rows.contains_key(&(Arc::clone(&scope), *seq)))
            {
                return Err(StoreError::Fenced);
            }
            for (seq, body) in rows.iter().chain(compaction.archived) {
                memory
                    .rows
                    .insert((Arc::clone(&scope), *seq), Arc::from(body.as_str()));
                memory.writes.rows += 1;
                memory.writes.bytes += body.len() as u64;
            }
            for seq in std::iter::once(&compaction.dropped)
                .chain(compaction.superseded)
                .flat_map(Clone::clone)
            {
                memory.rows.remove(&(Arc::clone(&scope), seq));
            }
            Ok(())
        });
        Box::pin(async move { result })
    }

    fn extend<'a>(
        &'a self,
        scope: &'a str,
        extension: Extension<'a>,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        let scope = Arc::<str>::from(scope);
        let result = self.memory.update(|memory| {
            let moved = (Arc::clone(&scope), extension.seq);
            if memory.rows.contains_key(&moved) {
                return Err(StoreError::Fenced);
            }
            let row = (Arc::clone(&scope), extension.row);
            let Some(body) = memory.rows.get(&row) else {
                return Err(StoreError::Fenced);
            };
            let Some(kept) = body.len().checked_sub(extension.cut) else {
                return Err(StoreError::Backend(format!(
                    "turn log row {} is shorter than its {}-byte seal",
                    extension.row, extension.cut
                )));
            };
            let body = format!("{}{}", &body[..kept], extension.tail);
            memory.rows.remove(&row);
            memory.writes.rows += 1;
            memory.writes.bytes += body.len() as u64;
            memory.rows.insert(moved, Arc::from(body));
            Ok(())
        });
        Some(Box::pin(async move { result }))
    }

    fn checkpoint<'a>(
        &'a self,
        scope: &'a str,
    ) -> Option<StoreFuture<'a, Result<Option<StoredCheckpoint>, StoreError>>> {
        let found = self
            .memory
            .read(|memory| memory.checkpoints.get(scope).map(|saved| (**saved).clone()));
        Some(Box::pin(async move { Ok(found) }))
    }

    fn save_checkpoint<'a>(
        &'a self,
        scope: &'a str,
        checkpoint: &'a StoredCheckpoint,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        let saved = Arc::new(checkpoint.clone());
        self.memory.update(|memory| {
            memory
                .checkpoints
                .insert(Arc::from(scope), Arc::clone(&saved));
        });
        Some(Box::pin(async move { Ok(()) }))
    }
}
