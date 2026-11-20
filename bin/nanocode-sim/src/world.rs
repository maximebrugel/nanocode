use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use nanocode_sync::Slot;

use crate::{
    executor::Clock,
    rng::{FNV_OFFSET, fnv},
};

#[derive(Clone, Copy, Debug, Default)]
pub struct WorldCounts {
    pub requests: u64,
    pub effects: u64,
    pub acked: u64,
    pub spawned: u64,
    pub submitted: u64,
    pub waits: u64,
    pub faults: u64,
    pub connections: u64,
    pub reuses: u64,
    pub child_crashes: u64,
    pub child_model_calls: u64,
    pub hedges: u64,
}

#[derive(Default)]
struct State {
    effects: BTreeMap<String, u64>,
    sent: BTreeMap<String, BTreeSet<u64>>,
    acked: BTreeMap<String, BTreeMap<u64, String>>,
    seen: BTreeMap<String, BTreeSet<(u64, u64)>>,
    cancelled: BTreeMap<String, BTreeSet<u64>>,
    receipts: BTreeSet<String>,
    durable_receipts: BTreeMap<String, u64>,
    epoch: u64,
    holds_receipts: bool,
    crash_pending: bool,
    violations: Vec<String>,
    counts: WorldCounts,
    digest: u64,
    verbose: bool,
    lines: Vec<String>,
}

pub struct World {
    state: Slot<State>,
    clock: Arc<Clock>,
}

impl World {
    pub fn new(clock: Arc<Clock>, verbose: bool) -> Arc<Self> {
        Arc::new(Self {
            state: Slot::new(Some(State {
                digest: FNV_OFFSET,
                verbose,
                ..State::default()
            })),
            clock,
        })
    }

    fn with<R>(&self, change: impl FnOnce(&mut State) -> R) -> R {
        let mut state = self
            .state
            .take()
            .expect("the simulator polls one task at a time, so the world is never taken twice");
        let result = change(&mut state);
        assert!(self.state.put(state).is_none(), "the world has one home");
        result
    }

    pub fn trace(&self, line: &str) {
        let now_ns = self.clock.now_ns();
        self.with(|state| {
            let stamped = format!("{now_ns} {line}");
            state.digest = fnv(fnv(state.digest, stamped.as_bytes()), b"\n");
            if state.verbose {
                println!("{stamped}");
            }
            state.lines.push(stamped);
        });
    }

    pub fn wrong(&self, message: String) {
        self.trace(&format!("WRONG {message}"));
        self.with(|state| state.violations.push(message));
    }

    pub fn count(&self, change: impl FnOnce(&mut WorldCounts)) {
        self.with(|state| change(&mut state.counts));
    }

    pub fn effect(&self, key: &str) {
        let runs = self.with(|state| {
            state.counts.effects += 1;
            let runs = state.effects.entry(key.to_owned()).or_insert(0);
            *runs += 1;
            *runs
        });
        self.trace(&format!("effect {key} run {runs}"));
        if runs > 1 {
            self.wrong(format!("effect {key} ran {runs} times"));
        }
    }

    pub fn sent(&self, label: &str, message: u64) {
        self.with(|state| {
            state
                .sent
                .entry(label.to_owned())
                .or_default()
                .insert(message);
        });
    }

    pub fn was_sent(&self, label: &str, message: u64) -> bool {
        self.with(|state| {
            state
                .sent
                .get(label)
                .is_some_and(|sent| sent.contains(&message))
        })
    }

    pub fn ack(&self, label: &str, turn: u64, text: &str) {
        self.trace(&format!("ack {label} turn {turn} {text}"));
        self.with(|state| {
            state.counts.acked += 1;
            state
                .acked
                .entry(label.to_owned())
                .or_default()
                .insert(turn, text.to_owned());
        });
    }

    pub fn acked(&self, label: &str) -> BTreeMap<u64, String> {
        self.with(|state| state.acked.get(label).cloned().unwrap_or_default())
    }

    pub fn see(&self, label: &str, turn: u64) {
        self.with(|state| {
            let epoch = state.epoch;
            state
                .seen
                .entry(label.to_owned())
                .or_default()
                .insert((epoch, turn));
        });
    }

    pub fn saw(&self, label: &str, turn: u64) -> bool {
        self.with(|state| {
            state
                .seen
                .get(label)
                .is_some_and(|seen| seen.contains(&(state.epoch, turn)))
        })
    }

    pub fn cancelled(&self, label: &str, turn: u64) {
        self.trace(&format!("cancelled {label} turn {turn}"));
        self.with(|state| {
            state
                .cancelled
                .entry(label.to_owned())
                .or_default()
                .insert(turn);
        });
    }

    pub fn cancelled_turns(&self, label: &str) -> BTreeSet<u64> {
        self.with(|state| state.cancelled.get(label).cloned().unwrap_or_default())
    }

    pub fn receipt(&self, label: &str) {
        if self.with(|state| state.receipts.insert(label.to_owned())) {
            self.trace(&format!("receipt {label}"));
        }
    }

    pub fn durable_receipt(&self, label: &str) {
        self.with(|state| {
            let epoch = state.epoch;
            state
                .durable_receipts
                .entry(label.to_owned())
                .or_insert(epoch);
        });
    }

    pub fn durable_receipt_epoch(&self, label: &str) -> Option<u64> {
        self.with(|state| state.durable_receipts.get(label).copied())
    }

    pub fn begin_epoch(&self, epoch: u64) {
        self.with(|state| state.epoch = epoch);
    }

    pub fn epoch(&self) -> u64 {
        self.with(|state| state.epoch)
    }

    pub fn receipted(&self, label: &str) -> bool {
        self.with(|state| state.receipts.contains(label))
    }

    pub fn hold_receipts(&self) {
        self.with(|state| state.holds_receipts = true);
    }

    pub fn crash_pending(&self, pending: bool) {
        self.with(|state| state.crash_pending = pending);
    }

    pub fn holds_receipt(&self) -> bool {
        self.with(|state| state.holds_receipts && state.crash_pending)
    }

    pub fn first_violation(&self) -> Option<String> {
        self.with(|state| state.violations.first().cloned())
    }

    pub fn lines(&self) -> Vec<String> {
        self.with(|state| std::mem::take(&mut state.lines))
    }

    pub fn digest(&self) -> u64 {
        self.with(|state| state.digest)
    }

    pub fn counts(&self) -> WorldCounts {
        self.with(|state| state.counts)
    }
}
