//! Kernel performance benchmarks — cycle-accurate measurement of every
//! syscall category on M4 Pro silicon.
//!
//! Two measurement modes:
//! - **SVC benchmark:** full trap + dispatch + eret (null syscall).
//! - **Dispatch benchmarks:** direct `kern.dispatch()` calls. Isolates
//!   kernel logic from trap overhead.
//!
//! Statistics: 10 warmup, 1000 measurement iterations. Reports median + P99.

use crate::{
    address_space::AddressSpace,
    config,
    frame::arch,
    syscall::{Kernel, num},
    thread::Thread,
    types::{AddressSpaceId, ObjectType, Priority, Rights, ThreadId},
};

const WARMUP: usize = 10;
const ITERATIONS: usize = 1000;

struct BenchResult {
    name: &'static str,
    median: u64,
    p99: u64,
    threshold_mult: u64,
}

impl BenchResult {
    fn passed(&self) -> bool {
        self.median <= self.threshold_mult
    }
}

fn stats(samples: &mut [u64; ITERATIONS]) -> (u64, u64) {
    samples.sort_unstable();

    let median = samples[ITERATIONS / 2];
    let p99 = samples[ITERATIONS * 99 / 100];

    (median, p99)
}

fn bench_syscall(
    kern: &mut Kernel,
    current: ThreadId,
    name: &'static str,
    threshold: u64,
    syscall_num: u64,
    args: [u64; 6],
) -> BenchResult {
    for _ in 0..WARMUP {
        kern.dispatch(current, 0, syscall_num, &args);
    }

    let mut samples = [0u64; ITERATIONS];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        kern.dispatch(current, 0, syscall_num, &args);
        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    let (median, p99) = stats(&mut samples);

    BenchResult {
        name,
        median,
        p99,
        threshold_mult: threshold * 10,
    }
}

fn bench_create_close(
    kern: &mut Kernel,
    current: ThreadId,
    name: &'static str,
    threshold: u64,
    create_num: u64,
    create_args: [u64; 6],
) -> BenchResult {
    for _ in 0..WARMUP {
        let (err, handle) = kern.dispatch(current, 0, create_num, &create_args);

        if err == 0 {
            kern.dispatch(current, 0, num::HANDLE_CLOSE, &[handle, 0, 0, 0, 0, 0]);
        }
    }

    let mut samples = [0u64; ITERATIONS];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();
        let (err, handle) = kern.dispatch(current, 0, create_num, &create_args);

        arch::isb();
        *s = arch::read_cycle_counter().wrapping_sub(start);

        if err == 0 {
            kern.dispatch(current, 0, num::HANDLE_CLOSE, &[handle, 0, 0, 0, 0, 0]);
        }
    }

    let (median, p99) = stats(&mut samples);

    BenchResult {
        name,
        median,
        p99,
        threshold_mult: threshold * 10,
    }
}

fn setup_bench_env(kern: &mut Kernel) -> ThreadId {
    let asid = kern.alloc_asid().expect("bench: asid alloc");
    let space = AddressSpace::new(AddressSpaceId(0), asid, 0);
    let (space_idx, space_gen) = kern.spaces.alloc(space).expect("bench: space alloc");

    kern.spaces.get_mut(space_idx).unwrap().id = AddressSpaceId(space_idx);

    let space = kern.spaces.get_mut(space_idx).unwrap();

    space
        .handles_mut()
        .allocate(ObjectType::AddressSpace, space_idx, Rights::ALL, space_gen)
        .expect("bench: space handle");

    let thread = Thread::new(
        ThreadId(0),
        Some(AddressSpaceId(space_idx)),
        Priority::Medium,
        0x1000,
        0x2000,
        0,
    );
    let (tid_idx, _) = kern.threads.alloc(thread).expect("bench: thread alloc");

    kern.threads.get_mut(tid_idx).unwrap().id = ThreadId(tid_idx);
    kern.scheduler
        .enqueue(0, ThreadId(tid_idx), Priority::Medium);
    kern.alive_threads += 1;

    let space = kern.spaces.get_mut(space_idx).unwrap();

    space.set_thread_head(Some(tid_idx));

    ThreadId(tid_idx)
}

