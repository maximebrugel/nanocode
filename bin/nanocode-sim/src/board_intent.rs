use std::{collections::BTreeMap, sync::Arc};

use nanocode_board::{
    Board, BoardError, Change, Event, EventId, Fold, NewTask, Op, Outcome, Phase, Rejection,
    TaskId,
    access::{Can, Grant, Who},
    intent::{Intent, Refusal, Report},
    sim::Mutation as BoardRule,
    tasks::{Need, Planned, TaskOp, Terms},
};
use nanocode_turnlog::{LogHost, MemoryLog};

use crate::rng::Rng;

pub const ALL: [BoardRule; 3] = [
    BoardRule::GoalClaimedByAnyone,
    BoardRule::ReportsDropped,
    BoardRule::RetryPostsTwice,
];

pub const fn name(rule: BoardRule) -> Option<&'static str> {
    match rule {
        BoardRule::GoalClaimedByAnyone => Some("goal_claimed_by_anyone"),
        BoardRule::ReportsDropped => Some("reports_dropped"),
        BoardRule::RetryPostsTwice => Some("retry_posts_twice"),
        _ => None,
    }
}

const OWNER: &str = "agent:lead";
const PLANNER: &str = "agent:planner";
const WORKERS: [&str; 3] = ["agent:w0", "agent:w1", "agent:w2"];
const LEASE: u64 = 60_000;

#[derive(Clone, Copy, Debug, Default)]
pub struct IntentStats {
    pub goals: u64,
    pub reports: u64,
    pub retries: u64,
    pub refused: u64,
}

struct Host {
    log: Vec<String>,
    fold: Fold,
    minted: u64,
}

impl Host {
    fn append(&mut self, by: &str, at_ms: u64, op: Op) -> Result<Result<u64, Rejection>, String> {
        self.minted += 1;
        let id = EventId(format!("{by}.{}", self.minted));
        let event = Event {
            id: id.clone(),
            by: by.to_owned(),
            at_ms,
            op,
        };
        let body = serde_json::to_string(&event).map_err(|error| error.to_string())?;
        self.log.push(body);
        let seq = self.log.len() as u64;
        self.fold
            .apply(seq, &self.log[self.log.len() - 1])
            .map_err(|error| format!("intent fold: {error}"))?;
        self.fold
            .outcome(&id)
            .cloned()
            .ok_or_else(|| format!("{id} did not fold"))
    }

    fn refold(&self) -> Result<Fold, String> {
        let mut fold = Fold::new();
        for (index, body) in self.log.iter().enumerate() {
            fold.apply(index as u64 + 1, body)
                .map_err(|error| format!("intent refold: {error}"))?;
        }
        Ok(fold)
    }
}

fn grant(who: Who, can: &[Can]) -> Op {
    Op::Grant {
        grant: Grant {
            who: vec![who],
            can: BTreeMap::from([("*".to_owned(), can.to_vec())]),
        },
    }
}

fn posted(fold: &Fold, task: &TaskId) -> usize {
    fold.entries()
        .iter()
        .filter(|entry| &entry.task == task && matches!(entry.change, Change::Posted))
        .count()
}

fn reported(fold: &Fold, parent: &TaskId, child: &TaskId) -> Vec<Report> {
    fold.entries()
        .iter()
        .filter(|entry| &entry.task == parent)
        .filter_map(|entry| match &entry.change {
            Change::Reported {
                child: from,
                report,
            } if from == child => Some(report.clone()),
            _ => None,
        })
        .collect()
}

fn storm(
    host: &mut Host,
    rng: &mut Rng,
    now: u64,
    task: &NewTask,
    stats: &mut IntentStats,
) -> Result<u64, String> {
    let by = if task.parent.is_some() {
        PLANNER
    } else {
        WORKERS[rng.pick(WORKERS.len())]
    };
    let original = host
        .append(by, now, Op::Post { task: task.clone() })?
        .map_err(|rejection| format!("posting {} was refused: {rejection:?}", task.id))?;
    for _ in 0..rng.range(1, 5) {
        let writer = [OWNER, PLANNER, WORKERS[rng.pick(WORKERS.len())]][rng.pick(3)];
        stats.retries += 1;
        match host.append(writer, now, Op::Post { task: task.clone() })? {
            Ok(seq) if seq == original => {}
            other => {
                return Err(format!(
                    "a retried post of {} by {writer} gave {other:?}, not the original row {original}",
                    task.id
                ));
            }
        }
    }
    let mut changed = task.clone();
    changed.spec.push_str(" changed");
    if host.append(by, now, Op::Post { task: changed })? != Err(Rejection::Exists) {
        return Err(format!(
            "a different spec under {} was not refused",
            task.id
        ));
    }
    if posted(&host.fold, &task.id) != 1 {
        return Err(format!(
            "{} was posted {} times by a retry storm",
            task.id,
            posted(&host.fold, &task.id)
        ));
    }
    Ok(original)
}

