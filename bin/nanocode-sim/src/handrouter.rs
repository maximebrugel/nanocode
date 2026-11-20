use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
    ops::Range,
    process::ExitCode,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use nanocode_code_mode::embedded::HandCalls;
use nanocode_code_mode::{ToolContext, ToolOutput, ToolOutputBody};
use nanocode_hands::{
    Chunks, Clock, Fetch, GetError, HandError, HandHost, HandPhase, HandProvider, HandRef,
    HandSize, HandStatus, HostFuture, MemoryProvider, Placement, Placements, Policy, ProviderError,
    PutError, REFUSED, Router, Sha256Digest, StepOutcome, Stored, Target, UNCERTAIN, Workspace,
    sim::{self as hands_sim, Mutation as HandRule},
};
use nanocode_sync::{Atom, OnceCell, block_on};
use serde_json::{Value, json};

use crate::rng::{FNV_OFFSET, Rng, fnv};

const SESSION: &str = "session";
const WORKSPACE_TOOLS: [&str; 3] = ["read", "write", "grep"];
const HAND_TOOLS: [&str; 5] = ["read", "write", "exec", "build", "xcodebuild"];
const BAD_WIRES: [&str; 4] = ["pool:", "cloud", "hosted:x", "native"];
const PPM: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandRouterMutation {
    None,
    ForwardsRefused,
    SkipsToolCheck,
    ReplaysUncertain,
    RecoverReruns,
    ReleasesBusy,
    ReleasesEarly,
    NeverReleases,
    PlacementSkipsUncertain,
    StopsUnknown,
    ListKeepsStopped,
}

impl HandRouterMutation {
    const ALL: [Self; 11] = [
        Self::None,
        Self::ForwardsRefused,
        Self::SkipsToolCheck,
        Self::ReplaysUncertain,
        Self::RecoverReruns,
        Self::ReleasesBusy,
        Self::ReleasesEarly,
        Self::NeverReleases,
        Self::PlacementSkipsUncertain,
        Self::StopsUnknown,
        Self::ListKeepsStopped,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ForwardsRefused => "forwards_refused",
            Self::SkipsToolCheck => "skips_tool_check",
            Self::ReplaysUncertain => "replays_uncertain",
            Self::RecoverReruns => "recover_reruns",
            Self::ReleasesBusy => "releases_busy",
            Self::ReleasesEarly => "releases_early",
            Self::NeverReleases => "never_releases",
            Self::PlacementSkipsUncertain => "placement_skips_uncertain",
            Self::StopsUnknown => "stops_unknown",
            Self::ListKeepsStopped => "list_keeps_stopped",
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
            Self::ForwardsRefused => HandRule::ForwardsRefused,
            Self::SkipsToolCheck => HandRule::SkipsToolCheck,
            Self::ReplaysUncertain => HandRule::ReplaysUncertain,
            Self::RecoverReruns => HandRule::RecoverReruns,
            Self::ReleasesBusy => HandRule::ReleasesBusy,
            Self::ReleasesEarly => HandRule::ReleasesEarly,
            Self::NeverReleases => HandRule::NeverReleases,
            Self::PlacementSkipsUncertain => HandRule::PlacementSkipsUncertain,
            Self::StopsUnknown => HandRule::StopsUnknown,
            Self::ListKeepsStopped => HandRule::ListKeepsStopped,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HandRouterStats {
    pub seeds: u64,
    pub calls: u64,
    pub refused: u64,
    pub done: u64,
    pub uncertain: u64,
    pub retries: u64,
    pub recovers: u64,
    pub starts: u64,
    pub releases: u64,
    pub midstep_releases: u64,
}

impl HandRouterStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.calls += other.calls;
        self.refused += other.refused;
        self.done += other.done;
        self.uncertain += other.uncertain;
        self.retries += other.retries;
        self.recovers += other.recovers;
        self.starts += other.starts;
        self.releases += other.releases;
        self.midstep_releases += other.midstep_releases;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Stopped,
    Running { since_ms: u64 },
    Lost,
}

#[derive(Clone)]
struct FakeHand {
    tools: Vec<String>,
    phase: Phase,
    in_flight: u32,
    last_end_ms: u64,
    used_since_release: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ran {
    Done { text: String },
    Uncertain,
}

#[derive(Clone, Default)]
struct Log {
    dispatches: BTreeMap<String, Vec<String>>,
    ran: BTreeMap<String, Ran>,
    expected_placements: BTreeMap<String, Placement>,
    placements: Vec<(String, Placement)>,
    starts: BTreeSet<HandRef>,
    start_count: u64,
    releases: u64,
    midstep_releases: u64,
    violations: Vec<String>,
    trace: Vec<String>,
}

struct World {
    now_ms: AtomicU64,
    idle_ms: u64,
    loss_ppm: u64,
    midstep_ppm: u64,
    rng: Atom<Rng>,
    hands: Atom<BTreeMap<HandRef, FakeHand>>,
    log: Atom<Log>,
    router: OnceCell<Weak<Router>>,
}

impl World {
    fn now(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }

