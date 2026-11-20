use std::{collections::BTreeMap, num::NonZeroUsize, ops::Range, process::ExitCode, sync::Arc};

use nanocode_code_mode::{ToolContext, ToolOutput};
use nanocode_hands::{
    AUTO, Auto, Chunks, Clock as RouterClock, Fetch, GetError, HandError, HandHost, HandPhase,
    HandRef, HandStatus, HostFuture, Placed, Placement, Placements, Policy, PutError, Router,
    Sha256Digest, Shell, StepOutcome, Stored, Target, Workspace as HandWorkspace,
    sim::{self as hands_sim, Mutation as HandRule},
};
use nanocode_sync::{Atom, block_on};
use serde_json::{Value, json};

use crate::rng::{FNV_OFFSET, Rng, fnv};

const SESSION: &str = "auto-session";
const IDLE_MS: u64 = 60_000;
const BASH: &str = "Bash";
const SCRIPT: &str = "command";
const SHELLS: &[Shell] = &[Shell {
    tool: BASH,
    script: SCRIPT,
}];
const WORKSPACE_TOOLS: &[&str] = &[BASH, "Read", "Write", "Edit", "Glob", "Grep"];
const HAND_TOOLS: &[&str] = &[
    BASH,
    "Read",
    "Write",
    "Edit",
    "Glob",
    "Grep",
    "Screenshot",
    "Browser",
];
const HAS: &[&str] = &[
    "echo", "cat", "ls", "grep", "sed", "git", "python3", "curl", "jq", "cd", "export", "wc",
];
const MISSING: &[&str] = &["cargo", "node", "docker", "make", "npm", "xcodebuild"];
const WIRES: &[&str] = &["workspace", "hosted"];
const STEPS: Range<u64> = 16..64;
const PIECES: Range<u64> = 1..5;
const EXPLICIT_PPM: u64 = 400_000;
const NO_SCRIPT_PPM: u64 = 50_000;
const UNPARSED_PPM: u64 = 250_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandAutoMutation {
    None,
    MovesExplicit,
    PrefersHand,
    MiscountsAuto,
}

impl HandAutoMutation {
    const ALL: [Self; 4] = [
        Self::None,
        Self::MovesExplicit,
        Self::PrefersHand,
        Self::MiscountsAuto,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MovesExplicit => "moves_explicit",
            Self::PrefersHand => "prefers_hand",
            Self::MiscountsAuto => "miscounts_auto",
        }
    }

    pub fn names() -> String {
        Self::ALL
            .iter()
            .skip(1)
            .map(|mutation| mutation.name())
            .collect::<Vec<_>>()
            .join("|")
    }

    const fn rule(self) -> HandRule {
        match self {
            Self::None => HandRule::None,
            Self::MovesExplicit => HandRule::AutoMovesExplicit,
            Self::PrefersHand => HandRule::AutoPrefersHand,
            Self::MiscountsAuto => HandRule::CountsAutoAsHosted,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct HandAutoStats {
    pub seeds: u64,
    pub explicit: u64,
    pub auto: u64,
    pub auto_workspace: u64,
    pub auto_hosted: u64,
    pub refused: u64,
}

impl HandAutoStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.explicit += other.explicit;
        self.auto += other.auto;
        self.auto_workspace += other.auto_workspace;
        self.auto_hosted += other.auto_hosted;
        self.refused += other.refused;
    }
}

struct Outcome {
    digest: u64,
    wrong: Option<String>,
    stats: HandAutoStats,
    shape: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece {
    Has(usize),
    Missing(usize),
    ByPath,
    Dynamic,
    Function(usize),
    Unparsed,
}

impl Piece {
    fn draw(rng: &mut Rng) -> Self {
        match rng.below(8) {
            0 => Self::Missing(rng.pick(MISSING.len())),
            1 => Self::ByPath,
            2 => Self::Dynamic,
            3 => Self::Function(rng.pick(HAS.len())),
            4 if rng.chance(UNPARSED_PPM) => Self::Unparsed,
            _ => Self::Has(rng.pick(HAS.len())),
        }
    }

    const fn here(self) -> bool {
        matches!(self, Self::Has(_) | Self::Function(_))
    }

