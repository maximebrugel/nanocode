macro_rules! ensure {
    ($holds:expr, $($reason:tt)+) => {
        if !$holds {
            return Err(format!($($reason)+));
        }
    };
}

mod board_fold;
mod board_partitioned;
mod board_sync;
mod live_fold;
mod partitions;
mod rng;

use std::cell::RefCell;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rng::Rng;

pub type Checked = Result<(), String>;

const ITERATIONS_DEFAULT: u64 = 100_000;
// How long a stop (a signal or the --seconds deadline) waits for workers to finish their iteration before it prints
// the summary anyway: well inside the driver's 10 s grace between SIGTERM and SIGKILL.
const STOP_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    BoardFold,
    BoardFoldSpread,
    BoardPartitioned,
    BoardSyncIngest,
    LiveFoldDecode,
    PartitionsRow,
}

impl Target {
    const ALL: [Self; 6] = [
        Self::BoardFold,
        Self::BoardFoldSpread,
        Self::BoardPartitioned,
        Self::BoardSyncIngest,
        Self::LiveFoldDecode,
        Self::PartitionsRow,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::BoardFold => "board_fold",
            Self::BoardFoldSpread => "board_fold_spread",
            Self::BoardPartitioned => "board_partitioned",
            Self::BoardSyncIngest => "board_sync_ingest",
            Self::LiveFoldDecode => "live_fold_decode",
            Self::PartitionsRow => "partitions_row",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.name() == name)
    }

    fn check(self, rng: &mut Rng) -> Checked {
        match self {
            Self::BoardFold => board_fold::check(rng),
            Self::BoardFoldSpread => board_fold::spread(rng),
            Self::BoardPartitioned => board_partitioned::check(rng),
            Self::BoardSyncIngest => board_sync::check(rng),
            Self::LiveFoldDecode => live_fold::check(rng),
            Self::PartitionsRow => partitions::check(rng),
        }
    }
}

enum Count {
    Iterations(u64),
    One(u64),
}

struct Run {
    seed: u64,
    count: Count,
    jobs: usize,
    deadline: Option<Duration>,
    targets: Vec<Target>,
}

fn usage() -> ExitCode {
    let names: Vec<&str> = Target::ALL.iter().map(|target| target.name()).collect();
    eprintln!(
        "usage: nanocode-fuzz fuzz [--seed S] [--iterations N | --iteration I] [--seconds S] [--jobs J] [--target {}]",
        names.join("|")
    );
    ExitCode::from(2)
}

fn parse(args: &[String]) -> Option<Run> {
    let (command, rest) = args.split_first()?;
    if command != "fuzz" {
        return None;
    }
    let mut seed = None;
    let mut count = None;
    let mut jobs = None;
    let mut deadline = None;
    let mut target = None;
    let mut rest = rest.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next()?;
        match flag.as_str() {
            "--seed" if seed.is_none() => seed = Some(value.parse().ok()?),
            "--iterations" if count.is_none() => {
                count = Some(Count::Iterations(value.parse().ok()?));
            }
            "--iteration" if count.is_none() => count = Some(Count::One(value.parse().ok()?)),
            "--jobs" if jobs.is_none() => jobs = Some(value.parse().ok().filter(|jobs| *jobs > 0)?),
            "--seconds" if deadline.is_none() => {
                deadline = Some(Duration::try_from_secs_f64(value.parse().ok()?).ok()?);
            }
            "--target" if target.is_none() => target = Some(Target::parse(value)?),
            _ => return None,
        }
    }
    let seed = seed.unwrap_or_else(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is past the epoch");
        u64::try_from(now.as_nanos() % u128::from(u64::MAX)).expect("a remainder fits a u64")
    });
    let jobs = jobs.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from));
    let unbounded = if deadline.is_some() {
        u64::MAX
    } else {
        ITERATIONS_DEFAULT
    };
    Some(Run {
        seed,
        count: count.unwrap_or(Count::Iterations(unbounded)),
        jobs,
        deadline,
        targets: target.map_or_else(|| Target::ALL.to_vec(), |target| vec![target]),
    })
}

thread_local! {
    static PANICKED: RefCell<String> = const { RefCell::new(String::new()) };
}

fn record_panics() {
    std::panic::set_hook(Box::new(|info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_default();
        PANICKED
            .with(|panicked| *panicked.borrow_mut() = format!("panic: {message} at {location}"));
    }));
}

fn one(seed: u64, target: Target, iteration: u64) -> Checked {
    let checked = std::panic::catch_unwind(|| {
        let mut rng = Rng::new(seed, iteration, target.name());
        target.check(&mut rng)
    });
    checked.unwrap_or_else(|_| Err(PANICKED.with(|panicked| panicked.take())))
}

