//! ddd5f7a60 + 4877ce7df: creating and dropping a whole python3 interpreter
//! (stdlib, the python-capi extensions, boot) leaves next to nothing behind.

use pin_regress::Counting;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const WARM: usize = 50;
const CYCLES: usize = 1000;
const MAX_BYTES_PER_CYCLE: f64 = 1024.0;

#[test]
fn interpreter_create_drop_leaks_under_1kb_per_cycle() {
    for _ in 0..WARM {
        drop(nanocode_python::Python::new().expect("an interpreter"));
    }
    let before = Counting::live();
    let started = std::time::Instant::now();
    for _ in 0..CYCLES {
        drop(nanocode_python::Python::new().expect("an interpreter"));
    }
    let grown = Counting::live() - before;
    let per_cycle = grown as f64 / CYCLES as f64;
    eprintln!(
        "heap grew {grown} bytes over {CYCLES} create+drop cycles: {per_cycle:.1} bytes per cycle ({:.1} ms per cycle)",
        started.elapsed().as_secs_f64() * 1000.0 / CYCLES as f64
    );
    assert!(
        per_cycle < MAX_BYTES_PER_CYCLE,
        "each interpreter left {per_cycle:.1} bytes behind (bound {MAX_BYTES_PER_CYCLE})"
    );
}
