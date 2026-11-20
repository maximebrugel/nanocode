use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mutation {
    None,
    SettleDropsDeletes,
    SettleKeepsStart,
    ResumeSkipsFirst,
    FloorIgnored,
    RebaseSkipped,
    SnapshotSkipsRow,
    SnapshotAtHead,
    OutlineLeaks,
    EncodeEveryFrame,
    RebaseDropsTouched,
}

static MUTATION: AtomicU8 = AtomicU8::new(Mutation::None as u8);
thread_local! {
    // Per thread, so the simulator's parallel seed workers each count their own encodes.
    static ENCODES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub fn mutate(mutation: Mutation) {
    MUTATION.store(mutation as u8, Ordering::SeqCst);
}

pub fn mutated(mutation: Mutation) -> bool {
    MUTATION.load(Ordering::Relaxed) == mutation as u8
}

pub fn encodes() -> u64 {
    ENCODES.get()
}

pub(crate) fn count_encode() {
    ENCODES.set(ENCODES.get() + 1);
}
