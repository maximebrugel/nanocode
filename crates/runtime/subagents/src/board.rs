use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};

use nanocode_board::access::{Identity, Verifier};
use nanocode_board::sim::{Mutation, mutated};
use nanocode_board::tasks::Terms;
use nanocode_board::{
    Applied, Board, BoardError, Change, ClaimId, EventId, NewTask, Op, Outcome, Phase, Rejection,
    TaskId, Write,
};
use nanocode_sync::{Owned, mpsc, oneshot, watch};
use nanocode_turnlog::LogHost;

use crate::model::{AgentId, AgentStatus, AgentUpdate, MessageSender};

#[path = "brief.rs"]
mod brief;
#[path = "hosting.rs"]
mod hosting;
#[path = "steers.rs"]
mod steers;

pub use hosting::{
    DISPATCH_LABEL, DISPATCH_POLL_MS, DISPATCH_POSTER, DISPATCH_ROLE, Dispatching, HostedView,
    dispatch_policy, dispatch_scope,
};

#[cfg(not(target_family = "wasm"))]
pub type ReadFuture<'a, T> = nanocode_sync::future::BoxFuture<'a, T>;

#[cfg(target_family = "wasm")]
pub type ReadFuture<'a, T> = nanocode_sync::future::LocalBoxFuture<'a, T>;

type Letter = Box<dyn for<'a> FnOnce(&'a mut Inner) -> ReadFuture<'a, ()> + Send>;

pub type BoardAgents = BTreeMap<AgentId, Option<AgentStatus>>;

pub trait Coordination: Send + Sync {
    fn observe(&self, root_session_id: &str, update: &AgentUpdate);
    fn changes(&self) -> watch::Receiver<u64>;
    fn agents<'a>(
        &'a self,
        root_session_id: &'a str,
    ) -> ReadFuture<'a, Result<BoardAgents, String>>;
    fn brief<'a>(&'a self, _session: &'a str) -> ReadFuture<'a, Result<Option<String>, String>> {
        Box::pin(std::future::ready(Ok(None)))
    }
    fn reconcile(
        &self,
        _root_session_id: &str,
        _finished: Vec<(AgentId, AgentStatus, Option<u64>)>,
    ) {
    }
    fn steers<'a>(&'a self, _session: &'a str) -> ReadFuture<'a, Result<Vec<String>, String>> {
        Box::pin(std::future::ready(Ok(Vec::new())))
    }
    fn attach(&self, _registry: std::sync::Weak<crate::Registry>) {}
    fn detach(&self) {}
    fn release_root(&self, _root_session_id: &str) {}
}

pub const LEASE_MS: u64 = 1 << 30;
pub const PRINCIPAL: &str = "nanocode:subagents";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoardStats {
    pub posts: u64,
    pub claims: u64,
    pub reclaims: u64,
    pub takeovers: u64,
    pub finishes: u64,
    pub finish_replays: u64,
    pub reuses: u64,
    pub replies: u64,
    pub steers: u64,
    pub delivered: u64,
    pub unheard: u64,
    pub missed: u64,
    pub cancels: u64,
    pub fenced: u64,
    pub late: u64,
    pub deferred: u64,
}

impl BoardStats {
    pub const fn add(&mut self, other: &Self) {
        self.posts += other.posts;
        self.claims += other.claims;
        self.reclaims += other.reclaims;
        self.takeovers += other.takeovers;
        self.finishes += other.finishes;
        self.finish_replays += other.finish_replays;
        self.reuses += other.reuses;
        self.replies += other.replies;
        self.steers += other.steers;
        self.delivered += other.delivered;
        self.unheard += other.unheard;
        self.missed += other.missed;
        self.cancels += other.cancels;
        self.fenced += other.fenced;
        self.late += other.late;
        self.deferred += other.deferred;
    }
}

struct Named;

impl Verifier for Named {
    fn verify(&self, credential: &str) -> Option<String> {
        Some(credential.to_owned())
    }
}

