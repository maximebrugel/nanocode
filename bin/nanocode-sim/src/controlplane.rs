use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    process::ExitCode,
    time::Instant,
};

use nanocode_board::{
    Change, ClaimId, Entry, Event, EventId, Fold, NewTask, Op, Outcome, Phase, Rejection, TaskId,
    sim::{self as board_sim, Mutation as BoardRule},
};

use crate::rng::{FNV_OFFSET, Rng, fnv};

const OWNER: &str = "user:lead";
const DISPATCHER: &str = "user:lead+dispatch";
const AGENTS_DEFAULT: u64 = 10_000;
const RETRIES_MAX: u64 = 64;
const STRANDED_ROUNDS_MAX: u64 = 2;
const SETTLE_LEASES_MAX: u64 = 400;
const CRASH_SPAWNS_MAX: u64 = 8;
const PICK_CHECKS: u64 = 200;
const PERCENT: u64 = 100;
const PPM: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlplaneMutation {
    None,
    ClaimIgnoresLease,
    PostOverwrites,
    NextIgnoresRank,
    BudgetSkipsInflight,
    WakeDropped,
    GoalFinishedEarly,
    SpawnRetryRenames,
}

impl ControlplaneMutation {
    const ALL: [Self; 8] = [
        Self::None,
        Self::ClaimIgnoresLease,
        Self::PostOverwrites,
        Self::NextIgnoresRank,
        Self::BudgetSkipsInflight,
        Self::WakeDropped,
        Self::GoalFinishedEarly,
        Self::SpawnRetryRenames,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ClaimIgnoresLease => "claim_ignores_lease",
            Self::PostOverwrites => "post_overwrites",
            Self::NextIgnoresRank => "next_ignores_rank",
            Self::BudgetSkipsInflight => "budget_skips_inflight",
            Self::WakeDropped => "wake_dropped",
            Self::GoalFinishedEarly => "goal_finished_early",
            Self::SpawnRetryRenames => "spawn_retry_renames",
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
            Self::ClaimIgnoresLease => BoardRule::ClaimIgnoresLease,
            Self::PostOverwrites => BoardRule::PostOverwrites,
            Self::NextIgnoresRank => BoardRule::NextIgnoresRank,
            _ => BoardRule::None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ControlplaneStats {
    pub seeds: u64,
    pub events: u64,
    pub goals: u64,
    pub tasks: u64,
    pub spawns: u64,
    pub launches: u64,
    pub claims: u64,
    pub races_lost: u64,
    pub steals: u64,
    pub fenced: u64,
    pub retries: u64,
    pub dupes: u64,
    pub crashes: u64,
    pub dispatcher_crashes: u64,
    pub reaped: u64,
    pub retired: u64,
    pub pick_checks: u64,
    pub alive_max: u64,
    pub wall_ms: u64,
}

impl ControlplaneStats {
    pub fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.events += other.events;
        self.goals += other.goals;
        self.tasks += other.tasks;
        self.spawns += other.spawns;
        self.launches += other.launches;
        self.claims += other.claims;
        self.races_lost += other.races_lost;
        self.steals += other.steals;
        self.fenced += other.fenced;
        self.retries += other.retries;
        self.dupes += other.dupes;
        self.crashes += other.crashes;
        self.dispatcher_crashes += other.dispatcher_crashes;
        self.reaped += other.reaped;
        self.retired += other.retired;
        self.pick_checks += other.pick_checks;
        self.alive_max = self.alive_max.max(other.alive_max);
        self.wall_ms += other.wall_ms;
    }
}

struct Shape {
    agents: u64,
    members: u64,
    projects: u64,
    member_max: u64,
    project_max: u64,
    usd_max: u64,
    prices: Vec<u64>,
    goals: u64,
    storms: u64,
    storm_gap_ms: u64,
    children_max: u64,
    rank_ppm: u64,
    work_ms_max: u64,
    work_lease_ms: u64,
    own_lease_ms: u64,
    net_ms_max: u64,
    drop_ppm: u64,
    ack_lost_ppm: u64,
    retry_ms: u64,
    crash_ppm: u64,
    pause_ppm: u64,
    width: u64,
    round_ms: u64,
    spawn_ms_max: u64,
    start_timeout_ms: u64,
    idle_retire_ms: u64,
    dispatch_crash_ppm: u64,
    restart_ms: u64,
    wake_ms_max: u64,
    resend_ppm: u64,
    pick_check_ppm: u64,
}

impl Shape {
    fn of(seed: u64, agents: u64) -> Self {
        let mut rng = Rng::derive(seed, "controlplane shape");
        let members = rng.range(2, 16);
        let projects = rng.range(1, 8);
        let share = |rng: &mut Rng, parts: u64| {
            (agents * rng.range(PERCENT / 2, PERCENT) / PERCENT / parts).max(1)
        };
        let member_max = share(&mut rng, members);
        let project_max = share(&mut rng, projects);
        let prices = (0..projects).map(|_| rng.range(1, 5)).collect::<Vec<_>>();
        let price_max = prices.iter().copied().max().unwrap_or(1);
        let usd_max =
            (agents * price_max * rng.range(PERCENT / 2, PERCENT) / PERCENT).max(price_max);
        let net_ms_max = rng.range(1, 40);
        let retry_ms = rng.range(net_ms_max, 4 * net_ms_max);
        let wake_ms_max = rng.range(1, 30);
        let round_ms = rng.range(4 * net_ms_max + 2 * retry_ms + wake_ms_max, 1_500);
        let spawn_ms_max = rng.range(50, 3_000);
        let work_ms_max = rng.range(500, 10_000);
        let bound_ms = STRANDED_ROUNDS_MAX * round_ms + spawn_ms_max;
        Self {
            agents,
            members,
            projects,
            member_max,
            project_max,
            usd_max,
            prices,
            goals: (agents * rng.range(PERCENT / 4, PERCENT * 2 / 3) / PERCENT).max(1),
            storms: rng.range(1, 6),
            storm_gap_ms: rng.range(1_000, 30_000),
            children_max: rng.range(1, 8),
            rank_ppm: rng.range(0, PPM),
            work_ms_max,
            work_lease_ms: rng.range(work_ms_max + 1, 3 * work_ms_max),
            own_lease_ms: rng.range(4 * round_ms, 30_000_u64.max(5 * round_ms)),
            net_ms_max,
            drop_ppm: rng.range(0, 50_000),
            ack_lost_ppm: rng.range(10_000, 80_000),
            retry_ms,
            crash_ppm: rng.range(5_000, 20_000),
            pause_ppm: rng.range(0, 100_000),
            width: rng.range(1, 4),
            round_ms,
            spawn_ms_max,
            start_timeout_ms: 2 * spawn_ms_max + 8 * retry_ms + 4 * net_ms_max,
            idle_retire_ms: rng.range(3 * bound_ms, 6 * bound_ms),
            dispatch_crash_ppm: rng.range(10_000, 150_000),
            restart_ms: rng.range(round_ms, 10 * round_ms),
            wake_ms_max,
            resend_ppm: rng.range(100_000, 600_000),
            pick_check_ppm: (PPM * PICK_CHECKS / (agents * 4)).clamp(1, PPM),
        }
    }

