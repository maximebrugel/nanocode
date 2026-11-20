use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    Change, Event, Fold, NewTask, Op, Outcome, Rejection, TaskId,
    access::{Actor, Can},
    sim::{Mutation, mutated},
    tasks::TaskOp,
};

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    #[default]
    Task,
    Goal,
}

impl Intent {
    pub const fn is_task(&self) -> bool {
        matches!(self, Self::Task)
    }

    pub const fn is_goal(&self) -> bool {
        matches!(self, Self::Goal)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    NotPlanner,
    GoalNotTopLevel,
    GoalInBatch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "text", rename_all = "snake_case")]
pub enum Report {
    Done(String),
    Failed(String),
    Cancelled(String),
}

impl Report {
    fn of(change: &Change) -> Option<Self> {
        match change {
            Change::Finished(Outcome::Done(result)) => Some(Self::Done(result.clone())),
            Change::Finished(Outcome::Failed(error)) => Some(Self::Failed(error.clone())),
            Change::Cancelled { reason } => Some(Self::Cancelled(reason.clone())),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Posted {
    seq: u64,
    digest: [u64; 2],
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intents {
    #[serde(default, with = "crate::pairs")]
    posts: BTreeMap<TaskId, Posted>,
    #[serde(default)]
    goals: BTreeSet<TaskId>,
    #[serde(default, with = "crate::pairs")]
    reports: BTreeMap<TaskId, BTreeMap<TaskId, Report>>,
}

impl Intents {
    pub const fn new() -> Self {
        Self {
            posts: BTreeMap::new(),
            goals: BTreeSet::new(),
            reports: BTreeMap::new(),
        }
    }
}

fn digest(task: &NewTask) -> [u64; 2] {
    let body = serde_json::to_vec(task).unwrap_or_default();
    let mut hash: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    for byte in body {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
    }
    [(hash >> 64) as u64, hash as u64]
}

impl Fold {
    pub fn intent(&self, task: &TaskId) -> Option<Intent> {
        self.tasks.get(task).map(|task| task.spec.terms.intent)
    }

    pub fn goals(&self) -> impl Iterator<Item = &TaskId> {
        self.intents.goals.iter()
    }

    pub fn reports(&self, parent: &TaskId) -> impl Iterator<Item = (&TaskId, &Report)> {
        self.intents.reports.get(parent).into_iter().flatten()
    }

    pub fn report(&self, parent: &TaskId, child: &TaskId) -> Option<&Report> {
        self.intents.reports.get(parent)?.get(child)
    }

    pub fn unreported(&self, parent: &TaskId) -> usize {
        let reports = self.intents.reports.get(parent);
        self.children(parent)
            .filter(|child| reports.is_none_or(|reports| !reports.contains_key(*child)))
            .count()
    }

    pub fn pick_goal(
        &self,
        actor: &str,
        labels: &BTreeMap<String, String>,
        at_ms: u64,
    ) -> Option<&TaskId> {
        let me = Actor::parse(actor).ok()?;
        self.intents.goals.iter().find(|id| {
            self.tasks.get(*id).is_some_and(|task| {
                labels
                    .iter()
                    .all(|(key, value)| task.spec.labels.get(key) == Some(value))
            }) && self.claimable(id, at_ms).is_ok()
                && self.admits(id, actor).is_ok()
                && self.access.may(&me, &id.0, Can::Label)
                && self.access.may(&me, &id.0, Can::Plan)
        })
    }

    pub(crate) fn repost(&self, event: &Event) -> Option<u64> {
        let Op::Post { task } = &event.op else {
            return None;
        };
        let posted = self.intents.posts.get(&task.id)?;
        (posted.digest == digest(task)
            && !mutated(Mutation::RetryPostsTwice)
            && self.authorize(&event.by, &event.op).is_ok())
        .then_some(posted.seq)
    }

    pub(crate) fn reposted(&self, task: &NewTask) -> bool {
        mutated(Mutation::RetryPostsTwice)
            && self
                .intents
                .posts
                .get(&task.id)
                .is_some_and(|posted| posted.digest == digest(task))
    }

    pub(crate) fn intended(&self, by: &str, op: &Op) -> Result<(), Rejection> {
        let refuse = |refusal| Err(Rejection::Intent(refusal));
        let goal = |task: &TaskId| self.intent(task).is_some_and(|intent| intent.is_goal());
        match op {
            Op::Post { task } if task.terms.intent.is_goal() && task.parent.is_some() => {
                refuse(Refusal::GoalNotTopLevel)
            }
            Op::Move {
                task,
                parent: Some(_),
            } if goal(task) => refuse(Refusal::GoalNotTopLevel),
            Op::Tasks(TaskOp::Batch { tasks, .. })
                if tasks.iter().any(|task| task.terms.intent.is_goal()) =>
            {
                refuse(Refusal::GoalInBatch)
            }
            Op::Claim { task, .. } if goal(task) && !mutated(Mutation::GoalClaimedByAnyone) => {
                let planner =
                    Actor::parse(by).is_ok_and(|actor| self.access.may(&actor, &task.0, Can::Plan));
                if planner {
                    Ok(())
                } else {
                    refuse(Refusal::NotPlanner)
                }
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn intend(&mut self, seq: u64, event: &Event) {
        let Op::Post { task } = &event.op else {
            return;
        };
        self.intents.posts.insert(
            task.id.clone(),
            Posted {
                seq,
                digest: digest(task),
            },
        );
        if task.terms.intent.is_goal()
            && self
                .tasks
                .get(&task.id)
                .is_some_and(|task| !task.finished())
        {
            self.intents.goals.insert(task.id.clone());
        }
    }

    pub(crate) fn reported(&mut self, seq: u64, event: &Event, task: &TaskId, change: &Change) {
        let Some(report) = Report::of(change) else {
            return;
        };
        self.intents.goals.remove(task);
        let Some(parent) = self
            .tasks
            .get(task)
            .and_then(|task| task.spec.parent.clone())
        else {
            return;
        };
        if mutated(Mutation::ReportsDropped) {
            return;
        }
        self.intents
            .reports
            .entry(parent.clone())
            .or_default()
            .insert(task.clone(), report.clone());
        self.record(
            seq,
            event,
            &parent,
            Change::Reported {
                child: task.clone(),
                report,
            },
        );
    }
}
