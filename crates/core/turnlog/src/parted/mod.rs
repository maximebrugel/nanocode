//! One fenced, partitioned, group-committed log host for every log kind.
//!
//! Every scope (a session's turn log, a board slice, a thread's records, a cost log) lands in one
//! partition. A node appends to a partition only while it holds that partition's fence (one lease
//! row per partition, nanocode_partitions::Row), and concurrent appends to one partition from any
//! scope and any log kind go down as one transaction: each append is all-or-nothing, a seq that
//! already exists rejects only its own append, and a stale fence rejects the whole batch. The
//! store behind it is a PartStore: SqlParts for Postgres and SQLite, ModelParts for the simulator.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    num::NonZeroU32,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use nanocode_durability_core::{StoreError, StoreFuture};
use nanocode_partitions::{Partition, Row, partition_of};
use nanocode_sync::oneshot;

use crate::host::{Compaction, Extension, LogHost, Rider, Write};

pub mod model;
pub mod sim;
pub mod sql;

pub use model::{Fault, Faults, ModelParts};
pub use sql::SqlParts;

pub type Rows = Vec<(u64, String)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Append {
    pub scope: Arc<str>,
    pub rows: Rows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Committed,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub pos: u64,
    pub scope: Arc<str>,
    pub seq: u64,
    pub body: String,
}

/// The storage a PartedLog runs on. The epoch is the fence token a write carries: None writes
/// unfenced (single-writer deployments), Some(epoch) is admitted only while the partition's fence
/// row is held at that epoch.
pub trait PartStore: Send + Sync {
    fn prepare(&self, count: NonZeroU32) -> StoreFuture<'_, Result<(), StoreError>>;

    /// One transaction: the fence check, then every append all-or-nothing, each row given the
    /// next position. A stale fence is Err(Fenced) for the whole batch.
    fn commit<'a>(
        &'a self,
        part: Partition,
        epoch: Option<u64>,
        batch: &'a [Append],
    ) -> StoreFuture<'a, Result<Vec<Verdict>, StoreError>>;

    fn scan<'a>(
        &'a self,
        part: Partition,
        scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Rows, StoreError>>;

    /// The scope's latest snapshot row (a body starting with s) and every row after it.
    fn load<'a>(
        &'a self,
        part: Partition,
        scope: &'a str,
    ) -> StoreFuture<'a, Result<Rows, StoreError>>;

    /// Rewrites archived rows and drops superseded ranges, never a row a cursor still needs.
    fn compact<'a>(
        &'a self,
        part: Partition,
        epoch: Option<u64>,
        scope: &'a str,
        compaction: Compaction<'a>,
    ) -> StoreFuture<'a, Result<(), StoreError>>;

    fn extend<'a>(
        &'a self,
        part: Partition,
        epoch: Option<u64>,
        scope: &'a str,
        extension: Extension<'a>,
    ) -> StoreFuture<'a, Result<(), StoreError>>;

    /// Rows of the partition after position after, in position order: the follower feed.
    fn follow(
        &self,
        part: Partition,
        after: u64,
        limit: usize,
    ) -> StoreFuture<'_, Result<Vec<Entry>, StoreError>>;

    fn fence(&self, part: Partition) -> StoreFuture<'_, Result<Row<String>, StoreError>>;

    /// Replaces the fence row only if it still is expected.
    fn swap_fence<'a>(
        &'a self,
        part: Partition,
        expected: &'a Row<String>,
        next: &'a Row<String>,
    ) -> StoreFuture<'a, Result<bool, StoreError>>;

    /// Records that reader consumed the partition through pos (it never moves back).
    fn cursor<'a>(
        &'a self,
        part: Partition,
        reader: &'a str,
        pos: u64,
    ) -> StoreFuture<'a, Result<(), StoreError>>;

    /// Drops the scope's rows below before, never the latest snapshot or anything after it, and
    /// never a row some cursor of the partition has not consumed. Returns the rows dropped.
    fn prune<'a>(
        &'a self,
        part: Partition,
        epoch: Option<u64>,
        scope: &'a str,
        before: u64,
    ) -> StoreFuture<'a, Result<u64, StoreError>>;
}

pub type Place = Arc<dyn Fn(&str, NonZeroU32) -> Partition + Send + Sync>;
pub type RingFn = Arc<dyn Fn(&str, u64) + Send + Sync>;

/// Where a scope lives: a "/p<n>" suffix names partition n (board slices), a UUID segment hashes
/// its 16 bytes (everything a thread owns lands beside it), anything else hashes the scope's
/// bytes. The hash is nanocode_partitions::partition_of.
#[must_use]
pub fn place(scope: &str, count: NonZeroU32) -> Partition {
    if let Some((_, tail)) = scope.rsplit_once("/p")
        && let Ok(n) = tail.parse::<u32>()
        && n < count.get()
    {
        return Partition(n);
    }
    if let Some(bytes) = scope.split(['/', ':', '@']).find_map(uuid_bytes) {
        return partition_of(&bytes, count);
    }
    partition_of(scope.as_bytes(), count)
}

