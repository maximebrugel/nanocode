use std::{collections::BTreeMap, ops::Range, process::ExitCode};

use nanocode_cost_fold::{
    Applied, CommitCost, Cost, CostError, CostEvent, Ledger, Placement, Step, Tokens,
    sim::{self as cost_sim, Mutation as CostRule},
};

use crate::rng::{FNV_OFFSET, Rng, fnv};

const ROOTS_MAX: u64 = 3;
const CHAIN: u64 = 2;
const SUBAGENTS_MAX: u64 = 12;
const TURNS_PER_AGENT_MAX: u64 = 3;
const STEPS_MAX: u64 = 48;
const COMMITS_MAX: u64 = 6;
const CHANGES_MAX: u64 = 4;
const TOKENS_MAX: u64 = 200_000;
const NANO_USD_MAX: u64 = 5_000_000;
const HAND_MS_MAX: u64 = 120_000;
const OWN_TASK_PPM: u64 = 400_000;
const INHERITED_TASK_PPM: u64 = 200_000;
const PRICED_PPM: u64 = 700_000;
const PLACED_PPM: u64 = 500_000;
const AGENT_FIRST_PPM: u64 = 300_000;
const RETRY_PPM: u64 = 300_000;
const MODELS: [&str; 3] = ["model-a", "model-b", "model-c"];
const HANDS: [&str; 2] = ["hosted", "box"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostMutation {
    None,
    RetriesCounted,
    RollupSkipsChild,
    OrphanSubagents,
}

impl CostMutation {
    const ALL: [Self; 4] = [
        Self::None,
        Self::RetriesCounted,
        Self::RollupSkipsChild,
        Self::OrphanSubagents,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RetriesCounted => "retries_counted",
            Self::RollupSkipsChild => "rollup_skips_child",
            Self::OrphanSubagents => "orphan_subagents",
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

    const fn rule(self) -> CostRule {
        match self {
            Self::None => CostRule::None,
            Self::RetriesCounted => CostRule::RetriesCounted,
            Self::RollupSkipsChild => CostRule::RollupSkipsChild,
            Self::OrphanSubagents => CostRule::OrphanSubagents,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CostStats {
    pub seeds: u64,
    pub agents: u64,
    pub untasked: u64,
    pub tasks: u64,
    pub depth: u64,
    pub steps: u64,
    pub unpriced: u64,
    pub placed: u64,
    pub unpriced_hand: u64,
    pub commits: u64,
    pub deliveries: u64,
    pub retries: u64,
    pub changed: u64,
}

impl CostStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.agents += other.agents;
        self.untasked += other.untasked;
        self.tasks += other.tasks;
        if other.depth > self.depth {
            self.depth = other.depth;
        }
        self.steps += other.steps;
        self.unpriced += other.unpriced;
        self.placed += other.placed;
        self.unpriced_hand += other.unpriced_hand;
        self.commits += other.commits;
        self.deliveries += other.deliveries;
        self.retries += other.retries;
        self.changed += other.changed;
    }
}

struct Spec {
    name: String,
    parent: Option<usize>,
    task: Option<String>,
}

struct Commit {
    sha: String,
    agent: usize,
    turn: String,
}

struct World {
    agents: Vec<Spec>,
    tasks: Vec<String>,
    steps: Vec<Step>,
    commits: Vec<Commit>,
}

fn task_of(agents: &[Spec], index: usize) -> Option<String> {
    let mut at = index;
    loop {
        if let Some(task) = &agents[at].task {
            return Some(task.clone());
        }
        at = agents[at].parent?;
    }
}

const fn depth(agents: &[Spec], index: usize) -> u64 {
    let mut depth = 0;
    let mut at = agents[index].parent;
    while let Some(parent) = at {
        depth += 1;
        at = agents[parent].parent;
    }
    depth
}

fn generate(rng: &mut Rng) -> World {
    let mut agents: Vec<Spec> = Vec::new();
    let mut tasks: Vec<String> = Vec::new();
    let new_task = |tasks: &mut Vec<String>| {
        let task = format!("task-{}", tasks.len());
        tasks.push(task.clone());
        task
    };
    for _ in 0..rng.range(1, ROOTS_MAX) {
        let task = new_task(&mut tasks);
        agents.push(Spec {
            name: format!("agent-{}", agents.len()),
            parent: None,
            task: Some(task),
        });
    }
    for index in 0..rng.range(CHAIN, SUBAGENTS_MAX) {
        let parent = if index < CHAIN {
            agents.len() - 1
        } else {
            rng.pick(agents.len())
        };
        let task = if index < CHAIN {
            None
        } else if rng.chance(OWN_TASK_PPM) {
            Some(new_task(&mut tasks))
        } else if rng.chance(INHERITED_TASK_PPM) {
            task_of(&agents, parent)
        } else {
            None
        };
        agents.push(Spec {
            name: format!("agent-{}", agents.len()),
            parent: Some(parent),
            task,
        });
    }
    let turns: Vec<Vec<String>> = agents
        .iter()
        .map(|agent| {
            (0..rng.range(1, TURNS_PER_AGENT_MAX))
                .map(|turn| format!("{}/turn-{turn}", agent.name))
                .collect()
        })
        .collect();
    let steps = (0..rng.range(1, STEPS_MAX))
        .map(|key| {
            let agent = rng.pick(agents.len());
            let turn = turns[agent][rng.pick(turns[agent].len())].clone();
            let tokens = Tokens {
                input: rng.below(TOKENS_MAX),
                cache_read: rng.below(TOKENS_MAX),
                cache_write: rng.below(TOKENS_MAX),
                output: rng.below(TOKENS_MAX),
                reasoning: rng.below(TOKENS_MAX),
            };
            let model_nano_usd = rng.chance(PRICED_PPM).then(|| rng.below(NANO_USD_MAX));
            let placement = rng.chance(PLACED_PPM).then(|| Placement {
                hand: HANDS[rng.pick(HANDS.len())].to_owned(),
                hand_ms: rng.below(HAND_MS_MAX),
                hand_nano_usd: rng.chance(PRICED_PPM).then(|| rng.below(NANO_USD_MAX)),
            });
            Step {
                key: format!("step-{key}"),
                agent: agents[agent].name.clone(),
                turn,
                model: MODELS[rng.pick(MODELS.len())].to_owned(),
                tokens,
                model_nano_usd,
                placement,
            }
        })
        .collect();
    let commits = (0..rng.below(COMMITS_MAX + 1))
        .map(|sha| {
            let agent = rng.pick(agents.len());
            let turn = turns[agent][rng.pick(turns[agent].len())].clone();
            Commit {
                sha: format!("commit-{sha}"),
                agent,
                turn,
            }
        })
        .collect();
    World {
        agents,
        tasks,
        steps,
        commits,
    }
}

fn agent_row(world: &World, index: usize) -> CostEvent {
    let spec = &world.agents[index];
    CostEvent::Agent {
        agent: spec.name.clone(),
        parent: spec.parent.map(|parent| world.agents[parent].name.clone()),
        task: spec.task.clone(),
    }
}

fn index_of(world: &World, name: &str) -> usize {
    world
        .agents
        .iter()
        .position(|agent| agent.name == name)
        .expect("every generated step names a generated agent")
}

fn schedule(world: &World, rng: &mut Rng) -> Vec<CostEvent> {
    let mut usage: Vec<(usize, CostEvent)> = world
        .steps
        .iter()
        .map(|step| (index_of(world, &step.agent), CostEvent::Step(step.clone())))
        .chain(world.commits.iter().map(|commit| {
            (
                commit.agent,
                CostEvent::Commit {
                    sha: commit.sha.clone(),
                    agent: world.agents[commit.agent].name.clone(),
                    turn: commit.turn.clone(),
                },
            )
        }))
        .collect();
    let mut log = Vec::new();
    let mut emitted = 0;
    while emitted < world.agents.len() || !usage.is_empty() {
        let ready: Vec<usize> = (0..usage.len())
            .filter(|index| usage[*index].0 < emitted)
            .collect();
        if emitted < world.agents.len() && (ready.is_empty() || rng.chance(AGENT_FIRST_PPM)) {
            log.push(agent_row(world, emitted));
            emitted += 1;
            continue;
        }
        let (_, event) = usage.remove(ready[rng.pick(ready.len())]);
        log.push(event);
    }
    log
}

fn deliver(log: Vec<CostEvent>, rng: &mut Rng) -> Vec<(CostEvent, Applied)> {
    let mut deliveries: Vec<(CostEvent, Applied)> = Vec::new();
    for event in log {
        deliveries.push((event, Applied::Recorded));
        while rng.chance(RETRY_PPM) {
            let earlier = deliveries[rng.pick(deliveries.len())].0.clone();
            deliveries.push((earlier, Applied::Duplicate));
        }
    }
    deliveries
}

const fn plus(mut sum: Cost, cost: &Cost) -> Cost {
    sum.steps += cost.steps;
    sum.tokens.input += cost.tokens.input;
    sum.tokens.cache_read += cost.tokens.cache_read;
    sum.tokens.cache_write += cost.tokens.cache_write;
    sum.tokens.output += cost.tokens.output;
    sum.tokens.reasoning += cost.tokens.reasoning;
    sum.model_nano_usd += cost.model_nano_usd;
    sum.unpriced_model_steps += cost.unpriced_model_steps;
    sum.hand_ms += cost.hand_ms;
    sum.hand_nano_usd += cost.hand_nano_usd;
    sum.unpriced_hand_steps += cost.unpriced_hand_steps;
    sum
}

fn plain(step: &Step) -> Cost {
    let placement = step.placement.as_ref();
    Cost {
        steps: 1,
        tokens: step.tokens,
        model_nano_usd: step.model_nano_usd.unwrap_or_default(),
        unpriced_model_steps: u64::from(step.model_nano_usd.is_none()),
        hand_ms: placement.map_or(0, |placement| placement.hand_ms),
        hand_nano_usd: placement
            .and_then(|placement| placement.hand_nano_usd)
            .unwrap_or_default(),
        unpriced_hand_steps: u64::from(
            placement.is_some_and(|placement| placement.hand_nano_usd.is_none()),
        ),
    }
}

struct Reference {
    own: Vec<Cost>,
    total: Vec<Cost>,
    task_of: Vec<Option<String>>,
    task_parent: BTreeMap<String, String>,
    task_own: BTreeMap<Option<String>, Cost>,
    task_total: BTreeMap<String, Cost>,
    turns: BTreeMap<String, Cost>,
    sum: Cost,
}

fn reference(world: &World) -> Reference {
    let task_of: Vec<Option<String>> = (0..world.agents.len())
        .map(|index| self::task_of(&world.agents, index))
        .collect();
    let mut own = vec![Cost::default(); world.agents.len()];
    let mut task_own: BTreeMap<Option<String>, Cost> = BTreeMap::new();
    let mut turns: BTreeMap<String, Cost> = BTreeMap::new();
    let mut sum = Cost::default();
    for step in &world.steps {
        let cost = plain(step);
        let agent = index_of(world, &step.agent);
        own[agent] = plus(own[agent], &cost);
        let task = task_own.entry(task_of[agent].clone()).or_default();
        *task = plus(*task, &cost);
        let turn = turns.entry(step.turn.clone()).or_default();
        *turn = plus(*turn, &cost);
        sum = plus(sum, &cost);
    }
    let mut total = own.clone();
    for index in (0..world.agents.len()).rev() {
        if let Some(parent) = world.agents[index].parent {
            total[parent] = plus(total[parent], &total[index]);
        }
    }
    let mut task_parent = BTreeMap::new();
    for spec in &world.agents {
        let (Some(task), Some(parent)) = (&spec.task, spec.parent) else {
            continue;
        };
        if let Some(above) = &task_of[parent]
            && above != task
        {
            task_parent.insert(task.clone(), above.clone());
        }
    }
    let mut task_total: BTreeMap<String, Cost> = world
        .tasks
        .iter()
        .map(|task| {
            let own = task_own
                .get(&Some(task.clone()))
                .copied()
                .unwrap_or_default();
            (task.clone(), own)
        })
        .collect();
    for task in world.tasks.iter().rev() {
        if let Some(above) = task_parent.get(task) {
            let below = task_total[task];
            let entry = task_total
                .get_mut(above)
                .expect("a parent task is generated");
            *entry = plus(*entry, &below);
        }
    }
    Reference {
        own,
        total,
        task_of,
        task_parent,
        task_own,
        task_total,
        turns,
        sum,
    }
}

fn readers(ledger: &Ledger, world: &World) -> String {
    let agents: Vec<_> = world
        .agents
        .iter()
        .map(|spec| {
            (
                ledger.agent_own(&spec.name),
                ledger.agent_total(&spec.name),
                ledger.task_of(&spec.name),
                ledger.parent(&spec.name),
                ledger.children(&spec.name).collect::<Vec<_>>(),
            )
        })
        .collect();
    let tasks: Vec<_> = world
        .tasks
        .iter()
        .map(|task| {
            (
                ledger.task_own(task),
                ledger.task_total(task),
                ledger.task_parent(task),
                ledger.task_children(task).collect::<Vec<_>>(),
            )
        })
        .collect();
    format!(
        "{agents:?} {tasks:?} {:?} {:?} {:?} {:?} {:?}",
        ledger.by_agent(),
        ledger.by_turn(),
        ledger.by_task(),
        ledger.commits(),
        ledger.total()
    )
}

fn apply_all(
    ledger: &mut Ledger,
    deliveries: &[(CostEvent, Applied)],
    json: bool,
) -> Result<(), String> {
    for (position, (event, expected)) in deliveries.iter().enumerate() {
        let applied = if json {
            let body = serde_json::to_string(event).map_err(|error| error.to_string())?;
            ledger.apply_json(&body)
        } else {
            ledger.apply(event.clone())
        };
        if applied != Ok(*expected) {
            return Err(format!(
                "delivery {position} {event:?} gave {applied:?}, expected {expected:?}"
            ));
        }
    }
    Ok(())
}

fn check(ledger: &Ledger, world: &World, model: &Reference) -> Result<(), String> {
    for agent in ledger.agents() {
        let own = ledger.agent_own(agent).unwrap_or_default();
        let children = ledger
            .children(agent)
            .map(|child| ledger.agent_total(child).unwrap_or_default())
            .fold(own, |sum, cost| plus(sum, &cost));
        if ledger.agent_total(agent) != Some(children) {
            return Err(format!(
                "conservation: agent {agent} total {:?} != own plus children {children:?}",
                ledger.agent_total(agent)
            ));
        }
    }
    for task in &world.tasks {
        let children = ledger
            .task_children(task)
            .map(|child| ledger.task_total(child))
            .fold(ledger.task_own(task), |sum, cost| plus(sum, &cost));
        if ledger.task_total(task) != children {
            return Err(format!(
                "conservation: task {task} total {:?} != own plus children {children:?}",
                ledger.task_total(task)
            ));
        }
    }
    let total = ledger.total();
    let by_task = ledger.by_task().values().fold(Cost::default(), plus);
    let by_agent = ledger.by_agent().values().fold(Cost::default(), plus);
    let by_turn = ledger.by_turn().values().fold(Cost::default(), plus);
    if by_task != total || by_agent != total || by_turn != total {
        return Err(format!(
            "conservation: total {total:?} by_task {by_task:?} by_agent {by_agent:?} by_turn {by_turn:?}"
        ));
    }
    if total != model.sum {
        return Err(format!(
            "double count: total {total:?} != unique step sum {:?}",
            model.sum
        ));
    }
    for (index, spec) in world.agents.iter().enumerate() {
        let task = ledger.task_of(&spec.name).map(str::to_owned);
        if task != model.task_of[index] {
            return Err(format!(
                "task_of {} is {task:?}, nearest ancestor task is {:?}",
                spec.name, model.task_of[index]
            ));
        }
        if ledger.agent_own(&spec.name) != Some(model.own[index]) {
            return Err(format!(
                "agent_own {} is {:?}, reference {:?}",
                spec.name,
                ledger.agent_own(&spec.name),
                model.own[index]
            ));
        }
        if ledger.agent_total(&spec.name) != Some(model.total[index]) {
            return Err(format!(
                "agent_total {} is {:?}, reference {:?}",
                spec.name,
                ledger.agent_total(&spec.name),
                model.total[index]
            ));
        }
    }
    for task in &world.tasks {
        if ledger.task_parent(task) != model.task_parent.get(task).map(String::as_str) {
            return Err(format!(
                "task_parent {task} is {:?}, reference {:?}",
                ledger.task_parent(task),
                model.task_parent.get(task)
            ));
        }
        if ledger.task_total(task) != model.task_total[task] {
            return Err(format!(
                "task_total {task} is {:?}, reference {:?}",
                ledger.task_total(task),
                model.task_total[task]
            ));
        }
    }
    if *ledger.by_task() != model.task_own {
        return Err(format!(
            "by_task {:?} != reference {:?}",
            ledger.by_task(),
            model.task_own
        ));
    }
    if *ledger.by_turn() != model.turns {
        return Err(format!(
            "by_turn {:?} != reference {:?}",
            ledger.by_turn(),
            model.turns
        ));
    }
    for commit in &world.commits {
        let expected = CommitCost {
            sha: commit.sha.clone(),
            agent: world.agents[commit.agent].name.clone(),
            turn: commit.turn.clone(),
            task: model.task_of[commit.agent].clone(),
            turn_cost: model.turns.get(&commit.turn).copied().unwrap_or_default(),
        };
        if ledger.commit_cost(&commit.sha).as_ref() != Some(&expected) {
            return Err(format!(
                "commit_cost {} is {:?}, reference {expected:?}",
                commit.sha,
                ledger.commit_cost(&commit.sha)
            ));
        }
    }
    if ledger.commits().len() != world.commits.len() {
        return Err(format!(
            "commits lists {} rows, {} were linked",
            ledger.commits().len(),
            world.commits.len()
        ));
    }
    Ok(())
}

fn changed(step: &Step, world: &World, how: usize) -> Step {
    let mut step = step.clone();
    match how {
        0 => step.tokens.output += 1,
        1 => step.model_nano_usd = step.model_nano_usd.map_or(Some(0), |_| None),
        2 => {
            step.placement = match step.placement {
                Some(_) => None,
                None => Some(Placement {
                    hand: HANDS[0].to_owned(),
                    hand_ms: 0,
                    hand_nano_usd: None,
                }),
            }
        }
        _ => {
            let agent = (index_of(world, &step.agent) + 1) % world.agents.len();
            step.agent = world.agents[agent].name.clone();
        }
    }
    step
}

const CHANGE_KINDS: usize = 4;

fn changes(ledger: &mut Ledger, world: &World, rng: &mut Rng) -> Result<u64, String> {
    let count = rng.range(1, CHANGES_MAX);
    for _ in 0..count {
        let step = changed(
            &world.steps[rng.pick(world.steps.len())],
            world,
            rng.pick(CHANGE_KINDS),
        );
        let before = ledger.clone();
        let applied = ledger.apply(CostEvent::Step(step.clone()));
        let expected = Err(CostError::StepChanged {
            key: step.key.clone(),
        });
        if applied != expected {
            return Err(format!(
                "changed row {step:?} gave {applied:?}, expected {expected:?}"
            ));
        }
        if *ledger != before {
            return Err(format!(
                "rejected changed row {} still moved the ledger",
                step.key
            ));
        }
    }
    Ok(count)
}

pub struct CostOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: CostStats,
    shape: String,
}

fn run(seed: u64, stats: &mut CostStats) -> Result<u64, String> {
    let mut rng = Rng::derive(seed, "cost");
    let world = generate(&mut rng);
    let log = schedule(&world, &mut rng);
    let deliveries = deliver(log, &mut rng);
    stats.agents = world.agents.len() as u64;
    stats.untasked = world
        .agents
        .iter()
        .filter(|agent| agent.task.is_none())
        .count() as u64;
    stats.tasks = world.tasks.len() as u64;
    stats.depth = (0..world.agents.len())
        .map(|index| depth(&world.agents, index))
        .max()
        .unwrap_or_default();
    stats.steps = world.steps.len() as u64;
    stats.unpriced = world
        .steps
        .iter()
        .filter(|step| step.model_nano_usd.is_none())
        .count() as u64;
    stats.placed = world
        .steps
        .iter()
        .filter(|step| step.placement.is_some())
        .count() as u64;
    stats.unpriced_hand = world
        .steps
        .iter()
        .filter(|step| {
            step.placement
                .as_ref()
                .is_some_and(|placement| placement.hand_nano_usd.is_none())
        })
        .count() as u64;
    stats.commits = world.commits.len() as u64;
    stats.deliveries = deliveries.len() as u64;
    stats.retries = deliveries
        .iter()
        .filter(|(_, applied)| *applied == Applied::Duplicate)
        .count() as u64;
    let model = reference(&world);
    let mut live = Ledger::new();
    apply_all(&mut live, &deliveries, false)?;
    check(&live, &world, &model)?;
    let mut replayed = Ledger::new();
    apply_all(&mut replayed, &deliveries, true).map_err(|error| format!("replay: {error}"))?;
    let (live_readers, replayed_readers) = (readers(&live, &world), readers(&replayed, &world));
    if live_readers != replayed_readers || live != replayed {
        return Err(format!(
            "replay: fresh ledger readers differ: {live_readers} then {replayed_readers}"
        ));
    }
    stats.changed = changes(&mut live, &world, &mut rng)?;
    Ok(fnv(FNV_OFFSET, live_readers.as_bytes()))
}

pub fn simulate(seed: u64, mutation: CostMutation) -> CostOutcome {
    cost_sim::mutate(mutation.rule());
    let mut stats = CostStats {
        seeds: 1,
        ..CostStats::default()
    };
    let verdict = run(seed, &mut stats);
    cost_sim::mutate(CostRule::None);
    let shape = format!(
        "agents={} tasks={} depth={} steps={} commits={} deliveries={} retries={}",
        stats.agents,
        stats.tasks,
        stats.depth,
        stats.steps,
        stats.commits,
        stats.deliveries,
        stats.retries
    );
    let (digest, wrong) = match verdict {
        Ok(digest) => (digest, None),
        Err(message) => (0, Some(message)),
    };
    CostOutcome {
        digest,
        wrong,
        stats,
        shape,
    }
}

pub fn run_seed(seed: u64, mutation: CostMutation) -> CostOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        cost_sim::mutate(CostRule::None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        CostOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: CostStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: CostMutation,
    twice: bool,
    verbose: bool,
    totals: &mut CostStats,
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

pub fn replay(seed: u64, mutation: CostMutation) -> String {
    let mut command = format!("nanocode-sim --scenario cost --seed {seed}");
    if mutation != CostMutation::None {
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
    mutation: CostMutation,
    totals: &CostStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=cost mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        CostMutation::None if failed == 0 => ExitCode::SUCCESS,
        CostMutation::None => ExitCode::FAILURE,
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
