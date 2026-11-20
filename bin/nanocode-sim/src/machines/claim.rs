use std::collections::BTreeMap;

use nanocode_board::{
    ClaimId, Event, EventId, Fold, NewTask, Op, Outcome, Phase, Task, TaskId,
    access::{Can, Grant, Who},
    mirror::{Join, Mode, Settle, SyncOp, Target},
    sim::{Mutation, mutate},
    stage::{Act, STAYS, Stage},
    tasks::{OnFail, TaskOp, Terms},
};
use nanocode_machine::Machine;

use super::{Canary, Entry, Model, Part, Report, walk};

const OWNER: &str = "user:owner";
const WORKERS: [&str; 2] = ["agent:w1", "agent:w2"];
const TASKS: [&str; 2] = ["a", "b"];
const TICK_MS: u64 = 1_000;
const LEASE_MS: u64 = 2 * TICK_MS;
const RETRIES: u64 = 2;

fn task(index: usize) -> TaskId {
    TaskId(TASKS[index].to_owned())
}

const fn resting(stage: Stage) -> bool {
    matches!(stage, Stage::Done | Stage::Failed | Stage::Cancelled)
}

#[derive(Clone)]
pub struct World {
    fold: Fold,
    now: u64,
    minted: u64,
    held: BTreeMap<(usize, usize), ClaimId>,
    echo: Option<String>,
}

impl World {
    fn submit(&mut self, by: &str, op: Op, redeliver: bool) -> Result<(EventId, bool), String> {
        self.minted += 1;
        let id = EventId(format!("{by}.{}", self.minted));
        let event = Event {
            id: id.clone(),
            by: by.to_owned(),
            at_ms: self.now,
            op,
        };
        let body = serde_json::to_string(&event).map_err(|error| error.to_string())?;
        self.fold
            .apply(self.fold.next(), &body)
            .map_err(|error| format!("claim fold: {error}"))?;
        let accepted = matches!(self.fold.outcome(&id), Some(Ok(_)));
        if redeliver {
            let first = (self.facts(), self.fold.outcome(&id).cloned());
            self.fold
                .apply(self.fold.next(), &body)
                .map_err(|error| format!("claim fold: {error}"))?;
            let again = (self.facts(), self.fold.outcome(&id).cloned());
            if again != first {
                self.echo = Some(format!(
                    "redelivering {} moved {first:?} to {again:?}",
                    id.0
                ));
            }
        }
        Ok((id, accepted))
    }

    fn task(&self, index: usize) -> Option<&Task> {
        self.fold.task(&task(index))
    }

    fn stage(&self, index: usize) -> Option<Stage> {
        self.task(index).map(|task| task.stage(self.now))
    }

    fn attempts(&self, index: usize) -> u64 {
        self.task(index).map_or(0, |task| task.attempts)
    }

    fn current(&self, index: usize, worker: usize) -> bool {
        matches!(
            (self.held.get(&(index, worker)), self.task(index).map(|task| &task.phase)),
            (Some(known), Some(Phase::Claimed { claim, .. })) if known == claim
        )
    }

    fn claim(&self, index: usize, worker: usize) -> ClaimId {
        self.held
            .get(&(index, worker))
            .cloned()
            .unwrap_or_else(|| ClaimId(format!("{}.none", WORKERS[worker])))
    }

    fn timed(&self) -> bool {
        (0..TASKS.len()).any(|index| {
            self.task(index).is_some_and(|task| {
                task.due_at_ms.is_some_and(|due| due > self.now)
                    || matches!(task.phase, Phase::Claimed { until_ms, .. } if until_ms > self.now)
            })
        })
    }

    fn view(&self, index: usize) -> String {
        let Some(task) = self.task(index) else {
            return "absent".to_owned();
        };
        let stage = task.stage(self.now);
        if resting(stage) {
            return format!("{} attempts={}", stage.wire(), task.attempts);
        }
        let holder = match &task.phase {
            Phase::Claimed {
                worker, until_ms, ..
            } => format!(" by={worker} left={}", until_ms.saturating_sub(self.now)),
            _ => String::new(),
        };
        let due = task
            .due_at_ms
            .filter(|due| *due > self.now)
            .map(|due| due - self.now);
        let known: Vec<bool> = (0..WORKERS.len())
            .map(|worker| self.current(index, worker))
            .collect();
        format!(
            "{} attempts={}{holder} due={due:?} known={known:?}",
            stage.wire(),
            task.attempts
        )
    }

