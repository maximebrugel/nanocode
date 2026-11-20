use std::{
    cmp::Ordering,
    collections::BTreeMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use serde::{Deserialize, Serialize};

use crate::{
    Change, ClaimId, Event, Fold, NewTask, Op, Outcome, Phase, Rejection, Task, TaskId,
    access::{Actor, Can},
    schedule::{Invalid, Recur, Retry},
    sim::{Mutation, mutated},
    stage::Act,
};

pub const BATCH: usize = 100;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFail {
    #[default]
    Cascade,
    Wait,
}

impl OnFail {
    pub fn is_cascade(&self) -> bool {
        *self == Self::Cascade
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terms {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "OnFail::is_cascade")]
    pub on_fail: OnFail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recur: Option<Recur>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<Retry>,
    #[serde(default, skip_serializing_if = "crate::intent::Intent::is_task")]
    pub intent: crate::intent::Intent,
}

impl Terms {
    pub fn due(due_at_ms: u64) -> Self {
        Self {
            due_at_ms: Some(due_at_ms),
            ..Self::default()
        }
    }

    pub fn every(self, every_ms: u64) -> Self {
        Self {
            recur: Some(Recur::Every { every_ms }),
            ..self
        }
    }

    pub fn cron(self, cron: &str, offset_minutes: i16) -> Self {
        Self {
            recur: Some(Recur::Cron {
                cron: cron.to_owned(),
                offset_minutes,
            }),
            ..self
        }
    }

    pub fn retry(self, max_attempts: u64, backoff_ms: u64) -> Self {
        Self {
            retry: Some(Retry {
                max_attempts,
                backoff_ms,
            }),
            ..self
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn check(&self) -> Result<(), Rejection> {
        let checked = self
            .recur
            .as_ref()
            .map_or(Ok(()), |recur| recur.check(self.due_at_ms))
            .and(self.retry.as_ref().map_or(Ok(()), Retry::check));
        checked.map_err(|invalid| Rejection::Tasks(Refusal::Schedule(invalid)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub series: TaskId,
    pub slot: u64,
}

pub fn occurrence(series: &TaskId, slot: u64) -> TaskId {
    TaskId(format!("{series}@{slot}"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    Task(TaskId),
    Key(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Planned {
    pub key: String,
    pub parent: Option<Need>,
    pub spec: String,
    pub labels: BTreeMap<String, String>,
    pub needs: Vec<Need>,
    #[serde(default, skip_serializing_if = "Terms::is_default")]
    pub terms: Terms,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "act", rename_all = "snake_case")]
pub enum TaskOp {
    Block {
        task: TaskId,
        claim: Option<ClaimId>,
        kind: String,
        reason: String,
    },
    Unblock {
        task: TaskId,
    },
    Reopen {
        task: TaskId,
    },
    Assign {
        task: TaskId,
        to: Option<String>,
    },
    Batch {
        name: TaskId,
        tasks: Vec<Planned>,
    },
}

impl TaskOp {
    pub const fn task(&self) -> &TaskId {
        match self {
            Self::Block { task, .. }
            | Self::Unblock { task }
            | Self::Reopen { task }
            | Self::Assign { task, .. } => task,
            Self::Batch { name, .. } => name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    Assigned(String),
    Blocked,
    NotBlocked,
    NotFailed,
    EmptyBatch,
    TooLarge(usize),
    DuplicateKey(String),
    UnknownKey(String),
    Cycle(String),
    Duplicate,
    NotDue(u64),
    Schedule(Invalid),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Moved {
    Blocked {
        kind: String,
        reason: String,
        ended: Option<ClaimId>,
    },
    Unblocked,
    Reopened,
    Assigned {
        to: Option<String>,
    },
    Retrying {
        error: String,
        due_at_ms: u64,
    },
}

pub fn acts_for(by: &str, assignee: &str) -> bool {
    Actor::parse(by).is_ok_and(|by| Actor::parse(assignee).is_ok_and(|to| to.covers(&by)))
}

pub fn member(name: &TaskId, key: &str) -> TaskId {
    TaskId(format!("{name}/{key}"))
}

pub fn resolve(name: &TaskId, tasks: &[Planned]) -> Vec<NewTask> {
    let id = |need: &Need| match need {
        Need::Task(task) => task.clone(),
        Need::Key(key) => member(name, key),
    };
    tasks
        .iter()
        .map(|planned| NewTask {
            id: member(name, &planned.key),
            parent: planned.parent.as_ref().map(id),
            spec: planned.spec.clone(),
            labels: planned.labels.clone(),
            needs: planned.needs.iter().map(id).collect(),
            terms: planned.terms.clone(),
        })
        .collect()
}

fn cyclic(at: usize, edges: &[Vec<usize>], marks: &mut [u8]) -> bool {
    match marks[at] {
        1 => return true,
        2 => return false,
        _ => {}
    }
    marks[at] = 1;
    if edges[at].iter().any(|next| cyclic(*next, edges, marks)) {
        return true;
    }
    marks[at] = 2;
    false
}

impl Task {
    fn new(spec: &NewTask, series: TaskId) -> Self {
        Self {
            spec: spec.clone(),
            phase: Phase::Open,
            attempts: 0,
            replies: Vec::new(),
            assignee: spec.terms.assignee.clone(),
            rank: None,
            due_at_ms: spec.terms.due_at_ms,
            occurrence: spec
                .terms
                .recur
                .as_ref()
                .and(spec.terms.due_at_ms)
                .map(|slot| Occurrence { series, slot }),
            cap: None,
        }
    }

    pub fn finished(&self) -> bool {
        match &self.phase {
            Phase::Failed { .. } => self.spec.terms.on_fail == OnFail::Cascade,
            phase => phase.terminal(),
        }
    }

    fn cascades(&self) -> bool {
        self.spec.terms.on_fail == OnFail::Cascade || mutated(Mutation::FailIgnoresPolicy)
    }
}

impl Fold {
    pub(crate) fn dooms(&self, above: &TaskId) -> bool {
        self.tasks.get(above).is_some_and(|task| match task.phase {
            Phase::Cancelled { .. } => true,
            Phase::Failed { .. } => task.cascades(),
            _ => false,
        })
    }

    pub(crate) fn cascades(&self, id: &TaskId) -> bool {
        self.tasks.get(id).is_some_and(Task::cascades)
    }

    pub fn admits(&self, task: &TaskId, actor: &str) -> Result<(), Rejection> {
        match self.tasks.get(task).and_then(|task| task.assignee.as_ref()) {
            Some(to) if !acts_for(actor, to) && !mutated(Mutation::ClaimIgnoresAssignee) => {
                Err(Rejection::Tasks(Refusal::Assigned(to.clone())))
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn assigned(&self, by: &str, op: &Op) -> Result<(), Rejection> {
        match op {
            Op::Claim { task, .. } => self.admits(task, by),
            _ => Ok(()),
        }
    }

    pub fn until(&self, task: &TaskId, claim: &ClaimId) -> Option<u64> {
        match self.tasks.get(task).map(|task| &task.phase) {
            Some(Phase::Claimed {
                claim: held,
                until_ms,
                ..
            }) if held == claim => Some(*until_ms),
            _ => None,
        }
    }

    pub fn pick(
        &self,
        actor: &str,
        labels: &BTreeMap<String, String>,
        at_ms: u64,
    ) -> Option<&TaskId> {
        self.spread(actor, labels, at_ms, 1)
    }

    pub fn spread(
        &self,
        actor: &str,
        labels: &BTreeMap<String, String>,
        at_ms: u64,
        width: usize,
    ) -> Option<&TaskId> {
        let me = Actor::parse(actor).ok()?;
        let sets = labels
            .iter()
            .map(|(key, value)| self.labelled.get(key).and_then(|values| values.get(value)))
            .collect::<Option<Vec<_>>>()?;
        let candidates = sets
            .into_iter()
            .min_by_key(|ids| ids.len())
            .unwrap_or(&self.live);
        let mut tier = None;
        let mut head: Vec<(usize, &TaskId)> = Vec::new();
        for id in candidates {
            let (Some(task), Some(posted)) = (self.tasks.get(id), self.posted.get(id)) else {
                continue;
            };
            if !labels
                .iter()
                .all(|(key, value)| task.spec.labels.get(key) == Some(value))
                || task.spec.terms.intent.is_goal()
                || self.claimable(id, at_ms).is_err()
                || self.admits(id, actor).is_err()
                || !self.access.may(&me, &id.0, Can::Label)
            {
                continue;
            }
            let rank = task.rank.filter(|_| !mutated(Mutation::NextIgnoresRank));
            let key = (
                rank.is_none(),
                rank,
                task.assignee.is_none() && !mutated(Mutation::NextIgnoresAssignee),
            );
            let key = if width > 1 && mutated(Mutation::SpreadIgnoresTier) {
                (false, None, false)
            } else {
                key
            };
            match tier.as_ref().map(|held| key.cmp(held)) {
                Some(Ordering::Greater) => continue,
                Some(Ordering::Equal) => {}
                _ => {
                    tier = Some(key);
                    head.clear();
                }
            }
            let at = head.partition_point(|(held, _)| held < posted);
            if at < width {
                head.insert(at, (*posted, id));
                head.truncate(width);
            }
        }
        let salt = |id: &TaskId| {
            let mut hasher = DefaultHasher::new();
            (actor, id).hash(&mut hasher);
            hasher.finish()
        };
        head.into_iter()
            .map(|(_, id)| id)
            .min_by_key(|id| if width > 1 { salt(id) } else { 0 })
    }

    pub(crate) fn check_tasks(&self, at_ms: u64, op: &TaskOp) -> Result<(), Rejection> {
        if let TaskOp::Batch { name, tasks } = op {
            return self.check_batch(name, tasks);
        }
        let task = self.tasks.get(op.task()).ok_or(Rejection::UnknownTask)?;
        match op {
            TaskOp::Block { claim, .. } => {
                let now = self.now_ms.max(at_ms);
                task.may(Act::Block, now)?;
                match (&task.phase, claim) {
                    (Phase::Claimed { claim: held, .. }, Some(claim)) if held == claim => Ok(()),
                    (_, Some(_)) => Err(Rejection::NotHolder),
                    (
                        Phase::Claimed {
                            claim, until_ms, ..
                        },
                        None,
                    ) if *until_ms > now => Err(Rejection::Held {
                        claim: claim.clone(),
                        until_ms: *until_ms,
                    }),
                    (_, None) => Ok(()),
                }
            }
            TaskOp::Unblock { .. } if mutated(Mutation::UnblockAnything) => Ok(()),
            TaskOp::Reopen { .. } if mutated(Mutation::ReopenAnything) => Ok(()),
            TaskOp::Unblock { .. } => task.may(Act::Unblock, self.now_ms.max(at_ms)).map(drop),
            TaskOp::Reopen { .. } => task.may(Act::Reopen, self.now_ms.max(at_ms)).map(drop),
            TaskOp::Assign { .. } if task.finished() => Err(Rejection::Terminal),
            TaskOp::Assign { .. } | TaskOp::Batch { .. } => Ok(()),
        }
    }

    fn check_batch(&self, name: &TaskId, tasks: &[Planned]) -> Result<(), Rejection> {
        if tasks.is_empty() {
            return Err(Rejection::Tasks(Refusal::EmptyBatch));
        }
        if tasks.len() > BATCH {
            return Err(Rejection::Tasks(Refusal::TooLarge(tasks.len())));
        }
        let mut keys = BTreeMap::new();
        for (index, planned) in tasks.iter().enumerate() {
            if keys.insert(planned.key.as_str(), index).is_some() {
                return Err(Rejection::Tasks(Refusal::DuplicateKey(planned.key.clone())));
            }
            if self.tasks.contains_key(&member(name, &planned.key)) {
                return Err(Rejection::Exists);
            }
            planned.terms.check()?;
        }
        let mut edges = vec![Vec::new(); tasks.len()];
        for (index, planned) in tasks.iter().enumerate() {
            match &planned.parent {
                Some(Need::Task(parent)) if !self.tasks.contains_key(parent) => {
                    return Err(Rejection::UnknownParent(parent.clone()));
                }
                Some(Need::Key(key)) if keys.get(key.as_str()).is_none_or(|at| *at >= index) => {
                    return Err(Rejection::Tasks(Refusal::UnknownKey(key.clone())));
                }
                _ => {}
            }
            for need in &planned.needs {
                match need {
                    Need::Task(task) if !self.tasks.contains_key(task) => {
                        return Err(Rejection::UnknownNeed(task.clone()));
                    }
                    Need::Task(_) => {}
                    Need::Key(key) => match keys.get(key.as_str()) {
                        Some(at) => edges[index].push(*at),
                        None => return Err(Rejection::Tasks(Refusal::UnknownKey(key.clone()))),
                    },
                }
            }
        }
        if mutated(Mutation::BatchIgnoresCycles) {
            return Ok(());
        }
        let mut marks = vec![0; tasks.len()];
        match (0..tasks.len()).find(|at| cyclic(*at, &edges, &mut marks)) {
            Some(at) => Err(Rejection::Tasks(Refusal::Cycle(tasks[at].key.clone()))),
            None => Ok(()),
        }
    }

    pub(crate) fn place(&mut self, seq: u64, event: &Event, task: &NewTask, series: TaskId) {
        if let Some(parent) = &task.parent {
            self.children
                .entry(parent.clone())
                .or_default()
                .insert(task.id.clone());
        }
        for need in &task.needs {
            self.dependents
                .entry(need.clone())
                .or_default()
                .insert(task.id.clone());
        }
        self.tasks.insert(task.id.clone(), Task::new(task, series));
        self.record(seq, event, &task.id, Change::Posted);
    }

    pub(crate) fn scheduled(
        &self,
        id: &TaskId,
        now_ms: u64,
        outcome: &Outcome,
    ) -> Result<Option<u64>, Rejection> {
        let refuse = |invalid| Rejection::Tasks(Refusal::Schedule(invalid));
        let task = self.tasks.get(id).ok_or(Rejection::UnknownTask)?;
        if let (Outcome::Failed(_), Some(retry)) = (outcome, &task.spec.terms.retry)
            && task.attempts < retry.max_attempts
        {
            let delay = retry
                .delay(task.attempts.max(1))
                .ok_or(refuse(Invalid::Overflow))?;
            let due_at_ms = now_ms.checked_add(delay).ok_or(refuse(Invalid::Overflow))?;
            return Ok(Some(due_at_ms));
        }
        if let Some(recur) = &task.spec.terms.recur
            && let Some(occurrence) = &task.occurrence
        {
            recur.next_after(occurrence.slot).map_err(refuse)?;
        }
        Ok(None)
    }

    pub(crate) fn recur(&mut self, seq: u64, event: &Event, id: &TaskId) {
        let Some(task) = self.tasks.get(id) else {
            return;
        };
        let (Some(recur), Some(Occurrence { series, slot })) =
            (&task.spec.terms.recur, &task.occurrence)
        else {
            return;
        };
        if !task.finished() {
            return;
        }
        let after = if mutated(Mutation::RecurrenceSkipsOnRestart) {
            (*slot).max(self.now_ms)
        } else {
            *slot
        };
        let Ok(next) = recur.next_after(after) else {
            return;
        };
        let series = series.clone();
        let (next, next_id) = if mutated(Mutation::DueFiresTwice) {
            (*slot, TaskId(format!("{series}@{slot}.{seq}")))
        } else {
            (next, occurrence(&series, next))
        };
        if self.tasks.contains_key(&next_id) {
            return;
        }
        let mut spec = task.spec.clone();
        spec.id = next_id.clone();
        spec.terms.due_at_ms = Some(next);
        self.place(seq, event, &spec, series);
        let doomed = spec
            .parent
            .iter()
            .chain(&spec.needs)
            .find(|above| self.dooms(above))
            .cloned();
        if let Some(above) = doomed {
            self.cancel(seq, event, &next_id, format!("{above} did not finish"));
        }
    }

    pub(crate) fn steer(&mut self, seq: u64, event: &Event, op: &TaskOp) {
        let id = op.task().clone();
        let now = self.now_ms;
        let change = match op {
            TaskOp::Batch { name, tasks } => {
                assert!(tasks.len() <= BATCH, "a checked batch fits one write");
                let placed = resolve(name, tasks);
                for task in &placed {
                    self.place(seq, event, task, task.id.clone());
                }
                for task in &placed {
                    let doomed = task
                        .parent
                        .iter()
                        .chain(&task.needs)
                        .find(|above| self.dooms(above))
                        .cloned();
                    if let Some(above) = doomed
                        && !mutated(Mutation::PostIgnoresDoomedNeeds)
                    {
                        self.cancel(seq, event, &task.id, format!("{above} did not finish"));
                    }
                }
                return;
            }
            TaskOp::Block { kind, reason, .. } => {
                let Some(task) = self.tasks.get_mut(&id) else {
                    return;
                };
                let ended = match &task.phase {
                    Phase::Claimed { claim, .. } => Some(claim.clone()),
                    _ => None,
                };
                let blocked = Phase::Blocked {
                    kind: kind.clone(),
                    reason: reason.clone(),
                };
                if let Err(illegal) = task.enter(blocked, now) {
                    self.refused = Some(illegal);
                    return;
                }
                Moved::Blocked {
                    kind: kind.clone(),
                    reason: reason.clone(),
                    ended,
                }
            }
            TaskOp::Unblock { .. } | TaskOp::Reopen { .. } => {
                if !self.enter(&id, Phase::Open, None, now) {
                    return;
                }
                if matches!(op, TaskOp::Reopen { .. }) {
                    Moved::Reopened
                } else {
                    Moved::Unblocked
                }
            }
            TaskOp::Assign { to, .. } => {
                if let Some(task) = self.tasks.get_mut(&id) {
                    task.assignee.clone_from(to);
                }
                Moved::Assigned { to: to.clone() }
            }
        };
        self.record(seq, event, &id, Change::Tasks(change));
    }
}