    fn advance(&self, ms: u64) {
        self.now_ms.fetch_add(ms, Ordering::SeqCst);
    }

    fn draw(&self, bound: u64) -> u64 {
        self.rng.update(|rng| rng.below(bound.max(1)))
    }

    fn chance(&self, ppm: u64) -> bool {
        self.rng.update(|rng| rng.chance(ppm))
    }

    fn note(&self, event: String) {
        self.log.update(|log| log.trace.push(event.clone()));
    }

    fn violate(&self, violation: String) {
        self.log
            .update(|log| log.violations.push(violation.clone()));
    }

    fn dispatched(&self, step: &str, target: &str) {
        self.log.update(|log| {
            log.dispatches
                .entry(step.to_owned())
                .or_default()
                .push(target.to_owned());
        });
    }

    fn start(&self, hand: &HandRef) -> Result<(), String> {
        let now_ms = self.now();
        let started = self.hands.update(|hands| {
            let Some(fake) = hands.get_mut(hand) else {
                return Err(format!("{hand} is not attached"));
            };
            match fake.phase {
                Phase::Lost => Err(format!("{hand} is lost")),
                Phase::Running { .. } => {
                    fake.in_flight += 1;
                    fake.used_since_release = true;
                    Ok(false)
                }
                Phase::Stopped => {
                    fake.phase = Phase::Running { since_ms: now_ms };
                    fake.in_flight += 1;
                    fake.used_since_release = true;
                    Ok(true)
                }
            }
        })?;
        if started {
            self.log.update(|log| {
                log.starts.insert(hand.clone());
                log.start_count += 1;
            });
            self.note(format!("start {hand}"));
        }
        Ok(())
    }

    fn end(&self, hand: &HandRef) {
        let now_ms = self.now();
        self.hands.update(|hands| {
            if let Some(fake) = hands.get_mut(hand) {
                fake.in_flight = fake.in_flight.saturating_sub(1);
                fake.last_end_ms = now_ms;
            }
        });
    }

    fn midstep(&self) {
        if !self.chance(self.midstep_ppm) {
            return;
        }
        let Some(router) = self.router.get().and_then(Weak::upgrade) else {
            return;
        };
        self.advance(self.idle_ms + 1 + self.draw(self.idle_ms));
        let released = block_on(router.release_idle());
        self.log
            .update(|log| log.midstep_releases += released.len() as u64);
    }
}

struct FakeClock(Arc<World>);

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.now()
    }
}

struct FakePlacements(Arc<World>);

impl Placements for FakePlacements {
    fn placed(&self, _session: &str, step: &str, placement: Placement) {
        self.0
            .log
            .update(|log| log.placements.push((step.to_owned(), placement.clone())));
    }
}

struct FakeHost(Arc<World>);