fn uuid_bytes(segment: &str) -> Option<[u8; 16]> {
    if segment.len() != 36 {
        return None;
    }
    let mut bytes = [0_u8; 16];
    let mut digits = segment.bytes().filter(|byte| *byte != b'-');
    for byte in &mut bytes {
        let high = char::from(digits.next()?).to_digit(16)?;
        let low = char::from(digits.next()?).to_digit(16)?;
        *byte = u8::try_from(high * 16 + low).ok()?;
    }
    digits.next().is_none().then_some(bytes)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub commits: u64,
    pub appends: u64,
    pub rows: u64,
    pub conflicts: u64,
    pub fenced: u64,
}

enum Turn {
    Done(Result<(), StoreError>),
    Lead(Append),
}

struct Waiter {
    append: Append,
    reply: oneshot::Sender<Turn>,
}

#[derive(Default)]
struct Queue {
    leading: bool,
    waiting: VecDeque<Waiter>,
}

pub struct PartedLog<S: ?Sized> {
    store: Arc<S>,
    count: NonZeroU32,
    place: Place,
    node: Option<Arc<str>>,
    held: Mutex<HashMap<Partition, u64>>,
    queues: Mutex<HashMap<Partition, Queue>>,
    ring: Mutex<Option<RingFn>>,
    stats: Mutex<Stats>,
    max_rows: usize,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn uncertain() -> StoreError {
    StoreError::Backend("group commit ended without an answer; the append may have landed".into())
}

impl<S: PartStore + ?Sized> PartedLog<S> {
    /// An unfenced host: one writer per deployment, appends still grouped per partition.
    pub fn new(store: Arc<S>, count: NonZeroU32) -> Self {
        Self {
            store,
            count,
            place: Arc::new(place),
            node: None,
            held: Mutex::new(HashMap::new()),
            queues: Mutex::new(HashMap::new()),
            ring: Mutex::new(None),
            stats: Mutex::new(Stats::default()),
            max_rows: 512,
        }
    }

    /// A fenced host for node: it appends only to partitions it claimed.
    pub fn fenced(store: Arc<S>, count: NonZeroU32, node: &str) -> Self {
        Self {
            node: Some(Arc::from(node)),
            ..Self::new(store, count)
        }
    }

    #[must_use]
    pub fn with_place(mut self, place: Place) -> Self {
        self.place = place;
        self
    }

    #[must_use]
    pub fn with_max_rows(mut self, max_rows: usize) -> Self {
        self.max_rows = max_rows.max(1);
        self
    }

    /// Calls ring(scope, head) after every committed append, for local watchers. Postgres also
    /// notifies other nodes on the nanocode_log channel inside the transaction.
    pub fn set_ring(&self, ring: RingFn) {
        *lock(&self.ring) = Some(ring);
    }

    pub const fn store(&self) -> &Arc<S> {
        &self.store
    }

    pub const fn count(&self) -> NonZeroU32 {
        self.count
    }

    pub fn partition(&self, scope: &str) -> Partition {
        (self.place)(scope, self.count)
    }

    pub fn stats(&self) -> Stats {
        *lock(&self.stats)
    }

    pub fn held(&self, part: Partition) -> Option<u64> {
        lock(&self.held).get(&part).copied()
    }

    pub async fn prepare(&self) -> Result<(), StoreError> {
        self.store.prepare(self.count).await
    }

    fn epoch(&self, part: Partition) -> Result<Option<u64>, StoreError> {
        if self.node.is_none() {
            return Ok(None);
        }
        self.held(part).map(Some).ok_or(StoreError::Fenced)
    }

    fn node(&self) -> Result<String, StoreError> {
        self.node.as_deref().map(str::to_owned).ok_or_else(|| {
            StoreError::Backend("an unfenced log host has no node to claim for".into())
        })
    }

    /// Claims the partition's fence row for this node, raising its epoch. holder_live says
    /// whether the current holder is still a live member, so a dead holder's row is taken before
    /// it expires.
    pub async fn claim(
        &self,
        part: Partition,
        now_ms: u64,
        ttl_ms: u64,
        holder_live: bool,
    ) -> Result<u64, StoreError> {
        let node = self.node()?;
        let row = self.store.fence(part).await?;
        let (next, epoch) = row
            .claim(&node, now_ms, ttl_ms, holder_live)
            .map_err(|_| StoreError::Fenced)?;
        if !self.store.swap_fence(part, &row, &next).await? {
            return Err(StoreError::Fenced);
        }
        lock(&self.held).insert(part, epoch);
        Ok(epoch)
    }

