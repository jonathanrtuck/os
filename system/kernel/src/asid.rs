//! 8-bit ASID allocator.
//!
//! Allocates ASIDs from 1..255. ASID 0 is reserved (kernel / idle TTBR0).
//! Freed ASIDs are recycled via a small stack.

use super::sync::IrqMutex;

#[derive(Clone, Copy)]
pub struct Asid(pub u8);

struct State {
    next: u8,
    freed: [u8; 255],
    freed_count: usize,
}

static STATE: IrqMutex<State> = IrqMutex::new(State {
    next: 1,
    freed: [0; 255],
    freed_count: 0,
});

/// Allocate the next ASID. Reuses freed ASIDs first. Panics if all 255 are exhausted.
pub fn alloc() -> Asid {
    let mut s = STATE.lock();

    if s.freed_count > 0 {
        s.freed_count -= 1;

        return Asid(s.freed[s.freed_count]);
    }

    let id = s.next;

    assert!(id != 0, "ASID pool exhausted");

    s.next = s.next.wrapping_add(1);

    Asid(id)
}
/// Return an ASID to the pool for reuse.
pub fn free(asid: Asid) {
    let mut s = STATE.lock();
    let idx = s.freed_count;

    s.freed[idx] = asid.0;
    s.freed_count += 1;
}
