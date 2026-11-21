use core::cell::Cell;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicIsize, AtomicPtr, AtomicU64, AtomicUsize, Ordering, fence};

use super::class::SMALL_BYTES_MAX;
use super::os::{self, check};

pub const SEGMENT_BYTES: usize = 4 << 20;
pub const SLICE_BYTES: usize = 64 << 10;
pub const SLICES: usize = SEGMENT_BYTES / SLICE_BYTES;
pub const HEADER_SLICES: usize = 1;
pub const SLICES_FREE_ALL: u64 = u64::MAX << HEADER_SLICES;
pub const OWNER_ABANDONED: u64 = u64::MAX;
pub const KIND_SMALL: usize = 1;
pub const KIND_LARGE: usize = 2;
const LARGE_HEADER_BYTES: usize = 4096;
const ABANDONED_SLOTS: usize = 1024;
const SEGMENT_CACHE_SLOTS: usize = 16;
const LARGE_CACHE_SLOTS: usize = 16;
const LARGE_CACHE_BYTES_MAX: usize = 8 << 20;
const LARGE_CACHE_FIT: usize = 2;
const LARGE_UNITS_LIMIT: usize = SEGMENT_BYTES;
const ADDRESS_BITS: usize = 48;
const REGISTRY_SEGMENTS: usize = 1 << (ADDRESS_BITS - SEGMENT_BYTES.trailing_zeros() as usize);
const REGISTRY_WORDS: usize = REGISTRY_SEGMENTS / u64::BITS as usize;
const SUMMARY_WORDS: usize = REGISTRY_WORDS / u64::BITS as usize;

pub static ABANDONED: Slots<ABANDONED_SLOTS> = Slots::new();
static SEGMENT_CACHE: Slots<SEGMENT_CACHE_SLOTS> = Slots::new();
static LARGE_CACHE: Slots<LARGE_CACHE_SLOTS> = Slots::new();
static LARGE_CACHED_BYTES: AtomicUsize = AtomicUsize::new(0);
static REGISTERED: [AtomicU64; REGISTRY_WORDS] = [const { AtomicU64::new(0) }; REGISTRY_WORDS];
static REGISTERED_SUMMARY: [AtomicU64; SUMMARY_WORDS] =
    [const { AtomicU64::new(0) }; SUMMARY_WORDS];
static UNREGISTERED: AtomicUsize = AtomicUsize::new(0);
static READERS: AtomicUsize = AtomicUsize::new(0);

pub struct Shared<T: Atomic>(T::Cell);

pub trait Atomic: Copy {
    type Cell;
    fn load(cell: &Self::Cell) -> Self;
    fn store(cell: &Self::Cell, value: Self);
}

impl Atomic for usize {
    type Cell = AtomicUsize;
    #[inline]
    fn load(cell: &AtomicUsize) -> Self {
        cell.load(Ordering::Relaxed)
    }
    #[inline]
    fn store(cell: &AtomicUsize, value: Self) {
        cell.store(value, Ordering::Relaxed);
    }
}

impl Atomic for u64 {
    type Cell = AtomicU64;
    #[inline]
    fn load(cell: &AtomicU64) -> Self {
        cell.load(Ordering::Relaxed)
    }
    #[inline]
    fn store(cell: &AtomicU64, value: Self) {
        cell.store(value, Ordering::Relaxed);
    }
}

impl<T: Atomic> Shared<T> {
    #[inline]
    pub fn get(&self) -> T {
        T::load(&self.0)
    }

    #[inline]
    pub fn set(&self, value: T) {
        T::store(&self.0, value);
    }
}

pub struct Block {
    pub next: *mut Self,
}

pub struct Page {
    pub free: Cell<*mut Block>,
    pub thread_free: AtomicPtr<Block>,
    pub bump: Cell<usize>,
    pub end: Cell<usize>,
    pub block_bytes: Shared<usize>,
    pub used: Shared<usize>,
    pub head: Shared<usize>,
    pub span: Shared<usize>,
    pub class: Shared<usize>,
    pub next: Cell<*mut Self>,
    pub prev: Cell<*mut Self>,
    pub zero: Cell<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Any,
    Zero(usize),
}

#[repr(C)]
pub struct Segment {
    pub kind: Cell<usize>,
    pub mapping_bytes: Cell<usize>,
    pub owner: AtomicU64,
    pub free_slices: Shared<u64>,
    pub dirty_slices: Shared<u64>,
    pub next: Cell<*mut Self>,
    pub prev: Cell<*mut Self>,
    pub pages: [Page; SLICES],
}

