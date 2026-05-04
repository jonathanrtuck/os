//! Endpoint — synchronous IPC with call/recv/reply protocol.
//!
//! Endpoints implement synchronous RPC: a client calls (blocks), the server
//! recvs the highest-priority call, processes it, and replies via a one-shot
//! reply cap. Handle transfer is staged in the PendingCall and installed by
//! the syscall layer.
//!
//! The send queue uses per-priority inline ring buffers — 4 levels × 4 slots
//! = 16 total. O(1) enqueue, O(4) dequeue (check each level). No heap
//! allocation on the IPC path.

use alloc::vec::Vec;

use crate::{
    config,
    handle::Handle,
    types::{EndpointId, EventId, Priority, SyscallError, ThreadId},
};

const NUM_PRIORITY_LEVELS: usize = 4;
const SLOTS_PER_PRIORITY: usize = config::MAX_PENDING_PER_ENDPOINT / NUM_PRIORITY_LEVELS;

struct PriorityRing {
    slots: [Option<PendingCall>; SLOTS_PER_PRIORITY],
    head: u8,
    len: u8,
}

impl PriorityRing {
    const fn new() -> Self {
        PriorityRing {
            slots: [const { None }; SLOTS_PER_PRIORITY],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, item: PendingCall) -> Result<(), SyscallError> {
        if self.len as usize >= SLOTS_PER_PRIORITY {
            return Err(SyscallError::BufferFull);
        }

        let tail = (self.head as usize + self.len as usize) % SLOTS_PER_PRIORITY;
        self.slots[tail] = Some(item);
        self.len += 1;

        Ok(())
    }

    fn pop(&mut self) -> Option<PendingCall> {
        if self.len == 0 {
            return None;
        }

        let item = self.slots[self.head as usize].take();
        self.head = ((self.head as usize + 1) % SLOTS_PER_PRIORITY) as u8;
        self.len -= 1;

        item
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

struct PrioritySendQueue {
    rings: [PriorityRing; NUM_PRIORITY_LEVELS],
    total: u16,
}

impl PrioritySendQueue {
    const fn new() -> Self {
        PrioritySendQueue {
            rings: [const { PriorityRing::new() }; NUM_PRIORITY_LEVELS],
            total: 0,
        }
    }

    fn enqueue(&mut self, call: PendingCall) -> Result<(), SyscallError> {
        let level = call.priority as usize;
        self.rings[level].push(call)?;
        self.total += 1;

        Ok(())
    }

    fn dequeue_highest(&mut self) -> Option<PendingCall> {
        for level in (0..NUM_PRIORITY_LEVELS).rev() {
            if let Some(call) = self.rings[level].pop() {
                self.total -= 1;
                return Some(call);
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
        for level in (0..NUM_PRIORITY_LEVELS).rev() {
            if !self.rings[level].is_empty() {
                return match level {
                    3 => Some(Priority::High),
                    2 => Some(Priority::Medium),
                    1 => Some(Priority::Low),
                    0 => Some(Priority::Idle),
                    _ => None,
                };
            }
        }

        None
    }

    fn drain_callers(&mut self, out: &mut impl FnMut(ThreadId)) {
        for level in 0..NUM_PRIORITY_LEVELS {
            while let Some(call) = self.rings[level].pop() {
                out(call.caller);
            }
        }
        self.total = 0;
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
        debug_assert!(len <= MSG_SIZE);

        self.len = len;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyCapId(pub u32);

/// A pending call waiting in the send queue.
#[derive(Debug)]
pub struct PendingCall {
    pub caller: ThreadId,
    pub priority: Priority,
    pub message: Message,
    pub handles: [Option<Handle>; config::MAX_IPC_HANDLES],
    pub handle_count: u8,
    pub badge: u32,
    pub reply_buf: usize,
}

/// Tracks a reply cap issued to a server, linking it to the blocked caller.
struct ActiveReply {
    cap_id: ReplyCapId,
    caller: ThreadId,
    reply_buf: usize,
}

/// A synchronous IPC endpoint.
///
/// Manages the call queue (pending calls from clients), active replies
/// (calls the server is processing), and recv waiters (servers waiting
/// for calls). Thread blocking/waking is the syscall layer's concern.
pub struct Endpoint {
    pub id: EndpointId,
    send_queue: PrioritySendQueue,
    active_replies: Vec<ActiveReply>,
    recv_waiters: [Option<ThreadId>; config::MAX_RECV_WAITERS],
    recv_waiter_count: usize,
    next_reply_id: u32,
    badge_counter: u32,
    bound_event: Option<EventId>,
    peer_closed: bool,
}

#[allow(clippy::new_without_default)]
impl Endpoint {
    pub fn new(id: EndpointId) -> Self {
        Endpoint {
            id,
            send_queue: PrioritySendQueue::new(),
            active_replies: Vec::with_capacity(4),
            recv_waiters: [None; config::MAX_RECV_WAITERS],
            recv_waiter_count: 0,
            next_reply_id: 0,
            badge_counter: 0,
            bound_event: None,
            peer_closed: false,
        }
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
        self.send_queue.len() >= config::MAX_PENDING_PER_ENDPOINT
    }

    pub fn pending_reply_count(&self) -> usize {
        self.active_replies.len()
    }

    pub fn recv_waiter_count(&self) -> usize {
        self.recv_waiter_count
    }

    pub fn bound_event(&self) -> Option<EventId> {
        self.bound_event
    }

    /// Allocate the next unique badge value for this endpoint.
    pub fn next_badge(&mut self) -> u32 {
        let b = self.badge_counter;

        self.badge_counter += 1;

        b
    }

    pub const ENDPOINT_READABLE_BIT: u64 = 1;

    /// Enqueue a call into the send queue.
    /// Returns `Ok(Some((event_id, bits)))` if a bound event should be signaled.
    pub fn enqueue_call(
        &mut self,
        call: PendingCall,
    ) -> Result<Option<(EventId, u64)>, SyscallError> {
        if self.peer_closed {
            return Err(SyscallError::PeerClosed);
        }

        self.send_queue.enqueue(call)?;

        Ok(self
            .bound_event
            .map(|eid| (eid, Self::ENDPOINT_READABLE_BIT)))
    }

    /// Dequeue the highest-priority pending call and issue a reply cap.
    pub fn dequeue_call(&mut self) -> Option<(PendingCall, ReplyCapId)> {
        let call = self.send_queue.dequeue_highest()?;
        let cap_id = ReplyCapId(self.next_reply_id);

        self.next_reply_id += 1;

        self.active_replies.push(ActiveReply {
            cap_id,
            caller: call.caller,
            reply_buf: call.reply_buf,
        });

        Some((call, cap_id))
    }

    /// Consume a reply cap, returning (caller_thread_id, caller_reply_buf).
    pub fn consume_reply(&mut self, cap_id: ReplyCapId) -> Result<(ThreadId, usize), SyscallError> {
        let pos = self
            .active_replies
            .iter()
            .position(|r| r.cap_id == cap_id)
            .ok_or(SyscallError::InvalidHandle)?;
        let reply = self.active_replies.swap_remove(pos);

        Ok((reply.caller, reply.reply_buf))
    }

    /// Highest priority among pending callers (for priority inheritance).
    pub fn highest_caller_priority(&self) -> Option<Priority> {
        self.send_queue.highest_priority()
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

    /// Close the peer end. Returns all blocked thread IDs:
    /// callers in send queue + callers awaiting reply + recv waiters.
    pub fn close_peer(&mut self) -> Vec<ThreadId> {
        self.peer_closed = true;

        let mut blocked = Vec::new();

        self.send_queue.drain_callers(&mut |tid| blocked.push(tid));
        for reply in self.active_replies.drain(..) {
            blocked.push(reply.caller);
        }
        for slot in &mut self.recv_waiters {
            if let Some(tid) = slot.take() {
                blocked.push(tid);
            }
        }

        self.recv_waiter_count = 0;

        blocked
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

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ObjectType, Rights};

    fn make_endpoint(id: u32) -> Endpoint {
        Endpoint::new(EndpointId(id))
    }

    fn make_call(caller: u32, priority: Priority, badge: u32) -> PendingCall {
        PendingCall {
            caller: ThreadId(caller),
            priority,
            message: Message::from_bytes(b"hello").unwrap(),
            handles: [const { None }; config::MAX_IPC_HANDLES],
            handle_count: 0,
            badge,
            reply_buf: 0,
        }
    }

    fn make_handle(obj_id: u32) -> Handle {
        Handle {
            object_type: ObjectType::Vmo,
            object_id: obj_id,
            rights: Rights::READ,
            generation: 0,
            badge: 0,
        }
    }

    // -- Protocol roundtrip --

    #[test]
    fn call_recv_reply_roundtrip() {
        let mut ep = make_endpoint(0);
        let call = PendingCall {
            caller: ThreadId(1),
            priority: Priority::Medium,
            message: Message::from_bytes(b"request").unwrap(),
            handles: [const { None }; config::MAX_IPC_HANDLES],
            handle_count: 0,
            badge: 42,
            reply_buf: 0,
        };

        ep.enqueue_call(call).unwrap();

        let (received, reply_cap) = ep.dequeue_call().unwrap();

        assert_eq!(received.caller, ThreadId(1));
        assert_eq!(received.badge, 42);
        assert_eq!(received.message.as_bytes(), b"request");

        let (caller, _reply_buf) = ep.consume_reply(reply_cap).unwrap();

        assert_eq!(caller, ThreadId(1));
        assert_eq!(ep.pending_reply_count(), 0);
    }

    #[test]
    fn handle_transfer_staged_in_call() {
        let mut ep = make_endpoint(0);
        let mut handles = [const { None }; config::MAX_IPC_HANDLES];

        handles[0] = Some(make_handle(99));
        handles[1] = Some(make_handle(100));

        let call = PendingCall {
            caller: ThreadId(1),
            priority: Priority::Medium,
            message: Message::empty(),
            handles,
            handle_count: 2,
            badge: 0,
            reply_buf: 0,
        };

        ep.enqueue_call(call).unwrap();

        let (received, _) = ep.dequeue_call().unwrap();

        assert_eq!(received.handle_count, 2);
        assert_eq!(received.handles[0].as_ref().unwrap().object_id, 99);
        assert_eq!(received.handles[1].as_ref().unwrap().object_id, 100);
    }

    // -- Priority ordering --

    #[test]
    fn many_to_one_priority_ordering() {
        let mut ep = make_endpoint(0);

        ep.enqueue_call(make_call(1, Priority::Low, 10)).unwrap();
        ep.enqueue_call(make_call(2, Priority::High, 20)).unwrap();
        ep.enqueue_call(make_call(3, Priority::Medium, 30)).unwrap();

        let (first, _) = ep.dequeue_call().unwrap();

        assert_eq!(first.caller, ThreadId(2));

        let (second, _) = ep.dequeue_call().unwrap();

        assert_eq!(second.caller, ThreadId(3));

        let (third, _) = ep.dequeue_call().unwrap();

        assert_eq!(third.caller, ThreadId(1));
    }

    #[test]
    fn highest_caller_priority_tracks_queue() {
        let mut ep = make_endpoint(0);

        assert!(ep.highest_caller_priority().is_none());

        ep.enqueue_call(make_call(1, Priority::Low, 0)).unwrap();

        assert_eq!(ep.highest_caller_priority(), Some(Priority::Low));

        ep.enqueue_call(make_call(2, Priority::High, 0)).unwrap();

        assert_eq!(ep.highest_caller_priority(), Some(Priority::High));
    }

    // -- Reply cap one-shot --

    #[test]
    fn reply_cap_consumed_once() {
        let mut ep = make_endpoint(0);

        ep.enqueue_call(make_call(1, Priority::Medium, 0)).unwrap();

        let (_, cap) = ep.dequeue_call().unwrap();

        assert!(ep.consume_reply(cap).is_ok());
        assert_eq!(ep.consume_reply(cap), Err(SyscallError::InvalidHandle));
    }

    // -- Peer closed --

    #[test]
    fn peer_closed_unblocks_all() {
        let mut ep = make_endpoint(0);

        ep.enqueue_call(make_call(1, Priority::Medium, 0)).unwrap();
        ep.enqueue_call(make_call(2, Priority::Medium, 0)).unwrap();
        ep.enqueue_call(make_call(3, Priority::Medium, 0)).unwrap();
        ep.dequeue_call().unwrap(); // one call moves to active_replies
        ep.add_recv_waiter(ThreadId(10)).unwrap();

        let blocked = ep.close_peer();

        assert_eq!(blocked.len(), 4);
        assert!(blocked.contains(&ThreadId(1)));
        assert!(blocked.contains(&ThreadId(2)));
        assert!(blocked.contains(&ThreadId(10)));
    }

    #[test]
    fn enqueue_on_closed_endpoint() {
        let mut ep = make_endpoint(0);

        ep.close_peer();

        assert_eq!(
            ep.enqueue_call(make_call(1, Priority::Medium, 0)),
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

        assert!(ep.dequeue_call().is_none());
    }

    #[test]
    fn send_queue_exhaustion() {
        let mut ep = make_endpoint(0);
        let priorities = [Priority::Idle, Priority::Low, Priority::Medium, Priority::High];

        for (i, &pri) in (0..config::MAX_PENDING_PER_ENDPOINT)
            .zip(priorities.iter().cycle())
        {
            ep.enqueue_call(make_call(i as u32, pri, 0)).unwrap();
        }

        assert_eq!(
            ep.enqueue_call(make_call(999, Priority::Medium, 0)),
            Err(SyscallError::BufferFull)
        );
    }

    #[test]
    fn per_priority_ring_exhaustion() {
        let mut ep = make_endpoint(0);

        for i in 0..SLOTS_PER_PRIORITY {
            ep.enqueue_call(make_call(i as u32, Priority::Medium, 0))
                .unwrap();
        }

        assert_eq!(
            ep.enqueue_call(make_call(999, Priority::Medium, 0)),
            Err(SyscallError::BufferFull)
        );

        ep.enqueue_call(make_call(100, Priority::High, 0)).unwrap();

        assert_eq!(ep.pending_call_count(), SLOTS_PER_PRIORITY + 1);
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
}
