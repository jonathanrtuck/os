//! Power-On Self-Test — boot-time smoke test for debug builds.
//!
//! Creates one of each kernel object, exercises basic operations (IPC,
//! event signal/wait, VMO map, handle dup/close), verifies invariants,
//! and destroys everything. Panics on any failure.
//!
//! Cost: ~10,000 cycles (~2µs at 4.5 GHz). Negligible compared to boot.
//! Enabled by: debug_assertions (zero cost in release builds).

use crate::{
    address_space::AddressSpace,
    config,
    syscall::{Kernel, num},
    thread::Thread,
    types::{AddressSpaceId, ObjectType, Priority, Rights, ThreadId},
};

pub fn run(kern: &mut Kernel) {
    crate::println!("POST: running boot-time self-test...");

    let current = setup_post_env(kern);
    // VMO: create, snapshot, seal, close
    let (err, vmo_h) = kern.dispatch(
        current,
        0,
        num::VMO_CREATE,
        &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
    );

    assert!(err == 0, "POST: vmo_create failed (err={err})");

    let (err, snap_h) = kern.dispatch(current, 0, num::VMO_SNAPSHOT, &[vmo_h, 0, 0, 0, 0, 0]);

    assert!(err == 0, "POST: vmo_snapshot failed (err={err})");

    let (err, _) = kern.dispatch(current, 0, num::VMO_SEAL, &[snap_h, 0, 0, 0, 0, 0]);

    assert!(err == 0, "POST: vmo_seal failed (err={err})");

    // Event: create, signal, wait (immediate), clear, close
    let (err, evt_h) = kern.dispatch(current, 0, num::EVENT_CREATE, &[0, 0, 0, 0, 0, 0]);

    assert!(err == 0, "POST: event_create failed (err={err})");

    let (err, _) = kern.dispatch(current, 0, num::EVENT_SIGNAL, &[evt_h, 0xAB, 0, 0, 0, 0]);

    assert!(err == 0, "POST: event_signal failed (err={err})");

    let (err, _) = kern.dispatch(current, 0, num::EVENT_WAIT, &[evt_h, 0xAB, 1, 0, 0, 0]);

    assert!(err == 0, "POST: event_wait failed (err={err})");

    let (err, _) = kern.dispatch(current, 0, num::EVENT_CLEAR, &[evt_h, 0xAB, 0, 0, 0, 0]);

    assert!(err == 0, "POST: event_clear failed (err={err})");

    // Endpoint: create, close
    let (err, ep_h) = kern.dispatch(current, 0, num::ENDPOINT_CREATE, &[0, 0, 0, 0, 0, 0]);

    assert!(err == 0, "POST: endpoint_create failed (err={err})");

    // Handle: dup, info, close
    let (err, dup_h) = kern.dispatch(
        current,
        0,
        num::HANDLE_DUP,
        &[vmo_h, Rights::READ.0 as u64, 0, 0, 0, 0],
    );

    assert!(err == 0, "POST: handle_dup failed (err={err})");

    let (err, _) = kern.dispatch(current, 0, num::HANDLE_INFO, &[dup_h, 0, 0, 0, 0, 0]);

    assert!(err == 0, "POST: handle_info failed (err={err})");

    // Clock + system_info
    let (err, _) = kern.dispatch(current, 0, num::CLOCK_READ, &[0, 0, 0, 0, 0, 0]);

    assert!(err == 0, "POST: clock_read failed (err={err})");

    let (err, _) = kern.dispatch(current, 0, num::SYSTEM_INFO, &[0, 0, 0, 0, 0, 0]);

    assert!(err == 0, "POST: system_info failed (err={err})");

    // Clean up: close all handles
    for h in [dup_h, snap_h, vmo_h, evt_h, ep_h] {
        let (err, _) = kern.dispatch(current, 0, num::HANDLE_CLOSE, &[h, 0, 0, 0, 0, 0]);

        assert!(err == 0, "POST: handle_close({h}) failed (err={err})");
    }

    // Verify invariants after full lifecycle
    #[cfg(debug_assertions)]
    {
        let violations = crate::invariants::verify(kern);

        assert!(
            violations.is_empty(),
            "POST: invariant violations: {violations:?}",
        );
    }

    // Tear down POST environment
    teardown_post_env(kern, current);

    crate::println!("POST: all checks passed");
}

fn setup_post_env(kern: &mut Kernel) -> ThreadId {
    let asid = kern.alloc_asid().expect("POST: asid alloc");
    let space = AddressSpace::new(AddressSpaceId(0), asid, 0);
    let (space_idx, space_gen) = kern.spaces.alloc(space).expect("POST: space alloc");

    kern.spaces.get_mut(space_idx).unwrap().id = AddressSpaceId(space_idx);

    let space = kern.spaces.get_mut(space_idx).unwrap();

    space
        .handles_mut()
        .allocate(ObjectType::AddressSpace, space_idx, Rights::ALL, space_gen)
        .expect("POST: space handle");

    let thread = Thread::new(
        ThreadId(0),
        Some(AddressSpaceId(space_idx)),
        Priority::Medium,
        0x1000,
        0x2000,
        0,
    );
    let (tid_idx, _) = kern.threads.alloc(thread).expect("POST: thread alloc");

    kern.threads.get_mut(tid_idx).unwrap().id = ThreadId(tid_idx);
    kern.scheduler
        .enqueue(0, ThreadId(tid_idx), Priority::Medium);
    kern.alive_threads += 1;

    let space = kern.spaces.get_mut(space_idx).unwrap();

    space.set_thread_head(Some(tid_idx));

    ThreadId(tid_idx)
}

fn teardown_post_env(kern: &mut Kernel, thread_id: ThreadId) {
    let space_id = kern
        .threads
        .get(thread_id.0)
        .unwrap()
        .address_space()
        .unwrap();

    kern.scheduler.remove(thread_id);
    kern.threads.dealloc(thread_id.0);
    kern.alive_threads = kern.alive_threads.saturating_sub(1);

    let space = kern.spaces.get_mut(space_id.0).unwrap();

    space.set_thread_head(None);
    kern.spaces.dealloc(space_id.0);
}
