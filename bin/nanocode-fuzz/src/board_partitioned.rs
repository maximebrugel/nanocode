//! A partitioned board driven end to end: P partition folds, a lease row per partition whose
//! takeovers raise the epoch claims are fenced by, tasks homed by their thread's UUID, and
//! claims under a label-scoped or partition-wide cap.
//!
//! Oracles, checked on every write:
//! - one live claim per task: a claim lands only on a task whose last claim lapsed, was released
//!   or finished, and a fenced claim lands only at its partition's current lease epoch;
//! - the cap holds: no partition ever holds more covered tasks than the cap, since every claim on
//!   a covered task carries it and a lapsed claim renews only into room under its cap;
//! - a finished task keeps the worker whose claim finished it;
//! - a thread's tasks home in partition_of over its UUID's raw bytes however the label spells the
//!   UUID, a child without a key of its own homes with its parent, and a partition's scope is the
//!   one pyramid2 opens for it (subagents/p{n}, the bare scope at P = 1);
//! - one serial fold of every accepted row, caps stripped, accepts each of them and ends with the
//!   same tasks as the merged partition folds.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use nanocode_board::{
    Cap, ClaimId, Event, EventId, Fence, Fold, NewTask, Op, Outcome, Phase, Rejection, Task,
    TaskId,
    access::{Can, Grant, Who},
    shards::{self, Merged, Route},
    tasks::Terms,
};
use nanocode_partitions::{Partition, Row, partition_of};

use crate::{Checked, rng::Rng};

const OWNER: &str = "user:lead";
const NODES: [&str; 3] = ["n0", "n1", "n2"];
const WORKERS: [&str; 4] = ["agent:w0", "agent:w1", "agent:w2", "user:w3"];
const POOL_KEY: &str = "pool";
const POOL: &str = "lent";

#[derive(Clone, Debug)]
struct Live {
    claim: ClaimId,
    worker: String,
    until_ms: u64,
}

type Verdict = Result<u64, Rejection>;

struct World {
    count: NonZeroU32,
    folds: Vec<Fold>,
    leases: Vec<Row<String>>,
    now: u64,
    minted: u64,
    cap: Option<Cap>,
    tasks: Vec<TaskId>,
    /// The claim the model believes each task holds, until it ends.
    claims: BTreeMap<TaskId, Live>,
    /// Claims superseded by a takeover, which their old holders may still try to use.
    superseded: Vec<(TaskId, Live)>,
    /// Every accepted event once, in the order it landed, for the serial reference.
    accepted: Vec<Event>,
}

fn spell(rng: &mut Rng, bytes: &[u8; 16]) -> String {
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let hex = if rng.chance(1, 4) {
        hex.to_uppercase()
    } else {
        hex
    };
    if rng.chance(1, 2) {
        format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        )
    } else {
        hex
    }
}

fn uuid(rng: &mut Rng) -> [u8; 16] {
    let mut bytes = [0; 16];
    for byte in &mut bytes {
        *byte = rng.next().to_le_bytes()[0];
    }
    // Thread ids are v4 or v7; a few keep whatever nibble the bytes gave.
    match rng.below(3) {
        0 => bytes[6] = (bytes[6] & 0x0f) | 0x40,
        1 => bytes[6] = (bytes[6] & 0x0f) | 0x70,
        _ => {}
    }
    bytes
}

