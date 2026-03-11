//! Host-side tests for ticket spinlock logic.
//!
//! sync.rs depends on aarch64 inline asm for DAIF masking, so we cannot
//! include it via #[path]. We duplicate the ticket spinlock logic using
//! std atomics to test: counter ordering, FIFO fairness, and overflow
//! behavior.

use std::sync::atomic::{AtomicU32, Ordering};

/// Minimal ticket-spinlock state mirroring kernel IrqMutex internals.
struct TicketLock {
    next_ticket: AtomicU32,
    now_serving: AtomicU32,
}

impl TicketLock {
    const fn new() -> Self {
        Self {
            next_ticket: AtomicU32::new(0),
            now_serving: AtomicU32::new(0),
        }
    }

    /// Acquire: take a ticket, spin until served. Returns ticket number.
    fn lock(&self) -> u32 {
        let my_ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);

        while self.now_serving.load(Ordering::Acquire) != my_ticket {
            core::hint::spin_loop();
        }

        my_ticket
    }

    /// Release: increment the serving counter.
    fn unlock(&self) {
        self.now_serving.fetch_add(1, Ordering::Release);
    }

    fn now_serving(&self) -> u32 {
        self.now_serving.load(Ordering::Relaxed)
    }

    fn next_ticket(&self) -> u32 {
        self.next_ticket.load(Ordering::Relaxed)
    }
}

// --- Ticket Spinlock Tests ---

#[test]
fn ticket_lock_initial_state() {
    let lock = TicketLock::new();

    assert_eq!(lock.next_ticket(), 0);
    assert_eq!(lock.now_serving(), 0);
}

#[test]
fn ticket_lock_acquire_release_single() {
    let lock = TicketLock::new();

    let ticket = lock.lock();
    assert_eq!(ticket, 0);
    assert_eq!(lock.next_ticket(), 1);
    assert_eq!(lock.now_serving(), 0);

    lock.unlock();
    assert_eq!(lock.now_serving(), 1);
}

#[test]
fn ticket_lock_sequential_acquire_release() {
    let lock = TicketLock::new();

    // Three sequential lock/unlock cycles.
    for i in 0..3u32 {
        let ticket = lock.lock();
        assert_eq!(ticket, i, "ticket should increment sequentially");
        lock.unlock();
    }

    assert_eq!(lock.next_ticket(), 3);
    assert_eq!(lock.now_serving(), 3);
}

#[test]
fn ticket_lock_fifo_ordering() {
    // Verify that tickets are served in FIFO order.
    // Simulate: thread A takes ticket 0, thread B takes ticket 1.
    // Thread B must wait until thread A unlocks.
    let lock = TicketLock::new();

    let ticket_a = lock.next_ticket.fetch_add(1, Ordering::Relaxed);
    let ticket_b = lock.next_ticket.fetch_add(1, Ordering::Relaxed);

    assert_eq!(ticket_a, 0);
    assert_eq!(ticket_b, 1);

    // Thread A is currently being served (now_serving == 0 == ticket_a).
    assert_eq!(lock.now_serving(), ticket_a);

    // Thread B would spin here because now_serving != ticket_b.
    assert_ne!(lock.now_serving(), ticket_b);

    // Thread A releases.
    lock.unlock();

    // Now thread B can proceed.
    assert_eq!(lock.now_serving(), ticket_b);
}

#[test]
fn ticket_lock_concurrent_fifo() {
    // Multi-threaded test: multiple threads acquire the lock and record
    // their acquisition order. The order must match ticket order (FIFO).
    use std::sync::{Arc, Barrier};

    let lock = Arc::new(TicketLock::new());
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let n_threads = 8;
    let barrier = Arc::new(Barrier::new(n_threads));

    let handles: Vec<_> = (0..n_threads)
        .map(|_| {
            let lock = Arc::clone(&lock);
            let order = Arc::clone(&order);
            let barrier = Arc::clone(&barrier);

            std::thread::spawn(move || {
                barrier.wait();
                let ticket = lock.lock();
                order.lock().unwrap().push(ticket);
                lock.unlock();
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    let acquired = order.lock().unwrap();
    assert_eq!(acquired.len(), n_threads);

    // The order of acquisition must be sorted (FIFO by ticket number).
    let mut sorted = acquired.clone();
    sorted.sort();
    assert_eq!(*acquired, sorted, "ticket lock must be FIFO");
}

#[test]
fn ticket_lock_no_data_race_on_counter() {
    // Stress test: many threads contending on the same lock.
    // The protected counter must be exactly incremented by the total
    // number of lock/unlock pairs.
    use std::sync::Arc;

    let lock = Arc::new(TicketLock::new());
    let counter = Arc::new(AtomicU32::new(0));
    let n_threads = 8;
    let n_iters = 1000;

    let handles: Vec<_> = (0..n_threads)
        .map(|_| {
            let lock = Arc::clone(&lock);
            let counter = Arc::clone(&counter);

            std::thread::spawn(move || {
                for _ in 0..n_iters {
                    lock.lock();
                    // Critical section: non-atomic increment.
                    let val = counter.load(Ordering::Relaxed);
                    counter.store(val + 1, Ordering::Relaxed);
                    lock.unlock();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        counter.load(Ordering::Relaxed),
        n_threads as u32 * n_iters,
        "counter must equal total increments (no data race)"
    );
}

#[test]
fn ticket_lock_u32_near_overflow() {
    // Verify that ticket numbers near u32::MAX don't panic.
    // With ≤ 8 cores, wrapping can't cause a real collision.
    let lock = TicketLock::new();

    // Start counters near u32::MAX.
    lock.next_ticket
        .store(u32::MAX - 2, Ordering::Relaxed);
    lock.now_serving
        .store(u32::MAX - 2, Ordering::Relaxed);

    // Three lock/unlock cycles spanning the u32 wrap.
    for _ in 0..3 {
        let _ticket = lock.lock();
        lock.unlock();
    }

    // After wrapping, now_serving should be u32::MAX - 2 + 3 = wrapping to 1.
    assert_eq!(
        lock.now_serving(),
        (u32::MAX - 2).wrapping_add(3),
        "counters must wrap correctly"
    );
}
