//! Stress tests for scheduling context concurrent access.
//!
//! Models the kernel's scheduling context ref_count management to verify:
//! - Multiple threads binding to the same context
//! - Handle closed while threads are still bound
//! - Threads exit in random order
//! - ref_count reaches zero exactly when last thread exits
//!
//! Fulfills: VAL-STRESS-005 (scheduling context concurrent access)
//!
//! Run with: cargo test --test stress_sched_ctx_concurrent -- --test-threads=1

#[path = "../../kernel/scheduling_context.rs"]
mod scheduling_context;

use scheduling_context::*;

// ============================================================
// Seeded PRNG (xorshift64) — deterministic, no external deps.
// ============================================================

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 1 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_usize(&mut self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        (self.next_u64() % max as u64) as usize
    }

    fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = self.next_usize(i + 1);
            slice.swap(i, j);
        }
    }
}

// ============================================================
// Model of kernel scheduling context management.
//
// Faithfully replicates the ref_count logic from scheduler.rs:
// - create_scheduling_context: ref_count = 1 (handle)
// - bind_scheduling_context: ref_count += 1 (per thread bind)
// - borrow_scheduling_context: ref_count += 1 (per borrow)
// - return_scheduling_context: ref_count -= 1 (return borrow)
// - release_scheduling_context: ref_count -= 1 (handle close)
// - release_thread_context_ids: ref_count -= 1 for each of
//   context_id and saved_context_id on thread exit
// - When ref_count reaches 0, slot is freed.
// ============================================================

#[derive(Debug)]
struct ContextSlot {
    #[allow(dead_code)]
    ctx: SchedulingContext,
    ref_count: u32,
}

struct ContextTable {
    slots: Vec<Option<ContextSlot>>,
    free_ids: Vec<u32>,
}

impl ContextTable {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_ids: Vec::new(),
        }
    }

    /// Create a scheduling context. Returns id. ref_count = 1 (the handle).
    fn create(&mut self, budget: u64, period: u64) -> Option<SchedulingContextId> {
        if !validate_params(budget, period) {
            return None;
        }

        let ctx = SchedulingContext::new(budget, period, 0);
        let slot = ContextSlot { ctx, ref_count: 1 };

        if let Some(free_id) = self.free_ids.pop() {
            self.slots[free_id as usize] = Some(slot);
            Some(SchedulingContextId(free_id))
        } else {
            let id = self.slots.len() as u32;
            self.slots.push(Some(slot));
            Some(SchedulingContextId(id))
        }
    }

    /// Increment ref_count (bind or borrow). Returns false if slot is gone.
    fn inc_ref(&mut self, id: SchedulingContextId) -> bool {
        match self.slots.get_mut(id.0 as usize) {
            Some(Some(entry)) => {
                entry.ref_count += 1;
                true
            }
            _ => false,
        }
    }

    /// Decrement ref_count. Frees slot if it reaches 0. Returns the new ref_count
    /// (or None if the slot was already empty).
    fn dec_ref(&mut self, id: SchedulingContextId) -> Option<u32> {
        if let Some(slot) = self.slots.get_mut(id.0 as usize) {
            if let Some(entry) = slot {
                entry.ref_count = entry.ref_count.saturating_sub(1);
                let rc = entry.ref_count;
                if rc == 0 {
                    *slot = None;
                    self.free_ids.push(id.0);
                }
                return Some(rc);
            }
        }
        None
    }

    /// Check if a context slot is alive (Some).
    fn is_alive(&self, id: SchedulingContextId) -> bool {
        matches!(self.slots.get(id.0 as usize), Some(Some(_)))
    }

    /// Get ref_count (None if slot is empty).
    fn ref_count(&self, id: SchedulingContextId) -> Option<u32> {
        self.slots
            .get(id.0 as usize)
            .and_then(|s| s.as_ref())
            .map(|e| e.ref_count)
    }

    /// Count of live (non-freed) contexts.
    fn live_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

