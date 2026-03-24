//! Kernel stress test — saturates IPC, scheduler, and timer paths.
//!
//! Creates channel pairs and worker threads that rapidly signal/wait,
//! reproducing the syscall pattern that triggers kernel crashes under
//! high concurrency. Runs headless (no devices needed).
//!
//! Each worker thread does a tight ping-pong loop:
//!   wait([my_channel]) → channel_signal(peer_channel) → repeat
//!
//! The main thread creates/destroys timers to stress the timer table and
//! allocator simultaneously.

#![no_std]
#![no_main]

const ITERATIONS: u64 = 10_000_000;
const NUM_PAIRS: usize = 3;
const TIMER_ITERATIONS: u64 = 1_000_000;

fn print_u64(mut n: u64) {
    if n == 0 {
        sys::print(b"0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 20;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys::print(&buf[i..]);
}

/// Worker thread entry point. Packed arguments in a u64:
///   bits [7:0]   = wait handle (my receive endpoint)
///   bits [15:8]  = signal handle (peer's receive endpoint)
///   bits [63:16] = iteration count
extern "C" fn worker_entry(args: u64) -> ! {
    let wait_h = (args & 0xFF) as u8;
    let signal_h = ((args >> 8) & 0xFF) as u8;
    let iters = args >> 16;

    for _ in 0..iters {
        // Signal peer, then wait for peer to signal back.
        let _ = sys::channel_signal(sys::ChannelHandle(signal_h));
        let _ = sys::wait(&[wait_h], u64::MAX);
    }

    sys::exit();
}

/// Timer stress: rapidly create and destroy timers.
extern "C" fn timer_worker(_args: u64) -> ! {
    for _ in 0..TIMER_ITERATIONS {
        // Create a short timer, wait on it (poll mode), then close it.
        if let Ok(h) = sys::timer_create(1_000) {
            // Poll (timeout=0) — don't actually block, just exercise the path.
            let _ = sys::wait(&[h.0], 0);
            let _ = sys::handle_close(h.0);
        }
    }

    sys::exit();
}

/// Allocate a user stack for a new thread. Returns the stack top VA.
fn alloc_thread_stack() -> u64 {
    const STACK_PAGES: u64 = 4; // 16 KiB
    match sys::memory_alloc(STACK_PAGES) {
        Ok(va) => (va + (STACK_PAGES as usize * 4096)) as u64,
        Err(_) => {
            sys::print(b"stress: stack alloc failed\n");
            sys::exit();
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x94\xA5 stress test starting\n");
    sys::print(b"     ");
    print_u64(NUM_PAIRS as u64);
    sys::print(b" channel pairs, ");
    print_u64(ITERATIONS);
    sys::print(b" iterations/worker, ");
    print_u64(TIMER_ITERATIONS);
    sys::print(b" timer ops\n");

    // Create channel pairs and worker threads.
    // Each pair: endpoint A (even index) and endpoint B (odd index).
    // Worker on side A: wait on A, signal B.
    // Worker on side B: wait on B, signal A.
    let mut thread_handles: [u8; NUM_PAIRS * 2 + 1] = [0; NUM_PAIRS * 2 + 1];
    let mut thread_count: usize = 0;

    for pair in 0..NUM_PAIRS {
        let (ep_a, ep_b) = match sys::channel_create() {
            Ok(pair) => pair,
            Err(_) => {
                sys::print(b"stress: channel_create failed\n");
                sys::exit();
            }
        };

        // Pack arguments: wait_handle | (signal_handle << 8) | (iters << 16)
        let args_a: u64 = ep_a.0 as u64 | ((ep_b.0 as u64) << 8) | (ITERATIONS << 16);
        let args_b: u64 = ep_b.0 as u64 | ((ep_a.0 as u64) << 8) | (ITERATIONS << 16);

        let stack_a = alloc_thread_stack();
        let stack_b = alloc_thread_stack();

        // Threads receive their packed args in x0 (first argument register).
        // We use a trampoline that the thread_create entry point convention
        // passes x0 = 0. Instead, write args to the stack and have the
        // thread read from there.
        //
        // Actually, thread_create(entry_va, stack_top) starts the thread at
        // entry_va with SP=stack_top. x0 is undefined. We need a way to pass
        // args. Use the stack: push args below stack_top, set SP accordingly,
        // and have the worker read from SP.

        // Write args onto the stack (below stack_top).
        let args_a_ptr = (stack_a - 8) as *mut u64;
        unsafe { core::ptr::write_volatile(args_a_ptr, args_a) };
        let args_b_ptr = (stack_b - 8) as *mut u64;
        unsafe { core::ptr::write_volatile(args_b_ptr, args_b) };

        // Thread entry reads args from [SP], so set SP = stack_top - 8.
        match sys::thread_create(worker_trampoline as u64, stack_a - 16) {
            Ok(h) => {
                thread_handles[thread_count] = h.0;
                thread_count += 1;
            }
            Err(_) => {
                sys::print(b"stress: thread_create failed (A)\n");
                sys::exit();
            }
        }

        match sys::thread_create(worker_trampoline as u64, stack_b - 16) {
            Ok(h) => {
                thread_handles[thread_count] = h.0;
                thread_count += 1;
            }
            Err(_) => {
                sys::print(b"stress: thread_create failed (B)\n");
                sys::exit();
            }
        }

        // Kick off the first signal so the ping-pong starts.
        let _ = sys::channel_signal(ep_a);

        sys::print(b"     pair ");
        print_u64(pair as u64);
        sys::print(b": handles ");
        print_u64(ep_a.0 as u64);
        sys::print(b"/");
        print_u64(ep_b.0 as u64);
        sys::print(b" started\n");
    }

    // Start a timer stress thread too.
    let timer_stack = alloc_thread_stack();
    // Timer worker doesn't need args — write 0.
    let timer_args_ptr = (timer_stack - 8) as *mut u64;
    unsafe { core::ptr::write_volatile(timer_args_ptr, 0) };

    match sys::thread_create(timer_trampoline as u64, timer_stack - 16) {
        Ok(h) => {
            thread_handles[thread_count] = h.0;
            thread_count += 1;
        }
        Err(_) => {
            sys::print(b"stress: timer thread failed\n");
        }
    }

    sys::print(b"     ");
    print_u64(thread_count as u64);
    sys::print(b" worker threads running\n");

    // Wait for all threads to complete.
    for i in 0..thread_count {
        let _ = sys::wait(&[thread_handles[i]], u64::MAX);
    }

    sys::print(b"  \xE2\x9C\x85 stress test PASS\n");
    sys::exit();
}

/// Trampoline: reads packed args from the stack and calls worker_entry.
#[unsafe(no_mangle)]
extern "C" fn worker_trampoline() -> ! {
    let args: u64;
    unsafe {
        // Args were written at SP+8 by the parent (stack_top - 8).
        // thread_create set SP = stack_top - 16, so args are at SP+8.
        // No `nomem`: this reads from the stack — LLVM must not reorder
        // stores to [sp, #8] past this load.
        core::arch::asm!(
            "ldr {0}, [sp, #8]",
            out(reg) args,
            options(nostack)
        );
    }
    worker_entry(args);
}

/// Timer worker trampoline.
#[unsafe(no_mangle)]
extern "C" fn timer_trampoline() -> ! {
    let args: u64;
    unsafe {
        // No `nomem`: reads from stack memory.
        core::arch::asm!(
            "ldr {0}, [sp, #8]",
            out(reg) args,
            options(nostack)
        );
    }
    timer_worker(args);
}
