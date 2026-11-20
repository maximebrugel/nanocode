use nanocode_board::{
    Event, EventId, Fold, NewTask, Op, TaskId,
    delivery::{STAYS, Signal, Slot, Stage},
    mirror::{Join, Mode, SyncOp, Target},
    sim::{Mutation, mutate},
};
use nanocode_machine::Machine;

use super::{Canary, Entry, Model, Part, Report, walk};

const WRITER: &str = "agent:delivery";
const PART: &str = "delivery";

fn moves() -> Vec<(&'static str, &'static str)> {
    Stage::MOVES
        .iter()
        .flat_map(|(from, tos)| tos.iter().map(move |to| (from.wire(), to.wire())))
        .collect()
}

fn stays() -> Vec<(&'static str, &'static str)> {
    STAYS
        .iter()
        .map(|(stage, signal)| (stage.wire(), signal.wire()))
        .collect()
}

const fn resting(stage: Stage) -> bool {
    matches!(stage, Stage::Removed | Stage::GivenUp)
}

fn counts(before: Slot, after: Slot, signal: Signal) -> Result<(), String> {
    match signal {
        Signal::Attempt
            if after.attempts != before.attempts + 1 || after.throttled != before.throttled =>
        {
            Err(format!(
                "an attempt moved the counters {before:?} -> {after:?}"
            ))
        }
        Signal::Refund
            if after.attempts != before.attempts || after.counted() + 1 != before.counted() =>
        {
            Err(format!(
                "a 429 refund did not take back exactly one counted attempt {before:?} -> {after:?}"
            ))
        }
        Signal::GiveUp if before.attempts == 0 && !before.orphan => {
            Err(format!("gave up on {before:?} without an attempt"))
        }
        _ => Ok(()),
    }
}

fn sound(slot: Slot) -> Result<(), String> {
    if slot.throttled > slot.attempts {
        return Err(format!("more refunds than attempts in {slot:?}"));
    }
    if slot.copy.is_some() && slot.dead {
        return Err(format!(
            "a delivered copy is still dead-lettered in {slot:?}"
        ));
    }
    if slot.stage() == Stage::GivenUp && slot.attempts == 0 && !slot.orphan {
        return Err(format!("dead-lettered without an attempt in {slot:?}"));
    }
    Ok(())
}

pub struct Table {
    bound: u64,
}

impl Model for Table {
    type State = Slot;
    type Event = Signal;

    fn name(&self) -> &'static str {
        "delivery_table"
    }

    fn depth(&self) -> usize {
        usize::try_from(self.bound * 2 + 4).unwrap_or(usize::MAX)
    }

    fn initial(&self) -> Result<Vec<Slot>, String> {
        Ok(vec![
            Slot::default(),
            Slot {
                orphan: true,
                ..Slot::default()
            },
        ])
    }

    fn events(&self, slot: &Slot) -> Vec<Signal> {
        Signal::ALL
            .into_iter()
            .filter(|signal| *signal != Signal::Attempt || slot.attempts < self.bound)
            .collect()
    }

    fn label(&self, signal: &Signal) -> &'static str {
        signal.wire()
    }

    fn apply(&self, slot: &mut Slot, signal: &Signal) -> Result<bool, String> {
        Ok(slot.on(*signal).map(|next| *slot = next).is_ok())
    }

    fn key(&self, slot: &Slot) -> String {
        format!("{slot:?}")
    }

    fn parts(&self, slot: &Slot) -> Vec<Part> {
        vec![Part {
            name: PART.to_owned(),
            phase: slot.stage().wire(),
            detail: format!("{slot:?}"),
        }]
    }

    fn moves(&self) -> Vec<(&'static str, &'static str)> {
        moves()
    }

    fn stays(&self) -> Vec<(&'static str, &'static str)> {
        stays()
    }

    fn addresses(&self, _signal: &Signal) -> Option<String> {
        Some(PART.to_owned())
    }

    fn done(&self, slot: &Slot) -> bool {
        resting(slot.stage())
    }

    fn invariant(&self, slot: &Slot) -> Result<(), String> {
        sound(*slot)
    }

    fn after(
        &self,
        before: &Slot,
        signal: &Signal,
        after: &Slot,
        accepted: bool,
    ) -> Result<(), String> {
        if accepted {
            counts(*before, *after, *signal)
        } else {
            Ok(())
        }
    }

    fn idempotent(&self, signal: &Signal) -> bool {
        matches!(signal, Signal::Receipt | Signal::Remove | Signal::GiveUp)
    }
}

#[derive(Clone)]
pub struct World {
    fold: Fold,
    seq: u64,
    revision: u64,
    deleted: bool,
    root: EventId,
    message: EventId,
}

#[derive(Clone, Debug)]
pub enum Step {
    Attempt,
    Refund,
    Receipt { late: bool },
    Remove,
    GiveUp,
    Revise,
    Retract,
    DeadRoot,
}

