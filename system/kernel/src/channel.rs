//! IPC channels — shared memory with signal/wait notification.
//!
//! A channel connects two thread endpoints via a shared physical page
//! mapped into both address spaces. The kernel provides signal/wait
//! primitives for synchronization; the shared memory protocol is
//! entirely userspace.
//!
//! Lock ordering: channel lock is always released before acquiring the
//! scheduler lock (via try_wake / with_thread_mut). Never hold both.

use super::addr_space::PageAttrs;
use super::handle::{ChannelId, HandleObject, Rights};
use super::page_alloc;
use super::paging::{CHANNEL_SHM_BASE, PAGE_SIZE};
use super::scheduler;
use super::sync::IrqMutex;
use super::thread::ThreadId;
use alloc::vec::Vec;

struct Channel {
    shared_pa: usize,
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
        page_alloc::free_frame(pa);
    }
}
/// Create a channel between two threads.
///
/// Allocates a shared physical page, maps it at a fixed VA in both address
/// spaces, and inserts READ_WRITE handles into both handle tables.
pub fn create(id_a: ThreadId, id_b: ThreadId) -> ChannelId {
    // Allocate shared page (acquires page_alloc lock, released immediately).
    let shared_pa = page_alloc::alloc_frame().expect("out of frames for channel");

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
    scheduler::with_thread_mut(id_a, |thread| {
        if let Some(addr_space) = &mut thread.address_space {
            addr_space.map_shared(va, shared_pa as u64, &PageAttrs::user_rw());
        }

        thread
            .handles
            .insert(HandleObject::Channel(channel_id), Rights::READ_WRITE)
            .expect("handle table full");
    });
    scheduler::with_thread_mut(id_b, |thread| {
        if let Some(addr_space) = &mut thread.address_space {
            addr_space.map_shared(va, shared_pa as u64, &PageAttrs::user_rw());
        }

        thread
            .handles
            .insert(HandleObject::Channel(channel_id), Rights::READ_WRITE)
            .expect("handle table full");
    });

    channel_id
}
/// Signal the other endpoint of a channel.
///
/// Sets the peer's pending_signal flag and wakes it if blocked.
/// Lost-wakeup safe: the flag persists even if the peer isn't waiting yet.
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
    scheduler::try_wake(peer_id);
}
