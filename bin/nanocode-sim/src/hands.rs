use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    process::ExitCode,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use nanocode::turnlog::{Compaction, LogHost, OneLog, Policy, Rider, Write};
use nanocode_code_mode::contract::DEFAULT_TOOL_OUTPUT_TOKENS;
use nanocode_code_mode::embedded::{CodeModeHostError, HandCalls, HostFuture};
use nanocode_code_mode::remote::{
    Carried, CodeModeExecutors, NOT_RECEIVED, PEER_CLOSED, RemoteBarrier, RemoteCatalog,
    RemoteCodeModeHost, RemoteContext, RemoteDelivery, RemoteFrame, RemoteLink, RemoteOperation,
    RemoteReply, RemoteTransport, output_from_wire, output_to_wire,
};
use nanocode_code_mode::{ToolContext, ToolOutput, ToolOutputBody};
use nanocode_durability_core::{
    OwnerId, Settle, StateCommit, StateStore, StoreError, StoreFuture, SummaryUpdate,
};
use nanocode_http::StatusCode;
use nanocode_sync::FutureExt;
use nanocode_sync::{Atom, Owned, mpsc};
use nanocode_tool_runtime::remote::accept::{
    Acceptor, AcceptorOptions, AcceptorRoute, HandRequests, PendingRequest, Refusal,
};
use serde_json::{Value, json};

use crate::{
    executor::{Clock, Ended, Executor, Task},
    rng::{FNV_OFFSET, Rng, fnv},
};

