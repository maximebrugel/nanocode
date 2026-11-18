use std::{
    cell::{Cell, OnceCell},
    ffi::{CStr, c_char, c_int, c_uint, c_void},
    ptr::null_mut,
    sync::atomic::{AtomicU64, Ordering},
};

use nanocode_platform::Instant;

fn ascii(c: c_int) -> Option<u8> {
    u8::try_from(c).ok().filter(u8::is_ascii)
}

fn class(c: c_int, test: fn(&u8) -> bool) -> c_int {
    c_int::from(ascii(c).is_some_and(|byte| test(&byte)))
}

#[unsafe(no_mangle)]
pub extern "C" fn isalnum(c: c_int) -> c_int {
    class(c, u8::is_ascii_alphanumeric)
}

#[unsafe(no_mangle)]
pub extern "C" fn isalpha(c: c_int) -> c_int {
    class(c, u8::is_ascii_alphabetic)
}

#[unsafe(no_mangle)]
pub extern "C" fn iscntrl(c: c_int) -> c_int {
    class(c, u8::is_ascii_control)
}

#[unsafe(no_mangle)]
pub extern "C" fn isdigit(c: c_int) -> c_int {
    class(c, u8::is_ascii_digit)
}

#[unsafe(no_mangle)]
pub extern "C" fn isgraph(c: c_int) -> c_int {
    class(c, u8::is_ascii_graphic)
}

#[unsafe(no_mangle)]
pub extern "C" fn islower(c: c_int) -> c_int {
    class(c, u8::is_ascii_lowercase)
}

#[unsafe(no_mangle)]
pub extern "C" fn isupper(c: c_int) -> c_int {
    class(c, u8::is_ascii_uppercase)
}

#[unsafe(no_mangle)]
pub extern "C" fn isprint(c: c_int) -> c_int {
    class(c, |byte| byte.is_ascii_graphic() || *byte == b' ')
}

#[unsafe(no_mangle)]
pub extern "C" fn ispunct(c: c_int) -> c_int {
    class(c, u8::is_ascii_punctuation)
}

#[unsafe(no_mangle)]
pub extern "C" fn isspace(c: c_int) -> c_int {
    class(c, |byte| byte.is_ascii_whitespace() || *byte == b'\x0b')
}

#[unsafe(no_mangle)]
pub extern "C" fn isxdigit(c: c_int) -> c_int {
    class(c, u8::is_ascii_hexdigit)
}

#[unsafe(no_mangle)]
pub extern "C" fn tolower(c: c_int) -> c_int {
    ascii(c).map_or(c, |byte| c_int::from(byte.to_ascii_lowercase()))
}

#[unsafe(no_mangle)]
pub extern "C" fn toupper(c: c_int) -> c_int {
    ascii(c).map_or(c, |byte| c_int::from(byte.to_ascii_uppercase()))
}

thread_local! {
    static START: OnceCell<Instant> = const { OnceCell::new() };
}

