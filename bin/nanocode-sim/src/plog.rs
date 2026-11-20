//! The one partitioned log host (nanocode_turnlog::parted) under nodes that claim, lose and
//! retake partitions, concurrent appenders that collide on seqs and share group commits, lost
//! acknowledgements, serialization failures, followers with cursors and retention.
//!
//! Canaries (one per seeded defect): one owner per partition at any fence, no duplicate or gap in
//! a scope's seqs, all-or-nothing appends, a ring for every committed append and a follower that
//! sees every committed row, a snapshot plus its tail folds like the whole log, retention never
//! drops a row a cursor still needs, and an uncertain append is checked, never re-run.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    future::Future,
    num::NonZeroU32,
    ops::Range,
    pin::Pin,
    process::ExitCode,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use nanocode_durability_core::{StoreError, StoreFuture};
use nanocode_partitions::Partition;
use nanocode_turnlog::{
    LogHost,
    parted::{Entry, Fault, ModelParts, PartStore, PartedLog, sim as parted_sim},
};

use crate::rng::{FNV_OFFSET, Rng, fnv};

pub use parted_sim::Mutation as PlogMutation;

pub fn names() -> String {
    PlogMutation::ALL
        .iter()
        .map(|mutation| mutation.name())
        .collect::<Vec<_>>()
        .join("|")
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PlogStats {
    pub seeds: u64,
    pub appends: u64,
    pub acked: u64,
    pub uncertain: u64,
    pub conflicts: u64,
    pub fenced: u64,
    pub claims: u64,
    pub commits: u64,
    pub grouped: u64,
    pub before: u64,
    pub after: u64,
    pub pruned: u64,
    pub followed: u64,
}

impl PlogStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.appends += other.appends;
        self.acked += other.acked;
        self.uncertain += other.uncertain;
        self.conflicts += other.conflicts;
        self.fenced += other.fenced;
        self.claims += other.claims;
        self.commits += other.commits;
        self.grouped += other.grouped;
        self.before += other.before;
        self.after += other.after;
        self.pruned += other.pruned;
        self.followed += other.followed;
    }
}

struct Yield(bool);