const KEY: &str = "hand";
const SESSION: &str = "session";
const TURN: &str = "turn";
const SCOPE: &str = "hands";
const SENT: &str = "hand_request_sent";
const REPLIED: &str = "hand_request_replied";
const ADMITTED: &str = "admitted";
const ANSWERED: &str = "answered";
const SNAPSHOT_RATIO: usize = 2;
const CALLS_MAX: u64 = 5;
const START_MS_MAX: u64 = 300;
const RUN_MS_MAX: u64 = 400;
const COMMIT_MS_MAX: u64 = 20;
const CRASHES_MAX: u64 = 3;
const DROPS_MAX: u64 = 3;
const EVENT_MS_MAX: u64 = 1_500;
const RESTART_PPM: u64 = 300_000;
const REDIAL_MS_MAX: u64 = 40;
const WRITE_MS_MAX: u64 = 3;
const STALL_EVERY: u64 = 3;
const CANCELS_MAX: u64 = 6;
const TICK: Duration = Duration::from_millis(1);
const GRACE: Duration = Duration::from_secs(5);
const SETTLE_HORIZON_MS: u64 = 60_000;
const FRAME_BYTES_MAX: usize = 1 << 20;
const RUNTIMES_MAX: usize = 4;
const ATTACHES_QUEUED_MAX: usize = 64;
const MS: u64 = 1_000_000;
const FLEET_MUTATED: usize = 3;
const CELL_PPM: u64 = 500_000;
const CELL: &str = "cell";
const FLEET_COMMIT_MS_MAX: u64 = 200;
const REOPEN_MS_MAX: u64 = START_MS_MAX + RUN_MS_MAX;
/// Share of seeds whose host crashes land next to a call's start, reply or commit instead of
/// anywhere in EVENT_MS_MAX: the windows between a durable step, the stored request and the
/// stored reply are a few milliseconds wide, so uniform crashes almost never hit them.
const AIM_PPM: u64 = 800_000;
const AIM_US_MAX: u64 = 8_000;
/// Share of aimed crashes that land inside a window the probe saw (a call's wait for its hand,
/// or a stored reply's wait for its commit), when it saw one.
const WAIT_PPM: u64 = 600_000;
/// Share of aimed crashes that land between a resume naming a reply the brain already read and
/// that call's commit, when the probe saw one: only a crash there makes the next process ask
/// the hand for a reply it was told to keep.
const RESUMED_PPM: u64 = 700_000;
/// Share of the remaining aimed crashes in a fleet that land while an admitted call's request
/// waits for its hand, when the probe saw one: only a crash there makes recovery ask a hand
/// that never restarted about a call it was never sent, which must come back as not
/// dispatched. A single hand keeps its old aim, which its own checks were tuned against.
const UNSENT_PPM: u64 = 700_000;
/// Share of aimed seeds that also move one of a hand's socket drops next to a call: dispatch to
/// a connected hand takes almost no simulated time, so a call only waits for its hand (the
/// window where recovery must answer that nothing was sent) while that hand redials.
const DROP_PPM: u64 = 850_000;
/// Share of moved drops that land just after a stored reply instead of just before a call.
const REPLY_PPM: u64 = 500_000;
const DROP_US_MAX: u64 = 2_000;
/// Separates an execution's label from the send count in a reply's value.
const SEND: &str = " send ";
/// Share of seeds whose hands drop retained replies the store holds without restarting.
const EVICT_PPM: u64 = 500_000;
const UNSENT: [&str; 3] = [
    "not dispatched",
    "never dispatched",
    "nothing was dispatched",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandsMutation {
    None,
    AckBeforeStore,
    SkipRecord,
    ForgetPending,
    CrossReplies,
    MangleRuntime,
    CarryDropsLog,
    CarryDropsRequest,
    DispatchUnsettled,
    RideDropsReply,
    FleetUnbarriered,
    FleetCrossHands,
    FleetRecoverNone,
    FleetSwallowUnsent,
    FleetRedispatch,
    ResumeWaitingOnly,
    ReopenOrphaned,
    ReopenUnprobed,
    CancelLate,
    ReplyOverwrites,
}

impl HandsMutation {
    const ALL: [Self; 20] = [
        Self::None,
        Self::AckBeforeStore,
        Self::SkipRecord,
        Self::ForgetPending,
        Self::CrossReplies,
        Self::MangleRuntime,
        Self::CarryDropsLog,
        Self::CarryDropsRequest,
        Self::DispatchUnsettled,
        Self::RideDropsReply,
        Self::FleetUnbarriered,
        Self::FleetCrossHands,
        Self::FleetRecoverNone,
        Self::FleetSwallowUnsent,
        Self::FleetRedispatch,
        Self::ResumeWaitingOnly,
        Self::ReopenOrphaned,
        Self::ReopenUnprobed,
        Self::CancelLate,
        Self::ReplyOverwrites,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::AckBeforeStore => "ack_before_store",
            Self::SkipRecord => "skip_record",
            Self::ForgetPending => "forget_pending",
            Self::CrossReplies => "cross_replies",
            Self::MangleRuntime => "mangle_runtime",
            Self::CarryDropsLog => "carry_drops_log",
            Self::CarryDropsRequest => "carry_drops_request",
            Self::DispatchUnsettled => "dispatch_unsettled",
            Self::RideDropsReply => "ride_drops_reply",
            Self::FleetUnbarriered => "fleet_unbarriered",
            Self::FleetCrossHands => "fleet_cross_hands",
            Self::FleetRecoverNone => "fleet_recover_none",
            Self::FleetSwallowUnsent => "fleet_swallow_unsent",
            Self::FleetRedispatch => "fleet_redispatch",
            Self::ResumeWaitingOnly => "resume_waiting_only",
            Self::ReopenOrphaned => "reopen_orphaned",
            Self::ReopenUnprobed => "reopen_unprobed",
            Self::CancelLate => "cancel_late",
            Self::ReplyOverwrites => "reply_overwrites",
        }
    }

    const fn options(self, options: HandsOptions) -> HandsOptions {
        match self {
            Self::ReopenOrphaned | Self::ReopenUnprobed => HandsOptions {
                reopen: true,
                ..options
            },
            Self::CancelLate => HandsOptions {
                fleet: 1,
                cancel: true,
                ..options
            },
            _ => HandsOptions {
                fleet: self.fleet(options.fleet),
                ..options
            },
        }
    }

    const fn fleet(self, fleet: usize) -> usize {
        match self {
            Self::FleetUnbarriered
            | Self::FleetCrossHands
            | Self::FleetRecoverNone
            | Self::FleetSwallowUnsent
            | Self::FleetRedispatch
                if fleet == 1 =>
            {
                FLEET_MUTATED
            }
            _ => fleet,
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

#[derive(Clone, Copy, Debug)]
pub struct HandsOptions {
    pub fleet: usize,
    pub reopen: bool,
    pub cancel: bool,
}

#[derive(Debug, Default)]
pub struct HandsStats {
    pub seeds: u64,
    pub crashes: u64,
    pub restarts: u64,
    pub drops: u64,
    pub calls: u64,
    pub recovered: u64,
    pub unknown: u64,
    pub folded: u64,
    pub rode: u64,
    pub replayed: u64,
    pub retained: u64,
    pub reopened: u64,
    pub orphaned: u64,
    pub stalled: u64,
    pub stopped: u64,
}

impl HandsStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.crashes += other.crashes;
        self.restarts += other.restarts;
        self.drops += other.drops;
        self.calls += other.calls;
        self.recovered += other.recovered;
        self.unknown += other.unknown;
        self.folded += other.folded;
        self.rode += other.rode;
        self.replayed += other.replayed;
        self.retained += other.retained;
        self.reopened += other.reopened;
        self.orphaned += other.orphaned;
        self.stalled += other.stalled;
        self.stopped += other.stopped;
    }
}

struct Script {
    starts: Vec<u64>,
    runs: Vec<u64>,
    commits: Vec<u64>,
    crashes: Vec<u64>,
    hands: Vec<usize>,
    cells: Vec<bool>,
    schedules: Vec<Schedule>,
    reopen: Option<u64>,
    cancels: Vec<u64>,
    evicts: bool,
}

struct Schedule {
    restarts: Vec<u64>,
    drops: Vec<u64>,
}

fn times(rng: &mut Rng, count: u64) -> Vec<u64> {
    let mut times: Vec<u64> = (0..count)
        .map(|_| rng.range(1, EVENT_MS_MAX) * MS)
        .collect();
    times.sort_unstable();
    times
}

impl Script {
    fn of(seed: u64, options: HandsOptions) -> Self {
        let mut script = Self::drawn(seed, options.fleet);
        script.evicts = Rng::derive(seed, "hands evict script").chance(EVICT_PPM);
        if options.reopen {
            let mut rng = Rng::derive(seed, "hands reopen script");
            script.reopen = Some(rng.range(1, REOPEN_MS_MAX) * MS);
        }
        if options.cancel && options.fleet <= 1 {
            let mut rng = Rng::derive(seed, "hands cancel script");
            let cancels = rng.range(1, CANCELS_MAX);
            script.cancels = (0..cancels)
                .map(|_| rng.range(0, START_MS_MAX) * MS)
                .collect();
            script.cancels.sort_unstable();
        }
        script
    }

    /// Moves the drawn crashes to within AIM_US_MAX of moments a crash-free probe of the same
    /// seed reached (a call admitted, its request run on a hand, its reply, its commit): the
    /// windows between a durable start, the stored request, the stored reply and the commit
    /// are a few milliseconds wide, so crashes drawn across EVENT_MS_MAX almost never hit one.
    /// Most crashes land instead inside a window the probe saw (between a call's durable
    /// admission and its request reaching a hand, or between a stored reply and its commit).
    fn aim(&mut self, rng: &mut Rng, marks: &Marks) {
        if marks.around.is_empty() {
            return;
        }
        let mut crashes: Vec<u64> = self
            .crashes
            .iter()
            .map(|_| {
                if !marks.resumed.is_empty() && rng.chance(RESUMED_PPM) {
                    let (from, to) = marks.resumed[rng.pick(marks.resumed.len())];
                    return rng.range(from + 1, to).max(MS);
                }
                if !marks.unsent.is_empty() && rng.chance(UNSENT_PPM) {
                    let (from, to) = marks.unsent[rng.pick(marks.unsent.len())];
                    return rng.range(from + 1, to).max(MS);
                }
                if !marks.waits.is_empty() && rng.chance(WAIT_PPM) {
                    let (from, to) = marks.waits[rng.pick(marks.waits.len())];
                    return rng.range(from + 1, to).max(MS);
                }
                let mark = marks.around[rng.pick(marks.around.len())];
                let jitter = rng.range(0, 2 * AIM_US_MAX) * 1_000;
                (mark + jitter).saturating_sub(AIM_US_MAX * 1_000).max(MS)
            })
            .collect();
        crashes.sort_unstable();
        crashes.dedup();
        self.crashes = crashes;
    }

    /// Moves one of a hand's socket drops (or adds one when it has none) to the instant given.
    fn drop_at(&mut self, rng: &mut Rng, slot: usize, at: u64) {
        let drops = &mut self.schedules[slot].drops;
        let at = at.max(MS);
        if drops.is_empty() {
            drops.push(at);
        } else {
            let index = rng.pick(drops.len());
            drops[index] = at;
        }
        drops.sort_unstable();
        drops.dedup();
    }

    /// Moves a drop next to a moment the crash-free probe reached: just before a call's
    /// admission, so its hand is away when the call dispatches, or just after a reply the
    /// hand sent, so it redials while that reply waits for its commit. Returns whether it moved.
    fn aim_drop(&mut self, rng: &mut Rng, marks: &Marks) -> bool {
        let jitter = rng.range(0, DROP_US_MAX + 1) * 1_000;
        if !marks.replies.is_empty() && rng.chance(REPLY_PPM) {
            let (slot, at, _) = marks.replies[rng.pick(marks.replies.len())];
            self.drop_at(rng, slot, at + jitter);
            return true;
        }
        if marks.admissions.is_empty() {
            return false;
        }
        let (slot, at) = marks.admissions[rng.pick(marks.admissions.len())];
        self.drop_at(rng, slot, at.saturating_sub(jitter));
        true
    }

    fn drawn(seed: u64, fleet: usize) -> Self {
        if fleet > 1 {
            return Self::fleet(seed, fleet);
        }
        let mut rng = Rng::derive(seed, "hands script");
        let calls = rng.range(1, CALLS_MAX);
        let starts: Vec<u64> = (0..calls)
            .map(|_| rng.range(0, START_MS_MAX) * MS)
            .collect();
        let runs = (0..calls).map(|_| rng.range(1, RUN_MS_MAX) * MS).collect();
        let commits = (0..calls)
            .map(|_| rng.range(0, COMMIT_MS_MAX) * MS)
            .collect();
        let crashes = rng.range(0, CRASHES_MAX);
        let crashes = times(&mut rng, crashes);
        let restarts = u64::from(rng.chance(RESTART_PPM));
        let restarts = times(&mut rng, restarts);
        let drops = rng.range(0, DROPS_MAX);
        let drops = times(&mut rng, drops);
        let hands = vec![0; starts.len()];
        let cells = vec![false; starts.len()];
        Self {
            starts,
            runs,
            commits,
            crashes,
            hands,
            cells,
            schedules: vec![Schedule { restarts, drops }],
            reopen: None,
            cancels: Vec::new(),
            evicts: false,
        }
    }

    fn fleet(seed: u64, fleet: usize) -> Self {
        let mut rng = Rng::derive(seed, "hands fleet script");
        let width = u64::try_from(fleet).expect("a fleet size fits in 64 bits");
        let calls = rng.range(width, CALLS_MAX * width);
        let starts = (0..calls)
            .map(|_| rng.range(0, START_MS_MAX) * MS)
            .collect();
        let runs = (0..calls).map(|_| rng.range(1, RUN_MS_MAX) * MS).collect();
        let commits = (0..calls)
            .map(|_| rng.range(0, FLEET_COMMIT_MS_MAX) * MS)
            .collect();
        let cells: Vec<bool> = (0..calls).map(|_| rng.chance(CELL_PPM)).collect();
        let hands = cells
            .iter()
            .map(|cell| {
                if *cell {
                    1 + rng.pick(fleet - 1)
                } else {
                    rng.pick(fleet)
                }
            })
            .collect();
        let crashes = rng.range(0, CRASHES_MAX);
        let crashes = times(&mut rng, crashes);
        let schedules = (0..fleet)
            .map(|_| {
                let restarts = u64::from(rng.chance(RESTART_PPM));
                let restarts = times(&mut rng, restarts);
                let drops = rng.range(0, DROPS_MAX);
                let drops = times(&mut rng, drops);
                Schedule { restarts, drops }
            })
            .collect();
        Self {
            starts,
            runs,
            commits,
            crashes,
            hands,
            cells,
            schedules,
            reopen: None,
            cancels: Vec::new(),
            evicts: false,
        }
    }

    fn describe(&self) -> String {
        let restarts: usize = self.schedules.iter().map(|at| at.restarts.len()).sum();
        let drops: usize = self.schedules.iter().map(|at| at.drops.len()).sum();
        let shape = format!(
            "calls={} crashes={} restarts={restarts} drops={drops}",
            self.starts.len(),
            self.crashes.len(),
        );
        let shape = match self.reopen {
            Some(at) => format!("{shape} reopen={}", at / MS),
            None => shape,
        };
        let shape = if self.cancels.is_empty() {
            shape
        } else {
            format!("{shape} cancels={}", self.cancels.len())
        };
        match self.schedules.len() {
            1 => shape,
            fleet => format!(
                "{shape} hands={fleet} cells={}",
                self.cells.iter().filter(|cell| **cell).count()
            ),
        }
    }
}

#[derive(Clone)]
struct Row {
    id: u64,
    key: String,
    runtime: String,
    turn_id: Option<String>,
    reply: Option<RemoteReply>,
}

struct Store {
    rows: Atom<BTreeMap<(String, String), Row>>,
    writes: AtomicU64,
    abandoned: Atom<BTreeSet<(String, u64)>>,
    orphaned: AtomicU64,
    stalling: AtomicBool,
    stalled: AtomicU64,
    /// Every stored reply a later reply replaced: a stored reply is final.
    overwrites: Atom<Vec<String>>,
    seed: u64,
    mutation: HandsMutation,
}

impl Store {
    async fn write(&self) {
        let count = self.writes.fetch_add(1, Ordering::Relaxed);
        let delay = fnv(self.seed, &count.to_le_bytes()) % WRITE_MS_MAX + 1;
        nanocode_platform::sleep(Duration::from_millis(delay)).await;
    }

    fn insert(&self, request: &PendingRequest) {
        if self.mutation == HandsMutation::SkipRecord {
            return;
        }
        let runtime = if self.mutation == HandsMutation::MangleRuntime {
            String::new()
        } else {
            request.runtime.clone()
        };
        let row = Row {
            id: request.id,
            key: request.key.clone(),
            runtime,
            turn_id: request.turn_id.clone(),
            reply: None,
        };
        let at = (request.session_id.clone(), request.call_id.clone());
        self.rows.update(|rows| {
            rows.insert(at.clone(), row.clone());
        });
    }

    async fn stall(&self, request: &PendingRequest) {
        if self.stalling.load(Ordering::Relaxed)
            && fnv(self.seed, request.call_id.as_bytes()).is_multiple_of(STALL_EVERY)
        {
            self.stalled.fetch_add(1, Ordering::Relaxed);
            while self.stalling.load(Ordering::Relaxed) {
                nanocode_platform::sleep(TICK).await;
            }
        }
    }

    fn abandon(&self) {
        let unanswered: Vec<(String, u64)> = self.rows.read(|rows| {
            rows.values()
                .filter(|row| row.reply.is_none())
                .map(|row| (row.key.clone(), row.id))
                .collect()
        });
        self.abandoned
            .update(|abandoned| abandoned.extend(unanswered.iter().cloned()));
    }

    fn orphan(&self, key: &str, id: u64) -> bool {
        self.abandoned
            .read(|abandoned| abandoned.contains(&(key.to_owned(), id)))
    }

    fn recorded(&self, session_id: &str, call_id: &str) -> bool {
        self.rows
            .read(|rows| rows.contains_key(&(session_id.to_owned(), call_id.to_owned())))
    }

    fn answer(&self, key: &str, runtime: &str, id: u64, reply: &RemoteReply) {
        let cross = self.mutation == HandsMutation::CrossReplies;
        let overwrite = self.mutation == HandsMutation::ReplyOverwrites;
        let runtime = if self.mutation == HandsMutation::MangleRuntime {
            ""
        } else {
            runtime
        };
        let mut replaced = Vec::new();
        self.rows.update(|rows| {
            for row in rows.values_mut() {
                let same = row.key == key && row.runtime == runtime;
                let open = row.reply.is_none() || overwrite;
                if same && open && (row.id == id || cross) {
                    if let Some(stored) = row.reply.as_ref().filter(|stored| *stored != reply) {
                        replaced.push(format!(
                            "request {} replaced its stored reply {stored:?} with {reply:?}",
                            row.id
                        ));
                    }
                    row.reply = Some(reply.clone());
                }
            }
        });
        if !replaced.is_empty() {
            self.overwrites.update(|overwrites| overwrites.extend(replaced.iter().cloned()));
        }
    }

    fn answered(&self, key: &str, id: u64) -> bool {
        self.rows.read(|rows| {
            rows.values()
                .any(|row| row.key == key && row.id == id && row.reply.is_some())
        })
    }

    fn unanswered(&self, key: &str, id: u64) -> bool {
        self.rows.read(|rows| {
            rows.values()
                .any(|row| row.key == key && row.id == id && row.reply.is_none())
        })
    }
}

impl HandRequests for Store {
    fn sent<'a>(&'a self, request: &'a PendingRequest) -> HostFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.write().await;
            self.insert(request);
            Ok(())
        })
    }

    fn sent_after<'a>(
        &'a self,
        request: &'a PendingRequest,
        barrier: &'a dyn RemoteBarrier,
    ) -> HostFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let turn =
                serde_json::to_string(&request.turn_id).map_err(|error| error.to_string())?;
            let params = vec![
                request.session_id.clone(),
                request.call_id.clone(),
                request.id.to_string(),
                request.key.clone(),
                request.runtime.clone(),
                turn,
            ];
            let carried = Carried {
                host: std::ptr::from_ref(self) as usize,
                statements: vec![(SENT, params)],
            };
            if !barrier.carry(carried).await? {
                self.sent(request).await?;
            }
            self.stall(request).await;
            Ok(())
        })
    }

    fn replied<'a>(
        &'a self,
        key: &'a str,
        runtime: &'a str,
        id: u64,
        reply: &'a RemoteReply,
    ) -> HostFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if self.mutation == HandsMutation::AckBeforeStore {
                return Ok(());
            }
            self.write().await;
            if self.orphan(key, id) {
                if self.mutation == HandsMutation::ReopenOrphaned {
                    return Ok(());
                }
                self.orphaned.fetch_add(1, Ordering::Relaxed);
            }
            self.answer(key, runtime, id, reply);
            Ok(())
        })
    }

    fn rider(&self, key: &str, runtime: &str, id: u64, reply: &RemoteReply) -> Option<Carried> {
        if self.mutation == HandsMutation::AckBeforeStore {
            return None;
        }
        let reply = serde_json::to_string(reply).ok()?;
        Some(Carried {
            host: std::ptr::from_ref(self) as usize,
            statements: vec![(
                REPLIED,
                vec![key.to_owned(), runtime.to_owned(), id.to_string(), reply],
            )],
        })
    }

    fn pending<'a>(
        &'a self,
        key: &'a str,
        runtime: &'a str,
    ) -> HostFuture<'a, Result<Vec<u64>, String>> {
        Box::pin(async move {
            self.write().await;
            if self.mutation == HandsMutation::ForgetPending {
                return Ok(Vec::new());
            }
            Ok(self.rows.read(|rows| {
                rows.values()
                    .filter(|row| row.key == key && row.runtime == runtime && row.reply.is_none())
                    .map(|row| row.id)
                    .collect()
            }))
        })
    }

    fn find<'a>(
        &'a self,
        session_id: &'a str,
        call_id: &'a str,
    ) -> HostFuture<'a, Result<Option<PendingRequest>, String>> {
        Box::pin(async move {
            let at = (session_id.to_owned(), call_id.to_owned());
            let found = self.rows.read(|rows| {
                rows.get(&at).map(|row| PendingRequest {
                    id: row.id,
                    key: row.key.clone(),
                    runtime: row.runtime.clone(),
                    session_id: session_id.to_owned(),
                    call_id: call_id.to_owned(),
                    turn_id: row.turn_id.clone(),
                    reply: row.reply.clone(),
                })
            });
            self.write().await;
            Ok(found)
        })
    }

    fn settle<'a>(&'a self, session_id: &'a str) -> HostFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.write().await;
            self.rows
                .update(|rows| rows.retain(|(session, _), _| session != session_id));
            Ok(())
        })
    }
}

