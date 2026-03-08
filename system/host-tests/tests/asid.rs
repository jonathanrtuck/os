//! Host-side tests for the ASID allocation algorithm.
//!
//! Duplicates the bitmap allocation logic from kernel address_space_id.rs (the
//! kernel version uses inline asm for TLB flushes, which can't run
//! on the host). Tests verify: allocation range, uniqueness, exhaustion
//! + rollover, generation monotonicity, and free-and-reuse.

const MAX_ASID: u8 = 255;

/// Simplified ASID allocator mirroring kernel logic (no TLB flush).
struct AsidAllocator {
    bitmap: [u64; 4],
    generation: u64,
    next_hint: u8,
}

impl AsidAllocator {
    fn new() -> Self {
        Self {
            bitmap: [0; 4],
            generation: 0,
            next_hint: 1,
        }
    }

    /// Allocate an ASID. Returns (asid, generation).
    fn alloc(&mut self) -> (u8, u64) {
        // Mark ASID 0 as always in-use.
        self.bitmap[0] |= 1;

        let start = self.next_hint;

        for offset in 0..255u16 {
            let id = ((start as u16 - 1 + offset) % 255 + 1) as u8;
            let word = (id / 64) as usize;
            let bit = id % 64;

            if self.bitmap[word] & (1u64 << bit) == 0 {
                self.bitmap[word] |= 1u64 << bit;
                self.next_hint = id.wrapping_add(1);

                if self.next_hint == 0 {
                    self.next_hint = 1;
                }

                return (id, self.generation);
            }
        }

        // Rollover — flush TLB (no-op on host) and start new generation.
        self.generation += 1;
        self.bitmap = [0; 4];
        self.bitmap[0] |= 1; // Re-reserve ASID 0.
        self.bitmap[0] |= 1u64 << 1;
        self.next_hint = 2;

        (1, self.generation)
    }

    /// Free an ASID.
    fn free(&mut self, asid: u8) {
        if asid == 0 {
            return;
        }

        let word = (asid / 64) as usize;
        let bit = asid % 64;

        self.bitmap[word] &= !(1u64 << bit);
    }
}

// --- Tests ---

#[test]
fn alloc_returns_nonzero() {
    let mut a = AsidAllocator::new();
    let (asid, _) = a.alloc();

    assert_ne!(asid, 0, "ASID 0 is reserved for kernel");
}

#[test]
fn alloc_sequential_unique() {
    let mut a = AsidAllocator::new();
    let mut seen = std::collections::HashSet::new();

    for _ in 0..10 {
        let (asid, _) = a.alloc();

        assert!(seen.insert(asid), "duplicate ASID {asid}");
        assert!(asid >= 1 && asid <= MAX_ASID, "ASID out of range: {asid}");
    }
}

#[test]
fn alloc_255_then_rollover() {
    let mut a = AsidAllocator::new();
    let mut asids = Vec::new();

    // Allocate all 255 ASIDs (1..=255).
    for i in 0..255 {
        let (asid, gen) = a.alloc();

        assert_eq!(gen, 0, "first 255 should be generation 0 (i={i})");
        assert!(asid >= 1 && asid <= MAX_ASID);

        asids.push(asid);
    }

    // All 255 should be unique.
    let unique: std::collections::HashSet<u8> = asids.iter().copied().collect();

    assert_eq!(unique.len(), 255, "all 255 ASIDs should be unique");

    // Next allocation triggers rollover.
    let (asid, gen) = a.alloc();

    assert_eq!(gen, 1, "should be generation 1 after rollover");
    assert_eq!(asid, 1, "first ASID after rollover should be 1");
}

#[test]
fn generation_monotonic() {
    let mut a = AsidAllocator::new();
    let mut last_gen = 0u64;

    // Two full rounds + partial.
    for _ in 0..600 {
        let (_, gen) = a.alloc();

        assert!(
            gen >= last_gen,
            "generation must be monotonically non-decreasing"
        );

        last_gen = gen;
    }

    // After 600 allocations: ceil(600/255) = 3 generations (0, 1, 2).
    assert!(last_gen >= 2, "should have rolled over at least twice");
}

#[test]
fn free_and_reuse() {
    let mut a = AsidAllocator::new();

    let (asid1, _) = a.alloc();
    let (asid2, _) = a.alloc();

    a.free(asid1);

    // The freed ASID should eventually be reallocated.
    // 254 free IDs remain (255 minus asid2). The freed asid1 may be last
    // in scan order if the hint has moved past it.
    let mut found = false;

    for _ in 0..254 {
        let (asid, _) = a.alloc();

        if asid == asid1 {
            found = true;

            break;
        }
    }

    assert!(found, "freed ASID {asid1} should be reused before rollover");

    // asid2 should still be allocated (not double-used).
    let word = (asid2 / 64) as usize;
    let bit = asid2 % 64;

    assert!(
        a.bitmap[word] & (1u64 << bit) != 0,
        "asid2 should still be marked allocated"
    );
}

#[test]
fn free_zero_is_noop() {
    let mut a = AsidAllocator::new();

    a.free(0);

    // ASID 0 should still be reserved.
    assert!(a.bitmap[0] & 1 == 0, "ASID 0 reservation only set on alloc");

    // After first alloc, verify 0 stays reserved.
    let _ = a.alloc();

    a.free(0);

    assert!(a.bitmap[0] & 1 != 0, "ASID 0 should remain reserved");
}

#[test]
fn double_rollover() {
    let mut a = AsidAllocator::new();

    // 3 full rounds = 765 allocations.
    for _ in 0..765 {
        let _ = a.alloc();
    }

    let (_, gen) = a.alloc();

    assert_eq!(gen, 3, "should be generation 3 after 766 allocations");
}
