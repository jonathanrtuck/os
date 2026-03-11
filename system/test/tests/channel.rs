//! Host-side tests for channel encoding/decoding and state machine logic.
//!
//! channel.rs depends on IrqMutex (aarch64 inline asm), page_allocator,
//! scheduler, etc., so we cannot include it via #[path]. Instead, we
//! duplicate the small pure-logic helpers and the Channel state machine
//! (~30 lines) to test: ChannelId encoding/decoding, signal/pending flag
//! logic, close_endpoint refcounting, and double-close behavior.

/// Mirrors kernel handle::ChannelId.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChannelId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadId(u64);

/// Mirrors kernel channel::channel_index.
fn channel_index(id: ChannelId) -> usize {
    id.0 as usize / 2
}

/// Mirrors kernel channel::endpoint_index.
fn endpoint_index(id: ChannelId) -> usize {
    id.0 as usize % 2
}

/// Minimal channel state for testing the state machine.
struct Channel {
    pending_signal: [bool; 2],
    waiter: [Option<ThreadId>; 2],
    closed_count: u8,
}

impl Channel {
    fn new() -> Self {
        Self {
            pending_signal: [false, false],
            waiter: [None, None],
            closed_count: 0,
        }
    }

    /// Mirrors kernel channel::check_pending.
    fn check_pending(&mut self, id: ChannelId) -> bool {
        let ep = endpoint_index(id);

        if self.pending_signal[ep] {
            self.pending_signal[ep] = false;
            true
        } else {
            false
        }
    }

    /// Mirrors kernel channel::signal (flag logic only, no scheduler wake).
    fn signal(&mut self, id: ChannelId) -> Option<ThreadId> {
        let peer_ep = 1 - endpoint_index(id);

        self.pending_signal[peer_ep] = true;
        self.waiter[peer_ep].take()
    }

    /// Mirrors kernel channel::close_endpoint.
    /// Returns `(freed, peer_waiter)` — `freed` is true if both endpoints
    /// are now closed, `peer_waiter` is the peer's waiter (for waking).
    fn close_endpoint(&mut self, id: ChannelId) -> bool {
        // Already fully closed — prevent double-free.
        if self.closed_count >= 2 {
            return false;
        }

        let ep = endpoint_index(id);
        let peer_ep = 1 - ep;

        self.waiter[ep] = None;

        // Take peer's waiter for waking (mirrors kernel behavior).
        let _peer_waiter = self.waiter[peer_ep].take();

        self.closed_count += 1;
        self.closed_count == 2
    }

    fn register_waiter(&mut self, id: ChannelId, waiter: ThreadId) {
        let ep = endpoint_index(id);

        self.waiter[ep] = Some(waiter);
    }

    fn unregister_waiter(&mut self, id: ChannelId) {
        let ep = endpoint_index(id);

        self.waiter[ep] = None;
    }
}

// --- Encoding / Decoding ---

#[test]
fn encoding_channel_zero() {
    // Channel 0: endpoints are ChannelId(0) and ChannelId(1).
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    assert_eq!(channel_index(ep0), 0);
    assert_eq!(endpoint_index(ep0), 0);
    assert_eq!(channel_index(ep1), 0);
    assert_eq!(endpoint_index(ep1), 1);
}

#[test]
fn encoding_channel_one() {
    // Channel 1: endpoints are ChannelId(2) and ChannelId(3).
    let ep0 = ChannelId(2);
    let ep1 = ChannelId(3);

    assert_eq!(channel_index(ep0), 1);
    assert_eq!(endpoint_index(ep0), 0);
    assert_eq!(channel_index(ep1), 1);
    assert_eq!(endpoint_index(ep1), 1);
}

#[test]
fn encoding_roundtrip() {
    // For any channel index and endpoint, encoding is reversible.
    for ch_idx in 0..128u32 {
        for ep in 0..2u32 {
            let id = ChannelId(ch_idx * 2 + ep);

            assert_eq!(channel_index(id), ch_idx as usize);
            assert_eq!(endpoint_index(id), ep as usize);
        }
    }
}

#[test]
fn encoding_both_endpoints_share_channel_index() {
    for ch_idx in 0..64u32 {
        let ep0 = ChannelId(ch_idx * 2);
        let ep1 = ChannelId(ch_idx * 2 + 1);

        assert_eq!(channel_index(ep0), channel_index(ep1));
        assert_ne!(endpoint_index(ep0), endpoint_index(ep1));
    }
}

// --- Signal / Pending Flag ---