struct StepHost {
    rows: Atom<BTreeMap<u64, String>>,
    store: Arc<Store>,
    folded: AtomicU64,
    rode: AtomicU64,
}

impl StepHost {
    fn applied(&self, rows: &[(u64, String)], compaction: &Compaction<'_>) {
        self.rows.update(|stored| {
            stored.extend(rows.iter().chain(compaction.archived).cloned());
            for range in std::iter::once(&compaction.dropped).chain(compaction.superseded) {
                for seq in range.clone() {
                    stored.remove(&seq);
                }
            }
        });
    }

    fn carried(&self, statements: &[nanocode_durability_core::SqlStatement<'_>]) {
        for statement in statements {
            if statement.sql == REPLIED {
                let [key, runtime, id, reply] = statement.params[..] else {
                    panic!("the simulated replied statement has four parameters");
                };
                self.rode.fetch_add(1, Ordering::Relaxed);
                if self.store.mutation != HandsMutation::RideDropsReply {
                    let reply = serde_json::from_str(reply).expect("the reply is JSON");
                    let id = id.parse().expect("request ids are numbers");
                    self.store.answer(key, runtime, id, &reply);
                }
                continue;
            }
            let [session_id, call_id, id, key, runtime, turn] = statement.params[..] else {
                panic!("the simulated ledger statement has six parameters");
            };
            assert_eq!(
                statement.sql, SENT,
                "only the ledger's sent statement rides"
            );
            self.store.insert(&PendingRequest {
                id: id.parse().expect("request ids are numbers"),
                key: key.to_owned(),
                runtime: runtime.to_owned(),
                session_id: session_id.to_owned(),
                call_id: call_id.to_owned(),
                turn_id: serde_json::from_str(turn).expect("the turn id is JSON"),
                reply: None,
            });
        }
    }
}

impl LogHost for StepHost {
    fn scan<'a>(
        &'a self,
        _scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        let rows = self.rows.read(|rows| {
            rows.range(from..to)
                .map(|(seq, body)| (*seq, body.clone()))
                .collect()
        });
        Box::pin(async move { Ok(rows) })
    }