impl HandHost for FakeHost {
    fn status<'a>(
        &'a self,
        _session: &'a str,
    ) -> HostFuture<'a, Result<Vec<HandStatus>, HandError>> {
        Box::pin(async move {
            Ok(self.0.hands.read(|hands| {
                hands
                    .iter()
                    .map(|(hand, fake)| HandStatus {
                        hand: hand.clone(),
                        tools: fake.tools.clone(),
                        phase: match fake.phase {
                            Phase::Stopped => HandPhase::Stopped,
                            Phase::Running { since_ms } => HandPhase::Running { since_ms },
                            Phase::Lost => HandPhase::Lost {
                                reason: "the machine stopped answering".into(),
                            },
                        },
                    })
                    .collect()
            }))
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
            let world = &self.0;
            let wire = hand.to_string();
            world.dispatched(step, &wire);
            let runs = world.hands.read(|hands| {
                hands
                    .get(hand)
                    .map(|fake| fake.tools.iter().any(|name| name == tool))
            });
            if runs != Some(true) {
                world.violate(format!(
                    "step {step}: {wire} was sent {tool}, which it cannot run"
                ));
            }
            if let Err(reason) = world.start(hand) {
                world.violate(format!("step {step}: dispatched to {wire}: {reason}"));
                return StepOutcome::Refused {
                    reason: reason.into(),
                };
            }
            world.midstep();
            let hand_ms = 1 + world.draw(world.idle_ms);
            world.advance(hand_ms);
            let usd = world.chance(PPM / 2).then(|| world.draw(PPM));
            let lost = world.chance(world.loss_ppm);
            if lost {
                world.hands.update(|hands| {
                    if let Some(fake) = hands.get_mut(hand) {
                        fake.phase = Phase::Lost;
                    }
                });
            }
            world.end(hand);
            let placement = Placement {
                hand: wire.clone(),
                hand_ms,
                hand_nano_usd: usd,
            };
            let text = format!("ran {tool} on {wire} for {step}");
            world.log.update(|log| {
                log.expected_placements
                    .insert(step.to_owned(), placement.clone());
                log.ran.insert(
                    step.to_owned(),
                    if lost {
                        Ran::Uncertain
                    } else {
                        Ran::Done { text: text.clone() }
                    },
                );
            });
            world.note(format!("run {step} {wire} {tool} lost={lost} ms={hand_ms}"));
            if lost {
                return StepOutcome::Uncertain {
                    hand: hand.clone(),
                    reason: "the Hand stopped answering mid-step".into(),
                    hand_ms,
                    usd,
                };
            }
            StepOutcome::Done {
                output: ToolOutput::text(text),
                hand_ms,
                usd,
            }
        })
    }

    fn put<'a>(
        &'a self,
        _session: &'a str,
        hand: &'a HandRef,
        _path: &'a str,
        _chunks: Box<dyn Chunks + 'a>,
        _sha256: Sha256Digest,
    ) -> HostFuture<'a, Result<Stored, PutError>> {
        Box::pin(async move {
            self.0
                .violate(format!("{hand} was sent a file, which no step does"));
            Err(PutError::Refused {
                reason: "this Hand takes no files".into(),
            })
        })
    }

    fn get<'a>(
        &'a self,
        _session: &'a str,
        hand: &'a HandRef,
        _path: &'a str,
        _chunk_bytes: NonZeroUsize,
    ) -> HostFuture<'a, Result<Fetch<'a>, GetError>> {
        Box::pin(async move {
            self.0
                .violate(format!("{hand} was asked for a file, which no step does"));
            Err(GetError::Refused {
                reason: "this Hand serves no files".into(),
            })
        })
    }

    fn release<'a>(
        &'a self,
        _session: &'a str,
        hand: &'a HandRef,
    ) -> HostFuture<'a, Result<(), HandError>> {
        Box::pin(async move {
            let world = &self.0;
            let now_ms = world.now();
            let idle_ms = world.idle_ms;
            let problem = world.hands.update(|hands| {
                let fake = hands.get_mut(hand)?;
                let problem = if fake.in_flight > 0 {
                    Some(format!("{hand} released with {} operations in flight", fake.in_flight))
                } else if now_ms.saturating_sub(fake.last_end_ms) < idle_ms {
                    Some(format!(
                        "{hand} released {} ms after its last use, before the {idle_ms} ms idle period",
                        now_ms.saturating_sub(fake.last_end_ms)
                    ))
                } else {
                    None
                };
                fake.phase = Phase::Stopped;
                fake.used_since_release = false;
                problem
            });
            if let Some(problem) = problem {
                world.violate(problem);
            }
            world.log.update(|log| log.releases += 1);
            world.note(format!("release {hand} at {now_ms}"));
            Ok(())
        })
    }
}

