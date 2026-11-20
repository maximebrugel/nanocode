use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use nanocode_board::{
    Board, BoardError, ClaimId, Event, EventId, Fold, NewTask, Op, Outcome, Phase, TaskId,
    schedule::{Cron, Recur, Retry},
    stage::Stage,
    tasks::Terms,
    wake::{Napping, Timer, Wake},
};
use nanocode_turnlog::{LogHost, MemoryLog};

use crate::rng::Rng;

const WRITER: &str = "agent:due";
const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;
const LEASE: u64 = 5 * MINUTE;
// A backstop for a drain that spins in virtual time without the lapsed-lease
// check catching it; passing seeds have needed just over a day.
const DRAIN: u64 = 7 * 24 * HOUR;
const CRONS: [&str; 7] = [
    "*/5 * * * *",
    "*/30 8-18 * JAN-JUN,sep MON-FRI",
    "0 * * * *",
    "15,45 * * * *",
    "*/10 9-17 * * 1-5",
    "30 2 * * *",
    "7-59/20 */2 * * *",
];
const OFFSETS: [i16; 5] = [-480, 0, 60, 330, 840];
const NAMED: [(&str, &str); 5] = [
    ("0 9 * JAN-MAR MON-FRI", "0 9 * 1-3 1-5"),
    ("*/15 * * jun,Dec sun", "*/15 * * 6,12 0"),
    ("30 2 1 FEB/3 SAT,SUN", "30 2 1 2/3 6,0"),
    ("0 0 * * TUE-SAT/2", "0 0 * * 2-6/2"),
    ("0 0 * Nov-dec Sat", "0 0 * 11-12 6"),
];
const UNNAMED: [&str; 5] = [
    "0 0 * * FOO",
    "0 0 * JANUARY *",
    "0 0 * MON *",
    "0 0 * * DEC",
    "JAN 0 * * *",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct DueStats {
    pub runs: u64,
    pub retries: u64,
    pub restarts: u64,
}

struct Host {
    log: Vec<String>,
    fold: Fold,
    minted: u64,
}

impl Host {
    fn append(&mut self, at_ms: u64, op: Op) -> Result<bool, String> {
        self.minted += 1;
        let id = EventId(format!("{WRITER}.{}", self.minted));
        let event = Event {
            id: id.clone(),
            by: WRITER.to_owned(),
            at_ms,
            op,
        };
        let body = serde_json::to_string(&event).map_err(|error| error.to_string())?;
        self.log.push(body);
        let seq = self.log.len() as u64;
        self.fold
            .apply(seq, &self.log[self.log.len() - 1])
            .map_err(|error| format!("due fold: {error}"))?;
        Ok(matches!(self.fold.outcome(&id), Some(Ok(_))))
    }

    fn restart(&mut self) -> Result<(), String> {
        self.fold = Fold::new();
        for (index, body) in self.log.iter().enumerate() {
            self.fold
                .apply(index as u64 + 1, body)
                .map_err(|error| format!("due refold: {error}"))?;
        }
        self.minted = self.fold.minted(WRITER);
        Ok(())
    }
}

struct Expect {
    recur: Option<Recur>,
    retry: Option<Retry>,
    first_ms: u64,
}

#[derive(Default)]
struct Track {
    due_ms: u64,
    claims: u64,
    terminal: bool,
}

struct Oracle {
    series: BTreeMap<TaskId, Expect>,
    tasks: BTreeMap<TaskId, Track>,
    fired: BTreeMap<TaskId, Vec<u64>>,
    stats: DueStats,
}

impl Oracle {
    fn due(&self, fold: &Fold, id: &TaskId) -> Result<(TaskId, u64), String> {
        let task = fold
            .task(id)
            .ok_or_else(|| format!("{id} is ready but not on the board"))?;
        match &task.occurrence {
            Some(occurrence) => Ok((occurrence.series.clone(), occurrence.slot)),
            None => {
                let expect = self
                    .series
                    .get(id)
                    .ok_or_else(|| format!("{id} has no series"))?;
                Ok((id.clone(), expect.first_ms))
            }
        }
    }

    fn claimed(&mut self, fold: &Fold, id: &TaskId, now: u64) -> Result<(), String> {
        let (_, slot) = self.due(fold, id)?;
        let track = self.tasks.entry(id.clone()).or_insert_with(|| Track {
            due_ms: slot,
            ..Track::default()
        });
        if track.terminal {
            return Err(format!(
                "{id} was claimed at {now} after it ran out of attempts"
            ));
        }
        if now < track.due_ms {
            return Err(format!(
                "{id} was claimed at {now}, before it was due at {}",
                track.due_ms
            ));
        }
        track.claims += 1;
        Ok(())
    }

    fn finished(&mut self, fold: &Fold, id: &TaskId, now: u64, failed: bool) -> Result<(), String> {
        let (series, slot) = self.due(fold, id)?;
        let retry = self.series.get(&series).and_then(|expect| expect.retry);
        let track = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| format!("{id} finished without a claim"))?;
        let again = failed && retry.is_some_and(|retry| track.claims < retry.max_attempts);
        if again {
            let delay = retry
                .and_then(|retry| retry.backoff_ms.checked_mul(1 << (track.claims - 1)))
                .ok_or_else(|| format!("{id} backoff overflowed"))?;
            track.due_ms = now + delay;
            self.stats.retries += 1;
            return Ok(());
        }
        track.terminal = true;
        self.stats.runs += 1;
        let fired = self.fired.entry(series).or_default();
        if fired.contains(&slot) {
            return Err(format!("{id} fired due time {slot} again at {now}"));
        }
        fired.push(slot);
        Ok(())
    }
}