    fn describe(&self) -> String {
        format!(
            "agents={} members={} projects={} member_max={} project_max={} usd_max={} goals={} storms={} children_max={} round_ms={} spawn_ms_max={} work_ms_max={}",
            self.agents,
            self.members,
            self.projects,
            self.member_max,
            self.project_max,
            self.usd_max,
            self.goals,
            self.storms,
            self.children_max,
            self.round_ms,
            self.spawn_ms_max,
            self.work_ms_max
        )
    }

    const fn pairs(&self) -> usize {
        (self.members * self.projects) as usize
    }

    const fn member(&self, pair: usize) -> usize {
        pair / self.projects as usize
    }

    const fn project(&self, pair: usize) -> usize {
        pair % self.projects as usize
    }
}

type Key = (bool, u64, u64);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Who {
    Agent(usize),
    Member,
    Dispatcher(u64),
}

enum Then {
    Own,
    Renew,
    Retire,
    Claim(TaskId),
    Child { goal: TaskId, index: u64 },
    ChildRank { goal: TaskId, index: u64 },
    Finish(TaskId),
    Goal(TaskId),
    Ranked,
    Dupe { conflict: bool, seq: u64 },
    Spawn { task: TaskId, pair: usize },
    Cancel(TaskId),
}

struct Write {
    who: Who,
    event: Event,
    body: String,
    then: Then,
    attempt: u64,
}

enum Act {
    Arrive(Box<Write>),
    Reply(Box<Write>, bool),
    Launch { intent: TaskId, pair: usize },
    Renew(usize),
    Work { agent: usize, claim: ClaimId },
    Wake(usize),
    Retire { agent: usize, since_ms: u64 },
    Lapse { task: TaskId, until_ms: u64 },
    Storm(u64),
    Round,
    Audit,
    Restart,
}

#[derive(Clone, PartialEq, Eq)]
enum Life {
    Starting,
    Seeking,
    Idle { since_ms: u64 },
    Busy { task: TaskId, claim: ClaimId },
    Planning { claim: ClaimId },
    Retiring,
    Gone,
}

struct Proc {
    writer: String,
    pair: usize,
    intent: TaskId,
    minted: u64,
    own: Option<ClaimId>,
    life: Life,
}

struct Intent {
    pair: usize,
    posted_ms: u64,
    until_ms: Option<u64>,
}

#[derive(Default)]
struct Index {
    entries: usize,
    ready: Vec<BTreeSet<(Key, TaskId)>>,
    keys: BTreeMap<TaskId, (usize, Key)>,
    held: BTreeMap<TaskId, (ClaimId, u64)>,
    lapses: BTreeSet<(u64, TaskId)>,
    open_intents: BTreeSet<(u64, TaskId)>,
    intent_leases: BTreeSet<(u64, TaskId)>,
    intents: BTreeMap<TaskId, Intent>,
    intent_labels: BTreeSet<String>,
    spawned: Vec<u64>,
    ledger_member: Vec<u64>,
    ledger_project: Vec<u64>,
    ledger_usd: u64,
    claimed_member: Vec<u64>,
    claimed_project: Vec<u64>,
    claimed_usd: u64,
    posted: BTreeSet<TaskId>,
    ready_since: BTreeMap<TaskId, u64>,
    goal_posted: BTreeMap<TaskId, u64>,
    goal_first: BTreeSet<TaskId>,
    goals_done: u64,
    woken: Vec<usize>,
    lapse_acts: Vec<(TaskId, u64)>,
}

#[derive(Default)]
struct Dispatch {
    epoch: u64,
    up: bool,
    up_since_ms: u64,
    inflight: BTreeMap<TaskId, usize>,
    cancelling: BTreeSet<TaskId>,
    next: BTreeMap<usize, u64>,
    crash_after: Option<u64>,
}

struct Sim {
    shape: Shape,
    mutation: ControlplaneMutation,
    rng: Rng,
    now_ms: u64,
    order: u64,
    queue: BTreeMap<(u64, u64), Act>,
    fold: Fold,
    index: Index,
    procs: Vec<Proc>,
    idle: Vec<BTreeSet<usize>>,
    alive_member: Vec<u64>,
    alive_project: Vec<u64>,
    alive_usd: u64,
    alive: u64,
    launching: Vec<u64>,
    dispatch: Dispatch,
    member_minted: u64,
    storms_posted: u64,
    streak_idle: Vec<u64>,
    streak_unserved: Vec<u64>,
    idle_open_ms: u64,
    claim_ms: Vec<u64>,
    goal_ms: Vec<u64>,
    digest: u64,
    stats: ControlplaneStats,
    wrong: Option<String>,
    settled: bool,
}

pub struct ControlplaneOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: ControlplaneStats,
    shape: String,
    score: String,
}

