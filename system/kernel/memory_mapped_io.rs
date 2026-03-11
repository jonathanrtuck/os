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
    debug_assert!(
        addr.is_multiple_of(4),
        "write32: addr must be 4-byte aligned"
    );

    // SAFETY: Volatile write to a 32-bit MMIO register. The debug_assert above
    // guards alignment in debug builds. All callers pass hardware register
    // addresses (GIC, UART, virtio-mmio) which are architecturally 4-byte
    // aligned. Caller must ensure `addr` is a valid, mapped MMIO address.
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}
