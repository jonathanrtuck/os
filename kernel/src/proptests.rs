//! Property-based tests — verify kernel invariants over the space of inputs.
//!
//! Hand-written tests check the cases you thought of. Property tests check
//! the cases you didn't.

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use proptest::prelude::*;

    use crate::{
        address_space::AddressSpace,
        config,
        syscall::{Kernel, num},
        thread::Thread,
        types::{AddressSpaceId, HandleId, ObjectType, Priority, Rights, SyscallError, ThreadId},
    };

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
            .set_state(crate::thread::ThreadRunState::Running);
        k.scheduler.core_mut(0).set_current(Some(ThreadId(0)));

        k
    }

    fn call(k: &mut Kernel, num: u64, args: &[u64; 6]) -> (u64, u64) {
        k.dispatch(ThreadId(0), 0, num, args)
    }

    fn inv(k: &Kernel) {
        crate::invariants::assert_valid(k);
    }

    // =========================================================================
    // BOUNDARY VALUE PROPERTIES
    // =========================================================================

    fn boundary_size() -> impl Strategy<Value = u64> {
        prop_oneof![
            Just(0u64),
            Just(1),
            Just(config::PAGE_SIZE as u64 - 1),
            Just(config::PAGE_SIZE as u64),
            Just(config::PAGE_SIZE as u64 + 1),
            Just(config::PAGE_SIZE as u64 * 2),
            Just(config::MAX_PHYS_MEM as u64),
            Just(config::MAX_PHYS_MEM as u64 + 1),
            Just(u64::MAX),
            Just(u64::MAX - config::PAGE_SIZE as u64 + 1),
            1..=(config::PAGE_SIZE as u64 * 4),
        ]
    }

    fn boundary_u64() -> impl Strategy<Value = u64> {
        prop_oneof![
            Just(0u64),
            Just(1),
            Just(u32::MAX as u64),
            Just(u32::MAX as u64 + 1),
            Just(u64::MAX),
            Just(1u64 << 63),
            Just(1u64 << 32),
            0..=u64::MAX,
        ]
    }

    fn boundary_handle() -> impl Strategy<Value = u64> {
        prop_oneof![
            Just(0u64),
            Just(1),
            Just(config::MAX_HANDLES as u64 - 1),
            Just(config::MAX_HANDLES as u64),
            Just(u32::MAX as u64),
            0..=(config::MAX_HANDLES as u64 + 10),
        ]
    }

    fn valid_rights() -> impl Strategy<Value = u64> {
        prop_oneof![
            Just(0u64),
            Just(Rights::ALL.0 as u64),
            Just(Rights::READ.0 as u64),
            Just(Rights::WRITE.0 as u64),
            Just(Rights::MAP.0 as u64),
            Just(Rights::DUP.0 as u64),
            Just(u32::MAX as u64),
            0..=0x1FFu64,
        ]
    }

    // =========================================================================
    // VMO PROPERTIES
    // =========================================================================

    proptest! {
        #[test]
        fn vmo_create_invalid_size_never_panics(size in boundary_size()) {
            let mut k = setup_kernel();
            let (err, _) = call(&mut k, num::VMO_CREATE, &[size, 0, 0, 0, 0, 0]);

            prop_assert!(err <= SyscallError::NotFound as u64);

            if size == 0 || size > config::MAX_PHYS_MEM as u64 {
                prop_assert_eq!(err, SyscallError::InvalidArgument as u64);
            }

            inv(&k);
        }

        #[test]
        fn vmo_create_invalid_flags_never_panics(flags in 0u64..=u32::MAX as u64) {
            let mut k = setup_kernel();
            let (err, _) = call(
                &mut k,
                num::VMO_CREATE,
                &[config::PAGE_SIZE as u64, flags, 0, 0, 0, 0],
            );

            prop_assert!(err <= SyscallError::NotFound as u64);

            if flags & !1 != 0 {
                prop_assert_eq!(err, SyscallError::InvalidArgument as u64);
            }

            inv(&k);
        }

        #[test]
        fn vmo_seal_then_resize_always_fails(new_size in boundary_size()) {
            let mut k = setup_kernel();
            let (_, hid) = call(
                &mut k,
                num::VMO_CREATE,
                &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
            );

            call(&mut k, num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

            let (err, _) = call(
                &mut k,
                num::VMO_RESIZE,
                &[hid, new_size, 0, 0, 0, 0],
            );

            prop_assert_ne!(err, 0, "resize on sealed VMO must never succeed");
            inv(&k);
        }

        #[test]
        fn vmo_snapshot_preserves_size(pages in 1usize..=8) {
            let mut k = setup_kernel();
            let size = (pages * config::PAGE_SIZE) as u64;
            let (_, hid) = call(&mut k, num::VMO_CREATE, &[size, 0, 0, 0, 0, 0]);
            let (err, snap_hid) = call(&mut k, num::VMO_SNAPSHOT, &[hid, 0, 0, 0, 0, 0]);

            prop_assert_eq!(err, 0);

            let snap_obj_id = k
                .spaces
                .get(0)
                .unwrap()
                .handles()
                .lookup(HandleId(snap_hid as u32))
                .unwrap()
                .object_id;
            let snap_size = k.vmos.get(snap_obj_id).unwrap().size();

            prop_assert_eq!(snap_size, pages * config::PAGE_SIZE);
            inv(&k);
        }
    }

    // =========================================================================
    // HANDLE PROPERTIES
    // =========================================================================

    proptest! {
        #[test]
        fn handle_dup_rights_attenuation(rights in valid_rights()) {
            let mut k = setup_kernel();
            let (_, hid) = call(
                &mut k,
                num::VMO_CREATE,
                &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
            );
            let (err, dup_hid) = call(
                &mut k,
                num::HANDLE_DUP,
                &[hid, rights, 0, 0, 0, 0],
            );

            if rights > Rights::ALL.0 as u64 {
                prop_assert_ne!(err, 0, "invalid rights should fail");
            } else if err == 0 {
                let orig = k
                    .spaces
                    .get(0)
                    .unwrap()
                    .handles()
                    .lookup(HandleId(hid as u32))
                    .unwrap();
                let dup = k
                    .spaces
                    .get(0)
                    .unwrap()
                    .handles()
                    .lookup(HandleId(dup_hid as u32))
                    .unwrap();

                prop_assert!(
                    dup.rights.is_subset_of(orig.rights),
                    "dup rights {:?} must be subset of original {:?}",
                    dup.rights,
                    orig.rights
                );
            }

            inv(&k);
        }

        #[test]
        fn handle_close_idempotent(handle_id in boundary_handle()) {
            let mut k = setup_kernel();
            let (_, hid) = call(
                &mut k,
                num::VMO_CREATE,
                &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
            );

            if handle_id == hid {
                let (err1, _) = call(&mut k, num::HANDLE_CLOSE, &[handle_id, 0, 0, 0, 0, 0]);

                prop_assert_eq!(err1, 0);

                let (err2, _) = call(&mut k, num::HANDLE_CLOSE, &[handle_id, 0, 0, 0, 0, 0]);

                prop_assert_eq!(err2, SyscallError::InvalidHandle as u64);
            } else {
                let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[handle_id, 0, 0, 0, 0, 0]);

                prop_assert!(
                    err == SyscallError::InvalidHandle as u64
                        || err == 0
                );
            }

            inv(&k);
        }

        #[test]
        fn handle_info_on_invalid_handle_never_panics(handle_id in boundary_handle()) {
            let mut k = setup_kernel();
            let (err, _) = call(&mut k, num::HANDLE_INFO, &[handle_id, 0, 0, 0, 0, 0]);

            prop_assert!(err <= SyscallError::NotFound as u64);
            inv(&k);
        }
    }

    // =========================================================================
    // EVENT PROPERTIES
    // =========================================================================

    proptest! {
        #[test]
        fn event_signal_is_or_accumulative(bits1 in boundary_u64(), bits2 in boundary_u64()) {
            let mut k = setup_kernel();
            let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            call(&mut k, num::EVENT_SIGNAL, &[hid, bits1, 0, 0, 0, 0]);
            call(&mut k, num::EVENT_SIGNAL, &[hid, bits2, 0, 0, 0, 0]);

            let obj_id = k
                .spaces
                .get(0)
                .unwrap()
                .handles()
                .lookup(HandleId(hid as u32))
                .unwrap()
                .object_id;
            let actual = k.events.get(obj_id).unwrap().bits();

            prop_assert_eq!(actual, bits1 | bits2);
            inv(&k);
        }

        #[test]
        fn event_clear_is_and_not(
            initial in boundary_u64(),
            clear_mask in boundary_u64(),
        ) {
            let mut k = setup_kernel();
            let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            call(&mut k, num::EVENT_SIGNAL, &[hid, initial, 0, 0, 0, 0]);
            call(&mut k, num::EVENT_CLEAR, &[hid, clear_mask, 0, 0, 0, 0]);

            let obj_id = k
                .spaces
                .get(0)
                .unwrap()
                .handles()
                .lookup(HandleId(hid as u32))
                .unwrap()
                .object_id;
            let actual = k.events.get(obj_id).unwrap().bits();

            prop_assert_eq!(actual, initial & !clear_mask);
            inv(&k);
        }
    }

    // =========================================================================
    // SYSCALL DISPATCH PROPERTIES
    // =========================================================================

    proptest! {
        #[test]
        fn unknown_syscall_returns_invalid_argument(
            syscall_num in 30u64..=u64::MAX,
            args in proptest::array::uniform6(0u64..=u64::MAX),
        ) {
            let mut k = setup_kernel();
            let (err, _) = call(&mut k, syscall_num, &args);

            prop_assert_eq!(err, SyscallError::InvalidArgument as u64);
            inv(&k);
        }

        #[test]
        fn pointer_free_syscalls_never_panic(
            a0 in boundary_handle(),
            a1 in boundary_u64(),
        ) {
            let mut k = setup_kernel();
            let pointer_free = [
                num::EVENT_CREATE,
                num::EVENT_SIGNAL,
                num::EVENT_CLEAR,
                num::ENDPOINT_CREATE,
                num::HANDLE_CLOSE,
                num::HANDLE_INFO,
                num::CLOCK_READ,
                num::SYSTEM_INFO,
            ];

            for &syscall in &pointer_free {
                let (err, _) = call(&mut k, syscall, &[a0, a1, 0, 0, 0, 0]);

                prop_assert!(err <= SyscallError::NotFound as u64);
            }

            inv(&k);
        }
    }

    // =========================================================================
    // MULTI-STEP SEQUENCE PROPERTIES
    // =========================================================================

    proptest! {
        #[test]
        fn create_close_cycle_preserves_invariants(
            iterations in 1usize..=50,
            obj_type in 0u8..=2,
        ) {
            let mut k = setup_kernel();

            for _ in 0..iterations {
                let hid = match obj_type {
                    0 => {
                        let (e, h) = call(
                            &mut k,
                            num::VMO_CREATE,
                            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
                        );
                        if e != 0 { break; }
                        h
                    }
                    1 => {
                        let (e, h) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
                        if e != 0 { break; }
                        h
                    }
                    _ => {
                        let (e, h) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
                        if e != 0 { break; }
                        h
                    }
                };

                let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

                prop_assert_eq!(err, 0);
            }

            inv(&k);
        }

        #[test]
        fn generation_revocation_prevents_stale_access(iterations in 1usize..=20) {
            let mut k = setup_kernel();
            let mut prev_hid = None;

            for _ in 0..iterations {
                let (err, hid) = call(
                    &mut k,
                    num::VMO_CREATE,
                    &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
                );

                if err != 0 { break; }

                if let Some(old) = prev_hid {
                    let (close_err, _) = call(
                        &mut k,
                        num::HANDLE_CLOSE,
                        &[old, 0, 0, 0, 0, 0],
                    );

                    prop_assert_eq!(close_err, 0);

                    let (info_err, _) = call(
                        &mut k,
                        num::HANDLE_INFO,
                        &[old, 0, 0, 0, 0, 0],
                    );

                    prop_assert_eq!(
                        info_err,
                        SyscallError::InvalidHandle as u64,
                        "closed handle must not be usable"
                    );
                }

                prev_hid = Some(hid);
            }

            inv(&k);
        }
    }
}
