use std::{cell::RefCell, sync::Arc, time::Duration};

use nanocode_agent_core::AgentHandle;
use nanocode_board::{Board, NewTask, Op, Phase, TaskId};
use nanocode_dispatch::{Budgets, IntentState, Policy, Role};
use nanocode_dispatcher::{BRIEF, Dispatcher, Pool};
use nanocode_subagents::{
    AgentId, AgentStatus, AgentTask, Coordination, DEFAULT_MAX_SUBAGENTS, DISPATCH_LABEL,
    DISPATCH_POLL_MS, DISPATCH_ROLE, HostedView, Registry, ScopedAgentUpdate, SubagentControl,
    SubagentPool,
};
use nanocode_sync::mpsc::UnboundedReceiver;
use nanocode_turnlog::{LogHost, MemoryLog};
use serde_json::Value;

use crate::{
    board::identity,
    executor::{Clock, Ended, Executor, Task},
    rng::Rng,
    scripted::{Counts, Ledger, Script, ScriptedFactory, ScriptedStore, simulated_model},
};

pub const CHILDREN_MIN: u64 = 1;
pub const CHILDREN_MAX: u64 = 64;
pub const RESIDENT_MAX: u64 = 4;
pub const TURN_STEPS_MAX: u64 = 8;
pub const STEP_LATENCY_MS_MIN: u64 = 50;
pub const STEP_LATENCY_MS_MAX: u64 = 20_000;
pub const RESTORE_LATENCY_MS_MAX: u64 = 2_000;
pub const SHUTDOWN_LATENCY_MS_MAX: u64 = 1_000;
pub const CANCEL_PPM: u64 = 250_000;
pub const POOLED_MAX: u64 = 4;
pub const RUNTIME_STOP_TIMEOUT: Duration = Duration::from_secs(30);
const ROOT_SESSION: &str = "root";
const POOL_SCOPE: &str = "pool";
const POOL_OWNER: &str = "agent:sim";
const POOL_POSTER: &str = "agent:sim+poster";
const POOL_KIND: &str = "kind";
const POOL_WORK: &str = "code";
const POOL_MEMBER: &str = "member";
const POOL_PROJECT: &str = "project";
const POOL_CHARTER_MS: u64 = 1_000;
const POOL_LEASE_MS: u64 = 3_600_000;
const POOL_IDLE_RETIRE_MS: u64 = 60_000;
const POOL_PASSES_MAX: u64 = 16;
const HOSTED_PASSES_MAX: u64 = 24;
const HOSTED_POLL: Duration = Duration::from_millis(DISPATCH_POLL_MS);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentsMutation {
    None,
    CancelIgnored,
    ShutdownHangs,
    ShutdownKeepsHarness,
    TurnLeaks,
    BoardFailuresUnread,
    PoolRetireKeepsAgent,
    PoolForgetsIntent,
}

impl AgentsMutation {
    const ALL: [Self; 8] = [
        Self::None,
        Self::CancelIgnored,
        Self::ShutdownHangs,
        Self::ShutdownKeepsHarness,
        Self::TurnLeaks,
        Self::BoardFailuresUnread,
        Self::PoolRetireKeepsAgent,
        Self::PoolForgetsIntent,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CancelIgnored => "cancel_ignored",
            Self::ShutdownHangs => "shutdown_hangs",
            Self::ShutdownKeepsHarness => "shutdown_keeps_harness",
            Self::TurnLeaks => "turn_leaks",
            Self::BoardFailuresUnread => "board_failures_unread",
            Self::PoolRetireKeepsAgent => "pool_retire_keeps_agent",
            Self::PoolForgetsIntent => "pool_forgets_intent",
        }
    }

    pub fn names() -> String {
        Self::ALL.map(Self::name).join("|")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Close,
    Cancel,
}

struct Shape {
    children: u64,
    pooled: u64,
    resident_max: usize,
    stop_after: Duration,
    stop: Stop,
    script: Script,
}

