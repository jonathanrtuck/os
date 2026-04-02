//! Adversarial tests for boundary values and state confusion across all syscalls.
//!
//! **Boundary values:** u64::MAX for counts/sizes, zero-length buffers, extreme
//! parameters for all parameter-accepting syscalls.
//!
//! **State confusion:** double-close, signal-after-close, ack-without-register,
//! start-already-started, kill-already-exited.
//!
//! **Specific syscall coverage:** device_map RAM rejection, thread_create address
//! validation, scheduling_context_create extreme budget/period, process_create
//! malformed ELF, handle_send to started process.
//!
//! Tests duplicate the pure validation logic from kernel source. The kernel
//! targets aarch64-unknown-none so we cannot import it directly.
//!
//! Fulfills: VAL-FUZZ-003, VAL-FUZZ-005, VAL-FUZZ-006, VAL-FUZZ-007,
//!           VAL-FUZZ-008, VAL-FUZZ-009, VAL-FUZZ-010
//!
//! Run with: cargo test --test adversarial_boundaries_state -- --test-threads=1

// --- Stubs for kernel types ---

#[path = "../../paging.rs"]
mod paging;

mod event {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct EventId(pub u32);
}

#[path = "../../handle.rs"]
mod handle;

#[path = "../../scheduling_context.rs"]
mod scheduling_context;

#[path = "../../executable.rs"]
mod executable;

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
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}
mod vmo {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct VmoId(pub u32);
}
// Stub for address_space module referenced by executable.rs
mod address_space {
    pub struct PageAttrs {
        pub el0: bool,
        pub writable: bool,
        pub executable: bool,
    }
    impl PageAttrs {
        pub fn user_rx() -> Self {
            Self {
                el0: true,
                writable: false,
                executable: true,
            }
        }
        pub fn user_rw() -> Self {
            Self {
                el0: true,
                writable: true,
                executable: false,
            }
        }
        pub fn user_ro() -> Self {
            Self {
                el0: true,
                writable: false,
                executable: false,
            }
        }
        pub fn user_xo() -> Self {
            Self {
                el0: false,
                writable: false,
                executable: true,
            }
        }
    }
}

use handle::*;
use paging::*;
use scheduling_context::*;

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

// --- Helper constructors for HandleObject variants ---

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
// Validation functions duplicated from syscall.rs
// ==========================================================================

/// sys_write validation.
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

/// sys_wait validation.
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

