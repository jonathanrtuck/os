//! Kernel performance benchmarks — cycle-accurate measurement of every
//! syscall category on M4 Pro silicon.
//!
//! Two measurement modes:
//! - **SVC benchmark:** full trap + dispatch + eret (null syscall).
//! - **Dispatch benchmarks:** direct `crate::syscall::dispatch()` calls.
//!   Isolates kernel logic from trap overhead.
//!
//! Statistics: 10 warmup, 1000 measurement iterations. Reports median + P99.

use crate::{
    address_space::AddressSpace,
    config,
    frame::{arch, state},
    syscall::num,
    thread::{Thread, ThreadRunState},
    types::{AddressSpaceId, HandleId, ObjectType, Priority, Rights, ThreadId},
};

const WARMUP: usize = 10;
const ITERATIONS: usize = 1000;
const BATCH_N: usize = 500;
const BATCH_SAMPLES: usize = 100;

#[derive(Default)]
struct BenchResult {
    name: &'static str,
    median: u64,
    p99: u64,
    threshold_mult: u64,
    /// HVF guest ticks per op (24 MHz), captured around the bench. Zero
    /// when HVF timing is not advertised.
    guest_ticks_per_op: u64,
    /// HVF host ticks per op. Zero when HVF timing is not advertised.
    host_ticks_per_op: u64,
    /// VMEXITs per 1000 ops, ×10 for one decimal of precision. The single
    /// fence HVC per snapshot interval contributes ~1/(WARMUP+ITERATIONS)
    /// per op, well below 0.1/k for the run() bench shape.
    exits_per_kop_x10: u64,
}

impl BenchResult {
    fn passed(&self) -> bool {
        self.median <= self.threshold_mult
    }
}

/// Capture an HVF timing delta around a `bench_*` helper invocation that
/// reports a `BenchResult`. Per-op fields are filled in from the captured
/// deltas; on hosts without HVF timing they stay zero.
fn capture_hvf_into(result: &mut BenchResult, total_ops: usize, hvf: HvfDelta) {
    let ops = total_ops as u64;

    if ops == 0 || !arch::hvf_timing::enabled() {
        return;
    }

    result.guest_ticks_per_op = hvf.guest_ticks / ops;
    result.host_ticks_per_op = hvf.host_ticks / ops;
    result.exits_per_kop_x10 = hvf.exits_total * 10_000 / ops;
}

/// Run a `bench_*` helper and overlay HVF per-op columns onto its result.
/// `total_ops` is the number of "iterations" the helper executes —
/// matching the unit reported by `BenchResult::median` (per-syscall for
/// `bench_syscall`, per-create-pair for `bench_create_close`).
fn bench_with_hvf<F: FnOnce() -> BenchResult>(total_ops: usize, do_bench: F) -> BenchResult {
    let (mut r, hvf) = capture_hvf(do_bench);

    capture_hvf_into(&mut r, total_ops, hvf);
    r
}

/// Snapshot a delta around a closure. Issues fence HVCs to make HVF flush
/// its accumulated counters into the shared page before each read.
fn capture_hvf<R, F: FnOnce() -> R>(do_bench: F) -> (R, HvfDelta) {
    arch::hvf_timing::force_snapshot();

    let start = arch::hvf_timing::read(0);
    let result = do_bench();

    arch::hvf_timing::force_snapshot();

    let end = arch::hvf_timing::read(0);
    let d = end.diff(&start);

    (
        result,
        HvfDelta {
            guest_ticks: d.guest_ticks,
            host_ticks: d.host_ticks,
            exits_total: d.exits_total,
        },
    )
}

#[derive(Clone, Copy, Default)]
struct HvfDelta {
    guest_ticks: u64,
    host_ticks: u64,
    exits_total: u64,
}

struct CycleEstimate {
    name: &'static str,
    cycles_x10: u64,
    theoretical: u64,
    /// HVF guest cycles per op, ×10. Zero when HVF timing is not advertised.
    guest_cycles_x10: u64,
    /// HVF host cycles per op, ×10. Zero when HVF timing is not advertised.
    host_cycles_x10: u64,
    /// VMEXITs per 1000 ops. Zero when HVF timing is not advertised. Scaling
    /// to 1000 because most syscalls do not VMEXIT — exits/op rounds to zero.
    exits_per_kop: u64,
}

fn ticks_to_cycles_x10(total_ticks: u64, batch_size: usize) -> u64 {
    // 1 tick at 24 MHz = 187.5 CPU cycles at 4.5 GHz.
    // Returns cycles × 10 for one decimal place of precision.
    total_ticks * 1875 / batch_size as u64
}

/// Total ops per `bench_batched_*` invocation: BATCH_N warmup +
/// BATCH_SAMPLES outer × BATCH_N inner. Used to scale HVF deltas captured
/// around the entire invocation into per-op cycles.
const TOTAL_BATCHED_OPS: usize = BATCH_N + BATCH_N * BATCH_SAMPLES;

/// Run an op closure in the standard batched-measurement shape: BATCH_N
/// warmup iterations, then BATCH_SAMPLES outer × BATCH_N inner. Returns the
/// median outer-iteration tick count (i.e., ticks for BATCH_N invocations).
fn run_batched_op<F: FnMut()>(mut op: F) -> u64 {
    for _ in 0..BATCH_N {
        op();
    }

    let mut samples = [0u64; BATCH_SAMPLES];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        for _ in 0..BATCH_N {
            op();
        }

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    samples.sort_unstable();

    samples[BATCH_SAMPLES / 2]
}

/// Capture an HVF timing delta around `do_bench`. Returns a `CycleEstimate`
/// with HVF columns filled in. `total_ops` is the number of operations the
/// closure executes — used to scale the captured deltas into per-op cycles.
/// On hosts without HVF timing the HVF fields are zero.
///
/// The fence HVC at each boundary (`force_snapshot`) is what makes the
/// reading meaningful: HVF only updates per-vCPU `guest_ticks` at exit
/// boundaries, so without the fence a bench that never VMEXITs would
/// produce a zero delta even after millions of iterations. The fence
/// itself contributes one HVC-class exit to each side of the interval.
fn estimate_with_hvf<F: FnOnce() -> u64>(
    name: &'static str,
    theoretical: u64,
    cycles_x10_per_op_div: usize,
    total_ops: usize,
    do_bench: F,
) -> CycleEstimate {
    arch::hvf_timing::force_snapshot();

    let start = arch::hvf_timing::read(0);
    let median_ticks = do_bench();

    arch::hvf_timing::force_snapshot();

    let end = arch::hvf_timing::read(0);
    let delta = end.diff(&start);
    let ops = total_ops as u64;
    let (guest_x10, host_x10, exits_kop) = if ops == 0 || !arch::hvf_timing::enabled() {
        (0, 0, 0)
    } else {
        (
            delta.guest_ticks * 1875 / ops,
            delta.host_ticks * 1875 / ops,
            delta.exits_total * 1000 / ops,
        )
    };

    CycleEstimate {
        name,
        cycles_x10: ticks_to_cycles_x10(median_ticks, cycles_x10_per_op_div),
        theoretical,
        guest_cycles_x10: guest_x10,
        host_cycles_x10: host_x10,
        exits_per_kop: exits_kop,
    }
}

