//! Virgil3D render service — thick GPU driver.
//!
//! Reads the scene graph from shared memory, performs the tree walk, and
//! renders using Virgil3D/Gallium3D commands through virtio-gpu 3D mode.
//! The scene graph is the only interface — all rendering complexity is
//! internal to this driver (leaf node behind a simple boundary).
//!
//! Status: scaffolding — compiles and is embedded in init, but not yet
//! spawned or functional. See docs/superpowers/plans/ for the full plan.

#![no_std]
#![no_main]

extern crate alloc;

/// Compute the base VA of channel N's shared pages.
fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x8E\xAE virgil-render - starting\n");

    // TODO: Phase 1 — device init, virgl context creation, clear screen
    // TODO: Phase 2 — scene graph tree walk, solid rectangles
    // TODO: Phase 3 — glyph atlas, text rendering
    // TODO: Phase 4 — init integration, render service selection

    sys::print(b"     scaffolding only \xe2\x80\x94 entering idle\n");
    loop {
        let _ = sys::wait(&[0], u64::MAX);
    }
}
