//! CPU context saved/restored across exception boundaries.
//!
//! Layout must match the CTX_* offsets in exception.S exactly.
//! The const assertions below enforce this at compile time.

/// CPU register state saved on exception entry and restored on return.
///
/// Each thread embeds a `Context` at offset 0. `TPIDR_EL1` always points
/// at the current thread, and exception.S saves/restores registers at the
/// offsets defined here.
#[repr(C)]
pub struct Context {
    pub x: [u64; 31], // x0..x30
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
    pub sp_el0: u64,
    pub tpidr_el0: u64,
    pub q: [u128; 32],
    pub fpcr: u64,
    pub fpsr: u64,
}

const _: () = {
    assert!(core::mem::offset_of!(Context, x) == 0x000);
    assert!(core::mem::offset_of!(Context, sp) == 0x0F8);
    assert!(core::mem::offset_of!(Context, elr) == 0x100);
    assert!(core::mem::offset_of!(Context, spsr) == 0x108);
    assert!(core::mem::offset_of!(Context, sp_el0) == 0x110);
    assert!(core::mem::offset_of!(Context, tpidr_el0) == 0x118);
    assert!(core::mem::offset_of!(Context, q) == 0x120);
    assert!(core::mem::offset_of!(Context, fpcr) == 0x320);
    assert!(core::mem::offset_of!(Context, fpsr) == 0x328);
    assert!(core::mem::size_of::<Context>() == 0x330);
};