/// Print a `BenchResult` row, with HVF columns if the counter page is
/// advertised. The HVF columns are per-iteration ticks (24 MHz, divide by
/// the host's tick rate to get cycles) — guest, host, and exits/kop ×10.
/// Iteration units are bench-helper specific:
///   `bench_syscall`        → 1 syscall
///   `bench_create_close`   → 1 create + 1 close pair
///   workload helpers       → 1 workload run
fn print_bench_row(r: &BenchResult, status: &str, hvf_on: bool) {
    if hvf_on {
        crate::println!(
            "  {:24} median {:>6}  P99 {:>6}  [{}]   guest {:>6}  host {:>6}  exits {:>3}.{}/k",
            r.name,
            r.median,
            r.p99,
            status,
            r.guest_ticks_per_op,
            r.host_ticks_per_op,
            r.exits_per_kop_x10 / 10,
            r.exits_per_kop_x10 % 10,
        );
    } else {
        crate::println!(
            "  {:30} median {:>6}  P99 {:>6}  [{}]",
            r.name,
            r.median,
            r.p99,
            status,
        );
    }
}

fn stats(samples: &mut [u64; ITERATIONS]) -> (u64, u64) {
    samples.sort_unstable();

    let median = samples[ITERATIONS / 2];
    let p99 = samples[ITERATIONS * 99 / 100];

    (median, p99)
}

fn bench_syscall(
    current: ThreadId,
    name: &'static str,
    threshold: u64,
    syscall_num: u64,
    args: [u64; 6],
) -> BenchResult {
    let space_id = crate::syscall::thread_space_id(current).ok();

    for _ in 0..WARMUP {
        crate::syscall::dispatch(current, space_id, 0, syscall_num, &args);
    }

    let mut samples = [0u64; ITERATIONS];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        crate::syscall::dispatch(current, space_id, 0, syscall_num, &args);

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    let (median, p99) = stats(&mut samples);

    BenchResult {
        name,
        median,
        p99,
        threshold_mult: threshold * 10,
        ..Default::default()
    }
}

fn bench_create_close(
    current: ThreadId,
    name: &'static str,
    threshold: u64,
    create_num: u64,
    create_args: [u64; 6],
) -> BenchResult {
    let space_id = crate::syscall::thread_space_id(current).ok();

    for _ in 0..WARMUP {
        let (err, handle) =
            crate::syscall::dispatch(current, space_id, 0, create_num, &create_args);

        if err == 0 {
            crate::syscall::dispatch(
                current,
                space_id,
                0,
                num::HANDLE_CLOSE,
                &[handle, 0, 0, 0, 0, 0],
            );
        }
    }

    let mut samples = [0u64; ITERATIONS];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();
        let (err, handle) =
            crate::syscall::dispatch(current, space_id, 0, create_num, &create_args);

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);

        if err == 0 {
            crate::syscall::dispatch(
                current,
                space_id,
                0,
                num::HANDLE_CLOSE,
                &[handle, 0, 0, 0, 0, 0],
            );
        }
    }

    let (median, p99) = stats(&mut samples);

    BenchResult {
        name,
        median,
        p99,
        threshold_mult: threshold * 10,
        ..Default::default()
    }
}

fn bench_batched_dispatch(current: ThreadId, syscall_num: u64, args: [u64; 6]) -> u64 {
    let space_id = crate::syscall::thread_space_id(current).ok();

    for _ in 0..BATCH_N {
        crate::syscall::dispatch(current, space_id, 0, syscall_num, &args);
    }

    let mut samples = [0u64; BATCH_SAMPLES];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        for _ in 0..BATCH_N {
            crate::syscall::dispatch(current, space_id, 0, syscall_num, &args);
        }

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    samples.sort_unstable();

    samples[BATCH_SAMPLES / 2]
}

fn bench_batched_create_close(current: ThreadId, create_num: u64, create_args: [u64; 6]) -> u64 {
    let space_id = crate::syscall::thread_space_id(current).ok();

    for _ in 0..BATCH_N {
        let (err, h) = crate::syscall::dispatch(current, space_id, 0, create_num, &create_args);

        if err == 0 {
            crate::syscall::dispatch(current, space_id, 0, num::HANDLE_CLOSE, &[h, 0, 0, 0, 0, 0]);
        }
    }

    let mut samples = [0u64; BATCH_SAMPLES];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        for _ in 0..BATCH_N {
            let (err, h) = crate::syscall::dispatch(current, space_id, 0, create_num, &create_args);

            if err == 0 {
                crate::syscall::dispatch(
                    current,
                    space_id,
                    0,
                    num::HANDLE_CLOSE,
                    &[h, 0, 0, 0, 0, 0],
                );
            }
        }

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    samples.sort_unstable();

    samples[BATCH_SAMPLES / 2]
}

fn setup_bench_env() -> ThreadId {
    let asid = state::alloc_asid().expect("bench: asid alloc");
    let space = AddressSpace::new(AddressSpaceId(0), asid, 0);
    let (space_idx, space_gen) = state::spaces()
        .alloc_shared(space)
        .expect("bench: space alloc");

    state::spaces().write(space_idx).unwrap().id = AddressSpaceId(space_idx);
    #[cfg(target_os = "none")]
    state::spaces()
        .write(space_idx)
        .unwrap()
        .set_aslr_seed(crate::frame::arch::entropy::random_u64());

    {
        let mut space = state::spaces().write(space_idx).unwrap();

        space
            .handles_mut()
            .allocate(ObjectType::AddressSpace, space_idx, Rights::ALL, space_gen)
            .expect("bench: space handle");
    }

    let thread = Thread::new(
        ThreadId(0),
        Some(AddressSpaceId(space_idx)),
        Priority::Medium,
        0x1000,
        0x2000,
        0,
    );
    let (tid_idx, _) = state::threads()
        .alloc_shared(thread)
        .expect("bench: thread alloc");

    state::threads().write(tid_idx).unwrap().id = ThreadId(tid_idx);
    state::schedulers()
        .core(0)
        .lock()
        .enqueue(ThreadId(tid_idx), Priority::Medium);
    state::inc_alive_threads();

    {
        let mut space = state::spaces().write(space_idx).unwrap();

        space.set_thread_head(Some(tid_idx));
    }

    ThreadId(tid_idx)
}

