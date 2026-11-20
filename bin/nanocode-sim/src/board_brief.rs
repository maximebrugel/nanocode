use std::{collections::BTreeMap, ops::Range, process::ExitCode};

use nanocode_board::{
    ClaimId, Event, EventId, Fold, NewTask, Op, Outcome, TaskId,
    access::{Can, Grant, Who},
    brief::{Brief, BriefError, BriefPolicy},
    sim::{self as board_sim, Mutation as BoardRule},
    steer::SteerOp,
};

use crate::rng::{FNV_OFFSET, Rng, fnv};

const OWNER: &str = "user:lead";
const WORKER: &str = "agent:w0";
const SECOND: u64 = 1_000;
const LEASE: u64 = 30 * SECOND;
const HISTORY: &str = "parent-history";
const FILLER: [&str; 4] = ["word ", "é ", "∑ ", "🦀 "];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BriefMutation {
    None,
    OverBudget,
    DropsSteer,
    LeaksParentHistory,
}

impl BriefMutation {
    const ALL: [Self; 4] = [
        Self::None,
        Self::OverBudget,
        Self::DropsSteer,
        Self::LeaksParentHistory,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OverBudget => "brief_over_budget",
            Self::DropsSteer => "brief_drops_steer",
            Self::LeaksParentHistory => "brief_leaks_parent_history",
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
            Self::OverBudget => BoardRule::BriefOverBudget,
            Self::DropsSteer => BoardRule::BriefDropsSteer,
            Self::LeaksParentHistory => BoardRule::BriefLeaksParentHistory,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct BriefStats {
    pub seeds: u64,
    pub briefs: u64,
    pub over_budget: u64,
    pub truncated: u64,
    pub steers: u64,
    pub brief_tokens: u64,
    pub fork_tokens: u64,
}

impl BriefStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.briefs += other.briefs;
        self.over_budget += other.over_budget;
        self.truncated += other.truncated;
        self.steers += other.steers;
        self.brief_tokens += other.brief_tokens;
        self.fork_tokens += other.fork_tokens;
    }
}

struct Host {
    log: Vec<String>,
    fold: Fold,
}

impl Host {
    fn append(&mut self, by: &str, at_ms: u64, op: Op) -> Result<(EventId, u64), String> {
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
            .map_err(|error| format!("brief fold: {error}"))?;
        match self.fold.outcome(&id) {
            Some(Ok(_)) => Ok((id, seq)),
            Some(Err(rejection)) => Err(format!("{} rejected: {rejection:?}", id.0)),
            None => Err(format!("{} has no outcome", id.0)),
        }
    }

    fn refold(&self) -> Result<Fold, String> {
        let mut fold = Fold::new();
        for (index, body) in self.log.iter().enumerate() {
            fold.apply(index as u64 + 1, body)
                .map_err(|error| format!("brief refold: {error}"))?;
        }
        Ok(fold)
    }
}

fn text(rng: &mut Rng, tag: &str, bytes: Range<u64>) -> String {
    let bytes = rng.range(bytes.start, bytes.end);
    let mut body = format!("{tag} ");
    let filler = FILLER[rng.pick(FILLER.len())];
    while (body.len() as u64) < bytes {
        body.push_str(filler);
    }
    body
}

fn post(host: &mut Host, now: u64, task: NewTask) -> Result<TaskId, String> {
    let id = task.id.clone();
    host.append(OWNER, now, Op::Post { task })?;
    Ok(id)
}

fn finish(host: &mut Host, now: u64, task: &TaskId, outcome: Outcome) -> Result<(), String> {
    let claim = claim(host, now, task)?;
    let op = Op::Finish {
        task: task.clone(),
        claim,
        outcome,
    };
    host.append(WORKER, now, op).map(|_| ())
}

fn claim(host: &mut Host, now: u64, task: &TaskId) -> Result<ClaimId, String> {
    let op = Op::Claim {
        task: task.clone(),
        lease_ms: LEASE,
        fence: None,
        cap: None,
    };
    let (id, _) = host.append(WORKER, now, op)?;
    Ok(ClaimId(id.0))
}

fn new_task(id: &str, parent: Option<TaskId>, spec: String) -> NewTask {
    NewTask {
        id: TaskId(id.to_owned()),
        parent,
        spec,
        labels: BTreeMap::new(),
        needs: Vec::new(),
        terms: Default::default(),
    }
}

struct World {
    host: Host,
    child: TaskId,
    spec: String,
    history: Vec<String>,
    results: Vec<String>,
    steers: BTreeMap<u64, String>,
}