fn brute(cron: &str, offset: i16, after_ms: u64, next_ms: u64) -> Result<(), String> {
    let parsed = Cron::parse(cron)?;
    let offset = i64::from(offset);
    let minute = |ms: u64| ms as i64 / MINUTE as i64 + offset;
    if !next_ms.is_multiple_of(MINUTE) || next_ms <= after_ms || !parsed.matches(minute(next_ms)) {
        return Err(format!(
            "{cron} at {offset}: {next_ms} is not a match after {after_ms}"
        ));
    }
    match (minute(after_ms) + 1..minute(next_ms)).find(|at| parsed.matches(*at)) {
        Some(at) => Err(format!(
            "{cron} at {offset}: next after {after_ms} skipped minute {at}"
        )),
        None => Ok(()),
    }
}

fn expected(expect: &Expect, until_ms: u64) -> Result<Vec<u64>, String> {
    let mut slots = vec![expect.first_ms];
    if expect.first_ms > until_ms {
        return Ok(Vec::new());
    }
    let Some(recur) = &expect.recur else {
        return Ok(slots);
    };
    loop {
        let last = slots[slots.len() - 1];
        let next = recur
            .next_after(last)
            .map_err(|invalid| format!("{invalid:?}"))?;
        if let Recur::Cron {
            cron,
            offset_minutes,
        } = recur
            && next - last <= 2 * 24 * HOUR
        {
            brute(cron, *offset_minutes, last, next)?;
        }
        if next > until_ms {
            return Ok(slots);
        }
        slots.push(next);
    }
}

fn post(rng: &mut Rng, number: u64, now: u64) -> Result<(NewTask, Expect), String> {
    let recur = match rng.below(3) {
        0 => None,
        1 => Some(Recur::Every {
            every_ms: rng.range(2, 40) * MINUTE,
        }),
        _ => Some(Recur::Cron {
            cron: CRONS[rng.pick(CRONS.len())].to_owned(),
            offset_minutes: OFFSETS[rng.pick(OFFSETS.len())],
        }),
    };
    let first_ms = match &recur {
        Some(cron @ Recur::Cron { .. }) => cron
            .next_after(now)
            .map_err(|invalid| format!("{invalid:?}"))?,
        _ => now + rng.range(0, 3 * HOUR),
    };
    let retry = rng.chance(600_000).then(|| Retry {
        max_attempts: rng.range(1, 5),
        backoff_ms: rng.range(1, 10) * MINUTE,
    });
    let task = NewTask {
        id: TaskId(format!("due{number}")),
        parent: None,
        spec: "due".to_owned(),
        labels: BTreeMap::new(),
        needs: Vec::new(),
        terms: Terms {
            due_at_ms: Some(first_ms),
            recur: recur.clone(),
            retry,
            ..Terms::default()
        },
    };
    Ok((
        task,
        Expect {
            recur,
            retry,
            first_ms,
        },
    ))
}

fn step(
    host: &mut Host,
    oracle: &mut Oracle,
    rng: &mut Rng,
    now: u64,
    fail_ppm: u64,
    drop_ppm: u64,
) -> Result<bool, String> {
    let ready = host.fold.ready(now).cloned().collect::<Vec<_>>();
    for id in &ready {
        if !host.append(
            now,
            Op::Claim {
                task: id.clone(),
                lease_ms: LEASE,
                fence: None,
                cap: None,
            },
        )? {
            continue;
        }
        oracle.claimed(&host.fold, id, now)?;
        let Some(Phase::Claimed { claim, .. }) = host.fold.task(id).map(|task| task.phase.clone())
        else {
            return Err(format!("{id} was claimed but is not held"));
        };
        if rng.chance(drop_ppm) {
            continue;
        }
        let failed = rng.chance(fail_ppm);
        let outcome = if failed {
            Outcome::Failed("due run failed".to_owned())
        } else {
            Outcome::Done("due run done".to_owned())
        };
        let claim: ClaimId = claim;
        if host.append(
            now,
            Op::Finish {
                task: id.clone(),
                claim,
                outcome,
            },
        )? {
            oracle.finished(&host.fold, id, now, failed)?;
        }
    }
    Ok(!ready.is_empty())
}

