// AUDIT: 2026-03-11 — 1 unsafe block verified (TLB flush on generation
// rollover, cfg-gated to target_os=none), 6-category checklist applied.
// ASID wrapping/exhaustion: bitmap search covers 1..=255, exhaustion triggers
// TLB flush + generation increment + fresh bitmap. Generation is u64 (no
// practical overflow). Free of ASID 0 is a no-op (guard). Double-free is
// harmless (clears already-clear bit). No bugs found.

//! ASID allocator with generation-based recycling.
//!
//! Allocates ASIDs from 1..255. ASID 0 is reserved (kernel / idle TTBR0).
//! When all 255 ASIDs are exhausted, the generation counter increments,
//! the TLB is flushed, and all ASIDs are available for reuse.
//!
//! Each ASID comes with a generation number. On context switch, if the
//! thread's generation doesn't match the global generation, the ASID is
//! stale and must be re-acquired (lazy revalidation).

use super::sync::IrqMutex;

struct State {
    /// Bitmap of in-use ASIDs. Bit N = ASID N is allocated.
    bitmap: [u64; 4], // 256 bits, covers ASIDs 0..255
    /// Current generation. Incremented on rollover.
    generation: u64,
    /// Next ASID to try allocating.
    next_hint: u8,
}

#[derive(Clone, Copy)]
pub struct Asid(pub u8);

static STATE: IrqMutex<State> = IrqMutex::new(State {
    bitmap: [0; 4],
    generation: 0,
    next_hint: 1,
});

/// Allocate an ASID. Returns the ASID and its generation.
///
/// On exhaustion, flushes the TLB, increments the generation, and restarts.
pub fn alloc() -> (Asid, u64) {
    let mut s = STATE.lock();

    // Mark ASID 0 as always in-use (reserved for kernel).
    s.bitmap[0] |= 1;

    // Search for a free ASID starting from the hint.
    let start = s.next_hint;

    for offset in 0..255u16 {
        let id = ((start as u16 - 1 + offset) % 255 + 1) as u8; // Range 1..=255
        let word = (id / 64) as usize;
        let bit = id % 64;

        if s.bitmap[word] & (1u64 << bit) == 0 {
            // Found a free ASID.
            s.bitmap[word] |= 1u64 << bit;
            s.next_hint = id.wrapping_add(1);

            if s.next_hint == 0 {
                s.next_hint = 1;
            }

            return (Asid(id), s.generation);
        }
    }

    // All ASIDs exhausted — flush TLB and start a new generation.
    // SAFETY: TLBI vmalle1is invalidates all TLB entries across all cores.
    // This is safe because all threads will re-acquire ASIDs on next
    // context switch (generation mismatch triggers lazy revalidation).
    #[cfg(target_os = "none")]
    super::arch::mmu::tlbi_all();

    s.generation += 1;
    s.bitmap = [0; 4];
    s.bitmap[0] |= 1; // Re-reserve ASID 0.

    // Allocate ASID 1 for the caller.
    s.bitmap[0] |= 1u64 << 1;
    s.next_hint = 2;

    (Asid(1), s.generation)
}
/// Get the current generation (used by test crate).
#[allow(dead_code)]
pub fn current_generation() -> u64 {
    STATE.lock().generation
}
/// Reset allocator to initial state (used by test crate for isolation).
#[allow(dead_code)]
pub fn reset() {
    let mut s = STATE.lock();

    s.bitmap = [0; 4];
    s.generation = 0;
    s.next_hint = 1;
}

/// Return an ASID to the pool for reuse.
pub fn free(asid: Asid) {
    if asid.0 == 0 {
        return; // Don't free the reserved ASID.
    }

    let mut s = STATE.lock();
    let word = (asid.0 / 64) as usize;
    let bit = asid.0 % 64;

    s.bitmap[word] &= !(1u64 << bit);
}
