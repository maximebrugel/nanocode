use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    process::ExitCode,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use nanocode_board::{Board, ClaimId, NewTask, Op, Outcome, Phase, TaskId};
use nanocode_dispatch::{Agent, AgentPhase, Budgets, Domain, Policy, Role};
use nanocode_dispatcher::{
    BRIEF, Dispatcher, Pool, PoolError, PoolFuture, Start,
    sim::{self as dispatcher_sim, Mutation as DispatcherRule},
};
use nanocode_turnlog::{LogHost, MemoryLog};

use crate::{
    board::identity,
    rng::{FNV_OFFSET, Rng, fnv},
};

const SCOPE: &str = "dispatcher";
const OWNER: &str = "agent:sim";
const POSTER: &str = "agent:sim+poster";
const KIND: &str = "kind";
const MEMBER: &str = "member";
const PROJECT: &str = "project";
const LEASE_MS: u64 = 3_600_000;
const POSTING_STEPS: u64 = 120;
const SETTLE_STEPS: u64 = 4_000;
const STEP_MS_MAX: u64 = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DispatcherMutation(DispatcherRule);

impl DispatcherMutation {
    pub const NONE: Self = Self(DispatcherRule::None);

    pub fn parse(name: &str) -> Option<Self> {
        DispatcherRule::ALL
            .into_iter()
            .find(|rule| rule.name() == name)
            .map(Self)
    }

    pub const fn name(self) -> &'static str {
        self.0.name()
    }

    pub fn names() -> String {
        DispatcherRule::ALL
            .iter()
            .skip(1)
            .map(|rule| rule.name())
            .collect::<Vec<_>>()
            .join("|")
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DispatcherStats {
    pub seeds: u64,
    pub tasks: u64,
    pub done: u64,
    pub starts: u64,
    pub retires: u64,
    pub reaped: u64,
    pub claims_released: u64,
    pub briefs_blocked: u64,
    pub crashes: u64,
    pub lost_starts: u64,
    pub hangs: u64,
    pub passes: u64,
    pub parked_ms: u64,
}

impl DispatcherStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.tasks += other.tasks;
        self.done += other.done;
        self.starts += other.starts;
        self.retires += other.retires;
        self.reaped += other.reaped;
        self.claims_released += other.claims_released;
        self.briefs_blocked += other.briefs_blocked;
        self.crashes += other.crashes;
        self.lost_starts += other.lost_starts;
        self.hangs += other.hangs;
        self.passes += other.passes;
        self.parked_ms += other.parked_ms;
    }
}

struct Bot {
    agent: Agent,
    worker: String,
    ready_at: u64,
    work: Option<(TaskId, ClaimId, u64)>,
    hung: bool,
}

struct Shared {
    rng: Rng,
    now: u64,
    bots: Vec<Bot>,
    epoch: u64,
    fail_ppm: u64,
    lost_ppm: u64,
    crash_pending: bool,
    retired: Vec<(String, u64)>,
    wrong: Vec<String>,
    stats: DispatcherStats,
}

struct SimPool(Mutex<Shared>);

impl SimPool {
    fn borrow(&self) -> MutexGuard<'_, Shared> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Pool for SimPool {
    fn agents(&self) -> PoolFuture<'_, Vec<Agent>> {
        let agents = self.borrow().bots.iter().map(|bot| bot.agent.clone()).collect();
        Box::pin(std::future::ready(agents))
    }

    fn start(&self, start: Start) -> PoolFuture<'_, Result<(), PoolError>> {
        let mut guard = self.borrow();
        let shared = &mut *guard;
        let intent = start.spawn.intent.clone();
        if start.worker != format!("{OWNER}+{}", start.agent) {
            let message = format!("{intent} started {} as {}", start.agent, start.worker);
            shared.wrong.push(message);
        }
        if let Some(bot) = shared
            .bots
            .iter()
            .find(|bot| bot.agent.intent.as_ref() == Some(&intent))
        {
            let message = format!("intent {intent} started twice while {} lives", bot.agent.id);
            shared.wrong.push(message);
        }
        if start.brief.task != start.spawn.task || start.brief.tokens > BRIEF.tokens_max {
            let message = format!("{intent} started without its own brief within policy");
            shared.wrong.push(message);
        }
        if shared.rng.chance(shared.fail_ppm) {
            return Box::pin(std::future::ready(Err(PoolError::Placement(
                "no capacity".to_owned(),
            ))));
        }
        let lost = shared.rng.chance(shared.lost_ppm);
        let placed = !lost || shared.rng.chance(500_000);
        if placed {
            let id = start.agent.clone();
            let now = shared.now;
            let ready_at = now + shared.rng.range(50, 2_000);
            shared.bots.push(Bot {
                agent: Agent {
                    id,
                    domain: start.spawn.domain.clone(),
                    cost_nano_usd: start.spawn.cost_nano_usd,
                    intent: Some(intent),
                    phase: AgentPhase::Starting,
                },
                worker: start.worker,
                ready_at,
                work: None,
                hung: false,
            });
            shared.epoch += 1;
        }
        if lost {
            shared.stats.lost_starts += 1;
            shared.crash_pending = true;
            return Box::pin(std::future::ready(Err(PoolError::Unknown(
                "the start reply was lost".to_owned(),
            ))));
        }
        Box::pin(std::future::ready(Ok(())))
    }

    fn retire<'a>(&'a self, agent: &'a str) -> PoolFuture<'a, Result<(), PoolError>> {
        let mut guard = self.borrow();
        let shared = &mut *guard;
        if shared.rng.chance(shared.fail_ppm / 4) {
            return Box::pin(std::future::ready(Err(PoolError::Unknown(
                "retire timed out".to_owned(),
            ))));
        }
        let before = shared.bots.len();
        shared.bots.retain(|bot| bot.agent.id != agent);
        if shared.bots.len() != before {
            shared.epoch += 1;
            let retired = (agent.to_owned(), shared.now);
            shared.retired.push(retired);
        }
        Box::pin(std::future::ready(Ok(())))
    }
}

