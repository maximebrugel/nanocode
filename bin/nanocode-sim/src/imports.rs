use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use nanocode::turnlog::{
    Extension, Imported, LogHost, OneLog, Policy,
    import_sim::{self, Mutation},
};
use nanocode_durability_core::{OwnerId, StateStore, StoreError, StoreFuture, StoreRecord};
use nanocode_sync::{Atom, FutureExt};

use crate::{
    onelog::RESTART_CAP,
    rng::{FNV_OFFSET, Rng, fnv},
};

pub use import_sim::Mutation as ImportsMutation;

const SCOPE: &str = "imports";
const STATES: [&str; 5] = ["root", "root:child-a", "root:child-b", "other", "other:child"];
const OPS_MIN: u64 = 30;
const OPS_MAX: u64 = 90;
const REVISION_MAX: u64 = 400;
const RECORDS_MAX: u64 = 4;
const PAYLOAD_PAD_MAX: u64 = 600;
const SNAPSHOT_ROWS_MIN: u64 = 2;
const SNAPSHOT_ROWS_MAX: u64 = 6;
const DOOM_APPENDS_MAX: u64 = 3;

pub fn names() -> String {
    ImportsMutation::ALL
        .iter()
        .map(|mutation| mutation.name())
        .collect::<Vec<_>>()
        .join("|")
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ImportsStats {
    pub seeds: u64,
    pub copied: u64,
    pub held: u64,
    pub conflicts: u64,
    pub fenced: u64,
    pub blips: u64,
    pub crashes: u64,
    pub sessions: u64,
}

impl ImportsStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.copied += other.copied;
        self.held += other.held;
        self.conflicts += other.conflicts;
        self.fenced += other.fenced;
        self.blips += other.blips;
        self.crashes += other.crashes;
        self.sessions += other.sessions;
    }
}

#[derive(Default)]
struct Host {
    blip: AtomicBool,
    doom: AtomicU64,
    dead: AtomicBool,
    rows: Atom<BTreeMap<u64, String>>,
}