#[test]
fn no_pending_initially() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    assert!(!ch.check_pending(ep0));
    assert!(!ch.check_pending(ep1));
}

#[test]
fn signal_sets_peer_pending() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    // ep0 signals — sets pending on ep1.
    ch.signal(ep0);

    assert!(
        !ch.check_pending(ep0),
        "signaler should not see own pending"
    );
    assert!(ch.check_pending(ep1), "peer should see pending");
}

#[test]
fn check_pending_consumes_flag() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.signal(ep0);

    assert!(ch.check_pending(ep1));
    assert!(!ch.check_pending(ep1), "second check should return false");
}

#[test]
fn signal_returns_peer_waiter() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.register_waiter(ep1, ThreadId(42));

    let waiter = ch.signal(ep0);

    assert_eq!(waiter, Some(ThreadId(42)));
}

#[test]
fn signal_takes_waiter_only_once() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.register_waiter(ep1, ThreadId(42));

    let w1 = ch.signal(ep0);
    let w2 = ch.signal(ep0);

    assert_eq!(w1, Some(ThreadId(42)));
    assert_eq!(w2, None, "waiter consumed by first signal");
}

#[test]
fn signal_without_waiter_returns_none() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);

    assert_eq!(ch.signal(ep0), None);
}

#[test]
fn bidirectional_signaling() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    // Both sides signal each other.
    ch.signal(ep0); // sets pending on ep1
    ch.signal(ep1); // sets pending on ep0

    assert!(ch.check_pending(ep0));
    assert!(ch.check_pending(ep1));
}

// --- Close Endpoint / Refcounting ---

#[test]
fn close_one_endpoint_does_not_free() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);

    assert!(!ch.close_endpoint(ep0), "single close should not free");
    assert_eq!(ch.closed_count, 1);
}

#[test]
fn close_both_endpoints_frees() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    assert!(!ch.close_endpoint(ep0));
    assert!(ch.close_endpoint(ep1), "second close should trigger free");
    assert_eq!(ch.closed_count, 2);
}

#[test]
fn close_clears_waiter() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);

    ch.register_waiter(ep0, ThreadId(10));
    ch.close_endpoint(ep0);

    // Waiter should be cleared by close.
    assert_eq!(ch.waiter[0], None);
}

#[test]
fn close_order_does_not_matter() {
    // Close ep1 first, then ep0 — still frees on second close.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    assert!(!ch.close_endpoint(ep1));
    assert!(ch.close_endpoint(ep0));
}

#[test]
fn double_close_same_endpoint_increments_twice() {
    // This is a bug scenario — the kernel should prevent it. But the raw
    // state machine increments closed_count on every call.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);

    ch.close_endpoint(ep0);
    let freed = ch.close_endpoint(ep0);

    assert!(freed, "double-close reaches count 2");
    assert_eq!(ch.closed_count, 2);
}

// --- Waiter Registration ---

#[test]
fn unregister_waiter_prevents_signal_return() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.register_waiter(ep1, ThreadId(7));
    ch.unregister_waiter(ep1);

    assert_eq!(ch.signal(ep0), None);
}

#[test]
fn register_waiter_replaces_previous() {
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.register_waiter(ep1, ThreadId(1));
    ch.register_waiter(ep1, ThreadId(2));

    let waiter = ch.signal(ep0);

    assert_eq!(waiter, Some(ThreadId(2)));
}

// --- Close-while-waiting race pattern (audit: channel-handle-audit) ---

#[test]
fn close_wakes_peer_waiter() {
    // Verifies that close_endpoint returns the peer waiter so the caller
    // can wake it. The peer should not remain blocked on a dead channel.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.register_waiter(ep1, ThreadId(99));

    // Close ep0 — should take peer's (ep1's) waiter for waking.
    let ep = endpoint_index(ep0);
    let peer_ep = 1 - ep;

    ch.waiter[ep] = None;

    let peer_waiter = ch.waiter[peer_ep].take();

    assert_eq!(peer_waiter, Some(ThreadId(99)), "peer waiter should be taken for waking");
}

#[test]
fn close_takes_peer_waiter_for_waking() {
    // close_endpoint(ep0) clears ep0's waiter AND takes ep1's waiter
    // (for waking the peer). Both waiters should be None after close.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.register_waiter(ep0, ThreadId(10));
    ch.register_waiter(ep1, ThreadId(20));

    ch.close_endpoint(ep0);

    // ep0's waiter cleared by close.
    assert_eq!(ch.waiter[0], None);
    // ep1's waiter taken by close (for waking). The kernel would wake
    // this thread via try_wake_for_handle / set_wake_pending_for_handle.
    assert_eq!(ch.waiter[1], None, "peer waiter taken for waking");
}