pub fn run() {
    crate::println!("--- benchmarks ---");

    let current = setup_bench_env();
    let space_id = crate::syscall::thread_space_id(current).ok();
    let mut results = alloc::vec::Vec::new();
    // ── Trap overhead ─────────────────────────────────────────────
    let svc_null_result = bench_with_hvf(WARMUP + ITERATIONS, || {
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

        BenchResult {
            name: "svc null (trap+eret)",
            median,
            p99,
            threshold_mult: 2000,
            ..Default::default()
        }
    });

    results.push(svc_null_result);
    // ── Dispatch overhead (no SVC) ────────────────────────────────
    results.push(bench_with_hvf(WARMUP + ITERATIONS, || {
        bench_syscall(current, "invalid syscall (dispatch)", 200, 255, [0; 6])
    }));
    // ── Object creation + close ───────────────────────────────────
    // Each iteration runs two syscalls (create + close), so total_ops
    // doubles. The cycles_x10 column reports the create-only median
    // (close runs outside the cycle counter window); the HVF columns
    // span both — that is the actual per-pair cost.
    results.push(bench_with_hvf(2 * (WARMUP + ITERATIONS), || {
        bench_create_close(
            current,
            "vmo_create+close",
            400,
            num::VMO_CREATE,
            [config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        )
    }));
    results.push(bench_with_hvf(2 * (WARMUP + ITERATIONS), || {
        bench_create_close(
            current,
            "event_create+close",
            400,
            num::EVENT_CREATE,
            [0; 6],
        )
    }));
    results.push(bench_with_hvf(2 * (WARMUP + ITERATIONS), || {
        bench_create_close(
            current,
            "endpoint_create+close",
            400,
            num::ENDPOINT_CREATE,
            [0; 6],
        )
    }));

    // ── Event operations ──────────────────────────────────────────
    let (_, evt_h) = crate::syscall::dispatch(current, space_id, 0, num::EVENT_CREATE, &[0; 6]);

    results.push(bench_with_hvf(WARMUP + ITERATIONS, || {
        bench_syscall(
            current,
            "event_signal",
            300,
            num::EVENT_SIGNAL,
            [evt_h, 0x1, 0, 0, 0, 0],
        )
    }));
    results.push(bench_with_hvf(WARMUP + ITERATIONS, || {
        bench_syscall(
            current,
            "event_clear",
            300,
            num::EVENT_CLEAR,
            [evt_h, 0x1, 0, 0, 0, 0],
        )
    }));

    // Signal bits so wait returns immediately.
    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::EVENT_SIGNAL,
        &[evt_h, 0xFF, 0, 0, 0, 0],
    );

    results.push(bench_with_hvf(WARMUP + ITERATIONS, || {
        bench_syscall(
            current,
            "event_wait (signaled)",
            300,
            num::EVENT_WAIT,
            [evt_h, 0xFF, 1, 0, 0, 0],
        )
    }));

    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::HANDLE_CLOSE,
        &[evt_h, 0, 0, 0, 0, 0],
    );

    // ── Handle operations ─────────────────────────────────────────
    let (_, vmo_h) = crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::VMO_CREATE,
        &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
    );

    results.push(bench_with_hvf(WARMUP + ITERATIONS, || {
        bench_syscall(
            current,
            "handle_info",
            100,
            num::HANDLE_INFO,
            [vmo_h, 0, 0, 0, 0, 0],
        )
    }));

    // handle_dup + close (paired)
    let handle_dup_result = bench_with_hvf(2 * (WARMUP + ITERATIONS), || {
        for _ in 0..WARMUP {
            let (err, dup) = crate::syscall::dispatch(
                current,
                space_id,
                0,
                num::HANDLE_DUP,
                &[vmo_h, Rights::ALL.0 as u64, 0, 0, 0, 0],
            );

            if err == 0 {
                crate::syscall::dispatch(
                    current,
                    space_id,
                    0,
                    num::HANDLE_CLOSE,
                    &[dup, 0, 0, 0, 0, 0],
                );
            }
        }

        let mut samples = [0u64; ITERATIONS];

        for s in &mut samples {
            arch::isb();

            let start = arch::read_cycle_counter();
            let (err, dup) = crate::syscall::dispatch(
                current,
                space_id,
                0,
                num::HANDLE_DUP,
                &[vmo_h, Rights::ALL.0 as u64, 0, 0, 0, 0],
            );

            arch::isb();

            *s = arch::read_cycle_counter().wrapping_sub(start);

            if err == 0 {
                crate::syscall::dispatch(
                    current,
                    space_id,
                    0,
                    num::HANDLE_CLOSE,
                    &[dup, 0, 0, 0, 0, 0],
                );
            }
        }

        let (median, p99) = stats(&mut samples);

        BenchResult {
            name: "handle_dup",
            median,
            p99,
            threshold_mult: 2000,
            ..Default::default()
        }
    });

    results.push(handle_dup_result);

    // ── VMO operations ────────────────────────────────────────────
    let vmo_snap_result = bench_with_hvf(2 * (WARMUP + ITERATIONS), || {
        for _ in 0..WARMUP {
            let (err, snap) = crate::syscall::dispatch(
                current,
                space_id,
                0,
                num::VMO_SNAPSHOT,
                &[vmo_h, 0, 0, 0, 0, 0],
            );

            if err == 0 {
                crate::syscall::dispatch(
                    current,
                    space_id,
                    0,
                    num::HANDLE_CLOSE,
                    &[snap, 0, 0, 0, 0, 0],
                );
            }
        }

        let mut samples = [0u64; ITERATIONS];

        for s in &mut samples {
            arch::isb();

            let start = arch::read_cycle_counter();
            let (err, snap) = crate::syscall::dispatch(
                current,
                space_id,
                0,
                num::VMO_SNAPSHOT,
                &[vmo_h, 0, 0, 0, 0, 0],
            );

            arch::isb();

            *s = arch::read_cycle_counter().wrapping_sub(start);

            if err == 0 {
                crate::syscall::dispatch(
                    current,
                    space_id,
                    0,
                    num::HANDLE_CLOSE,
                    &[snap, 0, 0, 0, 0, 0],
                );
            }
        }

        let (median, p99) = stats(&mut samples);

        BenchResult {
            name: "vmo_snapshot+close",
            median,
            p99,
            threshold_mult: 8000,
            ..Default::default()
        }
    });

    results.push(vmo_snap_result);

    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::HANDLE_CLOSE,
        &[vmo_h, 0, 0, 0, 0, 0],
    );

    // ── Info syscalls ─────────────────────────────────────────────
    results.push(bench_with_hvf(WARMUP + ITERATIONS, || {
        bench_syscall(current, "clock_read", 50, num::CLOCK_READ, [0; 6])
    }));
    results.push(bench_with_hvf(WARMUP + ITERATIONS, || {
        bench_syscall(current, "system_info", 100, num::SYSTEM_INFO, [0; 6])
    }));

    // ── Print results ─────────────────────────────────────────────
    let mut all_pass = true;
    let hvf_on = arch::hvf_timing::enabled();

    for r in &results {
        let status = if r.passed() {
            "PASS"
        } else {
            all_pass = false;
            "FAIL"
        };

        print_bench_row(r, status, hvf_on);
    }

    // ── Workload benchmarks ─────────────────────────────────────────
    crate::println!("--- workloads ---");

    results.push(bench_with_hvf(WARMUP + ITERATIONS, || {
        bench_document_editing(current)
    }));
    results.push(bench_with_hvf(WARMUP + ITERATIONS, || {
        bench_ipc_storm(current)
    }));
    results.push(bench_with_hvf(WARMUP + ITERATIONS, || {
        bench_object_lifecycle_churn(current)
    }));

    for r in results.iter().skip(results.len() - 3) {
        let status = if r.passed() {
            "PASS"
        } else {
            all_pass = false;
            "FAIL"
        };

        print_bench_row(r, status, hvf_on);
    }

    if all_pass {
        crate::println!("benchmarks: all passed");
    } else {
        crate::println!("benchmarks: STRUCTURAL REGRESSION DETECTED");
    }

    run_cycle_estimates(current);

    #[cfg(feature = "profile")]
    run_profile(current);

    teardown_bench_env(current);
}

fn teardown_bench_env(thread_id: ThreadId) {
    let space_id = state::threads()
        .read(thread_id.0)
        .unwrap()
        .address_space()
        .unwrap();

    state::schedulers().remove(thread_id);
    state::threads().dealloc_shared(thread_id.0);
    state::dec_alive_threads();

    {
        let mut space = state::spaces().write(space_id.0).unwrap();

        space.set_thread_head(None);
    }

    state::spaces().dealloc_shared(space_id.0);
}

fn bench_document_editing(current: ThreadId) -> BenchResult {
    let page = config::PAGE_SIZE as u64;

    for _ in 0..WARMUP {
        document_editing_iteration(current, page);
    }

    let mut samples = [0u64; ITERATIONS];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        document_editing_iteration(current, page);

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    let (median, p99) = stats(&mut samples);

    BenchResult {
        name: "workload: doc editing",
        median,
        p99,
        threshold_mult: 50000,
        ..Default::default()
    }
}

