use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    future::Future,
    ops::Range,
    pin::Pin,
    process::ExitCode,
    sync::Arc,
};

use nanocode_board::{
    Board, Change, ClaimId, Event, EventId, Fold, NewTask, Op, Outcome, Phase, Rejection, TaskId,
    access::{Can, Grant, Who},
    sim::{self as board_sim, Mutation as BoardRule},
    steer::{Steer, SteerOp, Steered},
    tasks::{Lease, Pace, Step, Steps},
};
use nanocode_turnlog::{LogHost, MemoryLog};

use crate::{
    board::identity,
    rng::{FNV_OFFSET, Rng, fnv},
};

const OWNER: &str = "user:lead";
const SECOND: u64 = 1_000;
const LEASE: u64 = 30 * SECOND;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteerMutation {
    None,
    LostOnMove,
    RedeliveredAfterHeed,
    HeedFromStaleClaim,
    ByClaimFollows,
    ToTerminal,
    FinishIgnoresSteers,
    KeptOnCancel,
    StepTakesTwice,
    StepLosesUnseen,
}

impl SteerMutation {
    const ALL: [Self; 10] = [
        Self::None,
        Self::LostOnMove,
        Self::RedeliveredAfterHeed,
        Self::HeedFromStaleClaim,
        Self::ByClaimFollows,
        Self::ToTerminal,
        Self::FinishIgnoresSteers,
        Self::KeptOnCancel,
        Self::StepTakesTwice,
        Self::StepLosesUnseen,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::LostOnMove => "steer_lost_on_move",
            Self::RedeliveredAfterHeed => "steer_redelivered_after_heed",
            Self::HeedFromStaleClaim => "heed_from_stale_claim",
            Self::ByClaimFollows => "steer_by_claim_follows",
            Self::ToTerminal => "steer_to_terminal",
            Self::FinishIgnoresSteers => "finish_ignores_steers",
            Self::KeptOnCancel => "steer_kept_on_cancel",
            Self::StepTakesTwice => "step_takes_steers_twice",
            Self::StepLosesUnseen => "step_heeds_unseen_steers",
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

    const fn board(self) -> BoardRule {
        match self {
            Self::None => BoardRule::None,
            Self::LostOnMove => BoardRule::SteerLostOnMove,
            Self::RedeliveredAfterHeed => BoardRule::SteerRedeliveredAfterHeed,
            Self::HeedFromStaleClaim => BoardRule::HeedFromStaleClaim,
            Self::ByClaimFollows => BoardRule::SteerByClaimFollows,
            Self::ToTerminal => BoardRule::SteerToTerminal,
            Self::FinishIgnoresSteers => BoardRule::FinishIgnoresSteers,
            Self::KeptOnCancel => BoardRule::SteerKeptOnCancel,
            Self::StepTakesTwice => BoardRule::StepSteersTwice,
            Self::StepLosesUnseen => BoardRule::StepHeedsUnseen,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SteerStats {
    pub seeds: u64,
    pub sent: u64,
    pub refused: u64,
    pub heeded: u64,
    pub pending: u64,
    pub dropped: u64,
    pub takeovers: u64,
    pub stale: u64,
    pub crashes: u64,
    pub restarts: u64,
    pub steps: u64,
    pub stepped: u64,
    pub latencies: Vec<u64>,
}

impl SteerStats {
    pub fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.sent += other.sent;
        self.refused += other.refused;
        self.heeded += other.heeded;
        self.pending += other.pending;
        self.dropped += other.dropped;
        self.takeovers += other.takeovers;
        self.stale += other.stale;
        self.crashes += other.crashes;
        self.restarts += other.restarts;
        self.steps += other.steps;
        self.stepped += other.stepped;
        self.latencies.extend_from_slice(&other.latencies);
    }

    fn p99(&self) -> u64 {
        let mut sorted = self.latencies.clone();
        sorted.sort_unstable();
        sorted
            .get(sorted.len().saturating_sub(1) * 99 / 100)
            .copied()
            .unwrap_or(0)
    }
}

struct Host {
    log: Vec<String>,
    fold: Fold,
}

impl Host {
    fn append(
        &mut self,
        by: &str,
        at_ms: u64,
        op: Op,
    ) -> Result<(EventId, Result<u64, Rejection>), String> {
        let id = EventId(format!("{by}.{}", self.log.len() + 1));
        let event = Event {
            id: id.clone(),
            by: by.to_owned(),
            at_ms,
            op,
        };
        let body = serde_json::to_string(&event).map_err(|error| error.to_string())?;
        self.log.push(body);
        let seq = self.log.len() as u64;
        self.fold
            .apply(seq, &self.log[self.log.len() - 1])
            .map_err(|error| format!("steer fold: {error}"))?;
        match self.fold.outcome(&id) {
            Some(Ok(_)) => Ok((id, Ok(seq))),
            Some(Err(rejection)) => Ok((id, Err(rejection.clone()))),
            None => Err(format!("{} has no outcome", id.0)),
        }
    }

    fn restart(&mut self, from_snapshot: bool) -> Result<(), String> {
        if from_snapshot {
            self.fold = Fold::restore(&self.fold.snapshot()).map_err(|error| error.to_string())?;
            return Ok(());
        }
        self.fold = Fold::new();
        for (index, body) in self.log.iter().enumerate() {
            self.fold
                .apply(index as u64 + 1, body)
                .map_err(|error| format!("steer refold: {error}"))?;
        }
        Ok(())
    }

    fn holder(&self, task: &TaskId) -> Option<ClaimId> {
        match self.fold.task(task).map(|task| &task.phase) {
            Some(Phase::Claimed { claim, .. }) => Some(claim.clone()),
            _ => None,
        }
    }

    fn finished(&self, task: &TaskId) -> bool {
        self.fold.task(task).is_some_and(|task| task.finished())
    }

    fn pending(&self, task: &TaskId) -> Vec<u64> {
        self.fold.steers(task).map(|steer| steer.seq).collect()
    }
}

#[derive(Default)]
struct Worker {
    held: Option<(TaskId, ClaimId)>,
    taken: Vec<u64>,
}

#[derive(Default)]
struct Oracle {
    sent: BTreeMap<u64, (TaskId, u64)>,
    taken: BTreeMap<u64, BTreeSet<ClaimId>>,
    acked: BTreeMap<u64, ClaimId>,
    dropped: BTreeSet<u64>,
    claims: BTreeMap<TaskId, Vec<ClaimId>>,
    stats: SteerStats,
}

fn expect(
    what: &str,
    accepted: bool,
    wanted: bool,
    verdict: &Result<u64, Rejection>,
) -> Result<(), String> {
    if accepted == wanted {
        return Ok(());
    }
    Err(format!(
        "{what}: wanted {}, the fold said {verdict:?}",
        if wanted { "accepted" } else { "rejected" }
    ))
}

fn post(host: &mut Host, now: u64, number: u64) -> Result<TaskId, String> {
    let id = TaskId(format!("t{number}"));
    let task = NewTask {
        id: id.clone(),
        parent: None,
        spec: "steered".to_owned(),
        labels: BTreeMap::new(),
        needs: Vec::new(),
        terms: Default::default(),
    };
    let (_, verdict) = host.append(OWNER, now, Op::Post { task })?;
    verdict.map_err(|rejection| format!("post {id}: {rejection:?}"))?;
    Ok(id)
}

fn send(
    host: &mut Host,
    oracle: &mut Oracle,
    rng: &mut Rng,
    now: u64,
    task: &TaskId,
) -> Result<(), String> {
    let holder = host.holder(task);
    let claim = match rng.below(3) {
        0 => None,
        1 => holder.clone(),
        _ => oracle.claims.get(task).and_then(|claims| {
            (!claims.is_empty()).then(|| claims[rng.pick(claims.len())].clone())
        }),
    };
    let wanted = !host.finished(task)
        && claim
            .as_ref()
            .is_none_or(|claim| Some(claim) == holder.as_ref());
    let by_claim = claim.is_some();
    let op = Op::Steer(SteerOp::Send {
        task: task.clone(),
        claim,
        body: format!("steer at {now}"),
    });
    let (_, verdict) = host.append(OWNER, now, op)?;
    expect(
        if by_claim {
            "steer by claim"
        } else {
            "steer by task"
        },
        verdict.is_ok(),
        wanted,
        &verdict,
    )?;
    match verdict {
        Ok(seq) => {
            oracle.stats.sent += 1;
            oracle.sent.insert(seq, (task.clone(), now));
        }
        Err(_) => oracle.stats.refused += 1,
    }
    Ok(())
}

fn heed(
    host: &mut Host,
    oracle: &mut Oracle,
    worker: &mut Worker,
    name: &str,
    now: u64,
) -> Result<(), String> {
    let Some((task, claim)) = worker.held.clone() else {
        return Ok(());
    };
    let Some(&upto) = worker.taken.iter().max() else {
        return Ok(());
    };
    let current = host.holder(&task).as_ref() == Some(&claim);
    let wanted = current && host.pending(&task).contains(&upto);
    if !current {
        oracle.stats.stale += 1;
    }
    let op = Op::Steer(SteerOp::Heed {
        task: task.clone(),
        claim: claim.clone(),
        upto,
    });
    let (_, verdict) = host.append(name, now, op)?;
    expect("heed", verdict.is_ok(), wanted, &verdict)?;
    worker.taken.clear();
    if verdict.is_err() {
        if !current {
            worker.held = None;
        }
        return Ok(());
    }
    let heeded: Vec<u64> = oracle
        .sent
        .range(..=upto)
        .filter(|(seq, (on, _))| *on == task && !oracle.acked.contains_key(seq))
        .map(|(seq, _)| *seq)
        .collect();
    for seq in heeded {
        if !oracle
            .taken
            .get(&seq)
            .is_some_and(|claims| claims.contains(&claim))
        {
            return Err(format!(
                "steer {seq} on {task} was heeded by {} without being delivered to it",
                claim.0
            ));
        }
        let at = oracle.sent[&seq].1;
        oracle.stats.latencies.push(now - at);
        oracle.stats.heeded += 1;
        oracle.acked.insert(seq, claim.clone());
    }
    Ok(())
}

fn take(host: &Host, oracle: &mut Oracle, worker: &mut Worker) -> Result<(), String> {
    let Some((task, claim)) = worker.held.clone() else {
        return Ok(());
    };
    let Ok(steers) = host.fold.steers_for(&task, &claim) else {
        worker.held = None;
        worker.taken.clear();
        return Ok(());
    };
    let seqs: Vec<u64> = steers.map(|steer| steer.seq).collect();
    for seq in &seqs {
        if let Some(by) = oracle.acked.get(seq) {
            return Err(format!(
                "steer {seq} on {task} was delivered to {} after {} heeded it",
                claim.0, by.0
            ));
        }
        oracle.taken.entry(*seq).or_default().insert(claim.clone());
    }
    worker.taken = seqs;
    Ok(())
}

fn finish(host: &mut Host, worker: &mut Worker, name: &str, now: u64) -> Result<(), String> {
    let Some((task, claim)) = worker.held.clone() else {
        return Ok(());
    };
    let current = host.holder(&task).as_ref() == Some(&claim);
    let pending = host.pending(&task);
    let op = Op::Finish {
        task: task.clone(),
        claim,
        outcome: Outcome::Done("steered".to_owned()),
    };
    let (_, verdict) = host.append(name, now, op)?;
    expect(
        &format!("finish {task} with {} steers pending", pending.len()),
        verdict.is_ok(),
        current && pending.is_empty(),
        &verdict,
    )?;
    if verdict.is_ok() || !current {
        worker.held = None;
        worker.taken.clear();
    }
    Ok(())
}

fn step(
    host: &mut Host,
    oracle: &mut Oracle,
    rng: &mut Rng,
    worker: &mut Worker,
    name: &str,
    now: u64,
    tasks: &[TaskId],
) -> Result<(), String> {
    let Some((task, claim)) = worker.held.clone() else {
        let task = tasks[rng.pick(tasks.len())].clone();
        let before = host.holder(&task);
        let (id, verdict) = host.append(
            name,
            now,
            Op::Claim {
                task: task.clone(),
                lease_ms: LEASE,
                fence: None,
                cap: None,
            },
        )?;
        if verdict.is_ok() {
            if before.is_some() {
                oracle.stats.takeovers += 1;
            }
            let claim = ClaimId(id.0);
            oracle
                .claims
                .entry(task.clone())
                .or_default()
                .push(claim.clone());
            worker.held = Some((task, claim));
        }
        return Ok(());
    };
    match rng.below(20) {
        0 => {
            oracle.stats.crashes += 1;
            *worker = Worker::default();
        }
        1 => {
            let op = Op::Release {
                task,
                claim,
                note: "let go".to_owned(),
            };
            let (_, verdict) = host.append(name, now, op)?;
            let _ = verdict;
            *worker = Worker::default();
        }
        2..=4 => {
            let op = Op::Renew {
                task,
                claim,
                lease_ms: LEASE,
            };
            let (_, verdict) = host.append(name, now, op)?;
            if verdict.is_err() {
                *worker = Worker::default();
            }
        }
        5 | 6 => finish(host, worker, name, now)?,
        7..=12 if !worker.taken.is_empty() => heed(host, oracle, worker, name, now)?,
        _ => take(host, oracle, worker)?,
    }
    Ok(())
}

/// Cancelling a task with pending steers drops every one of them exactly once and says so
/// in one Dropped change, so the sender never waits on a steer no holder will read.
fn cancel(host: &mut Host, oracle: &mut Oracle, now: u64, task: TaskId) -> Result<(), String> {
    let pending = host.pending(&task);
    let before = host.fold.entries().len();
    let op = Op::Cancel {
        task: task.clone(),
        reason: "dropped".to_owned(),
    };
    if host.append(OWNER, now, op)?.1.is_err() {
        return Ok(());
    }
    let left = host.pending(&task);
    if !left.is_empty() {
        return Err(format!("cancelled {task} still holds steers {left:?}"));
    }
    let drops: Vec<(u64, u64)> = host.fold.entries()[before..]
        .iter()
        .filter(|entry| entry.task == task)
        .filter_map(|entry| match &entry.change {
            Change::Steered(Steered::Dropped { upto, count }) => Some((*upto, *count)),
            _ => None,
        })
        .collect();
    let wanted: Vec<(u64, u64)> = pending
        .last()
        .map(|upto| vec![(*upto, pending.len() as u64)])
        .unwrap_or_default();
    if drops != wanted {
        return Err(format!(
            "cancel of {task} reported drops {drops:?}, pending were {pending:?}"
        ));
    }
    for seq in pending {
        if oracle.acked.contains_key(&seq) || !oracle.dropped.insert(seq) {
            return Err(format!(
                "steer {seq} on {task} dropped after it was settled"
            ));
        }
        oracle.stats.dropped += 1;
    }
    Ok(())
}

fn run(seed: u64) -> Result<(SteerStats, u64), String> {
    let mut rng = Rng::derive(seed, "board steer");
    let mut now = 1_700_000_000_000 + rng.range(0, 400 * 24 * 3_600 * SECOND);
    let mut host = Host {
        log: Vec::new(),
        fold: Fold::new(),
    };
    let mut oracle = Oracle::default();
    host.append(OWNER, now, Op::Charter {})?
        .1
        .map_err(|rejection| format!("charter: {rejection:?}"))?;
    let grant = Grant {
        who: vec![Who::Kind("agent".to_owned())],
        can: BTreeMap::from([("*".to_owned(), vec![Can::Read, Can::Label])]),
    };
    host.append(OWNER, now, Op::Grant { grant })?
        .1
        .map_err(|rejection| format!("grant: {rejection:?}"))?;
    let names: Vec<String> = (0..rng.range(1, 5))
        .map(|n| format!("agent:w{n}"))
        .collect();
    let mut workers: Vec<Worker> = names.iter().map(|_| Worker::default()).collect();
    let mut posted = 0;
    let mut tasks = Vec::new();
    for _ in 0..rng.range(1, 5) {
        tasks.push(post(&mut host, now, posted)?);
        posted += 1;
    }
    let steer_ppm = rng.range(100_000, 500_000);
    let gap = rng.range(200, 4 * SECOND);
    for _ in 0..rng.range(200, 1_200) {
        now += rng.range(1, gap);
        let roll = rng.below(1_000_000);
        if roll < steer_ppm {
            let task = tasks[rng.pick(tasks.len())].clone();
            send(&mut host, &mut oracle, &mut rng, now, &task)?;
        } else if roll < steer_ppm + 4_000 {
            let task = tasks[rng.pick(tasks.len())].clone();
            cancel(&mut host, &mut oracle, now, task)?;
        } else if roll < steer_ppm + 12_000 {
            oracle.stats.restarts += 1;
            host.restart(rng.chance(500_000))?;
            for worker in &mut workers {
                *worker = Worker::default();
            }
        } else {
            let index = rng.pick(workers.len());
            step(
                &mut host,
                &mut oracle,
                &mut rng,
                &mut workers[index],
                &names[index],
                now,
                &tasks,
            )?;
        }
        if tasks.iter().all(|task| host.finished(task)) || rng.chance(2_000) {
            tasks.push(post(&mut host, now, posted)?);
            posted += 1;
            tasks.retain(|task| !host.finished(task));
        }
    }
    for (seq, (task, _)) in &oracle.sent {
        let pending = host.pending(task).contains(seq);
        match (oracle.acked.get(seq), pending) {
            (Some(by), true) => {
                return Err(format!(
                    "steer {seq} on {task} is still pending after {} heeded it",
                    by.0
                ));
            }
            (None, false) if oracle.dropped.contains(seq) => {}
            (None, false) => return Err(format!("steer {seq} on {task} was lost unheeded")),
            (None, true) => oracle.stats.pending += 1,
            (Some(_), false) => {}
        }
    }
    let mut digest = FNV_OFFSET;
    for body in &host.log {
        digest = fnv(digest, body.as_bytes());
    }
    let (steps, stepped, delivered) = worked(seed)?;
    oracle.stats.steps = steps;
    oracle.stats.stepped = stepped;
    for seq in delivered {
        digest = fnv(digest, &seq.to_le_bytes());
    }
    oracle.stats.seeds = 1;
    Ok((oracle.stats, digest))
}

struct Clock(Cell<u64>);

impl Pace for Clock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }

    fn sleep(&self, ms: u64) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        self.0.set(self.0.get() + ms);
        Box::pin(std::future::ready(()))
    }
}

