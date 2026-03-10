//! Userspace syscall wrappers.
//!
//! Provides safe Rust functions for each kernel syscall. Compiled as an `rlib`
//! and linked into user binaries by the kernel's build.rs.
//!
//! # Syscall ABI (aarch64)
//!
//! | Register | Role            |
//! |----------|-----------------|
//! | x8       | Syscall number  |
//! | x0..x5   | Arguments       |
//! | x0       | Return value    |
//!
//! Invoke via `svc #0`. All other registers are preserved across the call.
//! Negative return values indicate errors (see kernel `syscall::Error` and
//! `handle::HandleError` for the error codes).

#![no_std]

mod nr {
    pub const EXIT: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
    pub const HANDLE_CLOSE: u64 = 3;
    pub const CHANNEL_SIGNAL: u64 = 4;
    pub const CHANNEL_CREATE: u64 = 5;
    pub const SCHEDULING_CONTEXT_CREATE: u64 = 6;
    pub const SCHEDULING_CONTEXT_BORROW: u64 = 7;
    pub const SCHEDULING_CONTEXT_RETURN: u64 = 8;
    pub const SCHEDULING_CONTEXT_BIND: u64 = 9;
    pub const FUTEX_WAIT: u64 = 10;
    pub const FUTEX_WAKE: u64 = 11;
    pub const WAIT: u64 = 12;
    pub const TIMER_CREATE: u64 = 13;
    pub const INTERRUPT_REGISTER: u64 = 14;
    pub const INTERRUPT_ACK: u64 = 15;
    pub const DEVICE_MAP: u64 = 16;
    pub const DMA_ALLOC: u64 = 17;
    pub const DMA_FREE: u64 = 18;
    pub const THREAD_CREATE: u64 = 19;
    pub const PROCESS_CREATE: u64 = 20;
    pub const PROCESS_START: u64 = 21;
    pub const HANDLE_SEND: u64 = 22;
    pub const PROCESS_KILL: u64 = 23;
    pub const MEMORY_SHARE: u64 = 24;
}

// ---------------------------------------------------------------------------
// Raw syscall primitives
// ---------------------------------------------------------------------------

