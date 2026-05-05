#![no_main]

use kernel::{
    address_space::AddressSpace,
    syscall::Kernel,
    thread::Thread,
    types::{AddressSpaceId, Priority, ThreadId},
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
    if data.len() < 56 {
        return;
    }

    let mut k = setup_kernel();
    let syscall_num = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let mut args = [0u64; 6];

    for (i, arg) in args.iter_mut().enumerate() {
        let offset = 8 + i * 8;

        *arg = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    }

    let takes_user_ptr = matches!(
        syscall_num,
        9 | 10 | 11 | 2 | 14 | 17
    );

    if takes_user_ptr {
        return;
    }

    let (error, _value) = k.dispatch(ThreadId(0), 0, syscall_num, &args);

    assert!(error <= 12, "invalid error code: {error}");

    let violations = kernel::invariants::verify(&k);
    assert!(
        violations.is_empty(),
        "invariant violations: {:?}",
        violations
    );
});
