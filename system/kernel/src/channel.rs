//! IPC channels — shared memory with signal/wait notification.
//!
//! A channel connects two thread endpoints via a shared physical page
//! mapped into both address spaces. The kernel provides signal/wait
//! primitives for synchronization; the shared memory protocol is
//! entirely userspace.
//!
//! # Protocol
//!
//! ```text
//! create(A, B):
//!   kernel allocates shared page, maps at CHANNEL_SHM_BASE in both
//!   address spaces, inserts READ_WRITE handle in both handle tables.
//!
//! signal(handle):       sets peer's pending_signal flag, wakes peer if blocked
//! wait(handle):         if pending_signal → consume + return immediately
//!                       else → block until peer signals
//! close_endpoint(id):   decrements closed_count; frees shared page when both close
//! ```
//!
//! Lost-wakeup safe: `signal` sets a persistent flag before waking, and
//! `wait` checks the flag before blocking. Even if `signal` arrives before
//! `wait`, the flag is consumed on the next `wait` call.
//!
//! # Lock Ordering
//!
//! Channel lock is always released before acquiring the scheduler lock
//! (via try_wake / with_thread_mut). Never hold both.

use super::address_space::PageAttrs;
use super::handle::{ChannelId, HandleError, HandleObject, Rights};
use super::memory;
use super::page_allocator;
use super::paging::{CHANNEL_SHM_BASE, PAGE_SIZE};
use super::scheduler;
use super::sync::IrqMutex;
use super::thread::ThreadId;
use alloc::vec::Vec;

struct Channel {
    shared_pa: memory::Pa,
    endpoints: [Endpoint; 2],
    closed_count: u8,
}
struct Endpoint {
    thread_id: ThreadId,
    pending_signal: bool,
}
struct State {
    channels: Vec<Channel>,
}

static STATE: IrqMutex<State> = IrqMutex::new(State {
    channels: Vec::new(),
});

/// Check and consume a pending signal for the caller's endpoint.
///
/// Returns `true` if a signal was pending (now consumed), meaning the
/// caller should NOT block. Returns `false` if no signal was pending,
/// meaning the caller should block.
pub fn check_pending(id: ChannelId, caller: ThreadId) -> bool {
    let mut s = STATE.lock();
    let ch = &mut s.channels[id.0 as usize];
    let self_idx = if ch.endpoints[0].thread_id == caller {
        0
    } else {
        1
    };

    if ch.endpoints[self_idx].pending_signal {
        ch.endpoints[self_idx].pending_signal = false;
        true
    } else {
        false
    }
}
/// Close one endpoint of a channel. Frees the shared page when both sides close.
pub fn close_endpoint(id: ChannelId) {
    let shared_pa = {
        let mut s = STATE.lock();
        let ch = &mut s.channels[id.0 as usize];

        ch.closed_count += 1;

        if ch.closed_count == 2 {
            Some(ch.shared_pa)
        } else {
            None
        }
    };

    // Free outside channel lock (acquires page_alloc lock).
    if let Some(pa) = shared_pa {
        page_allocator::free_frame(pa);
    }
}
/// Create a channel between two threads.
///
/// Allocates a shared physical page, maps it at a fixed VA in both address
/// spaces, and inserts READ_WRITE handles into both handle tables.
/// Returns `HandleError::TableFull` if either handle table is full
/// (the shared page and channel are cleaned up on failure).
pub fn create(id_a: ThreadId, id_b: ThreadId) -> Result<ChannelId, HandleError> {
    // Allocate shared page (acquires page_alloc lock, released immediately).
    let shared_pa = page_allocator::alloc_frame().expect("out of frames for channel");
    // Push channel BEFORE inserting handles so the channel exists in the Vec
    // before any thread can reference it. Prevents TOCTOU if preemption is
    // active (handle lookup would index into channels before the push).
    let (idx, va) = {
        let mut s = STATE.lock();
        let idx = s.channels.len() as u32;
        let va = CHANNEL_SHM_BASE + (idx as u64) * PAGE_SIZE;

        s.channels.push(Channel {
            shared_pa,
            endpoints: [
                Endpoint {
                    thread_id: id_a,
                    pending_signal: false,
                },
                Endpoint {
                    thread_id: id_b,
                    pending_signal: false,
                },
            ],
            closed_count: 0,
        });

        (idx, va)
    };
    let channel_id = ChannelId(idx);
    // Insert handles (acquires scheduler lock; channel lock already released).
    let handle_a = scheduler::with_process_of_thread(id_a, |process| {
        process
            .address_space
            .map_shared(va, shared_pa.as_u64(), &PageAttrs::user_rw());

        process
            .handles
            .insert(HandleObject::Channel(channel_id), Rights::READ_WRITE)
    });
    let handle_a = match handle_a {
        Ok(h) => h,
        Err(e) => {
            // Clean up: free the shared page.
            page_allocator::free_frame(shared_pa);

            return Err(e);
        }
    };
    let result_b = scheduler::with_process_of_thread(id_b, |process| {
        process
            .address_space
            .map_shared(va, shared_pa.as_u64(), &PageAttrs::user_rw());

        process
            .handles
            .insert(HandleObject::Channel(channel_id), Rights::READ_WRITE)
    });

    if let Err(e) = result_b {
        // Clean up the handle we inserted into process A.
        scheduler::with_process_of_thread(id_a, |process| {
            let _ = process.handles.close(handle_a);
        });
        page_allocator::free_frame(shared_pa);

        return Err(e);
    }

    Ok(channel_id)
}
/// Signal the other endpoint of a channel.
///
/// Sets the peer's pending_signal flag and wakes it if blocked. If the peer
/// is not yet blocked (in the gap between checking readiness and calling
/// `block_current_unless_woken`), sets `wake_pending` on the peer so the
/// block is skipped.
pub fn signal(id: ChannelId, caller: ThreadId) {
    let peer_id = {
        let mut s = STATE.lock();
        let ch = &mut s.channels[id.0 as usize];
        let peer_idx = if ch.endpoints[0].thread_id == caller {
            1
        } else {
            0
        };

        ch.endpoints[peer_idx].pending_signal = true;

        ch.endpoints[peer_idx].thread_id
    };

    // Wake outside channel lock (acquires scheduler lock).
    // If the peer has a wait set, try_wake_for_handle resolves the return index.
    if !scheduler::try_wake_for_handle(peer_id, HandleObject::Channel(id)) {
        // Peer not blocked yet — set pending flag for lost-wakeup prevention.
        scheduler::set_wake_pending_for_handle(peer_id, HandleObject::Channel(id));
    }
}
