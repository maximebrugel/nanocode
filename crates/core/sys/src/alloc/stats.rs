use core::sync::atomic::{AtomicUsize, Ordering};

static MAPPED: AtomicUsize = AtomicUsize::new(0);
static IN_PAGES: AtomicUsize = AtomicUsize::new(0);
static LARGE_LIVE: AtomicUsize = AtomicUsize::new(0);
static LARGE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// What the heap holds, counted on its slow paths only (page and mapping changes), so reading it costs
/// two loads and the allocation fast path pays nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Bytes in pages that hold blocks (small allocations) plus live large mappings: what the program
    /// keeps, at page granularity. A leak grows this.
    pub allocated: usize,
    /// Bytes mapped from the OS and not yet unmapped, including free slices, cached segments and cached
    /// large mappings. Purged ranges stay mapped but are not resident, so RSS can be lower.
    pub retained: usize,
}

pub fn stats() -> Stats {
    Stats {
        allocated: IN_PAGES.load(Ordering::Relaxed),
        retained: MAPPED.load(Ordering::Relaxed),
    }
}

pub fn mapped(bytes: usize) {
    MAPPED.fetch_add(bytes, Ordering::Relaxed);
}

pub fn unmapped(bytes: usize) {
    MAPPED.fetch_sub(bytes, Ordering::Relaxed);
}

pub fn taken(bytes: usize) {
    IN_PAGES.fetch_add(bytes, Ordering::Relaxed);
}

pub fn given(bytes: usize) {
    IN_PAGES.fetch_sub(bytes, Ordering::Relaxed);
}

pub fn large_taken(bytes: usize) {
    LARGE_LIVE.fetch_add(1, Ordering::Relaxed);
    LARGE_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

pub fn large_given(bytes: usize) {
    LARGE_LIVE.fetch_sub(1, Ordering::Relaxed);
    LARGE_BYTES.fetch_sub(bytes, Ordering::Relaxed);
}

pub fn large_grown(bytes: usize) {
    LARGE_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

pub fn large() -> (usize, usize) {
    (
        LARGE_LIVE.load(Ordering::Relaxed),
        LARGE_BYTES.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use core::alloc::Layout;

    use super::super::Local;
    use super::stats;

    #[test]
    fn allocated_follows_pages_and_large_mappings() {
        let local = Local::new();
        let before = stats();
        let layout = Layout::from_size_align(1024, 8).unwrap();
        let blocks: Vec<_> = (0..4096).map(|_| local.allocate(layout).unwrap()).collect();
        let held = stats();
        assert!(
            held.allocated >= before.allocated + 4 << 20,
            "4 MiB of blocks sit in pages"
        );
        assert!(
            held.retained >= held.allocated,
            "pages sit in mapped segments"
        );
        for block in blocks {
            local.deallocate(block);
        }
        let freed = stats();
        assert!(
            freed.allocated <= before.allocated + (1 << 20),
            "freed pages leave allocated"
        );

        let large = Layout::from_size_align(16 << 20, 8).unwrap();
        let big = local.allocate(large).unwrap();
        assert!(
            stats().allocated >= freed.allocated + (16 << 20),
            "a large mapping is allocated"
        );
        local.deallocate(big);
        assert!(
            stats().allocated <= freed.allocated + (1 << 20),
            "a freed large mapping is not"
        );
    }
}
