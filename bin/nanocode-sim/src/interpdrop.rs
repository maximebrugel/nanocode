use std::{
    fmt::Write as _,
    ops::{Range, RangeInclusive},
    process::ExitCode,
};

use nanocode_python::rustpython_vm::{
    Interpreter, PyObjectRef, PyRef, Settings, VirtualMachine,
    builtins::PyWeak,
    import::{ExternalFrozen, set_external_frozen},
    sim::{self as vm_sim, Mutation as Rule},
};

use crate::{
    executor::Clock,
    rng::{FNV_OFFSET, Rng, fnv},
};

const INTERPRETERS: RangeInclusive<u64> = 2..=200;
const FUNCTIONS: RangeInclusive<u64> = 1..=6;
const CLASSES: RangeInclusive<u64> = 1..=4;
const CLOSURES: RangeInclusive<u64> = 0..=3;
const RINGS: RangeInclusive<u64> = 0..=4;
const RING_NODES: RangeInclusive<u64> = 1..=5;
const SELF_BOUND_PPM: u64 = 500_000;
const HELD: &str = "class Sentinel:\n    pass\nown = {\"sentinel\": Sentinel()}\nspace = {\"__builtins__\": own}\nexec(\"def held():\\n    return sentinel\\n\", space)\nheld = space.pop(\"held\")\nown[\"held\"] = held\nsentinel = own[\"sentinel\"]\n";
const FULL: usize = 2;
const SETUP: &str = "def ring(cls, n):\n    nodes = [cls() for _ in range(n)]\n    for i in range(n):\n        nodes[i].next = nodes[(i + 1) % n]\n    return nodes[0]\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterpDropMutation {
    None,
    HidesFunctionBuiltins,
}

impl InterpDropMutation {
    const ALL: [Self; 2] = [Self::None, Self::HidesFunctionBuiltins];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::HidesFunctionBuiltins => "hides_function_builtins",
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
            Self::HidesFunctionBuiltins => Rule::HidesFunctionBuiltins,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InterpDropStats {
    pub seeds: u64,
    pub interpreters: u64,
    pub functions: u64,
    pub classes: u64,
    pub closures: u64,
    pub rings: u64,
    pub freed: u64,
}

impl InterpDropStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.interpreters += other.interpreters;
        self.functions += other.functions;
        self.classes += other.classes;
        self.closures += other.closures;
        self.rings += other.rings;
        self.freed += other.freed;
    }
}

struct Held {
    function: PyObjectRef,
    sentinel: PyRef<PyWeak>,
}

