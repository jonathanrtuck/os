//! Comprehensive verification test suite — boundary values, failure paths,
//! object lifecycles, bare-metal path coverage, and generation revocation.
//!
//! These tests are organized by the class of bug they prevent, not by the
//! module they exercise. Each category catches a specific failure mode that
//! unit tests within individual modules miss.

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use crate::{
        address_space::AddressSpace,
        config,
        syscall::{Kernel, num},
        thread::Thread,
        types::{
            AddressSpaceId, HandleId, ObjectType, Priority, Rights, SyscallError, ThreadId, VmoId,
        },
        vmo::{Vmo, VmoFlags},
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

    // =========================================================================
    // BOUNDARY VALUE TESTS
    //
    // Every packed encoding and value range is tested at its boundaries.
    // =========================================================================

    #[test]
    fn event_signal_all_64_bits_through_syscall() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid, u64::MAX, 0, 0, 0, 0]);

        let event = k.events.get(0).unwrap();

        assert_eq!(event.bits(), u64::MAX);
        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn event_clear_upper_bits_through_syscall() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let upper: u64 = 0xFFFF_FFFF_0000_0000;

        call(&mut k, num::EVENT_SIGNAL, &[hid, u64::MAX, 0, 0, 0, 0]);
        call(&mut k, num::EVENT_CLEAR, &[hid, upper, 0, 0, 0, 0]);

        assert_eq!(k.events.get(0).unwrap().bits(), 0x0000_0000_FFFF_FFFF);
        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn event_wait_each_bit_position() {
        for bit in [0, 1, 15, 16, 31, 32, 47, 48, 62, 63] {
            let mut k = setup_kernel();
            let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
            let mask = 1u64 << bit;

            call(&mut k, num::EVENT_SIGNAL, &[hid, mask, 0, 0, 0, 0]);

            let (err, value) = call(&mut k, num::EVENT_WAIT, &[hid, mask, 0, 0, 0, 0]);

            assert_eq!(err, 0, "bit {bit}: unexpected error");
            assert_eq!(value, hid, "bit {bit}: wrong handle returned");

            {
                let v = crate::invariants::verify(&*k);

                assert!(v.is_empty(), "invariant violations: {:?}", v);
            }
        }
    }

    #[test]
    fn vmo_create_page_boundary_sizes() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::VMO_CREATE, &[1, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0, "size=1 should succeed");

        let (err, _) = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        );

        assert_eq!(err, 0, "size=PAGE_SIZE should succeed");

        let (err, _) = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64 - 1, 0, 0, 0, 0, 0],
        );

        assert_eq!(err, 0, "size=PAGE_SIZE-1 should succeed");

        let (err, _) = call(&mut k, num::VMO_CREATE, &[0, 0, 0, 0, 0, 0]);

        assert_eq!(
            err,
            SyscallError::InvalidArgument as u64,
            "size=0 must fail"
        );

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn vmo_create_rejects_oversized() {
        let mut k = setup_kernel();
        let too_big = (config::MAX_PHYS_MEM as u64) + 1;
        let (err, _) = call(&mut k, num::VMO_CREATE, &[too_big, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn handle_info_encodes_all_object_types() {
        let mut k = setup_kernel();
        let types = [
            (num::VMO_CREATE, &[4096u64, 0, 0, 0, 0, 0], ObjectType::Vmo),
            (num::EVENT_CREATE, &[0; 6], ObjectType::Event),
            (num::ENDPOINT_CREATE, &[0; 6], ObjectType::Endpoint),
        ];

        for (syscall, args, expected_type) in &types {
            let (err, hid) = call(&mut k, *syscall, args);

            assert_eq!(err, 0);

            let (err, info) = call(&mut k, num::HANDLE_INFO, &[hid, 0, 0, 0, 0, 0]);

            assert_eq!(err, 0);

            let obj_type = (info >> 32) as u8;

            assert_eq!(obj_type, *expected_type as u8);
        }

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn all_rights_bits_preserved_through_dup() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let individual_rights = [
            Rights::READ,
            Rights::WRITE,
            Rights::EXECUTE,
            Rights::MAP,
            Rights::DUP,
            Rights::TRANSFER,
            Rights::SIGNAL,
            Rights::WAIT,
            Rights::SPAWN,
        ];

        for right in &individual_rights {
            let (err, dup_hid) = call(&mut k, num::HANDLE_DUP, &[hid, right.0 as u64, 0, 0, 0, 0]);

            assert_eq!(err, 0, "dup with right {:?} failed", right);

            let (_, info) = call(&mut k, num::HANDLE_INFO, &[dup_hid, 0, 0, 0, 0, 0]);
            let rights = (info & 0xFFFF_FFFF) as u32;

            assert_eq!(rights, right.0, "right {:?} not preserved", right);
        }

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn priority_values_all_valid() {
        let mut k = setup_kernel();
        let (_, tid_hid) = call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);

        for pri_val in 0..=3u64 {
            let (err, _) = call(
                &mut k,
                num::THREAD_SET_PRIORITY,
                &[tid_hid, pri_val, 0, 0, 0, 0],
            );

            assert_eq!(err, 0, "priority {} should be valid", pri_val);
        }

        let (err, _) = call(&mut k, num::THREAD_SET_PRIORITY, &[tid_hid, 4, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn affinity_values_all_valid() {
        let mut k = setup_kernel();
        let (_, tid_hid) = call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);

        for hint in 0..=2u64 {
            let (err, _) = call(
                &mut k,
                num::THREAD_SET_AFFINITY,
                &[tid_hid, hint, 0, 0, 0, 0],
            );

            assert_eq!(err, 0, "affinity {} should be valid", hint);
        }

        let (err, _) = call(&mut k, num::THREAD_SET_AFFINITY, &[tid_hid, 3, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn system_info_all_selectors() {
        let mut k = setup_kernel();
        let (err, val) = call(&mut k, num::SYSTEM_INFO, &[0, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, config::PAGE_SIZE as u64);

        let (err, val) = call(&mut k, num::SYSTEM_INFO, &[1, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, crate::endpoint::MSG_SIZE as u64);

        let (err, val) = call(&mut k, num::SYSTEM_INFO, &[2, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, 1);

        let (err, _) = call(&mut k, num::SYSTEM_INFO, &[3, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn every_syscall_number_unknown_rejected() {
        let mut k = setup_kernel();

        for num in 30..=35 {
            let (err, _) = call(&mut k, num, &[0; 6]);

            assert_eq!(
                err,
                SyscallError::InvalidArgument as u64,
                "syscall {} should be rejected",
                num
            );
        }

        let (err, _) = call(&mut k, u64::MAX, &[0; 6]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    // =========================================================================
    // FAILURE PATH TESTS
    //
    // Every multi-step syscall is tested with failures at each step.
    // =========================================================================

    #[test]
    fn vmo_create_rollback_on_handle_table_full() {
        let mut k = setup_kernel();

        // Fill the handle table.
        for _ in 0..config::MAX_HANDLES {
            let (err, _) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

            if err != 0 {
                break;
            }
        }

        let vmo_count_before = k.vmos.count();
        // Next create should fail — and must not leak a VMO.
        let (err, _) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0);
        assert_eq!(
            k.vmos.count(),
            vmo_count_before,
            "VMO leaked on handle alloc failure"
        );

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn event_create_rollback_on_handle_table_full() {
        let mut k = setup_kernel();

        for _ in 0..config::MAX_HANDLES {
            let (err, _) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            if err != 0 {
                break;
            }
        }

        let event_count_before = k.events.count();
        let (err, _) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_ne!(err, 0);
        assert_eq!(k.events.count(), event_count_before, "Event leaked");

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn endpoint_create_rollback_on_handle_table_full() {
        let mut k = setup_kernel();

        for _ in 0..config::MAX_HANDLES {
            let (err, _) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

            if err != 0 {
                break;
            }
        }

        let ep_count_before = k.endpoints.count();
        let (err, _) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        assert_ne!(err, 0);
        assert_eq!(k.endpoints.count(), ep_count_before, "Endpoint leaked");

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn thread_create_rollback_on_handle_table_full() {
        let mut k = setup_kernel();

        for _ in 0..config::MAX_HANDLES {
            let (err, _) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

            if err != 0 {
                break;
            }
        }

        let thread_count_before = k.threads.count();
        let (err, _) = call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);

        assert_ne!(err, 0);
        assert_eq!(k.threads.count(), thread_count_before, "Thread leaked");

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn wrong_handle_type_for_every_typed_syscall() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (_, event_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, _ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        // VMO syscalls with event handle.
        let vmo_syscalls = [
            num::VMO_MAP,
            num::VMO_SNAPSHOT,
            num::VMO_SEAL,
            num::VMO_RESIZE,
        ];

        for &sc in &vmo_syscalls {
            let (err, _) = call(&mut k, sc, &[event_hid, 0, 0, 0, 0, 0]);

            assert_eq!(
                err,
                SyscallError::WrongHandleType as u64,
                "syscall {} accepted wrong type",
                sc
            );
        }

        // Event syscalls with VMO handle.
        for &sc in &[num::EVENT_SIGNAL, num::EVENT_CLEAR, num::EVENT_BIND_IRQ] {
            let (err, _) = call(&mut k, sc, &[vmo_hid, 0, 0, 0, 0, 0]);

            assert_eq!(
                err,
                SyscallError::WrongHandleType as u64,
                "syscall {} accepted wrong type",
                sc
            );
        }

        // Thread syscalls with event handle.
        for &sc in &[num::THREAD_SET_PRIORITY, num::THREAD_SET_AFFINITY] {
            let (err, _) = call(&mut k, sc, &[event_hid, 0, 0, 0, 0, 0]);

            assert_eq!(
                err,
                SyscallError::WrongHandleType as u64,
                "syscall {} accepted wrong type",
                sc
            );
        }

        // IPC syscalls with VMO handle.
        let (err, _) = call(&mut k, num::CALL, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::WrongHandleType as u64);

        let (err, _) = call(&mut k, num::RECV, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::WrongHandleType as u64);

        let (err, _) = call(&mut k, num::REPLY, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::WrongHandleType as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn handle_close_then_use_returns_invalid() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        call(&mut k, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

        let (err, _) = call(&mut k, num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidHandle as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn double_close_returns_invalid() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidHandle as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn insufficient_rights_for_signal() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let read_only = Rights::READ.0 as u64;
        let (_, dup_hid) = call(&mut k, num::HANDLE_DUP, &[hid, read_only, 0, 0, 0, 0]);
        let (err, _) = call(&mut k, num::EVENT_SIGNAL, &[dup_hid, 0b1, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InsufficientRights as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn insufficient_rights_for_map() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let no_map = Rights::READ.0 as u64;
        let (_, dup) = call(&mut k, num::HANDLE_DUP, &[hid, no_map, 0, 0, 0, 0]);
        let perms = (Rights::READ.0 | Rights::MAP.0) as u64;
        let (err, _) = call(&mut k, num::VMO_MAP, &[dup, 0, perms, 0, 0, 0]);

        assert_eq!(err, SyscallError::InsufficientRights as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn vmo_resize_to_zero() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (err, _) = call(&mut k, num::VMO_RESIZE, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let vmo = k.vmos.get(0).unwrap();

        assert_eq!(vmo.size(), 0);
        assert_eq!(vmo.page_count(), 0);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    // =========================================================================
    // GENERATION REVOCATION TESTS
    //
    // Verify that stale handles are rejected after object dealloc+realloc.
    // =========================================================================

    #[test]
    fn generation_mismatch_after_dealloc_realloc() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let old_gen = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .generation;
        let old_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        // Manually dealloc the VMO (simulating external destruction).
        k.vmos.dealloc(old_obj_id);

        // Reallocate a new VMO in the same slot.
        let new_vmo = Vmo::new(VmoId(0), 8192, VmoFlags::NONE);
        let (new_idx, new_gen) = k.vmos.alloc(new_vmo).unwrap();

        assert_eq!(new_idx, old_obj_id, "should reuse same slot");
        assert_ne!(old_gen, new_gen, "generation must differ");

        // The old handle still points to the same slot but with stale generation.
        let (err, _) = call(&mut k, num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::GenerationMismatch as u64);
    }

    #[test]
    fn fresh_handle_after_realloc_works() {
        let mut k = setup_kernel();
        let (_, hid1) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        call(&mut k, num::HANDLE_CLOSE, &[hid1, 0, 0, 0, 0, 0]);
        k.vmos.dealloc(0);

        let (err, hid2) = call(&mut k, num::VMO_CREATE, &[8192, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::VMO_SEAL, &[hid2, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    // =========================================================================
    // OBJECT LIFECYCLE TESTS
    //
    // Create → use → destroy → verify cleanup for every object type.
    // =========================================================================

    #[test]
    fn vmo_full_lifecycle() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(k.vmos.count(), 1);

        // Map, then unmap.
        let perms = (Rights::READ.0 | Rights::MAP.0) as u64;
        let (_, va) = call(&mut k, num::VMO_MAP, &[hid, 0, perms, 0, 0, 0]);

        assert!(va > 0);

        call(&mut k, num::VMO_UNMAP, &[va, 0, 0, 0, 0, 0]);

        // Snapshot.
        let (_, snap_hid) = call(&mut k, num::VMO_SNAPSHOT, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(k.vmos.count(), 2);

        // Close both.
        call(&mut k, num::HANDLE_CLOSE, &[snap_hid, 0, 0, 0, 0, 0]);
        call(&mut k, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn event_full_lifecycle() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_eq!(k.events.count(), 1);

        // Signal and check.
        call(&mut k, num::EVENT_SIGNAL, &[hid, 0xFF, 0, 0, 0, 0]);

        assert_eq!(k.events.get(0).unwrap().bits(), 0xFF);

        // Wait (should return immediately).
        let (err, _) = call(&mut k, num::EVENT_WAIT, &[hid, 0x0F, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // Clear.
        call(&mut k, num::EVENT_CLEAR, &[hid, 0xFF, 0, 0, 0, 0]);

        assert_eq!(k.events.get(0).unwrap().bits(), 0);

        // Close.
        call(&mut k, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn endpoint_full_lifecycle() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(k.endpoints.count(), 1);

        // Close.
        call(&mut k, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn thread_full_lifecycle() {
        let mut k = setup_kernel();
        let initial_count = k.threads.count();
        let (err, tid_hid) = call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 42, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(k.threads.count(), initial_count + 1);

        // Set priority.
        call(&mut k, num::THREAD_SET_PRIORITY, &[tid_hid, 3, 0, 0, 0, 0]);

        // Close handle.
        call(&mut k, num::HANDLE_CLOSE, &[tid_hid, 0, 0, 0, 0, 0]);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn space_create_and_destroy_lifecycle() {
        let mut k = setup_kernel();
        let initial_space_count = k.spaces.count();
        let (err, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(k.spaces.count(), initial_space_count + 1);

        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(k.spaces.count(), initial_space_count);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn space_destroy_cleans_up_vmo_refcounts() {
        let mut k = setup_kernel();
        // Create a child space.
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let target_space_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(space_hid as u32))
            .unwrap()
            .object_id;
        // Create a VMO and give the child space a handle to it.
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let vmo_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(vmo_hid as u32))
            .unwrap()
            .object_id;

        k.vmos.get_mut(vmo_obj_id).unwrap().add_ref();

        let vmo_gen = k.vmos.generation(vmo_obj_id);
        let target = k.spaces.get_mut(target_space_id).unwrap();

        target
            .handles_mut()
            .allocate(ObjectType::Vmo, vmo_obj_id, Rights::ALL, vmo_gen)
            .unwrap();

        assert_eq!(k.vmos.get(vmo_obj_id).unwrap().refcount(), 2);

        // Destroy the child space — should release one refcount.
        call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_eq!(
            k.vmos.get(vmo_obj_id).unwrap().refcount(),
            1,
            "VMO refcount should be decremented on space destroy"
        );

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn space_destroy_signals_peer_closed() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let target_space_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(space_hid as u32))
            .unwrap()
            .object_id;
        // Create an endpoint and give the child space a handle.
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;
        let ep_gen = k.endpoints.generation(ep_obj_id);
        let target = k.spaces.get_mut(target_space_id).unwrap();

        target
            .handles_mut()
            .allocate(ObjectType::Endpoint, ep_obj_id, Rights::ALL, ep_gen)
            .unwrap();

        assert!(!k.endpoints.get(ep_obj_id).unwrap().is_peer_closed());

        // Destroy the child space — should signal PeerClosed.
        call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert!(
            k.endpoints.get(ep_obj_id).unwrap().is_peer_closed(),
            "endpoint should be peer_closed after space destroy"
        );

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    // =========================================================================
    // BARE-METAL PATH COVERAGE
    //
    // Verify RegisterState is correctly initialized for new threads.
    // =========================================================================

    #[test]
    fn thread_create_initializes_register_state() {
        let mut k = setup_kernel();
        let (err, _) = call(
            &mut k,
            num::THREAD_CREATE,
            &[0xDEAD_0000, 0xBEEF_0000, 42, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let thread = k.threads.get(1).unwrap();
        let rs = thread
            .register_state()
            .expect("thread_create must init RegisterState");

        assert_eq!(rs.pc, 0xDEAD_0000, "pc must be entry_point");
        assert_eq!(rs.sp, 0xBEEF_0000, "sp must be stack_top");
        assert_eq!(rs.gprs[0], 42, "x0 must be arg");
        assert_eq!(rs.pstate, 0, "pstate must be EL0t");

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn thread_create_in_initializes_register_state() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let (err, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_hid, 0xCAFE_0000, 0xFACE_0000, 99, 0, 0],
        );

        assert_eq!(err, 0);

        // Find the new thread (it's the last one allocated).
        let new_tid = k.threads.count() as u32 - 1;
        let thread = k.threads.get(new_tid).unwrap();
        let rs = thread
            .register_state()
            .expect("thread_create_in must init RegisterState");

        assert_eq!(rs.pc, 0xCAFE_0000);
        assert_eq!(rs.sp, 0xFACE_0000);
        assert_eq!(rs.gprs[0], 99);
        assert_eq!(rs.pstate, 0);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    // =========================================================================
    // IRQ BINDING EDGE CASES
    // =========================================================================

    #[test]
    fn irq_bind_boundary_intids() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        // First valid SPI.
        let (err, _) = call(&mut k, num::EVENT_BIND_IRQ, &[hid, 32, 0b1, 0, 0, 0]);

        assert_eq!(err, 0);

        // Last valid INTID.
        let (err2, _) = call(
            &mut k,
            num::EVENT_BIND_IRQ,
            &[hid, (config::MAX_IRQS - 1) as u64, 0b10, 0, 0, 0],
        );

        assert_eq!(err2, 0);

        // Just past valid range.
        let (err3, _) = call(
            &mut k,
            num::EVENT_BIND_IRQ,
            &[hid, config::MAX_IRQS as u64, 0b100, 0, 0, 0],
        );

        assert_eq!(err3, SyscallError::InvalidArgument as u64);

        // SGI range (kernel-internal).
        let (err4, _) = call(&mut k, num::EVENT_BIND_IRQ, &[hid, 0, 0b1, 0, 0, 0]);

        assert_eq!(err4, SyscallError::InvalidArgument as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    // =========================================================================
    // ENDPOINT-EVENT BINDING EDGE CASES
    // =========================================================================

    #[test]
    fn endpoint_bind_event_wrong_types_rejected() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let (_, ev_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        // VMO as endpoint arg.
        let (err, _) = call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[vmo_hid, ev_hid, 0, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::WrongHandleType as u64);

        // VMO as event arg.
        let (err, _) = call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[ep_hid, vmo_hid, 0, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::WrongHandleType as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn endpoint_bind_event_double_bind_rejected() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let (_, ev1_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, ev2_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (err, _) = call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[ep_hid, ev1_hid, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let (err, _) = call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[ep_hid, ev2_hid, 0, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    // =========================================================================
    // SELF-DESTROY PREVENTION
    // =========================================================================

    #[test]
    fn space_destroy_self_rejected() {
        let mut k = setup_kernel();
        // space 0 handle doesn't exist in the handle table by default.
        // Create a handle to space 0 in space 0.
        let space_gen = k.spaces.generation(0);

        k.spaces
            .get_mut(0)
            .unwrap()
            .handles_mut()
            .allocate(ObjectType::AddressSpace, 0, Rights::ALL, space_gen)
            .unwrap();

        // Find the handle ID (it's the newest one).
        let hid = k.spaces.get(0).unwrap().handles().count() as u64 - 1;

        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    // =========================================================================
    // VMO MAP_INTO CROSS-SPACE
    // =========================================================================

    #[test]
    fn vmo_map_into_cross_space() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let perms = Rights::READ.0 as u64;
        let (err, va) = call(
            &mut k,
            num::VMO_MAP_INTO,
            &[vmo_hid, space_hid, 0, perms, 0, 0],
        );

        assert_eq!(err, 0);
        assert!(va > 0);

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }

    #[test]
    fn vmo_set_pager_through_syscall() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let (err, _) = call(&mut k, num::VMO_SET_PAGER, &[vmo_hid, ep_hid, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(k.vmos.get(0).unwrap().pager().is_some());

        {
            let v = crate::invariants::verify(&*k);

            assert!(v.is_empty(), "invariant violations: {:?}", v);
        }
    }
}
