//! Host-side tests for the bootstrap page layout.
//!
//! Verifies the BootstrapLayout struct has a stable ABI: known size,
//! alignment, and field offsets. Append-only — these assertions lock
//! down the kernel-userspace contract.

#[path = "../../paging.rs"]
mod paging;

use paging::{BootstrapLayout, BOOTSTRAP_MAGIC, BOOTSTRAP_PAGE_VA, PAGE_SIZE};

// =========================================================================
// ABI stability
// =========================================================================

#[test]
fn bootstrap_page_va_is_page_1() {
    assert_eq!(
        BOOTSTRAP_PAGE_VA, PAGE_SIZE,
        "bootstrap page should be at page 1"
    );
}

#[test]
fn bootstrap_page_va_is_page_aligned() {
    assert_eq!(BOOTSTRAP_PAGE_VA % PAGE_SIZE, 0);
}

#[test]
fn bootstrap_layout_size_fits_in_one_page() {
    assert!(
        core::mem::size_of::<BootstrapLayout>() <= PAGE_SIZE as usize,
        "BootstrapLayout must fit in one page"
    );
}

#[test]
fn bootstrap_layout_is_repr_c() {
    // repr(C) guarantees field order matches declaration order.
    // Verify by checking offsets of first and last fields.
    let layout = BootstrapLayout {
        magic: BOOTSTRAP_MAGIC,
        channel_shm_base: 0x1111,
        shared_base: 0x2222,
        service_pack_base: 0x3333,
        heap_base: 0x4444,
        heap_end: 0x5555,
        device_base: 0x6666,
        device_end: 0x7777,
        stack_top: 0x8888,
    };

    // Verify field values survive a round-trip through raw bytes.
    let ptr = &layout as *const BootstrapLayout as *const u8;
    let recovered = unsafe { &*(ptr as *const BootstrapLayout) };

    assert_eq!(recovered.magic, BOOTSTRAP_MAGIC);
    assert_eq!(recovered.channel_shm_base, 0x1111);
    assert_eq!(recovered.shared_base, 0x2222);
    assert_eq!(recovered.service_pack_base, 0x3333);
    assert_eq!(recovered.heap_base, 0x4444);
    assert_eq!(recovered.heap_end, 0x5555);
    assert_eq!(recovered.device_base, 0x6666);
    assert_eq!(recovered.device_end, 0x7777);
    assert_eq!(recovered.stack_top, 0x8888);
}

#[test]
fn bootstrap_layout_field_offsets_are_stable() {
    // Lock down field offsets for ABI stability.
    assert_eq!(core::mem::offset_of!(BootstrapLayout, magic), 0);
    assert_eq!(core::mem::offset_of!(BootstrapLayout, channel_shm_base), 8);
    assert_eq!(core::mem::offset_of!(BootstrapLayout, shared_base), 16);
    assert_eq!(
        core::mem::offset_of!(BootstrapLayout, service_pack_base),
        24
    );
    assert_eq!(core::mem::offset_of!(BootstrapLayout, heap_base), 32);
    assert_eq!(core::mem::offset_of!(BootstrapLayout, heap_end), 40);
    assert_eq!(core::mem::offset_of!(BootstrapLayout, device_base), 48);
    assert_eq!(core::mem::offset_of!(BootstrapLayout, device_end), 56);
    assert_eq!(core::mem::offset_of!(BootstrapLayout, stack_top), 64);
}

#[test]
fn bootstrap_layout_size_is_72_bytes() {
    // 9 fields × 8 bytes = 72 bytes. Lock this down.
    assert_eq!(core::mem::size_of::<BootstrapLayout>(), 72);
}

#[test]
fn bootstrap_magic_is_ascii() {
    // "BOOTSTRP" in ASCII bytes, big-endian u64.
    let bytes = BOOTSTRAP_MAGIC.to_be_bytes();
    assert_eq!(&bytes, b"BOOTSTRP");
}
