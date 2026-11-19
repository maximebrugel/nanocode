//! Minimal repro: several threads, each blocking on its own current-thread
//! nanocode-executor runtime, looping on short sleeps. Exits 0 when every
//! thread finishes, or reports the threads still stuck after 10 s.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn main() {
    let threads: usize = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(8);
    let done = Arc::new(AtomicUsize::new(0));
    for index in 0..threads {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let runtime = nanocode_executor::Builder::new_current_thread()
                .build()
                .expect("a runtime");
            runtime.block_on(async {
                for step in 0..500u64 {
                    nanocode_executor::sleep(Duration::from_millis(1 + (step + index as u64) % 3))
                        .await;
                }
            });
            done.fetch_add(1, Ordering::SeqCst);
        });
    }
    let started = Instant::now();
    while done.load(Ordering::SeqCst) < threads && started.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let finished = done.load(Ordering::SeqCst);
    println!(
        "{finished} of {threads} threads finished in {:?}",
        started.elapsed()
    );
    std::process::exit(i32::from(finished < threads));
}
