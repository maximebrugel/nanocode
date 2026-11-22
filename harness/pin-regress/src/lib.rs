use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicIsize, Ordering},
};

/// The system allocator, counting the bytes it has handed out and not had back.
pub struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);

impl Counting {
    pub fn live() -> isize {
        LIVE.load(Ordering::SeqCst)
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::SeqCst);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            LIVE.fetch_add(new_size as isize - layout.size() as isize, Ordering::SeqCst);
        }
        new
    }
}

/// The settings nanocode-python boots its interpreters with, its frozen standard library installed.
pub fn settings() -> rustpython_vm::Settings {
    rustpython_vm::import::set_external_frozen(rustpython_vm::import::ExternalFrozen {
        find: nanocode_python_data::find_module,
        load: nanocode_python_data::load_module,
    });
    let mut settings = rustpython_vm::Settings::default();
    settings.install_signal_handlers = false;
    settings.import_site = false;
    settings.user_site_directory = false;
    settings.write_bytecode = false;
    settings.ignore_environment = true;
    settings.isolated = true;
    settings
}
