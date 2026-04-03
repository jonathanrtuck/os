//! Host-side tests for syscall BEHAVIOR (not validation).
//!
//! `kernel_syscall.rs` covers argument validation (bounds checking, error codes).
//! This file tests the actual logic that executes after validation passes:
//!
//! 1. **CLOCK_GET**: `counter_to_ns` conversion (overflow, frequency, edge cases)
//! 2. **WAIT**: Multiplexing logic (readiness scan, timeout=0 polling, empty set)
//! 3. **VMO_READ/VMO_WRITE**: Data transfer through VMO pages (cross-page, seal)
//! 4. **MEMORY_ALLOC/MEMORY_FREE**: Heap page accounting (double-free, limits)
//! 5. **HANDLE_CLOSE**: Handle lifecycle (close, double-close, resource cleanup)
//!
//! Uses self-contained models (no kernel imports) following the pattern from
//! `kernel_process_exit.rs`.

// ==========================================================================
// (1) CLOCK_GET — counter_to_ns conversion
// ==========================================================================

/// Model of `timer::counter_to_ns`. Duplicated from kernel/timer.rs.
/// Uses u128 intermediate to avoid overflow.
fn counter_to_ns(ticks: u64, freq: u64) -> u64 {
    if freq == 0 {
        return 0;
    }

    (ticks as u128 * 1_000_000_000 / freq as u128) as u64
}

#[test]
fn clock_get_zero_ticks_is_zero_ns() {
    assert_eq!(counter_to_ns(0, 24_000_000), 0);
}

#[test]
fn clock_get_zero_frequency_returns_zero() {
    // Guard: if CNTFRQ hasn't been initialized, don't divide by zero.
    assert_eq!(counter_to_ns(1_000_000, 0), 0);
    assert_eq!(counter_to_ns(u64::MAX, 0), 0);
}

#[test]
fn clock_get_24mhz_one_second() {
    // Apple Silicon hypervisor: CNTFRQ = 24 MHz.
    let freq = 24_000_000u64;
    let one_second_ticks = freq;

    assert_eq!(counter_to_ns(one_second_ticks, freq), 1_000_000_000);
}

#[test]
fn clock_get_62_5mhz_one_second() {
    // QEMU virt: CNTFRQ = 62.5 MHz.
    let freq = 62_500_000u64;
    let one_second_ticks = freq;

    assert_eq!(counter_to_ns(one_second_ticks, freq), 1_000_000_000);
}

#[test]
fn clock_get_24mhz_one_millisecond() {
    let freq = 24_000_000u64;
    let one_ms_ticks = freq / 1000; // 24000 ticks

    assert_eq!(counter_to_ns(one_ms_ticks, freq), 1_000_000);
}

#[test]
fn clock_get_24mhz_one_microsecond() {
    let freq = 24_000_000u64;
    let one_us_ticks = freq / 1_000_000; // 24 ticks

    assert_eq!(counter_to_ns(one_us_ticks, freq), 1_000);
}

#[test]
fn clock_get_single_tick() {
    // Single tick at 24 MHz = 41.666... ns, truncated to 41.
    let freq = 24_000_000u64;
    let ns = counter_to_ns(1, freq);

    assert_eq!(ns, 41); // 1_000_000_000 / 24_000_000 = 41.666...
}

#[test]
fn clock_get_u128_prevents_overflow() {
    // Without u128 intermediate: u64::MAX * 1_000_000_000 would overflow u64.
    // With u128: produces a correct (large but valid) result.
    let freq = 24_000_000u64;
    let ticks = u64::MAX;
    let ns = counter_to_ns(ticks, freq);

    // Expected: u64::MAX / 24_000_000 * 1_000_000_000
    // = 768614336404564650 * 41.666... ≈ 768614336404564650 * 41 + remainder
    // Exact: (u64::MAX as u128 * 1_000_000_000 / 24_000_000 as u128) as u64
    let expected = (u64::MAX as u128 * 1_000_000_000 / 24_000_000u128) as u64;
    assert_eq!(ns, expected);

    // Verify it's a plausible value (more than 768 quadrillion nanoseconds).
    assert!(ns > 768_000_000_000_000_000);
}

#[test]
fn clock_get_max_ticks_max_freq_no_panic() {
    // Extreme: both ticks and freq at maximum. Should not panic.
    let ns = counter_to_ns(u64::MAX, u64::MAX);

    // u64::MAX ticks at u64::MAX Hz = 1 second exactly.
    assert_eq!(ns, 1_000_000_000);
}

#[test]
fn clock_get_monotonic_consistency() {
    // Increasing ticks always produces increasing nanoseconds.
    let freq = 24_000_000u64;
    let mut prev = 0u64;

    for ticks in [
        1,
        10,
        100,
        1000,
        10_000,
        100_000,
        1_000_000,
        freq,
        freq * 60,
    ] {
        let ns = counter_to_ns(ticks, freq);
        assert!(
            ns >= prev,
            "ns must be monotonically non-decreasing: {} < {} at ticks={}",
            ns,
            prev,
            ticks
        );
        prev = ns;
    }
}

