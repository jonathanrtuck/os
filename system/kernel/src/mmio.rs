//! Memory-mapped I/O helpers.
//!
//! Centralizes all volatile hardware access so `unsafe` lives in one place.
//! Every other module goes through these instead of raw pointer casts.

#[inline(always)]
pub fn read8(addr: usize) -> u8 {
    unsafe { core::ptr::read_volatile(addr as *const u8) }
}
#[inline(always)]
pub fn read32(addr: usize) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}
#[inline(always)]
pub fn write8(addr: usize, val: u8) {
    unsafe { core::ptr::write_volatile(addr as *mut u8, val) }
}
#[inline(always)]
pub fn write32(addr: usize, val: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

/// Clean and invalidate data cache by VA to Point of Coherency.
///
/// Used for DMA: clean before device reads (flush dirty data to RAM),
/// invalidate after device writes (discard stale cache lines).
/// ARM caches are not coherent with DMA by default.
#[inline(always)]
pub fn dc_civac(va: usize) {
    unsafe {
        core::arch::asm!(
            "dc civac, {va}",
            va = in(reg) va,
            options(nostack)
        );
    }
}

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

    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}