    fn load<'a>(
        &'a self,
        _scope: &'a str,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        let rows = self.rows.read(|rows| {
            let start = rows
                .iter()
                .rev()
                .find(|(_, body)| body.starts_with('s'))
                .map_or(0, |(seq, _)| *seq);
            rows.range(start..)
                .map(|(seq, body)| (*seq, body.clone()))
                .collect()
        });
        Box::pin(async move { Ok(rows) })
    }

    fn append<'a>(
        &'a self,
        _scope: &'a str,
        rows: &'a [(u64, String)],
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.store.write().await;
            self.applied(
                rows,
                &Compaction {
                    archived: &[],
                    dropped: 0..0,
                    superseded: &[],
                },
            );
            Ok(())
        })
    }

    fn append_compacting<'a>(
        &'a self,
        _scope: &'a str,
        rows: &'a [(u64, String)],
        compaction: Compaction<'a>,
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.store.write().await;
            self.applied(rows, &compaction);
            Ok(())
        })
    }

    fn carry<'a>(
        &'a self,
        _scope: &'a str,
        write: Write<'a>,
        rider: Rider<'a>,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        if rider.host != Arc::as_ptr(&self.store) as usize {
            return None;
        }
        let Write::Append { rows, compaction } = write else {
            return None;
        };
        Some(Box::pin(async move {
            self.store.write().await;
            if self.store.mutation != HandsMutation::CarryDropsLog {
                self.applied(rows, &compaction);
            }
            if self.store.mutation != HandsMutation::CarryDropsRequest {
                self.carried(rider.statements);
            }
            self.folded.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }))
    }
}

fn policy() -> Policy {
    Policy {
        snapshot_ratio: SNAPSHOT_RATIO,
        ..Policy::default()
    }
}

fn step(index: usize) -> String {
    format!("step-{index}")
}

fn durable_step(host: &Arc<StepHost>, index: usize) -> bool {
    let host = Arc::clone(host) as Arc<dyn LogHost>;
    let read = async move {
        let log = OneLog::open(host, SCOPE, policy())
            .await
            .expect("the simulated turn log opens");
        let mut store = log.store();
        store
            .peek(&step(index))
            .await
            .expect("the simulated turn log reads")
            .payload
            .is_some_and(|payload| payload == ADMITTED || payload == ANSWERED)
    };
    read.now_or_never()
        .expect("the simulated turn log answers at once")
}

struct SimLink {
    incoming: Owned<mpsc::UnboundedReceiver<String>>,
    outgoing: mpsc::UnboundedSender<String>,
    resumes: String,
    read: Arc<Atom<BTreeSet<u64>>>,
}

impl RemoteLink for SimLink {
    fn send(
        &self,
        frames: Vec<RemoteFrame>,
        delivery: RemoteDelivery,
    ) -> HostFuture<'_, Result<(), CodeModeHostError>> {
        self.send_encoded(
            frames.into_iter().map(|frame| (frame, None)).collect(),
            delivery,
        )
    }

    fn send_encoded(
        &self,
        frames: Vec<(RemoteFrame, Option<String>)>,
        _: RemoteDelivery,
    ) -> HostFuture<'_, Result<(), CodeModeHostError>> {
        Box::pin(async move {
            for (frame, text) in frames {
                let text = match text {
                    Some(text) => text,
                    None => frame.encode(FRAME_BYTES_MAX)?,
                };
                self.outgoing
                    .send(text)
                    .map_err(|_| CodeModeHostError::new("socket write failed"))?;
            }
            Ok(())
        })
    }

    fn receive(&self) -> HostFuture<'_, Result<Vec<RemoteFrame>, CodeModeHostError>> {
        Box::pin(async move {
            match self.incoming.acquire().await.recv().await {
                Some(text) => {
                    let frame = RemoteFrame::decode(&text, FRAME_BYTES_MAX)?;
                    if let RemoteFrame::Reply { id, .. } = &frame {
                        self.read.update(|read| {
                            read.insert(*id);
                        });
                    }
                    Ok(vec![frame])
                }
                None => Err(CodeModeHostError::new(PEER_CLOSED)),
            }
        })
    }

    fn close<'a>(&'a self, _: &'a str) -> HostFuture<'a, ()> {
        Box::pin(async {})
    }

    fn max_frame_bytes(&self) -> Option<usize> {
        Some(FRAME_BYTES_MAX)
    }

    fn resumes(&self) -> Option<String> {
        Some(self.resumes.clone())
    }
}

#[derive(Clone, Debug)]
enum Outcome {
    Value(Value),
    NotDispatched,
    Unknown,
}

#[derive(Clone, Debug)]
enum CallState {
    Fresh,
    Admitted,
    Committed(Outcome),
}

struct Call {
    state: CallState,
    exposed: bool,
    hand: usize,
    cell: bool,
}

#[derive(Default)]
struct Hand {
    runtime: u64,
    running: BTreeMap<u64, (usize, u64, String)>,
    /// Each finished request's execution label and how many times the hand sent its reply:
    /// every send carries that count, so a resend differs from the reply it repeats.
    retained: BTreeMap<u64, (String, u64)>,
    executions: BTreeMap<usize, Vec<Value>>,
    callbacks: BTreeMap<u64, usize>,
    held: Vec<u64>,
    asked: Vec<usize>,
    next_callback: u64,
    seen: BTreeSet<u64>,
}

struct Slot {
    key: String,
    hand: RefCell<Hand>,
    restarts: RefCell<Vec<u64>>,
    drops: RefCell<Vec<u64>>,
    read: Arc<Atom<BTreeSet<u64>>>,
}

struct Waiting<'a> {
    world: &'a World,
    token: Option<u64>,
}

impl<'a> Waiting<'a> {
    fn enter(world: &'a World, acceptor: &Acceptor, index: usize) -> Self {
        if world.script.cancels.is_empty() || acceptor.connected(KEY) {
            return Self { world, token: None };
        }
        let token = world.tokens.get();
        world.tokens.set(token + 1);
        world
            .waiting
            .borrow_mut()
            .insert(token, (index, world.dials.get()));
        Self {
            world,
            token: Some(token),
        }
    }

    fn stopped(&self) -> bool {
        self.token
            .is_some_and(|token| self.world.stopped.borrow().contains(&token))
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token {
            self.world.waiting.borrow_mut().remove(&token);
        }
    }
}

