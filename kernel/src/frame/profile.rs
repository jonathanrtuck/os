//! Compile-time-gateable cycle profiling for syscall path decomposition.
//!
//! When `feature = "profile"` is enabled, `stamp()` reads CNTVCT_EL0 and
//! stores it to a global trace buffer. When disabled, every call compiles
//! to nothing. Cost per stamp: 2 cycles (mrs) + 1 cycle (str) = ~3 cycles,
//! within the 4-cycle budget.
//!
//! The buffer is a single global — safe because the bench is single-threaded
//! on core 0. If SMP profiling is ever needed, index by core_id.
//!
//! Assembly stamps in `exception.S` write directly to `__profile_buf` via
//! `adrp + add + mrs + str`. The `.ifdef PROFILE` guard ensures zero cost
//! when disabled.

/// Named stamp slots. Always compiled (zero-cost constants) so callers
/// don't need `#[cfg]` on every stamp call site.
pub mod slot {
    // ── Assembly stamps (written from exception.S) ─────────────
    pub const ASM_BEFORE_HANDLER: usize = 0;
    pub const ASM_AFTER_HANDLER: usize = 1;
    pub const ASM_SVC_ENTRY: usize = 2;
    pub const ASM_SVC_BEFORE_BL: usize = 3;
    pub const ASM_SVC_AFTER_BL: usize = 4;

    // ── Bench boundary stamps ──────────────────────────────────
    pub const BENCH_BEFORE: usize = 5;
    pub const BENCH_AFTER: usize = 6;

    // ── SVC handler stages ─────────────────────────────────────
    pub const HANDLER_ENTRY: usize = 7;
    pub const HANDLER_PERCPU_DONE: usize = 8;
    pub const HANDLER_EXIT: usize = 9;

    // ── dispatch() stages ──────────────────────────────────────
    pub const DISPATCH_ENTER: usize = 10;
    pub const DISPATCH_MATCH: usize = 11;
    pub const DISPATCH_EXIT: usize = 12;

    // ── Per-syscall shared stages (overwritten per call) ───────
    pub const SYS_SPACE_ID: usize = 13;
    pub const SYS_HANDLE_LOOKUP: usize = 14;
    pub const SYS_WORK: usize = 15;
    pub const SYS_ALLOC: usize = 16;
    pub const SYS_HANDLE_INSTALL: usize = 17;

    // ── IPC-specific stages ────────────────────────────────────
    pub const IPC_EP_LOOKUP: usize = 18;
    pub const IPC_PEER_CHECK: usize = 19;
    pub const IPC_MSG_READ: usize = 20;
    pub const IPC_HANDLE_STAGE: usize = 21;
    pub const IPC_RECV_POP: usize = 22;
    pub const IPC_SPACE_SWITCH: usize = 23;
    pub const IPC_MSG_WRITE: usize = 24;
    pub const IPC_HANDLE_INSTALL: usize = 25;
    pub const IPC_REPLY_CAP: usize = 26;
    pub const IPC_PRIORITY: usize = 27;
    pub const IPC_BEFORE_SWITCH: usize = 28;
    pub const IPC_AFTER_SWITCH: usize = 29;

    pub const MAX: usize = 32;
}

#[cfg(feature = "profile")]
// SAFETY: no_mangle is required so exception.S can reference this symbol
// via adrp+add. Only accessed from stamp() (single-threaded bench context)
// and assembly (same core, interrupts masked in SVC path).
#[unsafe(no_mangle)]
static mut __profile_buf: [u64; slot::MAX] = [0; slot::MAX];

#[inline(always)]
pub fn stamp(#[allow(unused_variables)] s: usize) {
    #[cfg(feature = "profile")]
    {
        // SAFETY: CNTVCT_EL0 is a read-only counter register. The buffer
        // is accessed only during bench runs (single-threaded, core 0).
        unsafe {
            __profile_buf[s] = super::arch::read_cycle_counter();
        }
    }
}

pub fn read() -> [u64; slot::MAX] {
    #[cfg(feature = "profile")]
    {
        // SAFETY: reading the buffer in the same single-threaded context
        // that writes it. No concurrent access.
        unsafe { __profile_buf }
    }
    #[cfg(not(feature = "profile"))]
    {
        [0; slot::MAX]
    }
}

pub fn reset() {
    #[cfg(feature = "profile")]
    {
        // SAFETY: single-threaded bench context.
        unsafe {
            __profile_buf = [0; slot::MAX];
        }
    }
}