struct World {
    host: Arc<dyn LogHost>,
    poster: Board,
    probe: Board,
    workers: BTreeMap<String, Board>,
    pool: SimPool,
    policy: Policy,
    crash_ppm: u64,
    posted: Vec<TaskId>,
    seen: Option<(u64, u64)>,
    wake_at: Option<u64>,
    failures: Vec<String>,
}

fn shown(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

impl World {
    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.pool.borrow()
    }

    async fn dispatcher(&self) -> Result<Dispatcher, String> {
        let board = Board::open(self.host.clone(), SCOPE, identity(OWNER))
            .await
            .map_err(shown)?;
        Ok(Dispatcher::new(board, self.policy.clone(), BRIEF))
    }

    async fn post(&mut self) -> Result<(), String> {
        let (task, now) = {
            let mut shared = self.shared();
            let id = TaskId(format!("t{}", shared.rng.below(1 << 40)));
            let kind = if shared.rng.chance(250_000) { "goal" } else { "code" };
            let labels = [
                (KIND, kind.to_owned()),
                (MEMBER, format!("m{}", shared.rng.below(3))),
                (PROJECT, format!("p{}", shared.rng.below(3))),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
            let bytes = if shared.rng.chance(30_000) { 40_000 } else { 64 };
            let task = NewTask {
                id,
                parent: None,
                spec: "w".repeat(bytes),
                labels,
                needs: Vec::new(),
                terms: Default::default(),
            };
            (task, shared.now)
        };
        let id = task.id.clone();
        self.poster.refresh().await.map_err(shown)?;
        if self.poster.submit(now, Op::Post { task }).await.is_ok() {
            self.posted.push(id);
            self.shared().stats.tasks += 1;
        }
        Ok(())
    }

    fn labels(&self, domain: &Domain) -> BTreeMap<String, String> {
        let mut labels = self
            .policy
            .roles
            .iter()
            .find(|role| role.name == domain.role)
            .map(|role| role.labels.clone())
            .unwrap_or_default();
        labels.insert(MEMBER.to_owned(), domain.member.clone());
        labels.insert(PROJECT.to_owned(), domain.project.clone());
        labels
    }

    async fn worker(&mut self, worker: &str) -> Result<&mut Board, String> {
        if !self.workers.contains_key(worker) {
            let board = Board::open(self.host.clone(), SCOPE, identity(worker))
                .await
                .map_err(shown)?;
            self.workers.insert(worker.to_owned(), board);
        }
        let board = self
            .workers
            .get_mut(worker)
            .ok_or("a worker board vanished")?;
        board.refresh().await.map_err(shown)?;
        Ok(board)
    }

    async fn claim(&mut self, index: usize, worker: &str, domain: &Domain, now: u64) -> Result<(), String> {
        let labels = self.labels(domain);
        let board = self.worker(worker).await?;
        let Some(task) = board.fold().pick(worker, &labels, now).cloned() else {
            return Ok(());
        };
        let claim = Op::Claim {
            task: task.clone(),
            lease_ms: LEASE_MS,
            fence: None,
            cap: None,
        };
        if board.submit(now, claim).await.is_err() {
            return Ok(());
        }
        let Some(Phase::Claimed { claim, .. }) =
            board.fold().task(&task).map(|task| task.phase.clone())
        else {
            return Ok(());
        };
        let mut shared = self.shared();
        let done = now + shared.rng.range(200, 6_000);
        let hang = shared.rng.chance(60_000);
        shared.stats.hangs += u64::from(hang);
        shared.epoch += 1;
        let bot = &mut shared.bots[index];
        bot.work = Some((task, claim, done));
        bot.hung = hang;
        bot.agent.phase = if hang {
            AgentPhase::Idle { since_ms: now }
        } else {
            AgentPhase::Busy
        };
        Ok(())
    }

    async fn finish(&mut self, index: usize, worker: &str, work: (TaskId, ClaimId), now: u64) -> Result<(), String> {
        let (task, claim) = work;
        let board = self.worker(worker).await?;
        let finish = Op::Finish {
            task,
            claim,
            outcome: Outcome::Done("ok".to_owned()),
        };
        let finished = board.submit(now, finish).await.is_ok();
        let mut shared = self.shared();
        shared.stats.done += u64::from(finished);
        shared.epoch += 1;
        let bot = &mut shared.bots[index];
        bot.work = None;
        bot.agent.phase = AgentPhase::Idle { since_ms: now };
        Ok(())
    }

    async fn bots_step(&mut self) -> Result<(), String> {
        let count = self.shared().bots.len();
        for index in 0..count {
            let (phase, worker, ready_at, work, hung, domain, now) = {
                let shared = self.shared();
                let bot = &shared.bots[index];
                (
                    bot.agent.phase,
                    bot.worker.clone(),
                    bot.ready_at,
                    bot.work.clone(),
                    bot.hung,
                    bot.agent.domain.clone(),
                    shared.now,
                )
            };
            match (phase, work) {
                (AgentPhase::Starting, _) if ready_at <= now => {
                    let mut shared = self.shared();
                    shared.bots[index].agent.phase = AgentPhase::Idle { since_ms: now };
                    shared.epoch += 1;
                }
                (AgentPhase::Idle { .. }, None) if !hung => {
                    self.claim(index, &worker, &domain, now).await?;
                }
                (AgentPhase::Busy, Some((task, claim, done))) if done <= now => {
                    self.finish(index, &worker, (task, claim), now).await?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn check_budget(&self) -> Result<(), String> {
        let shared = self.pool.borrow();
        let budgets = &self.policy.budgets;
        let mut members = BTreeMap::<&str, u64>::new();
        let mut projects = BTreeMap::<&str, u64>::new();
        let mut cost = 0;
        for bot in &shared.bots {
            *members.entry(&bot.agent.domain.member).or_default() += 1;
            *projects.entry(&bot.agent.domain.project).or_default() += 1;
            cost += bot.agent.cost_nano_usd;
        }
        if let Some((member, count)) = members
            .iter()
            .find(|(_, count)| **count > budgets.agents_per_member_max)
        {
            return Err(format!("member {member} runs {count} agents over its budget"));
        }
        if let Some((project, count)) = projects
            .iter()
            .find(|(_, count)| **count > budgets.agents_per_project_max)
        {
            return Err(format!("project {project} runs {count} agents over its budget"));
        }
        if cost > budgets.nano_usd_max {
            return Err(format!(
                "agents cost {cost} over the budget {}",
                budgets.nano_usd_max
            ));
        }
        Ok(())
    }

    fn check_claims(&self) -> Result<(), String> {
        let shared = self.pool.borrow();
        let alive = shared
            .bots
            .iter()
            .map(|bot| bot.worker.as_str())
            .collect::<BTreeSet<_>>();
        let workers = format!("{OWNER}+");
        for (id, task) in self.probe.fold().tasks() {
            if let Phase::Claimed { worker, .. } = &task.phase
                && worker.starts_with(&workers)
                && !alive.contains(worker.as_str())
            {
                let when = shared
                    .retired
                    .iter()
                    .filter(|(agent, _)| worker.ends_with(agent.as_str()))
                    .map(|(_, at)| *at)
                    .collect::<Vec<_>>();
                return Err(format!(
                    "{id} stays claimed by {worker}, which was retired at {when:?} (now {}): {:?}; failures {:?}",
                    shared.now, task.phase, self.failures
                ));
            }
        }
        Ok(())
    }

    fn parked(&self, step_ms: u64) {
        let mut shared = self.shared();
        let now = shared.now;
        let retire_ms = self.policy.idle_retire_ms;
        let parked = shared
            .bots
            .iter()
            .filter(|bot| match bot.agent.phase {
                AgentPhase::Idle { since_ms } => now.saturating_sub(since_ms) > retire_ms,
                _ => false,
            })
            .count() as u64;
        shared.stats.parked_ms += parked * step_ms;
    }

    async fn step(&mut self, dispatcher: &mut Option<Dispatcher>, posting: bool) -> Result<(), String> {
        let step_ms = {
            let mut shared = self.shared();
            let step_ms = shared.rng.range(20, STEP_MS_MAX);
            shared.now += step_ms;
            step_ms
        };
        if posting && self.shared().rng.chance(450_000) {
            self.post().await?;
        }
        self.bots_step().await?;
        self.parked(step_ms);
        if self.shared().rng.chance(self.crash_ppm) {
            self.shared().stats.crashes += 1;
            *dispatcher = None;
        }
        if dispatcher.is_none() {
            *dispatcher = Some(self.dispatcher().await?);
            self.seen = None;
        }
        let active = dispatcher.as_mut().ok_or("no dispatcher")?;
        self.probe.refresh().await.map_err(shown)?;
        let (now, epoch) = {
            let shared = self.shared();
            (shared.now, shared.epoch)
        };
        let due = self.wake_at.is_some_and(|at| at <= now);
        if self.seen == Some((self.probe.fold().next(), epoch)) && !due {
            return Ok(());
        }
        let before = active.counters();
        let pass = active.pass(&self.pool, now).await.map_err(shown)?;
        self.failures = pass.failures.iter().map(|failure| format!("{failure:?}")).collect();
        let counters = active.counters();
        self.wake_at = pass.wake_at_ms;
        self.probe.refresh().await.map_err(shown)?;
        let epoch = self.shared().epoch;
        self.seen = Some((self.probe.fold().next(), epoch));
        let crashed = {
            let mut shared = self.shared();
            shared.stats.passes += 1;
            shared.stats.starts += counters.starts - before.starts;
            shared.stats.retires += counters.retires - before.retires;
            shared.stats.reaped += counters.reaped - before.reaped;
            shared.stats.claims_released += counters.claims_released - before.claims_released;
            shared.stats.briefs_blocked += counters.briefs_blocked - before.briefs_blocked;
            if let Some(wrong) = shared.wrong.first() {
                return Err(wrong.clone());
            }
            std::mem::take(&mut shared.crash_pending)
        };
        if crashed {
            *dispatcher = None;
        }
        self.check_budget()?;
        self.check_claims()
    }

    fn open(&self) -> Vec<TaskId> {
        self.posted
            .iter()
            .filter(|id| {
                self.probe
                    .fold()
                    .task(id)
                    .is_none_or(|task| matches!(task.phase, Phase::Open | Phase::Claimed { .. }))
            })
            .cloned()
            .collect()
    }

    fn quiet(&self) -> bool {
        let shared = self.pool.borrow();
        self.wake_at.is_none()
            && shared.bots.iter().all(|bot| {
                matches!(bot.agent.phase, AgentPhase::Idle { .. })
                    && (bot.hung || bot.work.is_none())
            })
            && self.posted.iter().all(|id| {
                self.probe
                    .fold()
                    .task(id)
                    .is_some_and(|task| !matches!(task.phase, Phase::Open))
            })
    }
}

fn policy(rng: &mut Rng) -> Policy {
    let role = |name: &str, kind: &str, cost: u64| Role {
        name: name.to_owned(),
        labels: [(KIND.to_owned(), kind.to_owned())].into_iter().collect(),
        cost_nano_usd: cost,
        placement: format!("{name}-pool"),
    };
    Policy {
        roles: vec![role("coder", "code", 3), role("planner", "goal", 5)],
        member_label: MEMBER.to_owned(),
        project_label: PROJECT.to_owned(),
        intent_prefix: "dispatch/".to_owned(),
        budgets: Budgets {
            agents_per_member_max: rng.range(1, 5),
            agents_per_project_max: rng.range(1, 6),
            nano_usd_max: rng.range(5, 30),
        },
        idle_retire_ms: rng.range(1_000, 20_000),
        start_timeout_ms: rng.range(1_000, 6_000),
        start_attempts_max: rng.range(1, 4),
    }
}

pub struct DispatcherOutcome {
    pub digest: u64,
    pub wrong: Option<String>,
    pub stats: DispatcherStats,
    pub shape: String,
}

async fn drive(world: &mut World) -> Result<(), String> {
    let mut dispatcher = None;
    for _ in 0..POSTING_STEPS {
        world.step(&mut dispatcher, true).await?;
    }
    world.crash_ppm = 0;
    world.shared().lost_ppm = 0;
    for _ in 0..SETTLE_STEPS {
        world.step(&mut dispatcher, false).await?;
        if world.quiet() {
            break;
        }
    }
    let open = world.open();
    if let Some(first) = open.first() {
        let phase = world.probe.fold().task(first).map(|task| task.phase.clone());
        let shared = world.pool.borrow();
        let bots = shared
            .bots
            .iter()
            .map(|bot| format!("{}:{:?}:hung={}", bot.agent.id, bot.agent.phase, bot.hung))
            .collect::<Vec<_>>();
        return Err(format!(
            "{} tasks stranded open or claimed after settling at {}, first {first} {phase:?}; wake_at {:?}; bots {bots:?}",
            open.len(),
            shared.now,
            world.wake_at
        ));
    }
    let shared = world.pool.borrow();
    if let Some(bot) = shared.bots.first() {
        return Err(format!(
            "{} agents left parked with nothing to do, first {} ({:?}), idle_retire_ms {}",
            shared.bots.len(),
            bot.agent.id,
            bot.agent.phase,
            world.policy.idle_retire_ms
        ));
    }
    Ok(())
}

pub fn simulate(seed: u64, mutation: DispatcherMutation) -> DispatcherOutcome {
    let mut rng = Rng::derive(seed, "dispatcher");
    let policy = policy(&mut rng);
    let fail_ppm = rng.range(0, 300_000);
    let lost_ppm = rng.range(0, 80_000);
    let crash_ppm = rng.range(0, 40_000);
    let shape = format!(
        "budgets={:?} idle_retire_ms={} start_timeout_ms={} attempts={} fail_ppm={fail_ppm} lost_ppm={lost_ppm} crash_ppm={crash_ppm}",
        policy.budgets, policy.idle_retire_ms, policy.start_timeout_ms, policy.start_attempts_max
    );
    let host: Arc<dyn LogHost> = Arc::new(MemoryLog::new());
    dispatcher_sim::mutate(mutation.0);
    let outcome = nanocode_sync::block_on(async {
        let open = |actor: &str| Board::open(host.clone(), SCOPE, identity(actor));
        open(OWNER)
            .await
            .map_err(shown)?
            .charter(1_000)
            .await
            .map_err(shown)?;
        let poster = open(POSTER).await.map_err(shown)?;
        let probe = open(OWNER).await.map_err(shown)?;
        let mut world = World {
            host: host.clone(),
            poster,
            probe,
            workers: BTreeMap::new(),
            pool: SimPool(Mutex::new(Shared {
                rng,
                now: 1_000,
                bots: Vec::new(),
                epoch: 0,
                fail_ppm,
                lost_ppm,
                crash_pending: false,
                retired: Vec::new(),
                wrong: Vec::new(),
                stats: DispatcherStats {
                    seeds: 1,
                    ..DispatcherStats::default()
                },
            })),
            policy,
            crash_ppm,
            posted: Vec::new(),
            seen: None,
            wake_at: None,
            failures: Vec::new(),
        };
        let verdict = drive(&mut world).await;
        world.probe.refresh().await.map_err(shown)?;
        let stats = world.pool.borrow().stats;
        let rows = host.load(SCOPE).await.map_err(shown)?;
        let digest = rows
            .iter()
            .fold(FNV_OFFSET, |hash, (_, body)| fnv(hash, body.as_bytes()));
        Ok::<_, String>((verdict, stats, digest))
    });
    dispatcher_sim::mutate(DispatcherRule::None);
    match outcome {
        Ok((verdict, stats, digest)) => DispatcherOutcome {
            digest,
            wrong: verdict.err(),
            stats,
            shape,
        },
        Err(error) => DispatcherOutcome {
            digest: 0,
            wrong: Some(error),
            stats: DispatcherStats::default(),
            shape,
        },
    }
}

pub fn run_seed(seed: u64, mutation: DispatcherMutation) -> DispatcherOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        dispatcher_sim::mutate(DispatcherRule::None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        DispatcherOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: DispatcherStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: DispatcherMutation,
    twice: bool,
    verbose: bool,
    totals: &mut DispatcherStats,
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

pub fn replay(seed: u64, mutation: DispatcherMutation) -> String {
    let mut command = format!("nanocode-sim --scenario dispatcher --seed {seed}");
    if mutation != DispatcherMutation::NONE {
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
    mutation: DispatcherMutation,
    totals: &DispatcherStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=dispatcher mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        DispatcherMutation::NONE if failed == 0 => ExitCode::SUCCESS,
        DispatcherMutation::NONE => ExitCode::FAILURE,
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