const _: () = {
    assert!(size_of::<Segment>() <= HEADER_SLICES * SLICE_BYTES);
    assert!(SLICES == u64::BITS as usize);
    assert!(LARGE_HEADER_BYTES >= 2 * size_of::<usize>());
};

pub struct Slots<const N: usize> {
    slots: [AtomicUsize; N],
    filled: AtomicIsize,
}

impl<const N: usize> Slots<N> {
    const fn new() -> Self {
        Self {
            slots: [const { AtomicUsize::new(0) }; N],
            filled: AtomicIsize::new(0),
        }
    }

    pub fn put(&self, value: usize) -> bool {
        check(value != 0, "a slot holds a nonzero value");
        for slot in &self.slots {
            if slot.load(Ordering::Relaxed) == 0
                && slot
                    .compare_exchange(0, value, Ordering::Release, Ordering::Relaxed)
                    .is_ok()
            {
                self.filled.fetch_add(1, Ordering::Release);
                return true;
            }
        }
        false
    }

    pub fn filled(&self) -> usize {
        usize::try_from(self.filled.load(Ordering::Relaxed)).unwrap_or(0)
    }

    pub fn take(&self, fits: impl Fn(usize) -> bool) -> usize {
        if self.filled.load(Ordering::Acquire) <= 0 {
            return 0;
        }
        for slot in &self.slots {
            let value = slot.load(Ordering::Relaxed);
            if value != 0
                && fits(value)
                && slot
                    .compare_exchange(value, 0, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                self.filled.fetch_sub(1, Ordering::Relaxed);
                return value;
            }
        }
        0
    }
}

impl Page {
    #[inline]
    pub fn take(&self) -> *mut u8 {
        let block = self.free.get();
        if !block.is_null() {
            self.free.set(unsafe { (*block).next });
            self.used.set(self.used.get() + 1);
            return block.cast();
        }
        let bump = self.bump.get();
        let next = bump + self.block_bytes.get();
        if next <= self.end.get() {
            self.bump.set(next);
            self.used.set(self.used.get() + 1);
            return bump as *mut u8;
        }
        null_mut()
    }

    #[inline]
    pub fn take_filled(&self, fill: Fill) -> *mut u8 {
        let zero = self.free.get().is_null() && self.zero.get();
        let block = self.take();
        if let Fill::Zero(bytes) = fill
            && !zero
            && !block.is_null()
        {
            unsafe { block.write_bytes(0, bytes) };
        }
        block
    }

    #[inline]
    pub unsafe fn free_local(&self, block: *mut Block) -> usize {
        let used = self.used.get();
        check(used > 0, "a block freed by its page's owner was handed out");
        unsafe { (*block).next = self.free.get() };
        self.free.set(block);
        self.used.set(used - 1);
        used - 1
    }

    #[inline]
    pub unsafe fn free_remote(&self, block: *mut Block) {
        let mut head = self.thread_free.load(Ordering::Relaxed);
        loop {
            unsafe { (*block).next = head };
            match self.thread_free.compare_exchange_weak(
                head,
                block,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(seen) => head = seen,
            }
        }
    }

    pub fn capacity(&self) -> usize {
        self.span.get() * SLICE_BYTES / self.block_bytes.get()
    }

    pub fn collect(&self) {
        if self.thread_free.load(Ordering::Relaxed).is_null() {
            return;
        }
        let list = self.thread_free.swap(null_mut(), Ordering::Acquire);
        if list.is_null() {
            return;
        }
        let capacity = self.capacity();
        let mut tail = list;
        let mut count = 1;
        unsafe {
            while !(*tail).next.is_null() {
                tail = (*tail).next;
                count += 1;
                check(
                    count <= capacity,
                    "a page's freed list is no longer than its blocks",
                );
            }
            (*tail).next = self.free.get();
        }
        self.free.set(list);
        let used = self.used.get();
        check(
            count <= used,
            "every block freed by another thread was handed out",
        );
        self.used.set(used - count);
    }

    pub fn span_mask(&self) -> u64 {
        span_mask(self.head.get(), self.span.get())
    }
}

pub const fn span_mask(start: usize, span: usize) -> u64 {
    ((1u64 << span) - 1) << start
}