impl Shape {
    fn of(seed: u64, mutation: AgentsMutation) -> Self {
        let mut rng = Rng::derive(seed, "agents-shape");
        let children = rng.range(CHILDREN_MIN, CHILDREN_MAX);
        let resident_max = rng.range(0, RESIDENT_MAX);
        let stop = if rng.chance(CANCEL_PPM) {
            Stop::Cancel
        } else {
            Stop::Close
        };
        let script = Script {
            steps_max: rng.range(1, TURN_STEPS_MAX),
            step_latency_ms_min: STEP_LATENCY_MS_MIN,
            step_latency_ms_max: rng.range(STEP_LATENCY_MS_MIN, STEP_LATENCY_MS_MAX),
            restore_latency_ms_max: rng.range(0, RESTORE_LATENCY_MS_MAX),
            shutdown_latency_ms_max: rng.range(0, SHUTDOWN_LATENCY_MS_MAX),
            mutation,
        };
        let turn_ms_max = script.steps_max * script.step_latency_ms_max;
        let stop_after = Duration::from_millis(rng.range(0, turn_ms_max));
        let pooled = rng.range(1, POOLED_MAX);
        Self {
            children,
            pooled,
            resident_max: usize::try_from(resident_max).expect("a resident bound fits in usize"),
            stop_after,
            stop,
            script,
        }
    }

