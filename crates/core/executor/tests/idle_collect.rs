//! A worker that sits idle collects the blocks other threads freed into its heap, so their pages go back
//! instead of staying allocated until that worker next allocates.

#[cfg(unix)]
#[global_allocator]
static HEAP: nanocode_sys::alloc::Heap = nanocode_sys::alloc::Heap;

#[cfg(unix)]
#[test]
fn an_idle_worker_gives_back_pages_another_thread_emptied() {
    use std::time::{Duration, Instant};

    let runtime = nanocode_executor::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    let blocks: Vec<Box<[u8; 640]>> = runtime.block_on(async {
        nanocode_executor::spawn(async { (0..(64usize << 20) / 640).map(|_| Box::new([1u8; 640])).collect() })
            .await
            .expect("the worker allocates")
    });
    let held = nanocode_sys::alloc::Heap::stats().allocated;
    drop(blocks);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut allocated = nanocode_sys::alloc::Heap::stats().allocated;
    while allocated + (48 << 20) > held && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        allocated = nanocode_sys::alloc::Heap::stats().allocated;
    }
    assert!(
        allocated + (48 << 20) <= held,
        "64 MiB the idle worker allocated and this thread freed go back within 10 s: {held} -> {allocated} bytes allocated"
    );
    drop(runtime);
}
