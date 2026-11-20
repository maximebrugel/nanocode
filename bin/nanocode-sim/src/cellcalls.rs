use std::{
    collections::HashMap,
    ops::Range,
    process::ExitCode,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use nanocode_code_mode::code_mode::CellJournals;
use nanocode_code_mode_engine::{CellTools, EmbeddedHost, Interrupt, RuntimeEvent};
use nanocode_tool_runtime::{SessionEnvironment, Tools, runtime::ToolRuntime};
use serde_json::{Value, json};

use crate::{
    executor::Clock,
    rng::{FNV_OFFSET, Rng, fnv},
};

const CALLS_MIN: u64 = 1_000;
const CALLS_MAX: u64 = 30_000;
const BATCH_MAX: u64 = 500;
const PAD_MAX: u64 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellCallsMutation {
    None,
    CrossResults,
    RootRecoveryUnbound,
}

impl CellCallsMutation {
    const ALL: [Self; 3] = [Self::None, Self::CrossResults, Self::RootRecoveryUnbound];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CrossResults => "cross_results",
            Self::RootRecoveryUnbound => "root_recovery_unbound",
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
pub struct CellCallsStats {
    pub seeds: u64,
    pub calls: u64,
    pub batches: u64,
}

impl CellCallsStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.calls += other.calls;
        self.batches += other.batches;
    }
}

pub struct CellCallsOutcome {
    pub digest: u64,
    pub shape: String,
    pub stats: CellCallsStats,
    pub wrong: Option<String>,
}

fn block<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::yield_now();
    }
}

fn drive(
    host: &mut EmbeddedHost,
    calls: u64,
    pad: &str,
    mutation: CellCallsMutation,
    digest: &mut u64,
) -> Result<(), String> {
    let mut seen = vec![false; usize::try_from(calls).expect("a call count fits in usize")];
    let mut received = 0_u64;
    let mut previous = 0_u64;

    loop {
        match block(host.read_event()).map_err(|error| format!("read_event: {error}"))? {
            RuntimeEvent::ToolCall { id, input, .. } => {
                let index = input.get("i").and_then(Value::as_u64).ok_or_else(|| {
                    format!("cell_call_once: call {id} carries no index: {input}")
                })?;
                let slot = usize::try_from(index)
                    .ok()
                    .and_then(|index| seen.get_mut(index))
                    .ok_or_else(|| {
                        format!("cell_call_once: call {id} names index {index} of {calls}")
                    })?;
                if *slot {
                    return Err(format!(
                        "cell_call_once: index {index} reached the host twice (call {id}, {received} received)"
                    ));
                }
                *slot = true;
                received += 1;
                *digest = fnv(*digest, &index.to_le_bytes());

                let answer = if mutation == CellCallsMutation::CrossResults && received > 1 {
                    previous
                } else {
                    index
                };
                previous = index;
                let encoded = json!({ "i": answer, "p": pad }).to_string();
                host.send_tool_result(1, id, encoded, true)
                    .map_err(|error| format!("send_tool_result: {error}"))?;
            }
            RuntimeEvent::Done { stored, .. } => {
                if received != calls {
                    return Err(format!(
                        "cell_call_once: {received} of {calls} calls reached the host"
                    ));
                }

                let expected = json!([calls, calls * (calls - 1) / 2]);
                let reported = stored
                    .get("done")
                    .and_then(|raw| serde_json::from_str::<Value>(raw.get()).ok());
                if reported.as_ref() != Some(&expected) {
                    return Err(format!(
                        "cell_call_once: the cell reported {reported:?}, not {expected}"
                    ));
                }
                return Ok(());
            }
            RuntimeEvent::Error { message, .. } => {
                return Err(format!("cell failed after {received} calls: {message}"));
            }
            _ => {}
        }
    }
}

fn root_recovery(seed: u64, mutation: CellCallsMutation) -> Result<(), String> {
    let root = format!("cell-calls-{seed}");
    let child = format!("{root}.child");

    for session in [&root, &child] {
        let journals = CellJournals::new();
        let tools = Tools::builder()
            .without_defaults()
            .cell_journals(journals.clone())
            .build()
            .map_err(|error| format!("tools: {error}"))?;
        let tools = if mutation == CellCallsMutation::RootRecoveryUnbound {
            tools
        } else {
            tools.for_session(&SessionEnvironment::new(session, &root))
        };
        drop(ToolRuntime::new_with_tools(
            std::env::temp_dir(),
            Vec::new(),
            &tools,
        ));

        let preempts = journals.preempts_recovery(session);
        if preempts != (*session == root) {
            return Err(format!(
                "root_recovery_preemptible: session {session} of root {root} lets a steer cut its recovered observation short: {preempts}"
            ));
        }
    }
    Ok(())
}

fn simulate(seed: u64, mutation: CellCallsMutation) -> CellCallsOutcome {
    let mut rng = Rng::derive(seed, "cell_calls");
    let calls = rng.range(CALLS_MIN, CALLS_MAX);
    let batch = rng.range(1, BATCH_MAX);
    let pad = "x".repeat(usize::try_from(rng.below(PAD_MAX)).expect("a pad fits in usize"));
    let shape = format!("calls={calls} batch={batch} pad={}", pad.len());

    let code = format!(
        "const n = {calls}, b = {batch}; let c = 0, s = 0; for (let i = 0; i < n; i += b) {{ const r = await Promise.all(Array.from({{ length: Math.min(b, n - i) }}, (_, j) => tools.t({{ i: i + j }}))); for (const v of r) {{ const o = typeof v === 'string' ? JSON.parse(v) : v; c += 1; s += o.i; }} }} store('done', [c, s]);"
    );
    let tools = CellTools::encode(&[json!({ "kind": "function", "name": "t", "tool_name": "t" })])
        .expect("the cell tool encodes");
    crate::host::enter(seed, &Clock::new());
    let mut host = EmbeddedHost::spawn().expect("the embedded host starts");
    let mut digest = fnv(FNV_OFFSET, shape.as_bytes());

    let wrong = root_recovery(seed, mutation)
        .and_then(|()| {
            host.start_cell(
                1,
                &code,
                HashMap::new(),
                Arc::new(tools),
                None,
                None,
                Interrupt::new(),
            )
            .map_err(|error| format!("start_cell: {error}"))
        })
        .and_then(|()| drive(&mut host, calls, &pad, mutation, &mut digest))
        .err();

    CellCallsOutcome {
        digest,
        shape,
        stats: CellCallsStats {
            seeds: 1,
            calls,
            batches: calls.div_ceil(batch),
        },
        wrong,
    }
}

pub fn judge(
    seed: u64,
    mutation: CellCallsMutation,
    twice: bool,
    verbose: bool,
    totals: &mut CellCallsStats,
) -> Option<String> {
    let first = simulate(seed, mutation);
    totals.add(&first.stats);
    if verbose {
        println!("seed {seed} digest {:016x} {}", first.digest, first.shape);
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

pub fn replay(seed: u64, mutation: CellCallsMutation) -> String {
    let mut command = format!("nanocode-sim --scenario cell_calls --seed {seed}");
    if mutation != CellCallsMutation::None {
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
    mutation: CellCallsMutation,
    totals: &CellCallsStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=cell_calls mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        CellCallsMutation::None if failed == 0 => ExitCode::SUCCESS,
        CellCallsMutation::None => ExitCode::FAILURE,
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
