use std::{
    ops::{Range, RangeInclusive},
    panic::{AssertUnwindSafe, catch_unwind},
    process::ExitCode,
    sync::{
        Once,
        atomic::{AtomicBool, Ordering},
    },
};

use nanocode_python::rustpython_vm::{
    Interpreter, PyObjectRef, PyRef, Settings, VirtualMachine,
    builtins::PyWeak,
    import::{ExternalFrozen, set_external_frozen},
    sim::{self as vm_sim, Mutation as Rule, UNWOUND},
};

use crate::{
    executor::Clock,
    rng::{FNV_OFFSET, Rng, fnv},
};

const INTERPRETERS: RangeInclusive<u64> = 2..=12;
const RINGS: RangeInclusive<u64> = 1..=4;
const RING_NODES: RangeInclusive<u64> = 1..=5;
const HELD_MAX: u64 = 1 << 20;
const UNWIND_PPM: u64 = 750_000;
const GENERATIONS: u64 = 3;
const FULL: usize = 2;
const SETUP: &str = "class C:\n    pass\ndef ring(n):\n    nodes = [C() for _ in range(n)]\n    for i in range(n):\n        nodes[i].next = nodes[(i + 1) % n]\n    return nodes[0]\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GcUnwindMutation {
    None,
    KeepsLeftoverBits,
}

impl GcUnwindMutation {
    const ALL: [Self; 2] = [Self::None, Self::KeepsLeftoverBits];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::KeepsLeftoverBits => "keeps_leftover_bits",
        }
    }

    pub fn names() -> String {
        Self::ALL[1..]
            .iter()
            .map(|mutation| mutation.name())
            .collect::<Vec<_>>()
            .join("|")
    }

    const fn rule(self) -> Rule {
        match self {
            Self::None => Rule::None,
            Self::KeepsLeftoverBits => Rule::KeepsLeftoverBits,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GcUnwindStats {
    pub seeds: u64,
    pub interpreters: u64,
    pub rings: u64,
    pub unwound: u64,
    pub held_through: u64,
    pub freed: u64,
}

impl GcUnwindStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.interpreters += other.interpreters;
        self.rings += other.rings;
        self.unwound += other.unwound;
        self.held_through += other.held_through;
        self.freed += other.freed;
    }
}

struct Outcome {
    digest: u64,
    wrong: Option<String>,
    stats: GcUnwindStats,
    shape: String,
}

static BROKEN: AtomicBool = AtomicBool::new(false);

fn quiet_unwinds() {
    static QUIET: Once = Once::new();
    QUIET.call_once(|| {
        let shown = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload().downcast_ref::<&str>() != Some(&UNWOUND) {
                shown(info);
            }
        }));
    });
}

fn interpreter(rng: &mut Rng) -> Interpreter {
    set_external_frozen(ExternalFrozen {
        find: nanocode_python_data::find_module,
        load: nanocode_python_data::load_module,
    });
    let mut settings = Settings::default();
    settings.install_signal_handlers = false;
    settings.import_site = false;
    settings.user_site_directory = false;
    settings.write_bytecode = false;
    settings.ignore_environment = true;
    settings.isolated = true;
    settings.hash_seed = Some(rng.draw() as u32);
    Interpreter::without_stdlib(settings)
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_owned())
}

fn draw(rng: &mut Rng, range: &RangeInclusive<u64>) -> u64 {
    rng.range(*range.start(), *range.end())
}

fn collect(vm: &VirtualMachine, generation: usize) -> Result<(), String> {
    catch_unwind(AssertUnwindSafe(|| vm.state.gc.collect_force(generation)))
        .map(|_| ())
        .map_err(|payload| panic_text(payload.as_ref()))
}

fn check(
    vm: &VirtualMachine,
    previous: &[PyRef<PyWeak>],
    rng: &mut Rng,
    stats: &mut GcUnwindStats,
) -> Result<(), String> {
    if previous.is_empty() {
        return Ok(());
    }
    let mut held = Vec::new();
    let mut members: Vec<PyObjectRef> = Vec::new();
    for (index, weak) in previous.iter().enumerate() {
        if rng.below(2) == 0
            && let Some(member) = weak.upgrade()
        {
            held.push(index);
            members.push(member);
        }
    }
    let holder = vm.ctx.new_list(members);
    collect(vm, FULL).map_err(|message| {
        format!("gc.collect(2) with a list holding rings {held:?} panicked: {message}")
    })?;
    if let Some(index) = held
        .iter()
        .find(|&&index| previous[index].upgrade().is_none())
    {
        return Err(format!("ring {index}, held by a live list, was freed"));
    }
    stats.held_through += held.len() as u64;
    drop(holder);
    collect(vm, FULL).map_err(|message| format!("gc.collect(2) panicked: {message}"))?;
    let survivors: Vec<usize> = (0..previous.len())
        .filter(|&index| previous[index].upgrade().is_some())
        .collect();
    if !survivors.is_empty() {
        return Err(format!(
            "rings {survivors:?} of {} left by the previous interpreter survived gc.collect(2)",
            previous.len()
        ));
    }
    stats.freed += previous.len() as u64;
    Ok(())
}

