//! Endpoint — synchronous IPC with call/recv/reply protocol.
//!
//! Endpoints implement synchronous RPC: a client calls (blocks), the server
//! recvs the highest-priority call, processes it, and replies via a one-shot
//! reply cap. Handle transfer is staged in the PendingCall and installed by
//! the syscall layer.

use alloc::vec::Vec;

use crate::{
    config,
    handle::Handle,
    types::{EndpointId, EventId, Priority, SyscallError, ThreadId},
};

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
    pub handles: Vec<Handle>,
    pub badge: u32,
}

/// Tracks a reply cap issued to a server, linking it to the blocked caller.
struct ActiveReply {
    cap_id: ReplyCapId,
    caller: ThreadId,
}

/// A synchronous IPC endpoint.
///
/// Manages the call queue (pending calls from clients), active replies
/// (calls the server is processing), and recv waiters (servers waiting
/// for calls). Thread blocking/waking is the syscall layer's concern.
pub struct Endpoint {
    pub id: EndpointId,
    generation: u64,
    send_queue: Vec<PendingCall>,
    active_replies: Vec<ActiveReply>,
    recv_waiters: Vec<ThreadId>,
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
            generation: 0,
            send_queue: Vec::new(),
            active_replies: Vec::new(),
            recv_waiters: Vec::new(),
            next_reply_id: 0,
            badge_counter: 0,
            bound_event: None,
            peer_closed: false,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
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

    pub fn pending_reply_count(&self) -> usize {
        self.active_replies.len()
    }

    pub fn recv_waiter_count(&self) -> usize {
        self.recv_waiters.len()
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

    /// Enqueue a call into the send queue.
    pub fn enqueue_call(&mut self, call: PendingCall) -> Result<(), SyscallError> {
        if self.peer_closed {
            return Err(SyscallError::PeerClosed);
        }
        if self.send_queue.len() >= config::MAX_PENDING_PER_ENDPOINT {
            return Err(SyscallError::BufferFull);
        }
        self.send_queue.push(call);
        Ok(())
    }

    /// Dequeue the highest-priority pending call and issue a reply cap.
    pub fn dequeue_call(&mut self) -> Option<(PendingCall, ReplyCapId)> {
        if self.send_queue.is_empty() {
            return None;
        }

        let best_idx = self
            .send_queue
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| c.priority)
            .map(|(i, _)| i)
            .unwrap();

        let call = self.send_queue.swap_remove(best_idx);
        let cap_id = ReplyCapId(self.next_reply_id);
        self.next_reply_id += 1;
        self.active_replies.push(ActiveReply {
            cap_id,
            caller: call.caller,
        });

        Some((call, cap_id))
    }

    /// Consume a reply cap, returning the blocked caller's thread ID.
    pub fn consume_reply(&mut self, cap_id: ReplyCapId) -> Result<ThreadId, SyscallError> {
        let pos = self
            .active_replies
            .iter()
            .position(|r| r.cap_id == cap_id)
            .ok_or(SyscallError::InvalidHandle)?;
        let reply = self.active_replies.swap_remove(pos);
        Ok(reply.caller)
    }

    /// Highest priority among pending callers (for priority inheritance).
    pub fn highest_caller_priority(&self) -> Option<Priority> {
        self.send_queue.iter().map(|c| c.priority).max()
    }

    /// Add a server thread to the recv waiters list.
    pub fn add_recv_waiter(&mut self, thread: ThreadId) -> Result<(), SyscallError> {
        if self.peer_closed {
            return Err(SyscallError::PeerClosed);
        }
        self.recv_waiters.push(thread);
        Ok(())
    }

    /// Remove a recv waiter (on timeout or cancel).
    pub fn remove_recv_waiter(&mut self, thread: ThreadId) -> bool {
        if let Some(pos) = self.recv_waiters.iter().position(|&t| t == thread) {
            self.recv_waiters.swap_remove(pos);
            true
        } else {
            false
        }
    }

    /// Drain all recv waiters (for wakeup when a call arrives).
    pub fn drain_recv_waiters(&mut self) -> Vec<ThreadId> {
        core::mem::take(&mut self.recv_waiters)
    }

    /// Close the peer end. Returns all blocked thread IDs:
    /// callers in send queue + callers awaiting reply + recv waiters.
    pub fn close_peer(&mut self) -> Vec<ThreadId> {
        self.peer_closed = true;
        let mut blocked = Vec::new();

        for call in self.send_queue.drain(..) {
            blocked.push(call.caller);
        }
        for reply in self.active_replies.drain(..) {
            blocked.push(reply.caller);
        }
        blocked.append(&mut self.recv_waiters);

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

    /// Increment generation, revoking all handles.
    pub fn revoke(&mut self) {
        self.generation += 1;
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
            handles: Vec::new(),
            badge,
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
            handles: Vec::new(),
            badge: 42,
        };
        ep.enqueue_call(call).unwrap();

        let (received, reply_cap) = ep.dequeue_call().unwrap();
        assert_eq!(received.caller, ThreadId(1));
        assert_eq!(received.badge, 42);
        assert_eq!(received.message.as_bytes(), b"request");

        let caller = ep.consume_reply(reply_cap).unwrap();
        assert_eq!(caller, ThreadId(1));
        assert_eq!(ep.pending_reply_count(), 0);
    }

    #[test]
    fn handle_transfer_staged_in_call() {
        let mut ep = make_endpoint(0);
        let call = PendingCall {
            caller: ThreadId(1),
            priority: Priority::Medium,
            message: Message::empty(),
            handles: vec![make_handle(99), make_handle(100)],
            badge: 0,
        };
        ep.enqueue_call(call).unwrap();

        let (received, _) = ep.dequeue_call().unwrap();
        assert_eq!(received.handles.len(), 2);
        assert_eq!(received.handles[0].object_id, 99);
        assert_eq!(received.handles[1].object_id, 100);
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
        for i in 0..config::MAX_PENDING_PER_ENDPOINT {
            ep.enqueue_call(make_call(i as u32, Priority::Medium, 0))
                .unwrap();
        }
        assert_eq!(
            ep.enqueue_call(make_call(999, Priority::Medium, 0)),
            Err(SyscallError::BufferFull)
        );
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
        assert_eq!(waiters, vec![ThreadId(2)]);
        assert_eq!(ep.recv_waiter_count(), 0);
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

    #[test]
    fn generation_revoke() {
        let mut ep = make_endpoint(0);
        assert_eq!(ep.generation(), 0);
        ep.revoke();
        assert_eq!(ep.generation(), 1);
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