// SIGTERM or SIGINT stops the workers after their current iteration, and the run still prints its summary, so a
// shard stopped early (a box over its cores, an outer timeout) keeps its execs and failures. The summary waits at
// most STOP_WAIT for a worker still in an iteration.
#[cfg(unix)]
fn catch_stop() {
    use nanocode_sys::unix::signal;
    for number in [signal::SIGTERM, signal::SIGINT] {
        if let Err(error) = signal::listen(number) {
            eprintln!("nanocode-fuzz: cannot catch signal {number}: {error}");
        }
    }
}

#[cfg(not(unix))]
const fn catch_stop() {}

#[cfg(unix)]
fn stop_signal() -> Option<&'static str> {
    use nanocode_sys::unix::signal;
    if signal::take_pending(signal::SIGTERM) {
        Some("SIGTERM")
    } else if signal::take_pending(signal::SIGINT) {
        Some("SIGINT")
    } else {
        None
    }
}

#[cfg(not(unix))]
const fn stop_signal() -> Option<&'static str> {
    None
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(run) = parse(&args) else {
        return usage();
    };
    record_panics();
    catch_stop();
    let (first, total) = match run.count {
        Count::Iterations(n) => (0, n),
        Count::One(i) => (i, 1),
    };
    let width = run.targets.len() as u64;
    let work = total.saturating_mul(width);
    let cursor = AtomicU64::new(0);
    let done = AtomicU64::new(0);
    let started = Instant::now();
    let due = run.deadline.map(|deadline| started + deadline);
    let stop = AtomicBool::new(false);
    let failed = AtomicU64::new(0);
    let names: Vec<&str> = run.targets.iter().map(|target| target.name()).collect();
    let summary = |stopped_by: Option<&str>, unfinished: usize| {
        let seconds = started.elapsed().as_secs_f64();
        let work = done.load(Ordering::Relaxed);
        let failed = failed.load(Ordering::Relaxed);
        if let Some(name) = stopped_by {
            eprintln!("nanocode-fuzz: stopped by {name} after {seconds:.3}s");
        }
        if unfinished > 0 {
            eprintln!(
                "nanocode-fuzz: {unfinished} worker(s) still in an iteration {}s after the stop; not waiting for them",
                STOP_WAIT.as_secs()
            );
        }
        println!(
            "FUZZ iterations={work} failed={failed} seed={} seconds={seconds:.3} iterations_per_s={:.0} targets=[{}]",
            run.seed,
            work as f64 / seconds.max(f64::MIN_POSITIVE),
            names.join(",")
        );
        failed == 0
    };
    let mut stopped_by = None;
    let mut stopping_since: Option<Instant> = None;
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..run.jobs.min(usize::try_from(work).unwrap_or(usize::MAX)).max(1))
            .map(|_| {
                scope.spawn(|| loop {
                    if stop.load(Ordering::Relaxed) || due.is_some_and(|due| Instant::now() >= due) {
                        return;
                    }
                    let at = cursor.fetch_add(1, Ordering::Relaxed);
                    if at >= work {
                        return;
                    }
                    let target = run.targets[usize::try_from(at % width).expect("a target index fits")];
                    let iteration = first + at / width;
                    if let Err(reason) = one(run.seed, target, iteration) {
                        let reason = reason.replace('\n', " ");
                        // Print as found, so a shard killed at its deadline keeps its failures.
                        println!(
                            "FAIL fuzz target={} seed={} iteration={iteration} {reason} :: replay: cargo run -q -p nanocode-fuzz --release -- fuzz --seed {} --target {} --iteration {iteration}",
                            target.name(),
                            run.seed,
                            run.seed,
                            target.name()
                        );
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                    done.fetch_add(1, Ordering::Relaxed);
                })
            })
            .collect();
        loop {
            let unfinished = workers
                .iter()
                .filter(|worker| !worker.is_finished())
                .count();
            if unfinished == 0 {
                return;
            }
            if stopped_by.is_none()
                && let Some(name) = stop_signal()
            {
                stopped_by = Some(name);
                stop.store(true, Ordering::Relaxed);
            }
            if stopping_since.is_none()
                && (stopped_by.is_some() || due.is_some_and(|due| Instant::now() >= due))
            {
                stopping_since = Some(Instant::now());
            }
            // A worker deep in one long iteration, or starved at SCHED_IDLE on a busy box, must not cost the run its
            // summary before the SIGKILL that follows SIGTERM: print what finished and exit without joining it.
            if stopping_since.is_some_and(|since| since.elapsed() >= STOP_WAIT) {
                let passed = summary(stopped_by, unfinished);
                std::process::exit(i32::from(!passed));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    if summary(stopped_by, 0) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