enum Via<'a> {
    Route(&'a AcceptorRoute, &'a Arc<dyn RemoteBarrier>, &'a Acceptor),
    Hands(&'a Arc<dyn HandCalls>),
}

struct World {
    reopen: Cell<Option<u64>>,
    dials: Cell<u64>,
    waiting: RefCell<BTreeMap<u64, (usize, u64)>>,
    stopped: RefCell<BTreeSet<u64>>,
    tokens: Cell<u64>,
    script: Script,
    store: Arc<Store>,
    log: Arc<StepHost>,
    slots: Vec<Slot>,
    calls: RefCell<Vec<Call>>,
    rng: RefCell<Rng>,
    clock: Arc<Clock>,
    trace: RefCell<Vec<String>>,
    marks: RefCell<Marks>,
    wrong: RefCell<Vec<String>>,
    stats: RefCell<HandsStats>,
}

impl World {
    fn note(&self, line: String) {
        self.marks.borrow_mut().around.push(self.now());
        self.trace.borrow_mut().push(line);
    }

    fn fail(&self, invariant: &str, line: String) {
        self.wrong.borrow_mut().push(format!("{invariant}: {line}"));
    }

    fn done(&self) -> bool {
        self.calls
            .borrow()
            .iter()
            .all(|call| matches!(call.state, CallState::Committed(_)))
    }

    fn now(&self) -> u64 {
        self.clock.now_ns()
    }

    const fn fleet(&self) -> bool {
        self.slots.len() > 1
    }

    fn due(events: &RefCell<Vec<u64>>, now: u64) -> bool {
        let mut events = events.borrow_mut();
        if events.first().is_some_and(|at| *at <= now) {
            events.remove(0);
            return true;
        }
        false
    }

    fn restart_due(&self, at: usize) -> bool {
        let slot = &self.slots[at];
        if !Self::due(&slot.restarts, self.now()) {
            return false;
        }
        let mut hand = slot.hand.borrow_mut();
        hand.runtime += 1;
        hand.running.clear();
        hand.retained.clear();
        let cells: Vec<usize> = std::mem::take(&mut hand.callbacks).into_values().collect();
        let mut calls = self.calls.borrow_mut();
        for call in calls.iter_mut() {
            if call.hand == at && !matches!(call.state, CallState::Committed(_)) {
                call.exposed = true;
            }
        }
        for index in cells {
            self.note(format!("call {index} dies with its cell"));
            calls[index].exposed = true;
            calls[index].state = CallState::Committed(Outcome::Unknown);
        }
        drop(calls);
        self.stats.borrow_mut().restarts += 1;
        self.note(format!("{} restarts as runtime {}", slot.key, hand.runtime));
        true
    }

    fn reply(id: u64, value: Value) -> String {
        RemoteFrame::Reply {
            id,
            result: Ok(Some(value)),
        }
        .encode(FRAME_BYTES_MAX)
        .expect("a simulated reply fits its frame")
    }

    fn receive(&self, at: usize, text: &str, out: &mpsc::UnboundedSender<String>) {
        let frame = RemoteFrame::decode(text, FRAME_BYTES_MAX).expect("the brain sends frames");
        let slot = &self.slots[at];
        let mut hand = slot.hand.borrow_mut();
        match frame {
            RemoteFrame::Request {
                id,
                operation: RemoteOperation::Call { context, .. },
                ..
            } => {
                if let Some(text) = self.resend(&mut hand, id) {
                    let _ = out.send(text);
                    return;
                }
                if hand.running.contains_key(&id) {
                    return;
                }
                let index = numbered(&context.call_id);
                let cell = self.calls.borrow()[index].cell;
                if !cell && !durable_step(&self.log, index) {
                    self.fail(
                        "folded",
                        format!("call {index} reached the hand before its step was durable"),
                    );
                }
                if !self.store.recorded(&context.session_id, &context.call_id) {
                    self.fail(
                        "folded",
                        format!("call {index} reached the hand before its request was recorded"),
                    );
                }
                let runs = hand.executions.entry(index).or_default();
                let label = format!("call {index} run {}", runs.len());
                runs.push(json!(label));
                let done = self.now() + self.script.runs[index];
                hand.seen.insert(id);
                hand.running.insert(id, (index, done, label));
                self.marks.borrow_mut().reached(index, self.now());
                self.note(format!("{} runs call {index} as request {id}", slot.key));
            }
            RemoteFrame::Request {
                id,
                operation: RemoteOperation::CancelTurn { .. },
                ..
            } if self.script.reopen.is_some() || !self.script.cancels.is_empty() => {
                let reply = RemoteFrame::Reply {
                    id,
                    result: Ok(None),
                };
                let _ = out.send(reply.encode(FRAME_BYTES_MAX).expect("fits"));
            }
            RemoteFrame::Ack { ids } => {
                for id in ids {
                    if self.store.unanswered(&slot.key, id) {
                        self.fail(
                            "ack_after_store",
                            format!("request {id} was acked before its reply was stored"),
                        );
                    }
                    hand.retained.remove(&id);
                }
            }
            RemoteFrame::CallbackResult { id, result } => {
                let Some(index) = hand.callbacks.remove(&id) else {
                    return;
                };
                let outcome = match result {
                    Ok(value) => output_from_wire(value)
                        .map_or(Outcome::Unknown, |output| Self::output(&output)),
                    Err(text) if UNSENT.iter().any(|unsent| text.contains(unsent)) => {
                        Outcome::NotDispatched
                    }
                    Err(_) => Outcome::Unknown,
                };
                self.note(format!("call {index} answers its cell with {outcome:?}"));
                self.calls.borrow_mut()[index].state = CallState::Committed(outcome);
            }
            RemoteFrame::Resume { pending } => {
                for id in std::mem::take(&mut hand.held) {
                    if let Some(index) = hand.callbacks.get(&id) {
                        let _ = out.send(self.callback(id, *index));
                    }
                }
                let read = slot.read.read(BTreeSet::clone);
                for id in &pending {
                    if let Some((label, _)) = hand.retained.get(id)
                        && read.contains(id)
                    {
                        let index = labelled(label);
                        self.marks.borrow_mut().reread.push((self.now(), index));
                    }
                }
                let mut pending = pending;
                if self.store.mutation == HandsMutation::ResumeWaitingOnly {
                    pending.retain(|id| !(hand.retained.contains_key(id) && read.contains(id)));
                }
                hand.retained.retain(|id, _| pending.contains(id));
                for id in pending {
                    if let Some(text) = self.resend(&mut hand, id) {
                        let _ = out.send(text);
                    } else if !hand.running.contains_key(&id) {
                        if !hand.seen.contains(&id)
                            && self.store.mutation == HandsMutation::ReopenUnprobed
                        {
                            continue;
                        }
                        let refused = RemoteFrame::Reply {
                            id,
                            result: Err(NOT_RECEIVED.to_owned()),
                        };
                        let _ = out.send(refused.encode(FRAME_BYTES_MAX).expect("fits"));
                    }
                }
            }
            _ => {}
        }
    }

    fn callback(&self, id: u64, index: usize) -> String {
        let hand = self.slots[self.calls.borrow()[index].hand].key.clone();
        RemoteFrame::Callback {
            id,
            name: "run".to_owned(),
            input: json!({}),
            freeform: false,
            context: RemoteContext::new(
                SESSION.to_owned(),
                format!("{CELL}/code-{index}"),
                Some(TURN.to_owned()),
            ),
            hand: Some(hand),
            compact: false,
            output_prefix: false,
            answers: false,
            asked: false,
        }
        .encode(FRAME_BYTES_MAX)
        .expect("a simulated callback fits its frame")
    }

    fn ask(&self, out: &mpsc::UnboundedSender<String>) {
        let now = self.now();
        let due: Vec<usize> = {
            let hand = self.slots[0].hand.borrow();
            self.calls
                .borrow()
                .iter()
                .enumerate()
                .filter(|(index, call)| {
                    call.cell
                        && matches!(call.state, CallState::Fresh)
                        && self.script.starts[*index] <= now
                        && !hand.asked.contains(index)
                })
                .map(|(index, _)| index)
                .collect()
        };
        for index in due {
            let id = {
                let mut hand = self.slots[0].hand.borrow_mut();
                hand.asked.push(index);
                let id = hand.next_callback;
                hand.next_callback += 1;
                hand.callbacks.insert(id, index);
                id
            };
            self.note(format!("the cell asks for call {index} as callback {id}"));
            let _ = out.send(self.callback(id, index));
        }
    }

    fn finish(&self, at: usize, out: &mpsc::UnboundedSender<String>) {
        let now = self.now();
        let mut hand = self.slots[at].hand.borrow_mut();
        let done: Vec<u64> = hand
            .running
            .iter()
            .filter(|(_, (_, at, _))| *at <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in done {
            let Some((index, _, label)) = hand.running.remove(&id) else {
                continue;
            };
            self.marks.borrow_mut().replies.push((at, now, index));
            let value = self.sent(&label, 0);
            hand.retained.insert(id, (label, 1));
            let _ = out.send(Self::reply(id, value));
        }
        if self.script.evicts {
            self.evict(at, &mut hand);
        }
    }

    /// The value of send number `send` of an execution's reply: its label and the count, which
    /// check strips again to compare with what ran.
    fn sent(&self, label: &str, send: u64) -> Value {
        let text = format!("{label}{SEND}{send}");
        if self.fleet() {
            output_to_wire(ToolOutput::text(text), false).expect("a simulated output encodes")
        } else {
            json!(text)
        }
    }

    /// Sends a retained reply again, numbered past every earlier send of it.
    fn resend(&self, hand: &mut Hand, id: u64) -> Option<String> {
        let (label, sends) = hand.retained.get_mut(&id)?;
        let value = self.sent(label, *sends);
        *sends += 1;
        Some(Self::reply(id, value))
    }

    /// Drops retained replies the store already holds, within one runtime: a resume that still
    /// names one then gets not-received, which must never replace the stored reply.
    fn evict(&self, at: usize, hand: &mut Hand) {
        let key = &self.slots[at].key;
        let stored: Vec<u64> = hand
            .retained
            .keys()
            .copied()
            .filter(|id| self.store.answered(key, *id))
            .collect();
        for id in stored {
            hand.retained.remove(&id);
            self.note(format!("{key} evicts its retained reply to request {id}"));
        }
    }

    async fn connection(
        &self,
        at: usize,
        out: mpsc::UnboundedSender<String>,
        mut incoming: mpsc::UnboundedReceiver<String>,
    ) {
        loop {
            if self.done() || self.restart_due(at) {
                return;
            }
            if Self::due(&self.slots[at].drops, self.now()) {
                self.stats.borrow_mut().drops += 1;
                if self.fleet() {
                    self.note(format!("{} socket drops", self.slots[at].key));
                } else {
                    self.note("socket drops".to_owned());
                }
                return;
            }
            self.finish(at, &out);
            if at == 0 && self.fleet() {
                self.ask(&out);
            }
            nanocode_sync::select! {
                text = incoming.recv() => match text {
                    Some(text) => self.receive(at, &text, &out),
                    None => return,
                },
                () = self.clock.sleep(TICK) => {}
            }
        }
    }

    async fn hand(&self, at: usize, acceptor: &Arc<Acceptor>) {
        let slot = &self.slots[at];
        loop {
            let redial = self.rng.borrow_mut().range(1, REDIAL_MS_MAX);
            let until = self.now() + redial * MS;
            while self.now() < until {
                if self.done() {
                    return;
                }
                self.restart_due(at);
                self.clock.sleep(TICK).await;
            }
            let runtime = if self.fleet() {
                format!("{}-runtime-{}", slot.key, slot.hand.borrow().runtime)
            } else {
                format!("runtime-{}", slot.hand.borrow().runtime)
            };
            let catalog = json!({"type": "catalog", "definitions": [], "runtime_id": runtime, "resumable": true});
            let catalog = RemoteCatalog::decode(&catalog.to_string(), FRAME_BYTES_MAX)
                .expect("the simulated catalog decodes");
            let (to_brain, from_hand) = mpsc::unbounded_channel();
            let (to_hand, from_brain) = mpsc::unbounded_channel();
            let link = SimLink {
                incoming: Owned::new(from_hand),
                outgoing: to_hand,
                resumes: runtime,
                read: Arc::clone(&slot.read),
            };
            self.dials.set(self.dials.get() + 1);
            let brain = async {
                {
                    let mut hand = slot.hand.borrow_mut();
                    hand.held = hand.callbacks.keys().copied().collect();
                }
                match acceptor.attach(slot.key.clone(), catalog, link).await {
                    Ok(attached) => attached.ended().await,
                    Err(error) => self.note(format!("attach refused: {error}")),
                }
            };
            nanocode_sync::future::join(brain, self.connection(at, to_brain, from_brain)).await;
        }
    }

    async fn canceller(&self, route: &AcceptorRoute, acceptor: &Acceptor) {
        for &at in &self.script.cancels {
            let now = self.now();
            if at > now {
                self.clock.sleep(Duration::from_nanos(at - now)).await;
            }
            if self.done() {
                return;
            }
            if acceptor.connected(KEY) {
                continue;
            }
            let dials = self.dials.get();
            let stopped: Vec<(u64, usize)> = self
                .waiting
                .borrow()
                .iter()
                .filter(|(_, (_, since))| *since == dials)
                .map(|(token, (index, _))| (*token, *index))
                .collect();
            if stopped.is_empty() {
                continue;
            }
            self.stats.borrow_mut().stopped += 1;
            self.note(format!(
                "turn stopped while calls {:?} wait for the hand",
                stopped.iter().map(|(_, index)| *index).collect::<Vec<_>>()
            ));
            self.stopped
                .borrow_mut()
                .extend(stopped.iter().map(|(token, _)| *token));
            if self.store.mutation == HandsMutation::CancelLate {
                while !acceptor.connected(KEY) && !self.done() {
                    self.clock.sleep(TICK).await;
                }
            }
            let cancel = RemoteOperation::CancelTurn {
                session_id: SESSION.to_owned(),
                turn_id: None,
            };
            let _ = route.request(cancel, Vec::new(), None).await;
        }
    }

    async fn adopt(&self, primary: &AcceptorRoute) {
        let origin = RemoteContext::new(SESSION.to_owned(), CELL.to_owned(), Some(TURN.to_owned()));
        while !self.done() {
            primary.adopt(&origin, Vec::new());
            self.clock.sleep(TICK).await;
        }
    }

    async fn admit(&self, log: &Arc<OneLog>, index: usize) -> bool {
        self.step(log, index, ADMITTED).await
    }

    async fn step(&self, log: &Arc<OneLog>, index: usize, payload: &str) -> bool {
        let mut store = log.store();
        let step = step(index);
        let result = async {
            let owned = store.acquire(&step, OwnerId::new()).await?;
            let commit = StateCommit {
                payload,
                records: &[],
                events: &[],
                summary: &SummaryUpdate::Unchanged,
                settle: Settle::Durable,
            };
            store
                .commit(&step, &owned.owner, owned.state.revision, commit)
                .await?;
            if payload == ANSWERED {
                store.flush().await?;
            }
            Ok::<_, StoreError>(())
        };
        match result.await {
            Ok(_) => true,
            Err(error) => {
                self.note(format!("call {index} step not committed: {error}"));
                false
            }
        }
    }

    fn named(&self, index: usize) -> &str {
        let hand = self.calls.borrow()[index].hand;
        let hand = if self.store.mutation == HandsMutation::FleetCrossHands {
            (hand + 1) % self.slots.len()
        } else {
            hand
        };
        &self.slots[hand].key
    }

    fn output(output: &ToolOutput) -> Outcome {
        let ToolOutputBody::Text(text) = &output.output else {
            return Outcome::Unknown;
        };
        if output.success {
            Outcome::Value(json!(text))
        } else if UNSENT.iter().any(|unsent| text.contains(unsent)) {
            Outcome::NotDispatched
        } else {
            Outcome::Unknown
        }
    }

    async fn dispatch(&self, via: &Via<'_>, index: usize) -> Outcome {
        let call_id = index.to_string();
        match via {
            Via::Route(route, barrier, acceptor) => {
                let waiting = Waiting::enter(self, acceptor, index);
                let context =
                    RemoteContext::new(SESSION.to_owned(), call_id, Some(TURN.to_owned()));
                let operation = RemoteOperation::Call {
                    name: "run".to_owned(),
                    input: json!({}),
                    context,
                    selected_tools: None,
                    run_time_ms: None,
                };
                let result = if self.store.mutation == HandsMutation::DispatchUnsettled {
                    route.call(operation).await
                } else {
                    route.call_after(operation, Arc::clone(barrier)).await
                };
                let outcome = match result {
                    Ok(value) => Outcome::Value(value),
                    Err(error) if error.is_unsent() => Outcome::NotDispatched,
                    Err(_) => Outcome::Unknown,
                };
                if waiting.stopped() && !matches!(outcome, Outcome::NotDispatched) {
                    self.fail(
                        "stopped",
                        format!(
                            "call {index} waited for an unattached hand while its turn was stopped, then returned {outcome:?}"
                        ),
                    );
                }
                outcome
            }
            Via::Hands(calls) => {
                let context =
                    ToolContext::new("", SESSION, &call_id, &[], DEFAULT_TOOL_OUTPUT_TOKENS)
                        .with_turn_id(Some(TURN));
                let output = calls
                    .call(self.named(index), "run", json!({}), context)
                    .await;
                Self::output(&output)
            }
        }
    }

    async fn recover(&self, via: &Via<'_>, index: usize) -> Outcome {
        self.stats.borrow_mut().recovered += 1;
        let call_id = index.to_string();
        match via {
            Via::Route(route, _, _) => {
                let context =
                    RemoteContext::new(SESSION.to_owned(), call_id, Some(TURN.to_owned()));
                let recovered = route
                    .recover(&context)
                    .expect("the acceptor's route recovers")
                    .await;
                match recovered {
                    Ok(Some(value)) => Outcome::Value(value),
                    Ok(None) => Outcome::Unknown,
                    Err(error) if error.is_unsent() => self.dispatch(via, index).await,
                    Err(_) => Outcome::Unknown,
                }
            }
            Via::Hands(_) if self.store.mutation == HandsMutation::FleetRecoverNone => {
                Outcome::Unknown
            }
            Via::Hands(calls) => {
                let context =
                    ToolContext::new("", SESSION, &call_id, &[], DEFAULT_TOOL_OUTPUT_TOKENS)
                        .with_turn_id(Some(TURN));
                let outcome = calls
                    .recover(self.named(index), context)
                    .await
                    .map_or(Outcome::Unknown, |output| Self::output(&output));
                match outcome {
                    Outcome::NotDispatched
                        if self.store.mutation == HandsMutation::FleetSwallowUnsent =>
                    {
                        Outcome::Unknown
                    }
                    Outcome::Value(value) => {
                        if self.calls.borrow()[index].hand > 0 {
                            self.stats.borrow_mut().retained += 1;
                        }
                        Outcome::Value(value)
                    }
                    outcome => outcome,
                }
            }
        }
    }

    async fn session_call(&self, via: &Via<'_>, log: &Arc<OneLog>, index: usize) {
        if self.calls.borrow()[index].cell {
            return;
        }
        let state = self.calls.borrow()[index].state.clone();
        let outcome = match state {
            CallState::Committed(_) => return,
            CallState::Fresh => {
                let start = self.script.starts[index];
                let now = self.now();
                if start > now {
                    self.clock.sleep(Duration::from_nanos(start - now)).await;
                }
                if !self.admit(log, index).await {
                    return;
                }
                self.marks
                    .borrow_mut()
                    .admit(index, self.calls.borrow()[index].hand, self.now());
                self.calls.borrow_mut()[index].state = CallState::Admitted;
                self.dispatch(via, index).await
            }
            CallState::Admitted => self.recover(via, index).await,
        };
        self.clock
            .sleep(Duration::from_nanos(self.script.commits[index]))
            .await;
        if !self.step(log, index, ANSWERED).await {
            return;
        }
        self.marks.borrow_mut().commits.insert(index, self.now());
        self.note(format!("call {index} commits {outcome:?}"));
        self.calls.borrow_mut()[index].state = CallState::Committed(outcome);
    }

    fn check(&self) {
        for overwrite in self.store.overwrites.read(Vec::clone) {
            self.fail("stored", overwrite);
        }
        for (index, call) in self.calls.borrow().iter().enumerate() {
            for (at, slot) in self.slots.iter().enumerate() {
                if at != call.hand && slot.hand.borrow().executions.contains_key(&index) {
                    self.fail(
                        "routed",
                        format!(
                            "call {index} for {} ran on {}",
                            self.slots[call.hand].key, slot.key
                        ),
                    );
                }
            }
            let hand = self.slots[call.hand].hand.borrow();
            let runs = hand.executions.get(&index).map_or(&[][..], Vec::as_slice);
            if runs.len() > 1 {
                self.fail("once", format!("call {index} ran {} times", runs.len()));
            }
            match &call.state {
                CallState::Committed(Outcome::Value(value)) if runs != [executed(value)] => {
                    self.fail(
                        "real",
                        format!("call {index} committed {value} but ran {runs:?}"),
                    );
                }
                CallState::Committed(Outcome::NotDispatched) if !runs.is_empty() => {
                    self.fail(
                        "real",
                        format!("call {index} committed not dispatched but ran"),
                    );
                }
                CallState::Committed(Outcome::Unknown) if !call.exposed => {
                    self.fail(
                        "honest",
                        format!("call {index} committed unknown though its hand never restarted"),
                    );
                }
                CallState::Committed(Outcome::Unknown) => {
                    self.stats.borrow_mut().unknown += 1;
                }
                CallState::Fresh | CallState::Admitted => {
                    self.fail("settles", format!("call {index} never settled"));
                }
                CallState::Committed(_) => {}
            }
        }
    }
}

struct Redispatch(Arc<dyn HandCalls>);

impl HandCalls for Redispatch {
    fn here(&self, hand: &str) -> bool {
        self.0.here(hand)
    }

    fn auto(&self, tool: &str) -> bool {
        self.0.auto(tool)
    }

    fn call<'a>(
        &'a self,
        hand: &'a str,
        name: &'a str,
        input: Value,
        context: ToolContext<'a>,
    ) -> HostFuture<'a, ToolOutput> {
        self.0.call(hand, name, input, context)
    }
}

