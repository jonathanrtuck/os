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
    pub const CHANNEL_WAIT: u64 = 5;
    pub const SCHEDULING_CONTEXT_CREATE: u64 = 6;
    pub const SCHEDULING_CONTEXT_BORROW: u64 = 7;
    pub const SCHEDULING_CONTEXT_RETURN: u64 = 8;
    pub const SCHEDULING_CONTEXT_BIND: u64 = 9;
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

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Signal the peer on a channel (write direction).
///
/// Returns 0 on success, or a negative error code.
pub fn channel_signal(handle: u8) -> i64 {
    unsafe { syscall1(nr::CHANNEL_SIGNAL, handle as u64) as i64 }
}
/// Wait for a signal on a channel (read direction).
///
/// Blocks the calling thread until the peer signals. Returns 0 on success,
/// or a negative error code.
pub fn channel_wait(handle: u8) -> i64 {
    unsafe { syscall1(nr::CHANNEL_WAIT, handle as u64) as i64 }
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
/// Close a handle, releasing the associated kernel resource.
///
/// Returns 0 on success, or a negative error code.
pub fn handle_close(handle: u8) -> i64 {
    unsafe { syscall1(nr::HANDLE_CLOSE, handle as u64) as i64 }
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
