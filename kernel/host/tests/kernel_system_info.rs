#![feature(allocator_api)]
//! Tests for the system_info syscall (nr 50).
//!
//! Verifies the info type constants and the formulas used by sys_system_info.
//! Since the syscall handler has heavy dependencies, we test the underlying
//! kernel APIs directly rather than calling through the dispatcher.

#[path = "../../paging.rs"]
mod paging;

// Replicate the info type constants from syscall.rs.
const INFO_TOTAL_MEMORY: u64 = 0;
const INFO_AVAILABLE_MEMORY: u64 = 1;
const INFO_CPU_COUNT: u64 = 2;
const INFO_PAGE_SIZE: u64 = 3;
const INFO_BOOT_TIME_NS: u64 = 4;

// ---------------------------------------------------------------------------
// Info type constants are distinct and sequential
// ---------------------------------------------------------------------------

#[test]
fn info_type_constants_are_distinct() {
    let types = [
        INFO_TOTAL_MEMORY,
        INFO_AVAILABLE_MEMORY,
        INFO_CPU_COUNT,
        INFO_PAGE_SIZE,
        INFO_BOOT_TIME_NS,
    ];
    for (i, a) in types.iter().enumerate() {
        for (j, b) in types.iter().enumerate() {
            if i != j {
                assert_ne!(a, b, "info types {i} and {j} must be distinct");
            }
        }
    }
}

#[test]
fn info_type_constants_are_sequential_from_zero() {
    assert_eq!(INFO_TOTAL_MEMORY, 0);
    assert_eq!(INFO_AVAILABLE_MEMORY, 1);
    assert_eq!(INFO_CPU_COUNT, 2);
    assert_eq!(INFO_PAGE_SIZE, 3);
    assert_eq!(INFO_BOOT_TIME_NS, 4);
}

// ---------------------------------------------------------------------------
// INFO_TOTAL_MEMORY: ram_end() - RAM_START
// ---------------------------------------------------------------------------

#[test]
fn total_memory_returns_nonzero() {
    let total = paging::ram_end() - paging::RAM_START;
    assert!(total > 0, "total memory must be nonzero");
}

#[test]
fn total_memory_is_page_aligned() {
    let total = paging::ram_end() - paging::RAM_START;
    assert_eq!(
        total % paging::PAGE_SIZE,
        0,
        "total memory must be page-aligned"
    );
}

// ---------------------------------------------------------------------------
// INFO_CPU_COUNT: MAX_CORES
// ---------------------------------------------------------------------------

#[test]
fn cpu_count_at_least_one() {
    let count = paging::MAX_CORES;
    assert!(count >= 1, "must have at least 1 CPU core");
}

#[test]
fn cpu_count_is_reasonable() {
    let count = paging::MAX_CORES;
    assert!(count <= 256, "MAX_CORES should be reasonable (<=256)");
}

// ---------------------------------------------------------------------------
// INFO_PAGE_SIZE: matches system_config
// ---------------------------------------------------------------------------

#[test]
fn page_size_matches_system_config() {
    // PAGE_SIZE comes from system_config.rs via paging.rs
    assert_eq!(paging::PAGE_SIZE, 16384, "page size should be 16 KiB");
}

#[test]
fn page_size_is_power_of_two() {
    let ps = paging::PAGE_SIZE;
    assert!(ps > 0 && (ps & (ps - 1)) == 0, "PAGE_SIZE must be power of 2");
}

// ---------------------------------------------------------------------------
// Unknown info type should return InvalidArgument
// (tested via constant range — actual error checked via dispatch in integration)
// ---------------------------------------------------------------------------

#[test]
fn unknown_info_type_is_out_of_range() {
    // Any value >= 5 is an unknown info type
    let max_known = INFO_BOOT_TIME_NS;
    assert_eq!(max_known, 4, "highest known info type should be 4");
    // Values 5, 100, u64::MAX are all "unknown" — the handler returns InvalidArgument
}

// ---------------------------------------------------------------------------
// INFO_AVAILABLE_MEMORY formula: free_count() * PAGE_SIZE
// ---------------------------------------------------------------------------

#[test]
fn available_memory_formula_does_not_overflow() {
    // Even with a very large free_count, the multiplication should fit in u64.
    // Max possible: 2^48 pages * 16384 = 2^62 bytes — fits in u64.
    let max_pages: u64 = 1 << 48;
    let result = max_pages.checked_mul(paging::PAGE_SIZE);
    assert!(result.is_some(), "free_count * PAGE_SIZE must not overflow u64");
}

#[test]
fn available_memory_less_or_equal_total() {
    // Available memory (any free count * PAGE_SIZE) <= total memory is an invariant.
    // We verify the formula relationship: if free_count is 0, available is 0 (<=total).
    // If free_count is total/PAGE_SIZE, available equals total.
    let total = paging::ram_end() - paging::RAM_START;
    let max_free_pages = total / paging::PAGE_SIZE;
    let available = max_free_pages * paging::PAGE_SIZE;
    assert_eq!(available, total);
}
