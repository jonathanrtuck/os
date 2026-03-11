//! Futex — fast userspace mutual exclusion.
//!
//! Provides two operations:
//! - `wait(pa, thread_id)` — add a thread to the wait queue for a physical address.
//! - `wake(pa, count)` — wake up to `count` threads waiting on a physical address.
//!
//! The physical address (not virtual) is used as the key so that threads in
//! different processes sharing the same physical page can synchronize correctly.
//!
//! # Protocol
//!
//! Fast path (userspace, no syscall): atomic CAS to acquire/release.
//! Slow path (contention):
//! 1. Waiter: `futex_wait(addr, expected)` — kernel checks `*addr == expected`,
//!    if so, blocks the thread. If value changed, returns WouldBlock.
//! 2. Waker: sets the value in userspace, calls `futex_wake(addr, 1)` — kernel
//!    wakes one sleeping thread.
//!
//! # Lost-wakeup prevention
//!
//! There is a race window between the waiter being recorded in the futex table
//! and actually blocking in the scheduler. If a waker runs during this window:
//! - `try_wake` finds the thread still Running (not Blocked) → returns false.
//! - Waker calls `scheduler::set_wake_pending(tid)` to set a flag on the thread.
//! - When the waiter enters `scheduler::block_current_unless_woken()`, it checks
//!   the flag and returns immediately instead of blocking.
//!
// AUDIT: 2026-03-11 — 0 unsafe blocks. 6-category checklist applied. Two-phase
// lock ordering (futex → scheduler) verified correct. Lost-wakeup prevention via
// wake_pending flag is sound. swap_remove in wake loop correctly handles index
// management. Thread cleanup (remove_thread) scans all 64 buckets. No bugs found.

use super::sync::IrqMutex;
use super::thread::ThreadId;
use alloc::vec::Vec;

const BUCKET_COUNT: usize = 64;

struct Waiter {
    thread_id: ThreadId,
    pa: u64,
}
struct WaitTable {
    buckets: [Vec<Waiter>; BUCKET_COUNT],
}

static WAIT_TABLE: IrqMutex<WaitTable> = IrqMutex::new(WaitTable::new());

impl WaitTable {
    const fn new() -> Self {
        Self {
            buckets: [const { Vec::new() }; BUCKET_COUNT],
        }
    }

    fn bucket_index(pa: u64) -> usize {
        // Word-aligned addresses, spread across buckets.
        ((pa >> 2) as usize) % BUCKET_COUNT
    }
}

/// Remove a thread from all futex wait queues.
///
/// Called during process cleanup to prevent dangling references.
pub fn remove_thread(thread_id: ThreadId) {
    let mut table = WAIT_TABLE.lock();

    for bucket in &mut table.buckets {
        bucket.retain(|w| w.thread_id != thread_id);
    }
}
/// Record a thread as waiting on a physical address.
///
/// Called by the futex_wait syscall after verifying the value matches.
/// The thread is NOT yet blocked — the caller must subsequently call
/// `scheduler::block_current_unless_woken()`.
pub fn wait(pa: u64, thread_id: ThreadId) {
    let mut table = WAIT_TABLE.lock();
    let idx = WaitTable::bucket_index(pa);

    table.buckets[idx].push(Waiter { thread_id, pa });
}
/// Wake up to `count` threads waiting on a physical address.
///
/// Returns the number of threads actually woken. For each waiter found,
/// attempts `scheduler::try_wake`. If the thread is not yet blocked
/// (still Running), sets a pending-wake flag via `scheduler::set_wake_pending`
/// so the thread won't block when it enters the scheduler.
pub fn wake(pa: u64, count: u32) -> u32 {
    // Phase 1: collect thread IDs to wake (under futex lock).
    let to_wake = {
        let mut table = WAIT_TABLE.lock();
        let idx = WaitTable::bucket_index(pa);
        let bucket = &mut table.buckets[idx];
        let mut collected = Vec::new();
        let mut i = 0;

        while i < bucket.len() && collected.len() < count as usize {
            if bucket[i].pa == pa {
                let waiter = bucket.swap_remove(i);

                collected.push(waiter.thread_id);
                // Don't increment i — swap_remove moved the last element here.
            } else {
                i += 1;
            }
        }

        collected
    };
    // Futex lock released here.

    // Phase 2: wake threads (under scheduler lock, NOT futex lock).
    let mut woken = 0u32;

    for tid in to_wake {
        if super::scheduler::try_wake(tid) {
            woken += 1;
        } else {
            // Thread not yet blocked — set pending flag so it won't block.
            super::scheduler::set_wake_pending(tid);

            woken += 1;
        }
    }

    woken
}