impl World {
    fn merged(&self) -> Merged<'_> {
        Merged::new(self.folds.iter().collect())
    }

    fn write(&mut self, by: &str, op: Op) -> Result<(Option<Partition>, Verdict), String> {
        let (route, op) = self.merged().place(by, op);
        let parts: Vec<Partition> = match &route {
            Route::One(partition) => vec![*partition],
            Route::Every => (0..self.count.get()).map(Partition).collect(),
            Route::Cross { .. } | Route::Refused(_) => {
                return Err(format!("{op:?} routed {route:?}"));
            }
        };
        self.minted += 1;
        let event = Event {
            id: EventId(format!("{by}.{}", self.minted)),
            by: by.to_owned(),
            at_ms: self.now,
            op,
        };
        let body = serde_json::to_string(&event).map_err(|error| error.to_string())?;
        let mut verdict: Option<Verdict> = None;
        for partition in &parts {
            let fold = &mut self.folds[partition.0 as usize];
            fold.apply(fold.next(), &body)
                .map_err(|error| format!("partition {} refused a row: {error}", partition.0))?;
            let outcome = fold
                .outcome(&event.id)
                .cloned()
                .ok_or_else(|| format!("{} has no outcome", event.id))?;
            if let Some(earlier) = &verdict {
                ensure!(
                    *earlier == outcome,
                    "partitions disagree on {}: {earlier:?} then {outcome:?}",
                    event.id
                );
            }
            verdict = Some(outcome);
        }
        let verdict = verdict.ok_or("a write went to no partition")?;
        if verdict.is_ok() {
            self.accepted.push(event);
        }
        let one = match route {
            Route::One(partition) => Some(partition),
            _ => None,
        };
        Ok((one, verdict))
    }

    fn takeover(&mut self, rng: &mut Rng) -> Checked {
        let index = rng.index(self.folds.len());
        let partition = u32::try_from(index).map_err(|error| error.to_string())?;
        let node = rng.word(&NODES);
        let ttl = rng.range(1, 2_000);
        let live = rng.chance(1, 2);
        let current = self.leases[index].epoch();
        let Ok((row, epoch)) = self.leases[index].claim(&node, self.now, ttl, live) else {
            // The lease is held: an epoch at or below the fence must bounce.
            let stale = rng.below(current + 1);
            let (_, verdict) = self.write(
                OWNER,
                Op::Epoch {
                    partition,
                    epoch: stale,
                },
            )?;
            ensure!(
                matches!(verdict, Err(Rejection::Stale { .. })),
                "epoch {stale} on partition {partition} at {current} gave {verdict:?}"
            );
            return Ok(());
        };
        ensure!(
            epoch > current,
            "a takeover of partition {partition} moved its epoch {current} -> {epoch}"
        );
        self.leases[index] = row;
        let (_, verdict) = self.write(OWNER, Op::Epoch { partition, epoch })?;
        ensure!(
            verdict.is_ok(),
            "the takeover's epoch {epoch} on partition {partition} was refused: {verdict:?}"
        );
        let fenced = self.folds[index].epoch(partition);
        ensure!(
            fenced == epoch,
            "partition {partition} fences at {fenced} after a takeover to {epoch}"
        );
        // Holders on this partition may now be stale; their claims stay until they lapse.
        Ok(())
    }

    fn claim(&mut self, rng: &mut Rng) -> Checked {
        let task = rng.pick(&self.tasks).clone();
        let home = self
            .merged()
            .home(&task)
            .ok_or_else(|| format!("{task} has no home"))?;
        let index = home.0 as usize;
        let current = self.leases[index].epoch();
        let fence = match rng.below(6) {
            0 => None,
            1 => Some(current.saturating_sub(1)),
            2 => Some(rng.below(current + 2)),
            _ => Some(current),
        }
        .map(|epoch| Fence {
            partition: home.0,
            epoch,
        });
        let covered = self.cap.as_ref().is_some_and(|cap| {
            self.merged()
                .task(&task)
                .is_some_and(|held| cap.covers(held))
        });
        let cap = if covered || rng.chance(1, 3) {
            self.cap.clone()
        } else {
            None
        };
        let worker = rng.word(&WORKERS);
        let lease_ms = rng.range(1, 600);
        let held_before = cap
            .as_ref()
            .map(|cap| self.folds[index].held(cap, self.now));
        let live_before = self
            .claims
            .get(&task)
            .filter(|live| live.until_ms > self.now)
            .cloned();
        let (routed, verdict) = self.write(
            &worker,
            Op::Claim {
                task: task.clone(),
                lease_ms,
                fence,
                cap: cap.clone(),
            },
        )?;
        ensure!(
            routed == Some(home),
            "a claim on {task} routed to {routed:?}, homed in {home:?}"
        );
        match verdict {
            Ok(_) => {
                ensure!(
                    live_before.is_none(),
                    "{task} claimed by {worker} at {} while {live_before:?} held it",
                    self.now
                );
                if let Some(fence) = fence {
                    ensure!(
                        fence.epoch == current,
                        "{task} claimed under {fence:?} with the lease at epoch {current}"
                    );
                }
                if let (Some(cap), Some(held)) = (&cap, held_before) {
                    ensure!(
                        held < cap.max() as usize,
                        "{task} claimed under {cap:?} with {held} already held"
                    );
                }
                let claim = self
                    .accepted
                    .last()
                    .map(|event| ClaimId(event.id.0.clone()))
                    .ok_or("an accepted claim was recorded")?;
                let fresh = Live {
                    claim,
                    worker,
                    until_ms: self.now.saturating_add(lease_ms),
                };
                if let Some(old) = self.claims.insert(task.clone(), fresh) {
                    self.superseded.push((task, old));
                }
            }
            Err(Rejection::Stale { fence: held }) => ensure!(
                fence.is_some_and(|fence| fence.epoch != current) && held.epoch == current,
                "a claim on {task} under {fence:?} was refused stale at {held:?}, lease at {current}"
            ),
            Err(Rejection::Full { cap: full }) => ensure!(
                Some(&full) == cap.as_ref() && held_before >= Some(full.max() as usize),
                "a claim on {task} was refused full under {full:?} with {held_before:?} held"
            ),
            Err(Rejection::Held { .. }) => ensure!(
                live_before.is_some(),
                "a claim on {task} was refused as held, but its last claim ended"
            ),
            Err(_) => {}
        }
        Ok(())
    }

    /// A holder, or the holder of a superseded claim, renews, releases or finishes.
    fn settle(&mut self, rng: &mut Rng) -> Checked {
        let picked = if !self.superseded.is_empty() && rng.chance(1, 5) {
            let at = rng.index(self.superseded.len());
            Some(self.superseded[at].clone())
        } else {
            self.claims
                .iter()
                .nth(rng.index(self.claims.len().max(1)))
                .map(|(task, live)| (task.clone(), live.clone()))
        };
        let Some((task, live)) = picked else {
            return Ok(());
        };
        let holds = matches!(
            self.merged().task(&task).map(|held| &held.phase),
            Some(Phase::Claimed { claim, .. }) if *claim == live.claim
        );
        let mine = |world: &Self| {
            world
                .claims
                .get(&task)
                .is_some_and(|now| now.claim == live.claim)
        };
        match rng.below(3) {
            0 => {
                let lease_ms = rng.range(1, 600);
                // A lapsed claim renews only into room under the cap it was taken with.
                let home = self.merged().home(&task).map(|home| home.0 as usize);
                let full = match (&self.cap, home) {
                    (Some(cap), Some(home)) if live.until_ms <= self.now => self
                        .merged()
                        .task(&task)
                        .and_then(|held| held.cap.clone())
                        .filter(|taken| {
                            taken == cap
                                && self.folds[home].held(cap, self.now) >= cap.max() as usize
                        }),
                    _ => None,
                };
                let (_, verdict) = self.write(
                    &live.worker,
                    Op::Renew {
                        task: task.clone(),
                        claim: live.claim.clone(),
                        lease_ms,
                    },
                )?;
                if let (Err(Rejection::Full { cap }), true, Some(full)) = (&verdict, holds, &full) {
                    ensure!(
                        cap == full,
                        "a lapsed renew of {task} refused under {cap:?}, taken under {full:?}"
                    );
                } else {
                    ensure!(
                        verdict.is_ok() == holds,
                        "a renew of {task} by {} (holds: {holds}, full: {full:?}) gave {verdict:?}",
                        live.worker
                    );
                }
                if verdict.is_ok() {
                    ensure!(mine(self), "a superseded claim on {task} renewed");
                    let until_ms = self.now.saturating_add(lease_ms);
                    self.claims.insert(task, Live { until_ms, ..live });
                }
            }
            1 => {
                let (_, verdict) = self.write(
                    &live.worker,
                    Op::Release {
                        task: task.clone(),
                        claim: live.claim.clone(),
                        note: String::new(),
                    },
                )?;
                ensure!(
                    verdict.is_ok() == holds,
                    "a release of {task} (holds: {holds}) gave {verdict:?}"
                );
                if verdict.is_ok() {
                    ensure!(mine(self), "a superseded claim on {task} released");
                    self.claims.remove(&task);
                }
            }
            _ => {
                let outcome = if rng.chance(3, 4) {
                    Outcome::Done(rng.text(3))
                } else {
                    Outcome::Failed(rng.text(3))
                };
                let (_, verdict) = self.write(
                    &live.worker,
                    Op::Finish {
                        task: task.clone(),
                        claim: live.claim.clone(),
                        outcome,
                    },
                )?;
                ensure!(
                    verdict.is_ok() == holds,
                    "a finish of {task} (holds: {holds}) gave {verdict:?}"
                );
                if verdict.is_ok() {
                    ensure!(mine(self), "a superseded claim on {task} finished");
                    match self.merged().task(&task).map(|held| &held.phase) {
                        Some(Phase::Done { worker, .. } | Phase::Failed { worker, .. }) => ensure!(
                            worker.as_ref() == Some(&live.worker),
                            "{task} finished under {}'s claim keeps worker {worker:?}",
                            live.worker
                        ),
                        Some(Phase::Open) => {}
                        other => return Err(format!("{task} finished into {other:?}")),
                    }
                    self.claims.remove(&task);
                }
            }
        }
        Ok(())
    }

    fn capped(&self) -> Checked {
        let Some(cap) = &self.cap else {
            return Ok(());
        };
        for (index, fold) in self.folds.iter().enumerate() {
            let held = fold.held(cap, self.now);
            ensure!(
                held <= cap.max() as usize,
                "partition {index} holds {held} tasks under {cap:?} at {}",
                self.now
            );
        }
        Ok(())
    }

    fn reference(&self) -> Checked {
        let mut serial = Fold::default();
        for event in &self.accepted {
            let mut event = event.clone();
            if let Op::Claim { cap, .. } = &mut event.op {
                *cap = None;
            }
            let body = serde_json::to_string(&event).map_err(|error| error.to_string())?;
            serial
                .apply(serial.next(), &body)
                .map_err(|error| format!("the serial fold refused {}: {error}", event.id))?;
            let outcome = serial.outcome(&event.id);
            ensure!(
                matches!(outcome, Some(Ok(_))),
                "the serial fold judged {} ({:?}) {outcome:?}",
                event.id,
                event.op
            );
        }
        let merged = self.merged();
        let tasks = merged.tasks();
        ensure!(
            tasks.len() == serial.tasks().len(),
            "{} merged tasks, {} serial",
            tasks.len(),
            serial.tasks().len()
        );
        for (id, task) in serial.tasks() {
            // The serial fold saw no caps, so no task there records one.
            let mut merged_task = tasks.get(id).map(|held| Task::clone(held));
            if let Some(merged_task) = &mut merged_task {
                merged_task.cap = None;
            }
            ensure!(
                merged_task.as_ref() == Some(Task::clone(task)).as_ref(),
                "{id} differs: merged {merged_task:?}, serial {task:?}"
            );
        }
        Ok(())
    }
}

