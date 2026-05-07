//! SMP benchmarks — measures cross-core IPC, object churn scaling, and wake
//! latency from EL0 with real syscalls and scheduling.
//!
//! Reports results via thread_exit args. The kernel reads a0-a5 and prints
//! cycle estimates. Each result is the total timer ticks for BATCH_N iterations.
//!
//! Result layout (thread_exit args):
//!   a0 = 0 (success)
//!   a1 = IPC 2-core: total ticks for BATCH_N round-trips
//!   a2 = object churn 1-core: total ticks for BATCH_N iterations
//!   a3 = object churn multi-core: wall-clock ticks for BATCH_N per worker
//!   a4 = wake round-trip: total ticks for BATCH_N signal-wake cycles
//!   a5 = BATCH_N | (num_churn_workers << 16)

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::{
    raw,
    types::{Handle, Rights, SyscallError},
};

const PAGE_SIZE: usize = 16384;
const MSG_SIZE: usize = 128;
const BATCH_N: usize = 500;
const WARMUP: usize = 100;

fn isb() {
    abi::system::isb();
}

fn ticks() -> u64 {
    abi::system::raw_counter()
}

// ── IPC 2-core round-trip ────────────────────────────────────────

fn bench_ipc_2core() -> u64 {
    let ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => return 0,
    };
    let stack_vmo = match abi::vmo::create(PAGE_SIZE * 4, 0) {
        Ok(h) => h,
        Err(_) => return 0,
    };
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let stack_va = match abi::vmo::map(stack_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => return 0,
    };
    let stack_top = stack_va + PAGE_SIZE * 4;
    let rounds = WARMUP + BATCH_N;
    let arg = (ep.0 as usize) | (rounds << 16);
    let server = match abi::thread::create(ipc_server_entry as *const () as usize, stack_top, arg) {
        Ok(h) => h,
        Err(_) => return 0,
    };
    let _ = abi::thread::set_affinity(server, 1);
    let mut buf = [0u8; MSG_SIZE];

    for _ in 0..WARMUP {
        let _ = abi::ipc::call(ep, &mut buf, 8, &[], &mut []);
    }

    isb();

    let start = ticks();

    for _ in 0..BATCH_N {
        let _ = abi::ipc::call(ep, &mut buf, 8, &[], &mut []);
    }

    isb();

    let elapsed = ticks() - start;
    let _ = abi::handle::close(ep);

    elapsed
}

extern "C" fn ipc_server_entry(arg: usize) -> ! {
    let ep = Handle((arg & 0xFFFF) as u32);
    let rounds = arg >> 16;

    for _ in 0..rounds {
        let mut msg_buf = [0u8; MSG_SIZE];
        let mut handles_buf = [0u32; 4];
        let recv = match abi::ipc::recv(ep, &mut msg_buf, &mut handles_buf) {
            Ok(r) => r,
            Err(_) => break,
        };

        if abi::ipc::reply(ep, recv.reply_cap, &msg_buf[..8], &[]).is_err() {
            break;
        }
    }

    abi::thread::exit(0);
}

// ── Object churn (1-core baseline) ──────────────────────────────

fn bench_churn_1core() -> u64 {
    for _ in 0..WARMUP {
        churn_iteration();
    }

    isb();

    let start = ticks();

    for _ in 0..BATCH_N {
        churn_iteration();
    }

    isb();

    ticks() - start
}

fn churn_iteration() {
    let vmo = abi::vmo::create(PAGE_SIZE, 0);
    let evt = abi::event::create();
    let ep = abi::ipc::endpoint_create();

    if let Ok(h) = ep {
        let _ = abi::handle::close(h);
    }
    if let Ok(h) = evt {
        let _ = abi::event::signal(h, 0xFF);
        let _ = abi::handle::close(h);
    }
    if let Ok(h) = vmo {
        let snap = abi::vmo::snapshot(h);

        if let Ok(s) = snap {
            let _ = abi::handle::close(s);
        }

        let _ = abi::handle::close(h);
    }
}

// ── Object churn (multi-core) ───────────────────────────────────

