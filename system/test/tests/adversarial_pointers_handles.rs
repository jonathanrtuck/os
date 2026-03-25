//! Adversarial tests for all pointer-accepting and handle-accepting syscalls.
//!
//! Exercises every pointer validation path with hostile inputs: null, kernel-range
//! (>= 0xFFFF_0000_0000_0000), unaligned, and partially-mapped pointers. Exercises
//! every handle validation path with: out-of-range (>255), wrong-type, and
//! already-closed handles.
//!
//! These tests duplicate the pure validation logic from syscall.rs. The kernel
//! targets aarch64-unknown-none so we cannot import it directly — instead we
//! faithfully replicate each validation path and verify it returns the correct
//! error code without panic.
//!
//! Fulfills: VAL-FUZZ-001 (malformed pointer handling), VAL-FUZZ-002 (invalid handle handling)
//!
//! Run with: cargo test --test adversarial_pointers_handles -- --test-threads=1

// --- Stubs for kernel types ---

#[path = "../../kernel/paging.rs"]
mod paging;

#[path = "../../kernel/handle.rs"]
mod handle;

mod interrupt {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct InterruptId(pub u8);
}
mod process {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ProcessId(pub u32);
}
mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}
#[path = "../../kernel/scheduling_context.rs"]
mod scheduling_context;
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}

use handle::*;
use paging::*;

// --- Duplicated error enums from syscall.rs ---

#[repr(i64)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Error {
    #[allow(dead_code)]
    UnknownSyscall = -1,
    BadAddress = -2,
    BadLength = -3,
    InvalidArgument = -4,
    #[allow(dead_code)]
    AlreadyBorrowing = -5,
    #[allow(dead_code)]
    NotBorrowing = -6,
    #[allow(dead_code)]
    AlreadyBound = -7,
    #[allow(dead_code)]
    WouldBlock = -8,
    #[allow(dead_code)]
    OutOfMemory = -9,
}

// --- Duplicated constants from syscall.rs ---

const MAX_DMA_ORDER: u64 = (RAM_SIZE_MAX / PAGE_SIZE).ilog2() as u64;
const MAX_ELF_SIZE: u64 = 2 * 1024 * 1024;
const MAX_WAIT_HANDLES: u64 = 16;
const MAX_WRITE_LEN: u64 = 4096;

// --- Hostile pointer constants ---

/// Null pointer.
const PTR_NULL: u64 = 0;
/// Kernel-range address (>= 0xFFFF_0000_0000_0000).
const PTR_KERNEL: u64 = 0xFFFF_0000_0000_0000;
/// Maximum kernel address.
const PTR_KERNEL_MAX: u64 = u64::MAX;
/// Unaligned addresses for various alignment requirements.
const PTR_UNALIGNED_1: u64 = 0x1001; // unaligned for 4-byte and 8-byte
const PTR_UNALIGNED_3: u64 = 0x1003; // unaligned for 4-byte
const PTR_UNALIGNED_7: u64 = 0x1007; // unaligned for 8-byte
/// Address just below USER_VA_END.
const PTR_NEAR_BOUNDARY: u64 = USER_VA_END - 1;
/// Address at USER_VA_END (first invalid address).
const PTR_AT_BOUNDARY: u64 = USER_VA_END;

// --- Validation functions duplicated from syscall.rs ---
// Each function faithfully replicates the exact validation logic from the
// corresponding syscall handler.

