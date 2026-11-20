use nanocode_machine::{Illegal, Machine, machine};

use crate::{
    Fold, Op, Phase, Rejection, Task, TaskId,
    sim::{Mutation, mutated},
    tasks::{OnFail, Refusal},
};

machine! {
    pub enum Stage {
        Scheduled = "scheduled",
        Open = "open",
        Held = "held",
        Lapsed = "lapsed",
        Blocked = "blocked",
        Stalled = "stalled",
        Done = "done",
        Failed = "failed",
        Cancelled = "cancelled",
    }
    moves {
        Scheduled => [Open, Blocked, Done, Cancelled],
        Open => [Held, Lapsed, Blocked, Done, Cancelled],
        Held => [Held, Lapsed, Open, Scheduled, Blocked, Stalled, Done, Failed, Cancelled],
        Lapsed => [Held, Lapsed, Open, Scheduled, Blocked, Stalled, Done, Failed, Cancelled],
        Blocked => [Open, Scheduled, Done, Cancelled],
        Stalled => [Open, Cancelled],
        Done => [],
        Failed => [],
        Cancelled => [],
    }
}

impl Stage {
    pub fn of(phase: &Phase, on_fail: OnFail, due_at_ms: Option<u64>, now_ms: u64) -> Self {
        match phase {
            Phase::Open if due_at_ms.is_some_and(|due| due > now_ms) => Self::Scheduled,
            Phase::Open => Self::Open,
            Phase::Claimed { until_ms, .. } if *until_ms > now_ms => Self::Held,
            Phase::Claimed { .. } => Self::Lapsed,
            Phase::Blocked { .. } => Self::Blocked,
            Phase::Failed { .. } if on_fail.is_cascade() => Self::Failed,
            Phase::Failed { .. } => Self::Stalled,
            Phase::Done { .. } => Self::Done,
            Phase::Cancelled { .. } => Self::Cancelled,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Claim,
    Renew,
    Release,
    Finish,
    Block,
    Unblock,
    Reopen,
    Cancel,
    Settle,
}

impl Act {
    pub const ALL: [Self; 9] = [
        Self::Claim,
        Self::Renew,
        Self::Release,
        Self::Finish,
        Self::Block,
        Self::Unblock,
        Self::Reopen,
        Self::Cancel,
        Self::Settle,
    ];

    pub const fn from(self) -> &'static [Stage] {
        use Stage::{Blocked, Held, Lapsed, Open, Scheduled, Stalled};
        match self {
            Self::Claim => &[Open, Lapsed],
            Self::Renew | Self::Release | Self::Finish => &[Held, Lapsed],
            Self::Block => &[Scheduled, Open, Held, Lapsed],
            Self::Unblock => &[Blocked],
            Self::Reopen => &[Stalled],
            Self::Cancel => &[Scheduled, Open, Held, Lapsed, Blocked, Stalled],
            Self::Settle => &[Scheduled, Open, Held, Lapsed, Blocked],
        }
    }

    pub const fn wire(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Renew => "renew",
            Self::Release => "release",
            Self::Finish => "finish",
            Self::Block => "block",
            Self::Unblock => "unblock",
            Self::Reopen => "reopen",
            Self::Cancel => "cancel",
            Self::Settle => "settle",
        }
    }

    pub fn starts(self, stage: Stage) -> bool {
        match (self, stage) {
            (Self::Unblock, _) if mutated(Mutation::UnblockRefused) => false,
            (Self::Reopen | Self::Cancel, Stage::Stalled) if mutated(Mutation::StalledStuck) => {
                false
            }
            _ => self.from().contains(&stage) || mutated(Mutation::LifecycleUnchecked),
        }
    }
}

pub const STAYS: [(Stage, Act); 3] = [
    (Stage::Held, Act::Renew),
    (Stage::Lapsed, Act::Renew),
    (Stage::Lapsed, Act::Claim),
];

impl Task {
    pub fn stage(&self, now_ms: u64) -> Stage {
        Stage::of(&self.phase, self.spec.terms.on_fail, self.due_at_ms, now_ms)
    }

    pub fn may(&self, act: Act, now_ms: u64) -> Result<Stage, Rejection> {
        let stage = self.stage(now_ms);
        if act.starts(stage) {
            Ok(stage)
        } else {
            Err(self.refusal(act, stage))
        }
    }

    pub fn refusal(&self, act: Act, stage: Stage) -> Rejection {
        match (act, stage, &self.phase) {
            (
                Act::Claim,
                Stage::Held,
                Phase::Claimed {
                    claim, until_ms, ..
                },
            ) => Rejection::Held {
                claim: claim.clone(),
                until_ms: *until_ms,
            },
            (Act::Claim, Stage::Scheduled, _) => {
                Rejection::Tasks(Refusal::NotDue(self.due_at_ms.unwrap_or_default()))
            }
            (Act::Claim | Act::Block, Stage::Blocked, _) => Rejection::Tasks(Refusal::Blocked),
            (
                Act::Renew | Act::Release | Act::Finish,
                Stage::Scheduled | Stage::Open | Stage::Blocked,
                _,
            ) => Rejection::NotHolder,
            (Act::Unblock, ..) => Rejection::Tasks(Refusal::NotBlocked),
            (Act::Reopen, Stage::Failed, _) => Rejection::Terminal,
            (Act::Reopen, ..) => Rejection::Tasks(Refusal::NotFailed),
            _ => Rejection::Terminal,
        }
    }

    pub fn enter(&mut self, phase: Phase, now_ms: u64) -> Result<Stage, Illegal<Stage>> {
        self.enter_due(phase, self.due_at_ms, now_ms)
    }

    pub fn enter_due(
        &mut self,
        phase: Phase,
        due_at_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<Stage, Illegal<Stage>> {
        let from = self.stage(now_ms);
        let to = Stage::of(&phase, self.spec.terms.on_fail, due_at_ms, now_ms);
        match from.next(to) {
            Err(illegal) if !loosened() => Err(illegal),
            _ => {
                self.phase = phase;
                self.due_at_ms = due_at_ms;
                Ok(to)
            }
        }
    }
}

impl Fold {
    pub(crate) fn enter(
        &mut self,
        id: &TaskId,
        phase: Phase,
        due_at_ms: Option<Option<u64>>,
        now_ms: u64,
    ) -> bool {
        let Some(task) = self.tasks.get_mut(id) else {
            return false;
        };
        let due_at_ms = due_at_ms.unwrap_or(task.due_at_ms);
        match task.enter_due(phase, due_at_ms, now_ms) {
            Ok(_) => true,
            Err(illegal) => {
                self.refused = Some(illegal);
                false
            }
        }
    }
}

pub fn rejection(illegal: Illegal<Stage>) -> Rejection {
    Rejection::Illegal {
        from: illegal.from.wire().to_owned(),
        to: illegal.to.wire().to_owned(),
    }
}

impl Fold {
    pub(crate) fn miscount(&mut self, op: &Op) {
        if let Op::Claim { task, .. } = op
            && let Some(task) = self.tasks.get_mut(task)
        {
            task.attempts += 1;
        }
    }
}

fn loosened() -> bool {
    [
        Mutation::ClaimIgnoresBlock,
        Mutation::DueIgnored,
        Mutation::UnblockAnything,
        Mutation::ReopenAnything,
        Mutation::FinishIgnoresClaim,
        Mutation::LifecycleUnchecked,
    ]
    .into_iter()
    .any(mutated)
}
