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
    let syscall_size = 56;
    let max_syscalls = 16;
    let count = (data.len() / syscall_size).min(max_syscalls);
    let mut k = setup_kernel();

    for i in 0..count {
        let offset = i * syscall_size;
        let chunk = &data[offset..offset + syscall_size];
        let syscall_num = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
        let mut args = [0u64; 6];

        for (j, arg) in args.iter_mut().enumerate() {
            let arg_offset = 8 + j * 8;

            *arg = u64::from_le_bytes(chunk[arg_offset..arg_offset + 8].try_into().unwrap());
        }

        let skip = matches!(syscall_num, 9 | 10 | 11 | 2 | 14 | 17 | 16 | 18 | 22);

        if skip {
            continue;
        }

        if k.scheduler.core(0).current() != Some(ThreadId(0)) {
            break;
        }

        let (error, _) = k.dispatch(ThreadId(0), 0, syscall_num, &args);

        assert!(error <= 12);
    }

    let violations = kernel::invariants::verify(&k);

    assert!(
        violations.is_empty(),
        "invariant violations: {:?}",
        violations
    );
});
