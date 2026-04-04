//! Channel IPC — create a channel, write to shared memory, signal, wait.
//!
//! Demonstrates:
//! - Channel creation (syscall 7) — returns two endpoint handles
//! - Shared memory layout (each channel gets two 16 KiB pages at CHANNEL_SHM_BASE)
//! - Signaling (syscall 8) — wake the far endpoint
//! - Waiting (syscall 9) — block until a handle is ready
//! - Handle close (syscall 3)
//!
//! Since we're init (the only process), we demonstrate self-IPC:
//! write on endpoint A, signal A (wakes B), wait on B.
//!
//! Build:
//!   cd kernel/examples && cargo build --release --bin channels
//!
//! Run with kernel:
//!   cd kernel && OS_INIT_ELF=examples/target/aarch64-unknown-none/release/channels \
//!     cargo build --release
//!   hypervisor target/aarch64-unknown-none/release/kernel

#![no_std]
#![no_main]

use kernel_examples::{
    channel_create, channel_signal, exit, handle_close, print, unwrap_or_exit, wait,
};

/// Base VA for channel shared memory (from system_config).
const CHANNEL_SHM_BASE: u64 = 0x0000_0000_4000_0000;
/// Each channel endpoint gets one 16 KiB page. Channel N's pages start at
/// CHANNEL_SHM_BASE + N * 2 * PAGE_SIZE. Endpoint A writes to page 0,
/// endpoint B writes to page 1.
const PAGE_SIZE: u64 = 16384;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    print(b"=== Channel IPC Example ===\n\n");

    // Create a channel — returns two handles (A and B).
    let (handle_a, handle_b) = unwrap_or_exit(channel_create(), b"channel_create");

    print(b"Created channel: endpoint A=");

    kernel_examples::print_u64(handle_a as u64);

    print(b", endpoint B=");

    kernel_examples::print_u64(handle_b as u64);

    print(b"\n");

    // Write a message into A's outgoing shared memory page.
    // Channel 0, endpoint A writes to page 0.
    let shm_a_out = CHANNEL_SHM_BASE + (handle_a as u64) * 2 * PAGE_SIZE;
    let msg = b"Hello from endpoint A!";

    // SAFETY: shm_a_out is the kernel-mapped shared memory page for this channel.
    // Writing within the first PAGE_SIZE bytes is valid.
    unsafe {
        core::ptr::copy_nonoverlapping(msg.as_ptr(), shm_a_out as *mut u8, msg.len());
    }

    print(b"Wrote to A's SHM: \"Hello from endpoint A!\"\n");

    // Signal endpoint A — this makes endpoint B ready.
    unwrap_or_exit(channel_signal(handle_a), b"channel_signal");

    print(b"Signaled A (B should now be ready)\n");

    // Wait on endpoint B.
    let handles = [handle_b];

    let idx = unwrap_or_exit(wait(&handles, 0), b"wait");

    print(b"Wait returned: index ");

    kernel_examples::print_u64(idx);

    print(b" (endpoint B is ready)\n");

    // Read from B's incoming page (same as A's outgoing page).
    let shm_b_in = shm_a_out;
    let received = unsafe { core::slice::from_raw_parts(shm_b_in as *const u8, msg.len()) };

    print(b"Read from B's SHM: \"");
    print(received);
    print(b"\"\n");

    // Clean up.
    unwrap_or_exit(handle_close(handle_a), b"close A");
    unwrap_or_exit(handle_close(handle_b), b"close B");

    print(b"\nHandles closed. Done.\n");

    exit()
}