pub fn run(kern: &mut Kernel) {
    crate::println!("--- benchmarks ---");

    let current = setup_bench_env(kern);

    let mut results = alloc::vec::Vec::new();

    // ── Trap overhead ─────────────────────────────────────────────
    {
        for _ in 0..WARMUP {
            let _ = arch::svc_null();
        }

        let mut samples = [0u64; ITERATIONS];

        for s in &mut samples {
            arch::isb();

            let start = arch::read_cycle_counter();
            let _ = arch::svc_null();

            arch::isb();
            *s = arch::read_cycle_counter().wrapping_sub(start);
        }

        let (median, p99) = stats(&mut samples);

        results.push(BenchResult {
            name: "svc null (trap+eret)",
            median,
            p99,
            threshold_mult: 2000,
        });
    }

    // ── Dispatch overhead (no SVC) ────────────────────────────────
    results.push(bench_syscall(
        kern,
        current,
        "invalid syscall (dispatch)",
        200,
        255,
        [0; 6],
    ));

    // ── Object creation + close ───────────────────────────────────
    results.push(bench_create_close(
        kern,
        current,
        "vmo_create+close",
        400,
        num::VMO_CREATE,
        [config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
    ));

    results.push(bench_create_close(
        kern,
        current,
        "event_create+close",
        400,
        num::EVENT_CREATE,
        [0; 6],
    ));

    results.push(bench_create_close(
        kern,
        current,
        "endpoint_create+close",
        400,
        num::ENDPOINT_CREATE,
        [0; 6],
    ));

    // ── Event operations ──────────────────────────────────────────
    let (_, evt_h) = kern.dispatch(current, 0, num::EVENT_CREATE, &[0; 6]);

    results.push(bench_syscall(
        kern,
        current,
        "event_signal",
        300,
        num::EVENT_SIGNAL,
        [evt_h, 0x1, 0, 0, 0, 0],
    ));

    results.push(bench_syscall(
        kern,
        current,
        "event_clear",
        300,
        num::EVENT_CLEAR,
        [evt_h, 0x1, 0, 0, 0, 0],
    ));

    // Signal bits so wait returns immediately.
    kern.dispatch(current, 0, num::EVENT_SIGNAL, &[evt_h, 0xFF, 0, 0, 0, 0]);

    results.push(bench_syscall(
        kern,
        current,
        "event_wait (signaled)",
        300,
        num::EVENT_WAIT,
        [evt_h, 0xFF, 1, 0, 0, 0],
    ));

    kern.dispatch(current, 0, num::HANDLE_CLOSE, &[evt_h, 0, 0, 0, 0, 0]);

    // ── Handle operations ─────────────────────────────────────────
    let (_, vmo_h) = kern.dispatch(
        current,
        0,
        num::VMO_CREATE,
        &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
    );

    results.push(bench_syscall(
        kern,
        current,
        "handle_info",
        100,
        num::HANDLE_INFO,
        [vmo_h, 0, 0, 0, 0, 0],
    ));

    // handle_dup + close (paired)
    {
        for _ in 0..WARMUP {
            let (err, dup) = kern.dispatch(
                current,
                0,
                num::HANDLE_DUP,
                &[vmo_h, Rights::ALL.0 as u64, 0, 0, 0, 0],
            );

            if err == 0 {
                kern.dispatch(current, 0, num::HANDLE_CLOSE, &[dup, 0, 0, 0, 0, 0]);
            }
        }

        let mut samples = [0u64; ITERATIONS];

        for s in &mut samples {
            arch::isb();

            let start = arch::read_cycle_counter();
            let (err, dup) = kern.dispatch(
                current,
                0,
                num::HANDLE_DUP,
                &[vmo_h, Rights::ALL.0 as u64, 0, 0, 0, 0],
            );

            arch::isb();
            *s = arch::read_cycle_counter().wrapping_sub(start);

            if err == 0 {
                kern.dispatch(current, 0, num::HANDLE_CLOSE, &[dup, 0, 0, 0, 0, 0]);
            }
        }

        let (median, p99) = stats(&mut samples);

        results.push(BenchResult {
            name: "handle_dup",
            median,
            p99,
            threshold_mult: 2000,
        });
    }

    // ── VMO operations ────────────────────────────────────────────
    {
        for _ in 0..WARMUP {
            let (err, snap) = kern.dispatch(current, 0, num::VMO_SNAPSHOT, &[vmo_h, 0, 0, 0, 0, 0]);

            if err == 0 {
                kern.dispatch(current, 0, num::HANDLE_CLOSE, &[snap, 0, 0, 0, 0, 0]);
            }
        }

        let mut samples = [0u64; ITERATIONS];

        for s in &mut samples {
            arch::isb();

            let start = arch::read_cycle_counter();
            let (err, snap) = kern.dispatch(current, 0, num::VMO_SNAPSHOT, &[vmo_h, 0, 0, 0, 0, 0]);

            arch::isb();
            *s = arch::read_cycle_counter().wrapping_sub(start);

            if err == 0 {
                kern.dispatch(current, 0, num::HANDLE_CLOSE, &[snap, 0, 0, 0, 0, 0]);
            }
        }

        let (median, p99) = stats(&mut samples);

        results.push(BenchResult {
            name: "vmo_snapshot+close",
            median,
            p99,
            threshold_mult: 8000,
        });
    }

    kern.dispatch(current, 0, num::HANDLE_CLOSE, &[vmo_h, 0, 0, 0, 0, 0]);

    // ── Info syscalls ─────────────────────────────────────────────
    results.push(bench_syscall(
        kern,
        current,
        "clock_read",
        50,
        num::CLOCK_READ,
        [0; 6],
    ));

    results.push(bench_syscall(
        kern,
        current,
        "system_info",
        100,
        num::SYSTEM_INFO,
        [0; 6],
    ));

    // ── Print results ─────────────────────────────────────────────
    let mut all_pass = true;

    for r in &results {
        let status = if r.passed() {
            "PASS"
        } else {
            all_pass = false;
            "FAIL"
        };

        crate::println!(
            "  {:30} median {:>6}  P99 {:>6}  [{}]",
            r.name,
            r.median,
            r.p99,
            status,
        );
    }

    if all_pass {
        crate::println!("benchmarks: all passed");
    } else {
        crate::println!("benchmarks: STRUCTURAL REGRESSION DETECTED");
    }
}