#[test]
fn clock_get_round_trip_approximate() {
    // Converting ticks→ns→ticks should be approximately identity.
    let freq = 24_000_000u64;
    let original_ticks = 24_000_000u64; // exactly 1 second

    let ns = counter_to_ns(original_ticks, freq);
    // Reverse: ns_to_ticks (same u128 formula, inverted).
    let recovered_ticks = (ns as u128 * freq as u128 / 1_000_000_000u128) as u64;

    assert_eq!(recovered_ticks, original_ticks);
}

// ==========================================================================
// (2) WAIT — multiplexing logic
// ==========================================================================

/// Minimal model of a waitable object's readiness state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitableObject {
    Channel(u32),
    Timer(u32),
}

/// Model of sys_wait readiness scan. Returns the user_index of the first
/// ready handle, or None if nothing is ready.
fn scan_readiness(
    wait_set: &[(WaitableObject, u8)], // (object, user_index)
    ready_check: impl Fn(WaitableObject) -> bool,
) -> Option<u8> {
    for &(obj, user_index) in wait_set {
        if ready_check(obj) {
            return Some(user_index);
        }
    }
    None
}

/// Models the wait syscall's top-level behavior.
/// Returns: Ok(user_index) if a handle is ready, Err(WouldBlock) if
/// timeout=0 and nothing ready, Err(InvalidArgument) for bad inputs.
#[derive(Debug, PartialEq, Eq)]
enum WaitResult {
    Ready(u8),
    WouldBlock,
    InvalidArgument,
}

const MAX_WAIT_HANDLES: usize = 16;
const TIMEOUT_SENTINEL: u8 = 0xFF;

fn model_sys_wait(
    wait_set: &[(WaitableObject, u8)],
    timeout: u64,
    ready_check: impl Fn(WaitableObject) -> bool,
) -> WaitResult {
    // Validation: empty set rejected.
    if wait_set.is_empty() || wait_set.len() > MAX_WAIT_HANDLES {
        return WaitResult::InvalidArgument;
    }

    // Readiness scan: first-ready wins.
    if let Some(user_index) = scan_readiness(wait_set, &ready_check) {
        // Timeout sentinel returns WouldBlock (internal timer expired).
        if user_index == TIMEOUT_SENTINEL {
            return WaitResult::WouldBlock;
        }
        return WaitResult::Ready(user_index);
    }

    // None ready. Timeout=0 is poll mode: return immediately.
    if timeout == 0 {
        return WaitResult::WouldBlock;
    }

    // Would block and wait for wakeup (not modeled here — tested via
    // the scheduler. We return WouldBlock as a stand-in for "blocked").
    WaitResult::WouldBlock
}

#[test]
fn wait_single_ready_handle_returns_its_index() {
    let wait_set = vec![
        (WaitableObject::Channel(0), 0),
        (WaitableObject::Channel(1), 1),
        (WaitableObject::Timer(0), 2),
    ];

    let result = model_sys_wait(&wait_set, u64::MAX, |obj| {
        matches!(obj, WaitableObject::Channel(1))
    });

    assert_eq!(result, WaitResult::Ready(1));
}

#[test]
fn wait_first_ready_wins_when_multiple_ready() {
    let wait_set = vec![
        (WaitableObject::Channel(0), 0),
        (WaitableObject::Channel(1), 1),
        (WaitableObject::Channel(2), 2),
    ];

    // All three are ready — first in scan order wins.
    let result = model_sys_wait(&wait_set, u64::MAX, |_| true);

    assert_eq!(result, WaitResult::Ready(0));
}

#[test]
fn wait_timeout_zero_returns_would_block_when_none_ready() {
    let wait_set = vec![(WaitableObject::Channel(0), 0)];

    let result = model_sys_wait(&wait_set, 0, |_| false);

    assert_eq!(result, WaitResult::WouldBlock);
}

#[test]
fn wait_timeout_zero_still_returns_ready_if_something_ready() {
    let wait_set = vec![(WaitableObject::Channel(0), 0)];

    let result = model_sys_wait(&wait_set, 0, |_| true);

    assert_eq!(result, WaitResult::Ready(0));
}

#[test]
fn wait_empty_set_returns_invalid_argument() {
    let result = model_sys_wait(&[], u64::MAX, |_| true);

    assert_eq!(result, WaitResult::InvalidArgument);
}

#[test]
fn wait_exceeding_max_handles_returns_invalid_argument() {
    let wait_set: Vec<(WaitableObject, u8)> = (0..=MAX_WAIT_HANDLES as u32)
        .map(|i| (WaitableObject::Channel(i), i as u8))
        .collect();

    assert_eq!(wait_set.len(), MAX_WAIT_HANDLES + 1);

    let result = model_sys_wait(&wait_set, u64::MAX, |_| true);

    assert_eq!(result, WaitResult::InvalidArgument);
}