pub const fn find_run(free: u64, span: usize) -> Option<usize> {
    let mut runs = free;
    let mut shift = 1;
    while shift < span {
        runs &= free >> shift;
        shift += 1;
    }
    if runs == 0 {
        None
    } else {
        Some(runs.trailing_zeros() as usize)
    }
}

#[inline]
pub fn segment_of(pointer: *mut u8) -> *mut Segment {
    (pointer.addr() & !(SEGMENT_BYTES - 1)) as *mut Segment
}

#[inline]
pub unsafe fn page_of(segment: *mut Segment, pointer: *mut u8) -> *mut Page {
    let slice = (pointer.addr() - segment.addr()) / SLICE_BYTES;
    unsafe {
        let head = (*segment).pages[slice].head.get();
        (&raw mut (*segment).pages[head]).cast()
    }
}

pub unsafe fn acquire(owner: u64) -> *mut Segment {
    let cached = SEGMENT_CACHE.take(|_| true);
    let base = if cached == 0 {
        unsafe { os::map_aligned(SEGMENT_BYTES, SEGMENT_BYTES) }
    } else {
        cached
    };
    if base == 0 {
        return null_mut();
    }
    let segment = base as *mut Segment;
    unsafe {
        (*segment).kind.set(KIND_SMALL);
        (*segment).mapping_bytes.set(SEGMENT_BYTES);
        (*segment).owner.store(owner, Ordering::Relaxed);
        (*segment).free_slices.set(SLICES_FREE_ALL);
        (*segment).dirty_slices.set(0);
        (*segment).next.set(null_mut());
        (*segment).prev.set(null_mut());
        if cached == 0 {
            register(base);
            return segment;
        }
        for page in &(*segment).pages {
            check(
                page.thread_free.load(Ordering::Relaxed).is_null(),
                "a reused segment has no pending frees",
            );
            page.block_bytes.set(0);
            page.used.set(0);
            page.free.set(null_mut());
        }
    }
    segment
}

pub unsafe fn purge_dirty(segment: *mut Segment) -> u64 {
    unsafe {
        let free = (*segment).free_slices.get();
        let dirty = (*segment).dirty_slices.get() & free;
        let mut rest = dirty;
        while rest != 0 {
            let start = rest.trailing_zeros() as usize;
            let run = (!(rest >> start)).trailing_zeros() as usize;
            os::purge(segment.addr() + start * SLICE_BYTES, run * SLICE_BYTES);
            rest &= !span_mask(start, run);
        }
        (*segment)
            .dirty_slices
            .set((*segment).dirty_slices.get() & !dirty);
        dirty
    }
}