fn bench_churn_multicore(num_cores: usize) -> (u64, usize) {
    let worker_count = if num_cores > 4 { 4 } else { num_cores };
    let sync_event = match abi::event::create() {
        Ok(h) => h,
        Err(_) => return (0, 0),
    };
    let rounds = WARMUP + BATCH_N;
    let mut threads = [Handle(0); 4];
    let mut stack_vmos = [Handle(0); 4];

    for i in 0..worker_count {
        let stack_vmo = match abi::vmo::create(PAGE_SIZE * 4, 0) {
            Ok(h) => h,
            Err(_) => return (0, 0),
        };
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
        let stack_va = match abi::vmo::map(stack_vmo, 0, rw) {
            Ok(va) => va,
            Err(_) => return (0, 0),
        };
        let stack_top = stack_va + PAGE_SIZE * 4;
        let arg = (sync_event.0 as usize) | (i << 16) | (rounds << 32);
        let thread =
            match abi::thread::create(churn_worker_entry as *const () as usize, stack_top, arg) {
                Ok(h) => h,
                Err(_) => return (0, 0),
            };
        let _ = abi::thread::set_affinity(thread, i as u64);

        threads[i] = thread;
        stack_vmos[i] = stack_vmo;
    }

    let done_mask: u64 = (1u64 << worker_count) - 1;

    isb();

    let start = ticks();
    // Spin instead of event_wait to isolate deadlock
    for _ in 0..1_000_000 {
        core::hint::spin_loop();
    }

    isb();

    let elapsed = ticks() - start;
    let _ = abi::handle::close(sync_event);

    // Thread and stack VMO handles kept alive — workers may still be
    // between signaling done and calling thread_exit.

    (elapsed, worker_count)
}

extern "C" fn churn_worker_entry(arg: usize) -> ! {
    let event_handle = Handle((arg & 0xFFFF) as u32);
    let worker_id = (arg >> 16) & 0xFFFF;
    let iterations = arg >> 32;

    for _ in 0..iterations {
        churn_iteration();
    }

    let done_bit = 1u64 << worker_id;
    let _ = abi::event::signal(event_handle, done_bit);

    abi::thread::exit(0);
}

// ── Cross-core wake latency ─────────────────────────────────────

fn bench_wake_latency() -> u64 {
    let ping = match abi::event::create() {
        Ok(h) => h,
        Err(_) => return 0,
    };
    let pong = match abi::event::create() {
        Ok(h) => h,
        Err(_) => return 0,
    };
    let stack_vmo = match abi::vmo::create(PAGE_SIZE * 4, 0) {
        Ok(h) => h,
        Err(_) => return 0,
    };
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let stack_va = match abi::vmo::map(stack_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => return 0,
    };
    let stack_top = stack_va + PAGE_SIZE * 4;
    let rounds = WARMUP + BATCH_N;
    let arg = (ping.0 as usize) | ((pong.0 as usize) << 16) | (rounds << 32);
    let thread = match abi::thread::create(wake_pong_entry as *const () as usize, stack_top, arg) {
        Ok(h) => h,
        Err(_) => return 0,
    };
    let _ = abi::thread::set_affinity(thread, 1);

    for _ in 0..WARMUP {
        let _ = abi::event::signal(ping, 0x1);
        let _ = abi::event::wait(&[(pong, 0x1)]);
        let _ = abi::event::clear(pong, 0x1);
    }

    isb();

    let start = ticks();

    for _ in 0..BATCH_N {
        let _ = abi::event::signal(ping, 0x1);
        let _ = abi::event::wait(&[(pong, 0x1)]);
        let _ = abi::event::clear(pong, 0x1);
    }

    isb();

    let elapsed = ticks() - start;
    let _ = abi::handle::close(ping);
    let _ = abi::handle::close(pong);

    elapsed
}

extern "C" fn wake_pong_entry(arg: usize) -> ! {
    let ping = Handle((arg & 0xFFFF) as u32);
    let pong = Handle(((arg >> 16) & 0xFFFF) as u32);
    let rounds = arg >> 32;

    for _ in 0..rounds {
        if abi::event::wait(&[(ping, 0x1)]).is_err() {
            break;
        }

        let _ = abi::event::clear(ping, 0x1);
        let _ = abi::event::signal(pong, 0x1);
    }

    abi::thread::exit(0);
}

extern "C" fn noop_entry(_arg: usize) -> ! {
    abi::thread::exit(0);
}

// ── Entry point ──────────────────────────────────────────────────

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let num_cores = abi::system::info(abi::system::INFO_NUM_CORES).unwrap_or(1) as usize;
    // Single-core benchmark first — proves the binary runs.
    let churn_1core = bench_churn_1core();
    // Multi-core benchmarks (any of these may hang if scheduling is broken).
    let wake_ticks = if num_cores >= 2 {
        bench_wake_latency()
    } else {
        0
    };
    let ipc_ticks = if num_cores >= 2 { bench_ipc_2core() } else { 0 };
    let (churn_multi, worker_count) = if num_cores >= 2 {
        bench_churn_multicore(num_cores)
    } else {
        (0, 0)
    };

    loop {
        raw::syscall(
            raw::num::THREAD_EXIT,
            0xBEEF,
            ipc_ticks,
            churn_1core,
            churn_multi,
            wake_ticks,
            (BATCH_N as u64) | ((worker_count as u64) << 16),
        );
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
