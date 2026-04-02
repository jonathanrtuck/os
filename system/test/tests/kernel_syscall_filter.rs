//! Host-side tests for per-process syscall filtering.
//!
//! The actual filter runs inside the kernel's dispatch(), which we cannot link.
//! These tests duplicate the pure filter logic and verify correctness of:
//! - Default mask (all allowed)
//! - EXIT always allowed regardless of mask
//! - Mask=0 blocks everything except EXIT
//! - Specific bit patterns
//! - Editor mask scenario from HARDENING.md
//! - SyscallBlocked error code value
//! - Out-of-range syscall numbers bypass the filter

// --- Must match kernel/syscall.rs::nr exactly (dense 0–38) ---

mod nr {
    // Runtime basics (0–2)
    pub const EXIT: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
    // Capability layer (3–6)
    pub const HANDLE_CLOSE: u64 = 3;
    pub const HANDLE_SEND: u64 = 4;
    pub const HANDLE_SET_BADGE: u64 = 5;
    pub const HANDLE_GET_BADGE: u64 = 6;
    // IPC (7–8)
    pub const CHANNEL_CREATE: u64 = 7;
    pub const CHANNEL_SIGNAL: u64 = 8;
    // Event loop (9)
    pub const WAIT: u64 = 9;
    // Userspace sync (10–11)
    pub const FUTEX_WAIT: u64 = 10;
    pub const FUTEX_WAKE: u64 = 11;
    // Time (12)
    pub const TIMER_CREATE: u64 = 12;
    // Heap memory (13–14)
    pub const MEMORY_ALLOC: u64 = 13;
    pub const MEMORY_FREE: u64 = 14;
    // VMO (15–24)
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
    // Pager (25–26)
    pub const VMO_SET_PAGER: u64 = 25;
    pub const PAGER_SUPPLY: u64 = 26;
    // Process/thread lifecycle (27–31)
    pub const PROCESS_CREATE: u64 = 27;
    pub const PROCESS_START: u64 = 28;
    pub const PROCESS_KILL: u64 = 29;
    pub const PROCESS_SET_SYSCALL_FILTER: u64 = 30;
    pub const THREAD_CREATE: u64 = 31;
    // Scheduling (32–35)
    pub const SCHEDULING_CONTEXT_CREATE: u64 = 32;
    pub const SCHEDULING_CONTEXT_BORROW: u64 = 33;
    pub const SCHEDULING_CONTEXT_RETURN: u64 = 34;
    pub const SCHEDULING_CONTEXT_BIND: u64 = 35;
    // Device layer (36–38)
    pub const DEVICE_MAP: u64 = 36;
    pub const INTERRUPT_REGISTER: u64 = 37;
    pub const INTERRUPT_ACK: u64 = 38;
    /// Total syscall count (for iteration bounds).
    pub const COUNT: u64 = 39;
}

// --- Duplicated Error enum from syscall.rs ---

#[repr(i64)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Error {
    UnknownSyscall = -1,
    BadAddress = -2,
    BadLength = -3,
    InvalidArgument = -4,
    AlreadyBorrowing = -5,
    NotBorrowing = -6,
    AlreadyBound = -7,
    WouldBlock = -8,
    OutOfMemory = -9,
    SyscallBlocked = -15,
}

// --- Duplicated userspace SyscallError (subset for filter-relevant codes) ---

#[repr(i64)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum SyscallError {
    UnknownSyscall = -1,
    InvalidArgument = -4,
    SyscallBlocked = -15,
}

/// Duplicate of the filter check in dispatch().
///
/// Returns true if the syscall is allowed, false if blocked.
/// EXIT is always allowed. Syscall numbers >= 64 are always allowed
/// (they'll hit UnknownSyscall in the match arm, not the filter).
fn is_syscall_allowed(syscall_nr: u64, mask: u64) -> bool {
    if syscall_nr == nr::EXIT {
        return true;
    }
    if syscall_nr >= 64 {
        return true;
    }

    mask & (1u64 << syscall_nr) != 0
}

// ==========================================================================
// Default mask tests
// ==========================================================================

#[test]
fn default_mask_allows_all() {
    let mask = u64::MAX;

    // Every syscall 0-38 should be allowed.
    for nr in 0..nr::COUNT {
        assert!(
            is_syscall_allowed(nr, mask),
            "syscall {} should be allowed with default mask",
            nr
        );
    }
}

// ==========================================================================
// EXIT always allowed
// ==========================================================================