struct FakeWorkspace(Arc<World>);

impl Workspace for FakeWorkspace {
    fn runs(&self, tool: &str) -> bool {
        WORKSPACE_TOOLS.contains(&tool)
    }

    fn run<'a>(
        &'a self,
        _session: &'a str,
        tool: &'a str,
        _input: Value,
        context: ToolContext<'a>,
    ) -> HostFuture<'a, ToolOutput> {
        Box::pin(async move {
            let world = &self.0;
            let step = context.call_id();
            world.dispatched(step, "workspace");
            if !WORKSPACE_TOOLS.contains(&tool) {
                world.violate(format!(
                    "step {step}: the workspace was sent {tool}, which it cannot run"
                ));
            }
            let hand_ms = world.draw(world.idle_ms);
            world.advance(hand_ms);
            let text = format!("ran {tool} on workspace for {step}");
            world.log.update(|log| {
                log.expected_placements.insert(
                    step.to_owned(),
                    Placement {
                        hand: "workspace".to_owned(),
                        hand_ms,
                        hand_nano_usd: None,
                    },
                );
                log.ran
                    .insert(step.to_owned(), Ran::Done { text: text.clone() });
            });
            world.note(format!("run {step} workspace {tool}"));
            ToolOutput::text(text)
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
            self.0
                .violate("the workspace was sent a file, which no step does".to_owned());
            Err(PutError::Refused {
                reason: "this workspace takes no files".into(),
            })
        })
    }

    fn get<'a>(
        &'a self,
        _session: &'a str,
        _path: &'a str,
        _chunk_bytes: NonZeroUsize,
    ) -> HostFuture<'a, Result<Fetch<'a>, GetError>> {
        Box::pin(async move {
            self.0
                .violate("the workspace was asked for a file, which no step does".to_owned());
            Err(GetError::Refused {
                reason: "this workspace serves no files".into(),
            })
        })
    }
}

fn code(output: &ToolOutput) -> Option<String> {
    output.structured_result()["code"]
        .as_str()
        .map(str::to_owned)
}

fn text(output: &ToolOutput) -> Option<&str> {
    match &output.output {
        ToolOutputBody::Text(text) => Some(text),
        ToolOutputBody::Content(_) => None,
    }
}

struct Call {
    wire: String,
    tool: &'static str,
    step: String,
}

pub struct HandRouterOutcome {
    pub digest: u64,
    pub wrong: Option<String>,
    pub stats: HandRouterStats,
    pub shape: String,
}

fn runnable(world: &World, wire: &str, tool: &str) -> bool {
    match Target::parse(wire) {
        Err(_) => false,
        Ok(Target::Workspace) => WORKSPACE_TOOLS.contains(&tool),
        Ok(Target::Hand(hand)) => world.hands.read(|hands| {
            hands.get(&hand).is_some_and(|fake| {
                fake.phase != Phase::Lost && fake.tools.iter().any(|name| name == tool)
            })
        }),
    }
}