struct Stepper<'a> {
    lead: Board,
    clock: &'a Clock,
    rng: Rng,
    task: TaskId,
    steps: u64,
    left: u64,
    send_ppm: u64,
    sent: BTreeMap<u64, u64>,
    seen: Vec<u64>,
}

impl Stepper<'_> {
    async fn send(&mut self) {
        let now = self.clock.0.get() + self.rng.range(1, SECOND);
        self.clock.0.set(now);
        let body = format!("steer in step {}", self.steps);
        match self.lead.steer(now, self.task.clone(), body).await {
            Ok(applied) => {
                self.sent.insert(applied.seq, self.steps);
            }
            Err(error) => panic!("steer to the worked task failed: {error:?}"),
        }
    }

    fn heard(&mut self, steers: &[Steer]) {
        for steer in steers {
            if self.seen.contains(&steer.seq) {
                panic!("steer {} was taken twice, again in step {}", steer.seq, self.steps);
            }
            match self.sent.get(&steer.seq) {
                Some(at) if *at >= self.steps => panic!(
                    "steer {} sent during step {at} was taken mid-step by step {}",
                    steer.seq, self.steps
                ),
                Some(_) => self.seen.push(steer.seq),
                None => panic!("step {} was given steer {} nobody sent", self.steps, steer.seq),
            }
        }
    }
}