fn build(rng: &mut Rng, now: &mut u64) -> Result<World, String> {
    let mut host = Host {
        log: Vec::new(),
        fold: Fold::new(),
    };
    host.append(OWNER, *now, Op::Charter {})?;
    let grant = Grant {
        who: vec![Who::Kind("agent".to_owned())],
        can: BTreeMap::from([("*".to_owned(), vec![Can::Read, Can::Label])]),
    };
    host.append(OWNER, *now, Op::Grant { grant })?;
    let goal = text(rng, "goal", 10..400);
    let parent = post(&mut host, *now, new_task("p", None, goal))?;
    let mut history = Vec::new();
    for turn in 0..rng.range(0, 40) {
        let body = text(rng, &format!("{HISTORY}-{turn}"), 20..3_000);
        let op = Op::Reply {
            task: parent.clone(),
            body: body.clone(),
        };
        host.append(OWNER, *now, op)?;
        history.push(body);
    }
    let mut needs = Vec::new();
    let mut results = Vec::new();
    for need in 0..rng.range(0, 5) {
        let id = post(
            &mut host,
            *now,
            new_task(
                &format!("n{need}"),
                Some(parent.clone()),
                format!("need {need}"),
            ),
        )?;
        *now += rng.range(1, SECOND);
        let result = text(rng, &format!("result-{need}"), 1..2_000);
        finish(&mut host, *now, &id, Outcome::Done(result.clone()))?;
        needs.push(id);
        results.push(result);
    }
    let spec = text(rng, "spec", 1..600);
    let mut child = new_task("c", Some(parent), spec.clone());
    child.needs = needs;
    for label in 0..rng.range(0, 4) {
        child
            .labels
            .insert(format!("scope{label}"), format!("crates/part{label}"));
    }
    let child = post(&mut host, *now, child)?;
    Ok(World {
        host,
        child,
        spec,
        history,
        results,
        steers: BTreeMap::new(),
    })
}

fn steer(world: &mut World, rng: &mut Rng, now: u64, number: u64) -> Result<(), String> {
    let body = text(rng, &format!("steer-{number}"), 1..300);
    let op = Op::Steer(SteerOp::Send {
        task: world.child.clone(),
        claim: None,
        body: body.clone(),
    });
    let (_, seq) = world.host.append(OWNER, now, op)?;
    world.steers.insert(seq, body);
    Ok(())
}

fn heed(world: &mut World, rng: &mut Rng, now: u64) -> Result<(), String> {
    let Some(&upto) = world.steers.keys().nth(rng.pick(world.steers.len().max(1))) else {
        return Ok(());
    };
    let claim = claim(&mut world.host, now, &world.child)?;
    let op = Op::Steer(SteerOp::Heed {
        task: world.child.clone(),
        claim: claim.clone(),
        upto,
    });
    world.host.append(WORKER, now, op)?;
    world.steers.retain(|seq, _| *seq > upto);
    let op = Op::Release {
        task: world.child.clone(),
        claim,
        note: "heeded".to_owned(),
    };
    world.host.append(WORKER, now, op).map(|_| ())
}

fn judge_brief(
    world: &World,
    policy: &BriefPolicy,
    brief: &Result<Brief, BriefError>,
    stats: &mut BriefStats,
) -> Result<(), String> {
    let required = world.spec.len() + world.steers.values().map(String::len).sum::<usize>();
    let brief = match brief {
        Err(BriefError::OverBudget {
            tokens_required,
            tokens_max,
        }) => {
            if tokens_required <= tokens_max || *tokens_max != policy.tokens_max {
                return Err(format!(
                    "over budget refusal with {tokens_required} of {tokens_max} tokens"
                ));
            }
            if *tokens_required < policy.tokens(required) {
                return Err(format!(
                    "refusal counts {tokens_required} tokens under the spec and steers alone"
                ));
            }
            stats.over_budget += 1;
            return Ok(());
        }
        Err(error) => return Err(format!("brief failed: {error:?}")),
        Ok(brief) => brief,
    };
    if brief.tokens > policy.tokens_max {
        return Err(format!(
            "brief is {} tokens over a budget of {}",
            brief.tokens, policy.tokens_max
        ));
    }
    if brief.text.len() as u64 > policy.bytes_max()
        || brief.tokens != policy.tokens(brief.text.len())
    {
        return Err(format!(
            "brief of {} bytes claims {} tokens under {policy:?}",
            brief.text.len(),
            brief.tokens
        ));
    }
    if !brief.text.contains(&world.spec) {
        return Err("brief lost the task spec".to_owned());
    }
    for (seq, body) in &world.steers {
        if !brief.text.contains(body.as_str()) || !brief.steers.contains(seq) {
            return Err(format!("brief dropped pending steer {seq}"));
        }
    }
    if brief.steers.len() != world.steers.len() {
        return Err(format!(
            "brief lists steers {:?}, pending are {:?}",
            brief.steers,
            world.steers.keys().collect::<Vec<_>>()
        ));
    }
    if brief.text.contains(HISTORY) {
        return Err("brief leaks the parent's transcript".to_owned());
    }
    if brief.omitted_bytes == 0 {
        for result in &world.results {
            if !brief.text.contains(result.as_str()) {
                return Err("brief dropped a prior result with room to spare".to_owned());
            }
        }
    } else {
        stats.truncated += 1;
    }
    let parent: usize = world.history.iter().map(String::len).sum();
    stats.briefs += 1;
    stats.steers += brief.steers.len() as u64;
    stats.brief_tokens += brief.tokens;
    stats.fork_tokens += policy.tokens(parent + brief.text.len() + brief.omitted_bytes as usize);
    Ok(())
}

