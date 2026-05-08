//! Endpoint — synchronous IPC with call/recv/reply protocol.
//!
//! Endpoints implement synchronous RPC: a client calls (blocks), the server
//! recvs the highest-priority call, processes it, and replies via a one-shot
//! reply cap.
//!
//! The send queue stores only caller ThreadIds — message data and handles
//! live in the caller's Thread struct (IpcCallState). This eliminates the
//! fixed send-queue depth limit (each entry is 4 bytes, not ~200) and the
//! TOCTOU window (message is copied into kernel memory at syscall entry).
//!
//! The queue uses per-priority ring buffers — 4 levels × 16 slots = 64
//! total. O(1) enqueue, O(4) dequeue (check each level).

use crate::{
    config,
    frame::ring::FixedRing,
    types::{EndpointId, EventId, Priority, SyscallError, ThreadId},
};

const IPC_BUCKETS: usize = 4;
const SLOTS_PER_BUCKET: usize = 16;

const IPC_BUCKET_REPRESENTATIVE: [Priority; IPC_BUCKETS] = [
    Priority::IDLE,
    Priority::LOW,
    Priority::NORMAL,
    Priority::HIGH,
];

struct PrioritySendQueue {
    rings: [FixedRing<ThreadId, SLOTS_PER_BUCKET>; IPC_BUCKETS],
    total: u16,
}

impl PrioritySendQueue {
    const fn new() -> Self {
        PrioritySendQueue {
            rings: [const { FixedRing::new() }; IPC_BUCKETS],
            total: 0,
        }
    }

    fn enqueue(&mut self, caller: ThreadId, priority: Priority) -> Result<(), SyscallError> {
        let bucket = priority.ipc_bucket();

        if !self.rings[bucket].push(caller) {
            return Err(SyscallError::BufferFull);
        }

        self.total += 1;

        Ok(())
    }

    fn dequeue_highest(&mut self) -> Option<ThreadId> {
        for bucket in (0..IPC_BUCKETS).rev() {
            if let Some(tid) = self.rings[bucket].pop() {
                self.total -= 1;

                return Some(tid);
            }
        }

        None
    }

    fn is_empty(&self) -> bool {
        self.total == 0
    }

    fn len(&self) -> usize {
        self.total as usize
    }

    fn highest_priority(&self) -> Option<Priority> {
        for bucket in (0..IPC_BUCKETS).rev() {
            if !self.rings[bucket].is_empty() {
                return Some(IPC_BUCKET_REPRESENTATIVE[bucket]);
            }
        }

        None
    }
}

/// Inline storage for drained recv waiters — no heap allocation on the IPC hot path.
#[derive(Debug)]
pub struct DrainList {
    items: [ThreadId; config::MAX_RECV_WAITERS],
    len: usize,
}

