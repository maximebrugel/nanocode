use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A byte buffer that only grows: a byte once written never changes, so shared views
/// of the written bytes stay valid while whoever holds their end writes past it.
pub struct Grow {
    data: Box<[UnsafeCell<MaybeUninit<u8>>]>,
    /// The end of the bytes claimed for writing: every append moves it first.
    claimed: AtomicUsize,
    /// The end of the written bytes: every byte below it is initialized and never
    /// written again.
    written: AtomicUsize,
}

// SAFETY: bytes below `written` are only read, and each byte at or past it is written
// by the one append that claimed it, before `written` moves past it.
unsafe impl Sync for Grow {}

impl Grow {
    /// `bytes` in a buffer with room for `capacity` bytes in all.
    #[must_use]
    pub fn with_capacity(bytes: &[u8], capacity: usize) -> Self {
        let capacity = capacity.max(bytes.len());
        let mut data: Vec<UnsafeCell<MaybeUninit<u8>>> = Vec::with_capacity(capacity);
        // SAFETY: `MaybeUninit` needs no initialization, and `UnsafeCell<MaybeUninit<u8>>`
        // has the layout of `u8`, so the first `bytes.len()` slots take `bytes`.
        unsafe {
            data.set_len(capacity);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.as_mut_ptr().cast::<u8>(), bytes.len());
        }
        Self {
            data: data.into_boxed_slice(),
            claimed: AtomicUsize::new(bytes.len()),
            written: AtomicUsize::new(bytes.len()),
        }
    }

    /// The written bytes.
    pub fn as_slice(&self) -> &[u8] {
        let written = self.written.load(Ordering::Acquire);
        // SAFETY: the bytes below `written` are initialized and no append writes them
        // again; `UnsafeCell<MaybeUninit<u8>>` has the layout of `u8`.
        unsafe { std::slice::from_raw_parts(UnsafeCell::raw_get(self.data.as_ptr()).cast::<u8>(), written) }
    }

    /// Writes `more` at `at` and returns true when `at` is the end of the written
    /// bytes and the buffer has room for `more`; else changes nothing and returns false.
    pub fn append(&self, at: usize, more: &[u8]) -> bool {
        let Some(end) = at.checked_add(more.len()) else {
            return false;
        };
        if end > self.data.len()
            || self
                .claimed
                .compare_exchange(at, end, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
        {
            return false;
        }
        // SAFETY: the exchange claimed `at..end` for this call alone, and no slice covers
        // it, since `written` stays at most `at` until the store below.
        unsafe {
            std::ptr::copy_nonoverlapping(
                more.as_ptr(),
                UnsafeCell::raw_get(self.data.as_ptr().add(at)).cast::<u8>(),
                more.len(),
            );
        }
        self.written.store(end, Ordering::Release);
        true
    }
}
