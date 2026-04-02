//! Multi-threading — spawn a thread, synchronize with a futex.
//!
//! Demonstrates:
//! - Page allocation (syscall 14) — allocate a stack for the child thread
//! - Thread creation (syscall 35) — spawn a new thread in the same process
//! - Futex synchronization (syscalls 10, 11) — child signals parent via futex
//! - Clock reads for timing
//!
//! Build:
//!   cd kernel/examples && cargo build --release --bin threads
//!
//! Run with kernel:
//!   cd kernel && OS_INIT_ELF=examples/target/aarch64-unknown-none/release/threads \
//!     cargo build --release
//!   hypervisor target/aarch64-unknown-none/release/kernel

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use kernel_examples::{
    clock_get, exit, futex_wait, futex_wake, memory_alloc, print, print_u64, thread_create,
    unwrap_or_exit,
};

/// Shared state between parent and child. Placed in .bss (zero-initialized).
static FUTEX: AtomicU32 = AtomicU32::new(0);

/// Child thread entry point.
extern "C" fn child_entry() -> ! {
    print(b"  [child] Started!\n");

    // Simulate some work.
    let start = clock_get();
    let mut sum: u64 = 0;
    for i in 0..100_000u64 {
        sum = sum.wrapping_add(i);
    }
    let elapsed = clock_get() - start;

    print(b"  [child] Computed sum = ");
    print_u64(sum);
    print(b" in ");
    print_u64(elapsed / 1000);
    print(b" us\n");

    // Signal the parent via futex: store 1, then wake.
    FUTEX.store(1, Ordering::Release);
    let _ = futex_wake(&FUTEX, 1);

    print(b"  [child] Signaled parent. Exiting.\n");
    exit()
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    print(b"=== Threading Example ===\n\n");

    // Allocate a stack for the child thread (4 pages = 64 KiB).
    let stack_pages = 4u64;
    let stack_base = unwrap_or_exit(memory_alloc(stack_pages), b"memory_alloc");
    let stack_top = unsafe { stack_base.add((stack_pages * 16384) as usize) } as u64;

    // Stack must be 16-byte aligned (AArch64 requirement).
    assert!(stack_top & 0xF == 0, "stack not aligned");

    print(b"[parent] Allocated child stack: ");
    kernel_examples::print_hex(stack_base as u64);
    print(b" .. ");
    kernel_examples::print_hex(stack_top);
    print(b"\n");

    // Spawn the child thread.
    let child = unwrap_or_exit(thread_create(child_entry, stack_top), b"thread_create");
    print(b"[parent] Spawned child thread (handle ");
    print_u64(child as u64);
    print(b")\n");

    // Wait for the child to signal via futex.
    print(b"[parent] Waiting for child...\n");
    while FUTEX.load(Ordering::Acquire) == 0 {
        // futex_wait blocks if the value is still 0.
        let _ = futex_wait(&FUTEX, 0);
    }

    print(b"[parent] Child signaled! Futex value = ");
    print_u64(FUTEX.load(Ordering::Relaxed) as u64);
    print(b"\n\nDone.\n");

    exit()
}