fn post(world: &mut World, rng: &mut Rng, threads: &[[u8; 16]]) -> Checked {
    let index = world.tasks.len();
    let thread = (!threads.is_empty() && rng.chance(3, 4)).then(|| *rng.pick(threads));
    let parent =
        (!world.tasks.is_empty() && rng.chance(1, 3)).then(|| rng.pick(&world.tasks).clone());
    let mut labels = BTreeMap::new();
    if let Some(thread) = &thread {
        labels.insert(shards::THREAD_LABEL.to_owned(), spell(rng, thread));
    }
    if rng.chance(1, 2) {
        labels.insert(POOL_KEY.to_owned(), POOL.to_owned());
    }
    let id = TaskId(format!("t{index}"));
    let task = NewTask {
        id: id.clone(),
        parent: parent.clone(),
        spec: format!("task {index}"),
        labels,
        needs: Vec::new(),
        terms: Terms::default(),
    };
    let parent_home = parent
        .as_ref()
        .and_then(|parent| world.merged().home(parent));
    let (routed, verdict) = world.write(OWNER, Op::Post { task })?;
    ensure!(verdict.is_ok(), "the post of {id} was refused: {verdict:?}");
    let home = world.merged().home(&id);
    ensure!(
        home == routed,
        "{id} routed to {routed:?} but homes in {home:?}"
    );
    if let Some(thread) = thread {
        let want = partition_of(&thread, world.count);
        ensure!(
            home == Some(want),
            "{id} of thread {thread:02x?} homes in {home:?}, not {want:?}"
        );
    } else if let Some(parent_home) = parent_home {
        ensure!(
            home == Some(parent_home),
            "child {id} homes in {home:?}, its parent in {parent_home:?}"
        );
    }
    world.tasks.push(id);
    Ok(())
}

