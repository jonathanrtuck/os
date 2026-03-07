//! IPC channels — shared memory with signal/wait notification.
//!
//! A channel connects two thread endpoints via a shared physical page
//! mapped into both address spaces. The kernel provides signal/wait
//! primitives for synchronization; the shared memory protocol is
//! entirely userspace.

use super::addr_space::PageAttrs;
use super::handle::{ChannelId, HandleObject, Rights};
use super::page_alloc;
use super::scheduler;
use super::thread::ThreadId;
use alloc::vec::Vec;
use core::cell::SyncUnsafeCell;

/// Base VA where channel shared pages are mapped in user address spaces.
const CHANNEL_SHM_BASE: u64 = 0x0000_0000_4000_0000;

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

static STATE: SyncUnsafeCell<State> = SyncUnsafeCell::new(State {
    channels: Vec::new(),
});

fn state() -> &'static mut State {
    unsafe { &mut *STATE.get() }
}

/// Check and consume a pending signal for the caller's endpoint.
///
/// Returns `true` if a signal was pending (now consumed), meaning the
/// caller should NOT block. Returns `false` if no signal was pending,
/// meaning the caller should block.
pub fn check_pending(id: ChannelId, caller: ThreadId) -> bool {
    let s = state();
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
    let s = state();
    let ch = &mut s.channels[id.0 as usize];

    ch.closed_count += 1;

    if ch.closed_count == 2 {
        page_alloc::free_frame(ch.shared_pa);
    }
}
/// Create a channel between two threads.
///
/// Allocates a shared physical page, maps it at a fixed VA in both address
/// spaces, and inserts READ_WRITE handles into both handle tables.
pub fn create(id_a: ThreadId, id_b: ThreadId) -> ChannelId {
    let s = state();
    let idx = s.channels.len() as u32;
    let channel_id = ChannelId(idx);
    let shared_pa = page_alloc::alloc_frame().expect("out of frames for channel");
    let va = CHANNEL_SHM_BASE + (idx as u64) * super::PAGE_SIZE;

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

    channel_id
}
/// Signal the other endpoint of a channel.
///
/// Sets the peer's pending_signal flag and wakes it if blocked.
/// Lost-wakeup safe: the flag persists even if the peer isn't waiting yet.
pub fn signal(id: ChannelId, caller: ThreadId) {
    let s = state();
    let ch = &mut s.channels[id.0 as usize];
    let peer_idx = if ch.endpoints[0].thread_id == caller {
        1
    } else {
        0
    };

    ch.endpoints[peer_idx].pending_signal = true;

    scheduler::try_wake(ch.endpoints[peer_idx].thread_id);
}
