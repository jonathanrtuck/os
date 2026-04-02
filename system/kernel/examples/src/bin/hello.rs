//! Hello World — the simplest possible userspace program.
//!
//! Demonstrates:
//! - Entry point (`_start`, not `main`)
//! - Writing to the kernel serial console (syscall 1)
//! - Clean thread exit (syscall 0)
//! - Reading the monotonic clock (syscall 13)
//!
//! Build:
//!   cd kernel/examples && cargo build --release --bin hello
//!
//! Run with kernel:
//!   cd kernel && OS_INIT_ELF=examples/target/aarch64-unknown-none/release/hello \
//!     cargo build --release
//!   hypervisor target/aarch64-unknown-none/release/kernel

#![no_std]
#![no_main]

use kernel_examples::{clock_get, exit, print, print_hex, print_u64};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    print(b"Hello from userspace!\n");
    print(b"\n");

    // Read the monotonic clock to prove syscalls work bidirectionally.
    let ns = clock_get();
    print(b"Boot time: ");
    print_u64(ns / 1_000_000);
    print(b" ms (");
    print_hex(ns);
    print(b" ns)\n");

    print(b"\nGoodbye.\n");
    exit()
}