    fn describe(&self) -> String {
        format!(
            "scenario=agents children={} pooled={} resident<={} stop={:?} after_ms={} steps<={} latency_ms<={} restore_ms<={} shutdown_ms<={}",
            self.children,
            self.pooled,
            self.resident_max,
            self.stop,
            self.stop_after.as_millis(),
            self.script.steps_max,
            self.script.step_latency_ms_max,
            self.script.restore_latency_ms_max,
            self.script.shutdown_latency_ms_max
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AgentStats {
    pub seeds: u64,
    pub closes: u64,
    pub cancels: u64,
    pub children: u64,
    pub spawned: u64,
    pub restored: u64,
    pub parked: u64,
    pub steps: u64,
    pub turns: u64,
    pub stop_ms_max: u64,
}

impl AgentStats {
    pub fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.closes += other.closes;
        self.cancels += other.cancels;
        self.children += other.children;
        self.spawned += other.spawned;
        self.restored += other.restored;
        self.parked += other.parked;
        self.steps += other.steps;
        self.turns += other.turns;
        self.stop_ms_max = self.stop_ms_max.max(other.stop_ms_max);
    }

    fn of(shape: &Shape, counts: Counts, stop_ms: u64) -> Self {
        Self {
            seeds: 1,
            closes: u64::from(shape.stop == Stop::Close),
            cancels: u64::from(shape.stop == Stop::Cancel),
            children: shape.children,
            spawned: counts.spawned,
            restored: counts.restored,
            parked: counts.parked,
            steps: counts.steps,
            turns: counts.turns,
            stop_ms_max: stop_ms,
        }
    }
}

pub struct AgentOutcome {
    pub digest: u64,
    pub wrong: Option<String>,
    pub stats: AgentStats,
    pub shape: String,
}

struct Run {
    registry: Arc<Registry>,
    control: SubagentControl,
    root: AgentHandle,
    updates: UnboundedReceiver<ScopedAgentUpdate>,
    plane: Option<Arc<dyn Coordination>>,
}

fn build(ledger: &Arc<Ledger>, shape: &Shape) -> Run {
    let (registry, control, updates) = nanocode_subagents::channel(DEFAULT_MAX_SUBAGENTS);
    let plane = crate::boardagents::plane();
    control.set_coordination(plane.clone());
    control.set_hibernation(true);
    control.set_max_resident(shape.resident_max);
    registry.set_store(Arc::new(ScriptedStore::new(Arc::clone(ledger))));
    let root = AgentHandle::new(
        ROOT_SESSION,
        simulated_model(),
        Arc::new(ScriptedFactory::new(Arc::clone(ledger))),
    );
    Run {
        registry,
        control,
        root,
        updates,
        plane,
    }
}

fn tasks(children: u64) -> Vec<AgentTask> {
    (0..children)
        .map(|child| AgentTask {
            role: "worker".to_owned(),
            task: format!("task {child}"),
            output_schema: Value::Object(serde_json::Map::new()),
        })
        .collect()
}

async fn stop(run: &Run, shape: &Shape, clock: &Clock) -> Result<u64, String> {
    let began_ns = clock.now_ns();
    match shape.stop {
        Stop::Close => run
            .control
            .close_all(ROOT_SESSION)
            .await
            .map_err(|error| format!("close_all failed: {error}"))?,
        Stop::Cancel => run.control.cancel_all(ROOT_SESSION).await,
    }
    let took = Duration::from_nanos(clock.now_ns() - began_ns);
    let took_ms = u64::try_from(took.as_millis()).expect("a virtual stop fits in u64 milliseconds");
    if took >= RUNTIME_STOP_TIMEOUT {
        return Err(format!(
            "{:?} took {took_ms} ms of virtual time, not under the runtime's stop deadline of {} ms",
            shape.stop,
            RUNTIME_STOP_TIMEOUT.as_millis()
        ));
    }
    Ok(took_ms)
}

const fn settled(stop: Stop, status: &AgentStatus) -> bool {
    match stop {
        Stop::Close => matches!(status, AgentStatus::Closed),
        Stop::Cancel => matches!(
            status,
            AgentStatus::Interrupted | AgentStatus::Completed { .. } | AgentStatus::Failed { .. }
        ),
    }
}

async fn judge(
    run: &Run,
    shape: &Shape,
    ledger: &Ledger,
    retired: &[AgentId],
    hosted: u64,
) -> Result<(), String> {
    let turns = ledger.running_turns();
    if turns > 0 {
        return Err(format!(
            "{turns} children still hold a turn after {:?}",
            shape.stop
        ));
    }
    let open = ledger.open_agents();
    if shape.stop == Stop::Close && open > 0 {
        return Err(format!(
            "{open} children still hold a resident harness after Close"
        ));
    }
    let listed = run
        .registry
        .directory(ROOT_SESSION, true, false)
        .await
        .map_err(|error| {
            format!(
                "the directory did not answer after {:?}: {error}",
                shape.stop
            )
        })?;
    let count = u64::try_from(listed.len()).expect("a child count fits in u64");
    if count != shape.children + shape.pooled + hosted {
        return Err(format!(
            "the directory lists {count} of {} children, {} pooled and {hosted} hosted agents",
            shape.children, shape.pooled
        ));
    }
    for entry in listed {
        let closed = retired.contains(&entry.agent_id);
        if closed && entry.status != AgentStatus::Closed {
            return Err(format!(
                "the pool retired agent {} but it is {:?} after {:?}",
                entry.agent_id, entry.status, shape.stop
            ));
        }
        if !closed && !settled(shape.stop, &entry.status) {
            return Err(format!(
                "agent {} is {:?} after {:?}",
                entry.agent_id, entry.status, shape.stop
            ));
        }
    }
    if let Some(plane) = &run.plane {
        board_agrees(run, plane, true).await?;
        if let Some(error) = crate::boardagents::audit().await {
            return Err(error);
        }
    }
    Ok(())
}

async fn board_agrees(
    run: &Run,
    plane: &Arc<dyn Coordination>,
    settled: bool,
) -> Result<(), String> {
    let listed = run
        .registry
        .directory(ROOT_SESSION, true, false)
        .await
        .map_err(|error| format!("list_agents from the board failed: {error}"))?;
    let ids = listed
        .iter()
        .map(|entry| entry.agent_id)
        .collect::<Vec<_>>();
    let (waited, ..) = run
        .registry
        .wait(ROOT_SESSION, "board-agrees", &ids, Duration::ZERO)
        .await
        .map_err(|error| format!("wait_agent from the board failed: {error}"))?;
    run.registry.set_coordination(None);
    let registry = run.registry.directory(ROOT_SESSION, true, false).await;
    run.registry.set_coordination(Some(Arc::clone(plane)));
    let registry = registry.map_err(|error| format!("the registry did not list: {error}"))?;
    let board = plane
        .agents(ROOT_SESSION)
        .await
        .map_err(|error| format!("the board did not read: {error}"))?;
    let truth = |id| {
        registry
            .iter()
            .find(|entry| entry.agent_id == id)
            .map(|entry| &entry.status)
    };
    let finished = |status: &AgentStatus| {
        matches!(
            status,
            AgentStatus::Completed { .. } | AgentStatus::Failed { .. }
        )
    };
    if settled && listed.len() != registry.len() {
        return Err(format!(
            "list_agents from the board lists {} of the registry's {} children",
            listed.len(),
            registry.len()
        ));
    }
    let answers = listed
        .iter()
        .map(|entry| ("list_agents", entry.agent_id, &entry.status))
        .chain(
            waited
                .iter()
                .map(|summary| ("wait_agent", summary.agent_id, &summary.status)),
        );
    for entry in &registry {
        let read = board.get(&entry.agent_id);
        let agrees = match read {
            _ if !finished(&entry.status) => true,
            Some(Some(status)) => *status == entry.status,
            _ => !settled,
        };
        if !agrees {
            return Err(format!(
                "the board reads agent {} as {read:?}, the registry {:?}",
                entry.agent_id, entry.status
            ));
        }
    }
    for (tool, id, status) in answers {
        let expected = truth(id);
        let lagging = !settled && !finished(status);
        if expected != Some(status) && !lagging {
            return Err(format!(
                "{tool} from the board says agent {id} is {status:?}, the registry {expected:?}"
            ));
        }
    }
    Ok(())
}

fn pool_policy() -> Policy {
    Policy {
        roles: vec![Role {
            name: "worker".to_owned(),
            labels: [(POOL_KIND.to_owned(), POOL_WORK.to_owned())]
                .into_iter()
                .collect(),
            cost_nano_usd: 1,
            placement: "sim".to_owned(),
        }],
        member_label: POOL_MEMBER.to_owned(),
        project_label: POOL_PROJECT.to_owned(),
        intent_prefix: "dispatch/".to_owned(),
        budgets: Budgets {
            agents_per_member_max: POOLED_MAX,
            agents_per_project_max: POOLED_MAX,
            nano_usd_max: POOLED_MAX,
        },
        idle_retire_ms: POOL_IDLE_RETIRE_MS,
        start_timeout_ms: POOL_LEASE_MS,
        start_attempts_max: 1,
    }
}

fn shown(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

fn board_ms(clock: &Clock) -> u64 {
    POOL_CHARTER_MS + clock.now_ns() / 1_000_000
}

async fn pool_board(
    host: &Arc<dyn LogHost>,
    shape: &Shape,
    clock: &Clock,
) -> Result<Dispatcher, String> {
    let open = |actor: &str| Board::open(Arc::clone(host), POOL_SCOPE, identity(actor));
    open(POOL_OWNER)
        .await
        .map_err(shown)?
        .charter(POOL_CHARTER_MS)
        .await
        .map_err(shown)?;
    let mut poster = open(POOL_POSTER).await.map_err(shown)?;
    for index in 0..shape.pooled {
        let labels = [
            (POOL_KIND, POOL_WORK),
            (POOL_MEMBER, "member"),
            (POOL_PROJECT, "project"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        let task = NewTask {
            id: TaskId(format!("pooled-{index}")),
            parent: None,
            spec: format!("pooled task {index}"),
            labels,
            needs: Vec::new(),
            terms: Default::default(),
        };
        poster
            .submit(board_ms(clock), Op::Post { task })
            .await
            .map_err(|error| format!("the poster could not post pooled-{index}: {error:?}"))?;
    }
    let board = open(POOL_OWNER).await.map_err(shown)?;
    Ok(Dispatcher::new(board, pool_policy(), BRIEF))
}

async fn claim_work(
    host: &Arc<dyn LogHost>,
    worker: &str,
    task: &TaskId,
    at_ms: u64,
) -> Result<(), String> {
    let mut board = Board::open(Arc::clone(host), POOL_SCOPE, identity(worker))
        .await
        .map_err(shown)?;
    if !matches!(
        board.fold().task(task).map(|task| &task.phase),
        Some(Phase::Open)
    ) {
        return Ok(());
    }
    let claim = Op::Claim {
        task: task.clone(),
        lease_ms: POOL_LEASE_MS,
        fence: None,
        cap: None,
    };
    board
        .submit(at_ms, claim)
        .await
        .map_err(|error| format!("{worker} could not claim {task}: {error:?}"))?;
    Ok(())
}

async fn dispatch(
    dispatcher: &mut Dispatcher,
    pool: &SubagentPool,
    at_ms: u64,
) -> Result<(), String> {
    let pass = dispatcher
        .pass(pool, at_ms)
        .await
        .map_err(|error| format!("a dispatcher pass over the pool failed: {error:?}"))?;
    match pass.failures.first() {
        Some(failure) => Err(format!(
            "the dispatcher could not drive the pool: {failure:?}"
        )),
        None => Ok(()),
    }
}

async fn placed(
    host: &Arc<dyn LogHost>,
    dispatcher: &mut Dispatcher,
    pool: &SubagentPool,
    shape: &Shape,
    clock: &Clock,
) -> Result<(), String> {
    let policy = pool_policy();
    for _ in 0..POOL_PASSES_MAX {
        dispatch(dispatcher, pool, board_ms(clock)).await?;
        let intents = nanocode_dispatch::intents(dispatcher.board().fold(), &policy);
        let mut live = 0;
        for agent in pool.agents().await {
            let Some(intent) = intents
                .iter()
                .find(|intent| agent.intent.as_ref() == Some(&intent.id))
            else {
                continue;
            };
            let settled = IntentState::Live {
                agent: agent.id.clone(),
            };
            live += u64::from(intent.state == settled);
            let worker = dispatcher.worker_of(&agent.id);
            claim_work(host, &worker, &intent.task, board_ms(clock)).await?;
        }
        if live == shape.pooled {
            break;
        }
    }
    let intents = nanocode_dispatch::intents(dispatcher.board().fold(), &policy);
    let listed = pool.agents().await;
    if u64::try_from(intents.len()).expect("an intent count fits in u64") != shape.pooled {
        return Err(format!(
            "the dispatcher recorded {} intents for {} pooled tasks",
            intents.len(),
            shape.pooled
        ));
    }
    for intent in &intents {
        let IntentState::Live { agent } = &intent.state else {
            return Err(format!(
                "intent {} for {} is {:?} after {POOL_PASSES_MAX} passes",
                intent.id, intent.task, intent.state
            ));
        };
        let Some(listed) = listed.iter().find(|listed| listed.id == *agent) else {
            return Err(format!(
                "the board settled {} on {agent} but the pool does not list it",
                intent.id
            ));
        };
        if listed.intent.as_ref() != Some(&intent.id) {
            return Err(format!(
                "the pool lists {agent} under {:?}, the board settled it for {}",
                listed.intent, intent.id
            ));
        }
    }
    if listed.len() != intents.len() {
        return Err(format!(
            "the pool lists {} agents for {} intents",
            listed.len(),
            intents.len()
        ));
    }
    Ok(())
}

async fn pooled(run: &Run, shape: &Shape, clock: &Arc<Clock>) -> Result<Vec<AgentId>, String> {
    let now = Arc::clone(clock);
    let pool = SubagentPool::new(
        run.root.clone(),
        Arc::clone(&run.registry),
        ROOT_SESSION,
        Box::new(move || board_ms(&now)),
    );
    let host: Arc<dyn LogHost> = Arc::new(MemoryLog::new());
    let mut dispatcher = pool_board(&host, shape, clock).await?;
    placed(&host, &mut dispatcher, &pool, shape, clock).await?;
    let Some(first) = pool.agents().await.first().map(|agent| agent.id.clone()) else {
        return Err("the dispatcher placed no pooled agent".to_owned());
    };
    let Some(idled) = pool.agent_id(&first) else {
        return Err(format!("the pool forgot {first} before retiring it"));
    };
    run.registry
        .interrupt(ROOT_SESSION, idled)
        .await
        .map_err(|error| format!("interrupting {first} failed: {error}"))?;
    let idle = pool
        .agents()
        .await
        .into_iter()
        .filter_map(|agent| Some((pool.agent_id(&agent.id)?, agent.id)))
        .collect::<Vec<_>>();
    dispatch(
        &mut dispatcher,
        &pool,
        board_ms(clock) + POOL_IDLE_RETIRE_MS,
    )
    .await?;
    let kept = pool.agents().await;
    let gone = idle
        .iter()
        .filter(|(_, name)| !kept.iter().any(|agent| agent.id == *name))
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    if !gone.contains(&idled) {
        return Err(format!(
            "the dispatcher left {first} (agent {idled}) idle past idle_retire_ms"
        ));
    }
    let directory = run
        .registry
        .directory(ROOT_SESSION, true, false)
        .await
        .map_err(|error| format!("the directory did not answer after a retire: {error}"))?;
    for id in &gone {
        let status = directory
            .iter()
            .find(|entry| entry.agent_id == *id)
            .map(|entry| &entry.status);
        if status != Some(&AgentStatus::Closed) {
            return Err(format!(
                "the dispatcher retired agent {id} but the runtime has it {status:?}"
            ));
        }
    }
    Ok(gone)
}

/// Posts pooled work to the dispatcher the board hosts for the root, the
/// production path, and waits until it placed an agent on each task.
async fn hosted(shape: &Shape) -> Result<u64, String> {
    let Some(plane) = crate::boardagents::hosting_plane() else {
        return Ok(0);
    };
    if !plane.hosting().iter().any(|root| root == ROOT_SESSION) {
        return Err("the board hosts no dispatcher for the root session".to_owned());
    }
    for index in 0..shape.pooled {
        let labels = [
            (DISPATCH_LABEL, DISPATCH_ROLE),
            (POOL_MEMBER, "member"),
            (POOL_PROJECT, "project"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        let task = NewTask {
            id: TaskId(format!("hosted-{index}")),
            parent: None,
            spec: format!("hosted task {index}"),
            labels,
            needs: Vec::new(),
            terms: Default::default(),
        };
        plane.post_dispatch(ROOT_SESSION, task).await?;
    }
    let mut waiting = "no pass yet".to_owned();
    for _ in 0..HOSTED_PASSES_MAX {
        nanocode_platform::sleep(HOSTED_POLL).await;
        let Some(view) = plane.hosted(ROOT_SESSION).await? else {
            return Err("the hosted dispatcher stopped before its work was placed".to_owned());
        };
        match hosted_placed(&view, shape.pooled)? {
            None => return Ok(shape.pooled),
            Some(reason) => waiting = reason,
        }
    }
    Err(format!(
        "the hosted dispatcher did not place its work after {HOSTED_PASSES_MAX} polls: {waiting}"
    ))
}

/// `Err` when the hosted pool and its board disagree, `Ok(Some(_))` while
/// placement is still under way, `Ok(None)` once every task has its agent.
fn hosted_placed(view: &HostedView, want: u64) -> Result<Option<String>, String> {
    for agent in &view.agents {
        let Some(intent) = &agent.intent else {
            return Err(format!(
                "the hosted pool lists {} without the intent it was placed for",
                agent.id
            ));
        };
        if !view.intents.iter().any(|known| known.id == *intent) {
            return Err(format!(
                "the hosted pool lists {} under {intent}, which {} never recorded",
                agent.id, view.scope
            ));
        }
    }
    if u64::try_from(view.intents.len()).expect("an intent count fits in u64") != want {
        return Ok(Some(format!(
            "{} intents for {want} hosted tasks",
            view.intents.len()
        )));
    }
    for intent in &view.intents {
        let phase = view.phases.get(&intent.task.0);
        // A worker that claimed its task and then ended it was placed. The
        // scripted harness never calls submit_result, so each worker's turn
        // ends Failed once it runs out of steps, and a slow placement (a
        // lagging board costs the dispatcher a pass) lets the first workers
        // end before the last one claims.
        if matches!(
            phase,
            Some(Phase::Done { .. } | Phase::Failed { worker: Some(_), .. })
        ) {
            continue;
        }
        let IntentState::Live { agent } = &intent.state else {
            return Ok(Some(format!("intent {} is {:?}", intent.id, intent.state)));
        };
        let Some(listed) = view.agents.iter().find(|listed| listed.id == *agent) else {
            return Err(format!(
                "{} settled {} on {agent} but the hosted pool does not list it",
                view.scope, intent.id
            ));
        };
        if listed.intent.as_ref() != Some(&intent.id) {
            return Err(format!(
                "the hosted pool lists {agent} under {:?}, {} settled it for {}",
                listed.intent, view.scope, intent.id
            ));
        }
        match phase {
            Some(Phase::Claimed { worker, .. }) if worker.ends_with(&format!("+{agent}")) => {}
            Some(Phase::Claimed { worker, .. }) => {
                return Err(format!(
                    "{} is claimed by {worker}, not by {agent} that the dispatcher placed on it",
                    intent.task
                ));
            }
            other => return Ok(Some(format!("{} is {other:?}", intent.task))),
        }
    }
    Ok(None)
}

async fn steer_running(run: &Run) -> Result<(), String> {
    let listed = run
        .registry
        .directory(ROOT_SESSION, true, false)
        .await
        .map_err(|error| format!("the directory did not answer before steering: {error}"))?;
    for entry in listed {
        if entry.status == AgentStatus::Running {
            crate::boardagents::steer(ROOT_SESSION, entry.agent_id).await;
        }
    }
    Ok(())
}

async fn drive(shape: &Shape, ledger: &Arc<Ledger>, clock: &Arc<Clock>) -> Result<u64, String> {
    let run = build(ledger, shape);
    nanocode_subagents::start_agents(
        &run.root,
        &run.registry,
        ROOT_SESSION,
        tasks(shape.children),
    )
    .await
    .map_err(|error| format!("start_agents failed: {error}"))?;
    ledger.trace("started");
    let retired = pooled(&run, shape, clock).await?;
    ledger.trace("pooled");
    let hosted = hosted(shape).await?;
    if hosted > 0 {
        ledger.trace("hosted");
    }
    if run.plane.is_some() {
        steer_running(&run).await?;
        ledger.trace("steered");
    }
    nanocode_platform::sleep(shape.stop_after).await;
    if let Some(plane) = &run.plane {
        board_agrees(&run, plane, false).await?;
    }
    ledger.trace("stop");
    crate::boardagents::stop_hosting();
    let took_ms = stop(&run, shape, clock).await?;
    ledger.trace("stopped");
    nanocode_platform::sleep(RUNTIME_STOP_TIMEOUT).await;
    judge(&run, shape, ledger, &retired, hosted).await?;
    drop(run.updates);
    Ok(took_ms)
}

pub fn simulate(seed: u64, mutation: AgentsMutation) -> AgentOutcome {
    if mutation == AgentsMutation::BoardFailuresUnread {
        nanocode_board::sim::mutate(nanocode_board::sim::Mutation::SubagentFailuresUnread);
    }
    if mutation == AgentsMutation::PoolRetireKeepsAgent {
        nanocode_board::sim::mutate(nanocode_board::sim::Mutation::PoolRetireKeepsAgent);
    }
    if mutation == AgentsMutation::PoolForgetsIntent {
        nanocode_board::sim::mutate(nanocode_board::sim::Mutation::PoolForgetsIntent);
    }
    let shape = Shape::of(seed, mutation);
    let clock = Clock::new();
    crate::host::enter(seed, &clock);
    let ledger = Ledger::new(seed, shape.script, Arc::clone(&clock));
    let result = RefCell::new(None);
    let mut executor = Executor::new(Arc::clone(&clock), Rng::derive(seed, "scheduler"));
    let task: Task<'_> = Box::pin(async {
        let outcome = drive(&shape, &ledger, &clock).await;
        result.replace(Some(outcome));
    });
    let ended = executor.run(vec![task], None);
    let outcome = match (ended, result.into_inner()) {
        (_, Some(outcome)) => outcome,
        (Ended::Stuck, None) => Err("every task waits and no timer is set".to_owned()),
        (ended, None) => Err(format!("the run ended {ended:?} without a verdict")),
    };
    let stop_ms = *outcome.as_ref().unwrap_or(&0);
    nanocode_board::sim::mutate(nanocode_board::sim::Mutation::None);
    AgentOutcome {
        digest: ledger.digest(),
        wrong: outcome.err(),
        stats: AgentStats::of(&shape, ledger.counts(), stop_ms),
        shape: shape.describe(),
    }
}

pub fn run_seed(seed: u64, mutation: AgentsMutation) -> AgentOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default();
        AgentOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: AgentStats::default(),
            shape: String::new(),
        }
    })
}