#[test]
fn wait_exactly_max_handles_succeeds() {
    let wait_set: Vec<(WaitableObject, u8)> = (0..MAX_WAIT_HANDLES as u32)
        .map(|i| (WaitableObject::Channel(i), i as u8))
        .collect();

    assert_eq!(wait_set.len(), MAX_WAIT_HANDLES);

    // Last handle is ready.
    let result = model_sys_wait(&wait_set, u64::MAX, |obj| {
        matches!(obj, WaitableObject::Channel(15))
    });

    assert_eq!(result, WaitResult::Ready(15));
}

#[test]
fn wait_timeout_sentinel_returns_would_block() {
    // When the internal timeout timer fires first, its user_index is TIMEOUT_SENTINEL.
    // sys_wait translates this to WouldBlock.
    let wait_set = vec![
        (WaitableObject::Channel(0), 0),
        (WaitableObject::Timer(99), TIMEOUT_SENTINEL), // internal timeout
    ];

    // Only the timeout timer is ready.
    let result = model_sys_wait(&wait_set, 5_000_000, |obj| {
        matches!(obj, WaitableObject::Timer(99))
    });

    assert_eq!(result, WaitResult::WouldBlock);
}

#[test]
fn wait_real_handle_beats_timeout_sentinel() {
    // If both a real handle and the timeout timer are ready, scan order wins.
    // Real handles are scanned before the timeout entry (it's appended last).
    let wait_set = vec![
        (WaitableObject::Channel(0), 0),
        (WaitableObject::Timer(99), TIMEOUT_SENTINEL), // internal timeout
    ];

    // Both ready — channel at index 0 is scanned first.
    let result = model_sys_wait(&wait_set, 5_000_000, |_| true);

    assert_eq!(result, WaitResult::Ready(0));
}

#[test]
fn wait_scan_order_matches_user_array_order() {
    // The scan order must match the order handles were passed by the user.
    // user_index reflects position in the user's handle array.
    let wait_set = vec![
        (WaitableObject::Channel(10), 0),
        (WaitableObject::Channel(20), 1),
        (WaitableObject::Channel(30), 2),
    ];

    // Channel 30 (index 2) is the only one ready.
    let result = model_sys_wait(&wait_set, u64::MAX, |obj| {
        matches!(obj, WaitableObject::Channel(30))
    });

    assert_eq!(result, WaitResult::Ready(2));
}

// ==========================================================================
// (3) VMO_READ/VMO_WRITE — data transfer through VMO pages
// ==========================================================================

/// Page size for VMO model (matches kernel's 16 KiB).
const PAGE_SIZE: u64 = 16384;

/// Minimal model of a VMO for read/write behavior testing.
/// Uses in-memory Vec<u8> pages instead of physical addresses.
struct ModelVmo {
    /// Per-page storage: page_index -> page data. None = uncommitted (zero-fill).
    pages: Vec<Option<Vec<u8>>>,
    size_pages: u64,
    sealed: bool,
}

impl ModelVmo {
    fn new(size_pages: u64) -> Self {
        Self {
            pages: vec![None; size_pages as usize],
            size_pages,
            sealed: false,
        }
    }

    /// Read from VMO into buffer. Returns bytes read.
    /// Mirrors kernel vmo::read logic: uncommitted pages return zeros.
    fn read(&self, offset: u64, buf: &mut [u8]) -> Option<u64> {
        let vmo_size_bytes = self.size_pages * PAGE_SIZE;

        if offset >= vmo_size_bytes {
            return Some(0);
        }

        let available = (vmo_size_bytes - offset) as usize;
        let to_read = buf.len().min(available);
        let mut bytes_done = 0usize;

        while bytes_done < to_read {
            let current_offset = offset + bytes_done as u64;
            let page_idx = (current_offset / PAGE_SIZE) as usize;
            let page_off = (current_offset % PAGE_SIZE) as usize;
            let chunk = (PAGE_SIZE as usize - page_off).min(to_read - bytes_done);

            if let Some(page_data) = &self.pages[page_idx] {
                buf[bytes_done..bytes_done + chunk]
                    .copy_from_slice(&page_data[page_off..page_off + chunk]);
            } else {
                // Uncommitted — return zeros without allocating.
                buf[bytes_done..bytes_done + chunk].fill(0);
            }

            bytes_done += chunk;
        }

        Some(bytes_done as u64)
    }