fn round(world: &World, rng: &mut Rng, stats: &mut BriefStats) -> Result<(), String> {
    let bytes_per_token = rng.range(2, 6);
    let tokens_max = if rng.chance(500_000) {
        rng.range(4_000, 20_000)
    } else {
        rng.range(1, 1_500)
    };
    let policy = BriefPolicy {
        tokens_max,
        bytes_per_token,
    };
    let brief = world.host.fold.brief(&world.child, &policy);
    judge_brief(world, &policy, &brief, stats)?;
    if world.host.fold.brief(&world.child, &policy) != brief {
        return Err("brief differs between two calls".to_owned());
    }
    let restored = Fold::restore(&world.host.fold.snapshot()).map_err(|error| error.to_string())?;
    if restored.brief(&world.child, &policy) != brief {
        return Err("brief differs after a snapshot restore".to_owned());
    }
    if world.host.refold()?.brief(&world.child, &policy) != brief {
        return Err("brief differs after a refold".to_owned());
    }
    Ok(())
}

fn run(seed: u64) -> Result<(BriefStats, u64), String> {
    let mut rng = Rng::derive(seed, "board brief");
    let mut now = 1_700_000_000_000 + rng.range(0, 400 * 24 * 3_600 * SECOND);
    let mut world = build(&mut rng, &mut now)?;
    let mut stats = BriefStats::default();
    let mut sent = 0;
    for _ in 0..rng.range(4, 24) {
        now += rng.range(1, 4 * SECOND);
        match rng.below(4) {
            0 | 1 => {
                steer(&mut world, &mut rng, now, sent)?;
                sent += 1;
            }
            2 => heed(&mut world, &mut rng, now)?,
            _ => {}
        }
        round(&world, &mut rng, &mut stats)?;
    }
    let mut digest = FNV_OFFSET;
    for body in &world.host.log {
        digest = fnv(digest, body.as_bytes());
    }
    digest = fnv(digest, &stats.brief_tokens.to_le_bytes());
    stats.seeds = 1;
    Ok((stats, digest))
}

fn run_seed(seed: u64, mutation: BriefMutation) -> (Result<BriefStats, String>, u64) {
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
    mutation: BriefMutation,
    twice: bool,
    verbose: bool,
    totals: &mut BriefStats,
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

pub fn replay(seed: u64, mutation: BriefMutation) -> String {
    let mut command = format!("nanocode-sim --scenario board_brief --seed {seed}");
    if mutation != BriefMutation::None {
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
    mutation: BriefMutation,
    totals: &BriefStats,
) -> ExitCode {
    let briefs = totals.briefs.max(1);
    println!(
        "RESULT seeds={}..{} scenario=board_brief mutation={} passed={passed} failed={failed} twice={twice} briefs={} over_budget={} truncated={} steers={} brief_tokens_mean={} fork_tokens_mean={} (simulated)",
        seeds.start,
        seeds.end,
        mutation.name(),
        totals.briefs,
        totals.over_budget,
        totals.truncated,
        totals.steers,
        totals.brief_tokens / briefs,
        totals.fork_tokens / briefs,
    );
    match mutation {
        BriefMutation::None if failed == 0 => ExitCode::SUCCESS,
        BriefMutation::None => ExitCode::FAILURE,
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
