//! Live sync under faults, driving nanocode-live-fold. Per seed: a partitioned
//! log whose holders change by epoch as nodes fail and rejoin, nodes that follow
//! every partition and fold one shared Live per view instance for their own
//! subscribers, sockets that backpressure, coalesce, overflow and drop, and
//! clients (Replica) that resubscribe by cursor on a random node and send
//! optimistic mutations they resend after a drop. A reference folded only from
//! the log judges every frame a client applies.

mod fold;
mod model;

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::Range,
    path::PathBuf,
    pin::pin,
    process::ExitCode,
    sync::{Arc, OnceLock},
    task::{Context, Poll, Waker},
};

use nanocode_live::intent::{
    self, Admit, Applier, Intents, Ledger, Optimistic, Outcome, Rejection, Settled, Submitting,
    Verdict, Views,
};
use nanocode_live_fold::{
    Access, Ack, ClientFrame, Cursor, Entry, ErrCode, Failure, Intent, Live, Replica, Resume, Seal,
    ServerFrame, decode_client, decode_server, encode, sim, snapshot_frame,
};
use nanocode_partitions::{Partition, assignee};
use nanocode_sync::{Inbox, Slot};
use serde_json::{Value, json};

use crate::rng::{FNV_OFFSET, Rng, fnv};
use fold::{Rows, from_rows, puts, to_rows};
use model::{Layout, Policy, READERS, Shape, State, ThreadsView, reference, rejected};

const TITLES: [&str; 5] = ["", "plan", "ship", "fix", "ship"];
const QUIESCE_ROUNDS: usize = 4_096;

static TRACE_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn trace_to(dir: &str) {
    let _ = TRACE_DIR.set(PathBuf::from(dir));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveMutation {
    None,
    SettleDropsDeletes,
    SettleKeepsStart,
    ResumeSkipsFirst,
    FloorIgnored,
    RebaseSkipped,
    SnapshotSkipsRow,
    SnapshotAtHead,
    OutlineLeaks,
    EncodeEveryFrame,
    RebaseDropsTouched,
    RepeatsReapplied,
    AckBeforeCommit,
    DropOnAck,
    UnknownRefuses,
    AckEarly,
    InstancePerSocket,
    CoalesceDropsFrame,
    RebuildSkipsFirst,
    OverloadKeepsFrames,
}

impl LiveMutation {
    const ALL: [Self; 20] = [
        Self::None,
        Self::SettleDropsDeletes,
        Self::SettleKeepsStart,
        Self::ResumeSkipsFirst,
        Self::FloorIgnored,
        Self::RebaseSkipped,
        Self::SnapshotSkipsRow,
        Self::SnapshotAtHead,
        Self::OutlineLeaks,
        Self::EncodeEveryFrame,
        Self::RebaseDropsTouched,
        Self::RepeatsReapplied,
        Self::AckBeforeCommit,
        Self::DropOnAck,
        Self::UnknownRefuses,
        Self::AckEarly,
        Self::InstancePerSocket,
        Self::CoalesceDropsFrame,
        Self::RebuildSkipsFirst,
        Self::OverloadKeepsFrames,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SettleDropsDeletes => "settle_drops_deletes",
            Self::SettleKeepsStart => "settle_keeps_start",
            Self::ResumeSkipsFirst => "resume_skips_first",
            Self::FloorIgnored => "floor_ignored",
            Self::RebaseSkipped => "rebase_skipped",
            Self::SnapshotSkipsRow => "snapshot_skips_row",
            Self::SnapshotAtHead => "snapshot_at_head",
            Self::OutlineLeaks => "outline_leaks",
            Self::EncodeEveryFrame => "encode_every_frame",
            Self::RebaseDropsTouched => "rebase_drops_touched",
            Self::RepeatsReapplied => "repeats_reapplied",
            Self::AckBeforeCommit => "ack_before_commit",
            Self::DropOnAck => "drop_on_ack",
            Self::UnknownRefuses => "unknown_refuses",
            Self::AckEarly => "ack_early",
            Self::InstancePerSocket => "instance_per_socket",
            Self::CoalesceDropsFrame => "coalesce_drops_frame",
            Self::RebuildSkipsFirst => "rebuild_skips_first",
            Self::OverloadKeepsFrames => "overload_keeps_frames",
        }
    }

    const fn fold(self) -> sim::Mutation {
        match self {
            Self::SettleDropsDeletes => sim::Mutation::SettleDropsDeletes,
            Self::SettleKeepsStart => sim::Mutation::SettleKeepsStart,
            Self::ResumeSkipsFirst => sim::Mutation::ResumeSkipsFirst,
            Self::FloorIgnored => sim::Mutation::FloorIgnored,
            Self::RebaseSkipped => sim::Mutation::RebaseSkipped,
            Self::SnapshotSkipsRow => sim::Mutation::SnapshotSkipsRow,
            Self::SnapshotAtHead => sim::Mutation::SnapshotAtHead,
            Self::OutlineLeaks => sim::Mutation::OutlineLeaks,
            Self::EncodeEveryFrame => sim::Mutation::EncodeEveryFrame,
            Self::RebaseDropsTouched => sim::Mutation::RebaseDropsTouched,
            _ => sim::Mutation::None,
        }
    }

    const fn intent(self) -> intent::sim::Mutation {
        match self {
            Self::RepeatsReapplied => intent::sim::Mutation::RepeatsReapplied,
            Self::AckBeforeCommit => intent::sim::Mutation::AckBeforeCommit,
            Self::DropOnAck => intent::sim::Mutation::DropOnAck,
            Self::UnknownRefuses => intent::sim::Mutation::UnknownRefuses,
            _ => intent::sim::Mutation::None,
        }
    }

    pub fn names() -> String {
        Self::ALL[1..]
            .iter()
            .map(|mutation| mutation.name())
            .collect::<Vec<_>>()
            .join("|")
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LiveStats {
    pub seeds: u64,
    pub versioned: u64,
    pub commits: u64,
    pub frames: u64,
    pub diffs: u64,
    pub snaps: u64,
    pub acks: u64,
    pub reconciled: u64,
    pub drops: u64,
    pub failovers: u64,
    pub resumes: u64,
    pub cross_node: u64,
    pub rebased: u64,
    pub inside: u64,
    pub overloads: u64,
    pub coalesced: u64,
    pub unknown: u64,
    pub refused: u64,
    pub repeats: u64,
    pub batches: u64,
    pub folds: u64,
    pub encodes: u64,
    pub sends: u64,
    pub max_subs: u64,
}

impl LiveStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.versioned += other.versioned;
        self.commits += other.commits;
        self.frames += other.frames;
        self.diffs += other.diffs;
        self.snaps += other.snaps;
        self.acks += other.acks;
        self.reconciled += other.reconciled;
        self.drops += other.drops;
        self.failovers += other.failovers;
        self.resumes += other.resumes;
        self.cross_node += other.cross_node;
        self.rebased += other.rebased;
        self.inside += other.inside;
        self.overloads += other.overloads;
        self.coalesced += other.coalesced;
        self.unknown += other.unknown;
        self.refused += other.refused;
        self.repeats += other.repeats;
        self.batches += other.batches;
        self.folds += other.folds;
        self.encodes += other.encodes;
        self.sends += other.sends;
        if other.max_subs > self.max_subs {
            self.max_subs = other.max_subs;
        }
    }
}

struct LogEntry {
    pos: u64,
    epoch: u64,
    record: Value,
}

struct Applied {
    at: Cursor,
    rejected: bool,
}

struct Instance {
    shape: Shape,
    live: Live<ThreadsView>,
    subs: BTreeSet<(usize, u32)>,
}

#[derive(Clone)]
enum Call {
    Commit(u32, Vec<(u64, Value)>),
    Ack(usize, Ack),
}

#[derive(Default)]
struct SimViews {
    calls: Inbox<Call>,
}

impl Views for SimViews {
    type Conn = usize;
    type Record = Value;

    fn commit(&self, partition: u32, _epoch: u64, batch: Vec<(u64, Value)>) {
        self.calls.push(Call::Commit(partition, batch));
    }