/// sys_dma_alloc validation.
fn validate_dma_alloc(order: u64, pa_out_ptr: u64) -> Result<(), Error> {
    if order > MAX_DMA_ORDER {
        return Err(Error::InvalidArgument);
    }
    if pa_out_ptr >= USER_VA_END || pa_out_ptr & 7 != 0 {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// sys_dma_free validation.
fn validate_dma_free(va: u64, _order: u64) -> Result<(), Error> {
    if !(DMA_BUFFER_BASE..DMA_BUFFER_END).contains(&va) {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

/// sys_process_create validation.
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

/// sys_memory_alloc validation.
fn validate_memory_alloc(page_count: u64) -> Result<(), Error> {
    if page_count == 0 {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

/// sys_memory_free validation.
fn validate_memory_free(va: u64, _page_count: u64) -> Result<(), Error> {
    if !(HEAP_BASE..HEAP_END).contains(&va) {
        return Err(Error::InvalidArgument);
    }
    if va & (PAGE_SIZE - 1) != 0 {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// sys_futex_wait / sys_futex_wake validation.
fn validate_futex(addr: u64) -> Result<(), Error> {
    if addr >= USER_VA_END || addr & 3 != 0 {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// sys_memory_share validation.
fn validate_memory_share(target_handle_nr: u64, pa: u64, page_count: u64) -> Result<(), Error> {
    if target_handle_nr > u16::MAX as u64 {
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

/// sys_thread_create validation.
fn validate_thread_create(entry_va: u64, stack_top: u64) -> Result<(), Error> {
    if entry_va >= USER_VA_END {
        return Err(Error::BadAddress);
    }
    if stack_top >= USER_VA_END || stack_top & 0xF != 0 {
        return Err(Error::BadAddress);
    }
    Ok(())
}

/// sys_device_map validation.
fn validate_device_map(pa: u64, size: u64) -> Result<(), Error> {
    if size == 0 {
        return Err(Error::InvalidArgument);
    }
    let end = pa.checked_add(size).ok_or(Error::InvalidArgument)?;
    if !(end <= RAM_START || pa >= RAM_END_MAX) {
        return Err(Error::InvalidArgument); // Overlaps RAM
    }
    Ok(())
}

/// sys_scheduling_context_create validation (delegates to validate_params).
fn validate_scheduling_context_create(budget: u64, period: u64) -> Result<(), Error> {
    if !validate_params(budget, period) {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

/// sys_interrupt_register validation.
fn validate_interrupt_register(irq: u64) -> Result<(), Error> {
    if irq > u32::MAX as u64 {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

/// Handle number validation.
fn validate_handle_nr(handle_nr: u64) -> Result<u16, Error> {
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }
    Ok(handle_nr as u16)
}

/// Simulate process state: tracks whether process has been started.
struct SimProcess {
    handles: HandleTable,
    started: bool,
}

impl SimProcess {
    fn new() -> Self {
        Self {
            handles: HandleTable::new(),
            started: false,
        }
    }
}

/// Simulate handle_send: target must be Process, target process must not be started.
fn simulate_handle_send(
    caller: &mut HandleTable,
    target: &SimProcess,
    target_handle_nr: u16,
    source_handle_nr: u16,
) -> Result<(), &'static str> {
    // Check target is a Process handle
    match caller.get(Handle(target_handle_nr), Rights::WRITE) {
        Ok(HandleObject::Process(_)) => {}
        Ok(_) => return Err("wrong target type"),
        Err(_) => return Err("invalid target handle"),
    };
    // Check source exists
    match caller.get(Handle(source_handle_nr), Rights::READ) {
        Ok(_) => {}
        Err(_) => return Err("invalid source handle"),
    };
    // Check target process is not started
    if target.started {
        return Err("already started");
    }
    Ok(())
}

/// Simulate process_start: target must be Process, returns false if already started.
fn simulate_process_start(
    handles: &HandleTable,
    handle_nr: u16,
    started: bool,
) -> Result<(), &'static str> {
    match handles.get(Handle(handle_nr), Rights::WRITE) {
        Ok(HandleObject::Process(_)) => {}
        Ok(_) => return Err("wrong type"),
        Err(_) => return Err("invalid handle"),
    };
    if started {
        return Err("already started");
    }
    Ok(())
}

/// Simulate process_kill: target must be Process.
fn simulate_process_kill(
    handles: &HandleTable,
    handle_nr: u16,
    target_alive: bool,
) -> Result<(), &'static str> {
    match handles.get(Handle(handle_nr), Rights::WRITE) {
        Ok(HandleObject::Process(_)) => {}
        Ok(_) => return Err("wrong type"),
        Err(_) => return Err("invalid handle"),
    };
    if !target_alive {
        return Err("already exited");
    }
    Ok(())
}

/// ELF helper: compute checked_page_count (matches process.rs).
fn checked_page_count(mem_size: u64) -> Result<u64, &'static str> {
    mem_size
        .checked_add(PAGE_SIZE - 1)
        .map(|n| n / PAGE_SIZE)
        .ok_or("segment mem_size overflow")
}

/// ELF helper: compute checked_vma_end (matches process.rs).
fn checked_vma_end(base_va: u64, page_count: u64) -> Result<u64, &'static str> {
    page_count
        .checked_mul(PAGE_SIZE)
        .and_then(|size| base_va.checked_add(size))
        .ok_or("segment VMA end overflow")
}

// ==========================================================================
// SECTION 1: Boundary value tests — u64::MAX for counts/sizes
// ==========================================================================

#[test]
fn boundary_write_u64_max_len() {
    // u64::MAX exceeds MAX_WRITE_LEN → BadLength.
    assert_eq!(validate_write(0x1000, u64::MAX), Err(Error::BadLength));
}

#[test]
fn boundary_write_u64_max_ptr() {
    // u64::MAX as pointer → BadAddress (>= USER_VA_END).
    assert_eq!(validate_write(u64::MAX, 0), Err(Error::BadAddress));
    assert_eq!(validate_write(u64::MAX, 1), Err(Error::BadAddress));
}

#[test]
fn boundary_write_u64_max_both() {
    // Both u64::MAX: len check fires first.
    assert_eq!(validate_write(u64::MAX, u64::MAX), Err(Error::BadLength));
}

#[test]
fn boundary_write_max_valid_len() {
    // Exactly MAX_WRITE_LEN with valid ptr: passes validation.
    assert!(validate_write(0x1000, MAX_WRITE_LEN).is_ok());
    // MAX_WRITE_LEN + 1: fails.
    assert_eq!(
        validate_write(0x1000, MAX_WRITE_LEN + 1),
        Err(Error::BadLength)
    );
}

#[test]
fn boundary_wait_u64_max_count() {
    // u64::MAX as count: exceeds MAX_WAIT_HANDLES → InvalidArgument.
    assert_eq!(validate_wait(0x1000, u64::MAX), Err(Error::InvalidArgument));
}

#[test]
fn boundary_wait_u64_max_ptr() {
    assert_eq!(validate_wait(u64::MAX, 1), Err(Error::BadAddress));
}

#[test]
fn boundary_wait_max_valid_count() {
    // MAX_WAIT_HANDLES is valid.
    assert!(validate_wait(0x1000, MAX_WAIT_HANDLES).is_ok());
    // MAX_WAIT_HANDLES + 1 is invalid.
    assert_eq!(
        validate_wait(0x1000, MAX_WAIT_HANDLES + 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn boundary_wait_count_boundary_overflow() {
    // handles_ptr near USER_VA_END + large count = overflow.
    assert_eq!(validate_wait(USER_VA_END - 1, 2), Err(Error::BadAddress));
    // handles_ptr + count wraps u64.
    assert_eq!(validate_wait(u64::MAX - 5, 10), Err(Error::BadAddress));
}

#[test]
fn boundary_dma_alloc_u64_max_order() {
    assert_eq!(
        validate_dma_alloc(u64::MAX, 0x1000),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn boundary_dma_alloc_max_valid_order() {
    // MAX_DMA_ORDER is valid.
    assert!(validate_dma_alloc(MAX_DMA_ORDER, 0x1000).is_ok());
    // MAX_DMA_ORDER + 1 is invalid.
    assert_eq!(
        validate_dma_alloc(MAX_DMA_ORDER + 1, 0x1000),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn boundary_dma_alloc_u64_max_ptr() {
    assert_eq!(validate_dma_alloc(0, u64::MAX), Err(Error::BadAddress));
}

#[test]
fn boundary_dma_free_u64_max_va() {
    assert_eq!(validate_dma_free(u64::MAX, 0), Err(Error::InvalidArgument));
}

#[test]
fn boundary_dma_free_at_boundaries() {
    // Just below DMA region.
    assert_eq!(
        validate_dma_free(DMA_BUFFER_BASE - 1, 0),
        Err(Error::InvalidArgument)
    );
    // At DMA_BUFFER_END (exclusive).
    assert_eq!(
        validate_dma_free(DMA_BUFFER_END, 0),
        Err(Error::InvalidArgument)
    );
    // At DMA_BUFFER_BASE (inclusive).
    assert!(validate_dma_free(DMA_BUFFER_BASE, 0).is_ok());
    // Just below DMA_BUFFER_END.
    assert!(validate_dma_free(DMA_BUFFER_END - 1, 0).is_ok());
}

#[test]
fn boundary_process_create_u64_max_len() {
    // u64::MAX exceeds MAX_ELF_SIZE → BadLength.
    assert_eq!(
        validate_process_create(0x1000, u64::MAX),
        Err(Error::BadLength)
    );
}

#[test]
fn boundary_process_create_max_elf_size() {
    // Exactly MAX_ELF_SIZE is valid.
    assert!(validate_process_create(0x1000, MAX_ELF_SIZE).is_ok());
    // MAX_ELF_SIZE + 1 is invalid.
    assert_eq!(
        validate_process_create(0x1000, MAX_ELF_SIZE + 1),
        Err(Error::BadLength)
    );
}

#[test]
fn boundary_process_create_ptr_plus_len_overflow() {
    // elf_ptr + elf_len overflows u64 → BadAddress.
    assert_eq!(
        validate_process_create(u64::MAX - 10, 100),
        Err(Error::BadAddress)
    );
}

#[test]
fn boundary_memory_alloc_u64_max() {
    // u64::MAX pages: validation only checks > 0, so it passes.
    // The actual allocation would fail with OutOfMemory on the real kernel.
    assert!(validate_memory_alloc(u64::MAX).is_ok());
}

#[test]
fn boundary_memory_alloc_zero() {
    assert_eq!(validate_memory_alloc(0), Err(Error::InvalidArgument));
}

#[test]
fn boundary_memory_alloc_one() {
    assert!(validate_memory_alloc(1).is_ok());
}

#[test]
fn boundary_memory_free_u64_max_va() {
    assert_eq!(
        validate_memory_free(u64::MAX, 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn boundary_memory_free_heap_boundaries() {
    // At HEAP_BASE (valid, page-aligned).
    assert!(validate_memory_free(HEAP_BASE, 1).is_ok());
    // Just below HEAP_BASE.
    assert_eq!(
        validate_memory_free(HEAP_BASE - PAGE_SIZE, 1),
        Err(Error::InvalidArgument)
    );
    // At HEAP_END (exclusive boundary → invalid).
    assert_eq!(
        validate_memory_free(HEAP_END, 1),
        Err(Error::InvalidArgument)
    );
    // Just below HEAP_END (must be page-aligned).
    let last_page = HEAP_END - PAGE_SIZE;
    assert!(validate_memory_free(last_page, 1).is_ok());
}

#[test]
fn boundary_futex_u64_max() {
    // u64::MAX is >= USER_VA_END → BadAddress.
    assert_eq!(validate_futex(u64::MAX), Err(Error::BadAddress));
}

#[test]
fn boundary_futex_u64_max_aligned() {
    // u64::MAX is not 4-byte aligned, but address check fires first.
    // Let's test u64::MAX - 3 (which IS 4-byte aligned but still in kernel range).
    assert_eq!(validate_futex(u64::MAX - 3), Err(Error::BadAddress));
}

#[test]
fn boundary_memory_share_u64_max_page_count() {
    // page_count > MAX_SHARE_PAGES → InvalidArgument.
    assert_eq!(
        validate_memory_share(0, RAM_START, u64::MAX),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn boundary_memory_share_max_page_count() {
    // MAX_SHARE_PAGES = RAM_SIZE_MAX / PAGE_SIZE / 2 (half of RAM).
    const MAX_SHARE_PAGES: u64 = RAM_SIZE_MAX / PAGE_SIZE / 2;
    assert!(validate_memory_share(0, RAM_START, MAX_SHARE_PAGES).is_ok());
    // One more is invalid.
    assert_eq!(
        validate_memory_share(0, RAM_START, MAX_SHARE_PAGES + 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn boundary_memory_share_pa_overflow() {
    // pa + page_count * PAGE_SIZE overflows u64.
    // page_count = 2048, pa near end of RAM → checked_add would catch it
    // actually page_count <= 2048 is fine, but let's pick a PA that when added
    // to 2048 * PAGE_SIZE would overflow. This requires a very high PA.
    // Use a valid page_count but PA that causes end_pa > RAM_END_MAX.
    let pa = RAM_END_MAX - PAGE_SIZE;
    assert_eq!(validate_memory_share(0, pa, 2), Err(Error::BadAddress));
}

#[test]
fn boundary_memory_share_handle_u64_max() {
    assert_eq!(
        validate_memory_share(u64::MAX, RAM_START, 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn boundary_interrupt_register_u64_max() {
    // u64::MAX > u32::MAX → InvalidArgument.
    assert_eq!(
        validate_interrupt_register(u64::MAX),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn boundary_interrupt_register_u32_max() {
    // u32::MAX is valid (fits in u32).
    assert!(validate_interrupt_register(u32::MAX as u64).is_ok());
}

#[test]
fn boundary_interrupt_register_u32_max_plus_one() {
    assert_eq!(
        validate_interrupt_register(u32::MAX as u64 + 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn boundary_handle_nr_u64_max() {
    assert_eq!(validate_handle_nr(u64::MAX), Err(Error::InvalidArgument));
}

#[test]
fn boundary_handle_nr_65536() {
    assert_eq!(validate_handle_nr(65536), Err(Error::InvalidArgument));
}

#[test]
fn boundary_handle_nr_65535() {
    // u16::MAX is valid.
    assert!(validate_handle_nr(65535).is_ok());
}

// ==========================================================================
// SECTION 2: Boundary value tests — zero-length buffers
// ==========================================================================

#[test]
fn boundary_write_zero_length() {
    // Zero length with valid ptr: passes (nothing to write).
    assert!(validate_write(0x1000, 0).is_ok());
}

#[test]
fn boundary_write_zero_length_null_ptr() {
    // Zero length with null ptr: null is < USER_VA_END so passes.
    assert!(validate_write(0, 0).is_ok());
}

#[test]
fn boundary_process_create_zero_length() {
    // Zero length: fails (elf_len must be > 0).
    assert_eq!(validate_process_create(0x1000, 0), Err(Error::BadLength));
}

#[test]
fn boundary_wait_zero_count() {
    assert_eq!(validate_wait(0x1000, 0), Err(Error::InvalidArgument));
}

#[test]
fn boundary_memory_share_zero_pages() {
    assert_eq!(
        validate_memory_share(0, RAM_START, 0),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn boundary_memory_alloc_zero_pages() {
    assert_eq!(validate_memory_alloc(0), Err(Error::InvalidArgument));
}

// ==========================================================================
// SECTION 3: Boundary value sweep — all syscalls with extreme values
// ==========================================================================

/// Sweep all validation functions with u64::MAX to ensure none panic.
#[test]
fn boundary_u64_max_sweep_no_panic() {
    let max = u64::MAX;
    let _ = validate_write(max, max);
    let _ = validate_write(max, 0);
    let _ = validate_write(0, max);
    let _ = validate_wait(max, max);
    let _ = validate_wait(max, 0);
    let _ = validate_wait(0, max);
    let _ = validate_dma_alloc(max, max);
    let _ = validate_dma_alloc(max, 0);
    let _ = validate_dma_alloc(0, max);
    let _ = validate_dma_free(max, max);
    let _ = validate_process_create(max, max);
    let _ = validate_process_create(max, 0);
    let _ = validate_process_create(0, max);
    let _ = validate_memory_alloc(max);
    let _ = validate_memory_alloc(0);
    let _ = validate_memory_free(max, max);
    let _ = validate_memory_free(0, max);
    let _ = validate_futex(max);
    let _ = validate_memory_share(max, max, max);
    let _ = validate_memory_share(0, max, 0);
    let _ = validate_memory_share(max, 0, max);
    let _ = validate_thread_create(max, max);
    let _ = validate_thread_create(max, 0);
    let _ = validate_thread_create(0, max);
    let _ = validate_device_map(max, max);
    let _ = validate_device_map(max, 0);
    let _ = validate_device_map(0, max);
    let _ = validate_scheduling_context_create(max, max);
    let _ = validate_scheduling_context_create(max, 0);
    let _ = validate_scheduling_context_create(0, max);
    let _ = validate_interrupt_register(max);
    let _ = validate_handle_nr(max);
}

/// Sweep all validation functions with zero to ensure none panic.
#[test]
fn boundary_zero_sweep_no_panic() {
    let _ = validate_write(0, 0);
    let _ = validate_wait(0, 0);
    let _ = validate_dma_alloc(0, 0);
    let _ = validate_dma_free(0, 0);
    let _ = validate_process_create(0, 0);
    let _ = validate_memory_alloc(0);
    let _ = validate_memory_free(0, 0);
    let _ = validate_futex(0);
    let _ = validate_memory_share(0, 0, 0);
    let _ = validate_thread_create(0, 0);
    let _ = validate_device_map(0, 0);
    let _ = validate_scheduling_context_create(0, 0);
    let _ = validate_interrupt_register(0);
    let _ = validate_handle_nr(0);
}

/// Sweep with boundary-adjacent values: u64::MAX - 1, 1, USER_VA_END ± 1.
#[test]
fn boundary_adjacent_values_no_panic() {
    let vals: &[u64] = &[
        0,
        1,
        u64::MAX - 1,
        u64::MAX,
        USER_VA_END - 1,
        USER_VA_END,
        USER_VA_END + 1,
        RAM_START,
        RAM_START - 1,
        RAM_END_MAX,
        RAM_END_MAX + 1,
        HEAP_BASE,
        HEAP_BASE - 1,
        HEAP_END,
        HEAP_END - 1,
        DMA_BUFFER_BASE,
        DMA_BUFFER_END,
        PAGE_SIZE,
        PAGE_SIZE - 1,
    ];

    for &v in vals {
        let _ = validate_write(v, v.min(MAX_WRITE_LEN));
        let _ = validate_wait(v, v.min(MAX_WAIT_HANDLES));
        let _ = validate_dma_alloc(v, v);
        let _ = validate_dma_free(v, v);
        let _ = validate_memory_alloc(v);
        let _ = validate_memory_free(v, v);
        let _ = validate_futex(v);
        let _ = validate_thread_create(v, v);
        let _ = validate_device_map(v, v);
        let _ = validate_scheduling_context_create(v, v);
        let _ = validate_interrupt_register(v);
        let _ = validate_handle_nr(v);
    }
}

// ==========================================================================
// SECTION 4: device_map RAM rejection (VAL-FUZZ-006)
// ==========================================================================

#[test]
fn device_map_pa_within_ram() {
    // PA entirely within RAM → reject.
    assert_eq!(
        validate_device_map(RAM_START, PAGE_SIZE),
        Err(Error::InvalidArgument)
    );
    assert_eq!(
        validate_device_map(RAM_START + PAGE_SIZE, PAGE_SIZE),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn device_map_pa_at_ram_start() {
    // PA exactly at RAM_START with size 1 → overlaps RAM.
    assert_eq!(
        validate_device_map(RAM_START, 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn device_map_pa_at_ram_end_minus_one() {
    // PA at RAM_END_MAX - 1 with size 1 → end = RAM_END_MAX, pa >= RAM_START → overlaps.
    assert_eq!(
        validate_device_map(RAM_END_MAX - 1, 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn device_map_pa_spanning_ram() {
    // Starts before RAM, ends inside RAM.
    assert_eq!(
        validate_device_map(RAM_START - PAGE_SIZE, 2 * PAGE_SIZE),
        Err(Error::InvalidArgument)
    );
    // Starts inside RAM, ends after RAM.
    assert_eq!(
        validate_device_map(RAM_END_MAX - PAGE_SIZE, 2 * PAGE_SIZE),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn device_map_pa_before_ram() {
    // Entirely before RAM → valid device MMIO.
    assert!(validate_device_map(0, PAGE_SIZE).is_ok());
    // Just before RAM_START.
    assert!(validate_device_map(RAM_START - PAGE_SIZE, PAGE_SIZE).is_ok());
}

#[test]
fn device_map_pa_after_ram() {
    // Entirely after RAM → valid device MMIO.
    assert!(validate_device_map(RAM_END_MAX, PAGE_SIZE).is_ok());
    assert!(validate_device_map(RAM_END_MAX + PAGE_SIZE, PAGE_SIZE).is_ok());
}

#[test]
fn device_map_pa_at_ram_boundaries() {
    // end == RAM_START (end = pa + size, doesn't overlap because end <= RAM_START).
    assert!(validate_device_map(RAM_START - PAGE_SIZE, PAGE_SIZE).is_ok());
    // pa == RAM_END_MAX (starts at RAM_END_MAX, doesn't overlap because pa >= RAM_END_MAX).
    assert!(validate_device_map(RAM_END_MAX, PAGE_SIZE).is_ok());
}

#[test]
fn device_map_size_zero() {
    // Size 0 → InvalidArgument (before RAM check).
    assert_eq!(
        validate_device_map(0x1000_0000, 0),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn device_map_size_overflow() {
    // pa + size overflows u64.
    assert_eq!(
        validate_device_map(u64::MAX, 1),
        Err(Error::InvalidArgument)
    );
    assert_eq!(
        validate_device_map(u64::MAX - 10, 20),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn device_map_u64_max_pa() {
    assert_eq!(
        validate_device_map(u64::MAX, u64::MAX),
        Err(Error::InvalidArgument) // size=u64::MAX checked_add overflows
    );
}

#[test]
fn device_map_entire_ram_range() {
    // Map exactly the entire RAM range → overlaps.
    assert_eq!(
        validate_device_map(RAM_START, RAM_SIZE_MAX),
        Err(Error::InvalidArgument)
    );
}

// ==========================================================================
// SECTION 5: thread_create address validation (VAL-FUZZ-007)
// ==========================================================================

#[test]
fn thread_create_kernel_entry_va() {
    // entry_va in kernel range (>= 0xFFFF_0000_0000_0000).
    assert_eq!(
        validate_thread_create(0xFFFF_0000_0000_0000, 0x1000_0000),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_thread_create(u64::MAX, 0x1000_0000),
        Err(Error::BadAddress)
    );
}

#[test]
fn thread_create_entry_at_user_va_end() {
    assert_eq!(
        validate_thread_create(USER_VA_END, 0x1000_0000),
        Err(Error::BadAddress)
    );
}

#[test]
fn thread_create_entry_just_below_user_va_end() {
    // USER_VA_END - 1 is a valid address (< USER_VA_END).
    assert!(validate_thread_create(USER_VA_END - 1, 0x1000_0000).is_ok());
}

#[test]
fn thread_create_misaligned_stack_top() {
    // Stack must be 16-byte aligned.
    for offset in &[1u64, 2, 4, 7, 8, 0xF] {
        assert_eq!(
            validate_thread_create(0x1000, 0x1000_0000 + offset),
            Err(Error::BadAddress),
            "stack_top with offset {offset} should fail alignment check"
        );
    }
}

#[test]
fn thread_create_stack_in_kernel_range() {
    assert_eq!(
        validate_thread_create(0x1000, 0xFFFF_0000_0000_0000),
        Err(Error::BadAddress)
    );
    assert_eq!(
        validate_thread_create(0x1000, u64::MAX),
        Err(Error::BadAddress)
    );
}

#[test]
fn thread_create_stack_at_user_va_end() {
    assert_eq!(
        validate_thread_create(0x1000, USER_VA_END),
        Err(Error::BadAddress)
    );
}

#[test]
fn thread_create_both_zero() {
    // Both zero: entry_va=0 < USER_VA_END, stack_top=0 is 16-byte aligned
    // and < USER_VA_END. Passes validation (AT would catch unmapped pages).
    assert!(validate_thread_create(0, 0).is_ok());
}

#[test]
fn thread_create_stack_16_byte_aligned_boundary() {
    // At USER_VA_END - 16: still valid (< USER_VA_END and 16-byte aligned).
    assert!(validate_thread_create(0x1000, USER_VA_END - 16).is_ok());
    // At USER_VA_END - 15: fails (not 16-byte aligned).
    assert_eq!(
        validate_thread_create(0x1000, USER_VA_END - 15),
        Err(Error::BadAddress)
    );
}

// ==========================================================================
// SECTION 6: scheduling_context_create parameter boundaries (VAL-FUZZ-008)
// ==========================================================================

#[test]
fn sched_ctx_zero_budget() {
    // budget = 0 < MIN_BUDGET_NS → InvalidArgument.
    assert_eq!(
        validate_scheduling_context_create(0, MIN_PERIOD_NS),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn sched_ctx_zero_period() {
    // period = 0 < MIN_PERIOD_NS → InvalidArgument.
    assert_eq!(
        validate_scheduling_context_create(MIN_BUDGET_NS, 0),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn sched_ctx_budget_gt_period() {
    // budget > period → InvalidArgument.
    assert_eq!(
        validate_scheduling_context_create(MIN_PERIOD_NS + 1, MIN_PERIOD_NS),
        Err(Error::InvalidArgument)
    );
    assert_eq!(
        validate_scheduling_context_create(MAX_PERIOD_NS + 1, MAX_PERIOD_NS),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn sched_ctx_u64_max_budget() {
    assert_eq!(
        validate_scheduling_context_create(u64::MAX, u64::MAX),
        Err(Error::InvalidArgument) // period > MAX_PERIOD_NS
    );
}

#[test]
fn sched_ctx_u64_max_period() {
    assert_eq!(
        validate_scheduling_context_create(MIN_BUDGET_NS, u64::MAX),
        Err(Error::InvalidArgument) // period > MAX_PERIOD_NS
    );
}

#[test]
fn sched_ctx_below_min_budget() {
    // Budget just below MIN_BUDGET_NS (100µs = 100_000 ns).
    assert_eq!(
        validate_scheduling_context_create(MIN_BUDGET_NS - 1, MIN_PERIOD_NS),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn sched_ctx_exactly_min_budget() {
    // Budget exactly at MIN_BUDGET_NS with matching period: valid.
    assert!(validate_scheduling_context_create(MIN_BUDGET_NS, MIN_PERIOD_NS).is_ok());
}

#[test]
fn sched_ctx_below_min_period() {
    assert_eq!(
        validate_scheduling_context_create(MIN_BUDGET_NS, MIN_PERIOD_NS - 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn sched_ctx_above_max_period() {
    assert_eq!(
        validate_scheduling_context_create(MIN_BUDGET_NS, MAX_PERIOD_NS + 1),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn sched_ctx_exactly_max_period() {
    // Budget <= period, period == MAX_PERIOD_NS: valid.
    assert!(validate_scheduling_context_create(MIN_BUDGET_NS, MAX_PERIOD_NS).is_ok());
}

#[test]
fn sched_ctx_budget_equals_period() {
    // budget == period: valid (100% utilization).
    assert!(validate_scheduling_context_create(MIN_PERIOD_NS, MIN_PERIOD_NS).is_ok());
    assert!(validate_scheduling_context_create(MAX_PERIOD_NS, MAX_PERIOD_NS).is_ok());
}

#[test]
fn sched_ctx_boundary_sweep_no_panic() {
    let vals: &[u64] = &[
        0,
        1,
        MIN_BUDGET_NS - 1,
        MIN_BUDGET_NS,
        MIN_BUDGET_NS + 1,
        MIN_PERIOD_NS - 1,
        MIN_PERIOD_NS,
        MIN_PERIOD_NS + 1,
        MAX_PERIOD_NS - 1,
        MAX_PERIOD_NS,
        MAX_PERIOD_NS + 1,
        u64::MAX / 2,
        u64::MAX - 1,
        u64::MAX,
    ];

    for &b in vals {
        for &p in vals {
            let _ = validate_scheduling_context_create(b, p);
        }
    }
}

// ==========================================================================
// SECTION 7: process_create malformed ELF (VAL-FUZZ-009)
// ==========================================================================

#[test]
fn elf_empty_buffer() {
    // Zero-length buffer: process_create validation rejects before ELF parse.
    assert_eq!(validate_process_create(0x1000, 0), Err(Error::BadLength));
}

#[test]
fn elf_too_small() {
    // Less than 64 bytes (minimum ELF header).
    let data = vec![0u8; 32];
    assert!(executable::parse_header(&data).is_err());
}

#[test]
fn elf_bad_magic() {
    // 64 bytes with wrong magic.
    let mut data = vec![0u8; 64];
    data[0] = 0x00; // Not 0x7F
    data[1] = b'E';
    data[2] = b'L';
    data[3] = b'F';
    assert!(executable::parse_header(&data).is_err());
}

#[test]
fn elf_valid_magic_garbage_fields() {
    // Valid magic but all other fields are garbage.
    let mut data = vec![0xFFu8; 64];
    data[0] = 0x7F;
    data[1] = b'E';
    data[2] = b'L';
    data[3] = b'F';
    // Class check: data[4] should be 2 (ELF64), it's 0xFF → fails.
    assert!(executable::parse_header(&data).is_err());
}

#[test]
fn elf_wrong_class() {
    let mut data = build_elf_header();
    data[4] = 1; // ELFCLASS32 instead of ELFCLASS64
    assert!(executable::parse_header(&data).is_err());
}

#[test]
fn elf_wrong_endianness() {
    let mut data = build_elf_header();
    data[5] = 2; // Big-endian instead of little-endian
    assert!(executable::parse_header(&data).is_err());
}

#[test]
fn elf_wrong_type() {
    let mut data = build_elf_header();
    // e_type at offset 16 should be ET_EXEC (2), set to ET_DYN (3).
    data[16] = 3;
    data[17] = 0;
    assert!(executable::parse_header(&data).is_err());
}

#[test]
fn elf_wrong_machine() {
    let mut data = build_elf_header();
    // e_machine at offset 18 should be EM_AARCH64 (183), set to EM_X86_64 (62).
    data[18] = 62;
    data[19] = 0;
    assert!(executable::parse_header(&data).is_err());
}

#[test]
fn elf_bad_ph_ent_size() {
    let mut data = build_elf_header();
    // e_phentsize at offset 54 must be >= 56. Set to 40 (too small).
    data[54] = 40;
    data[55] = 0;
    assert!(executable::parse_header(&data).is_err());
}

#[test]
fn elf_truncated_just_magic() {
    // Only the 4-byte magic and nothing else.
    let data = vec![0x7F, b'E', b'L', b'F'];
    assert!(executable::parse_header(&data).is_err());
}

#[test]
fn elf_valid_header_no_segments() {
    // Valid header with ph_count = 0 → parses fine (no segments to load).
    let data = build_elf_header();
    let header = executable::parse_header(&data).unwrap();
    assert_eq!(header.ph_count, 0);
}

#[test]
fn elf_segment_out_of_bounds() {
    // Valid header claiming 1 segment, but data isn't long enough.
    let mut data = build_elf_header();
    // Set ph_count = 1, ph_offset = 64, but data is only 64 bytes.
    write_u16_le(&mut data, 56, 1); // ph_count = 1
    write_u64_le(&mut data, 32, 64); // ph_offset = 64
    let header = executable::parse_header(&data).unwrap();
    assert!(executable::load_segment(&data, &header, 0).is_err());
}

#[test]
fn elf_segment_file_size_gt_mem_size() {
    // Valid header + segment where file_size > mem_size.
    let mut data = build_elf_with_segment();
    let phdr_off = 64usize; // segment starts at offset 64
                            // file_size at phdr_off + 32, mem_size at phdr_off + 40
    write_u64_le(&mut data, phdr_off + 32, 0x2000); // file_size = 0x2000
    write_u64_le(&mut data, phdr_off + 40, 0x1000); // mem_size = 0x1000 (< file_size!)
    let header = executable::parse_header(&data).unwrap();
    assert!(executable::load_segment(&data, &header, 0).is_err());
}

#[test]
fn elf_segment_data_out_of_bounds() {
    // Valid segment, but file_offset + file_size exceeds data length.
    let mut data = build_elf_with_segment();
    let phdr_off = 64usize;
    // Set file_offset to point near end of data, file_size to exceed.
    let file_off = (data.len() - 4) as u64;
    write_u64_le(&mut data, phdr_off + 8, file_off); // file_offset
    write_u64_le(&mut data, phdr_off + 32, 100); // file_size = 100 (exceeds data)
    write_u64_le(&mut data, phdr_off + 40, 100); // mem_size >= file_size
    let header = executable::parse_header(&data).unwrap();
    let seg = executable::load_segment(&data, &header, 0)
        .unwrap()
        .unwrap();
    assert!(executable::segment_data(&data, &seg).is_err());
}

#[test]
fn elf_segment_vaddr_in_kernel_range() {
    // Segment vaddr in kernel range. The ELF parser doesn't check this
    // (process.rs does the address-space validation), but it should parse
    // without panic.
    let mut data = build_elf_with_segment();
    let phdr_off = 64usize;
    write_u64_le(&mut data, phdr_off + 16, 0xFFFF_0000_0000_0000); // vaddr = kernel range
    let header = executable::parse_header(&data).unwrap();
    let seg = executable::load_segment(&data, &header, 0)
        .unwrap()
        .unwrap();
    assert_eq!(seg.vaddr, 0xFFFF_0000_0000_0000);
}

#[test]
fn elf_segment_enormous_mem_size() {
    // Segment with mem_size = u64::MAX - PAGE_SIZE + 1 → checked_page_count
    // should handle overflow.
    let result = checked_page_count(u64::MAX);
    // u64::MAX + (PAGE_SIZE - 1) overflows → None → Err.
    assert!(result.is_err());
}

#[test]
fn elf_segment_enormous_vaddr_plus_mem_size() {
    // vaddr + page_count * PAGE_SIZE overflows u64.
    let result = checked_vma_end(u64::MAX - PAGE_SIZE, 2);
    assert!(result.is_err());
}

#[test]
fn elf_checked_page_count_zero() {
    // mem_size = 0 → 0 pages (valid).
    assert_eq!(checked_page_count(0), Ok(0));
}

#[test]
fn elf_checked_page_count_one_byte() {
    // 1 byte needs 1 page.
    assert_eq!(checked_page_count(1), Ok(1));
}

#[test]
fn elf_checked_page_count_exact_page() {
    // Exactly PAGE_SIZE bytes = 1 page.
    assert_eq!(checked_page_count(PAGE_SIZE), Ok(1));
}

#[test]
fn elf_checked_page_count_page_plus_one() {
    // PAGE_SIZE + 1 bytes = 2 pages.
    assert_eq!(checked_page_count(PAGE_SIZE + 1), Ok(2));
}

#[test]
fn elf_checked_vma_end_zero_pages() {
    assert_eq!(checked_vma_end(0x1000, 0), Ok(0x1000));
}

#[test]
fn elf_checked_vma_end_overflow() {
    // Large page_count causes mul overflow.
    assert!(checked_vma_end(0x1000, u64::MAX / PAGE_SIZE + 1).is_err());
}

// ==========================================================================
// SECTION 8: handle_send semantic restrictions (VAL-FUZZ-010)
// ==========================================================================

#[test]
fn handle_send_to_started_process() {
    // handle_send rejects if target process has already been started.
    let mut caller = HandleTable::new();
    let _ph = caller.insert(pr(1), Rights::READ_WRITE).unwrap();
    let _sh = caller.insert(ch(1), Rights::READ_WRITE).unwrap();
    let mut target = SimProcess::new();
    target.started = true; // Already started

    assert_eq!(
        simulate_handle_send(&mut caller, &target, 0, 1),
        Err("already started")
    );
}

#[test]
fn handle_send_to_not_started_process() {
    // handle_send succeeds if target process is not started.
    let mut caller = HandleTable::new();
    let _ph = caller.insert(pr(1), Rights::READ_WRITE).unwrap();
    let _sh = caller.insert(ch(1), Rights::READ_WRITE).unwrap();
    let target = SimProcess::new(); // Not started

    assert!(simulate_handle_send(&mut caller, &target, 0, 1).is_ok());
}

#[test]
fn handle_send_already_closed_handle() {
    // Source handle is already closed.
    let mut caller = HandleTable::new();
    let _ph = caller.insert(pr(1), Rights::READ_WRITE).unwrap();
    let sh = caller.insert(ch(1), Rights::READ_WRITE).unwrap();
    caller.close(sh).unwrap();
    let target = SimProcess::new();

    assert_eq!(
        simulate_handle_send(&mut caller, &target, 0, 1),
        Err("invalid source handle")
    );
}

#[test]
fn handle_send_wrong_type_target() {
    // Target is a Channel, not a Process.
    let mut caller = HandleTable::new();
    let _ch = caller.insert(ch(1), Rights::READ_WRITE).unwrap(); // slot 0: Channel
    let _ch2 = caller.insert(ch(2), Rights::READ_WRITE).unwrap(); // slot 1: source
    let target = SimProcess::new();

    assert_eq!(
        simulate_handle_send(&mut caller, &target, 0, 1),
        Err("wrong target type")
    );
}

#[test]
fn handle_send_all_non_process_target_types() {
    // All non-Process handle types as target should fail.
    let mut caller = HandleTable::new();
    caller.insert(ch(1), Rights::READ_WRITE).unwrap(); // 0: Channel
    caller.insert(tm(1), Rights::READ_WRITE).unwrap(); // 1: Timer
    caller.insert(int(1), Rights::READ_WRITE).unwrap(); // 2: Interrupt
    caller.insert(sc(1), Rights::READ_WRITE).unwrap(); // 3: SchedulingContext
    caller.insert(th(1), Rights::READ_WRITE).unwrap(); // 4: Thread
    caller.insert(pr(1), Rights::READ_WRITE).unwrap(); // 5: Process (valid)
    caller.insert(ch(2), Rights::READ_WRITE).unwrap(); // 6: source handle
    let target = SimProcess::new();

    for wrong_target in 0..5u16 {
        assert_eq!(
            simulate_handle_send(&mut caller, &target, wrong_target, 6),
            Err("wrong target type"),
            "target slot {wrong_target} should fail (not Process)"
        );
    }
    // Slot 5 (Process) should succeed.
    assert!(simulate_handle_send(&mut caller, &target, 5, 6).is_ok());
}

// ==========================================================================
// SECTION 9: State confusion — double-close (VAL-FUZZ-005)
// ==========================================================================

#[test]
fn state_double_close_channel() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));
}

#[test]
fn state_double_close_timer() {
    let mut t = HandleTable::new();
    let h = t.insert(tm(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));
}

#[test]
fn state_double_close_interrupt() {
    let mut t = HandleTable::new();
    let h = t.insert(int(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));
}

#[test]
fn state_double_close_scheduling_context() {
    let mut t = HandleTable::new();
    let h = t.insert(sc(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));
}

#[test]
fn state_double_close_process() {
    let mut t = HandleTable::new();
    let h = t.insert(pr(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));
}

#[test]
fn state_double_close_thread() {
    let mut t = HandleTable::new();
    let h = t.insert(th(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));
}

#[test]
fn state_triple_close() {
    // Even a triple close should consistently return InvalidHandle.
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));
    assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));
}

// ==========================================================================
// SECTION 10: State confusion — signal-after-close
// ==========================================================================

fn try_channel_signal(t: &HandleTable, handle_nr: u16) -> Result<(), &'static str> {
    match t.get(Handle(handle_nr), Rights::WRITE) {
        Ok(HandleObject::Channel(_)) => Ok(()),
        Ok(_) => Err("wrong type"),
        Err(_) => Err("invalid handle"),
    }
}

#[test]
fn state_signal_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert_eq!(try_channel_signal(&t, h.0), Err("invalid handle"));
}

#[test]
fn state_signal_after_close_reinsert() {
    // Close, then insert a new handle in the same slot.
    // Signal should operate on the new handle, not the old one.
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    // Insert a Timer at the same slot (slot 0 is free now).
    let h2 = t.insert(tm(1), Rights::READ_WRITE).unwrap();
    assert_eq!(h.0, h2.0); // Same slot reused
                           // Signal should fail: slot has a Timer, not a Channel.
    assert_eq!(try_channel_signal(&t, h2.0), Err("wrong type"));
}

// ==========================================================================
// SECTION 11: State confusion — ack-without-register
// ==========================================================================

fn try_interrupt_ack(t: &HandleTable, handle_nr: u16) -> Result<(), &'static str> {
    match t.get(Handle(handle_nr), Rights::WRITE) {
        Ok(HandleObject::Interrupt(_)) => Ok(()),
        Ok(_) => Err("wrong type"),
        Err(_) => Err("invalid handle"),
    }
}

#[test]
fn state_ack_without_register() {
    // Ack on an empty handle table → invalid handle.
    let t = HandleTable::new();
    assert_eq!(try_interrupt_ack(&t, 0), Err("invalid handle"));
    assert_eq!(try_interrupt_ack(&t, 255), Err("invalid handle"));
}

#[test]
fn state_ack_on_non_interrupt_handle() {
    // Ack on a Channel handle → wrong type.
    let mut t = HandleTable::new();
    t.insert(ch(1), Rights::READ_WRITE).unwrap();
    assert_eq!(try_interrupt_ack(&t, 0), Err("wrong type"));
}

#[test]
fn state_ack_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(int(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert_eq!(try_interrupt_ack(&t, h.0), Err("invalid handle"));
}

// ==========================================================================
// SECTION 12: State confusion — start-already-started
// ==========================================================================

#[test]
fn state_start_already_started() {
    let mut t = HandleTable::new();
    t.insert(pr(1), Rights::READ_WRITE).unwrap();
    // First start: succeeds.
    assert!(simulate_process_start(&t, 0, false).is_ok());
    // Second start: fails (already started).
    assert_eq!(simulate_process_start(&t, 0, true), Err("already started"));
}

#[test]
fn state_start_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(pr(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert_eq!(
        simulate_process_start(&t, h.0, false),
        Err("invalid handle")
    );
}

#[test]
fn state_start_wrong_type() {
    let mut t = HandleTable::new();
    t.insert(ch(1), Rights::READ_WRITE).unwrap();
    assert_eq!(simulate_process_start(&t, 0, false), Err("wrong type"));
}

// ==========================================================================
// SECTION 13: State confusion — kill-already-exited
// ==========================================================================

#[test]
fn state_kill_already_exited() {
    let mut t = HandleTable::new();
    t.insert(pr(1), Rights::READ_WRITE).unwrap();
    // Kill of a living process: succeeds.
    assert!(simulate_process_kill(&t, 0, true).is_ok());
    // Kill of an already-exited process: fails.
    assert_eq!(simulate_process_kill(&t, 0, false), Err("already exited"));
}

#[test]
fn state_kill_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(pr(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert_eq!(simulate_process_kill(&t, h.0, true), Err("invalid handle"));
}

#[test]
fn state_kill_wrong_type() {
    let mut t = HandleTable::new();
    t.insert(ch(1), Rights::READ_WRITE).unwrap();
    assert_eq!(simulate_process_kill(&t, 0, true), Err("wrong type"));
}

// ==========================================================================
// SECTION 14: State confusion — scheduling_context operations after close
// ==========================================================================

fn try_sched_bind(t: &HandleTable, handle_nr: u16) -> Result<(), &'static str> {
    match t.get(Handle(handle_nr), Rights::READ) {
        Ok(HandleObject::SchedulingContext(_)) => Ok(()),
        _ => Err("invalid or wrong type"),
    }
}

fn try_sched_borrow(t: &HandleTable, handle_nr: u16) -> Result<(), &'static str> {
    match t.get(Handle(handle_nr), Rights::READ) {
        Ok(HandleObject::SchedulingContext(_)) => Ok(()),
        _ => Err("invalid or wrong type"),
    }
}

#[test]
fn state_sched_bind_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(sc(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert_eq!(try_sched_bind(&t, h.0), Err("invalid or wrong type"));
}

#[test]
fn state_sched_borrow_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(sc(1), Rights::READ_WRITE).unwrap();
    t.close(h).unwrap();
    assert_eq!(try_sched_borrow(&t, h.0), Err("invalid or wrong type"));
}

#[test]
fn state_sched_bind_wrong_type() {
    let mut t = HandleTable::new();
    t.insert(ch(1), Rights::READ_WRITE).unwrap();
    assert_eq!(try_sched_bind(&t, 0), Err("invalid or wrong type"));
}

// ==========================================================================
// SECTION 15: State confusion — timer/wait operations after close
// ==========================================================================

#[test]
fn state_wait_on_closed_handle() {
    // If a handle in the wait set has been closed, get() returns InvalidHandle.
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::READ).unwrap();
    // Simulate wait's handle resolution step.
    assert!(t.get(h, Rights::READ).is_ok());
    t.close(h).unwrap();
    assert!(matches!(
        t.get(h, Rights::READ),
        Err(HandleError::InvalidHandle)
    ));
}

// ==========================================================================
// SECTION 16: State confusion — comprehensive state confusion matrix
// ==========================================================================

/// For every handle type, test: insert → close → double-close → get → close.
/// Ensures no panic on any sequence.
#[test]
fn state_confusion_all_types_lifecycle() {
    let types: &[HandleObject] = &[ch(1), tm(1), int(1), sc(1), pr(1), th(1)];

    for obj in types {
        let mut t = HandleTable::new();
        let h = t.insert(*obj, Rights::READ_WRITE).unwrap();

        // Get succeeds.
        assert!(t.get(h, Rights::READ).is_ok());

        // Close succeeds.
        t.close(h).unwrap();

        // Double-close fails.
        assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));

        // Get after close fails.
        assert!(matches!(
            t.get(h, Rights::READ),
            Err(HandleError::InvalidHandle)
        ));

        // Triple-close fails.
        assert!(matches!(t.close(h), Err(HandleError::InvalidHandle)));
    }
}

/// Interleave close and reinsert operations to test slot reuse.
#[test]
fn state_confusion_close_reinsert_cycle() {
    let mut t = HandleTable::new();

    for i in 0..50u32 {
        // Insert a Channel.
        let h = t.insert(ch(i), Rights::READ_WRITE).unwrap();
        assert!(matches!(
            t.get(h, Rights::READ),
            Ok(HandleObject::Channel(_))
        ));

        // Close it.
        t.close(h).unwrap();

        // Insert a Timer at the freed slot.
        let h2 = t.insert(tm(i as u8), Rights::READ_WRITE).unwrap();
        assert_eq!(h.0, h2.0); // Same slot reused.

        // The old handle type should NOT be accessible.
        assert!(matches!(
            t.get(h2, Rights::READ),
            Ok(HandleObject::Timer(_))
        ));

        // Clean up.
        t.close(h2).unwrap();
    }
}

/// Close handles in reverse order, then try to access all.
#[test]
fn state_confusion_close_reverse_order() {
    let mut t = HandleTable::new();
    let handles: Vec<Handle> = (0..10u32)
        .map(|i| t.insert(ch(i), Rights::READ_WRITE).unwrap())
        .collect();

    // Close in reverse order.
    for &h in handles.iter().rev() {
        t.close(h).unwrap();
    }

    // All should be invalid.
    for &h in &handles {
        assert!(matches!(
            t.get(h, Rights::READ),
            Err(HandleError::InvalidHandle)
        ));
    }
}

// ==========================================================================
// SECTION 17: Scheduling context — runtime state boundaries
// ==========================================================================

#[test]
fn sched_ctx_runtime_charge_beyond_budget() {
    // Charge more than the remaining budget → saturates to 0.
    let ctx = SchedulingContext::new(1_000_000, 10_000_000, 0);
    let charged = ctx.charge(u64::MAX);
    assert_eq!(charged.remaining, 0);
    assert!(!charged.has_budget());
}

#[test]
fn sched_ctx_runtime_replenish_far_future() {
    // Replenish with now_ns far in the future (many skipped periods).
    let ctx = SchedulingContext::new(1_000_000, 1_000_000, 0);
    let replenished = ctx.maybe_replenish(1_000_000_000_000); // 1000 seconds later
    assert_eq!(replenished.remaining, 1_000_000);
    assert!(replenished.has_budget());
}

#[test]
fn sched_ctx_runtime_replenish_at_overflow() {
    // replenish_at near u64::MAX → saturating_add prevents overflow.
    let ctx = SchedulingContext {
        budget: 1_000_000,
        period: 1_000_000,
        remaining: 0,
        replenish_at: u64::MAX - 100,
    };
    let replenished = ctx.maybe_replenish(u64::MAX);
    assert_eq!(replenished.remaining, 1_000_000);
    // replenish_at uses saturating_add, so it saturates to u64::MAX.
    assert_eq!(replenished.replenish_at, u64::MAX);
}

#[test]
fn sched_ctx_runtime_charge_zero() {
    let ctx = SchedulingContext::new(1_000_000, 10_000_000, 0);
    let charged = ctx.charge(0);
    assert_eq!(charged.remaining, 1_000_000);
}

#[test]
fn sched_ctx_runtime_max_valid_params() {
    // Test with the maximum valid parameters: budget = period = MAX_PERIOD_NS.
    // validate_params ensures budget/period are within [MIN, MAX] range,
    // so SchedulingContext::new() never receives u64::MAX.
    // NOTE: SchedulingContext::new(u64::MAX, u64::MAX, u64::MAX) panics in debug
    // mode due to now_ns + period overflow. This is safe because validate_params
    // rejects such inputs before new() is called. We test with valid maximums.
    let ctx = SchedulingContext::new(MAX_PERIOD_NS, MAX_PERIOD_NS, 0);
    assert_eq!(ctx.budget, MAX_PERIOD_NS);
    assert_eq!(ctx.remaining, MAX_PERIOD_NS);
    assert_eq!(ctx.replenish_at, MAX_PERIOD_NS);
    assert!(ctx.has_budget());

    // Even with now_ns near u64::MAX, if period is small, no overflow.
    let ctx2 = SchedulingContext::new(MIN_BUDGET_NS, MIN_PERIOD_NS, u64::MAX - MIN_PERIOD_NS);
    assert_eq!(ctx2.budget, MIN_BUDGET_NS);
    assert_eq!(ctx2.replenish_at, u64::MAX);
}

// ==========================================================================
// SECTION 18: Handle table boundary conditions
// ==========================================================================

#[test]
fn handle_table_full_all_types() {
    let mut t = HandleTable::new();
    // Fill all slots with different types.
    for i in 0..handle::MAX_HANDLES as u32 {
        let obj = match i % 6 {
            0 => ch(i),
            1 => tm(i as u8),
            2 => int(i as u8),
            3 => sc(i),
            4 => pr(i),
            _ => th(i as u64),
        };
        t.insert(obj, Rights::READ_WRITE).unwrap();
    }
    // Next insertion fails.
    assert!(matches!(
        t.insert(ch(999), Rights::READ),
        Err(HandleError::TableFull)
    ));
}

#[test]
fn handle_table_get_every_slot_empty() {
    let t = HandleTable::new();
    for i in 0..=255u16 {
        assert!(matches!(
            t.get(Handle(i), Rights::READ),
            Err(HandleError::InvalidHandle)
        ));
    }
}

// ==========================================================================
// Helpers for ELF construction
// ==========================================================================

/// Build a minimal valid ELF64 aarch64 header (64 bytes).
fn build_elf_header() -> Vec<u8> {
    let mut data = vec![0u8; 64];
    // Magic.
    data[0] = 0x7F;
    data[1] = b'E';
    data[2] = b'L';
    data[3] = b'F';
    // Class: ELF64.
    data[4] = 2;
    // Data: little-endian.
    data[5] = 1;
    // Version.
    data[6] = 1;
    // e_type: ET_EXEC.
    write_u16_le(&mut data, 16, 2);
    // e_machine: EM_AARCH64.
    write_u16_le(&mut data, 18, 183);
    // e_version.
    write_u32_le(&mut data, 20, 1);
    // e_entry.
    write_u64_le(&mut data, 24, 0x400000);
    // e_phoff: 0 (no program headers by default).
    write_u64_le(&mut data, 32, 0);
    // e_phentsize: 56.
    write_u16_le(&mut data, 54, 56);
    // e_phnum: 0.
    write_u16_le(&mut data, 56, 0);
    data
}

/// Build a valid ELF64 with one PT_LOAD segment.
fn build_elf_with_segment() -> Vec<u8> {
    let mut data = build_elf_header();
    // Set ph_offset to 64 (right after header), ph_count to 1.
    write_u64_le(&mut data, 32, 64);
    write_u16_le(&mut data, 56, 1);

    // Append program header (56 bytes).
    let phdr_start = data.len();
    data.resize(phdr_start + 56, 0);

    // p_type = PT_LOAD (1).
    write_u32_le(&mut data, phdr_start, 1);
    // p_flags = PF_R | PF_X.
    write_u32_le(&mut data, phdr_start + 4, 5);
    // p_offset = 0 (file data starts at beginning).
    write_u64_le(&mut data, phdr_start + 8, 0);
    // p_vaddr.
    write_u64_le(&mut data, phdr_start + 16, 0x400000);
    // p_paddr.
    write_u64_le(&mut data, phdr_start + 24, 0x400000);
    // p_filesz.
    write_u64_le(&mut data, phdr_start + 32, 64);
    // p_memsz.
    write_u64_le(&mut data, phdr_start + 40, PAGE_SIZE);
    // p_align.
    write_u64_le(&mut data, phdr_start + 48, PAGE_SIZE);

    data
}

fn write_u16_le(data: &mut [u8], offset: usize, val: u16) {
    let bytes = val.to_le_bytes();
    data[offset] = bytes[0];
    data[offset + 1] = bytes[1];
}

fn write_u32_le(data: &mut [u8], offset: usize, val: u32) {
    let bytes = val.to_le_bytes();
    data[offset..offset + 4].copy_from_slice(&bytes);
}

fn write_u64_le(data: &mut [u8], offset: usize, val: u64) {
    let bytes = val.to_le_bytes();
    data[offset..offset + 8].copy_from_slice(&bytes);
}
