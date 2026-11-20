use crate::mirror::Action;
use crate::sim::{Mutation, mutated};
use nanocode_machine::{Illegal, Machine, machine};
use serde::{Deserialize, Serialize};

machine! {
    pub enum Stage {
        Pending = "pending",
        Attempted = "attempted",
        Throttled = "throttled",
        Delivered = "delivered",
        Removed = "removed",
        GivenUp = "given_up",
    }
    moves {
        Pending => [Attempted, Delivered, GivenUp],
        Attempted => [Throttled, Delivered, GivenUp],
        Throttled => [Attempted, Delivered, GivenUp],
        Delivered => [Removed],
        GivenUp => [Delivered],
        Removed => [],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Signal {
    Attempt,
    Refund,
    Receipt,
    Remove,
    GiveUp,
}

impl Signal {
    pub const ALL: [Self; 5] = [
        Self::Attempt,
        Self::Refund,
        Self::Receipt,
        Self::Remove,
        Self::GiveUp,
    ];

    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Attempt => "attempt",
            Self::Refund => "refund",
            Self::Receipt => "receipt",
            Self::Remove => "remove",
            Self::GiveUp => "give_up",
        }
    }
}

pub const STAYS: &[(Stage, Signal)] = &[
    (Stage::Attempted, Signal::Attempt),
    (Stage::Attempted, Signal::Refund),
    (Stage::Delivered, Signal::Attempt),
    (Stage::Delivered, Signal::Refund),
    (Stage::Delivered, Signal::Receipt),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Held {
    Live,
    Removed,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct Slot {
    pub attempts: u64,
    pub throttled: u64,
    pub copy: Option<Held>,
    pub dead: bool,
    pub orphan: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    Orphan,
    Sweep,
    Retry { exhausted: bool },
    Exhausted,
    Send,
}

impl Slot {
    #[must_use]
    pub const fn counted(self) -> u64 {
        self.attempts.saturating_sub(self.throttled)
    }

    #[must_use]
    pub const fn stage(self) -> Stage {
        match (self.copy, self.dead) {
            (Some(Held::Removed), _) => Stage::Removed,
            (Some(Held::Live), _) => Stage::Delivered,
            (None, true) => Stage::GivenUp,
            (None, false) if self.counted() > 0 => Stage::Attempted,
            (None, false) if self.throttled > 0 => Stage::Throttled,
            (None, false) => Stage::Pending,
        }
    }

    pub fn on(self, signal: Signal) -> Result<Self, Illegal<Stage>> {
        let from = self.stage();
        if from == Stage::Throttled && mutated(Mutation::DeliveryThrottledStuck) {
            return Err(Illegal { from, to: from });
        }
        let mut next = self;
        match signal {
            Signal::Attempt => next.attempts += 1,
            Signal::Refund if self.counted() > 0 => next.throttled += 1,
            Signal::Refund => {
                return Err(Illegal {
                    from,
                    to: Stage::Throttled,
                });
            }
            Signal::Receipt
                if next.copy.is_none()
                    && !(from == Stage::GivenUp && mutated(Mutation::DeliveryDeadStays)) =>
            {
                next.copy = Some(Held::Live);
                next.dead = false;
            }
            Signal::Receipt => {}
            Signal::Remove => next.copy = next.copy.map(|_| Held::Removed),
            Signal::GiveUp if from == Stage::Pending && !self.orphan => {
                return Err(Illegal {
                    from,
                    to: Stage::GivenUp,
                });
            }
            Signal::GiveUp => next.dead = true,
        }
        let to = next.stage();
        let stays = STAYS.contains(&(from, signal))
            || (from, signal) == (Stage::Removed, Signal::Receipt)
                && mutated(Mutation::DeliveryRemovedEdits)
            || (from, signal) == (Stage::GivenUp, Signal::GiveUp)
                && mutated(Mutation::DeliveryDeadRepeats);
        match to == from {
            true if stays => Ok(next),
            false if from.may(to) => Ok(next),
            _ => Err(Illegal { from, to }),
        }
    }

    #[must_use]
    pub const fn plan(self, action: &Action, limit: u64) -> Plan {
        let counted = self.counted();
        match action {
            Action::Orphan => Plan::Orphan,
            Action::Sweep => Plan::Sweep,
            Action::Post { .. } if counted > 0 => Plan::Retry {
                exhausted: counted >= limit,
            },
            Action::Post { .. } if counted >= limit => Plan::Exhausted,
            Action::Post { .. } | Action::Edit { .. } | Action::Delete { .. } => Plan::Send,
        }
    }
}
