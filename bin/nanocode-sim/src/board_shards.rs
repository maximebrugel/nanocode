//! Partitioned board writes: P partition logs, one or two committers per partition, many
//! writers claiming and finishing tasks through the router.
//!
//! Invariants, checked on every seed:
//! - no lost or double claim: every claim a writer was told landed is the fold's accepted claim
//!   at that seq, and every claim a writer was told was refused is refused in the fold;
//! - the merged view of the partition folds equals one serial fold of the same events;
//! - every partition's seqs run from 0 without a gap;
//! - group commit is all or nothing: every append lands every row or none, and each group round
//!   is one append;
//! - a renew of a lapsed claim takes a slot again: under a cap it lands only with room, and is
//!   refused full only without it;
//! - with one committer per partition, claims on different tasks never retry because of each
//!   other: no append loses a seq race, and a claim is only refused for its own task's state.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    ops::Range,
    process::ExitCode,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use nanocode_board::{
    Applied, Board, BoardError, Cap, ClaimId, EventId, Fence, Fold, NewTask, Op, Outcome, Phase,
    Rejection, TaskId, Write,
    access::{Can, Grant, Who},
    deliver::Outbox,
    shards::{self, Committer, Merged, ReadyIndex, Route, Stats as CommitStats},
    sim::{self as board_sim, Mutation as BoardRule},
    stage::Stage,
};
use nanocode_durability_core::{StoreError, StoreFuture};
use nanocode_partitions::Partition;
use nanocode_sync::Slot;
use nanocode_turnlog::{LogHost, MemoryLog, ScopeRows};

use crate::{
    executor::{Clock, Ended, Executor, Task},
    rng::{FNV_OFFSET, Rng, fnv},
};

const SCOPE: &str = "shards";
const OWNER: &str = "user:lead";
const MS: u64 = 1_000_000;
const END: u64 = i64::MAX as u64;
const BENCH_ROUNDS: usize = 3;
const POOL_LABEL: &str = "pool";
const POOL: &str = "lent";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShardsMutation {
    None,
    GroupSkipsSeq,
    GroupVerdictFromPlan,
    GroupSplits,
    RouteIgnoresTask,
    MergeSkipsPartition,
    MergedIndexSkipsRows,
    MergedIndexIgnoresRank,
    ChildFollowsParent,
    CrossMoveKeepsParent,
    CrossMoveSkipsUnmark,
    CrossMoveIngestsFirst,
    EpochFenceIgnored,
    ClaimCapIgnored,
    ClaimCapIgnoresLabel,
    FinishForgetsWorker,
    RenewIgnoresCap,
}

