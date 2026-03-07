//! 8-bit ASID allocator.
//!
//! Allocates ASIDs linearly from 1..255. ASID 0 is reserved (used by the
//! kernel / idle TTBR0). No recycling — 255 user address spaces is plenty
//! for this spike.

#[derive(Clone, Copy)]
pub struct Asid(pub u8);

static mut NEXT: u8 = 1;

/// Allocate the next ASID. Panics if all 255 are exhausted.
pub fn alloc() -> Asid {
    unsafe {
        let id = NEXT;

        assert!(id != 0, "ASID pool exhausted");

        NEXT = NEXT.wrapping_add(1);

        Asid(id)
    }
}