    fn facts(&self) -> String {
        (0..TASKS.len())
            .map(|index| format!("{}: {}", TASKS[index], self.view(index)))
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Claim {
        task: usize,
        worker: usize,
        lapsed: bool,
    },
    Renew {
        task: usize,
        worker: usize,
    },
    Release {
        task: usize,
        worker: usize,
    },
    Finish {
        task: usize,
        worker: usize,
        done: bool,
    },
    Block {
        task: usize,
        worker: Option<usize>,
    },
    Unblock {
        task: usize,
    },
    Reopen {
        task: usize,
    },
    Cancel {
        task: usize,
    },
    Settle {
        task: usize,
        done: bool,
    },
    Tick,
}

impl Step {
    const fn act(self) -> Option<(usize, Act)> {
        match self {
            Self::Claim { task, .. } => Some((task, Act::Claim)),
            Self::Renew { task, .. } => Some((task, Act::Renew)),
            Self::Release { task, .. } => Some((task, Act::Release)),
            Self::Finish { task, .. } => Some((task, Act::Finish)),
            Self::Block { task, .. } => Some((task, Act::Block)),
            Self::Unblock { task } => Some((task, Act::Unblock)),
            Self::Reopen { task } => Some((task, Act::Reopen)),
            Self::Cancel { task } => Some((task, Act::Cancel)),
            Self::Settle { task, .. } => Some((task, Act::Settle)),
            Self::Tick => None,
        }
    }

    const fn holder(self) -> Option<(usize, usize)> {
        match self {
            Self::Renew { task, worker }
            | Self::Release { task, worker }
            | Self::Finish { task, worker, .. }
            | Self::Block {
                task,
                worker: Some(worker),
            } => Some((task, worker)),
            _ => None,
        }
    }
}

pub struct Folded {
    depth: usize,
}

impl Model for Folded {
    type State = World;
    type Event = Step;

    fn name(&self) -> &'static str {
        "claim_fold"
    }

    fn depth(&self) -> usize {
        self.depth
    }

    fn initial(&self) -> Result<Vec<World>, String> {
        let mut world = World {
            fold: Fold::new(),
            now: 0,
            minted: 0,
            held: BTreeMap::new(),
            echo: None,
        };
        let grant = Grant {
            who: vec![Who::Kind("agent".to_owned())],
            can: BTreeMap::from([("*".to_owned(), vec![Can::Read, Can::Label])]),
        };
        let first = NewTask {
            id: task(0),
            parent: None,
            spec: "retried, waits when it fails".to_owned(),
            labels: BTreeMap::new(),
            needs: Vec::new(),
            terms: Terms {
                on_fail: OnFail::Wait,
                ..Terms::due(TICK_MS).retry(RETRIES, TICK_MS)
            },
        };
        let second = NewTask {
            id: task(1),
            parent: None,
            spec: "needs the first".to_owned(),
            labels: BTreeMap::new(),
            needs: vec![task(0)],
            terms: Terms::default(),
        };
        let mut setup = vec![
            Op::Charter {},
            Op::Grant { grant },
            Op::Post { task: first },
            Op::Post { task: second },
        ];
        for (index, name) in TASKS.iter().enumerate() {
            setup.push(Op::Sync {
                sync: SyncOp::Link {
                    task: task(index),
                    target: Target::new("chat", *name),
                    join: Join::Root {
                        message: format!("root-{name}"),
                    },
                    mode: Mode::Always,
                },
            });
        }
        for op in setup {
            let shown = format!("{op:?}");
            let (id, accepted) = world.submit(OWNER, op, false)?;
            if !accepted {
                return Err(format!(
                    "setup refused {shown}: {:?}",
                    world.fold.outcome(&id)
                ));
            }
        }
        Ok(vec![world])
    }

    fn events(&self, world: &World) -> Vec<Step> {
        let mut steps = Vec::new();
        for task in 0..TASKS.len() {
            for worker in 0..WORKERS.len() {
                steps.extend([
                    Step::Claim {
                        task,
                        worker,
                        lapsed: false,
                    },
                    Step::Claim {
                        task,
                        worker,
                        lapsed: true,
                    },
                    Step::Renew { task, worker },
                    Step::Release { task, worker },
                    Step::Finish {
                        task,
                        worker,
                        done: true,
                    },
                    Step::Finish {
                        task,
                        worker,
                        done: false,
                    },
                    Step::Block {
                        task,
                        worker: Some(worker),
                    },
                ]);
            }
            steps.extend([
                Step::Block { task, worker: None },
                Step::Unblock { task },
                Step::Reopen { task },
                Step::Cancel { task },
                Step::Settle { task, done: true },
                Step::Settle { task, done: false },
            ]);
        }
        if world.timed() {
            steps.push(Step::Tick);
        }
        steps
    }

