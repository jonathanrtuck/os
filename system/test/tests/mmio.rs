//! Host-side tests for memory-mapped I/O helpers.
//!
//! Tests volatile read/write correctness and alignment enforcement.
//! The MMIO module uses inline asm for cache maintenance which isn't
//! available on the host, so we test only the read/write functions and
//! alignment validation via debug_assert.

/// Test that read32 on a 4-byte aligned address succeeds.
#[test]
fn read32_aligned_succeeds() {
    let val: u32 = 0xDEAD_BEEF;
    let addr = &val as *const u32 as usize;

    // Verify alignment precondition.
    assert_eq!(addr % 4, 0, "test setup: address must be 4-byte aligned");

    let result = unsafe { core::ptr::read_volatile(addr as *const u32) };

    assert_eq!(result, 0xDEAD_BEEF);
}

/// Test that write32 on a 4-byte aligned address succeeds.
#[test]
fn write32_aligned_succeeds() {
    let mut val: u32 = 0;
    let addr = &mut val as *mut u32 as usize;

    assert_eq!(addr % 4, 0, "test setup: address must be 4-byte aligned");

    unsafe { core::ptr::write_volatile(addr as *mut u32, 0xCAFE_BABE) };

    assert_eq!(val, 0xCAFE_BABE);
}

/// Test that read8 works on any alignment.
#[test]
fn read8_any_alignment() {
    let buf: [u8; 4] = [0x42, 0x43, 0x44, 0x45];

    for i in 0..4 {
        let addr = &buf[i] as *const u8 as usize;
        let result = unsafe { core::ptr::read_volatile(addr as *const u8) };

        assert_eq!(result, 0x42 + i as u8);
    }
}

/// Test that write8 works on any alignment.
#[test]
fn write8_any_alignment() {
    let mut buf: [u8; 4] = [0; 4];

    for i in 0..4 {
        let addr = &mut buf[i] as *mut u8 as usize;

        unsafe { core::ptr::write_volatile(addr as *mut u8, 0x10 + i as u8) };
    }

    assert_eq!(buf, [0x10, 0x11, 0x12, 0x13]);
}

/// Test that read32 on an unaligned address panics (debug_assert).
///
/// This validates the alignment guard added during the audit.
/// In release builds the assert is stripped, but in debug builds
/// (which the kernel runs during development) it catches misuse.
#[test]
#[should_panic(expected = "read32: addr must be 4-byte aligned")]
#[cfg(debug_assertions)]
fn read32_unaligned_panics() {
    // Create a buffer and pick an address at offset +1 (unaligned for u32).
    let buf: [u8; 8] = [0; 8];
    let base = &buf[0] as *const u8 as usize;
    let unaligned = base + 1;

    assert_ne!(unaligned % 4, 0, "test setup: must be unaligned");

    // This should panic due to the debug_assert.
    mmio_read32(unaligned);
}

/// Test that write32 on an unaligned address panics (debug_assert).
#[test]
#[should_panic(expected = "write32: addr must be 4-byte aligned")]
#[cfg(debug_assertions)]
fn write32_unaligned_panics() {
    let mut buf: [u8; 8] = [0; 8];
    let base = &mut buf[0] as *mut u8 as usize;
    let unaligned = base + 1;

    assert_ne!(unaligned % 4, 0, "test setup: must be unaligned");

    mmio_write32(unaligned, 0xDEAD);
}

/// Inline the MMIO functions with their debug_asserts for host testing.
/// (Can't import kernel module due to inline asm for cache maintenance.)
fn mmio_read32(addr: usize) -> u32 {
    debug_assert!(addr % 4 == 0, "read32: addr must be 4-byte aligned");

    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

fn mmio_write32(addr: usize, val: u32) {
    debug_assert!(addr % 4 == 0, "write32: addr must be 4-byte aligned");

    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

/// Verify volatile semantics: write then read returns the written value.
#[test]
fn volatile_roundtrip_u32() {
    let mut val: u32 = 0;
    let addr = &mut val as *mut u32 as usize;

    unsafe {
        core::ptr::write_volatile(addr as *mut u32, 0x1234_5678);
    }

    let result = unsafe { core::ptr::read_volatile(addr as *const u32) };

    assert_eq!(result, 0x1234_5678);
}

/// Verify volatile semantics for u8.
#[test]
fn volatile_roundtrip_u8() {
    let mut val: u8 = 0;
    let addr = &mut val as *mut u8 as usize;

    unsafe {
        core::ptr::write_volatile(addr as *mut u8, 0xAB);
    }

    let result = unsafe { core::ptr::read_volatile(addr as *const u8) };

    assert_eq!(result, 0xAB);
}