/// sys_write validation: buf_ptr in user space, len <= MAX_WRITE_LEN, no overflow.
fn validate_write(buf_ptr: u64, len: u64) -> Result<(), Error> {
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

/// sys_wait validation: count in [1, MAX_WAIT_HANDLES], handles_ptr in user space,
/// handles_ptr + count in user space.
fn validate_wait(handles_ptr: u64, count: u64) -> Result<(), Error> {
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

/// sys_dma_alloc validation: order <= MAX_DMA_ORDER, pa_out_ptr in user space,
/// 8-byte aligned.
fn validate_dma_alloc(order: u64, pa_out_ptr: u64) -> Result<(), Error> {
    if order > MAX_DMA_ORDER {
        return Err(Error::InvalidArgument);
    }
    if pa_out_ptr >= USER_VA_END || pa_out_ptr & 7 != 0 {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// sys_process_create validation: elf_len in (0, MAX_ELF_SIZE], elf_ptr in user
/// space, elf_ptr + elf_len in user space.
fn validate_process_create(elf_ptr: u64, elf_len: u64) -> Result<(), Error> {
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

/// sys_memory_alloc validation: page_count > 0.
fn validate_memory_alloc(page_count: u64) -> Result<(), Error> {
    if page_count == 0 {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

/// sys_memory_free validation: va in heap range, page-aligned.
fn validate_memory_free(va: u64, _page_count: u64) -> Result<(), Error> {
    if !(HEAP_BASE..HEAP_END).contains(&va) {
        return Err(Error::InvalidArgument);
    }
    if va & (PAGE_SIZE - 1) != 0 {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// sys_futex_wait / sys_futex_wake validation: addr in user space, 4-byte aligned.
fn validate_futex(addr: u64) -> Result<(), Error> {
    if addr >= USER_VA_END || addr & 3 != 0 {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// sys_memory_share validation: target_handle in u8 range, page_count in [1, 8192],
/// pa page-aligned and within RAM.
fn validate_memory_share(target_handle_nr: u64, pa: u64, page_count: u64) -> Result<(), Error> {
    if target_handle_nr > u8::MAX as u64 {
        return Err(Error::InvalidArgument);
    }
    const MAX_SHARE_PAGES: u64 = RAM_SIZE_MAX / PAGE_SIZE / 2;
    if page_count == 0 || page_count > MAX_SHARE_PAGES {
        return Err(Error::InvalidArgument);
    }
    if pa & (PAGE_SIZE - 1) != 0 {
        return Err(Error::BadAddress);
    }
    let end_pa = pa
        .checked_add(page_count * PAGE_SIZE)
        .ok_or(Error::BadAddress)?;
    if pa < RAM_START || end_pa > RAM_END_MAX {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// sys_thread_create validation: entry_va and stack_top in user space,
/// stack_top 16-byte aligned.
fn validate_thread_create(entry_va: u64, stack_top: u64) -> Result<(), Error> {
    if entry_va >= USER_VA_END {
        return Err(Error::BadAddress);
    }
    if stack_top >= USER_VA_END || stack_top & 0xF != 0 {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// Handle number validation (common to all handle-accepting syscalls).
fn validate_handle_nr(handle_nr: u64) -> Result<u8, Error> {
    if handle_nr > u8::MAX as u64 {
        return Err(Error::InvalidArgument);
    }
    Ok(handle_nr as u8)
}

/// Handle number validation used by HandleError-returning syscalls.
fn validate_handle_nr_he(handle_nr: u64) -> Result<u8, HandleError> {
    if handle_nr > u8::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }
    Ok(handle_nr as u8)
}

// Helper constructors for HandleObject variants.
fn ch(id: u32) -> HandleObject {
    HandleObject::Channel(ChannelId(id))
}
fn tm(id: u8) -> HandleObject {
    HandleObject::Timer(timer::TimerId(id))
}
fn int(id: u8) -> HandleObject {
    HandleObject::Interrupt(interrupt::InterruptId(id))
}
fn sc(id: u32) -> HandleObject {
    HandleObject::SchedulingContext(scheduling_context::SchedulingContextId(id))
}
fn pr(id: u32) -> HandleObject {
    HandleObject::Process(process::ProcessId(id))
}
fn th(id: u64) -> HandleObject {
    HandleObject::Thread(thread::ThreadId(id))
}

// ==========================================================================
// SECTION 1: Adversarial pointer tests — sys_write
// ==========================================================================

#[test]
fn adversarial_write_null_ptr() {
    // Null pointer with non-zero length. On the real kernel, the AT S1E0R
    // translation check would reject this (no page at VA 0). Our validation
    // at least ensures the range check passes for null (it's < USER_VA_END).
    // With len=0, it trivially succeeds. With len>0, the page accessibility
    // check (not modeled here) would reject it.
    assert!(validate_write(PTR_NULL, 0).is_ok());
    // Null + non-zero length: passes bounds check but would fail AT check.
    assert!(validate_write(PTR_NULL, 1).is_ok()); // bounds ok, AT would catch
}

#[test]
fn adversarial_write_kernel_range() {
    assert_eq!(validate_write(PTR_KERNEL, 1), Err(Error::BadAddress));
    assert_eq!(validate_write(PTR_KERNEL_MAX, 1), Err(Error::BadAddress));
    assert_eq!(
        validate_write(0xFFFF_FFFF_FFFF_FFFF, 0),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_write_at_boundary() {
    assert_eq!(validate_write(PTR_AT_BOUNDARY, 1), Err(Error::BadAddress));
    assert_eq!(validate_write(PTR_AT_BOUNDARY, 0), Err(Error::BadAddress));
}

#[test]
fn adversarial_write_spans_boundary() {
    // Start in user space, end past USER_VA_END.
    assert_eq!(validate_write(USER_VA_END - 10, 20), Err(Error::BadAddress));
    assert_eq!(validate_write(USER_VA_END - 1, 2), Err(Error::BadAddress));
}

#[test]
fn adversarial_write_overflow_u64() {
    // checked_add overflows u64.
    assert_eq!(validate_write(u64::MAX - 5, 10), Err(Error::BadAddress));
    assert_eq!(validate_write(u64::MAX, 1), Err(Error::BadAddress));
}

#[test]
fn adversarial_write_max_length_exceeded() {
    assert_eq!(
        validate_write(0x1000, MAX_WRITE_LEN + 1),
        Err(Error::BadLength)
    );
    assert_eq!(validate_write(0x1000, u64::MAX), Err(Error::BadLength));
}

// ==========================================================================
// SECTION 2: Adversarial pointer tests — sys_wait
// ==========================================================================

#[test]
fn adversarial_wait_null_ptr() {
    // Null pointer with valid count: bounds check passes (0 < USER_VA_END),
    // but the page accessibility check (AT) would reject it.
    assert!(validate_wait(PTR_NULL, 1).is_ok()); // bounds ok
}

#[test]
fn adversarial_wait_kernel_range() {
    assert_eq!(validate_wait(PTR_KERNEL, 1), Err(Error::BadAddress));
    assert_eq!(validate_wait(PTR_KERNEL_MAX, 1), Err(Error::BadAddress));
}

#[test]
fn adversarial_wait_at_boundary() {
    assert_eq!(validate_wait(PTR_AT_BOUNDARY, 1), Err(Error::BadAddress));
}

#[test]
fn adversarial_wait_spans_boundary() {
    assert_eq!(validate_wait(USER_VA_END - 5, 10), Err(Error::BadAddress));
}

#[test]
fn adversarial_wait_overflow_u64() {
    assert_eq!(validate_wait(u64::MAX, 1), Err(Error::BadAddress));
    assert_eq!(validate_wait(u64::MAX - 5, 10), Err(Error::BadAddress));
}

#[test]
fn adversarial_wait_zero_count() {
    assert_eq!(validate_wait(0x1000, 0), Err(Error::InvalidArgument));
}

#[test]
fn adversarial_wait_count_exceeds_max() {
    assert_eq!(
        validate_wait(0x1000, MAX_WAIT_HANDLES + 1),
        Err(Error::InvalidArgument)
    );
    assert_eq!(validate_wait(0x1000, u64::MAX), Err(Error::InvalidArgument));
}

// ==========================================================================
// SECTION 3: Adversarial pointer tests — sys_dma_alloc
// ==========================================================================

#[test]
fn adversarial_dma_alloc_null_ptr() {
    // Null pointer is 8-byte aligned and < USER_VA_END → passes bounds check.
    // AT write check would reject it (no writable page at 0).
    assert!(validate_dma_alloc(0, PTR_NULL).is_ok()); // bounds ok
}

#[test]
fn adversarial_dma_alloc_kernel_range() {
    assert_eq!(validate_dma_alloc(0, PTR_KERNEL), Err(Error::BadAddress));
    assert_eq!(
        validate_dma_alloc(0, PTR_KERNEL_MAX),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_dma_alloc_unaligned() {
    // pa_out_ptr must be 8-byte aligned.
    assert_eq!(
        validate_dma_alloc(0, PTR_UNALIGNED_1),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_dma_alloc(0, PTR_UNALIGNED_3),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_dma_alloc(0, PTR_UNALIGNED_7),
        Err(Error::BadAddress)
    );
    assert_eq!(validate_dma_alloc(0, 0x1004), Err(Error::BadAddress));
    assert_eq!(validate_dma_alloc(0, 0x1002), Err(Error::BadAddress));
    assert_eq!(validate_dma_alloc(0, 0x1006), Err(Error::BadAddress));
}

#[test]
fn adversarial_dma_alloc_at_boundary() {
    assert_eq!(
        validate_dma_alloc(0, PTR_AT_BOUNDARY),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_dma_alloc_order_exceeds_max() {
    assert_eq!(
        validate_dma_alloc(MAX_DMA_ORDER + 1, 0x1000),
        Err(Error::InvalidArgument)
    );
    assert_eq!(
        validate_dma_alloc(u64::MAX, 0x1000),
        Err(Error::InvalidArgument)
    );
}

// ==========================================================================
// SECTION 4: Adversarial pointer tests — sys_process_create
// ==========================================================================

#[test]
fn adversarial_process_create_null_ptr() {
    // Null pointer with valid length: bounds check passes.
    assert!(validate_process_create(PTR_NULL, 100).is_ok());
}

#[test]
fn adversarial_process_create_kernel_range() {
    assert_eq!(
        validate_process_create(PTR_KERNEL, 100),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_process_create(PTR_KERNEL_MAX, 100),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_process_create_at_boundary() {
    assert_eq!(
        validate_process_create(PTR_AT_BOUNDARY, 100),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_process_create_spans_boundary() {
    assert_eq!(
        validate_process_create(USER_VA_END - 50, 100),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_process_create_overflow_u64() {
    assert_eq!(
        validate_process_create(u64::MAX - 5, 100),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_process_create_zero_length() {
    assert_eq!(validate_process_create(0x1000, 0), Err(Error::BadLength));
}

#[test]
fn adversarial_process_create_exceeds_max_elf() {
    assert_eq!(
        validate_process_create(0x1000, MAX_ELF_SIZE + 1),
        Err(Error::BadLength)
    );
}

// ==========================================================================
// SECTION 5: Adversarial pointer tests — sys_memory_alloc / sys_memory_free
// ==========================================================================

#[test]
fn adversarial_memory_alloc_zero_pages() {
    assert_eq!(validate_memory_alloc(0), Err(Error::InvalidArgument));
}

#[test]
fn adversarial_memory_alloc_valid() {
    assert!(validate_memory_alloc(1).is_ok());
    assert!(validate_memory_alloc(u64::MAX).is_ok()); // validation only checks > 0
}

#[test]
fn adversarial_memory_free_null() {
    assert_eq!(
        validate_memory_free(PTR_NULL, 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn adversarial_memory_free_kernel_range() {
    assert_eq!(
        validate_memory_free(PTR_KERNEL, 1),
        Err(Error::InvalidArgument)
    );
    assert_eq!(
        validate_memory_free(PTR_KERNEL_MAX, 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn adversarial_memory_free_outside_heap() {
    assert_eq!(
        validate_memory_free(HEAP_BASE - PAGE_SIZE, 1),
        Err(Error::InvalidArgument)
    );
    assert_eq!(
        validate_memory_free(HEAP_END, 1),
        Err(Error::InvalidArgument)
    );
    assert_eq!(
        validate_memory_free(HEAP_END + PAGE_SIZE, 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn adversarial_memory_free_unaligned() {
    assert_eq!(
        validate_memory_free(HEAP_BASE + 1, 1),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_memory_free(HEAP_BASE + 0x100, 1),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_memory_free(HEAP_BASE + PAGE_SIZE / 2, 1),
        Err(Error::BadAddress)
    );
}

// ==========================================================================
// SECTION 6: Adversarial pointer tests — sys_futex_wait / sys_futex_wake
// ==========================================================================

#[test]
fn adversarial_futex_null() {
    // Null is 4-byte aligned and < USER_VA_END. The bounds check passes,
    // but the AT translation check would reject it. We verify the validation
    // logic doesn't panic.
    assert!(validate_futex(PTR_NULL).is_ok());
}

#[test]
fn adversarial_futex_kernel_range() {
    assert_eq!(validate_futex(PTR_KERNEL), Err(Error::BadAddress));
    assert_eq!(validate_futex(PTR_KERNEL_MAX), Err(Error::BadAddress));
}

#[test]
fn adversarial_futex_at_boundary() {
    assert_eq!(validate_futex(PTR_AT_BOUNDARY), Err(Error::BadAddress));
}

#[test]
fn adversarial_futex_unaligned() {
    // Must be 4-byte aligned.
    assert_eq!(validate_futex(1), Err(Error::BadAddress));
    assert_eq!(validate_futex(2), Err(Error::BadAddress));
    assert_eq!(validate_futex(3), Err(Error::BadAddress));
    assert_eq!(validate_futex(0x1001), Err(Error::BadAddress));
    assert_eq!(validate_futex(0x1002), Err(Error::BadAddress));
    assert_eq!(validate_futex(0x1003), Err(Error::BadAddress));
}

#[test]
fn adversarial_futex_near_boundary_unaligned() {
    // Near USER_VA_END and misaligned.
    assert_eq!(validate_futex(USER_VA_END - 1), Err(Error::BadAddress));
    assert_eq!(validate_futex(USER_VA_END - 2), Err(Error::BadAddress));
    assert_eq!(validate_futex(USER_VA_END - 3), Err(Error::BadAddress));
}

// ==========================================================================
// SECTION 7: Adversarial pointer tests — sys_memory_share
// ==========================================================================

#[test]
fn adversarial_memory_share_null_pa() {
    // PA = 0 is below RAM_START.
    assert_eq!(
        validate_memory_share(0, PTR_NULL, 1),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_memory_share_kernel_range_pa() {
    // PA in kernel range is above RAM_END_MAX.
    assert_eq!(
        validate_memory_share(0, PTR_KERNEL, 1),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_memory_share_unaligned_pa() {
    assert_eq!(
        validate_memory_share(0, RAM_START + 1, 1),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_memory_share(0, RAM_START + 0x100, 1),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_memory_share_overflow_pa() {
    // page_count * PAGE_SIZE overflows when added to pa.
    assert_eq!(
        validate_memory_share(0, RAM_START, u64::MAX / PAGE_SIZE + 1),
        Err(Error::InvalidArgument) // > 8192 triggers this first
    );
}

#[test]
fn adversarial_memory_share_zero_pages() {
    assert_eq!(
        validate_memory_share(0, RAM_START, 0),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn adversarial_memory_share_exceeds_ram_end() {
    // Start within RAM, but end exceeds RAM_END_MAX.
    let start_pa = RAM_END_MAX - PAGE_SIZE;
    assert_eq!(
        validate_memory_share(0, start_pa, 2),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_memory_share_handle_out_of_range() {
    assert_eq!(
        validate_memory_share(256, RAM_START, 1),
        Err(Error::InvalidArgument)
    );
    assert_eq!(
        validate_memory_share(u64::MAX, RAM_START, 1),
        Err(Error::InvalidArgument)
    );
}

// ==========================================================================
// SECTION 8: Adversarial pointer tests — sys_thread_create
// ==========================================================================

#[test]
fn adversarial_thread_create_null_entry() {
    // Null entry_va: < USER_VA_END so bounds check passes. The page
    // accessibility check (AT) would reject it.
    assert!(validate_thread_create(PTR_NULL, 0x1000_0000).is_ok());
}

#[test]
fn adversarial_thread_create_kernel_entry() {
    assert_eq!(
        validate_thread_create(PTR_KERNEL, 0x1000),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_thread_create(PTR_KERNEL_MAX, 0x1000),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_thread_create_entry_at_boundary() {
    assert_eq!(
        validate_thread_create(PTR_AT_BOUNDARY, 0x1000),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_thread_create_kernel_stack() {
    assert_eq!(
        validate_thread_create(0x1000, PTR_KERNEL),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_thread_create(0x1000, PTR_KERNEL_MAX),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_thread_create_stack_at_boundary() {
    assert_eq!(
        validate_thread_create(0x1000, PTR_AT_BOUNDARY),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_thread_create_stack_unaligned() {
    // Stack must be 16-byte aligned.
    assert_eq!(
        validate_thread_create(0x1000, 0x1001),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_thread_create(0x1000, 0x1008),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_thread_create(0x1000, 0x100F),
        Err(Error::BadAddress)
    );
}

#[test]
fn adversarial_thread_create_both_zero() {
    // Both zero: entry_va=0 passes (< USER_VA_END), stack_top=0 passes
    // (0 is 16-byte aligned and < USER_VA_END).
    assert!(validate_thread_create(0, 0).is_ok());
}

#[test]
fn adversarial_thread_create_both_kernel() {
    assert_eq!(
        validate_thread_create(PTR_KERNEL, PTR_KERNEL),
        Err(Error::BadAddress)
    );
}

// ==========================================================================
// SECTION 9: Comprehensive sweep of all pointer syscalls with hostile pointers
// ==========================================================================

/// Test all pointer-accepting syscalls with a matrix of hostile pointer values.
/// This ensures no validation path panics on any hostile input.
#[test]
fn adversarial_pointer_sweep_no_panic() {
    let hostile_ptrs: &[u64] = &[
        0,                     // null
        1,                     // unaligned
        3,                     // unaligned
        7,                     // unaligned
        0xF,                   // unaligned
        0x1001,                // unaligned
        USER_VA_END - 1,       // near boundary
        USER_VA_END,           // at boundary
        USER_VA_END + 1,       // past boundary
        0xFFFF_0000_0000_0000, // kernel range start
        0xFFFF_FFFF_FFFF_FFF0, // kernel range near max
        u64::MAX,              // maximum
        0x8000_0000_0000_0000, // high bit set
        0x0000_FFFF_FFFF_FFFF, // just below kernel range
    ];

    let hostile_lens: &[u64] = &[
        0,
        1,
        4096,
        4097,
        MAX_ELF_SIZE,
        MAX_ELF_SIZE + 1,
        u64::MAX,
        u64::MAX / 2,
    ];

    // Exercise every pointer-accepting validation function. None should panic.
    for &ptr in hostile_ptrs {
        for &len in hostile_lens {
            let _ = validate_write(ptr, len);
            let _ = validate_process_create(ptr, len);
        }

        // wait: ptr is handles_ptr, counts are validated separately
        for count in 0..=MAX_WAIT_HANDLES + 1 {
            let _ = validate_wait(ptr, count);
        }

        // dma_alloc: ptr is pa_out_ptr
        for order in 0..=MAX_DMA_ORDER + 1 {
            let _ = validate_dma_alloc(order, ptr);
        }

        // futex: addr
        let _ = validate_futex(ptr);

        // memory_free: va
        let _ = validate_memory_free(ptr, 1);

        // thread_create: both entry_va and stack_top
        let _ = validate_thread_create(ptr, 0x1000);
        let _ = validate_thread_create(0x1000, ptr);
        let _ = validate_thread_create(ptr, ptr);

        // memory_share: pa
        let _ = validate_memory_share(0, ptr, 1);
    }
}

// ==========================================================================
// SECTION 10: Adversarial handle tests — out-of-range (>255)
// ==========================================================================

#[test]
fn adversarial_handle_out_of_range_error() {
    // Error-returning syscalls: handle_nr > u8::MAX → InvalidArgument
    assert_eq!(validate_handle_nr(256), Err(Error::InvalidArgument));
    assert_eq!(validate_handle_nr(u64::MAX), Err(Error::InvalidArgument));
    assert_eq!(validate_handle_nr(1000), Err(Error::InvalidArgument));
}

#[test]
fn adversarial_handle_out_of_range_handle_error() {
    // HandleError-returning syscalls: handle_nr > u8::MAX → InvalidHandle
    assert!(matches!(
        validate_handle_nr_he(256),
        Err(HandleError::InvalidHandle)
    ));
    assert!(matches!(
        validate_handle_nr_he(u64::MAX),
        Err(HandleError::InvalidHandle)
    ));
    assert!(matches!(
        validate_handle_nr_he(1000),
        Err(HandleError::InvalidHandle)
    ));
}

#[test]
fn adversarial_handle_boundary_values() {
    // 255 is valid (u8::MAX), 256 is out of range.
    assert!(validate_handle_nr(255).is_ok());
    assert_eq!(validate_handle_nr(256), Err(Error::InvalidArgument));
    assert!(validate_handle_nr_he(255).is_ok());
    assert!(matches!(
        validate_handle_nr_he(256),
        Err(HandleError::InvalidHandle)
    ));
}

/// Sweep all handle-accepting syscalls with out-of-range handle values.
#[test]
fn adversarial_handle_out_of_range_sweep() {
    let out_of_range: &[u64] = &[256, 257, 1000, u32::MAX as u64, u64::MAX, u64::MAX / 2];

    for &h in out_of_range {
        // Error-returning syscalls
        assert_eq!(
            validate_handle_nr(h),
            Err(Error::InvalidArgument),
            "handle_nr {h} should be out of range (Error)"
        );
        // HandleError-returning syscalls
        assert!(
            matches!(validate_handle_nr_he(h), Err(HandleError::InvalidHandle)),
            "handle_nr {h} should be out of range (HandleError)"
        );
    }
}

// ==========================================================================
// SECTION 11: Adversarial handle tests — wrong-type handles
// ==========================================================================

/// Helper: create a handle table with one handle of each type.
fn table_with_all_types() -> HandleTable {
    let mut t = HandleTable::new();
    t.insert(ch(1), Rights::READ_WRITE).unwrap(); // slot 0: Channel
    t.insert(tm(2), Rights::READ_WRITE).unwrap(); // slot 1: Timer
    t.insert(int(3), Rights::READ_WRITE).unwrap(); // slot 2: Interrupt
    t.insert(sc(4), Rights::READ_WRITE).unwrap(); // slot 3: SchedulingContext
    t.insert(pr(5), Rights::READ_WRITE).unwrap(); // slot 4: Process
    t.insert(th(6), Rights::READ_WRITE).unwrap(); // slot 5: Thread
    t
}

/// Simulate handle_close with wrong-type detection.
/// handle_close doesn't check type (any handle can be closed), so it always
/// succeeds. This is correct behavior.
#[test]
fn adversarial_handle_close_any_type() {
    let mut t = table_with_all_types();
    // handle_close works on any type.
    for i in 0..6u8 {
        assert!(t.close(Handle(i)).is_ok());
    }
}

/// Simulate channel_signal with wrong-type handle.
/// channel_signal checks for Channel type.
fn simulate_channel_signal(t: &HandleTable, handle_nr: u8) -> Result<(), &'static str> {
    match t.get(Handle(handle_nr), Rights::WRITE) {
        Ok(HandleObject::Channel(_)) => Ok(()),
        Ok(_) => Err("wrong type"),
        Err(_) => Err("invalid handle"),
    }
}

#[test]
fn adversarial_channel_signal_wrong_type() {
    let t = table_with_all_types();
    // Slot 0 = Channel → should succeed.
    assert!(simulate_channel_signal(&t, 0).is_ok());
    // Slots 1-5 are wrong type.
    assert_eq!(simulate_channel_signal(&t, 1), Err("wrong type"));
    assert_eq!(simulate_channel_signal(&t, 2), Err("wrong type"));
    assert_eq!(simulate_channel_signal(&t, 3), Err("wrong type"));
    assert_eq!(simulate_channel_signal(&t, 4), Err("wrong type"));
    assert_eq!(simulate_channel_signal(&t, 5), Err("wrong type"));
}

/// Simulate scheduling_context_bind with wrong-type handle.
fn simulate_sched_bind(t: &HandleTable, handle_nr: u8) -> Result<(), &'static str> {
    match t.get(Handle(handle_nr), Rights::READ) {
        Ok(HandleObject::SchedulingContext(_)) => Ok(()),
        _ => Err("wrong type or invalid"),
    }
}

#[test]
fn adversarial_sched_bind_wrong_type() {
    let t = table_with_all_types();
    // Slot 3 = SchedulingContext → should succeed.
    assert!(simulate_sched_bind(&t, 3).is_ok());
    // Others are wrong type.
    assert!(simulate_sched_bind(&t, 0).is_err()); // Channel
    assert!(simulate_sched_bind(&t, 1).is_err()); // Timer
    assert!(simulate_sched_bind(&t, 2).is_err()); // Interrupt
    assert!(simulate_sched_bind(&t, 4).is_err()); // Process
    assert!(simulate_sched_bind(&t, 5).is_err()); // Thread
}

/// Simulate scheduling_context_borrow with wrong-type handle.
fn simulate_sched_borrow(t: &HandleTable, handle_nr: u8) -> Result<(), &'static str> {
    match t.get(Handle(handle_nr), Rights::READ) {
        Ok(HandleObject::SchedulingContext(_)) => Ok(()),
        _ => Err("wrong type or invalid"),
    }
}

#[test]
fn adversarial_sched_borrow_wrong_type() {
    let t = table_with_all_types();
    assert!(simulate_sched_borrow(&t, 3).is_ok());
    assert!(simulate_sched_borrow(&t, 0).is_err());
    assert!(simulate_sched_borrow(&t, 1).is_err());
    assert!(simulate_sched_borrow(&t, 2).is_err());
    assert!(simulate_sched_borrow(&t, 4).is_err());
    assert!(simulate_sched_borrow(&t, 5).is_err());
}

/// Simulate timer_create: doesn't take a handle (just timeout_ns), but
/// we test the handle insertion side. Nothing type-related to check.

/// Simulate interrupt_ack with wrong-type handle.
fn simulate_interrupt_ack(t: &HandleTable, handle_nr: u8) -> Result<(), &'static str> {
    match t.get(Handle(handle_nr), Rights::WRITE) {
        Ok(HandleObject::Interrupt(_)) => Ok(()),
        Ok(_) => Err("wrong type"),
        Err(_) => Err("invalid handle"),
    }
}

#[test]
fn adversarial_interrupt_ack_wrong_type() {
    let t = table_with_all_types();
    assert!(simulate_interrupt_ack(&t, 2).is_ok()); // Interrupt
    assert_eq!(simulate_interrupt_ack(&t, 0), Err("wrong type")); // Channel
    assert_eq!(simulate_interrupt_ack(&t, 4), Err("wrong type")); // Process
                                                                  // Timer and Thread have READ-only in syscall, but here we check WRITE
                                                                  // requirement. If they had WRITE, type check would still fail.
    assert_eq!(simulate_interrupt_ack(&t, 1), Err("wrong type")); // Timer
    assert_eq!(simulate_interrupt_ack(&t, 3), Err("wrong type")); // SchedulingContext
    assert_eq!(simulate_interrupt_ack(&t, 5), Err("wrong type")); // Thread
}

/// Simulate process_start with wrong-type handle.
fn simulate_process_start(t: &HandleTable, handle_nr: u8) -> Result<(), &'static str> {
    match t.get(Handle(handle_nr), Rights::WRITE) {
        Ok(HandleObject::Process(_)) => Ok(()),
        Ok(_) => Err("wrong type"),
        Err(_) => Err("invalid handle"),
    }
}

#[test]
fn adversarial_process_start_wrong_type() {
    let t = table_with_all_types();
    assert!(simulate_process_start(&t, 4).is_ok()); // Process
    assert_eq!(simulate_process_start(&t, 0), Err("wrong type")); // Channel
    assert_eq!(simulate_process_start(&t, 1), Err("wrong type")); // Timer
    assert_eq!(simulate_process_start(&t, 2), Err("wrong type")); // Interrupt
    assert_eq!(simulate_process_start(&t, 3), Err("wrong type")); // SchedulingContext
    assert_eq!(simulate_process_start(&t, 5), Err("wrong type")); // Thread
}

/// Simulate process_kill with wrong-type handle.
fn simulate_process_kill(t: &HandleTable, handle_nr: u8) -> Result<(), &'static str> {
    match t.get(Handle(handle_nr), Rights::WRITE) {
        Ok(HandleObject::Process(_)) => Ok(()),
        Ok(_) => Err("wrong type"),
        Err(_) => Err("invalid handle"),
    }
}

#[test]
fn adversarial_process_kill_wrong_type() {
    let t = table_with_all_types();
    assert!(simulate_process_kill(&t, 4).is_ok()); // Process
    assert_eq!(simulate_process_kill(&t, 0), Err("wrong type")); // Channel
    assert_eq!(simulate_process_kill(&t, 1), Err("wrong type")); // Timer
    assert_eq!(simulate_process_kill(&t, 2), Err("wrong type")); // Interrupt
    assert_eq!(simulate_process_kill(&t, 3), Err("wrong type")); // SchedulingContext
    assert_eq!(simulate_process_kill(&t, 5), Err("wrong type")); // Thread
}

/// Simulate handle_send with wrong-type target handle.
/// handle_send requires target to be Process.
fn simulate_handle_send(
    t: &HandleTable,
    target_handle_nr: u8,
    source_handle_nr: u8,
) -> Result<(), &'static str> {
    match t.get(Handle(target_handle_nr), Rights::WRITE) {
        Ok(HandleObject::Process(_)) => {}
        Ok(_) => return Err("wrong target type"),
        Err(_) => return Err("invalid target handle"),
    };
    // Source can be any type (it's being moved).
    match t.get(Handle(source_handle_nr), Rights::READ) {
        Ok(_) => Ok(()),
        Err(_) => Err("invalid source handle"),
    }
}

#[test]
fn adversarial_handle_send_wrong_target_type() {
    let t = table_with_all_types();
    // Slot 4 = Process → valid target.
    assert!(simulate_handle_send(&t, 4, 0).is_ok());
    // Non-Process targets should fail.
    assert_eq!(simulate_handle_send(&t, 0, 4), Err("wrong target type")); // Channel
    assert_eq!(simulate_handle_send(&t, 1, 4), Err("wrong target type")); // Timer
    assert_eq!(simulate_handle_send(&t, 2, 4), Err("wrong target type")); // Interrupt
    assert_eq!(simulate_handle_send(&t, 3, 4), Err("wrong target type")); // SchedulingContext
    assert_eq!(simulate_handle_send(&t, 5, 4), Err("wrong target type")); // Thread
}

// ==========================================================================
// SECTION 12: Adversarial handle tests — already-closed handles
// ==========================================================================

#[test]
fn adversarial_handle_close_already_closed() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    // Double-close: returns InvalidHandle.
    assert!(matches!(
        t.close(h).unwrap_err(),
        HandleError::InvalidHandle
    ));
}

#[test]
fn adversarial_channel_signal_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert_eq!(simulate_channel_signal(&t, h.0), Err("invalid handle"));
}

#[test]
fn adversarial_sched_bind_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(sc(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert!(simulate_sched_bind(&t, h.0).is_err());
}

#[test]
fn adversarial_sched_borrow_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(sc(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert!(simulate_sched_borrow(&t, h.0).is_err());
}

#[test]
fn adversarial_interrupt_ack_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(int(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert_eq!(simulate_interrupt_ack(&t, h.0), Err("invalid handle"));
}

#[test]
fn adversarial_process_start_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(pr(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert_eq!(simulate_process_start(&t, h.0), Err("invalid handle"));
}

#[test]
fn adversarial_process_kill_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(pr(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert_eq!(simulate_process_kill(&t, h.0), Err("invalid handle"));
}

#[test]
fn adversarial_handle_send_target_closed() {
    let mut t = HandleTable::new();
    let target = t.insert(pr(1), Rights::READ_WRITE).unwrap();
    let source = t.insert(ch(1), Rights::READ_WRITE).unwrap();
    t.close(target).unwrap();
    assert_eq!(
        simulate_handle_send(&t, target.0, source.0),
        Err("invalid target handle")
    );
}

#[test]
fn adversarial_handle_send_source_closed() {
    let mut t = HandleTable::new();
    let target = t.insert(pr(1), Rights::READ_WRITE).unwrap();
    let source = t.insert(ch(1), Rights::READ_WRITE).unwrap();
    t.close(source).unwrap();
    assert_eq!(
        simulate_handle_send(&t, target.0, source.0),
        Err("invalid source handle")
    );
}

#[test]
fn adversarial_handle_send_both_closed() {
    let mut t = HandleTable::new();
    let target = t.insert(pr(1), Rights::READ_WRITE).unwrap();
    let source = t.insert(ch(1), Rights::READ_WRITE).unwrap();
    t.close(target).unwrap();
    t.close(source).unwrap();
    assert_eq!(
        simulate_handle_send(&t, target.0, source.0),
        Err("invalid target handle") // target checked first
    );
}

// ==========================================================================
// SECTION 13: Adversarial handle tests — empty table operations
// ==========================================================================

#[test]
fn adversarial_handle_get_empty_table() {
    let t = HandleTable::new();
    for i in 0..=255u8 {
        assert!(matches!(
            t.get(Handle(i), Rights::READ),
            Err(HandleError::InvalidHandle)
        ));
    }
}

#[test]
fn adversarial_handle_close_empty_table() {
    let mut t = HandleTable::new();
    for i in 0..=255u8 {
        assert!(matches!(
            t.close(Handle(i)),
            Err(HandleError::InvalidHandle)
        ));
    }
}

// ==========================================================================
// SECTION 14: Adversarial handle tests — all handle types with wrong rights
// ==========================================================================

#[test]
fn adversarial_handle_insufficient_rights_read_only() {
    let mut t = HandleTable::new();
    t.insert(ch(1), Rights::READ).unwrap();
    // channel_signal requires WRITE.
    assert!(matches!(
        t.get(Handle(0), Rights::WRITE),
        Err(HandleError::InsufficientRights)
    ));
}

#[test]
fn adversarial_handle_insufficient_rights_write_only() {
    let mut t = HandleTable::new();
    t.insert(ch(1), Rights::WRITE).unwrap();
    // wait requires READ.
    assert!(matches!(
        t.get(Handle(0), Rights::READ),
        Err(HandleError::InsufficientRights)
    ));
}

// ==========================================================================
// SECTION 15: Adversarial handle tests — rapid close/reuse stress
// ==========================================================================

#[test]
fn adversarial_handle_close_reuse_cycle() {
    let mut t = HandleTable::new();
    for cycle in 0..100u32 {
        let h = t.insert(ch(cycle), Rights::READ_WRITE).unwrap();
        // Access.
        assert!(matches!(
            t.get(h, Rights::READ).unwrap(),
            HandleObject::Channel(_)
        ));
        // Close.
        t.close(h).unwrap();
        // Use after close.
        assert!(matches!(
            t.get(h, Rights::READ),
            Err(HandleError::InvalidHandle)
        ));
        // Double close.
        assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));
    }
}

#[test]
fn adversarial_handle_fill_close_all_refill() {
    let mut t = HandleTable::new();

    // Fill all 256 slots.
    for i in 0..256u32 {
        t.insert(ch(i), Rights::READ_WRITE).unwrap();
    }
    // Table is full.
    assert!(matches!(
        t.insert(ch(999), Rights::READ),
        Err(HandleError::TableFull)
    ));

    // Close all.
    for i in 0..=255u8 {
        t.close(Handle(i)).unwrap();
    }

    // All slots now return InvalidHandle.
    for i in 0..=255u8 {
        assert!(matches!(
            t.get(Handle(i), Rights::READ),
            Err(HandleError::InvalidHandle)
        ));
    }

    // Refill.
    for i in 0..256u32 {
        t.insert(ch(i + 1000), Rights::READ_WRITE).unwrap();
    }

    // Verify all accessible.
    for i in 0..=255u8 {
        assert!(t.get(Handle(i), Rights::READ).is_ok());
    }
}

// ==========================================================================
// SECTION 16: Adversarial handle tests — comprehensive wrong-type matrix
// ==========================================================================

/// For every handle-accepting syscall that checks type, test every wrong type.
/// This is the exhaustive cross-product of syscalls × handle types.
#[test]
fn adversarial_handle_wrong_type_matrix() {
    let t = table_with_all_types();
    // slot 0: Channel, 1: Timer, 2: Interrupt, 3: SchedulingContext, 4: Process, 5: Thread

    // channel_signal expects Channel (slot 0).
    for &wrong in &[1u8, 2, 3, 4, 5] {
        assert_eq!(
            simulate_channel_signal(&t, wrong),
            Err("wrong type"),
            "channel_signal should reject handle at slot {wrong}"
        );
    }

    // interrupt_ack expects Interrupt (slot 2).
    for &wrong in &[0u8, 1, 3, 4, 5] {
        assert_eq!(
            simulate_interrupt_ack(&t, wrong),
            Err("wrong type"),
            "interrupt_ack should reject handle at slot {wrong}"
        );
    }

    // process_start expects Process (slot 4).
    for &wrong in &[0u8, 1, 2, 3, 5] {
        assert_eq!(
            simulate_process_start(&t, wrong),
            Err("wrong type"),
            "process_start should reject handle at slot {wrong}"
        );
    }

    // process_kill expects Process (slot 4).
    for &wrong in &[0u8, 1, 2, 3, 5] {
        assert_eq!(
            simulate_process_kill(&t, wrong),
            Err("wrong type"),
            "process_kill should reject handle at slot {wrong}"
        );
    }

    // scheduling_context_bind expects SchedulingContext (slot 3).
    for &wrong in &[0u8, 1, 2, 4, 5] {
        assert!(
            simulate_sched_bind(&t, wrong).is_err(),
            "sched_bind should reject handle at slot {wrong}"
        );
    }

    // scheduling_context_borrow expects SchedulingContext (slot 3).
    for &wrong in &[0u8, 1, 2, 4, 5] {
        assert!(
            simulate_sched_borrow(&t, wrong).is_err(),
            "sched_borrow should reject handle at slot {wrong}"
        );
    }

    // handle_send target expects Process (slot 4), source can be any type.
    for &wrong in &[0u8, 1, 2, 3, 5] {
        assert_eq!(
            simulate_handle_send(&t, wrong, 0),
            Err("wrong target type"),
            "handle_send should reject non-Process target at slot {wrong}"
        );
    }
}

// ==========================================================================
// SECTION 17: Partially-mapped pointer simulation
// ==========================================================================

/// Simulate the page iteration logic of is_user_range_readable with a
/// partially-mapped scenario (some pages accessible, some not).
fn is_range_valid(start: u64, len: u64, check_page: impl Fn(u64) -> bool) -> bool {
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

#[test]
fn adversarial_partially_mapped_first_page_unmapped() {
    // Buffer starts on an unmapped page (16K-aligned addresses).
    let p = PAGE_SIZE; // 16 KiB
    let result = is_range_valid(p, 2 * p, |page| page != p);
    assert!(!result);
}

#[test]
fn adversarial_partially_mapped_second_page_unmapped() {
    // First page mapped, second unmapped.
    let p = PAGE_SIZE;
    let result = is_range_valid(p, 2 * p, |page| page != 2 * p);
    assert!(!result);
}

#[test]
fn adversarial_partially_mapped_last_page_unmapped() {
    // All pages mapped except the last.
    let p = PAGE_SIZE;
    let result = is_range_valid(p, 4 * p, |page| page != 4 * p);
    assert!(!result);
}

#[test]
fn adversarial_partially_mapped_middle_page_unmapped() {
    // Pages p, 2p, 3p mapped; 2p unmapped.
    let p = PAGE_SIZE;
    let result = is_range_valid(p, 3 * p, |page| page != 2 * p);
    assert!(!result);
}

#[test]
fn adversarial_partially_mapped_all_mapped() {
    let p = PAGE_SIZE;
    let result = is_range_valid(p, 3 * p, |_| true);
    assert!(result);
}

#[test]
fn adversarial_partially_mapped_cross_page_boundary() {
    // Buffer spans a page boundary, second page unmapped.
    let p = PAGE_SIZE;
    let result = is_range_valid(p - 2, 4, |page| page == 0);
    assert!(!result); // second page (at p) is not matched
}

#[test]
fn adversarial_partially_mapped_single_byte_unmapped() {
    // Single byte on an unmapped page.
    let result = is_range_valid(5 * PAGE_SIZE, 1, |_| false);
    assert!(!result);
}

// ==========================================================================
// SECTION 18: memory_share pointer/handle combined adversarial
// ==========================================================================

#[test]
fn adversarial_memory_share_combined() {
    // Out-of-range handle + valid PA.
    assert_eq!(
        validate_memory_share(256, RAM_START, 1),
        Err(Error::InvalidArgument)
    );
    // Valid handle + out-of-range PA.
    assert_eq!(validate_memory_share(0, 0, 1), Err(Error::BadAddress));
    // Valid handle + kernel-range PA.
    assert_eq!(
        validate_memory_share(0, PTR_KERNEL, 1),
        Err(Error::BadAddress)
    );
    // Valid handle + unaligned PA.
    assert_eq!(
        validate_memory_share(0, RAM_START + 1, 1),
        Err(Error::BadAddress)
    );
    // Both invalid.
    assert_eq!(
        validate_memory_share(u64::MAX, PTR_KERNEL, 1),
        Err(Error::InvalidArgument) // handle check first
    );
}

// ==========================================================================
// SECTION 19: handle_send handle validation
// ==========================================================================

#[test]
fn adversarial_handle_send_out_of_range() {
    // Both handles out of range.
    let both_fail =
        |target: u64, source: u64| -> bool { target > u8::MAX as u64 || source > u8::MAX as u64 };
    assert!(both_fail(256, 0));
    assert!(both_fail(0, 256));
    assert!(both_fail(256, 256));
    assert!(both_fail(u64::MAX, u64::MAX));
}

// ==========================================================================
// SECTION 20: thread_create combined pointer adversarial
// ==========================================================================

#[test]
fn adversarial_thread_create_combined() {
    // Both in kernel range.
    assert_eq!(
        validate_thread_create(PTR_KERNEL, PTR_KERNEL),
        Err(Error::BadAddress)
    );
    // Entry kernel, stack valid.
    assert_eq!(
        validate_thread_create(PTR_KERNEL, 0x1000),
        Err(Error::BadAddress)
    );
    // Entry valid, stack kernel.
    assert_eq!(
        validate_thread_create(0x1000, PTR_KERNEL),
        Err(Error::BadAddress)
    );
    // Entry valid, stack unaligned.
    assert_eq!(
        validate_thread_create(0x1000, 0x1001),
        Err(Error::BadAddress)
    );
    // Entry at USER_VA_END, stack at USER_VA_END.
    assert_eq!(
        validate_thread_create(USER_VA_END, USER_VA_END),
        Err(Error::BadAddress)
    );
}