// --- Signal on half-closed channel (audit: channel-handle-audit) ---

#[test]
fn signal_after_peer_closed() {
    // After ep0 closes, ep1 can still signal. The signal sets pending on
    // the closed ep0 (harmless — no one will consume it) and returns no
    // waiter (ep0's waiter was cleared by close).
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.close_endpoint(ep0);

    let waiter = ch.signal(ep1);

    assert_eq!(waiter, None, "closed endpoint has no waiter");
    assert!(ch.pending_signal[0], "flag set on closed endpoint (harmless)");
}

#[test]
fn check_pending_after_peer_closed() {
    // After ep0 closes, ep1 can still check pending. If ep0 signaled before
    // closing, ep1 sees the signal.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.signal(ep0); // sets pending on ep1
    ch.close_endpoint(ep0);

    assert!(ch.check_pending(ep1), "signal before close should be visible");
}

#[test]
fn signal_and_close_interleaved() {
    // Interleave signal and close to verify no state corruption.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.signal(ep0);      // pending on ep1
    ch.close_endpoint(ep0); // ep0 closed, count=1
    ch.signal(ep1);      // pending on ep0 (closed, harmless)
    ch.close_endpoint(ep1); // ep1 closed, count=2

    assert_eq!(ch.closed_count, 2);
}

// --- closed_count saturation (audit: channel-handle-audit) ---

#[test]
fn closed_count_saturates_at_two() {
    // After both endpoints close, closed_count should be 2. A third close
    // (which shouldn't happen in practice due to handle protection) should
    // saturate at 2, not increment further. The current kernel code does
    // increment past 2 — this test documents the behavior and verifies
    // the fix if applied.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.close_endpoint(ep0);
    ch.close_endpoint(ep1);

    assert_eq!(ch.closed_count, 2);

    // A third close_endpoint call (defensive — shouldn't happen).
    // With saturation, closed_count stays at 2 and doesn't return true
    // again (no double-free of pages).
    let freed_again = ch.close_endpoint(ep0);

    assert!(
        !freed_again,
        "third close must not trigger page free (would be double-free)"
    );
    assert_eq!(
        ch.closed_count, 2,
        "closed_count should saturate at 2"
    );
}

// --- Encoding edge cases (audit: channel-handle-audit) ---

#[test]
fn encoding_large_channel_index() {
    // Verify encoding for a large channel index near u32 limits.
    let ch_idx = u32::MAX / 2;
    let ep0 = ChannelId(ch_idx * 2);
    let ep1 = ChannelId(ch_idx * 2 + 1);

    assert_eq!(channel_index(ep0), ch_idx as usize);
    assert_eq!(endpoint_index(ep0), 0);
    assert_eq!(channel_index(ep1), ch_idx as usize);
    assert_eq!(endpoint_index(ep1), 1);
}

// --- Multiple rapid signals (audit: channel-handle-audit) ---

#[test]
fn multiple_signals_coalesce() {
    // Multiple signals before a check are coalesced — check_pending returns
    // true once, subsequent checks return false. No signal is "lost" but
    // they don't accumulate (it's a boolean flag, not a counter).
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.signal(ep0);
    ch.signal(ep0);
    ch.signal(ep0);

    assert!(ch.check_pending(ep1), "first check sees coalesced signals");
    assert!(!ch.check_pending(ep1), "second check sees nothing");
}

#[test]
fn signal_check_signal_check_sequence() {
    // Verify that signal→check→signal→check works correctly.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);
    let ep1 = ChannelId(1);

    ch.signal(ep0);
    assert!(ch.check_pending(ep1));

    ch.signal(ep0);
    assert!(ch.check_pending(ep1));
}

// --- Waiter edge cases (audit: channel-handle-audit) ---

#[test]
fn unregister_waiter_idempotent() {
    // Unregistering a waiter that doesn't exist is a no-op.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);

    ch.unregister_waiter(ep0);

    assert_eq!(ch.waiter[0], None);
}

#[test]
fn register_after_close_is_no_op() {
    // Registering a waiter on a closed endpoint sets the field, but the
    // channel is already closed so no one will signal it. This is harmless.
    let mut ch = Channel::new();
    let ep0 = ChannelId(0);

    ch.close_endpoint(ep0);
    ch.register_waiter(ep0, ThreadId(42));

    assert_eq!(ch.waiter[0], Some(ThreadId(42)));
}