fn labels(pairs: &[(&str, String)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn children(goal: &TaskId) -> u64 {
    goal.0
        .rsplit_once('k')
        .and_then(|(_, count)| count.parse().ok())
        .expect("a goal names its child count")
}

fn is_goal(task: &TaskId) -> bool {
    task.0.starts_with('g') && !task.0.contains('/')
}

impl Sim {
    fn new(seed: u64, agents: u64, mutation: ControlplaneMutation) -> Self {
        let shape = Shape::of(seed, agents);
        let pairs = shape.pairs();
        let members = shape.members as usize;
        let projects = shape.projects as usize;
        let index = Index {
            ready: vec![BTreeSet::new(); pairs],
            spawned: vec![0; pairs],
            ledger_member: vec![0; members],
            ledger_project: vec![0; projects],
            claimed_member: vec![0; members],
            claimed_project: vec![0; projects],
            ..Index::default()
        };
        Self {
            shape,
            mutation,
            rng: Rng::derive(seed, "controlplane run"),
            now_ms: 0,
            order: 0,
            queue: BTreeMap::new(),
            fold: Fold::new(),
            index,
            procs: Vec::new(),
            idle: vec![BTreeSet::new(); pairs],
            alive_member: vec![0; members],
            alive_project: vec![0; projects],
            alive_usd: 0,
            alive: 0,
            launching: vec![0; pairs],
            dispatch: Dispatch {
                up: true,
                ..Dispatch::default()
            },
            member_minted: 0,
            storms_posted: 0,
            streak_idle: vec![0; pairs],
            streak_unserved: vec![0; pairs],
            idle_open_ms: 0,
            claim_ms: Vec::new(),
            goal_ms: Vec::new(),
            digest: FNV_OFFSET,
            stats: ControlplaneStats {
                seeds: 1,
                ..ControlplaneStats::default()
            },
            wrong: None,
            settled: false,
        }
    }

    fn at(&mut self, delay_ms: u64, act: Act) {
        self.order += 1;
        self.queue.insert((self.now_ms + delay_ms, self.order), act);
    }

    fn net(&mut self) -> u64 {
        self.rng.range(1, self.shape.net_ms_max)
    }

    fn fail(&mut self, why: String) {
        if self.wrong.is_none() {
            self.wrong = Some(format!("at {} ms: {why}", self.now_ms));
        }
    }

    fn send(&mut self, who: Who, id: EventId, by: &str, op: Op, then: Then) {
        let event = Event {
            id,
            by: by.to_owned(),
            at_ms: self.now_ms,
            op,
        };
        let body = serde_json::to_string(&event).expect("a board event encodes");
        let delay = self.net();
        self.at(
            delay,
            Act::Arrive(Box::new(Write {
                who,
                event,
                body,
                then,
                attempt: 0,
            })),
        );
    }

    fn agent_send(&mut self, agent: usize, op: Op, then: Then) {
        let proc = &mut self.procs[agent];
        proc.minted += 1;
        let id = EventId(format!("{}.{}", proc.writer, proc.minted));
        let by = proc.writer.clone();
        self.send(Who::Agent(agent), id, &by, op, then);
    }

    fn arrive(&mut self, write: Box<Write>) {
        if self.rng.chance(self.shape.drop_ppm) {
            let delay = self.net();
            self.at(delay, Act::Reply(write, false));
            return;
        }
        let seq = self.fold.next();
        self.fold
            .apply(seq, &write.body)
            .expect("the simulated log appends rows in order");
        self.digest = fnv(self.digest, write.body.as_bytes());
        self.stats.events += 1;
        self.observe();
        let delay = self.net();
        self.at(delay, Act::Reply(write, true));
    }

    fn rename(write: &mut Write) {
        let Then::Spawn { task, .. } = &mut write.then else {
            return;
        };
        let Op::Post { task: new } = &mut write.event.op else {
            return;
        };
        let renamed = TaskId(format!("{}r{}", new.id.0, write.attempt));
        new.id = renamed.clone();
        write.event.id = EventId(format!("{DISPATCHER}#{}", renamed.0));
        *task = renamed;
        write.body = serde_json::to_string(&write.event).expect("a board event encodes");
    }

    fn reply(&mut self, mut write: Box<Write>, applied: bool) {
        let live = match write.who {
            Who::Agent(agent) => self.procs[agent].life != Life::Gone,
            Who::Member => true,
            Who::Dispatcher(epoch) => self.dispatch.up && self.dispatch.epoch == epoch,
        };
        if !live {
            return;
        }
        if !applied || self.rng.chance(self.shape.ack_lost_ppm) {
            write.attempt += 1;
            assert!(
                write.attempt < RETRIES_MAX,
                "a write was retried {RETRIES_MAX} times"
            );
            self.stats.retries += 1;
            if self.mutation == ControlplaneMutation::SpawnRetryRenames {
                Self::rename(&mut write);
            }
            let delay = self.shape.retry_ms;
            self.at(delay, Act::Arrive(write));
            return;
        }
        let verdict = self
            .fold
            .outcome(&write.event.id)
            .expect("an applied write has a verdict")
            .clone();
        let claim = ClaimId(write.event.id.0.clone());
        match write.who {
            Who::Agent(agent) => self.heard(agent, write.then, verdict, claim),
            Who::Member => self.member_heard(write.then, verdict),
            Who::Dispatcher(_) => self.dispatcher_heard(write.then, verdict),
        }
    }

    fn observe(&mut self) {
        let start = self.index.entries;
        let end = self.fold.entries().len();
        self.index.entries = end;
        for at in start..end {
            let entry = self.fold.entries()[at].clone();
            self.entry(&entry);
        }
        for pair in std::mem::take(&mut self.index.woken) {
            self.wake(pair);
        }
        for (task, until_ms) in std::mem::take(&mut self.index.lapse_acts) {
            let delay = until_ms.saturating_sub(self.now_ms);
            self.at(delay, Act::Lapse { task, until_ms });
        }
    }

    fn entry(&mut self, entry: &Entry) {
        let task = self.fold.task(&entry.task).expect("an entry names a task");
        let labels = &task.spec.labels;
        let pair = labels
            .get("pair")
            .and_then(|pair| pair.parse::<usize>().ok())
            .expect("every task carries its pair");
        let agent = labels.get("pool").map(String::as_str) == Some("agent");
        let intent = labels.get("intent").cloned();
        let rank = task.rank;
        let parent = task.spec.parent.clone();
        let id = entry.task.clone();
        match &entry.change {
            Change::Posted => self.posted(id, pair, agent, intent, rank, entry),
            Change::Claimed {
                claim, until_ms, ..
            } => {
                if let Some((held, until)) = self.index.held.get(&id)
                    && *until > entry.at_ms
                {
                    let (held, until) = (held.clone(), *until);
                    self.fail(format!(
                        "{id} is claimed by {} while {} holds it until {until}",
                        claim.0, held.0
                    ));
                    return;
                }
                self.release(&id, agent);
                self.index
                    .held
                    .insert(id.clone(), (claim.clone(), *until_ms));
                if agent {
                    self.claimed_intent(&id, *until_ms);
                } else {
                    self.claimed_work(&id, pair, parent, *until_ms, entry.at_ms);
                }
            }
            Change::Renewed { claim, until_ms } => {
                self.release(&id, agent);
                self.index
                    .held
                    .insert(id.clone(), (claim.clone(), *until_ms));
                if agent {
                    self.index.intent_leases.insert((*until_ms, id.clone()));
                    if let Some(intent) = self.index.intents.get_mut(&id) {
                        intent.until_ms = Some(*until_ms);
                    }
                } else {
                    self.index.lapses.insert((*until_ms, id.clone()));
                    self.index.lapse_acts.push((id, *until_ms));
                }
            }
            Change::Released { .. } => {
                self.release(&id, agent);
                if !agent {
                    self.readied(&id, entry.at_ms);
                }
            }
            Change::Finished(_) | Change::Cancelled { .. } => {
                self.release(&id, agent);
                if agent {
                    self.ended_intent(&id);
                } else {
                    let (pair, key) = self.index.keys[&id];
                    self.index.ready[pair].remove(&(key, id.clone()));
                    if is_goal(&id) {
                        self.index.goals_done += 1;
                    }
                }
            }
            Change::Ranked { rank } => {
                if let Some((pair, key)) = self.index.keys.get(&id).copied() {
                    let new = (rank.is_none(), rank.unwrap_or(0), key.2);
                    if self.index.ready[pair].remove(&(key, id.clone())) {
                        self.index.ready[pair].insert((new, id.clone()));
                    }
                    self.index.keys.insert(id, (pair, new));
                }
            }
            _ => {}
        }
    }

    fn posted(
        &mut self,
        id: TaskId,
        pair: usize,
        agent: bool,
        intent: Option<String>,
        rank: Option<u64>,
        entry: &Entry,
    ) {
        if !self.index.posted.insert(id.clone()) {
            self.fail(format!("{id} was created twice"));
            return;
        }
        if agent {
            self.posted_intent(id, pair, intent, entry.at_ms);
            return;
        }
        let key = (rank.is_none(), rank.unwrap_or(0), entry.seq);
        self.index.keys.insert(id.clone(), (pair, key));
        self.index.ready[pair].insert((key, id.clone()));
        self.index.ready_since.insert(id.clone(), entry.at_ms);
        self.index.woken.push(pair);
        if is_goal(&id) {
            self.index.goal_posted.insert(id, entry.at_ms);
            self.stats.goals += 1;
        } else {
            self.stats.tasks += 1;
        }
    }

    fn release(&mut self, id: &TaskId, agent: bool) {
        if let Some((_, until)) = self.index.held.remove(id) {
            if agent {
                self.index.intent_leases.remove(&(until, id.clone()));
            } else {
                self.index.lapses.remove(&(until, id.clone()));
            }
        }
    }

    fn readied(&mut self, id: &TaskId, at_ms: u64) {
        let (pair, key) = self.index.keys[id];
        self.index.ready[pair].insert((key, id.clone()));
        self.index.ready_since.insert(id.clone(), at_ms);
        self.index.woken.push(pair);
    }

    fn posted_intent(&mut self, id: TaskId, pair: usize, intent: Option<String>, at_ms: u64) {
        let intent = intent.expect("an agent task names its intent");
        if !self.index.intent_labels.insert(intent.clone()) {
            self.fail(format!(
                "spawn intent {intent} was created twice, again as {id}"
            ));
            return;
        }
        let k = intent
            .rsplit_once('/')
            .and_then(|(_, k)| k.parse::<u64>().ok())
            .expect("an intent ends in its number");
        self.index.spawned[pair] = self.index.spawned[pair].max(k + 1);
        let (member, project) = (self.shape.member(pair), self.shape.project(pair));
        self.index.ledger_member[member] += 1;
        self.index.ledger_project[project] += 1;
        self.index.ledger_usd += self.shape.prices[project];
        self.index.open_intents.insert((at_ms, id.clone()));
        self.index.intents.insert(
            id,
            Intent {
                pair,
                posted_ms: at_ms,
                until_ms: None,
            },
        );
        self.stats.spawns += 1;
    }

    fn claimed_intent(&mut self, id: &TaskId, until_ms: u64) {
        let Some(intent) = self.index.intents.get_mut(id) else {
            return;
        };
        let first = intent.until_ms.is_none();
        intent.until_ms = Some(until_ms);
        let (pair, posted_ms) = (intent.pair, intent.posted_ms);
        if first {
            let (member, project) = (self.shape.member(pair), self.shape.project(pair));
            self.index.claimed_member[member] += 1;
            self.index.claimed_project[project] += 1;
            self.index.claimed_usd += self.shape.prices[project];
            self.index.open_intents.remove(&(posted_ms, id.clone()));
        }
        self.index.intent_leases.insert((until_ms, id.clone()));
    }

    fn ended_intent(&mut self, id: &TaskId) {
        let Some(intent) = self.index.intents.remove(id) else {
            return;
        };
        let (member, project) = (
            self.shape.member(intent.pair),
            self.shape.project(intent.pair),
        );
        let price = self.shape.prices[project];
        self.index.ledger_member[member] -= 1;
        self.index.ledger_project[project] -= 1;
        self.index.ledger_usd -= price;
        if intent.until_ms.is_some() {
            self.index.claimed_member[member] -= 1;
            self.index.claimed_project[project] -= 1;
            self.index.claimed_usd -= price;
        } else {
            self.index
                .open_intents
                .remove(&(intent.posted_ms, id.clone()));
        }
    }

    fn claimed_work(
        &mut self,
        id: &TaskId,
        pair: usize,
        parent: Option<TaskId>,
        until_ms: u64,
        at_ms: u64,
    ) {
        let (_, key) = self.index.keys[id];
        if !self.index.ready[pair].remove(&(key, id.clone())) {
            self.stats.steals += 1;
        }
        if let Some(since) = self.index.ready_since.remove(id) {
            self.claim_ms.push(at_ms.saturating_sub(since));
        }
        if let Some(goal) = parent
            && self.index.goal_first.insert(goal.clone())
            && let Some(posted) = self.index.goal_posted.get(&goal)
        {
            self.goal_ms.push(at_ms.saturating_sub(*posted));
        }
        self.index.lapses.insert((until_ms, id.clone()));
        self.index.lapse_acts.push((id.clone(), until_ms));
        self.stats.claims += 1;
    }

    fn promote(&mut self) {
        while let Some((until, id)) = self.index.lapses.first().cloned()
            && until <= self.now_ms
        {
            self.index.lapses.pop_first();
            self.readied(&id, until);
        }
        for pair in std::mem::take(&mut self.index.woken) {
            self.wake(pair);
        }
    }

    fn wake(&mut self, pair: usize) {
        if self.mutation == ControlplaneMutation::WakeDropped {
            return;
        }
        if let Some(agent) = self.idle[pair].pop_first() {
            let delay = self.rng.range(1, self.shape.wake_ms_max);
            self.at(delay, Act::Wake(agent));
        }
    }

    fn seek(&mut self, agent: usize) {
        self.promote();
        let pair = self.procs[agent].pair;
        self.idle[pair].remove(&agent);
        let ready = &self.index.ready[pair];
        if ready.is_empty() {
            let since_ms = self.now_ms;
            self.procs[agent].life = Life::Idle { since_ms };
            self.idle[pair].insert(agent);
            let delay = self.shape.idle_retire_ms;
            self.at(delay, Act::Retire { agent, since_ms });
            return;
        }
        let width = usize::try_from(self.shape.width)
            .expect("a width fits in usize")
            .min(ready.len());
        let choice = self.rng.pick(width);
        let task = ready
            .iter()
            .nth(choice)
            .map(|(_, id)| id.clone())
            .expect("a choice within the ready set");
        if self.rng.chance(self.shape.pick_check_ppm) {
            self.check_pick(agent, pair);
        }
        self.procs[agent].life = Life::Seeking;
        let lease_ms = self.shape.work_lease_ms;
        self.agent_send(
            agent,
            Op::Claim {
                task: task.clone(),
                lease_ms,
                fence: None,
                cap: None,
            },
            Then::Claim(task),
        );
    }

    fn check_pick(&mut self, agent: usize, pair: usize) {
        self.stats.pick_checks += 1;
        let filter = labels(&[("pool", "work".to_owned()), ("pair", pair.to_string())]);
        let best = self.index.ready[pair].first().map(|(_, id)| id.clone());
        let picked = self
            .fold
            .pick(&self.procs[agent].writer, &filter, self.now_ms)
            .cloned();
        if picked != best {
            self.fail(format!(
                "the board picks {picked:?} for pair {pair}, the best-ranked ready task is {best:?}"
            ));
        }
    }

    fn die(&mut self, agent: usize) {
        let pair = self.procs[agent].pair;
        self.procs[agent].life = Life::Gone;
        self.idle[pair].remove(&agent);
        let (member, project) = (self.shape.member(pair), self.shape.project(pair));
        self.alive_member[member] -= 1;
        self.alive_project[project] -= 1;
        self.alive_usd -= self.shape.prices[project];
        self.alive -= 1;
        if !self.index.ready[pair].is_empty() {
            self.wake(pair);
        }
    }

    fn heard(&mut self, agent: usize, then: Then, verdict: Result<u64, Rejection>, claim: ClaimId) {
        if self.rng.chance(self.shape.crash_ppm) {
            self.stats.crashes += 1;
            self.die(agent);
            return;
        }
        match then {
            Then::Own | Then::Renew => match verdict {
                Ok(_) => {
                    let starting = self.procs[agent].own.is_none();
                    if starting {
                        self.procs[agent].own = Some(claim);
                    }
                    let delay = self.shape.own_lease_ms / 2;
                    self.at(delay, Act::Renew(agent));
                    if starting {
                        self.seek(agent);
                    }
                }
                Err(_) => self.die(agent),
            },
            Then::Retire => {
                self.stats.retired += 1;
                self.die(agent);
            }
            Then::Claim(task) => match verdict {
                Ok(_) => {
                    let mut work = self.rng.range(1, self.shape.work_ms_max);
                    if self.rng.chance(self.shape.pause_ppm) {
                        work += self.shape.work_lease_ms;
                    }
                    self.procs[agent].life = Life::Busy {
                        task,
                        claim: claim.clone(),
                    };
                    self.at(work, Act::Work { agent, claim });
                }
                Err(_) => {
                    self.stats.races_lost += 1;
                    self.seek(agent);
                }
            },
            Then::Finish(task) => self.finished(agent, task, verdict.is_ok()),
            Then::Child { goal, index } => self.child_posted(agent, goal, index, verdict),
            Then::ChildRank { goal, index } => self.plan(agent, goal, index + 1),
            Then::Goal(_)
            | Then::Ranked
            | Then::Dupe { .. }
            | Then::Spawn { .. }
            | Then::Cancel(_) => {
                unreachable!("an agent sent a member or dispatcher write")
            }
        }
    }

    fn finished(&mut self, agent: usize, task: TaskId, ok: bool) {
        if !ok {
            self.stats.fenced += 1;
        }
        let early = self.mutation == ControlplaneMutation::GoalFinishedEarly;
        match self.procs[agent].life.clone() {
            Life::Busy { claim, .. } if early && ok && is_goal(&task) => {
                self.procs[agent].life = Life::Planning { claim };
                self.plan(agent, task, 0);
            }
            _ => self.seek(agent),
        }
    }

    fn child_posted(
        &mut self,
        agent: usize,
        goal: TaskId,
        index: u64,
        verdict: Result<u64, Rejection>,
    ) {
        match verdict {
            Ok(_) | Err(Rejection::Exists) => {}
            Err(error) => {
                self.fail(format!("{goal}'s task {index} was refused: {error:?}"));
                return;
            }
        }
        let child = TaskId(format!("{}/t{index}", goal.0));
        match self.child_rank(&goal, index) {
            Some(rank) => self.agent_send(
                agent,
                Op::Rank {
                    task: child,
                    rank: Some(rank),
                },
                Then::ChildRank { goal, index },
            ),
            None => self.plan(agent, goal, index + 1),
        }
    }

    fn child_rank(&self, goal: &TaskId, index: u64) -> Option<u64> {
        let mut rng = Rng::derive(fnv(FNV_OFFSET, goal.0.as_bytes()) ^ index, "child rank");
        rng.chance(self.shape.rank_ppm).then(|| rng.range(1, 100))
    }

    fn plan(&mut self, agent: usize, goal: TaskId, index: u64) {
        if index == children(&goal) {
            let Life::Planning { claim } = self.procs[agent].life.clone() else {
                unreachable!("a planner plans while planning");
            };
            if self.mutation == ControlplaneMutation::GoalFinishedEarly {
                self.seek(agent);
                return;
            }
            self.procs[agent].life = Life::Busy {
                task: goal.clone(),
                claim: claim.clone(),
            };
            self.agent_send(
                agent,
                Op::Finish {
                    task: goal.clone(),
                    claim,
                    outcome: Outcome::Done(format!("{goal} planned")),
                },
                Then::Finish(goal),
            );
            return;
        }
        let pair = self.procs[agent].pair;
        let child = NewTask {
            id: TaskId(format!("{}/t{index}", goal.0)),
            parent: Some(goal.clone()),
            spec: format!("task {index} of {goal}"),
            labels: labels(&[
                ("kind", "task".to_owned()),
                ("pool", "work".to_owned()),
                ("pair", pair.to_string()),
            ]),
            needs: Vec::new(),
            terms: Default::default(),
        };
        self.agent_send(agent, Op::Post { task: child }, Then::Child { goal, index });
    }

    fn work(&mut self, agent: usize, claim: ClaimId) {
        let Life::Busy { task, claim: held } = self.procs[agent].life.clone() else {
            return;
        };
        if held != claim {
            return;
        }
        if is_goal(&task) && self.mutation != ControlplaneMutation::GoalFinishedEarly {
            self.procs[agent].life = Life::Planning { claim };
            self.plan(agent, task, 0);
            return;
        }
        self.agent_send(
            agent,
            Op::Finish {
                task: task.clone(),
                claim,
                outcome: Outcome::Done(format!("{task} done")),
            },
            Then::Finish(task),
        );
    }

    fn renew(&mut self, agent: usize) {
        let proc = &self.procs[agent];
        if matches!(proc.life, Life::Gone | Life::Retiring) {
            return;
        }
        let Some(claim) = proc.own.clone() else {
            return;
        };
        let task = proc.intent.clone();
        let lease_ms = self.shape.own_lease_ms;
        self.agent_send(
            agent,
            Op::Renew {
                task,
                claim,
                lease_ms,
            },
            Then::Renew,
        );
    }

    fn retire(&mut self, agent: usize, since_ms: u64) {
        if self.procs[agent].life != (Life::Idle { since_ms }) {
            return;
        }
        let pair = self.procs[agent].pair;
        if !self.index.ready[pair].is_empty() {
            self.seek(agent);
            return;
        }
        self.idle[pair].remove(&agent);
        self.procs[agent].life = Life::Retiring;
        let task = self.procs[agent].intent.clone();
        let claim = self.procs[agent]
            .own
            .clone()
            .expect("an idle agent holds its own claim");
        self.agent_send(
            agent,
            Op::Finish {
                task,
                claim,
                outcome: Outcome::Done("retired idle".to_owned()),
            },
            Then::Retire,
        );
    }

    fn launch(&mut self, intent: TaskId, pair: usize) {
        self.launching[pair] -= 1;
        let (member, project) = (self.shape.member(pair), self.shape.project(pair));
        self.alive_member[member] += 1;
        self.alive_project[project] += 1;
        self.alive_usd += self.shape.prices[project];
        self.alive += 1;
        self.stats.launches += 1;
        self.stats.alive_max = self.stats.alive_max.max(self.alive);
        if self.alive_member[member] > self.shape.member_max {
            self.fail(format!(
                "member {member} runs {} agents, over its budget of {}",
                self.alive_member[member], self.shape.member_max
            ));
        }
        if self.alive_project[project] > self.shape.project_max {
            self.fail(format!(
                "project {project} runs {} agents, over its budget of {}",
                self.alive_project[project], self.shape.project_max
            ));
        }
        if self.alive_usd > self.shape.usd_max {
            self.fail(format!(
                "agents burn {} a tick, over the budget of {}",
                self.alive_usd, self.shape.usd_max
            ));
        }
        let agent = self.procs.len();
        self.procs.push(Proc {
            writer: format!("{OWNER}+a{agent}"),
            pair,
            intent: intent.clone(),
            minted: 0,
            own: None,
            life: Life::Starting,
        });
        let lease_ms = self.shape.own_lease_ms;
        self.agent_send(
            agent,
            Op::Claim {
                task: intent,
                lease_ms,
                fence: None,
                cap: None,
            },
            Then::Own,
        );
    }

    fn storm(&mut self, storm: u64) {
        let per = self.shape.goals.div_ceil(self.shape.storms);
        let first = storm * per;
        let last = (first + per).min(self.shape.goals);
        for goal in first..last {
            let pair = usize::try_from(self.rng.below(self.shape.pairs() as u64))
                .expect("a pair fits in usize");
            let k = self.rng.range(1, self.shape.children_max);
            let task = NewTask {
                id: TaskId(format!("g{goal}k{k}")),
                parent: None,
                spec: format!("goal {goal}"),
                labels: labels(&[
                    ("kind", "goal".to_owned()),
                    ("pool", "work".to_owned()),
                    ("pair", pair.to_string()),
                ]),
                needs: Vec::new(),
                terms: Default::default(),
            };
            let by = format!("{OWNER}+m{}", self.shape.member(pair));
            let id = EventId(format!("{by}#{}", task.id.0));
            let goal = task.id.clone();
            self.send(Who::Member, id, &by, Op::Post { task }, Then::Goal(goal));
        }
        self.storms_posted += 1;
        if self.storms_posted < self.shape.storms {
            let gap = self.shape.storm_gap_ms;
            self.at(gap, Act::Storm(storm + 1));
        }
    }

    fn member_heard(&mut self, then: Then, verdict: Result<u64, Rejection>) {
        match then {
            Then::Goal(goal) => {
                let Ok(seq) = verdict else {
                    self.fail(format!("goal {goal} was refused: {verdict:?}"));
                    return;
                };
                self.storm_retries(goal, seq);
            }
            Then::Ranked => {}
            Then::Dupe { conflict, seq } => match (conflict, verdict) {
                (false, Ok(again)) if again == seq => {}
                (true, Err(Rejection::Exists)) => {}
                (conflict, verdict) => self.fail(format!(
                    "a retried goal post (conflicting {conflict}) got {verdict:?}, the first landed at {seq}"
                )),
            },
            _ => unreachable!("a member sent an agent or dispatcher write"),
        }
    }

    fn storm_retries(&mut self, goal: TaskId, seq: u64) {
        let spec = self.fold.task(&goal).expect("a posted goal").spec.clone();
        let pair = self.index.keys.get(&goal).map_or(0, |(pair, _)| *pair);
        let by = format!("{OWNER}+m{}", self.shape.member(pair));
        self.send(
            Who::Member,
            EventId(format!("{by}#{}-rank", goal.0)),
            &by,
            Op::Rank {
                task: goal.clone(),
                rank: Some(0),
            },
            Then::Ranked,
        );
        while self.rng.chance(self.shape.resend_ppm) {
            self.stats.dupes += 1;
            let fresh = self.rng.chance(PPM / 2);
            let conflict = fresh && self.rng.chance(PPM / 2);
            let mut task = spec.clone();
            if conflict {
                task.spec.push_str(" again");
            }
            let id = if fresh {
                self.member_minted += 1;
                EventId(format!("{by}.{}", self.member_minted))
            } else {
                EventId(format!("{by}#{}", goal.0))
            };
            self.send(
                Who::Member,
                id,
                &by,
                Op::Post { task },
                Then::Dupe { conflict, seq },
            );
        }
    }

    fn dispatcher_heard(&mut self, then: Then, verdict: Result<u64, Rejection>) {
        match then {
            Then::Spawn { task, pair } => {
                self.dispatch.inflight.remove(&task);
                if verdict.is_ok() {
                    self.launching[pair] += 1;
                    let delay = self.rng.range(1, self.shape.spawn_ms_max);
                    self.at(delay, Act::Launch { intent: task, pair });
                }
            }
            Then::Cancel(task) => {
                self.dispatch.cancelling.remove(&task);
                if verdict.is_ok() {
                    self.stats.reaped += 1;
                }
            }
            _ => unreachable!("the dispatcher sent an agent or member write"),
        }
    }

    fn census(&self) -> (Vec<u64>, Vec<u64>, Vec<u64>) {
        let mut idle = vec![0; self.shape.pairs()];
        let mut coming = self.launching.clone();
        let mut seeking = vec![0; self.shape.pairs()];
        for proc in &self.procs {
            match proc.life {
                Life::Idle { .. } => idle[proc.pair] += 1,
                Life::Starting => coming[proc.pair] += 1,
                Life::Seeking => {
                    coming[proc.pair] += 1;
                    seeking[proc.pair] += 1;
                }
                _ => {}
            }
        }
        (idle, coming, seeking)
    }

    fn headroom(&self, pair: usize, inflight: &[u64]) -> u64 {
        let (member, project) = (self.shape.member(pair), self.shape.project(pair));
        let skips = self.mutation == ControlplaneMutation::BudgetSkipsInflight;
        let (mut members, mut projects, mut usd) = if skips {
            (
                self.index.claimed_member[member],
                self.index.claimed_project[project],
                self.index.claimed_usd,
            )
        } else {
            (
                self.index.ledger_member[member],
                self.index.ledger_project[project],
                self.index.ledger_usd,
            )
        };
        if !skips {
            for (other, count) in inflight.iter().enumerate() {
                if self.shape.member(other) == member {
                    members += count;
                }
                if self.shape.project(other) == project {
                    projects += count;
                }
                usd += count * self.shape.prices[self.shape.project(other)];
            }
        }
        self.shape
            .member_max
            .saturating_sub(members)
            .min(self.shape.project_max.saturating_sub(projects))
            .min(self.shape.usd_max.saturating_sub(usd) / self.shape.prices[project])
    }

    fn inflight(&self) -> Vec<u64> {
        let mut inflight = vec![0; self.shape.pairs()];
        if self.dispatch.up {
            for pair in self.dispatch.inflight.values() {
                inflight[*pair] += 1;
            }
        }
        inflight
    }

    fn round(&mut self) {
        let delay = self.shape.round_ms;
        self.at(delay, Act::Round);
        if !self.dispatch.up {
            return;
        }
        self.promote();
        self.reap();
        if self.rng.chance(self.shape.dispatch_crash_ppm) {
            self.dispatch.crash_after = Some(self.rng.below(CRASH_SPAWNS_MAX));
        }
        let (idle, coming, seeking) = self.census();
        for pair in 0..self.shape.pairs() {
            let mut inflight = self.inflight();
            let ready = self.index.ready[pair].len() as u64;
            for _ in 0..ready.saturating_sub(seeking[pair]).min(idle[pair]) {
                self.wake(pair);
            }
            let need = ready.saturating_sub(idle[pair] + coming[pair] + inflight[pair]);
            for _ in 0..need {
                if !self.dispatch.up || self.headroom(pair, &inflight) == 0 {
                    break;
                }
                self.spawn(pair);
                inflight[pair] += 1;
            }
        }
    }

    fn reap(&mut self) {
        let now = self.now_ms;
        let timeout = self.shape.start_timeout_ms;
        let lapsed = self
            .index
            .intent_leases
            .iter()
            .take_while(|(until, _)| *until <= now)
            .map(|(_, id)| id.clone());
        let stale = self
            .index
            .open_intents
            .iter()
            .take_while(|(at, _)| at + timeout <= now)
            .map(|(_, id)| id.clone());
        let gone = lapsed.chain(stale).collect::<Vec<_>>();
        for task in gone {
            if self.dispatch.inflight.contains_key(&task)
                || !self.dispatch.cancelling.insert(task.clone())
            {
                continue;
            }
            let epoch = self.dispatch.epoch;
            let id = EventId(format!("{DISPATCHER}#cancel-{}-e{epoch}", task.0));
            self.send(
                Who::Dispatcher(epoch),
                id,
                DISPATCHER,
                Op::Cancel {
                    task: task.clone(),
                    reason: "agent gone".to_owned(),
                },
                Then::Cancel(task),
            );
        }
    }

    fn spawn(&mut self, pair: usize) {
        if let Some(after) = self.dispatch.crash_after {
            if after == 0 {
                self.crash();
                return;
            }
            self.dispatch.crash_after = Some(after - 1);
        }
        let k = self
            .dispatch
            .next
            .get(&pair)
            .copied()
            .unwrap_or(0)
            .max(self.index.spawned[pair]);
        self.dispatch.next.insert(pair, k + 1);
        let intent = format!("{pair}/{k}");
        let task = TaskId(format!("agents/{intent}"));
        let new = NewTask {
            id: task.clone(),
            parent: None,
            spec: format!("agent for pair {pair}"),
            labels: labels(&[
                ("kind", "agent".to_owned()),
                ("pool", "agent".to_owned()),
                ("pair", pair.to_string()),
                ("intent", intent),
            ]),
            needs: Vec::new(),
            terms: Default::default(),
        };
        self.dispatch.inflight.insert(task.clone(), pair);
        let epoch = self.dispatch.epoch;
        let id = EventId(format!("{DISPATCHER}#{}", task.0));
        self.send(
            Who::Dispatcher(epoch),
            id,
            DISPATCHER,
            Op::Post { task: new },
            Then::Spawn { task, pair },
        );
    }

    fn crash(&mut self) {
        self.stats.dispatcher_crashes += 1;
        self.dispatch.up = false;
        self.dispatch.epoch += 1;
        self.dispatch.inflight.clear();
        self.dispatch.cancelling.clear();
        self.dispatch.next.clear();
        self.dispatch.crash_after = None;
        let delay = self.shape.restart_ms;
        self.at(delay, Act::Restart);
    }

    fn audit(&mut self) {
        let delay = self.shape.round_ms;
        self.at(delay, Act::Audit);
        self.promote();
        let (idle, coming, seeking) = self.census();
        let inflight = self.inflight();
        let round = self.shape.round_ms;
        let bound = STRANDED_ROUNDS_MAX * round;
        let warm = self.dispatch.up && self.now_ms >= self.dispatch.up_since_ms + round;
        let mut any = false;
        for pair in 0..self.shape.pairs() {
            let ready = self.index.ready[pair].len() as u64;
            let oldest = self.index.ready[pair]
                .iter()
                .filter_map(|(_, id)| self.index.ready_since.get(id))
                .min()
                .map_or(0, |since| self.now_ms.saturating_sub(*since));
            if ready > seeking[pair] && idle[pair] > 0 {
                any = true;
                self.streak_idle[pair] = oldest;
            } else {
                self.streak_idle[pair] = 0;
            }
            let unserved = ready > idle[pair] + coming[pair] + inflight[pair];
            if warm && unserved && self.headroom(pair, &inflight) > 0 {
                self.streak_unserved[pair] += round;
            } else {
                self.streak_unserved[pair] = 0;
            }
            if self.streak_idle[pair] > bound {
                self.fail(format!(
                    "pair {pair} has {ready} ready tasks, the oldest ready for {1} ms, while {0} agents sit idle",
                    idle[pair], self.streak_idle[pair]
                ));
            }
            if self.streak_unserved[pair] > bound {
                self.fail(format!(
                    "pair {pair} has {ready} ready tasks and budget left but nothing spawned for {} ms",
                    self.streak_unserved[pair]
                ));
            }
        }
        if any {
            self.idle_open_ms += round;
        }
        self.settle();
    }

    fn settle(&mut self) {
        let planning = self
            .procs
            .iter()
            .any(|proc| matches!(proc.life, Life::Planning { .. }));
        let busy = self.index.ready.iter().any(|ready| !ready.is_empty())
            || !self.index.lapses.is_empty()
            || self
                .index
                .held
                .keys()
                .any(|id| !id.0.starts_with("agents/"));
        self.settled = self.storms_posted == self.shape.storms
            && self.index.goals_done == self.shape.goals
            && !planning
            && !busy;
    }

    fn lapse(&mut self, task: &TaskId, until_ms: u64) {
        if self
            .index
            .held
            .get(task)
            .is_some_and(|(_, until)| *until == until_ms)
        {
            self.promote();
        }
    }

    fn step(&mut self, act: Act) {
        match act {
            Act::Arrive(write) => self.arrive(write),
            Act::Reply(write, applied) => self.reply(write, applied),
            Act::Launch { intent, pair } => self.launch(intent, pair),
            Act::Renew(agent) => self.renew(agent),
            Act::Work { agent, claim } => self.work(agent, claim),
            Act::Wake(agent) => {
                if matches!(self.procs[agent].life, Life::Idle { .. }) {
                    self.seek(agent);
                } else {
                    self.wake(self.procs[agent].pair);
                }
            }
            Act::Retire { agent, since_ms } => self.retire(agent, since_ms),
            Act::Lapse { task, until_ms } => self.lapse(&task, until_ms),
            Act::Storm(storm) => self.storm(storm),
            Act::Round => self.round(),
            Act::Audit => self.audit(),
            Act::Restart => {
                self.dispatch.up = true;
                self.dispatch.up_since_ms = self.now_ms;
            }
        }
    }

    fn charter(&mut self) {
        let event = Event {
            id: EventId(format!("{OWNER}.0")),
            by: OWNER.to_owned(),
            at_ms: 0,
            op: Op::Charter {},
        };
        let body = serde_json::to_string(&event).expect("a board event encodes");
        let seq = self.fold.next();
        self.fold.apply(seq, &body).expect("the charter applies");
        assert!(
            matches!(self.fold.outcome(&event.id), Some(Ok(_))),
            "the owner charters the board"
        );
    }

    fn run(&mut self) {
        self.charter();
        self.at(0, Act::Storm(0));
        self.at(0, Act::Round);
        self.at(0, Act::Audit);
        let settle_ms_max = self.shape.storms * self.shape.storm_gap_ms
            + SETTLE_LEASES_MAX * (self.shape.work_lease_ms + self.shape.own_lease_ms);
        while let Some(((at, _), act)) = self.queue.pop_first() {
            self.now_ms = at;
            self.step(act);
            if self.wrong.is_some() || self.settled {
                break;
            }
            if self.now_ms > settle_ms_max {
                self.fail(format!(
                    "the board did not settle within {settle_ms_max} ms"
                ));
                break;
            }
        }
        if self.wrong.is_none() {
            self.finale();
        }
    }

    fn finale(&mut self) {
        for (id, task) in self.fold.tasks() {
            if id.0.starts_with("agents/") {
                continue;
            }
            if !matches!(task.phase, Phase::Done { .. }) {
                self.wrong = Some(format!("{id} ended {:?}", task.phase));
                return;
            }
            if is_goal(id) {
                let made = self.fold.children(id).count() as u64;
                if made != children(id) {
                    self.wrong = Some(format!(
                        "goal {id} is done with {made} of its {} tasks",
                        children(id)
                    ));
                    return;
                }
            }
        }
    }

    fn score(&self, wall_ms: u64) -> String {
        let p99 = |samples: &[u64]| {
            let mut sorted = samples.to_vec();
            sorted.sort_unstable();
            let at = (sorted.len() * 99 / 100).min(sorted.len().saturating_sub(1));
            sorted.get(at).copied().unwrap_or(0)
        };
        let budget = (self.shape.member_max * self.shape.members)
            .min(self.shape.project_max * self.shape.projects);
        format!(
            "SCORE controlplane simulated board_events={} board_events_per_wall_s={} claim_p99_sim_ms={} goal_to_first_claim_p99_sim_ms={} alive_max={} alive_budget={} idle_with_open_work_sim_ms={} sim_ms={} wall_ms={wall_ms}",
            self.stats.events,
            self.stats.events * 1_000 / wall_ms.max(1),
            p99(&self.claim_ms),
            p99(&self.goal_ms),
            self.stats.alive_max,
            budget,
            self.idle_open_ms,
            self.now_ms
        )
    }
}

fn simulate(seed: u64, agents: u64, mutation: ControlplaneMutation) -> ControlplaneOutcome {
    board_sim::mutate(mutation.rule());
    let started = Instant::now();
    let mut sim = Sim::new(seed, agents, mutation);
    sim.run();
    board_sim::mutate(BoardRule::None);
    let wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    sim.stats.wall_ms = wall_ms;
    let digest = fnv(sim.digest, &sim.stats.events.to_le_bytes());
    ControlplaneOutcome {
        digest,
        wrong: sim.wrong.clone(),
        score: sim.score(wall_ms),
        stats: sim.stats.clone(),
        shape: sim.shape.describe(),
    }
}

fn run_seed(seed: u64, agents: u64, mutation: ControlplaneMutation) -> ControlplaneOutcome {
    std::panic::catch_unwind(|| simulate(seed, agents, mutation)).unwrap_or_else(|panic| {
        board_sim::mutate(BoardRule::None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        ControlplaneOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: ControlplaneStats::default(),
            shape: String::new(),
            score: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    agents: Option<u64>,
    mutation: ControlplaneMutation,
    twice: bool,
    verbose: bool,
    totals: &mut ControlplaneStats,
) -> Option<String> {
    let agents = agents.unwrap_or(AGENTS_DEFAULT).max(1);
    let first = run_seed(seed, agents, mutation);
    totals.add(&first.stats);
    println!("{} seed={seed}", first.score);
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
    let second = run_seed(seed, agents, mutation);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, agents: Option<u64>, mutation: ControlplaneMutation) -> String {
    let mut command = format!("nanocode-sim --scenario controlplane --seed {seed}");
    if let Some(agents) = agents {
        command.push_str(&format!(" --ops {agents}"));
    }
    if mutation != ControlplaneMutation::None {
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
    mutation: ControlplaneMutation,
    totals: &ControlplaneStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=controlplane mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        ControlplaneMutation::None if failed == 0 => ExitCode::SUCCESS,
        ControlplaneMutation::None => ExitCode::FAILURE,
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
