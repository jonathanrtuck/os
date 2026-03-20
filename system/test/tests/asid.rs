//! Host-side tests for the ASID allocation algorithm.
//!
//! Includes the kernel's address_space_id.rs directly via #[path]. The TLB
//! flush asm is gated with #[cfg(target_os = "none")] so it compiles away
//! on the host. IrqMutex is stubbed with a simple UnsafeCell wrapper.

mod sync {
    use core::{
        cell::UnsafeCell,
        ops::{Deref, DerefMut},
    };

    pub struct IrqGuard<'a, T> {
        data: &'a UnsafeCell<T>,
    }
    pub struct IrqMutex<T> {
        data: UnsafeCell<T>,
    }

    impl<T> Deref for IrqGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            unsafe { &*self.data.get() }
        }
    }
    impl<T> DerefMut for IrqGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            unsafe { &mut *self.data.get() }
        }
    }
    impl<T> IrqMutex<T> {
        pub fn lock(&self) -> IrqGuard<'_, T> {
            IrqGuard { data: &self.data }
        }
        pub const fn new(val: T) -> Self {
            Self {
                data: UnsafeCell::new(val),
            }
        }
    }
    // SAFETY: Single-threaded host tests.
    unsafe impl<T> Sync for IrqMutex<T> {}
}

#[path = "../../kernel/address_space_id.rs"]
mod address_space_id;

use address_space_id::{alloc, current_generation, free, Asid};

const MAX_ASID: u8 = 255;

// --- Tests ---

#[test]
fn alloc_returns_nonzero() {
    let (asid, _) = alloc();

    assert_ne!(asid.0, 0, "ASID 0 is reserved for kernel");
}

#[test]
fn alloc_sequential_unique() {
    let mut seen = std::collections::HashSet::new();

    for _ in 0..10 {
        let (asid, _) = alloc();

        assert!(seen.insert(asid.0), "duplicate ASID {}", asid.0);
        assert!(
            asid.0 >= 1 && asid.0 <= MAX_ASID,
            "ASID out of range: {}",
            asid.0
        );
    }
}

#[test]
fn alloc_255_then_rollover() {
    // Record generation before filling.
    let gen_before = current_generation();

    let mut asids = Vec::new();

    // Allocate until rollover. We may already have some ASIDs consumed by
    // earlier tests (global state), so allocate until generation changes.
    for _ in 0..512 {
        let (asid, gen) = alloc();

        if gen > gen_before {
            // Rollover happened.
            assert_eq!(asid.0, 1, "first ASID after rollover should be 1");
            return;
        }

        asids.push(asid.0);
    }

    panic!("expected rollover within 512 allocations");
}

#[test]
fn generation_monotonic() {
    let mut last_gen = current_generation();

    for _ in 0..600 {
        let (_, gen) = alloc();

        assert!(
            gen >= last_gen,
            "generation must be monotonically non-decreasing"
        );

        last_gen = gen;
    }
}

#[test]
fn free_and_reuse() {
    let gen_start = current_generation();
    let (asid1, _) = alloc();
    let (_asid2, _) = alloc();

    free(asid1);

    // The freed ASID should eventually be reallocated before rollover.
    let mut found = false;

    for _ in 0..254 {
        let (asid, gen) = alloc();

        if gen > gen_start {
            // Hit rollover — freed ASID may have been reused in new gen.
            break;
        }

        if asid.0 == asid1.0 {
            found = true;
            break;
        }
    }

    assert!(
        found,
        "freed ASID {} should be reused before rollover",
        asid1.0
    );
}

#[test]
fn free_zero_is_noop() {
    // Freeing ASID 0 should not panic or corrupt state.
    free(Asid(0));

    // Should still be able to allocate normally.
    let (asid, _) = alloc();

    assert_ne!(asid.0, 0);
}

#[test]
fn double_rollover() {
    let gen_start = current_generation();

    // Allocate enough to force at least 2 rollovers.
    let mut last_gen = gen_start;

    for _ in 0..1000 {
        let (_, gen) = alloc();

        last_gen = gen;

        if gen >= gen_start + 3 {
            break;
        }
    }

    assert!(
        last_gen >= gen_start + 2,
        "should have rolled over at least twice (start={gen_start}, end={last_gen})"
    );
}
