// Userspace syscall wrappers.
//
// Provides safe Rust functions for each kernel syscall. Compiled as an `rlib`
// and linked into user binaries by the kernel's build.rs.
//
// # Syscall ABI (aarch64)
//
// | Register | Role            |
// |----------|-----------------|
// | x8       | Syscall number  |
// | x0..x5   | Arguments       |
// | x0       | Return value    |
//
// Invoke via `svc #0`. All other registers are preserved across the call.
// Negative return values indicate errors — decoded into `SyscallError`.

#![no_std]

mod asm;
mod counter;
mod error;
mod heap;
mod io;
mod syscalls;
mod types;

pub use counter::{counter, counter_freq, counter_to_ns};
pub use error::SyscallError;
pub use heap::heap_stats;
pub use io::{format_u32, print, print_u32, write};
pub use syscalls::{
    channel_create, channel_signal, device_map, dma_alloc, dma_free, exit, futex_wait, futex_wake,
    handle_close, handle_get_badge, handle_send, handle_set_badge, interrupt_ack,
    interrupt_register, memory_alloc, memory_free,
    memory_share, process_create, process_kill, process_set_syscall_filter, process_start,
    scheduling_context_bind, scheduling_context_borrow, scheduling_context_create,
    scheduling_context_return, thread_create, timer_create, wait, yield_now,
};
pub use types::{
    ChannelHandle, HeapStats, InterruptHandle, ProcessHandle, SchedHandle, SyscallResult,
    ThreadHandle, TimerHandle,
};

mod system_config {
    #![allow(dead_code)]
    include!(env!("SYSTEM_CONFIG"));
}

const PAGE_SIZE: usize = system_config::PAGE_SIZE as usize;

#[global_allocator]
static HEAP: heap::UserHeap = heap::UserHeap::new();

// ---------------------------------------------------------------------------
// Panic handler — prints location and exits.
// ---------------------------------------------------------------------------
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    print(b"PANIC: ");
    if let Some(loc) = info.location() {
        print(loc.file().as_bytes());
        print(b":");
        print_u32(loc.line());
    } else {
        print(b"(no location)");
    }
    print(b"\n");
    exit()
}