impl Host {
    fn refuses(&self) -> bool {
        if self.dead.load(Ordering::SeqCst) {
            return true;
        }
        if self.blip.swap(false, Ordering::SeqCst) {
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
        self.blip.store(false, Ordering::SeqCst);
        self.doom.store(0, Ordering::SeqCst);
        self.dead.store(false, Ordering::SeqCst);
    }

    fn copy(&self) -> Self {
        Self {
            rows: Atom::new(self.rows.read(Clone::clone)),
            ..Self::default()
        }
    }

    fn refused() -> StoreError {
        StoreError::Backend("the simulated host refused this row".to_owned())
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
            return Box::pin(async { Err(Self::refused()) });
        }
        let taken = self.rows.update(|stored| {
            if rows.iter().any(|(seq, _)| stored.contains_key(seq)) {
                return true;
            }
            stored.extend(rows.iter().cloned());
            false
        });
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
            return Some(Box::pin(async { Err(Self::refused()) }));
        }
        let taken = self.rows.update(|stored| {
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
        });
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
struct State {
    revision: u64,
    payload: String,
    records: BTreeMap<String, String>,
}

impl State {
    fn records(&self) -> Vec<StoreRecord> {
        self.records
            .iter()
            .map(|(key, value)| StoreRecord {
                key: key.clone(),
                value: value.as_str().into(),
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kept {
    Imported,
    Advanced,
}

fn ready<T>(future: impl Future<Output = T>) -> T {
    future
        .now_or_never()
        .expect("the simulated import host answers every call at once")
}

fn read(log: &Arc<OneLog>, state: &str, keys: &[String]) -> Result<State, String> {
    let mut store = log.store();
    let stored = ready(store.peek(state)).map_err(|error| format!("peek {state}: {error}"))?;
    let values = ready(store.read_records(state, keys))
        .map_err(|error| format!("read records of {state}: {error}"))?;
    let mut records = BTreeMap::new();
    for (key, value) in keys.iter().zip(values) {
        if let Some(value) = value {
            records.insert(key.clone(), value);
        }
    }
    Ok(State {
        revision: stored.revision,
        payload: stored.payload.unwrap_or_default(),
        records,
    })
}

struct Run {
    acknowledged: BTreeMap<String, (State, Kept)>,
    attempted: BTreeSet<String>,
    host: Arc<Host>,
    log: Option<Arc<OneLog>>,
    owned: BTreeSet<String>,
    policy: Policy,
    rng: Rng,
    sources: BTreeMap<String, State>,
    digest: u64,
    stats: ImportsStats,
    verbose: bool,
}

impl Run {
    fn trace(&mut self, line: &str) {
        self.digest = fnv(fnv(self.digest, line.as_bytes()), b"\n");
        if self.verbose {
            println!("    imports {line}");
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
        self.owned.clear();
        self.recovered(&log)?;
        Ok(log)
    }

    fn keys(&self, state: &str) -> Vec<String> {
        let mut keys = self.sources[state].records.keys().cloned().collect::<Vec<_>>();
        if let Some((kept, _)) = self.acknowledged.get(state) {
            keys.extend(kept.records.keys().cloned());
        }
        keys.sort();
        keys.dedup();
        keys
    }

    fn recovered(&mut self, log: &Arc<OneLog>) -> Result<(), String> {
        for state in STATES {
            let held = read(log, state, &self.keys(state))?;
            match self.acknowledged.get(state) {
                Some((kept, _)) if held != *kept => {
                    return Err(format!(
                        "recovered {state} at revision {} differs from the acknowledged revision {}",
                        held.revision, kept.revision
                    ));
                }
                Some(_) => {}
                None if held.revision == 0 => {}
                None if self.attempted.contains(state) && held == self.sources[state] => {}
                None => {
                    return Err(format!(
                        "recovered {state} at revision {} that no import wrote",
                        held.revision
                    ));
                }
            }
        }
        self.trace("recovered");
        Ok(())
    }

    fn durable(&self, state: &str, expected: &State) -> Result<(), String> {
        let host: Arc<dyn LogHost> = Arc::new(self.host.copy());
        let reader = ready(OneLog::open(host, SCOPE, self.policy.clone()))
            .map_err(|error| format!("a second reader refused the log: {error}"))?;
        let held = read(&reader, state, &self.keys(state))?;
        if held != *expected {
            return Err(format!(
                "a second reader sees {state} at revision {}, not the acknowledged revision {}",
                held.revision, expected.revision
            ));
        }
        Ok(())
    }

    fn fault(&mut self) -> bool {
        match self.rng.below(6) {
            0 => {
                self.host.blip.store(true, Ordering::SeqCst);
                self.stats.blips += 1;
                self.trace("fault blip");
                true
            }
            1 => {
                let appends = self.rng.range(1, DOOM_APPENDS_MAX + 1);
                self.host.doom.store(appends, Ordering::SeqCst);
                self.trace(&format!("fault doom {appends}"));
                false
            }
            _ => false,
        }
    }

    fn import(&mut self, state: &str) -> Result<(), String> {
        let log = self.open()?;
        let source = self.sources[state].clone();
        let kept = self.acknowledged.get(state).cloned();
        let owned = self.owned.contains(state);
        let blip = self.fault();
        self.attempted.insert(state.to_owned());
        let result = ready(log.import_state(
            state,
            source.revision,
            &source.payload,
            &source.records(),
        ));
        self.host.doom.store(0, Ordering::SeqCst);
        let blipped = blip && !self.host.blip.swap(false, Ordering::SeqCst);
        self.trace(&format!("import {state} r{} {result:?}", source.revision));
        match (result, kept, owned) {
            (Err(StoreError::Fenced), _, true) => {
                self.stats.fenced += 1;
                Ok(())
            }
            (_, _, true) => Err(format!("import of {state} ran while a session owns it")),
            (Err(StoreError::Conflict { expected, actual }), Some((held, Kept::Advanced)), false)
                if expected == source.revision && actual == held.revision =>
            {
                self.stats.conflicts += 1;
                Ok(())
            }
            (Ok(Imported::Copied), Some((_, Kept::Imported)), false) => Err(format!(
                "import of {state} copied a state an earlier import already acknowledged"
            )),
            (Ok(imported), None | Some((_, Kept::Imported)), false) => {
                match imported {
                    Imported::Copied => self.stats.copied += 1,
                    Imported::Held => self.stats.held += 1,
                }
                self.acknowledged
                    .insert(state.to_owned(), (source.clone(), Kept::Imported));
                self.durable(state, &source)
            }
            (Ok(imported), Some((held, Kept::Advanced)), false) => Err(format!(
                "import of {state} at revision {} answered {imported:?} over a session at revision {}",
                source.revision, held.revision
            )),
            (Err(_), _, false) if self.host.dead.load(Ordering::SeqCst) => self.crash(),
            (Err(_), _, false) if blipped => Ok(()),
            (Err(error), _, false) => Err(format!("import of {state} failed on a healthy host: {error}")),
        }
    }

    fn session(&mut self, state: &str) -> Result<(), String> {
        let Some((held, _)) = self.acknowledged.get(state).cloned() else {
            return Ok(());
        };
        let log = self.open()?;
        let mut store = log.store();
        let owned = ready(store.acquire(state, OwnerId::new()))
            .map_err(|error| format!("acquire {state}: {error}"))?;
        self.owned.insert(state.to_owned());
        if owned.state.revision != held.revision {
            return Err(format!(
                "a session acquired {state} at revision {}, not the acknowledged {}",
                owned.state.revision, held.revision
            ));
        }
        let next = State {
            revision: held.revision + 1,
            payload: format!("{}+{}", held.payload, self.rng.below(1000)),
            records: held.records.clone(),
        };
        let revision = ready(store.replace(state, &owned.owner, held.revision, &next.payload, &[]))
            .map_err(|error| format!("replace {state}: {error}"))?;
        ready(store.flush()).map_err(|error| format!("flush {state}: {error}"))?;
        if revision != next.revision {
            return Err(format!("a session advanced {state} to {revision}, not {}", next.revision));
        }
        self.stats.sessions += 1;
        self.trace(&format!("session {state} r{revision}"));
        self.acknowledged
            .insert(state.to_owned(), (next, Kept::Advanced));
        Ok(())
    }

    fn crash(&mut self) -> Result<(), String> {
        self.trace("crash");
        self.stats.crashes += 1;
        self.log = None;
        self.open().map(|_| ())
    }

    fn step(&mut self) -> Result<(), String> {
        let state = STATES[self.rng.pick(STATES.len())];
        match self.rng.below(10) {
            0 => self.session(state),
            1 => {
                self.log = None;
                self.stats.crashes += 1;
                self.trace("restart");
                self.open().map(|_| ())
            }
            _ => self.import(state),
        }
    }
}

fn source(rng: &mut Rng, state: &str) -> State {
    let revision = rng.range(1, REVISION_MAX + 1);
    let pad = "x".repeat(usize::try_from(rng.below(PAYLOAD_PAD_MAX)).expect("a pad fits"));
    let records = (0..rng.below(RECORDS_MAX + 1))
        .map(|index| (format!("record-{index}"), format!("{state}/{index}/{}", rng.below(1000))))
        .collect();
    State {
        revision,
        payload: format!("{{\"state\":\"{state}\",\"revision\":{revision},\"pad\":\"{pad}\"}}"),
        records,
    }
}

fn run(seed: u64, verbose: bool, stats: &mut ImportsStats) -> Result<u64, String> {
    crate::host::enter(seed, &crate::executor::Clock::new());
    let mut rng = Rng::derive(seed, "imports");
    let snapshot_rows = rng.range(SNAPSHOT_ROWS_MIN, SNAPSHOT_ROWS_MAX);
    let ops = rng.range(OPS_MIN, OPS_MAX);
    let sources = STATES
        .iter()
        .map(|state| ((*state).to_owned(), source(&mut rng, state)))
        .collect();
    let mut run = Run {
        acknowledged: BTreeMap::new(),
        attempted: BTreeSet::new(),
        host: Arc::new(Host::default()),
        log: None,
        owned: BTreeSet::new(),
        policy: Policy {
            restarts: RESTART_CAP,
            snapshot_ratio: 2,
            snapshot_rows: Some(usize::try_from(snapshot_rows).expect("a row count fits")),
            ..Policy::default()
        },
        rng,
        sources,
        digest: FNV_OFFSET,
        stats: ImportsStats {
            seeds: 1,
            ..ImportsStats::default()
        },
        verbose,
    };
    run.trace(&format!("seed {seed} ops={ops} snapshot_rows={snapshot_rows}"));
    let mut result = Ok(());
    for _ in 0..ops {
        result = run.step();
        if result.is_err() {
            break;
        }
    }
    if result.is_ok() {
        run.log = None;
        result = run.open().map(|_| ());
    }
    *stats = run.stats;
    result.map(|()| run.digest)
}

pub struct ImportsOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: ImportsStats,
}

pub fn run_seed(seed: u64, mutation: ImportsMutation, verbose: bool) -> ImportsOutcome {
    import_sim::mutate(mutation);
    let outcome = std::panic::catch_unwind(|| {
        let mut stats = ImportsStats::default();
        let result = run(seed, verbose, &mut stats);
        (result, stats)
    });
    import_sim::mutate(Mutation::None);
    match outcome {
        Ok((Ok(digest), stats)) => ImportsOutcome {
            digest,
            wrong: None,
            stats,
        },
        Ok((Err(message), stats)) => ImportsOutcome {
            digest: 0,
            wrong: Some(message),
            stats,
        },
        Err(panic) => ImportsOutcome {
            digest: 0,
            wrong: Some(format!(
                "panic: {}",
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                    .unwrap_or_default()
            )),
            stats: ImportsStats::default(),
        },
    }
}

pub fn judge(
    seed: u64,
    mutation: ImportsMutation,
    twice: bool,
    verbose: bool,
    totals: &mut ImportsStats,
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

pub fn replay(seed: u64, mutation: ImportsMutation) -> String {
    let mut command = format!("nanocode-sim --scenario imports --seed {seed}");
    if mutation != ImportsMutation::None {
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
    mutation: ImportsMutation,
    totals: &ImportsStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=imports mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        ImportsMutation::None if failed == 0 => ExitCode::SUCCESS,
        ImportsMutation::None => ExitCode::FAILURE,
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