#[inline(always)]
unsafe fn syscall0(nr: u64) -> u64 {
    let ret: u64;

    core::arch::asm!(
        "svc #0",
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );

    ret
}
#[inline(always)]
unsafe fn syscall1(nr: u64, a0: u64) -> u64 {
    let ret: u64;

    core::arch::asm!(
        "svc #0",
        in("x0") a0,
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );

    ret
}
#[inline(always)]
unsafe fn syscall2(nr: u64, a0: u64, a1: u64) -> u64 {
    let ret: u64;

    core::arch::asm!(
        "svc #0",
        in("x0") a0,
        in("x1") a1,
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );

    ret
}
#[inline(always)]
unsafe fn syscall3(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;

    core::arch::asm!(
        "svc #0",
        in("x0") a0,
        in("x1") a1,
        in("x2") a2,
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );

    ret
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a channel with two endpoints.
///
/// Returns a packed value `handle_a | (handle_b << 8)` on success, where
/// both handles refer to endpoints of the same shared-memory channel. The
/// shared page is automatically mapped into the calling process. Returns a
/// negative error code on failure.
pub fn channel_create() -> i64 {
    unsafe { syscall0(nr::CHANNEL_CREATE) as i64 }
}
/// Signal the peer on a channel (write direction).
///
/// Returns 0 on success, or a negative error code.
pub fn channel_signal(handle: u8) -> i64 {
    unsafe { syscall1(nr::CHANNEL_SIGNAL, handle as u64) as i64 }
}
/// Map a device's MMIO region into this process's address space.
///
/// The kernel maps `size` bytes starting at physical address `pa` with device
/// memory attributes (non-cacheable). Returns the user VA on success, or a
/// negative error code. The PA must be in device MMIO space (not RAM).
pub fn device_map(pa: u64, size: u64) -> i64 {
    unsafe { syscall2(nr::DEVICE_MAP, pa, size) as i64 }
}
/// Allocate a DMA-capable buffer (2^order contiguous physical pages).
///
/// Returns the user VA of the mapped buffer on success, or a negative error
/// code. The physical address is written to `pa_out` for programming device
/// DMA registers. Order 0–4 (4 KiB – 64 KiB).
pub fn dma_alloc(order: u32, pa_out: &mut u64) -> i64 {
    unsafe { syscall2(nr::DMA_ALLOC, order as u64, pa_out as *mut u64 as u64) as i64 }
}
/// Free a DMA buffer previously allocated with `dma_alloc`.
///
/// `va` must be the value returned by `dma_alloc`. `order` must match the
/// original allocation. Returns 0 on success, or a negative error code.
pub fn dma_free(va: u64, order: u32) -> i64 {
    unsafe { syscall2(nr::DMA_FREE, va, order as u64) as i64 }
}
/// Terminate the calling process. Does not return.
pub fn exit() -> ! {
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") nr::EXIT,
            options(noreturn, nostack),
        );
    }
}
/// Wait on a futex. Blocks if the 32-bit value at `addr` equals `expected`.
///
/// Returns 0 on success (was woken), or a negative error code:
/// - `-2` (BadAddress): invalid or unaligned address.
/// - `-8` (WouldBlock): value at `addr` != `expected` (no block occurred).
pub fn futex_wait(addr: *const u32, expected: u32) -> i64 {
    unsafe { syscall2(nr::FUTEX_WAIT, addr as u64, expected as u64) as i64 }
}
/// Wake up to `count` threads waiting on a futex at `addr`.
///
/// Returns the number of threads woken on success, or a negative error code.
pub fn futex_wake(addr: *const u32, count: u32) -> i64 {
    unsafe { syscall2(nr::FUTEX_WAKE, addr as u64, count as u64) as i64 }
}
/// Close a handle, releasing the associated kernel resource.
///
/// Returns 0 on success, or a negative error code.
pub fn handle_close(handle: u8) -> i64 {
    unsafe { syscall1(nr::HANDLE_CLOSE, handle as u64) as i64 }
}
/// Send a handle to a suspended child process.
///
/// Copies the handle at `source_handle` (in the caller's table) into the
/// target process identified by `target_handle` (which must be a Process
/// handle). The target must not have been started yet. For channel handles,
/// the shared page is also mapped into the target's address space. Returns
/// 0 on success, or a negative error code.
pub fn handle_send(target_handle: u8, source_handle: u8) -> i64 {
    unsafe { syscall2(nr::HANDLE_SEND, target_handle as u64, source_handle as u64) as i64 }
}
/// Acknowledge an interrupt, allowing the device to fire again.
///
/// Clears the pending flag and re-enables the IRQ in the GIC. Must be called
/// after processing each interrupt. Returns 0 on success.
pub fn interrupt_ack(handle: u8) -> i64 {
    unsafe { syscall1(nr::INTERRUPT_ACK, handle as u64) as i64 }
}
/// Register for a hardware interrupt. Returns a waitable handle.
///
/// The handle becomes ready when the IRQ fires. Use `wait` to block until
/// the interrupt occurs, then call `interrupt_ack` after processing.
/// Returns the handle index on success, or a negative error code.
pub fn interrupt_register(irq: u32) -> i64 {
    unsafe { syscall1(nr::INTERRUPT_REGISTER, irq as u64) as i64 }
}
/// Map physical pages into a target process's shared memory region.
///
/// Maps `page_count` contiguous physical pages starting at `pa` into the
/// target process (identified by `target_handle`, a Process handle). The
/// target must not have been started yet. Returns the VA in the target's
/// address space on success, or a negative error code.
pub fn memory_share(target_handle: u8, pa: u64, page_count: u64) -> i64 {
    unsafe { syscall3(nr::MEMORY_SHARE, target_handle as u64, pa, page_count) as i64 }
}
/// Create a process from an ELF binary in memory. Returns a waitable handle.
///
/// The child process starts suspended — call `process_start` with the returned
/// handle to make its thread runnable. The handle becomes ready when the child's
/// last thread exits. Returns the handle index on success, or a negative error.
pub fn process_create(elf_ptr: *const u8, elf_len: usize) -> i64 {
    unsafe { syscall2(nr::PROCESS_CREATE, elf_ptr as u64, elf_len as u64) as i64 }
}
/// Kill a process, terminating all its threads.
///
/// The handle must be a Process handle with write rights. All threads in the
/// target process are terminated and full cleanup runs. The Process handle
/// becomes ready (waitable notification). Returns 0 on success.
pub fn process_kill(handle: u8) -> i64 {
    unsafe { syscall1(nr::PROCESS_KILL, handle as u64) as i64 }
}
/// Start a suspended child process.
///
/// Makes all suspended threads in the process identified by `handle` runnable.
/// Returns 0 on success, or a negative error code.
pub fn process_start(handle: u8) -> i64 {
    unsafe { syscall1(nr::PROCESS_START, handle as u64) as i64 }
}
/// Bind a scheduling context to the calling thread.
///
/// The thread must not already have a context bound. Returns 0 on success,
/// or a negative error code.
pub fn scheduling_context_bind(handle: u8) -> i64 {
    unsafe { syscall1(nr::SCHEDULING_CONTEXT_BIND, handle as u64) as i64 }
}
/// Borrow another scheduling context (context donation).
///
/// Saves the current context and switches to the one identified by `handle`.
/// Returns 0 on success, or a negative error code.
pub fn scheduling_context_borrow(handle: u8) -> i64 {
    unsafe { syscall1(nr::SCHEDULING_CONTEXT_BORROW, handle as u64) as i64 }
}
/// Create a scheduling context with the given budget and period (both in ns).
///
/// Returns the handle index on success, or a negative error code.
pub fn scheduling_context_create(budget: u64, period: u64) -> i64 {
    unsafe { syscall2(nr::SCHEDULING_CONTEXT_CREATE, budget, period) as i64 }
}
/// Return a borrowed scheduling context, restoring the saved one.
///
/// Returns 0 on success, or a negative error code.
pub fn scheduling_context_return() -> i64 {
    unsafe { syscall0(nr::SCHEDULING_CONTEXT_RETURN) as i64 }
}
/// Create a new thread in the calling process.
///
/// The thread starts at `entry_va` with user stack pointer `stack_top`.
/// Returns a waitable handle (becomes ready on thread exit), or a negative
/// error code.
pub fn thread_create(entry_va: u64, stack_top: u64) -> i64 {
    unsafe { syscall2(nr::THREAD_CREATE, entry_va, stack_top) as i64 }
}
/// Create a one-shot timer that fires after `timeout_ns` nanoseconds.
///
/// Returns the handle index on success, or a negative error code.
/// Wait on the returned handle via `wait` to block until the deadline.
pub fn timer_create(timeout_ns: u64) -> i64 {
    unsafe { syscall1(nr::TIMER_CREATE, timeout_ns) as i64 }
}
/// Wait for an event on one or more handles.
///
/// Blocks until any handle in `handles` has a pending event or the timeout
/// expires. Returns the index of the first ready handle (0-based) on success,
/// or a negative error code. Timeout of `u64::MAX` waits forever; `0` polls
/// without blocking.
pub fn wait(handles: &[u8], timeout_ns: u64) -> i64 {
    unsafe {
        syscall3(
            nr::WAIT,
            handles.as_ptr() as u64,
            handles.len() as u64,
            timeout_ns,
        ) as i64
    }
}
/// Write `buf` to the kernel console (UART).
///
/// Returns the number of bytes written on success, or a negative error code.
pub fn write(buf: &[u8]) -> i64 {
    unsafe { syscall2(nr::WRITE, buf.as_ptr() as u64, buf.len() as u64) as i64 }
}
/// Yield the current timeslice to the scheduler.
pub fn yield_now() {
    unsafe {
        syscall0(nr::YIELD);
    }
}

// ---------------------------------------------------------------------------
// Panic handler — exits the process instead of spinning.
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit()
}
