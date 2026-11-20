use std::sync::atomic::{AtomicU8, Ordering};

/// Seeded defects the simulator's plog scenario must catch, one canary each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mutation {
    None,
    /// The store admits a batch whatever its fence epoch (two owners write one partition).
    Unfenced,
    /// The store writes only the first row of a multi-row append.
    Partial,
    /// The store overwrites a row whose seq already exists instead of rejecting the append.
    Overwrite,
    /// Two appends of one batch with the same seq both go to the store.
    BatchDuplicates,
    /// The second committed append of a batch rings no watcher.
    MissedRing,
    /// Load starts after the latest snapshot instead of at it.
    SnapshotSkipped,
    /// Prune ignores reader cursors.
    PruneIgnoresCursors,
    /// A lost acknowledgement is answered as a seq conflict, so the caller re-runs the step.
    LostAckRejected,
}

impl Mutation {
    pub const ALL: [Self; 8] = [
        Self::Unfenced,
        Self::Partial,
        Self::Overwrite,
        Self::BatchDuplicates,
        Self::MissedRing,
        Self::SnapshotSkipped,
        Self::PruneIgnoresCursors,
        Self::LostAckRejected,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unfenced => "unfenced",
            Self::Partial => "partial",
            Self::Overwrite => "overwrite",
            Self::BatchDuplicates => "batch_duplicates",
            Self::MissedRing => "missed_ring",
            Self::SnapshotSkipped => "snapshot_skipped",
            Self::PruneIgnoresCursors => "prune_ignores_cursors",
            Self::LostAckRejected => "lost_ack_rejected",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        std::iter::once(Self::None)
            .chain(Self::ALL)
            .find(|mutation| mutation.name() == name)
    }
}

static MUTATION: AtomicU8 = AtomicU8::new(Mutation::None as u8);

pub fn mutate(mutation: Mutation) {
    MUTATION.store(mutation as u8, Ordering::SeqCst);
}

#[must_use]
pub fn mutated(mutation: Mutation) -> bool {
    MUTATION.load(Ordering::Relaxed) == mutation as u8
}
