//! The `psh` program: `psh::run_as` with the process's arguments (`lib.rs`).

// nanocode's own allocator, as nanocode's binaries and the parity harness use: the utilities
// allocate per value and per line, and jq over 5 MB took 449 ms on the C library's malloc
// against 369 ms on this one.
#[global_allocator]
static ALLOCATOR: nanocode_sys::alloc::Heap = nanocode_sys::alloc::Heap;

fn main() -> std::process::ExitCode {
    let mut args = std::env::args_os();
    let arg0 = args.next().unwrap_or_default();
    psh::run_as(arg0, args.collect())
}