fn scopes(count: NonZeroU32) -> Checked {
    for partition in [0, count.get() / 2, count.get() - 1] {
        let scope = shards::scope_of("subagents", count, Partition(partition));
        let want = if count.get() == 1 {
            "subagents".to_owned()
        } else {
            format!("subagents/p{partition}")
        };
        ensure!(
            scope == want,
            "partition {partition} of {count} is scope {scope}, pyramid2 opens {want}"
        );
    }
    Ok(())
}

pub fn check(rng: &mut Rng) -> Checked {
    let count = NonZeroU32::new(*rng.pick(&[1, 2, 3, 4, 7, 8, 64])).ok_or("a nonzero count")?;
    scopes(count)?;
    let max = u32::try_from(rng.range(1, 4)).map_err(|error| error.to_string())?;
    let cap = match rng.below(3) {
        0 => None,
        1 => Some(Cap::Partition(max)),
        _ => Some(Cap::Label {
            max,
            key: POOL_KEY.to_owned(),
            value: POOL.to_owned(),
        }),
    };
    let mut world = World {
        count,
        folds: (0..count.get()).map(|_| Fold::default()).collect(),
        leases: (0..count.get()).map(|_| Row::Free { epoch: 0 }).collect(),
        now: rng.below(1_000),
        minted: 0,
        cap,
        tasks: Vec::new(),
        claims: BTreeMap::new(),
        superseded: Vec::new(),
        accepted: Vec::new(),
    };
    let (_, chartered) = world.write(OWNER, Op::Charter {})?;
    ensure!(chartered.is_ok(), "the charter was refused: {chartered:?}");
    let grant = Grant {
        who: vec![Who::Everyone],
        can: BTreeMap::from([("*".to_owned(), Can::ALL.to_vec())]),
    };
    let (_, granted) = world.write(OWNER, Op::Grant { grant })?;
    ensure!(granted.is_ok(), "the grant was refused: {granted:?}");
    for _ in 0..count.get().min(8) {
        world.takeover(rng)?;
    }
    let threads: Vec<[u8; 16]> = (0..rng.range(0, 6)).map(|_| uuid(rng)).collect();
    for thread in &threads {
        let spelled = spell(rng, thread);
        let read = shards::uuid_bytes(&spelled);
        ensure!(
            read.as_ref() == Some(thread),
            "{spelled} reads back as {read:02x?}"
        );
    }
    for _ in 0..rng.range(1, 12) {
        post(&mut world, rng, &threads)?;
    }
    for _ in 0..rng.range(10, 160) {
        world.now = world.now.saturating_add(rng.below(250));
        match rng.below(10) {
            0 => world.takeover(rng)?,
            1 => post(&mut world, rng, &threads)?,
            2..=5 => world.claim(rng)?,
            _ => world.settle(rng)?,
        }
        world.capped()?;
    }
    world.reference()
}