    fn ack(&self, conn: &usize, ack: Ack) {
        self.calls.push(Call::Ack(*conn, ack));
    }
}

#[derive(Default)]
struct SimApplier {
    partitions: u32,
    next: Slot<Result<(), Rejection>>,
    submitted: Inbox<Intent>,
}

impl Applier for SimApplier {
    type Record = Value;

    fn submit<'a>(&'a self, intent: &'a Intent) -> Submitting<'a> {
        let result = self.next.take().unwrap_or(Ok(()));
        if !matches!(result, Err(Rejection::Refused(_))) {
            self.submitted.push(intent.clone());
        }
        Box::pin(std::future::ready(result))
    }

    fn partition(&self, intent: &Intent) -> u32 {
        let thread = intent
            .args
            .get("thread")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        small(thread % u64::from(self.partitions.max(1)))
    }

    fn outcome(&self, record: &Value) -> Option<Outcome> {
        outcome_of(record)
    }
}

fn failure_of(value: &Value) -> Failure {
    Failure {
        code: value["code"].as_str().unwrap_or_default().to_owned(),
        message: value["message"].as_str().unwrap_or_default().to_owned(),
    }
}

fn outcome_of(record: &Value) -> Option<Outcome> {
    let client = record.get("client")?.as_str()?.to_owned();
    let mid = record.get("mid")?.as_u64()?;
    let verdict = if let Some(repeat) = record.get("repeat") {
        Verdict::Repeated(
            match repeat
                .get("cursor")
                .and_then(|cursor| serde_json::from_value::<Cursor>(cursor.clone()).ok())
            {
                Some(cursor) => Settled::Applied(cursor),
                None => Settled::Failed(failure_of(&repeat["error"])),
            },
        )
    } else if let Some(error) = record.get("error").filter(|error| !error.is_null()) {
        Verdict::Failed(failure_of(error))
    } else {
        Verdict::Applied
    };
    Some(Outcome {
        client,
        mid,
        verdict,
    })
}