#[test]
fn exit_always_allowed_with_zero_mask() {
    assert!(is_syscall_allowed(nr::EXIT, 0));
}

#[test]
fn exit_always_allowed_with_arbitrary_mask() {
    // Mask that explicitly has bit 0 clear.
    assert!(is_syscall_allowed(nr::EXIT, 0xFFFF_FFFF_FFFF_FFFE));
    assert!(is_syscall_allowed(nr::EXIT, 0));
    assert!(is_syscall_allowed(nr::EXIT, 42));
}

// ==========================================================================
// Mask=0 blocks all except EXIT
// ==========================================================================

#[test]
fn mask_zero_blocks_all_except_exit() {
    let mask = 0u64;

    // EXIT is allowed.
    assert!(is_syscall_allowed(nr::EXIT, mask));

    // Every other syscall (1-38) is blocked.
    for nr in 1..nr::COUNT {
        assert!(
            !is_syscall_allowed(nr, mask),
            "syscall {} should be blocked with mask=0",
            nr
        );
    }
}

// ==========================================================================
// Specific mask allows only set bits
// ==========================================================================

#[test]
fn specific_mask_allows_only_set_bits() {
    // Allow only WRITE (1) and YIELD (2).
    let mask = (1u64 << nr::WRITE) | (1u64 << nr::YIELD);

    assert!(is_syscall_allowed(nr::EXIT, mask)); // EXIT always allowed
    assert!(is_syscall_allowed(nr::WRITE, mask));
    assert!(is_syscall_allowed(nr::YIELD, mask));
    assert!(!is_syscall_allowed(nr::HANDLE_CLOSE, mask));
    assert!(!is_syscall_allowed(nr::CHANNEL_SIGNAL, mask));
    assert!(!is_syscall_allowed(nr::PROCESS_CREATE, mask));
    assert!(!is_syscall_allowed(nr::DEVICE_MAP, mask));
}

// ==========================================================================
// Editor mask scenario from HARDENING.md
// ==========================================================================

#[test]
fn editor_mask_scenario() {
    // Minimal editor sandbox: EXIT, WRITE, YIELD, HANDLE_CLOSE,
    // CHANNEL_SIGNAL, WAIT, FUTEX_WAIT, FUTEX_WAKE, MEMORY_ALLOC, MEMORY_FREE.
    let editor_mask = (1u64 << nr::EXIT)
        | (1u64 << nr::WRITE)
        | (1u64 << nr::YIELD)
        | (1u64 << nr::HANDLE_CLOSE)
        | (1u64 << nr::CHANNEL_SIGNAL)
        | (1u64 << nr::WAIT)
        | (1u64 << nr::FUTEX_WAIT)
        | (1u64 << nr::FUTEX_WAKE)
        | (1u64 << nr::MEMORY_ALLOC)
        | (1u64 << nr::MEMORY_FREE);

    // Allowed syscalls.
    let allowed = [
        nr::EXIT,
        nr::WRITE,
        nr::YIELD,
        nr::HANDLE_CLOSE,
        nr::CHANNEL_SIGNAL,
        nr::WAIT,
        nr::FUTEX_WAIT,
        nr::FUTEX_WAKE,
        nr::MEMORY_ALLOC,
        nr::MEMORY_FREE,
    ];

    for &nr in &allowed {
        assert!(
            is_syscall_allowed(nr, editor_mask),
            "editor should be allowed syscall {}",
            nr
        );
    }

    // Everything else should be blocked.
    let blocked = [
        nr::HANDLE_SEND,
        nr::HANDLE_SET_BADGE,
        nr::HANDLE_GET_BADGE,
        nr::CHANNEL_CREATE,
        nr::TIMER_CREATE,
        nr::VMO_CREATE,
        nr::VMO_MAP,
        nr::VMO_UNMAP,
        nr::VMO_READ,
        nr::VMO_WRITE,
        nr::VMO_GET_INFO,
        nr::VMO_SNAPSHOT,
        nr::VMO_RESTORE,
        nr::VMO_SEAL,
        nr::VMO_OP_RANGE,
        nr::VMO_SET_PAGER,
        nr::PAGER_SUPPLY,
        nr::PROCESS_CREATE,
        nr::PROCESS_START,
        nr::PROCESS_KILL,
        nr::PROCESS_SET_SYSCALL_FILTER,
        nr::THREAD_CREATE,
        nr::SCHEDULING_CONTEXT_CREATE,
        nr::SCHEDULING_CONTEXT_BORROW,
        nr::SCHEDULING_CONTEXT_RETURN,
        nr::SCHEDULING_CONTEXT_BIND,
        nr::DEVICE_MAP,
        nr::INTERRUPT_REGISTER,
        nr::INTERRUPT_ACK,
    ];

    for &nr in &blocked {
        assert!(
            !is_syscall_allowed(nr, editor_mask),
            "editor should NOT be allowed syscall {}",
            nr
        );
    }
}