/// The execution a reply value names, without the send count `World::sent` added.
fn executed(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|text| text.rsplit_once(SEND))
        .map_or_else(|| value.clone(), |(label, _)| json!(label))
}

fn numbered(call_id: &str) -> usize {
    call_id
        .rsplit_once("/code-")
        .map_or(call_id, |(_, index)| index)
        .parse()
        .expect("calls are numbered")
}

fn options(fleet: usize) -> AcceptorOptions {
    AcceptorOptions {
        path: "/".to_owned(),
        frame_bytes_max: FRAME_BYTES_MAX,
        head_timeout: GRACE,
        catalog_timeout: GRACE,
        ping: GRACE,
        heartbeat_timeout: GRACE,
        operation_timeout: None,
        reconnect_grace: GRACE,
        runtimes_max: RUNTIMES_MAX.max(fleet),
        attaches_queued_max: ATTACHES_QUEUED_MAX,
    }
}

async fn opened(world: &World) -> Arc<OneLog> {
    let host = Arc::clone(&world.log) as Arc<dyn LogHost>;
    let log = OneLog::open(host, SCOPE, policy())
        .await
        .expect("the simulated turn log opens");
    log.settle_durably();
    for (index, call) in world.calls.borrow_mut().iter_mut().enumerate() {
        if matches!(call.state, CallState::Admitted) && !durable_step(&world.log, index) {
            world.note(format!("call {index} step was lost; the model asks again"));
            world.stats.borrow_mut().replayed += 1;
            call.state = CallState::Fresh;
        }
    }
    log
}

