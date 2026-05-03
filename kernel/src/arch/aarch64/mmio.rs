//! Memory-mapped I/O helpers.
//!
//! Centralizes all volatile hardware access so `unsafe` lives in one place.
//! Every other module goes through these instead of raw pointer casts.

#[inline(always)]
pub fn read32(addr: usize) -> u32 {
    debug_assert!(
        addr.is_multiple_of(4),
        "read32: addr must be 4-byte aligned"
    );

    // SAFETY: Caller must ensure `addr` is a valid MMIO register. Volatile
    // read prevents the compiler from optimizing away or reordering the access.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

#[inline(always)]
pub fn write8(addr: usize, val: u8) {
    // SAFETY: Caller must ensure `addr` is a valid MMIO register.
    unsafe { core::ptr::write_volatile(addr as *mut u8, val) }
}

#[inline(always)]
pub fn write32(addr: usize, val: u32) {
    debug_assert!(
        addr.is_multiple_of(4),
        "write32: addr must be 4-byte aligned"
    );

    // SAFETY: Caller must ensure `addr` is a valid MMIO register.
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}
