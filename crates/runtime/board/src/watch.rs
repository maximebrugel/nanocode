use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    pin::Pin,
};

use serde::{Deserialize, Serialize};

use crate::{
    Applied, Board, BoardError, Change, Entry, Event, EventId, Fold, Op, Phase, Rejection, Task,
    TaskId,
    access::{Actor, Denied},
    sim::{Mutation, mutated},
    wake::{Timer, Wake},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Key {
    Cursor {
        actor: String,
        scope: Option<TaskId>,
    },
    Watcher {
        watcher: String,
        thread: TaskId,
    },
}

impl Key {
    pub fn actor(&self) -> &str {
        match self {
            Self::Cursor { actor, .. } => actor,
            Self::Watcher { watcher, .. } => watcher,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Posted,
    Claimed,
    Renewed,
    Released,
    Finished,
    Cancelled,
    Replied,
}

impl Change {
    pub const fn kind(&self) -> Kind {
        match self {
            Self::Posted => Kind::Posted,
            Self::Claimed { .. } => Kind::Claimed,
            Self::Renewed { .. } => Kind::Renewed,
            Self::Released { .. } => Kind::Released,
            Self::Finished(_) => Kind::Finished,
            Self::Cancelled { .. } => Kind::Cancelled,
            Self::Replied { .. } => Kind::Replied,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    #[serde(default)]
    pub under: Option<TaskId>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub kinds: BTreeSet<Kind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub tries: u64,
    pub backoff_ms: u64,
    pub backoff_cap_ms: u64,
}

impl Policy {
    pub fn backoff(&self, tries: u64) -> u64 {
        let doubled = u32::try_from(tries.saturating_sub(1))
            .ok()
            .and_then(|shift| 1_u64.checked_shl(shift))
            .unwrap_or(u64::MAX);
        self.backoff_ms.saturating_mul(doubled).min(self.backoff_cap_ms)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    Consume {
        key: Key,
        lease: Option<EventId>,
        from: u64,
        to: u64,
        effects: Vec<Op>,
    },
    Watch {
        watcher: String,
        filter: Filter,
        policy: Policy,
    },
    Unwatch {
        watcher: String,
    },
    Take {
        watcher: String,
        thread: TaskId,
        at: u64,
        upto: u64,
        lease_ms: u64,
    },
    Fail {
        watcher: String,
        thread: TaskId,
        lease: EventId,
        error: String,
    },
    Bury {
        watcher: String,
        thread: TaskId,
        at: u64,
    },
    Schedule {
        id: String,
        by: String,
        send: Box<Op>,
        first_ms: u64,
        every_ms: Option<u64>,
        until_ms: Option<u64>,
        times: Option<u64>,
    },
    Fire {
        id: String,
        n: u64,
    },
    Unschedule {
        id: String,
        by: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Stale { position: u64 },
    Backward,
    Ahead,
    UnknownWatcher,
    NotHead { head: Option<u64> },
    Leased { until_ms: u64 },
    Backoff { until_ms: u64 },
    Exhausted,
    Untried,
    NotLease,
    Effect(Box<Rejection>),
    Unsendable,
    Exists,
    UnknownSchedule,
    NotDue { due_ms: Option<u64> },
    NotCreator,
    Shape,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attempt {
    Idle {
        tries: u64,
        not_before_ms: u64,
    },
    Leased {
        lease: EventId,
        until_ms: u64,
        tries: u64,
        upto: u64,
    },
}

impl Attempt {
    pub const fn tries(&self) -> u64 {
        match self {
            Self::Idle { tries, .. } | Self::Leased { tries, .. } => *tries,
        }
    }
}

pub const BATCH: usize = 16;

const FRESH: Attempt = Attempt::Idle {
    tries: 0,
    not_before_ms: 0,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    pub pending: VecDeque<u64>,
    pub done: u64,
    pub attempt: Attempt,
    pub narrow: u64,
}

impl Lane {
    pub fn single(&self, at: u64) -> bool {
        self.attempt.tries() > 0 || at <= self.narrow
    }

    pub fn head(&self) -> Option<u64> {
        if mutated(Mutation::WatchOutOfOrder) {
            return self.pending.back().copied();
        }
        self.pending.front().copied()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watcher {
    pub filter: Filter,
    pub policy: Policy,
    pub since: u64,
    pub lanes: BTreeMap<TaskId, Lane>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadLetter {
    pub watcher: String,
    pub thread: TaskId,
    pub at: u64,
    pub tries: u64,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    pub by: String,
    pub send: Op,
    pub first_ms: u64,
    pub every_ms: Option<u64>,
    pub until_ms: Option<u64>,
    pub times: Option<u64>,
    pub sent: u64,
    pub slot: u64,
    pub ended: Option<String>,
}

impl Schedule {
    pub fn slot_ms(&self, slot: u64) -> Option<u64> {
        match self.every_ms {
            None => (slot == 0).then_some(self.first_ms),
            Some(every) => self.first_ms.checked_add(every.checked_mul(slot)?),
        }
    }

    pub fn due_ms(&self) -> Option<u64> {
        if self.ended.is_some() {
            return None;
        }
        let stops = !mutated(Mutation::FireIgnoresStop);
        if stops && self.times.is_some_and(|times| self.sent >= times) {
            return None;
        }
        let due = self.slot_ms(self.slot)?;
        if stops && self.until_ms.is_some_and(|until| due > until) {
            return None;
        }
        if mutated(Mutation::FireEarly) {
            return Some(self.first_ms);
        }
        Some(due)
    }

    fn after(&self, now_ms: u64) -> u64 {
        let next = self.slot + 1;
        match self.every_ms {
            Some(every) if now_ms >= self.first_ms && !mutated(Mutation::FireReplays) => {
                next.max((now_ms - self.first_ms) / every + 1)
            }
            _ => next,
        }
    }

    pub fn send(&self, n: u64) -> Op {
        match &self.send {
            Op::Post { task } if self.every_ms.is_some() => {
                let mut task = task.clone();
                task.id = TaskId(format!("{}/{n}", task.id));
                Op::Post { task }
            }
            send => send.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    cursors: BTreeMap<Key, u64>,
    watchers: BTreeMap<String, Watcher>,
    schedules: BTreeMap<String, Schedule>,
    authors: BTreeMap<EventId, String>,
    dead: Vec<DeadLetter>,
    indexed: usize,
}

impl Marks {
    pub const fn new() -> Self {
        Self {
            cursors: BTreeMap::new(),
            watchers: BTreeMap::new(),
            schedules: BTreeMap::new(),
            authors: BTreeMap::new(),
            dead: Vec::new(),
            indexed: 0,
        }
    }

    pub const fn watchers(&self) -> &BTreeMap<String, Watcher> {
        &self.watchers
    }

    pub const fn schedules(&self) -> &BTreeMap<String, Schedule> {
        &self.schedules
    }

    pub fn dead(&self) -> &[DeadLetter] {
        &self.dead
    }

    pub fn author(&self, event: &EventId) -> Option<&str> {
        self.authors.get(event).map(String::as_str)
    }

    pub fn lane(&self, watcher: &str, thread: &TaskId) -> Option<&Lane> {
        self.watchers.get(watcher)?.lanes.get(thread)
    }
}

pub fn root(tasks: &BTreeMap<TaskId, Task>, id: &TaskId) -> TaskId {
    let mut at = id;
    while let Some(parent) = tasks.get(at).and_then(|task| task.spec.parent.as_ref()) {
        at = parent;
    }
    at.clone()
}

pub fn under(tasks: &BTreeMap<TaskId, Task>, id: &TaskId, above: &TaskId) -> bool {
    let mut at = Some(id);
    while let Some(id) = at {
        if id == above {
            return true;
        }
        at = tasks.get(id).and_then(|task| task.spec.parent.as_ref());
    }
    false
}

impl Filter {
    pub fn matches(&self, tasks: &BTreeMap<TaskId, Task>, entry: &Entry) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&entry.change.kind()) {
            return false;
        }
        if let Some(above) = &self.under
            && !under(tasks, &entry.task, above)
        {
            return false;
        }
        if self.labels.is_empty() {
            return true;
        }
        let labelled = |id: &TaskId| {
            tasks.get(id).is_some_and(|task| {
                self.labels
                    .iter()
                    .all(|(key, value)| task.spec.labels.get(key) == Some(value))
            })
        };
        labelled(&entry.task) || labelled(&root(tasks, &entry.task))
    }
}

impl Fold {
    pub const fn marks(&self) -> &Marks {
        &self.marks
    }

    pub fn position(&self, key: &Key) -> u64 {
        match key {
            Key::Cursor { .. } => self.marks.cursors.get(key).copied().unwrap_or_default(),
            Key::Watcher { watcher, thread } => self
                .marks
                .lane(watcher, thread)
                .map_or(0, |lane| lane.done),
        }
    }

    pub fn head(&self, watcher: &str, thread: &TaskId) -> Option<u64> {
        self.marks.lane(watcher, thread)?.head()
    }

    pub fn due_ms(&self) -> Option<u64> {
        let schedules = self.marks.schedules.values().filter_map(Schedule::due_ms);
        let lanes = self
            .marks
            .watchers
            .values()
            .flat_map(|watcher| watcher.lanes.values())
            .filter(|lane| !lane.pending.is_empty())
            .map(|lane| match lane.attempt {
                Attempt::Idle { not_before_ms, .. } => not_before_ms,
                Attempt::Leased { until_ms, .. } => until_ms,
            });
        let leases = self.tasks.values().filter_map(|task| match task.phase {
            Phase::Claimed { until_ms, .. } => Some(until_ms),
            _ => None,
        });
        schedules.chain(lanes).chain(leases).min()
    }

    fn lane_of(&self, watcher: &str, thread: &TaskId) -> Result<(&Watcher, &Lane), Rejection> {
        let found = self
            .marks
            .watchers
            .get(watcher)
            .ok_or(refused(Refusal::UnknownWatcher))?;
        let lane = found
            .lanes
            .get(thread)
            .ok_or(refused(Refusal::NotHead { head: None }))?;
        Ok((found, lane))
    }

    fn effects(&self, at_ms: u64, effects: &[Op]) -> Result<(), Rejection> {
        if effects.iter().any(|op| matches!(op, Op::Watch(_))) {
            return Err(refused(Refusal::Unsendable));
        }
        match effects {
            [] => Ok(()),
            [only] => self.check(at_ms, only).map_err(effect),
            _ => {
                let mut scratch = self.clone();
                for (index, op) in effects.iter().enumerate() {
                    scratch.check(at_ms, op).map_err(effect)?;
                    let inner = Event {
                        id: EventId(format!("check/{index}")),
                        by: String::new(),
                        at_ms,
                        op: op.clone(),
                    };
                    scratch.transition(scratch.next, &inner);
                }
                Ok(())
            }
        }
    }

    pub(crate) fn authorize_step(
        &self,
        actor: &Actor,
        by: &str,
        step: &Step,
    ) -> Result<(), Rejection> {
        if mutated(Mutation::WatchUnchecked) {
            return Ok(());
        }
        let access = &self.access;
        let acts = |named: &str| match Actor::parse(named) {
            Ok(named) if named.covers(actor) || actor.covers(&named) => Ok(()),
            _ => Err(Rejection::Access(Denied::NotWatcher)),
        };
        let creator = |made: &str| {
            if made == by {
                Ok(())
            } else {
                Err(Rejection::Access(Denied::NotCreator))
            }
        };
        match step {
            Step::Consume { key, effects, .. } => {
                acts(key.actor())?;
                match key {
                    Key::Cursor { scope: None, .. } => {}
                    Key::Cursor {
                        scope: Some(thread),
                        ..
                    }
                    | Key::Watcher { thread, .. } => access.see(actor, thread)?,
                }
                effects
                    .iter()
                    .try_for_each(|op| self.authorize(by, op).map_err(effect))
            }
            Step::Watch {
                watcher, filter, ..
            } => {
                acts(watcher)?;
                filter
                    .under
                    .as_ref()
                    .map_or(Ok(()), |under| access.see(actor, under))
            }
            Step::Unwatch { watcher } => acts(watcher),
            Step::Take {
                watcher, thread, ..
            }
            | Step::Fail {
                watcher, thread, ..
            }
            | Step::Bury {
                watcher, thread, ..
            } => {
                acts(watcher)?;
                access.see(actor, thread)
            }
            Step::Schedule {
                by: made, send, ..
            } => {
                creator(made)?;
                self.authorize(by, send).map_err(effect)
            }
            Step::Fire { .. } => Ok(()),
            Step::Unschedule { by: made, .. } => creator(made),
        }
    }

    pub(crate) fn check_step(&self, at_ms: u64, step: &Step) -> Result<(), Rejection> {
        let now = self.now_ms.max(at_ms);
        match step {
            Step::Consume {
                key,
                lease,
                from,
                to,
                effects,
            } => {
                let position = self.position(key);
                if *from != position && !mutated(Mutation::ConsumeIgnoresCursor) {
                    return Err(refused(Refusal::Stale { position }));
                }
                if to <= from {
                    return Err(refused(Refusal::Backward));
                }
                if *to >= self.next {
                    return Err(refused(Refusal::Ahead));
                }
                match (key, lease) {
                    (Key::Cursor { .. }, None) => {}
                    (Key::Watcher { watcher, thread }, Some(lease)) => {
                        let (_, lane) = self.lane_of(watcher, thread)?;
                        match &lane.attempt {
                            Attempt::Leased {
                                lease: held, upto, ..
                            } if held == lease || mutated(Mutation::AckIgnoresLease) => {
                                if upto != to {
                                    return Err(refused(Refusal::NotHead { head: Some(*upto) }));
                                }
                            }
                            _ => return Err(refused(Refusal::NotLease)),
                        }
                    }
                    _ => return Err(refused(Refusal::NotLease)),
                }
                self.effects(at_ms, effects)
            }
            Step::Watch { policy, .. } => {
                if policy.tries == 0 {
                    return Err(refused(Refusal::Shape));
                }
                Ok(())
            }
            Step::Unwatch { watcher } => self
                .marks
                .watchers
                .get(watcher)
                .map(drop)
                .ok_or(refused(Refusal::UnknownWatcher)),
            Step::Take {
                watcher,
                thread,
                at,
                upto,
                ..
            } => {
                let (found, lane) = self.lane_of(watcher, thread)?;
                if lane.head() != Some(*at) {
                    return Err(refused(Refusal::NotHead { head: lane.head() }));
                }
                let batched = lane.pending.iter().take(BATCH).any(|pending| pending == upto);
                if upto < at || !batched || (lane.single(*at) && upto != at) {
                    return Err(refused(Refusal::Shape));
                }
                let bounded = !mutated(Mutation::TriesUnbounded);
                match &lane.attempt {
                    Attempt::Leased { until_ms, .. }
                        if *until_ms > now && !mutated(Mutation::TakeIgnoresLease) =>
                    {
                        Err(refused(Refusal::Leased {
                            until_ms: *until_ms,
                        }))
                    }
                    Attempt::Idle { not_before_ms, .. }
                        if *not_before_ms > now && !mutated(Mutation::TakeIgnoresBackoff) =>
                    {
                        Err(refused(Refusal::Backoff {
                            until_ms: *not_before_ms,
                        }))
                    }
                    attempt if bounded && attempt.tries() >= found.policy.tries => {
                        Err(refused(Refusal::Exhausted))
                    }
                    _ => Ok(()),
                }
            }
            Step::Fail {
                watcher,
                thread,
                lease,
                ..
            } => {
                let (_, lane) = self.lane_of(watcher, thread)?;
                match &lane.attempt {
                    Attempt::Leased { lease: held, .. } if held == lease => Ok(()),
                    Attempt::Leased { .. } if mutated(Mutation::AckIgnoresLease) => Ok(()),
                    _ => Err(refused(Refusal::NotLease)),
                }
            }
            Step::Bury {
                watcher, thread, at, ..
            } => {
                let (found, lane) = self.lane_of(watcher, thread)?;
                if lane.head() != Some(*at) {
                    return Err(refused(Refusal::NotHead { head: lane.head() }));
                }
                match &lane.attempt {
                    Attempt::Leased { until_ms, .. } if *until_ms > now => {
                        Err(refused(Refusal::Leased {
                            until_ms: *until_ms,
                        }))
                    }
                    attempt if attempt.tries() >= found.policy.tries => Ok(()),
                    _ => Err(refused(Refusal::Untried)),
                }
            }
            Step::Schedule {
                id,
                send,
                every_ms,
                times,
                ..
            } => {
                if self.marks.schedules.contains_key(id) {
                    return Err(refused(Refusal::Exists));
                }
                if !matches!(**send, Op::Post { .. } | Op::Reply { .. }) {
                    return Err(refused(Refusal::Unsendable));
                }
                if *every_ms == Some(0) || *times == Some(0) {
                    return Err(refused(Refusal::Shape));
                }
                Ok(())
            }
            Step::Fire { id, n } => {
                let schedule = self
                    .marks
                    .schedules
                    .get(id)
                    .ok_or(refused(Refusal::UnknownSchedule))?;
                if *n != schedule.sent {
                    return Err(refused(Refusal::Stale {
                        position: schedule.sent,
                    }));
                }
                match schedule.due_ms() {
                    Some(due) if due <= now => Ok(()),
                    due_ms => Err(refused(Refusal::NotDue { due_ms })),
                }
            }
            Step::Unschedule { id, by } => match self.marks.schedules.get(id) {
                None => Err(refused(Refusal::UnknownSchedule)),
                Some(schedule) if schedule.ended.is_some() => {
                    Err(refused(Refusal::UnknownSchedule))
                }
                Some(schedule) if schedule.by != *by => Err(refused(Refusal::NotCreator)),
                Some(_) => Ok(()),
            },
        }
    }

    fn inner(&mut self, seq: u64, id: EventId, by: &str, at_ms: u64, op: Op) {
        let inner = Event {
            id,
            by: by.to_owned(),
            at_ms,
            op,
        };
        self.seen.insert(inner.id.clone(), Ok(seq));
        self.access.record(&inner.by, &inner.id, &inner.op);
        self.transition(seq, &inner);
    }

    fn bury(&mut self, watcher: &str, thread: &TaskId, error: String) {
        let Some(lane) = self
            .marks
            .watchers
            .get_mut(watcher)
            .and_then(|found| found.lanes.get_mut(thread))
        else {
            return;
        };
        let Some(at) = lane.head() else {
            return;
        };
        let tries = lane.attempt.tries();
        lane.done = lane.done.max(at);
        lane.pending.retain(|pending| *pending != at);
        lane.attempt = FRESH;
        self.marks.dead.push(DeadLetter {
            watcher: watcher.to_owned(),
            thread: thread.clone(),
            at,
            tries,
            error,
        });
    }

    pub(crate) fn apply_step(&mut self, seq: u64, event: &Event, step: &Step) {
        let now = self.now_ms;
        match step {
            Step::Consume {
                key, to, effects, ..
            } => {
                for (index, op) in effects.iter().enumerate() {
                    let id = EventId(format!("{}/{index}", event.id));
                    self.marks.authors.insert(id.clone(), key.actor().to_owned());
                    self.inner(seq, id, &event.by, event.at_ms, op.clone());
                }
                match key {
                    Key::Cursor { .. } => {
                        self.marks.cursors.insert(key.clone(), *to);
                    }
                    Key::Watcher { watcher, thread } => {
                        if let Some(lane) = self
                            .marks
                            .watchers
                            .get_mut(watcher)
                            .and_then(|found| found.lanes.get_mut(thread))
                        {
                            lane.done = *to;
                            lane.pending.retain(|pending| pending > to);
                            lane.attempt = FRESH;
                        }
                    }
                }
            }
            Step::Watch {
                watcher,
                filter,
                policy,
            } => {
                let same = self
                    .marks
                    .watchers
                    .get(watcher)
                    .is_some_and(|found| found.filter == *filter && found.policy == *policy);
                if !same {
                    self.marks.watchers.insert(
                        watcher.clone(),
                        Watcher {
                            filter: filter.clone(),
                            policy: *policy,
                            since: seq,
                            lanes: BTreeMap::new(),
                        },
                    );
                }
            }
            Step::Unwatch { watcher } => {
                self.marks.watchers.remove(watcher);
            }
            Step::Take {
                watcher,
                thread,
                upto,
                lease_ms,
                ..
            } => {
                if let Some(lane) = self
                    .marks
                    .watchers
                    .get_mut(watcher)
                    .and_then(|found| found.lanes.get_mut(thread))
                {
                    lane.attempt = Attempt::Leased {
                        lease: event.id.clone(),
                        until_ms: now.saturating_add(*lease_ms),
                        tries: lane.attempt.tries() + 1,
                        upto: *upto,
                    };
                }
            }
            Step::Fail {
                watcher,
                thread,
                error,
                ..
            } => {
                let Some(found) = self.marks.watchers.get_mut(watcher) else {
                    return;
                };
                let policy = found.policy;
                let Some(lane) = found.lanes.get_mut(thread) else {
                    return;
                };
                let tries = lane.attempt.tries();
                if let Attempt::Leased { upto, .. } = lane.attempt {
                    lane.narrow = lane.narrow.max(upto);
                }
                if tries >= policy.tries && !mutated(Mutation::TriesUnbounded) {
                    self.bury(watcher, thread, error.clone());
                } else {
                    lane.attempt = Attempt::Idle {
                        tries,
                        not_before_ms: now.saturating_add(policy.backoff(tries)),
                    };
                }
            }
            Step::Bury {
                watcher, thread, ..
            } => self.bury(watcher, thread, "exhausted".to_owned()),
            Step::Schedule {
                id,
                by,
                send,
                first_ms,
                every_ms,
                until_ms,
                times,
            } => {
                self.marks.schedules.insert(
                    id.clone(),
                    Schedule {
                        by: by.clone(),
                        send: (**send).clone(),
                        first_ms: *first_ms,
                        every_ms: *every_ms,
                        until_ms: *until_ms,
                        times: *times,
                        sent: 0,
                        slot: 0,
                        ended: None,
                    },
                );
            }
            Step::Fire { id, n } => {
                let Some(schedule) = self.marks.schedules.get(id) else {
                    return;
                };
                let send = schedule.send(*n);
                let by = schedule.by.clone();
                let sent = EventId(format!("fire/{id}/{n}"));
                let verdict = if self.seen.contains_key(&sent) {
                    Err(Rejection::Exists)
                } else {
                    self.authorize(&by, &send)
                        .and_then(|()| self.check(event.at_ms, &send))
                };
                if verdict.is_ok() {
                    self.inner(seq, sent, &by, event.at_ms, send);
                }
                if let Some(schedule) = self.marks.schedules.get_mut(id) {
                    schedule.sent += 1;
                    schedule.slot = schedule.after(now);
                    if let Err(rejection) = verdict {
                        schedule.ended = Some(format!("{rejection:?}"));
                    }
                }
            }
            Step::Unschedule { id, .. } => {
                if let Some(schedule) = self.marks.schedules.get_mut(id)
                    && !mutated(Mutation::UnscheduleIgnored)
                {
                    schedule.ended = Some("cancelled".to_owned());
                }
            }
        }
    }

    pub(crate) fn index_marks(&mut self) {
        let Self {
            entries,
            tasks,
            marks,
            access,
            ..
        } = self;
        for entry in &entries[marks.indexed..] {
            let author = marks.authors.get(&entry.event);
            let thread = root(tasks, &entry.task);
            for (name, watcher) in &mut marks.watchers {
                if entry.seq <= watcher.since
                    || (author == Some(name) && !mutated(Mutation::WatchSeesOwnWrites))
                    || !watcher.filter.matches(tasks, entry)
                    || !Actor::parse(name).is_ok_and(|actor| access.reads(&actor, &entry.task.0))
                {
                    continue;
                }
                let lane = watcher.lanes.entry(thread.clone()).or_insert(Lane {
                    pending: VecDeque::new(),
                    done: 0,
                    attempt: FRESH,
                    narrow: 0,
                });
                if entry.seq > lane.done && lane.pending.back() != Some(&entry.seq) {
                    lane.pending.push_back(entry.seq);
                }
            }
        }
        marks.indexed = entries.len();
    }
}

const fn refused(refusal: Refusal) -> Rejection {
    Rejection::Watch(refusal)
}

fn effect(rejection: Rejection) -> Rejection {
    refused(Refusal::Effect(Box::new(rejection)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub key: Key,
    pub from: u64,
    pub to: u64,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub watcher: String,
    pub thread: TaskId,
    pub at: u64,
    pub upto: u64,
    pub id: EventId,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Runnable {
    Take { thread: TaskId, at: u64, upto: u64 },
    Bury { thread: TaskId, at: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub id: String,
    pub send: Op,
    pub first_ms: u64,
    pub every_ms: Option<u64>,
    pub until_ms: Option<u64>,
    pub times: Option<u64>,
}

pub type Handled<'a> = Pin<Box<dyn Future<Output = Result<Vec<Op>, String>> + 'a>>;

pub trait Handler {
    fn run<'a>(&'a self, lease: &'a Lease) -> Handled<'a>;
}

pub struct Registered<'h> {
    pub name: String,
    pub filter: Filter,
    pub policy: Policy,
    pub lease_ms: u64,
    pub handler: Box<dyn Handler + 'h>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Served {
    pub acked: u64,
    pub failed: u64,
    pub buried: u64,
    pub lost: u64,
}

impl Board {
    async fn step(&mut self, at_ms: u64, step: Step) -> Result<Applied, BoardError> {
        self.submit(at_ms, Op::Watch(step)).await
    }

    pub fn pending(&self, actor: &str, scope: Option<&TaskId>, filter: &Filter) -> Batch {
        let key = Key::Cursor {
            actor: actor.to_owned(),
            scope: scope.cloned(),
        };
        let fold = &self.fold;
        let from = fold.position(&key);
        let start = fold.entries.partition_point(|entry| entry.seq <= from);
        let entries = fold.entries[start..]
            .iter()
            .filter(|entry| fold.access.reads(self.identity.actor(), &entry.task.0))
            .filter(|entry| scope.is_none_or(|above| under(&fold.tasks, &entry.task, above)))
            .filter(|entry| filter.matches(&fold.tasks, entry))
            .cloned()
            .collect();
        Batch {
            key,
            from,
            to: fold.next - 1,
            entries,
        }
    }

    pub async fn consume(
        &mut self,
        at_ms: u64,
        batch: &Batch,
        effects: Vec<Op>,
    ) -> Result<Applied, BoardError> {
        self.commit(at_ms, batch.key.clone(), None, batch.from, batch.to, effects)
            .await
    }

    async fn commit(
        &mut self,
        at_ms: u64,
        key: Key,
        lease: Option<EventId>,
        from: u64,
        to: u64,
        effects: Vec<Op>,
    ) -> Result<Applied, BoardError> {
        if mutated(Mutation::ConsumeSplits) {
            for op in &effects {
                self.submit(at_ms, op.clone()).await?;
            }
            let step = Step::Consume {
                key,
                lease,
                from,
                to,
                effects: Vec::new(),
            };
            return self.step(at_ms, step).await;
        }
        let step = Step::Consume {
            key,
            lease,
            from,
            to,
            effects,
        };
        self.step(at_ms, step).await
    }

    pub async fn watch(
        &mut self,
        at_ms: u64,
        watcher: &str,
        filter: &Filter,
        policy: Policy,
    ) -> Result<Option<Applied>, BoardError> {
        self.refresh().await?;
        let current = self.fold.marks.watchers.get(watcher);
        if current.is_some_and(|found| found.filter == *filter && found.policy == policy) {
            return Ok(None);
        }
        let step = Step::Watch {
            watcher: watcher.to_owned(),
            filter: filter.clone(),
            policy,
        };
        self.step(at_ms, step).await.map(Some)
    }

    pub async fn unwatch(&mut self, at_ms: u64, watcher: &str) -> Result<Applied, BoardError> {
        let step = Step::Unwatch {
            watcher: watcher.to_owned(),
        };
        self.step(at_ms, step).await
    }

    pub fn runnable(&self, watcher: &str, at_ms: u64) -> Vec<Runnable> {
        let Some(found) = self.fold.marks.watchers.get(watcher) else {
            return Vec::new();
        };
        let now = self.fold.now_ms.max(at_ms);
        found
            .lanes
            .iter()
            .filter_map(|(thread, lane)| {
                let at = lane.head()?;
                let thread = thread.clone();
                let tries = lane.attempt.tries();
                match &lane.attempt {
                    Attempt::Leased { until_ms, .. } if *until_ms > now => None,
                    Attempt::Idle { not_before_ms, .. } if *not_before_ms > now => None,
                    _ if tries >= found.policy.tries => Some(Runnable::Bury { thread, at }),
                    _ if lane.single(at) => Some(Runnable::Take {
                        thread,
                        at,
                        upto: at,
                    }),
                    _ => {
                        let last = lane.pending.iter().take(BATCH).copied().max().unwrap_or(at);
                        Some(Runnable::Take {
                            thread,
                            at,
                            upto: last.max(at),
                        })
                    }
                }
            })
            .collect()
    }

    pub async fn take(
        &mut self,
        at_ms: u64,
        watcher: &str,
        thread: &TaskId,
        (at, upto): (u64, u64),
        lease_ms: u64,
    ) -> Result<Lease, BoardError> {
        let step = Step::Take {
            watcher: watcher.to_owned(),
            thread: thread.clone(),
            at,
            upto,
            lease_ms,
        };
        let applied = self.step(at_ms, step).await?;
        let fold = &self.fold;
        let filter = fold
            .marks
            .watchers
            .get(watcher)
            .map(|found| found.filter.clone())
            .unwrap_or_default();
        let start = fold.entries.partition_point(|entry| entry.seq < at);
        let entries = fold.entries[start..]
            .iter()
            .take_while(|entry| entry.seq <= upto)
            .filter(|entry| root(&fold.tasks, &entry.task) == *thread)
            .filter(|entry| fold.marks.author(&entry.event) != Some(watcher))
            .filter(|entry| filter.matches(&fold.tasks, entry))
            .cloned()
            .collect();
        Ok(Lease {
            watcher: watcher.to_owned(),
            thread: thread.clone(),
            at,
            upto,
            id: applied.event,
            entries,
        })
    }

    pub async fn ack(
        &mut self,
        at_ms: u64,
        lease: &Lease,
        effects: Vec<Op>,
    ) -> Result<Applied, BoardError> {
        let key = Key::Watcher {
            watcher: lease.watcher.clone(),
            thread: lease.thread.clone(),
        };
        self.refresh().await?;
        let from = self.fold.position(&key);
        self.commit(at_ms, key, Some(lease.id.clone()), from, lease.upto, effects)
            .await
    }

    pub async fn fail(
        &mut self,
        at_ms: u64,
        lease: &Lease,
        error: &str,
    ) -> Result<Applied, BoardError> {
        let step = Step::Fail {
            watcher: lease.watcher.clone(),
            thread: lease.thread.clone(),
            lease: lease.id.clone(),
            error: error.to_owned(),
        };
        self.step(at_ms, step).await
    }

    pub async fn bury(
        &mut self,
        at_ms: u64,
        watcher: &str,
        thread: &TaskId,
        at: u64,
    ) -> Result<Applied, BoardError> {
        let step = Step::Bury {
            watcher: watcher.to_owned(),
            thread: thread.clone(),
            at,
        };
        self.step(at_ms, step).await
    }

    pub async fn schedule(&mut self, at_ms: u64, plan: Plan) -> Result<Applied, BoardError> {
        let step = Step::Schedule {
            id: plan.id,
            by: self.writer().to_owned(),
            send: Box::new(plan.send),
            first_ms: plan.first_ms,
            every_ms: plan.every_ms,
            until_ms: plan.until_ms,
            times: plan.times,
        };
        self.step(at_ms, step).await
    }

    pub async fn unschedule(&mut self, at_ms: u64, id: &str) -> Result<Applied, BoardError> {
        let step = Step::Unschedule {
            id: id.to_owned(),
            by: self.writer().to_owned(),
        };
        self.step(at_ms, step).await
    }

    pub async fn fire_due(&mut self, at_ms: u64) -> Result<u64, BoardError> {
        let mut fired = 0;
        loop {
            self.refresh().await?;
            let now = self.fold.now_ms.max(at_ms);
            let due = self
                .fold
                .marks
                .schedules
                .iter()
                .find(|(_, schedule)| schedule.due_ms().is_some_and(|due| due <= now))
                .map(|(id, schedule)| (id.clone(), schedule.sent));
            let Some((id, n)) = due else {
                return Ok(fired);
            };
            let key = format!("fire:{id}:{n}");
            match self
                .submit_keyed(&key, at_ms, Op::Watch(Step::Fire { id, n }))
                .await
            {
                Ok(_) => fired += 1,
                Err(BoardError::Rejected(_)) => return Ok(fired),
                Err(error) => return Err(error),
            }
        }
    }

    pub async fn wait(
        &mut self,
        wake: &dyn Wake,
        timer: &dyn Timer,
        scope: Option<&TaskId>,
        filter: &Filter,
        after: u64,
        until_ms: u64,
    ) -> Result<Vec<Entry>, BoardError> {
        loop {
            self.fire_due(timer.now_ms()).await?;
            let fold = &self.fold;
            let start = fold.entries.partition_point(|entry| entry.seq <= after);
            let found = fold.entries[start..]
                .iter()
                .filter(|entry| fold.access.reads(self.identity.actor(), &entry.task.0))
                .filter(|entry| scope.is_none_or(|above| under(&fold.tasks, &entry.task, above)))
                .filter(|entry| filter.matches(&fold.tasks, entry))
                .cloned()
                .collect::<Vec<_>>();
            let now = timer.now_ms();
            if !found.is_empty() || now >= until_ms {
                return Ok(found);
            }
            let head = fold.next - 1;
            let wake_at = fold
                .due_ms()
                .filter(|due| *due > now)
                .map_or(until_ms, |due| due.min(until_ms));
            wake.wait(&self.scope, head, wake_at).await;
        }
    }

    pub async fn serve(
        &mut self,
        timer: &dyn Timer,
        watcher: &Registered<'_>,
    ) -> Result<Served, BoardError> {
        let mut served = Served::default();
        self.watch(timer.now_ms(), &watcher.name, &watcher.filter, watcher.policy)
            .await?;
        self.fire_due(timer.now_ms()).await?;
        loop {
            let runnable = self.runnable(&watcher.name, timer.now_ms());
            if runnable.is_empty() {
                return Ok(served);
            }
            let before = served;
            self.serve_once(timer, watcher, runnable, &mut served).await?;
            if served.acked + served.failed + served.buried == before.acked + before.failed + before.buried {
                return Ok(served);
            }
        }
    }

    async fn serve_once(
        &mut self,
        timer: &dyn Timer,
        watcher: &Registered<'_>,
        runnable: Vec<Runnable>,
        served: &mut Served,
    ) -> Result<(), BoardError> {
        for runnable in runnable {
            let (thread, at) = match runnable {
                Runnable::Bury { thread, at } => {
                    match self.bury(timer.now_ms(), &watcher.name, &thread, at).await {
                        Ok(_) => served.buried += 1,
                        Err(BoardError::Rejected(_)) => served.lost += 1,
                        Err(error) => return Err(error),
                    }
                    continue;
                }
                Runnable::Take { thread, at, upto } => (thread, (at, upto)),
            };
            let lease = match self
                .take(timer.now_ms(), &watcher.name, &thread, at, watcher.lease_ms)
                .await
            {
                Ok(lease) => lease,
                Err(BoardError::Rejected(_)) => {
                    served.lost += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let settled = match watcher.handler.run(&lease).await {
                Ok(effects) => self.ack(timer.now_ms(), &lease, effects).await.map(|_| true),
                Err(error) => self.fail(timer.now_ms(), &lease, &error).await.map(|_| false),
            };
            match settled {
                Ok(true) => served.acked += 1,
                Ok(false) => served.failed += 1,
                Err(BoardError::Rejected(_)) => served.lost += 1,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