async fn runtime(world: &World, acceptor: &Arc<Acceptor>, log: &Arc<OneLog>) {
    let barrier = log.barrier();
    if !world.fleet() {
        let route = acceptor.route(Arc::new(|| Box::pin(async { Some(KEY.to_owned()) })));
        let via = Via::Route(&route, &barrier, acceptor);
        let calls =
            (0..world.script.starts.len()).map(|index| world.session_call(&via, log, index));
        nanocode_sync::future::join(
            nanocode_sync::join_in_order(calls),
            world.canceller(&route, acceptor),
        )
        .await;
        return;
    }
    let executors = CodeModeExecutors::new();
    for (at, slot) in world.slots.iter().enumerate() {
        let key = slot.key.clone();
        let route = acceptor.route(Arc::new(move || {
            let key = key.clone();
            Box::pin(async move { Some(key) })
        }));
        let host = RemoteCodeModeHost::new(route, Vec::new());
        let unbarriered = match world.store.mutation {
            HandsMutation::DispatchUnsettled => true,
            HandsMutation::FleetUnbarriered => at > 0,
            _ => false,
        };
        let host = if unbarriered {
            host
        } else {
            host.barrier(Arc::clone(&barrier))
        };
        executors.insert(slot.key.clone(), Arc::new(host));
    }
    let hands = executors.calls();
    let primary = acceptor.route(Arc::new(|| Box::pin(async { Some("hand-0".to_owned()) })));
    let routed: Arc<dyn HandCalls> = if world.store.mutation == HandsMutation::FleetRedispatch {
        Arc::new(Redispatch(Arc::clone(&hands)))
    } else {
        Arc::clone(&hands)
    };
    primary.route_hands(routed);
    let via = Via::Hands(&hands);
    let calls = (0..world.script.starts.len()).map(|index| world.session_call(&via, log, index));
    let sessions = nanocode_sync::join_in_order(calls);
    nanocode_sync::future::join(sessions, world.adopt(&primary)).await;
}

async fn runtimes(world: &World, acceptor: &Arc<Acceptor>, log: Arc<OneLog>) {
    let mut log = log;
    loop {
        let reopen = world.reopen.take().filter(|at| *at > world.now());
        let Some(at) = reopen else {
            world.store.stalling.store(false, Ordering::Relaxed);
            runtime(world, acceptor, &log).await;
            return;
        };
        let until = at - world.now();
        world.store.stalling.store(true, Ordering::Relaxed);
        nanocode_sync::select! {
            () = runtime(world, acceptor, &log) => return,
            () = world.clock.sleep(Duration::from_nanos(until)) => {}
        }
        world.store.stalling.store(false, Ordering::Relaxed);
        if log.settle_deferred().await.is_err() {
            world.note("the brain could not settle its dropped runtime's log".to_owned());
        }
        world.store.abandon();
        world.stats.borrow_mut().reopened += 1;
        world.note("the brain drops its runtime and reopens in process".to_owned());
        drop(log);
        log = opened(world).await;
    }
}

async fn process(world: &World) {
    for slot in &world.slots {
        slot.read.update(BTreeSet::clear);
    }
    let log = opened(world).await;
    let (acceptor, _attaches) = Acceptor::new(
        options(world.slots.len()),
        Arc::clone(&world.store) as Arc<dyn HandRequests>,
        |_, _| {
            Err(Refusal {
                status: StatusCode::NOT_FOUND,
                body: String::new(),
            })
        },
    );
    let drivers = (0..world.slots.len()).map(|at| world.hand(at, &acceptor));
    nanocode_sync::future::join(
        runtimes(world, &acceptor, log),
        nanocode_sync::join_in_order(drivers),
    )
    .await;
}

pub struct HandsOutcome {
    pub digest: u64,
    pub trace: Vec<String>,
    pub wrong: Option<String>,
    pub stats: HandsStats,
    pub shape: String,
}

fn simulate(seed: u64, mutation: HandsMutation, options: HandsOptions) -> HandsOutcome {
    let options = mutation.options(options);
    let mut script = Script::of(seed, options);
    let mut rng = Rng::derive(seed, "hands aim");
    if !script.crashes.is_empty() && rng.chance(AIM_PPM) {
        let mut probe = Script::of(seed, options);
        probe.crashes.clear();
        let (_, mut marks) = play(seed, mutation, options, probe);
        marks.settle();
        if rng.chance(DROP_PPM) && script.aim_drop(&mut rng, &marks) {
            let mut probe = Script::of(seed, options);
            probe.crashes.clear();
            for (at, schedule) in probe.schedules.iter_mut().enumerate() {
                schedule.drops.clone_from(&script.schedules[at].drops);
            }
            marks = play(seed, mutation, options, probe).1;
            marks.settle();
        }
        if options.fleet <= 1 {
            marks.unsent.clear();
        }
        script.aim(&mut rng, &marks);
    }
    play(seed, mutation, options, script).0
}

