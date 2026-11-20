use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mutation {
    None,
    IgnoresBudget,
    ForgetsIntent,
    SkipsRank,
    LeavesIdle,
    StrandsWork,
}

impl Mutation {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::IgnoresBudget,
        Self::ForgetsIntent,
        Self::SkipsRank,
        Self::LeavesIdle,
        Self::StrandsWork,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::IgnoresBudget => "dispatcher_ignores_budget",
            Self::ForgetsIntent => "dispatcher_forgets_intent",
            Self::SkipsRank => "dispatcher_skips_rank",
            Self::LeavesIdle => "dispatcher_leaves_idle",
            Self::StrandsWork => "dispatcher_strands_work",
        }
    }
}

static MUTATION: AtomicU8 = AtomicU8::new(Mutation::None as u8);

pub fn mutate(mutation: Mutation) {
    MUTATION.store(mutation as u8, Ordering::SeqCst);
}

pub fn mutated(mutation: Mutation) -> bool {
    MUTATION.load(Ordering::Relaxed) == mutation as u8
}
