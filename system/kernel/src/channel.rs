//! IPC channels — shared memory with signal/wait notification.
//!
//! A channel has two endpoints, each identified by a unique ChannelId.
//! Endpoint identity is encoded in the ChannelId: `channel_index * 2 +
//! endpoint` (0 or 1). Both endpoints share a physical page mapped into
//! each process's address space at the same VA.
//!
//! # Protocol
//!
//! ```text
//! create():
//!   kernel allocates shared page, returns (ChannelId, ChannelId).
//!   Mapping + handle insertion done separately (boot or syscall path).
//!
//! signal(my_id):       sets peer's pending_signal flag, wakes peer if blocked
//! check_pending(id):   if pending_signal → consume + return true
//!                      else → return false
//! close_endpoint(id):  decrements closed_count; frees shared page when both close
//! ```
//!
//! Lost-wakeup safe: `signal` sets a persistent flag before waking, and
//! `check_pending` checks the flag before blocking. Even if `signal` arrives
//! before `wait`, the flag is consumed on the next check.
//!
//! # Lock Ordering
//!
//! Channel lock is always released before acquiring the scheduler lock
//! (via try_wake / set_wake_pending). Never hold both.

use super::handle::{ChannelId, Handle, HandleError, HandleObject, Rights};
use super::memory;
use super::page_allocator;
use super::paging::{CHANNEL_SHM_BASE, PAGE_SIZE};
use super::process::ProcessId;
use super::scheduler;
use super::sync::IrqMutex;
use super::thread::ThreadId;
use alloc::vec::Vec;

struct Channel {
    shared_pa: memory::Pa,
    pending_signal: [bool; 2],
    waiter: [Option<ThreadId>; 2],
    closed_count: u8,
}
struct State {
    channels: Vec<Channel>,
}

static STATE: IrqMutex<State> = IrqMutex::new(State {
    channels: Vec::new(),
});

fn channel_index(id: ChannelId) -> usize {
    id.0 as usize / 2
}
fn endpoint_index(id: ChannelId) -> usize {
    id.0 as usize % 2
}

/// Check and consume a pending signal for this endpoint.
///
/// Returns `true` if a signal was pending (now consumed), meaning the
/// caller should NOT block. Returns `false` if no signal was pending.
pub fn check_pending(id: ChannelId) -> bool {
    let mut s = STATE.lock();
    let ch = &mut s.channels[channel_index(id)];
    let ep = endpoint_index(id);

    if ch.pending_signal[ep] {
        ch.pending_signal[ep] = false;
        true
    } else {
        false
    }
}
/// Close one endpoint of a channel. Frees the shared page when both sides close.
pub fn close_endpoint(id: ChannelId) {
    let shared_pa = {
        let mut s = STATE.lock();
        let ch = &mut s.channels[channel_index(id)];
        let ep = endpoint_index(id);

        ch.waiter[ep] = None;
        ch.closed_count += 1;

        if ch.closed_count == 2 {
            let pa = ch.shared_pa;

            ch.shared_pa = memory::Pa(0);

            Some(pa)
        } else {
            None
        }
    };

    if let Some(pa) = shared_pa {
        page_allocator::free_frame(pa);
    }
}
/// Create a channel. Allocates a shared physical page and returns two endpoint IDs.
///
/// Both endpoints start unmapped — use `setup_endpoint` for boot-time setup
/// or map the shared page via the syscall path.
pub fn create() -> Option<(ChannelId, ChannelId)> {
    let shared_pa = page_allocator::alloc_frame()?;
    let mut s = STATE.lock();
    let idx = s.channels.len() as u32;

    s.channels.push(Channel {
        shared_pa,
        pending_signal: [false, false],
        waiter: [None, None],
        closed_count: 0,
    });

    Some((ChannelId(idx * 2), ChannelId(idx * 2 + 1)))
}
/// Register a thread as the waiter for a channel endpoint.
pub fn register_waiter(id: ChannelId, waiter: ThreadId) {
    let mut s = STATE.lock();
    let ch = &mut s.channels[channel_index(id)];
    let ep = endpoint_index(id);

    ch.waiter[ep] = Some(waiter);
}
/// Set up an endpoint for a process: map shared page + insert handle.
///
/// Boot-time helper. Acquires channel lock (for PA), then scheduler lock
/// (for process access). Uses the target's per-process channel SHM bump
/// allocator. Returns the handle index.
pub fn setup_endpoint(id: ChannelId, pid: ProcessId) -> Result<Handle, HandleError> {
    let shared_pa = {
        let s = STATE.lock();
        s.channels[channel_index(id)].shared_pa
    };

    scheduler::with_process(pid, |process| {
        process
            .address_space
            .map_channel_page(shared_pa.as_u64())
            .ok_or(HandleError::TableFull)?;
        process
            .handles
            .insert(HandleObject::Channel(id), Rights::READ_WRITE)
    })
    .unwrap_or(Err(HandleError::InvalidHandle))
}
/// Return the shared page PA and VA for a channel.
///
/// Both endpoints of the same channel share the same page.
/// Returns `None` if the channel is fully closed (both endpoints closed).
pub fn shared_info(id: ChannelId) -> Option<(memory::Pa, u64)> {
    let s = STATE.lock();
    let idx = channel_index(id);
    let ch = &s.channels[idx];

    if ch.closed_count >= 2 {
        return None;
    }

    Some((ch.shared_pa, CHANNEL_SHM_BASE + (idx as u64) * PAGE_SIZE))
}
/// Signal the peer endpoint of a channel.
///
/// Sets the peer's pending_signal flag and wakes it if blocked. Two-phase
/// wake: collect waiter under channel lock, wake under scheduler lock.
pub fn signal(id: ChannelId) {
    let (waiter, peer_id) = {
        let mut s = STATE.lock();
        let ch_idx = channel_index(id);
        let ch = &mut s.channels[ch_idx];
        let peer_ep = 1 - endpoint_index(id);

        ch.pending_signal[peer_ep] = true;

        let peer_channel_id = ChannelId(ch_idx as u32 * 2 + peer_ep as u32);

        (ch.waiter[peer_ep].take(), peer_channel_id)
    };

    if let Some(waiter_id) = waiter {
        // Reason is the peer's own ChannelId — matches the HandleObject stored
        // in the peer's wait set.
        let reason = HandleObject::Channel(peer_id);

        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
}
/// Unregister a waiter (cleanup when `wait` returns).
pub fn unregister_waiter(id: ChannelId) {
    let mut s = STATE.lock();
    let ch = &mut s.channels[channel_index(id)];
    let ep = endpoint_index(id);

    ch.waiter[ep] = None;
}
