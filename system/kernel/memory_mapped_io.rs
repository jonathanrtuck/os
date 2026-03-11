//! Memory-mapped I/O helpers.
//!
//! Centralizes all volatile hardware access so `unsafe` lives in one place.
//! Every other module goes through these instead of raw pointer casts.

/// Clean and invalidate a range of cache lines covering `[va, va+len)`.
///
/// Cache line size is 64 bytes on all Cortex-A cores QEMU emulates.
/// Issues DSB after the loop to ensure all maintenance completes.
pub fn cache_clean_invalidate_range(va: usize, len: usize) {
    const CACHE_LINE: usize = 64;

    let start = va & !(CACHE_LINE - 1);
    let end = va + len;
    let mut addr = start;

    while addr < end {
        dc_civac(addr);
        addr += CACHE_LINE;
    }

    // SAFETY: DSB SY is a data synchronization barrier instruction with no
    // memory operands. It ensures all preceding cache maintenance (DC CIVAC)
    // operations complete before any subsequent memory access. `nostack`
    // is correct — DSB does not touch the stack.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

/// Clean and invalidate data cache by VA to Point of Coherency.
///
/// Used for DMA: clean before device reads (flush dirty data to RAM),
/// invalidate after device writes (discard stale cache lines).
/// ARM caches are not coherent with DMA by default.
#[inline(always)]
pub fn dc_civac(va: usize) {
    // SAFETY: DC CIVAC is a cache maintenance instruction that operates on the
    // cache line containing the virtual address `va`. The address must be in
    // mapped memory (kernel VA space, established by boot.S and memory::init).
    // `nostack` is correct — DC does not touch the stack. The `in(reg)`
    // constraint correctly passes `va` without clobbering other registers.
    unsafe {
        core::arch::asm!(
            "dc civac, {va}",
            va = in(reg) va,
            options(nostack)
        );
    }
}
#[inline(always)]
pub fn read8(addr: usize) -> u8 {
    // SAFETY: Volatile read of a single byte. No alignment requirement for u8.
    // Caller must ensure `addr` is a valid, mapped MMIO register address
    // (device memory mapped via boot.S identity map or address_space::map_device_mmio).
    unsafe { core::ptr::read_volatile(addr as *const u8) }
}
#[inline(always)]
pub fn read32(addr: usize) -> u32 {
    debug_assert!(addr % 4 == 0, "read32: addr must be 4-byte aligned");

    // SAFETY: Volatile read of a 32-bit MMIO register. The debug_assert above
    // guards alignment in debug builds. All callers pass hardware register
    // addresses (GIC, UART, virtio-mmio) which are architecturally 4-byte
    // aligned. Caller must ensure `addr` is a valid, mapped MMIO address.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}
#[inline(always)]
pub fn write8(addr: usize, val: u8) {
    // SAFETY: Volatile write of a single byte. No alignment requirement for u8.
    // Caller must ensure `addr` is a valid, mapped MMIO register address.
    unsafe { core::ptr::write_volatile(addr as *mut u8, val) }
}
#[inline(always)]
pub fn write32(addr: usize, val: u32) {
    debug_assert!(addr % 4 == 0, "write32: addr must be 4-byte aligned");

    // SAFETY: Volatile write to a 32-bit MMIO register. The debug_assert above
    // guards alignment in debug builds. All callers pass hardware register
    // addresses (GIC, UART, virtio-mmio) which are architecturally 4-byte
    // aligned. Caller must ensure `addr` is a valid, mapped MMIO address.
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}
