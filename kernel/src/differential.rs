//! Host-vs-bare-metal differential testing.
//!
//! Defines canonical syscall sequences that must produce identical results
//! on both the host target (via `Kernel::dispatch()`) and bare-metal (via
//! real SVC calls in the hypervisor). Any divergence indicates a `#[cfg]`
//! bug where the host stub and real implementation disagree.
//!
//! The expected results generated here are compiled into the bare-metal
//! integration test binary for comparison.

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use crate::{
        address_space::AddressSpace,
        config,
        syscall::{Kernel, num},
        thread::Thread,
        types::{AddressSpaceId, HandleId, ObjectType, Priority, Rights, SyscallError, ThreadId},
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

    fn inv(k: &Kernel) {
        crate::invariants::assert_valid(k);
    }

    // =====================================================================
    // Canonical scenarios — pointer-free syscalls only.
    //
    // These produce deterministic results that the bare-metal integration
    // test can compare against. Each test documents its expected sequence
    // of (error, value) results as comments — the bare-metal test mirrors
    // these exactly.
    // =====================================================================

    #[test]
    fn diff_object_lifecycle() {
        let mut k = setup_kernel();

        // Create VMO → (0, handle_id)
        let (e, vmo) = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        );
        assert_eq!(e, 0);

        // Info on VMO → (0, packed_type_rights)
        let (e, info) = call(&mut k, num::HANDLE_INFO, &[vmo, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        let obj_type = (info >> 32) as u8;
        let rights = (info & 0xFFFF_FFFF) as u32;
        assert_eq!(obj_type, ObjectType::Vmo as u8);
        assert_eq!(rights, Rights::ALL.0);

        // Dup with read-only → (0, dup_handle_id)
        let (e, dup) = call(
            &mut k,
            num::HANDLE_DUP,
            &[vmo, Rights::READ.0 as u64, 0, 0, 0, 0],
        );
        assert_eq!(e, 0);
        assert_ne!(dup, vmo);

        // Info on dup → read-only rights
        let (e, info) = call(&mut k, num::HANDLE_INFO, &[dup, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        let dup_rights = (info & 0xFFFF_FFFF) as u32;
        assert_eq!(dup_rights, Rights::READ.0);

        // Close dup → (0, 0)
        let (e, _) = call(&mut k, num::HANDLE_CLOSE, &[dup, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        // Info on closed dup → InvalidHandle
        let (e, _) = call(&mut k, num::HANDLE_INFO, &[dup, 0, 0, 0, 0, 0]);
        assert_eq!(e, SyscallError::InvalidHandle as u64);

        // Original still valid
        let (e, _) = call(&mut k, num::HANDLE_INFO, &[vmo, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        // Close original
        let (e, _) = call(&mut k, num::HANDLE_CLOSE, &[vmo, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        inv(&k);
    }

    #[test]
    fn diff_event_signal_clear_check() {
        let mut k = setup_kernel();

        let (e, evt) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        assert_eq!(e, 0);

        // Signal bits 0x5
        let (e, _) = call(&mut k, num::EVENT_SIGNAL, &[evt, 0x5, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        // Wait for bit 0x4 — should succeed immediately
        let (e, fired) = call(&mut k, num::EVENT_WAIT, &[evt, 0x4, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        assert_eq!(fired, evt);

        // Clear bit 0x1
        let (e, _) = call(&mut k, num::EVENT_CLEAR, &[evt, 0x1, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        // Wait for bit 0x1 — should block (bit was cleared)
        let (e, _) = call(&mut k, num::EVENT_WAIT, &[evt, 0x1, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        assert_eq!(
            k.threads.get(0).unwrap().state(),
            crate::thread::ThreadRunState::Blocked
        );

        // Restore thread: Blocked → Ready → Running
        k.threads
            .get_mut(0)
            .unwrap()
            .set_state(crate::thread::ThreadRunState::Ready);
        k.threads
            .get_mut(0)
            .unwrap()
            .set_state(crate::thread::ThreadRunState::Running);
        k.scheduler.core_mut(0).set_current(Some(ThreadId(0)));

        inv(&k);
    }

    #[test]
    fn diff_endpoint_lifecycle() {
        let mut k = setup_kernel();

        let (e, ep) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        assert_eq!(e, 0);

        let (e, info) = call(&mut k, num::HANDLE_INFO, &[ep, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        let obj_type = (info >> 32) as u8;
        assert_eq!(obj_type, ObjectType::Endpoint as u8);

        // Create event and bind
        let (e, evt) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        assert_eq!(e, 0);

        let (e, _) = call(&mut k, num::ENDPOINT_BIND_EVENT, &[ep, evt, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        // Close both
        let (e, _) = call(&mut k, num::HANDLE_CLOSE, &[ep, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        let (e, _) = call(&mut k, num::HANDLE_CLOSE, &[evt, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        inv(&k);
    }

    #[test]
    fn diff_system_info_constants() {
        let mut k = setup_kernel();

        // Page size
        let (e, val) = call(&mut k, num::SYSTEM_INFO, &[0, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        assert_eq!(val, config::PAGE_SIZE as u64);

        // Message size
        let (e, val) = call(&mut k, num::SYSTEM_INFO, &[1, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        assert_eq!(val, crate::endpoint::MSG_SIZE as u64);

        // Core count
        let (e, val) = call(&mut k, num::SYSTEM_INFO, &[2, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        assert!(val >= 1);

        // Invalid info type
        let (e, _) = call(&mut k, num::SYSTEM_INFO, &[3, 0, 0, 0, 0, 0]);
        assert_eq!(e, SyscallError::InvalidArgument as u64);

        let (e, _) = call(&mut k, num::SYSTEM_INFO, &[u64::MAX, 0, 0, 0, 0, 0]);
        assert_eq!(e, SyscallError::InvalidArgument as u64);

        inv(&k);
    }

    #[test]
    fn diff_error_codes_match() {
        let mut k = setup_kernel();

        // Invalid syscall → InvalidArgument
        let (e, _) = call(&mut k, 30, &[0; 6]);
        assert_eq!(e, SyscallError::InvalidArgument as u64);

        let (e, _) = call(&mut k, u64::MAX, &[0; 6]);
        assert_eq!(e, SyscallError::InvalidArgument as u64);

        // Invalid handle → InvalidHandle
        let (e, _) = call(&mut k, num::HANDLE_INFO, &[999, 0, 0, 0, 0, 0]);
        assert_eq!(e, SyscallError::InvalidHandle as u64);

        // Wrong handle type — use VMO handle for endpoint operation
        let (_, vmo) = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        );
        let (e, _) = call(&mut k, num::EVENT_SIGNAL, &[vmo, 0x1, 0, 0, 0, 0]);
        assert_eq!(e, SyscallError::WrongHandleType as u64);

        // VMO create with zero size → InvalidArgument
        let (e, _) = call(&mut k, num::VMO_CREATE, &[0, 0, 0, 0, 0, 0]);
        assert_eq!(e, SyscallError::InvalidArgument as u64);

        // VMO seal then resize → AlreadySealed
        let (e, _) = call(&mut k, num::VMO_SEAL, &[vmo, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        let (e, _) = call(
            &mut k,
            num::VMO_RESIZE,
            &[vmo, config::PAGE_SIZE as u64 * 2, 0, 0, 0, 0],
        );
        assert_eq!(e, SyscallError::AlreadySealed as u64);

        // Dup with rights escalation → InsufficientRights
        let (_, read_only) = call(
            &mut k,
            num::HANDLE_DUP,
            &[vmo, Rights::READ.0 as u64, 0, 0, 0, 0],
        );
        let (e, _) = call(
            &mut k,
            num::HANDLE_DUP,
            &[read_only, Rights::ALL.0 as u64, 0, 0, 0, 0],
        );
        assert_eq!(e, SyscallError::InsufficientRights as u64);

        inv(&k);
    }

    #[test]
    fn diff_vmo_snapshot_seal_resize() {
        let mut k = setup_kernel();

        let (e, vmo) = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        );
        assert_eq!(e, 0);

        // Snapshot
        let (e, snap) = call(&mut k, num::VMO_SNAPSHOT, &[vmo, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        // Snapshot info shows VMO type
        let (e, info) = call(&mut k, num::HANDLE_INFO, &[snap, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);
        assert_eq!((info >> 32) as u8, ObjectType::Vmo as u8);

        // Resize original
        let (e, _) = call(
            &mut k,
            num::VMO_RESIZE,
            &[vmo, config::PAGE_SIZE as u64 * 2, 0, 0, 0, 0],
        );
        assert_eq!(e, 0);

        // Seal snapshot — should succeed
        let (e, _) = call(&mut k, num::VMO_SEAL, &[snap, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        // Resize snapshot — fails (sealed)
        let (e, _) = call(
            &mut k,
            num::VMO_RESIZE,
            &[snap, config::PAGE_SIZE as u64, 0, 0, 0, 0],
        );
        assert_eq!(e, SyscallError::AlreadySealed as u64);

        inv(&k);
    }

    #[test]
    fn diff_ipc_call_blocks_without_receiver() {
        let mut k = setup_kernel();

        let (e, ep) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        assert_eq!(e, 0);

        // Call with empty message — should block (no receiver)
        let mut buf = [0u8; 128];
        let (e, _) = call(
            &mut k,
            num::CALL,
            &[ep, buf.as_mut_ptr() as u64, 0, 0, 0, 0],
        );
        assert_eq!(e, 0);
        assert_eq!(
            k.threads.get(0).unwrap().state(),
            crate::thread::ThreadRunState::Blocked
        );

        // Thread is blocked on the endpoint — verify the pending call exists
        let pending = k
            .endpoints
            .get(
                k.spaces
                    .get(0)
                    .unwrap()
                    .handles()
                    .lookup(HandleId(ep as u32))
                    .unwrap()
                    .object_id,
            )
            .unwrap()
            .pending_call_count();
        assert_eq!(pending, 1);
    }

    #[test]
    fn diff_handle_table_slot_reuse() {
        let mut k = setup_kernel();
        let page = config::PAGE_SIZE as u64;

        // Create two VMOs so we have handles at slots 0 and 1
        let (_, h1) = call(&mut k, num::VMO_CREATE, &[page, 0, 0, 0, 0, 0]);
        let (_, h2) = call(&mut k, num::VMO_CREATE, &[page, 0, 0, 0, 0, 0]);
        assert_ne!(h1, h2);

        // Close h1, create new — should reuse h1's slot
        call(&mut k, num::HANDLE_CLOSE, &[h1, 0, 0, 0, 0, 0]);

        let (e, h3) = call(&mut k, num::VMO_CREATE, &[page, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        // h2 is still valid (unaffected by h1's close/reuse)
        let (e, _) = call(&mut k, num::HANDLE_INFO, &[h2, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        // h3 is valid
        let (e, _) = call(&mut k, num::HANDLE_INFO, &[h3, 0, 0, 0, 0, 0]);
        assert_eq!(e, 0);

        // Double close returns InvalidHandle
        let (e, _) = call(&mut k, num::HANDLE_CLOSE, &[h1, 0, 0, 0, 0, 0]);
        // May succeed (slot reused to h3) or fail — depends on whether h1 == h3
        if h1 == h3 {
            assert_eq!(e, 0);
        } else {
            assert_eq!(e, SyscallError::InvalidHandle as u64);
        }

        inv(&k);
    }
}