#[unsafe(no_mangle)]
pub extern "C" fn clock() -> i64 {
    START.with(|start| {
        i64::try_from(start.get_or_init(Instant::now).elapsed().as_micros()).unwrap_or(i64::MAX)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcmp(left: *const c_char, right: *const c_char) -> c_int {
    unsafe { strncmp(left, right, usize::MAX) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strncmp(left: *const c_char, right: *const c_char, limit: usize) -> c_int {
    for at in 0..limit {
        let (a, b) = unsafe { (*left.add(at) as u8, *right.add(at) as u8) };
        if a != b || a == 0 {
            return c_int::from(a) - c_int::from(b);
        }
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strchr(text: *const c_char, wanted: c_int) -> *mut c_char {
    let wanted = wanted as u8;
    let mut at = text;
    loop {
        let byte = unsafe { *at } as u8;
        if byte == wanted {
            return at.cast_mut();
        }
        if byte == 0 {
            return null_mut();
        }
        at = unsafe { at.add(1) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strrchr(text: *const c_char, wanted: c_int) -> *mut c_char {
    let wanted = wanted as u8;
    let mut found = null_mut();
    let mut at = text;
    loop {
        let byte = unsafe { *at } as u8;
        if byte == wanted {
            found = at.cast_mut();
        }
        if byte == 0 {
            return found;
        }
        at = unsafe { at.add(1) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strstr(haystack: *const c_char, needle: *const c_char) -> *mut c_char {
    let needle = unsafe { std::ffi::CStr::from_ptr(needle) }.to_bytes();
    let haystack_bytes = unsafe { std::ffi::CStr::from_ptr(haystack) }.to_bytes();
    if needle.is_empty() {
        return haystack.cast_mut();
    }
    haystack_bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .map_or(null_mut(), |at| unsafe { haystack.add(at) }.cast_mut())
}

#[unsafe(no_mangle)]
pub extern "C" fn malloc(size: usize) -> *mut c_void {
    crate::heap::PyMem_RawMalloc(size)
}

#[unsafe(no_mangle)]
pub extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
    crate::heap::PyMem_RawCalloc(count, size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(pointer: *mut c_void, size: usize) -> *mut c_void {
    unsafe { crate::heap::PyMem_RawRealloc(pointer, size) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(pointer: *mut c_void) {
    unsafe { crate::heap::PyMem_RawFree(pointer) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strdup(text: *const c_char) -> *mut c_char {
    let length = unsafe { CStr::from_ptr(text) }.to_bytes_with_nul().len();
    let copy = malloc(length).cast::<c_char>();
    if !copy.is_null() {
        unsafe { copy.copy_from_nonoverlapping(text, length) };
    }
    copy
}

#[unsafe(no_mangle)]
pub extern "C" fn fread(
    _data: *mut c_void,
    _size: usize,
    _count: usize,
    _file: *mut c_void,
) -> usize {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn fwrite(
    _data: *const c_void,
    _size: usize,
    _count: usize,
    _file: *mut c_void,
) -> usize {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ferror(_file: *mut c_void) -> c_int {
    1
}

thread_local! {
    static ERRNO: Cell<c_int> = const { Cell::new(0) };
}

#[unsafe(no_mangle)]
pub extern "C" fn __errno_location() -> *mut c_int {
    ERRNO.with(Cell::as_ptr)
}

static STDERR: u8 = 0;

#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static stderr: &u8 = &STDERR;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcpy(target: *mut c_char, source: *const c_char) -> *mut c_char {
    let length = unsafe { CStr::from_ptr(source) }.to_bytes_with_nul().len();
    unsafe { target.copy_from(source, length) };
    target
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strncpy(
    target: *mut c_char,
    source: *const c_char,
    limit: usize,
) -> *mut c_char {
    let mut at = 0;
    while at < limit && unsafe { *source.add(at) } != 0 {
        unsafe { *target.add(at) = *source.add(at) };
        at += 1;
    }
    while at < limit {
        unsafe { *target.add(at) = 0 };
        at += 1;
    }
    target
}

static SEED: AtomicU64 = AtomicU64::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn srand(seed: c_uint) {
    SEED.store(u64::from(seed).wrapping_sub(1), Ordering::Relaxed);
}

#[unsafe(no_mangle)]
pub extern "C" fn rand() -> c_int {
    let step = |seed: u64| seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let previous = SEED
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |seed| {
            Some(step(seed))
        })
        .unwrap_or_else(|seed| seed);
    (step(previous) >> 33) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn qsort(
    base: *mut c_void,
    count: usize,
    size: usize,
    compare: unsafe extern "C" fn(*const c_void, *const c_void) -> c_int,
) {
    if count < 2 || size == 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts_mut(base.cast::<u8>(), count * size) };
    let mut items: Vec<Vec<u8>> = bytes.chunks_exact(size).map(<[u8]>::to_vec).collect();
    items.sort_by(|left, right| {
        unsafe { compare(left.as_ptr().cast(), right.as_ptr().cast()) }.cmp(&0)
    });
    for (chunk, item) in bytes.chunks_exact_mut(size).zip(items) {
        chunk.copy_from_slice(&item);
    }
}
