use std::{collections::BTreeMap, num::NonZeroU64, pin::pin, sync::Arc};

use nanocode_code_mode::embedded::HandCalls;
use nanocode_cost_fold::Placement;
use nanocode_sync::{AtomMap, Notify};
use nanocode_tool_contracts::{ToolContext, ToolOutput, ToolOutputWire};
use serde_json::{Value, json};

use crate::{
    HandError, HandHost, HandPhase, HandRef, HostFuture, StepOutcome, Target, TargetError,
    Workspace,
    auto::{AUTO, Auto, Decision, Origin, decide},
    sim::{self, Mutation},
};

pub const UNCERTAIN: &str = "UNCERTAIN_OUTCOME";
pub const REFUSED: &str = "TARGET_REFUSED";
pub const WORKSPACE: &str = "workspace";

pub trait Clock: Send + Sync + 'static {
    fn now_ms(&self) -> u64;
}

pub trait Placements: Send + Sync + 'static {
    fn placed(&self, session: &str, step: &str, placement: Placement);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Policy {
    pub idle_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Placed {
    pub explicit: u64,
    pub auto: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Stats {
    pub placed: BTreeMap<Target, Placed>,
    pub reasons: BTreeMap<&'static str, u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Use {
    Busy { holds: NonZeroU64 },
    Idle { since_ms: u64 },
    Releasing,
}

#[derive(Clone)]
enum Step {
    Running,
    Finished { wire: Option<Arc<str>> },
}

pub struct Released {
    pub session: Box<str>,
    pub hand: HandRef,
    pub result: Result<(), HandError>,
}

type UseKey = (Box<str>, HandRef);
type StepKey = (Box<str>, Box<str>);

pub struct Router {
    workspace: Arc<dyn Workspace>,
    host: Arc<dyn HandHost>,
    clock: Arc<dyn Clock>,
    placements: Arc<dyn Placements>,
    policy: Policy,
    auto: Auto,
    placed: AtomMap<Target, Placed>,
    reasons: AtomMap<&'static str, u64>,
    uses: AtomMap<UseKey, Use>,
    steps: AtomMap<StepKey, Step>,
    released: Notify,
}

impl Router {
    #[must_use]
    pub fn new(
        workspace: Arc<dyn Workspace>,
        host: Arc<dyn HandHost>,
        clock: Arc<dyn Clock>,
        placements: Arc<dyn Placements>,
        policy: Policy,
    ) -> Self {
        Self {
            workspace,
            host,
            clock,
            placements,
            policy,
            auto: Auto::default(),
            placed: AtomMap::new(),
            reasons: AtomMap::new(),
            uses: AtomMap::new(),
            steps: AtomMap::new(),
            released: Notify::new(),
        }
    }

    #[must_use]
    pub const fn with_auto(mut self, auto: Auto) -> Self {
        self.auto = auto;
        self
    }

    pub fn decide(&self, hand: &str, tool: &str, input: &Value) -> Result<Decision, TargetError> {
        let explicit = match hand {
            "" | AUTO => None,
            wire => Some(Target::parse(wire)?),
        };
        let explicit = explicit.filter(|_| !sim::mutated(Mutation::AutoMovesExplicit));
        Ok(decide(
            explicit,
            tool,
            input,
            &self.auto,
            self.workspace.as_ref(),
        ))
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            placed: self.placed.update(|map| {
                map.iter()
                    .map(|(target, placed)| (target.clone(), *placed))
                    .collect()
            }),
            reasons: self.reasons.update(|map| {
                map.iter()
                    .map(|(reason, count)| (*reason, *count))
                    .collect()
            }),
        }
    }

    pub async fn route(
        &self,
        hand: &str,
        tool: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> ToolOutput {
        let decision = match self.decide(hand, tool, &input) {
            Ok(decision) => decision,
            Err(error) => return refused(hand, &error.to_string()),
        };
        let origin = if hand.is_empty() || hand == AUTO {
            Origin::Auto
        } else {
            Origin::Explicit
        };
        let at = Placing {
            origin,
            reason: decision.reason.name(),
        };
        match decision.target {
            Target::Workspace => self.on_workspace(tool, input, context, at).await,
            Target::Hand(hand) => self.on_hand(&hand, tool, input, context, at).await,
        }
    }

    fn count(&self, target: &Target, at: Placing) {
        let target = match (target, at.origin) {
            (Target::Workspace, Origin::Auto) if sim::mutated(Mutation::CountsAutoAsHosted) => {
                Target::Hand(HandRef::Hosted)
            }
            (target, _) => target.clone(),
        };
        self.placed.update(|map| {
            let placed = map.entry(target.clone()).or_default();
            match at.origin {
                Origin::Explicit => placed.explicit = placed.explicit.saturating_add(1),
                Origin::Auto => placed.auto = placed.auto.saturating_add(1),
            }
        });
        self.reasons.update(|map| {
            let count = map.entry(at.reason).or_default();
            *count = count.saturating_add(1);
        });
    }

    async fn on_workspace(
        &self,
        tool: &str,
        input: Value,
        context: ToolContext<'_>,
        at: Placing,
    ) -> ToolOutput {
        if !self.workspace.runs(tool) && !sim::mutated(Mutation::SkipsToolCheck) {
            return refused(WORKSPACE, &format!("the workspace cannot run `{tool}`"));
        }
        let (session, step) = (context.session_id(), context.call_id());
        if !self.claim(session, step) {
            return dispatched_before(WORKSPACE, step);
        }
        self.count(&Target::Workspace, at);
        let started_ms = self.clock.now_ms();
        let output = self.workspace.run(session, tool, input, context).await;
        let placement = Placement {
            hand: WORKSPACE.to_owned(),
            hand_ms: self.clock.now_ms().saturating_sub(started_ms),
            hand_nano_usd: None,
        };
        self.placements.placed(session, step, placement);
        self.finish(session, step, output)
    }

    async fn on_hand(
        &self,
        hand: &HandRef,
        tool: &str,
        input: Value,
        context: ToolContext<'_>,
        at: Placing,
    ) -> ToolOutput {
        let (session, step) = (context.session_id(), context.call_id());
        let wire = hand.to_string();
        let statuses = match self.host.status(session).await {
            Ok(statuses) => statuses,
            Err(error) => return refused(&wire, &format!("could not read the Hands: {error}")),
        };
        let Some(status) = statuses.iter().find(|status| status.hand == *hand) else {
            return refused(&wire, &format!("`{wire}` is not attached to this session"));
        };
        if let HandPhase::Lost { reason } = &status.phase {
            return refused(&wire, &format!("`{wire}` was lost: {reason}"));
        }
        if !status.tools.iter().any(|name| name == tool) && !sim::mutated(Mutation::SkipsToolCheck)
        {
            if sim::mutated(Mutation::ForwardsRefused) && self.workspace.runs(tool) {
                return self.on_workspace(tool, input, context, at).await;
            }
            return refused(&wire, &format!("`{wire}` cannot run `{tool}`"));
        }
        if !self.claim(session, step) {
            return dispatched_before(&wire, step);
        }
        self.count(&Target::Hand(hand.clone()), at);
        self.hold(session, hand).await;
        let replay = sim::mutated(Mutation::ReplaysUncertain).then(|| input.clone());
        let outcome = self
            .host
            .run(session, step, hand, tool, input, context)
            .await;
        let outcome = match (outcome, replay) {
            (StepOutcome::Uncertain { .. }, Some(input)) => {
                self.host
                    .run(session, step, hand, tool, input, context)
                    .await
            }
            (outcome, _) => outcome,
        };
        self.unhold(session, hand);
        let placement = outcome.placement(hand);
        let skip = sim::mutated(Mutation::PlacementSkipsUncertain)
            && matches!(outcome, StepOutcome::Uncertain { .. });
        if let Some(placement) = placement.filter(|_| !skip) {
            self.placements.placed(session, step, placement);
        }
        match outcome {
            StepOutcome::Done { output, .. } => self.finish(session, step, output),
            StepOutcome::Uncertain { hand, reason, .. } => {
                self.steps
                    .insert(key(session, step), Step::Finished { wire: None });
                uncertain(&hand.to_string(), &reason)
            }
            StepOutcome::Refused { reason } => {
                self.steps
                    .remove_if(&key(session, step), |step| matches!(step, Step::Running));
                refused(&wire, &reason)
            }
        }
    }

    pub async fn recover(&self, hand: &str, context: ToolContext<'_>) -> Option<ToolOutput> {
        let step = self
            .steps
            .get(&key(context.session_id(), context.call_id()))?;
        match step {
            Step::Finished { wire: Some(text) } => {
                let wire = serde_json::from_str::<ToolOutputWire>(&text).ok()?;
                ToolOutput::from_wire(wire).ok()
            }
            Step::Finished { wire: None } if sim::mutated(Mutation::RecoverReruns) => {
                if let Ok(Target::Hand(target)) = Target::parse(hand) {
                    let (session, step) = (context.session_id(), context.call_id());
                    let _ = self
                        .host
                        .run(session, step, &target, "", Value::Null, context)
                        .await;
                }
                Some(uncertain(hand, "its Hand was lost mid-step"))
            }
            Step::Finished { wire: None } | Step::Running => Some(uncertain(
                hand,
                "its outcome was not recorded before the call moved",
            )),
        }
    }

    pub async fn release_idle(&self) -> Vec<Released> {
        let now_ms = self.clock.now_ms();
        let idle_ms = self.policy.idle_ms;
        let mut due: Vec<UseKey> = self.uses.update(|map| {
            let due: Vec<UseKey> = map
                .iter()
                .filter(|(_, held)| match held {
                    _ if sim::mutated(Mutation::NeverReleases) => false,
                    Use::Busy { .. } => sim::mutated(Mutation::ReleasesBusy),
                    Use::Idle { .. } if sim::mutated(Mutation::ReleasesEarly) => true,
                    Use::Idle { since_ms } => now_ms.saturating_sub(*since_ms) >= idle_ms,
                    Use::Releasing => false,
                })
                .map(|(key, _)| key.clone())
                .collect();
            for key in &due {
                map.insert(key.clone(), Use::Releasing);
            }
            due
        });
        due.sort();
        let mut released = Vec::with_capacity(due.len());
        for (session, hand) in due {
            let result = self.host.release(&session, &hand).await;
            let use_key = (session.clone(), hand.clone());
            self.uses
                .remove_if(&use_key, |held| *held == Use::Releasing);
            self.released.notify_waiters();
            released.push(Released {
                session,
                hand,
                result,
            });
        }
        released
    }

    #[must_use]
    pub fn next_release_ms(&self) -> Option<u64> {
        let idle_ms = self.policy.idle_ms;
        self.uses.update(|map| {
            map.values()
                .filter_map(|held| match held {
                    Use::Idle { since_ms } => Some(since_ms.saturating_add(idle_ms)),
                    Use::Busy { .. } | Use::Releasing => None,
                })
                .min()
        })
    }

    pub fn end_session(&self, session: &str) {
        self.steps
            .update(|map| map.retain(|(owner, _), _| owner.as_ref() != session));
    }

    async fn hold(&self, session: &str, hand: &HandRef) {
        let use_key: UseKey = (session.into(), hand.clone());
        loop {
            let mut notified = pin!(self.released.notified());
            notified.as_mut().enable();
            let held = self.uses.update(|map| {
                let holds = match map.get(&use_key) {
                    Some(Use::Releasing) => return false,
                    Some(Use::Busy { holds }) => holds.saturating_add(1),
                    Some(Use::Idle { .. }) | None => NonZeroU64::MIN,
                };
                map.insert(use_key.clone(), Use::Busy { holds });
                true
            });
            if held {
                return;
            }
            notified.await;
        }
    }

    fn unhold(&self, session: &str, hand: &HandRef) {
        let use_key: UseKey = (session.into(), hand.clone());
        let now_ms = self.clock.now_ms();
        self.uses.update(|map| {
            let next = match map.get(&use_key) {
                Some(Use::Busy { holds }) => match NonZeroU64::new(holds.get() - 1) {
                    Some(holds) => Use::Busy { holds },
                    None => Use::Idle { since_ms: now_ms },
                },
                Some(held) => *held,
                None => Use::Idle { since_ms: now_ms },
            };
            map.insert(use_key.clone(), next);
        });
    }

    fn claim(&self, session: &str, step: &str) -> bool {
        let step_key = key(session, step);
        self.steps.update(|map| {
            if map.contains_key(&step_key) {
                return false;
            }
            map.insert(step_key.clone(), Step::Running);
            true
        })
    }

    fn finish(&self, session: &str, step: &str, mut output: ToolOutput) -> ToolOutput {
        let receipts = output.take_receipts();
        let (output, wire) = match output.into_wire() {
            Ok(wire) => {
                let text = serde_json::to_string(&wire).ok().map(Arc::<str>::from);
                match ToolOutput::from_wire(wire) {
                    Ok(output) => (output, text),
                    Err(error) => (
                        ToolOutput::error(format!("failed to decode tool result: {error}")),
                        None,
                    ),
                }
            }
            Err(error) => (
                ToolOutput::error(format!("failed to encode tool result: {error}")),
                None,
            ),
        };
        self.steps
            .insert(key(session, step), Step::Finished { wire });
        receipts
            .into_iter()
            .fold(output, |output, receipt| output.with_receipt(receipt))
    }
}

impl HandCalls for Router {
    fn here(&self, _hand: &str) -> bool {
        false
    }

    fn auto(&self, tool: &str) -> bool {
        self.workspace.runs(tool) && !sim::mutated(Mutation::AutoOff)
    }

    fn call<'a>(
        &'a self,
        hand: &'a str,
        name: &'a str,
        input: Value,
        context: ToolContext<'a>,
    ) -> HostFuture<'a, ToolOutput> {
        Box::pin(self.route(hand, name, input, context))
    }

    fn recover<'a>(
        &'a self,
        hand: &'a str,
        context: ToolContext<'a>,
    ) -> HostFuture<'a, Option<ToolOutput>> {
        Box::pin(Self::recover(self, hand, context))
    }
}

#[derive(Clone, Copy)]
struct Placing {
    origin: Origin,
    reason: &'static str,
}

fn key(session: &str, step: &str) -> StepKey {
    (session.into(), step.into())
}

#[must_use]
pub fn refused(hand: &str, reason: &str) -> ToolOutput {
    let message = format!("{reason}; nothing was dispatched");
    ToolOutput::error(message.clone()).with_structured_result(json!({
        "code": REFUSED,
        "hand": hand,
        "message": message,
    }))
}

#[must_use]
pub fn uncertain(hand: &str, reason: &str) -> ToolOutput {
    let message = format!(
        "The call on `{hand}` may or may not have run ({reason}). Do not assume it did not run or automatically repeat it."
    );
    ToolOutput::error(message.clone()).with_structured_result(json!({
        "code": UNCERTAIN,
        "hand": hand,
        "reason": reason,
        "message": message,
    }))
}

fn dispatched_before(hand: &str, step: &str) -> ToolOutput {
    uncertain(hand, &format!("step `{step}` was already dispatched once"))
}