impl Future for Yield {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

type Rung = Arc<Mutex<BTreeSet<(String, u64)>>>;
type Task<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// Polls the tasks in a seeded random order until all finish; every wait in this scenario is a
/// yield, so a noop waker is enough.
fn drive(rng: &mut Rng, mut tasks: Vec<Task<'_>>, limit: u64) -> Result<(), String> {
    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut steps = 0;
    while !tasks.is_empty() {
        steps += 1;
        if steps > limit {
            return Err(format!(
                "stuck: {} tasks left after {limit} polls",
                tasks.len()
            ));
        }
        let index = rng.pick(tasks.len());
        if tasks[index].as_mut().poll(&mut context).is_ready() {
            drop(tasks.swap_remove(index));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ack {
    Committed,
    Uncertain,
}

#[derive(Clone, Debug)]
struct Step {
    id: String,
    scope: String,
    rows: usize,
    ack: Ack,
    head: u64,
    node: usize,
}

#[derive(Default)]
struct Ledger {
    steps: Vec<Step>,
    rung: Vec<BTreeSet<(String, u64)>>,
    seen: BTreeMap<(String, u64), String>,
    wrong: Vec<String>,
    stats: PlogStats,
}

struct Shape {
    parts: u32,
    nodes: usize,
    scopes: Vec<String>,
    workers: usize,
    ops: usize,
    before_ppm: u64,
    after_ppm: u64,
    max_rows: usize,
}

impl Shape {
    fn of(rng: &mut Rng) -> Self {
        let parts = u32::try_from(rng.range(1, 5)).unwrap_or(1);
        let nodes = rng.range(2, 5) as usize;
        let scopes = (0..rng.range(2, 7))
            .map(|index| match index % 3 {
                0 => format!("board/p{}", index % u64::from(parts)),
                1 => format!("session:{index}"),
                _ => format!("records/0000000{index}-0000-0000-0000-00000000000{index}"),
            })
            .collect();
        Self {
            parts,
            nodes,
            scopes,
            workers: rng.range(1, 4) as usize,
            ops: rng.range(8, 40) as usize,
            before_ppm: rng.below(60_000),
            after_ppm: rng.below(60_000),
            max_rows: rng.range(1, 9) as usize,
        }
    }

    fn describe(&self) -> String {
        format!(
            "parts={} nodes={} scopes={} workers={} ops={} before={} after={} max_rows={}",
            self.parts,
            self.nodes,
            self.scopes.len(),
            self.workers,
            self.ops,
            self.before_ppm,
            self.after_ppm,
            self.max_rows
        )
    }
}

fn fold(rows: &[(u64, String)]) -> Result<i64, String> {
    let mut state = 0_i64;
    for (seq, body) in rows {
        if let Some(value) = body.strip_prefix('s') {
            state = value
                .parse()
                .map_err(|_| format!("bad snapshot {seq} {body}"))?;
        } else if let Some((_, value)) = body.rsplit_once(':') {
            state += value
                .parse::<i64>()
                .map_err(|_| format!("bad delta {seq} {body}"))?;
        }
    }
    Ok(state)
}

fn step_of(body: &str) -> Option<(&str, usize)> {
    let delta = body.strip_prefix('d')?;
    let (id, rest) = delta.split_once('#')?;
    let (index, _) = rest.split_once(':')?;
    Some((id, index.parse().ok()?))
}

struct Node {
    log: PartedLog<ModelParts>,
}

async fn head(log: &PartedLog<ModelParts>, scope: &str) -> Result<u64, StoreError> {
    let rows = log.scan(scope, 0, i64::MAX as u64).await?;
    Ok(rows.last().map_or(0, |(seq, _)| seq + 1))
}

async fn append_step(
    node: &Node,
    index: usize,
    scope: &str,
    id: String,
    count: usize,
    value: i64,
    ledger: &Mutex<Ledger>,
) {
    let mut tries = 0;
    loop {
        tries += 1;
        let Ok(at) = head(&node.log, scope).await else {
            return;
        };
        let rows: Vec<(u64, String)> = (0..count)
            .map(|row| (at + row as u64, format!("d{id}#{row}:{value}")))
            .collect();
        let last = at + count as u64 - 1;
        let answer = node.log.append(scope, &rows).await;
        let mut ledger = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ledger.stats.appends += 1;
        let step = |ack| Step {
            id: id.clone(),
            scope: scope.to_owned(),
            rows: count,
            ack,
            head: last,
            node: index,
        };
        match answer {
            Ok(()) => {
                ledger.stats.acked += 1;
                ledger.steps.push(step(Ack::Committed));
                return;
            }
            Err(StoreError::Fenced) => {
                ledger.stats.conflicts += 1;
                if tries >= 3 || node.log.held(node.log.partition(scope)).is_none() {
                    return;
                }
            }
            Err(_) => {
                ledger.stats.uncertain += 1;
                ledger.steps.push(step(Ack::Uncertain));
                return;
            }
        }
    }
}

async fn snapshot(node: &Node, scope: &str) {
    let Ok(rows) = node.log.scan(scope, 0, i64::MAX as u64).await else {
        return;
    };
    let Ok(loaded) = node.log.load(scope).await else {
        return;
    };
    if rows.first().is_some_and(|(seq, _)| *seq > 0)
        && loaded
            .first()
            .is_none_or(|(_, body)| !body.starts_with('s'))
    {
        return;
    }
    let base = if rows.first().is_some_and(|(seq, _)| *seq > 0) {
        &loaded
    } else {
        &rows
    };
    let Ok(state) = fold(base) else {
        return;
    };
    let at = rows.last().map_or(0, |(seq, _)| seq + 1);
    let _ = node.log.append(scope, &[(at, format!("s{state}"))]).await;
}

async fn worker(
    shape: &Shape,
    nodes: &[Node],
    index: usize,
    worker: usize,
    seed: u64,
    ledger: &Mutex<Ledger>,
) {
    let mut rng = Rng::derive(seed, &format!("plog worker {index}/{worker}"));
    let node = &nodes[index];
    for op in 0..shape.ops {
        let scope = &shape.scopes[rng.pick(shape.scopes.len())];
        let part = node.log.partition(scope);
        match rng.below(100) {
            0..=9 => {
                let live = rng.chance(500_000);
                if node
                    .log
                    .claim(part, op as u64, 1_000_000, live)
                    .await
                    .is_ok()
                {
                    ledger
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .stats
                        .claims += 1;
                }
            }
            10..=14 => {
                let _ = node.log.release(part).await;
            }
            15..=19 => snapshot(node, scope).await,
            20..=23 => {
                if let Ok(rows) = node.log.scan(scope, 0, i64::MAX as u64).await
                    && let Some((last, _)) = rows.last()
                    && let Ok(dropped) = node.log.prune(scope, *last).await
                {
                    ledger
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .stats
                        .pruned += dropped;
                }
            }
            24..=29 => follow(node, index, part, ledger).await,
            _ => {
                let count = rng.range(1, shape.max_rows as u64 + 1) as usize;
                let value = rng.range(1, 100) as i64;
                let id = format!("{index}.{worker}.{op}");
                append_step(node, index, scope, id, count, value, ledger).await;
            }
        }
    }
}

async fn follow(node: &Node, index: usize, part: Partition, ledger: &Mutex<Ledger>) {
    let reader = format!("reader{index}");
    let mut after = node.log.store().cursor_of(part, &reader);
    loop {
        let Ok(entries) = node.log.follow(part, after, 16).await else {
            return;
        };
        if entries.is_empty() {
            return;
        }
        record(&mut after, index, &entries, ledger);
        if node.log.cursor(part, &reader, after).await.is_err() {
            return;
        }
    }
}

fn record(after: &mut u64, index: usize, entries: &[Entry], ledger: &Mutex<Ledger>) {
    let mut ledger = ledger
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for entry in entries {
        if entry.pos <= *after {
            ledger.wrong.push(format!(
                "follower {index} went back from pos {} to {}",
                *after, entry.pos
            ));
        }
        *after = entry.pos;
        ledger.stats.followed += 1;
        if let Some(body) = ledger.seen.get(&(entry.scope.to_string(), entry.seq))
            && *body != entry.body
        {
            let message = format!(
                "{} seq {} read as {body} then {}",
                entry.scope, entry.seq, entry.body
            );
            ledger.wrong.push(message);
        }
        ledger
            .seen
            .insert((entry.scope.to_string(), entry.seq), entry.body.clone());
    }
}

fn run(seed: u64, mutation: PlogMutation, stats: &mut PlogStats) -> Result<u64, String> {
    let mut rng = Rng::derive(seed, "plog");
    let shape = Shape::of(&mut rng);
    let count = NonZeroU32::new(shape.parts).ok_or("no partitions")?;
    let faults_rng = Arc::new(Mutex::new(Rng::derive(seed, "plog faults")));
    let pause_rng = Arc::new(Mutex::new(Rng::derive(seed, "plog pauses")));
    let (before, after) = (shape.before_ppm, shape.after_ppm);
    let model = Arc::new(
        ModelParts::new()
            .with_faults(Arc::new(move |op| {
                if op != "commit" {
                    return Fault::None;
                }
                let mut rng = faults_rng
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if rng.chance(before) {
                    Fault::Before
                } else if rng.chance(after) {
                    Fault::After
                } else {
                    Fault::None
                }
            }))
            .with_sleep(Arc::new(move || -> StoreFuture<'static, ()> {
                let yields = pause_rng
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .below(3);
                Box::pin(async move {
                    for _ in 0..yields {
                        Yield(false).await;
                    }
                })
            })),
    );
    nanocode_sync::block_on(model.prepare(count)).map_err(|error| error.to_string())?;
    let ledger = Mutex::new(Ledger {
        rung: vec![BTreeSet::new(); shape.nodes],
        ..Ledger::default()
    });
    let rungs: Vec<Rung> = (0..shape.nodes).map(|_| Arc::default()).collect();
    let nodes: Vec<Node> = (0..shape.nodes)
        .map(|index| {
            let log = PartedLog::fenced(Arc::clone(&model), count, &format!("node{index}"))
                .with_max_rows(shape.max_rows * 3);
            let rung = Arc::clone(&rungs[index]);
            log.set_ring(Arc::new(move |scope, head| {
                rung.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert((scope.to_owned(), head));
            }));
            Node { log }
        })
        .collect();
    for index in 0..shape.nodes {
        for part in 0..shape.parts {
            let reader = format!("reader{index}");
            nanocode_sync::block_on(model.cursor(Partition(part), &reader, 0))
                .map_err(|error| error.to_string())?;
        }
    }
    parted_sim::mutate(mutation);
    let tasks: Vec<Task<'_>> = (0..shape.nodes)
        .flat_map(|index| (0..shape.workers).map(move |work| (index, work)))
        .map(|(index, work)| -> Task<'_> {
            Box::pin(worker(&shape, &nodes, index, work, seed, &ledger))
        })
        .collect();
    let driven = drive(&mut rng, tasks, 2_000_000);
    let calm: Vec<Task<'_>> = (0..shape.nodes)
        .flat_map(|index| (0..shape.parts).map(move |part| (index, part)))
        .map(|(index, part)| -> Task<'_> {
            Box::pin(follow(&nodes[index], index, Partition(part), &ledger))
        })
        .collect();
    let drained = drive(&mut rng, calm, 2_000_000);
    parted_sim::mutate(PlogMutation::None);
    driven?;
    drained?;
    let mut ledger = ledger
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (index, rung) in rungs.iter().enumerate() {
        ledger.rung[index] = rung
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
    }
    let model_stats = model.stats();
    for node in &nodes {
        let log_stats = node.log.stats();
        ledger.stats.commits += log_stats.commits;
        ledger.stats.grouped += log_stats.appends.saturating_sub(log_stats.commits);
        ledger.stats.fenced += log_stats.fenced;
    }
    ledger.stats.before = model_stats.before;
    ledger.stats.after = model_stats.after;
    *stats = ledger.stats;
    stats.seeds = 1;
    parted_sim::mutate(mutation);
    let verdict = check(
        &shape,
        &model,
        &nodes,
        &ledger,
        model_stats.unique_violations,
    );
    parted_sim::mutate(PlogMutation::None);
    verdict
}

fn check(
    shape: &Shape,
    model: &ModelParts,
    nodes: &[Node],
    ledger: &Ledger,
    unique_violations: u64,
) -> Result<u64, String> {
    if let Some(wrong) = ledger.wrong.first() {
        return Err(wrong.clone());
    }
    if unique_violations > 0 {
        return Err(format!(
            "{unique_violations} batches hit a unique violation: one batch carried a seq twice"
        ));
    }
    let mut present: BTreeMap<(String, u64), String> = BTreeMap::new();
    for part in 0..shape.parts {
        for (scope, seq, stored) in model.rows(Partition(part)) {
            if let Some(epoch) = stored.epoch
                && epoch != stored.fence
            {
                return Err(format!(
                    "{scope} seq {seq} was written with fence epoch {epoch} while the partition was at {}: two owners",
                    stored.fence
                ));
            }
            if let Some(seen) = ledger.seen.get(&(scope.to_string(), seq))
                && *seen != stored.body
            {
                return Err(format!(
                    "{scope} seq {seq} was followed as {seen} but holds {}: a seq was written twice",
                    stored.body
                ));
            }
            present.insert((scope.to_string(), seq), stored.body);
        }
    }
    let mut all = ledger.seen.clone();
    all.extend(present.clone());
    let mut by_step: HashMap<&str, BTreeSet<usize>> = HashMap::new();
    let mut seqs_of_step: HashMap<(&str, usize), usize> = HashMap::new();
    for body in all.values() {
        if let Some((id, row)) = step_of(body) {
            by_step.entry(id).or_default().insert(row);
            *seqs_of_step.entry((id, row)).or_default() += 1;
        }
    }
    if let Some(((id, row), times)) = seqs_of_step.iter().find(|(_, times)| **times > 1) {
        return Err(format!(
            "step {id} row {row} is in the log {times} times: an append was re-run"
        ));
    }
    for step in &ledger.steps {
        let rows = by_step.get(step.id.as_str()).map_or(0, BTreeSet::len);
        match step.ack {
            Ack::Committed if rows != step.rows => {
                return Err(format!(
                    "acknowledged step {} on {} has {rows} of its {} rows",
                    step.id, step.scope, step.rows
                ));
            }
            Ack::Uncertain if rows != 0 && rows != step.rows => {
                return Err(format!(
                    "uncertain step {} on {} landed {rows} of its {} rows",
                    step.id, step.scope, step.rows
                ));
            }
            _ => {}
        }
        if step.ack == Ack::Committed
            && !ledger.rung[step.node].contains(&(step.scope.clone(), step.head))
        {
            return Err(format!(
                "node {} committed {} through seq {} and rang no watcher",
                step.node, step.scope, step.head
            ));
        }
    }
    for ((scope, seq), body) in &all {
        if !ledger.seen.contains_key(&(scope.clone(), *seq)) {
            return Err(format!(
                "{scope} seq {seq} ({body}) was committed and no follower saw it"
            ));
        }
    }
    let mut digest = FNV_OFFSET;
    for scope in &shape.scopes {
        let rows: Vec<(u64, String)> = all
            .iter()
            .filter(|((row_scope, _), _)| row_scope == scope)
            .map(|((_, seq), body)| (*seq, body.clone()))
            .collect();
        for pair in rows.windows(2) {
            if pair[1].0 != pair[0].0 + 1 {
                return Err(format!(
                    "{scope} jumps from seq {} to {}",
                    pair[0].0, pair[1].0
                ));
            }
        }
        let whole = fold(&rows)?;
        let loaded =
            nanocode_sync::block_on(nodes[0].log.load(scope)).map_err(|error| error.to_string())?;
        let from_snapshot = fold(&loaded)?;
        if loaded
            .first()
            .is_some_and(|(seq, _)| rows.first().is_some_and(|(first, _)| seq != first))
            && loaded
                .first()
                .is_none_or(|(_, body)| !body.starts_with('s'))
        {
            return Err(format!(
                "{scope} loads from seq {:?} without its snapshot",
                loaded.first().map(|(seq, _)| seq)
            ));
        }
        if whole != from_snapshot {
            return Err(format!(
                "{scope}: snapshot plus tail folds to {from_snapshot}, the whole log to {whole}"
            ));
        }
        digest = fnv(digest, format!("{scope}={whole};").as_bytes());
    }
    Ok(fnv(digest, format!("{}", all.len()).as_bytes()))
}

pub struct PlogOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: PlogStats,
    shape: String,
}

pub fn simulate(seed: u64, mutation: PlogMutation) -> PlogOutcome {
    let mut rng = Rng::derive(seed, "plog");
    let shape = Shape::of(&mut rng).describe();
    let mut stats = PlogStats::default();
    let (digest, wrong) = match run(seed, mutation, &mut stats) {
        Ok(digest) => (digest, None),
        Err(message) => (0, Some(message)),
    };
    PlogOutcome {
        digest,
        wrong,
        stats,
        shape,
    }
}

pub fn run_seed(seed: u64, mutation: PlogMutation) -> PlogOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        parted_sim::mutate(PlogMutation::None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        PlogOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: PlogStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: PlogMutation,
    twice: bool,
    verbose: bool,
    totals: &mut PlogStats,
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

pub fn replay(seed: u64, mutation: PlogMutation) -> String {
    let mut command = format!("nanocode-sim --scenario plog --seed {seed}");
    if mutation != PlogMutation::None {
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
    mutation: PlogMutation,
    totals: &PlogStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=plog mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        PlogMutation::None if failed == 0 => ExitCode::SUCCESS,
        PlogMutation::None => ExitCode::FAILURE,
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
