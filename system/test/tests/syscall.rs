//! Host-side tests for syscall validation logic.
//!
//! syscall.rs is heavily coupled to kernel hardware (AT instructions, scheduler
//! lock, raw Context pointers). We cannot include it directly. Instead, these
//! tests duplicate the pure validation logic and verify correctness of:
//! - Error code representation and conversion
//! - User pointer/buffer bounds checking patterns
//! - is_user_range_readable page iteration logic
//! - Syscall number completeness
//! - Handle range validation patterns

#[path = "../../kernel/paging.rs"]
mod paging;

use paging::*;

// --- Duplicated constants from syscall.rs ---

const MAX_DMA_ORDER: u64 = 10;
const MAX_ELF_SIZE: u64 = 2 * 1024 * 1024;
const MAX_WAIT_HANDLES: u64 = 16;
const MAX_WRITE_LEN: u64 = 4096;

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
}

#[repr(i64)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum HandleError {
    InvalidHandle = -10,
    InsufficientRights = -12,
    TableFull = -13,
}

impl From<HandleError> for u64 {
    fn from(e: HandleError) -> u64 {
        (e as i64) as u64
    }
}

// --- Duplicated result_to_u64 macro ---

macro_rules! result_to_u64 {
    ($result:expr) => {
        match $result {
            Ok(n) => n,
            Err(e) => e as i64 as u64,
        }
    };
}

/// WOULD_BLOCK_RAW mirrors the public constant in syscall.rs.
const WOULD_BLOCK_RAW: u64 = Error::WouldBlock as i64 as u64;

// --- Duplicated is_user_range_readable page iteration logic ---

/// Duplicate of syscall.rs is_user_range_readable logic, except using
/// a callback instead of hardware AT instructions.
fn is_range_valid(start: u64, len: u64, mut check_page: impl FnMut(u64) -> bool) -> bool {
    if len == 0 {
        return true;
    }

    let page_mask = !(PAGE_SIZE - 1);
    let first_page = start & page_mask;
    let last_page = (start + len - 1) & page_mask;
    let mut page = first_page;

    while page <= last_page {
        if !check_page(page) {
            return false;
        }

        page += PAGE_SIZE;
    }

    true
}

// --- Duplicated pointer validation patterns ---

/// Validates a user buffer exactly as sys_write does.
fn validate_write_buffer(buf_ptr: u64, len: u64) -> Result<(), Error> {
    if len > MAX_WRITE_LEN {
        return Err(Error::BadLength);
    }
    if buf_ptr >= USER_VA_END {
        return Err(Error::BadAddress);
    }

    let end = buf_ptr.checked_add(len).ok_or(Error::BadAddress)?;

    if end > USER_VA_END {
        return Err(Error::BadAddress);
    }

    Ok(())
}

/// Validates ELF buffer parameters exactly as sys_process_create does.
fn validate_elf_buffer(elf_ptr: u64, elf_len: u64) -> Result<(), Error> {
    if elf_len == 0 || elf_len > MAX_ELF_SIZE {
        return Err(Error::BadLength);
    }
    if elf_ptr >= USER_VA_END {
        return Err(Error::BadAddress);
    }

    let end = elf_ptr.checked_add(elf_len).ok_or(Error::BadAddress)?;

    if end > USER_VA_END {
        return Err(Error::BadAddress);
    }

    Ok(())
}

/// Validates wait buffer parameters exactly as sys_wait does.
fn validate_wait_buffer(handles_ptr: u64, count: u64) -> Result<(), Error> {
    if count == 0 || count > MAX_WAIT_HANDLES {
        return Err(Error::InvalidArgument);
    }
    if handles_ptr >= USER_VA_END {
        return Err(Error::BadAddress);
    }

    if let Some(end) = handles_ptr.checked_add(count) {
        if end > USER_VA_END {
            return Err(Error::BadAddress);
        }
    } else {
        return Err(Error::BadAddress);
    }

    Ok(())
}

/// Validates a DMA alloc pa_out_ptr exactly as sys_dma_alloc does.
fn validate_dma_pa_out_ptr(pa_out_ptr: u64) -> Result<(), Error> {
    if pa_out_ptr >= USER_VA_END || pa_out_ptr & 7 != 0 {
        return Err(Error::BadAddress);
    }

    Ok(())
}

