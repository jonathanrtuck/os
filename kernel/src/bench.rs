//! Kernel performance benchmarks — diagnostic baselines, not optimization targets.
//!
//! Measures cycle counts via CNTVCT_EL0. Results printed to serial.
//! Pass/fail against 10x thresholds to detect structural regressions
//! (e.g., accidentally holding a lock across a syscall path).

use crate::frame::arch;

struct BenchResult {
    name: &'static str,
    cycles: u64,
    threshold: u64,
}

impl BenchResult {
    fn passed(&self) -> bool {
        self.cycles <= self.threshold
    }
}

fn measure<F: FnOnce()>(f: F) -> u64 {
    arch::isb();

    let start = arch::read_cycle_counter();

    f();

    arch::isb();

    let end = arch::read_cycle_counter();

    end - start
}

fn bench_n<F: Fn()>(name: &'static str, iterations: usize, expected: u64, f: F) -> BenchResult {
    for _ in 0..10 {
        f();
    }

    let mut total = 0u64;

    for _ in 0..iterations {
        total += measure(&f);
    }

    let avg = total / iterations as u64;

    BenchResult {
        name,
        cycles: avg,
        threshold: expected * 10,
    }
}

pub fn run() {
    crate::println!("--- benchmarks ---");

    let results = [bench_n("null syscall", 10000, 200, || {
        let _ = arch::svc_null();
    })];
    let mut all_pass = true;

    for r in &results {
        let status = if r.passed() {
            "PASS"
        } else {
            all_pass = false;
            "FAIL"
        };

        crate::println!("  {:30} {:>6} cycles  [{}]", r.name, r.cycles, status);
    }

    if all_pass {
        crate::println!("benchmarks: all passed");
    } else {
        crate::println!("benchmarks: STRUCTURAL REGRESSION DETECTED");
    }
}
