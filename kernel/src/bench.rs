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
    thread::{Thread, ThreadRunState},
    types::{AddressSpaceId, HandleId, ObjectType, Priority, Rights, ThreadId},
};

const WARMUP: usize = 10;
const ITERATIONS: usize = 1000;
const BATCH_N: usize = 500;
const BATCH_SAMPLES: usize = 100;

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

struct CycleEstimate {
    name: &'static str,
    cycles_x10: u64,
    theoretical: u64,
}

fn ticks_to_cycles_x10(total_ticks: u64, batch_size: usize) -> u64 {
    // 1 tick at 24 MHz = 187.5 CPU cycles at 4.5 GHz.
    // Returns cycles × 10 for one decimal place of precision.
    total_ticks * 1875 / batch_size as u64
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

fn bench_batched_dispatch(
    kern: &mut Kernel,
    current: ThreadId,
    syscall_num: u64,
    args: [u64; 6],
) -> u64 {
    for _ in 0..BATCH_N {
        kern.dispatch(current, 0, syscall_num, &args);
    }

    let mut samples = [0u64; BATCH_SAMPLES];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        for _ in 0..BATCH_N {
            kern.dispatch(current, 0, syscall_num, &args);
        }

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    samples.sort_unstable();
    samples[BATCH_SAMPLES / 2]
}

fn bench_batched_create_close(
    kern: &mut Kernel,
    current: ThreadId,
    create_num: u64,
    create_args: [u64; 6],
) -> u64 {
    for _ in 0..BATCH_N {
        let (err, h) = kern.dispatch(current, 0, create_num, &create_args);

        if err == 0 {
            kern.dispatch(current, 0, num::HANDLE_CLOSE, &[h, 0, 0, 0, 0, 0]);
        }
    }

    let mut samples = [0u64; BATCH_SAMPLES];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        for _ in 0..BATCH_N {
            let (err, h) = kern.dispatch(current, 0, create_num, &create_args);

            if err == 0 {
                kern.dispatch(current, 0, num::HANDLE_CLOSE, &[h, 0, 0, 0, 0, 0]);
            }
        }

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    samples.sort_unstable();
    samples[BATCH_SAMPLES / 2]
}

fn setup_bench_env(kern: &mut Kernel) -> ThreadId {
    let asid = kern.alloc_asid().expect("bench: asid alloc");
    let space = AddressSpace::new(AddressSpaceId(0), asid, 0);
    let (space_idx, space_gen) = kern.spaces.alloc(space).expect("bench: space alloc");

    kern.spaces.get_mut(space_idx).unwrap().id = AddressSpaceId(space_idx);
    #[cfg(target_os = "none")]
    kern.spaces
        .get_mut(space_idx)
        .unwrap()
        .set_aslr_seed(crate::frame::arch::entropy::random_u64());

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

    // ── Workload benchmarks ─────────────────────────────────────────
    crate::println!("--- workloads ---");

    results.push(bench_document_editing(kern, current));
    results.push(bench_ipc_storm(kern, current));
    results.push(bench_object_lifecycle_churn(kern, current));

    for r in results.iter().skip(results.len() - 3) {
        crate::println!(
            "  {:30} median {:>6}  P99 {:>6}  [{}]",
            r.name,
            r.median,
            r.p99,
            if r.passed() {
                "PASS"
            } else {
                all_pass = false;
                "FAIL"
            },
        );
    }

    if all_pass {
        crate::println!("benchmarks: all passed");
    } else {
        crate::println!("benchmarks: STRUCTURAL REGRESSION DETECTED");
    }

    run_cycle_estimates(kern, current);
    teardown_bench_env(kern, current);
}

fn teardown_bench_env(kern: &mut Kernel, thread_id: ThreadId) {
    let space_id = kern
        .threads
        .get(thread_id.0)
        .unwrap()
        .address_space()
        .unwrap();

    kern.scheduler.remove(thread_id);
    kern.threads.dealloc(thread_id.0);
    kern.alive_threads = kern.alive_threads.saturating_sub(1);

    let space = kern.spaces.get_mut(space_id.0).unwrap();

    space.set_thread_head(None);
    kern.spaces.dealloc(space_id.0);
}

fn bench_document_editing(kern: &mut Kernel, current: ThreadId) -> BenchResult {
    let page = config::PAGE_SIZE as u64;

    for _ in 0..WARMUP {
        document_editing_iteration(kern, current, page);
    }

    let mut samples = [0u64; ITERATIONS];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        document_editing_iteration(kern, current, page);

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    let (median, p99) = stats(&mut samples);

    BenchResult {
        name: "workload: doc editing",
        median,
        p99,
        threshold_mult: 50000,
    }
}

fn document_editing_iteration(kern: &mut Kernel, current: ThreadId, page: u64) {
    // Simulate: create VMO (document content), map it, snapshot (undo point),
    // create event (compositor notify), signal it, close everything.
    let (_, vmo) = kern.dispatch(current, 0, num::VMO_CREATE, &[page, 0, 0, 0, 0, 0]);
    let (_, snap) = kern.dispatch(current, 0, num::VMO_SNAPSHOT, &[vmo, 0, 0, 0, 0, 0]);
    let (_, evt) = kern.dispatch(current, 0, num::EVENT_CREATE, &[0; 6]);

    kern.dispatch(current, 0, num::EVENT_SIGNAL, &[evt, 0x1, 0, 0, 0, 0]);
    kern.dispatch(current, 0, num::EVENT_CLEAR, &[evt, 0x1, 0, 0, 0, 0]);
    kern.dispatch(current, 0, num::HANDLE_CLOSE, &[evt, 0, 0, 0, 0, 0]);
    kern.dispatch(current, 0, num::HANDLE_CLOSE, &[snap, 0, 0, 0, 0, 0]);
    kern.dispatch(current, 0, num::HANDLE_CLOSE, &[vmo, 0, 0, 0, 0, 0]);
}

fn bench_ipc_storm(kern: &mut Kernel, current: ThreadId) -> BenchResult {
    let (_, ep) = kern.dispatch(current, 0, num::ENDPOINT_CREATE, &[0; 6]);

    for _ in 0..WARMUP {
        ipc_storm_iteration(kern, current, ep);
    }

    let mut samples = [0u64; ITERATIONS];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        ipc_storm_iteration(kern, current, ep);

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    kern.dispatch(current, 0, num::HANDLE_CLOSE, &[ep, 0, 0, 0, 0, 0]);

    let (median, p99) = stats(&mut samples);

    BenchResult {
        name: "workload: IPC storm (10 calls)",
        median,
        p99,
        threshold_mult: 100000,
    }
}

fn ipc_storm_iteration(kern: &mut Kernel, current: ThreadId, ep: u64) {
    let mut buf = [0u8; 128];

    // 10 rapid call attempts — each will enqueue a pending call (thread blocks
    // then we restore). We measure the enqueue path, which is the hot path.
    for _ in 0..10 {
        kern.dispatch(
            current,
            0,
            num::CALL,
            &[ep, buf.as_mut_ptr() as u64, 8, 0, 0, 0],
        );

        // Restore thread for next iteration (thread blocked on call).
        if let Some(t) = kern.threads.get_mut(current.0)
            && t.state() == crate::thread::ThreadRunState::Blocked
        {
            t.set_state(crate::thread::ThreadRunState::Ready);
            t.set_state(crate::thread::ThreadRunState::Running);
        }
    }

    // Drain the endpoint so it doesn't fill up across iterations.
    if let Ok(handle) = kern
        .spaces
        .get(0)
        .unwrap()
        .handles()
        .lookup(crate::types::HandleId(ep as u32))
    {
        let ep_id = handle.object_id;

        if let Some(endpoint) = kern.endpoints.get_mut(ep_id) {
            while endpoint.dequeue_call().is_some() {}
        }
    }
}

fn bench_object_lifecycle_churn(kern: &mut Kernel, current: ThreadId) -> BenchResult {
    for _ in 0..WARMUP {
        object_churn_iteration(kern, current);
    }

    let mut samples = [0u64; ITERATIONS];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        object_churn_iteration(kern, current);

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    let (median, p99) = stats(&mut samples);

    BenchResult {
        name: "workload: object churn",
        median,
        p99,
        threshold_mult: 100000,
    }
}

fn object_churn_iteration(kern: &mut Kernel, current: ThreadId) {
    let page = config::PAGE_SIZE as u64;
    let mut handles = [0u64; 8];

    // Create 4 VMOs + 2 events + 2 endpoints
    for h in &mut handles[..4] {
        let (_, hid) = kern.dispatch(current, 0, num::VMO_CREATE, &[page, 0, 0, 0, 0, 0]);

        *h = hid;
    }
    for h in &mut handles[4..6] {
        let (_, hid) = kern.dispatch(current, 0, num::EVENT_CREATE, &[0; 6]);

        *h = hid;
    }
    for h in &mut handles[6..8] {
        let (_, hid) = kern.dispatch(current, 0, num::ENDPOINT_CREATE, &[0; 6]);

        *h = hid;
    }

    // Close all in reverse order — exercises different table paths
    for h in handles.iter().rev() {
        kern.dispatch(current, 0, num::HANDLE_CLOSE, &[*h, 0, 0, 0, 0, 0]);
    }
}

struct IpcBenchEnv {
    server: ThreadId,
    client_ep_h: u64,
    server_ep_h: u64,
    ep_obj_id: u32,
    server_space_idx: u32,
}

fn setup_ipc_bench(kern: &mut Kernel, client: ThreadId) -> IpcBenchEnv {
    let asid = kern.alloc_asid().expect("ipc bench: server asid");
    let space = AddressSpace::new(AddressSpaceId(0), asid, 0);
    let (space_idx, space_gen) = kern.spaces.alloc(space).expect("ipc bench: server space");

    kern.spaces.get_mut(space_idx).unwrap().id = AddressSpaceId(space_idx);
    #[cfg(target_os = "none")]
    kern.spaces
        .get_mut(space_idx)
        .unwrap()
        .set_aslr_seed(crate::frame::arch::entropy::random_u64());

    let server_space = kern.spaces.get_mut(space_idx).unwrap();

    server_space
        .handles_mut()
        .allocate(ObjectType::AddressSpace, space_idx, Rights::ALL, space_gen)
        .expect("ipc bench: space handle");

    let thread = Thread::new(
        ThreadId(0),
        Some(AddressSpaceId(space_idx)),
        Priority::Medium,
        0x3000,
        0x4000,
        0,
    );
    let (server_idx, _) = kern
        .threads
        .alloc(thread)
        .expect("ipc bench: server thread");

    kern.threads.get_mut(server_idx).unwrap().id = ThreadId(server_idx);
    kern.alive_threads += 1;

    let (err, client_ep_h) = kern.dispatch(client, 0, num::ENDPOINT_CREATE, &[0; 6]);

    assert_eq!(err, 0);

    let client_space_id = kern.thread_space_id(client).unwrap();
    let handle = kern
        .spaces
        .get(client_space_id.0)
        .unwrap()
        .handles()
        .lookup(HandleId(client_ep_h as u32))
        .unwrap();
    let ep_obj_id = handle.object_id;
    let ep_gen = handle.generation;
    let server_space = kern.spaces.get_mut(space_idx).unwrap();
    let server_ep_h = server_space
        .handles_mut()
        .allocate(ObjectType::Endpoint, ep_obj_id, Rights::ALL, ep_gen)
        .expect("ipc bench: server ep handle");

    kern.endpoints.get_mut(ep_obj_id).unwrap().add_ref();

    IpcBenchEnv {
        server: ThreadId(server_idx),
        client_ep_h,
        server_ep_h: server_ep_h.0 as u64,
        ep_obj_id,
        server_space_idx: space_idx,
    }
}

fn force_running(kern: &mut Kernel, tid: ThreadId) {
    kern.scheduler.remove(tid);

    if let Some(t) = kern.threads.get_mut(tid.0) {
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

fn bench_ipc_null_round_trip(kern: &mut Kernel, env: &IpcBenchEnv, client: ThreadId) -> u64 {
    for _ in 0..BATCH_N {
        ipc_null_iteration(kern, env, client);
    }

    let mut samples = [0u64; BATCH_SAMPLES];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        for _ in 0..BATCH_N {
            ipc_null_iteration(kern, env, client);
        }

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    samples.sort_unstable();
    samples[BATCH_SAMPLES / 2]
}

fn ipc_null_iteration(kern: &mut Kernel, env: &IpcBenchEnv, client: ThreadId) {
    kern.endpoints
        .get_mut(env.ep_obj_id)
        .unwrap()
        .add_recv_waiter(env.server)
        .ok();

    let server_space_id = kern
        .thread_space_id(env.server)
        .unwrap_or(AddressSpaceId(env.server_space_idx));

    if let Some(t) = kern.threads.get_mut(env.server.0) {
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

    kern.dispatch(client, 0, num::CALL, &[env.client_ep_h, 0, 0, 0, 0, 0]);

    force_running(kern, env.server);

    let (_, packed) = kern.dispatch(env.server, 0, num::RECV, &[env.server_ep_h, 0, 0, 0, 0, 0]);
    let reply_cap = packed >> 32;

    kern.dispatch(
        env.server,
        0,
        num::REPLY,
        &[env.server_ep_h, reply_cap, 0, 0, 0, 0],
    );

    force_running(kern, client);
    force_running(kern, env.server);
}

fn teardown_ipc_bench(kern: &mut Kernel, env: &IpcBenchEnv, client: ThreadId) {
    kern.dispatch(
        client,
        0,
        num::HANDLE_CLOSE,
        &[env.client_ep_h, 0, 0, 0, 0, 0],
    );
    kern.scheduler.remove(env.server);
    kern.threads.dealloc(env.server.0);
    kern.alive_threads = kern.alive_threads.saturating_sub(1);

    let space = kern.spaces.get_mut(env.server_space_idx).unwrap();

    space.set_thread_head(None);
    kern.spaces.dealloc(env.server_space_idx);
}

fn bench_fault_lookup(kern: &mut Kernel, current: ThreadId) -> u64 {
    let page = config::PAGE_SIZE as u64;
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let space_id = kern.thread_space_id(current).unwrap();
    let (_, vmo_h) = kern.dispatch(current, 0, num::VMO_CREATE, &[page * 4, 0, 0, 0, 0, 0]);
    let (_, va) = kern.dispatch(current, 0, num::VMO_MAP, &[vmo_h, 0, rw.0 as u64, 0, 0, 0]);
    let fault_addr = va as usize + config::PAGE_SIZE;

    for _ in 0..BATCH_N {
        let space = kern.spaces.get(space_id.0).unwrap();
        let _ = space.find_mapping(fault_addr);
    }

    let mut samples = [0u64; BATCH_SAMPLES];

    for s in &mut samples {
        arch::isb();

        let start = arch::read_cycle_counter();

        for _ in 0..BATCH_N {
            let space = kern.spaces.get(space_id.0).unwrap();
            let mapping = space.find_mapping(fault_addr);

            if let Some(m) = mapping {
                let vmo_id = m.vmo_id;
                let page_idx = (fault_addr - m.va_start) / config::PAGE_SIZE;
                let _ = kern.vmos.get(vmo_id.0).map(|v| v.page_at(page_idx));
            }
        }

        arch::isb();

        *s = arch::read_cycle_counter().wrapping_sub(start);
    }

    kern.dispatch(current, 0, num::VMO_UNMAP, &[va, 0, 0, 0, 0, 0]);
    kern.dispatch(current, 0, num::HANDLE_CLOSE, &[vmo_h, 0, 0, 0, 0, 0]);
    samples.sort_unstable();
    samples[BATCH_SAMPLES / 2]
}

fn run_cycle_estimates(kern: &mut Kernel, current: ThreadId) {
    crate::println!(
        "--- cycle estimates ({}x{} samples, 24MHz->4.5GHz) ---",
        BATCH_N,
        BATCH_SAMPLES,
    );

    let mut estimates: alloc::vec::Vec<CycleEstimate> = alloc::vec::Vec::new();

    // ── SVC null (real trap + ERET round-trip) ───────────────────
    {
        for _ in 0..BATCH_N {
            let _ = arch::svc_null();
        }

        let mut samples = [0u64; BATCH_SAMPLES];

        for s in &mut samples {
            arch::isb();

            let start = arch::read_cycle_counter();

            for _ in 0..BATCH_N {
                let _ = arch::svc_null();
            }

            arch::isb();

            *s = arch::read_cycle_counter().wrapping_sub(start);
        }

        samples.sort_unstable();
        estimates.push(CycleEstimate {
            name: "svc null (trap+eret)",
            cycles_x10: ticks_to_cycles_x10(samples[BATCH_SAMPLES / 2], BATCH_N),
            theoretical: 50,
        });
    }

    // ── Dispatch-only syscalls ───────────────────────────────────
    estimates.push(CycleEstimate {
        name: "dispatch overhead",
        cycles_x10: ticks_to_cycles_x10(
            bench_batched_dispatch(kern, current, 255, [0; 6]),
            BATCH_N,
        ),
        theoretical: 5,
    });
    estimates.push(CycleEstimate {
        name: "clock_read",
        cycles_x10: ticks_to_cycles_x10(
            bench_batched_dispatch(kern, current, num::CLOCK_READ, [0; 6]),
            BATCH_N,
        ),
        theoretical: 10,
    });
    estimates.push(CycleEstimate {
        name: "system_info",
        cycles_x10: ticks_to_cycles_x10(
            bench_batched_dispatch(kern, current, num::SYSTEM_INFO, [0; 6]),
            BATCH_N,
        ),
        theoretical: 10,
    });

    // ── Handle operations (need a live VMO) ──────────────────────
    let (_, vmo_h) = kern.dispatch(
        current,
        0,
        num::VMO_CREATE,
        &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
    );

    estimates.push(CycleEstimate {
        name: "handle_info",
        cycles_x10: ticks_to_cycles_x10(
            bench_batched_dispatch(kern, current, num::HANDLE_INFO, [vmo_h, 0, 0, 0, 0, 0]),
            BATCH_N,
        ),
        theoretical: 15,
    });

    {
        for _ in 0..BATCH_N {
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

        let mut samples = [0u64; BATCH_SAMPLES];

        for s in &mut samples {
            arch::isb();

            let start = arch::read_cycle_counter();

            for _ in 0..BATCH_N {
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

            arch::isb();

            *s = arch::read_cycle_counter().wrapping_sub(start);
        }

        samples.sort_unstable();
        estimates.push(CycleEstimate {
            name: "handle_dup+close",
            cycles_x10: ticks_to_cycles_x10(samples[BATCH_SAMPLES / 2], BATCH_N),
            theoretical: 30,
        });
    }

    // ── VMO snapshot+close ───────────────────────────────────────
    {
        for _ in 0..BATCH_N {
            let (err, snap) = kern.dispatch(current, 0, num::VMO_SNAPSHOT, &[vmo_h, 0, 0, 0, 0, 0]);

            if err == 0 {
                kern.dispatch(current, 0, num::HANDLE_CLOSE, &[snap, 0, 0, 0, 0, 0]);
            }
        }

        let mut samples = [0u64; BATCH_SAMPLES];

        for s in &mut samples {
            arch::isb();

            let start = arch::read_cycle_counter();

            for _ in 0..BATCH_N {
                let (err, snap) =
                    kern.dispatch(current, 0, num::VMO_SNAPSHOT, &[vmo_h, 0, 0, 0, 0, 0]);

                if err == 0 {
                    kern.dispatch(current, 0, num::HANDLE_CLOSE, &[snap, 0, 0, 0, 0, 0]);
                }
            }

            arch::isb();

            *s = arch::read_cycle_counter().wrapping_sub(start);
        }

        samples.sort_unstable();
        estimates.push(CycleEstimate {
            name: "vmo_snapshot+close",
            cycles_x10: ticks_to_cycles_x10(samples[BATCH_SAMPLES / 2], BATCH_N),
            theoretical: 60,
        });
    }

    kern.dispatch(current, 0, num::HANDLE_CLOSE, &[vmo_h, 0, 0, 0, 0, 0]);

    // ── Event operations ─────────────────────────────────────────
    let (_, evt_h) = kern.dispatch(current, 0, num::EVENT_CREATE, &[0; 6]);

    estimates.push(CycleEstimate {
        name: "event_signal",
        cycles_x10: ticks_to_cycles_x10(
            bench_batched_dispatch(kern, current, num::EVENT_SIGNAL, [evt_h, 0x1, 0, 0, 0, 0]),
            BATCH_N,
        ),
        theoretical: 15,
    });
    estimates.push(CycleEstimate {
        name: "event_clear",
        cycles_x10: ticks_to_cycles_x10(
            bench_batched_dispatch(kern, current, num::EVENT_CLEAR, [evt_h, 0x1, 0, 0, 0, 0]),
            BATCH_N,
        ),
        theoretical: 15,
    });
    kern.dispatch(current, 0, num::EVENT_SIGNAL, &[evt_h, 0xFF, 0, 0, 0, 0]);
    estimates.push(CycleEstimate {
        name: "event_wait (signaled)",
        cycles_x10: ticks_to_cycles_x10(
            bench_batched_dispatch(kern, current, num::EVENT_WAIT, [evt_h, 0xFF, 1, 0, 0, 0]),
            BATCH_N,
        ),
        theoretical: 15,
    });
    kern.dispatch(current, 0, num::HANDLE_CLOSE, &[evt_h, 0, 0, 0, 0, 0]);

    // ── Object create+close pairs ────────────────────────────────
    estimates.push(CycleEstimate {
        name: "vmo create+close",
        cycles_x10: ticks_to_cycles_x10(
            bench_batched_create_close(
                kern,
                current,
                num::VMO_CREATE,
                [config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
            ),
            BATCH_N,
        ),
        theoretical: 50,
    });
    estimates.push(CycleEstimate {
        name: "event create+close",
        cycles_x10: ticks_to_cycles_x10(
            bench_batched_create_close(kern, current, num::EVENT_CREATE, [0; 6]),
            BATCH_N,
        ),
        theoretical: 50,
    });
    estimates.push(CycleEstimate {
        name: "endpoint create+close",
        cycles_x10: ticks_to_cycles_x10(
            bench_batched_create_close(kern, current, num::ENDPOINT_CREATE, [0; 6]),
            BATCH_N,
        ),
        theoretical: 50,
    });

    // ── IPC round-trip ──────────────────────────────────────────
    let ipc_env = setup_ipc_bench(kern, current);

    estimates.push(CycleEstimate {
        name: "IPC null round-trip",
        cycles_x10: ticks_to_cycles_x10(
            bench_ipc_null_round_trip(kern, &ipc_env, current),
            BATCH_N,
        ),
        theoretical: 150,
    });

    teardown_ipc_bench(kern, &ipc_env, current);

    // ── Page fault kernel path ──────────────────────────────────
    estimates.push(CycleEstimate {
        name: "fault lookup+page_at",
        cycles_x10: ticks_to_cycles_x10(bench_fault_lookup(kern, current), BATCH_N),
        theoretical: 15,
    });

    // ── Print results ────────────────────────────────────────────
    let mut within_2x = 0u32;
    let mut total_rated = 0u32;

    for e in &estimates {
        let ratio_x10 = e.cycles_x10.checked_div(e.theoretical).unwrap_or(0);

        if e.theoretical > 0 {
            total_rated += 1;

            if ratio_x10 <= 20 {
                within_2x += 1;
            }
        }

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

    crate::println!(
        "  {}/{} within 2x of theoretical floor",
        within_2x,
        total_rated,
    );
}
