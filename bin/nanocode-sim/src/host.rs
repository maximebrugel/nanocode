use std::{
    cell::RefCell,
    sync::{Arc, Mutex, PoisonError, atomic::AtomicU64},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use nanocode_platform::{Host, HostFuture, Lent};

use crate::{executor::Clock, rng::Rng};

pub const SIMULATED_START_UNIX_SECS: u64 = 1_794_441_600;

struct Simulation {
    clock: Arc<Clock>,
    random: Rng,
    spawned: Vec<(HostFuture, String)>,
    started: Instant,
    stamps: &'static AtomicU64,
}

type Shared = Arc<Mutex<Simulation>>;

thread_local! {
    // Shared so a thread that works for this one while it waits (the native Python
    // interpreter's) reads the same clock and draws from the same stream; the two never run at once.
    static CURRENT: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

fn with<R>(use_simulation: impl FnOnce(&mut Simulation) -> R) -> R {
    CURRENT.with_borrow(|current| {
        let simulation = current
            .as_ref()
            .expect("the platform host is used only inside a simulated seed");
        use_simulation(&mut simulation.lock().unwrap_or_else(PoisonError::into_inner))
    })
}

/// Gives the borrowing thread back the simulation it had before it took on a lent one.
struct Restore(Option<Shared>);

impl Drop for Restore {
    fn drop(&mut self) {
        let kept = self.0.take();
        CURRENT.with_borrow_mut(|current| *current = kept);
    }
}

struct SimulatedHost;

static HOST: SimulatedHost = SimulatedHost;

thread_local! {
    // Each worker thread of --jobs runs its own seeds, so the UUID stamps and the pass flag are
    // per thread. The stamps are leaked once per thread because Host hands out a reference.
    static UUID_STAMPS: &'static AtomicU64 = Box::leak(Box::new(AtomicU64::new(0)));
    static SECOND_PASS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn second_pass<R>(run: impl FnOnce() -> R) -> R {
    SECOND_PASS.set(true);
    let result = run();
    SECOND_PASS.set(false);
    result
}

fn origin() -> Instant {
    let now = Instant::now();
    let span = Duration::from_secs(SIMULATED_START_UNIX_SECS);
    if SECOND_PASS.get() {
        now.checked_sub(span).expect(
            "the second pass's origin lies as far before the real clock as the simulated epoch",
        )
    } else {
        now.checked_add(span).expect(
            "the first pass's origin lies as far after the real clock as the simulated epoch",
        )
    }
}

impl Host for SimulatedHost {
    fn now(&self) -> Instant {
        with(|simulation| simulation.started + Duration::from_nanos(simulation.clock.now_ns()))
    }

    fn system_time(&self) -> SystemTime {
        let now_ns = with(|simulation| simulation.clock.now_ns());
        UNIX_EPOCH + Duration::from_secs(SIMULATED_START_UNIX_SECS) + Duration::from_nanos(now_ns)
    }

    fn sleep(&self, duration: Duration) -> HostFuture {
        with(|simulation| Box::pin(simulation.clock.sleep(duration)))
    }

    fn fill_random(&self, bytes: &mut [u8]) -> std::io::Result<()> {
        with(|simulation| {
            for chunk in bytes.chunks_mut(size_of::<u64>()) {
                let drawn = simulation.random.draw().to_le_bytes();
                chunk.copy_from_slice(&drawn[..chunk.len()]);
            }
        });
        Ok(())
    }

    fn uuid_stamps(&self) -> Option<&AtomicU64> {
        Some(with(|simulation| simulation.stamps))
    }

    fn lend(&self) -> Option<Lent> {
        let simulation = CURRENT.with_borrow(Option::clone)?;
        Some(Lent::new(move || {
            let kept = CURRENT.with_borrow_mut(|current| current.replace(simulation));
            Box::new(Restore(kept))
        }))
    }

    fn spawn(&self, future: HostFuture) {
        let site = if std::env::var_os("NANOCODE_SIM_POLLS").is_some() {
            let trace = std::backtrace::Backtrace::force_capture().to_string();
            if std::env::var_os("NANOCODE_SIM_RAW").is_some() {
                eprintln!("{trace}");
            }
            trace
                .lines()
                .filter(|line| line.contains("/crates/") && !line.contains("platform/src"))
                .map(|line| line.rsplit("/crates/").next().unwrap_or(line))
                .take(4)
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" < ")
        } else {
            String::new()
        };
        with(|simulation| simulation.spawned.push((future, site)));
    }
}

pub fn install() -> std::io::Result<()> {
    nanocode_platform::install(&HOST)
}

pub fn enter(seed: u64, clock: &Arc<Clock>) {
    let stamps = UUID_STAMPS.with(|stamps| *stamps);
    stamps.store(0, std::sync::atomic::Ordering::Relaxed);
    CURRENT.with_borrow_mut(|current| {
        *current = Some(Arc::new(Mutex::new(Simulation {
            clock: Arc::clone(clock),
            random: Rng::derive(seed, "host"),
            spawned: Vec::new(),
            started: origin(),
            stamps,
        })));
    });
}

pub fn entered<R>(seed: u64, clock: &Arc<Clock>, run: impl FnOnce() -> R) -> R {
    let kept = Restore(CURRENT.with_borrow_mut(Option::take));
    enter(seed, clock);
    let ran = run();
    drop(kept);
    ran
}

pub fn spawned() -> Vec<(HostFuture, String)> {
    CURRENT.with_borrow(|current| {
        current
            .as_ref()
            .map(|simulation| {
                std::mem::take(
                    &mut simulation
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .spawned,
                )
            })
            .unwrap_or_default()
    })
}