    /// Write to VMO from buffer. Returns bytes written.
    /// Mirrors kernel vmo::write logic: commits pages on first write.
    /// Returns None if sealed.
    fn write(&mut self, offset: u64, data: &[u8]) -> Option<u64> {
        if self.sealed {
            return None;
        }

        let vmo_size_bytes = self.size_pages * PAGE_SIZE;

        if offset >= vmo_size_bytes {
            return Some(0);
        }

        let available = (vmo_size_bytes - offset) as usize;
        let to_write = data.len().min(available);
        let mut bytes_done = 0usize;

        while bytes_done < to_write {
            let current_offset = offset + bytes_done as u64;
            let page_idx = (current_offset / PAGE_SIZE) as usize;
            let page_off = (current_offset % PAGE_SIZE) as usize;
            let chunk = (PAGE_SIZE as usize - page_off).min(to_write - bytes_done);

            // Commit page if not already committed (zero-filled).
            if self.pages[page_idx].is_none() {
                self.pages[page_idx] = Some(vec![0u8; PAGE_SIZE as usize]);
            }

            let page_data = self.pages[page_idx].as_mut().unwrap();
            page_data[page_off..page_off + chunk]
                .copy_from_slice(&data[bytes_done..bytes_done + chunk]);

            bytes_done += chunk;
        }

        Some(bytes_done as u64)
    }

    fn seal(&mut self) {
        self.sealed = true;
    }
}

#[test]
fn vmo_write_then_read_single_page() {
    let mut vmo = ModelVmo::new(1);
    let data = b"hello world";

    let written = vmo.write(0, data).unwrap();
    assert_eq!(written, data.len() as u64);

    let mut buf = vec![0u8; data.len()];
    let read = vmo.read(0, &mut buf).unwrap();
    assert_eq!(read, data.len() as u64);
    assert_eq!(&buf, data);
}

#[test]
fn vmo_read_uncommitted_returns_zeros() {
    let vmo = ModelVmo::new(2);
    let mut buf = vec![0xFFu8; 100];

    let read = vmo.read(0, &mut buf).unwrap();
    assert_eq!(read, 100);
    assert!(
        buf.iter().all(|&b| b == 0),
        "uncommitted pages must zero-fill"
    );
}

#[test]
fn vmo_write_cross_page_boundary() {
    let mut vmo = ModelVmo::new(2);

    // Write data that spans the boundary between page 0 and page 1.
    let offset = PAGE_SIZE - 5;
    let data = [0xAA; 10]; // 5 bytes on page 0, 5 bytes on page 1

    let written = vmo.write(offset, &data).unwrap();
    assert_eq!(written, 10);

    // Read back across the same boundary.
    let mut buf = [0u8; 10];
    let read = vmo.read(offset, &mut buf).unwrap();
    assert_eq!(read, 10);
    assert_eq!(buf, [0xAA; 10]);
}

#[test]
fn vmo_write_at_end_truncated() {
    let mut vmo = ModelVmo::new(1);
    let vmo_size = PAGE_SIZE;

    // Write 100 bytes starting 10 bytes before the end.
    let offset = vmo_size - 10;
    let data = [0xBB; 100];

    let written = vmo.write(offset, &data).unwrap();
    assert_eq!(written, 10, "only 10 bytes fit before VMO end");

    // Verify those 10 bytes were written.
    let mut buf = [0u8; 10];
    let read = vmo.read(offset, &mut buf).unwrap();
    assert_eq!(read, 10);
    assert_eq!(buf, [0xBB; 10]);
}

#[test]
fn vmo_write_past_end_returns_zero() {
    let mut vmo = ModelVmo::new(1);

    // Offset at or beyond VMO size.
    let written = vmo.write(PAGE_SIZE, b"data").unwrap();
    assert_eq!(written, 0, "offset == vmo size: nothing writable");

    let written = vmo.write(PAGE_SIZE + 1000, b"data").unwrap();
    assert_eq!(written, 0, "offset beyond vmo size: nothing writable");
}

#[test]
fn vmo_read_past_end_returns_zero() {
    let vmo = ModelVmo::new(1);

    let mut buf = [0u8; 10];
    let read = vmo.read(PAGE_SIZE, &mut buf).unwrap();
    assert_eq!(read, 0, "offset == vmo size: nothing readable");

    let read = vmo.read(PAGE_SIZE * 100, &mut buf).unwrap();
    assert_eq!(read, 0, "offset far beyond vmo size: nothing readable");
}

#[test]
fn vmo_sealed_write_rejected() {
    let mut vmo = ModelVmo::new(1);

    // Write succeeds before seal.
    assert!(vmo.write(0, b"data").is_some());

    vmo.seal();

    // Write rejected after seal.
    assert!(
        vmo.write(0, b"more data").is_none(),
        "sealed VMO must reject writes"
    );
}

#[test]
fn vmo_sealed_read_succeeds() {
    let mut vmo = ModelVmo::new(1);
    let data = b"frozen content";

    vmo.write(0, data).unwrap();
    vmo.seal();

    // Read still works after seal.
    let mut buf = vec![0u8; data.len()];
    let read = vmo.read(0, &mut buf).unwrap();
    assert_eq!(read, data.len() as u64);
    assert_eq!(&buf, data);
}