// ==========================================================================
// set_syscall_filter only before start
// ==========================================================================

#[test]
fn mask_set_only_before_start() {
    // Simulates the started check in sys_process_set_syscall_filter.
    struct FakeProcess {
        started: bool,
        syscall_mask: u64,
    }

    fn set_filter(target: &mut FakeProcess, mask: u64) -> Result<u64, Error> {
        if target.started {
            return Err(Error::InvalidArgument);
        }

        target.syscall_mask = mask;

        Ok(0)
    }

    let mut p = FakeProcess {
        started: false,
        syscall_mask: u64::MAX,
    };

    // Before start: succeeds.
    assert!(set_filter(&mut p, 0x1234).is_ok());
    assert_eq!(p.syscall_mask, 0x1234);

    // After start: fails.
    p.started = true;
    assert_eq!(set_filter(&mut p, 0), Err(Error::InvalidArgument));
    assert_eq!(p.syscall_mask, 0x1234); // Unchanged.
}

// ==========================================================================
// Syscall number out of range bypasses filter
// ==========================================================================

#[test]
fn syscall_nr_out_of_range_not_filtered() {
    // Syscall numbers >= 64 should bypass the filter (returns true),
    // even with mask=0. They'll hit UnknownSyscall in the match arm.
    assert!(is_syscall_allowed(64, 0));
    assert!(is_syscall_allowed(100, 0));
    assert!(is_syscall_allowed(u64::MAX, 0));
}

// ==========================================================================
// Error code value tests
// ==========================================================================

#[test]
fn syscall_blocked_error_code_value() {
    assert_eq!(Error::SyscallBlocked as i64, -15);
}

#[test]
fn syscall_blocked_u64_representation() {
    let blocked = Error::SyscallBlocked as i64 as u64;

    // Two's complement of -15.
    assert_eq!(blocked, 0xFFFF_FFFF_FFFF_FFF1);
    // Round-trip back to i64.
    assert_eq!(blocked as i64, -15);
}

#[test]
fn userspace_syscall_blocked_matches_kernel() {
    assert_eq!(
        SyscallError::SyscallBlocked as i64,
        Error::SyscallBlocked as i64
    );
    assert_eq!(SyscallError::SyscallBlocked as i64, -15);
}

// ==========================================================================
// Syscall number 30 is PROCESS_SET_SYSCALL_FILTER
// ==========================================================================

#[test]
fn process_set_syscall_filter_is_nr_30() {
    assert_eq!(nr::PROCESS_SET_SYSCALL_FILTER, 30);
}

// ==========================================================================
// Mask bit arithmetic edge cases
// ==========================================================================

#[test]
fn mask_bit_31_allows_syscall_31() {
    let mask = 1u64 << 31;

    assert!(is_syscall_allowed(31, mask));
    // But not 30 (bit 30 is clear).
    assert!(!is_syscall_allowed(30, mask));
}

#[test]
fn mask_single_bit_per_syscall() {
    // Verify that each syscall maps to exactly one bit.
    for nr in 0u64..nr::COUNT {
        let mask = 1u64 << nr;

        // This syscall is allowed (plus EXIT which is always allowed).
        assert!(is_syscall_allowed(nr, mask));

        // Other non-EXIT syscalls are not allowed.
        for other in 1u64..nr::COUNT {
            if other != nr {
                assert!(
                    !is_syscall_allowed(other, mask),
                    "syscall {} should be blocked when only bit {} is set",
                    other,
                    nr
                );
            }
        }
    }
}

#[test]
fn default_process_mask_is_u64_max() {
    // Process::new() initializes syscall_mask to u64::MAX.
    // Verify that this means all 39 syscalls are allowed.
    let mask = u64::MAX;

    for nr in 0u64..nr::COUNT {
        assert!(
            mask & (1u64 << nr) != 0,
            "bit {} should be set in u64::MAX",
            nr
        );
    }
}
