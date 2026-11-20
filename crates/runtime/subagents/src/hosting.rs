use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use nanocode_board::brief::BriefPolicy;
use nanocode_board::{Board, BoardError, NewTask, Op, Outcome, Phase, Rejection};
use nanocode_dispatch::{Agent, Budgets, Intent, IntentState, Policy, Role};
use nanocode_dispatcher::{BRIEF, Dispatcher, Pool, agent_name};
use nanocode_sync::Atom;
use nanocode_sync::future::{AbortHandle, abortable};
use nanocode_turnlog::LogHost;

use super::{identity, profile_of};
use crate::Registry;
use crate::pool::{Ended, PoolClock, SubagentPool};

pub const DISPATCH_LABEL: &str = "dispatch";
pub const DISPATCH_ROLE: &str = "subagent";
pub const DISPATCH_POLL_MS: u64 = 5_000;
pub const DISPATCH_LEASE_MS: u64 = 10 * 60 * 1_000;
pub const DISPATCH_IDLE_RETIRE_MS: u64 = 5 * 60 * 1_000;
pub const DISPATCH_POSTER: &str = "poster";
const READER: &str = "auditor";

#[derive(Clone)]
pub struct Dispatching {
    pub policy: Policy,
    pub brief: BriefPolicy,
    pub lease_ms: u64,
    pub poll_ms: u64,
    pub now_ms: PoolClock,
}

impl Default for Dispatching {
    fn default() -> Self {
        Self {
            policy: dispatch_policy(),
            brief: BRIEF,
            lease_ms: DISPATCH_LEASE_MS,
            poll_ms: DISPATCH_POLL_MS,
            now_ms: Arc::new(nanocode_platform::unix_millis),
        }
    }
}

pub fn dispatch_policy() -> Policy {
    Policy {
        roles: vec![Role {
            name: DISPATCH_ROLE.to_owned(),
            labels: BTreeMap::from([(DISPATCH_LABEL.to_owned(), DISPATCH_ROLE.to_owned())]),
            cost_nano_usd: 1,
            placement: "subagents".to_owned(),
        }],
        member_label: "member".to_owned(),
        project_label: "project".to_owned(),
        intent_prefix: "dispatch/".to_owned(),
        budgets: Budgets {
            agents_per_member_max: 4,
            agents_per_project_max: 8,
            nano_usd_max: 8,
        },
        idle_retire_ms: DISPATCH_IDLE_RETIRE_MS,
        start_timeout_ms: DISPATCH_LEASE_MS,
        start_attempts_max: 2,
    }
}