fn names() -> Result<(), String> {
    for (named, numbered) in NAMED {
        if Cron::parse(named)? != Cron::parse(numbered)? {
            return Err(format!("{named:?} parses unlike {numbered:?}"));
        }
    }
    match UNNAMED.iter().find(|cron| Cron::parse(cron).is_ok()) {
        Some(cron) => Err(format!("{cron:?} parsed though a name is out of place")),
        None => Ok(()),
    }
}

struct Stepped(Cell<u64>);

impl Timer for Stepped {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }

    fn sleep_until(&self, at_ms: u64) -> Napping<'_> {
        self.0.set(self.0.get().max(at_ms));
        Box::pin(std::future::ready(()))
    }
}

struct Deaf<'t>(&'t Stepped);

impl Wake for Deaf<'_> {
    fn wait<'a>(&'a self, _scope: &'a str, _seen: u64, until_ms: u64) -> Napping<'a> {
        self.0.sleep_until(until_ms)
    }
}

struct Owed {
    due_ms: u64,
    retry: Option<Retry>,
    claims: u64,
}

async fn waits(seed: u64) -> Result<u64, String> {
    let shown = |error: BoardError| format!("due waiter: {error}");
    let mut rng = Rng::derive(seed, "board due wake");
    let start = 1_700_000_000_000 + rng.range(0, 400 * 24 * HOUR);
    let timer = Stepped(Cell::new(start));
    let wake = Deaf(&timer);
    let host: Arc<dyn LogHost> = Arc::new(MemoryLog::new());
    let mut board = Board::open(host, "due", super::access::identity(WRITER))
        .await
        .map_err(shown)?;
    board.charter(start).await.map_err(shown)?;
    let mut owed = BTreeMap::new();
    for number in 0..rng.range(1, 6) {
        let due_ms = start + rng.range(1, 3 * HOUR);
        let mut terms = Terms::due(due_ms);
        if rng.chance(500_000) {
            terms = terms.retry(rng.range(2, 5), rng.range(1, 10) * MINUTE);
        }
        let task = NewTask {
            id: TaskId(format!("wake{number}")),
            parent: None,
            spec: "wake".to_owned(),
            labels: BTreeMap::new(),
            needs: Vec::new(),
            terms: Terms::default(),
        };
        let retry = terms.retry;
        board
            .post_due(start, task.clone(), terms)
            .await
            .map_err(shown)?;
        owed.insert(
            task.id,
            Owed {
                due_ms,
                retry,
                claims: 0,
            },
        );
    }
    let mut wakes = 0;
    while let Some(want) = owed.values().map(|owed| owed.due_ms).min() {
        let until = timer.now_ms() + 4 * HOUR;
        let picked = board
            .next_ready(&wake, &timer, &BTreeMap::new(), until)
            .await
            .map_err(shown)?;
        let now = timer.now_ms();
        let Some(id) = picked else {
            return Err(format!(
                "a waiter for a task due at {want} woke at {now} with none ready"
            ));
        };
        if now != want {
            return Err(format!("a waiter for a task due at {want} woke at {now}"));
        }
        let task = owed
            .get_mut(&id)
            .ok_or_else(|| format!("the waiter picked {id}, which owes nothing"))?;
        if task.due_ms > now {
            return Err(format!(
                "the waiter picked {id} at {now}, before it was due at {}",
                task.due_ms
            ));
        }
        wakes += 1;
        board
            .submit(
                now,
                Op::Claim {
                    task: id.clone(),
                    lease_ms: LEASE,
                    fence: None,
                    cap: None,
                },
            )
            .await
            .map_err(shown)?;
        let Some(Phase::Claimed { claim, .. }) =
            board.fold().task(&id).map(|task| task.phase.clone())
        else {
            return Err(format!("the waiter claimed {id} but does not hold it"));
        };
        task.claims += 1;
        let again = task
            .retry
            .filter(|retry| task.claims < retry.max_attempts && rng.chance(600_000));
        let outcome = match again {
            Some(_) => Outcome::Failed("due run failed".to_owned()),
            None => Outcome::Done("due run done".to_owned()),
        };
        let finish = Op::Finish {
            task: id.clone(),
            claim,
            outcome,
        };
        board.submit(now, finish).await.map_err(shown)?;
        match again {
            Some(retry) => {
                task.due_ms = now + retry.delay(task.claims).ok_or("a retry delay overflowed")?
            }
            None => {
                owed.remove(&id);
            }
        }
    }
    Ok(wakes)
}