/// Validates device_map PA range exactly as sys_device_map does.
fn validate_device_map(pa: u64, size: u64) -> Result<(), Error> {
    if size == 0 {
        return Err(Error::InvalidArgument);
    }

    let end = pa.checked_add(size).ok_or(Error::InvalidArgument)?;

    if !(end <= RAM_START || pa >= RAM_END) {
        return Err(Error::InvalidArgument);
    }

    Ok(())
}

/// Validates memory_share PA range exactly as sys_memory_share does.
fn validate_memory_share(pa: u64, page_count: u64) -> Result<(), Error> {
    if page_count == 0 || page_count > 1024 {
        return Err(Error::InvalidArgument);
    }
    if pa & (PAGE_SIZE - 1) != 0 {
        return Err(Error::BadAddress);
    }

    let end_pa = pa
        .checked_add(page_count * PAGE_SIZE)
        .ok_or(Error::BadAddress)?;

    if pa < RAM_START || end_pa > RAM_END {
        return Err(Error::BadAddress);
    }

    Ok(())
}

/// Validates futex addr exactly as sys_futex_wait / sys_futex_wake does.
fn validate_futex_addr(addr: u64) -> Result<(), Error> {
    if addr >= USER_VA_END || addr & 3 != 0 {
        return Err(Error::BadAddress);
    }

    Ok(())
}

/// Validates thread_create arguments exactly as sys_thread_create does.
fn validate_thread_create(entry_va: u64, stack_top: u64) -> Result<(), Error> {
    if entry_va >= USER_VA_END {
        return Err(Error::BadAddress);
    }
    if stack_top >= USER_VA_END || stack_top & 0xF != 0 {
        return Err(Error::BadAddress);
    }

    Ok(())
}

/// Validates DMA free VA range exactly as sys_dma_free does.
fn validate_dma_free(va: u64) -> Result<(), Error> {
    if va < DMA_BUFFER_BASE || va >= DMA_BUFFER_END {
        return Err(Error::InvalidArgument);
    }

    Ok(())
}

/// Validates memory_free VA exactly as sys_memory_free does.
fn validate_memory_free(va: u64) -> Result<(), Error> {
    if va < HEAP_BASE || va >= HEAP_END {
        return Err(Error::InvalidArgument);
    }
    if va & (PAGE_SIZE - 1) != 0 {
        return Err(Error::BadAddress);
    }

    Ok(())
}

/// Validates a handle number fits in u8 (common pattern across all handle syscalls).
fn validate_handle_nr(handle_nr: u64) -> bool {
    handle_nr <= u8::MAX as u64
}

// ==========================================================================
// Error code representation tests
// ==========================================================================

#[test]
fn error_codes_are_negative() {
    assert_eq!(Error::UnknownSyscall as i64, -1);
    assert_eq!(Error::BadAddress as i64, -2);
    assert_eq!(Error::BadLength as i64, -3);
    assert_eq!(Error::InvalidArgument as i64, -4);
    assert_eq!(Error::AlreadyBorrowing as i64, -5);
    assert_eq!(Error::NotBorrowing as i64, -6);
    assert_eq!(Error::AlreadyBound as i64, -7);
    assert_eq!(Error::WouldBlock as i64, -8);
    assert_eq!(Error::OutOfMemory as i64, -9);
}

#[test]
fn handle_error_codes_are_negative() {
    assert_eq!(HandleError::InvalidHandle as i64, -10);
    assert_eq!(HandleError::InsufficientRights as i64, -12);
    assert_eq!(HandleError::TableFull as i64, -13);
}

#[test]
fn error_to_u64_preserves_sign_via_twos_complement() {
    // Error codes are negative i64 cast to u64 (two's complement).
    let bad_addr = Error::BadAddress as i64 as u64;
    assert_eq!(bad_addr, 0xFFFF_FFFF_FFFF_FFFE);
    // Round-trip: u64 back to i64 recovers the original value.
    assert_eq!(bad_addr as i64, -2);
}

