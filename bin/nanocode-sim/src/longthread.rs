use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    ops::Range,
    process::ExitCode,
};

use nanocode::tool_contract::ToolContext;
use nanocode_tool_runtime::{Tools, runtime::ToolRuntime};

use crate::{
    executor::{Clock, Ended, Executor, Task},
    rng::{FNV_OFFSET, Rng, fnv},
};

const CELLS_MIN: u64 = 40;
const CELLS_MAX: u64 = 400;
const READS_PER_CALL_MAX: u64 = 12;
const SESSION: &str = "long-thread";
const PROBE: &str = "const a = await tools.sh({ command: 'python3 plot.py --out out/probe.png' }); const b = await tools.t({ k: 'probe' }); let e = 0; try { await tools.sh({ command: 'fail' }); } catch { e = 1; } text([a, b, e].join('|'));";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LongThreadMutation {
    None,
    RereadHistory,
    GrowingOutput,
}

impl LongThreadMutation {
    const ALL: [Self; 3] = [Self::None, Self::RereadHistory, Self::GrowingOutput];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RereadHistory => "reread_history",
            Self::GrowingOutput => "growing_output",
        }
    }

    pub fn names() -> String {
        Self::ALL[1..]
            .iter()
            .map(|mutation| mutation.name())
            .collect::<Vec<_>>()
            .join("|")
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LongThreadStats {
    pub seeds: u64,
    pub cells: u64,
    pub calls: u64,
    pub reads: u64,
}

impl LongThreadStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.cells += other.cells;
        self.calls += other.calls;
        self.reads += other.reads;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Work {
    calls: u64,
    reads: u64,
    output: usize,
}

thread_local! {
    static CALLS: Cell<u64> = const { Cell::new(0) };
    static READS: Cell<u64> = const { Cell::new(0) };
    static STATE: RefCell<State> = const {
        RefCell::new(State {
            mutation: LongThreadMutation::None,
            named: BTreeSet::new(),
            answers: Vec::new(),
        })
    };
}

fn bump(counter: &'static std::thread::LocalKey<Cell<u64>>) {
    counter.with(|count| count.set(count.get() + 1));
}

struct State {
    mutation: LongThreadMutation,
    named: BTreeSet<String>,
    answers: Vec<String>,
}

fn state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with_borrow_mut(f)
}

fn images(text: &str) -> impl Iterator<Item = &str> {
    text.split_whitespace().filter(|word| {
        [".png", ".jpg", ".webp"]
            .iter()
            .any(|extension| word.ends_with(extension))
    })
}

#[nanocode::tool(name = "Read", description = "Read a file")]
async fn read(file_path: String) -> std::io::Result<String> {
    missing(&file_path)
}

fn missing(file_path: &str) -> std::io::Result<String> {
    bump(&READS);
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{file_path} does not exist"),
    ))
}

#[nanocode::tool(name = "sh", description = "Run a command")]
async fn sh(command: String) -> std::io::Result<String> {
    bump(&CALLS);
    let history = state(|state| {
        let history = if state.mutation == LongThreadMutation::RereadHistory {
            state.named.iter().cloned().collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        state.named.extend(images(&command).map(str::to_owned));
        history
    });
    for path in history {
        let _ = missing(&path);
    }
    if command == "fail" {
        return Err(std::io::Error::other("command failed"));
    }
    let written = images(&command).collect::<Vec<_>>().join(" ");
    Ok(if command.contains("python3") {
        format!("wrote {written} Figure_1.png")
    } else {
        format!("ran {written}")
    })
}

#[nanocode::tool(name = "t", description = "Answer a key")]
async fn t(k: String) -> std::io::Result<String> {
    bump(&CALLS);
    Ok(state(|state| {
        state.answers.push(format!("t:{k}"));
        if state.mutation == LongThreadMutation::GrowingOutput {
            state.answers.join(",")
        } else {
            format!("t:{k}")
        }
    }))
}

fn block<T>(
    seed: u64,
    clock: &std::sync::Arc<Clock>,
    future: impl Future<Output = T>,
) -> Option<T> {
    let slot = std::cell::RefCell::new(None);
    let task: Task<'_> = Box::pin(async {
        *slot.borrow_mut() = Some(future.await);
    });
    match Executor::new(
        std::sync::Arc::clone(clock),
        Rng::derive(seed, "long_thread_scheduler"),
    )
    .run(vec![task], None)
    {
        Ended::Finished => slot.into_inner(),
        Ended::Crashed | Ended::Stuck => None,
    }
}