impl Step {
    const fn signal(&self) -> Option<Signal> {
        match self {
            Self::Attempt => Some(Signal::Attempt),
            Self::Refund => Some(Signal::Refund),
            Self::Receipt { .. } => Some(Signal::Receipt),
            Self::Remove => Some(Signal::Remove),
            Self::GiveUp => Some(Signal::GiveUp),
            Self::Revise | Self::Retract | Self::DeadRoot => None,
        }
    }
}

fn task() -> TaskId {
    TaskId("t".to_owned())
}

fn target() -> Target {
    Target::new("chat", "c1")
}

impl World {
    fn submit(&mut self, op: Op) -> Result<(EventId, bool), String> {
        self.seq += 1;
        let id = EventId(format!("{WRITER}.{}", self.seq));
        let event = Event {
            id: id.clone(),
            by: WRITER.to_owned(),
            at_ms: self.seq * 1_000,
            op,
        };
        let body = serde_json::to_string(&event).map_err(|error| error.to_string())?;
        self.fold
            .apply(self.seq, &body)
            .map_err(|error| format!("delivery fold: {error}"))?;
        let accepted = matches!(self.fold.outcome(&id), Some(Ok(_)));
        Ok((id, accepted))
    }

    fn slot(&self) -> Slot {
        self.fold.mirror().slot(&self.message, &target())
    }

    fn owed(&self) -> Vec<String> {
        self.fold
            .mirror()
            .outbox()
            .into_iter()
            .filter(|delivery| delivery.message == self.message)
            .map(|delivery| format!("{:?}@{}", delivery.action, delivery.revision))
            .collect()
    }
}

pub struct Folded {
    bound: u64,
    revisions: u64,
    depth: usize,
}

impl Model for Folded {
    type State = World;
    type Event = Step;

    fn name(&self) -> &'static str {
        "delivery_fold"
    }

    fn depth(&self) -> usize {
        self.depth
    }

    fn initial(&self) -> Result<Vec<World>, String> {
        let mut world = World {
            fold: Fold::new(),
            seq: 0,
            revision: 0,
            deleted: false,
            root: EventId(String::new()),
            message: EventId(String::new()),
        };
        let post = NewTask {
            id: task(),
            parent: None,
            spec: "deliver me".to_owned(),
            labels: Default::default(),
            needs: Vec::new(),
            terms: Default::default(),
        };
        let setup: [Op; 4] = [
            Op::Charter {},
            Op::Post { task: post },
            Op::Sync {
                sync: SyncOp::Link {
                    task: task(),
                    target: target(),
                    join: Join::Thread,
                    mode: Mode::Always,
                },
            },
            Op::Reply {
                task: task(),
                body: "hello".to_owned(),
            },
        ];
        for op in setup {
            let shown = format!("{op:?}");
            let (id, accepted) = world.submit(op)?;
            if !accepted {
                return Err(format!(
                    "setup refused {shown}: {:?}",
                    world.fold.outcome(&id)
                ));
            }
            if world.root.0.is_empty()
                && world.fold.mirror().slot(&id, &target()) == Slot::default()
                && matches!(world.fold.outcome(&id), Some(Ok(_)))
                && shown.starts_with("Post")
            {
                world.root = id.clone();
            }
            world.message = id;
        }
        if world.owed().is_empty() {
            return Err("the reply owes no delivery".to_owned());
        }
        Ok(vec![world])
    }

    fn events(&self, world: &World) -> Vec<Step> {
        let mut steps = vec![
            Step::Refund,
            Step::Receipt { late: false },
            Step::Remove,
            Step::GiveUp,
            Step::Retract,
        ];
        if !world.fold.mirror().slot(&world.root, &target()).dead {
            steps.push(Step::DeadRoot);
        }
        if world.slot().attempts < self.bound {
            steps.push(Step::Attempt);
        }
        if world.revision > 0 {
            steps.push(Step::Receipt { late: true });
        }
        if world.revision < self.revisions {
            steps.push(Step::Revise);
        }
        steps
    }

    fn label(&self, step: &Step) -> &'static str {
        match step {
            Step::Revise => "revise",
            Step::Retract => "retract",
            Step::DeadRoot => "dead_root",
            _ => step.signal().map_or("", Signal::wire),
        }
    }

    fn apply(&self, world: &mut World, step: &Step) -> Result<bool, String> {
        let (task, message, target, revision) =
            (task(), world.message.clone(), target(), world.revision);
        if let Step::DeadRoot = step {
            let root = world.root.clone();
            let tried = SyncOp::Attempt {
                task: task.clone(),
                message: root.clone(),
                target: target.clone(),
                revision: 0,
            };
            let (_, attempted) = world.submit(Op::Sync { sync: tried })?;
            let dead = SyncOp::Undelivered {
                task,
                message: root,
                target,
                error: "dead root".to_owned(),
            };
            let (_, given) = world.submit(Op::Sync { sync: dead })?;
            return Ok(attempted && given);
        }
        let sync = match step {
            Step::Attempt => SyncOp::Attempt {
                task,
                message,
                target,
                revision,
            },
            Step::Refund => SyncOp::Throttled {
                task,
                message,
                target,
                revision,
            },
            Step::Receipt { late } => SyncOp::Synced {
                task,
                message,
                target,
                external: "x1".to_owned(),
                revision: if *late { revision - 1 } else { revision },
                thread: None,
            },
            Step::Remove => SyncOp::Removed {
                task,
                message,
                target,
            },
            Step::GiveUp => SyncOp::Undelivered {
                task,
                message,
                target,
                error: "failed".to_owned(),
            },
            Step::Revise => SyncOp::Revise {
                task,
                message,
                body: format!("hello {}", revision + 1),
            },
            Step::Retract | Step::DeadRoot => SyncOp::Retract { task, message },
        };
        let (_, accepted) = world.submit(Op::Sync { sync })?;
        if accepted {
            match step {
                Step::Revise => world.revision += 1,
                Step::Retract => world.deleted = true,
                _ => {}
            }
        }
        Ok(accepted)
    }

    fn key(&self, world: &World) -> String {
        let held = world
            .fold
            .mirror()
            .held(&world.message, &target())
            .map(|copy| (copy.revision, copy.removed));
        format!(
            "{:?} root={:?} held={held:?} rev={} deleted={} notes={} owed={:?}",
            world.slot(),
            world.fold.mirror().slot(&world.root, &target()),
            world.revision,
            world.deleted,
            world.fold.entries().len(),
            world.owed()
        )
    }

    fn parts(&self, world: &World) -> Vec<Part> {
        let held = world
            .fold
            .mirror()
            .held(&world.message, &target())
            .map(|copy| (copy.revision, copy.removed));
        vec![Part {
            name: PART.to_owned(),
            phase: world.slot().stage().wire(),
            detail: format!(
                "{:?} held={held:?}",
                Slot {
                    orphan: false,
                    ..world.slot()
                }
            ),
        }]
    }

    fn moves(&self) -> Vec<(&'static str, &'static str)> {
        moves()
    }

    fn stays(&self) -> Vec<(&'static str, &'static str)> {
        stays()
    }

    fn addresses(&self, step: &Step) -> Option<String> {
        step.signal().map(|_| PART.to_owned())
    }

    fn done(&self, world: &World) -> bool {
        resting(world.slot().stage()) || world.owed().is_empty()
    }

    fn invariant(&self, world: &World) -> Result<(), String> {
        sound(world.slot())
    }

    fn after(
        &self,
        before: &World,
        step: &Step,
        after: &World,
        accepted: bool,
    ) -> Result<(), String> {
        match step.signal() {
            Some(signal) if accepted => counts(before.slot(), after.slot(), signal),
            _ => Ok(()),
        }
    }

    fn idempotent(&self, step: &Step) -> bool {
        matches!(
            step,
            Step::Receipt { .. } | Step::Remove | Step::GiveUp | Step::Retract
        )
    }
}