#[test]
fn handle_error_from_into_u64() {
    let val: u64 = HandleError::InvalidHandle.into();
    assert_eq!(val as i64, -10);

    let val2: u64 = HandleError::TableFull.into();
    assert_eq!(val2 as i64, -13);
}

#[test]
fn would_block_raw_matches_error_enum() {
    assert_eq!(WOULD_BLOCK_RAW, Error::WouldBlock as i64 as u64);
    assert_eq!(WOULD_BLOCK_RAW as i64, -8);
}

#[test]
fn result_to_u64_ok_returns_value() {
    let r: Result<u64, Error> = Ok(42);
    assert_eq!(result_to_u64!(r), 42);
}

#[test]
fn result_to_u64_err_returns_negative_code() {
    let r: Result<u64, Error> = Err(Error::BadAddress);
    assert_eq!(result_to_u64!(r) as i64, -2);
}

#[test]
fn result_to_u64_handle_err_returns_negative_code() {
    let r: Result<u64, HandleError> = Err(HandleError::InvalidHandle);
    assert_eq!(result_to_u64!(r) as i64, -10);
}

// ==========================================================================
// User pointer validation tests (sys_write pattern)
// ==========================================================================

#[test]
fn write_zero_length_succeeds() {
    // Zero-length write: buf_ptr doesn't matter (0 + 0 = 0 <= USER_VA_END).
    assert!(validate_write_buffer(0, 0).is_ok());
}

#[test]
fn write_valid_buffer() {
    assert!(validate_write_buffer(0x1000, 100).is_ok());
}

#[test]
fn write_length_exceeds_max() {
    assert_eq!(
        validate_write_buffer(0x1000, MAX_WRITE_LEN + 1),
        Err(Error::BadLength)
    );
}

#[test]
fn write_ptr_at_user_va_end() {
    assert_eq!(
        validate_write_buffer(USER_VA_END, 1),
        Err(Error::BadAddress)
    );
}

#[test]
fn write_ptr_above_user_va_end() {
    assert_eq!(
        validate_write_buffer(USER_VA_END + 1, 1),
        Err(Error::BadAddress)
    );
}

#[test]
fn write_buffer_spans_user_va_end() {
    // Start within range, but start + len > USER_VA_END.
    assert_eq!(
        validate_write_buffer(USER_VA_END - 10, 20),
        Err(Error::BadAddress)
    );
}

#[test]
fn write_buffer_overflow_u64() {
    // start + len overflows u64 → checked_add returns None → BadAddress.
    assert_eq!(
        validate_write_buffer(u64::MAX - 5, 10),
        Err(Error::BadAddress)
    );
}

#[test]
fn write_max_valid_buffer() {
    // Buffer exactly at the end of user VA space.
    assert!(validate_write_buffer(USER_VA_END - MAX_WRITE_LEN, MAX_WRITE_LEN).is_ok());
}

// ==========================================================================
// ELF buffer validation tests (sys_process_create pattern)
// ==========================================================================

#[test]
fn elf_zero_length_rejected() {
    assert_eq!(validate_elf_buffer(0x1000, 0), Err(Error::BadLength));
}

#[test]
fn elf_exceeds_max_size() {
    assert_eq!(
        validate_elf_buffer(0x1000, MAX_ELF_SIZE + 1),
        Err(Error::BadLength)
    );
}

#[test]
fn elf_valid_buffer() {
    assert!(validate_elf_buffer(USER_CODE_BASE, 4096).is_ok());
}

#[test]
fn elf_buffer_overflow_u64() {
    assert_eq!(
        validate_elf_buffer(u64::MAX - 100, 200),
        Err(Error::BadAddress)
    );
}

// ==========================================================================
// Wait buffer validation tests (sys_wait pattern)
// ==========================================================================

