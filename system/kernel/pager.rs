//! Pager fault dispatch — forwards page faults to userspace pager processes.
//!
//! When a VMO with an attached pager receives a page fault on an uncommitted
//! page, the kernel writes the fault offset to a ring in the pager channel's
//! shared page and signals the channel. The faulting thread blocks until
//! `pager_supply` commits the page and wakes it.
//!
//! # Pager Ring Protocol
//!
//! The kernel writes to the pager channel's shared page (ep0→ep1 direction):
//!
//! ```text
//! [0..8]:   write_head (u64, kernel increments)
//! [8..16]:  read_head (u64, pager updates after consuming)
//! [16..]:   entries[]: u64 page offsets
//! ```
//!
//! Standard SPSC ring. Kernel produces, pager consumes.

use super::{
    channel, handle::ChannelId, memory, paging::PAGE_SIZE, scheduler, vmo::VmoId, Context,
};

const RING_CAPACITY: usize = (PAGE_SIZE as usize - RING_HEADER_SIZE) / 8;
const RING_HEADER_SIZE: usize = 16;

/// Block the current (faulting) thread until the pager supplies the page.
///
/// Returns a different thread's Context pointer (the next thread to run).
/// The blocked thread resumes when `pager_supply` wakes it.
pub fn block_for_pager(ctx: *mut Context, vmo_id: VmoId, page_offset: u64) -> *const Context {
    scheduler::block_current_for_pager(ctx, vmo_id, page_offset)
}
/// Write a fault page offset to the pager channel's ring and signal the channel.
///
/// Called from `user_fault_handler` when a pager-backed VMO page fault
/// needs to be forwarded. The VMO lock is NOT held at this point.
pub fn dispatch_fault(channel_id: ChannelId, page_offset: u64) {
    // Get the channel's shared pages.
    let pages = match channel::shared_pages(channel_id) {
        Some(p) => p,
        None => return, // Channel closed — pager is dead
    };

    // Write to page[0] (ep0→ep1 direction = kernel → pager).
    // SAFETY: pages[0] is a valid allocated frame. phys_to_virt produces
    // a valid kernel VA. The ring header and entries are within PAGE_SIZE.
    unsafe {
        let ring_va = memory::phys_to_virt(pages[0]) as *mut u64;
        let write_head = ring_va.read_volatile() as usize;
        let read_head = ring_va.add(1).read_volatile() as usize;
        // Check if ring is full.
        let next = (write_head + 1) % RING_CAPACITY;

        if next == read_head % RING_CAPACITY {
            return; // Ring full — pager is overwhelmed, drop this fault.
                    // Thread will retry after pager catches up.
        }

        // Write the page offset entry.
        let entry_ptr = (ring_va as *mut u8).add(RING_HEADER_SIZE) as *mut u64;

        entry_ptr
            .add(write_head % RING_CAPACITY)
            .write_volatile(page_offset);
        // Advance write head.
        ring_va.write_volatile((write_head + 1) as u64);
    }

    // Signal the pager channel so it wakes from wait().
    channel::signal(channel_id);
}