pub unsafe fn release(segment: *mut Segment) {
    unsafe {
        check(
            (*segment).free_slices.get() == SLICES_FREE_ALL,
            "a released segment holds no pages",
        );
        purge_dirty(segment);
        if !SEGMENT_CACHE.put(segment.addr()) {
            unregister(segment.addr());
            os::unmap(segment.addr(), SEGMENT_BYTES);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Contents {
    Zero,
    Dirty,
}

pub unsafe fn large_allocate(bytes: usize, align: usize) -> *mut u8 {
    unsafe { large_allocate_with(bytes, align).0 }
}

pub unsafe fn large_allocate_with(bytes: usize, align: usize) -> (*mut u8, Contents) {
    let offset = align.max(LARGE_HEADER_BYTES);
    if offset >= SEGMENT_BYTES {
        return (null_mut(), Contents::Dirty);
    }
    let page = os::page_bytes();
    let Some(need) = offset
        .checked_add(bytes)
        .and_then(|end| end.checked_next_multiple_of(page))
    else {
        return (null_mut(), Contents::Dirty);
    };
    let units = need / page;
    let cached = if units < LARGE_UNITS_LIMIT {
        LARGE_CACHE.take(|value| {
            let have = value & (SEGMENT_BYTES - 1);
            have >= units && have <= units * LARGE_CACHE_FIT
        })
    } else {
        0
    };
    let (base, mapping_bytes, contents) = if cached == 0 {
        (
            unsafe { os::map_aligned(need, SEGMENT_BYTES) },
            need,
            Contents::Zero,
        )
    } else {
        let have = (cached & (SEGMENT_BYTES - 1)) * page;
        LARGE_CACHED_BYTES.fetch_sub(have, Ordering::Relaxed);
        (cached & !(SEGMENT_BYTES - 1), have, Contents::Dirty)
    };
    if base == 0 {
        return (null_mut(), Contents::Dirty);
    }
    let segment = base as *mut Segment;
    unsafe {
        (*segment).kind.set(KIND_LARGE);
        (*segment).mapping_bytes.set(mapping_bytes);
    }
    super::stats::taken(mapping_bytes);
    super::stats::large_taken(mapping_bytes);
    ((base + offset) as *mut u8, contents)
}

pub unsafe fn large_free(segment: *mut Segment) {
    let bytes = unsafe { (*segment).mapping_bytes.get() };
    super::stats::given(bytes);
    super::stats::large_given(bytes);
    let units = bytes / os::page_bytes();
    if units < LARGE_UNITS_LIMIT {
        let before = LARGE_CACHED_BYTES.fetch_add(bytes, Ordering::Relaxed);
        if before + bytes <= LARGE_CACHE_BYTES_MAX && LARGE_CACHE.put(segment.addr() | units) {
            return;
        }
        LARGE_CACHED_BYTES.fetch_sub(bytes, Ordering::Relaxed);
    }
    unsafe { os::unmap(segment.addr(), bytes) };
}

pub unsafe fn large_resize(segment: *mut Segment, pointer: *mut u8, bytes_new: usize) -> bool {
    let offset = pointer.addr() - segment.addr();
    let mapping_bytes = unsafe { (*segment).mapping_bytes.get() };
    let capacity = mapping_bytes - offset;
    if bytes_new <= capacity {
        return bytes_new > capacity / LARGE_CACHE_FIT && bytes_new > SMALL_BYTES_MAX;
    }
    let Some(need) = offset
        .checked_add(bytes_new)
        .and_then(|end| end.checked_next_multiple_of(os::page_bytes()))
    else {
        return false;
    };
    if unsafe { os::grow_in_place(segment.addr(), mapping_bytes, need) } {
        unsafe { (*segment).mapping_bytes.set(need) };
        super::stats::taken(need - mapping_bytes);
        super::stats::large_grown(need - mapping_bytes);
        return true;
    }
    false
}

pub fn segments_cached() -> usize {
    SEGMENT_CACHE.filled()
}

pub fn large_cached() -> (usize, usize) {
    (
        LARGE_CACHE.filled(),
        LARGE_CACHED_BYTES.load(Ordering::Relaxed),
    )
}

pub fn unregistered() -> usize {
    UNREGISTERED.load(Ordering::Relaxed)
}

const fn registry_index(base: usize) -> Option<usize> {
    let index = base / SEGMENT_BYTES;
    if index < REGISTRY_SEGMENTS {
        Some(index)
    } else {
        None
    }
}

fn register(base: usize) {
    let Some(index) = registry_index(base) else {
        UNREGISTERED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let word = index / u64::BITS as usize;
    REGISTERED_SUMMARY[word / u64::BITS as usize]
        .fetch_or(1 << (word % u64::BITS as usize), Ordering::Release);
    REGISTERED[word].fetch_or(1 << (index % u64::BITS as usize), Ordering::Release);
}

fn unregister(base: usize) {
    let Some(index) = registry_index(base) else {
        UNREGISTERED.fetch_sub(1, Ordering::Relaxed);
        return;
    };
    let word = index / u64::BITS as usize;
    REGISTERED[word].fetch_and(!(1 << (index % u64::BITS as usize)), Ordering::Relaxed);
    fence(Ordering::SeqCst);
    while READERS.load(Ordering::Relaxed) != 0 {
        std::thread::yield_now();
    }
    fence(Ordering::Acquire);
}

pub fn each_registered(mut visit: impl FnMut(&Segment)) {
    READERS.fetch_add(1, Ordering::Relaxed);
    fence(Ordering::SeqCst);
    for (summary_index, summary) in REGISTERED_SUMMARY.iter().enumerate() {
        let mut words = summary.load(Ordering::Acquire);
        while words != 0 {
            let word = summary_index * u64::BITS as usize + words.trailing_zeros() as usize;
            words &= words - 1;
            let mut bits = REGISTERED[word].load(Ordering::Acquire);
            while bits != 0 {
                let index = word * u64::BITS as usize + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                visit(unsafe { &*((index * SEGMENT_BYTES) as *const Segment) });
            }
        }
    }
    READERS.fetch_sub(1, Ordering::Release);
}