// ============================================================
// Thread model with scheduling context binding.
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadId(u64);

struct ModelThread {
    #[allow(dead_code)]
    id: ThreadId,
    /// Bound context (bind_scheduling_context).
    context_id: Option<SchedulingContextId>,
    /// Saved context during borrow (borrow_scheduling_context).
    saved_context_id: Option<SchedulingContextId>,
    exited: bool,
}

impl ModelThread {
    fn new(id: ThreadId) -> Self {
        Self {
            id,
            context_id: None,
            saved_context_id: None,
            exited: false,
        }
    }
}

// ============================================================
// Test 1: Multiple threads bind to same context, handle closed
// while bound, threads exit in random order.
//
// This is the primary test for VAL-STRESS-005.
// ============================================================

#[test]
fn stress_sched_ctx_concurrent_bind_close_exit() {
    let mut rng = Rng::new(42);
    let mut table = ContextTable::new();

    for round in 0..50 {
        // Create a scheduling context.
        let ctx_id = table
            .create(MIN_BUDGET_NS, MIN_PERIOD_NS)
            .expect("should create context");

        assert_eq!(
            table.ref_count(ctx_id),
            Some(1),
            "round {}: initial ref_count should be 1 (handle)",
            round
        );

        // Create 5-10 threads that all bind to this context.
        let num_threads = 5 + rng.next_usize(6);
        let mut threads: Vec<ModelThread> = Vec::new();

        for i in 0..num_threads {
            let mut t = ModelThread::new(ThreadId(round as u64 * 1000 + i as u64));
            // Bind: ref_count += 1.
            assert!(
                table.inc_ref(ctx_id),
                "round {}: bind should succeed (context alive)",
                round
            );
            t.context_id = Some(ctx_id);
            threads.push(t);
        }

        assert_eq!(
            table.ref_count(ctx_id),
            Some(1 + num_threads as u32),
            "round {}: ref_count should be 1 (handle) + {} (binds)",
            round,
            num_threads
        );

        // Close the handle (release_scheduling_context): ref_count -= 1.
        let rc = table.dec_ref(ctx_id);
        assert_eq!(
            rc,
            Some(num_threads as u32),
            "round {}: ref_count after handle close should be {} (thread binds)",
            round,
            num_threads
        );

        // Context should still be alive because threads hold refs.
        assert!(
            table.is_alive(ctx_id),
            "round {}: context should be alive while threads bound",
            round
        );

        // Exit threads in random order.
        let mut indices: Vec<usize> = (0..threads.len()).collect();
        rng.shuffle(&mut indices);

        for (exit_order, &idx) in indices.iter().enumerate() {
            let t = &mut threads[idx];
            assert!(!t.exited, "round {} thread {}: should not be already exited", round, idx);

            // release_thread_context_ids: dec ref for context_id.
            if let Some(cid) = t.context_id.take() {
                let rc_after = table.dec_ref(cid);
                let remaining_threads = num_threads - exit_order - 1;

                if remaining_threads == 0 {
                    // This was the last thread — context should be freed.
                    assert_eq!(
                        rc_after,
                        Some(0),
                        "round {}: last thread exit should bring ref_count to 0",
                        round
                    );
                    assert!(
                        !table.is_alive(ctx_id),
                        "round {}: context should be freed when last thread exits",
                        round
                    );
                } else {
                    assert_eq!(
                        rc_after,
                        Some(remaining_threads as u32),
                        "round {} exit_order {}: ref_count should match remaining threads",
                        round,
                        exit_order
                    );
                    assert!(
                        table.is_alive(ctx_id),
                        "round {}: context should still be alive with {} threads",
                        round,
                        remaining_threads
                    );
                }
            }

            // Also release saved_context_id if any (not set in this test).
            if let Some(cid) = t.saved_context_id.take() {
                table.dec_ref(cid);
            }

            t.exited = true;
        }

        // Context should now be freed.
        assert!(
            !table.is_alive(ctx_id),
            "round {}: context must be freed after all threads exit",
            round
        );
    }

    // All contexts should be freed.
    assert_eq!(table.live_count(), 0, "all contexts freed after all rounds");
}