    fn text(self, index: usize) -> String {
        match self {
            Self::Has(command) => format!("{} arg{index}", HAS[command]),
            Self::Missing(command) => format!("{} arg{index}", MISSING[command]),
            Self::ByPath => format!("./scripts/step{index}.sh"),
            Self::Dynamic => format!("$tool{index} --version"),
            Self::Function(command) => {
                format!("f{index}() {{ {} inner; }}; f{index}", HAS[command])
            }
            Self::Unparsed => format!("if true; then echo arg{index}"),
        }
    }
}

struct Call {
    step: String,
    wire: &'static str,
    tool: &'static str,
    input: Value,
    expected: Option<&'static str>,
}

#[derive(Clone, Default)]
struct Log {
    dispatched: BTreeMap<String, String>,
}

struct Space(Arc<Atom<Log>>);
struct Host(Arc<Atom<Log>>);
struct Still;
struct Unplaced;

impl HandWorkspace for Space {
    fn runs(&self, tool: &str) -> bool {
        WORKSPACE_TOOLS.contains(&tool)
    }

    fn has(&self, command: &str) -> bool {
        HAS.contains(&command)
    }

    fn run<'a>(
        &'a self,
        _session: &'a str,
        tool: &'a str,
        _input: Value,
        context: ToolContext<'a>,
    ) -> HostFuture<'a, ToolOutput> {
        Box::pin(async move {
            let step = context.call_id().to_owned();
            self.0.update(|log| {
                log.dispatched.insert(step.clone(), "workspace".to_owned());
            });
            ToolOutput::text(format!("ran {tool} in the workspace"))
        })
    }

    fn put<'a>(
        &'a self,
        _session: &'a str,
        _path: &'a str,
        _chunks: Box<dyn Chunks + 'a>,
        _sha256: Sha256Digest,
    ) -> HostFuture<'a, Result<Stored, PutError>> {
        Box::pin(async move {
            Err(PutError::Refused {
                reason: "this scenario copies nothing".into(),
            })
        })
    }

    fn get<'a>(
        &'a self,
        _session: &'a str,
        _path: &'a str,
        _chunk_bytes: NonZeroUsize,
    ) -> HostFuture<'a, Result<Fetch<'a>, GetError>> {
        Box::pin(async move { Err(GetError::NotFound) })
    }
}

impl HandHost for Host {
    fn status<'a>(
        &'a self,
        _session: &'a str,
    ) -> HostFuture<'a, Result<Vec<HandStatus>, HandError>> {
        Box::pin(async move {
            Ok(vec![HandStatus {
                hand: HandRef::Hosted,
                tools: HAND_TOOLS.iter().map(|tool| (*tool).to_owned()).collect(),
                phase: HandPhase::Stopped,
            }])
        })
    }

    fn run<'a>(
        &'a self,
        _session: &'a str,
        step: &'a str,
        hand: &'a HandRef,
        tool: &'a str,
        _input: Value,
        _context: ToolContext<'a>,
    ) -> HostFuture<'a, StepOutcome> {
        Box::pin(async move {
            let wire = hand.to_string();
            self.0.update(|log| {
                log.dispatched.insert(step.to_owned(), wire.clone());
            });
            StepOutcome::Done {
                output: ToolOutput::text(format!("ran {tool} on {wire}")),
                hand_ms: 1,
                usd: None,
            }
        })
    }

    fn put<'a>(
        &'a self,
        _session: &'a str,
        _hand: &'a HandRef,
        _path: &'a str,
        _chunks: Box<dyn Chunks + 'a>,
        _sha256: Sha256Digest,
    ) -> HostFuture<'a, Result<Stored, PutError>> {
        Box::pin(async move {
            Err(PutError::Refused {
                reason: "this scenario copies nothing".into(),
            })
        })
    }

    fn get<'a>(
        &'a self,
        _session: &'a str,
        _hand: &'a HandRef,
        _path: &'a str,
        _chunk_bytes: NonZeroUsize,
    ) -> HostFuture<'a, Result<Fetch<'a>, GetError>> {
        Box::pin(async move { Err(GetError::NotFound) })
    }

    fn release<'a>(
        &'a self,
        _session: &'a str,
        _hand: &'a HandRef,
    ) -> HostFuture<'a, Result<(), HandError>> {
        Box::pin(async move { Ok(()) })
    }
}

impl RouterClock for Still {
    fn now_ms(&self) -> u64 {
        0
    }
}

impl Placements for Unplaced {
    fn placed(&self, _session: &str, _step: &str, _placement: Placement) {}
}