impl Steps for Stepper<'_> {
    fn step(&mut self, lease: Lease, steers: Vec<Steer>) -> Pin<Box<dyn Future<Output = Step> + '_>> {
        Box::pin(async move {
            self.steps += 1;
            assert!(self.steps < 10_000, "the worked task never finished");
            assert!(lease.task == self.task, "a step ran under another task's lease");
            self.heard(&steers);
            for _ in 0..self.rng.range(0, 3) {
                if self.rng.chance(self.send_ppm) {
                    self.send().await;
                }
            }
            let now = self.clock.0.get() + self.rng.range(1, SECOND);
            self.clock.0.set(now);
            if self.left == 0 || !steers.is_empty() && self.rng.chance(200_000) {
                return Step::Done(Outcome::Done(format!("worked {} steps", self.steps)));
            }
            self.left -= 1;
            Step::More
        })
    }
}

fn worked(seed: u64) -> Result<(u64, u64, Vec<u64>), String> {
    let mut rng = Rng::derive(seed, "board steer work");
    let clock = Clock(Cell::new(1_700_000_000_000 + rng.range(0, 400 * 24 * 3_600 * SECOND)));
    let host: Arc<dyn LogHost> = Arc::new(MemoryLog::new());
    let scope = "steer-work";
    nanocode_sync::block_on(async {
        let shown = |error| format!("{error:?}");
        let open = |actor: &str| Board::open(host.clone(), scope, identity(actor));
        let mut lead = open(OWNER).await.map_err(shown)?;
        lead.charter(clock.now_ms()).await.map_err(shown)?;
        let grant = Grant {
            who: vec![Who::Kind("agent".to_owned())],
            can: BTreeMap::from([("*".to_owned(), vec![Can::Read, Can::Label])]),
        };
        lead.submit(clock.now_ms(), Op::Grant { grant })
            .await
            .map_err(shown)?;
        let task = TaskId("worked".to_owned());
        let new = NewTask {
            id: task.clone(),
            parent: None,
            spec: "work in steps".to_owned(),
            labels: BTreeMap::new(),
            needs: Vec::new(),
            terms: Default::default(),
        };
        lead.submit(clock.now_ms(), Op::Post { task: new })
            .await
            .map_err(shown)?;
        let mut worker = open("agent:w").await.map_err(shown)?;
        let mut stepper = Stepper {
            lead,
            clock: &clock,
            task: task.clone(),
            steps: 0,
            left: rng.range(1, 12),
            send_ppm: rng.range(200_000, 800_000),
            sent: BTreeMap::new(),
            seen: Vec::new(),
            rng: Rng::derive(seed, "board steer stepper"),
        };
        for _ in 0..rng.range(0, 3) {
            stepper.send().await;
        }
        let finished = worker.work(&task, LEASE, &clock, &mut stepper).await;
        finished.map_err(|error| format!("the worked task did not finish: {error:?}"))?;
        stepper.lead.refresh().await.map_err(shown)?;
        let fold = stepper.lead.fold();
        if !fold.task(&task).is_some_and(|task| task.finished()) {
            return Err("work returned before its task finished".to_owned());
        }
        for (seq, at) in &stepper.sent {
            if !stepper.seen.contains(seq) {
                return Err(format!(
                    "steer {seq} sent during step {at} was heeded at a step boundary without being taken"
                ));
            }
        }
        Ok((stepper.steps, stepper.seen.len() as u64, stepper.seen))
    })
}