pub fn run(seed: u64) -> Result<DueStats, String> {
    names()?;
    nanocode_sync::block_on(waits(seed))?;
    let mut rng = Rng::derive(seed, "board due");
    let start = 1_700_000_000_000 + rng.range(0, 400 * 24 * HOUR);
    let horizon = start + rng.range(2, 9) * HOUR;
    let fail_ppm = rng.range(0, 400_000);
    let drop_ppm = rng.range(0, 100_000);
    let crash_ppm = rng.range(0, 40_000);
    let mut host = Host {
        log: Vec::new(),
        fold: Fold::new(),
        minted: 0,
    };
    let mut oracle = Oracle {
        series: BTreeMap::new(),
        tasks: BTreeMap::new(),
        fired: BTreeMap::new(),
        stats: DueStats::default(),
    };
    let mut now = start;
    if !host.append(now, Op::Charter {})? {
        return Err("due board charter was refused".to_owned());
    }
    for number in 0..rng.range(2, 7) {
        let (task, expect) = post(&mut rng, number, now)?;
        let id = task.id.clone();
        if !host.append(now, Op::Post { task })? {
            return Err(format!("posting {id} was refused"));
        }
        oracle.series.insert(id, expect);
    }
    while now < horizon {
        now += rng.range(1, 3 * MINUTE);
        if rng.chance(crash_ppm) {
            host.restart()?;
            oracle.stats.restarts += 1;
            now += rng.range(0, 90 * MINUTE);
        }
        step(&mut host, &mut oracle, &mut rng, now, fail_ppm, drop_ppm)?;
    }
    host.restart()?;
    let cut = now;
    now += LEASE;
    let mut rounds = 0;
    loop {
        if step(&mut host, &mut oracle, &mut rng, now, fail_ppm, 0)? {
            rounds += 1;
            let mut owed = oracle.series.len();
            for expect in oracle.series.values() {
                let tries = expect.retry.map_or(1, |retry| retry.max_attempts as usize);
                owed += expected(expect, now)?.len() * tries;
            }
            if rounds > owed {
                return Err(format!("due tasks never drained at {now}"));
            }
            continue;
        }
        let mut owed = false;
        for (id, task) in host.fold.tasks() {
            let open = !task.finished() && oracle.due(&host.fold, id)?.1 <= cut;
            if open && task.stage(now) == Stage::Lapsed && host.fold.claimable(id, now).is_err() {
                return Err(format!(
                    "{id}'s lease lapsed but it cannot be claimed at {now}"
                ));
            }
            owed |= open;
        }
        match host.fold.next_due() {
            Some(due_ms) if owed && due_ms > cut + DRAIN => {
                return Err(format!(
                    "due tasks owed before {cut} were still open at {due_ms}"
                ));
            }
            Some(due_ms) if owed => now = due_ms,
            _ if owed => {
                return Err(format!(
                    "due tasks owed before {cut} have no due time at {now}"
                ));
            }
            _ => break,
        }
    }
    for (id, task) in host.fold.tasks() {
        let claims = oracle.tasks.get(id).map_or(0, |track| track.claims);
        if task.attempts != claims {
            return Err(format!(
                "{id} counts {} attempts after {claims} claims",
                task.attempts
            ));
        }
    }
    for (series, expect) in &oracle.series {
        let slots = expected(expect, cut)?;
        let later = expected(expect, now)?;
        let fired = oracle.fired.get(series).cloned().unwrap_or_default();
        let unique = fired.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != fired.len() {
            return Err(format!("{series} fired a due time twice: {fired:?}"));
        }
        if let Some(missed) = slots.iter().find(|slot| !unique.contains(slot)) {
            let open = host
                .fold
                .tasks()
                .iter()
                .filter(|(_, task)| !task.finished())
                .map(|(id, task)| {
                    format!(
                        "{id} {:?} due={:?} {:?}",
                        task.phase,
                        task.due_at_ms,
                        host.fold.claimable(id, now)
                    )
                })
                .collect::<Vec<_>>();
            return Err(format!(
                "{series} never fired its due time {missed} (fired {} of {}) now={now} open={open:?}",
                fired.len(),
                slots.len()
            ));
        }
        if let Some(extra) = fired.iter().find(|slot| !later.contains(slot)) {
            return Err(format!(
                "{series} fired at {extra}, which is not one of its due times"
            ));
        }
    }
    Ok(oracle.stats)
}