// ============================================================
// Test 2: Threads exit before handle close.
//
// All threads bind, all exit, THEN the handle is closed.
// The last ref should be the handle itself.
// ============================================================

#[test]
fn stress_sched_ctx_threads_exit_before_handle_close() {
    let mut rng = Rng::new(0xDEAD_BEEF);
    let mut table = ContextTable::new();

    for round in 0..50 {
        let ctx_id = table.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap();

        let num_threads = 3 + rng.next_usize(8);
        let mut threads: Vec<ModelThread> = Vec::new();

        for i in 0..num_threads {
            let mut t = ModelThread::new(ThreadId(round as u64 * 1000 + i as u64));
            table.inc_ref(ctx_id);
            t.context_id = Some(ctx_id);
            threads.push(t);
        }

        // Exit all threads in random order.
        let mut indices: Vec<usize> = (0..threads.len()).collect();
        rng.shuffle(&mut indices);

        for &idx in &indices {
            let t = &mut threads[idx];
            if let Some(cid) = t.context_id.take() {
                table.dec_ref(cid);
            }
            t.exited = true;
        }

        // Context should still be alive (handle ref = 1).
        assert!(
            table.is_alive(ctx_id),
            "round {}: context should be alive (handle ref remains)",
            round
        );
        assert_eq!(table.ref_count(ctx_id), Some(1));

        // Now close the handle — this should free the context.
        let rc = table.dec_ref(ctx_id);
        assert_eq!(rc, Some(0), "round {}: handle close should free context", round);
        assert!(!table.is_alive(ctx_id));
    }

    assert_eq!(table.live_count(), 0);
}

// ============================================================
// Test 3: Borrow and return during concurrent access.
//
// Threads bind to context A, some borrow context B (saving A),
// then return B (restoring A), then exit. Tests the
// saved_context_id ref counting path.
// ============================================================

#[test]
fn stress_sched_ctx_borrow_return_concurrent() {
    let mut rng = Rng::new(0xCAFE_BABE);
    let mut table = ContextTable::new();

    for round in 0..30 {
        let ctx_a = table.create(MIN_BUDGET_NS, 10_000_000).unwrap();
        let ctx_b = table.create(MIN_BUDGET_NS * 2, 20_000_000).unwrap();

        let num_threads = 4 + rng.next_usize(7);
        let mut threads: Vec<ModelThread> = Vec::new();

        // All threads bind to ctx_a.
        for i in 0..num_threads {
            let mut t = ModelThread::new(ThreadId(round as u64 * 1000 + i as u64));
            table.inc_ref(ctx_a);
            t.context_id = Some(ctx_a);
            threads.push(t);
        }

        // Half the threads borrow ctx_b.
        let borrowers = num_threads / 2;
        for i in 0..borrowers {
            let t = &mut threads[i];
            // borrow: save current, switch to ctx_b.
            t.saved_context_id = t.context_id; // save A
            table.inc_ref(ctx_b); // ref B
            t.context_id = Some(ctx_b);
        }

        // Expected ref counts:
        // ctx_a: 1 (handle) + num_threads (binds, but borrowers saved it) = still num_threads+1
        //   Wait, borrow doesn't decrement ctx_a — it saves the ID but doesn't release.
        //   The scheduler's borrow_scheduling_context increments ctx_b but doesn't decrement ctx_a.
        //   ctx_a ref_count stays at 1 + num_threads.
        // ctx_b: 1 (handle) + borrowers (borrows).
        assert_eq!(
            table.ref_count(ctx_a),
            Some(1 + num_threads as u32),
            "round {}: ctx_a ref_count after borrow",
            round
        );
        assert_eq!(
            table.ref_count(ctx_b),
            Some(1 + borrowers as u32),
            "round {}: ctx_b ref_count after borrow",
            round
        );

        // Borrowers return ctx_b (restore ctx_a).
        for i in 0..borrowers {
            let t = &mut threads[i];
            let borrowed = t.context_id;
            t.context_id = t.saved_context_id.take(); // restore A
            // return: dec ref on borrowed (B).
            if let Some(cid) = borrowed {
                table.dec_ref(cid);
            }
        }

        // After return: all threads have ctx_a, no saved contexts.
        // ctx_b ref_count = 1 (handle only).
        assert_eq!(
            table.ref_count(ctx_b),
            Some(1),
            "round {}: ctx_b ref_count after all returns",
            round
        );

        // Close both handles.
        table.dec_ref(ctx_a); // ctx_a: num_threads refs remain
        table.dec_ref(ctx_b); // ctx_b: 0 refs — freed

        assert!(!table.is_alive(ctx_b), "round {}: ctx_b freed after handle close", round);
        assert!(table.is_alive(ctx_a), "round {}: ctx_a alive (thread refs)", round);

        // Exit all threads in random order.
        let mut indices: Vec<usize> = (0..threads.len()).collect();
        rng.shuffle(&mut indices);

        for &idx in &indices {
            let t = &mut threads[idx];
            if let Some(cid) = t.context_id.take() {
                table.dec_ref(cid);
            }
            if let Some(cid) = t.saved_context_id.take() {
                table.dec_ref(cid);
            }
            t.exited = true;
        }

        assert!(!table.is_alive(ctx_a), "round {}: ctx_a freed after all exits", round);
    }

    assert_eq!(table.live_count(), 0);
}

