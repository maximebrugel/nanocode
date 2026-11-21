use core::sync::atomic::Ordering;

use super::class::{CLASS_BYTES, CLASSES};
use super::segment::{
    self, HEADER_SLICES, OWNER_ABANDONED, SLICE_BYTES, SLICES, SLICES_FREE_ALL, Segment,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Class {
    pub block_bytes: usize,
    pub pages: usize,
    pub bytes: usize,
    pub blocks: usize,
    pub used: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mappings {
    pub count: usize,
    pub bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Segments {
    pub owned: usize,
    pub abandoned: usize,
    pub empty: usize,
    pub cached: usize,
    pub unregistered: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slices {
    pub headers: usize,
    pub in_pages: usize,
    pub free_dirty: usize,
    pub free_clean: usize,
    pub torn: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub classes: [Class; CLASSES],
    pub large: Mappings,
    pub large_cached: Mappings,
    pub segments: Segments,
    pub slices: Slices,
    pub allocated: usize,
    pub retained: usize,
}

impl Profile {
    pub const SLICE_BYTES: usize = SLICE_BYTES;
    pub const SEGMENT_BYTES: usize = segment::SEGMENT_BYTES;

    pub const fn resident_max(&self) -> usize {
        (self.slices.headers + self.slices.in_pages + self.slices.free_dirty) * SLICE_BYTES
            + self.large.bytes
            + self.large_cached.bytes
    }
}

pub fn census() -> Profile {
    let mut profile = Profile {
        classes: [Class::default(); CLASSES],
        large: Mappings::default(),
        large_cached: Mappings::default(),
        segments: Segments::default(),
        slices: Slices::default(),
        allocated: 0,
        retained: 0,
    };
    for (class, entry) in profile.classes.iter_mut().enumerate() {
        entry.block_bytes = CLASS_BYTES[class];
    }
    segment::each_registered(|seg| walk(seg, &mut profile));
    let (count, bytes) = super::stats::large();
    profile.large = Mappings { count, bytes };
    let (count, bytes) = segment::large_cached();
    profile.large_cached = Mappings { count, bytes };
    profile.segments.cached = segment::segments_cached();
    profile.segments.unregistered = segment::unregistered();
    let stats = super::stats::stats();
    profile.allocated = stats.allocated;
    profile.retained = stats.retained;
    profile
}

fn walk(seg: &Segment, profile: &mut Profile) {
    let free = seg.free_slices.peek();
    let dirty = seg.dirty_slices.peek();
    let slices = &mut profile.slices;
    slices.headers += HEADER_SLICES;
    slices.free_dirty += (free & dirty & SLICES_FREE_ALL).count_ones() as usize;
    slices.free_clean += (free & !dirty & SLICES_FREE_ALL).count_ones() as usize;
    if free == SLICES_FREE_ALL {
        profile.segments.empty += 1;
        return;
    }
    if seg.owner.load(Ordering::Relaxed) == OWNER_ABANDONED {
        profile.segments.abandoned += 1;
    } else {
        profile.segments.owned += 1;
    }
    let mut slice = HEADER_SLICES;
    while slice < SLICES {
        if free & (1 << slice) != 0 {
            slice += 1;
            continue;
        }
        let page = &seg.pages[slice];
        let span = page.span.peek();
        let class = page.class.peek();
        let block_bytes = page.block_bytes.peek();
        let used = page.used.peek();
        let whole = page.head.peek() == slice
            && span > 0
            && span <= SLICES - slice
            && class < CLASSES
            && block_bytes == CLASS_BYTES[class];
        let blocks = if whole { span * SLICE_BYTES / block_bytes } else { 0 };
        if !whole || used > blocks {
            profile.slices.torn += 1;
            slice += 1;
            continue;
        }
        let entry = &mut profile.classes[class];
        entry.pages += 1;
        entry.bytes += span * SLICE_BYTES;
        entry.blocks += blocks;
        entry.used += used;
        profile.slices.in_pages += span;
        slice += span;
    }
}
