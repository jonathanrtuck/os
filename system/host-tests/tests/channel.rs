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

    /// Mirrors kernel channel::close_endpoint (returns true if both closed).
    fn close_endpoint(&mut self, id: ChannelId) -> bool {
        let ep = endpoint_index(id);

        self.waiter[ep] = None;
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
