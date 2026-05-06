#![no_main]

use kernel::{
    address_space::AddressSpace,
    config,
    syscall::{Kernel, num},
    thread::Thread,
    types::{AddressSpaceId, Priority, Rights, ThreadId},
};
use libfuzzer_sys::fuzz_target;

const MAX_THREADS: usize = 4;
const MAX_HANDLES_TRACKED: usize = 16;

struct FuzzState {
    thread_ids: [ThreadId; MAX_THREADS],
    active_threads: usize,
    handles: [u64; MAX_HANDLES_TRACKED],
    handle_count: usize,
}

fn setup_kernel() -> (Box<Kernel>, FuzzState) {
    let mut k = Box::new(Kernel::new(2));
    let space0 = AddressSpace::new(AddressSpaceId(0), 1, 0);

    k.spaces.alloc(space0);

    let space1 = AddressSpace::new(AddressSpaceId(1), 2, 0);

    k.spaces.alloc(space1);

    let t0 = Thread::new(
        ThreadId(0),
        Some(AddressSpaceId(0)),
        Priority::Medium,
        0,
        0,
        0,
    );

    k.threads.alloc(t0);
    k.threads
        .get_mut(0)
        .unwrap()
        .set_state(kernel::thread::ThreadRunState::Running);
    k.scheduler.core_mut(0).set_current(Some(ThreadId(0)));

    let t1 = Thread::new(
        ThreadId(1),
        Some(AddressSpaceId(1)),
        Priority::High,
        0,
        0,
        0,
    );

    k.threads.alloc(t1);
    k.scheduler.enqueue(1, ThreadId(1), Priority::High);

    let state = FuzzState {
        thread_ids: [ThreadId(0), ThreadId(1), ThreadId(0), ThreadId(0)],
        active_threads: 2,
        handles: [u64::MAX; MAX_HANDLES_TRACKED],
        handle_count: 0,
    };

    (k, state)
}

fuzz_target!(|data: &[u8]| {
    let (mut k, mut st) = setup_kernel();

    for chunk in data.chunks_exact(4) {
        let op = chunk[0];
        let arg1 = chunk[1];
        let arg2 = chunk[2];
        let thread_sel = chunk[3];
        let tid_idx = (thread_sel as usize) % st.active_threads;
        let tid = st.thread_ids[tid_idx];
        let core_id = tid_idx % 2;
        let thread_state = k.threads.get(tid.0).map(|t| t.state());

        match thread_state {
            Some(kernel::thread::ThreadRunState::Ready) => {
                k.scheduler.remove(tid);
                k.threads
                    .get_mut(tid.0)
                    .unwrap()
                    .set_state(kernel::thread::ThreadRunState::Running);
                k.scheduler.core_mut(core_id).set_current(Some(tid));
            }
            Some(kernel::thread::ThreadRunState::Running) => {}
            _ => continue,
        }

        match op % 12 {
            0 => {
                let (err, hid) = k.dispatch(
                    tid,
                    core_id,
                    num::VMO_CREATE,
                    &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
                );

                if err == 0 && st.handle_count < MAX_HANDLES_TRACKED {
                    st.handles[st.handle_count] = hid;
                    st.handle_count += 1;
                }
            }
            1 => {
                let (err, hid) = k.dispatch(tid, core_id, num::ENDPOINT_CREATE, &[0; 6]);

                if err == 0 && st.handle_count < MAX_HANDLES_TRACKED {
                    st.handles[st.handle_count] = hid;
                    st.handle_count += 1;
                }
            }
            2 => {
                let (err, hid) = k.dispatch(tid, core_id, num::EVENT_CREATE, &[0; 6]);

                if err == 0 && st.handle_count < MAX_HANDLES_TRACKED {
                    st.handles[st.handle_count] = hid;
                    st.handle_count += 1;
                }
            }
            3 => {
                if st.handle_count > 0 {
                    let idx = (arg1 as usize) % st.handle_count;
                    let hid = st.handles[idx];

                    k.dispatch(tid, core_id, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

                    st.handles[idx] = st.handles[st.handle_count - 1];
                    st.handles[st.handle_count - 1] = u64::MAX;
                    st.handle_count -= 1;
                }
            }
            4 => {
                if st.handle_count > 0 {
                    let idx = (arg1 as usize) % st.handle_count;
                    let hid = st.handles[idx];
                    let (err, dup) = k.dispatch(
                        tid,
                        core_id,
                        num::HANDLE_DUP,
                        &[hid, Rights::ALL.0 as u64, 0, 0, 0, 0],
                    );

                    if err == 0 && st.handle_count < MAX_HANDLES_TRACKED {
                        st.handles[st.handle_count] = dup;
                        st.handle_count += 1;
                    }
                }
            }
            5 => {
                if st.handle_count > 0 {
                    let idx = (arg1 as usize) % st.handle_count;
                    let hid = st.handles[idx];
                    let bits = ((arg2 as u64) << 1) | 1;

                    k.dispatch(tid, core_id, num::EVENT_SIGNAL, &[hid, bits, 0, 0, 0, 0]);
                }
            }
            6 => {
                if st.handle_count > 0 {
                    let idx = (arg1 as usize) % st.handle_count;
                    let hid = st.handles[idx];

                    k.dispatch(tid, core_id, num::EVENT_CLEAR, &[hid, u64::MAX, 0, 0, 0, 0]);
                }
            }
            7 => {
                if st.handle_count > 0 {
                    let idx = (arg1 as usize) % st.handle_count;
                    let hid = st.handles[idx];

                    k.dispatch(tid, core_id, num::HANDLE_INFO, &[hid, 0, 0, 0, 0, 0]);
                }
            }
            8 => {
                if st.handle_count > 0 {
                    let idx = (arg1 as usize) % st.handle_count;
                    let hid = st.handles[idx];
                    let mut buf = [0u8; 64];

                    k.dispatch(
                        tid,
                        core_id,
                        num::CALL,
                        &[hid, buf.as_mut_ptr() as u64, arg2 as u64 % 64, 0, 0, 0],
                    );
                }
            }
            9 => {
                if st.handle_count > 0 {
                    let idx = (arg1 as usize) % st.handle_count;
                    let hid = st.handles[idx];
                    let mut buf = [0u8; 128];

                    k.dispatch(
                        tid,
                        core_id,
                        num::RECV,
                        &[hid, buf.as_mut_ptr() as u64, 128, 0, 0, 0],
                    );
                }
            }
            10 => {
                k.dispatch(tid, core_id, num::SYSTEM_INFO, &[0; 6]);
            }
            _ => {
                k.dispatch(tid, core_id, num::CLOCK_READ, &[0; 6]);
            }
        }
    }

    let violations = kernel::invariants::verify(&k);

    assert!(
        violations.is_empty(),
        "invariant violations after {} ops: {:?}",
        data.len() / 4,
        violations
    );
});