    pub async fn renew(&self, part: Partition, now_ms: u64, ttl_ms: u64) -> Result<(), StoreError> {
        let node = self.node()?;
        let epoch = self.epoch(part)?.ok_or(StoreError::Fenced)?;
        let row = self.store.fence(part).await?;
        let renewed = match row.renew(part, &node, epoch, now_ms, ttl_ms) {
            Ok(next) => self.store.swap_fence(part, &row, &next).await?,
            Err(_) => false,
        };
        if renewed {
            Ok(())
        } else {
            self.forget(part);
            Err(StoreError::Fenced)
        }
    }

    /// Stops admitting appends to the partition, waits for its committer to drain (a commit in
    /// flight lands or fails), then frees the fence row at the epoch it held.
    pub async fn release(&self, part: Partition) -> Result<(), StoreError> {
        let node = self.node()?;
        let Some(epoch) = lock(&self.held).remove(&part) else {
            return Ok(());
        };
        self.drain(part).await;
        let row = self.store.fence(part).await?;
        let released = match row.release(part, &node, epoch) {
            Ok(next) => self.store.swap_fence(part, &row, &next).await?,
            Err(_) => false,
        };
        if released {
            Ok(())
        } else {
            Err(StoreError::Fenced)
        }
    }

    /// Forgets the partition without touching its row (the node lost it).
    pub fn forget(&self, part: Partition) {
        lock(&self.held).remove(&part);
    }

    /// Resolves once every append queued on the partition before the call has its answer.
    pub async fn drain(&self, part: Partition) {
        let _ = self
            .submit(
                part,
                Append {
                    scope: Arc::from(""),
                    rows: Vec::new(),
                },
            )
            .await;
    }

    pub async fn follow(
        &self,
        part: Partition,
        after: u64,
        limit: usize,
    ) -> Result<Vec<Entry>, StoreError> {
        self.store.follow(part, after, limit).await
    }

    pub async fn cursor(&self, part: Partition, reader: &str, pos: u64) -> Result<(), StoreError> {
        self.store.cursor(part, reader, pos).await
    }

    pub async fn prune(&self, scope: &str, before: u64) -> Result<u64, StoreError> {
        let part = self.partition(scope);
        let epoch = self.epoch(part)?;
        self.store.prune(part, epoch, scope, before).await
    }

    async fn submit(&self, part: Partition, append: Append) -> Result<(), StoreError> {
        let (reply, mut answer) = oneshot::channel();
        let lead = {
            let mut queues = lock(&self.queues);
            let queue = queues.entry(part).or_default();
            queue.waiting.push_back(Waiter { append, reply });
            !std::mem::replace(&mut queue.leading, true)
        };
        if !lead {
            match (&mut answer).await {
                Ok(Turn::Done(result)) => return result,
                Ok(Turn::Lead(append)) => {
                    let (reply, next) = oneshot::channel();
                    lock(&self.queues)
                        .entry(part)
                        .or_default()
                        .waiting
                        .push_front(Waiter { append, reply });
                    answer = next;
                }
                Err(_) => return Err(uncertain()),
            }
        }
        self.lead(part).await;
        match answer.await {
            Ok(Turn::Done(result)) => result,
            Ok(Turn::Lead(_)) | Err(_) => Err(uncertain()),
        }
    }

    async fn lead(&self, part: Partition) {
        let mut leading = Leading {
            queues: &self.queues,
            part,
            done: false,
        };
        loop {
            let batch = {
                let mut queues = lock(&self.queues);
                let queue = queues.entry(part).or_default();
                let mut batch = Vec::new();
                let mut rows = 0;
                while let Some(waiter) = queue.waiting.front() {
                    if !batch.is_empty() && rows + waiter.append.rows.len() > self.max_rows {
                        break;
                    }
                    rows += waiter.append.rows.len();
                    batch.extend(queue.waiting.pop_front());
                }
                if batch.is_empty() {
                    queue.leading = false;
                    leading.done = true;
                    return;
                }
                batch
            };
            self.commit(part, batch).await;
        }
    }