fn goal(number: u64) -> NewTask {
    NewTask {
        id: TaskId(format!("goal{number}")),
        parent: None,
        spec: format!("goal {number}"),
        labels: BTreeMap::new(),
        needs: Vec::new(),
        terms: Terms {
            intent: Intent::Goal,
            ..Terms::default()
        },
    }
}

fn claim(
    host: &mut Host,
    by: &str,
    now: u64,
    task: &TaskId,
) -> Result<Option<nanocode_board::ClaimId>, String> {
    let outcome = host.append(
        by,
        now,
        Op::Claim {
            task: task.clone(),
            lease_ms: LEASE,
            fence: None,
            cap: None,
        },
    )?;
    Ok(
        match (outcome, host.fold.task(task).map(|task| task.phase.clone())) {
            (Ok(_), Some(Phase::Claimed { claim, .. })) => Some(claim),
            _ => None,
        },
    )
}

fn plan(
    host: &mut Host,
    rng: &mut Rng,
    now: u64,
    number: u64,
    stats: &mut IntentStats,
) -> Result<(), String> {
    let task = goal(number);
    let id = task.id.clone();
    storm(host, rng, now, &task, stats)?;
    stats.goals += 1;
    let mut nested = goal(number + 100);
    nested.parent = Some(id.clone());
    if host.append(PLANNER, now, Op::Post { task: nested })?
        != Err(Rejection::Intent(Refusal::GoalNotTopLevel))
    {
        return Err(format!("a goal under {id} was not refused"));
    }
    for worker in WORKERS {
        if host.fold.pick(worker, &BTreeMap::new(), now) == Some(&id) {
            return Err(format!("pick handed goal {id} to {worker}"));
        }
        if host.fold.pick_goal(worker, &BTreeMap::new(), now).is_some() {
            return Err(format!("pick_goal handed a goal to {worker}"));
        }
    }
    let intruder = WORKERS[rng.pick(WORKERS.len())];
    if claim(host, intruder, now, &id)?.is_some() {
        return Err(format!(
            "{intruder} claimed goal {id} without the planner role"
        ));
    }
    stats.refused += 1;
    if host.fold.pick_goal(PLANNER, &BTreeMap::new(), now) != Some(&id) {
        return Err(format!("pick_goal did not offer {id} to the planner"));
    }
    let goal_claim = claim(host, PLANNER, now, &id)?
        .ok_or_else(|| format!("the planner could not claim {id}"))?;
    let children = rng.range(1, 6);
    let batch = (0..children)
        .map(|key| Planned {
            key: format!("c{key}"),
            parent: Some(Need::Task(id.clone())),
            spec: format!("step {key} of {id}"),
            labels: BTreeMap::new(),
            needs: Vec::new(),
            terms: Terms::default(),
        })
        .collect::<Vec<_>>();
    let name = TaskId(format!("plan{number}"));
    host.append(
        PLANNER,
        now,
        Op::Tasks(TaskOp::Batch {
            name: name.clone(),
            tasks: batch,
        }),
    )?
    .map_err(|rejection| format!("the plan for {id} was refused: {rejection:?}"))?;
    let mut ids = (0..children)
        .map(|key| TaskId(format!("{name}/c{key}")))
        .collect::<Vec<_>>();
    for extra in 0..rng.range(0, 3) {
        let task = NewTask {
            id: TaskId(format!("{id}/extra{extra}")),
            parent: Some(id.clone()),
            spec: format!("extra {extra}"),
            labels: BTreeMap::new(),
            needs: Vec::new(),
            terms: Terms::default(),
        };
        storm(host, rng, now, &task, stats)?;
        ids.push(task.id);
    }
    if host.fold.unreported(&id) != ids.len() {
        return Err(format!(
            "{id} has {} unreported children, not {}",
            host.fold.unreported(&id),
            ids.len()
        ));
    }
    for child in &ids {
        let expected = match rng.below(3) {
            0 => {
                host.append(
                    OWNER,
                    now,
                    Op::Cancel {
                        task: child.clone(),
                        reason: "dropped".to_owned(),
                    },
                )?
                .map_err(|rejection| format!("cancelling {child} was refused: {rejection:?}"))?;
                Report::Cancelled("dropped".to_owned())
            }
            pick => {
                let worker = WORKERS[rng.pick(WORKERS.len())];
                let held = claim(host, worker, now, child)?
                    .ok_or_else(|| format!("{worker} could not claim {child}"))?;
                let (outcome, report) = if pick == 1 {
                    (
                        Outcome::Done("ok".to_owned()),
                        Report::Done("ok".to_owned()),
                    )
                } else {
                    (
                        Outcome::Failed("no".to_owned()),
                        Report::Failed("no".to_owned()),
                    )
                };
                host.append(
                    worker,
                    now,
                    Op::Finish {
                        task: child.clone(),
                        claim: held,
                        outcome,
                    },
                )?
                .map_err(|rejection| format!("finishing {child} was refused: {rejection:?}"))?;
                report
            }
        };
        if host.fold.report(&id, child) != Some(&expected) {
            return Err(format!(
                "{id} folded {:?} for {child}, not {expected:?}",
                host.fold.report(&id, child)
            ));
        }
        if reported(&host.fold, &id, child) != vec![expected] {
            return Err(format!(
                "{id} did not record exactly one report from {child}"
            ));
        }
        stats.reports += 1;
    }
    if host.fold.unreported(&id) != 0 || host.fold.reports(&id).count() != ids.len() {
        return Err(format!("{id} is missing reports after every child ended"));
    }
    host.append(
        PLANNER,
        now,
        Op::Finish {
            task: id.clone(),
            claim: goal_claim,
            outcome: Outcome::Done("planned".to_owned()),
        },
    )?
    .map_err(|rejection| format!("finishing goal {id} was refused: {rejection:?}"))?;
    if host.fold.goals().any(|open| open == &id) {
        return Err(format!("finished goal {id} is still open"));
    }
    Ok(())
}