fn draw_call(rng: &mut Rng, index: u64) -> Call {
    let step = format!("step-{index}");
    let tool = HAND_TOOLS[rng.pick(HAND_TOOLS.len())];
    let wire = if rng.chance(EXPLICIT_PPM) {
        WIRES[rng.pick(WIRES.len())]
    } else {
        AUTO
    };
    let (input, fits) = if tool == BASH && !rng.chance(NO_SCRIPT_PPM) {
        let pieces: Vec<Piece> = (0..rng.range(PIECES.start, PIECES.end))
            .map(|_| Piece::draw(rng))
            .collect();
        let script = pieces
            .iter()
            .enumerate()
            .map(|(index, piece)| piece.text(index))
            .collect::<Vec<_>>()
            .join(["; ", " && ", " | "][rng.pick(3)]);
        (
            json!({ SCRIPT: script }),
            pieces.iter().all(|piece| piece.here()),
        )
    } else if tool == BASH {
        (json!({}), false)
    } else {
        (json!({ "path": format!("file{index}") }), true)
    };
    let runs_here = WORKSPACE_TOOLS.contains(&tool) && fits;
    let expected = match wire {
        AUTO if runs_here => Some("workspace"),
        AUTO => Some("hosted"),
        "workspace" if WORKSPACE_TOOLS.contains(&tool) => Some("workspace"),
        "workspace" => None,
        named => Some(named),
    };
    Call {
        step,
        wire,
        tool,
        input,
        expected,
    }
}

fn simulate(seed: u64, mutation: HandAutoMutation) -> Outcome {
    hands_sim::mutate(mutation.rule());
    let mut rng = Rng::derive(seed, "hand_auto");
    let log = Arc::new(Atom::new(Log::default()));
    let router = Router::new(
        Arc::new(Space(Arc::clone(&log))),
        Arc::new(Host(Arc::clone(&log))),
        Arc::new(Still),
        Arc::new(Unplaced),
        Policy { idle_ms: IDLE_MS },
    )
    .with_auto(Auto { shells: SHELLS });
    let count = rng.range(STEPS.start, STEPS.end);
    let calls: Vec<Call> = (0..count).map(|index| draw_call(&mut rng, index)).collect();
    let mut stats = HandAutoStats {
        seeds: 1,
        ..HandAutoStats::default()
    };
    let mut wrong = None;
    let mut digest = fnv(FNV_OFFSET, &seed.to_le_bytes());
    let mut expected_placed: BTreeMap<Target, Placed> = BTreeMap::new();
    for call in &calls {
        let context = ToolContext::new("", SESSION, &call.step, &[], usize::MAX);
        let decision = router.decide(call.wire, call.tool, &call.input);
        block_on(router.route(call.wire, call.tool, call.input.clone(), context));
        let landed = log.read(|log| log.dispatched.get(&call.step).cloned());
        digest = fnv(digest, call.step.as_bytes());
        digest = fnv(digest, landed.as_deref().unwrap_or("-").as_bytes());
        let auto = call.wire == AUTO;
        if auto {
            stats.auto += 1;
        } else {
            stats.explicit += 1;
        }
        match landed.as_deref() {
            Some("workspace") if auto => stats.auto_workspace += 1,
            Some(_) if auto => stats.auto_hosted += 1,
            None => stats.refused += 1,
            Some(_) => {}
        }
        if let Some(target) = landed.as_deref().and_then(|wire| Target::parse(wire).ok()) {
            let placed = expected_placed.entry(target).or_default();
            if auto {
                placed.auto += 1;
            } else {
                placed.explicit += 1;
            }
        }
        if wrong.is_none() && landed.as_deref() != call.expected {
            let why = decision.map_or_else(|error| error.to_string(), |found| found.to_string());
            wrong = Some(format!(
                "{} {} via {} with {}: landed on {:?}, expected {:?} ({why})",
                call.step, call.tool, call.wire, call.input, landed, call.expected
            ));
        }
    }
    let seen = router.stats();
    if wrong.is_none() && seen.placed != expected_placed {
        wrong = Some(format!(
            "placement counts {:?} differ from dispatches {:?}",
            seen.placed, expected_placed
        ));
    }
    let dispatched = calls.len() as u64 - stats.refused;
    let reasons: u64 = seen.reasons.values().sum();
    if wrong.is_none() && reasons != dispatched {
        wrong = Some(format!(
            "reason counts sum to {reasons}, but {dispatched} calls were dispatched"
        ));
    }
    hands_sim::mutate(HandRule::None);
    Outcome {
        digest,
        wrong,
        shape: format!("calls={} auto={}", calls.len(), stats.auto),
        stats,
    }
}

fn run_seed(seed: u64, mutation: HandAutoMutation) -> Outcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        hands_sim::mutate(HandRule::None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        Outcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: HandAutoStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: HandAutoMutation,
    twice: bool,
    verbose: bool,
    totals: &mut HandAutoStats,
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

pub fn replay(seed: u64, mutation: HandAutoMutation) -> String {
    let mut command = format!("nanocode-sim --scenario hand_auto --seed {seed}");
    if mutation != HandAutoMutation::None {
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
    mutation: HandAutoMutation,
    totals: &HandAutoStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=hand_auto mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        HandAutoMutation::None if failed == 0 => ExitCode::SUCCESS,
        HandAutoMutation::None => ExitCode::FAILURE,
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