impl ShardsMutation {
    const ALL: [Self; 17] = [
        Self::None,
        Self::GroupSkipsSeq,
        Self::GroupVerdictFromPlan,
        Self::GroupSplits,
        Self::RouteIgnoresTask,
        Self::MergeSkipsPartition,
        Self::MergedIndexSkipsRows,
        Self::MergedIndexIgnoresRank,
        Self::ChildFollowsParent,
        Self::CrossMoveKeepsParent,
        Self::CrossMoveSkipsUnmark,
        Self::CrossMoveIngestsFirst,
        Self::EpochFenceIgnored,
        Self::ClaimCapIgnored,
        Self::ClaimCapIgnoresLabel,
        Self::FinishForgetsWorker,
        Self::RenewIgnoresCap,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::GroupSkipsSeq => "group_skips_seq",
            Self::GroupVerdictFromPlan => "group_verdict_from_plan",
            Self::GroupSplits => "group_splits",
            Self::RouteIgnoresTask => "route_ignores_task",
            Self::MergeSkipsPartition => "merge_skips_partition",
            Self::MergedIndexSkipsRows => "merged_index_skips_rows",
            Self::MergedIndexIgnoresRank => "merged_index_ignores_rank",
            Self::ChildFollowsParent => "child_follows_parent",
            Self::CrossMoveKeepsParent => "cross_move_keeps_parent",
            Self::CrossMoveSkipsUnmark => "cross_move_skips_unmark",
            Self::CrossMoveIngestsFirst => "cross_move_ingests_first",
            Self::EpochFenceIgnored => "epoch_fence_ignored",
            Self::ClaimCapIgnored => "claim_cap_ignored",
            Self::ClaimCapIgnoresLabel => "claim_cap_ignores_label",
            Self::FinishForgetsWorker => "finish_forgets_worker",
            Self::RenewIgnoresCap => "renew_ignores_cap",
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

    const fn rule(self) -> BoardRule {
        match self {
            Self::None => BoardRule::None,
            Self::GroupSkipsSeq => BoardRule::GroupSkipsSeq,
            Self::GroupVerdictFromPlan => BoardRule::GroupVerdictFromPlan,
            Self::GroupSplits => BoardRule::GroupSplits,
            Self::RouteIgnoresTask => BoardRule::RouteIgnoresTask,
            Self::MergeSkipsPartition => BoardRule::MergeSkipsPartition,
            Self::MergedIndexSkipsRows => BoardRule::MergedIndexSkipsRows,
            Self::MergedIndexIgnoresRank => BoardRule::MergedIndexIgnoresRank,
            Self::ChildFollowsParent => BoardRule::ChildFollowsParent,
            Self::CrossMoveKeepsParent => BoardRule::CrossMoveKeepsParent,
            Self::CrossMoveSkipsUnmark => BoardRule::CrossMoveSkipsUnmark,
            Self::CrossMoveIngestsFirst => BoardRule::CrossMoveIngestsFirst,
            Self::EpochFenceIgnored => BoardRule::EpochFenceIgnored,
            Self::ClaimCapIgnored => BoardRule::ClaimCapIgnored,
            Self::ClaimCapIgnoresLabel => BoardRule::ClaimCapIgnoresLabel,
            Self::FinishForgetsWorker => BoardRule::FinishForgetsWorker,
            Self::RenewIgnoresCap => BoardRule::RenewIgnoresCap,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ShardsStats {
    pub seeds: u64,
    pub partitions: u64,
    pub writers: u64,
    pub tasks: u64,
    pub claims: u64,
    pub refused: u64,
    pub raced: u64,
    pub unsettled: u64,
    pub appends: u64,
    pub rows: u64,
    pub conflicts: u64,
    pub groups: u64,
    pub largest: u64,
    pub faults: u64,
    pub elapsed_ms: u64,
    pub contended: u64,
    pub ranked: u64,
    pub ranked_away: u64,
    pub picks: u64,
    pub moved: u64,
    pub crashes: u64,
    pub lapsed_renews: u64,
    pub renews_full: u64,
}

impl ShardsStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.partitions += other.partitions;
        self.writers += other.writers;
        self.tasks += other.tasks;
        self.claims += other.claims;
        self.refused += other.refused;
        self.raced += other.raced;
        self.unsettled += other.unsettled;
        self.appends += other.appends;
        self.rows += other.rows;
        self.conflicts += other.conflicts;
        self.groups += other.groups;
        if other.largest > self.largest {
            self.largest = other.largest;
        }
        self.faults += other.faults;
        self.elapsed_ms += other.elapsed_ms;
        self.contended += other.contended;
        self.ranked += other.ranked;
        self.ranked_away += other.ranked_away;
        self.picks += other.picks;
        self.moved += other.moved;
        self.crashes += other.crashes;
        self.lapsed_renews += other.lapsed_renews;
        self.renews_full += other.renews_full;
    }
}

/// Host costs: each call waits `rtt_us` round trip plus `row_us` per row it carries, and a
/// faulty seed adds jitter up to `jitter_ms` on each side.
#[derive(Clone, Copy, Debug)]
pub struct Cost {
    pub rtt_us: u64,
    pub row_us: u64,
    pub jitter_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Scan,
    Index,
}

#[derive(Clone, Debug)]
struct Shape {
    partitions: u32,
    committers: u32,
    writers: u64,
    roots: u64,
    child_ppm: u64,
    thread_ppm: u64,
    top: usize,
    /// Each writer claims only tasks that hash to it, so claims race only the lease and
    /// group commit, never each other: the bench's throughput shape.
    spread: bool,
    batch: usize,
    lease_ms: u64,
    step_ms: u64,
    poll_ms: u64,
    before_ppm: u64,
    after_ppm: u64,
    read_ppm: u64,
    cost: Cost,
    deadline_ms: u64,
    rank_ppm: u64,
    moves: u64,
    crash_ppm: u64,
    pick: Pick,
    audit: bool,
    epochs: u64,
    fence_ppm: u64,
    cap: Option<Cap>,
    /// Share of fenced claims a writer makes from a view one epoch behind
    /// its partition, as an agent that read the board before the last bump.
    stale_ppm: u64,
    /// Share of landed claims whose writer, under a cap, sleeps past its lease and then renews,
    /// so the renew must take a slot again while other writers may have filled the cap.
    lapse_ppm: u64,
    /// How many lapses the whole run may take. A lapsed claim holds its cap slot for a whole
    /// lease, so lapses under a tight cap run one lease after another: at 400k ppm of claims
    /// lapsing a 2.4 s lease under a cap of one, 137 tasks need longer than the deadline
    /// however the writers race (seed 1795200002000022054). The budget keeps the time
    /// lapses hold the cap to a quarter of the deadline.
    lapse_budget: u64,
}

impl Shape {
    fn of(seed: u64) -> Self {
        let mut rng = Rng::derive(seed, "board shards shape");
        let partitions = [1, 2, 4, 8, 16][rng.pick(5)];
        let calm = rng.chance(250_000);
        let shape = Self {
            partitions,
            committers: if rng.chance(250_000) { 2 } else { 1 },
            writers: rng.range(2, 40),
            roots: rng.range(8, 120),
            child_ppm: rng.range(0, 400_000),
            thread_ppm: rng.range(0, 1_000_000),
            top: rng.range(1, 5) as usize,
            spread: false,
            batch: [1, 4, 64][rng.pick(3)],
            lease_ms: rng.range(200, 2_000),
            step_ms: rng.range(1, 40),
            poll_ms: rng.range(1, 20),
            before_ppm: if calm { 0 } else { rng.range(0, 30_000) },
            after_ppm: if calm { 0 } else { rng.range(0, 30_000) },
            read_ppm: if calm { 0 } else { rng.range(0, 20_000) },
            cost: Cost {
                rtt_us: rng.range(200, 3_000),
                row_us: rng.range(0, 30),
                jitter_ms: rng.range(0, 4),
            },
            deadline_ms: 120_000,
            rank_ppm: rng.range(0, 200_000),
            moves: rng.range(0, 40),
            crash_ppm: rng.range(0, 400_000),
            pick: Pick::Index,
            audit: true,
            epochs: rng.range(0, 24),
            fence_ppm: rng.range(0, 1_000_000),
            cap: rng
                .chance(500_000)
                .then(|| cap_of(seed, rng.range(1, 6) as u32)),
            stale_ppm: rng.range(100_000, 600_000),
            lapse_ppm: 0,
            lapse_budget: 0,
        };
        // Drawn apart from the shape's own stream so every earlier seed keeps its shape.
        let lapse_ppm = if shape.cap.is_some() {
            Rng::derive(seed, "board shards lapse").range(50_000, 400_000)
        } else {
            0
        };
        let lease_ms = shape.lease_ms.max(shape.lease_floor_ms());
        let lapse_budget = shape.cap.as_ref().map_or(0, |cap| {
            shape.deadline_ms / 4 * u64::from(cap.max()) / (lease_ms + shape.step_ms)
        });
        Self {
            lease_ms,
            lapse_ppm,
            lapse_budget,
            ..shape
        }
    }

    /// The shortest lease a winner can use: every writer queues a write on one committer, so a
    /// claim waits about one round per batch of writers before it lands, and its finish waits
    /// as long again. A lease shorter than that lapses before the finish lands however the
    /// claims race, and the board only moves when a rank reorders the stuck head (seed
    /// 156486433204944099: 31 writers at batch 1 behind a 218 ms lease). Four rounds of
    /// margin plus a step keep the lease above the queue without lengthening any lease that
    /// already was.
    const fn lease_floor_ms(&self) -> u64 {
        let rounds = self.writers.div_ceil(self.batch as u64);
        let round_ms = self.cost.rtt_us / 1_000 + 2 * self.cost.jitter_ms + 1;
        4 * rounds * round_ms + 2 * self.step_ms
    }

    fn describe(&self) -> String {
        format!(
            "partitions={} committers={} writers={} roots={} batch={} top={} faults={}/{}/{} rtt_us={}",
            self.partitions,
            self.committers,
            self.writers,
            self.roots,
            self.batch,
            self.top,
            self.before_ppm,
            self.after_ppm,
            self.read_ppm,
            self.cost.rtt_us
        )
    }
}

struct Call {
    rows: Vec<(u64, String)>,
    ok: bool,
}

/// A memory log with costs, faults and a record of every append it was asked for.
struct ShardLog {
    inner: MemoryLog,
    rng: Slot<Rng>,
    clock: Arc<Clock>,
    cost: Cost,
    before_ppm: u64,
    after_ppm: u64,
    read_ppm: u64,
    calls: Mutex<BTreeMap<String, Vec<Call>>>,
    appends: AtomicU64,
    rows: AtomicU64,
    faults: AtomicU64,
    conflicts: AtomicU64,
}

impl ShardLog {
    fn roll<T>(&self, each: impl FnOnce(&mut Rng) -> T) -> T {
        let mut rng = self
            .rng
            .take()
            .expect("the simulator polls one task at a time");
        let value = each(&mut rng);
        assert!(self.rng.put(rng).is_none(), "the fault rng has one home");
        value
    }

    async fn wait(&self, rows: usize) {
        let jitter = if self.cost.jitter_ms == 0 {
            0
        } else {
            self.roll(|rng| rng.range(0, self.cost.jitter_ms * MS))
        };
        let ns = (self.cost.rtt_us * 1_000 + self.cost.row_us * 1_000 * rows as u64) / 2 + jitter;
        self.clock.sleep(Duration::from_nanos(ns)).await;
    }

    fn injected(&self, what: &str) -> StoreError {
        self.faults.fetch_add(1, Ordering::SeqCst);
        StoreError::Backend(format!("injected shards {what} fault"))
    }

    async fn read(
        &self,
        scope: &str,
        from: u64,
        to: u64,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        self.wait(0).await;
        if self.roll(|rng| rng.chance(self.read_ppm)) {
            return Err(self.injected("read"));
        }
        let rows = self.inner.scan(scope, from, to).await;
        self.wait(rows.as_ref().map_or(0, Vec::len)).await;
        rows
    }

    /// Every scope's rows from its seq on in one round trip, as SqlLog reads them: one fault fails them all.
    async fn reads(&self, scans: &[(&str, u64)]) -> Result<Vec<Vec<(u64, String)>>, StoreError> {
        self.wait(0).await;
        if self.roll(|rng| rng.chance(self.read_ppm)) {
            return Err(self.injected("read"));
        }
        let mut rows = Vec::with_capacity(scans.len());
        for (scope, from) in scans {
            rows.push(self.inner.scan(scope, *from, END).await?);
        }
        self.wait(rows.iter().map(Vec::len).sum()).await;
        Ok(rows)
    }
}

impl LogHost for ShardLog {
    fn call_bound(&self) -> Option<std::time::Duration> {
        None
    }

    fn scan<'a>(
        &'a self,
        scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        Box::pin(self.read(scope, from, to))
    }

    fn scans<'a>(
        &'a self,
        scans: &'a [(&'a str, u64)],
    ) -> StoreFuture<'a, Result<Vec<ScopeRows>, StoreError>> {
        Box::pin(self.reads(scans))
    }

    fn load<'a>(
        &'a self,
        scope: &'a str,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        Box::pin(self.read(scope, 0, END))
    }

    fn append<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.wait(rows.len()).await;
            self.appends.fetch_add(1, Ordering::SeqCst);
            if self.roll(|rng| rng.chance(self.before_ppm)) {
                self.calls
                    .lock()
                    .expect("calls lock")
                    .entry(scope.to_owned())
                    .or_default()
                    .push(Call {
                        rows: rows.to_vec(),
                        ok: false,
                    });
                return Err(self.injected("append before"));
            }
            let appended = self.inner.append(scope, rows).await;
            if appended.is_err() {
                self.conflicts.fetch_add(1, Ordering::SeqCst);
            } else {
                self.rows.fetch_add(rows.len() as u64, Ordering::SeqCst);
            }
            self.calls
                .lock()
                .expect("calls lock")
                .entry(scope.to_owned())
                .or_default()
                .push(Call {
                    rows: rows.to_vec(),
                    ok: appended.is_ok(),
                });
            self.wait(0).await;
            if appended.is_ok() && self.roll(|rng| rng.chance(self.after_ppm)) {
                return Err(self.injected("append after"));
            }
            appended
        })
    }
}