fn run_seed(seed: u64, mutation: SteerMutation) -> (Result<SteerStats, String>, u64) {
    board_sim::mutate(mutation.board());
    let outcome = std::panic::catch_unwind(|| run(seed));
    board_sim::mutate(BoardRule::None);
    match outcome {
        Ok(Ok((stats, digest))) => (Ok(stats), digest),
        Ok(Err(wrong)) => (Err(wrong), 0),
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                .unwrap_or_default();
            (Err(format!("panic: {message}")), 0)
        }
    }
}

pub fn judge(
    seed: u64,
    mutation: SteerMutation,
    twice: bool,
    verbose: bool,
    totals: &mut SteerStats,
) -> Option<String> {
    let (first, digest) = run_seed(seed, mutation);
    if verbose {
        println!("seed {seed} digest {digest:016x} {first:?}");
    }
    let stats = match first {
        Ok(stats) => stats,
        Err(wrong) => return Some(wrong),
    };
    totals.add(&stats);
    if !twice {
        return None;
    }
    let (second, again) = run_seed(seed, mutation);
    match second {
        Err(wrong) => Some(format!("second run: {wrong}")),
        Ok(_) if again != digest => Some(format!(
            "nondeterminism: trace digest {digest:016x} then {again:016x}"
        )),
        Ok(_) => None,
    }
}

pub fn replay(seed: u64, mutation: SteerMutation) -> String {
    let mut command = format!("nanocode-sim --scenario board_steer --seed {seed}");
    if mutation != SteerMutation::None {
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
    mutation: SteerMutation,
    totals: &SteerStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=board_steer mutation={} passed={passed} failed={failed} twice={twice} sent={} refused={} heeded={} pending={} dropped={} takeovers={} stale_heeds={} crashes={} restarts={} steps={} stepped={} heed_latency_p99_ms={} (simulated)",
        seeds.start,
        seeds.end,
        mutation.name(),
        totals.sent,
        totals.refused,
        totals.heeded,
        totals.pending,
        totals.dropped,
        totals.takeovers,
        totals.stale,
        totals.crashes,
        totals.restarts,
        totals.steps,
        totals.stepped,
        totals.p99(),
    );
    match mutation {
        SteerMutation::None if failed == 0 => ExitCode::SUCCESS,
        SteerMutation::None => ExitCode::FAILURE,
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
