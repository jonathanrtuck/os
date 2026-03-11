// AUDIT: 2026-03-11 — 0 unsafe blocks (all safe Rust under IrqMutex). 6-category
// checklist applied. Found: closed_count could exceed 2 on redundant close_endpoint
// calls, risking double-free of shared pages. Fixed with early-return guard when
// closed_count >= 2. Close-while-blocked-reader race verified correct (two-phase wake
// pattern). Signal on half-closed channel is harmless. Shared memory pages freed
// exactly once when both endpoints close. Lock ordering (channel → scheduler) verified.

//! IPC channels — shared memory with signal/wait notification.
//!
//! A channel has two endpoints, each identified by a unique ChannelId.
//! Endpoint identity is encoded in the ChannelId: `channel_index * 2 +
//! endpoint` (0 or 1). Each channel has two shared physical pages:
//!
//! - Page 0: endpoint 0 → endpoint 1 ring buffer (ep0 produces, ep1 consumes)
//! - Page 1: endpoint 1 → endpoint 0 ring buffer (ep1 produces, ep0 consumes)
//!
//! Both pages are mapped into both processes. The userspace `ipc` library
//! provides the ring buffer data structure on top of these pages. The kernel
//! remains ignorant of message format — it provides shared pages and doorbells.
//!
//! # Protocol
//!
//! ```text
//! create():
//!   kernel allocates two shared pages, returns (ChannelId, ChannelId).
//!   Mapping + handle insertion done separately (boot or syscall path).
//!
//! signal(my_id):       sets peer's pending_signal flag, wakes peer if blocked
//! check_pending(id):   if pending_signal → consume + return true
//!                      else → return false
//! close_endpoint(id):  decrements closed_count; frees shared pages when both close
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
    /// Two shared pages: [0] = ep0→ep1 direction, [1] = ep1→ep0 direction.
    pages: [memory::Pa; 2],
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
/// Close one endpoint of a channel. Frees both shared pages when both sides close.
///
/// Wakes the peer's waiter if one is registered — the peer should not remain
/// blocked on a half-closed channel.
pub fn close_endpoint(id: ChannelId) {
    let (pages_to_free, peer_wake) = {
        let mut s = STATE.lock();
        let ch_idx = channel_index(id);
        let ch = &mut s.channels[ch_idx];

        // Already fully closed — prevent double-free of shared pages.
        if ch.closed_count >= 2 {
            return;
        }

        let ep = endpoint_index(id);
        let peer_ep = 1 - ep;

        ch.waiter[ep] = None;

        // Wake the peer's waiter — they're waiting on a now-dead channel.
        let peer_waiter = ch.waiter[peer_ep].take();
        let peer_channel_id = ChannelId(ch_idx as u32 * 2 + peer_ep as u32);

        ch.closed_count += 1;

        let pages = if ch.closed_count == 2 {
            let pages = ch.pages;

            ch.pages = [memory::Pa(0), memory::Pa(0)];

            Some(pages)
        } else {
            None
        };

        (pages, peer_waiter.map(|w| (w, peer_channel_id)))
    };

    if let Some((waiter_id, peer_id)) = peer_wake {
        let reason = HandleObject::Channel(peer_id);

        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }

    if let Some(pages) = pages_to_free {
        page_allocator::free_frame(pages[0]);
        page_allocator::free_frame(pages[1]);
    }
}
/// Create a channel. Allocates two shared physical pages and returns two endpoint IDs.
///
/// Page 0 carries messages from endpoint 0 to endpoint 1.
/// Page 1 carries messages from endpoint 1 to endpoint 0.
/// Both endpoints start unmapped — use `setup_endpoint` for boot-time setup
/// or map the shared pages via the syscall path.
pub fn create() -> Option<(ChannelId, ChannelId)> {
    let page0 = page_allocator::alloc_frame()?;
    let page1 = match page_allocator::alloc_frame() {
        Some(pa) => pa,
        None => {
            page_allocator::free_frame(page0);
            return None;
        }
    };
    let mut s = STATE.lock();
    let idx = s.channels.len() as u32;

    s.channels.push(Channel {
        pages: [page0, page1],
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
/// Set up an endpoint for a process: map both shared pages + insert handle.
///
/// Boot-time helper. Acquires channel lock (for PAs), then scheduler lock
/// (for process access). Uses the target's per-process channel SHM bump
/// allocator. Both pages are mapped at consecutive VAs. Returns the handle index.
pub fn setup_endpoint(id: ChannelId, pid: ProcessId) -> Result<Handle, HandleError> {
    let pages = {
        let s = STATE.lock();
        s.channels[channel_index(id)].pages
    };

    scheduler::with_process(pid, |process| {
        // Map both pages (ep0→ep1 and ep1→ep0 rings) at consecutive VAs.
        process
            .address_space
            .map_channel_page(pages[0].as_u64())
            .ok_or(HandleError::TableFull)?;
        process
            .address_space
            .map_channel_page(pages[1].as_u64())
            .ok_or(HandleError::TableFull)?;
        process
            .handles
            .insert(HandleObject::Channel(id), Rights::READ_WRITE)
    })
    .unwrap_or(Err(HandleError::InvalidHandle))
}
/// Return the shared page PAs for a channel.
///
/// Returns `[page0_pa, page1_pa]` where page 0 is the ep0→ep1 ring and
/// page 1 is the ep1→ep0 ring. Returns `None` if the channel is fully closed.
pub fn shared_pages(id: ChannelId) -> Option<[memory::Pa; 2]> {
    let s = STATE.lock();
    let idx = channel_index(id);
    let ch = &s.channels[idx];

    if ch.closed_count >= 2 {
        return None;
    }

    Some(ch.pages)
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
