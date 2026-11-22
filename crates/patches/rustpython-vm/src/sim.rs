use core::cell::Cell;
use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mutation {
    None,
    KeepsLeftoverBits,
}

static MUTATION: AtomicU8 = AtomicU8::new(Mutation::None as u8);

pub fn mutate(mutation: Mutation) {
    MUTATION.store(mutation as u8, Ordering::SeqCst);
}

pub fn mutated(mutation: Mutation) -> bool {
    MUTATION.load(Ordering::Relaxed) == mutation as u8
}

pub const UNWOUND: &str =
    "nanocode-sim unwound a collection between taking its candidates and splitting them";

std::thread_local! {
    static UNWIND_NEXT_COLLECTION: Cell<bool> = const { Cell::new(false) };
}

pub fn unwind_next_collection() {
    UNWIND_NEXT_COLLECTION.set(true);
}

pub(crate) fn unwind_if_asked() {
    if UNWIND_NEXT_COLLECTION.replace(false) {
        std::panic::panic_any(UNWOUND);
    }
}