// ============================================================
// Test 4: Thread exit while borrowing (doesn't return first).
//
// When a thread exits while borrowing ctx_b (saved = ctx_a),
// release_thread_context_ids decrements BOTH context_id (B)
// and saved_context_id (A). This must free both when ref_counts
// drop to zero.
// ============================================================

#[test]
fn stress_sched_ctx_exit_while_borrowing() {
    let mut rng = Rng::new(0xFEED_FACE);
    let mut table = ContextTable::new();

    for round in 0..40 {
        let ctx_a = table.create(MIN_BUDGET_NS, 10_000_000).unwrap();
        let ctx_b = table.create(MIN_BUDGET_NS * 2, 20_000_000).unwrap();

        let num_threads = 3 + rng.next_usize(5);
        let mut threads: Vec<ModelThread> = Vec::new();

        // All threads bind to ctx_a, then borrow ctx_b.
        for i in 0..num_threads {
            let mut t = ModelThread::new(ThreadId(round as u64 * 1000 + i as u64));
            table.inc_ref(ctx_a); // bind A
            t.context_id = Some(ctx_a);

            // borrow B
            t.saved_context_id = t.context_id; // save A
            table.inc_ref(ctx_b);
            t.context_id = Some(ctx_b);

            threads.push(t);
        }

        // ref_count(A) = 1 (handle) + num_threads (saved refs)
        // ref_count(B) = 1 (handle) + num_threads (active refs)
        assert_eq!(table.ref_count(ctx_a), Some(1 + num_threads as u32));
        assert_eq!(table.ref_count(ctx_b), Some(1 + num_threads as u32));

        // Close handles first.
        table.dec_ref(ctx_a);
        table.dec_ref(ctx_b);

        // Both still alive (thread refs).
        assert!(table.is_alive(ctx_a));
        assert!(table.is_alive(ctx_b));

        // Exit threads in random order WITHOUT returning first.
        // release_thread_context_ids decrements both context_id and saved_context_id.
        let mut indices: Vec<usize> = (0..threads.len()).collect();
        rng.shuffle(&mut indices);

        for (exit_order, &idx) in indices.iter().enumerate() {
            let t = &mut threads[idx];

            if let Some(cid) = t.context_id.take() {
                table.dec_ref(cid); // dec B
            }
            if let Some(cid) = t.saved_context_id.take() {
                table.dec_ref(cid); // dec A
            }
            t.exited = true;

            let remaining = num_threads - exit_order - 1;
            if remaining == 0 {
                assert!(!table.is_alive(ctx_a), "round {}: A should be freed", round);
                assert!(!table.is_alive(ctx_b), "round {}: B should be freed", round);
            } else {
                assert!(
                    table.is_alive(ctx_a),
                    "round {}: A alive with {} thread refs",
                    round,
                    remaining
                );
                assert!(
                    table.is_alive(ctx_b),
                    "round {}: B alive with {} thread refs",
                    round,
                    remaining
                );
            }
        }
    }

    assert_eq!(table.live_count(), 0);
}