/// What a run reached: the instant of every trace note, each call's admission and each reply
/// a hand sent (with its hand slot), each commit, and the windows a crash is aimed into.
#[derive(Default)]
struct Marks {
    around: Vec<u64>,
    admitted: BTreeMap<usize, u64>,
    admissions: Vec<(usize, u64)>,
    replies: Vec<(usize, u64, usize)>,
    commits: BTreeMap<usize, u64>,
    waits: Vec<(u64, u64)>,
    /// The windows from a call's admission until its request reached a hand, or until its
    /// commit when the request never reached one.
    unsent: Vec<(u64, u64)>,
    /// Each resume that named a reply the hand retains and this brain process already read,
    /// with that reply's call.
    reread: Vec<(u64, usize)>,
    /// The window from each such resume to its call's commit.
    resumed: Vec<(u64, u64)>,
}

impl Marks {
    fn admit(&mut self, index: usize, slot: usize, now: u64) {
        self.admitted.entry(index).or_insert(now);
        self.admissions.push((slot, now));
    }

    /// Notes the wait from a call's admission to its request reaching a hand, when it waited.
    fn reached(&mut self, index: usize, now: u64) {
        if let Some(from) = self.admitted.remove(&index)
            && now > from + 1
        {
            self.waits.push((from, now));
            self.unsent.push((from, now));
        }
    }

    /// Adds the window from each sent reply to its call's commit, once the run ended.
    fn settle(&mut self) {
        for (index, from) in &self.admitted {
            if let Some(commit) = self.commits.get(index)
                && *commit > from + 1
            {
                self.unsent.push((*from, *commit));
            }
        }
        for (_, at, index) in &self.replies {
            if let Some(commit) = self.commits.get(index)
                && *commit > at + 1
            {
                self.waits.push((*at, *commit));
            }
        }
        for (at, index) in &self.reread {
            if let Some(commit) = self.commits.get(index)
                && *commit > at + 1
            {
                self.resumed.push((*at, *commit));
            }
        }
    }
}

/// The call an execution label (`call {index} run {n}`) belongs to.
fn labelled(label: &str) -> usize {
    label
        .split(' ')
        .nth(1)
        .and_then(|index| index.parse().ok())
        .expect("an execution label names its call")
}

/// Runs one seed's script, returning its outcome and what it reached.
fn play(
    seed: u64,
    mutation: HandsMutation,
    options: HandsOptions,
    script: Script,
) -> (HandsOutcome, Marks) {
    let fleet = options.fleet;
    let shape = script.describe();
    let clock = Clock::new();
    crate::host::enter(seed, &clock);
    let calls = script
        .hands
        .iter()
        .zip(&script.cells)
        .map(|(hand, cell)| Call {
            state: CallState::Fresh,
            exposed: false,
            hand: *hand,
            cell: *cell,
        })
        .collect();
    let slots = script
        .schedules
        .iter()
        .enumerate()
        .map(|(at, schedule)| Slot {
            key: if fleet > 1 {
                format!("hand-{at}")
            } else {
                KEY.to_owned()
            },
            hand: RefCell::new(Hand::default()),
            restarts: RefCell::new(schedule.restarts.clone()),
            drops: RefCell::new(schedule.drops.clone()),
            read: Arc::default(),
        })
        .collect();
    let crashes = script.crashes.clone();
    let store = Arc::new(Store {
        rows: Atom::default(),
        writes: AtomicU64::new(0),
        abandoned: Atom::default(),
        overwrites: Atom::default(),
        orphaned: AtomicU64::new(0),
        stalling: AtomicBool::new(false),
        stalled: AtomicU64::new(0),
        seed,
        mutation,
    });
    let reopen = script.reopen;
    let world = World {
        reopen: Cell::new(reopen),
        dials: Cell::new(0),
        waiting: RefCell::new(BTreeMap::new()),
        stopped: RefCell::new(BTreeSet::new()),
        tokens: Cell::new(0),
        slots,
        script,
        log: Arc::new(StepHost {
            rows: Atom::default(),
            store: Arc::clone(&store),
            folded: AtomicU64::new(0),
            rode: AtomicU64::new(0),
        }),
        store,
        calls: RefCell::new(calls),
        rng: RefCell::new(Rng::derive(seed, "hands world")),
        clock: Arc::clone(&clock),
        trace: RefCell::new(Vec::new()),
        marks: RefCell::new(Marks::default()),
        wrong: RefCell::new(Vec::new()),
        stats: RefCell::new(HandsStats {
            seeds: 1,
            calls: u64::try_from(Script::of(seed, options).starts.len()).unwrap_or(u64::MAX),
            ..HandsStats::default()
        }),
    };
    let world = Rc::new(world);
    let mut epoch = 0_u64;
    loop {
        let crash = crashes
            .get(usize::try_from(epoch).unwrap_or(usize::MAX))
            .copied();
        let horizon = crash.unwrap_or_else(|| world.now() + SETTLE_HORIZON_MS * MS);
        let tasks: Vec<Task<'_>> = vec![Box::pin(process(&world))];
        let mut executor = Executor::new(
            Arc::clone(&clock),
            Rng::derive(seed ^ epoch, "hands scheduler"),
        );
        let ended = executor.run(tasks, Some(horizon));
        drop(executor);
        drop(crate::host::spawned());
        match ended {
            Ended::Crashed if crash.is_some() => {
                world.stats.borrow_mut().crashes += 1;
                world.note(format!("host crashes in epoch {epoch}"));
                epoch += 1;
            }
            Ended::Crashed => {
                world.note("the run reached its horizon".to_owned());
                break;
            }
            Ended::Stuck => {
                world.fail("settles", "every task waits and no timer is set".to_owned());
                break;
            }
            Ended::Finished => break,
        }
    }
    world.check();
    world.stats.borrow_mut().folded = world.log.folded.load(Ordering::Relaxed);
    world.stats.borrow_mut().rode = world.log.rode.load(Ordering::Relaxed);
    world.stats.borrow_mut().orphaned = world.store.orphaned.load(Ordering::Relaxed);
    world.stats.borrow_mut().stalled = world.store.stalled.load(Ordering::Relaxed);
    let trace = world.trace.borrow();
    let mut digest = fnv(FNV_OFFSET, shape.as_bytes());
    for line in trace.iter() {
        digest = fnv(digest, line.as_bytes());
    }
    digest = fnv(digest, &clock.now_ns().to_le_bytes());
    let wrong = world.wrong.borrow().first().cloned();
    let stats = std::mem::take(&mut *world.stats.borrow_mut());
    let trace = trace.clone();
    let marks = std::mem::take(&mut *world.marks.borrow_mut());
    (
        HandsOutcome {
            digest,
            trace,
            wrong,
            stats,
            shape,
        },
        marks,
    )
}

pub fn run_seed(seed: u64, mutation: HandsMutation, options: HandsOptions) -> HandsOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation, options)).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        HandsOutcome {
            digest: 0,
            trace: Vec::new(),
            wrong: Some(format!("panic: {message}")),
            stats: HandsStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge_seed(
    seed: u64,
    mutation: HandsMutation,
    options: HandsOptions,
    twice: bool,
    verbose: bool,
    totals: &mut HandsStats,
) -> Option<String> {
    let first = run_seed(seed, mutation, options);
    totals.add(&first.stats);
    if verbose {
        println!("seed {seed} digest {:016x} {}", first.digest, first.shape);
        if first.wrong.is_some() {
            for line in &first.trace {
                println!("  trace: {line}");
            }
        }
    }
    if first.wrong.is_some() || !twice {
        return first
            .wrong
            .map(|message| format!("{} :: {message}", first.shape));
    }
    let second = crate::host::second_pass(|| run_seed(seed, mutation, options));
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: trace digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: HandsMutation, options: HandsOptions) -> String {
    let mut command = format!("nanocode-sim --scenario hands --seed {seed}");
    if options.fleet > 1 {
        command.push_str(&format!(" --fleet {}", options.fleet));
    }
    if options.reopen {
        command.push_str(" --reopen");
    }
    if options.cancel {
        command.push_str(" --cancel");
    }
    if mutation != HandsMutation::None {
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
    mutation: HandsMutation,
    options: HandsOptions,
    totals: &HandsStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=hands mutation={} fleet={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name(),
        mutation.options(options).fleet
    );
    match mutation {
        HandsMutation::None if failed == 0 => ExitCode::SUCCESS,
        HandsMutation::None => ExitCode::FAILURE,
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