    async fn commit(&self, part: Partition, batch: Vec<Waiter>) {
        let mut seen: HashSet<(Arc<str>, u64)> = HashSet::new();
        let mut sent = Vec::new();
        let mut slots = Vec::with_capacity(batch.len());
        for waiter in &batch {
            let append = &waiter.append;
            if append.rows.is_empty() {
                slots.push(None);
                continue;
            }
            let clash = append
                .rows
                .iter()
                .any(|(seq, _)| seen.contains(&(Arc::clone(&append.scope), *seq)));
            if clash && !sim::mutated(sim::Mutation::BatchDuplicates) {
                slots.push(Some(Err(StoreError::Fenced)));
                continue;
            }
            seen.extend(
                append
                    .rows
                    .iter()
                    .map(|(seq, _)| (Arc::clone(&append.scope), *seq)),
            );
            slots.push(Some(Ok(sent.len())));
            sent.push(append.clone());
        }
        let answer = if sent.is_empty() {
            Ok(Vec::new())
        } else {
            match self.epoch(part) {
                Ok(epoch) => self.store.commit(part, epoch, &sent).await,
                Err(error) => Err(error),
            }
        };
        let answer = match answer {
            Err(StoreError::Backend(_)) if sim::mutated(sim::Mutation::LostAckRejected) => {
                Ok(vec![Verdict::Conflict; sent.len()])
            }
            answer => answer,
        };
        let fenced = matches!(answer, Err(StoreError::Fenced));
        if fenced {
            self.forget(part);
        }
        let ring = lock(&self.ring).clone();
        let mut stats = Stats {
            commits: u64::from(!sent.is_empty()),
            fenced: u64::from(fenced),
            ..Stats::default()
        };
        let mut rung = 0;
        for (waiter, slot) in batch.into_iter().zip(slots) {
            let result = match slot {
                None => Ok(()),
                Some(Err(error)) => Err(error),
                Some(Ok(at)) => match &answer {
                    Ok(verdicts) => match verdicts.get(at) {
                        Some(Verdict::Committed) => Ok(()),
                        Some(Verdict::Conflict) => Err(StoreError::Fenced),
                        None => Err(uncertain()),
                    },
                    Err(error) => Err(error.clone()),
                },
            };
            match &result {
                Ok(()) if !waiter.append.rows.is_empty() => {
                    stats.appends += 1;
                    stats.rows += waiter.append.rows.len() as u64;
                    rung += 1;
                    let missed = sim::mutated(sim::Mutation::MissedRing) && rung == 2;
                    if let Some(ring) = &ring
                        && !missed
                        && let Some(head) = waiter.append.rows.iter().map(|(seq, _)| *seq).max()
                    {
                        ring(&waiter.append.scope, head);
                    }
                }
                Err(StoreError::Fenced) => stats.conflicts += 1,
                _ => {}
            }
            let _ = waiter.reply.send(Turn::Done(result));
        }
        let mut total = lock(&self.stats);
        total.commits += stats.commits;
        total.appends += stats.appends;
        total.rows += stats.rows;
        total.conflicts += stats.conflicts;
        total.fenced += stats.fenced;
    }
}

/// Hands the partition's lead to the next waiter when the leading future is dropped mid-batch;
/// the batch in flight answers uncertain through its dropped replies.
struct Leading<'a> {
    queues: &'a Mutex<HashMap<Partition, Queue>>,
    part: Partition,
    done: bool,
}

impl Drop for Leading<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut queues = lock(self.queues);
        let queue = queues.entry(self.part).or_default();
        while let Some(waiter) = queue.waiting.pop_front() {
            if waiter.reply.send(Turn::Lead(waiter.append)).is_ok() {
                return;
            }
        }
        queue.leading = false;
    }
}

impl<S: PartStore + ?Sized> LogHost for PartedLog<S> {
    fn scan<'a>(
        &'a self,
        scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        self.store.scan(self.partition(scope), scope, from, to)
    }

    fn load<'a>(
        &'a self,
        scope: &'a str,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        self.store.load(self.partition(scope), scope)
    }

    fn append<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            if rows.is_empty() {
                return Ok(());
            }
            let append = Append {
                scope: Arc::from(scope),
                rows: rows.to_vec(),
            };
            self.submit(self.partition(scope), append).await
        })
    }

    /// The rows go down through the partition's group commit; the archive and drops follow in a
    /// second fenced transaction once the rows landed. Compaction only shrinks the log, so a
    /// crash between the two leaves an uncompacted but complete log.
    fn append_compacting<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
        compaction: Compaction<'a>,
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.append(scope, rows).await?;
            if compaction.archived.is_empty()
                && compaction.dropped.is_empty()
                && compaction.superseded.iter().all(std::ops::Range::is_empty)
            {
                return Ok(());
            }
            let part = self.partition(scope);
            let epoch = self.epoch(part)?;
            self.store.compact(part, epoch, scope, compaction).await
        })
    }

    fn extend<'a>(
        &'a self,
        scope: &'a str,
        extension: Extension<'a>,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        Some(Box::pin(async move {
            let part = self.partition(scope);
            let epoch = self.epoch(part)?;
            self.store.extend(part, epoch, scope, extension).await?;
            if let Some(ring) = lock(&self.ring).clone() {
                ring(scope, extension.seq);
            }
            Ok(())
        }))
    }

    fn carry<'a>(
        &'a self,
        _scope: &'a str,
        _write: Write<'a>,
        _rider: Rider<'a>,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        None
    }
}