// ============================================================
// Test 5: Mixed operations — some threads bind, some borrow,
// handle closed mid-operation, threads exit randomly.
//
// 100 rounds with varying thread counts and operation mixes.
// ============================================================

#[test]
fn stress_sched_ctx_mixed_operations_random() {
    let mut rng = Rng::new(0xC0DE_CAFE);
    let mut table = ContextTable::new();

    for round in 0..100 {
        // Create 1-3 contexts.
        let num_contexts = 1 + rng.next_usize(3);
        let mut ctx_ids: Vec<SchedulingContextId> = Vec::new();
        for _ in 0..num_contexts {
            let budget = MIN_BUDGET_NS * (1 + rng.next_usize(5) as u64);
            let period = budget * (2 + rng.next_usize(5) as u64);
            let period = period.min(MAX_PERIOD_NS);
            if let Some(id) = table.create(budget, period) {
                ctx_ids.push(id);
            }
        }

        if ctx_ids.is_empty() {
            continue;
        }

        let num_threads = 2 + rng.next_usize(8);
        let mut threads: Vec<ModelThread> = Vec::new();

        // Phase 1: Create threads and bind to random contexts.
        for i in 0..num_threads {
            let mut t = ModelThread::new(ThreadId(round as u64 * 1000 + i as u64));
            let cid = ctx_ids[rng.next_usize(ctx_ids.len())];
            table.inc_ref(cid);
            t.context_id = Some(cid);
            threads.push(t);
        }

        // Phase 2: Some threads borrow a different context.
        let borrowers = rng.next_usize(num_threads);
        for i in 0..borrowers {
            let t = &mut threads[i];
            if ctx_ids.len() > 1 {
                // Pick a different context to borrow.
                let current = t.context_id.unwrap();
                let mut borrow_cid = ctx_ids[rng.next_usize(ctx_ids.len())];
                if borrow_cid == current && ctx_ids.len() > 1 {
                    // Pick a different one.
                    borrow_cid = *ctx_ids.iter().find(|&&c| c != current).unwrap();
                }
                t.saved_context_id = t.context_id;
                table.inc_ref(borrow_cid);
                t.context_id = Some(borrow_cid);
            }
        }

        // Phase 3: Close some (or all) handles before threads exit.
        let handles_to_close = rng.next_usize(ctx_ids.len() + 1);
        let mut closed_handles: Vec<SchedulingContextId> = Vec::new();
        for i in 0..handles_to_close {
            let cid = ctx_ids[i];
            table.dec_ref(cid);
            closed_handles.push(cid);
        }

        // Phase 4: Some borrowers return before exiting.
        let returners = rng.next_usize(borrowers + 1);
        for i in 0..returners {
            let t = &mut threads[i];
            if let Some(saved) = t.saved_context_id.take() {
                let borrowed = t.context_id;
                t.context_id = Some(saved);
                if let Some(cid) = borrowed {
                    table.dec_ref(cid);
                }
            }
        }

        // Phase 5: Exit all threads in random order.
        let mut indices: Vec<usize> = (0..threads.len()).collect();
        rng.shuffle(&mut indices);

        for &idx in &indices {
            let t = &mut threads[idx];
            if let Some(cid) = t.context_id.take() {
                table.dec_ref(cid);
            }
            if let Some(cid) = t.saved_context_id.take() {
                table.dec_ref(cid);
            }
            t.exited = true;
        }

        // Phase 6: Close remaining handles.
        for i in handles_to_close..ctx_ids.len() {
            let cid = ctx_ids[i];
            table.dec_ref(cid);
        }

        // All contexts from this round should be freed.
        for &cid in &ctx_ids {
            assert!(
                !table.is_alive(cid),
                "round {}: context {:?} should be freed",
                round,
                cid
            );
        }
    }

    assert_eq!(table.live_count(), 0, "all contexts freed after all rounds");
}