#[derive(Clone, Debug)]
enum Told {
    Applied(u64),
    Refused(Rejection),
}

#[derive(Default)]
struct Oracle {
    told: Vec<(Partition, EventId, TaskId, Told)>,
    stats: ShardsStats,
    wrong: Vec<String>,
    commits: Vec<(Partition, CommitStats)>,
}

struct World {
    shape: Shape,
    clock: Arc<Clock>,
    host: Arc<ShardLog>,
    count: NonZeroU32,
    plan: Vec<NewTask>,
    committers: RefCell<Vec<Vec<Committer>>>,
    view: RefCell<Vec<Fold>>,
    index: RefCell<ReadyIndex>,
    oracle: RefCell<Oracle>,
    rng: RefCell<Rng>,
    spins: std::cell::Cell<u64>,
    lapsed: std::cell::Cell<u64>,
    pick_ns: std::cell::Cell<u128>,
    parents: RefCell<BTreeMap<TaskId, Option<TaskId>>>,
    marked: RefCell<BTreeSet<TaskId>>,
}

fn thread_of(seed: u64, index: u64) -> String {
    let high = fnv(FNV_OFFSET ^ seed, &index.to_le_bytes());
    let low = fnv(high, b"thread");
    format!("{high:016x}{low:016x}")
}

fn cap_of(seed: u64, max: u32) -> Cap {
    if Rng::derive(seed, "board shards cap").chance(500_000) {
        Cap::Label {
            max,
            key: POOL_LABEL.to_owned(),
            value: POOL.to_owned(),
        }
    } else {
        Cap::Partition(max)
    }
}

fn plan(seed: u64, shape: &Shape) -> Vec<NewTask> {
    let mut rng = Rng::derive(seed, "board shards plan");
    let mut tasks = Vec::new();
    for index in 0..shape.roots {
        let labels = if rng.chance(shape.thread_ppm) {
            BTreeMap::from([(shards::THREAD_LABEL.to_owned(), thread_of(seed, index))])
        } else {
            BTreeMap::new()
        };
        tasks.push(NewTask {
            id: TaskId(format!("t{index}")),
            parent: None,
            spec: format!("root {index}"),
            labels,
            needs: Vec::new(),
            terms: nanocode_board::tasks::Terms::default(),
        });
    }
    for index in 0..shape.roots {
        if rng.chance(shape.child_ppm) {
            let parent = TaskId(format!("t{}", rng.below(shape.roots)));
            let labels = if rng.chance(shape.thread_ppm) {
                BTreeMap::from([(
                    shards::THREAD_LABEL.to_owned(),
                    thread_of(seed, shape.roots + index),
                )])
            } else {
                BTreeMap::new()
            };
            tasks.push(NewTask {
                id: TaskId(format!("c{index}")),
                parent: Some(parent),
                spec: format!("child {index}"),
                labels,
                needs: Vec::new(),
                terms: nanocode_board::tasks::Terms::default(),
            });
        }
    }
    if let Some(Cap::Label { key, value, .. }) = &shape.cap {
        let mut pool = Rng::derive(seed, "board shards pool");
        for task in &mut tasks {
            if pool.chance(500_000) {
                task.labels.insert(key.clone(), value.clone());
            }
        }
    }
    tasks
}

impl World {
    fn now_ms(&self) -> u64 {
        self.clock.now_ns() / MS
    }

    async fn nap(&self, ms: u64) {
        self.clock.sleep(Duration::from_nanos(ms.max(1) * MS)).await;
    }

    fn scope(&self, partition: Partition) -> String {
        shards::scope_of(SCOPE, self.count, partition)
    }