fn filler(rng: &mut Rng, index: u64) -> String {
    match rng.below(5) {
        0 => format!(
            "text(await tools.sh({{ command: 'python3 chart{index}.py --out out/c{index}.png' }}));"
        ),
        1 => format!("text(await tools.sh({{ command: 'convert a{index}.jpg b{index}.webp' }}));"),
        2 => format!("text(await tools.t({{ k: 'k{index}' }}));"),
        3 => "try { await tools.sh({ command: 'fail' }); } catch { text('failed'); }".to_owned(),
        _ => format!(
            "const r = await Promise.all([tools.t({{ k: 'x{index}' }}), tools.sh({{ command: 'ls d{index}.png' }})]); text(r.join(' '));"
        ),
    }
}

struct Outcome {
    digest: u64,
    shape: String,
    stats: LongThreadStats,
    wrong: Option<String>,
}

fn simulate(seed: u64, mutation: LongThreadMutation) -> Outcome {
    let mut rng = Rng::derive(seed, "long_thread");
    let cells = rng.range(CELLS_MIN, CELLS_MAX);
    let probes = [2, cells / 2, cells - 1];
    let shape = format!("cells={cells} probes={probes:?}");
    let mut stats = LongThreadStats {
        seeds: 1,
        ..LongThreadStats::default()
    };
    let mut digest = fnv(FNV_OFFSET, shape.as_bytes());
    CALLS.set(0);
    READS.set(0);
    let clock = Clock::new();
    crate::host::enter(seed, &clock);
    state(|state| {
        state.mutation = mutation;
        state.named.clear();
        state.answers.clear();
    });

    let tools = match Tools::builder()
        .without_defaults()
        .add(read)
        .add(sh)
        .add(t)
        .build()
    {
        Ok(tools) => tools,
        Err(error) => {
            return Outcome {
                digest,
                shape,
                stats,
                wrong: Some(format!("tools: {error}")),
            };
        }
    };
    let runtime = ToolRuntime::new_with_tools(std::env::temp_dir(), Vec::new(), &tools);
    let mut measured: Vec<(u64, Work)> = Vec::new();
    let mut wrong = None;

    for index in 0..cells {
        let source = if probes.contains(&index) {
            PROBE.to_owned()
        } else {
            filler(&mut rng, index)
        };
        let (calls, reads) = (CALLS.get(), READS.get());
        let call = format!("cell-{index}");
        let result = block(
            seed,
            &clock,
            runtime.execute_code(
                &source,
                ToolContext::new(SESSION, "turn", &call, &[], usize::MAX),
            ),
        );
        let output = match result {
            None => {
                wrong = Some(format!("cell {index} never finished"));
                break;
            }
            Some(Ok(execution)) => {
                serde_json::to_string(&execution.output).map_or(0, |text| text.len())
            }
            Some(Err(error)) => {
                wrong = Some(format!("cell {index} failed: {error}"));
                break;
            }
        };
        let work = Work {
            calls: CALLS.get() - calls,
            reads: READS.get() - reads,
            output,
        };
        stats.cells += 1;
        stats.calls += work.calls;
        stats.reads += work.reads;
        digest = fnv(digest, &work.reads.to_le_bytes());
        if work.reads > READS_PER_CALL_MAX * work.calls.max(1) {
            wrong = Some(format!(
                "host_calls_bounded: cell {index} of {cells} made {} host reads for {} calls the cell asked for (at most {READS_PER_CALL_MAX} each)",
                work.reads, work.calls
            ));
            break;
        }
        if probes.contains(&index) {
            if let Some((first, expected)) = measured.first()
                && *expected != work
            {
                wrong = Some(format!(
                    "turn_work_flat: the same probe cell cost {work:?} at cell {index} but {expected:?} at cell {first}"
                ));
                break;
            }
            measured.push((index, work));
        }
    }
    let _ = block(seed, &clock, runtime.control().cancel());
    Outcome {
        digest,
        shape,
        stats,
        wrong,
    }
}

pub fn judge(
    seed: u64,
    mutation: LongThreadMutation,
    twice: bool,
    verbose: bool,
    totals: &mut LongThreadStats,
) -> Option<String> {
    let first = simulate(seed, mutation);
    totals.add(&first.stats);
    if verbose {
        println!(
            "seed {seed} digest {:016x} {} {:?}",
            first.digest, first.shape, first.stats
        );
    }
    if first.wrong.is_some() || !twice {
        return first
            .wrong
            .map(|message| format!("{} :: {message}", first.shape));
    }
    let second = simulate(seed, mutation);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: trace digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: LongThreadMutation) -> String {
    let mut command = format!("nanocode-sim --scenario long_thread --seed {seed}");
    if mutation != LongThreadMutation::None {
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
    mutation: LongThreadMutation,
    totals: &LongThreadStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=long_thread mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        LongThreadMutation::None if failed == 0 => ExitCode::SUCCESS,
        LongThreadMutation::None => ExitCode::FAILURE,
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