async fn keyed(seed: u64, stats: &mut IntentStats) -> Result<(), String> {
    let shown = |error: BoardError| format!("intent keyed: {error}");
    let mut rng = Rng::derive(seed, "board intent keyed");
    let host: Arc<dyn LogHost> = Arc::new(MemoryLog::new());
    let mut lead = Board::open(host.clone(), "intent", super::access::identity(OWNER))
        .await
        .map_err(shown)?;
    lead.charter(0).await.map_err(shown)?;
    lead.submit_keyed(
        "grant",
        0,
        grant(
            Who::Kind("agent".to_owned()),
            &[Can::Read, Can::Post, Can::Label],
        ),
    )
    .await
    .map_err(shown)?;
    let task = goal(seed % 7);
    let mut writers = Vec::new();
    for number in 0..rng.range(2, 6) {
        let actor = [OWNER, PLANNER, WORKERS[rng.pick(WORKERS.len())]][rng.pick(3)];
        let board = Board::open(host.clone(), "intent", super::access::identity(actor))
            .await
            .map_err(shown)?;
        writers.push((number, board));
    }
    let mut seqs = Vec::new();
    for _ in 0..rng.range(2, 9) {
        let at = rng.pick(writers.len());
        if rng.chance(300_000) {
            let actor = writers[at].1.writer().to_owned();
            writers[at].1 = Board::open(host.clone(), "intent", super::access::identity(&actor))
                .await
                .map_err(shown)?;
        }
        let key = format!("post-{}", writers[at].0);
        let applied = writers[at]
            .1
            .submit_keyed(&key, 1, Op::Post { task: task.clone() })
            .await
            .map_err(shown)?;
        seqs.push(applied.seq);
        stats.retries += 1;
    }
    lead.refresh().await.map_err(shown)?;
    if seqs.iter().any(|seq| *seq != seqs[0]) {
        return Err(format!(
            "a keyed retry storm of {} answered rows {seqs:?}",
            task.id
        ));
    }
    if posted(lead.fold(), &task.id) != 1 {
        return Err(format!(
            "a keyed retry storm posted {} {} times",
            task.id,
            posted(lead.fold(), &task.id)
        ));
    }
    Ok(())
}

pub fn run(seed: u64) -> Result<IntentStats, String> {
    let mut rng = Rng::derive(seed, "board intent");
    let mut stats = IntentStats::default();
    let mut host = Host {
        log: Vec::new(),
        fold: Fold::new(),
        minted: 0,
    };
    let now = 1_700_000_000_000 + rng.range(0, 1_000_000);
    host.append(OWNER, now, Op::Charter {})?
        .map_err(|rejection| format!("the intent charter was refused: {rejection:?}"))?;
    for op in [
        grant(
            Who::Kind("agent".to_owned()),
            &[Can::Read, Can::Post, Can::Label],
        ),
        grant(
            Who::Principal(PLANNER.to_owned()),
            &[Can::Read, Can::Post, Can::Label, Can::Plan],
        ),
    ] {
        host.append(OWNER, now, op)?
            .map_err(|rejection| format!("an intent grant was refused: {rejection:?}"))?;
    }
    for number in 0..rng.range(1, 5) {
        plan(&mut host, &mut rng, now + number, number, &mut stats)?;
    }
    let refolded = host.refold()?;
    if refolded.entries() != host.fold.entries() {
        return Err("refolding the intent log changed its entries".to_owned());
    }
    for number in 0..stats.goals {
        let id = goal(number).id;
        if refolded.reports(&id).collect::<Vec<_>>() != host.fold.reports(&id).collect::<Vec<_>>() {
            return Err(format!("refolding changed the reports of {id}"));
        }
    }
    nanocode_sync::block_on(keyed(seed, &mut stats))?;
    Ok(stats)
}