pub fn profile_of(session: &str) -> String {
    let plain = !session.is_empty()
        && !session.starts_with("x-")
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    if plain {
        return session.to_owned();
    }
    let mut hex = String::from("x-");
    for byte in session.bytes() {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn session_task(
    root: &str,
    session: &str,
    agent: AgentId,
    parent: &TaskId,
    k: u64,
    spec: &str,
) -> NewTask {
    NewTask {
        id: task_of(session, k),
        parent: Some(parent.clone()),
        spec: spec.to_owned(),
        labels: BTreeMap::from([
            ("session".to_owned(), session.to_owned()),
            ("root".to_owned(), root.to_owned()),
            ("agent".to_owned(), agent.to_string()),
        ]),
        needs: Vec::new(),
        terms: Terms::default(),
    }
}

pub fn worker_of(session: &str) -> String {
    format!("{PRINCIPAL}+{}", profile_of(session))
}

fn identity(profile: Option<&str>) -> Result<Identity, String> {
    let owner = Identity::verify(&Named, PRINCIPAL).map_err(|denied| format!("{denied:?}"))?;
    match profile {
        Some(profile) => owner
            .profile(profile)
            .map_err(|denied| format!("{denied:?}")),
        None => Ok(owner),
    }
}

struct Session {
    parent: TaskId,
    k: u64,
    claim: Option<ClaimId>,
    heard: Option<u64>,
    boundaries: u64,
    sends: Vec<u64>,
    last_output: Option<Option<String>>,
}

impl Session {
    fn task(&self, session: &str) -> TaskId {
        task_of(session, self.k)
    }
}

fn task_of(session: &str, k: u64) -> TaskId {
    TaskId(format!("{session}/{k}"))
}

fn root_task(root: &str) -> TaskId {
    TaskId(format!("root:{root}"))
}

struct Inner {
    host: Arc<dyn LogHost>,
    scope: String,
    clock: u64,
    dispatcher: Option<Board>,
    workers: BTreeMap<String, Board>,
    sessions: BTreeMap<String, Session>,
    agents: BTreeMap<(String, AgentId), String>,
    roots: BTreeSet<String>,
    stats: BoardStats,
    errors: Vec<String>,
    changes: watch::Sender<u64>,
    brief: Option<nanocode_board::brief::BriefPolicy>,
    hosts: Arc<hosting::Hosts>,
    held: Option<Held>,
}

struct Held {
    session: String,
    task: TaskId,
    key: String,
    write: Write,
    last: Option<String>,
}

struct Desk {
    inner: Inner,
    letters: mpsc::UnboundedReceiver<Letter>,
}

impl Desk {
    async fn drain(&mut self) {
        while let Ok(letter) = self.letters.try_recv() {
            letter(&mut self.inner).await;
        }
        self.inner.settle().await;
    }
}

pub struct BoardCoordination {
    hosts: Arc<hosting::Hosts>,
    desk: Arc<Owned<Desk>>,
    letters: mpsc::UnboundedSender<Letter>,
    draining: Arc<AtomicBool>,
    changes: watch::Sender<u64>,
}

fn polled_once<F: Future>(future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

async fn open(
    host: &Arc<dyn LogHost>,
    scope: &str,
    profile: Option<&str>,
) -> Result<Option<Board>, String> {
    let identity = identity(profile)?;
    let opening = Board::open(Arc::clone(host), scope, identity);
    let opened = if mutated(Mutation::BoardPolledOnce) && profile != Some("auditor") {
        match polled_once(opening) {
            Some(opened) => opened,
            None => return Ok(None),
        }
    } else {
        opening.await
    };
    opened.map(Some).map_err(|error| error.to_string())
}

impl BoardCoordination {
    /// Sets how each root's hosted dispatcher places pooled agents, or turns
    /// hosting off with `None`. Running dispatchers stop and restart under
    /// the new settings at the root's next update.
    pub fn dispatching(&self, dispatching: Option<Dispatching>) {
        self.hosts.configure(dispatching);
    }

    /// Roots with a dispatcher running.
    pub fn hosting(&self) -> Vec<String> {
        self.hosts.hosting()
    }

    /// The intents and pool of the dispatcher hosted for `root`.
    pub async fn hosted(&self, root: &str) -> Result<Option<HostedView>, String> {
        self.hosts.view(root).await
    }

    /// Posts work for `root`'s hosted dispatcher to place a pooled agent on.
    pub async fn post_dispatch(&self, root: &str, task: NewTask) -> Result<(), String> {
        self.hosts.post(root, task).await
    }

    pub fn new(host: Arc<dyn LogHost>, scope: &str) -> Arc<Self> {
        let changes = watch::Sender::new(0);
        let (letters, received) = mpsc::unbounded_channel();
        let hosts = Arc::new(hosting::Hosts::new(Arc::clone(&host), scope));
        Arc::new(Self {
            hosts: Arc::clone(&hosts),
            changes: changes.clone(),
            letters,
            draining: Arc::new(AtomicBool::new(false)),
            desk: Arc::new(Owned::new(Desk {
                inner: Inner {
                    host,
                    scope: scope.to_owned(),
                    clock: 0,
                    dispatcher: None,
                    workers: BTreeMap::new(),
                    sessions: BTreeMap::new(),
                    agents: BTreeMap::new(),
                    roots: BTreeSet::new(),
                    stats: BoardStats::default(),
                    errors: Vec::new(),
                    changes,
                    brief: None,
                    hosts,
                    held: None,
                },
                letters: received,
            })),
        })
    }

    fn send(&self, letter: Letter) -> bool {
        if self.letters.send(letter).is_err() {
            return true;
        }
        if self.draining.swap(true, Ordering::AcqRel) {
            return true;
        }
        let desk = Arc::clone(&self.desk);
        let draining = Arc::clone(&self.draining);
        let drained = async move {
            let mut desk = desk.acquire_owned().await;
            draining.store(false, Ordering::Release);
            desk.drain().await;
        };
        self.spawn(drained)
    }

    #[cfg(not(target_family = "wasm"))]
    fn spawn(&self, drained: impl Future<Output = ()> + Send + 'static) -> bool {
        let Ok(runtime) = nanocode_platform::Handle::try_current() else {
            self.draining.store(false, Ordering::Release);
            return false;
        };
        runtime.spawn(drained);
        true
    }

    #[cfg(target_family = "wasm")]
    fn spawn(&self, drained: impl Future<Output = ()> + 'static) -> bool {
        nanocode_platform::spawn_local(drained);
        true
    }

    async fn call<R, F>(&self, job: F) -> Option<R>
    where
        R: Send + 'static,
        F: for<'a> FnOnce(&'a mut Inner) -> ReadFuture<'a, R> + Send + 'static,
    {
        let (reply, replied) = oneshot::channel();
        let spawned = self.send(Box::new(move |inner| {
            Box::pin(async move {
                inner.settle().await;
                let output = job(inner).await;
                drop(reply.send(output));
            })
        }));
        if !spawned {
            self.desk.acquire().await.drain().await;
        }
        replied.await.ok()
    }

    pub async fn stats(&self) -> BoardStats {
        self.call(|inner| Box::pin(async move { inner.stats() }))
            .await
            .unwrap_or_default()
    }

    pub async fn errors(&self) -> Vec<String> {
        self.call(|inner| Box::pin(async move { inner.errors() }))
            .await
            .unwrap_or_else(|| vec!["the board coordination task stopped".to_owned()])
    }

    pub async fn terminal_after(&self, cursor: u64) -> (Vec<(TaskId, Phase)>, u64) {
        self.call(move |inner| Box::pin(inner.terminal_after(cursor)))
            .await
            .unwrap_or((Vec::new(), cursor))
    }

    pub async fn children(&self, parent: &str) -> Vec<String> {
        let parent = parent.to_owned();
        self.call(move |inner| Box::pin(async move { inner.children(&parent).await }))
            .await
            .unwrap_or_default()
    }

    pub async fn audit(&self) -> Result<BoardStats, String> {
        self.call(|inner| Box::pin(inner.audit()))
            .await
            .unwrap_or_else(|| Err("the board coordination task stopped".to_owned()))
    }
}

impl Inner {
    fn stats(&self) -> BoardStats {
        self.stats.clone()
    }

    fn errors(&self) -> Vec<String> {
        self.errors.clone()
    }

    async fn terminal_after(&mut self, cursor: u64) -> (Vec<(TaskId, Phase)>, u64) {
        let Some(board) = self.dispatcher_board().await else {
            return (Vec::new(), cursor);
        };
        let _ = board.catch_up().await;
        let fold = board.fold();
        let terminal = fold
            .entries()
            .iter()
            .filter(|entry| entry.seq > cursor && entry.change.terminal())
            .filter_map(|entry| {
                fold.task(&entry.task)
                    .map(|task| (entry.task.clone(), task.phase.clone()))
            })
            .collect();
        (terminal, fold.next().saturating_sub(1))
    }

    async fn children(&mut self, parent: &str) -> Vec<String> {
        let Some(board) = self.dispatcher_board().await else {
            return Vec::new();
        };
        let _ = board.catch_up().await;
        let prefix = format!("{parent}/");
        let root = root_task(parent);
        board
            .fold()
            .tasks()
            .iter()
            .filter(|(id, task)| {
                id.0.ends_with("/1")
                    && task
                        .spec
                        .parent
                        .as_ref()
                        .is_some_and(|up| up.0.starts_with(&prefix) || *up == root)
            })
            .filter_map(|(_, task)| task.spec.labels.get("session").cloned())
            .collect()
    }

    async fn agents(&mut self, root: &str) -> Result<BoardAgents, String> {
        let Some(board) = self.dispatcher_board().await else {
            return Err("the subagent board is not open".to_owned());
        };
        // Agents' statuses overlay the runtime's own: a stale fold would report finished
        // agents as running.
        if let Err(error) = board.catch_up().await {
            return Err(format!("the subagent board did not refresh: {error}"));
        }
        let fold = board.fold();
        let mut latest = BTreeMap::<AgentId, (u64, &Phase)>::new();
        for (id, task) in fold.tasks() {
            if task.spec.labels.get("root").map(String::as_str) != Some(root) {
                continue;
            }
            let Some(agent) = task.spec.labels.get("agent") else {
                continue;
            };
            let agent = agent
                .parse::<u64>()
                .map(AgentId::new)
                .map_err(|error| format!("board task {id} has agent label {agent:?}: {error}"))?;
            let name = id.0.as_str();
            let (_, k) = name
                .rsplit_once('/')
                .ok_or_else(|| format!("board task {id} has no attempt number"))?;
            let k = k
                .parse::<u64>()
                .map_err(|error| format!("board task {id} has attempt {k:?}: {error}"))?;
            if latest.get(&agent).is_none_or(|(seen, _)| *seen < k) {
                latest.insert(agent, (k, &task.phase));
            }
        }
        latest
            .into_iter()
            .map(|(agent, (_, phase))| Ok((agent, finished(phase)?)))
            .collect()
    }

    async fn audit(&self) -> Result<BoardStats, String> {
        if let Some(error) = self.errors.first() {
            return Err(format!("board coordination error: {error}"));
        }
        let board = match open(&self.host, &self.scope, Some("auditor")).await {
            Ok(Some(board)) => board,
            Ok(None) => return Err("board audit open is pending".to_owned()),
            Err(error) => return Err(format!("board audit open: {error}")),
        };
        let fold = board.fold();
        if fold.access().owner() != Some(PRINCIPAL) && !fold.tasks().is_empty() {
            return Err(format!("board owner is {:?}", fold.access().owner()));
        }
        for entry in fold.entries() {
            if let Change::Claimed { worker, .. } = &entry.change {
                let session = fold
                    .task(&entry.task)
                    .and_then(|task| task.spec.labels.get("session"));
                if session.map(|session| worker_of(session)).as_ref() != Some(worker) {
                    return Err(format!(
                        "board claim of {} by {worker}, which is not its session {session:?}",
                        entry.task
                    ));
                }
            }
        }
        for (id, task) in fold.tasks() {
            if let Some(parent) = &task.spec.parent
                && fold.task(parent).is_none()
            {
                return Err(format!("board task {id} has no parent {parent}"));
            }
        }
        for (session, state) in &self.sessions {
            let id = state.task(session);
            if fold.task(&id).is_none() {
                return Err(format!(
                    "board has no task {id} for session {session}, which the registry added"
                ));
            }
        }
        for (session, state) in &self.sessions {
            let Some(expected) = &state.last_output else {
                continue;
            };
            let id = state.task(session);
            let phase = fold.task(&id).map(|task| &task.phase);
            let matches = match (expected, phase) {
                (Some(output), Some(Phase::Done { result, .. })) => result == output,
                (None, Some(Phase::Failed { .. })) => true,
                _ => false,
            };
            if !matches {
                return Err(format!(
                    "board task {id} is {phase:?}, but the registry reported {expected:?}"
                ));
            }
        }
        Ok(self.stats.clone())
    }
}

fn finished(phase: &Phase) -> Result<Option<AgentStatus>, String> {
    match phase {
        Phase::Done { result, .. } => serde_json::from_str(result)
            .map(|output| Some(AgentStatus::Completed { output }))
            .map_err(|error| format!("board result {result:?} is not JSON: {error}")),
        Phase::Failed { .. } if mutated(Mutation::SubagentFailuresUnread) => Ok(None),
        Phase::Failed { error, .. } => Ok(Some(AgentStatus::Failed {
            error: error.clone(),
        })),
        Phase::Cancelled { .. } => Ok(Some(AgentStatus::Closed)),
        _ => Ok(None),
    }
}

impl Inner {
    const fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn fail(&mut self, error: String) {
        if self.errors.len() < 16 {
            self.errors.push(error);
        }
    }

    async fn settle(&mut self) {
        let Some(held) = self.held.take() else {
            return;
        };
        if !self.workers.contains_key(&held.session) {
            match open(&self.host, &self.scope, Some(&profile_of(&held.session))).await {
                Ok(Some(board)) => {
                    self.workers.insert(held.session.clone(), board);
                }
                Ok(None) => return,
                Err(error) => {
                    self.fail(format!("open worker {}: {error}", held.session));
                    return;
                }
            }
        }
        let Some(board) = self.workers.get_mut(&held.session) else {
            return;
        };
        let _ = board.catch_up().await;
        let before = board.fold().next();
        let submitted = board
            .submit_keyed(&held.key, held.write.at_ms, held.write.op.clone())
            .await;
        self.finished(held, submitted, before);
    }

    fn finished(&mut self, held: Held, submitted: Result<Applied, BoardError>, before: u64) {
        let Held {
            session,
            task,
            key,
            last,
            ..
        } = held;
        match submitted {
            Ok(applied) => {
                if applied.seq < before {
                    self.stats.finish_replays += 1;
                } else {
                    self.stats.finishes += 1;
                }
                if let Some(state) = self.sessions.get_mut(&session) {
                    state.last_output = Some(last);
                    state.claim = None;
                    state.heard = None;
                }
            }
            Err(BoardError::Rejected(Rejection::NotHolder)) => {
                self.stats.fenced += 1;
                self.fail(format!(
                    "finish {task}: fenced, {session} no longer holds the claim"
                ));
            }
            Err(error) => self.fail(format!("finish {task} ({key}): {error}")),
        }
    }

    async fn unhold(&mut self, held: Option<Held>) {
        if held.is_some() {
            self.held = held;
            self.settle().await;
        }
    }

    async fn dispatcher_board(&mut self) -> Option<&mut Board> {
        self.settle().await;
        if self.dispatcher.is_none() {
            match open(&self.host, &self.scope, None).await {
                Ok(Some(mut board)) => {
                    let now = board.fold().now_ms();
                    let start = if now > 0 { now + LEASE_MS + 1 } else { 0 };
                    self.clock = self.clock.max(start) + 1;
                    if board.fold().access().owner().is_none() {
                        match board.charter(self.clock).await {
                            Ok(_) | Err(BoardError::Rejected(Rejection::Exists)) => {}
                            Err(error) => {
                                self.fail(format!("charter: {error}"));
                                return None;
                            }
                        }
                    }
                    self.dispatcher = Some(board);
                    self.reopen().await;
                    if let Err(error) = self.restored() {
                        self.fail(error);
                    }
                }
                Ok(None) => return None,
                Err(error) => {
                    self.fail(format!("open dispatcher: {error}"));
                    return None;
                }
            }
        }
        self.dispatcher.as_mut()
    }

    fn restored(&self) -> Result<(), String> {
        let Some(board) = self.dispatcher.as_ref() else {
            return Ok(());
        };
        for (id, task) in board.fold().tasks() {
            let labels = &task.spec.labels;
            let Some(root) = labels.get("root") else {
                continue;
            };
            let (Some(session), Some(agent)) = (labels.get("session"), labels.get("agent")) else {
                if !self.roots.contains(root) {
                    return Err(format!(
                        "reopen did not restore root {root} from board task {id}"
                    ));
                }
                continue;
            };
            let k =
                id.0.strip_prefix(session.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    .and_then(|k| k.parse::<u64>().ok());
            let (Some(k), Ok(agent)) = (k, agent.parse::<u64>()) else {
                continue;
            };
            if self.agents.get(&(root.clone(), AgentId::new(agent))) != Some(session) {
                return Err(format!(
                    "reopen did not restore agent {agent} of {root} from board task {id}"
                ));
            }
            if self.sessions.get(session).is_none_or(|state| state.k < k) {
                return Err(format!(
                    "reopen did not restore session {session} at attempt {k} from board task {id}"
                ));
            }
        }
        Ok(())
    }

    async fn reopen(&mut self) {
        if mutated(Mutation::ReopenForgetsAgents) {
            return;
        }
        let Some(board) = self.dispatcher.as_mut() else {
            return;
        };
        // A fold that missed rows would forget the agents posted in them.
        if let Err(error) = board.catch_up().await {
            self.fail(format!(
                "reopen: the subagent board did not refresh: {error}"
            ));
            return;
        }
        let mut roots = Vec::new();
        let mut attempts = Vec::new();
        let mut errors = Vec::new();
        for (id, task) in board.fold().tasks() {
            let labels = &task.spec.labels;
            let Some(root) = labels.get("root") else {
                continue;
            };
            let (Some(session), Some(agent)) = (labels.get("session"), labels.get("agent")) else {
                roots.push(root.clone());
                continue;
            };
            let k =
                id.0.strip_prefix(session.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    .and_then(|k| k.parse::<u64>().ok());
            let (Some(k), Ok(agent), Some(parent)) =
                (k, agent.parse::<u64>(), task.spec.parent.clone())
            else {
                errors.push(format!(
                    "reopen: board task {id} has labels {labels:?} and parent {:?}",
                    task.spec.parent
                ));
                continue;
            };
            attempts.push((
                root.clone(),
                AgentId::new(agent),
                session.clone(),
                parent,
                k,
            ));
        }
        self.roots.extend(roots);
        for error in errors {
            self.fail(error);
        }
        for (root, agent, session, parent, k) in attempts {
            self.agents.insert((root, agent), session.clone());
            let state = self.sessions.entry(session).or_insert(Session {
                parent: parent.clone(),
                k,
                claim: None,
                heard: None,
                boundaries: 0,
                sends: Vec::new(),
                last_output: None,
            });
            if k > state.k {
                state.k = k;
                state.parent = parent;
            }
        }
    }

    async fn worker(&mut self, session: &str) -> Option<&mut Board> {
        self.settle().await;
        if !self.workers.contains_key(session) {
            match open(&self.host, &self.scope, Some(&profile_of(session))).await {
                Ok(Some(board)) => {
                    self.workers.insert(session.to_owned(), board);
                }
                Ok(None) => return None,
                Err(error) => {
                    self.fail(format!("open worker {session}: {error}"));
                    return None;
                }
            }
        }
        self.workers.get_mut(session)
    }

    async fn dispatch(&mut self, key: &str, op: Op) -> Option<Result<(Applied, bool), BoardError>> {
        let at = self.tick();
        let board = self.dispatcher_board().await?;
        let _ = board.catch_up().await;
        let before = board.fold().next();
        let result = board.submit_keyed(key, at, op).await;
        Some(result.map(|applied| {
            let replay = applied.seq < before;
            (applied, replay)
        }))
    }

    async fn phase(&mut self, task: &TaskId) -> Option<Phase> {
        let board = self.dispatcher_board().await?;
        let _ = board.refresh().await;
        board.fold().task(task).map(|task| task.phase.clone())
    }

    async fn ensure_root(&mut self, root: &str) -> TaskId {
        self.hosts.host(root);
        let id = root_task(root);
        if self.roots.insert(root.to_owned()) {
            let task = NewTask {
                id: id.clone(),
                parent: None,
                spec: format!("root session {root}"),
                labels: BTreeMap::from([("root".to_owned(), root.to_owned())]),
                needs: Vec::new(),
                terms: Terms::default(),
            };
            match self
                .dispatch(&format!("post:{id}"), Op::Post { task })
                .await
            {
                Some(Ok((_, false))) => self.stats.posts += 1,
                Some(Ok((_, true))) | None => {}
                Some(Err(error)) => self.fail(format!("post {id}: {error}")),
            }
        }
        id
    }

    async fn post(
        &mut self,
        root: &str,
        session: &str,
        agent: AgentId,
        parent: &TaskId,
        k: u64,
        spec: &str,
    ) {
        let id = task_of(session, k);
        let task = session_task(root, session, agent, parent, k, spec);
        match self
            .dispatch(&format!("post:{id}"), Op::Post { task })
            .await
        {
            Some(Ok((_, false))) => self.stats.posts += 1,
            Some(Ok((_, true))) | None => {}
            Some(Err(error)) => self.fail(format!("post {id}: {error}")),
        }
    }

    /// Posts a session's task k and claims it for the session's worker as one board group, so
    /// both share one append instead of taking one each. Each op keeps its own verdict and the
    /// keys the two-step path uses (post:{id} by the dispatcher, claim:{id}:1 by the worker), so a
    /// retry of either path replays rather than doubles. Returns None, having written nothing,
    /// when the task is already on the board (a restore or replay): the caller then takes the
    /// two-step path, which reads the task's phase before claiming.
    #[allow(clippy::too_many_arguments)]
    async fn post_claim(
        &mut self,
        root: &str,
        session: &str,
        agent: AgentId,
        parent: &TaskId,
        k: u64,
        spec: &str,
        held: Option<Held>,
    ) -> Option<Option<ClaimId>> {
        let id = task_of(session, k);
        let task = session_task(root, session, agent, parent, k, spec);
        let worker = worker_of(session);
        if self.worker(session).await.is_none() {
            self.unhold(held).await;
            return None;
        }
        let post_at = self.tick();
        let claim_at = self.tick();
        let Some(board) = self.dispatcher_board().await else {
            self.unhold(held).await;
            return None;
        };
        let _ = board.catch_up().await;
        if board.fold().task(&id).is_some() {
            self.unhold(held).await;
            return None;
        }
        let before = board.fold().next();
        let mut writes: Vec<Write> = held.iter().map(|held| held.write.clone()).collect();
        writes.extend([
            Write {
                by: None,
                key: Some(format!("post:{id}")),
                at_ms: post_at,
                op: Op::Post { task },
            },
            Write {
                by: Some(worker.clone()),
                key: Some(format!("claim:{id}:1")),
                at_ms: claim_at,
                op: Op::Claim {
                    task: id.clone(),
                    lease_ms: LEASE_MS,
                    fence: None,
                    cap: None,
                },
            },
        ]);
        let (mut verdicts, _) = board.submit_group(writes).await;
        let claimed = verdicts.pop();
        let posted = verdicts.pop();
        let finished = verdicts.pop();
        let claim = match &claimed {
            Some(Ok(_)) => board.fold().entries().iter().rev().find_map(|entry| {
                match &entry.change {
                    Change::Claimed {
                        claim,
                        worker: holder,
                        ..
                    } if entry.task == id && *holder == worker => Some(claim.clone()),
                    _ => None,
                }
            }),
            _ => None,
        };
        match posted {
            Some(Ok(applied)) if applied.seq >= before => self.stats.posts += 1,
            Some(Ok(_)) | None => {}
            Some(Err(error)) => self.fail(format!("post {id}: {error}")),
        }
        if let Some(held) = held {
            let verdict =
                finished.unwrap_or_else(|| Err(BoardError::Dropped("unsettled".into())));
            self.finished(held, verdict, before);
        }
        match claimed {
            Some(Ok(_)) => self.stats.claims += 1,
            Some(Err(BoardError::Rejected(Rejection::Terminal))) => {}
            Some(Err(error)) => self.fail(format!("claim {id} for {session}: {error}")),
            None => {}
        }
        Some(claim)
    }

    fn claimed(&mut self, session: &str, claim: Option<ClaimId>) {
        if let Some(state) = self.sessions.get_mut(session) {
            if state.claim != claim {
                state.heard = None;
            }
            state.claim = claim;
        }
    }

    async fn claim(&mut self, session: &str, restored: bool) {
        let Some(state) = self.sessions.get(session) else {
            return;
        };
        let id = state.task(session);
        let at = self.tick();
        let worker = worker_of(session);
        let Some(board) = self.worker(session).await else {
            return;
        };
        let _ = board.catch_up().await;
        let (phase, attempts) = board
            .fold()
            .task(&id)
            .map(|task| (task.phase.clone(), task.attempts))
            .unzip();
        let mut claims = 0;
        let mut took_over = false;
        let mut failure = None;
        let claim =
            match phase {
                Some(Phase::Claimed {
                    claim,
                    worker: holder,
                    until_ms,
                }) if holder == worker && until_ms > at => Some(claim),
                Some(phase) if phase.terminal() => None,
                Some(_) => {
                    let key = format!("claim:{id}:{}", attempts.unwrap_or(0) + 1);
                    match board
                        .submit_keyed(
                            &key,
                            at,
                            Op::Claim {
                                task: id.clone(),
                                lease_ms: LEASE_MS,
                                fence: None,
                                cap: None,
                            },
                        )
                        .await
                    {
                        Ok(_) => {
                            claims = 1;
                            board.fold().entries().iter().rev().find_map(|entry| {
                                match &entry.change {
                                    Change::Claimed {
                                        claim,
                                        took_over: over,
                                        worker: holder,
                                        ..
                                    } if entry.task == id && *holder == worker => {
                                        took_over = over.is_some();
                                        Some(claim.clone())
                                    }
                                    _ => None,
                                }
                            })
                        }
                        // A refresh that failed leaves a stale fold: the task can have finished
                        // since, and a finished task has nothing to claim.
                        Err(BoardError::Rejected(Rejection::Terminal)) => None,
                        Err(error) => {
                            failure = Some(format!("claim {id} for {session}: {error}"));
                            None
                        }
                    }
                }
                None => {
                    failure = Some(format!("claim {id}: the task was never posted"));
                    None
                }
            };
        if restored && claim.is_some() {
            self.stats.reclaims += 1;
        }
        self.stats.claims += claims;
        if took_over {
            self.stats.takeovers += 1;
        }
        if let Some(failure) = failure {
            self.fail(failure);
        }
        if let Some(state) = self.sessions.get_mut(session) {
            if state.claim != claim {
                state.heard = None;
            }
            state.claim = claim;
        }
    }

    async fn added(
        &mut self,
        root: &str,
        id: AgentId,
        session: &str,
        parent: Option<AgentId>,
        spec: &str,
    ) {
        self.agents
            .insert((root.to_owned(), id), session.to_owned());
        if let Some(state) = self.sessions.get(session) {
            if state.claim.is_none() {
                self.claim(session, true).await;
            }
            return;
        }
        let root_id = self.ensure_root(root).await;
        let parent_task = parent
            .and_then(|parent| self.agents.get(&(root.to_owned(), parent)).cloned())
            .and_then(|parent| self.sessions.get(&parent).map(|state| state.task(&parent)))
            .unwrap_or(root_id);
        let mut k = 0;
        if let Some(board) = self.dispatcher_board().await {
            let _ = board.catch_up().await;
            while board.fold().task(&task_of(session, k + 1)).is_some() {
                k += 1;
            }
        }
        let restored = k > 0;
        let mut grouped = None;
        if !restored {
            k = 1;
            grouped = self
                .post_claim(root, session, id, &parent_task, k, spec, None)
                .await;
            if grouped.is_none() {
                self.post(root, session, id, &parent_task, k, spec).await;
            }
        }
        self.sessions.insert(
            session.to_owned(),
            Session {
                parent: parent_task,
                k,
                claim: None,
                heard: None,
                boundaries: 0,
                sends: Vec::new(),
                last_output: None,
            },
        );
        match grouped {
            Some(claim) => self.claimed(session, claim),
            None => self.claim(session, restored).await,
        }
    }

    async fn session_of(&mut self, root: &str, id: AgentId) -> Option<String> {
        if self.dispatcher.is_none() {
            let _ = self.dispatcher_board().await;
        }
        self.agents.get(&(root.to_owned(), id)).cloned()
    }

    async fn running(&mut self, root: &str, id: AgentId, session: &str) {
        let Some(state) = self.sessions.get(session) else {
            return;
        };
        let current = state.task(session);
        let parent = state.parent.clone();
        let k = state.k;
        let held = self
            .held
            .take_if(|held| held.session == session && held.task == current);
        let mut grouped = match held {
            Some(held) => {
                self.post_claim(root, session, id, &parent, k + 1, "reused", Some(held))
                    .await
            }
            None => None,
        };
        if grouped.is_none() {
            match self.phase(&current).await {
                Some(Phase::Cancelled { .. }) => {
                    self.stats.late += 1;
                    return;
                }
                Some(phase) if phase.terminal() => {}
                _ => return,
            }
            grouped = self
                .post_claim(root, session, id, &parent, k + 1, "reused", None)
                .await;
            if grouped.is_none() {
                self.post(root, session, id, &parent, k + 1, "reused").await;
            }
        }
        self.stats.reuses += 1;
        if let Some(state) = self.sessions.get_mut(session) {
            state.k = k + 1;
            state.claim = None;
            state.heard = None;
            state.last_output = None;
        }
        match grouped {
            Some(claim) => self.claimed(session, claim),
            None => self.claim(session, false).await,
        }
    }

    async fn finish(
        &mut self,
        root: &str,
        id: AgentId,
        session: &str,
        status: &AgentStatus,
        revision: Option<u64>,
    ) {
        let outcome = match status {
            AgentStatus::Completed { output } => Outcome::Done(output.to_string()),
            AgentStatus::Failed { error } => Outcome::Failed(error.clone()),
            _ => return,
        };
        let mut key = match revision {
            Some(revision) => format!("finish:{session}:{revision}"),
            None => format!(
                "finish:{session}:k{}",
                self.sessions.get(session).map_or(0, |state| state.k)
            ),
        };
        let event = EventId(format!("{}#{key}", worker_of(session)));
        let current = self.sessions.get(session).map(|state| state.task(session));
        let replayed = match self.dispatcher_board().await {
            Some(board) => {
                let _ = board.catch_up().await;
                let fold = board.fold();
                fold.outcome(&event).map(|outcome| {
                    let seq = match outcome {
                        Ok(seq) => Some(*seq),
                        Err(_) => None,
                    };
                    seq.and_then(|seq| fold.entries().iter().find(|entry| entry.seq == seq))
                        .zip(current.as_ref())
                        .is_some_and(|(entry, current)| {
                            entry.task != *current
                                && fold
                                    .task(current)
                                    .is_some_and(|task| !task.phase.terminal())
                        })
                })
            }
            None => None,
        };
        match replayed {
            Some(true) => {
                key = format!(
                    "{key}@{}",
                    self.sessions.get(session).map_or(0, |state| state.k)
                )
            }
            Some(false) => {
                self.stats.finish_replays += 1;
                return;
            }
            None => {}
        }
        if let Some(current) = self.sessions.get(session).map(|state| state.task(session))
            && matches!(self.phase(&current).await, Some(Phase::Cancelled { .. }))
        {
            self.stats.late += 1;
            return;
        }
        self.running(root, id, session).await;
        if self
            .sessions
            .get(session)
            .is_some_and(|state| state.claim.is_none())
        {
            self.claim(session, false).await;
        }
        let Some(state) = self.sessions.get(session) else {
            return;
        };
        let task = state.task(session);
        let Some(claim) = state.claim.clone() else {
            let phase = self.phase(&task).await;
            self.fail(format!(
                "finish {task} ({key}): {session} holds no claim, the task is {phase:?}"
            ));
            return;
        };
        if let Err(error) = self.heed_heard(session).await {
            self.fail(format!("finish {task} ({key}): {error}"));
        }
        let last = match &outcome {
            Outcome::Done(output) => Some(output.clone()),
            Outcome::Failed(_) => None,
        };
        let at = self.tick();
        if self.worker(session).await.is_none() {
            return;
        }
        self.held = Some(Held {
            session: session.to_owned(),
            task: task.clone(),
            write: Write {
                by: Some(worker_of(session)),
                key: Some(key.clone()),
                at_ms: at,
                op: Op::Finish {
                    task,
                    claim,
                    outcome,
                },
            },
            key,
            last,
        });
    }

    async fn reply(&mut self, session: &str, key: &str, body: String, steer: bool) {
        let Some(state) = self.sessions.get(session) else {
            return;
        };
        let task = state.task(session);
        match self.dispatch(key, Op::Reply { task, body }).await {
            Some(Ok((_, false))) if steer => self.stats.steers += 1,
            Some(Ok((_, false))) => self.stats.replies += 1,
            Some(Ok((_, true))) | None => {}
            Some(Err(error)) => self.fail(format!("reply {key}: {error}")),
        }
    }

    async fn cancel(&mut self, session: &str) {
        let Some(state) = self.sessions.get(session) else {
            return;
        };
        let task = state.task(session);
        if self.phase(&task).await.is_none_or(|phase| phase.terminal()) {
            return;
        }
        let key = format!("cancel:{task}");
        match self
            .dispatch(
                &key,
                Op::Cancel {
                    task,
                    reason: "closed".to_owned(),
                },
            )
            .await
        {
            Some(Ok((_, false))) => self.stats.cancels += 1,
            Some(Ok((_, true)) | Err(BoardError::Rejected(Rejection::Terminal))) | None => {}
            Some(Err(error)) => self.fail(format!("{key}: {error}")),
        }
        if let Some(state) = self.sessions.get_mut(session) {
            state.claim = None;
            state.last_output = None;
        }
    }
}

enum Seen {
    Added {
        id: AgentId,
        session: String,
        parent: Option<AgentId>,
        task: String,
    },
    Status {
        id: AgentId,
        status: AgentStatus,
    },
    Completion {
        id: AgentId,
        status: AgentStatus,
        revision: u64,
    },
    Message {
        message_id: String,
        agents: Vec<AgentId>,
    },
}

impl Seen {
    fn of(update: &AgentUpdate) -> Option<Self> {
        match update {
            AgentUpdate::Added(descriptor) => Some(Self::Added {
                id: descriptor.id,
                session: descriptor.session_id.clone(),
                parent: descriptor.parent,
                task: descriptor.task.clone(),
            }),
            AgentUpdate::Status { id, status } => Some(Self::Status {
                id: *id,
                status: status.clone(),
            }),
            AgentUpdate::Completion {
                id,
                status,
                revision,
            } => Some(Self::Completion {
                id: *id,
                status: status.clone(),
                revision: *revision,
            }),
            AgentUpdate::Message(message) => Some(Self::Message {
                message_id: message.message_id.value().to_string(),
                agents: message
                    .thread
                    .participants
                    .iter()
                    .filter_map(|participant| match participant {
                        MessageSender::Agent { agent_id } => Some(*agent_id),
                        _ => None,
                    })
                    .collect(),
            }),
            AgentUpdate::Event { .. } => None,
        }
    }
}

impl Inner {
    async fn reconcile(&mut self, root: &str, finished: Vec<(AgentId, AgentStatus, Option<u64>)>) {
        for (id, status, revision) in finished {
            let Some(session) = self.session_of(root, id).await else {
                continue;
            };
            let Some(task) = self
                .sessions
                .get(&session)
                .map(|state| state.task(&session))
            else {
                continue;
            };
            if self
                .phase(&task)
                .await
                .is_some_and(|phase| !phase.terminal())
            {
                self.finish(root, id, &session, &status, revision).await;
            }
        }
    }

    async fn seen(&mut self, root: &str, seen: Seen) {
        match seen {
            Seen::Added {
                id,
                session,
                parent,
                task,
            } => self.added(root, id, &session, parent, &task).await,
            Seen::Status { id, status } => {
                let Some(session) = self.session_of(root, id).await else {
                    return;
                };
                match &status {
                    AgentStatus::Running => self.running(root, id, &session).await,
                    AgentStatus::Interrupted => {
                        let seq = self.tick();
                        self.reply(
                            &session,
                            &format!("steer:{session}:{seq}"),
                            "interrupt".to_owned(),
                            true,
                        )
                        .await;
                    }
                    AgentStatus::Closed => self.cancel(&session).await,
                    AgentStatus::Completed { .. } | AgentStatus::Failed { .. } => {
                        self.finish(root, id, &session, &status, None).await;
                    }
                    AgentStatus::Pending | AgentStatus::Closing => {}
                }
            }
            Seen::Completion {
                id,
                status,
                revision,
            } => {
                let Some(session) = self.session_of(root, id).await else {
                    return;
                };
                self.finish(root, id, &session, &status, Some(revision))
                    .await;
            }
            Seen::Message { message_id, agents } => {
                for agent_id in agents {
                    if let Some(session) = self.session_of(root, agent_id).await {
                        self.reply(
                            &session,
                            &format!("reply:{root}:{message_id}:{agent_id}"),
                            format!("message {message_id}"),
                            false,
                        )
                        .await;
                    }
                }
            }
        }
    }
}

impl Coordination for BoardCoordination {
    fn observe(&self, root: &str, update: &AgentUpdate) {
        let Some(seen) = Seen::of(update) else {
            return;
        };
        let root = root.to_owned();
        self.send(Box::new(move |inner| {
            Box::pin(async move {
                inner.seen(&root, seen).await;
                inner.changes.send_modify(|revision| *revision += 1);
            })
        }));
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn reconcile(&self, root: &str, finished: Vec<(AgentId, AgentStatus, Option<u64>)>) {
        let root = root.to_owned();
        self.send(Box::new(move |inner| {
            Box::pin(async move {
                inner.reconcile(&root, finished).await;
                inner.changes.send_modify(|revision| *revision += 1);
            })
        }));
    }

    fn agents<'a>(&'a self, root: &'a str) -> ReadFuture<'a, Result<BoardAgents, String>> {
        let root = root.to_owned();
        Box::pin(async move {
            self.call(move |inner| Box::pin(async move { inner.agents(&root).await }))
                .await
                .unwrap_or_else(|| Err("the board coordination task stopped".to_owned()))
        })
    }

    fn brief<'a>(&'a self, session: &'a str) -> ReadFuture<'a, Result<Option<String>, String>> {
        self.briefed(session)
    }

    fn steers<'a>(&'a self, session: &'a str) -> ReadFuture<'a, Result<Vec<String>, String>> {
        self.boundary(session)
    }

    fn attach(&self, registry: std::sync::Weak<crate::Registry>) {
        self.hosts.attach(registry);
    }

    fn detach(&self) {
        self.hosts.stop_all();
    }

    fn release_root(&self, root_session_id: &str) {
        self.hosts.release(root_session_id);
    }
}