#[test]
fn vmo_write_commits_page_on_demand() {
    let mut vmo = ModelVmo::new(4);

    // Pages start uncommitted.
    assert!(vmo.pages.iter().all(|p| p.is_none()));

    // Write to page 2 — only that page should be committed.
    vmo.write(PAGE_SIZE * 2, b"data").unwrap();

    assert!(vmo.pages[0].is_none(), "page 0 still uncommitted");
    assert!(vmo.pages[1].is_none(), "page 1 still uncommitted");
    assert!(vmo.pages[2].is_some(), "page 2 committed by write");
    assert!(vmo.pages[3].is_none(), "page 3 still uncommitted");
}

#[test]
fn vmo_cross_page_write_commits_both_pages() {
    let mut vmo = ModelVmo::new(3);

    // Write spanning pages 1 and 2.
    let offset = PAGE_SIZE + PAGE_SIZE - 4;
    let data = [0xCC; 8]; // 4 bytes on page 1, 4 bytes on page 2

    vmo.write(offset, &data).unwrap();

    assert!(vmo.pages[0].is_none(), "page 0 untouched");
    assert!(vmo.pages[1].is_some(), "page 1 committed");
    assert!(vmo.pages[2].is_some(), "page 2 committed");
}

#[test]
fn vmo_write_preserves_existing_data_on_page() {
    let mut vmo = ModelVmo::new(1);

    // Write at offset 0.
    vmo.write(0, b"AAAA").unwrap();
    // Write at offset 100 (same page, different position).
    vmo.write(100, b"BBBB").unwrap();

    // Both writes should coexist.
    let mut buf = [0u8; 4];
    vmo.read(0, &mut buf).unwrap();
    assert_eq!(&buf, b"AAAA");

    vmo.read(100, &mut buf).unwrap();
    assert_eq!(&buf, b"BBBB");

    // Bytes between should be zero (initial commit zero-fills).
    let mut gap = [0xFFu8; 1];
    vmo.read(50, &mut gap).unwrap();
    assert_eq!(gap[0], 0);
}

#[test]
fn vmo_full_page_write_and_read() {
    let mut vmo = ModelVmo::new(1);

    // Write an entire page.
    let data = vec![0x42u8; PAGE_SIZE as usize];
    let written = vmo.write(0, &data).unwrap();
    assert_eq!(written, PAGE_SIZE);

    // Read it back.
    let mut buf = vec![0u8; PAGE_SIZE as usize];
    let read = vmo.read(0, &mut buf).unwrap();
    assert_eq!(read, PAGE_SIZE);
    assert_eq!(buf, data);
}

#[test]
fn vmo_read_buffer_larger_than_vmo() {
    let vmo = ModelVmo::new(1);

    // Buffer larger than VMO — should only read up to VMO size.
    let mut buf = vec![0xFFu8; (PAGE_SIZE * 2) as usize];
    let read = vmo.read(0, &mut buf).unwrap();
    assert_eq!(read, PAGE_SIZE, "clamped to VMO size");

    // Only the first PAGE_SIZE bytes should be zeroed.
    assert!(buf[..PAGE_SIZE as usize].iter().all(|&b| b == 0));
    // The rest should be untouched (0xFF).
    assert!(buf[PAGE_SIZE as usize..].iter().all(|&b| b == 0xFF));
}

#[test]
fn vmo_write_empty_data() {
    let mut vmo = ModelVmo::new(1);

    let written = vmo.write(0, &[]).unwrap();
    assert_eq!(written, 0);

    // Page should not be committed.
    assert!(vmo.pages[0].is_none());
}

#[test]
fn vmo_read_empty_buffer() {
    let vmo = ModelVmo::new(1);

    let mut buf = [];
    let read = vmo.read(0, &mut buf).unwrap();
    assert_eq!(read, 0);
}

// ==========================================================================
// (4) MEMORY_ALLOC/MEMORY_FREE — heap page accounting
// ==========================================================================

const HEAP_BASE: u64 = 0x0000_0000_0100_0000; // 16 MiB (from paging.rs)
const MODEL_PAGE_SIZE: u64 = 16384;
const MAX_HEAP_PAGES: u64 = 256; // model limit

/// Model of the per-process heap allocator.
/// Mirrors address_space.rs map_heap / unmap_heap accounting.
struct HeapAllocator {
    /// Tracks allocated VA ranges. Vec of (va, page_count).
    allocations: Vec<(u64, u64)>,
    /// Next VA to hand out.
    next_va: u64,
    /// Maximum pages allowed.
    max_pages: u64,
    /// Total pages currently allocated.
    used_pages: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum HeapError {
    InvalidArgument,
    OutOfMemory,
    BadAddress,
}

impl HeapAllocator {
    fn new(max_pages: u64) -> Self {
        Self {
            allocations: Vec::new(),
            next_va: HEAP_BASE,
            max_pages,
            used_pages: 0,
        }
    }

