use std::{
    collections::BTreeMap,
    ops::Range,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use nanocode::{
    backend::Prompt,
    session_host::Submission,
    turnlog::{
        Extension, Fire, LogHost, OneLog, Policy, Rider, Statements, Trigger, Wake, WakeError,
        WakeRider, WakeSummary, Write,
        wake_sim::{self, Mutation},
    },
};
use nanocode_durability_core::{StoreError, StoreFuture};
use nanocode_sync::{Atom, FutureExt};
use serde_json::{Map, Value, json, value::to_raw_value};

use crate::{
    onelog::RESTART_CAP,
    rng::{FNV_OFFSET, Rng, fnv},
};

pub use wake_sim::Mutation as WakesMutation;

const SCOPE: &str = "wakes";
const KEYS: [&str; 4] = ["check-a", "check-b", "check-c", "check-d"];
const TOPICS: [&str; 2] = ["ci", "pull_request"];
const REPOSITORIES: [&str; 3] = ["one", "two", "three"];
const OPS_MIN: u64 = 40;
const OPS_MAX: u64 = 120;
const STEP_MS_MAX: u64 = 90_000;
const DELAY_MS_MAX: u64 = 600_000;
const HEARD_MAX: u64 = 3;
const SNAPSHOT_ROWS_MIN: u64 = 2;
const SNAPSHOT_ROWS_MAX: u64 = 6;
const DOOM_APPENDS_MAX: u64 = 3;
const WAKES_MAX_MIN: u64 = 2;
const INDEX_HOST: usize = 1;
const INDEX_STATEMENT: &str = "wake_index";

pub fn names() -> String {
    WakesMutation::ALL
        .iter()
        .map(|mutation| mutation.name())
        .collect::<Vec<_>>()
        .join("|")
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WakesStats {
    pub seeds: u64,
    pub armed: u64,
    pub disarmed: u64,
    pub heard: u64,
    pub full: u64,
    pub fired: u64,
    pub rearmed: u64,
    pub crashes: u64,
    pub restarts: u64,
    pub refused_full: u64,
    pub indexed: u64,
}

impl WakesStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.armed += other.armed;
        self.disarmed += other.disarmed;
        self.heard += other.heard;
        self.full += other.full;
        self.fired += other.fired;
        self.rearmed += other.rearmed;
        self.crashes += other.crashes;
        self.restarts += other.restarts;
        self.refused_full += other.refused_full;
        self.indexed += other.indexed;
    }
}

#[derive(Default)]
struct Host {
    doom: AtomicU64,
    dead: AtomicBool,
    index: Atom<BTreeMap<String, WakeSummary>>,
    rows: Atom<BTreeMap<u64, String>>,
}