pub fn simulate(seed: u64, mutation: HandRouterMutation) -> HandRouterOutcome {
    let mut rng = Rng::derive(seed, "hand_router");
    let idle_ms = rng.range(2, 60);
    let loss_ppm = rng.range(PPM / 20, PPM / 4);
    let midstep_ppm = rng.range(PPM / 20, PPM / 3);
    let pool = ["hosted", "pool:a", "pool:b", "native:dev", "mac"];
    let mut hands = BTreeMap::new();
    let mut absent = Vec::new();
    for wire in pool {
        let Ok(hand) = HandRef::parse(wire) else {
            continue;
        };
        if !rng.chance(PPM * 3 / 4) {
            absent.push(wire.to_owned());
            continue;
        }
        let tools: Vec<String> = HAND_TOOLS
            .iter()
            .filter(|_| rng.chance(PPM / 2))
            .map(|tool| (*tool).to_owned())
            .collect();
        hands.insert(
            hand,
            FakeHand {
                tools,
                phase: Phase::Stopped,
                in_flight: 0,
                last_end_ms: 0,
                used_since_release: false,
            },
        );
    }
    let attached: Vec<String> = hands.keys().map(ToString::to_string).collect();
    let world = Arc::new(World {
        now_ms: AtomicU64::new(idle_ms),
        idle_ms,
        loss_ppm,
        midstep_ppm,
        rng: Atom::new(Rng::derive(seed, "hand_router_host")),
        hands: Atom::new(hands),
        log: Atom::new(Log::default()),
        router: OnceCell::new(),
    });
    let router = Arc::new(Router::new(
        Arc::new(FakeWorkspace(Arc::clone(&world))),
        Arc::new(FakeHost(Arc::clone(&world))),
        Arc::new(FakeClock(Arc::clone(&world))),
        Arc::new(FakePlacements(Arc::clone(&world))),
        Policy { idle_ms },
    ));
    let _ = world.router.set(Arc::downgrade(&router));
    let shape = format!(
        "hands={} idle_ms={idle_ms} loss_ppm={loss_ppm} midstep_ppm={midstep_ppm}",
        attached.join(",")
    );
    hands_sim::mutate(mutation.rule());
    let mut stats = HandRouterStats {
        seeds: 1,
        ..HandRouterStats::default()
    };
    let mut wrong: Option<String> = None;
    let mut named: BTreeSet<HandRef> = BTreeSet::new();
    let mut calls: Vec<Call> = Vec::new();
    let ops = rng.range(20, 120);
    let mut targets: Vec<String> = vec!["workspace".to_owned()];
    targets.extend(attached.iter().cloned());
    for op in 0..ops {
        if wrong.is_some() {
            break;
        }
        let kind = rng.below(100);
        let mut fail = |message: String| {
            if wrong.is_none() {
                wrong = Some(format!("op {op}: {message}"));
            }
        };
        if kind < 62 {
            let wire = match rng.below(10) {
                0 => BAD_WIRES[rng.pick(BAD_WIRES.len())].to_owned(),
                1 if !absent.is_empty() => absent[rng.pick(absent.len())].clone(),
                _ => targets[rng.pick(targets.len())].clone(),
            };
            let all: Vec<&'static str> = HAND_TOOLS
                .iter()
                .chain(WORKSPACE_TOOLS.iter())
                .copied()
                .collect();
            let tool = all[rng.pick(all.len())];
            let step = format!("step-{op}");
            if let Ok(Target::Hand(hand)) = Target::parse(&wire) {
                named.insert(hand);
            }
            let expect_run = runnable(&world, &wire, tool);
            let context = ToolContext::new("", SESSION, &step, &[], usize::MAX);
            let output = block_on(router.call(&wire, tool, json!({}), context));
            stats.calls += 1;
            let (dispatches, ran) = world.log.read(|log| {
                (
                    log.dispatches.get(&step).cloned().unwrap_or_default(),
                    log.ran.get(&step).cloned(),
                )
            });
            let result = code(&output);
            world.note(format!("call {step} {wire} {tool} -> {result:?}"));
            if dispatches.iter().any(|target| *target != wire) {
                fail(format!(
                    "step {step} named {wire} but went to {dispatches:?}"
                ));
            }
            if !expect_run {
                stats.refused += 1;
                if !dispatches.is_empty() {
                    fail(format!(
                        "step {step}: {wire} cannot run {tool}, yet it was dispatched to {dispatches:?}"
                    ));
                }
                if result.as_deref() != Some(REFUSED) {
                    fail(format!(
                        "step {step}: {wire} cannot run {tool}, yet the result is {result:?}"
                    ));
                }
                continue;
            }
            if dispatches.len() != 1 {
                fail(format!(
                    "step {step} was dispatched {} times",
                    dispatches.len()
                ));
            }
            match ran {
                Some(Ran::Done { text: expected }) => {
                    stats.done += 1;
                    if text(&output) != Some(expected.as_str()) || !output.success {
                        fail(format!(
                            "step {step}: result {:?} is not what ran: {expected}",
                            text(&output)
                        ));
                    }
                }
                Some(Ran::Uncertain) => {
                    stats.uncertain += 1;
                    if result.as_deref() != Some(UNCERTAIN) {
                        fail(format!(
                            "step {step}: its Hand was lost mid-step, yet the result is {result:?}"
                        ));
                    }
                }
                None => fail(format!("step {step}: runnable on {wire} but nothing ran")),
            }
            calls.push(Call { wire, tool, step });
        } else if kind < 72 && !calls.is_empty() {
            let call = &calls[rng.pick(calls.len())];
            let before = world
                .log
                .read(|log| log.dispatches.get(&call.step).map_or(0, Vec::len));
            let context = ToolContext::new("", SESSION, &call.step, &[], usize::MAX);
            let output = block_on(router.call(&call.wire, call.tool, json!({}), context));
            stats.retries += 1;
            let after = world
                .log
                .read(|log| log.dispatches.get(&call.step).map_or(0, Vec::len));
            world.note(format!("retry {} -> {:?}", call.step, code(&output)));
            if after != before {
                fail(format!(
                    "step {} ran again when called a second time",
                    call.step
                ));
            }
            if output.success {
                fail(format!(
                    "a second call of step {} reported success",
                    call.step
                ));
            }
        } else if kind < 82 && !calls.is_empty() {
            let call = &calls[rng.pick(calls.len())];
            let before = world
                .log
                .read(|log| log.dispatches.get(&call.step).map_or(0, Vec::len));
            let context = ToolContext::new("", SESSION, &call.step, &[], usize::MAX);
            let output = block_on(router.recover(&call.wire, context));
            stats.recovers += 1;
            let after = world
                .log
                .read(|log| log.dispatches.get(&call.step).map_or(0, Vec::len));
            let ran = world.log.read(|log| log.ran.get(&call.step).cloned());
            world.note(format!(
                "recover {} -> {:?}",
                call.step,
                output.as_ref().map(code)
            ));
            if after != before {
                fail(format!("recovering step {} dispatched it again", call.step));
            }
            match (ran, output) {
                (Some(Ran::Done { text: expected }), Some(output)) => {
                    if text(&output) != Some(expected.as_str()) {
                        fail(format!(
                            "recovering step {} gave {:?}, not {expected}",
                            call.step,
                            text(&output)
                        ));
                    }
                }
                (Some(Ran::Uncertain), Some(output)) => {
                    if code(&output).as_deref() != Some(UNCERTAIN) {
                        fail(format!(
                            "recovering uncertain step {} gave {:?}",
                            call.step,
                            code(&output)
                        ));
                    }
                }
                (ran, output) => fail(format!(
                    "recovering step {} ({ran:?}) gave {:?}",
                    call.step,
                    output.as_ref().map(code)
                )),
            }
        } else {
            world.advance(rng.below(idle_ms * 2));
            let released = block_on(router.release_idle());
            world.note(format!("tick released {}", released.len()));
        }
        let violations = world.log.read(|log| log.violations.clone());
        if let Some(violation) = violations.first() {
            fail(violation.clone());
        }
    }
    if wrong.is_none() {
        wrong = provide(&mut rng);
    }
    if wrong.is_none() {
        world.advance(idle_ms);
        block_on(router.release_idle());
        let unreleased: Vec<String> = world.hands.read(|hands| {
            hands
                .iter()
                .filter(|(_, fake)| fake.used_since_release && fake.in_flight == 0)
                .map(|(hand, _)| hand.to_string())
                .collect()
        });
        let log = world.log.read(Clone::clone);
        if let Some(violation) = log.violations.first() {
            wrong = Some(violation.clone());
        } else if !unreleased.is_empty() {
            wrong = Some(format!(
                "{unreleased:?} still held {idle_ms} ms after their last use"
            ));
        } else if let Some(hand) = log.starts.iter().find(|hand| !named.contains(*hand)) {
            wrong = Some(format!("{hand} started without any step naming it"));
        } else {
            let mut placed: BTreeMap<String, Vec<Placement>> = BTreeMap::new();
            for (step, placement) in &log.placements {
                placed
                    .entry(step.clone())
                    .or_default()
                    .push(placement.clone());
            }
            for (step, expected) in &log.expected_placements {
                let got = placed.remove(step).unwrap_or_default();
                if got != [expected.clone()] {
                    wrong = Some(format!("step {step} placed {got:?}, expected {expected:?}"));
                    break;
                }
            }
            if wrong.is_none()
                && let Some((step, extra)) = placed.into_iter().next()
            {
                wrong = Some(format!("step {step} placed {extra:?} without running"));
            }
        }
    }
    hands_sim::mutate(HandRule::None);
    let log = world.log.read(Clone::clone);
    stats.starts = log.start_count;
    stats.releases = log.releases;
    stats.midstep_releases = log.midstep_releases;
    let digest = log
        .trace
        .iter()
        .fold(FNV_OFFSET, |hash, event| fnv(hash, event.as_bytes()));
    HandRouterOutcome {
        digest,
        wrong,
        stats,
        shape,
    }
}