    /// Model of sys_memory_alloc. Returns the VA on success.
    fn alloc(&mut self, page_count: u64) -> Result<u64, HeapError> {
        if page_count == 0 {
            return Err(HeapError::InvalidArgument);
        }
        if self.used_pages + page_count > self.max_pages {
            return Err(HeapError::OutOfMemory);
        }

        let va = self.next_va;
        self.next_va += page_count * MODEL_PAGE_SIZE;
        self.allocations.push((va, page_count));
        self.used_pages += page_count;

        Ok(va)
    }

    /// Model of sys_memory_free. Frees the allocation at the given VA.
    fn free(&mut self, va: u64) -> Result<(), HeapError> {
        if va < HEAP_BASE {
            return Err(HeapError::InvalidArgument);
        }
        if va % MODEL_PAGE_SIZE != 0 {
            return Err(HeapError::BadAddress);
        }

        let idx = self
            .allocations
            .iter()
            .position(|&(alloc_va, _)| alloc_va == va)
            .ok_or(HeapError::InvalidArgument)?;

        let (_, page_count) = self.allocations.remove(idx);
        self.used_pages -= page_count;

        Ok(())
    }
}

#[test]
fn heap_alloc_returns_valid_va() {
    let mut heap = HeapAllocator::new(MAX_HEAP_PAGES);
    let va = heap.alloc(1).unwrap();

    assert_eq!(va, HEAP_BASE);
    assert_eq!(heap.used_pages, 1);
}

#[test]
fn heap_alloc_zero_pages_rejected() {
    let mut heap = HeapAllocator::new(MAX_HEAP_PAGES);

    assert_eq!(heap.alloc(0), Err(HeapError::InvalidArgument));
}

#[test]
fn heap_alloc_sequential_addresses() {
    let mut heap = HeapAllocator::new(MAX_HEAP_PAGES);

    let va1 = heap.alloc(2).unwrap();
    let va2 = heap.alloc(3).unwrap();

    assert_eq!(va1, HEAP_BASE);
    assert_eq!(va2, HEAP_BASE + 2 * MODEL_PAGE_SIZE);
    assert_eq!(heap.used_pages, 5);
}

#[test]
fn heap_free_valid_allocation() {
    let mut heap = HeapAllocator::new(MAX_HEAP_PAGES);

    let va = heap.alloc(4).unwrap();
    assert_eq!(heap.used_pages, 4);

    heap.free(va).unwrap();
    assert_eq!(heap.used_pages, 0);
}

#[test]
fn heap_double_free_rejected() {
    let mut heap = HeapAllocator::new(MAX_HEAP_PAGES);

    let va = heap.alloc(1).unwrap();
    heap.free(va).unwrap();

    // Second free: allocation no longer exists.
    assert_eq!(heap.free(va), Err(HeapError::InvalidArgument));
}

#[test]
fn heap_free_wrong_address_rejected() {
    let mut heap = HeapAllocator::new(MAX_HEAP_PAGES);

    heap.alloc(1).unwrap();

    // Free with an address that was never allocated.
    assert_eq!(
        heap.free(HEAP_BASE + 999 * MODEL_PAGE_SIZE),
        Err(HeapError::InvalidArgument)
    );
}

#[test]
fn heap_free_unaligned_rejected() {
    let mut heap = HeapAllocator::new(MAX_HEAP_PAGES);

    assert_eq!(heap.free(HEAP_BASE + 1), Err(HeapError::BadAddress));
}

#[test]
fn heap_free_below_heap_base_rejected() {
    let mut heap = HeapAllocator::new(MAX_HEAP_PAGES);

    assert_eq!(heap.free(0), Err(HeapError::InvalidArgument));
    assert_eq!(
        heap.free(HEAP_BASE - MODEL_PAGE_SIZE),
        Err(HeapError::InvalidArgument)
    );
}

#[test]
fn heap_alloc_beyond_limit_rejected() {
    let mut heap = HeapAllocator::new(4);

    let _va = heap.alloc(4).unwrap();

    // At capacity — next alloc fails.
    assert_eq!(heap.alloc(1), Err(HeapError::OutOfMemory));
}

#[test]
fn heap_alloc_exactly_at_limit() {
    let mut heap = HeapAllocator::new(8);

    // Allocate exactly to the limit.
    let va = heap.alloc(8).unwrap();
    assert_eq!(heap.used_pages, 8);

    // Free and re-allocate.
    heap.free(va).unwrap();
    assert_eq!(heap.used_pages, 0);

    let va2 = heap.alloc(8).unwrap();
    assert_eq!(heap.used_pages, 8);
    // VA advances (no reuse in this simple model).
    assert!(va2 > va);
}

#[test]
fn heap_alloc_after_free_succeeds() {
    let mut heap = HeapAllocator::new(2);

    let va1 = heap.alloc(2).unwrap();
    assert_eq!(heap.alloc(1), Err(HeapError::OutOfMemory));

    heap.free(va1).unwrap();

    // Now there's room again.
    let va2 = heap.alloc(1).unwrap();
    assert_eq!(heap.used_pages, 1);
    assert!(va2 > va1, "VA space advances even after free");
}

#[test]
fn heap_mixed_alloc_free_accounting() {
    let mut heap = HeapAllocator::new(MAX_HEAP_PAGES);

    let va1 = heap.alloc(10).unwrap();
    let va2 = heap.alloc(20).unwrap();
    let va3 = heap.alloc(5).unwrap();
    assert_eq!(heap.used_pages, 35);

    // Free the middle allocation.
    heap.free(va2).unwrap();
    assert_eq!(heap.used_pages, 15);

    // Free the first.
    heap.free(va1).unwrap();
    assert_eq!(heap.used_pages, 5);

    // Free the last.
    heap.free(va3).unwrap();
    assert_eq!(heap.used_pages, 0);
}

// ==========================================================================
// (5) HANDLE_CLOSE — handle lifecycle
// ==========================================================================

/// Simplified handle table model.
/// Mirrors kernel handle.rs close semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelHandleObject {
    Channel(u32),
    Timer(u32),
    Vmo(u32),
    Event(u32),
}