impl DrainList {
    pub fn as_slice(&self) -> &[ThreadId] {
        &self.items[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// IPC message size: one ARM64 cache line.
pub const MSG_SIZE: usize = 128;

/// A fixed-size IPC message payload.
#[derive(Clone, PartialEq, Eq)]
pub struct Message {
    data: [u8; MSG_SIZE],
    len: usize,
}

impl Message {
    pub fn empty() -> Self {
        Message {
            data: [0; MSG_SIZE],
            len: 0,
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SyscallError> {
        if bytes.len() > MSG_SIZE {
            return Err(SyscallError::InvalidArgument);
        }

        let mut msg = Self::empty();

        msg.data[..bytes.len()].copy_from_slice(bytes);
        msg.len = bytes.len();

        Ok(msg)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }

    pub fn data_mut(&mut self) -> &mut [u8; MSG_SIZE] {
        &mut self.data
    }

    pub fn set_len(&mut self, len: usize) {
        self.len = len.min(MSG_SIZE);
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl core::fmt::Debug for Message {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Message({} bytes)", self.len)
    }
}

/// A one-shot reply capability identifier.
///
/// Encodes `(nonce << SLOT_BITS) | slot_index` so that consume_reply can
/// extract the slot in O(1) instead of scanning all active replies. The
/// 60-bit nonce prevents stale cap IDs from matching after slot reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyCapId(pub u64);

const SLOT_BITS: u32 = 4;
const SLOT_MASK: u64 = (1 << SLOT_BITS) - 1;

const _: () = {
    assert!(config::MAX_ACTIVE_REPLIES <= (1 << SLOT_BITS));
};

/// Tracks a reply cap issued to a server, linking it to the blocked caller.
/// Message data and reply addresses are in the caller's Thread (IpcCallState).
#[derive(Clone, Copy)]
struct ActiveReply {
    cap_id: ReplyCapId,
    caller: ThreadId,
}

/// Result of closing an endpoint's peer end. Lists all threads that were
/// blocked on this endpoint and must be woken with PeerClosed.
pub struct CloseResult {
    send_callers: [ThreadId; SLOTS_PER_BUCKET * IPC_BUCKETS],
    send_caller_len: usize,
    reply_callers: [ThreadId; config::MAX_ACTIVE_REPLIES],
    reply_caller_len: usize,
    recv_waiters: [ThreadId; config::MAX_RECV_WAITERS],
    recv_waiter_len: usize,
}

impl CloseResult {
    fn new() -> Self {
        CloseResult {
            send_callers: [ThreadId(0); SLOTS_PER_BUCKET * IPC_BUCKETS],
            send_caller_len: 0,
            reply_callers: [ThreadId(0); config::MAX_ACTIVE_REPLIES],
            reply_caller_len: 0,
            recv_waiters: [ThreadId(0); config::MAX_RECV_WAITERS],
            recv_waiter_len: 0,
        }
    }

    pub fn send_callers(&self) -> &[ThreadId] {
        &self.send_callers[..self.send_caller_len]
    }

    pub fn reply_callers(&self) -> &[ThreadId] {
        &self.reply_callers[..self.reply_caller_len]
    }

    pub fn recv_waiters(&self) -> &[ThreadId] {
        &self.recv_waiters[..self.recv_waiter_len]
    }

    pub fn all_thread_ids(&self) -> impl Iterator<Item = ThreadId> + '_ {
        self.send_callers[..self.send_caller_len]
            .iter()
            .copied()
            .chain(self.reply_callers[..self.reply_caller_len].iter().copied())
            .chain(self.recv_waiters[..self.recv_waiter_len].iter().copied())
    }
}

/// A synchronous IPC endpoint.
///
/// Manages the call queue (pending calls from clients), active replies
/// (calls the server is processing), and recv waiters (servers waiting
/// for calls). Thread blocking/waking is the syscall layer's concern.
pub struct Endpoint {
    pub id: EndpointId,
    send_queue: PrioritySendQueue,
    active_replies: [Option<ActiveReply>; config::MAX_ACTIVE_REPLIES],
    active_reply_count: u8,
    recv_waiters: [Option<ThreadId>; config::MAX_RECV_WAITERS],
    recv_waiter_count: usize,
    next_reply_id: u64,
    badge_counter: u32,
    bound_event: Option<EventId>,
    active_server: Option<ThreadId>,
    peer_closed: bool,
    refcount: core::sync::atomic::AtomicUsize,
}

#[allow(clippy::new_without_default)]
impl Endpoint {
    pub fn new(id: EndpointId) -> Self {
        Endpoint {
            id,
            send_queue: PrioritySendQueue::new(),
            active_replies: [None; config::MAX_ACTIVE_REPLIES],
            active_reply_count: 0,
            recv_waiters: [None; config::MAX_RECV_WAITERS],
            recv_waiter_count: 0,
            next_reply_id: 0,
            badge_counter: 0,
            bound_event: None,
            active_server: None,
            peer_closed: false,
            refcount: core::sync::atomic::AtomicUsize::new(1),
        }
    }

    pub fn refcount(&self) -> usize {
        self.refcount.load(core::sync::atomic::Ordering::Relaxed)
    }

    pub fn add_ref(&self) {
        self.refcount
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }

    pub fn release_ref(&self) -> bool {
        let prev = self
            .refcount
            .fetch_sub(1, core::sync::atomic::Ordering::Release);

        assert!(prev > 0, "Endpoint refcount underflow");

        if prev == 1 {
            core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

            return true;
        }

        false
    }

    pub fn is_peer_closed(&self) -> bool {
        self.peer_closed
    }

    pub fn has_pending_calls(&self) -> bool {
        !self.send_queue.is_empty()
    }

    pub fn pending_call_count(&self) -> usize {
        self.send_queue.len()
    }

    pub fn is_full(&self) -> bool {
        self.send_queue.len() >= SLOTS_PER_BUCKET * IPC_BUCKETS
    }

    pub fn pending_reply_count(&self) -> usize {
        self.active_reply_count as usize
    }

    pub fn recv_waiter_count(&self) -> usize {
        self.recv_waiter_count
    }

    pub fn bound_event(&self) -> Option<EventId> {
        self.bound_event
    }

    pub fn active_server(&self) -> Option<ThreadId> {
        self.active_server
    }

    pub fn set_active_server(&mut self, server: Option<ThreadId>) {
        self.active_server = server;
    }

    /// Allocate the next unique badge value for this endpoint.
    pub fn next_badge(&mut self) -> u32 {
        let b = self.badge_counter;

        self.badge_counter = self.badge_counter.wrapping_add(1);

        b
    }

    pub const ENDPOINT_READABLE_BIT: u64 = 1;

    /// Enqueue a caller into the send queue.
    /// The caller's message and handles are in its Thread (IpcCallState).
    /// Returns `Ok(Some((event_id, bits)))` if a bound event should be signaled.
    pub fn enqueue_call(
        &mut self,
        caller: ThreadId,
        priority: Priority,
    ) -> Result<Option<(EventId, u64)>, SyscallError> {
        if self.peer_closed {
            return Err(SyscallError::PeerClosed);
        }

        self.send_queue.enqueue(caller, priority)?;

        Ok(self
            .bound_event
            .map(|eid| (eid, Self::ENDPOINT_READABLE_BIT)))
    }

    /// Dequeue the highest-priority pending caller and issue a reply cap.
    ///
    /// Returns `None` if the send queue is empty OR all reply slots are
    /// occupied (backpressure — the server must reply before receiving more).
    pub fn dequeue_caller(&mut self) -> Option<(ThreadId, ReplyCapId)> {
        let free_slot = self.active_replies.iter().position(|s| s.is_none())?;
        let caller = self.send_queue.dequeue_highest()?;
        let cap_id = ReplyCapId((self.next_reply_id << SLOT_BITS) | (free_slot as u64));

        self.next_reply_id = self.next_reply_id.wrapping_add(1);
        self.active_replies[free_slot] = Some(ActiveReply { cap_id, caller });
        self.active_reply_count += 1;

        Some((caller, cap_id))
    }

    /// Consume a reply cap, returning the blocked caller's ThreadId.
    /// O(1) via the slot index encoded in the cap ID.
    pub fn consume_reply(&mut self, cap_id: ReplyCapId) -> Result<ThreadId, SyscallError> {
        let slot_idx = (cap_id.0 & SLOT_MASK) as usize;

        if slot_idx >= config::MAX_ACTIVE_REPLIES {
            return Err(SyscallError::InvalidHandle);
        }

        let slot = &mut self.active_replies[slot_idx];

        if let Some(r) = slot
            && r.cap_id == cap_id
        {
            let caller = r.caller;

            *slot = None;

            self.active_reply_count -= 1;

            Ok(caller)
        } else {
            Err(SyscallError::InvalidHandle)
        }
    }

    /// Highest priority among pending callers (for priority inheritance).
    pub fn highest_caller_priority(&self) -> Option<Priority> {
        self.send_queue.highest_priority()
    }

    /// Return the first active reply cap, if any. Used by kernel-side IPC
    /// benchmarks that drive CALL/RECV/REPLY without a userspace pointer for
    /// `reply_cap_out`. In production, the cap reaches the server via user
    /// memory; this lets the bench reach in and recover it directly.
    pub fn first_active_reply_cap(&self) -> Option<ReplyCapId> {
        self.active_replies
            .iter()
            .find_map(|slot| slot.as_ref().map(|r| r.cap_id))
    }

    /// Add a server thread to the recv waiters list.
    pub fn add_recv_waiter(&mut self, thread: ThreadId) -> Result<(), SyscallError> {
        if self.peer_closed {
            return Err(SyscallError::PeerClosed);
        }

        for slot in &mut self.recv_waiters {
            if slot.is_none() {
                *slot = Some(thread);
                self.recv_waiter_count += 1;

                return Ok(());
            }
        }

        Err(SyscallError::BufferFull)
    }

    /// Pop the first recv waiter for IPC direct transfer.
    pub fn pop_recv_waiter(&mut self) -> Option<ThreadId> {
        for slot in &mut self.recv_waiters {
            if let Some(tid) = slot.take() {
                self.recv_waiter_count -= 1;

                return Some(tid);
            }
        }

        None
    }

    /// Allocate a reply cap without dequeuing from the send queue.
    /// Used by CALL's direct-transfer fast path.
    pub fn allocate_reply_cap(&mut self, caller: ThreadId) -> Option<ReplyCapId> {
        let free_slot = self.active_replies.iter().position(|s| s.is_none())?;
        let cap_id = ReplyCapId((self.next_reply_id << SLOT_BITS) | (free_slot as u64));

        self.next_reply_id = self.next_reply_id.wrapping_add(1);
        self.active_replies[free_slot] = Some(ActiveReply { cap_id, caller });
        self.active_reply_count += 1;

        Some(cap_id)
    }

    /// Remove a recv waiter (on timeout or cancel).
    pub fn remove_recv_waiter(&mut self, thread: ThreadId) -> bool {
        for slot in &mut self.recv_waiters {
            if *slot == Some(thread) {
                *slot = None;
                self.recv_waiter_count -= 1;

                return true;
            }
        }

        false
    }

    /// Drain all recv waiters (for wakeup when a call arrives). No heap allocation.
    pub fn drain_recv_waiters(&mut self) -> DrainList {
        let mut list = DrainList {
            items: [ThreadId(0); config::MAX_RECV_WAITERS],
            len: 0,
        };

        for slot in &mut self.recv_waiters {
            if let Some(tid) = slot.take() {
                list.items[list.len] = tid;
                list.len += 1;
            }
        }

        self.recv_waiter_count = 0;

        list
    }

    /// Close the peer end. Returns all blocked thread IDs grouped by category.
    /// Handle recovery from send callers is the syscall layer's concern —
    /// handles are in each thread's IpcCallState.
    pub fn close_peer(&mut self) -> Option<CloseResult> {
        self.peer_closed = true;

        if self.send_queue.is_empty() && self.active_reply_count == 0 && self.recv_waiter_count == 0
        {
            return None;
        }

        let mut result = CloseResult::new();

        for level in 0..IPC_BUCKETS {
            while let Some(tid) = self.send_queue.rings[level].pop() {
                if result.send_caller_len < result.send_callers.len() {
                    result.send_callers[result.send_caller_len] = tid;
                    result.send_caller_len += 1;
                }
            }
        }

        self.send_queue.total = 0;

        for slot in &mut self.active_replies {
            if let Some(reply) = slot.take()
                && result.reply_caller_len < config::MAX_ACTIVE_REPLIES
            {
                result.reply_callers[result.reply_caller_len] = reply.caller;
                result.reply_caller_len += 1;
            }
        }

        self.active_reply_count = 0;

        for slot in &mut self.recv_waiters {
            if let Some(tid) = slot.take()
                && result.recv_waiter_len < config::MAX_RECV_WAITERS
            {
                result.recv_waiters[result.recv_waiter_len] = tid;
                result.recv_waiter_len += 1;
            }
        }

        self.recv_waiter_count = 0;

        Some(result)
    }

    /// Bind an event to this endpoint (for channel-event integration).
    pub fn bind_event(&mut self, event: EventId) -> Result<(), SyscallError> {
        if self.bound_event.is_some() {
            return Err(SyscallError::InvalidArgument);
        }

        self.bound_event = Some(event);

        Ok(())
    }

    /// Unbind the event.
    pub fn unbind_event(&mut self) {
        self.bound_event = None;
    }

    #[cfg(any(test, fuzzing, debug_assertions))]
    pub fn verify_internal_counts(&self) -> Result<(), &'static str> {
        let actual_active = self.active_replies.iter().filter(|s| s.is_some()).count();

        if actual_active != self.active_reply_count as usize {
            return Err("active_reply_count mismatch");
        }

        let actual_recv = self.recv_waiters.iter().filter(|s| s.is_some()).count();

        if actual_recv != self.recv_waiter_count {
            return Err("recv_waiter_count mismatch");
        }

        let ring_sum: usize = self.send_queue.rings.iter().map(|r| r.len()).sum();

        if ring_sum != self.send_queue.total as usize {
            return Err("send_queue total mismatch");
        }

        Ok(())
    }

    #[cfg(any(test, fuzzing, debug_assertions))]
    pub fn all_caller_thread_ids(&self) -> alloc::vec::Vec<crate::types::ThreadId> {
        let mut ids = alloc::vec::Vec::new();

        for ring in &self.send_queue.rings {
            for &tid in ring.iter() {
                ids.push(tid);
            }
        }

        for r in self.active_replies.iter().flatten() {
            ids.push(r.caller);
        }

        ids
    }

    #[cfg(any(test, fuzzing, debug_assertions))]
    pub fn all_recv_waiter_ids(&self) -> alloc::vec::Vec<crate::types::ThreadId> {
        self.recv_waiters.iter().filter_map(|s| *s).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_struct_size() {
        assert!(
            core::mem::size_of::<Endpoint>() <= 768,
            "Endpoint struct grew beyond 768 bytes: {}",
            core::mem::size_of::<Endpoint>()
        );
    }

    fn make_endpoint(id: u32) -> Endpoint {
        Endpoint::new(EndpointId(id))
    }

    fn enqueue(ep: &mut Endpoint, caller: u32, priority: Priority) {
        ep.enqueue_call(ThreadId(caller), priority).unwrap();
    }

    // -- Protocol roundtrip --

    #[test]
    fn call_recv_reply_roundtrip() {
        let mut ep = make_endpoint(0);

        enqueue(&mut ep, 1, Priority::Medium);

        let (caller, reply_cap) = ep.dequeue_caller().unwrap();

        assert_eq!(caller, ThreadId(1));

        let resolved = ep.consume_reply(reply_cap).unwrap();

        assert_eq!(resolved, ThreadId(1));
        assert_eq!(ep.pending_reply_count(), 0);
    }

    // -- Priority ordering --

    #[test]
    fn many_to_one_priority_ordering() {
        let mut ep = make_endpoint(0);

        enqueue(&mut ep, 1, Priority::Low);
        enqueue(&mut ep, 2, Priority::High);
        enqueue(&mut ep, 3, Priority::Medium);

        let (first, _) = ep.dequeue_caller().unwrap();

        assert_eq!(first, ThreadId(2));

        let (second, _) = ep.dequeue_caller().unwrap();

        assert_eq!(second, ThreadId(3));

        let (third, _) = ep.dequeue_caller().unwrap();

        assert_eq!(third, ThreadId(1));
    }

    #[test]
    fn highest_caller_priority_tracks_queue() {
        let mut ep = make_endpoint(0);

        assert!(ep.highest_caller_priority().is_none());

        enqueue(&mut ep, 1, Priority::Low);

        assert_eq!(ep.highest_caller_priority(), Some(Priority::Low));

        enqueue(&mut ep, 2, Priority::High);

        assert_eq!(ep.highest_caller_priority(), Some(Priority::High));
    }

    // -- Reply cap one-shot --

    #[test]
    fn reply_cap_consumed_once() {
        let mut ep = make_endpoint(0);

        enqueue(&mut ep, 1, Priority::Medium);

        let (_, cap) = ep.dequeue_caller().unwrap();

        assert!(ep.consume_reply(cap).is_ok());
        assert_eq!(ep.consume_reply(cap), Err(SyscallError::InvalidHandle));
    }

    // -- Peer closed --

    #[test]
    fn peer_closed_unblocks_all() {
        let mut ep = make_endpoint(0);

        enqueue(&mut ep, 1, Priority::Medium);
        enqueue(&mut ep, 2, Priority::Low);
        enqueue(&mut ep, 3, Priority::High);

        ep.dequeue_caller().unwrap();
        ep.add_recv_waiter(ThreadId(10)).unwrap();

        let result = ep.close_peer().unwrap();
        let all_ids: alloc::vec::Vec<_> = result.all_thread_ids().collect();

        assert_eq!(all_ids.len(), 4);
        assert!(all_ids.contains(&ThreadId(1)));
        assert!(all_ids.contains(&ThreadId(2)));
        assert!(all_ids.contains(&ThreadId(10)));
    }

    #[test]
    fn enqueue_on_closed_endpoint() {
        let mut ep = make_endpoint(0);

        ep.close_peer();

        assert_eq!(
            ep.enqueue_call(ThreadId(1), Priority::Medium),
            Err(SyscallError::PeerClosed)
        );
    }

    #[test]
    fn recv_waiter_on_closed_endpoint() {
        let mut ep = make_endpoint(0);

        ep.close_peer();

        assert_eq!(
            ep.add_recv_waiter(ThreadId(1)),
            Err(SyscallError::PeerClosed)
        );
    }

    // -- Queue management --

    #[test]
    fn dequeue_empty_returns_none() {
        let mut ep = make_endpoint(0);

        assert!(ep.dequeue_caller().is_none());
    }

    #[test]
    fn send_queue_exhaustion() {
        let mut ep = make_endpoint(0);
        let priorities = [
            Priority::Idle,
            Priority::Low,
            Priority::Medium,
            Priority::High,
        ];
        let total = SLOTS_PER_BUCKET * IPC_BUCKETS;

        for (i, &pri) in (0..total).zip(priorities.iter().cycle()) {
            enqueue(&mut ep, i as u32, pri);
        }

        assert_eq!(
            ep.enqueue_call(ThreadId(999), Priority::Medium),
            Err(SyscallError::BufferFull)
        );
    }

    #[test]
    fn per_priority_ring_exhaustion() {
        let mut ep = make_endpoint(0);

        for i in 0..SLOTS_PER_BUCKET {
            enqueue(&mut ep, i as u32, Priority::Medium);
        }

        assert_eq!(
            ep.enqueue_call(ThreadId(999), Priority::Medium),
            Err(SyscallError::BufferFull)
        );

        enqueue(&mut ep, 100, Priority::High);

        assert_eq!(ep.pending_call_count(), SLOTS_PER_BUCKET + 1);
    }

    // -- Recv waiters --

    #[test]
    fn recv_waiter_lifecycle() {
        let mut ep = make_endpoint(0);

        ep.add_recv_waiter(ThreadId(1)).unwrap();
        ep.add_recv_waiter(ThreadId(2)).unwrap();

        assert_eq!(ep.recv_waiter_count(), 2);
        assert!(ep.remove_recv_waiter(ThreadId(1)));
        assert_eq!(ep.recv_waiter_count(), 1);
        assert!(!ep.remove_recv_waiter(ThreadId(1)));

        let waiters = ep.drain_recv_waiters();

        assert_eq!(waiters.len(), 1);
        assert_eq!(waiters.as_slice()[0], ThreadId(2));
        assert_eq!(ep.recv_waiter_count(), 0);
    }

    #[test]
    fn recv_waiter_exhaustion() {
        let mut ep = make_endpoint(0);

        for i in 0..config::MAX_RECV_WAITERS {
            ep.add_recv_waiter(ThreadId(i as u32)).unwrap();
        }

        assert_eq!(
            ep.add_recv_waiter(ThreadId(999)),
            Err(SyscallError::BufferFull)
        );
    }

    #[test]
    fn drain_recv_waiters_after_mixed_add_remove() {
        let mut ep = make_endpoint(0);

        ep.add_recv_waiter(ThreadId(1)).unwrap();
        ep.add_recv_waiter(ThreadId(2)).unwrap();
        ep.add_recv_waiter(ThreadId(3)).unwrap();
        ep.remove_recv_waiter(ThreadId(2));

        let drained = ep.drain_recv_waiters();

        assert_eq!(drained.len(), 2);
        assert!(drained.as_slice().contains(&ThreadId(1)));
        assert!(drained.as_slice().contains(&ThreadId(3)));
    }

    // -- Badge and event --

    #[test]
    fn badge_counter_increments() {
        let mut ep = make_endpoint(0);

        assert_eq!(ep.next_badge(), 0);
        assert_eq!(ep.next_badge(), 1);
        assert_eq!(ep.next_badge(), 2);
    }

    #[test]
    fn bind_event() {
        let mut ep = make_endpoint(0);

        ep.bind_event(EventId(5)).unwrap();

        assert_eq!(ep.bound_event(), Some(EventId(5)));
        assert_eq!(
            ep.bind_event(EventId(6)),
            Err(SyscallError::InvalidArgument)
        );

        ep.unbind_event();

        assert!(ep.bound_event().is_none());
    }

    // -- Message --

    #[test]
    fn message_roundtrip() {
        let msg = Message::from_bytes(b"test data").unwrap();

        assert_eq!(msg.as_bytes(), b"test data");
    }

    #[test]
    fn message_too_large() {
        let big = [0u8; MSG_SIZE + 1];

        assert_eq!(
            Message::from_bytes(&big),
            Err(SyscallError::InvalidArgument)
        );
    }

    // -- Adversarial / boundary tests --

    #[test]
    fn dequeue_blocked_by_full_active_replies() {
        let mut ep = make_endpoint(0);
        let priorities = [
            Priority::Idle,
            Priority::Low,
            Priority::Medium,
            Priority::High,
        ];

        for (i, &pri) in (0..config::MAX_ACTIVE_REPLIES).zip(priorities.iter().cycle()) {
            enqueue(&mut ep, i as u32, pri);
        }

        for _ in 0..config::MAX_ACTIVE_REPLIES {
            assert!(ep.dequeue_caller().is_some());
        }

        assert_eq!(ep.pending_reply_count(), config::MAX_ACTIVE_REPLIES);

        enqueue(&mut ep, 100, Priority::Medium);

        assert!(ep.has_pending_calls());
        assert!(ep.dequeue_caller().is_none());
    }

    #[test]
    fn consume_reply_invalid_cap_id() {
        let mut ep = make_endpoint(0);

        assert_eq!(
            ep.consume_reply(ReplyCapId(0)),
            Err(SyscallError::InvalidHandle)
        );
        assert_eq!(
            ep.consume_reply(ReplyCapId(u64::MAX)),
            Err(SyscallError::InvalidHandle)
        );

        enqueue(&mut ep, 1, Priority::Medium);

        let (_, valid_cap) = ep.dequeue_caller().unwrap();
        let bogus_cap = ReplyCapId(valid_cap.0.wrapping_add(1));

        assert_eq!(
            ep.consume_reply(bogus_cap),
            Err(SyscallError::InvalidHandle)
        );
        assert!(ep.consume_reply(valid_cap).is_ok());
    }

    #[test]
    fn next_reply_id_wraparound() {
        let mut ep = make_endpoint(0);

        ep.next_reply_id = u64::MAX - 1;

        enqueue(&mut ep, 1, Priority::Low);
        enqueue(&mut ep, 2, Priority::Medium);
        enqueue(&mut ep, 3, Priority::High);

        let (_, cap_a) = ep.dequeue_caller().unwrap();
        let (_, cap_b) = ep.dequeue_caller().unwrap();
        let (_, cap_c) = ep.dequeue_caller().unwrap();

        assert_ne!(cap_a, cap_b);
        assert_ne!(cap_b, cap_c);
        assert_ne!(cap_a, cap_c);

        assert_eq!(ep.consume_reply(cap_a).unwrap(), ThreadId(3));
        assert_eq!(ep.consume_reply(cap_b).unwrap(), ThreadId(2));
        assert_eq!(ep.consume_reply(cap_c).unwrap(), ThreadId(1));
    }

    #[test]
    fn message_set_len_clamps_to_msg_size() {
        let mut msg = Message::empty();

        msg.set_len(MSG_SIZE + 100);

        assert_eq!(msg.len(), MSG_SIZE);
    }

    #[test]
    fn message_set_len_zero() {
        let mut msg = Message::from_bytes(b"some data").unwrap();

        assert_eq!(msg.len(), 9);

        msg.set_len(0);

        assert_eq!(msg.len(), 0);
        assert!(msg.is_empty());
        assert_eq!(msg.as_bytes(), &[]);
    }

    #[test]
    fn enqueue_call_peer_closed() {
        let mut ep = make_endpoint(0);

        ep.close_peer();

        assert_eq!(
            ep.enqueue_call(ThreadId(1), Priority::High),
            Err(SyscallError::PeerClosed)
        );
    }

    #[test]
    fn send_queue_full_single_priority() {
        let mut ep = make_endpoint(0);

        for i in 0..SLOTS_PER_BUCKET {
            enqueue(&mut ep, i as u32, Priority::High);
        }

        assert_eq!(
            ep.enqueue_call(ThreadId(99), Priority::High),
            Err(SyscallError::BufferFull)
        );

        enqueue(&mut ep, 100, Priority::Low);
    }

    #[test]
    fn close_peer_returns_all_blocked_ids() {
        let mut ep = make_endpoint(0);

        enqueue(&mut ep, 1, Priority::Low);
        enqueue(&mut ep, 2, Priority::High);

        let _ = ep.dequeue_caller().unwrap();

        enqueue(&mut ep, 3, Priority::Medium);

        ep.add_recv_waiter(ThreadId(10)).unwrap();
        ep.add_recv_waiter(ThreadId(11)).unwrap();

        let result = ep.close_peer().unwrap();
        let all_ids: alloc::vec::Vec<_> = result.all_thread_ids().collect();

        assert_eq!(all_ids.len(), 5);
        assert!(all_ids.contains(&ThreadId(1)));
        assert!(all_ids.contains(&ThreadId(2)));
        assert!(all_ids.contains(&ThreadId(3)));
        assert!(all_ids.contains(&ThreadId(10)));
        assert!(all_ids.contains(&ThreadId(11)));
        assert_eq!(result.send_callers().len(), 2);
        assert_eq!(result.reply_callers().len(), 1);
        assert_eq!(result.reply_callers()[0], ThreadId(2));
        assert_eq!(result.recv_waiters().len(), 2);
    }

    #[test]
    fn reply_cap_id_skips_active_collision_on_wraparound() {
        let mut ep = make_endpoint(0);

        ep.next_reply_id = u64::MAX - 1;

        let priorities = [Priority::Low, Priority::Medium, Priority::High];

        for (i, &pri) in (0..3u32).zip(priorities.iter()) {
            enqueue(&mut ep, i, pri);
        }

        let (_, cap0) = ep.dequeue_caller().unwrap();
        let (_, cap1) = ep.dequeue_caller().unwrap();
        let (_, cap2) = ep.dequeue_caller().unwrap();

        assert_ne!(cap0, cap1);
        assert_ne!(cap1, cap2);
        assert_ne!(cap0, cap2);
        assert_eq!(ep.consume_reply(cap0).unwrap(), ThreadId(2));
        assert_eq!(ep.consume_reply(cap1).unwrap(), ThreadId(1));
        assert_eq!(ep.consume_reply(cap2).unwrap(), ThreadId(0));
    }

    #[test]
    fn all_caller_thread_ids_after_wraparound() {
        let mut ep = make_endpoint(0);

        enqueue(&mut ep, 1, Priority::Medium);
        enqueue(&mut ep, 2, Priority::Medium);

        ep.dequeue_caller().unwrap();
        ep.dequeue_caller().unwrap();

        enqueue(&mut ep, 3, Priority::Medium);
        enqueue(&mut ep, 4, Priority::Medium);

        let ids = ep.all_caller_thread_ids();

        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&ThreadId(3)));
        assert!(ids.contains(&ThreadId(4)));
    }
}