fn now<F: Future>(future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

struct Node {
    name: String,
    alive: bool,
    follow: Vec<Cursor>,
    instances: BTreeMap<String, Instance>,
    views: Arc<SimViews>,
    applier: Arc<SimApplier>,
    intents: Vec<Intents<SimApplier, SimViews>>,
}

struct ServerSub {
    key: String,
    access: Access,
    ahead: Option<Cursor>,
}

struct Frame {
    sub: Option<u32>,
    text: String,
    stale: bool,
}

struct Socket {
    node: usize,
    client: usize,
    alive: bool,
    writable: bool,
    queue: Vec<Frame>,
    writes: VecDeque<Vec<String>>,
    inbound: VecDeque<String>,
    subs: BTreeMap<u32, ServerSub>,
    /// Subscriptions this socket dropped for overload that the client has not resubscribed yet.
    dropped: BTreeSet<u32>,
}

struct ClientSub {
    key: nanocode_live_fold::ViewKey,
    shape: Shape,
    access: Access,
    replica: Replica,
    node: Option<usize>,
    synced: bool,
    gone: bool,
}

struct Client {
    name: String,
    reader: &'static str,
    socket: Option<usize>,
    subs: BTreeMap<u32, ClientSub>,
    next_sub: u32,
    optimistic: Optimistic<Intent>,
    unacked: BTreeMap<u64, String>,
    events: Vec<Value>,
}

struct World {
    rng: Rng,
    seed: u64,
    mutation: LiveMutation,
    layout: Layout,
    threads: u32,
    ring: usize,
    bound: usize,
    log: Vec<Vec<LogEntry>>,
    states: Vec<Vec<(u64, State)>>,
    empty: State,
    next_pos: u64,
    epochs: Vec<u64>,
    holders: Vec<Option<usize>>,
    inputs: Vec<VecDeque<Intent>>,
    applied: BTreeMap<(String, u64), Vec<Applied>>,
    ledgers: Vec<Ledger>,
    nodes: Vec<Node>,
    sockets: Vec<Socket>,
    clients: Vec<Client>,
    policy: Policy,
    stats: LiveStats,
    wrong: Option<String>,
    digest: u64,
    tracing: bool,
    posts: u64,
}

pub struct LiveOutcome {
    pub digest: u64,
    pub shape: String,
    pub stats: LiveStats,
    pub wrong: Option<String>,
}

fn small(value: u64) -> u32 {
    u32::try_from(value).expect("a small count fits in u32")
}

impl World {
    fn new(seed: u64, mutation: LiveMutation) -> Self {
        let mut rng = Rng::derive(seed, "live");
        let partitions = small(rng.range(1, 3));
        let threads = partitions * small(rng.range(1, 4));
        let versioned = rng.chance(500_000);
        let nodes = rng.range(2, 5);
        let clients = rng.range(2, 10);
        let ring = usize::try_from(rng.range(1, 6)).expect("a ring capacity fits in usize");
        let bound = usize::try_from(rng.range(2, 12)).expect("a queue bound fits in usize");
        let tracing = TRACE_DIR.get().is_some();
        let mut world = Self {
            rng,
            seed,
            mutation,
            layout: Layout {
                partitions,
                versioned,
            },
            threads,
            ring,
            bound,
            log: (0..partitions).map(|_| Vec::new()).collect(),
            states: (0..partitions).map(|_| Vec::new()).collect(),
            empty: State::new(),
            next_pos: 0,
            epochs: vec![0; partitions as usize],
            holders: vec![None; partitions as usize],
            inputs: (0..partitions).map(|_| VecDeque::new()).collect(),
            applied: BTreeMap::new(),
            ledgers: (0..partitions).map(|_| Ledger::new()).collect(),
            nodes: Vec::new(),
            sockets: Vec::new(),
            clients: Vec::new(),
            policy: Policy {
                partitions,
                threads,
            },
            stats: LiveStats {
                seeds: 1,
                versioned: u64::from(versioned),
                ..LiveStats::default()
            },
            wrong: None,
            digest: FNV_OFFSET,
            tracing,
            posts: 0,
        };
        for index in 0..nodes {
            let node = world.fresh_node(format!("n{index}"));
            world.nodes.push(node);
        }
        world.rebalance();
        for index in 0..clients {
            let reader = READERS[world.rng.pick(READERS.len())];
            let name = format!(
                "{:08x}-{:04x}-4000-8000-{:012x}",
                world.rng.draw() & 0xffff_ffff,
                world.rng.draw() & 0xffff,
                index
            );
            world.clients.push(Client {
                optimistic: Optimistic::new(name.clone(), intent::FIRST_MID),
                name,
                reader,
                socket: None,
                subs: BTreeMap::new(),
                next_sub: 1,
                unacked: BTreeMap::new(),
                events: Vec::new(),
            });
            let part = small(world.rng.below(u64::from(partitions)));
            let client = world.clients.len() - 1;
            world.subscribe(client, Shape::List(part));
        }
        world
    }

    const fn partitions(&self) -> u32 {
        self.layout.partitions
    }

    fn fresh_node(&self, name: String) -> Node {
        let views = Arc::new(SimViews::default());
        let applier = Arc::new(SimApplier {
            partitions: self.partitions(),
            ..SimApplier::default()
        });
        Node {
            name,
            alive: true,
            follow: (0..self.partitions())
                .map(|part| Cursor::new(part, 0, 0))
                .collect(),
            instances: BTreeMap::new(),
            intents: (0..self.partitions())
                .map(|_| Intents::new(Arc::clone(&applier), Arc::clone(&views)))
                .collect(),
            views,
            applier,
        }
    }

    fn fail(&mut self, message: String) {
        if self.wrong.is_none() {
            self.wrong = Some(message);
        }
    }

    fn state_at(&self, part: u32, pos: u64) -> &State {
        let states = &self.states[part as usize];
        let index = states.partition_point(|(at, _)| *at <= pos);
        if index == 0 {
            &self.empty
        } else {
            &states[index - 1].1
        }
    }

    fn head(&self, part: u32) -> u64 {
        self.log[part as usize].last().map_or(0, |entry| entry.pos)
    }

    fn rebalance(&mut self) {
        let live = self
            .nodes
            .iter()
            .filter(|node| node.alive)
            .map(|node| node.name.clone())
            .collect::<BTreeSet<_>>();
        for part in 0..self.partitions() {
            let holder = assignee(Partition(part), &live)
                .and_then(|name| self.nodes.iter().position(|node| &node.name == name));
            if holder != self.holders[part as usize] {
                self.holders[part as usize] = holder;
                self.epochs[part as usize] += 1;
                self.stats.failovers += 1;
            }
        }
    }

    fn commit(&mut self, part: u32, record: Value) {
        let gap = if self.rng.chance(200_000) {
            self.rng.range(1, 3)
        } else {
            0
        };
        self.next_pos += 1 + gap;
        let pos = self.next_pos;
        let epoch = self.epochs[part as usize];
        let mut state = self.states[part as usize]
            .last()
            .map(|(_, state)| state.clone())
            .unwrap_or_default();
        model::effect(&mut state, &record);
        self.states[part as usize].push((pos, state));
        if let Some(outcome) = outcome_of(&record) {
            self.ledgers[part as usize].fold(Cursor::new(part, epoch, pos), outcome);
        }
        if let (Some(client), Some(mid), None) = (
            record.get("client").and_then(Value::as_str),
            record.get("mid").and_then(Value::as_u64),
            record.get("repeat"),
        ) {
            self.applied
                .entry((client.to_owned(), mid))
                .or_default()
                .push(Applied {
                    at: Cursor::new(part, epoch, pos),
                    rejected: rejected(&record),
                });
        }
        self.digest = fnv(self.digest, record.to_string().as_bytes());
        self.log[part as usize].push(LogEntry { pos, epoch, record });
        self.stats.commits += 1;
    }

    fn thread_in(&mut self, part: u32) -> u32 {
        let per = self.threads / self.partitions();
        part + self.partitions() * small(self.rng.below(u64::from(per)))
    }

    fn body(&mut self) -> String {
        self.posts += 1;
        format!("secret-{}", self.posts)
    }

    fn app_write(&mut self) {
        let part = small(self.rng.below(u64::from(self.partitions())));
        if self.holders[part as usize].is_none() {
            return;
        }
        let thread = self.thread_in(part);
        let record = match self.rng.below(4) {
            0 => {
                json!({"thread": thread, "kind": "rename", "title": TITLES[1 + self.rng.pick(TITLES.len() - 1)]})
            }
            1 => json!({"thread": thread, "kind": "post", "body": self.body()}),
            2 => json!({"thread": thread, "kind": "seal", "on": self.rng.chance(500_000)}),
            _ => json!({"thread": thread, "kind": "drop"}),
        };
        self.commit(part, record);
    }

    fn apply_inputs(&mut self, part: u32, all: bool) -> bool {
        if self.holders[part as usize].is_none() || self.inputs[part as usize].is_empty() {
            return false;
        }
        let available = self.inputs[part as usize].len() as u64;
        let take = if all {
            available
        } else {
            self.rng.range(1, available)
        };
        for _ in 0..take {
            let Some(intent) = self.inputs[part as usize].pop_front() else {
                break;
            };
            let thread_id = intent
                .args
                .get("thread")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if let Admit::Repeat(settled) = self.ledgers[part as usize].admit(part, &intent) {
                let settled = match settled {
                    Settled::Applied(cursor) => json!({"cursor": cursor}),
                    Settled::Failed(failure) => json!({"error": failure}),
                };
                self.stats.repeats += 1;
                self.commit(
                    part,
                    json!({"thread": thread_id, "kind": "repeat", "client": intent.client, "mid": intent.mid, "repeat": settled}),
                );
                continue;
            }
            let thread = intent
                .args
                .get("thread")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let mut record = json!({
                "thread": thread,
                "kind": intent.name,
                "client": intent.client,
                "mid": intent.mid,
            });
            for field in ["title", "body", "on"] {
                if let Some(value) = intent.args.get(field) {
                    record[field] = value.clone();
                }
            }
            if intent.name == "rename"
                && intent.args.get("title").and_then(Value::as_str) == Some("")
            {
                record["error"] = json!({"code": "invalid", "message": "a title is not empty"});
            }
            self.commit(part, record);
        }
        true
    }

    fn instance_key(&self, view: &str, socket: usize) -> String {
        if self.mutation == LiveMutation::InstancePerSocket {
            format!("{view}#{socket}")
        } else {
            view.to_owned()
        }
    }

    fn create_instance(&mut self, node: usize, key: &str, shape: Shape) {
        let part = shape.partition(self.partitions());
        let at = self.nodes[node].follow[part as usize];
        let mut view = ThreadsView::new(shape, self.layout);
        let mut ops = Vec::new();
        let skip = usize::from(self.mutation == LiveMutation::RebuildSkipsFirst);
        for entry in self.log[part as usize]
            .iter()
            .filter(|entry| entry.pos <= at.pos)
            .skip(skip)
        {
            nanocode_live_fold::View::apply(&mut view, entry.pos, &entry.record, &mut ops);
            ops.clear();
        }
        let live = Live::new(shape.key(), view, at, self.ring);
        let expected = reference(
            self.state_at(part, at.pos),
            shape,
            Access::Full,
            self.layout,
        );
        let rebuilt = from_rows(live.table().rows());
        if rebuilt != expected {
            self.fail(format!(
                "rebuild_equals_fold: node {} rebuilt {key} at pos {} with {} rows, the fold from pos 0 has {}",
                self.nodes[node].name,
                at.pos,
                rebuilt.len(),
                expected.len()
            ));
        }
        self.nodes[node].instances.insert(
            key.to_owned(),
            Instance {
                shape,
                live,
                subs: BTreeSet::new(),
            },
        );
    }

    fn broadcast(&mut self, node: usize, key: &str, entry: &Arc<Entry>) {
        self.broadcast_except(node, key, entry, None);
    }

    fn broadcast_except(
        &mut self,
        node: usize,
        key: &str,
        entry: &Arc<Entry>,
        except: Option<(usize, u32)>,
    ) {
        let Some(instance) = self.nodes[node].instances.get(key) else {
            return;
        };
        let subs = instance
            .subs
            .iter()
            .copied()
            .filter(|sub| Some(*sub) != except)
            .collect::<Vec<_>>();
        let before = sim::encodes();
        let mut levels = BTreeSet::new();
        for (socket, id) in subs {
            let Some(sub) = self.sockets[socket].subs.get(&id) else {
                continue;
            };
            if sub.ahead.is_some() {
                continue;
            }
            let access = sub.access;
            let Some(bytes) = entry.frame(access, &self.policy) else {
                continue;
            };
            levels.insert(access == Access::Outline);
            self.stats.sends += 1;
            self.enqueue(socket, Some(id), bytes.frame(id));
        }
        let encodes = sim::encodes() - before;
        self.stats.encodes += encodes;
        if encodes > levels.len() as u64 {
            self.fail(format!(
                "fanout_o_views: node {} encoded one diff of {key} {encodes} times for {} access levels",
                self.nodes[node].name,
                levels.len()
            ));
        }
    }

    fn follow(&mut self, node: usize, part: u32, limit: usize) -> bool {
        let after = self.nodes[node].follow[part as usize].pos;
        let batch = self.log[part as usize]
            .iter()
            .filter(|entry| entry.pos > after)
            .take(limit)
            .map(|entry| {
                (
                    Cursor::new(part, entry.epoch, entry.pos),
                    entry.record.clone(),
                )
            })
            .collect::<Vec<_>>();
        let Some(last) = batch.last().map(|(at, _)| *at) else {
            return false;
        };
        let partitions = self.partitions();
        let keys = self.nodes[node]
            .instances
            .iter()
            .filter(|(_, instance)| instance.shape.partition(partitions) == part)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let views = keys
            .iter()
            .filter_map(|key| self.nodes[node].instances.get(key))
            .map(|instance| instance.shape.key().canonical())
            .collect::<BTreeSet<_>>();
        if keys.len() > views.len() {
            self.fail(format!(
                "fanout_o_views: node {} folds {} instances for {} distinct views",
                self.nodes[node].name,
                keys.len(),
                views.len()
            ));
        }
        self.nodes[node].follow[part as usize] = last;
        let mut runs: Vec<(u64, Vec<(u64, Value)>)> = Vec::new();
        for (at, record) in &batch {
            match runs.last_mut() {
                Some((epoch, run)) if *epoch == at.epoch => run.push((at.pos, record.clone())),
                _ => runs.push((at.epoch, vec![(at.pos, record.clone())])),
            }
        }
        let epochs = batch
            .iter()
            .map(|(at, _)| (at.pos, at.epoch))
            .collect::<BTreeMap<_, _>>();
        let intents = self.nodes[node].intents[part as usize].clone();
        for (epoch, run) in runs {
            intents.commit(part, epoch, run);
        }
        for call in self.nodes[node].views.calls.drain() {
            match call {
                Call::Commit(part, run) => {
                    let run = run
                        .into_iter()
                        .map(|(pos, record)| (Cursor::new(part, epochs[&pos], pos), record))
                        .collect::<Vec<_>>();
                    self.fold_commit(node, &keys, &run);
                }
                Call::Ack(socket, ack) => self.deliver_ack(socket, ack),
            }
        }
        for key in keys {
            let ahead = self.nodes[node].instances.get(&key).map(|instance| {
                instance
                    .subs
                    .iter()
                    .copied()
                    .filter(|(socket, id)| {
                        self.sockets[*socket]
                            .subs
                            .get(id)
                            .and_then(|sub| sub.ahead)
                            .is_some_and(|cursor| cursor.pos <= last.pos)
                    })
                    .collect::<Vec<_>>()
            });
            for (socket, id) in ahead.unwrap_or_default() {
                let cursor = self.sockets[socket].subs.get(&id).and_then(|sub| sub.ahead);
                self.resume(socket, id, cursor);
            }
        }
        true
    }

    fn fold_commit(&mut self, node: usize, keys: &[String], batch: &[(Cursor, Value)]) {
        self.stats.batches += 1;
        for key in keys {
            let Some(instance) = self.nodes[node].instances.get_mut(key) else {
                continue;
            };
            let mut fold = instance.live.begin();
            for (at, record) in batch {
                fold.fold(*at, record);
            }
            self.stats.folds += batch.len() as u64;
            if let Some(entry) = fold.finish() {
                self.broadcast(node, key, &entry);
            }
        }
    }

    fn drain_node(&mut self, node: usize) {
        for intent in self.nodes[node].applier.submitted.drain() {
            let thread = intent
                .args
                .get("thread")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let part = small(thread % u64::from(self.partitions()));
            self.inputs[part as usize].push_back(intent);
        }
        for call in self.nodes[node].views.calls.drain() {
            match call {
                Call::Commit(..) => {
                    self.fail("wire: intents committed a batch outside a follow".to_owned())
                }
                Call::Ack(socket, ack) => self.deliver_ack(socket, ack),
            }
        }
    }

    fn set_ahead(&mut self, socket: usize, id: u32, ahead: Option<Cursor>) {
        if let Some(sub) = self.sockets[socket].subs.get_mut(&id) {
            sub.ahead = ahead;
        }
    }

    fn snapshot(&mut self, socket: usize, id: u32, key: &str, access: Access) {
        let node = self.sockets[socket].node;
        let Some(instance) = self.nodes[node].instances.get(key) else {
            return;
        };
        let snapshot = instance.live.snapshot();
        let Some(bytes) = snapshot_frame(&snapshot, access, &self.policy) else {
            return;
        };
        self.set_ahead(socket, id, None);
        self.enqueue(socket, Some(id), bytes.frame(id));
    }

    fn resume(&mut self, socket: usize, id: u32, cursor: Option<Cursor>) {
        let node = self.sockets[socket].node;
        let Some(sub) = self.sockets[socket].subs.get(&id) else {
            return;
        };
        let (key, access) = (sub.key.clone(), sub.access);
        self.stats.resumes += 1;
        let ahead = cursor.is_none_or(|cursor| {
            self.nodes[node]
                .instances
                .get(&key)
                .is_some_and(|instance| cursor.pos > instance.live.head().pos)
        });
        if !ahead
            && let Some(instance) = self.nodes[node].instances.get_mut(&key)
            && let Some(entry) = instance.live.advance()
        {
            self.broadcast_except(node, &key, &entry, Some((socket, id)));
        }
        for _ in 0..2 {
            let Some(instance) = self.nodes[node].instances.get_mut(&key) else {
                return;
            };
            match instance.live.resume(cursor.as_ref()) {
                Resume::Advance => {
                    let entry = instance.live.advance();
                    self.set_ahead(socket, id, cursor);
                    if let Some(entry) = entry {
                        self.broadcast(node, &key, &entry);
                    }
                }
                Resume::Ahead => {
                    self.set_ahead(socket, id, cursor);
                    return;
                }
                Resume::Snapshot => {
                    self.snapshot(socket, id, &key, access);
                    if let Some(entry) = self.nodes[node]
                        .instances
                        .get_mut(&key)
                        .and_then(|instance| instance.live.advance())
                    {
                        self.broadcast(node, &key, &entry);
                    }
                    return;
                }
                Resume::Diffs(entries) => {
                    let at = cursor.map_or(0, |cursor| cursor.pos);
                    let ring = instance.live.ring();
                    let original = ring.entries().find(|entry| entry.diff().to.pos > at);
                    let inside = original.is_some_and(|entry| entry.diff().from.pos < at);
                    if inside {
                        self.stats.inside += 1;
                    }
                    if entries
                        .first()
                        .zip(original)
                        .is_some_and(|(first, original)| !Arc::ptr_eq(first, original))
                    {
                        self.stats.rebased += 1;
                    }
                    self.set_ahead(socket, id, None);
                    for entry in entries {
                        if let Some(bytes) = entry.frame(access, &self.policy) {
                            self.enqueue(socket, Some(id), bytes.frame(id));
                        }
                    }
                    return;
                }
            }
        }
        self.fail(format!(
            "no_gap_no_dup: resume of sub {id} at {:?} still asked to advance after advancing",
            cursor.map(|cursor| cursor.pos)
        ));
    }

    fn deliver_ack(&mut self, socket: usize, mut ack: Ack) {
        if !self.sockets[socket].alive {
            return;
        }
        let node = self.sockets[socket].node;
        if let Ok(at) = ack.result {
            let keys = self.sockets[socket]
                .subs
                .values()
                .filter(|sub| sub.ahead.is_none())
                .map(|sub| sub.key.clone())
                .collect::<BTreeSet<_>>();
            for key in keys {
                let Some(instance) = self.nodes[node].instances.get_mut(&key) else {
                    continue;
                };
                if instance.shape.partition(self.layout.partitions) != at.partition
                    || instance.live.tip().pos >= at.pos
                {
                    continue;
                }
                if let Some(entry) = instance.live.advance() {
                    self.broadcast(node, &key, &entry);
                }
            }
        }
        if self.mutation == LiveMutation::AckEarly
            && let Ok(cursor) = &mut ack.result
        {
            cursor.pos -= 1;
        }
        let text = encode(&ServerFrame::Ack(ack));
        self.enqueue(socket, None, text);
    }

    fn enqueue(&mut self, socket: usize, sub: Option<u32>, text: String) {
        let bound = self.bound;
        let keeps = self.mutation == LiveMutation::OverloadKeepsFrames;
        let state = &self.sockets[socket];
        if !state.alive || (!keeps && sub.is_some_and(|id| !state.subs.contains_key(&id))) {
            return;
        }
        let stale = sub.filter(|id| state.dropped.contains(id));
        if let Some(id) = stale
            && state.writable
        {
            self.fail(format!(
                "overload_drops_frames: socket {socket} wrote a frame for sub {id} after dropping it for overload"
            ));
        }
        let state = &mut self.sockets[socket];
        if state.writable {
            state.writes.push_back(vec![text]);
            return;
        }
        state.queue.push(Frame {
            sub,
            text,
            stale: stale.is_some(),
        });
        if state
            .queue
            .iter()
            .filter(|frame| frame.sub.is_some())
            .count()
            <= bound
        {
            return;
        }
        let Some(victim) = sub.or_else(|| state.queue.iter().rev().find_map(|frame| frame.sub))
        else {
            return;
        };
        let first = state
            .queue
            .iter()
            .position(|frame| frame.sub == Some(victim))
            .unwrap_or(state.queue.len());
        if keeps {
            for frame in state.queue.iter_mut().filter(|frame| frame.sub == Some(victim)) {
                frame.stale = true;
            }
        } else {
            state.queue.retain(|frame| frame.sub != Some(victim));
        }
        let first = first.min(state.queue.len());
        state.queue.insert(
            first,
            Frame {
                sub: None,
                stale: false,
                text: encode(&ServerFrame::Err {
                    id: Some(victim),
                    code: ErrCode::Overloaded,
                    message: "the socket's queue overflowed".to_owned(),
                }),
            },
        );
        state.dropped.insert(victim);
        let held = state.queue.iter().filter(|frame| frame.sub.is_some()).count();
        self.stats.overloads += 1;
        self.remove_sub(socket, victim);
        if held > bound {
            self.fail(format!(
                "bounded_queue: socket {socket} holds {held} subscription frames after an overflow, over its bound {bound}"
            ));
        }
    }

    fn remove_sub(&mut self, socket: usize, id: u32) {
        let node = self.sockets[socket].node;
        let Some(sub) = self.sockets[socket].subs.remove(&id) else {
            return;
        };
        let empty = self.nodes[node]
            .instances
            .get_mut(&sub.key)
            .is_some_and(|instance| {
                instance.subs.remove(&(socket, id));
                instance.subs.is_empty()
            });
        if empty {
            self.nodes[node].instances.remove(&sub.key);
        }
    }

    fn drain(&mut self, socket: usize) {
        let stale = self.sockets[socket]
            .queue
            .iter()
            .find(|frame| frame.stale)
            .and_then(|frame| frame.sub);
        if let Some(id) = stale {
            self.fail(format!(
                "overload_drops_frames: socket {socket} wrote a frame for sub {id} after dropping it for overload"
            ));
        }
        let state = &mut self.sockets[socket];
        state.writable = true;
        if state.queue.is_empty() {
            return;
        }
        let mut frames = state
            .queue
            .drain(..)
            .map(|frame| frame.text)
            .collect::<Vec<_>>();
        if self.mutation == LiveMutation::CoalesceDropsFrame && frames.len() > 1 {
            frames.remove(0);
        }
        state.writes.push_back(frames);
        self.stats.coalesced += 1;
    }

    fn server_receive(&mut self, socket: usize, text: &str) {
        let node = self.sockets[socket].node;
        let client = self.sockets[socket].client;
        let frame = match decode_client(text) {
            Ok(frame) => frame,
            Err(error) => {
                self.fail(format!(
                    "wire: a client frame did not decode ({error}): {text}"
                ));
                return;
            }
        };
        match frame {
            ClientFrame::Sub { id, view, cursor } => {
                self.sockets[socket].dropped.remove(&id);
                self.remove_sub(socket, id);
                let Some(shape) = Shape::parse(&view, self.partitions(), self.threads) else {
                    let text = encode(&ServerFrame::Err {
                        id: Some(id),
                        code: ErrCode::UnknownView,
                        message: "no such view".to_owned(),
                    });
                    self.enqueue(socket, None, text);
                    return;
                };
                let access = self.policy.access(&view, self.clients[client].reader);
                if access == Access::Denied {
                    let text = encode(&ServerFrame::Err {
                        id: Some(id),
                        code: ErrCode::Denied,
                        message: "this reader may not read the view".to_owned(),
                    });
                    self.enqueue(socket, None, text);
                    return;
                }
                let canonical = view.canonical();
                let key = self.instance_key(&canonical, socket);
                if !self.nodes[node].instances.contains_key(&key) {
                    self.create_instance(node, &key, shape);
                }
                if let Some(instance) = self.nodes[node].instances.get_mut(&key) {
                    instance.subs.insert((socket, id));
                    self.stats.max_subs = self.stats.max_subs.max(instance.subs.len() as u64);
                }
                self.sockets[socket].subs.insert(
                    id,
                    ServerSub {
                        key,
                        access,
                        ahead: cursor,
                    },
                );
                self.resume(socket, id, cursor);
            }
            ClientFrame::Unsub { id } => self.remove_sub(socket, id),
            ClientFrame::Mut(intent) => {
                if intent.client != self.clients[client].name {
                    self.fail(format!(
                        "wire: a socket of {} carried a mutation of {}",
                        self.clients[client].name, intent.client
                    ));
                    return;
                }
                let result = if intent.name == "bogus" {
                    self.stats.refused += 1;
                    Err(Rejection::Refused(Failure {
                        code: "unknown_mutation".to_owned(),
                        message: "no such mutation".to_owned(),
                    }))
                } else if self.rng.chance(250_000) {
                    self.stats.unknown += 1;
                    Err(Rejection::Unknown("the submit timed out".to_owned()))
                } else {
                    Ok(())
                };
                self.nodes[node].applier.next.put(result);
                let thread = intent
                    .args
                    .get("thread")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let part = small(thread % u64::from(self.partitions()));
                let intents = self.nodes[node].intents[part as usize].clone();
                if now(intents.receive(socket, intent)).is_none() {
                    self.fail("liveness: an intent did not settle in one poll".to_owned());
                }
                self.drain_node(node);
            }
        }
    }

    fn trace(&mut self, client: usize, event: Value) {
        if self.tracing {
            self.clients[client].events.push(event);
        }
    }

    fn send(&mut self, client: usize, frame: &ClientFrame) {
        let Some(socket) = self.clients[client].socket else {
            return;
        };
        let text = encode(frame);
        if self.tracing {
            let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
            self.trace(client, json!({"k": "send", "frame": value}));
        }
        self.sockets[socket].inbound.push_back(text);
    }

    fn connect(&mut self, client: usize) {
        if self.clients[client].socket.is_some() {
            return;
        }
        let alive = (0..self.nodes.len())
            .filter(|node| self.nodes[*node].alive)
            .collect::<Vec<_>>();
        let node = alive[self.rng.pick(alive.len())];
        self.sockets.push(Socket {
            node,
            client,
            alive: true,
            writable: true,
            queue: Vec::new(),
            dropped: BTreeSet::new(),
            writes: VecDeque::new(),
            inbound: VecDeque::new(),
            subs: BTreeMap::new(),
        });
        self.clients[client].socket = Some(self.sockets.len() - 1);
        self.trace(client, json!({"k": "open", "node": node}));
        let mut frames = Vec::new();
        for (id, sub) in &mut self.clients[client].subs {
            if sub.gone {
                continue;
            }
            if sub.node.is_some_and(|last| last != node) && sub.replica.cursor().is_some() {
                self.stats.cross_node += 1;
            }
            sub.node = Some(node);
            sub.synced = false;
            frames.push(ClientFrame::Sub {
                id: *id,
                view: sub.key.clone(),
                cursor: sub.replica.cursor(),
            });
        }
        for frame in frames {
            self.send(client, &frame);
        }
        let muts = self.clients[client]
            .optimistic
            .resend()
            .map(|(mid, intent)| (mid, intent.name.clone(), ClientFrame::Mut(intent.clone())))
            .collect::<Vec<_>>();
        for (mid, name, frame) in muts {
            // An acked intent sent again is acked again, from the log's record of the first apply.
            self.clients[client].unacked.entry(mid).or_insert(name);
            self.send(client, &frame);
        }
    }

    fn drop_socket(&mut self, socket: usize) {
        if !self.sockets[socket].alive {
            return;
        }
        let client = self.sockets[socket].client;
        let node = self.sockets[socket].node;
        let ids = self.sockets[socket]
            .subs
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for id in ids {
            self.remove_sub(socket, id);
        }
        for intents in &self.nodes[node].intents {
            intents.close(socket);
        }
        let state = &mut self.sockets[socket];
        state.alive = false;
        state.queue.clear();
        state.writes.clear();
        state.inbound.clear();
        if self.clients[client].socket == Some(socket) {
            self.clients[client].socket = None;
            self.trace(client, json!({"k": "drop"}));
        }
        self.stats.drops += 1;
    }

    fn subscribe(&mut self, client: usize, shape: Shape) {
        let id = self.clients[client].next_sub;
        self.clients[client].next_sub += 1;
        let key = shape.key();
        let access = self.policy.access(&key, self.clients[client].reader);
        let node = self.clients[client]
            .socket
            .map(|socket| self.sockets[socket].node);
        self.clients[client].subs.insert(
            id,
            ClientSub {
                key: key.clone(),
                shape,
                access,
                replica: Replica::default(),
                node,
                synced: false,
                gone: false,
            },
        );
        self.send(
            client,
            &ClientFrame::Sub {
                id,
                view: key,
                cursor: None,
            },
        );
    }

    fn mutate(&mut self, client: usize) {
        let thread = self.rng.below(u64::from(self.threads));
        let mid = self.clients[client].optimistic.next();
        let (name, args) = match self.rng.below(16) {
            0..5 => (
                "rename",
                json!({"thread": thread, "title": TITLES[self.rng.pick(TITLES.len())]}),
            ),
            5..10 => ("post", json!({"thread": thread, "body": self.body()})),
            10..15 => (
                "seal",
                json!({"thread": thread, "on": self.rng.chance(500_000)}),
            ),
            _ => ("bogus", json!({"thread": thread})),
        };
        let intent = Intent {
            client: self.clients[client].name.clone(),
            mid,
            name: name.to_owned(),
            args,
        };
        let sent = self.clients[client].optimistic.send(intent.clone());
        debug_assert_eq!(sent, mid);
        self.clients[client].unacked.insert(mid, name.to_owned());
        self.send(client, &ClientFrame::Mut(intent));
    }

    fn view(&self, client: usize, part: u32) -> Cursor {
        let partitions = self.partitions();
        self.clients[client]
            .subs
            .values()
            .filter(|sub| !sub.gone && sub.shape.partition(partitions) == part)
            .map(|sub| {
                sub.replica
                    .cursor()
                    .unwrap_or_else(|| Cursor::new(part, 0, 0))
            })
            .min_by_key(|cursor| cursor.pos)
            .unwrap_or_else(|| Cursor::new(part, 0, u64::MAX))
    }

    fn leaks(
        &self,
        sub: &ClientSub,
        at: Cursor,
        puts: &[(String, String, Value)],
    ) -> Option<String> {
        if sub.access == Access::Full {
            return None;
        }
        if sub.access == Access::Denied {
            return Some(format!(
                "{} reached a denied subscriber",
                sub.key.canonical()
            ));
        }
        let state = self.state_at(at.partition, at.pos);
        puts.iter().find_map(|(table, key, value)| {
            if table != "post" {
                return None;
            }
            let thread = key
                .split('/')
                .next()
                .and_then(|id| id.parse::<u32>().ok())?;
            let sealed = state.get(&thread).is_some_and(|thread| thread.sealed);
            (sealed || value.get("sealed") == Some(&Value::Bool(true))).then(|| {
                format!(
                    "post {key} of sealed thread {thread} reached an outline subscriber of {}",
                    sub.key.canonical()
                )
            })
        })
    }

    fn check_view(&mut self, client: usize, id: u32) {
        let Some(sub) = self.clients[client].subs.get(&id) else {
            return;
        };
        let Some(cursor) = sub.replica.cursor() else {
            return;
        };
        let expected = reference(
            self.state_at(cursor.partition, cursor.pos),
            sub.shape,
            sub.access,
            self.layout,
        );
        let rows = from_rows(sub.replica.rows());
        if rows != expected {
            let extra = rows
                .iter()
                .find(|(row, value)| expected.get(*row) != Some(*value))
                .map(|(row, value)| (row.clone(), value.to_string()));
            let missing = expected
                .iter()
                .find(|(row, value)| rows.get(*row) != Some(*value))
                .map(|(row, value)| (row.clone(), value.to_string()));
            let message = format!(
                "client_view_equals_fold: client {client} ({:?}) sub {id} {} at pos {} differs from the fold (client-only or changed {extra:?}, fold-only or changed {missing:?})",
                sub.access,
                sub.key.canonical(),
                cursor.pos
            );
            self.fail(message);
        }
        if self.tracing {
            let shown = to_rows(&self.shown(client, id, rows.clone()));
            let rows = to_rows(&rows);
            self.trace(
                client,
                json!({"k": "check", "id": id, "cursor": cursor, "rows": rows, "shown": shown}),
            );
        }
    }

    fn shown(&self, client: usize, id: u32, mut rows: Rows) -> Rows {
        let _ = id;
        for (_, intent) in self.clients[client].optimistic.rebase() {
            let Some(thread) = intent.args.get("thread").and_then(Value::as_u64) else {
                continue;
            };
            let row = ("thread".to_owned(), thread.to_string());
            let Some(value) = rows.get_mut(&row) else {
                continue;
            };
            match intent.name.as_str() {
                "rename" => value["title"] = intent.args["title"].clone(),
                "seal" => value["sealed"] = intent.args["on"].clone(),
                _ => {}
            }
        }
        rows
    }

    fn client_receive(&mut self, client: usize, text: &str) {
        self.stats.frames += 1;
        self.digest = fnv(self.digest, text.as_bytes());
        if self.tracing {
            let value = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
            self.trace(client, json!({"k": "recv", "frame": value}));
        }
        let frame = match decode_server(text) {
            Ok(frame) => frame,
            Err(error) => {
                self.fail(format!(
                    "wire: a server frame did not decode ({error}): {text}"
                ));
                return;
            }
        };
        match frame {
            ServerFrame::Snap { id, cursor, rows } => {
                self.stats.snaps += 1;
                let Some(sub) = self.clients[client].subs.get(&id) else {
                    return;
                };
                let put = rows
                    .iter()
                    .map(|row| (row.table.clone(), row.key.clone(), row.value.clone()))
                    .collect::<Vec<_>>();
                if let Some(leak) = self.leaks(sub, cursor, &put) {
                    self.fail(format!("sealed_never_leaks: snapshot {leak}"));
                }
                if let Some(sub) = self.clients[client].subs.get_mut(&id) {
                    sub.replica.snap(cursor, rows);
                    sub.synced = true;
                }
                self.check_view(client, id);
            }
            ServerFrame::Diff { id, from, to, ops } => {
                self.stats.diffs += 1;
                let Some(sub) = self.clients[client].subs.get(&id) else {
                    return;
                };
                let leak = self.leaks(sub, to, &puts(&ops));
                let canonical = sub.key.canonical();
                if let Some(leak) = leak {
                    self.fail(format!("sealed_never_leaks: diff {leak}"));
                }
                let applied = self.clients[client].subs.get_mut(&id).map(|sub| {
                    sub.synced = true;
                    sub.replica.diff(from, to, ops)
                });
                if let Some(Err(gap)) = applied {
                    self.fail(format!(
                        "no_gap_no_dup: client {client} sub {id} {canonical} at {:?} got a diff from {} to {}",
                        gap.have.map(|cursor| cursor.pos),
                        from.pos,
                        to.pos
                    ));
                    return;
                }
                self.check_view(client, id);
            }
            ServerFrame::Ack(ack) => {
                self.stats.acks += 1;
                self.receive_ack(client, ack);
            }
            ServerFrame::Err { id, code, .. } => {
                let Some(id) = id else {
                    self.fail("wire: a server error named no subscription".to_owned());
                    return;
                };
                let Some(sub) = self.clients[client].subs.get_mut(&id) else {
                    return;
                };
                if code == ErrCode::Overloaded {
                    sub.synced = false;
                    let frame = ClientFrame::Sub {
                        id,
                        view: sub.key.clone(),
                        cursor: sub.replica.cursor(),
                    };
                    self.send(client, &frame);
                } else {
                    let expected = sub.access == Access::Denied && code == ErrCode::Denied;
                    sub.gone = true;
                    if !expected {
                        self.fail(format!(
                            "wire: client {client} sub {id} was refused with {code:?}"
                        ));
                    }
                }
            }
            ServerFrame::Eph { .. } => {}
        }
        self.reconcile(client);
    }

    fn receive_ack(&mut self, client: usize, ack: Ack) {
        let Some(name) = self.clients[client].unacked.remove(&ack.mid) else {
            self.fail(format!(
                "mutation_exactly_once: client {client} got an ack for mid {} it is not waiting on",
                ack.mid
            ));
            return;
        };
        let applied = self
            .applied
            .get(&(self.clients[client].name.clone(), ack.mid))
            .map(|applied| {
                applied
                    .iter()
                    .map(|applied| (applied.at, applied.rejected))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        match (name.as_str(), applied.as_slice(), &ack.result) {
            ("bogus", [], Err(_)) => {}
            (_, [(at, false)], Ok(cursor))
                if cursor.partition == at.partition && cursor.pos >= at.pos => {}
            (_, [(_, true)], Err(_)) => {}
            (_, applied, result) => {
                let applied = applied
                    .iter()
                    .map(|(at, rejected)| (at.pos, *rejected))
                    .collect::<Vec<_>>();
                self.fail(format!(
                    "mutation_exactly_once: client {client} mid {} ({name}) acked {result:?}, the log applied it at {applied:?} (pos, rejected)",
                    ack.mid
                ));
                return;
            }
        }
        if let Ok(at) = ack.result {
            let partitions = self.partitions();
            let behind = self.clients[client]
                .subs
                .iter()
                .filter(|(_, sub)| {
                    sub.synced && !sub.gone && sub.shape.partition(partitions) == at.partition
                })
                .find_map(|(id, sub)| {
                    let cursor = sub.replica.cursor()?;
                    (cursor.pos < at.pos).then_some((*id, cursor.pos))
                });
            if let Some((id, pos)) = behind {
                self.fail(format!(
                    "mutation_exactly_once: client {client} got the ack of mid {} at pos {} while its live sub {id} was at pos {pos}",
                    ack.mid, at.pos
                ));
                return;
            }
            let view = self.view(client, at.partition);
            if self.clients[client].optimistic.ack(&ack, view).is_some() {
                self.stats.reconciled += 1;
                if view.pos < at.pos {
                    self.fail(format!(
                        "mutation_exactly_once: client {client} dropped its optimistic mid {} at view pos {} before the ack's pos {}",
                        ack.mid, view.pos, at.pos
                    ));
                }
            }
        } else {
            let view = Cursor::new(0, 0, 0);
            if self.clients[client].optimistic.ack(&ack, view).is_some() {
                self.stats.reconciled += 1;
            }
        }
    }

    fn reconcile(&mut self, client: usize) {
        for part in 0..self.partitions() {
            let view = self.view(client, part);
            let landed = self.clients[client].optimistic.reached(view);
            self.stats.reconciled += landed.len() as u64;
        }
    }

    fn deliver_down(&mut self, socket: usize) -> bool {
        let Some(write) = self.sockets[socket].writes.pop_front() else {
            return false;
        };
        let client = self.sockets[socket].client;
        for text in write {
            self.client_receive(client, &text);
        }
        true
    }

    fn deliver_up(&mut self, socket: usize) -> bool {
        let Some(text) = self.sockets[socket].inbound.pop_front() else {
            return false;
        };
        self.server_receive(socket, &text);
        true
    }

    fn live_sockets(&self) -> Vec<usize> {
        (0..self.sockets.len())
            .filter(|socket| self.sockets[*socket].alive)
            .collect()
    }

    fn pick_socket(&mut self) -> Option<usize> {
        let live = self.live_sockets();
        (!live.is_empty()).then(|| live[self.rng.pick(live.len())])
    }

    fn kill(&mut self, node: usize) {
        if !self.nodes[node].alive || self.nodes.iter().filter(|node| node.alive).count() < 2 {
            return;
        }
        let sockets = (0..self.sockets.len())
            .filter(|socket| self.sockets[*socket].alive && self.sockets[*socket].node == node)
            .collect::<Vec<_>>();
        for socket in sockets {
            self.drop_socket(socket);
        }
        let name = self.nodes[node].name.clone();
        self.nodes[node] = self.fresh_node(name);
        self.nodes[node].alive = false;
        self.rebalance();
    }

    fn restart(&mut self, node: usize) {
        if self.nodes[node].alive {
            return;
        }
        self.nodes[node].alive = true;
        self.rebalance();
    }

    fn step(&mut self) {
        let clients = self.clients.len();
        let nodes = self.nodes.len();
        match self.rng.below(100) {
            0..15 => self.app_write(),
            15..25 => {
                let part = small(self.rng.below(u64::from(self.partitions())));
                self.apply_inputs(part, false);
            }
            25..45 => {
                let node = self.rng.pick(nodes);
                if self.nodes[node].alive {
                    let part = small(self.rng.below(u64::from(self.partitions())));
                    let limit =
                        usize::try_from(self.rng.range(1, 4)).expect("a limit fits in usize");
                    self.follow(node, part, limit);
                }
            }
            45..60 => {
                if let Some(socket) = self.pick_socket() {
                    self.deliver_down(socket);
                }
            }
            60..70 => {
                if let Some(socket) = self.pick_socket() {
                    self.deliver_up(socket);
                }
            }
            70..75 => {
                if let Some(socket) = self.pick_socket() {
                    if self.sockets[socket].writable {
                        self.sockets[socket].writable = false;
                    } else {
                        self.drain(socket);
                    }
                }
            }
            75..80 => {
                let client = self.rng.pick(clients);
                self.mutate(client);
            }
            80..83 => {
                let client = self.rng.pick(clients);
                let shape = if self.rng.chance(500_000) {
                    Shape::List(small(self.rng.below(u64::from(self.partitions()))))
                } else {
                    Shape::Thread(small(self.rng.below(u64::from(self.threads))))
                };
                self.subscribe(client, shape);
            }
            83..85 => {
                let client = self.rng.pick(clients);
                let ids = self.clients[client]
                    .subs
                    .keys()
                    .copied()
                    .collect::<Vec<_>>();
                if ids.len() > 1 {
                    let id = ids[self.rng.pick(ids.len())];
                    self.clients[client].subs.remove(&id);
                    self.send(client, &ClientFrame::Unsub { id });
                }
            }
            85..91 => {
                let client = self.rng.pick(clients);
                self.connect(client);
            }
            91..94 => {
                if let Some(socket) = self.pick_socket() {
                    self.drop_socket(socket);
                }
            }
            94..97 => {
                let node = self.rng.pick(nodes);
                self.kill(node);
            }
            _ => {
                let node = self.rng.pick(nodes);
                self.restart(node);
            }
        }
    }

    fn quiesce(&mut self) {
        for _ in 0..QUIESCE_ROUNDS {
            let mut progress = false;
            for client in 0..self.clients.len() {
                if self.clients[client].socket.is_none() {
                    self.connect(client);
                    progress = true;
                }
            }
            for socket in self.live_sockets() {
                if !self.sockets[socket].writable {
                    self.drain(socket);
                    progress = true;
                }
                while self.deliver_up(socket) {
                    progress = true;
                }
            }
            for part in 0..self.partitions() {
                progress |= self.apply_inputs(part, true);
            }
            for node in 0..self.nodes.len() {
                if !self.nodes[node].alive {
                    continue;
                }
                for part in 0..self.partitions() {
                    while self.follow(node, part, usize::MAX) {
                        progress = true;
                    }
                }
            }
            for socket in self.live_sockets() {
                while self.deliver_down(socket) {
                    progress = true;
                }
            }
            if !progress || self.wrong.is_some() {
                return;
            }
        }
        self.fail(format!(
            "liveness: still busy after {QUIESCE_ROUNDS} quiet rounds"
        ));
    }

    fn judge_end(&mut self) {
        for socket in self.live_sockets() {
            let node = self.sockets[socket].node;
            let stuck = self.sockets[socket].subs.iter().find_map(|(id, sub)| {
                let cursor = sub.ahead?;
                let head = self.nodes[node].instances.get(&sub.key).map(|instance| instance.live.head().pos);
                Some(format!(
                    "liveness: sub {id} of socket {socket} on node {} still waits at pos {} with the view at {head:?} and the node at {}",
                    self.nodes[node].name,
                    cursor.pos,
                    self.nodes[node].follow[cursor.partition as usize].pos
                ))
            });
            if let Some(message) = stuck {
                self.fail(message);
            }
        }
        for client in 0..self.clients.len() {
            let held = self.clients[client]
                .optimistic
                .rebase()
                .next()
                .map(|(mid, _)| mid);
            if let Some(mid) = held {
                let message = format!(
                    "mutation_exactly_once: client {client} still holds optimistic mid {mid} after the run settled"
                );
                self.fail(message);
            }
            for mid in intent::FIRST_MID..self.clients[client].optimistic.next() {
                let count = self
                    .applied
                    .get(&(self.clients[client].name.clone(), mid))
                    .map_or(0, Vec::len);
                if count > 1 {
                    self.fail(format!(
                        "mutation_exactly_once: client {client} mid {mid} was applied {count} times"
                    ));
                }
            }
            let ids = self.clients[client]
                .subs
                .keys()
                .copied()
                .collect::<Vec<_>>();
            for id in ids {
                let sub = &self.clients[client].subs[&id];
                if sub.gone {
                    continue;
                }
                let part = sub.shape.partition(self.partitions());
                let expected = reference(
                    self.state_at(part, self.head(part)),
                    sub.shape,
                    sub.access,
                    self.layout,
                );
                let cursor = sub.replica.cursor();
                if from_rows(sub.replica.rows()) != expected
                    || cursor.is_none_or(|cursor| cursor.pos > self.head(part))
                {
                    let message = format!(
                        "client_view_equals_fold: client {client} sub {id} {} settled at {:?} without the log's head {}",
                        sub.key.canonical(),
                        cursor.map(|cursor| cursor.pos),
                        self.head(part)
                    );
                    self.fail(message);
                }
            }
        }
        let mut wrong = None;
        for node in &self.nodes {
            for (key, instance) in &node.instances {
                let part = instance.shape.partition(self.partitions());
                let at = node.follow[part as usize].pos;
                let expected = reference(
                    self.state_at(part, at),
                    instance.shape,
                    Access::Full,
                    self.layout,
                );
                if from_rows(instance.live.table().rows()) != expected {
                    wrong.get_or_insert(format!(
                        "rebuild_equals_fold: node {} folded {key} incrementally to pos {at} unlike the fold from pos 0",
                        node.name
                    ));
                }
            }
        }
        if let Some(message) = wrong {
            self.fail(message);
        }
    }

    fn replayed(&self) -> (Value, Value, Value) {
        let partitions = self.partitions();
        let mut shapes = (0..partitions).map(Shape::List).collect::<Vec<_>>();
        shapes.extend((0..self.threads).map(Shape::Thread));
        let mut lives = shapes
            .iter()
            .map(|shape| {
                Live::new(
                    shape.key(),
                    ThreadsView::new(*shape, self.layout),
                    Cursor::new(shape.partition(partitions), 0, 0),
                    1,
                )
            })
            .collect::<Vec<_>>();
        let mut entries = self
            .log
            .iter()
            .enumerate()
            .flat_map(|(part, log)| log.iter().map(move |entry| (small(part as u64), entry)))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(_, entry)| entry.pos);
        let mut commits = Vec::new();
        for (part, entry) in entries {
            let at = Cursor::new(part, entry.epoch, entry.pos);
            let mut diffs = Vec::new();
            for (index, (shape, live)) in shapes.iter().zip(&mut lives).enumerate() {
                if shape.partition(partitions) != part {
                    continue;
                }
                let mut batch = live.begin();
                batch.fold(at, &entry.record);
                if let Some(done) = batch.finish()
                    && !done.diff().ops.is_empty()
                {
                    diffs.push(json!({"view": index, "ops": done.diff().ops}));
                }
            }
            commits.push(json!({"pos": entry.pos, "diffs": diffs}));
        }
        let finals = shapes
            .iter()
            .zip(&lives)
            .enumerate()
            .map(|(index, (shape, live))| {
                json!({"view": index, "pos": self.head(shape.partition(partitions)), "rows": live.table().rows()})
            })
            .collect::<Vec<_>>();
        let views = shapes.iter().map(|shape| shape.key()).collect::<Vec<_>>();
        (json!(views), json!(commits), json!(finals))
    }

    fn write_trace(&self) {
        let Some(dir) = TRACE_DIR.get() else {
            return;
        };
        let clients = self
            .clients
            .iter()
            .map(|client| {
                let finals = client
                    .subs
                    .iter()
                    .filter(|(_, sub)| !sub.gone)
                    .map(|(id, sub)| {
                        json!({"id": id, "view": sub.key, "cursor": sub.replica.cursor(), "rows": sub.replica.rows()})
                    })
                    .collect::<Vec<_>>();
                json!({"client": client.name, "reader": client.reader, "events": client.events, "final": finals})
            })
            .collect::<Vec<_>>();
        let (views, commits, finals) = self.replayed();
        let trace = json!({
            "version": 1,
            "source": "nanocode-sim",
            "scenario": "live",
            "seed": self.seed,
            "views": views,
            "commits": commits,
            "final": finals,
            "versioned": self.layout.versioned,
            "mutators": {
                "rename": "merge {title: args.title} into row thread/<args.thread>",
                "seal": "merge {sealed: args.on} into row thread/<args.thread>",
                "post": "none",
            },
            "clients": clients,
        });
        let path = dir.join(format!("live-{}.json", self.seed));
        let staged = dir.join(format!(".live-{}.json.tmp", self.seed));
        let written = std::fs::write(&staged, trace.to_string())
            .and_then(|()| std::fs::rename(&staged, &path));
        if let Err(error) = written {
            eprintln!("could not write {}: {error}", path.display());
        }
    }
}

pub fn simulate(seed: u64, mutation: LiveMutation) -> LiveOutcome {
    sim::mutate(mutation.fold());
    intent::sim::mutate(mutation.intent());
    let mut world = World::new(seed, mutation);
    let steps = world.rng.range(300, 1_500);
    for client in 0..world.clients.len() {
        world.connect(client);
    }
    for _ in 0..steps {
        world.step();
        if world.wrong.is_some() {
            break;
        }
    }
    if world.wrong.is_none() {
        world.quiesce();
    }
    if world.wrong.is_none() {
        world.judge_end();
    }
    world.write_trace();
    let shape = format!(
        "partitions={} threads={} versioned={} nodes={} clients={} ring={} bound={} steps={steps}",
        world.partitions(),
        world.threads,
        world.layout.versioned,
        world.nodes.len(),
        world.clients.len(),
        world.ring,
        world.bound
    );
    LiveOutcome {
        digest: world.digest,
        shape,
        stats: world.stats,
        wrong: world.wrong,
    }
}

pub fn judge(
    seed: u64,
    mutation: LiveMutation,
    twice: bool,
    verbose: bool,
    totals: &mut LiveStats,
) -> Option<String> {
    let first = simulate(seed, mutation);
    totals.add(&first.stats);
    if verbose {
        println!(
            "seed {seed} digest {:016x} {} {:?}",
            first.digest, first.shape, first.stats
        );
    }
    if first.wrong.is_some() || !twice {
        return first
            .wrong
            .map(|message| format!("{} :: {message}", first.shape));
    }
    let second = simulate(seed, mutation);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: trace digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: LiveMutation) -> String {
    let mut command = format!("nanocode-sim --scenario live --seed {seed}");
    if mutation != LiveMutation::None {
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
    mutation: LiveMutation,
    totals: &LiveStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=live mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        LiveMutation::None if failed == 0 => ExitCode::SUCCESS,
        LiveMutation::None => ExitCode::FAILURE,
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