    fn merged<T>(&self, each: impl FnOnce(&Merged<'_>) -> T) -> T {
        let view = self.view.borrow();
        each(&Merged::new(view.iter().collect()))
    }

    async fn refresh(&self) -> bool {
        let scopes: Vec<String> = (0..self.count.get())
            .map(|index| self.scope(Partition(index)))
            .collect();
        let scans: Vec<(&str, u64)> = {
            let view = self.view.borrow();
            scopes
                .iter()
                .zip(view.iter())
                .map(|(scope, fold)| (scope.as_str(), fold.next()))
                .collect()
        };
        let Ok(scanned) = self.host.scans(&scans).await else {
            return false;
        };
        for (index, rows) in scanned.into_iter().enumerate() {
            let mut view = self.view.borrow_mut();
            let fold = &mut view[index];
            for (seq, body) in rows {
                if seq == fold.next()
                    && let Err(error) = fold.apply(seq, &body)
                {
                    self.oracle.borrow_mut().wrong.push(format!(
                        "partition {index} row {seq} does not fold: {error}"
                    ));
                    return false;
                }
            }
        }
        let view = self.view.borrow();
        self.index
            .borrow_mut()
            .catch_up(&Merged::new(view.iter().collect()));
        true
    }

    fn pick(&self, now: u64, keep: impl Fn(&TaskId) -> bool) -> Vec<TaskId> {
        let view = self.view.borrow();
        let merged = Merged::new(view.iter().collect());
        let index = self.index.borrow();
        let top = self.shape.top;
        let indexed = || -> Vec<TaskId> {
            index
                .ready(&merged, now)
                .filter(|(_, id)| keep(id))
                .take(top)
                .map(|(_, id)| id.clone())
                .collect()
        };
        let scanned = || -> Vec<TaskId> {
            merged
                .ready(now)
                .into_iter()
                .filter(|(_, id)| keep(id))
                .take(top)
                .map(|(_, id)| id.clone())
                .collect()
        };
        let started = std::time::Instant::now();
        let picked = match self.shape.pick {
            Pick::Index => indexed(),
            Pick::Scan => scanned(),
        };
        self.pick_ns
            .set(self.pick_ns.get() + started.elapsed().as_nanos());
        if self.shape.audit {
            let other = match self.shape.pick {
                Pick::Index => scanned(),
                Pick::Scan => indexed(),
            };
            if picked != other {
                self.oracle.borrow_mut().wrong.push(format!(
                    "the merged ready index at {now} ms picks {picked:?} but a scan of the folds picks {other:?}"
                ));
            }
        }
        self.oracle.borrow_mut().stats.picks += 1;
        picked
    }

    fn committer(&self, partition: Partition) -> Option<Committer> {
        let committers = self.committers.borrow();
        let set = committers.get(partition.0 as usize)?;
        let pick = self.rng.borrow_mut().pick(set.len());
        set.get(pick).cloned()
    }

    async fn send(
        &self,
        partition: Partition,
        by: &str,
        key: String,
        op: Op,
    ) -> Result<Applied, BoardError> {
        let committer = self
            .committer(partition)
            .ok_or_else(|| BoardError::Dropped("no committer".into()))?;
        committer
            .submit(Write {
                by: Some(by.to_owned()),
                key: Some(key),
                at_ms: self.now_ms(),
                op,
            })
            .await
    }

    async fn submit(
        &self,
        by: &str,
        key: String,
        op: Op,
    ) -> (Option<Partition>, Result<Applied, BoardError>) {
        let (route, op) = self.merged(|merged| merged.place(by, op));
        match route {
            Route::One(partition) => (Some(partition), self.send(partition, by, key, op).await),
            Route::Every => {
                let mut last = Err(BoardError::Dropped("no partitions".into()));
                for index in 0..self.count.get() {
                    last = self
                        .send(Partition(index), by, key.clone(), op.clone())
                        .await;
                    if last.is_err() {
                        break;
                    }
                }
                (None, last)
            }
            Route::Cross { .. } => (
                None,
                Err(BoardError::Dropped(
                    "a cross-partition move lands through the outbox".into(),
                )),
            ),
            Route::Refused(why) => (None, Err(BoardError::Dropped(why))),
        }
    }

    /// Submits until the write settles: landed, or refused by the fold.
    async fn settle(&self, by: &str, key: &str, op: Op) -> Result<Applied, Rejection> {
        loop {
            match self.submit(by, key.to_owned(), op.clone()).await.1 {
                Ok(applied) => return Ok(applied),
                Err(BoardError::Rejected(rejection)) => return Err(rejection),
                Err(_) => {
                    self.nap(self.shape.poll_ms).await;
                    let _ = self.refresh().await;
                }
            }
        }
    }

    /// Lands an owner write in one partition, whether this call or an earlier one landed it.
    async fn settle_in(&self, partition: Partition, key: &str, op: Op) {
        loop {
            match self
                .send(partition, OWNER, key.to_owned(), op.clone())
                .await
            {
                Ok(_) | Err(BoardError::Rejected(_)) => return,
                Err(_) => self.nap(self.shape.poll_ms).await,
            }
        }
    }

    fn done(&self) -> bool {
        self.merged(|merged| {
            merged.len() == self.plan.len() && merged.tasks().values().all(|task| task.finished())
        })
    }
}

async fn lead(world: &World) {
    let everyone = Grant {
        who: vec![Who::Everyone],
        can: BTreeMap::from([(
            "*".to_owned(),
            vec![Can::Read, Can::Post, Can::Label, Can::Rank],
        )]),
    };
    for index in 0..world.count.get() {
        let partition = Partition(index);
        world.settle_in(partition, "charter", Op::Charter {}).await;
        world
            .settle_in(
                partition,
                "grant",
                Op::Grant {
                    grant: everyone.clone(),
                },
            )
            .await;
    }
    let _ = world.refresh().await;
    for task in &world.plan {
        if task.parent.is_some() {
            while !world.merged(|merged| {
                task.parent
                    .as_ref()
                    .is_some_and(|parent| merged.task(parent).is_some())
            }) {
                world.nap(world.shape.poll_ms).await;
                let _ = world.refresh().await;
            }
        }
        let key = format!("post:{}", task.id);
        if std::env::var_os("NC_SHARDS_TRACE").is_some() {
            eprintln!(
                "trace post {} now_ms={} appends={}",
                task.id,
                world.now_ms(),
                world.host.appends.load(Ordering::SeqCst)
            );
        }
        if let Err(rejection) = world
            .settle(OWNER, &key, Op::Post { task: task.clone() })
            .await
        {
            world
                .oracle
                .borrow_mut()
                .wrong
                .push(format!("posting {} was refused: {rejection:?}", task.id));
        }
    }
}

/// Bumps every partition's epoch once within the first steps, so fenced
/// claims from a view one epoch behind meet a bumped partition early, then
/// bumps `epochs` random partitions about every half lease (none at all when
/// `epochs` is 0).
async fn fencer(world: &World) {
    let mut rng = Rng::derive(world.shape.epochs, "board shards fencer");
    if world.shape.epochs == 0 {
        return;
    }
    let first = u64::from(world.count.get());
    for bump in 0..first + world.shape.epochs {
        let gap_ms = if bump < first {
            world.shape.step_ms
        } else {
            world.shape.lease_ms / 2
        };
        world.nap(rng.range(1, gap_ms + 1)).await;
        if world.done() {
            return;
        }
        let _ = world.refresh().await;
        let partition = if bump < first {
            Partition(bump as u32 % world.count)
        } else {
            Partition(rng.below(u64::from(world.count.get())) as u32)
        };
        let epoch = world.view.borrow()[partition.0 as usize].epoch(partition.0) + 1;
        let op = Op::Epoch {
            partition: partition.0,
            epoch,
        };
        world
            .settle_in(partition, &format!("epoch:{bump}"), op)
            .await;
    }
}

async fn writer(world: &World, index: u64) {
    let by = format!("agent:w{index}");
    let mut rng = Rng::derive(fnv(FNV_OFFSET, by.as_bytes()), "board shards writer");
    let mut lapses = Rng::derive(fnv(FNV_OFFSET, by.as_bytes()), "board shards lapses");
    let mut tries = 0_u64;
    loop {
        if world.now_ms() > world.shape.deadline_ms {
            return;
        }
        if !world.refresh().await {
            world.nap(world.shape.poll_ms).await;
            continue;
        }
        if world.done() {
            return;
        }
        world.spins.set(world.spins.get() + 1);
        if std::env::var_os("NC_SHARDS_TRACE").is_some() && world.spins.get().is_multiple_of(500) {
            let (posted, finished) = world.merged(|merged| {
                (
                    merged.len(),
                    merged
                        .tasks()
                        .values()
                        .filter(|task| task.finished())
                        .count(),
                )
            });
            eprintln!(
                "trace spins={} now_ms={} posted={posted}/{} finished={finished} told={} appends={} wrong={:?}",
                world.spins.get(),
                world.now_ms(),
                world.plan.len(),
                world.oracle.borrow().told.len(),
                world.host.appends.load(Ordering::SeqCst),
                world.oracle.borrow().wrong.first()
            );
        }
        let now = world.now_ms();
        let ready = world.pick(now, |id| {
            !world.shape.spread || fnv(FNV_OFFSET, id.0.as_bytes()) % world.shape.writers == index
        });
        if ready.is_empty() {
            world.nap(world.shape.poll_ms).await;
            continue;
        }
        let task = ready[rng.pick(ready.len())].clone();
        tries += 1;
        if rng.chance(world.shape.rank_ppm) {
            let rank = Op::Rank {
                task: task.clone(),
                rank: Some(rng.below(world.plan.len() as u64)),
            };
            if world
                .submit(&by, format!("rank:{tries}"), rank)
                .await
                .1
                .is_ok()
            {
                let away = world.merged(|merged| {
                    merged.parent(&task).is_some_and(|parent| {
                        merged
                            .home(&parent)
                            .is_some_and(|at| Some(at) != merged.home(&task))
                    })
                });
                let mut oracle = world.oracle.borrow_mut();
                oracle.stats.ranked += 1;
                oracle.stats.ranked_away += u64::from(away);
            }
            continue;
        }
        let key = format!("claim:{tries}");
        let fence = rng.chance(world.shape.fence_ppm).then(|| {
            let home = world
                .merged(|merged| merged.home(&task))
                .unwrap_or(Partition(0));
            let epoch = world.view.borrow()[home.0 as usize].epoch(home.0);
            let stale = rng.chance(world.shape.stale_ppm);
            Fence {
                partition: home.0,
                epoch: if stale {
                    epoch.saturating_sub(1)
                } else {
                    epoch
                },
            }
        });
        let claim = Op::Claim {
            task: task.clone(),
            lease_ms: world.shape.lease_ms,
            fence,
            cap: world.shape.cap.clone(),
        };
        let (partition, verdict) = world.submit(&by, key.clone(), claim).await;
        let event = EventId(format!("{by}#{key}"));
        let partition = partition.unwrap_or(Partition(0));
        match verdict {
            Ok(applied) => {
                world.oracle.borrow_mut().told.push((
                    partition,
                    event.clone(),
                    task.clone(),
                    Told::Applied(applied.seq),
                ));
                if lapses.chance(world.shape.lapse_ppm)
                    && world.lapsed.get() < world.shape.lapse_budget
                {
                    world.lapsed.set(world.lapsed.get() + 1);
                    world
                        .nap(world.shape.lease_ms + lapses.range(1, world.shape.step_ms + 1))
                        .await;
                    let renew = Op::Renew {
                        task: task.clone(),
                        claim: ClaimId(event.0.clone()),
                        lease_ms: world.shape.lease_ms,
                    };
                    if world
                        .submit(&by, format!("renew:{tries}"), renew)
                        .await
                        .1
                        .is_err()
                    {
                        continue;
                    }
                } else {
                    world.nap(rng.range(1, world.shape.step_ms + 1)).await;
                }
                let finish = Op::Finish {
                    task: task.clone(),
                    claim: ClaimId(event.0.clone()),
                    outcome: Outcome::Done(format!("{task} by {by}")),
                };
                let _ = world.submit(&by, format!("finish:{tries}"), finish).await;
            }
            Err(BoardError::Rejected(rejection)) => {
                world.oracle.borrow_mut().told.push((
                    partition,
                    event,
                    task,
                    Told::Refused(rejection),
                ));
            }
            Err(BoardError::Unsettled { .. }) => world.oracle.borrow_mut().stats.unsettled += 1,
            Err(_) => world.nap(world.shape.poll_ms).await,
        }
    }
}

async fn mover(world: &World) {
    let by = OWNER;
    let mut rng = Rng::derive(fnv(FNV_OFFSET, by.as_bytes()), "board shards mover");
    let mut seq = 0_u64;
    while seq < world.shape.moves && world.now_ms() < world.shape.deadline_ms {
        world.nap(rng.range(1, world.shape.step_ms * 4 + 2)).await;
        if !world.refresh().await || world.merged(|merged| merged.len()) < world.plan.len() {
            continue;
        }
        let plan = world.merged(|merged| {
            let open: Vec<&TaskId> = merged
                .tasks()
                .into_iter()
                .filter(|(_, task)| !task.finished())
                .map(|(id, _)| id)
                .collect();
            if open.len() < 2 {
                return None;
            }
            let task = open[rng.pick(open.len())].clone();
            let parent = (!rng.chance(200_000)).then(|| open[rng.pick(open.len())].clone());
            if parent.as_ref() == Some(&task) {
                return None;
            }
            let old = merged.parent(&task);
            let cross = parent
                .as_ref()
                .is_some_and(|parent| merged.home(parent) != merged.home(&task));
            let deliveries = merged.moves(by, seq, &task, parent.as_ref()).ok()?;
            Some((task, parent, old, cross, deliveries))
        });
        let Some((task, parent, old, cross, deliveries)) = plan else {
            continue;
        };
        world
            .parents
            .borrow_mut()
            .entry(task.clone())
            .or_insert(old);
        let mut outbox = Outbox::new();
        for delivery in &deliveries {
            let _ = outbox.post(delivery.clone());
        }
        let mut saved = outbox.clone();
        while outbox.pending() > 0 && world.now_ms() < world.shape.deadline_ms {
            let committers: Vec<Committer> = (0..world.count.get())
                .filter_map(|index| world.committer(Partition(index)))
                .collect();
            if committers.len() != world.count.get() as usize {
                return;
            }
            let crash = rng.chance(world.shape.crash_ppm);
            let before = rng.chance(500_000);
            let mut persisted = false;
            let delivered = shards::deliver(&mut outbox, &committers, world.now_ms(), |now| {
                if crash && persisted {
                    return false;
                }
                persisted = true;
                if crash && before {
                    return false;
                }
                saved = now.clone();
                !crash
            })
            .await;
            if crash || delivered.is_err() {
                world.oracle.borrow_mut().stats.crashes += u64::from(crash);
                outbox = saved.clone();
                world.nap(world.shape.poll_ms).await;
            }
        }
        while !world.refresh().await {
            world.nap(world.shape.poll_ms).await;
        }
        let landed = world.merged(|merged| {
            deliveries.iter().all(|delivery| {
                merged
                    .fold(Partition(delivery.part))
                    .and_then(|fold| {
                        fold.outcome(&EventId(format!(
                            "{by}#{}",
                            shards::delivery_key(&delivery.key)
                        )))
                    })
                    .is_some_and(Result::is_ok)
            })
        });
        if landed {
            world.parents.borrow_mut().insert(task.clone(), parent);
            if cross {
                world.marked.borrow_mut().insert(task);
            } else {
                world.marked.borrow_mut().remove(&task);
            }
            world.oracle.borrow_mut().stats.moved += 1;
        }
        seq += 1;
    }
}

async fn run(world: &World) {
    let hosted: Arc<dyn LogHost> = Arc::clone(&world.host) as Arc<dyn LogHost>;
    let mut boards = Vec::new();
    for index in 0..world.count.get() {
        for node in 0..world.shape.committers {
            let identity = crate::board::identity(&format!("agent:node{node}"));
            let scope = world.scope(Partition(index));
            loop {
                match Board::open(Arc::clone(&hosted), &scope, identity.clone()).await {
                    Ok(board) => {
                        boards.push((Partition(index), board));
                        break;
                    }
                    Err(_) => world.nap(world.shape.poll_ms).await,
                }
            }
        }
    }
    let mut drives: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>>> = Vec::new();
    for (partition, board) in boards {
        let (committer, drive) = Committer::start(board, world.shape.batch);
        world.committers.borrow_mut()[partition.0 as usize].push(committer);
        drives.push(Box::pin(async move {
            let (_, stats) = drive.await;
            world.oracle.borrow_mut().commits.push((partition, stats));
        }));
    }
    drives.push(Box::pin(async move {
        let mut actors: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>>> = vec![
            Box::pin(lead(world)),
            Box::pin(mover(world)),
            Box::pin(fencer(world)),
        ];
        for index in 0..world.shape.writers {
            actors.push(Box::pin(writer(world, index)));
        }
        nanocode_sync::join_in_order(actors).await;
        world.committers.borrow_mut().clear();
    }));
    nanocode_sync::join_in_order(drives).await;
}

/// One fold of every partition's rows, interleaved by each partition's clock after the row (the
/// running max of event times), so the serial fold's clock at every row equals the clock its own
/// partition's fold had there.
fn serial(logs: &[Vec<(u64, String)>], folds: &[Fold]) -> Result<Fold, String> {
    let mut rows = Vec::new();
    for (partition, log) in logs.iter().enumerate() {
        let mut clock = 0;
        for (position, (_, body)) in log.iter().enumerate() {
            let mut event: nanocode_board::Event = serde_json::from_str(body)
                .map_err(|error| format!("partition {partition} row {position}: {error}"))?;
            clock = clock.max(event.at_ms);
            if let Op::Claim { cap, .. } = &mut event.op {
                if matches!(
                    folds[partition].outcome(&event.id),
                    Some(Err(Rejection::Full { .. }))
                ) {
                    continue;
                }
                *cap = None;
            }
            if matches!(event.op, Op::Renew { .. })
                && matches!(
                    folds[partition].outcome(&event.id),
                    Some(Err(Rejection::Full { .. }))
                )
            {
                continue;
            }
            let body = serde_json::to_string(&event)
                .map_err(|error| format!("partition {partition} row {position}: {error}"))?;
            rows.push((clock, partition, position, body));
        }
    }
    rows.sort();
    let mut fold = Fold::new();
    let first = fold.next();
    for (seq, (_, _, _, body)) in (first..).zip(rows) {
        fold.apply(seq, &body)
            .map_err(|error| format!("serial fold: {error}"))?;
    }
    Ok(fold)
}

fn check(world: &World, logs: &[Vec<(u64, String)>]) -> Result<ShardsStats, String> {
    let oracle = world.oracle.borrow();
    if let Some(wrong) = oracle.wrong.first() {
        return Err(wrong.clone());
    }
    let first = Fold::new().next();
    for (index, log) in logs.iter().enumerate() {
        if let Some((at, (seq, _))) = log
            .iter()
            .enumerate()
            .find(|(at, (seq, _))| *seq != first + *at as u64)
        {
            return Err(format!(
                "partition {index} has a gap: row {at} has seq {seq}"
            ));
        }
    }
    let mut folds = Vec::new();
    let mut lapsed_renews = 0;
    let mut renews_full = 0;
    for (index, log) in logs.iter().enumerate() {
        let mut fold = Fold::new();
        let mut clock = 0;
        for (seq, body) in log {
            let event: nanocode_board::Event = serde_json::from_str(body)
                .map_err(|error| format!("partition {index} row {seq}: {error}"))?;
            let (fence, cap) = match &event.op {
                Op::Claim { fence, cap, .. } => (
                    fence.map(|fence| (fence, fold.epoch(fence.partition))),
                    cap.clone(),
                ),
                _ => (None, None),
            };
            let finisher = match &event.op {
                Op::Finish { task, .. } => fold.task(task).and_then(|task| match &task.phase {
                    Phase::Claimed { worker, .. } => Some(worker.clone()),
                    _ => None,
                }),
                _ => None,
            };
            clock = clock.max(event.at_ms);
            let lapsed = match &event.op {
                Op::Renew { task, .. } => fold
                    .task(task)
                    .filter(|task| task.stage(clock) == Stage::Lapsed)
                    .and_then(|task| task.cap.clone()),
                _ => None,
            };
            fold.apply(*seq, body)
                .map_err(|error| format!("partition {index} does not fold: {error}"))?;
            if let Some(cap) = &lapsed {
                let held = fold.held(cap, event.at_ms);
                let max = cap.max() as usize;
                match fold.outcome(&event.id) {
                    Some(Ok(landed)) if landed == seq => {
                        lapsed_renews += 1;
                        if held > max {
                            return Err(format!(
                                "partition {index} row {seq}: renew {} revived a lapsed claim over its cap {cap:?} with {held} live claims",
                                event.id
                            ));
                        }
                    }
                    Some(Err(Rejection::Full { .. })) => {
                        renews_full += 1;
                        if held < max {
                            return Err(format!(
                                "partition {index} row {seq}: renew {} was refused as full under {cap:?} with {held} live claims",
                                event.id
                            ));
                        }
                    }
                    _ => {}
                }
            }
            if let Some((fence, held)) = fence
                && fold.outcome(&event.id) == Some(&Ok(*seq))
                && fence.epoch != held
            {
                return Err(format!(
                    "partition {index} row {seq}: claim {} landed fenced at epoch {} while partition {} was at epoch {held}",
                    event.id, fence.epoch, fence.partition
                ));
            }
            if let Some(cap) = &cap {
                let held = fold.held(cap, event.at_ms);
                let max = cap.max() as usize;
                match fold.outcome(&event.id) {
                    Some(Ok(landed)) if landed == seq && held > max => {
                        return Err(format!(
                            "partition {index} row {seq}: claim {} landed over its cap {cap:?} with {held} live claims",
                            event.id
                        ));
                    }
                    Some(Err(Rejection::Full { .. })) if held < max => {
                        return Err(format!(
                            "partition {index} row {seq}: claim {} was refused as full under {cap:?} with {held} live claims",
                            event.id
                        ));
                    }
                    _ => {}
                }
            }
            if let (Op::Finish { task, .. }, Some(worker)) = (&event.op, &finisher)
                && fold.outcome(&event.id) == Some(&Ok(*seq))
                && let Some(task) = fold.task(task)
                && !matches!(&task.phase, Phase::Done { worker: Some(done), .. } if done == worker)
            {
                return Err(format!(
                    "partition {index} row {seq}: {} finished under {worker}'s claim shows {:?}",
                    task.spec.id, task.phase
                ));
            }
        }
        folds.push(fold);
    }
    let calls = world.host.calls.lock().expect("calls lock");
    for (index, log) in logs.iter().enumerate() {
        let scope = world.scope(Partition(index as u32));
        let by_seq: BTreeMap<u64, &String> = log.iter().map(|(seq, body)| (*seq, body)).collect();
        for call in calls.get(&scope).into_iter().flatten() {
            let present = call
                .rows
                .iter()
                .filter(|(seq, body)| by_seq.get(seq).is_some_and(|landed| *landed == body))
                .count();
            if call.ok && present != call.rows.len() {
                return Err(format!(
                    "partition {index}: an acknowledged append of {} rows has only {present} in the log",
                    call.rows.len()
                ));
            }
        }
        let rounds: u64 = oracle
            .commits
            .iter()
            .filter(|(partition, _)| partition.0 as usize == index)
            .map(|(_, stats)| stats.appends)
            .sum();
        let appends = calls.get(&scope).map_or(0, Vec::len) as u64;
        if appends != rounds {
            return Err(format!(
                "partition {index}: committers made {rounds} group rounds but the host took {appends} appends"
            ));
        }
    }
    drop(calls);
    let mut stats = oracle.stats;
    stats.lapsed_renews = lapsed_renews;
    stats.renews_full = renews_full;
    for (partition, event, task, told) in &oracle.told {
        let home = Merged::new(folds.iter().collect()).home(task);
        let fold = &folds[home.unwrap_or(*partition).0 as usize];
        let outcome = fold.outcome(event);
        match told {
            Told::Applied(seq) => {
                stats.claims += 1;
                if outcome != Some(&Ok(*seq)) {
                    return Err(format!(
                        "double claim: {event} on {task} was reported landed at {seq} but the fold has {outcome:?}"
                    ));
                }
            }
            Told::Refused(rejection) => {
                stats.refused += 1;
                if let Some(Ok(seq)) = outcome {
                    return Err(format!(
                        "lost claim: {event} on {task} landed at {seq} but its writer was told {rejection:?}"
                    ));
                }
                match rejection {
                    Rejection::Held { .. }
                    | Rejection::Terminal
                    | Rejection::Stale { .. }
                    | Rejection::Full { .. } => {
                        stats.raced += 1;
                    }
                    other if world.shape.committers == 1 => {
                        return Err(format!(
                            "{event} on {task} was refused for {other:?}, not for its own task's claim"
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    let merged = Merged::new(folds.iter().collect());
    let serial = serial(logs, &folds)?;
    let ids: BTreeSet<&TaskId> = merged.tasks().keys().copied().collect();
    let serial_ids: BTreeSet<&TaskId> = serial.tasks().keys().collect();
    if ids != serial_ids {
        return Err(format!(
            "the merged view holds {} tasks, the serial fold {}",
            ids.len(),
            serial_ids.len()
        ));
    }
    for id in ids {
        let (Some(left), Some(right)) = (merged.task(id), serial.task(id)) else {
            return Err(format!("{id} is missing from a view"));
        };
        if left.phase != right.phase || left.attempts != right.attempts || left.rank != right.rank {
            return Err(format!(
                "{id} differs: merged {:?} serial {:?}",
                left.phase, right.phase
            ));
        }
    }
    let now = world.now_ms() + world.shape.lease_ms * 4;
    let merged_ready: Vec<&TaskId> = merged.ready(now).into_iter().map(|(_, id)| id).collect();
    let mut serial_ready: Vec<(u64, Partition, &TaskId)> = serial
        .ready(now)
        .map(|id| {
            let rank = serial
                .task(id)
                .and_then(|task| task.rank)
                .unwrap_or(u64::MAX);
            (rank, merged.home(id).unwrap_or(Partition(u32::MAX)), id)
        })
        .collect();
    serial_ready.sort();
    let serial_ready: Vec<&TaskId> = serial_ready.into_iter().map(|(_, _, id)| id).collect();
    let mut index = ReadyIndex::new();
    index.catch_up(&merged);
    let indexed: Vec<&TaskId> = index.ready(&merged, now).map(|(_, id)| id).collect();
    if indexed != merged_ready {
        return Err(format!(
            "ready order differs: index {indexed:?} merged {merged_ready:?}"
        ));
    }
    if merged_ready != serial_ready {
        return Err(format!(
            "ready order differs: merged {merged_ready:?} serial {serial_ready:?}"
        ));
    }
    let mut seen = BTreeSet::new();
    for fold in &folds {
        for id in fold.tasks().keys() {
            if !seen.insert(id) {
                return Err(format!("{id} is shown by more than one partition"));
            }
        }
    }
    for (id, task) in merged.tasks() {
        for (label, home) in &task.spec.labels {
            if let Some(child) = label.strip_prefix(shards::CHILD_LABEL) {
                let child = TaskId(child.to_owned());
                if merged.parent(&child).as_ref() != Some(id)
                    || merged.home(&child).map(|home| home.0.to_string()).as_ref() != Some(home)
                {
                    return Err(format!(
                        "{id} marks {child} as its child in partition {home}, but {child} has parent {:?} in {:?}",
                        merged.parent(&child),
                        merged.home(&child)
                    ));
                }
            }
        }
        for child in merged.children(id) {
            if merged.parent(child).as_ref() != Some(id) {
                return Err(format!(
                    "{child} is a child of {id} but has parent {:?}",
                    merged.parent(child)
                ));
            }
        }
    }
    for task in &world.plan {
        if let Some(key) = shards::own_key(task) {
            let want = nanocode_partitions::partition_of(&key, world.count);
            if merged.home(&task.id).is_some_and(|home| home != want) {
                return Err(format!(
                    "{} names its own routing key but sits in partition {:?}, not {want:?}",
                    task.id,
                    merged.home(&task.id)
                ));
            }
        }
        let want = world
            .parents
            .borrow()
            .get(&task.id)
            .cloned()
            .unwrap_or_else(|| task.parent.clone());
        if merged.task(&task.id).is_some() && merged.parent(&task.id) != want {
            return Err(format!(
                "{} has parent {:?} in the merged view, expected {want:?}",
                task.id,
                merged.parent(&task.id),
            ));
        }
        if world.marked.borrow().contains(&task.id)
            && let Some(parent) = &want
            && merged.task(parent).and_then(|above| {
                above
                    .spec
                    .labels
                    .get(&format!("{}{}", shards::CHILD_LABEL, task.id))
            }) != merged
                .home(&task.id)
                .map(|home| home.0.to_string())
                .as_ref()
        {
            return Err(format!(
                "{} moved under {parent} in another partition but {parent} does not mark it",
                task.id
            ));
        }
        if let Some(parent) = &want
            && merged.task(&task.id).is_some()
            && !merged.children(parent).contains(&&task.id)
        {
            return Err(format!(
                "{} is missing from the merged children of {parent}",
                task.id
            ));
        }
    }
    if !merged.tasks().values().all(|task| task.finished()) || merged.len() != world.plan.len() {
        return Err(format!(
            "the board did not finish: {} of {} tasks posted, {} finished",
            merged.len(),
            world.plan.len(),
            merged
                .tasks()
                .values()
                .filter(|task| task.finished())
                .count()
        ));
    }
    let conflicts = world.host.conflicts.load(Ordering::SeqCst);
    if world.shape.committers == 1 && conflicts > 0 {
        return Err(format!(
            "{conflicts} appends lost a seq race with one committer per partition"
        ));
    }
    for (_, commit) in &oracle.commits {
        stats.groups += commit.groups;
        stats.largest = stats.largest.max(commit.largest);
    }
    stats.conflicts = conflicts;
    Ok(stats)
}

pub struct ShardsOutcome {
    digest: u64,
    pick_ns: u128,
    wrong: Option<String>,
    stats: ShardsStats,
    shape: String,
}

fn simulate_shape(seed: u64, shape: Shape, mutation: ShardsMutation) -> ShardsOutcome {
    let described = shape.describe();
    let clock = Clock::new();
    // Boards reach the platform host on their own (a checkpoint save is a task they spawn), so the
    // seed runs under its simulated host like every other scenario: without it the first save
    // panics on a thread with no simulation.
    crate::host::enter(seed, &clock);
    let count = NonZeroU32::new(shape.partitions).unwrap_or(NonZeroU32::MIN);
    let host = Arc::new(ShardLog {
        inner: MemoryLog::new(),
        rng: Slot::new(Some(Rng::derive(seed, "board shards faults"))),
        clock: Arc::clone(&clock),
        cost: shape.cost,
        before_ppm: shape.before_ppm,
        after_ppm: shape.after_ppm,
        read_ppm: shape.read_ppm,
        calls: Mutex::new(BTreeMap::new()),
        appends: AtomicU64::new(0),
        rows: AtomicU64::new(0),
        faults: AtomicU64::new(0),
        conflicts: AtomicU64::new(0),
    });
    let planned = plan(seed, &shape);
    let world = World {
        plan: planned,
        clock: Arc::clone(&clock),
        host: Arc::clone(&host),
        count,
        committers: RefCell::new(vec![Vec::new(); count.get() as usize]),
        view: RefCell::new(vec![Fold::new(); count.get() as usize]),
        index: RefCell::new(ReadyIndex::new()),
        oracle: RefCell::new(Oracle::default()),
        rng: RefCell::new(Rng::derive(seed, "board shards routing")),
        spins: std::cell::Cell::new(0),
        lapsed: std::cell::Cell::new(0),
        pick_ns: std::cell::Cell::new(0),
        parents: RefCell::new(BTreeMap::new()),
        marked: RefCell::new(BTreeSet::new()),
        shape,
    };
    board_sim::mutate(mutation.rule());
    let mut executor = Executor::new(
        Arc::clone(&clock),
        Rng::derive(seed, "board shards scheduler"),
    );
    let task: Task<'_> = Box::pin(run(&world));
    let ended = executor.run(vec![task], Some((world.shape.deadline_ms + 60_000) * MS));
    drop(executor);
    let elapsed_ms = world.now_ms();
    let logs: Vec<Vec<(u64, String)>> = (0..count.get())
        .map(|index| {
            nanocode_sync::block_on(host.inner.scan(&world.scope(Partition(index)), 0, END))
                .unwrap_or_default()
        })
        .collect();
    let verdict = match ended {
        Ended::Finished => check(&world, &logs),
        Ended::Crashed => Err(format!(
            "the board did not settle within {} ms",
            world.shape.deadline_ms
        )),
        Ended::Stuck => Err("the run got stuck".to_owned()),
    };
    board_sim::mutate(BoardRule::None);
    let mut digest = fnv(FNV_OFFSET, described.as_bytes());
    for log in &logs {
        for (seq, body) in log {
            digest = fnv(digest, &seq.to_le_bytes());
            digest = fnv(digest, body.as_bytes());
        }
    }
    let mut stats = verdict
        .as_ref()
        .copied()
        .unwrap_or_else(|_| world.oracle.borrow().stats);
    stats.seeds = 1;
    stats.partitions = u64::from(count.get());
    stats.writers = world.shape.writers;
    stats.tasks = world.plan.len() as u64;
    stats.appends = host.appends.load(Ordering::SeqCst);
    stats.rows = host.rows.load(Ordering::SeqCst);
    stats.faults = host.faults.load(Ordering::SeqCst);
    stats.elapsed_ms = elapsed_ms;
    stats.contended = u64::from(world.shape.committers > 1);
    ShardsOutcome {
        digest,
        pick_ns: world.pick_ns.get(),
        wrong: verdict.err(),
        stats,
        shape: described,
    }
}

pub fn simulate(seed: u64, mutation: ShardsMutation) -> ShardsOutcome {
    // Checkpoints are off by default; half the seeds turn them on at the cadence a deployment
    // would set, so the committers' boards spawn saves as they grow. Drawn apart from the shape's
    // stream so every seed keeps its shape.
    let checkpoints = Rng::derive(seed, "board shards checkpoints").chance(500_000);
    nanocode_board::checkpoint::checkpoint_every_here(
        checkpoints.then_some(nanocode_board::checkpoint::CHECKPOINT_EVERY),
    );
    let outcome = simulate_shape(seed, Shape::of(seed), mutation);
    nanocode_board::checkpoint::checkpoint_every_here(None);
    outcome
}

pub fn run_seed(seed: u64, mutation: ShardsMutation) -> ShardsOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        board_sim::mutate(BoardRule::None);
        nanocode_board::checkpoint::checkpoint_every_here(None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        ShardsOutcome {
            digest: 0,
            pick_ns: 0,
            wrong: Some(format!("panic: {message}")),
            stats: ShardsStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: ShardsMutation,
    twice: bool,
    verbose: bool,
    totals: &mut ShardsStats,
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
            "{} :: nondeterminism: trace digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: ShardsMutation) -> String {
    let mut command = format!("nanocode-sim --scenario board_shards --seed {seed}");
    if mutation != ShardsMutation::None {
        command.push_str(" --mutation ");
        command.push_str(mutation.name());
    }
    command
}

/// Throughput under a fixed shape and cost model, in simulated time, with no faults: one line
/// per (partitions, writers) point.
pub fn bench(partitions: &[u32], writers: &[u64], batch: usize, cost: Cost) {
    for &count in partitions {
        for &writers in writers {
            let shape = Shape {
                partitions: count,
                committers: 1,
                writers,
                roots: (writers * 8).max(256),
                child_ppm: 0,
                thread_ppm: 1_000_000,
                top: 4,
                spread: true,
                batch,
                lease_ms: 60_000,
                step_ms: 1,
                poll_ms: 1,
                before_ppm: 0,
                after_ppm: 0,
                read_ppm: 0,
                cost,
                deadline_ms: 3_600_000,
                rank_ppm: 0,
                moves: 0,
                crash_ppm: 0,
                pick: Pick::Index,
                audit: false,
                epochs: 0,
                fence_ppm: 0,
                cap: None,
                stale_ppm: 0,
                lapse_ppm: 0,
                lapse_budget: 0,
            };
            let mut walls = Vec::new();
            for _ in 0..BENCH_ROUNDS {
                for pick in [Pick::Scan, Pick::Index] {
                    let started = std::time::Instant::now();
                    let outcome = simulate_shape(
                        0xb0a2d,
                        Shape {
                            pick,
                            ..shape.clone()
                        },
                        ShardsMutation::None,
                    );
                    walls.push((
                        pick,
                        started.elapsed().as_micros(),
                        outcome.pick_ns,
                        outcome.stats.picks,
                    ));
                }
            }
            for pick in [Pick::Scan, Pick::Index] {
                let mine = || walls.iter().filter(|(each, ..)| *each == pick);
                let mut micros: Vec<u128> = mine().map(|(_, micros, ..)| *micros).collect();
                let mut nanos: Vec<u128> = mine().map(|(_, _, nanos, _)| *nanos).collect();
                micros.sort_unstable();
                nanos.sort_unstable();
                let picks = mine().next().map_or(0, |(.., picks)| *picks);
                println!(
                    "metric board.wall_ms{{partitions={count},writers={writers},batch={batch},pick={pick:?}}} median={:.1} min={:.1} picks={picks} rounds={BENCH_ROUNDS}",
                    micros[micros.len() / 2] as f64 / 1000.0,
                    micros[0] as f64 / 1000.0,
                );
                println!(
                    "metric board.pick_us{{partitions={count},writers={writers},batch={batch},pick={pick:?}}} median={:.2}",
                    nanos[nanos.len() / 2] as f64 / 1000.0 / picks.max(1) as f64,
                );
            }
            let outcome = simulate_shape(0xb0a2d, shape, ShardsMutation::None);
            let stats = outcome.stats;
            let secs = (stats.elapsed_ms.max(1)) as f64 / 1000.0;
            println!(
                "metric board.claims_per_s{{partitions={count},writers={writers},batch={batch},mode=simulated}} {:.0}",
                stats.claims as f64 / secs
            );
            println!(
                "metric board.appends_per_s{{partitions={count},writers={writers},batch={batch},mode=simulated}} {:.0}",
                stats.appends as f64 / secs
            );
            println!(
                "metric board.rows_per_s{{partitions={count},writers={writers},batch={batch},mode=simulated}} {:.0}",
                stats.rows as f64 / secs
            );
            println!(
                "metric board.retry_rate{{partitions={count},writers={writers},batch={batch},mode=simulated}} {:.4} refused_rate={:.4} largest_group={} ok={}",
                stats.conflicts as f64 / stats.appends.max(1) as f64,
                stats.refused as f64 / (stats.claims + stats.refused).max(1) as f64,
                stats.largest,
                outcome.wrong.as_deref().unwrap_or("yes")
            );
        }
    }
}

pub fn report(
    seeds: &Range<u64>,
    passed: u64,
    failed: u64,
    twice: bool,
    mutation: ShardsMutation,
    totals: &ShardsStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=board_shards mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        ShardsMutation::None if failed == 0 => ExitCode::SUCCESS,
        ShardsMutation::None => ExitCode::FAILURE,
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