struct Ticks(AtomicU64);

impl Clock for Ticks {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn provide(rng: &mut Rng) -> Option<String> {
    let clock = Arc::new(Ticks(AtomicU64::new(0)));
    let hands_max = rng.range(1, 8) as usize;
    let prices = [rng.range(1, PPM), rng.range(1, PPM)];
    let provider = MemoryProvider::new(clock.clone(), hands_max, prices);
    let mut expected: BTreeMap<Box<str>, HandSize> = BTreeMap::new();
    let mut stopped: Vec<Box<str>> = Vec::new();
    for _ in 0..rng.range(4, 40) {
        clock.0.fetch_add(rng.below(1_000), Ordering::SeqCst);
        let now_ms = clock.now_ms();
        match rng.below(3) {
            0 => {
                let size = HandSize::ALL[rng.pick(HandSize::ALL.len())];
                match block_on(provider.start(size)) {
                    Ok(hand) => {
                        if expected.len() >= hands_max {
                            return Some(format!("start {size} ran past {hands_max} Hands"));
                        }
                        if hand.size != size
                            || hand.started_ms != now_ms
                            || hand.nano_usd_per_hour != prices[size as usize]
                        {
                            return Some(format!("start {size} at {now_ms} ms gave {hand:?}"));
                        }
                        if expected.insert(hand.id.clone(), size).is_some() {
                            return Some(format!("start reused id {}", hand.id));
                        }
                    }
                    Err(ProviderError::Exhausted { .. }) if expected.len() >= hands_max => {}
                    Err(error) => {
                        return Some(format!(
                            "start {size} with {} of {hands_max} running failed: {error}",
                            expected.len()
                        ));
                    }
                }
            }
            1 if !expected.is_empty() => {
                let ids: Vec<Box<str>> = expected.keys().cloned().collect();
                let id = ids[rng.pick(ids.len())].clone();
                if let Err(error) = block_on(provider.stop(&id)) {
                    return Some(format!("stop of running {id} failed: {error}"));
                }
                expected.remove(&id);
                stopped.push(id);
            }
            _ => {
                let id: Box<str> = if stopped.is_empty() || rng.chance(PPM / 2) {
                    "never-started".into()
                } else {
                    stopped[rng.pick(stopped.len())].clone()
                };
                match block_on(provider.stop(&id)) {
                    Err(ProviderError::UnknownHand { id: named }) if named == id => {}
                    other => return Some(format!("stop of unknown {id} gave {other:?}")),
                }
            }
        }
        let listed = match block_on(provider.list()) {
            Ok(listed) => listed,
            Err(error) => return Some(format!("list failed: {error}")),
        };
        let seen: BTreeMap<Box<str>, HandSize> = listed
            .into_iter()
            .map(|hand| (hand.id, hand.size))
            .collect();
        if seen != expected {
            return Some(format!("list gave {seen:?}, expected {expected:?}"));
        }
    }
    None
}

pub fn run_seed(seed: u64, mutation: HandRouterMutation) -> HandRouterOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        hands_sim::mutate(HandRule::None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        HandRouterOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: HandRouterStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: HandRouterMutation,
    twice: bool,
    verbose: bool,
    totals: &mut HandRouterStats,
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

pub fn replay(seed: u64, mutation: HandRouterMutation) -> String {
    let mut command = format!("nanocode-sim --scenario hand_router --seed {seed}");
    if mutation != HandRouterMutation::None {
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
    mutation: HandRouterMutation,
    totals: &HandRouterStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=hand_router mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        HandRouterMutation::None if failed == 0 => ExitCode::SUCCESS,
        HandRouterMutation::None => ExitCode::FAILURE,
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