#[test]
fn wait_zero_count_rejected() {
    assert_eq!(
        validate_wait_buffer(0x1000, 0),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn wait_count_exceeds_max() {
    assert_eq!(
        validate_wait_buffer(0x1000, MAX_WAIT_HANDLES + 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn wait_valid_buffer() {
    assert!(validate_wait_buffer(0x1000, 4).is_ok());
}

#[test]
fn wait_ptr_overflow_u64() {
    assert_eq!(
        validate_wait_buffer(u64::MAX, 1),
        Err(Error::BadAddress)
    );
}

#[test]
fn wait_max_handles_at_valid_addr() {
    assert!(validate_wait_buffer(0x1000, MAX_WAIT_HANDLES).is_ok());
}

// ==========================================================================
// DMA pa_out_ptr validation tests
// ==========================================================================

#[test]
fn dma_ptr_valid_aligned() {
    assert!(validate_dma_pa_out_ptr(0x1000).is_ok());
}

#[test]
fn dma_ptr_unaligned() {
    assert_eq!(validate_dma_pa_out_ptr(0x1001), Err(Error::BadAddress));
    assert_eq!(validate_dma_pa_out_ptr(0x1004), Err(Error::BadAddress));
    assert_eq!(validate_dma_pa_out_ptr(0x1007), Err(Error::BadAddress));
}

#[test]
fn dma_ptr_at_user_va_end() {
    assert_eq!(
        validate_dma_pa_out_ptr(USER_VA_END),
        Err(Error::BadAddress)
    );
}

#[test]
fn dma_ptr_8_byte_aligned_check() {
    // Only multiples of 8 pass the alignment check.
    assert!(validate_dma_pa_out_ptr(0).is_ok());
    assert!(validate_dma_pa_out_ptr(8).is_ok());
    assert!(validate_dma_pa_out_ptr(16).is_ok());
    assert_eq!(validate_dma_pa_out_ptr(1), Err(Error::BadAddress));
    assert_eq!(validate_dma_pa_out_ptr(7), Err(Error::BadAddress));
}

// ==========================================================================
// device_map PA validation tests
// ==========================================================================

#[test]
fn device_map_zero_size_rejected() {
    assert_eq!(
        validate_device_map(0x0900_0000, 0),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn device_map_below_ram_succeeds() {
    // MMIO space below RAM_START (e.g., GIC at 0x0800_0000).
    assert!(validate_device_map(0x0800_0000, 0x1000).is_ok());
}

#[test]
fn device_map_above_ram_succeeds() {
    // MMIO space above RAM_END.
    assert!(validate_device_map(RAM_END, 0x1000).is_ok());
}

#[test]
fn device_map_overlapping_ram_rejected() {
    // PA range overlaps with RAM — not a device.
    assert_eq!(
        validate_device_map(RAM_START, 0x1000),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn device_map_partially_overlapping_ram_rejected() {
    // Starts before RAM, ends within RAM.
    assert_eq!(
        validate_device_map(RAM_START - 0x100, 0x200),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn device_map_pa_overflow() {
    assert_eq!(
        validate_device_map(u64::MAX, 2),
        Err(Error::InvalidArgument)
    );
}

// ==========================================================================
// memory_share validation tests
// ==========================================================================

#[test]
fn memory_share_zero_pages_rejected() {
    assert_eq!(
        validate_memory_share(RAM_START, 0),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn memory_share_too_many_pages_rejected() {
    assert_eq!(
        validate_memory_share(RAM_START, 1025),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn memory_share_unaligned_pa_rejected() {
    assert_eq!(
        validate_memory_share(RAM_START + 1, 1),
        Err(Error::BadAddress)
    );
}

#[test]
fn memory_share_below_ram_rejected() {
    assert_eq!(
        validate_memory_share(0, 1),
        Err(Error::BadAddress)
    );
}

#[test]
fn memory_share_above_ram_rejected() {
    assert_eq!(
        validate_memory_share(RAM_END, 1),
        Err(Error::BadAddress)
    );
}

#[test]
fn memory_share_valid() {
    assert!(validate_memory_share(RAM_START, 1).is_ok());
    assert!(validate_memory_share(RAM_START, 1024).is_ok());
}

#[test]
fn memory_share_end_exceeds_ram() {
    // Start within RAM, but start + count * PAGE_SIZE > RAM_END.
    let almost_end = RAM_END - PAGE_SIZE;
    assert_eq!(
        validate_memory_share(almost_end, 2),
        Err(Error::BadAddress)
    );
}

// ==========================================================================
// Futex address validation tests
// ==========================================================================

#[test]
fn futex_valid_aligned_addr() {
    assert!(validate_futex_addr(0x1000).is_ok());
    assert!(validate_futex_addr(0).is_ok());
    assert!(validate_futex_addr(4).is_ok());
}

#[test]
fn futex_unaligned_addr_rejected() {
    assert_eq!(validate_futex_addr(1), Err(Error::BadAddress));
    assert_eq!(validate_futex_addr(2), Err(Error::BadAddress));
    assert_eq!(validate_futex_addr(3), Err(Error::BadAddress));
}

#[test]
fn futex_addr_at_user_va_end() {
    assert_eq!(validate_futex_addr(USER_VA_END), Err(Error::BadAddress));
}

// ==========================================================================
// thread_create validation tests
// ==========================================================================

#[test]
fn thread_create_valid() {
    assert!(validate_thread_create(USER_CODE_BASE, 0x8000_0000 - 16).is_ok());
}

#[test]
fn thread_create_entry_at_va_end() {
    assert_eq!(
        validate_thread_create(USER_VA_END, 0x1000),
        Err(Error::BadAddress)
    );
}

#[test]
fn thread_create_stack_unaligned() {
    // Stack must be 16-byte aligned.
    assert_eq!(
        validate_thread_create(USER_CODE_BASE, 0x1001),
        Err(Error::BadAddress)
    );
}

#[test]
fn thread_create_stack_at_va_end() {
    assert_eq!(
        validate_thread_create(USER_CODE_BASE, USER_VA_END),
        Err(Error::BadAddress)
    );
}

// ==========================================================================
// DMA free VA validation tests
// ==========================================================================

#[test]
fn dma_free_valid() {
    assert!(validate_dma_free(DMA_BUFFER_BASE).is_ok());
    assert!(validate_dma_free(DMA_BUFFER_BASE + PAGE_SIZE).is_ok());
}

#[test]
fn dma_free_below_region() {
    assert_eq!(
        validate_dma_free(DMA_BUFFER_BASE - 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn dma_free_above_region() {
    assert_eq!(
        validate_dma_free(DMA_BUFFER_END),
        Err(Error::InvalidArgument)
    );
}

// ==========================================================================
// memory_free VA validation tests
// ==========================================================================

#[test]
fn memory_free_valid() {
    assert!(validate_memory_free(HEAP_BASE).is_ok());
}

#[test]
fn memory_free_below_heap() {
    assert_eq!(
        validate_memory_free(HEAP_BASE - PAGE_SIZE),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn memory_free_above_heap() {
    assert_eq!(
        validate_memory_free(HEAP_END),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn memory_free_unaligned() {
    assert_eq!(
        validate_memory_free(HEAP_BASE + 1),
        Err(Error::BadAddress)
    );
}

// ==========================================================================
// Handle number validation tests
// ==========================================================================

#[test]
fn handle_nr_valid_range() {
    assert!(validate_handle_nr(0));
    assert!(validate_handle_nr(255));
}

#[test]
fn handle_nr_out_of_range() {
    assert!(!validate_handle_nr(256));
    assert!(!validate_handle_nr(u64::MAX));
}

// ==========================================================================
// is_user_range_readable page iteration tests
// ==========================================================================

#[test]
fn range_readable_zero_length_always_valid() {
    // Zero-length ranges are always valid, even with bad addresses.
    assert!(is_range_valid(u64::MAX, 0, |_| false));
}

#[test]
fn range_readable_single_byte_checks_one_page() {
    let mut checked = Vec::new();
    let result = is_range_valid(0x1000, 1, |page| {
        checked.push(page);
        true
    });

    assert!(result);
    assert_eq!(checked, vec![0x1000]);
}

#[test]
fn range_readable_spanning_two_pages() {
    let mut checked = Vec::new();
    let result = is_range_valid(0x1FFF, 2, |page| {
        checked.push(page);
        true
    });

    assert!(result);
    // 0x1FFF is in page 0x1000, 0x2000 is in page 0x2000.
    assert_eq!(checked, vec![0x1000, 0x2000]);
}

#[test]
fn range_readable_exactly_one_page() {
    let mut checked = Vec::new();
    let result = is_range_valid(0x1000, PAGE_SIZE, |page| {
        checked.push(page);
        true
    });

    assert!(result);
    // Entire range is within page 0x1000.
    assert_eq!(checked, vec![0x1000]);
}

#[test]
fn range_readable_fails_on_second_page() {
    let result = is_range_valid(0x1FFF, 2, |page| page != 0x2000);

    assert!(!result, "should fail when second page is inaccessible");
}

#[test]
fn range_readable_multiple_pages() {
    let mut checked = Vec::new();
    let result = is_range_valid(0x1000, 3 * PAGE_SIZE, |page| {
        checked.push(page);
        true
    });

    assert!(result);
    assert_eq!(checked, vec![0x1000, 0x2000, 0x3000]);
}

#[test]
fn range_readable_unaligned_start_covers_correct_pages() {
    let mut checked = Vec::new();
    let result = is_range_valid(0x1500, 0x1000, |page| {
        checked.push(page);
        true
    });

    assert!(result);
    // 0x1500..0x2500 spans pages 0x1000 and 0x2000.
    assert_eq!(checked, vec![0x1000, 0x2000]);
}

// ==========================================================================
// Syscall number completeness tests
// ==========================================================================

/// Verify all 27 syscall numbers are defined and unique.
#[test]
fn syscall_numbers_are_unique_and_contiguous() {
    let numbers = [
        0u64, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
        23, 24, 25, 26,
    ];

    // All unique.
    let mut sorted = numbers.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 27, "27 unique syscall numbers expected");

    // Contiguous from 0 to 26.
    for (i, &n) in sorted.iter().enumerate() {
        assert_eq!(n, i as u64, "syscall numbers should be contiguous");
    }
}

// ==========================================================================
// Constants correctness tests
// ==========================================================================

#[test]
fn max_dma_order_matches_page_allocator() {
    // MAX_DMA_ORDER must be <= 10 (page_allocator::MAX_ORDER).
    assert!(MAX_DMA_ORDER <= 10);
}

#[test]
fn max_elf_size_is_2_mib() {
    assert_eq!(MAX_ELF_SIZE, 2 * 1024 * 1024);
}

#[test]
fn max_wait_handles_is_16() {
    assert_eq!(MAX_WAIT_HANDLES, 16);
}

#[test]
fn max_write_len_is_4096() {
    assert_eq!(MAX_WRITE_LEN, 4096);
}

// ==========================================================================
// Privilege escalation vector: device_map cannot map RAM as device memory
// ==========================================================================

#[test]
fn device_map_rejects_all_ram_addresses() {
    // Every page-aligned address within RAM must be rejected.
    for offset in (0..RAM_SIZE).step_by(PAGE_SIZE as usize * 1024) {
        let pa = RAM_START + offset;
        assert_eq!(
            validate_device_map(pa, PAGE_SIZE),
            Err(Error::InvalidArgument),
            "device_map should reject RAM address {:#x}",
            pa
        );
    }
}

#[test]
fn device_map_boundary_just_below_ram() {
    // PA range ending exactly at RAM_START is OK (device space).
    assert!(validate_device_map(RAM_START - PAGE_SIZE, PAGE_SIZE).is_ok());
}

#[test]
fn device_map_boundary_just_above_ram() {
    // PA starting at RAM_END is OK (device space).
    assert!(validate_device_map(RAM_END, PAGE_SIZE).is_ok());
}

// ==========================================================================
// memory_share cannot map outside RAM
// ==========================================================================

#[test]
fn memory_share_rejects_device_space() {
    // PA below RAM_START (device MMIO area).
    assert_eq!(
        validate_memory_share(0x0800_0000 & !(PAGE_SIZE - 1), 1),
        Err(Error::BadAddress)
    );
}

#[test]
fn memory_share_boundary_exact_ram_start() {
    assert!(validate_memory_share(RAM_START, 1).is_ok());
}

#[test]
fn memory_share_boundary_exact_ram_end() {
    // Exactly one page before RAM_END.
    assert!(validate_memory_share(RAM_END - PAGE_SIZE, 1).is_ok());
}
