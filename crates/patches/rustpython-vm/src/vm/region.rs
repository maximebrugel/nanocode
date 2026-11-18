use core::alloc::{GlobalAlloc, Layout};

pub struct Regions<A>(pub A);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    pub bytes: u64,
    pub blocks: u64,
    pub held: u64,
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for Regions<A> {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if run::active() {
            unsafe { run::alloc(layout, false) }
        } else {
            unsafe { self.0.alloc(layout) }
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if run::active() {
            unsafe { run::alloc(layout, true) }
        } else {
            unsafe { self.0.alloc_zeroed(layout) }
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if run::owns(ptr) {
            unsafe { run::free(ptr, layout) }
        } else {
            unsafe { self.0.dealloc(ptr, layout) }
        }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if run::retired(ptr) {
            let moved =
                unsafe { self.alloc(Layout::from_size_align_unchecked(size, layout.align())) };
            if !moved.is_null() {
                unsafe {
                    core::ptr::copy_nonoverlapping(ptr, moved, layout.size().min(size));
                    run::free(ptr, layout);
                }
            }
            moved
        } else if run::owns(ptr) {
            unsafe { run::realloc(ptr, layout, size) }
        } else {
            unsafe { self.0.realloc(ptr, layout, size) }
        }
    }
}

pub fn enter(poison: bool) {
    run::enter(poison);
}

pub fn leave() -> Kept {
    run::leave()
}

pub fn set(on: bool) -> bool {
    run::set(on)
}

pub fn owns(ptr: *mut u8) -> bool {
    run::owns(ptr)
}

#[inline]
pub fn persistent_for<R>(owner: *const u8, f: impl FnOnce() -> R) -> R {
    if run::active() && !run::owns(owner.cast_mut()) {
        run::set(false);
        let result = f();
        run::set(true);
        result
    } else {
        f()
    }
}

#[cfg(target_arch = "wasm32")]
mod run {
    use super::Kept;
    use core::alloc::Layout;
    use core::arch::wasm32;
    use core::cell::UnsafeCell;
    use core::ptr;

    const PAGE: usize = 65536;
    const PAGES: usize = u32::MAX as usize / PAGE + 1;
    const GENERATIONS: usize = 15;

    struct Pages(u8);

    struct Generation {
        heap: dlmalloc::Dlmalloc<Pages>,
        bytes: usize,
        blocks: usize,
        retired: bool,
    }

    impl Generation {
        const fn new(id: u8) -> Self {
            Self {
                heap: dlmalloc::Dlmalloc::new_with_allocator(Pages(id)),
                bytes: 0,
                blocks: 0,
                retired: false,
            }
        }
    }

    struct State {
        owner: [u8; PAGES],
        live: [u16; PAGES],
        top: usize,
        current: u8,
        active: bool,
        poison: bool,
        generations: [Generation; GENERATIONS],
    }

    struct Shared<T>(UnsafeCell<T>);

    unsafe impl<T> Sync for Shared<T> {}

    const FREE: u8 = u8::MAX;
    const PERSISTENT: u8 = 0;

    static STATE: Shared<State> = Shared(UnsafeCell::new(State {
        owner: [PERSISTENT; PAGES],
        live: [0; PAGES],
        top: 0,
        current: 0,
        active: false,
        poison: false,
        generations: {
            let mut generations = [const { Generation::new(0) }; GENERATIONS];
            let mut id = 0;
            while id < GENERATIONS {
                generations[id] = Generation::new(id as u8 + 1);
                id += 1;
            }
            generations
        },
    }));

    fn state() -> &'static mut State {
        unsafe { &mut *STATE.0.get() }
    }

    impl State {
        fn take(&mut self, count: usize, id: u8) -> Option<usize> {
            let mut run = 0;
            for page in 0..self.top {
                if self.owner[page] == FREE {
                    run += 1;
                    if run == count {
                        let first = page + 1 - count;
                        self.owner[first..=page].fill(id);
                        return Some(first);
                    }
                } else {
                    run = 0;
                }
            }
            None
        }

        fn grow(&mut self, count: usize, id: u8) -> Option<usize> {
            let first = wasm32::memory_grow(0, count);
            if first == usize::MAX {
                return None;
            }
            self.owner[first..first + count].fill(id);
            self.top = self.top.max(first + count);
            Some(first)
        }

        fn release(&mut self, start: usize, size: usize) {
            self.owner[start / PAGE..(start + size) / PAGE].fill(FREE);
        }

        fn count(&mut self, ptr: *mut u8, size: usize, add: bool) {
            let first = ptr.addr() / PAGE;
            let last = (ptr.addr() + size.max(1) - 1) / PAGE;
            for page in first..=last {
                if add {
                    self.live[page] += 1;
                } else {
                    self.live[page] -= 1;
                }
            }
        }

        fn release_dead(&mut self, id: u8, first: usize, last: usize) {
            for page in first..=last {
                if self.owner[page] == id && self.live[page] == 0 {
                    self.free_page(page);
                }
            }
        }

        fn free_page(&mut self, page: usize) {
            if self.poison {
                unsafe {
                    ptr::write_bytes(
                        ptr::with_exposed_provenance_mut::<u8>(page * PAGE),
                        0xdb,
                        PAGE,
                    );
                }
            }
            self.owner[page] = FREE;
        }

        fn generation(&mut self, id: u8) -> &mut Generation {
            &mut self.generations[usize::from(id) - 1]
        }

        fn drop_generation(&mut self, id: u8) {
            for page in 0..self.top {
                if self.owner[page] == id {
                    self.free_page(page);
                }
            }
            *self.generation(id) = Generation::new(id);
        }
    }

    unsafe impl dlmalloc::Allocator for Pages {
        fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
            let count = size.div_ceil(PAGE);
            let state = state();
            match state
                .take(count, self.0)
                .or_else(|| state.grow(count, self.0))
            {
                Some(first) => (
                    ptr::with_exposed_provenance_mut(first * PAGE),
                    count * PAGE,
                    0,
                ),
                None => (ptr::null_mut(), 0, 0),
            }
        }

        fn remap(&self, _: *mut u8, _: usize, _: usize, _: bool) -> *mut u8 {
            ptr::null_mut()
        }

        fn free_part(&self, ptr: *mut u8, old: usize, new: usize) -> bool {
            let start = ptr.addr() + new;
            if start % PAGE != 0 || old % PAGE != 0 {
                return false;
            }
            state().release(start, old - new);
            true
        }

        fn free(&self, ptr: *mut u8, size: usize) -> bool {
            if ptr.addr() % PAGE != 0 || size % PAGE != 0 {
                return false;
            }
            state().release(ptr.addr(), size);
            true
        }

        fn can_release_part(&self, _: u32) -> bool {
            true
        }

        fn allocates_zeros(&self) -> bool {
            false
        }

        fn page_size(&self) -> usize {
            PAGE
        }
    }

    #[inline]
    pub(super) fn active() -> bool {
        state().active
    }

    #[inline]
    fn owner(ptr: *mut u8) -> u8 {
        let state = state();
        let page = ptr.addr() / PAGE;
        if page < state.top {
            state.owner[page]
        } else {
            PERSISTENT
        }
    }

    #[inline]
    pub(super) fn owns(ptr: *mut u8) -> bool {
        owner(ptr) != PERSISTENT
    }

    #[inline]
    pub(super) fn retired(ptr: *mut u8) -> bool {
        let id = owner(ptr);
        id != PERSISTENT && state().generation(id).retired
    }

    pub(super) unsafe fn alloc(layout: Layout, zeroed: bool) -> *mut u8 {
        let state = state();
        let generation = state.generation(state.current);
        let ptr = unsafe {
            if zeroed {
                generation.heap.calloc(layout.size(), layout.align())
            } else {
                generation.heap.malloc(layout.size(), layout.align())
            }
        };
        if !ptr.is_null() {
            generation.bytes += layout.size();
            generation.blocks += 1;
            state.count(ptr, layout.size(), true);
        }
        ptr
    }

    pub(super) unsafe fn free(ptr: *mut u8, layout: Layout) {
        let id = owner(ptr);
        let state = state();
        let generation = state.generation(id);
        generation.bytes -= layout.size();
        generation.blocks -= 1;
        let retired = generation.retired;
        let empty = generation.blocks == 0;
        if !retired {
            unsafe { generation.heap.free(ptr, layout.size(), layout.align()) }
        }
        state.count(ptr, layout.size(), false);
        if retired {
            if empty {
                state.drop_generation(id);
            } else {
                let first = ptr.addr() / PAGE;
                let last = (ptr.addr() + layout.size().max(1) - 1) / PAGE;
                state.release_dead(id, first, last);
            }
        }
    }

    pub(super) unsafe fn realloc(ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let id = owner(ptr);
        let state = state();
        let generation = state.generation(id);
        let moved = unsafe {
            generation
                .heap
                .realloc(ptr, layout.size(), layout.align(), size)
        };
        if !moved.is_null() {
            generation.bytes = generation.bytes - layout.size() + size;
            state.count(ptr, layout.size(), false);
            state.count(moved, size, true);
        }
        moved
    }

    pub(super) fn set(on: bool) -> bool {
        let state = state();
        let was = state.active;
        state.active = on && state.current != 0;
        was
    }

    pub(super) fn enter(poison: bool) {
        let state = state();
        state.poison = poison;
        state.current = (1..=GENERATIONS as u8)
            .find(|&id| {
                let generation = state.generation(id);
                !generation.retired && generation.blocks == 0
            })
            .unwrap_or(0);
        state.active = state.current != 0;
    }

    pub(super) fn leave() -> Kept {
        let state = state();
        state.active = false;
        let id = core::mem::replace(&mut state.current, 0);
        let mut kept = Kept::default();
        if id != 0 {
            let generation = state.generation(id);
            kept.bytes = generation.bytes as u64;
            kept.blocks = generation.blocks as u64;
            if generation.blocks == 0 {
                state.drop_generation(id);
            } else {
                generation.retired = true;
                let top = state.top;
                state.release_dead(id, 0, top.saturating_sub(1));
            }
        }
        kept.held = state.owner[..state.top]
            .iter()
            .filter(|&&owner| owner != PERSISTENT && owner != FREE)
            .count() as u64
            * PAGE as u64;
        kept
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod run {
    use super::Kept;
    use core::alloc::Layout;

    #[inline]
    pub(super) fn active() -> bool {
        false
    }

    #[inline]
    pub(super) fn owns(_: *mut u8) -> bool {
        false
    }

    #[inline]
    pub(super) fn retired(_: *mut u8) -> bool {
        false
    }

    pub(super) unsafe fn alloc(_: Layout, _: bool) -> *mut u8 {
        unreachable!("no run region outside wasm32")
    }

    pub(super) unsafe fn free(_: *mut u8, _: Layout) {
        unreachable!("no run region outside wasm32")
    }

    pub(super) unsafe fn realloc(_: *mut u8, _: Layout, _: usize) -> *mut u8 {
        unreachable!("no run region outside wasm32")
    }

    pub(super) fn enter(_: bool) {}

    pub(super) fn set(_: bool) -> bool {
        false
    }

    pub(super) fn leave() -> Kept {
        Kept::default()
    }
}