// ============================================================
// Test 6: Slot reuse after concurrent access.
//
// Create contexts, run concurrent thread lifecycle, verify
// slot IDs are reused correctly after freeing.
// ============================================================

#[test]
fn stress_sched_ctx_slot_reuse_after_concurrent() {
    let mut rng = Rng::new(0xBAD_F00D);
    let mut table = ContextTable::new();

    let mut seen_ids: Vec<SchedulingContextId> = Vec::new();

    for round in 0..100 {
        let ctx_id = table.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap();

        // After the first round, we should see ID reuse.
        if round > 0 {
            assert!(
                seen_ids.contains(&ctx_id),
                "round {}: should reuse freed slot ID {:?}",
                round,
                ctx_id
            );
        }

        let num_threads = 2 + rng.next_usize(4);
        let mut threads: Vec<ModelThread> = Vec::new();

        for i in 0..num_threads {
            let mut t = ModelThread::new(ThreadId(round as u64 * 1000 + i as u64));
            table.inc_ref(ctx_id);
            t.context_id = Some(ctx_id);
            threads.push(t);
        }

        // Close handle.
        table.dec_ref(ctx_id);

        // Exit all threads.
        let mut indices: Vec<usize> = (0..threads.len()).collect();
        rng.shuffle(&mut indices);
        for &idx in &indices {
            let t = &mut threads[idx];
            if let Some(cid) = t.context_id.take() {
                table.dec_ref(cid);
            }
            t.exited = true;
        }

        assert!(!table.is_alive(ctx_id));
        if !seen_ids.contains(&ctx_id) {
            seen_ids.push(ctx_id);
        }
    }

    assert_eq!(table.live_count(), 0);
}

// ============================================================
// Test 7: Many threads on one context with high churn.
//
// 20 threads bind to one context, then exit one at a time.
// After each exit, verify ref_count is exactly correct.
// ============================================================

#[test]
fn stress_sched_ctx_many_threads_precise_refcount() {
    let mut rng = Rng::new(0x1111_2222);
    let mut table = ContextTable::new();

    for round in 0..20 {
        let ctx_id = table.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap();
        let num_threads = 20;

        // Bind all threads.
        let mut threads: Vec<ModelThread> = Vec::new();
        for i in 0..num_threads {
            let mut t = ModelThread::new(ThreadId(round as u64 * 1000 + i as u64));
            table.inc_ref(ctx_id);
            t.context_id = Some(ctx_id);
            threads.push(t);
        }

        // Close handle.
        table.dec_ref(ctx_id);

        // Expected: ref_count = num_threads (20).
        assert_eq!(table.ref_count(ctx_id), Some(num_threads as u32));

        // Exit in random order, checking ref_count after each exit.
        let mut indices: Vec<usize> = (0..threads.len()).collect();
        rng.shuffle(&mut indices);

        for (exit_order, &idx) in indices.iter().enumerate() {
            let t = &mut threads[idx];
            if let Some(cid) = t.context_id.take() {
                table.dec_ref(cid);
            }
            t.exited = true;

            let remaining = num_threads - exit_order - 1;
            if remaining == 0 {
                assert!(!table.is_alive(ctx_id));
            } else {
                assert_eq!(
                    table.ref_count(ctx_id),
                    Some(remaining as u32),
                    "round {} after exit {}: ref_count mismatch",
                    round,
                    exit_order
                );
            }
        }
    }

    assert_eq!(table.live_count(), 0);
}