fn document_editing_iteration(current: ThreadId, page: u64) {
    let space_id = crate::syscall::thread_space_id(current).ok();
    // Simulate: create VMO (document content), map it, snapshot (undo point),
    // create event (compositor notify), signal it, close everything.
    let (_, vmo) = crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::VMO_CREATE,
        &[page, 0, 0, 0, 0, 0],
    );
    let (_, snap) = crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::VMO_SNAPSHOT,
        &[vmo, 0, 0, 0, 0, 0],
    );
    let (_, evt) = crate::syscall::dispatch(current, space_id, 0, num::EVENT_CREATE, &[0; 6]);

    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::EVENT_SIGNAL,
        &[evt, 0x1, 0, 0, 0, 0],
    );
    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::EVENT_CLEAR,
        &[evt, 0x1, 0, 0, 0, 0],
    );
    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::HANDLE_CLOSE,
        &[evt, 0, 0, 0, 0, 0],
    );
    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::HANDLE_CLOSE,
        &[snap, 0, 0, 0, 0, 0],
    );
    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::HANDLE_CLOSE,
        &[vmo, 0, 0, 0, 0, 0],
    );
}

fn bench_ipc_storm(current: ThreadId) -> BenchResult {
    let space_id = crate::syscall::thread_space_id(current).ok();
    let (_, ep) = crate::syscall::dispatch(current, space_id, 0, num::ENDPOINT_CREATE, &[0; 6]);

    for _ in 0..WARMUP {
        ipc_storm_iteration(current, ep);
    }

    let mut samples = [0u64; ITERATIONS];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        ipc_storm_iteration(current, ep);

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::HANDLE_CLOSE,
        &[ep, 0, 0, 0, 0, 0],
    );

    let (median, p99) = stats(&mut samples);

    BenchResult {
        name: "workload: IPC storm (10 calls)",
        median,
        p99,
        threshold_mult: 100000,
        ..Default::default()
    }
}

fn ipc_storm_iteration(current: ThreadId, ep: u64) {
    let space_id = crate::syscall::thread_space_id(current).ok();
    let mut buf = [0u8; 128];

    // 10 rapid call attempts — each will enqueue a pending call (thread blocks
    // then we restore). We measure the enqueue path, which is the hot path.
    for _ in 0..10 {
        crate::syscall::dispatch(
            current,
            space_id,
            0,
            num::CALL,
            &[ep, buf.as_mut_ptr() as u64, 8, 0, 0, 0],
        );

        // Restore thread for next iteration (thread blocked on call).
        if let Some(mut t) = state::threads().write(current.0)
            && t.state() == crate::thread::ThreadRunState::Blocked
        {
            t.set_state(crate::thread::ThreadRunState::Ready);
            t.set_state(crate::thread::ThreadRunState::Running);
        }
    }

    // Drain the endpoint so it doesn't fill up across iterations.
    if let Some(space) = state::spaces().read(0)
        && let Ok(handle) = space.handles().lookup(crate::types::HandleId(ep as u32))
    {
        let ep_id = handle.object_id;

        drop(space);

        if let Some(mut endpoint) = state::endpoints().write(ep_id) {
            while endpoint.dequeue_call().is_some() {}
        }
    }
}

fn bench_object_lifecycle_churn(current: ThreadId) -> BenchResult {
    for _ in 0..WARMUP {
        object_churn_iteration(current);
    }

    let mut samples = [0u64; ITERATIONS];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        object_churn_iteration(current);

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    let (median, p99) = stats(&mut samples);

    BenchResult {
        name: "workload: object churn",
        median,
        p99,
        threshold_mult: 100000,
        ..Default::default()
    }
}

fn object_churn_iteration(current: ThreadId) {
    let space_id = crate::syscall::thread_space_id(current).ok();
    let page = config::PAGE_SIZE as u64;
    let mut handles = [0u64; 8];

    // Create 4 VMOs + 2 events + 2 endpoints
    for h in &mut handles[..4] {
        let (_, hid) = crate::syscall::dispatch(
            current,
            space_id,
            0,
            num::VMO_CREATE,
            &[page, 0, 0, 0, 0, 0],
        );

        *h = hid;
    }
    for h in &mut handles[4..6] {
        let (_, hid) = crate::syscall::dispatch(current, space_id, 0, num::EVENT_CREATE, &[0; 6]);

        *h = hid;
    }
    for h in &mut handles[6..8] {
        let (_, hid) =
            crate::syscall::dispatch(current, space_id, 0, num::ENDPOINT_CREATE, &[0; 6]);

        *h = hid;
    }

    // Close all in reverse order — exercises different table paths
    for h in handles.iter().rev() {
        crate::syscall::dispatch(
            current,
            space_id,
            0,
            num::HANDLE_CLOSE,
            &[*h, 0, 0, 0, 0, 0],
        );
    }
}

struct IpcBenchEnv {
    server: ThreadId,
    client_ep_h: u64,
    server_ep_h: u64,
    ep_obj_id: u32,
    server_space_idx: u32,
}

fn setup_ipc_bench(client: ThreadId) -> IpcBenchEnv {
    let client_space_id = crate::syscall::thread_space_id(client).ok();
    let asid = state::alloc_asid().expect("ipc bench: server asid");
    let space = AddressSpace::new(AddressSpaceId(0), asid, 0);
    let (space_idx, space_gen) = state::spaces()
        .alloc_shared(space)
        .expect("ipc bench: server space");

    state::spaces().write(space_idx).unwrap().id = AddressSpaceId(space_idx);
    #[cfg(target_os = "none")]
    state::spaces()
        .write(space_idx)
        .unwrap()
        .set_aslr_seed(crate::frame::arch::entropy::random_u64());

    {
        let mut server_space = state::spaces().write(space_idx).unwrap();

        server_space
            .handles_mut()
            .allocate(ObjectType::AddressSpace, space_idx, Rights::ALL, space_gen)
            .expect("ipc bench: space handle");
    }

    let thread = Thread::new(
        ThreadId(0),
        Some(AddressSpaceId(space_idx)),
        Priority::Medium,
        0x3000,
        0x4000,
        0,
    );
    let (server_idx, _) = state::threads()
        .alloc_shared(thread)
        .expect("ipc bench: server thread");

    state::threads().write(server_idx).unwrap().id = ThreadId(server_idx);
    state::inc_alive_threads();

    let (err, client_ep_h) =
        crate::syscall::dispatch(client, client_space_id, 0, num::ENDPOINT_CREATE, &[0; 6]);

    assert_eq!(err, 0);

    let client_space_id = crate::syscall::thread_space_id(client).unwrap();
    let (ep_obj_id, ep_gen) = {
        let space = state::spaces().read(client_space_id.0).unwrap();
        let handle = space
            .handles()
            .lookup(HandleId(client_ep_h as u32))
            .unwrap();

        (handle.object_id, handle.generation)
    };

    {
        let mut server_space = state::spaces().write(space_idx).unwrap();
        let server_ep_h = server_space
            .handles_mut()
            .allocate(ObjectType::Endpoint, ep_obj_id, Rights::ALL, ep_gen)
            .expect("ipc bench: server ep handle");

        drop(server_space);

        state::endpoints().write(ep_obj_id).unwrap().add_ref();

        IpcBenchEnv {
            server: ThreadId(server_idx),
            client_ep_h,
            server_ep_h: server_ep_h.0 as u64,
            ep_obj_id,
            server_space_idx: space_idx,
        }
    }
}