#[derive(Debug, PartialEq, Eq)]
enum HandleCloseError {
    InvalidHandle,
}

struct ModelHandleTable {
    slots: Vec<Option<ModelHandleObject>>,
}

/// Tracks which kernel resources were released by close.
#[derive(Debug, Default)]
struct ResourceCleanup {
    channels_closed: Vec<u32>,
    timers_destroyed: Vec<u32>,
    vmos_destroyed: Vec<u32>,
    events_destroyed: Vec<u32>,
}

impl ModelHandleTable {
    fn new(capacity: usize) -> Self {
        Self {
            slots: vec![None; capacity],
        }
    }

    /// Insert a handle object. Returns the handle index.
    fn insert(&mut self, obj: ModelHandleObject) -> Result<u16, HandleCloseError> {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(obj);
                return Ok(i as u16);
            }
        }
        Err(HandleCloseError::InvalidHandle)
    }

    /// Close a handle. Returns the object that was in the slot.
    /// Mirrors kernel HandleTable::close.
    fn close(&mut self, handle: u16) -> Result<ModelHandleObject, HandleCloseError> {
        let slot = self
            .slots
            .get_mut(handle as usize)
            .ok_or(HandleCloseError::InvalidHandle)?;
        let obj = slot.ok_or(HandleCloseError::InvalidHandle)?;

        *slot = None;

        Ok(obj)
    }

    /// Count of occupied slots.
    fn count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

/// Model of sys_handle_close: close the slot, then release kernel resources.
fn model_handle_close(
    table: &mut ModelHandleTable,
    handle: u16,
    cleanup: &mut ResourceCleanup,
) -> Result<(), HandleCloseError> {
    // Validate handle range (kernel checks handle_nr > u16::MAX, but
    // we already have a u16 here).
    let obj = table.close(handle)?;

    // Release kernel resources associated with the closed handle.
    match obj {
        ModelHandleObject::Channel(id) => cleanup.channels_closed.push(id),
        ModelHandleObject::Timer(id) => cleanup.timers_destroyed.push(id),
        ModelHandleObject::Vmo(id) => cleanup.vmos_destroyed.push(id),
        ModelHandleObject::Event(id) => cleanup.events_destroyed.push(id),
    }

    Ok(())
}

#[test]
fn handle_close_valid_handle_succeeds() {
    let mut table = ModelHandleTable::new(256);
    let mut cleanup = ResourceCleanup::default();

    let h = table.insert(ModelHandleObject::Channel(42)).unwrap();
    assert_eq!(table.count(), 1);

    model_handle_close(&mut table, h, &mut cleanup).unwrap();

    assert_eq!(table.count(), 0);
    assert_eq!(cleanup.channels_closed, vec![42]);
}

#[test]
fn handle_close_removes_from_table() {
    let mut table = ModelHandleTable::new(256);
    let mut cleanup = ResourceCleanup::default();

    let h1 = table.insert(ModelHandleObject::Timer(1)).unwrap();
    let h2 = table.insert(ModelHandleObject::Timer(2)).unwrap();
    let h3 = table.insert(ModelHandleObject::Timer(3)).unwrap();
    assert_eq!(table.count(), 3);

    // Close the middle one.
    model_handle_close(&mut table, h2, &mut cleanup).unwrap();
    assert_eq!(table.count(), 2);
    assert_eq!(cleanup.timers_destroyed, vec![2]);

    // The other two should still be accessible.
    assert!(table.slots[h1 as usize].is_some());
    assert!(table.slots[h3 as usize].is_some());
}

