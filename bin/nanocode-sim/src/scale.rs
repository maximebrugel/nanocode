use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::NonZeroU32,
    ops::Range,
    process::ExitCode,
};

use nanocode_partitions::{Partition, Token, assignment, plan};

use crate::{
    partitions::{NodeId, Record, Store, TICK_MS, TTL_MS, thread_partition},
    rng::{FNV_OFFSET, PPM, Rng, fnv},
};

const STEP_CPU_US: u64 = 119;
const CLAIM_CPU_US: u64 = 5;
const FOLD_US: u64 = 2;
const RECORDS_PER_THREAD: u64 = 454;
const NET_RTT_US: u64 = 409;
const STORE_TXN_US: u64 = 171;
const STORE_ROW_US: u64 = 26;
const STORE_FSYNC_US: u64 = 25;
const STORE_READ_US: u64 = 2;
const SYNC_FOLD_US: u64 = 1;
const SYNC_WATCH_US: u64 = 2;
const RECORDS_PER_STEP: u64 = 2;
const STEP_US: u64 =
    STEP_CPU_US + CLAIM_CPU_US + RECORDS_PER_STEP * (FOLD_US + SYNC_FOLD_US + SYNC_WATCH_US);
const CATCH_UP_US: u64 = RECORDS_PER_THREAD * (STORE_READ_US + SYNC_FOLD_US);
const WRITE_US: u64 = STORE_TXN_US + STORE_ROW_US + STORE_FSYNC_US;
const LEASE_TXN_US: u64 = WRITE_US + STORE_TXN_US + STORE_FSYNC_US;
const RECORDS_PER_TXN_MAX: u64 = 1_024;
const US_PER_MS: u64 = 1_000;
const MS_PER_S: u64 = 1_000;
const TICK_US: u64 = TICK_MS * US_PER_MS;
const MEASURE_TICKS: u64 = TTL_MS / TICK_MS;
const SETTLE_TICKS: u64 = 2 * TTL_MS / TICK_MS;
const DEFAULT_NODES: [usize; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
const NODES_LIMIT: usize = 1_000;
const PREDICT_NODES_MAX: u64 = 100_000;
const SPEEDUP_NODES: usize = 10;
const SPEEDUP_MIN: u64 = 8;
const AGENT_COUNTS: [u32; 2] = [1_000, 10_000];
const PARTITIONS_MIN: u64 = 64;
const PARTITIONS_MAX: u64 = 256;
const HOT_SHARE_PPM: u64 = 250_000;
const COLD_THINK_US: u64 = 50_000;
const P99_PPM: u64 = 990_000;
const MILLI: u64 = 1_000;
const PERCENT: u64 = 100;
const BASIS_POINTS: u64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleMutation {
    None,
    GlobalLock,
    RowPerRecord,
    OneNode,
    DoubleClaim,
    IgnoreHot,
}

impl ScaleMutation {
    const ALL: [Self; 6] = [
        Self::None,
        Self::GlobalLock,
        Self::RowPerRecord,
        Self::OneNode,
        Self::DoubleClaim,
        Self::IgnoreHot,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::GlobalLock => "global_lock",
            Self::RowPerRecord => "row_per_record",
            Self::OneNode => "one_node",
            Self::DoubleClaim => "double_claim",
            Self::IgnoreHot => "ignore_hot",
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

    const fn commit_us(self, records: u64) -> u64 {
        match self {
            Self::GlobalLock => records * WRITE_US,
            Self::RowPerRecord => STORE_TXN_US + STORE_FSYNC_US + records * STORE_ROW_US,
            Self::None | Self::OneNode | Self::DoubleClaim | Self::IgnoreHot => WRITE_US,
        }
    }
}

/// How agents load the partitions: uniform (every thread steps flat out, threads hashed
/// across partitions) or skewed (a quarter of the threads step flat out in one hot partition,
/// the rest think between steps), so one partition and its owner can bottleneck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleLoad {
    Uniform,
    Skewed,
}

impl ScaleLoad {
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Uniform, Self::Skewed]
            .into_iter()
            .find(|load| load.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Uniform => "uniform",
            Self::Skewed => "skewed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScaleConfig {
    pub mutation: ScaleMutation,
    pub load: ScaleLoad,
    pub nodes: Vec<usize>,
}

impl Default for ScaleConfig {
    fn default() -> Self {
        Self {
            mutation: ScaleMutation::None,
            load: ScaleLoad::Uniform,
            nodes: DEFAULT_NODES.to_vec(),
        }
    }
}

/// Parses --nodes: increasing node counts, the first 1 (the speedup baseline).
pub fn parse_nodes(list: &str) -> Option<Vec<usize>> {
    let nodes = list
        .split(',')
        .map(|count| count.trim().parse().ok())
        .collect::<Option<Vec<usize>>>()?;
    (nodes.first() == Some(&1)
        && nodes.windows(2).all(|pair| pair[0] < pair[1])
        && nodes.last().is_some_and(|last| *last <= NODES_LIMIT))
    .then_some(nodes)
}

/// The largest node count whose per-tick store work fits in a tick when every node runs
/// flat out: one commit per partition (more with a records cap), a heartbeat and a renew per
/// node, and a renewed row per partition.
fn predicted_nodes(partitions: u64, cap: Option<u64>) -> u64 {
    let records_per_node = RECORDS_PER_STEP * TICK_US / STEP_US;
    let cost = |nodes: u64| {
        let txns = cap.map_or(partitions, |cap| {
            partitions * (nodes * records_per_node).div_ceil(partitions * cap)
        });
        txns * WRITE_US + nodes * LEASE_TXN_US + partitions * STORE_ROW_US
    };
    (1..=PREDICT_NODES_MAX)
        .take_while(|nodes| cost(*nodes) <= TICK_US)
        .last()
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Default)]
struct Stage {
    nodes: usize,
    steps_per_s: u64,
    claims_per_s: u64,
    txns_per_s: u64,
    store_ppm: u64,
    part_max_ppm: u64,
    part_p99_ppm: u64,
    node_max_ppm: u64,
    idle_nodes: u64,
    backlog: u64,
    warmup_ms: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct StageSum {
    runs: u64,
    steps: u64,
    claims: u64,
    store_ppm: u64,
    part_max_ppm: u64,
    part_p99_ppm: u64,
    node_max_ppm: u64,
}

impl StageSum {
    const fn add(&mut self, other: &Self) {
        self.runs += other.runs;
        self.steps += other.steps;
        self.claims += other.claims;
        self.store_ppm += other.store_ppm;
        self.part_max_ppm += other.part_max_ppm;
        self.part_p99_ppm += other.part_p99_ppm;
        self.node_max_ppm += other.node_max_ppm;
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScaleStats {
    pub seeds: u64,
    stages: BTreeMap<(u32, usize), StageSum>,
    speedup_min: BTreeMap<u32, u64>,
    speedup_max: BTreeMap<u32, u64>,
    caught: BTreeMap<u32, u64>,
}

impl ScaleStats {
    pub fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        for (key, sum) in &other.stages {
            self.stages.entry(*key).or_default().add(sum);
        }
        for (agents, speedup) in &other.speedup_min {
            let min = self.speedup_min.entry(*agents).or_insert(*speedup);
            *min = (*min).min(*speedup);
        }
        for (agents, speedup) in &other.speedup_max {
            let max = self.speedup_max.entry(*agents).or_default();
            *max = (*max).max(*speedup);
        }
        for (agents, caught) in &other.caught {
            *self.caught.entry(*agents).or_default() += caught;
        }
    }

    fn record(&mut self, agents: u32, stages: &[Stage], complete: bool) {
        for stage in stages {
            self.stages
                .entry((agents, stage.nodes))
                .or_default()
                .add(&StageSum {
                    runs: 1,
                    steps: stage.steps_per_s,
                    claims: stage.claims_per_s,
                    store_ppm: stage.store_ppm,
                    part_max_ppm: stage.part_max_ppm,
                    part_p99_ppm: stage.part_p99_ppm,
                    node_max_ppm: stage.node_max_ppm,
                });
        }
        let (Some(first), Some(last), true) = (stages.first(), stages.last(), complete) else {
            return;
        };
        let speedup = last.steps_per_s * MILLI / first.steps_per_s.max(1);
        let min = self.speedup_min.entry(agents).or_insert(speedup);
        *min = (*min).min(speedup);
        let max = self.speedup_max.entry(agents).or_default();
        *max = (*max).max(speedup);
    }

    fn summary(&self) -> String {
        let mut out = format!("seeds={}", self.seeds);
        for agents in AGENT_COUNTS {
            let sums = self
                .stages
                .iter()
                .filter(|((count, _), _)| *count == agents)
                .map(|((_, nodes), sum)| (*nodes, *sum))
                .collect::<Vec<_>>();
            let list = |value: fn(&StageSum) -> u64, scale: u64| {
                sums.iter()
                    .map(|(_, sum)| (value(sum) / sum.runs.max(1) / scale).to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let pct = PPM / PERCENT;
            let bp = PPM / BASIS_POINTS;
            out.push_str(&format!(
                " agents{agents}=[runs={} nodes={} steps_per_s={} claims_per_s={} store_util_pct={} part_util_max_bp={} part_util_p99_bp={} node_util_max_pct={} speedup_min_x1000={} speedup_max_x1000={} caught={}]",
                sums.first().map_or(0, |(_, sum)| sum.runs),
                sums.iter()
                    .map(|(nodes, _)| nodes.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                list(|sum| sum.steps, 1),
                list(|sum| sum.claims, 1),
                list(|sum| sum.store_ppm, pct),
                list(|sum| sum.part_max_ppm, bp),
                list(|sum| sum.part_p99_ppm, bp),
                list(|sum| sum.node_max_ppm, pct),
                self.speedup_min.get(&agents).copied().unwrap_or(0),
                self.speedup_max.get(&agents).copied().unwrap_or(0),
                self.caught.get(&agents).copied().unwrap_or(0)
            ));
        }
        out
    }
}

#[derive(Clone, Copy)]
struct Agent {
    partition: Partition,
    think_us: u64,
    logged: u32,
    checked: u32,
    ready_at: u64,
}

struct Node {
    id: NodeId,
    clock: u64,
    busy: u64,
    held: BTreeMap<Partition, u64>,
    queue: VecDeque<u32>,
    parked: BTreeSet<(u64, u32)>,
}

impl Node {
    fn enqueue(&mut self, agent: u32, state: &Agent) {
        if state.think_us == 0 {
            self.queue.push_back(agent);
        } else {
            self.parked.insert((state.ready_at, agent));
        }
    }
}

struct World {
    mutation: ScaleMutation,
    count: NonZeroU32,
    store: Store,
    nodes: Vec<Node>,
    node_base: u32,
    agents: Vec<Agent>,
    by_partition: BTreeMap<Partition, Vec<u32>>,
    pending: BTreeMap<Partition, u64>,
    part_busy: Vec<u64>,
    cursor: u32,
    now: u64,
    store_clock: u64,
    store_busy: u64,
    committed: u64,
    claims: u64,
    txns: u64,
}

impl World {
    fn new(seed: u64, agents: u32, config: &ScaleConfig) -> (Self, String) {
        let mut rng = Rng::derive(seed, &format!("scale-{agents}"));
        let count = NonZeroU32::new(
            u32::try_from(rng.range(PARTITIONS_MIN, PARTITIONS_MAX))
                .expect("a partition count fits a u32"),
        )
        .expect("a partition count is at least PARTITIONS_MIN");
        let thread_base = u32::try_from(rng.below(u64::from(u32::MAX - agents)))
            .expect("a thread base below u32::MAX fits a u32");
        let node_base = u32::try_from(rng.below(u64::from(u32::MAX) - DEFAULT_NODES.len() as u64))
            .expect("a node base below u32::MAX fits a u32");
        let mut skew = Rng::derive(seed, &format!("scale-skew-{agents}"));
        let hot = (config.load == ScaleLoad::Skewed).then(|| {
            Partition(
                u32::try_from(skew.below(u64::from(count.get())))
                    .expect("a partition below the count fits a u32"),
            )
        });
        let mut by_partition: BTreeMap<Partition, Vec<u32>> = BTreeMap::new();
        let agents = (0..agents)
            .map(|index| {
                let (partition, think_us) = match hot {
                    Some(hot) if skew.below(PPM) < HOT_SHARE_PPM => (hot, 0),
                    Some(_) => (thread_partition(thread_base + index, count), COLD_THINK_US),
                    None => (thread_partition(thread_base + index, count), 0),
                };
                by_partition.entry(partition).or_default().push(index);
                Agent {
                    partition,
                    think_us,
                    logged: 0,
                    checked: 0,
                    ready_at: 0,
                }
            })
            .collect::<Vec<_>>();
        let partitions = u64::from(count.get());
        let mut shape = format!(
            "agents={} partitions={count} thread_base={thread_base} node_base={node_base} load={} predicted_saturation_nodes={} capped={}",
            agents.len(),
            config.load.name(),
            predicted_nodes(partitions, None),
            predicted_nodes(partitions, Some(RECORDS_PER_TXN_MAX))
        );
        if let Some(hot) = hot {
            shape.push_str(&format!(
                " hot_partition={} hot_agents={}",
                hot.0,
                by_partition.get(&hot).map_or(0, Vec::len)
            ));
        }
        (
            Self {
                mutation: config.mutation,
                count,
                store: Store::new(count),
                nodes: Vec::new(),
                node_base,
                agents,
                by_partition,
                pending: BTreeMap::new(),
                part_busy: vec![0; count.get() as usize],
                cursor: 0,
                now: 0,
                store_clock: 0,
                store_busy: 0,
                committed: 0,
                claims: 0,
                txns: 0,
            },
            shape,
        )
    }

    fn store_write(&mut self, cost: u64) {
        self.store_clock = self.store_clock.max(self.now) + cost;
        self.store_busy += cost;
    }

    fn join(&mut self) {
        let index = u32::try_from(self.nodes.len()).expect("a node index fits a u32");
        self.nodes.push(Node {
            id: NodeId::new(self.node_base.wrapping_add(index)),
            clock: self.now,
            busy: 0,
            held: BTreeMap::new(),
            queue: VecDeque::new(),
            parked: BTreeSet::new(),
        });
    }

    fn live(&self) -> BTreeSet<NodeId> {
        let live = self.store.alive(self.now / US_PER_MS, TTL_MS);
        match (self.mutation, live.first()) {
            (ScaleMutation::OneNode, Some(first)) => BTreeSet::from([*first]),
            _ => live,
        }
    }

    fn lease(&mut self, at: usize) -> Result<(), String> {
        let now_ms = self.now / US_PER_MS;
        let id = self.nodes[at].id;
        self.store.heartbeat(id, now_ms);
        self.store_write(WRITE_US);
        let live = self.live();
        let mut steps = plan(
            &id,
            self.count,
            &live,
            &self.nodes[at].held,
            &self.store.rows,
            now_ms,
        );
        if self.mutation == ScaleMutation::DoubleClaim {
            steps.claim = assignment(self.count, &live)
                .into_iter()
                .filter(|(partition, owner)| {
                    *owner == id && !self.nodes[at].held.contains_key(partition)
                })
                .map(|(partition, _)| partition)
                .collect();
        }
        for token in steps.release {
            self.store
                .release(token.partition, id, token.epoch)
                .map_err(|fenced| {
                    format!("invariant 1 (single holder): node {at} release fenced: {fenced:?}")
                })?;
            self.store_write(WRITE_US);
            let node = &mut self.nodes[at];
            node.held.remove(&token.partition);
            let agents = &self.agents;
            node.queue
                .retain(|agent| agents[*agent as usize].partition != token.partition);
            node.parked
                .retain(|(_, agent)| agents[*agent as usize].partition != token.partition);
        }
        for partition in steps.claim {
            let holder_live = self.mutation != ScaleMutation::DoubleClaim
                && self.store.rows[&partition]
                    .holder()
                    .is_some_and(|holder| live.contains(holder));
            let Ok((_, epoch)) = self.store.claim(partition, id, now_ms, TTL_MS, holder_live)
            else {
                continue;
            };
            self.claims += 1;
            self.store_write(WRITE_US);
            let moved = self
                .by_partition
                .get(&partition)
                .cloned()
                .unwrap_or_default();
            let node = &mut self.nodes[at];
            node.held.insert(partition, epoch);
            let cost = CLAIM_CPU_US + moved.len() as u64 * CATCH_UP_US;
            node.clock += cost;
            node.busy += cost;
            for agent in moved {
                node.enqueue(agent, &self.agents[agent as usize]);
            }
        }
        let held = self.nodes[at].held.clone();
        for (partition, epoch) in &held {
            self.store
                .renew(*partition, id, *epoch, now_ms, TTL_MS)
                .map_err(|fenced| {
                    format!("invariant 1 (single holder): node {at} renew fenced: {fenced:?}")
                })?;
        }
        self.claims += held.len() as u64;
        self.store_write(STORE_TXN_US + STORE_FSYNC_US + held.len() as u64 * STORE_ROW_US);
        Ok(())
    }

    fn run_steps(&mut self, at: usize, end: u64) -> Result<(), String> {
        let node = &mut self.nodes[at];
        while node.clock < end {
            while let Some(&(ready_at, agent)) = node.parked.first() {
                if ready_at > node.clock {
                    break;
                }
                node.parked.pop_first();
                node.queue.push_back(agent);
            }
            let Some(agent) = node.queue.pop_front() else {
                match node.parked.first() {
                    Some(&(ready_at, _)) if ready_at < end => {
                        node.clock = ready_at;
                        continue;
                    }
                    _ => break,
                }
            };
            let state = &mut self.agents[agent as usize];
            if state.ready_at >= end {
                node.queue.push_front(agent);
                break;
            }
            node.clock = node.clock.max(state.ready_at);
            let partition = state.partition;
            let token = Token {
                partition,
                epoch: node.held[&partition],
            };
            let step = state.logged / 2;
            for record in [
                Record::Started {
                    thread: agent,
                    step,
                    hand: None,
                },
                Record::Done {
                    thread: agent,
                    step,
                },
            ] {
                self.store
                    .append(token, node.id, record)
                    .map_err(|fenced| {
                        format!("invariant 1 (single holder): node {at} append fenced: {fenced:?}")
                    })?;
            }
            state.logged += 2;
            node.clock += STEP_US;
            node.busy += STEP_US;
            state.ready_at = node.clock + NET_RTT_US + state.think_us;
            *self.pending.entry(partition).or_default() += RECORDS_PER_STEP;
            node.enqueue(agent, state);
        }
        Ok(())
    }

    /// Commits pending records, at most RECORDS_PER_TXN_MAX per transaction, round-robin
    /// over partitions, until nothing is pending or the store's tick is spent.
    fn commit(&mut self, end: u64) {
        self.store_clock = self.store_clock.max(self.now);
        loop {
            let mut progressed = false;
            let order = (self.cursor..self.count.get()).chain(0..self.cursor);
            for partition in order.map(Partition) {
                if self.store_clock >= end {
                    self.cursor = partition.0;
                    return;
                }
                let Some(pending) = self.pending.get_mut(&partition) else {
                    continue;
                };
                let records = (*pending).min(RECORDS_PER_TXN_MAX);
                *pending -= records;
                if *pending == 0 {
                    self.pending.remove(&partition);
                }
                let cost = self.mutation.commit_us(records);
                self.store_clock += cost;
                self.store_busy += cost;
                self.part_busy[partition.0 as usize] += cost;
                self.committed += records;
                self.txns += 1;
                progressed = true;
            }
            self.cursor = 0;
            if !progressed || self.mutation == ScaleMutation::IgnoreHot {
                return;
            }
        }
    }

    fn check_logs(&mut self) -> Result<(), String> {
        for log in self.store.logs.values_mut() {
            for entry in log.drain(..) {
                let (thread, step) = entry.record.key();
                let agent = self.agents.get_mut(thread as usize).ok_or_else(|| {
                    format!("invariant 2 (one run per step): unknown thread {thread}")
                })?;
                let fits = step == agent.checked / 2
                    && match entry.record {
                        Record::Started { .. } => agent.checked % 2 == 0,
                        Record::Done { .. } => agent.checked % 2 == 1,
                        Record::Uncertain { .. } => false,
                    };
                if !fits {
                    return Err(format!(
                        "invariant 2 (one run per step): thread {thread} logged {:?} after {} records",
                        entry.record, agent.checked
                    ));
                }
                agent.checked += 1;
            }
        }
        match self
            .agents
            .iter()
            .position(|agent| agent.checked != agent.logged)
        {
            Some(thread) => Err(format!(
                "invariant 2 (one run per step): thread {thread} ran {} records but the store logged {}",
                self.agents[thread].logged, self.agents[thread].checked
            )),
            None => Ok(()),
        }
    }

    fn tick(&mut self) -> Result<(), String> {
        let end = self.now + TICK_US;
        for at in 0..self.nodes.len() {
            self.nodes[at].clock = self.nodes[at].clock.max(self.now);
            self.lease(at)?;
            self.run_steps(at, end)?;
        }
        self.commit(end);
        if self.store_clock < end
            && let Some((partition, records)) = self.pending.first_key_value()
        {
            return Err(format!(
                "invariant 5 (no starved partition): partition {} kept {records} records pending while the store idled {} us of the tick",
                partition.0,
                end - self.store_clock
            ));
        }
        self.check_logs()?;
        self.now = end;
        Ok(())
    }

    fn settled(&self) -> bool {
        let live = self.live();
        let now_ms = self.now / US_PER_MS;
        self.nodes
            .iter()
            .all(|node| node.clock < self.now + STEP_US)
            && assignment(self.count, &live)
                .iter()
                .all(|(partition, owner)| {
                    self.store.rows[partition].holder() == Some(owner)
                        && self
                            .nodes
                            .iter()
                            .any(|node| node.id == *owner && node.held.contains_key(partition))
                })
            && self.nodes.iter().all(|node| {
                plan(
                    &node.id,
                    self.count,
                    &live,
                    &node.held,
                    &self.store.rows,
                    now_ms,
                )
                .release
                .is_empty()
            })
    }

    fn stage(&mut self, nodes: usize) -> Result<Stage, String> {
        while self.nodes.len() < nodes {
            self.join();
        }
        let catch_up_ticks = (self.agents.len() as u64 * CATCH_UP_US).div_ceil(TICK_US);
        let warmup_max = catch_up_ticks + SETTLE_TICKS;
        let started = self.now;
        let mut ticks = 0;
        loop {
            self.tick()?;
            ticks += 1;
            if self.settled() {
                break;
            }
            if ticks > warmup_max {
                return Err(format!(
                    "invariant 3 (progress): {} nodes not settled after {warmup_max} ticks",
                    self.nodes.len()
                ));
            }
        }
        let warmup_ms = (self.now - started) / US_PER_MS;
        let (committed, claims, busy, txns) =
            (self.committed, self.claims, self.store_busy, self.txns);
        let part_busy = self.part_busy.clone();
        let node_busy = self.nodes.iter().map(|node| node.busy).collect::<Vec<_>>();
        for _ in 0..MEASURE_TICKS {
            self.tick()?;
        }
        let window_us = MEASURE_TICKS * TICK_US;
        let per_s = |count: u64| count * MS_PER_S * US_PER_MS / window_us;
        let mut part_ppm = self
            .part_busy
            .iter()
            .zip(&part_busy)
            .map(|(after, before)| (after - before) * PPM / window_us)
            .collect::<Vec<_>>();
        part_ppm.sort_unstable();
        let p99_at = (part_ppm.len() as u64 * P99_PPM).div_ceil(PPM).max(1) - 1;
        Ok(Stage {
            nodes,
            steps_per_s: per_s((self.committed - committed) / RECORDS_PER_STEP),
            claims_per_s: per_s(self.claims - claims),
            txns_per_s: per_s(self.txns - txns),
            store_ppm: (self.store_busy - busy) * PPM / window_us,
            part_max_ppm: part_ppm.last().copied().unwrap_or(0),
            part_p99_ppm: part_ppm[p99_at as usize],
            node_max_ppm: self
                .nodes
                .iter()
                .zip(&node_busy)
                .map(|(node, before)| ((node.busy - before) * PPM / window_us).min(PPM))
                .max()
                .unwrap_or(0),
            idle_nodes: self
                .nodes
                .iter()
                .filter(|node| node.held.is_empty())
                .count() as u64,
            backlog: self.pending.values().sum(),
            warmup_ms,
        })
    }
}

pub struct ScaleOutcome {
    pub digest: u64,
    pub wrong: Option<String>,
    pub stats: ScaleStats,
    pub shape: String,
    lines: Vec<String>,
}

/// Runs every world (1k and 10k agents) even after one goes wrong, so a mutation is caught
/// only when every world catches it.
fn simulate(seed: u64, config: &ScaleConfig) -> ScaleOutcome {
    let mut stats = ScaleStats {
        seeds: 1,
        ..ScaleStats::default()
    };
    let mut digest = FNV_OFFSET;
    let mut shapes = Vec::new();
    let mut lines = Vec::new();
    let mut errors = Vec::new();
    for agents in AGENT_COUNTS {
        let (mut world, shape) = World::new(seed, agents, config);
        shapes.push(shape.clone());
        let mut stages = Vec::new();
        let mut wrong = None;
        for nodes in &config.nodes {
            match world.stage(*nodes) {
                Ok(stage) => stages.push(stage),
                Err(message) => {
                    wrong = Some(format!(
                        "{shape} at {} ms: {message}",
                        world.now / US_PER_MS
                    ));
                    break;
                }
            }
        }
        let base = stages.first().map_or(1, |stage| stage.steps_per_s.max(1));
        let pct = PPM / PERCENT;
        let bp = PPM / BASIS_POINTS;
        for stage in &stages {
            for value in [
                stage.steps_per_s,
                stage.claims_per_s,
                stage.store_ppm,
                stage.warmup_ms,
                stage.txns_per_s,
                stage.part_max_ppm,
                stage.part_p99_ppm,
                stage.node_max_ppm,
                stage.idle_nodes,
                stage.backlog,
            ] {
                digest = fnv(digest, &value.to_le_bytes());
            }
            lines.push(format!(
                "  agents={agents} nodes={} steps_per_s={} claims_per_s={} txns_per_s={} store_util_pct={} part_util_max_bp={} part_util_p99_bp={} node_util_max_pct={} idle_nodes={} backlog={} speedup={}.{:03} warmup_ms={}",
                stage.nodes,
                stage.steps_per_s,
                stage.claims_per_s,
                stage.txns_per_s,
                stage.store_ppm / pct,
                stage.part_max_ppm / bp,
                stage.part_p99_ppm / bp,
                stage.node_max_ppm / pct,
                stage.idle_nodes,
                stage.backlog,
                stage.steps_per_s / base,
                stage.steps_per_s * MILLI / base % MILLI,
                stage.warmup_ms
            ));
        }
        stats.record(agents, &stages, wrong.is_none());
        if wrong.is_none()
            && config.load == ScaleLoad::Uniform
            && let Some(at) = stages.iter().find(|stage| stage.nodes == SPEEDUP_NODES)
            && at.steps_per_s < SPEEDUP_MIN * stages[0].steps_per_s
        {
            wrong = Some(format!(
                "{shape}: invariant 4 (scale-out): {SPEEDUP_NODES} nodes ran {} steps/s, under {SPEEDUP_MIN}x the {} steps/s of 1 node",
                at.steps_per_s, stages[0].steps_per_s
            ));
        }
        if wrong.is_some() {
            *stats.caught.entry(agents).or_default() += 1;
        }
        errors.push(wrong);
    }
    let wrong = if config.mutation == ScaleMutation::None {
        errors.into_iter().flatten().next()
    } else {
        errors
            .iter()
            .all(Option::is_some)
            .then(|| errors.into_iter().flatten().collect::<Vec<_>>().join(" | "))
    };
    ScaleOutcome {
        digest,
        wrong,
        stats,
        shape: shapes.join(" "),
        lines,
    }
}

pub fn judge(
    seed: u64,
    config: &ScaleConfig,
    twice: bool,
    verbose: bool,
    totals: &mut ScaleStats,
) -> Option<String> {
    let first = simulate(seed, config);
    totals.add(&first.stats);
    if verbose {
        println!("seed {seed} digest {:016x} {}", first.digest, first.shape);
        for line in &first.lines {
            println!("{line}");
        }
    }
    if first.wrong.is_some() || !twice {
        return first.wrong;
    }
    let second = simulate(seed, config);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, config: &ScaleConfig) -> String {
    let mut command = format!("nanocode-sim --scenario scale --seed {seed}");
    if config.load != ScaleLoad::Uniform {
        command.push_str(" --load ");
        command.push_str(config.load.name());
    }
    if config.nodes != DEFAULT_NODES {
        command.push_str(" --nodes ");
        command.push_str(
            &config
                .nodes
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    if config.mutation != ScaleMutation::None {
        command.push_str(" --mutation ");
        command.push_str(config.mutation.name());
    }
    command
}

pub fn report(
    seeds: &Range<u64>,
    passed: u64,
    failed: u64,
    twice: bool,
    config: &ScaleConfig,
    totals: &ScaleStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=scale load={} mutation={} passed={passed} failed={failed} twice={twice} stats={}",
        seeds.start,
        seeds.end,
        config.load.name(),
        config.mutation.name(),
        totals.summary()
    );
    match config.mutation {
        ScaleMutation::None if failed == 0 => ExitCode::SUCCESS,
        ScaleMutation::None => ExitCode::FAILURE,
        mutation if failed > 0 => {
            println!("MUTATION {} caught by {failed} seeds", mutation.name());
            ExitCode::SUCCESS
        }
        mutation => {
            println!("MUTATION {} NOT caught", mutation.name());
            ExitCode::FAILURE
        }
    }
}
