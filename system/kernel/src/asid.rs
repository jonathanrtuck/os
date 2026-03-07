//! 8-bit ASID allocator.
//!
//! Allocates ASIDs from 1..255. ASID 0 is reserved (kernel / idle TTBR0).
//! Freed ASIDs are recycled via a small stack.

#[derive(Clone, Copy)]
pub struct Asid(pub u8);

static mut NEXT: u8 = 1;
static mut FREED: [u8; 255] = [0; 255];
static mut FREED_COUNT: usize = 0;

/// Allocate the next ASID. Reuses freed ASIDs first. Panics if all 255 are exhausted.
pub fn alloc() -> Asid {
    unsafe {
        if FREED_COUNT > 0 {
            FREED_COUNT -= 1;

            return Asid(FREED[FREED_COUNT]);
        }

        let id = NEXT;

        assert!(id != 0, "ASID pool exhausted");

        NEXT = NEXT.wrapping_add(1);

        Asid(id)
    }
}
/// Return an ASID to the pool for reuse.
pub fn free(asid: Asid) {
    unsafe {
        FREED[FREED_COUNT] = asid.0;
        FREED_COUNT += 1;
    }
}