fn force_running(tid: ThreadId) {
    state::schedulers().remove(tid);

    if let Some(mut t) = state::threads().write(tid.0) {
        match t.state() {
            ThreadRunState::Blocked => {
                t.set_state(ThreadRunState::Ready);
                t.set_state(ThreadRunState::Running);
            }
            ThreadRunState::Ready => {
                t.set_state(ThreadRunState::Running);
            }
            _ => {}
        }
    }
}

fn bench_ipc_null_round_trip(env: &IpcBenchEnv, client: ThreadId) -> u64 {
    for _ in 0..BATCH_N {
        ipc_null_iteration(env, client);
    }

    let mut samples = [0u64; BATCH_SAMPLES];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        for _ in 0..BATCH_N {
            ipc_null_iteration(env, client);
        }

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    samples.sort_unstable();

    samples[BATCH_SAMPLES / 2]
}

fn ipc_null_iteration(env: &IpcBenchEnv, client: ThreadId) {
    let client_space_id = crate::syscall::thread_space_id(client).ok();
    // The bench drives a full IPC round-trip from kernel mode by issuing
    // CALL/RECV/REPLY directly, bypassing real thread context switches.
    // CALL's fast path calls sched::block_current(client) → switch_away →
    // pick_next, which removes the client from the run queue. Without
    // re-enqueueing each iteration, the next CALL's pick_next returns None
    // and switch_away parks into the idle loop — never returning to the
    // bench code.
    state::schedulers().remove(client);

    if let Some(mut t) = state::threads().write(client.0) {
        t.set_state(crate::thread::ThreadRunState::Ready);
    }

    state::schedulers()
        .core(0)
        .lock()
        .enqueue(client, Priority::Medium);
    state::endpoints()
        .write(env.ep_obj_id)
        .unwrap()
        .add_recv_waiter(env.server)
        .ok();

    let server_space_id =
        crate::syscall::thread_space_id(env.server).unwrap_or(AddressSpaceId(env.server_space_idx));

    if let Some(mut t) = state::threads().write(env.server.0) {
        t.set_recv_state(crate::thread::RecvState {
            endpoint_id: env.ep_obj_id,
            space_id: server_space_id,
            out_buf: 0,
            out_cap: 0,
            handles_out: 0,
            handles_cap: 0,
            reply_cap_out: 0,
        });
    }

    crate::syscall::dispatch(
        client,
        client_space_id,
        0,
        num::CALL,
        &[env.client_ep_h, 0, 0, 0, 0, 0],
    );

    force_running(env.server);

    let _ = crate::syscall::dispatch(
        env.server,
        Some(server_space_id),
        0,
        num::RECV,
        &[env.server_ep_h, 0, 0, 0, 0, 0],
    );
    // The reply_cap is normally written into the server's recv_state.reply_cap_out
    // userspace buffer. The bench has no userspace pointer (passes 0), so reach
    // into the endpoint's active_replies and pull the cap out directly. Without
    // this, REPLY would error with InvalidArgument and leak one slot per
    // iteration — exhausting the MAX_PENDING_PER_ENDPOINT pool after 8 calls.
    let reply_cap = state::endpoints()
        .read(env.ep_obj_id)
        .and_then(|ep| ep.first_active_reply_cap())
        .map_or(0, |c| c.0);

    crate::syscall::dispatch(
        env.server,
        Some(server_space_id),
        0,
        num::REPLY,
        &[env.server_ep_h, reply_cap, 0, 0, 0, 0],
    );

    force_running(client);
    force_running(env.server);
}

fn teardown_ipc_bench(env: &IpcBenchEnv, client: ThreadId) {
    let client_space_id = crate::syscall::thread_space_id(client).ok();

    crate::syscall::dispatch(
        client,
        client_space_id,
        0,
        num::HANDLE_CLOSE,
        &[env.client_ep_h, 0, 0, 0, 0, 0],
    );

    state::schedulers().remove(env.server);
    state::threads().dealloc_shared(env.server.0);
    state::dec_alive_threads();

    {
        let mut space = state::spaces().write(env.server_space_idx).unwrap();

        space.set_thread_head(None);
    }

    state::spaces().dealloc_shared(env.server_space_idx);
}

fn bench_fault_lookup(current: ThreadId) -> u64 {
    let page = config::PAGE_SIZE as u64;
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let space_id_val = crate::syscall::thread_space_id(current).unwrap();
    let space_id = Some(space_id_val);
    let (_, vmo_h) = crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::VMO_CREATE,
        &[page * 4, 0, 0, 0, 0, 0],
    );
    let (_, va) = crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::VMO_MAP,
        &[vmo_h, 0, rw.0 as u64, 0, 0, 0],
    );
    let fault_addr = va as usize + config::PAGE_SIZE;

    for _ in 0..BATCH_N {
        let space = state::spaces().read(space_id_val.0).unwrap();
        let _ = space.find_mapping(fault_addr);
    }

    let mut samples = [0u64; BATCH_SAMPLES];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        for _ in 0..BATCH_N {
            let space = state::spaces().read(space_id_val.0).unwrap();
            let mapping = space.find_mapping(fault_addr);

            if let Some(m) = mapping {
                let vmo_id = m.vmo_id;
                let page_idx = (fault_addr - m.va_start) / config::PAGE_SIZE;

                drop(space);

                let _ = state::vmos().read(vmo_id.0).map(|v| v.page_at(page_idx));
            }
        }

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    crate::syscall::dispatch(current, space_id, 0, num::VMO_UNMAP, &[va, 0, 0, 0, 0, 0]);
    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::HANDLE_CLOSE,
        &[vmo_h, 0, 0, 0, 0, 0],
    );

    samples.sort_unstable();

    samples[BATCH_SAMPLES / 2]
}

