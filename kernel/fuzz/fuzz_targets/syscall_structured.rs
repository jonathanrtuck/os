#![no_main]

use kernel::{
    address_space::AddressSpace,
    config,
    syscall::{Kernel, num},
    thread::Thread,
    types::{AddressSpaceId, Priority, Rights, ThreadId},
};
use libfuzzer_sys::fuzz_target;

fn setup_kernel() -> Box<Kernel> {
    let mut k = Box::new(Kernel::new(1));
    let space = AddressSpace::new(AddressSpaceId(0), 1, 0);

    k.spaces.alloc(space);

    let thread = Thread::new(
        ThreadId(0),
        Some(AddressSpaceId(0)),
        Priority::Medium,
        0,
        0,
        0,
    );

    k.threads.alloc(thread);
    k.threads
        .get_mut(0)
        .unwrap()
        .set_state(kernel::thread::ThreadRunState::Running);
    k.scheduler.core_mut(0).set_current(Some(ThreadId(0)));

    k
}

fuzz_target!(|data: &[u8]| {
    let mut k = setup_kernel();
    let mut handles: [u64; 32] = [u64::MAX; 32];
    let mut handle_count = 0usize;

    for chunk in data.chunks_exact(3) {
        let op = chunk[0];
        let arg1 = chunk[1];
        let arg2 = chunk[2];

        match op % 8 {
            0 => {
                let (err, hid) = k.dispatch(
                    ThreadId(0),
                    0,
                    num::VMO_CREATE,
                    &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
                );

                if err == 0 && handle_count < 32 {
                    handles[handle_count] = hid;
                    handle_count += 1;
                }
            }
            1 => {
                let (err, hid) = k.dispatch(ThreadId(0), 0, num::ENDPOINT_CREATE, &[0; 6]);

                if err == 0 && handle_count < 32 {
                    handles[handle_count] = hid;
                    handle_count += 1;
                }
            }
            2 => {
                let (err, hid) = k.dispatch(ThreadId(0), 0, num::EVENT_CREATE, &[0; 6]);

                if err == 0 && handle_count < 32 {
                    handles[handle_count] = hid;
                    handle_count += 1;
                }
            }
            3 => {
                if handle_count > 0 {
                    let idx = (arg1 as usize) % handle_count;
                    let hid = handles[idx];

                    k.dispatch(ThreadId(0), 0, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

                    handles[idx] = handles[handle_count - 1];
                    handles[handle_count - 1] = u64::MAX;
                    handle_count -= 1;
                }
            }
            4 => {
                if handle_count > 0 {
                    let idx = (arg1 as usize) % handle_count;
                    let hid = handles[idx];
                    let (err, dup_hid) = k.dispatch(
                        ThreadId(0),
                        0,
                        num::HANDLE_DUP,
                        &[hid, Rights::ALL.0 as u64, 0, 0, 0, 0],
                    );

                    if err == 0 && handle_count < 32 {
                        handles[handle_count] = dup_hid;
                        handle_count += 1;
                    }
                }
            }
            5 => {
                if handle_count > 0 {
                    let idx = (arg1 as usize) % handle_count;
                    let hid = handles[idx];
                    let bits = ((arg2 as u64) << 1) | 1;

                    k.dispatch(ThreadId(0), 0, num::EVENT_SIGNAL, &[hid, bits, 0, 0, 0, 0]);
                }
            }
            6 => {
                if handle_count > 0 {
                    let idx = (arg1 as usize) % handle_count;
                    let hid = handles[idx];

                    k.dispatch(
                        ThreadId(0),
                        0,
                        num::EVENT_CLEAR,
                        &[hid, u64::MAX, 0, 0, 0, 0],
                    );
                }
            }
            _ => {
                if handle_count > 0 {
                    let idx = (arg1 as usize) % handle_count;
                    let hid = handles[idx];

                    k.dispatch(ThreadId(0), 0, num::HANDLE_INFO, &[hid, 0, 0, 0, 0, 0]);
                }
            }
        }
    }

    let violations = kernel::invariants::verify(&k);

    assert!(
        violations.is_empty(),
        "invariant violations after {} ops: {:?}",
        data.len() / 3,
        violations
    );
});