pub fn dispatch_scope(scope: &str, root: &str) -> String {
    format!("{scope}-dispatch-{}", profile_of(root))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostedView {
    pub scope: String,
    pub intents: Vec<Intent>,
    pub agents: Vec<Agent>,
    pub phases: BTreeMap<String, Phase>,
}

#[derive(Clone)]
struct Hosted {
    pool: Arc<SubagentPool>,
    policy: Policy,
    stop: AbortHandle,
}

pub(super) struct Hosts {
    host: Arc<dyn LogHost>,
    scope: String,
    registry: Atom<Option<Weak<Registry>>>,
    dispatching: Atom<Option<Dispatching>>,
    hosted: Atom<BTreeMap<String, Hosted>>,
    pools: Atom<BTreeMap<String, Arc<SubagentPool>>>,
}

impl Hosts {
    pub(super) fn new(host: Arc<dyn LogHost>, scope: &str) -> Self {
        Self {
            host,
            scope: scope.to_owned(),
            registry: Atom::new(None),
            dispatching: Atom::new(Some(Dispatching::default())),
            hosted: Atom::new(BTreeMap::new()),
            pools: Atom::new(BTreeMap::new()),
        }
    }

    pub(super) fn attach(&self, registry: Weak<Registry>) {
        self.registry.set(Some(registry));
    }

    pub(super) fn configure(&self, dispatching: Option<Dispatching>) {
        self.dispatching.set(dispatching);
        self.stop_all();
    }

    pub(super) fn host(&self, root: &str) {
        if self.hosted.read(|hosted| hosted.contains_key(root)) {
            return;
        }
        let Some(dispatching) = Option::clone(&self.dispatching.get()) else {
            return;
        };
        let registry = self
            .registry
            .read(|registry| registry.as_ref().and_then(Weak::upgrade));
        let Some(registry) = registry else {
            return;
        };
        let Some(parent) = registry.session_handle(root) else {
            return;
        };
        let placed = Arc::new(SubagentPool::clocked(
            parent,
            &registry,
            root,
            Arc::clone(&dispatching.now_ms),
        ));
        let pool = self.pools.update(|pools| {
            Arc::clone(
                pools
                    .entry(root.to_owned())
                    .or_insert_with(|| Arc::clone(&placed)),
            )
        });
        let scope = dispatch_scope(&self.scope, root);
        let (serving, stop) = abortable(serve(
            Arc::clone(&self.host),
            scope,
            Arc::clone(&pool),
            dispatching.clone(),
        ));
        let hosted = Hosted {
            pool,
            policy: dispatching.policy,
            stop,
        };
        let inserted = self.hosted.update(|all| {
            if all.contains_key(root) {
                return false;
            }
            all.insert(root.to_owned(), hosted.clone());
            true
        });
        if !inserted {
            return;
        }
        if !spawn(async move {
            let _ = serving.await;
        }) {
            self.hosted.update(|all| {
                all.remove(root);
            });
        }
    }

    pub(super) fn release(&self, root: &str) {
        self.pools.update(|pools| {
            pools.remove(root);
        });
        let stopped = self.hosted.update(|all| all.remove(root));
        if let Some(hosted) = stopped {
            hosted.stop.abort();
        }
    }

    pub(super) fn stop_all(&self) {
        let stopped = self.hosted.update(std::mem::take);
        for hosted in stopped.into_values() {
            hosted.stop.abort();
        }
    }

    pub(super) fn hosting(&self) -> Vec<String> {
        self.hosted.read(|hosted| hosted.keys().cloned().collect())
    }

    pub(super) async fn view(&self, root: &str) -> Result<Option<HostedView>, String> {
        let Some(hosted) = self.hosted.read(|hosted| hosted.get(root).cloned()) else {
            return Ok(None);
        };
        let scope = dispatch_scope(&self.scope, root);
        let board = Board::open(Arc::clone(&self.host), &scope, identity(Some(READER))?)
            .await
            .map_err(|error| format!("open {scope}: {error}"))?;
        let fold = board.fold();
        let intents = nanocode_dispatch::intents(fold, &hosted.policy);
        let phases = intents
            .iter()
            .filter_map(|intent| {
                let task = fold.task(&intent.task)?;
                Some((intent.task.0.clone(), task.phase.clone()))
            })
            .collect();
        let agents = hosted.pool.agents().await;
        Ok(Some(HostedView {
            scope,
            intents,
            agents,
            phases,
        }))
    }

    pub(super) async fn post(&self, root: &str, task: NewTask) -> Result<(), String> {
        let now = self
            .dispatching
            .read(|dispatching| {
                dispatching
                    .as_ref()
                    .map(|dispatching| (dispatching.now_ms)())
            })
            .ok_or_else(|| "dispatching is off".to_owned())?;
        let scope = dispatch_scope(&self.scope, root);
        let mut board = Board::open(
            Arc::clone(&self.host),
            &scope,
            identity(Some(DISPATCH_POSTER))?,
        )
        .await
        .map_err(|error| format!("open {scope}: {error}"))?;
        match board.submit(now, Op::Post { task }).await {
            Ok(_) | Err(BoardError::Rejected(Rejection::Exists)) => Ok(()),
            Err(error) => Err(format!("post to {scope}: {error}")),
        }
    }
}

impl Drop for Hosts {
    fn drop(&mut self) {
        self.stop_all();
    }
}

#[cfg(not(target_family = "wasm"))]
fn spawn(serving: impl Future<Output = ()> + Send + 'static) -> bool {
    let Ok(runtime) = nanocode_platform::Handle::try_current() else {
        return false;
    };
    runtime.spawn(serving);
    true
}

#[cfg(target_family = "wasm")]
fn spawn(serving: impl Future<Output = ()> + 'static) -> bool {
    nanocode_platform::spawn_local(serving);
    true
}

async fn nap(ms: u64) {
    nanocode_platform::sleep(Duration::from_millis(ms.max(1))).await;
}

async fn open_dispatcher(
    host: &Arc<dyn LogHost>,
    scope: &str,
    dispatching: &Dispatching,
) -> Result<Dispatcher, String> {
    let mut board = Board::open(Arc::clone(host), scope, identity(None)?)
        .await
        .map_err(|error| format!("open {scope}: {error}"))?;
    if board.fold().access().owner().is_none() {
        match board.charter((dispatching.now_ms)()).await {
            Ok(_) | Err(BoardError::Rejected(Rejection::Exists)) => {}
            Err(error) => return Err(format!("charter {scope}: {error}")),
        }
    }
    Ok(Dispatcher::new(
        board,
        dispatching.policy.clone(),
        dispatching.brief,
    ))
}

async fn serve(
    host: Arc<dyn LogHost>,
    scope: String,
    pool: Arc<SubagentPool>,
    dispatching: Dispatching,
) {
    let mut dispatcher = loop {
        match open_dispatcher(&host, &scope, &dispatching).await {
            Ok(dispatcher) => break dispatcher,
            Err(error) => {
                nanocode_trace::warn!(%scope, %error, "could not open the dispatch board");
                nap(dispatching.poll_ms).await;
            }
        }
    };
    loop {
        let now = (dispatching.now_ms)();
        let wake_at = match dispatcher.pass(&*pool, now).await {
            Ok(pass) => {
                for failure in &pass.failures {
                    nanocode_trace::warn!(%scope, ?failure, "a dispatch pass failed a step");
                }
                pass.wake_at_ms
            }
            Err(error) => {
                nanocode_trace::warn!(%scope, ?error, "a dispatch pass failed");
                None
            }
        };
        let ops = claims(&dispatcher, &pool, &dispatching, now);
        work(&host, &scope, ops, now).await;
        let poll = now.saturating_add(dispatching.poll_ms);
        let at = wake_at.map_or(poll, |at| at.min(poll));
        nap(at.saturating_sub(now)).await;
    }
}

fn claims(
    dispatcher: &Dispatcher,
    pool: &SubagentPool,
    dispatching: &Dispatching,
    now: u64,
) -> Vec<(String, Op)> {
    let fold = dispatcher.board().fold();
    let mut ops = Vec::new();
    for intent in nanocode_dispatch::intents(fold, &dispatching.policy) {
        // Claim the work as soon as the pool placed its agent: a running
        // agent is no supply to the planner, so work left open past the pass
        // that settles the intent would get a second intent.
        let agent = match &intent.state {
            IntentState::Live { agent } => agent.clone(),
            IntentState::Starting { .. } => agent_name(&intent.id),
            _ => continue,
        };
        if pool.agent_id(&agent).is_none() {
            continue;
        }
        let agent = &agent;
        let task = intent.task;
        let op = match fold.task(&task).map(|task| &task.phase) {
            Some(Phase::Open) if pool.ended(agent) != Some(Ended::Stopped) => Some(Op::Claim {
                task,
                lease_ms: dispatching.lease_ms,
                fence: None,
                cap: None,
            }),
            Some(Phase::Claimed {
                claim,
                worker,
                until_ms,
            }) if *worker == dispatcher.worker_of(agent) => {
                let claim = claim.clone();
                match pool.ended(agent) {
                    Some(Ended::Completed(output)) => Some(Op::Finish {
                        task,
                        claim,
                        outcome: Outcome::Done(output),
                    }),
                    Some(Ended::Failed(error)) => Some(Op::Finish {
                        task,
                        claim,
                        outcome: Outcome::Failed(error),
                    }),
                    Some(Ended::Stopped) => Some(Op::Release {
                        task,
                        claim,
                        note: format!("{agent} stopped"),
                    }),
                    None if until_ms.saturating_sub(now) < dispatching.lease_ms / 2 => {
                        Some(Op::Renew {
                            task,
                            claim,
                            lease_ms: dispatching.lease_ms,
                        })
                    }
                    None => None,
                }
            }
            _ => None,
        };
        if let Some(op) = op {
            ops.push((agent.clone(), op));
        }
    }
    ops
}

async fn work(host: &Arc<dyn LogHost>, scope: &str, ops: Vec<(String, Op)>, now: u64) {
    for (agent, op) in ops {
        let worker = match identity(Some(&agent)) {
            Ok(worker) => worker,
            Err(error) => {
                nanocode_trace::warn!(%scope, %agent, %error, "no worker identity");
                continue;
            }
        };
        let submitted = match Board::open(Arc::clone(host), scope, worker).await {
            Ok(mut board) => board.submit(now, op).await.map(drop),
            Err(error) => Err(error),
        };
        if let Err(error) = submitted {
            nanocode_trace::warn!(%scope, %agent, %error, "a pooled agent's board write failed");
        }
    }
}