fn build(
    vm: &VirtualMachine,
    rng: &mut Rng,
    unwinds: bool,
    stats: &mut GcUnwindStats,
    shape: &mut String,
) -> Result<Vec<PyRef<PyWeak>>, String> {
    let scope = vm.new_scope_with_builtins();
    vm.run_string(scope.clone(), SETUP, "<gc_unwind>")
        .map_err(|_| "the ring setup raised".to_owned())?;
    let ring = scope
        .globals
        .get_item("ring", vm)
        .map_err(|_| "ring is not defined".to_owned())?;
    let rings = draw(rng, &RINGS);
    let mut heads = Vec::new();
    let mut weaks = Vec::new();
    for _ in 0..rings {
        let nodes = draw(rng, &RING_NODES);
        shape.push_str(&format!("{nodes},"));
        let head = ring
            .call((vm.ctx.new_int(nodes),), vm)
            .map_err(|_| "ring() raised".to_owned())?;
        weaks.push(
            head.downgrade(None, vm)
                .map_err(|_| "a ring head takes no weakref".to_owned())?,
        );
        heads.push(head);
    }
    stats.rings += rings;
    if unwinds {
        let count = rng.below(HELD_MAX + 1);
        let pinned = heads[rng.pick(heads.len())].clone();
        let held: Vec<PyObjectRef> = (0..count).map(|_| pinned.clone()).collect();
        let generation = rng.below(GENERATIONS) as usize;
        shape.push_str(&format!("u{generation}"));
        vm_sim::unwind_next_collection();
        match collect(vm, generation) {
            Err(message) if message == UNWOUND => {}
            Err(message) => {
                return Err(format!(
                    "the collection asked to unwind panicked instead: {message}"
                ));
            }
            Ok(()) => return Err("the collection asked to unwind finished".to_owned()),
        }
        drop(held);
        drop(pinned);
        stats.unwound += 1;
    }
    drop(heads);
    if rng.below(2) == 0 {
        shape.push('c');
        collect(vm, FULL).map_err(|message| {
            format!("gc.collect(2) in the interpreter that built the rings panicked: {message}")
        })?;
    }
    shape.push(' ');
    Ok(weaks)
}

fn run_seed(seed: u64, mutation: GcUnwindMutation) -> Outcome {
    vm_sim::mutate(mutation.rule());
    quiet_unwinds();
    crate::host::enter(seed, &Clock::new());
    let mut rng = Rng::derive(seed, "gc_unwind");
    let interpreters = draw(&mut rng, &INTERPRETERS);
    let mut shape = format!("interpreters={interpreters} rings=");
    let mut stats = GcUnwindStats {
        seeds: 1,
        interpreters,
        ..GcUnwindStats::default()
    };
    if BROKEN.load(Ordering::SeqCst) {
        return Outcome {
            digest: FNV_OFFSET,
            wrong: Some(
                "an earlier seed left this process's Python heap broken; replay it alone"
                    .to_owned(),
            ),
            stats,
            shape,
        };
    }
    let mut pending: Vec<PyRef<PyWeak>> = Vec::new();
    let mut wrong = None;
    for round in 0..interpreters {
        let last = round + 1 == interpreters;
        let unwinds = round == 0 || rng.chance(UNWIND_PPM);
        let previous = std::mem::take(&mut pending);
        let ran = catch_unwind(AssertUnwindSafe(|| {
            let interpreter = interpreter(&mut rng);
            let made = interpreter.enter(|vm| {
                check(vm, &previous, &mut rng, &mut stats)?;
                if last {
                    return Ok(Vec::new());
                }
                build(vm, &mut rng, unwinds, &mut stats, &mut shape)
            });
            drop(interpreter);
            made
        }));
        drop(previous);
        match ran {
            Ok(Ok(made)) => pending = made,
            Ok(Err(message)) => wrong = Some(format!("interpreter {round}: {message}")),
            Err(payload) => {
                wrong = Some(format!(
                    "interpreter {round} panicked: {}",
                    panic_text(payload.as_ref())
                ));
            }
        }
        if wrong.is_some() {
            BROKEN.store(true, Ordering::SeqCst);
            std::mem::forget(std::mem::take(&mut pending));
            break;
        }
    }
    let digest = fnv(
        fnv(FNV_OFFSET, shape.as_bytes()),
        format!("{} {} {}", stats.rings, stats.unwound, stats.freed).as_bytes(),
    );
    Outcome {
        digest,
        wrong,
        stats,
        shape,
    }
}

pub fn judge(
    seed: u64,
    mutation: GcUnwindMutation,
    twice: bool,
    verbose: bool,
    totals: &mut GcUnwindStats,
) -> Option<String> {
    let first = run_seed(seed, mutation);
    totals.add(&first.stats);
    if verbose {
        println!(
            "seed {seed} digest {:016x} {} stats={:?}",
            first.digest, first.shape, first.stats
        );
    }
    if first.wrong.is_some() || !twice {
        return first
            .wrong
            .map(|message| format!("{} :: {message}", first.shape));
    }
    let second = run_seed(seed, mutation);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: GcUnwindMutation) -> String {
    let mut command = format!("nanocode-sim --scenario gc_unwind --seed {seed}");
    if mutation != GcUnwindMutation::None {
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
    mutation: GcUnwindMutation,
    totals: &GcUnwindStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=gc_unwind mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        GcUnwindMutation::None if failed == 0 => ExitCode::SUCCESS,
        GcUnwindMutation::None => ExitCode::FAILURE,
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