// ============================================================
// Test 8: Interleaved bind/borrow/return/exit across
// multiple contexts simultaneously.
//
// 500 operations: randomly create contexts, bind threads,
// borrow, return, exit, close handles. Verify no leaks.
// ============================================================

#[test]
fn stress_sched_ctx_interleaved_500_ops() {
    let mut rng = Rng::new(0xABBA_ACDC);
    let mut table = ContextTable::new();

    let mut live_contexts: Vec<SchedulingContextId> = Vec::new();
    let mut live_threads: Vec<ModelThread> = Vec::new();
    let mut next_thread_id: u64 = 0;

    for _op in 0..500 {
        let action = rng.next_usize(10);

        match action {
            0..=1 => {
                // Create a new context.
                let budget = MIN_BUDGET_NS * (1 + rng.next_usize(3) as u64);
                let period = budget * (2 + rng.next_usize(3) as u64);
                let period = period.min(MAX_PERIOD_NS);
                if let Some(id) = table.create(budget, period) {
                    live_contexts.push(id);
                }
            }
            2..=4 if !live_contexts.is_empty() => {
                // Create a thread and bind to a random context.
                let cid = live_contexts[rng.next_usize(live_contexts.len())];
                if table.is_alive(cid) {
                    table.inc_ref(cid);
                    let mut t = ModelThread::new(ThreadId(next_thread_id));
                    next_thread_id += 1;
                    t.context_id = Some(cid);
                    live_threads.push(t);
                }
            }
            5 if !live_threads.is_empty() && live_contexts.len() > 1 => {
                // Borrow a different context.
                let idx = rng.next_usize(live_threads.len());
                let t = &mut live_threads[idx];
                if t.saved_context_id.is_none() && t.context_id.is_some() {
                    let current = t.context_id.unwrap();
                    let candidates: Vec<_> = live_contexts
                        .iter()
                        .filter(|&&c| c != current && table.is_alive(c))
                        .copied()
                        .collect();
                    if !candidates.is_empty() {
                        let borrow_cid = candidates[rng.next_usize(candidates.len())];
                        table.inc_ref(borrow_cid);
                        t.saved_context_id = t.context_id;
                        t.context_id = Some(borrow_cid);
                    }
                }
            }
            6 if !live_threads.is_empty() => {
                // Return borrowed context.
                let idx = rng.next_usize(live_threads.len());
                let t = &mut live_threads[idx];
                if let Some(saved) = t.saved_context_id.take() {
                    let borrowed = t.context_id;
                    t.context_id = Some(saved);
                    if let Some(cid) = borrowed {
                        table.dec_ref(cid);
                    }
                }
            }
            7..=8 if !live_threads.is_empty() => {
                // Exit a random thread.
                let idx = rng.next_usize(live_threads.len());
                let t = &mut live_threads[idx];
                if let Some(cid) = t.context_id.take() {
                    table.dec_ref(cid);
                }
                if let Some(cid) = t.saved_context_id.take() {
                    table.dec_ref(cid);
                }
                live_threads.swap_remove(idx);
            }
            _ if !live_contexts.is_empty() => {
                // Close a context handle.
                let idx = rng.next_usize(live_contexts.len());
                let cid = live_contexts[idx];
                if table.is_alive(cid) {
                    table.dec_ref(cid);
                }
                // Remove from live list (handle is closed regardless).
                live_contexts.swap_remove(idx);
            }
            _ => {} // No-op if lists are empty.
        }
    }

    // Cleanup: exit all remaining threads, close all remaining handles.
    for t in &mut live_threads {
        if let Some(cid) = t.context_id.take() {
            table.dec_ref(cid);
        }
        if let Some(cid) = t.saved_context_id.take() {
            table.dec_ref(cid);
        }
    }

    for &cid in &live_contexts {
        if table.is_alive(cid) {
            table.dec_ref(cid);
        }
    }

    assert_eq!(
        table.live_count(),
        0,
        "all contexts freed after 500 interleaved operations"
    );
}
