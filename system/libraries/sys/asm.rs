//! Low-level syscall assembly glue and helpers.

use crate::{SyscallError, SyscallResult};

/// Syscall numbers — must match kernel/syscall.rs::nr exactly.
pub mod nr {
    // --- Runtime basics (0–2) ---
    pub const EXIT: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
    // --- Capability layer (3–6) ---
    pub const HANDLE_CLOSE: u64 = 3;
    pub const HANDLE_SEND: u64 = 4;
    pub const HANDLE_SET_BADGE: u64 = 5;
    pub const HANDLE_GET_BADGE: u64 = 6;
    // --- IPC (7–8) ---
    pub const CHANNEL_CREATE: u64 = 7;
    pub const CHANNEL_SIGNAL: u64 = 8;
    // --- Event loop (9) ---
    pub const WAIT: u64 = 9;
    // --- Userspace sync (10–11) ---
    pub const FUTEX_WAIT: u64 = 10;
    pub const FUTEX_WAKE: u64 = 11;
    // --- Time (12) ---
    pub const TIMER_CREATE: u64 = 12;
    // --- Heap memory (13–14) ---
    pub const MEMORY_ALLOC: u64 = 13;
    pub const MEMORY_FREE: u64 = 14;
    // --- Virtual Memory Objects (15–24) ---
    pub const VMO_CREATE: u64 = 15;
    pub const VMO_MAP: u64 = 16;
    pub const VMO_UNMAP: u64 = 17;
    pub const VMO_READ: u64 = 18;
    pub const VMO_WRITE: u64 = 19;
    pub const VMO_GET_INFO: u64 = 20;
    pub const VMO_SNAPSHOT: u64 = 21;
    pub const VMO_RESTORE: u64 = 22;
    pub const VMO_SEAL: u64 = 23;
    pub const VMO_OP_RANGE: u64 = 24;
    pub const VMO_SET_PAGER: u64 = 25;
    pub const PAGER_SUPPLY: u64 = 26;
    // --- Events (27–29) ---
    pub const EVENT_CREATE: u64 = 27;
    pub const EVENT_SIGNAL: u64 = 28;
    pub const EVENT_RESET: u64 = 29;
    // --- Process/thread lifecycle (30–34) ---
    pub const PROCESS_CREATE: u64 = 30;
    pub const PROCESS_START: u64 = 31;
    pub const PROCESS_KILL: u64 = 32;
    pub const PROCESS_SET_SYSCALL_FILTER: u64 = 33;
    pub const THREAD_CREATE: u64 = 34;
    // --- Scheduling (35–38) ---
    pub const SCHEDULING_CONTEXT_CREATE: u64 = 35;
    pub const SCHEDULING_CONTEXT_BORROW: u64 = 36;
    pub const SCHEDULING_CONTEXT_RETURN: u64 = 37;
    pub const SCHEDULING_CONTEXT_BIND: u64 = 38;
    // --- Device layer (39–41) ---
    pub const DEVICE_MAP: u64 = 39;
    pub const INTERRUPT_REGISTER: u64 = 40;
    pub const INTERRUPT_ACK: u64 = 41;
}

pub fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

/// Convert a raw syscall return value to a `SyscallResult`.
///
/// Non-negative → `Ok(value as u64)`. Negative → `Err(SyscallError)`.
pub fn result(raw: i64) -> SyscallResult<u64> {
    if raw >= 0 {
        Ok(raw as u64)
    } else {
        Err(SyscallError::from_raw(raw))
    }
}

#[inline(always)]
pub unsafe fn syscall0(nr: u64) -> u64 {
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
pub unsafe fn syscall1(nr: u64, a0: u64) -> u64 {
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
pub unsafe fn syscall2(nr: u64, a0: u64, a1: u64) -> u64 {
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
pub unsafe fn syscall3(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
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

#[inline(always)]
pub unsafe fn syscall4(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;

    core::arch::asm!(
        "svc #0",
        in("x0") a0,
        in("x1") a1,
        in("x2") a2,
        in("x3") a3,
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );

    ret
}