fn run_cycle_estimates(current: ThreadId) {
    let space_id = crate::syscall::thread_space_id(current).ok();

    crate::println!(
        "--- cycle estimates ({}x{} samples, 24MHz->4.5GHz) ---",
        BATCH_N,
        BATCH_SAMPLES,
    );

    let mut estimates: alloc::vec::Vec<CycleEstimate> = alloc::vec::Vec::new();

    // ── Empty nop microbench (HVF instrumentation sanity check) ──
    //
    // A pair of nops: pure guest cycles, no traps, no MMIO. With the HVF
    // instrumentation working correctly we expect:
    //   guest_cycles ≈ measured_cycles
    //   host_cycles  ≈ 0
    //   exits        ≈ 0/k (only the fence HVCs, amortized)
    //
    // If guest_cycles diverges from measured_cycles, the host is "stealing"
    // time (timer interrupts, hv_vcpu_run scheduling jitter). If exits is
    // non-zero, we're picking up unexpected VMEXITs in the bench window.
    estimates.push(estimate_with_hvf(
        "nop;nop (sanity)",
        2,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || run_batched_op(arch::nop_pair),
    ));
    // ── SVC null (real trap + ERET round-trip) ───────────────────
    estimates.push(estimate_with_hvf(
        "svc null (trap+eret)",
        50,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || {
            run_batched_op(|| {
                let _ = arch::svc_null();
            })
        },
    ));
    // ── Dispatch-only syscalls ───────────────────────────────────
    estimates.push(estimate_with_hvf(
        "dispatch overhead",
        5,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || bench_batched_dispatch(current, 255, [0; 6]),
    ));
    estimates.push(estimate_with_hvf(
        "clock_read",
        10,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || bench_batched_dispatch(current, num::CLOCK_READ, [0; 6]),
    ));
    estimates.push(estimate_with_hvf(
        "system_info",
        10,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || bench_batched_dispatch(current, num::SYSTEM_INFO, [0; 6]),
    ));

    // ── Handle operations (need a live VMO) ──────────────────────
    let (_, vmo_h) = crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::VMO_CREATE,
        &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
    );

    estimates.push(estimate_with_hvf(
        "handle_info",
        15,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || bench_batched_dispatch(current, num::HANDLE_INFO, [vmo_h, 0, 0, 0, 0, 0]),
    ));
    estimates.push(estimate_with_hvf(
        "handle_dup+close",
        30,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || {
            run_batched_op(|| {
                let (err, dup) = crate::syscall::dispatch(
                    current,
                    space_id,
                    0,
                    num::HANDLE_DUP,
                    &[vmo_h, Rights::ALL.0 as u64, 0, 0, 0, 0],
                );

                if err == 0 {
                    crate::syscall::dispatch(
                        current,
                        space_id,
                        0,
                        num::HANDLE_CLOSE,
                        &[dup, 0, 0, 0, 0, 0],
                    );
                }
            })
        },
    ));
    // ── VMO snapshot+close ───────────────────────────────────────
    estimates.push(estimate_with_hvf(
        "vmo_snapshot+close",
        60,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || {
            run_batched_op(|| {
                let (err, snap) = crate::syscall::dispatch(
                    current,
                    space_id,
                    0,
                    num::VMO_SNAPSHOT,
                    &[vmo_h, 0, 0, 0, 0, 0],
                );

                if err == 0 {
                    crate::syscall::dispatch(
                        current,
                        space_id,
                        0,
                        num::HANDLE_CLOSE,
                        &[snap, 0, 0, 0, 0, 0],
                    );
                }
            })
        },
    ));

    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::HANDLE_CLOSE,
        &[vmo_h, 0, 0, 0, 0, 0],
    );

    // ── Event operations ─────────────────────────────────────────
    let (_, evt_h) = crate::syscall::dispatch(current, space_id, 0, num::EVENT_CREATE, &[0; 6]);

    estimates.push(estimate_with_hvf(
        "event_signal",
        15,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || bench_batched_dispatch(current, num::EVENT_SIGNAL, [evt_h, 0x1, 0, 0, 0, 0]),
    ));
    estimates.push(estimate_with_hvf(
        "event_clear",
        15,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || bench_batched_dispatch(current, num::EVENT_CLEAR, [evt_h, 0x1, 0, 0, 0, 0]),
    ));

    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::EVENT_SIGNAL,
        &[evt_h, 0xFF, 0, 0, 0, 0],
    );

    estimates.push(estimate_with_hvf(
        "event_wait (signaled)",
        15,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || bench_batched_dispatch(current, num::EVENT_WAIT, [evt_h, 0xFF, 1, 0, 0, 0]),
    ));

    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::HANDLE_CLOSE,
        &[evt_h, 0, 0, 0, 0, 0],
    );

    // ── Object create+close pairs ────────────────────────────────
    estimates.push(estimate_with_hvf(
        "vmo create+close",
        50,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || {
            bench_batched_create_close(
                current,
                num::VMO_CREATE,
                [config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
            )
        },
    ));
    estimates.push(estimate_with_hvf(
        "event create+close",
        50,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || bench_batched_create_close(current, num::EVENT_CREATE, [0; 6]),
    ));
    estimates.push(estimate_with_hvf(
        "endpoint create+close",
        50,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || bench_batched_create_close(current, num::ENDPOINT_CREATE, [0; 6]),
    ));

    // ── IPC round-trip ──────────────────────────────────────────

    let ipc_env = setup_ipc_bench(current);

    estimates.push(estimate_with_hvf(
        "IPC null round-trip",
        150,
        BATCH_N,
        TOTAL_BATCHED_OPS,
        || bench_ipc_null_round_trip(&ipc_env, current),
    ));

    teardown_ipc_bench(&ipc_env, current);

    // ── Page fault kernel path ──────────────────────────────────
    estimates.push(estimate_with_hvf(
        "fault lookup+page_at",
        15,
        BATCH_N,
        BATCH_N + BATCH_N * BATCH_SAMPLES,
        || bench_fault_lookup(current),
    ));

    // ── Print results ────────────────────────────────────────────
    let mut within_2x = 0u32;
    let mut total_rated = 0u32;
    let hvf_on = arch::hvf_timing::enabled();

    for e in &estimates {
        let ratio_x10 = e.cycles_x10.checked_div(e.theoretical).unwrap_or(0);

        if e.theoretical > 0 {
            total_rated += 1;

            if ratio_x10 <= 20 {
                within_2x += 1;
            }
        }

        if hvf_on {
            // Per-op breakdown: total cycles, guest cycles inside hv_vcpu_run,
            // host cycles in HVF handlers, exit count scaled to per-1000-ops.
            crate::println!(
                "  {:24} {:>5}.{} cyc  (floor ~{:>3})  {}.{}x  guest {:>5}.{}  host {:>5}.{}  exits {:>3}.{}/k",
                e.name,
                e.cycles_x10 / 10,
                e.cycles_x10 % 10,
                e.theoretical,
                ratio_x10 / 10,
                ratio_x10 % 10,
                e.guest_cycles_x10 / 10,
                e.guest_cycles_x10 % 10,
                e.host_cycles_x10 / 10,
                e.host_cycles_x10 % 10,
                e.exits_per_kop / 10,
                e.exits_per_kop % 10,
            );
        } else {
            crate::println!(
                "  {:30} {:>5}.{} cyc  (floor ~{:>3})  {}.{}x",
                e.name,
                e.cycles_x10 / 10,
                e.cycles_x10 % 10,
                e.theoretical,
                ratio_x10 / 10,
                ratio_x10 % 10,
            );
        }
    }

    crate::println!(
        "  {}/{} within 2x of theoretical floor",
        within_2x,
        total_rated,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Phase 1+2: Cycle profile — per-stage breakdown of every measured path.
//
// CNTVCT_EL0 runs at 24 MHz (1 tick ≈ 187.5 CPU cycles at 4.5 GHz).
// Most stages complete in < 1 tick, so single-iteration stamps read 0.
// Fix: accumulate ticks over N iterations and divide. A stage that takes
// 50 cycles (0.27 ticks) will advance the counter in ~27% of iterations,
// giving ~135 accumulated ticks / 500 ops × 1875 = 50.6 cycles.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "profile")]
const PROFILE_N: usize = 500;
#[cfg(feature = "profile")]
const PROFILE_OUTER: usize = 100;

#[cfg(feature = "profile")]
fn print_stage(label: &str, cyc_x10: u64) {
    crate::println!("    {:40} {:>5}.{} cyc", label, cyc_x10 / 10, cyc_x10 % 10,);
}

#[cfg(feature = "profile")]
fn accum_dispatch(
    current: ThreadId,
    syscall_num: u64,
    args: [u64; 6],
    ref_slot: usize,
) -> [u64; 32] {
    use crate::frame::profile;

    let space_id = crate::syscall::thread_space_id(current).ok();

    for _ in 0..PROFILE_N {
        profile::reset();
        crate::syscall::dispatch(current, space_id, 0, syscall_num, &args);
    }

    let mut accum = [0u64; 32];
    let mut outer_samples = alloc::vec![0u64; PROFILE_OUTER];

    for (oi, sample) in outer_samples.iter_mut().enumerate() {
        let mut inner_accum = [0u64; 32];

        for _ in 0..PROFILE_N {
            profile::reset();
            crate::syscall::dispatch(current, space_id, 0, syscall_num, &args);
            let t = profile::read();
            let r = t[ref_slot];

            if r == 0 {
                continue;
            }

            for s in 0..32 {
                if t[s] > 0 && t[s] >= r {
                    inner_accum[s] += t[s] - r;
                }
            }
        }

        for s in 0..32 {
            accum[s] += inner_accum[s];
        }

        let _ = (oi, sample);
    }

    let total_ops = (PROFILE_OUTER * PROFILE_N) as u64;
    let mut result = [0u64; 32];

    for s in 0..32 {
        result[s] = accum[s] * 1875 / total_ops;
    }

    result
}

#[cfg(feature = "profile")]
fn accum_svc(ref_slot: usize) -> [u64; 32] {
    use crate::frame::profile;

    for _ in 0..PROFILE_N {
        let _ = arch::svc_null();
    }

    let mut accum = [0u64; 32];

    for _ in 0..PROFILE_OUTER {
        for _ in 0..PROFILE_N {
            profile::reset();
            profile::stamp(profile::slot::BENCH_BEFORE);

            let _ = arch::svc_null();

            profile::stamp(profile::slot::BENCH_AFTER);

            let t = profile::read();
            let r = t[ref_slot];

            if r == 0 {
                continue;
            }

            for s in 0..32 {
                if t[s] > 0 && t[s] >= r {
                    accum[s] += t[s] - r;
                }
            }
        }
    }

    let total_ops = (PROFILE_OUTER * PROFILE_N) as u64;
    let mut result = [0u64; 32];

    for s in 0..32 {
        result[s] = accum[s] * 1875 / total_ops;
    }

    result
}

#[cfg(feature = "profile")]
fn accum_create_close(
    current: ThreadId,
    create_num: u64,
    create_args: [u64; 6],
    ref_slot: usize,
) -> [u64; 32] {
    use crate::frame::profile;

    let space_id = crate::syscall::thread_space_id(current).ok();

    for _ in 0..PROFILE_N {
        let (err, h) = crate::syscall::dispatch(current, space_id, 0, create_num, &create_args);

        if err == 0 {
            crate::syscall::dispatch(current, space_id, 0, num::HANDLE_CLOSE, &[h, 0, 0, 0, 0, 0]);
        }
    }

    let mut accum = [0u64; 32];

    for _ in 0..PROFILE_OUTER {
        for _ in 0..PROFILE_N {
            profile::reset();
            let (err, h) = crate::syscall::dispatch(current, space_id, 0, create_num, &create_args);
            let t = profile::read();
            let r = t[ref_slot];

            if err == 0 {
                crate::syscall::dispatch(
                    current,
                    space_id,
                    0,
                    num::HANDLE_CLOSE,
                    &[h, 0, 0, 0, 0, 0],
                );
            }

            if r == 0 {
                continue;
            }

            for s in 0..32 {
                if t[s] > 0 && t[s] >= r {
                    accum[s] += t[s] - r;
                }
            }
        }
    }

    let total_ops = (PROFILE_OUTER * PROFILE_N) as u64;
    let mut result = [0u64; 32];

    for s in 0..32 {
        result[s] = accum[s] * 1875 / total_ops;
    }

    result
}

#[cfg(feature = "profile")]
fn accum_dup_close(current: ThreadId, src_handle: u64, ref_slot: usize) -> [u64; 32] {
    use crate::frame::profile;

    let space_id = crate::syscall::thread_space_id(current).ok();

    for _ in 0..PROFILE_N {
        let (err, dup) = crate::syscall::dispatch(
            current,
            space_id,
            0,
            num::HANDLE_DUP,
            &[src_handle, Rights::ALL.0 as u64, 0, 0, 0, 0],
        );

        if err == 0 {
            crate::syscall::dispatch(
                current,
                space_id,
                0,
                num::HANDLE_CLOSE,
                &[dup, 0, 0, 0, 0, 0],
            );
        }
    }

    let mut accum = [0u64; 32];

    for _ in 0..PROFILE_OUTER {
        for _ in 0..PROFILE_N {
            profile::reset();
            let (err, dup) = crate::syscall::dispatch(
                current,
                space_id,
                0,
                num::HANDLE_DUP,
                &[src_handle, Rights::ALL.0 as u64, 0, 0, 0, 0],
            );
            let t = profile::read();
            let r = t[ref_slot];

            if err == 0 {
                crate::syscall::dispatch(
                    current,
                    space_id,
                    0,
                    num::HANDLE_CLOSE,
                    &[dup, 0, 0, 0, 0, 0],
                );
            }

            if r == 0 {
                continue;
            }

            for s in 0..32 {
                if t[s] > 0 && t[s] >= r {
                    accum[s] += t[s] - r;
                }
            }
        }
    }

    let total_ops = (PROFILE_OUTER * PROFILE_N) as u64;
    let mut result = [0u64; 32];

    for s in 0..32 {
        result[s] = accum[s] * 1875 / total_ops;
    }

    result
}

#[cfg(feature = "profile")]
fn stage(c: &[u64; 32], from: usize, to: usize) -> u64 {
    c[to].saturating_sub(c[from])
}

#[cfg(feature = "profile")]
fn run_profile(current: ThreadId) {
    use crate::frame::profile::slot;

    let space_id = crate::syscall::thread_space_id(current).ok();

    crate::println!(
        "--- cycle profile ({}x{} accumulated, 24MHz→4.5GHz) ---",
        PROFILE_OUTER,
        PROFILE_N,
    );
    // ── SVC null (EL1 slow path) ─────────────────────────────────
    crate::println!();
    crate::println!("  SVC null (EL1, full TrapFrame path):");

    let c = accum_svc(slot::BENCH_BEFORE);

    print_stage("total", stage(&c, slot::BENCH_BEFORE, slot::BENCH_AFTER));
    print_stage(
        "trap + GPR/FP save → asm_before_handler",
        stage(&c, slot::BENCH_BEFORE, slot::ASM_BEFORE_HANDLER),
    );
    print_stage(
        "asm → Rust handler entry",
        stage(&c, slot::ASM_BEFORE_HANDLER, slot::HANDLER_ENTRY),
    );
    print_stage(
        "percpu read (ESR decode + space lookup)",
        stage(&c, slot::HANDLER_ENTRY, slot::HANDLER_PERCPU_DONE),
    );
    print_stage(
        "dispatch (match + error return)",
        stage(&c, slot::DISPATCH_ENTER, slot::DISPATCH_EXIT),
    );
    print_stage(
        "handler cleanup + return to asm",
        stage(&c, slot::HANDLER_EXIT, slot::ASM_AFTER_HANDLER),
    );
    print_stage(
        "FP/GPR restore + msr ELR/SPSR + eret",
        stage(&c, slot::ASM_AFTER_HANDLER, slot::BENCH_AFTER),
    );

    // ── dispatch(invalid) ────────────────────────────────────────
    crate::println!();
    crate::println!("  dispatch(invalid):");

    let c = accum_dispatch(current, 255, [0; 6], slot::DISPATCH_ENTER);

    print_stage(
        "total",
        stage(&c, slot::DISPATCH_ENTER, slot::DISPATCH_EXIT),
    );

    // ── clock_read ───────────────────────────────────────────────
    crate::println!();
    crate::println!("  clock_read:");

    let c = accum_dispatch(current, num::CLOCK_READ, [0; 6], slot::DISPATCH_ENTER);

    print_stage(
        "total",
        stage(&c, slot::DISPATCH_ENTER, slot::DISPATCH_EXIT),
    );
    print_stage(
        "  work (mrs cntvct + conversion)",
        stage(&c, slot::SYS_WORK, slot::DISPATCH_EXIT),
    );

    // ── handle_info ──────────────────────────────────────────────
    let (_, vmo_h) = crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::VMO_CREATE,
        &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
    );

    crate::println!();
    crate::println!("  handle_info:");

    let c = accum_dispatch(
        current,
        num::HANDLE_INFO,
        [vmo_h, 0, 0, 0, 0, 0],
        slot::DISPATCH_ENTER,
    );

    print_stage(
        "total",
        stage(&c, slot::DISPATCH_ENTER, slot::DISPATCH_EXIT),
    );
    print_stage(
        "  thread_space_id",
        stage(&c, slot::DISPATCH_ENTER, slot::SYS_SPACE_ID),
    );
    print_stage(
        "  lookup_handle (gen check)",
        stage(&c, slot::SYS_SPACE_ID, slot::SYS_HANDLE_LOOKUP),
    );
    print_stage(
        "  result packing + return",
        stage(&c, slot::SYS_HANDLE_LOOKUP, slot::DISPATCH_EXIT),
    );

    // ── handle_dup ───────────────────────────────────────────────
    crate::println!();
    crate::println!("  handle_dup:");

    let c = accum_dup_close(current, vmo_h, slot::DISPATCH_ENTER);

    print_stage(
        "total",
        stage(&c, slot::DISPATCH_ENTER, slot::DISPATCH_EXIT),
    );
    print_stage(
        "  thread_space_id",
        stage(&c, slot::DISPATCH_ENTER, slot::SYS_SPACE_ID),
    );
    print_stage(
        "  lookup_handle",
        stage(&c, slot::SYS_SPACE_ID, slot::SYS_HANDLE_LOOKUP),
    );
    print_stage(
        "  duplicate + add_ref",
        stage(&c, slot::SYS_HANDLE_LOOKUP, slot::SYS_WORK),
    );

    // ── vmo_snapshot ─────────────────────────────────────────────
    crate::println!();
    crate::println!("  vmo_snapshot+close:");

    let c = accum_create_close(
        current,
        num::VMO_SNAPSHOT,
        [vmo_h, 0, 0, 0, 0, 0],
        slot::DISPATCH_ENTER,
    );

    print_stage(
        "total",
        stage(&c, slot::DISPATCH_ENTER, slot::DISPATCH_EXIT),
    );
    print_stage(
        "  thread_space_id",
        stage(&c, slot::DISPATCH_ENTER, slot::SYS_SPACE_ID),
    );
    print_stage(
        "  lookup_handle",
        stage(&c, slot::SYS_SPACE_ID, slot::SYS_HANDLE_LOOKUP),
    );
    print_stage(
        "  clone_for_snapshot",
        stage(&c, slot::SYS_HANDLE_LOOKUP, slot::SYS_WORK),
    );
    print_stage("  alloc_shared", stage(&c, slot::SYS_WORK, slot::SYS_ALLOC));
    print_stage(
        "  handle install",
        stage(&c, slot::SYS_ALLOC, slot::SYS_HANDLE_INSTALL),
    );

    crate::syscall::dispatch(
        current,
        space_id,
        0,
        num::HANDLE_CLOSE,
        &[vmo_h, 0, 0, 0, 0, 0],
    );

    // ── endpoint_create ──────────────────────────────────────────
    crate::println!();
    crate::println!("  endpoint_create+close:");

    let c = accum_create_close(current, num::ENDPOINT_CREATE, [0; 6], slot::DISPATCH_ENTER);

    print_stage(
        "total",
        stage(&c, slot::DISPATCH_ENTER, slot::DISPATCH_EXIT),
    );
    print_stage(
        "  thread_space_id",
        stage(&c, slot::DISPATCH_ENTER, slot::SYS_SPACE_ID),
    );
    print_stage(
        "  alloc_shared",
        stage(&c, slot::SYS_SPACE_ID, slot::SYS_ALLOC),
    );
    print_stage(
        "  handle install",
        stage(&c, slot::SYS_ALLOC, slot::SYS_HANDLE_INSTALL),
    );

    // ── IPC null round-trip ──────────────────────────────────────
    crate::println!();
    crate::println!("  IPC null round-trip (CALL fast path):");

    crate::println!("  (IPC profiling skipped — IPC_AFTER_SWITCH stamp deadlocks)");
    crate::println!();
    return;

    let ipc_env = setup_ipc_bench(current);
    let mut accum = [0u64; 32];
    let ipc_ref = slot::IPC_EP_LOOKUP;

    for _ in 0..10 {
        ipc_null_iteration(&ipc_env, current);
    }

    for _ in 0..10 {
        for _ in 0..100 {
            crate::frame::profile::reset();

            ipc_null_iteration(&ipc_env, current);

            let t = crate::frame::profile::read();
            let r = t[ipc_ref];

            if r == 0 {
                continue;
            }

            for s in 0..32 {
                if t[s] > 0 && t[s] >= r {
                    accum[s] += t[s] - r;
                }
            }
        }
    }

    let ipc_total_ops = (10 * 100) as u64;
    let mut c = [0u64; 32];

    for s in 0..32 {
        c[s] = accum[s] * 1875 / ipc_total_ops;
    }

    print_stage(
        "peer check (read ep)",
        stage(&c, slot::IPC_EP_LOOKUP, slot::IPC_PEER_CHECK),
    );
    print_stage(
        "read_user_message",
        stage(&c, slot::IPC_PEER_CHECK, slot::IPC_MSG_READ),
    );
    print_stage(
        "remove_handles_atomic",
        stage(&c, slot::IPC_MSG_READ, slot::IPC_HANDLE_STAGE),
    );
    print_stage(
        "pop_recv_waiter (write ep)",
        stage(&c, slot::IPC_HANDLE_STAGE, slot::IPC_RECV_POP),
    );
    print_stage(
        "switch_to_space_of (TTBR0)",
        stage(&c, slot::IPC_RECV_POP, slot::IPC_SPACE_SWITCH),
    );
    print_stage(
        "write message to server buf",
        stage(&c, slot::IPC_SPACE_SWITCH, slot::IPC_MSG_WRITE),
    );
    print_stage(
        "install handles into server",
        stage(&c, slot::IPC_MSG_WRITE, slot::IPC_HANDLE_INSTALL),
    );
    print_stage(
        "allocate reply_cap",
        stage(&c, slot::IPC_HANDLE_INSTALL, slot::IPC_REPLY_CAP),
    );
    print_stage(
        "priority boost",
        stage(&c, slot::IPC_REPLY_CAP, slot::IPC_PRIORITY),
    );
    print_stage(
        "before_switch check",
        stage(&c, slot::IPC_PRIORITY, slot::IPC_BEFORE_SWITCH),
    );
    print_stage(
        "direct_switch (block+switch+resume)",
        stage(&c, slot::IPC_BEFORE_SWITCH, slot::IPC_AFTER_SWITCH),
    );

    teardown_ipc_bench(&ipc_env, current);

    crate::println!();
}
