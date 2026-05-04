//! Event — level-triggered signal bits with wait queue.
//!
//! Events are the universal synchronization primitive. Signal bits are OR'd
//! on signal, AND-NOT'd on clear. Waiters match against current bits
//! (level-triggered: if bits are already set, the waiter wakes immediately).

use core::sync::atomic::{AtomicU64, Ordering};

use crate::{
    config,
    types::{EndpointId, EventId, SyscallError, ThreadId},
};

/// Inline storage for threads woken by a signal — no heap allocation.
#[derive(Debug)]
pub struct WakeList {
    items: [WakeInfo; config::MAX_WAITERS_PER_EVENT],
    len: usize,
}

impl WakeList {
    fn new() -> Self {
        WakeList {
            items: [WakeInfo {
                thread_id: ThreadId(0),
                fired_bits: 0,
            }; config::MAX_WAITERS_PER_EVENT],
            len: 0,
        }
    }

    fn push(&mut self, info: WakeInfo) {
        if self.len < self.items.len() {
            self.items[self.len] = info;
            self.len += 1;
        }
    }

    pub fn as_slice(&self) -> &[WakeInfo] {
        &self.items[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Information returned when a waiter is woken by a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeInfo {
    pub thread_id: ThreadId,
    pub fired_bits: u64,
}

/// A pending waiter: thread ID + requested bit mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Waiter {
    thread_id: ThreadId,
    mask: u64,
}

/// An event object — signal bits + waiter queue.
pub struct Event {
    pub id: EventId,
    bits: AtomicU64,
    waiters: [Option<Waiter>; config::MAX_WAITERS_PER_EVENT],
    waiter_count: usize,
    bound_endpoint: Option<EndpointId>,
}

#[allow(clippy::new_without_default)]
impl Event {
    pub fn new(id: EventId) -> Self {
        Event {
            id,
            bits: AtomicU64::new(0),
            waiters: [None; config::MAX_WAITERS_PER_EVENT],
            waiter_count: 0,
            bound_endpoint: None,
        }
    }

    pub fn bits(&self) -> u64 {
        self.bits.load(Ordering::Acquire)
    }

    pub fn bound_endpoint(&self) -> Option<EndpointId> {
        self.bound_endpoint
    }

    pub fn waiter_count(&self) -> usize {
        self.waiter_count
    }

    /// Check if any requested bits are currently set. Returns the matching
    /// bits, or None if no match (caller should block the thread).
    pub fn check(&self, mask: u64) -> Option<u64> {
        let fired = self.bits.load(Ordering::Acquire) & mask;

        if fired != 0 { Some(fired) } else { None }
    }

    /// Signal (OR) bits and wake all matching waiters.
    /// On ARM64 with LSE, fetch_or compiles to a single LDSET instruction (~4 cycles).
    pub fn signal(&mut self, bits: u64) -> WakeList {
        self.bits.fetch_or(bits, Ordering::Release);

        let current_bits = self.bits.load(Ordering::Acquire);
        let mut woken = WakeList::new();

        for slot in &mut self.waiters {
            if let Some(waiter) = slot {
                let fired = current_bits & waiter.mask;

                if fired != 0 {
                    woken.push(WakeInfo {
                        thread_id: waiter.thread_id,
                        fired_bits: fired,
                    });

                    *slot = None;
                    self.waiter_count -= 1;
                }
            }
        }

        woken
    }

    /// Clear (AND-NOT) bits.
    /// On ARM64 with LSE, fetch_and compiles to a single LDCLR instruction (~4 cycles).
    pub fn clear(&mut self, bits: u64) {
        self.bits.fetch_and(!bits, Ordering::Release);
    }

    /// Add a waiter to the queue. Returns Err if the queue is full.
    pub fn add_waiter(&mut self, thread_id: ThreadId, mask: u64) -> Result<(), SyscallError> {
        for slot in &mut self.waiters {
            if slot.is_none() {
                *slot = Some(Waiter { thread_id, mask });
                self.waiter_count += 1;

                return Ok(());
            }
        }

        Err(SyscallError::BufferFull)
    }

    /// Remove a waiter by thread ID (for timeout or cancellation).
    pub fn remove_waiter(&mut self, thread_id: ThreadId) -> bool {
        for slot in &mut self.waiters {
            if let Some(w) = slot
                && w.thread_id == thread_id
            {
                *slot = None;
                self.waiter_count -= 1;

                return true;
            }
        }

        false
    }

    /// Bind this event to a channel endpoint.
    pub fn bind_endpoint(&mut self, endpoint: EndpointId) -> Result<(), SyscallError> {
        if self.bound_endpoint.is_some() {
            return Err(SyscallError::InvalidArgument);
        }

        self.bound_endpoint = Some(endpoint);

        Ok(())
    }

    /// Unbind the channel endpoint.
    pub fn unbind_endpoint(&mut self) {
        self.bound_endpoint = None;
    }

    #[cfg(test)]
    pub fn verify_internal_counts(&self) -> Result<(), &'static str> {
        let actual = self.waiters.iter().filter(|s| s.is_some()).count();

        if actual != self.waiter_count {
            return Err("waiter_count mismatch");
        }

        Ok(())
    }

    #[cfg(test)]
    pub fn all_waiter_thread_ids(&self) -> alloc::vec::Vec<crate::types::ThreadId> {
        self.waiters
            .iter()
            .filter_map(|s| s.as_ref().map(|w| w.thread_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(id: u32) -> Event {
        Event::new(EventId(id))
    }

    #[test]
    fn new_event_has_no_bits() {
        let e = make_event(0);

        assert_eq!(e.bits(), 0);
        assert!(e.check(u64::MAX).is_none());
    }

    #[test]
    fn signal_before_wait_is_immediate() {
        let mut e = make_event(0);

        e.signal(0b101);

        assert_eq!(e.check(0b100), Some(0b100));
        assert!(e.check(0b010).is_none());
    }

    #[test]
    fn wait_then_signal_wakes() {
        let mut e = make_event(0);

        e.add_waiter(ThreadId(1), 0b11).unwrap();

        let woken = e.signal(0b01);

        assert_eq!(woken.len(), 1);
        assert_eq!(woken.as_slice()[0].thread_id, ThreadId(1));
        assert_eq!(woken.as_slice()[0].fired_bits, 0b01);
        assert_eq!(e.waiter_count(), 0);
    }

    #[test]
    fn signal_only_wakes_matching_waiters() {
        let mut e = make_event(0);

        e.add_waiter(ThreadId(1), 0b01).unwrap();
        e.add_waiter(ThreadId(2), 0b10).unwrap();

        let woken = e.signal(0b01);

        assert_eq!(woken.len(), 1);
        assert_eq!(woken.as_slice()[0].thread_id, ThreadId(1));
        assert_eq!(e.waiter_count(), 1);
    }

    #[test]
    fn multi_waiter_signal_wakes_all_matching() {
        let mut e = make_event(0);

        e.add_waiter(ThreadId(1), 0b01).unwrap();
        e.add_waiter(ThreadId(2), 0b11).unwrap();
        e.add_waiter(ThreadId(3), 0b10).unwrap();

        let woken = e.signal(0b01);

        assert_eq!(woken.len(), 2);
        assert!(woken.as_slice().iter().any(|w| w.thread_id == ThreadId(1)));
        assert!(woken.as_slice().iter().any(|w| w.thread_id == ThreadId(2)));
        assert_eq!(e.waiter_count(), 1);
    }

    #[test]
    fn coalescing_signal_same_bit_twice() {
        let mut e = make_event(0);

        e.signal(0b01);
        e.signal(0b01);

        assert_eq!(e.bits(), 0b01);
        assert_eq!(e.check(0b01), Some(0b01));
    }

    #[test]
    fn clear_resets_bits() {
        let mut e = make_event(0);

        e.signal(0b11);
        e.clear(0b01);

        assert_eq!(e.bits(), 0b10);
        assert!(e.check(0b01).is_none());
        assert_eq!(e.check(0b10), Some(0b10));
    }

    #[test]
    fn clear_then_check_blocks() {
        let mut e = make_event(0);

        e.signal(0b11);
        e.clear(0b01);

        assert!(e.check(0b01).is_none());
    }

    #[test]
    fn check_returns_none_when_no_bits_match() {
        let e = make_event(0);

        assert!(e.check(0b11).is_none());
    }

    #[test]
    fn remove_waiter() {
        let mut e = make_event(0);

        e.add_waiter(ThreadId(5), 0b1).unwrap();

        assert_eq!(e.waiter_count(), 1);
        assert!(e.remove_waiter(ThreadId(5)));
        assert_eq!(e.waiter_count(), 0);
        assert!(!e.remove_waiter(ThreadId(5)));
    }

    #[test]
    fn waiter_queue_exhaustion() {
        let mut e = make_event(0);

        for i in 0..config::MAX_WAITERS_PER_EVENT {
            e.add_waiter(ThreadId(i as u32), 0b1).unwrap();
        }

        assert_eq!(
            e.add_waiter(ThreadId(999), 0b1),
            Err(SyscallError::BufferFull)
        );
    }

    #[test]
    fn bind_endpoint() {
        let mut e = make_event(0);

        assert!(e.bind_endpoint(EndpointId(7)).is_ok());
        assert_eq!(e.bound_endpoint(), Some(EndpointId(7)));
        assert_eq!(
            e.bind_endpoint(EndpointId(8)),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn unbind_and_rebind_endpoint() {
        let mut e = make_event(0);

        e.bind_endpoint(EndpointId(7)).unwrap();
        e.unbind_endpoint();

        assert!(e.bound_endpoint().is_none());
        assert!(e.bind_endpoint(EndpointId(8)).is_ok());
    }

    // --- Adversarial and boundary tests ---

    #[test]
    fn signal_with_mask_zero_is_noop() {
        let mut e = make_event(1);

        e.add_waiter(ThreadId(1), 0b11).unwrap();

        let woken = e.signal(0);

        assert_eq!(e.bits(), 0);
        assert!(woken.is_empty());
        assert_eq!(e.waiter_count(), 1);
    }

    #[test]
    fn add_waiter_with_mask_zero_succeeds_but_never_fires() {
        let mut e = make_event(2);

        assert!(e.add_waiter(ThreadId(1), 0).is_ok());
        assert_eq!(e.waiter_count(), 1);

        // Signal every possible bit — the zero-mask waiter must not wake.
        let woken = e.signal(u64::MAX);

        assert!(woken.is_empty());
        assert_eq!(e.waiter_count(), 1);
    }

    #[test]
    fn add_waiter_at_capacity_returns_buffer_full() {
        let mut e = make_event(3);

        for i in 0..config::MAX_WAITERS_PER_EVENT {
            assert!(e.add_waiter(ThreadId(i as u32), 1 << (i % 64)).is_ok());
        }

        assert_eq!(e.waiter_count(), config::MAX_WAITERS_PER_EVENT);
        assert_eq!(
            e.add_waiter(ThreadId(999), 0b1),
            Err(SyscallError::BufferFull)
        );
        // Capacity unchanged after rejection.
        assert_eq!(e.waiter_count(), config::MAX_WAITERS_PER_EVENT);
    }

    #[test]
    fn signal_all_64_bits() {
        let mut e = make_event(4);
        let woken = e.signal(u64::MAX);

        assert_eq!(e.bits(), u64::MAX);
        assert!(woken.is_empty());
        assert_eq!(e.check(u64::MAX), Some(u64::MAX));
    }

    #[test]
    fn check_with_upper_32_bits() {
        let mut e = make_event(5);
        let upper_mask: u64 = 0xFFFF_FFFF_0000_0000;

        // No bits set — check returns None.
        assert!(e.check(upper_mask).is_none());

        // Set bit 63 and bit 32.
        e.signal(1u64 << 63 | 1u64 << 32);

        assert_eq!(e.check(upper_mask), Some(1u64 << 63 | 1u64 << 32));
        // Lower 32 bits are unset.
        assert!(e.check(0x0000_0000_FFFF_FFFF).is_none());
    }

    #[test]
    fn remove_waiter_that_does_not_exist_is_noop() {
        let mut e = make_event(6);

        // Empty queue — remove returns false, count stays 0.
        assert!(!e.remove_waiter(ThreadId(42)));
        assert_eq!(e.waiter_count(), 0);

        // Add a different thread, then try removing a non-existent one.
        e.add_waiter(ThreadId(1), 0b1).unwrap();

        assert!(!e.remove_waiter(ThreadId(42)));
        assert_eq!(e.waiter_count(), 1);
    }

    #[test]
    fn signal_then_clear_then_check() {
        let mut e = make_event(7);

        e.signal(0b1111);

        assert_eq!(e.bits(), 0b1111);

        e.clear(0b1010);

        assert_eq!(e.bits(), 0b0101);

        // Cleared bits must not match.
        assert!(e.check(0b1010).is_none());
        // Surviving bits still match.
        assert_eq!(e.check(0b0101), Some(0b0101));
        // Mixed mask only returns surviving bits.
        assert_eq!(e.check(0b1111), Some(0b0101));
    }

    #[test]
    fn wake_list_capacity_all_waiters_wake_simultaneously() {
        let mut e = make_event(8);

        // Fill every waiter slot, all watching bit 0.
        for i in 0..config::MAX_WAITERS_PER_EVENT {
            e.add_waiter(ThreadId(i as u32), 0b1).unwrap();
        }

        assert_eq!(e.waiter_count(), config::MAX_WAITERS_PER_EVENT);

        let woken = e.signal(0b1);

        assert_eq!(woken.len(), config::MAX_WAITERS_PER_EVENT);
        assert_eq!(e.waiter_count(), 0);

        // Every thread must appear exactly once.
        for i in 0..config::MAX_WAITERS_PER_EVENT {
            assert!(
                woken
                    .as_slice()
                    .iter()
                    .any(|w| w.thread_id == ThreadId(i as u32)),
            );
        }
    }
}