#[test]
fn handle_close_already_closed_returns_error() {
    let mut table = ModelHandleTable::new(256);
    let mut cleanup = ResourceCleanup::default();

    let h = table.insert(ModelHandleObject::Vmo(10)).unwrap();

    model_handle_close(&mut table, h, &mut cleanup).unwrap();

    // Double-close: slot is now empty.
    let err = model_handle_close(&mut table, h, &mut cleanup);
    assert_eq!(err, Err(HandleCloseError::InvalidHandle));

    // Cleanup should only have been called once.
    assert_eq!(cleanup.vmos_destroyed, vec![10]);
}

#[test]
fn handle_close_out_of_range_returns_error() {
    let mut table = ModelHandleTable::new(256);
    let mut cleanup = ResourceCleanup::default();

    // Handle beyond table capacity.
    let err = model_handle_close(&mut table, 300, &mut cleanup);
    assert_eq!(err, Err(HandleCloseError::InvalidHandle));
}

#[test]
fn handle_close_empty_slot_returns_error() {
    let mut table = ModelHandleTable::new(256);
    let mut cleanup = ResourceCleanup::default();

    // Slot 0 exists but is empty (no handle was inserted at index 0...
    // well, first insert goes to 0, so test slot 5 which is empty).
    let err = model_handle_close(&mut table, 5, &mut cleanup);
    assert_eq!(err, Err(HandleCloseError::InvalidHandle));
}

#[test]
fn handle_close_dispatches_correct_resource_cleanup() {
    let mut table = ModelHandleTable::new(256);
    let mut cleanup = ResourceCleanup::default();

    let h_ch = table.insert(ModelHandleObject::Channel(1)).unwrap();
    let h_timer = table.insert(ModelHandleObject::Timer(2)).unwrap();
    let h_vmo = table.insert(ModelHandleObject::Vmo(3)).unwrap();
    let h_event = table.insert(ModelHandleObject::Event(4)).unwrap();

    model_handle_close(&mut table, h_ch, &mut cleanup).unwrap();
    model_handle_close(&mut table, h_timer, &mut cleanup).unwrap();
    model_handle_close(&mut table, h_vmo, &mut cleanup).unwrap();
    model_handle_close(&mut table, h_event, &mut cleanup).unwrap();

    assert_eq!(cleanup.channels_closed, vec![1]);
    assert_eq!(cleanup.timers_destroyed, vec![2]);
    assert_eq!(cleanup.vmos_destroyed, vec![3]);
    assert_eq!(cleanup.events_destroyed, vec![4]);
}

#[test]
fn handle_close_slot_can_be_reused() {
    let mut table = ModelHandleTable::new(4);
    let mut cleanup = ResourceCleanup::default();

    // Fill all slots.
    let h0 = table.insert(ModelHandleObject::Channel(0)).unwrap();
    let _h1 = table.insert(ModelHandleObject::Channel(1)).unwrap();
    let _h2 = table.insert(ModelHandleObject::Channel(2)).unwrap();
    let _h3 = table.insert(ModelHandleObject::Channel(3)).unwrap();
    assert_eq!(table.count(), 4);

    // Table is full — insert fails.
    assert!(table.insert(ModelHandleObject::Channel(99)).is_err());

    // Close slot 0.
    model_handle_close(&mut table, h0, &mut cleanup).unwrap();
    assert_eq!(table.count(), 3);

    // Now insert should succeed, reusing slot 0.
    let h_new = table.insert(ModelHandleObject::Timer(99)).unwrap();
    assert_eq!(h_new, 0, "reuses freed slot 0");
    assert_eq!(table.count(), 4);
}

#[test]
fn handle_close_does_not_affect_other_handles() {
    let mut table = ModelHandleTable::new(256);
    let mut cleanup = ResourceCleanup::default();

    let h0 = table.insert(ModelHandleObject::Channel(10)).unwrap();
    let h1 = table.insert(ModelHandleObject::Timer(20)).unwrap();

    model_handle_close(&mut table, h0, &mut cleanup).unwrap();

    // h1 should still work.
    let obj = table.close(h1).unwrap();
    assert_eq!(obj, ModelHandleObject::Timer(20));
}

#[test]
fn handle_close_sequential_all_handles() {
    let mut table = ModelHandleTable::new(256);
    let mut cleanup = ResourceCleanup::default();

    let handles: Vec<u16> = (0..10)
        .map(|i| table.insert(ModelHandleObject::Channel(i)).unwrap())
        .collect();

    assert_eq!(table.count(), 10);

    // Close all in reverse order.
    for &h in handles.iter().rev() {
        model_handle_close(&mut table, h, &mut cleanup).unwrap();
    }

    assert_eq!(table.count(), 0);
    assert_eq!(cleanup.channels_closed.len(), 10);

    // Verify cleanup order matches close order (reverse of insertion).
    let expected: Vec<u32> = (0..10).rev().collect();
    assert_eq!(cleanup.channels_closed, expected);
}
