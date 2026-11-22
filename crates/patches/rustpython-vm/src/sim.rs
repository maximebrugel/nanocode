use core::cell::Cell;
use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mutation {
    None,
    KeepsLeftoverBits,
    /// An interpreter dropped without finalize() skips finalizing.
    DropSkipsFinalize,
    /// A type's subclass list never drops its dead entries.
    SubclassesKeepDead,
    /// python-capi builds and leaks its module definitions for every interpreter.
    DefsPerInterpreter,
    /// A type's subclass list is scanned for dead entries on every push.
    PrunesEveryPush,
    /// A type's subclass list is scanned whenever its length is a power of two.
    PrunesAtPowersOfTwo,
    /// A dropped interpreter runs Python code while it finalizes.
    DropRunsCode,
    /// A panic while a dropped interpreter finalizes leaves its drop.
    DropLetsPanicOut,
    /// Drop finalizes again after an explicit finalize().
    DropFinalizesTwice,
    /// Drop finalizes while unwinding from a panic.
    DropFinalizesWhileUnwinding,
    HidesFunctionBuiltins,
}

static MUTATION: AtomicU8 = AtomicU8::new(Mutation::None as u8);

pub fn mutate(mutation: Mutation) {
    MUTATION.store(mutation as u8, Ordering::SeqCst);
}

pub fn mutated(mutation: Mutation) -> bool {
    MUTATION.load(Ordering::Relaxed) == mutation as u8
}

std::thread_local! {
    static SUBCLASS_SCANS: Cell<u64> = const { Cell::new(0) };
}

/// Subclass-list entries scanned for dead ones on this thread so far.
pub fn subclass_scans() -> u64 {
    SUBCLASS_SCANS.get()
}

pub(crate) fn note_subclass_scan(entries: usize) {
    SUBCLASS_SCANS.set(SUBCLASS_SCANS.get() + entries as u64);
}

std::thread_local! {
    static SHUT_DOWNS: Cell<u64> = const { Cell::new(0) };
}

/// Interpreters finalized on this thread so far, explicitly or by drop.
pub fn shut_downs() -> u64 {
    SHUT_DOWNS.get()
}

pub(crate) fn note_shut_down() {
    SHUT_DOWNS.set(SHUT_DOWNS.get() + 1);
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