fn table(deep: bool, stop: Option<&'static str>) -> Report {
    walk(
        &Table {
            bound: if deep { 6 } else { 3 },
        },
        stop,
    )
}

fn folded(deep: bool, stop: Option<&'static str>) -> Report {
    let model = if deep {
        Folded {
            bound: 4,
            revisions: 2,
            depth: 11,
        }
    } else {
        Folded {
            bound: 2,
            revisions: 1,
            depth: 7,
        }
    };
    walk(&model, stop)
}

pub fn models() -> Vec<Entry> {
    vec![
        Entry {
            name: "delivery_table",
            run: table,
        },
        Entry {
            name: "delivery_fold",
            run: folded,
        },
    ]
}

fn plant(mutation: Mutation, on: bool) {
    mutate(if on { mutation } else { Mutation::None });
}

pub fn canaries() -> Vec<Canary> {
    vec![
        Canary {
            name: "delivery_refusals_write",
            model: "delivery_fold",
            property: "refused_unchanged",
            plant: |on| plant(Mutation::DeliveryRefusalsWrite, on),
        },
        Canary {
            name: "delivery_removed_edits",
            model: "delivery_fold",
            property: "terminal_frozen",
            plant: |on| plant(Mutation::DeliveryRemovedEdits, on),
        },
        Canary {
            name: "delivery_dead_repeats",
            model: "delivery_table",
            property: "declared",
            plant: |on| plant(Mutation::DeliveryDeadRepeats, on),
        },
        Canary {
            name: "delivery_dead_stays",
            model: "delivery_table",
            property: "reached",
            plant: |on| plant(Mutation::DeliveryDeadStays, on),
        },
        Canary {
            name: "delivery_receipt_keeps_dead",
            model: "delivery_fold",
            property: "invariant",
            plant: |on| plant(Mutation::DeliveryReceiptKeepsDead, on),
        },
        Canary {
            name: "delivery_attempt_uncounted",
            model: "delivery_fold",
            property: "postcondition",
            plant: |on| plant(Mutation::DeliveryAttemptUncounted, on),
        },
        Canary {
            name: "delivery_receipts_renote",
            model: "delivery_fold",
            property: "idempotent",
            plant: |on| plant(Mutation::DeliveryReceiptsRenote, on),
        },
        Canary {
            name: "delivery_throttled_stuck",
            model: "delivery_table",
            property: "no_dead_end",
            plant: |on| plant(Mutation::DeliveryThrottledStuck, on),
        },
    ]
}