impl Host {
    fn refuses(&self) -> bool {
        if self.dead.load(Ordering::SeqCst) {
            return true;
        }
        let doom = self.doom.load(Ordering::SeqCst);
        if doom == 0 {
            return false;
        }
        self.doom.store(doom - 1, Ordering::SeqCst);
        if doom == 1 {
            self.dead.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }

    fn revive(&self) {
        self.doom.store(0, Ordering::SeqCst);
        self.dead.store(false, Ordering::SeqCst);
    }

    fn died() -> StoreError {
        StoreError::Backend("the simulated host died before this row landed".to_owned())
    }

    fn appended(&self, rows: &[(u64, String)]) -> bool {
        self.rows.update(|stored| {
            if rows.iter().any(|(seq, _)| stored.contains_key(seq)) {
                return true;
            }
            stored.extend(rows.iter().cloned());
            false
        })
    }

    fn extended(&self, extension: Extension<'_>) -> bool {
        self.rows.update(|stored| {
            if stored.contains_key(&extension.seq) {
                return true;
            }
            let Some(body) = stored.remove(&extension.row) else {
                return true;
            };
            let kept = body.len().checked_sub(extension.cut).unwrap_or_else(|| {
                panic!("turn log row {} is shorter than its seal", extension.row)
            });
            stored.insert(
                extension.seq,
                format!("{}{}", &body[..kept], extension.tail),
            );
            false
        })
    }

    fn summary(due: &str, topics: &str) -> WakeSummary {
        WakeSummary {
            next_due: (!due.is_empty()).then(|| due.parse().expect("a due time is a number")),
            topics: topics
                .split(',')
                .filter(|topic| !topic.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    fn indexed(&self, statements: &[nanocode_durability_core::SqlStatement<'_>]) {
        for statement in statements {
            assert_eq!(statement.sql, INDEX_STATEMENT, "only the wake index rides");
            let [scope, due, topics] = statement.params[..] else {
                panic!("a wake index statement has a scope, a due time and topics");
            };
            let summary = Self::summary(due, topics);
            self.index.update(|index| {
                index.insert(scope.to_owned(), summary.clone());
            });
        }
    }
}

struct Index(Arc<Host>);

impl WakeRider for Index {
    fn host(&self) -> usize {
        INDEX_HOST
    }

    fn statements(&self, scope: &str, summary: &WakeSummary) -> Statements {
        let due = summary.next_due.map(|at| at.to_string()).unwrap_or_default();
        vec![(
            INDEX_STATEMENT,
            vec![scope.to_owned(), due, summary.topics.join(",")],
        )]
    }

    fn prepare(&self) -> StoreFuture<'_, Result<(), StoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn write<'a>(
        &'a self,
        scope: &'a str,
        summary: &'a WakeSummary,
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            if self.0.refuses() {
                return Err(Host::died());
            }
            self.0.index.update(|index| {
                index.insert(scope.to_owned(), summary.clone());
            });
            Ok(())
        })
    }
}

impl LogHost for Host {
    fn scan<'a>(
        &'a self,
        _scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        let rows = self.rows.read(|rows| {
            rows.range(from..to)
                .map(|(seq, body)| (*seq, body.clone()))
                .collect()
        });
        Box::pin(async move { Ok(rows) })
    }

    fn load<'a>(
        &'a self,
        _scope: &'a str,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        let rows = self.rows.read(|rows| {
            let start = rows
                .iter()
                .rev()
                .find(|(_, body)| body.starts_with('s'))
                .map_or(0, |(seq, _)| *seq);
            rows.range(start..)
                .map(|(seq, body)| (*seq, body.clone()))
                .collect()
        });
        Box::pin(async move { Ok(rows) })
    }

    fn append<'a>(
        &'a self,
        _scope: &'a str,
        rows: &'a [(u64, String)],
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        if self.refuses() {
            return Box::pin(async { Err(Self::died()) });
        }
        let taken = self.appended(rows);
        Box::pin(async move {
            if taken {
                Err(StoreError::Fenced)
            } else {
                Ok(())
            }
        })
    }

    fn extend<'a>(
        &'a self,
        _scope: &'a str,
        extension: Extension<'a>,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        if self.refuses() {
            return Some(Box::pin(async { Err(Self::died()) }));
        }
        let taken = self.extended(extension);
        Some(Box::pin(async move {
            if taken {
                Err(StoreError::Fenced)
            } else {
                Ok(())
            }
        }))
    }

    fn carry<'a>(
        &'a self,
        _scope: &'a str,
        write: Write<'a>,
        rider: Rider<'a>,
    ) -> Option<StoreFuture<'a, Result<(), StoreError>>> {
        if rider.host != INDEX_HOST {
            return None;
        }
        if self.refuses() {
            return Some(Box::pin(async { Err(Self::died()) }));
        }
        let taken = match write {
            Write::Append { rows, .. } => self.appended(rows),
            Write::Extend(extension) => self.extended(extension),
        };
        if !taken {
            self.indexed(rider.statements);
        }
        Some(Box::pin(async move {
            if taken {
                Err(StoreError::Fenced)
            } else {
                Ok(())
            }
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Expected {
    due_at: Option<u64>,
    generation: u64,
    heard: u64,
    heard_max: u64,
    on: Option<(String, String)>,
}

impl Expected {
    const fn due(&self, now: u64) -> bool {
        self.heard > 0
            || match self.due_at {
                Some(at) => at <= now,
                None => false,
            }
    }
}

fn ready<T>(future: impl Future<Output = T>) -> T {
    future
        .now_or_never()
        .expect("the simulated wake host answers every call at once")
}

struct Run {
    attempts: u64,
    clock: u64,
    digest: u64,
    expected: BTreeMap<String, Expected>,
    fired: BTreeMap<u64, u64>,
    host: Arc<Host>,
    log: Option<Arc<OneLog>>,
    policy: Policy,
    rng: Rng,
    stats: WakesStats,
    verbose: bool,
}

impl Run {
    fn trace(&mut self, line: &str) {
        self.digest = fnv(fnv(self.digest, line.as_bytes()), b"\n");
        if self.verbose {
            println!("    wakes {line}");
        }
    }

    fn open(&mut self) -> Result<Arc<OneLog>, String> {
        if let Some(log) = &self.log {
            return Ok(Arc::clone(log));
        }
        self.host.revive();
        let host: Arc<dyn LogHost> = Arc::clone(&self.host) as Arc<dyn LogHost>;
        let log = ready(OneLog::open(host, SCOPE, self.policy.clone()))
            .map_err(|error| format!("reopen refused the log: {error}"))?;
        self.log = Some(Arc::clone(&log));
        self.recovered(&log)?;
        self.indexed()?;
        ready(log.index_wakes(Arc::new(Index(Arc::clone(&self.host)))))
            .map_err(|error| format!("indexing the wakes refused: {error}"))?;
        self.indexed()?;
        Ok(log)
    }

    fn crash(&mut self, why: &str) -> Result<(), String> {
        if !self.host.dead.load(Ordering::SeqCst) {
            return Err(format!("{why}, on a live host"));
        }
        self.trace(&format!("crash {why}"));
        self.stats.crashes += 1;
        self.log = None;
        Ok(())
    }

    fn recovered(&mut self, log: &OneLog) -> Result<(), String> {
        let (wakes, turns) = ready(log.read(|fold| {
            let wakes = fold
                .wakes()
                .iter()
                .map(|(key, armed)| (key.to_owned(), (armed.generation, armed.heard.len() as u64)))
                .collect::<BTreeMap<_, _>>();
            let turns = fold
                .turns()
                .iter()
                .map(|turn| (turn.id.clone(), turn.metadata.get().to_owned()))
                .collect::<Vec<_>>();
            (wakes, turns)
        }));
        let expected = self
            .expected
            .iter()
            .map(|(key, wake)| (key.clone(), (wake.generation, wake.heard)))
            .collect::<BTreeMap<_, _>>();
        if wakes != expected {
            return Err(format!(
                "recovered wakes {wakes:?} differ from the acknowledged {expected:?}"
            ));
        }
        let mut admitted: BTreeMap<u64, u64> = BTreeMap::new();
        for (id, metadata) in turns {
            let generation = serde_json::from_str::<Value>(&metadata)
                .ok()
                .and_then(|origin| origin["generation"].as_u64())
                .ok_or_else(|| format!("turn {id} has no wake origin"))?;
            *admitted.entry(generation).or_default() += 1;
        }
        for (generation, count) in &admitted {
            if *count > 1 {
                return Err(format!("wake generation {generation} fired {count} turns"));
            }
            if !self.fired.contains_key(generation) {
                return Err(format!(
                    "wake generation {generation} admitted a turn its fire did not acknowledge"
                ));
            }
        }
        for generation in self.fired.keys() {
            if !admitted.contains_key(generation) {
                return Err(format!(
                    "wake generation {generation} acknowledged a fire the log lost"
                ));
            }
        }
        self.trace(&format!(
            "recovered wakes={} turns={}",
            wakes.len(),
            admitted.len()
        ));
        Ok(())
    }

    fn summary(&self) -> WakeSummary {
        let mut topics = self
            .expected
            .values()
            .filter(|wake| wake.heard < wake.heard_max)
            .filter_map(|wake| wake.on.as_ref().map(|(topic, _)| topic.clone()))
            .collect::<Vec<_>>();
        topics.sort();
        topics.dedup();
        WakeSummary {
            next_due: self
                .expected
                .values()
                .filter_map(|wake| if wake.heard > 0 { Some(0) } else { wake.due_at })
                .min(),
            topics,
        }
    }

    fn indexed(&mut self) -> Result<(), String> {
        let held = self
            .host
            .index
            .read(|index| index.get(SCOPE).cloned())
            .unwrap_or_default();
        let expected = self.summary();
        if held != expected {
            return Err(format!(
                "the wake index holds {held:?}, the acknowledged wakes need {expected:?}"
            ));
        }
        self.stats.indexed += 1;
        Ok(())
    }

    fn wake(&mut self) -> (Wake, Expected) {
        let due_at = (self.rng.below(3) > 0).then(|| self.clock + self.rng.below(DELAY_MS_MAX));
        let on = (due_at.is_none() || self.rng.below(2) == 0).then(|| {
            (
                TOPICS[self.rng.pick(TOPICS.len())].to_owned(),
                REPOSITORIES[self.rng.pick(REPOSITORIES.len())].to_owned(),
            )
        });
        let heard_max = self.rng.range(1, HEARD_MAX);
        let wake = Wake {
            due_at,
            on: on.as_ref().map(|(topic, repository)| Trigger {
                filter: BTreeMap::from([("repository".to_owned(), vec![json!(repository)])]),
                heard_max,
                topic: topic.clone(),
            }),
            payload: to_raw_value(&json!({"note": "check"})).expect("a note encodes as JSON"),
        };
        let expected = Expected {
            due_at,
            generation: 0,
            heard: 0,
            heard_max,
            on,
        };
        (wake, expected)
    }

    fn arm(&mut self) -> Result<(), String> {
        let log = self.open()?;
        let key = KEYS[self.rng.pick(KEYS.len())];
        let (wake, mut expected) = self.wake();
        let full = self.policy.wakes_max.is_some_and(|max| {
            !self.expected.contains_key(key) && self.expected.len() >= max
        });
        match ready(log.arm(key, wake)) {
            Ok(generation) if full => Err(format!(
                "arm {key} took generation {generation} past the bound of {:?} wakes",
                self.policy.wakes_max
            )),
            Err(WakeError::Full { max }) if full => {
                self.stats.refused_full += 1;
                self.trace(&format!("arm {key} full at {max}"));
                Ok(())
            }
            Err(WakeError::Full { max }) => Err(format!(
                "arm {key} refused as full at {max} with {} wakes",
                self.expected.len()
            )),
            Ok(generation) => {
                expected.generation = generation;
                expected.heard = self.expected.get(key).map_or(0, |prior| prior.heard);
                self.expected.insert(key.to_owned(), expected);
                self.stats.armed += 1;
                self.trace(&format!("arm {key} generation={generation}"));
                self.indexed()
            }
            Err(WakeError::Store(error)) => self.crash(&format!("arm {key}: {error}")),
            Err(WakeError::Unfit(unfit)) => Err(format!("a fit wake was refused: {unfit}")),
        }
    }

    fn disarm(&mut self) -> Result<(), String> {
        let log = self.open()?;
        let key = KEYS[self.rng.pick(KEYS.len())];
        match ready(log.disarm(key)) {
            Ok(generation) => {
                let held = self.expected.remove(key).map(|wake| wake.generation);
                if held != generation {
                    return Err(format!(
                        "disarm {key} found generation {generation:?}, acknowledged {held:?}"
                    ));
                }
                self.stats.disarmed += u64::from(generation.is_some());
                self.trace(&format!("disarm {key} generation={generation:?}"));
                self.indexed()
            }
            Err(error) => self.crash(&format!("disarm {key}: {error}")),
        }
    }

    fn hear(&mut self) -> Result<(), String> {
        let log = self.open()?;
        let topic = TOPICS[self.rng.pick(TOPICS.len())];
        let repository = REPOSITORIES[self.rng.pick(REPOSITORIES.len())];
        let mut fields = Map::new();
        fields.insert("repository".to_owned(), json!(repository));
        let event = to_raw_value(&json!({"topic": topic, "repository": repository}))
            .expect("an event encodes as JSON");
        let matching = |wake: &Expected| {
            wake.on
                .as_ref()
                .is_some_and(|(on, wanted)| on == topic && wanted == repository)
        };
        let (mut heard, mut full) = (Vec::new(), Vec::new());
        for (key, wake) in &self.expected {
            if !matching(wake) {
                continue;
            }
            let found = (key.clone(), wake.generation);
            if wake.heard >= wake.heard_max {
                full.push(found);
            } else {
                heard.push(found);
            }
        }
        match ready(log.hear(topic, &fields, event)) {
            Ok(outcome) => {
                if outcome.heard != heard || outcome.full != full {
                    return Err(format!(
                        "{topic}/{repository} heard {:?} full {:?}, expected {heard:?} full {full:?}",
                        outcome.heard, outcome.full
                    ));
                }
                for (key, _) in &heard {
                    if let Some(wake) = self.expected.get_mut(key) {
                        wake.heard += 1;
                    }
                }
                self.stats.heard += heard.len() as u64;
                self.stats.full += full.len() as u64;
                self.trace(&format!(
                    "hear {topic}/{repository} heard={} full={}",
                    heard.len(),
                    full.len()
                ));
                self.indexed()
            }
            Err(error) => self.crash(&format!("hear {topic}: {error}")),
        }
    }

    fn fire_due(&mut self) -> Result<(), String> {
        let log = self.open()?;
        let due = ready(log.due_wakes(self.clock));
        let expected = self
            .expected
            .iter()
            .filter(|(_, wake)| wake.due(self.clock))
            .map(|(key, wake)| (key.clone(), wake.generation))
            .collect::<Vec<_>>();
        let found = due
            .iter()
            .map(|(key, armed)| (key.clone(), armed.generation))
            .collect::<Vec<_>>();
        if found != expected {
            return Err(format!(
                "due wakes {found:?} differ from the acknowledged {expected:?}"
            ));
        }
        for (key, armed) in due {
            let heard = armed.heard.len() as u64;
            self.attempts += 1;
            let submission = Submission {
                request_id: format!("fire-{}", self.attempts),
                prompt: Prompt::new(format!("{key} woke with {heard} events")),
                hand: None,
                metadata: to_raw_value(&json!({"generation": armed.generation, "heard": heard}))
                    .expect("an origin encodes as JSON"),
            };
            let repeat = self.rng.below(3) == 0;
            let next = repeat.then(|| {
                let (mut wake, _) = self.wake();
                wake.due_at = Some(self.clock + self.rng.range(1, DELAY_MS_MAX));
                wake
            });
            let next_expected = next.as_ref().map(|wake| Expected {
                due_at: wake.due_at,
                generation: 0,
                heard: 0,
                heard_max: wake.on.as_ref().map_or(0, |on| on.heard_max),
                on: wake.on.as_ref().map(|on| {
                    let repository = on.filter["repository"][0]
                        .as_str()
                        .expect("a repository filter is a string")
                        .to_owned();
                    (on.topic.clone(), repository)
                }),
            });
            match ready(log.fire(&key, armed.generation, self.clock, &submission, next)) {
                Ok(Fire::Fired { next, .. }) => {
                    if heard != self.expected.get(&key).map_or(0, |wake| wake.heard) {
                        return Err(format!(
                            "{key} fired with {heard} events, acknowledged otherwise"
                        ));
                    }
                    *self.fired.entry(armed.generation).or_default() += 1;
                    self.expected.remove(&key);
                    if let (Some(generation), Some(mut wake)) = (next, next_expected) {
                        wake.generation = generation;
                        self.expected.insert(key.clone(), wake);
                        self.stats.rearmed += 1;
                    }
                    self.stats.fired += 1;
                    self.trace(&format!(
                        "fire {key} generation={} heard={heard} next={next:?}",
                        armed.generation
                    ));
                    self.indexed()?;
                }
                Ok(Fire::NotDue | Fire::Stale | Fire::Taken(_)) => {
                    return Err(format!("due wake {key} did not fire"));
                }
                Err(WakeError::Full { max }) => {
                    return Err(format!("a rearm of {key} was refused as full at {max}"));
                }
                Err(WakeError::Store(error)) => {
                    return self.crash(&format!("fire {key}: {error}"));
                }
                Err(WakeError::Unfit(unfit)) => {
                    return Err(format!("a fit rearm was refused: {unfit}"));
                }
            }
        }
        Ok(())
    }

    fn step(&mut self) -> Result<(), String> {
        match self.rng.below(10) {
            0..=2 => self.arm(),
            3 => self.disarm(),
            4 | 5 => self.hear(),
            6 => {
                self.clock += self.rng.below(STEP_MS_MAX);
                self.fire_due()
            }
            7 => self.fire_due(),
            8 => {
                let appends = self.rng.range(1, DOOM_APPENDS_MAX);
                self.host.doom.store(appends, Ordering::SeqCst);
                self.trace(&format!("doom after {appends} appends"));
                Ok(())
            }
            _ => {
                self.stats.restarts += 1;
                self.trace("restart");
                self.log = None;
                self.open().map(drop)
            }
        }
    }

    fn drain(&mut self) -> Result<(), String> {
        self.host.revive();
        self.log = None;
        self.open()?;
        let mut rounds = 0;
        while let Some(at) = self
            .expected
            .values()
            .filter_map(|wake| if wake.heard > 0 { Some(0) } else { wake.due_at })
            .min()
        {
            rounds += 1;
            if rounds > OPS_MAX {
                return Err(format!("wakes still due after {OPS_MAX} drain rounds"));
            }
            self.clock = self.clock.max(at);
            self.fire_due()?;
            if self.log.is_none() {
                return Err("the drain crashed on a live host".to_owned());
            }
        }
        self.log = None;
        self.open()?;
        Ok(())
    }
}

fn run(seed: u64, verbose: bool, stats: &mut WakesStats) -> Result<u64, String> {
    crate::host::enter(seed, &crate::executor::Clock::new());
    let mut rng = Rng::derive(seed, "wakes");
    let snapshot_rows = rng.range(SNAPSHOT_ROWS_MIN, SNAPSHOT_ROWS_MAX);
    let wakes_max = (rng.below(2) == 0).then(|| {
        usize::try_from(rng.range(WAKES_MAX_MIN, KEYS.len() as u64)).expect("a bound fits")
    });
    let ops = rng.range(OPS_MIN, OPS_MAX);
    let mut state = Run {
        attempts: 0,
        clock: 0,
        digest: FNV_OFFSET,
        expected: BTreeMap::new(),
        fired: BTreeMap::new(),
        host: Arc::new(Host::default()),
        log: None,
        policy: Policy {
            restarts: RESTART_CAP,
            snapshot_ratio: 2,
            snapshot_rows: Some(usize::try_from(snapshot_rows).expect("a row count fits")),
            wakes_max,
            ..Policy::default()
        },
        rng,
        stats: WakesStats {
            seeds: 1,
            ..WakesStats::default()
        },
        verbose,
    };
    state.trace(&format!(
        "seed {seed} ops={ops} snapshot_rows={snapshot_rows} wakes_max={wakes_max:?}"
    ));
    let mut result = Ok(());
    for _ in 0..ops {
        result = state.step();
        if result.is_err() {
            break;
        }
    }
    if result.is_ok() {
        result = state.drain();
    }
    *stats = state.stats;
    result.map(|()| state.digest)
}

pub struct WakesOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: WakesStats,
}

pub fn run_seed(seed: u64, mutation: WakesMutation, verbose: bool) -> WakesOutcome {
    wake_sim::mutate(mutation);
    let outcome = std::panic::catch_unwind(|| {
        let mut stats = WakesStats::default();
        let result = run(seed, verbose, &mut stats);
        (result, stats)
    });
    wake_sim::mutate(Mutation::None);
    match outcome {
        Ok((Ok(digest), stats)) => WakesOutcome {
            digest,
            wrong: None,
            stats,
        },
        Ok((Err(message), stats)) => WakesOutcome {
            digest: 0,
            wrong: Some(message),
            stats,
        },
        Err(panic) => WakesOutcome {
            digest: 0,
            wrong: Some(format!(
                "panic: {}",
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                    .unwrap_or_default()
            )),
            stats: WakesStats::default(),
        },
    }
}

pub fn judge(
    seed: u64,
    mutation: WakesMutation,
    twice: bool,
    verbose: bool,
    totals: &mut WakesStats,
) -> Option<String> {
    let first = run_seed(seed, mutation, verbose);
    totals.add(&first.stats);
    if verbose {
        println!(
            "seed {seed} digest {:016x} stats={:?}",
            first.digest, first.stats
        );
    }
    if first.wrong.is_some() || !twice {
        return first.wrong;
    }
    let second = run_seed(seed, mutation, false);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "nondeterminism: digest {:016x} then {:016x}",
            first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: WakesMutation) -> String {
    let mut command = format!("nanocode-sim --scenario wakes --seed {seed}");
    if mutation != WakesMutation::None {
        command.push_str(" --mutation ");
        command.push_str(mutation.name());
    }
    command
}

pub fn report(
    seeds: &Range<u64>,
    passed: u64,
    failed: u64,
    twice: bool,
    mutation: WakesMutation,
    totals: &WakesStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=wakes mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        WakesMutation::None if failed == 0 => ExitCode::SUCCESS,
        WakesMutation::None => ExitCode::FAILURE,
        _ if failed > 0 => {
            println!("MUTATION {} caught by {failed} seeds", mutation.name());
            ExitCode::SUCCESS
        }
        _ => {
            println!("MUTATION {} NOT caught", mutation.name());
            ExitCode::FAILURE
        }
    }
}