    fn label(&self, step: &Step) -> &'static str {
        step.act().map_or("tick", |(_, act)| act.wire())
    }

    fn apply(&self, world: &mut World, step: &Step) -> Result<bool, String> {
        world.echo = None;
        let (by, op) = match *step {
            Step::Tick => {
                world.now += TICK_MS;
                return Ok(true);
            }
            Step::Claim {
                task: index,
                worker,
                lapsed,
            } => (
                WORKERS[worker],
                Op::Claim {
                    task: task(index),
                    lease_ms: if lapsed { 0 } else { LEASE_MS },
                    fence: None,
                    cap: None,
                },
            ),
            Step::Renew {
                task: index,
                worker,
            } => (
                WORKERS[worker],
                Op::Renew {
                    task: task(index),
                    claim: world.claim(index, worker),
                    lease_ms: LEASE_MS,
                },
            ),
            Step::Release {
                task: index,
                worker,
            } => (
                WORKERS[worker],
                Op::Release {
                    task: task(index),
                    claim: world.claim(index, worker),
                    note: "released".to_owned(),
                },
            ),
            Step::Finish {
                task: index,
                worker,
                done,
            } => (
                WORKERS[worker],
                Op::Finish {
                    task: task(index),
                    claim: world.claim(index, worker),
                    outcome: if done {
                        Outcome::Done("done".to_owned())
                    } else {
                        Outcome::Failed("failed".to_owned())
                    },
                },
            ),
            Step::Block {
                task: index,
                worker,
            } => (
                worker.map_or(OWNER, |worker| WORKERS[worker]),
                Op::Tasks(TaskOp::Block {
                    task: task(index),
                    claim: worker.map(|worker| world.claim(index, worker)),
                    kind: "input".to_owned(),
                    reason: "blocked".to_owned(),
                }),
            ),
            Step::Unblock { task: index } => {
                (OWNER, Op::Tasks(TaskOp::Unblock { task: task(index) }))
            }
            Step::Reopen { task: index } => {
                (OWNER, Op::Tasks(TaskOp::Reopen { task: task(index) }))
            }
            Step::Cancel { task: index } => (
                OWNER,
                Op::Cancel {
                    task: task(index),
                    reason: "cancelled".to_owned(),
                },
            ),
            Step::Settle { task: index, done } => (
                OWNER,
                Op::Sync {
                    sync: SyncOp::Settle {
                        task: task(index),
                        outcome: if done {
                            Settle::Done("settled".to_owned())
                        } else {
                            Settle::Cancelled("settled".to_owned())
                        },
                    },
                },
            ),
        };
        let (id, accepted) = world.submit(by, op, true)?;
        if accepted && let Step::Claim { task, worker, .. } = *step {
            world.held.insert((task, worker), ClaimId(id.0));
        }
        Ok(accepted)
    }

    fn key(&self, world: &World) -> String {
        world.facts()
    }

    fn parts(&self, world: &World) -> Vec<Part> {
        (0..TASKS.len())
            .filter_map(|index| {
                world.stage(index).map(|stage| Part {
                    name: TASKS[index].to_owned(),
                    phase: stage.wire(),
                    detail: world.view(index),
                })
            })
            .collect()
    }

    fn moves(&self) -> Vec<(&'static str, &'static str)> {
        Stage::MOVES
            .iter()
            .flat_map(|(from, tos)| {
                tos.iter()
                    .filter(move |to| *to != from)
                    .map(move |to| (from.wire(), to.wire()))
            })
            .collect()
    }

    fn stays(&self) -> Vec<(&'static str, &'static str)> {
        STAYS
            .iter()
            .map(|(stage, act)| (stage.wire(), act.wire()))
            .collect()
    }

    fn addresses(&self, step: &Step) -> Option<String> {
        step.act().map(|(index, _)| TASKS[index].to_owned())
    }

    fn done(&self, world: &World) -> bool {
        (0..TASKS.len()).all(|index| world.stage(index).is_some_and(resting))
    }

    fn invariant(&self, world: &World) -> Result<(), String> {
        for (index, name) in TASKS.iter().enumerate() {
            let Some(task) = world.task(index) else {
                continue;
            };
            let phase = |id: &TaskId| world.fold.task(id).map(|need| &need.phase);
            if let Phase::Claimed { claim, worker, .. } = &task.phase {
                if let Some(need) = task
                    .spec
                    .needs
                    .iter()
                    .find(|need| !matches!(phase(need), Some(Phase::Done { .. })))
                {
                    return Err(format!(
                        "{name} is claimed while its need {need} is not done"
                    ));
                }
                let holder = WORKERS.iter().position(|known| known == worker);
                if holder.and_then(|holder| world.held.get(&(index, holder))) != Some(claim) {
                    return Err(format!("{worker} holds {name} without knowing its claim"));
                }
            }
            let doomed = task.spec.needs.iter().find(|need| {
                world.fold.task(need).is_some_and(|need| match need.phase {
                    Phase::Cancelled { .. } => true,
                    Phase::Failed { .. } => need.spec.terms.on_fail.is_cascade(),
                    _ => false,
                })
            });
            if let Some(need) = doomed
                && !task.phase.terminal()
            {
                return Err(format!(
                    "{name} is still live after its need {need} did not finish"
                ));
            }
        }
        Ok(())
    }

    fn after(
        &self,
        before: &World,
        step: &Step,
        after: &World,
        accepted: bool,
    ) -> Result<(), String> {
        if let Some(echo) = &after.echo {
            return Err(echo.clone());
        }
        if !accepted {
            return Ok(());
        }
        for (index, name) in TASKS.iter().enumerate() {
            let (old, new) = (before.attempts(index), after.attempts(index));
            let claimed = matches!(step, Step::Claim { task, .. } if *task == index);
            if new != old + u64::from(claimed) {
                return Err(format!(
                    "{name} attempts went {old} -> {new} on an accepted {}",
                    self.label(step)
                ));
            }
        }
        if let Step::Claim { task, .. } = *step
            && let Some(stage) = before.stage(task)
            && !Act::Claim.from().contains(&stage)
        {
            return Err(format!("claimed {} from {}", TASKS[task], stage.wire()));
        }
        if let Some((task, worker)) = step.holder()
            && !before.current(task, worker)
        {
            return Err(format!(
                "{} changed {} without holding its claim",
                WORKERS[worker], TASKS[task]
            ));
        }
        Ok(())
    }

    fn idempotent(&self, step: &Step) -> bool {
        !matches!(step, Step::Tick | Step::Claim { lapsed: true, .. })
    }
}

