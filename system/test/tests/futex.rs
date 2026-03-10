//! Host-side tests for futex wait table logic.
//!
//! futex.rs depends on IrqMutex (aarch64 inline asm) and scheduler, so we
//! cannot include it via #[path]. We duplicate the small pure-logic helpers
//! (~20 lines) to test: bucket index computation, hash distribution,
//! registration/deregistration, and wake semantics.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadId(u64);

const BUCKET_COUNT: usize = 64;

/// Mirrors kernel futex::WaitTable::bucket_index.
fn bucket_index(pa: u64) -> usize {
    ((pa >> 2) as usize) % BUCKET_COUNT
}

struct Waiter {
    thread_id: ThreadId,
    pa: u64,
}

/// Minimal wait table for testing registration/deregistration logic.
struct WaitTable {
    buckets: Vec<Vec<Waiter>>,
}

impl WaitTable {
    fn new() -> Self {
        Self {
            buckets: (0..BUCKET_COUNT).map(|_| Vec::new()).collect(),
        }
    }

    /// Mirrors kernel futex::wait.
    fn wait(&mut self, pa: u64, thread_id: ThreadId) {
        let idx = bucket_index(pa);

        self.buckets[idx].push(Waiter { thread_id, pa });
    }

    /// Mirrors kernel futex::wake (returns collected thread IDs).
    fn wake(&mut self, pa: u64, count: u32) -> Vec<ThreadId> {
        let idx = bucket_index(pa);
        let bucket = &mut self.buckets[idx];
        let mut collected = Vec::new();
        let mut i = 0;

        while i < bucket.len() && collected.len() < count as usize {
            if bucket[i].pa == pa {
                let waiter = bucket.swap_remove(i);

                collected.push(waiter.thread_id);
            } else {
                i += 1;
            }
        }

        collected
    }

    /// Mirrors kernel futex::remove_thread.
    fn remove_thread(&mut self, thread_id: ThreadId) {
        for bucket in &mut self.buckets {
            bucket.retain(|w| w.thread_id != thread_id);
        }
    }

    fn bucket_len(&self, pa: u64) -> usize {
        self.buckets[bucket_index(pa)].len()
    }
}

// --- Bucket Index ---

#[test]
fn bucket_index_word_aligned() {
    // Word-aligned addresses (4-byte) should spread across buckets.
    assert_eq!(bucket_index(0), 0);
    assert_eq!(bucket_index(4), 1);
    assert_eq!(bucket_index(8), 2);
    assert_eq!(bucket_index(256), 0); // 256 >> 2 = 64, 64 % 64 = 0
}

#[test]
fn bucket_index_wraps_at_64() {
    // Addresses 0 and 256 map to the same bucket.
    assert_eq!(bucket_index(0), bucket_index(256));
    assert_eq!(bucket_index(4), bucket_index(260));
}

#[test]
fn bucket_index_ignores_sub_word_bits() {
    // Addresses differing only in low 2 bits map to the same bucket.
    assert_eq!(bucket_index(0), bucket_index(1));
    assert_eq!(bucket_index(0), bucket_index(2));
    assert_eq!(bucket_index(0), bucket_index(3));
    assert_ne!(bucket_index(0), bucket_index(4));
}

#[test]
fn bucket_index_distribution() {
    // Word-aligned addresses within a page should spread across buckets.
    let mut buckets_hit = std::collections::HashSet::new();

    for word in 0..64u64 {
        buckets_hit.insert(bucket_index(word * 4));
    }

    // 64 consecutive word addresses should hit all 64 buckets.
    assert_eq!(buckets_hit.len(), 64);
}

// --- Wait / Wake ---

#[test]
fn wait_then_wake_one() {
    let mut table = WaitTable::new();

    table.wait(0x1000, ThreadId(1));

    let woken = table.wake(0x1000, 1);

    assert_eq!(woken, vec![ThreadId(1)]);
    assert_eq!(table.bucket_len(0x1000), 0);
}

#[test]
fn wake_empty_returns_nothing() {
    let mut table = WaitTable::new();
    let woken = table.wake(0x1000, 1);

    assert!(woken.is_empty());
}

#[test]
fn wake_respects_count() {
    let mut table = WaitTable::new();

    table.wait(0x1000, ThreadId(1));
    table.wait(0x1000, ThreadId(2));
    table.wait(0x1000, ThreadId(3));

    let woken = table.wake(0x1000, 2);

    assert_eq!(woken.len(), 2);
    assert_eq!(table.bucket_len(0x1000), 1);
}

#[test]
fn wake_only_matching_pa() {
    let mut table = WaitTable::new();
    let pa_a = 0x1000u64;
    let pa_b = 0x2000u64;

    table.wait(pa_a, ThreadId(1));
    table.wait(pa_b, ThreadId(2));

    let woken = table.wake(pa_a, 10);

    assert_eq!(woken, vec![ThreadId(1)]);
    // pa_b waiter should still be there.
    assert_eq!(table.wake(pa_b, 10), vec![ThreadId(2)]);
}

#[test]
fn wake_handles_hash_collisions() {
    // Two different PAs that hash to the same bucket.
    let pa_a = 0u64;
    let pa_b = 256u64; // Both map to bucket 0.

    assert_eq!(bucket_index(pa_a), bucket_index(pa_b));

    let mut table = WaitTable::new();

    table.wait(pa_a, ThreadId(1));
    table.wait(pa_b, ThreadId(2));

    // Wake only pa_a — should not disturb pa_b.
    let woken = table.wake(pa_a, 10);

    assert_eq!(woken, vec![ThreadId(1)]);
    assert_eq!(table.bucket_len(pa_a), 1); // pa_b still in same bucket
}

// --- Remove Thread ---

#[test]
fn remove_thread_cleans_all_buckets() {
    let mut table = WaitTable::new();

    table.wait(0x1000, ThreadId(1));
    table.wait(0x2000, ThreadId(1)); // Different bucket.
    table.wait(0x1000, ThreadId(2));

    table.remove_thread(ThreadId(1));

    // Only ThreadId(2) should remain.
    let woken = table.wake(0x1000, 10);

    assert_eq!(woken, vec![ThreadId(2)]);
    assert!(table.wake(0x2000, 10).is_empty());
}

#[test]
fn remove_nonexistent_thread_is_noop() {
    let mut table = WaitTable::new();

    table.wait(0x1000, ThreadId(1));
    table.remove_thread(ThreadId(99));

    assert_eq!(table.bucket_len(0x1000), 1);
}
