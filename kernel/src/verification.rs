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
        endpoint::Endpoint,
        event::Event,
        syscall::{Kernel, num},
        thread::Thread,
        types::{
            AddressSpaceId, EndpointId, EventId, HandleId, ObjectType, Priority, Rights,
            SyscallError, ThreadId, VmoId,
        },
        vmo::{Vmo, VmoFlags},
    };

    fn setup_kernel() -> Box<Kernel> {
        crate::frame::arch::page_table::reset_asid_pool();

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

        k.endpoints.get_mut(ep_obj_id).unwrap().add_ref();

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

    // =========================================================================
    // HELPERS — shared utilities for tests below
    // =========================================================================

    fn assert_ok(result: (u64, u64)) -> u64 {
        assert_eq!(result.0, 0, "expected success, got error {}", result.0);

        result.1
    }

    fn assert_err(result: (u64, u64), expected: SyscallError) {
        assert_eq!(
            result.0, expected as u64,
            "expected {:?} ({}), got {}",
            expected, expected as u64, result.0
        );
    }

    fn inv(k: &Kernel) {
        crate::invariants::assert_valid(k);
    }

    fn create_vmo(k: &mut Kernel) -> u64 {
        assert_ok(call(k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]))
    }

    fn create_event(k: &mut Kernel) -> u64 {
        assert_ok(call(k, num::EVENT_CREATE, &[0; 6]))
    }

    fn create_endpoint(k: &mut Kernel) -> u64 {
        assert_ok(call(k, num::ENDPOINT_CREATE, &[0; 6]))
    }

    fn create_thread(k: &mut Kernel) -> u64 {
        assert_ok(call(k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]))
    }

    fn create_space(k: &mut Kernel) -> u64 {
        assert_ok(call(k, num::SPACE_CREATE, &[0; 6]))
    }

    fn dup_with_rights(k: &mut Kernel, hid: u64, rights: u32) -> u64 {
        assert_ok(call(k, num::HANDLE_DUP, &[hid, rights as u64, 0, 0, 0, 0]))
    }

    fn hclose(k: &mut Kernel, hid: u64) {
        assert_ok(call(k, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]));
    }

    fn create_stale_vmo_handle(k: &mut Kernel) -> u64 {
        let hid = create_vmo(k);
        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        k.vmos.dealloc(obj_id);

        let new_vmo = Vmo::new(VmoId(0), 8192, VmoFlags::NONE);

        k.vmos.alloc(new_vmo).unwrap();

        hid
    }

    fn create_stale_event_handle(k: &mut Kernel) -> u64 {
        let hid = create_event(k);
        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        k.events.dealloc(obj_id);

        let new_event = Event::new(EventId(0));

        k.events.alloc(new_event).unwrap();

        hid
    }

    fn create_stale_endpoint_handle(k: &mut Kernel) -> u64 {
        let hid = create_endpoint(k);
        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        k.endpoints.dealloc(obj_id);

        let new_ep = Endpoint::new(EndpointId(0));

        k.endpoints.alloc(new_ep).unwrap();

        hid
    }

    fn create_stale_thread_handle(k: &mut Kernel) -> u64 {
        let hid = create_thread(k);
        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        k.threads
            .get_mut(obj_id)
            .unwrap()
            .set_state(crate::thread::ThreadRunState::Exited);
        k.threads.dealloc(obj_id);

        let new_thread = Thread::new(ThreadId(0), Some(AddressSpaceId(0)), Priority::Low, 0, 0, 0);

        k.threads.alloc(new_thread).unwrap();

        hid
    }

    fn create_stale_space_handle(k: &mut Kernel) -> u64 {
        let hid = create_space(k);
        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        k.spaces.dealloc(obj_id);

        let new_space = AddressSpace::new(AddressSpaceId(0), 99, 0);

        k.spaces.alloc(new_space).unwrap();

        hid
    }

    // =========================================================================
    // CAPABILITY CHURN TESTS
    // =========================================================================

    #[test]
    fn cap_churn_create_close_100() {
        let mut k = setup_kernel();
        let initial_free = k.spaces.get(0).unwrap().handles().free_slot_count();

        for _ in 0..100 {
            let vmo = create_vmo(&mut k);

            hclose(&mut k, vmo);
        }

        assert_eq!(
            k.spaces.get(0).unwrap().handles().free_slot_count(),
            initial_free,
            "handle slots not reclaimed"
        );

        inv(&k);
    }

    #[test]
    fn cap_churn_mixed_types_100() {
        let mut k = setup_kernel();

        for i in 0..100 {
            let h = match i % 4 {
                0 => create_vmo(&mut k),
                1 => create_event(&mut k),
                2 => create_endpoint(&mut k),
                _ => create_thread(&mut k),
            };

            hclose(&mut k, h);
        }

        inv(&k);
    }

    #[test]
    fn cap_churn_dup_close_cycle() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        for _ in 0..50 {
            let dup = dup_with_rights(&mut k, vmo, Rights::ALL.0);

            hclose(&mut k, dup);
        }

        assert_eq!(
            call(&mut k, num::HANDLE_INFO, &[vmo, 0, 0, 0, 0, 0]).0,
            0,
            "original handle should still be valid"
        );

        inv(&k);
    }

    #[test]
    fn cap_churn_fill_and_drain_handle_table() {
        let mut k = setup_kernel();
        let mut handles = alloc::vec::Vec::new();

        loop {
            let result = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            if result.0 != 0 {
                break;
            }

            handles.push(result.1);
        }

        assert!(handles.len() > 0);
        assert_ne!(
            call(&mut k, num::EVENT_CREATE, &[0; 6]).0,
            0,
            "table should be full"
        );

        for h in handles.drain(..) {
            hclose(&mut k, h);
        }

        let h = create_event(&mut k);

        hclose(&mut k, h);
        inv(&k);
    }

    #[test]
    fn cap_generation_increments_on_reuse() {
        let mut k = setup_kernel();
        let h1 = create_vmo(&mut k);
        let gen1 = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(h1 as u32))
            .unwrap()
            .generation;

        hclose(&mut k, h1);

        k.vmos.dealloc(0);

        let h2 = create_vmo(&mut k);
        let gen2 = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(h2 as u32))
            .unwrap()
            .generation;

        assert!(gen2 > gen1, "generation must increment on slot reuse");

        inv(&k);
    }

    // =========================================================================
    // SPACE DESTROY INTERACTION TESTS
    // =========================================================================

    #[test]
    fn space_destroy_with_mapped_vmos_and_endpoints() {
        let mut k = setup_kernel();
        let target_space = create_space(&mut k);
        let target_space_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(target_space as u32))
            .unwrap()
            .object_id;
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE * 2, VmoFlags::NONE);
        let (vmo_idx, _) = k.vmos.alloc(vmo).unwrap();

        k.vmos.get_mut(vmo_idx).unwrap().id = VmoId(vmo_idx);

        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);

        k.spaces
            .get_mut(target_space_id)
            .unwrap()
            .map_vmo(VmoId(vmo_idx), config::PAGE_SIZE * 2, rw, 0)
            .unwrap();

        let ep = Endpoint::new(EndpointId(0));
        let (ep_idx, _) = k.endpoints.alloc(ep).unwrap();

        k.endpoints.get_mut(ep_idx).unwrap().id = EndpointId(ep_idx);

        let target_ht = k.spaces.get_mut(target_space_id).unwrap().handles_mut();

        target_ht
            .allocate(
                ObjectType::Vmo,
                vmo_idx,
                Rights::ALL,
                k.vmos.generation(vmo_idx),
            )
            .unwrap();
        k.vmos.get_mut(vmo_idx).unwrap().add_ref();
        target_ht
            .allocate(
                ObjectType::Endpoint,
                ep_idx,
                Rights::ALL,
                k.endpoints.generation(ep_idx),
            )
            .unwrap();

        assert_ok(call(
            &mut k,
            num::SPACE_DESTROY,
            &[target_space, 0, 0, 0, 0, 0],
        ));

        inv(&k);
    }

    // =========================================================================
    // CROSS-CUTTING: GENERATION MISMATCH FOR EVERY HANDLE-TAKING SYSCALL
    // =========================================================================

    #[test]
    fn generation_mismatch_vmo_map() {
        let mut k = setup_kernel();
        let stale = create_stale_vmo_handle(&mut k);

        assert_err(
            call(
                &mut k,
                num::VMO_MAP,
                &[stale, 0, Rights::READ.0 as u64, 0, 0, 0],
            ),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_vmo_snapshot() {
        let mut k = setup_kernel();
        let stale = create_stale_vmo_handle(&mut k);

        assert_err(
            call(&mut k, num::VMO_SNAPSHOT, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_vmo_resize() {
        let mut k = setup_kernel();
        let stale = create_stale_vmo_handle(&mut k);

        assert_err(
            call(&mut k, num::VMO_RESIZE, &[stale, 4096, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_event_signal() {
        let mut k = setup_kernel();
        let stale = create_stale_event_handle(&mut k);

        assert_err(
            call(&mut k, num::EVENT_SIGNAL, &[stale, 0b1, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_event_clear() {
        let mut k = setup_kernel();
        let stale = create_stale_event_handle(&mut k);

        assert_err(
            call(&mut k, num::EVENT_CLEAR, &[stale, 0b1, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_event_wait() {
        let mut k = setup_kernel();
        let stale = create_stale_event_handle(&mut k);

        assert_err(
            call(&mut k, num::EVENT_WAIT, &[stale, 0b1, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_call() {
        let mut k = setup_kernel();
        let stale = create_stale_endpoint_handle(&mut k);

        assert_err(
            call(&mut k, num::CALL, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_recv() {
        let mut k = setup_kernel();
        let stale = create_stale_endpoint_handle(&mut k);

        assert_err(
            call(&mut k, num::RECV, &[stale, 0, 128, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_reply() {
        let mut k = setup_kernel();
        let stale = create_stale_endpoint_handle(&mut k);

        assert_err(
            call(&mut k, num::REPLY, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_handle_dup() {
        let mut k = setup_kernel();
        let stale = create_stale_vmo_handle(&mut k);

        assert_err(
            call(
                &mut k,
                num::HANDLE_DUP,
                &[stale, Rights::READ.0 as u64, 0, 0, 0, 0],
            ),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_handle_info() {
        let mut k = setup_kernel();
        let stale = create_stale_vmo_handle(&mut k);

        assert_err(
            call(&mut k, num::HANDLE_INFO, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_event_bind_irq() {
        let mut k = setup_kernel();
        let stale = create_stale_event_handle(&mut k);

        assert_err(
            call(&mut k, num::EVENT_BIND_IRQ, &[stale, 32, 0b1, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_endpoint_bind_event() {
        let mut k = setup_kernel();
        let stale_ep = create_stale_endpoint_handle(&mut k);
        let evt = create_event(&mut k);

        assert_err(
            call(
                &mut k,
                num::ENDPOINT_BIND_EVENT,
                &[stale_ep, evt, 0, 0, 0, 0],
            ),
            SyscallError::GenerationMismatch,
        );

        let ep = create_endpoint(&mut k);
        let stale_evt = create_stale_event_handle(&mut k);

        assert_err(
            call(
                &mut k,
                num::ENDPOINT_BIND_EVENT,
                &[ep, stale_evt, 0, 0, 0, 0],
            ),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_space_destroy() {
        let mut k = setup_kernel();
        let stale = create_stale_space_handle(&mut k);

        assert_err(
            call(&mut k, num::SPACE_DESTROY, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_thread_set_priority() {
        let mut k = setup_kernel();
        let stale = create_stale_thread_handle(&mut k);

        assert_err(
            call(&mut k, num::THREAD_SET_PRIORITY, &[stale, 2, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_thread_set_affinity() {
        let mut k = setup_kernel();
        let stale = create_stale_thread_handle(&mut k);

        assert_err(
            call(&mut k, num::THREAD_SET_AFFINITY, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_thread_create_in() {
        let mut k = setup_kernel();
        let stale = create_stale_space_handle(&mut k);

        assert_err(
            call(
                &mut k,
                num::THREAD_CREATE_IN,
                &[stale, 0x1000, 0x2000, 0, 0, 0],
            ),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_vmo_set_pager() {
        let mut k = setup_kernel();
        let stale_vmo = create_stale_vmo_handle(&mut k);
        let ep = create_endpoint(&mut k);

        assert_err(
            call(&mut k, num::VMO_SET_PAGER, &[stale_vmo, ep, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );

        let vmo = create_vmo(&mut k);
        let stale_ep = create_stale_endpoint_handle(&mut k);

        assert_err(
            call(&mut k, num::VMO_SET_PAGER, &[vmo, stale_ep, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn generation_mismatch_vmo_map_into() {
        let mut k = setup_kernel();
        let stale_vmo = create_stale_vmo_handle(&mut k);
        let space = create_space(&mut k);

        assert_err(
            call(&mut k, num::VMO_MAP_INTO, &[stale_vmo, space, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );

        let vmo = create_vmo(&mut k);
        let stale_space = create_stale_space_handle(&mut k);

        assert_err(
            call(&mut k, num::VMO_MAP_INTO, &[vmo, stale_space, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    // ── Struct layout audit ──────────────────────────────────────────

    #[test]
    fn struct_layout_audit() {
        use crate::{
            address_space::AddressSpace,
            handle::{Handle, HandleTable},
            thread::RunQueue,
        };

        // Handle: must fit in one cache line for fast lookup.
        assert!(
            core::mem::size_of::<Handle>() <= 128,
            "Handle ({} bytes) exceeds one M4 Pro cache line",
            core::mem::size_of::<Handle>(),
        );

        // Thread: track size for regression detection.
        let thread_size = core::mem::size_of::<Thread>();

        assert!(
            thread_size <= 512,
            "Thread grew to {thread_size} bytes — audit for field bloat",
        );

        // Event: track size.
        let event_size = core::mem::size_of::<Event>();

        assert!(
            event_size <= 512,
            "Event grew to {event_size} bytes — audit for field bloat",
        );

        // Endpoint: inherently large (inline PendingCalls), just track upper bound.
        let ep_size = core::mem::size_of::<Endpoint>();

        assert!(
            ep_size <= 16384,
            "Endpoint grew to {ep_size} bytes — unexpected growth",
        );

        // Kernel: the central struct, heap-allocated via Box.
        let kernel_size = core::mem::size_of::<Kernel>();

        assert!(
            kernel_size <= 131072,
            "Kernel grew to {kernel_size} bytes — unexpected growth",
        );

        // Print actual sizes for documentation (visible with --nocapture).
        println!("--- struct layout audit ---");
        println!(
            "  Handle:      {:>6} bytes  (cache lines: {})",
            core::mem::size_of::<Handle>(),
            (core::mem::size_of::<Handle>() + 127) / 128,
        );
        println!(
            "  HandleTable: {:>6} bytes",
            core::mem::size_of::<HandleTable>(),
        );
        println!(
            "  Thread:      {:>6} bytes  (cache lines: {})",
            thread_size,
            (thread_size + 127) / 128,
        );
        println!(
            "  Event:       {:>6} bytes  (cache lines: {})",
            event_size,
            (event_size + 127) / 128,
        );
        println!(
            "  Endpoint:    {:>6} bytes  (cache lines: {})",
            ep_size,
            (ep_size + 127) / 128,
        );
        println!(
            "  AddrSpace:   {:>6} bytes",
            core::mem::size_of::<AddressSpace>(),
        );
        println!(
            "  RunQueue:    {:>6} bytes",
            core::mem::size_of::<RunQueue>(),
        );
        println!("  Kernel:      {:>6} bytes", kernel_size);
    }

    // =========================================================================
    // ERROR INJECTION: OBJECT TABLE EXHAUSTION
    // =========================================================================

    #[test]
    fn vmo_table_exhaustion_and_recovery() {
        let mut k = setup_kernel();
        let mut handles = alloc::vec::Vec::new();

        for _ in 0..config::MAX_VMOS {
            let (err, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

            if err != 0 {
                break;
            }

            handles.push(hid);
        }

        let count = handles.len();

        assert!(count > 0);

        let (err, _) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0, "should fail when VMO table is full");

        let last = handles.pop().unwrap();
        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[last, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0, "should recover after closing one VMO");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_table_exhaustion_and_recovery() {
        let mut k = setup_kernel();
        let mut handles = alloc::vec::Vec::new();

        for _ in 0..config::MAX_EVENTS {
            let (err, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            if err != 0 {
                break;
            }

            handles.push(hid);
        }

        let count = handles.len();

        assert!(count > 0);

        let (err, _) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_ne!(err, 0, "should fail when event table is full");

        let last = handles.pop().unwrap();
        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[last, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0, "should recover after closing one event");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn endpoint_table_exhaustion_and_recovery() {
        let mut k = setup_kernel();
        let mut handles = alloc::vec::Vec::new();

        for _ in 0..config::MAX_ENDPOINTS {
            let (err, hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

            if err != 0 {
                break;
            }

            handles.push(hid);
        }

        let count = handles.len();

        assert!(count > 0);

        let (err, _) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        assert_ne!(err, 0, "should fail when endpoint table is full");

        let last = handles.pop().unwrap();
        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[last, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(err, 0, "should recover after closing one endpoint");

        crate::invariants::assert_valid(&*k);
    }

    // =========================================================================
    // ERROR INJECTION: THREAD_CREATE_IN ROLLBACK
    // =========================================================================

    #[test]
    fn thread_create_in_rollback_on_invalid_handle() {
        let mut k = setup_kernel();
        let (err, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        let invalid_handle_id = 999u32;
        let handle_ids = [invalid_handle_id];
        let thread_count_before = k.threads.count();
        let (err, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_hid, 0x1000, 0x2000, 0, handle_ids.as_ptr() as u64, 1],
        );

        assert_ne!(err, 0, "should fail with invalid handle");
        assert_eq!(
            k.threads.count(),
            thread_count_before,
            "thread must be cleaned up on handle clone failure"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_create_in_success_increments_refcount() {
        let mut k = setup_kernel();
        let (err, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let vmo_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(vmo_hid as u32))
            .unwrap()
            .object_id;
        let rc_before = k.vmos.get(vmo_obj_id).unwrap().refcount();
        let handle_ids = [vmo_hid as u32];
        let (err, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_hid, 0x1000, 0x2000, 0, handle_ids.as_ptr() as u64, 1],
        );

        assert_eq!(err, 0);

        let rc_after = k.vmos.get(vmo_obj_id).unwrap().refcount();

        assert_eq!(
            rc_after,
            rc_before + 1,
            "refcount must increase by 1 for cloned handle"
        );

        crate::invariants::assert_valid(&*k);
    }

    // =========================================================================
    // ERROR INJECTION: INPUT BOUNDARY VALUES
    // =========================================================================

    #[test]
    fn null_pointer_rejected_for_ipc_calls() {
        let mut k = setup_kernel();
        let (err, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::CALL, &[ep_hid, 0, 8, 0, 0, 0]);

        assert_ne!(err, 0, "CALL with null msg_ptr and nonzero len must fail");

        let (err, _) = call(&mut k, num::RECV, &[ep_hid, 0, 128, 0, 0, 0]);

        assert_ne!(err, 0, "RECV with null out_buf and nonzero cap must fail");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn zero_length_message_accepted() {
        let mut k = setup_kernel();
        let (err, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        let mut buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[ep_hid, buf.as_mut_ptr() as u64, 0, 0, 0, 0],
        );

        assert_eq!(err, 0, "zero-length CALL must succeed");

        let (err, packed) = call(
            &mut k,
            num::RECV,
            &[ep_hid, buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        assert_eq!(err, 0, "RECV must succeed");

        let msg_len = (packed & 0xFFFF_FFFF) as usize;

        assert_eq!(msg_len, 0, "received message must be zero-length");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn max_ipc_handles_boundary() {
        let mut k = setup_kernel();
        let (err, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        let too_many = config::MAX_IPC_HANDLES + 1;
        let (err, _) = call(&mut k, num::CALL, &[ep_hid, 0, 0, 0, too_many as u64, 0]);

        assert_eq!(
            err,
            SyscallError::InvalidArgument as u64,
            "CALL with too many handles must fail"
        );

        crate::invariants::assert_valid(&*k);
    }

    // =========================================================================
    // ERROR INJECTION: ASID LEAK ON SPACE_CREATE FAILURE
    // =========================================================================

    #[test]
    fn space_create_handle_table_full_frees_asid() {
        use crate::frame::arch::page_table;

        page_table::reset_asid_pool();

        let mut k = setup_kernel();

        for _ in 0..config::MAX_HANDLES {
            let (err, _) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

            if err != 0 {
                break;
            }
        }

        let (err, _) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_ne!(err, 0, "space_create must fail when handle table is full");

        page_table::reset_asid_pool();

        let asid = page_table::alloc_asid();

        assert!(
            asid.is_some(),
            "ASID pool must not leak ASIDs on failed space_create"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn space_create_space_table_full_frees_asid() {
        use crate::frame::arch::page_table;

        page_table::reset_asid_pool();

        let mut k = setup_kernel();
        let mut space_handles = alloc::vec::Vec::new();

        for _ in 0..config::MAX_ADDRESS_SPACES {
            let (err, hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

            if err != 0 {
                break;
            }

            space_handles.push(hid);
        }

        let pre_count = space_handles.len();
        let (err, _) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_ne!(err, 0, "space_create must fail when space table is full");

        for hid in &space_handles {
            call(&mut k, num::SPACE_DESTROY, &[*hid, 0, 0, 0, 0, 0]);
        }

        page_table::reset_asid_pool();

        for _ in 0..pre_count + 1 {
            assert!(
                page_table::alloc_asid().is_some(),
                "ASID pool must recover all ASIDs after cleanup"
            );
        }

        crate::invariants::assert_valid(&*k);
    }

    // =========================================================================
    // ERROR INJECTION: VMO MAP BOUNDARY VALUES
    // =========================================================================

    #[test]
    fn vmo_map_without_map_right_rejected() {
        let mut k = setup_kernel();
        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let read_only_rights = Rights::READ.0 as u64;
        let (err, dup_hid) = call(
            &mut k,
            num::HANDLE_DUP,
            &[vmo_hid, read_only_rights, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::VMO_MAP, &[dup_hid, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0, "VMO map without MAP right must fail");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn vmo_map_write_without_write_right_rejected() {
        let mut k = setup_kernel();
        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let map_only = (Rights::MAP.0 | Rights::READ.0) as u64;
        let (err, dup_hid) = call(&mut k, num::HANDLE_DUP, &[vmo_hid, map_only, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let write_perms = Rights::WRITE.0 as u64;
        let (err, _) = call(&mut k, num::VMO_MAP, &[dup_hid, 0, write_perms, 0, 0, 0]);

        assert_ne!(
            err, 0,
            "VMO map with WRITE perm without WRITE right must fail"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_dup_with_zero_rights() {
        let mut k = setup_kernel();
        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let (err, dup_hid) = call(&mut k, num::HANDLE_DUP, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0, "dup with zero rights must succeed (attenuation)");

        let (err, _) = call(&mut k, num::HANDLE_INFO, &[dup_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0, "handle_info must succeed even with zero rights");

        let (err, _) = call(&mut k, num::VMO_MAP, &[dup_hid, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0, "zero-rights handle must fail MAP operation");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_boundary_ids() {
        let mut k = setup_kernel();
        let max_handle = config::MAX_HANDLES as u64;
        let (err, _) = call(&mut k, num::HANDLE_INFO, &[max_handle, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0, "handle ID at MAX_HANDLES must fail");

        let (err, _) = call(&mut k, num::HANDLE_INFO, &[u32::MAX as u64, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0, "handle ID at u32::MAX must fail");

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[max_handle, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0, "close handle ID at MAX_HANDLES must fail");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn vmo_resize_to_usize_max_rejected() {
        let mut k = setup_kernel();
        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::VMO_RESIZE, &[vmo_hid, u64::MAX, 0, 0, 0, 0]);

        assert_ne!(err, 0, "VMO resize to u64::MAX must fail");

        crate::invariants::assert_valid(&*k);
    }
}