struct Outcome {
    digest: u64,
    wrong: Option<String>,
    stats: InterpDropStats,
    shape: String,
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

fn draw(rng: &mut Rng, range: &RangeInclusive<u64>) -> u64 {
    rng.range(*range.start(), *range.end())
}

fn program(rng: &mut Rng, stats: &mut InterpDropStats, shape: &mut String) -> String {
    let functions = draw(rng, &FUNCTIONS);
    let classes = draw(rng, &CLASSES);
    let closures = draw(rng, &CLOSURES);
    let rings = draw(rng, &RINGS);
    let mut source = SETUP.to_owned();
    for function in 0..functions {
        let _ = write!(source, "def f{function}(x):\n    return x + {function}\n");
    }
    for class in 0..classes {
        let callee = rng.below(functions);
        let _ = write!(
            source,
            "class K{class}:\n    def m(self):\n        return f{callee}({class})\n"
        );
        if rng.chance(SELF_BOUND_PPM) {
            let _ = write!(
                source,
                "k{class} = K{class}()\nk{class}.bound = k{class}.m\n"
            );
        }
    }
    for closure in 0..closures {
        let _ = write!(
            source,
            "def make{closure}():\n    held = []\n    def inner():\n        return held\n    held.append(inner)\n    return inner\nclosure{closure} = make{closure}()\n"
        );
    }
    for ring in 0..rings {
        let nodes = draw(rng, &RING_NODES);
        let class = rng.below(classes);
        let _ = writeln!(source, "ring{ring} = ring(K{class}, {nodes})");
    }
    let _ = writeln!(source, "total = K0().m()");
    let _ = write!(shape, "f{functions}k{classes}c{closures}r{rings}");
    stats.functions += functions;
    stats.classes += classes;
    stats.closures += closures;
    stats.rings += rings;
    source
}

fn round(
    vm: &VirtualMachine,
    previous: Option<Held>,
    holds: bool,
    rng: &mut Rng,
    stats: &mut InterpDropStats,
    shape: &mut String,
) -> Result<(usize, usize, Option<Held>), String> {
    let sentinel = previous.map(|held| {
        drop(held.function);
        held.sentinel
    });
    let freed = vm.state.gc.collect_force(FULL).collected;
    if sentinel.is_some_and(|sentinel| sentinel.upgrade().is_some()) {
        return Err("the previous interpreter's function was released, yet the sentinel only its __builtins__ dict holds survived gc.collect(2)".to_owned());
    }
    let walked = vm.state.gc.collect_force(FULL).candidates;
    let scope = vm.new_scope_with_builtins();
    let mut source = program(rng, stats, shape);
    if holds {
        source.push_str(HELD);
    }
    vm.run_string(scope.clone(), &source, "<interp_drop>")
        .map_err(|_| format!("the program raised:\n{source}"))?;
    let held = holds
        .then(|| {
            let global = |name: &str| {
                scope
                    .globals
                    .get_item(name, vm)
                    .map_err(|_| format!("{name} is not defined"))
            };
            Ok::<_, String>(Held {
                function: global("held")?,
                sentinel: global("sentinel")?
                    .downgrade(None, vm)
                    .map_err(|_| "the sentinel takes no weakref".to_owned())?,
            })
        })
        .transpose()?;
    vm.state.gc.collect_force(FULL);
    drop(scope);
    Ok((freed, walked, held))
}

fn run_seed(seed: u64, mutation: InterpDropMutation) -> Outcome {
    vm_sim::mutate(mutation.rule());
    crate::host::enter(seed, &Clock::new());
    let mut rng = Rng::derive(seed, "interp_drop");
    let mut shape = String::new();
    let mut stats = InterpDropStats {
        seeds: 1,
        ..InterpDropStats::default()
    };
    let interpreters = draw(&mut rng, &INTERPRETERS);
    let mut walked = Vec::new();
    let mut pending = None;
    let mut wrong = None;
    for index in 0..interpreters {
        let holds = index + 1 < interpreters;
        let previous = pending.take();
        let interpreter = interpreter(&mut rng);
        let ran =
            interpreter.enter(|vm| round(vm, previous, holds, &mut rng, &mut stats, &mut shape));
        drop(interpreter);
        shape.push(' ');
        stats.interpreters += 1;
        match ran {
            Ok((freed, count, held)) => {
                if index > 0 {
                    stats.freed += freed as u64;
                }
                walked.push(count);
                pending = held;
            }
            Err(message) => {
                wrong = Some(format!("interpreter {index}: {message}"));
                break;
            }
        }
        if let (Some(&first), Some(&last)) = (walked.first(), walked.last())
            && last != first
        {
            wrong = Some(format!(
                "interpreter {index}'s full collection after settling walked {last} objects, interpreter 0's walked {first}: the {index} interpreters dropped before it left objects no collection frees"
            ));
            break;
        }
    }
    std::mem::forget(pending);
    let mut digest = fnv(FNV_OFFSET, shape.as_bytes());
    for count in &walked {
        digest = fnv(digest, &count.to_le_bytes());
    }
    digest = fnv(digest, &stats.freed.to_le_bytes());
    Outcome {
        digest,
        wrong,
        stats,
        shape: format!(
            "interpreters={interpreters} walked={} programs={}",
            walked.first().copied().unwrap_or_default(),
            shape.trim_end()
        ),
    }
}

pub fn judge(
    seed: u64,
    mutation: InterpDropMutation,
    twice: bool,
    verbose: bool,
    totals: &mut InterpDropStats,
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

pub fn replay(seed: u64, mutation: InterpDropMutation) -> String {
    let mut command = format!("nanocode-sim --scenario interp_drop --seed {seed}");
    if mutation != InterpDropMutation::None {
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
    mutation: InterpDropMutation,
    totals: &InterpDropStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=interp_drop mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        InterpDropMutation::None if failed == 0 => ExitCode::SUCCESS,
        InterpDropMutation::None => ExitCode::FAILURE,
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