fn folded(deep: bool, stop: Option<&'static str>) -> Report {
    walk(
        &Folded {
            depth: if deep { 14 } else { 10 },
        },
        stop,
    )
}

pub fn models() -> Vec<Entry> {
    vec![Entry {
        name: "claim_fold",
        run: folded,
    }]
}

fn plant(mutation: Mutation, on: bool) {
    mutate(if on { mutation } else { Mutation::None });
}

pub fn canaries() -> Vec<Canary> {
    vec![
        Canary {
            name: "claim_lifecycle_unchecked",
            model: "claim_fold",
            property: "declared",
            plant: |on| plant(Mutation::LifecycleUnchecked, on),
        },
        Canary {
            name: "claim_refusals_count_attempts",
            model: "claim_fold",
            property: "refused_unchanged",
            plant: |on| plant(Mutation::RefusalsCountAttempts, on),
        },
        Canary {
            name: "claim_replays_apply",
            model: "claim_fold",
            property: "postcondition",
            plant: |on| plant(Mutation::ReplaysApply, on),
        },
        Canary {
            name: "claim_unblock_refused",
            model: "claim_fold",
            property: "reached",
            plant: |on| plant(Mutation::UnblockRefused, on),
        },
        Canary {
            name: "claim_stalled_stuck",
            model: "claim_fold",
            property: "no_dead_end",
            plant: |on| plant(Mutation::StalledStuck, on),
        },
        Canary {
            name: "claim_reopen_anything",
            model: "claim_fold",
            property: "terminal_frozen",
            plant: |on| plant(Mutation::ReopenAnything, on),
        },
        Canary {
            name: "claim_ignores_block",
            model: "claim_fold",
            property: "declared",
            plant: |on| plant(Mutation::ClaimIgnoresBlock, on),
        },
        Canary {
            name: "claim_ignores_needs",
            model: "claim_fold",
            property: "invariant",
            plant: |on| plant(Mutation::ClaimIgnoresNeeds, on),
        },
        Canary {
            name: "claim_attempts_not_counted",
            model: "claim_fold",
            property: "postcondition",
            plant: |on| plant(Mutation::AttemptsNotCounted, on),
        },
        Canary {
            name: "claim_ignores_lease",
            model: "claim_fold",
            property: "idempotent",
            plant: |on| plant(Mutation::ClaimIgnoresLease, on),
        },
        Canary {
            name: "claim_due_ignored",
            model: "claim_fold",
            property: "declared",
            plant: |on| plant(Mutation::DueIgnored, on),
        },
        Canary {
            name: "claim_finish_ignores_claim",
            model: "claim_fold",
            property: "postcondition",
            plant: |on| plant(Mutation::FinishIgnoresClaim, on),
        },
    ]
}
