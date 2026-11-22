//! 4877ce7df: PyType::add_subclass prunes dead weak references when the list is full,
//! so a base's subclass list stays within twice its live subclasses plus its starting room,
//! and creating a class costs the same however many came before.

use std::time::{Duration, Instant};

use rustpython_vm::{Interpreter, builtins::PyType};

const BATCH: usize = 500;
const BATCHES: usize = 4;
const MAX_TIME_RATIO: f64 = 1.5;

fn churn(live: usize) {
    Interpreter::without_stdlib(pin_regress::settings()).enter(|vm| {
        let scope = vm.new_scope_with_builtins();
        let source = format!(
            "import gc\n\
             class B: pass\n\
             keep = [type('L', (B,), {{}}) for _ in range({live})]\n\
             def churn():\n    c = type('C', (B,), {{}})\n    del c\n    gc.collect()\n"
        );
        vm.run_string(scope.clone(), &source, "<pin-regress>")
            .map_err(|error| vm.print_exception(&error))
            .expect("the setup runs");
        let base = scope.globals.get_item("B", vm).expect("B");
        let base = base.downcast_ref::<PyType>().expect("B is a type");
        let churn = scope.globals.get_item("churn", vm).expect("churn");
        let start_capacity = base.subclasses.read().capacity();
        let bound = 2 * live + start_capacity;
        let (mut peak_len, mut peak_capacity) = (0, 0);
        let mut times: Vec<Duration> = Vec::new();
        for batch in 0..=BATCHES {
            let started = Instant::now();
            for _ in 0..BATCH {
                churn.call((), vm).expect("churn runs");
                let subclasses = base.subclasses.read();
                peak_len = peak_len.max(subclasses.len());
                peak_capacity = peak_capacity.max(subclasses.capacity());
            }
            if batch > 0 {
                times.push(started.elapsed());
            }
        }
        let listed = vm
            .run_block_expr(scope.clone(), "len(B.__subclasses__())")
            .expect("__subclasses__ runs");
        let listed: usize = listed.try_into_value(vm).expect("a length");
        let slowest = times.iter().max().expect("batches").as_secs_f64();
        let fastest = times.iter().min().expect("batches").as_secs_f64();
        eprintln!(
            "live {live}: listed {listed}, start capacity {start_capacity}, peak len {peak_len}, peak capacity {peak_capacity}, bound {bound}, batch ms {:?}, max/min {:.2}",
            times.iter().map(|time| time.as_secs_f64() * 1000.0).map(|ms| (ms * 10.0).round() / 10.0).collect::<Vec<_>>(),
            slowest / fastest
        );
        assert_eq!(listed, live, "B.__subclasses__() is not the live subclasses");
        assert!(peak_len <= bound, "live {live}: the subclass list reached {peak_len} entries (bound {bound})");
        assert!(peak_capacity <= bound, "live {live}: the subclass list reached capacity {peak_capacity} (bound {bound})");
        assert!(
            slowest / fastest < MAX_TIME_RATIO,
            "live {live}: batch times {times:?} are not flat"
        );
    });
}

macro_rules! cases {
    ($($name:ident => $live:expr,)*) => {$(
        #[test]
        fn $name() {
            churn($live);
        }
    )*};
}

cases! {
    live_2_7_minus_2 => (1 << 7) - 2,
    live_2_7_plus_2 => (1 << 7) + 2,
    live_2_8_minus_2 => (1 << 8) - 2,
    live_2_8_plus_2 => (1 << 8) + 2,
    live_2_9_minus_2 => (1 << 9) - 2,
    live_2_9_plus_2 => (1 << 9) + 2,
    live_2_10_minus_2 => (1 << 10) - 2,
    live_2_10_plus_2 => (1 << 10) + 2,
}
