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

// --- Duplicated constants from syscall.rs ---

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
    pub const MEMORY_ALLOC: u64 = 25;
    pub const MEMORY_FREE: u64 = 26;
    pub const PROCESS_SET_SYSCALL_FILTER: u64 = 27;
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
/// EXIT is always allowed. Syscall numbers >= 32 are always allowed
/// (they'll hit UnknownSyscall in the match arm, not the filter).
fn is_syscall_allowed(syscall_nr: u64, mask: u32) -> bool {
    if syscall_nr == nr::EXIT {
        return true;
    }
    if syscall_nr >= 32 {
        return true;
    }

    mask & (1u32 << syscall_nr as u32) != 0
}

// ==========================================================================
// Default mask tests
// ==========================================================================

#[test]
fn default_mask_allows_all() {
    let mask = u32::MAX;

    // Every syscall 0-27 should be allowed.
    for nr in 0..=27 {
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
    assert!(is_syscall_allowed(nr::EXIT, 0xFFFF_FFFE));
    assert!(is_syscall_allowed(nr::EXIT, 0));
    assert!(is_syscall_allowed(nr::EXIT, 42));
}

// ==========================================================================
// Mask=0 blocks all except EXIT
// ==========================================================================

#[test]
fn mask_zero_blocks_all_except_exit() {
    let mask = 0u32;

    // EXIT is allowed.
    assert!(is_syscall_allowed(nr::EXIT, mask));

    // Every other syscall (1-27) is blocked.
    for nr in 1..=27 {
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
    let mask = (1u32 << nr::WRITE) | (1u32 << nr::YIELD);

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
    // HARDENING.md recommends: EXIT, WRITE, YIELD, HANDLE_CLOSE,
    // CHANNEL_SIGNAL, WAIT, FUTEX_WAIT, FUTEX_WAKE, MEMORY_ALLOC, MEMORY_FREE.
    let editor_mask = (1u32 << nr::EXIT)
        | (1u32 << nr::WRITE)
        | (1u32 << nr::YIELD)
        | (1u32 << nr::HANDLE_CLOSE)
        | (1u32 << nr::CHANNEL_SIGNAL)
        | (1u32 << nr::WAIT)
        | (1u32 << nr::FUTEX_WAIT)
        | (1u32 << nr::FUTEX_WAKE)
        | (1u32 << nr::MEMORY_ALLOC)
        | (1u32 << nr::MEMORY_FREE);

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

    // Blocked syscalls (the dangerous 17).
    let blocked = [
        nr::CHANNEL_CREATE,
        nr::SCHEDULING_CONTEXT_CREATE,
        nr::SCHEDULING_CONTEXT_BORROW,
        nr::SCHEDULING_CONTEXT_RETURN,
        nr::SCHEDULING_CONTEXT_BIND,
        nr::TIMER_CREATE,
        nr::INTERRUPT_REGISTER,
        nr::INTERRUPT_ACK,
        nr::DEVICE_MAP,
        nr::DMA_ALLOC,
        nr::DMA_FREE,
        nr::THREAD_CREATE,
        nr::PROCESS_CREATE,
        nr::PROCESS_START,
        nr::HANDLE_SEND,
        nr::PROCESS_KILL,
        nr::MEMORY_SHARE,
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
        syscall_mask: u32,
    }

    fn set_filter(target: &mut FakeProcess, mask: u32) -> Result<u64, Error> {
        if target.started {
            return Err(Error::InvalidArgument);
        }

        target.syscall_mask = mask;

        Ok(0)
    }

    let mut p = FakeProcess {
        started: false,
        syscall_mask: u32::MAX,
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
    // Syscall numbers >= 32 should bypass the filter (returns true),
    // even with mask=0. They'll hit UnknownSyscall in the match arm.
    assert!(is_syscall_allowed(32, 0));
    assert!(is_syscall_allowed(64, 0));
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
// Syscall number 27 is defined
// ==========================================================================

#[test]
fn process_set_syscall_filter_is_nr_27() {
    assert_eq!(nr::PROCESS_SET_SYSCALL_FILTER, 27);
}

// ==========================================================================
// Mask bit arithmetic edge cases
// ==========================================================================

#[test]
fn mask_bit_31_allows_syscall_31() {
    let mask = 1u32 << 31;

    assert!(is_syscall_allowed(31, mask));
    // But not 30 (bit 30 is clear).
    assert!(!is_syscall_allowed(30, mask));
}

#[test]
fn mask_single_bit_per_syscall() {
    // Verify that each syscall maps to exactly one bit.
    for nr in 0u64..28 {
        let mask = 1u32 << nr as u32;

        // This syscall is allowed (plus EXIT which is always allowed).
        assert!(is_syscall_allowed(nr, mask));

        // Other non-EXIT syscalls are not allowed.
        for other in 1u64..28 {
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
fn default_process_mask_is_u32_max() {
    // Process::new() initializes syscall_mask to u32::MAX.
    // Verify that this means all 28 syscalls are allowed.
    let mask = u32::MAX;

    for nr in 0u64..28 {
        assert!(
            mask & (1u32 << nr as u32) != 0,
            "bit {} should be set in u32::MAX",
            nr
        );
    }
}
